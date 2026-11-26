//! Memory: the kernel heap, DMA buffers, and mapping device registers (MMIO).
//!
//! The heap is the largest usable RAM region (on a PC with lots of memory, nearly all of it),
//! reached through Limine's higher-half direct map (HHDM: virtual = physical + offset).
//! Because of that, any heap pointer can be turned back into a physical address for devices
//! that do DMA.
//!
//! Small blocks (up to 4 KiB: most of what Rust code allocates) come from free lists, one per
//! size, carved out of 64 KiB pieces: taking or giving one back is a few instructions, each
//! size has its own lock (so cores rarely wait for each other), and they don't break up the
//! space big blocks need. Bigger blocks (pictures, sound, files) come from a first-fit list
//! over the rest.

use core::alloc::{GlobalAlloc, Layout};
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};

use alloc::alloc::{alloc_zeroed, dealloc};
use limine::memory_map::{Entry, EntryType};
use linked_list_allocator::LockedHeap;
use spin::Mutex;
use x86_64::registers::control::Cr3;
use x86_64::structures::paging::{
    FrameAllocator, Mapper, OffsetPageTable, Page, PageTable, PageTableFlags, PhysFrame, Size4KiB,
};
use x86_64::{PhysAddr, VirtAddr};

#[global_allocator]
static ALLOCATOR: Heap = Heap::new();

/// The heap's sizes of small blocks (bytes). The powers of two among them also serve blocks
/// that need lining up to more than 16 bytes.
const CLASSES: [usize; 28] = [16, 32, 48, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024, 1280, 1536, 1792, 2048, 2560, 3072, 3584, 4096];
const SMALL_MAX: usize = 4096;
/// Small blocks are carved out of pieces this big (and lined up to it).
const PIECE: usize = 64 * 1024;

/// Free small blocks of one size: a list threaded through the blocks themselves, and the
/// rest of the piece being carved up.
struct FreeList {
    head: usize,
    bump: usize,
    end: usize,
}

struct Heap {
    big: LockedHeap,
    small: [Mutex<FreeList>; CLASSES.len()],
    /// Bytes handed out (small blocks count their whole size), for Settings > This PC.
    used: AtomicUsize,
    size: AtomicUsize,
}

impl Heap {
    const fn new() -> Heap {
        Heap {
            big: LockedHeap::empty(),
            small: [const { Mutex::new(FreeList { head: 0, bump: 0, end: 0 }) }; CLASSES.len()],
            used: AtomicUsize::new(0),
            size: AtomicUsize::new(0),
        }
    }
}

/// Which small size serves `layout` (None: a big block).
fn class_of(layout: Layout) -> Option<usize> {
    let (size, align) = (layout.size().max(1), layout.align());
    let need = if align <= 16 { size } else { size.max(align).next_power_of_two() };
    if need > SMALL_MAX || align > SMALL_MAX {
        return None;
    }
    // Every size up to 128 is a multiple of 16; then four steps per doubling.
    let i = if need <= 128 {
        need.div_ceil(16) - 1
    } else {
        let p = usize::BITS - 1 - (need - 1).leading_zeros(); // 2^p < need <= 2^(p+1)
        let step = 1usize << (p - 2);
        let k = (need - (1 << p)).div_ceil(step); // 1..=4
        7 + (p as usize - 7) * 4 + k
    };
    debug_assert!(CLASSES[i] >= need && (i == 0 || CLASSES[i - 1] < need));
    Some(i)
}

unsafe impl GlobalAlloc for Heap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        let Some(c) = class_of(layout) else {
            let p = unsafe { self.big.alloc(layout) };
            if !p.is_null() {
                self.used.fetch_add(layout.size(), Relaxed);
            }
            return p;
        };
        let size = CLASSES[c];
        let mut fl = self.small[c].lock();
        if fl.head != 0 {
            let p = fl.head;
            fl.head = unsafe { *(p as *const usize) };
            self.used.fetch_add(size, Relaxed);
            return p as *mut u8;
        }
        if fl.bump + size > fl.end {
            let piece = unsafe { self.big.alloc(Layout::from_size_align_unchecked(PIECE, PIECE)) };
            if piece.is_null() {
                return piece;
            }
            fl.bump = piece as usize;
            fl.end = piece as usize + PIECE;
        }
        let p = fl.bump;
        fl.bump += size;
        self.used.fetch_add(size, Relaxed);
        p as *mut u8
    }

    unsafe fn dealloc(&self, ptr: *mut u8, layout: Layout) {
        match class_of(layout) {
            None => {
                self.used.fetch_sub(layout.size(), Relaxed);
                unsafe { self.big.dealloc(ptr, layout) }
            }
            Some(c) => {
                let mut fl = self.small[c].lock();
                unsafe { *(ptr as *mut usize) = fl.head };
                fl.head = ptr as usize;
                self.used.fetch_sub(CLASSES[c], Relaxed);
            }
        }
    }

    unsafe fn realloc(&self, ptr: *mut u8, layout: Layout, new_size: usize) -> *mut u8 {
        // Still the same small size: nothing to move.
        let new = unsafe { Layout::from_size_align_unchecked(new_size, layout.align()) };
        if let (Some(a), Some(b)) = (class_of(layout), class_of(new)) {
            if a == b {
                return ptr;
            }
        }
        let p = unsafe { self.alloc(new) };
        if !p.is_null() {
            unsafe { core::ptr::copy_nonoverlapping(ptr, p, layout.size().min(new_size)) };
            unsafe { self.dealloc(ptr, layout) };
        }
        p
    }
}

/// The heap's size and how much of it is in use (bytes).
pub fn heap_use() -> (usize, usize) {
    (ALLOCATOR.size.load(Relaxed), ALLOCATOR.used.load(Relaxed))
}

static HHDM: AtomicU64 = AtomicU64::new(0);

/// Returns (heap size in bytes, total usable RAM in bytes).
pub fn init(entries: &[&Entry], hhdm_offset: u64) -> (usize, u64) {
    HHDM.store(hhdm_offset, Relaxed);
    let usable = entries.iter().filter(|e| e.entry_type == EntryType::USABLE);
    let total: u64 = usable.clone().map(|e| e.length).sum();
    let best = usable.max_by_key(|e| e.length).expect("no usable memory");
    let size = best.length as usize;
    unsafe {
        ALLOCATOR.big.lock().init((best.base + hhdm_offset) as *mut u8, size);
    }
    ALLOCATOR.size.store(size, Relaxed);
    (size, total)
}

/// Physical address of a heap (or other HHDM) pointer.
pub fn phys_of<T>(p: *const T) -> u64 {
    p as u64 - HHDM.load(Relaxed)
}

/// A zeroed, physically contiguous buffer that a device can read/write by DMA.
pub struct Dma {
    ptr: *mut u8,
    layout: Layout,
}

unsafe impl Send for Dma {}

impl Dma {
    /// `align` must be a power of two. Buffers up to `align` bytes never cross an
    /// `align` boundary, which is what xHCI needs (rings: 64 KiB, contexts: page).
    pub fn new(size: usize, align: usize) -> Dma {
        let layout = Layout::from_size_align(size.max(1), align).unwrap();
        let ptr = unsafe { alloc_zeroed(layout) };
        assert!(!ptr.is_null(), "out of memory for DMA buffer");
        Dma { ptr, layout }
    }
    pub fn phys(&self) -> u64 {
        phys_of(self.ptr)
    }
    pub fn ptr<T>(&self) -> *mut T {
        self.ptr as *mut T
    }
    pub fn len(&self) -> usize {
        self.layout.size()
    }
    pub fn bytes(&self) -> &[u8] {
        unsafe { core::slice::from_raw_parts(self.ptr, self.layout.size()) }
    }
    pub fn bytes_mut(&mut self) -> &mut [u8] {
        unsafe { core::slice::from_raw_parts_mut(self.ptr, self.layout.size()) }
    }
}

impl Drop for Dma {
    fn drop(&mut self) {
        unsafe { dealloc(self.ptr, self.layout) };
    }
}

// ---- MMIO mapping -----------------------------------------------------------

/// Device registers are mapped into their own virtual window, uncached.
const MMIO_BASE: u64 = 0xFFFF_C000_0000_0000;
static MMIO_NEXT: Mutex<u64> = Mutex::new(MMIO_BASE);

struct HeapFrames;

unsafe impl FrameAllocator<Size4KiB> for HeapFrames {
    fn allocate_frame(&mut self) -> Option<PhysFrame<Size4KiB>> {
        // Page-table pages come from the heap; they're never freed.
        let p = unsafe { alloc_zeroed(Layout::from_size_align(4096, 4096).ok()?) };
        if p.is_null() {
            return None;
        }
        Some(PhysFrame::containing_address(PhysAddr::new(phys_of(p))))
    }
}

/// How memory at a virtual address is cached, by its page-table entry and the processor's
/// PAT: "WB", "WC", "UC"... (None if it isn't mapped).
pub fn caching_of(virt: u64) -> Option<&'static str> {
    let hhdm = HHDM.load(Relaxed);
    let (l4_frame, _) = Cr3::read();
    let mut table = l4_frame.start_address().as_u64();
    for level in (1..=4).rev() {
        let idx = (virt >> (12 + 9 * (level - 1))) & 511;
        let e = unsafe { *((table + hhdm + idx * 8) as *const u64) };
        if e & 1 == 0 {
            return None;
        }
        let huge = level > 1 && e & (1 << 7) != 0;
        if level == 1 || huge {
            // PAT index from PWT (bit 3), PCD (bit 4) and PAT (bit 7 in 4 KiB entries, 12 in big ones).
            let pat_bit = if huge { e >> 12 & 1 } else { e >> 7 & 1 };
            let i = (pat_bit << 2 | (e >> 4 & 1) << 1 | (e >> 3 & 1)) as u32;
            let pat = unsafe { x86_64::registers::model_specific::Msr::new(0x277).read() };
            return Some(match (pat >> (i * 8)) & 7 {
                0 => "UC",
                1 => "WC",
                4 => "WT",
                5 => "WP",
                6 => "WB",
                7 => "UC-",
                _ => "?",
            });
        }
        table = e & 0x000F_FFFF_FFFF_F000;
    }
    None
}

/// Map a graphics card's video memory (cached, for fast drawing; used for virtual machines'
/// cards, which don't need write-combining). Returns the virtual address.
pub fn map_framebuffer(phys: u64, size: u64) -> u64 {
    map_with(phys, size, PageTableFlags::PRESENT | PageTableFlags::WRITABLE)
}

/// Map video memory write-combined (writes go out in bursts), using the PAT entry the boot
/// loader set up for write-combining (index 5: PWT and the PAT bit); uncached if there isn't
/// one. Returns the virtual address.
pub fn map_wc(phys: u64, size: u64) -> u64 {
    let pat = unsafe { x86_64::registers::model_specific::Msr::new(0x277).read() };
    if (pat >> 40) & 7 != 1 {
        return map_mmio(phys, size);
    }
    let virt = map_with(phys, size, PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::WRITE_THROUGH);
    // The PAT bit of a 4 KiB entry is bit 7, which the mapper won't set itself.
    let hhdm = HHDM.load(Relaxed);
    let (l4_frame, _) = Cr3::read();
    let l4 = unsafe { &mut *((l4_frame.start_address().as_u64() + hhdm) as *mut PageTable) };
    let mut mapper = unsafe { OffsetPageTable::new(l4, VirtAddr::new(hhdm)) };
    let first = virt & !0xFFF;
    let pages = ((virt + size + 0xFFF) & !0xFFF) / 4096 - first / 4096;
    for i in 0..pages {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(first + i * 4096));
        let flags = PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::WRITE_THROUGH | PageTableFlags::HUGE_PAGE;
        if let Ok(f) = unsafe { mapper.update_flags(page, flags) } {
            f.flush();
        }
    }
    virt
}

/// Map `size` bytes of device registers at physical address `phys`. Returns the virtual address.
pub fn map_mmio(phys: u64, size: u64) -> u64 {
    map_with(phys, size, PageTableFlags::PRESENT | PageTableFlags::WRITABLE | PageTableFlags::NO_CACHE | PageTableFlags::WRITE_THROUGH)
}

fn map_with(phys: u64, size: u64, flags: PageTableFlags) -> u64 {
    let hhdm = HHDM.load(Relaxed);
    let first = phys & !0xFFF;
    let pages = ((phys + size + 0xFFF) & !0xFFF) / 4096 - first / 4096;
    let mut next = MMIO_NEXT.lock();
    let virt_base = *next;
    *next += (pages + 1) * 4096; // leave a guard page

    let (l4_frame, _) = Cr3::read();
    let l4 = unsafe { &mut *((l4_frame.start_address().as_u64() + hhdm) as *mut PageTable) };
    let mut mapper = unsafe { OffsetPageTable::new(l4, VirtAddr::new(hhdm)) };
    for i in 0..pages {
        let page = Page::<Size4KiB>::containing_address(VirtAddr::new(virt_base + i * 4096));
        let frame = PhysFrame::containing_address(PhysAddr::new(first + i * 4096));
        unsafe {
            mapper.map_to(page, frame, flags, &mut HeapFrames).expect("map_mmio failed").flush();
        }
    }
    virt_base + (phys - first)
}

//! Memory: the kernel heap, DMA buffers, and mapping device registers (MMIO).
//!
//! The heap is the largest usable RAM region, reached through Limine's higher-half
//! direct map (HHDM: virtual = physical + offset). Because of that, any heap pointer
//! can be turned back into a physical address for devices that do DMA.

use core::alloc::Layout;
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

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
static ALLOCATOR: LockedHeap = LockedHeap::empty();

/// Full-resolution photo editing needs a few hundred MB; use what the machine has, up to 2 GiB.
const MAX_HEAP: u64 = 2 * 1024 * 1024 * 1024;

static HHDM: AtomicU64 = AtomicU64::new(0);

/// Returns (heap size in bytes, total usable RAM in bytes).
pub fn init(entries: &[&Entry], hhdm_offset: u64) -> (usize, u64) {
    HHDM.store(hhdm_offset, Relaxed);
    let usable = entries.iter().filter(|e| e.entry_type == EntryType::USABLE);
    let total: u64 = usable.clone().map(|e| e.length).sum();
    let best = usable.max_by_key(|e| e.length).expect("no usable memory");
    let size = best.length.min(MAX_HEAP) as usize;
    unsafe {
        ALLOCATOR.lock().init((best.base + hhdm_offset) as *mut u8, size);
    }
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

/// Map a graphics card's video memory (cached, for fast drawing; used for virtual machines'
/// cards, which don't need write-combining). Returns the virtual address.
pub fn map_framebuffer(phys: u64, size: u64) -> u64 {
    map_with(phys, size, PageTableFlags::PRESENT | PageTableFlags::WRITABLE)
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

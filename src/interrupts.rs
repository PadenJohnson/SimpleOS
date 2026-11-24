//! GDT, IDT and the legacy 8259 PIC.
//!
//! Real PCs send interrupts QEMU never does: "spurious" IRQ 7 / 15 from the 8259 PIC, and
//! timers or other local-APIC interrupts the firmware left switched on. Every one of the 256
//! vectors has a handler, so none of them can crash Solon, and every CPU exception gets its
//! own message. Double faults run on their own stack, so even running out of stack still
//! shows the crash screen.

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Lazy;
use x86_64::instructions::port::Port;
use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
use x86_64::instructions::tables::load_tss;
use x86_64::registers::control::Cr2;
use x86_64::registers::model_specific::Msr;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};
use x86_64::structures::tss::TaskStateSegment;
use x86_64::VirtAddr;

use crate::ps2;

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const IRQ_BASE: u8 = 32;
const VEC_TIMER: u8 = IRQ_BASE;
const VEC_KEYBOARD: u8 = IRQ_BASE + 1;
const VEC_LAPIC_SPURIOUS: u8 = 0xFF;

/// Timer ticks since boot (HZ per second).
pub static TICKS: AtomicU64 = AtomicU64::new(0);
pub const HZ: u64 = 100;
const VEC_MOUSE: u8 = IRQ_BASE + 12;

/// Interrupts nobody asked for (counted for the diagnostics log).
pub static STRAY: AtomicU64 = AtomicU64::new(0);
/// Virtual address of the local APIC's registers (0 if it's off or not mapped yet).
static LAPIC: AtomicU64 = AtomicU64::new(0);

/// Busy-wait for at least `ms` milliseconds (interrupts must be enabled).
pub fn sleep_ms(ms: u64) {
    let end = TICKS.load(Ordering::Relaxed) + ms * HZ / 1000 + 1;
    while TICKS.load(Ordering::Relaxed) < end {
        core::hint::spin_loop();
    }
}

struct Selectors {
    code: SegmentSelector,
    data: SegmentSelector,
    tss: SegmentSelector,
}

const DOUBLE_FAULT_IST: u16 = 0;
const EMERGENCY_STACK: usize = 32 * 1024;
static mut DF_STACK: [u8; EMERGENCY_STACK] = [0; EMERGENCY_STACK];

static TSS: Lazy<TaskStateSegment> = Lazy::new(|| {
    let mut tss = TaskStateSegment::new();
    let start = VirtAddr::from_ptr(core::ptr::addr_of!(DF_STACK));
    tss.interrupt_stack_table[DOUBLE_FAULT_IST as usize] = start + EMERGENCY_STACK as u64;
    tss
});

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    let tss = gdt.append(Descriptor::tss_segment(&TSS));
    (gdt, Selectors { code, data, tss })
});

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    // CPU exceptions
    idt.divide_error.set_handler_fn(divide_error);
    idt.debug.set_handler_fn(debug);
    idt.non_maskable_interrupt.set_handler_fn(nmi);
    idt.breakpoint.set_handler_fn(breakpoint);
    idt.overflow.set_handler_fn(overflow);
    idt.bound_range_exceeded.set_handler_fn(bound_range);
    idt.invalid_opcode.set_handler_fn(invalid_opcode);
    idt.device_not_available.set_handler_fn(device_not_available);
    unsafe {
        idt.double_fault.set_handler_fn(double_fault).set_stack_index(DOUBLE_FAULT_IST);
    }
    idt.invalid_tss.set_handler_fn(invalid_tss);
    idt.segment_not_present.set_handler_fn(segment_not_present);
    idt.stack_segment_fault.set_handler_fn(stack_segment_fault);
    idt.general_protection_fault.set_handler_fn(general_protection);
    idt.page_fault.set_handler_fn(page_fault);
    idt.x87_floating_point.set_handler_fn(x87_floating_point);
    idt.alignment_check.set_handler_fn(alignment_check);
    idt.machine_check.set_handler_fn(machine_check);
    idt.simd_floating_point.set_handler_fn(simd_floating_point);
    idt.virtualization.set_handler_fn(virtualization);
    // Every other vector: stray interrupts are acknowledged and ignored.
    for v in IRQ_BASE..=255u8 {
        idt[v].set_handler_fn(stray);
    }
    for irq in 0..16u8 {
        idt[IRQ_BASE + irq].set_handler_fn(if irq < 8 { pic_master_stray } else { pic_slave_stray });
    }
    idt[IRQ_BASE + 7].set_handler_fn(pic_irq7);
    idt[IRQ_BASE + 15].set_handler_fn(pic_irq15);
    idt[VEC_LAPIC_SPURIOUS].set_handler_fn(lapic_spurious);
    idt[VEC_TIMER].set_handler_fn(timer);
    idt[VEC_KEYBOARD].set_handler_fn(keyboard);
    idt[VEC_MOUSE].set_handler_fn(mouse);
    idt
});

pub fn init() {
    // Load our own GDT so we don't depend on the one Limine left behind.
    let (gdt, sel) = &*GDT;
    gdt.load();
    unsafe {
        CS::set_reg(sel.code);
        SS::set_reg(sel.data);
        DS::set_reg(sel.data);
        ES::set_reg(sel.data);
        load_tss(sel.tss);
    }
    IDT.load();
    quiet_lapic();
    unsafe {
        pic_init();
        pit_init();
    }
}

/// Another core, started to do work: its own GDT and TSS (each core needs its own TSS, and its
/// own double-fault stack in it), and the IDT every core shares. Interrupts stay off there.
pub fn init_ap() {
    let stack: &'static mut [u8] = alloc::vec![0u8; EMERGENCY_STACK].leak();
    let mut tss = TaskStateSegment::new();
    tss.interrupt_stack_table[DOUBLE_FAULT_IST as usize] = VirtAddr::from_ptr(stack.as_ptr()) + EMERGENCY_STACK as u64;
    let tss: &'static TaskStateSegment = alloc::boxed::Box::leak(alloc::boxed::Box::new(tss));
    let mut gdt = GlobalDescriptorTable::new();
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    let tss_sel = gdt.append(Descriptor::tss_segment(tss));
    let gdt: &'static GlobalDescriptorTable = alloc::boxed::Box::leak(alloc::boxed::Box::new(gdt));
    gdt.load();
    unsafe {
        CS::set_reg(code);
        SS::set_reg(data);
        DS::set_reg(data);
        ES::set_reg(data);
        load_tss(tss_sel);
    }
    IDT.load();
}

/// Firmware sometimes leaves the local APIC's own timer (and other local interrupts)
/// running. Solon uses the old PIT and PIC, so switch those off; keep the APIC itself on,
/// because the PIC's interrupts pass through it on most PCs.
fn quiet_lapic() {
    let base = unsafe { Msr::new(0x1B).read() };
    if base & (1 << 11) == 0 {
        kprintln!("solon: local APIC off");
        return;
    }
    if base & (1 << 10) != 0 {
        // x2APIC mode: registers are MSRs.
        unsafe {
            for msr in [0x832u32, 0x833, 0x834, 0x837] {
                let mut m = Msr::new(msr);
                let v = m.read();
                m.write(v | (1 << 16));
            }
            let mut svr = Msr::new(0x80F);
            let v = svr.read();
            svr.write((v & !0xFF) | VEC_LAPIC_SPURIOUS as u64);
        }
        kprintln!("solon: local APIC (x2APIC) quieted");
        return;
    }
    let phys = base & 0xF_FFFF_F000;
    let virt = crate::memory::map_mmio(phys, 0x1000);
    LAPIC.store(virt, Ordering::Relaxed);
    let reg = |off: u64| (virt + off) as *mut u32;
    unsafe {
        // Mask the timer, thermal, performance and error interrupts.
        for off in [0x320u64, 0x330, 0x340, 0x370] {
            let v = core::ptr::read_volatile(reg(off));
            core::ptr::write_volatile(reg(off), v | (1 << 16));
        }
        // Spurious interrupts from the APIC itself land on a vector we handle.
        let svr = core::ptr::read_volatile(reg(0xF0));
        core::ptr::write_volatile(reg(0xF0), (svr & !0xFF) | VEC_LAPIC_SPURIOUS as u32);
    }
    kprintln!("solon: local APIC at {:#x} quieted", phys);
}

fn lapic_eoi() {
    let v = LAPIC.load(Ordering::Relaxed);
    if v != 0 {
        unsafe { core::ptr::write_volatile((v + 0xB0) as *mut u32, 0) };
    } else if unsafe { Msr::new(0x1B).read() } & (1 << 10) != 0 {
        unsafe { Msr::new(0x80B).write(0) };
    }
}

/// The PIC's "in service" register (which IRQs it really raised).
fn pic_isr(cmd: u16) -> u8 {
    unsafe {
        Port::<u8>::new(cmd).write(0x0B);
        Port::<u8>::new(cmd).read()
    }
}

fn note_stray(what: &str) {
    let n = STRAY.fetch_add(1, Ordering::Relaxed);
    if n < 5 || n.is_power_of_two() {
        kprintln!("solon: ignored stray interrupt ({}), {} so far", what, n + 1);
    }
}

/// Program the PIT (channel 0, square wave) to fire IRQ0 at HZ.
unsafe fn pit_init() {
    let divisor = (1_193_182 / HZ) as u16;
    Port::<u8>::new(0x43).write(0x36);
    Port::<u8>::new(0x40).write(divisor as u8);
    Port::<u8>::new(0x40).write((divisor >> 8) as u8);
}

/// Remap the PIC so IRQs land on vectors 32..47, then unmask only keyboard + mouse.
unsafe fn pic_init() {
    let mut m_cmd = Port::<u8>::new(PIC1_CMD);
    let mut m_data = Port::<u8>::new(PIC1_DATA);
    let mut s_cmd = Port::<u8>::new(PIC2_CMD);
    let mut s_data = Port::<u8>::new(PIC2_DATA);
    let mut wait = Port::<u8>::new(0x80);

    m_cmd.write(0x11);
    wait.write(0);
    s_cmd.write(0x11);
    wait.write(0);
    m_data.write(IRQ_BASE);
    wait.write(0);
    s_data.write(IRQ_BASE + 8);
    wait.write(0);
    m_data.write(4); // slave PIC is on IRQ2
    wait.write(0);
    s_data.write(2);
    wait.write(0);
    m_data.write(1); // 8086 mode
    wait.write(0);
    s_data.write(1);
    wait.write(0);

    // Masks: 1 = disabled. Enable IRQ0 (timer), IRQ1 (keyboard), IRQ2 (cascade), IRQ12 (mouse).
    m_data.write(!((1u8 << 0) | (1 << 1) | (1 << 2)));
    s_data.write(!(1u8 << 4));
}

fn eoi(irq: u8) {
    unsafe {
        if irq >= 8 {
            Port::<u8>::new(PIC2_CMD).write(0x20);
        }
        Port::<u8>::new(PIC1_CMD).write(0x20);
    }
}

extern "x86-interrupt" fn breakpoint(frame: InterruptStackFrame) {
    kprintln!("BREAKPOINT\n{:#?}", frame);
}

extern "x86-interrupt" fn double_fault(frame: InterruptStackFrame, _code: u64) -> ! {
    panic!("DOUBLE FAULT\n{:#?}", frame);
}

/// A model-specific register is being probed: a #GP on its RDMSR means "not here".
static MSR_PROBE: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);
static MSR_FAULTED: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// Read a model-specific register that this processor may not have (another make or
/// generation, or an emulator): None instead of a crash.
pub fn rdmsr_safe(msr: u32) -> Option<u64> {
    x86_64::instructions::interrupts::without_interrupts(|| {
        MSR_FAULTED.store(false, Ordering::SeqCst);
        MSR_PROBE.store(true, Ordering::SeqCst);
        let (lo, hi): (u32, u32);
        unsafe { core::arch::asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nostack)) };
        MSR_PROBE.store(false, Ordering::SeqCst);
        if MSR_FAULTED.swap(false, Ordering::SeqCst) {
            None
        } else {
            Some((hi as u64) << 32 | lo as u64)
        }
    })
}

extern "x86-interrupt" fn general_protection(mut frame: InterruptStackFrame, code: u64) {
    // A probed RDMSR (0F 32) faulted: skip it and say so.
    let ip = frame.instruction_pointer.as_u64();
    if MSR_PROBE.load(Ordering::SeqCst) && unsafe { core::ptr::read_volatile(ip as *const [u8; 2]) } == [0x0f, 0x32] {
        MSR_FAULTED.store(true, Ordering::SeqCst);
        unsafe { frame.as_mut().update(|f| f.instruction_pointer += 2u64) };
        return;
    }
    panic!("GENERAL PROTECTION FAULT (code {:#x})\n{:#?}", code, frame);
}

extern "x86-interrupt" fn page_fault(frame: InterruptStackFrame, code: PageFaultErrorCode) {
    panic!("PAGE FAULT at {:?}, {:?}\n{:#?}", Cr2::read(), code, frame);
}

extern "x86-interrupt" fn timer(_frame: InterruptStackFrame) {
    TICKS.fetch_add(1, Ordering::Relaxed);
    eoi(0);
}

extern "x86-interrupt" fn keyboard(_frame: InterruptStackFrame) {
    let sc = unsafe { Port::<u8>::new(0x60).read() };
    ps2::on_key_byte(sc);
    eoi(1);
}

extern "x86-interrupt" fn mouse(_frame: InterruptStackFrame) {
    let b = unsafe { Port::<u8>::new(0x60).read() };
    ps2::on_mouse_byte(b);
    eoi(12);
}

// ---- stray interrupts --------------------------------------------------------------------

/// Spurious IRQ 7: the PIC raises it without a real interrupt (no acknowledgement then).
extern "x86-interrupt" fn pic_irq7(_frame: InterruptStackFrame) {
    if pic_isr(PIC1_CMD) & 0x80 != 0 {
        eoi(7);
    }
    note_stray("IRQ 7");
}

/// Spurious IRQ 15: only the first PIC gets acknowledged.
extern "x86-interrupt" fn pic_irq15(_frame: InterruptStackFrame) {
    if pic_isr(PIC2_CMD) & 0x80 != 0 {
        eoi(15);
    } else {
        unsafe { Port::<u8>::new(PIC1_CMD).write(0x20) };
    }
    note_stray("IRQ 15");
}

extern "x86-interrupt" fn pic_master_stray(_frame: InterruptStackFrame) {
    eoi(0);
    note_stray("PIC");
}

extern "x86-interrupt" fn pic_slave_stray(_frame: InterruptStackFrame) {
    eoi(8);
    note_stray("PIC 2");
}

/// The local APIC's own spurious interrupt: needs no acknowledgement.
extern "x86-interrupt" fn lapic_spurious(_frame: InterruptStackFrame) {
    note_stray("APIC spurious");
}

/// Any other vector (an interrupt the firmware set up): acknowledge it at the APIC.
extern "x86-interrupt" fn stray(_frame: InterruptStackFrame) {
    lapic_eoi();
    note_stray("APIC");
}

// ---- CPU exceptions ----------------------------------------------------------------------

extern "x86-interrupt" fn nmi(_frame: InterruptStackFrame) {
    // Hardware errors and watchdogs; nothing Solon can fix, so carry on.
    note_stray("NMI");
}

extern "x86-interrupt" fn debug(_frame: InterruptStackFrame) {
    note_stray("debug");
}

macro_rules! fatal {
    ($name:ident, $msg:expr) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame) {
            panic!("{}\n{:#?}", $msg, frame);
        }
    };
    ($name:ident, $msg:expr, code) => {
        extern "x86-interrupt" fn $name(frame: InterruptStackFrame, code: u64) {
            panic!("{} (code {:#x})\n{:#?}", $msg, code, frame);
        }
    };
}

fatal!(divide_error, "DIVIDE BY ZERO");
fatal!(overflow, "OVERFLOW");
fatal!(bound_range, "BOUND RANGE EXCEEDED");
fatal!(invalid_opcode, "INVALID INSTRUCTION");
fatal!(device_not_available, "FPU NOT AVAILABLE");
fatal!(x87_floating_point, "x87 FLOATING POINT ERROR");
fatal!(simd_floating_point, "SIMD FLOATING POINT ERROR");
fatal!(virtualization, "VIRTUALIZATION EXCEPTION");
fatal!(invalid_tss, "INVALID TSS", code);
fatal!(segment_not_present, "SEGMENT NOT PRESENT", code);
fatal!(stack_segment_fault, "STACK SEGMENT FAULT", code);
fatal!(alignment_check, "ALIGNMENT CHECK", code);

extern "x86-interrupt" fn machine_check(frame: InterruptStackFrame) -> ! {
    panic!("MACHINE CHECK (the processor reported a hardware error)\n{:#?}", frame);
}

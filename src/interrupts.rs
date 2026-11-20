//! GDT, IDT and the legacy 8259 PIC.

use core::sync::atomic::{AtomicU64, Ordering};
use spin::Lazy;
use x86_64::instructions::port::Port;
use x86_64::instructions::segmentation::{Segment, CS, DS, ES, SS};
use x86_64::registers::control::Cr2;
use x86_64::structures::gdt::{Descriptor, GlobalDescriptorTable, SegmentSelector};
use x86_64::structures::idt::{InterruptDescriptorTable, InterruptStackFrame, PageFaultErrorCode};

use crate::ps2;

const PIC1_CMD: u16 = 0x20;
const PIC1_DATA: u16 = 0x21;
const PIC2_CMD: u16 = 0xA0;
const PIC2_DATA: u16 = 0xA1;

const IRQ_BASE: u8 = 32;
const VEC_TIMER: u8 = IRQ_BASE;
const VEC_KEYBOARD: u8 = IRQ_BASE + 1;

/// Timer ticks since boot (HZ per second).
pub static TICKS: AtomicU64 = AtomicU64::new(0);
pub const HZ: u64 = 100;
const VEC_MOUSE: u8 = IRQ_BASE + 12;

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
}

static GDT: Lazy<(GlobalDescriptorTable, Selectors)> = Lazy::new(|| {
    let mut gdt = GlobalDescriptorTable::new();
    let code = gdt.append(Descriptor::kernel_code_segment());
    let data = gdt.append(Descriptor::kernel_data_segment());
    (gdt, Selectors { code, data })
});

static IDT: Lazy<InterruptDescriptorTable> = Lazy::new(|| {
    let mut idt = InterruptDescriptorTable::new();
    idt.breakpoint.set_handler_fn(breakpoint);
    idt.double_fault.set_handler_fn(double_fault);
    idt.general_protection_fault.set_handler_fn(general_protection);
    idt.page_fault.set_handler_fn(page_fault);
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
    }
    IDT.load();
    unsafe {
        pic_init();
        pit_init();
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

extern "x86-interrupt" fn general_protection(frame: InterruptStackFrame, code: u64) {
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

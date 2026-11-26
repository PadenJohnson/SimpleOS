//! Running the firmware's ACPI code (AML), for laptop batteries and the charger.
//!
//! Every PC's firmware describes its devices in ACPI tables, and for some of them it supplies
//! small programs in a bytecode called AML: how to ask the battery for its charge, how to tell
//! whether the charger is plugged in. Windows and Linux run those programs, and so does Solon,
//! with the acpi crate's interpreter (vendored in vendor/acpi, with fixes found by running it
//! over real laptops' tables: tools/acpitest). `battery.rs` finds the devices and reads them.
//!
//! The interpreter runs on a worker core, never the first: it may wait on slow hardware, and
//! firmware code can do things nobody tested. If it panics anyway, the panic handler returns
//! here (`guard`) instead of showing the crash screen, and Solon just goes without the battery
//! icon. The embedded controller (EC), which most laptops' battery code talks to, is reached
//! through its two I/O ports, found in the firmware's description of it (`_CRS`).

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::sync::Arc;
use core::panic::PanicInfo;
use core::ptr::{read_volatile, write_volatile, NonNull};
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering::SeqCst};

use acpi::aml::op_region::{OpRegion, RegionHandler, RegionSpace};
use acpi::aml::{AmlError, Interpreter};
use acpi::registers::FixedRegisters;
use acpi::sdt::facs::Facs;
use acpi::sdt::fadt::Fadt;
use acpi::{AcpiTables, Handle, Handler, PciAddress, PhysicalMapping, RawPhysicalMapping};
use alloc::collections::BTreeMap;
use spin::Mutex;
use x86_64::instructions::port::Port;

use crate::battery::{self, Devices, Power};
use crate::interrupts::{HZ, TICKS};

/// How often the batteries are read.
const PERIOD: u64 = 5 * HZ;
/// Read what each battery is (`_BIF`) again every this many readings.
const INFO_EVERY: u32 = 60;
/// Give up after this many failed readings in a row.
const MAX_ERRORS: u32 = 5;

// ---- the interpreter's view of the machine -----------------------------------------------

/// Physical pages mapped outside the direct map (device registers the firmware's code uses).
static MAPPED: Mutex<BTreeMap<u64, (u64, u64)>> = Mutex::new(BTreeMap::new());

/// A virtual address for `len` bytes at physical `phys`: in the direct map if it's there
/// (RAM, ACPI tables), otherwise mapped uncached once and kept.
fn virt(phys: u64, len: u64) -> u64 {
    let hhdm = crate::memory::hhdm();
    let first = phys & !0xFFF;
    let last = (phys + len.max(1) - 1) & !0xFFF;
    if (first..=last).step_by(4096).all(|p| crate::memory::caching_of(hhdm + p).is_some()) {
        return hhdm + phys;
    }
    let mut m = MAPPED.lock();
    if let Some(&(v, pages)) = m.get(&first) {
        if first + pages * 4096 > last {
            return v + (phys - first);
        }
    }
    let pages = (last - first) / 4096 + 1;
    let v = crate::memory::map_mmio(first, pages * 4096);
    m.insert(first, (v, pages));
    v + (phys - first)
}

fn read_n(a: usize, n: usize) -> u64 {
    let v = virt(a as u64, n as u64);
    unsafe {
        match n {
            1 => read_volatile(v as *const u8) as u64,
            2 if v % 2 == 0 => read_volatile(v as *const u16) as u64,
            4 if v % 4 == 0 => read_volatile(v as *const u32) as u64,
            8 if v % 8 == 0 => read_volatile(v as *const u64),
            _ => (0..n).rev().fold(0, |acc, k| acc << 8 | read_volatile((v + k as u64) as *const u8) as u64),
        }
    }
}

fn write_n(a: usize, n: usize, value: u64) {
    let v = virt(a as u64, n as u64);
    unsafe {
        match n {
            1 => write_volatile(v as *mut u8, value as u8),
            2 if v % 2 == 0 => write_volatile(v as *mut u16, value as u16),
            4 if v % 4 == 0 => write_volatile(v as *mut u32, value as u32),
            8 if v % 8 == 0 => write_volatile(v as *mut u64, value),
            _ => (0..n).for_each(|k| write_volatile((v + k as u64) as *mut u8, (value >> (8 * k)) as u8)),
        }
    }
}

fn pci_at(a: PciAddress, off: u16) -> Option<(u8, u8, u8, u8)> {
    (a.segment() == 0 && off < 256).then_some((a.bus(), a.device(), a.function(), off as u8))
}

#[derive(Clone, Copy)]
pub struct Os;

impl Handler for Os {
    unsafe fn map_physical_region<T>(&self, physical_address: usize, size: usize) -> RawPhysicalMapping<T> {
        let v = virt(physical_address as u64, size.max(1) as u64);
        unsafe { RawPhysicalMapping::new(physical_address, NonNull::new(v as *mut T).expect("null mapping"), size, size) }
    }
    unsafe fn unmap_physical_region<T>(&self, _region: RawPhysicalMapping<T>) {}

    fn read_u8(&self, a: usize) -> u8 {
        read_n(a, 1) as u8
    }
    fn read_u16(&self, a: usize) -> u16 {
        read_n(a, 2) as u16
    }
    fn read_u32(&self, a: usize) -> u32 {
        read_n(a, 4) as u32
    }
    fn read_u64(&self, a: usize) -> u64 {
        read_n(a, 8)
    }
    fn write_u8(&self, a: usize, v: u8) {
        write_n(a, 1, v as u64)
    }
    fn write_u16(&self, a: usize, v: u16) {
        write_n(a, 2, v as u64)
    }
    fn write_u32(&self, a: usize, v: u32) {
        write_n(a, 4, v as u64)
    }
    fn write_u64(&self, a: usize, v: u64) {
        write_n(a, 8, v)
    }

    fn read_io_u8(&self, port: u16) -> u8 {
        unsafe { Port::<u8>::new(port).read() }
    }
    fn read_io_u16(&self, port: u16) -> u16 {
        unsafe { Port::<u16>::new(port).read() }
    }
    fn read_io_u32(&self, port: u16) -> u32 {
        unsafe { Port::<u32>::new(port).read() }
    }
    fn write_io_u8(&self, port: u16, v: u8) {
        unsafe { Port::<u8>::new(port).write(v) }
    }
    fn write_io_u16(&self, port: u16, v: u16) {
        unsafe { Port::<u16>::new(port).write(v) }
    }
    fn write_io_u32(&self, port: u16, v: u32) {
        unsafe { Port::<u32>::new(port).write(v) }
    }

    fn read_pci_u8(&self, a: PciAddress, off: u16) -> u8 {
        pci_at(a, off).map_or(0xFF, |(b, d, f, o)| crate::pci::read8(b, d, f, o))
    }
    fn read_pci_u16(&self, a: PciAddress, off: u16) -> u16 {
        pci_at(a, off).map_or(0xFFFF, |(b, d, f, o)| crate::pci::read16(b, d, f, o))
    }
    fn read_pci_u32(&self, a: PciAddress, off: u16) -> u32 {
        pci_at(a, off).map_or(0xFFFF_FFFF, |(b, d, f, o)| crate::pci::read32(b, d, f, o))
    }
    fn write_pci_u8(&self, a: PciAddress, off: u16, v: u8) {
        if let Some((b, d, f, o)) = pci_at(a, off) {
            crate::pci::write8(b, d, f, o, v)
        }
    }
    fn write_pci_u16(&self, a: PciAddress, off: u16, v: u16) {
        if let Some((b, d, f, o)) = pci_at(a, off) {
            crate::pci::write16(b, d, f, o, v)
        }
    }
    fn write_pci_u32(&self, a: PciAddress, off: u16, v: u32) {
        if let Some((b, d, f, o)) = pci_at(a, off) {
            crate::pci::write32(b, d, f, o, v)
        }
    }

    fn nanos_since_boot(&self) -> u64 {
        crate::cpu::now_us() * 1000
    }
    fn stall(&self, us: u64) {
        let end = crate::cpu::now_us() + us;
        while crate::cpu::now_us() < end {
            core::hint::spin_loop();
        }
    }
    fn sleep(&self, ms: u64) {
        // On a worker core: nothing else to do meanwhile.
        self.stall(ms * 1000);
    }

    // Only one core ever runs the firmware's code, one method at a time, so its mutexes are
    // always free.
    fn create_mutex(&self) -> Handle {
        static NEXT: AtomicU32 = AtomicU32::new(1);
        Handle(NEXT.fetch_add(1, SeqCst))
    }
    fn acquire(&self, _mutex: Handle, _timeout: u16) -> Result<(), AmlError> {
        Ok(())
    }
    fn release(&self, _mutex: Handle) {}
}

// ---- the embedded controller --------------------------------------------------------------

/// The EC's standard interface (ACPI 12.2): a data port and a command/status port. Reading
/// byte `a` is command 0x80, then `a`, then the answer; writing is 0x81, `a`, the value.
struct Ec {
    data: u16,
    cmd: u16,
}

const EC_OBF: u8 = 0x01; // output buffer full: a byte to read
const EC_IBF: u8 = 0x02; // input buffer full: the EC hasn't taken the last byte yet

static EC_TIMEOUTS: AtomicU32 = AtomicU32::new(0);

impl Ec {
    fn status(&self) -> u8 {
        unsafe { Port::<u8>::new(self.cmd).read() }
    }

    fn wait(&self, ready: impl Fn(u8) -> bool) -> Result<(), AmlError> {
        let end = crate::cpu::now_us() + 50_000;
        loop {
            if ready(self.status()) {
                return Ok(());
            }
            if crate::cpu::now_us() > end {
                if EC_TIMEOUTS.fetch_add(1, SeqCst) < 3 {
                    kprintln!("battery: the embedded controller didn't answer (status {:#x})", self.status());
                }
                return Err(AmlError::HostError(String::from("embedded controller timeout")));
            }
            core::hint::spin_loop();
        }
    }

    fn send(&self, port: u16, v: u8) -> Result<(), AmlError> {
        self.wait(|s| s & EC_IBF == 0)?;
        unsafe { Port::<u8>::new(port).write(v) };
        Ok(())
    }

    fn read(&self, at: u64) -> Result<u8, AmlError> {
        let at = u8::try_from(at).map_err(|_| AmlError::HostError(String::from("EC address past 255")))?;
        // A stale byte the firmware left would be taken for the answer.
        if self.status() & EC_OBF != 0 {
            unsafe { Port::<u8>::new(self.data).read() };
        }
        self.send(self.cmd, 0x80)?;
        self.send(self.data, at)?;
        self.wait(|s| s & EC_OBF != 0)?;
        Ok(unsafe { Port::<u8>::new(self.data).read() })
    }

    fn write(&self, at: u64, v: u8) -> Result<(), AmlError> {
        let at = u8::try_from(at).map_err(|_| AmlError::HostError(String::from("EC address past 255")))?;
        self.send(self.cmd, 0x81)?;
        self.send(self.data, at)?;
        self.send(self.data, v)?;
        self.wait(|s| s & EC_IBF == 0)
    }

    fn read_n(&self, at: u64, n: usize) -> Result<u64, AmlError> {
        let mut v = 0;
        for k in (0..n).rev() {
            v = v << 8 | self.read(at + k as u64)? as u64;
        }
        Ok(v)
    }

    fn write_n(&self, at: u64, n: usize, v: u64) -> Result<(), AmlError> {
        for k in 0..n {
            self.write(at + k as u64, (v >> (8 * k)) as u8)?;
        }
        Ok(())
    }
}

impl RegionHandler for Ec {
    fn read_u8(&self, r: &OpRegion) -> Result<u8, AmlError> {
        self.read(r.base)
    }
    fn read_u16(&self, r: &OpRegion) -> Result<u16, AmlError> {
        self.read_n(r.base, 2).map(|v| v as u16)
    }
    fn read_u32(&self, r: &OpRegion) -> Result<u32, AmlError> {
        self.read_n(r.base, 4).map(|v| v as u32)
    }
    fn read_u64(&self, r: &OpRegion) -> Result<u64, AmlError> {
        self.read_n(r.base, 8)
    }
    fn write_u8(&self, r: &OpRegion, v: u8) -> Result<(), AmlError> {
        self.write(r.base, v)
    }
    fn write_u16(&self, r: &OpRegion, v: u16) -> Result<(), AmlError> {
        self.write_n(r.base, 2, v as u64)
    }
    fn write_u32(&self, r: &OpRegion, v: u32) -> Result<(), AmlError> {
        self.write_n(r.base, 4, v as u64)
    }
    fn write_u64(&self, r: &OpRegion, v: u64) -> Result<(), AmlError> {
        self.write_n(r.base, 8, v)
    }
}

// ---- a panic in the firmware's code comes back here -----------------------------------------

pub mod guard {
    use core::panic::PanicInfo;
    use core::sync::atomic::{AtomicU32, Ordering::SeqCst};

    /// The callee-saved registers, stack pointer and return address of `call_saving`.
    #[repr(C)]
    struct Jump([u64; 8]);

    static mut JUMP: Jump = Jump([0; 8]);
    /// The core running guarded code (its APIC ID), or none.
    static CORE: AtomicU32 = AtomicU32::new(u32::MAX);

    fn core_id() -> u32 {
        core::arch::x86_64::__cpuid(1).ebx >> 24
    }

    /// Save where to come back to, then call `f(arg)`. Returns 0 when `f` returns, 1 when a
    /// panic inside it jumped back (`jump_back`).
    #[unsafe(naked)]
    unsafe extern "C" fn call_saving(jump: *mut Jump, f: extern "C" fn(*mut u8), arg: *mut u8) -> u64 {
        core::arch::naked_asm!(
            "mov [rdi], rbx",
            "mov [rdi + 8], rbp",
            "mov [rdi + 16], r12",
            "mov [rdi + 24], r13",
            "mov [rdi + 32], r14",
            "mov [rdi + 40], r15",
            "lea rax, [rsp + 8]",
            "mov [rdi + 48], rax",
            "mov rax, [rsp]",
            "mov [rdi + 56], rax",
            "sub rsp, 8", // keep the stack lined up to 16 bytes for the call
            "mov rax, rsi",
            "mov rdi, rdx",
            "call rax",
            "add rsp, 8",
            "xor eax, eax",
            "ret",
        )
    }

    /// Return from `call_saving` once more, with 1, abandoning everything called since.
    #[unsafe(naked)]
    unsafe extern "C" fn jump_back(jump: *const Jump) -> ! {
        core::arch::naked_asm!(
            "mov rbx, [rdi]",
            "mov rbp, [rdi + 8]",
            "mov r12, [rdi + 16]",
            "mov r13, [rdi + 24]",
            "mov r14, [rdi + 32]",
            "mov r15, [rdi + 40]",
            "mov rsp, [rdi + 48]",
            "mov eax, 1",
            "jmp qword ptr [rdi + 56]",
        )
    }

    /// Run `f`; None if it panicked. What it was doing is abandoned, not cleaned up (memory
    /// leaks and its locks stay taken), so whatever it worked on must not be used again.
    /// One core at a time.
    pub fn run<R, F: FnOnce() -> R>(f: F) -> Option<R> {
        struct Slot<F, R> {
            f: Option<F>,
            r: Option<R>,
        }
        extern "C" fn call<F: FnOnce() -> R, R>(p: *mut u8) {
            let slot = unsafe { &mut *(p as *mut Slot<F, R>) };
            if let Some(f) = slot.f.take() {
                slot.r = Some(f());
            }
        }
        let mut slot = Slot { f: Some(f), r: None };
        CORE.store(core_id(), SeqCst);
        let panicked = unsafe { call_saving(&raw mut JUMP, call::<F, R>, &mut slot as *mut Slot<F, R> as *mut u8) };
        CORE.store(u32::MAX, SeqCst);
        if panicked != 0 {
            None
        } else {
            slot.r.take()
        }
    }

    /// From the panic handler: if this core is in `run`, go back there.
    pub fn catch(info: &PanicInfo) {
        if CORE.load(SeqCst) == core_id() {
            CORE.store(u32::MAX, SeqCst);
            kprintln!("battery: the firmware's code stopped Solon's ACPI interpreter: {}", info);
            unsafe { jump_back(&raw const JUMP) }
        }
    }
}

/// For the panic handler.
pub fn catch_panic(info: &PanicInfo) {
    guard::catch(info);
}

// ---- reading the batteries in the background ----------------------------------------------

struct Machine {
    interp: Interpreter<Os>,
    devices: Devices,
    last: Option<Power>,
    reads: u32,
    errors: u32,
}

static MACHINE: Mutex<Option<Machine>> = Mutex::new(None);
/// The latest reading, and a number that changes with it.
static LATEST: Mutex<Option<Power>> = Mutex::new(None);
static VERSION: AtomicU64 = AtomicU64::new(0);
/// A reading is under way on a worker core.
static BUSY: AtomicBool = AtomicBool::new(false);
/// Never again: no battery, turned off, or the firmware's code failed for good.
static STOPPED: AtomicBool = AtomicBool::new(false);
static NEXT: AtomicU64 = AtomicU64::new(0);

/// The crate's warnings go to the log (a few: some firmware warns a lot).
struct Logger;
static LOG_LINES: AtomicU32 = AtomicU32::new(0);

impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::Level::Warn
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) && LOG_LINES.fetch_add(1, SeqCst) < 30 {
            kprintln!("acpi: {}", r.args());
        }
    }
    fn flush(&self) {}
}
static LOGGER: Logger = Logger;

/// The RSDP's physical address (0: not started), whether the first reading worked, and
/// boot option "batterycrashtest".
static RSDP: AtomicU64 = AtomicU64::new(0);
static READY: AtomicBool = AtomicBool::new(false);
static CRASH_TEST: AtomicBool = AtomicBool::new(false);

/// Start reading the batteries, if the firmware describes any (on a worker core, as soon as
/// one is free; the first reading arrives a moment later). Boot option "nobattery" leaves
/// the firmware's code alone; "batterycrashtest" checks a panic in it is survived.
pub fn start(rsdp_phys: u64, cmdline: &str) {
    if cmdline.split_whitespace().any(|w| w == "nobattery") {
        kprintln!("battery: not read (boot option nobattery)");
        STOPPED.store(true, SeqCst);
        return;
    }
    if crate::smp::threads() < 2 {
        // The firmware's code doesn't run on the first core (see the top of this file).
        kprintln!("battery: not read (only one processor core in use)");
        STOPPED.store(true, SeqCst);
        return;
    }
    let _ = log::set_logger(&LOGGER).map(|()| log::set_max_level(log::LevelFilter::Warn));
    CRASH_TEST.store(cmdline.split_whitespace().any(|w| w == "batterycrashtest"), SeqCst);
    RSDP.store(rsdp_phys, SeqCst);
    NEXT.store(0, SeqCst);
    poll(TICKS.load(SeqCst));
}

/// The first reading: find everything and set up the interpreter.
fn first_reading() {
    let t0 = crate::cpu::now_us();
    let rsdp = RSDP.load(SeqCst);
    let crash = CRASH_TEST.load(SeqCst);
    match guard::run(|| {
        if crash {
            panic!("battery crash test requested on the boot command line");
        }
        init(rsdp)
    }) {
        Some(Ok(Some(m))) => {
            kprintln!(
                "battery: {} bay(s), {} charger(s), EC {} ({} ms)",
                m.devices.batteries.len(),
                m.devices.adapters.len(),
                m.devices.ec.as_ref().map_or(String::from("none"), |(p, d, c)| format!("{} at {:#x}/{:#x}", p.as_string(), d, c)),
                (crate::cpu::now_us() - t0) / 1000
            );
            if let Some(p) = &m.last {
                kprintln!("battery: {}", battery::describe(p));
                publish(p.clone());
            }
            *MACHINE.lock() = Some(m);
            READY.store(true, SeqCst);
        }
        Some(Ok(None)) => {
            kprintln!("battery: none (the firmware describes no battery)");
            STOPPED.store(true, SeqCst);
        }
        Some(Err(e)) => {
            kprintln!("battery: can't be read: {}", e);
            STOPPED.store(true, SeqCst);
        }
        None => {
            kprintln!("battery: not read; Solon carries on without it");
            STOPPED.store(true, SeqCst);
        }
    }
}

fn aml_of(t: &acpi::AmlTable) -> &'static [u8] {
    let v = virt(t.phys_address as u64, t.length as u64);
    unsafe { core::slice::from_raw_parts((v + 36) as *const u8, (t.length as usize).saturating_sub(36)) }
}

fn init(rsdp: u64) -> Result<Option<Machine>, String> {
    let tables = unsafe { AcpiTables::from_rsdp(Os, rsdp as usize) }.map_err(|e| format!("tables: {:?}", e))?;
    let dsdt = tables.dsdt().map_err(|e| format!("no DSDT: {:?}", e))?;
    if !core::iter::once(dsdt).chain(tables.ssdts()).any(|t| battery::mentions_battery(aml_of(&t))) {
        return Ok(None);
    }
    let fadt = tables.find_table::<Fadt>().ok_or("no FADT")?;
    let regs = FixedRegisters::new(&fadt, Os).map_err(|e| format!("registers: {:?}", e))?;
    let facs = fadt.facs_address().ok().filter(|&a| a != 0).map(|a| unsafe { PhysicalMapping::new(a, core::mem::size_of::<Facs>(), Os) });
    let interp = Interpreter::new(Os, dsdt.revision, Arc::new(regs), facs);
    interp.load_table(aml_of(&dsdt)).map_err(|e| format!("DSDT: {:?}", e))?;
    for t in tables.ssdts() {
        if let Err(e) = interp.load_table(aml_of(&t)) {
            kprintln!("acpi: an SSDT at {:#x} stopped loading: {:?}", t.phys_address, e);
        }
    }
    // As Linux does without an ECDT: devices first, then the EC's address space, then `_REG`.
    interp.initialize_namespace();
    let devices = battery::find(&interp);
    if devices.batteries.is_empty() {
        return Ok(None);
    }
    if let Some((_, data, cmd)) = devices.ec {
        interp.install_region_handler(RegionSpace::EmbeddedControl, Box::new(Ec { data, cmd }));
        if let Err(e) = battery::connect_ec(&interp, &devices) {
            kprintln!("acpi: the EC's _REG failed: {:?}", e);
        }
    }
    let last = battery::read(&interp, &devices, None, true).map_err(|e| format!("first reading: {:?}", e))?;
    Ok(Some(Machine { interp, devices, last: Some(last), reads: 1, errors: 0 }))
}

fn publish(p: Power) {
    let mut l = LATEST.lock();
    if l.as_ref() != Some(&p) {
        // Log what a person would notice: plugged in or not, and every 10%.
        let step = |p: &Power| (p.on_ac, p.charging(), p.percent().map(|v| v / 10));
        if l.as_ref().map(step) != Some(step(&p)) && l.is_some() {
            kprintln!("battery: {}", battery::describe(&p));
        }
        *l = Some(p);
        VERSION.fetch_add(1, SeqCst);
    }
}

/// Call often (the main loop): starts a reading every few seconds.
pub fn poll(now: u64) {
    if STOPPED.load(SeqCst) || RSDP.load(SeqCst) == 0 || BUSY.load(SeqCst) || now < NEXT.load(SeqCst) {
        return;
    }
    NEXT.store(now + PERIOD, SeqCst);
    BUSY.store(true, SeqCst);
    let task: crate::smp::Task = if READY.load(SeqCst) {
        Box::new(|| {
            next_reading();
            BUSY.store(false, SeqCst);
        })
    } else {
        Box::new(|| {
            first_reading();
            NEXT.store(TICKS.load(SeqCst) + PERIOD, SeqCst);
            BUSY.store(false, SeqCst);
        })
    };
    if crate::smp::spawn(task).is_err() {
        // Every core busy (the boot screen's animation, a video): try again in a second.
        BUSY.store(false, SeqCst);
        NEXT.store(now + HZ, SeqCst);
    }
}

fn next_reading() {
    let r = guard::run(|| {
        let mut g = MACHINE.lock();
        let m = g.as_mut()?;
        m.reads += 1;
        let refresh = m.reads % INFO_EVERY == 0;
        Some(match battery::read(&m.interp, &m.devices, m.last.as_ref(), refresh) {
            Ok(p) => {
                m.errors = 0;
                m.last = Some(p.clone());
                Ok(p)
            }
            Err(e) => {
                m.errors += 1;
                Err((e, m.errors))
            }
        })
    });
    match r {
        Some(Some(Ok(p))) => publish(p),
        Some(Some(Err((e, n)))) => {
            kprintln!("battery: reading failed: {:?}", e);
            if n >= MAX_ERRORS {
                kprintln!("battery: giving up after {} failed readings", n);
                STOPPED.store(true, SeqCst);
            }
        }
        Some(None) => STOPPED.store(true, SeqCst),
        // Panicked: the interpreter (and the lock on it) are left as they were, unused.
        None => STOPPED.store(true, SeqCst),
    }
}

/// Changes whenever there's a new reading.
pub fn version() -> u64 {
    VERSION.load(SeqCst)
}

/// The latest reading (None: no battery, or not read yet).
pub fn latest() -> Option<Power> {
    LATEST.lock().clone()
}

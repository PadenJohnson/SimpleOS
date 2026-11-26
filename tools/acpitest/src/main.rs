//! acpitest: runs Solon's battery code (src/battery.rs) on the host against a laptop's ACPI
//! tables, so it can be checked on machines nobody here owns.
//!
//! Usage: acpitest <folder>...
//!
//! Each folder holds one machine's tables as binary files: dsdt, ssdt* (.aml or .dat) and, if
//! there is one, facp.dat (the FADT). If it also has expected.txt, the reading must be exactly
//! that (tests/basics is such a machine, written for this). `fetch.sh` makes such folders from the dumps people have
//! shared at github.com/linuxhw/ACPI. The tables are laid out in pretend physical memory with
//! an RSDP and XSDT in front, and loaded by the acpi crate exactly as Solon loads them. The
//! embedded controller is pretend too: 256 registers holding made-up but plausible numbers,
//! so the firmware's battery code runs its real path (its answers are nonsense, but any
//! error, panic or endless loop shows up). With ACPITEST_EC=done, the registers start at 0
//! and one the firmware writes reads 0 again afterwards, as when the EC has finished a
//! request (SMBus transfers).


extern crate alloc;

#[path = "../../../src/battery.rs"]
#[allow(dead_code)]
mod battery;

use std::collections::HashMap;
use std::path::Path;
use std::ptr::NonNull;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use acpi::aml::op_region::{OpRegion, RegionHandler, RegionSpace};
use acpi::aml::{AmlError, Interpreter};
use acpi::{AcpiTables, Handle, Handler, PciAddress, PhysicalMapping, RawPhysicalMapping};

/// Where the pretend tables live in pretend physical memory.
const TABLES_AT: usize = 0x10_0000;

#[derive(Clone)]
struct Host {
    /// The tables (from TABLES_AT on).
    tables: Arc<Vec<u8>>,
    /// Everything else the firmware writes, byte by byte.
    mem: Arc<Mutex<HashMap<usize, u8>>>,
    io_reads: Arc<AtomicU32>,
}

impl Handler for Host {
    unsafe fn map_physical_region<T>(&self, physical_address: usize, size: usize) -> RawPhysicalMapping<T> {
        let off = physical_address.checked_sub(TABLES_AT).filter(|o| o + size <= self.tables.len());
        let ptr = match off {
            Some(o) => self.tables.as_ptr().wrapping_add(o) as *mut T,
            None => {
                // Not a table: hand out zeros (leaked; the tool runs briefly).
                let v: &'static mut [u8] = Box::leak(vec![0u8; size.max(std::mem::size_of::<T>()) + 64].into_boxed_slice());
                v.as_mut_ptr() as *mut T
            }
        };
        unsafe { RawPhysicalMapping::new(physical_address, NonNull::new(ptr).unwrap(), size, size) }
    }
    unsafe fn unmap_physical_region<T>(&self, _region: RawPhysicalMapping<T>) {}

    fn read_u8(&self, a: usize) -> u8 {
        *self.mem.lock().unwrap().get(&a).unwrap_or(&0)
    }
    fn read_u16(&self, a: usize) -> u16 {
        self.read_u8(a) as u16 | (self.read_u8(a + 1) as u16) << 8
    }
    fn read_u32(&self, a: usize) -> u32 {
        self.read_u16(a) as u32 | (self.read_u16(a + 2) as u32) << 16
    }
    fn read_u64(&self, a: usize) -> u64 {
        self.read_u32(a) as u64 | (self.read_u32(a + 4) as u64) << 32
    }
    fn write_u8(&self, a: usize, v: u8) {
        self.mem.lock().unwrap().insert(a, v);
    }
    fn write_u16(&self, a: usize, v: u16) {
        self.write_u8(a, v as u8);
        self.write_u8(a + 1, (v >> 8) as u8);
    }
    fn write_u32(&self, a: usize, v: u32) {
        self.write_u16(a, v as u16);
        self.write_u16(a + 2, (v >> 16) as u16);
    }
    fn write_u64(&self, a: usize, v: u64) {
        self.write_u32(a, v as u32);
        self.write_u32(a + 4, (v >> 32) as u32);
    }

    // I/O ports: nothing there (reads float high), except that status bits firmware loops on
    // look idle.
    fn read_io_u8(&self, _port: u16) -> u8 {
        self.io_reads.fetch_add(1, Ordering::Relaxed);
        0
    }
    fn read_io_u16(&self, _port: u16) -> u16 {
        self.io_reads.fetch_add(1, Ordering::Relaxed);
        0
    }
    fn read_io_u32(&self, _port: u16) -> u32 {
        self.io_reads.fetch_add(1, Ordering::Relaxed);
        0
    }
    fn write_io_u8(&self, _port: u16, _value: u8) {}
    fn write_io_u16(&self, _port: u16, _value: u16) {}
    fn write_io_u32(&self, _port: u16, _value: u32) {}

    fn read_pci_u8(&self, _a: PciAddress, _o: u16) -> u8 {
        0xFF
    }
    fn read_pci_u16(&self, _a: PciAddress, _o: u16) -> u16 {
        0xFFFF
    }
    fn read_pci_u32(&self, _a: PciAddress, _o: u16) -> u32 {
        0xFFFF_FFFF
    }
    fn write_pci_u8(&self, _a: PciAddress, _o: u16, _v: u8) {}
    fn write_pci_u16(&self, _a: PciAddress, _o: u16, _v: u16) {}
    fn write_pci_u32(&self, _a: PciAddress, _o: u16, _v: u32) {}

    fn nanos_since_boot(&self) -> u64 {
        static START: std::sync::OnceLock<Instant> = std::sync::OnceLock::new();
        START.get_or_init(Instant::now).elapsed().as_nanos() as u64
    }
    fn stall(&self, _us: u64) {}
    fn sleep(&self, _ms: u64) {}

    fn create_mutex(&self) -> Handle {
        static NEXT: AtomicU32 = AtomicU32::new(1);
        Handle(NEXT.fetch_add(1, Ordering::Relaxed))
    }
    fn acquire(&self, _mutex: Handle, _timeout: u16) -> Result<(), AmlError> {
        Ok(())
    }
    fn release(&self, _mutex: Handle) {}
}

/// The pretend embedded controller.
struct Ec {
    regs: Mutex<[u8; 256]>,
    reads: AtomicU32,
}

impl Ec {
    fn new() -> Ec {
        // Plausible values: mostly small numbers, so capacities and rates come out nonzero.
        let mut regs = [0u8; 256];
        if std::env::var_os("ACPITEST_EC").is_none_or(|v| v != "done") {
            for (i, r) in regs.iter_mut().enumerate() {
                *r = (0x20 + i * 7) as u8;
            }
        }
        Ec { regs: Mutex::new(regs), reads: AtomicU32::new(0) }
    }
    fn get(&self, at: u64, n: usize) -> u64 {
        self.reads.fetch_add(1, Ordering::Relaxed);
        let r = self.regs.lock().unwrap();
        (0..n).rev().fold(0u64, |v, k| v << 8 | r[(at as usize + k) & 0xFF] as u64)
    }
    fn set(&self, at: u64, n: usize, v: u64) {
        // ACPITEST_EC=done: a register written reads 0 afterwards, as an EC that has carried
        // out the request (an SMBus transfer's protocol register, say) clears it.
        let done = std::env::var_os("ACPITEST_EC").is_some_and(|v| v == "done");
        let mut r = self.regs.lock().unwrap();
        for k in 0..n {
            r[(at as usize + k) & 0xFF] = if done { 0 } else { (v >> (8 * k)) as u8 };
        }
    }
}

struct EcHandler(Arc<Ec>);

impl RegionHandler for EcHandler {
    fn read_u8(&self, r: &OpRegion) -> Result<u8, AmlError> {
        Ok(self.0.get(r.base, 1) as u8)
    }
    fn read_u16(&self, r: &OpRegion) -> Result<u16, AmlError> {
        Ok(self.0.get(r.base, 2) as u16)
    }
    fn read_u32(&self, r: &OpRegion) -> Result<u32, AmlError> {
        Ok(self.0.get(r.base, 4) as u32)
    }
    fn read_u64(&self, r: &OpRegion) -> Result<u64, AmlError> {
        Ok(self.0.get(r.base, 8))
    }
    fn write_u8(&self, r: &OpRegion, v: u8) -> Result<(), AmlError> {
        self.0.set(r.base, 1, v as u64);
        Ok(())
    }
    fn write_u16(&self, r: &OpRegion, v: u16) -> Result<(), AmlError> {
        self.0.set(r.base, 2, v as u64);
        Ok(())
    }
    fn write_u32(&self, r: &OpRegion, v: u32) -> Result<(), AmlError> {
        self.0.set(r.base, 4, v as u64);
        Ok(())
    }
    fn write_u64(&self, r: &OpRegion, v: u64) -> Result<(), AmlError> {
        self.0.set(r.base, 8, v);
        Ok(())
    }
}

struct Logger;
impl log::Log for Logger {
    fn enabled(&self, m: &log::Metadata) -> bool {
        m.level() <= log::max_level()
    }
    fn log(&self, r: &log::Record) {
        if self.enabled(r.metadata()) {
            println!("    acpi {}: {}", r.level(), r.args());
        }
    }
    fn flush(&self) {}
}

fn checksum(t: &mut [u8], at: usize) {
    t[at] = 0;
    let s = t.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    t[at] = 0u8.wrapping_sub(s);
}

fn header(sig: &[u8; 4], len: usize, rev: u8) -> Vec<u8> {
    let mut h = vec![0u8; 36];
    h[0..4].copy_from_slice(sig);
    h[4..8].copy_from_slice(&(len as u32).to_le_bytes());
    h[8] = rev;
    h[10..16].copy_from_slice(b"SOLON ");
    h[16..24].copy_from_slice(b"ACPITEST");
    h
}

/// Lay out RSDP, XSDT, FADT, FACS, DSDT and SSDTs; returns the memory and the RSDP address.
fn build(dir: &Path) -> Result<(Vec<u8>, usize), String> {
    let dsdt = std::fs::read(dir.join("dsdt.aml")).or_else(|_| std::fs::read(dir.join("dsdt.dat"))).map_err(|e| format!("dsdt.aml: {e}"))?;
    let mut ssdts: Vec<_> = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("ssdt") && (n.ends_with(".aml") || n.ends_with(".dat"))))
        .collect();
    // In the order the firmware lists them (ssdt2 before ssdt10): one may build on another.
    ssdts.sort_by_key(|p| {
        let n = p.file_stem().and_then(|n| n.to_str()).unwrap_or("");
        n.trim_start_matches("ssdt").parse::<u32>().unwrap_or(0)
    });
    let mut fadt = match std::fs::read(dir.join("facp.dat")) {
        Ok(f) if f.len() >= 116 => f,
        _ => {
            // A minimal FADT: PM1a event block at 0x400, control at 0x404 (ACPI 2+ layout).
            let mut f = header(b"FACP", 244, 4);
            f.resize(244, 0);
            f[56..60].copy_from_slice(&0x400u32.to_le_bytes());
            f[64..68].copy_from_slice(&0x404u32.to_le_bytes());
            f[88] = 4;
            f[89] = 2;
            f
        }
    };
    let mut mem = vec![0u8; 0x1000];
    let place = |mem: &mut Vec<u8>, t: &[u8]| -> usize {
        while mem.len() % 16 != 0 {
            mem.push(0);
        }
        let at = TABLES_AT + mem.len();
        mem.extend_from_slice(t);
        at
    };
    let mut facs = vec![0u8; 64];
    facs[0..4].copy_from_slice(b"FACS");
    facs[4..8].copy_from_slice(&64u32.to_le_bytes());
    let facs_at = place(&mut mem, &facs);
    let dsdt_at = place(&mut mem, &dsdt);
    // FADT: point it at our DSDT and FACS (both the 32-bit and, if long enough, 64-bit fields).
    fadt[36..40].copy_from_slice(&(facs_at as u32).to_le_bytes());
    fadt[40..44].copy_from_slice(&(dsdt_at as u32).to_le_bytes());
    if fadt.len() >= 148 {
        fadt[132..140].copy_from_slice(&(facs_at as u64).to_le_bytes());
        fadt[140..148].copy_from_slice(&(dsdt_at as u64).to_le_bytes());
    }
    let flen = fadt.len();
    fadt[4..8].copy_from_slice(&(flen as u32).to_le_bytes());
    checksum(&mut fadt, 9);
    let mut entries = vec![place(&mut mem, &fadt)];
    for s in &ssdts {
        let t = std::fs::read(s).map_err(|e| e.to_string())?;
        entries.push(place(&mut mem, &t));
    }
    let mut xsdt = header(b"XSDT", 36 + 8 * entries.len(), 1);
    for e in &entries {
        xsdt.extend_from_slice(&(*e as u64).to_le_bytes());
    }
    checksum(&mut xsdt, 9);
    let xsdt_at = place(&mut mem, &xsdt);
    let mut rsdp = vec![0u8; 36];
    rsdp[0..8].copy_from_slice(b"RSD PTR ");
    rsdp[9..15].copy_from_slice(b"SOLON ");
    rsdp[15] = 2;
    rsdp[20..24].copy_from_slice(&36u32.to_le_bytes());
    rsdp[24..32].copy_from_slice(&(xsdt_at as u64).to_le_bytes());
    let first: u8 = rsdp[..20].iter().fold(0u8, |a, &b| a.wrapping_add(b));
    rsdp[8] = 0u8.wrapping_sub(first);
    let all: u8 = rsdp.iter().fold(0u8, |a, &b| a.wrapping_add(b));
    rsdp[32] = 0u8.wrapping_sub(all);
    let rsdp_at = TABLES_AT + 0x100;
    mem[0x100..0x100 + 36].copy_from_slice(&rsdp);
    Ok((mem, rsdp_at))
}

fn run(dir: &Path) -> Result<String, String> {
    let (mem, rsdp) = build(dir)?;
    let mentions = std::fs::read_dir(dir)
        .map_err(|e| e.to_string())?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().extension().is_some_and(|x| x == "aml") || e.file_name().to_str().is_some_and(|n| (n.starts_with("dsdt") || n.starts_with("ssdt")) && n.ends_with(".dat")))
        .any(|e| battery::mentions_battery(&std::fs::read(e.path()).unwrap_or_default()));
    if !mentions {
        return Ok(String::from("no battery in the tables (Solon leaves it alone)"));
    }
    let host = Host { tables: Arc::new(mem), mem: Arc::default(), io_reads: Arc::default() };
    let t0 = Instant::now();
    let tables = unsafe { AcpiTables::from_rsdp(host.clone(), rsdp) }.map_err(|e| format!("tables: {e:?}"))?;
    let interp = solon_interpreter(&tables, host.clone())?;
    let ec = Arc::new(Ec::new());
    interp.install_region_handler(RegionSpace::EmbeddedControl, Box::new(EcHandler(ec.clone())));
    let load = t0.elapsed();
    interp.initialize_namespace();
    let init = t0.elapsed();
    let devices = battery::find(&interp);
    println!(
        "    {} batteries, {} adapters, EC {:?}",
        devices.batteries.len(),
        devices.adapters.len(),
        devices.ec.as_ref().map(|(p, d, c)| format!("{} ports {:#x}/{:#x}", p.as_string(), d, c))
    );
    if let Err(e) = battery::connect_ec(&interp, &devices) {
        println!("    _REG: {e:?}");
    }
    let p = match battery::read(&interp, &devices, None, true) {
        Ok(p) => p,
        Err(e) => {
            // Which method on which device: try each on its own.
            for dev in devices.batteries.iter().chain(devices.adapters.iter()) {
                for m in ["_STA", "_BIX", "_BIF", "_BST", "_PSR"] {
                    if let Err(e) = battery::eval(&interp, dev, m, vec![]) {
                        println!("    {}.{}: {:?}", dev.as_string(), m, e);
                    }
                }
            }
            return Err(format!("reading: {e:?}"));
        }
    };
    let p2 = battery::read(&interp, &devices, Some(&p), false).map_err(|e| format!("reading again: {e:?}"))?;
    let all = t0.elapsed();
    // A machine made for the test knows its answers.
    if let Ok(want) = std::fs::read_to_string(dir.join("expected.txt")) {
        let got = battery::describe(&p2);
        if got != want.trim() {
            return Err(format!("expected\n    {}\n  got\n    {}", want.trim(), got));
        }
    }
    Ok(format!(
        "{} | load {} ms, init {} ms, all {} ms, EC reads {}, port reads {}, same twice {}",
        battery::describe(&p2),
        load.as_millis(),
        init.as_millis(),
        all.as_millis(),
        ec.reads.load(Ordering::Relaxed),
        host.io_reads.load(Ordering::Relaxed),
        p == p2
    ))
}

/// The same steps as Solon's `aml.rs`: an interpreter over the DSDT and every SSDT.
fn solon_interpreter(tables: &AcpiTables<Host>, host: Host) -> Result<Interpreter<Host>, String> {
    use acpi::sdt::fadt::Fadt;
    let fadt = tables.find_table::<Fadt>().ok_or("no FADT")?;
    let regs = acpi::registers::FixedRegisters::new(&fadt, host.clone()).map_err(|e| format!("registers: {e:?}"))?;
    let facs = fadt.facs_address().ok().map(|a| unsafe { PhysicalMapping::new(a, 64, host.clone()) });
    let dsdt = tables.dsdt().map_err(|e| format!("dsdt: {e:?}"))?;
    let interp = Interpreter::new(host.clone(), dsdt.revision, Arc::new(regs), facs);
    let load = |t: acpi::AmlTable| -> Result<(), String> {
        let m = unsafe { host.map_physical_region::<u8>(t.phys_address, t.length as usize) };
        let s = unsafe { std::slice::from_raw_parts(m.virtual_start().as_ptr().add(36), t.length as usize - 36) };
        interp.load_table(s).map_err(|e| format!("loading table at {:#x}: {e:?}", t.phys_address))
    };
    load(dsdt)?;
    for s in tables.ssdts() {
        if let Err(e) = load(s) {
            println!("    {e} (skipped)");
        }
    }
    Ok(interp)
}

fn main() {
    log::set_logger(&Logger).unwrap();
    // ACPITEST_TRACE=1: every operation the interpreter does (a lot).
    let trace = std::env::var_os("ACPITEST_TRACE").is_some();
    log::set_max_level(if trace { log::LevelFilter::Trace } else { log::LevelFilter::Warn });
    let dirs: Vec<String> = std::env::args().skip(1).collect();
    if dirs.is_empty() {
        eprintln!("usage: acpitest <folder>...");
        std::process::exit(2);
    }
    let (mut ok, mut bad) = (0, 0);
    for d in &dirs {
        println!("{d}:");
        let dir = Path::new(d).to_path_buf();
        // A watchdog: firmware code that loops forever on our pretend hardware.
        let done = Arc::new(std::sync::atomic::AtomicBool::new(false));
        let flag = done.clone();
        let name = d.clone();
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_secs(50));
            if !flag.load(Ordering::SeqCst) {
                println!("  HANG: {name} still running after 50 s");
                std::process::exit(3);
            }
        });
        let r = std::panic::catch_unwind(|| run(&dir));
        done.store(true, Ordering::SeqCst);
        match r {
            Ok(Ok(s)) => {
                ok += 1;
                println!("  ok: {s}");
            }
            Ok(Err(e)) => {
                bad += 1;
                println!("  ERROR: {e}");
            }
            Err(_) => {
                bad += 1;
                println!("  PANIC (see above)");
            }
        }
    }
    println!("{ok} ok, {bad} failed");
}

//! The processor: what it is (CPUID), how fast it runs (APERF/MPERF), and on AMD Ryzen and
//! EPYC (family 17h and newer) its temperatures and power, read the way Linux's k10temp and
//! RAPL drivers read them:
//! - temperatures from the System Management Network (SMN), through the root complex's
//!   index/data pair in PCI configuration space (00:00.0, registers 0x60 and 0x64):
//!   Tctl at SMN 0x59800, each chiplet (CCD) at 0x59800 + an offset per generation;
//! - power from the package energy counter (MSR 0xC001029B, units in MSR 0xC0010299);
//! - the clock from APERF/MPERF (MSRs 0xE8/0xE7: busy cycles at the real clock, and at the
//!   base clock) and the P-state table (MSRs 0xC0010064 and up).
//! Every model-specific register is read with `rdmsr_safe`, so a processor (or emulator)
//! without one shows nothing instead of crashing.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::arch::x86_64::{__cpuid, __cpuid_count, _rdtsc};
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};
use spin::{Mutex, Once};

use crate::interrupts::rdmsr_safe;

/// What the processor is.
pub struct Info {
    pub vendor: String,
    /// As the processor names itself, e.g. "AMD Ryzen 7 5800X 8-Core Processor".
    pub brand: String,
    pub family: u32,
    pub model: u32,
    pub stepping: u32,
    pub cores: u32,
    pub threads: u32,
    /// The time-stamp counter's rate (MHz), measured against the timer.
    pub tsc_mhz: u32,
    /// The P-states' clocks in MHz, fastest first (AMD).
    pub pstates: Vec<u32>,
    /// AMD family 17h or newer (Zen): SMN temperatures.
    pub zen: bool,
    /// Where each chiplet's temperature is (SMN), and which chiplets report one.
    ccd_offset: u32,
    pub ccds: Vec<u32>,
    /// APERF/MPERF are there.
    aperf: bool,
    /// The package energy counter's unit: 1 / 2^esu joules (None: no counter).
    esu: Option<u32>,
    /// Older Ryzen X models report Tctl this many milli-degrees above the real temperature.
    tctl_offset: i32,
}

static INFO: Once<Info> = Once::new();

/// What the processor is (worked out once).
pub fn info() -> &'static Info {
    INFO.call_once(probe)
}

/// Time-stamp counts per microsecond, measured once over 10 timer ticks (100 ms).
pub fn tsc_per_us() -> u64 {
    static PER_US: AtomicU64 = AtomicU64::new(0);
    let mut per_us = PER_US.load(Relaxed);
    if per_us == 0 {
        let ticks = &crate::interrupts::TICKS;
        let t0 = ticks.load(Relaxed);
        while ticks.load(Relaxed) == t0 {
            core::hint::spin_loop();
        }
        let (start, c0) = (ticks.load(Relaxed), unsafe { _rdtsc() });
        while ticks.load(Relaxed) < start + 10 {
            core::hint::spin_loop();
        }
        per_us = ((unsafe { _rdtsc() } - c0) / 100_000).max(1);
        PER_US.store(per_us, Relaxed);
    }
    per_us
}

/// Vector instructions this processor has and Solon switched on (see `enable_simd`).
static SIMD: AtomicU64 = AtomicU64::new(0);
const HAS_SSE41: u64 = 1;
const HAS_AVX2: u64 = 2;

/// Switch on the processor's floating-point and vector units on this core (every core calls
/// this first thing): SSE (CR0, CR4) always, and AVX/AVX2 where the processor has them (the
/// XSAVE state they need, in CR4 and XCR0). Solon has no processes to switch between, so no
/// register state needs saving; interrupt handlers save the vector registers they use.
pub fn enable_simd() {
    unsafe {
        let mut cr0: u64;
        core::arch::asm!("mov {}, cr0", out(reg) cr0, options(nomem, nostack));
        cr0 = (cr0 & !(1 << 2 | 1 << 3)) | 1 << 1; // EM off, TS off, MP on
        core::arch::asm!("mov cr0, {}", in(reg) cr0, options(nomem, nostack));
        let mut cr4: u64;
        core::arch::asm!("mov {}, cr4", out(reg) cr4, options(nomem, nostack));
        cr4 |= 1 << 9 | 1 << 10; // OSFXSR, OSXMMEXCPT
        let (_, _, ecx1, _) = cpuid(1);
        let xsave = ecx1 & (1 << 26) != 0;
        let avx = ecx1 & (1 << 28) != 0;
        if xsave {
            cr4 |= 1 << 18; // OSXSAVE
        }
        core::arch::asm!("mov cr4, {}", in(reg) cr4, options(nomem, nostack));
        let mut have = 0;
        if ecx1 & (1 << 19) != 0 {
            have |= HAS_SSE41;
        }
        if xsave && avx {
            // XCR0: x87, SSE and AVX state.
            let (lo, hi): (u32, u32);
            core::arch::asm!("xgetbv", in("ecx") 0, out("eax") lo, out("edx") hi, options(nomem, nostack));
            let xcr0 = (hi as u64) << 32 | lo as u64 | 0b111;
            core::arch::asm!("xsetbv", in("ecx") 0, in("eax") xcr0 as u32, in("edx") (xcr0 >> 32) as u32, options(nomem, nostack));
            let (_, ebx7, _, _) = cpuid_sub(7, 0);
            if cpuid(0).0 >= 7 && ebx7 & (1 << 5) != 0 {
                have |= HAS_AVX2;
            }
        }
        SIMD.store(have, Relaxed);
    }
}

/// The processor has AVX2 (and Solon switched it on).
pub fn avx2() -> bool {
    SIMD.load(Relaxed) & HAS_AVX2 != 0
}

/// The processor has SSE4.1.
pub fn sse41() -> bool {
    SIMD.load(Relaxed) & HAS_SSE41 != 0
}

/// What the floating-point and vector unit has, for the log and Settings > This PC.
pub fn simd_text() -> String {
    let mut v = String::from("On: SSE2");
    if sse41() {
        v.push_str(", SSE4.1");
    }
    if avx2() {
        v.push_str(", AVX2");
    }
    v
}

/// Microseconds since start (from the time-stamp counter), for timing work.
pub fn now_us() -> u64 {
    (unsafe { _rdtsc() }) / tsc_per_us()
}

fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    let r = __cpuid(leaf);
    (r.eax, r.ebx, r.ecx, r.edx)
}

fn cpuid_sub(leaf: u32, sub: u32) -> (u32, u32, u32, u32) {
    let r = __cpuid_count(leaf, sub);
    (r.eax, r.ebx, r.ecx, r.edx)
}

/// An SMN register (AMD Zen): through the root complex's index/data pair.
fn smn(addr: u32) -> u32 {
    crate::pci::write32(0, 0, 0, 0x60, addr);
    crate::pci::read32(0, 0, 0, 0x64)
}

fn probe() -> Info {
    let (max, b, c, d) = cpuid(0);
    let mut vendor = String::new();
    for r in [b, d, c] {
        for k in 0..4 {
            vendor.push(((r >> (k * 8)) & 0xff) as u8 as char);
        }
    }
    let (sig, ebx1, _, _) = cpuid(1);
    let base_family = (sig >> 8) & 0xf;
    let family = if base_family == 0xf { base_family + ((sig >> 20) & 0xff) } else { base_family };
    let model = if base_family >= 0x6 { ((sig >> 4) & 0xf) | ((sig >> 12) & 0xf0) } else { (sig >> 4) & 0xf };
    let stepping = sig & 0xf;
    let max_ext = cpuid(0x8000_0000).0;
    let mut brand = String::new();
    if max_ext >= 0x8000_0004 {
        for leaf in 0x8000_0002..=0x8000_0004 {
            let (a, b, c, d) = cpuid(leaf);
            for r in [a, b, c, d] {
                for k in 0..4 {
                    let ch = ((r >> (k * 8)) & 0xff) as u8;
                    if ch != 0 {
                        brand.push(ch as char);
                    }
                }
            }
        }
    }
    let brand = String::from(brand.trim());
    let amd = vendor == "AuthenticAMD" || vendor == "HygonGenuine";
    // Threads and cores.
    let mut threads = ((ebx1 >> 16) & 0xff).max(1);
    let mut per_core = 1;
    if amd && max_ext >= 0x8000_0008 {
        threads = (cpuid(0x8000_0008).2 & 0xff) + 1;
        if max_ext >= 0x8000_001e {
            per_core = ((cpuid(0x8000_001e).1 >> 8) & 0xff) + 1;
        }
    } else if !amd && max >= 0xb {
        let smt = cpuid_sub(0xb, 0).1 & 0xffff;
        let pkg = cpuid_sub(0xb, 1).1 & 0xffff;
        if smt > 0 && pkg > 0 {
            threads = pkg;
            per_core = smt;
        }
    }
    let cores = (threads / per_core.max(1)).max(1);
    let tsc_mhz = (tsc_per_us() as u32).max(1);
    // P-states (AMD family 17h+: clock = FID / DID x 200 MHz).
    let zen = amd && family >= 0x17;
    let mut pstates = Vec::new();
    if zen {
        for n in 0..8 {
            if let Some(v) = rdmsr_safe(0xc001_0064 + n) {
                let (fid, did) = ((v & 0xff) as u32, ((v >> 8) & 0x3f) as u32);
                if v >> 63 != 0 && did != 0 {
                    let mhz = fid * 200 / did;
                    if mhz > 0 && !pstates.contains(&mhz) {
                        pstates.push(mhz);
                    }
                }
            }
        }
        pstates.sort_unstable_by(|a, b| b.cmp(a));
    }
    let aperf = max >= 6 && cpuid(6).2 & 1 != 0 && rdmsr_safe(0xe8).is_some() && rdmsr_safe(0xe7).is_some();
    let esu = if zen { rdmsr_safe(0xc001_0299).filter(|_| rdmsr_safe(0xc001_029b).is_some()).map(|v| ((v >> 8) & 0x1f) as u32) } else { None };
    // Temperatures: the root complex must be AMD's.
    let rc_amd = crate::pci::read32(0, 0, 0, 0) & 0xffff == 0x1022;
    // Where the chiplet temperatures are, and how many chiplets there can be (k10temp:
    // registers past the last chiplet hold other things, some with the "valid" bit set).
    let (ccd_offset, ccd_max) = match (family, model) {
        (0x17, 0x01 | 0x08 | 0x11 | 0x18) => (0x154, 4),
        (0x17, 0x31 | 0x47 | 0x60 | 0x68 | 0x71) => (0x154, 8),
        (0x17, 0xa0..=0xaf) => (0x300, 8),
        (0x19, 0x00..=0x01 | 0x08 | 0x21 | 0x50..=0x5f) => (0x154, 8),
        (0x19, 0x40..=0x4f) => (0x300, 8),
        (0x19, 0x10..=0x1f | 0xa0..=0xaf) => (0x300, 12),
        (0x19, 0x60..=0x7f) => (0x308, 8),
        (0x1a, 0x00..=0x1f) => (0x1f0, 16),
        (0x1a, 0x40..=0x4f) => (0x308, 8),
        _ => (0, 0),
    };
    let zen_temps = zen && rc_amd;
    let mut ccds = Vec::new();
    if zen_temps && ccd_offset != 0 {
        for i in 0..ccd_max {
            let v = smn(0x59800 + ccd_offset + i * 4);
            // Valid bit, not an all-ones error read, and a temperature a chiplet could have.
            let t = (v & 0x7ff) as i32 * 125 - 49000;
            if v != 0xffff_ffff && v & (1 << 11) != 0 && (0..=125_000).contains(&t) {
                ccds.push(i);
            }
        }
    }
    let tctl_offset = [("Ryzen 5 1600X", 20000), ("Ryzen 7 1700X", 20000), ("Ryzen 7 1800X", 20000), ("Ryzen 7 2700X", 10000), ("Threadripper 19", 27000), ("Threadripper 29", 27000)]
        .iter()
        .find(|(n, _)| family == 0x17 && brand.contains(n))
        .map(|&(_, o)| o)
        .unwrap_or(0);
    Info { vendor, brand, family, model, stepping, cores, threads, tsc_mhz, pstates, zen: zen_temps, ccd_offset, ccds, aperf, esu, tctl_offset }
}

/// Readings at one moment.
#[derive(Clone, Default)]
pub struct Sample {
    /// The average clock while busy since the last sample (MHz).
    pub mhz: Option<u32>,
    /// The control temperature (Tctl, adjusted to the real one) and each chiplet's, in
    /// milli-degrees Celsius.
    pub temp: Option<i32>,
    pub ccd: Vec<(u32, i32)>,
    /// Package power since the last sample (milliwatts).
    pub mw: Option<u32>,
}

struct Last {
    tsc: u64,
    aperf: u64,
    mperf: u64,
    energy: u64,
}

static LAST: Mutex<Option<Last>> = Mutex::new(None);

/// Read the sensors. Clock and power are averages since the previous call (the first call
/// only starts the counters).
pub fn sample() -> Sample {
    let i = info();
    let mut s = Sample::default();
    if i.zen {
        let v = smn(0x59800);
        let mut t = ((v >> 21) & 0x7ff) as i32 * 125;
        if v & (1 << 19) != 0 || (v >> 16) & 3 == 3 {
            t -= 49000;
        }
        s.temp = Some(t - i.tctl_offset);
        for &c in &i.ccds {
            let v = smn(0x59800 + i.ccd_offset + c * 4);
            s.ccd.push((c, (v & 0x7ff) as i32 * 125 - 49000));
        }
    }
    let tsc = unsafe { _rdtsc() };
    let (aperf, mperf) = if i.aperf { (rdmsr_safe(0xe8).unwrap_or(0), rdmsr_safe(0xe7).unwrap_or(0)) } else { (0, 0) };
    let energy = if i.esu.is_some() { rdmsr_safe(0xc001_029b).unwrap_or(0) & 0xffff_ffff } else { 0 };
    let mut last = LAST.lock();
    if let Some(l) = last.as_ref() {
        let (da, dm) = (aperf.wrapping_sub(l.aperf), mperf.wrapping_sub(l.mperf));
        if i.aperf && dm > 0 {
            s.mhz = Some((i.tsc_mhz as u64 * da / dm) as u32);
        }
        let dt = tsc.wrapping_sub(l.tsc);
        if let (Some(esu), true) = (i.esu, dt > 0) {
            let de = energy.wrapping_sub(l.energy) & 0xffff_ffff;
            // joules = de / 2^esu; seconds = dt / (tsc_mhz * 1e6); milliwatts = joules / s * 1000
            let us = dt / (i.tsc_mhz as u64).max(1);
            if us > 0 {
                s.mw = Some(((de as u128 * 1_000_000_000 / us as u128) >> esu) as u32);
            }
        }
    }
    *last = Some(Last { tsc, aperf, mperf, energy });
    s
}

/// The latest readings, sampled at most once a second (screens that redraw often get the
/// same numbers, and averages over a useful stretch).
pub fn latest() -> Sample {
    static CACHE: Mutex<Option<(u64, Sample)>> = Mutex::new(None);
    let now = crate::interrupts::TICKS.load(Relaxed);
    let mut c = CACHE.lock();
    match c.as_ref() {
        Some((at, s)) if now < at + crate::interrupts::HZ => s.clone(),
        _ => {
            let s = sample();
            *c = Some((now, s.clone()));
            s
        }
    }
}

/// A sample in words, for the log.
pub fn describe_sample(s: &Sample) -> String {
    let mut parts = Vec::new();
    if let Some(m) = s.mhz {
        parts.push(format!("{} MHz", m));
    }
    if let Some(t) = s.temp {
        parts.push(format!("Tctl {}", celsius(t)));
    }
    for &(c, t) in &s.ccd {
        parts.push(format!("chiplet {} {}", c + 1, celsius(t)));
    }
    if let Some(mw) = s.mw {
        parts.push(format!("package {}.{} W", mw / 1000, mw % 1000 / 100));
    }
    if parts.is_empty() {
        String::from("no sensors this Solon can read")
    } else {
        parts.join(", ")
    }
}

/// Degrees with one decimal, from milli-degrees.
pub fn celsius(mc: i32) -> String {
    let t = (mc + if mc >= 0 { 50 } else { -50 }) / 100;
    format!("{}.{} \u{b0}C", t / 10, (t % 10).abs())
}

/// One line for the log.
pub fn describe() -> String {
    let i = info();
    format!(
        "{} ({}, family {:#x} model {:#x} stepping {}), {} cores, {} threads, time-stamp counter {} MHz{}",
        if i.brand.is_empty() { "unnamed processor" } else { &i.brand },
        i.vendor, i.family, i.model, i.stepping, i.cores, i.threads, i.tsc_mhz,
        if i.pstates.is_empty() { String::new() } else { format!(", P-states {:?} MHz", i.pstates) }
    )
}

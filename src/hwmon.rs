//! The motherboard's sensor chip: Nuvoton NCT6798D / NCT6799D "Super I/O" hardware monitor
//! (on ASUS ROG STRIX B550 boards and many others): fan speeds, temperatures and voltages.
//! Register map from Linux's nct6775 driver (nct6775-core.c, nct6775-platform.c).
//!
//! Found through the Super I/O configuration ports (0x2e or 0x4e: the 0x87 0x87 key, chip
//! ID in registers 0x20/0x21, the hardware monitor's I/O base in logical device 0x0b,
//! registers 0x60/0x61), once at start. After that only the monitor's own index/data
//! ports are used (base + 5 and base + 6; register 0x4e picks the bank), and only read:
//! fan control is left to the board.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU16, Ordering::Relaxed};
use spin::Mutex;
use x86_64::instructions::port::Port;

/// The monitor's I/O base (0: none found), and the chip.
static BASE: AtomicU16 = AtomicU16::new(0);
static CHIP: Mutex<&'static str> = Mutex::new("");

fn outb(port: u16, v: u8) {
    unsafe { Port::<u8>::new(port).write(v) }
}

fn inb(port: u16) -> u8 {
    unsafe { Port::<u8>::new(port).read() }
}

/// Look for the chip (once, at start). Returns what was found, for the log.
pub fn init() -> String {
    for sio in [0x2e_u16, 0x4e] {
        outb(sio, 0x87);
        outb(sio, 0x87);
        let reg = |r: u8| {
            outb(sio, r);
            inb(sio + 1)
        };
        let id = (reg(0x20) as u16) << 8 | reg(0x21) as u16;
        let chip = match id & 0xfff8 {
            0xd428 => Some("Nuvoton NCT6798D"),
            0xd800 => Some("Nuvoton NCT6799D"),
            _ => None,
        };
        let known = match id & 0xfff8 {
            0xb470 | 0xc330 | 0xc560 | 0xc800 | 0xc910 | 0xd120 | 0xd350 | 0xd420 | 0xd450 => Some("an older Nuvoton chip"),
            _ => None,
        };
        let mut base = 0;
        if chip.is_some() {
            outb(sio, 0x07);
            outb(sio + 1, 0x0b); // logical device: hardware monitor
            base = ((reg(0x60) as u16) << 8 | reg(0x61) as u16) & !7;
            let enabled = reg(0x30) & 1 != 0;
            if !enabled {
                base = 0;
            }
        }
        outb(sio, 0xaa);
        if let Some(name) = chip {
            if base != 0 {
                BASE.store(base, Relaxed);
                *CHIP.lock() = name;
                return format!("{} (ID {:#06x}) at {:#x}, monitor at {:#x}", name, id, sio, base);
            }
            return format!("{} (ID {:#06x}) at {:#x}, monitor switched off", name, id, sio);
        }
        if let Some(k) = known {
            return format!("{} (ID {:#06x}) at {:#x}: not read yet", k, id, sio);
        }
    }
    String::from("no Nuvoton sensor chip")
}

/// Read a monitor register (bank in the high byte).
fn rd(base: u16, reg: u16) -> u8 {
    outb(base + 5, 0x4e);
    outb(base + 6, (reg >> 8) as u8);
    outb(base + 5, reg as u8);
    inb(base + 6)
}

fn rd16(base: u16, reg: u16) -> u16 {
    (rd(base, reg) as u16) << 8 | rd(base, reg + 1) as u16
}

/// What the chip reads now.
#[derive(Clone, Default)]
pub struct Readings {
    pub chip: &'static str,
    /// Name and milli-degrees Celsius.
    pub temps: Vec<(String, i32)>,
    /// Header number (from 1) and RPM.
    pub fans: Vec<(u32, u32)>,
    /// Name and millivolts.
    pub volts: Vec<(&'static str, u32)>,
}

/// What each temperature source is (nct6798_temp_label).
fn source_name(src: u8) -> &'static str {
    match src {
        1 => "motherboard (SYSTIN)",
        2 => "CPU socket (CPUTIN)",
        3 => "AUXTIN0",
        4 => "AUXTIN1",
        5 => "AUXTIN2",
        6 => "AUXTIN3",
        7 => "AUXTIN4",
        8 => "SMBus master 0",
        9 => "SMBus master 1",
        16 => "PECI agent 0",
        17 => "PECI agent 1",
        18..=21 => "chipset",
        22..=25 => "memory",
        _ => "",
    }
}

/// Read everything (None: no chip).
pub fn read() -> Option<Readings> {
    let base = BASE.load(Relaxed);
    if base == 0 {
        return None;
    }
    let mut r = Readings { chip: *CHIP.lock(), ..Readings::default() };
    // Temperatures: 8 channels, each with a source (NCT6798_REG_TEMP / _TEMP_SOURCE); the
    // value is a byte in degrees, or for 0x150 a 9-bit value in half degrees.
    let temp_regs = [0x027u16, 0x150, 0x670, 0x672, 0x674, 0x676, 0x678, 0x67a];
    let src_regs = [0x621u16, 0x622, 0xc26, 0xc27, 0xc28, 0xc29, 0xc2a, 0xc2b];
    let mut seen = Vec::new();
    for (&tr, &sr) in temp_regs.iter().zip(src_regs.iter()) {
        let src = rd(base, sr) & 0x1f;
        if src == 0 || seen.contains(&src) {
            continue;
        }
        seen.push(src);
        let raw = if tr == 0x150 { rd16(base, tr) } else { (rd(base, tr) as u16) << 8 };
        let mc = (raw as i16 as i32) / 128 * 500;
        let name = source_name(src);
        // Unconnected inputs read as nonsense; keep what a real sensor could say.
        if !name.is_empty() && (-40_000..=125_000).contains(&mc) {
            r.temps.push((String::from(name), mc));
        }
    }
    // The processor's own temperature, as the chip reads it over AMD SB-TSI (8.3 format).
    for (k, &tr) in [0x409u16, 0x40b].iter().enumerate() {
        let raw = rd16(base, tr);
        if raw != 0 && raw != 0xffff {
            let mc = (raw >> 5) as i32 * 125;
            if (0..=125_000).contains(&mc) {
                r.temps.push((format!("CPU (SB-TSI {})", k), mc));
            }
        }
    }
    // Fans: 16-bit RPM (NCT6779_REG_FAN).
    for (k, &fr) in [0x4c0u16, 0x4c2, 0x4c4, 0x4c6, 0x4c8, 0x4ca, 0x4ce].iter().enumerate() {
        let rpm = rd16(base, fr) as u32;
        if rpm != 0xffff {
            r.fans.push((k as u32 + 1, rpm));
        }
    }
    // Voltages: 8 mV steps, some halved inside the chip (scale_in_6798). Inputs whose
    // meaning depends on the board's resistors (+12 V, +5 V) are left out.
    for &(name, reg, scale) in &[("CPU core", 0x480u16, 8u32), ("3.3 V", 0x483, 16), ("3.3 V standby", 0x487, 16), ("analog 3.3 V", 0x482, 16), ("battery", 0x488, 16)] {
        let mv = rd(base, reg) as u32 * scale;
        if mv > 0 {
            r.volts.push((name, mv));
        }
    }
    // Leave the chip on bank 0, as the firmware expects to find it.
    outb(base + 5, 0x4e);
    outb(base + 6, 0);
    Some(r)
}

/// The latest readings, read at most once a second.
pub fn latest() -> Option<Readings> {
    static CACHE: Mutex<Option<(u64, Readings)>> = Mutex::new(None);
    if BASE.load(Relaxed) == 0 {
        return None;
    }
    let now = crate::interrupts::TICKS.load(Relaxed);
    let mut c = CACHE.lock();
    match c.as_ref() {
        Some((at, r)) if now < at + crate::interrupts::HZ => Some(r.clone()),
        _ => {
            let r = read()?;
            *c = Some((now, r.clone()));
            Some(r)
        }
    }
}

/// The readings in one line, for the log.
pub fn describe(r: &Readings) -> String {
    let temps: Vec<String> = r.temps.iter().map(|(n, t)| format!("{} {}", n, crate::cpu::celsius(*t))).collect();
    let fans: Vec<String> = r.fans.iter().map(|(n, rpm)| format!("{}: {}", n, rpm)).collect();
    let volts: Vec<String> = r.volts.iter().map(|(n, mv)| format!("{} {}.{:03} V", n, mv / 1000, mv % 1000)).collect();
    format!("{}; fans (RPM) {}; {}", temps.join(", "), fans.join(", "), volts.join(", "))
}

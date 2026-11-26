//! Laptop batteries and the charger, read the way every operating system reads them: by
//! running the firmware's own ACPI code (AML) for the battery devices (PNP0C0A) and the AC
//! adapter (ACPI0003). That code mostly asks the laptop's embedded controller (EC), the small
//! processor that looks after the battery, keyboard and fans.
//!
//! This file finds the devices and makes sense of their answers; `aml.rs` runs the code on the
//! machine. It uses nothing from the kernel but `alloc` and the acpi crate, so the host tool
//! `tools/acpitest` runs this same file against other laptops' ACPI tables.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::str::FromStr;

use acpi::aml::namespace::{AmlName, NamespaceLevelKind};
use acpi::aml::object::{Object, WrappedObject};
use acpi::aml::{AmlError, Interpreter};
use acpi::Handler;

/// The devices the firmware describes.
#[derive(Clone, Debug, Default)]
pub struct Devices {
    pub batteries: Vec<AmlName>,
    pub adapters: Vec<AmlName>,
    /// The embedded controller, and its data and command ports.
    pub ec: Option<(AmlName, u16, u16)>,
}

/// What a battery is (changes rarely: read again now and then).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Info {
    /// Capacities in mWh (otherwise mAh, and rates in mA).
    pub mwh: bool,
    pub design: Option<u32>,
    /// What it holds when full now (less than `design` as it ages).
    pub full: Option<u32>,
    /// Design voltage, mV.
    pub voltage: Option<u32>,
    pub cycles: Option<u32>,
    pub model: String,
    pub serial: String,
    pub chemistry: String,
    pub maker: String,
}

/// One battery bay and what's in it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Battery {
    /// Where it is in the firmware's namespace (\_SB.BAT0).
    pub name: String,
    pub present: bool,
    pub info: Option<Info>,
    pub charging: bool,
    pub discharging: bool,
    pub critical: bool,
    pub remaining: Option<u32>,
    pub rate: Option<u32>,
    /// mV
    pub voltage: Option<u32>,
}

impl Battery {
    fn full(&self) -> Option<u32> {
        let i = self.info.as_ref()?;
        i.full.filter(|&f| f > 0).or(i.design.filter(|&d| d > 0))
    }

    /// Charge, 0-100.
    pub fn percent(&self) -> Option<u8> {
        let (r, f) = (self.remaining?, self.full()?);
        Some(((r as u64 * 100 + f as u64 / 2) / f as u64).min(100) as u8)
    }

    /// Minutes until empty (discharging) or full (charging), if the battery says how fast.
    pub fn minutes(&self) -> Option<u32> {
        let rate = self.rate.filter(|&r| r > 0)?;
        let r = self.remaining?;
        let left = if self.discharging {
            r
        } else if self.charging {
            self.full()?.saturating_sub(r)
        } else {
            return None;
        };
        let m = left as u64 * 60 / rate as u64;
        (m < 100 * 60).then_some(m as u32)
    }

    /// What it holds now against what it held new, 0-100.
    pub fn health(&self) -> Option<u8> {
        let i = self.info.as_ref()?;
        let (f, d) = (i.full.filter(|&f| f > 0)?, i.design.filter(|&d| d > 0)?);
        Some((f as u64 * 100 / d as u64).min(100) as u8)
    }
}

/// Everything at one moment.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Power {
    pub batteries: Vec<Battery>,
    /// Plugged in (None if the firmware doesn't say).
    pub on_ac: Option<bool>,
}

impl Power {
    fn present(&self) -> impl Iterator<Item = &Battery> {
        self.batteries.iter().filter(|b| b.present)
    }

    pub fn has_battery(&self) -> bool {
        self.present().next().is_some()
    }

    /// Charge of all batteries together, 0-100.
    pub fn percent(&self) -> Option<u8> {
        let (mut r, mut f) = (0u64, 0u64);
        for b in self.present() {
            r += b.remaining? as u64;
            f += b.full()? as u64;
        }
        if f == 0 {
            return None;
        }
        Some(((r * 100 + f / 2) / f).min(100) as u8)
    }

    pub fn charging(&self) -> bool {
        self.present().any(|b| b.charging)
    }

    /// Running on the battery: unplugged, or a battery says it's emptying.
    pub fn on_battery(&self) -> bool {
        self.has_battery() && (self.on_ac == Some(false) || (self.on_ac.is_none() && self.present().any(|b| b.discharging)))
    }

    pub fn minutes(&self) -> Option<u32> {
        let mut total = 0;
        let mut any = false;
        for b in self.present().filter(|b| b.charging || b.discharging) {
            total += b.minutes()?;
            any = true;
        }
        any.then_some(total)
    }
}

fn path(name: &str, scope: &AmlName) -> Result<AmlName, AmlError> {
    AmlName::from_str(name)?.resolve(scope)
}

/// Evaluate `name` under device `dev` (a method or a plain value). None if it isn't there.
pub fn eval<H: Handler>(i: &Interpreter<H>, dev: &AmlName, name: &str, args: Vec<WrappedObject>) -> Result<Option<WrappedObject>, AmlError> {
    Ok(i.evaluate_if_present(path(name, dev)?, args)?.map(|o| o.unwrap_reference()))
}

fn int(o: &WrappedObject) -> Option<u64> {
    match &*o.clone().unwrap_reference() {
        Object::Integer(v) => Some(*v),
        Object::Buffer(b) if !b.is_empty() => Some(b.iter().take(8).rev().fold(0u64, |a, &x| a << 8 | x as u64)),
        Object::String(s) => {
            let s = s.trim();
            match s.strip_prefix("0x").or(s.strip_prefix("0X")) {
                Some(h) => u64::from_str_radix(h, 16).ok(),
                None => s.parse().ok(),
            }
        }
        _ => None,
    }
}

fn text(o: &WrappedObject) -> String {
    let s = match &*o.clone().unwrap_reference() {
        Object::String(s) => s.clone(),
        Object::Buffer(b) => b.iter().take_while(|&&c| c != 0).map(|&c| if c.is_ascii_graphic() || c == b' ' { c as char } else { ' ' }).collect(),
        Object::Integer(v) if *v != 0 => format!("{}", v),
        _ => String::new(),
    };
    String::from(s.trim_matches(|c: char| c == '\0' || c.is_whitespace()))
}

/// A 32-bit value from a battery package; all ones means "unknown".
fn value(p: &[WrappedObject], at: usize) -> Option<u32> {
    let v = int(p.get(at)?)? as u32;
    (v != 0xFFFF_FFFF).then_some(v)
}

/// A compressed EISA ID ("PNP0C0A" packed into 32 bits) back as text.
fn eisa(id: u64) -> String {
    let v = (id as u32).swap_bytes();
    let c = |shift: u32| (((v >> shift) & 0x1F) as u8 + 0x40) as char;
    format!("{}{}{}{:04X}", c(26), c(21), c(16), v & 0xFFFF)
}

fn hid<H: Handler>(i: &Interpreter<H>, dev: &AmlName) -> Option<String> {
    let o = eval(i, dev, "_HID", vec![]).ok()??;
    match &*o {
        Object::Integer(v) => Some(eisa(*v)),
        Object::String(s) => Some(s.clone()),
        _ => None,
    }
}

/// The embedded controller's ports from its resources: the first I/O port is for data, the
/// second for commands and status.
fn ec_ports<H: Handler>(i: &Interpreter<H>, dev: &AmlName) -> Option<(u16, u16)> {
    let o = eval(i, dev, "_CRS", vec![]).ok()??;
    let Object::Buffer(b) = &*o else { return None };
    let mut ports = Vec::new();
    let mut at = 0;
    while at < b.len() {
        let tag = b[at];
        if tag & 0x80 != 0 {
            // Large item: tag, 16-bit length.
            let len = *b.get(at + 1)? as usize | (*b.get(at + 2)? as usize) << 8;
            at += 3 + len;
            continue;
        }
        let len = (tag & 7) as usize;
        match tag >> 3 {
            0x08 if len >= 7 => ports.push(u16::from_le_bytes([*b.get(at + 2)?, *b.get(at + 3)?])), // I/O port
            0x09 if len >= 3 => ports.push(u16::from_le_bytes([*b.get(at + 1)?, *b.get(at + 2)?]) & 0x3FF), // fixed I/O
            0x0F => break, // end tag
            _ => {}
        }
        at += 1 + len;
    }
    match ports[..] {
        [data, cmd, ..] if data != 0 && cmd != 0 => Some((data, cmd)),
        _ => None,
    }
}

/// Find the batteries, AC adapters and embedded controller.
pub fn find<H: Handler>(i: &Interpreter<H>) -> Devices {
    let mut devices = Vec::new();
    let _ = i.namespace.lock().traverse(|path, level| {
        if level.kind == NamespaceLevelKind::Device {
            devices.push(path.clone());
        }
        Ok(true)
    });
    let mut d = Devices::default();
    for p in devices {
        match hid(i, &p).as_deref() {
            Some("PNP0C0A") => d.batteries.push(p),
            Some("ACPI0003") => d.adapters.push(p),
            Some("PNP0C09") if d.ec.is_none() => {
                let (data, cmd) = ec_ports(i, &p).unwrap_or((0x62, 0x66));
                d.ec = Some((p, data, cmd));
            }
            _ => {}
        }
    }
    d
}

/// Tell the firmware the embedded controller's address space is ready (`_REG`), as an
/// operating system does once it can reach the EC. Much firmware only uses the EC after this.
pub fn connect_ec<H: Handler>(i: &Interpreter<H>, d: &Devices) -> Result<(), AmlError> {
    if let Some((ec, ..)) = &d.ec {
        eval(i, ec, "_REG", vec![Object::Integer(3).wrap(), Object::Integer(1).wrap()])?;
    }
    Ok(())
}

/// `_STA`: bit 0 = the device exists, bit 4 = a battery is in it. Without one, both.
fn status<H: Handler>(i: &Interpreter<H>, dev: &AmlName) -> Result<u64, AmlError> {
    Ok(match eval(i, dev, "_STA", vec![])? {
        Some(o) => int(&o).unwrap_or(0x1F),
        None => 0x1F,
    })
}

fn package(o: Option<WrappedObject>) -> Option<Vec<WrappedObject>> {
    match &*o? {
        Object::Package(p) => Some(p.clone()),
        _ => None,
    }
}

/// What a battery is: `_BIX` (ACPI 4 and later) or else `_BIF`.
pub fn read_info<H: Handler>(i: &Interpreter<H>, dev: &AmlName) -> Result<Option<Info>, AmlError> {
    if let Some(p) = package(eval(i, dev, "_BIX", vec![])?).filter(|p| p.len() >= 20) {
        return Ok(Some(Info {
            mwh: value(&p, 1) == Some(0),
            design: value(&p, 2),
            full: value(&p, 3),
            voltage: value(&p, 5),
            cycles: value(&p, 8).filter(|&c| c > 0),
            model: text(&p[16]),
            serial: text(&p[17]),
            chemistry: text(&p[18]),
            maker: text(&p[19]),
        }));
    }
    if let Some(p) = package(eval(i, dev, "_BIF", vec![])?).filter(|p| p.len() >= 13) {
        return Ok(Some(Info {
            mwh: value(&p, 0) == Some(0),
            design: value(&p, 1),
            full: value(&p, 2),
            voltage: value(&p, 4),
            cycles: None,
            model: text(&p[9]),
            serial: text(&p[10]),
            chemistry: text(&p[11]),
            maker: text(&p[12]),
        }));
    }
    Ok(None)
}

/// A battery's state now (`_STA`, `_BST`). Keeps `info` as it was unless `refresh` asks for
/// a new one (or there's none yet).
pub fn read_battery<H: Handler>(i: &Interpreter<H>, dev: &AmlName, old: Option<&Battery>, refresh: bool) -> Result<Battery, AmlError> {
    let mut b = Battery { name: dev.as_string(), ..Battery::default() };
    let sta = status(i, dev)?;
    b.present = sta & 1 != 0 && sta & 0x10 != 0;
    if !b.present {
        return Ok(b);
    }
    b.info = match old.and_then(|o| o.info.clone()) {
        Some(info) if !refresh && old.is_some_and(|o| o.present) => Some(info),
        _ => read_info(i, dev)?,
    };
    if let Some(p) = package(eval(i, dev, "_BST", vec![])?).filter(|p| p.len() >= 4) {
        let state = value(&p, 0).unwrap_or(0);
        b.discharging = state & 1 != 0;
        b.charging = state & 2 != 0 && !b.discharging;
        b.critical = state & 4 != 0;
        // Some firmware reports the rate as a negative number while discharging.
        b.rate = value(&p, 1).map(|r| if r & 0x8000_0000 != 0 { r.wrapping_neg() } else { r }).filter(|&r| r < 0x8000_0000);
        b.remaining = value(&p, 2);
        b.voltage = value(&p, 3);
    }
    Ok(b)
}

/// Everything: each battery and whether the charger is plugged in.
pub fn read<H: Handler>(i: &Interpreter<H>, d: &Devices, old: Option<&Power>, refresh: bool) -> Result<Power, AmlError> {
    let mut p = Power::default();
    for dev in &d.batteries {
        let name = dev.as_string();
        let prev = old.and_then(|o| o.batteries.iter().find(|b| b.name == name));
        p.batteries.push(read_battery(i, dev, prev, refresh)?);
    }
    for dev in &d.adapters {
        if status(i, dev)? & 1 == 0 {
            continue;
        }
        if let Some(o) = eval(i, dev, "_PSR", vec![])? {
            let on = int(&o).unwrap_or(0) != 0;
            p.on_ac = Some(p.on_ac.unwrap_or(false) || on);
        }
    }
    Ok(p)
}

/// Whether ACPI tables mention a battery at all (the ID "PNP0C0A" as AML stores it, packed or
/// as text), to leave desktops alone.
pub fn mentions_battery(aml: &[u8]) -> bool {
    aml.windows(4).any(|w| w == [0x41, 0xD0, 0x0C, 0x0A]) || aml.windows(7).any(|w| w == b"PNP0C0A")
}

/// "2 h 05 min", "45 min".
pub fn duration_text(minutes: u32) -> String {
    if minutes >= 60 {
        format!("{} h {:02} min", minutes / 60, minutes % 60)
    } else {
        format!("{} min", minutes.max(1))
    }
}

/// How things stand, for people: "54%, about 2 h 10 min left", "87%, charging (1 h 20 min until
/// full)", "100%, plugged in".
pub fn status_text(p: &Power) -> String {
    let Some(pc) = p.percent() else { return String::from("Charge unknown") };
    if p.charging() {
        match p.minutes() {
            Some(m) => format!("{}%, charging ({} until full)", pc, duration_text(m)),
            None => format!("{}%, charging", pc),
        }
    } else if p.on_battery() {
        match p.minutes() {
            Some(m) => format!("{}%, about {} left", pc, duration_text(m)),
            None => format!("{}%, on battery", pc),
        }
    } else {
        format!("{}%, plugged in", pc)
    }
}

/// One line for the log.
pub fn describe(p: &Power) -> String {
    let mut s = String::new();
    for b in &p.batteries {
        if !s.is_empty() {
            s.push_str("; ");
        }
        if !b.present {
            s.push_str(&format!("{}: empty bay", b.name));
            continue;
        }
        let unit = if b.info.as_ref().is_some_and(|i| i.mwh) { "mWh" } else { "mAh" };
        s.push_str(&format!(
            "{}: {}% ({:?} of {:?} {}, design {:?}), {}, rate {:?}, {:?} mV",
            b.name,
            b.percent().map_or(String::from("?"), |v| format!("{}", v)),
            b.remaining,
            b.full(),
            unit,
            b.info.as_ref().and_then(|i| i.design),
            if b.charging { "charging" } else if b.discharging { "discharging" } else { "idle" },
            b.rate,
            b.voltage
        ));
        if let Some(i) = &b.info {
            s.push_str(&format!(", \"{}\" {} {}", i.model, i.chemistry, i.maker));
        }
    }
    match p.on_ac {
        Some(true) => s.push_str("; plugged in"),
        Some(false) => s.push_str("; on battery"),
        None => {}
    }
    s
}

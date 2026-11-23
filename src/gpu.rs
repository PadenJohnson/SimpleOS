//! The PC's graphics cards: which there are (PCI display controllers), their vendor, model
//! family and year from the device number, and which one shows the screen. This is the start
//! of native graphics support (see docs/graphics.md); for now every card is used through the
//! picture the firmware set up, and this tells the user (and later drivers) what's there.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Vendor {
    Intel,
    Amd,
    Nvidia,
    /// QEMU, VirtualBox, VMware...: a virtual machine's card.
    Virtual,
    Other,
}

#[derive(Clone, Debug)]
#[allow(dead_code)] // the PCI details are for the native drivers to come
pub struct Gpu {
    pub vendor: Vendor,
    pub vendor_id: u16,
    pub device_id: u16,
    /// "Intel UHD Graphics (Comet Lake, 2019)", "NVIDIA GeForce (Ampere, 2020)"...
    pub name: String,
    /// PCI location (bus, device, function).
    pub at: (u8, u8, u8),
    /// Its memory windows (physical address, size).
    pub bars: Vec<(u64, u64)>,
    /// The screen picture is in this card's memory.
    pub shows_screen: bool,
}

pub static GPUS: Mutex<Vec<Gpu>> = Mutex::new(Vec::new());

/// Intel built-in graphics: family and year from the device number (the families in use
/// on PCs today; older ones are named by number).
fn intel(dev: u16) -> String {
    let hi = dev >> 8;
    let (name, family, year) = match dev {
        0x0102..=0x016A => ("HD Graphics", "Sandy/Ivy Bridge", 2011),
        0x0402..=0x0D2E => ("HD Graphics", "Haswell", 2013),
        0x1602..=0x163E => ("HD Graphics", "Broadwell", 2014),
        0x1902..=0x193D => ("HD Graphics", "Skylake", 2015),
        0x5902..=0x593B => ("HD/UHD Graphics", "Kaby Lake", 2016),
        0x3E90..=0x3EA9 | 0x87C0 | 0x87CA => ("UHD Graphics", "Coffee Lake", 2017),
        0x9B21..=0x9BF6 => ("UHD Graphics", "Comet Lake", 2019),
        0x8A50..=0x8A71 => ("Iris Plus / UHD Graphics", "Ice Lake", 2019),
        0x9A40..=0x9AF8 => ("Iris Xe / UHD Graphics", "Tiger Lake", 2020),
        0x4C8A..=0x4C9A => ("UHD Graphics", "Rocket Lake", 2021),
        0x4626..=0x46D4 => ("Iris Xe / UHD Graphics", "Alder Lake", 2021),
        0xA780..=0xA7AD | 0xA720..=0xA721 => ("Iris Xe / UHD Graphics", "Raptor Lake", 2022),
        0x7D41 | 0x7D51 | 0x7D67 => ("Arc Graphics", "Arrow Lake", 2024),
        0x7D40..=0x7DDD => ("Arc Graphics", "Meteor Lake", 2023),
        0x6420 | 0x64A0 | 0x64B0 => ("Arc Graphics", "Lunar Lake", 2024),
        0x5690..=0x56C2 => ("Arc (A-series)", "Alchemist", 2022),
        0xE202..=0xE212 => ("Arc (B-series)", "Battlemage", 2024),
        _ => return format!("Intel graphics (device {:04X}, family {:02X})", dev, hi),
    };
    format!("Intel {} ({}, {})", name, family, year)
}

/// AMD Radeon: rough family and year from the device number.
fn amd(dev: u16) -> String {
    let (family, year) = match dev {
        0x6860..=0x687F => ("Vega", 2017),
        0x6600..=0x666F => ("GCN 1-2", 2012),
        0x6700..=0x67FF => ("GCN 1-4 (R9/RX 400-500)", 2012),
        0x6800..=0x699F => ("GCN (HD 7000 / RX 500)", 2012),
        0x15D8 | 0x15DD | 0x1636 | 0x1638 | 0x164C | 0x1681 => ("Vega (Ryzen built-in)", 2018),
        0x66A0..=0x66AF => ("Vega 20", 2018),
        0x7310..=0x731F | 0x7340..=0x734F | 0x7360..=0x736F => ("RDNA 1 (RX 5000)", 2019),
        0x73A0..=0x73FF => ("RDNA 2 (RX 6000)", 2020),
        0x163F | 0x164D | 0x1506 | 0x15BF | 0x15C8 => ("RDNA 2-3 (Ryzen built-in)", 2021),
        0x7440..=0x748F => ("RDNA 3 (RX 7000)", 2022),
        0x150E | 0x1586 => ("RDNA 3.5 (Ryzen built-in)", 2024),
        0x7550..=0x759F => ("RDNA 4 (RX 9000)", 2025),
        0x9400..=0x9FFF => ("TeraScale (HD 2000-6000)", 2007),
        _ => return format!("AMD Radeon (device {:04X})", dev),
    };
    format!("AMD Radeon ({}, {})", family, year)
}

/// NVIDIA GeForce/Quadro: architecture and year from the device number.
fn nvidia(dev: u16) -> String {
    let (family, year) = match dev >> 8 {
        0x06 | 0x08 | 0x0A | 0x0C => ("Tesla", 2008),
        0x0D | 0x0E | 0x10 if dev < 0x0FC0 => ("Fermi", 2010),
        0x0F..=0x12 => ("Kepler", 2012),
        0x13 | 0x14 | 0x17 => ("Maxwell", 2014),
        0x15 | 0x1B..=0x1D => ("Pascal", 2016),
        0x1E | 0x1F | 0x21 => ("Turing", 2018),
        0x20 | 0x22..=0x25 => ("Ampere", 2020),
        0x26..=0x28 => ("Ada Lovelace", 2022),
        0x29..=0x2F => ("Blackwell", 2025),
        _ => return format!("NVIDIA graphics (device {:04X})", dev),
    };
    format!("NVIDIA GeForce ({}, {})", family, year)
}

/// Find the graphics cards; `fb_phys` is where the firmware's screen picture is.
pub fn init(fb_phys: u64) {
    let mut found = Vec::new();
    for d in crate::pci::scan().into_iter().filter(|d| d.class == 3) {
        let (vendor, name) = match d.vendor {
            0x8086 => (Vendor::Intel, intel(d.device)),
            0x1002 => (Vendor::Amd, amd(d.device)),
            0x10DE => (Vendor::Nvidia, nvidia(d.device)),
            0x1234 => (Vendor::Virtual, String::from("QEMU / Bochs standard graphics")),
            0x1AF4 => (Vendor::Virtual, String::from("Virtio graphics (virtual machine)")),
            0x80EE => (Vendor::Virtual, String::from("VirtualBox graphics")),
            0x15AD => (Vendor::Virtual, String::from("VMware SVGA (virtual machine)")),
            0x1B36 => (Vendor::Virtual, String::from("QXL (virtual machine)")),
            v => (Vendor::Other, format!("Graphics {:04X}:{:04X}", v, d.device)),
        };
        // Memory windows (64-bit ones take two slots).
        let mut bars = Vec::new();
        let mut n = 0;
        while n < 6 {
            let raw = d.read(0x10 + n * 4);
            if raw & 1 == 0 && raw != 0 {
                if let Some((base, size)) = d.bar(n) {
                    if base != 0 && size != 0 {
                        bars.push((base, size));
                    }
                }
                n += if (raw >> 1) & 3 == 2 { 2 } else { 1 };
            } else {
                n += 1;
            }
        }
        let shows_screen = bars.iter().any(|&(b, s)| fb_phys >= b && fb_phys < b + s);
        kprintln!("gpu: {} [{:04x}:{:04x}] at {:02x}:{:02x}.{}{}", name, d.vendor, d.device, d.bus, d.dev, d.func, if shows_screen { ", shows the screen" } else { "" });
        found.push(Gpu { vendor, vendor_id: d.vendor, device_id: d.device, name, at: (d.bus, d.dev, d.func), bars, shows_screen });
    }
    if found.is_empty() {
        kprintln!("gpu: no graphics card found on PCI");
    }
    *GPUS.lock() = found;
}

/// The card showing the screen (or the first one), for Settings.
pub fn main_name() -> Option<String> {
    let g = GPUS.lock();
    g.iter().find(|x| x.shows_screen).or(g.first()).map(|x| x.name.clone())
}

/// How Solon uses it today, for Settings.
pub fn support() -> &'static str {
    let g = GPUS.lock();
    match g.iter().find(|x| x.shows_screen).or(g.first()).map(|x| x.vendor) {
        Some(Vendor::Virtual) if crate::display::vbe_present() => "Solon's own driver: changes resolution directly",
        Some(Vendor::Nvidia) if crate::nvidia::REPORT.lock().is_some() => "Solon's NVIDIA driver, stage 1: reads the card (no changes yet)",
        Some(_) => "Through the firmware's screen (a native driver is planned)",
        None => "Through the firmware's screen",
    }
}

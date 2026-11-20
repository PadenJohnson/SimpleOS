//! USB devices on top of xHCI: enumeration, hot-plug, hubs, HID boot keyboards/mice,
//! and mass-storage drives with their mounted volumes (read; written only by Copy to USB and Erase).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use crate::fs::{self, BlockDev, DirEntry, FsError, Volume};
use crate::interrupts::{sleep_ms, HZ, TICKS};
use crate::memory::Dma;
use crate::msc::MassStorage;
use crate::usbprint::UsbPrinter;
use crate::phone::Phone;
use crate::pci;
use crate::ps2;
use crate::xhci::{dma_buf, EpConfig, EpType, Speed, UsbError, Xhci, CC_SHORT, CC_SUCCESS};

pub struct Drive {
    pub id: u32,
    /// This is the stick Solon booted from (the only USB drive Solon may write to).
    pub is_boot: bool,
    ctrl: usize,
    slot: u8,
    pub name: String,
    pub ms: MassStorage,
    pub volumes: Vec<Volume>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HidKind {
    Keyboard,
    Mouse,
}

struct Hid {
    ctrl: usize,
    slot: u8,
    dci: u8,
    len: usize,
    kind: HidKind,
    buf: Dma,
    prev: [u8; 8],
}

/// Where a device sits in the USB tree.
#[derive(Clone, Copy)]
struct Place {
    root_port: u8,
    route: u32,
    /// Number of hubs between the device and the computer.
    depth: u8,
    /// Transaction translator for low/full-speed devices behind a high-speed hub.
    tt: Option<(u8, u8, bool)>,
    /// (hub slot, hub port) or None if plugged straight into the computer.
    parent: Option<(u8, u8)>,
}

struct Dev {
    ctrl: usize,
    slot: u8,
    place: Place,
}

struct Hub {
    ctrl: usize,
    slot: u8,
    ports: u8,
    speed: Speed,
    place: Place,
}

pub struct Usb {
    hcs: Vec<Xhci>,
    devs: Vec<Dev>,
    hubs: Vec<Hub>,
    hids: Vec<Hid>,
    pub drives: Vec<Drive>,
    pub printers: Vec<UsbPrinter>,
    /// Phones and cameras (MTP / PTP). Read only.
    pub phones: Vec<Phone>,
    next_id: u32,
    next_hub_poll: u64,
    boot_disk: Option<[u8; 16]>,
}

/// A drive as a block device for the file system code.
struct Disk<'a> {
    hc: &'a mut Xhci,
    ms: &'a mut MassStorage,
}

impl BlockDev for Disk<'_> {
    fn block_size(&self) -> usize {
        self.ms.block_size as usize
    }
    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
        self.ms.read(self.hc, lba, buf).map_err(|e| {
            kprintln!("usb: read error at block {}: {:?}", lba, e);
            FsError::Io
        })
    }
}

/// Solon's own boot stick as a *writable* block device. This is the only way to write to
/// any USB drive, and it can only be obtained for the drive Solon booted from.
pub struct BootDisk<'a> {
    hc: &'a mut Xhci,
    ms: &'a mut MassStorage,
}

impl BlockDev for BootDisk<'_> {
    fn block_size(&self) -> usize {
        self.ms.block_size as usize
    }
    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
        self.ms.read(self.hc, lba, buf).map_err(|_| FsError::Io)
    }
    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
        if lba + (buf.len() / self.ms.block_size as usize) as u64 > self.ms.blocks {
            return Err(FsError::Io);
        }
        self.ms.write(self.hc, lba, buf).map_err(|e| {
            kprintln!("usb: boot stick write error at block {}: {:?}", lba, e);
            FsError::Io
        })
    }
    fn flush(&mut self) -> Result<(), FsError> {
        self.ms.flush(self.hc).map_err(|_| FsError::Io)
    }
}

struct Iface {
    num: u8,
    alt: u8,
    class: u8,
    sub: u8,
    proto: u8,
    /// Interface name string index.
    istr: u8,
    /// (address, attributes, max packet, interval, max burst)
    eps: Vec<(u8, u8, u16, u8, u8)>,
}

fn parse_config(d: &[u8]) -> Vec<Iface> {
    let mut out: Vec<Iface> = Vec::new();
    let mut i = 0;
    let mut skip_alt = false;
    while i + 2 <= d.len() {
        let len = d[i] as usize;
        if len < 2 || i + len > d.len() {
            break;
        }
        let b = &d[i..i + len];
        match b[1] {
            4 if len >= 9 => {
                // Only alternate setting 0, except printers (IPP over USB often lives in an alternate).
                skip_alt = b[3] != 0 && b[5] != 7;
                if !skip_alt {
                    out.push(Iface { num: b[2], alt: b[3], class: b[5], sub: b[6], proto: b[7], istr: b[8], eps: Vec::new() });
                }
            }
            5 if len >= 7 && !skip_alt => {
                if let Some(f) = out.last_mut() {
                    f.eps.push((b[2], b[3], u16::from_le_bytes([b[4], b[5]]) & 0x7FF, b[6], 0));
                }
            }
            0x30 if len >= 6 && !skip_alt => {
                // SuperSpeed companion: max burst for the previous endpoint.
                if let Some(e) = out.last_mut().and_then(|f| f.eps.last_mut()) {
                    e.4 = b[2];
                }
            }
            _ => {}
        }
        i += len;
    }
    out
}

/// USB HID keyboard usage -> PS/2 set 1 scancode (what the rest of Solon speaks).
fn usage_to_set1(u: u8) -> u8 {
    const LETTERS: [u8; 26] = [
        0x1E, 0x30, 0x2E, 0x20, 0x12, 0x21, 0x22, 0x23, 0x17, 0x24, 0x25, 0x26, 0x32, 0x31, 0x18, 0x19, 0x10, 0x13,
        0x1F, 0x14, 0x16, 0x2F, 0x11, 0x2D, 0x15, 0x2C,
    ];
    match u {
        0x04..=0x1D => LETTERS[(u - 0x04) as usize],
        0x1E..=0x27 => u - 0x1E + 0x02, // 1..9, 0
        0x28 => 0x1C,                   // Enter
        0x29 => 0x01,                   // Esc
        0x2A => 0x0E,                   // Backspace
        0x2B => 0x0F,                   // Tab
        0x2C => 0x39,                   // Space
        0x2D => 0x0C,
        0x2E => 0x0D,
        0x3A..=0x43 => u - 0x3A + 0x3B, // F1..F10
        0x44 => 0x57,
        0x45 => 0x58,
        0x4A => 0x47, // Home
        0x4B => 0x49, // PageUp
        0x4C => 0x53, // Delete
        0x4D => 0x4F, // End
        0x4E => 0x51, // PageDown
        0x4F => 0x4D, // Right
        0x50 => 0x4B, // Left
        0x51 => 0x50, // Down
        0x52 => 0x48, // Up
        _ => 0,
    }
}

// Hub class requests
const HUB_PORT_STATUS: u8 = 0xA3;
const HUB_SET_PORT_FEATURE: u8 = 0x23;
const PORT_RESET: u16 = 4;
const PORT_POWER: u16 = 8;
const C_PORT_CONNECTION: u16 = 16;
const C_PORT_RESET: u16 = 20;

fn hub_port_status(hc: &mut Xhci, slot: u8, port: u8) -> Option<(u16, u16)> {
    let buf = dma_buf(4);
    hc.control(slot, HUB_PORT_STATUS, 0, 0, port as u16, Some((&buf, 4))).ok()?;
    let b = buf.bytes();
    Some((u16::from_le_bytes([b[0], b[1]]), u16::from_le_bytes([b[2], b[3]])))
}

fn hub_clear(hc: &mut Xhci, slot: u8, port: u8, feature: u16) {
    let _ = hc.control(slot, HUB_SET_PORT_FEATURE, 1, feature, port as u16, None);
}

impl Usb {
    pub fn init(boot_disk: Option<[u8; 16]>) -> Usb {
        let mut hcs = Vec::new();
        for d in pci::scan() {
            if d.class == 0x0C && d.subclass == 0x03 {
                match d.prog_if {
                    0x30 => match Xhci::init(&d) {
                        Some(hc) => hcs.push(hc),
                        None => kprintln!("usb: xHCI {:04x}:{:04x} failed to start", d.vendor, d.device),
                    },
                    0x20 => kprintln!("usb: EHCI (USB 2) controller found; not supported yet"),
                    0x00 | 0x10 => kprintln!("usb: UHCI/OHCI (USB 1) controller found; not supported yet"),
                    _ => {}
                }
            }
        }
        if hcs.is_empty() {
            kprintln!("usb: no xHCI controller");
        }
        let mut usb = Usb {
            hcs,
            devs: Vec::new(),
            hubs: Vec::new(),
            hids: Vec::new(),
            drives: Vec::new(),
            printers: Vec::new(),
            phones: Vec::new(),
            next_id: 1,
            next_hub_poll: 0,
            boot_disk,
        };
        usb.poll();
        // Devices behind hubs need a few rounds of hub polling to appear.
        for _ in 0..3 {
            if usb.hubs.is_empty() {
                break;
            }
            usb.next_hub_poll = 0;
            usb.poll();
        }
        usb
    }

    /// Handle plug/unplug and input reports. Returns true if the set of drives changed.
    pub fn poll(&mut self) -> bool {
        let mut changed = false;
        for ci in 0..self.hcs.len() {
            self.hcs[ci].poll();
            for port in self.hcs[ci].take_port_changes() {
                let present = self.devs.iter().any(|d| d.ctrl == ci && d.place.parent.is_none() && d.place.root_port == port);
                let connected = self.hcs[ci].port_connected(port);
                let replugged = self.hcs[ci].take_connect_change(port);
                if present && (!connected || replugged) {
                    changed |= self.detach_where(ci, |p| p.root_port == port);
                }
                let present = self.devs.iter().any(|d| d.ctrl == ci && d.place.parent.is_none() && d.place.root_port == port);
                if connected && !present {
                    if let Some(speed) = self.hcs[ci].reset_port(port) {
                        let place = Place { root_port: port, route: 0, depth: 0, tt: None, parent: None };
                        changed |= self.attach(ci, place, speed);
                    }
                }
            }
        }
        let now = TICKS.load(Relaxed);
        if !self.hubs.is_empty() && now >= self.next_hub_poll {
            self.next_hub_poll = now + HZ / 4;
            changed |= self.poll_hubs();
        }
        self.poll_hid();
        changed
    }

    fn poll_hubs(&mut self) -> bool {
        let mut changed = false;
        let hubs: Vec<(usize, u8, u8, Speed, Place)> =
            self.hubs.iter().map(|h| (h.ctrl, h.slot, h.ports, h.speed, h.place)).collect();
        for (ci, hslot, ports, hspeed, hplace) in hubs {
            if !self.hubs.iter().any(|h| h.ctrl == ci && h.slot == hslot) {
                continue; // removed while we were going round
            }
            for port in 1..=ports {
                let Some((status, change)) = hub_port_status(&mut self.hcs[ci], hslot, port) else { continue };
                let connected = status & 1 != 0;
                let present = self.devs.iter().any(|d| d.ctrl == ci && d.place.parent == Some((hslot, port)));
                if change & 1 != 0 {
                    hub_clear(&mut self.hcs[ci], hslot, port, C_PORT_CONNECTION);
                }
                if present && (!connected || change & 1 != 0) {
                    changed |= self.detach_where(ci, |p| p.parent == Some((hslot, port)));
                }
                let present = self.devs.iter().any(|d| d.ctrl == ci && d.place.parent == Some((hslot, port)));
                if connected && !present {
                    let Some(speed) = self.reset_hub_port(ci, hslot, port, hspeed) else { continue };
                    let tt = if hspeed == Speed::High && matches!(speed, Speed::Low | Speed::Full) {
                        Some((hslot, port, false))
                    } else {
                        hplace.tt
                    };
                    let place = Place {
                        root_port: hplace.root_port,
                        route: hplace.route | ((port.min(15) as u32) << (4 * hplace.depth as u32)),
                        depth: hplace.depth + 1,
                        tt,
                        parent: Some((hslot, port)),
                    };
                    changed |= self.attach(ci, place, speed);
                }
            }
        }
        changed
    }

    fn reset_hub_port(&mut self, ci: usize, hslot: u8, port: u8, hub_speed: Speed) -> Option<Speed> {
        let hc = &mut self.hcs[ci];
        let _ = hc.control(hslot, HUB_SET_PORT_FEATURE, 3, PORT_RESET, port as u16, None);
        let mut status = 0;
        for _ in 0..50 {
            sleep_ms(10);
            let (s, c) = hub_port_status(hc, hslot, port)?;
            status = s;
            if c & (1 << 4) != 0 || (s & (1 << 4) == 0 && s & 2 != 0) {
                break;
            }
        }
        hub_clear(hc, hslot, port, C_PORT_RESET);
        sleep_ms(20); // reset recovery
        if status & 2 == 0 {
            kprintln!("usb: hub {} port {} did not enable", hslot, port);
            return None;
        }
        Some(if hub_speed == Speed::Super {
            Speed::Super
        } else if status & (1 << 9) != 0 {
            Speed::Low
        } else if status & (1 << 10) != 0 {
            Speed::High
        } else {
            Speed::Full
        })
    }

    fn attach(&mut self, ci: usize, place: Place, speed: Speed) -> bool {
        let slot = match self.hcs[ci].enable_slot() {
            Ok(s) => s,
            Err(e) => {
                kprintln!("usb: enable slot failed: {:?}", e);
                return false;
            }
        };
        match self.setup_device(ci, place, slot, speed) {
            Ok(drive_added) => drive_added,
            Err(e) => {
                kprintln!("usb: port {} route {:#x}: device setup failed: {:?}", place.root_port, place.route, e);
                self.devs.retain(|d| !(d.ctrl == ci && d.slot == slot));
                self.hubs.retain(|h| !(h.ctrl == ci && h.slot == slot));
                self.hids.retain(|h| !(h.ctrl == ci && h.slot == slot));
                self.hcs[ci].disable_slot(slot);
                false
            }
        }
    }

    fn setup_device(&mut self, ci: usize, place: Place, slot: u8, speed: Speed) -> Result<bool, UsbError> {
        let hc = &mut self.hcs[ci];
        hc.address_device(slot, place.root_port, place.route, speed, place.tt)?;
        let buf = dma_buf(512);
        // First 8 bytes tell us EP0's real max packet size.
        hc.control(slot, 0x80, 6, 0x0100, 0, Some((&buf, 8)))?;
        let b = buf.bytes();
        let mps = if speed == Speed::Super { 1u16 << b[7].min(9) } else { b[7] as u16 };
        if mps != speed.default_mps0() && mps >= 8 {
            hc.set_ep0_mps(slot, mps)?;
        }
        hc.control(slot, 0x80, 6, 0x0100, 0, Some((&buf, 18)))?;
        let dd: [u8; 18] = buf.bytes()[..18].try_into().unwrap();
        let (vid, pid) = (u16::from_le_bytes([dd[8], dd[9]]), u16::from_le_bytes([dd[10], dd[11]]));
        let product = string_descriptor(hc, slot, dd[15]);

        hc.control(slot, 0x80, 6, 0x0200, 0, Some((&buf, 9)))?;
        let total = (u16::from_le_bytes([buf.bytes()[2], buf.bytes()[3]]) as usize).clamp(9, 4096);
        let cfg_buf = dma_buf(total);
        hc.control(slot, 0x80, 6, 0x0200, 0, Some((&cfg_buf, total)))?;
        let cfg = &cfg_buf.bytes()[..total];
        let config_value = cfg[5];
        let ifaces = parse_config(cfg);
        kprintln!(
            "usb: port {} route {:#x} slot {}: {:04x}:{:04x} {:?} \"{}\" ({} interface(s))",
            place.root_port,
            place.route,
            slot,
            vid,
            pid,
            speed,
            product,
            ifaces.len()
        );
        self.devs.push(Dev { ctrl: ci, slot, place });

        hc.control(slot, 0x00, 9, config_value as u16, 0, None)?; // SET_CONFIGURATION

        // Hubs
        if dd[4] == 9 || ifaces.first().map(|f| f.class) == Some(9) {
            return self.setup_hub(ci, slot, speed, place).map(|_| false);
        }

        // Pick the interfaces we know how to drive.
        let mut eps = Vec::new();
        // Printers: prefer IPP over USB (protocol 4), then bidirectional (2), then plain (1).
        let printer_iface = [4u8, 2, 1].iter().find_map(|&p| {
            ifaces.iter().find(|f| f.class == 7 && f.sub == 1 && f.proto == p && f.eps.iter().any(|e| e.0 & 0x80 == 0 && e.1 & 3 == 2))
        });
        let mut printer_plan = None;
        if let Some(f) = printer_iface {
            let o = f.eps.iter().find(|e| e.0 & 0x80 == 0 && e.1 & 3 == 2).unwrap();
            let i = f.eps.iter().find(|e| e.0 & 0x80 != 0 && e.1 & 3 == 2);
            eps.push(EpConfig { dci: (o.0 & 0xF) * 2, kind: EpType::BulkOut, max_packet: o.2, interval: 0, max_burst: o.4 });
            if let Some(i) = i {
                eps.push(EpConfig { dci: (i.0 & 0xF) * 2 + 1, kind: EpType::BulkIn, max_packet: i.2, interval: 0, max_burst: i.4 });
            }
            printer_plan = Some((f.num, f.alt, f.proto, o.0, i.map(|e| e.0)));
        }
        let mut hid_plans = Vec::new();
        let mut msc_plan = None;
        // Phones and cameras: PTP (still image class 6/1/1: iPhones, cameras, many Android
        // phones), or Android's MTP as a vendor interface named "MTP".
        let mut phone_plan = None;
        for f in &ifaces {
            let bulk_in = f.eps.iter().find(|e| e.0 & 0x80 != 0 && e.1 & 3 == 2);
            let bulk_out = f.eps.iter().find(|e| e.0 & 0x80 == 0 && e.1 & 3 == 2);
            let (Some(i), Some(o)) = (bulk_in, bulk_out) else { continue };
            let ptp = f.class == 6 && f.sub == 1 && f.proto == 1;
            let mtp = f.class == 0xFF && f.istr != 0 && string_descriptor(&mut self.hcs[ci], slot, f.istr).trim().eq_ignore_ascii_case("MTP");
            if (ptp || mtp) && phone_plan.is_none() {
                eps.push(EpConfig { dci: (i.0 & 0xF) * 2 + 1, kind: EpType::BulkIn, max_packet: i.2, interval: 0, max_burst: i.4 });
                eps.push(EpConfig { dci: (o.0 & 0xF) * 2, kind: EpType::BulkOut, max_packet: o.2, interval: 0, max_burst: o.4 });
                phone_plan = Some((f.num, i.0, o.0));
            }
        }
        for f in &ifaces {
            if phone_plan.is_some_and(|(n, _, _)| n == f.num) {
                continue;
            }
            if f.class == 3 && f.sub == 1 && (f.proto == 1 || f.proto == 2) {
                if let Some(&(addr, _, mps, interval, burst)) = f.eps.iter().find(|e| e.0 & 0x80 != 0 && e.1 & 3 == 3) {
                    let dci = (addr & 0xF) * 2 + 1;
                    eps.push(EpConfig { dci, kind: EpType::InterruptIn, max_packet: mps, interval, max_burst: burst });
                    let kind = if f.proto == 1 { HidKind::Keyboard } else { HidKind::Mouse };
                    hid_plans.push((f.num, dci, mps, kind));
                }
            } else if f.class == 7 {
                // printer: handled above
            } else if f.class == 8 && f.sub == 6 && f.proto == 0x50 && msc_plan.is_none() {
                let ein = f.eps.iter().find(|e| e.0 & 0x80 != 0 && e.1 & 3 == 2);
                let eout = f.eps.iter().find(|e| e.0 & 0x80 == 0 && e.1 & 3 == 2);
                if let (Some(i), Some(o)) = (ein, eout) {
                    eps.push(EpConfig { dci: (i.0 & 0xF) * 2 + 1, kind: EpType::BulkIn, max_packet: i.2, interval: 0, max_burst: i.4 });
                    eps.push(EpConfig { dci: (o.0 & 0xF) * 2, kind: EpType::BulkOut, max_packet: o.2, interval: 0, max_burst: o.4 });
                    msc_plan = Some((f.num, i.0, o.0));
                }
            } else {
                kprintln!("usb:   interface {} class {:02x}/{:02x}/{:02x}: no driver", f.num, f.class, f.sub, f.proto);
            }
        }
        if eps.is_empty() {
            return Ok(false);
        }
        let hc = &mut self.hcs[ci];
        hc.configure_endpoints(slot, &eps, None)?;

        for (iface, dci, mps, kind) in hid_plans {
            let _ = hc.control(slot, 0x21, 0x0B, 0, iface as u16, None); // SET_PROTOCOL(boot)
            let _ = hc.control(slot, 0x21, 0x0A, 0, iface as u16, None); // SET_IDLE(0)
            let len = (mps as usize).clamp(3, 8);
            let hid = Hid { ctrl: ci, slot, dci, len, kind, buf: dma_buf(64), prev: [0; 8] };
            hc.queue(slot, dci, hid.buf.phys(), len)?;
            kprintln!("usb:   {} ready", if kind == HidKind::Keyboard { "keyboard" } else { "mouse" });
            self.hids.push(hid);
        }

        if let Some((iface, alt, proto, eout, ein)) = printer_plan {
            if alt != 0 {
                let _ = hc.control(slot, 0x01, 0x0B, alt as u16, iface as u16, None); // SET_INTERFACE
            }
            let id = self.next_id;
            self.next_id += 1;
            let p = UsbPrinter::new(hc, id, ci, slot, iface, alt, proto, eout, ein, &product);
            kprintln!(
                "usb:   printer \"{}\": {}, languages {:?}{}",
                p.name,
                if p.ipp { "IPP over USB" } else { "raw" },
                p.langs,
                if p.usable() { "" } else { " (no language Solon speaks yet)" }
            );
            self.printers.push(p);
            return Ok(true);
        }

        if let Some((iface, ein, eout)) = phone_plan {
            let _ = iface;
            let id = self.next_id;
            self.next_id += 1;
            let mut p = Phone::usb(id, ci, slot, ein, eout, &product, vid);
            p.start(&mut self.hcs);
            kprintln!("usb:   phone \"{}\": {} storage area(s){}", p.name, p.storages.len(), p.problem.map(|m| alloc::format!(" ({})", m)).unwrap_or_default());
            self.phones.push(p);
            return Ok(true);
        }

        if let Some((iface, ein, eout)) = msc_plan {
            let mut ms = MassStorage::new(slot, iface, ein, eout);
            if let Err(e) = ms.start(hc) {
                kprintln!("usb:   storage did not start: {:?}", e);
                return Ok(false);
            }
            let boot_id = self.boot_disk;
            let (volumes, is_boot) = {
                let blocks = ms.blocks;
                let mut disk = Disk { hc, ms: &mut ms };
                let mut lba1 = alloc::vec![0u8; disk.block_size()];
                let is_boot = boot_id.is_some()
                    && disk.read_blocks(1, &mut lba1).is_ok()
                    && &lba1[0..8] == b"EFI PART"
                    && Some(&lba1[56..72]) == boot_id.as_ref().map(|g| &g[..]);
                (fs::probe(&mut disk, blocks), is_boot)
            };
            if is_boot {
                kprintln!("usb:   this is the stick Solon booted from");
            }
            let name = if !product.is_empty() {
                product.clone()
            } else if !ms.product.is_empty() {
                format!("{} {}", ms.vendor, ms.product)
            } else {
                String::from("USB drive")
            };
            kprintln!(
                "usb:   storage \"{}\": {} MiB, {}-byte blocks, {} volume(s)",
                name,
                ms.size_bytes() >> 20,
                ms.block_size,
                volumes.len()
            );
            for v in &volumes {
                kprintln!("usb:     {} \"{}\" {} MiB", v.kind.name(), v.label, v.size >> 20);
            }
            let id = self.next_id;
            self.next_id += 1;
            self.drives.push(Drive { id, is_boot, ctrl: ci, slot, name, ms, volumes });
            return Ok(true);
        }
        Ok(false)
    }

    fn setup_hub(&mut self, ci: usize, slot: u8, speed: Speed, place: Place) -> Result<(), UsbError> {
        if place.depth >= 5 {
            kprintln!("usb:   hub nested too deep; ignored");
            return Ok(());
        }
        let hc = &mut self.hcs[ci];
        let buf = dma_buf(64);
        let desc_type = if speed == Speed::Super { 0x2A00 } else { 0x2900 };
        hc.control(slot, 0xA0, 6, desc_type, 0, Some((&buf, 12)))?;
        let ports = buf.bytes()[2].min(15);
        let power_ms = buf.bytes()[5] as u64 * 2;
        hc.configure_endpoints(slot, &[], Some(ports))?;
        if speed == Speed::Super {
            let _ = hc.control(slot, 0x20, 12, place.depth as u16, 0, None); // SET_HUB_DEPTH
        }
        for p in 1..=ports {
            let _ = hc.control(slot, HUB_SET_PORT_FEATURE, 3, PORT_POWER, p as u16, None);
        }
        sleep_ms(power_ms.clamp(20, 500));
        kprintln!("usb:   hub with {} ports ready", ports);
        self.hubs.push(Hub { ctrl: ci, slot, ports, speed, place });
        self.next_hub_poll = 0;
        Ok(())
    }

    /// Remove every device whose place matches, plus everything plugged into those devices.
    fn detach_where(&mut self, ci: usize, matches: impl Fn(&Place) -> bool) -> bool {
        let mut gone: Vec<u8> = self.devs.iter().filter(|d| d.ctrl == ci && matches(&d.place)).map(|d| d.slot).collect();
        // Children of removed hubs.
        let mut i = 0;
        while i < gone.len() {
            let hub = gone[i];
            for d in &self.devs {
                if d.ctrl == ci && matches!(d.place.parent, Some((h, _)) if h == hub) && !gone.contains(&d.slot) {
                    gone.push(d.slot);
                }
            }
            i += 1;
        }
        let before = self.drives.len();
        for &slot in gone.iter().rev() {
            kprintln!("usb: slot {} disconnected", slot);
            self.hcs[ci].disable_slot(slot);
        }
        self.devs.retain(|d| !(d.ctrl == ci && gone.contains(&d.slot)));
        self.hubs.retain(|h| !(h.ctrl == ci && gone.contains(&h.slot)));
        self.hids.retain(|h| !(h.ctrl == ci && gone.contains(&h.slot)));
        let printers_before = self.printers.len();
        self.printers.retain(|p| !(p.ctrl == ci && gone.contains(&p.slot)));
        let printer_gone = self.printers.len() != printers_before;
        let phones_before = self.phones.len();
        self.phones.retain(|p| !p.ctrl_slot().is_some_and(|(c, s)| c == ci && gone.contains(&s)));
        let phone_gone = self.phones.len() != phones_before;
        self.drives.retain(|d| !(d.ctrl == ci && gone.contains(&d.slot)));
        self.drives.len() != before || printer_gone || phone_gone
    }

    fn poll_hid(&mut self) {
        for h in &mut self.hids {
            let hc = &mut self.hcs[h.ctrl];
            let Some(c) = hc.take_completion(h.slot, h.dci) else { continue };
            if c.code == CC_SUCCESS || c.code == CC_SHORT {
                let n = h.len.saturating_sub(c.residual as usize).min(8);
                let mut r = [0u8; 8];
                r[..n].copy_from_slice(&h.buf.bytes()[..n]);
                match h.kind {
                    HidKind::Keyboard => keyboard_report(&h.prev, &r),
                    HidKind::Mouse if n >= 3 => {
                        let wheel = if n >= 4 { -(r[3] as i8) } else { 0 };
                        ps2::inject_mouse(r[1] as i8 as i32, r[2] as i8 as i32, r[0] & 7, wheel);
                    }
                    _ => {}
                }
                h.prev = r;
            } else {
                let _ = hc.recover_endpoint(h.slot, h.dci);
            }
            let _ = hc.queue(h.slot, h.dci, h.buf.phys(), h.len);
        }
    }

    // ---- file access for the apps -------------------------------------------------

    pub fn list(&mut self, drive: u32, vol: usize, dir: &DirEntry) -> Result<Vec<DirEntry>, FsError> {
        if let Some(p) = self.phones.iter_mut().find(|p| p.id == drive) {
            return p.list(&mut self.hcs, vol, dir);
        }
        let Usb { hcs, drives, .. } = self;
        let d = drives.iter_mut().find(|d| d.id == drive).ok_or(FsError::Io)?;
        let v = d.volumes.get_mut(vol).ok_or(FsError::Io)?;
        let mut disk = Disk { hc: &mut hcs[d.ctrl], ms: &mut d.ms };
        v.list(&mut disk, dir)
    }

    pub fn read_file(&mut self, drive: u32, vol: usize, file: &DirEntry) -> Result<Vec<u8>, FsError> {
        let data = if let Some(p) = self.phones.iter_mut().find(|p| p.id == drive) {
            p.read_file(&mut self.hcs, file)?
        } else {
            let Usb { hcs, drives, .. } = self;
            let d = drives.iter_mut().find(|d| d.id == drive).ok_or(FsError::Io)?;
            let v = d.volumes.get_mut(vol).ok_or(FsError::Io)?;
            let mut disk = Disk { hc: &mut hcs[d.ctrl], ms: &mut d.ms };
            v.read_file(&mut disk, file)?
        };
        // Checksum in the log lets tests verify every byte came through intact.
        let mut h: u32 = 0x811C_9DC5;
        for &b in &data {
            h = (h ^ b as u32).wrapping_mul(0x0100_0193);
        }
        kprintln!("usb: read \"{}\" {} bytes fnv {:08x}", file.name, data.len(), h);
        Ok(data)
    }

    /// Send a raw print job (PCL) to a printer.
    pub fn printer_send(&mut self, id: u32, data: &[u8]) -> Result<(), &'static str> {
        let Usb { hcs, printers, .. } = self;
        let p = printers.iter_mut().find(|p| p.id == id).ok_or("The printer was disconnected")?;
        p.send(&mut hcs[p.ctrl], data).map_err(|_| "Couldn't send the job to the printer")
    }

    /// One IPP request/response with a printer (IPP over USB).
    pub fn printer_ipp(&mut self, id: u32, request: &[u8]) -> Result<Vec<u8>, &'static str> {
        let Usb { hcs, printers, .. } = self;
        let p = printers.iter_mut().find(|p| p.id == id).ok_or("The printer was disconnected")?;
        p.ipp_request(&mut hcs[p.ctrl], request)
    }

    /// Add a file (`data` = Some) or folder to another USB drive ("Copy to USB"). Only ever
    /// adds: nothing existing is changed or deleted. Solon's own boot stick is refused.
    pub fn usb_create(&mut self, drive: u32, vol: usize, dir: &DirEntry, name: &str, data: Option<&[u8]>) -> Result<DirEntry, FsError> {
        let Usb { hcs, drives, .. } = self;
        let d = drives.iter_mut().find(|d| d.id == drive).ok_or(FsError::Io)?;
        if d.is_boot {
            return Err(FsError::ReadOnly);
        }
        let v = d.volumes.get_mut(vol).ok_or(FsError::Io)?;
        let mut dev = BootDisk { hc: &mut hcs[d.ctrl], ms: &mut d.ms };
        if !v.writes_ready {
            v.enable_writes(&mut dev)?;
        }
        let r = match data {
            Some(bytes) => v.create_file(&mut dev, dir, name, bytes),
            None => v.mkdir(&mut dev, dir, name),
        };
        kprintln!("usb: wrote \"{}\" to drive {}: {}", name, drive, if r.is_ok() { "ok" } else { "failed" });
        r
    }

    /// Erase a USB drive and format it as one exFAT volume. Never the boot stick.
    pub fn format_drive(&mut self, drive: u32, label: &str) -> Result<(), FsError> {
        let Usb { hcs, drives, .. } = self;
        let d = drives.iter_mut().find(|d| d.id == drive).ok_or(FsError::Io)?;
        if d.is_boot {
            return Err(FsError::ReadOnly);
        }
        let blocks = d.ms.blocks;
        {
            let mut dev = BootDisk { hc: &mut hcs[d.ctrl], ms: &mut d.ms };
            let (start, len) = fs::part::write_single_mbr(&mut dev, blocks, 0x07)?;
            let serial = crate::rtc::fat_timestamp() ^ blocks as u32;
            fs::exfat::format(&mut dev, start, len, label, serial)?;
        }
        let mut disk = Disk { hc: &mut hcs[d.ctrl], ms: &mut d.ms };
        d.volumes = fs::probe(&mut disk, blocks);
        kprintln!("usb: drive {} erased and formatted, {} volume(s)", drive, d.volumes.len());
        Ok(())
    }

    /// Free space on a USB drive volume.
    pub fn volume_free(&mut self, drive: u32, vol: usize) -> Option<u64> {
        let Usb { hcs, drives, .. } = self;
        let d = drives.iter_mut().find(|d| d.id == drive)?;
        let v = d.volumes.get_mut(vol)?;
        let mut disk = Disk { hc: &mut hcs[d.ctrl], ms: &mut d.ms };
        v.count_free(&mut disk).ok()
    }

    /// The id of Solon's boot stick, if it's plugged in.
    pub fn boot_drive(&self) -> Option<u32> {
        self.drives.iter().find(|d| d.is_boot).map(|d| d.id)
    }

    /// Writable access to Solon's boot stick (and only that drive).
    pub fn boot_disk(&mut self, drive: u32) -> Option<(BootDisk<'_>, u64)> {
        let Usb { hcs, drives, .. } = self;
        let d = drives.iter_mut().find(|d| d.id == drive && d.is_boot)?;
        let blocks = d.ms.blocks;
        Some((BootDisk { hc: &mut hcs[d.ctrl], ms: &mut d.ms }, blocks))
    }

    pub fn has_drive(&self, drive: u32) -> bool {
        self.drives.iter().any(|d| d.id == drive) || self.phones.iter().any(|p| p.id == drive)
    }
}

fn keyboard_report(prev: &[u8; 8], now: &[u8; 8]) {
    // Modifier byte: left/right ctrl, shift, alt, gui.
    const MODS: [u8; 8] = [0x1D, 0x2A, 0x38, 0x5B, 0x1D, 0x36, 0x38, 0x5C];
    for (bit, &sc) in MODS.iter().enumerate() {
        let (was, is) = (prev[0] >> bit & 1, now[0] >> bit & 1);
        if was != is {
            ps2::inject(ps2::Event::Key(if is == 1 { sc } else { sc | 0x80 }));
        }
    }
    if now[2] == 1 {
        return; // "phantom" state: too many keys held
    }
    for &u in &prev[2..8] {
        if u > 3 && !now[2..8].contains(&u) {
            let sc = usage_to_set1(u);
            if sc != 0 {
                ps2::inject(ps2::Event::Key(sc | 0x80));
            }
        }
    }
    for &u in &now[2..8] {
        if u > 3 && !prev[2..8].contains(&u) {
            let sc = usage_to_set1(u);
            if sc != 0 {
                ps2::inject(ps2::Event::Key(sc));
            }
        }
    }
}

fn string_descriptor(hc: &mut Xhci, slot: u8, index: u8) -> String {
    if index == 0 {
        return String::new();
    }
    let buf = dma_buf(256);
    if hc.control(slot, 0x80, 6, 0x0300 | index as u16, 0x0409, Some((&buf, 255))).is_err() {
        return String::new();
    }
    let b = buf.bytes();
    let len = (b[0] as usize).min(255);
    if len < 2 || b[1] != 3 {
        return String::new();
    }
    let units: Vec<u16> = b[2..len].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let s = fs::utf16_name(&units);
    String::from(s.trim())
}

/// Something on USB the apps can browse: a drive's volume or a phone's storage.
pub struct UsbSource {
    pub drive: u32,
    pub vol: usize,
    /// Short name (volume label, or "Pixel 7" / "Pixel 7: SD card").
    pub label: String,
    pub phone: bool,
}

impl Usb {
    /// Every browsable volume and phone storage (not Solon's own stick).
    pub fn sources(&self) -> Vec<UsbSource> {
        let mut v = Vec::new();
        for d in self.drives.iter().filter(|d| !d.is_boot) {
            for (i, vol) in d.volumes.iter().enumerate() {
                let label = if vol.label.is_empty() { d.name.clone() } else { vol.label.clone() };
                v.push(UsbSource { drive: d.id, vol: i, label, phone: false });
            }
        }
        for p in &self.phones {
            for (i, s) in p.storages.iter().enumerate() {
                let label = if p.storages.len() == 1 { p.name.clone() } else { alloc::format!("{}: {}", p.name, s.label) };
                v.push(UsbSource { drive: p.id, vol: i, label, phone: true });
            }
        }
        v
    }

    /// The top folder of a source.
    pub fn source_root(&self, drive: u32, vol: usize) -> Option<DirEntry> {
        if let Some(p) = self.phones.iter().find(|p| p.id == drive) {
            return p.root(vol);
        }
        self.drives.iter().find(|d| d.id == drive)?.volumes.get(vol).map(|v| v.root())
    }

    pub fn is_phone(&self, drive: u32) -> bool {
        self.phones.iter().any(|p| p.id == drive)
    }

    /// Part of a file on a phone (big files are copied a piece at a time).
    pub fn read_part(&mut self, drive: u32, file: &DirEntry, offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
        let p = self.phones.iter_mut().find(|p| p.id == drive).ok_or(FsError::Io)?;
        p.read_part(&mut self.hcs, file, offset, len)
    }

    /// Ask phones that weren't sharing files again (after the user unlocked them).
    /// Returns true if something changed.
    pub fn retry_phones(&mut self) -> bool {
        let mut changed = false;
        let Usb { hcs, phones, .. } = self;
        for p in phones.iter_mut().filter(|p| p.storages.is_empty()) {
            changed |= p.retry(hcs);
        }
        changed
    }

    /// The pretend phone (boot option "testphone").
    pub fn add_test_phone(&mut self) {
        let id = self.next_id;
        self.next_id += 1;
        let mut p = Phone::test(id);
        p.start(&mut self.hcs);
        kprintln!("usb: test phone \"{}\": {} storage area(s)", p.name, p.storages.len());
        self.phones.push(p);
    }
}

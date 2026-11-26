//! Internal disks (NVMe, SATA and IDE). Unlike USB drives these can be written, but Solon
//! only ever writes inside its own library partition (see `library.rs`) and to the
//! partition table when the user confirms creating that partition.
//!
//! A SATA controller can be set (in the firmware) to one of three modes, and laptops ship in
//! all of them: AHCI (class 01:06), "RAID" (01:04; Intel's and AMD's still work like AHCI,
//! so `ahci.rs` drives them) or "ATA"/"IDE" (01:01, `ide.rs`).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ahci::{self, AhciPort};
use crate::fs::{BlockDev, FsError};
use crate::ide::{self, IdeDisk};
use crate::nvme::Nvme;
use crate::pci;

pub enum Driver {
    Nvme(Nvme),
    Ahci(AhciPort),
    Ide(IdeDisk),
}

pub struct InternalDisk {
    pub name: String,
    pub kind: &'static str,
    pub block_size: usize,
    pub blocks: u64,
    driver: Driver,
}

impl InternalDisk {
    pub fn size_bytes(&self) -> u64 {
        self.blocks * self.block_size as u64
    }
}

impl BlockDev for InternalDisk {
    fn block_size(&self) -> usize {
        self.block_size
    }
    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError> {
        if lba + (buf.len() / self.block_size) as u64 > self.blocks {
            return Err(FsError::Io);
        }
        let r = match &mut self.driver {
            Driver::Nvme(n) => n.read(lba, buf).map_err(|e| e as u32),
            Driver::Ahci(a) => a.read(lba, buf),
            Driver::Ide(d) => d.read(lba, buf),
        };
        r.map_err(|e| {
            kprintln!("disk: {} read error {:#x} at block {}", self.name, e, lba);
            FsError::Io
        })
    }
    fn write_blocks(&mut self, lba: u64, buf: &[u8]) -> Result<(), FsError> {
        if lba + (buf.len() / self.block_size) as u64 > self.blocks {
            return Err(FsError::Io);
        }
        let r = match &mut self.driver {
            Driver::Nvme(n) => n.write(lba, buf).map_err(|e| e as u32),
            Driver::Ahci(a) => a.write(lba, buf),
            Driver::Ide(d) => d.write(lba, buf),
        };
        r.map_err(|e| {
            kprintln!("disk: {} write error {:#x} at block {}", self.name, e, lba);
            FsError::Io
        })
    }
    fn flush(&mut self) -> Result<(), FsError> {
        let r = match &mut self.driver {
            Driver::Nvme(n) => n.flush().map_err(|e| e as u32),
            Driver::Ahci(a) => a.flush(),
            Driver::Ide(d) => d.flush(),
        };
        r.map_err(|_| FsError::Io)
    }
}

/// Find all internal disks.
pub fn scan() -> Vec<InternalDisk> {
    let mut out = Vec::new();
    for d in pci::scan() {
        if d.class != 0x01 {
            continue;
        }
        kprintln!(
            "disk: controller {:04x}:{:04x} at {:02x}:{:02x}.{}, class 01:{:02x}:{:02x} ({})",
            d.vendor, d.device, d.bus, d.dev, d.func, d.subclass, d.prog_if, kind_name(d.subclass, d.prog_if)
        );
        match (d.subclass, d.prog_if) {
            (0x08, 0x02) => match Nvme::init(&d) {
                Some(n) => {
                    let name = if n.model.is_empty() { String::from("NVMe SSD") } else { n.model.clone() };
                    out.push(InternalDisk { name, kind: "NVMe", block_size: n.block_size, blocks: n.blocks, driver: Driver::Nvme(n) });
                }
                None => kprintln!("disk: NVMe controller {:04x}:{:04x} failed to start", d.vendor, d.device),
            },
            (0x06, 0x01) => add_sata(&mut out, &d, false),
            // "RAID On": Intel's Rapid Storage and AMD's RAID are AHCI underneath.
            (0x04, _) if matches!(d.vendor, 0x8086 | 0x1022) && ahci::is_ahci(&d) => add_sata(&mut out, &d, true),
            (0x04, _) => kprintln!("disk:   a RAID controller Solon has no driver for"),
            (0x01, _) => {
                for disk in ide::init(&d) {
                    let name = if disk.model.is_empty() { String::from("ATA disk") } else { disk.model.clone() };
                    out.push(InternalDisk { name, kind: "SATA", block_size: disk.block_size, blocks: disk.blocks, driver: Driver::Ide(disk) });
                }
            }
            _ => {}
        }
    }
    for d in &out {
        kprintln!("disk: {} {} {} ({}-byte blocks)", d.kind, d.name, human(d.size_bytes()), d.block_size);
    }
    if out.is_empty() {
        kprintln!("disk: no internal disks found");
    }
    out
}

fn add_sata(out: &mut Vec<InternalDisk>, d: &pci::Device, raid: bool) {
    for p in ahci::init(d) {
        let name = if p.model.is_empty() { String::from("SATA disk") } else { p.model.clone() };
        let mut disk = InternalDisk { name, kind: "SATA", block_size: p.block_size, blocks: p.blocks, driver: Driver::Ahci(p) };
        // A disk that's part of a RAID volume (several disks acting as one) holds only pieces
        // of its files: reading it alone shows nonsense and writing breaks the volume.
        if raid && intel_raid_member(&mut disk) {
            kprintln!("disk:   {} is part of an Intel RAID volume; left alone", disk.name);
            continue;
        }
        out.push(disk);
    }
}

/// Intel's RAID keeps its description in the second-to-last 512-byte block.
fn intel_raid_member(d: &mut InternalDisk) -> bool {
    let bs = d.block_size as u64;
    let at = (d.blocks * bs).saturating_sub(1024) / bs;
    let mut buf = alloc::vec![0u8; d.block_size];
    if d.read_blocks(at, &mut buf).is_err() {
        return false;
    }
    let off = ((d.blocks * bs - 1024) - at * bs) as usize;
    buf.get(off..off + 24).is_some_and(|s| s == b"Intel Raid ISM Cfg Sig. ")
}

fn kind_name(subclass: u8, prog_if: u8) -> &'static str {
    match (subclass, prog_if) {
        (0x01, _) => "IDE",
        (0x04, _) => "RAID",
        (0x05, _) => "ATA",
        (0x06, 0x01) => "AHCI SATA",
        (0x06, _) => "SATA",
        (0x07, _) => "SAS",
        (0x08, 0x02) => "NVMe",
        (0x08, _) => "flash",
        _ => "other",
    }
}

pub fn human(n: u64) -> String {
    const UNITS: [&str; 5] = ["bytes", "KB", "MB", "GB", "TB"];
    if n < 1000 {
        return format!("{} bytes", n);
    }
    let mut v = n;
    let mut u = 0;
    let mut tenths = 0;
    while v >= 1000 && u < UNITS.len() - 1 {
        tenths = (v % 1000) / 100;
        v /= 1000;
        u += 1;
    }
    format!("{}.{} {}", v, tenths, UNITS[u])
}

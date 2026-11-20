//! Internal disks (NVMe and SATA). Unlike USB drives these can be written, but Solon
//! only ever writes inside its own library partition (see `library.rs`) and to the
//! partition table when the user confirms creating that partition.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::ahci::{self, AhciPort};
use crate::fs::{BlockDev, FsError};
use crate::nvme::Nvme;
use crate::pci;

pub enum Driver {
    Nvme(Nvme),
    Ahci(AhciPort),
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
        };
        r.map_err(|_| FsError::Io)
    }
}

/// Find all internal disks.
pub fn scan() -> Vec<InternalDisk> {
    let mut out = Vec::new();
    for d in pci::scan() {
        match (d.class, d.subclass, d.prog_if) {
            (0x01, 0x08, 0x02) => match Nvme::init(&d) {
                Some(n) => {
                    let name = if n.model.is_empty() { String::from("NVMe SSD") } else { n.model.clone() };
                    out.push(InternalDisk { name, kind: "NVMe", block_size: n.block_size, blocks: n.blocks, driver: Driver::Nvme(n) });
                }
                None => kprintln!("disk: NVMe controller {:04x}:{:04x} failed to start", d.vendor, d.device),
            },
            (0x01, 0x06, 0x01) => {
                for p in ahci::init(&d) {
                    let name = if p.model.is_empty() { String::from("SATA disk") } else { p.model.clone() };
                    out.push(InternalDisk { name, kind: "SATA", block_size: p.block_size, blocks: p.blocks, driver: Driver::Ahci(p) });
                }
            }
            _ => {}
        }
    }
    for d in &out {
        kprintln!("disk: {} {} {} ({}-byte blocks)", d.kind, d.name, human(d.size_bytes()), d.block_size);
    }
    out
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

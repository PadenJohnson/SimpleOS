//! PCI bus scan through the legacy configuration ports (0xCF8 / 0xCFC).

use alloc::vec::Vec;
use x86_64::instructions::port::Port;

#[derive(Clone, Copy, Debug)]
pub struct Device {
    pub bus: u8,
    pub dev: u8,
    pub func: u8,
    pub vendor: u16,
    pub device: u16,
    pub class: u8,
    pub subclass: u8,
    pub prog_if: u8,
}

fn addr(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    0x8000_0000 | (bus as u32) << 16 | (dev as u32) << 11 | (func as u32) << 8 | (off as u32 & 0xFC)
}

pub fn read32(bus: u8, dev: u8, func: u8, off: u8) -> u32 {
    unsafe {
        Port::<u32>::new(0xCF8).write(addr(bus, dev, func, off));
        Port::<u32>::new(0xCFC).read()
    }
}

pub fn write32(bus: u8, dev: u8, func: u8, off: u8, v: u32) {
    unsafe {
        Port::<u32>::new(0xCF8).write(addr(bus, dev, func, off));
        Port::<u32>::new(0xCFC).write(v);
    }
}

/// One byte of configuration space (registers that must be changed a byte at a time).
pub fn read8(bus: u8, dev: u8, func: u8, off: u8) -> u8 {
    unsafe {
        Port::<u32>::new(0xCF8).write(addr(bus, dev, func, off));
        Port::<u8>::new(0xCFC + (off as u16 & 3)).read()
    }
}

pub fn write8(bus: u8, dev: u8, func: u8, off: u8, v: u8) {
    unsafe {
        Port::<u32>::new(0xCF8).write(addr(bus, dev, func, off));
        Port::<u8>::new(0xCFC + (off as u16 & 3)).write(v);
    }
}

impl Device {
    pub fn read(&self, off: u8) -> u32 {
        read32(self.bus, self.dev, self.func, off)
    }
    pub fn write(&self, off: u8, v: u32) {
        write32(self.bus, self.dev, self.func, off, v)
    }
    pub fn read8(&self, off: u8) -> u8 {
        read8(self.bus, self.dev, self.func, off)
    }
    /// Change the bits of byte `off` in `mask` to `v`.
    pub fn mask8(&self, off: u8, mask: u8, v: u8) {
        let old = self.read8(off);
        write8(self.bus, self.dev, self.func, off, (old & !mask) | (v & mask));
    }

    /// Physical address and size of a memory BAR (handles 64-bit BARs).
    pub fn bar(&self, n: u8) -> Option<(u64, u64)> {
        let off = 0x10 + n * 4;
        let lo = self.read(off);
        if lo & 1 != 0 {
            return None; // I/O space BAR
        }
        let is64 = (lo >> 1) & 3 == 2;
        // Size: write all ones, read back, restore. Decoding is disabled meanwhile.
        let cmd = self.read(0x04);
        self.write(0x04, cmd & !0b11);
        self.write(off, 0xFFFF_FFFF);
        let size_lo = self.read(off) & !0xF;
        self.write(off, lo);
        let (hi, size_hi) = if is64 {
            let hi = self.read(off + 4);
            self.write(off + 4, 0xFFFF_FFFF);
            let s = self.read(off + 4);
            self.write(off + 4, hi);
            (hi, s)
        } else {
            (0, 0xFFFF_FFFF)
        };
        self.write(0x04, cmd);
        let base = (hi as u64) << 32 | (lo & !0xF) as u64;
        let mask = (size_hi as u64) << 32 | size_lo as u64;
        let size = (!mask).wrapping_add(1) & if is64 { u64::MAX } else { 0xFFFF_FFFF };
        Some((base, size))
    }

    /// Enable memory decoding and bus mastering (DMA); disable legacy INTx (we poll).
    pub fn enable(&self) {
        let cmd = self.read(0x04) & 0xFFFF;
        self.write(0x04, cmd | 0b110 | 1 << 10);
    }
}

fn probe(bus: u8, dev: u8, func: u8) -> Option<Device> {
    let id = read32(bus, dev, func, 0);
    if id & 0xFFFF == 0xFFFF {
        return None;
    }
    let class = read32(bus, dev, func, 0x08);
    Some(Device {
        bus,
        dev,
        func,
        vendor: id as u16,
        device: (id >> 16) as u16,
        class: (class >> 24) as u8,
        subclass: (class >> 16) as u8,
        prog_if: (class >> 8) as u8,
    })
}

pub fn scan() -> Vec<Device> {
    let mut found = Vec::new();
    for bus in 0..=255u8 {
        for dev in 0..32u8 {
            let Some(d) = probe(bus, dev, 0) else { continue };
            let multi = (read32(bus, dev, 0, 0x0C) >> 16) & 0x80 != 0;
            found.push(d);
            if multi {
                for func in 1..8u8 {
                    if let Some(d) = probe(bus, dev, func) {
                        found.push(d);
                    }
                }
            }
        }
    }
    found
}

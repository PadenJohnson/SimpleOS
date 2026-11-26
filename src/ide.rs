//! IDE / parallel ATA disks (PCI class 01:01). Most laptops and many desktops from before
//! about 2012 can run their SATA controller this way ("ATA", "IDE", "Legacy" or "Compatible"
//! in the firmware's settings), and some ship set to it. Each controller has two channels
//! (primary and secondary) with up to two disks on each (master and slave).
//!
//! Data moves by bus-master DMA when the controller has it (the disk copies straight into
//! memory, as fast as the link goes), otherwise a word at a time through the data port (PIO).
//! Everything is polled. CD/DVD drives (ATAPI) are skipped.

use alloc::string::String;
use alloc::vec::Vec;
use core::arch::asm;
use core::sync::atomic::Ordering;

use crate::interrupts::{HZ, TICKS};
use crate::memory::Dma;
use crate::pci;

const ATA_IDENTIFY: u8 = 0xEC;
const ATA_READ: u8 = 0x20;
const ATA_READ_EXT: u8 = 0x24;
const ATA_WRITE: u8 = 0x30;
const ATA_WRITE_EXT: u8 = 0x34;
const ATA_READ_DMA: u8 = 0xC8;
const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA: u8 = 0xCA;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_FLUSH: u8 = 0xE7;
const ATA_FLUSH_EXT: u8 = 0xEA;

// Status register bits.
const BSY: u8 = 0x80;
const DF: u8 = 0x20;
const DRQ: u8 = 0x08;
const ERR: u8 = 0x01;

/// What one DMA transfer moves at most: one 64 KiB buffer, one table entry.
const DMA_MAX: usize = 64 * 1024;

fn inb(port: u16) -> u8 {
    let v: u8;
    unsafe { asm!("in al, dx", out("al") v, in("dx") port, options(nomem, nostack, preserves_flags)) };
    v
}
fn outb(port: u16, v: u8) {
    unsafe { asm!("out dx, al", in("dx") port, in("al") v, options(nomem, nostack, preserves_flags)) };
}
fn outl(port: u16, v: u32) {
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") v, options(nomem, nostack, preserves_flags)) };
}
/// Read `buf.len()` 16-bit words from the data port.
fn insw(port: u16, buf: &mut [u16]) {
    unsafe {
        asm!("rep insw", in("dx") port, inout("rdi") buf.as_mut_ptr() => _, inout("rcx") buf.len() => _,
             options(nostack, preserves_flags));
    }
}
fn outsw(port: u16, buf: &[u16]) {
    unsafe {
        asm!("rep outsw", in("dx") port, inout("rsi") buf.as_ptr() => _, inout("rcx") buf.len() => _,
             options(nostack, preserves_flags, readonly));
    }
}

fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Spin until `f` is true, for at most `ms` milliseconds (at least one timer tick).
fn wait(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = ticks() + ms * HZ / 1000 + 1;
    while !f() {
        if ticks() > end {
            return f();
        }
        core::hint::spin_loop();
    }
    true
}

/// One channel's registers.
#[derive(Clone, Copy)]
struct Channel {
    /// Command block: data, error/features, count, LBA x3, device, status/command.
    cmd: u16,
    /// Alternate status (read) / device control (write).
    ctl: u16,
    /// Bus-master registers for this channel (command, status, table address), if any.
    bm: Option<u16>,
}

impl Channel {
    fn status(&self) -> u8 {
        inb(self.cmd + 7)
    }
    fn alt(&self) -> u8 {
        inb(self.ctl)
    }
    /// The 400 ns a device needs after being selected before its status means anything.
    fn settle(&self) {
        for _ in 0..5 {
            self.alt();
        }
    }
    fn not_busy(&self, ms: u64) -> bool {
        wait(ms, || self.alt() & BSY == 0)
    }
}

pub struct IdeDisk {
    ch: Channel,
    slave: bool,
    lba48: bool,
    /// Sectors per PIO data block (the disk's logical sector size in words).
    words: usize,
    /// Bus-master DMA: the one-entry table and the 64 KiB buffer, both below 4 GiB.
    dma: Option<(Dma, Dma)>,
    pub blocks: u64,
    pub block_size: usize,
    pub model: String,
    pub how: &'static str,
}

/// Find every hard disk or SSD on an IDE controller.
pub fn init(dev: &pci::Device) -> Vec<IdeDisk> {
    let mut out = Vec::new();
    // Turn on I/O decoding and bus mastering (we poll, so legacy interrupts stay off).
    let cmd = dev.read(0x04) & 0xFFFF;
    dev.write(0x04, cmd | 0b101 | 1 << 10);
    let io_bar = |n: u8| -> Option<u16> {
        let v = dev.read(0x10 + n * 4);
        (v & 1 == 1 && v & 0xFFFC != 0).then_some((v & 0xFFFC) as u16)
    };
    let bm = if dev.prog_if & 0x80 != 0 { io_bar(4) } else { None };
    let pi = dev.prog_if;
    // Programming interface bit 0 / 2: that channel uses the BARs ("native" mode), otherwise
    // the fixed ports every PC has had since the 1980s.
    let primary = if pi & 1 != 0 {
        io_bar(0).zip(io_bar(1)).map(|(c, k)| Channel { cmd: c, ctl: k + 2, bm })
    } else {
        Some(Channel { cmd: 0x1F0, ctl: 0x3F6, bm })
    };
    let secondary = if pi & 4 != 0 {
        io_bar(2).zip(io_bar(3)).map(|(c, k)| Channel { cmd: c, ctl: k + 2, bm: bm.map(|b| b + 8) })
    } else {
        Some(Channel { cmd: 0x170, ctl: 0x376, bm: bm.map(|b| b + 8) })
    };
    kprintln!(
        "ide: {:04x}:{:04x} at {:02x}:{:02x}.{}: {} mode, bus master {}",
        dev.vendor, dev.device, dev.bus, dev.dev, dev.func,
        if pi & 5 == 5 { "native" } else if pi & 5 == 0 { "compatibility" } else { "mixed" },
        if bm.is_some() { "yes" } else { "no" }
    );
    for (name, ch) in [("primary", primary), ("secondary", secondary)] {
        let Some(ch) = ch else {
            kprintln!("ide:   {} channel: no ports assigned", name);
            continue;
        };
        if !reset(&ch) {
            continue;
        }
        for slave in [false, true] {
            if let Some(d) = IdeDisk::probe(ch, slave) {
                out.push(d);
            }
        }
    }
    out
}

/// Reset both devices on a channel. False if nothing is there.
fn reset(ch: &Channel) -> bool {
    // A channel with nothing attached floats: every bit reads 1.
    if ch.alt() == 0xFF {
        return false;
    }
    outb(ch.ctl, 0x06); // reset, interrupts off
    wait(1, || false);
    outb(ch.ctl, 0x02); // interrupts stay off
    wait(3, || false);
    // A spinning disk may take a while to come back; an SSD is instant.
    if !ch.not_busy(3000) {
        kprintln!("ide:   channel {:#x} stays busy after reset (status {:#x})", ch.cmd, ch.alt());
        return false;
    }
    true
}

impl IdeDisk {
    fn probe(ch: Channel, slave: bool) -> Option<IdeDisk> {
        let who = if slave { "slave" } else { "master" };
        outb(ch.cmd + 6, 0xA0 | (slave as u8) << 4);
        ch.settle();
        if ch.alt() == 0xFF || !ch.not_busy(1000) {
            return None;
        }
        // After a reset each device leaves a signature in the LBA registers.
        let sig = (inb(ch.cmd + 4), inb(ch.cmd + 5));
        if matches!(sig, (0x14, 0xEB) | (0x69, 0x96)) {
            kprintln!("ide:   channel {:#x} {}: CD/DVD drive (skipped)", ch.cmd, who);
            return None;
        }
        for r in 2..6 {
            outb(ch.cmd + r, 0);
        }
        outb(ch.cmd + 7, ATA_IDENTIFY);
        ch.settle();
        if ch.status() == 0 {
            return None; // nobody home
        }
        if !ch.not_busy(2000) {
            kprintln!("ide:   channel {:#x} {}: no answer to IDENTIFY", ch.cmd, who);
            return None;
        }
        // A packet device (CD/DVD) aborts IDENTIFY and shows its signature instead.
        if inb(ch.cmd + 4) != 0 || inb(ch.cmd + 5) != 0 {
            return None;
        }
        if !wait(1000, || ch.alt() & (DRQ | ERR) != 0) || ch.alt() & ERR != 0 {
            return None;
        }
        let mut id = [0u16; 256];
        insw(ch.cmd, &mut id);
        ch.status();
        let model: String = id[27..47].iter().flat_map(|w| [(w >> 8) as u8, *w as u8]).map(|c| c as char).collect();
        let model = String::from(model.trim());
        let lba48 = id[83] & (1 << 10) != 0 && id[86] & (1 << 10) != 0;
        let blocks = if lba48 {
            id[100] as u64 | (id[101] as u64) << 16 | (id[102] as u64) << 32 | (id[103] as u64) << 48
        } else {
            id[60] as u64 | (id[61] as u64) << 16
        };
        let mut block_size = 512;
        if id[106] & 0xC000 == 0x4000 && id[106] & (1 << 12) != 0 {
            block_size = (id[117] as usize | (id[118] as usize) << 16) * 2;
        }
        if id[49] & (1 << 9) == 0 || blocks == 0 || !(512..=4096).contains(&block_size) {
            kprintln!("ide:   channel {:#x} {}: {} can't be used (no LBA addressing)", ch.cmd, who, model);
            return None;
        }
        // Multiword or Ultra DMA modes the disk has (and the firmware chose; we leave timing alone).
        let disk_dma = id[49] & (1 << 8) != 0 && (id[63] & 0x0700 != 0 || id[88] & 0x7F00 != 0);
        let mut d = IdeDisk {
            ch,
            slave,
            lba48,
            words: block_size / 2,
            dma: None,
            blocks,
            block_size,
            model,
            how: "PIO",
        };
        if disk_dma {
            if let (Some(bm), Some(table), Some(buf)) = (ch.bm, Dma::below_4g(8, 64), Dma::below_4g(DMA_MAX, DMA_MAX)) {
                d.dma = Some((table, buf));
                // Use it only if it reads the first block just as PIO does.
                let mut a = alloc::vec![0u8; block_size];
                let mut b = alloc::vec![0u8; block_size];
                let ok = d.pio(false, 0, 1, &mut a).is_ok() && d.dma_read(0, &mut b).is_ok() && a == b;
                if ok {
                    d.how = "DMA";
                } else {
                    kprintln!("ide:   bus master {:#x} didn't work for {}; using PIO", bm, d.model);
                    d.dma = None;
                    reset(&ch);
                }
            }
        }
        kprintln!(
            "ide:   channel {:#x} {}: {}, {} blocks, {}-bit addresses, {}",
            ch.cmd, who, d.model, blocks, if lba48 { 48 } else { 28 }, d.how
        );
        Some(d)
    }

    /// Select the disk and load the address and count registers.
    fn setup(&self, lba: u64, count: usize) -> Result<(), u32> {
        let (cmd, ch) = (self.ch.cmd, &self.ch);
        if !ch.not_busy(5000) {
            return Err(0x100 | ch.alt() as u32);
        }
        if self.lba48 {
            outb(cmd + 6, 0x40 | (self.slave as u8) << 4);
            ch.settle();
            outb(cmd + 2, (count >> 8) as u8);
            outb(cmd + 3, (lba >> 24) as u8);
            outb(cmd + 4, (lba >> 32) as u8);
            outb(cmd + 5, (lba >> 40) as u8);
        } else {
            outb(cmd + 6, 0xE0 | (self.slave as u8) << 4 | ((lba >> 24) & 0xF) as u8);
            ch.settle();
        }
        outb(cmd + 2, count as u8);
        outb(cmd + 3, lba as u8);
        outb(cmd + 4, (lba >> 8) as u8);
        outb(cmd + 5, (lba >> 16) as u8);
        Ok(())
    }

    /// Most blocks one command moves: 65536 (48-bit) or 256 (28-bit); a count of 0 means that.
    fn per_command(&self) -> usize {
        if self.lba48 { 65536 } else { 256 }
    }

    /// Move `count` blocks through the data port. `buf` holds exactly that many.
    fn pio(&mut self, write: bool, lba: u64, count: usize, buf: &mut [u8]) -> Result<(), u32> {
        let ch = self.ch;
        self.setup(lba, count & (self.per_command() - 1))?;
        let op = match (write, self.lba48) {
            (false, true) => ATA_READ_EXT,
            (false, false) => ATA_READ,
            (true, true) => ATA_WRITE_EXT,
            (true, false) => ATA_WRITE,
        };
        outb(ch.cmd + 7, op);
        let mut words = alloc::vec![0u16; self.words];
        for chunk in buf.chunks_exact_mut(self.block_size) {
            ch.settle();
            if !wait(5000, || ch.alt() & (BSY | DRQ) == DRQ || ch.alt() & (ERR | DF) != 0) {
                return Err(0x200 | ch.alt() as u32);
            }
            let st = ch.alt();
            if st & (ERR | DF) != 0 {
                return Err(0x300 | inb(ch.cmd + 1) as u32);
            }
            if write {
                for (w, c) in words.iter_mut().zip(chunk.chunks_exact(2)) {
                    *w = u16::from_le_bytes([c[0], c[1]]);
                }
                outsw(ch.cmd, &words);
            } else {
                insw(ch.cmd, &mut words);
                for (w, c) in words.iter().zip(chunk.chunks_exact_mut(2)) {
                    c.copy_from_slice(&w.to_le_bytes());
                }
            }
        }
        ch.settle();
        if !ch.not_busy(5000) {
            return Err(0x400 | ch.alt() as u32);
        }
        let st = ch.status();
        if st & (ERR | DF) != 0 {
            return Err(0x500 | inb(ch.cmd + 1) as u32);
        }
        Ok(())
    }

    /// One bus-master transfer of up to 64 KiB through the DMA buffer.
    fn dma_xfer(&mut self, write: bool, lba: u64, count: usize) -> Result<(), u32> {
        let ch = self.ch;
        let (Some(bm), Some((table, buf))) = (ch.bm, self.dma.as_ref()) else { return Err(0x600) };
        let bytes = count * self.block_size;
        unsafe {
            let t = table.ptr::<u32>();
            core::ptr::write_volatile(t, buf.phys() as u32);
            // Byte count (0 means 64 KiB) and "last entry".
            core::ptr::write_volatile(t.add(1), (bytes as u32 & 0xFFFF) | 1 << 31);
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        outb(bm, 0); // stopped
        outl(bm + 4, table.phys() as u32);
        outb(bm + 2, inb(bm + 2) | 0x06); // clear error and interrupt
        // Direction bit: 1 = the controller writes memory (a disk read).
        let dir = if write { 0 } else { 0x08 };
        outb(bm, dir);
        self.setup(lba, count & (self.per_command() - 1))?;
        let op = match (write, self.lba48) {
            (false, true) => ATA_READ_DMA_EXT,
            (false, false) => ATA_READ_DMA,
            (true, true) => ATA_WRITE_DMA_EXT,
            (true, false) => ATA_WRITE_DMA,
        };
        outb(ch.cmd + 7, op);
        outb(bm, dir | 1); // go
        ch.settle();
        // Done when the controller has moved everything (active clears) and the disk isn't
        // busy, or the disk reports an error.
        let done = wait(5000, || {
            let st = ch.alt();
            st & BSY == 0 && (inb(bm + 2) & 1 == 0 || st & (ERR | DF) != 0)
        });
        outb(bm, dir); // stop
        let bms = inb(bm + 2);
        outb(bm + 2, bms | 0x06);
        let st = ch.status();
        if !done || st & (ERR | DF) != 0 || bms & 0x02 != 0 {
            return Err(0x700 | (bms as u32) << 16 | st as u32);
        }
        core::sync::atomic::fence(Ordering::SeqCst);
        Ok(())
    }

    fn dma_read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), u32> {
        let count = out.len() / self.block_size;
        self.dma_xfer(false, lba, count)?;
        let (_, buf) = self.dma.as_ref().unwrap();
        out.copy_from_slice(&buf.bytes()[..out.len()]);
        Ok(())
    }

    pub fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), u32> {
        let per = if self.dma.is_some() { DMA_MAX / self.block_size } else { self.per_command().min(128) };
        let mut done = 0;
        for chunk in out.chunks_mut(per * self.block_size) {
            let n = chunk.len() / self.block_size;
            if self.dma.is_some() {
                self.dma_read(lba + done, chunk)?;
            } else {
                self.pio(false, lba + done, n, chunk)?;
            }
            done += n as u64;
        }
        Ok(())
    }

    pub fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), u32> {
        let per = if self.dma.is_some() { DMA_MAX / self.block_size } else { self.per_command().min(128) };
        let mut done = 0;
        for chunk in data.chunks(per * self.block_size) {
            let n = chunk.len() / self.block_size;
            if let Some((_, buf)) = self.dma.as_mut() {
                buf.bytes_mut()[..chunk.len()].copy_from_slice(chunk);
                self.dma_xfer(true, lba + done, n)?;
            } else {
                let mut tmp = chunk.to_vec();
                self.pio(true, lba + done, n, &mut tmp)?;
            }
            done += n as u64;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), u32> {
        let ch = self.ch;
        self.setup(0, 0)?;
        outb(ch.cmd + 7, if self.lba48 { ATA_FLUSH_EXT } else { ATA_FLUSH });
        ch.settle();
        if !ch.not_busy(30_000) || ch.status() & (ERR | DF) != 0 {
            return Err(0x800 | ch.alt() as u32);
        }
        Ok(())
    }
}

//! AHCI (SATA) driver, polled: hard drives and SATA SSDs. CD/DVD drives are skipped.

use alloc::string::String;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::interrupts::{HZ, TICKS};
use crate::memory::{map_mmio, Dma};
use crate::pci;

pub const MAX_XFER: usize = 64 * 1024;

const ATA_IDENTIFY: u8 = 0xEC;
const ATA_READ_DMA_EXT: u8 = 0x25;
const ATA_WRITE_DMA_EXT: u8 = 0x35;
const ATA_FLUSH_EXT: u8 = 0xEA;

pub struct AhciPort {
    port: u64, // virtual address of this port's registers
    cmd_list: Dma,
    _fis: Dma,
    table: Dma,
    buf: Dma,
    pub blocks: u64,
    pub block_size: usize,
    pub model: String,
}

#[inline]
fn r32(a: u64) -> u32 {
    unsafe { read_volatile(a as *const u32) }
}
#[inline]
fn w32(a: u64, v: u32) {
    unsafe { write_volatile(a as *mut u32, v) }
}

fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

fn wait(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = ticks() + ms * HZ / 1000 + 1;
    while !f() {
        if ticks() > end {
            return false;
        }
        core::hint::spin_loop();
    }
    true
}

/// Find every SATA disk on an AHCI controller.
pub fn init(dev: &pci::Device) -> Vec<AhciPort> {
    let mut out = Vec::new();
    let Some((bar, size)) = dev.bar(5) else { return out };
    dev.enable();
    let hba = map_mmio(bar, size.max(0x1100));
    // BIOS/OS handoff if supported.
    if r32(hba + 0x24) & 1 != 0 {
        w32(hba + 0x28, r32(hba + 0x28) | 2);
        wait(100, || r32(hba + 0x28) & 1 == 0);
    }
    w32(hba + 0x04, r32(hba + 0x04) | 1 << 31); // AHCI mode
    let pi = r32(hba + 0x0C);
    for p in 0..32u64 {
        if pi & (1 << p) == 0 {
            continue;
        }
        let port = hba + 0x100 + p * 0x80;
        let ssts = r32(port + 0x28);
        if ssts & 0xF != 3 {
            continue; // nothing attached
        }
        let sig = r32(port + 0x24);
        if sig != 0x0000_0101 {
            continue; // not a plain SATA disk (e.g. ATAPI CD drive)
        }
        if let Some(d) = AhciPort::new(port) {
            out.push(d);
        }
    }
    out
}

impl AhciPort {
    fn new(port: u64) -> Option<AhciPort> {
        // Stop the command engine before changing its memory.
        w32(port + 0x18, r32(port + 0x18) & !1);
        wait(500, || r32(port + 0x18) & (1 << 15) == 0);
        w32(port + 0x18, r32(port + 0x18) & !(1 << 4));
        wait(500, || r32(port + 0x18) & (1 << 14) == 0);

        let cmd_list = Dma::new(1024, 1024);
        let fis = Dma::new(256, 256);
        let table = Dma::new(256, 128);
        let p = cmd_list.phys();
        w32(port + 0x00, p as u32);
        w32(port + 0x04, (p >> 32) as u32);
        let f = fis.phys();
        w32(port + 0x08, f as u32);
        w32(port + 0x0C, (f >> 32) as u32);
        w32(port + 0x30, 0xFFFF_FFFF); // clear errors
        w32(port + 0x10, 0xFFFF_FFFF); // clear interrupt status
        w32(port + 0x14, 0); // no interrupts
        wait(500, || r32(port + 0x20) & 0x88 == 0);
        w32(port + 0x18, r32(port + 0x18) | 1 << 4); // FIS receive
        w32(port + 0x18, r32(port + 0x18) | 1); // start

        let mut d = AhciPort {
            port,
            cmd_list,
            _fis: fis,
            table,
            buf: Dma::new(MAX_XFER, MAX_XFER),
            blocks: 0,
            block_size: 512,
            model: String::new(),
        };
        if d.command(ATA_IDENTIFY, 0, 0, 512, false).is_err() {
            kprintln!("ahci: IDENTIFY failed");
            return None;
        }
        let id: Vec<u16> = d.buf.bytes()[..512].chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
        // Model: words 27..46, two characters per word, high byte first.
        let model: String = id[27..47].iter().flat_map(|w| [(w >> 8) as u8, *w as u8]).map(|c| c as char).collect();
        d.model = String::from(model.trim());
        if id[83] & (1 << 10) == 0 {
            kprintln!("ahci: {} has no 48-bit addressing; skipped", d.model);
            return None;
        }
        d.blocks = id[100] as u64 | (id[101] as u64) << 16 | (id[102] as u64) << 32 | (id[103] as u64) << 48;
        if id[106] & 0xC000 == 0x4000 && id[106] & (1 << 12) != 0 {
            let words = id[117] as usize | (id[118] as usize) << 16;
            d.block_size = words * 2;
        }
        if !(512..=4096).contains(&d.block_size) || d.blocks == 0 {
            return None;
        }
        Some(d)
    }

    /// One ATA command through slot 0. Data goes through our DMA buffer.
    fn command(&mut self, cmd: u8, lba: u64, count: u16, bytes: usize, write: bool) -> Result<(), u32> {
        let port = self.port;
        if !wait(1000, || r32(port + 0x20) & 0x88 == 0) {
            return Err(r32(port + 0x20));
        }
        unsafe {
            let t = self.table.ptr::<u8>();
            core::ptr::write_bytes(t, 0, 0x90);
            // Register H2D FIS
            *t = 0x27;
            *t.add(1) = 0x80;
            *t.add(2) = cmd;
            *t.add(4) = lba as u8;
            *t.add(5) = (lba >> 8) as u8;
            *t.add(6) = (lba >> 16) as u8;
            *t.add(7) = 0x40; // LBA mode
            *t.add(8) = (lba >> 24) as u8;
            *t.add(9) = (lba >> 32) as u8;
            *t.add(10) = (lba >> 40) as u8;
            *t.add(12) = count as u8;
            *t.add(13) = (count >> 8) as u8;
            // One PRD entry
            let prd = t.add(0x80) as *mut u32;
            let b = self.buf.phys();
            write_volatile(prd, b as u32);
            write_volatile(prd.add(1), (b >> 32) as u32);
            write_volatile(prd.add(3), (bytes.max(2) as u32 - 1) & 0x3F_FFFF);
            // Command header 0
            let h = self.cmd_list.ptr::<u32>();
            let prdtl = if bytes > 0 { 1u32 } else { 0 };
            write_volatile(h, 5 | (write as u32) << 6 | prdtl << 16);
            write_volatile(h.add(1), 0);
            let tp = self.table.phys();
            write_volatile(h.add(2), tp as u32);
            write_volatile(h.add(3), (tp >> 32) as u32);
        }
        fence(Ordering::SeqCst);
        w32(port + 0x10, 0xFFFF_FFFF);
        w32(port + 0x38, 1);
        let ok = wait(5000, || r32(port + 0x38) & 1 == 0 || r32(port + 0x10) & (1 << 30) != 0);
        if !ok || r32(port + 0x10) & (1 << 30) != 0 || r32(port + 0x20) & 1 != 0 {
            let tfd = r32(port + 0x20);
            // Recover: restart the engine.
            w32(port + 0x18, r32(port + 0x18) & !1);
            wait(500, || r32(port + 0x18) & (1 << 15) == 0);
            w32(port + 0x30, 0xFFFF_FFFF);
            w32(port + 0x10, 0xFFFF_FFFF);
            w32(port + 0x18, r32(port + 0x18) | 1);
            return Err(tfd);
        }
        Ok(())
    }

    pub fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), u32> {
        let per = MAX_XFER / self.block_size;
        let total = out.len() / self.block_size;
        let mut done = 0;
        while done < total {
            let n = (total - done).min(per);
            let bytes = n * self.block_size;
            self.command(ATA_READ_DMA_EXT, lba + done as u64, n as u16, bytes, false)?;
            out[done * self.block_size..done * self.block_size + bytes].copy_from_slice(&self.buf.bytes()[..bytes]);
            done += n;
        }
        Ok(())
    }

    pub fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), u32> {
        let per = MAX_XFER / self.block_size;
        let total = data.len() / self.block_size;
        let mut done = 0;
        while done < total {
            let n = (total - done).min(per);
            let bytes = n * self.block_size;
            self.buf.bytes_mut()[..bytes].copy_from_slice(&data[done * self.block_size..done * self.block_size + bytes]);
            self.command(ATA_WRITE_DMA_EXT, lba + done as u64, n as u16, bytes, true)?;
            done += n;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), u32> {
        self.command(ATA_FLUSH_EXT, 0, 0, 0, false)
    }
}

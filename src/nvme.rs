//! NVMe SSD driver (polled). One admin queue pair and one I/O queue pair, first namespace.

use alloc::string::String;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::interrupts::{HZ, TICKS};
use crate::memory::{map_mmio, Dma};
use crate::pci;

const QUEUE: usize = 64;
/// Bytes per I/O command (our bounce buffer; 16 pages described by a PRP list).
pub const MAX_XFER: usize = 64 * 1024;

struct Queue {
    sq: Dma,
    cq: Dma,
    sq_tail: usize,
    cq_head: usize,
    phase: u32,
    id: u16,
}

pub struct Nvme {
    regs: u64,
    stride: u64,
    admin: Queue,
    io: Queue,
    cid: u16,
    pub nsid: u32,
    pub block_size: usize,
    pub blocks: u64,
    pub model: String,
    buf: Dma,
    prp_list: Dma,
}

#[inline]
fn r32(a: u64) -> u32 {
    unsafe { read_volatile(a as *const u32) }
}
#[inline]
fn w32(a: u64, v: u32) {
    unsafe { write_volatile(a as *mut u32, v) }
}
fn r64(a: u64) -> u64 {
    r32(a) as u64 | (r32(a + 4) as u64) << 32
}
fn w64(a: u64, v: u64) {
    w32(a, v as u32);
    w32(a + 4, (v >> 32) as u32);
}

fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

fn ascii(b: &[u8]) -> String {
    let s: String = b.iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { ' ' }).collect();
    String::from(s.trim())
}

impl Queue {
    fn new(id: u16) -> Queue {
        Queue { sq: Dma::new(QUEUE * 64, 4096), cq: Dma::new(QUEUE * 16, 4096), sq_tail: 0, cq_head: 0, phase: 1, id }
    }
}

impl Nvme {
    pub fn init(dev: &pci::Device) -> Option<Nvme> {
        let (bar, size) = dev.bar(0)?;
        dev.enable();
        let regs = map_mmio(bar, size.max(0x2000));
        let cap = r64(regs);
        let stride = 4u64 << ((cap >> 32) & 0xF);
        let timeout = ((cap >> 24) & 0xFF).max(1) * HZ / 2 + HZ;

        // Disable, set up admin queues, enable.
        w32(regs + 0x14, r32(regs + 0x14) & !1);
        let end = ticks() + timeout;
        while r32(regs + 0x1C) & 1 != 0 {
            if ticks() > end {
                kprintln!("nvme: controller did not stop");
                return None;
            }
        }
        let admin = Queue::new(0);
        w32(regs + 0x24, ((QUEUE as u32 - 1) << 16) | (QUEUE as u32 - 1));
        w64(regs + 0x28, admin.sq.phys());
        w64(regs + 0x30, admin.cq.phys());
        w32(regs + 0x14, 1 | 6 << 16 | 4 << 20); // EN, 64-byte SQ entries, 16-byte CQ entries, 4 KiB pages
        let end = ticks() + timeout;
        while r32(regs + 0x1C) & 1 == 0 {
            if r32(regs + 0x1C) & 2 != 0 || ticks() > end {
                kprintln!("nvme: controller did not start");
                return None;
            }
        }

        let mut n = Nvme {
            regs,
            stride,
            admin,
            io: Queue::new(1),
            cid: 0,
            nsid: 1,
            block_size: 512,
            blocks: 0,
            model: String::new(),
            buf: Dma::new(MAX_XFER, MAX_XFER),
            prp_list: Dma::new(4096, 4096),
        };
        // PRP list for our fixed 64 KiB buffer: pages 2..16.
        for i in 1..MAX_XFER / 4096 {
            unsafe { n.prp_list.ptr::<u64>().add(i - 1).write_volatile(n.buf.phys() + (i * 4096) as u64) };
        }

        // Identify controller (model name).
        let id = Dma::new(4096, 4096);
        n.admin_cmd([0x06, 0, 0, 0, 0, 0], id.phys(), 1).ok()?;
        n.model = ascii(&id.bytes()[24..64]);
        // First active namespace.
        n.admin_cmd([0x06, 0, 0, 0, 0, 0], id.phys(), 2).ok()?;
        let first = u32::from_le_bytes(id.bytes()[0..4].try_into().unwrap());
        n.nsid = if first == 0 { 1 } else { first };
        n.admin_cmd_ns([0x06, n.nsid, 0, 0, 0, 0], id.phys(), 0).ok()?;
        let b = id.bytes();
        n.blocks = u64::from_le_bytes(b[0..8].try_into().unwrap());
        let fmt = (b[26] & 0xF) as usize;
        let lbads = b[128 + fmt * 4 + 2];
        n.block_size = 1usize << lbads;
        if !(512..=4096).contains(&n.block_size) || n.blocks == 0 {
            kprintln!("nvme: unusable namespace (block size {}, {} blocks)", n.block_size, n.blocks);
            return None;
        }

        // I/O queues: completion first, then submission.
        let cq = n.io.cq.phys();
        n.admin_cmd_raw(0x05, 0, cq, 0, [1 | ((QUEUE as u32 - 1) << 16), 1, 0, 0, 0, 0]).ok()?;
        let sq = n.io.sq.phys();
        n.admin_cmd_raw(0x01, 0, sq, 0, [1 | ((QUEUE as u32 - 1) << 16), 1 | 1 << 16, 0, 0, 0, 0]).ok()?;
        Some(n)
    }

    fn db(&self, qid: u16, completion: bool) -> u64 {
        self.regs + 0x1000 + (2 * qid as u64 + completion as u64) * self.stride
    }

    /// Submit one command on `admin` or the I/O queue and wait for its completion.
    fn submit(&mut self, admin: bool, opcode: u8, nsid: u32, prp1: u64, prp2: u64, cdw: [u32; 6]) -> Result<u32, u16> {
        self.cid = self.cid.wrapping_add(1);
        let cid = self.cid;
        let q = if admin { &mut self.admin } else { &mut self.io };
        let e = unsafe { q.sq.ptr::<u32>().add(q.sq_tail * 16) };
        let mut cmd = [0u32; 16];
        cmd[0] = opcode as u32 | (cid as u32) << 16;
        cmd[1] = nsid;
        cmd[6] = prp1 as u32;
        cmd[7] = (prp1 >> 32) as u32;
        cmd[8] = prp2 as u32;
        cmd[9] = (prp2 >> 32) as u32;
        cmd[10..16].copy_from_slice(&cdw);
        for (i, v) in cmd.iter().enumerate() {
            unsafe { write_volatile(e.add(i), *v) };
        }
        q.sq_tail = (q.sq_tail + 1) % QUEUE;
        let (qid, tail) = (q.id, q.sq_tail);
        fence(Ordering::SeqCst);
        w32(self.db(qid, false), tail as u32);

        let end = ticks() + 5 * HZ;
        loop {
            let q = if admin { &mut self.admin } else { &mut self.io };
            let c = unsafe { q.cq.ptr::<u32>().add(q.cq_head * 4) };
            let dw3 = unsafe { read_volatile(c.add(3)) };
            if (dw3 >> 16) & 1 == q.phase {
                let result = unsafe { read_volatile(c) };
                q.cq_head = (q.cq_head + 1) % QUEUE;
                if q.cq_head == 0 {
                    q.phase ^= 1;
                }
                let (qid, head) = (q.id, q.cq_head);
                w32(self.db(qid, true), head as u32);
                if dw3 as u16 != cid {
                    continue; // stale completion
                }
                let status = ((dw3 >> 17) & 0x7FFF) as u16;
                return if status == 0 { Ok(result) } else { Err(status) };
            }
            if ticks() > end {
                return Err(0xFFFF);
            }
            core::hint::spin_loop();
        }
    }

    fn admin_cmd(&mut self, c: [u32; 6], buf: u64, cns: u32) -> Result<u32, u16> {
        self.submit(true, c[0] as u8, 0, buf, 0, [cns, 0, 0, 0, 0, 0])
    }
    fn admin_cmd_ns(&mut self, c: [u32; 6], buf: u64, cns: u32) -> Result<u32, u16> {
        self.submit(true, c[0] as u8, c[1], buf, 0, [cns, 0, 0, 0, 0, 0])
    }
    fn admin_cmd_raw(&mut self, op: u8, nsid: u32, prp1: u64, prp2: u64, cdw: [u32; 6]) -> Result<u32, u16> {
        self.submit(true, op, nsid, prp1, prp2, cdw)
    }

    fn rw(&mut self, write: bool, lba: u64, blocks: usize) -> Result<(), u16> {
        let bytes = blocks * self.block_size;
        let prp2 = if bytes <= 4096 {
            0
        } else if bytes <= 8192 {
            self.buf.phys() + 4096
        } else {
            self.prp_list.phys()
        };
        let op = if write { 0x01 } else { 0x02 };
        let nsid = self.nsid;
        let prp1 = self.buf.phys();
        self.submit(false, op, nsid, prp1, prp2, [lba as u32, (lba >> 32) as u32, blocks as u32 - 1, 0, 0, 0]).map(|_| ())
    }

    pub fn read(&mut self, lba: u64, out: &mut [u8]) -> Result<(), u16> {
        let per = MAX_XFER / self.block_size;
        let total = out.len() / self.block_size;
        let mut done = 0;
        while done < total {
            let n = (total - done).min(per);
            self.rw(false, lba + done as u64, n)?;
            let bytes = n * self.block_size;
            out[done * self.block_size..done * self.block_size + bytes].copy_from_slice(&self.buf.bytes()[..bytes]);
            done += n;
        }
        Ok(())
    }

    pub fn write(&mut self, lba: u64, data: &[u8]) -> Result<(), u16> {
        let per = MAX_XFER / self.block_size;
        let total = data.len() / self.block_size;
        let mut done = 0;
        while done < total {
            let n = (total - done).min(per);
            let bytes = n * self.block_size;
            self.buf.bytes_mut()[..bytes].copy_from_slice(&data[done * self.block_size..done * self.block_size + bytes]);
            self.rw(true, lba + done as u64, n)?;
            done += n;
        }
        Ok(())
    }

    pub fn flush(&mut self) -> Result<(), u16> {
        let nsid = self.nsid;
        self.submit(false, 0x00, nsid, 0, 0, [0; 6]).map(|_| ())
    }
}

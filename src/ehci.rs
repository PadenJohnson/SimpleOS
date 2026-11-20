//! USB 2 (EHCI) host controllers: polled control and bulk transfers on the asynchronous
//! schedule. Enough for USB drives, printers, phones and hubs on USB 2 ports (on many
//! laptops some ports only reach the USB 2 controller, and older PCs have nothing else).
//!
//! Each endpoint has a queue head (QH) that stays on the schedule; a transfer is a short
//! chain of transfer descriptors (qTDs) hung under it. Interrupt transfers (keyboards,
//! mice) aren't supported here. Full/low-speed devices only work behind a USB 2 hub (as on
//! Intel chipsets, whose ports all sit behind a built-in hub); plugged straight into a root
//! port they belong to a USB 1 companion controller, which Solon doesn't drive.

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::interrupts::{HZ, TICKS};
use crate::memory::{map_mmio, Dma};
use crate::pci;
use crate::xhci::{EpConfig, EpType, Speed, UsbError};

const POOL: usize = 64 * 1024;
const QH_SIZE: usize = 96;
const QTD_SIZE: usize = 64;
const N_QH: usize = 320;
const N_QTD: usize = 160;
const QTD_AREA: usize = N_QH * QH_SIZE; // 30720
const SETUP_AREA: usize = QTD_AREA + N_QTD * QTD_SIZE; // 40960
/// Bytes per qTD (4 whole pages).
const CHUNK: usize = 16 * 1024;
const T: u32 = 1; // terminate bit

// Token bits
const ACTIVE: u32 = 1 << 7;
const HALTED: u32 = 1 << 6;
const ERRORS: u32 = 0x3C; // data buffer, babble, transaction error, missed micro-frame
const IOC: u32 = 1 << 15;
const PID_OUT: u32 = 0;
const PID_IN: u32 = 1;
const PID_SETUP: u32 = 2;

// Port bits
const CCS: u32 = 1 << 0;
const CSC: u32 = 1 << 1;
const PED: u32 = 1 << 2;
const PEDC: u32 = 1 << 3;
const OCC: u32 = 1 << 5;
const PR: u32 = 1 << 8;
const PP: u32 = 1 << 12;
const PO: u32 = 1 << 13;
const W1C: u32 = CSC | PEDC | OCC;

fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

fn wait_until(ms: u64, mut f: impl FnMut() -> bool) -> bool {
    let end = ticks() + ms * HZ / 1000 + 1;
    loop {
        if f() {
            return true;
        }
        if ticks() > end {
            return false;
        }
        core::hint::spin_loop();
    }
}

fn r32(a: u64) -> u32 {
    unsafe { read_volatile(a as *const u32) }
}
fn w32(a: u64, v: u32) {
    unsafe { write_volatile(a as *mut u32, v) }
}

struct Dev {
    speed: Speed,
    /// Endpoint queue heads by xHCI-style index (EP0 = 1, OUT n = 2n, IN n = 2n+1).
    qh: [Option<usize>; 32],
}

pub struct Ehci {
    pub name: alloc::string::String,
    pci: pci::Device,
    op: u64,
    ports: u8,
    ext64: bool,
    pool: Dma,
    qh_used: Vec<bool>,
    devs: Vec<Option<Dev>>,
    reserved: Vec<bool>,
    port_changes: Vec<u8>,
    warned_interrupt: bool,
}

impl Ehci {
    pub fn init(dev: &pci::Device) -> Option<Ehci> {
        let (bar, size) = dev.bar(0)?;
        dev.enable();
        let cap = map_mmio(bar, size.max(0x100));
        let caplen = r32(cap) & 0xFF;
        let hcs = r32(cap + 4);
        let hcc = r32(cap + 8);
        let op = cap + caplen as u64;
        let ports = (hcs & 0xF) as u8;
        let ext64 = hcc & 1 != 0;

        // Take the controller from the firmware (it may be using it through SMM).
        let mut eecp = ((hcc >> 8) & 0xFF) as u8;
        let mut guard = 0;
        while eecp >= 0x40 && guard < 16 {
            guard += 1;
            let v = dev.read(eecp);
            if v & 0xFF == 1 {
                dev.write(eecp, v | 1 << 24);
                if !wait_until(1000, || dev.read(eecp) & (1 << 16) == 0) {
                    kprintln!("ehci: firmware didn't let go of the controller; taking it anyway");
                    dev.write(eecp, dev.read(eecp) & !(1 << 16));
                }
                dev.write(eecp + 4, 0); // no more SMIs
            }
            eecp = ((v >> 8) & 0xFF) as u8;
        }

        // Stop and reset.
        w32(op, r32(op) & !1);
        wait_until(20, || r32(op + 4) & (1 << 12) != 0);
        w32(op, 2);
        if !wait_until(250, || r32(op) & 2 == 0) {
            kprintln!("ehci: reset timed out");
            return None;
        }
        w32(op + 8, 0); // no interrupts: everything is polled

        let pool = Dma::new(POOL, POOL);
        if !ext64 && pool.phys() >> 32 != 0 {
            kprintln!("ehci: controller can't reach memory above 4 GB; not used");
            return None;
        }
        if ext64 {
            w32(op + 0x10, (pool.phys() >> 32) as u32);
        }
        let mut e = Ehci {
            name: alloc::format!("{:04x}:{:04x}", dev.vendor, dev.device),
            pci: *dev,
            op,
            ports,
            ext64,
            pool,
            qh_used: alloc::vec![false; N_QH],
            devs: (0..128).map(|_| None).collect(),
            reserved: alloc::vec![false; 128],
            port_changes: Vec::new(),
            warned_interrupt: false,
        };
        // The head of the asynchronous schedule: a queue head that never runs anything.
        e.qh_used[0] = true;
        let h = e.qh_phys(0);
        e.qw(0, 0, h | 2);
        e.qw(0, 1, 1 << 15 | 2 << 12); // H (head of reclamation list), high speed
        e.qw(0, 4, T);
        e.qw(0, 5, T);
        e.qw(0, 6, HALTED);
        fence(Ordering::SeqCst);
        w32(op + 0x18, h);
        w32(op, 8 << 16 | 1 << 5 | 1); // 1 ms interrupt threshold, async schedule on, run
        if !wait_until(100, || r32(op + 4) & (1 << 15) != 0 && r32(op + 4) & (1 << 12) == 0) {
            kprintln!("ehci: controller did not start");
            return None;
        }
        w32(op + 0x40, 1); // route every port to this controller (not the USB 1 companions)
        if hcs & (1 << 4) != 0 {
            for p in 1..=ports {
                let v = e.portsc(p);
                e.write_portsc(p, v | PP);
            }
        }
        wait_until(40, || false);
        for p in 1..=ports {
            if e.portsc(p) & CCS != 0 {
                e.port_changes.push(p);
            }
        }
        kprintln!("ehci {}: {} ports{}", e.name, ports, if ext64 { "" } else { " (32-bit)" });
        Some(e)
    }

    // ---- pool ----------------------------------------------------------------------

    fn base(&self) -> u64 {
        self.pool.ptr::<u8>() as u64
    }
    fn qh_phys(&self, i: usize) -> u32 {
        (self.pool.phys() + (i * QH_SIZE) as u64) as u32
    }
    fn qtd_phys(&self, i: usize) -> u32 {
        (self.pool.phys() + (QTD_AREA + i * QTD_SIZE) as u64) as u32
    }
    /// Write dword `dw` of queue head `i`.
    fn qw(&self, i: usize, dw: usize, v: u32) {
        w32(self.base() + (i * QH_SIZE + dw * 4) as u64, v)
    }
    fn qr(&self, i: usize, dw: usize) -> u32 {
        r32(self.base() + (i * QH_SIZE + dw * 4) as u64)
    }
    fn tw(&self, i: usize, dw: usize, v: u32) {
        w32(self.base() + (QTD_AREA + i * QTD_SIZE + dw * 4) as u64, v)
    }
    fn tr(&self, i: usize, dw: usize) -> u32 {
        r32(self.base() + (QTD_AREA + i * QTD_SIZE + dw * 4) as u64)
    }

    fn new_qh(&mut self, addr: u8, ep: u8, speed: Speed, mps: u16, control: bool, tt: Option<(u8, u8)>) -> Option<usize> {
        let i = (1..N_QH).find(|&i| !self.qh_used[i])?;
        self.qh_used[i] = true;
        for dw in 0..(QH_SIZE / 4) {
            self.qw(i, dw, 0);
        }
        let eps = match speed {
            Speed::Full => 0,
            Speed::Low => 1,
            _ => 2,
        };
        let c_flag = control && speed != Speed::High;
        self.qw(i, 1, addr as u32 | (ep as u32) << 8 | eps << 12 | (control as u32) << 14 | (mps as u32 & 0x7FF) << 16 | (c_flag as u32) << 27);
        let (hub, port) = tt.unwrap_or((0, 0));
        self.qw(i, 2, 1 << 30 | (hub as u32 & 0x7F) << 16 | (port as u32 & 0x7F) << 23);
        self.qw(i, 4, T);
        self.qw(i, 5, T);
        // Link in after the head.
        let next = self.qr(0, 0);
        self.qw(i, 0, next);
        fence(Ordering::SeqCst);
        self.qw(0, 0, self.qh_phys(i) | 2);
        Some(i)
    }

    /// Take queue heads off the schedule and wait until the controller has let go of them.
    fn unlink(&mut self, qhs: &[usize]) {
        for &q in qhs {
            let target = self.qh_phys(q) | 2;
            let mut p = 0;
            for _ in 0..N_QH {
                let link = self.qr(p, 0);
                if link == target {
                    self.qw(p, 0, self.qr(q, 0));
                    break;
                }
                p = ((link & !0x1F) - self.pool.phys() as u32) as usize / QH_SIZE;
                if p == 0 {
                    break;
                }
            }
        }
        fence(Ordering::SeqCst);
        // Interrupt on async advance doorbell: once it rings, nothing points at them.
        w32(self.op + 4, 1 << 5);
        w32(self.op, r32(self.op) | 1 << 6);
        wait_until(50, || r32(self.op + 4) & (1 << 5) != 0);
        w32(self.op + 4, 1 << 5);
    }

    fn relink(&mut self, q: usize) {
        let next = self.qr(0, 0);
        self.qw(q, 0, next);
        fence(Ordering::SeqCst);
        self.qw(0, 0, self.qh_phys(q) | 2);
    }

    // ---- ports ---------------------------------------------------------------------

    fn portsc_addr(&self, p: u8) -> u64 {
        self.op + 0x44 + 4 * (p as u64 - 1)
    }
    pub fn portsc(&self, p: u8) -> u32 {
        r32(self.portsc_addr(p))
    }
    /// Write a port register without clearing change bits by accident.
    fn write_portsc(&self, p: u8, v: u32) {
        w32(self.portsc_addr(p), v & !W1C);
    }

    pub fn port_connected(&self, p: u8) -> bool {
        let v = self.portsc(p);
        v & CCS != 0 && v & PO == 0
    }

    pub fn take_connect_change(&self, p: u8) -> bool {
        let v = self.portsc(p);
        w32(self.portsc_addr(p), (v & !W1C) | (v & W1C));
        v & CSC != 0
    }

    pub fn take_port_changes(&mut self) -> Vec<u8> {
        let mut v = core::mem::take(&mut self.port_changes);
        v.sort_unstable();
        v.dedup();
        v
    }

    pub fn poll(&mut self) {
        for p in 1..=self.ports {
            if self.portsc(p) & CSC != 0 {
                self.port_changes.push(p);
            }
        }
    }

    /// Reset a connected port. High-speed devices come back enabled; full/low-speed ones
    /// belong to a USB 1 companion controller, so the port is handed over.
    pub fn reset_port(&mut self, p: u8) -> Option<Speed> {
        let v = self.portsc(p);
        if v & CCS == 0 || v & PO != 0 {
            return None;
        }
        if (v >> 10) & 3 == 1 {
            kprintln!("ehci: port {}: low-speed device (needs USB 1 support); skipped", p);
            self.write_portsc(p, v | PO);
            return None;
        }
        self.write_portsc(p, (v & !PED) | PR);
        wait_until(50, || false);
        let v = self.portsc(p);
        self.write_portsc(p, v & !PR);
        wait_until(20, || self.portsc(p) & PR == 0);
        wait_until(5, || false);
        let v = self.portsc(p);
        w32(self.portsc_addr(p), (v & !W1C) | (v & W1C)); // clear changes
        if v & PED == 0 {
            kprintln!("ehci: port {}: full-speed device (needs USB 1 support); skipped", p);
            self.write_portsc(p, v | PO);
            return None;
        }
        wait_until(10, || false); // reset recovery
        Some(Speed::High)
    }

    // ---- devices -------------------------------------------------------------------

    pub fn enable_slot(&mut self) -> Result<u8, UsbError> {
        let s = (1..128).find(|&s| !self.reserved[s] && self.devs[s].is_none()).ok_or(UsbError::Failed(9))?;
        self.reserved[s] = true;
        Ok(s as u8)
    }

    pub fn disable_slot(&mut self, slot: u8) {
        if let Some(d) = self.devs.get_mut(slot as usize).and_then(|d| d.take()) {
            let qhs: Vec<usize> = d.qh.iter().flatten().copied().collect();
            self.unlink(&qhs);
            for q in qhs {
                self.qh_used[q] = false;
            }
        }
        if let Some(r) = self.reserved.get_mut(slot as usize) {
            *r = false;
        }
    }

    pub fn has_slot(&self, slot: u8) -> bool {
        matches!(self.devs.get(slot as usize), Some(Some(_)))
    }

    /// Give the device its address (= its slot number) and set up its control endpoint.
    /// `tt`: (hub slot, hub port, multi-TT) for full/low-speed devices behind a USB 2 hub.
    pub fn address_device(&mut self, slot: u8, _port: u8, _route: u32, speed: Speed, tt: Option<(u8, u8, bool)>) -> Result<(), UsbError> {
        if !(1..128).contains(&slot) {
            return Err(UsbError::NoDevice);
        }
        let tt = tt.map(|(h, p, _)| (h, p));
        let q = self.new_qh(0, 0, speed, speed.default_mps0(), true, tt).ok_or(UsbError::Failed(9))?;
        let mut qh = [None; 32];
        qh[1] = Some(q);
        self.devs[slot as usize] = Some(Dev { speed, qh });
        // SET_ADDRESS through address 0, then talk to the new address.
        self.control(slot, 0x00, 5, slot as u16, 0, None)?;
        wait_until(3, || false);
        let v = self.qr(q, 1);
        self.qw(q, 1, (v & !0x7F) | slot as u32);
        Ok(())
    }

    pub fn set_ep0_mps(&mut self, slot: u8, mps: u16) -> Result<(), UsbError> {
        let q = self.devs.get(slot as usize).and_then(|d| d.as_ref()).and_then(|d| d.qh[1]).ok_or(UsbError::NoDevice)?;
        let v = self.qr(q, 1);
        self.qw(q, 1, (v & !(0x7FF << 16)) | (mps as u32 & 0x7FF) << 16);
        Ok(())
    }

    pub fn configure_endpoints(&mut self, slot: u8, eps: &[EpConfig], _hub: Option<u8>) -> Result<(), UsbError> {
        let (speed, tt) = {
            let d = self.devs.get(slot as usize).and_then(|d| d.as_ref()).ok_or(UsbError::NoDevice)?;
            let q0 = d.qh[1].ok_or(UsbError::NoDevice)?;
            let caps = self.qr(q0, 2);
            let tt = if speed_needs_tt(d.speed) { Some((((caps >> 16) & 0x7F) as u8, ((caps >> 23) & 0x7F) as u8)) } else { None };
            (d.speed, tt)
        };
        for ep in eps {
            if matches!(ep.kind, EpType::InterruptIn) {
                if !self.warned_interrupt {
                    kprintln!("ehci: interrupt endpoints (keyboards, mice) aren't supported on USB 2 ports yet");
                    self.warned_interrupt = true;
                }
                continue;
            }
            let q = self.new_qh(slot, ep.dci / 2, speed, ep.max_packet, false, tt).ok_or(UsbError::Failed(9))?;
            if let Some(d) = self.devs[slot as usize].as_mut() {
                d.qh[ep.dci as usize] = Some(q);
            }
        }
        Ok(())
    }

    /// After a stall: clear the halt and the data toggle on the endpoint's queue head.
    pub fn recover_endpoint(&mut self, slot: u8, dci: u8) -> Result<(), UsbError> {
        let q = self.devs.get(slot as usize).and_then(|d| d.as_ref()).and_then(|d| d.qh[dci as usize]).ok_or(UsbError::NoDevice)?;
        self.qw(q, 4, T);
        self.qw(q, 5, T);
        self.qw(q, 6, 0);
        Ok(())
    }

    // ---- transfers -------------------------------------------------------------------

    /// Fill qTD `i`: next qTD (or terminate), alternate next, token and buffer pages.
    fn set_qtd(&self, i: usize, next: Option<usize>, alt: Option<usize>, token: u32, buf: u64, len: usize) {
        self.tw(i, 0, next.map(|n| self.qtd_phys(n)).unwrap_or(T));
        self.tw(i, 1, alt.map(|n| self.qtd_phys(n)).unwrap_or(T));
        for k in 0..5 {
            let page = if len == 0 { 0 } else if k == 0 { buf } else { (buf & !0xFFF) + 0x1000 * k as u64 };
            self.tw(i, 3 + k, page as u32);
            if self.ext64 {
                self.tw(i, 8 + k, (page >> 32) as u32);
            }
        }
        fence(Ordering::SeqCst);
        self.tw(i, 2, token | (len as u32) << 16 | 3 << 10);
    }

    /// Hang qTDs 0..n under queue head `q` and wait. Returns (bytes moved by qTDs
    /// `count_from..n`, short packet seen).
    fn run(&mut self, q: usize, n: usize, count_from: usize, lens: &[usize], timeout_ms: u64, stop_on_short: bool) -> Result<usize, UsbError> {
        // A halted queue head (left over from an error) won't run anything.
        if self.qr(q, 6) & HALTED != 0 {
            self.qw(q, 6, self.qr(q, 6) & (1 << 31));
        }
        self.qw(q, 5, T);
        fence(Ordering::SeqCst);
        self.qw(q, 4, self.qtd_phys(0));
        let done = wait_until(timeout_ms, || {
            for i in 0..n {
                let t = self.tr(i, 2);
                if t & ACTIVE != 0 {
                    return false;
                }
                if t & HALTED != 0 || (stop_on_short && (t >> 16) & 0x7FFF != 0) {
                    return true; // error, or a short bulk packet: the rest won't run
                }
            }
            true
        });
        if !done {
            // Stop the controller looking at this queue, then tidy it.
            self.unlink(&[q]);
            for i in 0..n {
                self.tw(i, 2, 0);
            }
            self.qw(q, 4, T);
            self.qw(q, 5, T);
            self.qw(q, 6, 0);
            self.relink(q);
            return Err(UsbError::Timeout);
        }
        let mut moved = 0;
        for i in 0..n {
            let t = self.tr(i, 2);
            if t & ACTIVE != 0 {
                break;
            }
            if t & HALTED != 0 {
                return Err(if t & ERRORS != 0 { UsbError::Failed((t & 0xFF) as u8) } else { UsbError::Stall });
            }
            let left = ((t >> 16) & 0x7FFF) as usize;
            if i >= count_from {
                moved += lens[i] - left;
            }
            if left != 0 {
                break;
            }
        }
        Ok(moved)
    }

    /// Control transfer on endpoint 0.
    pub fn control(&mut self, slot: u8, request_type: u8, request: u8, value: u16, index: u16, data: Option<(&Dma, usize)>) -> Result<usize, UsbError> {
        let q = self.devs.get(slot as usize).and_then(|d| d.as_ref()).and_then(|d| d.qh[1]).ok_or(UsbError::NoDevice)?;
        let dir_in = request_type & 0x80 != 0;
        let len = data.map(|(_, l)| l).unwrap_or(0);
        let setup = [request_type, request, value as u8, (value >> 8) as u8, index as u8, (index >> 8) as u8, len as u8, (len >> 8) as u8];
        let sp = self.base() + SETUP_AREA as u64;
        for (k, b) in setup.iter().enumerate() {
            unsafe { write_volatile((sp + k as u64) as *mut u8, *b) };
        }
        let setup_phys = self.pool.phys() + SETUP_AREA as u64;
        let mut lens = alloc::vec![8usize];
        let mut n = 1;
        if let Some((buf, l)) = data {
            lens.push(l);
            n += 1;
            let _ = buf;
        }
        lens.push(0);
        n += 1;
        // Status stage first (filled last-to-first so each qTD's next is ready).
        let status_pid = if len == 0 || !dir_in { PID_IN } else { PID_OUT };
        self.set_qtd(n - 1, None, None, ACTIVE | IOC | status_pid << 8 | 1 << 31, 0, 0);
        if let Some((buf, l)) = data {
            let pid = if dir_in { PID_IN } else { PID_OUT };
            self.set_qtd(1, Some(2), None, ACTIVE | pid << 8 | 1 << 31, buf.phys(), l);
        }
        self.set_qtd(0, Some(1), None, ACTIVE | PID_SETUP << 8, setup_phys, 8);
        match self.run(q, n, 1, &lens, 1000, false) {
            Ok(_) => Ok(len),
            Err(e) => {
                let _ = self.recover_endpoint(slot, 1);
                Err(e)
            }
        }
    }

    /// Bulk transfer and wait. Returns bytes actually transferred.
    pub fn bulk(&mut self, slot: u8, dci: u8, buf: u64, len: usize, timeout_ms: u64) -> Result<usize, UsbError> {
        let q = self.devs.get(slot as usize).and_then(|d| d.as_ref()).and_then(|d| d.qh[dci as usize]).ok_or(UsbError::NoDevice)?;
        let pid = if dci & 1 == 1 { PID_IN } else { PID_OUT };
        let chunks = len.div_ceil(CHUNK).max(1);
        if chunks + 1 > N_QTD {
            return Err(UsbError::Failed(8));
        }
        // A spare, inactive qTD: after a short packet the controller moves there and stops.
        let stop = chunks;
        self.tw(stop, 0, T);
        self.tw(stop, 1, T);
        self.tw(stop, 2, 0);
        let mut lens = Vec::with_capacity(chunks);
        for i in (0..chunks).rev() {
            let off = i * CHUNK;
            let l = (len - off.min(len)).min(CHUNK);
            let next = if i + 1 < chunks { Some(i + 1) } else { None };
            let ioc = if next.is_none() { IOC } else { 0 };
            self.set_qtd(i, next, Some(stop), ACTIVE | ioc | pid << 8, buf + off as u64, l);
        }
        for i in 0..chunks {
            lens.push((len - (i * CHUNK).min(len)).min(CHUNK));
        }
        self.run(q, chunks, 0, &lens, timeout_ms, true)
    }

    /// Interrupt transfers aren't supported on USB 2 ports.
    pub fn queue(&mut self, _slot: u8, _dci: u8, _buf: u64, _len: usize) -> Result<(), UsbError> {
        Err(UsbError::Failed(10))
    }

    pub fn shutdown(&mut self) {
        w32(self.op, r32(self.op) & !1);
        wait_until(20, || r32(self.op + 4) & (1 << 12) != 0);
        let _ = self.pci;
        kprintln!("ehci {}: stopped for power off", self.name);
    }

    pub fn ports(&self) -> u8 {
        self.ports
    }
}

fn speed_needs_tt(s: Speed) -> bool {
    matches!(s, Speed::Low | Speed::Full)
}

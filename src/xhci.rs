//! xHCI (USB 3) host controller driver. Polled: no interrupts, the main loop calls `poll`.
//!
//! Terms: a TRB is a 16-byte work item; rings are circular TRB queues shared with the
//! controller; a "slot" is one attached device; a DCI is an endpoint index (EP0 = 1,
//! EPn OUT = 2n, EPn IN = 2n + 1).

use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{fence, Ordering};

use crate::interrupts::{HZ, TICKS};
use crate::memory::{map_mmio, Dma};
use crate::pci;

// TRB types
const TRB_NORMAL: u32 = 1;
const TRB_SETUP: u32 = 2;
const TRB_DATA: u32 = 3;
const TRB_STATUS: u32 = 4;
const TRB_LINK: u32 = 6;
const TRB_NOOP: u32 = 8;
const TRB_ENABLE_SLOT: u32 = 9;
const TRB_DISABLE_SLOT: u32 = 10;
const TRB_ADDRESS_DEVICE: u32 = 11;
const TRB_CONFIGURE_EP: u32 = 12;
const TRB_EVALUATE_CTX: u32 = 13;
const TRB_RESET_EP: u32 = 14;
const TRB_SET_TR_DEQUEUE: u32 = 16;
const EVT_TRANSFER: u32 = 32;
const EVT_CMD_COMPLETE: u32 = 33;
const EVT_PORT_CHANGE: u32 = 34;

// TRB control bits
const CYCLE: u32 = 1 << 0;
const TOGGLE: u32 = 1 << 1;
const ISP: u32 = 1 << 2;
const IOC: u32 = 1 << 5;
const IDT: u32 = 1 << 6;

// Completion codes
pub const CC_SUCCESS: u8 = 1;
pub const CC_STALL: u8 = 6;
pub const CC_SHORT: u8 = 13;

// PORTSC bits
const PORT_CCS: u32 = 1 << 0;
const PORT_PED: u32 = 1 << 1;
const PORT_PR: u32 = 1 << 4;
const PORT_PP: u32 = 1 << 9;
const PORT_WPR: u32 = 1 << 31;
const PORT_CHANGE_BITS: u32 = 0x00FE_0000; // CSC PEC WRC OCC PRC PLC CEC (write 1 to clear)
const PORT_PRC: u32 = 1 << 21;

const RING_TRBS: usize = 256;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Speed {
    Full,
    Low,
    High,
    Super,
}

impl Speed {
    fn from_portsc(v: u32) -> Speed {
        match (v >> 10) & 0xF {
            2 => Speed::Low,
            3 => Speed::High,
            1 => Speed::Full,
            _ => Speed::Super,
        }
    }
    fn code(self) -> u32 {
        match self {
            Speed::Full => 1,
            Speed::Low => 2,
            Speed::High => 3,
            Speed::Super => 4,
        }
    }
    pub fn default_mps0(self) -> u16 {
        match self {
            Speed::Low | Speed::Full => 8,
            Speed::High => 64,
            Speed::Super => 512,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UsbError {
    Timeout,
    Stall,
    Failed(u8),
    NoDevice,
}

fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

// ---- rings ------------------------------------------------------------------

struct Ring {
    buf: Dma,
    enq: usize,
    cycle: u32,
}

impl Ring {
    fn new() -> Ring {
        let buf = Dma::new(RING_TRBS * 16, 4096);
        let r = Ring { buf, enq: 0, cycle: 1 };
        // Link TRB at the end, pointing back to the start.
        let phys = r.buf.phys();
        r.write_trb(RING_TRBS - 1, [phys as u32, (phys >> 32) as u32, 0, TRB_LINK << 10 | TOGGLE]);
        r
    }

    fn trb_ptr(&self, i: usize) -> *mut u32 {
        unsafe { self.buf.ptr::<u32>().add(i * 4) }
    }

    fn write_trb(&self, i: usize, t: [u32; 4]) {
        let p = self.trb_ptr(i);
        unsafe {
            write_volatile(p, t[0]);
            write_volatile(p.add(1), t[1]);
            write_volatile(p.add(2), t[2]);
            fence(Ordering::SeqCst);
            write_volatile(p.add(3), t[3]); // control (with cycle bit) last
        }
    }

    /// Make sure the next `n` TRBs fit before the link TRB, so a multi-TRB transfer
    /// never wraps in the middle.
    fn reserve(&mut self, n: usize) {
        while self.enq + n > RING_TRBS - 1 {
            self.push([0, 0, 0, TRB_NOOP << 10]);
        }
    }

    /// Queue a TRB (cycle bit is filled in). Returns its physical address.
    fn push(&mut self, t: [u32; 4]) -> u64 {
        let phys = self.buf.phys() + (self.enq * 16) as u64;
        self.write_trb(self.enq, [t[0], t[1], t[2], (t[3] & !CYCLE) | self.cycle]);
        self.enq += 1;
        if self.enq == RING_TRBS - 1 {
            // Hand the link TRB to the controller, then wrap.
            let p = self.buf.phys();
            self.write_trb(RING_TRBS - 1, [p as u32, (p >> 32) as u32, 0, TRB_LINK << 10 | TOGGLE | self.cycle]);
            self.cycle ^= 1;
            self.enq = 0;
        }
        phys
    }

    /// Dequeue pointer for Set TR Dequeue / context init: next enqueue slot + cycle state.
    fn dequeue_ptr(&self) -> u64 {
        self.buf.phys() + (self.enq * 16) as u64 | self.cycle as u64
    }
}

struct EventRing {
    seg: Dma,
    _erst: Dma,
    deq: usize,
    cycle: u32,
}

// ---- devices ------------------------------------------------------------------

struct Slot {
    speed: Speed,
    in_ctx: Dma,
    out_ctx: Dma,
    rings: Vec<Option<Ring>>, // index = DCI (1..=31)
}

#[derive(Clone, Copy, Debug)]
pub enum EpType {
    BulkOut,
    BulkIn,
    InterruptIn,
}

#[derive(Clone, Copy, Debug)]
pub struct EpConfig {
    pub dci: u8,
    pub kind: EpType,
    pub max_packet: u16,
    /// Raw bInterval from the endpoint descriptor (interrupt endpoints).
    pub interval: u8,
    pub max_burst: u8,
}

#[derive(Clone, Copy, Debug)]
pub struct Completion {
    pub slot: u8,
    pub dci: u8,
    pub code: u8,
    /// Bytes NOT transferred.
    pub residual: u32,
}

pub struct Xhci {
    pub name: alloc::string::String,
    op: u64,
    rt: u64,
    db: u64,
    max_slots: u8,
    max_ports: u8,
    csz: usize,
    dcbaa: Dma,
    _scratch: Vec<Dma>,
    cmd: Ring,
    evt: EventRing,
    slots: Vec<Option<Slot>>,
    cmd_result: Option<(u64, u8, u8)>,
    done: Vec<Completion>,
    /// Ports (1-based) whose status changed since the last `take_port_changes`.
    port_changes: Vec<u8>,
}

#[inline]
fn r32(a: u64) -> u32 {
    unsafe { read_volatile(a as *const u32) }
}
#[inline]
fn w32(a: u64, v: u32) {
    unsafe { write_volatile(a as *mut u32, v) }
}
fn w64(a: u64, v: u64) {
    w32(a, v as u32);
    w32(a + 4, (v >> 32) as u32);
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

impl Xhci {
    pub fn init(dev: &pci::Device) -> Option<Xhci> {
        let (bar, size) = dev.bar(0)?;
        dev.enable();
        let cap = map_mmio(bar, size.max(0x1000));
        let caplen = r32(cap) & 0xFF;
        let hcs1 = r32(cap + 0x04);
        let hcs2 = r32(cap + 0x08);
        let hcc1 = r32(cap + 0x10);
        let op = cap + caplen as u64;
        let db = cap + (r32(cap + 0x14) & !0x3) as u64;
        let rt = cap + (r32(cap + 0x18) & !0x1F) as u64;
        let max_slots = (hcs1 & 0xFF).min(64) as u8;
        let max_ports = (hcs1 >> 24) as u8;
        let csz = if hcc1 & (1 << 2) != 0 { 64 } else { 32 };

        Self::bios_handoff(cap, hcc1);

        // Stop, then reset.
        w32(op, r32(op) & !1);
        if !wait_until(100, || r32(op + 4) & 1 != 0) {
            kprintln!("xhci: controller did not halt");
        }
        w32(op, r32(op) | 2);
        if !wait_until(1000, || r32(op) & 2 == 0 && r32(op + 4) & (1 << 11) == 0) {
            kprintln!("xhci: reset timed out");
            return None;
        }

        w32(op + 0x38, max_slots as u32);

        // Device context base address array (+ scratchpad buffers the controller may need).
        let dcbaa = Dma::new((max_slots as usize + 1) * 8, 4096);
        let n_scratch = ((hcs2 >> 21) & 0x1F) << 5 | (hcs2 >> 27);
        let mut scratch = Vec::new();
        if n_scratch > 0 {
            let arr = Dma::new(n_scratch as usize * 8, 4096);
            for i in 0..n_scratch as usize {
                let page = Dma::new(4096, 4096);
                unsafe { arr.ptr::<u64>().add(i).write_volatile(page.phys()) };
                scratch.push(page);
            }
            unsafe { dcbaa.ptr::<u64>().write_volatile(arr.phys()) };
            scratch.push(arr);
        }
        w64(op + 0x30, dcbaa.phys());

        let cmd = Ring::new();
        w64(op + 0x18, cmd.buf.phys() | 1);

        // Event ring: one segment, described by a one-entry segment table.
        let seg = Dma::new(RING_TRBS * 16, 4096);
        let erst = Dma::new(64, 64);
        unsafe {
            erst.ptr::<u64>().write_volatile(seg.phys());
            erst.ptr::<u32>().add(2).write_volatile(RING_TRBS as u32);
        }
        let ir0 = rt + 0x20;
        w32(ir0 + 0x08, 1); // ERSTSZ
        w64(ir0 + 0x18, seg.phys()); // ERDP
        w64(ir0 + 0x10, erst.phys()); // ERSTBA (last: this arms the ring)

        // Run.
        w32(op, r32(op) | 1);
        if !wait_until(100, || r32(op + 4) & 1 == 0) {
            kprintln!("xhci: controller did not start");
            return None;
        }

        let mut hc = Xhci {
            name: alloc::format!("{:04x}:{:04x}", dev.vendor, dev.device),
            op,
            rt,
            db,
            max_slots,
            max_ports,
            csz,
            dcbaa,
            _scratch: scratch,
            cmd,
            evt: EventRing { seg, _erst: erst, deq: 0, cycle: 1 },
            slots: (0..=max_slots).map(|_| None).collect(),
            cmd_result: None,
            done: Vec::new(),
            port_changes: Vec::new(),
        };
        // Power all ports; connected ones are picked up through port changes.
        for p in 1..=max_ports {
            let v = hc.portsc(p);
            if v & PORT_PP == 0 {
                hc.write_portsc(p, v | PORT_PP);
            }
        }
        wait_until(50, || false); // let link training settle
        for p in 1..=max_ports {
            if hc.portsc(p) & PORT_CCS != 0 {
                hc.port_changes.push(p);
            }
        }
        kprintln!("xhci {}: {} ports, {} slots, {}-byte contexts", hc.name, hc.max_ports, max_slots, csz);
        Some(hc)
    }

    /// Ask the firmware (BIOS/UEFI SMM) to give up the controller.
    fn bios_handoff(cap: u64, hcc1: u32) {
        let mut off = ((hcc1 >> 16) as u64) << 2;
        while off != 0 {
            let p = cap + off;
            let v = r32(p);
            if v & 0xFF == 1 {
                if v & (1 << 16) != 0 {
                    w32(p, v | 1 << 24);
                    if !wait_until(1000, || r32(p) & (1 << 16) == 0) {
                        kprintln!("xhci: firmware did not release the controller");
                    }
                }
                let ctl = r32(p + 4);
                w32(p + 4, (ctl & !0x0000_E011) | 0xE000_0000);
                return;
            }
            let next = ((v >> 8) & 0xFF) as u64;
            if next == 0 {
                return;
            }
            off += next << 2;
        }
    }

    // ---- ports -------------------------------------------------------------

    fn portsc_addr(&self, port: u8) -> u64 {
        self.op + 0x400 + 0x10 * (port as u64 - 1)
    }
    pub fn portsc(&self, port: u8) -> u32 {
        r32(self.portsc_addr(port))
    }
    /// Write PORTSC without accidentally clearing change bits or disabling the port.
    fn write_portsc(&self, port: u8, v: u32) {
        w32(self.portsc_addr(port), v & !(PORT_CHANGE_BITS | PORT_PED));
    }
    fn clear_port_changes(&self, port: u8) {
        let v = self.portsc(port);
        w32(self.portsc_addr(port), (v & !PORT_PED) | PORT_CHANGE_BITS);
    }

    pub fn port_connected(&self, port: u8) -> bool {
        self.portsc(port) & PORT_CCS != 0
    }

    /// Did the connection actually change (as opposed to e.g. our own reset finishing)?
    /// Clears the port's change bits.
    pub fn take_connect_change(&self, port: u8) -> bool {
        let csc = self.portsc(port) & (1 << 17) != 0;
        self.clear_port_changes(port);
        csc
    }

    pub fn take_port_changes(&mut self) -> Vec<u8> {
        let mut v = core::mem::take(&mut self.port_changes);
        v.sort_unstable();
        v.dedup();
        v
    }

    /// Reset/enable a connected port. Returns its speed once enabled.
    pub fn reset_port(&mut self, port: u8) -> Option<Speed> {
        self.clear_port_changes(port);
        let v = self.portsc(port);
        if v & PORT_CCS == 0 {
            return None;
        }
        if v & PORT_PED == 0 {
            // USB 2 ports need a reset to enable; USB 3 ports normally enable themselves,
            // so if one hasn't, try a warm reset.
            let usb3 = Speed::from_portsc(v) == Speed::Super;
            self.write_portsc(port, v | if usb3 { PORT_WPR } else { PORT_PR });
            wait_until(500, || self.portsc(port) & PORT_PRC != 0 || self.portsc(port) & PORT_PED != 0);
            wait_until(100, || self.portsc(port) & PORT_PED != 0);
            self.clear_port_changes(port);
        }
        let v = self.portsc(port);
        if v & PORT_PED == 0 {
            kprintln!("xhci: port {} would not enable (portsc {:#x})", port, v);
            return None;
        }
        wait_until(20, || false); // reset recovery time
        Some(Speed::from_portsc(v))
    }

    // ---- events ------------------------------------------------------------

    /// Drain the event ring into the pending command/transfer/port-change queues.
    pub fn poll(&mut self) {
        let mut any = false;
        loop {
            let p = unsafe { self.evt.seg.ptr::<u32>().add(self.evt.deq * 4) };
            let ctrl = unsafe { read_volatile(p.add(3)) };
            if ctrl & 1 != self.evt.cycle {
                break;
            }
            fence(Ordering::SeqCst);
            let (lo, hi, status) = unsafe { (read_volatile(p), read_volatile(p.add(1)), read_volatile(p.add(2))) };
            let ty = (ctrl >> 10) & 0x3F;
            let code = (status >> 24) as u8;
            match ty {
                EVT_CMD_COMPLETE => {
                    self.cmd_result = Some(((hi as u64) << 32 | lo as u64, code, (ctrl >> 24) as u8));
                }
                EVT_TRANSFER => self.done.push(Completion {
                    slot: (ctrl >> 24) as u8,
                    dci: ((ctrl >> 16) & 0x1F) as u8,
                    code,
                    residual: status & 0xFF_FFFF,
                }),
                EVT_PORT_CHANGE => self.port_changes.push((lo >> 24) as u8),
                _ => {}
            }
            self.evt.deq += 1;
            if self.evt.deq == RING_TRBS {
                self.evt.deq = 0;
                self.evt.cycle ^= 1;
            }
            any = true;
        }
        if any {
            let erdp = self.evt.seg.phys() + (self.evt.deq * 16) as u64;
            w64(self.rt + 0x20 + 0x18, erdp | 1 << 3); // EHB: event handler busy cleared
        }
    }

    pub fn take_completion(&mut self, slot: u8, dci: u8) -> Option<Completion> {
        let i = self.done.iter().position(|c| c.slot == slot && c.dci == dci)?;
        Some(self.done.remove(i))
    }

    fn ring_doorbell(&self, slot: u8, target: u32) {
        fence(Ordering::SeqCst);
        w32(self.db + 4 * slot as u64, target);
    }

    fn command(&mut self, trb: [u32; 4]) -> Result<u8, UsbError> {
        self.cmd_result = None;
        let phys = self.cmd.push(trb);
        self.ring_doorbell(0, 0);
        let end = ticks() + 2 * HZ;
        loop {
            self.poll();
            if let Some((ptr, code, slot)) = self.cmd_result {
                if ptr == phys {
                    self.cmd_result = None;
                    return if code == CC_SUCCESS { Ok(slot) } else { Err(UsbError::Failed(code)) };
                }
            }
            if ticks() > end {
                return Err(UsbError::Timeout);
            }
            core::hint::spin_loop();
        }
    }

    fn wait_transfer(&mut self, slot: u8, dci: u8, timeout_ms: u64) -> Result<Completion, UsbError> {
        let end = ticks() + timeout_ms * HZ / 1000 + 1;
        loop {
            self.poll();
            if let Some(c) = self.take_completion(slot, dci) {
                return match c.code {
                    CC_SUCCESS | CC_SHORT => Ok(c),
                    CC_STALL => Err(UsbError::Stall),
                    other => Err(UsbError::Failed(other)),
                };
            }
            if ticks() > end {
                return Err(UsbError::Timeout);
            }
            core::hint::spin_loop();
        }
    }

    // ---- contexts ------------------------------------------------------------

    fn ctx_dw(&self, ctx: &Dma, index: usize, dw: usize) -> *mut u32 {
        unsafe { ctx.ptr::<u8>().add(index * self.csz + dw * 4) as *mut u32 }
    }

    /// Input context helpers: index 0 = input control, 1 = slot, 1 + dci = endpoint.
    fn in_set(&self, s: &Slot, index: usize, dw: usize, v: u32) {
        unsafe { write_volatile(self.ctx_dw(&s.in_ctx, index, dw), v) };
    }

    fn clear_input(&self, s: &Slot) {
        unsafe { core::ptr::write_bytes(s.in_ctx.ptr::<u8>(), 0, s.in_ctx.len()) };
    }

    pub fn enable_slot(&mut self) -> Result<u8, UsbError> {
        self.command([0, 0, 0, TRB_ENABLE_SLOT << 10])
    }

    pub fn disable_slot(&mut self, slot: u8) {
        let _ = self.command([0, 0, 0, TRB_DISABLE_SLOT << 10 | (slot as u32) << 24]);
        unsafe { self.dcbaa.ptr::<u64>().add(slot as usize).write_volatile(0) };
        self.done.retain(|c| c.slot != slot);
        if let Some(s) = self.slots.get_mut(slot as usize) {
            *s = None;
        }
    }

    /// Give the device in `slot` an address and set up its control endpoint.
    /// `port` is the root hub port the device (or its hub chain) is plugged into, `route`
    /// the hub route string, and `tt` the (hub slot, hub port, multi-TT) translator that a
    /// low/full-speed device behind a high-speed hub talks through.
    pub fn address_device(
        &mut self,
        slot: u8,
        port: u8,
        route: u32,
        speed: Speed,
        tt: Option<(u8, u8, bool)>,
    ) -> Result<(), UsbError> {
        if slot as usize >= self.slots.len() {
            return Err(UsbError::NoDevice);
        }
        let s = Slot {
            speed,
            in_ctx: Dma::new(33 * self.csz, 4096),
            out_ctx: Dma::new(32 * self.csz, 4096),
            rings: (0..32).map(|_| None).collect(),
        };
        let ep0 = Ring::new();
        self.in_set(&s, 0, 1, 0b11); // add slot + EP0
        let mtt = matches!(tt, Some((_, _, true)));
        self.in_set(&s, 1, 0, 1 << 27 | (mtt as u32) << 25 | speed.code() << 20 | (route & 0xF_FFFF)); // 1 context entry
        self.in_set(&s, 1, 1, (port as u32) << 16);
        if let Some((hub_slot, hub_port, _)) = tt {
            self.in_set(&s, 1, 2, hub_slot as u32 | (hub_port as u32) << 8);
        }
        self.in_set(&s, 2, 1, 3 << 1 | 4 << 3 | (speed.default_mps0() as u32) << 16); // CErr=3, control
        let dq = ep0.dequeue_ptr();
        self.in_set(&s, 2, 2, dq as u32);
        self.in_set(&s, 2, 3, (dq >> 32) as u32);
        self.in_set(&s, 2, 4, 8); // average TRB length
        unsafe { self.dcbaa.ptr::<u64>().add(slot as usize).write_volatile(s.out_ctx.phys()) };
        let in_phys = s.in_ctx.phys();
        let mut s = s;
        s.rings[1] = Some(ep0);
        self.slots[slot as usize] = Some(s);
        self.command([in_phys as u32, (in_phys >> 32) as u32, 0, TRB_ADDRESS_DEVICE << 10 | (slot as u32) << 24])?;
        Ok(())
    }

    /// Update EP0's max packet size (full-speed devices report it in their first 8 bytes).
    pub fn set_ep0_mps(&mut self, slot: u8, mps: u16) -> Result<(), UsbError> {
        let s = self.slots[slot as usize].as_ref().ok_or(UsbError::NoDevice)?;
        self.clear_input(s);
        self.in_set(s, 0, 1, 1 << 1);
        self.in_set(s, 2, 1, 3 << 1 | 4 << 3 | (mps as u32) << 16);
        let p = s.in_ctx.phys();
        self.command([p as u32, (p >> 32) as u32, 0, TRB_EVALUATE_CTX << 10 | (slot as u32) << 24]).map(|_| ())
    }

    /// Add endpoints to a configured device. `hub` = Some(number of ports) marks the device as a hub.
    pub fn configure_endpoints(&mut self, slot: u8, eps: &[EpConfig], hub: Option<u8>) -> Result<(), UsbError> {
        let csz = self.csz;
        let s = self.slots[slot as usize].as_mut().ok_or(UsbError::NoDevice)?;
        let speed = s.speed;
        unsafe { core::ptr::write_bytes(s.in_ctx.ptr::<u8>(), 0, s.in_ctx.len()) };
        let mut add = 1u32; // slot context
        let mut max_dci = 1;
        let mut rings = Vec::new();
        for ep in eps {
            let ring = Ring::new();
            let dq = ring.dequeue_ptr();
            let (ty, avg) = match ep.kind {
                EpType::BulkOut => (2u32, 3072u32),
                EpType::BulkIn => (6, 3072),
                EpType::InterruptIn => (7, ep.max_packet as u32),
            };
            let interval = match (ep.kind, speed) {
                (EpType::InterruptIn, Speed::Low | Speed::Full) => {
                    // bInterval is in 1 ms frames; xHCI wants 2^n * 125 us.
                    let frames = (ep.interval.max(1) as u32) * 8;
                    31 - frames.leading_zeros()
                }
                (EpType::InterruptIn, _) => (ep.interval.clamp(1, 16) - 1) as u32,
                _ => 0,
            };
            let esit = if matches!(ep.kind, EpType::InterruptIn) {
                ep.max_packet as u32 * (ep.max_burst as u32 + 1)
            } else {
                0
            };
            let idx = 1 + ep.dci as usize;
            let base = unsafe { s.in_ctx.ptr::<u8>().add(idx * csz) as *mut u32 };
            unsafe {
                write_volatile(base, interval << 16);
                write_volatile(base.add(1), 3 << 1 | ty << 3 | (ep.max_burst as u32) << 8 | (ep.max_packet as u32) << 16);
                write_volatile(base.add(2), dq as u32);
                write_volatile(base.add(3), (dq >> 32) as u32);
                write_volatile(base.add(4), avg | esit << 16);
            }
            add |= 1 << ep.dci;
            max_dci = max_dci.max(ep.dci as u32);
            rings.push((ep.dci, ring));
        }
        // Input control context: add flags.
        unsafe { write_volatile(s.in_ctx.ptr::<u8>().add(4) as *mut u32, add) };
        // Slot context: same as the current one but with more context entries.
        for dw in 0..8 {
            let v = unsafe { read_volatile(s.out_ctx.ptr::<u8>().add(dw * 4) as *const u32) };
            unsafe { write_volatile(s.in_ctx.ptr::<u8>().add(csz + dw * 4) as *mut u32, v) };
        }
        let slot_dw = |i: usize| unsafe { s.in_ctx.ptr::<u8>().add(csz + i * 4) as *mut u32 };
        let dw0 = unsafe { read_volatile(slot_dw(0)) };
        let hub_bit = if hub.is_some() { 1 << 26 } else { 0 };
        unsafe { write_volatile(slot_dw(0), (dw0 & !(0x1F << 27)) | max_dci << 27 | hub_bit) };
        if let Some(ports) = hub {
            let dw1 = unsafe { read_volatile(slot_dw(1)) };
            unsafe { write_volatile(slot_dw(1), (dw1 & 0x00FF_FFFF) | (ports as u32) << 24) };
        }
        // Device address / slot state are output-only fields.
        unsafe { write_volatile(slot_dw(3), 0) };
        for (dci, ring) in rings {
            s.rings[dci as usize] = Some(ring);
        }
        let p = s.in_ctx.phys();
        self.command([p as u32, (p >> 32) as u32, 0, TRB_CONFIGURE_EP << 10 | (slot as u32) << 24]).map(|_| ())
    }

    /// After a stall: reset the endpoint and point it past any stuck TRBs.
    pub fn recover_endpoint(&mut self, slot: u8, dci: u8) -> Result<(), UsbError> {
        let _ = self.command([0, 0, 0, TRB_RESET_EP << 10 | (dci as u32) << 16 | (slot as u32) << 24]);
        let dq = {
            let s = self.slots[slot as usize].as_ref().ok_or(UsbError::NoDevice)?;
            s.rings[dci as usize].as_ref().ok_or(UsbError::NoDevice)?.dequeue_ptr()
        };
        self.command([dq as u32, (dq >> 32) as u32, 0, TRB_SET_TR_DEQUEUE << 10 | (dci as u32) << 16 | (slot as u32) << 24])
            .map(|_| ())?;
        self.done.retain(|c| !(c.slot == slot && c.dci == dci));
        Ok(())
    }

    // ---- transfers -------------------------------------------------------------

    /// Control transfer on EP0. `data` is the data stage buffer (IN or OUT per bit 7 of request_type).
    pub fn control(
        &mut self,
        slot: u8,
        request_type: u8,
        request: u8,
        value: u16,
        index: u16,
        data: Option<(&Dma, usize)>,
    ) -> Result<usize, UsbError> {
        let dir_in = request_type & 0x80 != 0;
        let len = data.map(|(_, l)| l).unwrap_or(0);
        let ring = self.slots.get_mut(slot as usize).and_then(|s| s.as_mut()).and_then(|s| s.rings[1].as_mut());
        let ring = ring.ok_or(UsbError::NoDevice)?;
        ring.reserve(3);
        let setup_lo = request_type as u32 | (request as u32) << 8 | (value as u32) << 16;
        let setup_hi = index as u32 | (len as u32) << 16;
        let trt = if len == 0 { 0 } else if dir_in { 3 } else { 2 };
        ring.push([setup_lo, setup_hi, 8, TRB_SETUP << 10 | IDT | trt << 16]);
        if let Some((buf, l)) = data {
            let p = buf.phys();
            ring.push([p as u32, (p >> 32) as u32, l as u32, TRB_DATA << 10 | (dir_in as u32) << 16]);
        }
        let status_in = len == 0 || !dir_in;
        ring.push([0, 0, 0, TRB_STATUS << 10 | IOC | (status_in as u32) << 16]);
        self.ring_doorbell(slot, 1);
        match self.wait_transfer(slot, 1, 1000) {
            Ok(_) => Ok(len), // residual of the status stage is 0; callers check descriptor lengths
            Err(e) => {
                if e == UsbError::Stall {
                    let _ = self.recover_endpoint(slot, 1);
                }
                Err(e)
            }
        }
    }

    /// Queue a transfer on a bulk/interrupt endpoint without waiting for it.
    pub fn queue(&mut self, slot: u8, dci: u8, buf: u64, len: usize) -> Result<(), UsbError> {
        let ring = self.slots.get_mut(slot as usize).and_then(|s| s.as_mut()).and_then(|s| s.rings[dci as usize].as_mut());
        let ring = ring.ok_or(UsbError::NoDevice)?;
        ring.push([buf as u32, (buf >> 32) as u32, len as u32, TRB_NORMAL << 10 | IOC | ISP]);
        self.ring_doorbell(slot, dci as u32);
        Ok(())
    }

    /// Bulk transfer and wait. Returns bytes actually transferred.
    pub fn bulk(&mut self, slot: u8, dci: u8, buf: u64, len: usize, timeout_ms: u64) -> Result<usize, UsbError> {
        self.queue(slot, dci, buf, len)?;
        let c = self.wait_transfer(slot, dci, timeout_ms)?;
        Ok(len.saturating_sub(c.residual as usize))
    }

    pub fn has_slot(&self, slot: u8) -> bool {
        matches!(self.slots.get(slot as usize), Some(Some(_)))
    }

    #[allow(dead_code)]
    pub fn max_slots(&self) -> u8 {
        self.max_slots
    }
}

/// Small helper for DMA buffers used in control transfers.
pub fn dma_buf(len: usize) -> Dma {
    Dma::new(len.max(64), 64)
}

//! PS/2 keyboard + mouse. Interrupt handlers push events into a queue;
//! the main loop pops them and does the drawing.

use core::sync::atomic::{AtomicI32, AtomicU8, Ordering::*};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::instructions::port::Port;

const DATA: u16 = 0x60;
const CMD: u16 = 0x64; // status register when read, command register when written

#[derive(Clone, Copy, Debug)]
pub enum Event {
    /// Raw scancode (set 1). Bit 7 set = key released.
    Key(u8),
    /// Absolute position after this packet, button bits (bit0 left, bit1 right, bit2 middle),
    /// and wheel movement (positive = scroll down / toward the user).
    Mouse { x: i32, y: i32, buttons: u8, wheel: i8 },
}

const QLEN: usize = 256;
struct Queue {
    buf: [Event; QLEN],
    head: usize,
    len: usize,
}

static QUEUE: Mutex<Queue> = Mutex::new(Queue { buf: [Event::Key(0); QLEN], head: 0, len: 0 });

/// Called from interrupt handlers only (interrupts are already off there).
fn push(e: Event) {
    let mut q = QUEUE.lock();
    if q.len == QLEN {
        return; // drop when full
    }
    let i = (q.head + q.len) % QLEN;
    q.buf[i] = e;
    q.len += 1;
}

/// Add an event from outside an interrupt handler (USB input).
pub fn inject(e: Event) {
    without_interrupts(|| push(e));
}

/// Relative mouse movement from a USB mouse (dy positive = down, wheel positive = scroll down).
pub fn inject_mouse(dx: i32, dy: i32, buttons: u8, wheel: i8) {
    without_interrupts(|| {
        let (x, y) = keep(MOUSE_X.load(Relaxed) + speed(dx, &REM_X), MOUSE_Y.load(Relaxed) + speed(dy, &REM_Y));
        MOUSE_X.store(x, Relaxed);
        MOUSE_Y.store(y, Relaxed);
        push(Event::Mouse { x, y, buttons, wheel });
    });
}

/// Called from the main loop.
pub fn pop() -> Option<Event> {
    without_interrupts(|| {
        let mut q = QUEUE.lock();
        if q.len == 0 {
            return None;
        }
        let e = q.buf[q.head];
        q.head = (q.head + 1) % QLEN;
        q.len -= 1;
        Some(e)
    })
}

/// Must be called with interrupts disabled (used right before `hlt`).
pub fn pending() -> bool {
    QUEUE.lock().len > 0
}

static MOUSE_X: AtomicI32 = AtomicI32::new(0);
static MOUSE_Y: AtomicI32 = AtomicI32::new(0);
static BOUND_W: AtomicI32 = AtomicI32::new(1);
static BOUND_H: AtomicI32 = AtomicI32::new(1);
/// The screens of an extended desktop, x, y, w, h each (w = 0: none): the pointer stays on
/// them, out of the corners of the rectangle around them.
static AREAS: [AtomicI32; 8] = [const { AtomicI32::new(0) }; 8];

static PKT: [AtomicU8; 4] = [AtomicU8::new(0), AtomicU8::new(0), AtomicU8::new(0), AtomicU8::new(0)];
static PKT_IDX: AtomicU8 = AtomicU8::new(0);
/// 3 for a plain mouse, 4 once the scroll wheel (IntelliMouse mode) is enabled.
static PKT_LEN: AtomicU8 = AtomicU8::new(3);

/// One screen, w x h: the pointer goes to its middle.
pub fn set_bounds(w: usize, h: usize) {
    without_interrupts(|| {
        BOUND_W.store(w as i32, Relaxed);
        BOUND_H.store(h as i32, Relaxed);
        for a in &AREAS {
            a.store(0, Relaxed);
        }
        MOUSE_X.store(w as i32 / 2, Relaxed);
        MOUSE_Y.store(h as i32 / 2, Relaxed);
    });
}

/// A desktop of w x h with screens (x, y, w, h each, at most two) in it. The pointer stays
/// where it is if it's on one, or else goes to the nearest point on one.
pub fn set_area(w: usize, h: usize, screens: &[(i32, i32, i32, i32)]) {
    without_interrupts(|| {
        BOUND_W.store(w.max(1) as i32, Relaxed);
        BOUND_H.store(h.max(1) as i32, Relaxed);
        for k in 0..2 {
            let r = screens.get(k).copied().unwrap_or((0, 0, 0, 0));
            for (i, v) in [r.0, r.1, r.2, r.3].into_iter().enumerate() {
                AREAS[k * 4 + i].store(v, Relaxed);
            }
        }
        let (x, y) = keep(MOUSE_X.load(Relaxed), MOUSE_Y.load(Relaxed));
        MOUSE_X.store(x, Relaxed);
        MOUSE_Y.store(y, Relaxed);
    });
}

/// Put the pointer at (x, y), or the nearest point on a screen.
pub fn warp(x: i32, y: i32) {
    without_interrupts(|| {
        let (x, y) = keep(x, y);
        MOUSE_X.store(x, Relaxed);
        MOUSE_Y.store(y, Relaxed);
    });
}

/// The nearest point to (x, y) inside the desktop and on a screen.
fn keep(x: i32, y: i32) -> (i32, i32) {
    let x = x.clamp(0, BOUND_W.load(Relaxed) - 1);
    let y = y.clamp(0, BOUND_H.load(Relaxed) - 1);
    let mut best: Option<(i64, i32, i32)> = None;
    for k in 0..2 {
        let a = |i: usize| AREAS[k * 4 + i].load(Relaxed);
        let (rx, ry, rw, rh) = (a(0), a(1), a(2), a(3));
        if rw <= 0 || rh <= 0 {
            continue;
        }
        let (cx, cy) = (x.clamp(rx, rx + rw - 1), y.clamp(ry, ry + rh - 1));
        let d = (cx - x) as i64 * (cx - x) as i64 + (cy - y) as i64 * (cy - y) as i64;
        if best.is_none_or(|b| d < b.0) {
            best = Some((d, cx, cy));
        }
    }
    best.map_or((x, y), |b| (b.1, b.2))
}

pub fn mouse_pos() -> (i32, i32) {
    (MOUSE_X.load(Relaxed), MOUSE_Y.load(Relaxed))
}

/// Pointer speed in percent (100 = normal), set from Settings.
static SPEED: AtomicI32 = AtomicI32::new(100);
static REM_X: AtomicI32 = AtomicI32::new(0);
static REM_Y: AtomicI32 = AtomicI32::new(0);

pub fn set_speed(percent: i32) {
    SPEED.store(percent.clamp(20, 400), Relaxed);
}

/// Scale a movement by the pointer speed, keeping the leftover fraction for next time.
fn speed(d: i32, rem: &AtomicI32) -> i32 {
    let v = d * SPEED.load(Relaxed) + rem.load(Relaxed);
    let out = v / 100;
    rem.store(v - out * 100, Relaxed);
    out
}

pub fn has_wheel() -> bool {
    PKT_LEN.load(Relaxed) == 4
}

unsafe fn wait_write() -> bool {
    for _ in 0..100_000 {
        if Port::<u8>::new(CMD).read() & 0b10 == 0 {
            return true;
        }
    }
    false
}

unsafe fn wait_read() -> bool {
    for _ in 0..100_000 {
        if Port::<u8>::new(CMD).read() & 0b01 != 0 {
            return true;
        }
    }
    false
}

unsafe fn controller_cmd(c: u8) {
    wait_write();
    Port::<u8>::new(CMD).write(c);
}

unsafe fn write_data(d: u8) {
    wait_write();
    Port::<u8>::new(DATA).write(d);
}

unsafe fn read_data() -> Option<u8> {
    if wait_read() {
        Some(Port::<u8>::new(DATA).read())
    } else {
        None
    }
}

/// Send a byte to the mouse (not the keyboard) and wait for its ACK (0xFA).
unsafe fn mouse_write(b: u8) -> bool {
    controller_cmd(0xD4);
    write_data(b);
    read_data() == Some(0xFA)
}

/// Enable the PS/2 mouse and its IRQ. Returns false if the mouse didn't respond.
/// Call before interrupts are enabled, so the ACK bytes aren't eaten by the IRQ handlers.
pub fn init() -> bool {
    unsafe {
        // Drain anything stale in the output buffer.
        for _ in 0..16 {
            if Port::<u8>::new(CMD).read() & 1 == 0 {
                break;
            }
            Port::<u8>::new(DATA).read();
        }

        controller_cmd(0xA8); // enable the auxiliary (mouse) port
        controller_cmd(0x20); // read controller config byte
        let mut cfg = read_data().unwrap_or(0x47);
        cfg |= 0b11; // enable IRQ1 (keyboard) and IRQ12 (mouse)
        cfg &= !(1 << 5); // make sure the mouse clock is enabled
        controller_cmd(0x60); // write controller config byte
        write_data(cfg);

        let mut ok = mouse_write(0xF6); // defaults

        // Unlock the scroll wheel: the magic sample-rate sequence 200, 100, 80,
        // after which a wheel mouse reports device ID 3 and sends 4-byte packets.
        for rate in [200u8, 100, 80] {
            ok &= mouse_write(0xF3) && mouse_write(rate);
        }
        if mouse_write(0xF2) && read_data() == Some(3) {
            PKT_LEN.store(4, Relaxed);
        }

        ok &= mouse_write(0xF4); // start streaming packets

        // Drop any late replies so the first IRQ starts on a packet boundary.
        for _ in 0..16 {
            if !wait_read() {
                break;
            }
            Port::<u8>::new(DATA).read();
        }
        ok
    }
}

pub fn on_key_byte(scancode: u8) {
    if scancode == 0xFA {
        return; // stray ACK, not a key
    }
    push(Event::Key(scancode));
}

/// Called from the IRQ12 handler with one byte; every 3 bytes make a packet.
pub fn on_mouse_byte(b: u8) {
    let i = PKT_IDX.load(Relaxed) as usize;
    // Bit 3 is always set in byte 0. Both overflow bits set is really a late
    // ACK (0xFA) or similar reply, not a packet start; skip it to stay in sync.
    if i == 0 && (b & 0x08 == 0 || b & 0xC0 == 0xC0) {
        return;
    }
    PKT[i].store(b, Relaxed);
    if i + 1 < PKT_LEN.load(Relaxed) as usize {
        PKT_IDX.store(i as u8 + 1, Relaxed);
        return;
    }
    PKT_IDX.store(0, Relaxed);

    let flags = PKT[0].load(Relaxed);
    if flags & 0xC0 != 0 {
        return; // overflow, discard
    }
    let mut dx = PKT[1].load(Relaxed) as i32;
    let mut dy = PKT[2].load(Relaxed) as i32;
    if flags & 0x10 != 0 {
        dx -= 256;
    }
    if flags & 0x20 != 0 {
        dy -= 256;
    }

    // PS/2 reports "up" as positive Y; screen Y grows downward.
    let (x, y) = keep(MOUSE_X.load(Relaxed) + speed(dx, &REM_X), MOUSE_Y.load(Relaxed) - speed(dy, &REM_Y));
    MOUSE_X.store(x, Relaxed);
    MOUSE_Y.store(y, Relaxed);
    // Wheel is the low 4 bits of byte 3, as a signed nibble.
    let wheel = if PKT_LEN.load(Relaxed) == 4 { ((PKT[3].load(Relaxed) << 4) as i8) >> 4 } else { 0 };
    push(Event::Mouse { x, y, buttons: flags & 0b111, wheel });
}

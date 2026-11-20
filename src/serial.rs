//! Kernel log: COM1 serial output (QEMU `-serial`), plus an in-memory copy so the log
//! can be shown on screen (F12, and on the crash screen) on real PCs without a serial port.

use core::fmt::{self, Write};
use spin::Mutex;
use x86_64::instructions::interrupts::without_interrupts;
use x86_64::instructions::port::Port;

const COM1: u16 = 0x3F8;
const LOG_SIZE: usize = 32 * 1024;

pub struct LogBuf {
    buf: [u8; LOG_SIZE],
    /// Total bytes ever written; the ring holds the last LOG_SIZE of them.
    written: usize,
}

pub static LOG: Mutex<LogBuf> = Mutex::new(LogBuf { buf: [0; LOG_SIZE], written: 0 });

impl LogBuf {
    fn push(&mut self, b: u8) {
        self.buf[self.written % LOG_SIZE] = b;
        self.written += 1;
    }

    /// Call `f` with each of the last `max` lines, oldest first.
    /// Every line still in the log, oldest first.
    pub fn lines(&self) -> alloc::vec::Vec<alloc::vec::Vec<u8>> {
        let len = self.written.min(LOG_SIZE);
        let mut out = alloc::vec::Vec::new();
        let mut cur = alloc::vec::Vec::new();
        for k in self.written - len..self.written {
            let b = self.buf[k % LOG_SIZE];
            if b == b'\n' {
                out.push(core::mem::take(&mut cur));
            } else if cur.len() < 160 {
                cur.push(b);
            }
        }
        if len == LOG_SIZE && !out.is_empty() {
            out.remove(0); // probably cut off at the start
        }
        out
    }

    pub fn last_lines(&self, max: usize, mut f: impl FnMut(&[u8])) {
        let len = self.written.min(LOG_SIZE);
        let start = self.written - len;
        // Walk back to find where the last `max` lines begin.
        let mut lines = 0;
        let mut from = start;
        let mut i = self.written;
        while i > start {
            i -= 1;
            if self.buf[i % LOG_SIZE] == b'\n' && i + 1 != self.written {
                lines += 1;
                if lines == max {
                    from = i + 1;
                    break;
                }
            }
        }
        let mut line = [0u8; 160];
        let mut n = 0;
        for k in from..self.written {
            let b = self.buf[k % LOG_SIZE];
            if b == b'\n' {
                f(&line[..n]);
                n = 0;
            } else if n < line.len() {
                line[n] = b;
                n += 1;
            }
        }
        if n > 0 {
            f(&line[..n]);
        }
    }
}

pub fn init() {
    unsafe {
        Port::<u8>::new(COM1 + 1).write(0x00); // no serial interrupts
        Port::<u8>::new(COM1 + 3).write(0x80); // enable divisor latch
        Port::<u8>::new(COM1).write(0x03); // 38400 baud (low byte)
        Port::<u8>::new(COM1 + 1).write(0x00); // (high byte)
        Port::<u8>::new(COM1 + 3).write(0x03); // 8 bits, no parity, 1 stop
        Port::<u8>::new(COM1 + 2).write(0xC7); // enable FIFO
        Port::<u8>::new(COM1 + 4).write(0x0B); // RTS/DSR set
    }
}

const COM2: u16 = 0x2F8;

/// COM2 carries raw bytes for the hidden test printer (QEMU's second `-serial`).
pub fn com2_init() {
    unsafe {
        Port::<u8>::new(COM2 + 1).write(0x00);
        Port::<u8>::new(COM2 + 3).write(0x80);
        Port::<u8>::new(COM2).write(0x01); // 115200 baud
        Port::<u8>::new(COM2 + 1).write(0x00);
        Port::<u8>::new(COM2 + 3).write(0x03);
        Port::<u8>::new(COM2 + 2).write(0xC7);
        Port::<u8>::new(COM2 + 4).write(0x0B);
    }
}

pub fn com2_write(data: &[u8]) {
    for &b in data {
        unsafe {
            for _ in 0..10_000 {
                if Port::<u8>::new(COM2 + 5).read() & 0x20 != 0 {
                    break;
                }
            }
            Port::<u8>::new(COM2).write(b);
        }
    }
}

struct Serial;

impl Write for Serial {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            unsafe {
                // Wait (briefly) for the transmit buffer; a PC without COM1 must not hang here.
                for _ in 0..10_000 {
                    if Port::<u8>::new(COM1 + 5).read() & 0x20 != 0 {
                        break;
                    }
                }
                Port::<u8>::new(COM1).write(b);
            }
        }
        Ok(())
    }
}

struct ToLog<'a>(&'a mut LogBuf);

impl Write for ToLog<'_> {
    fn write_str(&mut self, s: &str) -> fmt::Result {
        for b in s.bytes() {
            self.0.push(b);
        }
        Ok(())
    }
}

pub fn _print(args: fmt::Arguments) {
    without_interrupts(|| {
        let _ = Serial.write_fmt(args);
        if let Some(mut log) = LOG.try_lock() {
            let _ = ToLog(&mut log).write_fmt(args);
        }
    });
}

macro_rules! kprintln {
    () => ($crate::serial::_print(format_args!("\n")));
    ($($arg:tt)*) => ($crate::serial::_print(format_args!("{}\n", format_args!($($arg)*))));
}

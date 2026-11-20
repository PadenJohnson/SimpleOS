//! Crash screen: if the kernel panics, show what happened directly on the framebuffer
//! (no heap use) so problems on real PCs can be reported from a photo of the screen.

use core::fmt::Write;
use core::sync::atomic::{AtomicU64, AtomicUsize, Ordering::Relaxed};
use noto_sans_mono_bitmap::{get_raster, get_raster_width, FontWeight, RasterHeight};

static FB_BASE: AtomicU64 = AtomicU64::new(0);
static FB_W: AtomicUsize = AtomicUsize::new(0);
static FB_H: AtomicUsize = AtomicUsize::new(0);
static FB_PITCH: AtomicUsize = AtomicUsize::new(0);
static FB_SHIFTS: AtomicU64 = AtomicU64::new(0);

/// Remember where the screen is (called once at boot).
pub fn set_framebuffer(base: *mut u8, w: usize, h: usize, pitch: usize, r: u8, g: u8, b: u8) {
    FB_BASE.store(base as u64, Relaxed);
    FB_W.store(w, Relaxed);
    FB_H.store(h, Relaxed);
    FB_PITCH.store(pitch, Relaxed);
    FB_SHIFTS.store((r as u64) << 16 | (g as u64) << 8 | b as u64, Relaxed);
}

struct Fb {
    base: *mut u8,
    w: usize,
    h: usize,
    pitch: usize,
    sh: (u32, u32, u32),
}

impl Fb {
    fn get() -> Option<Fb> {
        let base = FB_BASE.load(Relaxed) as *mut u8;
        if base.is_null() {
            return None;
        }
        let s = FB_SHIFTS.load(Relaxed);
        Some(Fb {
            base,
            w: FB_W.load(Relaxed),
            h: FB_H.load(Relaxed),
            pitch: FB_PITCH.load(Relaxed),
            sh: ((s >> 16) as u32 & 0xFF, (s >> 8) as u32 & 0xFF, s as u32 & 0xFF),
        })
    }
    fn put(&self, x: usize, y: usize, rgb: u32) {
        if x < self.w && y < self.h {
            let (r, g, b) = ((rgb >> 16) & 0xFF, (rgb >> 8) & 0xFF, rgb & 0xFF);
            let px = r << self.sh.0 | g << self.sh.1 | b << self.sh.2;
            unsafe { core::ptr::write_volatile(self.base.add(y * self.pitch + x * 4) as *mut u32, px) };
        }
    }
    fn fill(&self, x: usize, y: usize, w: usize, h: usize, rgb: u32) {
        for yy in y..(y + h).min(self.h) {
            for xx in x..(x + w).min(self.w) {
                self.put(xx, yy, rgb);
            }
        }
    }
    /// Draw ASCII text; returns the x after the last character.
    fn text(&self, x: usize, y: usize, s: &[u8], size: RasterHeight, bold: bool, rgb: u32) -> usize {
        let weight = if bold { FontWeight::Bold } else { FontWeight::Regular };
        let cw = get_raster_width(weight, size);
        let mut cx = x;
        for &ch in s {
            let c = if ch == b'\n' {
                ' '
            } else if (0x20..0x7F).contains(&ch) {
                ch as char
            } else {
                '?'
            };
            if cx + cw > self.w {
                break;
            }
            if let Some(g) = get_raster(c, weight, size) {
                for (gy, row) in g.rows().enumerate() {
                    for (gx, &a) in row.iter().enumerate() {
                        if a > 128 {
                            self.put(cx + gx, y + gy, rgb);
                        }
                    }
                }
            }
            cx += cw;
        }
        cx
    }
}

/// Fixed-size text buffer for formatting without the heap.
struct StackStr {
    buf: [u8; 600],
    len: usize,
}

impl Write for StackStr {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        for b in s.bytes() {
            if self.len < self.buf.len() {
                self.buf[self.len] = b;
                self.len += 1;
            }
        }
        Ok(())
    }
}

/// Draw the crash screen. Safe to call from the panic handler.
pub fn show(info: &core::panic::PanicInfo) {
    let Some(fb) = Fb::get() else { return };
    let bg = 0x4A1414;
    fb.fill(0, 0, fb.w, fb.h, bg);
    let white = 0xFFFFFF;
    let dim = 0xE0B0B0;
    let mut y = 40;
    fb.text(40, y, b"Solon stopped because of a problem.", RasterHeight::Size32, true, white);
    y += 50;
    fb.text(40, y, b"Your photos and files are safe. Please take a picture of this screen, then restart the computer.", RasterHeight::Size16, false, dim);
    y += 36;

    let mut msg = StackStr { buf: [0; 600], len: 0 };
    let _ = write!(msg, "{}", info);
    // Wrap the message across lines.
    let cols = ((fb.w - 80) / get_raster_width(FontWeight::Bold, RasterHeight::Size20)).max(20);
    for chunk in msg.buf[..msg.len].chunks(cols) {
        fb.text(40, y, chunk, RasterHeight::Size20, true, white);
        y += 26;
    }
    y += 16;
    fb.text(40, y, b"Last log lines:", RasterHeight::Size16, true, dim);
    y += 24;
    let max_lines = (fb.h.saturating_sub(y + 20)) / 18;
    if let Some(log) = crate::serial::LOG.try_lock() {
        log.last_lines(max_lines, |line| {
            fb.text(40, y, line, RasterHeight::Size16, false, white);
            y += 18;
        });
    }
}

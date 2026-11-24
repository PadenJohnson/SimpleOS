//! 32-bpp linear framebuffer access, and putting the desktop on it: as is, enlarged by whole
//! or half steps (text size), or smoothly scaled into part of the screen (a lower resolution
//! shown the way a monitor would, with black bars if its shape differs).
//!
//! Writing to the graphics card's memory is far slower than to ordinary memory, so `present`
//! remembers the last picture and sends only what changed (typing a letter sends a few rows
//! of a few words, not the whole screen).

use alloc::vec::Vec;
use core::cell::{Cell, RefCell};
use core::ptr;
use limine::framebuffer::Framebuffer;

#[derive(Clone, Copy)]
pub struct Rgb(pub u8, pub u8, pub u8);

/// What was sent last: how it was laid out, and the desktop picture itself.
struct Last {
    w: usize,
    h: usize,
    scale: u32,
    dst: (usize, usize, usize, usize),
    px: Vec<u32>,
}

pub struct Screen {
    base: *mut u8,
    pub width: usize,
    pub height: usize,
    pitch: usize, // bytes per row
    r_shift: u8,
    g_shift: u8,
    b_shift: u8,
    last: RefCell<Option<Last>>,
    /// Pixels written to the card, and how many a full-screen copy each time would have taken
    /// (for the diagnostics log).
    written: Cell<u64>,
    full: Cell<u64>,
}

/// Columns x0..x1 of rows y0..y1 (end exclusive).
#[derive(Clone, Copy, Debug)]
struct Area {
    x0: usize,
    x1: usize,
    y0: usize,
    y1: usize,
}

impl Screen {
    pub fn new(fb: &Framebuffer) -> Option<Self> {
        if fb.bpp() != 32 {
            return None;
        }
        Some(Self {
            base: fb.addr() as *mut u8,
            width: fb.width() as usize,
            height: fb.height() as usize,
            pitch: fb.pitch() as usize,
            r_shift: fb.red_mask_shift(),
            g_shift: fb.green_mask_shift(),
            b_shift: fb.blue_mask_shift(),
            last: RefCell::new(None),
            written: Cell::new(0),
            full: Cell::new(0),
        })
    }

    /// A picture in memory a driver set up itself (a second monitor): 32 bits a pixel,
    /// red, green and blue at these bit positions.
    pub fn raw(base: *mut u8, width: usize, height: usize, pitch: usize, shifts: (u8, u8, u8)) -> Self {
        Self {
            base,
            width,
            height,
            pitch,
            r_shift: shifts.0,
            g_shift: shifts.1,
            b_shift: shifts.2,
            last: RefCell::new(None),
            written: Cell::new(0),
            full: Cell::new(0),
        }
    }

    /// Where a `w` x `h` picture goes to fill this screen without changing its shape (black
    /// bars at the sides or top and bottom if needed): (x, y, w, h).
    pub fn fit(&self, w: usize, h: usize) -> (usize, usize, usize, usize) {
        if w == 0 || h == 0 {
            return (0, 0, self.width, self.height);
        }
        if self.width * h <= self.height * w {
            let oh = h * self.width / w;
            (0, (self.height - oh) / 2, self.width, oh)
        } else {
            let ow = w * self.height / h;
            ((self.width - ow) / 2, 0, ow, self.height)
        }
    }

    /// The graphics card switched to another mode (Bochs VBE): new address, size and row length.
    pub fn set_mode(&mut self, base: *mut u8, width: usize, height: usize, pitch: usize) {
        self.base = base;
        self.width = width;
        self.height = height;
        self.pitch = pitch;
        *self.last.borrow_mut() = None;
    }

    pub fn base_addr(&self) -> u64 {
        self.base as u64
    }

    /// Bytes per row.
    pub fn pitch(&self) -> usize {
        self.pitch
    }

    /// Something else drew on the screen: the next `present` sends everything.
    pub fn invalidate(&self) {
        *self.last.borrow_mut() = None;
    }

    /// (pixels written, pixels a full copy each time would have written) since the start.
    pub fn traffic(&self) -> (u64, u64) {
        (self.written.get(), self.full.get())
    }

    /// Paint the whole screen black (around a scaled desktop).
    pub fn clear(&self) {
        for row in 0..self.height {
            let dst = unsafe { self.base.add(row * self.pitch) as *mut u32 };
            for i in 0..self.width {
                unsafe { ptr::write_volatile(dst.add(i), 0) };
            }
        }
        self.invalidate();
    }

    /// Let the crash screen draw here too.
    pub fn register_for_crashes(&self) {
        crate::crash::set_framebuffer(self.base, self.width, self.height, self.pitch, self.r_shift, self.g_shift, self.b_shift);
    }

    /// Convert an RGB colour into the framebuffer's native pixel format.
    pub fn pack(&self, c: Rgb) -> u32 {
        ((c.0 as u32) << self.r_shift) | ((c.1 as u32) << self.g_shift) | ((c.2 as u32) << self.b_shift)
    }

    fn native(&self) -> bool {
        self.r_shift == 16 && self.g_shift == 8 && self.b_shift == 0
    }

    #[inline]
    fn conv(&self, p: u32) -> u32 {
        if self.native() {
            p
        } else {
            self.pack(Rgb((p >> 16) as u8, (p >> 8) as u8, p as u8))
        }
    }

    #[inline]
    pub fn put(&self, x: usize, y: usize, px: u32) {
        if x < self.width && y < self.height {
            unsafe {
                ptr::write_volatile(self.base.add(y * self.pitch + x * 4) as *mut u32, px);
            }
        }
    }

    #[inline]
    pub fn get(&self, x: usize, y: usize) -> u32 {
        if x < self.width && y < self.height {
            unsafe { ptr::read_volatile(self.base.add(y * self.pitch + x * 4) as *const u32) }
        } else {
            0
        }
    }

    /// Write converted pixels to row `y` from column `x`.
    fn write_row(&self, y: usize, x: usize, src: &[u32]) {
        let n = src.len().min(self.width.saturating_sub(x));
        if y >= self.height || n == 0 {
            return;
        }
        let dst = unsafe { self.base.add(y * self.pitch + x * 4) as *mut u32 };
        if self.native() {
            unsafe { ptr::copy_nonoverlapping(src.as_ptr(), dst, n) };
        } else {
            for (i, &p) in src[..n].iter().enumerate() {
                unsafe { ptr::write_volatile(dst.add(i), self.conv(p)) };
            }
        }
        self.written.set(self.written.get() + n as u64);
    }

    /// Put the desktop canvas on the screen in `dst` (x, y, w, h): directly when it fills the
    /// screen at a whole/half text scale, otherwise smoothly scaled. Only what changed since
    /// the last time is sent.
    pub fn present(&self, c: &crate::gfx::Canvas, scale: u32, dst: (usize, usize, usize, usize)) {
        self.full.set(self.full.get() + (dst.2 * dst.3) as u64);
        let direct = dst == (0, 0, self.width, self.height) && c.w * scale as usize / 2 == self.width && c.h * scale as usize / 2 == self.height;
        // Which rows changed, and where in them (in the canvas).
        let mut last = self.last.borrow_mut();
        let same_layout = last.as_ref().is_some_and(|l| l.w == c.w && l.h == c.h && l.scale == scale && l.dst == dst);
        let mut rows: Vec<(usize, usize, usize)> = Vec::new();
        if same_layout {
            let l = last.as_mut().unwrap();
            for y in 0..c.h {
                let (a, b) = (&c.px[y * c.w..(y + 1) * c.w], &mut l.px[y * c.w..(y + 1) * c.w]);
                if a == b {
                    continue;
                }
                let x0 = a.iter().zip(b.iter()).position(|(p, q)| p != q).unwrap_or(0);
                let x1 = c.w - a.iter().rev().zip(b.iter().rev()).position(|(p, q)| p != q).unwrap_or(0);
                b[x0..x1].copy_from_slice(&a[x0..x1]);
                rows.push((y, x0, x1));
            }
            if rows.is_empty() {
                return;
            }
        } else {
            *last = Some(Last { w: c.w, h: c.h, scale, dst, px: c.px.clone() });
            rows = (0..c.h).map(|y| (y, 0, c.w)).collect();
        }
        drop(last);
        if direct && scale <= 2 {
            // Same size: just the changed part of each changed row.
            for &(y, x0, x1) in &rows {
                self.write_row(y, x0, &c.px[y * c.w + x0..y * c.w + x1]);
            }
            return;
        }
        // Scaled: the screen area covering the changed rows, a band at a time (a run of
        // changed rows close together makes one band).
        let mut bands: Vec<Area> = Vec::new();
        for &(y, x0, x1) in &rows {
            match bands.last_mut() {
                Some(b) if y <= b.y1 + 2 => {
                    b.y1 = y + 1;
                    b.x0 = b.x0.min(x0);
                    b.x1 = b.x1.max(x1);
                }
                _ => bands.push(Area { x0, x1, y0: y, y1: y + 1 }),
            }
        }
        for b in bands {
            if direct {
                self.blit_scaled_area(c, scale, b);
            } else {
                self.blit_fit_area(c, dst, b);
            }
        }
    }

    /// Enlarge part of a canvas (`a`, in canvas pixels) by `scale` halves (3 = 1.5x, 4 = 2x)
    /// with nearest-neighbour sampling.
    fn blit_scaled_area(&self, c: &crate::gfx::Canvas, scale: u32, a: Area) {
        let s = scale as usize;
        // Screen columns and rows whose source falls in the area.
        let ox0 = a.x0 * s / 2;
        let ox1 = (a.x1 * s).div_ceil(2).min(self.width);
        let oy0 = a.y0 * s / 2;
        let oy1 = (a.y1 * s).div_ceil(2).min(self.height);
        if ox0 >= ox1 {
            return;
        }
        let xmap: Vec<usize> = (ox0..ox1).map(|x| (x * 2 / s).min(c.w - 1)).collect();
        let mut line = alloc::vec![0u32; ox1 - ox0];
        let mut last_src = usize::MAX;
        for row in oy0..oy1 {
            let sy = (row * 2 / s).min(c.h - 1);
            if sy != last_src {
                let src = &c.px[sy * c.w..(sy + 1) * c.w];
                for (d, &sx) in line.iter_mut().zip(&xmap) {
                    *d = src[sx];
                }
                last_src = sy;
            }
            self.write_row(row, ox0, &line);
        }
    }

    /// Scale part of a canvas (`a`) into its place in `dst` with bilinear filtering (what a
    /// monitor does with a resolution that isn't its own).
    fn blit_fit_area(&self, c: &crate::gfx::Canvas, dst: (usize, usize, usize, usize), a: Area) {
        let (dx, dy, dw, dh) = dst;
        let (dw, dh) = (dw.min(self.width.saturating_sub(dx)), dh.min(self.height.saturating_sub(dy)));
        if dw == 0 || dh == 0 || c.w == 0 || c.h == 0 {
            return;
        }
        // Output pixels that blend source pixels of the area (one pixel more each side).
        let ox0 = (a.x0.saturating_sub(1) * dw / c.w).min(dw);
        let ox1 = ((a.x1 + 1) * dw).div_ceil(c.w).min(dw);
        let oy0 = (a.y0.saturating_sub(1) * dh / c.h).min(dh);
        let oy1 = ((a.y1 + 1) * dh).div_ceil(c.h).min(dh);
        if ox0 >= ox1 || oy0 >= oy1 {
            return;
        }
        // Source position of each output column: index and weight of the next one (0..256).
        let xmap: Vec<(usize, u32)> = (ox0..ox1)
            .map(|x| {
                let f = (((x as u64 * 2 + 1) * c.w as u64 * 256) / (dw as u64 * 2)).saturating_sub(128);
                (((f >> 8) as usize).min(c.w - 1), (f & 255) as u32)
            })
            .collect();
        let lerp = |a: u32, b: u32, w: u32| -> u32 {
            let rb = ((a & 0xFF00FF) * (256 - w) + (b & 0xFF00FF) * w) >> 8;
            let g = ((a & 0x00FF00) * (256 - w) + (b & 0x00FF00) * w) >> 8;
            (rb & 0xFF00FF) | (g & 0x00FF00)
        };
        let hrow = |sy: usize, out: &mut [u32]| {
            let src = &c.px[sy * c.w..(sy + 1) * c.w];
            for (o, &(i, w)) in out.iter_mut().zip(&xmap) {
                *o = if w == 0 || i + 1 >= c.w { src[i] } else { lerp(src[i], src[i + 1], w) };
            }
        };
        let n = ox1 - ox0;
        let (mut ra, mut rb, mut line) = (alloc::vec![0u32; n], alloc::vec![0u32; n], alloc::vec![0u32; n]);
        let mut have = (usize::MAX, usize::MAX);
        for row in oy0..oy1 {
            let f = (((row as u64 * 2 + 1) * c.h as u64 * 256) / (dh as u64 * 2)).saturating_sub(128);
            let (y0, wy) = (((f >> 8) as usize).min(c.h - 1), (f & 255) as u32);
            let y1 = (y0 + 1).min(c.h - 1);
            if have != (y0, y1) {
                if have.1 == y0 {
                    core::mem::swap(&mut ra, &mut rb);
                } else {
                    hrow(y0, &mut ra);
                }
                hrow(y1, &mut rb);
                have = (y0, y1);
            }
            for i in 0..n {
                line[i] = if wy == 0 { ra[i] } else { lerp(ra[i], rb[i], wy) };
            }
            self.write_row(dy + row, dx + ox0, &line);
        }
    }
}

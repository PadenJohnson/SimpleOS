//! Minimal 32-bpp linear framebuffer access, and putting the desktop on it: as is, enlarged
//! by whole or half steps (text size), or smoothly scaled into part of the screen (a lower
//! resolution shown the way a monitor would, with black bars if its shape differs).

use core::ptr;
use limine::framebuffer::Framebuffer;

#[derive(Clone, Copy)]
pub struct Rgb(pub u8, pub u8, pub u8);

pub struct Screen {
    base: *mut u8,
    pub width: usize,
    pub height: usize,
    pitch: usize, // bytes per row
    r_shift: u8,
    g_shift: u8,
    b_shift: u8,
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
        })
    }

    /// The graphics card switched to another mode (Bochs VBE): new address, size and row length.
    pub fn set_mode(&mut self, base: *mut u8, width: usize, height: usize, pitch: usize) {
        self.base = base;
        self.width = width;
        self.height = height;
        self.pitch = pitch;
    }

    /// Paint the whole screen black (around a scaled desktop).
    pub fn clear(&self) {
        for row in 0..self.height {
            let dst = unsafe { self.base.add(row * self.pitch) as *mut u32 };
            for i in 0..self.width {
                unsafe { ptr::write_volatile(dst.add(i), 0) };
            }
        }
    }

    /// Put the desktop canvas on the screen in `dst` (x, y, w, h): directly when it fills the
    /// screen at a whole/half text scale, otherwise smoothly scaled.
    pub fn present(&self, c: &crate::gfx::Canvas, scale: u32, dst: (usize, usize, usize, usize)) {
        let full = dst == (0, 0, self.width, self.height);
        if full && c.w * scale as usize / 2 == self.width && c.h * scale as usize / 2 == self.height {
            self.blit_scaled(c, scale)
        } else {
            self.blit_fit(c, dst)
        }
    }

    /// Scale a canvas into `dst` with bilinear filtering (what a monitor does with a resolution
    /// that isn't its own).
    pub fn blit_fit(&self, c: &crate::gfx::Canvas, dst: (usize, usize, usize, usize)) {
        let (dx, dy, dw, dh) = dst;
        let (dw, dh) = (dw.min(self.width.saturating_sub(dx)), dh.min(self.height.saturating_sub(dy)));
        if dw == 0 || dh == 0 || c.w == 0 || c.h == 0 {
            return;
        }
        let native = self.r_shift == 16 && self.g_shift == 8 && self.b_shift == 0;
        // Source position of each output column: index and weight of the next one (0..256).
        let xmap: alloc::vec::Vec<(usize, u32)> = (0..dw)
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
        let mut a = alloc::vec![0u32; dw];
        let mut b = alloc::vec![0u32; dw];
        let mut line = alloc::vec![0u32; dw];
        let mut have = (usize::MAX, usize::MAX);
        for row in 0..dh {
            let f = (((row as u64 * 2 + 1) * c.h as u64 * 256) / (dh as u64 * 2)).saturating_sub(128);
            let (y0, wy) = (((f >> 8) as usize).min(c.h - 1), (f & 255) as u32);
            let y1 = (y0 + 1).min(c.h - 1);
            if have != (y0, y1) {
                if have.1 == y0 {
                    core::mem::swap(&mut a, &mut b);
                } else {
                    hrow(y0, &mut a);
                }
                hrow(y1, &mut b);
                have = (y0, y1);
            }
            for i in 0..dw {
                let p = if wy == 0 { a[i] } else { lerp(a[i], b[i], wy) };
                line[i] = if native { p } else { self.pack(Rgb((p >> 16) as u8, (p >> 8) as u8, p as u8)) };
            }
            let d = unsafe { self.base.add((dy + row) * self.pitch + dx * 4) as *mut u32 };
            unsafe { ptr::copy_nonoverlapping(line.as_ptr(), d, dw) };
        }
    }

    /// Let the crash screen draw here too.
    pub fn register_for_crashes(&self) {
        crate::crash::set_framebuffer(self.base, self.width, self.height, self.pitch, self.r_shift, self.g_shift, self.b_shift);
    }

    /// Convert an RGB colour into the framebuffer's native pixel format.
    pub fn pack(&self, c: Rgb) -> u32 {
        ((c.0 as u32) << self.r_shift) | ((c.1 as u32) << self.g_shift) | ((c.2 as u32) << self.b_shift)
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

    /// Copy an off-screen canvas (0x00RRGGBB pixels, same size as the screen) to the framebuffer.
    pub fn blit(&self, c: &crate::gfx::Canvas) {
        let w = c.w.min(self.width);
        let h = c.h.min(self.height);
        let native = self.r_shift == 16 && self.g_shift == 8 && self.b_shift == 0;
        for row in 0..h {
            let src = &c.px[row * c.w..row * c.w + w];
            let dst = unsafe { self.base.add(row * self.pitch) as *mut u32 };
            if native {
                unsafe { ptr::copy_nonoverlapping(src.as_ptr(), dst, w) };
            } else {
                for (i, &p) in src.iter().enumerate() {
                    let px = self.pack(Rgb((p >> 16) as u8, (p >> 8) as u8, p as u8));
                    unsafe { ptr::write_volatile(dst.add(i), px) };
                }
            }
        }
    }

    /// Copy a canvas drawn at a smaller "logical" size, enlarging it by `scale` halves
    /// (2 = 1x, 3 = 1.5x, 4 = 2x) with nearest-neighbour sampling.
    pub fn blit_scaled(&self, c: &crate::gfx::Canvas, scale: u32) {
        if scale <= 2 {
            return self.blit(c);
        }
        let s = scale as usize;
        let xmap: alloc::vec::Vec<usize> = (0..self.width).map(|x| (x * 2 / s).min(c.w - 1)).collect();
        let native = self.r_shift == 16 && self.g_shift == 8 && self.b_shift == 0;
        let mut line = alloc::vec![0u32; self.width];
        let mut last_src = usize::MAX;
        for row in 0..self.height {
            let sy = (row * 2 / s).min(c.h - 1);
            if sy != last_src {
                let src = &c.px[sy * c.w..(sy + 1) * c.w];
                for (d, &sx) in line.iter_mut().zip(&xmap) {
                    let p = src[sx];
                    *d = if native { p } else { self.pack(Rgb((p >> 16) as u8, (p >> 8) as u8, p as u8)) };
                }
                last_src = sy;
            }
            let dst = unsafe { self.base.add(row * self.pitch) as *mut u32 };
            unsafe { ptr::copy_nonoverlapping(line.as_ptr(), dst, self.width) };
        }
    }
}

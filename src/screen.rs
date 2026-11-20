//! Minimal 32-bpp linear framebuffer access.

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

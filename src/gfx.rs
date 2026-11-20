//! Off-screen drawing surface (0x00RRGGBB pixels) with shapes and text.
//! Everything is drawn here first, then copied to the real framebuffer in one go.

use alloc::vec;
use alloc::vec::Vec;
use noto_sans_mono_bitmap::{get_raster, get_raster_width, FontWeight, RasterHeight};

pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// Linear mix: t = 0 gives `a`, t = 255 gives `b`.
#[inline]
pub fn mix(a: u32, b: u32, t: u32) -> u32 {
    let ch = |s: u32| {
        let x = (a >> s) & 0xFF;
        let y = (b >> s) & 0xFF;
        ((x * (255 - t) + y * t) / 255) << s
    };
    ch(16) | ch(8) | ch(0)
}

#[derive(Clone, Copy)]
pub enum Size {
    Small,  // 16 px
    Medium, // 20 px
    Large,  // 32 px
}

impl Size {
    fn raster(self) -> RasterHeight {
        match self {
            Size::Small => RasterHeight::Size16,
            Size::Medium => RasterHeight::Size20,
            Size::Large => RasterHeight::Size32,
        }
    }
}

fn weight(bold: bool) -> FontWeight {
    if bold {
        FontWeight::Bold
    } else {
        FontWeight::Regular
    }
}

pub fn text_width(s: &str, size: Size, bold: bool) -> i32 {
    (s.chars().count() * get_raster_width(weight(bold), size.raster())) as i32
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Rect {
    pub x: i32,
    pub y: i32,
    pub w: i32,
    pub h: i32,
}

impl Rect {
    pub const fn new(x: i32, y: i32, w: i32, h: i32) -> Self {
        Self { x, y, w, h }
    }
    pub fn contains(&self, px: i32, py: i32) -> bool {
        px >= self.x && py >= self.y && px < self.x + self.w && py < self.y + self.h
    }
}

pub struct Canvas {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
    /// Drawing is restricted to this rectangle (x0, y0, x1, y1).
    clip_box: (i32, i32, i32, i32),
}

impl Canvas {
    pub fn new(w: usize, h: usize) -> Self {
        Self { w, h, px: vec![0; w * h], clip_box: (0, 0, w as i32, h as i32) }
    }

    /// Restrict all drawing to `r` (intersected with the canvas).
    pub fn set_clip(&mut self, r: Rect) {
        self.clip_box = (r.x.max(0), r.y.max(0), (r.x + r.w).min(self.w as i32), (r.y + r.h).min(self.h as i32));
    }

    pub fn reset_clip(&mut self) {
        self.clip_box = (0, 0, self.w as i32, self.h as i32);
    }

    #[inline]
    fn idx(&self, x: i32, y: i32) -> Option<usize> {
        let (x0, y0, x1, y1) = self.clip_box;
        if x >= x0 && y >= y0 && x < x1 && y < y1 {
            Some(y as usize * self.w + x as usize)
        } else {
            None
        }
    }

    /// Draw an image (0xAARRGGBB) with its top-left at (x, y), alpha-blending if it has transparency.
    pub fn draw_image(&mut self, img: &crate::image::Image, x: i32, y: i32) {
        let Some((x0, y0, x1, y1)) = self.clip(x, y, img.w as i32, img.h as i32) else { return };
        for row in y0..y1 {
            let sy = (row as i32 - y) as usize;
            let sx = (x0 as i32 - x) as usize;
            let src = &img.px[sy * img.w + sx..sy * img.w + sx + (x1 - x0)];
            let dst = &mut self.px[row * self.w + x0..row * self.w + x1];
            if img.opaque {
                for (d, s) in dst.iter_mut().zip(src) {
                    *d = s & 0xFF_FFFF;
                }
            } else {
                for (d, &s) in dst.iter_mut().zip(src) {
                    let a = s >> 24;
                    if a == 255 {
                        *d = s & 0xFF_FFFF;
                    } else if a > 0 {
                        *d = mix(*d, s & 0xFF_FFFF, a);
                    }
                }
            }
        }
    }

    #[inline]
    pub fn blend(&mut self, x: i32, y: i32, c: u32, alpha: u32) {
        if let Some(i) = self.idx(x, y) {
            self.px[i] = if alpha >= 255 { c } else { mix(self.px[i], c, alpha) };
        }
    }

    /// Clip a rectangle to the canvas. Returns (x0, y0, x1, y1) or None if empty.
    fn clip(&self, x: i32, y: i32, w: i32, h: i32) -> Option<(usize, usize, usize, usize)> {
        let (cx0, cy0, cx1, cy1) = self.clip_box;
        let x0 = x.max(cx0);
        let y0 = y.max(cy0);
        let x1 = (x + w).min(cx1);
        let y1 = (y + h).min(cy1);
        if x0 >= x1 || y0 >= y1 {
            None
        } else {
            Some((x0 as usize, y0 as usize, x1 as usize, y1 as usize))
        }
    }

    pub fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: u32) {
        if let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) {
            for row in y0..y1 {
                self.px[row * self.w + x0..row * self.w + x1].fill(c);
            }
        }
    }

    pub fn fill_rect_alpha(&mut self, x: i32, y: i32, w: i32, h: i32, c: u32, alpha: u32) {
        if let Some((x0, y0, x1, y1)) = self.clip(x, y, w, h) {
            for row in y0..y1 {
                for p in &mut self.px[row * self.w + x0..row * self.w + x1] {
                    *p = mix(*p, c, alpha);
                }
            }
        }
    }

    pub fn gradient_v(&mut self, x: i32, y: i32, w: i32, h: i32, top: u32, bottom: u32) {
        for row in 0..h {
            let t = (row * 255 / (h - 1).max(1)) as u32;
            self.fill_rect(x, y + row, w, 1, mix(top, bottom, t));
        }
    }

    /// How far a rounded corner of radius r eats into row j (0 = outermost row).
    fn corner_inset(r: i32, j: i32) -> i32 {
        let dy = 2 * r - 2 * j - 1;
        for i in 0..r {
            let dx = 2 * r - 2 * i - 1;
            if dx * dx + dy * dy <= 4 * r * r {
                return i;
            }
        }
        r
    }

    pub fn round_rect_alpha(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: u32, alpha: u32) {
        let r = r.min(w / 2).min(h / 2);
        for j in 0..h {
            let inset = if j < r {
                Self::corner_inset(r, j)
            } else if j >= h - r {
                Self::corner_inset(r, h - 1 - j)
            } else {
                0
            };
            self.fill_rect_alpha(x + inset, y + j, w - 2 * inset, 1, c, alpha);
        }
    }

    pub fn round_rect(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: u32) {
        self.round_rect_alpha(x, y, w, h, r, c, 255);
    }

    pub fn outline_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: u32) {
        self.fill_rect(x, y, w, 1, c);
        self.fill_rect(x, y + h - 1, w, 1, c);
        self.fill_rect(x, y, 1, h, c);
        self.fill_rect(x + w - 1, y, 1, h, c);
    }

    pub fn circle(&mut self, cx: i32, cy: i32, r: i32, c: u32) {
        self.circle_alpha(cx, cy, r, c, 255);
    }

    pub fn circle_alpha(&mut self, cx: i32, cy: i32, r: i32, c: u32, alpha: u32) {
        for dy in -r..=r {
            for dx in -r..=r {
                if dx * dx + dy * dy <= r * r {
                    self.blend(cx + dx, cy + dy, c, alpha);
                }
            }
        }
    }

    pub fn triangle(&mut self, p: [(i32, i32); 3], c: u32) {
        let minx = p.iter().map(|q| q.0).min().unwrap();
        let maxx = p.iter().map(|q| q.0).max().unwrap();
        let miny = p.iter().map(|q| q.1).min().unwrap();
        let maxy = p.iter().map(|q| q.1).max().unwrap();
        let edge = |a: (i32, i32), b: (i32, i32), x: i32, y: i32| (b.0 - a.0) * (y - a.1) - (b.1 - a.1) * (x - a.0);
        for y in miny..=maxy {
            for x in minx..=maxx {
                let e0 = edge(p[0], p[1], x, y);
                let e1 = edge(p[1], p[2], x, y);
                let e2 = edge(p[2], p[0], x, y);
                if (e0 >= 0 && e1 >= 0 && e2 >= 0) || (e0 <= 0 && e1 <= 0 && e2 <= 0) {
                    self.blend(x, y, c, 255);
                }
            }
        }
    }

    /// Draw text with its top-left at (x, y). Returns the width drawn.
    pub fn text(&mut self, x: i32, y: i32, s: &str, size: Size, bold: bool, c: u32) -> i32 {
        let mut cx = x;
        for ch in s.chars() {
            let g = get_raster(ch, weight(bold), size.raster()).or_else(|| get_raster('?', weight(bold), size.raster()));
            if let Some(g) = g {
                for (gy, row) in g.rows().enumerate() {
                    for (gx, &a) in row.iter().enumerate() {
                        if a > 0 {
                            self.blend(cx + gx as i32, y + gy as i32, c, a as u32);
                        }
                    }
                }
                cx += g.width() as i32;
            }
        }
        cx - x
    }

    pub fn text_centered(&mut self, cx: i32, y: i32, s: &str, size: Size, bold: bool, c: u32) {
        let w = text_width(s, size, bold);
        self.text(cx - w / 2, y, s, size, bold, c);
    }
}

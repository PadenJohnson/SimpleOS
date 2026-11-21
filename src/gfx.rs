//! Off-screen drawing surface (0x00RRGGBB pixels) with shapes and text.
//! Everything is drawn here first, then copied to the real framebuffer in one go.

use alloc::vec;
use alloc::vec::Vec;
use crate::font::Face;

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
    Small,  // 16 px box
    Medium, // 20 px box
    Large,  // 32 px box
}

impl Size {
    /// The font's pixel size (em).
    pub fn px(self) -> u16 {
        match self {
            Size::Small => 15,
            Size::Medium => 18,
            Size::Large => 29,
        }
    }
    /// Baseline below the top of the text box, so text sits centred where it always has.
    pub fn baseline(self) -> i32 {
        let h = match self {
            Size::Small => 16,
            Size::Medium => 20,
            Size::Large => 32,
        };
        // Centre the capitals (0.714 em tall) in the box.
        h / 2 + (self.px() as i32 * 357 + 500) / 1000
    }
}

pub fn text_width(s: &str, size: Size, bold: bool) -> i32 {
    crate::font::width(s, if bold { Face::SansBold } else { Face::Sans }, size.px())
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

#[derive(Clone)]
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

    /// A filled rounded rectangle with smooth (anti-aliased) corners.
    pub fn round_rect_alpha(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, c: u32, alpha: u32) {
        let r = r.min(w / 2).min(h / 2).max(0);
        if r == 0 {
            self.fill_rect_alpha(x, y, w, h, c, alpha);
            return;
        }
        self.fill_rect_alpha(x, y + r, w, h - 2 * r, c, alpha);
        for j in 0..r {
            self.fill_rect_alpha(x + r, y + j, w - 2 * r, 1, c, alpha);
            self.fill_rect_alpha(x + r, y + h - 1 - j, w - 2 * r, 1, c, alpha);
            for i in 0..r {
                let (dx, dy) = (r * 16 - (i * 16 + 8), r * 16 - (j * 16 + 8));
                let cov = (r * 16 - isqrt(dx * dx + dy * dy) + 8).clamp(0, 16) as u32;
                if cov > 0 {
                    let a = alpha * cov / 16;
                    self.blend(x + i, y + j, c, a);
                    self.blend(x + w - 1 - i, y + j, c, a);
                    self.blend(x + i, y + h - 1 - j, c, a);
                    self.blend(x + w - 1 - i, y + h - 1 - j, c, a);
                }
            }
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

    /// A filled circle with a smooth edge.
    pub fn circle_alpha(&mut self, cx: i32, cy: i32, r: i32, c: u32, alpha: u32) {
        let inner = (r - 1).max(0) * (r - 1).max(0);
        for dy in -r - 1..=r + 1 {
            for dx in -r - 1..=r + 1 {
                let d2 = dx * dx + dy * dy;
                if d2 <= inner {
                    self.blend(cx + dx, cy + dy, c, alpha);
                } else {
                    let cov = (r * 16 + 8 - isqrt(d2 * 256)).clamp(0, 16) as u32;
                    if cov > 0 {
                        self.blend(cx + dx, cy + dy, c, alpha * cov / 16);
                    }
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

    /// Draw text with its top-left at (x, y) (the box is 16, 20 or 32 px tall for Small,
    /// Medium and Large). Returns the width drawn.
    pub fn text(&mut self, x: i32, y: i32, s: &str, size: Size, bold: bool, c: u32) -> i32 {
        let face = if bold { Face::SansBold } else { Face::Sans };
        self.text_face(x * 64, y + size.baseline(), s, face, size.px(), c) / 64
    }

    /// Draw text in any face and pixel size with its baseline at `base`, starting at x64
    /// (in 1/64 px). Returns the advance in 1/64 px.
    pub fn text_face(&mut self, x64: i32, base: i32, s: &str, face: Face, px: u16, c: u32) -> i32 {
        let Some(mut fonts) = crate::font::FONTS.try_lock() else { return 0 };
        let mut pen = x64;
        for ch in s.chars() {
            let g = fonts.glyph(face, ch, px);
            if g.w > 0 {
                let gx = (pen + 32).div_euclid(64) + g.xmin;
                let gy = base - g.ymin - g.h as i32;
                self.coverage(gx, gy, g.w, g.h, &g.cov, c);
            }
            pen += g.adv;
        }
        pen - x64
    }

    /// Blend colour `c` through an 8-bit coverage mask whose top-left is at (x, y).
    pub fn coverage(&mut self, x: i32, y: i32, w: usize, h: usize, cov: &[u8], c: u32) {
        let Some((x0, y0, x1, y1)) = self.clip(x, y, w as i32, h as i32) else { return };
        for row in y0..y1 {
            let sy = (row as i32 - y) as usize;
            let src = &cov[sy * w + (x0 as i32 - x) as usize..sy * w + (x1 as i32 - x) as usize];
            let dst = &mut self.px[row * self.w + x0..row * self.w + x1];
            for (d, &a) in dst.iter_mut().zip(src) {
                if a == 255 {
                    *d = c;
                } else if a > 0 {
                    *d = mix(*d, c, a as u32);
                }
            }
        }
    }
    pub fn text_centered(&mut self, cx: i32, y: i32, s: &str, size: Size, bold: bool, c: u32) {
        let w = text_width(s, size, bold);
        self.text(cx - w / 2, y, s, size, bold, c);
    }
}

/// Integer square root (rounded down).
pub fn isqrt(n: i32) -> i32 {
    if n <= 0 {
        return 0;
    }
    let n = n as u32;
    let mut x = n;
    let mut y = (x + 1) / 2;
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x as i32
}

impl Canvas {
    /// A soft drop shadow around the rounded rectangle (x, y, w, h): it fades out over `blur`
    /// pixels. Only the band around the edge is drawn (the inside will be covered anyway).
    pub fn shadow(&mut self, x: i32, y: i32, w: i32, h: i32, r: i32, blur: i32, alpha: u32) {
        let r = r.min(w / 2).min(h / 2).max(0);
        let blur = blur.max(1);
        let (cx2, cy2) = (2 * x + w, 2 * y + h);
        let (x0, y0, x1, y1) = (x - blur, y - blur, x + w + blur, y + h + blur);
        let seg = |c: &mut Canvas, py: i32, xa: i32, xb: i32| {
            for px in xa..xb {
                let qx = (2 * px + 1 - cx2).abs() - (w - 2 * r);
                let qy = (2 * py + 1 - cy2).abs() - (h - 2 * r);
                let (ox, oy) = (qx.max(0), qy.max(0));
                let d = isqrt(ox * ox + oy * oy) + qx.max(qy).min(0) - 2 * r; // half pixels
                if d >= 2 * blur {
                    continue;
                }
                let a = if d <= 0 {
                    alpha
                } else {
                    let t = (2 * blur - d) as u32;
                    alpha * t * t / (4 * (blur * blur) as u32)
                };
                if a > 0 {
                    c.blend(px, py, 0, a);
                }
            }
        };
        for py in y0..y1 {
            if py >= y + r && py < y + h - r {
                seg(self, py, x0, x + r + 1);
                seg(self, py, x + w - r - 1, x1);
            } else {
                seg(self, py, x0, x1);
            }
        }
    }
}

/// Monospaced text (the built-in Noto Sans Mono bitmaps), for the text editor: 16, 20 or 32 px.
pub fn mono_raster(px: u8) -> noto_sans_mono_bitmap::RasterHeight {
    use noto_sans_mono_bitmap::RasterHeight;
    match px {
        0..=17 => RasterHeight::Size16,
        18..=25 => RasterHeight::Size20,
        _ => RasterHeight::Size32,
    }
}

/// Width of one monospaced character cell.
pub fn mono_width(px: u8) -> i32 {
    noto_sans_mono_bitmap::get_raster_width(noto_sans_mono_bitmap::FontWeight::Regular, mono_raster(px)) as i32
}

impl Canvas {
    /// One monospaced character with its cell's top-left at (x, y).
    pub fn mono_char(&mut self, x: i32, y: i32, ch: char, px: u8, c: u32) {
        use noto_sans_mono_bitmap::{get_raster, FontWeight};
        let g = get_raster(ch, FontWeight::Regular, mono_raster(px)).or_else(|| get_raster('?', FontWeight::Regular, mono_raster(px)));
        if let Some(g) = g {
            for (gy, row) in g.rows().enumerate() {
                for (gx, &a) in row.iter().enumerate() {
                    if a > 0 {
                        self.blend(x + gx as i32, y + gy as i32, c, a as u32);
                    }
                }
            }
        }
    }
}

impl Canvas {
    /// Copy another canvas in with its top-left at (x, y) (clipped).
    pub fn blit(&mut self, src: &Canvas, x: i32, y: i32) {
        let Some((x0, y0, x1, y1)) = self.clip(x, y, src.w as i32, src.h as i32) else { return };
        for row in y0..y1 {
            let sy = (row as i32 - y) as usize;
            let sx = (x0 as i32 - x) as usize;
            let s = &src.px[sy * src.w + sx..sy * src.w + sx + (x1 - x0)];
            self.px[row * self.w + x0..row * self.w + x1].copy_from_slice(s);
        }
    }
}

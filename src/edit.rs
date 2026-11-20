//! Photo edits, in the spirit of Lightroom: a small, non-destructive description of what to
//! do to a photo. The same `Edits` are applied to a screen-sized preview while editing and
//! to the full-resolution original when saving.
//!
//! Order: rotate/flip -> straighten -> crop -> exposure & white balance (in linear light)
//! -> tone curve (contrast, highlights, shadows, whites, blacks, fade) -> clarity and
//! sharpening (local contrast on brightness) -> vibrance & saturation -> black & white
//! -> vignette.

use alloc::vec;
use alloc::vec::Vec;
use spin::Lazy;

use crate::image::Image;

/// Crop rectangle in units of 1/10000 of the (rotated, straightened) image's width/height.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

pub const FULL: u32 = 10_000;

impl Crop {
    pub const ALL: Crop = Crop { x: 0, y: 0, w: FULL, h: FULL };
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Edits {
    /// Quarter turns clockwise (0..=3).
    pub turns: u8,
    /// Mirror left-right (applied after rotating).
    pub flip: bool,
    /// Small rotation to level the horizon, in tenths of a degree (-450..=450).
    pub straighten: i32,
    pub crop: Crop,
    // Light, each -100..=100
    pub exposure: i32,
    pub contrast: i32,
    pub highlights: i32,
    pub shadows: i32,
    pub whites: i32,
    pub blacks: i32,
    // Color, each -100..=100
    pub temperature: i32,
    pub tint: i32,
    pub vibrance: i32,
    pub saturation: i32,
    // Effects
    /// Local contrast, -100..=100.
    pub clarity: i32,
    /// Darker (negative) or lighter (positive) corners, -100..=100.
    pub vignette: i32,
    /// 0..=100.
    pub sharpen: i32,
    /// Lifted blacks for a matte, film look, 0..=100.
    pub fade: i32,
    pub bw: bool,
    /// The preset last picked (0 = none), to show it as chosen.
    pub preset: u8,
}

impl Default for Edits {
    fn default() -> Self {
        Edits {
            turns: 0,
            flip: false,
            straighten: 0,
            crop: Crop::ALL,
            exposure: 0,
            contrast: 0,
            highlights: 0,
            shadows: 0,
            whites: 0,
            blacks: 0,
            temperature: 0,
            tint: 0,
            vibrance: 0,
            saturation: 0,
            clarity: 0,
            vignette: 0,
            sharpen: 0,
            fade: 0,
            bw: false,
            preset: 0,
        }
    }
}

impl Edits {
    pub fn is_original(&self) -> bool {
        Edits { preset: 0, ..*self } == Edits::default()
    }

    /// The same look with no rotation, straightening or crop.
    pub fn look(&self) -> Edits {
        Edits { turns: 0, flip: false, straighten: 0, crop: Crop::ALL, ..*self }
    }

    fn has_tone(&self) -> bool {
        self.look() != Edits { preset: self.preset, ..Edits::default() }.look()
    }

    /// Rotate clockwise; the crop turns with the picture.
    pub fn rotate(&mut self, clockwise: bool) {
        let c = self.crop;
        self.crop = if clockwise {
            Crop { x: FULL - c.y - c.h, y: c.x, w: c.h, h: c.w }
        } else {
            Crop { x: c.y, y: FULL - c.x - c.w, w: c.h, h: c.w }
        };
        // Mirroring then rotating clockwise equals rotating counter-clockwise then mirroring.
        let cw = clockwise != self.flip;
        self.turns = (self.turns + if cw { 1 } else { 3 }) % 4;
    }

    pub fn mirror(&mut self) {
        self.flip = !self.flip;
        self.crop.x = FULL - self.crop.x - self.crop.w;
        self.straighten = -self.straighten;
    }
}

// ---- presets ---------------------------------------------------------------------------

pub struct Preset {
    pub name: &'static str,
    pub set: fn(&mut Edits),
}

/// Set the look (keeping the framing) to defaults, then the preset's values.
fn base(e: &mut Edits) {
    *e = Edits { turns: e.turns, flip: e.flip, straighten: e.straighten, crop: e.crop, ..Edits::default() };
}

pub const PRESETS: [Preset; 12] = [
    Preset { name: "Original", set: |e| base(e) },
    Preset { name: "Vivid", set: |e| { base(e); e.contrast = 20; e.vibrance = 45; e.saturation = 8; e.clarity = 15; } },
    Preset { name: "Warm sun", set: |e| { base(e); e.temperature = 32; e.tint = 6; e.vibrance = 18; e.highlights = -15; e.shadows = 10; } },
    Preset { name: "Cool", set: |e| { base(e); e.temperature = -28; e.contrast = 10; e.vibrance = 10; } },
    Preset { name: "Soft portrait", set: |e| { base(e); e.contrast = -10; e.clarity = -25; e.shadows = 20; e.highlights = -10; e.vibrance = 8; e.temperature = 8; } },
    Preset { name: "Dramatic", set: |e| { base(e); e.contrast = 35; e.highlights = -45; e.shadows = 30; e.whites = 15; e.blacks = -20; e.clarity = 45; e.vibrance = 10; e.vignette = -35; } },
    Preset { name: "Matte", set: |e| { base(e); e.fade = 35; e.contrast = -12; e.saturation = -15; e.highlights = -10; } },
    Preset { name: "Vintage", set: |e| { base(e); e.fade = 28; e.temperature = 25; e.tint = 8; e.saturation = -30; e.contrast = -8; e.vignette = -25; } },
    Preset { name: "Faded film", set: |e| { base(e); e.fade = 40; e.saturation = -20; e.temperature = 10; e.shadows = 15; e.highlights = -15; } },
    Preset { name: "Bright & airy", set: |e| { base(e); e.exposure = 18; e.highlights = -25; e.shadows = 35; e.contrast = -15; e.vibrance = 10; e.temperature = -5; } },
    Preset { name: "Black & white", set: |e| { base(e); e.bw = true; e.contrast = 15; e.clarity = 15; } },
    Preset { name: "B&W punchy", set: |e| { base(e); e.bw = true; e.contrast = 45; e.blacks = -25; e.whites = 20; e.clarity = 30; e.vignette = -20; } },
];

pub fn apply_preset(e: &mut Edits, i: usize) {
    (PRESETS[i].set)(e);
    e.preset = i as u8;
}

// ---- geometry ----------------------------------------------------------------------------

/// Size of the image after rotating.
pub fn turned_size(w: usize, h: usize, turns: u8) -> (usize, usize) {
    if turns % 2 == 1 {
        (h, w)
    } else {
        (w, h)
    }
}

/// Rotate + mirror (no crop, no color).
pub fn orient(src: &Image, turns: u8, flip: bool) -> Image {
    if turns == 0 && !flip {
        return Image { w: src.w, h: src.h, px: src.px.clone(), opaque: src.opaque };
    }
    let (w, h) = turned_size(src.w, src.h, turns);
    let mut px = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let dx = if flip { w - 1 - x } else { x };
            // (dx, y) in the rotated image -> source coordinates.
            let (sx, sy) = match turns % 4 {
                0 => (dx, y),
                1 => (y, src.h - 1 - dx),
                2 => (src.w - 1 - dx, src.h - 1 - y),
                _ => (src.w - 1 - y, dx),
            };
            px[y * w + x] = src.px[sy * src.w + sx];
        }
    }
    Image { w, h, px, opaque: src.opaque }
}

/// Rotate by a small angle (tenths of a degree) and zoom in just enough that no empty
/// corners show. The size stays the same.
pub fn straighten(src: &Image, tenths: i32) -> Image {
    if tenths == 0 || src.w < 2 || src.h < 2 {
        return Image { w: src.w, h: src.h, px: src.px.clone(), opaque: src.opaque };
    }
    let a = tenths as f32 / 10.0 * core::f32::consts::PI / 180.0;
    let (s, c) = (libm::sinf(a), libm::cosf(a));
    let (w, h) = (src.w as f32, src.h as f32);
    let (sa, ca) = (s.abs(), c.abs());
    // Largest same-shaped rectangle inside the rotated picture.
    let scale = (w / (w * ca + h * sa)).min(h / (w * sa + h * ca));
    let (cx, cy) = ((w - 1.0) / 2.0, (h - 1.0) / 2.0);
    let mut px = vec![0u32; src.w * src.h];
    for y in 0..src.h {
        let dy = (y as f32 - cy) * scale;
        for x in 0..src.w {
            let dx = (x as f32 - cx) * scale;
            // Turn back into source coordinates.
            let sx = c * dx + s * dy + cx;
            let sy = -s * dx + c * dy + cy;
            px[y * src.w + x] = bilinear(src, sx, sy);
        }
    }
    Image { w: src.w, h: src.h, px, opaque: src.opaque }
}

fn bilinear(img: &Image, x: f32, y: f32) -> u32 {
    let x = x.clamp(0.0, (img.w - 1) as f32);
    let y = y.clamp(0.0, (img.h - 1) as f32);
    let (x0, y0) = (x as usize, y as usize);
    let (x1, y1) = ((x0 + 1).min(img.w - 1), (y0 + 1).min(img.h - 1));
    let (fx, fy) = (((x - x0 as f32) * 256.0) as u32, ((y - y0 as f32) * 256.0) as u32);
    let p = |xx: usize, yy: usize| img.px[yy * img.w + xx];
    let (a, b, cc, d) = (p(x0, y0), p(x1, y0), p(x0, y1), p(x1, y1));
    let mut out = 0u32;
    for shift in [0u32, 8, 16, 24] {
        let ch = |v: u32| (v >> shift) & 0xFF;
        let top = ch(a) * (256 - fx) + ch(b) * fx;
        let bot = ch(cc) * (256 - fx) + ch(d) * fx;
        let v = (top * (256 - fy) + bot * fy) >> 16;
        out |= v.min(255) << shift;
    }
    out
}

/// Crop rectangle in pixels for an image of size (w, h).
pub fn crop_pixels(c: Crop, w: usize, h: usize) -> (usize, usize, usize, usize) {
    let x = (c.x as usize * w / FULL as usize).min(w - 1);
    let y = (c.y as usize * h / FULL as usize).min(h - 1);
    let cw = ((c.w as usize * w + FULL as usize / 2) / FULL as usize).clamp(1, w - x);
    let ch = ((c.h as usize * h + FULL as usize / 2) / FULL as usize).clamp(1, h - y);
    (x, y, cw, ch)
}

pub fn crop(src: Image, c: Crop) -> Image {
    if c == Crop::ALL {
        return src;
    }
    let (x, y, w, h) = crop_pixels(c, src.w, src.h);
    let mut px = Vec::with_capacity(w * h);
    for row in y..y + h {
        px.extend_from_slice(&src.px[row * src.w + x..row * src.w + x + w]);
    }
    Image { w, h, px, opaque: src.opaque }
}

/// Rotate, mirror, straighten and (unless `with_crop` is false) crop.
pub fn geometry(src: &Image, e: &Edits, with_crop: bool) -> Image {
    let img = straighten(&orient(src, e.turns, e.flip), e.straighten);
    if with_crop {
        crop(img, e.crop)
    } else {
        img
    }
}

// ---- light and color ------------------------------------------------------------------------

/// sRGB value -> linear light (0..=4095).
static TO_LINEAR: Lazy<[u16; 256]> = Lazy::new(|| {
    let mut t = [0u16; 256];
    for (v, o) in t.iter_mut().enumerate() {
        let c = v as f32 / 255.0;
        let l = if c <= 0.04045 { c / 12.92 } else { libm::powf((c + 0.055) / 1.055, 2.4) };
        *o = (l * 4095.0 + 0.5) as u16;
    }
    t
});

/// Linear light (0..=4095) -> sRGB value.
static TO_SRGB: Lazy<[u8; 4096]> = Lazy::new(|| {
    let mut t = [0u8; 4096];
    for (v, o) in t.iter_mut().enumerate() {
        let l = v as f32 / 4095.0;
        let c = if l <= 0.003_130_8 { l * 12.92 } else { 1.055 * libm::powf(l, 1.0 / 2.4) - 0.055 };
        *o = (c * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    }
    t
});

/// Three 256-entry tables (red, green, blue) doing exposure, white balance, the tone curve
/// and fade in one lookup per channel.
fn tone_tables(e: &Edits) -> [[u8; 256]; 3] {
    let ev = e.exposure as f32 / 100.0 * 2.0; // up to ±2 stops
    let gain = libm::powf(2.0, ev);
    let t = e.temperature as f32 / 100.0;
    let m = e.tint as f32 / 100.0;
    let gains = [gain * (1.0 + 0.42 * t + 0.10 * m), gain * (1.0 - 0.24 * m), gain * (1.0 - 0.42 * t + 0.10 * m)];
    // Tone curve on 0..1 values.
    let (c, hi, sh, wh, bl, fade) = (
        e.contrast as f32 / 100.0,
        e.highlights as f32 / 100.0,
        e.shadows as f32 / 100.0,
        e.whites as f32 / 100.0,
        e.blacks as f32 / 100.0,
        e.fade as f32 / 100.0,
    );
    let mut curve = [0u8; 256];
    for (v, o) in curve.iter_mut().enumerate() {
        let x = v as f32 / 255.0;
        let inv = 1.0 - x;
        // Each control works mostly on its own part of the range.
        let mut y = x
            + 0.30 * sh * 6.75 * x * inv * inv
            + 0.30 * hi * 6.75 * x * x * inv
            + 0.18 * bl * inv * inv * inv * inv
            + 0.18 * wh * x * x * x * x;
        y = y.clamp(0.0, 1.0);
        // Contrast: an S-curve around the middle (or a flatter one).
        let s = y * y * (3.0 - 2.0 * y);
        y += c * (s - y) * 1.1;
        y = y.clamp(0.0, 1.0);
        // Fade: lift the blacks.
        y = fade * 0.22 + y * (1.0 - fade * 0.22);
        *o = (y * 255.0 + 0.5).clamp(0.0, 255.0) as u8;
    }
    let lin = &*TO_LINEAR;
    let srgb = &*TO_SRGB;
    let mut out = [[0u8; 256]; 3];
    for ch in 0..3 {
        for v in 0..256 {
            let mut l = lin[v] as f32 * gains[ch];
            // A soft shoulder instead of hard clipping when brightening.
            if l > 3276.0 {
                let over = l - 3276.0;
                l = 3276.0 + over / (1.0 + over / 819.0);
            }
            let s = srgb[(l as usize).min(4095)];
            out[ch][v] = curve[s as usize];
        }
    }
    out
}

#[inline]
fn luma(r: i32, g: i32, b: i32) -> i32 {
    (54 * r + 183 * g + 19 * b) >> 8
}

fn luminance(img: &Image) -> Vec<u8> {
    img.px.iter().map(|&p| luma(((p >> 16) & 0xFF) as i32, ((p >> 8) & 0xFF) as i32, (p & 0xFF) as i32) as u8).collect()
}

/// Box blur (two passes horizontally and vertically: close to a Gaussian).
fn blur(src: &[u8], w: usize, h: usize, r: usize) -> Vec<u8> {
    let mut a = src.to_vec();
    if w < 2 || h < 2 {
        return a;
    }
    let mut b = vec![0u8; src.len()];
    let r = r.min(w - 1).min(h - 1).max(1);
    for _ in 0..2 {
        // Horizontal: a -> b
        for y in 0..h {
            let row = &a[y * w..(y + 1) * w];
            let mut sum: u32 = 0;
            for x in 0..=r.min(w - 1) {
                sum += row[x] as u32;
            }
            sum += row[0] as u32 * r as u32;
            for x in 0..w {
                b[y * w + x] = (sum / (2 * r as u32 + 1)) as u8;
                let add = row[(x + r + 1).min(w - 1)] as u32;
                let sub = row[x.saturating_sub(r)] as u32;
                sum = sum + add - sub;
            }
        }
        // Vertical: b -> a
        for x in 0..w {
            let mut sum: u32 = 0;
            for y in 0..=r.min(h - 1) {
                sum += b[y * w + x] as u32;
            }
            sum += b[x] as u32 * r as u32;
            for y in 0..h {
                a[y * w + x] = (sum / (2 * r as u32 + 1)) as u8;
                let add = b[(y + r + 1).min(h - 1) * w + x] as u32;
                let sub = b[y.saturating_sub(r) * w + x] as u32;
                sum = sum + add - sub;
            }
        }
    }
    a
}

/// What clarity and sharpening need from a picture: its brightness, and blurred versions
/// of it (computed once per framing, reused while sliders move).
pub struct Detail {
    lum: Vec<u8>,
    wide: Vec<u8>,
    fine: Vec<u8>,
}

impl Detail {
    pub fn of(img: &Image) -> Detail {
        let lum = luminance(img);
        let r = (img.w.max(img.h) / 70).clamp(3, 120);
        let wide = blur(&lum, img.w, img.h, r);
        let fine = blur(&lum, img.w, img.h, 1);
        Detail { lum, wide, fine }
    }
}

/// Light, color and effects, in place. `detail` (from the same image before this call) is
/// needed for clarity and sharpening; without it they're skipped.
pub fn render(img: &mut Image, e: &Edits, detail: Option<&Detail>) {
    if !e.has_tone() {
        return;
    }
    let lut = tone_tables(e);
    let clarity = e.clarity * 140 / 100; // -140..140 (/100)
    let sharp = e.sharpen * 200 / 100; // 0..200 (/100)
    let local = detail.filter(|d| d.lum.len() == img.px.len() && (clarity != 0 || sharp != 0));
    let sat = e.saturation;
    let vib = e.vibrance;
    // Vignette: per-column and per-row parts of the distance from the middle.
    let vig = e.vignette;
    let (w, h) = (img.w, img.h);
    let dist = |i: usize, n: usize| -> u32 {
        let d = (i as i64 * 2 - (n as i64 - 1)).unsigned_abs() as u64 * 1024 / n.max(1) as u64; // 0..1024
        (d * d / 1024) as u32 // squared, 0..1024
    };
    let col: Vec<u32> = if vig != 0 { (0..w).map(|x| dist(x, w)).collect() } else { Vec::new() };
    for y in 0..h {
        let row_d = if vig != 0 { dist(y, h) } else { 0 };
        for x in 0..w {
            let i = y * w + x;
            let p = img.px[i];
            let a = p & 0xFF00_0000;
            let mut r = lut[0][((p >> 16) & 0xFF) as usize] as i32;
            let mut g = lut[1][((p >> 8) & 0xFF) as usize] as i32;
            let mut b = lut[2][(p & 0xFF) as usize] as i32;
            if let Some(d) = local {
                let l = d.lum[i] as i32;
                let mut delta = 0;
                if clarity != 0 {
                    // Mostly in the mid-tones.
                    let m = 255 - ((l - 128).abs() * 2).min(255);
                    delta += (l - d.wide[i] as i32) * clarity * m / (100 * 255);
                }
                if sharp != 0 {
                    delta += (l - d.fine[i] as i32) * sharp / 100;
                }
                r += delta;
                g += delta;
                b += delta;
            }
            if sat != 0 || vib != 0 {
                let y0 = luma(r, g, b);
                let mx = r.max(g).max(b);
                let mn = r.min(g).min(b);
                let s = (mx - mn).clamp(0, 255);
                // Vibrance boosts dull colors most and leaves strong ones alone.
                let f = 100 + sat + vib * (255 - s) / 255;
                let f = f.max(0);
                r = y0 + (r - y0) * f / 100;
                g = y0 + (g - y0) * f / 100;
                b = y0 + (b - y0) * f / 100;
            }
            if e.bw {
                let y0 = luma(r.clamp(0, 255), g.clamp(0, 255), b.clamp(0, 255));
                r = y0;
                g = y0;
                b = y0;
            }
            if vig != 0 {
                let d2 = (col[x] + row_d) as i32; // 0..2048
                // Smooth: starts a third of the way out, strongest in the corners.
                let m = ((d2 - 500).max(0) * 256 / 1548).min(256);
                let m = m * m / 256;
                let f = 256 + vig * m * 7 / 1000; // ±~180/256
                r = r * f / 256;
                g = g * f / 256;
                b = b * f / 256;
            }
            img.px[i] = a | (r.clamp(0, 255) as u32) << 16 | (g.clamp(0, 255) as u32) << 8 | b.clamp(0, 255) as u32;
        }
    }
}

/// Light and color without clarity or sharpening (small previews, thumbnails).
pub fn color(img: &mut Image, e: &Edits) {
    render(img, e, None);
}

/// Apply every edit.
pub fn apply(src: &Image, e: &Edits) -> Image {
    let mut img = geometry(src, e, true);
    let detail = if e.clarity != 0 || e.sharpen != 0 { Some(Detail::of(&img)) } else { None };
    render(&mut img, e, detail.as_ref());
    img
}

// ---- auto and histogram ---------------------------------------------------------------------

/// Brightness and color counts (red, green, blue, brightness), 256 bins each.
pub fn histogram(img: &Image) -> [[u32; 256]; 4] {
    let mut h = [[0u32; 256]; 4];
    let step = (img.px.len() / 300_000).max(1);
    for &p in img.px.iter().step_by(step) {
        let (r, g, b) = (((p >> 16) & 0xFF) as usize, ((p >> 8) & 0xFF) as usize, (p & 0xFF) as usize);
        h[0][r] += 1;
        h[1][g] += 1;
        h[2][b] += 1;
        h[3][luma(r as i32, g as i32, b as i32) as usize] += 1;
    }
    h
}

/// Lightroom-style Auto: settings for exposure, contrast, highlights, shadows, whites,
/// blacks, white balance, vibrance and saturation, worked out from the picture (with its
/// framing applied). Effects and framing are kept.
pub fn auto(img: &Image, e: &Edits) -> Edits {
    let hist = histogram(img);
    let total: u32 = hist[3].iter().sum::<u32>().max(1);
    let pct = |frac: u32| -> i32 {
        let target = (total as u64 * frac as u64 / 1000) as u32;
        let mut acc = 0;
        for (v, &c) in hist[3].iter().enumerate() {
            acc += c;
            if acc >= target {
                return v as i32;
            }
        }
        255
    };
    let (lo, mid, hi) = (pct(5), pct(500), pct(995));
    let bright = hist[3][235..].iter().sum::<u32>() * 100 / total; // % near white
    let lin = &*TO_LINEAR;
    // Exposure: bring the middle brightness to about 46% grey.
    let target = 118usize;
    let ev = libm::log2f(lin[target] as f32 / (lin[mid.clamp(1, 254) as usize] as f32).max(1.0));
    let exposure = ((ev / 2.0 * 100.0) as i32).clamp(-60, 70);
    // Where the darkest/brightest end up after that exposure (roughly).
    let shift = |v: i32| -> i32 {
        let l = lin[v.clamp(0, 255) as usize] as f32 * libm::powf(2.0, exposure as f32 / 50.0);
        TO_SRGB[(l as usize).min(4095)] as i32
    };
    let (lo2, hi2) = (shift(lo), shift(hi));
    let whites = ((248 - hi2) * 100 / 46).clamp(-40, 50);
    let blacks = (-(lo2 - 8) * 100 / 46).clamp(-50, 30);
    let span = hi2 - lo2;
    let contrast = ((210 - span) / 3).clamp(-15, 25);
    let shadows = if mid < 90 { 30 } else if mid < 120 { 15 } else { 5 };
    let highlights = if bright > 8 { -45 } else if bright > 3 { -25 } else { -10 };
    // White balance is left as it is (a sunset should stay warm), like Lightroom's Auto.
    let (temperature, tint) = (e.temperature, e.tint);
    kprintln!("editor: auto: mid {} lo {} hi {} -> exposure {} contrast {} hi {} sh {} wh {} bl {} temp {} tint {}", mid, lo, hi, exposure, contrast, highlights, shadows, whites, blacks, temperature, tint);
    Edits {
        exposure,
        contrast,
        highlights,
        shadows,
        whites,
        blacks,
        temperature,
        tint,
        vibrance: 25,
        saturation: 5,
        preset: 0,
        ..*e
    }
}

/// Encode as JPEG (transparent areas become white).
pub fn encode_jpeg(img: &mut Image, quality: u8) -> Result<Vec<u8>, &'static str> {
    if img.w > 65_535 || img.h > 65_535 {
        return Err("Image too large to save");
    }
    if !img.opaque {
        for p in img.px.iter_mut() {
            let a = *p >> 24;
            if a < 255 {
                *p = crate::gfx::mix(0xFF_FFFF, *p & 0xFF_FFFF, a) | 0xFF00_0000;
            }
        }
        img.opaque = true;
    }
    // Pixels are 0xAARRGGBB in memory = B, G, R, A bytes.
    let bytes = unsafe { core::slice::from_raw_parts(img.px.as_ptr() as *const u8, img.px.len() * 4) };
    let mut out = Vec::new();
    let enc = jpeg_encoder::Encoder::new(&mut out, quality);
    enc.encode(bytes, img.w as u16, img.h as u16, jpeg_encoder::ColorType::Bgra).map_err(|_| "Couldn't encode the photo")?;
    Ok(out)
}

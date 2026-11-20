//! Photo edits: a small, non-destructive description of what to do to a photo.
//! The same `Edits` are applied to a screen-sized preview while editing and to the
//! full-resolution original when saving.
//!
//! Order: rotate/flip -> crop -> light & color -> filter.

use alloc::vec;
use alloc::vec::Vec;

use crate::image::Image;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Filter {
    None,
    BlackWhite,
    Sepia,
    Vivid,
    Warm,
    Cool,
    Fade,
}

impl Filter {
    pub const ALL: [Filter; 7] = [Filter::None, Filter::BlackWhite, Filter::Sepia, Filter::Vivid, Filter::Warm, Filter::Cool, Filter::Fade];
    pub fn name(self) -> &'static str {
        match self {
            Filter::None => "Original",
            Filter::BlackWhite => "Black & white",
            Filter::Sepia => "Sepia",
            Filter::Vivid => "Vivid",
            Filter::Warm => "Warm",
            Filter::Cool => "Cool",
            Filter::Fade => "Faded",
        }
    }
}

/// Crop rectangle in units of 1/10000 of the (rotated) image's width and height.
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
    pub crop: Crop,
    /// Each -100..=100.
    pub brightness: i32,
    pub contrast: i32,
    pub saturation: i32,
    pub warmth: i32,
    pub filter: Filter,
}

impl Default for Edits {
    fn default() -> Self {
        Edits { turns: 0, flip: false, crop: Crop::ALL, brightness: 0, contrast: 0, saturation: 0, warmth: 0, filter: Filter::None }
    }
}

impl Edits {
    pub fn is_original(&self) -> bool {
        *self == Edits::default()
    }

    fn has_color(&self) -> bool {
        self.brightness != 0 || self.contrast != 0 || self.saturation != 0 || self.warmth != 0 || self.filter != Filter::None
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
    }
}

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

#[inline]
fn clamp8(v: i32) -> u32 {
    v.clamp(0, 255) as u32
}

/// Light, color and filter, in place.
pub fn color(img: &mut Image, e: &Edits) {
    if !e.has_color() {
        return;
    }
    // Filters are expressed as extra adjustments where possible.
    let (mut contrast, mut saturation, mut warmth, mut lift) = (e.contrast, e.saturation, e.warmth, 0);
    match e.filter {
        Filter::Vivid => {
            saturation += 45;
            contrast += 15;
        }
        Filter::Warm => warmth += 35,
        Filter::Cool => warmth -= 35,
        Filter::Fade => {
            saturation -= 35;
            contrast -= 15;
            lift = 30;
        }
        _ => {}
    }
    // Brightness + contrast + fade lift as one lookup table.
    let mut lut = [0u8; 256];
    for (v, out) in lut.iter_mut().enumerate() {
        let mut x = v as i32 + e.brightness * 128 / 100;
        x = (x - 128) * (100 + contrast.clamp(-100, 200)) / 100 + 128;
        x = lift + x * (255 - lift) / 255;
        *out = x.clamp(0, 255) as u8;
    }
    let sat = (100 + saturation).clamp(0, 300);
    let warm = warmth.clamp(-100, 100) * 30 / 100; // up to ±30 levels
    for p in img.px.iter_mut() {
        let a = *p & 0xFF00_0000;
        let mut r = lut[((*p >> 16) & 0xFF) as usize] as i32;
        let mut g = lut[((*p >> 8) & 0xFF) as usize] as i32;
        let mut b = lut[(*p & 0xFF) as usize] as i32;
        if sat != 100 {
            let y = (77 * r + 150 * g + 29 * b) >> 8;
            r = y + (r - y) * sat / 100;
            g = y + (g - y) * sat / 100;
            b = y + (b - y) * sat / 100;
        }
        if warm != 0 {
            r += warm;
            b -= warm;
        }
        match e.filter {
            Filter::BlackWhite => {
                let y = (77 * r.clamp(0, 255) + 150 * g.clamp(0, 255) + 29 * b.clamp(0, 255)) >> 8;
                r = y;
                g = y;
                b = y;
            }
            Filter::Sepia => {
                let (r0, g0, b0) = (r.clamp(0, 255), g.clamp(0, 255), b.clamp(0, 255));
                r = (393 * r0 + 769 * g0 + 189 * b0) / 1000;
                g = (349 * r0 + 686 * g0 + 168 * b0) / 1000;
                b = (272 * r0 + 534 * g0 + 131 * b0) / 1000;
            }
            _ => {}
        }
        *p = a | clamp8(r) << 16 | clamp8(g) << 8 | clamp8(b);
    }
}

/// Apply every edit.
pub fn apply(src: &Image, e: &Edits) -> Image {
    let mut img = crop(orient(src, e.turns, e.flip), e.crop);
    color(&mut img, e);
    img
}

/// Suggest light & color settings: stretch the darkest/brightest 1% to the full range,
/// center the mid-tones, and add a little saturation.
pub fn auto_fix(img: &Image) -> (i32, i32, i32) {
    let mut hist = [0u32; 256];
    let step = (img.px.len() / 200_000).max(1);
    let mut n = 0u32;
    for p in img.px.iter().step_by(step) {
        let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
        hist[((77 * r + 150 * g + 29 * b) >> 8) as usize] += 1;
        n += 1;
    }
    let pct = |frac: u32| -> i32 {
        let target = n * frac / 1000;
        let mut acc = 0;
        for (v, &c) in hist.iter().enumerate() {
            acc += c;
            if acc >= target {
                return v as i32;
            }
        }
        255
    };
    let (lo, hi, mid) = (pct(10), pct(990), pct(500));
    let span = (hi - lo).max(32);
    let contrast = ((255 * 100 / span) - 100).clamp(0, 60);
    // Brightness that moves the median toward 128 after the contrast stretch.
    let mid_after = (mid - 128) * (100 + contrast) / 100 + 128;
    let brightness = ((128 - mid_after) * 100 / 128 / 2).clamp(-40, 40);
    (brightness, contrast, 12)
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

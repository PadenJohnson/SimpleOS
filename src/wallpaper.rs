//! The desktop wallpapers: soft, light gradients with a few large blurred blooms of colour,
//! drawn once per screen size (integer maths, with a touch of noise so there's no banding).

use alloc::vec;
use alloc::vec::Vec;

use crate::gfx::rgb;

struct Theme {
    /// Top-left and bottom-right of the base gradient.
    a: u32,
    b: u32,
    /// Blooms: colour, centre (in 1/100 of the width/height), radius (1/100 of the width), strength 0..255.
    blooms: [(u32, i32, i32, i32, u32); 3],
    /// Colour for the clock and text drawn on it.
    ink: u32,
}

/// Same order as settings::BACKGROUNDS.
const THEMES: [Theme; 5] = [
    // Blue
    Theme { a: rgb(222, 236, 252), b: rgb(160, 194, 240), blooms: [(rgb(108, 160, 240), 12, 88, 55, 200), (rgb(255, 255, 255), 78, 14, 45, 220), (rgb(154, 132, 232), 96, 96, 48, 150)], ink: rgb(22, 40, 74) },
    // Green
    Theme { a: rgb(226, 244, 232), b: rgb(158, 212, 186), blooms: [(rgb(70, 180, 150), 10, 90, 55, 190), (rgb(255, 252, 230), 80, 12, 45, 220), (rgb(120, 200, 230), 95, 92, 45, 140)], ink: rgb(18, 58, 44) },
    // Purple
    Theme { a: rgb(240, 232, 252), b: rgb(196, 172, 236), blooms: [(rgb(236, 140, 200), 14, 86, 52, 170), (rgb(255, 255, 255), 76, 14, 45, 220), (rgb(120, 140, 240), 96, 94, 48, 160)], ink: rgb(54, 30, 88) },
    // Gray
    Theme { a: rgb(242, 244, 247), b: rgb(196, 202, 212), blooms: [(rgb(170, 182, 200), 12, 88, 55, 170), (rgb(255, 255, 255), 78, 14, 45, 230), (rgb(150, 160, 178), 96, 96, 45, 120)], ink: rgb(36, 42, 52) },
    // Warm
    Theme { a: rgb(254, 238, 224), b: rgb(242, 190, 160), blooms: [(rgb(248, 150, 120), 12, 88, 55, 180), (rgb(255, 250, 236), 78, 12, 45, 220), (rgb(236, 120, 150), 96, 94, 45, 130)], ink: rgb(84, 40, 24) },
];

pub fn ink(bg: usize) -> u32 {
    THEMES[bg.min(THEMES.len() - 1)].ink
}

fn ch(c: u32, s: u32) -> i32 {
    ((c >> s) & 0xFF) as i32
}

pub fn make(bg: usize, w: usize, h: usize) -> Vec<u32> {
    let t = &THEMES[bg.min(THEMES.len() - 1)];
    let mut px = vec![0u32; w * h];
    let (wi, hi) = (w as i64, h as i64);
    // Blooms in pixels: centre, radius squared.
    let blooms: Vec<(i64, i64, i64, [i32; 3], i32)> = t
        .blooms
        .iter()
        .map(|&(c, cx, cy, r, s)| (cx as i64 * wi / 100, cy as i64 * hi / 100, (r as i64 * wi / 100).pow(2).max(1), [ch(c, 16), ch(c, 8), ch(c, 0)], s as i32))
        .collect();
    let (a, b) = ([ch(t.a, 16), ch(t.a, 8), ch(t.a, 0)], [ch(t.b, 16), ch(t.b, 8), ch(t.b, 0)]);
    let span = (wi + hi).max(1);
    let mut seed = 0x1234_5678u32;
    for y in 0..h {
        for x in 0..w {
            // Diagonal base gradient, in 1/4096.
            let g = ((x as i64 + y as i64) * 4096 / span) as i32;
            let mut col = [0i32; 3];
            for k in 0..3 {
                col[k] = (a[k] * (4096 - g) + b[k] * g) * 16; // 1/16 levels, x4096
            }
            for &(bx, by, r2, c, s) in &blooms {
                let d2 = (x as i64 - bx).pow(2) + (y as i64 - by).pow(2);
                if d2 < r2 {
                    // Smooth falloff: (1 - d²/r²)², in 1/4096.
                    let f = (4096 - d2 * 4096 / r2) as i32;
                    let f = f * f / 4096 * s / 255;
                    for k in 0..3 {
                        col[k] += ((c[k] * 4096 * 16 - col[k]) as i64 * f as i64 / 4096) as i32;
                    }
                }
            }
            // A little noise hides banding.
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            let n = (seed & 15) as i32;
            let v = |k: usize| (((col[k] / 4096) + n) / 16).clamp(0, 255) as u32;
            px[y * w + x] = v(0) << 16 | v(1) << 8 | v(2);
        }
    }
    px
}

// ---- photo backgrounds -------------------------------------------------------------------

/// Built-in nature photos (CC0, see assets/wallpapers/CREDITS.md): name, photo, preview.
pub const PHOTOS: [(&str, &[u8], &[u8]); 5] = [
    ("Autumn", include_bytes!("../assets/wallpapers/autumn.jpg"), include_bytes!("../assets/wallpapers/thumbs/autumn.jpg")),
    ("Sunset", include_bytes!("../assets/wallpapers/sunset.jpg"), include_bytes!("../assets/wallpapers/thumbs/sunset.jpg")),
    ("Forest", include_bytes!("../assets/wallpapers/forest.jpg"), include_bytes!("../assets/wallpapers/thumbs/forest.jpg")),
    ("Field", include_bytes!("../assets/wallpapers/field.jpg"), include_bytes!("../assets/wallpapers/thumbs/field.jpg")),
    ("Snow", include_bytes!("../assets/wallpapers/snow.jpg"), include_bytes!("../assets/wallpapers/thumbs/snow.jpg")),
];

/// Background numbers (Settings.background): the colours (0-4), the photos, then "your photo".
pub const FIRST_PHOTO: usize = THEMES.len();
pub const CUSTOM: usize = FIRST_PHOTO + PHOTOS.len();
pub const COUNT: usize = CUSTOM + 1;
/// What new installs start with.
pub const DEFAULT: usize = FIRST_PHOTO;

/// The name of a background, for the Settings window.
pub fn name(bg: usize) -> &'static str {
    match bg {
        b if b < FIRST_PHOTO => crate::settings::BACKGROUNDS[b].0,
        b if b < CUSTOM => PHOTOS[b - FIRST_PHOTO].0,
        _ => "Your photo",
    }
}

/// A photo filling w x h (cropped to the screen's shape, never stretched).
pub fn photo(img: &crate::image::Image, w: usize, h: usize) -> Vec<u32> {
    crate::frame::cover(img, w, h)
}

/// Clock colour for the part of the wallpaper inside `r`: dark on bright pictures, white
/// (with a soft shadow behind it) on dark ones. Returns (ink, needs a shadow).
pub fn ink_for(px: &[u32], w: usize, h: usize, r: (i32, i32, i32, i32)) -> (u32, bool) {
    let (x0, y0) = (r.0.clamp(0, w as i32) as usize, r.1.clamp(0, h as i32) as usize);
    let (x1, y1) = ((r.0 + r.2).clamp(0, w as i32) as usize, (r.1 + r.3).clamp(0, h as i32) as usize);
    let (mut sum, mut n) = (0u64, 0u64);
    for y in (y0..y1).step_by(4) {
        for x in (x0..x1).step_by(4) {
            let p = px[y * w + x];
            sum += (299 * (p >> 16 & 0xFF) + 587 * (p >> 8 & 0xFF) + 114 * (p & 0xFF)) as u64 / 1000;
            n += 1;
        }
    }
    if n > 0 && sum / n < 160 {
        (rgb(255, 255, 255), true)
    } else {
        (rgb(20, 28, 40), false)
    }
}

/// Darken an oval (centre cx, cy; radii rx, ry) with a soft edge, up to `strength`/255 in
/// the middle, so white text over a photo stays readable.
pub fn soft_shade(px: &mut [u32], w: usize, h: usize, cx: i32, cy: i32, rx: i32, ry: i32, strength: u32) {
    let (rx, ry) = (rx.max(1) as i64, ry.max(1) as i64);
    for y in (cy as i64 - ry).max(0)..(cy as i64 + ry).min(h as i64) {
        for x in (cx as i64 - rx).max(0)..(cx as i64 + rx).min(w as i64) {
            let (dx, dy) = (x - cx as i64, y - cy as i64);
            // Distance from the centre, 0 (middle) to 4096 (edge).
            let d = (dx * dx * 4096 / (rx * rx) + dy * dy * 4096 / (ry * ry)).min(4096);
            let f = ((4096 - d) * (4096 - d) / 4096) as u32; // smooth fall-off
            let a = 256 - strength * f / 4096;
            let p = &mut px[y as usize * w + x as usize];
            let rb = ((*p & 0xFF00FF) * a >> 8) & 0xFF00FF;
            let g = ((*p & 0x00FF00) * a >> 8) & 0x00FF00;
            *p = 0xFF00_0000 | rb | g;
        }
    }
}

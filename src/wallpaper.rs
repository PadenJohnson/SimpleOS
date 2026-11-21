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

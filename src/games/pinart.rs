//! Drawing for the pinball table: a layer with transparency, drawn at several times the
//! final size and shrunk at the end (smooth edges everywhere), with polygons, round strokes,
//! gradients, soft shadows and glows, and shading for spheres and domes (lit from the top
//! left, as the table's lamps would light them).

use alloc::vec;
use alloc::vec::Vec;

use crate::font::Face;
use crate::gfx::Canvas;
use crate::image::Image;

/// Pixels are 0xAARRGGBB, alpha not premultiplied.
#[derive(Clone)]
pub struct Layer {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

#[inline]
pub fn chan(c: u32, s: u32) -> u32 {
    (c >> s) & 0xff
}

#[inline]
pub fn rgb3(r: u32, g: u32, b: u32) -> u32 {
    r.min(255) << 16 | g.min(255) << 8 | b.min(255)
}

/// Mix two colours: t = 0 gives a, 255 gives b.
#[inline]
pub fn mixc(a: u32, b: u32, t: u32) -> u32 {
    let t = t.min(255);
    let m = |s: u32| (chan(a, s) * (255 - t) + chan(b, s) * t) / 255;
    rgb3(m(16), m(8), m(0))
}

/// Scale a colour's brightness (1.0 = as is), saturating.
#[inline]
pub fn scale(c: u32, k: f32) -> u32 {
    let m = |s: u32| ((chan(c, s) as f32 * k) as i32).clamp(0, 255) as u32;
    rgb3(m(16), m(8), m(0))
}

/// Add white light (0..1) to a colour.
#[inline]
pub fn lighten(c: u32, k: f32) -> u32 {
    let m = |s: u32| (chan(c, s) as f32 + (255.0 - chan(c, s) as f32) * k.clamp(0.0, 1.0)) as u32;
    rgb3(m(16), m(8), m(0))
}

/// The light: from the top left and a little in front.
const LIGHT: (f32, f32, f32) = (-0.42, -0.58, 0.70);

/// A lit sphere's colour at normalised offset (nx, ny) from its centre (inside the unit
/// circle): base colour, how much ambient light, specular strength and sharpness.
pub fn sphere(base: u32, nx: f32, ny: f32, ambient: f32, spec: f32, power: f32) -> u32 {
    let nz = libm::sqrtf((1.0 - nx * nx - ny * ny).max(0.0));
    let (lx, ly, lz) = LIGHT;
    let d = (nx * lx + ny * ly + nz * lz).max(0.0);
    // Reflection of the light, seen from straight above.
    let rz = 2.0 * d * nz - lz;
    let s = libm::powf(rz.max(0.0), power) * spec;
    lighten(scale(base, ambient + (1.0 - ambient) * d * 1.15), s)
}

/// A chrome ball: the bright ceiling reflected above, the dark playfield below, a hot spot.
pub fn chrome(nx: f32, ny: f32) -> u32 {
    let nz = libm::sqrtf((1.0 - nx * nx - ny * ny).max(0.0));
    // Reflected view ray's vertical component picks sky or floor.
    let up = -ny * 0.8 + nz * 0.25;
    let base = if up > 0.05 {
        mixc(0x9aa4b4, 0xf4f6fa, ((up - 0.05) * 400.0) as u32)
    } else {
        mixc(0x2a3040, 0x707888, ((up + 0.6).max(0.0) * 300.0) as u32)
    };
    let (lx, ly, lz) = LIGHT;
    let d = (nx * lx + ny * ly + nz * lz).max(0.0);
    let rz = 2.0 * d * nz - lz;
    let rim = (1.0 - nz) * 0.35;
    lighten(scale(base, 1.0 - rim), libm::powf(rz.max(0.0), 40.0))
}

impl Layer {
    pub fn new(w: usize, h: usize) -> Layer {
        Layer { w, h, px: vec![0; w * h] }
    }

    /// Paint colour `c` with opacity `a` over pixel `i`.
    #[inline]
    pub fn over_i(&mut self, i: usize, c: u32, a: u32) {
        if a == 0 {
            return;
        }
        let d = self.px[i];
        let da = d >> 24;
        if a >= 255 || da == 0 {
            self.px[i] = (a.min(255) << 24) | (c & 0xff_ffff);
            return;
        }
        let oa = a * 255 + da * (255 - a); // x255
        let m = |s: u32| (chan(c, s) * a * 255 + chan(d, s) * da * (255 - a)) / oa.max(1);
        self.px[i] = ((oa / 255) << 24) | rgb3(m(16), m(8), m(0));
    }

    #[inline]
    pub fn plot(&mut self, x: i32, y: i32, c: u32, a: u32) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.over_i(y as usize * self.w + x as usize, c, a);
        }
    }

    /// Fill a polygon (even-odd); `f` gives the colour and opacity at each pixel centre.
    pub fn poly_with(&mut self, pts: &[(f32, f32)], mut f: impl FnMut(f32, f32) -> (u32, u32)) {
        if pts.len() < 3 {
            return;
        }
        let miny = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min).max(0.0) as i32;
        let maxy = (pts.iter().map(|p| p.1).fold(f32::MIN, f32::max).min(self.h as f32 - 1.0)) as i32;
        let mut xs: Vec<f32> = Vec::new();
        for y in miny..=maxy {
            let cy = y as f32 + 0.5;
            xs.clear();
            for k in 0..pts.len() {
                let (a, b) = (pts[k], pts[(k + 1) % pts.len()]);
                if (a.1 <= cy) != (b.1 <= cy) {
                    xs.push(a.0 + (cy - a.1) / (b.1 - a.1) * (b.0 - a.0));
                }
            }
            xs.sort_unstable_by(|a, b| a.partial_cmp(b).unwrap_or(core::cmp::Ordering::Equal));
            for pair in xs.chunks(2) {
                if pair.len() < 2 {
                    break;
                }
                let x0 = libm::ceilf(pair[0] - 0.5).max(0.0) as i32;
                let x1 = libm::floorf(pair[1] - 0.5).min(self.w as f32 - 1.0) as i32;
                for x in x0..=x1 {
                    let (c, a) = f(x as f32 + 0.5, cy);
                    self.over_i(y as usize * self.w + x as usize, c, a);
                }
            }
        }
    }

    pub fn poly(&mut self, pts: &[(f32, f32)], c: u32, a: u32) {
        self.poly_with(pts, |_, _| (c, a));
    }

    /// Fill a circle; `f` gets the pixel's offset from the centre divided by the radius.
    pub fn circle_with(&mut self, cx: f32, cy: f32, r: f32, mut f: impl FnMut(f32, f32) -> (u32, u32)) {
        if r <= 0.0 {
            return;
        }
        let (y0, y1) = (libm::floorf(cy - r).max(0.0) as i32, libm::ceilf(cy + r).min(self.h as f32 - 1.0) as i32);
        let (x0, x1) = (libm::floorf(cx - r).max(0.0) as i32, libm::ceilf(cx + r).min(self.w as f32 - 1.0) as i32);
        for y in y0..=y1 {
            let dy = (y as f32 + 0.5 - cy) / r;
            for x in x0..=x1 {
                let dx = (x as f32 + 0.5 - cx) / r;
                if dx * dx + dy * dy <= 1.0 {
                    let (c, a) = f(dx, dy);
                    self.over_i(y as usize * self.w + x as usize, c, a);
                }
            }
        }
    }

    pub fn circle(&mut self, cx: f32, cy: f32, r: f32, c: u32, a: u32) {
        self.circle_with(cx, cy, r, |_, _| (c, a));
    }

    pub fn ring(&mut self, cx: f32, cy: f32, r: f32, width: f32, c: u32, a: u32) {
        let inner = ((r - width) / r).max(0.0);
        let i2 = inner * inner;
        self.circle_with(cx, cy, r, |dx, dy| if dx * dx + dy * dy >= i2 { (c, a) } else { (0, 0) });
    }

    /// A thick line with round ends; `f` gets the position across it (-1 one edge, 1 the
    /// other) and along it (0..1).
    pub fn capsule_with(&mut self, ax: f32, ay: f32, bx: f32, by: f32, r: f32, mut f: impl FnMut(f32, f32) -> (u32, u32)) {
        let (dx, dy) = (bx - ax, by - ay);
        let len2 = (dx * dx + dy * dy).max(1e-6);
        let len = libm::sqrtf(len2);
        let (y0, y1) = (libm::floorf(ay.min(by) - r).max(0.0) as i32, libm::ceilf(ay.max(by) + r).min(self.h as f32 - 1.0) as i32);
        let (x0, x1) = (libm::floorf(ax.min(bx) - r).max(0.0) as i32, libm::ceilf(ax.max(bx) + r).min(self.w as f32 - 1.0) as i32);
        for y in y0..=y1 {
            let py = y as f32 + 0.5;
            for x in x0..=x1 {
                let px = x as f32 + 0.5;
                let t = (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0);
                let (qx, qy) = (ax + dx * t - px, ay + dy * t - py);
                if qx * qx + qy * qy <= r * r {
                    // Signed distance across: positive on the left of a->b.
                    let across = ((px - ax) * -dy + (py - ay) * dx) / len / r;
                    let (c, a) = f(across.clamp(-1.0, 1.0), t);
                    self.over_i(y as usize * self.w + x as usize, c, a);
                }
            }
        }
    }

    pub fn capsule(&mut self, ax: f32, ay: f32, bx: f32, by: f32, r: f32, c: u32, a: u32) {
        self.capsule_with(ax, ay, bx, by, r, |_, _| (c, a));
    }

    /// Round strokes along a polyline.
    pub fn polyline(&mut self, pts: &[(f32, f32)], r: f32, c: u32, a: u32) {
        for w in pts.windows(2) {
            self.capsule(w[0].0, w[0].1, w[1].0, w[1].1, r, c, a);
        }
    }

    /// Text with its baseline at `base` (pixel size `size`), left edge at x.
    pub fn text(&mut self, x: f32, base: f32, s: &str, face: Face, size: u16, c: u32, a: u32) {
        let w = crate::font::width(s, face, size).max(1) as usize + size as usize;
        let h = size as usize * 2;
        let mut m = Canvas::new(w, h);
        let top = size as i32 * 3 / 2;
        m.text_face((size as i32 / 4) * 64, top, s, face, size, 0xff_ffff);
        let ox = x as i32 - size as i32 / 4;
        let oy = base as i32 - top;
        for yy in 0..h {
            for xx in 0..w {
                let cov = m.px[yy * w + xx] & 0xff;
                if cov > 0 {
                    self.plot(ox + xx as i32, oy + yy as i32, c, cov * a / 255);
                }
            }
        }
    }

    pub fn text_centered(&mut self, cx: f32, base: f32, s: &str, face: Face, size: u16, c: u32, a: u32) {
        let w = crate::font::width(s, face, size) as f32;
        self.text(cx - w / 2.0, base, s, face, size, c, a);
    }

    /// Text with a dark outline (labels on the playfield).
    pub fn label(&mut self, cx: f32, base: f32, s: &str, size: u16, c: u32, outline: u32) {
        let o = (size as f32 / 9.0).max(1.0);
        for (dx, dy) in [(-o, 0.0), (o, 0.0), (0.0, -o), (0.0, o), (-o, -o), (o, o), (-o, o), (o, -o)] {
            self.text_centered(cx + dx, base + dy, s, Face::SansBold, size, outline, 255);
        }
        self.text_centered(cx, base, s, Face::SansBold, size, c, 255);
    }

    /// Paint another layer over this one with its top left at (x, y), at opacity `a`.
    pub fn draw(&mut self, src: &Layer, x: i32, y: i32, a: u32) {
        for sy in 0..src.h {
            let ty = y + sy as i32;
            if ty < 0 || ty as usize >= self.h {
                continue;
            }
            for sx in 0..src.w {
                let tx = x + sx as i32;
                if tx < 0 || tx as usize >= self.w {
                    continue;
                }
                let p = src.px[sy * src.w + sx];
                let pa = (p >> 24) * a / 255;
                if pa > 0 {
                    self.over_i(ty as usize * self.w + tx as usize, p, pa);
                }
            }
        }
    }

    /// A soft shadow (or glow) of this layer's shapes: their opacity, blurred, in one colour.
    pub fn silhouette(&self, c: u32, strength: u32, radius: usize) -> Layer {
        let (w, h) = (self.w, self.h);
        let mut a: Vec<u32> = self.px.iter().map(|p| (p >> 24) * strength / 255).collect();
        if radius > 0 {
            let mut tmp = vec![0u32; w.max(h)];
            for _ in 0..3 {
                box_pass(&mut a, w, h, radius, true, &mut tmp);
                box_pass(&mut a, w, h, radius, false, &mut tmp);
            }
        }
        let rgb = c & 0xff_ffff;
        Layer { w, h, px: a.iter().map(|&v| if v == 0 { 0 } else { v.min(255) << 24 | rgb }).collect() }
    }

    /// Fill the whole layer with a vertical gradient, opaque.
    pub fn gradient_rows(&mut self, f: impl Fn(f32) -> u32) {
        for y in 0..self.h {
            let c = 0xff00_0000 | f(y as f32 / self.h.max(1) as f32);
            self.px[y * self.w..(y + 1) * self.w].fill(c);
        }
    }

    /// Shrink by `ss` (averaging `ss` x `ss` blocks) into an image.
    pub fn shrink(&self, ss: usize, opaque: bool) -> Image {
        let (w, h) = (self.w / ss, self.h / ss);
        let mut px = vec![0u32; w * h];
        let n = (ss * ss) as u32;
        for y in 0..h {
            for x in 0..w {
                let (mut sa, mut sr, mut sg, mut sb) = (0u32, 0u32, 0u32, 0u32);
                for j in 0..ss {
                    let row = (y * ss + j) * self.w + x * ss;
                    for i in 0..ss {
                        let p = self.px[row + i];
                        let a = p >> 24;
                        sa += a;
                        sr += chan(p, 16) * a;
                        sg += chan(p, 8) * a;
                        sb += chan(p, 0) * a;
                    }
                }
                px[y * w + x] = if sa == 0 {
                    0
                } else {
                    let a = if opaque { 255 } else { sa / n };
                    a << 24 | rgb3(sr / sa, sg / sa, sb / sa)
                };
            }
        }
        Image { w, h, px, opaque }
    }

    /// As an image without shrinking.
    pub fn to_image(&self, opaque: bool) -> Image {
        let px = if opaque { self.px.iter().map(|p| p | 0xff00_0000).collect() } else { self.px.clone() };
        Image { w: self.w, h: self.h, px, opaque }
    }

    /// From an image (for compositing more on top).
    pub fn from_image(img: &Image) -> Layer {
        let px = if img.opaque { img.px.iter().map(|p| p | 0xff00_0000).collect() } else { img.px.clone() };
        Layer { w: img.w, h: img.h, px }
    }
}

/// One box-blur pass along rows (or columns) with a running sum.
fn box_pass(c: &mut [u32], w: usize, h: usize, r: usize, rows: bool, tmp: &mut [u32]) {
    let (n, lines) = if rows { (w, h) } else { (h, w) };
    let idx = |line: usize, k: usize| if rows { line * w + k } else { k * w + line };
    let div = (2 * r + 1) as u32;
    for line in 0..lines {
        let mut sum = 0u32;
        // Edges extend with zeros (shapes fade out at the borders).
        for k in 0..=r.min(n - 1) {
            sum += c[idx(line, k)];
        }
        for k in 0..n {
            tmp[k] = sum / div;
            let add = k + r + 1;
            if add < n {
                sum += c[idx(line, add)];
            }
            if k >= r {
                sum -= c[idx(line, k - r)];
            }
        }
        for k in 0..n {
            c[idx(line, k)] = tmp[k];
        }
    }
}

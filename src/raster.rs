//! Anti-aliased filling of outlines (font glyphs, PDF paths): lines and curves are turned into
//! exact per-pixel coverage (the signed-area method), in fixed point because Solon's kernel
//! has no hardware floating point. Supports the nonzero and even-odd fill rules.

use alloc::vec;
use alloc::vec::Vec;

/// 1/256 px fixed point.
fn fix(v: f32) -> i32 {
    let v = v * 256.0;
    if v > 2.0e9 {
        2_000_000_000
    } else if v < -2.0e9 {
        -2_000_000_000
    } else {
        v as i32
    }
}

pub struct Raster {
    pub w: usize,
    pub h: usize,
    stride: usize,
    a: Vec<i32>,
    /// Rows touched since the last clear (y0..y1), and the columns (x0..x1).
    y0: usize,
    y1: usize,
    x0: usize,
    x1: usize,
    /// Current point and subpath start, for the path-building helpers.
    cur: (f32, f32),
    start: (f32, f32),
}

impl Raster {
    pub fn new(w: usize, h: usize) -> Raster {
        let stride = w + 2;
        Raster { w, h, stride, a: vec![0; stride * h.max(1)], y0: h, y1: 0, x0: stride, x1: 0, cur: (0.0, 0.0), start: (0.0, 0.0) }
    }

    pub fn is_empty(&self) -> bool {
        self.y0 >= self.y1
    }

    /// Forget everything drawn (only the touched area is wiped).
    pub fn clear(&mut self) {
        if self.y0 < self.y1 {
            let x1 = self.x1.min(self.stride);
            for y in self.y0..self.y1 {
                self.a[y * self.stride + self.x0..y * self.stride + x1].fill(0);
            }
        }
        self.y0 = self.h;
        self.y1 = 0;
        self.x0 = self.stride;
        self.x1 = 0;
    }

    // ---- path building ------------------------------------------------------------------

    pub fn move_to(&mut self, x: f32, y: f32) {
        self.close();
        self.cur = (x, y);
        self.start = (x, y);
    }

    pub fn line_to(&mut self, x: f32, y: f32) {
        let (cx, cy) = self.cur;
        self.line(cx, cy, x, y);
        self.cur = (x, y);
    }

    pub fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x0, y0) = self.cur;
        let (ddx, ddy) = (x0 - 2.0 * x1 + x, y0 - 2.0 * y1 + y);
        let dd = libm::sqrtf(ddx * ddx + ddy * ddy);
        let n = (libm::sqrtf(dd * 2.0) as usize + 1).min(64);
        let mut px = (x0, y0);
        for k in 1..=n {
            let t = k as f32 / n as f32;
            let mt = 1.0 - t;
            let q = (mt * mt * x0 + 2.0 * mt * t * x1 + t * t * x, mt * mt * y0 + 2.0 * mt * t * y1 + t * t * y);
            self.line(px.0, px.1, q.0, q.1);
            px = q;
        }
        self.cur = (x, y);
    }

    pub fn cubic_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x0, y0) = self.cur;
        let d1 = (x0 - 2.0 * x1 + x2).abs() + (y0 - 2.0 * y1 + y2).abs();
        let d2 = (x1 - 2.0 * x2 + x).abs() + (y1 - 2.0 * y2 + y).abs();
        let dd = d1.max(d2);
        let n = (libm::sqrtf(dd * 3.0) as usize + 1).min(100);
        let mut px = (x0, y0);
        for k in 1..=n {
            let t = k as f32 / n as f32;
            let mt = 1.0 - t;
            let (a, b, c, d) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
            let q = (a * x0 + b * x1 + c * x2 + d * x, a * y0 + b * y1 + c * y2 + d * y);
            self.line(px.0, px.1, q.0, q.1);
            px = q;
        }
        self.cur = (x, y);
    }

    /// Close the current subpath (filling always treats subpaths as closed).
    pub fn close(&mut self) {
        if self.cur != self.start {
            let (cx, cy) = self.cur;
            let (sx, sy) = self.start;
            self.line(cx, cy, sx, sy);
        }
        self.cur = self.start;
    }

    // ---- coverage -----------------------------------------------------------------------

    /// Add one edge, in pixel coordinates.
    pub fn line(&mut self, x0: f32, y0: f32, x1: f32, y1: f32) {
        self.line_fx(fix(x0), fix(y0), fix(x1), fix(y1));
    }

    fn line_fx(&mut self, x0: i32, y0: i32, x1: i32, y1: i32) {
        if y0 == y1 {
            return;
        }
        let (dir, x0, y0, x1, y1) = if y0 < y1 { (1, x0, y0, x1, y1) } else { (-1, x1, y1, x0, y0) };
        let hmax = (self.h as i32) << 8;
        if y1 <= 0 || y0 >= hmax {
            return;
        }
        let dxdy: i64 = (((x1 - x0) as i64) << 16) / (y1 - y0) as i64;
        let ys = y0.max(0);
        let ye = y1.min(hmax);
        let mut x: i64 = ((x0 as i64) << 16) + (ys - y0) as i64 * dxdy;
        let mut y = ys;
        let r0 = (ys >> 8) as usize;
        let r1 = ((ye + 255) >> 8) as usize;
        self.y0 = self.y0.min(r0);
        self.y1 = self.y1.max(r1.min(self.h));
        while y < ye {
            let row = y >> 8;
            let ynext = ((row + 1) << 8).min(ye);
            let dy = ynext - y;
            let xnext = x + dy as i64 * dxdy;
            self.span(row as usize, (x >> 16) as i32, (xnext >> 16) as i32, dy * dir);
            x = xnext;
            y = ynext;
        }
    }

    fn span(&mut self, row: usize, xa: i32, xb: i32, d: i32) {
        let wmax = (self.w as i32) << 8;
        let xa = xa.clamp(0, wmax);
        let xb = xb.clamp(0, wmax);
        let (x0, x1) = if xa < xb { (xa, xb) } else { (xb, xa) };
        let base = row * self.stride;
        let x0i = x0 >> 8;
        let x1i = (x1 + 255) >> 8;
        self.x0 = self.x0.min(x0i as usize);
        self.x1 = self.x1.max(x1i as usize + 2);
        let x0floor = x0i << 8;
        let a = &mut self.a;
        if x1i <= x0i + 1 {
            let xmf = ((xa + xb) >> 1) - x0floor;
            a[base + x0i as usize] += d * (256 - xmf);
            a[base + x0i as usize + 1] += d * xmf;
        } else {
            let w = (x1 - x0) as i64;
            let x0f = (x0 - x0floor) as i64;
            let x1f = (x1 - (x1i << 8) + 256) as i64;
            let a0 = (256 - x0f) * (256 - x0f) * 128 / w;
            let am = x1f * x1f * 128 / w;
            let d = d as i64;
            let mut put = |i: i32, v: i64| a[base + i as usize] += ((d * v) >> 8) as i32;
            put(x0i, a0);
            if x1i == x0i + 2 {
                put(x0i + 1, 65536 - a0 - am);
            } else {
                let s = 65536 * 256 / w;
                let a1 = (384 - x0f) * 65536 / w;
                put(x0i + 1, a1 - a0);
                for xi in x0i + 2..x1i - 1 {
                    put(xi, s);
                }
                let a2 = a1 + (x1i - x0i - 3) as i64 * s;
                put(x1i - 1, 65536 - a2 - am);
            }
            put(x1i, am);
        }
    }

    /// Visit each touched row's coverage (0..=255) for columns x0..x1:
    /// `f(y, x0, coverage)`.
    pub fn rows(&self, evenodd: bool, mut f: impl FnMut(usize, usize, &[u8])) {
        if self.is_empty() {
            return;
        }
        let x0 = self.x0.min(self.w);
        let x1 = self.x1.min(self.w);
        if x0 >= x1 {
            return;
        }
        let mut out = vec![0u8; x1 - x0];
        for y in self.y0..self.y1 {
            let row = &self.a[y * self.stride..y * self.stride + x1];
            // Everything left of x0 is zero (nothing was drawn there).
            let mut acc = 0i32;
            for (o, &v) in out.iter_mut().zip(&row[x0..x1]) {
                acc += v;
                let m = acc.unsigned_abs();
                let c = if evenodd {
                    let m = m & 0x1FFFF;
                    if m > 65536 {
                        131072 - m
                    } else {
                        m
                    }
                } else {
                    m.min(65536)
                };
                *o = ((c * 255 + 32768) >> 16) as u8;
            }
            f(y, x0, &out);
        }
    }

    /// The coverage as one w*h mask.
    pub fn mask(&self, evenodd: bool) -> Vec<u8> {
        let mut m = vec![0u8; self.w * self.h];
        let w = self.w;
        self.rows(evenodd, |y, x0, cov| m[y * w + x0..y * w + x0 + cov.len()].copy_from_slice(cov));
        m
    }
}

//! Motion compensation shared by the MPEG-1/2 and MPEG-4 Part 2 decoders: blocks copied from
//! a reference picture at half-pixel positions (edges repeat when a vector points outside).

/// A picture plane, or one field of it (every other line).
#[derive(Clone, Copy)]
pub struct Plane<'a> {
    pub d: &'a [u8],
    pub off: usize,
    pub stride: usize,
    pub w: usize,
    pub h: usize,
    /// For a field: which one (0 top, 1 bottom) and the frame's height. Outside the picture
    /// the frame's edge rows repeat (as FFmpeg does), not the field's.
    pub field: Option<(usize, usize)>,
}

impl<'a> Plane<'a> {
    pub fn frame(d: &'a [u8], stride: usize, h: usize) -> Plane<'a> {
        Plane { d, off: 0, stride, w: stride, h, field: None }
    }
    /// Field 0 (top) or 1 (bottom) of a frame plane.
    pub fn field(d: &'a [u8], stride: usize, h: usize, f: usize) -> Plane<'a> {
        Plane { d, off: f * stride, stride: stride * 2, w: stride, h: h / 2, field: Some((f, h)) }
    }
    #[inline]
    fn at(&self, x: i32, y: i32) -> u8 {
        let x = x.clamp(0, self.w as i32 - 1) as usize;
        if let Some((f, fh)) = self.field {
            let row = (2 * y + f as i32).clamp(0, fh as i32 - 1) as usize;
            return self.d[row * (self.stride / 2) + x];
        }
        let y = y.clamp(0, self.h as i32 - 1) as usize;
        self.d[self.off + y * self.stride + x]
    }
}

/// Predict a `w` x `h` block whose top-left is (x + hx/2, y + hy/2) in `src`, into `dst` at
/// `at` (rows `ds` apart); `avg` averages with what's there (two-way prediction). `rnd` is
/// MPEG-4's rounding control (0 for MPEG-1/2).
#[allow(clippy::too_many_arguments)]
pub fn half(src: &Plane, x: i32, y: i32, hx: bool, hy: bool, w: usize, h: usize, rnd: u32, dst: &mut [u8], at: usize, ds: usize, avg: bool) {
    let inside = x >= 0 && y >= 0 && x as usize + w + 1 <= src.w && y as usize + h + 1 <= src.h;
    let mut tmp = [0u8; 17 * 17];
    let tw = w + 1;
    for r in 0..h + 1 {
        if inside {
            let s = src.off + (y as usize + r) * src.stride + x as usize;
            tmp[r * tw..r * tw + tw].copy_from_slice(&src.d[s..s + tw]);
        } else {
            for c in 0..tw {
                tmp[r * tw + c] = src.at(x + c as i32, y + r as i32);
            }
        }
    }
    for r in 0..h {
        let row = &tmp[r * tw..];
        let next = &tmp[(r + 1) * tw..];
        let o = at + r * ds;
        for c in 0..w {
            let p = match (hx, hy) {
                (false, false) => row[c] as u32,
                (true, false) => (row[c] as u32 + row[c + 1] as u32 + 1 - rnd) >> 1,
                (false, true) => (row[c] as u32 + next[c] as u32 + 1 - rnd) >> 1,
                (true, true) => (row[c] as u32 + row[c + 1] as u32 + next[c] as u32 + next[c + 1] as u32 + 2 - rnd) >> 2,
            };
            let d = &mut dst[o + c];
            *d = if avg { ((*d as u32 + p + 1) >> 1) as u8 } else { p as u8 };
        }
    }
}

/// How a predicted block is written: plainly, plainly with MPEG-4's "no rounding" (rounding
/// control 1), or averaged with what's there (the second half of a two-way prediction).
#[derive(Clone, Copy, PartialEq)]
pub enum Op {
    Put,
    PutNoRnd,
    Avg,
}

#[inline]
fn clip8(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

/// MPEG-4's 8-tap half-sample filter over n + 1 samples (`get(k)` for k in 0..=n), mirrored at
/// the block's ends; `out(i, v)` gets the unrounded sums (x32).
#[inline]
fn lowpass(n: usize, get: impl Fn(usize) -> i32, mut out: impl FnMut(usize, i32)) {
    let s = |k: isize| -> i32 {
        let k = if k < 0 { -k - 1 } else if k > n as isize { 2 * n as isize + 1 - k } else { k };
        get(k as usize)
    };
    for i in 0..n as isize {
        let v = (s(i) + s(i + 1)) * 20 - (s(i - 1) + s(i + 2)) * 6 + (s(i - 2) + s(i + 3)) * 3 - (s(i - 3) + s(i + 4));
        out(i as usize, v);
    }
}

/// Quarter-pixel prediction (MPEG-4 qpel) of an n x n block (n = 8 or 16) whose top-left is
/// (x + dx/4, y + dy/4), the same steps as FFmpeg's qpel functions.
#[allow(clippy::too_many_arguments)]
pub fn qpel(src: &Plane, x: i32, y: i32, dx: u32, dy: u32, n: usize, op: Op, dst: &mut [u8], at: usize, ds: usize) {
    const W: usize = 17;
    // The full-sample block, one more row and column than the output.
    let mut f = [0u8; W * W];
    let inside = x >= 0 && y >= 0 && x as usize + n + 1 <= src.w && y as usize + n + 1 <= src.h;
    for r in 0..=n {
        for c in 0..=n {
            f[r * W + c] = if inside { src.d[src.off + (y as usize + r) * src.stride + x as usize + c] } else { src.at(x + c as i32, y + r as i32) };
        }
    }
    let round = op != Op::PutNoRnd;
    let rb = if round { 16 } else { 15 };
    let l2i = |a: u8, b: u8| -> u8 { ((a as u32 + b as u32 + round as u32) >> 1) as u8 };
    // The final write of one value `p` (already filtered/averaged) at (r, c).
    let put = |dst: &mut [u8], r: usize, c: usize, p: u8| {
        let d = &mut dst[at + r * ds + c];
        *d = if op == Op::Avg { ((*d as u32 + p as u32 + 1) >> 1) as u8 } else { p };
    };
    // Final average of two predictions.
    let fin2 = |dst: &mut [u8], r: usize, c: usize, a: u8, b: u8| {
        let p = if op == Op::PutNoRnd { ((a as u32 + b as u32) >> 1) as u8 } else { ((a as u32 + b as u32 + 1) >> 1) as u8 };
        put(dst, r, c, p);
    };
    let filt = |v: i32| clip8((v + rb) >> 5);
    match (dx, dy) {
        (0, 0) => {
            for r in 0..n {
                for c in 0..n {
                    put(dst, r, c, f[r * W + c]);
                }
            }
        }
        (_, 0) => {
            for r in 0..n {
                let mut h = [0u8; 16];
                lowpass(n, |k| f[r * W + k] as i32, |i, v| h[i] = filt(v));
                for c in 0..n {
                    match dx {
                        2 => put(dst, r, c, h[c]),
                        1 => fin2(dst, r, c, f[r * W + c], h[c]),
                        _ => fin2(dst, r, c, f[r * W + c + 1], h[c]),
                    }
                }
            }
        }
        (0, _) => {
            for c in 0..n {
                let mut v = [0u8; 16];
                lowpass(n, |k| f[k * W + c] as i32, |i, s| v[i] = filt(s));
                for r in 0..n {
                    match dy {
                        2 => put(dst, r, c, v[r]),
                        1 => fin2(dst, r, c, f[r * W + c], v[r]),
                        _ => fin2(dst, r, c, f[(r + 1) * W + c], v[r]),
                    }
                }
            }
        }
        _ => {
            // Horizontal half samples for n + 1 rows, blended toward the nearer full column.
            let mut hh = [0u8; W * W];
            for r in 0..=n {
                lowpass(n, |k| f[r * W + k] as i32, |i, v| hh[r * W + i] = filt(v));
                if dx != 2 {
                    for c in 0..n {
                        let full = f[r * W + c + (dx == 3) as usize];
                        hh[r * W + c] = l2i(hh[r * W + c], full);
                    }
                }
            }
            for c in 0..n {
                let mut v = [0u8; 16];
                lowpass(n, |k| hh[k * W + c] as i32, |i, s| v[i] = filt(s));
                for r in 0..n {
                    match dy {
                        2 => put(dst, r, c, v[r]),
                        1 => fin2(dst, r, c, hh[r * W + c], v[r]),
                        _ => fin2(dst, r, c, hh[(r + 1) * W + c], v[r]),
                    }
                }
            }
        }
    }
}

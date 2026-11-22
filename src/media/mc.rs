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
}

impl<'a> Plane<'a> {
    pub fn frame(d: &'a [u8], stride: usize, h: usize) -> Plane<'a> {
        Plane { d, off: 0, stride, w: stride, h }
    }
    /// Field 0 (top) or 1 (bottom) of a frame plane.
    pub fn field(d: &'a [u8], stride: usize, h: usize, f: usize) -> Plane<'a> {
        Plane { d, off: f * stride, stride: stride * 2, w: stride, h: h / 2 }
    }
    #[inline]
    fn at(&self, x: i32, y: i32) -> u8 {
        let x = x.clamp(0, self.w as i32 - 1) as usize;
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

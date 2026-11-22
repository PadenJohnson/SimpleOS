//! HEVC intra prediction: reference samples (with substitution and smoothing), then planar,
//! DC or one of 33 angles. Same steps as FFmpeg's hevcpred.

const ANGLE: [i32; 33] = [32, 26, 21, 17, 13, 9, 5, 2, 0, -2, -5, -9, -13, -17, -21, -26, -32, -26, -21, -17, -13, -9, -5, -2, 0, 2, 5, 9, 13, 17, 21, 26, 32];
const INV_ANGLE: [i32; 15] = [-4096, -1638, -910, -630, -482, -390, -315, -256, -315, -390, -482, -630, -910, -1638, -4096];

pub struct Params {
    pub log2: u32,
    pub luma: bool,
    pub mode: u32,
    pub bit_depth: u32,
    pub strong_smoothing: bool,
}

/// Predict the block at (x, y) of `plane` (rows `stride` apart). `avail(dx, dy)` says whether
/// the sample at (x + dx, y + dy) may be used (dx or dy is -1 for the neighbours).
pub fn intra(plane: &mut [u16], stride: usize, x: usize, y: usize, p: &Params, avail: &dyn Fn(isize, isize) -> bool) {
    let n = 1usize << p.log2;
    // p[0] = left[2n-1] .. p[2n-1] = left[0], p[2n] = corner, p[2n+1..] = top[0..2n].
    let total = 4 * n + 1;
    let mut s = [0u16; 4 * 32 + 1];
    let mut ok = [false; 4 * 32 + 1];
    let at = |dx: isize, dy: isize| plane[(y as isize + dy) as usize * stride + (x as isize + dx) as usize];
    let mut any = false;
    for i in 0..2 * n {
        let dy = (2 * n - 1 - i) as isize;
        if avail(-1, dy) {
            s[i] = at(-1, dy);
            ok[i] = true;
            any = true;
        }
    }
    if avail(-1, -1) {
        s[2 * n] = at(-1, -1);
        ok[2 * n] = true;
        any = true;
    }
    for i in 0..2 * n {
        if avail(i as isize, -1) {
            s[2 * n + 1 + i] = at(i as isize, -1);
            ok[2 * n + 1 + i] = true;
            any = true;
        }
    }
    if !any {
        let v = 1u16 << (p.bit_depth - 1);
        for e in s.iter_mut().take(total) {
            *e = v;
        }
    } else {
        if !ok[0] {
            let k = (1..total).find(|&k| ok[k]).unwrap_or(0);
            s[0] = s[k];
        }
        for i in 1..total {
            if !ok[i] {
                s[i] = s[i - 1];
            }
        }
    }
    // left[-1..2n] and top[-1..2n] with the corner at index 0.
    let mut left = [0i32; 2 * 32 + 1];
    let mut top = [0i32; 2 * 32 + 1];
    left[0] = s[2 * n] as i32;
    top[0] = left[0];
    for i in 0..2 * n {
        left[1 + i] = s[2 * n - 1 - i] as i32;
        top[1 + i] = s[2 * n + 1 + i] as i32;
    }
    // Smoothing.
    if p.luma && p.mode != 1 && n != 4 {
        let thresh = [7, 1, 0][p.log2 as usize - 3];
        let dist = (p.mode as i32 - 26).abs().min((p.mode as i32 - 10).abs());
        if dist > thresh {
            let t = 1 << (p.bit_depth - 5);
            if p.strong_smoothing && p.log2 == 5 && (top[0] + top[64] - 2 * top[32]).abs() < t && (left[0] + left[64] - 2 * left[32]).abs() < t {
                let (c, tr, bl) = (top[0], top[64], left[64]);
                for i in 0..63 {
                    top[1 + i] = ((63 - i as i32) * c + (i as i32 + 1) * tr + 32) >> 6;
                    left[1 + i] = ((63 - i as i32) * c + (i as i32 + 1) * bl + 32) >> 6;
                }
            } else {
                let (ol, ot) = (left, top);
                for i in 1..2 * n {
                    left[i] = (ol[i + 1] + 2 * ol[i] + ol[i - 1] + 2) >> 2;
                    top[i] = (ot[i + 1] + 2 * ot[i] + ot[i - 1] + 2) >> 2;
                }
                let c = (ol[1] + 2 * ol[0] + ot[1] + 2) >> 2;
                left[0] = c;
                top[0] = c;
            }
        }
    }
    let maxv = (1i32 << p.bit_depth) - 1;
    let mut put = |dx: usize, dy: usize, v: i32| plane[(y + dy) * stride + x + dx] = v as u16;
    let lf = |i: isize| left[(i + 1) as usize];
    let tp = |i: isize| top[(i + 1) as usize];
    match p.mode {
        0 => {
            let ni = n as i32;
            for yy in 0..n {
                for xx in 0..n {
                    let v = ((ni - 1 - xx as i32) * lf(yy as isize) + (xx as i32 + 1) * tp(n as isize) + (ni - 1 - yy as i32) * tp(xx as isize) + (yy as i32 + 1) * lf(n as isize) + ni) >> (p.log2 + 1);
                    put(xx, yy, v);
                }
            }
        }
        1 => {
            let mut dc = n as i32;
            for i in 0..n {
                dc += lf(i as isize) + tp(i as isize);
            }
            dc >>= p.log2 + 1;
            for yy in 0..n {
                for xx in 0..n {
                    put(xx, yy, dc);
                }
            }
            if p.luma && n < 32 {
                put(0, 0, (lf(0) + 2 * dc + tp(0) + 2) >> 2);
                for xx in 1..n {
                    put(xx, 0, (tp(xx as isize) + 3 * dc + 2) >> 2);
                }
                for yy in 1..n {
                    put(0, yy, (lf(yy as isize) + 3 * dc + 2) >> 2);
                }
            }
        }
        mode => {
            let angle = ANGLE[mode as usize - 2];
            let ni = n as isize;
            let last = (ni * angle as isize) >> 5;
            // Reference row/column, extended with projected samples for negative angles.
            let mut r = [0i32; 3 * 32 + 4];
            let base = n + 1; // r[base + k] is ref[k]
            let (main, side): (&dyn Fn(isize) -> i32, &dyn Fn(isize) -> i32) = if mode >= 18 { (&tp, &lf) } else { (&lf, &tp) };
            for k in -1..2 * ni {
                r[(base as isize + k) as usize] = main(k);
            }
            if angle < 0 && last < -1 {
                let inv = INV_ANGLE[mode as usize - 11] as isize;
                for k in last..=-1 {
                    r[(base as isize + k - 1) as usize] = side(-1 + ((k * inv + 128) >> 8));
                }
            }
            for j in 0..n {
                let pos = (j as i32 + 1) * angle;
                let idx = (pos >> 5) as isize;
                let fact = pos & 31;
                for i in 0..n {
                    let a = r[(base as isize + i as isize + idx) as usize];
                    let v = if fact != 0 {
                        let b = r[(base as isize + i as isize + idx + 1) as usize];
                        ((32 - fact) * a + fact * b + 16) >> 5
                    } else {
                        a
                    };
                    if mode >= 18 {
                        put(i, j, v);
                    } else {
                        put(j, i, v);
                    }
                }
            }
            if p.luma && n < 32 {
                if mode == 26 {
                    for yy in 0..n {
                        put(0, yy, (tp(0) + ((lf(yy as isize) - lf(-1)) >> 1)).clamp(0, maxv));
                    }
                } else if mode == 10 {
                    for xx in 0..n {
                        put(xx, 0, (lf(0) + ((tp(xx as isize) - tp(-1)) >> 1)).clamp(0, maxv));
                    }
                }
            }
        }
    }
}

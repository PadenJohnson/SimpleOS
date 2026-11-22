//! HEVC motion compensation: luma at quarter samples (8-tap filter), chroma at eighth samples
//! (4-tap), 14-bit intermediate predictions, then default or explicit weighting.

use super::tab::{EPEL_FILTERS, QPEL_FILTERS};

/// A reference plane: samples, stride, and the picture's size in this plane (edges repeat).
pub struct RefPlane<'a> {
    pub d: &'a [u16],
    pub stride: usize,
    pub w: usize,
    pub h: usize,
}

/// Interpolated prediction (14-bit) of a w x h block at integer position (x, y) plus the
/// fraction (fx, fy) in 1/4 (luma, taps = 8) or 1/8 (chroma, taps = 4) units.
#[allow(clippy::too_many_arguments)]
pub fn predict(r: &RefPlane, x: isize, y: isize, fx: usize, fy: usize, w: usize, h: usize, luma: bool, bit_depth: u32, out: &mut [i16], src: &mut [u16], tmp: &mut [i32]) {
    let taps: usize = if luma { 8 } else { 4 };
    let half = taps / 2 - 1; // samples before the position
    let mut fxc = [0i32; 8];
    let mut fyc = [0i32; 8];
    for k in 0..taps {
        fxc[k] = if luma { QPEL_FILTERS[fx][k] } else { EPEL_FILTERS[fx][k] };
        fyc[k] = if luma { QPEL_FILTERS[fy][k] } else { EPEL_FILTERS[fy][k] };
    }
    let shift1 = bit_depth.saturating_sub(8).min(4);
    let shift3 = 14 - bit_depth;
    // The source area (with the filter's extra samples) in a local buffer, edges repeated.
    const BW: usize = 64 + 7;
    let (sw, sh) = (w + taps - 1, h + taps - 1);
    let (x0, y0) = (x - half as isize, y - half as isize);
    if x0 >= 0 && y0 >= 0 && x0 as usize + sw <= r.w && y0 as usize + sh <= r.h {
        for j in 0..sh {
            let o = (y0 as usize + j) * r.stride + x0 as usize;
            src[j * BW..j * BW + sw].copy_from_slice(&r.d[o..o + sw]);
        }
    } else {
        for j in 0..sh {
            let yy = (y0 + j as isize).clamp(0, r.h as isize - 1) as usize * r.stride;
            for i in 0..sw {
                src[j * BW + i] = r.d[yy + (x0 + i as isize).clamp(0, r.w as isize - 1) as usize];
            }
        }
    }
    let s = |i: usize, j: usize| src[j * BW + i] as i32;
    if fx == 0 && fy == 0 {
        for j in 0..h {
            for i in 0..w {
                out[j * w + i] = (s(i + half, j + half) << shift3) as i16;
            }
        }
    } else if fy == 0 {
        for j in 0..h {
            let row = &src[(j + half) * BW..(j + half) * BW + sw];
            for i in 0..w {
                let mut a = 0;
                for k in 0..taps {
                    a += fxc[k] * row[i + k] as i32;
                }
                out[j * w + i] = (a >> shift1) as i16;
            }
        }
    } else if fx == 0 {
        for j in 0..h {
            for i in 0..w {
                let mut a = 0;
                for k in 0..taps {
                    a += fyc[k] * s(i + half, j + k);
                }
                out[j * w + i] = (a >> shift1) as i16;
            }
        }
    } else {
        for j in 0..sh {
            let row = &src[j * BW..j * BW + sw];
            for i in 0..w {
                let mut a = 0;
                for k in 0..taps {
                    a += fxc[k] * row[i + k] as i32;
                }
                tmp[j * 64 + i] = a >> shift1;
            }
        }
        for j in 0..h {
            for i in 0..w {
                let mut a = 0;
                for k in 0..taps {
                    a += fyc[k] * tmp[(j + k) * 64 + i];
                }
                out[j * w + i] = (a >> 6) as i16;
            }
        }
    }
}

/// Weighting: None = default; Some((w, o, log2wd)) explicit (offset already scaled).
#[allow(clippy::too_many_arguments)]
pub fn put_uni(dst: &mut [u16], at: usize, stride: usize, p: &[i16], w: usize, h: usize, bit_depth: u32, wt: Option<(i32, i32, u32)>) {
    let max = (1i32 << bit_depth) - 1;
    let shift = 14 - bit_depth;
    for j in 0..h {
        for i in 0..w {
            let v = p[j * w + i] as i32;
            let r = match wt {
                None => (v + (1 << (shift - 1))) >> shift,
                Some((wgt, o, l)) => {
                    let l = l + shift;
                    if l >= 1 {
                        ((v * wgt + (1 << (l - 1))) >> l) + o
                    } else {
                        v * wgt + o
                    }
                }
            };
            dst[at + j * stride + i] = r.clamp(0, max) as u16;
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub fn put_bi(dst: &mut [u16], at: usize, stride: usize, p0: &[i16], p1: &[i16], w: usize, h: usize, bit_depth: u32, wt: Option<(i32, i32, i32, i32, u32)>) {
    let max = (1i32 << bit_depth) - 1;
    let shift = 15 - bit_depth;
    for j in 0..h {
        for i in 0..w {
            let (a, b) = (p0[j * w + i] as i32, p1[j * w + i] as i32);
            let r = match wt {
                None => (a + b + (1 << (shift - 1))) >> shift,
                Some((w0, o0, w1, o1, l)) => {
                    let l = l + 14 - bit_depth;
                    (a * w0 + b * w1 + ((o0 + o1 + 1) << l)) >> (l + 1)
                }
            };
            dst[at + j * stride + i] = r.clamp(0, max) as u16;
        }
    }
}

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
    if luma {
        let (mut cx, mut cy) = ([0i32; 8], [0i32; 8]);
        cx.copy_from_slice(&QPEL_FILTERS[fx]);
        cy.copy_from_slice(&QPEL_FILTERS[fy]);
        filter_block::<8>(r, x, y, fx, fy, &cx, &cy, w, h, bit_depth, out, src, tmp);
    } else {
        let (mut cx, mut cy) = ([0i32; 4], [0i32; 4]);
        cx.copy_from_slice(&EPEL_FILTERS[fx]);
        cy.copy_from_slice(&EPEL_FILTERS[fy]);
        filter_block::<4>(r, x, y, fx, fy, &cx, &cy, w, h, bit_depth, out, src, tmp);
    }
}

const BW: usize = 64 + 7;

#[allow(clippy::too_many_arguments)]
fn filter_block<const T: usize>(r: &RefPlane, x: isize, y: isize, fx: usize, fy: usize, cx: &[i32; T], cy: &[i32; T], w: usize, h: usize, bit_depth: u32, out: &mut [i16], src: &mut [u16], tmp: &mut [i32]) {
    let half = T / 2 - 1;
    let shift1 = bit_depth.saturating_sub(8).min(4);
    let shift3 = 14 - bit_depth;
    // Integer position: just scale (read straight from the picture when inside).
    let (x0, y0) = (x - half as isize, y - half as isize);
    let (sw, sh) = (w + T - 1, h + T - 1);
    let inside = x0 >= 0 && y0 >= 0 && x0 as usize + sw <= r.w && y0 as usize + sh <= r.h;
    if inside {
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
    if fx == 0 && fy == 0 {
        for j in 0..h {
            let s = &src[(j + half) * BW + half..(j + half) * BW + half + w];
            let o = &mut out[j * w..j * w + w];
            for i in 0..w {
                o[i] = ((s[i] as i32) << shift3) as i16;
            }
        }
        return;
    }
    // Horizontal pass into tmp (rows needed by the vertical pass), or straight to out.
    let hrows = if fy == 0 { 0..h } else { 0..sh };
    if fx != 0 {
        for j in hrows.clone() {
            let sj = if fy == 0 { j + half } else { j };
            let row = &src[sj * BW..sj * BW + sw];
            let t = &mut tmp[j * 64..j * 64 + w];
            for v in t.iter_mut() {
                *v = 0;
            }
            for k in 0..T {
                let c = cx[k];
                let rk = &row[k..k + w];
                for i in 0..w {
                    t[i] += c * rk[i] as i32;
                }
            }
            for v in t.iter_mut() {
                *v >>= shift1;
            }
        }
        if fy == 0 {
            for j in 0..h {
                let t = &tmp[j * 64..j * 64 + w];
                let o = &mut out[j * w..j * w + w];
                for i in 0..w {
                    o[i] = t[i] as i16;
                }
            }
            return;
        }
        // Vertical over tmp, shift 6.
        let mut acc = [0i32; 64];
        for j in 0..h {
            let a = &mut acc[..w];
            for v in a.iter_mut() {
                *v = 0;
            }
            for k in 0..T {
                let c = cy[k];
                let t = &tmp[(j + k) * 64..(j + k) * 64 + w];
                for i in 0..w {
                    a[i] += c * t[i];
                }
            }
            let o = &mut out[j * w..j * w + w];
            for i in 0..w {
                o[i] = (a[i] >> 6) as i16;
            }
        }
    } else {
        // Vertical only, from the samples.
        let mut acc = [0i32; 64];
        for j in 0..h {
            let a = &mut acc[..w];
            for v in a.iter_mut() {
                *v = 0;
            }
            for k in 0..T {
                let c = cy[k];
                let s = &src[(j + k) * BW + half..(j + k) * BW + half + w];
                for i in 0..w {
                    a[i] += c * s[i] as i32;
                }
            }
            let o = &mut out[j * w..j * w + w];
            for i in 0..w {
                o[i] = (a[i] >> shift1) as i16;
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

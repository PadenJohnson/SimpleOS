//! HEVC in-loop filters, applied to the whole picture after it is decoded: the deblocking
//! filter (vertical edges, then horizontal) and sample adaptive offset (SAO).

use alloc::vec::Vec;

use super::tab::{BETA_TABLE, TC_TABLE};

/// What the filters need about the decoded picture (4x4 luma block granularity).
pub struct Info<'a> {
    pub w: usize,
    pub h: usize,
    /// Blocks per row (w / 4 rounded up).
    pub bw: usize,
    /// Boundary strength of each block's left / top edge (0 = not filtered).
    pub bs_v: &'a [u8],
    pub bs_h: &'a [u8],
    pub qp: &'a [i8],
    /// Samples that must stay as decoded (lossless or PCM blocks).
    pub keep: &'a [bool],
    /// Per block: beta and tc offsets of the slice it belongs to.
    pub offsets: &'a [(i8, i8)],
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub bit_depth: u32,
    pub chroma: bool,
}

pub fn chroma_qp(qpi: i32) -> i32 {
    const QPC: [i32; 14] = [29, 30, 31, 32, 33, 33, 34, 34, 35, 35, 36, 36, 37, 37];
    if qpi < 30 {
        qpi
    } else if qpi > 43 {
        qpi - 6
    } else {
        QPC[(qpi - 30) as usize]
    }
}

/// Filter one luma edge segment of 4 lines. `px(i, k)` reaches sample p_i / q_i of line k as
/// an index: p side is k-th line offset - (i+1)*step, q side + i*step.
#[allow(clippy::too_many_arguments)]
fn luma_segment(pl: &mut [u16], base: usize, step: isize, line: isize, bs: u8, qp: i32, beta_off: i32, tc_off: i32, bd: u32, keep_p: bool, keep_q: bool) {
    let s = |k: usize| base as isize + k as isize * line;
    let get = |pl: &[u16], k: usize, i: isize| pl[(s(k) + i * step) as usize] as i32; // i <0: p_{-i-1}
    let scale = 1 << (bd - 8);
    let beta = BETA_TABLE[(qp + beta_off).clamp(0, 51) as usize] as i32 * scale;
    let tc = TC_TABLE[(qp + 2 * (bs as i32 - 1) + tc_off).clamp(0, 53) as usize] as i32 * scale;
    if tc == 0 {
        return;
    }
    let p = |pl: &[u16], k: usize, i: isize| get(pl, k, -i - 1);
    let q = |pl: &[u16], k: usize, i: isize| get(pl, k, i);
    let dp0 = (p(pl, 0, 2) - 2 * p(pl, 0, 1) + p(pl, 0, 0)).abs();
    let dp3 = (p(pl, 3, 2) - 2 * p(pl, 3, 1) + p(pl, 3, 0)).abs();
    let dq0 = (q(pl, 0, 2) - 2 * q(pl, 0, 1) + q(pl, 0, 0)).abs();
    let dq3 = (q(pl, 3, 2) - 2 * q(pl, 3, 1) + q(pl, 3, 0)).abs();
    let d = dp0 + dq0 + dp3 + dq3;
    if d >= beta {
        return;
    }
    let strong = |pl: &[u16], k: usize, dpq: i32| -> bool {
        2 * dpq < (beta >> 2) && (p(pl, k, 3) - p(pl, k, 0)).abs() + (q(pl, k, 0) - q(pl, k, 3)).abs() < (beta >> 3) && (p(pl, k, 0) - q(pl, k, 0)).abs() < ((5 * tc + 1) >> 1)
    };
    let use_strong = strong(pl, 0, dp0 + dq0) && strong(pl, 3, dp3 + dq3);
    let de_p = dp0 + dp3 < ((beta + (beta >> 1)) >> 3);
    let de_q = dq0 + dq3 < ((beta + (beta >> 1)) >> 3);
    let max = (1 << bd) - 1;
    for k in 0..4 {
        let (p0, p1, p2, p3) = (p(pl, k, 0), p(pl, k, 1), p(pl, k, 2), p(pl, k, 3));
        let (q0, q1, q2, q3) = (q(pl, k, 0), q(pl, k, 1), q(pl, k, 2), q(pl, k, 3));
        let set = |pl: &mut [u16], i: isize, v: i32| pl[(s(k) + i * step) as usize] = v as u16;
        if use_strong {
            let c = 2 * tc;
            if !keep_p {
                set(pl, -1, ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3).clamp(p0 - c, p0 + c));
                set(pl, -2, ((p2 + p1 + p0 + q0 + 2) >> 2).clamp(p1 - c, p1 + c));
                set(pl, -3, ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3).clamp(p2 - c, p2 + c));
            }
            if !keep_q {
                set(pl, 0, ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3).clamp(q0 - c, q0 + c));
                set(pl, 1, ((p0 + q0 + q1 + q2 + 2) >> 2).clamp(q1 - c, q1 + c));
                set(pl, 2, ((p0 + q0 + q1 + 3 * q2 + 2 * q3 + 4) >> 3).clamp(q2 - c, q2 + c));
            }
        } else {
            let mut delta = (9 * (q0 - p0) - 3 * (q1 - p1) + 8) >> 4;
            if delta.abs() >= tc * 10 {
                continue;
            }
            delta = delta.clamp(-tc, tc);
            let tc2 = tc >> 1;
            if !keep_p {
                set(pl, -1, (p0 + delta).clamp(0, max));
                if de_p {
                    let dp = ((((p2 + p0 + 1) >> 1) - p1 + delta) >> 1).clamp(-tc2, tc2);
                    set(pl, -2, (p1 + dp).clamp(0, max));
                }
            }
            if !keep_q {
                set(pl, 0, (q0 - delta).clamp(0, max));
                if de_q {
                    let dq = ((((q2 + q0 + 1) >> 1) - q1 - delta) >> 1).clamp(-tc2, tc2);
                    set(pl, 1, (q1 + dq).clamp(0, max));
                }
            }
        }
    }
}

/// Deblock the picture in place (planes: luma stride `ys`, chroma stride `cs`).
pub fn deblock(y: &mut [u16], u: &mut [u16], v: &mut [u16], ys: usize, cs: usize, f: &Info) {
    let (bw, bh) = (f.bw, f.h.div_ceil(4));
    let bd = f.bit_depth;
    for dir in 0..2 {
        // dir 0: vertical edges (filter across x), dir 1: horizontal.
        for by in 0..bh {
            for bx in 0..bw {
                let (onx, ony) = if dir == 0 { (bx % 2 == 0 && bx > 0, true) } else { (true, by % 2 == 0 && by > 0) };
                if !onx || !ony {
                    continue;
                }
                let i = by * bw + bx;
                let bs = if dir == 0 { f.bs_v[i] } else { f.bs_h[i] };
                if bs == 0 {
                    continue;
                }
                let ni = if dir == 0 { i - 1 } else { i - bw };
                let qp = (f.qp[i] as i32 + f.qp[ni] as i32 + 1) >> 1;
                let (bo, to) = f.offsets[i];
                let (x0, y0) = (bx * 4, by * 4);
                let (step, line) = if dir == 0 { (1isize, ys as isize) } else { (ys as isize, 1isize) };
                luma_segment(y, y0 * ys + x0, step, line, bs, qp, bo as i32, to as i32, bd, f.keep[ni], f.keep[i]);
                // Chroma (4:2:0): edges on the 8x8 chroma grid with strength 2.
                if f.chroma && bs == 2 && ((dir == 0 && bx % 4 == 0) || (dir == 1 && by % 4 == 0)) {
                    for (pl, off) in [(&mut *u, f.cb_qp_offset), (&mut *v, f.cr_qp_offset)] {
                        let qpc = chroma_qp(qp + off);
                        let tc = TC_TABLE[(qpc + 2 + to as i32).clamp(0, 53) as usize] as i32 * (1 << (bd - 8));
                        if tc == 0 {
                            continue;
                        }
                        let (cx, cy) = (x0 / 2, y0 / 2);
                        let (st, ln) = if dir == 0 { (1isize, cs as isize) } else { (cs as isize, 1isize) };
                        let max = (1 << bd) - 1;
                        for k in 0..2 {
                            let base = (cy * cs + cx) as isize + k * ln;
                            let g = |pl: &[u16], o: isize| pl[(base + o * st) as usize] as i32;
                            let (p0, p1, q0, q1) = (g(pl, -1), g(pl, -2), g(pl, 0), g(pl, 1));
                            let d = ((((q0 - p0) << 2) + p1 - q1 + 4) >> 3).clamp(-tc, tc);
                            if !f.keep[ni] {
                                pl[(base - st) as usize] = (p0 + d).clamp(0, max) as u16;
                            }
                            if !f.keep[i] {
                                pl[base as usize] = (q0 - d).clamp(0, max) as u16;
                            }
                        }
                    }
                }
            }
        }
    }
}

/// SAO parameters of one CTB and plane.
#[derive(Clone, Copy, Default)]
pub struct Sao {
    /// 0 none, 1 band, 2 edge.
    pub kind: u8,
    pub offset: [i32; 5],
    pub band: u8,
    pub class: u8,
}

/// Apply SAO to a plane. `sao(ctb_x, ctb_y)`; `same(x0, y0, x1, y1)` says whether two
/// samples (in this plane's coordinates) may be used together across a slice/tile edge;
/// `keep(x, y)` marks samples to leave alone.
#[allow(clippy::too_many_arguments)]
pub fn sao(plane: &mut [u16], stride: usize, w: usize, h: usize, ctb: usize, bit_depth: u32, params: &dyn Fn(usize, usize) -> Sao, same: &dyn Fn(usize, usize, isize, isize) -> bool, keep: &dyn Fn(usize, usize) -> bool) {
    let src: Vec<u16> = plane.to_vec();
    let max = (1i32 << bit_depth) - 1;
    const DIRS: [[(isize, isize); 2]; 4] = [[(-1, 0), (1, 0)], [(0, -1), (0, 1)], [(-1, -1), (1, 1)], [(1, -1), (-1, 1)]];
    for cy in 0..h.div_ceil(ctb) {
        for cx in 0..w.div_ceil(ctb) {
            let p = params(cx, cy);
            if p.kind == 0 {
                continue;
            }
            let (x0, y0) = (cx * ctb, cy * ctb);
            let (x1, y1) = ((x0 + ctb).min(w), (y0 + ctb).min(h));
            // Lossless/PCM samples are rare: look once per CTB.
            let mut any_keep = false;
            for yy in (y0..y1).step_by(2) {
                for xx in (x0..x1).step_by(2) {
                    any_keep |= keep(xx, yy);
                }
            }
            if p.kind == 1 {
                let shift = bit_depth - 5;
                let mut table = [0usize; 32];
                for k in 0..4 {
                    table[(k + p.band as usize) & 31] = k + 1;
                }
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        if any_keep && keep(xx, yy) {
                            continue;
                        }
                        let s = src[yy * stride + xx] as i32;
                        let o = p.offset[table[(s >> shift) as usize]];
                        plane[yy * stride + xx] = (s + o).clamp(0, max) as u16;
                    }
                }
            } else {
                let [(ax, ay), (bx, by)] = DIRS[p.class as usize];
                for yy in y0..y1 {
                    for xx in x0..x1 {
                        let (xa, ya, xb, yb) = (xx as isize + ax, yy as isize + ay, xx as isize + bx, yy as isize + by);
                        if xa < 0 || ya < 0 || xb < 0 || yb < 0 || xa >= w as isize || xb >= w as isize || ya >= h as isize || yb >= h as isize {
                            continue;
                        }
                        if any_keep && keep(xx, yy) {
                            continue;
                        }
                        let border = xx == x0 || xx + 1 == x1 || yy == y0 || yy + 1 == y1;
                        if border && (!same(xx, yy, xa, ya) || !same(xx, yy, xb, yb)) {
                            continue;
                        }
                        let s = src[yy * stride + xx] as i32;
                        let a = src[ya as usize * stride + xa as usize] as i32;
                        let b = src[yb as usize * stride + xb as usize] as i32;
                        let e = 2 + (s - a).signum() + (s - b).signum();
                        let idx = match e {
                            0 => 1,
                            1 => 2,
                            2 => 0,
                            k => k as usize,
                        };
                        plane[yy * stride + xx] = (s + p.offset[idx]).clamp(0, max) as u16;
                    }
                }
            }
        }
    }
}

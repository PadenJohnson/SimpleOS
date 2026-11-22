//! VP8 pictures (older WebM files). Ported from FFmpeg's vp8.c / vp8dsp.c (LGPL 2.1 or later;
//! David Conrad, Ronald S. Bultje, Fiona Glaser): frame header, macroblock modes, motion
//! vectors with split partitions, tokens, Walsh-Hadamard + DCT, intra prediction with
//! libvpx's edges, six-tap / bilinear motion compensation, normal and simple loop filters.

use crate::media::dec::{Pic as OutPic, VideoDec};
use crate::media::vp8tab::*;
use crate::media::vp9::Rac;
use crate::video::h264d::Frame;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;

const MODE_I4X4: u8 = 4;
const MV_ZERO: u8 = 5;
const MV_MV: u8 = 6;
const MV_SPLIT: u8 = 7;
const SPLIT_NONE: u8 = 4;
// 4x4 intra modes (FFmpeg's numbering).
const B_VE: u8 = 0;
const B_HE: u8 = 1;
const B_DC: u8 = 2;
const B_LD: u8 = 3;
const B_RD: u8 = 4;
const B_VR: u8 = 5;
const B_HD: u8 = 6;
const B_VL: u8 = 7;
const B_HU: u8 = 8;
const B_TM: u8 = 9;

const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];
const COEFF_BAND: [usize; 16] = [0, 1, 2, 3, 6, 4, 5, 6, 6, 6, 6, 6, 6, 6, 6, 7];
const TREE16_INTRA: [[i8; 2]; 4] = [[-4, 1], [2, 3], [0, -2], [-1, -3]];
const TREE16_INTER: [[i8; 2]; 4] = [[0, 1], [2, 3], [-2, -1], [-3, -4]];
const TREE8C: [[i8; 2]; 3] = [[0, 1], [-2, 2], [-1, -3]];
const TREE4: [[i8; 2]; 9] = [[-2, 1], [-9, 2], [0, 3], [4, 6], [-1, 5], [-4, -5], [-3, 7], [-7, 8], [-6, -8]];
const PRED16_PROB_INTRA: [u8; 4] = [145, 156, 163, 128];
const PRED16_PROB_INTER: [u8; 4] = [112, 86, 140, 37];
const PRED8C_PROB_INTRA: [u8; 3] = [142, 114, 183];
const PRED8C_PROB_INTER: [u8; 3] = [162, 101, 204];
const PRED4_PROB_INTER: [u8; 9] = [120, 90, 79, 133, 87, 85, 80, 111, 151];
/// 16x16 mode -> 4x4 context mode (DC, H, V, TM).
const PRED4X4_MODE: [u8; 4] = [B_DC, B_HE, B_VE, B_TM];
const MODE_CONTEXTS: [[u8; 4]; 6] = [[7, 1, 1, 143], [14, 18, 14, 107], [135, 64, 57, 68], [60, 56, 128, 65], [159, 134, 128, 34], [234, 188, 128, 28]];
const MBSPLITS: [[u8; 16]; 5] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1, 0, 0, 1, 1],
    [0, 0, 1, 1, 0, 0, 1, 1, 2, 2, 3, 3, 2, 2, 3, 3],
    [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15],
    [0; 16],
];
const MBFIRSTIDX: [[u8; 16]; 4] = [[0, 8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], [0, 2, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], [0, 2, 8, 10, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0], [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]];
const MBSPLIT_COUNT: [usize; 4] = [2, 2, 4, 16];
const MBSPLIT_PROB: [u8; 3] = [110, 111, 150];
const SUBMV_PROB: [[u8; 3]; 5] = [[147, 136, 18], [106, 145, 1], [179, 121, 1], [223, 1, 34], [208, 1, 1]];
const DCT_CAT_PROB: [&[u8]; 4] = [&[173, 148, 140], &[176, 155, 140, 135], &[180, 157, 141, 134, 130], &[254, 254, 243, 230, 196, 177, 153, 140, 133, 130, 129]];
const SUBPEL: [[i32; 6]; 7] = [[0, 6, 123, 12, 1, 0], [2, 11, 108, 36, 8, 1], [0, 9, 93, 50, 6, 0], [3, 16, 77, 77, 16, 3], [0, 6, 50, 93, 9, 0], [1, 8, 36, 108, 11, 2], [0, 1, 12, 123, 6, 0]];
const HEV_LUT: [[u8; 64]; 2] = [
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3],
    [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2, 2],
];

#[derive(Clone, Copy, Default, PartialEq, Eq)]
struct Mv {
    x: i16,
    y: i16,
}

#[derive(Clone, Copy, Default)]
struct Mb {
    skip: bool,
    mode: u8,
    ref_frame: u8,
    partitioning: u8,
    chroma_mode: u8,
    segment: u8,
    i4: [u8; 16],
    mv: Mv,
    bmv: [Mv; 16],
}

#[derive(Clone)]
struct Probs {
    segmentid: [u8; 3],
    mbskip: u8,
    intra: u8,
    last: u8,
    golden: u8,
    pred16: [u8; 4],
    pred8c: [u8; 3],
    /// [4][16][3][11]
    token: Vec<u8>,
    mvc: [[u8; 19]; 2],
}

struct Pic {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    seg: Vec<u8>,
}

#[derive(Clone, Copy, Default)]
struct Strength {
    level: i32,
    inner_limit: i32,
    inner: bool,
}

pub struct Vp8Dec {
    keyframe: bool,
    profile: u8,
    invisible: bool,
    w: usize,
    h: usize,
    mbw: usize,
    mbh: usize,
    seg_enabled: bool,
    seg_abs: bool,
    seg_update_map: bool,
    seg_quant: [i32; 4],
    seg_lf: [i32; 4],
    filter_simple: bool,
    filter_level: i32,
    sharpness: i32,
    lf_delta_enabled: bool,
    lf_mode: [i32; 8],
    lf_ref: [i32; 4],
    /// Per segment: luma dc, luma ac, y2 dc, y2 ac, chroma dc, chroma ac.
    qmat: [[i32; 6]; 4],
    sign_bias: [bool; 4],
    prob: [Probs; 2],
    update_last: bool,
    update_golden: i8,
    update_altref: i8,
    update_probabilities: bool,
    mbskip_enabled: bool,
    full_range: bool,
    /// 0 last decoded, 1 previous (last), 2 golden, 3 altref.
    refs: [Option<Rc<Pic>>; 4],
    mbs: Vec<Mb>,
    i4_top: Vec<u8>,
    i4_left: [u8; 4],
    top_nnz: Vec<[u8; 9]>,
    left_nnz: [u8; 9],
    saved: [Vec<u8>; 3],
    block: [[i16; 16]; 25],
    nnz: [u8; 25],
    strengths: Vec<Strength>,
    out: VecDeque<Frame>,
}

fn sint(c: &mut Rac, bits: u32) -> i32 {
    if !c.bit() {
        return 0;
    }
    let v = c.uint(bits) as i32;
    if c.bit() {
        -v
    } else {
        v
    }
}

fn read_mv_component(c: &mut Rac, p: &[u8; 19]) -> i32 {
    let mut x = 0i32;
    if c.prob(p[0]) {
        for i in 0..3 {
            x += (c.prob(p[9 + i]) as i32) << i;
        }
        for i in (4..=9).rev() {
            x += (c.prob(p[9 + i]) as i32) << i;
        }
        if x & 0xFFF0 == 0 || c.prob(p[12]) {
            x += 8;
        }
    } else {
        let mut ps = 2usize;
        let bit = c.prob(p[ps]) as usize;
        ps += 1 + 3 * bit;
        x += 4 * bit as i32;
        let bit = c.prob(p[ps]) as usize;
        ps += 1 + bit;
        x += 2 * bit as i32;
        x += c.prob(p[ps]) as i32;
    }
    if x != 0 && c.prob(p[1]) {
        -x
    } else {
        x
    }
}

/// Tokens of one 4x4 block; returns 0 or the index of the last coefficient plus one.
fn block_coeffs(c: &mut Rac, blk: &mut [i16; 16], tok: &[u8], mut i: usize, ctx: usize, q: (i32, i32)) -> usize {
    let tp = |i: usize, ctx: usize| (i * 3 + ctx) * 11;
    let mut t = tp(i, ctx);
    if !c.prob(tok[t]) {
        return 0;
    }
    loop {
        if !c.prob(tok[t + 1]) {
            i += 1;
            if i == 16 {
                break;
            }
            t = tp(i, 0);
            continue;
        }
        let coeff: i32;
        if !c.prob(tok[t + 2]) {
            coeff = 1;
            t = tp(i + 1, 1);
        } else {
            if !c.prob(tok[t + 3]) {
                let mut v = c.prob(tok[t + 4]) as i32;
                if v != 0 {
                    v += c.prob(tok[t + 5]) as i32;
                }
                coeff = v + 2;
            } else if !c.prob(tok[t + 6]) {
                if !c.prob(tok[t + 7]) {
                    coeff = 5 + c.prob(159) as i32;
                } else {
                    coeff = 7 + ((c.prob(165) as i32) << 1) + c.prob(145) as i32;
                }
            } else {
                let a = c.prob(tok[t + 8]) as usize;
                let b = c.prob(tok[t + 9 + a]) as usize;
                let cat = (a << 1) + b;
                let mut v = 0i32;
                for &p in DCT_CAT_PROB[cat] {
                    v = (v << 1) + c.prob(p) as i32;
                }
                coeff = 3 + (8 << cat) + v;
            }
            t = tp(i + 1, 2);
        }
        let q = if i != 0 { q.1 } else { q.0 };
        let v = if c.bit() { -coeff } else { coeff };
        blk[ZIGZAG[i]] = (v * q) as i16;
        i += 1;
        if i >= 16 {
            break;
        }
        if !c.prob(tok[t]) {
            break;
        }
    }
    i
}

#[inline]
fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

fn mul_20091(a: i32) -> i32 {
    ((a * 20091) >> 16) + a
}
fn mul_35468(a: i32) -> i32 {
    (a * 35468) >> 16
}

fn idct_add(d: &mut [u8], o: usize, s: usize, b: &mut [i16; 16]) {
    let mut tmp = [0i32; 16];
    for i in 0..4 {
        let (b0, b1, b2, b3) = (b[i] as i32, b[4 + i] as i32, b[8 + i] as i32, b[12 + i] as i32);
        let t0 = b0 + b2;
        let t1 = b0 - b2;
        let t2 = mul_35468(b1) - mul_20091(b3);
        let t3 = mul_20091(b1) + mul_35468(b3);
        tmp[i * 4] = (t0 + t3) as i16 as i32;
        tmp[i * 4 + 1] = (t1 + t2) as i16 as i32;
        tmp[i * 4 + 2] = (t1 - t2) as i16 as i32;
        tmp[i * 4 + 3] = (t0 - t3) as i16 as i32;
    }
    *b = [0; 16];
    for i in 0..4 {
        let t0 = tmp[i] + tmp[8 + i];
        let t1 = tmp[i] - tmp[8 + i];
        let t2 = mul_35468(tmp[4 + i]) - mul_20091(tmp[12 + i]);
        let t3 = mul_20091(tmp[4 + i]) + mul_35468(tmp[12 + i]);
        let r = o + i * s;
        d[r] = clip(d[r] as i32 + ((t0 + t3 + 4) >> 3));
        d[r + 1] = clip(d[r + 1] as i32 + ((t1 + t2 + 4) >> 3));
        d[r + 2] = clip(d[r + 2] as i32 + ((t1 - t2 + 4) >> 3));
        d[r + 3] = clip(d[r + 3] as i32 + ((t0 - t3 + 4) >> 3));
    }
}

fn idct_dc_add(d: &mut [u8], o: usize, s: usize, b: &mut [i16; 16]) {
    let dc = (b[0] as i32 + 4) >> 3;
    b[0] = 0;
    for y in 0..4 {
        for x in 0..4 {
            let p = &mut d[o + y * s + x];
            *p = clip(*p as i32 + dc);
        }
    }
}

fn add_block(d: &mut [u8], o: usize, s: usize, b: &mut [i16; 16], nnz: u8) {
    if nnz == 1 {
        idct_dc_add(d, o, s, b);
    } else if nnz > 1 {
        idct_add(d, o, s, b);
    }
}

/// Inverse Walsh-Hadamard of the luma DC block into each luma block's DC.
fn luma_dc_wht(blocks: &mut [[i16; 16]; 25], dc_only: bool) {
    if dc_only {
        let v = ((blocks[24][0] as i32 + 3) >> 3) as i16;
        blocks[24][0] = 0;
        for b in blocks.iter_mut().take(16) {
            b[0] = v;
        }
        return;
    }
    let mut dc = [0i32; 16];
    for k in 0..16 {
        dc[k] = blocks[24][k] as i32;
    }
    for i in 0..4 {
        let t0 = dc[i] + dc[12 + i];
        let t1 = dc[4 + i] + dc[8 + i];
        let t2 = dc[4 + i] - dc[8 + i];
        let t3 = dc[i] - dc[12 + i];
        dc[i] = (t0 + t1) as i16 as i32;
        dc[4 + i] = (t3 + t2) as i16 as i32;
        dc[8 + i] = (t0 - t1) as i16 as i32;
        dc[12 + i] = (t3 - t2) as i16 as i32;
    }
    for i in 0..4 {
        let t0 = dc[i * 4] + dc[i * 4 + 3] + 3;
        let t1 = dc[i * 4 + 1] + dc[i * 4 + 2];
        let t2 = dc[i * 4 + 1] - dc[i * 4 + 2];
        let t3 = dc[i * 4] - dc[i * 4 + 3] + 3;
        blocks[i * 4][0] = ((t0 + t1) >> 3) as i16;
        blocks[i * 4 + 1][0] = ((t3 + t2) >> 3) as i16;
        blocks[i * 4 + 2][0] = ((t0 - t1) >> 3) as i16;
        blocks[i * 4 + 3][0] = ((t3 - t2) >> 3) as i16;
    }
    blocks[24] = [0; 16];
}

// ---- Loop filter ----

fn clip8(v: i32) -> i32 {
    v.clamp(-128, 127)
}

struct Lf<'a> {
    d: &'a mut [u8],
}

impl Lf<'_> {
    #[inline]
    fn px(&self, at: isize, sb: isize, n: isize) -> i32 {
        self.d[(at + n * sb) as usize] as i32
    }
    fn simple_limit(&self, at: isize, sb: isize, flim: i32) -> bool {
        let (p1, p0, q0, q1) = (self.px(at, sb, -2), self.px(at, sb, -1), self.px(at, sb, 0), self.px(at, sb, 1));
        2 * (p0 - q0).abs() + ((p1 - q1).abs() >> 1) <= flim
    }
    fn normal_limit(&self, at: isize, sb: isize, e: i32, i: i32) -> bool {
        let p: [i32; 8] = core::array::from_fn(|k| self.px(at, sb, k as isize - 4));
        let (p3, p2, p1, p0, q0, q1, q2, q3) = (p[0], p[1], p[2], p[3], p[4], p[5], p[6], p[7]);
        self.simple_limit(at, sb, e) && (p3 - p2).abs() <= i && (p2 - p1).abs() <= i && (p1 - p0).abs() <= i && (q3 - q2).abs() <= i && (q2 - q1).abs() <= i && (q1 - q0).abs() <= i
    }
    fn hev(&self, at: isize, sb: isize, t: i32) -> bool {
        (self.px(at, sb, -2) - self.px(at, sb, -1)).abs() > t || (self.px(at, sb, 1) - self.px(at, sb, 0)).abs() > t
    }
    fn set(&mut self, at: isize, sb: isize, n: isize, v: i32) {
        self.d[(at + n * sb) as usize] = clip(v);
    }
    fn common(&mut self, at: isize, sb: isize, is4tap: bool) {
        let (p1, p0, q0, q1) = (self.px(at, sb, -2), self.px(at, sb, -1), self.px(at, sb, 0), self.px(at, sb, 1));
        let mut a = 3 * (q0 - p0);
        if is4tap {
            a += clip8(p1 - q1);
        }
        let a = clip8(a);
        let f1 = (a + 4).min(127) >> 3;
        let f2 = (a + 3).min(127) >> 3;
        self.set(at, sb, -1, p0 + f2);
        self.set(at, sb, 0, q0 - f1);
        if !is4tap {
            let a = (f1 + 1) >> 1;
            self.set(at, sb, -2, p1 + a);
            self.set(at, sb, 1, q1 - a);
        }
    }
    fn mbedge(&mut self, at: isize, sb: isize) {
        let p: [i32; 6] = core::array::from_fn(|k| self.px(at, sb, k as isize - 3));
        let (p2, p1, p0, q0, q1, q2) = (p[0], p[1], p[2], p[3], p[4], p[5]);
        let w = clip8(p1 - q1);
        let w = clip8(w + 3 * (q0 - p0));
        let a0 = (27 * w + 63) >> 7;
        let a1 = (18 * w + 63) >> 7;
        let a2 = (9 * w + 63) >> 7;
        self.set(at, sb, -3, p2 + a2);
        self.set(at, sb, -2, p1 + a1);
        self.set(at, sb, -1, p0 + a0);
        self.set(at, sb, 0, q0 - a0);
        self.set(at, sb, 1, q1 - a1);
        self.set(at, sb, 2, q2 - a2);
    }
    /// Macroblock edge filter over `n` lines.
    #[allow(clippy::too_many_arguments)]
    fn edge(&mut self, at: usize, sa: isize, sb: isize, n: usize, e: i32, i: i32, hev: i32, inner: bool) {
        for k in 0..n as isize {
            let p = at as isize + k * sa;
            if self.normal_limit(p, sb, e, i) {
                if self.hev(p, sb, hev) {
                    self.common(p, sb, true);
                } else if inner {
                    self.common(p, sb, false);
                } else {
                    self.mbedge(p, sb);
                }
            }
        }
    }
    fn simple(&mut self, at: usize, sa: isize, sb: isize, flim: i32) {
        for k in 0..16isize {
            let p = at as isize + k * sa;
            if self.simple_limit(p, sb, flim) {
                self.common(p, sb, true);
            }
        }
    }
}

// ---- Motion compensation ----

/// Predict bw x bh at integer (x, y) + eighth fraction (mx, my) from a plane of size w x h.
#[allow(clippy::too_many_arguments)]
fn mc(dst: &mut [u8], doff: usize, ds: usize, r: &[u8], w: i32, h: i32, x: i32, y: i32, mx: usize, my: usize, bw: usize, bh: usize, bilinear: bool) {
    let rs = w as usize;
    let (ex0, ex1) = if mx != 0 { (2, 3) } else { (0, 0) };
    let (ey0, ey1) = if my != 0 { (2, 3) } else { (0, 0) };
    let sw = bw + ex0 + ex1;
    let sh = bh + ey0 + ey1;
    let (sx, sy) = (x - ex0 as i32, y - ey0 as i32);
    let mut win = [0u8; 24 * 24];
    let inside = sx >= 0 && sy >= 0 && sx + sw as i32 <= w && sy + sh as i32 <= h;
    let (src, sst, so): (&[u8], usize, usize) = if inside {
        (r, rs, sy as usize * rs + sx as usize)
    } else {
        for j in 0..sh {
            let yy = (sy + j as i32).clamp(0, h - 1) as usize * rs;
            for i in 0..sw {
                win[j * 24 + i] = r[yy + (sx + i as i32).clamp(0, w - 1) as usize];
            }
        }
        (&win, 24, 0)
    };
    let base = so + ey0 * sst + ex0;
    if mx == 0 && my == 0 {
        for j in 0..bh {
            dst[doff + j * ds..doff + j * ds + bw].copy_from_slice(&src[base + j * sst..base + j * sst + bw]);
        }
        return;
    }
    if bilinear {
        let bil = |a: u8, b: u8, f: usize| ((((8 - f) as i32) * a as i32 + f as i32 * b as i32 + 4) >> 3) as u8;
        if my == 0 {
            for j in 0..bh {
                for i in 0..bw {
                    let o = base + j * sst + i;
                    dst[doff + j * ds + i] = bil(src[o], src[o + 1], mx);
                }
            }
        } else if mx == 0 {
            for j in 0..bh {
                for i in 0..bw {
                    let o = base + j * sst + i;
                    dst[doff + j * ds + i] = bil(src[o], src[o + sst], my);
                }
            }
        } else {
            let mut tmp = [0u8; 17 * 16];
            for j in 0..bh + 1 {
                for i in 0..bw {
                    let o = base + j * sst + i;
                    tmp[j * 16 + i] = bil(src[o], src[o + 1], mx);
                }
            }
            for j in 0..bh {
                for i in 0..bw {
                    dst[doff + j * ds + i] = bil(tmp[j * 16 + i], tmp[(j + 1) * 16 + i], my);
                }
            }
        }
        return;
    }
    let tap6 = |s: &[u8], o: usize, st: usize, f: &[i32; 6]| -> u8 {
        let v = |k: isize| s[(o as isize + k * st as isize) as usize] as i32;
        clip((f[2] * v(0) - f[1] * v(-1) + f[0] * v(-2) + f[3] * v(1) - f[4] * v(2) + f[5] * v(3) + 64) >> 7)
    };
    if my == 0 {
        let f = &SUBPEL[mx - 1];
        for j in 0..bh {
            for i in 0..bw {
                dst[doff + j * ds + i] = tap6(src, base + j * sst + i, 1, f);
            }
        }
    } else if mx == 0 {
        let f = &SUBPEL[my - 1];
        for j in 0..bh {
            for i in 0..bw {
                dst[doff + j * ds + i] = tap6(src, base + j * sst + i, sst, f);
            }
        }
    } else {
        let fx = &SUBPEL[mx - 1];
        let fy = &SUBPEL[my - 1];
        let mut tmp = [0u8; 21 * 16];
        let b0 = base - 2 * sst;
        for j in 0..bh + 5 {
            for i in 0..bw {
                tmp[j * 16 + i] = tap6(src, b0 + j * sst + i, 1, fx);
            }
        }
        for j in 0..bh {
            for i in 0..bw {
                dst[doff + j * ds + i] = tap6(&tmp, (j + 2) * 16 + i, 16, fy);
            }
        }
    }
}

fn avg3(a: i32, b: i32, c: i32) -> u8 {
    ((a + 2 * b + c + 2) >> 2) as u8
}
fn avg2(a: i32, b: i32) -> u8 {
    ((a + b + 1) >> 1) as u8
}

/// VP8 4x4 intra prediction. `a`: a[0] top-left, a[1..9] above + above-right; `l`: left
/// column, top to bottom.
fn pred4(mode: u8, d: &mut [u8], o: usize, s: usize, a: &[i32; 9], l: &[i32; 4]) {
    let p = a[0];
    let t = |i: usize| a[1 + i];
    let mut set = |x: usize, y: usize, v: u8| d[o + y * s + x] = v;
    match mode {
        B_DC => {
            let v = ((t(0) + t(1) + t(2) + t(3) + l[0] + l[1] + l[2] + l[3] + 4) >> 3) as u8;
            for y in 0..4 {
                for x in 0..4 {
                    set(x, y, v);
                }
            }
        }
        B_TM => {
            for y in 0..4 {
                for x in 0..4 {
                    set(x, y, clip(l[y] + t(x) - p));
                }
            }
        }
        B_VE => {
            for x in 0..4 {
                let v = avg3(if x == 0 { p } else { t(x - 1) }, t(x), t(x + 1));
                for y in 0..4 {
                    set(x, y, v);
                }
            }
        }
        B_HE => {
            let rows = [avg3(p, l[0], l[1]), avg3(l[0], l[1], l[2]), avg3(l[1], l[2], l[3]), avg3(l[2], l[3], l[3])];
            for y in 0..4 {
                for x in 0..4 {
                    set(x, y, rows[y]);
                }
            }
        }
        B_LD => {
            for y in 0..4 {
                for x in 0..4 {
                    let i = x + y;
                    let v = if i == 6 { avg3(t(6), t(7), t(7)) } else { avg3(t(i), t(i + 1), t(i + 2)) };
                    set(x, y, v);
                }
            }
        }
        B_VL => {
            set(0, 0, avg2(t(0), t(1)));
            set(0, 1, avg3(t(0), t(1), t(2)));
            let v = avg2(t(1), t(2));
            set(1, 0, v);
            set(0, 2, v);
            let v = avg3(t(1), t(2), t(3));
            set(1, 1, v);
            set(0, 3, v);
            let v = avg2(t(2), t(3));
            set(2, 0, v);
            set(1, 2, v);
            let v = avg3(t(2), t(3), t(4));
            set(2, 1, v);
            set(1, 3, v);
            let v = avg2(t(3), t(4));
            set(3, 0, v);
            set(2, 2, v);
            let v = avg3(t(3), t(4), t(5));
            set(3, 1, v);
            set(2, 3, v);
            set(3, 2, avg3(t(4), t(5), t(6)));
            set(3, 3, avg3(t(5), t(6), t(7)));
        }
        B_RD => {
            // Edge from bottom-left to top-right: l3 l2 l1 l0 p a0 a1 a2 a3.
            let e = [l[3], l[2], l[1], l[0], p, t(0), t(1), t(2), t(3)];
            for y in 0..4 {
                for x in 0..4 {
                    let i = 3 - y + x;
                    set(x, y, avg3(e[i], e[i + 1], e[i + 2]));
                }
            }
        }
        B_VR => {
            let (a0, a1, a2, a3) = (t(0), t(1), t(2), t(3));
            let (l0, l1, l2) = (l[0], l[1], l[2]);
            set(0, 3, avg3(l0, l1, l2));
            set(0, 2, avg3(p, l0, l1));
            let v = avg2(p, a0);
            set(0, 0, v);
            set(1, 2, v);
            let v = avg3(l0, p, a0);
            set(0, 1, v);
            set(1, 3, v);
            let v = avg2(a0, a1);
            set(1, 0, v);
            set(2, 2, v);
            let v = avg3(p, a0, a1);
            set(1, 1, v);
            set(2, 3, v);
            let v = avg2(a1, a2);
            set(2, 0, v);
            set(3, 2, v);
            let v = avg3(a0, a1, a2);
            set(2, 1, v);
            set(3, 3, v);
            set(3, 0, avg2(a2, a3));
            set(3, 1, avg3(a1, a2, a3));
        }
        B_HD => {
            let (l0, l1, l2, l3) = (l[0], l[1], l[2], l[3]);
            let (a0, a1, a2) = (t(0), t(1), t(2));
            set(2, 0, avg3(p, a0, a1));
            set(3, 0, avg3(a0, a1, a2));
            let v = avg2(p, l0);
            set(0, 0, v);
            set(2, 1, v);
            let v = avg3(a0, p, l0);
            set(1, 0, v);
            set(3, 1, v);
            let v = avg2(l0, l1);
            set(0, 1, v);
            set(2, 2, v);
            let v = avg3(p, l0, l1);
            set(1, 1, v);
            set(3, 2, v);
            let v = avg2(l1, l2);
            set(0, 2, v);
            set(2, 3, v);
            let v = avg3(l0, l1, l2);
            set(1, 2, v);
            set(3, 3, v);
            set(0, 3, avg2(l2, l3));
            set(1, 3, avg3(l1, l2, l3));
        }
        B_HU => {
            let (l0, l1, l2, l3) = (l[0], l[1], l[2], l[3]);
            set(0, 0, avg2(l0, l1));
            set(1, 0, avg3(l0, l1, l2));
            let v = avg2(l1, l2);
            set(2, 0, v);
            set(0, 1, v);
            let v = avg3(l1, l2, l3);
            set(3, 0, v);
            set(1, 1, v);
            let v = avg2(l2, l3);
            set(2, 1, v);
            set(0, 2, v);
            let v = avg3(l2, l3, l3);
            set(3, 1, v);
            set(1, 2, v);
            for (x, y) in [(2, 2), (3, 2), (0, 3), (1, 3), (2, 3), (3, 3)] {
                set(x, y, l3 as u8);
            }
        }
        _ => {}
    }
}

/// 16x16 / 8x8 prediction: DC 0, H 1, V 2, TM 3, with libvpx's frame-edge rules.
#[allow(clippy::too_many_arguments)]
fn pred_block(mode: u8, n: usize, d: &mut [u8], o: usize, s: usize, a: &[u8], l: &[u8], p: u8, has_top: bool, has_left: bool) {
    match mode {
        0 => {
            let log2 = n.trailing_zeros();
            let st: i32 = a[..n].iter().map(|&v| v as i32).sum();
            let sl: i32 = l[..n].iter().map(|&v| v as i32).sum();
            let v = match (has_top, has_left) {
                (true, true) => (st + sl + n as i32) >> (log2 + 1),
                (true, false) => (st + (n as i32 >> 1)) >> log2,
                (false, true) => (sl + (n as i32 >> 1)) >> log2,
                _ => 128,
            } as u8;
            for y in 0..n {
                d[o + y * s..o + y * s + n].iter_mut().for_each(|x| *x = v);
            }
        }
        1 => {
            for y in 0..n {
                let v = l[y];
                d[o + y * s..o + y * s + n].iter_mut().for_each(|x| *x = v);
            }
        }
        2 => {
            for y in 0..n {
                d[o + y * s..o + y * s + n].copy_from_slice(&a[..n]);
            }
        }
        _ => {
            for y in 0..n {
                for x in 0..n {
                    d[o + y * s + x] = clip(l[y] as i32 + a[x] as i32 - p as i32);
                }
            }
        }
    }
}

impl Vp8Dec {
    pub fn new() -> Vp8Dec {
        let p = Probs { segmentid: [255; 3], mbskip: 0, intra: 0, last: 0, golden: 0, pred16: PRED16_PROB_INTER, pred8c: PRED8C_PROB_INTER, token: vec![0; 4 * 16 * 3 * 11], mvc: [[0; 19]; 2] };
        Vp8Dec {
            keyframe: false,
            profile: 0,
            invisible: false,
            w: 0,
            h: 0,
            mbw: 0,
            mbh: 0,
            seg_enabled: false,
            seg_abs: false,
            seg_update_map: false,
            seg_quant: [0; 4],
            seg_lf: [0; 4],
            filter_simple: false,
            filter_level: 0,
            sharpness: 0,
            lf_delta_enabled: false,
            lf_mode: [0; 8],
            lf_ref: [0; 4],
            qmat: [[0; 6]; 4],
            sign_bias: [false; 4],
            prob: [p.clone(), p],
            update_last: false,
            update_golden: -1,
            update_altref: -1,
            update_probabilities: true,
            mbskip_enabled: false,
            full_range: false,
            refs: [None, None, None, None],
            mbs: Vec::new(),
            i4_top: Vec::new(),
            i4_left: [B_DC; 4],
            top_nnz: Vec::new(),
            left_nnz: [0; 9],
            saved: [Vec::new(), Vec::new(), Vec::new()],
            block: [[0; 16]; 25],
            nnz: [0; 25],
            strengths: Vec::new(),
            out: VecDeque::new(),
        }
    }

    fn decode_frame(&mut self, buf: &[u8]) -> Option<()> {
        if buf.len() < 3 {
            return None;
        }
        self.keyframe = buf[0] & 1 == 0;
        self.profile = (buf[0] >> 1) & 7;
        self.invisible = buf[0] & 0x10 == 0;
        let header_size = (u32::from_le_bytes([buf[0], buf[1], buf[2], 0]) >> 5) as usize;
        let mut b = &buf[3..];
        let (mut w, mut h) = (self.w, self.h);
        if self.keyframe {
            if b.len() < 7 || b[0] != 0x9d || b[1] != 0x01 || b[2] != 0x2a {
                return None;
            }
            w = (u16::from_le_bytes([b[3], b[4]]) & 0x3fff) as usize;
            h = (u16::from_le_bytes([b[5], b[6]]) & 0x3fff) as usize;
            b = &b[7..];
            self.update_golden = 0;
            self.update_altref = 0;
            let p = &mut self.prob[0];
            for i in 0..4 {
                for j in 0..16 {
                    let src = ((i * 8 + COEFF_BAND[j]) * 3) * 11;
                    let dst = ((i * 16 + j) * 3) * 11;
                    p.token[dst..dst + 33].copy_from_slice(&TOKEN_DEFAULT_PROBS[src..src + 33]);
                }
            }
            p.pred16 = PRED16_PROB_INTER;
            p.pred8c = PRED8C_PROB_INTER;
            for k in 0..2 {
                p.mvc[k].copy_from_slice(&MV_DEFAULT_PROB[k * 19..k * 19 + 19]);
            }
            self.seg_enabled = false;
            self.seg_abs = false;
            self.seg_update_map = false;
            self.seg_quant = [0; 4];
            self.seg_lf = [0; 4];
            self.lf_delta_enabled = false;
            self.lf_mode = [0; 8];
            self.lf_ref = [0; 4];
        } else if self.refs[1].is_none() || self.refs[2].is_none() || self.refs[3].is_none() {
            return None;
        }
        if header_size > b.len() || w == 0 || h == 0 {
            return None;
        }
        let mut c = Rac::new(&b[..header_size]);
        let b = &b[header_size..];
        if self.keyframe {
            let _colorspace = c.bit();
            self.full_range = c.bit();
        }
        self.seg_enabled = c.bit();
        if self.seg_enabled {
            self.seg_update_map = c.bit();
            let update_data = c.bit();
            if update_data {
                self.seg_abs = c.bit();
                for i in 0..4 {
                    self.seg_quant[i] = sint(&mut c, 7);
                }
                for i in 0..4 {
                    self.seg_lf[i] = sint(&mut c, 6);
                }
            }
            if self.seg_update_map {
                for i in 0..3 {
                    self.prob[0].segmentid[i] = if c.bit() { c.uint(8) as u8 } else { 255 };
                }
            }
        } else {
            self.seg_update_map = false;
        }
        self.filter_simple = c.bit();
        self.filter_level = c.uint(6) as i32;
        self.sharpness = c.uint(3) as i32;
        self.lf_delta_enabled = c.bit();
        if self.lf_delta_enabled && c.bit() {
            for i in 0..4 {
                if c.bit() {
                    let v = c.uint(6) as i32;
                    self.lf_ref[i] = if c.bit() { -v } else { v };
                }
            }
            for i in 4..8 {
                if c.bit() {
                    let v = c.uint(6) as i32;
                    self.lf_mode[i] = if c.bit() { -v } else { v };
                }
            }
        }
        // Coefficient partitions.
        let nparts = 1usize << c.uint(2);
        if b.len() < 3 * (nparts - 1) {
            return None;
        }
        let sizes = &b[..3 * (nparts - 1)];
        let mut data = &b[3 * (nparts - 1)..];
        let mut parts = Vec::with_capacity(nparts);
        for i in 0..nparts {
            let size = if i + 1 < nparts { u32::from_le_bytes([sizes[3 * i], sizes[3 * i + 1], sizes[3 * i + 2], 0]) as usize } else { data.len() };
            if size > data.len() {
                return None;
            }
            parts.push(Rac::new(&data[..size]));
            data = &data[size..];
        }
        if w != self.w || h != self.h || self.mbs.is_empty() {
            self.w = w;
            self.h = h;
            self.mbw = w.div_ceil(16);
            self.mbh = h.div_ceil(16);
            self.refs = [None, None, None, None];
            if !self.keyframe {
                return None;
            }
            self.mbs = vec![Mb::default(); (self.mbw + 1) * (self.mbh + 1)];
            self.i4_top = vec![B_DC; self.mbw * 4];
            self.top_nnz = vec![[0; 9]; self.mbw];
            self.saved = [vec![0; self.mbw * 16], vec![0; self.mbw * 8], vec![0; self.mbw * 8]];
            self.strengths = vec![Strength::default(); self.mbw];
        }
        // Quantisers.
        let yac = c.uint(7) as i32;
        let d: [i32; 5] = core::array::from_fn(|_| sint(&mut c, 4));
        for i in 0..4 {
            let base = if self.seg_enabled {
                if self.seg_abs {
                    self.seg_quant[i]
                } else {
                    self.seg_quant[i] + yac
                }
            } else {
                yac
            };
            let q = |v: i32| v.clamp(0, 127) as usize;
            let m = &mut self.qmat[i];
            m[0] = DC_QLOOKUP[q(base + d[0])];
            m[1] = AC_QLOOKUP[q(base)];
            m[2] = DC_QLOOKUP[q(base + d[1])] * 2;
            m[3] = (AC_QLOOKUP[q(base + d[2])] * 101581 >> 16).max(8);
            m[4] = DC_QLOOKUP[q(base + d[3])].min(132);
            m[5] = AC_QLOOKUP[q(base + d[4])];
        }
        if !self.keyframe {
            let ug = c.bit();
            let ua = c.bit();
            let rtu = |c: &mut Rac, update: bool, r: i8| -> i8 {
                if update {
                    return 0;
                }
                match c.uint(2) {
                    1 => 1,
                    2 => {
                        if r == 2 {
                            3
                        } else {
                            2
                        }
                    }
                    _ => -1,
                }
            };
            self.update_golden = rtu(&mut c, ug, 2);
            self.update_altref = rtu(&mut c, ua, 3);
            self.sign_bias[2] = c.bit();
            self.sign_bias[3] = c.bit();
        }
        self.update_probabilities = c.bit();
        if !self.update_probabilities {
            self.prob[1] = self.prob[0].clone();
        }
        self.update_last = self.keyframe || c.bit();
        {
            let p = &mut self.prob[0];
            for i in 0..4 {
                for j in 0..8 {
                    for k in 0..3 {
                        for l in 0..11 {
                            if c.prob(TOKEN_UPDATE_PROBS[((i * 8 + j) * 3 + k) * 11 + l]) {
                                let v = c.uint(8) as u8;
                                for m in 0..16 {
                                    if COEFF_BAND[m] == j {
                                        p.token[((i * 16 + m) * 3 + k) * 11 + l] = v;
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }
        self.mbskip_enabled = c.bit();
        if self.mbskip_enabled {
            self.prob[0].mbskip = c.uint(8) as u8;
        }
        if !self.keyframe {
            let p = &mut self.prob[0];
            p.intra = c.uint(8) as u8;
            p.last = c.uint(8) as u8;
            p.golden = c.uint(8) as u8;
            if c.bit() {
                for i in 0..4 {
                    p.pred16[i] = c.uint(8) as u8;
                }
            }
            if c.bit() {
                for i in 0..3 {
                    p.pred8c[i] = c.uint(8) as u8;
                }
            }
            for i in 0..2 {
                for j in 0..19 {
                    if c.prob(MV_UPDATE_PROB[i * 19 + j]) {
                        let v = (c.uint(7) << 1) as u8;
                        p.mvc[i][j] = if v == 0 { 1 } else { v };
                    }
                }
            }
        }

        // Decode.
        let (mbw, mbh) = (self.mbw, self.mbh);
        let prev = self.refs[0].clone();
        let mut cur = Pic { y: vec![0; mbw * 16 * mbh * 16], u: vec![0; mbw * 8 * mbh * 8], v: vec![0; mbw * 8 * mbh * 8], seg: vec![0; mbw * mbh] };
        self.mbs.iter_mut().for_each(|m| *m = Mb::default());
        self.top_nnz.iter_mut().for_each(|v| *v = [0; 9]);
        if self.keyframe {
            self.i4_top.iter_mut().for_each(|v| *v = B_DC);
        }
        let deblock = self.filter_level != 0;
        const MARGIN: i32 = 64;
        for mb_y in 0..mbh {
            let pc = mb_y & (nparts - 1);
            self.i4_left = [B_DC; 4];
            self.left_nnz = [0; 9];
            let mv_min_y = -MARGIN - 64 * mb_y as i32;
            let mv_max_y = (((mbh - 1) as i32) << 6) + MARGIN - 64 * mb_y as i32;
            for mb_x in 0..mbw {
                if c.is_end() {
                    return None;
                }
                let bounds = (-MARGIN - 64 * mb_x as i32, (((mbw - 1) as i32) << 6) + MARGIN - 64 * mb_x as i32, mv_min_y, mv_max_y);
                let idx = (mb_y + 1) * (mbw + 1) + mb_x + 1;
                let segref = prev.as_ref().map(|p| p.seg[mb_y * mbw + mb_x]);
                self.decode_mb_mode(&mut c, idx, mb_x, &mut cur.seg[mb_y * mbw + mb_x], segref, bounds);
                let mut mb = self.mbs[idx];
                if !mb.skip {
                    self.decode_mb_coeffs(&mut parts[pc], &mut mb, mb_x);
                    self.mbs[idx].skip = mb.skip;
                } else {
                    self.nnz = [0; 25];
                }
                if mb.mode <= MODE_I4X4 {
                    self.intra_predict(&mut cur, &mb, mb_x, mb_y);
                } else {
                    self.inter_predict(&mut cur, &mb, mb_x, mb_y);
                }
                if !mb.skip {
                    self.idct_mb(&mut cur, &mb, mb_x, mb_y);
                } else {
                    self.left_nnz[..8].iter_mut().for_each(|v| *v = 0);
                    self.top_nnz[mb_x][..8].iter_mut().for_each(|v| *v = 0);
                    if mb.mode != MODE_I4X4 && mb.mode != MV_SPLIT {
                        self.left_nnz[8] = 0;
                        self.top_nnz[mb_x][8] = 0;
                    }
                }
                if deblock {
                    self.strengths[mb_x] = self.filter_level_for(&mb);
                }
            }
            // Unfiltered bottom rows for the next row's intra prediction.
            let ys = mbw * 16;
            let r = (mb_y * 16 + 15) * ys;
            self.saved[0].copy_from_slice(&cur.y[r..r + ys]);
            let cs = mbw * 8;
            let r = (mb_y * 8 + 7) * cs;
            self.saved[1].copy_from_slice(&cur.u[r..r + cs]);
            self.saved[2].copy_from_slice(&cur.v[r..r + cs]);
            if deblock {
                for mb_x in 0..mbw {
                    let f = self.strengths[mb_x];
                    self.filter_mb(&mut cur, f, mb_x, mb_y);
                }
            }
        }
        let cur = Rc::new(cur);
        // Reference updates.
        let pick = |s: &Self, u: i8, keep: usize| -> Option<Rc<Pic>> {
            match u {
                0 => Some(cur.clone()),
                -1 => s.refs[keep].clone(),
                r => s.refs[r as usize].clone(),
            }
        };
        let altref = pick(self, self.update_altref, 3);
        let golden = pick(self, self.update_golden, 2);
        let last = if self.update_last { Some(cur.clone()) } else { self.refs[1].clone() };
        self.refs = [Some(cur.clone()), last, golden, altref];
        if !self.update_probabilities {
            self.prob[0] = self.prob[1].clone();
        }
        if !self.invisible {
            self.emit(&cur);
        }
        Some(())
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_mb_mode(&mut self, c: &mut Rac, idx: usize, mb_x: usize, seg: &mut u8, segref: Option<u8>, bounds: (i32, i32, i32, i32)) {
        let mut mb = self.mbs[idx];
        if self.seg_update_map {
            let bit = c.prob(self.prob[0].segmentid[0]) as u8;
            *seg = c.prob(self.prob[0].segmentid[1 + bit as usize]) as u8 + 2 * bit;
        } else if self.seg_enabled {
            if let Some(r) = segref {
                *seg = r;
            }
        }
        mb.segment = *seg;
        mb.skip = if self.mbskip_enabled { c.prob(self.prob[0].mbskip) } else { false };
        if self.keyframe {
            mb.mode = c.tree(&TREE16_INTRA, &PRED16_PROB_INTRA) as u8;
            if mb.mode == MODE_I4X4 {
                for y in 0..4 {
                    for x in 0..4 {
                        let t = self.i4_top[4 * mb_x + x] as usize;
                        let l = self.i4_left[y] as usize;
                        let o = (t * 10 + l) * 9;
                        let m = c.tree(&TREE4, &PRED4X4_PROB_INTRA[o..o + 9]) as u8;
                        mb.i4[y * 4 + x] = m;
                        self.i4_top[4 * mb_x + x] = m;
                        self.i4_left[y] = m;
                    }
                }
            } else {
                let m = PRED4X4_MODE[mb.mode as usize];
                self.i4_top[4 * mb_x..4 * mb_x + 4].iter_mut().for_each(|v| *v = m);
                self.i4_left = [m; 4];
            }
            mb.chroma_mode = c.tree(&TREE8C, &PRED8C_PROB_INTRA) as u8;
            mb.ref_frame = 0;
        } else if c.prob(self.prob[0].intra) {
            mb.ref_frame = if c.prob(self.prob[0].last) {
                if c.prob(self.prob[0].golden) {
                    3
                } else {
                    2
                }
            } else {
                1
            };
            self.mbs[idx] = mb;
            self.decode_mvs(c, idx, bounds);
            return;
        } else {
            mb.mode = c.tree(&TREE16_INTER, &self.prob[0].pred16) as u8;
            if mb.mode == MODE_I4X4 {
                for i in 0..16 {
                    mb.i4[i] = c.tree(&TREE4, &PRED4_PROB_INTER) as u8;
                }
            }
            mb.chroma_mode = c.tree(&TREE8C, &self.prob[0].pred8c) as u8;
            mb.ref_frame = 0;
            mb.partitioning = SPLIT_NONE;
            mb.bmv[0] = Mv::default();
        }
        self.mbs[idx] = mb;
    }

    fn decode_mvs(&mut self, c: &mut Rac, idx: usize, bounds: (i32, i32, i32, i32)) {
        let mbw1 = self.mbw + 1;
        let edges = [idx - mbw1, idx - 1, idx - mbw1 - 1];
        let mut mb = self.mbs[idx];
        let cur_bias = self.sign_bias[mb.ref_frame as usize];
        let mut near = [Mv::default(); 4];
        let mut cnt = [0u8; 4];
        let mut i = 0usize;
        for (n, &e) in edges.iter().enumerate() {
            let em = &self.mbs[e];
            if em.ref_frame != 0 {
                let mut mv = em.mv;
                if mv != Mv::default() {
                    if cur_bias != self.sign_bias[em.ref_frame as usize] {
                        mv = Mv { x: mv.x.wrapping_neg(), y: mv.y.wrapping_neg() };
                    }
                    if n == 0 || mv != near[i] {
                        i += 1;
                        near[i] = mv;
                    }
                    cnt[i] += 1 + (n != 2) as u8;
                } else {
                    cnt[0] += 1 + (n != 2) as u8;
                }
            }
        }
        let clamp = |m: Mv| Mv {
            x: (m.x as i32).clamp(bounds.0.clamp(-32768, 32767), bounds.1.clamp(-32768, 32767)) as i16,
            y: (m.y as i32).clamp(bounds.2.clamp(-32768, 32767), bounds.3.clamp(-32768, 32767)) as i16,
        };
        mb.partitioning = SPLIT_NONE;
        if c.prob(MODE_CONTEXTS[cnt[0] as usize][0]) {
            mb.mode = MV_MV;
            if cnt[3] != 0 && near[1] == near[3] {
                cnt[1] += 1;
            }
            if cnt[2] > cnt[1] {
                cnt.swap(1, 2);
                near.swap(1, 2);
            }
            if c.prob(MODE_CONTEXTS[cnt[1] as usize][1]) {
                if c.prob(MODE_CONTEXTS[cnt[2] as usize][2]) {
                    mb.mv = clamp(near[(cnt[1] >= cnt[0]) as usize]);
                    let sp = |e: usize| (self.mbs[e].mode == MV_SPLIT) as u8;
                    cnt[3] = (sp(edges[1]) + sp(edges[0])) * 2 + sp(edges[2]);
                    if c.prob(MODE_CONTEXTS[cnt[3] as usize][3]) {
                        mb.mode = MV_SPLIT;
                        self.mbs[idx] = mb;
                        let n = self.decode_splitmvs(c, idx);
                        let m = &mut self.mbs[idx];
                        m.mv = m.bmv[n - 1];
                        return;
                    } else {
                        mb.mv.y = (mb.mv.y as i32 + read_mv_component(c, &self.prob[0].mvc[0])) as i16;
                        mb.mv.x = (mb.mv.x as i32 + read_mv_component(c, &self.prob[0].mvc[1])) as i16;
                        mb.bmv[0] = mb.mv;
                    }
                } else {
                    mb.mv = clamp(near[2]);
                    mb.bmv[0] = mb.mv;
                }
            } else {
                mb.mv = clamp(near[1]);
                mb.bmv[0] = mb.mv;
            }
        } else {
            mb.mode = MV_ZERO;
            mb.mv = Mv::default();
            mb.bmv[0] = mb.mv;
        }
        self.mbs[idx] = mb;
    }

    fn decode_splitmvs(&mut self, c: &mut Rac, idx: usize) -> usize {
        let mbw1 = self.mbw + 1;
        let left = self.mbs[idx - 1];
        let top = self.mbs[idx - mbw1];
        let mut mb = self.mbs[idx];
        let part = if c.prob(MBSPLIT_PROB[0]) {
            if c.prob(MBSPLIT_PROB[1]) {
                c.prob(MBSPLIT_PROB[2]) as usize
            } else {
                2
            }
        } else {
            3
        };
        let num = MBSPLIT_COUNT[part];
        let cur_s = &MBSPLITS[part];
        let first = &MBFIRSTIDX[part];
        let left_s = &MBSPLITS[left.partitioning as usize];
        let top_s = &MBSPLITS[top.partitioning as usize];
        mb.partitioning = part as u8;
        for n in 0..num {
            let k = first[n] as usize;
            let l = if k & 3 == 0 { left.bmv[left_s[k + 3] as usize] } else { mb.bmv[cur_s[k - 1] as usize] };
            let a = if k <= 3 { top.bmv[top_s[k + 12] as usize] } else { mb.bmv[cur_s[k - 4] as usize] };
            let z = Mv::default();
            let sp = if l == a {
                &SUBMV_PROB[4 - (l != z) as usize]
            } else if a == z {
                &SUBMV_PROB[2]
            } else {
                &SUBMV_PROB[1 - (l != z) as usize]
            };
            mb.bmv[n] = if c.prob(sp[0]) {
                if c.prob(sp[1]) {
                    if c.prob(sp[2]) {
                        let y = mb.mv.y as i32 + read_mv_component(c, &self.prob[0].mvc[0]);
                        let x = mb.mv.x as i32 + read_mv_component(c, &self.prob[0].mvc[1]);
                        Mv { x: x as i16, y: y as i16 }
                    } else {
                        z
                    }
                } else {
                    a
                }
            } else {
                l
            };
        }
        self.mbs[idx] = mb;
        num
    }

    fn decode_mb_coeffs(&mut self, c: &mut Rac, mb: &mut Mb, mb_x: usize) {
        let q = self.qmat[mb.segment as usize];
        let tok = &self.prob[0].token;
        let t = &mut self.top_nnz[mb_x];
        let l = &mut self.left_nnz;
        let mut luma_start = 0;
        let mut luma_ctx = 3;
        let mut total = 0usize;
        let mut block_dc = 0u8;
        self.nnz = [0; 25];
        let tk = |k: usize| &tok[k * 528..(k + 1) * 528];
        if mb.mode != MODE_I4X4 && mb.mode != MV_SPLIT {
            let pred = (t[8] + l[8]) as usize;
            let n = block_coeffs(c, &mut self.block[24], tk(1), 0, pred, (q[2], q[3]));
            t[8] = (n != 0) as u8;
            l[8] = t[8];
            if n != 0 {
                total += n;
                block_dc = 1;
                luma_dc_wht(&mut self.block, n == 1);
            }
            luma_start = 1;
            luma_ctx = 0;
        }
        for y in 0..4 {
            for x in 0..4 {
                let pred = (l[y] + t[x]) as usize;
                let n = block_coeffs(c, &mut self.block[y * 4 + x], tk(luma_ctx), luma_start, pred, (q[0], q[1]));
                self.nnz[y * 4 + x] = n as u8 + block_dc;
                t[x] = (n != 0) as u8;
                l[y] = t[x];
                total += n;
            }
        }
        for i in 4..6 {
            for y in 0..2 {
                for x in 0..2 {
                    let pred = (l[i + 2 * y] + t[i + 2 * x]) as usize;
                    let bi = 16 + (i - 4) * 4 + y * 2 + x;
                    let n = block_coeffs(c, &mut self.block[bi], tk(2), 0, pred, (q[4], q[5]));
                    self.nnz[bi] = n as u8;
                    t[i + 2 * x] = (n != 0) as u8;
                    l[i + 2 * y] = (n != 0) as u8;
                    total += n;
                }
            }
        }
        if total == 0 {
            mb.skip = true;
        }
    }

    fn intra_predict(&mut self, cur: &mut Pic, mb: &Mb, mb_x: usize, mb_y: usize) {
        let ys = self.mbw * 16;
        let cs = self.mbw * 8;
        let (x0, y0) = (mb_x * 16, mb_y * 16);
        // Above row (unfiltered) and corner, as libvpx: 127 above the picture, 129 left of it.
        let above_y = |i: isize| -> u8 {
            if mb_y == 0 {
                127
            } else {
                let x = x0 as isize + i;
                if x < 0 {
                    129
                } else {
                    self.saved[0][(x as usize).min(ys - 1)]
                }
            }
        };
        if mb.mode < MODE_I4X4 {
            let a: [u8; 16] = core::array::from_fn(|i| above_y(i as isize));
            let l: [u8; 16] = core::array::from_fn(|i| if mb_x == 0 { 129 } else { cur.y[(y0 + i) * ys + x0 - 1] });
            let p = if mb_y == 0 { 127 } else { above_y(-1) };
            pred_block(mb.mode, 16, &mut cur.y, y0 * ys + x0, ys, &a, &l, p, mb_y > 0, mb_x > 0);
        } else {
            if mb.skip {
                self.nnz = [0; 25];
            }
            for y in 0..4 {
                for x in 0..4 {
                    let (bx, by) = (x0 + 4 * x, y0 + 4 * y);
                    let mut a = [0i32; 9];
                    // Top-left.
                    a[0] = if by == 0 {
                        127
                    } else if bx == 0 {
                        129
                    } else if y == 0 {
                        above_y(4 * x as isize - 1) as i32
                    } else {
                        cur.y[(by - 1) * ys + bx - 1] as i32
                    };
                    for i in 0..4 {
                        a[1 + i] = if by == 0 {
                            127
                        } else if y == 0 {
                            above_y((4 * x + i) as isize) as i32
                        } else {
                            cur.y[(by - 1) * ys + bx + i] as i32
                        };
                    }
                    for i in 0..4 {
                        a[5 + i] = if mb_y == 0 && (y == 0 || x == 3) {
                            127
                        } else if x == 3 {
                            // Right column: the above macroblock row, past this macroblock.
                            if mb_x == self.mbw - 1 {
                                above_y(15) as i32
                            } else {
                                above_y((16 + i) as isize) as i32
                            }
                        } else if y == 0 {
                            above_y((4 * x + 4 + i) as isize) as i32
                        } else {
                            cur.y[(by - 1) * ys + bx + 4 + i] as i32
                        };
                    }
                    let l: [i32; 4] = core::array::from_fn(|i| if bx == 0 { 129 } else { cur.y[(by + i) * ys + bx - 1] as i32 });
                    pred4(mb.i4[y * 4 + x], &mut cur.y, by * ys + bx, ys, &a, &l);
                    let n = self.nnz[y * 4 + x];
                    add_block(&mut cur.y, by * ys + bx, ys, &mut self.block[y * 4 + x], n);
                }
            }
        }
        // Chroma.
        let (cx, cy) = (mb_x * 8, mb_y * 8);
        for pl in 0..2 {
            let a: [u8; 8] = core::array::from_fn(|i| if mb_y == 0 { 127 } else { self.saved[1 + pl][cx + i] });
            let plane = if pl == 0 { &mut cur.u } else { &mut cur.v };
            let l: [u8; 8] = core::array::from_fn(|i| if mb_x == 0 { 129 } else { plane[(cy + i) * cs + cx - 1] });
            let p = if mb_y == 0 {
                127
            } else if mb_x == 0 {
                129
            } else {
                self.saved[1 + pl][cx - 1]
            };
            pred_block(mb.chroma_mode, 8, plane, cy * cs + cx, cs, &a, &l, p, mb_y > 0, mb_x > 0);
        }
    }

    fn inter_predict(&mut self, cur: &mut Pic, mb: &Mb, mb_x: usize, mb_y: usize) {
        let Some(r) = self.refs[mb.ref_frame as usize].clone() else { return };
        let ys = self.mbw * 16;
        let cs = self.mbw * 8;
        let (w, h) = (ys as i32, (self.mbh * 16) as i32);
        let bil = self.profile != 0;
        let full_uv = self.profile == 3;
        let (x0, y0) = ((mb_x * 16) as i32, (mb_y * 16) as i32);
        let luma = |cur: &mut Pic, mv: Mv, bx: i32, by: i32, bw: usize, bh: usize| {
            let mx = ((mv.x as i32 * 2) & 7) as usize;
            let my = ((mv.y as i32 * 2) & 7) as usize;
            let x = x0 + bx + (mv.x as i32 >> 2);
            let y = y0 + by + (mv.y as i32 >> 2);
            mc(&mut cur.y, (y0 + by) as usize * ys + (x0 + bx) as usize, ys, &r.y, w, h, x, y, mx, my, bw, bh, bil);
        };
        let chroma = |cur: &mut Pic, mut mv: Mv, bx: i32, by: i32, bw: usize, bh: usize| {
            if full_uv {
                mv.x &= !7;
                mv.y &= !7;
            }
            let mx = (mv.x as i32 & 7) as usize;
            let my = (mv.y as i32 & 7) as usize;
            let (cx0, cy0) = (x0 / 2 + bx, y0 / 2 + by);
            let x = cx0 + (mv.x as i32 >> 3);
            let y = cy0 + (mv.y as i32 >> 3);
            let o = cy0 as usize * cs + cx0 as usize;
            mc(&mut cur.u, o, cs, &r.u, w / 2, h / 2, x, y, mx, my, bw, bh, bil);
            mc(&mut cur.v, o, cs, &r.v, w / 2, h / 2, x, y, mx, my, bw, bh, bil);
        };
        let part = |cur: &mut Pic, mv: Mv, bx: i32, by: i32, bw: usize, bh: usize| {
            luma(cur, mv, bx, by, bw, bh);
            chroma(cur, mv, bx / 2, by / 2, bw / 2, bh / 2);
        };
        let b = &mb.bmv;
        match mb.partitioning {
            0 => {
                part(cur, b[0], 0, 0, 16, 8);
                part(cur, b[1], 0, 8, 16, 8);
            }
            1 => {
                part(cur, b[0], 0, 0, 8, 16);
                part(cur, b[1], 8, 0, 8, 16);
            }
            2 => {
                part(cur, b[0], 0, 0, 8, 8);
                part(cur, b[1], 8, 0, 8, 8);
                part(cur, b[2], 0, 8, 8, 8);
                part(cur, b[3], 8, 8, 8, 8);
            }
            3 => {
                for y in 0..4 {
                    for x in 0..4 {
                        luma(cur, b[4 * y + x], 4 * x as i32, 4 * y as i32, 4, 4);
                    }
                }
                for y in 0..2 {
                    for x in 0..2 {
                        let k = [2 * y * 4 + 2 * x, 2 * y * 4 + 2 * x + 1, (2 * y + 1) * 4 + 2 * x, (2 * y + 1) * 4 + 2 * x + 1];
                        let sx: i32 = k.iter().map(|&i| b[i].x as i32).sum();
                        let sy: i32 = k.iter().map(|&i| b[i].y as i32).sum();
                        let uv = Mv { x: ((sx + 2 + (sx >> 31)) >> 2) as i16, y: ((sy + 2 + (sy >> 31)) >> 2) as i16 };
                        chroma(cur, uv, 4 * x as i32, 4 * y as i32, 4, 4);
                    }
                }
            }
            _ => part(cur, mb.mv, 0, 0, 16, 16),
        }
    }

    fn idct_mb(&mut self, cur: &mut Pic, mb: &Mb, mb_x: usize, mb_y: usize) {
        let ys = self.mbw * 16;
        let cs = self.mbw * 8;
        if mb.mode != MODE_I4X4 {
            for y in 0..4 {
                for x in 0..4 {
                    let o = (mb_y * 16 + 4 * y) * ys + mb_x * 16 + 4 * x;
                    let n = self.nnz[y * 4 + x];
                    add_block(&mut cur.y, o, ys, &mut self.block[y * 4 + x], n);
                }
            }
        }
        for pl in 0..2 {
            for y in 0..2 {
                for x in 0..2 {
                    let bi = 16 + pl * 4 + y * 2 + x;
                    let o = (mb_y * 8 + 4 * y) * cs + mb_x * 8 + 4 * x;
                    let n = self.nnz[bi];
                    let d = if pl == 0 { &mut cur.u } else { &mut cur.v };
                    add_block(d, o, cs, &mut self.block[bi], n);
                }
            }
        }
    }

    fn filter_level_for(&self, mb: &Mb) -> Strength {
        let mut level = if self.seg_enabled {
            let l = self.seg_lf[mb.segment as usize];
            if self.seg_abs {
                l
            } else {
                l + self.filter_level
            }
        } else {
            self.filter_level
        };
        if self.lf_delta_enabled {
            level += self.lf_ref[mb.ref_frame as usize];
            level += self.lf_mode[mb.mode as usize];
        }
        let level = level.clamp(0, 63);
        let mut il = level;
        if self.sharpness != 0 {
            il >>= (self.sharpness + 3) >> 2;
            il = il.min(9 - self.sharpness);
        }
        Strength { level, inner_limit: il.max(1), inner: !mb.skip || mb.mode == MODE_I4X4 || mb.mode == MV_SPLIT }
    }

    fn filter_mb(&self, cur: &mut Pic, f: Strength, mb_x: usize, mb_y: usize) {
        if f.level == 0 {
            return;
        }
        let ys = self.mbw * 16;
        let cs = self.mbw * 8;
        let yo = mb_y * 16 * ys + mb_x * 16;
        let co = mb_y * 8 * cs + mb_x * 8;
        let (ysa, csa) = (ys as isize, cs as isize);
        let (il, inner) = (f.inner_limit, f.inner);
        if self.filter_simple {
            let b = 2 * f.level + il;
            let mbl = b + 4;
            let mut lf = Lf { d: &mut cur.y };
            if mb_x > 0 {
                lf.simple(yo, ysa, 1, mbl);
            }
            if inner {
                for k in [4, 8, 12] {
                    lf.simple(yo + k, ysa, 1, b);
                }
            }
            if mb_y > 0 {
                lf.simple(yo, 1, ysa, mbl);
            }
            if inner {
                for k in [4, 8, 12] {
                    lf.simple(yo + k * ys, 1, ysa, b);
                }
            }
            return;
        }
        let bl = f.level * 2 + il;
        let mbl = bl + 4;
        let hev = HEV_LUT[self.keyframe as usize][f.level as usize] as i32;
        if mb_x > 0 {
            Lf { d: &mut cur.y }.edge(yo, ysa, 1, 16, mbl, il, hev, false);
            Lf { d: &mut cur.u }.edge(co, csa, 1, 8, mbl, il, hev, false);
            Lf { d: &mut cur.v }.edge(co, csa, 1, 8, mbl, il, hev, false);
        }
        if inner {
            for k in [4, 8, 12] {
                Lf { d: &mut cur.y }.edge(yo + k, ysa, 1, 16, bl, il, hev, true);
            }
            Lf { d: &mut cur.u }.edge(co + 4, csa, 1, 8, bl, il, hev, true);
            Lf { d: &mut cur.v }.edge(co + 4, csa, 1, 8, bl, il, hev, true);
        }
        if mb_y > 0 {
            Lf { d: &mut cur.y }.edge(yo, 1, ysa, 16, mbl, il, hev, false);
            Lf { d: &mut cur.u }.edge(co, 1, csa, 8, mbl, il, hev, false);
            Lf { d: &mut cur.v }.edge(co, 1, csa, 8, mbl, il, hev, false);
        }
        if inner {
            for k in [4, 8, 12] {
                Lf { d: &mut cur.y }.edge(yo + k * ys, 1, ysa, 16, bl, il, hev, true);
            }
            Lf { d: &mut cur.u }.edge(co + 4 * cs, 1, csa, 8, bl, il, hev, true);
            Lf { d: &mut cur.v }.edge(co + 4 * cs, 1, csa, 8, bl, il, hev, true);
        }
    }

    fn emit(&mut self, p: &Pic) {
        let (w, h) = (self.w, self.h);
        let ow = w.next_multiple_of(2);
        let oh = h.next_multiple_of(2);
        let (ys, cs) = (self.mbw * 16, self.mbw * 8);
        let mut y = vec![0u8; ow * oh];
        let mut u = vec![128u8; ow * oh / 4];
        let mut v = vec![128u8; ow * oh / 4];
        for r in 0..h {
            y[r * ow..r * ow + w].copy_from_slice(&p.y[r * ys..r * ys + w]);
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        for r in 0..ch {
            u[r * (ow / 2)..r * (ow / 2) + cw].copy_from_slice(&p.u[r * cs..r * cs + cw]);
            v[r * (ow / 2)..r * (ow / 2) + cw].copy_from_slice(&p.v[r * cs..r * cs + cw]);
        }
        self.out.push_back(Frame { width: w, height: h, y, u, v, stride: ow, crop_x: 0, crop_y: 0, bt709: false, full_range: self.full_range, poc: 0 });
    }
}

impl VideoDec for Vp8Dec {
    fn decode(&mut self, data: &[u8]) {
        let _ = self.decode_frame(data);
    }
    fn pop(&mut self) -> Option<OutPic> {
        self.out.pop_front().map(OutPic::Yuv)
    }
    fn flush(&mut self) {}
    fn reset(&mut self) {
        self.out.clear();
    }
}

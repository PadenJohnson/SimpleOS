//! An H.264 encoder (Constrained Baseline profile, CAVLC), small enough to live in Solon.
//!
//! * Key frames (IDR) code every 16x16 macroblock with Intra 16x16 prediction
//!   (vertical, horizontal or DC; chroma DC).
//! * Other frames are P frames that refer to the previous frame with no motion: each
//!   macroblock is either skipped (unchanged) or coded as the difference from the same
//!   place in the previous frame. Slideshows, titles and still shots compress very well.
//! * One slice per frame, fixed quantizer, no deblocking filter (so the encoder's own
//!   reconstruction matches the decoder's exactly).
//!
//! Output is a list of NAL units (without start codes) per frame, ready for MP4.

use alloc::vec;
use alloc::vec::Vec;

// ---- bit writer ------------------------------------------------------------------------

pub struct Bits {
    out: Vec<u8>,
    cur: u64,
    n: u32,
}

impl Bits {
    pub fn new() -> Bits {
        Bits { out: Vec::new(), cur: 0, n: 0 }
    }
    pub fn put(&mut self, v: u32, len: u32) {
        if len == 0 {
            return;
        }
        self.cur = (self.cur << len) | (v as u64 & ((1u64 << len) - 1));
        self.n += len;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.cur >> self.n) as u8);
        }
    }
    pub fn flag(&mut self, b: bool) {
        self.put(b as u32, 1);
    }
    /// Unsigned Exp-Golomb.
    pub fn ue(&mut self, v: u32) {
        let x = v as u64 + 1;
        let len = 64 - x.leading_zeros();
        self.put(0, len - 1);
        if len > 32 {
            self.put((x >> 32) as u32, len - 32);
            self.put(x as u32, 32);
        } else {
            self.put(x as u32, len);
        }
    }
    /// Signed Exp-Golomb.
    pub fn se(&mut self, v: i32) {
        self.ue(if v > 0 { (2 * v - 1) as u32 } else { (-2 * v) as u32 });
    }
    /// RBSP trailing bits, then the bytes with emulation prevention.
    pub fn finish(mut self) -> Vec<u8> {
        self.put(1, 1);
        if self.n > 0 {
            let pad = 8 - self.n;
            self.put(0, pad);
        }
        let mut nal = Vec::with_capacity(self.out.len() + 16);
        let mut zeros = 0;
        for &b in &self.out {
            if zeros >= 2 && b <= 3 {
                nal.push(3);
                zeros = 0;
            }
            nal.push(b);
            zeros = if b == 0 { zeros + 1 } else { 0 };
        }
        nal
    }
}

// ---- tables ------------------------------------------------------------------------------

/// coeff_token lengths/codes for nC 0-1, 2-3, 4-7, 8+; index total_coeff*4 + trailing_ones.
const CT_LEN: [[u8; 68]; 4] = [
    [
        1, 0, 0, 0, 6, 2, 0, 0, 8, 6, 3, 0, 9, 8, 7, 5, 10, 9, 8, 6, 11, 10, 9, 7, 13, 11, 10, 8, 13, 13, 11, 9, 13, 13, 13, 10, 14, 14, 13, 11, 14, 14, 14, 13, 15, 15, 14, 14, 15, 15,
        15, 14, 16, 15, 15, 15, 16, 16, 16, 15, 16, 16, 16, 16, 16, 16, 16, 16,
    ],
    [
        2, 0, 0, 0, 6, 2, 0, 0, 6, 5, 3, 0, 7, 6, 6, 4, 8, 6, 6, 4, 8, 7, 7, 5, 9, 8, 8, 6, 11, 9, 9, 6, 11, 11, 11, 7, 12, 11, 11, 9, 12, 12, 12, 11, 12, 12, 12, 11, 13, 13, 13,
        12, 13, 13, 13, 13, 13, 14, 13, 13, 14, 14, 14, 13, 14, 14, 14, 14,
    ],
    [
        4, 0, 0, 0, 6, 4, 0, 0, 6, 5, 4, 0, 6, 5, 5, 4, 7, 5, 5, 4, 7, 5, 5, 4, 7, 6, 6, 4, 7, 6, 6, 4, 8, 7, 7, 5, 8, 8, 7, 6, 9, 8, 8, 7, 9, 9, 8, 8, 9, 9, 9, 8, 10, 9, 9, 9, 10,
        10, 10, 10, 10, 10, 10, 10, 10, 10, 10, 10,
    ],
    [6; 68],
];
const CT_BITS: [[u8; 68]; 4] = [
    [
        1, 0, 0, 0, 5, 1, 0, 0, 7, 4, 1, 0, 7, 6, 5, 3, 7, 6, 5, 3, 7, 6, 5, 4, 15, 6, 5, 4, 11, 14, 5, 4, 8, 10, 13, 4, 15, 14, 9, 4, 11, 10, 13, 12, 15, 14, 9, 12, 11, 10, 13, 8,
        15, 1, 9, 12, 11, 14, 13, 8, 7, 10, 9, 12, 4, 6, 5, 8,
    ],
    [
        3, 0, 0, 0, 11, 2, 0, 0, 7, 7, 3, 0, 7, 10, 9, 5, 7, 6, 5, 4, 4, 6, 5, 6, 7, 6, 5, 8, 15, 6, 5, 4, 11, 14, 13, 4, 15, 10, 9, 4, 11, 14, 13, 12, 8, 10, 9, 8, 15, 14, 13, 12,
        11, 10, 9, 12, 7, 11, 6, 8, 9, 8, 10, 1, 7, 6, 5, 4,
    ],
    [
        15, 0, 0, 0, 15, 14, 0, 0, 11, 15, 13, 0, 8, 12, 14, 12, 15, 10, 11, 11, 11, 8, 9, 10, 9, 14, 13, 9, 8, 10, 9, 8, 15, 14, 13, 13, 11, 14, 10, 12, 15, 10, 13, 12, 11, 14, 9,
        12, 8, 10, 13, 8, 13, 7, 9, 12, 9, 12, 11, 10, 5, 8, 7, 6, 1, 4, 3, 2,
    ],
    [
        3, 0, 0, 0, 0, 1, 0, 0, 4, 5, 6, 0, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 30, 31, 32, 33, 34, 35, 36, 37, 38, 39, 40, 41, 42,
        43, 44, 45, 46, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 59, 60, 61, 62, 63,
    ],
];
/// Chroma DC (nC = -1).
const CDC_LEN: [u8; 20] = [2, 0, 0, 0, 6, 1, 0, 0, 6, 6, 3, 0, 6, 7, 7, 6, 6, 8, 8, 7];
const CDC_BITS: [u8; 20] = [1, 0, 0, 0, 7, 1, 0, 0, 4, 6, 1, 0, 3, 3, 2, 5, 2, 3, 2, 0];

/// total_zeros, indexed [total_coeff - 1][total_zeros].
const TZ_LEN: [&[u8]; 15] = [
    &[1, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 9],
    &[3, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 6, 6, 6, 6],
    &[4, 3, 3, 3, 4, 4, 3, 3, 4, 5, 5, 6, 5, 6],
    &[5, 3, 4, 4, 3, 3, 3, 4, 3, 4, 5, 5, 5],
    &[4, 4, 4, 3, 3, 3, 3, 3, 4, 5, 4, 5],
    &[6, 5, 3, 3, 3, 3, 3, 3, 4, 3, 6],
    &[6, 5, 3, 3, 3, 2, 3, 4, 3, 6],
    &[6, 4, 5, 3, 2, 2, 3, 3, 6],
    &[6, 6, 4, 2, 2, 3, 2, 5],
    &[5, 5, 3, 2, 2, 2, 4],
    &[4, 4, 3, 3, 1, 3],
    &[4, 4, 2, 1, 3],
    &[3, 3, 1, 2],
    &[2, 2, 1],
    &[1, 1],
];
const TZ_BITS: [&[u8]; 15] = [
    &[1, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 3, 2, 1],
    &[7, 6, 5, 4, 3, 5, 4, 3, 2, 3, 2, 3, 2, 1, 0],
    &[5, 7, 6, 5, 4, 3, 4, 3, 2, 3, 2, 1, 1, 0],
    &[3, 7, 5, 4, 6, 5, 4, 3, 3, 2, 2, 1, 0],
    &[5, 4, 3, 7, 6, 5, 4, 3, 2, 1, 1, 0],
    &[1, 1, 7, 6, 5, 4, 3, 2, 1, 1, 0],
    &[1, 1, 5, 4, 3, 3, 2, 1, 1, 0],
    &[1, 1, 1, 3, 3, 2, 2, 1, 0],
    &[1, 0, 1, 3, 2, 1, 1, 1],
    &[1, 0, 1, 3, 2, 1, 1],
    &[0, 1, 1, 2, 1, 3],
    &[0, 1, 1, 1, 1],
    &[0, 1, 1, 1],
    &[0, 1, 1],
    &[0, 1],
];
/// Chroma DC total_zeros, [total_coeff - 1][total_zeros].
const CTZ_LEN: [&[u8]; 3] = [&[1, 2, 3, 3], &[1, 2, 2], &[1, 1]];
const CTZ_BITS: [&[u8]; 3] = [&[1, 1, 1, 0], &[1, 1, 0], &[1, 0]];
/// run_before, [min(zeros_left, 7) - 1][run_before].
const RUN_LEN: [&[u8]; 7] = [&[1, 1], &[1, 2, 2], &[2, 2, 2, 2], &[2, 2, 2, 3, 3], &[2, 2, 3, 3, 3, 3], &[2, 3, 3, 3, 3, 3, 3], &[3, 3, 3, 3, 3, 3, 3, 4, 5, 6, 7, 8, 9, 10, 11]];
const RUN_BITS: [&[u8]; 7] = [&[1, 0], &[1, 1, 0], &[3, 2, 1, 0], &[3, 2, 1, 1, 0], &[3, 2, 3, 2, 1, 0], &[3, 0, 1, 3, 2, 5, 4], &[7, 6, 5, 4, 3, 2, 1, 1, 1, 1, 1, 1, 1, 1, 1]];

/// Exp-Golomb code number -> coded_block_pattern, for inter macroblocks.
const GOLOMB_TO_INTER_CBP: [u8; 48] = [
    0, 16, 1, 2, 4, 8, 32, 3, 5, 10, 12, 15, 47, 7, 11, 13, 14, 6, 9, 31, 35, 37, 42, 44, 33, 34, 36, 40, 39, 43, 45, 46, 17, 18, 20, 24, 19, 21, 26, 28, 23, 27, 29, 30, 22, 25, 38, 41,
];

/// 4x4 zigzag: scan position -> raster index.
const ZIGZAG: [usize; 16] = [0, 1, 4, 8, 5, 2, 3, 6, 9, 12, 13, 10, 7, 11, 14, 15];

/// Forward quantization multipliers and inverse scales by (qp % 6, position class).
const MF: [[i32; 3]; 6] = [[13107, 5243, 8066], [11916, 4660, 7490], [10082, 4194, 6554], [9362, 3647, 5825], [8192, 3355, 5243], [7282, 2893, 4559]];
const V: [[i32; 3]; 6] = [[10, 16, 13], [11, 18, 14], [13, 20, 16], [14, 23, 18], [16, 25, 20], [18, 29, 23]];
/// Chroma quantizer for luma quantizer >= 30.
const QPC: [i32; 22] = [29, 30, 31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38, 39, 39, 39, 39];

fn chroma_qp(qp: i32) -> i32 {
    if qp < 30 {
        qp
    } else {
        QPC[(qp - 30).min(21) as usize]
    }
}

#[inline]
fn class(i: usize) -> usize {
    let (r, c) = (i / 4, i % 4);
    if r % 2 == 0 && c % 2 == 0 {
        0
    } else if r % 2 == 1 && c % 2 == 1 {
        1
    } else {
        2
    }
}

// ---- transforms ----------------------------------------------------------------------

fn fdct4(x: &[i32; 16]) -> [i32; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let (a, b, c, d) = (x[i * 4], x[i * 4 + 1], x[i * 4 + 2], x[i * 4 + 3]);
        let (s0, s1, d0, d1) = (a + d, b + c, a - d, b - c);
        t[i * 4] = s0 + s1;
        t[i * 4 + 2] = s0 - s1;
        t[i * 4 + 1] = 2 * d0 + d1;
        t[i * 4 + 3] = d0 - 2 * d1;
    }
    let mut y = [0i32; 16];
    for j in 0..4 {
        let (a, b, c, d) = (t[j], t[4 + j], t[8 + j], t[12 + j]);
        let (s0, s1, d0, d1) = (a + d, b + c, a - d, b - c);
        y[j] = s0 + s1;
        y[8 + j] = s0 - s1;
        y[4 + j] = 2 * d0 + d1;
        y[12 + j] = d0 - 2 * d1;
    }
    y
}

/// Inverse 4x4 transform with the standard's rounding, giving the residual.
fn idct4(d: &[i32; 16]) -> [i32; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let (a, b, c, e) = (d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]);
        let (p, q, r, s) = (a + c, a - c, (b >> 1) - e, b + (e >> 1));
        t[i * 4] = p + s;
        t[i * 4 + 1] = q + r;
        t[i * 4 + 2] = q - r;
        t[i * 4 + 3] = p - s;
    }
    let mut out = [0i32; 16];
    for j in 0..4 {
        let (a, b, c, e) = (t[j], t[4 + j], t[8 + j], t[12 + j]);
        let (p, q, r, s) = (a + c, a - c, (b >> 1) - e, b + (e >> 1));
        out[j] = (p + s + 32) >> 6;
        out[4 + j] = (q + r + 32) >> 6;
        out[8 + j] = (q - r + 32) >> 6;
        out[12 + j] = (p - s + 32) >> 6;
    }
    out
}

fn hadamard4(x: &[i32; 16]) -> [i32; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let (a, b, c, d) = (x[i * 4], x[i * 4 + 1], x[i * 4 + 2], x[i * 4 + 3]);
        t[i * 4] = a + b + c + d;
        t[i * 4 + 1] = a + b - c - d;
        t[i * 4 + 2] = a - b - c + d;
        t[i * 4 + 3] = a - b + c - d;
    }
    let mut y = [0i32; 16];
    for j in 0..4 {
        let (a, b, c, d) = (t[j], t[4 + j], t[8 + j], t[12 + j]);
        y[j] = a + b + c + d;
        y[4 + j] = a + b - c - d;
        y[8 + j] = a - b - c + d;
        y[12 + j] = a - b + c - d;
    }
    y
}

fn quant(v: i32, mf: i32, qbits: i32, f: i32) -> i32 {
    let q = (v.abs() * mf + f) >> qbits;
    let q = q.min(2000); // keep within what Baseline's level escape can carry
    if v < 0 {
        -q
    } else {
        q
    }
}

// ---- CAVLC ---------------------------------------------------------------------------------

/// Write one residual block (coefficients in scan order). Returns its total_coeff.
/// `nc` = -1 for chroma DC.
fn residual_block(bw: &mut Bits, coef: &[i32], nc: i32) -> u8 {
    let max = coef.len();
    let total = coef.iter().filter(|&&c| c != 0).count();
    // Trailing ones: up to three +-1 at the high-frequency end.
    let mut t1 = 0;
    let mut nz: Vec<i32> = Vec::with_capacity(16); // non-zeros from high to low frequency
    for &c in coef.iter().rev() {
        if c != 0 {
            nz.push(c);
        }
    }
    for &c in &nz {
        if c.abs() == 1 && t1 < 3 {
            t1 += 1;
        } else {
            break;
        }
    }
    let idx = total * 4 + t1;
    if nc == -1 {
        bw.put(CDC_BITS[idx] as u32, CDC_LEN[idx] as u32);
    } else {
        let tab = match nc {
            0..=1 => 0,
            2..=3 => 1,
            4..=7 => 2,
            _ => 3,
        };
        if tab == 3 {
            // Fixed 6 bits: total_coeff - 1 (or 3 for none) and trailing ones.
            let code = if total == 0 { 3 } else { ((total - 1) << 2) | t1 };
            bw.put(code as u32, 6);
        } else {
            bw.put(CT_BITS[tab][idx] as u32, CT_LEN[tab][idx] as u32);
        }
    }
    if total == 0 {
        return 0;
    }
    for &c in nz.iter().take(t1) {
        bw.flag(c < 0);
    }
    let mut suffix_len: u32 = if total > 10 && t1 < 3 { 1 } else { 0 };
    for (k, &level) in nz.iter().enumerate().skip(t1) {
        let mut code: i32 = if level > 0 { 2 * level - 2 } else { -2 * level - 1 };
        if k == t1 && t1 < 3 {
            code -= 2;
        }
        let (prefix, suffix, slen): (u32, u32, u32) = if suffix_len == 0 {
            if code < 14 {
                (code as u32, 0, 0)
            } else if code < 30 {
                (14, (code - 14) as u32, 4)
            } else {
                (15, (code - 30) as u32, 12)
            }
        } else if code < (15 << suffix_len) {
            ((code >> suffix_len) as u32, (code & ((1 << suffix_len) - 1)) as u32, suffix_len)
        } else {
            (15, (code - (15 << suffix_len)) as u32, 12)
        };
        bw.put(0, prefix);
        bw.put(1, 1);
        bw.put(suffix, slen);
        if suffix_len == 0 {
            suffix_len = 1;
        }
        if level.abs() > (3 << (suffix_len - 1)) && suffix_len < 6 {
            suffix_len += 1;
        }
    }
    // Zeros below the highest non-zero coefficient.
    let last = coef.iter().rposition(|&c| c != 0).unwrap();
    let total_zeros = last + 1 - total;
    if total < max {
        if nc == -1 {
            bw.put(CTZ_BITS[total - 1][total_zeros] as u32, CTZ_LEN[total - 1][total_zeros] as u32);
        } else {
            bw.put(TZ_BITS[total - 1][total_zeros] as u32, TZ_LEN[total - 1][total_zeros] as u32);
        }
    }
    // Runs of zeros before each non-zero, from the top down.
    let mut zeros_left = total_zeros;
    let mut pos = last as isize;
    let mut done = 0;
    while done < total - 1 && zeros_left > 0 {
        // Count zeros directly below `pos`.
        let mut run = 0;
        let mut p = pos - 1;
        while p >= 0 && coef[p as usize] == 0 {
            run += 1;
            p -= 1;
        }
        let t = zeros_left.min(7) - 1;
        bw.put(RUN_BITS[t][run] as u32, RUN_LEN[t][run] as u32);
        zeros_left -= run;
        pos = p;
        done += 1;
    }
    total as u8
}

// ---- frames ------------------------------------------------------------------------------

/// A picture in YUV 4:2:0 (sizes padded to whole macroblocks).
#[derive(Clone)]
pub struct Yuv {
    pub w: usize,
    pub h: usize,
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
}

impl Yuv {
    /// From 0xAARRGGBB pixels (BT.601, video range), padded with edge copies.
    pub fn from_rgb(px: &[u32], w: usize, h: usize) -> Yuv {
        let pw = w.div_ceil(16) * 16;
        let ph = h.div_ceil(16) * 16;
        let mut y = vec![0u8; pw * ph];
        let mut u = vec![0u8; pw * ph / 4];
        let mut v = vec![0u8; pw * ph / 4];
        let get = |x: usize, yy: usize| px[yy.min(h - 1) * w + x.min(w - 1)];
        for yy in 0..ph {
            for x in 0..pw {
                let p = get(x, yy);
                let (r, g, b) = (((p >> 16) & 0xFF) as i32, ((p >> 8) & 0xFF) as i32, (p & 0xFF) as i32);
                y[yy * pw + x] = (((66 * r + 129 * g + 25 * b + 128) >> 8) + 16) as u8;
            }
        }
        for cy in 0..ph / 2 {
            for cx in 0..pw / 2 {
                let (mut r, mut g, mut b) = (0, 0, 0);
                for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                    let p = get(cx * 2 + dx, cy * 2 + dy);
                    r += ((p >> 16) & 0xFF) as i32;
                    g += ((p >> 8) & 0xFF) as i32;
                    b += (p & 0xFF) as i32;
                }
                let (r, g, b) = ((r + 2) / 4, (g + 2) / 4, (b + 2) / 4);
                u[cy * (pw / 2) + cx] = (((-38 * r - 74 * g + 112 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
                v[cy * (pw / 2) + cx] = (((112 * r - 94 * g - 18 * b + 128) >> 8) + 128).clamp(0, 255) as u8;
            }
        }
        Yuv { w: pw, h: ph, y, u, v }
    }
}

pub struct Encoder {
    /// Shown size, and the size in macroblocks.
    pub width: usize,
    pub height: usize,
    mbw: usize,
    mbh: usize,
    qp: i32,
    frame_num: u32,
    idr_id: u32,
    /// Reconstruction of the last frame (what the decoder has).
    reference: Option<Yuv>,
    /// Frames since the last key frame.
    since_key: u32,
    pub key_interval: u32,
}

/// Total-coefficient counts of every 4x4 block in the picture (for CAVLC's nC).
struct Counts {
    luma: Vec<u8>, // (mbw*4) x (mbh*4)
    cb: Vec<u8>,   // (mbw*2) x (mbh*2)
    cr: Vec<u8>,
    lw: usize,
    cw: usize,
}

impl Counts {
    fn nc(arr: &[u8], stride: usize, x: usize, y: usize) -> i32 {
        let a = if x > 0 { Some(arr[y * stride + x - 1] as i32) } else { None };
        let b = if y > 0 { Some(arr[(y - 1) * stride + x] as i32) } else { None };
        match (a, b) {
            (Some(a), Some(b)) => (a + b + 1) >> 1,
            (Some(a), None) => a,
            (None, Some(b)) => b,
            _ => 0,
        }
    }
}

/// Luma 4x4 block index -> (x, y) inside the macroblock.
fn blk_xy(b: usize) -> (usize, usize) {
    ((b & 1) * 4 + ((b >> 2) & 1) * 8, ((b >> 1) & 1) * 4 + (b >> 3) * 8)
}

impl Encoder {
    pub fn new(width: usize, height: usize, qp: i32) -> Encoder {
        Encoder {
            width,
            height,
            mbw: width.div_ceil(16),
            mbh: height.div_ceil(16),
            qp: qp.clamp(10, 45),
            frame_num: 0,
            idr_id: 0,
            reference: None,
            since_key: 0,
            key_interval: 60,
        }
    }

    pub fn sps(&self) -> Vec<u8> {
        let mut b = Bits::new();
        b.put(0x67, 8); // NAL header: ref_idc 3, type 7
        b.put(66, 8); // Baseline
        b.put(0b1100_0000, 8); // constraint_set0 and 1 (Constrained Baseline)
        b.put(40, 8); // level 4.0
        b.ue(0); // sps id
        b.ue(4); // log2_max_frame_num - 4 -> 8 bits
        b.ue(2); // pic_order_cnt_type 2
        b.ue(1); // max_num_ref_frames
        b.flag(false); // gaps
        b.ue(self.mbw as u32 - 1);
        b.ue(self.mbh as u32 - 1);
        b.flag(true); // frame_mbs_only
        b.flag(true); // direct_8x8_inference
        let (cr, cb) = (self.mbw * 16 - self.width, self.mbh * 16 - self.height);
        b.flag(cr > 0 || cb > 0);
        if cr > 0 || cb > 0 {
            b.ue(0);
            b.ue(cr as u32 / 2);
            b.ue(0);
            b.ue(cb as u32 / 2);
        }
        b.flag(false); // no VUI
        b.finish()
    }

    pub fn pps(&self) -> Vec<u8> {
        let mut b = Bits::new();
        b.put(0x68, 8); // ref_idc 3, type 8
        b.ue(0); // pps id
        b.ue(0); // sps id
        b.flag(false); // CAVLC
        b.flag(false); // bottom_field_pic_order_in_frame_present
        b.ue(0); // one slice group
        b.ue(0); // num_ref_idx_l0_default - 1
        b.ue(0); // l1
        b.flag(false); // weighted_pred
        b.put(0, 2); // weighted_bipred_idc
        b.se(self.qp - 26); // pic_init_qp
        b.se(0); // pic_init_qs
        b.se(0); // chroma_qp_index_offset
        b.flag(true); // deblocking_filter_control_present
        b.flag(false); // constrained_intra_pred
        b.flag(false); // redundant_pic_cnt_present
        b.finish()
    }

    /// Encode a frame (0xAARRGGBB, width x height). Returns (slice NAL, is key frame).
    /// `force_key` starts a new key frame (e.g. at a scene cut).
    pub fn encode(&mut self, px: &[u32], force_key: bool) -> (Vec<u8>, bool) {
        let cur = Yuv::from_rgb(px, self.width, self.height);
        let key = force_key || self.reference.is_none() || self.since_key >= self.key_interval;
        let (nal, recon) = if key { self.encode_idr(&cur) } else { self.encode_p(&cur) };
        self.reference = Some(recon);
        self.frame_num = (self.frame_num + 1) % 256;
        self.since_key = if key { 1 } else { self.since_key + 1 };
        (nal, key)
    }

    fn slice_header(&self, b: &mut Bits, key: bool) {
        b.ue(0); // first_mb_in_slice
        b.ue(if key { 7 } else { 5 }); // all I / all P
        b.ue(0); // pps id
        b.put(self.frame_num, 8);
        if key {
            b.ue(self.idr_id);
        } else {
            b.flag(false); // num_ref_idx_active_override
            b.flag(false); // ref_pic_list_modification_flag_l0
        }
        if key {
            b.flag(false); // no_output_of_prior_pics
            b.flag(false); // long_term_reference
        } else {
            b.flag(false); // adaptive_ref_pic_marking_mode
        }
        b.se(0); // slice_qp_delta (pic_init_qp already is our qp)
        b.ue(1); // disable the deblocking filter
    }

    fn new_counts(&self) -> Counts {
        Counts { luma: vec![0; self.mbw * 4 * self.mbh * 4], cb: vec![0; self.mbw * 2 * self.mbh * 2], cr: vec![0; self.mbw * 2 * self.mbh * 2], lw: self.mbw * 4, cw: self.mbw * 2 }
    }

    fn encode_idr(&mut self, cur: &Yuv) -> (Vec<u8>, Yuv) {
        let mut b = Bits::new();
        b.put(0x65, 8); // ref_idc 3, IDR slice
        self.frame_num = 0;
        self.slice_header(&mut b, true);
        self.idr_id = (self.idr_id + 1) % 16;
        let mut rec = Yuv { w: cur.w, h: cur.h, y: vec![0; cur.y.len()], u: vec![0; cur.u.len()], v: vec![0; cur.v.len()] };
        let mut counts = self.new_counts();
        for my in 0..self.mbh {
            for mx in 0..self.mbw {
                self.intra_mb(&mut b, cur, &mut rec, &mut counts, mx, my, 0);
            }
        }
        (b.finish(), rec)
    }

    fn encode_p(&mut self, cur: &Yuv) -> (Vec<u8>, Yuv) {
        let refp = self.reference.take().unwrap();
        let mut b = Bits::new();
        b.put(0x41, 8); // ref_idc 2, non-IDR slice
        self.slice_header(&mut b, false);
        let mut rec = refp.clone();
        let mut counts = self.new_counts();
        let mut skip_run = 0u32;
        for my in 0..self.mbh {
            for mx in 0..self.mbw {
                // Code the difference from the same place in the previous frame.
                let coded = self.inter_mb(cur, &refp, &mut rec, &mut counts, mx, my);
                match coded {
                    None => skip_run += 1,
                    Some(mb) => {
                        b.ue(skip_run);
                        skip_run = 0;
                        b.put(0, 0);
                        for (v, l) in mb.bits {
                            b.put(v, l);
                        }
                    }
                }
            }
        }
        if skip_run > 0 {
            b.ue(skip_run);
        }
        (b.finish(), rec)
    }

    /// Intra 16x16 macroblock. `mb_type_base` is 0 in I slices (5 in P slices).
    fn intra_mb(&self, b: &mut Bits, cur: &Yuv, rec: &mut Yuv, counts: &mut Counts, mx: usize, my: usize, mb_type_base: u32) {
        let qp = self.qp;
        let w = cur.w;
        let (x0, y0) = (mx * 16, my * 16);
        let has_top = my > 0;
        let has_left = mx > 0;
        // Pick the 16x16 prediction with the smallest difference.
        let top: Vec<i32> = if has_top { (0..16).map(|i| rec.y[(y0 - 1) * w + x0 + i] as i32).collect() } else { Vec::new() };
        let left: Vec<i32> = if has_left { (0..16).map(|i| rec.y[(y0 + i) * w + x0 - 1] as i32).collect() } else { Vec::new() };
        let dc = match (has_top, has_left) {
            (true, true) => (top.iter().sum::<i32>() + left.iter().sum::<i32>() + 16) >> 5,
            (true, false) => (top.iter().sum::<i32>() + 8) >> 4,
            (false, true) => (left.iter().sum::<i32>() + 8) >> 4,
            _ => 128,
        };
        let src = |x: usize, y: usize| cur.y[(y0 + y) * w + x0 + x] as i32;
        let cost = |mode: u32| -> i32 {
            let mut s = 0;
            for y in 0..16 {
                for x in 0..16 {
                    let p = match mode {
                        0 => top[x],
                        1 => left[y],
                        _ => dc,
                    };
                    s += (src(x, y) - p).abs();
                }
            }
            s
        };
        let mut mode = 2;
        let mut best = cost(2);
        if has_top {
            let c = cost(0);
            if c < best {
                best = c;
                mode = 0;
            }
        }
        if has_left && cost(1) < best {
            mode = 1;
        }
        let pred = |x: usize, y: usize| -> i32 {
            match mode {
                0 => top[x],
                1 => left[y],
                _ => dc,
            }
        };
        // Luma: transform all 16 blocks, pull out the DC coefficients.
        let qbits = 15 + qp / 6;
        let f = (1 << qbits) / 3;
        let mut ac = [[0i32; 16]; 16]; // quantized, raster order per block
        let mut dcs = [0i32; 16];
        for blk in 0..16 {
            let (bx, by) = blk_xy(blk);
            let mut d = [0i32; 16];
            for i in 0..16 {
                let (x, y) = (bx + i % 4, by + i / 4);
                d[i] = src(x, y) - pred(x, y);
            }
            let t = fdct4(&d);
            // DC position in raster order of the 4x4 grid of blocks.
            dcs[(by / 4) * 4 + bx / 4] = t[0];
            for i in 1..16 {
                ac[blk][i] = quant(t[i], MF[(qp % 6) as usize][class(i)], qbits, f);
            }
        }
        let hd = hadamard4(&dcs);
        let mut dcq = [0i32; 16];
        for i in 0..16 {
            dcq[i] = quant(hd[i] / 2, MF[(qp % 6) as usize][0], qbits + 1, 2 * f);
        }
        let any_ac = ac.iter().any(|blk| blk.iter().skip(1).any(|&c| c != 0));
        let cbp_luma = if any_ac { 15 } else { 0 };
        // Chroma.
        let qpc = chroma_qp(qp);
        let (cdc, cac, cpred) = self.chroma_intra(cur, rec, mx, my, qpc);
        let chroma_ac = cac.iter().any(|comp| comp.iter().any(|blk| blk.iter().skip(1).any(|&c| c != 0)));
        let chroma_dc = cdc.iter().any(|comp| comp.iter().any(|&c| c != 0));
        let cbp_chroma = if chroma_ac { 2 } else if chroma_dc { 1 } else { 0 };
        // Syntax.
        let mb_type = 1 + mode + 4 * cbp_chroma + if cbp_luma == 15 { 12 } else { 0 };
        b.ue(mb_type_base + mb_type);
        b.ue(0); // intra_chroma_pred_mode: DC
        b.se(0); // mb_qp_delta
        let (lbx, lby) = (mx * 4, my * 4);
        let scan = |blk: &[i32; 16], start: usize| -> Vec<i32> { (start..16).map(|k| blk[ZIGZAG[k]]).collect() };
        let nc0 = Counts::nc(&counts.luma, counts.lw, lbx, lby);
        residual_block(b, &(0..16).map(|k| dcq[ZIGZAG[k]]).collect::<Vec<_>>(), nc0);
        for blk in 0..16 {
            let (bx, by) = blk_xy(blk);
            let (cx, cy) = (lbx + bx / 4, lby + by / 4);
            let n = if cbp_luma == 15 {
                let nc = Counts::nc(&counts.luma, counts.lw, cx, cy);
                residual_block(b, &scan(&ac[blk], 1), nc)
            } else {
                0
            };
            counts.luma[cy * counts.lw + cx] = n;
        }
        self.chroma_syntax(b, counts, mx, my, &cdc, &cac, cbp_chroma);
        // Reconstruct luma: inverse DC transform, then each block.
        let ihd = hadamard4(&dcq);
        let v0 = V[(qp % 6) as usize][0];
        let mut dcr = [0i32; 16];
        for i in 0..16 {
            dcr[i] = if qp >= 36 { (ihd[i] * v0 * 16) << (qp / 6 - 6) } else { (ihd[i] * v0 * 16 + (1 << (5 - qp / 6))) >> (6 - qp / 6) };
        }
        for blk in 0..16 {
            let (bx, by) = blk_xy(blk);
            let mut d = [0i32; 16];
            d[0] = dcr[(by / 4) * 4 + bx / 4];
            if cbp_luma == 15 {
                for i in 1..16 {
                    d[i] = (ac[blk][i] * V[(qp % 6) as usize][class(i)]) << (qp / 6);
                }
            }
            let r = idct4(&d);
            for i in 0..16 {
                let (x, y) = (bx + i % 4, by + i / 4);
                rec.y[(y0 + y) * w + x0 + x] = (pred(x, y) + r[i]).clamp(0, 255) as u8;
            }
        }
        self.chroma_recon(rec, mx, my, qpc, &cdc, &cac, &cpred, cbp_chroma);
    }

    /// Chroma DC prediction (per 4x4 block, by the standard's rules), and the quantized
    /// DC and AC coefficients for both components.
    #[allow(clippy::type_complexity)]
    fn chroma_intra(&self, cur: &Yuv, rec: &Yuv, mx: usize, my: usize, qpc: i32) -> ([[i32; 4]; 2], [[[i32; 16]; 4]; 2], [[i32; 64]; 2]) {
        let cw = cur.w / 2;
        let (x0, y0) = (mx * 8, my * 8);
        let mut preds = [[0i32; 64]; 2];
        for (ci, (plane, rplane)) in [(&cur.u, &rec.u), (&cur.v, &rec.v)].iter().enumerate() {
            let _ = plane;
            for blk in 0..4 {
                let (bx, by) = ((blk % 2) * 4, (blk / 2) * 4);
                let top = if my > 0 { Some((0..4).map(|i| rplane[(y0 - 1) * cw + x0 + bx + i] as i32).sum::<i32>()) } else { None };
                let left = if mx > 0 { Some((0..4).map(|i| rplane[(y0 + by + i) * cw + x0 - 1] as i32).sum::<i32>()) } else { None };
                let p = match (blk, top, left) {
                    (0 | 3, Some(t), Some(l)) => (t + l + 4) >> 3,
                    (0 | 3, Some(t), None) => (t + 2) >> 2,
                    (0 | 3, None, Some(l)) => (l + 2) >> 2,
                    (1, Some(t), _) => (t + 2) >> 2,
                    (1, None, Some(l)) => (l + 2) >> 2,
                    (2, _, Some(l)) => (l + 2) >> 2,
                    (2, Some(t), None) => (t + 2) >> 2,
                    _ => 128,
                };
                for i in 0..16 {
                    preds[ci][(by + i / 4) * 8 + bx + i % 4] = p;
                }
            }
        }
        let (dc, ac) = self.chroma_residual(cur, mx, my, qpc, &preds, true);
        (dc, ac, preds)
    }

    /// Transform and quantize chroma against a prediction.
    fn chroma_residual(&self, cur: &Yuv, mx: usize, my: usize, qpc: i32, preds: &[[i32; 64]; 2], intra: bool) -> ([[i32; 4]; 2], [[[i32; 16]; 4]; 2]) {
        let cw = cur.w / 2;
        let (x0, y0) = (mx * 8, my * 8);
        let qbits = 15 + qpc / 6;
        let f = if intra { (1 << qbits) / 3 } else { (1 << qbits) / 6 };
        let mut dcq = [[0i32; 4]; 2];
        let mut acq = [[[0i32; 16]; 4]; 2];
        for (ci, plane) in [&cur.u, &cur.v].iter().enumerate() {
            let mut dcs = [0i32; 4];
            for blk in 0..4 {
                let (bx, by) = ((blk % 2) * 4, (blk / 2) * 4);
                let mut d = [0i32; 16];
                for i in 0..16 {
                    let (x, y) = (bx + i % 4, by + i / 4);
                    d[i] = plane[(y0 + y) * cw + x0 + x] as i32 - preds[ci][y * 8 + x];
                }
                let t = fdct4(&d);
                dcs[blk] = t[0];
                for i in 1..16 {
                    acq[ci][blk][i] = quant(t[i], MF[(qpc % 6) as usize][class(i)], qbits, f);
                }
            }
            let h = [dcs[0] + dcs[1] + dcs[2] + dcs[3], dcs[0] - dcs[1] + dcs[2] - dcs[3], dcs[0] + dcs[1] - dcs[2] - dcs[3], dcs[0] - dcs[1] - dcs[2] + dcs[3]];
            for k in 0..4 {
                dcq[ci][k] = quant(h[k], MF[(qpc % 6) as usize][0], qbits + 1, 2 * f);
            }
        }
        (dcq, acq)
    }

    fn chroma_syntax(&self, b: &mut Bits, counts: &mut Counts, mx: usize, my: usize, cdc: &[[i32; 4]; 2], cac: &[[[i32; 16]; 4]; 2], cbp_chroma: u32) {
        if cbp_chroma > 0 {
            for comp in cdc {
                residual_block(b, comp, -1);
            }
        }
        for (ci, comp) in cac.iter().enumerate() {
            for blk in 0..4 {
                let (cx, cy) = (mx * 2 + blk % 2, my * 2 + blk / 2);
                let arr = if ci == 0 { &mut counts.cb } else { &mut counts.cr };
                let n = if cbp_chroma == 2 {
                    let nc = Counts::nc(arr, counts.cw, cx, cy);
                    let coef: Vec<i32> = (1..16).map(|k| comp[blk][ZIGZAG[k]]).collect();
                    residual_block(b, &coef, nc)
                } else {
                    0
                };
                let arr = if ci == 0 { &mut counts.cb } else { &mut counts.cr };
                arr[cy * counts.cw + cx] = n;
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn chroma_recon(&self, rec: &mut Yuv, mx: usize, my: usize, qpc: i32, cdc: &[[i32; 4]; 2], cac: &[[[i32; 16]; 4]; 2], preds: &[[i32; 64]; 2], cbp_chroma: u32) {
        let cw = rec.w / 2;
        let (x0, y0) = (mx * 8, my * 8);
        let v0 = V[(qpc % 6) as usize][0];
        for ci in 0..2 {
            let c = if cbp_chroma > 0 { cdc[ci] } else { [0; 4] };
            let f = [c[0] + c[1] + c[2] + c[3], c[0] - c[1] + c[2] - c[3], c[0] + c[1] - c[2] - c[3], c[0] - c[1] - c[2] + c[3]];
            let plane = if ci == 0 { &mut rec.u } else { &mut rec.v };
            for blk in 0..4 {
                let mut d = [0i32; 16];
                d[0] = ((f[blk] * v0 * 16) << (qpc / 6)) >> 5;
                if cbp_chroma == 2 {
                    for i in 1..16 {
                        d[i] = (cac[ci][blk][i] * V[(qpc % 6) as usize][class(i)]) << (qpc / 6);
                    }
                }
                let r = idct4(&d);
                let (bx, by) = ((blk % 2) * 4, (blk / 2) * 4);
                for i in 0..16 {
                    let (x, y) = (bx + i % 4, by + i / 4);
                    plane[(y0 + y) * cw + x0 + x] = (preds[ci][y * 8 + x] + r[i]).clamp(0, 255) as u8;
                }
            }
        }
    }

    /// A P macroblock against the co-located reference: None = skip (nothing to code).
    fn inter_mb(&self, cur: &Yuv, refp: &Yuv, rec: &mut Yuv, counts: &mut Counts, mx: usize, my: usize) -> Option<MbBits> {
        let qp = self.qp;
        let w = cur.w;
        let (x0, y0) = (mx * 16, my * 16);
        let qbits = 15 + qp / 6;
        let f = (1 << qbits) / 6;
        let mut q = [[0i32; 16]; 16];
        for blk in 0..16 {
            let (bx, by) = blk_xy(blk);
            let mut d = [0i32; 16];
            for i in 0..16 {
                let (x, y) = (x0 + bx + i % 4, y0 + by + i / 4);
                d[i] = cur.y[y * w + x] as i32 - refp.y[y * w + x] as i32;
            }
            let t = fdct4(&d);
            for i in 0..16 {
                q[blk][i] = quant(t[i], MF[(qp % 6) as usize][class(i)], qbits, f);
            }
        }
        let mut cbp_luma = 0u32;
        for b8 in 0..4 {
            if (0..4).any(|k| q[b8 * 4 + k].iter().any(|&c| c != 0)) {
                cbp_luma |= 1 << b8;
            }
        }
        let qpc = chroma_qp(qp);
        let cw = cur.w / 2;
        let mut preds = [[0i32; 64]; 2];
        for (ci, plane) in [&refp.u, &refp.v].iter().enumerate() {
            for y in 0..8 {
                for x in 0..8 {
                    preds[ci][y * 8 + x] = plane[(my * 8 + y) * cw + mx * 8 + x] as i32;
                }
            }
        }
        let (cdc, cac) = self.chroma_residual(cur, mx, my, qpc, &preds, false);
        let chroma_ac = cac.iter().any(|comp| comp.iter().any(|blk| blk.iter().skip(1).any(|&c| c != 0)));
        let chroma_dc = cdc.iter().any(|comp| comp.iter().any(|&c| c != 0));
        let cbp_chroma = if chroma_ac { 2 } else if chroma_dc { 1 } else { 0 };
        let (lbx, lby) = (mx * 4, my * 4);
        if cbp_luma == 0 && cbp_chroma == 0 {
            // Skipped: the reconstruction already holds the reference here; counts stay 0.
            return None;
        }
        let mut b = MbBits { bits: Vec::new() };
        b.ue(0); // P_L0_16x16
        b.se(0); // mvd x
        b.se(0); // mvd y
        let cbp = cbp_luma | cbp_chroma << 4;
        let code = GOLOMB_TO_INTER_CBP.iter().position(|&c| c as u32 == cbp).unwrap() as u32;
        b.ue(code);
        b.se(0); // mb_qp_delta
        let mut bw = Bits::new();
        for blk in 0..16 {
            let (bx, by) = blk_xy(blk);
            let (cx, cy) = (lbx + bx / 4, lby + by / 4);
            let n = if cbp_luma & (1 << (blk / 4)) != 0 {
                let nc = Counts::nc(&counts.luma, counts.lw, cx, cy);
                let coef: Vec<i32> = (0..16).map(|k| q[blk][ZIGZAG[k]]).collect();
                residual_block(&mut bw, &coef, nc)
            } else {
                0
            };
            counts.luma[cy * counts.lw + cx] = n;
        }
        self.chroma_syntax(&mut bw, counts, mx, my, &cdc, &cac, cbp_chroma);
        b.append(bw);
        // Reconstruct.
        for blk in 0..16 {
            let (bx, by) = blk_xy(blk);
            let mut d = [0i32; 16];
            if cbp_luma & (1 << (blk / 4)) != 0 {
                for i in 0..16 {
                    d[i] = (q[blk][i] * V[(qp % 6) as usize][class(i)]) << (qp / 6);
                }
            }
            let r = idct4(&d);
            for i in 0..16 {
                let (x, y) = (x0 + bx + i % 4, y0 + by + i / 4);
                rec.y[y * w + x] = (refp.y[y * w + x] as i32 + r[i]).clamp(0, 255) as u8;
            }
        }
        self.chroma_recon(rec, mx, my, qpc, &cdc, &cac, &preds, cbp_chroma);
        Some(b)
    }
}

/// Bits of one macroblock, kept aside until we know it isn't skipped.
struct MbBits {
    bits: Vec<(u32, u32)>,
}

impl MbBits {
    fn ue(&mut self, v: u32) {
        let x = v as u64 + 1;
        let len = 64 - x.leading_zeros();
        self.bits.push((0, len - 1));
        self.bits.push((x as u32, len));
    }
    fn se(&mut self, v: i32) {
        self.ue(if v > 0 { (2 * v - 1) as u32 } else { (-2 * v) as u32 });
    }
    /// Add whole bytes and the partial bits of another writer.
    fn append(&mut self, bw: Bits) {
        for &byte in &bw.out {
            self.bits.push((byte as u32, 8));
        }
        if bw.n > 0 {
            self.bits.push(((bw.cur & ((1 << bw.n) - 1)) as u32, bw.n));
        }
    }
}

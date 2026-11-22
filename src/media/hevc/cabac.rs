//! HEVC CABAC: the arithmetic decoder, context setup, and residual (coefficient) coding.

use super::tab::INIT_VALUES;
use crate::video::bits::Bits;
use crate::video::tables::CABAC_TABLES;

// Context offsets (in FFmpeg's order, so its initial values apply).
pub const SAO_MERGE: usize = 0;
pub const SAO_TYPE: usize = 1;
pub const SPLIT_CU: usize = 2;
pub const TRANSQUANT_BYPASS: usize = 5;
pub const SKIP: usize = 6;
pub const CU_QP_DELTA: usize = 9;
pub const PRED_MODE: usize = 12;
pub const PART_MODE: usize = 13;
pub const PREV_INTRA_LUMA: usize = 17;
pub const CHROMA_PRED: usize = 18;
pub const MERGE_FLAG: usize = 20;
pub const MERGE_IDX: usize = 21;
pub const INTER_PRED_IDC: usize = 22;
pub const REF_IDX: usize = 27;
pub const MVD_GT0: usize = 31;
pub const MVD_GT1: usize = 33;
pub const MVP_FLAG: usize = 35;
pub const NO_RESIDUAL: usize = 36;
pub const SPLIT_TRANSFORM: usize = 37;
pub const CBF_LUMA: usize = 40;
pub const CBF_CHROMA: usize = 42;
pub const TRANSFORM_SKIP: usize = 47;
const LAST_X: usize = 53;
const LAST_Y: usize = 71;
const SIG_GROUP: usize = 89;
const SIG: usize = 93;
const GT1: usize = 137;
const GT2: usize = 161;
pub const NUM_CTX: usize = 179;

#[derive(Clone)]
pub struct Cabac {
    pub state: [u8; NUM_CTX],
    pub mps: [u8; NUM_CTX],
    range: u32,
    off: u32,
}

impl Cabac {
    pub fn new() -> Cabac {
        Cabac { state: [0; NUM_CTX], mps: [0; NUM_CTX], range: 510, off: 0 }
    }

    /// Contexts for a slice: init_type 0 (I), 1, 2 (P/B, swapped by cabac_init_flag).
    pub fn init_contexts(&mut self, init_type: usize, qp: i32) {
        let qp = qp.clamp(0, 51);
        for i in 0..NUM_CTX {
            let v = INIT_VALUES[init_type * NUM_CTX + i] as i32;
            let m = (v >> 4) * 5 - 45;
            let n = ((v & 15) << 3) - 16;
            let pre = (((m * qp) >> 4) + n).clamp(1, 126);
            if pre <= 63 {
                self.state[i] = (63 - pre) as u8;
                self.mps[i] = 0;
            } else {
                self.state[i] = (pre - 64) as u8;
                self.mps[i] = 1;
            }
        }
    }

    pub fn start(&mut self, b: &mut Bits) {
        self.range = 510;
        self.off = b.read(9);
    }

    #[inline]
    pub fn bin(&mut self, b: &mut Bits, ctx: usize) -> u32 {
        let s = self.state[ctx];
        let lps = CABAC_TABLES[512 + ((self.range >> 6) & 3) as usize * 128 + 2 * s as usize] as u32;
        self.range -= lps;
        let bin;
        if self.off >= self.range {
            bin = 1 - self.mps[ctx] as u32;
            self.off -= self.range;
            self.range = lps;
            if s == 0 {
                self.mps[ctx] ^= 1;
            }
            self.state[ctx] = CABAC_TABLES[1024 + 127 - 2 * s as usize] >> 1;
        } else {
            bin = self.mps[ctx] as u32;
            if s < 62 {
                self.state[ctx] = s + 1;
            }
        }
        if self.range < 256 {
            let shift = self.range.leading_zeros() - 23;
            self.range <<= shift;
            self.off = (self.off << shift) | b.read(shift);
        }
        bin
    }

    #[inline]
    pub fn bypass(&mut self, b: &mut Bits) -> u32 {
        self.off = (self.off << 1) | b.bit();
        if self.off >= self.range {
            self.off -= self.range;
            1
        } else {
            0
        }
    }

    pub fn bypass_bits(&mut self, b: &mut Bits, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bypass(b);
        }
        v
    }

    pub fn terminate(&mut self, b: &mut Bits) -> bool {
        self.range -= 2;
        if self.off >= self.range {
            true
        } else {
            if self.range < 256 {
                self.range <<= 1;
                self.off = (self.off << 1) | b.bit();
            }
            false
        }
    }

    /// After a terminating bin of 1 (end of a substream, PCM): the next data starts at the next
    /// byte (the stop bit is already inside what the decoder has read).
    pub fn finish(&mut self, b: &mut Bits) {
        b.align();
    }

    // ---- Syntax elements ----

    pub fn cu_qp_delta_abs(&mut self, b: &mut Bits) -> i32 {
        let mut prefix = 0;
        let mut inc = 0;
        while prefix < 5 && self.bin(b, CU_QP_DELTA + inc) != 0 {
            prefix += 1;
            inc = 1;
        }
        if prefix >= 5 {
            let mut k = 0;
            let mut suffix = 0;
            while k < 7 && self.bypass(b) != 0 {
                suffix += 1 << k;
                k += 1;
            }
            while k > 0 {
                k -= 1;
                suffix += (self.bypass(b) as i32) << k;
            }
            return prefix + suffix;
        }
        prefix
    }

    pub fn part_mode(&mut self, b: &mut Bits, intra: bool, log2_cb: u32, log2_min_cb: u32, amp: bool) -> u8 {
        // 0 2Nx2N, 1 2NxN, 2 Nx2N, 3 NxN, 4 2NxnU, 5 2NxnD, 6 nLx2N, 7 nRx2N
        if self.bin(b, PART_MODE) != 0 {
            return 0;
        }
        if log2_cb == log2_min_cb {
            if intra {
                return 3;
            }
            if self.bin(b, PART_MODE + 1) != 0 {
                return 1;
            }
            if log2_cb == 3 {
                return 2;
            }
            if self.bin(b, PART_MODE + 2) != 0 {
                return 2;
            }
            return 3;
        }
        if !amp {
            return if self.bin(b, PART_MODE + 1) != 0 { 1 } else { 2 };
        }
        if self.bin(b, PART_MODE + 1) != 0 {
            if self.bin(b, PART_MODE + 3) != 0 {
                return 1;
            }
            return if self.bypass(b) != 0 { 5 } else { 4 };
        }
        if self.bin(b, PART_MODE + 3) != 0 {
            return 2;
        }
        if self.bypass(b) != 0 {
            7
        } else {
            6
        }
    }

    pub fn ref_idx(&mut self, b: &mut Bits, num: usize) -> usize {
        let max = num - 1;
        let max_ctx = max.min(2);
        let mut i = 0;
        while i < max_ctx && self.bin(b, REF_IDX + i) != 0 {
            i += 1;
        }
        if i == 2 {
            while i < max && self.bypass(b) != 0 {
                i += 1;
            }
        }
        i
    }

    /// mvd_coding: the (x, y) difference.
    pub fn mvd(&mut self, b: &mut Bits) -> [i32; 2] {
        let g0x = self.bin(b, MVD_GT0);
        let g0y = self.bin(b, MVD_GT0);
        let g1x = if g0x != 0 { self.bin(b, MVD_GT1 + 1) } else { 0 };
        let g1y = if g0y != 0 { self.bin(b, MVD_GT1 + 1) } else { 0 };
        let mut out = [0; 2];
        for (i, (g0, g1)) in [(g0x, g1x), (g0y, g1y)].iter().enumerate() {
            if *g0 == 0 {
                continue;
            }
            let mut v = 1i32;
            if *g1 != 0 {
                // EG1 code.
                let mut k = 1;
                let mut val = 2i32;
                while k < 31 && self.bypass(b) != 0 {
                    val += 1 << k;
                    k += 1;
                }
                while k > 0 {
                    k -= 1;
                    val += (self.bypass(b) as i32) << k;
                }
                v = val;
            }
            out[i] = if self.bypass(b) != 0 { -v } else { v };
        }
        out
    }

    fn abs_level_remaining(&mut self, b: &mut Bits, rice: u32) -> i32 {
        let mut prefix = 0;
        while prefix < 32 && self.bypass(b) != 0 {
            prefix += 1;
        }
        if prefix < 3 {
            ((prefix as i32) << rice) + self.bypass_bits(b, rice) as i32
        } else {
            let p3 = prefix - 3;
            let k = p3 + rice;
            if k > 22 {
                return 0;
            }
            let suffix = self.bypass_bits(b, k) as i32;
            (((1 << p3) + 2) << rice) + suffix
        }
    }
}

/// What residual coding needs to know about the block.
pub struct ResidualCtx {
    pub log2: u32,
    pub c_idx: usize,
    /// 0 diagonal, 1 horizontal, 2 vertical.
    pub scan: u8,
    pub sign_hiding: bool,
    pub bypass: bool,
    pub transform_skip_allowed: bool,
    pub intra: bool,
    /// Dequantisation: scale, shift, and the scaling matrix (16 = flat).
    pub scale: i64,
    pub shift: u32,
    pub matrix: Option<([u8; 64], u8)>,
}

const CTX_MAP4: [u8; 16] = [0, 1, 4, 5, 2, 3, 4, 5, 6, 6, 8, 8, 7, 7, 8, 8];

const fn diag_table<const N: usize, const M: usize>() -> [(u8, u8); M] {
    let mut t = [(0u8, 0u8); M];
    let mut k = 0;
    let mut d = 0;
    while d < 2 * N - 1 {
        let mut y = if d < N - 1 { d } else { N - 1 } as isize;
        let mut x = d as isize - y;
        while y >= 0 && (x as usize) < N {
            t[k] = (x as u8, y as u8);
            k += 1;
            y -= 1;
            x += 1;
        }
        d += 1;
    }
    t
}

const DIAG2: [(u8, u8); 4] = diag_table::<2, 4>();
const DIAG4: [(u8, u8); 16] = diag_table::<4, 16>();
const DIAG8: [(u8, u8); 64] = diag_table::<8, 64>();

#[inline]
fn scan_xy(scan: u8, n: usize, i: usize) -> (usize, usize) {
    match scan {
        1 => (i % n, i / n),
        2 => (i / n, i % n),
        _ => {
            let (x, y) = match n {
                1 => (0, 0),
                2 => DIAG2[i],
                4 => DIAG4[i],
                _ => DIAG8[i],
            };
            (x as usize, y as usize)
        }
    }
}

/// residual_coding(): coefficients (dequantised, raster order, size 1<<log2 squared) into
/// `coeffs`. Returns (transform_skip, max(last_x, last_y)).
pub fn residual(c: &mut Cabac, b: &mut Bits, r: &ResidualCtx, coeffs: &mut [i32]) -> (bool, usize) {
    let log2 = r.log2;
    let size = 1usize << log2;
    let chroma = r.c_idx > 0;
    let ts = r.transform_skip_allowed && c.bin(b, TRANSFORM_SKIP + chroma as usize) != 0;
    // Last significant position.
    let max = (log2 << 1) - 1;
    let (ctx_off, ctx_shift) = if !chroma { (3 * (log2 - 2) + ((log2 - 1) >> 2), (log2 + 1) >> 2) } else { (15, log2 - 2) };
    let mut px = 0;
    while px < max && c.bin(b, LAST_X + ((px >> ctx_shift) + ctx_off) as usize) != 0 {
        px += 1;
    }
    let mut py = 0;
    while py < max && c.bin(b, LAST_Y + ((py >> ctx_shift) + ctx_off) as usize) != 0 {
        py += 1;
    }
    let suffix = |c: &mut Cabac, b: &mut Bits, p: u32| -> usize {
        if p > 3 {
            let len = (p >> 1) - 1;
            let s = c.bypass_bits(b, len) as usize;
            (1usize << ((p >> 1) - 1)) * (2 + (p & 1) as usize) + s
        } else {
            p as usize
        }
    };
    let mut lx = suffix(c, b, px);
    let mut ly = suffix(c, b, py);
    if r.scan == 2 {
        core::mem::swap(&mut lx, &mut ly);
    }
    let max_xy = lx.max(ly);
    // Scan orders: subblocks and positions in a subblock.
    let nsb = size >> 2;
    let mut sb_order = [(0u8, 0u8); 64];
    for (i, e) in sb_order.iter_mut().enumerate().take(nsb * nsb) {
        let (x, y) = scan_xy(r.scan, nsb, i);
        *e = (x as u8, y as u8);
    }
    let mut pos_order = [(0u8, 0u8); 16];
    for (i, e) in pos_order.iter_mut().enumerate() {
        let (x, y) = scan_xy(r.scan, 4, i);
        *e = (x as u8, y as u8);
    }
    // The last position's index in the scan.
    let (lsx, lsy) = (lx >> 2, ly >> 2);
    let last_sb = (0..nsb * nsb).find(|&i| sb_order[i] == (lsx as u8, lsy as u8)).unwrap_or(0);
    let last_pos = (0..16).find(|&i| pos_order[i] == ((lx & 3) as u8, (ly & 3) as u8)).unwrap_or(0);
    let num_coeff = last_sb * 16 + last_pos + 1;
    for v in coeffs.iter_mut().take(size * size) {
        *v = 0;
    }
    let mut csbf = [[false; 8]; 8];
    let mut greater1_ctx = 1u32;
    let num_last_subset = (num_coeff - 1) >> 4;
    for i in (0..=num_last_subset).rev() {
        let (xs, ys) = (sb_order[i].0 as usize, sb_order[i].1 as usize);
        let mut implicit = false;
        if i < num_last_subset && i > 0 {
            let mut ctx_cg = 0;
            if xs + 1 < nsb {
                ctx_cg += csbf[xs + 1][ys] as usize;
            }
            if ys + 1 < nsb {
                ctx_cg += csbf[xs][ys + 1] as usize;
            }
            csbf[xs][ys] = c.bin(b, SIG_GROUP + ctx_cg.min(1) + if chroma { 2 } else { 0 }) != 0;
            implicit = true;
        } else {
            csbf[xs][ys] = (xs == lsx && ys == lsy) || (xs == 0 && ys == 0);
        }
        let last_scan_pos = num_coeff as isize - (i << 4) as isize - 1;
        let mut sig_idx = [0u8; 16];
        let mut nsig = 0usize;
        let n_end: isize;
        if i == num_last_subset {
            n_end = last_scan_pos - 1;
            sig_idx[0] = last_scan_pos as u8;
            nsig = 1;
        } else {
            n_end = 15;
        }
        let mut prev = 0;
        if xs + 1 < nsb {
            prev |= csbf[xs + 1][ys] as u32;
        }
        if ys + 1 < nsb {
            prev |= (csbf[xs][ys + 1] as u32) << 1;
        }
        if csbf[xs][ys] && n_end >= 0 {
            let sig_ctx = |n: usize| -> usize {
                let (xp, yp) = (pos_order[n].0 as usize, pos_order[n].1 as usize);
                let (xc, yc) = ((xs << 2) + xp, (ys << 2) + yp);
                let s = if log2 == 2 {
                    CTX_MAP4[(yc << 2) + xc] as usize
                } else if xc + yc == 0 {
                    0
                } else {
                    let mut s = match prev {
                        0 => if xp + yp == 0 { 2 } else if xp + yp < 3 { 1 } else { 0 },
                        1 => if yp == 0 { 2 } else if yp == 1 { 1 } else { 0 },
                        2 => if xp == 0 { 2 } else if xp == 1 { 1 } else { 0 },
                        _ => 2,
                    };
                    if !chroma {
                        if xs > 0 || ys > 0 {
                            s += 3;
                        }
                        s += if log2 == 3 { if r.scan == 0 { 9 } else { 15 } } else { 21 };
                    } else {
                        s += if log2 == 3 { 9 } else { 12 };
                    }
                    s
                };
                if chroma {
                    27 + s
                } else {
                    s
                }
            };
            let nb0 = nsig;
            let mut n = n_end;
            while n > 0 {
                if c.bin(b, SIG + sig_ctx(n as usize)) != 0 {
                    sig_idx[nsig] = n as u8;
                    nsig += 1;
                }
                n -= 1;
            }
            if nsig != nb0 {
                implicit = false;
            }
            if !implicit {
                if c.bin(b, SIG + sig_ctx(0)) != 0 {
                    sig_idx[nsig] = 0;
                    nsig += 1;
                }
            } else {
                sig_idx[nsig] = 0;
                nsig += 1;
            }
        }
        if nsig == 0 {
            continue;
        }
        let mut ctx_set = if i > 0 && !chroma { 2 } else { 0 };
        if i != num_last_subset && greater1_ctx == 0 {
            ctx_set += 1;
        }
        greater1_ctx = 1;
        let mut gt1 = [0u8; 8];
        let mut first_g1: isize = -1;
        for m in 0..nsig.min(8) {
            let inc = (ctx_set << 2) + greater1_ctx as usize + if chroma { 16 } else { 0 };
            let f = c.bin(b, GT1 + inc);
            gt1[m] = f as u8;
            if f != 0 && first_g1 < 0 {
                first_g1 = m as isize;
            }
            greater1_ctx = if f != 0 { 0 } else if greater1_ctx > 0 && greater1_ctx < 3 { greater1_ctx + 1 } else { greater1_ctx };
        }
        let last_nz = sig_idx[0] as isize;
        let first_nz = sig_idx[nsig - 1] as isize;
        let hidden = r.sign_hiding && !r.bypass && last_nz - first_nz >= 4;
        if first_g1 >= 0 {
            gt1[first_g1 as usize] += c.bin(b, GT2 + ctx_set + if chroma { 4 } else { 0 }) as u8;
        }
        let nsign = if hidden { nsig - 1 } else { nsig };
        let mut signs = c.bypass_bits(b, nsign as u32) << (32 - nsign as u32).min(31);
        if nsign == 0 {
            signs = 0;
        }
        let mut rice = 0u32;
        let mut sum_abs = 0i32;
        for m in 0..nsig {
            let n = sig_idx[m] as usize;
            let mut level: i32;
            if m < 8 {
                level = 1 + gt1[m] as i32;
                if level == if m as isize == first_g1 { 3 } else { 2 } {
                    level += c.abs_level_remaining(b, rice);
                    if level > (3 << rice) {
                        rice = (rice + 1).min(4);
                    }
                }
            } else {
                level = 1 + c.abs_level_remaining(b, rice);
                if level > (3 << rice) {
                    rice = (rice + 1).min(4);
                }
            }
            if hidden {
                sum_abs += level;
                if n as isize == first_nz && sum_abs & 1 != 0 {
                    level = -level;
                }
            }
            if signs & 0x8000_0000 != 0 {
                level = -level;
            }
            signs <<= 1;
            let (xc, yc) = ((xs << 2) + pos_order[n].0 as usize, (ys << 2) + pos_order[n].1 as usize);
            let v = if r.bypass {
                level as i64
            } else {
                let m = match &r.matrix {
                    Some((sl, dc)) if !(ts && log2 > 2) => {
                        if xc == 0 && yc == 0 && log2 >= 4 {
                            *dc as i64
                        } else {
                            let p = match log2 {
                                3 => (yc << 3) + xc,
                                4 => ((yc >> 1) << 3) + (xc >> 1),
                                5 => ((yc >> 2) << 3) + (xc >> 2),
                                _ => (yc << 2) + xc,
                            };
                            sl[p] as i64
                        }
                    }
                    _ => 16,
                };
                ((level as i64 * r.scale * m + (1 << (r.shift - 1))) >> r.shift).clamp(-32768, 32767)
            };
            coeffs[yc * size + xc] = v as i32;
        }
    }
    (ts, max_xy)
}

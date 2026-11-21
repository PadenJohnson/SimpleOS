//! H.264 CABAC: the arithmetic decoder and the macroblock syntax it carries (Main and High
//! profile videos, i.e. nearly all phone videos).

use super::bits::Bits;
use super::h264::ZIGZAG;
use super::h264d::{b_sub, b_type, partitions, Ctx, Mb, MbInfo, MbType, SliceKind, BLK_XY, ZIGZAG8};
use super::tables::{CABAC_INIT_I, CABAC_INIT_PB, CABAC_TABLES, SIG_8X8};

pub(crate) struct Cabac {
    state: [u8; 1024],
    mps: [u8; 1024],
    range: u32,
    off: u32,
}

#[inline]
fn lps_range(state: u8, q: u32) -> u32 {
    CABAC_TABLES[512 + q as usize * 128 + 2 * state as usize] as u32
}

#[inline]
fn trans_lps(state: u8) -> u8 {
    CABAC_TABLES[1024 + 127 - 2 * state as usize] >> 1
}

const CBF_OFF: [usize; 5] = [0, 4, 8, 12, 16];
const SIG_OFF: [usize; 5] = [0, 15, 29, 44, 47];
const ABS_OFF: [usize; 5] = [0, 10, 20, 30, 39];

impl Cabac {
    pub fn new() -> Cabac {
        Cabac { state: [0; 1024], mps: [0; 1024], range: 510, off: 0 }
    }

    pub fn init_slice(&mut self, kind: SliceKind, idc: usize, qp: i32) {
        let qp = qp.clamp(0, 51);
        for ctx in 0..1024 {
            let (m, n) = if kind == SliceKind::I { (CABAC_INIT_I[2 * ctx], CABAC_INIT_I[2 * ctx + 1]) } else { (CABAC_INIT_PB[idc * 2048 + 2 * ctx], CABAC_INIT_PB[idc * 2048 + 2 * ctx + 1]) };
            let pre = (((m as i32 * qp) >> 4) + n as i32).clamp(1, 126);
            if pre <= 63 {
                self.state[ctx] = (63 - pre) as u8;
                self.mps[ctx] = 0;
            } else {
                self.state[ctx] = (pre - 64) as u8;
                self.mps[ctx] = 1;
            }
        }
    }

    pub fn start(&mut self, b: &mut Bits) {
        self.range = 510;
        self.off = b.read(9);
    }

    #[inline]
    fn bin(&mut self, b: &mut Bits, ctx: usize) -> u32 {
        let s = self.state[ctx];
        let lps = lps_range(s, (self.range >> 6) & 3);
        self.range -= lps;
        let bin;
        if self.off >= self.range {
            bin = 1 - self.mps[ctx] as u32;
            self.off -= self.range;
            self.range = lps;
            if s == 0 {
                self.mps[ctx] ^= 1;
            }
            self.state[ctx] = trans_lps(s);
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
    fn bypass(&mut self, b: &mut Bits) -> u32 {
        self.off = (self.off << 1) | b.bit();
        if self.off >= self.range {
            self.off -= self.range;
            1
        } else {
            0
        }
    }

    pub fn terminate(&mut self, b: &mut Bits) -> bool {
        self.range -= 2;
        if self.off >= self.range {
            true
        } else {
            while self.range < 256 {
                self.range <<= 1;
                self.off = (self.off << 1) | b.bit();
            }
            false
        }
    }

    pub fn skip_flag(&mut self, b: &mut Bits, ctx: &Ctx) -> bool {
        let cond = |m: Option<&MbInfo>| m.is_some_and(|m| !matches!(m.ty, MbType::PSkip | MbType::BSkip)) as usize;
        let base = if ctx.kind == SliceKind::B { 24 } else { 11 };
        self.bin(b, base + cond(ctx.left()) + cond(ctx.top())) == 1
    }

    /// Intra mb_type (I slice: offset 3 with neighbour context; P/B: suffix at `base`).
    fn intra_mb_type(&mut self, b: &mut Bits, ctx: &Ctx, base: usize, islice: bool) -> u32 {
        let first = if islice {
            let cond = |m: Option<&MbInfo>| m.is_some_and(|m| !matches!(m.ty, MbType::I4x4 | MbType::I8x8)) as usize;
            self.bin(b, 3 + cond(ctx.left()) + cond(ctx.top()))
        } else {
            self.bin(b, base)
        };
        if first == 0 {
            return 0;
        }
        if self.terminate(b) {
            return 25;
        }
        let (luma_ctx, c1, c2, p1, p2) = if islice { (3 + 3, 3 + 4, 3 + 5, 3 + 6, 3 + 7) } else { (base + 1, base + 2, base + 2, base + 3, base + 3) };
        let luma = self.bin(b, luma_ctx);
        let mut chroma = self.bin(b, c1);
        if chroma != 0 {
            chroma += self.bin(b, c2);
        }
        let mode = self.bin(b, p1) << 1 | self.bin(b, p2);
        1 + mode + 4 * chroma + 12 * luma
    }

    fn set_intra(t: u32, mb: &mut Mb) {
        match t {
            0 => mb.ty = MbType::I4x4,
            25 => mb.ty = MbType::IPcm,
            _ => {
                let k = t - 1;
                mb.ty = MbType::I16x16;
                mb.i16_mode = (k % 4) as u8;
                mb.cbp = (((k / 4) % 3) << 4) as u8 | if k >= 12 { 15 } else { 0 };
            }
        }
    }

    fn ref_idx(&mut self, b: &mut Bits, ctx: &Ctx, mb: &Mb, direct: u16, l: usize, x: usize, y: usize) -> i8 {
        let cond = |n: Option<(Option<&MbInfo>, usize, usize)>| -> usize {
            match n {
                None => 0,
                Some((None, x, y)) => (direct & (1 << (y * 4 + x)) == 0 && mb.refs[l][y * 4 + x] > 0) as usize,
                Some((Some(m), x, y)) => {
                    let skip = matches!(m.ty, MbType::PSkip | MbType::BSkip);
                    (!skip && !m.ty.intra() && m.direct & (1 << (y * 4 + x)) == 0 && m.refs[l][y * 4 + x] > 0) as usize
                }
            }
        };
        let inc = cond(ctx.left_blk(x, y)) + 2 * cond(ctx.top_blk(x, y));
        let mut v = 0;
        if self.bin(b, 54 + inc) == 1 {
            v = 1;
            if self.bin(b, 58) == 1 {
                v = 2;
                while self.bin(b, 59) == 1 && v < 32 {
                    v += 1;
                }
            }
        }
        v
    }

    fn mvd(&mut self, b: &mut Bits, ctx: &Ctx, mb: &Mb, l: usize, x: usize, y: usize, comp: usize) -> i16 {
        let get = |n: Option<(Option<&MbInfo>, usize, usize)>| -> i32 {
            match n {
                None => 0,
                Some((None, x, y)) => (mb.mvd[l][y * 4 + x][comp] as i32).abs(),
                Some((Some(m), x, y)) => (m.mvd[l][y * 4 + x][comp] as i32).abs(),
            }
        };
        let sum = get(ctx.left_blk(x, y)) + get(ctx.top_blk(x, y));
        let base = if comp == 0 { 40 } else { 47 };
        let inc = if sum < 3 {
            0
        } else if sum > 32 {
            2
        } else {
            1
        };
        if self.bin(b, base + inc) == 0 {
            return 0;
        }
        let mut v = 1i32;
        while v < 9 && self.bin(b, base + [3, 4, 5, 6, 6, 6, 6, 6, 6][v as usize - 1]) == 1 {
            v += 1;
        }
        if v >= 9 {
            let mut k = 3;
            while self.bypass(b) == 1 && k < 24 {
                v += 1 << k;
                k += 1;
            }
            while k > 0 {
                k -= 1;
                v += (self.bypass(b) << k) as i32;
            }
        }
        let v = if self.bypass(b) == 1 { -v } else { v };
        v.clamp(-32768, 32767) as i16
    }

    /// coded_block_flag context increment for block category `cat` at (x, y) / component c.
    fn cbf_inc(ctx: &Ctx, mb: &Mb, cat: usize, c: usize, x: usize, y: usize) -> usize {
        let intra = mb.ty.intra();
        let other = |m: &MbInfo, x: usize, y: usize| -> usize {
            if m.ty == MbType::IPcm {
                return 1;
            }
            if matches!(m.ty, MbType::PSkip | MbType::BSkip) {
                return 0;
            }
            match cat {
                0 => (m.ty == MbType::I16x16 && m.cbf & (1 << 16) != 0) as usize,
                1 | 2 => {
                    if m.cbp & (1 << ((y / 2) * 2 + x / 2)) == 0 {
                        0
                    } else if m.t8x8 {
                        1
                    } else {
                        (m.cbf & (1 << (y * 4 + x)) != 0) as usize
                    }
                }
                3 => (m.cbp >> 4 != 0 && m.cbf & (1 << (17 + c)) != 0) as usize,
                _ => (m.cbp >> 4 == 2 && m.cbf & (1 << (19 + c * 4 + y * 2 + x)) != 0) as usize,
            }
        };
        let cur = |x: usize, y: usize| -> usize {
            match cat {
                1 | 2 => (mb.cbf & (1 << (y * 4 + x)) != 0) as usize,
                _ => (mb.cbf & (1 << (19 + c * 4 + y * 2 + x)) != 0) as usize,
            }
        };
        let size = if cat >= 3 { 2 } else { 4 };
        let (a, bb) = match cat {
            0 | 3 => (ctx.left().map(|m| other(m, 0, 0)).unwrap_or(intra as usize), ctx.top().map(|m| other(m, 0, 0)).unwrap_or(intra as usize)),
            _ => {
                let a = if x > 0 { cur(x - 1, y) } else { ctx.left().map(|m| other(m, size - 1, y)).unwrap_or(intra as usize) };
                let bb = if y > 0 { cur(x, y - 1) } else { ctx.top().map(|m| other(m, x, size - 1)).unwrap_or(intra as usize) };
                (a, bb)
            }
        };
        a + 2 * bb
    }

    /// Read one block's coefficients (scan order) into `out`. Returns the number of non-zero.
    fn residual(&mut self, b: &mut Bits, cat: usize, cbf_inc: Option<usize>, max: usize, out: &mut [i32]) -> u8 {
        if let Some(inc) = cbf_inc {
            if self.bin(b, 85 + CBF_OFF[cat] + inc) == 0 {
                return 0;
            }
        }
        let (sig_base, last_base, abs_base) = if cat == 5 { (402, 417, 426) } else { (105 + SIG_OFF[cat], 166 + SIG_OFF[cat], 227 + ABS_OFF[cat]) };
        let mut sig = [false; 64];
        let mut n = max;
        let mut i = 0;
        while i < max - 1 {
            let (si, li) = match cat {
                3 => (i.min(2), i.min(2)),
                5 => (SIG_8X8[i] as usize, CABAC_TABLES[1280 + i] as usize),
                _ => (i, i),
            };
            if self.bin(b, sig_base + si) == 1 {
                sig[i] = true;
                if self.bin(b, last_base + li) == 1 {
                    n = i + 1;
                    break;
                }
            }
            i += 1;
        }
        if n == max {
            sig[max - 1] = true;
        }
        let (mut gt1, mut eq1) = (0usize, 0usize);
        let mut count = 0u8;
        for i in (0..n).rev() {
            if !sig[i] {
                continue;
            }
            let inc0 = if gt1 != 0 { 0 } else { (1 + eq1).min(4) };
            let mut abs = 1i32;
            if self.bin(b, abs_base + inc0) == 1 {
                let inc = 5 + gt1.min(4 - (cat == 3) as usize);
                abs = 2;
                while abs < 15 && self.bin(b, abs_base + inc) == 1 {
                    abs += 1;
                }
                if abs == 15 {
                    // Exp-Golomb (k = 0) suffix.
                    let mut k = 0;
                    while self.bypass(b) == 1 && k < 24 {
                        abs += 1 << k;
                        k += 1;
                    }
                    while k > 0 {
                        k -= 1;
                        abs += (self.bypass(b) << k) as i32;
                    }
                }
            }
            if abs == 1 {
                eq1 += 1;
            } else {
                gt1 += 1;
            }
            out[i] = if self.bypass(b) == 1 { -abs } else { abs };
            count += 1;
        }
        count
    }

    pub fn parse_mb(&mut self, b: &mut Bits, ctx: &Ctx, prev_qp_nz: bool, mb: &mut Mb) -> Result<(), &'static str> {
        mb.reset(MbType::I4x4);
        let b_slice = ctx.kind == SliceKind::B;
        match ctx.kind {
            SliceKind::I => {
                let t = self.intra_mb_type(b, ctx, 3, true);
                Self::set_intra(t, mb);
            }
            SliceKind::P => {
                if self.bin(b, 14) == 0 {
                    mb.ty = if self.bin(b, 15) == 0 {
                        if self.bin(b, 16) == 0 {
                            MbType::P16x16
                        } else {
                            MbType::P8x8
                        }
                    } else if self.bin(b, 17) == 0 {
                        MbType::P8x16
                    } else {
                        MbType::P16x8
                    };
                } else {
                    let t = self.intra_mb_type(b, ctx, 17, false);
                    Self::set_intra(t, mb);
                }
            }
            SliceKind::B => {
                let cond = |m: Option<&MbInfo>| m.is_some_and(|m| !matches!(m.ty, MbType::BSkip | MbType::BDirect)) as usize;
                let t = if self.bin(b, 27 + cond(ctx.left()) + cond(ctx.top())) == 0 {
                    0
                } else if self.bin(b, 27 + 3) == 0 {
                    1 + self.bin(b, 27 + 5)
                } else {
                    let mut bits = self.bin(b, 27 + 4) << 3;
                    bits |= self.bin(b, 27 + 5) << 2;
                    bits |= self.bin(b, 27 + 5) << 1;
                    bits |= self.bin(b, 27 + 5);
                    if bits < 8 {
                        bits + 3
                    } else if bits == 13 {
                        100
                    } else if bits == 14 {
                        11
                    } else if bits == 15 {
                        22
                    } else {
                        ((bits << 1) | self.bin(b, 27 + 5)) - 4
                    }
                };
                if t == 100 {
                    let t = self.intra_mb_type(b, ctx, 32, false);
                    Self::set_intra(t, mb);
                } else {
                    mb.ty = b_type(t);
                }
            }
        }
        if mb.ty == MbType::IPcm {
            b.align();
            mb.pcm = (0..384).map(|_| b.read(8) as u8).collect();
            mb.nz = [16; 24];
            mb.cbp = 0x2F;
            mb.cbf = u32::MAX;
            return Ok(());
        }
        let intra = mb.ty.intra();
        let left = ctx.left();
        let top = ctx.top();
        if mb.ty == MbType::I4x4 {
            if ctx.t8x8_mode {
                let c = |m: Option<&MbInfo>| m.is_some_and(|m| m.t8x8) as usize;
                if self.bin(b, 399 + c(left) + c(top)) == 1 {
                    mb.ty = MbType::I8x8;
                    mb.t8x8 = true;
                }
            }
            let n = if mb.ty == MbType::I8x8 { 4 } else { 16 };
            for k in 0..n {
                mb.ipred[k] = if self.bin(b, 68) == 1 {
                    None
                } else {
                    let mut r = self.bin(b, 69);
                    r |= self.bin(b, 69) << 1;
                    r |= self.bin(b, 69) << 2;
                    Some(r as u8)
                };
            }
        }
        if intra {
            let c = |m: Option<&MbInfo>| m.is_some_and(|m| m.ty.intra() && m.ty != MbType::IPcm && m.chroma_mode != 0) as usize;
            let mut mode = 0;
            if self.bin(b, 64 + c(left) + c(top)) == 1 {
                mode = 1;
                if self.bin(b, 67) == 1 {
                    mode = 2;
                    if self.bin(b, 67) == 1 {
                        mode = 3;
                    }
                }
            }
            mb.chroma_mode = mode;
        } else if mb.ty != MbType::BDirect {
            let is8x8 = matches!(mb.ty, MbType::P8x8 | MbType::B8x8);
            if is8x8 {
                for k in 0..4 {
                    mb.sub[k] = if b_slice {
                        if self.bin(b, 36) == 0 {
                            0
                        } else if self.bin(b, 37) == 0 {
                            1 + self.bin(b, 39) as u8
                        } else {
                            let mut t = 3u8;
                            if self.bin(b, 38) == 1 {
                                if self.bin(b, 39) == 1 {
                                    11 + self.bin(b, 39) as u8
                                } else {
                                    t += 4;
                                    t += 2 * self.bin(b, 39) as u8;
                                    t + self.bin(b, 39) as u8
                                }
                            } else {
                                t += 2 * self.bin(b, 39) as u8;
                                t + self.bin(b, 39) as u8
                            }
                        }
                    } else if self.bin(b, 21) == 1 {
                        0
                    } else if self.bin(b, 22) == 0 {
                        1
                    } else if self.bin(b, 23) == 1 {
                        2
                    } else {
                        3
                    };
                }
            }
            let parts = partitions(mb.ty, &mb.sub, b_slice);
            // Blocks predicted in direct mode (for the ref_idx contexts).
            let mut direct = 0u16;
            for &(x, y, w, h, _, d) in &parts {
                if d {
                    for yy in y..y + h {
                        for xx in x..x + w {
                            direct |= 1 << (yy * 4 + xx);
                        }
                    }
                }
            }
            for l in 0..2 {
                let mut seen = [false; 4];
                for &(x, y, w, h, pred, d) in &parts {
                    if d || pred & (1 << l) == 0 {
                        continue;
                    }
                    let k8 = (y / 2) * 2 + x / 2;
                    let (fx, fy, fw, fh) = if is8x8 { ((x / 2) * 2, (y / 2) * 2, 2, 2) } else { (x, y, w, h) };
                    if is8x8 && seen[k8] {
                        continue;
                    }
                    seen[k8] = true;
                    let r = if ctx.num_ref[l] > 1 { self.ref_idx(b, ctx, mb, direct, l, fx, fy) } else { 0 };
                    for yy in fy..fy + fh {
                        for xx in fx..fx + fw {
                            mb.refs[l][yy * 4 + xx] = r;
                        }
                    }
                }
            }
            for l in 0..2 {
                for &(x, y, w, h, pred, d) in &parts {
                    if d || pred & (1 << l) == 0 {
                        continue;
                    }
                    let dx = self.mvd(b, ctx, mb, l, x, y, 0);
                    let dy = self.mvd(b, ctx, mb, l, x, y, 1);
                    for yy in y..y + h {
                        for xx in x..x + w {
                            mb.mvd[l][yy * 4 + xx] = [dx, dy];
                        }
                    }
                }
            }
        }
        if mb.ty != MbType::I16x16 {
            // coded_block_pattern
            let mut cbp = 0u8;
            for b8 in 0..4usize {
                let cond_other = |m: Option<&MbInfo>, bit: usize| -> usize {
                    match m {
                        None => 0,
                        Some(m) if m.ty == MbType::IPcm => 0,
                        Some(m) if matches!(m.ty, MbType::PSkip | MbType::BSkip) => 1,
                        Some(m) => (m.cbp & (1 << bit) == 0) as usize,
                    }
                };
                let a = if b8 & 1 == 1 { (cbp & (1 << (b8 - 1)) == 0) as usize } else { cond_other(left, b8 + 1) };
                let t = if b8 >= 2 { (cbp & (1 << (b8 - 2)) == 0) as usize } else { cond_other(top, b8 + 2) };
                if self.bin(b, 73 + a + 2 * t) == 1 {
                    cbp |= 1 << b8;
                }
            }
            let cc = |m: Option<&MbInfo>, two: bool| -> usize {
                match m {
                    None => 0,
                    Some(m) if m.ty == MbType::IPcm => 1,
                    Some(m) if matches!(m.ty, MbType::PSkip | MbType::BSkip) => 0,
                    Some(m) => (if two { m.cbp >> 4 == 2 } else { m.cbp >> 4 != 0 }) as usize,
                }
            };
            if self.bin(b, 77 + cc(left, false) + 2 * cc(top, false)) == 1 {
                cbp |= if self.bin(b, 77 + 4 + cc(left, true) + 2 * cc(top, true)) == 1 { 0x20 } else { 0x10 };
            }
            mb.cbp = cbp;
            if !intra && cbp & 15 != 0 && ctx.t8x8_mode {
                let small = match mb.ty {
                    MbType::P8x8 => mb.sub.iter().any(|&s| s != 0),
                    MbType::B8x8 => mb.sub.iter().any(|&s| {
                        let (_, shape, direct) = b_sub(s);
                        if direct {
                            !ctx.direct8x8
                        } else {
                            shape != 0
                        }
                    }),
                    MbType::BDirect => !ctx.direct8x8,
                    _ => false,
                };
                if !small {
                    let c = |m: Option<&MbInfo>| m.is_some_and(|m| m.t8x8) as usize;
                    if self.bin(b, 399 + c(left) + c(top)) == 1 {
                        mb.t8x8 = true;
                    }
                }
            }
        }
        if mb.cbp == 0 && mb.ty != MbType::I16x16 {
            return Ok(());
        }
        // mb_qp_delta
        let mut k = 0u32;
        if self.bin(b, 60 + prev_qp_nz as usize) == 1 {
            k = 1;
            if self.bin(b, 62) == 1 {
                k = 2;
                while self.bin(b, 63) == 1 && k < 60 {
                    k += 1;
                }
            }
        }
        mb.qp_delta = if k % 2 == 1 { k.div_ceil(2) as i32 } else { -((k / 2) as i32) };
        // Residual.
        if mb.ty == MbType::I16x16 {
            let inc = Self::cbf_inc(ctx, mb, 0, 0, 0, 0);
            let mut l = [0i32; 16];
            if self.residual(b, 0, Some(inc), 16, &mut l) > 0 {
                mb.cbf |= 1 << 16;
            }
            for k in 0..16 {
                mb.dc[ZIGZAG[k]] = l[k];
            }
        }
        for k8 in 0..4 {
            if mb.cbp & (1 << k8) == 0 {
                continue;
            }
            if mb.t8x8 {
                let mut l = [0i32; 64];
                let n = self.residual(b, 5, None, 64, &mut l);
                let mut c8 = [0i32; 64];
                for k in 0..64 {
                    c8[ZIGZAG8[k]] = l[k];
                }
                mb.luma8[k8] = c8;
                for j in 0..4 {
                    let (x, y) = BLK_XY[k8 * 4 + j];
                    mb.nz[y * 4 + x] = n;
                    mb.cbf |= 1 << (y * 4 + x);
                }
            } else {
                for j in 0..4 {
                    let (x, y) = BLK_XY[k8 * 4 + j];
                    let mut l = [0i32; 16];
                    if mb.ty == MbType::I16x16 {
                        let inc = Self::cbf_inc(ctx, mb, 1, 0, x, y);
                        let n = self.residual(b, 1, Some(inc), 15, &mut l);
                        for k in 0..15 {
                            mb.luma[y * 4 + x][ZIGZAG[k + 1]] = l[k];
                        }
                        mb.nz[y * 4 + x] = n;
                        if n > 0 {
                            mb.cbf |= 1 << (y * 4 + x);
                        }
                    } else {
                        let inc = Self::cbf_inc(ctx, mb, 2, 0, x, y);
                        let n = self.residual(b, 2, Some(inc), 16, &mut l);
                        for k in 0..16 {
                            mb.luma[y * 4 + x][ZIGZAG[k]] = l[k];
                        }
                        mb.nz[y * 4 + x] = n;
                        if n > 0 {
                            mb.cbf |= 1 << (y * 4 + x);
                        }
                    }
                }
            }
        }
        let cc = mb.cbp >> 4;
        if cc != 0 {
            for c in 0..2 {
                let inc = Self::cbf_inc(ctx, mb, 3, c, 0, 0);
                let mut l = [0i32; 16];
                if self.residual(b, 3, Some(inc), 4, &mut l) > 0 {
                    mb.cbf |= 1 << (17 + c);
                }
                mb.cdc[c].copy_from_slice(&l[..4]);
            }
        }
        if cc == 2 {
            for c in 0..2 {
                for k in 0..4 {
                    let (x, y) = (k % 2, k / 2);
                    let inc = Self::cbf_inc(ctx, mb, 4, c, x, y);
                    let mut l = [0i32; 16];
                    let n = self.residual(b, 4, Some(inc), 15, &mut l);
                    for i in 0..15 {
                        mb.cac[c * 4 + k][ZIGZAG[i + 1]] = l[i];
                    }
                    mb.nz[16 + c * 4 + k] = n;
                    if n > 0 {
                        mb.cbf |= 1 << (19 + c * 4 + k);
                    }
                }
            }
        }
        Ok(())
    }
}

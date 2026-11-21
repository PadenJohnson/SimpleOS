//! H.264 CAVLC macroblock parsing (Baseline profile, and Main/High streams that don't use
//! CABAC).

use super::bits::Bits;
use super::h264::{CDC_BITS, CDC_LEN, CTZ_BITS, CTZ_LEN, CT_BITS, CT_LEN, GOLOMB_TO_INTER_CBP, RUN_BITS, RUN_LEN, TZ_BITS, TZ_LEN, ZIGZAG};
use super::h264d::{b_sub, b_type, partitions, Ctx, Mb, MbInfo, MbType, SliceKind, BLK_XY, ZIGZAG8};

const GOLOMB_TO_INTRA_CBP: [u8; 48] = [
    47, 31, 15, 0, 23, 27, 29, 30, 7, 11, 13, 14, 39, 43, 45, 46, 16, 3, 5, 10, 12, 19, 21, 26, 28, 35, 37, 42, 44, 1, 2, 4, 8, 17, 18, 20, 24, 6, 9, 22, 25, 32, 33, 34, 36, 40, 38, 41,
];

fn intra_type(t: u32, b: &mut Bits, ctx: &Ctx, mb: &mut Mb) -> Result<(), &'static str> {
    match t {
        0 => {
            mb.ty = MbType::I4x4;
        }
        1..=24 => {
            let k = t - 1;
            mb.ty = MbType::I16x16;
            mb.i16_mode = (k % 4) as u8;
            mb.cbp = (((k / 4) % 3) << 4) as u8 | if k >= 12 { 15 } else { 0 };
        }
        25 => mb.ty = MbType::IPcm,
        _ => return Err("bad intra mb_type"),
    }
    let _ = (b, ctx);
    Ok(())
}

fn te(b: &mut Bits, range: usize) -> i8 {
    if range == 1 {
        (b.bit() ^ 1) as i8
    } else {
        b.ue().min(31) as i8
    }
}

/// Neighbour total_coeff for nC: block (x, y) of plane `p` (0 luma, 1 Cb, 2 Cr).
fn nc(ctx: &Ctx, mb: &Mb, p: usize, x: usize, y: usize) -> i32 {
    let size = if p == 0 { 4 } else { 2 };
    let idx = |m_nz: &[u8; 24], x: usize, y: usize| if p == 0 { m_nz[y * 4 + x] } else { m_nz[16 + (p - 1) * 4 + y * 2 + x] };
    let get = |n: Option<&MbInfo>, x: usize, y: usize| -> i32 {
        match n {
            None => idx(&mb.nz, x, y) as i32,
            Some(m) => {
                if m.ty == MbType::IPcm {
                    16
                } else {
                    idx(&m.nz, x, y) as i32
                }
            }
        }
    };
    let a = if x > 0 { Some(get(None, x - 1, y)) } else { ctx.left().map(|m| get(Some(m), size - 1, y)) };
    let bb = if y > 0 { Some(get(None, x, y - 1)) } else { ctx.top().map(|m| get(Some(m), x, size - 1)) };
    match (a, bb) {
        (Some(a), Some(b)) => (a + b + 1) >> 1,
        (Some(a), None) => a,
        (None, Some(b)) => b,
        _ => 0,
    }
}

fn coeff_token(b: &mut Bits, nc: i32) -> Result<(usize, usize), &'static str> {
    if nc >= 8 {
        let v = b.read(6) as usize;
        return Ok(if v == 3 { (0, 0) } else { ((v >> 2) + 1, v & 3) });
    }
    let (lens, codes, n): (&[u8], &[u8], usize) = if nc < 0 {
        (&CDC_LEN, &CDC_BITS, 20)
    } else {
        let t = match nc {
            0..=1 => 0,
            2..=3 => 1,
            _ => 2,
        };
        (&CT_LEN[t], &CT_BITS[t], 68)
    };
    let peek = b.peek(16);
    for idx in 0..n {
        let l = lens[idx] as u32;
        if l == 0 {
            continue;
        }
        if peek >> (16 - l) == codes[idx] as u32 {
            b.skip(l as usize);
            return Ok((idx / 4, idx % 4));
        }
    }
    Err("bad coeff_token")
}

/// Read one block's levels into `out[0..max]` (scan order). Returns total_coeff.
fn residual_block(b: &mut Bits, nc: i32, out: &mut [i32], max: usize) -> Result<u8, &'static str> {
    let (total, t1) = coeff_token(b, nc)?;
    if total == 0 {
        return Ok(0);
    }
    if total > max {
        return Err("too many coefficients");
    }
    let mut levels = [0i32; 16];
    let mut suffix_len: u32 = if total > 10 && t1 < 3 { 1 } else { 0 };
    for (i, level) in levels.iter_mut().enumerate().take(total) {
        if i < t1 {
            *level = 1 - 2 * b.bit() as i32;
            continue;
        }
        let mut prefix = 0u32;
        while b.bit() == 0 {
            prefix += 1;
            if prefix > 32 {
                return Err("bad level");
            }
        }
        let mut code = (prefix.min(15) << suffix_len) as i32;
        let ssize = if prefix == 14 && suffix_len == 0 { 4 } else if prefix >= 15 { prefix - 3 } else { suffix_len };
        if ssize > 0 {
            code += b.read(ssize) as i32;
        }
        if prefix >= 15 && suffix_len == 0 {
            code += 15;
        }
        if prefix >= 16 {
            code += (1 << (prefix - 3)) - 4096;
        }
        if i == t1 && t1 < 3 {
            code += 2;
        }
        *level = if code % 2 == 0 { (code + 2) >> 1 } else { (-code - 1) >> 1 };
        if suffix_len == 0 {
            suffix_len = 1;
        }
        if level.abs() > (3 << (suffix_len - 1)) && suffix_len < 6 {
            suffix_len += 1;
        }
    }
    let mut zeros = if total < max {
        let (lens, codes): (&[u8], &[u8]) = if nc < 0 { (CTZ_LEN[total - 1], CTZ_BITS[total - 1]) } else { (TZ_LEN[total - 1], TZ_BITS[total - 1]) };
        let peek = b.peek(16);
        let mut found = None;
        for k in 0..lens.len() {
            let l = lens[k] as u32;
            if peek >> (16 - l) == codes[k] as u32 {
                b.skip(l as usize);
                found = Some(k);
                break;
            }
        }
        found.ok_or("bad total_zeros")?
    } else {
        0
    };
    // Runs, from the highest coefficient down.
    let mut runs = [0usize; 16];
    for run in runs.iter_mut().take(total - 1) {
        if zeros == 0 {
            break;
        }
        let t = zeros.min(7) - 1;
        let peek = b.peek(16);
        let mut found = None;
        for k in 0..RUN_LEN[t].len() {
            let l = RUN_LEN[t][k] as u32;
            if peek >> (16 - l) == RUN_BITS[t][k] as u32 {
                b.skip(l as usize);
                found = Some(k);
                break;
            }
        }
        let r = found.ok_or("bad run_before")?;
        if r > zeros {
            return Err("bad run_before");
        }
        *run = r;
        zeros -= r;
    }
    runs[total - 1] = zeros;
    let mut pos: isize = -1;
    for i in (0..total).rev() {
        pos += runs[i] as isize + 1;
        if pos as usize >= max {
            return Err("coefficients out of range");
        }
        out[pos as usize] = levels[i];
    }
    Ok(total as u8)
}

pub(crate) fn parse_mb(b: &mut Bits, ctx: &Ctx, mb: &mut Mb) -> Result<(), &'static str> {
    let t = b.ue();
    mb.reset(MbType::I4x4);
    let b_slice = ctx.kind == SliceKind::B;
    match ctx.kind {
        SliceKind::I => intra_type(t, b, ctx, mb)?,
        SliceKind::P => match t {
            0 => mb.ty = MbType::P16x16,
            1 => mb.ty = MbType::P16x8,
            2 => mb.ty = MbType::P8x16,
            3 => mb.ty = MbType::P8x8,
            4 => mb.ty = MbType::P8x8Ref0,
            _ => intra_type(t - 5, b, ctx, mb)?,
        },
        SliceKind::B => {
            if t < 23 {
                mb.ty = b_type(t)
            } else {
                intra_type(t - 23, b, ctx, mb)?
            }
        }
    }
    if mb.ty == MbType::IPcm {
        b.align();
        mb.pcm = (0..384).map(|_| b.read(8) as u8).collect();
        mb.nz = [16; 24];
        mb.cbp = 0x2F;
        return Ok(());
    }
    let intra = mb.ty.intra();
    if mb.ty == MbType::I4x4 {
        if ctx.t8x8_mode && b.flag() {
            mb.ty = MbType::I8x8;
            mb.t8x8 = true;
        }
        let n = if mb.ty == MbType::I8x8 { 4 } else { 16 };
        for k in 0..n {
            mb.ipred[k] = if b.flag() { None } else { Some(b.read(3) as u8) };
        }
    }
    if intra {
        mb.chroma_mode = b.ue().min(3) as u8;
    } else {
        // Motion: reference indices then vector differences.
        let parts = partitions(mb.ty, &mb.sub, b_slice);
        let _ = parts;
        let is8x8 = matches!(mb.ty, MbType::P8x8 | MbType::P8x8Ref0 | MbType::B8x8);
        if is8x8 {
            for k in 0..4 {
                mb.sub[k] = b.ue().min(12) as u8;
            }
        }
        let parts = partitions(mb.ty, &mb.sub, b_slice);
        // Reference index per 8x8 (or partition): read in partition order, skipping repeats
        // within the same 8x8 for sub-partitions.
        for l in 0..2 {
            let mut seen = [false; 4];
            for &(x, y, w, h, pred, direct) in &parts {
                let k8 = (y / 2) * 2 + x / 2;
                if direct || pred & (1 << l) == 0 {
                    continue;
                }
                if is8x8 && seen[k8] {
                    // same sub-macroblock: same reference
                    let r = mb.refs[l][(y / 2) * 8 + (x / 2) * 2];
                    for yy in y..y + h {
                        for xx in x..x + w {
                            mb.refs[l][yy * 4 + xx] = r;
                        }
                    }
                    continue;
                }
                seen[k8] = true;
                let r = if ctx.num_ref[l] > 1 && mb.ty != MbType::P8x8Ref0 { te(b, ctx.num_ref[l] - 1) } else { 0 };
                // Fill the whole partition (for 8x8 types: the whole sub-macroblock).
                let (fx, fy, fw, fh) = if is8x8 { ((x / 2) * 2, (y / 2) * 2, 2, 2) } else { (x, y, w, h) };
                for yy in fy..fy + fh {
                    for xx in fx..fx + fw {
                        mb.refs[l][yy * 4 + xx] = r;
                    }
                }
            }
        }
        for l in 0..2 {
            for &(x, y, w, h, pred, direct) in &parts {
                if direct || pred & (1 << l) == 0 {
                    continue;
                }
                let d = [b.se().clamp(-32768, 32767) as i16, b.se().clamp(-32768, 32767) as i16];
                for yy in y..y + h {
                    for xx in x..x + w {
                        mb.mvd[l][yy * 4 + xx] = d;
                    }
                }
            }
        }
    }
    if mb.ty != MbType::I16x16 {
        let code = b.ue() as usize;
        if code >= 48 {
            return Err("bad coded_block_pattern");
        }
        mb.cbp = if intra { GOLOMB_TO_INTRA_CBP[code] } else { GOLOMB_TO_INTER_CBP[code] };
        if !intra && mb.cbp & 15 != 0 && ctx.t8x8_mode {
            let small = match mb.ty {
                MbType::P8x8 | MbType::P8x8Ref0 => mb.sub.iter().any(|&s| s != 0),
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
            if !small && b.flag() {
                mb.t8x8 = true;
            }
        }
    }
    if mb.cbp == 0 && mb.ty != MbType::I16x16 {
        return Ok(());
    }
    mb.qp_delta = b.se();
    if !(-26..=25).contains(&mb.qp_delta) {
        return Err("bad mb_qp_delta");
    }
    // Residual: luma.
    if mb.ty == MbType::I16x16 {
        let mut l = [0i32; 16];
        residual_block(b, nc(ctx, mb, 0, 0, 0), &mut l, 16)?;
        for k in 0..16 {
            mb.dc[ZIGZAG[k]] = l[k];
        }
    }
    for k8 in 0..4 {
        if mb.cbp & (1 << k8) == 0 {
            continue;
        }
        if mb.t8x8 {
            let mut c8 = [0i32; 64];
            for j in 0..4 {
                let (x, y) = BLK_XY[k8 * 4 + j];
                let mut l = [0i32; 16];
                let n = residual_block(b, nc(ctx, mb, 0, x, y), &mut l, 16)?;
                mb.nz[y * 4 + x] = n;
                for k in 0..16 {
                    c8[ZIGZAG8[4 * k + j]] = l[k];
                }
            }
            mb.luma8[k8] = c8;
        } else {
            for j in 0..4 {
                let (x, y) = BLK_XY[k8 * 4 + j];
                let mut l = [0i32; 16];
                let blk = &mut mb.luma[y * 4 + x];
                if mb.ty == MbType::I16x16 {
                    let n = residual_block(b, nc(ctx, mb, 0, x, y), &mut l, 15)?;
                    for k in 0..15 {
                        mb.luma[y * 4 + x][ZIGZAG[k + 1]] = l[k];
                    }
                    mb.nz[y * 4 + x] = n;
                } else {
                    let _ = blk;
                    let n = residual_block(b, nc(ctx, mb, 0, x, y), &mut l, 16)?;
                    for k in 0..16 {
                        mb.luma[y * 4 + x][ZIGZAG[k]] = l[k];
                    }
                    mb.nz[y * 4 + x] = n;
                }
            }
        }
    }
    // Chroma.
    let cc = mb.cbp >> 4;
    if cc != 0 {
        for c in 0..2 {
            let mut l = [0i32; 16];
            residual_block(b, -1, &mut l, 4)?;
            mb.cdc[c].copy_from_slice(&l[..4]);
        }
    }
    if cc == 2 {
        for c in 0..2 {
            for k in 0..4 {
                let (x, y) = (k % 2, k / 2);
                let mut l = [0i32; 16];
                let n = residual_block(b, nc(ctx, mb, c + 1, x, y), &mut l, 15)?;
                for i in 0..15 {
                    mb.cac[c * 4 + k][ZIGZAG[i + 1]] = l[i];
                }
                mb.nz[16 + c * 4 + k] = n;
            }
        }
    }
    Ok(())
}

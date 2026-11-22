//! Smaller formats: Netpbm (PBM/PGM/PPM/PAM), Targa (TGA), Windows icons (ICO/CUR) and
//! Photoshop (PSD/PSB, the flattened picture).

use super::{argb, image, Rd};
use crate::image::Image;
use alloc::vec;
use alloc::vec::Vec;

// ---- Netpbm ----

pub fn decode_pnm(d: &[u8]) -> Result<Image, &'static str> {
    let err = "Damaged PNM file";
    if d.len() < 3 || d[0] != b'P' {
        return Err("Not a valid PNM");
    }
    let kind = d[1];
    let mut p = 2;
    let token = |p: &mut usize| -> Option<usize> {
        loop {
            while *p < d.len() && d[*p].is_ascii_whitespace() {
                *p += 1;
            }
            if *p < d.len() && d[*p] == b'#' {
                while *p < d.len() && d[*p] != b'\n' {
                    *p += 1;
                }
                continue;
            }
            break;
        }
        let s = *p;
        while *p < d.len() && d[*p].is_ascii_digit() {
            *p += 1;
        }
        core::str::from_utf8(&d[s..*p]).ok()?.parse().ok()
    };
    let (w, h, maxval, depth);
    if kind == b'7' {
        // PAM: header lines "WIDTH n" ... "ENDHDR".
        let end = d.windows(6).take(1024).position(|w| w == b"ENDHDR").ok_or(err)?;
        let text = core::str::from_utf8(&d[..end]).map_err(|_| err)?;
        let field = |name: &str| -> Option<usize> { text[..end].lines().find_map(|l| l.strip_prefix(name)).and_then(|v| v.trim().parse().ok()) };
        w = field("WIDTH").ok_or(err)?;
        h = field("HEIGHT").ok_or(err)?;
        depth = field("DEPTH").unwrap_or(3);
        maxval = field("MAXVAL").unwrap_or(255);
        p = end + 6;
        while p < d.len() && d[p] != b'\n' {
            p += 1;
        }
        p += 1;
    } else {
        w = token(&mut p).ok_or(err)?;
        h = token(&mut p).ok_or(err)?;
        maxval = if kind == b'1' || kind == b'4' { 1 } else { token(&mut p).ok_or(err)? };
        depth = match kind {
            b'3' | b'6' => 3,
            _ => 1,
        };
        p += 1; // one whitespace byte before binary data
    }
    crate::image::check_size(w, h)?;
    let maxval = maxval.max(1);
    let mut px = vec![0xFF00_0000u32; w * h];
    let scale = |v: usize| ((v.min(maxval) * 255 + maxval / 2) / maxval) as u8;
    match kind {
        b'1' | b'2' | b'3' => {
            // Plain text.
            let mut q = p - 1;
            for i in 0..w * h {
                let mut c = [0u8; 3];
                for k in 0..depth {
                    let v = if kind == b'1' {
                        while q < d.len() && !(d[q] == b'0' || d[q] == b'1') {
                            q += 1;
                        }
                        let v = if d.get(q) == Some(&b'1') { 0 } else { 255 };
                        q += 1;
                        v
                    } else {
                        scale(token(&mut q).unwrap_or(0))
                    };
                    c[k] = v;
                }
                px[i] = if depth == 1 { argb(255, c[0], c[0], c[0]) } else { argb(255, c[0], c[1], c[2]) };
            }
        }
        b'4' => {
            let stride = w.div_ceil(8);
            for y in 0..h {
                for x in 0..w {
                    let b = d.get(p + y * stride + x / 8).copied().unwrap_or(0);
                    let v = if b & (0x80 >> (x % 8)) != 0 { 0 } else { 255 };
                    px[y * w + x] = argb(255, v, v, v);
                }
            }
        }
        _ => {
            let bps = if maxval > 255 { 2 } else { 1 };
            let sample = |i: usize| -> u8 {
                let o = p + i * bps;
                let v = if bps == 2 { u16::from_be_bytes([d.get(o).copied().unwrap_or(0), d.get(o + 1).copied().unwrap_or(0)]) as usize } else { d.get(o).copied().unwrap_or(0) as usize };
                scale(v)
            };
            for i in 0..w * h {
                let b = i * depth;
                px[i] = match depth {
                    1 => {
                        let v = sample(b);
                        argb(255, v, v, v)
                    }
                    2 => {
                        let v = sample(b);
                        argb(sample(b + 1), v, v, v)
                    }
                    3 => argb(255, sample(b), sample(b + 1), sample(b + 2)),
                    _ => argb(sample(b + 3), sample(b), sample(b + 1), sample(b + 2)),
                };
            }
        }
    }
    image(w, h, px)
}

// ---- Targa ----

pub fn looks_like_tga(d: &[u8]) -> bool {
    d.len() > 18 && matches!(d[2], 1 | 2 | 3 | 9 | 10 | 11) && d[1] <= 1 && matches!(d[16], 8 | 15 | 16 | 24 | 32)
}

pub fn decode_tga(d: &[u8]) -> Result<Image, &'static str> {
    let err = "Damaged TGA file";
    let r = Rd { d, le: true };
    let id_len = d[0] as usize;
    let cmap_type = d[1];
    let kind = d[2];
    let cmap_first = r.u16(3).ok_or(err)? as usize;
    let cmap_len = r.u16(5).ok_or(err)? as usize;
    let cmap_bits = d[7] as usize;
    let w = r.u16(12).ok_or(err)? as usize;
    let h = r.u16(14).ok_or(err)? as usize;
    let bpp = d[16] as usize;
    let desc = d[17];
    crate::image::check_size(w, h)?;
    let mut p = 18 + id_len;
    let col = |b: &[u8], bits: usize| -> u32 {
        match bits {
            8 => argb(255, b[0], b[0], b[0]),
            15 | 16 => {
                let v = u16::from_le_bytes([b[0], b[1]]);
                let c = |s: u16| (((v >> s) & 31) as u32 * 255 / 31) as u8;
                argb(255, c(10), c(5), c(0))
            }
            24 => argb(255, b[2], b[1], b[0]),
            _ => argb(b[3], b[2], b[1], b[0]),
        }
    };
    let mut cmap = Vec::new();
    if cmap_type == 1 {
        let eb = cmap_bits.div_ceil(8);
        for k in 0..cmap_len {
            let s = d.get(p + k * eb..p + k * eb + eb).ok_or(err)?;
            cmap.push(col(s, cmap_bits));
        }
        p += cmap_len * eb;
    }
    let pb = bpp.div_ceil(8);
    let pixel = |s: &[u8]| -> u32 {
        if kind == 1 || kind == 9 {
            let i = if pb == 2 { u16::from_le_bytes([s[0], s[1]]) as usize } else { s[0] as usize };
            cmap.get(i.wrapping_sub(cmap_first)).copied().unwrap_or(0xFF00_0000)
        } else {
            col(s, if kind == 3 || kind == 11 { 8 } else { bpp })
        }
    };
    let mut seq = Vec::with_capacity(w * h);
    if kind >= 9 {
        while seq.len() < w * h && p < d.len() {
            let c = d[p];
            p += 1;
            let n = (c & 0x7F) as usize + 1;
            if c & 0x80 != 0 {
                let s = d.get(p..p + pb).ok_or(err)?;
                let v = pixel(s);
                p += pb;
                for _ in 0..n {
                    seq.push(v);
                }
            } else {
                for _ in 0..n {
                    let s = d.get(p..p + pb).ok_or(err)?;
                    seq.push(pixel(s));
                    p += pb;
                }
            }
        }
    } else {
        for i in 0..w * h {
            let s = d.get(p + i * pb..p + i * pb + pb).ok_or(err)?;
            seq.push(pixel(s));
        }
    }
    seq.resize(w * h, 0xFF00_0000);
    // 32-bit Targas without alpha bits in the descriptor are opaque.
    if bpp == 32 && desc & 15 == 0 {
        seq.iter_mut().for_each(|v| *v |= 0xFF00_0000);
    }
    let top = desc & 0x20 != 0;
    let right = desc & 0x10 != 0;
    let mut px = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let sy = if top { y } else { h - 1 - y };
            let sx = if right { w - 1 - x } else { x };
            px[y * w + x] = seq[sy * w + sx];
        }
    }
    image(w, h, px)
}

// ---- ICO / CUR ----

pub fn decode_ico(d: &[u8]) -> Result<Image, &'static str> {
    let r = Rd { d, le: true };
    let n = r.u16(4).ok_or("Damaged icon file")? as usize;
    let mut best: Option<(usize, usize, usize, usize)> = None; // (area, bpp, offset, size)
    for k in 0..n.min(256) {
        let e = 6 + k * 16;
        let w = match r.u8(e) {
            Some(0) => 256,
            Some(v) => v as usize,
            None => break,
        };
        let h = match r.u8(e + 1) {
            Some(0) => 256,
            Some(v) => v as usize,
            None => break,
        };
        let bpp = r.u16(e + 6).unwrap_or(0) as usize;
        let size = r.u32(e + 8).unwrap_or(0) as usize;
        let off = r.u32(e + 12).unwrap_or(0) as usize;
        let cand = (w * h, bpp, off, size);
        if best.is_none_or(|b| (cand.0, cand.1) > (b.0, b.1)) {
            best = Some(cand);
        }
    }
    let (_, _, off, size) = best.ok_or("This icon has no pictures")?;
    let data = d.get(off..off.saturating_add(size).min(d.len())).ok_or("Damaged icon file")?;
    if data.starts_with(b"\x89PNG") {
        crate::image::decode(data)
    } else {
        super::bmp::decode_dib(data, None, true)
    }
}

// ---- Photoshop ----

pub fn decode_psd(d: &[u8]) -> Result<Image, &'static str> {
    let err = "Damaged Photoshop file";
    let r = Rd { d, le: false };
    let version = r.u16(4).ok_or(err)?;
    let channels = r.u16(12).ok_or(err)? as usize;
    let h = r.u32(14).ok_or(err)? as usize;
    let w = r.u32(18).ok_or(err)? as usize;
    let depth = r.u16(22).ok_or(err)? as usize;
    let mode = r.u16(24).ok_or(err)?;
    crate::image::check_size(w, h)?;
    if depth != 8 && depth != 16 {
        return Err("This Photoshop file's colour depth isn't supported");
    }
    let mut p = 26;
    let cm_len = r.u32(p).ok_or(err)? as usize;
    let palette = r.slice(p + 4, cm_len).unwrap_or(&[]);
    p += 4 + cm_len;
    p += 4 + r.u32(p).ok_or(err)? as usize; // image resources
    let lm_len = if version == 2 { (r.u32(p).ok_or(err)? as usize) << 32 | r.u32(p + 4).ok_or(err)? as usize } else { r.u32(p).ok_or(err)? as usize };
    p += if version == 2 { 8 } else { 4 } + lm_len;
    let comp = r.u16(p).ok_or(err)?;
    p += 2;
    let bps = depth / 8;
    let plane = w * h * bps;
    let nch = channels.min(5);
    let mut planes = vec![vec![0u8; plane]; nch];
    match comp {
        0 => {
            for c in 0..nch {
                let s = r.slice(p + c * plane, plane).ok_or(err)?;
                planes[c].copy_from_slice(s);
            }
        }
        1 => {
            let cw = if version == 2 { 4 } else { 2 };
            let counts_at = p;
            let mut q = p + channels * h * cw;
            for c in 0..channels {
                for y in 0..h {
                    let ci = counts_at + (c * h + y) * cw;
                    let n = if cw == 4 { r.u32(ci).ok_or(err)? as usize } else { r.u16(ci).ok_or(err)? as usize };
                    if c < nch {
                        let src = r.slice(q, n).ok_or(err)?;
                        let row = &mut planes[c][y * w * bps..(y + 1) * w * bps];
                        super::tiff::packbits_into(src, row);
                    }
                    q += n;
                }
            }
        }
        _ => return Err("This Photoshop file's compression isn't supported"),
    }
    let s = |c: usize, i: usize| -> u8 { planes.get(c).map(|pl| pl[i * bps]).unwrap_or(255) };
    let mut px = vec![0u32; w * h];
    for i in 0..w * h {
        px[i] = match mode {
            3 => argb(255, s(0, i), s(1, i), s(2, i)),
            1 | 8 => {
                let v = s(0, i);
                argb(255, v, v, v)
            }
            2 => {
                let k = s(0, i) as usize;
                let g = |c: usize| palette.get(c * 256 + k).copied().unwrap_or(0);
                argb(255, g(0), g(1), g(2))
            }
            4 => {
                // CMYK stored inverted (0 = full ink).
                let (c, m, y, k) = (s(0, i) as u32, s(1, i) as u32, s(2, i) as u32, s(3, i) as u32);
                argb(255, (c * k / 255) as u8, (m * k / 255) as u8, (y * k / 255) as u8)
            }
            _ => return Err("This Photoshop file's colour mode isn't supported"),
        };
    }
    image(w, h, px)
}

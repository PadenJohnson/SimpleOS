//! TIFF: strips or tiles; uncompressed, LZW, Deflate, PackBits or JPEG; predictor 2;
//! 1/2/4/8/16-bit samples; bilevel, grey, palette, RGB(A) and CMYK; chunky or planar.

use super::{argb, image, Rd};
use crate::image::Image;
use alloc::vec;
use alloc::vec::Vec;

/// One IFD entry's values.
#[derive(Clone)]
pub struct Entry {
    pub tag: u16,
    pub kind: u16,
    pub count: usize,
    /// Where the values are (file offset).
    pub at: usize,
}

pub struct Ifd<'a> {
    pub r: Rd<'a>,
    pub entries: Vec<Entry>,
    #[allow(dead_code)]
    pub next: usize,
}

fn type_size(kind: u16) -> usize {
    match kind {
        1 | 2 | 6 | 7 => 1,
        3 | 8 => 2,
        4 | 9 | 11 | 13 => 4,
        5 | 10 | 12 => 8,
        16..=18 => 8,
        _ => 1,
    }
}

impl<'a> Ifd<'a> {
    pub fn read(d: &'a [u8], le: bool, at: usize) -> Option<Ifd<'a>> {
        let r = Rd { d, le };
        let n = r.u16(at)? as usize;
        if n == 0 || n > 1000 {
            return None;
        }
        let mut entries = Vec::with_capacity(n);
        for k in 0..n {
            let e = at + 2 + k * 12;
            let tag = r.u16(e)?;
            let kind = r.u16(e + 2)?;
            let count = r.u32(e + 4)? as usize;
            let size = type_size(kind).saturating_mul(count);
            let at = if size <= 4 { e + 8 } else { r.u32(e + 8)? as usize };
            entries.push(Entry { tag, kind, count, at });
        }
        let next = r.u32(at + 2 + n * 12).unwrap_or(0) as usize;
        Some(Ifd { r, entries, next })
    }

    pub fn entry(&self, tag: u16) -> Option<&Entry> {
        self.entries.iter().find(|e| e.tag == tag)
    }

    /// All values of an integer tag.
    pub fn vals(&self, tag: u16) -> Vec<u32> {
        let Some(e) = self.entry(tag) else { return Vec::new() };
        let mut v = Vec::with_capacity(e.count.min(1 << 20));
        for i in 0..e.count.min(1 << 20) {
            let x = match e.kind {
                1 | 7 | 6 => self.r.u8(e.at + i).map(|x| x as u32),
                3 | 8 => self.r.u16(e.at + i * 2).map(|x| x as u32),
                4 | 9 | 13 => self.r.u32(e.at + i * 4),
                _ => None,
            };
            match x {
                Some(x) => v.push(x),
                None => break,
            }
        }
        v
    }

    pub fn val(&self, tag: u16) -> Option<u32> {
        self.vals(tag).first().copied()
    }

    pub fn bytes(&self, tag: u16) -> Option<&'a [u8]> {
        let e = self.entry(tag)?;
        self.r.slice(e.at, e.count * type_size(e.kind))
    }
}

pub fn header(d: &[u8]) -> Option<(bool, usize)> {
    let le = match d.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let r = Rd { d, le };
    let magic = r.u16(2)?;
    if magic != 42 && magic != 0x4F52 && magic != 0x5352 && magic != 0x55 {
        return None;
    }
    Some((le, r.u32(4)? as usize))
}

pub fn packbits_into(src: &[u8], dst: &mut [u8]) {
    let (mut p, mut o) = (0, 0);
    while p < src.len() && o < dst.len() {
        let n = src[p] as i8;
        p += 1;
        if n >= 0 {
            let n = n as usize + 1;
            for k in 0..n {
                if o < dst.len() {
                    dst[o] = src.get(p + k).copied().unwrap_or(0);
                    o += 1;
                }
            }
            p += n;
        } else if n != -128 {
            let v = src.get(p).copied().unwrap_or(0);
            p += 1;
            for _ in 0..(1 - n as isize) as usize {
                if o < dst.len() {
                    dst[o] = v;
                    o += 1;
                }
            }
        }
    }
}

/// TIFF LZW (MSB-first codes, early code-width change).
fn lzw(src: &[u8], want: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(want);
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut first = vec![0u8; 4096];
    let mut len = vec![0u16; 4096];
    for i in 0..256 {
        suffix[i] = i as u8;
        first[i] = i as u8;
        len[i] = 1;
    }
    let mut size = 9u32;
    let mut next = 258u32;
    let mut old: Option<u32> = None;
    let mut pos = 0usize;
    let mut tmp = vec![0u8; 4096];
    let get = |pos: &mut usize, n: u32| -> Option<u32> {
        let mut v = 0;
        for _ in 0..n {
            let b = *src.get(*pos >> 3)?;
            v = (v << 1) | ((b >> (7 - (*pos & 7))) & 1) as u32;
            *pos += 1;
        }
        Some(v)
    };
    while out.len() < want {
        let Some(code) = get(&mut pos, size) else { break };
        if code == 256 {
            size = 9;
            next = 258;
            old = None;
            continue;
        }
        if code == 257 {
            break;
        }
        let emit = |c: u32, out: &mut Vec<u8>, tmp: &mut [u8]| {
            let n = len[c as usize] as usize;
            let mut k = c as usize;
            for i in (0..n).rev() {
                tmp[i] = suffix[k];
                k = prefix[k] as usize;
            }
            out.extend_from_slice(&tmp[..n]);
        };
        match old {
            None => {
                if code >= 256 {
                    break;
                }
                out.push(code as u8);
            }
            Some(o) => {
                let fc = if code < next {
                    emit(code, &mut out, &mut tmp);
                    first[code as usize]
                } else if code == next {
                    let fc = first[o as usize];
                    emit(o, &mut out, &mut tmp);
                    out.push(fc);
                    fc
                } else {
                    break;
                };
                if next < 4096 {
                    prefix[next as usize] = o as u16;
                    suffix[next as usize] = fc;
                    first[next as usize] = first[o as usize];
                    len[next as usize] = len[o as usize] + 1;
                    next += 1;
                }
            }
        }
        old = Some(code);
        if next + 1 >= (1 << size) && size < 12 {
            size += 1;
        }
    }
    out
}

fn decompress(comp: u32, src: &[u8], want: usize) -> Result<Vec<u8>, &'static str> {
    let mut raw = match comp {
        1 => src.to_vec(),
        5 => lzw(src, want),
        8 | 32946 => miniz_oxide::inflate::decompress_to_vec_zlib(src).or_else(|_| miniz_oxide::inflate::decompress_to_vec(src)).map_err(|_| "Damaged TIFF file")?,
        32773 => {
            let mut v = vec![0u8; want];
            packbits_into(src, &mut v);
            v
        }
        _ => return Err("This TIFF's compression isn't supported"),
    };
    raw.resize(want, 0);
    Ok(raw)
}

/// Uncompressed / LZW / Deflate YCbCr with chroma subsampling (data units of Y..Y Cb Cr).
fn decode_ycbcr(d: &[u8], ifd: &Ifd, w: usize, h: usize, comp: u32) -> Result<Image, &'static str> {
    let ss = ifd.vals(530);
    let (sh, sv) = (ss.first().copied().unwrap_or(2).clamp(1, 4) as usize, ss.get(1).copied().unwrap_or(2).clamp(1, 4) as usize);
    let rps = (ifd.val(278).unwrap_or(h as u32) as usize).min(h).max(1);
    let offsets = ifd.vals(273);
    let counts = ifd.vals(279);
    // Reference black / white (rationals).
    let mut rb = [0f32, 255.0, 128.0, 255.0, 128.0, 255.0];
    if let Some(e) = ifd.entry(532) {
        for (k, v) in rb.iter_mut().enumerate() {
            if let (Some(n), Some(dd)) = (ifd.r.u32(e.at + k * 8), ifd.r.u32(e.at + k * 8 + 4)) {
                if dd != 0 {
                    *v = n as f32 / dd as f32;
                }
            }
        }
    }
    let unit = sh * sv + 2;
    let across = w.div_ceil(sh);
    let mut px = vec![0xFF00_0000u32; w * h];
    for (s, &off) in offsets.iter().enumerate() {
        let off = off as usize;
        let cnt = counts.get(s).map(|&c| c as usize).unwrap_or(d.len().saturating_sub(off));
        let Some(src) = d.get(off..(off + cnt).min(d.len())) else { continue };
        let urows = rps.div_ceil(sv);
        let raw = decompress(comp, src, across * urows * unit)?;
        for ur in 0..urows {
            for ux in 0..across {
                let u = &raw[(ur * across + ux) * unit..(ur * across + ux + 1) * unit];
                let cb = (u[sh * sv] as f32 - rb[2]) * 127.0 / (rb[3] - rb[2]).max(1.0);
                let cr = (u[sh * sv + 1] as f32 - rb[4]) * 127.0 / (rb[5] - rb[4]).max(1.0);
                for j in 0..sv {
                    for i in 0..sh {
                        let (x, y) = (ux * sh + i, s * rps + ur * sv + j);
                        if x >= w || y >= h || y >= (s + 1) * rps {
                            continue;
                        }
                        let yv = (u[j * sh + i] as f32 - rb[0]) * 255.0 / (rb[1] - rb[0]).max(1.0);
                        let c = |v: f32| (v + 0.5).clamp(0.0, 255.0) as u8;
                        px[y * w + x] = argb(255, c(yv + 1.402 * cr), c(yv - 0.344_136 * cb - 0.714_136 * cr), c(yv + 1.772 * cb));
                    }
                }
            }
        }
    }
    image(w, h, px)
}

/// Decode the picture described by `ifd`.
pub fn decode_ifd(d: &[u8], ifd: &Ifd) -> Result<Image, &'static str> {
    let err = "Damaged TIFF file";
    if ifd.val(262) == Some(6) && !matches!(ifd.val(259).unwrap_or(1), 6 | 7) && ifd.val(277) == Some(3) && ifd.entry(322).is_none() {
        let w = ifd.val(256).ok_or(err)? as usize;
        let h = ifd.val(257).ok_or(err)? as usize;
        crate::image::check_size(w, h)?;
        return decode_ycbcr(d, ifd, w, h, ifd.val(259).unwrap_or(1));
    }
    let w = ifd.val(256).ok_or(err)? as usize;
    let h = ifd.val(257).ok_or(err)? as usize;
    crate::image::check_size(w, h)?;
    let bps = ifd.val(258).unwrap_or(1) as usize;
    let comp = ifd.val(259).unwrap_or(1);
    let photo = ifd.val(262).unwrap_or(if comp == 7 { 6 } else { 1 });
    let spp = ifd.val(277).unwrap_or(1) as usize;
    let planar = ifd.val(284).unwrap_or(1) == 2 && spp > 1;
    let predictor = ifd.val(317).unwrap_or(1);
    let extra = ifd.vals(338);
    let fmt = ifd.val(339).unwrap_or(1);
    let le = ifd.r.le;
    if !matches!(bps, 1 | 2 | 4 | 8 | 16) {
        return Err("This TIFF's colour depth isn't supported");
    }
    if fmt == 3 {
        return Err("Floating-point TIFFs aren't supported");
    }
    let tiled = ifd.entry(322).is_some();
    let (bw, bh) = if tiled { (ifd.val(322).ok_or(err)? as usize, ifd.val(323).ok_or(err)? as usize) } else { (w, (ifd.val(278).unwrap_or(h as u32) as usize).min(h).max(1)) };
    let offsets = if tiled { ifd.vals(324) } else { ifd.vals(273) };
    let counts = if tiled { ifd.vals(325) } else { ifd.vals(279) };
    if offsets.is_empty() || bw == 0 || bh == 0 {
        return Err(err);
    }
    let across = w.div_ceil(bw);
    let down = h.div_ceil(bh);
    let per_plane = across * down;
    let spp_chunk = if planar { 1 } else { spp };
    let row_bytes = (bw * spp_chunk * bps).div_ceil(8);
    // Samples of the whole picture: sample[(y*w + x)*spp + s] in 0..=255.
    let mut samples = vec![0u8; w * h * spp];
    let jpeg_tables = ifd.bytes(347);
    for plane in 0..if planar { spp } else { 1 } {
        for b in 0..per_plane {
            let idx = plane * per_plane + b;
            let (Some(&off), cnt) = (offsets.get(idx), counts.get(idx).copied()) else { continue };
            let off = off as usize;
            let cnt = cnt.map(|c| c as usize).unwrap_or(d.len().saturating_sub(off));
            let Some(src) = d.get(off..off.saturating_add(cnt).min(d.len())) else { continue };
            let (bx, by) = ((b % across) * bw, (b / across) * bh);
            if comp == 7 || comp == 6 {
                // Each strip / tile is a JPEG (tables may be shared).
                let mut j = Vec::new();
                match jpeg_tables {
                    Some(t) if t.len() > 4 && src.len() > 2 && src.starts_with(&[0xFF, 0xD8]) => {
                        j.extend_from_slice(&t[..t.len() - 2]);
                        j.extend_from_slice(&src[2..]);
                    }
                    _ => j.extend_from_slice(src),
                }
                let img = crate::image::decode_jpeg_raw(&j, photo == 2)?;
                for y in 0..img.h.min(bh) {
                    for x in 0..img.w.min(bw) {
                        let (ox, oy) = (bx + x, by + y);
                        if ox < w && oy < h {
                            let p = img.px[y * img.w + x];
                            let o = (oy * w + ox) * spp;
                            let rgb = [(p >> 16) as u8, (p >> 8) as u8, p as u8];
                            for s in 0..spp.min(3) {
                                samples[o + s] = if spp == 1 { (p >> 8) as u8 } else { rgb[s] };
                            }
                        }
                    }
                }
                continue;
            }
            let rows = bh;
            let want = row_bytes * rows;
            let mut raw = decompress(comp, src, want)?;
            if predictor == 2 {
                for y in 0..rows {
                    let row = &mut raw[y * row_bytes..(y + 1) * row_bytes];
                    if bps == 8 {
                        for i in spp_chunk..bw * spp_chunk {
                            row[i] = row[i].wrapping_add(row[i - spp_chunk]);
                        }
                    } else if bps == 16 {
                        for i in spp_chunk..bw * spp_chunk {
                            let g = |r: &[u8], k: usize| if le { u16::from_le_bytes([r[k * 2], r[k * 2 + 1]]) } else { u16::from_be_bytes([r[k * 2], r[k * 2 + 1]]) };
                            let v = g(row, i).wrapping_add(g(row, i - spp_chunk));
                            let bytes = if le { v.to_le_bytes() } else { v.to_be_bytes() };
                            row[i * 2] = bytes[0];
                            row[i * 2 + 1] = bytes[1];
                        }
                    }
                }
            }
            let maxv = (1u32 << bps) - 1;
            for y in 0..rows {
                let oy = by + y;
                if oy >= h {
                    break;
                }
                let row = &raw[y * row_bytes..(y + 1) * row_bytes];
                for x in 0..bw {
                    let ox = bx + x;
                    if ox >= w {
                        break;
                    }
                    for s in 0..spp_chunk {
                        let i = x * spp_chunk + s;
                        let v = match bps {
                            8 => row[i],
                            16 => row[i * 2 + if le { 1 } else { 0 }],
                            _ => {
                                let bit = i * bps;
                                let byte = row[bit / 8];
                                let v = (byte >> (8 - bps - bit % 8)) as u32 & maxv;
                                if photo == 3 {
                                    v as u8
                                } else {
                                    (v * 255 / maxv) as u8
                                }
                            }
                        };
                        let ch = if planar { plane } else { s };
                        samples[(oy * w + ox) * spp + ch] = v;
                    }
                }
            }
        }
    }
    let alpha = if extra.is_empty() { None } else { Some(spp - extra.len()) };
    let associated = extra.first() == Some(&1);
    let cmap = ifd.vals(320);
    let mut px = vec![0u32; w * h];
    for i in 0..w * h {
        let s = &samples[i * spp..i * spp + spp];
        let a = alpha.map(|k| s[k]).unwrap_or(255);
        let p = match photo {
            0 => {
                let v = 255 - s[0];
                argb(255, v, v, v)
            }
            1 => argb(if spp >= 2 { s[1] } else { 255 }, s[0], s[0], s[0]),
            3 => {
                let n = 1usize << bps;
                let k = s[0] as usize;
                let c = |pl: usize| (cmap.get(pl * n + k).copied().unwrap_or(0) >> 8) as u8;
                argb(255, c(0), c(1), c(2))
            }
            5 => {
                let k = 255 - s[3.min(spp - 1)] as u32;
                let f = |c: u8| ((255 - c as u32) * k / 255) as u8;
                argb(255, f(s[0]), f(s[1]), f(s[2]))
            }
            _ => {
                if spp >= 3 {
                    argb(a, s[0], s[1], s[2])
                } else {
                    argb(255, s[0], s[0], s[0])
                }
            }
        };
        px[i] = if associated && a > 0 && a < 255 {
            let un = |c: u32| ((c * 255 + a as u32 / 2) / a as u32).min(255);
            (p & 0xFF00_0000) | un((p >> 16) & 255) << 16 | un((p >> 8) & 255) << 8 | un(p & 255)
        } else {
            p
        };
    }
    image(w, h, px)
}

pub fn decode(d: &[u8]) -> Result<Image, &'static str> {
    let (le, first) = header(d).ok_or("Not a valid TIFF")?;
    let ifd = Ifd::read(d, le, first).ok_or("Damaged TIFF file")?;
    let img = decode_ifd(d, &ifd)?;
    let o = ifd.val(274).unwrap_or(1) as u8;
    Ok(super::orient(img, o))
}

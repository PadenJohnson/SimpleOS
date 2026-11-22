//! GIF: the first frame (animated GIFs show their first picture).

use super::{argb, image, BitsLsb};
use crate::image::Image;
use alloc::vec;
use alloc::vec::Vec;

/// Gather the sub-blocks of a data stream starting at `p`; returns (data, position after).
fn sub_blocks(d: &[u8], mut p: usize) -> (Vec<u8>, usize) {
    let mut out = Vec::new();
    while let Some(&n) = d.get(p) {
        p += 1;
        if n == 0 {
            break;
        }
        let end = (p + n as usize).min(d.len());
        out.extend_from_slice(&d[p..end]);
        p = end;
    }
    (out, p)
}

fn lzw(data: &[u8], min_size: u32, n_pixels: usize) -> Vec<u8> {
    let mut out = Vec::with_capacity(n_pixels);
    let clear = 1u32 << min_size;
    let eoi = clear + 1;
    let mut prefix = vec![0u16; 4096];
    let mut suffix = vec![0u8; 4096];
    let mut first = vec![0u8; 4096];
    let mut len = vec![0u16; 4096];
    for i in 0..clear as usize {
        suffix[i] = i as u8;
        first[i] = i as u8;
        len[i] = 1;
    }
    let mut size = min_size + 1;
    let mut next = eoi + 1;
    let mut old: Option<u32> = None;
    let mut b = BitsLsb { d: data, pos: 0 };
    let mut tmp = vec![0u8; 4096];
    while out.len() < n_pixels {
        let Some(code) = b.get(size) else { break };
        if code == clear {
            size = min_size + 1;
            next = eoi + 1;
            old = None;
            continue;
        }
        if code == eoi {
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
                if code >= next {
                    break;
                }
                emit(code, &mut out, &mut tmp);
            }
            Some(o) => {
                let fc;
                if code < next {
                    emit(code, &mut out, &mut tmp);
                    fc = first[code as usize];
                } else if code == next {
                    fc = first[o as usize];
                    emit(o, &mut out, &mut tmp);
                    out.push(fc);
                } else {
                    break;
                }
                if next < 4096 {
                    prefix[next as usize] = o as u16;
                    suffix[next as usize] = fc;
                    first[next as usize] = first[o as usize];
                    len[next as usize] = len[o as usize] + 1;
                    next += 1;
                    if next == (1 << size) && size < 12 {
                        size += 1;
                    }
                }
            }
        }
        old = Some(code);
    }
    out
}

pub fn decode(d: &[u8]) -> Result<Image, &'static str> {
    if d.len() < 13 || !(d.starts_with(b"GIF87a") || d.starts_with(b"GIF89a")) {
        return Err("Not a valid GIF");
    }
    let sw = u16::from_le_bytes([d[6], d[7]]) as usize;
    let sh = u16::from_le_bytes([d[8], d[9]]) as usize;
    let flags = d[10];
    let mut p = 13;
    let mut gct: Vec<[u8; 3]> = Vec::new();
    if flags & 0x80 != 0 {
        let n = 2usize << (flags & 7);
        for k in 0..n {
            let o = p + k * 3;
            gct.push([*d.get(o).unwrap_or(&0), *d.get(o + 1).unwrap_or(&0), *d.get(o + 2).unwrap_or(&0)]);
        }
        p += n * 3;
    }
    let mut transparent: Option<u8> = None;
    while p < d.len() {
        match d[p] {
            0x21 => {
                let label = *d.get(p + 1).ok_or("Damaged GIF file")?;
                if label == 0xF9 && d.get(p + 2) == Some(&4) {
                    let pf = *d.get(p + 3).unwrap_or(&0);
                    if pf & 1 != 0 {
                        transparent = d.get(p + 6).copied();
                    }
                }
                let (_, np) = sub_blocks(d, p + 2);
                p = np;
            }
            0x2C => {
                if p + 10 > d.len() {
                    return Err("Damaged GIF file");
                }
                let ix = u16::from_le_bytes([d[p + 1], d[p + 2]]) as usize;
                let iy = u16::from_le_bytes([d[p + 3], d[p + 4]]) as usize;
                let iw = u16::from_le_bytes([d[p + 5], d[p + 6]]) as usize;
                let ih = u16::from_le_bytes([d[p + 7], d[p + 8]]) as usize;
                let f = d[p + 9];
                p += 10;
                let mut lct = Vec::new();
                if f & 0x80 != 0 {
                    let n = 2usize << (f & 7);
                    for k in 0..n {
                        let o = p + k * 3;
                        lct.push([*d.get(o).unwrap_or(&0), *d.get(o + 1).unwrap_or(&0), *d.get(o + 2).unwrap_or(&0)]);
                    }
                    p += n * 3;
                }
                let pal = if lct.is_empty() { &gct } else { &lct };
                let min = (*d.get(p).ok_or("Damaged GIF file")? as u32).clamp(2, 11);
                let (data, _) = sub_blocks(d, p + 1);
                let (sw, sh) = (sw.max(ix + iw).max(1), sh.max(iy + ih).max(1));
                crate::image::check_size(sw, sh)?;
                let idx = lzw(&data, min, iw * ih);
                let mut px = vec![0u32; sw * sh];
                // Interlaced rows come in passes.
                let rows: Vec<usize> = if f & 0x40 != 0 {
                    let mut r = Vec::with_capacity(ih);
                    for (start, step) in [(0, 8), (4, 8), (2, 4), (1, 2)] {
                        let mut y = start;
                        while y < ih {
                            r.push(y);
                            y += step;
                        }
                    }
                    r
                } else {
                    (0..ih).collect()
                };
                for (k, &y) in rows.iter().enumerate() {
                    for x in 0..iw {
                        let Some(&c) = idx.get(k * iw + x) else { break };
                        if Some(c) == transparent {
                            continue;
                        }
                        let rgb = pal.get(c as usize).copied().unwrap_or([0, 0, 0]);
                        let (ox, oy) = (ix + x, iy + y);
                        if ox < sw && oy < sh {
                            px[oy * sw + ox] = argb(255, rgb[0], rgb[1], rgb[2]);
                        }
                    }
                }
                return image(sw, sh, px);
            }
            0x3B => break,
            _ => p += 1,
        }
    }
    Err("This GIF has no picture")
}

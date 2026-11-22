//! Windows BMP (also the pictures inside ICO files): 1/4/8-bit palette, 16/24/32-bit,
//! RLE4/RLE8, bit fields, top-down or bottom-up.

use super::{argb, image, Rd};
use crate::image::Image;
use alloc::vec;
use alloc::vec::Vec;

pub fn decode(d: &[u8]) -> Result<Image, &'static str> {
    if d.len() < 26 || &d[0..2] != b"BM" {
        return Err("Not a valid BMP");
    }
    let r = Rd { d, le: true };
    let off = r.u32(10).ok_or("Damaged BMP file")? as usize;
    decode_dib(&d[14..], Some(off.saturating_sub(14)), false)
}

/// A DIB (header + palette + pixels). `pixels_at`: offset of the pixels from the DIB start
/// (None: right after the palette). `ico`: height is doubled and an AND mask follows.
pub fn decode_dib(d: &[u8], pixels_at: Option<usize>, ico: bool) -> Result<Image, &'static str> {
    let r = Rd { d, le: true };
    let err = "Damaged BMP file";
    let hs = r.u32(0).ok_or(err)? as usize;
    let (w, mut h, bpp, comp, mut ncolors);
    let core = hs == 12;
    if core {
        w = r.u16(4).ok_or(err)? as i32;
        h = r.u16(6).ok_or(err)? as i16 as i32;
        bpp = r.u16(10).ok_or(err)? as u32;
        comp = 0;
        ncolors = 0;
    } else {
        w = r.i32(4).ok_or(err)?;
        h = r.i32(8).ok_or(err)?;
        bpp = r.u16(14).ok_or(err)? as u32;
        comp = r.u32(16).ok_or(err)?;
        ncolors = r.u32(32).unwrap_or(0) as usize;
    }
    if ico {
        h /= 2;
    }
    let top_down = h < 0;
    let (w, h) = (w.unsigned_abs() as usize, h.unsigned_abs() as usize);
    crate::image::check_size(w, h)?;
    // Embedded JPEG / PNG.
    if comp == 4 || comp == 5 {
        let at = pixels_at.unwrap_or(hs);
        return crate::image::decode(d.get(at..).ok_or(err)?);
    }
    // Bit fields.
    let mut masks: Option<[u32; 4]> = None;
    let mut pal_at = hs;
    if comp == 3 || comp == 6 {
        let mut m = [r.u32(40).unwrap_or(0), r.u32(44).unwrap_or(0), r.u32(48).unwrap_or(0), 0];
        if comp == 6 || hs >= 56 {
            m[3] = r.u32(52).unwrap_or(0);
        }
        if hs == 40 {
            pal_at = 40 + if comp == 6 { 16 } else { 12 };
        }
        masks = Some(m);
    } else if hs >= 56 && bpp == 32 {
        // V4/V5 header with BI_RGB: no masks used.
    }
    let ps = if core { 3 } else { 4 };
    if bpp <= 8 && ncolors == 0 {
        ncolors = 1 << bpp;
    }
    let mut pal = Vec::new();
    for k in 0..ncolors.min(256) {
        let o = pal_at + k * ps;
        let (b, g, rr) = (r.u8(o).unwrap_or(0), r.u8(o + 1).unwrap_or(0), r.u8(o + 2).unwrap_or(0));
        pal.push(argb(255, rr, g, b));
    }
    let px_at = pixels_at.unwrap_or(pal_at + ncolors.min(256) * ps);
    let data = d.get(px_at..).unwrap_or(&[]);
    let mut px = vec![0xFF00_0000u32; w * h];
    let color = |i: usize| pal.get(i).copied().unwrap_or(0xFF00_0000);
    let row_of = |y: usize| if top_down { y } else { h - 1 - y };
    match comp {
        1 | 2 => {
            // RLE8 / RLE4, rows from the bottom.
            let (mut x, mut y) = (0usize, 0usize);
            let mut p = 0;
            let set = |px: &mut Vec<u32>, x: usize, y: usize, c: u32| {
                if x < w && y < h {
                    px[row_of(y) * w + x] = c;
                }
            };
            while p + 1 < data.len() {
                let (n, v) = (data[p] as usize, data[p + 1]);
                p += 2;
                if n > 0 {
                    for k in 0..n {
                        let idx = if comp == 1 { v } else if k & 1 == 0 { v >> 4 } else { v & 15 };
                        set(&mut px, x, y, color(idx as usize));
                        x += 1;
                    }
                } else {
                    match v {
                        0 => {
                            x = 0;
                            y += 1;
                        }
                        1 => break,
                        2 => {
                            x += *data.get(p).unwrap_or(&0) as usize;
                            y += *data.get(p + 1).unwrap_or(&0) as usize;
                            p += 2;
                        }
                        cnt => {
                            let cnt = cnt as usize;
                            let bytes = if comp == 1 { cnt } else { cnt.div_ceil(2) };
                            for k in 0..cnt {
                                let b = *data.get(p + if comp == 1 { k } else { k / 2 }).unwrap_or(&0);
                                let idx = if comp == 1 { b } else if k & 1 == 0 { b >> 4 } else { b & 15 };
                                set(&mut px, x, y, color(idx as usize));
                                x += 1;
                            }
                            p += bytes + (bytes & 1);
                        }
                    }
                }
            }
        }
        _ => {
            let stride = (w * bpp as usize).div_ceil(32) * 4;
            for y in 0..h {
                let row = data.get(y * stride..y * stride + stride).unwrap_or(&[]);
                let out = row_of(y) * w;
                for x in 0..w {
                    let c = match bpp {
                        1 | 2 | 4 | 8 => {
                            let bit = x * bpp as usize;
                            let b = *row.get(bit / 8).unwrap_or(&0);
                            let idx = (b >> (8 - bpp as usize - (bit % 8))) & ((1 << bpp) - 1) as u8;
                            color(idx as usize)
                        }
                        16 | 32 => {
                            let v = if bpp == 16 {
                                u16::from_le_bytes([*row.get(x * 2).unwrap_or(&0), *row.get(x * 2 + 1).unwrap_or(&0)]) as u32
                            } else {
                                u32::from_le_bytes([*row.get(x * 4).unwrap_or(&0), *row.get(x * 4 + 1).unwrap_or(&0), *row.get(x * 4 + 2).unwrap_or(&0), *row.get(x * 4 + 3).unwrap_or(&0)])
                            };
                            let m = masks.unwrap_or(if bpp == 16 { [0x7C00, 0x03E0, 0x001F, 0] } else { [0xFF_0000, 0xFF00, 0xFF, 0] });
                            let ch = |mask: u32| -> u8 {
                                if mask == 0 {
                                    return 0;
                                }
                                let shift = mask.trailing_zeros();
                                let bits = (mask >> shift).count_ones();
                                let v = (v & mask) >> shift;
                                if bits >= 8 {
                                    (v >> (bits - 8)) as u8
                                } else {
                                    ((v * 255 + ((1 << bits) - 1) / 2) / ((1 << bits) - 1)) as u8
                                }
                            };
                            let a = if m[3] != 0 { ch(m[3]) } else { 255 };
                            argb(a, ch(m[0]), ch(m[1]), ch(m[2]))
                        }
                        24 => argb(255, *row.get(x * 3 + 2).unwrap_or(&0), *row.get(x * 3 + 1).unwrap_or(&0), *row.get(x * 3).unwrap_or(&0)),
                        _ => return Err("This BMP's colour depth isn't supported"),
                    };
                    px[out + x] = c;
                }
            }
            if ico {
                // 32-bit icons carry alpha in the pixels; others use the AND mask.
                if bpp == 32 {
                    let stride = w * 4;
                    let any_alpha = (0..h).any(|y| (0..w).any(|x| data.get(y * stride + x * 4 + 3).copied().unwrap_or(0) != 0));
                    if any_alpha {
                        for y in 0..h {
                            for x in 0..w {
                                let a = data.get(y * stride + x * 4 + 3).copied().unwrap_or(255);
                                let o = row_of(y) * w + x;
                                px[o] = (px[o] & 0xFF_FFFF) | (a as u32) << 24;
                            }
                        }
                    }
                } else {
                    let mstride = w.div_ceil(32) * 4;
                    let mask_at = stride * h;
                    for y in 0..h {
                        for x in 0..w {
                            let b = data.get(mask_at + y * mstride + x / 8).copied().unwrap_or(0);
                            if b & (0x80 >> (x % 8)) != 0 {
                                px[row_of(y) * w + x] = 0;
                            }
                        }
                    }
                }
            }
        }
    }
    image(w, h, px)
}

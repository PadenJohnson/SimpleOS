//! WebP: lossy (a VP8 key frame, using the video decoder), lossless (VP8L), alpha, and the
//! first frame of animated WebPs.

use super::{argb, image, Rd};
use crate::image::Image;
use crate::media::dec::{Pic, VideoDec};
use alloc::vec;
use alloc::vec::Vec;

struct Chunk<'a> {
    id: [u8; 4],
    data: &'a [u8],
}

fn chunks(d: &[u8]) -> Vec<Chunk<'_>> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= d.len() {
        let id = [d[p], d[p + 1], d[p + 2], d[p + 3]];
        let n = u32::from_le_bytes([d[p + 4], d[p + 5], d[p + 6], d[p + 7]]) as usize;
        let s = p + 8;
        let e = (s + n).min(d.len());
        out.push(Chunk { id, data: &d[s..e] });
        p = s + n + (n & 1);
    }
    out
}

/// Lossy picture (VP8 key frame) to ARGB, libwebp-style YUV->RGB with smooth chroma.
fn lossy(vp8: &[u8]) -> Result<(usize, usize, Vec<u32>), &'static str> {
    let mut dec = crate::media::vp8::Vp8Dec::new();
    dec.decode(vp8);
    let Some(Pic::Yuv(f)) = dec.pop() else { return Err("Damaged WebP file") };
    let (w, h) = (f.width, f.height);
    crate::image::check_size(w, h)?;
    let cs = f.stride / 2;
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    // Chroma at full resolution: libwebp's "fancy" 9-3-3-1 upsampling.
    let up = |plane: &[u8]| -> Vec<u8> {
        let at = |x: isize, y: isize| plane[(y.clamp(0, ch as isize - 1) as usize) * cs + x.clamp(0, cw as isize - 1) as usize] as i32;
        let mut out = vec![0u8; w * h];
        for y in 0..h {
            let cy = (y as isize - 1) >> 1;
            let fy = if y & 1 == 1 { 0 } else { 1 };
            // Rows: the nearer chroma row weighs 3, the farther 1.
            let (near, far) = if fy == 0 { (cy, cy + 1) } else { (cy + 1, cy) };
            for x in 0..w {
                let cx = (x as isize - 1) >> 1;
                let (nx, fx) = if x & 1 == 1 { (cx, cx + 1) } else { (cx + 1, cx) };
                let v = 9 * at(nx, near) + 3 * at(fx, near) + 3 * at(nx, far) + at(fx, far);
                out[y * w + x] = ((v + 8) >> 4) as u8;
            }
        }
        out
    };
    let (u, v) = (up(&f.u), up(&f.v));
    let mut px = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let yy = f.y[y * f.stride + x] as i32;
            let (uu, vv) = (u[y * w + x] as i32, v[y * w + x] as i32);
            // libwebp: 14-bit fixed point BT.601 limited range.
            let c = |a: i32| ((a >> 6).clamp(0, 255)) as u8;
            let yv = (yy * 19077) >> 8;
            let r = c(yv + ((vv * 26149) >> 8) - 14234);
            let g = c(yv - ((uu * 6419) >> 8) - ((vv * 13320) >> 8) + 8708);
            let b = c(yv + ((uu * 33050) >> 8) - 17685);
            px[y * w + x] = argb(255, r, g, b);
        }
    }
    Ok((w, h, px))
}

fn apply_alpha(alph: &[u8], w: usize, h: usize, px: &mut [u32]) {
    let Some(&hdr) = alph.first() else { return };
    let comp = hdr & 3;
    let filter = (hdr >> 2) & 3;
    let data = &alph[1..];
    let mut a = match comp {
        0 => {
            let mut v = data.to_vec();
            v.resize(w * h, 255);
            v
        }
        1 => match super::vp8l::decode_alpha(data, w, h) {
            Ok(v) => v,
            Err(_) => return,
        },
        _ => return,
    };
    if filter != 0 {
        for x in 1..w {
            a[x] = a[x].wrapping_add(a[x - 1]);
        }
        for y in 1..h {
            a[y * w] = a[y * w].wrapping_add(a[(y - 1) * w]);
        }
        for y in 1..h {
            for x in 1..w {
                let i = y * w + x;
                let p = match filter {
                    1 => a[i - 1],
                    2 => a[i - w],
                    _ => (a[i - 1] as i32 + a[i - w] as i32 - a[i - w - 1] as i32).clamp(0, 255) as u8,
                };
                a[i] = a[i].wrapping_add(p);
            }
        }
    }
    for (p, &al) in px.iter_mut().zip(a.iter()) {
        *p = (*p & 0xFF_FFFF) | (al as u32) << 24;
    }
}

/// One picture from a list of chunks (ALPH + VP8, or VP8L).
fn picture(cs: &[Chunk]) -> Result<(usize, usize, Vec<u32>), &'static str> {
    if let Some(c) = cs.iter().find(|c| &c.id == b"VP8L") {
        return super::vp8l::decode(c.data);
    }
    let vp8 = cs.iter().find(|c| &c.id == b"VP8 ").ok_or("This WebP has no picture")?;
    let (w, h, mut px) = lossy(vp8.data)?;
    if let Some(a) = cs.iter().find(|c| &c.id == b"ALPH") {
        apply_alpha(a.data, w, h, &mut px);
    }
    Ok((w, h, px))
}

pub fn decode(d: &[u8]) -> Result<Image, &'static str> {
    if d.len() < 16 || &d[0..4] != b"RIFF" || &d[8..12] != b"WEBP" {
        return Err("Not a valid WebP");
    }
    let cs = chunks(&d[12..]);
    let mut orientation = 1;
    if let Some(e) = cs.iter().find(|c| &c.id == b"EXIF") {
        let t = if e.data.starts_with(b"Exif\0\0") { &e.data[6..] } else { e.data };
        orientation = super::tiff_orientation(t).unwrap_or(1);
    }
    let img = if let Some(f) = cs.iter().find(|c| &c.id == b"ANMF") {
        // Animated: draw the first frame on the canvas.
        let vp8x = cs.iter().find(|c| &c.id == b"VP8X").ok_or("Damaged WebP file")?;
        let r = Rd { d: vp8x.data, le: true };
        let u24 = |r: &Rd, o: usize| -> usize { r.u8(o).unwrap_or(0) as usize | (r.u8(o + 1).unwrap_or(0) as usize) << 8 | (r.u8(o + 2).unwrap_or(0) as usize) << 16 };
        let (cw, ch) = (u24(&r, 4) + 1, u24(&r, 7) + 1);
        crate::image::check_size(cw, ch)?;
        let fr = Rd { d: f.data, le: true };
        let (fx, fy) = (u24(&fr, 0) * 2, u24(&fr, 3) * 2);
        let inner = chunks(f.data.get(16..).unwrap_or(&[]));
        let (w, h, px) = picture(&inner)?;
        let mut canvas = vec![0u32; cw * ch];
        for y in 0..h {
            for x in 0..w {
                if fx + x < cw && fy + y < ch {
                    canvas[(fy + y) * cw + fx + x] = px[y * w + x];
                }
            }
        }
        image(cw, ch, canvas)?
    } else {
        let (w, h, px) = picture(&cs)?;
        image(w, h, px)?
    };
    Ok(super::orient(img, orientation))
}

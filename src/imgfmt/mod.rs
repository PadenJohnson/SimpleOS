//! Photo file formats beyond JPEG and PNG: GIF, BMP, TIFF, WebP, HEIC/HEIF, camera RAW
//! (via the full-size preview the camera stores in it), Photoshop PSD, TGA, ICO and PNM.

pub mod bmp;
pub mod gif;
pub mod heif;
pub mod misc;
pub mod raw;
pub mod tiff;
pub mod vp8l;
pub mod webp;

use crate::image::Image;
use alloc::vec::Vec;

/// Build an image from 0xAARRGGBB pixels.
pub fn image(w: usize, h: usize, px: Vec<u32>) -> Result<Image, &'static str> {
    crate::image::check_size(w, h)?;
    if px.len() < w * h {
        return Err("Damaged image data");
    }
    let opaque = px.iter().all(|&p| p >> 24 == 255);
    Ok(Image { w, h, px, opaque })
}

#[inline]
pub fn argb(a: u8, r: u8, g: u8, b: u8) -> u32 {
    (a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32
}

/// Little/big-endian readers over a byte slice (None past the end).
#[derive(Clone, Copy)]
pub struct Rd<'a> {
    pub d: &'a [u8],
    pub le: bool,
}

impl<'a> Rd<'a> {
    pub fn u8(&self, o: usize) -> Option<u8> {
        self.d.get(o).copied()
    }
    pub fn u16(&self, o: usize) -> Option<u16> {
        let s = self.d.get(o..o + 2)?;
        Some(if self.le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
    }
    pub fn u32(&self, o: usize) -> Option<u32> {
        let s = self.d.get(o..o + 4)?;
        Some(if self.le { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) })
    }
    pub fn i32(&self, o: usize) -> Option<i32> {
        self.u32(o).map(|v| v as i32)
    }
    pub fn slice(&self, o: usize, n: usize) -> Option<&'a [u8]> {
        self.d.get(o..o.checked_add(n)?)
    }
}

/// Turn/flip an image by an EXIF orientation (1..8).
pub fn orient(img: Image, o: u8) -> Image {
    if !(2..=8).contains(&o) {
        return img;
    }
    let (w, h) = (img.w, img.h);
    let swap = o >= 5;
    let (nw, nh) = if swap { (h, w) } else { (w, h) };
    let mut px = alloc::vec![0u32; w * h];
    for y in 0..nh {
        for x in 0..nw {
            // Source position for output (x, y).
            let (sx, sy) = match o {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (y, h - 1 - x),
                7 => (w - 1 - y, h - 1 - x),
                _ => (w - 1 - y, x),
            };
            px[y * nw + x] = img.px[sy * w + sx];
        }
    }
    Image { w: nw, h: nh, px, opaque: img.opaque }
}

/// The EXIF orientation stored in a TIFF structure (an Exif block after "Exif\0\0", or a TIFF file).
pub fn tiff_orientation(t: &[u8]) -> Option<u8> {
    let le = match t.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let r = Rd { d: t, le };
    let ifd = r.u32(4)? as usize;
    let n = r.u16(ifd)? as usize;
    for k in 0..n.min(500) {
        let e = ifd + 2 + k * 12;
        if r.u16(e)? == 0x0112 {
            let v = r.u16(e + 8)?;
            return (1..=8).contains(&v).then_some(v as u8);
        }
    }
    None
}

/// The orientation from a JPEG's EXIF segment (1 when there is none).
pub fn jpeg_orientation(b: &[u8]) -> u8 {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return 1;
    }
    let mut i = 2;
    while i + 4 <= b.len() && b[i] == 0xFF {
        let marker = b[i + 1];
        if marker == 0xFF {
            i += 1;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if marker == 0xDA || len < 2 {
            break;
        }
        let seg = &b[(i + 4).min(b.len())..(i + 2 + len).min(b.len())];
        if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
            return tiff_orientation(&seg[6..]).unwrap_or(1);
        }
        i += 2 + len;
    }
    1
}

/// MSB-first / LSB-first bit readers for LZW and friends.
pub struct BitsLsb<'a> {
    pub d: &'a [u8],
    pub pos: usize,
}

impl BitsLsb<'_> {
    pub fn get(&mut self, n: u32) -> Option<u32> {
        let mut v = 0u32;
        for k in 0..n {
            let b = *self.d.get(self.pos >> 3)?;
            v |= (((b >> (self.pos & 7)) & 1) as u32) << k;
            self.pos += 1;
        }
        Some(v)
    }
}

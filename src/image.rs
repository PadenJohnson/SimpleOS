//! Decoded images (every photo format Solon reads; see imgfmt/) and high-quality downscaling.

use alloc::vec;
use alloc::vec::Vec;

use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_core::result::DecodingResult;
use zune_jpeg::JpegDecoder;
use zune_png::PngDecoder;

/// Refuse images bigger than this (about 100 megapixels) so one file can't exhaust memory.
const MAX_DIM: usize = 12_000;
const MAX_PIXELS: usize = 100_000_000;

#[derive(Clone)]
pub struct Image {
    pub w: usize,
    pub h: usize,
    /// 0xAARRGGBB, row-major.
    pub px: Vec<u32>,
    /// True if every pixel has alpha 255 (lets drawing skip blending).
    pub opaque: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Format {
    Jpeg,
    Png,
    Gif,
    Bmp,
    Tiff,
    Raw,
    Webp,
    Heif,
    Psd,
    Ico,
    Pnm,
    Tga,
}

/// Every photo / picture file Solon can open (lower-case extensions).
pub const PHOTO_EXTS: [&str; 41] = [
    "jpg", "jpeg", "jpe", "jfif", "png", "apng", "gif", "bmp", "dib", "tif", "tiff", "webp", "heic", "heif", "hif", "psd", "psb", "ico", "cur", "tga", "pbm", "pgm", "ppm", "pnm", "pam", "dng", "cr2", "cr3", "crw", "nef", "nrw",
    "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "3fr", "erf",
];

/// True for a file name Solon treats as a photo.
pub fn is_photo_name(name: &str) -> bool {
    let Some(i) = name.rfind('.') else { return false };
    let ext = name[i + 1..].to_ascii_lowercase();
    PHOTO_EXTS.contains(&ext.as_str()) || crate::imgfmt::raw::RAW_EXTS.contains(&ext.as_str())
}

pub fn sniff(bytes: &[u8]) -> Option<Format> {
    let b = bytes;
    if b.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if b.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else if b.starts_with(b"GIF87a") || b.starts_with(b"GIF89a") {
        Some(Format::Gif)
    } else if b.starts_with(b"BM") && b.len() > 26 {
        Some(Format::Bmp)
    } else if b.len() > 16 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        Some(Format::Webp)
    } else if b.starts_with(b"FUJIFILMCCD-RAW") || (b.len() > 12 && &b[4..12] == b"ftypcrx ") {
        Some(Format::Raw)
    } else if crate::imgfmt::heif::is_heif(b) {
        Some(Format::Heif)
    } else if b.starts_with(b"II") || b.starts_with(b"MM") {
        let (le, first) = crate::imgfmt::tiff::header(b)?;
        let ifd = crate::imgfmt::tiff::Ifd::read(b, le, first)?;
        // Camera files: maker, sub-pictures, DNG version, or Olympus / Panasonic magic.
        let magic = crate::imgfmt::Rd { d: b, le }.u16(2)?;
        let photo = ifd.val(262).unwrap_or(2);
        let camera = magic != 42
            || b.get(8..10) == Some(b"CR")
            || ifd.entry(50706).is_some()
            || ifd.entry(330).is_some()
            || (ifd.entry(271).is_some() && (ifd.val(254) == Some(1) || !matches!(photo, 0..=3 | 5 | 6)));
        Some(if camera { Format::Raw } else { Format::Tiff })
    } else if b.starts_with(b"8BPS") {
        Some(Format::Psd)
    } else if b.len() > 6 && b[0] == 0 && b[1] == 0 && (b[2] == 1 || b[2] == 2) && b[3] == 0 && b[4] | b[5] != 0 {
        Some(Format::Ico)
    } else if b.len() > 3 && b[0] == b'P' && (b'1'..=b'7').contains(&b[1]) && b[2].is_ascii_whitespace() {
        Some(Format::Pnm)
    } else if crate::imgfmt::misc::looks_like_tga(b) {
        Some(Format::Tga)
    } else {
        None
    }
}

/// Decode any supported photo, turned upright (EXIF / HEIF orientation applied).
pub fn decode(bytes: &[u8]) -> Result<Image, &'static str> {
    use crate::imgfmt as f;
    match sniff(bytes) {
        Some(Format::Jpeg) => {
            let img = decode_jpeg_raw(bytes, false)?;
            Ok(f::orient(img, f::jpeg_orientation(bytes)))
        }
        Some(Format::Png) | None => decode_basic(bytes),
        Some(Format::Gif) => f::gif::decode(bytes),
        Some(Format::Bmp) => f::bmp::decode(bytes),
        Some(Format::Tiff) => f::tiff::decode(bytes),
        Some(Format::Raw) => f::raw::decode(bytes).or_else(|e| f::tiff::decode(bytes).map_err(|_| e)),
        Some(Format::Webp) => f::webp::decode(bytes),
        Some(Format::Heif) => f::heif::decode(bytes),
        Some(Format::Psd) => f::misc::decode_psd(bytes),
        Some(Format::Ico) => f::misc::decode_ico(bytes),
        Some(Format::Pnm) => f::misc::decode_pnm(bytes),
        Some(Format::Tga) => f::misc::decode_tga(bytes),
    }
}

/// A picture for a thumbnail grid: an embedded preview when the file has a big enough one
/// (HEIC), else the full picture. Returns (picture, full size).
/// A JPEG's or PNG's size from its header, without decoding it (None: another format, or
/// not found).
pub fn peek_size(b: &[u8]) -> Option<(usize, usize)> {
    if b.len() >= 24 && b.starts_with(&[0x89, b'P', b'N', b'G']) && &b[12..16] == b"IHDR" {
        let be = |o: usize| u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as usize;
        return Some((be(16), be(20)));
    }
    if !b.starts_with(&[0xFF, 0xD8]) {
        return None;
    }
    let mut i = 2;
    while i + 4 <= b.len() {
        if b[i] != 0xFF {
            return None;
        }
        let m = b[i + 1];
        if m == 0xFF {
            i += 1;
            continue;
        }
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        // Start of frame (any kind but DHT, JPG and DAC): precision, height, width.
        if (0xC0..=0xCF).contains(&m) && m != 0xC4 && m != 0xC8 && m != 0xCC {
            if i + 9 > b.len() {
                return None;
            }
            let h = u16::from_be_bytes([b[i + 5], b[i + 6]]) as usize;
            let w = u16::from_be_bytes([b[i + 7], b[i + 8]]) as usize;
            return Some((w, h));
        }
        if m == 0xDA || m == 0xD9 || len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

pub fn decode_preview(bytes: &[u8], min: usize) -> Result<(Image, (usize, usize)), &'static str> {
    if sniff(bytes) == Some(Format::Heif) {
        if let Some(t) = crate::imgfmt::heif::decode_thumb(bytes, min) {
            return Ok(t);
        }
    }
    let img = decode(bytes)?;
    let size = (img.w, img.h);
    Ok((img, size))
}

/// A JPEG as stored (no EXIF turning).
pub fn decode_jpeg_raw(bytes: &[u8], _rgb: bool) -> Result<Image, &'static str> {
    decode_basic(bytes)
}

fn decode_basic(bytes: &[u8]) -> Result<Image, &'static str> {
    let opts = DecoderOptions::default()
        .set_max_width(MAX_DIM)
        .set_max_height(MAX_DIM)
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .png_set_strip_to_8bit(true);
    let jpeg = bytes.starts_with(&[0xFF, 0xD8]);
    let png = bytes.starts_with(b"\x89PNG\r\n\x1a\n");
    match (jpeg, png) {
        (true, _) => {
            let mut d = JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
            d.decode_headers().map_err(|_| "Not a valid JPEG")?;
            let info = d.info().ok_or("Not a valid JPEG")?;
            let (w, h) = (info.width as usize, info.height as usize);
            check_size(w, h)?;
            let data = d.decode().map_err(|_| "Damaged JPEG file")?;
            let comps = d.output_colorspace().map(|c| c.num_components()).unwrap_or(3);
            from_bytes(w, h, &data, comps)
        }
        (_, true) => {
            let mut d = PngDecoder::new_with_options(ZCursor::new(bytes), opts);
            d.decode_headers().map_err(|_| "Not a valid PNG")?;
            let (w, h) = d.dimensions().ok_or("Not a valid PNG")?;
            check_size(w, h)?;
            let data = match d.decode().map_err(|_| "Damaged PNG file")? {
                DecodingResult::U8(v) => v,
                _ => return Err("Unsupported PNG depth"),
            };
            let comps = d.colorspace().map(|c| c.num_components()).unwrap_or(3);
            from_bytes(w, h, &data, comps)
        }
        _ => Err("Unknown file type"),
    }
}

pub fn check_size(w: usize, h: usize) -> Result<(), &'static str> {
    if w == 0 || h == 0 || w > MAX_DIM || h > MAX_DIM || w * h > MAX_PIXELS {
        Err("Image too large")
    } else {
        Ok(())
    }
}

fn from_bytes(w: usize, h: usize, data: &[u8], comps: usize) -> Result<Image, &'static str> {
    if comps == 0 || data.len() < w * h * comps {
        return Err("Damaged image data");
    }
    let mut px = Vec::with_capacity(w * h);
    let mut opaque = true;
    for p in data.chunks_exact(comps).take(w * h) {
        let (r, g, b, a) = match comps {
            1 => (p[0], p[0], p[0], 255),
            2 => (p[0], p[0], p[0], p[1]),
            3 => (p[0], p[1], p[2], 255),
            _ => (p[0], p[1], p[2], p[3]),
        };
        opaque &= a == 255;
        px.push((a as u32) << 24 | (r as u32) << 16 | (g as u32) << 8 | b as u32);
    }
    Ok(Image { w, h, px, opaque })
}

/// Size that fits (w, h) inside (max_w, max_h) keeping the aspect ratio. Never enlarges.
pub fn fit(w: usize, h: usize, max_w: usize, max_h: usize) -> (usize, usize) {
    if w <= max_w && h <= max_h {
        return (w, h);
    }
    // Compare w/max_w vs h/max_h without floats.
    if w * max_h >= h * max_w {
        (max_w, (h * max_w / w).max(1))
    } else {
        ((w * max_h / h).max(1), max_h)
    }
}

impl Image {
    /// Shrink to fit inside (max_w, max_h); returns a copy if it already fits.
    pub fn shrink_to_fit(&self, max_w: usize, max_h: usize) -> Image {
        let (dw, dh) = fit(self.w, self.h, max_w.max(1), max_h.max(1));
        if (dw, dh) == (self.w, self.h) {
            return Image { w: self.w, h: self.h, px: self.px.clone(), opaque: self.opaque };
        }
        self.downscale(dw, dh)
    }

    /// Area-average downscale: every destination pixel is the (alpha-weighted)
    /// mean of the source pixels it covers. Smooth, no aliasing.
    fn downscale(&self, dw: usize, dh: usize) -> Image {
        let mut px = vec![0u32; dw * dh];
        // Column ranges are the same for every row; compute once.
        let cols: Vec<(usize, usize)> = (0..dw)
            .map(|dx| {
                let x0 = dx * self.w / dw;
                let x1 = ((dx + 1) * self.w / dw).max(x0 + 1);
                (x0, x1)
            })
            .collect();
        let mut sums = vec![[0u64; 4]; dw];
        for dy in 0..dh {
            let y0 = dy * self.h / dh;
            let y1 = ((dy + 1) * self.h / dh).max(y0 + 1);
            sums.iter_mut().for_each(|s| *s = [0; 4]);
            for sy in y0..y1 {
                let row = &self.px[sy * self.w..(sy + 1) * self.w];
                for (s, &(x0, x1)) in sums.iter_mut().zip(&cols) {
                    for &p in &row[x0..x1] {
                        let a = (p >> 24) as u64;
                        s[0] += ((p >> 16) & 0xFF) as u64 * a;
                        s[1] += ((p >> 8) & 0xFF) as u64 * a;
                        s[2] += (p & 0xFF) as u64 * a;
                        s[3] += a;
                    }
                }
            }
            for (dx, (s, &(x0, x1))) in sums.iter().zip(&cols).enumerate() {
                let n = ((x1 - x0) * (y1 - y0)) as u64;
                let out = if s[3] == 0 {
                    0
                } else {
                    let a = s[3] / n;
                    let (r, g, b) = (s[0] / s[3], s[1] / s[3], s[2] / s[3]);
                    (a << 24 | r << 16 | g << 8 | b) as u32
                };
                px[dy * dw + dx] = out;
            }
        }
        let opaque = self.opaque;
        Image { w: dw, h: dh, px, opaque }
    }
}

/// Encode as a PNG (8-bit RGB, or RGBA if any pixel isn't opaque).
pub fn encode_png(img: &Image) -> Vec<u8> {
    let alpha = !img.opaque && img.px.iter().any(|p| p >> 24 != 255);
    let bpp = if alpha { 4 } else { 3 };
    let mut raw = Vec::with_capacity((img.w * bpp + 1) * img.h);
    for y in 0..img.h {
        // Filter "Up" for rows after the first (compresses pictures well, costs nothing).
        let filt = if y == 0 { 0 } else { 2 };
        raw.push(filt);
        for x in 0..img.w {
            let p = img.px[y * img.w + x];
            let px = [(p >> 16) as u8, (p >> 8) as u8, p as u8, (p >> 24) as u8];
            if filt == 2 {
                let q = img.px[(y - 1) * img.w + x];
                let up = [(q >> 16) as u8, (q >> 8) as u8, q as u8, (q >> 24) as u8];
                for k in 0..bpp {
                    raw.push(px[k].wrapping_sub(up[k]));
                }
            } else {
                raw.extend_from_slice(&px[..bpp]);
            }
        }
    }
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let mut out = Vec::from(&b"\x89PNG\r\n\x1a\n"[..]);
    let chunk = |out: &mut Vec<u8>, t: &[u8], d: &[u8]| {
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        let start = out.len();
        out.extend_from_slice(t);
        out.extend_from_slice(d);
        let mut crc = 0xFFFF_FFFFu32;
        for &b in &out[start..] {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(img.w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(img.h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, if alpha { 6 } else { 2 }, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    out
}

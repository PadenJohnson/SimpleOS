//! Decoded images (JPEG / PNG) and high-quality downscaling.

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
}

pub fn sniff(bytes: &[u8]) -> Option<Format> {
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        Some(Format::Png)
    } else {
        None
    }
}

pub fn decode(bytes: &[u8]) -> Result<Image, &'static str> {
    let opts = DecoderOptions::default()
        .set_max_width(MAX_DIM)
        .set_max_height(MAX_DIM)
        .jpeg_set_out_colorspace(ColorSpace::RGB)
        .png_set_strip_to_8bit(true);
    match sniff(bytes) {
        Some(Format::Jpeg) => {
            let mut d = JpegDecoder::new_with_options(ZCursor::new(bytes), opts);
            d.decode_headers().map_err(|_| "Not a valid JPEG")?;
            let info = d.info().ok_or("Not a valid JPEG")?;
            let (w, h) = (info.width as usize, info.height as usize);
            check_size(w, h)?;
            let data = d.decode().map_err(|_| "Damaged JPEG file")?;
            let comps = d.output_colorspace().map(|c| c.num_components()).unwrap_or(3);
            from_bytes(w, h, &data, comps)
        }
        Some(Format::Png) => {
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
        None => Err("Unknown file type"),
    }
}

fn check_size(w: usize, h: usize) -> Result<(), &'static str> {
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

//! Camera RAW files (Canon CR2/CR3, Nikon NEF, Sony ARW, Fujifilm RAF, Olympus ORF,
//! Panasonic RW2, Pentax PEF, Samsung SRW, Adobe DNG, ...): Solon shows the full-size JPEG
//! preview every camera stores inside the file (developing the raw sensor data itself isn't
//! supported).

use crate::image::Image;
use alloc::vec::Vec;

pub const RAW_EXTS: [&str; 20] = ["dng", "cr2", "cr3", "crw", "nef", "nrw", "arw", "srf", "sr2", "raf", "orf", "rw2", "pef", "srw", "3fr", "erf", "kdc", "mrw", "x3f", "iiq"];

/// Size (w, h) from a JPEG's frame header, if it's a baseline/progressive JPEG.
fn jpeg_dims(d: &[u8]) -> Option<(usize, usize)> {
    let mut i = 2;
    while i + 9 < d.len() {
        if d[i] != 0xFF {
            return None;
        }
        let m = d[i + 1];
        if m == 0xFF {
            i += 1;
            continue;
        }
        if (0xD0..=0xD7).contains(&m) || m == 0x01 {
            i += 2;
            continue;
        }
        let len = u16::from_be_bytes([d[i + 2], d[i + 3]]) as usize;
        match m {
            0xC0 | 0xC1 | 0xC2 => {
                let h = u16::from_be_bytes([d[i + 5], d[i + 6]]) as usize;
                let w = u16::from_be_bytes([d[i + 7], d[i + 8]]) as usize;
                return Some((w, h));
            }
            0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF | 0xDA => return None,
            _ => {}
        }
        if len < 2 {
            return None;
        }
        i += 2 + len;
    }
    None
}

/// All embedded JPEGs: (offset, w, h), biggest first.
fn embedded_jpegs(d: &[u8]) -> Vec<(usize, usize, usize)> {
    let mut found = Vec::new();
    let mut i = 0;
    while i + 4 < d.len() {
        if d[i] == 0xFF && d[i + 1] == 0xD8 && d[i + 2] == 0xFF && matches!(d[i + 3], 0xDB | 0xC4 | 0xE0..=0xEF | 0xC0 | 0xFE) {
            if let Some((w, h)) = jpeg_dims(&d[i..]) {
                if w >= 64 && h >= 64 {
                    found.push((i, w, h));
                }
            }
        }
        i += 1;
    }
    found.sort_by_key(|&(_, w, h)| core::cmp::Reverse(w * h));
    found
}

/// Orientation of the RAW (from its own TIFF structure, when it has one).
fn raw_orientation(d: &[u8]) -> u8 {
    if let Some(o) = super::tiff_orientation(d) {
        return o;
    }
    1
}

pub fn decode(d: &[u8]) -> Result<Image, &'static str> {
    let cands = embedded_jpegs(d);
    for (off, _, _) in cands.iter().take(4) {
        let j = &d[*off..];
        if let Ok(img) = crate::image::decode_jpeg_raw(j, false) {
            // The preview's own EXIF wins; else the RAW's.
            let o = match super::jpeg_orientation(j) {
                1 => raw_orientation(d),
                o => o,
            };
            return Ok(super::orient(img, o));
        }
    }
    Err("This RAW photo has no preview Solon can show")
}

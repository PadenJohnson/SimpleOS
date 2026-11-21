//! Encodes a test page with each of Solon's printer languages (PCL XL, PostScript, URF) and
//! decodes the result with separate, simple decoders, checking every pixel survives.

extern crate alloc;

#[path = "../../../src/image.rs"]
#[allow(dead_code)]
mod image;
#[path = "../../../src/print.rs"]
#[allow(dead_code)]
mod print;
#[path = "../../../src/printlang.rs"]
mod printlang;

use print::{Layout, Page, Paper};

fn unpackbits(d: &[u8], out: &mut Vec<u8>, eod: bool) -> usize {
    let mut i = 0;
    while i < d.len() {
        let c = d[i];
        i += 1;
        if c == 128 {
            if eod {
                return i;
            }
            continue;
        }
        if c < 128 {
            let n = c as usize + 1;
            out.extend_from_slice(&d[i..i + n]);
            i += n;
        } else {
            let n = 257 - c as usize;
            out.extend(std::iter::repeat_n(d[i], n));
            i += 1;
        }
    }
    i
}

fn test_page(w: usize, h: usize) -> Page {
    let mut p = Page::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let v = if (x / 7 + y / 5) % 3 == 0 { 0xFF_FFFF } else { ((x * 255 / w) as u32) << 16 | ((y * 255 / h) as u32) << 8 | (((x ^ y) & 255) as u32) };
            p.px[y * w + x] = v;
        }
    }
    p
}

fn gray(p: u32) -> u8 {
    let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
    ((77 * r + 150 * g + 29 * b) >> 8) as u8
}

fn check(name: &str, page: &Page, color: bool, got: &[u8], stride: usize, ox: usize, oy: usize) -> bool {
    let bpp = if color { 3 } else { 1 };
    let mut bad = 0;
    for y in 0..page.h {
        for x in 0..page.w {
            let p = page.px[y * page.w + x];
            let want: Vec<u8> = if color { vec![(p >> 16) as u8, (p >> 8) as u8, p as u8] } else { vec![gray(p)] };
            let at = (y + oy) * stride + (x + ox) * bpp;
            if got.get(at..at + bpp) != Some(&want[..]) {
                bad += 1;
            }
        }
    }
    println!("{}: {} ({} bad pixels of {})", name, if bad == 0 { "OK" } else { "FAIL" }, bad, page.w * page.h);
    bad == 0
}

fn decode_pclxl(data: &[u8]) -> (usize, usize, bool, Vec<u8>, u32) {
    let start = data.windows(13).position(|w| w == b") HP-PCL XL;2").unwrap();
    let mut i = start + data[start..].iter().position(|&c| c == b'\n').unwrap() + 1;
    let mut vals: Vec<u32> = Vec::new();
    let mut attrs: std::collections::HashMap<u8, Vec<u32>> = Default::default();
    let (mut w, mut h, mut color, mut stride) = (0usize, 0usize, false, 0usize);
    let mut img = Vec::new();
    let mut pending: Option<(usize, usize)> = None;
    let mut copies = 0;
    while i < data.len() {
        let t = data[i];
        i += 1;
        match t {
            0xC0 => {
                vals = vec![data[i] as u32];
                i += 1;
            }
            0xC1 => {
                vals = vec![u16::from_le_bytes([data[i], data[i + 1]]) as u32];
                i += 2;
            }
            0xD1 => {
                vals = vec![u16::from_le_bytes([data[i], data[i + 1]]) as u32, u16::from_le_bytes([data[i + 2], data[i + 3]]) as u32];
                i += 4;
            }
            0xF8 => {
                attrs.insert(data[i], vals.clone());
                i += 1;
            }
            0xFA => {
                let n = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
                i += 4;
                let (sl, bh) = pending.take().expect("data without ReadImage");
                let mut rows = Vec::new();
                unpackbits(&data[i..i + n], &mut rows, false);
                assert_eq!(rows.len(), stride * bh, "block size");
                img[sl * stride..(sl + bh) * stride].copy_from_slice(&rows);
                i += n;
            }
            0x1B => break,
            0x6A => color = attrs[&3][0] == 2,
            0xB0 => {
                w = attrs[&108][0] as usize;
                h = attrs[&107][0] as usize;
                stride = (w * if color { 3 } else { 1 }).div_ceil(4) * 4;
                img = vec![0; stride * h];
            }
            0xB1 => pending = Some((attrs[&109][0] as usize, attrs[&99][0] as usize)),
            0x44 => copies = attrs[&49][0],
            _ => {}
        }
    }
    (w, h, color, img, copies)
}

fn decode_ps(data: &[u8]) -> (usize, usize, Vec<u8>) {
    let s = String::from_utf8_lossy(data);
    let num = |k: &str| -> usize {
        let i = s.find(k).unwrap() + k.len();
        s[i..].trim_start().split(' ').next().unwrap().parse().unwrap()
    };
    let (w, h) = (num("/Width"), num("/Height"));
    let at = data.windows(6).position(|x| x == b"image\n").unwrap() + 6;
    let end = at + data[at..].windows(2).position(|x| x == b"~>").unwrap();
    let mut bytes = Vec::new();
    let mut group = Vec::new();
    for &c in &data[at..end] {
        match c {
            b'z' => bytes.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                group.push((c - b'!') as u32);
                if group.len() == 5 {
                    let v = group.iter().fold(0u32, |a, &d| a.wrapping_mul(85).wrapping_add(d));
                    bytes.extend_from_slice(&v.to_be_bytes());
                    group.clear();
                }
            }
            _ => {}
        }
    }
    if group.len() > 1 {
        let n = group.len();
        while group.len() < 5 {
            group.push(84);
        }
        let v = group.iter().fold(0u32, |a, &d| a.wrapping_mul(85).wrapping_add(d));
        bytes.extend_from_slice(&v.to_be_bytes()[..n - 1]);
    }
    let mut out = Vec::new();
    unpackbits(&bytes, &mut out, true);
    (w, h, out)
}

fn decode_urf(data: &[u8]) -> (usize, usize, usize, Vec<u8>) {
    assert_eq!(&data[..8], b"UNIRAST\0");
    assert_eq!(u32::from_be_bytes(data[8..12].try_into().unwrap()), 1);
    let h0 = &data[12..44];
    let bpp = h0[0] as usize / 8;
    let w = u32::from_be_bytes(h0[12..16].try_into().unwrap()) as usize;
    let h = u32::from_be_bytes(h0[16..20].try_into().unwrap()) as usize;
    let mut i = 44;
    let mut out = Vec::with_capacity(w * h * bpp);
    let mut y = 0;
    while y < h {
        let rep = data[i] as usize + 1;
        i += 1;
        let mut line = Vec::with_capacity(w * bpp);
        while line.len() < w * bpp {
            let c = data[i];
            i += 1;
            if c == 128 {
                line.resize(w * bpp, 255);
            } else if c < 128 {
                let px = &data[i..i + bpp];
                for _ in 0..=c {
                    line.extend_from_slice(px);
                }
                i += bpp;
            } else {
                let n = 257 - c as usize;
                line.extend_from_slice(&data[i..i + n * bpp]);
                i += n * bpp;
            }
        }
        for _ in 0..rep {
            out.extend_from_slice(&line);
        }
        y += rep;
    }
    (w, h, bpp, out)
}

fn png(path: &str, w: usize, h: usize, bpp: usize, stride: usize, px: &[u8]) {
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let p = &px[y * stride + x * bpp..y * stride + x * bpp + bpp];
            if bpp == 3 {
                raw.extend_from_slice(p);
            } else {
                raw.extend_from_slice(&[p[0], p[0], p[0]]);
            }
        }
    }
    let z = miniz_oxide::deflate::compress_to_vec_zlib(&raw, 6);
    let mut out = b"\x89PNG\r\n\x1a\n".to_vec();
    let chunk = |out: &mut Vec<u8>, t: &[u8], d: &[u8]| {
        out.extend_from_slice(&(d.len() as u32).to_be_bytes());
        let mut c = t.to_vec();
        c.extend_from_slice(d);
        out.extend_from_slice(&c);
        let mut crc = 0xFFFF_FFFFu32;
        for &b in &c {
            crc ^= b as u32;
            for _ in 0..8 {
                crc = if crc & 1 != 0 { 0xEDB8_8320 ^ (crc >> 1) } else { crc >> 1 };
            }
        }
        out.extend_from_slice(&(!crc).to_be_bytes());
    };
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&(w as u32).to_be_bytes());
    ihdr.extend_from_slice(&(h as u32).to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    chunk(&mut out, b"IHDR", &ihdr);
    chunk(&mut out, b"IDAT", &z);
    chunk(&mut out, b"IEND", &[]);
    std::fs::write(path, out).unwrap();
}

/// Decode a captured print job (PCL XL, PostScript, or URF inside IPP) to a PNG.
fn decode_file(input: &str, output: &str) {
    let data = std::fs::read(input).unwrap();
    let find = |n: &[u8]| data.windows(n.len()).position(|w| w == n);
    if let Some(at) = find(b") HP-PCL XL;2") {
        let (w, h, c, img, copies) = decode_pclxl(&data[at..]);
        println!("PCL XL page {}x{} {} copies {}", w, h, if c { "colour" } else { "grey" }, copies);
        let bpp = if c { 3 } else { 1 };
        png(output, w, h, bpp, (w * bpp).div_ceil(4) * 4, &img);
    } else if let Some(at) = find(b"%!PS-Adobe") {
        let (w, h, img) = decode_ps(&data[at..]);
        let bpp = img.len() / (w * h).max(1);
        println!("PostScript page {}x{} {} bytes/pixel", w, h, bpp);
        png(output, w, h, bpp, w * bpp, &img);
    } else if let Some(at) = find(b"UNIRAST\0") {
        let (w, h, bpp, img) = decode_urf(&data[at..]);
        println!("URF page {}x{} {} bytes/pixel", w, h, bpp);
        png(output, w, h, bpp, w * bpp, &img);
    } else {
        println!("no PCL XL, PostScript or URF job found");
    }
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() == 4 && args[1] == "decode" {
        decode_file(&args[2], &args[3]);
        return;
    }
    let dpi = 100;
    let (pw, ph) = print::page_pixels(Paper::Letter, dpi);
    let page = test_page(pw, ph);
    let mut ok = true;
    for color in [true, false] {
        let layout = Layout { paper: Paper::Letter, per_page: 1, fill: false, color, copies: 2 };
        let tag = if color { "colour" } else { "grey" };
        let mut j = printlang::PclXlJob::start(&layout, dpi);
        j.page(&page, color, dpi);
        let data = j.finish();
        let (w, h, c, img, copies) = decode_pclxl(&data);
        assert_eq!((w, h, c, copies), (page.w, page.h, color, 2));
        ok &= check(&format!("PCL XL {} ({} KB)", tag, data.len() / 1024), &page, color, &img, (page.w * if color { 3 } else { 1 }).div_ceil(4) * 4, 0, 0);

        let mut j = printlang::PsJob::start(&layout);
        j.page(&page, color, dpi);
        let data = j.finish();
        let (w, h, img) = decode_ps(&data);
        assert_eq!((w, h), (page.w, page.h));
        ok &= check(&format!("PostScript {} ({} KB)", tag, data.len() / 1024), &page, color, &img, page.w * if color { 3 } else { 1 }, 0, 0);

        let mut j = printlang::UrfJob::start();
        j.page(&page, &layout, dpi);
        let data = j.finish();
        let (w, h, bpp, img) = decode_urf(&data);
        let (fw, fh) = (8500 * dpi as usize / 1000, 11000 * dpi as usize / 1000);
        assert_eq!((w, h, bpp), (fw, fh, if color { 3 } else { 1 }));
        ok &= check(&format!("URF {} ({} KB)", tag, data.len() / 1024), &page, color, &img, w * bpp, (fw - page.w) / 2, (fh - page.h) / 2);
    }
    println!("{}", if ok { "ALL OK" } else { "SOME FAILED" });
}

/// Stand-ins for the kernel modules print.rs touches when laying out photos (not used here).
mod edit {
    pub fn orient(img: &crate::image::Image, _o: u32, _f: bool) -> crate::image::Image {
        crate::image::Image { w: img.w, h: img.h, px: img.px.clone(), opaque: img.opaque }
    }
}
mod gfx {
    pub fn mix(a: u32, b: u32, t: u32) -> u32 {
        let ch = |s: u32| ((((a >> s) & 0xFF) * (255 - t) + ((b >> s) & 0xFF) * t) / 255) << s;
        ch(16) | ch(8) | ch(0)
    }
}

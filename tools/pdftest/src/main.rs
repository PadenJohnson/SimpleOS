//! Host test for Solon's PDF reader: renders every page of the given PDFs to PNG files.
//! Usage: pdftest <out-folder> <scale (pixels per point)> <file.pdf>...

extern crate alloc;

macro_rules! kprintln {
    ($($t:tt)*) => { println!($($t)*) };
}

#[path = "../../../src/raster.rs"]
#[allow(dead_code)]
mod raster;
#[path = "../../../src/font.rs"]
#[allow(dead_code)]
mod font;
#[path = "../../../src/gfx.rs"]
#[allow(dead_code)]
mod gfx;
#[path = "../../../src/image.rs"]
#[allow(dead_code)]
mod image;
#[path = "../../../src/pdf/mod.rs"]
#[allow(dead_code)]
mod pdf;

use std::fs;
use std::path::Path;
use std::time::Instant;

fn png(path: &Path, w: usize, h: usize, px: &[u32]) {
    let mut raw = Vec::with_capacity((w * 3 + 1) * h);
    for y in 0..h {
        raw.push(0);
        for x in 0..w {
            let p = px[y * w + x];
            raw.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, p as u8]);
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
    fs::write(path, out).unwrap();
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 4 {
        eprintln!("usage: pdftest <out-folder> <scale> <file.pdf>...");
        std::process::exit(2);
    }
    println!("crypto self-test: {}", pdf::crypt::selftest());
    if args[1] == "edit" {
        edit_test(Path::new(&args[2]), &args[3], &args[4]);
        return;
    }
    let out = Path::new(&args[1]);
    fs::create_dir_all(out).unwrap();
    let scale: f32 = args[2].parse().unwrap_or(1.0);
    for f in &args[3..] {
        let data = fs::read(f).expect("read");
        let stem = Path::new(f).file_stem().unwrap().to_string_lossy().to_string();
        let t = Instant::now();
        match pdf::file::PdfFile::open(data) {
            Ok(file) => {
                let n = file.pages.len();
                println!("{}: {} pages, opened in {:?}", f, n, t.elapsed());
                let doc = pdf::render::PdfDoc::new(file);
                for i in 0..n.min(8) {
                    let t = Instant::now();
                    let c = doc.render(i, scale, 0);
                    println!("  page {} {}x{} in {:?}", i + 1, c.w, c.h, t.elapsed());
                    png(&out.join(format!("{}-{}.png", stem, i)), c.w, c.h, &c.px);
                }
            }
            Err(e) => println!("{}: couldn't open: {}", f, e),
        }
    }
}

#[allow(dead_code)]
fn edit_test(out: &Path, input: &str, photo: &str) {
    use pdf::edit::{OutPage, Overlay, PageSrc, Photo};
    let file = pdf::file::PdfFile::open(fs::read(input).unwrap()).unwrap();
    let jpg = fs::read(photo).unwrap();
    let im = image::decode(&jpg).unwrap();
    let ph = Photo::Jpeg { data: jpg.clone(), w: im.w, h: im.h, gray: false };
    let ov = vec![
        Overlay::Highlight { x0: 50.0, y0: 680.0, x1: 400.0, y1: 720.0, color: 0xFFDC00 },
        Overlay::Text { x: 60.0, y: 400.0, size: 18.0, color: 0xC81E1E, text: String::from("Added by Solon - café\nsecond line") },
        Overlay::Ink { strokes: vec![vec![(60.0, 300.0), (120.0, 340.0), (180.0, 290.0), (240.0, 350.0)]], width: 3.0, color: 0x1E3CAA },
    ];
    let none: Vec<Overlay> = Vec::new();
    let mut pages = vec![OutPage { src: PageSrc::Pdf { file: &file, index: 0 }, rotate: 0, overlays: &ov }];
    pages.push(OutPage { src: PageSrc::Photo(&ph), rotate: 0, overlays: &none });
    pages.push(OutPage { src: PageSrc::Pdf { file: &file, index: 0 }, rotate: 90, overlays: &none });
    let bytes = pdf::edit::save(&pages, Some("Edited by Solon"));
    let p = out.join("edited.pdf");
    fs::write(&p, &bytes).unwrap();
    println!("edited.pdf: {} bytes", bytes.len());
    let f2 = pdf::file::PdfFile::open(bytes).unwrap();
    let doc = pdf::render::PdfDoc::new(f2);
    for i in 0..doc.file.pages.len() {
        let c = doc.render(i, 0.8, 0);
        png(&out.join(format!("edited-{}.png", i)), c.w, c.h, &c.px);
    }
}

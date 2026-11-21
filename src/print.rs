//! Printing: lay photos out on pages and encode them for the printer.
//!
//! Two printer languages (chosen per printer from what it says it speaks):
//! * IPP over USB with PWG Raster: the "driverless" standard most printers since ~2015 use.
//! * PCL: older HP printers (LaserJets, many DeskJets). Colour via PCL5c "direct by pixel",
//!   black & white as a dithered 1-bit raster.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::image::{fit, Image};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Paper {
    Letter,
    A4,
    Photo4x6,
    Photo5x7,
}

impl Paper {
    pub const ALL: [Paper; 4] = [Paper::Letter, Paper::A4, Paper::Photo4x6, Paper::Photo5x7];
    pub fn name(self) -> &'static str {
        match self {
            Paper::Letter => "Letter",
            Paper::A4 => "A4",
            Paper::Photo4x6 => "4 x 6 photo",
            Paper::Photo5x7 => "5 x 7 photo",
        }
    }
    /// Size in 1/1000 inch.
    pub fn mils(self) -> (u32, u32) {
        match self {
            Paper::Letter => (8500, 11000),
            Paper::A4 => (8268, 11693),
            Paper::Photo4x6 => (4000, 6000),
            Paper::Photo5x7 => (5000, 7000),
        }
    }
    /// IPP / PWG media name.
    pub fn pwg(self) -> &'static str {
        match self {
            Paper::Letter => "na_letter_8.5x11in",
            Paper::A4 => "iso_a4_210x297mm",
            Paper::Photo4x6 => "na_index-4x6_4x6in",
            Paper::Photo5x7 => "na_5x7_5x7in",
        }
    }
    /// PCL page size code (only the sizes every PCL printer knows).
    fn pcl_code(self) -> Option<u32> {
        match self {
            Paper::Letter => Some(2),
            Paper::A4 => Some(26),
            _ => None,
        }
    }
    pub fn pcl_ok(self) -> bool {
        self.pcl_code().is_some()
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Layout {
    pub paper: Paper,
    /// 1, 4 or 9 photos per page.
    pub per_page: u32,
    /// Fill the space (cropping) instead of fitting the whole photo.
    pub fill: bool,
    pub color: bool,
    pub copies: u32,
}

/// Which printer a job goes to.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PrinterId {
    Usb(u32),
    /// Hidden test printer (boot option "testprinter=..."): jobs go out of COM2.
    Test,
}

/// What the print dialog needs to know about a printer.
#[derive(Clone, Debug)]
pub struct PrinterInfo {
    pub id: PrinterId,
    pub name: String,
    pub ipp: bool,
    pub lang: Lang,
    pub color: bool,
    pub papers: Vec<Paper>,
    pub dpi: u32,
}

/// Test printer kinds (boot option "testprinter=ipp|urf|pcl|pclmono|pclxl|ps").
pub fn test_printer(kind: &str) -> Option<PrinterInfo> {
    let (lang, color, name) = match kind {
        "ipp" => (Lang::Pwg, true, "Test printer (IPP)"),
        "urf" => (Lang::Urf, true, "Test printer (AirPrint raster)"),
        "pcl" => (Lang::Pcl, true, "Test printer (PCL color)"),
        "pclmono" => (Lang::Pcl, false, "Test printer (PCL)"),
        "pclxl" => (Lang::PclXl, true, "Test printer (PCL 6)"),
        "ps" => (Lang::PostScript, true, "Test printer (PostScript)"),
        _ => return None,
    };
    let papers = Paper::ALL.iter().copied().filter(|p| lang.any_paper() || p.pcl_ok()).collect();
    Some(PrinterInfo { id: PrinterId::Test, name: String::from(name), ipp: lang.ipp(), lang, color, papers, dpi: 100 })
}

/// The language Solon talks to a printer in.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Lang {
    /// IPP over USB with PWG raster ("driverless": IPP Everywhere, Mopria).
    Pwg,
    /// IPP over USB with Apple raster (AirPrint).
    Urf,
    /// PCL 3/5 raster (HP and compatibles).
    Pcl,
    /// PCL XL / PCL 6.
    PclXl,
    PostScript,
}

impl Lang {
    pub fn ipp(self) -> bool {
        matches!(self, Lang::Pwg | Lang::Urf)
    }
    /// Paper sizes beyond Letter and A4 (only the driverless kinds take any size by name).
    pub fn any_paper(self) -> bool {
        matches!(self, Lang::Pwg | Lang::Urf | Lang::PostScript)
    }
    /// In plain words, for the Printers app.
    pub fn describe(self) -> &'static str {
        match self {
            Lang::Pwg => "Driverless printing (IPP over USB)",
            Lang::Urf => "AirPrint printing (IPP over USB)",
            Lang::Pcl => "PCL (HP's printer language)",
            Lang::PclXl => "PCL 6",
            Lang::PostScript => "PostScript",
        }
    }
}

pub const DPI: u32 = 300;
/// Margin around the printable area (1/4 inch): almost every printer can print here.
const MARGIN_MILS: u32 = 250;
/// Gap between photos on a multi-photo page.
const GAP_MILS: u32 = 150;

/// Pixel size of the printable area at `dpi`.
pub fn page_pixels(paper: Paper, dpi: u32) -> (usize, usize) {
    let (w, h) = paper.mils();
    (((w - 2 * MARGIN_MILS) * dpi / 1000) as usize, ((h - 2 * MARGIN_MILS) * dpi / 1000) as usize)
}

/// A page being composed: white RGB pixels (0x00RRGGBB).
pub struct Page {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

/// Scale `img` to exactly (w, h) (bilinear; good for enlarging small photos for print).
fn resize_exact(img: &Image, w: usize, h: usize) -> Image {
    if w <= img.w && h <= img.h && img.w * h == img.h * w {
        return img.shrink_to_fit(w, h);
    }
    let mut px = vec![0u32; w * h];
    let (sw, sh) = (img.w as u64, img.h as u64);
    for y in 0..h {
        let fy = ((y as u64 * 2 + 1) * sh * 128 / (2 * h as u64)).saturating_sub(64);
        let y0 = (fy / 128).min(sh - 1) as usize;
        let y1 = (y0 + 1).min(img.h - 1);
        let wy = (fy % 128) as u32;
        for x in 0..w {
            let fx = ((x as u64 * 2 + 1) * sw * 128 / (2 * w as u64)).saturating_sub(64);
            let x0 = (fx / 128).min(sw - 1) as usize;
            let x1 = (x0 + 1).min(img.w - 1);
            let wx = (fx % 128) as u32;
            let p = |xx: usize, yy: usize| img.px[yy * img.w + xx];
            let mut out = 0u32;
            for s in [16u32, 8, 0] {
                let c = |q: u32| (q >> s) & 0xFF;
                let top = c(p(x0, y0)) * (128 - wx) + c(p(x1, y0)) * wx;
                let bot = c(p(x0, y1)) * (128 - wx) + c(p(x1, y1)) * wx;
                let v = (top * (128 - wy) + bot * wy) / (128 * 128);
                out |= v << s;
            }
            px[y * w + x] = out | (img.px[y0 * img.w + x0] & 0xFF00_0000);
        }
    }
    Image { w, h, px, opaque: img.opaque }
}

impl Page {
    pub fn new(w: usize, h: usize) -> Page {
        Page { w, h, px: vec![0xFF_FFFF; w * h] }
    }

    /// Cell rectangles (x, y, w, h) for `n` photos per page.
    pub fn cells(&self, per_page: u32, dpi: u32) -> Vec<(usize, usize, usize, usize)> {
        let k = match per_page {
            4 => 2,
            9 => 3,
            _ => 1,
        };
        let gap = (GAP_MILS * dpi / 1000) as usize * if k > 1 { 1 } else { 0 };
        let cw = (self.w - gap * (k - 1)) / k;
        let ch = (self.h - gap * (k - 1)) / k;
        let mut v = Vec::new();
        for r in 0..k {
            for c in 0..k {
                v.push((c * (cw + gap), r * (ch + gap), cw, ch));
            }
        }
        v
    }

    /// Place a photo in a cell, turning it to match the cell's orientation.
    pub fn place(&mut self, photo: &Image, cell: (usize, usize, usize, usize), fill: bool) {
        let (cx, cy, cw, ch) = cell;
        // Landscape photo on a portrait cell (or vice versa): rotate to use the space.
        let turned;
        let img = if (photo.w > photo.h) != (cw > ch) && photo.w != photo.h {
            turned = crate::edit::orient(photo, 1, false);
            &turned
        } else {
            photo
        };
        let (w, h) = if fill {
            // Cover the cell, then crop the overflow.
            if img.w as u64 * ch as u64 >= img.h as u64 * cw as u64 {
                ((img.w as u64 * ch as u64 / img.h as u64) as usize, ch)
            } else {
                (cw, (img.h as u64 * cw as u64 / img.w as u64) as usize)
            }
        } else {
            let (fw, fh) = fit(img.w, img.h, cw, ch);
            if fw == img.w && fh == img.h {
                // Small photo: enlarge to fit too.
                if img.w as u64 * ch as u64 >= img.h as u64 * cw as u64 {
                    (cw, ((img.h as u64 * cw as u64 / img.w as u64) as usize).max(1))
                } else {
                    (((img.w as u64 * ch as u64 / img.h as u64) as usize).max(1), ch)
                }
            } else {
                (fw, fh)
            }
        };
        let scaled = resize_exact(img, w.max(1), h.max(1));
        let ox = cx as i64 + (cw as i64 - w as i64) / 2;
        let oy = cy as i64 + (ch as i64 - h as i64) / 2;
        for y in 0..h {
            let py = oy + y as i64;
            if py < cy as i64 || py >= (cy + ch) as i64 {
                continue;
            }
            for x in 0..w {
                let px = ox + x as i64;
                if px < cx as i64 || px >= (cx + cw) as i64 {
                    continue;
                }
                let s = scaled.px[y * w + x];
                let a = s >> 24;
                let d = &mut self.px[py as usize * self.w + px as usize];
                *d = if a >= 255 { s & 0xFF_FFFF } else { crate::gfx::mix(0xFF_FFFF, s & 0xFF_FFFF, a) };
            }
        }
    }

    fn gray_row(&self, y: usize) -> impl Iterator<Item = u8> + '_ {
        self.px[y * self.w..(y + 1) * self.w].iter().map(|p| {
            let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
            ((77 * r + 150 * g + 29 * b) >> 8) as u8
        })
    }
}

// ---- PCL -----------------------------------------------------------------------------

/// TIFF PackBits (PCL compression mode 2).
fn packbits(row: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < row.len() {
        // Run of identical bytes?
        let mut run = 1;
        while i + run < row.len() && row[i + run] == row[i] && run < 128 {
            run += 1;
        }
        if run >= 3 {
            out.push((257 - run) as u8);
            out.push(row[i]);
            i += run;
            continue;
        }
        // Literal stretch until the next run of 3.
        let start = i;
        let mut n = 0;
        while i < row.len() && n < 128 {
            if i + 2 < row.len() && row[i] == row[i + 1] && row[i] == row[i + 2] {
                break;
            }
            i += 1;
            n += 1;
        }
        out.push((n - 1) as u8);
        out.extend_from_slice(&row[start..start + n]);
    }
}

pub struct PclJob {
    pub data: Vec<u8>,
}

impl PclJob {
    pub fn start(layout: &Layout) -> PclJob {
        let mut d = Vec::new();
        // PJL wrapper so printers that understand several languages pick PCL.
        d.extend_from_slice(b"\x1b%-12345X@PJL JOB NAME=\"Solon\"\r\n@PJL ENTER LANGUAGE=PCL\r\n");
        d.extend_from_slice(b"\x1bE"); // reset
        d.extend_from_slice(format!("\x1b&l{}A", layout.paper.pcl_code().unwrap_or(2)).as_bytes()); // paper size
        d.extend_from_slice(b"\x1b&l0O"); // portrait
        d.extend_from_slice(format!("\x1b&l{}X", layout.copies.clamp(1, 99)).as_bytes()); // copies
        PclJob { data: d }
    }

    pub fn page(&mut self, page: &Page, color: bool, dpi: u32) {
        let d = &mut self.data;
        d.extend_from_slice(format!("\x1b*t{}R", dpi).as_bytes()); // raster resolution (must match the page pixels)
        d.extend_from_slice(b"\x1b*p0x0Y"); // cursor at the top-left of the printable area
        if color {
            // Configure image data: RGB, direct by pixel, 8 bits per primary.
            d.extend_from_slice(b"\x1b*v6W\x00\x03\x00\x08\x08\x08");
        }
        d.extend_from_slice(format!("\x1b*r{}S\x1b*r{}T", page.w, page.h).as_bytes());
        d.extend_from_slice(b"\x1b*r1A"); // start raster at cursor
        d.extend_from_slice(b"\x1b*b2M"); // PackBits rows
        let mut row = Vec::new();
        let mut packed = Vec::new();
        if color {
            for y in 0..page.h {
                row.clear();
                for p in &page.px[y * page.w..(y + 1) * page.w] {
                    row.extend_from_slice(&[(p >> 16) as u8, (p >> 8) as u8, *p as u8]);
                }
                packed.clear();
                packbits(&row, &mut packed);
                d.extend_from_slice(format!("\x1b*b{}W", packed.len()).as_bytes());
                d.extend_from_slice(&packed);
            }
        } else {
            // Floyd-Steinberg dithering to 1 bit (1 = black).
            let w = page.w;
            let mut err_cur = vec![0i32; w + 2];
            let mut err_next = vec![0i32; w + 2];
            for y in 0..page.h {
                row.clear();
                row.resize((w + 7) / 8, 0);
                for (x, g) in page.gray_row(y).enumerate() {
                    let v = g as i32 + err_cur[x + 1] / 16;
                    let (bit, out) = if v < 128 { (1, 0) } else { (0, 255) };
                    let e = v - out;
                    if bit == 1 {
                        row[x / 8] |= 0x80 >> (x % 8);
                    }
                    err_cur[x + 2] += e * 7;
                    err_next[x] += e * 3;
                    err_next[x + 1] += e * 5;
                    err_next[x + 2] += e;
                }
                core::mem::swap(&mut err_cur, &mut err_next);
                err_next.iter_mut().for_each(|v| *v = 0);
                packed.clear();
                packbits(&row, &mut packed);
                d.extend_from_slice(format!("\x1b*b{}W", packed.len()).as_bytes());
                d.extend_from_slice(&packed);
            }
        }
        d.extend_from_slice(b"\x1b*rC"); // end raster
        d.extend_from_slice(b"\x0c"); // form feed: next page
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.data.extend_from_slice(b"\x1bE\x1b%-12345X@PJL EOJ\r\n\x1b%-12345X");
        self.data
    }
}

// ---- PWG Raster ------------------------------------------------------------------------

pub struct PwgJob {
    pub data: Vec<u8>,
    pages: u32,
}

fn be32(buf: &mut [u8], off: usize, v: u32) {
    buf[off..off + 4].copy_from_slice(&v.to_be_bytes());
}

impl PwgJob {
    pub fn start() -> PwgJob {
        PwgJob { data: b"RaS2".to_vec(), pages: 0 }
    }

    /// Add a page. PWG pages cover the whole sheet, so the printable area is centered
    /// on a white full-size page.
    pub fn page(&mut self, page: &Page, layout: &Layout, dpi: u32, total_pages: u32) {
        let (pw_mils, ph_mils) = layout.paper.mils();
        let w = (pw_mils * dpi / 1000) as usize;
        let h = (ph_mils * dpi / 1000) as usize;
        let mx = (w - page.w.min(w)) / 2;
        let my = (h - page.h.min(h)) / 2;
        let color = layout.color;
        let bpp = if color { 3 } else { 1 };

        let mut hdr = vec![0u8; 1796];
        hdr[0..9].copy_from_slice(b"PwgRaster");
        be32(&mut hdr, 276, dpi);
        be32(&mut hdr, 280, dpi);
        be32(&mut hdr, 340, 1); // NumCopies: copies are requested once, through IPP
        be32(&mut hdr, 352, pw_mils * 72 / 1000); // PageSize in points
        be32(&mut hdr, 356, ph_mils * 72 / 1000);
        be32(&mut hdr, 372, w as u32);
        be32(&mut hdr, 376, h as u32);
        be32(&mut hdr, 384, 8); // BitsPerColor
        be32(&mut hdr, 388, 8 * bpp as u32); // BitsPerPixel
        be32(&mut hdr, 392, (w * bpp) as u32); // BytesPerLine
        be32(&mut hdr, 396, 0); // chunky
        be32(&mut hdr, 400, if color { 19 } else { 18 }); // sRGB / sGray
        be32(&mut hdr, 420, bpp as u32); // NumColors
        be32(&mut hdr, 452, total_pages); // TotalPageCount
        be32(&mut hdr, 456, 1); // CrossFeedTransform
        be32(&mut hdr, 460, 1); // FeedTransform
        be32(&mut hdr, 464, mx as u32); // ImageBox
        be32(&mut hdr, 468, my as u32);
        be32(&mut hdr, 472, (mx + page.w) as u32);
        be32(&mut hdr, 476, (my + page.h) as u32);
        be32(&mut hdr, 480, 0x00FF_FFFF); // AlternatePrimary (white)
        let name = layout.paper.pwg().as_bytes();
        hdr[1732..1732 + name.len()].copy_from_slice(name);
        self.data.extend_from_slice(&hdr);

        // Lines: repeat-count byte, then runs of pixels.
        let white: [u8; 3] = [255, 255, 255];
        let pixel = |x: usize, y: usize| -> [u8; 3] {
            if y < my || y >= my + page.h || x < mx || x >= mx + page.w {
                return white;
            }
            let p = page.px[(y - my) * page.w + (x - mx)];
            if color {
                [(p >> 16) as u8, (p >> 8) as u8, p as u8]
            } else {
                let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
                let v = ((77 * r + 150 * g + 29 * b) >> 8) as u8;
                [v, v, v]
            }
        };
        let row = |y: usize| -> Vec<u8> {
            let mut r = Vec::with_capacity(w * bpp);
            for x in 0..w {
                r.extend_from_slice(&pixel(x, y)[..bpp]);
            }
            r
        };
        let mut y = 0;
        while y < h {
            let line = row(y);
            let mut rep = 0;
            while y + rep + 1 < h && rep < 255 && row(y + rep + 1) == line {
                rep += 1;
            }
            self.data.push(rep as u8);
            Self::encode_line(&line, bpp, &mut self.data);
            y += rep + 1;
        }
        self.pages += 1;
    }

    fn encode_line(line: &[u8], bpp: usize, out: &mut Vec<u8>) {
        let n = line.len() / bpp;
        let px = |i: usize| &line[i * bpp..(i + 1) * bpp];
        let mut i = 0;
        while i < n {
            let mut run = 1;
            while i + run < n && run < 128 && px(i + run) == px(i) {
                run += 1;
            }
            if run > 1 {
                out.push((run - 1) as u8);
                out.extend_from_slice(px(i));
                i += run;
                continue;
            }
            let start = i;
            let mut lit = 0;
            while i < n && lit < 128 {
                if i + 1 < n && px(i + 1) == px(i) {
                    break;
                }
                i += 1;
                lit += 1;
            }
            // Control 129..255 = 128..2 literal pixels; a single pixel is a run of one.
            out.push(if lit == 1 { 0 } else { (257 - lit) as u8 });
            out.extend_from_slice(&line[start * bpp..(start + lit) * bpp]);
        }
    }

    pub fn finish(self) -> Vec<u8> {
        self.data
    }
}

// ---- IPP --------------------------------------------------------------------------------

const TAG_OPERATION: u8 = 0x01;
const TAG_JOB: u8 = 0x02;
const TAG_END: u8 = 0x03;
const TAG_INTEGER: u8 = 0x21;
const TAG_KEYWORD: u8 = 0x44;
const TAG_URI: u8 = 0x45;
const TAG_CHARSET: u8 = 0x47;
const TAG_LANGUAGE: u8 = 0x48;
const TAG_MIME: u8 = 0x49;
const TAG_NAME: u8 = 0x42;

fn attr(out: &mut Vec<u8>, tag: u8, name: &str, value: &[u8]) {
    out.push(tag);
    out.extend_from_slice(&(name.len() as u16).to_be_bytes());
    out.extend_from_slice(name.as_bytes());
    out.extend_from_slice(&(value.len() as u16).to_be_bytes());
    out.extend_from_slice(value);
}

fn ipp_header(op: u16, id: u32) -> Vec<u8> {
    let mut v = vec![2, 0];
    v.extend_from_slice(&op.to_be_bytes());
    v.extend_from_slice(&id.to_be_bytes());
    v.push(TAG_OPERATION);
    attr(&mut v, TAG_CHARSET, "attributes-charset", b"utf-8");
    attr(&mut v, TAG_LANGUAGE, "attributes-natural-language", b"en");
    attr(&mut v, TAG_URI, "printer-uri", b"ipp://localhost/ipp/print");
    v
}

/// Get-Printer-Attributes request (asks for what we need to know).
pub fn ipp_get_attributes() -> Vec<u8> {
    let mut v = ipp_header(0x000B, 1);
    for (i, name) in ["printer-make-and-model", "document-format-supported", "media-supported", "print-color-mode-supported", "pwg-raster-document-resolution-supported", "printer-state", "printer-state-reasons", "urf-supported"]
        .iter()
        .enumerate()
    {
        // First value carries the attribute name; further values have an empty name.
        attr(&mut v, TAG_KEYWORD, if i == 0 { "requested-attributes" } else { "" }, name.as_bytes());
    }
    v.push(TAG_END);
    v
}

/// Print-Job request carrying a PWG Raster document.
pub fn ipp_print_job(layout: &Layout, document: &[u8], format: &str) -> Vec<u8> {
    let mut v = ipp_header(0x0002, 2);
    attr(&mut v, TAG_NAME, "requesting-user-name", b"solon");
    attr(&mut v, TAG_NAME, "job-name", b"Solon photos");
    attr(&mut v, TAG_MIME, "document-format", format.as_bytes());
    v.push(TAG_JOB);
    attr(&mut v, TAG_INTEGER, "copies", &layout.copies.clamp(1, 99).to_be_bytes());
    attr(&mut v, TAG_KEYWORD, "media", layout.paper.pwg().as_bytes());
    attr(&mut v, TAG_KEYWORD, "print-color-mode", if layout.color { b"color" } else { b"monochrome" });
    attr(&mut v, TAG_KEYWORD, "print-scaling", b"none");
    v.push(TAG_END);
    v.extend_from_slice(document);
    v
}

/// Wrap an IPP message in an HTTP POST (IPP over USB speaks HTTP/1.1).
pub fn http_post(body: &[u8]) -> Vec<u8> {
    let mut v = format!(
        "POST /ipp/print HTTP/1.1\r\nHost: localhost\r\nContent-Type: application/ipp\r\nContent-Length: {}\r\n\r\n",
        body.len()
    )
    .into_bytes();
    v.extend_from_slice(body);
    v
}

/// What we learned from Get-Printer-Attributes.
#[derive(Default, Debug)]
pub struct IppInfo {
    pub status: u16,
    pub model: String,
    pub formats: Vec<String>,
    /// AirPrint raster abilities ("RS300", "SRGB24", ...).
    pub urf: Vec<String>,
    pub media: Vec<String>,
    pub color_modes: Vec<String>,
    pub resolutions: Vec<u32>,
    pub state: u32,
    pub state_reasons: Vec<String>,
}

/// Parse an IPP response: status code plus the attributes we care about.
pub fn ipp_parse(b: &[u8]) -> Option<IppInfo> {
    if b.len() < 8 {
        return None;
    }
    let mut info = IppInfo { status: u16::from_be_bytes([b[2], b[3]]), ..Default::default() };
    let mut i = 8;
    let mut last = String::new();
    while i < b.len() {
        let tag = b[i];
        i += 1;
        if tag == TAG_END {
            break;
        }
        if tag < 0x10 {
            continue; // group delimiter
        }
        if i + 2 > b.len() {
            break;
        }
        let nl = u16::from_be_bytes([b[i], b[i + 1]]) as usize;
        i += 2;
        let name = core::str::from_utf8(b.get(i..i + nl)?).unwrap_or("");
        i += nl;
        if !name.is_empty() {
            last = String::from(name);
        }
        let vl = u16::from_be_bytes([*b.get(i)?, *b.get(i + 1)?]) as usize;
        i += 2;
        let val = b.get(i..i + vl)?;
        i += vl;
        let text = || String::from(core::str::from_utf8(val).unwrap_or(""));
        match last.as_str() {
            "printer-make-and-model" => info.model = text(),
            "document-format-supported" => info.formats.push(text()),
            "urf-supported" => info.urf.push(text()),
            "media-supported" => info.media.push(text()),
            "print-color-mode-supported" => info.color_modes.push(text()),
            "pwg-raster-document-resolution-supported" if vl == 9 => {
                info.resolutions.push(u32::from_be_bytes([val[0], val[1], val[2], val[3]]))
            }
            "printer-state" if vl == 4 => info.state = u32::from_be_bytes([val[0], val[1], val[2], val[3]]),
            "printer-state-reasons" => info.state_reasons.push(text()),
            _ => {}
        }
    }
    Some(info)
}

/// Split an HTTP response into (status code, body) once it's complete.
pub fn http_response(buf: &[u8]) -> Option<(u32, &[u8])> {
    let end = buf.windows(4).position(|w| w == b"\r\n\r\n")? + 4;
    let head = core::str::from_utf8(&buf[..end]).ok()?;
    let status: u32 = head.split(' ').nth(1)?.parse().ok()?;
    let mut length = None;
    let mut chunked = false;
    for line in head.split("\r\n") {
        let lower = line.to_ascii_lowercase();
        if let Some(v) = lower.strip_prefix("content-length:") {
            length = v.trim().parse::<usize>().ok();
        }
        if lower.starts_with("transfer-encoding:") && lower.contains("chunked") {
            chunked = true;
        }
    }
    if chunked {
        // Find the terminating zero-length chunk; return the raw (still chunked) body,
        // de-chunked into a static-free view is done by `dechunk`.
        if buf[end..].windows(5).any(|w| w == b"0\r\n\r\n") {
            return Some((status, &buf[end..]));
        }
        return None;
    }
    let len = length.unwrap_or(0);
    if buf.len() >= end + len {
        Some((status, &buf[end..end + len]))
    } else {
        None
    }
}

/// Undo HTTP chunked transfer encoding.
pub fn dechunk(body: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < body.len() {
        let Some(nl) = body[i..].windows(2).position(|w| w == b"\r\n") else { break };
        let size = usize::from_str_radix(core::str::from_utf8(&body[i..i + nl]).unwrap_or("0").split(';').next().unwrap_or("0").trim(), 16).unwrap_or(0);
        i += nl + 2;
        if size == 0 || i + size > body.len() {
            break;
        }
        out.extend_from_slice(&body[i..i + size]);
        i += size + 2;
    }
    out
}

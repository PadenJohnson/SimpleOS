//! More printer languages, so printers from more companies work:
//! * PCL XL ("PCL 6"): HP, Brother, Kyocera, Lexmark, Ricoh, Xerox, Samsung and Konica laser
//!   printers. Pages go as colour or grey images, PackBits ("RLE") compressed.
//! * PostScript (level 2): many office lasers (Brother "BR-Script", Kyocera "KPDL", Xerox,
//!   Lexmark, Ricoh). Pages go as images, run-length compressed and ASCII85 encoded.
//! * URF ("Apple raster", used by AirPrint) over IPP-USB, for driverless printers that don't
//!   take PWG raster.
//! All three take the same `Page` the other languages do (the printable area, 1/4-inch margins).

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use crate::print::{Layout, Page, Paper};

const MARGIN_MILS: u32 = 250;

fn rgb_row(page: &Page, y: usize, color: bool, out: &mut Vec<u8>) {
    out.clear();
    for p in &page.px[y * page.w..(y + 1) * page.w] {
        let (r, g, b) = ((p >> 16) & 0xFF, (p >> 8) & 0xFF, p & 0xFF);
        if color {
            out.extend_from_slice(&[r as u8, g as u8, b as u8]);
        } else {
            out.push(((77 * r + 150 * g + 29 * b) >> 8) as u8);
        }
    }
}

/// TIFF PackBits, as PCL XL "RLE" and PostScript RunLengthDecode both read it.
pub fn packbits(data: &[u8], out: &mut Vec<u8>) {
    let mut i = 0;
    while i < data.len() {
        let mut run = 1;
        while i + run < data.len() && data[i + run] == data[i] && run < 128 {
            run += 1;
        }
        if run >= 3 {
            out.push((257 - run) as u8);
            out.push(data[i]);
            i += run;
            continue;
        }
        let start = i;
        let mut n = 0;
        while i < data.len() && n < 128 {
            if i + 2 < data.len() && data[i] == data[i + 1] && data[i] == data[i + 2] {
                break;
            }
            i += 1;
            n += 1;
        }
        out.push((n - 1) as u8);
        out.extend_from_slice(&data[start..start + n]);
    }
}

// ---- PCL XL ---------------------------------------------------------------------------------

// Data type tags.
const UBYTE: u8 = 0xC0;
const UINT16: u8 = 0xC1;
const UINT16_XY: u8 = 0xD1;
const ATTR: u8 = 0xF8;
const EMBED32: u8 = 0xFA;
// Attributes.
const A_COLOR_SPACE: u8 = 3;
const A_MEDIA_SIZE: u8 = 37;
const A_ORIENTATION: u8 = 40;
const A_PAGE_COPIES: u8 = 49;
const A_POINT: u8 = 76;
const A_COLOR_DEPTH: u8 = 98;
const A_BLOCK_HEIGHT: u8 = 99;
const A_COLOR_MAPPING: u8 = 100;
const A_COMPRESS_MODE: u8 = 101;
const A_DEST_SIZE: u8 = 103;
const A_SOURCE_HEIGHT: u8 = 107;
const A_SOURCE_WIDTH: u8 = 108;
const A_START_LINE: u8 = 109;
const A_DATA_ORG: u8 = 130;
const A_MEASURE: u8 = 134;
const A_SOURCE_TYPE: u8 = 136;
const A_UNITS_PER_MEASURE: u8 = 137;
const A_ERROR_REPORT: u8 = 143;
// Operators.
const BEGIN_SESSION: u8 = 0x41;
const END_SESSION: u8 = 0x42;
const BEGIN_PAGE: u8 = 0x43;
const END_PAGE: u8 = 0x44;
const OPEN_DATA_SOURCE: u8 = 0x48;
const CLOSE_DATA_SOURCE: u8 = 0x49;
const SET_COLOR_SPACE: u8 = 0x6A;
const SET_CURSOR: u8 = 0x6B;
const BEGIN_IMAGE: u8 = 0xB0;
const READ_IMAGE: u8 = 0xB1;
const END_IMAGE: u8 = 0xB2;

pub struct PclXlJob {
    pub data: Vec<u8>,
    copies: u32,
    paper: Paper,
}

impl PclXlJob {
    fn ubyte(&mut self, v: u8, a: u8) {
        self.data.extend_from_slice(&[UBYTE, v, ATTR, a]);
    }
    fn uint16(&mut self, v: u16, a: u8) {
        self.data.push(UINT16);
        self.data.extend_from_slice(&v.to_le_bytes());
        self.data.extend_from_slice(&[ATTR, a]);
    }
    fn xy(&mut self, x: u16, y: u16, a: u8) {
        self.data.push(UINT16_XY);
        self.data.extend_from_slice(&x.to_le_bytes());
        self.data.extend_from_slice(&y.to_le_bytes());
        self.data.extend_from_slice(&[ATTR, a]);
    }
    fn op(&mut self, o: u8) {
        self.data.push(o);
    }

    pub fn start(layout: &Layout, dpi: u32) -> PclXlJob {
        let mut j = PclXlJob { data: Vec::new(), copies: layout.copies.clamp(1, 99), paper: layout.paper };
        j.data.extend_from_slice(b"\x1b%-12345X@PJL JOB NAME=\"Solon\"\r\n@PJL ENTER LANGUAGE=PCLXL\r\n");
        // Little-endian binary stream, protocol class 2.0.
        j.data.extend_from_slice(b") HP-PCL XL;2;0;Comment Solon\n");
        j.ubyte(0, A_MEASURE); // inches
        j.xy(dpi as u16, dpi as u16, A_UNITS_PER_MEASURE);
        j.ubyte(0, A_ERROR_REPORT); // no error pages
        j.op(BEGIN_SESSION);
        j.ubyte(0, A_SOURCE_TYPE); // default source
        j.ubyte(1, A_DATA_ORG); // binary, low byte first
        j.op(OPEN_DATA_SOURCE);
        j
    }

    pub fn page(&mut self, page: &Page, color: bool, dpi: u32) {
        self.ubyte(0, A_ORIENTATION); // portrait
        // Media size: eLetterPaper 0, eA4Paper 2; others by name aren't in class 2.0, so use Letter.
        let media = if self.paper == Paper::A4 { 2 } else { 0 };
        self.ubyte(media, A_MEDIA_SIZE);
        self.op(BEGIN_PAGE);
        self.ubyte(if color { 2 } else { 1 }, A_COLOR_SPACE); // eRGB / eGray
        self.op(SET_COLOR_SPACE);
        let m = (MARGIN_MILS * dpi / 1000) as u16;
        self.xy(m, m, A_POINT);
        self.op(SET_CURSOR);
        let (w, h) = (page.w as u16, page.h as u16);
        self.ubyte(0, A_COLOR_MAPPING); // direct pixel
        self.ubyte(2, A_COLOR_DEPTH); // 8 bits
        self.uint16(w, A_SOURCE_WIDTH);
        self.uint16(h, A_SOURCE_HEIGHT);
        self.xy(w, h, A_DEST_SIZE);
        self.op(BEGIN_IMAGE);
        // Rows are padded to 4 bytes; send blocks of up to 128 rows.
        let bpp = if color { 3 } else { 1 };
        let stride = (page.w * bpp).div_ceil(4) * 4;
        let mut row = Vec::with_capacity(stride);
        let mut y = 0;
        while y < page.h {
            let n = 128.min(page.h - y);
            let mut raw = Vec::with_capacity(stride * n);
            for k in 0..n {
                rgb_row(page, y + k, color, &mut row);
                row.resize(stride, 0xFF);
                raw.extend_from_slice(&row);
            }
            let mut packed = Vec::new();
            packbits(&raw, &mut packed);
            self.uint16(y as u16, A_START_LINE);
            self.uint16(n as u16, A_BLOCK_HEIGHT);
            self.ubyte(1, A_COMPRESS_MODE); // RLE (PackBits)
            self.op(READ_IMAGE);
            self.data.push(EMBED32);
            self.data.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            self.data.extend_from_slice(&packed);
            y += n;
        }
        self.op(END_IMAGE);
        let copies = self.copies as u16;
        self.uint16(copies, A_PAGE_COPIES);
        self.op(END_PAGE);
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.op(CLOSE_DATA_SOURCE);
        self.op(END_SESSION);
        self.data.extend_from_slice(b"\x1b%-12345X@PJL EOJ\r\n\x1b%-12345X");
        self.data
    }
}

// ---- PostScript -----------------------------------------------------------------------------

fn ascii85(data: &[u8], out: &mut Vec<u8>) {
    let mut col = 0;
    let mut push = |out: &mut Vec<u8>, b: u8| {
        out.push(b);
        col += 1;
        if col >= 75 {
            out.push(b'\n');
            col = 0;
        }
    };
    for chunk in data.chunks(4) {
        let mut b = [0u8; 4];
        b[..chunk.len()].copy_from_slice(chunk);
        let v = u32::from_be_bytes(b);
        if v == 0 && chunk.len() == 4 {
            push(out, b'z');
            continue;
        }
        let mut digits = [0u8; 5];
        let mut x = v;
        for k in (0..5).rev() {
            digits[k] = (x % 85) as u8 + b'!';
            x /= 85;
        }
        for &d in &digits[..chunk.len() + 1] {
            push(out, d);
        }
    }
    out.extend_from_slice(b"~>\n");
}

pub struct PsJob {
    pub data: Vec<u8>,
    pages: u32,
    paper: Paper,
}

impl PsJob {
    pub fn start(layout: &Layout) -> PsJob {
        let mut d = Vec::new();
        d.extend_from_slice(b"\x1b%-12345X@PJL JOB NAME=\"Solon\"\r\n@PJL ENTER LANGUAGE=POSTSCRIPT\r\n");
        d.extend_from_slice(b"%!PS-Adobe-3.0\n%%Creator: Solon\n%%LanguageLevel: 2\n%%EndComments\n");
        d.extend_from_slice(format!("<< /NumCopies {} >> setpagedevice\n", layout.copies.clamp(1, 99)).as_bytes());
        PsJob { data: d, pages: 0, paper: layout.paper }
    }

    pub fn page(&mut self, page: &Page, color: bool, dpi: u32) {
        self.pages += 1;
        let (pw, ph) = self.paper.mils();
        let (ptw, pth) = (pw * 72 / 1000, ph * 72 / 1000);
        let m = MARGIN_MILS as f32 * 72.0 / 1000.0;
        // The image's size on paper in points.
        let (iw, ih) = (page.w as f32 * 72.0 / dpi as f32, page.h as f32 * 72.0 / dpi as f32);
        let d = &mut self.data;
        d.extend_from_slice(format!("%%Page: {0} {0}\n<< /PageSize [{1} {2}] >> setpagedevice\ngsave\n", self.pages, ptw, pth).as_bytes());
        d.extend_from_slice(format!("{} {} translate {} {} scale\n", m, pth as f32 - m - ih, iw, ih).as_bytes());
        let (cs, dec) = if color { ("/DeviceRGB", "[0 1 0 1 0 1]") } else { ("/DeviceGray", "[0 1]") };
        d.extend_from_slice(
            format!(
                "{} setcolorspace\n<< /ImageType 1 /Width {} /Height {} /BitsPerComponent 8 /Decode {} /ImageMatrix [{} 0 0 -{} 0 {}] /DataSource currentfile /ASCII85Decode filter /RunLengthDecode filter >> image\n",
                cs, page.w, page.h, dec, page.w, page.h, page.h
            )
            .as_bytes(),
        );
        let mut raw = Vec::new();
        let mut row = Vec::new();
        let mut packed = Vec::new();
        for y in 0..page.h {
            rgb_row(page, y, color, &mut row);
            raw.extend_from_slice(&row);
            // Compress in chunks to keep memory down.
            if raw.len() > 256 * 1024 || y + 1 == page.h {
                packbits(&raw, &mut packed);
                raw.clear();
            }
        }
        packed.push(128); // end of data
        ascii85(&packed, d);
        d.extend_from_slice(b"grestore\nshowpage\n");
    }

    pub fn finish(mut self) -> Vec<u8> {
        self.data.extend_from_slice(format!("%%Trailer\n%%Pages: {}\n%%EOF\n", self.pages).as_bytes());
        self.data.extend_from_slice(b"\x04\x1b%-12345X@PJL EOJ\r\n\x1b%-12345X");
        self.data
    }
}

// ---- URF (Apple raster) ---------------------------------------------------------------------

pub struct UrfJob {
    pub data: Vec<u8>,
    count_at: usize,
    pages: u32,
}

impl UrfJob {
    pub fn start() -> UrfJob {
        let mut d = b"UNIRAST\0".to_vec();
        let count_at = d.len();
        d.extend_from_slice(&0u32.to_be_bytes());
        UrfJob { data: d, count_at, pages: 0 }
    }

    /// A page covers the whole sheet: the printable area is centred on white.
    pub fn page(&mut self, page: &Page, layout: &Layout, dpi: u32) {
        let (pw, ph) = layout.paper.mils();
        let (w, h) = ((pw * dpi / 1000) as usize, (ph * dpi / 1000) as usize);
        let (mx, my) = ((w - page.w.min(w)) / 2, (h - page.h.min(h)) / 2);
        let color = layout.color;
        let bpp = if color { 3 } else { 1 };
        let mut hdr = [0u8; 32];
        hdr[0] = (8 * bpp) as u8;
        hdr[1] = if color { 1 } else { 0 }; // sRGB / sGray
        hdr[2] = 1; // one-sided
        hdr[3] = 4; // normal quality
        hdr[12..16].copy_from_slice(&(w as u32).to_be_bytes());
        hdr[16..20].copy_from_slice(&(h as u32).to_be_bytes());
        hdr[20..24].copy_from_slice(&dpi.to_be_bytes());
        self.data.extend_from_slice(&hdr);
        let white = vec![255u8; w * bpp];
        let mut line = Vec::with_capacity(w * bpp);
        let mut tmp = Vec::new();
        let make = |y: usize, line: &mut Vec<u8>, tmp: &mut Vec<u8>| {
            line.clear();
            if y < my || y >= my + page.h {
                line.extend_from_slice(&white);
                return;
            }
            line.extend_from_slice(&white[..mx * bpp]);
            rgb_row(page, y - my, color, tmp);
            line.extend_from_slice(tmp);
            line.resize(w * bpp, 255);
        };
        let mut y = 0;
        let mut next = Vec::with_capacity(w * bpp);
        while y < h {
            make(y, &mut line, &mut tmp);
            let mut rep = 0;
            while y + rep + 1 < h && rep < 255 {
                make(y + rep + 1, &mut next, &mut tmp);
                if next != line {
                    break;
                }
                rep += 1;
            }
            self.data.push(rep as u8);
            encode_line(&line, bpp, &mut self.data);
            y += rep + 1;
        }
        self.pages += 1;
    }

    pub fn finish(mut self) -> Vec<u8> {
        let at = self.count_at;
        self.data[at..at + 4].copy_from_slice(&self.pages.to_be_bytes());
        self.data
    }
}

/// One line in the PWG/URF run-length form (counts are in pixels).
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
        out.push(if lit == 1 { 0 } else { (257 - lit) as u8 });
        out.extend_from_slice(&line[start * bpp..(start + lit) * bpp]);
    }
}

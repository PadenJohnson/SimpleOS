//! Writing PDF files: a low-level object writer (also used to save edited PDFs), and
//! `DocPdf`, which makes new PDFs from text, rectangles and photos (Writer's "Save as PDF",
//! PDFs from photos). Text uses the Noto fonts, embedded with only the glyphs used.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::font::{Face, FONTS};

pub fn zlib(data: &[u8]) -> Vec<u8> {
    miniz_oxide::deflate::compress_to_vec_zlib(data, 6)
}

/// A number as PDF writes it (up to 3 decimals, no exponent).
pub fn num(v: f32) -> String {
    let neg = v < 0.0;
    let m = (if neg { -v } else { v } * 1000.0 + 0.5) as i64;
    let (int, frac) = (m / 1000, m % 1000);
    let mut s = if neg && m != 0 { String::from("-") } else { String::new() };
    s += &format!("{}", int);
    if frac != 0 {
        let f = format!("{:03}", frac);
        s.push('.');
        s.push_str(f.trim_end_matches('0'));
    }
    s
}

/// Numbered objects, written out with a cross-reference table.
pub struct PdfOut {
    objs: Vec<Option<Vec<u8>>>,
}

impl PdfOut {
    pub fn new() -> PdfOut {
        PdfOut { objs: Vec::new() }
    }
    /// Reserve an object number.
    pub fn alloc(&mut self) -> u32 {
        self.objs.push(None);
        self.objs.len() as u32
    }
    pub fn set(&mut self, id: u32, body: Vec<u8>) {
        self.objs[id as usize - 1] = Some(body);
    }
    pub fn add(&mut self, body: Vec<u8>) -> u32 {
        let id = self.alloc();
        self.set(id, body);
        id
    }
    pub fn add_str(&mut self, body: &str) -> u32 {
        self.add(Vec::from(body.as_bytes()))
    }
    /// A stream object: `dict` is the inside of the dictionary (without << >> and Length).
    pub fn stream_body(dict: &str, data: &[u8], compress: bool) -> Vec<u8> {
        let (data, filter) = if compress { (zlib(data), " /Filter /FlateDecode") } else { (Vec::from(data), "") };
        let mut b = Vec::from(format!("<< {}{} /Length {} >>\nstream\n", dict, filter, data.len()).as_bytes());
        b.extend_from_slice(&data);
        b.extend_from_slice(b"\nendstream");
        b
    }
    pub fn add_stream(&mut self, dict: &str, data: &[u8], compress: bool) -> u32 {
        self.add(Self::stream_body(dict, data, compress))
    }

    pub fn finish(self, root: u32, info: Option<u32>) -> Vec<u8> {
        let mut out = Vec::from(&b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n"[..]);
        let mut offsets = Vec::with_capacity(self.objs.len());
        for (i, o) in self.objs.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", i + 1).as_bytes());
            match o {
                Some(b) => out.extend_from_slice(b),
                None => out.extend_from_slice(b"null"),
            }
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n0000000000 65535 f \n", self.objs.len() + 1).as_bytes());
        for off in offsets {
            out.extend_from_slice(format!("{:010} 00000 n \n", off).as_bytes());
        }
        let info = info.map(|i| format!(" /Info {} 0 R", i)).unwrap_or_default();
        out.extend_from_slice(format!("trailer\n<< /Size {} /Root {} 0 R{} >>\nstartxref\n{}\n%%EOF\n", self.objs.len() + 1, root, info, xref).as_bytes());
        out
    }
}

fn rgb_op(c: u32, op: &str) -> String {
    let ch = |s: u32| num(((c >> s) & 0xFF) as f32 / 255.0);
    format!("{} {} {} {}", ch(16), ch(8), ch(0), op)
}

struct FontUse {
    id: u32,
    gids: BTreeSet<u16>,
    uni: BTreeMap<u16, char>,
}

/// Make a new PDF from text, boxes and pictures. Coordinates are points from the top-left.
pub struct DocPdf {
    out: PdfOut,
    pages_id: u32,
    w: f32,
    h: f32,
    pages: Vec<u32>,
    content: String,
    fonts: BTreeMap<Face, FontUse>,
    page_fonts: BTreeSet<Face>,
    page_images: Vec<(String, u32)>,
    images: usize,
}

impl DocPdf {
    pub fn new(w: i32, h: i32) -> DocPdf {
        let mut out = PdfOut::new();
        let pages_id = out.alloc();
        DocPdf { out, pages_id, w: w as f32, h: h as f32, pages: Vec::new(), content: String::new(), fonts: BTreeMap::new(), page_fonts: BTreeSet::new(), page_images: Vec::new(), images: 0 }
    }

    pub fn page_size(&mut self, w: f32, h: f32) {
        self.w = w;
        self.h = h;
    }

    pub fn begin_page(&mut self) {
        self.content.clear();
        self.page_fonts.clear();
        self.page_images.clear();
    }

    pub fn text(&mut self, x: f32, base: f32, s: &str, face: Face, size: f32, color: u32) {
        if s.is_empty() {
            return;
        }
        if !self.fonts.contains_key(&face) {
            let id = self.out.alloc();
            self.fonts.insert(face, FontUse { id, gids: BTreeSet::new(), uni: BTreeMap::new() });
        }
        let fu = self.fonts.get_mut(&face).unwrap();
        let mut hex = String::with_capacity(s.len() * 4);
        {
            let mut f = FONTS.lock();
            for ch in s.chars() {
                let mut g = f.glyph_index(face, ch);
                if g == 0 {
                    g = f.glyph_index(face, '?');
                }
                fu.gids.insert(g);
                fu.uni.entry(g).or_insert(ch);
                hex += &format!("{:04X}", g);
            }
        }
        self.page_fonts.insert(face);
        self.content += &format!("BT /F{} {} Tf {} 1 0 0 1 {} {} Tm <{}> Tj ET\n", face as u8, num(size), rgb_op(color, "rg"), num(x), num(self.h - base), hex);
    }

    pub fn rect(&mut self, x: f32, y: f32, w: f32, h: f32, color: u32) {
        self.content += &format!("{} {} {} {} {} re f\n", rgb_op(color, "rg"), num(x), num(self.h - y - h), num(w), num(h));
    }

    /// A JPEG photo drawn into (x, y, w, h).
    pub fn jpeg(&mut self, data: &[u8], pw: usize, ph: usize, gray: bool, x: f32, y: f32, w: f32, h: f32) {
        let cs = if gray { "/DeviceGray" } else { "/DeviceRGB" };
        let id = self.out.add(PdfOut::stream_body(&format!("/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {} /BitsPerComponent 8 /Filter /DCTDecode", pw, ph, cs), data, false));
        self.images += 1;
        let name = format!("Im{}", self.images);
        self.content += &format!("q {} 0 0 {} {} {} cm /{} Do Q\n", num(w), num(h), num(x), num(self.h - y - h), name);
        self.page_images.push((name, id));
    }

    pub fn end_page(&mut self) {
        let content = self.out.add_stream("", self.content.as_bytes(), true);
        let mut res = String::from("<< /ProcSet [/PDF /Text /ImageC /ImageB]");
        if !self.page_fonts.is_empty() {
            res += " /Font <<";
            for f in &self.page_fonts {
                res += &format!(" /F{} {} 0 R", *f as u8, self.fonts[f].id);
            }
            res += " >>";
        }
        if !self.page_images.is_empty() {
            res += " /XObject <<";
            for (n, id) in &self.page_images {
                res += &format!(" /{} {} 0 R", n, id);
            }
            res += " >>";
        }
        res += " >>";
        let page = self.out.add_str(&format!("<< /Type /Page /Parent {} 0 R /MediaBox [0 0 {} {}] /Resources {} /Contents {} 0 R >>", self.pages_id, num(self.w), num(self.h), res, content));
        self.pages.push(page);
    }

    fn write_font(out: &mut PdfOut, face: Face, fu: &FontUse) {
        let data = face.data();
        let sub = super::ttf::subset(data, &fu.gids).unwrap_or_else(|| Vec::from(data));
        let file = out.add(PdfOut::stream_body(&format!("/Length1 {}", sub.len()), &sub, true));
        let (upem, asc, desc, _) = FONTS.lock().vmetrics(face);
        let k = |v: i32| v * 1000 / upem.max(1);
        // Six-letter subset tag from the glyph set.
        let mut h = 0x9E37_79B9u32;
        for g in &fu.gids {
            h = (h ^ *g as u32).wrapping_mul(0x0100_0193);
        }
        let tag: String = (0..6).map(|i| (b'A' + ((h >> (i * 5)) % 26) as u8) as char).collect();
        let name = format!("{}+{}", tag, face.ps_name());
        let italic = if face.italic() { -12 } else { 0 };
        let flags = 32 | if face.serif() { 2 } else { 0 } | if face.italic() { 64 } else { 0 };
        let desc_id = out.add_str(&format!(
            "<< /Type /FontDescriptor /FontName /{} /Flags {} /FontBBox [-600 -400 2000 1200] /ItalicAngle {} /Ascent {} /Descent {} /CapHeight 714 /StemV {} /FontFile2 {} 0 R >>",
            name,
            flags,
            italic,
            k(asc),
            -k(desc),
            if face.bold() { 140 } else { 80 },
            file
        ));
        let mut w = String::from("[");
        {
            let mut f = FONTS.lock();
            for g in &fu.gids {
                let (adv, upem) = f.advance_units(face, *g);
                w += &format!(" {} [{}]", g, adv as u32 * 1000 / upem.max(1) as u32);
            }
        }
        w += " ]";
        let cid = out.add_str(&format!(
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /{} /CIDSystemInfo << /Registry (Adobe) /Ordering (Identity) /Supplement 0 >> /FontDescriptor {} 0 R /W {} /CIDToGIDMap /Identity >>",
            name, desc_id, w
        ));
        let mut cmap = String::from("/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n/CMapName /Adobe-Identity-UCS def\n/CMapType 2 def\n1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n");
        let pairs: Vec<(&u16, &char)> = fu.uni.iter().collect();
        for chunk in pairs.chunks(100) {
            cmap += &format!("{} beginbfchar\n", chunk.len());
            for (g, ch) in chunk {
                let mut u = [0u16; 2];
                let units = ch.encode_utf16(&mut u);
                let hex: String = units.iter().map(|x| format!("{:04X}", x)).collect();
                cmap += &format!("<{:04X}> <{}>\n", g, hex);
            }
            cmap += "endbfchar\n";
        }
        cmap += "endcmap\nCMapName currentdict /CMap defineresource pop\nend\nend\n";
        let tu = out.add_stream("", cmap.as_bytes(), true);
        out.set(fu.id, Vec::from(format!("<< /Type /Font /Subtype /Type0 /BaseFont /{} /Encoding /Identity-H /DescendantFonts [{} 0 R] /ToUnicode {} 0 R >>", name, cid, tu).as_bytes()));
    }

    pub fn finish(mut self) -> Vec<u8> {
        if self.pages.is_empty() {
            self.begin_page();
            self.end_page();
        }
        let fonts = core::mem::take(&mut self.fonts);
        for (face, fu) in &fonts {
            Self::write_font(&mut self.out, *face, fu);
        }
        let kids: String = self.pages.iter().map(|p| format!("{} 0 R ", p)).collect();
        self.out.set(self.pages_id, Vec::from(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids, self.pages.len()).as_bytes()));
        let root = self.out.add_str(&format!("<< /Type /Catalog /Pages {} 0 R >>", self.pages_id));
        let info = self.out.add_str("<< /Producer (Solon) /Creator (Solon) >>");
        self.out.finish(root, Some(info))
    }
}

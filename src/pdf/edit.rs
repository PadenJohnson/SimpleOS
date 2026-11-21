//! Saving an edited PDF: the pages chosen (from one or more PDFs, in any order, turned),
//! photos as new pages, and what was added on top (text, drawing, highlights, signatures)
//! as an extra content stream. Pages are copied object by object, so everything on them
//! (fonts, pictures, form appearances) comes along unchanged.

use alloc::collections::{BTreeMap, BTreeSet};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::file::PdfFile;
use super::obj::{Dict, Obj};
use super::write::{num, PdfOut};

/// Something added on top of a page, in the page's own (PDF user space) points.
#[derive(Clone, Debug)]
pub enum Overlay {
    Text { x: f32, y: f32, size: f32, color: u32, text: String },
    Ink { strokes: Vec<Vec<(f32, f32)>>, width: f32, color: u32 },
    Highlight { x0: f32, y0: f32, x1: f32, y1: f32, color: u32 },
}

/// A photo page: a JPEG, or raw RGB pixels.
#[derive(Clone)]
pub enum Photo {
    Jpeg { data: Vec<u8>, w: usize, h: usize, gray: bool },
    Rgb { px: Vec<u8>, w: usize, h: usize },
}

impl Photo {
    pub fn size(&self) -> (usize, usize) {
        match self {
            Photo::Jpeg { w, h, .. } | Photo::Rgb { w, h, .. } => (*w, *h),
        }
    }
    /// The page a photo goes on: Letter, turned to suit the photo, in points.
    pub fn page_size(&self) -> (f32, f32) {
        let (w, h) = self.size();
        if w > h {
            (792.0, 612.0)
        } else {
            (612.0, 792.0)
        }
    }
    /// Where the photo sits on its page (x, y from the bottom-left, w, h).
    pub fn placement(&self) -> (f32, f32, f32, f32) {
        let (pw, ph) = self.page_size();
        let (w, h) = self.size();
        let (aw, ah) = (pw - 72.0, ph - 72.0);
        let s = (aw / w.max(1) as f32).min(ah / h.max(1) as f32);
        let (dw, dh) = (w as f32 * s, h as f32 * s);
        ((pw - dw) / 2.0, (ph - dh) / 2.0, dw, dh)
    }
}

pub enum PageSrc<'a> {
    Pdf { file: &'a PdfFile, index: usize },
    Photo(&'a Photo),
}

pub struct OutPage<'a> {
    pub src: PageSrc<'a>,
    /// Extra turn (degrees, multiple of 90).
    pub rotate: i32,
    pub overlays: &'a [Overlay],
}

fn esc_name(n: &[u8]) -> String {
    let mut s = String::from("/");
    for &b in n {
        if b <= b' ' || b >= 0x7F || b"#()<>[]{}/%".contains(&b) {
            s += &format!("#{:02X}", b);
        } else {
            s.push(b as char);
        }
    }
    s
}

fn hex(s: &[u8]) -> String {
    let mut o = String::with_capacity(s.len() * 2 + 2);
    o.push('<');
    for b in s {
        o += &format!("{:02X}", b);
    }
    o.push('>');
    o
}

struct Copier<'a> {
    src: &'a PdfFile,
    map: BTreeMap<u32, u32>,
    queue: Vec<u32>,
    /// Old page objects that are being written (map to their new numbers).
    pages: BTreeMap<u32, u32>,
}

impl Copier<'_> {
    fn reference(&mut self, out: &mut PdfOut, old: u32) -> Option<u32> {
        if let Some(&p) = self.pages.get(&old) {
            return Some(p);
        }
        if let Some(&n) = self.map.get(&old) {
            return Some(n);
        }
        // Don't wander into other pages or the page tree.
        let o = self.src.get(old);
        if let Some(d) = o.as_dict() {
            if d.is("Type", "Page") || d.is("Type", "Pages") || d.is("Type", "Catalog") {
                return None;
            }
        }
        if matches!(o, Obj::Null) {
            return None;
        }
        let id = out.alloc();
        self.map.insert(old, id);
        self.queue.push(old);
        Some(id)
    }

    fn ser(&mut self, out: &mut PdfOut, o: &Obj, b: &mut String) {
        match o {
            Obj::Null => *b += "null",
            Obj::Bool(v) => *b += if *v { "true" } else { "false" },
            Obj::Int(i) => *b += &format!("{}", i),
            Obj::Real(r) => *b += &num(*r),
            Obj::Name(n) => *b += &esc_name(n),
            Obj::Str(s) => *b += &hex(s),
            Obj::Array(a) => {
                b.push('[');
                for (i, x) in a.iter().enumerate() {
                    if i > 0 {
                        b.push(' ');
                    }
                    self.ser(out, x, b);
                }
                b.push(']');
            }
            Obj::Dict(d) => self.ser_dict(out, d, b, None),
            Obj::Stream(d, _) => self.ser_dict(out, d, b, None),
            Obj::Ref(n, _) => match self.reference(out, *n) {
                Some(id) => *b += &format!("{} 0 R", id),
                None => *b += "null",
            },
        }
    }

    fn ser_dict(&mut self, out: &mut PdfOut, d: &Dict, b: &mut String, length: Option<usize>) {
        *b += "<<";
        for (k, v) in &d.0 {
            if k.as_slice() == b"Parent" || (length.is_some() && k.as_slice() == b"Length") {
                continue;
            }
            b.push(' ');
            *b += &esc_name(k);
            b.push(' ');
            self.ser(out, v, b);
        }
        if let Some(l) = length {
            *b += &format!(" /Length {}", l);
        }
        *b += " >>";
    }

    fn object_bytes(&mut self, out: &mut PdfOut, o: &Obj) -> Vec<u8> {
        let mut s = String::new();
        match o {
            Obj::Stream(d, data) => {
                let mut d = d.clone();
                // Decrypted data no longer needs a Crypt filter.
                if matches!(d.get("Filter"), Some(Obj::Name(n)) if n == b"Crypt") {
                    d.remove("Filter");
                    d.remove("DecodeParms");
                }
                self.ser_dict(out, &d, &mut s, Some(data.len()));
                let mut v = Vec::from(s.as_bytes());
                v.extend_from_slice(b"\nstream\n");
                v.extend_from_slice(data);
                v.extend_from_slice(b"\nendstream");
                v
            }
            o => {
                self.ser(out, o, &mut s);
                Vec::from(s.as_bytes())
            }
        }
    }

    fn drain(&mut self, out: &mut PdfOut) {
        while let Some(old) = self.queue.pop() {
            let o = self.src.get(old);
            let id = self.map[&old];
            let bytes = self.object_bytes(out, &o);
            out.set(id, bytes);
        }
    }
}

fn rgb(c: u32, op: &str) -> String {
    let ch = |s: u32| num(((c >> s) & 0xFF) as f32 / 255.0);
    format!("{} {} {} {}", ch(16), ch(8), ch(0), op)
}

/// WinAnsi bytes for Helvetica text.
fn winansi(s: &str) -> Vec<u8> {
    const HI: [u16; 32] = [0x20AC, 0, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0, 0x017D, 0, 0, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0, 0x017E, 0x0178];
    s.chars()
        .map(|c| {
            let u = c as u32;
            if (0x20..0x7F).contains(&u) || (0xA0..0x100).contains(&u) {
                u as u8
            } else {
                HI.iter().position(|&x| x as u32 == u).map(|i| 0x80 + i as u8).unwrap_or(b'?')
            }
        })
        .collect()
}

fn lit(b: &[u8]) -> String {
    let mut s = String::from("(");
    for &c in b {
        match c {
            b'(' | b')' | b'\\' => {
                s.push('\\');
                s.push(c as char);
            }
            0x20..=0x7E => s.push(c as char),
            _ => s += &format!("\\{:03o}", c),
        }
    }
    s.push(')');
    s
}

/// The content stream for overlays.
pub fn overlay_content(ov: &[Overlay]) -> String {
    let mut c = String::new();
    for o in ov {
        match o {
            Overlay::Highlight { x0, y0, x1, y1, color } => {
                c += &format!("q /SolonHL gs {} {} {} {} {} re f Q\n", rgb(*color, "rg"), num(x0.min(*x1)), num(y0.min(*y1)), num((x1 - x0).abs()), num((y1 - y0).abs()));
            }
            Overlay::Ink { strokes, width, color } => {
                c += &format!("q {} {} w 1 J 1 j\n", rgb(*color, "RG"), num(*width));
                for s in strokes {
                    for (i, (x, y)) in s.iter().enumerate() {
                        c += &format!("{} {} {}\n", num(*x), num(*y), if i == 0 { "m" } else { "l" });
                    }
                    if s.len() == 1 {
                        c += &format!("{} {} l\n", num(s[0].0 + 0.01), num(s[0].1));
                    }
                    c += "S\n";
                }
                c += "Q\n";
            }
            Overlay::Text { x, y, size, color, text } => {
                c += &format!("q BT /SolonF {} Tf {} TL {} 1 0 0 1 {} {} Tm\n", num(*size), num(size * 1.2), rgb(*color, "rg"), num(*x), num(*y));
                for (i, line) in text.split('\n').enumerate() {
                    if i > 0 {
                        c += "T*\n";
                    }
                    c += &format!("{} Tj\n", lit(&winansi(line)));
                }
                c += "ET Q\n";
            }
        }
    }
    c
}

/// Write the new PDF.
pub fn save(pages: &[OutPage], title: Option<&str>) -> Vec<u8> {
    let mut out = PdfOut::new();
    let pages_id = out.alloc();
    let font_id = out.add_str("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>");
    let hl_id = out.add_str("<< /Type /ExtGState /ca 0.4 /BM /Multiply >>");
    // One copier per source file.
    let mut copiers: Vec<(*const PdfFile, Copier)> = Vec::new();
    let mut page_ids = Vec::new();
    // Number the pages first, so links between kept pages still work.
    let mut new_ids = Vec::new();
    for p in pages {
        let id = out.alloc();
        new_ids.push(id);
        if let PageSrc::Pdf { file, index } = p.src {
            let key = file as *const PdfFile;
            if !copiers.iter().any(|(k, _)| *k == key) {
                copiers.push((key, Copier { src: file, map: BTreeMap::new(), queue: Vec::new(), pages: BTreeMap::new() }));
            }
            let pid = file.pages[index].id;
            let c = &mut copiers.iter_mut().find(|(k, _)| *k == key).unwrap().1;
            if pid != 0 {
                c.pages.entry(pid).or_insert(id);
            }
        }
    }
    let mut used_once: BTreeSet<u32> = BTreeSet::new();
    for (k, p) in pages.iter().enumerate() {
        let id = new_ids[k];
        let extra = overlay_content(p.overlays);
        match p.src {
            PageSrc::Pdf { file, index } => {
                let key = file as *const PdfFile;
                let c = &mut copiers.iter_mut().find(|(kk, _)| *kk == key).unwrap().1;
                let page = &file.pages[index];
                // The same page twice: the second copy gets its own number.
                if !used_once.insert(id) {
                    continue;
                }
                let mut d = page.dict.clone();
                d.remove("Parent");
                d.set("Resources", Obj::Dict(page.resources.clone()));
                d.set("MediaBox", Obj::Array(page.media.iter().map(|v| Obj::Real(*v)).collect()));
                d.set("CropBox", Obj::Array(page.crop.iter().map(|v| Obj::Real(*v)).collect()));
                d.set("Rotate", Obj::Int(((page.rotate + p.rotate).rem_euclid(360)) as i64));
                d.remove("StructParents");
                let mut s = String::new();
                c.ser_dict(&mut out, &d, &mut s, None);
                if !extra.is_empty() {
                    // Add our fonts/state to the resources and our drawing after the page's own
                    // (wrapped in q/Q so whatever the page leaves set doesn't affect us).
                    let q = out.add_stream("", b"q\n", false);
                    let ov = out.add_stream("", format!("Q\n{}", extra).as_bytes(), true);
                    let mut res = page.resources.clone();
                    let mut fonts = match file.dget(&res, "Font") {
                        Obj::Dict(f) => f,
                        _ => Dict::default(),
                    };
                    fonts.set("SolonF", Obj::Ref(u32::MAX, 0));
                    res.set("Font", Obj::Dict(fonts));
                    let mut gss = match file.dget(&res, "ExtGState") {
                        Obj::Dict(g) => g,
                        _ => Dict::default(),
                    };
                    gss.set("SolonHL", Obj::Ref(u32::MAX - 1, 0));
                    res.set("ExtGState", Obj::Dict(gss));
                    let mut rs = String::new();
                    c.ser_dict(&mut out, &res, &mut rs, None);
                    let rs = rs.replace(&format!("{} 0 R", "null"), "null");
                    // Our two placeholder references come out as "null": put the real ones in.
                    let rs = rs.replacen("/SolonF null", &format!("/SolonF {} 0 R", font_id), 1).replacen("/SolonHL null", &format!("/SolonHL {} 0 R", hl_id), 1);
                    let mut contents = String::from("[");
                    contents += &format!("{} 0 R ", q);
                    match page.dict.get("Contents") {
                        Some(Obj::Ref(n, _)) => {
                            // Either a stream or an array of them.
                            match file.get(*n) {
                                Obj::Array(a) => {
                                    for x in a {
                                        let mut t = String::new();
                                        c.ser(&mut out, &x, &mut t);
                                        contents += &t;
                                        contents.push(' ');
                                    }
                                }
                                _ => {
                                    if let Some(nid) = c.reference(&mut out, *n) {
                                        contents += &format!("{} 0 R ", nid);
                                    }
                                }
                            }
                        }
                        Some(Obj::Array(a)) => {
                            for x in a {
                                let mut t = String::new();
                                c.ser(&mut out, x, &mut t);
                                contents += &t;
                                contents.push(' ');
                            }
                        }
                        _ => {}
                    }
                    contents += &format!("{} 0 R]", ov);
                    // Swap Resources and Contents in the serialized page.
                    let mut d2 = d.clone();
                    d2.remove("Resources");
                    d2.remove("Contents");
                    let mut s2 = String::new();
                    c.ser_dict(&mut out, &d2, &mut s2, None);
                    let s2 = format!("{} /Resources {} /Contents {} >>", s2.trim_end_matches(" >>").trim_end_matches(">>"), rs, contents);
                    s = s2;
                }
                let s = s.replacen("<<", &format!("<< /Parent {} 0 R", pages_id), 1);
                out.set(id, Vec::from(s.as_bytes()));
                c.drain(&mut out);
            }
            PageSrc::Photo(ph) => {
                let (pw, phh) = ph.page_size();
                let (x, y, w, h) = ph.placement();
                let img = match ph {
                    Photo::Jpeg { data, w, h, gray } => out.add(PdfOut::stream_body(&format!("/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace {} /BitsPerComponent 8 /Filter /DCTDecode", w, h, if *gray { "/DeviceGray" } else { "/DeviceRGB" }), data, false)),
                    Photo::Rgb { px, w, h } => out.add(PdfOut::stream_body(&format!("/Type /XObject /Subtype /Image /Width {} /Height {} /ColorSpace /DeviceRGB /BitsPerComponent 8", w, h), px, true)),
                };
                let content = format!("q {} 0 0 {} {} {} cm /Im0 Do Q\n{}", num(w), num(h), num(x), num(y), extra);
                let cid = out.add_stream("", content.as_bytes(), true);
                let rot = p.rotate.rem_euclid(360);
                out.set(
                    id,
                    Vec::from(
                        format!(
                            "<< /Type /Page /Parent {} 0 R /MediaBox [0 0 {} {}] /Rotate {} /Resources << /XObject << /Im0 {} 0 R >> /Font << /SolonF {} 0 R >> /ExtGState << /SolonHL {} 0 R >> >> /Contents {} 0 R >>",
                            pages_id,
                            num(pw),
                            num(phh),
                            rot,
                            img,
                            font_id,
                            hl_id,
                            cid
                        )
                        .as_bytes(),
                    ),
                );
            }
        }
        page_ids.push(id);
    }
    let kids: String = page_ids.iter().map(|p| format!("{} 0 R ", p)).collect();
    out.set(pages_id, Vec::from(format!("<< /Type /Pages /Kids [{}] /Count {} >>", kids, page_ids.len()).as_bytes()));
    let root = out.add_str(&format!("<< /Type /Catalog /Pages {} 0 R >>", pages_id));
    let t = title.map(|t| format!(" /Title {}", hex(&{
        let mut v = Vec::from([0xFE, 0xFF]);
        for u in t.encode_utf16() {
            v.extend_from_slice(&u.to_be_bytes());
        }
        v
    }))).unwrap_or_default();
    let info = out.add_str(&format!("<< /Producer (Solon){} >>", t));
    out.finish(root, Some(info))
}

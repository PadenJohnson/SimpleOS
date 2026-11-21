//! A PDF file: its cross-reference table (or streams, or rebuilt by scanning when broken),
//! objects (including those packed in object streams), decryption, and the page list.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::crypt::{Decryptor, Method, Params};
use super::filter;
use super::obj::{Dict, Lexer, Obj, Tok};

#[derive(Clone, Copy, Debug)]
enum Entry {
    At(usize),
    InStream(u32, u32),
}

#[derive(Clone)]
pub struct Page {
    /// Object number of the page (0 if it had none).
    pub id: u32,
    pub dict: Dict,
    pub resources: Dict,
    pub media: [f32; 4],
    pub crop: [f32; 4],
    pub rotate: i32,
}

impl Page {}

pub struct PdfFile {
    pub data: Vec<u8>,
    xref: BTreeMap<u32, Entry>,
    pub trailer: Dict,
    cache: RefCell<BTreeMap<u32, Obj>>,
    crypt: Option<Decryptor>,
    encrypt_num: Option<u32>,
    pub pages: Vec<Page>,
    /// Object streams already unpacked: stream number -> (offsets, decoded data).
    objstm: RefCell<BTreeMap<u32, (Vec<(u32, usize)>, Vec<u8>)>>,
}

fn find_last(hay: &[u8], needle: &[u8]) -> Option<usize> {
    if hay.len() < needle.len() {
        return None;
    }
    (0..=hay.len() - needle.len()).rev().find(|&i| &hay[i..i + needle.len()] == needle)
}

fn find_from(hay: &[u8], needle: &[u8], from: usize) -> Option<usize> {
    if from >= hay.len() || hay.len() - from < needle.len() {
        return None;
    }
    hay[from..].windows(needle.len()).position(|w| w == needle).map(|i| i + from)
}

fn rect(o: Option<&Obj>) -> Option<[f32; 4]> {
    let a = o?.as_array()?;
    if a.len() < 4 {
        return None;
    }
    let v: Vec<f32> = a.iter().take(4).map(|x| x.as_f32().unwrap_or(0.0)).collect();
    Some([v[0].min(v[2]), v[1].min(v[3]), v[0].max(v[2]), v[1].max(v[3])])
}

impl PdfFile {
    pub fn open(data: Vec<u8>) -> Result<PdfFile, String> {
        if find_from(&data[..data.len().min(1024)], b"%PDF", 0).is_none() {
            return Err(String::from("it isn't a PDF file"));
        }
        let mut f = PdfFile { data, xref: BTreeMap::new(), trailer: Dict::default(), cache: RefCell::new(BTreeMap::new()), crypt: None, encrypt_num: None, pages: Vec::new(), objstm: RefCell::new(BTreeMap::new()) };
        let ok = f.read_xref_chain();
        if !ok || f.trailer.get("Root").is_none() {
            kprintln!("pdf: rebuilding the cross-reference table");
            f.rebuild();
        }
        f.setup_crypt()?;
        f.load_pages();
        if f.pages.is_empty() {
            // Maybe the table was wrong after all.
            f.xref.clear();
            f.cache.borrow_mut().clear();
            f.rebuild();
            f.load_pages();
        }
        if f.pages.is_empty() {
            return Err(String::from("no pages found in it"));
        }
        Ok(f)
    }

    fn read_xref_chain(&mut self) -> bool {
        let tail = self.data.len().saturating_sub(2048);
        let Some(sx) = find_last(&self.data[tail..], b"startxref").map(|i| i + tail) else { return false };
        let mut lx = Lexer::new(&self.data, sx + 9);
        let Some(Obj::Int(mut off)) = lx.obj() else { return false };
        let mut seen = Vec::new();
        let mut first = true;
        while off > 0 && (off as usize) < self.data.len() && !seen.contains(&off) && seen.len() < 100 {
            seen.push(off);
            let Some(trailer) = self.read_xref_at(off as usize) else { return !first };
            if first {
                self.trailer = trailer.clone();
                first = false;
            }
            if let Some(Obj::Int(x)) = trailer.get("XRefStm") {
                self.read_xref_at(*x as usize);
            }
            match trailer.get("Prev") {
                Some(Obj::Int(p)) => off = *p,
                _ => break,
            }
        }
        !first
    }

    /// Read one xref section (table or stream); entries already known win (newer sections
    /// are read first). Returns its trailer dictionary.
    fn read_xref_at(&mut self, off: usize) -> Option<Dict> {
        let mut lx = Lexer::new(&self.data, off);
        lx.skip_ws();
        if self.data[lx.p..].starts_with(b"xref") {
            lx.p += 4;
            loop {
                lx.skip_ws();
                if self.data[lx.p..].starts_with(b"trailer") {
                    lx.p += 7;
                    let Some(Obj::Dict(d)) = lx.obj() else { return None };
                    return Some(d);
                }
                let (Some(Obj::Int(start)), Some(Obj::Int(count))) = (lx.obj(), lx.obj()) else { return None };
                for i in 0..count.max(0) as u32 {
                    lx.skip_ws();
                    // "oooooooooo ggggg n" (often exactly 20 bytes, but be lenient).
                    let (Some(Obj::Int(o)), Some(Obj::Int(_g))) = (lx.obj(), lx.obj()) else { return None };
                    let kind = match lx.tok() {
                        Some(Tok::Word(w)) => w.first().copied().unwrap_or(b'f'),
                        _ => b'f',
                    };
                    let num = start as u32 + i;
                    if kind == b'n' && o > 0 {
                        self.xref.entry(num).or_insert(Entry::At(o as usize));
                    }
                }
            }
        }
        // An xref stream: "n g obj << /Type /XRef ... >> stream".
        let (d, raw) = self.parse_indirect_at(off, None)?;
        let Obj::Stream(d2, _) = &d else { return None };
        if !d2.is("Type", "XRef") {
            return None;
        }
        let dict = d2.clone();
        let (data, _) = filter::decode(&dict, &raw);
        let w: Vec<usize> = dict.get("W")?.as_array()?.iter().map(|o| o.as_i64().unwrap_or(0).clamp(0, 8) as usize).collect();
        if w.len() < 3 {
            return None;
        }
        let size = dict.get("Size").and_then(|o| o.as_i64()).unwrap_or(0);
        let index: Vec<i64> = match dict.get("Index").and_then(|o| o.as_array()) {
            Some(a) => a.iter().map(|o| o.as_i64().unwrap_or(0)).collect(),
            None => Vec::from([0, size]),
        };
        let rec = w[0] + w[1] + w[2];
        let field = |b: &[u8]| b.iter().fold(0u64, |a, &x| a << 8 | x as u64);
        let mut p = 0;
        for pair in index.chunks(2) {
            if pair.len() < 2 {
                break;
            }
            for i in 0..pair[1].max(0) {
                if p + rec > data.len() || rec == 0 {
                    break;
                }
                let r = &data[p..p + rec];
                p += rec;
                let t = if w[0] == 0 { 1 } else { field(&r[..w[0]]) };
                let f2 = field(&r[w[0]..w[0] + w[1]]);
                let f3 = field(&r[w[0] + w[1]..]);
                let num = (pair[0] + i) as u32;
                match t {
                    1 if f2 > 0 => {
                        self.xref.entry(num).or_insert(Entry::At(f2 as usize));
                    }
                    2 => {
                        self.xref.entry(num).or_insert(Entry::InStream(f2 as u32, f3 as u32));
                    }
                    _ => {}
                }
            }
        }
        Some(dict)
    }

    /// Scan the whole file for "n g obj" (for damaged files).
    fn rebuild(&mut self) {
        let d = &self.data;
        let mut i = 0;
        let mut found: Vec<(u32, usize)> = Vec::new();
        while let Some(k) = find_from(d, b" obj", i) {
            i = k + 4;
            // Walk back over "num gen".
            let mut j = k;
            while j > 0 && d[j - 1].is_ascii_digit() {
                j -= 1;
            }
            if j == k || j == 0 || d[j - 1] != b' ' {
                continue;
            }
            let gen_start = j;
            let mut s = j - 1;
            while s > 0 && d[s - 1].is_ascii_digit() {
                s -= 1;
            }
            if s == j - 1 {
                continue;
            }
            let _ = gen_start;
            if s > 0 && !super::obj::is_ws(d[s - 1]) && !super::obj::is_delim(d[s - 1]) {
                continue;
            }
            let num: u32 = core::str::from_utf8(&d[s..j - 1]).ok().and_then(|x| x.parse().ok()).unwrap_or(0);
            if num > 0 {
                found.push((num, s));
            }
        }
        for (num, off) in found {
            self.xref.insert(num, Entry::At(off)); // later copies win
        }
        // Trailer: the last "trailer" dictionary, or an XRef stream's, or find the catalog.
        if let Some(t) = find_last(&self.data, b"trailer") {
            let mut lx = Lexer::new(&self.data, t + 7);
            if let Some(Obj::Dict(dd)) = lx.obj() {
                if dd.get("Root").is_some() {
                    self.trailer = dd;
                }
            }
        }
        if self.trailer.get("Root").is_none() {
            let nums: Vec<u32> = self.xref.keys().copied().collect();
            for n in nums {
                if let Some(Obj::Stream(d, _)) | Some(Obj::Dict(d)) = Some(self.get(n)).filter(|o| matches!(o, Obj::Stream(..) | Obj::Dict(..))) {
                    if d.is("Type", "XRef") && d.get("Root").is_some() {
                        self.trailer = d.clone();
                        break;
                    }
                    if d.is("Type", "Catalog") {
                        self.trailer.set("Root", Obj::Ref(n, 0));
                    }
                }
            }
        }
    }

    fn setup_crypt(&mut self) -> Result<(), String> {
        let Some(enc) = self.trailer.get("Encrypt").cloned() else { return Ok(()) };
        if let Obj::Ref(n, _) = enc {
            self.encrypt_num = Some(n);
        }
        let e = self.resolve(&enc);
        let Some(d) = e.as_dict() else { return Ok(()) };
        if !d.is("Filter", "Standard") {
            return Err(String::from("it's protected in a way Solon can't open"));
        }
        let int = |k: &str, def: i64| d.get(k).and_then(|o| o.as_i64()).unwrap_or(def);
        let bytes = |k: &str| d.get(k).and_then(|o| o.as_str()).map(|s| s.to_vec()).unwrap_or_default();
        let v = int("V", 0);
        let (mut strings, mut streams) = (Method::Rc4, Method::Rc4);
        let mut length = int("Length", 40);
        if v >= 4 {
            let cf = self.resolve(d.get("CF").unwrap_or(&Obj::Null));
            let method_of = |name: Option<&[u8]>| -> (Method, i64) {
                let Some(name) = name else { return (Method::None, 0) };
                if name == b"Identity" {
                    return (Method::None, 0);
                }
                let f = cf.as_dict().and_then(|c| c.get(core::str::from_utf8(name).unwrap_or(""))).and_then(|o| o.as_dict());
                match f.and_then(|f| f.name("CFM")) {
                    Some(b"AESV2") => (Method::Aes, 128),
                    Some(b"AESV3") => (Method::Aes, 256),
                    Some(b"V2") => (Method::Rc4, f.and_then(|f| f.get("Length")).and_then(|o| o.as_i64()).map(|l| if l <= 32 { l * 8 } else { l }).unwrap_or(128)),
                    _ => (Method::None, 0),
                }
            };
            let (s, l1) = method_of(d.name("StrF"));
            let (st, l2) = method_of(d.name("StmF"));
            strings = s;
            streams = st;
            length = l1.max(l2).max(40);
        }
        let id0 = self.trailer.get("ID").and_then(|o| o.as_array()).and_then(|a| a.first()).and_then(|o| o.as_str()).map(|s| s.to_vec()).unwrap_or_default();
        let (o, u, ue) = (bytes("O"), bytes("U"), bytes("UE"));
        let p = Params {

            r: int("R", 2),
            length,
            o: &o,
            u: &u,

            ue: &ue,
            p: int("P", -1),
            id0: &id0,
            encrypt_metadata: !matches!(d.get("EncryptMetadata"), Some(Obj::Bool(false))),
            strings,
            streams,
        };
        match Decryptor::new(&p) {
            Some(dc) => {
                kprintln!("pdf: encrypted (V{} R{}), opened without a password", v, int("R", 2));
                self.crypt = Some(dc);
                self.cache.borrow_mut().clear();
                Ok(())
            }
            None => Err(String::from("it needs a password to open")),
        }
    }

    fn decrypt_obj(&self, o: &mut Obj, num: u32, gen: u16) {
        let Some(c) = &self.crypt else { return };
        match o {
            Obj::Str(s) => *s = c.decrypt(s, num, gen, false),
            Obj::Array(a) => a.iter_mut().for_each(|x| self.decrypt_obj(x, num, gen)),
            Obj::Dict(d) => d.0.iter_mut().for_each(|(_, x)| self.decrypt_obj(x, num, gen)),
            Obj::Stream(d, data) => {
                d.0.iter_mut().for_each(|(_, x)| self.decrypt_obj(x, num, gen));
                if !d.is("Type", "XRef") && !matches!(d.get("Filter"), Some(Obj::Name(n)) if n == b"Crypt") {
                    *data = c.decrypt(data, num, gen, true);
                }
            }
            _ => {}
        }
    }

    /// Parse "n g obj ... endobj" at `off`. Returns (object, raw stream data).
    fn parse_indirect_at(&self, off: usize, want: Option<u32>) -> Option<(Obj, Vec<u8>)> {
        let mut lx = Lexer::new(&self.data, off);
        let Some(Tok::Obj(Obj::Int(n))) = lx.tok() else { return None };
        let Some(Tok::Obj(Obj::Int(_))) = lx.tok() else { return None };
        let Some(Tok::Word(b"obj")) = lx.tok() else { return None };
        if let Some(w) = want {
            if n as u32 != w {
                return None;
            }
        }
        let o = lx.obj().unwrap_or(Obj::Null);
        let Obj::Dict(d) = o else { return Some((o, Vec::new())) };
        match lx.tok() {
            Some(Tok::Word(b"stream")) => {}
            _ => {
                return Some((Obj::Dict(d), Vec::new()));
            }
        }
        let mut start = lx.p;
        if self.data.get(start) == Some(&b'\r') {
            start += 1;
        }
        if self.data.get(start) == Some(&b'\n') {
            start += 1;
        }
        // Trust /Length when "endstream" follows it; otherwise search.
        let len = match d.get("Length") {
            Some(Obj::Int(l)) => Some(*l as usize),
            Some(Obj::Ref(r, _)) if Some(*r) != want => match self.get(*r) {
                Obj::Int(l) => Some(l as usize),
                _ => None,
            },
            _ => None,
        };
        let end = match len {
            Some(l) if start + l <= self.data.len() && find_from(&self.data[start + l..(start + l + 32).min(self.data.len())], b"endstream", 0).is_some() => start + l,
            _ => {
                let e = find_from(&self.data, b"endstream", start).unwrap_or(self.data.len());
                let mut e2 = e;
                while e2 > start && matches!(self.data[e2 - 1], b'\r' | b'\n') {
                    e2 -= 1;
                }
                e2
            }
        };
        let raw = self.data[start..end].to_vec();
        Some((Obj::Stream(d, Vec::new()), raw))
    }

    /// An object by number (Null if missing).
    pub fn get(&self, num: u32) -> Obj {
        if let Some(o) = self.cache.borrow().get(&num) {
            return o.clone();
        }
        let o = match self.xref.get(&num).copied() {
            Some(Entry::At(off)) => match self.parse_indirect_at(off, Some(num)) {
                Some((Obj::Stream(d, _), raw)) => {
                    let mut o = Obj::Stream(d, raw);
                    if Some(num) != self.encrypt_num {
                        self.decrypt_obj(&mut o, num, 0);
                    }
                    o
                }
                Some((mut o, _)) => {
                    if Some(num) != self.encrypt_num {
                        self.decrypt_obj(&mut o, num, 0);
                    }
                    o
                }
                None => Obj::Null,
            },
            Some(Entry::InStream(sn, idx)) => self.from_objstm(sn, idx, num),
            None => Obj::Null,
        };
        self.cache.borrow_mut().insert(num, o.clone());
        o
    }

    fn from_objstm(&self, sn: u32, idx: u32, num: u32) -> Obj {
        if !self.objstm.borrow().contains_key(&sn) {
            let s = self.get(sn);
            let Obj::Stream(d, raw) = s else { return Obj::Null };
            let (data, _) = filter::decode(&d, &raw);
            let n = d.get("N").and_then(|o| o.as_i64()).unwrap_or(0).clamp(0, 1_000_000) as usize;
            let first = d.get("First").and_then(|o| o.as_i64()).unwrap_or(0).max(0) as usize;
            let mut lx = Lexer::new(&data, 0);
            let mut offs = Vec::with_capacity(n);
            for _ in 0..n {
                match (lx.obj(), lx.obj()) {
                    (Some(Obj::Int(a)), Some(Obj::Int(b))) => offs.push((a as u32, first + b as usize)),
                    _ => break,
                }
            }
            self.objstm.borrow_mut().insert(sn, (offs, data));
        }
        let m = self.objstm.borrow();
        let (offs, data) = &m[&sn];
        let at = offs.get(idx as usize).filter(|(n2, _)| *n2 == num).or_else(|| offs.iter().find(|(n2, _)| *n2 == num));
        match at {
            Some(&(_, off)) if off < data.len() => Lexer::new(data, off).obj().unwrap_or(Obj::Null),
            _ => Obj::Null,
        }
    }

    /// Follow references.
    pub fn resolve(&self, o: &Obj) -> Obj {
        let mut cur = o.clone();
        for _ in 0..16 {
            match cur {
                Obj::Ref(n, _) => cur = self.get(n),
                _ => return cur,
            }
        }
        Obj::Null
    }

    /// A dictionary entry, with references followed.
    pub fn dget(&self, d: &Dict, k: &str) -> Obj {
        match d.get(k) {
            Some(o) => self.resolve(o),
            None => Obj::Null,
        }
    }

    /// A stream's data with the general filters undone (image filters left in place).
    pub fn stream_data(&self, o: &Obj) -> Option<(Dict, Vec<u8>)> {
        match self.resolve(o) {
            Obj::Stream(d, raw) => {
                let mut d2 = d.clone();
                // Resolve indirect filter parameters.
                for k in ["Filter", "DecodeParms"] {
                    if let Some(v) = d.get(k) {
                        let r = self.resolve(v);
                        d2.set(k, r);
                    }
                }
                let (data, _) = filter::decode(&d2, &raw);
                Some((d2, data))
            }
            _ => None,
        }
    }

    fn load_pages(&mut self) {
        let root = self.dget(&self.trailer.clone(), "Root");
        let Some(cat) = root.as_dict() else { return };
        let pages_ref = cat.get("Pages").cloned().unwrap_or(Obj::Null);
        let mut out = Vec::new();
        let mut visited = Vec::new();
        self.walk(&pages_ref, Dict::default(), None, None, 0, &mut out, &mut visited, 0);
        self.pages = out;
        kprintln!("pdf: {} page(s), {} objects", self.pages.len(), self.xref.len());
    }

    #[allow(clippy::too_many_arguments)]
    fn walk(&self, node: &Obj, res: Dict, media: Option<[f32; 4]>, crop: Option<[f32; 4]>, rotate: i32, out: &mut Vec<Page>, visited: &mut Vec<u32>, depth: u32) {
        if depth > 40 || out.len() > 20000 {
            return;
        }
        let id = if let Obj::Ref(n, _) = node { *n } else { 0 };
        if id != 0 {
            if visited.contains(&id) {
                return;
            }
            visited.push(id);
        }
        let o = self.resolve(node);
        let Some(d) = o.as_dict() else { return };
        let res = match self.dget(d, "Resources") {
            Obj::Dict(r) => r,
            _ => res,
        };
        let media = rect(Some(&self.dget(d, "MediaBox"))).or(media);
        let crop = rect(Some(&self.dget(d, "CropBox"))).or(crop);
        let rotate = self.dget(d, "Rotate").as_i64().map(|r| r as i32).unwrap_or(rotate);
        let is_pages = d.is("Type", "Pages") || (d.get("Kids").is_some() && !d.is("Type", "Page"));
        if is_pages {
            if let Obj::Array(kids) = self.dget(d, "Kids") {
                for k in &kids {
                    self.walk(k, res.clone(), media, crop, rotate, out, visited, depth + 1);
                }
            }
        } else {
            let m = media.unwrap_or([0.0, 0.0, 612.0, 792.0]);
            let mut c = crop.unwrap_or(m);
            // The crop box can't stick out of the media box.
            c = [c[0].max(m[0]), c[1].max(m[1]), c[2].min(m[2]), c[3].min(m[3])];
            if c[2] - c[0] < 1.0 || c[3] - c[1] < 1.0 {
                c = m;
            }
            out.push(Page { id, dict: d.clone(), resources: res, media: m, crop: c, rotate: rotate.rem_euclid(360) });
        }
    }

    /// Does the file need a password (we got this far, so no), and was it encrypted?
    pub fn encrypted(&self) -> bool {
        self.crypt.is_some()
    }

    pub fn title(&self) -> Option<String> {
        let info = self.dget(&self.trailer, "Info");
        let t = info.as_dict().map(|d| self.dget(d, "Title"))?;
        let s = super::obj::text_string(t.as_str()?);
        if s.trim().is_empty() {
            None
        } else {
            Some(s)
        }
    }
}

#[allow(dead_code)]
pub fn describe(f: &PdfFile) -> String {
    format!("{} pages", f.pages.len())
}

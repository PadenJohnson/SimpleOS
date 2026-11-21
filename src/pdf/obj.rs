//! PDF objects and the parser for them (shared by files and content streams).

use alloc::string::String;
use alloc::vec::Vec;

#[derive(Clone, Debug, PartialEq)]
pub enum Obj {
    Null,
    Bool(bool),
    Int(i64),
    Real(f32),
    Name(Vec<u8>),
    Str(Vec<u8>),
    Array(Vec<Obj>),
    Dict(Dict),
    /// Dictionary and raw (still encoded) data.
    Stream(Dict, Vec<u8>),
    Ref(u32, u16),
}

#[derive(Clone, Debug, PartialEq, Default)]
pub struct Dict(pub Vec<(Vec<u8>, Obj)>);

impl Dict {
    pub fn get(&self, k: &str) -> Option<&Obj> {
        self.0.iter().find(|(n, _)| n.as_slice() == k.as_bytes()).map(|(_, v)| v)
    }
    pub fn set(&mut self, k: &str, v: Obj) {
        match self.0.iter_mut().find(|(n, _)| n.as_slice() == k.as_bytes()) {
            Some(e) => e.1 = v,
            None => self.0.push((Vec::from(k.as_bytes()), v)),
        }
    }
    pub fn remove(&mut self, k: &str) {
        self.0.retain(|(n, _)| n.as_slice() != k.as_bytes());
    }
    pub fn name(&self, k: &str) -> Option<&[u8]> {
        match self.get(k) {
            Some(Obj::Name(n)) => Some(n),
            _ => None,
        }
    }
    pub fn is(&self, k: &str, v: &str) -> bool {
        self.name(k) == Some(v.as_bytes())
    }
}

impl Obj {
    pub fn as_f32(&self) -> Option<f32> {
        match self {
            Obj::Int(i) => Some(*i as f32),
            Obj::Real(r) => Some(*r),
            _ => None,
        }
    }
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Obj::Int(i) => Some(*i),
            Obj::Real(r) => Some(*r as i64),
            _ => None,
        }
    }
    pub fn as_name(&self) -> Option<&[u8]> {
        match self {
            Obj::Name(n) => Some(n),
            _ => None,
        }
    }
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Obj::Dict(d) | Obj::Stream(d, _) => Some(d),
            _ => None,
        }
    }
    pub fn as_array(&self) -> Option<&[Obj]> {
        match self {
            Obj::Array(a) => Some(a),
            _ => None,
        }
    }
    pub fn as_str(&self) -> Option<&[u8]> {
        match self {
            Obj::Str(s) => Some(s),
            _ => None,
        }
    }
    pub fn name_is(&self, n: &str) -> bool {
        self.as_name() == Some(n.as_bytes())
    }
}

pub fn is_ws(c: u8) -> bool {
    matches!(c, b' ' | b'\t' | b'\r' | b'\n' | 0x0C | 0)
}
pub fn is_delim(c: u8) -> bool {
    matches!(c, b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%')
}

/// A token from the stream of bytes.
#[derive(Debug, PartialEq)]
pub enum Tok<'a> {
    Obj(Obj),
    /// A bare word: operator or keyword (obj, endobj, stream, R, ...).
    Word(&'a [u8]),
    ArrayStart,
    ArrayEnd,
    DictStart,
    DictEnd,
}

pub struct Lexer<'a> {
    pub b: &'a [u8],
    pub p: usize,
}

impl<'a> Lexer<'a> {
    pub fn new(b: &'a [u8], p: usize) -> Lexer<'a> {
        Lexer { b, p }
    }

    pub fn skip_ws(&mut self) {
        while self.p < self.b.len() {
            let c = self.b[self.p];
            if is_ws(c) {
                self.p += 1;
            } else if c == b'%' {
                while self.p < self.b.len() && self.b[self.p] != b'\n' && self.b[self.p] != b'\r' {
                    self.p += 1;
                }
            } else {
                break;
            }
        }
    }

    pub fn tok(&mut self) -> Option<Tok<'a>> {
        self.skip_ws();
        let b = self.b;
        let c = *b.get(self.p)?;
        match c {
            b'[' => {
                self.p += 1;
                Some(Tok::ArrayStart)
            }
            b']' => {
                self.p += 1;
                Some(Tok::ArrayEnd)
            }
            b'<' if b.get(self.p + 1) == Some(&b'<') => {
                self.p += 2;
                Some(Tok::DictStart)
            }
            b'>' if b.get(self.p + 1) == Some(&b'>') => {
                self.p += 2;
                Some(Tok::DictEnd)
            }
            b'<' => {
                self.p += 1;
                let mut out = Vec::new();
                let mut hi: Option<u8> = None;
                while self.p < b.len() && b[self.p] != b'>' {
                    let d = match b[self.p] {
                        x @ b'0'..=b'9' => Some(x - b'0'),
                        x @ b'a'..=b'f' => Some(x - b'a' + 10),
                        x @ b'A'..=b'F' => Some(x - b'A' + 10),
                        _ => None,
                    };
                    if let Some(d) = d {
                        match hi.take() {
                            Some(h) => out.push(h << 4 | d),
                            None => hi = Some(d),
                        }
                    }
                    self.p += 1;
                }
                if let Some(h) = hi {
                    out.push(h << 4);
                }
                self.p += 1;
                Some(Tok::Obj(Obj::Str(out)))
            }
            b'(' => {
                self.p += 1;
                let mut out = Vec::new();
                let mut depth = 1;
                while self.p < b.len() {
                    let c = b[self.p];
                    self.p += 1;
                    match c {
                        b'\\' => {
                            let Some(&e) = b.get(self.p) else { break };
                            self.p += 1;
                            match e {
                                b'n' => out.push(b'\n'),
                                b'r' => out.push(b'\r'),
                                b't' => out.push(b'\t'),
                                b'b' => out.push(8),
                                b'f' => out.push(12),
                                b'0'..=b'7' => {
                                    let mut v = (e - b'0') as u32;
                                    for _ in 0..2 {
                                        match b.get(self.p) {
                                            Some(&d @ b'0'..=b'7') => {
                                                v = v * 8 + (d - b'0') as u32;
                                                self.p += 1;
                                            }
                                            _ => break,
                                        }
                                    }
                                    out.push(v as u8);
                                }
                                b'\r' => {
                                    if b.get(self.p) == Some(&b'\n') {
                                        self.p += 1;
                                    }
                                }
                                b'\n' => {}
                                x => out.push(x),
                            }
                        }
                        b'(' => {
                            depth += 1;
                            out.push(c);
                        }
                        b')' => {
                            depth -= 1;
                            if depth == 0 {
                                break;
                            }
                            out.push(c);
                        }
                        _ => out.push(c),
                    }
                }
                Some(Tok::Obj(Obj::Str(out)))
            }
            b'/' => {
                self.p += 1;
                let mut out = Vec::new();
                while self.p < b.len() && !is_ws(b[self.p]) && !is_delim(b[self.p]) {
                    if b[self.p] == b'#' && self.p + 2 < b.len() {
                        if let Ok(v) = u8::from_str_radix(core::str::from_utf8(&b[self.p + 1..self.p + 3]).unwrap_or("zz"), 16) {
                            out.push(v);
                            self.p += 3;
                            continue;
                        }
                    }
                    out.push(b[self.p]);
                    self.p += 1;
                }
                Some(Tok::Obj(Obj::Name(out)))
            }
            b'+' | b'-' | b'.' | b'0'..=b'9' => {
                let s = self.p;
                self.p += 1;
                while self.p < b.len() && matches!(b[self.p], b'0'..=b'9' | b'.' | b'-' | b'+' | b'e' | b'E') {
                    self.p += 1;
                }
                Some(Tok::Obj(parse_num(&b[s..self.p])))
            }
            b'{' | b'}' => {
                self.p += 1;
                Some(Tok::Word(&b[self.p - 1..self.p]))
            }
            b')' | b'>' => {
                self.p += 1;
                self.tok()
            }
            _ => {
                let s = self.p;
                while self.p < b.len() && !is_ws(b[self.p]) && !is_delim(b[self.p]) {
                    self.p += 1;
                }
                let w = &b[s..self.p];
                Some(match w {
                    b"true" => Tok::Obj(Obj::Bool(true)),
                    b"false" => Tok::Obj(Obj::Bool(false)),
                    b"null" => Tok::Obj(Obj::Null),
                    _ => Tok::Word(w),
                })
            }
        }
    }

    /// Parse one object (arrays and dictionaries in full; `a b R` as a reference).
    /// Streams aren't handled here (the file reader does that after the dictionary).
    pub fn obj(&mut self) -> Option<Obj> {
        self.obj_depth(0)
    }

    fn obj_depth(&mut self, depth: u32) -> Option<Obj> {
        if depth > 64 {
            return None;
        }
        match self.tok()? {
            Tok::Obj(Obj::Int(n)) => {
                // Maybe "n g R".
                let save = self.p;
                if let Some(Tok::Obj(Obj::Int(g))) = self.tok() {
                    let save2 = self.p;
                    if let Some(Tok::Word(b"R")) = self.tok() {
                        return Some(Obj::Ref(n.max(0) as u32, g.max(0) as u16));
                    }
                    self.p = save2;
                }
                self.p = save;
                Some(Obj::Int(n))
            }
            Tok::Obj(o) => Some(o),
            Tok::ArrayStart => {
                let mut v = Vec::new();
                loop {
                    self.skip_ws();
                    if self.b.get(self.p) == Some(&b']') {
                        self.p += 1;
                        break;
                    }
                    if self.p >= self.b.len() {
                        break;
                    }
                    let save = self.p;
                    match self.obj_depth(depth + 1) {
                        Some(o) => v.push(o),
                        None => {
                            if self.p == save {
                                self.p += 1;
                            }
                        }
                    }
                }
                Some(Obj::Array(v))
            }
            Tok::DictStart => Some(Obj::Dict(self.dict_body(depth)?)),
            Tok::Word(_) => None,
            Tok::ArrayEnd | Tok::DictEnd => None,
        }
    }

    /// After "<<": read key/value pairs up to ">>".
    pub fn dict_body(&mut self, depth: u32) -> Option<Dict> {
        let mut d = Dict::default();
        loop {
            self.skip_ws();
            if self.p >= self.b.len() {
                break;
            }
            if self.b[self.p..].starts_with(b">>") {
                self.p += 2;
                break;
            }
            let save = self.p;
            match self.tok() {
                Some(Tok::Obj(Obj::Name(k))) => {
                    let v = self.obj_depth(depth + 1).unwrap_or(Obj::Null);
                    d.0.push((k, v));
                }
                Some(Tok::DictEnd) => break,
                None => break,
                _ => {
                    if self.p == save {
                        self.p += 1;
                    }
                }
            }
        }
        Some(d)
    }
}

pub fn parse_num(s: &[u8]) -> Obj {
    let st = core::str::from_utf8(s).unwrap_or("0");
    if !st.contains(['.', 'e', 'E']) {
        if let Ok(i) = st.parse::<i64>() {
            return Obj::Int(i);
        }
    }
    // Hand-rolled: PDFs have odd numbers like "--5" or "0.0.1".
    let mut neg = false;
    let mut i = 0;
    let b = s;
    while i < b.len() && (b[i] == b'-' || b[i] == b'+') {
        neg ^= b[i] == b'-';
        i += 1;
    }
    let mut v: f64 = 0.0;
    while i < b.len() && b[i].is_ascii_digit() {
        v = v * 10.0 + (b[i] - b'0') as f64;
        i += 1;
    }
    if i < b.len() && b[i] == b'.' {
        i += 1;
        let mut scale = 0.1;
        while i < b.len() && b[i].is_ascii_digit() {
            v += (b[i] - b'0') as f64 * scale;
            scale *= 0.1;
            i += 1;
        }
    }
    let v = if neg { -v } else { v };
    if !st.contains(['.', 'e', 'E']) {
        return Obj::Int(v as i64);
    }
    Obj::Real(v as f32)
}

/// Text strings (titles etc.): UTF-16 with a BOM, or PDFDocEncoding (close to Latin-1).
pub fn text_string(s: &[u8]) -> String {
    if s.len() >= 2 && s[0] == 0xFE && s[1] == 0xFF {
        let u: Vec<u16> = s[2..].chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect();
        return char::decode_utf16(u).map(|r| r.unwrap_or('?')).collect();
    }
    if s.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return String::from_utf8_lossy(&s[3..]).into_owned();
    }
    s.iter().map(|&b| b as char).collect()
}

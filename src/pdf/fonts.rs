//! Fonts inside PDFs: which glyph each character code means, how wide it is, and its outline.
//! Embedded TrueType/OpenType and CFF programs are drawn as-is; Type 1 programs too
//! (pdf/type1.rs); Type 3 fonts are drawn from their own content streams; fonts that
//! aren't embedded are stood in for by Noto Sans or Serif (matching bold/italic).

use alloc::collections::BTreeMap;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::file::PdfFile;
use super::obj::{Dict, Obj};
use crate::font::Face;
use ttf_parser::{GlyphId, OutlineBuilder};

// ---- encodings --------------------------------------------------------------------------------

const CP1252_HI: [u16; 32] = [
    0x20AC, 0x2022, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x2022, 0x017D, 0x2022, 0x2022, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x2022, 0x017E, 0x0178,
];

const MAC_HI: [u16; 128] = [
    0xC4, 0xC5, 0xC7, 0xC9, 0xD1, 0xD6, 0xDC, 0xE1, 0xE0, 0xE2, 0xE4, 0xE3, 0xE5, 0xE7, 0xE9, 0xE8, 0xEA, 0xEB, 0xED, 0xEC, 0xEE, 0xEF, 0xF1, 0xF3, 0xF2, 0xF4, 0xF6, 0xF5, 0xFA, 0xF9, 0xFB, 0xFC, 0x2020, 0xB0, 0xA2, 0xA3, 0xA7, 0x2022, 0xB6, 0xDF, 0xAE, 0xA9, 0x2122, 0xB4, 0xA8, 0x2260, 0xC6, 0xD8, 0x221E, 0xB1, 0x2264, 0x2265, 0xA5, 0xB5, 0x2202, 0x2211, 0x220F, 0x3C0, 0x222B, 0xAA, 0xBA, 0x3A9, 0xE6, 0xF8, 0xBF, 0xA1, 0xAC, 0x221A, 0x192, 0x2248, 0x2206, 0xAB, 0xBB, 0x2026, 0xA0, 0xC0, 0xC3, 0xD5, 0x152, 0x153, 0x2013, 0x2014, 0x201C, 0x201D,
    0x2018, 0x2019, 0xF7, 0x25CA, 0xFF, 0x178, 0x2044, 0x20AC, 0x2039, 0x203A, 0xFB01, 0xFB02, 0x2021, 0xB7, 0x201A, 0x201E, 0x2030, 0xC2, 0xCA, 0xC1, 0xCB, 0xC8, 0xCD, 0xCE, 0xCF, 0xCC, 0xD3, 0xD4, 0xF8FF, 0xD2, 0xDA, 0xDB, 0xD9, 0x131, 0x2C6, 0x2DC, 0xAF, 0x2D8, 0x2D9, 0x2DA, 0xB8, 0x2DD, 0x2DB, 0x2C7,
];

/// Adobe StandardEncoding's upper half (0 = unused).
const STD_HI: [(u8, u16); 51] = [
    (0xA1, 0xA1), (0xA2, 0xA2), (0xA3, 0xA3), (0xA4, 0x2044), (0xA5, 0xA5), (0xA6, 0x192), (0xA7, 0xA7), (0xA8, 0xA4), (0xA9, 0x27), (0xAA, 0x201C), (0xAB, 0xAB), (0xAC, 0x2039), (0xAD, 0x203A), (0xAE, 0xFB01), (0xAF, 0xFB02), (0xB1, 0x2013), (0xB2, 0x2020), (0xB3, 0x2021), (0xB4, 0xB7), (0xB6, 0xB6), (0xB7, 0x2022), (0xB8, 0x201A), (0xB9, 0x201E), (0xBA, 0x201D), (0xBB, 0xBB), (0xBC, 0x2026), (0xBD, 0x2030), (0xBF, 0xBF), (0xC1, 0x60), (0xC2, 0xB4), (0xC3, 0x2C6), (0xC4, 0x2DC), (0xC5, 0xAF), (0xC6, 0x2D8), (0xC7, 0x2D9), (0xC8, 0xA8), (0xCA, 0x2DA), (0xCB, 0xB8), (0xCD, 0x2DD), (0xCE, 0x2DB), (0xCF, 0x2C7), (0xD0, 0x2014), (0xE1, 0xC6), (0xE3, 0xAA), (0xE8, 0x141), (0xE9, 0xD8), (0xEA, 0x152), (0xEB, 0xBA), (0xF1, 0xE6), (0xF5, 0x131), (0xF8, 0x142),
];
const STD_HI2: [(u8, u16); 3] = [(0xF9, 0xF8), (0xFA, 0x153), (0xFB, 0xDF)];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Base {
    Standard,
    WinAnsi,
    MacRoman,
    Symbol,
}

fn base_char(b: Base, code: u8) -> Option<char> {
    let u = match b {
        Base::WinAnsi => match code {
            0x80..=0x9F => CP1252_HI[(code - 0x80) as usize] as u32,
            0x20..=0x7E | 0xA0..=0xFF => code as u32,
            _ => return None,
        },
        Base::MacRoman => match code {
            0x80..=0xFF => MAC_HI[(code - 0x80) as usize] as u32,
            0x20..=0x7E => code as u32,
            _ => return None,
        },
        Base::Standard => match code {
            0x27 => 0x2019,
            0x60 => 0x2018,
            0x20..=0x7E => code as u32,
            _ => STD_HI.iter().chain(STD_HI2.iter()).find(|(c, _)| *c == code).map(|(_, u)| *u as u32)?,
        },
        Base::Symbol => match code {
            0x20..=0x7E => code as u32,
            _ => return None,
        },
    };
    char::from_u32(u)
}

/// Glyph names that aren't just their character (the rest are worked out below).
const NAMES: &[(&str, u16)] = &[
    ("space", 0x20), ("exclam", 0x21), ("quotedbl", 0x22), ("numbersign", 0x23), ("dollar", 0x24), ("percent", 0x25), ("ampersand", 0x26), ("quotesingle", 0x27), ("quoteright", 0x2019), ("parenleft", 0x28), ("parenright", 0x29), ("asterisk", 0x2A), ("plus", 0x2B), ("comma", 0x2C), ("hyphen", 0x2D), ("period", 0x2E), ("slash", 0x2F),
    ("zero", 0x30), ("one", 0x31), ("two", 0x32), ("three", 0x33), ("four", 0x34), ("five", 0x35), ("six", 0x36), ("seven", 0x37), ("eight", 0x38), ("nine", 0x39), ("colon", 0x3A), ("semicolon", 0x3B), ("less", 0x3C), ("equal", 0x3D), ("greater", 0x3E), ("question", 0x3F), ("at", 0x40),
    ("bracketleft", 0x5B), ("backslash", 0x5C), ("bracketright", 0x5D), ("asciicircum", 0x5E), ("underscore", 0x5F), ("grave", 0x60), ("quoteleft", 0x2018), ("braceleft", 0x7B), ("bar", 0x7C), ("braceright", 0x7D), ("asciitilde", 0x7E),
    ("exclamdown", 0xA1), ("cent", 0xA2), ("sterling", 0xA3), ("currency", 0xA4), ("yen", 0xA5), ("brokenbar", 0xA6), ("section", 0xA7), ("dieresis", 0xA8), ("copyright", 0xA9), ("ordfeminine", 0xAA), ("guillemotleft", 0xAB), ("logicalnot", 0xAC), ("registered", 0xAE), ("macron", 0xAF), ("degree", 0xB0), ("plusminus", 0xB1), ("twosuperior", 0xB2), ("threesuperior", 0xB3), ("acute", 0xB4), ("mu", 0xB5), ("paragraph", 0xB6), ("periodcentered", 0xB7), ("cedilla", 0xB8), ("onesuperior", 0xB9), ("ordmasculine", 0xBA), ("guillemotright", 0xBB), ("onequarter", 0xBC), ("onehalf", 0xBD), ("threequarters", 0xBE), ("questiondown", 0xBF),
    ("AE", 0xC6), ("Eth", 0xD0), ("multiply", 0xD7), ("Oslash", 0xD8), ("Thorn", 0xDE), ("germandbls", 0xDF), ("ae", 0xE6), ("eth", 0xF0), ("divide", 0xF7), ("oslash", 0xF8), ("thorn", 0xFE), ("dotlessi", 0x131), ("Lslash", 0x141), ("lslash", 0x142), ("OE", 0x152), ("oe", 0x153), ("florin", 0x192),
    ("circumflex", 0x2C6), ("caron", 0x2C7), ("breve", 0x2D8), ("dotaccent", 0x2D9), ("ring", 0x2DA), ("ogonek", 0x2DB), ("tilde", 0x2DC), ("hungarumlaut", 0x2DD),
    ("endash", 0x2013), ("emdash", 0x2014), ("quotesinglbase", 0x201A), ("quotedblleft", 0x201C), ("quotedblright", 0x201D), ("quotedblbase", 0x201E), ("dagger", 0x2020), ("daggerdbl", 0x2021), ("bullet", 0x2022), ("ellipsis", 0x2026), ("perthousand", 0x2030), ("guilsinglleft", 0x2039), ("guilsinglright", 0x203A), ("fraction", 0x2044), ("Euro", 0x20AC), ("trademark", 0x2122), ("minus", 0x2212),
    ("fi", 0xFB01), ("fl", 0xFB02), ("ff", 0xFB00), ("ffi", 0xFB03), ("ffl", 0xFB04), ("nbspace", 0xA0), ("sfthyphen", 0xAD), ("arrowright", 0x2192), ("arrowleft", 0x2190), ("arrowup", 0x2191), ("arrowdown", 0x2193), ("checkmark", 0x2713), ("infinity", 0x221E), ("lessequal", 0x2264), ("greaterequal", 0x2265), ("notequal", 0x2260), ("approxequal", 0x2248), ("summation", 0x2211), ("product", 0x220F), ("radical", 0x221A), ("integral", 0x222B), ("partialdiff", 0x2202), ("pi", 0x3C0), ("alpha", 0x3B1), ("beta", 0x3B2), ("gamma", 0x3B3), ("delta", 0x3B4), ("epsilon", 0x3B5), ("theta", 0x3B8), ("lambda", 0x3BB), ("sigma", 0x3C3), ("omega", 0x3C9), ("Omega", 0x3A9), ("Delta", 0x394),
];

/// Latin letters with accents: base letter + accent name.
const ACCENTS: &[(&str, &str)] = &[
    ("grave", "\u{300}"), ("acute", "\u{301}"), ("circumflex", "\u{302}"), ("tilde", "\u{303}"), ("dieresis", "\u{308}"), ("ring", "\u{30A}"), ("cedilla", "\u{327}"), ("caron", "\u{30C}"), ("macron", "\u{304}"), ("breve", "\u{306}"), ("ogonek", "\u{328}"), ("dotaccent", "\u{307}"), ("hungarumlaut", "\u{30B}"),
];

const LATIN: &[(char, [char; 6])] = &[
    ('A', ['À', 'Á', 'Â', 'Ã', 'Ä', 'Å']),
    ('E', ['È', 'É', 'Ê', '\0', 'Ë', '\0']),
    ('I', ['Ì', 'Í', 'Î', '\0', 'Ï', '\0']),
    ('O', ['Ò', 'Ó', 'Ô', 'Õ', 'Ö', '\0']),
    ('U', ['Ù', 'Ú', 'Û', '\0', 'Ü', '\0']),
    ('Y', ['\0', 'Ý', '\0', '\0', 'Ÿ', '\0']),
    ('a', ['à', 'á', 'â', 'ã', 'ä', 'å']),
    ('e', ['è', 'é', 'ê', '\0', 'ë', '\0']),
    ('i', ['ì', 'í', 'î', '\0', 'ï', '\0']),
    ('o', ['ò', 'ó', 'ô', 'õ', 'ö', '\0']),
    ('u', ['ù', 'ú', 'û', '\0', 'ü', '\0']),
    ('y', ['\0', 'ý', '\0', '\0', 'ÿ', '\0']),
];

fn compose(base: char, accent: &str) -> Option<char> {
    let order = ["grave", "acute", "circumflex", "tilde", "dieresis", "ring"];
    if let Some(k) = order.iter().position(|a| *a == accent) {
        if let Some((_, v)) = LATIN.iter().find(|(b, _)| *b == base) {
            if v[k] != '\0' {
                return Some(v[k]);
            }
        }
    }
    match (base, accent) {
        ('C', "cedilla") => Some('Ç'),
        ('c', "cedilla") => Some('ç'),
        ('N', "tilde") => Some('Ñ'),
        ('n', "tilde") => Some('ñ'),
        ('S', "caron") => Some('Š'),
        ('s', "caron") => Some('š'),
        ('Z', "caron") => Some('Ž'),
        ('z', "caron") => Some('ž'),
        _ => None,
    }
}

pub fn name_to_char(name: &str) -> Option<char> {
    let name = name.split('.').next().unwrap_or(name);
    let name = name.split('_').next().unwrap_or(name);
    if name.chars().count() == 1 {
        return name.chars().next();
    }
    if let Some(&(_, u)) = NAMES.iter().find(|(n, _)| *n == name) {
        return char::from_u32(u as u32);
    }
    if let Some(h) = name.strip_prefix("uni") {
        if h.len() >= 4 {
            return u32::from_str_radix(&h[..4], 16).ok().and_then(char::from_u32);
        }
    }
    if let Some(h) = name.strip_prefix('u') {
        if (4..=6).contains(&h.len()) {
            if let Ok(v) = u32::from_str_radix(h, 16) {
                return char::from_u32(v);
            }
        }
    }
    let mut cs = name.chars();
    let first = cs.next()?;
    let rest: String = cs.collect();
    if first.is_ascii_alphabetic() {
        if let Some(c) = compose(first, &rest) {
            return Some(c);
        }
        if ACCENTS.iter().any(|(a, _)| *a == rest) {
            return Some(first);
        }
    }
    None
}

fn char_to_name(c: char) -> Option<String> {
    if c.is_ascii_alphabetic() {
        return Some(String::from(c));
    }
    NAMES.iter().find(|(_, u)| *u as u32 == c as u32).map(|(n, _)| String::from(*n))
}

// ---- the font -------------------------------------------------------------------------------

pub enum Prog {
    /// TrueType/OpenType data (parsed on use).
    Sfnt(Vec<u8>),
    /// A bare CFF font.
    Cff(Vec<u8>),
    Type1(super::type1::Type1),
    /// Not embedded: a Noto stand-in.
    Noto(Face),
}

pub struct Type3 {
    pub matrix: [f32; 6],
    pub procs: BTreeMap<u32, Obj>,
    pub resources: Option<Dict>,
}

pub struct PdfFont {
    pub prog: Prog,
    pub two_byte: bool,
    /// Code -> CID for CID fonts with a custom CMap (code lengths from the codespace).
    cmap: Option<Vec<(u32, u32, u32, u8)>>,
    cid_to_gid: Option<Vec<u16>>,
    /// Simple fonts: glyph for each code (resolved when loading).
    code_gid: Vec<u16>,
    /// Unicode for each simple-font code (for the Noto stand-in).
    code_char: Vec<Option<char>>,
    /// Widths in 1/1000 em.
    first: u32,
    widths: Vec<f32>,
    cid_widths: BTreeMap<u32, f32>,
    default_w: f32,
    /// Font units per em of the program (1000 for Type 1 and CFF).
    pub upem: f32,
    pub type3: Option<Type3>,
    /// CFF font matrix scale (glyph units to em), if not 1/1000.
    cff_scale: f32,
}

fn noto_for(name: &str, flags: i64) -> Face {
    let n = name.to_ascii_lowercase();
    let bold = n.contains("bold") || n.contains("black") || n.contains("heavy") || n.contains("semibold") || n.contains(",bold");
    let italic = n.contains("italic") || n.contains("oblique") || flags & 64 != 0;
    let serif = (n.contains("times") || n.contains("serif") || n.contains("georgia") || n.contains("garamond") || n.contains("cambria") || n.contains("book") || n.contains("roman") || n.contains("minion") || n.contains("palatino") || n.contains("courier") || flags & 2 != 0) && !n.contains("sans") && !n.contains("arial") && !n.contains("helvetica");
    Face::new(serif, bold, italic)
}

impl PdfFont {
    pub fn load(file: &PdfFile, fd: &Dict) -> PdfFont {
        let subtype = fd.name("Subtype").unwrap_or(b"Type1").to_vec();
        let base_name = String::from_utf8_lossy(fd.name("BaseFont").unwrap_or(b"")).into_owned();
        let mut f = PdfFont {
            prog: Prog::Noto(Face::Sans),
            two_byte: false,
            cmap: None,
            cid_to_gid: None,
            code_gid: vec![0; 256],
            code_char: vec![None; 256],
            first: 0,
            widths: Vec::new(),
            cid_widths: BTreeMap::new(),
            default_w: 500.0,
            upem: 1000.0,
            type3: None,
            cff_scale: 1.0,
        };
        if subtype == b"Type0" {
            f.two_byte = true;
            f.default_w = 1000.0;
            let desc = match file.dget(fd, "DescendantFonts") {
                Obj::Array(a) => a.first().map(|o| file.resolve(o)).unwrap_or(Obj::Null),
                _ => Obj::Null,
            };
            let cd = desc.as_dict().cloned().unwrap_or_default();
            if let Some(dw) = file.dget(&cd, "DW").as_f32() {
                f.default_w = dw;
            }
            if let Obj::Array(w) = file.dget(&cd, "W") {
                let w: Vec<Obj> = w.iter().map(|o| file.resolve(o)).collect();
                let mut i = 0;
                while i + 1 < w.len() {
                    let c0 = w[i].as_i64().unwrap_or(0) as u32;
                    match &w[i + 1] {
                        Obj::Array(list) => {
                            for (k, v) in list.iter().enumerate() {
                                f.cid_widths.insert(c0 + k as u32, file.resolve(v).as_f32().unwrap_or(f.default_w));
                            }
                            i += 2;
                        }
                        o => {
                            let c1 = o.as_i64().unwrap_or(0) as u32;
                            let v = w.get(i + 2).and_then(|x| x.as_f32()).unwrap_or(f.default_w);
                            for c in c0..=c1.min(c0 + 65535) {
                                f.cid_widths.insert(c, v);
                            }
                            i += 3;
                        }
                    }
                }
            }
            // Encoding: Identity-H/V, or an embedded CMap.
            match file.resolve(fd.get("Encoding").unwrap_or(&Obj::Null)) {
                s @ Obj::Stream(..) => {
                    if let Some((_, data)) = file.stream_data(&s) {
                        f.cmap = Some(parse_cmap(&data));
                    }
                }
                _ => {}
            }
            if let Obj::Stream(..) = file.dget(&cd, "CIDToGIDMap") {
                if let Some((_, data)) = file.stream_data(&file.dget(&cd, "CIDToGIDMap")) {
                    f.cid_to_gid = Some(data.chunks_exact(2).map(|p| u16::from_be_bytes([p[0], p[1]])).collect());
                }
            }
            let fdesc = file.dget(&cd, "FontDescriptor");
            f.load_program(file, fdesc.as_dict(), &base_name);
            if let Prog::Cff(data) = &f.prog {
                // CID-keyed CFF: CID -> glyph through the charset.
                if let Some(t) = ttf_parser::cff::Table::parse(data) {
                    let mut map = Vec::new();
                    for g in 0..t.number_of_glyphs() {
                        if let Some(cid) = t.glyph_cid(GlyphId(g)) {
                            let cid = cid as usize;
                            if map.len() <= cid {
                                map.resize(cid + 1, 0);
                            }
                            map[cid] = g;
                        }
                    }
                    if map.iter().any(|&g| g != 0) && f.cid_to_gid.is_none() {
                        f.cid_to_gid = Some(map);
                    }
                }
            }
            return f;
        }
        // Simple fonts.
        f.first = file.dget(fd, "FirstChar").as_i64().unwrap_or(0).clamp(0, 255) as u32;
        if let Obj::Array(w) = file.dget(fd, "Widths") {
            f.widths = w.iter().map(|o| file.resolve(o).as_f32().unwrap_or(0.0)).collect();
        }
        let fdesc = file.dget(fd, "FontDescriptor");
        if let Some(mw) = fdesc.as_dict().map(|d| file.dget(d, "MissingWidth")).and_then(|o| o.as_f32()) {
            f.default_w = mw;
        }
        let flags = fdesc.as_dict().map(|d| file.dget(d, "Flags")).and_then(|o| o.as_i64()).unwrap_or(32);
        if subtype == b"Type3" {
            let m: Vec<f32> = match file.dget(fd, "FontMatrix") {
                Obj::Array(a) => a.iter().map(|o| o.as_f32().unwrap_or(0.0)).collect(),
                _ => vec![0.001, 0.0, 0.0, 0.001, 0.0, 0.0],
            };
            let matrix = if m.len() == 6 { [m[0], m[1], m[2], m[3], m[4], m[5]] } else { [0.001, 0.0, 0.0, 0.001, 0.0, 0.0] };
            let procs_d = file.dget(fd, "CharProcs");
            let res = file.dget(fd, "Resources").as_dict().cloned();
            let mut procs = BTreeMap::new();
            let (_, diffs) = encoding_of(file, fd, Base::Standard);
            if let Some(pd) = procs_d.as_dict() {
                for code in 0..256u32 {
                    let name = diffs.get(&(code as u8)).cloned().or_else(|| base_char(Base::Standard, code as u8).and_then(char_to_name));
                    if let Some(n) = name {
                        if let Some(o) = pd.get(&n) {
                            procs.insert(code, o.clone());
                        }
                    }
                }
            }
            // Type 3 widths are in glyph space.
            for w in f.widths.iter_mut() {
                *w *= matrix[0] * 1000.0;
            }
            f.type3 = Some(Type3 { matrix, procs, resources: res });
            return f;
        }
        f.load_program(file, fdesc.as_dict(), &base_name);
        let symbolic = flags & 4 != 0 && flags & 32 == 0;
        let default_base = match (&f.prog, symbolic) {
            (Prog::Noto(_), _) if base_name.contains("Symbol") || base_name.contains("Dingbats") => Base::Symbol,
            (Prog::Sfnt(_), false) => Base::WinAnsi,
            _ => Base::Standard,
        };
        let (base, diffs) = encoding_of(file, fd, default_base);
        let has_enc = !matches!(file.dget(fd, "Encoding"), Obj::Null);
        for code in 0..256usize {
            let name = diffs.get(&(code as u8)).cloned();
            let ch = name.as_deref().and_then(name_to_char).or_else(|| if name.is_none() { base_char(base, code as u8) } else { None });
            f.code_char[code] = ch;
            f.code_gid[code] = match &f.prog {
                Prog::Sfnt(data) => match ttf_parser::Face::parse(data, 0) {
                    Ok(face) => sfnt_gid(&face, code as u8, ch, name.as_deref(), symbolic || !has_enc),
                    Err(_) => 0,
                },
                Prog::Cff(data) => match ttf_parser::cff::Table::parse(data) {
                    Some(t) => {
                        let nm = name.clone().or_else(|| if has_enc { ch.and_then(char_to_name) } else { None });
                        nm.and_then(|n| t.glyph_index_by_name(&n)).or_else(|| t.glyph_index(code as u8)).map(|g| g.0).unwrap_or(0)
                    }
                    None => 0,
                },
                Prog::Type1(t1) => {
                    let nm = name.clone().or_else(|| if has_enc { ch.and_then(char_to_name) } else { t1.encoding.get(code).cloned().flatten() });
                    nm.and_then(|n| t1.glyph_index(&n)).unwrap_or(0) as u16
                }
                Prog::Noto(_) => 0,
            };
        }
        if let Prog::Noto(_) = f.prog {
            f.prog = Prog::Noto(noto_for(&base_name, flags));
        }
        f
    }

    fn load_program(&mut self, file: &PdfFile, desc: Option<&Dict>, base_name: &str) {
        let Some(d) = desc else {
            self.prog = Prog::Noto(noto_for(base_name, 0));
            return;
        };
        if let Some((_, data)) = file.stream_data(&file.dget(d, "FontFile2")) {
            if let Ok(face) = ttf_parser::Face::parse(&data, 0) {
                self.upem = face.units_per_em() as f32;
                self.prog = Prog::Sfnt(data);
                return;
            }
        }
        if let Some((sd, data)) = file.stream_data(&file.dget(d, "FontFile3")) {
            if sd.is("Subtype", "OpenType") {
                if let Ok(face) = ttf_parser::Face::parse(&data, 0) {
                    self.upem = face.units_per_em() as f32;
                    self.prog = Prog::Sfnt(data);
                    return;
                }
            }
            if let Some(t) = ttf_parser::cff::Table::parse(&data) {
                let m = t.matrix();
                self.cff_scale = if m.sx != 0.0 { m.sx * 1000.0 } else { 1.0 };
                self.prog = Prog::Cff(data);
                return;
            }
        }
        if let Some((_, data)) = file.stream_data(&file.dget(d, "FontFile")) {
            if let Some(t1) = super::type1::Type1::parse(&data) {
                self.prog = Prog::Type1(t1);
                return;
            }
        }
        let flags = file.dget(d, "Flags").as_i64().unwrap_or(32);
        self.prog = Prog::Noto(noto_for(base_name, flags));
    }

    /// Split a string into character codes.
    pub fn codes(&self, s: &[u8]) -> Vec<u32> {
        if !self.two_byte {
            return s.iter().map(|&b| b as u32).collect();
        }
        match &self.cmap {
            None => s.chunks(2).map(|p| if p.len() == 2 { (p[0] as u32) << 8 | p[1] as u32 } else { p[0] as u32 }).collect(),
            Some(ranges) => {
                // Match code lengths 1-4 against the ranges.
                let mut out = Vec::new();
                let mut i = 0;
                while i < s.len() {
                    let mut done = false;
                    for len in 1..=4usize {
                        if i + len > s.len() {
                            break;
                        }
                        let code = s[i..i + len].iter().fold(0u32, |a, &b| a << 8 | b as u32);
                        if ranges.iter().any(|&(lo, hi, _, l)| l as usize == len && code >= lo && code <= hi) {
                            out.push(code | (len as u32) << 28);
                            i += len;
                            done = true;
                            break;
                        }
                    }
                    if !done {
                        let len = 2.min(s.len() - i);
                        out.push(s[i..i + len].iter().fold(0u32, |a, &b| a << 8 | b as u32) | (len as u32) << 28);
                        i += len;
                    }
                }
                out
            }
        }
    }

    fn cid(&self, code: u32) -> u32 {
        match &self.cmap {
            None => code,
            Some(ranges) => {
                let len = (code >> 28) as u8;
                let c = code & 0x0FFF_FFFF;
                ranges.iter().find(|&&(lo, hi, _, l)| (l == len || len == 0) && c >= lo && c <= hi).map(|&(lo, _, cid, _)| cid + (c - lo)).unwrap_or(0)
            }
        }
    }

    /// Is this code a single-byte space (for word spacing)?
    pub fn is_space(&self, code: u32) -> bool {
        if self.two_byte {
            (code >> 28) == 1 && (code & 0xFF) == 32
        } else {
            code == 32
        }
    }

    /// Width in 1/1000 em (text space units x1000).
    pub fn width(&self, code: u32) -> f32 {
        if self.two_byte {
            let cid = self.cid(code);
            return *self.cid_widths.get(&cid).unwrap_or(&self.default_w);
        }
        if code >= self.first {
            if let Some(&w) = self.widths.get((code - self.first) as usize) {
                if w > 0.0 || self.type3.is_some() {
                    return w;
                }
            }
        }
        if !self.widths.is_empty() && self.type3.is_none() {
            return self.default_w.max(0.0);
        }
        // No widths: measure the program.
        match &self.prog {
            Prog::Noto(face) => {
                let ch = self.code_char.get(code as usize).copied().flatten().unwrap_or(' ');
                let mut f = crate::font::FONTS.lock();
                let (upem, ..) = f.vmetrics(*face);
                f.char_units(*face, ch) as f32 * 1000.0 / upem as f32
            }
            Prog::Sfnt(data) => ttf_parser::Face::parse(data, 0).ok().and_then(|face| face.glyph_hor_advance(GlyphId(self.code_gid[code as usize & 255]))).map(|a| a as f32 * 1000.0 / self.upem).unwrap_or(500.0),
            Prog::Type1(t1) => t1.width(self.code_gid[code as usize & 255] as usize),
            _ => 500.0,
        }
    }

    /// Draw a glyph's outline into `b`, in em units (1.0 = font size), y up.
    pub fn outline(&self, code: u32, b: &mut dyn OutlineBuilder) {
        let mut sc = Scale { inner: b, s: 1.0 / self.upem, sx: 1.0 };
        match &self.prog {
            Prog::Sfnt(data) => {
                let Ok(face) = ttf_parser::Face::parse(data, 0) else { return };
                let gid = if self.two_byte {
                    let cid = self.cid(code);
                    match &self.cid_to_gid {
                        Some(m) => m.get(cid as usize).copied().unwrap_or(0),
                        None => cid as u16,
                    }
                } else {
                    self.code_gid[code as usize & 255]
                };
                face.outline_glyph(GlyphId(gid), &mut sc);
            }
            Prog::Cff(data) => {
                let Some(t) = ttf_parser::cff::Table::parse(data) else { return };
                let gid = if self.two_byte {
                    let cid = self.cid(code);
                    match &self.cid_to_gid {
                        Some(m) => m.get(cid as usize).copied().unwrap_or(cid as u16),
                        None => cid as u16,
                    }
                } else {
                    self.code_gid[code as usize & 255]
                };
                sc.s = self.cff_scale / 1000.0;
                let _ = t.outline(GlyphId(gid), &mut sc);
            }
            Prog::Type1(t1) => {
                sc.s = 1.0 / 1000.0;
                t1.outline(self.code_gid[code as usize & 255] as usize, &mut sc);
            }
            Prog::Noto(face) => {
                let ch = if self.two_byte { char::from_u32(self.cid(code)).unwrap_or('?') } else { self.code_char.get(code as usize).copied().flatten().unwrap_or(' ') };
                if ch == ' ' {
                    return;
                }
                // Squeeze or stretch the stand-in to the width the PDF expects (measured before
                // taking the font lock: width() may need it too).
                let want = self.width(code);
                let mut fonts = crate::font::FONTS.lock();
                let (upem, ..) = fonts.vmetrics(*face);
                // Squeeze or stretch the stand-in to the width the PDF expects.
                let have = fonts.char_units(*face, ch) as f32 * 1000.0 / upem as f32;
                let ttf = fonts.face(*face);
                let Some(gid) = ttf.glyph_index(ch) else { return };
                sc.s = 1.0 / upem as f32;
                sc.sx = if have > 1.0 && want > 1.0 { (want / have).clamp(0.5, 1.6) } else { 1.0 };
                ttf.outline_glyph(gid, &mut sc);
            }
        }
    }
}

struct Scale<'a> {
    inner: &'a mut dyn OutlineBuilder,
    s: f32,
    sx: f32,
}

impl OutlineBuilder for Scale<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        self.inner.move_to(x * self.s * self.sx, y * self.s);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        self.inner.line_to(x * self.s * self.sx, y * self.s);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (s, sx) = (self.s, self.sx);
        self.inner.quad_to(x1 * s * sx, y1 * s, x * s * sx, y * s);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (s, sx) = (self.s, self.sx);
        self.inner.curve_to(x1 * s * sx, y1 * s, x2 * s * sx, y2 * s, x * s * sx, y * s);
    }
    fn close(&mut self) {
        self.inner.close();
    }
}

fn sfnt_gid(face: &ttf_parser::Face, code: u8, ch: Option<char>, name: Option<&str>, symbolic: bool) -> u16 {
    use ttf_parser::PlatformId;
    let Some(cmap) = face.tables().cmap else {
        return name.and_then(|n| face.glyph_index_by_name(n)).map(|g| g.0).unwrap_or(code as u16);
    };
    let sub = |pid: PlatformId, eid: u16, c: u32| cmap.subtables.into_iter().find(|s| s.platform_id == pid && s.encoding_id == eid).and_then(|s| s.glyph_index(c)).map(|g| g.0);
    if !symbolic {
        if let Some(c) = ch {
            if let Some(g) = sub(PlatformId::Windows, 1, c as u32).or_else(|| sub(PlatformId::Unicode, 3, c as u32)).or_else(|| face.glyph_index(c).map(|g| g.0)) {
                return g;
            }
        }
        if let Some(g) = name.and_then(|n| face.glyph_index_by_name(n)) {
            return g.0;
        }
    }
    // Symbolic: (3,0) at F000+code, then (1,0) by code.
    sub(PlatformId::Windows, 0, 0xF000 + code as u32)
        .or_else(|| sub(PlatformId::Windows, 0, code as u32))
        .or_else(|| sub(PlatformId::Macintosh, 0, code as u32))
        .or_else(|| ch.and_then(|c| face.glyph_index(c)).map(|g| g.0))
        .unwrap_or(0)
}

/// The base encoding and the /Differences of a simple font.
fn encoding_of(file: &PdfFile, fd: &Dict, default: Base) -> (Base, BTreeMap<u8, String>) {
    let mut diffs = BTreeMap::new();
    let pick = |n: &[u8]| match n {
        b"WinAnsiEncoding" => Some(Base::WinAnsi),
        b"MacRomanEncoding" => Some(Base::MacRoman),
        b"StandardEncoding" => Some(Base::Standard),
        _ => None,
    };
    match file.dget(fd, "Encoding") {
        Obj::Name(n) => (pick(&n).unwrap_or(default), diffs),
        Obj::Dict(d) => {
            let base = d.name("BaseEncoding").and_then(pick).unwrap_or(default);
            if let Obj::Array(a) = file.dget(&d, "Differences") {
                let mut code = 0u32;
                for o in a.iter().map(|o| file.resolve(o)) {
                    match o {
                        Obj::Int(i) => code = i.clamp(0, 255) as u32,
                        Obj::Name(n) => {
                            if code < 256 {
                                diffs.insert(code as u8, String::from_utf8_lossy(&n).into_owned());
                            }
                            code += 1;
                        }
                        _ => {}
                    }
                }
            }
            (base, diffs)
        }
        _ => (default, diffs),
    }
}

/// Code space ranges and CID mappings of an embedded CMap:
/// (low code, high code, first CID, code length).
fn parse_cmap(data: &[u8]) -> Vec<(u32, u32, u32, u8)> {
    use super::obj::{Lexer, Tok};
    let mut lx = Lexer::new(data, 0);
    let mut stack: Vec<Obj> = Vec::new();
    let mut out = Vec::new();
    let mut mode = 0; // 1 cidrange, 2 cidchar
    let val = |o: &Obj| -> (u32, u8) {
        match o {
            Obj::Str(s) => (s.iter().fold(0u32, |a, &b| a << 8 | b as u32), s.len() as u8),
            Obj::Int(i) => (*i as u32, 0),
            _ => (0, 0),
        }
    };
    while let Some(t) = lx.tok() {
        match t {
            Tok::Obj(o) => {
                stack.push(o);
                if mode == 1 && stack.len() == 3 {
                    let (lo, l) = val(&stack[0]);
                    let (hi, _) = val(&stack[1]);
                    let (cid, _) = val(&stack[2]);
                    out.push((lo, hi, cid, l));
                    stack.clear();
                } else if mode == 2 && stack.len() == 2 {
                    let (c, l) = val(&stack[0]);
                    let (cid, _) = val(&stack[1]);
                    out.push((c, c, cid, l));
                    stack.clear();
                }
            }
            Tok::Word(b"begincidrange") => {
                mode = 1;
                stack.clear();
            }
            Tok::Word(b"begincidchar") => {
                mode = 2;
                stack.clear();
            }
            Tok::Word(_) => {
                mode = 0;
                stack.clear();
            }
            _ => stack.clear(),
        }
    }
    out
}

/// The glyph name StandardEncoding gives a code (for Type 1 "seac" and encodings).
pub fn std_glyph_name(code: u8) -> Option<String> {
    base_char(Base::Standard, code).and_then(|c| match c {
        '\u{2019}' => Some(String::from("quoteright")),
        '\u{2018}' => Some(String::from("quoteleft")),
        c => char_to_name(c),
    })
}

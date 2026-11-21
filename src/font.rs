//! Smooth, proportional text: the Noto Sans and Noto Serif families (SIL Open Font License,
//! see fonts/OFL.txt). Outlines are read on demand with ttf-parser and filled by our own
//! rasteriser; every glyph is drawn once per pixel size and kept.

use alloc::collections::BTreeMap;
use alloc::vec::Vec;
use spin::Mutex;
use ttf_parser::{GlyphId, OutlineBuilder};

use crate::raster::Raster;

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub enum Face {
    Sans,
    SansBold,
    SansItalic,
    SansBoldItalic,
    Serif,
    SerifBold,
    SerifItalic,
    SerifBoldItalic,
}

impl Face {
    pub const ALL: [Face; 8] = [Face::Sans, Face::SansBold, Face::SansItalic, Face::SansBoldItalic, Face::Serif, Face::SerifBold, Face::SerifItalic, Face::SerifBoldItalic];
    pub fn new(serif: bool, bold: bool, italic: bool) -> Face {
        Self::ALL[(serif as usize) * 4 + (italic as usize) * 2 + bold as usize]
    }
    pub fn serif(self) -> bool {
        self as usize >= 4
    }
    pub fn bold(self) -> bool {
        self as usize & 1 != 0
    }
    pub fn italic(self) -> bool {
        self as usize & 2 != 0
    }
    /// The TrueType file behind this face (for putting into PDFs).
    pub fn data(self) -> &'static [u8] {
        DATA[self as usize]
    }
    /// The font's PostScript name, for PDFs.
    pub fn ps_name(self) -> &'static str {
        ["NotoSans-Regular", "NotoSans-Bold", "NotoSans-Italic", "NotoSans-BoldItalic", "NotoSerif-Regular", "NotoSerif-Bold", "NotoSerif-Italic", "NotoSerif-BoldItalic"][self as usize]
    }
}

static DATA: [&[u8]; 8] = [
    include_bytes!("../fonts/NotoSans-Regular.ttf"),
    include_bytes!("../fonts/NotoSans-Bold.ttf"),
    include_bytes!("../fonts/NotoSans-Italic.ttf"),
    include_bytes!("../fonts/NotoSans-BoldItalic.ttf"),
    include_bytes!("../fonts/NotoSerif-Regular.ttf"),
    include_bytes!("../fonts/NotoSerif-Bold.ttf"),
    include_bytes!("../fonts/NotoSerif-Italic.ttf"),
    include_bytes!("../fonts/NotoSerif-BoldItalic.ttf"),
];

/// One rasterised glyph: coverage rows, top to bottom. Its top-left sits at
/// (pen + xmin, baseline - ymin - h).
pub struct Glyph {
    pub xmin: i32,
    pub ymin: i32,
    pub w: usize,
    pub h: usize,
    /// Advance in 1/64 px.
    pub adv: i32,
    pub cov: Vec<u8>,
}

pub struct Fonts {
    faces: [Option<ttf_parser::Face<'static>>; 8],
    glyphs: BTreeMap<(u8, u32, u16), Glyph>,
    units: BTreeMap<(u8, u32), u16>,
}

pub static FONTS: Mutex<Fonts> = Mutex::new(Fonts { faces: [None, None, None, None, None, None, None, None], glyphs: BTreeMap::new(), units: BTreeMap::new() });

/// Feeds an outline into a raster, scaling font units to pixels (y flipped).
pub struct ToRaster<'a> {
    pub r: &'a mut Raster,
    /// x' = a*x + c*y + e, y' = b*x + d*y + f
    pub m: [f32; 6],
}

impl ToRaster<'_> {
    fn p(&self, x: f32, y: f32) -> (f32, f32) {
        let m = &self.m;
        (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
    }
}

impl OutlineBuilder for ToRaster<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.p(x, y);
        self.r.move_to(x, y);
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (x, y) = self.p(x, y);
        self.r.line_to(x, y);
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        let (x1, y1) = self.p(x1, y1);
        let (x, y) = self.p(x, y);
        self.r.quad_to(x1, y1, x, y);
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (x1, y1) = self.p(x1, y1);
        let (x2, y2) = self.p(x2, y2);
        let (x, y) = self.p(x, y);
        self.r.cubic_to(x1, y1, x2, y2, x, y);
    }
    fn close(&mut self) {
        self.r.close();
    }
}

impl Fonts {
    pub fn face(&mut self, face: Face) -> &ttf_parser::Face<'static> {
        let i = face as usize;
        if self.faces[i].is_none() {
            self.faces[i] = Some(ttf_parser::Face::parse(DATA[i], 0).expect("built-in font"));
        }
        self.faces[i].as_ref().unwrap()
    }

    pub fn glyph(&mut self, face: Face, ch: char, px: u16) -> &Glyph {
        let key = (face as u8, ch as u32, px);
        if !self.glyphs.contains_key(&key) {
            let g = {
                let f = self.face(face);
                let gid = f.glyph_index(ch).or_else(|| if ch.is_whitespace() { f.glyph_index(' ') } else { f.glyph_index('?') }).unwrap_or(GlyphId(0));
                render(f, gid, px as f32)
            };
            self.glyphs.insert(key, g);
        }
        &self.glyphs[&key]
    }

    /// Advance of one character, in 1/64 px.
    pub fn advance(&mut self, face: Face, ch: char, px: u16) -> i32 {
        self.glyph(face, ch, px).adv
    }

    pub fn glyph_index(&mut self, face: Face, ch: char) -> u16 {
        self.face(face).glyph_index(ch).map(|g| g.0).unwrap_or(0)
    }

    /// Advance of a glyph, in font units, and the units per em.
    pub fn advance_units(&mut self, face: Face, gid: u16) -> (u16, u16) {
        let f = self.face(face);
        (f.glyph_hor_advance(GlyphId(gid)).unwrap_or(0), f.units_per_em())
    }

    /// Advance of a character in font units (missing characters measure as '?').
    pub fn char_units(&mut self, face: Face, ch: char) -> i32 {
        let key = (face as u8, ch as u32);
        if let Some(&u) = self.units.get(&key) {
            return u as i32;
        }
        let f = self.face(face);
        let gid = f.glyph_index(ch).or_else(|| f.glyph_index('?')).unwrap_or(GlyphId(0));
        let u = f.glyph_hor_advance(gid).unwrap_or(0);
        self.units.insert(key, u);
        u as i32
    }

    /// (units per em, ascender, descender (positive), line gap) in font units.
    pub fn vmetrics(&mut self, face: Face) -> (i32, i32, i32, i32) {
        let f = self.face(face);
        (f.units_per_em() as i32, f.ascender() as i32, -(f.descender() as i32), f.line_gap() as i32)
    }
}

/// Rasterise one glyph of a font at `px` pixels per em.
pub fn render(f: &ttf_parser::Face, gid: GlyphId, px: f32) -> Glyph {
    let s = px / f.units_per_em() as f32;
    let adv = (f.glyph_hor_advance(gid).unwrap_or(0) as f32 * s * 64.0 + 0.5) as i32;
    let Some(bb) = f.glyph_bounding_box(gid) else { return Glyph { xmin: 0, ymin: 0, w: 0, h: 0, adv, cov: Vec::new() } };
    let xmin = libm::floorf(bb.x_min as f32 * s) as i32;
    let xmax = libm::ceilf(bb.x_max as f32 * s) as i32;
    let ymin = libm::floorf(bb.y_min as f32 * s) as i32;
    let ymax = libm::ceilf(bb.y_max as f32 * s) as i32;
    let (w, h) = ((xmax - xmin).max(1) as usize, (ymax - ymin).max(1) as usize);
    let mut r = Raster::new(w, h);
    let mut b = ToRaster { r: &mut r, m: [s, 0.0, 0.0, -s, -xmin as f32, ymax as f32] };
    f.outline_glyph(gid, &mut b);
    r.close();
    Glyph { xmin, ymin, w, h, adv, cov: r.mask(false) }
}

/// Width of `s` in 1/64 px.
pub fn width64(s: &str, face: Face, px: u16) -> i32 {
    let mut f = FONTS.lock();
    s.chars().map(|ch| f.advance(face, ch, px)).sum()
}

/// Width of `s` in whole pixels (rounded up).
pub fn width(s: &str, face: Face, px: u16) -> i32 {
    (width64(s, face, px) + 63) / 64
}

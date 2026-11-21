//! Drawing PDF pages: the content stream interpreter (paths, strokes, clipping, colour spaces,
//! text, images, gradients, forms, Type 3 glyphs) and annotation appearances.

use alloc::collections::BTreeMap;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use core::cell::RefCell;

use super::file::{Page, PdfFile};
use super::fonts::PdfFont;
use super::func::Func;
use super::obj::{Dict, Lexer, Obj, Tok};
use crate::gfx::{mix, Canvas};
use crate::raster::Raster;
use ttf_parser::OutlineBuilder;

pub type M = [f32; 6];
pub const ID: M = [1.0, 0.0, 0.0, 1.0, 0.0, 0.0];

/// m then n.
pub fn mul(m: &M, n: &M) -> M {
    [m[0] * n[0] + m[1] * n[2], m[0] * n[1] + m[1] * n[3], m[2] * n[0] + m[3] * n[2], m[2] * n[1] + m[3] * n[3], m[4] * n[0] + m[5] * n[2] + n[4], m[4] * n[1] + m[5] * n[3] + n[5]]
}
pub fn apply(m: &M, x: f32, y: f32) -> (f32, f32) {
    (m[0] * x + m[2] * y + m[4], m[1] * x + m[3] * y + m[5])
}
pub fn invert(m: &M) -> Option<M> {
    let det = m[0] * m[3] - m[1] * m[2];
    if det.abs() < 1e-12 {
        return None;
    }
    let (a, b, c, d) = (m[3] / det, -m[1] / det, -m[2] / det, m[0] / det);
    Some([a, b, c, d, -(m[4] * a + m[5] * c), -(m[4] * b + m[5] * d)])
}
fn scale_of(m: &M) -> f32 {
    libm::sqrtf((m[0] * m[3] - m[1] * m[2]).abs())
}

/// The matrix from a page's user space to device pixels at `s` pixels per point.
pub fn page_matrix(p: &Page, s: f32, extra_rot: i32) -> M {
    let c = p.crop;
    match (p.rotate + extra_rot).rem_euclid(360) {
        90 => [0.0, s, s, 0.0, -c[1] * s, -c[0] * s],
        180 => [-s, 0.0, 0.0, s, c[2] * s, -c[1] * s],
        270 => [0.0, -s, -s, 0.0, c[3] * s, c[2] * s],
        _ => [s, 0.0, 0.0, -s, -c[0] * s, c[3] * s],
    }
}

/// Pixel size of a page at `s` pixels per point.
pub fn page_pixels(p: &Page, s: f32, extra_rot: i32) -> (usize, usize) {
    let (w, h) = ((p.crop[2] - p.crop[0]).abs(), (p.crop[3] - p.crop[1]).abs());
    let (w, h) = if (p.rotate + extra_rot).rem_euclid(180) == 90 { (h, w) } else { (w, h) };
    (((w * s) as usize).max(1), ((h * s) as usize).max(1))
}

// ---- colour -----------------------------------------------------------------------------------

#[derive(Clone)]
pub enum Cs {
    Gray,
    Rgb,
    Cmyk,
    Lab,
    Indexed(Rc<(Cs, Vec<u8>, usize)>),
    Tint(Rc<(Cs, Func, usize)>),
    Pattern,
}

impl Cs {
    pub fn n(&self) -> usize {
        match self {
            Cs::Gray | Cs::Indexed(_) | Cs::Pattern => 1,
            Cs::Rgb | Cs::Lab => 3,
            Cs::Cmyk => 4,
            Cs::Tint(t) => t.2,
        }
    }
    pub fn initial(&self) -> Vec<f32> {
        match self {
            Cs::Cmyk => vec![0.0, 0.0, 0.0, 1.0],
            Cs::Lab => vec![0.0, 0.0, 0.0],
            _ => vec![if matches!(self, Cs::Tint(_)) { 1.0 } else { 0.0 }; self.n()],
        }
    }
    pub fn rgb(&self, v: &[f32]) -> u32 {
        let g = |i: usize| v.get(i).copied().unwrap_or(0.0).clamp(0.0, 1.0);
        let pack = |r: f32, g: f32, b: f32| ((r.clamp(0.0, 1.0) * 255.0 + 0.5) as u32) << 16 | ((g.clamp(0.0, 1.0) * 255.0 + 0.5) as u32) << 8 | (b.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        match self {
            Cs::Gray => pack(g(0), g(0), g(0)),
            Cs::Rgb => pack(g(0), g(1), g(2)),
            Cs::Cmyk => {
                let k = g(3);
                pack((1.0 - g(0)) * (1.0 - k), (1.0 - g(1)) * (1.0 - k), (1.0 - g(2)) * (1.0 - k))
            }
            Cs::Lab => {
                let l = v.first().copied().unwrap_or(0.0);
                let (a, b) = (v.get(1).copied().unwrap_or(0.0), v.get(2).copied().unwrap_or(0.0));
                let fy = (l + 16.0) / 116.0;
                let (fx, fz) = (fy + a / 500.0, fy - b / 200.0);
                let f = |t: f32| if t > 0.2069 { t * t * t } else { (t - 0.1379) * 0.1284 };
                let (x, y, z) = (0.9505 * f(fx), f(fy), 1.089 * f(fz));
                let lin = |c: f32| if c <= 0.0031308 { 12.92 * c } else { 1.055 * libm::powf(c.max(0.0), 1.0 / 2.4) - 0.055 };
                pack(lin(3.2406 * x - 1.5372 * y - 0.4986 * z), lin(-0.9689 * x + 1.8758 * y + 0.0415 * z), lin(0.0557 * x - 0.204 * y + 1.057 * z))
            }
            Cs::Indexed(ix) => {
                let (base, table, hival) = (&ix.0, &ix.1, ix.2);
                let i = (v.first().copied().unwrap_or(0.0).max(0.0) as usize).min(hival);
                let n = base.n();
                let comps: Vec<f32> = (0..n).map(|k| table.get(i * n + k).copied().unwrap_or(0) as f32 / 255.0).collect();
                base.rgb(&comps)
            }
            Cs::Tint(t) => {
                let out = t.1.eval(v);
                t.0.rgb(&out)
            }
            Cs::Pattern => 0x808080,
        }
    }
}

pub fn load_cs(file: &PdfFile, o: &Obj, res: Option<&Dict>, depth: u32) -> Cs {
    if depth > 8 {
        return Cs::Gray;
    }
    let o = file.resolve(o);
    match &o {
        Obj::Name(n) => match n.as_slice() {
            b"DeviceGray" | b"G" | b"CalGray" => Cs::Gray,
            b"DeviceRGB" | b"RGB" | b"CalRGB" => Cs::Rgb,
            b"DeviceCMYK" | b"CMYK" => Cs::Cmyk,
            b"Pattern" => Cs::Pattern,
            b"Indexed" | b"I" => Cs::Gray,
            other => {
                // A named resource.
                let r = res.and_then(|r| file.dget(r, "ColorSpace").as_dict().cloned());
                match r.and_then(|d| d.get(core::str::from_utf8(other).unwrap_or("")).cloned()) {
                    Some(x) => load_cs(file, &x, res, depth + 1),
                    None => Cs::Rgb,
                }
            }
        },
        Obj::Array(a) if !a.is_empty() => {
            let kind = file.resolve(&a[0]);
            match kind.as_name().unwrap_or(b"") {
                b"CalGray" => Cs::Gray,
                b"CalRGB" => Cs::Rgb,
                b"Lab" => Cs::Lab,
                b"ICCBased" => {
                    let s = a.get(1).map(|x| file.resolve(x)).unwrap_or(Obj::Null);
                    let d = s.as_dict().cloned().unwrap_or_default();
                    match file.dget(&d, "N").as_i64().unwrap_or(3) {
                        1 => Cs::Gray,
                        4 => Cs::Cmyk,
                        _ => match d.get("Alternate") {
                            Some(alt) => load_cs(file, alt, res, depth + 1),
                            None => Cs::Rgb,
                        },
                    }
                }
                b"Indexed" | b"I" => {
                    let base = a.get(1).map(|x| load_cs(file, x, res, depth + 1)).unwrap_or(Cs::Rgb);
                    let hival = a.get(2).and_then(|x| file.resolve(x).as_i64()).unwrap_or(255).clamp(0, 4095) as usize;
                    let table = match a.get(3).map(|x| file.resolve(x)) {
                        Some(Obj::Str(s)) => s,
                        Some(s @ Obj::Stream(..)) => file.stream_data(&s).map(|x| x.1).unwrap_or_default(),
                        _ => Vec::new(),
                    };
                    Cs::Indexed(Rc::new((base, table, hival)))
                }
                b"Separation" | b"DeviceN" => {
                    let n = if kind.name_is("Separation") { 1 } else { a.get(1).and_then(|x| file.resolve(x).as_array().map(|v| v.len())).unwrap_or(1) };
                    let alt = a.get(2).map(|x| load_cs(file, x, res, depth + 1)).unwrap_or(Cs::Gray);
                    match a.get(3).and_then(|f| Func::load(file, f)) {
                        Some(f) => Cs::Tint(Rc::new((alt, f, n))),
                        None => Cs::Gray,
                    }
                }
                b"Pattern" => Cs::Pattern,
                _ => Cs::Rgb,
            }
        }
        _ => Cs::Rgb,
    }
}

// ---- clipping ---------------------------------------------------------------------------------

pub enum Clip {
    Rect(i32, i32, i32, i32),
    Mask { x0: i32, y0: i32, w: i32, h: i32, a: Vec<u8> },
}

impl Clip {
    fn bounds(&self) -> (i32, i32, i32, i32) {
        match self {
            Clip::Rect(a, b, c, d) => (*a, *b, *c, *d),
            Clip::Mask { x0, y0, w, h, .. } => (*x0, *y0, x0 + w, y0 + h),
        }
    }
    fn at(&self, x: i32, y: i32) -> u32 {
        match self {
            Clip::Rect(a, b, c, d) => {
                if x >= *a && x < *c && y >= *b && y < *d {
                    255
                } else {
                    0
                }
            }
            Clip::Mask { x0, y0, w, h, a } => {
                let (u, v) = (x - x0, y - y0);
                if u >= 0 && v >= 0 && u < *w && v < *h {
                    a[(v * w + u) as usize] as u32
                } else {
                    0
                }
            }
        }
    }
}

// ---- paths ------------------------------------------------------------------------------------

#[derive(Clone, Copy)]
enum P {
    Move(f32, f32),
    Line(f32, f32),
    Cubic(f32, f32, f32, f32, f32, f32),
    Close,
}

#[derive(Clone)]
enum Paint {
    Color(u32),
    Shade(Rc<Shading>),
}

pub struct Shading {
    kind: i64,
    cs: Cs,
    coords: Vec<f32>,
    func: Option<Func>,
    extend: [bool; 2],
    /// Device -> shading space.
    inv: M,
    lut: Vec<u32>,
    domain: [f32; 2],
    bg: Option<u32>,
}

impl Shading {
    fn load(file: &PdfFile, o: &Obj, res: &Dict, to_dev: &M) -> Option<Shading> {
        let o = file.resolve(o);
        let d = o.as_dict()?.clone();
        let kind = file.dget(&d, "ShadingType").as_i64()?;
        let cs = load_cs(file, d.get("ColorSpace").unwrap_or(&Obj::Null), Some(res), 0);
        let coords: Vec<f32> = file.dget(&d, "Coords").as_array().map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default();
        let func = d.get("Function").and_then(|f| Func::load(file, f));
        let ext: Vec<bool> = file.dget(&d, "Extend").as_array().map(|a| a.iter().map(|x| matches!(x, Obj::Bool(true))).collect()).unwrap_or_default();
        let dom: Vec<f32> = file.dget(&d, "Domain").as_array().map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default();
        let domain = [dom.first().copied().unwrap_or(0.0), dom.get(1).copied().unwrap_or(1.0)];
        let bg = file.dget(&d, "Background").as_array().map(|a| cs.rgb(&a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect::<Vec<_>>()));
        let mut lut = Vec::with_capacity(256);
        if let Some(f) = &func {
            for i in 0..256 {
                let t = domain[0] + (domain[1] - domain[0]) * i as f32 / 255.0;
                lut.push(cs.rgb(&f.eval(&[t])));
            }
        }
        let inv = invert(to_dev).unwrap_or(ID);
        Some(Shading { kind, cs, coords, func, extend: [ext.first().copied().unwrap_or(false), ext.get(1).copied().unwrap_or(false)], inv, lut, domain, bg })
    }

    /// Colour at a device pixel, or None outside.
    fn color(&self, x: f32, y: f32) -> Option<u32> {
        let (u, v) = apply(&self.inv, x, y);
        let c = &self.coords;
        let t = match self.kind {
            2 if c.len() >= 4 => {
                let (dx, dy) = (c[2] - c[0], c[3] - c[1]);
                let den = dx * dx + dy * dy;
                if den == 0.0 {
                    0.0
                } else {
                    ((u - c[0]) * dx + (v - c[1]) * dy) / den
                }
            }
            3 if c.len() >= 6 => {
                // Solve for the largest s with the point on circle(s).
                let (x0, y0, r0, x1, y1, r1) = (c[0], c[1], c[2], c[3], c[4], c[5]);
                let (cdx, cdy, dr) = (x1 - x0, y1 - y0, r1 - r0);
                let (pdx, pdy) = (u - x0, v - y0);
                let a = cdx * cdx + cdy * cdy - dr * dr;
                let b = pdx * cdx + pdy * cdy + r0 * dr;
                let cc = pdx * pdx + pdy * pdy - r0 * r0;
                let s = if a.abs() < 1e-6 {
                    if b == 0.0 {
                        return self.bg;
                    }
                    cc / (2.0 * b)
                } else {
                    let disc = b * b - a * cc;
                    if disc < 0.0 {
                        return self.bg;
                    }
                    let sq = libm::sqrtf(disc);
                    let s1 = (b + sq) / a;
                    let s2 = (b - sq) / a;
                    if r0 + s1 * dr >= 0.0 {
                        s1
                    } else {
                        s2
                    }
                };
                s
            }
            _ => {
                // Function-based and mesh shadings: a middle colour.
                return Some(self.lut.get(128).copied().or(self.bg).unwrap_or(0x808080));
            }
        };
        let t = if t < 0.0 {
            if self.extend[0] {
                0.0
            } else {
                return self.bg;
            }
        } else if t > 1.0 {
            if self.extend[1] {
                1.0
            } else {
                return self.bg;
            }
        } else {
            t
        };
        if self.lut.is_empty() {
            let _ = (&self.cs, &self.func, self.domain);
            return Some(0x808080);
        }
        Some(self.lut[(t * 255.0) as usize])
    }
}

#[derive(Clone)]
struct TextState {
    font: Option<Rc<PdfFont>>,
    font_id: u32,
    size: f32,
    tc: f32,
    tw: f32,
    th: f32,
    tl: f32,
    rise: f32,
    mode: u8,
}

#[derive(Clone)]
struct GS {
    ctm: M,
    fill: Paint,
    stroke: Paint,
    fcs: Cs,
    scs: Cs,
    fa: f32,
    sa: f32,
    lw: f32,
    cap: u8,
    dash: (Vec<f32>, f32),
    clip: Option<Rc<Clip>>,
    /// Blend mode Multiply (highlighter-style).
    multiply: bool,
    ts: TextState,
}

/// Decoded images, shared between pages.
pub struct Img {
    pub w: usize,
    pub h: usize,
    /// 0xAARRGGBB (alpha 0 = transparent).
    pub px: Vec<u32>,
    pub mask_only: bool,
}

struct GlyphBmp {
    x0: i32,
    y0: i32,
    w: usize,
    h: usize,
    cov: Vec<u8>,
}

/// A PDF ready for drawing: the file plus caches of fonts, glyphs and pictures.
pub struct PdfDoc {
    pub file: PdfFile,
    fonts: RefCell<BTreeMap<u32, Rc<PdfFont>>>,
    glyphs: RefCell<BTreeMap<(u32, u32, [i32; 4], u8, u8), Rc<GlyphBmp>>>,
    images: RefCell<BTreeMap<u32, Rc<Img>>>,
    ocg_off: Vec<u32>,
    next_font: core::cell::Cell<u32>,
}

impl PdfDoc {
    pub fn new(file: PdfFile) -> PdfDoc {
        let mut ocg_off = Vec::new();
        let root = file.dget(&file.trailer, "Root");
        if let Some(cat) = root.as_dict() {
            if let Some(ocp) = file.dget(cat, "OCProperties").as_dict() {
                if let Some(d) = file.dget(ocp, "D").as_dict() {
                    if let Some(off) = file.dget(d, "OFF").as_array() {
                        for o in off {
                            if let Obj::Ref(n, _) = o {
                                ocg_off.push(*n);
                            }
                        }
                    }
                }
            }
        }
        PdfDoc { file, fonts: RefCell::new(BTreeMap::new()), glyphs: RefCell::new(BTreeMap::new()), images: RefCell::new(BTreeMap::new()), ocg_off, next_font: core::cell::Cell::new(1 << 30) }
    }

    /// Draw page `i` at `s` pixels per point (plus an extra rotation in degrees).
    pub fn render(&self, i: usize, s: f32, extra_rot: i32) -> Canvas {
        let page = &self.file.pages[i];
        let (w, h) = page_pixels(page, s, extra_rot);
        let mut c = Canvas::new(w, h);
        c.px.fill(0xFF_FFFF);
        let base = page_matrix(page, s, extra_rot);
        let mut r = Ren { doc: self, c: &mut c, raster: Raster::new(w, h), gs: Vec::new(), budget: 3_000_000, hidden: 0, marked: Vec::new(), pbase: Vec::new() };
        let gs = GS {
            ctm: base,
            fill: Paint::Color(0),
            stroke: Paint::Color(0),
            fcs: Cs::Gray,
            scs: Cs::Gray,
            fa: 1.0,
            sa: 1.0,
            lw: 1.0,
            cap: 0,
            dash: (Vec::new(), 0.0),
            clip: None,
            multiply: false,
            ts: TextState { font: None, font_id: 0, size: 12.0, tc: 0.0, tw: 0.0, th: 1.0, tl: 0.0, rise: 0.0, mode: 0 },
        };
        let content = self.page_content(page);
        r.run(&content, &page.resources, gs.clone(), 0);
        r.annotations(page, gs);
        if self.glyphs.borrow().len() > 20000 {
            self.glyphs.borrow_mut().clear();
        }
        if self.images.borrow().len() > 40 {
            self.images.borrow_mut().clear();
        }
        c
    }

    pub fn page_content(&self, page: &Page) -> Vec<u8> {
        let mut out = Vec::new();
        match self.file.dget(&page.dict, "Contents") {
            s @ Obj::Stream(..) => {
                if let Some((_, d)) = self.file.stream_data(&s) {
                    out = d;
                }
            }
            Obj::Array(a) => {
                for o in a {
                    if let Some((_, d)) = self.file.stream_data(&o) {
                        out.extend_from_slice(&d);
                        out.push(b'\n');
                    }
                }
            }
            _ => {}
        }
        out
    }

    fn font(&self, o: &Obj) -> (u32, Rc<PdfFont>) {
        if let Obj::Ref(u32::MAX, _) = o {
            if let Some(f) = self.fonts.borrow().get(&u32::MAX) {
                return (u32::MAX, f.clone());
            }
            let mut d = Dict::default();
            d.set("Subtype", Obj::Name(b"Type1".to_vec()));
            d.set("BaseFont", Obj::Name(b"Helvetica".to_vec()));
            d.set("Encoding", Obj::Name(b"WinAnsiEncoding".to_vec()));
            let f = Rc::new(PdfFont::load(&self.file, &d));
            self.fonts.borrow_mut().insert(u32::MAX, f.clone());
            return (u32::MAX, f);
        }
        let id = match o {
            Obj::Ref(n, _) => *n,
            _ => {
                let n = self.next_font.get();
                self.next_font.set(n + 1);
                n
            }
        };
        if let Some(f) = self.fonts.borrow().get(&id) {
            return (id, f.clone());
        }
        let d = self.file.resolve(o).as_dict().cloned().unwrap_or_default();
        let f = Rc::new(PdfFont::load(&self.file, &d));
        self.fonts.borrow_mut().insert(id, f.clone());
        (id, f)
    }
}

struct Ren<'a> {
    doc: &'a PdfDoc,
    c: &'a mut Canvas,
    raster: Raster,
    gs: Vec<GS>,
    budget: i64,
    /// Inside hidden optional content.
    hidden: u32,
    marked: Vec<bool>,
    /// Pattern space: the matrix each content stream started with.
    pbase: Vec<M>,
}

struct PathBuilder<'p> {
    m: M,
    out: &'p mut Vec<P>,
}
impl OutlineBuilder for PathBuilder<'_> {
    fn move_to(&mut self, x: f32, y: f32) {
        let (a, b) = apply(&self.m, x, y);
        self.out.push(P::Move(a, b));
    }
    fn line_to(&mut self, x: f32, y: f32) {
        let (a, b) = apply(&self.m, x, y);
        self.out.push(P::Line(a, b));
    }
    fn quad_to(&mut self, x1: f32, y1: f32, x: f32, y: f32) {
        // As a cubic.
        let last = match self.out.last() {
            Some(P::Move(a, b)) | Some(P::Line(a, b)) | Some(P::Cubic(_, _, _, _, a, b)) => (*a, *b),
            _ => apply(&self.m, x1, y1),
        };
        let (cx, cy) = apply(&self.m, x1, y1);
        let (ex, ey) = apply(&self.m, x, y);
        self.out.push(P::Cubic(last.0 + 2.0 / 3.0 * (cx - last.0), last.1 + 2.0 / 3.0 * (cy - last.1), ex + 2.0 / 3.0 * (cx - ex), ey + 2.0 / 3.0 * (cy - ey), ex, ey));
    }
    fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x: f32, y: f32) {
        let (a, b) = apply(&self.m, x1, y1);
        let (c, d) = apply(&self.m, x2, y2);
        let (e, f) = apply(&self.m, x, y);
        self.out.push(P::Cubic(a, b, c, d, e, f));
    }
    fn close(&mut self) {
        self.out.push(P::Close);
    }
}

fn feed(r: &mut Raster, path: &[P], dx: f32, dy: f32) {
    for p in path {
        match *p {
            P::Move(x, y) => r.move_to(x - dx, y - dy),
            P::Line(x, y) => r.line_to(x - dx, y - dy),
            P::Cubic(a, b, c, d, e, f) => r.cubic_to(a - dx, b - dy, c - dx, d - dy, e - dx, f - dy),
            P::Close => r.close(),
        }
    }
    r.close();
}

fn bbox(path: &[P]) -> Option<(f32, f32, f32, f32)> {
    let mut b: Option<(f32, f32, f32, f32)> = None;
    let mut add = |x: f32, y: f32| {
        b = Some(match b {
            None => (x, y, x, y),
            Some((a, c, d, e)) => (a.min(x), c.min(y), d.max(x), e.max(y)),
        })
    };
    for p in path {
        match *p {
            P::Move(x, y) | P::Line(x, y) => add(x, y),
            P::Cubic(a, b2, c, d, e, f) => {
                add(a, b2);
                add(c, d);
                add(e, f);
            }
            P::Close => {}
        }
    }
    b
}

/// Is the path a single axis-aligned rectangle? Then its device box.
fn as_rect(path: &[P]) -> Option<(f32, f32, f32, f32)> {
    let pts: Vec<(f32, f32)> = path.iter().filter_map(|p| if let P::Move(x, y) | P::Line(x, y) = p { Some((*x, *y)) } else { None }).collect();
    if path.iter().any(|p| matches!(p, P::Cubic(..))) || !(4..=5).contains(&pts.len()) || path.iter().filter(|p| matches!(p, P::Move(..))).count() != 1 {
        return None;
    }
    let near = |a: f32, b: f32| (a - b).abs() < 0.01;
    let p = &pts[..4];
    let horiz_first = near(p[0].1, p[1].1) && near(p[1].0, p[2].0) && near(p[2].1, p[3].1) && near(p[3].0, p[0].0);
    let vert_first = near(p[0].0, p[1].0) && near(p[1].1, p[2].1) && near(p[2].0, p[3].0) && near(p[3].1, p[0].1);
    if horiz_first || vert_first {
        bbox(path)
    } else {
        None
    }
}

impl Ren<'_> {
    fn gs(&self) -> &GS {
        self.gs.last().unwrap()
    }

    // ---- painting coverage --------------------------------------------------------------------

    /// Blend coverage rows from the raster with a paint.
    fn paint_raster(&mut self, evenodd: bool, paint: &Paint, alpha: f32, offset: (i32, i32)) {
        let clip = self.gs().clip.clone();
        let multiply = self.gs().multiply;
        let a = (alpha.clamp(0.0, 1.0) * 255.0 + 0.5) as u32;
        if a == 0 || self.hidden > 0 {
            self.raster.clear();
            return;
        }
        let (cw, ch) = (self.c.w as i32, self.c.h as i32);
        let px = &mut self.c.px;
        let clip_b = clip.as_ref().map(|c| c.bounds());
        self.raster.rows(evenodd, |y, x0, cov| {
            let yy = y as i32 + offset.1;
            if yy < 0 || yy >= ch {
                return;
            }
            if let Some((_, cy0, _, cy1)) = clip_b {
                if yy < cy0 || yy >= cy1 {
                    return;
                }
            }
            for (k, &cv) in cov.iter().enumerate() {
                if cv == 0 {
                    continue;
                }
                let xx = (x0 + k) as i32 + offset.0;
                if xx < 0 || xx >= cw {
                    continue;
                }
                let mut v = cv as u32 * a / 255;
                if let Some(c) = &clip {
                    v = v * c.at(xx, yy) / 255;
                    if v == 0 {
                        continue;
                    }
                }
                let col = match paint {
                    Paint::Color(c) => *c,
                    Paint::Shade(s) => match s.color(xx as f32 + 0.5, yy as f32 + 0.5) {
                        Some(c) => c,
                        None => continue,
                    },
                };
                let i = (yy * cw + xx) as usize;
                let col = if multiply { mul_rgb(px[i], col) } else { col };
                px[i] = if v >= 255 { col } else { mix(px[i], col, v) };
            }
        });
        self.raster.clear();
    }

    fn fill_path(&mut self, path: &[P], evenodd: bool) {
        if path.is_empty() {
            return;
        }
        let paint = self.gs().fill.clone();
        let fa = self.gs().fa;
        feed(&mut self.raster, path, 0.0, 0.0);
        self.paint_raster(evenodd, &paint, fa, (0, 0));
    }

    fn stroke_path(&mut self, path: &[P]) {
        let gs = self.gs().clone();
        let s = scale_of(&gs.ctm);
        let w = (gs.lw * s).max(1.0);
        let hw = w / 2.0;
        // Flatten into polylines.
        let mut lines: Vec<(Vec<(f32, f32)>, bool)> = Vec::new();
        let mut cur: Vec<(f32, f32)> = Vec::new();
        for p in path {
            match *p {
                P::Move(x, y) => {
                    if cur.len() > 1 {
                        lines.push((core::mem::take(&mut cur), false));
                    }
                    cur.clear();
                    cur.push((x, y));
                }
                P::Line(x, y) => cur.push((x, y)),
                P::Cubic(a, b, c, d, e, f) => {
                    let (x0, y0) = *cur.last().unwrap_or(&(a, b));
                    let len = (a - x0).abs() + (b - y0).abs() + (c - a).abs() + (d - b).abs() + (e - c).abs() + (f - d).abs();
                    let n = ((libm::sqrtf(len) * 1.5) as usize).clamp(2, 64);
                    for k in 1..=n {
                        let t = k as f32 / n as f32;
                        let mt = 1.0 - t;
                        let (q0, q1, q2, q3) = (mt * mt * mt, 3.0 * mt * mt * t, 3.0 * mt * t * t, t * t * t);
                        cur.push((q0 * x0 + q1 * a + q2 * c + q3 * e, q0 * y0 + q1 * b + q2 * d + q3 * f));
                    }
                }
                P::Close => {
                    if let Some(&first) = cur.first() {
                        cur.push(first);
                        lines.push((core::mem::take(&mut cur), true));
                        cur.push(first);
                    }
                }
            }
        }
        if cur.len() > 1 {
            lines.push((cur, false));
        }
        // Dashes.
        if !gs.dash.0.is_empty() && gs.dash.0.iter().any(|&d| d > 0.0) {
            let pat: Vec<f32> = gs.dash.0.iter().map(|d| (d * s).max(0.5)).collect();
            let mut out = Vec::new();
            for (pts, _) in &lines {
                let mut k = 0;
                let mut left = pat[0];
                let mut phase = gs.dash.1 * s;
                while phase > 0.0 {
                    if phase >= left {
                        phase -= left;
                        k = (k + 1) % pat.len();
                        left = pat[k];
                    } else {
                        left -= phase;
                        phase = 0.0;
                    }
                }
                let mut on: Vec<(f32, f32)> = Vec::new();
                if k % 2 == 0 {
                    on.push(pts[0]);
                }
                for seg in pts.windows(2) {
                    let (a, b) = (seg[0], seg[1]);
                    let mut len = libm::sqrtf((b.0 - a.0) * (b.0 - a.0) + (b.1 - a.1) * (b.1 - a.1));
                    let mut pos = a;
                    let dirx = if len > 0.0 { (b.0 - a.0) / len } else { 0.0 };
                    let diry = if len > 0.0 { (b.1 - a.1) / len } else { 0.0 };
                    while len > left {
                        pos = (pos.0 + dirx * left, pos.1 + diry * left);
                        len -= left;
                        if k % 2 == 0 {
                            on.push(pos);
                            out.push((core::mem::take(&mut on), false));
                        } else {
                            on.push(pos);
                        }
                        k = (k + 1) % pat.len();
                        left = pat[k];
                        if out.len() > 20000 {
                            break;
                        }
                    }
                    left -= len;
                    if k % 2 == 0 {
                        on.push(b);
                    }
                }
                if k % 2 == 0 && on.len() > 1 {
                    out.push((on, false));
                }
            }
            lines = out;
        }
        let r = &mut self.raster;
        let circle = |r: &mut Raster, cx: f32, cy: f32| {
            let n = ((hw * 2.0) as usize).clamp(8, 32);
            r.move_to(cx + hw, cy);
            for k in 1..n {
                let t = k as f32 * core::f32::consts::TAU / n as f32;
                r.line_to(cx + hw * libm::cosf(t), cy + hw * libm::sinf(t));
            }
            r.close();
        };
        for (pts, closed) in &lines {
            for seg in pts.windows(2) {
                let (a, b) = (seg[0], seg[1]);
                let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                let len = libm::sqrtf(dx * dx + dy * dy);
                if len < 1e-4 {
                    continue;
                }
                let (nx, ny) = (-dy / len * hw, dx / len * hw);
                // Same winding for every piece so overlaps add up instead of cancelling.
                let quad = [(a.0 + nx, a.1 + ny), (b.0 + nx, b.1 + ny), (b.0 - nx, b.1 - ny), (a.0 - nx, a.1 - ny)];
                let area: f32 = (0..4).map(|i| quad[i].0 * quad[(i + 1) % 4].1 - quad[(i + 1) % 4].0 * quad[i].1).sum();
                let order: [usize; 4] = if area > 0.0 { [0, 1, 2, 3] } else { [3, 2, 1, 0] };
                r.move_to(quad[order[0]].0, quad[order[0]].1);
                for &i in &order[1..] {
                    r.line_to(quad[i].0, quad[i].1);
                }
                r.close();
            }
            if hw > 1.0 {
                let n = pts.len();
                for (i, &(x, y)) in pts.iter().enumerate() {
                    let end = !closed && (i == 0 || i == n - 1);
                    if !end || gs.cap == 1 {
                        circle(r, x, y);
                    }
                }
            }
            if gs.cap == 2 && !closed && pts.len() > 1 {
                for (p, q) in [(pts[0], pts[1]), (pts[pts.len() - 1], pts[pts.len() - 2])] {
                    let (dx, dy) = (p.0 - q.0, p.1 - q.1);
                    let len = libm::sqrtf(dx * dx + dy * dy).max(1e-4);
                    let (ux, uy) = (dx / len * hw, dy / len * hw);
                    let (nx, ny) = (-uy, ux);
                    let quad = [(p.0 + nx, p.1 + ny), (p.0 + nx + ux, p.1 + ny + uy), (p.0 - nx + ux, p.1 - ny + uy), (p.0 - nx, p.1 - ny)];
                    let area: f32 = (0..4).map(|i| quad[i].0 * quad[(i + 1) % 4].1 - quad[(i + 1) % 4].0 * quad[i].1).sum();
                    let order: [usize; 4] = if area > 0.0 { [0, 1, 2, 3] } else { [3, 2, 1, 0] };
                    r.move_to(quad[order[0]].0, quad[order[0]].1);
                    for &i in &order[1..] {
                        r.line_to(quad[i].0, quad[i].1);
                    }
                    r.close();
                }
            }
        }
        let paint = gs.stroke.clone();
        // Circles were drawn the same way round as the quads only by chance: use even-odd
        // safe nonzero by taking the absolute winding (the rasteriser's nonzero does that).
        self.paint_raster(false, &paint, gs.sa, (0, 0));
    }

    fn set_clip(&mut self, path: &[P], evenodd: bool) {
        let (cw, ch) = (self.c.w as i32, self.c.h as i32);
        let new = if let Some((x0, y0, x1, y1)) = as_rect(path) {
            let r = ((x0 + 0.5) as i32, (y0 + 0.5) as i32, (x1 + 0.5) as i32, (y1 + 0.5) as i32);
            let oldc = self.gs().clip.clone();
            match oldc.as_deref() {
                None => Clip::Rect(r.0.max(0), r.1.max(0), r.2.min(cw), r.3.min(ch)),
                Some(Clip::Rect(a, b, c, d)) => Clip::Rect(r.0.max(*a), r.1.max(*b), r.2.min(*c), r.3.min(*d)),
                Some(old) => self.mask_clip(path, evenodd, old),
            }
        } else {
            match self.gs().clip.clone() {
                Some(old) => self.mask_clip(path, evenodd, &old),
                None => self.mask_clip(path, evenodd, &Clip::Rect(0, 0, cw, ch)),
            }
        };
        self.gs.last_mut().unwrap().clip = Some(Rc::new(new));
    }

    fn mask_clip(&self, path: &[P], evenodd: bool, old: &Clip) -> Clip {
        let (ox0, oy0, ox1, oy1) = old.bounds();
        let Some((bx0, by0, bx1, by1)) = bbox(path) else { return Clip::Rect(0, 0, 0, 0) };
        let x0 = (bx0 as i32).max(ox0).max(0);
        let y0 = (by0 as i32).max(oy0).max(0);
        let x1 = ((bx1 + 1.0) as i32).min(ox1).min(self.c.w as i32);
        let y1 = ((by1 + 1.0) as i32).min(oy1).min(self.c.h as i32);
        if x1 <= x0 || y1 <= y0 {
            return Clip::Rect(0, 0, 0, 0);
        }
        let (w, h) = (x1 - x0, y1 - y0);
        let mut r = Raster::new(w as usize, h as usize);
        feed(&mut r, path, x0 as f32, y0 as f32);
        let mut a = r.mask(evenodd);
        for v in 0..h {
            for u in 0..w {
                let i = (v * w + u) as usize;
                if a[i] != 0 {
                    a[i] = (a[i] as u32 * old.at(x0 + u, y0 + v) / 255) as u8;
                }
            }
        }
        Clip::Mask { x0, y0, w, h, a }
    }

    // ---- the interpreter ------------------------------------------------------------------------

    fn run(&mut self, content: &[u8], res: &Dict, gs: GS, depth: u32) {
        if depth > 12 {
            return;
        }
        let base_len = self.gs.len();
        self.pbase.push(gs.ctm);
        self.gs.push(gs);
        let mut lx = Lexer::new(content, 0);
        let mut ops: Vec<Obj> = Vec::new();
        let mut path: Vec<P> = Vec::new();
        let mut cur = (0.0f32, 0.0f32);
        let mut start = (0.0f32, 0.0f32);
        let mut pending_clip: Option<bool> = None;
        let mut tm = ID;
        let mut tlm = ID;
        let file = &self.doc.file;
        let num = |ops: &[Obj], i: usize| ops.get(i).and_then(|o| o.as_f32()).unwrap_or(0.0);
        loop {
            self.budget -= 1;
            if self.budget < 0 {
                break;
            }
            lx.skip_ws();
            let save = lx.p;
            let Some(t) = lx.tok() else { break };
            let op: &[u8] = match t {
                Tok::Word(w) => w,
                Tok::Obj(_) | Tok::ArrayStart | Tok::DictStart => {
                    lx.p = save;
                    match lx.obj() {
                        Some(o) => ops.push(o),
                        None => {
                            if lx.p == save {
                                lx.p += 1;
                            }
                        }
                    }
                    if ops.len() > 100 {
                        ops.clear();
                    }
                    continue;
                }
                _ => continue,
            };
            let ctm = self.gs().ctm;
            let tr = |x: f32, y: f32| apply(&ctm, x, y);
            match op {
                // Graphics state.
                b"q" => {
                    if self.gs.len() < 200 {
                        let g = self.gs().clone();
                        self.gs.push(g);
                    }
                }
                b"Q" => {
                    if self.gs.len() > base_len + 1 {
                        self.gs.pop();
                    }
                }
                b"cm" if ops.len() >= 6 => {
                    let m = [num(&ops, 0), num(&ops, 1), num(&ops, 2), num(&ops, 3), num(&ops, 4), num(&ops, 5)];
                    let g = self.gs.last_mut().unwrap();
                    g.ctm = mul(&m, &g.ctm);
                }
                b"w" => self.gs.last_mut().unwrap().lw = num(&ops, 0),
                b"J" => self.gs.last_mut().unwrap().cap = num(&ops, 0) as u8,
                b"d" => {
                    let arr: Vec<f32> = ops.first().and_then(|o| o.as_array()).map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default();
                    self.gs.last_mut().unwrap().dash = (arr, num(&ops, 1));
                }
                b"gs" => {
                    if let Some(name) = ops.first().and_then(|o| o.as_name()) {
                        let egs = file.dget(res, "ExtGState");
                        if let Some(e) = egs.as_dict().map(|d| file.dget(d, core::str::from_utf8(name).unwrap_or(""))) {
                            if let Some(e) = e.as_dict() {
                                let g = self.gs.last_mut().unwrap();
                                if let Some(v) = file.dget(e, "ca").as_f32() {
                                    g.fa = v;
                                }
                                if let Some(v) = file.dget(e, "CA").as_f32() {
                                    g.sa = v;
                                }
                                if let Some(bm) = file.dget(e, "BM").as_name() {
                                    g.multiply = bm == b"Multiply";
                                }
                                if let Some(v) = file.dget(e, "LW").as_f32() {
                                    g.lw = v;
                                }
                                if let Obj::Array(fnt) = file.dget(e, "Font") {
                                    if let (Some(f), Some(sz)) = (fnt.first(), fnt.get(1).and_then(|x| x.as_f32())) {
                                        let (id, font) = self.doc.font(f);
                                        let g = self.gs.last_mut().unwrap();
                                        g.ts.font = Some(font);
                                        g.ts.font_id = id;
                                        g.ts.size = sz;
                                    }
                                }
                            }
                        }
                    }
                }
                // Paths.
                b"m" => {
                    cur = tr(num(&ops, 0), num(&ops, 1));
                    start = cur;
                    path.push(P::Move(cur.0, cur.1));
                }
                b"l" => {
                    cur = tr(num(&ops, 0), num(&ops, 1));
                    path.push(P::Line(cur.0, cur.1));
                }
                b"c" => {
                    let (a, b) = tr(num(&ops, 0), num(&ops, 1));
                    let (c, d) = tr(num(&ops, 2), num(&ops, 3));
                    cur = tr(num(&ops, 4), num(&ops, 5));
                    path.push(P::Cubic(a, b, c, d, cur.0, cur.1));
                }
                b"v" => {
                    let (c, d) = tr(num(&ops, 0), num(&ops, 1));
                    let p0 = cur;
                    cur = tr(num(&ops, 2), num(&ops, 3));
                    path.push(P::Cubic(p0.0, p0.1, c, d, cur.0, cur.1));
                }
                b"y" => {
                    let (a, b) = tr(num(&ops, 0), num(&ops, 1));
                    cur = tr(num(&ops, 2), num(&ops, 3));
                    path.push(P::Cubic(a, b, cur.0, cur.1, cur.0, cur.1));
                }
                b"h" => {
                    path.push(P::Close);
                    cur = start;
                }
                b"re" => {
                    let (x, y, w, h) = (num(&ops, 0), num(&ops, 1), num(&ops, 2), num(&ops, 3));
                    let p0 = tr(x, y);
                    let p1 = tr(x + w, y);
                    let p2 = tr(x + w, y + h);
                    let p3 = tr(x, y + h);
                    path.push(P::Move(p0.0, p0.1));
                    path.push(P::Line(p1.0, p1.1));
                    path.push(P::Line(p2.0, p2.1));
                    path.push(P::Line(p3.0, p3.1));
                    path.push(P::Close);
                    cur = p0;
                    start = p0;
                }
                b"S" | b"s" | b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*" | b"n" => {
                    if matches!(op, b"s" | b"b" | b"b*") {
                        path.push(P::Close);
                    }
                    let fill = matches!(op, b"f" | b"F" | b"f*" | b"B" | b"B*" | b"b" | b"b*");
                    let stroke = matches!(op, b"S" | b"s" | b"B" | b"B*" | b"b" | b"b*");
                    let eo = matches!(op, b"f*" | b"B*" | b"b*");
                    if fill {
                        self.fill_path(&path, eo);
                    }
                    if stroke {
                        self.stroke_path(&path);
                    }
                    if let Some(ceo) = pending_clip.take() {
                        self.set_clip(&path, ceo);
                    }
                    path.clear();
                }
                b"W" => pending_clip = Some(false),
                b"W*" => pending_clip = Some(true),
                // Colour.
                b"CS" | b"cs" => {
                    let cs = ops.first().map(|o| load_cs(file, o, Some(res), 0)).unwrap_or(Cs::Gray);
                    let init = cs.rgb(&cs.initial());
                    let g = self.gs.last_mut().unwrap();
                    if op == b"CS" {
                        g.scs = cs;
                        g.stroke = Paint::Color(init);
                    } else {
                        g.fcs = cs;
                        g.fill = Paint::Color(init);
                    }
                }
                b"SC" | b"SCN" | b"sc" | b"scn" => {
                    let fill = op[0] == b's';
                    let pattern = ops.last().and_then(|o| o.as_name()).map(|n| n.to_vec());
                    let cs = if fill { self.gs().fcs.clone() } else { self.gs().scs.clone() };
                    let paint = if let (Cs::Pattern, Some(pn)) = (&cs, pattern) {
                        self.pattern_paint(&pn, res, ctm)
                    } else {
                        let v: Vec<f32> = ops.iter().filter_map(|o| o.as_f32()).collect();
                        Paint::Color(cs.rgb(&v))
                    };
                    let g = self.gs.last_mut().unwrap();
                    if fill {
                        g.fill = paint;
                    } else {
                        g.stroke = paint;
                    }
                }
                b"g" | b"G" | b"rg" | b"RG" | b"k" | b"K" => {
                    let cs = match op {
                        b"g" | b"G" => Cs::Gray,
                        b"rg" | b"RG" => Cs::Rgb,
                        _ => Cs::Cmyk,
                    };
                    let v: Vec<f32> = ops.iter().filter_map(|o| o.as_f32()).collect();
                    let c = cs.rgb(&v);
                    let g = self.gs.last_mut().unwrap();
                    if op[0].is_ascii_lowercase() {
                        g.fill = Paint::Color(c);
                        g.fcs = cs;
                    } else {
                        g.stroke = Paint::Color(c);
                        g.scs = cs;
                    }
                }
                b"sh" => {
                    if let Some(name) = ops.first().and_then(|o| o.as_name()) {
                        let sd = file.dget(res, "Shading");
                        if let Some(o) = sd.as_dict().and_then(|d| d.get(core::str::from_utf8(name).unwrap_or("")).cloned()) {
                            if let Some(sh) = Shading::load(file, &o, res, &ctm) {
                                let (cw, ch) = (self.c.w as f32, self.c.h as f32);
                                let full = [P::Move(0.0, 0.0), P::Line(cw, 0.0), P::Line(cw, ch), P::Line(0.0, ch), P::Close];
                                feed(&mut self.raster, &full, 0.0, 0.0);
                                let fa = self.gs().fa;
                                self.paint_raster(false, &Paint::Shade(Rc::new(sh)), fa, (0, 0));
                            }
                        }
                    }
                }
                // Text.
                b"BT" => {
                    tm = ID;
                    tlm = ID;
                }
                b"ET" => {}
                b"Tc" => self.gs.last_mut().unwrap().ts.tc = num(&ops, 0),
                b"Tw" => self.gs.last_mut().unwrap().ts.tw = num(&ops, 0),
                b"Tz" => self.gs.last_mut().unwrap().ts.th = num(&ops, 0) / 100.0,
                b"TL" => self.gs.last_mut().unwrap().ts.tl = num(&ops, 0),
                b"Ts" => self.gs.last_mut().unwrap().ts.rise = num(&ops, 0),
                b"Tr" => self.gs.last_mut().unwrap().ts.mode = num(&ops, 0) as u8,
                b"Tf" => {
                    if let Some(name) = ops.first().and_then(|o| o.as_name()) {
                        let fd = file.dget(res, "Font");
                        if let Some(fo) = fd.as_dict().and_then(|d| d.get(core::str::from_utf8(name).unwrap_or("")).cloned()) {
                            let (id, font) = self.doc.font(&fo);
                            let g = self.gs.last_mut().unwrap();
                            g.ts.font = Some(font);
                            g.ts.font_id = id;
                        }
                    }
                    self.gs.last_mut().unwrap().ts.size = num(&ops, 1);
                }
                b"Td" | b"TD" => {
                    let (tx, ty) = (num(&ops, 0), num(&ops, 1));
                    if op == b"TD" {
                        self.gs.last_mut().unwrap().ts.tl = -ty;
                    }
                    tlm = mul(&[1.0, 0.0, 0.0, 1.0, tx, ty], &tlm);
                    tm = tlm;
                }
                b"Tm" => {
                    tlm = [num(&ops, 0), num(&ops, 1), num(&ops, 2), num(&ops, 3), num(&ops, 4), num(&ops, 5)];
                    tm = tlm;
                }
                b"T*" => {
                    let tl = self.gs().ts.tl;
                    tlm = mul(&[1.0, 0.0, 0.0, 1.0, 0.0, -tl], &tlm);
                    tm = tlm;
                }
                b"Tj" | b"'" | b"\"" | b"TJ" => {
                    if op == b"'" || op == b"\"" {
                        if op == b"\"" {
                            let g = self.gs.last_mut().unwrap();
                            g.ts.tw = num(&ops, 0);
                            g.ts.tc = num(&ops, 1);
                        }
                        let tl = self.gs().ts.tl;
                        tlm = mul(&[1.0, 0.0, 0.0, 1.0, 0.0, -tl], &tlm);
                        tm = tlm;
                    }
                    match op {
                        b"TJ" => {
                            if let Some(arr) = ops.first().and_then(|o| o.as_array()) {
                                for item in arr {
                                    match item {
                                        Obj::Str(s) => self.show(s, &mut tm, res, depth),
                                        o => {
                                            let adj = o.as_f32().unwrap_or(0.0);
                                            let ts = &self.gs().ts;
                                            let tx = -adj / 1000.0 * ts.size * ts.th;
                                            tm = mul(&[1.0, 0.0, 0.0, 1.0, tx, 0.0], &tm);
                                        }
                                    }
                                }
                            }
                        }
                        _ => {
                            if let Some(s) = ops.last().and_then(|o| o.as_str()) {
                                let s = s.to_vec();
                                self.show(&s, &mut tm, res, depth);
                            }
                        }
                    }
                }
                // Objects.
                b"Do" => {
                    if let Some(name) = ops.first().and_then(|o| o.as_name()) {
                        let xd = file.dget(res, "XObject");
                        if let Some(xo) = xd.as_dict().and_then(|d| d.get(core::str::from_utf8(name).unwrap_or("")).cloned()) {
                            self.xobject(&xo, res, depth);
                        }
                    }
                }
                b"BI" => {
                    // Inline image: key/value pairs, ID, data, EI.
                    let mut d = Dict::default();
                    loop {
                        match lx.obj() {
                            Some(Obj::Name(k)) => {
                                let v = lx.obj().unwrap_or(Obj::Null);
                                d.0.push((k, v));
                            }
                            _ => break,
                        }
                    }
                    // lx is just past "ID"; one whitespace byte follows.
                    let data_start = (lx.p + 1).min(content.len());
                    let mut end = data_start;
                    while end + 2 < content.len() {
                        if &content[end..end + 2] == b"EI" && (end == 0 || super::obj::is_ws(content[end - 1])) && (end + 2 >= content.len() || super::obj::is_ws(content[end + 2])) {
                            break;
                        }
                        end += 1;
                    }
                    let data = content[data_start..end.min(content.len())].to_vec();
                    lx.p = (end + 2).min(content.len());
                    let d = expand_inline(d);
                    self.image(&Obj::Stream(d, data), res, None);
                }
                b"BDC" | b"BMC" => {
                    let hide = op == b"BDC"
                        && ops.first().and_then(|o| o.as_name()) == Some(b"OC")
                        && ops.get(1).and_then(|o| o.as_name()).and_then(|n| {
                            let props = file.dget(res, "Properties");
                            props.as_dict().and_then(|p| p.get(core::str::from_utf8(n).unwrap_or("")).cloned())
                        }).is_some_and(|o| matches!(o, Obj::Ref(n, _) if self.doc.ocg_off.contains(&n)));
                    if hide {
                        self.hidden += 1;
                    }
                    self.marked.push(hide);
                }
                b"EMC" => {
                    if self.marked.pop() == Some(true) {
                        self.hidden = self.hidden.saturating_sub(1);
                    }
                }
                _ => {}
            }
            ops.clear();
        }
        self.gs.truncate(base_len);
        self.pbase.pop();
    }

    fn pattern_paint(&mut self, name: &[u8], res: &Dict, _ctm: M) -> Paint {
        let file = &self.doc.file;
        let pd = file.dget(res, "Pattern");
        let Some(po) = pd.as_dict().and_then(|d| d.get(core::str::from_utf8(name).unwrap_or("")).cloned()) else { return Paint::Color(0x808080) };
        let p = file.resolve(&po);
        let Some(d) = p.as_dict() else { return Paint::Color(0x808080) };
        let m: Vec<f32> = file.dget(d, "Matrix").as_array().map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default();
        let pm = if m.len() == 6 { [m[0], m[1], m[2], m[3], m[4], m[5]] } else { ID };
        // Patterns live in the space of the page (the default CTM at the start).
        let base = self.pbase.last().copied().unwrap_or(ID);
        if file.dget(d, "PatternType").as_i64() == Some(2) {
            if let Some(sh) = d.get("Shading").and_then(|s| Shading::load(file, s, res, &mul(&pm, &base))) {
                return Paint::Shade(Rc::new(sh));
            }
        }
        // Tiling patterns: a soft grey stands in.
        Paint::Color(0xC8C8C8)
    }

    fn show(&mut self, s: &[u8], tm: &mut M, res: &Dict, depth: u32) {
        let gs = self.gs().clone();
        let Some(font) = gs.ts.font.clone() else { return };
        let ts = &gs.ts;
        for code in font.codes(s) {
            let w0 = font.width(code) / 1000.0;
            let trm = mul(&mul(&[ts.size * ts.th, 0.0, 0.0, ts.size, 0.0, ts.rise], tm), &gs.ctm);
            if ts.mode != 3 && ts.mode != 7 && self.hidden == 0 {
                if let Some(t3) = &font.type3 {
                    if let Some(proc_) = t3.procs.get(&(code & 0xFFFF)) {
                        if let Some((_, data)) = self.doc.file.stream_data(proc_) {
                            let m = mul(&t3.matrix, &trm);
                            let mut g2 = gs.clone();
                            g2.ctm = m;
                            let r = t3.resources.clone().unwrap_or_else(|| res.clone());
                            if depth < 6 {
                                self.run(&data, &r, g2, depth + 1);
                            }
                        }
                    }
                } else {
                    self.glyph(&font, gs.ts.font_id, code, &trm, &gs);
                }
            }
            let space = if font.is_space(code) { ts.tw } else { 0.0 };
            let tx = (w0 * ts.size + ts.tc + space) * ts.th;
            *tm = mul(&[1.0, 0.0, 0.0, 1.0, tx, 0.0], tm);
        }
    }

    fn glyph(&mut self, font: &PdfFont, font_id: u32, code: u32, trm: &M, gs: &GS) {
        let (ox, oy) = (trm[4], trm[5]);
        if ox < -500.0 || oy < -500.0 || ox > self.c.w as f32 + 500.0 || oy > self.c.h as f32 + 500.0 {
            return;
        }
        let q = |v: f32| (v * 256.0) as i32;
        let (fx, fy) = (libm::floorf(ox), libm::floorf(oy));
        let (sx, sy) = (((ox - fx) * 4.0) as u8, ((oy - fy) * 4.0) as u8);
        let key = (font_id, code, [q(trm[0]), q(trm[1]), q(trm[2]), q(trm[3])], sx, sy);
        let cached = self.doc.glyphs.borrow().get(&key).cloned();
        let bmp = match cached {
            Some(b) => b,
            None => {
                let m = [trm[0], trm[1], trm[2], trm[3], sx as f32 / 4.0, sy as f32 / 4.0];
                let mut path = Vec::new();
                font.outline(code, &mut PathBuilder { m, out: &mut path });
                let b = match bbox(&path) {
                    Some((x0, y0, x1, y1)) if x1 - x0 < 2000.0 && y1 - y0 < 2000.0 => {
                        let (gx0, gy0) = (libm::floorf(x0) as i32 - 1, libm::floorf(y0) as i32 - 1);
                        let (w, h) = ((libm::ceilf(x1) as i32 - gx0 + 1) as usize, (libm::ceilf(y1) as i32 - gy0 + 1) as usize);
                        let mut r = Raster::new(w, h);
                        feed(&mut r, &path, gx0 as f32, gy0 as f32);
                        GlyphBmp { x0: gx0, y0: gy0, w, h, cov: r.mask(false) }
                    }
                    _ => GlyphBmp { x0: 0, y0: 0, w: 0, h: 0, cov: Vec::new() },
                };
                let b = Rc::new(b);
                self.doc.glyphs.borrow_mut().insert(key, b.clone());
                b
            }
        };
        if bmp.w == 0 {
            return;
        }
        let paint = if matches!(gs.ts.mode, 1 | 5) { gs.stroke.clone() } else { gs.fill.clone() };
        let a = (gs.fa.clamp(0.0, 1.0) * 255.0) as u32;
        let (bx, by) = (fx as i32 + bmp.x0, fy as i32 + bmp.y0);
        let (cw, ch) = (self.c.w as i32, self.c.h as i32);
        let clip = gs.clip.clone();
        for v in 0..bmp.h as i32 {
            let yy = by + v;
            if yy < 0 || yy >= ch {
                continue;
            }
            for u in 0..bmp.w as i32 {
                let cv = bmp.cov[(v * bmp.w as i32 + u) as usize] as u32;
                if cv == 0 {
                    continue;
                }
                let xx = bx + u;
                if xx < 0 || xx >= cw {
                    continue;
                }
                let mut al = cv * a / 255;
                if let Some(c) = &clip {
                    al = al * c.at(xx, yy) / 255;
                    if al == 0 {
                        continue;
                    }
                }
                let col = match &paint {
                    Paint::Color(c) => *c,
                    Paint::Shade(s) => s.color(xx as f32 + 0.5, yy as f32 + 0.5).unwrap_or(0),
                };
                let i = (yy * cw + xx) as usize;
                self.c.px[i] = if al >= 255 { col } else { mix(self.c.px[i], col, al) };
            }
        }
    }

    fn xobject(&mut self, xo: &Obj, res: &Dict, depth: u32) {
        let file = &self.doc.file;
        let o = file.resolve(xo);
        let Some(d) = o.as_dict() else { return };
        if let Some(oc) = d.get("OC") {
            if let Obj::Ref(n, _) = oc {
                if self.doc.ocg_off.contains(n) {
                    return;
                }
            }
        }
        if d.is("Subtype", "Image") {
            let id = if let Obj::Ref(n, _) = xo { Some(*n) } else { None };
            self.image(&o, res, id);
        } else if d.is("Subtype", "Form") {
            let Some((fd, data)) = file.stream_data(&o) else { return };
            let m: Vec<f32> = file.dget(&fd, "Matrix").as_array().map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default();
            let fm = if m.len() == 6 { [m[0], m[1], m[2], m[3], m[4], m[5]] } else { ID };
            let r = match file.dget(&fd, "Resources") {
                Obj::Dict(r) => r,
                _ => res.clone(),
            };
            self.form(&data, &fd, fm, &r, depth);
        }
    }

    fn form(&mut self, data: &[u8], fd: &Dict, fm: M, res: &Dict, depth: u32) {
        let file = &self.doc.file;
        let mut g = self.gs().clone();
        g.ctm = mul(&fm, &g.ctm);
        let bb: Vec<f32> = file.dget(fd, "BBox").as_array().map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default();
        self.gs.push(g.clone());
        if bb.len() == 4 {
            let tr = |x, y| apply(&g.ctm, x, y);
            let (p0, p1, p2, p3) = (tr(bb[0], bb[1]), tr(bb[2], bb[1]), tr(bb[2], bb[3]), tr(bb[0], bb[3]));
            self.set_clip(&[P::Move(p0.0, p0.1), P::Line(p1.0, p1.1), P::Line(p2.0, p2.1), P::Line(p3.0, p3.1), P::Close], false);
        }
        let g2 = self.gs().clone();
        self.gs.pop();
        self.run(data, res, g2, depth + 1);
    }

    fn image(&mut self, o: &Obj, res: &Dict, id: Option<u32>) {
        if self.hidden > 0 {
            return;
        }
        let gs = self.gs().clone();
        let img = match id.and_then(|i| self.doc.images.borrow().get(&i).cloned()) {
            Some(i) => i,
            None => {
                let Some(img) = decode_image(&self.doc.file, o, res) else { return };
                let img = Rc::new(img);
                if let Some(i) = id {
                    self.doc.images.borrow_mut().insert(i, img.clone());
                }
                img
            }
        };
        let color = match gs.fill {
            Paint::Color(c) => c,
            _ => 0,
        };
        draw_image(self.c, &img, &gs.ctm, gs.clip.as_deref(), (gs.fa.clamp(0.0, 1.0) * 255.0) as u32, color);
    }

    fn annotations(&mut self, page: &Page, gs: GS) {
        let file = &self.doc.file;
        let Obj::Array(annots) = file.dget(&page.dict, "Annots") else { return };
        for a in annots.iter().take(500) {
            let ad = file.resolve(a);
            let Some(d) = ad.as_dict() else { continue };
            let flags = file.dget(d, "F").as_i64().unwrap_or(0);
            if flags & (2 | 32) != 0 || d.is("Subtype", "Popup") || d.is("Subtype", "Link") {
                continue;
            }
            let ap = file.dget(d, "AP");
            let Some(apd) = ap.as_dict() else { continue };
            let mut n = file.dget(apd, "N");
            if let Obj::Dict(states) = &n {
                let st = file.dget(d, "AS");
                n = match st.as_name().and_then(|s| states.get(core::str::from_utf8(s).unwrap_or(""))) {
                    Some(o) => file.resolve(o),
                    None => continue,
                };
            }
            let Some((fd, data)) = file.stream_data(&n) else { continue };
            let rect: Vec<f32> = file.dget(d, "Rect").as_array().map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default();
            let bb: Vec<f32> = file.dget(&fd, "BBox").as_array().map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default();
            if rect.len() < 4 || bb.len() < 4 {
                continue;
            }
            let m: Vec<f32> = file.dget(&fd, "Matrix").as_array().map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default();
            let fm = if m.len() == 6 { [m[0], m[1], m[2], m[3], m[4], m[5]] } else { ID };
            // Map the transformed bounding box onto the annotation's rectangle.
            let pts = [apply(&fm, bb[0], bb[1]), apply(&fm, bb[2], bb[1]), apply(&fm, bb[2], bb[3]), apply(&fm, bb[0], bb[3])];
            let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
            for (x, y) in pts {
                x0 = x0.min(x);
                y0 = y0.min(y);
                x1 = x1.max(x);
                y1 = y1.max(y);
            }
            let (rx0, ry0, rx1, ry1) = (rect[0].min(rect[2]), rect[1].min(rect[3]), rect[0].max(rect[2]), rect[1].max(rect[3]));
            let sx = if x1 > x0 { (rx1 - rx0) / (x1 - x0) } else { 1.0 };
            let sy = if y1 > y0 { (ry1 - ry0) / (y1 - y0) } else { 1.0 };
            let a_m = [sx, 0.0, 0.0, sy, rx0 - x0 * sx, ry0 - y0 * sy];
            let r = match file.dget(&fd, "Resources") {
                Obj::Dict(r) => r,
                _ => page.resources.clone(),
            };
            self.gs.push(gs.clone());
            let full = mul(&fm, &a_m);
            self.form(&data, &fd, full, &r, 1);
            self.gs.pop();
        }
    }
}

fn expand_inline(d: Dict) -> Dict {
    let mut out = Dict::default();
    for (k, v) in d.0 {
        let key: &[u8] = match k.as_slice() {
            b"W" => b"Width",
            b"H" => b"Height",
            b"BPC" => b"BitsPerComponent",
            b"CS" => b"ColorSpace",
            b"D" => b"Decode",
            b"DP" => b"DecodeParms",
            b"F" => b"Filter",
            b"IM" => b"ImageMask",
            b"I" => b"Interpolate",
            other => other,
        };
        let v = match v {
            Obj::Name(n) => Obj::Name(match n.as_slice() {
                b"G" => b"DeviceGray".to_vec(),
                b"RGB" => b"DeviceRGB".to_vec(),
                b"CMYK" => b"DeviceCMYK".to_vec(),
                b"AHx" => b"ASCIIHexDecode".to_vec(),
                b"A85" => b"ASCII85Decode".to_vec(),
                b"LZW" => b"LZWDecode".to_vec(),
                b"Fl" => b"FlateDecode".to_vec(),
                b"RL" => b"RunLengthDecode".to_vec(),
                b"DCT" => b"DCTDecode".to_vec(),
                b"CCF" => b"CCITTFaxDecode".to_vec(),
                _ => n,
            }),
            o => o,
        };
        out.0.push((key.to_vec(), v));
    }
    out
}

/// Decode an image XObject (or inline image) to ARGB.
pub fn decode_image(file: &PdfFile, o: &Obj, res: &Dict) -> Option<Img> {
    let o = file.resolve(o);
    let Obj::Stream(d0, raw) = &o else { return None };
    let mut d = d0.clone();
    for k in ["Filter", "DecodeParms", "ColorSpace", "Decode", "Width", "Height", "BitsPerComponent", "ImageMask", "SMask", "Mask"] {
        if let Some(v) = d0.get(k) {
            if !matches!(k, "SMask" | "Mask") {
                d.set(k, file.resolve(v));
            }
        }
    }
    let w = d.get("Width").and_then(|x| x.as_i64()).unwrap_or(0).clamp(0, 20000) as usize;
    let h = d.get("Height").and_then(|x| x.as_i64()).unwrap_or(0).clamp(0, 20000) as usize;
    if w == 0 || h == 0 || w * h > 80_000_000 {
        return None;
    }
    let mask = matches!(d.get("ImageMask"), Some(Obj::Bool(true)));
    let (data, left) = super::filter::decode(&d, raw);
    let decode: Vec<f32> = d.get("Decode").and_then(|x| x.as_array()).map(|a| a.iter().map(|x| x.as_f32().unwrap_or(0.0)).collect()).unwrap_or_default();
    let mut px = vec![0xFF80_8080u32; w * h];
    match left.as_ref().map(|(n, _)| n.as_slice()) {
        Some(b"DCTDecode") | Some(b"DCT") => {
            let im = crate::image::decode(&data).ok()?;
            let invert = decode.first().copied() == Some(1.0);
            for y in 0..h.min(im.h) {
                for x in 0..w.min(im.w) {
                    let mut p = im.px[y * im.w + x] | 0xFF00_0000;
                    if invert {
                        p = (p & 0xFF00_0000) | (!p & 0xFF_FFFF);
                    }
                    px[y * w + x] = p;
                }
            }
        }
        Some(_) => {
            // JPEG 2000, fax and JBIG2 pictures aren't decoded yet: a light grey box.
            for p in px.iter_mut() {
                *p = 0xFFE8_E8E8;
            }
        }
        None => {
            let bpc = if mask { 1 } else { d.get("BitsPerComponent").and_then(|x| x.as_i64()).unwrap_or(8).clamp(1, 16) as usize };
            let cs = if mask { Cs::Gray } else { load_cs(file, d.get("ColorSpace").unwrap_or(&Obj::Name(b"DeviceGray".to_vec())), Some(res), 0) };
            let n = cs.n();
            let row = (w * n * bpc).div_ceil(8);
            let maxv = ((1u32 << bpc) - 1) as f32;
            let indexed = matches!(cs, Cs::Indexed(_));
            let mut comps = vec![0f32; n];
            // Fast paths for 8-bit gray/RGB with the default decode.
            let simple = bpc == 8 && decode.is_empty() && matches!(cs, Cs::Gray | Cs::Rgb);
            for y in 0..h {
                let r = match data.get(y * row..(y + 1) * row) {
                    Some(r) => r,
                    None => break,
                };
                for x in 0..w {
                    if simple {
                        px[y * w + x] = if n == 1 {
                            let g = r[x] as u32;
                            0xFF00_0000 | g << 16 | g << 8 | g
                        } else {
                            0xFF00_0000 | (r[x * 3] as u32) << 16 | (r[x * 3 + 1] as u32) << 8 | r[x * 3 + 2] as u32
                        };
                        continue;
                    }
                    for (k, c) in comps.iter_mut().enumerate() {
                        let bit = (x * n + k) * bpc;
                        let v = match bpc {
                            8 => r[bit / 8] as u32,
                            16 => (r[bit / 8] as u32) << 8 | r[bit / 8 + 1] as u32,
                            _ => (r[bit / 8] as u32 >> (8 - bit % 8 - bpc)) & ((1 << bpc) - 1),
                        };
                        let (d0, d1) = if decode.len() >= 2 * (k + 1) { (decode[2 * k], decode[2 * k + 1]) } else if indexed { (0.0, maxv) } else { (0.0, 1.0) };
                        *c = d0 + v as f32 * (d1 - d0) / maxv;
                    }
                    px[y * w + x] = if mask {
                        // Stencil: 0 = paint (unless Decode flips it).
                        if comps[0] < 0.5 {
                            0xFF00_0000
                        } else {
                            0
                        }
                    } else {
                        0xFF00_0000 | cs.rgb(&comps)
                    };
                }
            }
        }
    }
    // Soft mask or colour-key / stencil mask.
    if let Some(sm) = d0.get("SMask") {
        if let Some(m) = decode_image(file, sm, res) {
            for y in 0..h {
                for x in 0..w {
                    let (mx, my) = (x * m.w / w, y * m.h / h);
                    let a = (m.px[my * m.w + mx] >> 8) & 0xFF;
                    px[y * w + x] = (px[y * w + x] & 0xFF_FFFF) | a << 24;
                }
            }
        }
    } else if let Some(mk) = d0.get("Mask") {
        match file.resolve(mk) {
            Obj::Array(_ranges) => {}
            s @ Obj::Stream(..) => {
                if let Some(m) = decode_image(file, &s, res) {
                    for y in 0..h {
                        for x in 0..w {
                            let (mx, my) = (x * m.w / w, y * m.h / h);
                            if m.px[my * m.w + mx] >> 24 == 0 {
                                px[y * w + x] &= 0xFF_FFFF;
                            }
                        }
                    }
                }
            }
            _ => {}
        }
    }
    Some(Img { w, h, px, mask_only: mask })
}

/// Draw an image whose unit square is mapped by `m` (image row 0 at the top).
pub fn draw_image(c: &mut Canvas, img: &Img, m: &M, clip: Option<&Clip>, alpha: u32, stencil_color: u32) {
    let corners = [apply(m, 0.0, 0.0), apply(m, 1.0, 0.0), apply(m, 1.0, 1.0), apply(m, 0.0, 1.0)];
    let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
    for (x, y) in corners {
        x0 = x0.min(x);
        y0 = y0.min(y);
        x1 = x1.max(x);
        y1 = y1.max(y);
    }
    let (cw, ch) = (c.w as i32, c.h as i32);
    let mut bx0 = (x0 as i32).max(0);
    let mut by0 = (y0 as i32).max(0);
    let mut bx1 = (libm::ceilf(x1) as i32).min(cw);
    let mut by1 = (libm::ceilf(y1) as i32).min(ch);
    if let Some(cl) = clip {
        let (a, b, cc, d) = cl.bounds();
        bx0 = bx0.max(a);
        by0 = by0.max(b);
        bx1 = bx1.min(cc);
        by1 = by1.min(d);
    }
    if bx1 <= bx0 || by1 <= by0 {
        return;
    }
    let Some(inv) = invert(m) else { return };
    // Shrink big pictures first (box filter) so they don't shimmer.
    let dev_w = (x1 - x0).max(y1 - y0).max(1.0);
    let factor = ((img.w.max(img.h) as f32 / dev_w) as usize).clamp(1, 16);
    let small;
    let src: &Img = if factor >= 2 && !img.mask_only {
        let (sw, sh) = (img.w.div_ceil(factor), img.h.div_ceil(factor));
        let mut px = vec![0u32; sw * sh];
        for y in 0..sh {
            for x in 0..sw {
                let (mut a, mut r, mut g, mut b, mut n) = (0u32, 0u32, 0u32, 0u32, 0u32);
                for yy in y * factor..((y + 1) * factor).min(img.h) {
                    for xx in x * factor..((x + 1) * factor).min(img.w) {
                        let p = img.px[yy * img.w + xx];
                        a += p >> 24;
                        r += (p >> 16) & 0xFF;
                        g += (p >> 8) & 0xFF;
                        b += p & 0xFF;
                        n += 1;
                    }
                }
                let n = n.max(1);
                px[y * sw + x] = (a / n) << 24 | (r / n) << 16 | (g / n) << 8 | b / n;
            }
        }
        small = Img { w: sw, h: sh, px, mask_only: false };
        &small
    } else {
        img
    };
    let (iw, ih) = (src.w as i64, src.h as i64);
    // Fixed-point stepping through image space along each row.
    const F: f32 = 65536.0;
    let du = (inv[0] * iw as f32 * F) as i64;
    let dv = (-inv[1] * ih as f32 * F) as i64;
    for y in by0..by1 {
        let (u0, v0) = apply(&inv, bx0 as f32 + 0.5, y as f32 + 0.5);
        let mut u = (u0 * iw as f32 * F) as i64;
        let mut v = ((1.0 - v0) * ih as f32 * F) as i64;
        let row = (y * cw) as usize;
        for x in bx0..bx1 {
            let (sx, sy) = (u >> 16, v >> 16);
            u += du;
            v += dv;
            if sx < 0 || sy < 0 || sx >= iw || sy >= ih {
                continue;
            }
            let p = src.px[(sy * iw + sx) as usize];
            let mut a = (p >> 24) * alpha / 255;
            if let Some(cl) = clip {
                a = a * cl.at(x, y) / 255;
            }
            if a == 0 {
                continue;
            }
            let col = if src.mask_only { stencil_color } else { p & 0xFF_FFFF };
            let i = row + x as usize;
            c.px[i] = if a >= 255 { col } else { mix(c.px[i], col, a) };
        }
    }
}

impl PdfDoc {
    /// Draw overlay content (from pdf::edit) onto a page image, with `m` the page's
    /// user-space-to-pixels matrix. Uses the same drawing code as pages, so what's on screen
    /// is what gets saved.
    pub fn draw_overlay(&self, c: &mut Canvas, m: &M, content: &str) {
        let (w, h) = (c.w, c.h);
        let mut res = Dict::default();
        let mut fonts = Dict::default();
        fonts.set("SolonF", Obj::Ref(u32::MAX, 0));
        res.set("Font", Obj::Dict(fonts));
        let mut hl = Dict::default();
        hl.set("ca", Obj::Real(0.4));
        hl.set("BM", Obj::Name(b"Multiply".to_vec()));
        let mut gss = Dict::default();
        gss.set("SolonHL", Obj::Dict(hl));
        res.set("ExtGState", Obj::Dict(gss));
        let mut r = Ren { doc: self, c, raster: Raster::new(w, h), gs: Vec::new(), budget: 500_000, hidden: 0, marked: Vec::new(), pbase: Vec::new() };
        let gs = GS {
            ctm: *m,
            fill: Paint::Color(0),
            stroke: Paint::Color(0),
            fcs: Cs::Gray,
            scs: Cs::Gray,
            fa: 1.0,
            sa: 1.0,
            lw: 1.0,
            cap: 0,
            dash: (Vec::new(), 0.0),
            clip: None,
            multiply: false,
            ts: TextState { font: None, font_id: 0, size: 12.0, tc: 0.0, tw: 0.0, th: 1.0, tl: 0.0, rise: 0.0, mode: 0 },
        };
        r.run(content.as_bytes(), &res, gs, 0);
    }

    /// The same, without a PDF to hand (pages made only of photos).
    pub fn draw_overlay_plain(c: &mut Canvas, m: &M, content: &str) {
        let tiny = b"%PDF-1.4\n1 0 obj <</Type /Catalog /Pages 2 0 R>> endobj\n2 0 obj <</Type /Pages /Kids [3 0 R] /Count 1>> endobj\n3 0 obj <</Type /Page /MediaBox [0 0 612 792]>> endobj\ntrailer <</Root 1 0 R>>\n";
        if let Ok(f) = PdfFile::open(tiny.to_vec()) {
            PdfDoc::new(f).draw_overlay(c, m, content);
        }
    }
}

fn mul_rgb(a: u32, b: u32) -> u32 {
    let ch = |s: u32| (((a >> s) & 0xFF) * ((b >> s) & 0xFF) / 255) << s;
    ch(16) | ch(8) | ch(0)
}

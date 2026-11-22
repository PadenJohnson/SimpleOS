//! Paint: a simple drawing program in the spirit of MS Paint, with a clean Mac-style look.
//! Pencil, brush, spray, eraser, fill, colour picker, text, lines, rectangles, rounded
//! rectangles and ellipses (outline, filled or both), rectangular selection (move, copy, cut,
//! paste, delete, crop), rotate, flip, resize, zoom, undo/redo. Opens and saves PNG and JPEG.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use spin::Mutex;

use crate::docshell::{DocEditor, EdAction, Kind, DIM, INK};
use crate::gfx::{rgb, Canvas, Rect, Size};
use crate::image::Image;
use crate::keys::{Key, Keys};
use crate::print::Paper;
use crate::raster::Raster;

const ACCENT: u32 = rgb(0, 122, 255);
const TOOL_H: i32 = 56;
const SIDE_W: i32 = 84;
const BOTTOM_H: i32 = 58;
const MAX_SIDE: usize = 6000;

fn icon(c: &mut Canvas, x: i32, y: i32) {
    c.round_rect(x, y, 26, 28, 4, rgb(255, 120, 60));
    c.circle(x + 9, y + 11, 3, rgb(255, 255, 255));
    c.circle(x + 17, y + 9, 3, rgb(255, 230, 80));
    c.circle(x + 18, y + 17, 3, rgb(120, 220, 255));
}

pub static KIND: Kind = Kind {
    app: "Paint",
    list_title: "Pictures",
    new_label: "New picture",
    exts: &crate::image::PHOTO_EXTS,
    save_ext: "png",
    keeps: &["png", "jpg", "jpeg"],
    accent: ACCENT,
    empty: ["No pictures yet.", "Start one with New picture, or plug in a USB drive with photos."],
    extra: &[],
    icon,
};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Select,
    Pencil,
    Brush,
    Spray,
    Eraser,
    Fill,
    Picker,
    Text,
    Line,
    Rect,
    RoundRect,
    Ellipse,
}

const TOOLS: [Tool; 12] = [Tool::Select, Tool::Picker, Tool::Pencil, Tool::Brush, Tool::Spray, Tool::Eraser, Tool::Fill, Tool::Text, Tool::Line, Tool::Rect, Tool::RoundRect, Tool::Ellipse];

#[derive(Clone, Copy, PartialEq, Eq)]
enum Style {
    Outline,
    Filled,
    Both,
}

const SIZES: [i32; 6] = [1, 3, 5, 9, 15, 25];
const TEXT_SIZES: [u16; 6] = [12, 18, 24, 36, 54, 80];
const PALETTE: [u32; 28] = [
    rgb(0, 0, 0), rgb(127, 127, 127), rgb(136, 0, 21), rgb(237, 28, 36), rgb(255, 127, 39), rgb(255, 242, 0), rgb(34, 177, 76), rgb(0, 162, 232), rgb(63, 72, 204), rgb(163, 73, 164), rgb(255, 174, 201), rgb(181, 230, 29), rgb(153, 217, 234), rgb(200, 191, 231),
    rgb(255, 255, 255), rgb(195, 195, 195), rgb(185, 122, 87), rgb(255, 201, 14), rgb(239, 228, 176), rgb(255, 255, 160), rgb(0, 128, 64), rgb(112, 146, 190), rgb(0, 64, 128), rgb(102, 51, 153), rgb(255, 102, 0), rgb(96, 64, 32), rgb(64, 128, 128), rgb(220, 220, 220),
];

/// A picture: RGB pixels.
#[derive(Clone)]
struct Pic {
    w: usize,
    h: usize,
    px: Vec<u32>,
}

impl Pic {
    fn new(w: usize, h: usize, c: u32) -> Pic {
        Pic { w, h, px: vec![c; w * h] }
    }
    fn get(&self, x: i32, y: i32) -> Option<u32> {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            Some(self.px[y as usize * self.w + x as usize])
        } else {
            None
        }
    }
    fn set(&mut self, x: i32, y: i32, c: u32) {
        if x >= 0 && y >= 0 && (x as usize) < self.w && (y as usize) < self.h {
            self.px[y as usize * self.w + x as usize] = c;
        }
    }
    fn blend(&mut self, x: i32, y: i32, c: u32, a: u32) {
        if let Some(p) = self.get(x, y) {
            self.px[y as usize * self.w + x as usize] = if a >= 255 { c } else { crate::gfx::mix(p, c, a) };
        }
    }
    fn region(&self, x: i32, y: i32, w: usize, h: usize, bg: u32) -> Pic {
        let mut out = Pic::new(w, h, bg);
        for yy in 0..h {
            for xx in 0..w {
                if let Some(p) = self.get(x + xx as i32, y + yy as i32) {
                    out.px[yy * w + xx] = p;
                }
            }
        }
        out
    }
    fn paste(&mut self, other: &Pic, x: i32, y: i32) {
        for yy in 0..other.h {
            for xx in 0..other.w {
                self.set(x + xx as i32, y + yy as i32, other.px[yy * other.w + xx]);
            }
        }
    }
    fn fill_rect(&mut self, x: i32, y: i32, w: i32, h: i32, c: u32) {
        for yy in y.max(0)..(y + h).min(self.h as i32) {
            for xx in x.max(0)..(x + w).min(self.w as i32) {
                self.px[yy as usize * self.w + xx as usize] = c;
            }
        }
    }
    fn disc(&mut self, cx: i32, cy: i32, d: i32, c: u32) {
        if d <= 1 {
            self.set(cx, cy, c);
            return;
        }
        let r2 = d * d;
        let off = d / 2;
        for yy in 0..d {
            for xx in 0..d {
                let (dx, dy) = (2 * xx + 1 - d, 2 * yy + 1 - d);
                if dx * dx + dy * dy <= r2 {
                    self.set(cx - off + xx, cy - off + yy, c);
                }
            }
        }
    }
    /// Stamp `f` along a line from a to b (every pixel or so).
    fn along(&mut self, a: (i32, i32), b: (i32, i32), mut f: impl FnMut(&mut Pic, i32, i32)) {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let n = dx.abs().max(dy.abs()).max(1);
        for k in 0..=n {
            f(self, a.0 + dx * k / n, a.1 + dy * k / n);
        }
    }
    /// Fill the area of similar colour around (x, y).
    fn flood(&mut self, x: i32, y: i32, c: u32) {
        let Some(target) = self.get(x, y) else { return };
        if target == c {
            return;
        }
        let close = |p: u32| {
            let d = |s: u32| (((p >> s) & 0xFF) as i32 - ((target >> s) & 0xFF) as i32).abs();
            d(16) + d(8) + d(0) <= 48
        };
        let (w, h) = (self.w as i32, self.h as i32);
        let mut done = vec![false; self.w * self.h];
        let mut stack = vec![(x, y)];
        while let Some((sx, sy)) = stack.pop() {
            let mut l = sx;
            while l > 0 && !done[(sy * w + l - 1) as usize] && close(self.px[(sy * w + l - 1) as usize]) {
                l -= 1;
            }
            let mut r = sx;
            while r + 1 < w && !done[(sy * w + r + 1) as usize] && close(self.px[(sy * w + r + 1) as usize]) {
                r += 1;
            }
            for xx in l..=r {
                let i = (sy * w + xx) as usize;
                if done[i] || !close(self.px[i]) {
                    continue;
                }
                done[i] = true;
                self.px[i] = c;
                for ny in [sy - 1, sy + 1] {
                    if ny >= 0 && ny < h {
                        let j = (ny * w + xx) as usize;
                        if !done[j] && close(self.px[j]) {
                            stack.push((xx, ny));
                        }
                    }
                }
            }
        }
    }
    /// Fill a path (given as a list of closed polygons) smoothly.
    fn fill_polys(&mut self, polys: &[Vec<(f32, f32)>], c: u32, evenodd: bool) {
        let pts = polys.iter().flatten();
        let (mut x0, mut y0, mut x1, mut y1) = (f32::MAX, f32::MAX, f32::MIN, f32::MIN);
        for &(x, y) in pts {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x);
            y1 = y1.max(y);
        }
        if x0 > x1 {
            return;
        }
        let (ox, oy) = ((x0 as i32 - 1).max(0), (y0 as i32 - 1).max(0));
        let (ex, ey) = ((x1 as i32 + 2).min(self.w as i32), (y1 as i32 + 2).min(self.h as i32));
        if ex <= ox || ey <= oy {
            return;
        }
        let mut r = Raster::new((ex - ox) as usize, (ey - oy) as usize);
        for p in polys {
            if let Some(&(x, y)) = p.first() {
                r.move_to(x - ox as f32, y - oy as f32);
                for &(x, y) in &p[1..] {
                    r.line_to(x - ox as f32, y - oy as f32);
                }
                r.close();
            }
        }
        r.rows(evenodd, |yy, xx, cov| {
            for (k, &a) in cov.iter().enumerate() {
                if a > 0 {
                    self.blend(ox + (xx + k) as i32, oy + yy as i32, c, a as u32);
                }
            }
        });
    }
}

fn ellipse_poly(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<(f32, f32)> {
    let (cx, cy, rx, ry) = ((x0 + x1) / 2.0, (y0 + y1) / 2.0, ((x1 - x0) / 2.0).abs(), ((y1 - y0) / 2.0).abs());
    let n = ((rx + ry) as usize / 2).clamp(16, 180);
    (0..n).map(|k| {
        let t = k as f32 * core::f32::consts::TAU / n as f32;
        (cx + rx * libm::cosf(t), cy + ry * libm::sinf(t))
    }).collect()
}

fn round_rect_poly(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Vec<(f32, f32)> {
    let (x0, x1) = (x0.min(x1), x0.max(x1));
    let (y0, y1) = (y0.min(y1), y0.max(y1));
    let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0).max(0.0);
    let mut v = Vec::new();
    let corners = [(x1 - r, y0 + r, -90.0f32), (x1 - r, y1 - r, 0.0), (x0 + r, y1 - r, 90.0), (x0 + r, y0 + r, 180.0)];
    for (cx, cy, start) in corners {
        for k in 0..=8 {
            let t = (start + k as f32 * 90.0 / 8.0).to_radians();
            v.push((cx + r * libm::cosf(t), cy + r * libm::sinf(t)));
        }
    }
    v
}

fn rect_poly(x0: f32, y0: f32, x1: f32, y1: f32) -> Vec<(f32, f32)> {
    let (x0, x1) = (x0.min(x1), x0.max(x1));
    let (y0, y1) = (y0.min(y1), y0.max(y1));
    vec![(x0, y0), (x1, y0), (x1, y1), (x0, y1)]
}

/// A shape grown or shrunk by `d` (for outlines: outer minus inner).
fn shape(tool: Tool, a: (i32, i32), b: (i32, i32), d: f32) -> Vec<(f32, f32)> {
    let (x0, y0) = (a.0.min(b.0) as f32 + 0.5 - d, a.1.min(b.1) as f32 + 0.5 - d);
    let (x1, y1) = (a.0.max(b.0) as f32 + 0.5 + d, a.1.max(b.1) as f32 + 0.5 + d);
    if x1 <= x0 || y1 <= y0 {
        return Vec::new();
    }
    match tool {
        Tool::Ellipse => ellipse_poly(x0, y0, x1, y1),
        Tool::RoundRect => round_rect_poly(x0, y0, x1, y1, ((x1 - x0).min(y1 - y0) / 5.0).max(4.0 + d)),
        _ => rect_poly(x0, y0, x1, y1),
    }
}

fn line_poly(a: (i32, i32), b: (i32, i32), w: f32) -> Vec<Vec<(f32, f32)>> {
    let (ax, ay, bx, by) = (a.0 as f32 + 0.5, a.1 as f32 + 0.5, b.0 as f32 + 0.5, b.1 as f32 + 0.5);
    let (dx, dy) = (bx - ax, by - ay);
    let len = libm::sqrtf(dx * dx + dy * dy).max(0.001);
    let hw = (w / 2.0).max(0.5);
    let (nx, ny) = (-dy / len * hw, dx / len * hw);
    let mut out = vec![vec![(ax + nx, ay + ny), (bx + nx, by + ny), (bx - nx, by - ny), (ax - nx, ay - ny)]];
    if w > 2.0 {
        out.push(ellipse_poly(ax - hw, ay - hw, ax + hw, ay + hw));
        out.push(ellipse_poly(bx - hw, by - hw, bx + hw, by + hw));
    }
    out
}

/// Copied pictures (shared with the next picture you open).
static CLIP: Mutex<Option<Pic>> = Mutex::new(None);

struct Sel {
    x: i32,
    y: i32,
    w: usize,
    h: usize,
    /// Pixels lifted off the picture, being moved.
    float: Option<Pic>,
}

struct TextBox {
    x: i32,
    y: i32,
    text: String,
}

enum Drag {
    Stroke,
    Shape { a: (i32, i32) },
    NewSel { a: (i32, i32), b: (i32, i32) },
    MoveSel { last: (i32, i32) },
    Pan { last: (i32, i32) },
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Back,
    Undo,
    Redo,
    Save,
    Size(usize),
    Style(u8),
    ZoomOut,
    ZoomIn,
    Fit,
    RotL,
    RotR,
    FlipH,
    FlipV,
    Resize,
    Crop,
}

pub struct PaintEd {
    pic: Pic,
    undo: Vec<Pic>,
    redo: Vec<Pic>,
    tool: Tool,
    color: u32,
    color2: u32,
    size: usize,
    style: Style,
    zoom: i32,
    fit: bool,
    scroll: (i32, i32),
    keys: Keys,
    dirty: bool,
    drag: Option<Drag>,
    /// Picture before the current shape (shapes are redrawn on it while dragging).
    base: Option<Pic>,
    last: (i32, i32),
    sel: Option<Sel>,
    text: Option<TextBox>,
    text_size: usize,
    cursor: Option<(i32, i32)>,
    resize_dialog: bool,
    seed: u32,
}

pub fn new_doc() -> PaintEd {
    PaintEd::with(Pic::new(1200, 800, 0xFF_FFFF))
}

pub fn load(_name: &str, b: &[u8]) -> Result<PaintEd, String> {
    let img = crate::image::decode(b).map_err(String::from)?;
    let img = if img.w.max(img.h) > MAX_SIDE { img.shrink_to_fit(MAX_SIDE, MAX_SIDE) } else { img };
    let px = img.px.iter().map(|&p| {
        let a = p >> 24;
        if a >= 255 {
            p & 0xFF_FFFF
        } else {
            crate::gfx::mix(0xFF_FFFF, p & 0xFF_FFFF, a)
        }
    }).collect();
    Ok(PaintEd::with(Pic { w: img.w, h: img.h, px }))
}

impl PaintEd {
    fn with(pic: Pic) -> PaintEd {
        PaintEd {
            pic,
            undo: Vec::new(),
            redo: Vec::new(),
            tool: Tool::Brush,
            color: 0,
            color2: 0xFF_FFFF,
            size: 2,
            style: Style::Outline,
            zoom: 100,
            fit: true,
            scroll: (0, 0),
            keys: Keys::default(),
            dirty: false,
            drag: None,
            base: None,
            last: (0, 0),
            sel: None,
            text: None,
            text_size: 2,
            cursor: None,
            resize_dialog: false,
            seed: 0x1234_5678,
        }
    }

    // ---- geometry -------------------------------------------------------------------------

    fn area(r: Rect) -> Rect {
        Rect::new(r.x + SIDE_W, r.y + TOOL_H, r.w - SIDE_W, r.h - TOOL_H - BOTTOM_H)
    }

    fn zoom_now(&self, r: Rect) -> i32 {
        if !self.fit {
            return self.zoom;
        }
        let a = Self::area(r);
        let z = ((a.w - 40) * 100 / self.pic.w.max(1) as i32).min((a.h - 40) * 100 / self.pic.h.max(1) as i32);
        z.clamp(5, 100)
    }

    /// Top-left of the picture on screen, and the zoom (percent).
    fn view(&self, r: Rect) -> (i32, i32, i32) {
        let a = Self::area(r);
        let z = self.zoom_now(r);
        let (sw, sh) = (self.pic.w as i32 * z / 100, self.pic.h as i32 * z / 100);
        let ox = if sw + 40 <= a.w { a.x + (a.w - sw) / 2 } else { a.x + 20 - self.scroll.0 };
        let oy = if sh + 40 <= a.h { a.y + (a.h - sh) / 2 } else { a.y + 20 - self.scroll.1 };
        (ox, oy, z)
    }

    fn to_img(&self, r: Rect, x: i32, y: i32) -> (i32, i32) {
        let (ox, oy, z) = self.view(r);
        (((x - ox) * 100).div_euclid(z.max(1)), ((y - oy) * 100).div_euclid(z.max(1)))
    }

    fn clamp_scroll(&mut self, r: Rect) {
        let a = Self::area(r);
        let z = self.zoom_now(r);
        let (sw, sh) = (self.pic.w as i32 * z / 100, self.pic.h as i32 * z / 100);
        self.scroll.0 = self.scroll.0.clamp(0, (sw + 40 - a.w).max(0));
        self.scroll.1 = self.scroll.1.clamp(0, (sh + 40 - a.h).max(0));
    }

    fn buttons(&self, r: Rect) -> Vec<(Btn, Rect)> {
        let mut v = Vec::new();
        let y = r.y + 10;
        let mut x = r.x + 12;
        let mut put = |v: &mut Vec<(Btn, Rect)>, b: Btn, w: i32, gap: i32| {
            v.push((b, Rect::new(x, y, w, 36)));
            x += w + gap;
        };
        put(&mut v, Btn::Back, 104, 14);
        put(&mut v, Btn::Undo, 40, 4);
        put(&mut v, Btn::Redo, 40, 10);
        put(&mut v, Btn::Save, 64, 22);
        for k in 0..SIZES.len() {
            put(&mut v, Btn::Size(k), 32, if k + 1 == SIZES.len() { 18 } else { 2 });
        }
        for k in 0..3 {
            put(&mut v, Btn::Style(k), 36, if k == 2 { 18 } else { 2 });
        }
        put(&mut v, Btn::ZoomOut, 34, 64);
        put(&mut v, Btn::ZoomIn, 34, 4);
        put(&mut v, Btn::Fit, 46, 18);
        put(&mut v, Btn::RotL, 36, 2);
        put(&mut v, Btn::RotR, 36, 2);
        put(&mut v, Btn::FlipH, 36, 2);
        put(&mut v, Btn::FlipV, 36, 10);
        put(&mut v, Btn::Resize, 72, 4);
        put(&mut v, Btn::Crop, 56, 0);
        v
    }

    fn tool_rect(r: Rect, i: usize) -> Rect {
        Rect::new(r.x + 10 + (i % 2) as i32 * 34, r.y + TOOL_H + 12 + (i / 2) as i32 * 36, 30, 32)
    }

    fn palette_rect(r: Rect, i: usize) -> Rect {
        let y = r.y + r.h - BOTTOM_H + 7;
        Rect::new(r.x + 96 + (i % 14) as i32 * 24, y + (i / 14) as i32 * 23, 20, 20)
    }

    fn wells(r: Rect) -> (Rect, Rect) {
        let y = r.y + r.h - BOTTOM_H + 8;
        (Rect::new(r.x + 16, y, 34, 34), Rect::new(r.x + 40, y + 12, 34, 34))
    }

    fn resize_rects(r: Rect) -> Vec<(i32, Rect)> {
        let (dw, dh) = (520, 210);
        let (dx, dy) = (r.x + (r.w - dw) / 2, r.y + (r.h - dh) / 2);
        let mut v: Vec<(i32, Rect)> = [25, 50, 75, 125, 150, 200].iter().enumerate().map(|(k, &p)| (p, Rect::new(dx + 24 + k as i32 * 80, dy + 80, 72, 40))).collect();
        v.push((0, Rect::new(dx + dw - 24 - 100, dy + dh - 58, 100, 40)));
        v
    }

    // ---- editing --------------------------------------------------------------------------

    fn snapshot(&mut self) {
        self.undo.push(self.pic.clone());
        // Keep up to ~240 MB of undo.
        let per = self.pic.w * self.pic.h * 4;
        let keep = (240 * 1024 * 1024 / per.max(1)).clamp(3, 40);
        while self.undo.len() > keep {
            self.undo.remove(0);
        }
        self.redo.clear();
        self.dirty = true;
    }

    fn commit_float(&mut self) {
        if let Some(s) = &mut self.sel {
            if let Some(f) = s.float.take() {
                self.pic.paste(&f, s.x, s.y);
                self.dirty = true;
            }
        }
    }

    fn commit_text(&mut self) {
        let Some(t) = self.text.take() else { return };
        if t.text.trim().is_empty() {
            return;
        }
        self.snapshot();
        let px = TEXT_SIZES[self.text_size];
        let color = self.color;
        let mut fonts = crate::font::FONTS.lock();
        let face = crate::font::Face::Sans;
        let (upem, asc_u, _, _) = fonts.vmetrics(face);
        let asc = asc_u * px as i32 / upem.max(1);
        let lh = px as i32 * 13 / 10;
        for (k, line) in t.text.split('\n').enumerate() {
            let base = t.y + asc + k as i32 * lh;
            let mut pen = t.x * 64;
            for ch in line.chars() {
                let g = fonts.glyph(face, ch, px);
                let gx = (pen + 32).div_euclid(64) + g.xmin;
                let gy = base - g.ymin - g.h as i32;
                for yy in 0..g.h {
                    for xx in 0..g.w {
                        let a = g.cov[yy * g.w + xx];
                        if a > 0 {
                            self.pic.blend(gx + xx as i32, gy + yy as i32, color, a as u32);
                        }
                    }
                }
                pen += g.adv;
            }
        }
    }

    /// Before any change that isn't to the selection: put down what's floating and the text.
    fn settle(&mut self) {
        self.commit_text();
        self.commit_float();
    }

    fn brush_d(&self) -> i32 {
        SIZES[self.size]
    }

    fn dab(&mut self, x: i32, y: i32) {
        let d = self.brush_d();
        match self.tool {
            Tool::Pencil => self.pic.set(x, y, self.color),
            Tool::Brush => self.pic.disc(x, y, d.max(2), self.color),
            Tool::Eraser => {
                let s = (d * 2).max(6);
                self.pic.fill_rect(x - s / 2, y - s / 2, s, s, self.color2);
            }
            Tool::Spray => {
                let rad = (d * 3).max(8);
                for _ in 0..(rad / 2).max(6) {
                    self.seed ^= self.seed << 13;
                    self.seed ^= self.seed >> 17;
                    self.seed ^= self.seed << 5;
                    let dx = (self.seed % (2 * rad as u32 + 1)) as i32 - rad;
                    let dy = ((self.seed >> 12) % (2 * rad as u32 + 1)) as i32 - rad;
                    if dx * dx + dy * dy <= rad * rad {
                        self.pic.set(x + dx, y + dy, self.color);
                    }
                }
            }
            _ => {}
        }
    }

    fn stroke_to(&mut self, p: (i32, i32)) {
        let a = self.last;
        if self.tool == Tool::Spray {
            self.dab(p.0, p.1);
        } else {
            let tool = self.tool;
            let (c, c2, d) = (self.color, self.color2, self.brush_d());
            self.pic.along(a, p, |pic, x, y| match tool {
                Tool::Pencil => pic.set(x, y, c),
                Tool::Brush => pic.disc(x, y, d.max(2), c),
                Tool::Eraser => {
                    let s = (d * 2).max(6);
                    pic.fill_rect(x - s / 2, y - s / 2, s, s, c2);
                }
                _ => {}
            });
        }
        self.last = p;
    }

    /// Draw the shape being dragged onto a fresh copy of the picture underneath.
    fn draw_shape(&mut self, a: (i32, i32), b: (i32, i32)) {
        let Some(base) = &self.base else { return };
        self.pic.px.copy_from_slice(&base.px);
        let w = self.brush_d() as f32;
        if self.tool == Tool::Line {
            let polys = line_poly(a, b, w);
            self.pic.fill_polys(&polys, self.color, false);
            return;
        }
        if matches!(self.style, Style::Filled | Style::Both) {
            let inner = shape(self.tool, a, b, if self.style == Style::Both { -w / 2.0 } else { 0.0 });
            let fill = if self.style == Style::Both { self.color2 } else { self.color };
            self.pic.fill_polys(&[inner], fill, false);
        }
        if matches!(self.style, Style::Outline | Style::Both) {
            let outer = shape(self.tool, a, b, w / 2.0);
            let inner = shape(self.tool, a, b, -w / 2.0);
            self.pic.fill_polys(&[outer, inner], self.color, true);
        }
    }

    fn rotate(&mut self, right: bool) {
        self.settle();
        self.snapshot();
        let (w, h) = (self.pic.w, self.pic.h);
        let mut out = Pic::new(h, w, 0);
        for y in 0..h {
            for x in 0..w {
                let (nx, ny) = if right { (h - 1 - y, x) } else { (y, w - 1 - x) };
                out.px[ny * h + nx] = self.pic.px[y * w + x];
            }
        }
        self.pic = out;
        self.sel = None;
    }

    fn flip(&mut self, horiz: bool) {
        self.settle();
        self.snapshot();
        let (w, h) = (self.pic.w, self.pic.h);
        if horiz {
            for y in 0..h {
                self.pic.px[y * w..(y + 1) * w].reverse();
            }
        } else {
            for y in 0..h / 2 {
                for x in 0..w {
                    self.pic.px.swap(y * w + x, (h - 1 - y) * w + x);
                }
            }
        }
    }

    fn resize(&mut self, pct: i32) {
        self.settle();
        let (w, h) = ((self.pic.w as i32 * pct / 100).max(1) as usize, (self.pic.h as i32 * pct / 100).max(1) as usize);
        if w.max(h) > MAX_SIDE {
            return;
        }
        self.snapshot();
        let img = Image { w: self.pic.w, h: self.pic.h, px: self.pic.px.iter().map(|p| p | 0xFF00_0000).collect(), opaque: true };
        let out = if pct < 100 { img.shrink_to_fit(w, h) } else { img };
        let mut px = vec![0u32; w * h];
        for y in 0..h {
            for x in 0..w {
                px[y * w + x] = out.px[(y * out.h / h).min(out.h - 1) * out.w + (x * out.w / w).min(out.w - 1)] & 0xFF_FFFF;
            }
        }
        self.pic = Pic { w, h, px };
        self.sel = None;
    }

    fn crop(&mut self) {
        self.commit_text();
        let Some(s) = &self.sel else { return };
        let (x, y, w, h) = (s.x, s.y, s.w, s.h);
        self.commit_float();
        if w < 1 || h < 1 {
            return;
        }
        self.snapshot();
        self.pic = self.pic.region(x, y, w, h, self.color2);
        self.sel = None;
    }

    fn copy(&self) {
        if let Some(s) = &self.sel {
            let p = match &s.float {
                Some(f) => f.clone(),
                None => self.pic.region(s.x, s.y, s.w, s.h, 0xFF_FFFF),
            };
            *CLIP.lock() = Some(p);
        }
    }

    fn delete_sel(&mut self) {
        if let Some(s) = &mut self.sel {
            if s.float.take().is_some() {
                self.dirty = true;
                self.sel = None;
                return;
            }
        }
        if let Some(s) = &self.sel {
            let (x, y, w, h) = (s.x, s.y, s.w as i32, s.h as i32);
            self.snapshot();
            self.pic.fill_rect(x, y, w, h, self.color2);
        }
    }

    fn paste(&mut self, r: Rect) {
        let Some(p) = CLIP.lock().clone() else { return };
        self.settle();
        self.snapshot();
        let (ix, iy) = self.to_img(r, Self::area(r).x + 20, Self::area(r).y + 20);
        let (x, y) = (ix.max(0), iy.max(0));
        self.sel = Some(Sel { x, y, w: p.w, h: p.h, float: Some(p) });
        self.tool = Tool::Select;
    }

    fn press_button(&mut self, b: Btn, r: Rect) -> EdAction {
        match b {
            Btn::Back => {
                self.settle();
                return EdAction::Close;
            }
            Btn::Save => {
                self.settle();
                return EdAction::Save;
            }
            Btn::Undo => self.do_undo(),
            Btn::Redo => self.do_redo(),
            Btn::Size(k) => {
                if self.tool == Tool::Text {
                    self.text_size = k;
                } else {
                    self.size = k;
                }
            }
            Btn::Style(k) => self.style = [Style::Outline, Style::Filled, Style::Both][k as usize],
            Btn::ZoomOut | Btn::ZoomIn => {
                let z = self.zoom_now(r);
                let steps = [5, 10, 25, 50, 75, 100, 150, 200, 300, 400, 600, 800];
                let i = steps.iter().position(|&s| s >= z).unwrap_or(5);
                let i = if b == Btn::ZoomIn { (i + if steps[i] == z { 1 } else { 0 }).min(steps.len() - 1) } else { i.saturating_sub(1) };
                self.zoom = steps[i];
                self.fit = false;
                self.clamp_scroll(r);
            }
            Btn::Fit => {
                self.fit = true;
                self.scroll = (0, 0);
            }
            Btn::RotL => self.rotate(false),
            Btn::RotR => self.rotate(true),
            Btn::FlipH => self.flip(true),
            Btn::FlipV => self.flip(false),
            Btn::Resize => {
                self.settle();
                self.resize_dialog = true;
            }
            Btn::Crop => {
                if self.sel.is_some() {
                    self.crop();
                } else {
                    return EdAction::Toast(String::from("Choose an area with the Select tool first"));
                }
            }
        }
        EdAction::Redraw
    }

    fn do_undo(&mut self) {
        self.text = None;
        self.sel = None;
        if let Some(p) = self.undo.pop() {
            self.redo.push(core::mem::replace(&mut self.pic, p));
            self.dirty = true;
        }
    }

    fn do_redo(&mut self) {
        self.sel = None;
        if let Some(p) = self.redo.pop() {
            self.undo.push(core::mem::replace(&mut self.pic, p));
            self.dirty = true;
        }
    }

    // ---- drawing --------------------------------------------------------------------------

    fn draw_tool_icon(c: &mut Canvas, t: Tool, b: Rect, ink: u32) {
        let (x, y) = (b.x + b.w / 2, b.y + b.h / 2);
        match t {
            Tool::Select => {
                for k in 0..5 {
                    c.fill_rect(x - 9 + k * 4, y - 8, 2, 1, ink);
                    c.fill_rect(x - 9 + k * 4, y + 7, 2, 1, ink);
                    c.fill_rect(x - 9, y - 8 + k * 4, 1, 2, ink);
                    c.fill_rect(x + 8, y - 8 + k * 4, 1, 2, ink);
                }
            }
            Tool::Picker => {
                for k in 0..10 {
                    c.fill_rect(x - 6 + k, y + 6 - k, 2, 2, ink);
                }
                c.circle(x + 5, y - 5, 4, ink);
            }
            Tool::Pencil => {
                for k in 0..12 {
                    c.fill_rect(x - 7 + k, y + 6 - k, 3, 3, if k < 2 { rgb(60, 60, 60) } else { rgb(240, 180, 40) });
                }
            }
            Tool::Brush => {
                for k in 0..8 {
                    c.fill_rect(x + k - 1, y - k - 1, 2, 2, rgb(150, 100, 60));
                }
                c.circle(x - 4, y + 4, 4, ink);
            }
            Tool::Spray => {
                c.round_rect(x - 3, y - 2, 7, 11, 2, ink);
                for (dx, dy) in [(-8, -6), (-5, -9), (-9, -2), (-2, -8), (-6, -4)] {
                    c.fill_rect(x + dx, y + dy, 2, 2, ink);
                }
            }
            Tool::Eraser => {
                c.round_rect(x - 9, y - 3, 18, 10, 2, rgb(240, 140, 160));
                c.fill_rect(x - 9, y - 3, 6, 10, rgb(220, 220, 230));
            }
            Tool::Fill => {
                c.triangle([(x - 8, y - 2), (x, y - 9), (x + 6, y + 2)], ink);
                c.triangle([(x - 8, y - 2), (x + 6, y + 2), (x - 2, y + 8)], ink);
                c.circle(x + 7, y + 6, 2, ACCENT);
            }
            Tool::Text => {
                c.text_face((x - 6) * 64, y + 7, "A", crate::font::Face::SerifBold, 20, ink);
            }
            Tool::Line => {
                for k in 0..16 {
                    c.fill_rect(x - 8 + k, y + 7 - k, 2, 2, ink);
                }
            }
            Tool::Rect => {
                c.outline_rect(x - 9, y - 7, 18, 14, ink);
                c.outline_rect(x - 8, y - 6, 16, 12, ink);
            }
            Tool::RoundRect => {
                c.round_rect(x - 9, y - 7, 18, 14, 5, ink);
                c.round_rect(x - 7, y - 5, 14, 10, 3, rgb(255, 255, 255));
            }
            Tool::Ellipse => {
                c.circle(x, y, 8, ink);
                c.circle(x, y, 6, rgb(255, 255, 255));
            }
        }
    }

    fn draw_button(&self, c: &mut Canvas, b: Btn, r: Rect) {
        let on = match b {
            Btn::Size(k) => k == if self.tool == Tool::Text { self.text_size } else { self.size },
            Btn::Style(k) => self.style == [Style::Outline, Style::Filled, Style::Both][k as usize],
            Btn::Fit => self.fit,
            _ => false,
        };
        let enabled = match b {
            Btn::Undo => !self.undo.is_empty(),
            Btn::Redo => !self.redo.is_empty(),
            Btn::Crop => self.sel.is_some(),
            _ => true,
        };
        c.round_rect(r.x, r.y, r.w, r.h, 8, if on { rgb(210, 228, 255) } else { rgb(255, 255, 255) });
        c.outline_rect(r.x, r.y + r.h, r.w, 1, rgb(218, 222, 230));
        let ink = if enabled { INK } else { rgb(180, 184, 192) };
        let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2);
        let label = |c: &mut Canvas, t: &str| c.text_centered(cx, r.y + 9, t, Size::Small, true, ink);
        match b {
            Btn::Back => label(c, "< Pictures"),
            Btn::Undo => {
                c.triangle([(cx - 8, cy), (cx - 2, cy - 6), (cx - 2, cy + 6)], ink);
                c.fill_rect(cx - 2, cy - 2, 9, 4, ink);
            }
            Btn::Redo => {
                c.triangle([(cx + 8, cy), (cx + 2, cy - 6), (cx + 2, cy + 6)], ink);
                c.fill_rect(cx - 7, cy - 2, 9, 4, ink);
            }
            Btn::Save => {
                if self.dirty {
                    c.round_rect(r.x, r.y, r.w, r.h, 8, ACCENT);
                    c.text_centered(cx, r.y + 9, "Save", Size::Small, true, rgb(255, 255, 255));
                } else {
                    label(c, "Save");
                }
            }
            Btn::Size(k) => {
                if self.tool == Tool::Text {
                    let s = format!("{}", TEXT_SIZES[k]);
                    label(c, &s);
                } else {
                    let d = SIZES[k].min(22);
                    c.circle(cx, cy, (d / 2).max(1), ink);
                }
            }
            Btn::Style(k) => match k {
                0 => {
                    c.outline_rect(cx - 9, cy - 7, 18, 14, ink);
                    c.outline_rect(cx - 8, cy - 6, 16, 12, ink);
                }
                1 => c.fill_rect(cx - 9, cy - 7, 18, 14, ink),
                _ => {
                    c.fill_rect(cx - 9, cy - 7, 18, 14, ink);
                    c.fill_rect(cx - 6, cy - 4, 12, 8, rgb(200, 210, 230));
                }
            },
            Btn::ZoomOut => label(c, "-"),
            Btn::ZoomIn => label(c, "+"),
            Btn::Fit => label(c, "Fit"),
            Btn::RotL | Btn::RotR => {
                // A curved arrow (an arc of dots with a head).
                let dir = if b == Btn::RotR { 1 } else { -1 };
                for k in 0..14 {
                    let t = (200.0 + k as f32 * 18.0).to_radians();
                    let (px, py) = (cx as f32 + 7.0 * libm::cosf(t) * dir as f32, cy as f32 + 7.0 * libm::sinf(t));
                    c.fill_rect(px as i32 - 1, py as i32 - 1, 2, 2, ink);
                }
                let hx = cx + 7 * dir;
                c.triangle([(hx - 4, cy - 2), (hx + 4, cy - 2), (hx, cy + 3)], ink);
            }
            Btn::FlipH => {
                c.triangle([(cx - 9, cy), (cx - 2, cy - 6), (cx - 2, cy + 6)], ink);
                c.triangle([(cx + 9, cy), (cx + 2, cy - 6), (cx + 2, cy + 6)], ink);
            }
            Btn::FlipV => {
                c.triangle([(cx, cy - 9), (cx - 6, cy - 2), (cx + 6, cy - 2)], ink);
                c.triangle([(cx, cy + 9), (cx - 6, cy + 2), (cx + 6, cy + 2)], ink);
            }
            Btn::Resize => label(c, "Resize"),
            Btn::Crop => label(c, "Crop"),
        }
    }
}

fn ants(c: &mut Canvas, x0: i32, y0: i32, x1: i32, y1: i32) {
    let dash = |k: i32| if (k / 4) % 2 == 0 { 0 } else { 0xFF_FFFF };
    for x in x0..=x1 {
        c.blend(x, y0, dash(x), 255);
        c.blend(x, y1, dash(x + 2), 255);
    }
    for y in y0..=y1 {
        c.blend(x0, y, dash(y), 255);
        c.blend(x1, y, dash(y + 2), 255);
    }
}

impl DocEditor for PaintEd {
    fn draw(&self, c: &mut Canvas, r: Rect) {
        let a = Self::area(r);
        c.fill_rect(a.x, a.y, a.w, a.h, rgb(222, 224, 229));
        let (ox, oy, z) = self.view(r);
        let (sw, sh) = (self.pic.w as i32 * z / 100, self.pic.h as i32 * z / 100);
        c.set_clip(a);
        c.shadow(ox, oy + 2, sw, sh, 1, 10, 60);
        // The picture, scaled (nearest pixel; columns worked out once).
        let x0 = ox.max(a.x);
        let x1 = (ox + sw).min(a.x + a.w);
        let cols: Vec<usize> = (x0..x1).map(|sx| (((sx - ox) * 100 / z.max(1)) as usize).min(self.pic.w - 1)).collect();
        for sy in oy.max(a.y)..(oy + sh).min(a.y + a.h) {
            let iy = (((sy - oy) * 100 / z.max(1)) as usize).min(self.pic.h - 1);
            let src = &self.pic.px[iy * self.pic.w..(iy + 1) * self.pic.w];
            let row = sy as usize * c.w;
            for (k, &ix) in cols.iter().enumerate() {
                c.px[row + (x0 as usize + k)] = src[ix];
            }
        }
        // Floating selection pixels, and the selection outline.
        let to_s = |ix: i32, iy: i32| (ox + ix * z / 100, oy + iy * z / 100);
        if let Some(s) = &self.sel {
            if let Some(f) = &s.float {
                let (fx, fy) = to_s(s.x, s.y);
                let (fw, fh) = (f.w as i32 * z / 100, f.h as i32 * z / 100);
                for yy in fy.max(a.y)..(fy + fh).min(a.y + a.h) {
                    let iy = (((yy - fy) * 100 / z.max(1)) as usize).min(f.h - 1);
                    for xx in fx.max(a.x)..(fx + fw).min(a.x + a.w) {
                        let ix = (((xx - fx) * 100 / z.max(1)) as usize).min(f.w - 1);
                        c.blend(xx, yy, f.px[iy * f.w + ix], 255);
                    }
                }
            }
            let (sx0, sy0) = to_s(s.x, s.y);
            let (sx1, sy1) = to_s(s.x + s.w as i32, s.y + s.h as i32);
            ants(c, sx0, sy0, sx1 - 1, sy1 - 1);
        }
        if let Some(Drag::NewSel { a: p, b: q }) = &self.drag {
            let (sx0, sy0) = to_s(p.0.min(q.0), p.1.min(q.1));
            let (sx1, sy1) = to_s(p.0.max(q.0) + 1, p.1.max(q.1) + 1);
            ants(c, sx0, sy0, sx1 - 1, sy1 - 1);
        }
        // Text being typed.
        if let Some(t) = &self.text {
            let px = TEXT_SIZES[self.text_size] as i32 * z / 100;
            let (tx, ty) = to_s(t.x, t.y);
            let lh = px * 13 / 10;
            let mut maxw = 40;
            let n = t.text.split('\n').count() as i32;
            for (k, line) in t.text.split('\n').enumerate() {
                let base = ty + (px as f32 * 1.07) as i32 + k as i32 * lh;
                let w = c.text_face(tx * 64, base, line, crate::font::Face::Sans, px.max(4) as u16, self.color) / 64;
                maxw = maxw.max(w + 6);
                if k as i32 == n - 1 {
                    c.fill_rect(tx + w + 1, base - px, 2, px + px / 4, ACCENT);
                }
            }
            ants(c, tx - 4, ty - 2, tx + maxw, ty + n * lh + 4);
        }
        c.reset_clip();
        c.set_clip(r);
        // Toolbar.
        c.fill_rect(r.x, r.y, r.w, TOOL_H, rgb(246, 246, 248));
        c.fill_rect(r.x, r.y + TOOL_H - 1, r.w, 1, rgb(214, 218, 226));
        for (b, br) in self.buttons(r) {
            self.draw_button(c, b, br);
            if b == Btn::ZoomOut {
                let zs = format!("{}%", self.zoom_now(r));
                c.text_centered(br.x + br.w + 32, br.y + 9, &zs, Size::Small, true, INK);
            }
        }
        // Tool palette.
        c.fill_rect(r.x, a.y, SIDE_W, a.h, rgb(246, 246, 248));
        c.fill_rect(r.x + SIDE_W - 1, a.y, 1, a.h, rgb(214, 218, 226));
        for (i, &t) in TOOLS.iter().enumerate() {
            let b = Self::tool_rect(r, i);
            let on = t == self.tool;
            c.round_rect(b.x, b.y, b.w, b.h, 7, if on { ACCENT } else { rgb(255, 255, 255) });
            Self::draw_tool_icon(c, t, b, if on { rgb(255, 255, 255) } else { INK });
        }
        let name = match self.tool {
            Tool::Select => "Select",
            Tool::Picker => "Pick colour",
            Tool::Pencil => "Pencil",
            Tool::Brush => "Brush",
            Tool::Spray => "Spray",
            Tool::Eraser => "Eraser",
            Tool::Fill => "Fill",
            Tool::Text => "Text",
            Tool::Line => "Line",
            Tool::Rect => "Rectangle",
            Tool::RoundRect => "Rounded",
            Tool::Ellipse => "Ellipse",
        };
        let ty = Self::tool_rect(r, TOOLS.len() - 1).y + 44;
        c.text_centered(r.x + SIDE_W / 2, ty, name, Size::Small, false, DIM);
        // Colours.
        let by = r.y + r.h - BOTTOM_H;
        c.fill_rect(r.x, by, r.w, BOTTOM_H, rgb(246, 246, 248));
        c.fill_rect(r.x, by, r.w, 1, rgb(214, 218, 226));
        let (w1, w2) = Self::wells(r);
        c.round_rect(w2.x - 1, w2.y - 1, w2.w + 2, w2.h + 2, 7, rgb(150, 156, 166));
        c.round_rect(w2.x, w2.y, w2.w, w2.h, 6, self.color2);
        c.round_rect(w1.x - 1, w1.y - 1, w1.w + 2, w1.h + 2, 7, rgb(150, 156, 166));
        c.round_rect(w1.x, w1.y, w1.w, w1.h, 6, self.color);
        for (i, &col) in PALETTE.iter().enumerate() {
            let b = Self::palette_rect(r, i);
            c.round_rect(b.x - 1, b.y - 1, b.w + 2, b.h + 2, 5, rgb(190, 194, 202));
            c.round_rect(b.x, b.y, b.w, b.h, 4, col);
        }
        let mut status = format!("{} x {} pixels", self.pic.w, self.pic.h);
        if let Some((x, y)) = self.cursor {
            status = format!("{}, {}     {}", x, y, status);
        }
        if let Some(s) = &self.sel {
            status = format!("Selection {} x {}     {}", s.w, s.h, status);
        }
        let sw2 = crate::gfx::text_width(&status, Size::Small, false);
        c.text(r.x + r.w - 16 - sw2, by + 20, &status, Size::Small, false, DIM);
        let hint = "Click a colour to draw with it; Shift+click sets the second (fill) colour.";
        let hx = Self::palette_rect(r, 13).x + 34;
        if hx + crate::gfx::text_width(hint, Size::Small, false) + 24 < r.x + r.w - 16 - sw2 {
            c.text(hx, by + 20, hint, Size::Small, false, rgb(150, 156, 166));
        }
        // Resize dialog.
        if self.resize_dialog {
            c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 100);
            let (dw, dh) = (520, 210);
            let (dx, dy) = (r.x + (r.w - dw) / 2, r.y + (r.h - dh) / 2);
            c.shadow(dx, dy + 6, dw, dh, 14, 22, 100);
            c.round_rect(dx, dy, dw, dh, 14, rgb(255, 255, 255));
            c.text(dx + 24, dy + 20, "Resize the picture", Size::Medium, true, INK);
            c.text(dx + 24, dy + 50, &format!("Now {} x {} pixels. Pick a new size:", self.pic.w, self.pic.h), Size::Small, false, DIM);
            for (p, b) in Self::resize_rects(r) {
                if p == 0 {
                    crate::docshell::button(c, b, "Cancel", false, ACCENT);
                } else {
                    crate::docshell::button(c, b, &format!("{}%", p), false, ACCENT);
                }
            }
        }
    }

    fn on_press(&mut self, x: i32, y: i32, r: Rect, _now: u64) -> EdAction {
        if self.resize_dialog {
            for (p, b) in Self::resize_rects(r) {
                if b.contains(x, y) {
                    self.resize_dialog = false;
                    if p > 0 {
                        self.resize(p);
                    }
                }
            }
            return EdAction::Redraw;
        }
        if let Some((b, _)) = self.buttons(r).into_iter().find(|(_, br)| br.contains(x, y)) {
            return self.press_button(b, r);
        }
        for (i, &t) in TOOLS.iter().enumerate() {
            if Self::tool_rect(r, i).contains(x, y) {
                self.commit_text();
                if t != Tool::Select {
                    self.commit_float();
                    self.sel = None;
                }
                self.tool = t;
                return EdAction::Redraw;
            }
        }
        let (w1, w2) = Self::wells(r);
        if w1.contains(x, y) || w2.contains(x, y) {
            core::mem::swap(&mut self.color, &mut self.color2);
            return EdAction::Redraw;
        }
        for (i, &col) in PALETTE.iter().enumerate() {
            if Self::palette_rect(r, i).contains(x, y) {
                if self.keys.shift {
                    self.color2 = col;
                } else {
                    self.color = col;
                }
                return EdAction::Redraw;
            }
        }
        let a = Self::area(r);
        if !a.contains(x, y) {
            return EdAction::None;
        }
        let p = self.to_img(r, x, y);
        let inside = p.0 >= 0 && p.1 >= 0 && (p.0 as usize) < self.pic.w && (p.1 as usize) < self.pic.h;
        match self.tool {
            Tool::Select => {
                self.commit_text();
                let in_sel = self.sel.as_ref().is_some_and(|s| p.0 >= s.x && p.1 >= s.y && p.0 < s.x + s.w as i32 && p.1 < s.y + s.h as i32);
                if in_sel {
                    // Lift the pixels (leaving the second colour behind) and move them.
                    let lift = self.sel.as_ref().is_some_and(|s| s.float.is_none());
                    if lift {
                        self.snapshot();
                        let (sx, sy, sw, sh) = {
                            let s = self.sel.as_ref().unwrap();
                            (s.x, s.y, s.w, s.h)
                        };
                        let f = self.pic.region(sx, sy, sw, sh, self.color2);
                        self.pic.fill_rect(sx, sy, sw as i32, sh as i32, self.color2);
                        self.sel.as_mut().unwrap().float = Some(f);
                    }
                    self.drag = Some(Drag::MoveSel { last: p });
                } else {
                    self.commit_float();
                    self.sel = None;
                    self.drag = Some(Drag::NewSel { a: p, b: p });
                }
            }
            Tool::Picker => {
                if let Some(c) = self.pic.get(p.0, p.1) {
                    if self.keys.shift {
                        self.color2 = c;
                    } else {
                        self.color = c;
                    }
                    self.tool = Tool::Brush;
                }
            }
            Tool::Fill if inside => {
                self.settle();
                self.snapshot();
                let c = self.color;
                self.pic.flood(p.0, p.1, c);
            }
            Tool::Text => {
                if self.text.is_some() {
                    self.commit_text();
                } else if inside {
                    self.text = Some(TextBox { x: p.0, y: p.1, text: String::new() });
                }
            }
            Tool::Pencil | Tool::Brush | Tool::Eraser | Tool::Spray => {
                self.settle();
                self.snapshot();
                self.last = p;
                self.dab(p.0, p.1);
                self.drag = Some(Drag::Stroke);
            }
            Tool::Line | Tool::Rect | Tool::RoundRect | Tool::Ellipse => {
                self.settle();
                self.snapshot();
                self.base = Some(self.pic.clone());
                self.drag = Some(Drag::Shape { a: p });
            }
            _ => {
                // Outside the picture: drag to look around.
                self.drag = Some(Drag::Pan { last: (x, y) });
            }
        }
        EdAction::Redraw
    }

    fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        let p = self.to_img(r, x, y);
        self.cursor = Some(p);
        match self.drag.take() {
            Some(Drag::Stroke) => {
                self.stroke_to(p);
                self.drag = Some(Drag::Stroke);
            }
            Some(Drag::Shape { a, .. }) => {
                // Shift keeps it square / straight.
                let mut b = p;
                if self.keys.shift {
                    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
                    if self.tool == Tool::Line {
                        if dx.abs() > 2 * dy.abs() {
                            b.1 = a.1;
                        } else if dy.abs() > 2 * dx.abs() {
                            b.0 = a.0;
                        } else {
                            let m = dx.abs().min(dy.abs());
                            b = (a.0 + m * dx.signum(), a.1 + m * dy.signum());
                        }
                    } else {
                        let m = dx.abs().max(dy.abs());
                        b = (a.0 + m * if dx < 0 { -1 } else { 1 }, a.1 + m * if dy < 0 { -1 } else { 1 });
                    }
                }
                self.draw_shape(a, b);
                self.drag = Some(Drag::Shape { a });
            }
            Some(Drag::NewSel { a, .. }) => {
                let b = (p.0.clamp(0, self.pic.w as i32 - 1), p.1.clamp(0, self.pic.h as i32 - 1));
                self.drag = Some(Drag::NewSel { a: (a.0.clamp(0, self.pic.w as i32 - 1), a.1.clamp(0, self.pic.h as i32 - 1)), b });
            }
            Some(Drag::MoveSel { last }) => {
                if let Some(s) = &mut self.sel {
                    s.x += p.0 - last.0;
                    s.y += p.1 - last.1;
                }
                self.drag = Some(Drag::MoveSel { last: p });
            }
            Some(Drag::Pan { last }) => {
                self.scroll.0 -= x - last.0;
                self.scroll.1 -= y - last.1;
                self.clamp_scroll(r);
                self.drag = Some(Drag::Pan { last: (x, y) });
            }
            None => return false,
        }
        true
    }

    fn on_release(&mut self) -> bool {
        match self.drag.take() {
            Some(Drag::NewSel { a, b }) => {
                let (x0, y0, x1, y1) = (a.0.min(b.0), a.1.min(b.1), a.0.max(b.0), a.1.max(b.1));
                if x1 > x0 && y1 > y0 {
                    self.sel = Some(Sel { x: x0, y: y0, w: (x1 - x0 + 1) as usize, h: (y1 - y0 + 1) as usize, float: None });
                }
                true
            }
            Some(Drag::Shape { .. }) => {
                self.base = None;
                true
            }
            Some(_) => true,
            None => false,
        }
    }

    fn on_wheel(&mut self, delta: i8, r: Rect) -> bool {
        if self.keys.ctrl {
            let b = if delta > 0 { Btn::ZoomOut } else { Btn::ZoomIn };
            self.press_button(b, r);
            return true;
        }
        if self.fit {
            return false;
        }
        if self.keys.shift {
            self.scroll.0 += delta as i32 * 60;
        } else {
            self.scroll.1 += delta as i32 * 60;
        }
        self.clamp_scroll(r);
        true
    }

    fn on_key(&mut self, sc: u8, r: Rect) -> EdAction {
        let Some(key) = self.keys.feed(sc) else { return EdAction::None };
        if self.resize_dialog {
            if key == Key::Esc {
                self.resize_dialog = false;
                return EdAction::Redraw;
            }
            return EdAction::None;
        }
        if let Some(t) = &mut self.text {
            match key {
                Key::Char(ch) => t.text.push(ch),
                Key::Enter => t.text.push('\n'),
                Key::Backspace => {
                    t.text.pop();
                }
                Key::Esc => self.commit_text(),
                Key::Ctrl('v') => t.text.push_str(&crate::keys::paste()),
                _ => return EdAction::None,
            }
            return EdAction::Redraw;
        }
        match key {
            Key::Ctrl('z') => self.do_undo(),
            Key::Ctrl('y') => self.do_redo(),
            Key::Ctrl('s') => {
                self.settle();
                return EdAction::Save;
            }
            Key::Ctrl('p') => {
                self.settle();
                return EdAction::Print;
            }
            Key::Ctrl('a') => {
                self.commit_float();
                self.tool = Tool::Select;
                self.sel = Some(Sel { x: 0, y: 0, w: self.pic.w, h: self.pic.h, float: None });
            }
            Key::Ctrl('c') => self.copy(),
            Key::Ctrl('x') => {
                self.copy();
                self.delete_sel();
                self.sel = None;
            }
            Key::Ctrl('v') => self.paste(r),
            Key::Delete | Key::Backspace => {
                self.delete_sel();
            }
            Key::Ctrl('=') => {
                self.press_button(Btn::ZoomIn, r);
            }
            Key::Ctrl('-') => {
                self.press_button(Btn::ZoomOut, r);
            }
            Key::Esc => {
                if self.sel.is_some() {
                    self.commit_float();
                    self.sel = None;
                } else {
                    return EdAction::Close;
                }
            }
            Key::Left | Key::Right | Key::Up | Key::Down if self.sel.is_some() => {
                // Nudge the selection a pixel.
                if self.sel.as_ref().is_some_and(|s| s.float.is_none()) {
                    self.snapshot();
                    let (sx, sy, sw, sh) = {
                        let s = self.sel.as_ref().unwrap();
                        (s.x, s.y, s.w, s.h)
                    };
                    let f = self.pic.region(sx, sy, sw, sh, self.color2);
                    self.pic.fill_rect(sx, sy, sw as i32, sh as i32, self.color2);
                    self.sel.as_mut().unwrap().float = Some(f);
                }
                let s = self.sel.as_mut().unwrap();
                match key {
                    Key::Left => s.x -= 1,
                    Key::Right => s.x += 1,
                    Key::Up => s.y -= 1,
                    _ => s.y += 1,
                }
            }
            _ => return EdAction::None,
        }
        EdAction::Redraw
    }

    fn dirty(&self) -> bool {
        self.dirty || self.text.is_some() || self.sel.as_ref().is_some_and(|s| s.float.is_some())
    }

    fn mark_saved(&mut self) {
        self.dirty = false;
    }

    fn encode(&mut self, ext: &str) -> Result<Vec<u8>, String> {
        self.settle();
        let mut img = Image { w: self.pic.w, h: self.pic.h, px: self.pic.px.iter().map(|p| p | 0xFF00_0000).collect(), opaque: true };
        match ext {
            "jpg" | "jpeg" => crate::edit::encode_jpeg(&mut img, 92).map_err(String::from),
            _ => Ok(crate::image::encode_png(&img)),
        }
    }

    fn print_pages(&mut self, paper: Paper) -> Vec<Canvas> {
        self.settle();
        let (w, h) = crate::print::page_pixels(paper, 100);
        let mut c = Canvas::new(w, h);
        c.px.fill(0xFF_FFFF);
        let img = Image { w: self.pic.w, h: self.pic.h, px: self.pic.px.iter().map(|p| p | 0xFF00_0000).collect(), opaque: true };
        let fit = img.shrink_to_fit(w, h);
        c.draw_image(&fit, ((w - fit.w) / 2) as i32, ((h - fit.h) / 2) as i32);
        Vec::from([c])
    }
}

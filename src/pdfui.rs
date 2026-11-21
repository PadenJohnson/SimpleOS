//! The PDF app: view PDFs (page thumbnails, zoom, scrolling), add text, draw, highlight and
//! sign, turn/move/delete pages, add pages from other PDFs or photos, save and print.
//! "PDF from photos" starts a new PDF from pictures.

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;

use crate::docshell::{DocEditor, EdAction, Kind, DIM, INK};
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::keys::{Key, Keys};
use crate::pdf::edit::{self, Overlay, PageSrc, Photo};
use crate::pdf::file::PdfFile;
use crate::pdf::render::{self, PdfDoc, M};
use crate::print::Paper;

const ACCENT: u32 = rgb(200, 40, 40);
const TOOL_H: i32 = 96;
const STATUS_H: i32 = 28;
const SIDE_W: i32 = 150;
const GAP: i32 = 20;
const PICK_PDF: u8 = 1;
const PICK_PHOTOS: u8 = 2;

fn icon(c: &mut Canvas, x: i32, y: i32) {
    c.round_rect(x, y, 26, 28, 4, ACCENT);
    c.text_centered(x + 13, y + 7, "PDF", Size::Small, true, rgb(255, 255, 255));
}

pub static KIND: Kind = Kind {
    app: "PDF",
    list_title: "PDF files",
    new_label: "New empty PDF",
    exts: &["pdf"],
    save_ext: "pdf",
    keeps: &["pdf"],
    accent: ACCENT,
    empty: ["No PDF files yet.", "Plug in a USB drive that has PDFs, or make one from photos."],
    extra: &[("PDF from photos", PICK_PHOTOS, &["jpg", "jpeg", "png"], "Choose photos for the PDF")],
    icon,
};

#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
enum Src {
    Pdf(usize, usize),
    Photo(usize),
}

#[derive(Clone)]
struct PageItem {
    src: Src,
    rotate: i32,
    overlays: Vec<Overlay>,
    /// Changes when the overlays do (for the drawing cache).
    ver: u32,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Select,
    Text,
    Draw,
    Highlight,
    Sign,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Btn {
    Back,
    Save,
    SaveAs,
    Print,
    ZoomOut,
    ZoomIn,
    Fit,
    Undo,
    Tool(Tool),
    Color(usize),
    TurnLeft,
    TurnRight,
    MoveUp,
    MoveDown,
    Delete,
    AddPdf,
    AddPhotos,
}

const COLORS: [u32; 5] = [rgb(20, 20, 20), rgb(30, 60, 170), rgb(200, 30, 30), rgb(20, 130, 60), rgb(255, 220, 0)];

enum Drag {
    Ink { page: usize, pts: Vec<(f32, f32)> },
    Box { page: usize, a: (f32, f32), b: (f32, f32) },
    Move { page: usize, idx: usize, last: (f32, f32) },
}

struct SigPad {
    strokes: Vec<Vec<(i32, i32)>>,
    drawing: bool,
}

pub struct PdfEd {
    docs: Vec<Rc<PdfDoc>>,
    photos: Vec<Photo>,
    pages: Vec<PageItem>,
    undo: Vec<Vec<PageItem>>,
    cache: RefCell<BTreeMap<(Src, i32, i32, u32), Rc<Canvas>>>,
    bases: RefCell<BTreeMap<(Src, i32, i32), Rc<Canvas>>>,
    thumbs: RefCell<BTreeMap<(Src, i32), Rc<Canvas>>>,
    photo_imgs: RefCell<BTreeMap<usize, Rc<render::Img>>>,
    zoom: f32,
    fit: bool,
    scroll: i32,
    side_scroll: i32,
    current: usize,
    tool: Tool,
    color: usize,
    editing: Option<(usize, usize)>,
    selected: Option<(usize, usize)>,
    drag: Option<Drag>,
    keys: Keys,
    dirty: bool,
    signature: Option<Vec<Vec<(f32, f32)>>>,
    pad: Option<SigPad>,
    note: Option<String>,
    /// Main area size last drawn (for fitting).
    view_w: core::cell::Cell<i32>,
}

pub fn new_doc() -> PdfEd {
    PdfEd {
        docs: Vec::new(),
        photos: Vec::new(),
        pages: Vec::new(),
        undo: Vec::new(),
        cache: RefCell::new(BTreeMap::new()),
        bases: RefCell::new(BTreeMap::new()),
        thumbs: RefCell::new(BTreeMap::new()),
        photo_imgs: RefCell::new(BTreeMap::new()),
        zoom: 1.0,
        fit: true,
        scroll: 0,
        side_scroll: 0,
        current: 0,
        tool: Tool::Select,
        color: 0,
        editing: None,
        selected: None,
        drag: None,
        keys: Keys::default(),
        dirty: false,
        signature: None,
        pad: None,
        note: None,
        view_w: core::cell::Cell::new(900),
    }
}

pub fn load(_name: &str, b: &[u8]) -> Result<PdfEd, String> {
    let file = PdfFile::open(b.to_vec())?;
    let mut ed = new_doc();
    let n = file.pages.len();
    if file.encrypted() {
        ed.note = Some(String::from("This PDF was protected; saving makes an unprotected copy"));
    }
    ed.docs.push(Rc::new(PdfDoc::new(file)));
    ed.pages = (0..n).map(|i| PageItem { src: Src::Pdf(0, i), rotate: 0, overlays: Vec::new(), ver: 0 }).collect();
    Ok(ed)
}

fn photo_from(bytes: &[u8]) -> Option<Photo> {
    match crate::image::sniff(bytes) {
        Some(crate::image::Format::Jpeg) => {
            use zune_core::bytestream::ZCursor;
            let mut d = zune_jpeg::JpegDecoder::new(ZCursor::new(bytes));
            d.decode_headers().ok()?;
            let info = d.info()?;
            let gray = info.components == 1;
            // CMYK JPEGs would need more care: re-encode those.
            if info.components == 1 || info.components == 3 {
                return Some(Photo::Jpeg { data: bytes.to_vec(), w: info.width as usize, h: info.height as usize, gray });
            }
            let img = crate::image::decode(bytes).ok()?;
            Some(rgb_photo(&img))
        }
        Some(crate::image::Format::Png) => {
            let img = crate::image::decode(bytes).ok()?;
            Some(rgb_photo(&img))
        }
        None => None,
    }
}

fn rgb_photo(img: &crate::image::Image) -> Photo {
    let mut px = Vec::with_capacity(img.w * img.h * 3);
    for &p in &img.px {
        // Transparent parts go on white.
        let a = p >> 24;
        let ch = |s: u32| (((p >> s) & 0xFF) * a + 255 * (255 - a)) / 255;
        px.extend_from_slice(&[ch(16) as u8, ch(8) as u8, ch(0) as u8]);
    }
    Photo::Rgb { px, w: img.w, h: img.h }
}

impl PdfEd {
    // ---- pages ---------------------------------------------------------------------------

    /// The fake page a photo sits on (for matrices).
    fn photo_page(ph: &Photo) -> crate::pdf::file::Page {
        let (w, h) = ph.page_size();
        crate::pdf::file::Page { id: 0, dict: Default::default(), resources: Default::default(), media: [0.0, 0.0, w, h], crop: [0.0, 0.0, w, h], rotate: 0 }
    }

    fn page_obj(&self, it: &PageItem) -> crate::pdf::file::Page {
        match it.src {
            Src::Pdf(d, i) => self.docs[d].file.pages[i].clone(),
            Src::Photo(k) => Self::photo_page(&self.photos[k]),
        }
    }

    /// Size on screen of page `i` in points (after turning).
    fn page_pt(&self, i: usize) -> (f32, f32) {
        let p = self.page_obj(&self.pages[i]);
        let (w, h) = ((p.crop[2] - p.crop[0]).abs(), (p.crop[3] - p.crop[1]).abs());
        if (p.rotate + self.pages[i].rotate).rem_euclid(180) == 90 {
            (h, w)
        } else {
            (w, h)
        }
    }

    fn main_area(r: Rect) -> Rect {
        Rect::new(r.x + SIDE_W, r.y + TOOL_H, r.w - SIDE_W, r.h - TOOL_H - STATUS_H)
    }

    fn zoom_now(&self, r: Rect) -> f32 {
        if !self.fit || self.pages.is_empty() {
            return self.zoom;
        }
        let a = Self::main_area(r);
        let maxw = (0..self.pages.len()).map(|i| self.page_pt(i).0).fold(1.0f32, f32::max);
        ((a.w - 2 * GAP) as f32 / maxw).clamp(0.2, 2.5)
    }

    /// Screen rectangle of page i.
    fn page_rect(&self, r: Rect, i: usize) -> Rect {
        let a = Self::main_area(r);
        let z = self.zoom_now(r);
        let mut y = a.y + GAP - self.scroll;
        for k in 0..i {
            y += (self.page_pt(k).1 * z) as i32 + GAP;
        }
        let (w, h) = self.page_pt(i);
        let (pw, ph) = ((w * z) as i32, (h * z) as i32);
        Rect::new(a.x + ((a.w - pw) / 2).max(GAP), y, pw, ph)
    }

    fn total_h(&self, r: Rect) -> i32 {
        let z = self.zoom_now(r);
        (0..self.pages.len()).map(|k| (self.page_pt(k).1 * z) as i32 + GAP).sum::<i32>() + GAP
    }

    /// User space -> screen matrix for page i.
    fn matrix(&self, r: Rect, i: usize) -> M {
        let pr = self.page_rect(r, i);
        let z = self.zoom_now(r);
        let p = self.page_obj(&self.pages[i]);
        let m = render::page_matrix(&p, z, self.pages[i].rotate);
        [m[0], m[1], m[2], m[3], m[4] + pr.x as f32, m[5] + pr.y as f32]
    }

    fn hit_page(&self, r: Rect, x: i32, y: i32) -> Option<(usize, (f32, f32))> {
        for i in 0..self.pages.len() {
            let pr = self.page_rect(r, i);
            if pr.contains(x, y) {
                let inv = render::invert(&self.matrix(r, i))?;
                return Some((i, render::apply(&inv, x as f32 + 0.5, y as f32 + 0.5)));
            }
        }
        None
    }

    /// Draw page `i` (with what's been added) at `z` pixels per point.
    fn render_page(&self, i: usize, z: f32) -> Canvas {
        let base = self.render_base(i, z);
        self.with_overlays(i, z, base)
    }

    /// The page itself, without what's been added (cached: typing only redraws the additions).
    fn base_cached(&self, i: usize, z: f32) -> Rc<Canvas> {
        let it = &self.pages[i];
        let key = (it.src, it.rotate, (z * 1000.0) as i32);
        if let Some(c) = self.bases.borrow().get(&key) {
            return c.clone();
        }
        let c = Rc::new(self.render_base(i, z));
        let mut b = self.bases.borrow_mut();
        if b.len() > 6 {
            b.clear();
        }
        b.insert(key, c.clone());
        c
    }

    fn with_overlays(&self, i: usize, z: f32, mut c: Canvas) -> Canvas {
        let it = &self.pages[i];
        if !it.overlays.is_empty() {
            let pg = self.page_obj(it);
            let m = render::page_matrix(&pg, z, it.rotate);
            let content = edit::overlay_content(&it.overlays);
            let doc = self.docs.first().cloned();
            match doc {
                Some(d) => d.draw_overlay(&mut c, &m, &content),
                None => PdfDoc::draw_overlay_plain(&mut c, &m, &content),
            }
        }
        c
    }

    fn render_base(&self, i: usize, z: f32) -> Canvas {
        let it = &self.pages[i];
        match it.src {
            Src::Pdf(d, p) => self.docs[d].render(p, z, it.rotate),
            Src::Photo(k) => {
                let ph = &self.photos[k];
                let pg = Self::photo_page(ph);
                let (w, h) = render::page_pixels(&pg, z, it.rotate);
                let mut c = Canvas::new(w, h);
                c.px.fill(0xFF_FFFF);
                let img = self.photo_img(k);
                if let Some(img) = img {
                    let m = render::page_matrix(&pg, z, it.rotate);
                    let (x, y, pw, phh) = ph.placement();
                    let im = render::mul(&[pw, 0.0, 0.0, phh, x, y], &m);
                    render::draw_image(&mut c, &img, &im, None, 255, 0);
                }
                c
            }
        }
    }

    fn photo_img(&self, k: usize) -> Option<Rc<render::Img>> {
        if let Some(i) = self.photo_imgs.borrow().get(&k) {
            return Some(i.clone());
        }
        let img = match &self.photos[k] {
            Photo::Jpeg { data, .. } => {
                let im = crate::image::decode(data).ok()?;
                render::Img { w: im.w, h: im.h, px: im.px.iter().map(|p| p | 0xFF00_0000).collect(), mask_only: false }
            }
            Photo::Rgb { px, w, h } => render::Img { w: *w, h: *h, px: px.chunks_exact(3).map(|p| 0xFF00_0000 | (p[0] as u32) << 16 | (p[1] as u32) << 8 | p[2] as u32).collect(), mask_only: false },
        };
        let img = Rc::new(img);
        self.photo_imgs.borrow_mut().insert(k, img.clone());
        Some(img)
    }

    fn snapshot(&mut self) {
        self.undo.push(self.pages.clone());
        if self.undo.len() > 40 {
            self.undo.remove(0);
        }
        self.dirty = true;
    }

    fn touch(&mut self, i: usize) {
        if let Some(p) = self.pages.get_mut(i) {
            p.ver = p.ver.wrapping_add(1);
        }
        self.dirty = true;
    }

    fn finish_text(&mut self) {
        if let Some((p, k)) = self.editing.take() {
            if let Some(Overlay::Text { text, .. }) = self.pages.get(p).and_then(|pg| pg.overlays.get(k)) {
                if text.trim().is_empty() {
                    self.pages[p].overlays.remove(k);
                    self.touch(p);
                }
            }
        }
    }

    // ---- toolbar -------------------------------------------------------------------------

    fn buttons(&self, r: Rect) -> Vec<(Btn, Rect, &'static str)> {
        let row1: [(Btn, &str); 8] = [(Btn::Back, "< PDFs"), (Btn::Save, "Save"), (Btn::SaveAs, "Save as"), (Btn::Print, "Print"), (Btn::Undo, "Undo"), (Btn::ZoomOut, "-"), (Btn::ZoomIn, "+"), (Btn::Fit, "Fit width")];
        let row2: [(Btn, &str); 17] = [
            (Btn::Tool(Tool::Select), "Select"),
            (Btn::Tool(Tool::Text), "Text"),
            (Btn::Tool(Tool::Draw), "Draw"),
            (Btn::Tool(Tool::Highlight), "Highlight"),
            (Btn::Tool(Tool::Sign), "Sign"),
            (Btn::Color(0), ""),
            (Btn::Color(1), ""),
            (Btn::Color(2), ""),
            (Btn::Color(3), ""),
            (Btn::Color(4), ""),
            (Btn::TurnLeft, "Turn left"),
            (Btn::TurnRight, "Turn right"),
            (Btn::MoveUp, "Up"),
            (Btn::MoveDown, "Down"),
            (Btn::Delete, "Delete page"),
            (Btn::AddPdf, "Add PDF"),
            (Btn::AddPhotos, "Add photos"),
        ];
        let mut v = Vec::new();
        let mut x = r.x + 10;
        for (b, l) in row1 {
            let w = match b {
                Btn::ZoomOut | Btn::ZoomIn => 34,
                _ => text_width(l, Size::Small, true) + 24,
            };
            if matches!(b, Btn::Undo | Btn::ZoomOut) {
                x += 14;
            }
            if b == Btn::ZoomIn {
                x += 62;
            }
            v.push((b, Rect::new(x, r.y + 7, w, 34), l));
            x += w + 6;
        }
        let mut x = r.x + 10;
        for (b, l) in row2 {
            let w = match b {
                Btn::Color(_) => 28,
                _ => text_width(l, Size::Small, true) + 22,
            };
            if matches!(b, Btn::Color(0) | Btn::TurnLeft | Btn::AddPdf) {
                x += 12;
            }
            v.push((b, Rect::new(x, r.y + 52, w, 34), l));
            x += w + if matches!(b, Btn::Color(_)) { 4 } else { 5 };
        }
        v
    }

    fn sig_box(r: Rect) -> Rect {
        let w = 520.min(r.w - 40);
        Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - 300) / 2, w, 300)
    }
    fn sig_area(r: Rect) -> Rect {
        let b = Self::sig_box(r);
        Rect::new(b.x + 20, b.y + 54, b.w - 40, 170)
    }
    fn sig_buttons(r: Rect) -> [Rect; 3] {
        let b = Self::sig_box(r);
        [Rect::new(b.x + b.w - 20 - 120, b.y + b.h - 56, 120, 38), Rect::new(b.x + b.w - 20 - 120 - 10 - 90, b.y + b.h - 56, 90, 38), Rect::new(b.x + 20, b.y + b.h - 56, 90, 38)]
    }

    fn press_button(&mut self, b: Btn, r: Rect) -> EdAction {
        self.finish_text();
        let n = self.pages.len();
        let cur = self.current.min(n.saturating_sub(1));
        match b {
            Btn::Back => return EdAction::Close,
            Btn::Save => return if n == 0 { EdAction::Message(String::from("Add some pages first.")) } else { EdAction::Save },
            Btn::SaveAs => return if n == 0 { EdAction::Message(String::from("Add some pages first.")) } else { EdAction::SaveAs },
            Btn::Print => return EdAction::Print,
            Btn::Undo => {
                if let Some(p) = self.undo.pop() {
                    self.pages = p;
                    self.selected = None;
                    self.editing = None;
                    self.current = self.current.min(self.pages.len().saturating_sub(1));
                    self.cache.borrow_mut().clear();
                    self.bases.borrow_mut().clear();
                    self.dirty = true;
                }
            }
            Btn::ZoomOut | Btn::ZoomIn => {
                let z = self.zoom_now(r);
                let frac = self.scroll as f32 / self.total_h(r).max(1) as f32;
                self.fit = false;
                self.zoom = (if b == Btn::ZoomIn { z * 1.25 } else { z / 1.25 }).clamp(0.2, 4.0);
                self.scroll = (frac * self.total_h(r) as f32) as i32;
                self.clamp(r);
            }
            Btn::Fit => {
                self.fit = true;
                self.clamp(r);
            }
            Btn::Tool(t) => {
                if t == Tool::Sign && (self.signature.is_none() || self.tool == Tool::Sign) {
                    self.pad = Some(SigPad { strokes: Vec::new(), drawing: false });
                }
                self.tool = t;
                self.selected = None;
            }
            Btn::Color(k) => {
                self.color = k;
                if let Some((p, i)) = self.selected.or(self.editing) {
                    let c = COLORS[k];
                    self.snapshot();
                    match self.pages[p].overlays.get_mut(i) {
                        Some(Overlay::Text { color, .. }) | Some(Overlay::Ink { color, .. }) | Some(Overlay::Highlight { color, .. }) => *color = c,
                        None => {}
                    }
                    self.touch(p);
                }
            }
            Btn::TurnLeft | Btn::TurnRight if n > 0 => {
                self.snapshot();
                self.pages[cur].rotate = (self.pages[cur].rotate + if b == Btn::TurnLeft { 270 } else { 90 }).rem_euclid(360);
                self.clamp(r);
            }
            Btn::MoveUp if cur > 0 => {
                self.snapshot();
                self.pages.swap(cur, cur - 1);
                self.current = cur - 1;
                self.scroll_to(r, self.current);
            }
            Btn::MoveDown if cur + 1 < n => {
                self.snapshot();
                self.pages.swap(cur, cur + 1);
                self.current = cur + 1;
                self.scroll_to(r, self.current);
            }
            Btn::Delete if n > 1 => {
                self.snapshot();
                self.pages.remove(cur);
                self.current = cur.min(self.pages.len() - 1);
                self.selected = None;
                self.clamp(r);
                return EdAction::Toast(format!("Page {} deleted (Undo brings it back)", cur + 1));
            }
            Btn::Delete => return EdAction::Toast(String::from("A PDF needs at least one page")),
            Btn::AddPdf => return EdAction::Pick { purpose: PICK_PDF, exts: &["pdf"], title: "Add the pages of which PDF?", many: false },
            Btn::AddPhotos => return EdAction::Pick { purpose: PICK_PHOTOS, exts: &["jpg", "jpeg", "png"], title: "Choose photos to add as pages", many: true },
            _ => {}
        }
        EdAction::Redraw
    }

    fn clamp(&mut self, r: Rect) {
        let a = Self::main_area(r);
        self.scroll = self.scroll.clamp(0, (self.total_h(r) - a.h).max(0));
    }

    fn scroll_to(&mut self, r: Rect, i: usize) {
        let pr = self.page_rect(r, i);
        let a = Self::main_area(r);
        self.scroll += pr.y - a.y - GAP;
        self.clamp(r);
    }

    fn thumb_rect(&self, r: Rect, i: usize) -> Rect {
        let a = Self::main_area(r);
        Rect::new(r.x + 14, a.y + 10 + i as i32 * 150 - self.side_scroll, SIDE_W - 28, 136)
    }

    fn overlay_hit(&self, p: usize, pt: (f32, f32)) -> Option<usize> {
        let it = &self.pages[p];
        for (k, o) in it.overlays.iter().enumerate().rev() {
            let hit = match o {
                Overlay::Text { x, y, size, text, .. } => {
                    let lines = text.split('\n').count() as f32;
                    let w = text.split('\n').map(|l| l.chars().count()).max().unwrap_or(1) as f32 * size * 0.55;
                    pt.0 >= *x - 4.0 && pt.0 <= x + w.max(20.0) + 4.0 && pt.1 <= y + size && pt.1 >= y - size * 1.2 * (lines - 1.0) - size * 0.3
                }
                Overlay::Highlight { x0, y0, x1, y1, .. } => pt.0 >= x0.min(*x1) && pt.0 <= x0.max(*x1) && pt.1 >= y0.min(*y1) && pt.1 <= y0.max(*y1),
                Overlay::Ink { strokes, width, .. } => strokes.iter().flatten().any(|&(x, y)| (x - pt.0).abs() < width + 6.0 && (y - pt.1).abs() < width + 6.0),
            };
            if hit {
                return Some(k);
            }
        }
        None
    }
}

fn move_overlay(o: &mut Overlay, dx: f32, dy: f32) {
    match o {
        Overlay::Text { x, y, .. } => {
            *x += dx;
            *y += dy;
        }
        Overlay::Highlight { x0, y0, x1, y1, .. } => {
            *x0 += dx;
            *x1 += dx;
            *y0 += dy;
            *y1 += dy;
        }
        Overlay::Ink { strokes, .. } => {
            for s in strokes {
                for p in s {
                    p.0 += dx;
                    p.1 += dy;
                }
            }
        }
    }
}

impl DocEditor for PdfEd {
    fn draw(&self, c: &mut Canvas, r: Rect) {
        let a = Self::main_area(r);
        self.view_w.set(a.w);
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(226, 228, 233));
        // Pages.
        c.set_clip(a);
        let z = self.zoom_now(r);
        let zk = (z * 1000.0) as i32;
        let mut cache = self.cache.borrow_mut();
        for i in 0..self.pages.len() {
            let pr = self.page_rect(r, i);
            if pr.y > a.y + a.h || pr.y + pr.h < a.y {
                continue;
            }
            let it = &self.pages[i];
            let key = (it.src, it.rotate, zk, it.ver);
            let img = match cache.get(&key) {
                Some(im) => im.clone(),
                None => {
                    if cache.len() > 10 {
                        cache.clear();
                    }
                    let base = (*self.base_cached(i, z)).clone();
                    let im = Rc::new(self.with_overlays(i, z, base));
                    cache.insert(key, im.clone());
                    im
                }
            };
            c.shadow(pr.x, pr.y + 2, pr.w, pr.h, 2, 10, 60);
            let im = crate::image::Image { w: img.w, h: img.h, px: Vec::new(), opaque: true };
            let _ = im;
            blit(c, &img, pr.x, pr.y);
            if i == self.current && self.pages.len() > 1 {
                c.outline_rect(pr.x - 3, pr.y - 3, pr.w + 6, pr.h + 6, rgb(90, 140, 230));
            }
            // Selection / editing outline.
            if let Some((p, k)) = self.selected.or(self.editing) {
                if p == i {
                    if let Some(o) = it.overlays.get(k) {
                        let m = self.matrix(r, i);
                        let (x0, y0, x1, y1) = overlay_box(o);
                        let pts = [render::apply(&m, x0, y0), render::apply(&m, x1, y1)];
                        let (sx0, sy0) = (pts[0].0.min(pts[1].0) as i32, pts[0].1.min(pts[1].1) as i32);
                        let (sx1, sy1) = (pts[0].0.max(pts[1].0) as i32, pts[0].1.max(pts[1].1) as i32);
                        c.outline_rect(sx0 - 3, sy0 - 3, sx1 - sx0 + 6, sy1 - sy0 + 6, rgb(40, 110, 230));
                    }
                }
            }
        }
        // Drag previews.
        match &self.drag {
            Some(Drag::Ink { page, pts }) => {
                let m = self.matrix(r, *page);
                for w in pts.windows(2) {
                    let (a0, b0) = (render::apply(&m, w[0].0, w[0].1), render::apply(&m, w[1].0, w[1].1));
                    line(c, a0, b0, COLORS[self.color]);
                }
            }
            Some(Drag::Box { page, a: p0, b: p1 }) => {
                let m = self.matrix(r, *page);
                let (q0, q1) = (render::apply(&m, p0.0, p0.1), render::apply(&m, p1.0, p1.1));
                let (x0, y0) = (q0.0.min(q1.0) as i32, q0.1.min(q1.1) as i32);
                c.fill_rect_alpha(x0, y0, (q1.0 - q0.0).abs() as i32, (q1.1 - q0.1).abs() as i32, COLORS[4], 110);
            }
            _ => {}
        }
        if self.pages.is_empty() {
            c.text_centered(a.x + a.w / 2, a.y + a.h / 2 - 20, "This PDF has no pages yet.", Size::Medium, true, INK);
            c.text_centered(a.x + a.w / 2, a.y + a.h / 2 + 12, "Use Add photos or Add PDF (above) to put pages in.", Size::Small, false, DIM);
        }
        c.reset_clip();
        c.set_clip(r);
        // Thumbnails.
        let side = Rect::new(r.x, a.y, SIDE_W, a.h);
        c.fill_rect(side.x, side.y, side.w, side.h, rgb(240, 241, 244));
        c.fill_rect(side.x + side.w - 1, side.y, 1, side.h, rgb(214, 218, 226));
        c.set_clip(side);
        let mut thumbs = self.thumbs.borrow_mut();
        for i in 0..self.pages.len() {
            let tr = self.thumb_rect(r, i);
            if tr.y > side.y + side.h || tr.y + tr.h < side.y {
                continue;
            }
            if i == self.current {
                c.round_rect(tr.x - 6, tr.y - 4, tr.w + 12, tr.h + 8, 8, rgb(208, 222, 248));
            }
            let (pw, ph) = self.page_pt(i);
            let s = ((tr.w - 10) as f32 / pw).min((tr.h - 24) as f32 / ph);
            let it = &self.pages[i];
            let key = (it.src, it.rotate);
            let t = match thumbs.get(&key) {
                Some(t) => t.clone(),
                None => {
                    let t = Rc::new(match it.src {
                        Src::Pdf(d, p) => self.docs[d].render(p, s, it.rotate),
                        Src::Photo(_) => self.render_page(i, s),
                    });
                    thumbs.insert(key, t.clone());
                    t
                }
            };
            let tx = tr.x + (tr.w - t.w as i32) / 2;
            blit(c, &t, tx, tr.y);
            c.outline_rect(tx - 1, tr.y - 1, t.w as i32 + 2, t.h as i32 + 2, rgb(200, 204, 212));
            c.text_centered(tr.x + tr.w / 2, tr.y + t.h as i32 + 4, &format!("{}", i + 1), Size::Small, false, DIM);
        }
        drop(thumbs);
        c.reset_clip();
        c.set_clip(r);
        // Toolbar.
        c.fill_rect(r.x, r.y, r.w, TOOL_H, rgb(244, 246, 250));
        c.fill_rect(r.x, r.y + TOOL_H - 1, r.w, 1, rgb(220, 224, 232));
        for (b, br, label) in self.buttons(r) {
            let on = matches!(b, Btn::Tool(t) if t == self.tool) || matches!(b, Btn::Color(k) if k == self.color) || (b == Btn::Fit && self.fit);
            let primary = b == Btn::Save && self.dirty;
            c.round_rect(br.x, br.y, br.w, br.h, 7, if on { rgb(90, 140, 230) } else { rgb(216, 220, 228) });
            let bg = if primary { ACCENT } else if on && !matches!(b, Btn::Color(_)) { rgb(214, 226, 248) } else { rgb(255, 255, 255) };
            c.round_rect(br.x + if on { 2 } else { 1 }, br.y + if on { 2 } else { 1 }, br.w - if on { 4 } else { 2 }, br.h - if on { 4 } else { 2 }, 6, bg);
            match b {
                Btn::Color(k) => c.round_rect(br.x + 6, br.y + 7, br.w - 12, br.h - 14, 4, COLORS[k]),
                _ => c.text_centered(br.x + br.w / 2, br.y + 9, label, Size::Small, true, if primary { rgb(255, 255, 255) } else { INK }),
            }
            if b == Btn::ZoomOut {
                c.text_centered(br.x + br.w + 34, br.y + 9, &format!("{}%", (z * 100.0 + 0.5) as i32), Size::Small, true, INK);
            }
        }
        // Status bar.
        let sy = r.y + r.h - STATUS_H;
        c.fill_rect(r.x, sy, r.w, STATUS_H, rgb(244, 246, 250));
        c.fill_rect(r.x, sy, r.w, 1, rgb(220, 224, 232));
        let hint = match self.tool {
            Tool::Select => "Click something you added to choose it, drag to move it, Delete to remove it",
            Tool::Text => "Click on the page where the text should go, then type (Esc when done)",
            Tool::Draw => "Hold the mouse button and draw",
            Tool::Highlight => "Drag over what you want to highlight",
            Tool::Sign => "Click where your signature should go (press Sign again for a new one)",
        };
        let mut s = format!("Page {} of {}     {}", (self.current + 1).min(self.pages.len().max(1)), self.pages.len(), hint);
        if let Some(n) = &self.note {
            s = format!("{}     {}", n, s);
        }
        c.text(r.x + 14, sy + 6, &crate::photos::ellipsize(&s, r.w - 28, Size::Small, false), Size::Small, false, DIM);
        // Signature pad.
        if let Some(pad) = &self.pad {
            c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 110);
            let b = Self::sig_box(r);
            c.shadow(b.x, b.y + 6, b.w, b.h, 14, 22, 100);
            c.round_rect(b.x, b.y, b.w, b.h, 14, rgb(255, 255, 255));
            c.text(b.x + 20, b.y + 18, "Sign with the mouse in the box", Size::Medium, true, INK);
            let ar = Self::sig_area(r);
            c.round_rect(ar.x, ar.y, ar.w, ar.h, 8, rgb(248, 249, 252));
            c.fill_rect(ar.x + 20, ar.y + ar.h - 40, ar.w - 40, 1, rgb(180, 186, 196));
            for s in &pad.strokes {
                for w in s.windows(2) {
                    line(c, (w[0].0 as f32, w[0].1 as f32), (w[1].0 as f32, w[1].1 as f32), rgb(20, 40, 120));
                    line(c, (w[0].0 as f32 + 0.6, w[0].1 as f32), (w[1].0 as f32 + 0.6, w[1].1 as f32), rgb(20, 40, 120));
                }
            }
            let [use_b, cancel_b, clear_b] = Self::sig_buttons(r);
            crate::docshell::button(c, use_b, "Use this", true, ACCENT);
            crate::docshell::button(c, cancel_b, "Cancel", false, ACCENT);
            crate::docshell::button(c, clear_b, "Clear", false, ACCENT);
        }
    }

    fn on_press(&mut self, x: i32, y: i32, r: Rect, _now: u64) -> EdAction {
        if let Some(pad) = &mut self.pad {
            let ar = Self::sig_area(r);
            let [use_b, cancel_b, clear_b] = Self::sig_buttons(r);
            if ar.contains(x, y) {
                pad.strokes.push(Vec::from([(x, y)]));
                pad.drawing = true;
            } else if clear_b.contains(x, y) {
                pad.strokes.clear();
            } else if cancel_b.contains(x, y) {
                self.pad = None;
                if self.signature.is_none() {
                    self.tool = Tool::Select;
                }
            } else if use_b.contains(x, y) && pad.strokes.iter().any(|s| s.len() > 1) {
                // Normalise to a box 1.0 wide (y up), left/top at 0.
                let pts: Vec<(i32, i32)> = pad.strokes.iter().flatten().copied().collect();
                let (x0, x1) = (pts.iter().map(|p| p.0).min().unwrap(), pts.iter().map(|p| p.0).max().unwrap());
                let (y0, y1) = (pts.iter().map(|p| p.1).min().unwrap(), pts.iter().map(|p| p.1).max().unwrap());
                let w = (x1 - x0).max(1) as f32;
                let _ = y1;
                self.signature = Some(pad.strokes.iter().map(|s| s.iter().map(|&(px, py)| ((px - x0) as f32 / w, -((py - y0) as f32) / w)).collect()).collect());
                self.pad = None;
                self.tool = Tool::Sign;
            }
            return EdAction::Redraw;
        }
        if let Some((b, _, _)) = self.buttons(r).into_iter().find(|(_, br, _)| br.contains(x, y)) {
            return self.press_button(b, r);
        }
        if x < r.x + SIDE_W && y > r.y + TOOL_H {
            if let Some(i) = (0..self.pages.len()).find(|&i| self.thumb_rect(r, i).contains(x, y)) {
                self.finish_text();
                self.current = i;
                self.scroll_to(r, i);
                return EdAction::Redraw;
            }
            return EdAction::None;
        }
        let Some((p, pt)) = self.hit_page(r, x, y) else {
            self.finish_text();
            self.selected = None;
            return EdAction::Redraw;
        };
        self.current = p;
        let col = COLORS[self.color];
        match self.tool {
            Tool::Select => {
                self.finish_text();
                self.selected = self.overlay_hit(p, pt).map(|k| (p, k));
                if let Some((_, k)) = self.selected {
                    self.snapshot();
                    self.drag = Some(Drag::Move { page: p, idx: k, last: pt });
                }
            }
            Tool::Text => {
                if let Some(k) = self.overlay_hit(p, pt).filter(|&k| matches!(self.pages[p].overlays[k], Overlay::Text { .. })) {
                    self.finish_text();
                    self.editing = Some((p, k));
                } else {
                    self.finish_text();
                    self.snapshot();
                    let col = if self.color == 4 { COLORS[0] } else { col };
                    self.pages[p].overlays.push(Overlay::Text { x: pt.0, y: pt.1 - 10.0, size: 14.0, color: col, text: String::new() });
                    self.editing = Some((p, self.pages[p].overlays.len() - 1));
                    self.touch(p);
                }
            }
            Tool::Draw => {
                self.finish_text();
                self.drag = Some(Drag::Ink { page: p, pts: Vec::from([pt]) });
            }
            Tool::Highlight => {
                self.finish_text();
                self.drag = Some(Drag::Box { page: p, a: pt, b: pt });
            }
            Tool::Sign => {
                self.finish_text();
                if let Some(sig) = self.signature.clone() {
                    self.snapshot();
                    let w = 150.0;
                    let strokes: Vec<Vec<(f32, f32)>> = sig.iter().map(|s| s.iter().map(|&(u, v)| (pt.0 - w / 2.0 + u * w, pt.1 + 20.0 + v * w)).collect()).collect();
                    let c = if self.color == 4 { COLORS[1] } else { col };
                    self.pages[p].overlays.push(Overlay::Ink { strokes, width: 1.6, color: c });
                    self.touch(p);
                } else {
                    self.pad = Some(SigPad { strokes: Vec::new(), drawing: false });
                }
            }
        }
        EdAction::Redraw
    }

    fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        if let Some(pad) = &mut self.pad {
            let ar = Self::sig_area(r);
            if pad.drawing {
                if let Some(s) = pad.strokes.last_mut() {
                    s.push((x.clamp(ar.x, ar.x + ar.w), y.clamp(ar.y, ar.y + ar.h)));
                }
                return true;
            }
            return false;
        }
        let page = match &self.drag {
            Some(Drag::Ink { page, .. }) | Some(Drag::Box { page, .. }) | Some(Drag::Move { page, .. }) => *page,
            None => return false,
        };
        let Some(inv) = render::invert(&self.matrix(r, page)) else { return false };
        let pt = render::apply(&inv, x as f32, y as f32);
        match &mut self.drag {
            Some(Drag::Ink { pts, .. }) => pts.push(pt),
            Some(Drag::Box { b, .. }) => *b = pt,
            Some(Drag::Move { idx, last, .. }) => {
                let (dx, dy) = (pt.0 - last.0, pt.1 - last.1);
                let k = *idx;
                *last = pt;
                if let Some(o) = self.pages[page].overlays.get_mut(k) {
                    move_overlay(o, dx, dy);
                }
                self.touch(page);
            }
            None => {}
        }
        true
    }

    fn on_release(&mut self) -> bool {
        if let Some(pad) = &mut self.pad {
            pad.drawing = false;
            return true;
        }
        match self.drag.take() {
            Some(Drag::Ink { page, pts }) if !pts.is_empty() => {
                self.snapshot();
                self.pages[page].overlays.push(Overlay::Ink { strokes: Vec::from([pts]), width: 2.0, color: COLORS[self.color] });
                self.touch(page);
                true
            }
            Some(Drag::Box { page, a, b }) => {
                if (a.0 - b.0).abs() > 2.0 && (a.1 - b.1).abs() > 2.0 {
                    self.snapshot();
                    let c = if self.color == 0 { COLORS[4] } else { COLORS[self.color] };
                    self.pages[page].overlays.push(Overlay::Highlight { x0: a.0, y0: a.1, x1: b.0, y1: b.1, color: c });
                    self.touch(page);
                }
                true
            }
            _ => false,
        }
    }

    fn on_wheel(&mut self, delta: i8, r: Rect) -> bool {
        let old = self.scroll;
        self.scroll += delta as i32 * 80;
        self.clamp(r);
        // Keep the current page in step with what's on screen.
        let a = Self::main_area(r);
        if let Some(i) = (0..self.pages.len()).find(|&i| {
            let pr = self.page_rect(r, i);
            pr.y + pr.h > a.y + a.h / 3
        }) {
            self.current = i;
        }
        old != self.scroll
    }

    fn on_key(&mut self, sc: u8, r: Rect) -> EdAction {
        let Some(key) = self.keys.feed(sc) else { return EdAction::None };
        if self.pad.is_some() {
            if key == Key::Esc {
                self.pad = None;
                return EdAction::Redraw;
            }
            return EdAction::None;
        }
        if let Some((p, k)) = self.editing {
            if let Some(Overlay::Text { text, .. }) = self.pages.get_mut(p).and_then(|pg| pg.overlays.get_mut(k)) {
                match key {
                    Key::Char(ch) => text.push(ch),
                    Key::Enter => text.push('\n'),
                    Key::Backspace => {
                        text.pop();
                    }
                    Key::Esc => {
                        self.finish_text();
                        return EdAction::Redraw;
                    }
                    Key::Ctrl('v') => text.push_str(&crate::keys::paste()),
                    _ => return EdAction::None,
                }
                self.touch(p);
                return EdAction::Redraw;
            }
        }
        let n = self.pages.len();
        match key {
            Key::Delete | Key::Backspace => {
                if let Some((p, k)) = self.selected.take() {
                    self.snapshot();
                    if k < self.pages[p].overlays.len() {
                        self.pages[p].overlays.remove(k);
                    }
                    self.touch(p);
                    return EdAction::Redraw;
                }
                EdAction::None
            }
            Key::PageDown | Key::Down | Key::Right if n > 0 => {
                self.current = (self.current + 1).min(n - 1);
                self.scroll_to(r, self.current);
                EdAction::Redraw
            }
            Key::PageUp | Key::Up | Key::Left => {
                self.current = self.current.saturating_sub(1);
                self.scroll_to(r, self.current);
                EdAction::Redraw
            }
            Key::Home => {
                self.current = 0;
                self.scroll = 0;
                EdAction::Redraw
            }
            Key::End if n > 0 => {
                self.current = n - 1;
                self.scroll_to(r, n - 1);
                EdAction::Redraw
            }
            Key::Ctrl('s') => EdAction::Save,
            Key::Ctrl('p') => EdAction::Print,
            Key::Ctrl('z') => self.press_button(Btn::Undo, r),
            Key::Ctrl('=') => self.press_button(Btn::ZoomIn, r),
            Key::Ctrl('-') => self.press_button(Btn::ZoomOut, r),
            Key::Esc => {
                if self.selected.take().is_some() {
                    return EdAction::Redraw;
                }
                EdAction::Close
            }
            _ => EdAction::None,
        }
    }

    fn dirty(&self) -> bool {
        self.dirty
    }

    fn mark_saved(&mut self) {
        self.dirty = false;
    }

    fn encode(&mut self, _ext: &str) -> Result<Vec<u8>, String> {
        self.finish_text();
        if self.pages.is_empty() {
            return Err(String::from("there are no pages"));
        }
        let pages: Vec<edit::OutPage> = self
            .pages
            .iter()
            .map(|it| edit::OutPage {
                src: match it.src {
                    Src::Pdf(d, i) => PageSrc::Pdf { file: &self.docs[d].file, index: i },
                    Src::Photo(k) => PageSrc::Photo(&self.photos[k]),
                },
                rotate: it.rotate,
                overlays: &it.overlays,
            })
            .collect();
        let title = self.docs.first().and_then(|d| d.file.title());
        Ok(edit::save(&pages, title.as_deref()))
    }

    fn print_pages(&mut self, _paper: Paper) -> Vec<Canvas> {
        self.finish_text();
        (0..self.pages.len()).map(|i| self.render_page(i, 100.0 / 72.0)).collect()
    }

    fn picked(&mut self, purpose: u8, name: &str, bytes: Vec<u8>) -> EdAction {
        let at = if self.pages.is_empty() { 0 } else { self.current + 1 };
        match purpose {
            PICK_PDF => match PdfFile::open(bytes) {
                Ok(f) => {
                    self.snapshot();
                    let n = f.pages.len();
                    self.docs.push(Rc::new(PdfDoc::new(f)));
                    let d = self.docs.len() - 1;
                    for i in (0..n).rev() {
                        self.pages.insert(at, PageItem { src: Src::Pdf(d, i), rotate: 0, overlays: Vec::new(), ver: 0 });
                    }
                    EdAction::Toast(format!("Added {} page{} from \"{}\"", n, if n == 1 { "" } else { "s" }, name))
                }
                Err(e) => EdAction::Message(format!("Couldn't open \"{}\": {}", name, e)),
            },
            PICK_PHOTOS => match photo_from(&bytes) {
                Some(ph) => {
                    self.snapshot();
                    self.photos.push(ph);
                    let k = self.photos.len() - 1;
                    let pos = at.min(self.pages.len());
                    self.pages.insert(pos, PageItem { src: Src::Photo(k), rotate: 0, overlays: Vec::new(), ver: 0 });
                    self.current = pos;
                    EdAction::Redraw
                }
                None => EdAction::Message(format!("\"{}\" isn't a photo Solon can read.", name)),
            },
            _ => EdAction::None,
        }
    }
}

fn overlay_box(o: &Overlay) -> (f32, f32, f32, f32) {
    match o {
        Overlay::Text { x, y, size, text, .. } => {
            let lines = text.split('\n').count().max(1) as f32;
            let w = text.split('\n').map(|l| l.chars().count()).max().unwrap_or(1).max(1) as f32 * size * 0.55;
            (*x, y + size, x + w, y - size * 1.2 * (lines - 1.0) - size * 0.3)
        }
        Overlay::Highlight { x0, y0, x1, y1, .. } => (*x0, *y0, *x1, *y1),
        Overlay::Ink { strokes, .. } => {
            let pts: Vec<&(f32, f32)> = strokes.iter().flatten().collect();
            let x0 = pts.iter().map(|p| p.0).fold(f32::MAX, f32::min);
            let x1 = pts.iter().map(|p| p.0).fold(f32::MIN, f32::max);
            let y0 = pts.iter().map(|p| p.1).fold(f32::MAX, f32::min);
            let y1 = pts.iter().map(|p| p.1).fold(f32::MIN, f32::max);
            (x0, y0, x1, y1)
        }
    }
}

fn blit(c: &mut Canvas, src: &Canvas, x: i32, y: i32) {
    c.blit(src, x, y);
}

#[allow(dead_code)]
fn blit_slow(c: &mut Canvas, src: &Canvas, x: i32, y: i32) {
    for row in 0..src.h as i32 {
        let yy = y + row;
        for col in 0..src.w as i32 {
            c.blend(x + col, yy, src.px[(row * src.w as i32 + col) as usize], 255);
        }
    }
}

fn line(c: &mut Canvas, a: (f32, f32), b: (f32, f32), col: u32) {
    let n = ((b.0 - a.0).abs().max((b.1 - a.1).abs()) as i32).max(1);
    for k in 0..=n {
        let t = k as f32 / n as f32;
        let (x, y) = (a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t);
        c.fill_rect(x as i32 - 1, y as i32 - 1, 2, 2, col);
    }
}

//! Photos app: a scrollable thumbnail grid and a full-size viewer.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use crate::edit::Edits;
use crate::editor::{Editor, EditorAction, SaveChoice};
use crate::printui::{PrintAction, PrintDialog};
use crate::fs::DirEntry;
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::image::{self, Image};
use crate::interrupts::{HZ, TICKS};

const HEADER_H: i32 = 48;
const PAD: i32 = 20;
const THUMB_W: i32 = 168;
const THUMB_H: i32 = 120;
const CELL_W: i32 = 184;
const CELL_H: i32 = 164;
const WHEEL_STEP: i32 = 60;
const DOUBLE_CLICK_TICKS: u64 = 50;

const INK: u32 = rgb(40, 44, 52);
const INK_DIM: u32 = rgb(110, 116, 126);
const ACCENT: u32 = rgb(46, 134, 222);
const VIEW_BG: u32 = rgb(24, 26, 30);
const VIEW_BAR: u32 = rgb(38, 41, 48);
const VIEW_TEXT: u32 = rgb(230, 232, 236);

/// Where a photo's file bytes come from.
#[derive(Clone)]
pub enum Source {
    /// Loaded into memory by the bootloader.
    Memory(&'static [u8]),
    /// A file on a USB drive (read on demand).
    Usb { drive: u32, vol: usize, entry: DirEntry },
    /// A file in Solon's library on the internal disk (and the folder it's in).
    Library { dir: DirEntry, entry: DirEntry },
}

/// Something the Photos app needs the desktop to do (it owns storage).
pub enum PhotoAction {
    /// Open the editor on this photo (the desktop checks what saving is possible).
    Edit(usize),
    Save(usize, SaveChoice, Edits),
    Revert(usize),
    /// Open the print dialog (the desktop supplies the printer list).
    Print(usize),
    /// Print these photos.
    DoPrint { indices: Vec<usize>, layout: crate::print::Layout, printer: crate::print::PrinterInfo },
}

/// File bytes, either borrowed from memory or freshly read.
pub enum Bytes {
    Static(&'static [u8]),
    Owned(Vec<u8>),
}

impl core::ops::Deref for Bytes {
    type Target = [u8];
    fn deref(&self) -> &[u8] {
        match self {
            Bytes::Static(b) => b,
            Bytes::Owned(v) => v,
        }
    }
}

enum State {
    Pending,
    /// Screen-sized copy and a thumbnail.
    Ready { image: Image, thumb: Image },
    Failed(&'static str),
}

pub struct Photo {
    pub name: String,
    /// Original pixel size of the file.
    pub src_w: usize,
    pub src_h: usize,
    source: Source,
    state: State,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Mode {
    Grid,
    View(usize),
}

pub struct Photos {
    photos: Vec<Photo>,
    mode: Mode,
    selected: Option<usize>,
    scroll: i32,
    /// The viewed photo scaled to the current view area: (index, w, h, image).
    view_cache: Option<(usize, i32, i32, Image)>,
    last_click: Option<(usize, u64)>,
    /// Where these photos came from, e.g. "VACATION / DCIM".
    origin: String,
    max_w: usize,
    max_h: usize,
    /// The editor, when a photo is being edited (drawn over the viewer).
    editor: Option<Editor>,
    print: Option<PrintDialog>,
    pending: Option<PhotoAction>,
}

fn ticks() -> u64 {
    TICKS.load(Relaxed)
}

fn shrink_owned(img: Image, max_w: usize, max_h: usize) -> Image {
    if img.w <= max_w && img.h <= max_h {
        img
    } else {
        img.shrink_to_fit(max_w, max_h)
    }
}

/// Keep the end of a long path: "...DCIM / 100CANON".
pub fn ellipsize_left(s: &str, max_px: i32, size: Size, bold: bool) -> String {
    if text_width(s, size, bold) <= max_px {
        return String::from(s);
    }
    let chars: Vec<char> = s.chars().collect();
    let mut start = 0;
    while start < chars.len() {
        let tail: String = chars[start..].iter().collect();
        if text_width(&tail, size, bold) + text_width("...", size, bold) <= max_px {
            return String::from("...") + &tail;
        }
        start += 1;
    }
    String::from("...")
}

/// Cut a name down to `max_px` wide, ending in "..." if shortened.
pub fn ellipsize(s: &str, max_px: i32, size: Size, bold: bool) -> String {
    if text_width(s, size, bold) <= max_px {
        return String::from(s);
    }
    let mut out = String::new();
    for ch in s.chars() {
        out.push(ch);
        if text_width(&out, size, bold) + text_width("...", size, bold) > max_px {
            out.pop();
            break;
        }
    }
    out + "..."
}

impl Photos {
    /// An empty library. Photos are kept at no more than `max_w` x `max_h` (the screen size).
    pub fn new(max_w: usize, max_h: usize) -> Self {
        Self {
            photos: Vec::new(),
            mode: Mode::Grid,
            selected: None,
            scroll: 0,
            view_cache: None,
            last_click: None,
            origin: String::new(),
            max_w,
            max_h,
            editor: None,
            print: None,
            pending: None,
        }
    }

    /// Replace the shown photos. They're decoded later, one per `load_next` call.
    pub fn set(&mut self, origin: String, items: Vec<(String, Source)>, open_at: Option<usize>) {
        self.editor = None;
        self.photos = items
            .into_iter()
            .map(|(name, source)| Photo { name, src_w: 0, src_h: 0, source, state: State::Pending })
            .collect();
        self.origin = origin;
        self.scroll = 0;
        self.view_cache = None;
        self.last_click = None;
        self.selected = open_at;
        self.mode = match open_at {
            Some(i) if i < self.photos.len() => Mode::View(i),
            _ => Mode::Grid,
        };
        kprintln!("solon: photos from {} ({} files)", self.origin, self.photos.len());
    }

    pub fn origin(&self) -> &str {
        &self.origin
    }

    // ---- editing support (driven by the desktop) ---------------------------------------

    pub fn take_action(&mut self) -> Option<PhotoAction> {
        self.pending.take()
    }

    pub fn source(&self, i: usize) -> Option<&Source> {
        self.photos.get(i).map(|p| &p.source)
    }

    pub fn name(&self, i: usize) -> Option<&str> {
        self.photos.get(i).map(|p| p.name.as_str())
    }

    /// Open the editor on photo `i` (it must be loaded).
    pub fn start_edit(&mut self, i: usize, can_save: bool, can_replace: bool, has_original: bool) -> bool {
        let Some(p) = self.photos.get(i) else { return false };
        let State::Ready { image, .. } = &p.state else { return false };
        kprintln!("editor: open {}", p.name);
        self.editor = Some(Editor::new(i, p.name.clone(), image, p.src_w, p.src_h, can_save, can_replace, has_original));
        self.mode = Mode::View(i);
        true
    }

    pub fn end_edit(&mut self) {
        if self.editor.take().is_some() {
            kprintln!("editor: closed");
        }
    }

    /// A saved copy: add it right after the original and show it.
    pub fn insert_after(&mut self, i: usize, name: String, source: Source) {
        let at = (i + 1).min(self.photos.len());
        self.photos.insert(at, Photo { name, src_w: 0, src_h: 0, source, state: State::Pending });
        self.view_cache = None;
        self.mode = Mode::View(at);
        self.selected = Some(at);
    }

    /// The photo at `i` was replaced (or reverted): point it at the new file and reload it.
    pub fn replace_item(&mut self, i: usize, name: String, source: Source) {
        if let Some(p) = self.photos.get_mut(i) {
            *p = Photo { name, src_w: 0, src_h: 0, source, state: State::Pending };
            self.view_cache = None;
            self.mode = Mode::View(i);
        }
    }

    /// Mouse moved with the left button held (editor sliders and crop box).
    pub fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        self.editor.as_mut().map(|e| e.on_drag(x, y, r)).unwrap_or(false)
    }

    pub fn on_release(&mut self) -> bool {
        self.editor.as_mut().map(|e| e.on_release()).unwrap_or(false)
    }

    fn editor_action(&mut self, a: EditorAction) -> bool {
        match a {
            EditorAction::None => false,
            EditorAction::Redraw => true,
            EditorAction::Cancel => {
                self.end_edit();
                true
            }
            EditorAction::Save(choice, edits) => {
                if let Some(e) = &self.editor {
                    self.pending = Some(PhotoAction::Save(e.index, choice, edits));
                }
                true
            }
            EditorAction::Revert => {
                if let Some(e) = &self.editor {
                    self.pending = Some(PhotoAction::Revert(e.index));
                }
                true
            }
        }
    }

    fn edit_button(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 12 - 90, r.y + 8, 90, 32)
    }

    pub fn needs_loading(&self) -> bool {
        self.photos.iter().any(|p| matches!(p.state, State::Pending))
    }

    /// Decode the next pending photo (the one being viewed first). Returns true if one was processed.
    pub fn load_next(&mut self, fetch: &mut dyn FnMut(&Source) -> Result<Bytes, &'static str>) -> bool {
        let viewing = match self.mode {
            Mode::View(i) if matches!(self.photos[i].state, State::Pending) => Some(i),
            _ => None,
        };
        let Some(i) = viewing.or_else(|| self.photos.iter().position(|p| matches!(p.state, State::Pending))) else {
            return false;
        };
        let t0 = ticks();
        let (max_w, max_h) = (self.max_w, self.max_h);
        let p = &mut self.photos[i];
        p.state = match fetch(&p.source).and_then(|bytes| image::decode(&bytes)) {
            Ok(full) => {
                p.src_w = full.w;
                p.src_h = full.h;
                let image = shrink_owned(full, max_w, max_h);
                let thumb = image.shrink_to_fit(THUMB_W as usize, THUMB_H as usize);
                kprintln!("solon: photo {} {}x{} loaded in {} ms", p.name, p.src_w, p.src_h, (ticks() - t0) * 1000 / HZ);
                State::Ready { image, thumb }
            }
            Err(e) => {
                kprintln!("solon: photo {} failed: {}", p.name, e);
                State::Failed(e)
            }
        };
        if !self.needs_loading() {
            kprintln!("solon: all photos loaded");
        }
        true
    }

    /// A drive went away: photos from it that weren't loaded yet can't be anymore.
    pub fn drive_removed(&mut self, gone: &dyn Fn(u32) -> bool) {
        for p in &mut self.photos {
            if let (State::Pending, Source::Usb { drive, .. }) = (&p.state, &p.source) {
                if gone(*drive) {
                    p.state = State::Failed("The drive was removed");
                }
            }
        }
    }

    fn loaded(&self) -> usize {
        self.photos.iter().filter(|p| !matches!(p.state, State::Pending)).count()
    }

    // ---- layout -----------------------------------------------------------

    fn cols(r: Rect) -> i32 {
        ((r.w - 2 * PAD) / CELL_W).max(1)
    }

    fn grid_area(r: Rect) -> Rect {
        Rect::new(r.x, r.y + HEADER_H, r.w, r.h - HEADER_H)
    }

    fn cell_rect(&self, i: usize, r: Rect) -> Rect {
        let cols = Self::cols(r);
        let left = r.x + (r.w - cols * CELL_W) / 2;
        let (col, row) = (i as i32 % cols, i as i32 / cols);
        Rect::new(left + col * CELL_W, r.y + HEADER_H + 8 + row * CELL_H - self.scroll, CELL_W, CELL_H)
    }

    fn max_scroll(&self, r: Rect) -> i32 {
        let rows = (self.photos.len() as i32 + Self::cols(r) - 1) / Self::cols(r);
        (rows * CELL_H + 16 - Self::grid_area(r).h).max(0)
    }

    fn scroll_to_selected(&mut self, r: Rect) {
        if let Some(i) = self.selected {
            let cell = self.cell_rect(i, r);
            let area = Self::grid_area(r);
            if cell.y < area.y {
                self.scroll -= area.y - cell.y + 8;
            } else if cell.y + cell.h > area.y + area.h {
                self.scroll += cell.y + cell.h - (area.y + area.h) + 8;
            }
        }
        self.scroll = self.scroll.clamp(0, self.max_scroll(r));
    }

    fn back_button(r: Rect) -> Rect {
        Rect::new(r.x + 12, r.y + 8, 144, 32)
    }

    fn image_area(r: Rect) -> Rect {
        Rect::new(r.x + PAD, r.y + HEADER_H + PAD, r.w - 2 * PAD, r.h - HEADER_H - 2 * PAD)
    }

    /// Hit areas for the previous / next arrows in the viewer.
    fn arrow(r: Rect, next: bool) -> Rect {
        let ia = Self::image_area(r);
        let cx = if next { ia.x + ia.w - 36 } else { ia.x + 36 };
        Rect::new(cx - 28, ia.y + ia.h / 2 - 28, 56, 56)
    }

    // ---- drawing ----------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, r: Rect) {
        if let Some(e) = &mut self.editor {
            e.draw(c, r);
            return;
        }
        match self.mode {
            Mode::Grid => self.draw_grid(c, r),
            Mode::View(i) => self.draw_view(c, r, i),
        }
        if let Some(mut d) = self.print.take() {
            let photos = &self.photos;
            d.draw(c, r, &|i| match photos.get(i).map(|p| &p.state) {
                Some(State::Ready { thumb, .. }) => Some(Image { w: thumb.w, h: thumb.h, px: thumb.px.clone(), opaque: thumb.opaque }),
                _ => None,
            });
            self.print = Some(d);
        }
    }

    /// Open the print dialog on photo `i` with these printers.
    pub fn start_print(&mut self, i: usize, printers: alloc::vec::Vec<crate::print::PrinterInfo>) {
        self.print = Some(PrintDialog::new(i.min(self.photos.len().saturating_sub(1)), self.photos.len(), printers));
    }

    fn print_button(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 12 - 90 - 10 - 90, r.y + 8, 90, 32)
    }

    fn grid_print_button(r: Rect) -> Rect {
        Rect::new(r.x + r.w - PAD - 90, r.y + 8, 90, 32)
    }

    fn print_action(&mut self, a: PrintAction) -> bool {
        match a {
            PrintAction::None => false,
            PrintAction::Redraw => true,
            PrintAction::Cancel => {
                self.print = None;
                true
            }
            PrintAction::Print { indices, layout, printer } => {
                self.print = None;
                self.pending = Some(PhotoAction::DoPrint { indices, layout, printer });
                true
            }
        }
    }

    fn draw_grid(&self, c: &mut Canvas, r: Rect) {
        if self.photos.is_empty() {
            c.text_centered(r.x + r.w / 2, r.y + r.h / 2 - 30, "No photos yet.", Size::Medium, true, INK);
            c.text_centered(
                r.x + r.w / 2,
                r.y + r.h / 2,
                "Plug in a USB drive or SD card, then open Import.",
                Size::Small,
                false,
                INK_DIM,
            );
            return;
        }

        let n = self.photos.len();
        let title = if n == 1 { String::from("1 photo") } else { format!("{} photos", n) };
        let tw = c.text(r.x + PAD, r.y + 14, &title, Size::Medium, true, INK);
        let loaded = self.loaded();
        let sub = if loaded < n {
            format!("{}   |   loading {} of {}...", self.origin, loaded + 1, n)
        } else {
            self.origin.clone()
        };
        let sub = ellipsize(&sub, r.w / 2 - tw - 2 * PAD, Size::Small, false);
        c.text(r.x + PAD + tw + 16, r.y + 17, &sub, Size::Small, false, INK_DIM);
        let pb = Self::grid_print_button(r);
        c.round_rect(pb.x, pb.y, pb.w, pb.h, 8, rgb(226, 230, 236));
        c.text_centered(pb.x + pb.w / 2, pb.y + 7, "Print", Size::Small, true, INK);
        let hint = "Double-click a photo to view it";
        c.text(pb.x - 16 - text_width(hint, Size::Small, false), r.y + 17, hint, Size::Small, false, INK_DIM);
        c.fill_rect(r.x, r.y + HEADER_H - 1, r.w, 1, rgb(228, 231, 236));

        let area = Self::grid_area(r);
        c.set_clip(area);
        for (i, p) in self.photos.iter().enumerate() {
            let cell = self.cell_rect(i, r);
            if cell.y + cell.h < area.y || cell.y > area.y + area.h {
                continue;
            }
            if self.selected == Some(i) {
                c.round_rect_alpha(cell.x + 4, cell.y, cell.w - 8, cell.h - 4, 10, ACCENT, 45);
            }
            let (bx, by) = (cell.x + (CELL_W - THUMB_W) / 2, cell.y + 8);
            match &p.state {
                State::Ready { thumb: t, .. } => {
                    let (tx, ty) = (bx + (THUMB_W - t.w as i32) / 2, by + (THUMB_H - t.h as i32) / 2);
                    c.fill_rect_alpha(tx + 2, ty + 3, t.w as i32, t.h as i32, 0, 50); // shadow
                    if !t.opaque {
                        c.fill_rect(tx, ty, t.w as i32, t.h as i32, rgb(236, 238, 242));
                    }
                    c.draw_image(t, tx, ty);
                }
                State::Pending => {
                    c.round_rect(bx, by, THUMB_W, THUMB_H, 8, rgb(240, 242, 245));
                    c.text_centered(bx + THUMB_W / 2, by + THUMB_H / 2 - 8, "Loading...", Size::Small, false, INK_DIM);
                }
                State::Failed(e) => {
                    c.round_rect(bx, by, THUMB_W, THUMB_H, 8, rgb(236, 238, 242));
                    c.text_centered(bx + THUMB_W / 2, by + THUMB_H / 2 - 18, "Can't open", Size::Small, true, INK_DIM);
                    let msg = ellipsize(e, THUMB_W - 12, Size::Small, false);
                    c.text_centered(bx + THUMB_W / 2, by + THUMB_H / 2 + 2, &msg, Size::Small, false, INK_DIM);
                }
            }
            let label = ellipsize(&p.name, CELL_W - 16, Size::Small, false);
            c.text_centered(cell.x + CELL_W / 2, cell.y + THUMB_H + 16, &label, Size::Small, false, INK);
        }

        // Scrollbar
        let max = self.max_scroll(r);
        if max > 0 {
            let track = area.h - 16;
            let thumb_h = (track * area.h / (area.h + max)).max(30);
            let y = area.y + 8 + (track - thumb_h) * self.scroll / max;
            c.round_rect_alpha(r.x + r.w - 10, y, 6, thumb_h, 3, 0, 70);
        }
        c.set_clip(r);
    }

    fn draw_view(&mut self, c: &mut Canvas, r: Rect, i: usize) {
        c.fill_rect(r.x, r.y, r.w, r.h, VIEW_BG);
        c.fill_rect(r.x, r.y, r.w, HEADER_H, VIEW_BAR);

        // Back button
        let b = Self::back_button(r);
        c.round_rect(b.x, b.y, b.w, b.h, 8, rgb(60, 64, 74));
        c.triangle([(b.x + 14, b.y + 16), (b.x + 22, b.y + 10), (b.x + 22, b.y + 22)], VIEW_TEXT);
        c.text(b.x + 30, b.y + 7, "All photos", Size::Small, true, VIEW_TEXT);

        // Title: name, position, size
        let p = &self.photos[i];
        let info = if p.src_w > 0 {
            format!("{}   |   {} of {}   |   {} x {}", p.name, i + 1, self.photos.len(), p.src_w, p.src_h)
        } else {
            format!("{}   |   {} of {}", p.name, i + 1, self.photos.len())
        };
        let info = ellipsize(&info, r.w - 2 * (b.w + 24), Size::Small, false);
        c.text_centered(r.x + r.w / 2, r.y + 15, &info, Size::Small, false, VIEW_TEXT);
        if matches!(self.photos[i].state, State::Ready { .. }) {
            let eb = Self::edit_button(r);
            c.round_rect(eb.x, eb.y, eb.w, eb.h, 8, ACCENT);
            c.text_centered(eb.x + eb.w / 2, eb.y + 7, "Edit", Size::Small, true, rgb(255, 255, 255));
            let pb = Self::print_button(r);
            c.round_rect(pb.x, pb.y, pb.w, pb.h, 8, rgb(60, 64, 74));
            c.text_centered(pb.x + pb.w / 2, pb.y + 7, "Print", Size::Small, true, VIEW_TEXT);
        }

        let ia = Self::image_area(r);
        match &self.photos[i].state {
            State::Pending => {
                c.text_centered(ia.x + ia.w / 2, ia.y + ia.h / 2 - 10, "Loading...", Size::Medium, true, VIEW_TEXT);
            }
            State::Ready { image: img, .. } => {
                let stale = !matches!(&self.view_cache, Some((ci, w, h, _)) if *ci == i && *w == ia.w && *h == ia.h);
                if stale {
                    let scaled = img.shrink_to_fit(ia.w.max(1) as usize, ia.h.max(1) as usize);
                    self.view_cache = Some((i, ia.w, ia.h, scaled));
                }
                if let Some((_, _, _, s)) = &self.view_cache {
                    let (x, y) = (ia.x + (ia.w - s.w as i32) / 2, ia.y + (ia.h - s.h as i32) / 2);
                    if !s.opaque {
                        // Light checkerboard behind transparent images
                        for ty in (0..s.h as i32).step_by(16) {
                            for tx in (0..s.w as i32).step_by(16) {
                                let shade = if (tx / 16 + ty / 16) % 2 == 0 { rgb(200, 200, 204) } else { rgb(160, 160, 166) };
                                c.fill_rect(x + tx, y + ty, 16.min(s.w as i32 - tx), 16.min(s.h as i32 - ty), shade);
                            }
                        }
                    }
                    c.draw_image(s, x, y);
                }
            }
            State::Failed(e) => {
                c.text_centered(ia.x + ia.w / 2, ia.y + ia.h / 2 - 24, "Can't open this file", Size::Medium, true, VIEW_TEXT);
                c.text_centered(ia.x + ia.w / 2, ia.y + ia.h / 2 + 8, e, Size::Small, false, rgb(170, 176, 186));
            }
        }

        // Prev / next arrows
        for next in [false, true] {
            let show = if next { i + 1 < self.photos.len() } else { i > 0 };
            if !show {
                continue;
            }
            let a = Self::arrow(r, next);
            let (cx, cy) = (a.x + a.w / 2, a.y + a.h / 2);
            c.circle_alpha(cx, cy, 24, 0, 140);
            let pts = if next {
                [(cx - 5, cy - 10), (cx - 5, cy + 10), (cx + 9, cy)]
            } else {
                [(cx + 5, cy - 10), (cx + 5, cy + 10), (cx - 9, cy)]
            };
            c.triangle(pts, rgb(255, 255, 255));
        }
    }

    // ---- input ------------------------------------------------------------

    fn open_view(&mut self, i: usize) {
        self.mode = Mode::View(i);
        self.selected = Some(i);
        kprintln!("solon: view photo {}", self.photos[i].name);
    }

    fn back_to_grid(&mut self, r: Rect) {
        self.mode = Mode::Grid;
        self.view_cache = None;
        self.scroll_to_selected(r);
        kprintln!("solon: photo grid");
    }

    fn step(&mut self, delta: i32) -> bool {
        if let Mode::View(i) = self.mode {
            let j = i as i32 + delta;
            if j >= 0 && (j as usize) < self.photos.len() {
                self.open_view(j as usize);
                return true;
            }
        }
        false
    }

    /// Left button pressed at (x, y) inside the app area `r`. Returns true if a redraw is needed.
    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64) -> bool {
        if let Some(e) = &mut self.editor {
            let a = e.on_press(x, y, r);
            return self.editor_action(a);
        }
        if let Some(d) = &mut self.print {
            let a = d.on_press(x, y, r);
            return self.print_action(a);
        }
        if !self.photos.is_empty() && match self.mode {
            Mode::View(i) => Self::print_button(r).contains(x, y) && matches!(self.photos[i].state, State::Ready { .. }),
            Mode::Grid => Self::grid_print_button(r).contains(x, y),
        } {
            let i = match self.mode {
                Mode::View(i) => i,
                Mode::Grid => self.selected.unwrap_or(0),
            };
            self.pending = Some(PhotoAction::Print(i));
            return true;
        }
        match self.mode {
            Mode::View(i) => {
                if Self::edit_button(r).contains(x, y) && matches!(self.photos[i].state, State::Ready { .. }) {
                    self.pending = Some(PhotoAction::Edit(i));
                    return true;
                }
                if Self::back_button(r).contains(x, y) {
                    self.back_to_grid(r);
                    return true;
                }
                if Self::arrow(r, false).contains(x, y) {
                    return self.step(-1);
                }
                if Self::arrow(r, true).contains(x, y) {
                    return self.step(1);
                }
                false
            }
            Mode::Grid => {
                if !Self::grid_area(r).contains(x, y) {
                    return false;
                }
                let hit = (0..self.photos.len()).find(|&i| self.cell_rect(i, r).contains(x, y));
                match hit {
                    Some(i) => {
                        let double = matches!(self.last_click, Some((j, t)) if j == i && now - t <= DOUBLE_CLICK_TICKS);
                        self.selected = Some(i);
                        self.last_click = Some((i, now));
                        if double {
                            self.last_click = None;
                            self.open_view(i);
                        }
                        true
                    }
                    None => {
                        self.last_click = None;
                        self.selected.take().is_some()
                    }
                }
            }
        }
    }

    pub fn on_wheel(&mut self, delta: i8, r: Rect) -> bool {
        if self.editor.is_some() || self.print.is_some() {
            return false;
        }
        match self.mode {
            Mode::Grid => {
                let old = self.scroll;
                self.scroll = (self.scroll + delta as i32 * WHEEL_STEP).clamp(0, self.max_scroll(r));
                self.scroll != old
            }
            // In the viewer the wheel flips through photos.
            Mode::View(_) => self.step(if delta > 0 { 1 } else { -1 }),
        }
    }

    /// Scancode (set 1). None = not handled here (let the desktop deal with it),
    /// Some(redraw) = handled.
    pub fn on_key(&mut self, sc: u8, r: Rect) -> Option<bool> {
        if let Some(e) = &mut self.editor {
            let a = e.on_key(sc);
            return Some(self.editor_action(a));
        }
        if let Some(d) = &mut self.print {
            if sc & 0x80 != 0 {
                return Some(false);
            }
            let a = d.on_key(sc);
            return Some(self.print_action(a));
        }
        if sc & 0x80 != 0 {
            return Some(false); // key release
        }
        match self.mode {
            Mode::View(i) => Some(match sc {
                0x19 => {
                    // P: print
                    self.pending = Some(PhotoAction::Print(i));
                    true
                }
                0x12 if matches!(self.photos[i].state, State::Ready { .. }) => {
                    // E: edit
                    self.pending = Some(PhotoAction::Edit(i));
                    true
                }
                0x01 | 0x0E => {
                    // Esc / Backspace
                    self.back_to_grid(r);
                    true
                }
                0x4B | 0x49 => self.step(-1), // Left / PageUp
                0x4D | 0x51 | 0x39 => self.step(1), // Right / PageDown / Space
                _ => false,
            }),
            Mode::Grid => {
                let n = self.photos.len() as i32;
                if n == 0 {
                    return None;
                }
                let cols = Self::cols(r);
                let delta = match sc {
                    0x19 => {
                        self.pending = Some(PhotoAction::Print(self.selected.unwrap_or(0)));
                        return Some(true);
                    }
                    0x4B => -1,
                    0x4D => 1,
                    0x48 => -cols,
                    0x50 => cols,
                    0x1C => {
                        // Enter
                        if let Some(i) = self.selected {
                            self.open_view(i);
                            return Some(true);
                        }
                        return Some(false);
                    }
                    _ => return None,
                };
                let cur = self.selected.map(|i| i as i32);
                let next = match cur {
                    None => 0,
                    Some(i) => (i + delta).clamp(0, n - 1),
                };
                self.selected = Some(next as usize);
                self.scroll_to_selected(r);
                Some(true)
            }
        }
    }
}

//! The photo editor screen (inside the Photos window).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::edit::{self, Crop, Edits, Filter, FULL};
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::image::Image;

const HEADER_H: i32 = 48;
const PANEL_W: i32 = 300;
const TAB_H: i32 = 40;

const BG: u32 = rgb(24, 26, 30);
const BAR: u32 = rgb(38, 41, 48);
const PANEL: u32 = rgb(32, 35, 41);
const TEXT: u32 = rgb(230, 232, 236);
const TEXT_DIM: u32 = rgb(150, 156, 166);
const BTN: u32 = rgb(60, 64, 74);
const BTN_ON: u32 = rgb(46, 134, 222);
const DISABLED: u32 = rgb(44, 47, 54);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Crop,
    Adjust,
    Filters,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Aspect {
    Free,
    Original,
    Square,
    R4x6,
    R5x7,
    R8x10,
}

impl Aspect {
    const ALL: [Aspect; 6] = [Aspect::Free, Aspect::Original, Aspect::Square, Aspect::R4x6, Aspect::R5x7, Aspect::R8x10];
    fn name(self) -> &'static str {
        match self {
            Aspect::Free => "Free",
            Aspect::Original => "Original",
            Aspect::Square => "Square",
            Aspect::R4x6 => "4 x 6 print",
            Aspect::R5x7 => "5 x 7 print",
            Aspect::R8x10 => "8 x 10 print",
        }
    }
    /// Long side : short side (x1000), or None for free.
    fn ratio(self) -> Option<u32> {
        match self {
            Aspect::Free | Aspect::Original => None,
            Aspect::Square => Some(1000),
            Aspect::R4x6 => Some(1500),
            Aspect::R5x7 => Some(1400),
            Aspect::R8x10 => Some(1250),
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SaveChoice {
    Copy,
    Replace,
}

pub enum EditorAction {
    None,
    Redraw,
    Cancel,
    Save(SaveChoice, Edits),
    Revert,
}

#[derive(Clone, Copy)]
enum Drag {
    Slider(usize),
    CropMove { mx: i32, my: i32, start: Crop },
    CropCorner(u8),
}

pub struct Editor {
    pub index: usize,
    pub name: String,
    /// The unedited photo, shrunk to fit any orientation of the preview area.
    small: Image,
    /// Original full size, for keeping crop shapes exact.
    src_w: usize,
    src_h: usize,
    pub edits: Edits,
    history: Vec<Edits>,
    tool: Tool,
    aspect: Aspect,
    drag: Option<Drag>,
    /// Edits in effect when the current drag started (one undo step per drag).
    drag_start: Option<Edits>,
    preview: Option<(Edits, bool, i32, i32, Image)>,
    thumbs: Vec<Image>,
    dialog: bool,
    can_replace: bool,
    can_save: bool,
    pub has_original: bool,
    ctrl: bool,
}

fn button(c: &mut Canvas, b: Rect, label: &str, color: u32, text: u32) {
    c.round_rect(b.x, b.y, b.w, b.h, 8, color);
    c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, text);
}

impl Editor {
    /// `photo` is the screen-sized copy; (src_w, src_h) the original size.
    pub fn new(index: usize, name: String, photo: &Image, src_w: usize, src_h: usize, can_save: bool, can_replace: bool, has_original: bool) -> Editor {
        let side = 1100;
        let small = photo.shrink_to_fit(side, side);
        Editor {
            index,
            name,
            small,
            src_w: src_w.max(1),
            src_h: src_h.max(1),
            edits: Edits::default(),
            history: Vec::new(),
            tool: Tool::Crop,
            aspect: Aspect::Free,
            drag: None,
            drag_start: None,
            preview: None,
            thumbs: Vec::new(),
            dialog: false,
            can_replace,
            can_save,
            has_original,
            ctrl: false,
        }
    }

    // ---- layout ----------------------------------------------------------------

    fn cancel_btn(r: Rect) -> Rect {
        Rect::new(r.x + 12, r.y + 8, 96, 32)
    }
    fn save_btn(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 12 - 100, r.y + 8, 100, 32)
    }
    fn reset_btn(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 12 - 100 - 10 - 84, r.y + 8, 84, 32)
    }
    fn undo_btn(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 12 - 100 - 10 - 84 - 10 - 76, r.y + 8, 76, 32)
    }
    fn panel(r: Rect) -> Rect {
        Rect::new(r.x + r.w - PANEL_W, r.y + HEADER_H, PANEL_W, r.h - HEADER_H)
    }
    fn image_area(r: Rect) -> Rect {
        Rect::new(r.x + 20, r.y + HEADER_H + 20, r.w - PANEL_W - 40, r.h - HEADER_H - 40)
    }
    fn tab(r: Rect, i: i32) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + i * (PANEL_W / 3), p.y, PANEL_W / 3, TAB_H)
    }
    fn content_y(r: Rect) -> i32 {
        Self::panel(r).y + TAB_H + 16
    }
    // Crop tab
    fn rotate_btn(r: Rect, i: i32) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + 16 + i * 92, Self::content_y(r) + 26, 84, 34)
    }
    fn aspect_btn(r: Rect, i: i32) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + 16 + (i % 2) * 138, Self::content_y(r) + 110 + (i / 2) * 42, 130, 34)
    }
    // Adjust tab
    fn auto_btn(r: Rect) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + 16, Self::content_y(r), PANEL_W - 32, 36)
    }
    fn slider_track(r: Rect, i: usize) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + 24, Self::content_y(r) + 84 + i as i32 * 64, PANEL_W - 48, 6)
    }
    // Filters tab
    fn filter_cell(r: Rect, i: usize) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + 16 + (i as i32 % 2) * 138, Self::content_y(r) + (i as i32 / 2) * 112, 130, 104)
    }
    fn revert_btn(r: Rect) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + 16, p.y + p.h - 52, PANEL_W - 32, 36)
    }
    fn dialog(r: Rect) -> Rect {
        let w = 520.min(r.w - 40);
        Rect::new(r.x + (r.w - w) / 2, r.y + r.h / 2 - 120, w, 240)
    }
    fn dlg_copy(r: Rect) -> Rect {
        let d = Self::dialog(r);
        Rect::new(d.x + 24, d.y + 108, d.w - 48, 40)
    }
    fn dlg_replace(r: Rect) -> Rect {
        let d = Self::dialog(r);
        Rect::new(d.x + 24, d.y + 156, (d.w - 48) * 2 / 3 - 6, 40)
    }
    fn dlg_cancel(r: Rect) -> Rect {
        let d = Self::dialog(r);
        let w = (d.w - 48) / 3 - 6;
        Rect::new(d.x + d.w - 24 - w, d.y + 156, w, 40)
    }

    // ---- helpers -----------------------------------------------------------------

    fn change(&mut self, f: impl FnOnce(&mut Edits)) {
        let before = self.edits;
        f(&mut self.edits);
        if self.edits != before && self.drag.is_none() {
            self.history.push(before);
        }
    }

    fn slider_value(&self, i: usize) -> i32 {
        match i {
            0 => self.edits.brightness,
            1 => self.edits.contrast,
            2 => self.edits.saturation,
            _ => self.edits.warmth,
        }
    }

    fn set_slider(&mut self, i: usize, v: i32) {
        let v = v.clamp(-100, 100);
        match i {
            0 => self.edits.brightness = v,
            1 => self.edits.contrast = v,
            2 => self.edits.saturation = v,
            _ => self.edits.warmth = v,
        }
    }

    /// Size of the photo after rotating (original pixels).
    fn turned(&self) -> (usize, usize) {
        edit::turned_size(self.src_w, self.src_h, self.edits.turns)
    }

    /// Largest centered crop of the chosen shape (same orientation as the photo).
    fn crop_for(&self, a: Aspect) -> Crop {
        let (w, h) = self.turned();
        let ratio = match a {
            Aspect::Free | Aspect::Original => return Crop::ALL,
            _ => a.ratio().unwrap() as u64,
        };
        // Target width/height in pixels, long side along the photo's long side.
        let (tw, th) = if w >= h { (ratio, 1000) } else { (1000, ratio) };
        let (cw, ch) = if (w as u64) * th >= (h as u64) * tw {
            ((h as u64 * tw / th) as usize, h)
        } else {
            (w, (w as u64 * th / tw) as usize)
        };
        let nw = (cw as u64 * FULL as u64 / w as u64) as u32;
        let nh = (ch as u64 * FULL as u64 / h as u64) as u32;
        Crop { x: (FULL - nw) / 2, y: (FULL - nh) / 2, w: nw, h: nh }
    }

    /// The preview image and where it's drawn. In the crop tool the picture is shown
    /// uncropped so the crop box can be moved around.
    fn preview(&mut self, r: Rect) -> (i32, i32, &Image) {
        let area = Self::image_area(r);
        let crop_mode = self.tool == Tool::Crop;
        let stale = !matches!(&self.preview, Some((e, m, w, h, _)) if *e == self.edits && *m == crop_mode && *w == area.w && *h == area.h);
        if stale {
            let mut e = self.edits;
            if crop_mode {
                e.crop = Crop::ALL;
            }
            let img = edit::apply(&self.small, &e).shrink_to_fit(area.w.max(1) as usize, area.h.max(1) as usize);
            self.preview = Some((self.edits, crop_mode, area.w, area.h, img));
        }
        let img = &self.preview.as_ref().unwrap().4;
        (area.x + (area.w - img.w as i32) / 2, area.y + (area.h - img.h as i32) / 2, img)
    }

    fn crop_screen(&mut self, r: Rect) -> Rect {
        let c = self.edits.crop;
        let (px, py, img) = self.preview(r);
        let (pw, ph) = (img.w as i32, img.h as i32);
        Rect::new(
            px + (c.x as i64 * pw as i64 / FULL as i64) as i32,
            py + (c.y as i64 * ph as i64 / FULL as i64) as i32,
            (c.w as i64 * pw as i64 / FULL as i64) as i32,
            (c.h as i64 * ph as i64 / FULL as i64) as i32,
        )
    }

    // ---- drawing --------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y, r.w, r.h, BG);
        c.fill_rect(r.x, r.y, r.w, HEADER_H, BAR);
        button(c, Self::cancel_btn(r), "Cancel", BTN, TEXT);
        let title = crate::photos::ellipsize(&format!("Editing {}", self.name), r.w - 2 * 380, Size::Small, true);
        c.text_centered(r.x + (r.w - 240) / 2, r.y + 15, &title, Size::Small, true, TEXT);
        let undo_ok = !self.history.is_empty();
        button(c, Self::undo_btn(r), "Undo", if undo_ok { BTN } else { DISABLED }, if undo_ok { TEXT } else { TEXT_DIM });
        let reset_ok = !self.edits.is_original();
        button(c, Self::reset_btn(r), "Reset", if reset_ok { BTN } else { DISABLED }, if reset_ok { TEXT } else { TEXT_DIM });
        button(c, Self::save_btn(r), "Save", BTN_ON, rgb(255, 255, 255));

        // Picture
        let crop_mode = self.tool == Tool::Crop;
        let (px, py, img) = self.preview(r);
        let (pw, ph) = (img.w as i32, img.h as i32);
        if !img.opaque {
            c.fill_rect(px, py, pw, ph, rgb(200, 200, 204));
        }
        // Borrow juggling: draw from a reference held only for this call.
        {
            let img = &self.preview.as_ref().unwrap().4;
            c.draw_image(img, px, py);
        }
        if crop_mode {
            let cr = self.crop_screen(r);
            // Dim everything outside the crop box.
            c.fill_rect_alpha(px, py, pw, cr.y - py, 0, 140);
            c.fill_rect_alpha(px, cr.y + cr.h, pw, py + ph - cr.y - cr.h, 0, 140);
            c.fill_rect_alpha(px, cr.y, cr.x - px, cr.h, 0, 140);
            c.fill_rect_alpha(cr.x + cr.w, cr.y, px + pw - cr.x - cr.w, cr.h, 0, 140);
            // Box + rule-of-thirds lines + corner handles
            for k in 1..3 {
                c.fill_rect_alpha(cr.x + cr.w * k / 3, cr.y, 1, cr.h, rgb(255, 255, 255), 90);
                c.fill_rect_alpha(cr.x, cr.y + cr.h * k / 3, cr.w, 1, rgb(255, 255, 255), 90);
            }
            let white = rgb(255, 255, 255);
            c.fill_rect(cr.x, cr.y, cr.w, 2, white);
            c.fill_rect(cr.x, cr.y + cr.h - 2, cr.w, 2, white);
            c.fill_rect(cr.x, cr.y, 2, cr.h, white);
            c.fill_rect(cr.x + cr.w - 2, cr.y, 2, cr.h, white);
            for (hx, hy) in [(cr.x, cr.y), (cr.x + cr.w, cr.y), (cr.x, cr.y + cr.h), (cr.x + cr.w, cr.y + cr.h)] {
                c.fill_rect(hx - 7, hy - 7, 14, 14, white);
                c.fill_rect(hx - 5, hy - 5, 10, 10, BTN_ON);
            }
        }

        // Panel
        let p = Self::panel(r);
        c.fill_rect(p.x, p.y, p.w, p.h, PANEL);
        for (i, (t, name)) in [(Tool::Crop, "Crop"), (Tool::Adjust, "Adjust"), (Tool::Filters, "Filters")].iter().enumerate() {
            let tr = Self::tab(r, i as i32);
            let on = self.tool == *t;
            if on {
                c.fill_rect(tr.x, tr.y + tr.h - 3, tr.w, 3, BTN_ON);
            }
            c.text_centered(tr.x + tr.w / 2, tr.y + 11, name, Size::Small, true, if on { TEXT } else { TEXT_DIM });
        }
        c.fill_rect(p.x, p.y + TAB_H, p.w, 1, rgb(50, 54, 62));
        let y0 = Self::content_y(r);
        match self.tool {
            Tool::Crop => {
                c.text(p.x + 16, y0, "Rotate", Size::Small, true, TEXT_DIM);
                for (i, l) in ["Left", "Right", "Mirror"].iter().enumerate() {
                    button(c, Self::rotate_btn(r, i as i32), l, BTN, TEXT);
                }
                c.text(p.x + 16, y0 + 84, "Shape", Size::Small, true, TEXT_DIM);
                for (i, a) in Aspect::ALL.iter().enumerate() {
                    let on = self.aspect == *a;
                    button(c, Self::aspect_btn(r, i as i32), a.name(), if on { BTN_ON } else { BTN }, TEXT);
                }
                c.text(p.x + 16, y0 + 246, "Drag the corners to crop,", Size::Small, false, TEXT_DIM);
                c.text(p.x + 16, y0 + 266, "or drag inside to move.", Size::Small, false, TEXT_DIM);
            }
            Tool::Adjust => {
                button(c, Self::auto_btn(r), "Auto-fix", BTN_ON, rgb(255, 255, 255));
                for (i, name) in ["Brightness", "Contrast", "Saturation", "Warmth"].iter().enumerate() {
                    let t = Self::slider_track(r, i);
                    let v = self.slider_value(i);
                    c.text(t.x, t.y - 26, name, Size::Small, true, TEXT);
                    let vs = format!("{}", v);
                    c.text(t.x + t.w - text_width(&vs, Size::Small, false), t.y - 26, &vs, Size::Small, false, TEXT_DIM);
                    c.round_rect(t.x, t.y, t.w, t.h, 3, rgb(70, 75, 86));
                    let mid = t.x + t.w / 2;
                    let kx = t.x + (v + 100) * t.w / 200;
                    let (a, b) = if kx < mid { (kx, mid) } else { (mid, kx) };
                    c.fill_rect(a, t.y, b - a, t.h, BTN_ON);
                    c.circle(kx, t.y + 3, 9, rgb(255, 255, 255));
                }
            }
            Tool::Filters => {
                if self.thumbs.is_empty() {
                    let base = edit::apply(&self.small, &Edits { filter: Filter::None, ..self.edits }).shrink_to_fit(120, 76);
                    for f in Filter::ALL {
                        let mut t = Image { w: base.w, h: base.h, px: base.px.clone(), opaque: base.opaque };
                        edit::color(&mut t, &Edits { filter: f, ..Edits::default() });
                        self.thumbs.push(t);
                    }
                }
                for (i, f) in Filter::ALL.iter().enumerate() {
                    let cell = Self::filter_cell(r, i);
                    if self.edits.filter == *f {
                        c.round_rect(cell.x - 3, cell.y - 3, cell.w + 6, cell.h + 6, 8, BTN_ON);
                    }
                    c.round_rect(cell.x, cell.y, cell.w, cell.h, 6, rgb(44, 48, 56));
                    let t = &self.thumbs[i];
                    c.draw_image(t, cell.x + (cell.w - t.w as i32) / 2, cell.y + 4 + (76 - t.h as i32) / 2);
                    c.text_centered(cell.x + cell.w / 2, cell.y + 82, f.name(), Size::Small, false, TEXT);
                }
            }
        }
        if self.has_original {
            button(c, Self::revert_btn(r), "Revert to original", BTN, TEXT);
        }

        if self.dialog {
            self.draw_dialog(c, r);
        }
    }

    fn draw_dialog(&self, c: &mut Canvas, r: Rect) {
        c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 130);
        let d = Self::dialog(r);
        c.round_rect_alpha(d.x + 3, d.y + 6, d.w, d.h, 12, 0, 90);
        c.round_rect(d.x, d.y, d.w, d.h, 12, rgb(255, 255, 255));
        let ink = rgb(40, 44, 52);
        let dim = rgb(110, 116, 126);
        c.text(d.x + 24, d.y + 22, "Save edited photo", Size::Medium, true, ink);
        if !self.can_save {
            c.text(d.x + 24, d.y + 62, "Edited photos are saved in Solon storage.", Size::Small, false, ink);
            c.text(d.x + 24, d.y + 82, "Set it up in Import (\"Set up Solon storage\") first.", Size::Small, false, ink);
            button(c, Self::dlg_cancel(r), "OK", rgb(226, 230, 236), ink);
            return;
        }
        let why = if self.can_replace {
            "The original can always be restored with \"Revert to original\"."
        } else {
            "This photo isn't in Solon storage, so it can only be saved as a copy."
        };
        c.text(d.x + 24, d.y + 62, why, Size::Small, false, dim);
        button(c, Self::dlg_copy(r), "Save as a new copy", BTN_ON, rgb(255, 255, 255));
        let (bg, fg) = if self.can_replace { (rgb(226, 230, 236), ink) } else { (rgb(242, 244, 247), rgb(190, 194, 200)) };
        button(c, Self::dlg_replace(r), "Replace original", bg, fg);
        button(c, Self::dlg_cancel(r), "Cancel", rgb(226, 230, 236), ink);
    }

    // ---- input ------------------------------------------------------------------------

    fn undo(&mut self) -> EditorAction {
        match self.history.pop() {
            Some(e) => {
                self.edits = e;
                self.thumbs.clear();
                EditorAction::Redraw
            }
            None => EditorAction::None,
        }
    }

    fn set_tool(&mut self, t: Tool) {
        self.tool = t;
        self.thumbs.clear();
    }

    fn set_aspect(&mut self, a: Aspect) {
        self.aspect = a;
        let c = self.crop_for(a);
        self.change(|e| e.crop = c);
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect) -> EditorAction {
        if self.dialog {
            if self.can_save && Self::dlg_copy(r).contains(x, y) {
                self.dialog = false;
                return EditorAction::Save(SaveChoice::Copy, self.edits);
            }
            if self.can_save && self.can_replace && Self::dlg_replace(r).contains(x, y) {
                self.dialog = false;
                return EditorAction::Save(SaveChoice::Replace, self.edits);
            }
            if Self::dlg_cancel(r).contains(x, y) || !Self::dialog(r).contains(x, y) {
                self.dialog = false;
                return EditorAction::Redraw;
            }
            return EditorAction::None;
        }
        if Self::cancel_btn(r).contains(x, y) {
            return EditorAction::Cancel;
        }
        if Self::save_btn(r).contains(x, y) {
            self.dialog = true;
            return EditorAction::Redraw;
        }
        if Self::undo_btn(r).contains(x, y) {
            return self.undo();
        }
        if Self::reset_btn(r).contains(x, y) {
            self.aspect = Aspect::Free;
            self.change(|e| *e = Edits::default());
            self.thumbs.clear();
            return EditorAction::Redraw;
        }
        if self.has_original && Self::revert_btn(r).contains(x, y) {
            return EditorAction::Revert;
        }
        for (i, t) in [Tool::Crop, Tool::Adjust, Tool::Filters].iter().enumerate() {
            if Self::tab(r, i as i32).contains(x, y) {
                self.set_tool(*t);
                return EditorAction::Redraw;
            }
        }
        match self.tool {
            Tool::Crop => {
                for i in 0..3 {
                    if Self::rotate_btn(r, i).contains(x, y) {
                        match i {
                            0 => self.change(|e| e.rotate(false)),
                            1 => self.change(|e| e.rotate(true)),
                            _ => self.change(|e| e.mirror()),
                        }
                        return EditorAction::Redraw;
                    }
                }
                for (i, a) in Aspect::ALL.iter().enumerate() {
                    if Self::aspect_btn(r, i as i32).contains(x, y) {
                        self.set_aspect(*a);
                        return EditorAction::Redraw;
                    }
                }
                let cr = self.crop_screen(r);
                let corners = [(cr.x, cr.y), (cr.x + cr.w, cr.y), (cr.x, cr.y + cr.h), (cr.x + cr.w, cr.y + cr.h)];
                if let Some(k) = corners.iter().position(|&(hx, hy)| (x - hx).abs() <= 14 && (y - hy).abs() <= 14) {
                    self.drag_start = Some(self.edits);
                    self.drag = Some(Drag::CropCorner(k as u8));
                    return EditorAction::None;
                }
                if cr.contains(x, y) {
                    self.drag_start = Some(self.edits);
                    self.drag = Some(Drag::CropMove { mx: x, my: y, start: self.edits.crop });
                    return EditorAction::None;
                }
            }
            Tool::Adjust => {
                if Self::auto_btn(r).contains(x, y) {
                    let (b, c, s) = edit::auto_fix(&edit::apply(&self.small, &Edits { brightness: 0, contrast: 0, saturation: 0, ..self.edits }));
                    self.change(|e| {
                        e.brightness = b;
                        e.contrast = c;
                        e.saturation = s;
                    });
                    kprintln!("editor: auto-fix brightness {} contrast {} saturation {}", b, c, s);
                    return EditorAction::Redraw;
                }
                for i in 0..4 {
                    let t = Self::slider_track(r, i);
                    if Rect::new(t.x - 10, t.y - 14, t.w + 20, t.h + 28).contains(x, y) {
                        self.drag_start = Some(self.edits);
                        self.drag = Some(Drag::Slider(i));
                        self.on_drag(x, y, r);
                        return EditorAction::Redraw;
                    }
                }
            }
            Tool::Filters => {
                for (i, f) in Filter::ALL.iter().enumerate() {
                    if Self::filter_cell(r, i).contains(x, y) {
                        let f = *f;
                        self.change(|e| e.filter = f);
                        return EditorAction::Redraw;
                    }
                }
            }
        }
        EditorAction::None
    }

    /// Mouse moved with the button held.
    pub fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        let Some(d) = self.drag else { return false };
        match d {
            Drag::Slider(i) => {
                let t = Self::slider_track(r, i);
                let v = ((x - t.x) * 200 / t.w.max(1) - 100).clamp(-100, 100);
                let old = self.slider_value(i);
                self.set_slider(i, v);
                old != v
            }
            Drag::CropMove { mx, my, start } => {
                let (_, _, img) = self.preview(r);
                let (pw, ph) = (img.w.max(1) as i64, img.h.max(1) as i64);
                let dx = ((x - mx) as i64 * FULL as i64 / pw) as i32;
                let dy = ((y - my) as i64 * FULL as i64 / ph) as i32;
                let nx = (start.x as i32 + dx).clamp(0, (FULL - start.w) as i32) as u32;
                let ny = (start.y as i32 + dy).clamp(0, (FULL - start.h) as i32) as u32;
                let old = self.edits.crop;
                self.edits.crop = Crop { x: nx, y: ny, ..start };
                old != self.edits.crop
            }
            Drag::CropCorner(k) => {
                let (px, py, img) = self.preview(r);
                let (pw, ph) = (img.w.max(1) as i64, img.h.max(1) as i64);
                let fx = (((x - px) as i64 * FULL as i64 / pw).clamp(0, FULL as i64)) as i32;
                let fy = (((y - py) as i64 * FULL as i64 / ph).clamp(0, FULL as i64)) as i32;
                let c = self.edits.crop;
                // The opposite corner stays put.
                let (ox, oy) = match k {
                    0 => (c.x + c.w, c.y + c.h),
                    1 => (c.x, c.y + c.h),
                    2 => (c.x + c.w, c.y),
                    _ => (c.x, c.y),
                };
                let (ox, oy) = (ox as i32, oy as i32);
                let min = 500; // 5%
                let mut w = (fx - ox).abs().max(min);
                let mut h = (fy - oy).abs().max(min);
                // Keep the shape for fixed aspect ratios (ratio is in pixels).
                let shape = match self.aspect {
                    Aspect::Original => {
                        let (tw, th) = self.turned();
                        Some((tw as i64, th as i64))
                    }
                    a => a.ratio().map(|q| {
                        let (tw, th) = self.turned();
                        if tw >= th {
                            (q as i64, 1000)
                        } else {
                            (1000, q as i64)
                        }
                    }),
                };
                if let Some((sw, sh)) = shape {
                    let (tw, th) = self.turned();
                    // Normalized height that matches width w at the pixel ratio sw:sh.
                    let h_for_w = (w as i64 * tw as i64 * sh / (sw * th as i64)) as i32;
                    if h_for_w <= h {
                        h = h_for_w.max(1);
                    } else {
                        w = (h as i64 * th as i64 * sw / (sh * tw as i64)) as i32;
                    }
                }
                let dirx = if k == 0 || k == 2 { -1 } else { 1 };
                let diry = if k == 0 || k == 1 { -1 } else { 1 };
                let x0 = if dirx < 0 { ox - w } else { ox };
                let y0 = if diry < 0 { oy - h } else { oy };
                if x0 < 0 || y0 < 0 || x0 + w > FULL as i32 || y0 + h > FULL as i32 {
                    return false; // would leave the photo
                }
                let old = self.edits.crop;
                self.edits.crop = Crop { x: x0 as u32, y: y0 as u32, w: w as u32, h: h as u32 };
                old != self.edits.crop
            }
        }
    }

    pub fn on_release(&mut self) -> bool {
        self.drag = None;
        if let Some(before) = self.drag_start.take() {
            if before != self.edits {
                self.history.push(before);
                self.thumbs.clear();
                return true;
            }
        }
        false
    }

    pub fn on_key(&mut self, sc: u8) -> EditorAction {
        match sc {
            0x1D => {
                self.ctrl = true;
                return EditorAction::None;
            }
            0x9D => {
                self.ctrl = false;
                return EditorAction::None;
            }
            _ if sc & 0x80 != 0 => return EditorAction::None,
            _ => {}
        }
        if self.dialog {
            return match sc {
                0x01 => {
                    self.dialog = false;
                    EditorAction::Redraw
                }
                0x1C | 0x2E if self.can_save => {
                    // Enter / C: save as copy
                    self.dialog = false;
                    EditorAction::Save(SaveChoice::Copy, self.edits)
                }
                _ => EditorAction::None,
            };
        }
        match sc {
            0x01 => EditorAction::Cancel,
            0x2C if self.ctrl => self.undo(), // Ctrl+Z
            0x1F => {
                // S
                self.dialog = true;
                EditorAction::Redraw
            }
            0x13 => {
                self.change(|e| e.rotate(true)); // R
                EditorAction::Redraw
            }
            0x26 => {
                self.change(|e| e.rotate(false)); // L
                EditorAction::Redraw
            }
            0x32 => {
                self.change(|e| e.mirror()); // M
                EditorAction::Redraw
            }
            0x0F => {
                // Tab: next tool
                let t = match self.tool {
                    Tool::Crop => Tool::Adjust,
                    Tool::Adjust => Tool::Filters,
                    Tool::Filters => Tool::Crop,
                };
                self.set_tool(t);
                EditorAction::Redraw
            }
            0x02..=0x08 if self.tool == Tool::Filters => {
                let f = Filter::ALL[(sc - 0x02) as usize];
                self.change(|e| e.filter = f);
                EditorAction::Redraw
            }
            0x02..=0x07 if self.tool == Tool::Crop => {
                self.set_aspect(Aspect::ALL[(sc - 0x02) as usize]);
                EditorAction::Redraw
            }
            0x1E if self.tool == Tool::Adjust => {
                // A: auto-fix (same as the button)
                let (b, c, s) = edit::auto_fix(&edit::apply(&self.small, &Edits { brightness: 0, contrast: 0, saturation: 0, ..self.edits }));
                self.change(|e| {
                    e.brightness = b;
                    e.contrast = c;
                    e.saturation = s;
                });
                kprintln!("editor: auto-fix brightness {} contrast {} saturation {}", b, c, s);
                EditorAction::Redraw
            }
            _ => EditorAction::None,
        }
    }
}

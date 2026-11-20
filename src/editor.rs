//! The photo editor screen (inside the Photos window), laid out like Lightroom: the photo on
//! the left, and on the right a histogram, an Auto button and tabs for Light, Color,
//! Effects, Presets and Crop.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use crate::edit::{self, Crop, Detail, Edits, FULL, PRESETS};
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::image::Image;
use crate::interrupts::TICKS;

const HEADER_H: i32 = 48;
const PANEL_W: i32 = 330;
const TAB_H: i32 = 38;
const HIST_H: i32 = 86;
const SLIDER_STEP: i32 = 58;
const DOUBLE_CLICK: u64 = 45;

const BG: u32 = rgb(24, 26, 30);
const BAR: u32 = rgb(38, 41, 48);
const PANEL: u32 = rgb(32, 35, 41);
const TEXT: u32 = rgb(230, 232, 236);
const TEXT_DIM: u32 = rgb(150, 156, 166);
const BTN: u32 = rgb(60, 64, 74);
const BTN_ON: u32 = rgb(46, 134, 222);
const DISABLED: u32 = rgb(44, 47, 54);
const TRACK: u32 = rgb(70, 75, 86);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Light,
    Color,
    Effects,
    Presets,
    Crop,
}

const TABS: [(Tab, &str); 5] = [(Tab::Light, "Light"), (Tab::Color, "Color"), (Tab::Effects, "Effects"), (Tab::Presets, "Presets"), (Tab::Crop, "Crop")];

/// A slider and the edit it changes.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Field {
    Exposure,
    Contrast,
    Highlights,
    Shadows,
    Whites,
    Blacks,
    Temperature,
    Tint,
    Vibrance,
    Saturation,
    Clarity,
    Vignette,
    Sharpen,
    Fade,
    Straighten,
}

impl Field {
    fn name(self) -> &'static str {
        match self {
            Field::Exposure => "Exposure",
            Field::Contrast => "Contrast",
            Field::Highlights => "Highlights",
            Field::Shadows => "Shadows",
            Field::Whites => "Whites",
            Field::Blacks => "Blacks",
            Field::Temperature => "Temperature",
            Field::Tint => "Tint",
            Field::Vibrance => "Vibrance",
            Field::Saturation => "Saturation",
            Field::Clarity => "Clarity",
            Field::Vignette => "Vignette",
            Field::Sharpen => "Sharpen",
            Field::Fade => "Fade",
            Field::Straighten => "Straighten",
        }
    }
    fn range(self) -> (i32, i32) {
        match self {
            Field::Sharpen | Field::Fade => (0, 100),
            Field::Straighten => (-450, 450),
            _ => (-100, 100),
        }
    }
    fn get(self, e: &Edits) -> i32 {
        match self {
            Field::Exposure => e.exposure,
            Field::Contrast => e.contrast,
            Field::Highlights => e.highlights,
            Field::Shadows => e.shadows,
            Field::Whites => e.whites,
            Field::Blacks => e.blacks,
            Field::Temperature => e.temperature,
            Field::Tint => e.tint,
            Field::Vibrance => e.vibrance,
            Field::Saturation => e.saturation,
            Field::Clarity => e.clarity,
            Field::Vignette => e.vignette,
            Field::Sharpen => e.sharpen,
            Field::Fade => e.fade,
            Field::Straighten => e.straighten,
        }
    }
    fn set(self, e: &mut Edits, v: i32) {
        let (lo, hi) = self.range();
        let v = v.clamp(lo, hi);
        match self {
            Field::Exposure => e.exposure = v,
            Field::Contrast => e.contrast = v,
            Field::Highlights => e.highlights = v,
            Field::Shadows => e.shadows = v,
            Field::Whites => e.whites = v,
            Field::Blacks => e.blacks = v,
            Field::Temperature => e.temperature = v,
            Field::Tint => e.tint = v,
            Field::Vibrance => e.vibrance = v,
            Field::Saturation => e.saturation = v,
            Field::Clarity => e.clarity = v,
            Field::Vignette => e.vignette = v,
            Field::Sharpen => e.sharpen = v,
            Field::Fade => e.fade = v,
            Field::Straighten => e.straighten = v,
        }
    }
    fn show(self, v: i32) -> String {
        match self {
            // Exposure in stops, like a camera: +0.50
            Field::Exposure => {
                let s = v * 2; // hundredths of a stop
                format!("{}{}.{:02}", if s < 0 { "-" } else if s > 0 { "+" } else { "" }, s.abs() / 100, s.abs() % 100)
            }
            Field::Straighten => format!("{}{}.{} deg", if v < 0 { "-" } else { "" }, v.abs() / 10, v.abs() % 10),
            _ if v > 0 && self.range().0 < 0 => format!("+{}", v),
            _ => format!("{}", v),
        }
    }
}

fn fields(t: Tab) -> &'static [Field] {
    match t {
        Tab::Light => &[Field::Exposure, Field::Contrast, Field::Highlights, Field::Shadows, Field::Whites, Field::Blacks],
        Tab::Color => &[Field::Temperature, Field::Tint, Field::Vibrance, Field::Saturation],
        Tab::Effects => &[Field::Clarity, Field::Vignette, Field::Sharpen, Field::Fade],
        Tab::Crop => &[Field::Straighten],
        Tab::Presets => &[],
    }
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
    Slider(Field),
    CropMove { mx: i32, my: i32, start: Crop },
    CropCorner(u8),
}

/// The framing of the preview: (turns, flip, straighten, crop, showing the crop box, area).
type GeomKey = (u8, bool, i32, Crop, bool, i32, i32);

struct Cache {
    geom: GeomKey,
    /// The framed photo at preview size, before light and color.
    base: Image,
    detail: Option<Detail>,
    /// What the preview shows: (edits, before) it was made for.
    out_key: (Edits, bool),
    out: Image,
    hist: [[u32; 256]; 4],
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
    tab: Tab,
    aspect: Aspect,
    drag: Option<Drag>,
    /// Edits in effect when the current drag started (one undo step per drag).
    drag_start: Option<Edits>,
    cache: Option<Cache>,
    /// Preset previews, for this framing.
    thumbs: Option<(GeomKey, Vec<Image>)>,
    before: bool,
    last_slider: Option<(Field, u64)>,
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
        let small = photo.shrink_to_fit(1400, 1400);
        Editor {
            index,
            name,
            small,
            src_w: src_w.max(1),
            src_h: src_h.max(1),
            edits: Edits::default(),
            history: Vec::new(),
            tab: Tab::Light,
            aspect: Aspect::Free,
            drag: None,
            drag_start: None,
            cache: None,
            thumbs: None,
            before: false,
            last_slider: None,
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
    fn before_btn(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 12 - 100 - 10 - 84 - 10 - 76 - 10 - 140, r.y + 8, 140, 32)
    }
    fn panel(r: Rect) -> Rect {
        Rect::new(r.x + r.w - PANEL_W, r.y + HEADER_H, PANEL_W, r.h - HEADER_H)
    }
    fn image_area(r: Rect) -> Rect {
        Rect::new(r.x + 20, r.y + HEADER_H + 20, r.w - PANEL_W - 40, r.h - HEADER_H - 40)
    }
    fn hist_box(r: Rect) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + 16, p.y + 12, PANEL_W - 32, HIST_H)
    }
    fn auto_btn(r: Rect) -> Rect {
        let h = Self::hist_box(r);
        Rect::new(h.x, h.y + h.h + 10, h.w, 36)
    }
    fn tab_rect(r: Rect, i: usize) -> Rect {
        let a = Self::auto_btn(r);
        let p = Self::panel(r);
        let w = PANEL_W / TABS.len() as i32;
        Rect::new(p.x + i as i32 * w, a.y + a.h + 12, w, TAB_H)
    }
    fn content_y(r: Rect) -> i32 {
        let t = Self::tab_rect(r, 0);
        t.y + t.h + 14
    }
    /// Where slider `k` of the current tab is; the Crop tab's slider sits below its buttons.
    fn slider_track(&self, r: Rect, k: usize) -> Rect {
        let p = Self::panel(r);
        let top = Self::content_y(r) + if self.tab == Tab::Crop { 88 } else { 0 };
        Rect::new(p.x + 24, top + 30 + k as i32 * SLIDER_STEP, PANEL_W - 48, 6)
    }
    fn bw_btn(&self, r: Rect) -> Rect {
        let t = self.slider_track(r, fields(Tab::Color).len());
        Rect::new(t.x - 8, t.y - 18, t.w + 16, 36)
    }
    fn rotate_btn(r: Rect, i: i32) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + 16 + i * 100, Self::content_y(r) + 24, 92, 34)
    }
    fn aspect_btn(r: Rect, i: i32) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + 16 + (i % 2) * 152, Self::content_y(r) + 186 + (i / 2) * 42, 144, 34)
    }
    fn preset_cell(r: Rect, i: usize) -> Rect {
        let p = Self::panel(r);
        let y0 = Self::content_y(r) - 4;
        let rows = PRESETS.len().div_ceil(3) as i32;
        let h = ((p.y + p.h - 12 - y0) / rows).clamp(44, 120);
        Rect::new(p.x + 12 + (i as i32 % 3) * 103, y0 + (i as i32 / 3) * h, 99, h - 6)
    }
    fn revert_btn(r: Rect) -> Rect {
        let p = Self::panel(r);
        Rect::new(p.x + 16, p.y + p.h - 50, PANEL_W - 32, 36)
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

    fn geom_key(&self, r: Rect) -> GeomKey {
        let a = Self::image_area(r);
        let crop_mode = self.tab == Tab::Crop;
        let e = &self.edits;
        (e.turns, e.flip, e.straighten, if crop_mode { Crop::ALL } else { e.crop }, crop_mode, a.w, a.h)
    }

    /// Bring the preview up to date. In the crop tab the picture is shown uncropped so the
    /// crop box can be moved around.
    fn update(&mut self, r: Rect) {
        let area = Self::image_area(r);
        let key = self.geom_key(r);
        if !matches!(&self.cache, Some(c) if c.geom == key) {
            let crop_mode = self.tab == Tab::Crop;
            let base = edit::geometry(&self.small, &self.edits, !crop_mode).shrink_to_fit(area.w.max(1) as usize, area.h.max(1) as usize);
            let empty = Image { w: 0, h: 0, px: Vec::new(), opaque: true };
            self.cache = Some(Cache { geom: key, base, detail: None, out_key: (Edits::default(), true), out: empty, hist: [[0; 256]; 4] });
        }
        let want = (self.edits.look(), self.before);
        let c = self.cache.as_mut().unwrap();
        if c.out_key != want || c.out.w == 0 {
            let mut out = Image { w: c.base.w, h: c.base.h, px: c.base.px.clone(), opaque: c.base.opaque };
            if !self.before {
                if (self.edits.clarity != 0 || self.edits.sharpen != 0) && c.detail.is_none() {
                    c.detail = Some(Detail::of(&c.base));
                }
                edit::render(&mut out, &self.edits, c.detail.as_ref());
            }
            c.hist = edit::histogram(&out);
            c.out = out;
            c.out_key = want;
        }
    }

    /// Where the preview is drawn, and its size.
    fn preview_rect(&mut self, r: Rect) -> Rect {
        self.update(r);
        let area = Self::image_area(r);
        let img = &self.cache.as_ref().unwrap().out;
        Rect::new(area.x + (area.w - img.w as i32) / 2, area.y + (area.h - img.h as i32) / 2, img.w as i32, img.h as i32)
    }

    fn crop_screen(&mut self, r: Rect) -> Rect {
        let c = self.edits.crop;
        let p = self.preview_rect(r);
        Rect::new(
            p.x + (c.x as i64 * p.w as i64 / FULL as i64) as i32,
            p.y + (c.y as i64 * p.h as i64 / FULL as i64) as i32,
            (c.w as i64 * p.w as i64 / FULL as i64) as i32,
            (c.h as i64 * p.h as i64 / FULL as i64) as i32,
        )
    }

    /// Small previews of every preset on this framing.
    fn preset_thumbs(&mut self, r: Rect) -> &Vec<Image> {
        let key = self.geom_key(r);
        if !matches!(&self.thumbs, Some((k, _)) if *k == key) {
            let base = edit::geometry(&self.small.shrink_to_fit(400, 400), &self.edits, true).shrink_to_fit(91, 64);
            let mut v = Vec::new();
            for i in 0..PRESETS.len() {
                let mut e = self.edits;
                edit::apply_preset(&mut e, i);
                let mut t = Image { w: base.w, h: base.h, px: base.px.clone(), opaque: base.opaque };
                edit::color(&mut t, &e);
                v.push(t);
            }
            self.thumbs = Some((key, v));
        }
        &self.thumbs.as_ref().unwrap().1
    }

    // ---- drawing --------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y, r.w, r.h, BG);
        c.fill_rect(r.x, r.y, r.w, HEADER_H, BAR);
        button(c, Self::cancel_btn(r), "Cancel", BTN, TEXT);
        let title = crate::photos::ellipsize(&self.name, (Self::before_btn(r).x - r.x - 140).max(40), Size::Small, true);
        c.text(r.x + 124, r.y + 15, &title, Size::Small, true, TEXT);
        button(c, Self::before_btn(r), if self.before { "Showing before" } else { "Before / after" }, if self.before { BTN_ON } else { BTN }, TEXT);
        let undo_ok = !self.history.is_empty();
        button(c, Self::undo_btn(r), "Undo", if undo_ok { BTN } else { DISABLED }, if undo_ok { TEXT } else { TEXT_DIM });
        let reset_ok = !self.edits.is_original();
        button(c, Self::reset_btn(r), "Reset", if reset_ok { BTN } else { DISABLED }, if reset_ok { TEXT } else { TEXT_DIM });
        button(c, Self::save_btn(r), "Save", BTN_ON, rgb(255, 255, 255));

        // Picture
        let p = self.preview_rect(r);
        {
            let cache = self.cache.as_ref().unwrap();
            if !cache.out.opaque {
                c.fill_rect(p.x, p.y, p.w, p.h, rgb(200, 200, 204));
            }
            c.draw_image(&cache.out, p.x, p.y);
        }
        if self.before {
            c.round_rect_alpha(p.x + 10, p.y + 10, 86, 28, 6, 0, 150);
            c.text(p.x + 22, p.y + 15, "Before", Size::Small, true, rgb(255, 255, 255));
        }
        if self.tab == Tab::Crop && !self.before {
            let cr = self.crop_screen(r);
            c.fill_rect_alpha(p.x, p.y, p.w, cr.y - p.y, 0, 140);
            c.fill_rect_alpha(p.x, cr.y + cr.h, p.w, p.y + p.h - cr.y - cr.h, 0, 140);
            c.fill_rect_alpha(p.x, cr.y, cr.x - p.x, cr.h, 0, 140);
            c.fill_rect_alpha(cr.x + cr.w, cr.y, p.x + p.w - cr.x - cr.w, cr.h, 0, 140);
            // While straightening, a fine grid helps line things up.
            let lines = if matches!(self.drag, Some(Drag::Slider(Field::Straighten))) { 8 } else { 3 };
            for k in 1..lines {
                c.fill_rect_alpha(cr.x + cr.w * k / lines, cr.y, 1, cr.h, rgb(255, 255, 255), 90);
                c.fill_rect_alpha(cr.x, cr.y + cr.h * k / lines, cr.w, 1, rgb(255, 255, 255), 90);
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

        self.draw_panel(c, r);
        if self.dialog {
            self.draw_dialog(c, r);
        }
    }

    fn draw_histogram(&self, c: &mut Canvas, r: Rect) {
        let b = Self::hist_box(r);
        c.round_rect(b.x, b.y, b.w, b.h, 6, rgb(22, 24, 28));
        let Some(cache) = &self.cache else { return };
        let h = &cache.hist;
        // Scale to the tallest bin, ignoring the two ends (clipped pixels pile up there).
        let peak = (1..255).flat_map(|i| (0..3).map(move |ch| h[ch][i])).max().unwrap_or(1).max(1);
        let cols = [rgb(220, 70, 70), rgb(70, 200, 90), rgb(70, 120, 235)];
        let (ix, iy, iw, ih) = (b.x + 4, b.y + 4, b.w - 8, b.h - 8);
        for px in 0..iw {
            let bin = (px * 256 / iw) as usize;
            for (ch, col) in cols.iter().enumerate() {
                let v = ((h[ch][bin] as u64 * ih as u64 / peak as u64) as i32).min(ih);
                if v > 0 {
                    c.fill_rect_alpha(ix + px, iy + ih - v, 1, v, *col, 110);
                }
            }
            let v = ((h[3][bin] as u64 * ih as u64 / peak as u64) as i32).min(ih);
            if v > 0 {
                c.fill_rect_alpha(ix + px, iy + ih - v, 1, v, rgb(230, 230, 230), 60);
            }
        }
        // Clipping warnings: a lot of pure black or pure white.
        let total: u32 = h[3].iter().sum::<u32>().max(1);
        if h[3][0] * 100 / total >= 2 {
            c.fill_rect(b.x + 6, b.y + 6, 10, 10, rgb(70, 120, 235));
        }
        if h[3][255] * 100 / total >= 2 {
            c.fill_rect(b.x + b.w - 16, b.y + 6, 10, 10, rgb(240, 80, 70));
        }
    }

    fn draw_slider(&self, c: &mut Canvas, r: Rect, k: usize, f: Field) {
        let t = self.slider_track(r, k);
        let v = f.get(&self.edits);
        let (lo, hi) = f.range();
        c.text(t.x, t.y - 26, f.name(), Size::Small, true, TEXT);
        let vs = f.show(v);
        c.text(t.x + t.w - text_width(&vs, Size::Small, false), t.y - 26, &vs, Size::Small, false, if v != 0 { TEXT } else { TEXT_DIM });
        // Track: colour ranges for white balance, a plain track otherwise.
        match f {
            Field::Temperature | Field::Tint => {
                let (a, b) = if f == Field::Temperature { (rgb(70, 120, 230), rgb(235, 200, 60)) } else { (rgb(70, 190, 80), rgb(210, 80, 200)) };
                for x in 0..t.w {
                    c.fill_rect(t.x + x, t.y, 1, t.h, crate::gfx::mix(a, b, (x * 255 / t.w.max(1)) as u32));
                }
            }
            _ => c.round_rect(t.x, t.y, t.w, t.h, 3, TRACK),
        }
        let kx = t.x + ((v - lo) as i64 * t.w as i64 / (hi - lo) as i64) as i32;
        if !matches!(f, Field::Temperature | Field::Tint) {
            let zero = t.x + ((0 - lo) as i64 * t.w as i64 / (hi - lo) as i64) as i32;
            let (a, b) = if kx < zero { (kx, zero) } else { (zero, kx) };
            c.fill_rect(a, t.y, b - a, t.h, BTN_ON);
        }
        c.circle(kx, t.y + 3, 9, rgb(255, 255, 255));
    }

    fn draw_panel(&mut self, c: &mut Canvas, r: Rect) {
        let p = Self::panel(r);
        c.fill_rect(p.x, p.y, p.w, p.h, PANEL);
        self.draw_histogram(c, r);
        button(c, Self::auto_btn(r), "Auto", BTN_ON, rgb(255, 255, 255));
        for (i, (t, name)) in TABS.iter().enumerate() {
            let tr = Self::tab_rect(r, i);
            let on = self.tab == *t;
            if on {
                c.fill_rect(tr.x + 4, tr.y + tr.h - 3, tr.w - 8, 3, BTN_ON);
            }
            c.text_centered(tr.x + tr.w / 2, tr.y + 10, name, Size::Small, true, if on { TEXT } else { TEXT_DIM });
        }
        let t0 = Self::tab_rect(r, 0);
        c.fill_rect(p.x, t0.y + t0.h, p.w, 1, rgb(50, 54, 62));
        let y0 = Self::content_y(r);
        for (k, f) in fields(self.tab).iter().enumerate() {
            self.draw_slider(c, r, k, *f);
        }
        match self.tab {
            Tab::Color => {
                let b = self.bw_btn(r);
                button(c, b, if self.edits.bw { "Black & white: on" } else { "Black & white: off" }, if self.edits.bw { BTN_ON } else { BTN }, TEXT);
            }
            Tab::Crop => {
                c.text(p.x + 16, y0, "Rotate", Size::Small, true, TEXT_DIM);
                for (i, l) in ["Left", "Right", "Mirror"].iter().enumerate() {
                    button(c, Self::rotate_btn(r, i as i32), l, BTN, TEXT);
                }
                c.text(p.x + 16, Self::aspect_btn(r, 0).y - 26, "Shape", Size::Small, true, TEXT_DIM);
                for (i, a) in Aspect::ALL.iter().enumerate() {
                    let on = self.aspect == *a;
                    button(c, Self::aspect_btn(r, i as i32), a.name(), if on { BTN_ON } else { BTN }, TEXT);
                }
                let hint_y = Self::aspect_btn(r, 5).y + 50;
                c.text(p.x + 16, hint_y, "Drag the corners to crop, or", Size::Small, false, TEXT_DIM);
                c.text(p.x + 16, hint_y + 20, "inside the box to move it.", Size::Small, false, TEXT_DIM);
            }
            Tab::Presets => {
                let chosen = self.edits.preset as usize;
                let original = self.edits.is_original();
                let cells: Vec<Rect> = (0..PRESETS.len()).map(|i| Self::preset_cell(r, i)).collect();
                let thumbs = self.preset_thumbs(r);
                for (i, cell) in cells.iter().enumerate() {
                    if chosen == i && (i != 0 || !original) {
                        c.round_rect(cell.x - 3, cell.y - 3, cell.w + 6, cell.h + 6, 8, BTN_ON);
                    }
                    c.round_rect(cell.x, cell.y, cell.w, cell.h, 6, rgb(44, 48, 56));
                    let t = &thumbs[i];
                    let room = cell.h - 26;
                    if t.h as i32 <= room {
                        c.draw_image(t, cell.x + (cell.w - t.w as i32) / 2, cell.y + 3 + (room - t.h as i32) / 2);
                    }
                    c.text_centered(cell.x + cell.w / 2, cell.y + cell.h - 21, PRESETS[i].name, Size::Small, false, TEXT);
                }
            }
            _ => {}
        }
        if self.has_original && self.tab != Tab::Presets {
            button(c, Self::revert_btn(r), "Revert to original", BTN, TEXT);
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
            c.text(d.x + 24, d.y + 82, "Set it up in Disks (\"Set up Solon storage\") first.", Size::Small, false, ink);
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
                EditorAction::Redraw
            }
            None => EditorAction::None,
        }
    }

    fn set_aspect(&mut self, a: Aspect) {
        self.aspect = a;
        let c = self.crop_for(a);
        self.change(|e| e.crop = c);
    }

    fn auto(&mut self) {
        let framed = edit::geometry(&self.small, &self.edits, true);
        let e = edit::auto(&framed, &self.edits);
        self.change(|x| *x = e);
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
        if Self::before_btn(r).contains(x, y) {
            self.before = !self.before;
            return EditorAction::Redraw;
        }
        if Self::reset_btn(r).contains(x, y) {
            self.aspect = Aspect::Free;
            self.change(|e| *e = Edits::default());
            return EditorAction::Redraw;
        }
        if Self::auto_btn(r).contains(x, y) {
            self.auto();
            return EditorAction::Redraw;
        }
        if self.has_original && self.tab != Tab::Presets && Self::revert_btn(r).contains(x, y) {
            return EditorAction::Revert;
        }
        for (i, (t, _)) in TABS.iter().enumerate() {
            if Self::tab_rect(r, i).contains(x, y) {
                self.tab = *t;
                return EditorAction::Redraw;
            }
        }
        // Sliders (double-click puts one back to zero).
        for (k, f) in fields(self.tab).iter().enumerate() {
            let t = self.slider_track(r, k);
            if Rect::new(t.x - 12, t.y - 16, t.w + 24, t.h + 30).contains(x, y) {
                let now = TICKS.load(Relaxed);
                let double = matches!(self.last_slider, Some((g, at)) if g == *f && now - at <= DOUBLE_CLICK);
                self.last_slider = Some((*f, now));
                if double {
                    let f = *f;
                    self.change(|e| f.set(e, 0));
                    return EditorAction::Redraw;
                }
                self.drag_start = Some(self.edits);
                self.drag = Some(Drag::Slider(*f));
                self.on_drag(x, y, r);
                return EditorAction::Redraw;
            }
        }
        match self.tab {
            Tab::Color => {
                if self.bw_btn(r).contains(x, y) {
                    self.change(|e| e.bw = !e.bw);
                    return EditorAction::Redraw;
                }
            }
            Tab::Presets => {
                for i in 0..PRESETS.len() {
                    if Self::preset_cell(r, i).contains(x, y) {
                        self.change(|e| edit::apply_preset(e, i));
                        kprintln!("editor: preset {}", PRESETS[i].name);
                        return EditorAction::Redraw;
                    }
                }
            }
            Tab::Crop => {
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
            _ => {}
        }
        EditorAction::None
    }

    /// Mouse moved with the button held.
    pub fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        let Some(d) = self.drag else { return false };
        match d {
            Drag::Slider(f) => {
                let k = fields(self.tab).iter().position(|g| *g == f).unwrap_or(0);
                let t = self.slider_track(r, k);
                let (lo, hi) = f.range();
                let v = lo + ((x - t.x).clamp(0, t.w) as i64 * (hi - lo) as i64 / t.w.max(1) as i64) as i32;
                // Snap to zero near the middle.
                let v = if v.abs() <= (hi - lo) / 100 { 0 } else { v };
                let old = f.get(&self.edits);
                f.set(&mut self.edits, v);
                old != f.get(&self.edits)
            }
            Drag::CropMove { mx, my, start } => {
                let p = self.preview_rect(r);
                let (pw, ph) = (p.w.max(1) as i64, p.h.max(1) as i64);
                let dx = ((x - mx) as i64 * FULL as i64 / pw) as i32;
                let dy = ((y - my) as i64 * FULL as i64 / ph) as i32;
                let nx = (start.x as i32 + dx).clamp(0, (FULL - start.w) as i32) as u32;
                let ny = (start.y as i32 + dy).clamp(0, (FULL - start.h) as i32) as u32;
                let old = self.edits.crop;
                self.edits.crop = Crop { x: nx, y: ny, ..start };
                old != self.edits.crop
            }
            Drag::CropCorner(k) => {
                let p = self.preview_rect(r);
                let (pw, ph) = (p.w.max(1) as i64, p.h.max(1) as i64);
                let fx = (((x - p.x) as i64 * FULL as i64 / pw).clamp(0, FULL as i64)) as i32;
                let fy = (((y - p.y) as i64 * FULL as i64 / ph).clamp(0, FULL as i64)) as i32;
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
        let was_straighten = matches!(self.drag, Some(Drag::Slider(Field::Straighten)));
        self.drag = None;
        if let Some(before) = self.drag_start.take() {
            if before != self.edits {
                self.history.push(before);
                return true;
            }
        }
        was_straighten // redraw without the fine grid
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
            0x2B => {
                // \ : before / after, like Lightroom
                self.before = !self.before;
                EditorAction::Redraw
            }
            0x1E => {
                self.auto(); // A
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
                // Tab: next tab
                let i = TABS.iter().position(|(t, _)| *t == self.tab).unwrap_or(0);
                self.tab = TABS[(i + 1) % TABS.len()].0;
                EditorAction::Redraw
            }
            0x02..=0x0B if self.tab == Tab::Presets => {
                let i = (sc - 0x02) as usize;
                if i < PRESETS.len() {
                    self.change(|e| edit::apply_preset(e, i));
                }
                EditorAction::Redraw
            }
            0x02..=0x07 if self.tab == Tab::Crop => {
                self.set_aspect(Aspect::ALL[(sc - 0x02) as usize]);
                EditorAction::Redraw
            }
            _ => EditorAction::None,
        }
    }
}

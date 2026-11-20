//! The spreadsheet editor: a grid with a formula bar, toolbar, sheet tabs, formatting,
//! undo, copy/paste and fill down.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::doc::sheet::{cell_name, col_name, general, Cell, Content, NumFmt, Sheet, Style, Value, Workbook, MAX_COLS, MAX_ROWS};
use crate::doc::xlsx::shift_formula;
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::textfield::{TextEvent, TextField};

const INK: u32 = rgb(40, 44, 52);
const DIM: u32 = rgb(110, 116, 126);
const ACCENT: u32 = rgb(33, 115, 70); // spreadsheet green
const BTN: u32 = rgb(230, 233, 238);
const GRID: u32 = rgb(222, 225, 230);
const HEAD_BG: u32 = rgb(242, 243, 245);

const RH: i32 = 26; // row height
const HH: i32 = 24; // column header height
const RW: i32 = 56; // row number width
const TOOL_H: i32 = 40;
const BAR_H: i32 = 38;
const TABS_H: i32 = 34;
const DOUBLE: u64 = 50;
const UNDO_MAX: usize = 30;

/// Colours offered for text and fill.
const PALETTE: [u32; 15] = [
    0x000000, 0x595959, 0xC00000, 0xED7D31, 0xFFC000, 0x70AD47, 0x00B050, 0x4472C4, 0x5B9BD5, 0x7030A0, 0xFFF2CC, 0xE2EFDA, 0xDDEBF7, 0xFCE4D6, 0xEDEDED,
];

pub enum SheetAction {
    None,
    Redraw,
    Save,
    Print,
    Close,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Back,
    Save,
    Print,
    Undo,
    Bold,
    TextColor,
    Fill,
    Format,
    Left,
    Center,
    Right,
    Wider,
    Narrower,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Format,
    TextColor,
    Fill,
}

struct Rename {
    field: TextField,
}

/// Copied cells, relative to the top-left of the copy.
struct Clip {
    origin: (u32, u32),
    cells: Vec<((u32, u32), Cell)>,
    size: (u32, u32),
}

pub struct SheetEditor {
    pub wb: Workbook,
    pub sheet: usize,
    cur: (u32, u32),
    anchor: (u32, u32),
    top: u32,
    left: u32,
    edit: Option<TextField>,
    shift: bool,
    ctrl: bool,
    menu: Option<Menu>,
    rename: Option<Rename>,
    undo: Vec<(Vec<Sheet>, usize)>,
    clip: Option<Clip>,
    pub dirty: bool,
    last_click: Option<((u32, u32), u64)>,
    last_tab: Option<(usize, u64)>,
    dragging: bool,
    /// Short message in the tab bar (e.g. "Copied").
    note: Option<String>,
}

fn char_w() -> i32 {
    text_width("0", Size::Small, false)
}

fn col_px(sh: &Sheet, c: u32) -> i32 {
    // Our font is fixed-width and wider than Excel's Calibri, so widths are scaled up a bit.
    (sh.width(c) * 1.3 * char_w() as f32) as i32 + 8
}

fn button(c: &mut Canvas, b: Rect, label: &str, on: bool) {
    c.round_rect(b.x, b.y, b.w, b.h, 6, if on { rgb(200, 226, 210) } else { BTN });
    c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, INK);
}

impl SheetEditor {
    pub fn new(wb: Workbook) -> SheetEditor {
        SheetEditor {
            wb,
            sheet: 0,
            cur: (0, 0),
            anchor: (0, 0),
            top: 0,
            left: 0,
            edit: None,
            shift: false,
            ctrl: false,
            menu: None,
            rename: None,
            undo: Vec::new(),
            clip: None,
            dirty: false,
            last_click: None,
            last_tab: None,
            dragging: false,
            note: None,
        }
    }

    fn sh(&self) -> &Sheet {
        &self.wb.sheets[self.sheet]
    }

    /// Selected range: (r0, c0, r1, c1).
    fn sel(&self) -> (u32, u32, u32, u32) {
        (self.cur.0.min(self.anchor.0), self.cur.1.min(self.anchor.1), self.cur.0.max(self.anchor.0), self.cur.1.max(self.anchor.1))
    }

    fn snapshot(&mut self) {
        self.undo.push((self.wb.sheets.clone(), self.sheet));
        if self.undo.len() > UNDO_MAX {
            self.undo.remove(0);
        }
        self.dirty = true;
    }

    fn changed(&mut self) {
        self.wb.recalc();
        self.dirty = true;
    }

    // ---- layout -----------------------------------------------------------------

    fn tools(&self) -> Vec<(Tool, String)> {
        let fmt = self.sh().style(self.cur.0, self.cur.1).fmt;
        Vec::from([
            (Tool::Back, String::from("< Documents")),
            (Tool::Save, String::from("Save")),
            (Tool::Print, String::from("Print")),
            (Tool::Undo, String::from("Undo")),
            (Tool::Bold, String::from("B")),
            (Tool::TextColor, String::from("Text colour")),
            (Tool::Fill, String::from("Fill")),
            (Tool::Format, format!("{} v", fmt.label())),
            (Tool::Left, String::from("Left")),
            (Tool::Center, String::from("Centre")),
            (Tool::Right, String::from("Right")),
            (Tool::Wider, String::from("Wider")),
            (Tool::Narrower, String::from("Narrower")),
        ])
    }

    /// Toolbar buttons, wrapping onto more rows on narrow windows; and the toolbar height.
    fn tool_rects(&self, r: Rect) -> (Vec<(Tool, String, Rect)>, i32) {
        let mut out = Vec::new();
        let (mut x, mut y) = (r.x + 8, r.y + 6);
        for (t, label) in self.tools() {
            let w = text_width(&label, Size::Small, true) + 20;
            let w = if t == Tool::Bold { 34 } else { w };
            if x + w > r.x + r.w - 8 && x > r.x + 8 {
                x = r.x + 8;
                y += TOOL_H - 6;
            }
            out.push((t, label, Rect::new(x, y, w, TOOL_H - 12)));
            x += w + 6;
        }
        let h = y + TOOL_H - r.y;
        (out, h)
    }

    fn bar(&self, r: Rect) -> Rect {
        let th = self.tool_rects(r).1;
        Rect::new(r.x, r.y + th, r.w, BAR_H)
    }
    fn grid(&self, r: Rect) -> Rect {
        let b = self.bar(r);
        let y = b.y + b.h;
        Rect::new(r.x, y, r.w, r.y + r.h - TABS_H - y)
    }
    fn tabs(r: Rect) -> Rect {
        Rect::new(r.x, r.y + r.h - TABS_H, r.w, TABS_H)
    }
    fn tab_rects(&self, r: Rect) -> (Vec<Rect>, Rect) {
        let t = Self::tabs(r);
        let mut x = t.x + 8;
        let mut v = Vec::new();
        for s in &self.wb.sheets {
            let w = text_width(&s.name, Size::Small, true) + 28;
            v.push(Rect::new(x, t.y + 3, w, t.h - 6));
            x += w + 4;
        }
        (v, Rect::new(x + 4, t.y + 3, 32, t.h - 6))
    }

    fn visible_rows(&self, g: Rect) -> u32 {
        ((g.h - HH) / RH).max(1) as u32
    }

    /// Columns from `left` that fit (at least one), with their x positions.
    fn visible_cols(&self, g: Rect) -> Vec<(u32, i32, i32)> {
        let mut v = Vec::new();
        let mut x = g.x + RW;
        let mut c = self.left;
        while x < g.x + g.w && c < MAX_COLS {
            let w = col_px(self.sh(), c);
            v.push((c, x, w));
            x += w;
            c += 1;
        }
        v
    }

    fn cell_at(&self, g: Rect, x: i32, y: i32) -> Option<(u32, u32)> {
        if y < g.y + HH || x < g.x + RW {
            return None;
        }
        let r = self.top + ((y - g.y - HH) / RH) as u32;
        let c = self.visible_cols(g).into_iter().find(|(_, cx, w)| x >= *cx && x < cx + w)?.0;
        Some((r.min(MAX_ROWS - 1), c))
    }

    fn ensure_visible(&mut self, r: Rect) {
        let g = self.grid(r);
        let rows = self.visible_rows(g);
        if self.cur.0 < self.top {
            self.top = self.cur.0;
        } else if self.cur.0 >= self.top + rows {
            self.top = self.cur.0 + 1 - rows;
        }
        if self.cur.1 < self.left {
            self.left = self.cur.1;
        } else {
            loop {
                let cols = self.visible_cols(g);
                // Fully visible columns.
                let full = cols.iter().filter(|(_, x, w)| x + w <= g.x + g.w).count().max(1) as u32;
                if self.cur.1 < self.left + full || self.left >= self.cur.1 {
                    break;
                }
                self.left += 1;
            }
        }
    }

    // ---- drawing ----------------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(255, 255, 255));
        let st = self.sh().style(self.cur.0, self.cur.1);
        let (tools, th) = self.tool_rects(r);
        c.fill_rect(r.x, r.y, r.w, th, rgb(247, 248, 250));
        for (t, label, b) in &tools {
            let on = match t {
                Tool::Bold => st.bold,
                Tool::Left => st.align == -1,
                Tool::Center => st.align == 1,
                Tool::Right => st.align == 2,
                Tool::Format => self.menu == Some(Menu::Format),
                Tool::TextColor => self.menu == Some(Menu::TextColor),
                Tool::Fill => self.menu == Some(Menu::Fill),
                _ => false,
            };
            let disabled = *t == Tool::Undo && self.undo.is_empty();
            if disabled {
                c.round_rect(b.x, b.y, b.w, b.h, 6, rgb(244, 245, 247));
                c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, rgb(190, 194, 200));
            } else {
                button(c, *b, label, on);
            }
            // Colour bars under the colour buttons.
            if *t == Tool::TextColor {
                c.fill_rect(b.x + 8, b.y + b.h - 5, b.w - 16, 3, st.color.unwrap_or(0));
            }
            if *t == Tool::Fill {
                c.fill_rect(b.x + 8, b.y + b.h - 5, b.w - 16, 3, st.fill.unwrap_or(0xFFFFFF));
            }
        }

        // Formula bar
        let b = self.bar(r);
        c.fill_rect(b.x, b.y, b.w, 1, GRID);
        let name = cell_name(self.cur.0, self.cur.1);
        let (r0, c0, r1, c1) = self.sel();
        let name = if (r0, c0) != (r1, c1) { format!("{}:{}", cell_name(r0, c0), cell_name(r1, c1)) } else { name };
        c.round_rect(b.x + 8, b.y + 5, 110, b.h - 10, 4, HEAD_BG);
        let name = crate::photos::ellipsize(&name, 100, Size::Small, true);
        c.text(b.x + 14, b.y + 11, &name, Size::Small, true, INK);
        let fr = Rect::new(b.x + 126, b.y + 4, b.w - 134, b.h - 8);
        match &self.edit {
            Some(f) => f.draw(c, fr),
            None => {
                c.outline_rect(fr.x, fr.y, fr.w, fr.h, GRID);
                let t = self.wb.edit_text(self.sheet, self.cur.0, self.cur.1);
                let t = crate::photos::ellipsize(&t, fr.w - 20, Size::Small, false);
                c.text(fr.x + 10, fr.y + (fr.h - 16) / 2, &t, Size::Small, false, INK);
            }
        }

        self.draw_grid(c, r);
        self.draw_tabs(c, r);
        self.draw_menu(c, r);
        self.draw_rename(c, r);
    }

    fn draw_grid(&self, c: &mut Canvas, r: Rect) {
        let g = self.grid(r);
        let sh = self.sh();
        let cols = self.visible_cols(g);
        let rows = self.visible_rows(g) + 1;
        let (r0, c0, r1, c1) = self.sel();
        c.set_clip(g);
        // Headers
        c.fill_rect(g.x, g.y, g.w, HH, HEAD_BG);
        c.fill_rect(g.x, g.y, RW, g.h, HEAD_BG);
        for &(col, x, w) in &cols {
            let selected = col >= c0 && col <= c1;
            if selected {
                c.fill_rect(x, g.y, w, HH, rgb(210, 230, 218));
            }
            c.text_centered(x + w / 2, g.y + 4, &col_name(col), Size::Small, selected, if selected { ACCENT } else { DIM });
            c.fill_rect(x + w - 1, g.y, 1, g.h, GRID);
        }
        for i in 0..rows {
            let row = self.top + i;
            if row >= MAX_ROWS {
                break;
            }
            let y = g.y + HH + i as i32 * RH;
            let selected = row >= r0 && row <= r1;
            if selected {
                c.fill_rect(g.x, y, RW, RH, rgb(210, 230, 218));
            }
            let label = format!("{}", row + 1);
            c.text_centered(g.x + RW / 2, y + 5, &label, Size::Small, selected, if selected { ACCENT } else { DIM });
            c.fill_rect(g.x + RW, y + RH - 1, g.w - RW, 1, GRID);
        }
        c.fill_rect(g.x, g.y + HH - 1, g.w, 1, rgb(200, 204, 210));
        c.fill_rect(g.x + RW - 1, g.y, 1, g.h, rgb(200, 204, 210));

        // Cells
        let body = Rect::new(g.x + RW, g.y + HH, g.w - RW, g.h - HH);
        for i in 0..rows {
            let row = self.top + i;
            if row >= MAX_ROWS {
                break;
            }
            let y = g.y + HH + i as i32 * RH;
            for (k, &(col, x, w)) in cols.iter().enumerate() {
                let st = sh.style(row, col);
                if let Some(f) = st.fill {
                    c.fill_rect(x, y, w - 1, RH - 1, f);
                }
                let editing_here = self.edit.is_some() && (row, col) == self.cur;
                let text = if editing_here { self.edit.as_ref().unwrap().text.clone() } else { self.wb.display(self.sheet, row, col) };
                if text.is_empty() {
                    continue;
                }
                let value = self.wb.value(self.sheet, row, col);
                let align = match st.align {
                    0 => match value {
                        Value::Num(_) => 2,
                        Value::Bool(_) | Value::Err(_) => 1,
                        _ => -1,
                    },
                    a => a,
                };
                let align = if editing_here { -1 } else { align };
                let tw = text_width(&text, Size::Small, st.bold);
                // Text spills into empty cells to the right.
                let mut room = w;
                if align == -1 && tw > w - 8 {
                    for &(nc, _, nw) in cols.iter().skip(k + 1) {
                        if sh.cells.contains_key(&(row, nc)) || room > tw + 8 {
                            break;
                        }
                        room += nw;
                    }
                }
                let shown = if matches!(value, Value::Num(_)) && tw > w - 6 && !editing_here { "#".repeat(((w - 6) / char_w()).max(1) as usize) } else { text };
                let tw = text_width(&shown, Size::Small, st.bold);
                let tx = match align {
                    1 => x + (w - tw) / 2,
                    2 => x + w - 6 - tw,
                    _ => x + 5,
                };
                let clip = Rect::new(x, y, room - 1, RH - 1);
                let clip = intersect(clip, body);
                c.set_clip(clip);
                c.text(tx, y + 5, &shown, Size::Small, st.bold, st.color.unwrap_or(INK));
                c.set_clip(g);
            }
        }

        // Selection
        let pos = |row: u32, col: u32| -> Option<(i32, i32, i32, i32)> {
            if row < self.top {
                return None;
            }
            let (_, x, w) = *cols.iter().find(|(cc, _, _)| *cc == col)?;
            Some((x, g.y + HH + (row - self.top) as i32 * RH, w, RH))
        };
        c.set_clip(body);
        if (r0, c0) != (r1, c1) {
            // Shade the visible part of the range.
            for &(_, x, w) in cols.iter().filter(|(cc, _, _)| *cc >= c0 && *cc <= c1) {
                let ys = r0.max(self.top);
                let ye = r1.min(self.top + rows);
                if ys <= ye {
                    let y = g.y + HH + (ys - self.top) as i32 * RH;
                    c.fill_rect_alpha(x, y, w, (ye - ys + 1) as i32 * RH, ACCENT, 30);
                }
            }
        }
        if let Some((x, y, w, h)) = pos(self.cur.0, self.cur.1) {
            for k in 0..2 {
                c.outline_rect(x - 1 + k, y - 1 + k, w + 1 - 2 * k, h + 1 - 2 * k, ACCENT);
            }
        }
        c.set_clip(r);
    }

    fn draw_tabs(&self, c: &mut Canvas, r: Rect) {
        let t = Self::tabs(r);
        c.fill_rect(t.x, t.y, t.w, t.h, HEAD_BG);
        c.fill_rect(t.x, t.y, t.w, 1, rgb(200, 204, 210));
        let (tabs, plus) = self.tab_rects(r);
        for (i, (b, s)) in tabs.iter().zip(self.wb.sheets.iter()).enumerate() {
            let on = i == self.sheet;
            c.round_rect(b.x, b.y, b.w, b.h, 6, if on { rgb(255, 255, 255) } else { rgb(228, 231, 235) });
            if on {
                c.fill_rect(b.x + 6, b.y + b.h - 3, b.w - 12, 3, ACCENT);
            }
            c.text_centered(b.x + b.w / 2, b.y + 5, &s.name, Size::Small, on, if on { ACCENT } else { INK });
        }
        button(c, plus, "+", false);
        // Sum / average of the selection, or a note.
        let msg = match &self.note {
            Some(n) => n.clone(),
            None => self.selection_stats(),
        };
        if !msg.is_empty() {
            let w = text_width(&msg, Size::Small, false);
            if w < t.x + t.w - (plus.x + plus.w) - 24 {
                c.text(t.x + t.w - 14 - w, t.y + 9, &msg, Size::Small, false, DIM);
            }
        }
    }

    fn selection_stats(&self) -> String {
        let (r0, c0, r1, c1) = self.sel();
        if (r0, c0) == (r1, c1) {
            return String::new();
        }
        let (mut sum, mut n, mut count) = (0.0, 0usize, 0usize);
        let keys: Vec<(u32, u32)> = self.sh().cells.range((r0, 0)..=(r1, u32::MAX)).map(|(k, _)| *k).filter(|(_, c)| (c0..=c1).contains(c)).collect();
        for (r, c) in keys {
            count += 1;
            if let Value::Num(x) = self.wb.value(self.sheet, r, c) {
                sum += x;
                n += 1;
            }
        }
        if n == 0 {
            return if count > 0 { format!("Count: {}", count) } else { String::new() };
        }
        format!("Sum: {}    Average: {}    Count: {}", general(sum), general(sum / n as f64), count)
    }

    fn menu_rect(&self, r: Rect) -> Option<(Menu, Rect)> {
        let m = self.menu?;
        let tool = match m {
            Menu::Format => Tool::Format,
            Menu::TextColor => Tool::TextColor,
            Menu::Fill => Tool::Fill,
        };
        let b = self.tool_rects(r).0.into_iter().find(|(t, _, _)| *t == tool)?.2;
        let (w, h) = match m {
            Menu::Format => (230, 7 * 34 + 12),
            _ => (5 * 34 + 20, 3 * 34 + 20 + 36),
        };
        let x = b.x.min(r.x + r.w - w - 8);
        Some((m, Rect::new(x, b.y + b.h + 4, w, h)))
    }

    const FORMATS: [&'static str; 7] = ["General", "Number   1,234.00", "Money   $1,234.00", "Percent   12%", "Date   11/20/2026", "More decimals", "Fewer decimals"];

    fn draw_menu(&self, c: &mut Canvas, r: Rect) {
        let Some((m, b)) = self.menu_rect(r) else { return };
        c.round_rect_alpha(b.x + 2, b.y + 4, b.w, b.h, 8, 0, 70);
        c.round_rect(b.x, b.y, b.w, b.h, 8, rgb(255, 255, 255));
        c.outline_rect(b.x, b.y, b.w, b.h, GRID);
        match m {
            Menu::Format => {
                for (i, label) in Self::FORMATS.iter().enumerate() {
                    let y = b.y + 6 + i as i32 * 34;
                    if i == 5 {
                        c.fill_rect(b.x + 8, y - 2, b.w - 16, 1, GRID);
                    }
                    c.text(b.x + 14, y + 8, label, Size::Small, i < 5, INK);
                }
            }
            _ => {
                for (i, col) in PALETTE.iter().enumerate() {
                    let (x, y) = (b.x + 10 + (i as i32 % 5) * 34, b.y + 10 + (i as i32 / 5) * 34);
                    c.round_rect(x, y, 28, 28, 4, *col);
                    c.outline_rect(x, y, 28, 28, GRID);
                }
                let y = b.y + 10 + 3 * 34;
                c.round_rect(b.x + 10, y, b.w - 20, 28, 4, BTN);
                c.text_centered(b.x + b.w / 2, y + 6, if m == Menu::Fill { "No fill" } else { "Automatic" }, Size::Small, true, INK);
            }
        }
    }

    fn rename_rects(r: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
        let w = 460.min(r.w - 40);
        let d = Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - 200) / 2, w, 200);
        let field = Rect::new(d.x + 20, d.y + 60, d.w - 40, 38);
        let ok = Rect::new(d.x + d.w - 20 - 110, d.y + d.h - 54, 110, 36);
        let cancel = Rect::new(ok.x - 10 - 100, ok.y, 100, 36);
        let delete = Rect::new(d.x + 20, ok.y, 130, 36);
        (d, field, ok, cancel, delete)
    }

    fn draw_rename(&self, c: &mut Canvas, r: Rect) {
        let Some(rn) = &self.rename else { return };
        c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 100);
        let (d, field, ok, cancel, delete) = Self::rename_rects(r);
        c.round_rect(d.x, d.y, d.w, d.h, 12, rgb(255, 255, 255));
        c.text(d.x + 20, d.y + 18, "Sheet name", Size::Medium, true, INK);
        rn.field.draw(c, field);
        c.round_rect(ok.x, ok.y, ok.w, ok.h, 8, ACCENT);
        c.text_centered(ok.x + ok.w / 2, ok.y + 9, "Rename", Size::Small, true, rgb(255, 255, 255));
        button(c, cancel, "Cancel", false);
        if self.wb.sheets.len() > 1 {
            c.round_rect(delete.x, delete.y, delete.w, delete.h, 8, rgb(252, 232, 230));
            c.text_centered(delete.x + delete.w / 2, delete.y + 9, "Delete sheet", Size::Small, true, rgb(200, 60, 50));
        }
    }

    // ---- editing ----------------------------------------------------------------

    fn start_edit(&mut self, initial: &str) {
        self.edit = Some(TextField::any(initial, 2000, self.shift));
    }

    fn commit(&mut self) {
        if let Some(f) = self.edit.take() {
            let old = self.wb.edit_text(self.sheet, self.cur.0, self.cur.1);
            if f.text != old {
                self.snapshot();
                self.wb.set_input(self.sheet, self.cur.0, self.cur.1, f.text.trim_end());
                self.changed();
            }
        }
    }

    fn move_by(&mut self, dr: i64, dc: i64, extend: bool, r: Rect) {
        let nr = (self.cur.0 as i64 + dr).clamp(0, MAX_ROWS as i64 - 1) as u32;
        let nc = (self.cur.1 as i64 + dc).clamp(0, MAX_COLS as i64 - 1) as u32;
        self.cur = (nr, nc);
        if !extend {
            self.anchor = self.cur;
        }
        self.note = None;
        self.ensure_visible(r);
    }

    fn style_sel(&mut self, f: &dyn Fn(&mut Style)) {
        self.snapshot();
        let (r0, c0, r1, c1) = self.sel();
        self.wb.restyle(self.sheet, r0, c0, r1, c1, f);
        self.dirty = true;
    }

    fn clear_sel(&mut self) {
        let (r0, c0, r1, c1) = self.sel();
        let keys: Vec<(u32, u32)> = self.sh().cells.range((r0, 0)..=(r1, u32::MAX)).map(|(k, _)| *k).filter(|(_, c)| (c0..=c1).contains(c)).collect();
        if keys.is_empty() {
            return;
        }
        self.snapshot();
        for (row, col) in keys {
            self.wb.set_input(self.sheet, row, col, "");
        }
        self.changed();
    }

    fn copy(&mut self) {
        let (r0, c0, r1, c1) = self.sel();
        let cells: Vec<((u32, u32), Cell)> = self
            .sh()
            .cells
            .range((r0, 0)..=(r1, u32::MAX))
            .filter(|((_, c), _)| (c0..=c1).contains(c))
            .map(|((r, c), cell)| ((r - r0, c - c0), cell.clone()))
            .take(100_000)
            .collect();
        let n = (r1 - r0 + 1) as u64 * (c1 - c0 + 1) as u64;
        self.clip = Some(Clip { origin: (r0, c0), cells, size: (r1 - r0 + 1, c1 - c0 + 1) });
        self.note = Some(format!("Copied {} cell{} - Ctrl+V to paste", n, if n == 1 { "" } else { "s" }));
    }

    /// Paste the copied cells at `at`, moving relative references in formulas.
    fn paste_at(&mut self, at: (u32, u32)) {
        let Some(clip) = &self.clip else { return };
        let (h, w) = clip.size;
        let cells: Vec<((u32, u32), Cell)> = clip.cells.clone();
        let origin = clip.origin;
        let s = self.sheet;
        let sh = &mut self.wb.sheets[s];
        // Clear the destination first.
        let keys: Vec<(u32, u32)> = sh.cells.range((at.0, 0)..=(at.0 + h - 1, u32::MAX)).map(|(k, _)| *k).filter(|(_, c)| (at.1..at.1 + w).contains(c)).collect();
        for k in keys {
            sh.cells.remove(&k);
        }
        for ((dr, dc), mut cell) in cells {
            let (r, c) = (at.0 + dr, at.1 + dc);
            if r >= MAX_ROWS || c >= MAX_COLS {
                continue;
            }
            if let Content::Formula(f, _) = &cell.content {
                cell.content = Content::Formula(shift_formula(f, r as i64 - (origin.0 + dr) as i64, c as i64 - (origin.1 + dc) as i64), None);
            }
            sh.cells.insert((r, c), cell);
        }
    }

    fn paste(&mut self) {
        if self.clip.is_none() {
            return;
        }
        self.snapshot();
        let (r0, c0, r1, c1) = self.sel();
        let (h, w) = self.clip.as_ref().unwrap().size;
        // A bigger selection that's a whole multiple of the copy gets it repeated.
        let (sh_, sw) = (r1 - r0 + 1, c1 - c0 + 1);
        let (ny, nx) = if sh_ % h == 0 && sw % w == 0 && (sh_ > h || sw > w) { (sh_ / h, sw / w) } else { (1, 1) };
        for y in 0..ny.min(1000) {
            for x in 0..nx.min(100) {
                self.paste_at((r0 + y * h, c0 + x * w));
            }
        }
        self.cur = (r0, c0);
        self.anchor = (r0 + ny * h - 1, c0 + nx * w - 1);
        self.note = None;
        self.changed();
    }

    /// Ctrl+D: copy the top row of the selection down through it.
    fn fill_down(&mut self) {
        let (r0, c0, r1, c1) = self.sel();
        if r1 == r0 {
            return;
        }
        let saved = self.clip.take();
        let (cur, anchor) = (self.cur, self.anchor);
        self.cur = (r0, c0);
        self.anchor = (r0, c1);
        self.copy();
        self.cur = (r0 + 1, c0);
        self.anchor = (r1, c1);
        self.paste();
        self.clip = saved;
        self.cur = cur;
        self.anchor = anchor;
        self.note = None;
    }

    fn do_undo(&mut self) {
        if let Some((sheets, s)) = self.undo.pop() {
            self.wb.sheets = sheets;
            self.sheet = s.min(self.wb.sheets.len() - 1);
            self.wb.recalc();
            self.dirty = true;
            self.note = Some(String::from("Undone"));
        }
    }

    fn tool(&mut self, t: Tool, r: Rect) -> SheetAction {
        self.commit();
        let toggle_menu = |me: &mut Self, m: Menu| me.menu = if me.menu == Some(m) { None } else { Some(m) };
        match t {
            Tool::Back => return SheetAction::Close,
            Tool::Save => return SheetAction::Save,
            Tool::Print => return SheetAction::Print,
            Tool::Undo => self.do_undo(),
            Tool::Bold => {
                let b = !self.sh().style(self.cur.0, self.cur.1).bold;
                self.style_sel(&|s| s.bold = b);
            }
            Tool::TextColor => toggle_menu(self, Menu::TextColor),
            Tool::Fill => toggle_menu(self, Menu::Fill),
            Tool::Format => toggle_menu(self, Menu::Format),
            Tool::Left | Tool::Center | Tool::Right => {
                let a = match t {
                    Tool::Left => -1,
                    Tool::Center => 1,
                    _ => 2,
                };
                let a = if self.sh().style(self.cur.0, self.cur.1).align == a { 0 } else { a };
                self.style_sel(&|s| s.align = a);
            }
            Tool::Wider | Tool::Narrower => {
                self.snapshot();
                let (_, c0, _, c1) = self.sel();
                for col in c0..=c1.min(c0 + 200) {
                    let w = self.sh().width(col);
                    let nw = if t == Tool::Wider { w + 3.0 } else { (w - 3.0).max(2.0) };
                    self.wb.sheets[self.sheet].widths.insert(col, nw);
                }
                self.ensure_visible(r);
            }
        }
        SheetAction::Redraw
    }

    fn menu_pick(&mut self, m: Menu, b: Rect, x: i32, y: i32) {
        match m {
            Menu::Format => {
                let i = ((y - b.y - 6) / 34) as usize;
                let cur = self.sh().style(self.cur.0, self.cur.1).fmt;
                let decimals = match &cur {
                    NumFmt::Number { decimals, .. } | NumFmt::Money { decimals, .. } | NumFmt::Percent { decimals } => *decimals,
                    _ => 2,
                };
                let bump = |f: &NumFmt, d: i8| -> NumFmt {
                    let nd = |x: u8| (x as i8 + d).clamp(0, 8) as u8;
                    match f.clone() {
                        NumFmt::Number { decimals, thousands } => NumFmt::Number { decimals: nd(decimals), thousands },
                        NumFmt::Money { symbol, decimals } => NumFmt::Money { symbol, decimals: nd(decimals) },
                        NumFmt::Percent { decimals } => NumFmt::Percent { decimals: nd(decimals) },
                        _ => NumFmt::Number { decimals: if d > 0 { 1 } else { 0 }, thousands: false },
                    }
                };
                let new = match i {
                    0 => Some(NumFmt::General),
                    1 => Some(NumFmt::Number { decimals: 2, thousands: true }),
                    2 => Some(NumFmt::Money { symbol: '$', decimals: 2 }),
                    3 => Some(NumFmt::Percent { decimals: 0 }),
                    4 => Some(NumFmt::Date),
                    5 => Some(bump(&cur, 1)),
                    6 => Some(bump(&cur, -1)),
                    _ => None,
                };
                let _ = decimals;
                if let Some(f) = new {
                    self.style_sel(&|s| s.fmt = f.clone());
                }
                if i < 5 {
                    self.menu = None;
                }
            }
            _ => {
                let pick = if y >= b.y + 10 + 3 * 34 {
                    Some(None)
                } else {
                    let (cx, cy) = ((x - b.x - 10) / 34, (y - b.y - 10) / 34);
                    if (0..5).contains(&cx) && (0..3).contains(&cy) {
                        Some(Some(PALETTE[(cy * 5 + cx) as usize]))
                    } else {
                        None
                    }
                };
                if let Some(col) = pick {
                    if m == Menu::Fill {
                        self.style_sel(&|s| s.fill = col);
                    } else {
                        self.style_sel(&|s| s.color = col.filter(|&c| c != 0));
                    }
                    self.menu = None;
                }
            }
        }
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64) -> SheetAction {
        if self.rename.is_some() {
            let (_, _, ok, cancel, delete) = Self::rename_rects(r);
            if ok.contains(x, y) {
                self.finish_rename();
            } else if cancel.contains(x, y) {
                self.rename = None;
            } else if delete.contains(x, y) && self.wb.sheets.len() > 1 {
                self.snapshot();
                self.wb.sheets.remove(self.sheet);
                self.sheet = self.sheet.min(self.wb.sheets.len() - 1);
                self.rename = None;
                self.cur = (0, 0);
                self.anchor = (0, 0);
                self.top = 0;
                self.left = 0;
                self.changed();
            }
            return SheetAction::Redraw;
        }
        if let Some((m, b)) = self.menu_rect(r) {
            if b.contains(x, y) {
                self.menu_pick(m, b, x, y);
                return SheetAction::Redraw;
            }
            self.menu = None;
            // Clicking another tool still works below.
        }
        let (tools, _) = self.tool_rects(r);
        if let Some((t, _, _)) = tools.iter().find(|(_, _, b)| b.contains(x, y)) {
            if *t == Tool::Undo && self.undo.is_empty() {
                return SheetAction::None;
            }
            return self.tool(*t, r);
        }
        let b = self.bar(r);
        if b.contains(x, y) && x > b.x + 126 {
            if self.edit.is_none() {
                let t = self.wb.edit_text(self.sheet, self.cur.0, self.cur.1);
                self.start_edit(&t);
            }
            return SheetAction::Redraw;
        }
        let (tabs, plus) = self.tab_rects(r);
        if plus.contains(x, y) {
            self.commit();
            self.snapshot();
            let name = self.wb.new_sheet_name();
            self.wb.sheets.push(Sheet::new(&name));
            self.switch_sheet(self.wb.sheets.len() - 1);
            return SheetAction::Redraw;
        }
        if let Some(i) = tabs.iter().position(|t| t.contains(x, y)) {
            self.commit();
            let double = matches!(self.last_tab, Some((j, t)) if j == i && now - t <= DOUBLE);
            self.last_tab = Some((i, now));
            self.switch_sheet(i);
            if double {
                let name = self.wb.sheets[i].name.clone();
                self.rename = Some(Rename { field: TextField::new(&name, 31) });
            }
            return SheetAction::Redraw;
        }
        let g = self.grid(r);
        if g.contains(x, y) {
            self.commit();
            // Column / row headers select whole columns / rows.
            if y < g.y + HH && x >= g.x + RW {
                if let Some(&(col, _, _)) = self.visible_cols(g).iter().find(|(_, cx, w)| x >= *cx && x < cx + w) {
                    // The whole column; the view stays where it is.
                    self.cur = (0, col);
                    self.anchor = (MAX_ROWS - 1, col);
                }
                return SheetAction::Redraw;
            }
            if x < g.x + RW && y >= g.y + HH {
                let row = self.top + ((y - g.y - HH) / RH) as u32;
                self.cur = (row, 0);
                self.anchor = (row, MAX_COLS - 1);
                return SheetAction::Redraw;
            }
            if let Some(cell) = self.cell_at(g, x, y) {
                let double = matches!(self.last_click, Some((c, t)) if c == cell && now - t <= DOUBLE);
                self.last_click = Some((cell, now));
                self.cur = cell;
                if !self.shift {
                    self.anchor = cell;
                }
                self.note = None;
                self.dragging = true;
                if double {
                    let t = self.wb.edit_text(self.sheet, cell.0, cell.1);
                    self.start_edit(&t);
                    self.dragging = false;
                }
                return SheetAction::Redraw;
            }
        }
        SheetAction::None
    }

    pub fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        if !self.dragging {
            return false;
        }
        let g = self.grid(r);
        // Past the edges: scroll.
        let x = x.clamp(g.x + RW, g.x + g.w - 1);
        if y > g.y + g.h - 4 {
            self.top = (self.top + 1).min(MAX_ROWS - 1);
        } else if y < g.y + HH && self.top > 0 {
            self.top -= 1;
        }
        let y = y.clamp(g.y + HH, g.y + g.h - 1);
        if let Some(cell) = self.cell_at(g, x, y) {
            if cell != self.cur {
                self.cur = cell;
                return true;
            }
        }
        false
    }

    pub fn on_release(&mut self) -> bool {
        self.dragging = false;
        false
    }

    pub fn on_wheel(&mut self, delta: i8) -> bool {
        let old = self.top;
        self.top = (self.top as i64 + delta as i64 * 3).clamp(0, MAX_ROWS as i64 - 1) as u32;
        old != self.top
    }

    fn switch_sheet(&mut self, i: usize) {
        if i != self.sheet {
            self.sheet = i;
            self.cur = (0, 0);
            self.anchor = (0, 0);
            self.top = 0;
            self.left = 0;
            self.note = None;
        }
    }

    fn finish_rename(&mut self) {
        let Some(rn) = self.rename.take() else { return };
        let name = String::from(rn.field.text.trim());
        if name.is_empty() || name == self.wb.sheets[self.sheet].name {
            return;
        }
        if self.wb.sheets.iter().enumerate().any(|(i, s)| i != self.sheet && s.name.eq_ignore_ascii_case(&name)) {
            self.note = Some(String::from("Another sheet already has that name"));
            return;
        }
        self.snapshot();
        self.wb.sheets[self.sheet].name = name;
        self.changed();
    }

    /// None = not handled here (never: the editor takes every key while it's open).
    pub fn on_key(&mut self, sc: u8, r: Rect) -> SheetAction {
        match sc {
            0x2A | 0x36 => self.shift = true,
            0xAA | 0xB6 => self.shift = false,
            0x1D => self.ctrl = true,
            0x9D => self.ctrl = false,
            _ => {}
        }
        if let Some(rn) = &mut self.rename {
            return match rn.field.on_key(sc) {
                TextEvent::Submit => {
                    self.finish_rename();
                    SheetAction::Redraw
                }
                TextEvent::Cancel => {
                    self.rename = None;
                    SheetAction::Redraw
                }
                TextEvent::Changed => SheetAction::Redraw,
                TextEvent::None => SheetAction::None,
            };
        }
        if sc & 0x80 != 0 || matches!(sc, 0x2A | 0x36 | 0x1D) {
            if let Some(f) = &mut self.edit {
                f.on_key(sc); // keep its shift state in step
            }
            return SheetAction::None;
        }
        if self.menu.is_some() && sc == 0x01 {
            self.menu = None;
            return SheetAction::Redraw;
        }
        if self.ctrl {
            self.commit();
            return match sc {
                0x1F => SheetAction::Save,  // S
                0x19 => SheetAction::Print, // P
                0x30 => self.tool(Tool::Bold, r),
                0x2C => {
                    self.do_undo(); // Z
                    SheetAction::Redraw
                }
                0x2E => {
                    self.copy(); // C
                    SheetAction::Redraw
                }
                0x2D => {
                    self.copy(); // X
                    self.clear_sel();
                    SheetAction::Redraw
                }
                0x2F => {
                    self.paste(); // V
                    self.ensure_visible(r);
                    SheetAction::Redraw
                }
                0x20 => {
                    self.fill_down(); // D
                    SheetAction::Redraw
                }
                0x47 => {
                    self.cur = (0, 0); // Home
                    self.anchor = self.cur;
                    self.ensure_visible(r);
                    SheetAction::Redraw
                }
                _ => SheetAction::None,
            };
        }
        if self.edit.is_some() {
            match sc {
                0x1C | 0x0F | 0x48 | 0x50 | 0x4B | 0x4D => {
                    self.commit();
                    let (dr, dc) = match sc {
                        0x1C | 0x50 => (1, 0),
                        0x48 => (-1, 0),
                        0x0F | 0x4D => (0, 1),
                        _ => (0, -1),
                    };
                    let (dr, dc) = if self.shift && sc == 0x1C { (-1, 0) } else if self.shift && sc == 0x0F { (0, -1) } else { (dr, dc) };
                    self.move_by(dr, dc, false, r);
                }
                0x01 => self.edit = None,
                _ => {
                    self.edit.as_mut().unwrap().on_key(sc);
                }
            }
            return SheetAction::Redraw;
        }
        let ext = self.shift;
        match sc {
            0x01 => return SheetAction::Close,
            0x48 => self.move_by(-1, 0, ext, r),
            0x50 => self.move_by(1, 0, ext, r),
            0x4B => self.move_by(0, -1, ext, r),
            0x4D => self.move_by(0, 1, ext, r),
            0x49 => self.move_by(-20, 0, ext, r),
            0x51 => self.move_by(20, 0, ext, r),
            0x47 => {
                self.cur.1 = 0;
                if !ext {
                    self.anchor = self.cur;
                }
                self.ensure_visible(r);
            }
            0x1C => self.move_by(if self.shift { -1 } else { 1 }, 0, false, r),
            0x0F => self.move_by(0, if self.shift { -1 } else { 1 }, false, r),
            0x3C => {
                let t = self.wb.edit_text(self.sheet, self.cur.0, self.cur.1);
                self.start_edit(&t); // F2
            }
            0x53 => self.clear_sel(), // Delete
            0x0E => {
                self.anchor = self.cur;
                self.start_edit(""); // Backspace: clear and type
            }
            _ => {
                // Typing starts editing with that character.
                let mut f = TextField::any("", 2000, self.shift);
                if matches!(f.on_key(sc), TextEvent::Changed) {
                    self.anchor = self.cur;
                    self.edit = Some(f);
                } else {
                    return SheetAction::None;
                }
            }
        }
        SheetAction::Redraw
    }

    /// Finish typing in the current cell (before saving or printing).
    pub fn finish_edit(&mut self) {
        self.commit();
        self.menu = None;
    }
}

fn intersect(a: Rect, b: Rect) -> Rect {
    let x0 = a.x.max(b.x);
    let y0 = a.y.max(b.y);
    let x1 = (a.x + a.w).min(b.x + b.w);
    let y1 = (a.y + a.h).min(b.y + b.h);
    Rect::new(x0, y0, (x1 - x0).max(0), (y1 - y0).max(0))
}

/// Pages of a sheet for printing, drawn at 100 pixels per inch: the used area split into
/// page-sized pieces (down, then across), with grid lines and the column letters and row
/// numbers left out, like Excel's default.
pub fn render_pages(wb: &Workbook, s: usize, page_w: i32, page_h: i32) -> Vec<Canvas> {
    let sh = &wb.sheets[s];
    let Some((last_r, last_c)) = sh.extent() else { return Vec::new() };
    let margin = 50;
    let (aw, ah) = (page_w - 2 * margin, page_h - 2 * margin);
    // Column groups that fit across a page.
    let mut groups: Vec<(u32, u32)> = Vec::new();
    let mut start = 0;
    let mut w = 0;
    for c in 0..=last_c {
        let cw = col_px(sh, c);
        if w + cw > aw && c > start {
            groups.push((start, c - 1));
            start = c;
            w = 0;
        }
        w += cw;
    }
    groups.push((start, last_c));
    let rows_per = (ah / RH).max(1) as u32;
    let mut pages = Vec::new();
    for &(c0, c1) in &groups {
        let mut r0 = 0;
        while r0 <= last_r {
            let r1 = (r0 + rows_per - 1).min(last_r);
            let mut canvas = Canvas::new(page_w as usize, page_h as usize);
            canvas.fill_rect(0, 0, page_w, page_h, rgb(255, 255, 255));
            let mut x = margin;
            for c in c0..=c1 {
                let cw = col_px(sh, c);
                for r in r0..=r1 {
                    let y = margin + (r - r0) as i32 * RH;
                    let st = sh.style(r, c);
                    if let Some(f) = st.fill {
                        canvas.fill_rect(x, y, cw, RH, f);
                    }
                    canvas.outline_rect(x, y, cw + 1, RH + 1, rgb(200, 200, 200));
                    let text = wb.display(s, r, c);
                    if text.is_empty() {
                        continue;
                    }
                    let value = wb.value(s, r, c);
                    let align = match st.align {
                        0 => match value {
                            Value::Num(_) => 2,
                            Value::Bool(_) | Value::Err(_) => 1,
                            _ => -1,
                        },
                        a => a,
                    };
                    let tw = text_width(&text, Size::Small, st.bold);
                    let tx = match align {
                        1 => x + (cw - tw) / 2,
                        2 => x + cw - 5 - tw,
                        _ => x + 4,
                    };
                    canvas.set_clip(Rect::new(x, y, cw, RH));
                    canvas.text(tx, y + 5, &text, Size::Small, st.bold, st.color.unwrap_or(0));
                    canvas.reset_clip();
                }
                x += cw;
            }
            pages.push(canvas);
            r0 = r1 + 1;
        }
    }
    pages
}

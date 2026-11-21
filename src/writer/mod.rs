//! Writer: a word processor in the spirit of Word. Pages on screen as they'll print, fonts
//! (Sans and Serif) in any size, bold/italic/underline/strike, colours and highlighting,
//! headings, alignment, bullets and numbering, indents, page breaks, undo, find-free simple
//! editing. Opens and saves Word files (.docx), opens plain text, prints and saves as PDF.

pub mod doc;
pub mod docx;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::cell::RefCell;
use spin::Mutex;

use crate::docshell::{DocEditor, EdAction, Kind, DIM, INK};
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::keys::{self, Key, Keys};
use crate::print::Paper;
use doc::{Align, Block, Doc, Layout, List, PStyle, Para, Style, PAGE_BREAK};

const ACCENT: u32 = rgb(43, 87, 184);
const TOOL_H: i32 = 100;
const STATUS_H: i32 = 28;
const GAP: i32 = 24;
const DOUBLE: u64 = 45;

fn icon(c: &mut Canvas, x: i32, y: i32) {
    c.round_rect(x, y, 26, 28, 4, ACCENT);
    c.text_centered(x + 13, y + 6, "W", Size::Small, true, rgb(255, 255, 255));
}

pub static KIND: Kind = Kind {
    app: "Writer",
    list_title: "Writer documents",
    new_label: "New document",
    exts: &["docx"],
    save_ext: "docx",
    keeps: &["docx"],
    accent: ACCENT,
    empty: ["No documents yet.", "Start one with New document, or plug in a USB drive that has Word (.docx) files."],
    extra: &[],
    icon,
};

/// Rich copy: the plain text it was copied as, and the paragraphs.
static RICH: Mutex<Option<(String, Vec<Para>)>> = Mutex::new(None);

const COLORS: [u32; 10] = [rgb(0, 0, 0), rgb(90, 96, 106), rgb(200, 30, 30), rgb(230, 120, 0), rgb(30, 140, 60), rgb(20, 120, 160), rgb(30, 70, 200), rgb(120, 50, 170), rgb(170, 60, 120), rgb(110, 70, 30)];
const HIGHLIGHTS: [u32; 6] = [rgb(255, 240, 0), rgb(140, 240, 120), rgb(120, 220, 255), rgb(255, 160, 220), rgb(255, 190, 110), rgb(210, 210, 210)];
const SIZES: [u16; 16] = [8, 9, 10, 11, 12, 14, 16, 18, 20, 24, 28, 32, 36, 48, 60, 72];
const ZOOMS: [i32; 7] = [50, 75, 90, 100, 125, 150, 200];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Tool {
    Back,
    Save,
    SaveAs,
    Print,
    Pdf,
    Undo,
    Redo,
    ZoomOut,
    ZoomIn,
    Style,
    Font,
    SizeDown,
    SizeUp,
    Bold,
    Italic,
    Underline,
    Strike,
    Color,
    Highlight,
    Left,
    Center,
    Right,
    Justify,
    Bullets,
    Numbers,
    Outdent,
    Indent,
    PageBreak,
    Picture,
    Table,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Menu {
    Style,
    Font,
    Color,
    Highlight,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    None,
    Type,
    Delete,
}

type Pos = (usize, usize);

pub struct WriterEd {
    doc: Doc,
    lay: RefCell<Option<Layout>>,
    cur: Pos,
    anchor: Option<Pos>,
    /// Style for the next typed character (after B/I/U with nothing selected).
    pending: Option<Style>,
    zoom: i32,
    scroll: i32,
    keys: Keys,
    undo: Vec<(Vec<Para>, Pos)>,
    redo: Vec<(Vec<Para>, Pos)>,
    last_edit: EditKind,
    dirty: bool,
    menu: Option<Menu>,
    dragging: bool,
    last_click: Option<(Pos, u64)>,
    want_x: Option<i32>,
    view_h: core::cell::Cell<i32>,
    /// Pictures decoded at the size last drawn: (picture, w, h).
    pics: RefCell<alloc::collections::BTreeMap<(u16, i32, i32), Option<alloc::rc::Rc<crate::image::Image>>>>,
    /// From a .txt file: the first save asks for a name (as a .docx).
    note: Option<String>,
}

pub fn new_doc() -> WriterEd {
    WriterEd {
        doc: Doc::new(),
        lay: RefCell::new(None),
        cur: (0, 0),
        anchor: None,
        pending: None,
        zoom: 100,
        scroll: 0,
        keys: Keys::default(),
        undo: Vec::new(),
        redo: Vec::new(),
        last_edit: EditKind::None,
        dirty: false,
        menu: None,
        dragging: false,
        last_click: None,
        want_x: None,
        view_h: core::cell::Cell::new(600),
        pics: RefCell::new(alloc::collections::BTreeMap::new()),
        note: None,
    }
}

pub fn load(name: &str, b: &[u8]) -> Result<WriterEd, String> {
    let mut ed = new_doc();
    if crate::docshell::ext_of(name) == "txt" {
        let (text, _) = crate::textedit::decode_text(b);
        ed.doc.paras = text
            .split('\n')
            .map(|l| {
                let t: Vec<char> = l.strip_suffix('\r').unwrap_or(l).chars().collect();
                let n = t.len();
                Para { text: t, st: alloc::vec![Style::NORMAL; n], p: PStyle::NORMAL, end: Style::NORMAL, row: None }
            })
            .collect();
        if ed.doc.paras.is_empty() {
            ed.doc.paras.push(Para::new(PStyle::NORMAL, Style::NORMAL));
        }
    } else {
        let (doc, note) = docx::read(b)?;
        ed.doc = doc;
        ed.note = note;
    }
    Ok(ed)
}

fn toolbar_row1() -> &'static [(Tool, &'static str)] {
    &[(Tool::Back, "< Documents"), (Tool::Save, "Save"), (Tool::SaveAs, "Save as"), (Tool::Print, "Print"), (Tool::Pdf, "Save as PDF"), (Tool::Undo, "Undo"), (Tool::Redo, "Redo"), (Tool::ZoomOut, "-"), (Tool::ZoomIn, "+")]
}

fn toolbar_row2() -> &'static [(Tool, &'static str)] {
    &[
        (Tool::Style, ""),
        (Tool::Font, ""),
        (Tool::SizeDown, "-"),
        (Tool::SizeUp, "+"),
        (Tool::Bold, "B"),
        (Tool::Italic, "I"),
        (Tool::Underline, "U"),
        (Tool::Strike, "S"),
        (Tool::Color, "A"),
        (Tool::Highlight, ""),
        (Tool::Left, ""),
        (Tool::Center, ""),
        (Tool::Right, ""),
        (Tool::Justify, ""),
        (Tool::Bullets, ""),
        (Tool::Numbers, ""),
        (Tool::Outdent, ""),
        (Tool::Indent, ""),
        (Tool::PageBreak, "Page break"),
        (Tool::Picture, "Picture"),
        (Tool::Table, "Table"),
    ]
}

impl WriterEd {
    // ---- layout helpers ------------------------------------------------------------

    fn relayout(&self) {
        *self.lay.borrow_mut() = Some(doc::layout(&self.doc));
    }

    fn with_lay<T>(&self, f: impl FnOnce(&Layout) -> T) -> T {
        if self.lay.borrow().is_none() {
            self.relayout();
        }
        f(self.lay.borrow().as_ref().unwrap())
    }

    fn changed(&mut self) {
        *self.lay.borrow_mut() = None;
        self.dirty = true;
    }

    /// Screen pixels per point x1000.
    fn mpp(&self) -> i32 {
        self.zoom * 4000 / 300
    }
    fn page_px(&self) -> (i32, i32) {
        (self.doc.page_w * self.mpp() / 1000, self.doc.page_h * self.mpp() / 1000)
    }
    fn area(r: Rect) -> Rect {
        Rect::new(r.x, r.y + TOOL_H, r.w, r.h - TOOL_H - STATUS_H)
    }
    /// Top-left of a page on screen.
    fn page_origin(&self, r: Rect, page: usize) -> (i32, i32) {
        let a = Self::area(r);
        let (pw, ph) = self.page_px();
        (a.x + ((a.w - pw) / 2).max(GAP), a.y + GAP + page as i32 * (ph + GAP) - self.scroll)
    }
    fn total_h(&self) -> i32 {
        let pages = self.with_lay(|l| l.pages) as i32;
        pages * (self.page_px().1 + GAP) + GAP
    }
    fn to_px(&self, v64: i32) -> i32 {
        (v64 as i64 * self.mpp() as i64 / 64000) as i32
    }

    /// Screen position of a text position: (x, top, height).
    fn caret_xy(&self, r: Rect, p: Pos) -> (i32, i32, i32) {
        self.with_lay(|lay| {
            let k = lay.line_of(p.0, p.1);
            let l = &lay.lines[k];
            let (ox, oy) = self.page_origin(r, l.page);
            let m = &self.doc.margin;
            let i = p.1.clamp(l.start, l.end) - l.start;
            let x = ox + self.to_px(m[0] * 64 + l.x + l.xs[i.min(l.xs.len() - 1)]);
            let y = oy + self.to_px(m[1] * 64 + l.y);
            (x, y, self.to_px(l.h).max(8))
        })
    }

    fn pos_at(&self, r: Rect, x: i32, y: i32) -> Pos {
        self.with_lay(|lay| {
            let (_, ph) = self.page_px();
            let a = Self::area(r);
            let page = ((y - a.y - GAP + self.scroll) / (ph + GAP)).clamp(0, lay.pages as i32 - 1) as usize;
            let (ox, oy) = self.page_origin(r, page);
            let m = &self.doc.margin;
            let mut best = None;
            // In a table: the cell under the pointer (its lowest line above the pointer).
            let ux = ((x - ox) as i64 * 64000 / self.mpp() as i64) as i32 - m[0] * 64;
            let uy = ((y - oy) as i64 * 64000 / self.mpp() as i64) as i32 - m[1] * 64;
            let in_row = lay.boxes.iter().any(|&(pg, bx, by, bw, bh)| pg == page && ux >= bx && ux < bx + bw && uy >= by && uy < by + bh);
            if in_row {
                // The row box under the pointer, then that cell's lowest line starting above it.
                let rb = lay.boxes.iter().find(|&&(pg, bx, by, bw, bh)| pg == page && ux >= bx && ux < bx + bw && uy >= by && uy < by + bh).copied();
                if let Some((_, _, by, _, bh)) = rb {
                    let in_cell = |l: &&doc::Line| l.page == page && l.y >= by && l.y < by + bh && l.cell.is_some_and(|(a, b)| ux >= a && ux < b);
                    best = lay.lines.iter().filter(in_cell).filter(|l| l.y <= uy).last().or_else(|| lay.lines.iter().find(in_cell));
                }
            }
            if best.is_none() {
                for l in lay.lines.iter().filter(|l| l.page == page) {
                    let top = oy + self.to_px(m[1] * 64 + l.y);
                    best = Some(l);
                    if y < top + self.to_px(l.h) {
                        break;
                    }
                }
            }
            let Some(l) = best.or_else(|| lay.lines.iter().rev().find(|l| l.page < page)).or(lay.lines.first()) else { return (0, 0) };
            let rel = ((x - ox) as i64 * 64000 / self.mpp() as i64) as i32 - m[0] * 64 - l.x;
            let mut i = l.start;
            for k in 0..(l.end - l.start) {
                let mid = (l.xs[k] + l.xs[k + 1]) / 2;
                if rel > mid {
                    i = l.start + k + 1;
                }
            }
            // Don't land after a line's trailing space or soft break (that's the next line).
            let para = &self.doc.paras[l.para];
            if i == l.end && l.end > l.start && l.end < para.text.len() {
                i = l.end - 1;
            }
            if para.is_page_break() {
                i = 0;
            }
            (l.para, i)
        })
    }

    fn selection(&self) -> Option<(Pos, Pos)> {
        let a = self.anchor?;
        if a == self.cur {
            return None;
        }
        Some(if a < self.cur { (a, self.cur) } else { (self.cur, a) })
    }

    /// The style typing would use now.
    fn cur_style(&self) -> Style {
        if let Some(s) = self.pending {
            return s;
        }
        match self.selection() {
            Some((a, _)) => {
                let p = &self.doc.paras[a.0];
                if a.1 < p.st.len() {
                    p.st[a.1]
                } else {
                    p.style_at(a.1)
                }
            }
            None => self.doc.paras[self.cur.0].style_at(self.cur.1),
        }
    }

    fn cur_pstyle(&self) -> PStyle {
        self.doc.paras[self.cur.0].p
    }

    // ---- editing ---------------------------------------------------------------------

    fn snapshot(&mut self, kind: EditKind) {
        if kind == EditKind::None || kind != self.last_edit {
            self.undo.push((self.doc.paras.clone(), self.cur));
            if self.undo.len() > 60 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.last_edit = kind;
    }

    fn delete_range(&mut self, a: Pos, b: Pos) {
        if a.0 == b.0 && self.doc.paras[a.0].row.is_some() {
            // In a table row keep the cell edges (tabs): clear only what's between them.
            let p = &mut self.doc.paras[a.0];
            let mut k = b.1;
            while k > a.1 {
                k -= 1;
                if p.text[k] != '\t' {
                    p.text.remove(k);
                    p.st.remove(k);
                }
            }
            self.cur = a;
            self.anchor = None;
            self.changed();
            return;
        }
        if a.0 == b.0 {
            let p = &mut self.doc.paras[a.0];
            p.text.drain(a.1..b.1);
            p.st.drain(a.1..b.1);
        } else {
            let tail_t: Vec<char> = self.doc.paras[b.0].text[b.1..].to_vec();
            let tail_s: Vec<Style> = self.doc.paras[b.0].st[b.1..].to_vec();
            let p = &mut self.doc.paras[a.0];
            p.text.truncate(a.1);
            p.st.truncate(a.1);
            p.text.extend(tail_t);
            p.st.extend(tail_s);
            self.doc.paras.drain(a.0 + 1..=b.0);
        }
        self.cur = a;
        self.anchor = None;
        self.changed();
    }

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        self.delete_range(a, b);
        true
    }

    fn insert_char(&mut self, ch: char) {
        let mut st = self.cur_style();
        if ch != doc::PICTURE {
            // Text typed next to a picture is text.
            st.img = 0;
        }
        self.delete_selection();
        let (pi, i) = self.cur;
        let p = &mut self.doc.paras[pi];
        if p.is_page_break() {
            return;
        }
        p.text.insert(i, ch);
        p.st.insert(i, st);
        self.cur.1 += 1;
        self.pending = None;
        self.changed();
    }

    fn split_para(&mut self) {
        let mut st = self.cur_style();
        st.img = 0;
        self.delete_selection();
        let (pi, i) = self.cur;
        let p = &mut self.doc.paras[pi];
        // Enter on an empty list item ends the list.
        if p.text.is_empty() && p.p.list != List::None {
            p.p.list = List::None;
            p.p.indent = 0;
            self.changed();
            return;
        }
        let t = p.text.split_off(i);
        let s = p.st.split_off(i);
        let mut np = p.p;
        let mut end = st;
        if p.p.block != Block::Normal && t.is_empty() {
            np.block = Block::Normal;
            end = Block::Normal.style(st);
        }
        let new = Para { text: t, st: s, p: np, end, row: None };
        self.doc.paras.insert(pi + 1, new);
        self.cur = (pi + 1, 0);
        self.pending = if self.doc.paras[pi + 1].text.is_empty() { Some(end) } else { None };
        self.changed();
    }

    fn insert_page_break(&mut self) {
        self.split_para();
        let (pi, _) = self.cur;
        let st = self.doc.paras[pi].end;
        self.doc.paras.insert(pi, Para { text: alloc::vec![PAGE_BREAK], st: alloc::vec![st], p: PStyle::NORMAL, end: st, row: None });
        self.cur = (pi + 1, 0);
        self.changed();
    }

    fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let (pi, i) = self.cur;
        if i > 0 {
            let p = &mut self.doc.paras[pi];
            p.text.remove(i - 1);
            p.st.remove(i - 1);
            self.cur.1 -= 1;
        } else {
            let p = &mut self.doc.paras[pi];
            if p.p.list != List::None {
                p.p.list = List::None;
            } else if p.p.indent > 0 {
                p.p.indent -= 1;
            } else if pi > 0 {
                let prev_len = self.doc.paras[pi - 1].text.len();
                if self.doc.paras[pi - 1].is_page_break() {
                    self.doc.paras.remove(pi - 1);
                    self.cur = (pi - 1, 0);
                } else {
                    self.delete_range((pi - 1, prev_len), (pi, 0));
                    return;
                }
            }
        }
        self.changed();
    }

    fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let (pi, i) = self.cur;
        let len = self.doc.paras[pi].text.len();
        if i < len {
            let p = &mut self.doc.paras[pi];
            p.text.remove(i);
            p.st.remove(i);
            self.changed();
        } else if pi + 1 < self.doc.paras.len() {
            self.delete_range((pi, len), (pi + 1, 0));
        }
    }

    /// Change the style of the selected characters (or what typing will use).
    fn restyle(&mut self, f: impl Fn(&mut Style)) {
        match self.selection() {
            Some((a, b)) => {
                self.snapshot(EditKind::None);
                for pi in a.0..=b.0 {
                    let p = &mut self.doc.paras[pi];
                    let from = if pi == a.0 { a.1 } else { 0 };
                    let to = if pi == b.0 { b.1 } else { p.st.len() };
                    for s in &mut p.st[from..to] {
                        f(s);
                    }
                    if to == p.st.len() || p.st.is_empty() {
                        f(&mut p.end);
                    }
                }
                self.changed();
            }
            None => {
                let mut s = self.cur_style();
                f(&mut s);
                self.pending = Some(s);
                let p = &mut self.doc.paras[self.cur.0];
                if p.text.is_empty() {
                    p.end = s;
                }
            }
        }
    }

    /// Change the paragraphs touched by the selection (or the cursor's).
    fn repara(&mut self, f: impl Fn(&mut Para)) {
        self.snapshot(EditKind::None);
        let (a, b) = self.selection().unwrap_or((self.cur, self.cur));
        for pi in a.0..=b.0 {
            f(&mut self.doc.paras[pi]);
        }
        self.pending = None;
        self.changed();
    }

    fn toggle(&mut self, get: fn(&Style) -> bool, set: fn(&mut Style, bool)) {
        let on = !get(&self.cur_style());
        self.restyle(move |s| set(s, on));
    }

    fn selected_paras(&self) -> Vec<Para> {
        let Some((a, b)) = self.selection() else { return Vec::new() };
        let mut out = Vec::new();
        for pi in a.0..=b.0 {
            let p = &self.doc.paras[pi];
            let from = if pi == a.0 { a.1 } else { 0 };
            let to = if pi == b.0 { b.1 } else { p.text.len() };
            out.push(Para { text: p.text[from..to].to_vec(), st: p.st[from..to].to_vec(), p: p.p, end: p.style_at(to), row: None });
        }
        out
    }

    fn copy(&self) {
        let paras = self.selected_paras();
        if paras.is_empty() {
            return;
        }
        let plain: String = paras.iter().map(|p| p.text.iter().filter(|&&c| c != PAGE_BREAK).collect::<String>()).collect::<Vec<_>>().join("\n");
        keys::copy(&plain);
        *RICH.lock() = Some((plain, paras));
    }

    fn paste(&mut self) {
        let text = keys::paste();
        if text.is_empty() {
            return;
        }
        self.snapshot(EditKind::None);
        let rich = RICH.lock().clone().filter(|(p, _)| *p == text).map(|(_, v)| v);
        let st = self.cur_style();
        let paras: Vec<Para> = match rich {
            Some(v) => v,
            None => text
                .split('\n')
                .map(|l| {
                    let t: Vec<char> = l.chars().filter(|&c| c != '\r').collect();
                    let n = t.len();
                    Para { text: t, st: alloc::vec![st; n], p: PStyle::NORMAL, end: st, row: None }
                })
                .collect(),
        };
        self.delete_selection();
        let n = paras.len();
        for (k, p) in paras.into_iter().enumerate() {
            if k > 0 {
                self.split_para();
                if let Some(np) = self.doc.paras.get_mut(self.cur.0) {
                    np.p = p.p;
                }
            }
            let (pi, i) = self.cur;
            let dst = &mut self.doc.paras[pi];
            let m = p.text.len();
            for (j, (ch, s)) in p.text.into_iter().zip(p.st).enumerate() {
                dst.text.insert(i + j, ch);
                dst.st.insert(i + j, s);
            }
            self.cur.1 += m;
            let _ = n;
        }
        self.changed();
    }

    fn do_undo(&mut self) {
        if let Some((paras, cur)) = self.undo.pop() {
            self.redo.push((core::mem::replace(&mut self.doc.paras, paras), self.cur));
            self.cur = cur;
            self.fix_cursor();
            self.anchor = None;
            self.last_edit = EditKind::None;
            self.changed();
        }
    }

    fn do_redo(&mut self) {
        if let Some((paras, cur)) = self.redo.pop() {
            self.undo.push((core::mem::replace(&mut self.doc.paras, paras), self.cur));
            self.cur = cur;
            self.fix_cursor();
            self.anchor = None;
            self.last_edit = EditKind::None;
            self.changed();
        }
    }

    fn fix_cursor(&mut self) {
        let pi = self.cur.0.min(self.doc.paras.len() - 1);
        self.cur = (pi, self.cur.1.min(self.doc.paras[pi].text.len()));
    }

    fn scroll_to_cursor(&mut self, r: Rect) {
        let a = Self::area(r);
        let (_, y, h) = self.caret_xy(r, self.cur);
        if y < a.y + 10 {
            self.scroll -= a.y + 10 - y;
        } else if y + h > a.y + a.h - 10 {
            self.scroll += y + h - (a.y + a.h - 10);
        }
        self.clamp_scroll(r);
    }

    fn clamp_scroll(&mut self, r: Rect) {
        let a = Self::area(r);
        self.scroll = self.scroll.clamp(0, (self.total_h() - a.h).max(0));
    }

    fn move_vert(&mut self, r: Rect, dy: i32) {
        let (x, y, h) = self.caret_xy(r, self.cur);
        let x = *self.want_x.get_or_insert(x);
        let target = if dy < 0 { y - 4 } else { y + h + 4 };
        let target = target + if dy.abs() > 1 { dy } else { 0 };
        self.cur = self.pos_at(r, x, target);
    }

    fn word_at(&self, p: Pos) -> (Pos, Pos) {
        let t = &self.doc.paras[p.0].text;
        let is_w = |c: char| c.is_alphanumeric() || c == '\'' || c == '_';
        let (mut a, mut b) = (p.1.min(t.len()), p.1.min(t.len()));
        while a > 0 && is_w(t[a - 1]) {
            a -= 1;
        }
        while b < t.len() && is_w(t[b]) {
            b += 1;
        }
        ((p.0, a), (p.0, b))
    }

    // ---- toolbar ------------------------------------------------------------------------

    fn tool_rects(&self, r: Rect) -> Vec<(Tool, Rect)> {
        let mut v = Vec::new();
        let mut x = r.x + 10;
        let y1 = r.y + 7;
        for &(t, label) in toolbar_row1() {
            let w = match t {
                Tool::ZoomOut | Tool::ZoomIn => 34,
                _ => text_width(label, Size::Small, true) + 24,
            };
            if t == Tool::Undo || t == Tool::ZoomOut {
                x += 14;
            }
            if t == Tool::ZoomIn {
                x += 64; // the zoom percentage sits between - and +
            }
            v.push((t, Rect::new(x, y1, w, 34)));
            x += w + 6;
        }
        let mut x = r.x + 10;
        let y2 = r.y + 53;
        for &(t, label) in toolbar_row2() {
            let w = match t {
                Tool::Style => 128,
                Tool::Font => 84,
                Tool::PageBreak | Tool::Picture => text_width(label, Size::Small, true) + 24,
                Tool::Table => 84,
                _ => 34,
            };
            if matches!(t, Tool::Bold | Tool::Color | Tool::Left | Tool::Bullets | Tool::PageBreak) {
                x += 12;
            }
            if t == Tool::SizeUp {
                x += 40;
            }
            v.push((t, Rect::new(x, y2, w, 34)));
            x += w + 4;
        }
        v
    }

    fn menu_items(&self, m: Menu) -> usize {
        match m {
            Menu::Style => Block::ALL.len(),
            Menu::Font => 2,
            Menu::Color => COLORS.len(),
            Menu::Highlight => HIGHLIGHTS.len() + 1,
        }
    }

    fn menu_rect(&self, r: Rect, m: Menu) -> Rect {
        let tool = match m {
            Menu::Style => Tool::Style,
            Menu::Font => Tool::Font,
            Menu::Color => Tool::Color,
            Menu::Highlight => Tool::Highlight,
        };
        let b = self.tool_rects(r).into_iter().find(|(t, _)| *t == tool).unwrap().1;
        let n = self.menu_items(m) as i32;
        match m {
            Menu::Style | Menu::Font => Rect::new(b.x, b.y + b.h + 4, 200, n * 40 + 12),
            _ => Rect::new(b.x, b.y + b.h + 4, 5 * 38 + 16, ((n + 4) / 5) * 38 + 16),
        }
    }

    fn menu_item_rect(&self, r: Rect, m: Menu, i: usize) -> Rect {
        let b = self.menu_rect(r, m);
        match m {
            Menu::Style | Menu::Font => Rect::new(b.x + 6, b.y + 6 + i as i32 * 40, b.w - 12, 38),
            _ => Rect::new(b.x + 8 + (i % 5) as i32 * 38, b.y + 8 + (i / 5) as i32 * 38, 34, 34),
        }
    }

    fn press_tool(&mut self, t: Tool, r: Rect) -> EdAction {
        let toggle_menu = |s: &mut Self, m: Menu| {
            s.menu = if s.menu == Some(m) { None } else { Some(m) };
        };
        match t {
            Tool::Back => return EdAction::Close,
            Tool::Save => return EdAction::Save,
            Tool::SaveAs => return EdAction::SaveAs,
            Tool::Print => return EdAction::Print,
            Tool::Pdf => {
                return match self.to_pdf() {
                    Ok(bytes) => EdAction::Export { ext: "pdf", bytes },
                    Err(e) => EdAction::Message(e),
                }
            }
            Tool::Undo => self.do_undo(),
            Tool::Redo => self.do_redo(),
            Tool::ZoomOut | Tool::ZoomIn => {
                let i = ZOOMS.iter().position(|&z| z >= self.zoom).unwrap_or(3);
                let i = if t == Tool::ZoomIn { (i + 1).min(ZOOMS.len() - 1) } else { i.saturating_sub(1) };
                let frac = self.scroll as i64 * 1000 / self.total_h().max(1) as i64;
                self.zoom = ZOOMS[i];
                self.scroll = (frac * self.total_h() as i64 / 1000) as i32;
                self.clamp_scroll(r);
            }
            Tool::Style => toggle_menu(self, Menu::Style),
            Tool::Font => toggle_menu(self, Menu::Font),
            Tool::Color => toggle_menu(self, Menu::Color),
            Tool::Highlight => toggle_menu(self, Menu::Highlight),
            Tool::SizeDown | Tool::SizeUp => {
                let up = t == Tool::SizeUp;
                self.restyle(move |s| {
                    let pt = s.size / 2;
                    let next = if up { SIZES.iter().find(|&&z| z > pt).copied().unwrap_or(pt + 12) } else { SIZES.iter().rev().find(|&&z| z < pt).copied().unwrap_or(pt.max(2) - 1) };
                    s.size = next.clamp(6, 144) * 2;
                });
            }
            Tool::Bold => self.toggle(|s| s.bold, |s, v| s.bold = v),
            Tool::Italic => self.toggle(|s| s.italic, |s, v| s.italic = v),
            Tool::Underline => self.toggle(|s| s.underline, |s, v| s.underline = v),
            Tool::Strike => self.toggle(|s| s.strike, |s, v| s.strike = v),
            Tool::Left | Tool::Center | Tool::Right | Tool::Justify => {
                let a = match t {
                    Tool::Left => Align::Left,
                    Tool::Center => Align::Center,
                    Tool::Right => Align::Right,
                    _ => Align::Justify,
                };
                self.repara(move |p| p.p.align = a);
            }
            Tool::Bullets | Tool::Numbers => {
                let l = if t == Tool::Bullets { List::Bullet } else { List::Number };
                let on = self.cur_pstyle().list != l;
                self.repara(move |p| p.p.list = if on { l } else { List::None });
            }
            Tool::Indent => self.repara(|p| p.p.indent = (p.p.indent + 1).min(8)),
            Tool::Outdent => self.repara(|p| p.p.indent = p.p.indent.saturating_sub(1)),
            Tool::Table => {
                self.table_button();
                self.scroll_to_cursor(r);
            }
            Tool::Picture => return EdAction::Pick { purpose: 1, exts: &["jpg", "jpeg", "png"], title: "Choose a picture to put in", many: false },
            Tool::PageBreak => {
                self.snapshot(EditKind::None);
                self.insert_page_break();
                self.scroll_to_cursor(r);
            }
        }
        EdAction::Redraw
    }

    fn pick_menu(&mut self, m: Menu, i: usize) {
        self.menu = None;
        match m {
            Menu::Style => {
                let b = Block::ALL[i];
                self.repara(move |p| {
                    p.p.block = b;
                    for s in p.st.iter_mut() {
                        *s = b.style(*s);
                    }
                    p.end = b.style(p.end);
                    if b != Block::Normal {
                        p.p.list = List::None;
                    }
                });
            }
            Menu::Font => {
                let serif = i == 1;
                self.restyle(move |s| s.serif = serif);
            }
            Menu::Color => {
                let c = COLORS[i];
                self.restyle(move |s| s.color = c);
            }
            Menu::Highlight => {
                let h = HIGHLIGHTS.get(i).copied();
                self.restyle(move |s| s.highlight = h);
            }
        }
    }

    // ---- drawing --------------------------------------------------------------------------

    fn draw_tools(&self, c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y, r.w, TOOL_H, rgb(244, 246, 250));
        c.fill_rect(r.x, r.y + TOOL_H - 1, r.w, 1, rgb(220, 224, 232));
        let st = self.cur_style();
        let ps = self.cur_pstyle();
        for (t, b) in self.tool_rects(r) {
            let label = toolbar_row1().iter().chain(toolbar_row2()).find(|x| x.0 == t).map(|x| x.1).unwrap_or("");
            let label = if t == Tool::Table && self.in_table() { "Add row" } else { label };
            let on = match t {
                Tool::Bold => st.bold,
                Tool::Italic => st.italic,
                Tool::Underline => st.underline,
                Tool::Strike => st.strike,
                Tool::Left => ps.align == Align::Left,
                Tool::Center => ps.align == Align::Center,
                Tool::Right => ps.align == Align::Right,
                Tool::Justify => ps.align == Align::Justify,
                Tool::Bullets => ps.list == List::Bullet,
                Tool::Numbers => ps.list == List::Number,
                _ => false,
            };
            let primary = t == Tool::Save && self.dirty;
            let bg = if primary {
                ACCENT
            } else if on {
                rgb(208, 222, 246)
            } else {
                rgb(255, 255, 255)
            };
            c.round_rect(b.x, b.y, b.w, b.h, 7, rgb(216, 220, 228));
            c.round_rect(b.x + 1, b.y + 1, b.w - 2, b.h - 2, 6, bg);
            let ink = if primary { rgb(255, 255, 255) } else { INK };
            let (cx, cy) = (b.x + b.w / 2, b.y + b.h / 2);
            match t {
                Tool::Style => {
                    c.text(b.x + 10, b.y + 9, ps.block.name(), Size::Small, false, ink);
                    c.triangle([(b.x + b.w - 18, cy - 2), (b.x + b.w - 8, cy - 2), (b.x + b.w - 13, cy + 4)], DIM);
                }
                Tool::Font => {
                    let face = crate::font::Face::new(st.serif, false, false);
                    let name = if st.serif { "Serif" } else { "Sans" };
                    c.text_face((b.x + 10) * 64, b.y + 23, name, face, 15, ink);
                    c.triangle([(b.x + b.w - 18, cy - 2), (b.x + b.w - 8, cy - 2), (b.x + b.w - 13, cy + 4)], DIM);
                }
                Tool::Bold => {
                    c.text_centered(cx, b.y + 8, "B", Size::Medium, true, ink);
                }
                Tool::Italic => {
                    c.text_face((cx - 4) * 64, b.y + 24, "I", crate::font::Face::SerifItalic, 19, ink);
                }
                Tool::Underline => {
                    c.text_centered(cx, b.y + 7, "U", Size::Medium, false, ink);
                    c.fill_rect(cx - 7, b.y + 27, 14, 2, ink);
                }
                Tool::Strike => {
                    c.text_centered(cx, b.y + 7, "S", Size::Medium, false, ink);
                    c.fill_rect(cx - 8, cy, 16, 2, ink);
                }
                Tool::Color => {
                    c.text_centered(cx, b.y + 5, "A", Size::Medium, true, ink);
                    c.fill_rect(b.x + 7, b.y + 25, b.w - 14, 4, st.color);
                }
                Tool::Highlight => {
                    c.round_rect(cx - 8, b.y + 7, 16, 14, 3, st.highlight.unwrap_or(HIGHLIGHTS[0]));
                    c.fill_rect(cx - 2, b.y + 20, 4, 8, rgb(90, 90, 90));
                }
                Tool::Left | Tool::Center | Tool::Right | Tool::Justify => {
                    for k in 0..4 {
                        let w = if k % 2 == 1 && t != Tool::Justify { 10 } else { 16 };
                        let x0 = match t {
                            Tool::Center => cx - w / 2,
                            Tool::Right => cx + 8 - w,
                            _ => cx - 8,
                        };
                        c.fill_rect(x0, b.y + 9 + k * 5, w, 2, ink);
                    }
                }
                Tool::Bullets | Tool::Numbers => {
                    for k in 0..3 {
                        let y = b.y + 9 + k * 6;
                        if t == Tool::Bullets {
                            c.circle(cx - 7, y + 1, 2, ink);
                        } else {
                            c.text_face((cx - 11) * 64, y + 4, &format!("{}", k + 1), crate::font::Face::SansBold, 8, ink);
                        }
                        c.fill_rect(cx - 2, y, 11, 2, ink);
                    }
                }
                Tool::Indent | Tool::Outdent => {
                    for k in 0..4 {
                        c.fill_rect(cx - 2, b.y + 9 + k * 5, 11, 2, ink);
                    }
                    if t == Tool::Indent {
                        c.triangle([(cx - 10, b.y + 12), (cx - 10, b.y + 22), (cx - 4, b.y + 17)], ink);
                    } else {
                        c.triangle([(cx - 4, b.y + 12), (cx - 4, b.y + 22), (cx - 10, b.y + 17)], ink);
                    }
                }
                Tool::Undo | Tool::Redo => {
                    c.text_centered(cx, b.y + 9, label, Size::Small, true, ink);
                }
                _ => {
                    c.text_centered(cx, b.y + 9, label, Size::Small, true, ink);
                }
            }
            if t == Tool::SizeDown {
                c.text_centered(b.x + b.w + 22, b.y + 9, &format!("{}", st.size / 2), Size::Small, true, INK);
            }
            if t == Tool::ZoomOut {
                c.text_centered(b.x + b.w + 35, b.y + 9, &format!("{}%", self.zoom), Size::Small, true, INK);
            }
        }
        // Open menu.
        if let Some(m) = self.menu {
            let mr = self.menu_rect(r, m);
            c.shadow(mr.x, mr.y + 4, mr.w, mr.h, 10, 14, 80);
            c.round_rect(mr.x, mr.y, mr.w, mr.h, 10, rgb(255, 255, 255));
            for i in 0..self.menu_items(m) {
                let b = self.menu_item_rect(r, m, i);
                match m {
                    Menu::Style => {
                        let blk = Block::ALL[i];
                        if blk == ps.block {
                            c.round_rect(b.x, b.y, b.w, b.h, 6, rgb(226, 235, 250));
                        }
                        let s = blk.style(st);
                        let px = (s.size / 2).min(20) + 2;
                        c.text_face((b.x + 10) * 64, b.y + 26, blk.name(), s.face(), px, s.color);
                    }
                    Menu::Font => {
                        let serif = i == 1;
                        if serif == st.serif {
                            c.round_rect(b.x, b.y, b.w, b.h, 6, rgb(226, 235, 250));
                        }
                        c.text_face((b.x + 10) * 64, b.y + 26, if serif { "Serif (like Times)" } else { "Sans (like Arial)" }, crate::font::Face::new(serif, false, false), 16, INK);
                    }
                    Menu::Color => {
                        c.round_rect(b.x, b.y, b.w, b.h, 6, COLORS[i]);
                        if COLORS[i] == st.color {
                            c.round_rect(b.x + 12, b.y + 12, 10, 10, 5, rgb(255, 255, 255));
                        }
                    }
                    Menu::Highlight => {
                        if i < HIGHLIGHTS.len() {
                            c.round_rect(b.x, b.y, b.w, b.h, 6, HIGHLIGHTS[i]);
                        } else {
                            c.round_rect(b.x, b.y, b.w, b.h, 6, rgb(240, 240, 240));
                            c.fill_rect(b.x + 6, b.y + 16, 22, 2, rgb(200, 40, 40));
                        }
                    }
                }
            }
        }
    }

    /// Draw one page with its top-left at (ox, oy), at `mpp` pixels per point x1000.
    fn draw_page(&self, c: &mut Canvas, lay: &Layout, page: usize, ox: i32, oy: i32, mpp: i32, sel: Option<(Pos, Pos)>, clip: Rect, screen: bool) {
        let m = &self.doc.margin;
        let px = |v64: i32| (v64 as i64 * mpp as i64 / 64000) as i32;
        let mut buf = [0u8; 4];
        // Table cell borders.
        let thin = (mpp / 1400).max(1);
        for &(_, bx, by, bw, bh) in lay.boxes.iter().filter(|b| b.0 == page) {
            let (x0, y0) = (ox + px(m[0] * 64 + bx), oy + px(m[1] * 64 + by));
            let (x1, y1) = (ox + px(m[0] * 64 + bx + bw), oy + px(m[1] * 64 + by + bh));
            let col = rgb(120, 126, 136);
            c.fill_rect(x0, y0, x1 - x0 + thin, thin, col);
            c.fill_rect(x0, y1, x1 - x0 + thin, thin, col);
            c.fill_rect(x0, y0, thin, y1 - y0, col);
            c.fill_rect(x1, y0, thin, y1 - y0, col);
        }
        for l in lay.lines.iter().filter(|l| l.page == page) {
            let top = oy + px(m[1] * 64 + l.y);
            let lh = px(l.h).max(1);
            if top > clip.y + clip.h || top + lh < clip.y {
                continue;
            }
            let base = oy + px(m[1] * 64 + l.y + l.base);
            let lx = m[0] * 64 + l.x;
            let p = &self.doc.paras[l.para];
            if p.is_page_break() {
                if screen {
                    let y = top + lh / 2;
                    let (x0, x1) = (ox + px(m[0] * 64), ox + px((self.doc.page_w - m[2]) * 64));
                    let mut x = x0;
                    while x < x1 {
                        c.fill_rect(x, y, 6, 1, rgb(150, 160, 175));
                        x += 10;
                    }
                }
                continue;
            }
            if let Some((mk, mx)) = &l.marker {
                let st = p.style_at(0);
                let gpx = ((st.size as i32 * mpp + 1000) / 2000).max(1) as u16;
                c.text_face((ox * 64) + px(m[0] * 64 + mx) * 64, base, mk, st.face(), gpx, st.color);
            }
            for i in l.start..l.end {
                let ch = p.text[i];
                let st = p.st[i];
                let x0 = ox + px(lx + l.xs[i - l.start]);
                let x1 = ox + px(lx + l.xs[i - l.start + 1]);
                let in_sel = sel.is_some_and(|(a, b)| (l.para, i) >= a && (l.para, i) < b);
                if let Some(h) = st.highlight {
                    c.fill_rect(x0, top, x1 - x0, lh, h);
                }
                if in_sel {
                    c.fill_rect(x0, top, (x1 - x0).max(3), lh, rgb(190, 212, 248));
                }
                if st.img != 0 && ch == doc::PICTURE {
                    let ph = px(self.doc.image_of(&st).map(|im| im.h * 64).unwrap_or(0));
                    self.draw_picture(c, st.img, x0, base - ph, x1 - x0, ph);
                    if in_sel {
                        c.fill_rect_alpha(x0, base - ph, x1 - x0, ph, rgb(60, 110, 220), 90);
                    }
                } else if ch != ' ' && ch != '\t' && ch != '\n' {
                    let gpx = ((st.size as i32 * mpp + 1000) / 2000).max(1) as u16;
                    let x64 = (ox * 64) + ((lx + l.xs[i - l.start]) as i64 * mpp as i64 * 64 / 64000) as i32;
                    c.text_face(x64, base, ch.encode_utf8(&mut buf), st.face(), gpx, st.color);
                }
                let thick = ((st.size as i32 * mpp) / 30000).max(1);
                if st.underline {
                    c.fill_rect(x0, base + thick + 1, x1 - x0, thick, st.color);
                }
                if st.strike {
                    c.fill_rect(x0, base - px(st.size as i32 * 32 * 30 / 100), x1 - x0, thick, st.color);
                }
            }
            // A selected paragraph end shows as a small block.
            if sel.is_some_and(|(a, b)| (l.para, l.end) >= a && (l.para, l.end) < b) && l.end == p.text.len() {
                let x0 = ox + px(lx + *l.xs.last().unwrap());
                c.fill_rect(x0, top, 5, lh, rgb(190, 212, 248));
            }
        }
    }

    fn word_count(&self) -> usize {
        self.doc.paras.iter().map(|p| p.text.split(|c| c.is_whitespace() || *c == PAGE_BREAK).filter(|w| !w.is_empty()).count()).sum()
    }

    // ---- PDF ------------------------------------------------------------------------------

    pub fn to_pdf(&self) -> Result<Vec<u8>, String> {
        let lay = doc::layout(&self.doc);
        let mut pdf = crate::pdf::write::DocPdf::new(self.doc.page_w, self.doc.page_h);
        let m = self.doc.margin;
        for page in 0..lay.pages {
            pdf.begin_page();
            for &(_, bx, by, bw, bh) in lay.boxes.iter().filter(|b| b.0 == page) {
                let (x, y) = ((m[0] * 64 + bx) as f32 / 64.0, (m[1] * 64 + by) as f32 / 64.0);
                let (w, h) = (bw as f32 / 64.0, bh as f32 / 64.0);
                let t = 0.5;
                let col = 0x787E88;
                pdf.rect(x, y, w + t, t, col);
                pdf.rect(x, y + h, w + t, t, col);
                pdf.rect(x, y, t, h, col);
                pdf.rect(x + w, y, t, h, col);
            }
            for l in lay.lines.iter().filter(|l| l.page == page) {
                let p = &self.doc.paras[l.para];
                if p.is_page_break() {
                    continue;
                }
                let base = (m[1] * 64 + l.y + l.base) as f32 / 64.0;
                let lx = m[0] * 64 + l.x;
                if let Some((mk, mx)) = &l.marker {
                    let st = p.style_at(0);
                    pdf.text(((m[0] * 64 + mx) as f32) / 64.0, base, mk, st.face(), st.size as f32 / 2.0, st.color);
                }
                // Highlights first (under the text), then words.
                for i in l.start..l.end {
                    let st = p.st[i];
                    let (x0, x1) = ((lx + l.xs[i - l.start]) as f32 / 64.0, (lx + l.xs[i - l.start + 1]) as f32 / 64.0);
                    if let Some(h) = st.highlight {
                        pdf.rect(x0, (m[1] * 64 + l.y) as f32 / 64.0, x1 - x0, l.h as f32 / 64.0, h);
                    }
                }
                let mut i = l.start;
                while i < l.end {
                    let st = p.st[i];
                    if let Some(im) = self.doc.image_of(&st) {
                        let x = (lx + l.xs[i - l.start]) as f32 / 64.0;
                        pdf.picture(&im.data, x, base - im.h as f32, im.w as f32, im.h as f32);
                        i += 1;
                        continue;
                    }
                    if matches!(p.text[i], ' ' | '\t' | '\n') {
                        i += 1;
                        continue;
                    }
                    let mut j = i;
                    while j < l.end && p.st[j] == st && !matches!(p.text[j], ' ' | '\t' | '\n') {
                        j += 1;
                    }
                    let word: String = p.text[i..j].iter().collect();
                    let x = (lx + l.xs[i - l.start]) as f32 / 64.0;
                    pdf.text(x, base, &word, st.face(), st.size as f32 / 2.0, st.color);
                    let x1 = (lx + l.xs[j - l.start]) as f32 / 64.0;
                    let t = (st.size as f32 / 2.0 / 18.0).max(0.5);
                    if st.underline {
                        pdf.rect(x, base + t * 1.5, x1 - x, t, st.color);
                    }
                    if st.strike {
                        pdf.rect(x, base - st.size as f32 / 2.0 * 0.3, x1 - x, t, st.color);
                    }
                    i = j;
                }
            }
            pdf.end_page();
        }
        Ok(pdf.finish())
    }
}

impl DocEditor for WriterEd {
    fn draw(&self, c: &mut Canvas, r: Rect) {
        let a = Self::area(r);
        c.fill_rect(a.x, a.y, a.w, a.h, rgb(230, 232, 237));
        let (pw, ph) = self.page_px();
        let sel = self.selection();
        self.with_lay(|lay| {
            c.set_clip(a);
            for page in 0..lay.pages {
                let (ox, oy) = self.page_origin(r, page);
                if oy > a.y + a.h || oy + ph < a.y {
                    continue;
                }
                c.shadow(ox, oy + 2, pw, ph, 2, 10, 60);
                c.fill_rect(ox, oy, pw, ph, rgb(255, 255, 255));
                self.draw_page(c, lay, page, ox, oy, self.mpp(), sel, a, true);
            }
            c.reset_clip();
        });
        // Caret.
        if sel.is_none() {
            let (x, y, h) = self.caret_xy(r, self.cur);
            c.set_clip(a);
            c.fill_rect(x, y, 2, h, rgb(20, 60, 160));
            c.reset_clip();
        }
        c.set_clip(r);
        self.view_h.set(a.h);
        self.draw_tools(c, r);
        // Status bar.
        let sy = r.y + r.h - STATUS_H;
        c.fill_rect(r.x, sy, r.w, STATUS_H, rgb(244, 246, 250));
        c.fill_rect(r.x, sy, r.w, 1, rgb(220, 224, 232));
        let (page, pages) = self.with_lay(|lay| (lay.lines[lay.line_of(self.cur.0, self.cur.1)].page + 1, lay.pages));
        let mut status = format!("Page {} of {}     {} words", page, pages, self.word_count());
        if let Some(n) = &self.note {
            status = format!("{}     {}", status, n);
        }
        c.text(r.x + 14, sy + 6, &status, Size::Small, false, DIM);
    }

    fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64) -> EdAction {
        if let Some(m) = self.menu {
            for i in 0..self.menu_items(m) {
                if self.menu_item_rect(r, m, i).contains(x, y) {
                    self.pick_menu(m, i);
                    return EdAction::Redraw;
                }
            }
            self.menu = None;
            if self.menu_rect(r, m).contains(x, y) {
                return EdAction::Redraw;
            }
            if let Some((t, _)) = self.tool_rects(r).into_iter().find(|(_, b)| b.contains(x, y)) {
                if matches!(t, Tool::Style | Tool::Font | Tool::Color | Tool::Highlight) {
                    return EdAction::Redraw;
                }
            }
        }
        if let Some((t, _)) = self.tool_rects(r).into_iter().find(|(_, b)| b.contains(x, y)) {
            let a = self.press_tool(t, r);
            return a;
        }
        let a = Self::area(r);
        if a.contains(x, y) {
            let p = self.pos_at(r, x, y);
            let double = matches!(self.last_click, Some((q, t)) if q == p && now - t <= DOUBLE);
            self.last_click = Some((p, now));
            self.want_x = None;
            self.pending = None;
            if double {
                let (w0, w1) = self.word_at(p);
                self.anchor = Some(w0);
                self.cur = w1;
                self.dragging = false;
            } else {
                if !self.keys.shift || self.anchor.is_none() {
                    self.anchor = Some(if self.keys.shift { self.cur } else { p });
                }
                self.cur = p;
                self.dragging = true;
            }
            return EdAction::Redraw;
        }
        EdAction::None
    }

    fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        if !self.dragging {
            return false;
        }
        let a = Self::area(r);
        if y < a.y {
            self.scroll -= 20;
        } else if y > a.y + a.h {
            self.scroll += 20;
        }
        self.clamp_scroll(r);
        let p = self.pos_at(r, x, y.clamp(a.y, a.y + a.h - 1));
        if p != self.cur {
            self.cur = p;
        }
        true
    }

    fn on_release(&mut self) -> bool {
        self.dragging = false;
        false
    }

    fn on_wheel(&mut self, delta: i8, r: Rect) -> bool {
        let old = self.scroll;
        self.scroll += delta as i32 * 60;
        self.clamp_scroll(r);
        old != self.scroll
    }

    fn on_key(&mut self, sc: u8, r: Rect) -> EdAction {
        let Some(key) = self.keys.feed(sc) else { return EdAction::None };
        if self.menu.is_some() {
            if key == Key::Esc {
                self.menu = None;
                return EdAction::Redraw;
            }
            self.menu = None;
        }
        let (shift, ctrl) = (self.keys.shift, self.keys.ctrl);
        // Inside a table row: Tab moves between cells, Enter starts a new line in the cell, and
        // the edges of cells can't be deleted (that would merge them).
        if self.doc.paras[self.cur.0].row.is_some() && self.selection().is_none() {
            let (pi, i) = self.cur;
            let t = &self.doc.paras[pi].text;
            match key {
                Key::Tab => {
                    self.next_cell(!shift);
                    self.scroll_to_cursor(r);
                    return EdAction::Redraw;
                }
                Key::Enter if !ctrl => {
                    self.snapshot(EditKind::None);
                    self.insert_char('\n');
                    self.scroll_to_cursor(r);
                    return EdAction::Redraw;
                }
                Key::Backspace if i == 0 || t[i - 1] == '\t' => return EdAction::None,
                Key::Delete if i >= t.len() || t[i] == '\t' => return EdAction::None,
                _ => {}
            }
        }
        let before = self.cur;
        let mut moved = false;
        match key {
            Key::Char(ch) => {
                self.snapshot(if ch == ' ' { EditKind::None } else { EditKind::Type });
                self.insert_char(ch);
            }
            Key::Tab => {
                let p = self.cur_pstyle();
                if self.cur.1 == 0 && (p.list != List::None) {
                    self.press_tool(if shift { Tool::Outdent } else { Tool::Indent }, r);
                } else {
                    self.snapshot(EditKind::None);
                    self.insert_char('\t');
                }
            }
            Key::Enter if ctrl => {
                self.snapshot(EditKind::None);
                self.insert_page_break();
            }
            Key::Enter if shift => {
                self.snapshot(EditKind::None);
                self.insert_char('\n');
            }
            Key::Enter => {
                self.snapshot(EditKind::None);
                self.split_para();
            }
            Key::Backspace => {
                self.snapshot(EditKind::Delete);
                self.backspace();
            }
            Key::Delete => {
                self.snapshot(EditKind::Delete);
                self.delete();
            }
            Key::Left | Key::Right | Key::Up | Key::Down | Key::Home | Key::End | Key::PageUp | Key::PageDown => {
                moved = true;
                self.pending = None;
                if !shift {
                    if let Some((a, b)) = self.selection() {
                        if matches!(key, Key::Left | Key::Right) {
                            self.cur = if key == Key::Left { a } else { b };
                            self.anchor = None;
                            self.scroll_to_cursor(r);
                            return EdAction::Redraw;
                        }
                    }
                }
                let (pi, i) = self.cur;
                let n = self.doc.paras.len();
                match key {
                    Key::Left if ctrl => {
                        let t = &self.doc.paras[pi].text;
                        let mut j = i;
                        while j > 0 && !t[j - 1].is_alphanumeric() {
                            j -= 1;
                        }
                        while j > 0 && t[j - 1].is_alphanumeric() {
                            j -= 1;
                        }
                        self.cur = if i == 0 && pi > 0 { (pi - 1, self.doc.paras[pi - 1].text.len()) } else { (pi, j) };
                    }
                    Key::Right if ctrl => {
                        let t = &self.doc.paras[pi].text;
                        let mut j = i;
                        while j < t.len() && t[j].is_alphanumeric() {
                            j += 1;
                        }
                        while j < t.len() && !t[j].is_alphanumeric() {
                            j += 1;
                        }
                        self.cur = if i == t.len() && pi + 1 < n { (pi + 1, 0) } else { (pi, j) };
                    }
                    Key::Left => self.cur = if i > 0 { (pi, i - 1) } else if pi > 0 { (pi - 1, self.doc.paras[pi - 1].text.len()) } else { (pi, i) },
                    Key::Right => self.cur = if i < self.doc.paras[pi].text.len() { (pi, i + 1) } else if pi + 1 < n { (pi + 1, 0) } else { (pi, i) },
                    Key::Up => self.move_vert(r, -1),
                    Key::Down => self.move_vert(r, 1),
                    Key::PageUp => {
                        let h = self.view_h.get();
                        self.scroll -= h - 40;
                        self.clamp_scroll(r);
                        self.move_vert(r, -(h - 40));
                    }
                    Key::PageDown => {
                        let h = self.view_h.get();
                        self.scroll += h - 40;
                        self.clamp_scroll(r);
                        self.move_vert(r, h - 40);
                    }
                    Key::Home if ctrl => self.cur = (0, 0),
                    Key::End if ctrl => self.cur = (n - 1, self.doc.paras[n - 1].text.len()),
                    Key::Home | Key::End => {
                        let (s, e) = self.with_lay(|lay| {
                            let l = &lay.lines[lay.line_of(pi, i)];
                            (l.start, l.end)
                        });
                        let len = self.doc.paras[pi].text.len();
                        self.cur = if key == Key::Home { (pi, s) } else { (pi, if e < len { e.saturating_sub(1).max(s) } else { e }) };
                    }
                    _ => {}
                }
                if !matches!(key, Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
                    self.want_x = None;
                }
            }
            Key::Ctrl('b') => {
                self.press_tool(Tool::Bold, r);
            }
            Key::Ctrl('i') => {
                self.press_tool(Tool::Italic, r);
            }
            Key::Ctrl('u') => {
                self.press_tool(Tool::Underline, r);
            }
            Key::Ctrl('e') => {
                self.press_tool(Tool::Center, r);
            }
            Key::Ctrl('l') => {
                self.press_tool(Tool::Left, r);
            }
            Key::Ctrl('r') => {
                self.press_tool(Tool::Right, r);
            }
            Key::Ctrl('j') => {
                self.press_tool(Tool::Justify, r);
            }
            Key::Ctrl(']') => {
                self.press_tool(Tool::SizeUp, r);
            }
            Key::Ctrl('[') => {
                self.press_tool(Tool::SizeDown, r);
            }
            Key::Ctrl('s') => return EdAction::Save,
            Key::Ctrl('p') => return EdAction::Print,
            Key::Ctrl('z') => self.do_undo(),
            Key::Ctrl('y') => self.do_redo(),
            Key::Ctrl('a') => {
                self.anchor = Some((0, 0));
                let n = self.doc.paras.len();
                self.cur = (n - 1, self.doc.paras[n - 1].text.len());
                return EdAction::Redraw;
            }
            Key::Ctrl('c') => {
                self.copy();
                return EdAction::Redraw;
            }
            Key::Ctrl('x') => {
                if self.selection().is_some() {
                    self.copy();
                    self.snapshot(EditKind::None);
                    self.delete_selection();
                }
            }
            Key::Ctrl('v') => self.paste(),
            Key::Ctrl('=') => {
                self.press_tool(Tool::ZoomIn, r);
                return EdAction::Redraw;
            }
            Key::Ctrl('-') => {
                self.press_tool(Tool::ZoomOut, r);
                return EdAction::Redraw;
            }
            Key::Esc => return EdAction::Close,
            _ => return EdAction::None,
        }
        if moved {
            if shift {
                if self.anchor.is_none() {
                    self.anchor = Some(before);
                }
            } else {
                self.anchor = None;
            }
        } else if !matches!(key, Key::Ctrl(_)) {
            self.anchor = None;
        }
        self.scroll_to_cursor(r);
        EdAction::Redraw
    }

    fn dirty(&self) -> bool {
        self.dirty
    }

    fn mark_saved(&mut self) {
        self.dirty = false;
        self.note = None;
    }

    fn encode(&mut self, ext: &str) -> Result<Vec<u8>, String> {
        match ext {
            "pdf" => self.to_pdf(),
            "txt" => Ok(self.doc.plain_text().replace('\n', "\r\n").into_bytes()),
            _ => Ok(docx::write(&self.doc)),
        }
    }

    fn picked(&mut self, _purpose: u8, name: &str, bytes: Vec<u8>) -> EdAction {
        let Ok(img) = crate::image::decode(&bytes) else { return EdAction::Message(format!("\"{}\" isn't a picture Solon can read.", name)) };
        // About 96 pixels to the inch, but never wider than the text or taller than 7 inches.
        let cw = self.doc.page_w - self.doc.margin[0] - self.doc.margin[2];
        let mut w = (img.w as i32 * 3 / 4).max(16);
        let mut h = (img.h as i32 * 3 / 4).max(16);
        if w > cw {
            h = h * cw / w;
            w = cw;
        }
        if h > 504 {
            w = w * 504 / h;
            h = 504;
        }
        let ext = match crate::image::sniff(&bytes) {
            Some(crate::image::Format::Jpeg) => "jpeg",
            _ => "png",
        };
        self.snapshot(EditKind::None);
        self.doc.images.push(doc::DocImage { data: bytes, ext: String::from(ext), w: w.max(4), h: h.max(4) });
        let mut st = self.cur_style();
        st.img = self.doc.images.len() as u16;
        st.underline = false;
        st.strike = false;
        st.highlight = None;
        self.pending = Some(st);
        self.insert_char(doc::PICTURE);
        // Typing after the picture goes back to plain text style.
        let mut back = st;
        back.img = 0;
        self.pending = Some(back);
        EdAction::Redraw
    }

    fn print_pages(&mut self, _paper: Paper) -> Vec<Canvas> {
        let lay = doc::layout(&self.doc);
        let mpp = 100_000 / 72;
        let (w, h) = ((self.doc.page_w * mpp / 1000) as usize, (self.doc.page_h * mpp / 1000) as usize);
        (0..lay.pages)
            .map(|page| {
                let mut c = Canvas::new(w, h);
                c.px.fill(0xFF_FFFF);
                let clip = Rect::new(0, 0, w as i32, h as i32);
                self.draw_page(&mut c, &lay, page, 0, 0, mpp, None, clip, false);
                c
            })
            .collect()
    }
}

/// Scale a picture to exactly w x h (box filter when shrinking, nearest when enlarging).
fn scale_image(src: &crate::image::Image, w: usize, h: usize) -> crate::image::Image {
    if w <= src.w && h <= src.h {
        let s = src.shrink_to_fit(w, h);
        if s.w == w && s.h == h {
            return s;
        }
    }
    let mut px = alloc::vec![0u32; w * h];
    for y in 0..h {
        let sy = (y * src.h / h.max(1)).min(src.h - 1);
        for x in 0..w {
            px[y * w + x] = src.px[sy * src.w + (x * src.w / w.max(1)).min(src.w - 1)];
        }
    }
    crate::image::Image { w, h, px, opaque: src.opaque }
}

impl WriterEd {
    fn draw_picture(&self, c: &mut Canvas, img: u16, x: i32, y: i32, w: i32, h: i32) {
        if w <= 0 || h <= 0 || w > 6000 || h > 6000 {
            return;
        }
        let key = (img, w, h);
        let cached = self.pics.borrow().get(&key).cloned();
        let pic = match cached {
            Some(p) => p,
            None => {
                let decoded = self.doc.images.get(img as usize - 1).and_then(|im| crate::image::decode(&im.data).ok()).map(|d| alloc::rc::Rc::new(scale_image(&d, w as usize, h as usize)));
                let mut m = self.pics.borrow_mut();
                if m.len() > 40 {
                    m.clear();
                }
                m.insert(key, decoded.clone());
                decoded
            }
        };
        match pic {
            Some(p) => c.draw_image(&p, x, y),
            None => {
                // A kind of picture Solon can't show (kept in the file all the same).
                c.fill_rect(x, y, w, h, rgb(236, 238, 242));
                c.outline_rect(x, y, w, h, rgb(190, 196, 206));
                c.text_centered(x + w / 2, y + h / 2 - 8, "Picture", Size::Small, false, DIM);
            }
        }
    }
}

impl WriterEd {
    fn in_table(&self) -> bool {
        self.doc.paras[self.cur.0].row.is_some()
    }

    /// Tab / Shift+Tab in a table. Tab in the last cell adds a row, as in Word.
    fn next_cell(&mut self, forward: bool) {
        let (pi, i) = self.cur;
        let text = &self.doc.paras[pi].text;
        let table = self.doc.paras[pi].row.as_ref().map(|r| r.table);
        if forward {
            if let Some(t) = (i..text.len()).find(|&k| text[k] == '\t') {
                self.cur = (pi, t + 1);
            } else if self.doc.paras.get(pi + 1).and_then(|p| p.row.as_ref()).map(|r| r.table) == table {
                self.cur = (pi + 1, 0);
            } else {
                self.snapshot(EditKind::None);
                self.add_row();
            }
        } else {
            let start = text[..i].iter().rposition(|&c| c == '\t').map(|k| k + 1).unwrap_or(0);
            if start > 0 {
                let prev = text[..start - 1].iter().rposition(|&c| c == '\t').map(|k| k + 1).unwrap_or(0);
                self.cur = (pi, prev);
            } else if pi > 0 && self.doc.paras[pi - 1].row.as_ref().map(|r| r.table) == table {
                let pt = &self.doc.paras[pi - 1].text;
                let last = pt.iter().rposition(|&c| c == '\t').map(|k| k + 1).unwrap_or(0);
                self.cur = (pi - 1, last);
            }
        }
        self.anchor = None;
        self.want_x = None;
    }

    /// A new empty row under the cursor's row, with the same columns.
    fn add_row(&mut self) {
        let pi = self.cur.0;
        let p = &self.doc.paras[pi];
        let Some(row) = p.row.clone() else { return };
        let tabs = p.text.iter().filter(|&&c| c == '\t').count();
        let mut st = p.style_at(0);
        st.img = 0;
        let mut np = Para::new(p.p, st);
        np.row = Some(row);
        for _ in 0..tabs {
            np.text.push('\t');
            np.st.push(st);
        }
        self.doc.paras.insert(pi + 1, np);
        self.cur = (pi + 1, 0);
        self.changed();
    }

    /// The Table button: add a row when in a table, otherwise put in a 3 x 3 table.
    fn table_button(&mut self) {
        self.snapshot(EditKind::None);
        if self.in_table() {
            self.add_row();
            return;
        }
        let cw = self.doc.page_w - self.doc.margin[0] - self.doc.margin[2];
        let id = self.doc.paras.iter().filter_map(|p| p.row.as_ref().map(|r| r.table)).max().unwrap_or(0) + 1;
        let mut st = self.cur_style();
        st.img = 0;
        let pi = self.cur.0;
        // Start the table on its own line.
        let at = if self.doc.paras[pi].text.is_empty() {
            self.doc.paras.remove(pi);
            pi
        } else {
            pi + 1
        };
        for k in 0..3 {
            let mut np = Para::new(PStyle::NORMAL, st);
            np.p.after = 0;
            np.row = Some(doc::Row { table: id, widths: alloc::vec![cw / 3; 3] });
            for _ in 0..2 {
                np.text.push('\t');
                np.st.push(st);
            }
            self.doc.paras.insert(at + k, np);
        }
        // A normal line after it, to carry on typing.
        if self.doc.paras.get(at + 3).is_none_or(|p| p.row.is_some()) {
            self.doc.paras.insert(at + 3, Para::new(PStyle::NORMAL, st));
        }
        self.cur = (at, 0);
        self.anchor = None;
        self.changed();
    }
}

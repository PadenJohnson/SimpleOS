//! Text Editor: plain text files (.txt, notes, settings files, web pages, code). Keeps each
//! file's encoding and line endings; word wrap, find and replace, undo, bigger text, print.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::docshell::{button, DocEditor, EdAction, Kind, DIM, INK};
use crate::gfx::{mono_width, rgb, text_width, Canvas, Rect, Size};
use crate::keys::{self, Key, Keys};
use crate::print::Paper;

const ACCENT: u32 = rgb(80, 92, 112);
const BAR_H: i32 = 52;
const STATUS_H: i32 = 30;
const FIND_H: i32 = 50;
const DOUBLE: u64 = 45;

fn icon(c: &mut Canvas, x: i32, y: i32) {
    c.round_rect(x, y, 26, 28, 4, ACCENT);
    for k in 0..4 {
        c.fill_rect(x + 5, y + 7 + k * 5, if k == 3 { 10 } else { 16 }, 2, rgb(255, 255, 255));
    }
}

pub static KIND: Kind = Kind {
    app: "Text Editor",
    list_title: "Text files",
    new_label: "New text file",
    exts: &["txt", "text", "md", "log", "ini", "cfg", "conf", "json", "xml", "html", "htm", "css", "js", "py", "rs", "c", "h", "cpp", "sh", "bat", "toml", "yaml", "yml", "srt", "nfo"],
    save_ext: "txt",
    keeps: &["txt", "text", "md", "log", "ini", "cfg", "conf", "json", "xml", "html", "htm", "css", "js", "py", "rs", "c", "h", "cpp", "sh", "bat", "toml", "yaml", "yml", "srt", "nfo"],
    accent: ACCENT,
    empty: ["No text files yet.", "Start one with New text file, or plug in a USB drive that has .txt files."],
    extra: &[],
    icon,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Enc {
    Utf8,
    Utf8Bom,
    Utf16Le,
    Utf16Be,
    Latin1,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum EditKind {
    None,
    Type,
    Delete,
}

#[derive(Clone)]
struct Snapshot {
    lines: Vec<Vec<char>>,
    cur: (usize, usize),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Back,
    Save,
    SaveAs,
    Print,
    Undo,
    Redo,
    Find,
    Wrap,
    Smaller,
    Bigger,
}

const BUTTONS: [(Btn, &str); 10] = [
    (Btn::Back, "< Files"),
    (Btn::Save, "Save"),
    (Btn::SaveAs, "Save as"),
    (Btn::Print, "Print"),
    (Btn::Undo, "Undo"),
    (Btn::Redo, "Redo"),
    (Btn::Find, "Find"),
    (Btn::Wrap, "Wrap"),
    (Btn::Smaller, "A-"),
    (Btn::Bigger, "A+"),
];

struct Find {
    what: String,
    with: String,
    /// Typing goes into the replace box.
    in_with: bool,
}

pub struct TextEd {
    lines: Vec<Vec<char>>,
    cur: (usize, usize),
    anchor: Option<(usize, usize)>,
    /// First visible row, and how far it's scrolled sideways (no-wrap mode), in characters.
    top: usize,
    left: usize,
    wrap: bool,
    px: u8,
    enc: Enc,
    crlf: bool,
    dirty: bool,
    keys: Keys,
    undo: Vec<Snapshot>,
    redo: Vec<Snapshot>,
    last_edit: EditKind,
    find: Option<Find>,
    dragging: bool,
    last_click: Option<((usize, usize), u64)>,
    /// Remembered column for up/down.
    want_col: Option<usize>,
    /// Size of the text area last drawn (rows, columns), for paging and wrapping.
    view: core::cell::Cell<(usize, usize)>,
}

pub fn new_doc() -> TextEd {
    TextEd {
        lines: vec![Vec::new()],
        cur: (0, 0),
        anchor: None,
        top: 0,
        left: 0,
        wrap: true,
        px: 20,
        enc: Enc::Utf8,
        crlf: true,
        dirty: false,
        keys: Keys::default(),
        undo: Vec::new(),
        redo: Vec::new(),
        last_edit: EditKind::None,
        find: None,
        dragging: false,
        last_click: None,
        want_col: None,
        view: core::cell::Cell::new((30, 80)),
    }
}

/// Windows-1252 bytes 0x80..0x9F.
const CP1252: [u16; 32] = [
    0x20AC, 0x81, 0x201A, 0x0192, 0x201E, 0x2026, 0x2020, 0x2021, 0x02C6, 0x2030, 0x0160, 0x2039, 0x0152, 0x8D, 0x017D, 0x8F, 0x90, 0x2018, 0x2019, 0x201C, 0x201D, 0x2022, 0x2013, 0x2014, 0x02DC, 0x2122, 0x0161, 0x203A, 0x0153, 0x9D, 0x017E, 0x0178,
];

pub fn decode_text(b: &[u8]) -> (String, &'static str) {
    if b.starts_with(&[0xEF, 0xBB, 0xBF]) {
        return (String::from_utf8_lossy(&b[3..]).into_owned(), "utf8bom");
    }
    if b.len() >= 2 && (b[..2] == [0xFF, 0xFE] || b[..2] == [0xFE, 0xFF]) {
        let le = b[0] == 0xFF;
        let units: Vec<u16> = b[2..].chunks_exact(2).map(|p| if le { u16::from_le_bytes([p[0], p[1]]) } else { u16::from_be_bytes([p[0], p[1]]) }).collect();
        return (char::decode_utf16(units).map(|r| r.unwrap_or('\u{FFFD}')).collect(), if le { "utf16le" } else { "utf16be" });
    }
    match core::str::from_utf8(b) {
        Ok(s) => (String::from(s), "utf8"),
        Err(_) => (b.iter().map(|&x| if (0x80..0xA0).contains(&x) { char::from_u32(CP1252[(x - 0x80) as usize] as u32).unwrap_or('?') } else { x as char }).collect(), "latin1"),
    }
}

pub fn load(_name: &str, b: &[u8]) -> Result<TextEd, String> {
    if b.len() > 20 * 1024 * 1024 {
        return Err(String::from("it's too big for the Text Editor (over 20 MB)"));
    }
    let (text, enc) = decode_text(b);
    let mut ed = new_doc();
    ed.enc = match enc {
        "utf8bom" => Enc::Utf8Bom,
        "utf16le" => Enc::Utf16Le,
        "utf16be" => Enc::Utf16Be,
        "latin1" => Enc::Latin1,
        _ => Enc::Utf8,
    };
    ed.crlf = text.contains("\r\n") || !text.contains('\n');
    ed.lines = text.split('\n').map(|l| l.strip_suffix('\r').unwrap_or(l).chars().collect()).collect();
    if ed.lines.is_empty() {
        ed.lines.push(Vec::new());
    }
    Ok(ed)
}

fn is_word(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

impl TextEd {
    fn text(&self) -> String {
        let nl = if self.crlf { "\r\n" } else { "\n" };
        let mut s = String::new();
        for (i, l) in self.lines.iter().enumerate() {
            if i > 0 {
                s.push_str(nl);
            }
            s.extend(l.iter());
        }
        s
    }

    // ---- geometry -------------------------------------------------------------------

    fn bar(r: Rect) -> Rect {
        Rect::new(r.x, r.y, r.w, BAR_H)
    }
    fn button_rects(r: Rect) -> Vec<(Btn, Rect)> {
        let mut x = r.x + 12;
        BUTTONS
            .iter()
            .map(|&(b, label)| {
                let w = text_width(label, Size::Small, true) + 26;
                let rect = Rect::new(x, r.y + 9, w, 34);
                x += w + if matches!(b, Btn::Print | Btn::Redo | Btn::Wrap) { 18 } else { 6 };
                (b, rect)
            })
            .collect()
    }
    fn gutter(&self) -> i32 {
        let digits = format!("{}", self.lines.len()).len().max(2) as i32;
        digits * mono_width(self.px) + 20
    }
    fn area(&self, r: Rect) -> Rect {
        let top = r.y + BAR_H + if self.find.is_some() { FIND_H } else { 0 };
        Rect::new(r.x, top, r.w, r.y + r.h - STATUS_H - top)
    }
    fn line_h(&self) -> i32 {
        self.px as i32 + 4
    }
    fn cols(&self, r: Rect) -> usize {
        let a = self.area(r);
        ((a.w - self.gutter() - 16) / mono_width(self.px)).max(8) as usize
    }

    /// Display column of character `i` in a line (tabs count as 4).
    fn vcol(line: &[char], i: usize) -> usize {
        line[..i.min(line.len())].iter().map(|&c| if c == '\t' { 4 } else { 1 }).sum()
    }
    fn index_at_vcol(line: &[char], v: usize) -> usize {
        let mut col = 0;
        for (i, &c) in line.iter().enumerate() {
            let w = if c == '\t' { 4 } else { 1 };
            if col + w > v {
                return if v - col > w / 2 { i + 1 } else { i };
            }
            col += w;
        }
        line.len()
    }

    /// Visual rows: (line, first char, end char).
    fn rows(&self, cols: usize) -> Vec<(usize, usize, usize)> {
        let mut out = Vec::with_capacity(self.lines.len());
        for (li, l) in self.lines.iter().enumerate() {
            if !self.wrap || Self::vcol(l, l.len()) <= cols {
                out.push((li, 0, l.len()));
                continue;
            }
            let mut start = 0;
            while start < l.len() {
                // Fill up to `cols` display columns, breaking after a space if there is one.
                let mut end = start;
                let mut w = 0;
                while end < l.len() {
                    let cw = if l[end] == '\t' { 4 } else { 1 };
                    if w + cw > cols {
                        break;
                    }
                    w += cw;
                    end += 1;
                }
                if end < l.len() {
                    if let Some(sp) = (start + 1..end).rev().find(|&k| l[k - 1] == ' ') {
                        end = sp;
                    }
                }
                out.push((li, start, end));
                start = end;
            }
        }
        out
    }

    fn row_of(&self, rows: &[(usize, usize, usize)], p: (usize, usize)) -> usize {
        let mut best = 0;
        for (i, &(l, s, e)) in rows.iter().enumerate() {
            if l == p.0 && p.1 >= s && (p.1 < e || (p.1 == e && (i + 1 >= rows.len() || rows[i + 1].0 != l))) {
                return i;
            }
            if l == p.0 && p.1 >= s {
                best = i;
            }
        }
        best
    }

    fn selection(&self) -> Option<((usize, usize), (usize, usize))> {
        let a = self.anchor?;
        if a == self.cur {
            return None;
        }
        Some(if a < self.cur { (a, self.cur) } else { (self.cur, a) })
    }

    fn selected_text(&self) -> String {
        let Some((a, b)) = self.selection() else { return String::new() };
        let mut s = String::new();
        for l in a.0..=b.0 {
            let line = &self.lines[l];
            let from = if l == a.0 { a.1 } else { 0 };
            let to = if l == b.0 { b.1 } else { line.len() };
            s.extend(line[from..to].iter());
            if l != b.0 {
                s.push('\n');
            }
        }
        s
    }

    // ---- editing --------------------------------------------------------------------

    fn snapshot(&mut self, kind: EditKind) {
        if kind == EditKind::None || kind != self.last_edit {
            self.undo.push(Snapshot { lines: self.lines.clone(), cur: self.cur });
            if self.undo.len() > 200 {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
        self.last_edit = kind;
        self.dirty = true;
    }

    fn delete_selection(&mut self) -> bool {
        let Some((a, b)) = self.selection() else { return false };
        let tail: Vec<char> = self.lines[b.0][b.1..].to_vec();
        self.lines[a.0].truncate(a.1);
        self.lines[a.0].extend(tail);
        self.lines.drain(a.0 + 1..=b.0);
        self.cur = a;
        self.anchor = None;
        true
    }

    fn insert_str(&mut self, s: &str) {
        self.delete_selection();
        let mut parts = s.split('\n');
        let first: Vec<char> = parts.next().unwrap_or("").chars().filter(|&c| c != '\r').collect();
        let (l, c) = self.cur;
        let tail: Vec<char> = self.lines[l].split_off(c);
        let n = first.len();
        self.lines[l].extend(first);
        self.cur = (l, c + n);
        for p in parts {
            let line: Vec<char> = p.chars().filter(|&c| c != '\r').collect();
            let k = line.len();
            self.lines.insert(self.cur.0 + 1, line);
            self.cur = (self.cur.0 + 1, k);
        }
        self.lines[self.cur.0].extend(tail);
    }

    fn backspace(&mut self) {
        if self.delete_selection() {
            return;
        }
        let (l, c) = self.cur;
        if c > 0 {
            self.lines[l].remove(c - 1);
            self.cur.1 -= 1;
        } else if l > 0 {
            let line = self.lines.remove(l);
            let k = self.lines[l - 1].len();
            self.lines[l - 1].extend(line);
            self.cur = (l - 1, k);
        }
    }

    fn delete(&mut self) {
        if self.delete_selection() {
            return;
        }
        let (l, c) = self.cur;
        if c < self.lines[l].len() {
            self.lines[l].remove(c);
        } else if l + 1 < self.lines.len() {
            let next = self.lines.remove(l + 1);
            self.lines[l].extend(next);
        }
    }

    fn select_word(&mut self) {
        let (l, c) = self.cur;
        let line = &self.lines[l];
        let mut a = c.min(line.len());
        let mut b = a;
        while a > 0 && is_word(line[a - 1]) {
            a -= 1;
        }
        while b < line.len() && is_word(line[b]) {
            b += 1;
        }
        self.anchor = Some((l, a));
        self.cur = (l, b);
    }

    fn find_next(&mut self, forward: bool) -> bool {
        let Some(f) = &self.find else { return false };
        let what: Vec<char> = f.what.to_lowercase().chars().collect();
        if what.is_empty() {
            return false;
        }
        let n = self.lines.len();
        let start = if forward { self.selection().map(|(_, b)| b).unwrap_or(self.cur) } else { self.selection().map(|(a, _)| a).unwrap_or(self.cur) };
        let matches_at = |line: &[char], i: usize| i + what.len() <= line.len() && line[i..i + what.len()].iter().zip(&what).all(|(a, b)| a.to_lowercase().next() == Some(*b));
        for step in 0..=n {
            let l = if forward { (start.0 + step) % n } else { (start.0 + n - step % n) % n };
            let line = &self.lines[l];
            let hit = if forward {
                let from = if step == 0 { start.1 } else { 0 };
                (from..line.len()).find(|&i| matches_at(line, i))
            } else {
                let to = if step == 0 { start.1 } else { line.len() };
                (0..to).rev().find(|&i| matches_at(line, i))
            };
            if let Some(i) = hit {
                self.anchor = Some((l, i));
                self.cur = (l, i + what.len());
                return true;
            }
        }
        false
    }

    fn replace_one(&mut self) -> bool {
        let Some(f) = &self.find else { return false };
        let (what, with) = (f.what.to_lowercase(), f.with.clone());
        if self.selected_text().to_lowercase() == what && !what.is_empty() {
            self.snapshot(EditKind::None);
            self.insert_str(&with);
        }
        self.find_next(true)
    }

    fn replace_all(&mut self) -> usize {
        let Some(f) = &self.find else { return 0 };
        if f.what.is_empty() {
            return 0;
        }
        let with = f.with.clone();
        self.snapshot(EditKind::None);
        self.cur = (0, 0);
        self.anchor = None;
        let mut n = 0;
        let mut done_to = (0, 0);
        while n < 100_000 && self.find_next(true) {
            let start = self.selection().map(|(a, _)| a).unwrap_or(self.cur);
            // The search wrapped around to the top: everything's replaced.
            if start < done_to {
                break;
            }
            self.insert_str(&with);
            done_to = self.cur;
            n += 1;
        }
        self.anchor = None;
        n
    }

    fn scroll_to_cursor(&mut self, r: Rect) {
        let cols = self.cols(r);
        let rows = self.rows(cols);
        let row = self.row_of(&rows, self.cur);
        let visible = (self.area(r).h / self.line_h()).max(1) as usize;
        if row < self.top {
            self.top = row;
        } else if row >= self.top + visible {
            self.top = row + 1 - visible;
        }
        if !self.wrap {
            let v = Self::vcol(&self.lines[self.cur.0], self.cur.1);
            if v < self.left {
                self.left = v.saturating_sub(8);
            } else if v >= self.left + cols {
                self.left = v + 8 - cols;
            }
        } else {
            self.left = 0;
        }
    }

    fn move_vert(&mut self, r: Rect, delta: i32) {
        let cols = self.cols(r);
        let rows = self.rows(cols);
        let row = self.row_of(&rows, self.cur);
        let (l, s, _) = rows[row];
        let want = self.want_col.unwrap_or_else(|| Self::vcol(&self.lines[l], self.cur.1) - Self::vcol(&self.lines[l], s));
        let nr = (row as i32 + delta).clamp(0, rows.len() as i32 - 1) as usize;
        let (nl, ns, ne) = rows[nr];
        let line = &self.lines[nl];
        let base = Self::vcol(line, ns);
        let i = Self::index_at_vcol(line, base + want).clamp(ns, ne);
        self.cur = (nl, i);
        self.want_col = Some(want);
    }

    fn pos_at(&self, x: i32, y: i32, r: Rect) -> (usize, usize) {
        let a = self.area(r);
        let cols = self.cols(r);
        let rows = self.rows(cols);
        let row = (self.top as i32 + ((y - a.y - 6).max(0) / self.line_h())).clamp(0, rows.len() as i32 - 1) as usize;
        let (l, s, e) = rows[row];
        let line = &self.lines[l];
        let cw = mono_width(self.px);
        let v = ((x - a.x - self.gutter() + cw / 2).max(0) / cw) as usize + self.left;
        let i = Self::index_at_vcol(line, Self::vcol(line, s) + v).clamp(s, e);
        (l, i)
    }

    fn press_button(&mut self, b: Btn, r: Rect) -> EdAction {
        match b {
            Btn::Back => EdAction::Close,
            Btn::Save => EdAction::Save,
            Btn::SaveAs => EdAction::SaveAs,
            Btn::Print => EdAction::Print,
            Btn::Undo => {
                self.do_undo();
                EdAction::Redraw
            }
            Btn::Redo => {
                self.do_redo();
                EdAction::Redraw
            }
            Btn::Find => {
                self.toggle_find();
                EdAction::Redraw
            }
            Btn::Wrap => {
                self.wrap = !self.wrap;
                self.scroll_to_cursor(r);
                EdAction::Redraw
            }
            Btn::Smaller => {
                self.px = match self.px {
                    32 => 20,
                    _ => 16,
                };
                self.scroll_to_cursor(r);
                EdAction::Redraw
            }
            Btn::Bigger => {
                self.px = match self.px {
                    16 => 20,
                    _ => 32,
                };
                self.scroll_to_cursor(r);
                EdAction::Redraw
            }
        }
    }

    fn toggle_find(&mut self) {
        if self.find.is_some() {
            self.find = None;
        } else {
            let what = if self.selection().is_some_and(|(a, b)| a.0 == b.0) { self.selected_text() } else { String::new() };
            self.find = Some(Find { what, with: String::new(), in_with: false });
        }
    }

    fn do_undo(&mut self) {
        if let Some(s) = self.undo.pop() {
            self.redo.push(Snapshot { lines: core::mem::take(&mut self.lines), cur: self.cur });
            self.lines = s.lines;
            self.cur = s.cur;
            self.anchor = None;
            self.dirty = true;
            self.last_edit = EditKind::None;
        }
    }

    fn do_redo(&mut self) {
        if let Some(s) = self.redo.pop() {
            self.undo.push(Snapshot { lines: core::mem::take(&mut self.lines), cur: self.cur });
            self.lines = s.lines;
            self.cur = s.cur;
            self.anchor = None;
            self.dirty = true;
            self.last_edit = EditKind::None;
        }
    }

    fn find_rects(&self, r: Rect) -> [Rect; 6] {
        let y = r.y + BAR_H + 8;
        let w = ((r.w - 560) / 2).clamp(140, 320);
        let x0 = r.x + 60;
        let x1 = x0 + w + 110;
        [
            Rect::new(x0, y, w, 34),
            Rect::new(x1, y, w, 34),
            Rect::new(x1 + w + 12, y, 70, 34),
            Rect::new(x1 + w + 88, y, 84, 34),
            Rect::new(x1 + w + 178, y, 104, 34),
            Rect::new(x1 + w + 288, y, 40, 34),
        ]
    }
}

impl DocEditor for TextEd {
    fn draw(&self, c: &mut Canvas, r: Rect) {
        // Toolbar.
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(255, 255, 255));
        let bar = Self::bar(r);
        c.fill_rect(bar.x, bar.y, bar.w, bar.h, rgb(244, 246, 249));
        c.fill_rect(bar.x, bar.y + bar.h - 1, bar.w, 1, rgb(222, 226, 232));
        for (b, rect) in Self::button_rects(r) {
            let label = BUTTONS.iter().find(|x| x.0 == b).unwrap().1;
            let on = (b == Btn::Wrap && self.wrap) || (b == Btn::Find && self.find.is_some());
            let primary = b == Btn::Save && self.dirty;
            if on {
                c.round_rect(rect.x, rect.y, rect.w, rect.h, 8, rgb(214, 224, 240));
                c.text_centered(rect.x + rect.w / 2, rect.y + 8, label, Size::Small, true, INK);
            } else {
                button(c, rect, label, primary, ACCENT);
            }
        }
        // Find bar.
        if let Some(f) = &self.find {
            let fr = self.find_rects(r);
            let y = r.y + BAR_H;
            c.fill_rect(r.x, y, r.w, FIND_H, rgb(248, 249, 251));
            c.fill_rect(r.x, y + FIND_H - 1, r.w, 1, rgb(222, 226, 232));
            c.text(r.x + 14, fr[0].y + 9, "Find", Size::Small, true, DIM);
            c.text(fr[1].x - 100, fr[1].y + 9, "Replace with", Size::Small, true, DIM);
            for (k, (t, active)) in [(&f.what, !f.in_with), (&f.with, f.in_with)].iter().enumerate() {
                let b = fr[k];
                c.round_rect(b.x - 1, b.y - 1, b.w + 2, b.h + 2, 7, if *active { ACCENT } else { rgb(210, 214, 222) });
                c.round_rect(b.x, b.y, b.w, b.h, 6, rgb(255, 255, 255));
                let shown = crate::photos::ellipsize_left(t, b.w - 20, Size::Small, false);
                let tw = c.text(b.x + 8, b.y + 9, &shown, Size::Small, false, INK);
                if *active {
                    c.fill_rect(b.x + 9 + tw, b.y + 8, 2, 18, ACCENT);
                }
            }
            button(c, fr[2], "Next", true, ACCENT);
            button(c, fr[3], "Replace", false, ACCENT);
            button(c, fr[4], "Replace all", false, ACCENT);
            button(c, fr[5], "X", false, ACCENT);
        }
        // Text.
        let a = self.area(r);
        let cols = self.cols(r);
        let rows = self.rows(cols);
        let lh = self.line_h();
        let visible = (a.h / lh).max(1) as usize + 1;
        self.view.set((visible.saturating_sub(1).max(1), cols));
        let g = self.gutter();
        let cw = mono_width(self.px);
        c.fill_rect(a.x, a.y, g - 8, a.h, rgb(246, 247, 250));
        c.set_clip(a);
        let sel = self.selection();
        for (k, &(l, s, e)) in rows.iter().enumerate().skip(self.top).take(visible) {
            let y = a.y + 6 + (k - self.top) as i32 * lh;
            if s == 0 {
                let num = format!("{}", l + 1);
                let nx = a.x + g - 14 - num.len() as i32 * mono_width(16);
                for (i, ch) in num.chars().enumerate() {
                    c.mono_char(nx + i as i32 * mono_width(16), y + (lh - 16) / 2, ch, 16, rgb(160, 166, 176));
                }
            }
            let line = &self.lines[l];
            let base = Self::vcol(line, s);
            let mut v = base;
            for i in s..e {
                let ch = line[i];
                let w = if ch == '\t' { 4 } else { 1 };
                let x = a.x + g + (v as i32 - base as i32 - self.left as i32) * cw;
                if x > a.x + a.w {
                    break;
                }
                let in_sel = sel.is_some_and(|(p, q)| (l, i) >= p && (l, i) < q);
                if in_sel {
                    c.fill_rect(x, y, cw * w as i32, lh, rgb(196, 216, 246));
                }
                if x >= a.x + g - cw && ch != '\t' && ch != ' ' {
                    c.mono_char(x, y + 2, ch, self.px, INK);
                }
                v += w;
            }
            // Selected line ends show as a little block.
            if sel.is_some_and(|(p, q)| (l, e) >= p && (l, e) < q) && e == line.len() {
                let x = a.x + g + (v as i32 - base as i32 - self.left as i32) * cw;
                c.fill_rect(x, y, cw / 2, lh, rgb(196, 216, 246));
            }
        }
        // Cursor.
        let row = self.row_of(&rows, self.cur);
        if row >= self.top && row < self.top + visible {
            let (l, s, _) = rows[row];
            let line = &self.lines[l];
            let x = a.x + g + (Self::vcol(line, self.cur.1) as i32 - Self::vcol(line, s) as i32 - self.left as i32) * cw;
            let y = a.y + 6 + (row - self.top) as i32 * lh;
            c.fill_rect(x - 1, y, 2, lh, rgb(30, 90, 200));
        }
        c.reset_clip();
        c.set_clip(r);
        // Status bar.
        let sy = r.y + r.h - STATUS_H;
        c.fill_rect(r.x, sy, r.w, STATUS_H, rgb(244, 246, 249));
        c.fill_rect(r.x, sy, r.w, 1, rgb(222, 226, 232));
        let enc = match self.enc {
            Enc::Utf8 => "UTF-8",
            Enc::Utf8Bom => "UTF-8 (BOM)",
            Enc::Utf16Le | Enc::Utf16Be => "UTF-16",
            Enc::Latin1 => "Windows",
        };
        let status = format!("Line {}, column {}     {} lines     {}     {}", self.cur.0 + 1, self.cur.1 + 1, self.lines.len(), enc, if self.crlf { "Windows line endings" } else { "Unix line endings" });
        c.text(r.x + 14, sy + 7, &status, Size::Small, false, DIM);
    }

    fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64) -> EdAction {
        if let Some((b, _)) = Self::button_rects(r).into_iter().find(|(_, rect)| rect.contains(x, y)) {
            return self.press_button(b, r);
        }
        if self.find.is_some() {
            let fr = self.find_rects(r);
            if fr[0].contains(x, y) || fr[1].contains(x, y) {
                if let Some(f) = &mut self.find {
                    f.in_with = fr[1].contains(x, y);
                }
                return EdAction::Redraw;
            }
            if fr[2].contains(x, y) {
                if !self.find_next(true) {
                    return EdAction::Toast(String::from("Not found"));
                }
                self.scroll_to_cursor(r);
                return EdAction::Redraw;
            }
            if fr[3].contains(x, y) {
                self.replace_one();
                self.scroll_to_cursor(r);
                return EdAction::Redraw;
            }
            if fr[4].contains(x, y) {
                let n = self.replace_all();
                self.scroll_to_cursor(r);
                return EdAction::Toast(format!("Replaced {} time{}", n, if n == 1 { "" } else { "s" }));
            }
            if fr[5].contains(x, y) {
                self.find = None;
                return EdAction::Redraw;
            }
        }
        let a = self.area(r);
        if a.contains(x, y) {
            let p = self.pos_at(x, y, r);
            let double = matches!(self.last_click, Some((q, t)) if q == p && now - t <= DOUBLE);
            self.last_click = Some((p, now));
            self.cur = p;
            self.want_col = None;
            if double {
                self.select_word();
            } else if self.keys.shift {
                if self.anchor.is_none() {
                    self.anchor = Some(p);
                }
            } else {
                self.anchor = Some(p);
            }
            self.dragging = !double;
            return EdAction::Redraw;
        }
        EdAction::None
    }

    fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        if !self.dragging {
            return false;
        }
        let a = self.area(r);
        if y < a.y && self.top > 0 {
            self.top -= 1;
        } else if y > a.y + a.h {
            self.top += 1;
        }
        let p = self.pos_at(x, y.clamp(a.y, a.y + a.h - 1), r);
        if p != self.cur {
            self.cur = p;
            return true;
        }
        false
    }

    fn on_release(&mut self) -> bool {
        self.dragging = false;
        false
    }

    fn on_wheel(&mut self, delta: i8, r: Rect) -> bool {
        let rows = self.rows(self.cols(r)).len();
        let old = self.top;
        self.top = (self.top as i32 + delta as i32 * 3).clamp(0, rows.saturating_sub(1) as i32) as usize;
        old != self.top
    }

    fn on_key(&mut self, sc: u8, r: Rect) -> EdAction {
        let Some(key) = self.keys.feed(sc) else { return EdAction::None };
        // Typing into the find bar.
        if let Some(f) = &mut self.find {
            let field = if f.in_with { &mut f.with } else { &mut f.what };
            match key {
                Key::Char(ch) => {
                    field.push(ch);
                    return EdAction::Redraw;
                }
                Key::Backspace => {
                    field.pop();
                    return EdAction::Redraw;
                }
                Key::Tab => {
                    f.in_with = !f.in_with;
                    return EdAction::Redraw;
                }
                Key::Enter => {
                    let ok = if f.in_with { self.replace_one() } else { self.find_next(!self.keys.shift) };
                    self.scroll_to_cursor(r);
                    return if ok { EdAction::Redraw } else { EdAction::Toast(String::from("Not found")) };
                }
                Key::Esc => {
                    self.find = None;
                    return EdAction::Redraw;
                }
                Key::Ctrl('v') => {
                    field.push_str(&keys::paste().replace(['\r', '\n'], " "));
                    return EdAction::Redraw;
                }
                _ => {}
            }
        }
        let shift = self.keys.shift;
        let ctrl = self.keys.ctrl;
        let before = self.cur;
        let mut moved = false;
        let (rows_visible, _) = self.view.get();
        match key {
            Key::Char(ch) => {
                self.snapshot(if ch == ' ' { EditKind::None } else { EditKind::Type });
                let mut b = [0u8; 4];
                self.insert_str(ch.encode_utf8(&mut b));
            }
            Key::Tab => {
                self.snapshot(EditKind::None);
                self.insert_str("    ");
            }
            Key::Enter => {
                self.snapshot(EditKind::None);
                let indent: String = self.lines[self.cur.0].iter().take_while(|c| **c == ' ' || **c == '\t').collect();
                self.insert_str("\n");
                self.insert_str(&indent);
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
                let (l, c) = self.cur;
                match key {
                    Key::Left if ctrl => {
                        let line = &self.lines[l];
                        let mut i = c;
                        while i > 0 && !is_word(line[i - 1]) {
                            i -= 1;
                        }
                        while i > 0 && is_word(line[i - 1]) {
                            i -= 1;
                        }
                        self.cur = if c == 0 && l > 0 { (l - 1, self.lines[l - 1].len()) } else { (l, i) };
                    }
                    Key::Right if ctrl => {
                        let line = &self.lines[l];
                        let mut i = c;
                        while i < line.len() && is_word(line[i]) {
                            i += 1;
                        }
                        while i < line.len() && !is_word(line[i]) {
                            i += 1;
                        }
                        self.cur = if c == line.len() && l + 1 < self.lines.len() { (l + 1, 0) } else { (l, i) };
                    }
                    Key::Left => self.cur = if c > 0 { (l, c - 1) } else if l > 0 { (l - 1, self.lines[l - 1].len()) } else { (l, c) },
                    Key::Right => self.cur = if c < self.lines[l].len() { (l, c + 1) } else if l + 1 < self.lines.len() { (l + 1, 0) } else { (l, c) },
                    Key::Up => self.move_vert(r, -1),
                    Key::Down => self.move_vert(r, 1),
                    Key::PageUp => self.move_vert(r, -(rows_visible as i32)),
                    Key::PageDown => self.move_vert(r, rows_visible as i32),
                    Key::Home if ctrl => self.cur = (0, 0),
                    Key::End if ctrl => self.cur = (self.lines.len() - 1, self.lines.last().unwrap().len()),
                    Key::Home => {
                        let indent = self.lines[l].iter().take_while(|c| c.is_whitespace()).count();
                        self.cur = (l, if c == indent { 0 } else { indent });
                    }
                    Key::End => self.cur = (l, self.lines[l].len()),
                    _ => {}
                }
                if !matches!(key, Key::Up | Key::Down | Key::PageUp | Key::PageDown) {
                    self.want_col = None;
                }
            }
            Key::Ctrl('s') => return EdAction::Save,
            Key::Ctrl('p') => return EdAction::Print,
            Key::Ctrl('f') | Key::Ctrl('h') => {
                self.toggle_find();
                return EdAction::Redraw;
            }
            Key::Ctrl('a') => {
                self.anchor = Some((0, 0));
                self.cur = (self.lines.len() - 1, self.lines.last().unwrap().len());
            }
            Key::Ctrl('c') | Key::Ctrl('x') => {
                let t = self.selected_text();
                if !t.is_empty() {
                    keys::copy(&t);
                    if key == Key::Ctrl('x') {
                        self.snapshot(EditKind::None);
                        self.delete_selection();
                    }
                }
            }
            Key::Ctrl('v') => {
                let t = keys::paste();
                if !t.is_empty() {
                    self.snapshot(EditKind::None);
                    self.insert_str(&t);
                }
            }
            Key::Ctrl('z') => self.do_undo(),
            Key::Ctrl('y') => self.do_redo(),
            Key::Ctrl('=') => return self.press_button(Btn::Bigger, r),
            Key::Ctrl('-') => return self.press_button(Btn::Smaller, r),
            Key::Esc => return EdAction::Close,
            Key::F(3) => {
                if self.find.is_none() {
                    self.toggle_find();
                }
                self.find_next(!shift);
            }
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
        } else if !matches!(key, Key::Ctrl(_) | Key::F(_)) {
            self.anchor = None;
            self.want_col = None;
        }
        self.scroll_to_cursor(r);
        EdAction::Redraw
    }

    fn dirty(&self) -> bool {
        self.dirty
    }

    fn mark_saved(&mut self) {
        self.dirty = false;
    }

    fn encode(&mut self, _ext: &str) -> Result<Vec<u8>, String> {
        let s = self.text();
        Ok(match self.enc {
            Enc::Utf8 => s.into_bytes(),
            Enc::Utf8Bom => {
                let mut v = Vec::from([0xEF, 0xBB, 0xBF]);
                v.extend_from_slice(s.as_bytes());
                v
            }
            Enc::Utf16Le | Enc::Utf16Be => {
                let le = self.enc == Enc::Utf16Le;
                let mut v = if le { Vec::from([0xFF, 0xFE]) } else { Vec::from([0xFE, 0xFF]) };
                for u in s.encode_utf16() {
                    v.extend_from_slice(&if le { u.to_le_bytes() } else { u.to_be_bytes() });
                }
                v
            }
            Enc::Latin1 => s
                .chars()
                .map(|c| {
                    let u = c as u32;
                    if u < 0x80 || (0xA0..0x100).contains(&u) {
                        u as u8
                    } else {
                        CP1252.iter().position(|&x| x as u32 == u).map(|i| 0x80 + i as u8).unwrap_or(b'?')
                    }
                })
                .collect(),
        })
    }

    fn print_pages(&mut self, paper: Paper) -> Vec<Canvas> {
        let (pw, ph) = crate::print::page_pixels(paper, 100);
        let (pw, ph) = (pw as i32, ph as i32);
        let margin = 70;
        let px = 16u8;
        let cw = mono_width(px);
        let lh = 18;
        let cols = ((pw - 2 * margin) / cw).max(10) as usize;
        let per_page = ((ph - 2 * margin) / lh).max(5) as usize;
        let wrap = self.wrap;
        self.wrap = true;
        let rows = self.rows(cols);
        self.wrap = wrap;
        let mut pages = Vec::new();
        for chunk in rows.chunks(per_page) {
            let mut c = Canvas::new(pw as usize, ph as usize);
            c.px.fill(0xFF_FFFF);
            for (k, &(l, s, e)) in chunk.iter().enumerate() {
                let line = &self.lines[l];
                let mut v = 0;
                for &ch in &line[s..e] {
                    if ch != '\t' && ch != ' ' {
                        c.mono_char(margin + v * cw, margin + k as i32 * lh, ch, px, 0);
                    }
                    v += if ch == '\t' { 4 } else { 1 };
                }
            }
            pages.push(c);
        }
        if pages.len() > 1 {
            let n = pages.len();
            for (i, p) in pages.iter_mut().enumerate() {
                let t = format!("{} / {}", i + 1, n);
                p.text_centered(pw / 2, ph - margin / 2 - 8, &t, Size::Small, false, rgb(90, 90, 90));
            }
        }
        pages
    }
}

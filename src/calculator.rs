//! Calculator: big buttons, the sum shown as you type it, × and ÷ before + and − (like a
//! phone), percent, square root, squares and 1/x, a tape of past sums (click one to use its
//! answer again), and the keyboard: digits, + - * /, Enter, Backspace, Esc.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::keys::{Key, Keys};

const BG: u32 = rgb(246, 247, 250);
const INK: u32 = rgb(28, 32, 40);
const DIM: u32 = rgb(110, 116, 126);
const NUM: u32 = rgb(255, 255, 255);
const OPC: u32 = rgb(226, 232, 244);
const EQ: u32 = rgb(46, 124, 232);
const FUNC: u32 = rgb(236, 238, 243);

#[derive(Clone, Copy, PartialEq)]
enum B {
    Digit(u8),
    Dot,
    Op(char),
    Equals,
    Clear,
    Back,
    Percent,
    Neg,
    Sqrt,
    Square,
    Inverse,
}

const GRID: [[(B, &str); 4]; 6] = [
    [(B::Sqrt, "\u{221A}"), (B::Square, "x\u{00B2}"), (B::Inverse, "1/x"), (B::Percent, "%")],
    [(B::Clear, "C"), (B::Back, "\u{2190}"), (B::Neg, "+/\u{2212}"), (B::Op('/'), "\u{00F7}")],
    [(B::Digit(7), "7"), (B::Digit(8), "8"), (B::Digit(9), "9"), (B::Op('*'), "\u{00D7}")],
    [(B::Digit(4), "4"), (B::Digit(5), "5"), (B::Digit(6), "6"), (B::Op('-'), "\u{2212}")],
    [(B::Digit(1), "1"), (B::Digit(2), "2"), (B::Digit(3), "3"), (B::Op('+'), "+")],
    [(B::Digit(0), "0"), (B::Dot, "."), (B::Equals, "="), (B::Equals, "=")],
];

#[derive(Clone)]
enum Tok {
    Num(f64),
    Op(char),
}

pub struct Calculator {
    /// The sum so far (numbers and operators, without the number being typed).
    toks: Vec<Tok>,
    /// The number being typed.
    entry: String,
    /// The answer last shown (after =), which an operator carries on from.
    answer: Option<f64>,
    error: bool,
    /// Past sums: (sum, answer).
    tape: Vec<(String, f64)>,
    keys: Keys,
    pressed: Option<(usize, usize)>,
}

/// A number the way people write it: 1,234.5 — at most 12 significant digits.
pub fn show(v: f64) -> String {
    if v.is_nan() || v.is_infinite() {
        return String::from("Can't do that");
    }
    let a = if v < 0.0 { -v } else { v };
    if a != 0.0 && !(1e-9..1e15).contains(&a) {
        // Scientific: 1.2345e+20.
        let mut e = 0i32;
        let mut m = a;
        while m >= 10.0 {
            m /= 10.0;
            e += 1;
        }
        while m < 1.0 {
            m *= 10.0;
            e -= 1;
        }
        let ms = trim(&fixed(m, 9));
        return format!("{}{}e{}{}", if v < 0.0 { "-" } else { "" }, ms, if e < 0 { "-" } else { "+" }, e.abs());
    }
    // Decimals so the whole thing has about 12 significant digits.
    let int_digits = if a < 1.0 { 1 } else { libm::floor(libm::log10(a)) as i32 + 1 };
    let decimals = (12 - int_digits).clamp(0, 10) as usize;
    let s = trim(&fixed(a, decimals));
    let (ip, fp) = match s.find('.') {
        Some(i) => (&s[..i], &s[i..]),
        None => (s.as_str(), ""),
    };
    let mut grouped = String::new();
    for (k, ch) in ip.chars().enumerate() {
        if k > 0 && (ip.len() - k) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(ch);
    }
    let neg = v < 0.0 && s.chars().any(|c| c.is_ascii_digit() && c != '0');
    format!("{}{}{}", if neg { "-" } else { "" }, grouped, fp)
}

fn fixed(v: f64, decimals: usize) -> String {
    format!("{:.*}", decimals, v)
}

fn trim(s: &str) -> String {
    if s.contains('.') {
        String::from(s.trim_end_matches('0').trim_end_matches('.'))
    } else {
        String::from(s)
    }
}

fn op_sym(c: char) -> char {
    match c {
        '*' => '\u{00D7}',
        '/' => '\u{00F7}',
        '-' => '\u{2212}',
        o => o,
    }
}

/// × and ÷ first, then + and −.
fn eval(toks: &[Tok]) -> Option<f64> {
    let mut terms: Vec<f64> = Vec::new();
    let mut ops: Vec<char> = Vec::new();
    let mut cur: Option<f64> = None;
    let mut pending: Option<char> = None;
    for t in toks {
        match t {
            Tok::Num(v) => {
                cur = Some(match (cur, pending.take()) {
                    (Some(c), Some('*')) => c * v,
                    (Some(c), Some('/')) => {
                        if *v == 0.0 {
                            return None;
                        }
                        c / v
                    }
                    _ => *v,
                });
            }
            Tok::Op(o) => match o {
                '*' | '/' => pending = Some(*o),
                _ => {
                    terms.push(cur.take()?);
                    ops.push(*o);
                }
            },
        }
    }
    terms.push(cur?);
    let mut total = terms[0];
    for (k, o) in ops.iter().enumerate() {
        let v = terms[k + 1];
        total = if *o == '+' { total + v } else { total - v };
    }
    Some(total)
}

impl Calculator {
    pub fn new() -> Calculator {
        Calculator { toks: Vec::new(), entry: String::new(), answer: None, error: false, tape: Vec::new(), keys: Keys::default(), pressed: None }
    }

    fn entry_value(&self) -> Option<f64> {
        if self.entry.is_empty() || self.entry == "-" {
            return None;
        }
        self.entry.parse::<f64>().ok()
    }

    /// The sum as written so far.
    fn expression(&self) -> String {
        let mut s = String::new();
        for t in &self.toks {
            match t {
                Tok::Num(v) => s += &show(*v),
                Tok::Op(o) => {
                    s.push(' ');
                    s.push(op_sym(*o));
                    s.push(' ');
                }
            }
        }
        s
    }

    /// What the big display shows.
    fn display(&self) -> String {
        if self.error {
            return String::from("Can't divide by zero");
        }
        if !self.entry.is_empty() {
            // Show what's typed, with thousands separators.
            let neg = self.entry.starts_with('-');
            let body = self.entry.trim_start_matches('-');
            let (ip, fp) = match body.find('.') {
                Some(i) => (&body[..i], &body[i..]),
                None => (body, ""),
            };
            let mut g = String::new();
            for (k, ch) in ip.chars().enumerate() {
                if k > 0 && (ip.len() - k) % 3 == 0 {
                    g.push(',');
                }
                g.push(ch);
            }
            if g.is_empty() {
                g.push('0');
            }
            return format!("{}{}{}", if neg { "-" } else { "" }, g, fp);
        }
        match (self.answer, self.toks.last()) {
            (Some(a), None) => show(a),
            (_, Some(Tok::Num(v))) => show(*v),
            _ => String::from("0"),
        }
    }

    /// Put the number being typed (or the last answer) into the sum.
    fn commit_entry(&mut self) -> bool {
        if let Some(v) = self.entry_value() {
            self.toks.push(Tok::Num(v));
            self.entry.clear();
            return true;
        }
        if self.toks.is_empty() {
            if let Some(a) = self.answer {
                self.toks.push(Tok::Num(a));
                return true;
            }
        }
        matches!(self.toks.last(), Some(Tok::Num(_)))
    }

    /// Change the current number (typed, or the answer) with f.
    fn apply(&mut self, f: impl Fn(f64) -> Option<f64>) {
        let cur = self.entry_value().or_else(|| if self.toks.is_empty() { self.answer } else { None }).or(match self.toks.last() {
            Some(Tok::Num(v)) => Some(*v),
            _ => None,
        });
        let Some(v) = cur else { return };
        match f(v) {
            Some(r) if r.is_finite() => {
                if matches!(self.toks.last(), Some(Tok::Num(_))) && self.entry.is_empty() {
                    self.toks.pop();
                }
                self.entry = trim(&fixed(r, 10));
                if self.entry == "-0" {
                    self.entry = String::from("0");
                }
                self.answer = None;
            }
            _ => self.error = true,
        }
    }

    fn press(&mut self, b: B) {
        if self.error && b != B::Clear {
            self.error = false;
            self.toks.clear();
            self.entry.clear();
            self.answer = None;
            if !matches!(b, B::Digit(_) | B::Dot) {
                return;
            }
        }
        match b {
            B::Digit(d) => {
                if self.toks.is_empty() && self.entry.is_empty() {
                    self.answer = None;
                }
                if self.entry.chars().filter(|c| c.is_ascii_digit()).count() < 15 {
                    if self.entry == "0" {
                        self.entry.clear();
                    } else if self.entry == "-0" {
                        self.entry = String::from("-");
                    }
                    self.entry.push((b'0' + d) as char);
                }
            }
            B::Dot => {
                if self.toks.is_empty() && self.entry.is_empty() {
                    self.answer = None;
                }
                if !self.entry.contains('.') {
                    if self.entry.is_empty() || self.entry == "-" {
                        self.entry.push('0');
                    }
                    self.entry.push('.');
                }
            }
            B::Op(o) => {
                if self.commit_entry() {
                    self.toks.push(Tok::Op(o));
                } else if let Some(Tok::Op(_)) = self.toks.last() {
                    // Change your mind about the operator.
                    self.toks.pop();
                    self.toks.push(Tok::Op(o));
                }
                self.answer = None;
            }
            B::Equals => {
                if !self.commit_entry() {
                    if let Some(Tok::Op(_)) = self.toks.last() {
                        self.toks.pop();
                    }
                }
                if self.toks.len() < 2 {
                    if let Some(Tok::Num(v)) = self.toks.first() {
                        self.answer = Some(*v);
                    }
                    self.toks.clear();
                    return;
                }
                let expr = self.expression();
                match eval(&self.toks) {
                    Some(v) => {
                        self.tape.push((expr, v));
                        if self.tape.len() > 50 {
                            self.tape.remove(0);
                        }
                        self.answer = Some(v);
                    }
                    None => self.error = true,
                }
                self.toks.clear();
            }
            B::Clear => {
                self.toks.clear();
                self.entry.clear();
                self.answer = None;
                self.error = false;
            }
            B::Back => {
                if !self.entry.is_empty() {
                    self.entry.pop();
                    if self.entry == "-" {
                        self.entry.clear();
                    }
                } else if let Some(Tok::Op(_)) = self.toks.last() {
                    self.toks.pop();
                    if let Some(Tok::Num(v)) = self.toks.pop() {
                        self.entry = trim(&fixed(v, 10));
                    }
                }
            }
            B::Neg => {
                if !self.entry.is_empty() {
                    if let Some(rest) = self.entry.strip_prefix('-') {
                        self.entry = String::from(rest);
                    } else {
                        self.entry.insert(0, '-');
                    }
                } else {
                    self.apply(|v| Some(-v));
                }
            }
            B::Percent => {
                // 50 + 10% means 50 + 5 (ten percent of 50); otherwise just /100.
                let base = match (self.toks.len() >= 2, self.toks.last()) {
                    (true, Some(Tok::Op('+'))) | (true, Some(Tok::Op('-'))) => eval(&self.toks[..self.toks.len() - 1]),
                    _ => None,
                };
                match base {
                    Some(b) => self.apply(|v| Some(b * v / 100.0)),
                    None => self.apply(|v| Some(v / 100.0)),
                }
            }
            B::Sqrt => self.apply(|v| if v < 0.0 { None } else { Some(libm::sqrt(v)) }),
            B::Square => self.apply(|v| Some(v * v)),
            B::Inverse => self.apply(|v| if v == 0.0 { None } else { Some(1.0 / v) }),
        }
    }

    // ---- layout ---------------------------------------------------------------------------

    fn tape_w(r: Rect) -> i32 {
        if r.w > 700 {
            (r.w / 3).min(320)
        } else {
            0
        }
    }

    fn pad(r: Rect) -> Rect {
        let tw = Self::tape_w(r);
        let w = r.w - tw;
        Rect::new(r.x, r.y + 150, w, r.h - 150)
    }

    fn button(r: Rect, row: usize, col: usize) -> Rect {
        let p = Self::pad(r);
        let gap = 8;
        let bw = (p.w - 24 - 3 * gap) / 4;
        let bh = ((p.h - 16 - 5 * gap) / 6).max(30);
        let span = if row == 5 && col == 2 { 2 } else { 1 };
        Rect::new(p.x + 12 + col as i32 * (bw + gap), p.y + row as i32 * (bh + gap), bw * span + gap * (span - 1), bh)
    }

    fn tape_row(r: Rect, i: usize) -> Rect {
        let tw = Self::tape_w(r);
        Rect::new(r.x + r.w - tw + 10, r.y + 52 + i as i32 * 58, tw - 20, 52)
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y, r.w, r.h, BG);
        let p = Self::pad(r);
        // Display.
        let expr = self.expression();
        let ew = text_width(&expr, Size::Medium, false);
        c.text(p.x + p.w - 22 - ew.min(p.w - 40), r.y + 24, &crate::photos::ellipsize_left(&expr, p.w - 40, Size::Medium, false), Size::Medium, false, DIM);
        let shown = self.display();
        // As big as fits.
        use crate::font::{width, Face};
        let mut px: u16 = 64;
        while px > 20 && width(&shown, Face::Sans, px) > p.w - 40 {
            px -= 4;
        }
        let sw = width(&shown, Face::Sans, px);
        c.text_face((p.x + p.w - 22 - sw) * 64, r.y + 70 + px as i32 * 3 / 4, &shown, Face::Sans, px, if self.error { rgb(200, 40, 40) } else { INK });
        // Buttons.
        for (row, line) in GRID.iter().enumerate() {
            for (col, &(b, label)) in line.iter().enumerate() {
                if row == 5 && col == 3 {
                    continue;
                }
                let br = Self::button(r, row, col);
                let (bg, fg) = match b {
                    B::Digit(_) | B::Dot => (NUM, INK),
                    B::Equals => (EQ, rgb(255, 255, 255)),
                    B::Op(_) => (OPC, INK),
                    B::Clear => (rgb(252, 228, 226), rgb(180, 40, 30)),
                    _ => (FUNC, INK),
                };
                let down = self.pressed == Some((row, col));
                c.shadow(br.x, br.y + 2, br.w, br.h, 12, 4, if down { 0 } else { 40 });
                c.round_rect(br.x, br.y + if down { 1 } else { 0 }, br.w, br.h, 12, if down { crate::gfx::mix(bg, 0, 30) } else { bg });
                let size = (br.h / 2).clamp(16, 30) as u16;
                let lw = width(label, Face::Sans, size);
                match b {
                    B::Sqrt => {
                        // A radical sign drawn with lines (the font has none).
                        let (cx, cy) = (br.x + br.w / 2, br.y + br.h / 2);
                        let s = (br.h / 5).clamp(6, 12);
                        thick(c, cx - 2 * s, cy, cx - s, cy + s + s / 2, 2, fg);
                        thick(c, cx - s, cy + s + s / 2, cx, cy - 2 * s + s / 2, 2, fg);
                        thick(c, cx, cy - 2 * s + s / 2, cx + 2 * s, cy - 2 * s + s / 2, 2, fg);
                    }
                    B::Back => {
                        // A backspace key: an arrow-shaped tag with a cross.
                        let (cx, cy) = (br.x + br.w / 2, br.y + br.h / 2);
                        let s = (br.h / 5).clamp(6, 11);
                        thick(c, cx - 2 * s, cy, cx - s, cy - s, 2, fg);
                        thick(c, cx - 2 * s, cy, cx - s, cy + s, 2, fg);
                        thick(c, cx - s, cy - s, cx + 2 * s, cy - s, 2, fg);
                        thick(c, cx - s, cy + s, cx + 2 * s, cy + s, 2, fg);
                        thick(c, cx + 2 * s, cy - s, cx + 2 * s, cy + s, 2, fg);
                        thick(c, cx - s / 3, cy - s / 2, cx + s + s / 3, cy + s / 2, 2, fg);
                        thick(c, cx - s / 3, cy + s / 2, cx + s + s / 3, cy - s / 2, 2, fg);
                    }
                    _ => {
                        c.text_face((br.x + (br.w - lw) / 2) * 64, br.y + br.h / 2 + size as i32 * 36 / 100, label, Face::Sans, size, fg);
                    }
                }
            }
        }
        // Tape.
        let tw = Self::tape_w(r);
        if tw > 0 {
            let tx = r.x + r.w - tw;
            c.fill_rect(tx, r.y, tw, r.h, rgb(238, 240, 245));
            c.fill_rect(tx, r.y, 1, r.h, rgb(218, 222, 230));
            c.text(tx + 16, r.y + 18, "History", Size::Medium, true, INK);
            if self.tape.is_empty() {
                c.text(tx + 16, r.y + 60, "Your sums show up here.", Size::Small, false, DIM);
                c.text(tx + 16, r.y + 82, "Click one to use its answer.", Size::Small, false, DIM);
            }
            let n = ((r.h - 60) / 58).max(0) as usize;
            for (i, (e, v)) in self.tape.iter().rev().take(n).enumerate() {
                let tr = Self::tape_row(r, i);
                c.round_rect(tr.x, tr.y, tr.w, tr.h, 8, rgb(255, 255, 255));
                let e = crate::photos::ellipsize_left(&format!("{} =", e), tr.w - 20, Size::Small, false);
                c.text(tr.x + tr.w - 10 - text_width(&e, Size::Small, false), tr.y + 6, &e, Size::Small, false, DIM);
                let a = show(*v);
                c.text(tr.x + tr.w - 10 - text_width(&a, Size::Medium, true), tr.y + 26, &a, Size::Medium, true, INK);
            }
        }
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect) -> bool {
        for (row, line) in GRID.iter().enumerate() {
            for (col, &(b, _)) in line.iter().enumerate() {
                if row == 5 && col == 3 {
                    continue;
                }
                if Self::button(r, row, col).contains(x, y) {
                    self.pressed = Some((row, col));
                    self.press(b);
                    return true;
                }
            }
        }
        if Self::tape_w(r) > 0 {
            let n = self.tape.len();
            for i in 0..n {
                if Self::tape_row(r, i).contains(x, y) {
                    let v = self.tape[n - 1 - i].1;
                    if self.entry.is_empty() && matches!(self.toks.last(), Some(Tok::Op(_))) {
                        self.entry = trim(&fixed(v, 10));
                    } else {
                        self.toks.clear();
                        self.entry.clear();
                        self.answer = Some(v);
                    }
                    return true;
                }
            }
        }
        false
    }

    pub fn on_release(&mut self) -> bool {
        self.pressed.take().is_some()
    }

    pub fn on_key(&mut self, sc: u8) -> Option<bool> {
        let k = self.keys.feed(sc)?;
        let b = match k {
            Key::Char(c @ '0'..='9') => B::Digit(c as u8 - b'0'),
            Key::Char('.') | Key::Char(',') => B::Dot,
            Key::Char('+') => B::Op('+'),
            Key::Char('-') => B::Op('-'),
            Key::Char('*') | Key::Char('x') | Key::Char('X') => B::Op('*'),
            Key::Char('/') => B::Op('/'),
            Key::Char('=') | Key::Enter => B::Equals,
            Key::Char('%') => B::Percent,
            Key::Backspace => B::Back,
            Key::Delete => B::Clear,
            Key::Esc => {
                if self.toks.is_empty() && self.entry.is_empty() && !self.error {
                    return None; // Esc on a clear calculator closes it
                }
                B::Clear
            }
            Key::Ctrl('c') => {
                crate::keys::copy(&self.display().replace(',', ""));
                return Some(false);
            }
            Key::Ctrl('v') => {
                let t = crate::keys::paste().replace(',', "");
                if let Ok(v) = t.trim().parse::<f64>() {
                    self.entry = trim(&fixed(v, 10));
                    return Some(true);
                }
                return Some(false);
            }
            _ => return Some(false),
        };
        self.press(b);
        Some(true)
    }
}

/// A smooth line `w` pixels thick.
fn thick(c: &mut Canvas, x0: i32, y0: i32, x1: i32, y1: i32, w: i32, col: u32) {
    // A raster just big enough for the line.
    let (ox, oy) = (x0.min(x1) - w - 1, y0.min(y1) - w - 1);
    let (rw, rh) = (((x1 - x0).abs() + 2 * w + 3) as usize, ((y1 - y0).abs() + 2 * w + 3) as usize);
    let mut r = crate::raster::Raster::new(rw, rh);
    let (dx, dy) = ((x1 - x0) as f32, (y1 - y0) as f32);
    let len = libm::sqrtf(dx * dx + dy * dy).max(0.01);
    let (nx, ny) = (-dy / len * w as f32 / 2.0, dx / len * w as f32 / 2.0);
    let (ax, ay, bx, by) = ((x0 - ox) as f32 + 0.5, (y0 - oy) as f32 + 0.5, (x1 - ox) as f32 + 0.5, (y1 - oy) as f32 + 0.5);
    r.move_to(ax + nx, ay + ny);
    r.line_to(bx + nx, by + ny);
    r.line_to(bx - nx, by - ny);
    r.line_to(ax - nx, ay - ny);
    r.close();
    r.rows(false, |y, x, cov| {
        for (k, &a) in cov.iter().enumerate() {
            if a > 0 {
                c.blend(ox + (x + k) as i32, oy + y as i32, col, a as u32);
            }
        }
    });
}

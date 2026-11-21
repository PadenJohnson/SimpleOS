//! Writer's document: paragraphs of characters, each character with its own style, and
//! paragraph settings (alignment, lists, headings). Layout breaks it into lines and pages in
//! 1/64-point units using the fonts' own measurements, so the screen, printing and PDFs all
//! break lines in exactly the same places.

use alloc::vec;
use alloc::vec::Vec;

use crate::font::{Face, FONTS};
use crate::gfx::rgb;

/// Page break (a paragraph holding just this starts a new page).
pub const PAGE_BREAK: char = '\u{0C}';

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Style {
    pub serif: bool,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strike: bool,
    /// Size in half points (22 = 11 pt).
    pub size: u16,
    pub color: u32,
    /// Highlight colour, or None.
    pub highlight: Option<u32>,
}

impl Style {
    pub const NORMAL: Style = Style { serif: false, bold: false, italic: false, underline: false, strike: false, size: 22, color: 0, highlight: None };
    pub fn face(&self) -> Face {
        Face::new(self.serif, self.bold, self.italic)
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Align {
    Left,
    Center,
    Right,
    Justify,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum List {
    None,
    Bullet,
    Number,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Block {
    Normal,
    Title,
    H1,
    H2,
    H3,
}

impl Block {
    pub const ALL: [Block; 5] = [Block::Normal, Block::Title, Block::H1, Block::H2, Block::H3];
    pub fn name(self) -> &'static str {
        match self {
            Block::Normal => "Normal text",
            Block::Title => "Title",
            Block::H1 => "Heading 1",
            Block::H2 => "Heading 2",
            Block::H3 => "Heading 3",
        }
    }
    /// The character style this kind of paragraph starts with.
    pub fn style(self, base: Style) -> Style {
        let s = Style { bold: false, italic: false, underline: false, strike: false, highlight: None, ..base };
        match self {
            Block::Normal => Style { size: 22, color: 0, ..s },
            Block::Title => Style { size: 52, color: rgb(20, 30, 50), ..s },
            Block::H1 => Style { size: 32, bold: true, color: rgb(30, 70, 140), ..s },
            Block::H2 => Style { size: 26, bold: true, color: rgb(30, 70, 140), ..s },
            Block::H3 => Style { size: 24, bold: true, color: rgb(50, 60, 80), ..s },
        }
    }
    /// Space above, in points.
    pub fn space_before(self) -> i32 {
        match self {
            Block::Normal => 0,
            Block::Title => 0,
            Block::H1 => 18,
            Block::H2 => 12,
            Block::H3 => 10,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct PStyle {
    pub align: Align,
    pub list: List,
    pub block: Block,
    /// Indent level (0.5 inch each).
    pub indent: u8,
    /// Space after, in points.
    pub after: u16,
    /// Line spacing in percent.
    pub line: u16,
}

impl PStyle {
    pub const NORMAL: PStyle = PStyle { align: Align::Left, list: List::None, block: Block::Normal, indent: 0, after: 8, line: 100 };
}

#[derive(Clone, Debug)]
pub struct Para {
    pub text: Vec<char>,
    pub st: Vec<Style>,
    pub p: PStyle,
    /// Style for an empty paragraph (and for typing at its start).
    pub end: Style,
}

impl Para {
    pub fn new(p: PStyle, end: Style) -> Para {
        Para { text: Vec::new(), st: Vec::new(), p, end }
    }
    pub fn is_page_break(&self) -> bool {
        self.text.len() == 1 && self.text[0] == PAGE_BREAK
    }
    /// The style at position i (of the character before it, or the paragraph's).
    pub fn style_at(&self, i: usize) -> Style {
        if i > 0 && i <= self.st.len() {
            self.st[i - 1]
        } else if !self.st.is_empty() {
            self.st[0]
        } else {
            self.end
        }
    }
}

#[derive(Clone)]
pub struct Doc {
    pub paras: Vec<Para>,
    /// Page size and margins, in points.
    pub page_w: i32,
    pub page_h: i32,
    pub margin: [i32; 4], // left, top, right, bottom
}

impl Doc {
    pub fn new() -> Doc {
        Doc { paras: vec![Para::new(PStyle::NORMAL, Style::NORMAL)], page_w: 612, page_h: 792, margin: [72, 72, 72, 72] }
    }

    pub fn plain_text(&self) -> alloc::string::String {
        let mut s = alloc::string::String::new();
        for (i, p) in self.paras.iter().enumerate() {
            if i > 0 {
                s.push('\n');
            }
            s.extend(p.text.iter().filter(|&&c| c != PAGE_BREAK));
        }
        s
    }
}

/// One laid-out line.
#[derive(Clone)]
pub struct Line {
    pub para: usize,
    pub start: usize,
    pub end: usize,
    pub page: usize,
    /// Left edge of the text and top of the line within the page, 1/64 pt.
    pub x: i32,
    pub y: i32,
    pub h: i32,
    /// Baseline below the top.
    pub base: i32,
    /// Where each character starts (relative to x), plus the end: end - start + 1 entries.
    pub xs: Vec<i32>,
    /// The list marker ("•", "3.") to draw left of the first line, at this x.
    pub marker: Option<(alloc::string::String, i32)>,
}

pub struct Layout {
    pub lines: Vec<Line>,
    pub pages: usize,
}

/// Width of a character in 1/64 pt.
pub fn adv64(f: &mut crate::font::Fonts, st: &Style, ch: char) -> i32 {
    let face = st.face();
    let (upem, ..) = f.vmetrics(face);
    let ch = if ch == '\t' { ' ' } else { ch };
    let u = f.char_units(face, ch);
    (u as i64 * st.size as i64 * 32 / upem as i64) as i32
}

/// Ascent and descent (positive) of a style, 1/64 pt.
pub fn vert64(f: &mut crate::font::Fonts, st: &Style) -> (i32, i32) {
    let (upem, asc, desc, _) = f.vmetrics(st.face());
    let k = st.size as i64 * 32;
    ((asc as i64 * k / upem as i64) as i32, (desc as i64 * k / upem as i64) as i32)
}

const TAB64: i32 = 36 * 64; // half-inch tab stops

pub fn layout(doc: &Doc) -> Layout {
    let mut f = FONTS.lock();
    let content_w = (doc.page_w - doc.margin[0] - doc.margin[2]) * 64;
    let content_h = (doc.page_h - doc.margin[1] - doc.margin[3]) * 64;
    let mut lines: Vec<Line> = Vec::new();
    let mut page = 0;
    let mut y = 0i32;
    let mut number = 0u32;
    for (pi, p) in doc.paras.iter().enumerate() {
        if p.is_page_break() {
            let (a, d) = vert64(&mut f, &p.style_at(0));
            lines.push(Line { para: pi, start: 0, end: 1, page, x: 0, y, h: a + d, base: a, xs: vec![0, 0], marker: None });
            page += 1;
            y = 0;
            continue;
        }
        number = if p.p.list == List::Number { number + 1 } else { 0 };
        let indent = p.p.indent as i32 * 36 * 64 + if p.p.list != List::None { 18 * 64 } else { 0 };
        let width = (content_w - indent).max(72 * 64);
        let before = p.p.block.space_before() * 64;
        if y > 0 {
            y += before;
        }
        // Character advances.
        let n = p.text.len();
        let mut adv: Vec<i32> = Vec::with_capacity(n);
        for i in 0..n {
            adv.push(adv64(&mut f, &p.st[i], p.text[i]));
        }
        // Break into lines: (start, end).
        let mut breaks: Vec<(usize, usize)> = Vec::new();
        let mut start = 0;
        while start < n || breaks.is_empty() {
            let mut w = 0;
            let mut i = start;
            let mut last_space: Option<usize> = None;
            while i < n {
                let ch = p.text[i];
                if ch == '\n' {
                    i += 1;
                    break;
                }
                let a = if ch == '\t' { TAB64 - (w % TAB64) } else { adv[i] };
                if w + a > width && i > start && ch != ' ' {
                    if let Some(s) = last_space {
                        i = s + 1;
                    }
                    break;
                }
                if ch == ' ' {
                    last_space = Some(i);
                }
                w += a;
                i += 1;
            }
            breaks.push((start, i));
            if i >= n {
                break;
            }
            start = i;
        }
        let marker = match p.p.list {
            List::None => None,
            List::Bullet => Some(alloc::string::String::from(if p.p.indent % 2 == 1 { "\u{25E6}" } else { "\u{2022}" })),
            List::Number => Some(alloc::format!("{}.", number)),
        };
        let nb = breaks.len();
        for (k, &(s, e)) in breaks.iter().enumerate() {
            // Height from the tallest character (or the paragraph style if empty).
            let (mut asc, mut desc) = vert64(&mut f, &p.style_at(s));
            for i in s..e {
                let (a, d) = vert64(&mut f, &p.st[i]);
                asc = asc.max(a);
                desc = desc.max(d);
            }
            let h = (asc + desc) * p.p.line as i32 / 100;
            if y + h > content_h && y > 0 {
                page += 1;
                y = 0;
            }
            // Positions.
            let mut xs = Vec::with_capacity(e - s + 1);
            let mut x = 0;
            for i in s..e {
                xs.push(x);
                let ch = p.text[i];
                x += if ch == '\t' { TAB64 - (x % TAB64) } else if ch == '\n' { 0 } else { adv[i] };
            }
            xs.push(x);
            // Trailing spaces don't count for alignment.
            let mut vis_end = e;
            while vis_end > s && matches!(p.text[vis_end - 1], ' ' | '\n') {
                vis_end -= 1;
            }
            let used = xs[vis_end - s];
            let slack = (width - used).max(0);
            let last = k + 1 == nb || p.text[e - 1] == '\n';
            let shift = match p.p.align {
                Align::Center => slack / 2,
                Align::Right => slack,
                _ => 0,
            };
            if p.p.align == Align::Justify && !last {
                let spaces = (s..vis_end).filter(|&i| p.text[i] == ' ').count() as i32;
                if spaces > 0 {
                    let mut extra = 0;
                    let mut seen = 0;
                    for i in s..e {
                        xs[i - s] += extra;
                        if p.text[i] == ' ' && i < vis_end {
                            seen += 1;
                            extra = slack * seen / spaces;
                        }
                    }
                    let l = xs.len() - 1;
                    xs[l] += extra;
                }
            }
            let mk = if k == 0 { marker.clone().map(|m| (m, indent - 18 * 64)) } else { None };
            lines.push(Line { para: pi, start: s, end: e, page, x: indent + shift, y, h, base: asc, xs, marker: mk });
            y += h;
        }
        y += p.p.after as i32 * 64;
    }
    Layout { lines, pages: page + 1 }
}

impl Layout {
    /// The line holding position (para, i). At a soft break the position belongs to the
    /// start of the next line.
    pub fn line_of(&self, para: usize, i: usize) -> usize {
        let mut best = 0;
        for (k, l) in self.lines.iter().enumerate() {
            if l.para == para {
                if i >= l.start && i < l.end {
                    return k;
                }
                if i >= l.start {
                    best = k;
                }
            } else if l.para > para {
                break;
            }
        }
        best
    }
}

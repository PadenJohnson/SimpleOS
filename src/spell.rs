//! Spelling, shared by every app that edits text (Writer, Text Editor, text boxes in
//! Calendar, Spreadsheet, Movie titles...). One dictionary, one personal word list, one
//! setting; apps ask `misspelled` which words to underline (`squiggle`) and open a `Popup`
//! for suggestions on a right-click, F7, Shift+F10 or the Menu key.
//!
//! The dictionary is SCOWL's English lists up to size 60 (assets/spell/en.dict, made by
//! tools/spell/mkdict.py; see assets/spell/SCOWL-Copyright.txt), with American and British
//! spellings told apart. Words the user adds are kept in "Dictionary.txt" in the library
//! (the desktop saves it when `take_dirty` says so).

use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use spin::{Lazy, Mutex};

use crate::gfx::{rgb, text_width, Canvas, Rect, Size};

/// The spelling setting's choices (index = `Settings::spelling`).
pub const CHOICES: [&str; 3] = ["English (US)", "English (UK)", "Off"];

static MODE: AtomicU8 = AtomicU8::new(0);
static DIRTY: AtomicBool = AtomicBool::new(false);
/// Words added by the user (kept), and words ignored until the computer restarts.
static USER: Mutex<Vec<String>> = Mutex::new(Vec::new());
static IGNORED: Mutex<Vec<String>> = Mutex::new(Vec::new());

/// Choose English (US) (0), English (UK) (1) or off (2).
pub fn set_mode(m: usize) {
    MODE.store(m.min(2) as u8, Ordering::Relaxed);
}

pub fn enabled() -> bool {
    MODE.load(Ordering::Relaxed) < 2
}

// ---- the dictionary ----------------------------------------------------------------

struct Dict {
    /// "word" + tag + "\n", sorted.
    text: Vec<u8>,
    /// Where each word starts in `text`.
    starts: Vec<u32>,
    /// Open-addressed hash table of (word number + 1, hash); 0 = empty.
    table: Vec<(u32, u32)>,
}

fn hash(w: &[u8]) -> u32 {
    let mut h: u32 = 0x811c_9dc5;
    for &b in w {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h
}

static DICT: Lazy<Dict> = Lazy::new(|| {
    let text = miniz_oxide::inflate::decompress_to_vec_zlib(include_bytes!("../assets/spell/en.dict")).unwrap_or_default();
    let mut starts = Vec::with_capacity(100_000);
    let mut s = 0;
    for (i, &b) in text.iter().enumerate() {
        if b == b'\n' {
            if i > s + 1 {
                starts.push(s as u32);
            }
            s = i + 1;
        }
    }
    let size = (starts.len() * 2).next_power_of_two().max(16);
    starts.push(text.len() as u32); // the end of the last word
    let mut d = Dict { text, starts, table: alloc::vec![(0, 0); size] };
    for n in 0..d.starts.len() - 1 {
        let h = hash(d.entry(n).0);
        let mut k = h as usize & (size - 1);
        while d.table[k].0 != 0 {
            k = (k + 1) & (size - 1);
        }
        d.table[k] = (n as u32 + 1, h);
    }
    kprintln!("spell: {} words", d.starts.len() - 1);
    d
});

impl Dict {
    /// Word `n` and its tag (each line is the word, one tag byte and a newline).
    fn entry(&self, n: usize) -> (&[u8], u8) {
        let (st, next) = (self.starts[n] as usize, self.starts[n + 1] as usize);
        (&self.text[st..next - 2], self.text[next - 2] - b'A')
    }
    /// (commonness 0..6, region 0 all / 1 US / 2 UK) of a word spelled exactly so.
    fn find(&self, w: &[u8]) -> Option<(u8, u8)> {
        let mask = self.table.len() - 1;
        let h = hash(w);
        let mut k = h as usize & mask;
        loop {
            let (n, eh) = self.table[k];
            if n == 0 {
                return None;
            }
            if eh == h {
                let (e, tag) = self.entry(n as usize - 1);
                if e == w {
                    return Some((tag / 3, tag % 3));
                }
            }
            k = (k + 1) & mask;
        }
    }
}

/// How common a word spelled exactly so is (0 = very common .. 6), if it's right in the
/// chosen English.
fn level(w: &[u8]) -> Option<u8> {
    let region = MODE.load(Ordering::Relaxed) + 1; // 1 US, 2 UK
    match DICT.find(w) {
        Some((lv, r)) if r == 0 || r == region => Some(lv),
        _ => None,
    }
}

fn user_has(w: &str) -> bool {
    USER.lock().iter().any(|u| u == w) || IGNORED.lock().iter().any(|u| u == w)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Case {
    Lower,
    /// First letter capital, the rest small.
    Title,
    Upper,
    Mixed,
}

fn case_of(w: &str) -> Case {
    let mut cs = w.chars().filter(|c| c.is_alphabetic());
    let Some(first) = cs.next() else { return Case::Lower };
    let rest: Vec<char> = cs.collect();
    let rest_lower = rest.iter().all(|c| !c.is_uppercase());
    let rest_upper = rest.iter().all(|c| !c.is_lowercase());
    match (first.is_uppercase(), rest_lower, rest_upper) {
        (false, true, _) => Case::Lower,
        (true, true, _) => Case::Title,
        (true, false, true) => Case::Upper,
        _ => Case::Mixed,
    }
}

fn title(w: &str) -> String {
    let mut cs = w.chars();
    match cs.next() {
        Some(f) => f.to_uppercase().chain(cs).collect(),
        None => String::new(),
    }
}

fn spelled_ok(w: &str) -> bool {
    if level(w.as_bytes()).is_some() || user_has(w) {
        return true;
    }
    let lower = w.to_lowercase();
    match case_of(w) {
        // "The" at the start of a sentence; "HELLO" and "PARIS" in capitals.
        Case::Title => level(lower.as_bytes()).is_some() || user_has(&lower),
        Case::Upper => {
            let t = title(&lower);
            level(lower.as_bytes()).is_some() || level(t.as_bytes()).is_some() || user_has(&lower) || user_has(&t)
        }
        _ => false,
    }
}

/// Is this word (letters and apostrophes, as `words` finds them) spelled right? Always true
/// when spelling is off.
pub fn check(word: &str) -> bool {
    if !enabled() {
        return true;
    }
    let w: String = word.chars().map(|c| if c == '\u{2019}' { '\'' } else { c }).collect();
    if spelled_ok(&w) {
        return true;
    }
    // Possessives and "it's", "she's": right when the word before 's is.
    if let Some(base) = w.strip_suffix("'s").or_else(|| w.strip_suffix("'S")) {
        return base.chars().count() > 0 && spelled_ok(base);
    }
    false
}

// ---- finding words in text ---------------------------------------------------------

fn is_apos(c: char) -> bool {
    c == '\'' || c == '\u{2019}'
}

/// The words worth checking in `text`, as character ranges. Skips web and e-mail addresses,
/// file names and anything with digits, and single letters.
pub fn words(text: &[char]) -> Vec<(usize, usize)> {
    // Chunks are separated by spaces and odd characters (pictures, page breaks).
    let sep = |c: char| c.is_whitespace() || c.is_control() || c == '\u{FFFC}';
    let mut out = Vec::new();
    let mut i = 0;
    while i < text.len() {
        if sep(text[i]) {
            i += 1;
            continue;
        }
        let s = i;
        while i < text.len() && !sep(text[i]) {
            i += 1;
        }
        let chunk = &text[s..i];
        let addressy = chunk.iter().any(|&c| c.is_numeric() || "@/\\_<>{}=#%$~|^`*+&".contains(c))
            || chunk.windows(2).any(|p| p[0] == '.' && p[1].is_alphanumeric());
        if addressy {
            continue;
        }
        let mut k = s;
        while k < i {
            if !text[k].is_alphabetic() {
                k += 1;
                continue;
            }
            let ws = k;
            while k < i && (text[k].is_alphabetic() || (is_apos(text[k]) && k + 1 < i && text[k + 1].is_alphabetic())) {
                k += 1;
            }
            if k - ws > 1 {
                out.push((ws, k));
            }
        }
    }
    out
}

/// The misspelled words in `text` (character ranges), except one touching `caret` (the word
/// being typed). Empty when spelling is off.
pub fn misspelled(text: &[char], caret: Option<usize>) -> Vec<(usize, usize)> {
    if !enabled() {
        return Vec::new();
    }
    words(text)
        .into_iter()
        .filter(|&(s, e)| !caret.is_some_and(|c| c >= s && c <= e))
        .filter(|&(s, e)| !check(&text[s..e].iter().collect::<String>()))
        .collect()
}

/// The next misspelled word after position `from` in text of `n` lines (paragraphs), going
/// round to the start: (line, start, end).
pub fn next_misspelled<'a>(n: usize, line: impl Fn(usize) -> &'a [char], from: (usize, usize)) -> Option<(usize, usize, usize)> {
    if !enabled() || n == 0 {
        return None;
    }
    for k in 0..=n {
        let li = (from.0 + k) % n;
        let found = misspelled(line(li), None).into_iter().find(|&(s, _)| match k {
            0 => s > from.1,
            _ if k == n => s <= from.1,
            _ => true,
        });
        if let Some((s, e)) = found {
            return Some((li, s, e));
        }
    }
    None
}

/// The checkable word at or just before character `i`, if it's misspelled.
pub fn misspelled_at(text: &[char], i: usize) -> Option<(usize, usize)> {
    if !enabled() {
        return None;
    }
    words(text).into_iter().find(|&(s, e)| i >= s && i <= e).filter(|&(s, e)| !check(&text[s..e].iter().collect::<String>()))
}

// ---- suggestions -------------------------------------------------------------------

/// Keys next to each other on a QWERTY keyboard (typos usually hit a neighbour).
fn neighbours(a: u8, b: u8) -> bool {
    const ROWS: [&[u8]; 3] = [b"qwertyuiop", b"asdfghjkl", b"zxcvbnm"];
    let pos = |c: u8| ROWS.iter().enumerate().find_map(|(r, row)| row.iter().position(|&x| x == c).map(|p| (r as i32, p as i32)));
    match (pos(a), pos(b)) {
        (Some((ra, pa)), Some((rb, pb))) => (ra - rb).abs() <= 1 && (pa - pb).abs() <= 1,
        _ => false,
    }
}

/// Every spelling one edit away from `w` (delete, swap neighbours, replace, insert), with how
/// likely that edit is as a typo (lower = likelier).
fn edits(w: &[u8], f: &mut dyn FnMut(&[u8], i32)) {
    let mut buf: Vec<u8> = Vec::with_capacity(w.len() + 1);
    const LETTERS: &[u8] = b"abcdefghijklmnopqrstuvwxyz'";
    for i in 0..w.len() {
        buf.clear();
        buf.extend_from_slice(&w[..i]);
        buf.extend_from_slice(&w[i + 1..]);
        // Doubled letters ("occassion") are the likeliest slip.
        f(&buf, if i > 0 && w[i - 1] == w[i] { 2 } else if b"aeiou".contains(&w[i]) { 4 } else { 6 });
    }
    for i in 0..w.len().saturating_sub(1) {
        buf.clear();
        buf.extend_from_slice(w);
        buf.swap(i, i + 1);
        f(&buf, 3);
    }
    for i in 0..w.len() {
        for &c in LETTERS {
            if c != w[i] {
                buf.clear();
                buf.extend_from_slice(w);
                buf[i] = c;
                let vowels = |x: u8| b"aeiou".contains(&x);
                f(&buf, if neighbours(c, w[i]) { 4 } else if vowels(c) && vowels(w[i]) { 4 } else { 7 });
            }
        }
    }
    for i in 0..=w.len() {
        for &c in LETTERS {
            buf.clear();
            buf.extend_from_slice(&w[..i]);
            buf.push(c);
            buf.extend_from_slice(&w[i..]);
            f(&buf, if (i > 0 && w[i - 1] == c) || (i < w.len() && w[i] == c) { 2 } else if b"aeiou".contains(&c) { 4 } else { 6 });
        }
    }
}

/// Up to `max` corrections for a misspelled word, likeliest first, in the word's own
/// capitals ("Teh" -> "The").
pub fn suggest(word: &str, max: usize) -> Vec<String> {
    let w: String = word.chars().map(|c| if c == '\u{2019}' { '\'' } else { c }).collect();
    let case = case_of(&w);
    let lower = w.to_lowercase();
    let lw = lower.as_bytes();
    // (cost, word as spelled in the dictionary)
    let mut found: Vec<(i32, String)> = Vec::new();
    let consider = |cand: &[u8], cost: i32, found: &mut Vec<(i32, String)>| {
        // As typed in small letters, or a name ("paris" -> "Paris") or an abbreviation.
        let mut tmp = [0u8; 48];
        let n = cand.len();
        let hit = if let Some(lv) = level(cand) {
            Some((lv, 0))
        } else if n <= tmp.len() && cand[0].is_ascii_lowercase() {
            tmp[..n].copy_from_slice(cand);
            tmp[0] = tmp[0].to_ascii_uppercase();
            if let Some(lv) = level(&tmp[..n]) {
                Some((lv, 1))
            } else if n <= 6 {
                tmp[..n].make_ascii_uppercase();
                level(&tmp[..n]).map(|lv| (lv, 2))
            } else {
                None
            }
        } else {
            None
        };
        let Some((lv, form)) = hit else { return };
        let Ok(s) = core::str::from_utf8(if form == 0 { cand } else { &tmp[..n] }) else { return };
        let c = cost * 10 + lv as i32 * 3 + if cand.first() != lw.first() { 12 } else { 0 };
        match found.iter_mut().find(|f| f.1 == s) {
            Some(f) => f.0 = f.0.min(c),
            None => found.push((c, String::from(s))),
        }
    };
    // Same letters, different capitals ("iphone" -> "iPhone"; "paris" -> "Paris").
    consider(lw, 0, &mut found);
    let mut first: Vec<(Vec<u8>, i32)> = Vec::new();
    edits(lw, &mut |cand, cost| {
        consider(cand, cost, &mut found);
        first.push((cand.to_vec(), cost));
    });
    // Two edits away, when one wasn't enough.
    if found.len() < 3 && lw.len() <= 14 {
        for (c1, cost1) in &first {
            edits(c1, &mut |cand, cost| consider(cand, 6 + cost1 + cost, &mut found));
        }
    }
    // Two words run together ("alot" -> "a lot").
    for i in 1..lw.len() {
        let (a, b) = (&lw[..i], &lw[i..]);
        // Only common words ("a lot", "can not"), not "col our" or "occur ed".
        let common = |p: &[u8]| match p.len() {
            0 => None,
            1 if p != b"a" => None,
            1..=3 => level(p).filter(|&l| l == 0),
            _ => level(p).filter(|&l| l <= 1),
        };
        if let (Some(la), Some(lb)) = (common(a), common(b)) {
            let s = alloc::format!("{} {}", core::str::from_utf8(a).unwrap_or(""), core::str::from_utf8(b).unwrap_or(""));
            found.push((25 + (la + lb) as i32 * 3, s));
        }
    }
    found.sort_by(|a, b| a.0.cmp(&b.0).then_with(|| a.1.cmp(&b.1)));
    let mut out: Vec<String> = Vec::new();
    for (_, s) in found {
        let s = match case {
            Case::Title if s.chars().next().is_some_and(|c| c.is_lowercase()) => title(&s),
            Case::Upper => s.to_uppercase(),
            _ => s,
        };
        if s != w && !out.contains(&s) {
            out.push(s);
        }
        if out.len() >= max {
            break;
        }
    }
    out
}

// ---- the user's words --------------------------------------------------------------

/// Add a word to the personal dictionary (kept in Dictionary.txt).
pub fn add(word: &str) {
    let w: String = word.chars().map(|c| if c == '\u{2019}' { '\'' } else { c }).collect();
    let mut u = USER.lock();
    if !u.contains(&w) {
        u.push(w);
        u.sort();
        DIRTY.store(true, Ordering::Relaxed);
    }
}

/// Stop marking this word until the computer restarts.
pub fn ignore(word: &str) {
    let w: String = word.chars().map(|c| if c == '\u{2019}' { '\'' } else { c }).collect();
    let mut i = IGNORED.lock();
    if !i.contains(&w) {
        i.push(w);
    }
}

/// Load Dictionary.txt (one word per line).
pub fn load_user(text: &str) {
    let mut u: Vec<String> = text.lines().map(|l| String::from(l.trim())).filter(|l| !l.is_empty()).collect();
    u.sort();
    u.dedup();
    *USER.lock() = u;
}

/// Dictionary.txt's contents.
pub fn user_text() -> String {
    let mut s = String::new();
    for w in USER.lock().iter() {
        s.push_str(w);
        s.push('\n');
    }
    s
}

/// True once after words were added (time to save Dictionary.txt).
pub fn take_dirty() -> bool {
    DIRTY.swap(false, Ordering::Relaxed)
}

// ---- drawing -----------------------------------------------------------------------

pub const SQUIGGLE: u32 = rgb(220, 40, 40);

/// The red wavy line under a misspelled word: from x for w pixels, just under baseline y.
pub fn squiggle(c: &mut Canvas, x: i32, y: i32, w: i32) {
    let mut i = 0;
    while i < w {
        let dy = if (i / 2) % 2 == 0 { 0 } else { 1 };
        c.fill_rect(x + i, y + dy, 2.min(w - i), 1, SQUIGGLE);
        i += 2;
    }
}

/// What the user picked in a `Popup`.
pub enum Choice {
    Replace(String),
    Add,
    Ignore,
}

/// What a click or key did to an open `Popup`.
pub enum Pick {
    /// Still open (moved the highlight, or the key wasn't for it).
    Open,
    /// Close it, nothing picked.
    Close,
    /// Close it and let the app handle this key as usual (typing on).
    Pass,
    Chosen(Choice),
}

/// The suggestions menu for one misspelled word, shown below it.
#[derive(Clone)]
pub struct Popup {
    items: Vec<String>,
    /// Where the word is on screen (the menu opens below its bottom-left corner).
    pub x: i32,
    pub y: i32,
    sel: usize,
}

const ITEM_H: i32 = 32;
const ADD: &str = "Add to dictionary";
const IGNORE: &str = "Ignore";

impl Popup {
    /// `x`, `y`: the word's bottom-left corner on screen.
    pub fn new(word: &str, x: i32, y: i32) -> Popup {
        let items = suggest(word, 6);
        let sel = if items.is_empty() { 1 } else { 0 };
        Popup { items, x, y, sel }
    }

    /// Rows: suggestions (or "No suggestions"), then Add and Ignore.
    fn rows(&self) -> Vec<(&str, bool)> {
        let mut v: Vec<(&str, bool)> = self.items.iter().map(|s| (s.as_str(), true)).collect();
        if v.is_empty() {
            v.push(("No suggestions", false));
        }
        v.push((ADD, true));
        v.push((IGNORE, true));
        v
    }

    fn choice(&self, row: usize) -> Option<Choice> {
        let n = self.items.len().max(1);
        if row < self.items.len() {
            Some(Choice::Replace(self.items[row].clone()))
        } else if row == n {
            Some(Choice::Add)
        } else if row == n + 1 {
            Some(Choice::Ignore)
        } else {
            None
        }
    }

    /// The menu's box, kept inside `bounds` (above the word if there's no room below).
    pub fn rect(&self, bounds: Rect) -> Rect {
        let rows = self.rows();
        let w = rows.iter().map(|(s, _)| text_width(s, Size::Small, true)).max().unwrap_or(0).max(160) + 32;
        let h = rows.len() as i32 * ITEM_H + 12 + 6;
        let x = self.x.clamp(bounds.x, (bounds.x + bounds.w - w).max(bounds.x));
        let y = if self.y + 4 + h <= bounds.y + bounds.h { self.y + 4 } else { (self.y - h - 30).max(bounds.y) };
        Rect::new(x, y, w, h)
    }

    fn row_rect(&self, b: Rect, i: usize) -> Rect {
        let sep = if i >= self.items.len().max(1) { 6 } else { 0 };
        Rect::new(b.x + 4, b.y + 6 + i as i32 * ITEM_H + sep, b.w - 8, ITEM_H)
    }

    pub fn draw(&self, c: &mut Canvas, bounds: Rect) {
        let b = self.rect(bounds);
        c.shadow(b.x, b.y, b.w, b.h, 8, 10, 60);
        c.round_rect(b.x, b.y, b.w, b.h, 8, rgb(255, 255, 255));
        c.outline_rect(b.x, b.y, b.w, b.h, rgb(214, 218, 224));
        let rows = self.rows();
        let n = self.items.len().max(1);
        for (i, (label, active)) in rows.iter().enumerate() {
            let r = self.row_rect(b, i);
            if i == n {
                c.fill_rect(b.x + 8, r.y - 4, b.w - 16, 1, rgb(228, 231, 236));
            }
            if *active && i == self.sel {
                c.round_rect(r.x, r.y, r.w, r.h, 6, rgb(222, 234, 252));
            }
            let suggestion = i < self.items.len();
            let col = if !*active { rgb(140, 146, 156) } else { rgb(32, 36, 44) };
            c.text(r.x + 12, r.y + (ITEM_H - 18) / 2, label, Size::Small, suggestion, col);
        }
    }

    /// A click: on a row picks it; anywhere else closes the menu.
    pub fn press(&self, x: i32, y: i32, bounds: Rect) -> Pick {
        let b = self.rect(bounds);
        for i in 0..self.rows().len() {
            if self.row_rect(b, i).contains(x, y) {
                return match self.choice(i) {
                    Some(c) => Pick::Chosen(c),
                    None => Pick::Open,
                };
            }
        }
        Pick::Close
    }

    /// Up/Down move, Enter picks, Esc closes; any other key closes it and goes to the app.
    pub fn key(&mut self, k: &crate::keys::Key) -> Pick {
        use crate::keys::Key;
        let n = self.rows().len();
        match k {
            Key::Up | Key::Down => {
                let step = if matches!(k, Key::Up) { n - 1 } else { 1 };
                let mut i = self.sel;
                for _ in 0..n {
                    i = (i + step) % n;
                    if self.choice(i).is_some() {
                        break;
                    }
                }
                self.sel = i;
                Pick::Open
            }
            Key::Enter => match self.choice(self.sel) {
                Some(c) => Pick::Chosen(c),
                None => Pick::Close,
            },
            Key::Esc => Pick::Close,
            _ => Pick::Pass,
        }
    }
}

/// F7, Shift+F10 or the Menu key: open spelling suggestions for the word at the caret.
pub fn is_suggest_key(k: &crate::keys::Key, shift: bool) -> bool {
    use crate::keys::Key;
    matches!(k, Key::F(7) | Key::Menu) || (shift && matches!(k, Key::F(10)))
}

/// Apply a choice to a word in plain text (Replace changes `text`; Add/Ignore only teach the
/// checker). Returns the new end of the word.
pub fn apply(text: &mut Vec<char>, range: (usize, usize), choice: &Choice) -> usize {
    match choice {
        Choice::Replace(s) => {
            let new: Vec<char> = s.chars().collect();
            let n = new.len();
            text.splice(range.0..range.1, new);
            range.0 + n
        }
        Choice::Add => {
            add(&text[range.0..range.1].iter().collect::<String>());
            range.1
        }
        Choice::Ignore => {
            ignore(&text[range.0..range.1].iter().collect::<String>());
            range.1
        }
    }
}

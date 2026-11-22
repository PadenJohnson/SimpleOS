//! A one-line text box for names (new folder, rename). Keyboard scancodes (set 1) in,
//! characters out. Characters that can't be in file names are ignored.

use alloc::string::String;

use crate::gfx::{rgb, text_width, Canvas, Rect, Size};

pub enum TextEvent {
    None,
    Changed,
    Submit,
    Cancel,
}

pub struct TextField {
    pub text: String,
    shift: bool,
    max: usize,
    /// Accept every character (spreadsheet cells), not only ones allowed in file names.
    any: bool,
    /// The suggested text is "selected": typing replaces it, Backspace clears it.
    fresh: bool,
    /// The last scancode was the E0 prefix (arrows, the Menu key).
    e0: bool,
    /// Spelling suggestions open for the word at these characters (F7, Shift+F10, Menu key).
    spell: Option<(crate::spell::Popup, (usize, usize))>,
}

// Set 1 scancode rows: unshifted and shifted characters.
const ROWS: [(u8, &str, &str); 4] = [
    (0x02, "1234567890-=", "!@#$%^&*()_+"),
    (0x10, "qwertyuiop[]", "QWERTYUIOP{}"),
    (0x1E, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
    (0x2B, "\\zxcvbnm,./", "|ZXCVBNM<>?"),
];

fn char_for(sc: u8, shift: bool) -> Option<char> {
    if sc == 0x39 {
        return Some(' ');
    }
    for (first, plain, shifted) in ROWS {
        if sc >= first && ((sc - first) as usize) < plain.len() {
            let set = if shift { shifted } else { plain };
            return set.chars().nth((sc - first) as usize);
        }
    }
    None
}

impl TextField {
    pub fn new(initial: &str, max: usize) -> TextField {
        TextField { text: String::from(initial), shift: false, max, any: false, fresh: !initial.is_empty(), e0: false, spell: None }
    }

    /// A field for any text (formulas, cell contents).
    pub fn any(initial: &str, max: usize, shift: bool) -> TextField {
        TextField { text: String::from(initial), shift, max, any: true, fresh: false, e0: false, spell: None }
    }

    /// Start with the text selected, so typing replaces it.
    pub fn selected(mut self) -> TextField {
        self.fresh = !self.text.is_empty();
        self
    }

    /// Spelling suggestions are open: Enter, Esc and the arrows belong to them.
    pub fn menu_open(&self) -> bool {
        self.spell.is_some()
    }

    /// Spelling applies to free text (titles, cells), not file names or formulas.
    fn checks_spelling(&self) -> bool {
        self.any && !self.text.starts_with('=') && crate::spell::enabled()
    }

    pub fn on_key(&mut self, sc: u8) -> TextEvent {
        use crate::keys::Key;
        use crate::spell::Pick;
        let e0 = core::mem::replace(&mut self.e0, sc == 0xE0);
        if sc == 0xE0 {
            return TextEvent::None;
        }
        if let Some((mut pop, (s, e))) = self.spell.take() {
            if sc & 0x80 != 0 || matches!(sc, 0x2A | 0x36) {
                self.spell = Some((pop, (s, e)));
            } else {
                let key = match sc {
                    0x48 => Key::Up,
                    0x50 => Key::Down,
                    0x1C => Key::Enter,
                    0x01 => Key::Esc,
                    _ => Key::Char(' '),
                };
                match pop.key(&key) {
                    Pick::Open => {
                        self.spell = Some((pop, (s, e)));
                        return TextEvent::Changed;
                    }
                    Pick::Close => return TextEvent::Changed,
                    Pick::Chosen(ch) => {
                        let mut t: alloc::vec::Vec<char> = self.text.chars().collect();
                        if e <= t.len() {
                            crate::spell::apply(&mut t, (s, e), &ch);
                            self.text = t.into_iter().collect();
                        }
                        return TextEvent::Changed;
                    }
                    Pick::Pass => {} // typing on: handled below
                }
            }
        }
        // F7, Shift+F10 or the Menu key: suggestions for the last misspelled word.
        if (sc == 0x41 || (sc == 0x44 && self.shift) || (e0 && sc == 0x5D)) && self.checks_spelling() {
            let t: alloc::vec::Vec<char> = self.text.chars().collect();
            if let Some(&(s, e)) = crate::spell::misspelled(&t, None).last() {
                let word: String = t[s..e].iter().collect();
                self.spell = Some((crate::spell::Popup::new(&word, 0, 0), (s, e)));
                return TextEvent::Changed;
            }
            return TextEvent::None;
        }
        match sc {
            0x2A | 0x36 => {
                self.shift = true;
                return TextEvent::None;
            }
            0xAA | 0xB6 => {
                self.shift = false;
                return TextEvent::None;
            }
            _ if sc & 0x80 != 0 => return TextEvent::None,
            0x1C => return TextEvent::Submit,
            0x01 => return TextEvent::Cancel,
            0x0E => {
                if self.fresh {
                    self.text.clear();
                    self.fresh = false;
                    return TextEvent::Changed;
                }
                self.text.pop();
                return TextEvent::Changed;
            }
            _ => {}
        }
        match char_for(sc, self.shift) {
            Some(c) if (self.any || !"/\\:*?\"<>|".contains(c)) && (self.fresh || self.text.chars().count() < self.max) => {
                if self.fresh {
                    self.text.clear();
                    self.fresh = false;
                }
                self.text.push(c);
                TextEvent::Changed
            }
            _ => TextEvent::None,
        }
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect) {
        c.round_rect(r.x, r.y, r.w, r.h, 6, rgb(255, 255, 255));
        c.outline_rect(r.x, r.y, r.w, r.h, rgb(46, 134, 222));
        let shown = crate::photos::ellipsize_left(&self.text, r.w - 24, Size::Small, false);
        if self.fresh {
            // Selected suggestion: typing replaces it.
            let w = text_width(&shown, Size::Small, false);
            c.fill_rect(r.x + 8, r.y + 7, w + 4, r.h - 14, rgb(198, 222, 250));
        }
        let tw = c.text(r.x + 10, r.y + (r.h - 16) / 2, &shown, Size::Small, false, rgb(40, 44, 52));
        c.fill_rect(r.x + 11 + tw, r.y + 8, 2, r.h - 16, rgb(46, 134, 222)); // caret
        if self.checks_spelling() {
            // Misspelled words that are shown, except the one being typed at the end.
            let (chars, skip, at) = self.shown_layout(r);
            let mut words = crate::spell::misspelled(&chars, Some(chars.len()));
            words.retain(|&(s, _)| s >= skip);
            for (s, e) in words {
                crate::spell::squiggle(c, at(s), r.y + (r.h - 16) / 2 + 17, at(e) - at(s));
            }
        }
    }

    /// The text as shown (its start cut off with "..." when it's long), how many characters
    /// of that are the "...", and where character i of it is drawn.
    fn shown_layout(&self, r: Rect) -> (alloc::vec::Vec<char>, usize, impl Fn(usize) -> i32) {
        let shown = crate::photos::ellipsize_left(&self.text, r.w - 24, Size::Small, false);
        let chars: alloc::vec::Vec<char> = shown.chars().collect();
        let skip = if shown != self.text { 1 } else { 0 };
        let c2 = chars.clone();
        (chars, skip, move |i: usize| r.x + 10 + text_width(&c2[..i.min(c2.len())].iter().collect::<String>(), Size::Small, false))
    }

    /// The spelling suggestions, if open. Apps draw this after everything else so nothing
    /// covers it; `r` is where the field is.
    pub fn draw_menu(&self, c: &mut Canvas, r: Rect) {
        let Some((pop, (s, _))) = &self.spell else { return };
        let (chars, skip, at) = self.shown_layout(r);
        let total = self.text.chars().count();
        let mut p = pop.clone();
        // The word's place in the shown text (it may be cut off on the left).
        p.x = at((*s + chars.len()).saturating_sub(total).max(skip));
        p.y = r.y + r.h;
        p.draw(c, Rect::new(r.x - 200, r.y - 420, r.w + 400, r.h + 840));
    }
}

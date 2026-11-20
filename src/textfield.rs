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
        TextField { text: String::from(initial), shift: false, max }
    }

    pub fn on_key(&mut self, sc: u8) -> TextEvent {
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
                self.text.pop();
                return TextEvent::Changed;
            }
            _ => {}
        }
        match char_for(sc, self.shift) {
            Some(c) if !"/\\:*?\"<>|".contains(c) && self.text.chars().count() < self.max => {
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
        let tw = c.text(r.x + 10, r.y + (r.h - 16) / 2, &shown, Size::Small, false, rgb(40, 44, 52));
        let _ = text_width;
        c.fill_rect(r.x + 11 + tw, r.y + 8, 2, r.h - 16, rgb(46, 134, 222)); // caret
    }
}

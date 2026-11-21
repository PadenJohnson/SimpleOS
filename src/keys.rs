//! Turning keyboard scancodes (set 1) into keys for the text editors: characters with Shift
//! and Caps Lock, editing keys, and Ctrl shortcuts. Plus the clipboard the editors share.

use alloc::string::String;
use spin::Mutex;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Key {
    Char(char),
    Enter,
    Backspace,
    Delete,
    Tab,
    Esc,
    Left,
    Right,
    Up,
    Down,
    Home,
    End,
    PageUp,
    PageDown,
    /// Ctrl + a letter or digit (lower case), e.g. Ctrl('s').
    Ctrl(char),
    F(u8),
}

#[derive(Default, Clone, Copy)]
pub struct Keys {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub caps: bool,
}

// Set 1 scancode rows: unshifted and shifted characters.
const ROWS: [(u8, &str, &str); 4] = [
    (0x02, "1234567890-=", "!@#$%^&*()_+"),
    (0x10, "qwertyuiop[]", "QWERTYUIOP{}"),
    (0x1E, "asdfghjkl;'`", "ASDFGHJKL:\"~"),
    (0x2B, "\\zxcvbnm,./", "|ZXCVBNM<>?"),
];

impl Keys {
    /// Feed one scancode; returns the key it makes (key releases and modifiers make none).
    pub fn feed(&mut self, sc: u8) -> Option<Key> {
        match sc {
            0x2A | 0x36 => self.shift = true,
            0xAA | 0xB6 => self.shift = false,
            0x1D => self.ctrl = true,
            0x9D => self.ctrl = false,
            0x38 => self.alt = true,
            0xB8 => self.alt = false,
            0x3A => self.caps = !self.caps,
            _ => {}
        }
        if sc & 0x80 != 0 {
            return None;
        }
        Some(match sc {
            0x1C => Key::Enter,
            0x0E => Key::Backspace,
            0x53 => Key::Delete,
            0x0F => Key::Tab,
            0x01 => Key::Esc,
            0x4B => Key::Left,
            0x4D => Key::Right,
            0x48 => Key::Up,
            0x50 => Key::Down,
            0x47 => Key::Home,
            0x4F => Key::End,
            0x49 => Key::PageUp,
            0x51 => Key::PageDown,
            0x3B..=0x44 => Key::F(sc - 0x3A),
            0x57 => Key::F(11),
            0x58 => Key::F(12),
            0x39 if !self.ctrl => Key::Char(' '),
            0x37 => Key::Char('*'),
            0x4A => Key::Char('-'),
            0x4E => Key::Char('+'),
            0x56 => Key::Char(if self.shift { '|' } else { '\\' }),
            _ => {
                let (plain, shifted) = Self::chars(sc)?;
                if self.ctrl {
                    return Some(Key::Ctrl(plain));
                }
                let upper = if plain.is_ascii_alphabetic() { self.shift != self.caps } else { self.shift };
                Key::Char(if upper { shifted } else { plain })
            }
        })
    }

    fn chars(sc: u8) -> Option<(char, char)> {
        for (first, plain, shifted) in ROWS {
            if sc >= first && ((sc - first) as usize) < plain.len() {
                let i = (sc - first) as usize;
                return Some((plain.chars().nth(i)?, shifted.chars().nth(i)?));
            }
        }
        None
    }
}

/// Text copied in any editor.
pub static CLIPBOARD: Mutex<String> = Mutex::new(String::new());

pub fn copy(s: &str) {
    *CLIPBOARD.lock() = String::from(s);
}

pub fn paste() -> String {
    CLIPBOARD.lock().clone()
}

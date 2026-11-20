//! Software mouse cursor with save-under, so we never have to redraw the whole screen.

use crate::screen::{Rgb, Screen};

const W: usize = 12;
const H: usize = 16;

// X = black outline, . = white fill, space = transparent
const SPRITE: [&str; H] = [
    "X           ",
    "XX          ",
    "X.X         ",
    "X..X        ",
    "X...X       ",
    "X....X      ",
    "X.....X     ",
    "X......X    ",
    "X.......X   ",
    "X........X  ",
    "X.....XXXXX ",
    "X..X..X     ",
    "X.X X..X    ",
    "XX  X..X    ",
    "X    X..X   ",
    "     XXXX   ",
];

pub struct Cursor {
    x: usize,
    y: usize,
    under: [u32; W * H],
    drawn: bool,
}

impl Cursor {
    pub const fn new() -> Self {
        Self { x: 0, y: 0, under: [0; W * H], drawn: false }
    }

    /// Restore the pixels that were under the cursor.
    pub fn hide(&mut self, s: &Screen) {
        if !self.drawn {
            return;
        }
        for row in 0..H {
            for col in 0..W {
                s.put(self.x + col, self.y + row, self.under[row * W + col]);
            }
        }
        self.drawn = false;
    }

    /// Move the cursor: restore the old area, save the new one, draw the sprite.
    pub fn show_at(&mut self, s: &Screen, x: usize, y: usize) {
        self.hide(s);
        self.x = x;
        self.y = y;
        let black = s.pack(Rgb(0, 0, 0));
        let white = s.pack(Rgb(255, 255, 255));
        for row in 0..H {
            for col in 0..W {
                self.under[row * W + col] = s.get(x + col, y + row);
            }
        }
        for row in 0..H {
            for col in 0..W {
                match SPRITE[row].as_bytes().get(col) {
                    Some(b'X') => s.put(x + col, y + row, black),
                    Some(b'.') => s.put(x + col, y + row, white),
                    _ => {}
                }
            }
        }
        self.drawn = true;
    }
}

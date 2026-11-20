//! Software mouse cursor with save-under, so we never have to redraw the whole screen.
//! Drawn at 1x or 2x so it stays easy to see with large text or on 4K screens.

use crate::screen::{Rgb, Screen};

const W: usize = 12;
const H: usize = 16;
const MAX_ZOOM: usize = 2;

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
    zoom: usize,
    under: [u32; W * H * MAX_ZOOM * MAX_ZOOM],
    drawn: bool,
}

impl Cursor {
    pub const fn new() -> Self {
        Self { x: 0, y: 0, zoom: 1, under: [0; W * H * MAX_ZOOM * MAX_ZOOM], drawn: false }
    }

    /// 1 or 2 (call while hidden, e.g. right before a full redraw).
    pub fn set_zoom(&mut self, z: usize) {
        self.zoom = z.clamp(1, MAX_ZOOM);
    }

    /// Restore the pixels that were under the cursor.
    pub fn hide(&mut self, s: &Screen) {
        if !self.drawn {
            return;
        }
        let (w, h) = (W * self.zoom, H * self.zoom);
        for row in 0..h {
            for col in 0..w {
                s.put(self.x + col, self.y + row, self.under[row * w + col]);
            }
        }
        self.drawn = false;
    }

    /// Move the cursor: restore the old area, save the new one, draw the sprite.
    pub fn show_at(&mut self, s: &Screen, x: usize, y: usize) {
        self.hide(s);
        self.x = x;
        self.y = y;
        let z = self.zoom;
        let (w, h) = (W * z, H * z);
        let black = s.pack(Rgb(0, 0, 0));
        let white = s.pack(Rgb(255, 255, 255));
        for row in 0..h {
            for col in 0..w {
                self.under[row * w + col] = s.get(x + col, y + row);
            }
        }
        for row in 0..h {
            for col in 0..w {
                match SPRITE[row / z].as_bytes().get(col / z) {
                    Some(b'X') => s.put(x + col, y + row, black),
                    Some(b'.') => s.put(x + col, y + row, white),
                    _ => {}
                }
            }
        }
        self.drawn = true;
    }
}

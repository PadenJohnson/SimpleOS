//! Software mouse cursor with save-under, so we never have to redraw the whole screen.
//! Drawn at 1x or 2x so it stays easy to see with large text or on 4K screens. Besides the
//! arrow there are the double arrows shown over a window's edges and corners (drag to change
//! its size).

use crate::screen::{Rgb, Screen};

/// The biggest sprite, and the biggest zoom.
const MAX: usize = 16;
const MAX_ZOOM: usize = 2;

/// Which picture the pointer shows.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Shape {
    Arrow,
    /// Left and right (a window's side edge).
    ResizeH,
    /// Up and down (its top or bottom edge).
    ResizeV,
    /// Top-left to bottom-right (those corners).
    ResizeNwse,
    /// Top-right to bottom-left.
    ResizeNesw,
}

// X = black outline, . = white fill, space = transparent
const ARROW: [&str; 16] = [
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

const RESIZE_H: [&str; 16] = [
    "                ",
    "                ",
    "     X    X     ",
    "    XX    XX    ",
    "   X.X    X.X   ",
    "  X..X    X..X  ",
    " X...XXXXXX...X ",
    "X..............X",
    "X..............X",
    " X...XXXXXX...X ",
    "  X..X    X..X  ",
    "   X.X    X.X   ",
    "    XX    XX    ",
    "     X    X     ",
    "                ",
    "                ",
];

const RESIZE_V: [&str; 16] = [
    "       XX       ",
    "      X..X      ",
    "     X....X     ",
    "    X......X    ",
    "   X........X   ",
    "  XXXXX..XXXXX  ",
    "      X..X      ",
    "      X..X      ",
    "      X..X      ",
    "      X..X      ",
    "  XXXXX..XXXXX  ",
    "   X........X   ",
    "    X......X    ",
    "     X....X     ",
    "      X..X      ",
    "       XX       ",
];

const RESIZE_NWSE: [&str; 16] = [
    "                ",
    "                ",
    "  XXXXXXXXX     ",
    "  X......X      ",
    "  X.....X       ",
    "  X....X     X  ",
    "  X.....X   XX  ",
    "  X..X...X X.X  ",
    "  X.X X...X..X  ",
    "  XX   X.....X  ",
    "  X     X....X  ",
    "       X.....X  ",
    "      X......X  ",
    "     XXXXXXXXX  ",
    "                ",
    "                ",
];

const RESIZE_NESW: [&str; 16] = [
    "                ",
    "                ",
    "     XXXXXXXXX  ",
    "      X......X  ",
    "       X.....X  ",
    "  X     X....X  ",
    "  XX   X.....X  ",
    "  X.X X...X..X  ",
    "  X..X...X X.X  ",
    "  X.....X   XX  ",
    "  X....X     X  ",
    "  X.....X       ",
    "  X......X      ",
    "  XXXXXXXXX     ",
    "                ",
    "                ",
];

impl Shape {
    /// The picture, and where in it the pointer's position is (the hot spot).
    fn sprite(self) -> (&'static [&'static str; 16], usize, usize) {
        match self {
            Shape::Arrow => (&ARROW, 0, 0),
            Shape::ResizeH => (&RESIZE_H, 7, 7),
            Shape::ResizeV => (&RESIZE_V, 7, 7),
            Shape::ResizeNwse => (&RESIZE_NWSE, 7, 7),
            Shape::ResizeNesw => (&RESIZE_NESW, 7, 7),
        }
    }
}

pub struct Cursor {
    /// Where the sprite was drawn (its top-left corner) and how big.
    x: usize,
    y: usize,
    w: usize,
    h: usize,
    zoom: usize,
    shape: Shape,
    under: [u32; MAX * MAX * MAX_ZOOM * MAX_ZOOM],
    drawn: bool,
}

impl Cursor {
    pub const fn new() -> Self {
        Self { x: 0, y: 0, w: 0, h: 0, zoom: 1, shape: Shape::Arrow, under: [0; MAX * MAX * MAX_ZOOM * MAX_ZOOM], drawn: false }
    }

    /// 1 or 2 (call while hidden, e.g. right before a full redraw).
    pub fn set_zoom(&mut self, z: usize) {
        self.zoom = z.clamp(1, MAX_ZOOM);
    }

    /// Show another picture from the next `show_at` on. True if it changed.
    pub fn set_shape(&mut self, shape: Shape) -> bool {
        let changed = self.shape != shape;
        self.shape = shape;
        changed
    }

    /// Restore the pixels that were under the cursor.
    pub fn hide(&mut self, s: &Screen) {
        if !self.drawn {
            return;
        }
        for row in 0..self.h {
            for col in 0..self.w {
                s.put(self.x + col, self.y + row, self.under[row * self.w + col]);
            }
        }
        self.drawn = false;
    }

    /// Move the pointer to (px, py): restore the old area, save the new one, draw the sprite
    /// with its hot spot there.
    pub fn show_at(&mut self, s: &Screen, px: usize, py: usize) {
        self.hide(s);
        let z = self.zoom;
        let (sprite, hx, hy) = self.shape.sprite();
        let sw = sprite.iter().map(|r| r.len()).max().unwrap_or(0).min(MAX);
        let (x, y) = (px.saturating_sub(hx * z), py.saturating_sub(hy * z));
        let (w, h) = (sw * z, sprite.len().min(MAX) * z);
        self.x = x;
        self.y = y;
        self.w = w;
        self.h = h;
        let black = s.pack(Rgb(0, 0, 0));
        let white = s.pack(Rgb(255, 255, 255));
        for row in 0..h {
            for col in 0..w {
                self.under[row * w + col] = s.get(x + col, y + row);
            }
        }
        for row in 0..h {
            for col in 0..w {
                match sprite[row / z].as_bytes().get(col / z) {
                    Some(b'X') => s.put(x + col, y + row, black),
                    Some(b'.') => s.put(x + col, y + row, white),
                    _ => {}
                }
            }
        }
        self.drawn = true;
    }
}

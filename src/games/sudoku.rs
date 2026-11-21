//! Sudoku: fill the grid so every row, column and 3x3 box has 1 to 9. Puzzles are made on
//! the spot and always have exactly one answer.

use alloc::format;
use alloc::string::String;

use super::{banner, button, rand_below, shuffle, time_text, GameAction, Scores, Sfx, Sound};
use crate::gfx::{rgb, Canvas, Rect, Size};
use crate::interrupts::HZ;

const LEVELS: [(usize, &str); 3] = [(40, "Easy"), (32, "Medium"), (26, "Hard")];

pub struct Sudoku {
    level: usize,
    solution: [u8; 81],
    given: [bool; 81],
    cells: [u8; 81],
    notes: [u16; 81],
    sel: usize,
    notes_mode: bool,
    won: bool,
    start: u64,
    secs: u32,
    started: bool,
    new_best: bool,
}

fn ok(g: &[u8; 81], i: usize, v: u8) -> bool {
    let (r, c) = (i / 9, i % 9);
    for k in 0..9 {
        if g[r * 9 + k] == v || g[k * 9 + c] == v {
            return false;
        }
    }
    let (br, bc) = (r / 3 * 3, c / 3 * 3);
    for y in br..br + 3 {
        for x in bc..bc + 3 {
            if g[y * 9 + x] == v {
                return false;
            }
        }
    }
    true
}

/// Fill the grid (random order). True when complete.
fn fill(g: &mut [u8; 81]) -> bool {
    let Some(i) = g.iter().position(|&v| v == 0) else { return true };
    let mut order = [1u8, 2, 3, 4, 5, 6, 7, 8, 9];
    shuffle(&mut order);
    for v in order {
        if ok(g, i, v) {
            g[i] = v;
            if fill(g) {
                return true;
            }
            g[i] = 0;
        }
    }
    false
}

/// Count solutions, stopping at `limit`.
fn count(g: &mut [u8; 81], limit: u32) -> u32 {
    // The empty cell with the fewest choices.
    let mut best: Option<(usize, u32)> = None;
    for i in 0..81 {
        if g[i] == 0 {
            let n = (1..=9).filter(|&v| ok(g, i, v)).count() as u32;
            if n == 0 {
                return 0;
            }
            if best.is_none_or(|b| n < b.1) {
                best = Some((i, n));
            }
        }
    }
    let Some((i, _)) = best else { return 1 };
    let mut total = 0;
    for v in 1..=9 {
        if ok(g, i, v) {
            g[i] = v;
            total += count(g, limit - total);
            g[i] = 0;
            if total >= limit {
                break;
            }
        }
    }
    total
}

impl Sudoku {
    pub fn new() -> Sudoku {
        let mut s = Sudoku { level: 0, solution: [0; 81], given: [false; 81], cells: [0; 81], notes: [0; 81], sel: 40, notes_mode: false, won: false, start: 0, secs: 0, started: false, new_best: false };
        s.generate(0);
        s
    }

    fn generate(&mut self, level: usize) {
        self.level = level;
        let mut g = [0u8; 81];
        fill(&mut g);
        self.solution = g;
        // Take numbers away while the answer stays unique.
        let mut order: [usize; 81] = core::array::from_fn(|i| i);
        shuffle(&mut order);
        let mut left = 81;
        for &i in order.iter() {
            if left <= LEVELS[level].0 {
                break;
            }
            let v = g[i];
            g[i] = 0;
            let mut t = g;
            if count(&mut t, 2) != 1 {
                g[i] = v;
            } else {
                left -= 1;
            }
        }
        self.cells = g;
        self.given = core::array::from_fn(|i| g[i] != 0);
        self.notes = [0; 81];
        self.won = false;
        self.started = false;
        self.secs = 0;
        self.new_best = false;
        self.sel = (0..81).find(|&i| g[i] == 0).unwrap_or(0);
        let _ = rand_below(1);
    }

    fn clash(&self, i: usize) -> bool {
        let v = self.cells[i];
        if v == 0 {
            return false;
        }
        let mut g = self.cells;
        g[i] = 0;
        !ok(&g, i, v)
    }

    // ---- layout ---------------------------------------------------------------------------

    fn grid(r: Rect) -> (i32, i32, i32) {
        let size = ((r.h - 80) / 9).min((r.w - 260) / 9).clamp(24, 64);
        (r.x + 16, r.y + 64, size)
    }
    fn level_button(r: Rect, k: usize) -> Rect {
        Rect::new(r.x + 12 + k as i32 * 96, r.y + 12, 88, 34)
    }
    fn pad(r: Rect, k: usize) -> Rect {
        let (gx, gy, s) = Self::grid(r);
        let px = gx + 9 * s + 24;
        Rect::new(px + (k % 3) as i32 * 72, gy + (k / 3) as i32 * 64, 64, 56)
    }
    fn erase_button(r: Rect) -> Rect {
        let p = Self::pad(r, 9);
        Rect::new(p.x, p.y + 8, 100, 40)
    }
    fn notes_button(r: Rect) -> Rect {
        let p = Self::pad(r, 9);
        Rect::new(p.x + 108, p.y + 8, 100, 40)
    }
    fn new_button(r: Rect) -> Rect {
        let p = Self::pad(r, 9);
        Rect::new(p.x, p.y + 56, 208, 40)
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect, scores: &Scores) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(244, 241, 234));
        for k in 0..3 {
            button(c, Self::level_button(r, k), LEVELS[k].1, self.level == k);
        }
        let best = scores.sudoku[self.level];
        let info = format!("Time: {}     Best: {}", time_text(self.secs), if best > 0 { time_text(best) } else { String::from("-") });
        c.text(r.x + 320, r.y + 20, &info, Size::Small, true, rgb(70, 66, 60));
        let (gx, gy, s) = Self::grid(r);
        let sv = self.cells[self.sel];
        for i in 0..81 {
            let (row, col) = (i / 9, i % 9);
            let (x, y) = (gx + col as i32 * s, gy + row as i32 * s);
            let (sr, sc) = (self.sel / 9, self.sel % 9);
            let related = row == sr || col == sc || (row / 3 == sr / 3 && col / 3 == sc / 3);
            let bg = if i == self.sel {
                rgb(255, 214, 120)
            } else if sv != 0 && self.cells[i] == sv {
                rgb(255, 234, 180)
            } else if related {
                rgb(236, 230, 214)
            } else {
                rgb(255, 253, 248)
            };
            c.fill_rect(x, y, s, s, bg);
            let v = self.cells[i];
            if v != 0 {
                let col = if self.clash(i) {
                    rgb(210, 40, 40)
                } else if self.given[i] {
                    rgb(30, 30, 34)
                } else {
                    rgb(40, 90, 200)
                };
                let txt = format!("{}", v);
                let (size, th) = if s >= 40 { (Size::Large, 32) } else { (Size::Medium, 20) };
                c.text_centered(x + s / 2, y + (s - th) / 2, &txt, size, self.given[i], col);
            } else if self.notes[i] != 0 {
                for d in 1..=9u16 {
                    if self.notes[i] & (1 << d) != 0 {
                        let (nx, ny) = (x + 3 + ((d - 1) % 3) as i32 * (s - 6) / 3, y + 1 + ((d - 1) / 3) as i32 * (s - 4) / 3);
                        c.text(nx + (s / 3 - 10) / 2, ny, &format!("{}", d), Size::Small, false, rgb(120, 116, 108));
                    }
                }
            }
        }
        // Grid lines.
        for k in 0..=9 {
            let t = if k % 3 == 0 { 3 } else { 1 };
            let col = if k % 3 == 0 { rgb(60, 56, 50) } else { rgb(190, 184, 172) };
            c.fill_rect(gx + k * s - t / 2, gy, t, 9 * s, col);
            c.fill_rect(gx, gy + k * s - t / 2, 9 * s, t, col);
        }
        // Number pad.
        for k in 0..9 {
            let b = Self::pad(r, k);
            let done = self.cells.iter().filter(|&&v| v == k as u8 + 1).count() >= 9;
            c.round_rect(b.x, b.y, b.w, b.h, 10, if done { rgb(220, 216, 206) } else { rgb(255, 255, 255) });
            c.text_centered(b.x + b.w / 2, b.y + 12, &format!("{}", k + 1), Size::Large, true, if done { rgb(170, 164, 150) } else { rgb(40, 90, 200) });
        }
        button(c, Self::erase_button(r), "Erase", false);
        button(c, Self::notes_button(r), if self.notes_mode { "Notes: on" } else { "Notes: off" }, self.notes_mode);
        button(c, Self::new_button(r), "New puzzle", false);
        let p = Self::new_button(r);
        c.text(p.x, p.y + 56, "Type 1-9 or click the numbers.", Size::Small, false, rgb(110, 104, 94));
        c.text(p.x, p.y + 78, "Red means it clashes.", Size::Small, false, rgb(110, 104, 94));
        if self.won {
            banner(c, r, "Solved!", &if self.new_best { format!("New best time: {}", time_text(self.secs)) } else { format!("Time: {}", time_text(self.secs)) });
        }
    }

    fn put(&mut self, v: u8, now: u64, scores: &mut Scores, sfx: &mut Sfx) {
        let i = self.sel;
        if self.given[i] || self.won {
            return;
        }
        if !self.started {
            self.started = true;
            self.start = now;
        }
        if self.notes_mode && v != 0 {
            self.notes[i] ^= 1 << v;
            return;
        }
        self.cells[i] = if self.cells[i] == v { 0 } else { v };
        self.notes[i] = 0;
        sfx.play(Sound::Click);
        if v != 0 && self.cells[i] == v {
            // Clear this number from notes it now rules out.
            let (r, c) = (i / 9, i % 9);
            for j in 0..81 {
                if j / 9 == r || j % 9 == c || (j / 27 == i / 27 && (j % 9) / 3 == c / 3) {
                    self.notes[j] &= !(1 << v);
                }
            }
        }
        if self.cells == self.solution {
            self.won = true;
            self.new_best = Scores::best_time(&mut scores.sudoku[self.level], self.secs);
            sfx.play(Sound::Win);
        }
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, scores: &mut Scores, sfx: &mut Sfx) -> GameAction {
        for k in 0..3 {
            if Self::level_button(r, k).contains(x, y) {
                self.generate(k);
                return GameAction::Redraw;
            }
        }
        if Self::new_button(r).contains(x, y) || self.won {
            self.generate(self.level);
            return GameAction::Redraw;
        }
        let (gx, gy, s) = Self::grid(r);
        if x >= gx && y >= gy && x < gx + 9 * s && y < gy + 9 * s {
            self.sel = (((y - gy) / s) * 9 + (x - gx) / s) as usize;
            return GameAction::Redraw;
        }
        for k in 0..9 {
            if Self::pad(r, k).contains(x, y) {
                self.put(k as u8 + 1, now, scores, sfx);
                return GameAction::Redraw;
            }
        }
        if Self::erase_button(r).contains(x, y) {
            if !self.given[self.sel] {
                self.cells[self.sel] = 0;
                self.notes[self.sel] = 0;
            }
            return GameAction::Redraw;
        }
        if Self::notes_button(r).contains(x, y) {
            self.notes_mode = !self.notes_mode;
            return GameAction::Redraw;
        }
        GameAction::None
    }

    pub fn on_key(&mut self, sc: u8, now: u64, scores: &mut Scores, sfx: &mut Sfx) -> Option<GameAction> {
        let (r, c) = (self.sel / 9, self.sel % 9);
        match sc {
            0x02..=0x0A => self.put(sc - 1, now, scores, sfx),
            0x0B | 0x0E | 0x53 => {
                if !self.given[self.sel] {
                    self.cells[self.sel] = 0;
                    self.notes[self.sel] = 0;
                }
            }
            0x31 => self.notes_mode = !self.notes_mode, // N
            0x48 => self.sel = ((r + 8) % 9) * 9 + c,
            0x50 => self.sel = ((r + 1) % 9) * 9 + c,
            0x4B => self.sel = r * 9 + (c + 8) % 9,
            0x4D => self.sel = r * 9 + (c + 1) % 9,
            _ => return None,
        }
        Some(GameAction::Redraw)
    }

    pub fn tick(&mut self, now: u64) -> bool {
        if self.started && !self.won {
            let s = ((now - self.start) / HZ) as u32;
            if s != self.secs {
                self.secs = s;
                return true;
            }
        }
        false
    }
}

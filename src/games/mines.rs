//! Mines: uncover every square that isn't a mine. Numbers say how many mines touch a square.

use alloc::format;
use alloc::vec;
use alloc::vec::Vec;

use super::{banner, button, rand_below, time_text, GameAction, Scores, Sfx, Sound};
use crate::gfx::{rgb, Canvas, Rect, Size};
use crate::interrupts::HZ;

const LEVELS: [(usize, usize, usize, &str); 3] = [(9, 9, 10, "Easy"), (16, 16, 40, "Medium"), (30, 16, 99, "Hard")];
const MINE: u8 = 0x10;
const OPEN: u8 = 0x20;
const FLAG: u8 = 0x40;

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Playing,
    Won,
    Lost,
}

pub struct Mines {
    level: usize,
    w: usize,
    h: usize,
    mines: usize,
    cells: Vec<u8>,
    state: State,
    start: u64,
    secs: u32,
    flag_mode: bool,
    boom: Option<usize>,
    new_best: bool,
}

impl Mines {
    pub fn new() -> Mines {
        let mut m = Mines { level: 0, w: 9, h: 9, mines: 10, cells: Vec::new(), state: State::Ready, start: 0, secs: 0, flag_mode: false, boom: None, new_best: false };
        m.reset(0);
        m
    }

    fn reset(&mut self, level: usize) {
        let (w, h, n, _) = LEVELS[level];
        self.level = level;
        self.w = w;
        self.h = h;
        self.mines = n;
        self.cells = vec![0; w * h];
        self.state = State::Ready;
        self.secs = 0;
        self.boom = None;
        self.new_best = false;
    }

    fn neighbours(&self, i: usize) -> Vec<usize> {
        let (x, y) = ((i % self.w) as isize, (i / self.w) as isize);
        let mut v = Vec::with_capacity(8);
        for dy in -1..=1 {
            for dx in -1..=1 {
                let (nx, ny) = (x + dx, y + dy);
                if (dx, dy) != (0, 0) && nx >= 0 && ny >= 0 && (nx as usize) < self.w && (ny as usize) < self.h {
                    v.push(ny as usize * self.w + nx as usize);
                }
            }
        }
        v
    }

    /// Lay the mines, keeping the first square and its neighbours clear.
    fn lay(&mut self, first: usize) {
        let mut safe = self.neighbours(first);
        safe.push(first);
        let mut placed = 0;
        while placed < self.mines {
            let i = rand_below(self.cells.len());
            if self.cells[i] & MINE == 0 && !safe.contains(&i) {
                self.cells[i] |= MINE;
                placed += 1;
            }
        }
        for i in 0..self.cells.len() {
            let n = self.neighbours(i).iter().filter(|&&j| self.cells[j] & MINE != 0).count() as u8;
            self.cells[i] |= n;
        }
    }

    fn open(&mut self, i: usize, sfx: &mut Sfx) {
        if self.cells[i] & (OPEN | FLAG) != 0 {
            return;
        }
        if self.cells[i] & MINE != 0 {
            self.cells[i] |= OPEN;
            self.boom = Some(i);
            self.state = State::Lost;
            sfx.play(Sound::Boom);
            return;
        }
        // Flood out from empty squares.
        let mut stack = vec![i];
        while let Some(j) = stack.pop() {
            if self.cells[j] & (OPEN | FLAG) != 0 {
                continue;
            }
            self.cells[j] |= OPEN;
            if self.cells[j] & 0x0F == 0 {
                stack.extend(self.neighbours(j));
            }
        }
    }

    fn check_win(&mut self, scores: &mut Scores, sfx: &mut Sfx) -> bool {
        if self.state == State::Playing && self.cells.iter().all(|&c| c & MINE != 0 || c & OPEN != 0) {
            self.state = State::Won;
            for c in self.cells.iter_mut() {
                if *c & MINE != 0 {
                    *c |= FLAG;
                }
            }
            self.new_best = Scores::best_time(&mut scores.mines[self.level], self.secs);
            sfx.play(Sound::Win);
            return true;
        }
        false
    }

    // ---- layout ---------------------------------------------------------------------------

    fn level_button(r: Rect, k: usize) -> Rect {
        Rect::new(r.x + 12 + k as i32 * 96, r.y + 10, 88, 34)
    }
    fn flag_button(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 232, r.y + 10, 104, 34)
    }
    fn new_button(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 120, r.y + 10, 108, 34)
    }
    fn grid(&self, r: Rect) -> (i32, i32, i32) {
        let top = r.y + 96;
        let size = ((r.w - 24) / self.w as i32).min((r.h - 110) / self.h as i32).clamp(14, 44);
        let gw = size * self.w as i32;
        (r.x + (r.w - gw) / 2, top, size)
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect, scores: &Scores) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(236, 240, 245));
        for k in 0..3 {
            button(c, Self::level_button(r, k), LEVELS[k].3, self.level == k);
        }
        button(c, Self::flag_button(r), if self.flag_mode { "Flagging" } else { "Flag" }, self.flag_mode);
        button(c, Self::new_button(r), "New game", false);
        let left = self.mines as i32 - self.cells.iter().filter(|&&c| c & FLAG != 0).count() as i32;
        let best = scores.mines[self.level];
        let info = format!("Mines left: {}     Time: {}     Best: {}", left, time_text(self.secs), if best > 0 { time_text(best) } else { String::from("-") });
        c.text(r.x + 14, r.y + 58, &info, Size::Small, true, rgb(60, 66, 76));
        let (gx, gy, s) = self.grid(r);
        let lost = self.state == State::Lost;
        for i in 0..self.cells.len() {
            let (x, y) = (gx + (i % self.w) as i32 * s, gy + (i / self.w) as i32 * s);
            let cell = self.cells[i];
            let shown = cell & OPEN != 0 || (lost && cell & MINE != 0 && cell & FLAG == 0);
            if !shown {
                c.round_rect(x + 1, y + 1, s - 2, s - 2, 4, rgb(120, 156, 204));
                c.round_rect(x + 3, y + 3, s - 6, s - 8, 3, rgb(150, 184, 228));
                if cell & FLAG != 0 {
                    let wrong = lost && cell & MINE == 0;
                    c.fill_rect(x + s / 2 - 1, y + s / 4, 2, s / 2 + 1, rgb(50, 50, 50));
                    c.triangle([(x + s / 2 + 1, y + s / 4), (x + s / 2 + 1, y + s / 2), (x + s / 4 - 1, y + s * 3 / 8)], if wrong { rgb(120, 120, 120) } else { rgb(220, 50, 50) });
                    c.fill_rect(x + s / 3, y + s * 3 / 4, s / 3 + 1, 2, rgb(50, 50, 50));
                }
                continue;
            }
            let bg = if self.boom == Some(i) { rgb(240, 90, 80) } else { rgb(250, 251, 253) };
            c.fill_rect(x, y, s, s, rgb(210, 216, 224));
            c.fill_rect(x + 1, y + 1, s - 1, s - 1, bg);
            if cell & MINE != 0 {
                let (cx, cy, rr) = (x + s / 2, y + s / 2, s / 4);
                for (dx, dy) in [(1, 0), (0, 1), (1, 1), (1, -1)] {
                    super::line(c, cx - dx * rr * 3 / 2, cy - dy * rr * 3 / 2, cx + dx * rr * 3 / 2, cy + dy * rr * 3 / 2, 2, rgb(30, 30, 34));
                }
                c.circle(cx, cy, rr, rgb(30, 30, 34));
                c.circle(cx - rr / 3, cy - rr / 3, (rr / 4).max(1), rgb(230, 230, 230));
                continue;
            }
            let n = cell & 0x0F;
            if n > 0 {
                let col = [rgb(40, 90, 210), rgb(30, 140, 60), rgb(210, 50, 50), rgb(90, 40, 170), rgb(150, 60, 20), rgb(20, 140, 150), rgb(40, 40, 40), rgb(110, 110, 110)][n as usize - 1];
                let size = if s >= 30 { Size::Medium } else { Size::Small };
                let txt = format!("{}", n);
                let th = if s >= 30 { 20 } else { 16 };
                c.text_centered(x + s / 2, y + (s - th) / 2, &txt, size, true, col);
            }
        }
        match self.state {
            State::Won => banner(c, r, "You won!", &if self.new_best { format!("New best time: {}", time_text(self.secs)) } else { format!("Time: {}  -  press New game to play again", time_text(self.secs)) }),
            State::Lost => banner(c, r, "Boom!", "Press New game to try again"),
            _ => {}
        }
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, right: bool, scores: &mut Scores, sfx: &mut Sfx) -> GameAction {
        for k in 0..3 {
            if Self::level_button(r, k).contains(x, y) {
                self.reset(k);
                return GameAction::Redraw;
            }
        }
        if Self::new_button(r).contains(x, y) {
            self.reset(self.level);
            return GameAction::Redraw;
        }
        if Self::flag_button(r).contains(x, y) {
            self.flag_mode = !self.flag_mode;
            return GameAction::Redraw;
        }
        if matches!(self.state, State::Won | State::Lost) {
            self.reset(self.level);
            return GameAction::Redraw;
        }
        let (gx, gy, s) = self.grid(r);
        if x < gx || y < gy {
            return GameAction::None;
        }
        let (cx, cy) = (((x - gx) / s) as usize, ((y - gy) / s) as usize);
        if cx >= self.w || cy >= self.h {
            return GameAction::None;
        }
        let i = cy * self.w + cx;
        if right || self.flag_mode {
            if self.cells[i] & OPEN == 0 {
                self.cells[i] ^= FLAG;
                sfx.play(Sound::Click);
            }
            return GameAction::Redraw;
        }
        if self.state == State::Ready {
            if self.cells[i] & FLAG != 0 {
                return GameAction::None;
            }
            self.lay(i);
            self.state = State::Playing;
            self.start = now;
        }
        if self.cells[i] & OPEN != 0 {
            // A number whose mines are all flagged: open the rest around it.
            let n = (self.cells[i] & 0x0F) as usize;
            let nb = self.neighbours(i);
            if n > 0 && nb.iter().filter(|&&j| self.cells[j] & FLAG != 0).count() == n {
                for j in nb {
                    self.open(j, sfx);
                }
            }
        } else {
            self.open(i, sfx);
            if self.state == State::Playing {
                sfx.play(Sound::Click);
            }
        }
        self.check_win(scores, sfx);
        GameAction::Redraw
    }

    pub fn tick(&mut self, now: u64) -> bool {
        if self.state == State::Playing {
            let s = ((now - self.start) / HZ) as u32;
            if s != self.secs {
                self.secs = s;
                return true;
            }
        }
        false
    }
}

use alloc::string::String;

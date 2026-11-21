//! Games: Mines, Solitaire, FreeCell, Sudoku and Star Pinball (an original table).
//! Shared bits: random numbers, smooth lines, little sound effects, best scores.

pub mod cards;
pub mod mines;
pub mod pinball;
pub mod shooter;
pub mod sudoku;

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicU64, Ordering::Relaxed};

use crate::gfx::{rgb, Canvas, Rect, Size};

pub enum GameAction {
    None,
    Redraw,
}

impl GameAction {
    pub fn redraw(&self) -> bool {
        matches!(self, GameAction::Redraw)
    }
}

// ---- random numbers -------------------------------------------------------------------------

static SEED: AtomicU64 = AtomicU64::new(0);

/// A random 32-bit number (xorshift, seeded from the CPU's cycle counter).
pub fn rand() -> u32 {
    let mut s = SEED.load(Relaxed);
    if s == 0 {
        s = unsafe { core::arch::x86_64::_rdtsc() } | 1;
    }
    s ^= s << 13;
    s ^= s >> 7;
    s ^= s << 17;
    SEED.store(s, Relaxed);
    (s >> 16) as u32
}

pub fn rand_below(n: usize) -> usize {
    (rand() as usize) % n.max(1)
}

pub fn shuffle<T>(v: &mut [T]) {
    for i in (1..v.len()).rev() {
        v.swap(i, rand_below(i + 1));
    }
}

// ---- drawing helpers ------------------------------------------------------------------------

/// A smooth line `width` pixels wide.
pub fn line(c: &mut Canvas, x0: i32, y0: i32, x1: i32, y1: i32, width: i32, color: u32) {
    let (dx, dy) = (x1 - x0, y1 - y0);
    let half = (width.max(1) * 128) as i64; // 1/256 px
    if dx.abs() >= dy.abs() {
        if dx == 0 {
            c.circle(x0, y0, width / 2, color);
            return;
        }
        let (xa, xb, ya, yb) = if x0 <= x1 { (x0, x1, y0, y1) } else { (x1, x0, y1, y0) };
        for x in xa..=xb {
            let yc = (ya as i64 * 256) + (yb - ya) as i64 * 256 * (x - xa) as i64 / (xb - xa).max(1) as i64;
            span(c, x, yc, half, true, color);
        }
    } else {
        let (ya, yb, xa, xb) = if y0 <= y1 { (y0, y1, x0, x1) } else { (y1, y0, x1, x0) };
        for y in ya..=yb {
            let xc = (xa as i64 * 256) + (xb - xa) as i64 * 256 * (y - ya) as i64 / (yb - ya).max(1) as i64;
            span(c, y, xc, half, false, color);
        }
    }
    // Round ends.
    c.circle(x0, y0, width / 2, color);
    c.circle(x1, y1, width / 2, color);
}

/// Pixels across a line at `pos` (the other coordinate, centred at `centre` in 1/256 px).
fn span(c: &mut Canvas, pos: i32, centre: i64, half: i64, vertical_span: bool, color: u32) {
    let lo = centre - half;
    let hi = centre + half;
    let mut p = (lo >> 8) as i32;
    while (p as i64) * 256 <= hi {
        let a = (p as i64 * 256).max(lo);
        let b = ((p as i64 + 1) * 256).min(hi);
        let cover = (b - a).clamp(0, 256) as u32;
        if cover > 0 {
            let alpha = (cover * 255 / 256).min(255);
            if vertical_span {
                c.blend(pos, p, color, alpha);
            } else {
                c.blend(p, pos, color, alpha);
            }
        }
        p += 1;
    }
}

/// A ring (circle outline) of thickness `t`.
pub fn ring(c: &mut Canvas, cx: i32, cy: i32, r: i32, t: i32, color: u32) {
    for y in -r..=r {
        for x in -r..=r {
            let d2 = x * x + y * y;
            if d2 <= r * r && d2 >= (r - t).max(0) * (r - t).max(0) {
                c.blend(cx + x, cy + y, color, 255);
            }
        }
    }
}

pub fn button(c: &mut Canvas, b: Rect, label: &str, on: bool) {
    c.round_rect(b.x, b.y, b.w, b.h, 8, if on { rgb(46, 134, 222) } else { rgb(226, 230, 236) });
    c.text_centered(b.x + b.w / 2, b.y + (b.h - 16) / 2, label, Size::Small, true, if on { rgb(255, 255, 255) } else { rgb(40, 44, 52) });
}

pub fn time_text(secs: u32) -> String {
    format!("{}:{:02}", secs / 60, secs % 60)
}

/// A rounded banner in the middle of `r` (won / lost messages).
pub fn banner(c: &mut Canvas, r: Rect, title: &str, line2: &str) {
    let w = (crate::gfx::text_width(title, Size::Large, true).max(crate::gfx::text_width(line2, Size::Small, false)) + 60).min(r.w - 20);
    let (x, y) = (r.x + (r.w - w) / 2, r.y + r.h / 2 - 60);
    c.round_rect_alpha(x + 4, y + 6, w, 120, 16, 0, 90);
    c.round_rect(x, y, w, 120, 16, rgb(255, 255, 255));
    c.text_centered(x + w / 2, y + 22, title, Size::Large, true, rgb(40, 44, 52));
    c.text_centered(x + w / 2, y + 74, line2, Size::Small, false, rgb(90, 96, 106));
}

// ---- sound effects ----------------------------------------------------------------------------

#[derive(Clone, Copy)]
pub enum Sound {
    Click,
    Bump,
    Flip,
    Launch,
    Drop,
    Boom,
    Win,
    Lost,
}

struct Voice {
    freq: u32,
    end_freq: u32,
    t: u32,
    len: u32,
    vol: i32,
    noise: bool,
}

/// A few overlapping beeps, mixed straight into the sound card.
pub struct Sfx {
    voices: Vec<Voice>,
    running: bool,
    idle: u32,
    phase: Vec<u32>,
    noise: u32,
}

impl Sfx {
    pub fn new() -> Sfx {
        Sfx { voices: Vec::new(), running: false, idle: 0, phase: Vec::new(), noise: 12345 }
    }

    pub fn play(&mut self, s: Sound) {
        if !crate::hda::available() {
            return;
        }
        let ms = 48;
        let mut add = |freq: u32, end_freq: u32, len_ms: u32, vol: i32, noise: bool| self.voices.push(Voice { freq, end_freq, t: 0, len: len_ms * ms, vol, noise });
        match s {
            Sound::Click => add(1800, 1200, 25, 2500, false),
            Sound::Bump => add(520, 260, 90, 5000, false),
            Sound::Flip => add(220, 120, 40, 3500, true),
            Sound::Launch => add(180, 900, 220, 4000, false),
            Sound::Drop => add(900, 450, 80, 4000, false),
            Sound::Boom => add(160, 40, 600, 9000, true),
            Sound::Win => {
                add(523, 523, 150, 4000, false);
                self.voices.push(Voice { freq: 659, end_freq: 659, t: 0, len: 300 * ms, vol: 3500, noise: false });
                self.voices.push(Voice { freq: 784, end_freq: 784, t: 0, len: 450 * ms, vol: 3000, noise: false });
            }
            Sound::Lost => add(400, 120, 500, 4000, false),
        }
        if self.voices.len() > 12 {
            self.voices.remove(0);
        }
    }

    /// Top up the sound card (call every tick while a game has focus).
    pub fn pump(&mut self) {
        if self.voices.is_empty() {
            if self.running {
                self.idle += 1;
                if self.idle > 100 {
                    crate::hda::stop();
                    self.running = false;
                }
            }
            if !self.running {
                return;
            }
        }
        if !self.running {
            crate::hda::start();
            self.running = true;
        }
        self.idle = 0;
        let n = crate::hda::wanted().min(4800);
        if n == 0 {
            return;
        }
        let mut out = alloc::vec![0i16; n * 2];
        self.phase.resize(self.voices.len(), 0);
        for (vi, v) in self.voices.iter_mut().enumerate() {
            for k in 0..n {
                if v.t >= v.len {
                    break;
                }
                let f = v.freq as i64 + (v.end_freq as i64 - v.freq as i64) * v.t as i64 / v.len as i64;
                self.phase[vi] = self.phase[vi].wrapping_add((f as u32).wrapping_mul(89478)); // 2^32 / 48000
                let env = v.vol * (v.len - v.t) as i32 / v.len as i32;
                let s = if v.noise {
                    self.noise ^= self.noise << 13;
                    self.noise ^= self.noise >> 17;
                    self.noise ^= self.noise << 5;
                    if (self.noise >> 3) & 1 == 1 { env } else { -env }
                } else if self.phase[vi] < 0x8000_0000 {
                    env
                } else {
                    -env
                };
                out[k * 2] = out[k * 2].saturating_add(s as i16);
                out[k * 2 + 1] = out[k * 2 + 1].saturating_add(s as i16);
                v.t += 1;
            }
        }
        let mut i = 0;
        while i < self.voices.len() {
            if self.voices[i].t >= self.voices[i].len {
                self.voices.remove(i);
                self.phase.remove(i);
            } else {
                i += 1;
            }
        }
        crate::hda::queue(&out);
    }

    pub fn silence(&mut self) {
        self.voices.clear();
        if self.running {
            crate::hda::stop();
            self.running = false;
        }
    }
}

// ---- best scores --------------------------------------------------------------------------------

/// Kept in /Games.txt in Solon storage.
#[derive(Clone, Default, PartialEq)]
pub struct Scores {
    /// Mines best times (seconds) per level; 0 = none yet.
    pub mines: [u32; 3],
    pub sudoku: [u32; 3],
    pub solitaire: (u32, u32),
    pub freecell: (u32, u32),
    pub pinball: u32,
    /// Starbase Breach: the furthest deck unlocked.
    pub shooter: u32,
}

impl Scores {
    pub fn to_text(&self) -> String {
        format!(
            "mines={},{},{}\nsudoku={},{},{}\nsolitaire={},{}\nfreecell={},{}\npinball={}\nshooter={}\n",
            self.mines[0], self.mines[1], self.mines[2], self.sudoku[0], self.sudoku[1], self.sudoku[2], self.solitaire.0, self.solitaire.1, self.freecell.0, self.freecell.1, self.pinball, self.shooter
        )
    }

    pub fn from_text(t: &str) -> Scores {
        let mut s = Scores::default();
        for line in t.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let n: Vec<u32> = v.split(',').map(|x| x.trim().parse().unwrap_or(0)).collect();
            let g = |i: usize| n.get(i).copied().unwrap_or(0);
            match k.trim() {
                "mines" => s.mines = [g(0), g(1), g(2)],
                "sudoku" => s.sudoku = [g(0), g(1), g(2)],
                "solitaire" => s.solitaire = (g(0), g(1)),
                "freecell" => s.freecell = (g(0), g(1)),
                "pinball" => s.pinball = g(0),
                "shooter" => s.shooter = g(0),
                _ => {}
            }
        }
        s
    }

    /// Record a time if it's the best for that level. Returns true if it's a new best.
    pub fn best_time(slot: &mut u32, secs: u32) -> bool {
        if *slot == 0 || secs < *slot {
            *slot = secs.max(1);
            true
        } else {
            false
        }
    }
}

// ---- all the games together ----------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Game {
    Mines,
    Solitaire,
    FreeCell,
    Sudoku,
    Pinball,
    Shooter,
}

pub struct Games {
    pub mines: mines::Mines,
    pub solitaire: cards::Cards,
    pub freecell: cards::Cards,
    pub sudoku: sudoku::Sudoku,
    pub pinball: pinball::Pinball,
    pub shooter: shooter::Shooter,
    pub scores: Scores,
    saved: Scores,
    pub loaded: bool,
    sfx: Sfx,
    sound_for: Option<Game>,
}

impl Games {
    pub fn new() -> Games {
        Games {
            mines: mines::Mines::new(),
            solitaire: cards::Cards::new(cards::Kind::Klondike),
            freecell: cards::Cards::new(cards::Kind::FreeCell),
            sudoku: sudoku::Sudoku::new(),
            pinball: pinball::Pinball::new(),
            shooter: shooter::Shooter::new(),
            scores: Scores::default(),
            saved: Scores::default(),
            loaded: false,
            sfx: Sfx::new(),
            sound_for: None,
        }
    }

    /// Best scores from /Games.txt.
    pub fn load(&mut self, text: Option<&str>) {
        self.loaded = true;
        if let Some(t) = text {
            self.scores = Scores::from_text(t);
            self.saved = self.scores.clone();
        }
        self.pinball.high = self.scores.pinball;
        self.shooter.best_level = self.scores.shooter as usize;
    }

    /// The scores to write, if they changed.
    pub fn take_save(&mut self) -> Option<String> {
        if self.pinball.high > self.scores.pinball {
            self.scores.pinball = self.pinball.high;
        }
        if self.shooter.best_level as u32 > self.scores.shooter {
            self.scores.shooter = self.shooter.best_level as u32;
        }
        if self.scores != self.saved {
            self.saved = self.scores.clone();
            return Some(self.scores.to_text());
        }
        None
    }

    pub fn draw(&mut self, g: Game, c: &mut Canvas, r: Rect) {
        c.set_clip(r);
        match g {
            Game::Mines => self.mines.draw(c, r, &self.scores),
            Game::Solitaire => self.solitaire.draw(c, r, self.scores.solitaire),
            Game::FreeCell => self.freecell.draw(c, r, self.scores.freecell),
            Game::Sudoku => self.sudoku.draw(c, r, &self.scores),
            Game::Pinball => self.pinball.draw(c, r),
            Game::Shooter => self.shooter.draw(c, r),
        }
    }

    pub fn press(&mut self, g: Game, x: i32, y: i32, r: Rect, now: u64, right: bool) -> bool {
        let s = &mut self.sfx;
        match g {
            Game::Mines => self.mines.on_press(x, y, r, now, right, &mut self.scores, s).redraw(),
            Game::Solitaire if !right => self.solitaire.on_press(x, y, r, &mut self.scores.solitaire, s).redraw(),
            Game::FreeCell if !right => self.freecell.on_press(x, y, r, &mut self.scores.freecell, s).redraw(),
            Game::Sudoku if !right => self.sudoku.on_press(x, y, r, now, &mut self.scores, s).redraw(),
            Game::Pinball if !right => self.pinball.on_press(x, y, r, s).redraw(),
            Game::Shooter if !right => self.shooter.on_press(x, y, r, s).redraw(),
            _ => false,
        }
    }

    pub fn drag(&mut self, g: Game, x: i32, y: i32) -> bool {
        match g {
            Game::Solitaire => self.solitaire.on_drag(x, y),
            Game::FreeCell => self.freecell.on_drag(x, y),
            _ => false,
        }
    }

    pub fn release(&mut self, g: Game, r: Rect) -> bool {
        let s = &mut self.sfx;
        match g {
            Game::Solitaire => self.solitaire.on_release(r, &mut self.scores.solitaire, s),
            Game::FreeCell => self.freecell.on_release(r, &mut self.scores.freecell, s),
            Game::Pinball => self.pinball.on_release(s),
            Game::Shooter => self.shooter.on_release(),
            _ => false,
        }
    }

    /// A key for game `g`. None = not wanted.
    pub fn key(&mut self, g: Game, sc: u8, now: u64) -> Option<bool> {
        let s = &mut self.sfx;
        match g {
            Game::Sudoku => self.sudoku.on_key(sc, now, &mut self.scores, s).map(|a| a.redraw()),
            Game::Pinball => self.pinball.on_key(sc, s).map(|a| a.redraw()),
            Game::Shooter => self.shooter.on_key(sc, s).map(|a| a.redraw()),
            _ => None,
        }
    }

    /// Timers, animations, physics and sound. `visible`: game windows on screen;
    /// `focused`: the game that has the keyboard.
    pub fn tick(&mut self, visible: &[Game], focused: Option<Game>, now: u64) -> bool {
        let mut redraw = false;
        for &g in visible {
            redraw |= match g {
                Game::Mines => self.mines.tick(now),
                Game::Sudoku => self.sudoku.tick(now),
                Game::Solitaire => self.solitaire.tick(now, &mut self.scores.solitaire, &mut self.sfx),
                Game::FreeCell => self.freecell.tick(now, &mut self.scores.freecell, &mut self.sfx),
                Game::Pinball => self.pinball.tick(now, focused == Some(Game::Pinball), &mut self.sfx),
                Game::Shooter => self.shooter.tick(now, focused == Some(Game::Shooter), &mut self.sfx),
            };
        }
        if !visible.contains(&Game::Pinball) || focused != Some(Game::Pinball) {
            self.pinball.focus_lost();
        }
        // Sound only for the game in front.
        if focused != self.sound_for {
            if self.sound_for.is_some() {
                self.sfx.silence();
            }
            self.sound_for = focused;
        }
        if focused.is_some() {
            self.sfx.pump();
        }
        redraw
    }

    pub fn closed(&mut self, g: Game) {
        if self.sound_for == Some(g) {
            self.sfx.silence();
            self.sound_for = None;
        }
    }
}

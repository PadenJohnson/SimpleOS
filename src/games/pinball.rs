//! Star Pinball: an original space-themed table. Three planet bumpers, three rollover lanes
//! (light all three to raise the multiplier), four star targets (knock them all down to
//! light the wormhole jackpot), two slingshots, a plunger and a ball saver.
//!
//! The table is 620 x 1000 units; physics runs 8 small steps per 10 ms tick.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{button, line, rand_below, ring, GameAction, Sfx, Sound};
use crate::gfx::{rgb, Canvas, Rect, Size};
use crate::image::Image;

const BALL_R: f32 = 10.0;
const FLIP_R: f32 = 9.0;
const FLIP_LEN: f32 = 84.0;
const GRAVITY: f32 = 1400.0;
const STEPS: usize = 8;
const BUMPERS: [(f32, f32); 3] = [(220.0, 330.0), (355.0, 330.0), (288.0, 440.0)];
const BUMPER_R: f32 = 28.0;
const POSTS: [f32; 4] = [200.0, 260.0, 320.0, 380.0];
const TARGET_Y: [f32; 4] = [430.0, 480.0, 530.0, 580.0];
const TARGET_X: f32 = 48.0;
const HOLE: (f32, f32) = (475.0, 230.0);
const REST: (f32, f32) = (567.0, 944.0);
const SAVER_TICKS: u64 = 1000; // 10 s

#[derive(Clone, Copy, PartialEq, Eq)]
enum State {
    Ready,
    Playing,
    Over,
}

struct Flipper {
    px: f32,
    py: f32,
    rest: f32,
    up: f32,
    angle: f32,
    omega: f32,
}

impl Flipper {
    fn tip(&self) -> (f32, f32) {
        (self.px + FLIP_LEN * libm::cosf(self.angle), self.py + FLIP_LEN * libm::sinf(self.angle))
    }
}

pub struct Pinball {
    state: State,
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    in_play: bool,
    held_until: u64,
    flippers: [Flipper; 2],
    pressed: [bool; 2],
    pulling: bool,
    pull: f32,
    score: u32,
    ball_no: u32,
    balls: u32,
    mult: u32,
    lanes: [bool; 3],
    targets_down: [bool; 4],
    hole_lit: bool,
    flash: [u32; 5],
    message: Option<(String, u64)>,
    saver_until: u64,
    extra_at: u32,
    pub high: u32,
    now: u64,
    slow_since: u64,
    walls: Vec<(f32, f32, f32, f32)>,
    slings: [((f32, f32), (f32, f32)); 2],
    table: Option<(i32, Image)>,
    frame: u64,
    pub new_high: bool,
}

/// Closest point on segment ab to p, and the distance.
fn closest(px: f32, py: f32, ax: f32, ay: f32, bx: f32, by: f32) -> (f32, f32, f32) {
    let (dx, dy) = (bx - ax, by - ay);
    let len2 = dx * dx + dy * dy;
    let t = if len2 > 0.0 { (((px - ax) * dx + (py - ay) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
    let (cx, cy) = (ax + dx * t, ay + dy * t);
    let d = libm::sqrtf((px - cx) * (px - cx) + (py - cy) * (py - cy));
    (cx, cy, d)
}

impl Pinball {
    pub fn new() -> Pinball {
        let mut walls = Vec::new();
        let mut seg = |a: (f32, f32), b: (f32, f32)| walls.push((a.0, a.1, b.0, b.1));
        seg((30.0, 1000.0), (30.0, 300.0));
        // The top arch.
        let n = 28;
        for k in 0..n {
            let a0 = core::f32::consts::PI * (1.0 + k as f32 / n as f32);
            let a1 = core::f32::consts::PI * (1.0 + (k + 1) as f32 / n as f32);
            seg((310.0 + 280.0 * libm::cosf(a0), 300.0 + 280.0 * libm::sinf(a0)), (310.0 + 280.0 * libm::cosf(a1), 300.0 + 280.0 * libm::sinf(a1)));
        }
        seg((590.0, 300.0), (590.0, 1000.0));
        // Plunger lane.
        seg((545.0, 1000.0), (545.0, 340.0));
        seg((545.0, 955.0), (590.0, 955.0));
        // Inlane guides down to the flippers.
        seg((90.0, 690.0), (90.0, 800.0));
        seg((90.0, 800.0), (176.0, 873.0));
        seg((485.0, 690.0), (485.0, 800.0));
        seg((485.0, 800.0), (399.0, 873.0));
        // Slingshot backs.
        seg((118.0, 712.0), (118.0, 800.0));
        seg((118.0, 800.0), (162.0, 835.0));
        seg((457.0, 712.0), (457.0, 800.0));
        seg((457.0, 800.0), (413.0, 835.0));
        // Lane posts.
        for x in POSTS {
            seg((x, 95.0), (x, 150.0));
        }
        let mk = |px: f32, rest: f32, up: f32| Flipper { px, py: 880.0, rest, up, angle: rest, omega: 0.0 };
        let mut p = Pinball {
            state: State::Ready,
            x: REST.0,
            y: REST.1,
            vx: 0.0,
            vy: 0.0,
            in_play: false,
            held_until: 0,
            flippers: [mk(180.0, 0.52, -0.52), mk(395.0, core::f32::consts::PI - 0.52, core::f32::consts::PI + 0.52)],
            pressed: [false; 2],
            pulling: false,
            pull: 0.0,
            score: 0,
            ball_no: 1,
            balls: 3,
            mult: 1,
            lanes: [false; 3],
            targets_down: [false; 4],
            hole_lit: false,
            flash: [0; 5],
            message: None,
            saver_until: 0,
            extra_at: 150_000,
            high: 0,
            now: 0,
            slow_since: 0,
            walls,
            slings: [((118.0, 712.0), (162.0, 835.0)), ((457.0, 712.0), (413.0, 835.0))],
            table: None,
            frame: 0,
            new_high: false,
        };
        p.reset_ball();
        p
    }

    fn new_game(&mut self) {
        self.state = State::Playing;
        self.score = 0;
        self.ball_no = 1;
        self.balls = 3;
        self.mult = 1;
        self.lanes = [false; 3];
        self.targets_down = [false; 4];
        self.hole_lit = false;
        self.extra_at = 150_000;
        self.new_high = false;
        self.reset_ball();
        self.say("Hold Space (or Launch) and let go!");
    }

    fn reset_ball(&mut self) {
        self.x = REST.0;
        self.y = REST.1;
        self.vx = 0.0;
        self.vy = 0.0;
        self.in_play = false;
        self.held_until = 0;
    }

    fn say(&mut self, s: &str) {
        self.message = Some((String::from(s), self.now + 250));
    }

    fn add(&mut self, points: u32, sfx: &mut Sfx) {
        self.score = self.score.saturating_add(points * self.mult);
        if self.score >= self.extra_at {
            self.extra_at = self.extra_at.saturating_mul(3);
            self.balls += 1;
            self.say("Extra ball!");
            sfx.play(Sound::Win);
        }
    }

    fn launch(&mut self, sfx: &mut Sfx) {
        if self.state != State::Playing {
            self.new_game();
            return;
        }
        let near_rest = self.x > 545.0 && self.y > 880.0 && self.vy.abs() < 60.0;
        if near_rest {
            self.vy = -(1100.0 + 1300.0 * self.pull);
            self.vx = 0.0;
            if !self.in_play {
                self.in_play = true;
                self.saver_until = self.now + SAVER_TICKS;
            }
            sfx.play(Sound::Launch);
        }
        self.pull = 0.0;
    }

    // ---- physics ---------------------------------------------------------------------------

    fn step(&mut self, dt: f32, sfx: &mut Sfx) {
        // Flippers.
        for (k, f) in self.flippers.iter_mut().enumerate() {
            let target = if self.pressed[k] { f.up } else { f.rest };
            let speed = if self.pressed[k] { 22.0 } else { 14.0 };
            let diff = target - f.angle;
            let max = speed * dt;
            let d = diff.clamp(-max, max);
            f.angle += d;
            f.omega = d / dt;
        }
        if self.now < self.held_until {
            return;
        }
        self.vy += GRAVITY * dt;
        let sp = libm::sqrtf(self.vx * self.vx + self.vy * self.vy);
        if sp > 3200.0 {
            self.vx *= 3200.0 / sp;
            self.vy *= 3200.0 / sp;
        }
        self.x += self.vx * dt;
        self.y += self.vy * dt;
        // Walls.
        for i in 0..self.walls.len() {
            let (ax, ay, bx, by) = self.walls[i];
            self.collide_segment(ax, ay, bx, by, 0.45, 0.0);
        }
        // Star targets.
        for k in 0..4 {
            if !self.targets_down[k] {
                let y = TARGET_Y[k];
                if self.collide_segment(TARGET_X, y - 18.0, TARGET_X, y + 18.0, 0.5, 0.0) {
                    self.targets_down[k] = true;
                    self.flash[3] = 15;
                    sfx.play(Sound::Drop);
                    self.add(750, sfx);
                    if self.targets_down.iter().all(|&d| d) {
                        self.targets_down = [false; 4];
                        self.hole_lit = true;
                        self.add(5000, sfx);
                        self.say("Stars complete! The wormhole is lit");
                    }
                }
            }
        }
        // Slingshots: the slanted faces kick.
        for k in 0..2 {
            let ((ax, ay), (bx, by)) = self.slings[k];
            if self.collide_segment(ax, ay, bx, by, 0.4, 520.0) {
                self.flash[4] = 10;
                sfx.play(Sound::Bump);
                self.add(10, sfx);
            }
        }
        // Bumpers.
        for (k, &(bx, by)) in BUMPERS.iter().enumerate() {
            let (dx, dy) = (self.x - bx, self.y - by);
            let d = libm::sqrtf(dx * dx + dy * dy);
            if d < BUMPER_R + BALL_R && d > 0.01 {
                let (nx, ny) = (dx / d, dy / d);
                self.x = bx + nx * (BUMPER_R + BALL_R);
                self.y = by + ny * (BUMPER_R + BALL_R);
                let vn = self.vx * nx + self.vy * ny;
                if vn < 0.0 {
                    self.vx -= 2.0 * vn * nx;
                    self.vy -= 2.0 * vn * ny;
                }
                let out = self.vx * nx + self.vy * ny;
                let boost = (700.0 - out).max(250.0);
                self.vx += nx * boost;
                self.vy += ny * boost;
                self.flash[k] = 12;
                sfx.play(Sound::Bump);
                self.add(100, sfx);
            }
        }
        // Flippers (moving surfaces).
        for k in 0..2 {
            let f = &self.flippers[k];
            let (tx, ty) = f.tip();
            let (cx, cy, d) = closest(self.x, self.y, f.px, f.py, tx, ty);
            if d < BALL_R + FLIP_R && d > 0.01 {
                let (nx, ny) = ((self.x - cx) / d, (self.y - cy) / d);
                self.x = cx + nx * (BALL_R + FLIP_R);
                self.y = cy + ny * (BALL_R + FLIP_R);
                // Velocity of the flipper surface at the contact point.
                let (sx, sy) = (-f.omega * (cy - f.py), f.omega * (cx - f.px));
                let (rx, ry) = (self.vx - sx, self.vy - sy);
                let vn = rx * nx + ry * ny;
                if vn < 0.0 {
                    let e = 0.3;
                    self.vx = sx + rx - (1.0 + e) * vn * nx;
                    self.vy = sy + ry - (1.0 + e) * vn * ny;
                }
            }
        }
        // Rollover lanes (going down through them).
        if self.vy > 0.0 && (118.0..132.0).contains(&self.y) {
            for k in 0..3 {
                if self.x > POSTS[k] && self.x < POSTS[k + 1] && !self.lanes[k] {
                    self.lanes[k] = true;
                    sfx.play(Sound::Click);
                    self.add(500, sfx);
                    if self.lanes.iter().all(|&l| l) {
                        self.lanes = [false; 3];
                        self.mult = (self.mult + 1).min(5);
                        self.add(2000, sfx);
                        let m = format!("Multiplier x{}!", self.mult);
                        self.say(&m);
                    }
                }
            }
        }
        // The wormhole.
        let (hx, hy) = (self.x - HOLE.0, self.y - HOLE.1);
        if hx * hx + hy * hy < 16.0 * 16.0 && self.held_until == 0 {
            self.x = HOLE.0;
            self.y = HOLE.1;
            self.vx = -280.0 + rand_below(120) as f32;
            self.vy = 520.0;
            self.held_until = self.now + 120;
            if self.hole_lit {
                self.hole_lit = false;
                self.add(25_000, sfx);
                self.say("WORMHOLE JACKPOT!");
                sfx.play(Sound::Win);
            } else {
                self.add(2500, sfx);
                self.say("Wormhole!");
                sfx.play(Sound::Launch);
            }
        }
        if self.held_until != 0 && self.now >= self.held_until + 30 {
            self.held_until = 0;
        }
    }

    /// Bounce off a segment; `kick` pushes the ball away from it. True if it touched.
    fn collide_segment(&mut self, ax: f32, ay: f32, bx: f32, by: f32, e: f32, kick: f32) -> bool {
        let (cx, cy, d) = closest(self.x, self.y, ax, ay, bx, by);
        if d >= BALL_R || d < 0.001 {
            return false;
        }
        let (nx, ny) = ((self.x - cx) / d, (self.y - cy) / d);
        self.x = cx + nx * BALL_R;
        self.y = cy + ny * BALL_R;
        let vn = self.vx * nx + self.vy * ny;
        if vn < 0.0 {
            self.vx -= (1.0 + e) * vn * nx;
            self.vy -= (1.0 + e) * vn * ny;
            // A little friction along the wall.
            self.vx *= 0.995;
            self.vy *= 0.995;
            if kick > 0.0 && vn < -60.0 {
                self.vx += nx * kick;
                self.vy += ny * kick;
            }
            return true;
        }
        false
    }

    pub fn tick(&mut self, now: u64, focused: bool, sfx: &mut Sfx) -> bool {
        let elapsed = (now.saturating_sub(self.now)).min(3);
        self.now = now;
        if !focused {
            self.pressed = [false; 2];
            self.pulling = false;
            return false;
        }
        if self.pulling {
            self.pull = (self.pull + 0.02 * elapsed as f32).min(1.0);
        }
        for f in self.flash.iter_mut() {
            *f = f.saturating_sub(elapsed as u32);
        }
        if self.state == State::Playing {
            let dt = 0.01 / STEPS as f32;
            for _ in 0..elapsed as usize * STEPS {
                self.step(dt, sfx);
            }
            // Drained?
            if self.y > 1010.0 {
                if now < self.saver_until {
                    self.reset_ball();
                    self.in_play = false;
                    self.say("Ball saved! Launch again");
                } else {
                    sfx.play(Sound::Lost);
                    self.reset_ball();
                    self.mult = 1;
                    if self.ball_no >= self.balls {
                        self.state = State::Over;
                        if self.score > self.high {
                            self.high = self.score;
                            self.new_high = true;
                        }
                        self.say("Game over");
                    } else {
                        self.ball_no += 1;
                        let m = format!("Ball {} of {}", self.ball_no, self.balls);
                        self.say(&m);
                    }
                }
            }
            // A ball stuck somewhere gets a nudge.
            let slow = self.vx.abs() + self.vy.abs() < 25.0 && self.in_play && self.y < 900.0 && self.held_until == 0;
            if !slow {
                self.slow_since = now;
            } else if now > self.slow_since + 300 {
                self.vy = -500.0;
                self.vx = rand_below(400) as f32 - 200.0;
                self.slow_since = now;
            }
        }
        self.frame += 1;
        self.state == State::Playing || self.pulling || self.flash.iter().any(|&f| f > 0) || self.frame % 25 == 0
    }

    // ---- drawing -------------------------------------------------------------------------------

    fn scale(r: Rect) -> (f32, i32, i32) {
        let s = ((r.h - 10) as f32 / 1000.0).min((r.w - 330) as f32 / 620.0).max(0.2);
        let tw = (620.0 * s) as i32;
        let x0 = r.x + ((r.w - 320 - tw) / 2).max(6);
        (s, x0, r.y + 5)
    }

    /// The parts of the table that never change, drawn once per size.
    fn background(&mut self, s: f32) -> &Image {
        let key = (s * 1000.0) as i32;
        if self.table.as_ref().is_none_or(|t| t.0 != key) {
            let (w, h) = ((620.0 * s) as usize + 1, (1000.0 * s) as usize + 1);
            let mut c = Canvas::new(w, h);
            let p = |v: f32| (v * s) as i32;
            c.gradient_v(0, 0, w as i32, h as i32, rgb(10, 12, 40), rgb(40, 14, 60));
            // Nebula and stars (the same every time).
            let mut seed = 0x2468ACEu32;
            let mut rnd = |n: u32| {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                seed % n
            };
            for (cx, cy, rr, col) in [(150.0, 250.0, 160.0, rgb(90, 40, 140)), (430.0, 620.0, 190.0, rgb(30, 70, 140)), (260.0, 820.0, 120.0, rgb(120, 40, 90))] {
                c.circle_alpha(p(cx), p(cy), p(rr), col, 50);
                c.circle_alpha(p(cx), p(cy), p(rr * 0.6), col, 50);
            }
            for _ in 0..260 {
                let (x, y) = (rnd(w as u32) as i32, rnd(h as u32) as i32);
                let b = 120 + rnd(135) as u8;
                c.blend(x, y, rgb(b, b, b.saturating_add(20)), 255);
            }
            // Playfield inside the arch, darker outside.
            let glow = rgb(30, 120, 160);
            let neon = rgb(110, 230, 255);
            for &(ax, ay, bx, by) in &self.walls {
                line(&mut c, p(ax), p(ay), p(bx), p(by), (8.0 * s).max(3.0) as i32, glow);
            }
            for &(ax, ay, bx, by) in &self.walls {
                line(&mut c, p(ax), p(ay), p(bx), p(by), (3.0 * s).max(2.0) as i32, neon);
            }
            // Slingshots: filled triangles.
            for (a, b, cc) in [((118.0, 712.0), (118.0, 800.0), (162.0, 835.0)), ((457.0, 712.0), (457.0, 800.0), (413.0, 835.0))] {
                c.triangle([(p(a.0), p(a.1)), (p(b.0), p(b.1)), (p(cc.0), p(cc.1))], rgb(120, 40, 120));
            }
            for &((ax, ay), (bx, by)) in &self.slings {
                line(&mut c, p(ax), p(ay), p(bx), p(by), (4.0 * s).max(2.0) as i32, rgb(255, 120, 230));
            }
            // Lane arrows and name.
            c.text(p(205.0), p(965.0), "STAR PINBALL", Size::Small, true, rgb(140, 120, 200));
            c.text(p(552.0), p(980.0), "^", Size::Small, true, rgb(140, 120, 200));
            let px: Vec<u32> = c.px.iter().map(|v| v | 0xFF00_0000).collect();
            self.table = Some((key, Image { w, h, px, opaque: true }));
        }
        &self.table.as_ref().unwrap().1
    }

    pub fn draw(&mut self, c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(8, 8, 20));
        let (s, x0, y0) = Self::scale(r);
        let bg = self.background(s);
        c.draw_image(bg, x0, y0);
        let p = |v: f32| (v * s) as i32;
        let at = |x: f32, y: f32| (x0 + p(x), y0 + p(y));
        // Lane lights.
        for k in 0..3 {
            let (cx, cy) = at((POSTS[k] + POSTS[k + 1]) / 2.0, 70.0);
            c.circle(cx, cy, p(11.0).max(3), if self.lanes[k] { rgb(255, 220, 80) } else { rgb(70, 60, 30) });
        }
        // Star targets.
        for k in 0..4 {
            let (tx, ty) = at(TARGET_X - 6.0, TARGET_Y[k] - 18.0);
            if self.targets_down[k] {
                c.fill_rect(tx, ty, p(8.0).max(2), p(36.0), rgb(60, 50, 40));
            } else {
                let col = if self.flash[3] > 0 { rgb(255, 255, 255) } else { rgb(255, 160, 40) };
                c.round_rect(tx, ty, p(12.0).max(3), p(36.0), 3, col);
            }
        }
        // Wormhole.
        let (hx, hy) = at(HOLE.0, HOLE.1);
        let lit = self.hole_lit && (self.frame / 20) % 2 == 0;
        c.circle(hx, hy, p(22.0), if lit { rgb(255, 230, 120) } else { rgb(90, 60, 160) });
        c.circle(hx, hy, p(16.0), rgb(10, 0, 20));
        ring(c, hx, hy, p(10.0).max(3), 2, rgb(140, 100, 220));
        // Bumpers (planets).
        let cols = [(rgb(230, 120, 60), rgb(255, 200, 140)), (rgb(60, 160, 230), rgb(170, 230, 255)), (rgb(120, 200, 90), rgb(210, 250, 170))];
        for (k, &(bx, by)) in BUMPERS.iter().enumerate() {
            let (cx, cy) = at(bx, by);
            let (body, light) = cols[k];
            let rr = p(BUMPER_R);
            c.circle(cx, cy, rr + 3, if self.flash[k] > 0 { rgb(255, 255, 255) } else { rgb(40, 30, 70) });
            c.circle(cx, cy, rr, body);
            c.circle(cx - rr / 3, cy - rr / 3, rr / 3, light);
            ring(c, cx, cy, rr + p(8.0), 2, if self.flash[k] > 0 { rgb(255, 255, 255) } else { light });
        }
        // Plunger.
        let (lx, ly) = at(552.0, 955.0 + self.pull * 35.0);
        c.fill_rect(lx, ly, p(31.0), p(10.0), rgb(200, 200, 210));
        for k in 0..5 {
            c.fill_rect(lx + p(4.0), ly + p(12.0) + k * p(6.0 - self.pull * 3.0).max(2), p(23.0), 2, rgb(150, 150, 160));
        }
        // Flippers.
        for (k, f) in self.flippers.iter().enumerate() {
            let (tx, ty) = f.tip();
            let (ax, ay) = at(f.px, f.py);
            let (bx, by) = at(tx, ty);
            line(c, ax, ay, bx, by, (FLIP_R * 2.0 * s) as i32 + 2, rgb(60, 40, 10));
            line(c, ax, ay, bx, by, (FLIP_R * 2.0 * s) as i32 - 2, if self.pressed[k] { rgb(255, 230, 120) } else { rgb(240, 190, 70) });
            c.circle(ax, ay, p(5.0).max(2), rgb(120, 80, 20));
        }
        // Ball.
        let (bx, by) = at(self.x, self.y);
        let br = p(BALL_R).max(3);
        c.circle(bx + 2, by + 3, br, rgb(10, 10, 20));
        c.circle(bx, by, br, rgb(200, 205, 215));
        c.circle(bx - br / 3, by - br / 3, br / 3, rgb(255, 255, 255));
        // Side panel.
        let px = r.x + r.w - 310;
        let panel = Rect::new(px, r.y + 10, 300, r.h - 20);
        c.round_rect(panel.x, panel.y, panel.w, panel.h, 12, rgb(24, 24, 46));
        c.text(px + 20, r.y + 26, "STAR PINBALL", Size::Medium, true, rgb(160, 140, 255));
        c.text(px + 20, r.y + 66, "Score", Size::Small, false, rgb(150, 150, 190));
        c.text(px + 20, r.y + 86, &group(self.score), Size::Large, true, rgb(255, 230, 120));
        let ball = if self.state == State::Over { String::from("Game over") } else { format!("Ball {} of {}", self.ball_no, self.balls) };
        c.text(px + 20, r.y + 134, &ball, Size::Medium, true, rgb(230, 230, 250));
        c.text(px + 20, r.y + 164, &format!("Multiplier x{}", self.mult), Size::Small, true, rgb(200, 200, 240));
        c.text(px + 20, r.y + 188, &format!("High score {}", group(self.high)), Size::Small, false, rgb(200, 200, 240));
        if self.now < self.saver_until && self.in_play {
            c.text(px + 20, r.y + 212, "Ball saver on", Size::Small, true, rgb(120, 230, 150));
        }
        if let Some((m, until)) = &self.message {
            if self.now < *until || self.state != State::Playing {
                c.text(px + 20, r.y + 240, m, Size::Small, true, rgb(255, 200, 120));
            }
        }
        button(c, Self::launch_button(r), if self.state == State::Playing { "Launch" } else { "Start" }, true);
        // Plunger power.
        let lb = Self::launch_button(r);
        c.round_rect(lb.x, lb.y + lb.h + 8, lb.w, 10, 4, rgb(50, 50, 80));
        c.round_rect(lb.x, lb.y + lb.h + 8, (lb.w as f32 * self.pull) as i32 + 4, 10, 4, rgb(255, 160, 60));
        let help = ["How to play", "Left flipper: Left Shift, Z or Left", "Right flipper: Right Shift, / or Right", "Launch: hold Space, let go", "Or click and hold the left or", "right half of the table.", "Light the 3 lanes: multiplier", "Hit all 4 stars: wormhole jackpot"];
        for (k, h) in help.iter().enumerate() {
            c.text(px + 20, lb.y + lb.h + 34 + k as i32 * 22, h, Size::Small, k == 0, rgb(170, 170, 210));
        }
        if self.state != State::Playing {
            let (tx, ty) = at(310.0, 560.0);
            let w = 300;
            c.round_rect_alpha(tx - w / 2, ty - 40, w, 80, 14, 0, 170);
            let t = if self.state == State::Over { if self.new_high { "New high score!" } else { "Game over" } } else { "Star Pinball" };
            c.text_centered(tx, ty - 28, t, Size::Medium, true, rgb(255, 230, 120));
            c.text_centered(tx, ty + 6, "Press Space or Start to play", Size::Small, false, rgb(220, 220, 240));
        }
    }

    fn launch_button(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 290, r.y + 280, 260, 44)
    }

    // ---- input ---------------------------------------------------------------------------------

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, sfx: &mut Sfx) -> GameAction {
        if Self::launch_button(r).contains(x, y) {
            if self.state != State::Playing {
                self.new_game();
            } else {
                self.pulling = true;
            }
            return GameAction::Redraw;
        }
        let (s, x0, _) = Self::scale(r);
        let tw = (620.0 * s) as i32;
        if x >= x0 && x < x0 + tw {
            if self.state != State::Playing {
                self.new_game();
                return GameAction::Redraw;
            }
            let k = if x < x0 + tw / 2 { 0 } else { 1 };
            self.flip(k, true, sfx);
            return GameAction::Redraw;
        }
        GameAction::None
    }

    pub fn on_release(&mut self, sfx: &mut Sfx) -> bool {
        if self.pulling {
            self.pulling = false;
            self.launch(sfx);
        }
        self.pressed = [false; 2];
        true
    }

    fn flip(&mut self, k: usize, down: bool, sfx: &mut Sfx) {
        if down && !self.pressed[k] {
            sfx.play(Sound::Flip);
            // Changing lanes: the lit lanes move with the flippers.
            if self.state == State::Playing {
                if k == 0 {
                    self.lanes.rotate_left(1);
                } else {
                    self.lanes.rotate_right(1);
                }
            }
        }
        self.pressed[k] = down;
    }

    pub fn on_key(&mut self, sc: u8, sfx: &mut Sfx) -> Option<GameAction> {
        match sc {
            0x2A | 0x2C | 0x4B => self.flip(0, true, sfx),
            0xAA | 0xAC | 0xCB => self.flip(0, false, sfx),
            0x36 | 0x35 | 0x4D => self.flip(1, true, sfx),
            0xB6 | 0xB5 | 0xCD => self.flip(1, false, sfx),
            0x39 | 0x50 => {
                if self.state != State::Playing {
                    self.new_game();
                } else {
                    self.pulling = true;
                }
            }
            0xB9 | 0xD0 => {
                if self.pulling {
                    self.pulling = false;
                    self.launch(sfx);
                }
            }
            _ => return None,
        }
        Some(GameAction::Redraw)
    }

    pub fn focus_lost(&mut self) {
        self.pressed = [false; 2];
        self.pulling = false;
    }
}

/// 1234567 -> "1,234,567".
fn group(n: u32) -> String {
    let s = format!("{}", n);
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

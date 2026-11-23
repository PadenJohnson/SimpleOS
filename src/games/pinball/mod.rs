//! Neon Grill: an original pinball table in the style of a 1980s diner. Checkerboard floor,
//! chrome rails and neon; a burger, a donut and a pie as pop bumpers; the E-A-T rollover lanes,
//! the M-E-N-U drop targets, a fries spinner, a milkshake ramp and the kitchen window (a
//! saucer). The kitchen takes orders: each one asks for three dishes, made by hitting their
//! shots; when all three are ready, shoot the kitchen to serve it. Every third order starts the
//! lunch rush, a two-ball multiball where the shake ramp scores a jackpot. A dot-matrix display
//! shows the score and messages, as on the machines of the time.
//!
//! The table is 620 x 1000 units; physics runs 8 small steps per 10 ms tick.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{rand_below, GameAction, Sfx, Sound};
use crate::gfx::{rgb, Canvas, Rect, Size};

mod art;
mod intro;

const BALL_R: f32 = 10.0;
const FLIP_R: f32 = 9.0;
const FLIP_LEN: f32 = 84.0;
const GRAVITY: f32 = 1400.0;
const STEPS: usize = 8;
/// Pop bumpers: burger, donut, pie.
const BUMPERS: [(f32, f32); 3] = [(220.0, 330.0), (355.0, 330.0), (288.0, 440.0)];
const BUMPER_R: f32 = 28.0;
/// The E-A-T rollover lanes' posts.
const POSTS: [f32; 4] = [200.0, 260.0, 320.0, 380.0];
/// The M-E-N-U drop targets on the left wall.
const TARGET_Y: [f32; 4] = [440.0, 490.0, 540.0, 590.0];
const TARGET_X: f32 = 48.0;
/// The kitchen window (saucer).
const SAUCER: (f32, f32) = (140.0, 245.0);
/// The fries spinner across the left lane: x from, x to, y.
const SPINNER: (f32, f32, f32) = (30.0, 102.0, 380.0);
/// The shake ramp's mouth (x from, x to, y); a weaker shot rolls back out.
const MOUTH: (f32, f32, f32) = (410.0, 450.0, 600.0);
const RAMP_SPEED: f32 = 1150.0;
/// The ramp's path (a hairpin up the right side and back down to the right inlane).
const RAMP: [(f32, f32); 16] = [
    (430.0, 600.0),
    (442.0, 520.0),
    (456.0, 440.0),
    (470.0, 370.0),
    (482.0, 310.0),
    (492.0, 262.0),
    (504.0, 236.0),
    (516.0, 232.0),
    (526.0, 250.0),
    (528.0, 300.0),
    (526.0, 370.0),
    (520.0, 450.0),
    (510.0, 540.0),
    (498.0, 620.0),
    (488.0, 670.0),
    (477.0, 712.0),
];
const REST: (f32, f32) = (567.0, 944.0);
const SAVER_TICKS: u64 = 1000; // 10 s

/// The dishes: name, what makes it, how many hits, and where its lamp is.
const DISHES: [(&str, &str, u32, (f32, f32)); 5] = [
    ("Burger", "bumpers", 8, (288.0, 505.0)),
    ("Fries", "spinner", 12, (72.0, 405.0)),
    ("Shake", "ramp", 1, (430.0, 680.0)),
    ("Pie", "MENU targets", 2, (100.0, 515.0)),
    ("Coffee", "E-A-T lanes", 1, (290.0, 180.0)),
];

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

#[derive(Clone, Copy)]
struct Ball {
    x: f32,
    y: f32,
    vx: f32,
    vy: f32,
    /// Held in the kitchen window until then (and not caught again for a moment after).
    held_until: u64,
    /// Distance along the ramp while riding it.
    ramp: Option<f32>,
    /// Launched (a ball waiting at the plunger isn't).
    in_play: bool,
    slow_since: u64,
}

impl Ball {
    fn at_plunger() -> Ball {
        Ball { x: REST.0, y: REST.1, vx: 0.0, vy: 0.0, held_until: 0, ramp: None, in_play: false, slow_since: 0 }
    }
}

pub struct Pinball {
    state: State,
    balls: Vec<Ball>,
    flippers: [Flipper; 2],
    pressed: [bool; 2],
    pulling: bool,
    pull: f32,
    score: u32,
    ball_no: u32,
    ball_count: u32,
    mult: u32,
    lanes: [bool; 3],
    targets_down: [bool; 4],
    /// A completed M-E-N-U bank stays down a moment before it resets.
    targets_reset_at: u64,
    menus: u32,
    special_lit: bool,
    /// The current order: three dishes and how far each is.
    order: [usize; 3],
    made: [u32; 5],
    order_no: u32,
    served: u32,
    order_up: bool,
    rush: bool,
    dishes_this_ball: u32,
    /// Flashes: bumpers 0-2, targets 3, left sling 4, spinner 5, right sling 6.
    flash: [u32; 7],
    /// The inlane rollovers (left, right).
    inlanes: [bool; 2],
    spin_angle: f32,
    spin_vel: f32,
    /// A message for the display: text, when it appeared, until when.
    message: Option<(String, u64, u64)>,
    saver_until: u64,
    /// Each ball gets one ball saver.
    saver_given: bool,
    extra_at: u32,
    pub high: u32,
    now: u64,
    walls: Vec<(f32, f32, f32, f32)>,
    slings: [((f32, f32), (f32, f32)); 2],
    /// The one-way gate at the top of the plunger lane (stops balls only on the way down).
    gate: ((f32, f32), (f32, f32)),
    ramp_len: f32,
    art: Option<art::Art>,
    lamp_defs: Vec<art::LampDef>,
    /// The opening, while it plays.
    intro: Option<intro::Intro>,
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

/// Bounce a ball off a segment; `kick` pushes it away. True if it touched.
fn collide(b: &mut Ball, ax: f32, ay: f32, bx: f32, by: f32, e: f32, kick: f32) -> bool {
    let (cx, cy, d) = closest(b.x, b.y, ax, ay, bx, by);
    if d >= BALL_R || d < 0.001 {
        return false;
    }
    let (nx, ny) = ((b.x - cx) / d, (b.y - cy) / d);
    b.x = cx + nx * BALL_R;
    b.y = cy + ny * BALL_R;
    let vn = b.vx * nx + b.vy * ny;
    if vn < 0.0 {
        b.vx -= (1.0 + e) * vn * nx;
        b.vy -= (1.0 + e) * vn * ny;
        b.vx *= 0.995;
        b.vy *= 0.995;
        if kick > 0.0 && vn < -60.0 {
            b.vx += nx * kick;
            b.vy += ny * kick;
        }
        return true;
    }
    false
}

fn seg_len(a: (f32, f32), b: (f32, f32)) -> f32 {
    libm::sqrtf((b.0 - a.0) * (b.0 - a.0) + (b.1 - a.1) * (b.1 - a.1))
}

fn path_len() -> f32 {
    RAMP.windows(2).map(|w| seg_len(w[0], w[1])).sum()
}

/// The point `d` units along the ramp.
fn ramp_point(mut d: f32) -> (f32, f32) {
    for w in RAMP.windows(2) {
        let l = seg_len(w[0], w[1]);
        if d <= l {
            let t = d / l.max(0.001);
            return (w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t);
        }
        d -= l;
    }
    RAMP[RAMP.len() - 1]
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
        // Inlane guides down to the flippers (wide enough for the ball all the way down).
        seg((78.0, 690.0), (78.0, 800.0));
        seg((78.0, 800.0), (176.0, 872.0));
        seg((497.0, 690.0), (497.0, 800.0));
        seg((497.0, 800.0), (399.0, 872.0));
        // Slingshot backs.
        seg((118.0, 712.0), (118.0, 800.0));
        seg((118.0, 800.0), (162.0, 835.0));
        seg((457.0, 712.0), (457.0, 800.0));
        seg((457.0, 800.0), (413.0, 835.0));
        // Lane posts.
        for x in POSTS {
            seg((x, 95.0), (x, 150.0));
        }
        // The shake ramp's mouth.
        seg((398.0, 662.0), (MOUTH.0 - 2.0, MOUTH.2));
        seg((462.0, 662.0), (MOUTH.1 + 2.0, MOUTH.2));
        let mk = |px: f32, rest: f32, up: f32| Flipper { px, py: 880.0, rest, up, angle: rest, omega: 0.0 };
        let mut p = Pinball {
            state: State::Ready,
            balls: Vec::from([Ball::at_plunger()]),
            flippers: [mk(180.0, 0.52, -0.52), mk(395.0, core::f32::consts::PI - 0.52, core::f32::consts::PI + 0.52)],
            pressed: [false; 2],
            pulling: false,
            pull: 0.0,
            score: 0,
            ball_no: 1,
            ball_count: 3,
            mult: 1,
            lanes: [false; 3],
            targets_down: [false; 4],
            targets_reset_at: 0,
            menus: 0,
            special_lit: false,
            order: [0, 1, 2],
            made: [0; 5],
            order_no: 0,
            served: 0,
            order_up: false,
            rush: false,
            dishes_this_ball: 0,
            flash: [0; 7],
            inlanes: [false; 2],
            spin_angle: 0.0,
            spin_vel: 0.0,
            message: None,
            saver_until: 0,
            saver_given: false,
            extra_at: 200_000,
            high: 0,
            now: 0,
            walls,
            slings: [((118.0, 712.0), (162.0, 835.0)), ((457.0, 712.0), (413.0, 835.0))],
            gate: ((590.0, 316.0), (549.0, 340.0)),
            ramp_len: path_len(),
            art: None,
            lamp_defs: Vec::new(),
            intro: None,
            frame: 0,
            new_high: false,
        };
        p.new_order();
        p
    }

    fn new_game(&mut self) {
        self.state = State::Playing;
        self.score = 0;
        self.ball_no = 1;
        self.ball_count = 3;
        self.mult = 1;
        self.lanes = [false; 3];
        self.targets_down = [false; 4];
        self.targets_reset_at = 0;
        self.menus = 0;
        self.inlanes = [false; 2];
        self.special_lit = false;
        self.order_no = 0;
        self.served = 0;
        self.rush = false;
        self.dishes_this_ball = 0;
        self.extra_at = 200_000;
        self.new_high = false;
        self.balls = Vec::from([Ball::at_plunger()]);
        self.saver_given = false;
        self.new_order();
        self.say("Welcome in! Hold Space to launch");
    }

    /// A new ticket: three different dishes.
    fn new_order(&mut self) {
        let mut all = [0usize, 1, 2, 3, 4];
        super::shuffle(&mut all);
        self.order = [all[0], all[1], all[2]];
        self.order.sort_unstable();
        self.made = [0; 5];
        self.order_up = false;
        self.order_no += 1;
    }

    fn wanted(&self, dish: usize) -> bool {
        !self.order_up && self.order.contains(&dish) && self.made[dish] < DISHES[dish].2
    }

    /// A shot that makes (part of) a dish.
    fn cook(&mut self, dish: usize, amount: u32, sfx: &mut Sfx) {
        if !self.wanted(dish) {
            return;
        }
        self.made[dish] = (self.made[dish] + amount).min(DISHES[dish].2);
        if self.made[dish] == DISHES[dish].2 {
            self.dishes_this_ball += 1;
            self.add(1000, sfx);
            if self.order.iter().all(|&d| self.made[d] >= DISHES[d].2) {
                self.order_up = true;
                self.say("Order up! Shoot the kitchen");
                sfx.play(Sound::Bell);
            } else {
                let m = format!("{} ready", DISHES[dish].0);
                self.say(&m);
            }
        }
    }

    /// Show a message on the display, long enough to scroll all the way through.
    fn say(&mut self, s: &str) {
        let scroll = (s.chars().count() as u64 * 6 + 84) * 3 + 40;
        self.message = Some((String::from(s), self.now, self.now + scroll.max(300)));
    }

    fn add(&mut self, points: u32, sfx: &mut Sfx) {
        self.score = self.score.saturating_add(points.saturating_mul(self.mult));
        if self.score >= self.extra_at {
            self.extra_at = self.extra_at.saturating_mul(3);
            self.ball_count += 1;
            self.say("Extra ball!");
            sfx.play(Sound::Win);
        }
    }

    fn launch(&mut self, sfx: &mut Sfx) {
        if self.state != State::Playing {
            self.new_game();
            return;
        }
        let pull = self.pull;
        let mut launched = false;
        for b in self.balls.iter_mut() {
            let near_rest = b.x > 545.0 && b.y > 880.0 && b.vy.abs() < 60.0 && b.ramp.is_none();
            if near_rest {
                b.vy = -(1100.0 + 1300.0 * pull);
                b.vx = 0.0;
                if !b.in_play {
                    b.in_play = true;
                    launched = true;
                }
                sfx.play(Sound::Launch);
                break;
            }
        }
        if launched && !self.rush && !self.saver_given {
            self.saver_until = self.now + SAVER_TICKS;
            self.saver_given = true;
        }
        self.pull = 0.0;
    }

    /// A ball straight out of the plunger lane (multiball, or saved during it).
    fn auto_launch(&mut self) {
        let mut b = Ball::at_plunger();
        b.y = REST.1 - 20.0 * self.balls.len() as f32;
        b.vy = -2100.0;
        b.in_play = true;
        self.balls.push(b);
    }

    // ---- physics ---------------------------------------------------------------------------

    fn step(&mut self, dt: f32, sfx: &mut Sfx) {
        for (k, f) in self.flippers.iter_mut().enumerate() {
            let target = if self.pressed[k] { f.up } else { f.rest };
            let speed = if self.pressed[k] { 22.0 } else { 14.0 };
            let diff = target - f.angle;
            let max = speed * dt;
            let d = diff.clamp(-max, max);
            f.angle += d;
            f.omega = d / dt;
        }
        for i in 0..self.balls.len() {
            let mut b = self.balls[i];
            self.move_ball(&mut b, dt, sfx);
            self.balls[i] = b;
        }
        // Balls bounce off each other.
        let n = self.balls.len();
        for i in 0..n {
            for j in i + 1..n {
                let (a, b) = (self.balls[i], self.balls[j]);
                if a.ramp.is_some() || b.ramp.is_some() || self.now < a.held_until || self.now < b.held_until {
                    continue;
                }
                let (dx, dy) = (b.x - a.x, b.y - a.y);
                let d = libm::sqrtf(dx * dx + dy * dy);
                if d < 2.0 * BALL_R && d > 0.01 {
                    let (nx, ny) = (dx / d, dy / d);
                    let push = (2.0 * BALL_R - d) / 2.0;
                    let rel = (b.vx - a.vx) * nx + (b.vy - a.vy) * ny;
                    let imp = if rel < 0.0 { -(1.0 + 0.9) * rel / 2.0 } else { 0.0 };
                    let bi = &mut self.balls[i];
                    bi.x -= nx * push;
                    bi.y -= ny * push;
                    bi.vx -= imp * nx;
                    bi.vy -= imp * ny;
                    let bj = &mut self.balls[j];
                    bj.x += nx * push;
                    bj.y += ny * push;
                    bj.vx += imp * nx;
                    bj.vy += imp * ny;
                }
            }
        }
    }

    fn move_ball(&mut self, b: &mut Ball, dt: f32, sfx: &mut Sfx) {
        if self.now < b.held_until {
            return;
        }
        if b.held_until != 0 && self.now >= b.held_until + 30 {
            b.held_until = 0;
        }
        // Riding the ramp: along its path, then down the right inlane.
        if let Some(d) = b.ramp {
            let d = d + RAMP_SPEED * dt;
            if d >= self.ramp_len {
                let end = RAMP[RAMP.len() - 1];
                b.ramp = None;
                b.x = end.0;
                b.y = end.1;
                b.vx = 0.0;
                b.vy = 260.0;
            } else {
                b.ramp = Some(d);
                (b.x, b.y) = ramp_point(d);
            }
            return;
        }
        let py = b.y;
        b.vy += GRAVITY * dt;
        let sp = libm::sqrtf(b.vx * b.vx + b.vy * b.vy);
        if sp > 3200.0 {
            b.vx *= 3200.0 / sp;
            b.vy *= 3200.0 / sp;
        }
        b.x += b.vx * dt;
        b.y += b.vy * dt;
        for i in 0..self.walls.len() {
            let (ax, ay, bx, by) = self.walls[i];
            collide(b, ax, ay, bx, by, 0.45, 0.0);
        }
        // The plunger lane's gate only stops balls coming down.
        if b.vy > 0.0 {
            let ((ax, ay), (bx, by)) = self.gate;
            collide(b, ax, ay, bx, by, 0.3, 0.0);
        }
        // M-E-N-U drop targets.
        for k in 0..4 {
            if !self.targets_down[k] {
                let y = TARGET_Y[k];
                if collide(b, TARGET_X, y - 18.0, TARGET_X, y + 18.0, 0.5, 0.0) {
                    self.targets_down[k] = true;
                    self.flash[3] = 15;
                    sfx.play(Sound::Drop);
                    self.add(750, sfx);
                    self.cook(3, 1, sfx);
                    if self.targets_down.iter().all(|&d| d) {
                        self.targets_reset_at = self.now + 150;
                        self.menus += 1;
                        let value = 5000 * self.menus.min(5);
                        self.add(value, sfx);
                        if self.special_lit {
                            let m = format!("MENU {}", group(value * self.mult));
                            self.say(&m);
                        } else {
                            self.special_lit = true;
                            self.say("MENU complete! Daily special at the kitchen");
                        }
                    }
                }
            }
        }
        // Slingshots: the slanted faces kick.
        for k in 0..2 {
            let ((ax, ay), (bx, by)) = self.slings[k];
            if collide(b, ax, ay, bx, by, 0.4, 520.0) {
                self.flash[if k == 0 { 4 } else { 6 }] = 10;
                sfx.play(Sound::Bump);
                self.add(10, sfx);
            }
        }
        // Pop bumpers: on the grill.
        for (k, &(bx, by)) in BUMPERS.iter().enumerate() {
            let (dx, dy) = (b.x - bx, b.y - by);
            let d = libm::sqrtf(dx * dx + dy * dy);
            if d < BUMPER_R + BALL_R && d > 0.01 {
                let (nx, ny) = (dx / d, dy / d);
                b.x = bx + nx * (BUMPER_R + BALL_R);
                b.y = by + ny * (BUMPER_R + BALL_R);
                let vn = b.vx * nx + b.vy * ny;
                if vn < 0.0 {
                    b.vx -= 2.0 * vn * nx;
                    b.vy -= 2.0 * vn * ny;
                }
                let out = b.vx * nx + b.vy * ny;
                let boost = (700.0 - out).max(250.0);
                b.vx += nx * boost;
                b.vy += ny * boost;
                self.flash[k] = 12;
                sfx.play(Sound::Bump);
                self.add(100, sfx);
                self.cook(0, 1, sfx);
            }
        }
        // Flippers (moving surfaces).
        for k in 0..2 {
            let f = &self.flippers[k];
            let (tx, ty) = f.tip();
            let (cx, cy, d) = closest(b.x, b.y, f.px, f.py, tx, ty);
            if d < BALL_R + FLIP_R && d > 0.01 {
                let (nx, ny) = ((b.x - cx) / d, (b.y - cy) / d);
                b.x = cx + nx * (BALL_R + FLIP_R);
                b.y = cy + ny * (BALL_R + FLIP_R);
                let (sx, sy) = (-f.omega * (cy - f.py), f.omega * (cx - f.px));
                let (rx, ry) = (b.vx - sx, b.vy - sy);
                let vn = rx * nx + ry * ny;
                if vn < 0.0 {
                    let e = 0.3;
                    b.vx = sx + rx - (1.0 + e) * vn * nx;
                    b.vy = sy + ry - (1.0 + e) * vn * ny;
                }
            }
        }
        // E-A-T rollover lanes (going down through them).
        if b.vy > 0.0 && (118.0..132.0).contains(&b.y) {
            for k in 0..3 {
                if b.x > POSTS[k] && b.x < POSTS[k + 1] && !self.lanes[k] {
                    self.lanes[k] = true;
                    sfx.play(Sound::Click);
                    self.add(500, sfx);
                    self.cook(4, 1, sfx);
                    if self.lanes.iter().all(|&l| l) {
                        self.lanes = [false; 3];
                        self.mult = (self.mult + 1).min(5);
                        self.add(2000, sfx);
                        let m = format!("E-A-T! Multiplier x{}", self.mult);
                        self.say(&m);
                    }
                }
            }
        }
        // The inlanes: rolling down through them lights them; both lit is worth 5,000.
        if b.vy > 0.0 && py < 715.0 && b.y >= 715.0 {
            let lane = if b.x > 78.0 && b.x < 118.0 { Some(0) } else if b.x > 457.0 && b.x < 497.0 { Some(1) } else { None };
            if let Some(k) = lane {
                sfx.play(Sound::Click);
                self.add(500, sfx);
                self.inlanes[k] = true;
                if self.inlanes == [true, true] {
                    self.inlanes = [false, false];
                    self.add(5000, sfx);
                    self.say("Inlanes! 5,000");
                }
            }
        }
        // The fries spinner: passing through spins it.
        if (py - SPINNER.2) * (b.y - SPINNER.2) < 0.0 && b.x > SPINNER.0 && b.x < SPINNER.1 {
            let spins = ((b.vy.abs() / 140.0) as u32).clamp(1, 15);
            self.spin_vel = spins as f32 * 0.09;
            self.flash[5] = 20;
            sfx.play(Sound::Spin);
            self.add(100 * spins, sfx);
            self.cook(1, spins, sfx);
        }
        // The shake ramp: a strong enough shot rides it; a weak one rolls back.
        if py >= MOUTH.2 && b.y < MOUTH.2 && b.vy < 0.0 && b.x > MOUTH.0 && b.x < MOUTH.1 {
            if b.vy < -650.0 {
                b.ramp = Some(0.0);
                sfx.play(Sound::Launch);
                if self.rush {
                    self.add(25_000, sfx);
                    self.say("JACKPOT!");
                    sfx.play(Sound::Win);
                } else {
                    self.add(5000, sfx);
                }
                self.cook(2, 1, sfx);
            } else {
                b.y = MOUTH.2 + 1.0;
                b.vy = -b.vy * 0.35;
            }
        }
        // The kitchen window.
        let (hx, hy) = (b.x - SAUCER.0, b.y - SAUCER.1);
        if hx * hx + hy * hy < 16.0 * 16.0 && b.held_until == 0 {
            b.x = SAUCER.0;
            b.y = SAUCER.1;
            b.vx = 220.0 + rand_below(140) as f32;
            b.vy = 380.0;
            b.held_until = self.now + 110;
            self.kitchen(sfx);
        }
    }

    /// A ball in the kitchen window: serve the order, the daily special, or just a visit.
    fn kitchen(&mut self, sfx: &mut Sfx) {
        let mut said = false;
        if self.order_up {
            self.served += 1;
            let value = 10_000 + 5_000 * (self.served - 1);
            self.add(value, sfx);
            sfx.play(Sound::Bell);
            let m = format!("Order {} served! {}", self.order_no, group(value * self.mult));
            self.say(&m);
            said = true;
            self.new_order();
            if self.served % 3 == 0 && !self.rush {
                self.rush = true;
                self.auto_launch();
                self.saver_until = self.now + 800;
                self.say("LUNCH RUSH! Shake ramp = jackpot");
                sfx.play(Sound::Win);
            }
        }
        if self.special_lit {
            self.special_lit = false;
            self.add(25_000, sfx);
            if !said {
                self.say("Daily special! 25,000");
                sfx.play(Sound::Win);
                said = true;
            }
        }
        if !said {
            self.add(2500, sfx);
            self.say("Kitchen!");
            sfx.play(Sound::Click);
        }
    }

    /// Balls that went down the drain.
    fn drained(&mut self, n: usize, sfx: &mut Sfx) {
        if self.now < self.saver_until {
            for _ in 0..n {
                if self.rush || !self.balls.is_empty() {
                    self.auto_launch();
                } else {
                    self.balls.push(Ball::at_plunger());
                }
            }
            self.say("Ball saved!");
            return;
        }
        if self.balls.len() == 1 && self.rush {
            self.rush = false;
            self.say("Lunch rush is over");
        }
        if !self.balls.is_empty() {
            return;
        }
        sfx.play(Sound::Lost);
        self.rush = false;
        // End-of-ball bonus: 1,000 a dish, times the multiplier.
        let bonus = 1000 * self.dishes_this_ball * self.mult;
        self.score = self.score.saturating_add(bonus);
        self.dishes_this_ball = 0;
        self.mult = 1;
        if self.ball_no >= self.ball_count {
            self.state = State::Over;
            if self.score > self.high {
                self.high = self.score;
                self.new_high = true;
            }
            self.say(if self.new_high { "New high score!" } else { "Game over" });
            self.balls = Vec::from([Ball::at_plunger()]);
        } else {
            self.ball_no += 1;
            self.saver_given = false;
            self.balls.push(Ball::at_plunger());
            let m = if bonus > 0 { format!("Bonus {}. Ball {}", group(bonus), self.ball_no) } else { format!("Ball {}", self.ball_no) };
            self.say(&m);
        }
    }

    /// The window opened: play the opening (unless a game is going on).
    pub fn opened(&mut self, now: u64) {
        if self.state != State::Playing {
            self.intro = Some(intro::Intro::new(now));
        }
    }

    pub fn tick(&mut self, now: u64, focused: bool, sfx: &mut Sfx) -> bool {
        let elapsed = (now.saturating_sub(self.now)).min(3);
        self.now = now;
        if let Some(i) = self.intro.as_mut() {
            let t = i.time(now);
            if t >= 0.55 && i.sounds == 0 {
                i.sounds = 1;
                sfx.play(Sound::Bell);
            }
            if t >= intro::T_MOVE && i.sounds == 1 {
                i.sounds = 2;
                sfx.play(Sound::Launch);
            }
            if t >= intro::T_END {
                self.intro = None;
            }
            self.frame += 1;
            return true;
        }
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
        self.spin_angle += self.spin_vel * elapsed as f32 * 6.0;
        self.spin_vel *= libm::powf(0.96, elapsed as f32);
        if self.targets_reset_at != 0 && now >= self.targets_reset_at {
            self.targets_reset_at = 0;
            self.targets_down = [false; 4];
            sfx.play(Sound::Click);
        }
        if self.state == State::Playing {
            let dt = 0.01 / STEPS as f32;
            for _ in 0..elapsed as usize * STEPS {
                self.step(dt, sfx);
            }
            let before = self.balls.len();
            self.balls.retain(|b| b.y <= 1010.0);
            let gone = before - self.balls.len();
            if gone > 0 {
                self.drained(gone, sfx);
            }
            // A ball stuck somewhere gets a nudge.
            let pivots = [(self.flippers[0].px, self.flippers[0].py), (self.flippers[1].px, self.flippers[1].py)];
            for b in self.balls.iter_mut() {
                // A ball held on a raised flipper isn't stuck: the player is aiming.
                let on_flipper = pivots.iter().any(|&(fx, fy)| (b.x - fx) * (b.x - fx) + (b.y - fy) * (b.y - fy) < (FLIP_LEN + 25.0) * (FLIP_LEN + 25.0));
                let slow = b.vx.abs() + b.vy.abs() < 25.0 && b.in_play && b.y < 900.0 && b.held_until == 0 && b.ramp.is_none() && !on_flipper;
                if !slow {
                    b.slow_since = now;
                } else if now > b.slow_since + 300 {
                    b.vy = -500.0;
                    b.vx = rand_below(400) as f32 - 200.0;
                    b.slow_since = now;
                }
            }
        }
        self.frame += 1;
        self.state == State::Playing || self.pulling || self.flash.iter().any(|&f| f > 0) || self.spin_vel > 0.01 || self.frame % 5 == 0
    }

    // ---- drawing -------------------------------------------------------------------------------

    fn scale(r: Rect) -> (f32, i32, i32) {
        let s = ((r.h - 10) as f32 / 1000.0).min((r.w - 330) as f32 / 620.0).max(0.2);
        let tw = (620.0 * s) as i32;
        let x0 = r.x + ((r.w - 320 - tw) / 2).max(6);
        (s, x0, r.y + 5)
    }

    fn ensure_art(&mut self, s: f32) {
        let key = (s * 1000.0) as i32;
        if self.art.as_ref().is_none_or(|a| a.key != key) {
            self.art = Some(art::Art::build(s));
            self.lamp_defs = art::lamps();
        }
    }

    /// Which lamps are lit now: from the game, or a chase across the table between games.
    fn lamp_states(&self) -> [bool; art::L_COUNT] {
        let mut on = [false; art::L_COUNT];
        if self.state != State::Playing {
            let f = (self.frame / 5) as usize;
            for (i, l) in on.iter_mut().enumerate() {
                *l = (i * 7 + f) % 9 < 2;
            }
            return on;
        }
        let blink = (self.frame / 12) % 2 == 0;
        let fast = (self.frame / 6) % 2 == 0;
        for k in 0..3 {
            on[art::L_LANES + k] = self.lanes[k];
        }
        on[art::L_COFFEE] = self.wanted(4) && blink;
        on[art::L_ORDER] = self.order_up && fast;
        on[art::L_SPECIAL] = self.special_lit && blink;
        for k in 0..3 {
            on[art::L_SERVED + k] = self.rush || (self.served % 3) as usize > k;
        }
        on[art::L_FRIES] = self.wanted(1) && blink;
        for k in 0..4 {
            on[art::L_MENU + k] = self.targets_down[k];
        }
        on[art::L_PIE] = self.wanted(3) && blink;
        on[art::L_GRILL] = self.wanted(0) && blink;
        on[art::L_SHAKE] = (self.wanted(2) || self.rush) && blink;
        on[art::L_JACKPOT] = self.rush && fast;
        for d in 0..5 {
            let in_order = self.order.contains(&d);
            let done = self.made[d] >= DISHES[d].2;
            on[art::L_DISH + d] = in_order && if self.order_up { fast } else { done || blink };
        }
        let need: u32 = self.order.iter().map(|&d| DISHES[d].2).sum();
        let got: u32 = self.order.iter().map(|&d| self.made[d].min(DISHES[d].2)).sum();
        let ring = (got * 12 / need.max(1)) as usize;
        for k in 0..12 {
            on[art::L_RING + k] = if self.order_up { (k + (self.frame / 4) as usize) % 3 == 0 } else { k < ring };
        }
        on[art::L_RUSH] = self.rush && blink;
        for k in 0..4 {
            on[art::L_MULT + k] = self.mult as usize >= k + 2;
        }
        on[art::L_INLANE] = self.inlanes[0];
        on[art::L_INLANE + 1] = self.inlanes[1];
        on[art::L_AGAIN] = self.now < self.saver_until && self.balls.iter().any(|b| b.in_play) && blink;
        let waiting = self.balls.iter().any(|b| !b.in_play);
        let step = ((self.frame / 8) % 4) as usize;
        for k in 0..3 {
            on[art::L_PLUNGER + k] = waiting && step == k;
        }
        on
    }

    /// The table at scale `s` with its top left at (x0, y0): the playfield, lamps, targets,
    /// bumpers, flippers, the balls, and the ramp over the balls beneath it.
    fn draw_table(&mut self, c: &mut Canvas, s: f32, x0: i32, y0: i32) {
        self.ensure_art(s);
        let on = self.lamp_states();
        let a = self.art.as_ref().unwrap();
        let blit = |c: &mut Canvas, sp: &art::Sprite, x: f32, y: f32| {
            c.draw_image(&sp.img, x0 + (x * s) as i32 - sp.ox, y0 + (y * s) as i32 - sp.oy);
        };
        c.draw_image(&a.table, x0, y0);
        for (i, d) in self.lamp_defs.iter().enumerate() {
            blit(c, &a.lamps[i][on[i] as usize], d.x, d.y);
        }
        if self.order_up && (self.frame / 6) % 2 == 0 {
            blit(c, &a.kitchen_glow, SAUCER.0, SAUCER.1);
        }
        for k in 0..4 {
            blit(c, &a.targets[k][self.targets_down[k] as usize], TARGET_X, TARGET_Y[k]);
        }
        art::spinner(c, s, x0, y0, self.spin_angle, self.flash[5] > 0);
        if self.flash[4] > 0 {
            blit(c, &a.sling_glow[0], 118.0, 712.0);
        }
        if self.flash[6] > 0 {
            blit(c, &a.sling_glow[1], 457.0, 712.0);
        }
        for (k, &(bx, by)) in BUMPERS.iter().enumerate() {
            blit(c, &a.bumpers[k][(self.flash[k] > 0) as usize], bx, by);
        }
        for (k, f) in self.flippers.iter().enumerate() {
            art::flipper(c, s, x0, y0, f.px, f.py, FLIP_LEN, f.angle, self.pressed[k]);
        }
        art::plunger(c, s, x0, y0, self.pull);
        for b in self.balls.iter().filter(|b| b.ramp.is_none()) {
            blit(c, &a.ball_shadow, b.x + 3.0, b.y + 5.0);
            blit(c, &a.ball, b.x, b.y);
        }
        for b in self.balls.iter().filter(|b| b.ramp.is_some()) {
            blit(c, &a.ball_shadow, b.x + 10.0, b.y + 14.0);
        }
        c.draw_image(&a.ramp.img, x0 - a.ramp.ox, y0 - a.ramp.oy);
        for b in self.balls.iter().filter(|b| b.ramp.is_some()) {
            blit(c, &a.ball, b.x, b.y);
        }
    }

    pub fn draw(&mut self, c: &mut Canvas, r: Rect) {
        let (s, x0, y0) = Self::scale(r);
        if self.intro.is_some() {
            self.draw_intro(c, r, s, x0, y0);
            return;
        }
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(12, 9, 22));
        self.draw_table(c, s, x0, y0);
        self.draw_panel(c, r);
        if self.state != State::Playing {
            let (tx, ty) = (x0 + (288.0 * s) as i32, y0 + (560.0 * s) as i32);
            let w = 320;
            c.round_rect_alpha(tx - w / 2 - 3, ty - 49, w + 6, 98, 16, rgb(255, 80, 170), 230);
            c.round_rect_alpha(tx - w / 2, ty - 46, w, 92, 14, rgb(14, 10, 28), 235);
            let t = if self.state == State::Over { if self.new_high { "New high score!" } else { "Game over" } } else { "Neon Grill" };
            c.text_centered(tx + 1, ty - 33, t, Size::Large, true, rgb(140, 20, 80));
            c.text_centered(tx, ty - 34, t, Size::Large, true, rgb(255, 200, 230));
            c.text_centered(tx, ty + 12, "Press Space or Start to play", Size::Small, false, rgb(190, 240, 235));
        }
    }

    /// The opening: the 3D machine, its playfield showing the table as the game will draw it,
    /// fading into the game at the end.
    fn draw_intro(&mut self, c: &mut Canvas, r: Rect, s: f32, x0: i32, y0: i32) {
        let Some(mut intro) = self.intro.take() else { return };
        // The first frame: the table's artwork and the cabinet's, then the clock starts.
        if !intro.started() {
            self.ensure_art(s);
            intro.prepare();
            self.now = intro.start;
        }
        let now = self.now;
        // The playfield: the table drawn as the game draws it (lamps chasing), refreshed now
        // and then.
        if self.frame % 4 == 0 || !intro.has_field() {
            let (tw, th) = ((620.0 * s) as usize + 1, (1000.0 * s) as usize + 1);
            let mut tc = Canvas::new(tw, th);
            self.draw_table(&mut tc, s, 0, 0);
            intro.set_field(crate::image::Image { w: tw, h: th, px: tc.px.iter().map(|p| p | 0xff00_0000).collect(), opaque: true });
        }
        let alpha = intro.alpha(now);
        if alpha < 1.0 {
            c.fill_rect(r.x, r.y, r.w, r.h, rgb(12, 9, 22));
            self.draw_table(c, s, x0, y0);
            self.draw_panel(c, r);
        }
        let (frame, k) = intro.render(now, r.w.max(1) as usize, r.h.max(1) as usize, s, (x0 - r.x) as f32, (y0 - r.y) as f32);
        blit_scaled(c, r, frame, k, (alpha * 255.0) as u32);
        if alpha >= 1.0 {
            c.text(r.x + 16, r.y + r.h - 28, "Press Space or click to skip", Size::Small, false, rgb(150, 140, 170));
        }
        self.intro = Some(intro);
    }

    /// The side panel: a dot-matrix display in a chrome bezel, ball and multiplier, the order
    /// ticket, the launch button and how to play.
    fn draw_panel(&self, c: &mut Canvas, r: Rect) {
        let px = r.x + r.w - 310;
        let (top, bottom) = (r.y + 8, r.y + r.h - 8);
        c.round_rect(px - 2, top - 2, 304, bottom - top + 4, 15, rgb(255, 70, 160));
        c.round_rect(px, top, 300, bottom - top, 13, rgb(12, 10, 28));
        c.round_rect(px + 4, top + 4, 292, bottom - top - 8, 10, rgb(20, 18, 42));
        // Neon header.
        let hx = px + 150;
        for (dx, dy) in [(-2, 0), (2, 0), (0, -2), (0, 2)] {
            c.text_centered(hx + dx, top + 12 + dy, "NEON GRILL", Size::Medium, true, rgb(150, 20, 90));
        }
        c.text_centered(hx, top + 12, "NEON GRILL", Size::Medium, true, rgb(255, 205, 232));
        // Display: chrome bezel, black glass, amber dots.
        let (dx, dy) = (px + 12, top + 42);
        c.round_rect(dx - 5, dy - 5, 286, 96, 9, rgb(170, 176, 190));
        c.round_rect(dx - 3, dy - 3, 282, 92, 8, rgb(70, 74, 88));
        c.round_rect(dx, dy, 276, 86, 6, rgb(8, 6, 6));
        let amber = rgb(255, 150, 40);
        let dim = rgb(52, 26, 10);
        dot_text(c, dx + 6, dy + 8, &group(self.score), 4, 11, amber, dim, 0);
        let (msg, since) = match &self.message {
            Some((m, start, until)) if self.now < *until => (m.clone(), *start),
            _ if self.state == State::Over => (String::from(if self.new_high { "New high score!" } else { "Game over" }), 0),
            _ if self.state == State::Ready => (String::from("Neon Grill"), 0),
            _ => (format!("Order {}   x{}", self.order_no, self.mult), 0),
        };
        let scroll = if since > 0 { (self.now.saturating_sub(since) / 3) as usize } else { (self.frame / 3) as usize };
        dot_text(c, dx + 9, dy + 52, &msg, 3, 14, amber, dim, scroll);
        // Ball and multiplier.
        let by = dy + 102;
        let boxes = [("BALL", if self.state == State::Over { String::from("-") } else { format!("{}", self.ball_no) }, rgb(255, 80, 170)), ("MULTIPLIER", format!("x{}", self.mult), rgb(70, 220, 240))];
        for (i, (label, value, col)) in boxes.iter().enumerate() {
            let bx = px + 12 + i as i32 * 142;
            c.round_rect(bx, by, 134, 56, 9, *col);
            c.round_rect(bx + 2, by + 2, 130, 52, 8, rgb(14, 12, 30));
            c.text(bx + 10, by + 6, label, Size::Small, true, *col);
            c.text(bx + 10, by + 22, value, Size::Large, true, rgb(255, 255, 255));
        }
        let mut y = by + 64;
        c.text(px + 14, y, &format!("High score {}", group(self.high)), Size::Small, false, rgb(200, 190, 230));
        if self.now < self.saver_until && self.balls.iter().any(|b| b.in_play) {
            c.text(px + 196, y, "Saver on", Size::Small, true, rgb(120, 240, 190));
        }
        y += 24;
        // The order ticket.
        c.round_rect(px + 12, y + 2, 276, 116, 4, rgb(10, 8, 20));
        c.round_rect(px + 12, y, 276, 116, 4, rgb(250, 246, 232));
        c.round_rect(px + 12, y, 276, 22, 4, rgb(200, 28, 48));
        c.fill_rect(px + 12, y + 16, 276, 6, rgb(200, 28, 48));
        c.text(px + 22, y + 3, &format!("ORDER #{}", self.order_no), Size::Small, true, rgb(255, 245, 235));
        c.text(px + 176, y + 3, &format!("Served {}", self.served), Size::Small, false, rgb(255, 225, 225));
        let ink = rgb(40, 40, 60);
        for (i, &d) in self.order.iter().enumerate() {
            let (name, shot, need, _) = DISHES[d];
            let yy = y + 28 + i as i32 * 26;
            let done = self.made[d] >= need;
            if let Some(a) = &self.art {
                let sp = &a.lamps[art::L_DISH + d][done as usize];
                c.draw_image(&sp.img, px + 32 - sp.img.w as i32 / 2, yy + 9 - sp.img.h as i32 / 2);
            }
            let t = if need > 1 { format!("{} {}/{}", name, self.made[d], need) } else { String::from(name) };
            c.text(px + 54, yy, &t, Size::Small, true, if done { rgb(30, 140, 70) } else { ink });
            c.text(px + 172, yy, shot, Size::Small, false, rgb(120, 110, 100));
        }
        if self.order_up {
            c.text(px + 22, y + 98, "Order up! Shoot the kitchen", Size::Small, true, rgb(200, 28, 48));
        }
        // Launch button and plunger power.
        let lb = Self::launch_button(r);
        c.round_rect(lb.x - 2, lb.y - 2, lb.w + 4, lb.h + 4, 22, rgb(255, 120, 190));
        for k in 0..lb.h {
            let t = (k * 255 / lb.h.max(1)) as u32;
            c.fill_rect(lb.x + 4, lb.y + k, lb.w - 8, 1, crate::gfx::mix(rgb(255, 80, 160), rgb(170, 20, 90), t));
        }
        c.round_rect_alpha(lb.x, lb.y, 20, lb.h, 20, rgb(255, 80, 160), 255);
        c.round_rect_alpha(lb.x + lb.w - 20, lb.y, 20, lb.h, 20, rgb(170, 20, 90), 255);
        c.text_centered(lb.x + lb.w / 2, lb.y + (lb.h - 20) / 2, if self.state == State::Playing { "LAUNCH" } else { "START" }, Size::Medium, true, rgb(255, 255, 255));
        c.round_rect(lb.x, lb.y + lb.h + 8, lb.w, 10, 4, rgb(50, 30, 60));
        c.round_rect(lb.x, lb.y + lb.h + 8, (lb.w as f32 * self.pull) as i32 + 4, 10, 4, rgb(255, 170, 60));
        let help = [
            "How to play",
            "Flippers: Shift keys, Z and /, arrows",
            "Launch: hold Space, let go",
            "Make the 3 dishes on the ticket,",
            "then shoot the kitchen to serve.",
            "3 orders: lunch rush multiball",
            "E-A-T lanes: multiplier up",
        ];
        for (k, h) in help.iter().enumerate() {
            let yy = lb.y + lb.h + 28 + k as i32 * 19;
            if yy + 18 > bottom - 6 {
                break;
            }
            c.text(px + 14, yy, h, Size::Small, k == 0, if k == 0 { rgb(255, 190, 225) } else { rgb(190, 185, 215) });
        }
    }

    fn launch_button(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 290, r.y + 8 + 372, 260, 40)
    }

    // ---- input ---------------------------------------------------------------------------------

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, sfx: &mut Sfx) -> GameAction {
        if self.intro.take().is_some() {
            return GameAction::Redraw;
        }
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
            // Lane change: the lit lanes move with the flippers.
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
        if self.intro.is_some() {
            // Any key (pressed, not released) skips the opening.
            if sc & 0x80 == 0 {
                self.intro = None;
            }
            return Some(GameAction::Redraw);
        }
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

/// Copy a picture drawn at `k` times the size of `r` into `r` (bilinear when scaled), at
/// opacity `alpha`.
fn blit_scaled(c: &mut Canvas, r: Rect, src: &Canvas, k: f32, alpha: u32) {
    let step = ((1.0 / k) * 65536.0) as u32; // source pixels per target pixel, 16.16
    let inv = (k * 65536.0) as u32;
    let (sw, sh) = (src.w, src.h);
    for yy in 0..r.h.max(0) {
        let ty = r.y + yy;
        if ty < 0 || ty as usize >= c.h {
            continue;
        }
        let sy = ((yy as u32 * inv) as u64).saturating_sub(32768) as u32;
        let (y0, fy) = (((sy >> 16) as usize).min(sh - 1), (sy >> 8) & 0xff);
        let y1 = (y0 + 1).min(sh - 1);
        let row = ty as usize * c.w;
        for xx in 0..r.w.max(0) {
            let tx = r.x + xx;
            if tx < 0 || tx as usize >= c.w {
                continue;
            }
            let p = if k >= 0.999 {
                src.px[(yy as usize).min(sh - 1) * sw + (xx as usize).min(sw - 1)]
            } else {
                let sx = ((xx as u32 * inv) as u64).saturating_sub(32768) as u32;
                let (x0, fx) = (((sx >> 16) as usize).min(sw - 1), (sx >> 8) & 0xff);
                let x1 = (x0 + 1).min(sw - 1);
                let (a, b, cc, d) = (src.px[y0 * sw + x0], src.px[y0 * sw + x1], src.px[y1 * sw + x0], src.px[y1 * sw + x1]);
                let m = |s: u32| {
                    let top = ((a >> s) & 0xff) * (256 - fx) + ((b >> s) & 0xff) * fx;
                    let bot = ((cc >> s) & 0xff) * (256 - fx) + ((d >> s) & 0xff) * fx;
                    ((top * (256 - fy) + bot * fy) >> 16) & 0xff
                };
                m(16) << 16 | m(8) << 8 | m(0)
            };
            if alpha >= 255 {
                c.px[row + tx as usize] = p;
            } else {
                c.blend(tx, ty, p, alpha);
            }
        }
    }
    let _ = step;
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

/// 5x7 dot-matrix glyphs (rows top to bottom, bit 4 = leftmost).
fn glyph(ch: char) -> [u8; 7] {
    match ch.to_ascii_uppercase() {
        '0' => [0x0E, 0x11, 0x13, 0x15, 0x19, 0x11, 0x0E],
        '1' => [0x04, 0x0C, 0x04, 0x04, 0x04, 0x04, 0x0E],
        '2' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x08, 0x1F],
        '3' => [0x1F, 0x02, 0x04, 0x02, 0x01, 0x11, 0x0E],
        '4' => [0x02, 0x06, 0x0A, 0x12, 0x1F, 0x02, 0x02],
        '5' => [0x1F, 0x10, 0x1E, 0x01, 0x01, 0x11, 0x0E],
        '6' => [0x06, 0x08, 0x10, 0x1E, 0x11, 0x11, 0x0E],
        '7' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x08, 0x08],
        '8' => [0x0E, 0x11, 0x11, 0x0E, 0x11, 0x11, 0x0E],
        '9' => [0x0E, 0x11, 0x11, 0x0F, 0x01, 0x02, 0x0C],
        'A' => [0x0E, 0x11, 0x11, 0x11, 0x1F, 0x11, 0x11],
        'B' => [0x1E, 0x11, 0x11, 0x1E, 0x11, 0x11, 0x1E],
        'C' => [0x0E, 0x11, 0x10, 0x10, 0x10, 0x11, 0x0E],
        'D' => [0x1C, 0x12, 0x11, 0x11, 0x11, 0x12, 0x1C],
        'E' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x1F],
        'F' => [0x1F, 0x10, 0x10, 0x1E, 0x10, 0x10, 0x10],
        'G' => [0x0E, 0x11, 0x10, 0x17, 0x11, 0x11, 0x0F],
        'H' => [0x11, 0x11, 0x11, 0x1F, 0x11, 0x11, 0x11],
        'I' => [0x0E, 0x04, 0x04, 0x04, 0x04, 0x04, 0x0E],
        'J' => [0x07, 0x02, 0x02, 0x02, 0x02, 0x12, 0x0C],
        'K' => [0x11, 0x12, 0x14, 0x18, 0x14, 0x12, 0x11],
        'L' => [0x10, 0x10, 0x10, 0x10, 0x10, 0x10, 0x1F],
        'M' => [0x11, 0x1B, 0x15, 0x15, 0x11, 0x11, 0x11],
        'N' => [0x11, 0x11, 0x19, 0x15, 0x13, 0x11, 0x11],
        'O' => [0x0E, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'P' => [0x1E, 0x11, 0x11, 0x1E, 0x10, 0x10, 0x10],
        'Q' => [0x0E, 0x11, 0x11, 0x11, 0x15, 0x12, 0x0D],
        'R' => [0x1E, 0x11, 0x11, 0x1E, 0x14, 0x12, 0x11],
        'S' => [0x0F, 0x10, 0x10, 0x0E, 0x01, 0x01, 0x1E],
        'T' => [0x1F, 0x04, 0x04, 0x04, 0x04, 0x04, 0x04],
        'U' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x11, 0x0E],
        'V' => [0x11, 0x11, 0x11, 0x11, 0x11, 0x0A, 0x04],
        'W' => [0x11, 0x11, 0x11, 0x15, 0x15, 0x15, 0x0A],
        'X' => [0x11, 0x11, 0x0A, 0x04, 0x0A, 0x11, 0x11],
        'Y' => [0x11, 0x11, 0x11, 0x0A, 0x04, 0x04, 0x04],
        'Z' => [0x1F, 0x01, 0x02, 0x04, 0x08, 0x10, 0x1F],
        '!' => [0x04, 0x04, 0x04, 0x04, 0x04, 0x00, 0x04],
        '?' => [0x0E, 0x11, 0x01, 0x02, 0x04, 0x00, 0x04],
        ',' => [0x00, 0x00, 0x00, 0x00, 0x0C, 0x04, 0x08],
        '.' => [0x00, 0x00, 0x00, 0x00, 0x00, 0x0C, 0x0C],
        '-' => [0x00, 0x00, 0x00, 0x1F, 0x00, 0x00, 0x00],
        ':' => [0x00, 0x0C, 0x0C, 0x00, 0x0C, 0x0C, 0x00],
        '\'' => [0x0C, 0x04, 0x08, 0x00, 0x00, 0x00, 0x00],
        '/' => [0x01, 0x01, 0x02, 0x04, 0x08, 0x10, 0x10],
        '#' => [0x0A, 0x0A, 0x1F, 0x0A, 0x1F, 0x0A, 0x0A],
        '=' => [0x00, 0x00, 0x1F, 0x00, 0x1F, 0x00, 0x00],
        _ => [0; 7],
    }
}

/// Text on a dot-matrix display `cols` characters wide, `pitch` pixels a dot: centred if it
/// fits, otherwise scrolling right to left (`scroll` advances it), with the unlit dots dimly
/// visible.
fn dot_text(c: &mut Canvas, x: i32, y: i32, text: &str, pitch: i32, cols: usize, on: u32, off: u32, scroll: usize) {
    let chars: Vec<char> = text.chars().collect();
    let width = cols * 6; // dot columns
    let text_w = chars.len() * 6;
    let lit = |col: isize| -> [bool; 7] {
        // Dot column `col` of the laid-out text.
        let mut out = [false; 7];
        if col < 0 || col as usize >= text_w {
            return out;
        }
        let (ci, cx) = (col as usize / 6, col as usize % 6);
        if cx == 5 {
            return out;
        }
        let g = glyph(chars[ci]);
        for (row, bits) in g.iter().enumerate() {
            out[row] = bits & (0x10 >> cx) != 0;
        }
        out
    };
    let start: isize = if text_w <= width {
        -(((width - text_w) / 2) as isize)
    } else {
        // Scroll the text in from the right and out to the left, then again.
        let period = text_w + width;
        (scroll % period) as isize - width as isize
    };
    let d = (pitch - 1).max(1);
    for col in 0..width {
        let dots = lit(start + col as isize);
        for (row, &is_on) in dots.iter().enumerate() {
            c.fill_rect(x + col as i32 * pitch, y + row as i32 * pitch, d, d, if is_on { on } else { off });
        }
    }
}

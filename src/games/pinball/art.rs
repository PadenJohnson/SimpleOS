//! The Neon Grill table's artwork. The playfield is drawn once for each size at twice the
//! resolution and shrunk (smooth edges), with soft shadows under the rails, plastics and the
//! ramp; sprites cover what changes: lamps (lit and unlit), the food bumpers, the drop targets,
//! the ball, the ramp (see-through plastic, drawn over the balls beneath it) and glows. The
//! flippers are drawn each frame.

use alloc::vec::Vec;

use super::{BALL_R, BUMPER_R, MOUTH, POSTS, RAMP, SAUCER, SPINNER, TARGET_Y};
use crate::font::Face;
use crate::games::pinart::{chrome, lighten, mixc, scale, sphere, Layer};
use crate::gfx::Canvas;
use crate::image::Image;

/// Supersampling for the playfield and for sprites.
const SS: usize = 2;
const SSP: usize = 3;

/// The plate in the middle, where the order's dishes light up.
pub(super) const PLATE: (f32, f32) = (255.0, 605.0);

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Shape {
    Circle(f32),
    /// Width, height.
    Pill(f32, f32),
    /// Length, width, direction in degrees (0 = up, 90 = right).
    Arrow(f32, f32, f32),
    /// Size, direction.
    Tri(f32, f32),
    Star(f32),
    /// A dish's picture in a round insert: dish, radius.
    Dish(usize, f32),
}

pub(super) struct LampDef {
    pub x: f32,
    pub y: f32,
    pub shape: Shape,
    pub color: u32,
    pub text: &'static str,
}

pub(super) const L_LANES: usize = 0; // E, A, T
pub(super) const L_COFFEE: usize = 3;
pub(super) const L_ORDER: usize = 4;
pub(super) const L_SPECIAL: usize = 5;
pub(super) const L_SERVED: usize = 6; // three
pub(super) const L_FRIES: usize = 9;
pub(super) const L_MENU: usize = 10; // four
pub(super) const L_PIE: usize = 14;
pub(super) const L_GRILL: usize = 15;
pub(super) const L_SHAKE: usize = 16;
pub(super) const L_JACKPOT: usize = 17;
pub(super) const L_DISH: usize = 18; // five, in dish order
pub(super) const L_RING: usize = 23; // twelve
pub(super) const L_RUSH: usize = 35;
pub(super) const L_MULT: usize = 36; // 2x to 5x
pub(super) const L_INLANE: usize = 40; // left, right
pub(super) const L_AGAIN: usize = 42;
pub(super) const L_PLUNGER: usize = 43; // three
pub(super) const L_COUNT: usize = 46;

const PINK: u32 = 0xff5aa8;
const YELLOW: u32 = 0xffd23c;
const ORANGE: u32 = 0xff8a2a;
const CYAN: u32 = 0x4ad8ff;
const RED: u32 = 0xff3a4a;
const WHITE: u32 = 0xfff4e0;
const GREEN: u32 = 0x5cff8a;
const MAGENTA: u32 = 0xff50e8;
/// The dishes' insert colours: burger, fries, shake, pie, coffee.
const DISH_COLORS: [u32; 5] = [0xffa53c, 0xffe24a, 0xff7ac8, 0xff5a5a, 0xe0a060];

fn same_look(a: &LampDef, b: &LampDef) -> bool {
    a.shape == b.shape && a.color == b.color && a.text == b.text
}

/// Every lamp on the table, by the indices above.
pub(super) fn lamps() -> Vec<LampDef> {
    let l = |x: f32, y: f32, shape: Shape, color: u32, text: &'static str| LampDef { x, y, shape, color, text };
    let mut v = Vec::new();
    for (k, t) in ["E", "A", "T"].iter().enumerate() {
        v.push(l((POSTS[k] + POSTS[k + 1]) / 2.0, 72.0, Shape::Circle(12.0), PINK, t));
    }
    v.push(l(290.0, 180.0, Shape::Arrow(40.0, 26.0, 0.0), YELLOW, ""));
    v.push(l(SAUCER.0, 290.0, Shape::Pill(76.0, 15.0), ORANGE, "ORDER UP"));
    v.push(l(206.0, 246.0, Shape::Star(10.0), CYAN, ""));
    for k in 0..3 {
        v.push(l(124.0 + 16.0 * k as f32, 309.0, Shape::Circle(5.0), RED, ""));
    }
    v.push(l(72.0, 414.0, Shape::Arrow(40.0, 24.0, 0.0), YELLOW, ""));
    for &y in TARGET_Y.iter() {
        v.push(l(82.0, y, Shape::Circle(6.0), WHITE, ""));
    }
    v.push(l(116.0, 515.0, Shape::Arrow(36.0, 22.0, -90.0), YELLOW, ""));
    v.push(l(288.0, 490.0, Shape::Arrow(36.0, 24.0, 0.0), YELLOW, ""));
    v.push(l(430.0, 676.0, Shape::Arrow(40.0, 26.0, 0.0), YELLOW, ""));
    v.push(l(430.0, 627.0, Shape::Pill(50.0, 12.0), MAGENTA, "JACKPOT"));
    for d in 0..5 {
        let a = (-90.0 + 72.0 * d as f32).to_radians();
        v.push(l(PLATE.0 + 40.0 * libm::cosf(a), PLATE.1 + 40.0 * libm::sinf(a), Shape::Dish(d, 15.0), DISH_COLORS[d], ""));
    }
    for k in 0..12 {
        let a = (-90.0 + 30.0 * k as f32).to_radians();
        v.push(l(PLATE.0 + 80.0 * libm::cosf(a), PLATE.1 + 80.0 * libm::sinf(a), Shape::Circle(4.5), YELLOW, ""));
    }
    v.push(l(255.0, 699.0, Shape::Pill(108.0, 16.0), RED, "LUNCH RUSH"));
    for (k, t) in ["2X", "3X", "4X", "5X"].iter().enumerate() {
        v.push(l(207.0 + 32.0 * k as f32, 740.0, Shape::Circle(11.0), ORANGE, t));
    }
    v.push(l(98.0, 708.0, Shape::Circle(6.0), GREEN, ""));
    v.push(l(477.0, 708.0, Shape::Circle(6.0), GREEN, ""));
    v.push(l(288.0, 940.0, Shape::Pill(64.0, 14.0), RED, "SHOOT AGAIN"));
    for k in 0..3 {
        v.push(l(567.0, 800.0 - 40.0 * k as f32, Shape::Tri(13.0, 0.0), ORANGE, ""));
    }
    debug_assert_eq!(v.len(), L_COUNT);
    v
}

/// A sprite and where its anchor is inside it (draw at anchor position minus this).
#[derive(Clone)]
pub(super) struct Sprite {
    pub img: Image,
    pub ox: i32,
    pub oy: i32,
}

pub(super) struct Art {
    pub key: i32,
    pub table: Image,
    pub lamps: Vec<[Sprite; 2]>,
    pub bumpers: Vec<[Sprite; 2]>,
    pub targets: Vec<[Sprite; 2]>,
    pub ball: Sprite,
    pub ball_shadow: Sprite,
    pub ramp: Sprite,
    pub kitchen_glow: Sprite,
    pub sling_glow: [Sprite; 2],
}

/// Rounded rectangle as a polygon.
pub(super) fn rrect(x0: f32, y0: f32, x1: f32, y1: f32, r: f32) -> Vec<(f32, f32)> {
    let r = r.min((x1 - x0) / 2.0).min((y1 - y0) / 2.0);
    let mut v = Vec::new();
    for (cx, cy, a0) in [(x1 - r, y0 + r, -90.0f32), (x1 - r, y1 - r, 0.0), (x0 + r, y1 - r, 90.0), (x0 + r, y0 + r, 180.0)] {
        for i in 0..=6 {
            let a = (a0 + 15.0 * i as f32).to_radians();
            v.push((cx + r * libm::cosf(a), cy + r * libm::sinf(a)));
        }
    }
    v
}

fn rotate(pts: &[(f32, f32)], deg: f32, cx: f32, cy: f32) -> Vec<(f32, f32)> {
    let (s, c) = (libm::sinf(deg.to_radians()), libm::cosf(deg.to_radians()));
    pts.iter().map(|&(x, y)| (cx + x * c - y * s, cy + x * s + y * c)).collect()
}

/// Chrome across a rail: `t` from -1 (the lit side) to 1.
pub(super) fn chrome_profile(t: f32) -> u32 {
    let x = t.clamp(-1.0, 1.0);
    if x < -0.55 {
        mixc(0x3c424e, 0xc8ced8, ((x + 1.0) / 0.45 * 255.0) as u32)
    } else if x < -0.2 {
        mixc(0xc8ced8, 0xffffff, ((x + 0.55) / 0.35 * 255.0) as u32)
    } else if x < 0.4 {
        mixc(0xffffff, 0x8a92a0, ((x + 0.2) / 0.6 * 255.0) as u32)
    } else {
        mixc(0x8a92a0, 0x2c313a, ((x - 0.4) / 0.6 * 255.0) as u32)
    }
}

/// A rail or guide along a segment, shaded across with the lit side towards the light.
fn rail(l: &mut Layer, a: (f32, f32), b: (f32, f32), r: f32, shade: impl Fn(f32) -> u32) {
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    // The left side's normal is (dy, -dx) in screen terms; is it towards the top-left light?
    let flip = if dy * -0.6 + -dx * -0.8 > 0.0 { -1.0 } else { 1.0 };
    l.capsule_with(a.0, a.1, b.0, b.1, r, |across, _| (shade(across * flip), 255));
}

/// White plastic with a red edge, shaded.
fn plastic(t: f32) -> u32 {
    let a = t.abs();
    if a > 0.62 {
        scale(0xd2202e, 1.15 - a * 0.35)
    } else {
        lighten(scale(0xf3f1ec, 0.92 - t * 0.12), (0.3 - (t + 0.25).abs()).max(0.0) * 1.4)
    }
}

impl Art {
    pub fn build(s: f32) -> Art {
        let t0 = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let lap = |what: &str| kprintln!("pinball: art {} at {} ms", what, (crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed) - t0) * 10);
        let key = (s * 1000.0) as i32;
        let (w, h) = ((620.0 * s) as usize + 1, (1000.0 * s) as usize + 1);
        let k = s * SS as f32;
        let p = |x: f32, y: f32| (x * k, y * k);
        let u = |v: f32| v * k;

        // ---- the flat artwork ----------------------------------------------------------------
        let mut base = Layer::new(w * SS, h * SS);
        // The cabinet's inside walls: dark, a little lighter towards the top.
        base.gradient_rows(|t| mixc(0x2a1420, 0x12070c, (t * 255.0) as u32));
        let field = playfield_outline();
        let fpts: Vec<(f32, f32)> = field.iter().map(|&(x, y)| p(x, y)).collect();
        base.poly_with(&fpts, |x, y| {
            let (tx, ty) = (x / k, y / k);
            // Midnight blue to deep teal.
            let t = (ty / 1000.0).clamp(0.0, 1.0);
            let mut c = if t < 0.5 { mixc(0x2a1d5a, 0x123d66, (t * 2.0 * 255.0) as u32) } else { mixc(0x123d66, 0x0a2238, ((t - 0.5) * 2.0 * 255.0) as u32) };
            // Diner tiles, faintly.
            let (a, b) = (((tx + ty) / 30.0) as i32, ((tx - ty + 2000.0) / 30.0) as i32);
            if (a + b) & 1 == 0 {
                c = lighten(c, 0.035);
            }
            // Glows behind the bumpers and the plate.
            let g1 = 1.0 - ((tx - 288.0) * (tx - 288.0) + (ty - 380.0) * (ty - 380.0)) / (210.0 * 210.0);
            if g1 > 0.0 {
                c = mixc(c, 0xb0306a, (g1 * g1 * 70.0) as u32);
            }
            let g2 = 1.0 - ((tx - PLATE.0) * (tx - PLATE.0) + (ty - PLATE.1) * (ty - PLATE.1)) / (150.0 * 150.0);
            if g2 > 0.0 {
                c = mixc(c, 0x1f9aa8, (g2 * g2 * 80.0) as u32);
            }
            (c, 255)
        });
        // Atomic starbursts and boomerangs, mid-century diner style.
        for &(x, y, r, c) in &[(122.0, 150.0, 34.0, 0xfff0c8), (500.0, 150.0, 28.0, 0x8ff0e0), (220.0, 800.0, 24.0, 0xffb0d0), (355.0, 800.0, 24.0, 0xfff0c8)] {
            starburst(&mut base, p(x, y), u(r), c, 110);
        }
        // Twinkles.
        for &(x, y, r, c) in &[(178.0, 425.0, 9.0, 0x8ff0e0), (398.0, 470.0, 7.0, 0xffb0d0), (468.0, 560.0, 8.0, 0xfff0c8), (150.0, 640.0, 6.0, 0xffb0d0), (372.0, 600.0, 7.0, 0x8ff0e0), (60.0, 250.0, 6.0, 0xfff0c8), (250.0, 255.0, 6.0, 0x8ff0e0), (520.0, 400.0, 6.0, 0xffb0d0)] {
            twinkle(&mut base, p(x, y), u(r), c);
        }
        // Diner counter stripes across the lower field.
        for &(y0, y1, c) in &[(754.0, 759.0, 0xc8ced8), (759.0, 766.0, 0xff5aa0), (768.0, 773.0, 0x7af0d4)] {
            let (a, b) = (p(30.0, y0), p(545.0, y1));
            base.poly(&[a, (b.0, a.1), b, (a.0, b.1)], c, 255);
        }
        // The checkerboard floor under the flippers.
        let sq = 34.0;
        let mut row = 0;
        let mut y = 860.0f32;
        while y < 1000.0 {
            let mut x = 30.0f32;
            let mut col = row;
            while x < 545.0 {
                let c = if col % 2 == 0 { 0xe8e4da } else { 0x1c1c22 };
                let (a, b) = (p(x, y), p((x + sq).min(545.0), (y + sq).min(1000.0)));
                base.poly(&[a, (b.0, a.1), b, (a.0, b.1)], c, 255);
                x += sq;
                col += 1;
            }
            y += sq;
            row += 1;
        }
        // The plate.
        let (pc, pr) = (p(PLATE.0, PLATE.1), u(62.0));
        base.circle(pc.0 + u(3.0), pc.1 + u(5.0), pr, 0x000000, 90);
        base.circle_with(pc.0, pc.1, pr, |nx, ny| (sphere(0xf2efe8, nx * 0.5, ny * 0.5, 0.78, 0.25, 18.0), 255));
        base.ring(pc.0, pc.1, u(57.0), u(2.5), 0x23a8b4, 255);
        base.circle_with(pc.0, pc.1, u(48.0), |nx, ny| (scale(0xe6e1d6, 0.97 + (nx + ny) * 0.03), 255));
        // The kitchen window: dark inside with the warm light of the kitchen.
        let kw = rrect(u(92.0), u(209.0), u(188.0), u(281.0), u(9.0));
        base.poly_with(&kw, |_, y| {
            let t = ((y / k - 209.0) / 72.0).clamp(0.0, 1.0);
            (mixc(0x1a0e10, 0x7a3a12, (t * t * 255.0) as u32), 255)
        });
        // The apron: a plaque with the table's neon sign.
        let plaque = rrect(u(110.0), u(952.0), u(466.0), u(996.0), u(10.0));
        base.poly(&plaque, 0x0c0a14, 240);
        let size = (u(30.0)) as u16;
        for (o, a) in [(u(3.0), 60u32), (u(1.6), 110), (u(0.8), 170)] {
            for (dx, dy) in [(-o, 0.0), (o, 0.0), (0.0, -o), (0.0, o)] {
                base.text_centered(u(288.0) + dx, u(985.0) + dy, "NEON GRILL", Face::SansBold, size, 0xff2a8a, a);
            }
        }
        base.text_centered(u(288.0), u(985.0), "NEON GRILL", Face::SansBold, size, 0xffd8ec, 255);
        // Labels.
        let lsize = u(11.0) as u16;
        for &(x, y, t) in &[(290.0, 214.0, "COFFEE"), (66.0, 362.0, "FRIES"), (118.0, 547.0, "PIE"), (288.0, 520.0, "GRILL")] {
            base.label(u(x), u(y), t, lsize, 0xfff4dc, 0x14081a);
        }
        lap("flat artwork");
        let mut table = Layer::from_image(&base.shrink(SS, true));
        drop(base);
        lap("shrunk");

        // ---- rails, plastics and other raised parts ------------------------------------------
        let mut raised = Layer::new(w * SS, h * SS);
        // The outer wall: a chrome rail around the arch and down the sides.
        let mut outer = field.clone();
        outer.truncate(outer.len() - 1); // not along the bottom
        let steps: Vec<(f32, f32)> = outer.iter().map(|&(x, y)| p(x, y)).collect();
        for wnd in steps.windows(2) {
            rail(&mut raised, wnd[0], wnd[1], u(5.0), chrome_profile);
        }
        // The plunger lane's wall and gate.
        rail(&mut raised, p(545.0, 1000.0), p(545.0, 340.0), u(4.0), chrome_profile);
        rail(&mut raised, p(590.0, 316.0), p(549.0, 340.0), u(1.6), chrome_profile);
        // Inlane guides.
        for path in [[(78.0, 690.0), (78.0, 800.0), (176.0, 872.0)], [(497.0, 690.0), (497.0, 800.0), (399.0, 872.0)]] {
            for wnd in path.windows(2) {
                rail(&mut raised, p(wnd[0].0, wnd[0].1), p(wnd[1].0, wnd[1].1), u(5.0), plastic);
            }
        }
        // The ramp mouth's guides.
        rail(&mut raised, p(398.0, 662.0), p(MOUTH.0 - 2.0, MOUTH.2), u(4.5), plastic);
        rail(&mut raised, p(462.0, 662.0), p(MOUTH.1 + 2.0, MOUTH.2), u(4.5), plastic);
        // Lane guides with rubber posts.
        for &x in POSTS.iter() {
            rail(&mut raised, p(x, 95.0), p(x, 150.0), u(3.2), plastic);
            for yy in [95.0, 150.0] {
                post(&mut raised, p(x, yy), u(6.0));
            }
        }
        // Slingshots: white plastic, red edges, neon art, white rubber on the kicking face.
        for (a, b, c) in [((118.0, 712.0), (118.0, 800.0), (162.0, 835.0)), ((457.0, 712.0), (457.0, 800.0), (413.0, 835.0))] {
            let tri = [p(a.0, a.1), p(b.0, b.1), p(c.0, c.1)];
            raised.poly_with(&tri, |x, y| (mixc(0xfaf8f4, 0xd8d4cc, ((y / k - 712.0) / 123.0 * 160.0 + (x / k - 118.0).abs() * 0.0) as u32), 255));
            // Inset neon zigzag.
            let cx = (a.0 + b.0 + c.0) / 3.0;
            let cy = (a.1 + b.1 + c.1) / 3.0;
            let side = if a.0 < 300.0 { 1.0 } else { -1.0 };
            let zig = [p(cx - 6.0 * side, cy - 34.0), p(cx + 7.0 * side, cy - 10.0), p(cx - 5.0 * side, cy + 4.0), p(cx + 8.0 * side, cy + 30.0)];
            for wnd in zig.windows(2) {
                raised.capsule(wnd[0].0, wnd[0].1, wnd[1].0, wnd[1].1, u(3.2), 0xff4aa8, 120);
                raised.capsule(wnd[0].0, wnd[0].1, wnd[1].0, wnd[1].1, u(1.3), 0xffe0f0, 255);
            }
            for (q0, q1) in [(tri[1], tri[2]), (tri[0], tri[1])] {
                raised.capsule(q0.0, q0.1, q1.0, q1.1, u(2.6), 0xc81c2c, 255);
            }
            rail(&mut raised, tri[0], tri[2], u(4.0), |t| lighten(scale(0xe8e8ea, 0.85 - t * 0.15), (0.25 - (t + 0.3).abs()).max(0.0) * 2.0));
            for q in tri {
                post(&mut raised, q, u(5.0));
            }
        }
        // The kitchen window's chrome frame and the saucer's rim.
        let outer_f = rrect(u(88.0), u(205.0), u(192.0), u(285.0), u(12.0));
        let inner_f = rrect(u(95.0), u(212.0), u(185.0), u(278.0), u(8.0));
        raised.poly_with(&outer_f, |x, y| {
            let inside = point_in(&inner_f, x, y);
            if inside {
                (0, 0)
            } else {
                (chrome_profile(((y / k - 205.0) / 80.0) * 2.0 - 1.0), 255)
            }
        });
        let sc = p(SAUCER.0, SAUCER.1);
        raised.circle_with(sc.0, sc.1, u(22.0), |nx, ny| {
            let d = libm::sqrtf(nx * nx + ny * ny);
            if d < 0.76 {
                // The hole: dark, deeper towards the far side.
                (scale(0x140c0e, 1.0 - ny * 0.3), 255)
            } else {
                (chrome(nx * 0.9, ny * 0.9), 255)
            }
        });
        // The kitchen's sign.
        let sign = rrect(u(104.0), u(195.0), u(176.0), u(212.0), u(5.0));
        raised.poly(&sign, 0xc81c2c, 255);
        raised.text_centered(u(140.0), u(208.0), "KITCHEN", Face::SansBold, u(11.0) as u16, 0xfff4dc, 255);
        // The spinner's posts and wire.
        let (s0, s1) = (p(SPINNER.0 + 4.0, SPINNER.2), p(SPINNER.1 - 4.0, SPINNER.2));
        raised.capsule(s0.0, s0.1, s1.0, s1.1, u(1.2), 0xb8c0cc, 255);
        post(&mut raised, s0, u(4.0));
        post(&mut raised, s1, u(4.0));
        // The plunger lane's trough end and housing.
        let hb = rrect(u(547.0), u(955.0), u(588.0), u(1000.0), u(4.0));
        raised.poly_with(&hb, |x, _| (chrome_profile((x / k - 547.0) / 41.0 * 2.0 - 1.0), 255));

        lap("raised parts");
        let raised_img = Layer::from_image(&raised.shrink(SS, false));
        drop(raised);
        // Soft shadows below and to the right of the raised parts.
        let shadow = raised_img.silhouette(0x000000, 170, (3.0 * s).max(1.0) as usize);
        table.draw(&shadow, (4.0 * s) as i32, (6.0 * s) as i32, 255);
        // The ramp casts a longer shadow (it's higher).
        let ramp = ramp_sprite(s);
        let ramp_layer = Layer::from_image(&ramp.img);
        let ramp_shadow = ramp_layer.silhouette(0x000000, 120, (5.0 * s).max(1.0) as usize);
        table.draw(&ramp_shadow, -ramp.ox + (11.0 * s) as i32, -ramp.oy + (15.0 * s) as i32, 255);
        table.draw(&raised_img, 0, 0, 255);
        lap("shadows and ramp");

        // Lamps that look alike share their sprites.
        let defs = lamps();
        let mut lamps: Vec<[Sprite; 2]> = Vec::new();
        for (i, d) in defs.iter().enumerate() {
            match (0..i).find(|&j| same_look(&defs[j], d)) {
                Some(j) => {
                    let copy = [lamps[j][0].clone(), lamps[j][1].clone()];
                    lamps.push(copy);
                }
                None => lamps.push([lamp_sprite(d, false, s), lamp_sprite(d, true, s)]),
            }
        }
        let bumpers = (0..3).map(|i| [bumper_sprite(i, false, s), bumper_sprite(i, true, s)]).collect();
        lap("lamps");
        let targets = ["M", "E", "N", "U"].iter().map(|t| [target_sprite(t, true, s), target_sprite(t, false, s)]).collect();
        lap("bumpers and targets");
        Art {
            key,
            table: table.to_image(true),
            lamps,
            bumpers,
            targets,
            ball: ball_sprite(s),
            ball_shadow: ball_shadow(s),
            ramp,
            kitchen_glow: glow_ring(s, 30.0, 17.0, 0xffb040),
            sling_glow: [sling_glow(s, true), sling_glow(s, false)],
        }
    }
}

/// The playfield's outline: the arch, then down the sides (table units).
fn playfield_outline() -> Vec<(f32, f32)> {
    let mut v = Vec::new();
    v.push((30.0, 1000.0));
    v.push((30.0, 300.0));
    let n = 28;
    for i in 1..n {
        let a = core::f32::consts::PI * (1.0 + i as f32 / n as f32);
        v.push((310.0 + 280.0 * libm::cosf(a), 300.0 + 280.0 * libm::sinf(a)));
    }
    v.push((590.0, 300.0));
    v.push((590.0, 1000.0));
    v
}

fn point_in(poly: &[(f32, f32)], x: f32, y: f32) -> bool {
    let mut inside = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if (a.1 > y) != (b.1 > y) && x < a.0 + (y - a.1) / (b.1 - a.1) * (b.0 - a.0) {
            inside = !inside;
        }
    }
    inside
}

/// A rubber post: a dark ring with a chrome cap.
fn post(l: &mut Layer, c: (f32, f32), r: f32) {
    l.circle_with(c.0, c.1, r, |nx, ny| (sphere(0x2a2a30, nx, ny, 0.5, 0.45, 12.0), 255));
    l.circle_with(c.0, c.1, r * 0.5, |nx, ny| (chrome(nx, ny), 255));
}

fn starburst(l: &mut Layer, c: (f32, f32), r: f32, col: u32, a: u32) {
    for i in 0..12 {
        let ang = (i as f32 * 30.0 + 7.0).to_radians();
        let len = if i % 2 == 0 { r } else { r * 0.6 };
        let (ex, ey) = (c.0 + len * libm::cosf(ang), c.1 + len * libm::sinf(ang));
        l.capsule(c.0, c.1, ex, ey, r * 0.025 + 0.6, col, a);
        l.circle(ex, ey, r * 0.07 + 0.8, col, a);
    }
    l.circle(c.0, c.1, r * 0.1, col, a);
}

/// A four-pointed sparkle.
fn twinkle(l: &mut Layer, c: (f32, f32), r: f32, col: u32) {
    let pts: Vec<(f32, f32)> = (0..8)
        .map(|i| {
            let a = (i as f32 * 45.0 - 90.0).to_radians();
            let rr = if i % 2 == 0 { r } else { r * 0.22 };
            (c.0 + rr * libm::cosf(a), c.1 + rr * libm::sinf(a))
        })
        .collect();
    l.poly(&pts, col, 170);
    l.circle(c.0, c.1, r * 0.18, 0xffffff, 230);
}

// ---- sprites ---------------------------------------------------------------------------------

/// A sprite canvas of `half` units each way around its anchor.
fn sprite_layer(half: f32, s: f32) -> (Layer, f32) {
    let k = s * SSP as f32;
    let px = ((half * 2.0 * k) as usize / SSP + 1) * SSP;
    (Layer::new(px, px), k)
}

fn finish(l: &Layer) -> Sprite {
    let img = l.shrink(SSP, false);
    let (ox, oy) = (img.w as i32 / 2, img.h as i32 / 2);
    Sprite { img, ox, oy }
}

fn shape_points(shape: Shape, k: f32, cx: f32, cy: f32) -> Option<Vec<(f32, f32)>> {
    match shape {
        Shape::Pill(w, h) => Some(rrect(cx - w / 2.0 * k, cy - h / 2.0 * k, cx + w / 2.0 * k, cy + h / 2.0 * k, h / 2.0 * k)),
        Shape::Arrow(len, wid, ang) => {
            let (l2, w2) = (len / 2.0 * k, wid / 2.0 * k);
            let sh = -l2 + wid * 0.62 * k;
            let shaft = w2 * 0.5;
            let pts = [(0.0, -l2), (w2, sh), (shaft, sh), (shaft, l2), (-shaft, l2), (-shaft, sh), (-w2, sh)];
            Some(rotate(&pts, ang, cx, cy))
        }
        Shape::Tri(size, ang) => {
            let z = size * k;
            Some(rotate(&[(0.0, -z * 0.6), (z * 0.55, z * 0.4), (-z * 0.55, z * 0.4)], ang, cx, cy))
        }
        Shape::Star(r) => {
            let r = r * k;
            Some((0..10).map(|i| {
                let a = (i as f32 * 36.0 - 90.0).to_radians();
                let rr = if i % 2 == 0 { r } else { r * 0.45 };
                (cx + rr * libm::cosf(a), cy + rr * libm::sinf(a))
            }).collect())
        }
        Shape::Circle(_) | Shape::Dish(..) => None,
    }
}

fn shape_extent(shape: Shape) -> f32 {
    match shape {
        Shape::Circle(r) | Shape::Star(r) | Shape::Dish(_, r) => r,
        Shape::Pill(w, h) => w.max(h) / 2.0,
        Shape::Arrow(l, w, _) => l.max(w) / 2.0,
        Shape::Tri(z, _) => z * 0.7,
    }
}

fn fill_shape(l: &mut Layer, shape: Shape, k: f32, cx: f32, cy: f32, mut f: impl FnMut(f32, f32) -> (u32, u32)) {
    match shape_points(shape, k, cx, cy) {
        Some(pts) => l.poly_with(&pts, |x, y| f(x, y)),
        None => {
            let r = match shape {
                Shape::Circle(r) | Shape::Dish(_, r) => r,
                _ => 0.0,
            } * k;
            l.circle_with(cx, cy, r, |nx, ny| f(cx + nx * r, cy + ny * r));
        }
    }
}

/// An insert lamp: lit (bright, glowing, a hot centre) or unlit (dark coloured plastic with a
/// little gloss).
fn lamp_sprite(d: &LampDef, lit: bool, s: f32) -> Sprite {
    let ext = shape_extent(d.shape);
    let (mut l, k) = sprite_layer(ext + 9.0, s);
    let (cx, cy) = (l.w as f32 / 2.0, l.h as f32 / 2.0);
    let rad = ext * k;
    if lit {
        let mut g = Layer::new(l.w, l.h);
        fill_shape(&mut g, d.shape, k, cx, cy, |_, _| (d.color, 230));
        let g = g.silhouette(d.color, 255, (4.5 * k) as usize);
        l.draw(&g, 0, 0, 220);
    }
    // A dark rim, then the plastic.
    let mut rim = Layer::new(l.w, l.h);
    fill_shape(&mut rim, d.shape, k, cx, cy, |_, _| (0x080408, 255));
    let rim = rim.silhouette(0x080408, 255, (0.9 * k) as usize);
    l.draw(&rim, 0, 0, 200);
    let color = d.color;
    fill_shape(&mut l, d.shape, k, cx, cy, |x, y| {
        let dist = (libm::sqrtf((x - cx) * (x - cx) + (y - cy) * (y - cy)) / rad.max(1.0)).min(1.0);
        if lit {
            (mixc(lighten(color, 0.75), color, (dist * 300.0).min(255.0) as u32), 255)
        } else {
            let t = ((y - cy) / rad.max(1.0) * 0.5 + 0.5).clamp(0.0, 1.0);
            (scale(color, 0.36 - t * 0.14), 255)
        }
    });
    // Gloss.
    match d.shape {
        Shape::Circle(_) | Shape::Dish(..) => l.circle(cx - rad * 0.3, cy - rad * 0.35, rad * 0.4, 0xffffff, if lit { 70 } else { 34 }),
        Shape::Pill(pw, ph) => {
            let (hw, hh) = (pw / 2.0 * k, ph / 2.0 * k);
            l.poly(&rrect(cx - hw + hh * 0.5, cy - hh * 0.8, cx + hw - hh * 0.5, cy - hh * 0.2, hh * 0.3), 0xffffff, if lit { 60 } else { 26 });
        }
        _ => {}
    }
    if let Shape::Dish(dish, r) = d.shape {
        food(&mut l, dish, cx, cy, r * k * 0.8, !lit);
    }
    if !d.text.is_empty() {
        let size = match d.shape {
            Shape::Pill(_, h) => h * 0.62,
            Shape::Circle(r) => r * 0.95,
            _ => 8.0,
        } * k;
        let tc = if lit { 0x1a0606 } else { scale(color, 0.75) };
        l.text_centered(cx, cy + size * 0.36, d.text, Face::SansBold, size as u16, tc, 255);
    }
    finish(&l)
}

/// The dishes (in the plate's inserts, on the order ticket and on the cabinet's art).
pub(super) fn food(l: &mut Layer, dish: usize, cx: f32, cy: f32, r: f32, dim: bool) {
    let c = |col: u32| if dim { scale(col, 0.42) } else { col };
    match dish {
        0 => {
            // Burger.
            l.poly(&rrect(cx - 0.62 * r, cy + 0.18 * r, cx + 0.62 * r, cy + 0.5 * r, 0.14 * r), c(0xd8963c), 255);
            l.poly(&rrect(cx - 0.68 * r, cy - 0.02 * r, cx + 0.68 * r, cy + 0.2 * r, 0.1 * r), c(0x6a3418), 255);
            let lettuce: Vec<(f32, f32)> = (0..=12).map(|i| (cx - 0.7 * r + i as f32 * 0.1167 * r, cy - 0.04 * r + if i % 2 == 0 { 0.0 } else { 0.1 * r })).chain([(cx + 0.7 * r, cy - 0.12 * r), (cx - 0.7 * r, cy - 0.12 * r)]).collect();
            l.poly(&lettuce, c(0x58c040), 255);
            let mut dome = Vec::new();
            for i in 0..=16 {
                let a = core::f32::consts::PI * (1.0 + i as f32 / 16.0);
                dome.push((cx + 0.64 * r * libm::cosf(a), cy - 0.1 * r + 0.55 * r * libm::sinf(a)));
            }
            l.poly(&dome, c(0xe8a24c), 255);
            for &(dx, dy) in &[(-0.3, -0.4), (0.0, -0.5), (0.3, -0.38), (-0.12, -0.3), (0.16, -0.28)] {
                l.circle(cx + dx * r, cy + dy * r, 0.05 * r + 0.5, c(0xfff0c8), 255);
            }
        }
        1 => {
            // Fries in a red box.
            for i in 0..6 {
                let x = cx - 0.36 * r + i as f32 * 0.145 * r;
                let lean = (i as f32 - 2.5) * 0.04 * r;
                l.capsule(x, cy + 0.1 * r, x + lean, cy - 0.55 * r + (i % 2) as f32 * 0.1 * r, 0.07 * r + 0.4, c(0xffd24a), 255);
            }
            l.poly(&[(cx - 0.48 * r, cy - 0.05 * r), (cx + 0.48 * r, cy - 0.05 * r), (cx + 0.34 * r, cy + 0.62 * r), (cx - 0.34 * r, cy + 0.62 * r)], c(0xe0282e), 255);
            l.poly(&[(cx - 0.12 * r, cy + 0.1 * r), (cx + 0.12 * r, cy + 0.1 * r), (cx + 0.08 * r, cy + 0.45 * r), (cx - 0.08 * r, cy + 0.45 * r)], c(0xffd24a), 255);
        }
        2 => {
            // A milkshake with cream, a cherry and a straw.
            l.capsule(cx + 0.1 * r, cy - 0.2 * r, cx + 0.42 * r, cy - 0.78 * r, 0.06 * r + 0.4, c(0xff4050), 255);
            l.poly(&[(cx - 0.38 * r, cy - 0.22 * r), (cx + 0.38 * r, cy - 0.22 * r), (cx + 0.2 * r, cy + 0.62 * r), (cx - 0.2 * r, cy + 0.62 * r)], c(0xffa8d0), 255);
            l.poly(&[(cx - 0.42 * r, cy - 0.26 * r), (cx + 0.42 * r, cy - 0.26 * r), (cx + 0.4 * r, cy - 0.16 * r), (cx - 0.4 * r, cy - 0.16 * r)], c(0xf0f0f4), 255);
            for &(dx, dy, rr) in &[(-0.18, -0.36, 0.17), (0.12, -0.38, 0.18), (-0.02, -0.5, 0.17)] {
                l.circle(cx + dx * r, cy + dy * r, rr * r, c(0xfffaf4), 255);
            }
            l.circle(cx + 0.02 * r, cy - 0.68 * r, 0.12 * r, c(0xe0182c), 255);
        }
        3 => {
            // A slice of cherry pie.
            l.poly(&[(cx, cy - 0.55 * r), (cx + 0.6 * r, cy + 0.3 * r), (cx - 0.6 * r, cy + 0.3 * r)], c(0xc81e30), 255);
            for t in [0.3f32, 0.6] {
                let y = cy - 0.55 * r + t * 0.85 * r;
                let half = 0.6 * r * t;
                l.capsule(cx - half, y, cx + half, y, 0.05 * r + 0.4, c(0xf0be70), 255);
            }
            l.poly(&rrect(cx - 0.66 * r, cy + 0.26 * r, cx + 0.66 * r, cy + 0.5 * r, 0.1 * r), c(0xe0a858), 255);
        }
        _ => {
            // A cup of coffee on a saucer, steaming.
            l.poly(&rrect(cx - 0.62 * r, cy + 0.42 * r, cx + 0.62 * r, cy + 0.56 * r, 0.07 * r), c(0xf4f2ee), 255);
            l.ring(cx + 0.4 * r, cy + 0.1 * r, 0.2 * r, 0.07 * r + 0.4, c(0xf4f2ee), 255);
            l.poly(&rrect(cx - 0.42 * r, cy - 0.12 * r, cx + 0.36 * r, cy + 0.46 * r, 0.12 * r), c(0xf4f2ee), 255);
            l.poly(&rrect(cx - 0.36 * r, cy - 0.1 * r, cx + 0.3 * r, cy - 0.02 * r, 0.04 * r), c(0x5a2c14), 255);
            for i in 0..3 {
                let x = cx - 0.2 * r + i as f32 * 0.2 * r;
                l.capsule(x, cy - 0.2 * r, x + 0.06 * r, cy - 0.4 * r, 0.035 * r + 0.3, c(0xd8dce4), 200);
                l.capsule(x + 0.06 * r, cy - 0.4 * r, x - 0.02 * r, cy - 0.6 * r, 0.035 * r + 0.3, c(0xd8dce4), 200);
            }
        }
    }
}

/// A pop bumper: chrome base, white skirt, a food cap with a domed, glossy look, a shadow; lit,
/// a neon glow and a brighter cap.
fn bumper_sprite(kind: usize, lit: bool, s: f32) -> Sprite {
    let r0 = BUMPER_R;
    let (mut l, k) = sprite_layer(r0 + 18.0, s);
    let (cx, cy) = (l.w as f32 / 2.0, l.h as f32 / 2.0);
    let r = r0 * k;
    let glow = [0xffa040, 0xff60b0, 0xff4050][kind];
    // Shadow.
    let mut sh = Layer::new(l.w, l.h);
    sh.circle(cx + 5.0 * k, cy + 7.0 * k, r + 6.0 * k, 0x000000, 255);
    let sh = sh.silhouette(0x000000, 170, (3.5 * k) as usize);
    l.draw(&sh, 0, 0, 255);
    if lit {
        let mut g = Layer::new(l.w, l.h);
        g.circle(cx, cy, r + 12.0 * k, glow, 255);
        let g = g.silhouette(glow, 255, (5.0 * k) as usize);
        l.draw(&g, 0, 0, 230);
    }
    // Chrome base and white skirt with a coloured trim.
    l.circle_with(cx, cy, r + 6.0 * k, |nx, ny| (chrome(nx * 0.75, ny * 0.75), 255));
    l.circle_with(cx, cy, r + 3.2 * k, |nx, ny| (sphere(if lit { lighten(glow, 0.55) } else { 0xf4f0ea }, nx * 0.45, ny * 0.45, 0.7, 0.3, 10.0), 255));
    l.ring(cx, cy, r + 1.6 * k, 1.2 * k, scale(glow, if lit { 1.0 } else { 0.7 }), 255);
    // The cap: food, then a dome's shading over it.
    let mut cap = Layer::new(l.w, l.h);
    match kind {
        0 => {
            // Burger: lettuce peeking out, a sesame bun.
            cap.circle_with(cx, cy, r, |nx, ny| {
                let a = libm::atan2f(ny, nx);
                let edge = 0.94 + 0.06 * libm::sinf(a * 9.0);
                if nx * nx + ny * ny <= edge * edge { (0x58b040, 255) } else { (0, 0) }
            });
            cap.circle(cx, cy, r * 0.88, 0xd89038, 255);
            cap.circle(cx - r * 0.18, cy - r * 0.2, r * 0.5, 0xe6a650, 140);
            for i in 0..11 {
                let a = (i as f32 * 137.5).to_radians();
                let d = r * 0.62 * libm::sqrtf((i as f32 + 0.5) / 11.0);
                let (sx, sy) = (cx + d * libm::cosf(a), cy + d * libm::sinf(a));
                let (ex, ey) = (sx + r * 0.08 * libm::cosf(a * 2.0), sy + r * 0.08 * libm::sinf(a * 2.0));
                cap.capsule(sx, sy, ex, ey, r * 0.045, 0xfff2d0, 255);
            }
        }
        1 => {
            // Donut: dough, wavy pink icing, sprinkles, the hole.
            cap.circle(cx, cy, r, 0xc88446, 255);
            cap.circle_with(cx, cy, r * 0.9, |nx, ny| {
                let a = libm::atan2f(ny, nx);
                let edge = 0.93 + 0.07 * libm::sinf(a * 7.0);
                let d2 = nx * nx + ny * ny;
                if d2 <= edge * edge && d2 >= 0.4 * 0.4 { (0xff6eb2, 255) } else { (0, 0) }
            });
            let cols = [0xffffff, 0x60d8ff, 0xffe040, 0x70e070, 0xb070ff];
            for i in 0..16 {
                let a = (i as f32 * 61.0 + 13.0).to_radians();
                let d = r * (0.48 + 0.3 * ((i * 7 % 5) as f32 / 4.0));
                let (sx, sy) = (cx + d * libm::cosf(a), cy + d * libm::sinf(a));
                let b = a * 3.0;
                cap.capsule(sx, sy, sx + r * 0.1 * libm::cosf(b), sy + r * 0.1 * libm::sinf(b), r * 0.035, cols[i % 5], 255);
            }
            cap.circle_with(cx, cy, r * 0.3, |nx, ny| (chrome(nx * 0.8, ny * 0.8), 255));
        }
        _ => {
            // Cherry pie: crimped crust, red filling, a lattice.
            cap.circle_with(cx, cy, r, |nx, ny| {
                let a = libm::atan2f(ny, nx);
                let edge = 0.95 + 0.05 * libm::sinf(a * 16.0);
                if nx * nx + ny * ny <= edge * edge { (0xd9a352, 255) } else { (0, 0) }
            });
            cap.circle(cx, cy, r * 0.8, 0xb0182c, 255);
            for d in [-0.45f32, 0.0, 0.45] {
                let half = libm::sqrtf((0.8f32 * 0.8 - d * d).max(0.0)) * r;
                cap.capsule(cx + d * r, cy - half, cx + d * r, cy + half, r * 0.08, 0xeab86a, 255);
                cap.capsule(cx - half, cy + d * r, cx + half, cy + d * r, r * 0.08, 0xeab86a, 255);
            }
        }
    }
    // Dome shading and a highlight.
    for y in 0..cap.h {
        for x in 0..cap.w {
            let i = y * cap.w + x;
            let p = cap.px[i];
            if p >> 24 == 0 {
                continue;
            }
            let (nx, ny) = ((x as f32 + 0.5 - cx) / r, (y as f32 + 0.5 - cy) / r);
            if nx * nx + ny * ny > 1.0 {
                continue;
            }
            let shade = sphere(p & 0xff_ffff, nx * 0.8, ny * 0.8, 0.6, 0.55, 22.0);
            let shade = if lit { lighten(shade, 0.22) } else { shade };
            cap.px[i] = (p & 0xff00_0000) | shade;
        }
    }
    l.draw(&cap, 0, 0, 255);
    finish(&l)
}

/// A drop target, standing (with a letter) or down (a slot).
fn target_sprite(letter: &str, up: bool, s: f32) -> Sprite {
    let (mut l, k) = sprite_layer(26.0, s);
    let (cx, cy) = (l.w as f32 / 2.0, l.h as f32 / 2.0);
    if up {
        let mut sh = Layer::new(l.w, l.h);
        sh.poly(&rrect(cx - 7.0 * k + 4.0 * k, cy - 18.0 * k + 5.0 * k, cx + 7.0 * k + 4.0 * k, cy + 18.0 * k + 5.0 * k, 2.0 * k), 0, 255);
        let sh = sh.silhouette(0, 170, (2.5 * k) as usize);
        l.draw(&sh, 0, 0, 255);
        l.poly_with(&rrect(cx - 7.0 * k, cy - 18.0 * k, cx + 7.0 * k, cy + 18.0 * k, 2.0 * k), |x, _| {
            let t = (x - (cx - 7.0 * k)) / (14.0 * k);
            (if t < 0.18 { 0xffffff } else if t > 0.85 { 0xb8ae9c } else { 0xfff0d8 }, 255)
        });
        l.text_centered(cx + 0.5 * k, cy + 5.0 * k, letter, Face::SansBold, (13.0 * k) as u16, 0xd01828, 255);
    } else {
        l.poly(&rrect(cx - 7.5 * k, cy - 18.5 * k, cx + 4.5 * k, cy + 18.5 * k, 2.0 * k), 0x0c080c, 255);
        l.poly(&rrect(cx - 7.5 * k, cy - 18.5 * k, cx - 5.5 * k, cy + 18.5 * k, 1.0 * k), 0x5a5460, 255);
    }
    finish(&l)
}

fn ball_sprite(s: f32) -> Sprite {
    let (mut l, k) = sprite_layer(BALL_R + 1.5, s);
    let (cx, cy) = (l.w as f32 / 2.0, l.h as f32 / 2.0);
    l.circle_with(cx, cy, BALL_R * k, |nx, ny| (chrome(nx, ny), 255));
    finish(&l)
}

fn ball_shadow(s: f32) -> Sprite {
    let (mut l, k) = sprite_layer(BALL_R + 7.0, s);
    let (cx, cy) = (l.w as f32 / 2.0, l.h as f32 / 2.0);
    l.circle(cx, cy, BALL_R * k * 0.95, 0x000000, 255);
    let l = l.silhouette(0x000000, 150, (2.5 * k) as usize);
    finish(&l)
}

fn glow_ring(s: f32, outer: f32, inner: f32, c: u32) -> Sprite {
    let (mut l, k) = sprite_layer(outer + 8.0, s);
    let (cx, cy) = (l.w as f32 / 2.0, l.h as f32 / 2.0);
    l.ring(cx, cy, outer * k, (outer - inner) * k, c, 255);
    let l = l.silhouette(c, 255, (4.0 * k) as usize);
    finish(&l)
}

/// A slingshot's flash when it kicks; anchored at its corner (118, 712) or (457, 712).
fn sling_glow(s: f32, left: bool) -> Sprite {
    let (mut l, k) = sprite_layer(80.0, s);
    let (cx, cy) = (l.w as f32 / 2.0, l.h as f32 / 2.0);
    let sx = if left { 1.0 } else { -1.0 };
    let tri = [(cx, cy), (cx, cy + 88.0 * k), (cx + 44.0 * sx * k, cy + 123.0 * k)];
    l.poly(&tri, 0xff60b8, 255);
    let l = l.silhouette(0xff60b8, 200, (6.0 * k) as usize);
    finish(&l)
}

/// The shake ramp: see-through pink plastic with chevrons, white rails with red edges, a
/// chrome entrance flap with its sign, and posts holding it up. Anchored at table (0, 0).
fn ramp_sprite(s: f32) -> Sprite {
    let k = s * SS as f32;
    let (x0, y0, x1, y1) = (395.0f32, 200.0f32, 560.0f32, 728.0f32);
    let (w, h) = (((x1 - x0) * k) as usize / SS * SS + SS, ((y1 - y0) * k) as usize / SS * SS + SS);
    let mut l = Layer::new(w, h);
    let p = |x: f32, y: f32| ((x - x0) * k, (y - y0) * k);
    let pts: Vec<(f32, f32)> = RAMP.iter().map(|&(x, y)| p(x, y)).collect();
    // The plastic bed.
    let mut bed = Layer::new(w, h);
    bed.polyline(&pts, 16.0 * k, 0xff7ac0, 255);
    l.draw(&bed, 0, 0, 96);
    // Chevrons pointing along the way.
    let mut d = 22.0;
    let total: f32 = RAMP.windows(2).map(|wd| dist(wd[0], wd[1])).sum();
    while d < total - 14.0 {
        let (a, b) = (point_at(d), point_at(d + 1.0));
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let len = libm::sqrtf(dx * dx + dy * dy).max(1e-3);
        let (fx, fy) = (dx / len, dy / len);
        let (nx, ny) = (-fy, fx);
        let tip = p(a.0 + fx * 6.0, a.1 + fy * 6.0);
        for side in [-1.0f32, 1.0] {
            let end = p(a.0 - fx * 5.0 + nx * 11.0 * side, a.1 - fy * 5.0 + ny * 11.0 * side);
            l.capsule(tip.0, tip.1, end.0, end.1, 2.2 * k, 0xffffff, 120);
        }
        d += 30.0;
    }
    // Rails on both sides.
    for side in [-1.0f32, 1.0] {
        let off: Vec<(f32, f32)> = offset_path(16.0 * side).iter().map(|&(x, y)| p(x, y)).collect();
        l.polyline(&off, 3.8 * k, 0xc81c2c, 255);
        l.polyline(&off, 2.4 * k, 0xf6f4f0, 255);
        let hi: Vec<(f32, f32)> = off.iter().map(|&(x, y)| (x - 0.8 * k, y - 0.8 * k)).collect();
        l.polyline(&hi, 0.8 * k, 0xffffff, 255);
    }
    // Posts holding it up.
    for i in [3usize, 7, 11] {
        let q = offset_path(16.0)[i];
        let c = p(q.0, q.1);
        l.circle_with(c.0, c.1, 3.6 * k, |nx, ny| (chrome(nx, ny), 255));
    }
    // The entrance flap and sign.
    let flap = [p(MOUTH.0 - 2.0, MOUTH.2 - 4.0), p(MOUTH.1 + 2.0, MOUTH.2 - 4.0), p(MOUTH.1 + 1.0, MOUTH.2 + 20.0), p(MOUTH.0 - 1.0, MOUTH.2 + 20.0)];
    let fy0 = (MOUTH.2 - 4.0 - y0) * k;
    l.poly_with(&flap, |_, y| (chrome_profile(((y - fy0) / (24.0 * k)) * 2.0 - 1.0), 255));
    let sign = rrect(p(MOUTH.0 + 2.0, 0.0).0, p(0.0, MOUTH.2 + 3.0).1, p(MOUTH.1 - 2.0, 0.0).0, p(0.0, MOUTH.2 + 17.0).1, 3.0 * k);
    l.poly(&sign, 0xd8287c, 255);
    let c = p((MOUTH.0 + MOUTH.1) / 2.0, MOUTH.2 + 14.0);
    l.text_centered(c.0, c.1, "SHAKE", Face::SansBold, (10.0 * k) as u16, 0xfff0f8, 255);
    // The end, dropping into the right inlane.
    let e = p(RAMP[RAMP.len() - 1].0, RAMP[RAMP.len() - 1].1 + 2.0);
    l.poly(&rrect(e.0 - 14.0 * k, e.1 - 4.0 * k, e.0 + 14.0 * k, e.1 + 3.0 * k, 2.0 * k), 0xd0d6de, 255);
    let img = l.shrink(SS, false);
    Sprite { img, ox: -((x0 * s) as i32), oy: -((y0 * s) as i32) }
}

fn dist(a: (f32, f32), b: (f32, f32)) -> f32 {
    libm::sqrtf((b.0 - a.0) * (b.0 - a.0) + (b.1 - a.1) * (b.1 - a.1))
}

fn point_at(mut d: f32) -> (f32, f32) {
    for w in RAMP.windows(2) {
        let l = dist(w[0], w[1]);
        if d <= l {
            let t = d / l.max(1e-3);
            return (w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t);
        }
        d -= l;
    }
    RAMP[RAMP.len() - 1]
}

/// The ramp's path shifted sideways (positive: to the left of the direction of travel).
fn offset_path(off: f32) -> Vec<(f32, f32)> {
    let n = RAMP.len();
    (0..n)
        .map(|i| {
            let a = RAMP[if i == 0 { 0 } else { i - 1 }];
            let b = RAMP[if i == n - 1 { n - 1 } else { i + 1 }];
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let len = libm::sqrtf(dx * dx + dy * dy).max(1e-3);
            (RAMP[i].0 + dy / len * off, RAMP[i].1 - dx / len * off)
        })
        .collect()
}

// ---- drawn each frame --------------------------------------------------------------------------

/// A flipper: white plastic, red rubber edge, shaded as a rounded bar, a chrome pivot cap.
pub(super) fn flipper(c: &mut Canvas, s: f32, x0: i32, y0: i32, px: f32, py: f32, len: f32, angle: f32, pressed: bool) {
    let (dx, dy) = (libm::cosf(angle), libm::sinf(angle));
    let (r0, r1) = (10.5f32, 6.8f32);
    let (tx, ty) = (px + dx * len, py + dy * len);
    let minx = ((px.min(tx) - r0 - 1.0) * s) as i32 + x0;
    let maxx = ((px.max(tx) + r0 + 1.0) * s) as i32 + x0 + 1;
    let miny = ((py.min(ty) - r0 - 1.0) * s) as i32 + y0;
    let maxy = ((py.max(ty) + r0 + 1.0) * s) as i32 + y0 + 1;
    let body = if pressed { 0xffffff } else { 0xf6f2ea };
    for yy in miny..=maxy {
        for xx in minx..=maxx {
            let x = (xx - x0) as f32 / s + 0.5 / s;
            let y = (yy - y0) as f32 / s + 0.5 / s;
            let (vx, vy) = (x - px, y - py);
            let t = ((vx * dx + vy * dy) / len).clamp(0.0, 1.0);
            let r = r0 + (r1 - r0) * t;
            let (ax, ay) = (px + dx * len * t, py + dy * len * t);
            let (ox, oy) = (x - ax, y - ay);
            let d = libm::sqrtf(ox * ox + oy * oy);
            let cov = ((r - d) * s + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let across = (d / r).min(1.0);
            let (nx, ny) = if d > 1e-3 { (ox / d * across, oy / d * across) } else { (0.0, 0.0) };
            let col = if across > 0.74 { sphere(0xd2202c, nx, ny, 0.45, 0.5, 14.0) } else { sphere(body, nx * 0.8, ny * 0.8, 0.62, 0.65, 26.0) };
            c.blend(xx, yy, col, (cov * 255.0) as u32);
        }
    }
    // Pivot cap.
    let (cx, cy) = (x0 as f32 + px * s, y0 as f32 + py * s);
    let cr = 4.2 * s;
    let (ix0, ix1, iy0, iy1) = ((cx - cr - 1.0) as i32, (cx + cr + 1.0) as i32, (cy - cr - 1.0) as i32, (cy + cr + 1.0) as i32);
    for yy in iy0..=iy1 {
        for xx in ix0..=ix1 {
            let (nx, ny) = ((xx as f32 + 0.5 - cx) / cr, (yy as f32 + 0.5 - cy) / cr);
            let d = libm::sqrtf(nx * nx + ny * ny);
            let cov = ((1.0 - d) * cr + 0.5).clamp(0.0, 1.0);
            if cov > 0.0 {
                c.blend(xx, yy, chrome(nx.clamp(-1.0, 1.0), ny.clamp(-1.0, 1.0)), (cov * 255.0) as u32);
            }
        }
    }
}

/// The spinner's flap seen from above as it turns: a chrome plate whose width changes.
pub(super) fn spinner(c: &mut Canvas, s: f32, x0: i32, y0: i32, angle: f32, lit: bool) {
    let half = libm::fabsf(libm::cosf(angle)) * 4.5 + 0.8;
    let (xa, xb) = (SPINNER.0 + 9.0, SPINNER.1 - 9.0);
    let (ya, yb) = (SPINNER.2 - half, SPINNER.2 + half);
    let face = libm::sinf(angle) > 0.0;
    for yy in (y0 + (ya * s) as i32)..=(y0 + (yb * s) as i32) {
        let t = (((yy - y0) as f32 / s - ya) / (yb - ya).max(0.1)).clamp(0.0, 1.0);
        let base = if lit { 0xffe08a } else { 0xd8dde6 };
        let col = scale(base, if face { 0.75 + 0.45 * (1.0 - t) } else { 0.6 + 0.5 * t });
        c.fill_rect(x0 + (xa * s) as i32, yy, ((xb - xa) * s) as i32, 1, col);
    }
}

/// The plunger: a chrome rod and spring, pulled back by `pull` (0..1).
pub(super) fn plunger(c: &mut Canvas, s: f32, x0: i32, y0: i32, pull: f32) {
    let top = 957.0 + pull * 34.0;
    let x = x0 + (556.0 * s) as i32;
    let w = (22.0 * s) as i32;
    // Tip.
    for yy in 0..(7.0 * s).max(2.0) as i32 {
        let t = yy as f32 / (7.0 * s);
        c.fill_rect(x, y0 + (top * s) as i32 + yy, w, 1, mixc(0xffffff, 0x8a92a0, (t * 255.0) as u32));
    }
    // Spring coils.
    let coils = 6;
    let spring_top = top + 8.0;
    let gap = (1000.0 - spring_top) / coils as f32;
    for i in 0..coils {
        let yy = y0 + ((spring_top + gap * i as f32) * s) as i32;
        crate::games::line(c, x + 2, yy, x + w - 2, yy + (gap * s * 0.6) as i32, (1.6 * s).max(1.0) as i32 + 1, 0xb8c0cc);
    }
}


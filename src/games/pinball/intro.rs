//! The opening: the Neon Grill machine in 3D, in a dark arcade with a checkerboard floor. It
//! lights up one part after another (the topper's neon flickers on, then the coin door, the
//! backglass, the display and the playfield), then the camera swoops from a three-quarter view
//! to where the player stands and down onto the playfield, which ends up exactly where the
//! game draws it, and the game fades in.
//!
//! A small software 3D renderer: quads with textures or flat colours, perspective-correct
//! texture mapping with mipmaps, a depth buffer, near-plane clipping, light from one side plus
//! the machine's own lights, see-through glass, and glows. All the artwork is Solon's own.

use alloc::vec;
use alloc::vec::Vec;
use core::ops::{Add, Mul, Sub};

use super::art::{chrome_profile, food, rrect};
use crate::font::Face;
use crate::games::pinart::{chan, lighten, mixc, rgb3, scale, Layer};
use crate::gfx::Canvas;
use crate::image::Image;

/// When things happen, in seconds from the start.
const T_TOPPER: f32 = 0.55;
const T_COIN: f32 = 0.9;
const T_GLASS: f32 = 1.1;
const T_DISPLAY: f32 = 1.35;
const T_FIELD: f32 = 1.6;
pub(super) const T_MOVE: f32 = 2.3;
const T_FRONT: f32 = 3.9;
pub(super) const T_DOWN: f32 = 5.3;
pub(super) const T_END: f32 = 5.9;

#[derive(Clone, Copy, Debug)]
struct V3 {
    x: f32,
    y: f32,
    z: f32,
}

const fn v3(x: f32, y: f32, z: f32) -> V3 {
    V3 { x, y, z }
}

impl Add for V3 {
    type Output = V3;
    fn add(self, o: V3) -> V3 {
        v3(self.x + o.x, self.y + o.y, self.z + o.z)
    }
}
impl Sub for V3 {
    type Output = V3;
    fn sub(self, o: V3) -> V3 {
        v3(self.x - o.x, self.y - o.y, self.z - o.z)
    }
}
impl Mul<f32> for V3 {
    type Output = V3;
    fn mul(self, k: f32) -> V3 {
        v3(self.x * k, self.y * k, self.z * k)
    }
}
impl V3 {
    fn dot(self, o: V3) -> f32 {
        self.x * o.x + self.y * o.y + self.z * o.z
    }
    fn cross(self, o: V3) -> V3 {
        v3(self.y * o.z - self.z * o.y, self.z * o.x - self.x * o.z, self.x * o.y - self.y * o.x)
    }
    fn len(self) -> f32 {
        libm::sqrtf(self.dot(self))
    }
    fn norm(self) -> V3 {
        self * (1.0 / self.len().max(1e-6))
    }
    fn lerp(self, o: V3, t: f32) -> V3 {
        self + (o - self) * t
    }
}

/// The machine's own lights.
#[derive(Clone, Copy)]
enum Light {
    Topper,
    Glass,
    Display,
    Coin,
}

#[derive(Clone, Copy)]
enum Mat {
    /// A colour lit by the room.
    Flat(u32),
    /// Polished chrome (brighter facing the light, with a sheen).
    Chrome,
    /// A texture lit by the room.
    Tex(usize),
    /// A texture that lights itself when its light is on.
    Lit(usize, Light),
    /// A glowing colour.
    Glow(u32, Light),
    Playfield,
    Floor,
    Glass,
}

struct Quad {
    p: [V3; 4],
    uv: [(f32, f32); 4],
    mat: Mat,
    /// Only seen from the front.
    cull: bool,
}

const TEX_BACKGLASS: usize = 0;
const TEX_DISPLAY: usize = 1;
const TEX_TOPPER: usize = 2;
const TEX_SIDE: usize = 3;
const TEX_FRONT: usize = 4;
const TEX_BOXSIDE: usize = 5;

/// A texture and its smaller copies.
struct Tex {
    mips: Vec<Image>,
}

impl Tex {
    fn new(img: Image) -> Tex {
        let mut mips = vec![img];
        while mips.last().unwrap().w > 16 && mips.last().unwrap().h > 16 {
            let half = halve(mips.last().unwrap());
            mips.push(half);
        }
        Tex { mips }
    }
}

fn halve(img: &Image) -> Image {
    let (w, h) = (img.w / 2, img.h / 2);
    let mut px = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let mut s = [0u32; 3];
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                let p = img.px[(y * 2 + dy) * img.w + x * 2 + dx];
                s[0] += chan(p, 16);
                s[1] += chan(p, 8);
                s[2] += chan(p, 0);
            }
            px[y * w + x] = 0xff00_0000 | rgb3(s[0] / 4, s[1] / 4, s[2] / 4);
        }
    }
    Image { w, h, px, opaque: true }
}

/// Bilinear sample (u, v in 0..1).
fn sample(img: &Image, u: f32, v: f32) -> u32 {
    let x = (u * img.w as f32 - 0.5).clamp(0.0, img.w as f32 - 1.001);
    let y = (v * img.h as f32 - 0.5).clamp(0.0, img.h as f32 - 1.001);
    let (x0, y0) = (x as usize, y as usize);
    let (fx, fy) = (((x - x0 as f32) * 256.0) as u32, ((y - y0 as f32) * 256.0) as u32);
    let x1 = (x0 + 1).min(img.w - 1);
    let y1 = (y0 + 1).min(img.h - 1);
    let (a, b, c, d) = (img.px[y0 * img.w + x0], img.px[y0 * img.w + x1], img.px[y1 * img.w + x0], img.px[y1 * img.w + x1]);
    let m = |s: u32| {
        let top = chan(a, s) * (256 - fx) + chan(b, s) * fx;
        let bot = chan(c, s) * (256 - fx) + chan(d, s) * fx;
        (top * (256 - fy) + bot * fy) >> 16
    };
    rgb3(m(16), m(8), m(0))
}

/// The cabinet's frame: the body is tilted (the playfield rises towards the back).
struct Frame {
    origin: V3,
    along: V3,
    upb: V3,
}

impl Frame {
    fn new() -> Frame {
        let a = 6.5f32.to_radians();
        Frame { origin: v3(0.0, 80.0, 0.0), along: v3(0.0, libm::sinf(a), -libm::cosf(a)), upb: v3(0.0, libm::cosf(a), libm::sinf(a)) }
    }
    /// A point on the body: across, along (front to back), up from its bottom.
    fn at(&self, u: f32, v: f32, w: f32) -> V3 {
        self.origin + v3(u, 0.0, 0.0) + self.along * v + self.upb * w
    }
    fn field_centre(&self) -> V3 {
        self.at(0.0, 56.0, 28.0)
    }
}

const BW: f32 = 36.0; // half width
const BL: f32 = 112.0;
const BH: f32 = 32.0;
const FIELD_W: f32 = 31.0; // half width
const FIELD_V0: f32 = 6.0;
const FIELD_V1: f32 = 106.0;
const FIELD_H: f32 = 28.0;

fn quad(p: [V3; 4], mat: Mat) -> Quad {
    Quad { p, uv: [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)], mat, cull: true }
}

/// A box from its eight corners' extent along three edges, every face in one material (faces
/// ordered so their front is outside).
fn boxq(out: &mut Vec<Quad>, o: V3, a: V3, b: V3, c: V3, mat: Mat) {
    let centre = o + a * 0.5 + b * 0.5 + c * 0.5;
    let faces = [
        [o, o + a, o + a + b, o + b],
        [o + c, o + c + a, o + c + a + b, o + c + b],
        [o, o + b, o + b + c, o + c],
        [o + a, o + a + b, o + a + b + c, o + a + c],
        [o, o + a, o + a + c, o + c],
        [o + b, o + b + a, o + b + a + c, o + b + c],
    ];
    for mut f in faces {
        let n = (f[3] - f[0]).cross(f[1] - f[0]);
        let mid = (f[0] + f[2]) * 0.5;
        if n.dot(mid - centre) < 0.0 {
            f.swap(1, 3);
        }
        out.push(quad(f, mat));
    }
}

/// Axis-aligned box.
fn aabb(out: &mut Vec<Quad>, min: V3, max: V3, mat: Mat) {
    let d = max - min;
    boxq(out, min, v3(d.x, 0.0, 0.0), v3(0.0, d.y, 0.0), v3(0.0, 0.0, d.z), mat);
}

/// The whole machine and the floor.
fn scene() -> Vec<Quad> {
    let f = Frame::new();
    let mut q = Vec::new();
    let at = |u: f32, v: f32, w: f32| f.at(u, v, w);
    // The body: textured sides and front (corners: top left, top right, bottom right, bottom
    // left as seen from outside).
    q.push(quad([at(-BW, BL, BH), at(-BW, 0.0, BH), at(-BW, 0.0, 0.0), at(-BW, BL, 0.0)], Mat::Tex(TEX_SIDE)));
    q.push(quad([at(BW, 0.0, BH), at(BW, BL, BH), at(BW, BL, 0.0), at(BW, 0.0, 0.0)], Mat::Tex(TEX_SIDE)));
    q.push(quad([at(-BW, 0.0, BH), at(BW, 0.0, BH), at(BW, 0.0, 0.0), at(-BW, 0.0, 0.0)], Mat::Tex(TEX_FRONT)));
    q.push(quad([at(BW, BL, BH), at(-BW, BL, BH), at(-BW, BL, 0.0), at(BW, BL, 0.0)], Mat::Flat(0x141018)));
    q.push(quad([at(-BW, 0.0, 0.0), at(BW, 0.0, 0.0), at(BW, BL, 0.0), at(-BW, BL, 0.0)], Mat::Flat(0x08060a)));
    // The top: rails around the playfield, the playfield sunk below them, the glass.
    q.push(quad([at(-BW, BL, BH), at(-FIELD_W, BL, BH), at(-FIELD_W, 0.0, BH), at(-BW, 0.0, BH)], Mat::Chrome));
    q.push(quad([at(FIELD_W, BL, BH), at(BW, BL, BH), at(BW, 0.0, BH), at(FIELD_W, 0.0, BH)], Mat::Chrome));
    q.push(quad([at(-FIELD_W, FIELD_V0, BH), at(FIELD_W, FIELD_V0, BH), at(FIELD_W, 0.0, BH), at(-FIELD_W, 0.0, BH)], Mat::Flat(0x1c1820)));
    q.push(quad([at(-FIELD_W, BL, BH), at(FIELD_W, BL, BH), at(FIELD_W, FIELD_V1, BH), at(-FIELD_W, FIELD_V1, BH)], Mat::Flat(0x1c1820)));
    // Inner walls of the playfield's well.
    q.push(quad([at(-FIELD_W, FIELD_V1, BH), at(-FIELD_W, FIELD_V0, BH), at(-FIELD_W, FIELD_V0, FIELD_H), at(-FIELD_W, FIELD_V1, FIELD_H)], Mat::Flat(0x2a1420)));
    q.push(quad([at(FIELD_W, FIELD_V0, BH), at(FIELD_W, FIELD_V1, BH), at(FIELD_W, FIELD_V1, FIELD_H), at(FIELD_W, FIELD_V0, FIELD_H)], Mat::Flat(0x2a1420)));
    q.push(quad([at(-FIELD_W, FIELD_V1, BH), at(FIELD_W, FIELD_V1, BH), at(FIELD_W, FIELD_V1, FIELD_H), at(-FIELD_W, FIELD_V1, FIELD_H)], Mat::Flat(0x2a1420)));
    q.push(quad([at(FIELD_W, FIELD_V0, BH), at(-FIELD_W, FIELD_V0, BH), at(-FIELD_W, FIELD_V0, FIELD_H), at(FIELD_W, FIELD_V0, FIELD_H)], Mat::Flat(0x2a1420)));
    q.push(quad([at(-FIELD_W, FIELD_V1, FIELD_H), at(FIELD_W, FIELD_V1, FIELD_H), at(FIELD_W, FIELD_V0, FIELD_H), at(-FIELD_W, FIELD_V0, FIELD_H)], Mat::Playfield));
    let mut glass = quad([at(-FIELD_W, FIELD_V1, BH - 0.5), at(FIELD_W, FIELD_V1, BH - 0.5), at(FIELD_W, FIELD_V0, BH - 0.5), at(-FIELD_W, FIELD_V0, BH - 0.5)], Mat::Glass);
    glass.cull = false;
    q.push(glass);
    // Lockdown bar, flipper buttons, plunger.
    boxq(&mut q, at(-BW - 0.6, -1.0, BH - 1.0), v3(2.0 * BW + 1.2, 0.0, 0.0), f.along * 6.0, f.upb * 3.2, Mat::Chrome);
    for side in [-1.0f32, 1.0] {
        let x0 = if side < 0.0 { -BW - 1.6 } else { BW };
        boxq(&mut q, at(x0, 9.0, 23.0), v3(1.6, 0.0, 0.0), f.along * 4.0, f.upb * 4.0, Mat::Flat(0xd81c34));
    }
    boxq(&mut q, at(23.0, -7.0, 18.0), v3(3.6, 0.0, 0.0), f.along * 7.0, f.upb * 3.6, Mat::Chrome);
    boxq(&mut q, at(23.5, -9.5, 18.3), v3(2.6, 0.0, 0.0), f.along * 2.6, f.upb * 3.0, Mat::Flat(0xd81c34));
    // Coin slots glow orange.
    for x in [-9.0f32, 4.0] {
        let s = |u: f32, w: f32| at(u, -0.3, w);
        q.push(quad([s(x, 20.0), s(x + 5.0, 20.0), s(x + 5.0, 16.0), s(x, 16.0)], Mat::Glow(0xff9a2a, Light::Coin)));
    }
    // Legs with plates and feet.
    for &(u, v) in &[(-32.0f32, 5.0f32), (32.0, 5.0), (-32.0, 107.0), (32.0, 107.0)] {
        let top = at(u, v, 0.0);
        aabb(&mut q, v3(top.x - 2.0, 3.0, top.z - 2.0), v3(top.x + 2.0, top.y, top.z + 2.0), Mat::Chrome);
        aabb(&mut q, v3(top.x - 4.0, top.y - 7.0, top.z - 4.0), v3(top.x + 4.0, top.y - 0.5, top.z + 4.0), Mat::Chrome);
        aabb(&mut q, v3(top.x - 2.6, 0.0, top.z - 2.6), v3(top.x + 2.6, 3.0, top.z + 2.6), Mat::Flat(0x18161c));
    }
    // The backbox: dark, the backglass and display set in its front.
    let (z0, z1) = (-130.0f32, -104.0f32);
    let (y0, y1) = (122.0f32, 200.0f32);
    q.push(quad([v3(-BW, y1, z1), v3(BW, y1, z1), v3(BW, y0, z1), v3(-BW, y0, z1)], Mat::Flat(0x0e0c12)));
    q.push(quad([v3(BW, y1, z0), v3(-BW, y1, z0), v3(-BW, y0, z0), v3(BW, y0, z0)], Mat::Flat(0x0e0c12)));
    q.push(quad([v3(-BW, y1, z0), v3(-BW, y1, z1), v3(-BW, y0, z1), v3(-BW, y0, z0)], Mat::Tex(TEX_BOXSIDE)));
    q.push(quad([v3(BW, y1, z1), v3(BW, y1, z0), v3(BW, y0, z0), v3(BW, y0, z1)], Mat::Tex(TEX_BOXSIDE)));
    q.push(quad([v3(-BW, y1, z0), v3(BW, y1, z0), v3(BW, y1, z1), v3(-BW, y1, z1)], Mat::Flat(0x16121a)));
    q.push(quad([v3(-BW, y0, z1), v3(BW, y0, z1), v3(BW, y0, z0), v3(-BW, y0, z0)], Mat::Flat(0x08060a)));
    let zf = z1 + 0.3;
    q.push(quad([v3(-33.0, 197.0, zf), v3(33.0, 197.0, zf), v3(33.0, 142.0, zf), v3(-33.0, 142.0, zf)], Mat::Lit(TEX_BACKGLASS, Light::Glass)));
    q.push(quad([v3(-33.0, 139.0, zf), v3(33.0, 139.0, zf), v3(33.0, 125.0, zf), v3(-33.0, 125.0, zf)], Mat::Lit(TEX_DISPLAY, Light::Display)));
    // A chrome strip between them, and along the backbox's front edges.
    aabb(&mut q, v3(-34.0, 139.6, z1), v3(34.0, 141.4, z1 + 0.8), Mat::Chrome);
    aabb(&mut q, v3(-BW - 0.4, y1 - 1.0, z1), v3(BW + 0.4, y1 + 0.4, z1 + 1.0), Mat::Chrome);
    // The topper sign on top.
    let (tz0, tz1) = (-121.0f32, -117.0f32);
    q.push(quad([v3(-30.0, 227.0, tz1), v3(30.0, 227.0, tz1), v3(30.0, 202.0, tz1), v3(-30.0, 202.0, tz1)], Mat::Lit(TEX_TOPPER, Light::Topper)));
    boxq(&mut q, v3(-30.0, 202.0, tz0), v3(60.0, 0.0, 0.0), v3(0.0, 25.0, 0.0), v3(0.0, 0.0, tz1 - tz0 - 0.1), Mat::Flat(0x101018));
    aabb(&mut q, v3(-12.0, 200.0, -120.0), v3(-9.0, 202.0, -118.0), Mat::Chrome);
    aabb(&mut q, v3(9.0, 200.0, -120.0), v3(12.0, 202.0, -118.0), Mat::Chrome);
    // The floor.
    let mut floor = Quad {
        p: [v3(-900.0, 0.0, -1000.0), v3(900.0, 0.0, -1000.0), v3(900.0, 0.0, 600.0), v3(-900.0, 0.0, 600.0)],
        uv: [(-900.0, -1000.0), (900.0, -1000.0), (900.0, 600.0), (-900.0, 600.0)],
        mat: Mat::Floor,
        cull: false,
    };
    floor.cull = false;
    q.push(floor);
    q
}

// ---- textures: the cabinet's artwork -----------------------------------------------------------

/// A soft glow of what `draw` paints, worked out only around the box (x0, y0)-(x1, y1);
/// `draw` gets the offset to add to its coordinates.
fn glow_box(l: &mut Layer, x0: f32, y0: f32, x1: f32, y1: f32, radius: usize, color: u32, strengths: &[u32], draw: impl Fn(&mut Layer, f32, f32)) {
    let m = radius as f32 * 3.0 + 2.0;
    let (bx, by) = ((x0 - m).max(0.0) as i32, (y0 - m).max(0.0) as i32);
    let (ex, ey) = (((x1 + m) as i32).min(l.w as i32), ((y1 + m) as i32).min(l.h as i32));
    if ex <= bx || ey <= by {
        return;
    }
    let mut g = Layer::new((ex - bx) as usize, (ey - by) as usize);
    draw(&mut g, -bx as f32, -by as f32);
    let sil = g.silhouette(color, 255, radius);
    for &a in strengths {
        l.draw(&sil, bx, by, a);
    }
}

fn bounds(pts: &[(f32, f32)]) -> (f32, f32, f32, f32) {
    pts.iter().fold((f32::MAX, f32::MAX, f32::MIN, f32::MIN), |b, p| (b.0.min(p.0), b.1.min(p.1), b.2.max(p.0), b.3.max(p.1)))
}

fn tube(l: &mut Layer, pts: &[(f32, f32)], r: f32, glow: u32, core: u32) {
    let (x0, y0, x1, y1) = bounds(pts);
    glow_box(l, x0 - r * 3.0, y0 - r * 3.0, x1 + r * 3.0, y1 + r * 3.0, (r * 2.0) as usize, glow, &[255], |g, dx, dy| {
        let moved: Vec<(f32, f32)> = pts.iter().map(|&(x, y)| (x + dx, y + dy)).collect();
        g.polyline(&moved, r * 2.2, glow, 255);
    });
    l.polyline(pts, r, core, 255);
}

fn neon_text(l: &mut Layer, cx: f32, base: f32, s: &str, size: u16, glow: u32, core: u32) {
    let w = crate::font::width(s, Face::SansBold, size) as f32;
    let sz = size as f32;
    glow_box(l, cx - w / 2.0 - sz * 0.3, base - sz * 1.1, cx + w / 2.0 + sz * 0.3, base + sz * 0.4, (size / 9).max(1) as usize, glow, &[255, 160], |g, dx, dy| {
        g.text_centered(cx + dx, base + dy, s, Face::SansBold, size, glow, 255);
    });
    l.text_centered(cx, base, s, Face::SansBold, size, core, 255);
}

fn closed(mut v: Vec<(f32, f32)>) -> Vec<(f32, f32)> {
    if let Some(&first) = v.first() {
        v.push(first);
    }
    v
}

/// The backglass: a night street, the table's neon sign, and dinner on a chrome counter.
fn backglass() -> Image {
    let (w, h, ss) = (528usize, 440usize, 2usize);
    let k = ss as f32;
    let mut l = Layer::new(w * ss, h * ss);
    l.gradient_rows(|t| if t < 0.7 { mixc(0x060a24, 0x40164e, (t / 0.7 * 255.0) as u32) } else { 0x1c0c24 });
    let mut seed = 0x1234_5678u32;
    let mut rnd = |n: u32| {
        seed ^= seed << 13;
        seed ^= seed >> 17;
        seed ^= seed << 5;
        seed % n
    };
    for _ in 0..120 {
        let (x, y) = (rnd(w as u32) as f32 * k, rnd(250) as f32 * k);
        let b = 140 + rnd(115);
        l.circle(x, y, k * (0.6 + rnd(10) as f32 / 12.0), rgb3(b, b, 255), 255);
    }
    // The city behind.
    let mut x = 0.0;
    while x < w as f32 {
        let bw = 18.0 + rnd(30) as f32;
        let top = 230.0 + rnd(70) as f32;
        l.poly(&[(x * k, top * k), ((x + bw) * k, top * k), ((x + bw) * k, 300.0 * k), (x * k, 300.0 * k)], 0x120f2c, 255);
        for wy in (0..((300.0 - top) as u32 / 12)).map(|i| top + 6.0 + i as f32 * 12.0) {
            for wx in (0..(bw as u32 / 9)).map(|i| x + 4.0 + i as f32 * 9.0) {
                if rnd(4) == 0 {
                    l.poly(&[(wx * k, wy * k), ((wx + 3.0) * k, wy * k), ((wx + 3.0) * k, (wy + 4.0) * k), (wx * k, (wy + 4.0) * k)], 0xffd88a, 200);
                }
            }
        }
        x += bw + 2.0;
    }
    // The sign: a neon frame, the name, an arrow with bulbs.
    let frame = closed(rrect(70.0 * k, 34.0 * k, 458.0 * k, 186.0 * k, 26.0 * k));
    tube(&mut l, &frame, 4.0 * k, 0x1ad0e0, 0xc8fcff);
    neon_text(&mut l, 264.0 * k, 104.0 * k, "NEON", (66.0 * k) as u16, 0xff2a8a, 0xffe0f0);
    neon_text(&mut l, 264.0 * k, 168.0 * k, "GRILL", (58.0 * k) as u16, 0xff2a8a, 0xffe0f0);
    for i in 0..22 {
        let a = (i as f32 / 22.0) * core::f32::consts::TAU;
        let (bx, by) = (264.0 + 210.0 * libm::cosf(a), 110.0 + 92.0 * libm::sinf(a));
        if by > 26.0 && by < 200.0 {
            l.circle(bx * k, by * k, 4.0 * k, 0xffe070, 255);
            l.circle(bx * k - k, by * k - k, 1.5 * k, 0xffffff, 255);
        }
    }
    // The counter: chrome, red vinyl; dinner on top.
    for (dish, cx) in [(0usize, 130.0f32), (2, 264.0), (4, 398.0)] {
        food(&mut l, dish, cx * k, 292.0 * k, 62.0 * k, false);
    }
    l.poly_with(&[(0.0, 334.0 * k), (w as f32 * k, 334.0 * k), (w as f32 * k, 352.0 * k), (0.0, 352.0 * k)], |_, y| (chrome_profile((y / k - 334.0) / 18.0 * 2.0 - 1.0), 255));
    l.poly(&[(0.0, 352.0 * k), (w as f32 * k, 352.0 * k), (w as f32 * k, 384.0 * k), (0.0, 384.0 * k)], 0xc8182c, 255);
    neon_text(&mut l, 264.0 * k, 376.0 * k, "OPEN ALL NIGHT", (20.0 * k) as u16, 0x10a090, 0xc0fff0);
    // Floor tiles.
    for row in 0..3 {
        for col in 0..24 {
            let (x0, y0) = (col as f32 * 22.0, 384.0 + row as f32 * 19.0);
            let c = if (row + col) % 2 == 0 { 0xece6dc } else { 0x16141a };
            l.poly(&[(x0 * k, y0 * k), ((x0 + 22.0) * k, y0 * k), ((x0 + 22.0) * k, (y0 + 19.0) * k), (x0 * k, (y0 + 19.0) * k)], c, 255);
        }
    }
    // A thin chrome frame.
    for (a, b) in [((0.0, 0.0), (w as f32, 0.0)), ((w as f32, 0.0), (w as f32, h as f32)), ((w as f32, h as f32), (0.0, h as f32)), ((0.0, h as f32), (0.0, 0.0))] {
        l.capsule(a.0 * k, a.1 * k, b.0 * k, b.1 * k, 4.0 * k, 0xc8ced8, 255);
    }
    l.shrink(ss, true)
}

/// The topper: a teal sign in a chrome frame with the name in pink neon and a lit arrow.
fn topper() -> Image {
    let (w, h, ss) = (480usize, 200usize, 2usize);
    let k = ss as f32;
    let mut l = Layer::new(w * ss, h * ss);
    l.gradient_rows(|_| 0x060408);
    let board = rrect(6.0 * k, 6.0 * k, 474.0 * k, 194.0 * k, 30.0 * k);
    l.poly_with(&board, |_, y| (mixc(0x0d5a62, 0x062a30, (y / (h as f32 * k) * 255.0) as u32), 255));
    let edge = closed(rrect(6.0 * k, 6.0 * k, 474.0 * k, 194.0 * k, 30.0 * k));
    l.polyline(&edge, 6.0 * k, 0xc8ced8, 255);
    l.polyline(&edge, 2.0 * k, 0xffffff, 255);
    neon_text(&mut l, 240.0 * k, 112.0 * k, "NEON GRILL", (64.0 * k) as u16, 0xff2a8a, 0xffe6f2);
    let arrow = [(60.0 * k, 150.0 * k), (400.0 * k, 150.0 * k), (400.0 * k, 138.0 * k), (432.0 * k, 160.0 * k), (400.0 * k, 182.0 * k), (400.0 * k, 170.0 * k), (60.0 * k, 170.0 * k)];
    tube(&mut l, &closed(arrow.to_vec()), 2.5 * k, 0xffb020, 0xfff2b0);
    for i in 0..12 {
        let x = 76.0 + i as f32 * 28.0;
        l.circle(x * k, 160.0 * k, 4.0 * k, 0xffe070, 255);
    }
    l.shrink(ss, true)
}

/// The cabinet's side: glossy black, a pink and mint swoosh, the name and two dishes.
fn side() -> Image {
    let (w, h, ss) = (720usize, 204usize, 2usize);
    let k = ss as f32;
    let mut l = Layer::new(w * ss, h * ss);
    l.gradient_rows(|t| mixc(0x26222c, 0x08070a, (t * 255.0) as u32));
    let swoosh = |off: f32| -> Vec<(f32, f32)> { (0..=24).map(|i| { let t = i as f32 / 24.0; ((t * 720.0) * k, (150.0 - 70.0 * t * t + off) * k) }).collect() };
    for (off, col) in [(0.0, 0xff4aa0), (16.0, 0x6ef0d0)] {
        l.polyline(&swoosh(off), 5.0 * k, col, 255);
    }
    let size = (78.0 * k) as u16;
    let tw = crate::font::width("NEON GRILL", Face::SansBold, size) as f32;
    glow_box(&mut l, 360.0 * k - tw / 2.0 - 20.0 * k, 50.0 * k, 360.0 * k + tw / 2.0 + 20.0 * k, 140.0 * k, (3.0 * k) as usize, 0x0a0608, &[255, 255], |g, dx, dy| {
        g.text_centered(360.0 * k + dx, 118.0 * k + dy, "NEON GRILL", Face::SansBold, size, 0xffffff, 255);
    });
    l.text_centered(360.0 * k, 118.0 * k, "NEON GRILL", Face::SansBold, (78.0 * k) as u16, 0xff5aa8, 255);
    food(&mut l, 0, 96.0 * k, 92.0 * k, 54.0 * k, false);
    food(&mut l, 2, 624.0 * k, 88.0 * k, 54.0 * k, false);
    for y in [4.0f32, 200.0] {
        l.capsule(0.0, y * k, w as f32 * k, y * k, 4.0 * k, 0xc8ced8, 255);
    }
    l.shrink(ss, true)
}

/// The cabinet's front: black with the coin door.
fn front() -> Image {
    let (w, h, ss) = (288usize, 128usize, 2usize);
    let k = ss as f32;
    let mut l = Layer::new(w * ss, h * ss);
    l.gradient_rows(|t| mixc(0x24202a, 0x0a080c, (t * 255.0) as u32));
    let door = rrect(84.0 * k, 22.0 * k, 204.0 * k, 118.0 * k, 8.0 * k);
    l.poly(&door, 0xb8bec8, 255);
    l.poly(&rrect(88.0 * k, 26.0 * k, 200.0 * k, 114.0 * k, 6.0 * k), 0x1c1a20, 255);
    l.poly(&rrect(124.0 * k, 72.0 * k, 164.0 * k, 92.0 * k, 4.0 * k), 0x0c0a0e, 255);
    l.circle(40.0 * k, 30.0 * k, 9.0 * k, 0xd81c34, 255);
    l.circle(38.0 * k, 28.0 * k, 3.5 * k, 0xff8090, 255);
    l.text_centered(40.0 * k, 54.0 * k, "START", Face::SansBold, (11.0 * k) as u16, 0xd8d4dc, 255);
    l.capsule(0.0, 3.0 * k, w as f32 * k, 3.0 * k, 3.0 * k, 0xc8ced8, 255);
    l.shrink(ss, true)
}

/// The backbox's sides: black with a pink stripe.
fn box_side() -> Image {
    let (w, h, ss) = (104usize, 304usize, 2usize);
    let k = ss as f32;
    let mut l = Layer::new(w * ss, h * ss);
    l.gradient_rows(|t| mixc(0x221e28, 0x0a080c, (t * 255.0) as u32));
    l.capsule(30.0 * k, 0.0, 30.0 * k, h as f32 * k, 5.0 * k, 0xff4aa0, 255);
    l.capsule(44.0 * k, 0.0, 44.0 * k, h as f32 * k, 2.5 * k, 0x6ef0d0, 255);
    l.shrink(ss, true)
}

/// The display: amber dots behind black glass between two speaker grilles.
fn display(t: f32, on: f32) -> Image {
    let (w, h) = (520usize, 110usize);
    let mut c = Canvas::new(w, h);
    c.fill_rect(0, 0, w as i32, h as i32, 0x0a080c);
    for cx in [40i32, 480] {
        c.circle(cx, 55, 36, 0x2a2830);
        for gy in (-30..=30).step_by(6) {
            for gx in (-30..=30).step_by(6) {
                if gx * gx + gy * gy < 30 * 30 {
                    c.fill_rect(cx + gx - 1, 55 + gy - 1, 3, 3, 0x0c0b10);
                }
            }
        }
    }
    let amber = scale(0xff9628, 0.15 + 0.85 * on);
    let dim = 0x2a1606;
    let (line1, line2) = if on <= 0.0 { ("", "") } else if (t * 2.0) as i32 % 2 == 0 { ("NEON GRILL", "PRESS START") } else { ("NEON GRILL", "") };
    super::dot_text(&mut c, 92, 12, line1, 5, 12, amber, dim, 0);
    super::dot_text(&mut c, 92, 60, line2, 5, 12, amber, dim, 0);
    Image { w, h, px: c.px.iter().map(|p| p | 0xff00_0000).collect(), opaque: true }
}

// ---- the renderer --------------------------------------------------------------------------------

struct Cam {
    eye: V3,
    fw: V3,
    rt: V3,
    up: V3,
    f: f32,
    cx: f32,
    cy: f32,
}

impl Cam {
    fn look(eye: V3, target: V3, upv: V3, f: f32, cx: f32, cy: f32) -> Cam {
        let fw = (target - eye).norm();
        let rt = fw.cross(upv).norm();
        let up = rt.cross(fw);
        Cam { eye, fw, rt, up, f, cx, cy }
    }
    fn to_cam(&self, p: V3) -> V3 {
        let d = p - self.eye;
        v3(d.dot(self.rt), d.dot(self.up), d.dot(self.fw))
    }
}

#[derive(Clone, Copy)]
struct Vtx {
    c: V3,
    u: f32,
    v: f32,
}

#[derive(Clone, Copy)]
struct Sv {
    x: f32,
    y: f32,
    iz: f32,
    uz: f32,
    vz: f32,
}

const NEAR: f32 = 2.0;

/// How bright the machine's lights are.
#[derive(Clone, Copy)]
struct Lights {
    topper: f32,
    glass: f32,
    display: f32,
    coin: f32,
    field: f32,
    glass_streak: f32,
}

impl Lights {
    fn of(&self, l: Light) -> f32 {
        match l {
            Light::Topper => self.topper,
            Light::Glass => self.glass,
            Light::Display => self.display,
            Light::Coin => self.coin,
        }
    }
}

/// A neon light switching on: a few flickers, then steady.
fn flicker(t: f32, on_at: f32) -> f32 {
    let d = t - on_at;
    if d < 0.0 {
        return 0.0;
    }
    if d > 0.45 {
        return 1.0;
    }
    const PATTERN: [f32; 9] = [1.0, 0.0, 0.0, 1.0, 0.2, 1.0, 0.0, 1.0, 1.0];
    PATTERN[((d / 0.05) as usize).min(8)]
}

fn smooth(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * (3.0 - 2.0 * t)
}

fn catmull(p0: V3, p1: V3, p2: V3, p3: V3, t: f32) -> V3 {
    let (t2, t3) = (t * t, t * t * t);
    (p1 * 2.0 + (p2 - p0) * t + (p0 * 2.0 - p1 * 5.0 + p2 * 4.0 - p3) * t2 + (p1 * 3.0 - p0 - p2 * 3.0 + p3) * t3) * 0.5
}

pub struct Intro {
    pub start: u64,
    canvas: Canvas,
    zbuf: Vec<f32>,
    tex: Vec<Tex>,
    quads: Vec<Quad>,
    field: Option<Tex>,
    pub sounds: u8,
    /// The artwork is ready and the clock running (set on the first frame).
    started: bool,
}

impl Intro {
    pub fn new(now: u64) -> Intro {
        Intro { start: now, canvas: Canvas::new(1, 1), zbuf: Vec::new(), tex: Vec::new(), quads: Vec::new(), field: None, sounds: 0, started: false }
    }

    pub fn started(&self) -> bool {
        self.started
    }

    /// Build the cabinet's artwork, then start the clock (so a slow start doesn't eat into
    /// the opening).
    pub fn prepare(&mut self) {
        if self.tex.is_empty() {
            let t0 = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
            self.tex = vec![Tex::new(backglass()), Tex::new(display(0.0, 0.0)), Tex::new(topper()), Tex::new(side()), Tex::new(front()), Tex::new(box_side())];
            self.quads = scene();
            let t1 = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
            kprintln!("pinball: opening artwork built in {} ms", (t1 - t0) * 1000 / crate::interrupts::HZ);
        }
        self.start = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        self.started = true;
    }

    /// Seconds since the start (0 until the first frame).
    pub fn time(&self, now: u64) -> f32 {
        if !self.started {
            return 0.0;
        }
        now.saturating_sub(self.start) as f32 / crate::interrupts::HZ as f32
    }

    /// How opaque the opening is over the game (fading out at the end).
    pub fn alpha(&self, now: u64) -> f32 {
        let t = self.time(now);
        1.0 - ((t - T_DOWN) / (T_END - T_DOWN)).clamp(0.0, 1.0)
    }

    pub fn has_field(&self) -> bool {
        self.field.is_some()
    }

    /// Hand over the table's picture for the playfield (as the game draws it at `s`).
    pub fn set_field(&mut self, img: Image) {
        self.field = Some(Tex::new(img));
    }

    /// Draw the frame at `now` for the game's area `w` x `h`; the playfield ends at (fx, fy)
    /// with scale `s` (as the game draws it). Big areas are drawn smaller (the returned scale)
    /// to keep the frame rate up.
    pub fn render(&mut self, now: u64, w: usize, h: usize, s: f32, fx: f32, fy: f32) -> (&Canvas, f32) {
        let k = (1100.0 / w as f32).min(640.0 / h as f32).min(1.0);
        let (w, h) = (((w as f32 * k) as usize).max(1), ((h as f32 * k) as usize).max(1));
        let (s, fx, fy) = (s * k, fx * k, fy * k);
        if !self.started {
            self.prepare();
        }
        if self.canvas.w != w || self.canvas.h != h {
            self.canvas = Canvas::new(w, h);
            self.zbuf = vec![0.0; w * h];
        }
        let t = self.time(now);
        let lights = Lights {
            topper: flicker(t, T_TOPPER),
            glass: flicker(t, T_GLASS),
            display: smooth((t - T_DISPLAY) / 0.2),
            coin: smooth((t - T_COIN) / 0.15),
            field: smooth((t - T_FIELD) / 0.35),
            glass_streak: 1.0 - smooth((t - T_FRONT) / (T_DOWN - T_FRONT - 0.2)),
        };
        self.tex[TEX_DISPLAY] = Tex::new(display(t, lights.display));
        let cam = self.camera(t, w as f32, h as f32, s, fx, fy);
        // The room: dark, a little lighter at the horizon.
        for y in 0..h {
            let c = mixc(0x07050e, 0x1a1024, (y * 255 / h.max(1)) as u32);
            self.canvas.px[y * w..(y + 1) * w].fill(c);
        }
        self.zbuf.fill(0.0);
        for pass in 0..2 {
            for qi in 0..self.quads.len() {
                let is_glass = matches!(self.quads[qi].mat, Mat::Glass);
                if (pass == 1) != is_glass {
                    continue;
                }
                self.draw_quad(qi, &cam, &lights);
            }
        }
        self.glows(&cam, &lights, t);
        (&self.canvas, k)
    }

    fn camera(&self, t: f32, w: f32, h: f32, s: f32, fx: f32, fy: f32) -> Cam {
        let fr = Frame::new();
        let pivot = v3(0.0, 120.0, -58.0);
        let orbit = |t: f32| {
            let a = (-40.0 + 7.0 * smooth(t / T_MOVE)).to_radians();
            let r = 255.0;
            pivot + v3(r * libm::sinf(a), 64.0 - 6.0 * smooth(t / T_MOVE), r * libm::cosf(a))
        };
        let f1 = h * 1.12;
        let f2 = h * 1.3;
        let d = f2 / (10.0 * s);
        let k1 = orbit(T_MOVE);
        let k2 = v3(0.0, 176.0, 92.0);
        let k3 = fr.field_centre() + fr.upb * d;
        let (t1, t2, t3) = (pivot, v3(0.0, 112.0, -52.0), fr.field_centre());
        let centre = (w / 2.0, h / 2.0);
        let field_c = (fx + 310.0 * s, fy + 500.0 * s);
        if t <= T_MOVE {
            return Cam::look(orbit(t), pivot, v3(0.0, 1.0, 0.0), f1, centre.0, centre.1);
        }
        let p = smooth((t - T_MOVE) / (T_DOWN - T_MOVE));
        let (eye, target) = if p < 0.5 {
            let q = p * 2.0;
            (catmull(k1 + (k1 - k2), k1, k2, k3, q), t1.lerp(t2, smooth(q)))
        } else {
            let q = (p - 0.5) * 2.0;
            (catmull(k1, k2, k3, k3 + (k3 - k2), q), t2.lerp(t3, smooth(q)))
        };
        let (up, f, c) = if p < 0.5 {
            let q = smooth(p * 2.0);
            (v3(0.0, 1.0, 0.0), f1 + (f2 - f1) * q, centre)
        } else {
            let q = smooth((p - 0.5) * 2.0);
            (v3(0.0, 1.0, 0.0).lerp(fr.along, q).norm(), f2, (centre.0 + (field_c.0 - centre.0) * q, centre.1 + (field_c.1 - centre.1) * q))
        };
        // At the very end, exactly over the playfield.
        if t >= T_DOWN {
            return Cam::look(k3, t3, fr.along, f2, field_c.0, field_c.1);
        }
        Cam::look(eye, target, up, f, c.0, c.1)
    }

    fn draw_quad(&mut self, qi: usize, cam: &Cam, lights: &Lights) {
        let q = &self.quads[qi];
        let mat = q.mat;
        let cull = q.cull;
        // Lighting from the room: a key light from the front left, above.
        let n = (q.p[3] - q.p[0]).cross(q.p[1] - q.p[0]).norm();
        let key = v3(-0.45, 0.72, 0.55).norm();
        let lit = 0.34 + 0.72 * n.dot(key).max(0.0);
        let mut poly: Vec<Vtx> = (0..4).map(|i| Vtx { c: cam.to_cam(q.p[i]), u: q.uv[i].0, v: q.uv[i].1 }).collect();
        // Clip against the near plane.
        let mut out = Vec::with_capacity(6);
        for i in 0..poly.len() {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (ia, ib) = (a.c.z >= NEAR, b.c.z >= NEAR);
            if ia {
                out.push(a);
            }
            if ia != ib {
                let t = (NEAR - a.c.z) / (b.c.z - a.c.z);
                out.push(Vtx { c: a.c.lerp(b.c, t), u: a.u + (b.u - a.u) * t, v: a.v + (b.v - a.v) * t });
            }
        }
        poly = out;
        if poly.len() < 3 {
            return;
        }
        let sv: Vec<Sv> = poly
            .iter()
            .map(|p| {
                let iz = 1.0 / p.c.z;
                Sv { x: cam.cx + cam.f * p.c.x * iz, y: cam.cy - cam.f * p.c.y * iz, iz, uz: p.u * iz, vz: p.v * iz }
            })
            .collect();
        let mut area = 0.0;
        for i in 0..sv.len() {
            let (a, b) = (sv[i], sv[(i + 1) % sv.len()]);
            area += a.x * b.y - b.x * a.y;
        }
        if cull && area <= 0.0 {
            return;
        }
        // Texture and its level of detail for this quad's size on screen.
        let texture: Option<&Tex> = match mat {
            Mat::Tex(i) | Mat::Lit(i, _) => Some(&self.tex[i]),
            Mat::Playfield => self.field.as_ref(),
            _ => None,
        };
        let mip = texture.map(|tx| {
            let full = &tx.mips[0];
            let (mut umin, mut umax, mut vmin, mut vmax) = (f32::MAX, f32::MIN, f32::MAX, f32::MIN);
            for p in &poly {
                umin = umin.min(p.u);
                umax = umax.max(p.u);
                vmin = vmin.min(p.v);
                vmax = vmax.max(p.v);
            }
            let texels = ((umax - umin) * full.w as f32) * ((vmax - vmin) * full.h as f32);
            let ratio = libm::sqrtf(texels / (area.abs() * 0.5).max(1.0));
            let mut level = 0;
            let mut r = ratio;
            while r > 1.6 && level + 1 < tx.mips.len() {
                r /= 2.0;
                level += 1;
            }
            &tx.mips[level]
        });
        let (w, h) = (self.canvas.w, self.canvas.h);
        let px = &mut self.canvas.px;
        let zb = &mut self.zbuf;
        let shade = |u: f32, v: f32, z: f32, dst: u32| -> Option<u32> {
            Some(match mat {
                Mat::Flat(c) => scale(c, lit),
                Mat::Chrome => {
                    let sheen = (n.dot(key) * 0.5 + 0.5).clamp(0.0, 1.0);
                    lighten(scale(0x8a92a0, 0.5 + 0.7 * sheen), libm::powf(sheen, 8.0) * 0.6)
                }
                Mat::Tex(_) => scale(sample(mip.unwrap(), u, v), lit),
                Mat::Lit(_, l) => {
                    let p = lights.of(l);
                    scale(sample(mip.unwrap(), u, v), lit * 0.22 + p * 1.05)
                }
                Mat::Glow(c, l) => scale(c, 0.15 + 0.95 * lights.of(l)),
                Mat::Playfield => match mip {
                    Some(img) => scale(sample(img, u, v), 0.16 + 0.9 * lights.field),
                    None => 0x101820,
                },
                Mat::Floor => floor(u, v, z, lights),
                Mat::Glass => {
                    let streak = {
                        let d = (u - v * 0.45 - 0.25).abs();
                        if d < 0.07 { (1.0 - d / 0.07) * 0.22 } else { 0.0 }
                    };
                    let a = (streak + 0.05) * lights.glass_streak;
                    if a <= 0.004 {
                        return None;
                    }
                    mixc(dst, 0xe8f0ff, (a * 255.0) as u32)
                }
            })
        };
        let glass = matches!(mat, Mat::Glass);
        for i in 1..sv.len() - 1 {
            raster(px, zb, w, h, &sv[0], &sv[i], &sv[i + 1], !glass, &shade);
        }
    }

    /// Glows around the lit signs, added to the picture.
    fn glows(&mut self, cam: &Cam, lights: &Lights, _t: f32) {
        let spots = [(v3(0.0, 214.0, -116.0), 34.0, 0xff3a96, lights.topper * 0.55), (v3(0.0, 170.0, -103.0), 40.0, 0x6040ff, lights.glass * 0.3), (v3(0.0, 132.0, -103.0), 30.0, 0xff9a2a, lights.display * 0.25)];
        for (p, radius, col, k) in spots {
            if k <= 0.01 {
                continue;
            }
            let c = cam.to_cam(p);
            if c.z < NEAR {
                continue;
            }
            let (sx, sy) = (cam.cx + cam.f * c.x / c.z, cam.cy - cam.f * c.y / c.z);
            let r = cam.f * radius / c.z * 1.6;
            add_glow(&mut self.canvas, sx, sy, r, col, k);
        }
    }
}

/// The floor: black and white tiles, fading into the dark with distance, with the machine's
/// glow spilling onto it and its shadow beneath.
fn floor(x: f32, z: f32, depth: f32, lights: &Lights) -> u32 {
    let white = (((x * (1.0 / 30.0) + 1000.0) as i32 + (z * (1.0 / 30.0) + 1000.0) as i32) & 1) == 0;
    let contrast = (1.0 - (depth - 160.0) * (1.0 / 700.0)).clamp(0.0, 1.0);
    let lum = if white { 90.0 + 117.0 * contrast } else { 90.0 - 68.0 * contrast } * 0.55;
    // Shadow under the machine.
    let (dx, dz) = ((x.abs() - 38.0).max(0.0), ((z + 56.0).abs() - 62.0).max(0.0));
    let sh = (1.0 - (dx * dx + dz * dz) * (1.0 / 700.0)).max(0.0);
    let lum = lum * (1.0 - 0.65 * sh);
    let (mut r, mut g, mut b) = (lum, lum, lum * 1.03);
    // The machine's light.
    let spill = (1.0 - (x * x + (z + 40.0) * (z + 40.0)) * (1.0 / 57_600.0)).max(0.0);
    let a = spill * spill * (0.25 * lights.field + 0.2 * lights.topper) * 0.7;
    if a > 0.0 {
        r += (255.0 - r) * a;
        g += (74.0 - g) * a;
        b += (168.0 - b) * a;
    }
    // Fog.
    let f = ((depth - 250.0) * (1.0 / 900.0)).clamp(0.0, 1.0);
    r += (26.0 - r) * f;
    g += (16.0 - g) * f;
    b += (36.0 - b) * f;
    rgb3(r as u32, g as u32, b as u32)
}

fn add_glow(c: &mut Canvas, cx: f32, cy: f32, r: f32, col: u32, k: f32) {
    if r < 1.0 {
        return;
    }
    let (x0, x1) = ((cx - r).max(0.0) as i32, ((cx + r) as i32).min(c.w as i32 - 1));
    let (y0, y1) = ((cy - r).max(0.0) as i32, ((cy + r) as i32).min(c.h as i32 - 1));
    // Falloff by squared distance (0..255 of the radius squared), in 1/256ths.
    let mut fall = [0u32; 257];
    for (i, f) in fall.iter_mut().enumerate() {
        let d2 = i as f32 / 256.0;
        *f = ((1.0 - d2).max(0.0) * (1.0 - d2).max(0.0) * k * 256.0) as u32;
    }
    let (cr, cg, cb) = (chan(col, 16), chan(col, 8), chan(col, 0));
    let inv = 256.0 / (r * r);
    for y in y0..=y1 {
        let dy = y as f32 - cy;
        let row = y as usize * c.w;
        for x in x0..=x1 {
            let dx = x as f32 - cx;
            let d = ((dx * dx + dy * dy) * inv) as usize;
            if d >= 256 {
                continue;
            }
            let a = fall[d];
            let p = c.px[row + x as usize];
            c.px[row + x as usize] = rgb3(chan(p, 16) + (cr * a >> 8), chan(p, 8) + (cg * a >> 8), chan(p, 0) + (cb * a >> 8));
        }
    }
}

/// One triangle: depth-tested, perspective-correct (u/z, v/z and 1/z interpolated).
#[allow(clippy::too_many_arguments)]
fn raster<F: Fn(f32, f32, f32, u32) -> Option<u32>>(px: &mut [u32], zb: &mut [f32], w: usize, h: usize, a: &Sv, b: &Sv, c: &Sv, write_depth: bool, shade: &F) {
    let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
    if area.abs() < 1e-6 {
        return;
    }
    let minx = a.x.min(b.x).min(c.x).max(0.0) as i32;
    let maxx = (a.x.max(b.x).max(c.x) + 1.0).min(w as f32) as i32;
    let miny = a.y.min(b.y).min(c.y).max(0.0) as i32;
    let maxy = (a.y.max(b.y).max(c.y) + 1.0).min(h as f32) as i32;
    if minx >= maxx || miny >= maxy {
        return;
    }
    let inv = 1.0 / area;
    // Edge functions: w0 for edge b-c, w1 for c-a, w2 for a-b; stepping in x.
    let e = |p: &Sv, q: &Sv, x: f32, y: f32| (q.x - p.x) * (y - p.y) - (q.y - p.y) * (x - p.x);
    let (dx0, dx1, dx2) = (-(c.y - b.y), -(a.y - c.y), -(b.y - a.y));
    for y in miny..maxy {
        let py = y as f32 + 0.5;
        let fx = minx as f32 + 0.5;
        let (mut w0, mut w1, mut w2) = (e(b, c, fx, py), e(c, a, fx, py), e(a, b, fx, py));
        let row = y as usize * w;
        for x in minx..maxx {
            if (w0 >= 0.0 && w1 >= 0.0 && w2 >= 0.0) || (w0 <= 0.0 && w1 <= 0.0 && w2 <= 0.0) {
                let (l0, l1, l2) = (w0 * inv, w1 * inv, w2 * inv);
                let iz = l0 * a.iz + l1 * b.iz + l2 * c.iz;
                let i = row + x as usize;
                if iz > zb[i] {
                    let z = 1.0 / iz;
                    let u = (l0 * a.uz + l1 * b.uz + l2 * c.uz) * z;
                    let v = (l0 * a.vz + l1 * b.vz + l2 * c.vz) * z;
                    if let Some(col) = shade(u, v, z, px[i]) {
                        px[i] = col;
                        if write_depth {
                            zb[i] = iz;
                        }
                    }
                }
            }
            w0 += dx0;
            w1 += dx1;
            w2 += dx2;
        }
    }
}

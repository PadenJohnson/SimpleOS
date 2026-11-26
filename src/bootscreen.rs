//! The screen while Solon starts: a sunrise over calm water, "SimpleOS", a progress bar and,
//! in plain words, what's happening ("Looking for USB drives..."). The first core draws it at
//! each step; once the other cores are awake one of them keeps the bar moving smoothly (it
//! creeps on through long steps, such as finding USB devices, so the PC never looks stuck),
//! until the desktop takes over.

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::{AtomicBool, AtomicU32, Ordering::SeqCst};
use spin::Mutex;

use crate::font::Face;
use crate::gfx::{rgb, Canvas};
use crate::interrupts::TICKS;
use crate::screen::Screen;

/// What the core moving the bar needs: where the picture is and how to write a pixel, where
/// the bar goes, and what's behind it.
struct Bar {
    base: usize,
    width: usize,
    height: usize,
    pitch: usize,
    shifts: (u8, u8, u8),
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    /// The picture under the bar (w x h, 0x00RRGGBB).
    bg: Vec<u32>,
}

static BAR: Mutex<Option<Bar>> = Mutex::new(None);
/// Progress in tenths of a percent: where the step now under way starts and ends.
static FROM: AtomicU32 = AtomicU32::new(0);
static TO: AtomicU32 = AtomicU32::new(0);
/// The bar is being moved by another core (`ANIMATE` asks it to; `RUNNING` is its answer).
static ANIMATE: AtomicBool = AtomicBool::new(false);
static RUNNING: AtomicBool = AtomicBool::new(false);

/// The picture drawn so far (the first core's), and where its parts are.
struct Drawn {
    canvas: Canvas,
    /// The picture without the words under the bar (to clear them for the next step's).
    clean: Vec<u32>,
    status: (i32, i32, i32, i32),
    status_px: u16,
    status_base: i32,
}

static DRAWN: Mutex<Option<Drawn>> = Mutex::new(None);

const SKY_TOP: u32 = rgb(10, 18, 40);
const SKY_LOW: u32 = rgb(52, 58, 104);
const GLOW: u32 = rgb(255, 160, 86);
const SUN: u32 = rgb(255, 190, 92);
const SUN_CORE: u32 = rgb(255, 218, 140);
const SEA_TOP: u32 = rgb(30, 36, 70);
const SEA_LOW: u32 = rgb(8, 12, 28);
const WORDS: u32 = rgb(206, 214, 232);

/// Draw the whole boot screen on `screen` (again, after a change of resolution).
pub fn show(screen: &Screen) {
    pause();
    let (w, h) = (screen.width, screen.height);
    if w < 64 || h < 64 {
        return;
    }
    let mut c = Canvas::new(w, h);
    let (wi, hi) = (w as i32, h as i32);
    let r = (wi.min(hi) / 10).max(24); // the sun's radius
    let (sx, horizon) = (wi / 2, hi * 46 / 100);
    let sy = horizon + r * 2 / 5; // the sun's centre, a little below the horizon: rising
    // Sky and sea, a warm glow around the sun (fainter over the water), the sun: smooth,
    // worked out pixel by pixel.
    c.gradient_v(0, 0, wi, horizon, SKY_TOP, SKY_LOW);
    c.gradient_v(0, horizon, wi, hi - horizon, SEA_TOP, SEA_LOW);
    let rg = r as f32 * 3.4;
    let (x0, x1) = ((sx as f32 - rg) as i32, (sx as f32 + rg) as i32 + 1);
    let (y0, y1) = ((sy as f32 - rg) as i32, (sy as f32 + rg) as i32 + 1);
    for y in y0.max(0)..y1.min(hi) {
        for x in x0.max(0)..x1.min(wi) {
            let (dx, dy) = (x as f32 + 0.5 - sx as f32, y as f32 + 0.5 - sy as f32);
            let d = libm::sqrtf(dx * dx + dy * dy);
            if d >= rg {
                continue;
            }
            let k = 1.0 - d / rg;
            let a = if y < horizon { 190.0 * k * k * k } else { 70.0 * k * k * k };
            c.blend(x, y, GLOW, a as u32);
            if y < horizon && d < r as f32 + 0.5 {
                // The sun: lighter towards its middle, its edge smoothed.
                let t = (d / r as f32).min(1.0);
                let col = mix(SUN_CORE, SUN, (t * t * 255.0) as u32);
                let cover = (r as f32 + 0.5 - d).clamp(0.0, 1.0);
                c.blend(x, y, col, (cover * 255.0) as u32);
            }
        }
    }
    c.fill_rect_alpha(0, horizon, wi, 1, GLOW, 90);
    // The sun's reflection, broken into lines.
    let lines = 7;
    for k in 0..lines {
        let ly = horizon + 6 + k * (r / 5).max(4);
        let lw = r * 2 * (lines - k) / lines + r / 3;
        c.round_rect_alpha(sx - lw / 2, ly, lw, (r / 24).max(2), 2, SUN, 150 - k as u32 * 18);
    }
    // The name, the bar and the words.
    let title_px = (r * 7 / 10).clamp(28, 120) as u16;
    let title = "SimpleOS";
    let tw = crate::font::width(title, Face::SansBold, title_px);
    let title_base = horizon + r * 2 + title_px as i32;
    c.text_face((wi - tw) / 2 * 64, title_base, title, Face::SansBold, title_px, rgb(255, 255, 255));
    let bar_w = (wi * 28 / 100).clamp(200, 520);
    let bar_h = (r / 14).clamp(5, 12);
    let bar = (wi / 2 - bar_w / 2, title_base + title_px as i32 * 7 / 10, bar_w, bar_h);
    let status_px = (r / 5).clamp(16, 34) as u16;
    let status_base = bar.1 + bar.3 + status_px as i32 * 2;
    let status = (0, status_base - status_px as i32 * 3 / 2, wi, status_px as i32 * 2);
    let clean = c.px.clone();
    let bg: Vec<u32> = (0..bar.3).flat_map(|y| {
        let o = ((bar.1 + y) as usize) * w + bar.0 as usize;
        c.px[o..o + bar.2 as usize].to_vec()
    }).collect();
    *BAR.lock() = Some(Bar { base: screen.base_addr() as usize, width: w, height: h, pitch: screen.pitch(), shifts: screen.shifts(), x: bar.0, y: bar.1, w: bar.2, h: bar.3, bg });
    screen.present(&c, 2, (0, 0, w, h));
    *DRAWN.lock() = Some(Drawn { canvas: c, clean, status, status_px, status_base });
    draw_bar(FROM.load(SeqCst) as f32 / 1000.0, None);
}

/// A new step: the bar goes from `from` towards `to` (percent) and the words say what's
/// happening.
pub fn step(screen: &Screen, from: u32, to: u32, words: &str) {
    FROM.store(from * 10, SeqCst);
    TO.store(to.max(from) * 10, SeqCst);
    let mut g = DRAWN.lock();
    let Some(d) = g.as_mut() else { return };
    let (x, y, w, h) = d.status;
    let cw = d.canvas.w;
    for row in y.max(0)..(y + h).min(d.canvas.h as i32) {
        let o = row as usize * cw + x as usize;
        d.canvas.px[o..o + w as usize].copy_from_slice(&d.clean[o..o + w as usize]);
    }
    let tw = crate::font::width(words, Face::Sans, d.status_px);
    d.canvas.text_face((cw as i32 - tw) / 2 * 64, d.status_base, words, Face::Sans, d.status_px, WORDS);
    let full = (0, 0, d.canvas.w, d.canvas.h);
    screen.present_area(&d.canvas, full, 2, full, Some((0, y.max(0) as usize, cw, h as usize)));
    drop(g);
    if !RUNNING.load(SeqCst) {
        draw_bar(from as f32 / 100.0, None);
    }
    kprintln!("boot: {}", words);
}

/// From now on another core keeps the bar moving (if one is free).
pub fn animate() {
    if BAR.lock().is_none() || RUNNING.load(SeqCst) {
        return;
    }
    ANIMATE.store(true, SeqCst);
    RUNNING.store(true, SeqCst);
    if crate::smp::spawn(Box::new(animate_bar)).is_err() {
        RUNNING.store(false, SeqCst);
        ANIMATE.store(false, SeqCst);
    }
}

/// Stop moving the bar (before the screen changes underneath it), and wait until it's still.
pub fn pause() {
    ANIMATE.store(false, SeqCst);
    // It notices within a timer tick; give up after 300 ms, or a count of spins if the timer
    // isn't ticking (interrupts off).
    let start = TICKS.load(SeqCst);
    let mut spins = 0u64;
    while RUNNING.load(SeqCst) && TICKS.load(SeqCst) < start + 30 && spins < 400_000_000 {
        core::hint::spin_loop();
        spins += 1;
    }
}

/// The desktop is about to be drawn: stop, and let go of the boot screen's pictures.
pub fn finish(screen: &Screen) {
    pause();
    *DRAWN.lock() = None;
    *BAR.lock() = None;
    // What's on the screen now isn't what the screen remembers sending: send it all again.
    screen.invalidate();
}

/// The core moving the bar: about 50 times a second, closer to where the step says (quickly
/// at first, then creeping towards the step's end), with a soft shine travelling along it.
fn animate_bar() {
    let mut shown = FROM.load(SeqCst) as f32;
    let mut frame = 0u32;
    let mut seen = TICKS.load(SeqCst);
    while ANIMATE.load(SeqCst) {
        // Wait for the next timer tick (every 10 ms), drawing on every second one.
        while TICKS.load(SeqCst) == seen && ANIMATE.load(SeqCst) {
            crate::smp::sleep_on(&TICKS, seen);
        }
        seen = TICKS.load(SeqCst);
        frame += 1;
        if frame % 2 == 1 {
            continue;
        }
        let (from, to) = (FROM.load(SeqCst) as f32, TO.load(SeqCst) as f32);
        if shown < from {
            shown += ((from - shown) * 0.15).max(1.0);
        } else {
            shown += (to - shown) * 0.006;
        }
        shown = shown.min(to.max(from));
        draw_bar(shown / 1000.0, Some(frame / 2));
    }
    RUNNING.store(false, SeqCst);
}

/// Draw the bar filled to `value` (0-1), with the shine at its place for `frame`.
fn draw_bar(value: f32, frame: Option<u32>) {
    let g = BAR.lock();
    let Some(b) = g.as_ref() else { return };
    if b.w <= 0 || b.h <= 0 {
        return;
    }
    let fill = (value.clamp(0.0, 1.0) * b.w as f32) as i32;
    let rad = b.h as f32 / 2.0;
    let shine = frame.map(|f| {
        let period = 90u32; // frames per pass (about 1.8 s)
        let t = (f % period) as f32 / period as f32;
        -60.0 + t * (fill as f32 + 120.0)
    });
    let mut row = vec![0u32; b.w as usize];
    for y in 0..b.h {
        for x in 0..b.w {
            let bg = b.bg[(y * b.w + x) as usize];
            // The capsule's edge, smoothed: how much of this pixel is inside.
            let (cx, cy) = (x as f32 + 0.5, y as f32 + 0.5);
            let dx = if cx < rad { rad - cx } else if cx > b.w as f32 - rad { cx - (b.w as f32 - rad) } else { 0.0 };
            let dy = cy - rad;
            let d = libm::sqrtf(dx * dx + dy * dy);
            let cover = (rad - d + 0.5).clamp(0.0, 1.0);
            if cover <= 0.0 {
                row[x as usize] = bg;
                continue;
            }
            let (col, alpha) = if x < fill {
                // Gold to orange along the bar, lighter where the shine is.
                let t = x as f32 / b.w.max(1) as f32;
                let mut rgb3 = [255.0, 200.0 - 70.0 * t, 90.0 - 40.0 * t];
                if let Some(s) = shine {
                    let k = (1.0 - (x as f32 - s).abs() / 60.0).max(0.0);
                    for v in rgb3.iter_mut() {
                        *v += (255.0 - *v) * 0.55 * k;
                    }
                }
                (rgb(rgb3[0] as u8, rgb3[1] as u8, rgb3[2] as u8), 255.0)
            } else {
                (rgb(255, 255, 255), 46.0)
            };
            row[x as usize] = mix(bg, col, (alpha * cover) as u32);
        }
        write_row(b, b.y + y, b.x, &row);
    }
}

fn mix(a: u32, b: u32, alpha: u32) -> u32 {
    let f = |sh: u32| {
        let (x, y) = ((a >> sh) & 255, (b >> sh) & 255);
        (x * (255 - alpha) + y * alpha) / 255
    };
    f(16) << 16 | f(8) << 8 | f(0)
}

/// Write a row of 0x00RRGGBB pixels to the screen at (x, y), in the screen's own format.
fn write_row(b: &Bar, y: i32, x: i32, px: &[u32]) {
    if y < 0 || y as usize >= b.height || x < 0 {
        return;
    }
    let n = px.len().min(b.width.saturating_sub(x as usize));
    let dst = (b.base + y as usize * b.pitch + x as usize * 4) as *mut u32;
    let (rs, gs, bs) = b.shifts;
    for (i, &p) in px[..n].iter().enumerate() {
        let v = ((p >> 16) & 255) << rs | ((p >> 8) & 255) << gs | (p & 255) << bs;
        unsafe { core::ptr::write_volatile(dst.add(i), v) };
    }
}

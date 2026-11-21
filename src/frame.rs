//! Photo frame and screensaver: photos from Solon storage one after another, full screen,
//! fading into each other, with the time and date. It starts by itself when the computer
//! isn't used for a while (Settings: what to show, how soon, how long each photo stays), or
//! from the Photo Frame app. Moving the mouse or pressing a key ends it.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::font::{width, Face};
use crate::fs::DirEntry;
use crate::gfx::{rgb, Canvas, Rect, Size};
use crate::image::{self, Image};
use crate::interrupts::{HZ, TICKS};
use crate::library::Storage;
use crate::settings::{Settings, SAVERS, SAVER_WAIT, SLIDE_TIME};
use crate::usb::Usb;
use core::sync::atomic::Ordering::Relaxed;

/// How long one photo takes to fade into the next.
const FADE: u64 = 2 * HZ;
/// Enlarge small photos at most this much (in halves: 4 = 2x), so they don't go blurry.
const MAX_UP: usize = 4;

/// A photo made ready for the screen.
struct Slide {
    px: Vec<u32>,
    /// The folder it's in, shown small in the corner ("2026-11-20 Vacation").
    album: String,
}

pub struct Slideshow {
    /// Started from the Photo Frame app (not by sitting idle).
    pub manual: bool,
    clock_only: bool,
    photos: Vec<(DirEntry, DirEntry)>,
    order: Vec<usize>,
    pos: usize,
    size: (usize, usize),
    cur: Option<Slide>,
    next: Option<Slide>,
    /// When the fade to `next` started.
    fade: Option<u64>,
    /// When `cur` finished fading in.
    shown: u64,
    hold: u64,
    /// Photos in a row that couldn't be shown.
    failed: usize,
    /// The clock and date to show (refreshed by the desktop).
    pub time: (u8, u8),
    pub date: (u16, u8, u8),
    started: u64,
}

/// A simple random number generator (xorshift), seeded from the clock.
fn shuffle(v: &mut [usize], seed: u64) {
    let mut s = seed | 1;
    for i in (1..v.len()).rev() {
        s ^= s << 13;
        s ^= s >> 7;
        s ^= s << 17;
        v.swap(i, (s % (i as u64 + 1)) as usize);
    }
}

impl Slideshow {
    pub fn new(storage: &mut Storage, usb: &mut Usb, s: &Settings, manual: bool) -> Slideshow {
        let clock_only = !manual && s.saver == 1;
        let photos = if clock_only { Vec::new() } else { storage.all_photos(usb) };
        let mut order: Vec<usize> = (0..photos.len()).collect();
        if s.shuffle {
            shuffle(&mut order, TICKS.load(Relaxed) ^ crate::rtc::fat_timestamp() as u64);
        }
        kprintln!("frame: started ({}), {} photos", if manual { "photo frame" } else { "screensaver" }, photos.len());
        Slideshow {
            manual,
            clock_only,
            photos,
            order,
            pos: 0,
            size: (0, 0),
            cur: None,
            next: None,
            fade: None,
            shown: 0,
            hold: SLIDE_TIME[s.slide_time.min(SLIDE_TIME.len() - 1)].1 * HZ,
            failed: 0,
            time: (0, 0),
            date: (2026, 1, 1),
            started: TICKS.load(Relaxed),
        }
    }

    fn showing_photos(&self) -> bool {
        !self.clock_only && !self.photos.is_empty() && self.failed < self.photos.len().min(20)
    }

    /// Work to do without waiting for the next timer tick (fading, or a photo to get ready).
    pub fn busy(&self) -> bool {
        self.showing_photos() && (self.fade.is_some() || self.next.is_none())
    }

    /// Advance: get the next photo ready, and start or continue fading. True = redraw.
    pub fn tick(&mut self, now: u64, storage: &mut Storage, usb: &mut Usb) -> bool {
        if !self.showing_photos() || self.size.0 == 0 {
            return false;
        }
        if self.next.is_none() && self.fade.is_none() {
            // Decoding takes a moment; it happens while the current photo is just sitting there.
            self.next = self.load_next(storage, usb);
            if self.next.is_none() {
                return !self.showing_photos(); // gave up: show the clock instead
            }
            let now = TICKS.load(Relaxed);
            if self.cur.is_none() {
                self.fade = Some(now);
            }
            return true;
        }
        match self.fade {
            None if now >= self.shown + self.hold => {
                self.fade = Some(now);
                true
            }
            None => false,
            Some(start) if now >= start + FADE => {
                self.cur = self.next.take();
                self.fade = None;
                self.shown = now;
                true
            }
            Some(_) => true,
        }
    }

    /// Show the next photo straight away (Photo Frame: right arrow).
    pub fn skip(&mut self, now: u64) {
        if self.next.is_some() && self.fade.is_none() {
            self.fade = Some(now);
        }
    }

    fn load_next(&mut self, storage: &mut Storage, usb: &mut Usb) -> Option<Slide> {
        for _ in 0..self.photos.len().min(5) {
            let k = self.order[self.pos % self.order.len()];
            self.pos += 1;
            let (dir, file) = &self.photos[k];
            let r = storage.read_file(usb, file).map_err(|e| e.message()).and_then(|bytes| {
                let turn = exif_orientation(&bytes);
                image::decode(&bytes).map(|img| (img, turn))
            });
            match r {
                Ok((img, turn)) => {
                    self.failed = 0;
                    let px = prepare(&img, turn, self.size.0, self.size.1);
                    return Some(Slide { px, album: dir.name.clone() });
                }
                Err(e) => {
                    kprintln!("frame: can't show \"{}\": {}", file.name, e);
                    self.failed += 1;
                    if !self.showing_photos() {
                        return None;
                    }
                }
            }
        }
        None
    }

    pub fn draw(&mut self, c: &mut Canvas, s: &Settings) {
        let (w, h) = (c.w, c.h);
        if self.size != (w, h) {
            // First time, or the screen size changed: start the photos over at this size.
            self.size = (w, h);
            self.cur = None;
            self.next = None;
            self.fade = None;
        }
        if !self.showing_photos() {
            self.draw_clock_screen(c, s);
            return;
        }
        let a = self.fade.map(|t| ((TICKS.load(Relaxed).saturating_sub(t)) * 256 / FADE).min(256) as u32);
        match (&self.cur, &self.next, a) {
            (Some(cur), Some(next), Some(a)) => blend(&mut c.px, &cur.px, &next.px, a),
            (None, Some(next), Some(a)) => fade_in(&mut c.px, &next.px, a),
            (Some(cur), _, _) => c.px.copy_from_slice(&cur.px),
            _ => c.px.fill(0),
        }
        // The time and date along the bottom, over a soft shadow.
        let (w, h) = (w as i32, h as i32);
        let band = h * 3 / 10;
        for k in 0..band {
            let alpha = (k * k * 150 / (band * band)) as u32;
            c.fill_rect_alpha(0, h - band + k, w, 1, 0, alpha);
        }
        let big = (h / 7).clamp(48, 150) as u16;
        let small = (big / 3).max(18);
        let x = (w / 28).max(24);
        let base = h - x - small as i32 - small as i32 / 2;
        let white = rgb(255, 255, 255);
        let clock = s.clock_text(self.time.0, self.time.1);
        c.text_face(x * 64, base, &clock, Face::Sans, big, white);
        let date = format!("{} {}", crate::settings::long_date_text(self.date.0, self.date.1, self.date.2), self.date.0);
        c.text_face(x * 64, base + small as i32 + small as i32 / 2 + 4, &date, Face::Sans, small, rgb(235, 238, 242));
        // Which album the photo is from.
        let album = match (&self.next, a) {
            (Some(n), Some(a)) if a >= 128 => Some(&n.album),
            _ => self.cur.as_ref().map(|c| &c.album),
        };
        if let Some(album) = album {
            let px = (small * 3 / 4).max(16);
            let aw = width(album, Face::Sans, px);
            c.text_face((w - x - aw) * 64, base + small as i32 + small as i32 / 2 + 4, album, Face::Sans, px, rgb(220, 224, 230));
        }
        if self.manual && TICKS.load(Relaxed) < self.started + 8 * HZ {
            let hint = "Move the mouse or press a key to stop  |  Right arrow: next photo";
            let px = 15;
            c.text_face((w - x - width(hint, Face::Sans, px)) * 64, x + 10, hint, Face::Sans, px, rgb(200, 204, 210));
        }
    }

    /// No photos (or "Show the clock"): a big clock that drifts a little every minute so
    /// it never sits in one place for long.
    fn draw_clock_screen(&self, c: &mut Canvas, s: &Settings) {
        let (w, h) = (c.w as i32, c.h as i32);
        for y in 0..h {
            let col = crate::gfx::mix(rgb(16, 22, 34), rgb(30, 40, 62), (y * 255 / h.max(1)) as u32);
            c.fill_rect(0, y, w, 1, col);
        }
        let big = (h / 5).clamp(56, 220) as u16;
        let small = (big / 4).max(18);
        let m = self.time.1 as i32;
        let dx = ((m * 37) % 21 - 10) * w / 60;
        let dy = ((m * 53) % 13 - 6) * h / 50;
        let clock = s.clock_text(self.time.0, self.time.1);
        let cx = w / 2 + dx;
        let base = h / 2 + dy;
        c.text_face((cx - width(&clock, Face::Sans, big) / 2) * 64, base, &clock, Face::Sans, big, rgb(236, 240, 246));
        let date = format!("{} {}", crate::settings::long_date_text(self.date.0, self.date.1, self.date.2), self.date.0);
        c.text_face((cx - width(&date, Face::Sans, small) / 2) * 64, base + small as i32 * 2, &date, Face::Sans, small, rgb(170, 180, 196));
        if self.manual && !self.clock_only {
            let msg = "Your photos show here once there are some in Solon (use Import).";
            c.text_face((w / 2 - width(msg, Face::Sans, 16) / 2) * 64, h - 40, msg, Face::Sans, 16, rgb(150, 160, 176));
        }
    }
}

// ---- getting photos ready ----------------------------------------------------------------

/// The photo fitted to the screen, over a blurred, darkened copy of itself filling the rest.
fn prepare(img: &Image, turn: u8, w: usize, h: usize) -> Vec<u32> {
    let sideways = matches!(turn, 5..=8);
    let (iw, ih) = if sideways { (img.h, img.w) } else { (img.w, img.h) };
    // Background: a tiny copy, enlarged to cover the screen (that's what blurs it).
    let tiny = soften(&orient(&img.shrink_to_fit(24, 24), turn));
    let (tw, th) = (tiny.w as u64, tiny.h as u64);
    // Part of the tiny copy with the screen's shape (16.16 fixed point).
    let (rw, rh) = if tw * h as u64 >= th * w as u64 { (th * w as u64 * 65536 / h as u64, th * 65536) } else { (tw * 65536, tw * h as u64 * 65536 / w as u64) };
    let mut out = sample(&tiny, ((tw << 16) - rw) / 2, ((th << 16) - rh) / 2, rw, rh, w, h);
    for p in out.iter_mut() {
        *p = 0xFF00_0000 | ((*p >> 1 & 0x7F7F7F) - (*p >> 3 & 0x1F1F1F)); // about 3/8 as bright
    }
    // The photo itself, as big as fits (small ones enlarged up to 2x).
    let (fw, fh) = if iw * h >= ih * w { (w, (ih * w / iw).max(1)) } else { ((iw * h / ih).max(1), h) };
    let (fw, fh) = if fw > iw * MAX_UP / 2 { (iw * MAX_UP / 2, ih * MAX_UP / 2) } else { (fw, fh) };
    let photo = if fw <= iw {
        // Shrink first (on the photo as stored), then turn the small copy.
        let (sw, sh) = if sideways { (fh, fw) } else { (fw, fh) };
        orient(&img.shrink_to_fit(sw, sh), turn)
    } else {
        let o = orient(img, turn);
        let px = sample(&o, 0, 0, (o.w as u64) << 16, (o.h as u64) << 16, fw, fh);
        Image { w: fw, h: fh, px, opaque: o.opaque }
    };
    let (x0, y0) = ((w - photo.w.min(w)) / 2, (h - photo.h.min(h)) / 2);
    for y in 0..photo.h.min(h) {
        let src = &photo.px[y * photo.w..y * photo.w + photo.w.min(w)];
        let row = (y0 + y) * w + x0;
        if photo.opaque {
            out[row..row + src.len()].copy_from_slice(src);
        } else {
            for (d, &s) in out[row..row + src.len()].iter_mut().zip(src) {
                *d = mix_px(*d, s, s >> 24);
            }
        }
    }
    out
}

/// Bilinear resampling of a source rectangle (16.16 fixed point) into dw x dh pixels.
fn sample(src: &Image, rx: u64, ry: u64, rw: u64, rh: u64, dw: usize, dh: usize) -> Vec<u32> {
    let mut out = vec![0u32; dw * dh];
    let (sw, sh) = (src.w as i64, src.h as i64);
    let step_x = (rw / dw as u64) as i64;
    let step_y = (rh / dh as u64) as i64;
    for dy in 0..dh {
        // Centre of the destination pixel, back in the source, minus half a pixel.
        let fy = (ry as i64 + step_y * dy as i64 + step_y / 2 - 32768).clamp(0, (sh - 1) << 16);
        let (y0, wy) = ((fy >> 16) as usize, (fy & 0xFFFF) as u32 >> 8);
        let y1 = (y0 + 1).min(src.h - 1);
        for dx in 0..dw {
            let fx = (rx as i64 + step_x * dx as i64 + step_x / 2 - 32768).clamp(0, (sw - 1) << 16);
            let (x0, wx) = ((fx >> 16) as usize, (fx & 0xFFFF) as u32 >> 8);
            let x1 = (x0 + 1).min(src.w - 1);
            let top = mix_argb(src.px[y0 * src.w + x0], src.px[y0 * src.w + x1], wx);
            let bot = mix_argb(src.px[y1 * src.w + x0], src.px[y1 * src.w + x1], wx);
            out[dy * dw + dx] = mix_argb(top, bot, wy);
        }
    }
    out
}

/// a + (b - a) * t / 256, per colour channel.
#[inline]
fn mix_px(a: u32, b: u32, t: u32) -> u32 {
    let u = 256 - t;
    let rb = ((a & 0xFF00FF) * u + (b & 0xFF00FF) * t) >> 8 & 0xFF00FF;
    let g = ((a & 0x00FF00) * u + (b & 0x00FF00) * t) >> 8 & 0x00FF00;
    0xFF00_0000 | rb | g
}

fn blend(out: &mut [u32], a: &[u32], b: &[u32], t: u32) {
    for ((o, &x), &y) in out.iter_mut().zip(a).zip(b) {
        *o = mix_px(x, y, t);
    }
}

fn fade_in(out: &mut [u32], b: &[u32], t: u32) {
    for (o, &y) in out.iter_mut().zip(b) {
        *o = mix_px(0, y, t);
    }
}

/// Turn a photo the way its camera said it was held (EXIF orientation 1-8).
fn orient(img: &Image, turn: u8) -> Image {
    if turn <= 1 || turn > 8 {
        return Image { w: img.w, h: img.h, px: img.px.clone(), opaque: img.opaque };
    }
    let (w, h) = (img.w, img.h);
    let sideways = turn >= 5;
    let (ow, oh) = if sideways { (h, w) } else { (w, h) };
    let mut px = vec![0u32; w * h];
    for y in 0..h {
        for x in 0..w {
            let (nx, ny) = match turn {
                2 => (w - 1 - x, y),
                3 => (w - 1 - x, h - 1 - y),
                4 => (x, h - 1 - y),
                5 => (y, x),
                6 => (h - 1 - y, x),
                7 => (h - 1 - y, w - 1 - x),
                _ => (y, w - 1 - x),
            };
            px[ny * ow + nx] = img.px[y * w + x];
        }
    }
    Image { w: ow, h: oh, px, opaque: img.opaque }
}

/// The EXIF orientation of a JPEG (1 = upright), from its APP1 "Exif" block.
pub fn exif_orientation(b: &[u8]) -> u8 {
    if !b.starts_with(&[0xFF, 0xD8]) {
        return 1;
    }
    let mut i = 2;
    while i + 4 <= b.len() && b[i] == 0xFF {
        let marker = b[i + 1];
        let len = u16::from_be_bytes([b[i + 2], b[i + 3]]) as usize;
        if marker == 0xDA || len < 2 {
            break; // image data: no EXIF before it
        }
        let seg = &b[(i + 4).min(b.len())..(i + 2 + len).min(b.len())];
        if marker == 0xE1 && seg.starts_with(b"Exif\0\0") {
            return tiff_orientation(&seg[6..]).unwrap_or(1);
        }
        i += 2 + len;
    }
    1
}

fn tiff_orientation(t: &[u8]) -> Option<u8> {
    let le = match t.get(0..2)? {
        b"II" => true,
        b"MM" => false,
        _ => return None,
    };
    let u16_at = |o: usize| -> Option<u16> {
        let s = t.get(o..o + 2)?;
        Some(if le { u16::from_le_bytes([s[0], s[1]]) } else { u16::from_be_bytes([s[0], s[1]]) })
    };
    let u32_at = |o: usize| -> Option<u32> {
        let s = t.get(o..o + 4)?;
        Some(if le { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) })
    };
    let ifd = u32_at(4)? as usize;
    let n = u16_at(ifd)? as usize;
    for k in 0..n.min(200) {
        let e = ifd + 2 + k * 12;
        if u16_at(e)? == 0x0112 {
            let v = u16_at(e + 8)?;
            return (1..=8).contains(&v).then_some(v as u8);
        }
    }
    None
}

// ---- the Photo Frame app (start it, and the screensaver settings) ------------------------

pub enum FrameAction {
    None,
    Change(Settings),
    Start,
}

pub struct FrameApp {
    /// Photos in Solon storage (counted when the window opens).
    pub photos: usize,
}

const INK: u32 = rgb(28, 32, 40);
const DIM: u32 = rgb(106, 112, 124);
const ON: u32 = rgb(46, 134, 222);
const OFF: u32 = rgb(230, 233, 238);

fn choice(c: &mut Canvas, b: Rect, label: &str, on: bool) {
    c.round_rect(b.x, b.y, b.w, b.h, 8, if on { ON } else { OFF });
    c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, if on { rgb(255, 255, 255) } else { INK });
}

impl FrameApp {
    pub fn new() -> FrameApp {
        FrameApp { photos: 0 }
    }

    fn start_btn(r: Rect) -> Rect {
        Rect::new(r.x + 24, r.y + 110, 260, 48)
    }
    fn row_y(r: Rect, i: i32) -> i32 {
        r.y + 228 + i * 70
    }
    /// The buttons of option row `i` (0 what, 1 wait, 2 photo time, 3 order).
    fn buttons(r: Rect, i: i32) -> Vec<(usize, Rect)> {
        let (n, bw) = match i {
            0 => (SAVERS.len(), 150),
            1 => (SAVER_WAIT.len(), 106),
            2 => (SLIDE_TIME.len(), 110),
            _ => (2, 130),
        };
        let x0 = r.x + 24 + 200;
        let bw = bw.min((r.w - 224 - 24) / n as i32 - 8);
        (0..n).map(|k| (k, Rect::new(x0 + k as i32 * (bw + 8), Self::row_y(r, i) + 22, bw, 36))).collect()
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect, s: &Settings) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(250, 250, 252));
        c.text(r.x + 24, r.y + 20, "Photo Frame", Size::Large, true, INK);
        let about = if self.photos > 0 {
            format!("Shows your {} photos full screen, one after another, with the time and date.", crate::backup::count(self.photos))
        } else {
            String::from("Shows your photos full screen with the time and date. Import some photos first.")
        };
        c.text(r.x + 24, r.y + 68, &crate::photos::ellipsize(&about, r.w - 48, Size::Small, false), Size::Small, false, DIM);
        let b = Self::start_btn(r);
        c.round_rect(b.x, b.y, b.w, b.h, 10, ON);
        c.text_centered(b.x + b.w / 2, b.y + 14, "Start the photo frame", Size::Small, true, rgb(255, 255, 255));
        c.text(b.x + b.w + 18, b.y + 14, "Move the mouse or press a key to stop it.", Size::Small, false, DIM);
        c.fill_rect(r.x + 24, r.y + 184, r.w - 48, 1, rgb(224, 228, 234));
        c.text(r.x + 24, r.y + 196, "Screensaver: when the computer isn't used for a while", Size::Small, true, INK);
        let labels = ["Show", "Start after", "Each photo for", "Order"];
        for i in 0..4 {
            c.text(r.x + 24, Self::row_y(r, i) + 30, labels[i as usize], Size::Small, true, DIM);
            let dim = i > 0 && s.saver == 2 || (i >= 2 && s.saver == 1);
            for (k, b) in Self::buttons(r, i) {
                let (label, on) = match i {
                    0 => (SAVERS[k], s.saver == k),
                    1 => (SAVER_WAIT[k].0, s.saver_wait == k),
                    2 => (SLIDE_TIME[k].0, s.slide_time == k),
                    _ => (["Mixed up", "In order"][k], s.shuffle == (k == 0)),
                };
                if dim && !on {
                    c.round_rect(b.x, b.y, b.w, b.h, 8, rgb(244, 245, 247));
                    c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, rgb(190, 194, 200));
                } else {
                    choice(c, b, label, on);
                }
            }
        }
        let note = "Tip: leave the photo frame running to use this computer as a digital photo frame.";
        c.text(r.x + 24, Self::row_y(r, 4) + 16, &crate::photos::ellipsize(note, r.w - 48, Size::Small, false), Size::Small, false, DIM);
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, s: &Settings) -> FrameAction {
        if Self::start_btn(r).contains(x, y) {
            return FrameAction::Start;
        }
        let mut n = *s;
        for i in 0..4 {
            for (k, b) in Self::buttons(r, i) {
                if b.contains(x, y) {
                    match i {
                        0 => n.saver = k,
                        1 => n.saver_wait = k,
                        2 => n.slide_time = k,
                        _ => n.shuffle = k == 0,
                    }
                }
            }
        }
        if n != *s {
            FrameAction::Change(n)
        } else {
            FrameAction::None
        }
    }
}

/// Minutes of no use before the screensaver starts (None = never).
pub fn wait_ticks(s: &Settings) -> Option<u64> {
    (s.saver < 2).then(|| SAVER_WAIT[s.saver_wait.min(SAVER_WAIT.len() - 1)].1 * 60 * HZ)
}

/// Like `mix_px`, but keeps transparency.
#[inline]
fn mix_argb(a: u32, b: u32, t: u32) -> u32 {
    let u = 256 - t;
    let rb = ((a & 0xFF00FF) * u + (b & 0xFF00FF) * t) >> 8 & 0xFF00FF;
    let ag = ((a >> 8 & 0xFF00FF) * u + (b >> 8 & 0xFF00FF) * t) & 0xFF00FF00;
    ag | rb
}

/// Blur a tiny image a little (two 3x3 box passes), so it stretches into a smooth background.
fn soften(img: &Image) -> Image {
    let (w, h) = (img.w, img.h);
    // Transparent parts count as black.
    let mut cur: Vec<[u32; 3]> = img
        .px
        .iter()
        .map(|&p| {
            let a = p >> 24;
            [(p >> 16 & 0xFF) * a / 255, (p >> 8 & 0xFF) * a / 255, (p & 0xFF) * a / 255]
        })
        .collect();
    for _ in 0..2 {
        let mut next = cur.clone();
        for y in 0..h {
            for x in 0..w {
                let mut s = [0u32; 3];
                let mut n = 0;
                for yy in y.saturating_sub(1)..(y + 2).min(h) {
                    for xx in x.saturating_sub(1)..(x + 2).min(w) {
                        let p = cur[yy * w + xx];
                        s[0] += p[0];
                        s[1] += p[1];
                        s[2] += p[2];
                        n += 1;
                    }
                }
                next[y * w + x] = [s[0] / n, s[1] / n, s[2] / n];
            }
        }
        cur = next;
    }
    let px = cur.iter().map(|c| 0xFF00_0000 | c[0] << 16 | c[1] << 8 | c[2]).collect();
    Image { w, h, px, opaque: true }
}

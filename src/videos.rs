//! The Videos app: every video in Solon storage, on USB drives and on phones, with a picture
//! of each, and a player with sound, full screen and volume. Decoding is in `media`.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use crate::fs::DirEntry;
use crate::gfx::{rgb, Canvas, Rect, Size};
use crate::image::Image;
use crate::interrupts::{HZ, TICKS};
use crate::library::Storage;
use crate::media::dec::Pic;
use crate::media::Engine;
use crate::video::h264d::Frame;
use crate::movie::Io;
use crate::movieui::{LibIo, BOOT_FILES};
use crate::photos::{ellipsize, Source};
use crate::usb::Usb;

const BG: u32 = rgb(24, 26, 30);
const TILE: u32 = rgb(44, 48, 54);
const TEXT: u32 = rgb(232, 235, 239);
const DIM: u32 = rgb(150, 158, 168);
const ACCENT: u32 = rgb(231, 76, 60);
const BAR_H: i32 = 64;

struct Item {
    name: String,
    /// Where it is: "Solon storage", a drive's or phone's name.
    place: String,
    src: Source,
    size: u64,
    thumb: Option<Image>,
    dur_ms: Option<u64>,
    problem: Option<String>,
    tried: bool,
}

struct Play {
    from: u64,
    tick: u64,
    sound: bool,
    /// Where `from` plays in the sound output (frames since it started): the clock for the
    /// pictures is how far the output has played since then, and the sound queued at any
    /// place in the output is the video's sound for that moment, so the two can't drift apart.
    base: u64,
    /// The clock waits for the picture: at the start and after a jump, until the picture
    /// for `from` is decoded (from the key frame before it), so sound and picture start together.
    held: bool,
}

struct Player {
    item: usize,
    file: Engine,
    pos: u64,
    dur: u64,
    play: Option<Play>,
    picture: Option<Image>,
    shown: Option<i64>,
    /// Dragging: 1 = the position, 2 = the volume.
    dragging: u8,
    /// What the time and the position bar show (seconds, bar pixels): redrawn when they change.
    last_bar: (u64, u64),
    /// A big pause or play sign over the picture after a click on it (playing?, until tick).
    flash: Option<(bool, u64)>,
    /// The pictures are decoded on another core.
    elsewhere: bool,
}

/// How playback is keeping up, logged every 10 seconds while a video plays.
#[derive(Default)]
struct Stats {
    since_us: u64,
    decode_us: u64,
    convert_us: u64,
    pictures: u64,
    /// Ticks where the picture wasn't ready in time.
    late: u64,
}

pub enum VideosAction {
    None,
    Redraw,
}

pub struct VideosApp {
    items: Vec<Item>,
    scroll: i32,
    player: Option<Player>,
    last_click: Option<(usize, u64)>,
    body: Rect,
    /// Volume, 0-100.
    volume: u32,
    /// Filling the whole screen; the controls show until this tick (after the mouse moves).
    fullscreen: bool,
    controls_until: u64,
    last_press: u64,
    stats: Stats,
    /// What changed since the last look (for the desktop's video-only redraws).
    dirty: Option<Rect>,
}

/// File names Solon tries as videos (what's inside decides whether it can play them).
pub fn is_video(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".mp4", ".mov", ".m4v", ".3gp", ".3g2", ".avi", ".mkv", ".webm", ".mpg", ".mpeg", ".vob", ".ts", ".m2ts", ".mts", ".m2t", ".wmv", ".asf", ".flv", ".divx", ".xvid", ".ogv", ".mod", ".tod"]
        .iter()
        .any(|e| n.ends_with(e))
}

fn time_text(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn open_file(io: &mut dyn Io, src: &Source, size: u64) -> Result<Engine, String> {
    let mut read = |off: u64, len: usize| io.read(src, off, len);
    Engine::open(&mut read, size)
}

/// The picture at `ms` as an image of w x h (None if unchanged since `shown`).
fn picture(e: &mut Engine, io: &mut dyn Io, src: &Source, ms: u64, w: usize, h: usize, budget: usize, shown: Option<i64>) -> Option<(i64, Option<Image>)> {
    let mut read = |off: u64, len: usize| io.read(src, off, len);
    let (id, px) = e.picture(&mut read, ms, w, h, budget, shown)?;
    Some((id, px.map(|px| Image { w, h, px: px.iter().map(|v| v | 0xFF00_0000).collect(), opaque: true })))
}

/// Top the sound output up with the video's sound (at least `min` frames at a time, so it
/// isn't done in tiny pieces): what's queued at each place in the output is the sound for
/// that moment of the video, counted from `pl.base`.
fn queue_sound(e: &mut Engine, read: &mut crate::media::Read, pl: &Play, volume: u32, min: usize) {
    let want = crate::hda::wanted(crate::hda::VIDEOS);
    if want == 0 || want < min {
        return;
    }
    let at = crate::hda::write_pos();
    let start = pl.from * 48 + at.saturating_sub(pl.base);
    let mut pcm = e.sound(read, start, want);
    // Volume, on a curve that sounds even.
    let g = (volume * volume) as i32; // 0..10000
    for s in pcm.iter_mut() {
        *s = (*s as i32 * g / 10_000) as i16;
    }
    crate::hda::queue(crate::hda::VIDEOS, &pcm);
}

/// A raw pointer that may be shared between cores (each writes its own rows).
#[derive(Clone, Copy)]
struct Rows(*mut u32);
unsafe impl Send for Rows {}
unsafe impl Sync for Rows {}

/// A YUV picture at w x h for the screen (0xFFRRGGBB): brightness scaled smoothly (bilinear),
/// colour from the nearest sample, in bands of rows on every core.
fn to_screen(f: &Frame, w: usize, h: usize) -> Vec<u32> {
    let mut out = vec![0u32; w * h];
    if f.width == 0 || f.height == 0 || w == 0 || h == 0 {
        return out;
    }
    let cs = f.stride / 2;
    // Fixed-point YCbCr -> RGB (x1024), as Frame::to_rgb.
    let (cr_r, cb_g, cr_g, cb_b) = if f.bt709 { (1613, 192, 479, 1900) } else { (1436, 352, 731, 1815) };
    let (ys, yo) = if f.full_range { (1024, 0) } else { (1192, 16) };
    let (cr_r, cb_g, cr_g, cb_b) = if f.full_range { (cr_r, cb_g, cr_g, cb_b) } else { (cr_r * 1167 / 1024, cb_g * 1167 / 1024, cr_g * 1167 / 1024, cb_b * 1167 / 1024) };
    // Where each output column and row samples the picture: index and weight of the next (0-255).
    let map = |o: usize, n_out: usize, n_in: usize| -> (usize, u32) {
        let pos = (((2 * o + 1) * n_in) << 16) / (2 * n_out);
        let pos = pos.saturating_sub(1 << 15);
        let i = (pos >> 16).min(n_in - 1);
        (i, if i + 1 < n_in { ((pos >> 8) & 0xFF) as u32 } else { 0 })
    };
    let xs: Vec<(usize, u32)> = (0..w).map(|o| map(o, w, f.width)).collect();
    let yss: Vec<(usize, u32)> = (0..h).map(|o| map(o, h, f.height)).collect();
    let clip = |v: i32| v.clamp(0, 255) as u32;
    let rows = Rows(out.as_mut_ptr());
    let bands = (crate::smp::threads() * 2).min(h.div_ceil(16)).max(1);
    let per = h.div_ceil(bands);
    let (xs, yss) = (&xs, &yss);
    crate::smp::run(bands, &|b| {
        let rows = rows;
        for oy in b * per..((b + 1) * per).min(h) {
            let (sy, wy) = yss[oy];
            let y0 = (f.crop_y + sy) * f.stride + f.crop_x;
            let y1 = (f.crop_y + (sy + 1).min(f.height - 1)) * f.stride + f.crop_x;
            let c0 = (f.crop_y + sy) / 2 * cs;
            for (ox, &(sx, wx)) in xs.iter().enumerate() {
                let sx1 = (sx + 1).min(f.width - 1);
                let top = f.y[y0 + sx] as u32 * (256 - wx) + f.y[y0 + sx1] as u32 * wx;
                let bot = f.y[y1 + sx] as u32 * (256 - wx) + f.y[y1 + sx1] as u32 * wx;
                let luma = ((top * (256 - wy) + bot * wy + (1 << 15)) >> 16) as i32;
                let yv = (luma - yo) * ys;
                let ci = c0 + (f.crop_x + sx) / 2;
                let (cb, cr) = (f.u[ci] as i32 - 128, f.v[ci] as i32 - 128);
                let r = (yv + cr_r * cr + 512) >> 10;
                let g = (yv - cb_g * cb - cr_g * cr + 512) >> 10;
                let bl = (yv + cb_b * cb + 512) >> 10;
                unsafe { *rows.0.add(oy * w + ox) = 0xFF00_0000 | clip(r) << 16 | clip(g) << 8 | clip(bl) };
            }
        }
    });
    out
}

/// The smallest rectangle around both.
fn union(a: Option<Rect>, b: Rect) -> Rect {
    match a {
        None => b,
        Some(a) => {
            let (x0, y0) = (a.x.min(b.x), a.y.min(b.y));
            let (x1, y1) = ((a.x + a.w).max(b.x + b.w), (a.y + a.h).max(b.y + b.h));
            Rect::new(x0, y0, x1 - x0, y1 - y0)
        }
    }
}

/// The engine's current picture at w x h for the screen.
fn current_image(e: &Engine, w: usize, h: usize) -> Option<Image> {
    let px = match e.current()? {
        (_, Pic::Yuv(f)) if e.rotation() == 0 => to_screen(f, w, h),
        _ => e.current_rgb(w, h)?.iter().map(|v| v | 0xFF00_0000).collect(),
    };
    Some(Image { w, h, px, opaque: true })
}

/// Videos on a USB drive or phone (a few folders deep, and not forever).
fn usb_videos(usb: &mut Usb, drive: u32, vol: usize, label: &str, phone: bool, out: &mut Vec<Item>) {
    let Some(root) = usb.source_root(drive, vol) else { return };
    let mut stack = vec![(root, 0u32)];
    let mut seen = 0;
    while let Some((dir, depth)) = stack.pop() {
        let Ok(list) = usb.list(drive, vol, &dir) else { continue };
        for e in list {
            seen += 1;
            if seen > 20_000 {
                return;
            }
            if e.is_dir {
                let lower = e.name.to_ascii_lowercase();
                // Phones: only where photos and videos live (listing everything takes long).
                let wanted = !phone || depth > 0 || ["dcim", "movies", "videos", "download", "downloads", "pictures", "camera"].contains(&lower.as_str());
                if depth < 5 && wanted && !lower.starts_with('.') && lower != "android" {
                    stack.push((e, depth + 1));
                }
            } else if is_video(&e.name) && e.size > 0 {
                out.push(item(e.name.clone(), String::from(label), Source::Usb { drive, vol, entry: e.clone() }, e.size));
            }
        }
    }
}

fn item(name: String, place: String, src: Source, size: u64) -> Item {
    Item { name, place, src, size, thumb: None, dur_ms: None, problem: None, tried: false }
}

impl VideosApp {
    pub fn new() -> VideosApp {
        VideosApp { items: Vec::new(), scroll: 0, player: None, last_click: None, body: Rect::new(0, 0, 0, 0), volume: 80, fullscreen: false, controls_until: 0, last_press: 0, stats: Stats::default(), dirty: None }
    }

    pub fn playing(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.play.is_some())
    }

    /// The player fills the whole screen.
    pub fn fullscreen(&self) -> bool {
        self.fullscreen && self.player.is_some()
    }

    /// Find every video in Solon storage, on the boot drive, on USB drives and on phones.
    pub fn opened(&mut self, storage: &mut Storage, usb: &mut Usb) {
        if self.player.is_some() {
            return;
        }
        let mut items = Vec::new();
        for (name, bytes) in BOOT_FILES.lock().iter() {
            if is_video(name) {
                items.push(item(name.clone(), String::from("Solon stick"), Source::Memory(bytes), bytes.len() as u64));
            }
        }
        if storage.library.is_some() {
            if let Ok(root) = storage.root(usb) {
                let mut stack = vec![(root, 0u32)];
                while let Some((dir, depth)) = stack.pop() {
                    for e in storage.list(usb, &dir).unwrap_or_default() {
                        if e.is_dir {
                            let hidden = depth == 0 && (e.name.eq_ignore_ascii_case(crate::library::BIN) || e.name.eq_ignore_ascii_case("Originals"));
                            if !hidden && depth < 6 {
                                stack.push((e, depth + 1));
                            }
                        } else if is_video(&e.name) && e.size > 0 {
                            let size = e.size;
                            items.push(item(e.name.clone(), String::from("Solon storage"), Source::Library { dir: dir.clone(), entry: e }, size));
                        }
                    }
                }
            }
        }
        for s in usb.sources() {
            usb_videos(usb, s.drive, s.vol, &s.label, s.phone, &mut items);
        }
        // Keep pictures already made for videos that are still here.
        for it in items.iter_mut() {
            if let Some(old) = self.items.iter_mut().find(|o| o.name == it.name && o.size == it.size && o.place == it.place) {
                it.thumb = old.thumb.take();
                it.dur_ms = old.dur_ms;
                it.problem = old.problem.take();
                it.tried = old.tried;
            }
        }
        kprintln!("videos: {} videos found", items.len());
        self.items = items;
        self.scroll = 0;
    }

    // ---- layout -------------------------------------------------------------------------------

    fn tile(&self, r: Rect, i: usize) -> Rect {
        let cols = ((r.w - 20) / 230).max(1);
        let tw = (r.w - 20) / cols;
        Rect::new(r.x + 10 + (i as i32 % cols) * tw, r.y + 54 + (i as i32 / cols) * 210 - self.scroll, tw - 12, 198)
    }
    fn bar(r: Rect) -> Rect {
        Rect::new(r.x, r.y + r.h - BAR_H, r.w, BAR_H)
    }
    fn show_bar(&self) -> bool {
        !self.fullscreen || TICKS.load(Relaxed) < self.controls_until || !self.playing()
    }
    fn picture_rect(&self, r: Rect) -> Rect {
        let area = if self.fullscreen { r } else { Rect::new(r.x, r.y, r.w, r.h - BAR_H) };
        let (vw, vh) = self.player.as_ref().map(|p| (p.file.width, p.file.height)).unwrap_or((16, 9));
        let (vw, vh) = (vw as i32, vh as i32);
        let (w, h) = if area.w * vh > area.h * vw { (area.h * vw / vh, area.h) } else { (area.w, area.w * vh / vw) };
        Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w.max(2), h.max(2))
    }
    /// (back, play, seek bar, volume bar, full screen)
    fn controls(r: Rect) -> (Rect, Rect, Rect, Rect, Rect) {
        let b = Self::bar(r);
        let back = Rect::new(b.x + 12, b.y + 14, 90, 36);
        let play = Rect::new(b.x + 112, b.y + 14, 90, 36);
        let full = Rect::new(b.x + b.w - 12 - 120, b.y + 14, 120, 36);
        let vol = Rect::new(full.x - 24 - 110, b.y + 26, 110, 12);
        let seek = Rect::new(b.x + 222, b.y + 26, (vol.x - 40 - 150 - (b.x + 222)).max(60), 12);
        (back, play, seek, vol, full)
    }

    // ---- drawing --------------------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, r: Rect) {
        self.body = r;
        c.fill_rect(r.x, r.y, r.w, r.h, BG);
        if self.player.is_some() {
            self.draw_player(c, r);
            return;
        }
        c.text(r.x + 16, r.y + 14, "Videos", Size::Medium, true, TEXT);
        if self.items.is_empty() {
            c.text_centered(r.x + r.w / 2, r.y + r.h / 2 - 20, "No videos yet.", Size::Medium, true, TEXT);
            c.text_centered(r.x + r.w / 2, r.y + r.h / 2 + 12, "Videos in Solon storage, on USB drives and on phones show up here.", Size::Small, false, DIM);
            return;
        }
        c.set_clip(Rect::new(r.x, r.y + 48, r.w, r.h - 48));
        for i in 0..self.items.len() {
            let t = self.tile(r, i);
            if t.y + t.h < r.y || t.y > r.y + r.h {
                continue;
            }
            let it = &self.items[i];
            c.round_rect(t.x, t.y, t.w, t.h, 8, TILE);
            let pic = Rect::new(t.x + 6, t.y + 6, t.w - 12, 128);
            match &it.thumb {
                Some(img) => c.draw_image(img, pic.x + (pic.w - img.w as i32) / 2, pic.y + (pic.h - img.h as i32) / 2),
                None => c.fill_rect(pic.x, pic.y, pic.w, pic.h, rgb(16, 18, 20)),
            }
            let (cx, cy) = (pic.x + pic.w / 2, pic.y + pic.h / 2);
            c.circle_alpha(cx, cy, 18, 0, 150);
            c.triangle([(cx - 6, cy - 9), (cx - 6, cy + 9), (cx + 10, cy)], TEXT);
            c.text(t.x + 8, t.y + 140, &ellipsize(&it.name, t.w - 16, Size::Small, true), Size::Small, true, TEXT);
            let info = match (&it.problem, it.dur_ms) {
                (Some(_), _) => String::from("Can't play this one yet"),
                (None, Some(d)) => format!("{}  |  {}", time_text(d), it.place),
                _ => it.place.clone(),
            };
            c.text(t.x + 8, t.y + 160, &ellipsize(&info, t.w - 16, Size::Small, false), Size::Small, false, if it.problem.is_some() { ACCENT } else { DIM });
        }
        c.reset_clip();
    }

    fn draw_player(&mut self, c: &mut Canvas, r: Rect) {
        let pr = self.picture_rect(r);
        let show_bar = self.show_bar();
        let Some(p) = self.player.as_ref() else { return };
        match &p.picture {
            Some(img) => {
                // Black only around the picture (it's redrawn for every frame).
                let (ix, iy, iw, ih) = (pr.x + (pr.w - img.w as i32) / 2, pr.y + (pr.h - img.h as i32) / 2, img.w as i32, img.h as i32);
                c.fill_rect(r.x, r.y, r.w, iy - r.y, 0);
                c.fill_rect(r.x, iy + ih, r.w, r.y + r.h - iy - ih, 0);
                c.fill_rect(r.x, iy, ix - r.x, ih, 0);
                c.fill_rect(ix + iw, iy, r.x + r.w - ix - iw, ih, 0);
                c.draw_image(img, ix, iy);
            }
            None => c.fill_rect(r.x, r.y, r.w, r.h, 0),
        }
        if let Some((playing, _)) = p.flash {
            let (cx, cy) = (pr.x + pr.w / 2, pr.y + pr.h / 2);
            c.circle_alpha(cx, cy, 46, 0, 150);
            if playing {
                c.triangle([(cx - 14, cy - 22), (cx - 14, cy + 22), (cx + 24, cy)], TEXT);
            } else {
                c.round_rect(cx - 17, cy - 20, 12, 40, 3, TEXT);
                c.round_rect(cx + 5, cy - 20, 12, 40, 3, TEXT);
            }
        }
        let mut msg_y = r.y + r.h / 2 - 40;
        for m in [p.file.problem.as_deref(), p.file.sound_problem.as_deref()].into_iter().flatten() {
            c.text_centered(r.x + r.w / 2, msg_y, m, Size::Small, true, TEXT);
            msg_y += 26;
        }
        if p.file.problem.as_deref().is_some_and(|m| m.contains("HEVC")) {
            c.text_centered(r.x + r.w / 2, msg_y, "On an iPhone: Settings > Camera > Formats > Most Compatible, for new videos.", Size::Small, false, DIM);
        }
        if !show_bar {
            return;
        }
        let b = Self::bar(r);
        if self.fullscreen {
            c.fill_rect_alpha(b.x, b.y, b.w, b.h, rgb(20, 22, 26), 200);
        } else {
            c.fill_rect(b.x, b.y, b.w, b.h, rgb(30, 32, 36));
        }
        let (back, play, seek, vol, full) = Self::controls(r);
        for (rect, label, on) in [(back, "Back", false), (play, if p.play.is_some() { "Pause" } else { "Play" }, true), (full, if self.fullscreen { "Exit full" } else { "Full screen" }, false)] {
            c.round_rect(rect.x, rect.y, rect.w, rect.h, 8, if on { ACCENT } else { rgb(60, 64, 72) });
            c.text_centered(rect.x + rect.w / 2, rect.y + 10, label, Size::Small, true, TEXT);
        }
        c.round_rect(seek.x, seek.y, seek.w, seek.h, 6, rgb(70, 74, 82));
        let done = if p.dur > 0 { (seek.w as u64 * p.pos.min(p.dur) / p.dur) as i32 } else { 0 };
        c.round_rect(seek.x, seek.y, done.max(6), seek.h, 6, ACCENT);
        c.circle(seek.x + done, seek.y + seek.h / 2, 9, TEXT);
        c.text(seek.x + seek.w + 16, b.y + 22, &format!("{} / {}", time_text(p.pos), time_text(p.dur)), Size::Small, false, TEXT);
        // Volume: a speaker and a bar.
        let (sx, sy) = (vol.x - 24, vol.y + vol.h / 2);
        c.fill_rect(sx, sy - 4, 5, 8, TEXT);
        c.triangle([(sx + 4, sy - 4), (sx + 12, sy - 10), (sx + 12, sy + 10)], TEXT);
        c.triangle([(sx + 4, sy - 4), (sx + 12, sy + 10), (sx + 4, sy + 4)], TEXT);
        c.round_rect(vol.x, vol.y, vol.w, vol.h, 6, rgb(70, 74, 82));
        let vw = vol.w * self.volume as i32 / 100;
        c.round_rect(vol.x, vol.y, vw.max(6), vol.h, 6, TEXT);
        c.circle(vol.x + vw, vol.y + vol.h / 2, 8, TEXT);
        let name = ellipsize(&self.items[p.item].name, r.w / 2, Size::Small, true);
        c.round_rect_alpha(r.x + 10, r.y + 10, crate::gfx::text_width(&name, Size::Small, true) + 20, 30, 8, 0, 140);
        c.text(r.x + 20, r.y + 16, &name, Size::Small, true, TEXT);
    }

    // ---- playing ---------------------------------------------------------------------------------

    fn open_player(&mut self, i: usize, storage: &mut Storage, usb: &mut Usb) {
        let mut io = LibIo { storage, usb };
        let it = &mut self.items[i];
        match open_file(&mut io, &it.src, it.size) {
            Ok(f) => {
                let dur = f.duration_ms;
                it.dur_ms = Some(dur);
                it.problem = f.problem.clone();
                kprintln!("videos: playing \"{}\" ({} ms)", it.name, dur);
                // Decode on a core of its own if there's one free (the first core stays free
                // for the mouse, the sound and the screen).
                let mut f = f;
                let elsewhere = f.decode_elsewhere();
                kprintln!("videos: decoding the pictures {}", if elsewhere { "on a core of their own" } else { "on the first core" });
                self.player = Some(Player { item: i, file: f, pos: 0, dur, play: None, picture: None, shown: None, dragging: 0, last_bar: (u64::MAX, 0), flash: None, elsewhere });
                self.stats = Stats { since_us: crate::cpu::now_us(), ..Stats::default() };
                self.toggle();
            }
            Err(e) => {
                kprintln!("videos: can't open \"{}\": {}", it.name, e);
                it.problem = Some(e);
            }
        }
    }

    fn toggle(&mut self) {
        let Some(p) = self.player.as_mut() else { return };
        if let Some(pl) = p.play.take() {
            if pl.sound {
                crate::hda::stop(crate::hda::VIDEOS);
            }
            return;
        }
        if p.file.problem.is_some() && !p.file.has_sound() {
            return;
        }
        if p.pos + 200 >= p.dur {
            p.pos = 0;
        }
        // The clock (and the sound) start once the picture for this moment is ready.
        let sound = crate::hda::available() && p.file.has_sound();
        p.play = Some(Play { from: p.pos, tick: TICKS.load(Relaxed), sound, base: 0, held: true });
    }

    fn close_player(&mut self) {
        if let Some(p) = self.player.take() {
            if p.play.is_some_and(|pl| pl.sound) {
                crate::hda::stop(crate::hda::VIDEOS);
            }
        }
        self.fullscreen = false;
    }

    fn seek_to(&mut self, ms: u64) {
        let was_playing = self.playing();
        if was_playing {
            self.toggle();
        }
        if let Some(p) = self.player.as_mut() {
            p.pos = ms.min(p.dur);
            p.shown = None;
        }
        if was_playing {
            self.toggle();
        }
    }

    fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on && self.player.is_some();
        self.controls_until = TICKS.load(Relaxed) + 3 * HZ;
        if let Some(p) = self.player.as_mut() {
            p.picture = None; // made again at the new size
            p.shown = None;
        }
    }

    pub fn tick(&mut self, now: u64, storage: &mut Storage, usb: &mut Usb, visible: bool) -> VideosAction {
        if !visible && !self.fullscreen {
            return VideosAction::None;
        }
        let mut io = LibIo { storage, usb };
        let pic = self.picture_rect(self.body);
        let bar_was = self.show_bar();
        let volume = self.volume;
        let body = self.body;
        let mut dirty: Option<Rect> = None;
        // Another app (Music, the Sound Editor) took the sound: pause.
        if self.player.as_ref().and_then(|p| p.play.as_ref()).is_some_and(|pl| pl.sound && !pl.held && crate::hda::owner() != crate::hda::VIDEOS) {
            self.toggle();
        }
        let stats = &mut self.stats;
        if let Some(p) = self.player.as_mut() {
            let src = self.items[p.item].src.clone();
            let mut redraw = false;
            if let Some(pl) = p.play.as_mut().filter(|pl| !pl.held) {
                let elapsed = if pl.sound { crate::hda::played().saturating_sub(pl.base) * 1000 / 48000 } else { now.saturating_sub(pl.tick) * 1000 / HZ };
                if p.dragging != 1 {
                    p.pos = (pl.from + elapsed).min(p.dur);
                }
                if pl.sound {
                    let mut read = |off: u64, len: usize| io.read(&src, off, len);
                    queue_sound(&mut p.file, &mut read, pl, volume, 2048);
                }
                if p.pos >= p.dur {
                    if pl.sound {
                        crate::hda::stop(crate::hda::VIDEOS);
                    }
                    p.play = None;
                    redraw = true;
                }
            }
            if p.file.problem.is_none() && pic.w > 2 {
                let (vw, vh) = (p.file.width, p.file.height);
                let (w, h) = if pic.w as usize * vh > pic.h as usize * vw { (pic.h as usize * vw / vh, pic.h as usize) } else { (pic.w as usize, pic.w as usize * vh / vw) };
                let (w, h) = (w.max(2), h.max(2));
                // Decode towards the picture for now: a few pictures a tick while playing; at
                // the start, after a jump or when paused, as many as fit in about 30 ms (the
                // clock waits meanwhile, so the picture doesn't lag behind the sound).
                // (Decoding on another core: just take what's ready, without waiting.)
                let t0 = crate::cpu::now_us();
                let held = p.play.as_ref().is_none_or(|pl| pl.held);
                let mut ready;
                {
                    let mut read = |off: u64, len: usize| io.read(&src, off, len);
                    if held && !p.elsewhere {
                        loop {
                            ready = p.file.advance(&mut read, p.pos, 1);
                            if ready || crate::cpu::now_us() - t0 > 30_000 {
                                break;
                            }
                        }
                    } else {
                        ready = p.file.advance(&mut read, p.pos, if p.elsewhere { 0 } else { 3 });
                    }
                }
                stats.decode_us += crate::cpu::now_us() - t0;
                if !ready && !held {
                    stats.late += 1;
                }
                // The picture caught up: start the clock and the sound from here, with the
                // first sound queued at once (the clock counts from where it plays).
                if ready {
                    if let Some(pl) = p.play.as_mut().filter(|pl| pl.held) {
                        pl.held = false;
                        pl.from = p.pos;
                        pl.tick = now;
                        if pl.sound {
                            crate::hda::start(crate::hda::VIDEOS);
                            pl.base = crate::hda::write_pos();
                            let mut read = |off: u64, len: usize| io.read(&src, off, len);
                            queue_sound(&mut p.file, &mut read, pl, volume, 0);
                        }
                    }
                }
                // A new picture (or a new size): convert it, on every core.
                let same_size = p.picture.as_ref().is_some_and(|i| i.w == w && i.h == h);
                let id = p.file.current().map(|c| c.0);
                if id.is_some() && (id != p.shown || !same_size) {
                    let t1 = crate::cpu::now_us();
                    if let Some(img) = current_image(&p.file, w, h) {
                        p.picture = Some(img);
                        p.shown = id;
                        stats.pictures += 1;
                        redraw = true;
                        dirty = Some(pic);
                    }
                    stats.convert_us += crate::cpu::now_us() - t1;
                }
            }
            // The time and the position bar: redrawn when what they show changes.
            let (_, _, seek, _, _) = Self::controls(body);
            let bar = (p.pos / 1000, seek.w.max(1) as u64 * p.pos / p.dur.max(1));
            if bar != p.last_bar {
                p.last_bar = bar;
                redraw = true;
                dirty = Some(union(dirty, Self::bar(body)));
            }
            if p.flash.is_some_and(|f| now >= f.1) {
                p.flash = None;
                redraw = true;
                dirty = Some(union(dirty, pic));
            }
            // Every 10 seconds of playing: how it's keeping up.
            let t = crate::cpu::now_us();
            if p.play.is_some() && t >= stats.since_us + 10_000_000 {
                let n = stats.pictures.max(1);
                let (dec_us, dec_n) = p.file.take_decode_stats();
                kprintln!(
                    "videos: last 10 s: {} pictures; decoding {} ms a packet ({}), the first core spending {} ms a picture on reading and {} ms converting; {} late ticks",
                    stats.pictures, dec_us / dec_n.max(1) / 1000, if p.elsewhere { "on a core of its own" } else { "on the first core" }, stats.decode_us / n / 1000, stats.convert_us / n / 1000, stats.late
                );
                *stats = Stats { since_us: t, ..Stats::default() };
            } else if p.play.is_none() {
                stats.since_us = t;
                let _ = p.file.take_decode_stats();
            }
            if bar_was != self.show_bar() {
                redraw = true;
                dirty = Some(body);
            }
            self.dirty = if redraw { Some(dirty.unwrap_or(body)) } else { None };
            return if redraw { VideosAction::Redraw } else { VideosAction::None };
        }
        // Make one picture for the list.
        if let Some(i) = self.items.iter().position(|it| !it.tried) {
            let it = &mut self.items[i];
            it.tried = true;
            match open_file(&mut io, &it.src, it.size) {
                Ok(mut f) => {
                    it.dur_ms = Some(f.duration_ms);
                    it.problem = f.problem.clone();
                    if it.problem.is_none() {
                        let (vw, vh) = (f.width, f.height);
                        let (w, h) = crate::image::fit(vw, vh, 206, 128);
                        let (w, h) = if vw <= 206 && vh <= 128 { (vw, vh) } else { (w, h) };
                        let src = it.src.clone();
                        if let Some((_, Some(img))) = picture(&mut f, &mut io, &src, 0, w.max(2), h.max(2), 6, None) {
                            it.thumb = Some(img);
                        }
                    }
                }
                Err(e) => it.problem = Some(e),
            }
            return VideosAction::Redraw;
        }
        VideosAction::None
    }

    // ---- input -------------------------------------------------------------------------------------

    pub fn scroll_info(&self, r: Rect) -> Option<(Rect, i32, i32)> {
        if self.player.is_some() {
            return None;
        }
        let per = ((r.w - 20) / 230).max(1);
        let rows = (self.items.len() as i32 + per - 1) / per;
        Some((r, (rows * 210 + 60 - r.h).max(0), self.scroll))
    }
    pub fn set_scroll(&mut self, v: i32) {
        self.scroll = v.max(0);
    }

    pub fn on_wheel(&mut self, wheel: i8, r: Rect) -> bool {
        if self.player.is_some() {
            self.change_volume(wheel as i32 * 5);
            return true;
        }
        let rows = (self.items.len() as i32 + ((r.w - 20) / 230).max(1) - 1) / ((r.w - 20) / 230).max(1);
        let max = (rows * 210 + 60 - r.h).max(0);
        self.scroll = (self.scroll - wheel as i32 * 80).clamp(0, max);
        true
    }

    fn change_volume(&mut self, d: i32) {
        self.volume = (self.volume as i32 + d).clamp(0, 100) as u32;
        self.controls_until = TICKS.load(Relaxed) + 3 * HZ;
    }

    /// The mouse moved (full screen: show the controls for a while). True = redraw.
    pub fn on_move(&mut self) -> bool {
        if !self.fullscreen {
            return false;
        }
        let was = self.show_bar();
        self.controls_until = TICKS.load(Relaxed) + 3 * HZ;
        !was
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, storage: &mut Storage, usb: &mut Usb) -> VideosAction {
        self.body = r;
        if self.player.is_some() {
            let double = now.saturating_sub(self.last_press) < 40;
            self.last_press = now;
            let show_bar = self.show_bar();
            self.controls_until = now + 3 * HZ;
            let (back, play, seek, vol, full) = Self::controls(r);
            if show_bar && Self::bar(r).contains(x, y) {
                if back.contains(x, y) {
                    self.close_player();
                    return VideosAction::Redraw;
                }
                if play.contains(x, y) {
                    self.toggle();
                    return VideosAction::Redraw;
                }
                if full.contains(x, y) {
                    let on = !self.fullscreen;
                    self.set_fullscreen(on);
                    return VideosAction::Redraw;
                }
                if Rect::new(vol.x - 30, vol.y - 14, vol.w + 40, vol.h + 28).contains(x, y) {
                    self.volume = ((x - vol.x).clamp(0, vol.w) * 100 / vol.w.max(1)) as u32;
                    if let Some(p) = self.player.as_mut() {
                        p.dragging = 2;
                    }
                    return VideosAction::Redraw;
                }
                if Rect::new(seek.x, seek.y - 14, seek.w, seek.h + 28).contains(x, y) {
                    let dur = self.player.as_ref().map(|p| p.dur).unwrap_or(0);
                    self.seek_to(((x - seek.x).max(0) as u64 * dur / seek.w.max(1) as u64).min(dur));
                    if let Some(p) = self.player.as_mut() {
                        p.dragging = 1;
                    }
                    return VideosAction::Redraw;
                }
                return VideosAction::None;
            }
            // On the picture: a click pauses or plays (a big sign shows which), a double-click
            // switches full screen.
            if double {
                self.toggle(); // undo the first click's pause
                let on = !self.fullscreen;
                self.set_fullscreen(on);
                if let Some(p) = self.player.as_mut() {
                    p.flash = None;
                }
            } else {
                self.toggle();
                let playing = self.playing();
                if let Some(p) = self.player.as_mut() {
                    p.flash = Some((playing, now + HZ * 6 / 10));
                }
            }
            return VideosAction::Redraw;
        }
        for i in 0..self.items.len() {
            if self.tile(r, i).contains(x, y) && y > r.y + 48 {
                self.last_click = Some((i, now));
                self.open_player(i, storage, usb);
                return VideosAction::Redraw;
            }
        }
        VideosAction::None
    }

    pub fn on_drag(&mut self, x: i32, _y: i32, r: Rect) -> bool {
        let (_, _, seek, vol, _) = Self::controls(r);
        let Some(p) = self.player.as_mut() else { return false };
        match p.dragging {
            1 => {
                p.pos = ((x - seek.x).max(0) as u64 * p.dur / seek.w.max(1) as u64).min(p.dur);
                true
            }
            2 => {
                self.volume = ((x - vol.x).clamp(0, vol.w) * 100 / vol.w.max(1)) as u32;
                true
            }
            _ => false,
        }
    }

    pub fn on_release(&mut self) -> bool {
        let Some(p) = self.player.as_mut() else { return false };
        let d = p.dragging;
        p.dragging = 0;
        if d == 1 {
            let pos = p.pos;
            self.seek_to(pos);
        }
        d != 0
    }

    pub fn on_key(&mut self, sc: u8) -> Option<VideosAction> {
        if sc & 0x80 != 0 {
            return None;
        }
        self.player.as_ref()?;
        let pos = self.player.as_ref().map(|p| p.pos).unwrap_or(0);
        self.controls_until = TICKS.load(Relaxed) + 3 * HZ;
        match sc {
            0x39 | 0x1C => self.toggle(),
            0x01 if self.fullscreen => self.set_fullscreen(false),
            0x01 => self.close_player(),
            0x21 => {
                let on = !self.fullscreen;
                self.set_fullscreen(on);
            }
            0x4B => self.seek_to(pos.saturating_sub(10_000)),
            0x4D => self.seek_to(pos + 10_000),
            0x48 => self.change_volume(10),
            0x50 => self.change_volume(-10),
            _ => return None,
        }
        Some(VideosAction::Redraw)
    }

    pub fn closed(&mut self) {
        self.close_player();
    }

    /// What changed in the last tick (picture, controls), for the desktop's fast redraw.
    pub fn take_dirty(&mut self) -> Option<Rect> {
        self.dirty.take()
    }
}

#[allow(dead_code)]
fn _uses(_: DirEntry) {}

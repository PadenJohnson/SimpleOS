//! The Videos app: every video in Solon storage (phone MP4s, camera AVIs) with a picture
//! of each, and a player with sound.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use crate::gfx::{rgb, Canvas, Rect, Size};
use crate::image::Image;
use crate::interrupts::{HZ, TICKS};
use crate::library::Storage;
use crate::movie::Io;
use crate::movieui::{LibIo, BOOT_FILES};
use crate::photos::{ellipsize, Source};
use crate::usb::Usb;
use crate::video::avi::Avi;
use crate::video::mp4src::{frame_rgb, Mp4Source};

const BG: u32 = rgb(24, 26, 30);
const TILE: u32 = rgb(44, 48, 54);
const TEXT: u32 = rgb(232, 235, 239);
const DIM: u32 = rgb(150, 158, 168);
const ACCENT: u32 = rgb(231, 76, 60);

struct Item {
    name: String,
    src: Source,
    size: u64,
    thumb: Option<Image>,
    dur_ms: Option<u64>,
    problem: Option<&'static str>,
    tried: bool,
}

enum File {
    Mp4(Mp4Source),
    Avi(Avi),
}

struct Play {
    from: u64,
    tick: u64,
    sound: bool,
    /// 48 kHz frames queued since `from`.
    queued: u64,
}

struct Player {
    item: usize,
    file: File,
    pos: u64,
    dur: u64,
    play: Option<Play>,
    picture: Option<Image>,
    shown: Option<i64>,
    seeking: bool,
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
}

fn is_video(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".mp4", ".mov", ".m4v", ".3gp", ".avi"].iter().any(|e| n.ends_with(e))
}

fn time_text(ms: u64) -> String {
    let s = ms / 1000;
    if s >= 3600 {
        format!("{}:{:02}:{:02}", s / 3600, (s / 60) % 60, s % 60)
    } else {
        format!("{}:{:02}", s / 60, s % 60)
    }
}

fn open_file(io: &mut dyn Io, src: &Source, size: u64, name: &str) -> Result<File, &'static str> {
    let mut read = |off: u64, len: usize| io.read(src, off, len);
    if name.to_ascii_lowercase().ends_with(".avi") {
        Avi::parse(&mut read, size).map(File::Avi)
    } else {
        Mp4Source::open(&mut read, size).map(File::Mp4)
    }
}

impl File {
    fn duration(&self) -> u64 {
        match self {
            File::Mp4(m) => m.duration_ms,
            File::Avi(a) => a.duration_ms(),
        }
    }
    fn size(&self) -> (usize, usize) {
        match self {
            File::Mp4(m) => (m.width.max(1), m.height.max(1)),
            File::Avi(a) => (a.width.max(1) as usize, a.height.max(1) as usize),
        }
    }
    fn problem(&self) -> Option<&'static str> {
        match self {
            File::Mp4(m) => m.problem,
            File::Avi(_) => None,
        }
    }
    /// The picture at `ms`, at w x h; also an id of the frame (to skip redrawing).
    fn picture(&mut self, io: &mut dyn Io, src: &Source, ms: u64, w: usize, h: usize, budget: usize) -> Option<(i64, Vec<u32>)> {
        let mut read = |off: u64, len: usize| io.read(src, off, len);
        match self {
            File::Mp4(m) => {
                let rot = m.rotation;
                let px = frame_rgb(m.frame_at(&mut read, ms, budget)?, rot, w, h);
                Some((m.current_pts().unwrap_or(0), px))
            }
            File::Avi(a) => {
                let n = ((ms * a.rate as u64) / (1000 * a.scale.max(1) as u64)) as usize;
                let n = n.min(a.frames.len().saturating_sub(1));
                let jpeg = a.frame_jpeg(&mut read, n)?;
                let img = crate::image::decode(&jpeg).ok()?;
                let img = img.shrink_to_fit(w, h);
                let mut px = vec![0u32; w * h];
                for y in 0..h {
                    for x in 0..w {
                        px[y * w + x] = img.px[(y * img.h / h).min(img.h - 1) * img.w + (x * img.w / w).min(img.w - 1)] & 0xFF_FFFF;
                    }
                }
                Some((n as i64, px))
            }
        }
    }
    /// Sound at 48 kHz: `n` stereo frames starting `from` frames (48 kHz) into the video.
    fn sound(&mut self, io: &mut dyn Io, src: &Source, from: u64, n: usize) -> Vec<i16> {
        let mut read = |off: u64, len: usize| io.read(src, off, len);
        let (rate, start, pcm) = match self {
            File::Mp4(m) => {
                if m.audio_rate == 0 {
                    return vec![0; n * 2];
                }
                let rate = m.audio_rate as u64;
                let s0 = from * rate / 48000;
                let need = n as u64 * rate / 48000 + 3;
                (rate, s0, m.audio(&mut read, s0, need as usize))
            }
            File::Avi(a) => match &a.sound {
                Some(s) => {
                    let rate = s.rate.max(1) as u64;
                    let s0 = from * rate / 48000;
                    let need = n as u64 * rate / 48000 + 3;
                    (rate, s0, s.read_frames(&mut read, s0, need as usize))
                }
                None => return vec![0; n * 2],
            },
        };
        // Linear resampling to 48 kHz.
        let mut out = Vec::with_capacity(n * 2);
        for k in 0..n as u64 {
            let p = (from + k) * rate * 256 / 48000;
            let i = ((p >> 8) - start) as usize;
            let f = (p & 255) as i32;
            for ch in 0..2 {
                let a = *pcm.get(i * 2 + ch).unwrap_or(&0) as i32;
                let b = *pcm.get(i * 2 + 2 + ch).unwrap_or(&(a as i16)) as i32;
                out.push(((a * (256 - f) + b * f) >> 8) as i16);
            }
        }
        out
    }
}

impl VideosApp {
    pub fn new() -> VideosApp {
        VideosApp { items: Vec::new(), scroll: 0, player: None, last_click: None, body: Rect::new(0, 0, 0, 0) }
    }

    pub fn playing(&self) -> bool {
        self.player.as_ref().is_some_and(|p| p.play.is_some())
    }

    /// Find every video in Solon storage and on the boot drive.
    pub fn opened(&mut self, storage: &mut Storage, usb: &mut Usb) {
        if self.player.is_some() {
            return;
        }
        let mut items = Vec::new();
        for (name, bytes) in BOOT_FILES.lock().iter() {
            if is_video(name) {
                items.push(Item { name: name.clone(), src: Source::Memory(bytes), size: bytes.len() as u64, thumb: None, dur_ms: None, problem: None, tried: false });
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
                            items.push(Item { name: e.name.clone(), src: Source::Library { dir: dir.clone(), entry: e }, size, thumb: None, dur_ms: None, problem: None, tried: false });
                        }
                    }
                }
            }
        }
        items.sort_by(|a, b| crate::fs::natural_cmp(&a.name, &b.name));
        // Keep pictures already made.
        for it in items.iter_mut() {
            if let Some(old) = self.items.iter_mut().find(|o| o.name == it.name && o.size == it.size) {
                it.thumb = old.thumb.take();
                it.dur_ms = old.dur_ms;
                it.problem = old.problem;
                it.tried = old.tried;
            }
        }
        self.items = items;
        kprintln!("videos: {} found", self.items.len());
    }

    // ---- layout -------------------------------------------------------------------------------

    fn tile(&self, r: Rect, i: usize) -> Rect {
        let cols = ((r.w - 20) / 230).max(1);
        let tw = (r.w - 20) / cols;
        Rect::new(r.x + 10 + (i as i32 % cols) * tw, r.y + 54 + (i as i32 / cols) * 190 - self.scroll, tw - 12, 178)
    }
    fn bar(r: Rect) -> Rect {
        Rect::new(r.x, r.y + r.h - 64, r.w, 64)
    }
    fn picture_rect(&self, r: Rect) -> Rect {
        let area = Rect::new(r.x, r.y, r.w, r.h - 64);
        let (vw, vh) = self.player.as_ref().map(|p| p.file.size()).unwrap_or((16, 9));
        let (vw, vh) = (vw as i32, vh as i32);
        let (w, h) = if area.w * vh > area.h * vw { (area.h * vw / vh, area.h) } else { (area.w, area.w * vh / vw) };
        Rect::new(area.x + (area.w - w) / 2, area.y + (area.h - h) / 2, w.max(2), h.max(2))
    }
    fn controls(r: Rect) -> (Rect, Rect, Rect) {
        let b = Self::bar(r);
        let back = Rect::new(b.x + 12, b.y + 14, 90, 36);
        let play = Rect::new(b.x + 112, b.y + 14, 90, 36);
        let seek = Rect::new(b.x + 222, b.y + 26, b.w - 222 - 160, 12);
        (back, play, seek)
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
            c.text_centered(r.x + r.w / 2, r.y + r.h / 2 + 12, "Phone videos come in with Import; the Video Editor saves here too.", Size::Small, false, DIM);
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
            // Play badge.
            let (cx, cy) = (pic.x + pic.w / 2, pic.y + pic.h / 2);
            c.circle_alpha(cx, cy, 18, 0, 150);
            c.triangle([(cx - 6, cy - 9), (cx - 6, cy + 9), (cx + 10, cy)], TEXT);
            let name = ellipsize(&it.name, t.w - 16, Size::Small, true);
            c.text(t.x + 8, t.y + 140, &name, Size::Small, true, TEXT);
            let info = match (it.problem, it.dur_ms) {
                (Some(p), _) => String::from(if p.contains("HEVC") { "HEVC - can't play yet" } else { "Can't play" }),
                (None, Some(d)) => time_text(d),
                _ => String::new(),
            };
            c.text(t.x + 8, t.y + 158, &info, Size::Small, false, if it.problem.is_some() { ACCENT } else { DIM });
        }
        c.reset_clip();
    }

    fn draw_player(&mut self, c: &mut Canvas, r: Rect) {
        let pr = self.picture_rect(r);
        let Some(p) = self.player.as_ref() else { return };
        c.fill_rect(r.x, r.y, r.w, r.h, 0);
        if let Some(img) = &p.picture {
            c.draw_image(img, pr.x + (pr.w - img.w as i32) / 2, pr.y + (pr.h - img.h as i32) / 2);
        }
        if let Some(problem) = p.file.problem() {
            c.text_centered(r.x + r.w / 2, r.y + r.h / 2 - 40, problem, Size::Small, true, TEXT);
            if problem.contains("HEVC") {
                c.text_centered(r.x + r.w / 2, r.y + r.h / 2 - 14, "On an iPhone: Settings > Camera > Formats > Most Compatible, for new videos.", Size::Small, false, DIM);
            }
        }
        let b = Self::bar(r);
        c.fill_rect(b.x, b.y, b.w, b.h, rgb(30, 32, 36));
        let (back, play, seek) = Self::controls(r);
        for (rect, label) in [(back, "Back"), (play, if p.play.is_some() { "Pause" } else { "Play" })] {
            c.round_rect(rect.x, rect.y, rect.w, rect.h, 8, if label == "Back" { rgb(60, 64, 72) } else { ACCENT });
            c.text_centered(rect.x + rect.w / 2, rect.y + 10, label, Size::Small, true, TEXT);
        }
        c.round_rect(seek.x, seek.y, seek.w, seek.h, 6, rgb(70, 74, 82));
        let done = if p.dur > 0 { (seek.w as u64 * p.pos.min(p.dur) / p.dur) as i32 } else { 0 };
        c.round_rect(seek.x, seek.y, done.max(6), seek.h, 6, ACCENT);
        c.circle(seek.x + done, seek.y + seek.h / 2, 9, TEXT);
        c.text(seek.x + seek.w + 16, b.y + 22, &format!("{} / {}", time_text(p.pos), time_text(p.dur)), Size::Small, false, TEXT);
        let name = ellipsize(&self.items[p.item].name, r.w / 2, Size::Small, true);
        c.round_rect_alpha(r.x + 10, r.y + 10, crate::gfx::text_width(&name, Size::Small, true) + 20, 30, 8, 0, 140);
        c.text(r.x + 20, r.y + 16, &name, Size::Small, true, TEXT);
    }

    // ---- playing ---------------------------------------------------------------------------------

    fn open_player(&mut self, i: usize, storage: &mut Storage, usb: &mut Usb) {
        let mut io = LibIo { storage, usb };
        let it = &mut self.items[i];
        match open_file(&mut io, &it.src, it.size, &it.name) {
            Ok(f) => {
                let dur = f.duration();
                it.dur_ms = Some(dur);
                it.problem = f.problem();
                kprintln!("videos: playing \"{}\" ({} ms)", it.name, dur);
                self.player = Some(Player { item: i, file: f, pos: 0, dur, play: None, picture: None, shown: None, seeking: false });
                self.toggle();
            }
            Err(e) => {
                it.problem = Some(e);
                kprintln!("videos: can't open \"{}\": {}", it.name, e);
            }
        }
    }

    fn toggle(&mut self) {
        let Some(p) = self.player.as_mut() else { return };
        if let Some(pl) = p.play.take() {
            if pl.sound {
                crate::hda::stop();
            }
            return;
        }
        if p.file.problem().is_some() && !matches!(p.file, File::Mp4(ref m) if m.audio_rate > 0) {
            return;
        }
        if p.pos + 200 >= p.dur {
            p.pos = 0;
        }
        let sound = crate::hda::available();
        if sound {
            crate::hda::start();
        }
        p.play = Some(Play { from: p.pos, tick: TICKS.load(Relaxed), sound, queued: 0 });
    }

    fn close_player(&mut self) {
        if let Some(p) = self.player.take() {
            if p.play.is_some_and(|pl| pl.sound) {
                crate::hda::stop();
            }
        }
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

    pub fn tick(&mut self, now: u64, storage: &mut Storage, usb: &mut Usb, visible: bool) -> VideosAction {
        if !visible {
            return VideosAction::None;
        }
        let mut io = LibIo { storage, usb };
        let pic = self.picture_rect(self.body);
        if let Some(p) = self.player.as_mut() {
            let src = self.items[p.item].src.clone();
            let mut redraw = false;
            if let Some(pl) = p.play.as_mut() {
                let elapsed = if pl.sound { crate::hda::played() * 1000 / 48000 } else { (now - pl.tick) * 1000 / HZ };
                p.pos = (pl.from + elapsed).min(p.dur);
                if pl.sound {
                    let want = crate::hda::wanted();
                    if want >= 2048 {
                        let start = pl.from * 48 + pl.queued;
                        let pcm = p.file.sound(&mut io, &src, start, want);
                        crate::hda::queue(&pcm);
                        pl.queued += want as u64;
                    }
                }
                redraw = true;
                if p.pos >= p.dur {
                    if pl.sound {
                        crate::hda::stop();
                    }
                    p.play = None;
                }
            }
            if p.file.problem().is_none() && pic.w > 2 {
                let (vw, vh) = p.file.size();
                let (w, h) = if pic.w as usize * vh > pic.h as usize * vw { (pic.h as usize * vw / vh, pic.h as usize) } else { (pic.w as usize, pic.w as usize * vh / vw) };
                let budget = if p.play.is_some() { 3 } else { 400 };
                if let Some((id, px)) = p.file.picture(&mut io, &src, p.pos, w.max(2), h.max(2), budget) {
                    if p.shown != Some(id) || p.picture.as_ref().is_none_or(|i| i.w != w.max(2)) {
                        p.shown = Some(id);
                        p.picture = Some(Image { w: w.max(2), h: h.max(2), px: px.iter().map(|v| v | 0xFF00_0000).collect(), opaque: true });
                        redraw = true;
                    }
                }
            }
            return if redraw { VideosAction::Redraw } else { VideosAction::None };
        }
        // Make one picture for the list.
        if let Some(i) = self.items.iter().position(|it| !it.tried) {
            let it = &mut self.items[i];
            it.tried = true;
            match open_file(&mut io, &it.src, it.size, &it.name) {
                Ok(mut f) => {
                    it.dur_ms = Some(f.duration());
                    it.problem = f.problem();
                    if it.problem.is_none() {
                        let (vw, vh) = f.size();
                        let (w, h) = crate::image::fit(vw, vh, 206, 128);
                        let (w, h) = if vw <= 206 && vh <= 128 { (vw, vh) } else { (w, h) };
                        let src = it.src.clone();
                        if let Some((_, px)) = f.picture(&mut io, &src, 0, w.max(2), h.max(2), 4) {
                            it.thumb = Some(Image { w: w.max(2), h: h.max(2), px: px.iter().map(|v| v | 0xFF00_0000).collect(), opaque: true });
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
        Some((r, (rows * 190 + 60 - r.h).max(0), self.scroll))
    }
    pub fn set_scroll(&mut self, v: i32) {
        self.scroll = v.max(0);
    }

    pub fn on_wheel(&mut self, wheel: i8, r: Rect) -> bool {
        if self.player.is_some() {
            return false;
        }
        let rows = (self.items.len() as i32 + ((r.w - 20) / 230).max(1) - 1) / ((r.w - 20) / 230).max(1);
        let max = (rows * 190 + 60 - r.h).max(0);
        self.scroll = (self.scroll - wheel as i32 * 80).clamp(0, max);
        true
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, storage: &mut Storage, usb: &mut Usb) -> VideosAction {
        self.body = r;
        if self.player.is_some() {
            let (back, play, seek) = Self::controls(r);
            if back.contains(x, y) {
                self.close_player();
                return VideosAction::Redraw;
            }
            if play.contains(x, y) || self.picture_rect(r).contains(x, y) {
                self.toggle();
                return VideosAction::Redraw;
            }
            let grab = Rect::new(seek.x, seek.y - 14, seek.w, seek.h + 28);
            if grab.contains(x, y) {
                let dur = self.player.as_ref().map(|p| p.dur).unwrap_or(0);
                self.seek_to(((x - seek.x).max(0) as u64 * dur / seek.w.max(1) as u64).min(dur));
                if let Some(p) = self.player.as_mut() {
                    p.seeking = true;
                }
                return VideosAction::Redraw;
            }
            return VideosAction::None;
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
        let seeking = self.player.as_ref().is_some_and(|p| p.seeking);
        if !seeking {
            return false;
        }
        let (_, _, seek) = Self::controls(r);
        let dur = self.player.as_ref().map(|p| p.dur).unwrap_or(0);
        if let Some(p) = self.player.as_mut() {
            p.pos = ((x - seek.x).max(0) as u64 * dur / seek.w.max(1) as u64).min(dur);
        }
        true
    }

    pub fn on_release(&mut self) -> bool {
        let Some(p) = self.player.as_mut() else { return false };
        if p.seeking {
            p.seeking = false;
            let pos = p.pos;
            self.seek_to(pos);
            return true;
        }
        false
    }

    pub fn on_key(&mut self, sc: u8) -> Option<VideosAction> {
        if sc & 0x80 != 0 {
            return None;
        }
        self.player.as_ref()?;
        let pos = self.player.as_ref().map(|p| p.pos).unwrap_or(0);
        match sc {
            0x39 => self.toggle(),
            0x01 => self.close_player(),
            0x4B => self.seek_to(pos.saturating_sub(5000)),
            0x4D => self.seek_to(pos + 5000),
            _ => return None,
        }
        Some(VideosAction::Redraw)
    }

    pub fn closed(&mut self) {
        self.close_player();
    }
}

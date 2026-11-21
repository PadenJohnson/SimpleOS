//! The Video Editor app: photos, videos and music on the left, the preview in the middle,
//! the selected clip's settings on the right, and a timeline with tracks along the bottom
//! (like Kdenlive, simplified). Saves the project by itself; exports MP4 into /Videos.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;
use spin::Mutex;


use crate::gfx::{mix as cmix, rgb, Canvas, Rect, Size};
use crate::interrupts::{HZ, TICKS};
use crate::library::{unique_name, Storage};
use crate::movie::{self, Clip, Export, Frame, Io, Kind, Media, Project, Renderer, Track, Trans, MIN_MS, MOTIONS, STYLES, TRACKS, TRANSITIONS};
use crate::photos::{ellipsize, Source};
use crate::textfield::{TextEvent, TextField};
use crate::usb::Usb;

const BG: u32 = rgb(36, 39, 44);
const PANEL: u32 = rgb(47, 51, 57);
const PANEL2: u32 = rgb(58, 63, 70);
const TEXT: u32 = rgb(232, 235, 239);
const DIM: u32 = rgb(160, 168, 178);
const ACCENT: u32 = rgb(46, 134, 222);
const RED: u32 = rgb(231, 76, 60);
const SEL: u32 = rgb(255, 214, 10);
const PROJECT_FILE: &str = "Movie project.txt";

fn track_color(t: Track) -> u32 {
    match t {
        Track::Video => rgb(52, 116, 190),
        Track::Titles => rgb(142, 68, 173),
        Track::Music1 | Track::Music2 => rgb(39, 150, 90),
    }
}

/// Files the bootloader loaded (development and test boots): name and bytes.
pub static BOOT_FILES: Mutex<Vec<(String, &'static [u8])>> = Mutex::new(Vec::new());

/// Reads media from Solon storage and the boot drive.
pub struct LibIo<'a> {
    pub storage: &'a mut Storage,
    pub usb: &'a mut Usb,
}

impl Io for LibIo<'_> {
    fn read(&mut self, src: &Source, off: u64, len: usize) -> Option<Vec<u8>> {
        match src {
            Source::Memory(b) => {
                let s = (off as usize).min(b.len());
                Some(b[s..(s + len).min(b.len())].to_vec())
            }
            Source::Library { entry, .. } => {
                let len = (len as u64).min(entry.size.saturating_sub(off)) as usize;
                self.storage.read_part(self.usb, entry, off, len).ok()
            }
            Source::Usb { drive, vol, entry } => {
                let len = (len as u64).min(entry.size.saturating_sub(off)) as usize;
                if self.usb.is_phone(*drive) {
                    self.usb.read_part(*drive, entry, off, len).ok()
                } else {
                    self.usb.drive_read_part(*drive, *vol, entry, off, len).ok()
                }
            }
        }
    }
    fn read_all(&mut self, src: &Source) -> Option<Vec<u8>> {
        match src {
            Source::Memory(b) => Some(b.to_vec()),
            Source::Library { entry, .. } => self.storage.read_file(self.usb, entry).ok(),
            Source::Usb { drive, vol, entry } => self.usb.read_file(*drive, *vol, entry).ok(),
        }
    }
}

fn button(c: &mut Canvas, b: Rect, label: &str, on: bool, enabled: bool) {
    let bg = if !enabled { rgb(52, 56, 62) } else if on { ACCENT } else { PANEL2 };
    c.round_rect(b.x, b.y, b.w, b.h, 7, bg);
    let fg = if enabled { TEXT } else { rgb(110, 116, 124) };
    c.text_centered(b.x + b.w / 2, b.y + (b.h - 16) / 2, label, Size::Small, true, fg);
}

fn time_text(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}.{}", s / 60, s % 60, (ms % 1000) / 100)
}

fn short_time(ms: u64) -> String {
    let s = ms / 1000;
    format!("{}:{:02}", s / 60, s % 60)
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Grab {
    Move,
    Left,
    Right,
}

struct Drag {
    clip: usize,
    grab: Grab,
    /// Pointer x where the drag began, and the clip as it was.
    x0: i32,
    orig: Clip,
    moved: bool,
}

struct Play {
    /// Timeline position when playing started, and the tick it started at.
    from: u64,
    tick: u64,
    /// Sound: 48 kHz frames of the movie queued so far (from `from`).
    queued: u64,
    sound: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Dialog {
    None,
    Export,
    NewMovie,
}

pub enum MovieAction {
    None,
    Redraw,
    Toast(String),
}

pub struct MovieApp {
    pub project: Project,
    loaded: bool,
    /// Everything that can go into a movie.
    bin: Vec<Media>,
    bin_tab: Kind,
    bin_scroll: i32,
    bin_sel: Option<usize>,
    scanned: bool,
    sel: Option<usize>,
    playhead: u64,
    play: Option<Play>,
    renderer: Renderer,
    preview: Frame,
    /// Moment the preview shows (None: draw again).
    preview_t: Option<u64>,
    /// Timeline: pixels per second, and the time at its left edge.
    zoom: i64,
    scroll: u64,
    drag: Option<Drag>,
    scrubbing: bool,
    undo: Vec<Vec<Clip>>,
    redo: Vec<Vec<Clip>>,
    text: Option<TextField>,
    dialog: Dialog,
    name: TextField,
    size: usize,
    export: Option<(Export, String)>,
    save_at: Option<u64>,
    last_click: Option<(usize, u64)>,
    ctrl: bool,
    shift: bool,
    body: Rect,
    /// Last pointer position (for dragging).
    cursor: (i32, i32),
}

const SIZES: [(usize, usize, &str); 3] = [(1280, 720, "HD 720p"), (1920, 1080, "Full HD"), (854, 480, "Small")];

impl MovieApp {
    pub fn new() -> MovieApp {
        MovieApp {
            project: Project::new(),
            loaded: false,
            bin: Vec::new(),
            bin_tab: Kind::Photo,
            bin_scroll: 0,
            bin_sel: None,
            scanned: false,
            sel: None,
            playhead: 0,
            play: None,
            renderer: Renderer::new(16, 9),
            preview: Frame { w: 16, h: 9, px: vec![0; 144] },
            preview_t: None,
            zoom: 50,
            scroll: 0,
            drag: None,
            scrubbing: false,
            undo: Vec::new(),
            redo: Vec::new(),
            text: None,
            dialog: Dialog::None,
            name: TextField::new("My movie", 60),
            size: 0,
            export: None,
            save_at: None,
            last_click: None,
            ctrl: false,
            shift: false,
            body: Rect::new(0, 0, 0, 0),
            cursor: (0, 0),
        }
    }

    /// A preview is playing (the screensaver waits).
    pub fn playing(&self) -> bool {
        self.play.is_some()
    }

    pub fn exporting(&self) -> bool {
        self.export.is_some()
    }

    // ---- layout ---------------------------------------------------------------------------

    fn top_h(r: Rect) -> i32 {
        (r.h * 55 / 100).max(260)
    }
    fn bin_rect(r: Rect) -> Rect {
        Rect::new(r.x, r.y, 300, Self::top_h(r))
    }
    fn props_rect(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 320, r.y, 320, Self::top_h(r))
    }
    fn preview_area(r: Rect) -> Rect {
        Rect::new(r.x + 300, r.y, r.w - 620, Self::top_h(r))
    }
    /// The picture itself (16:9) inside the preview area.
    fn picture(r: Rect) -> Rect {
        let a = Self::preview_area(r);
        let (mw, mh) = (a.w - 24, a.h - 70);
        let (w, h) = if mw * 9 > mh * 16 { (mh * 16 / 9, mh) } else { (mw, mw * 9 / 16) };
        Rect::new(a.x + (a.w - w) / 2, a.y + 12, w.max(16), h.max(9))
    }
    fn controls(r: Rect) -> [Rect; 3] {
        let p = Self::picture(r);
        let y = p.y + p.h + 10;
        let cx = p.x + p.w / 2;
        [Rect::new(cx - 112, y, 60, 36), Rect::new(cx - 44, y, 88, 36), Rect::new(cx + 52, y, 60, 36)]
    }
    fn toolbar(r: Rect) -> Rect {
        Rect::new(r.x, r.y + Self::top_h(r), r.w, 46)
    }
    fn tool_buttons(r: Rect) -> Vec<(&'static str, Rect)> {
        let t = Self::toolbar(r);
        let y = t.y + 6;
        let mut v = Vec::new();
        let mut x = t.x + 10;
        for (label, w) in [("Split", 70), ("Delete", 78), ("Add title", 100), ("Undo", 66), ("Redo", 66)] {
            v.push((label, Rect::new(x, y, w, 34)));
            x += w + 8;
        }
        let mut x = t.x + t.w - 10;
        for (label, w) in [("Export video", 130), ("New movie", 110), ("+", 38), ("-", 38)] {
            x -= w;
            v.push((label, Rect::new(x, y, w, 34)));
            x -= 8;
        }
        v
    }
    fn timeline(r: Rect) -> Rect {
        let t = Self::toolbar(r);
        Rect::new(r.x, t.y + t.h, r.w, r.y + r.h - t.y - t.h)
    }
    const HEAD_W: i32 = 92;
    const RULER_H: i32 = 26;
    fn track_rect(r: Rect, k: usize) -> Rect {
        let tl = Self::timeline(r);
        let th = ((tl.h - Self::RULER_H - 14) / 4).max(24);
        Rect::new(tl.x + Self::HEAD_W, tl.y + Self::RULER_H + k as i32 * th, tl.w - Self::HEAD_W, th - 2)
    }
    fn x_of(&self, r: Rect, t: u64) -> i32 {
        let tr = Self::track_rect(r, 0);
        tr.x + ((t as i64 - self.scroll as i64) * self.zoom / 1000) as i32
    }
    fn t_of(&self, r: Rect, x: i32) -> u64 {
        let tr = Self::track_rect(r, 0);
        (self.scroll as i64 + (x - tr.x) as i64 * 1000 / self.zoom).max(0) as u64
    }
    fn clip_rect(&self, r: Rect, c: &Clip) -> Rect {
        let k = TRACKS.iter().position(|t| t.0 == c.track).unwrap_or(0);
        let tr = Self::track_rect(r, k);
        let x0 = self.x_of(r, c.start);
        let x1 = self.x_of(r, c.end());
        Rect::new(x0, tr.y + 3, (x1 - x0).max(4), tr.h - 6)
    }

    // ---- media ------------------------------------------------------------------------------

    /// Find everything usable: photos, AVI videos and WAV music in Solon storage (and on
    /// the boot drive).
    fn scan(&mut self, storage: &mut Storage, usb: &mut Usb) {
        self.bin.clear();
        for (name, bytes) in BOOT_FILES.lock().iter() {
            if let Some(kind) = movie::media_kind(name) {
                self.bin.push(Media::new(kind, name.clone(), format!("boot:{}", name), Source::Memory(bytes), bytes.len() as u64));
            }
        }
        if storage.library.is_some() {
            if let Ok(root) = storage.root(usb) {
                let mut stack = vec![(root, String::new(), 0u32)];
                while let Some((dir, path, depth)) = stack.pop() {
                    for e in storage.list(usb, &dir).unwrap_or_default() {
                        if e.is_dir {
                            let hidden = depth == 0 && (e.name.eq_ignore_ascii_case(crate::library::BIN) || e.name.eq_ignore_ascii_case("Originals"));
                            if !hidden && depth < 6 {
                                let p = format!("{}{}/", path, e.name);
                                stack.push((e, p, depth + 1));
                            }
                        } else if let Some(kind) = movie::media_kind(&e.name) {
                            let p = format!("{}{}", path, e.name);
                            let size = e.size;
                            self.bin.push(Media::new(kind, e.name.clone(), p, Source::Library { dir: dir.clone(), entry: e }, size));
                        }
                    }
                }
            }
        }
        self.bin.sort_by(|a, b| crate::fs::natural_cmp(&a.path, &b.path));
        self.scanned = true;
        kprintln!("movie: {} photos, {} videos, {} music files", self.count(Kind::Photo), self.count(Kind::Video), self.count(Kind::Music));
    }

    fn count(&self, k: Kind) -> usize {
        self.bin.iter().filter(|m| m.kind == k).count()
    }

    fn shown(&self) -> Vec<usize> {
        (0..self.bin.len()).filter(|&i| self.bin[i].kind == self.bin_tab).collect()
    }

    /// Turn a saved path back into a file.
    fn find(storage: &mut Storage, usb: &mut Usb, path: &str) -> Option<(Source, u64)> {
        if let Some(name) = path.strip_prefix("boot:") {
            return BOOT_FILES.lock().iter().find(|f| f.0 == name).map(|f| (Source::Memory(f.1), f.1.len() as u64));
        }
        let mut dir = storage.root(usb).ok()?;
        let parts: Vec<&str> = path.split('/').collect();
        for (i, part) in parts.iter().enumerate() {
            let e = storage.list(usb, &dir).ok()?.into_iter().find(|e| e.name.eq_ignore_ascii_case(part))?;
            if i == parts.len() - 1 {
                let size = e.size;
                return Some((Source::Library { dir, entry: e }, size));
            }
            dir = e;
        }
        None
    }

    /// Opening the app: find media and the saved project.
    pub fn opened(&mut self, storage: &mut Storage, usb: &mut Usb) {
        self.scan(storage, usb);
        if !self.loaded {
            self.loaded = true;
            if storage.library.is_some() {
                if let Ok(dir) = storage.videos_dir(usb) {
                    let file = storage.list(usb, &dir).unwrap_or_default().into_iter().find(|e| e.name == PROJECT_FILE);
                    if let Some(f) = file {
                        if let Ok(data) = storage.read_file(usb, &f) {
                            let text = String::from_utf8_lossy(&data).into_owned();
                            self.project = Project::from_text(&text, &mut |p| Self::find(storage, usb, p));
                            kprintln!("movie: project loaded, {} clips", self.project.clips.len());
                        }
                    }
                }
            }
        }
        self.preview_t = None;
    }

    fn save_project(&mut self, storage: &mut Storage, usb: &mut Usb) {
        if storage.library.is_none() {
            return;
        }
        let text = self.project.to_text();
        let Ok(dir) = storage.videos_dir(usb) else { return };
        let old = storage.list(usb, &dir).unwrap_or_default().into_iter().find(|e| e.name == PROJECT_FILE);
        match storage.save_file(usb, &dir, old.as_ref(), PROJECT_FILE, text.as_bytes()) {
            Ok(_) => kprintln!("movie: project saved ({} clips)", self.project.clips.len()),
            Err(e) => kprintln!("movie: couldn't save the project: {:?}", e),
        }
    }

    // ---- editing --------------------------------------------------------------------------

    /// Call before changing the project.
    fn checkpoint(&mut self) {
        self.undo.push(self.project.clips.clone());
        if self.undo.len() > 60 {
            self.undo.remove(0);
        }
        self.redo.clear();
    }

    fn changed(&mut self) {
        self.project.layout();
        self.preview_t = None;
        self.save_at = Some(TICKS.load(Relaxed) + 2 * HZ);
        if let Some(i) = self.sel {
            if i >= self.project.clips.len() {
                self.sel = None;
            }
        }
    }

    fn add_selected(&mut self, io: &mut dyn Io) -> MovieAction {
        let Some(bi) = self.bin_sel else { return MovieAction::Toast(String::from("Pick a photo, video or song first")) };
        let mut m = self.bin[bi].clone();
        m.thumb = self.bin[bi].thumb.as_ref().map(|t| crate::image::Image { w: t.w, h: t.h, px: t.px.clone(), opaque: t.opaque });
        let kind = m.kind;
        self.checkpoint();
        let mi = self.project.add_media(m);
        let at = self.playhead;
        let r = match kind {
            Kind::Music => self.project.add_music(mi, at, io),
            _ => self.project.append(mi, io),
        };
        match r {
            Some(i) => {
                self.sel = Some(i);
                self.text = None;
                self.changed();
                // Show where it went.
                let c = &self.project.clips[i];
                if kind != Kind::Music {
                    self.playhead = c.start;
                }
                self.ensure_visible(c.start, c.end());
                MovieAction::Redraw
            }
            None => {
                self.undo.pop();
                let why = self.project.media[mi].broken.unwrap_or("Couldn't read it");
                MovieAction::Toast(format!("Can't use {}: {}", self.project.media[mi].name, why))
            }
        }
    }

    fn ensure_visible(&mut self, a: u64, b: u64) {
        let r = self.body;
        let span = (Self::track_rect(r, 0).w as i64 * 1000 / self.zoom) as u64;
        if a < self.scroll {
            self.scroll = a.saturating_sub(500);
        } else if b > self.scroll + span {
            self.scroll = b.saturating_sub(span) + 500;
        }
    }

    fn split(&mut self) -> MovieAction {
        let t = self.playhead;
        // The selected clip if the playhead is in it, else the main-track clip under it.
        let i = self.sel.filter(|&i| self.project.clips[i].start < t && t < self.project.clips[i].end()).or_else(|| {
            self.project.video_indices().into_iter().rev().find(|&i| self.project.clips[i].start < t && t < self.project.clips[i].end())
        });
        let Some(i) = i else { return MovieAction::Toast(String::from("Move the red line onto a clip to split it")) };
        self.checkpoint();
        match self.project.split(i, t) {
            Some(n) => {
                self.sel = Some(n);
                self.changed();
                MovieAction::Redraw
            }
            None => {
                self.undo.pop();
                MovieAction::Toast(String::from("Too close to the clip's edge to split"))
            }
        }
    }

    fn delete(&mut self) -> MovieAction {
        let Some(i) = self.sel else { return MovieAction::Toast(String::from("Click a clip on the timeline first")) };
        self.checkpoint();
        self.project.clips.remove(i);
        self.sel = None;
        self.text = None;
        self.changed();
        MovieAction::Redraw
    }

    fn add_title(&mut self) -> MovieAction {
        self.checkpoint();
        let i = self.project.add_title(self.playhead, "Your title");
        self.sel = Some(i);
        self.text = Some(TextField::any("Your title", 80, false).selected());
        self.changed();
        MovieAction::Redraw
    }

    fn undo_redo(&mut self, redo: bool) -> MovieAction {
        let (from, to) = if redo { (&mut self.redo, &mut self.undo) } else { (&mut self.undo, &mut self.redo) };
        let Some(clips) = from.pop() else { return MovieAction::None };
        to.push(core::mem::replace(&mut self.project.clips, clips));
        self.sel = None;
        self.text = None;
        self.changed();
        MovieAction::Redraw
    }

    fn set_playhead(&mut self, t: u64) {
        self.playhead = t.min(self.project.duration());
        self.preview_t = None;
    }

    fn toggle_play(&mut self) {
        if self.play.is_some() {
            self.stop();
            return;
        }
        if self.project.duration() == 0 {
            return;
        }
        if self.playhead + 100 >= self.project.duration() {
            self.playhead = 0;
        }
        let sound = crate::hda::available();
        if sound {
            crate::hda::start();
        }
        self.play = Some(Play { from: self.playhead, tick: TICKS.load(Relaxed), queued: 0, sound });
    }

    fn stop(&mut self) {
        if let Some(p) = self.play.take() {
            if p.sound {
                crate::hda::stop();
            }
        }
        self.preview_t = None;
    }

    // ---- tick: playback, previews, thumbnails, export, saving ----------------------------------

    pub fn tick(&mut self, now: u64, storage: &mut Storage, usb: &mut Usb, visible: bool) -> MovieAction {
        let mut io = LibIo { storage, usb };
        // Export: a few frames each tick (keeps the screen alive).
        if let Some((job, _)) = self.export.as_mut() {
            let start = TICKS.load(Relaxed);
            while TICKS.load(Relaxed) < start + 5 && job.step(&mut self.project, &mut io) {}
            if job.done() {
                let (job, name) = self.export.take().unwrap();
                return self.write_export(job, &name, io.storage, io.usb);
            }
            return MovieAction::Redraw;
        }
        if let Some(due) = self.save_at {
            if now >= due && self.drag.is_none() {
                self.save_at = None;
                self.save_project(io.storage, io.usb);
            }
        }
        if !visible {
            return MovieAction::None;
        }
        let mut redraw = false;
        // Playing: follow the sound card's clock (or the timer), keep the sound topped up.
        if let Some(p) = self.play.as_mut() {
            let elapsed = if p.sound { crate::hda::played() * 1000 / movie::RATE as u64 } else { (now - p.tick) * 1000 / HZ };
            let t = p.from + elapsed;
            if p.sound {
                let want = crate::hda::wanted();
                if want >= 2048 {
                    let start = p.from * movie::RATE as u64 / 1000 + p.queued;
                    let pcm = movie::mix(&mut self.project, start, want, &mut io);
                    crate::hda::queue(&pcm);
                    p.queued += want as u64;
                }
            }
            if t >= self.project.duration() {
                self.playhead = self.project.duration();
                self.stop();
            } else {
                self.playhead = t;
                // Keep the playhead on screen.
                let r = self.body;
                let tr = Self::track_rect(r, 0);
                if self.x_of(r, t) > tr.x + tr.w - 40 {
                    self.scroll = t.saturating_sub(2000);
                }
            }
            if self.preview_t.is_none_or(|pt| self.playhead.abs_diff(pt) >= 40) {
                self.preview_t = None;
            }
            redraw = true;
        }
        // Clips loaded from a saved project get their pictures from the media list.
        for m in self.project.media.iter_mut().filter(|m| m.thumb.is_none()) {
            if let Some(t) = self.bin.iter().find(|b| b.path == m.path).and_then(|b| b.thumb.as_ref()) {
                m.thumb = Some(crate::image::Image { w: t.w, h: t.h, px: t.px.clone(), opaque: t.opaque });
                redraw = true;
            }
        }
        let pic = Self::picture(self.body);
        if pic.w as usize != self.renderer.w || pic.h as usize != self.renderer.h {
            self.renderer = Renderer::new(pic.w as usize, pic.h as usize);
            self.preview = Frame { w: pic.w as usize, h: pic.h as usize, px: vec![0; (pic.w * pic.h) as usize] };
            self.preview_t = None;
        }
        if self.preview_t.is_none() && self.body.w > 0 {
            let t = self.playhead;
            self.renderer.render(&mut self.project, t, &mut io, &mut self.preview);
            self.preview_t = Some(t);
            redraw = true;
        } else if self.play.is_none() && self.drag.is_none() {
            // Idle: make one thumbnail.
            if let Some(i) = self.bin.iter().position(|m| m.thumb.is_none() && m.broken.is_none() && m.kind != Kind::Music) {
                self.make_thumb(i, &mut io);
                redraw = true;
            }
        }
        if redraw {
            MovieAction::Redraw
        } else {
            MovieAction::None
        }
    }

    fn make_thumb(&mut self, i: usize, io: &mut dyn Io) {
        let m = &mut self.bin[i];
        let jpeg = match m.kind {
            Kind::Photo => io.read_all(&m.src),
            Kind::Video => {
                if !m.open(io) {
                    return;
                }
                let src = m.src.clone();
                let mut read = |off: u64, len: usize| io.read(&src, off, len);
                if let Some(mm) = m.mp4.as_mut() {
                    // Phone video: decode the first picture.
                    let (vw, vh) = (mm.width.max(1), mm.height.max(1));
                    let (w, h) = crate::image::fit(vw, vh, 132, 84);
                    let rot = mm.rotation;
                    let px = mm.frame_at(&mut read, 0, 8).map(|f| crate::video::mp4src::frame_rgb(f, rot, w.max(1), h.max(1)));
                    match px {
                        Some(px) => m.thumb = Some(crate::image::Image { w: w.max(1), h: h.max(1), px: px.iter().map(|v| v | 0xFF00_0000).collect(), opaque: true }),
                        None => m.broken = Some("Couldn't read it"),
                    }
                    // The bin keeps only the picture; the editor opens the video again when used.
                    m.mp4 = None;
                    return;
                }
                m.avi.as_ref().and_then(|a| a.frame_jpeg(&mut read, 0))
            }
            Kind::Music => return,
        };
        match jpeg.map(|d| crate::image::decode(&d)) {
            Some(Ok(img)) => m.thumb = Some(img.shrink_to_fit(132, 84)),
            _ => m.broken = Some("Couldn't read it"),
        }
    }

    fn write_export(&mut self, job: Export, name: &str, storage: &mut Storage, usb: &mut Usb) -> MovieAction {
        let (head, payload) = job.finish();
        let total = head.len() as u64 + payload.len() as u64;
        let result = (|| -> Result<String, crate::fs::FsError> {
            let dir = storage.videos_dir(usb)?;
            let existing = storage.names_in(usb, &dir)?;
            let fname = unique_name(&existing, &format!("{}.mp4", name), u64::MAX).ok_or(crate::fs::FsError::Exists)?;
            let p = storage.begin_file(usb, &dir, &fname, total)?;
            // Pieces must start on cluster boundaries: write header + payload as one stream
            // of 4 MiB pieces.
            const PIECE: u64 = 4 << 20;
            let mut r = Ok(());
            let mut off = 0u64;
            while off < total && r.is_ok() {
                let n = PIECE.min(total - off) as usize;
                let mut piece = Vec::with_capacity(n);
                let hl = head.len() as u64;
                if off < hl {
                    piece.extend_from_slice(&head[off as usize..(hl.min(off + n as u64)) as usize]);
                }
                let ps = off.max(hl) - hl;
                let pe = (off + n as u64 - hl.min(off + n as u64)).min(payload.len() as u64);
                if pe > ps {
                    piece.extend_from_slice(&payload[ps as usize..pe as usize]);
                }
                r = storage.write_part(usb, &p, off, &piece);
                off += n as u64;
            }
            if let Err(e) = r {
                let _ = storage.abort_file(usb, p);
                return Err(e);
            }
            storage.finish_file(usb, p)?;
            Ok(fname)
        })();
        match result {
            Ok(fname) => {
                kprintln!("movie: exported \"{}\" ({} bytes)", fname, total);
                MovieAction::Toast(format!("Saved \"{}\" in the Videos folder", fname))
            }
            Err(e) => {
                kprintln!("movie: export failed: {:?}", e);
                MovieAction::Toast(String::from("Couldn't save the video (is the disk full?)"))
            }
        }
    }

    // ---- drawing ------------------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, r: Rect) {
        self.body = r;
        c.fill_rect(r.x, r.y, r.w, r.h, BG);
        self.draw_bin(c, r);
        self.draw_preview(c, r);
        self.draw_props(c, r);
        self.draw_toolbar(c, r);
        self.draw_timeline(c, r);
        if self.export.is_some() || self.dialog != Dialog::None {
            self.draw_dialog(c, r);
        }
    }

    fn bin_tabs(r: Rect) -> [(Kind, &'static str, Rect); 3] {
        let b = Self::bin_rect(r);
        [(Kind::Photo, "Photos", Rect::new(b.x + 8, b.y + 8, 90, 30)), (Kind::Video, "Videos", Rect::new(b.x + 104, b.y + 8, 90, 30)), (Kind::Music, "Music", Rect::new(b.x + 200, b.y + 8, 90, 30))]
    }
    fn add_button(r: Rect) -> Rect {
        let b = Self::bin_rect(r);
        Rect::new(b.x + 8, b.y + b.h - 44, b.w - 16, 36)
    }
    fn bin_grid(r: Rect) -> Rect {
        let b = Self::bin_rect(r);
        Rect::new(b.x + 8, b.y + 46, b.w - 16, b.h - 46 - 52)
    }
    /// Tiles: two columns of pictures, or one column of rows for music.
    fn tile(&self, r: Rect, k: usize) -> Rect {
        let g = Self::bin_grid(r);
        if self.bin_tab == Kind::Music {
            Rect::new(g.x, g.y + k as i32 * 40 - self.bin_scroll, g.w, 36)
        } else {
            Rect::new(g.x + (k % 2) as i32 * 144, g.y + (k / 2) as i32 * 112 - self.bin_scroll, 138, 106)
        }
    }

    fn draw_bin(&self, c: &mut Canvas, r: Rect) {
        let b = Self::bin_rect(r);
        c.fill_rect(b.x, b.y, b.w, b.h, PANEL);
        for (k, label, t) in Self::bin_tabs(r) {
            button(c, t, label, self.bin_tab == k, true);
        }
        let g = Self::bin_grid(r);
        c.set_clip(g);
        let shown = self.shown();
        if shown.is_empty() {
            let msg = match self.bin_tab {
                Kind::Photo => ["No photos in Solon yet.", "Use Import to bring some in."],
                Kind::Video => ["No videos yet.", "Import them from a phone or camera."],
                Kind::Music => ["No music (.wav files).", "Copy some in with Files."],
            };
            c.text_centered(g.x + g.w / 2, g.y + 40, msg[0], Size::Small, false, DIM);
            c.text_centered(g.x + g.w / 2, g.y + 62, msg[1], Size::Small, false, DIM);
        }
        for (k, &i) in shown.iter().enumerate() {
            let t = self.tile(r, k);
            if t.y + t.h < g.y || t.y > g.y + g.h {
                continue;
            }
            let m = &self.bin[i];
            let selected = self.bin_sel == Some(i);
            c.round_rect(t.x, t.y, t.w, t.h, 6, if selected { ACCENT } else { PANEL2 });
            if m.kind == Kind::Music {
                note_icon(c, t.x + 10, t.y + 8);
                let name = ellipsize(&m.name, t.w - 50, Size::Small, false);
                c.text(t.x + 40, t.y + 10, &name, Size::Small, false, TEXT);
                continue;
            }
            match &m.thumb {
                Some(img) => c.draw_image(img, t.x + (t.w - img.w as i32) / 2, t.y + 3 + (84 - img.h as i32) / 2),
                None => c.fill_rect(t.x + 3, t.y + 3, t.w - 6, 84, rgb(30, 33, 37)),
            }
            if m.kind == Kind::Video {
                c.circle(t.x + t.w / 2, t.y + 45, 14, rgb(0, 0, 0));
                c.triangle([(t.x + t.w / 2 - 5, t.y + 37), (t.x + t.w / 2 - 5, t.y + 53), (t.x + t.w / 2 + 8, t.y + 45)], TEXT);
            }
            let name = ellipsize(&m.name, t.w - 8, Size::Small, false);
            c.text(t.x + 4, t.y + 88, &name, Size::Small, false, if m.broken.is_some() { RED } else { TEXT });
        }
        c.reset_clip();
        c.set_clip(Rect::new(r.x, r.y, r.w, r.h));
        let label = match self.bin_tab {
            Kind::Music => "Add music at the red line",
            _ => "Add to movie",
        };
        button(c, Self::add_button(r), label, true, self.bin_sel.is_some());
    }

    fn draw_preview(&self, c: &mut Canvas, r: Rect) {
        let a = Self::preview_area(r);
        c.fill_rect(a.x, a.y, a.w, a.h, BG);
        let p = Self::picture(r);
        if self.preview.w == p.w as usize && self.preview.h == p.h as usize {
            let (cw, ch) = (c.w as i32, c.h as i32);
            for y in 0..p.h {
                let yy = p.y + y;
                if yy < 0 || yy >= ch {
                    continue;
                }
                let x0 = p.x.max(0);
                let x1 = (p.x + p.w).min(cw);
                if x0 >= x1 {
                    continue;
                }
                let src = &self.preview.px[(y * p.w + (x0 - p.x)) as usize..(y * p.w + (x1 - p.x)) as usize];
                let d = (yy * cw) as usize;
                c.px[d + x0 as usize..d + x1 as usize].copy_from_slice(src);
            }
        } else {
            c.fill_rect(p.x, p.y, p.w, p.h, 0);
        }
        if self.project.clips.is_empty() {
            c.text_centered(p.x + p.w / 2, p.y + p.h / 2 - 20, "Your movie appears here.", Size::Medium, true, TEXT);
            c.text_centered(p.x + p.w / 2, p.y + p.h / 2 + 10, "Pick photos or videos on the left and press Add to movie.", Size::Small, false, DIM);
        }
        let [back, play, fwd] = Self::controls(r);
        button(c, back, "|<", false, true);
        button(c, play, if self.play.is_some() { "Pause" } else { "Play" }, true, true);
        button(c, fwd, ">|", false, true);
        let t = format!("{} / {}", time_text(self.playhead), time_text(self.project.duration()));
        c.text(fwd.x + fwd.w + 16, fwd.y + 10, &t, Size::Small, false, TEXT);
        if !crate::hda::available() {
            c.text(back.x - 150, back.y + 10, "(no sound card)", Size::Small, false, DIM);
        }
    }

    // Settings panel rows.
    fn prop_row(r: Rect, k: i32) -> Rect {
        let p = Self::props_rect(r);
        Rect::new(p.x + 14, p.y + 46 + k * 50, p.w - 28, 36)
    }
    /// Option buttons in a row (up to 3 per row).
    fn options(r: Rect, row: i32, n: usize) -> Vec<Rect> {
        let mut v = Vec::new();
        for k in 0..n {
            let rr = Self::prop_row(r, row + (k / 3) as i32);
            let per = n.min(3) as i32;
            let w = (rr.w - (per - 1) * 6) / per;
            v.push(Rect::new(rr.x + (k % 3) as i32 * (w + 6), rr.y, w, rr.h));
        }
        v
    }
    /// A "-  value  +" stepper.
    fn stepper(r: Rect, row: i32) -> (Rect, Rect) {
        let rr = Self::prop_row(r, row);
        (Rect::new(rr.x + rr.w - 140, rr.y, 40, rr.h), Rect::new(rr.x + rr.w - 40, rr.y, 40, rr.h))
    }

    fn draw_label(c: &mut Canvas, r: Rect, row: i32, label: &str) {
        let rr = Self::prop_row(r, row);
        c.text(rr.x, rr.y + 10, label, Size::Small, true, DIM);
    }

    fn draw_stepper(c: &mut Canvas, r: Rect, row: i32, label: &str, value: &str) {
        Self::draw_label(c, r, row, label);
        let (minus, plus) = Self::stepper(r, row);
        button(c, minus, "-", false, true);
        button(c, plus, "+", false, true);
        c.text_centered(minus.x + 70, minus.y + 10, value, Size::Small, true, TEXT);
    }

    fn draw_props(&self, c: &mut Canvas, r: Rect) {
        let p = Self::props_rect(r);
        c.fill_rect(p.x, p.y, p.w, p.h, PANEL);
        let Some(i) = self.sel else {
            c.text(p.x + 14, p.y + 14, "Movie", Size::Medium, true, TEXT);
            let lines = [
                format!("Length: {}", short_time(self.project.duration())),
                format!("{} clip(s)", self.project.clips.len()),
                String::new(),
                String::from("Click a clip on the timeline"),
                String::from("to change it here."),
                String::new(),
                String::from("Drag clips to move them,"),
                String::from("drag their ends to shorten."),
                String::from("Space plays and pauses."),
            ];
            for (k, l) in lines.iter().enumerate() {
                c.text(p.x + 14, p.y + 52 + k as i32 * 22, l, Size::Small, false, if k < 2 { TEXT } else { DIM });
            }
            return;
        };
        let clip = &self.project.clips[i];
        let media = clip.media.map(|m| &self.project.media[m]);
        let title = match (clip.track, media) {
            (Track::Titles, _) => String::from("Title"),
            (_, Some(m)) => ellipsize(&m.name, p.w - 28, Size::Medium, true),
            _ => String::from("Clip"),
        };
        c.text(p.x + 14, p.y + 14, &title, Size::Medium, true, TEXT);
        let len = format!("{}.{} s", clip.len / 1000, (clip.len % 1000) / 100);
        match (clip.track, media.map(|m| m.kind)) {
            (Track::Titles, _) => {
                Self::draw_label(c, r, 0, "Text (click to type)");
                let tr = Self::prop_row(r, 1);
                match &self.text {
                    Some(t) => t.draw(c, tr),
                    None => {
                        c.round_rect(tr.x, tr.y, tr.w, tr.h, 6, rgb(255, 255, 255));
                        let s = ellipsize(&clip.text, tr.w - 20, Size::Small, false);
                        c.text(tr.x + 10, tr.y + 10, &s, Size::Small, false, rgb(40, 44, 52));
                    }
                }
                Self::draw_label(c, r, 2, "Where");
                for (k, b) in Self::options(r, 3, 3).into_iter().enumerate() {
                    button(c, b, STYLES[k], clip.style == k, true);
                }
                Self::draw_stepper(c, r, 4, "Length", &len);
            }
            (Track::Video, Some(kind)) => {
                Self::draw_stepper(c, r, 0, "Length", &len);
                let mut row = 1;
                if kind == Kind::Photo {
                    Self::draw_label(c, r, row, "Movement");
                    for (k, b) in Self::options(r, row + 1, 5).into_iter().enumerate() {
                        button(c, b, MOTIONS[k].1, clip.motion == MOTIONS[k].0, true);
                    }
                    row += 3;
                } else {
                    Self::draw_stepper(c, r, row, "Sound", &format!("{}%", clip.volume));
                    row += 1;
                }
                Self::draw_label(c, r, row, "Start with");
                for (k, b) in Self::options(r, row + 1, 3).into_iter().enumerate() {
                    button(c, b, TRANSITIONS[k].1, clip.trans == TRANSITIONS[k].0, true);
                }
            }
            (_, Some(Kind::Music)) => {
                Self::draw_stepper(c, r, 0, "Volume", &format!("{}%", clip.volume));
                Self::draw_label(c, r, 1, "Fade in and out");
                let (_, plus) = Self::stepper(r, 1);
                let b = Rect::new(plus.x - 100, plus.y, 140, plus.h);
                button(c, b, if clip.fades { "On" } else { "Off" }, clip.fades, true);
                let rr = Self::prop_row(r, 2);
                c.text(rr.x, rr.y + 10, &format!("Plays {} from {}", len, short_time(clip.offset)), Size::Small, false, DIM);
            }
            _ => {}
        }
    }

    fn draw_toolbar(&self, c: &mut Canvas, r: Rect) {
        let t = Self::toolbar(r);
        c.fill_rect(t.x, t.y, t.w, t.h, PANEL2);
        for (label, b) in Self::tool_buttons(r) {
            let enabled = match label {
                "Split" | "Delete" => self.sel.is_some() || label == "Split",
                "Undo" => !self.undo.is_empty(),
                "Redo" => !self.redo.is_empty(),
                "Export video" => self.project.duration() > 0,
                _ => true,
            };
            button(c, b, label, label == "Export video", enabled);
        }
    }

    fn draw_timeline(&self, c: &mut Canvas, r: Rect) {
        let tl = Self::timeline(r);
        c.fill_rect(tl.x, tl.y, tl.w, tl.h, BG);
        // Ruler with time marks.
        let tr0 = Self::track_rect(r, 0);
        c.fill_rect(tl.x, tl.y, tl.w, Self::RULER_H, PANEL);
        let step = match self.zoom {
            z if z >= 200 => 1000,
            z if z >= 80 => 2000,
            z if z >= 40 => 5000,
            z if z >= 15 => 10_000,
            _ => 30_000,
        };
        c.set_clip(Rect::new(tr0.x, tl.y, tr0.w, tl.h));
        let mut t = self.scroll / step * step;
        let end = self.t_of(r, tr0.x + tr0.w);
        while t <= end {
            let x = self.x_of(r, t);
            c.fill_rect(x, tl.y + 16, 1, 10, DIM);
            c.text(x + 3, tl.y + 3, &short_time(t), Size::Small, false, DIM);
            t += step;
        }
        c.reset_clip();
        c.set_clip(r);
        // Tracks.
        for (k, (track, name)) in TRACKS.iter().enumerate() {
            let tr = Self::track_rect(r, k);
            c.fill_rect(tl.x, tr.y, Self::HEAD_W - 4, tr.h, PANEL);
            c.text(tl.x + 10, tr.y + tr.h / 2 - 8, name, Size::Small, true, cmix(track_color(*track), TEXT, 140));
            c.fill_rect(tr.x, tr.y, tr.w, tr.h, rgb(42, 45, 51));
        }
        c.set_clip(Rect::new(tr0.x, tl.y, tr0.w, tl.h));
        for (i, clip) in self.project.clips.iter().enumerate() {
            let mut cr = self.clip_rect(r, clip);
            if let Some(d) = &self.drag {
                if d.clip == i && d.grab == Grab::Move && clip.track == Track::Video && d.moved {
                    continue; // drawn at the pointer below
                }
            }
            if cr.x > tr0.x + tr0.w || cr.x + cr.w < tr0.x {
                continue;
            }
            cr.w = cr.w.max(3);
            let col = track_color(clip.track);
            c.round_rect(cr.x, cr.y, cr.w, cr.h, 5, col);
            let mut tx = cr.x + 6;
            if let Some(img) = clip.media.and_then(|m| self.project.media[m].thumb.as_ref()) {
                // A small picture at the start of photo/video clips.
                let th = (cr.h - 8).min(img.h as i32);
                if cr.w > 60 && th > 10 {
                    let tw = (img.w as i32 * th / img.h as i32).min(cr.w - 10);
                    draw_scaled(c, img, cr.x + 4, cr.y + 4, tw, th);
                    tx = cr.x + tw + 10;
                }
            }
            let label = match clip.media {
                Some(m) => self.project.media[m].name.clone(),
                None => clip.text.clone(),
            };
            if cr.x + cr.w - tx > 30 {
                let s = ellipsize(&label, cr.x + cr.w - tx - 4, Size::Small, false);
                c.text(tx, cr.y + 4, &s, Size::Small, false, TEXT);
            }
            if clip.track == Track::Video && clip.trans != Trans::Cut {
                // Transition marker at the start.
                let m = match clip.trans {
                    Trans::Crossfade => "x",
                    _ => "o",
                };
                c.round_rect(cr.x + 2, cr.y + cr.h - 20, 18, 18, 4, rgb(0, 0, 0));
                c.text(cr.x + 7, cr.y + cr.h - 20, m, Size::Small, true, SEL);
            }
            if self.sel == Some(i) {
                for k in 0..2 {
                    c.outline_rect(cr.x - k, cr.y - k, cr.w + 2 * k, cr.h + 2 * k, SEL);
                }
            }
        }
        // A main-track clip being moved: a ghost at the pointer, and where it will land.
        if let Some(d) = &self.drag {
            if d.grab == Grab::Move && d.orig.track == Track::Video && d.moved {
                let (gx, at) = self.drop_point(r, d);
                let tr = Self::track_rect(r, 0);
                let w = ((d.orig.len as i64 * self.zoom / 1000) as i32).max(20);
                c.round_rect_alpha(gx, tr.y + 3, w, tr.h - 6, 5, rgb(120, 180, 255), 170);
                c.outline_rect(gx, tr.y + 3, w, tr.h - 6, SEL);
                let vids = self.project.video_indices();
                let others: Vec<usize> = vids.into_iter().filter(|&v| v != d.clip).collect();
                let mx = match others.get(at) {
                    Some(&v) => self.x_of(r, self.project.clips[v].start),
                    None => others.last().map(|&v| self.x_of(r, self.project.clips[v].end())).unwrap_or(tr.x),
                };
                c.fill_rect(mx - 2, tr.y, 4, tr.h, SEL);
            }
        }
        // The playhead.
        let px = self.x_of(r, self.playhead);
        let last = Self::track_rect(r, 3);
        c.fill_rect(px - 1, tl.y, 2, last.y + last.h - tl.y, RED);
        c.triangle([(px - 7, tl.y), (px + 7, tl.y), (px, tl.y + 10)], RED);
        c.reset_clip();
        c.set_clip(r);
    }

    /// For moving a main-track clip: the ghost's x, and the position it would take
    /// among the other clips.
    fn drop_point(&self, r: Rect, d: &Drag) -> (i32, usize) {
        let x = self.cursor.0;
        let gx = self.x_of(r, d.orig.start) + (x - d.x0);
        let t = self.t_of(r, gx + ((d.orig.len as i64 * self.zoom / 1000) as i32) / 2);
        let others: Vec<usize> = self.project.video_indices().into_iter().filter(|&v| v != d.clip).collect();
        let at = others.iter().filter(|&&v| {
            let c = &self.project.clips[v];
            c.start + c.len / 2 < t
        });
        (gx, at.count())
    }

    fn draw_dialog(&self, c: &mut Canvas, r: Rect) {
        c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 140);
        let (w, h) = (460, 250);
        let d = Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h);
        c.round_rect(d.x, d.y, d.w, d.h, 12, PANEL);
        if let Some((job, name)) = &self.export {
            c.text(d.x + 20, d.y + 20, "Making your video...", Size::Medium, true, TEXT);
            c.text(d.x + 20, d.y + 56, &ellipsize(&format!("{}.mp4   ({} x {})", name, job.w, job.h), d.w - 40, Size::Small, false), Size::Small, false, DIM);
            let bar = Rect::new(d.x + 20, d.y + 100, d.w - 40, 22);
            c.round_rect(bar.x, bar.y, bar.w, bar.h, 8, BG);
            let fill = bar.w * job.percent() as i32 / 100;
            c.round_rect(bar.x, bar.y, fill.max(10), bar.h, 8, ACCENT);
            c.text_centered(d.x + d.w / 2, d.y + 136, &format!("{}%   ({} of {})", job.percent(), short_time(job.frame * 1000 / movie::FPS), short_time(job.frames * 1000 / movie::FPS)), Size::Small, false, TEXT);
            button(c, Self::dialog_buttons(r)[1], "Cancel", false, true);
            return;
        }
        match self.dialog {
            Dialog::Export => {
                c.text(d.x + 20, d.y + 18, "Export video", Size::Medium, true, TEXT);
                c.text(d.x + 20, d.y + 52, "Name", Size::Small, true, DIM);
                self.name.draw(c, Rect::new(d.x + 90, d.y + 44, d.w - 110, 34));
                c.text(d.x + 20, d.y + 100, "Size", Size::Small, true, DIM);
                for (k, b) in Self::size_buttons(r).iter().enumerate() {
                    button(c, *b, SIZES[k].2, self.size == k, true);
                }
                c.text(d.x + 20, d.y + 142, "Saved as an MP4 in the Videos folder.", Size::Small, false, DIM);
                let [ok, cancel] = Self::dialog_buttons(r);
                button(c, ok, "Export", true, !self.name.text.trim().is_empty());
                button(c, cancel, "Cancel", false, true);
            }
            Dialog::NewMovie => {
                c.text(d.x + 20, d.y + 20, "Start a new movie?", Size::Medium, true, TEXT);
                c.text(d.x + 20, d.y + 60, "The timeline will be cleared.", Size::Small, false, DIM);
                c.text(d.x + 20, d.y + 84, "(Undo brings it back.)", Size::Small, false, DIM);
                let [ok, cancel] = Self::dialog_buttons(r);
                button(c, ok, "New movie", true, true);
                button(c, cancel, "Cancel", false, true);
            }
            Dialog::None => {}
        }
    }

    fn dialog_rect(r: Rect) -> Rect {
        let (w, h) = (460, 250);
        Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h)
    }
    fn dialog_buttons(r: Rect) -> [Rect; 2] {
        let d = Self::dialog_rect(r);
        [Rect::new(d.x + d.w - 260, d.y + d.h - 56, 120, 38), Rect::new(d.x + d.w - 130, d.y + d.h - 56, 110, 38)]
    }
    fn size_buttons(r: Rect) -> [Rect; 3] {
        let d = Self::dialog_rect(r);
        [Rect::new(d.x + 90, d.y + 92, 110, 34), Rect::new(d.x + 206, d.y + 92, 110, 34), Rect::new(d.x + 322, d.y + 92, 110, 34)]
    }

    // ---- input ---------------------------------------------------------------------------------

    pub fn on_wheel(&mut self, wheel: i8, x: i32, y: i32, r: Rect) -> bool {
        if Self::bin_grid(r).contains(x, y) {
            let rows = if self.bin_tab == Kind::Music { self.shown().len() as i32 * 40 } else { ((self.shown().len() as i32 + 1) / 2) * 112 };
            let max = (rows - Self::bin_grid(r).h).max(0);
            self.bin_scroll = (self.bin_scroll - wheel as i32 * 60).clamp(0, max);
            return true;
        }
        if Self::timeline(r).contains(x, y) {
            // Scroll the timeline sideways.
            let d = 120 * 1000 / self.zoom;
            self.scroll = (self.scroll as i64 - wheel as i64 * d).clamp(0, self.project.duration() as i64) as u64;
            return true;
        }
        false
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, storage: &mut Storage, usb: &mut Usb) -> MovieAction {
        self.body = r;
        self.cursor = (x, y);
        let mut io = LibIo { storage, usb };
        if self.export.is_some() {
            if Self::dialog_buttons(r)[1].contains(x, y) {
                self.export = None;
                return MovieAction::Toast(String::from("Export cancelled"));
            }
            return MovieAction::None;
        }
        match self.dialog {
            Dialog::Export => {
                let [ok, cancel] = Self::dialog_buttons(r);
                for (k, b) in Self::size_buttons(r).iter().enumerate() {
                    if b.contains(x, y) {
                        self.size = k;
                        return MovieAction::Redraw;
                    }
                }
                if ok.contains(x, y) && !self.name.text.trim().is_empty() {
                    return self.start_export();
                }
                if cancel.contains(x, y) {
                    self.dialog = Dialog::None;
                    return MovieAction::Redraw;
                }
                return MovieAction::None;
            }
            Dialog::NewMovie => {
                let [ok, cancel] = Self::dialog_buttons(r);
                if ok.contains(x, y) {
                    self.checkpoint();
                    self.project.clips.clear();
                    self.sel = None;
                    self.playhead = 0;
                    self.scroll = 0;
                    self.dialog = Dialog::None;
                    self.changed();
                    return MovieAction::Redraw;
                }
                if cancel.contains(x, y) {
                    self.dialog = Dialog::None;
                    return MovieAction::Redraw;
                }
                return MovieAction::None;
            }
            Dialog::None => {}
        }
        // Clicking anywhere but the text box finishes typing a title.
        if self.text.is_some() && !Self::prop_row(r, 1).contains(x, y) {
            self.finish_text();
        }
        // Media bin.
        for (k, _, t) in Self::bin_tabs(r) {
            if t.contains(x, y) {
                self.bin_tab = k;
                self.bin_scroll = 0;
                self.bin_sel = None;
                return MovieAction::Redraw;
            }
        }
        if Self::add_button(r).contains(x, y) {
            return self.add_selected(&mut io);
        }
        if Self::bin_grid(r).contains(x, y) {
            let shown = self.shown();
            for (k, &i) in shown.iter().enumerate() {
                if self.tile(r, k).contains(x, y) {
                    let double = matches!(self.last_click, Some((j, t)) if j == i && now - t < 50);
                    self.bin_sel = Some(i);
                    self.last_click = Some((i, now));
                    if double {
                        self.last_click = None;
                        return self.add_selected(&mut io);
                    }
                    return MovieAction::Redraw;
                }
            }
            return MovieAction::None;
        }
        // Player controls.
        let [back, play, fwd] = Self::controls(r);
        if back.contains(x, y) {
            self.set_playhead(0);
            self.scroll = 0;
            return MovieAction::Redraw;
        }
        if play.contains(x, y) {
            self.toggle_play();
            return MovieAction::Redraw;
        }
        if fwd.contains(x, y) {
            let d = self.project.duration();
            self.set_playhead(d);
            self.ensure_visible(d, d);
            return MovieAction::Redraw;
        }
        // Toolbar.
        for (label, b) in Self::tool_buttons(r) {
            if b.contains(x, y) {
                return match label {
                    "Split" => self.split(),
                    "Delete" => self.delete(),
                    "Add title" => self.add_title(),
                    "Undo" => self.undo_redo(false),
                    "Redo" => self.undo_redo(true),
                    "+" | "-" => {
                        let mid = self.t_of(r, self.x_of(r, self.playhead));
                        self.zoom = if label == "+" { (self.zoom * 3 / 2).min(400) } else { (self.zoom * 2 / 3).max(5) };
                        let half = (Self::track_rect(r, 0).w as i64 * 1000 / self.zoom / 2) as u64;
                        self.scroll = mid.saturating_sub(half);
                        MovieAction::Redraw
                    }
                    "New movie" => {
                        self.stop();
                        self.dialog = Dialog::NewMovie;
                        MovieAction::Redraw
                    }
                    "Export video" => {
                        if self.project.duration() == 0 {
                            return MovieAction::Toast(String::from("Add some photos or videos first"));
                        }
                        self.stop();
                        self.dialog = Dialog::Export;
                        self.name = TextField::new(&self.name.text.clone(), 60);
                        MovieAction::Redraw
                    }
                    _ => MovieAction::None,
                };
            }
        }
        // Clip settings.
        if Self::props_rect(r).contains(x, y) {
            return self.props_press(x, y, r);
        }
        // Timeline.
        let tl = Self::timeline(r);
        if tl.contains(x, y) && x >= tl.x + Self::HEAD_W {
            if y < tl.y + Self::RULER_H {
                self.stop();
                self.scrubbing = true;
                self.set_playhead(self.t_of(r, x));
                return MovieAction::Redraw;
            }
            // A clip (topmost last drawn wins).
            let hit = (0..self.project.clips.len()).rev().find(|&i| self.clip_rect(r, &self.project.clips[i]).contains(x, y));
            match hit {
                Some(i) => {
                    let cr = self.clip_rect(r, &self.project.clips[i]);
                    let edge = 8.min(cr.w / 4);
                    let grab = if x < cr.x + edge { Grab::Left } else if x >= cr.x + cr.w - edge { Grab::Right } else { Grab::Move };
                    if self.sel != Some(i) {
                        self.text = None;
                    }
                    self.sel = Some(i);
                    self.drag = Some(Drag { clip: i, grab, x0: x, orig: self.project.clips[i].clone(), moved: false });
                    if self.play.is_none() {
                        let t = self.t_of(r, x);
                        let c = &self.project.clips[i];
                        self.set_playhead(t.clamp(c.start, c.end().saturating_sub(1)));
                    }
                }
                None => {
                    self.sel = None;
                    self.text = None;
                    self.stop();
                    self.scrubbing = true;
                    self.set_playhead(self.t_of(r, x));
                }
            }
            return MovieAction::Redraw;
        }
        MovieAction::None
    }

    fn props_press(&mut self, x: i32, y: i32, r: Rect) -> MovieAction {
        let Some(i) = self.sel else { return MovieAction::None };
        let clip = self.project.clips[i].clone();
        let kind = clip.media.map(|m| self.project.media[m].kind);
        let max = self.project.max_len(&clip);
        let step_len = |c: &mut Clip, d: i64| {
            c.len = ((c.len as i64 + d).max(MIN_MS as i64) as u64).min(max);
        };
        let mut new = clip.clone();
        match (clip.track, kind) {
            (Track::Titles, _) => {
                if Self::prop_row(r, 1).contains(x, y) {
                    if self.text.is_none() {
                        self.text = Some(TextField::any(&clip.text, 80, false));
                    }
                    return MovieAction::Redraw;
                }
                for (k, b) in Self::options(r, 3, 3).into_iter().enumerate() {
                    if b.contains(x, y) {
                        new.style = k;
                    }
                }
                let (minus, plus) = Self::stepper(r, 4);
                if minus.contains(x, y) {
                    step_len(&mut new, -500);
                }
                if plus.contains(x, y) {
                    step_len(&mut new, 500);
                }
            }
            (Track::Video, Some(kind)) => {
                let (minus, plus) = Self::stepper(r, 0);
                if minus.contains(x, y) {
                    step_len(&mut new, -500);
                }
                if plus.contains(x, y) {
                    step_len(&mut new, 500);
                }
                let mut row = 1;
                if kind == Kind::Photo {
                    for (k, b) in Self::options(r, row + 1, 5).into_iter().enumerate() {
                        if b.contains(x, y) {
                            new.motion = MOTIONS[k].0;
                        }
                    }
                    row += 3;
                } else {
                    let (minus, plus) = Self::stepper(r, row);
                    if minus.contains(x, y) {
                        new.volume = new.volume.saturating_sub(10);
                    }
                    if plus.contains(x, y) {
                        new.volume = (new.volume + 10).min(200);
                    }
                    row += 1;
                }
                for (k, b) in Self::options(r, row + 1, 3).into_iter().enumerate() {
                    if b.contains(x, y) {
                        new.trans = TRANSITIONS[k].0;
                    }
                }
            }
            (_, Some(Kind::Music)) => {
                let (minus, plus) = Self::stepper(r, 0);
                if minus.contains(x, y) {
                    new.volume = new.volume.saturating_sub(10);
                }
                if plus.contains(x, y) {
                    new.volume = (new.volume + 10).min(200);
                }
                let (_, plus) = Self::stepper(r, 1);
                if Rect::new(plus.x - 100, plus.y, 140, plus.h).contains(x, y) {
                    new.fades = !new.fades;
                }
            }
            _ => {}
        }
        if format!("{:?}", new) != format!("{:?}", clip) {
            self.checkpoint();
            self.project.clips[i] = new;
            self.changed();
            // Layout may reorder clips: keep the selection on this one.
            self.reselect(&clip, i);
            return MovieAction::Redraw;
        }
        MovieAction::None
    }

    /// After a change that may re-sort clips, select the clip that was at `i`.
    fn reselect(&mut self, was: &Clip, i: usize) {
        if self.project.clips.get(i).is_some_and(|c| c.track == was.track && c.media == was.media) {
            self.sel = Some(i);
            return;
        }
        self.sel = self.project.clips.iter().position(|c| c.track == was.track && c.media == was.media && c.offset == was.offset);
    }

    pub fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        self.cursor = (x, y);
        if self.scrubbing {
            self.set_playhead(self.t_of(r, x));
            return true;
        }
        let Some(d) = self.drag.as_mut() else { return false };
        if !d.moved && (x - d.x0).abs() < 4 {
            return false;
        }
        if !d.moved {
            d.moved = true;
            let snapshot = self.project.clips.clone();
            self.undo.push(snapshot);
            self.redo.clear();
        }
        let d = self.drag.as_ref().unwrap();
        let dt = (x - d.x0) as i64 * 1000 / self.zoom;
        let (i, grab, orig) = (d.clip, d.grab, d.orig.clone());
        let max = self.project.max_len(&Clip { offset: 0, ..orig.clone() });
        let c = &mut self.project.clips[i];
        let is_photo = orig.media.is_some_and(|m| self.project.media[m].kind == Kind::Photo);
        match grab {
            Grab::Move => {
                if orig.track == Track::Video {
                    return true; // reordered on release
                }
                let mut start = (orig.start as i64 + dt).max(0) as u64;
                // Snap to the playhead.
                if start.abs_diff(self.playhead) * self.zoom as u64 / 1000 < 8 {
                    start = self.playhead;
                }
                c.start = start;
                if matches!(orig.track, Track::Music1 | Track::Music2) {
                    let m2 = Self::track_rect(r, 3);
                    c.track = if y >= m2.y { Track::Music2 } else if y >= Self::track_rect(r, 2).y { Track::Music1 } else { c.track };
                }
            }
            Grab::Right => {
                let len = (orig.len as i64 + dt).max(MIN_MS as i64) as u64;
                let limit = if orig.track == Track::Titles || is_photo { 10 * 60 * 1000 } else { max.saturating_sub(orig.offset) };
                c.len = len.min(limit.max(MIN_MS));
            }
            Grab::Left => {
                // Pull the start in (or back out); the end stays where it is.
                let lo = if orig.track == Track::Titles || is_photo || orig.track == Track::Video { -(orig.len as i64) * 4 } else { -(orig.offset as i64) };
                let lo = if orig.track != Track::Video && orig.track != Track::Titles { lo.max(-(orig.start as i64)).max(-(orig.offset as i64)) } else if orig.track == Track::Titles { lo.max(-(orig.start as i64)) } else { lo };
                let mut d = dt.clamp(lo, orig.len as i64 - MIN_MS as i64);
                if !is_photo && orig.track != Track::Titles {
                    d = d.max(-(orig.offset as i64));
                }
                c.len = (orig.len as i64 - d) as u64;
                if !is_photo && orig.track != Track::Titles {
                    c.offset = (orig.offset as i64 + d) as u64;
                }
                if orig.track != Track::Video {
                    c.start = (orig.start as i64 + d) as u64;
                }
            }
        }
        // Keep the selection on the dragged clip through re-layout (only the main track
        // re-sorts, and dragging it never changes its order).
        self.project.layout();
        if orig.track != Track::Video {
            let id = (orig.media, orig.offset, orig.text.clone());
            self.sel = self.project.clips.iter().position(|c| c.track != Track::Video && (c.media, c.offset, c.text.clone()) == id && (c.track == orig.track || matches!(c.track, Track::Music1 | Track::Music2)));
            if let (Some(s), Some(d)) = (self.sel, self.drag.as_mut()) {
                d.clip = s;
            }
        }
        self.preview_t = None;
        true
    }

    pub fn on_release(&mut self) -> bool {
        self.scrubbing = false;
        let Some(d) = self.drag.take() else { return false };
        if !d.moved {
            return true;
        }
        if d.grab == Grab::Move && d.orig.track == Track::Video {
            let r = self.body;
            let (_, at) = self.drop_point(r, &d);
            let clip = self.project.clips.remove(d.clip);
            let vids = self.project.video_indices();
            let pos = vids.get(at).copied().unwrap_or(vids.last().map(|&v| v + 1).unwrap_or(0));
            self.project.clips.insert(pos, clip);
            self.sel = Some(pos);
        }
        self.changed();
        if let Some(s) = self.sel {
            if let Some(c) = self.project.clips.get(s) {
                if self.playhead < c.start || self.playhead >= c.end() {
                    self.set_playhead(c.start);
                }
            }
        }
        true
    }

    fn finish_text(&mut self) {
        let Some(t) = self.text.take() else { return };
        if let Some(i) = self.sel {
            if self.project.clips[i].track == Track::Titles && self.project.clips[i].text != t.text {
                self.checkpoint();
                self.project.clips[i].text = t.text;
                self.changed();
            }
        }
    }

    fn start_export(&mut self) -> MovieAction {
        let (w, h, _) = SIZES[self.size];
        let name = String::from(self.name.text.trim());
        self.dialog = Dialog::None;
        self.export = Some((Export::new(&self.project, w, h), name));
        kprintln!("movie: export {}x{} started", w, h);
        MovieAction::Redraw
    }

    pub fn on_key(&mut self, sc: u8) -> Option<MovieAction> {
        match sc {
            0x1D => self.ctrl = true,
            0x9D => self.ctrl = false,
            0x2A | 0x36 => self.shift = true,
            0xAA | 0xB6 => self.shift = false,
            _ => {}
        }
        if self.export.is_some() {
            if sc == 0x01 {
                self.export = None;
                return Some(MovieAction::Toast(String::from("Export cancelled")));
            }
            return Some(MovieAction::None);
        }
        if self.dialog == Dialog::Export {
            return Some(match self.name.on_key(sc) {
                TextEvent::Submit if !self.name.text.trim().is_empty() => self.start_export(),
                TextEvent::Cancel => {
                    self.dialog = Dialog::None;
                    MovieAction::Redraw
                }
                TextEvent::Changed => MovieAction::Redraw,
                _ => MovieAction::None,
            });
        }
        if self.dialog == Dialog::NewMovie {
            if sc == 0x01 {
                self.dialog = Dialog::None;
                return Some(MovieAction::Redraw);
            }
            return Some(MovieAction::None);
        }
        if let Some(t) = self.text.as_mut() {
            return Some(match t.on_key(sc) {
                TextEvent::Submit | TextEvent::Cancel => {
                    self.finish_text();
                    MovieAction::Redraw
                }
                TextEvent::Changed => {
                    // Show the new text in the preview as it's typed.
                    if let Some(i) = self.sel {
                        self.project.clips[i].text = t.text.clone();
                        self.preview_t = None;
                        self.save_at = Some(TICKS.load(Relaxed) + 2 * HZ);
                    }
                    MovieAction::Redraw
                }
                TextEvent::None => MovieAction::None,
            });
        }
        if sc & 0x80 != 0 {
            return None;
        }
        let frame = 1000 / movie::FPS;
        Some(match sc {
            0x39 => {
                self.toggle_play();
                MovieAction::Redraw
            }
            0x1F if !self.ctrl => self.split(),           // S
            0x53 | 0x0E => self.delete(),                 // Delete, Backspace
            0x2C if self.ctrl => self.undo_redo(false),   // Ctrl+Z
            0x15 if self.ctrl => self.undo_redo(true),    // Ctrl+Y
            0x14 if !self.ctrl => self.add_title(),       // T
            0x4B | 0x4D => {
                self.stop();
                let d = if self.shift { 1000 } else { frame };
                let t = if sc == 0x4B { self.playhead.saturating_sub(d) } else { self.playhead + d };
                self.set_playhead(t);
                self.ensure_visible(self.playhead, self.playhead);
                MovieAction::Redraw
            }
            0x47 => {
                self.set_playhead(0);
                self.scroll = 0;
                MovieAction::Redraw
            }
            0x4F => {
                let d = self.project.duration();
                self.set_playhead(d);
                self.ensure_visible(d, d);
                MovieAction::Redraw
            }
            0x01 if self.sel.is_some() => {
                self.sel = None;
                MovieAction::Redraw
            }
            _ => return None,
        })
    }

    /// Closing the window: stop the sound, save now.
    pub fn closed(&mut self, storage: &mut Storage, usb: &mut Usb) {
        self.stop();
        self.finish_text();
        if self.save_at.take().is_some() {
            self.save_project(storage, usb);
        }
    }
}

fn note_icon(c: &mut Canvas, x: i32, y: i32) {
    c.circle(x + 5, y + 15, 5, TEXT);
    c.circle(x + 17, y + 13, 5, TEXT);
    c.fill_rect(x + 8, y + 1, 2, 14, TEXT);
    c.fill_rect(x + 20, y - 1, 2, 14, TEXT);
    c.fill_rect(x + 8, y - 1, 14, 3, TEXT);
}

/// Nearest-neighbour scaled copy of an image (small timeline pictures).
fn draw_scaled(c: &mut Canvas, img: &crate::image::Image, x: i32, y: i32, w: i32, h: i32) {
    if w <= 0 || h <= 0 {
        return;
    }
    for yy in 0..h {
        let sy = (yy as usize * img.h / h as usize).min(img.h - 1);
        for xx in 0..w {
            let sx = (xx as usize * img.w / w as usize).min(img.w - 1);
            let p = img.px[sy * img.w + sx];
            c.blend(x + xx, y + yy, p & 0xFF_FFFF, 255);
        }
    }
}


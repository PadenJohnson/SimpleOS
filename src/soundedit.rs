//! The Sound Editor: a song (any sound file Solon plays) shown as its waveform, to cut, copy,
//! paste and delete parts of, keep only a part of, fade in or out, make louder or quieter,
//! even out (bring the loudest part up to just below full) or silence; with undo and redo.
//! It plays from the cursor (or just the selection) and saves a copy as FLAC (lossless) or
//! WAV in Solon storage's Music folder, never over the original.
//!
//! The whole song is decoded first, at 48 kHz stereo (about 11 MB a minute), a little each
//! tick so the window stays responsive.

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::interrupts::{HZ, TICKS};
use crate::library::Storage;
use crate::media::Engine;
use crate::movie::Io;
use crate::movieui::LibIo;
use crate::photos::{ellipsize, Source};
use crate::usb::Usb;

const BG: u32 = rgb(246, 247, 250);
const PANEL: u32 = rgb(236, 238, 243);
const LINE: u32 = rgb(222, 225, 232);
const INK: u32 = rgb(28, 32, 40);
const DIM: u32 = rgb(112, 120, 132);
const ACCENT: u32 = rgb(32, 120, 210);
const WAVE_BG: u32 = rgb(24, 28, 36);
const WAVE: u32 = rgb(92, 196, 255);
const WAVE_SEL: u32 = rgb(255, 214, 102);
const SEL_BG: u32 = rgb(58, 66, 86);
const WHITE: u32 = rgb(255, 255, 255);

const RATE: usize = 48_000;
/// Frames per entry of the peak summary used for drawing zoomed-out views.
const PEAK: usize = 256;
/// Undo steps kept (and at most this many bytes of them).
const UNDO_STEPS: usize = 30;
const UNDO_BYTES: usize = 1 << 30;

const BAR_H: i32 = 48;

/// One change: frames `at..at + removed.len()/2` were replaced by `inserted`.
struct Change {
    at: usize,
    removed: Vec<i16>,
    inserted: Vec<i16>,
    what: &'static str,
}

/// A song being opened: decoded a little each tick.
struct Loading {
    engine: Engine,
    src: Source,
    pos: u64,
    dur: u64,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Open,
    SaveFlac,
    SaveWav,
    Undo,
    Redo,
    Play,
    Stop,
    Cut,
    Copy,
    Paste,
    Delete,
    Keep,
    All,
    FadeIn,
    FadeOut,
    Louder,
    Quieter,
    Even,
    Silence,
    ZoomIn,
    ZoomOut,
    Fit,
}

/// The buttons, in rows: (button, label).
const ROWS: [&[(Btn, &str)]; 3] = [
    &[(Btn::Open, "Open a song"), (Btn::SaveFlac, "Save a copy (FLAC)"), (Btn::SaveWav, "Save a copy (WAV)"), (Btn::Undo, "Undo"), (Btn::Redo, "Redo")],
    &[(Btn::Play, "Play"), (Btn::Stop, "Stop"), (Btn::Cut, "Cut"), (Btn::Copy, "Copy"), (Btn::Paste, "Paste"), (Btn::Delete, "Delete"), (Btn::Keep, "Keep only this"), (Btn::All, "Select all")],
    &[(Btn::FadeIn, "Fade in"), (Btn::FadeOut, "Fade out"), (Btn::Louder, "Louder"), (Btn::Quieter, "Quieter"), (Btn::Even, "Even out"), (Btn::Silence, "Silence"), (Btn::ZoomOut, "Zoom out"), (Btn::ZoomIn, "Zoom in"), (Btn::Fit, "Whole song")],
];

struct Play {
    /// Where playing started (frame) and where that is in the sound output.
    from: usize,
    base: u64,
    /// Stop here (the end of the selection, or of the song).
    until: usize,
    pos: usize,
}

pub enum EditAction {
    None,
    Redraw,
    /// Show the list of songs to open (the desktop gathers them from the Music app).
    WantSongs,
}

pub struct SoundEditor {
    /// The song: 48 kHz interleaved stereo.
    pcm: Vec<i16>,
    name: String,
    title: String,
    artist: String,
    album: String,
    /// Peak summary: (min, max) of each channel per PEAK frames.
    peaks: Vec<[(i16, i16); 2]>,
    loading: Option<Loading>,
    problem: Option<String>,
    /// Selection (start, end) in frames, and the cursor.
    sel: Option<(usize, usize)>,
    cursor: usize,
    /// The view: first frame shown and frames per pixel.
    view: usize,
    zoom: f64,
    undo: Vec<Change>,
    redo: Vec<Change>,
    clip: Vec<i16>,
    play: Option<Play>,
    body: Rect,
    /// Dragging: 1 = selecting (from this frame), 2 = the overview.
    drag: Option<(u8, usize)>,
    /// Choosing a song: (name, place, source, size), and the list's scroll.
    choosing: Option<Vec<(String, String, Source, u64)>>,
    choose_scroll: i32,
    message: Option<(String, u64)>,
    /// Edited since the last save.
    changed: bool,
    keys: crate::keys::Keys,
}

fn time_text(frames: usize) -> String {
    let ms = frames as u64 * 1000 / RATE as u64;
    let s = ms / 1000;
    format!("{}:{:02}.{}", s / 60, s % 60, (ms % 1000) / 100)
}

impl SoundEditor {
    pub fn new() -> SoundEditor {
        SoundEditor {
            pcm: Vec::new(),
            name: String::new(),
            title: String::new(),
            artist: String::new(),
            album: String::new(),
            peaks: Vec::new(),
            loading: None,
            problem: None,
            sel: None,
            cursor: 0,
            view: 0,
            zoom: 1000.0,
            undo: Vec::new(),
            redo: Vec::new(),
            clip: Vec::new(),
            play: None,
            body: Rect::new(0, 0, 0, 0),
            drag: None,
            choosing: None,
            choose_scroll: 0,
            message: None,
            changed: false,
            keys: crate::keys::Keys::default(),
        }
    }

    fn frames(&self) -> usize {
        self.pcm.len() / 2
    }

    pub fn title(&self) -> Option<String> {
        if self.name.is_empty() {
            None
        } else {
            Some(format!("Sound Editor - {}{}", self.title, if self.changed { " (changed)" } else { "" }))
        }
    }

    /// The window opened with nothing in it: ask for the songs to choose from.
    pub fn opened(&mut self) -> EditAction {
        if self.pcm.is_empty() && self.loading.is_none() {
            EditAction::WantSongs
        } else {
            EditAction::None
        }
    }

    /// The songs to choose from (from the Music app).
    pub fn choose_from(&mut self, songs: Vec<(String, String, Source, u64)>) {
        self.choosing = Some(songs);
        self.choose_scroll = 0;
    }

    /// Start opening a song.
    pub fn open(&mut self, src: Source, name: String, io: &mut LibIo) {
        self.stop();
        self.choosing = None;
        let size = match &src {
            Source::Memory(b) => b.len() as u64,
            Source::Usb { entry, .. } | Source::Library { entry, .. } | Source::Boot { entry } => entry.size,
        };
        let tags = {
            let mut read = |off: u64, len: usize| io.read(&src, off, len);
            crate::media::tags::read(&mut read, size)
        };
        let opened = {
            let mut read = |off: u64, len: usize| io.read(&src, off, len);
            Engine::open(&mut read, size)
        };
        let stem = name.rsplit_once('.').map(|(a, _)| String::from(a)).unwrap_or_else(|| name.clone());
        self.title = tags.title.unwrap_or(stem);
        self.artist = tags.artist.unwrap_or_default();
        self.album = tags.album.unwrap_or_default();
        self.name = name;
        self.pcm.clear();
        self.peaks.clear();
        self.undo.clear();
        self.redo.clear();
        self.sel = None;
        self.cursor = 0;
        self.view = 0;
        self.changed = false;
        self.problem = None;
        match opened {
            Ok(e) if e.has_sound() => {
                let dur = e.duration_ms;
                kprintln!("soundedit: opening \"{}\" ({} ms)", self.name, dur);
                self.pcm.reserve((dur as usize * RATE / 1000 + RATE) * 2);
                self.loading = Some(Loading { engine: e, src, pos: 0, dur });
            }
            Ok(e) => self.problem = Some(e.sound_problem.clone().unwrap_or_else(|| String::from("This file has no sound"))),
            Err(e) => self.problem = Some(e),
        }
    }

    /// Decode some more of the song being opened (about 40 ms of work).
    fn load_more(&mut self, io: &mut LibIo) -> bool {
        let Some(l) = self.loading.as_mut() else { return false };
        let t0 = crate::cpu::now_us();
        while crate::cpu::now_us() - t0 < 40_000 {
            let n = RATE; // a second at a time
            let mut read = |off: u64, len: usize| io.read(&l.src, off, len);
            let mut pcm = l.engine.sound(&mut read, l.pos, n);
            let end = l.engine.sound_end();
            if let Some(e) = end {
                // Only up to the real end (the rest of this piece is silence after it).
                let keep = (e.saturating_sub(l.pos) as usize).min(n);
                pcm.truncate(keep * 2);
            }
            self.pcm.extend_from_slice(&pcm);
            l.pos += (pcm.len() / 2) as u64;
            // (A file that never says it has ended stops at its length plus a minute, or 3 hours.)
            let limit = if l.dur > 0 { (l.dur + 60_000) * 48 } else { 3 * 3600 * RATE as u64 };
            if end.is_some_and(|e| l.pos >= e) || pcm.is_empty() || l.pos >= limit {
                kprintln!("soundedit: opened \"{}\": {} s", self.name, l.pos / RATE as u64);
                self.loading = None;
                self.make_peaks();
                self.fit();
                return true;
            }
        }
        true
    }

    fn make_peaks(&mut self) {
        let n = self.frames().div_ceil(PEAK);
        self.peaks = (0..n)
            .map(|k| {
                let mut p = [(i16::MAX, i16::MIN); 2];
                let end = ((k + 1) * PEAK).min(self.frames());
                for i in k * PEAK..end {
                    for (c, pc) in p.iter_mut().enumerate() {
                        let v = self.pcm[i * 2 + c];
                        pc.0 = pc.0.min(v);
                        pc.1 = pc.1.max(v);
                    }
                }
                p
            })
            .collect();
    }

    /// Redo the peak summary from frame `at` on (after an edit there).
    fn peaks_from(&mut self, at: usize) {
        let k0 = at / PEAK;
        self.peaks.truncate(k0);
        let n = self.frames().div_ceil(PEAK);
        for k in k0..n {
            let mut p = [(i16::MAX, i16::MIN); 2];
            let end = ((k + 1) * PEAK).min(self.frames());
            for i in k * PEAK..end {
                for (c, pc) in p.iter_mut().enumerate() {
                    let v = self.pcm[i * 2 + c];
                    pc.0 = pc.0.min(v);
                    pc.1 = pc.1.max(v);
                }
            }
            self.peaks.push(p);
        }
    }

    // ---- editing ---------------------------------------------------------------------------

    /// Replace frames `at..at + len` with `new`, remembering it for undo.
    fn change(&mut self, at: usize, len: usize, new: Vec<i16>, what: &'static str) {
        self.stop();
        let removed: Vec<i16> = self.pcm.splice(at * 2..(at + len) * 2, new.iter().copied()).collect();
        self.undo.push(Change { at, removed, inserted: new, what });
        self.redo.clear();
        // Keep the undo history within bounds.
        while self.undo.len() > UNDO_STEPS || self.undo.iter().map(|c| (c.removed.len() + c.inserted.len()) * 2).sum::<usize>() > UNDO_BYTES {
            self.undo.remove(0);
        }
        self.changed = true;
        self.peaks_from(at);
    }

    /// The selection, or nothing (with a message).
    fn need_sel(&mut self, now: u64) -> Option<(usize, usize)> {
        match self.sel {
            Some((a, b)) if b > a => Some((a, b)),
            _ => {
                self.message = Some((String::from("First choose a part: drag across the waveform"), now + 3 * HZ));
                None
            }
        }
    }

    fn undo(&mut self) {
        let Some(c) = self.undo.pop() else { return };
        self.stop();
        self.pcm.splice(c.at * 2..c.at * 2 + c.inserted.len(), c.removed.iter().copied());
        self.sel = Some((c.at, c.at + c.removed.len() / 2)).filter(|s| s.1 > s.0);
        self.cursor = c.at;
        self.peaks_from(c.at);
        self.redo.push(c);
        self.changed = true;
    }

    fn redo(&mut self) {
        let Some(c) = self.redo.pop() else { return };
        self.stop();
        self.pcm.splice(c.at * 2..c.at * 2 + c.removed.len(), c.inserted.iter().copied());
        self.sel = Some((c.at, c.at + c.inserted.len() / 2)).filter(|s| s.1 > s.0);
        self.cursor = c.at;
        self.peaks_from(c.at);
        self.undo.push(c);
        self.changed = true;
    }

    /// Apply `f(frame index within the part, part length, sample)` over the selection (or the
    /// whole song when nothing is selected).
    fn effect(&mut self, what: &'static str, f: impl Fn(usize, usize, i32) -> i32) {
        let (a, b) = self.sel.filter(|s| s.1 > s.0).unwrap_or((0, self.frames()));
        if b <= a {
            return;
        }
        let n = b - a;
        let new: Vec<i16> = (0..n * 2).map(|k| f(k / 2, n, self.pcm[a * 2 + k] as i32).clamp(-32768, 32767) as i16).collect();
        self.change(a, n, new, what);
        self.sel = Some((a, b));
    }

    fn button(&mut self, b: Btn, now: u64, io: &mut LibIo) -> EditAction {
        if self.loading.is_some() && !matches!(b, Btn::Open) {
            return EditAction::None;
        }
        match b {
            Btn::Open => return EditAction::WantSongs,
            Btn::SaveFlac | Btn::SaveWav => self.save(b == Btn::SaveFlac, now, io),
            Btn::Undo => self.undo(),
            Btn::Redo => self.redo(),
            Btn::Play => self.toggle_play(),
            Btn::Stop => {
                self.stop();
                self.cursor = self.sel.map(|s| s.0).unwrap_or(0);
            }
            Btn::Cut | Btn::Copy => {
                let Some((a, z)) = self.need_sel(now) else { return EditAction::Redraw };
                self.clip = self.pcm[a * 2..z * 2].to_vec();
                if b == Btn::Cut {
                    self.change(a, z - a, Vec::new(), "Cut");
                    self.sel = None;
                    self.cursor = a;
                }
            }
            Btn::Paste => {
                if self.clip.is_empty() {
                    self.message = Some((String::from("Nothing to paste yet: Copy or Cut a part first"), now + 3 * HZ));
                } else {
                    // Over the selection, or at the cursor.
                    let (a, z) = self.sel.filter(|s| s.1 > s.0).unwrap_or((self.cursor, self.cursor));
                    let clip = self.clip.clone();
                    let n = clip.len() / 2;
                    self.change(a, z - a, clip, "Paste");
                    self.sel = Some((a, a + n));
                }
            }
            Btn::Delete => {
                let Some((a, z)) = self.need_sel(now) else { return EditAction::Redraw };
                self.change(a, z - a, Vec::new(), "Delete");
                self.sel = None;
                self.cursor = a;
            }
            Btn::Keep => {
                // One change (so one Undo brings the rest back): the song becomes the part.
                let Some((a, z)) = self.need_sel(now) else { return EditAction::Redraw };
                let part = self.pcm[a * 2..z * 2].to_vec();
                let all = self.frames();
                self.change(0, all, part, "Keep only this");
                self.sel = None;
                self.cursor = 0;
                self.fit();
            }
            Btn::All => self.sel = Some((0, self.frames())),
            Btn::FadeIn => self.effect("Fade in", |i, n, v| (v as i64 * i as i64 / n as i64) as i32),
            Btn::FadeOut => self.effect("Fade out", |i, n, v| (v as i64 * (n - i) as i64 / n as i64) as i32),
            Btn::Louder => self.effect("Louder", |_, _, v| v * 1414 / 1000), // +3 dB
            Btn::Quieter => self.effect("Quieter", |_, _, v| v * 707 / 1000), // -3 dB
            Btn::Even => {
                // The loudest sample up to 90% of full (about -1 dB).
                let (a, z) = self.sel.filter(|s| s.1 > s.0).unwrap_or((0, self.frames()));
                let peak = self.pcm[a * 2..z * 2].iter().map(|&v| (v as i32).abs()).max().unwrap_or(0);
                if peak > 0 {
                    let target = 29_490;
                    self.effect("Even out", move |_, _, v| (v as i64 * target / peak as i64) as i32);
                }
            }
            Btn::Silence => {
                let Some((a, z)) = self.need_sel(now) else { return EditAction::Redraw };
                self.change(a, z - a, vec![0; (z - a) * 2], "Silence");
                self.sel = Some((a, z));
            }
            Btn::ZoomIn => self.zoom_at(0.5, None),
            Btn::ZoomOut => self.zoom_at(2.0, None),
            Btn::Fit => self.fit(),
        }
        EditAction::Redraw
    }

    // ---- saving -------------------------------------------------------------------------------

    fn save(&mut self, flac: bool, now: u64, io: &mut LibIo) {
        if self.pcm.is_empty() {
            return;
        }
        if io.storage.library.is_none() {
            self.message = Some((String::from("There's no Solon storage to save in"), now + 4 * HZ));
            return;
        }
        let data = if flac { self.flac_file() } else { self.wav_file() };
        let ext = if flac { "flac" } else { "wav" };
        let result = (|| -> Result<String, crate::fs::FsError> {
            let root = io.storage.root(io.usb)?;
            let list = io.storage.list(io.usb, &root)?;
            let dir = match list.into_iter().find(|e| e.is_dir && e.name.eq_ignore_ascii_case("Music")) {
                Some(d) => d,
                None => io.storage.mkdir_in(io.usb, &root, "Music")?,
            };
            let names = io.storage.names_in(io.usb, &dir)?;
            let safe: String = self.title.chars().map(|c| if "\\/:*?\"<>|".contains(c) { '-' } else { c }).collect();
            let want = format!("{} (edited).{}", safe.trim(), ext);
            let name = crate::library::unique_name(&names, &want, data.len() as u64).unwrap_or(want);
            io.storage.save_file(io.usb, &dir, None, &name, &data)?;
            Ok(name)
        })();
        match result {
            Ok(name) => {
                kprintln!("soundedit: saved Music/{} ({} bytes)", name, data.len());
                self.message = Some((format!("Saved as Music/{} in Solon storage", name), now + 5 * HZ));
                self.changed = false;
            }
            Err(e) => self.message = Some((format!("Couldn't save: {}", e.message()), now + 5 * HZ)),
        }
    }

    fn wav_file(&self) -> Vec<u8> {
        let data_len = self.pcm.len() * 2;
        let mut out = Vec::with_capacity(44 + data_len);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&(RATE as u32).to_le_bytes());
        out.extend_from_slice(&(RATE as u32 * 4).to_le_bytes());
        out.extend_from_slice(&4u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data_len as u32).to_le_bytes());
        for s in &self.pcm {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }

    fn flac_file(&self) -> Vec<u8> {
        use crate::video::flac;
        let mut enc = flac::Encoder::new(RATE as u32);
        let mut frames = Vec::with_capacity(self.pcm.len());
        for block in self.pcm.chunks(flac::BLOCK * 2) {
            frames.extend_from_slice(&enc.frame(block));
        }
        // The tags, as a Vorbis comment block.
        let mut vc = Vec::new();
        let vendor = b"Solon Sound Editor";
        vc.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
        vc.extend_from_slice(vendor);
        let mut comments = vec![format!("TITLE={} (edited)", self.title)];
        if !self.artist.is_empty() {
            comments.push(format!("ARTIST={}", self.artist));
        }
        if !self.album.is_empty() {
            comments.push(format!("ALBUM={}", self.album));
        }
        vc.extend_from_slice(&(comments.len() as u32).to_le_bytes());
        for c in &comments {
            vc.extend_from_slice(&(c.len() as u32).to_le_bytes());
            vc.extend_from_slice(c.as_bytes());
        }
        let mut out = Vec::with_capacity(frames.len() + 256);
        out.extend_from_slice(b"fLaC");
        out.extend_from_slice(&[0x00, 0, 0, 34]);
        out.extend_from_slice(&enc.streaminfo());
        let n = vc.len() as u32;
        out.extend_from_slice(&[0x84, (n >> 16) as u8, (n >> 8) as u8, n as u8]);
        out.extend_from_slice(&vc);
        out.extend_from_slice(&frames);
        out
    }

    // ---- playing ---------------------------------------------------------------------------------

    fn toggle_play(&mut self) {
        if self.play.is_some() {
            self.stop();
            return;
        }
        if self.pcm.is_empty() || !crate::hda::available() {
            return;
        }
        let (from, until) = match self.sel {
            Some((a, b)) if b > a => (a, b),
            _ => (self.cursor.min(self.frames()), self.frames()),
        };
        crate::hda::start(crate::hda::SOUND_EDITOR);
        let base = crate::hda::write_pos();
        self.play = Some(Play { from, base, until, pos: from });
        self.feed();
    }

    fn stop(&mut self) {
        if let Some(p) = self.play.take() {
            crate::hda::stop(crate::hda::SOUND_EDITOR);
            if self.sel.is_none() {
                self.cursor = p.pos;
            }
        }
    }

    /// Keep the sound output topped up from the song.
    fn feed(&mut self) {
        let Some(p) = self.play.as_ref() else { return };
        let want = crate::hda::wanted(crate::hda::SOUND_EDITOR);
        if want == 0 {
            return;
        }
        let at = crate::hda::write_pos();
        let start = p.from + at.saturating_sub(p.base) as usize;
        let end = (start + want).min(p.until);
        let mut pcm = if start < end { self.pcm[start * 2..end * 2].to_vec() } else { Vec::new() };
        // Silence after the end (so the last bit plays out cleanly).
        pcm.resize(want * 2, 0);
        crate::hda::queue(crate::hda::SOUND_EDITOR, &pcm);
    }

    pub fn tick(&mut self, _now: u64, storage: &mut Storage, usb: &mut Usb, visible: bool) -> EditAction {
        let mut io = LibIo { storage, usb };
        let mut redraw = false;
        if self.loading.is_some() {
            redraw |= self.load_more(&mut io);
        }
        if self.play.is_some() && crate::hda::owner() != crate::hda::SOUND_EDITOR {
            self.play = None;
            redraw = true;
        }
        if let Some(p) = self.play.as_mut() {
            p.pos = (p.from + crate::hda::played().saturating_sub(p.base) as usize).min(p.until);
            let done = p.pos >= p.until;
            if done {
                self.stop();
                self.cursor = self.sel.map(|s| s.0).unwrap_or(0);
            } else {
                self.feed();
                // Keep the playhead in view.
                let w = self.wave_rect(self.body).w.max(1) as f64;
                if let Some(p) = &self.play {
                    if (p.pos as f64) > self.view as f64 + w * self.zoom * 0.95 {
                        self.view = p.pos;
                    }
                }
            }
            redraw = true;
        }
        if let Some((_, until)) = &self.message {
            if TICKS.load(Relaxed) >= *until {
                self.message = None;
                redraw = true;
            }
        }
        if redraw && visible {
            EditAction::Redraw
        } else {
            EditAction::None
        }
    }

    pub fn closed(&mut self) {
        self.stop();
    }

    // ---- layout --------------------------------------------------------------------------------

    fn buttons(&self, r: Rect) -> Vec<(Rect, Btn, &'static str)> {
        let mut out = Vec::new();
        for (row, items) in ROWS.iter().enumerate() {
            let mut x = r.x + 12;
            let y = r.y + 10 + row as i32 * BAR_H;
            for &(b, label) in items.iter() {
                // Undo and Redo get room for what they'd undo.
                let shown = if matches!(b, Btn::Undo | Btn::Redo) { "Undo Keep only this" } else { label };
                let w = text_width(shown, Size::Small, true) + 28;
                out.push((Rect::new(x, y, w, 36), b, label));
                x += w + 8;
            }
        }
        out
    }
    fn top_h() -> i32 {
        10 + ROWS.len() as i32 * BAR_H + 4
    }
    fn wave_rect(&self, r: Rect) -> Rect {
        let top = r.y + Self::top_h() + 30;
        Rect::new(r.x + 16, top, r.w - 32, (r.h - Self::top_h() - 30 - 96).max(60))
    }
    fn overview(&self, r: Rect) -> Rect {
        let w = self.wave_rect(r);
        Rect::new(w.x, w.y + w.h + 14, w.w, 28)
    }
    fn fit(&mut self) {
        let w = self.wave_rect(self.body).w.max(1) as f64;
        self.view = 0;
        self.zoom = (self.frames() as f64 / w).max(1.0 / 16.0);
    }
    /// Zoom by `factor` keeping the frame under `at_x` (or the middle) in place.
    fn zoom_at(&mut self, factor: f64, at_x: Option<i32>) {
        let w = self.wave_rect(self.body);
        let px = at_x.unwrap_or(w.x + w.w / 2).clamp(w.x, w.x + w.w) - w.x;
        let anchor = self.view as f64 + px as f64 * self.zoom;
        let max_zoom = (self.frames() as f64 / w.w.max(1) as f64).max(1.0 / 16.0);
        self.zoom = (self.zoom * factor).clamp(1.0 / 16.0, max_zoom);
        self.view = (anchor - px as f64 * self.zoom).max(0.0) as usize;
        self.clamp_view();
    }
    fn clamp_view(&mut self) {
        let w = self.wave_rect(self.body).w.max(1) as f64;
        let max = (self.frames() as f64 - w * self.zoom).max(0.0) as usize;
        self.view = self.view.min(max);
    }
    fn frame_at(&self, x: i32) -> usize {
        let w = self.wave_rect(self.body);
        ((self.view as f64 + (x - w.x).max(0) as f64 * self.zoom) as usize).min(self.frames())
    }
    fn x_of(&self, frame: usize) -> i32 {
        let w = self.wave_rect(self.body);
        w.x + ((frame as f64 - self.view as f64) / self.zoom) as i32
    }

    // ---- drawing -------------------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas, r: Rect) {
        let resized = self.body.w != r.w;
        self.body = r;
        if resized && !self.pcm.is_empty() && self.loading.is_none() {
            self.clamp_view();
        }
        c.fill_rect(r.x, r.y, r.w, r.h, BG);
        c.fill_rect(r.x, r.y, r.w, Self::top_h(), PANEL);
        c.fill_rect(r.x, r.y + Self::top_h() - 1, r.w, 1, LINE);
        if let Some(list) = &self.choosing {
            self.draw_choose(c, r, list.len());
            return;
        }
        let has = !self.pcm.is_empty() && self.loading.is_none();
        let sel = self.sel.is_some_and(|s| s.1 > s.0);
        for (rect, b, label) in self.buttons(r) {
            if rect.x + rect.w > r.x + r.w - 8 {
                continue;
            }
            let on = match b {
                Btn::Open => true,
                Btn::Undo => has && !self.undo.is_empty(),
                Btn::Redo => has && !self.redo.is_empty(),
                Btn::Cut | Btn::Copy | Btn::Delete | Btn::Keep | Btn::Silence => has && sel,
                Btn::Paste => has && !self.clip.is_empty(),
                _ => has,
            };
            let primary = matches!(b, Btn::Play) && on;
            c.round_rect(rect.x, rect.y, rect.w, rect.h, 8, if primary { ACCENT } else if on { WHITE } else { PANEL });
            // Undo and Redo say what they'd undo ("Undo Fade in").
            let what;
            let label: &str = match b {
                Btn::Play if self.play.is_some() => "Pause",
                Btn::Undo if on => {
                    what = format!("Undo {}", self.undo.last().map(|c| c.what).unwrap_or(""));
                    &what
                }
                Btn::Redo if on => {
                    what = format!("Redo {}", self.redo.last().map(|c| c.what).unwrap_or(""));
                    &what
                }
                _ => label,
            };
            c.text_centered(rect.x + rect.w / 2, rect.y + 10, label, Size::Small, true, if primary { WHITE } else if on { INK } else { rgb(170, 175, 185) });
        }
        let w = self.wave_rect(r);
        // The song's name above the waveform.
        let name = if self.name.is_empty() { String::from("No song open") } else if self.artist.is_empty() { self.title.clone() } else { format!("{} - {}", self.title, self.artist) };
        c.text(w.x, r.y + Self::top_h() + 6, &ellipsize(&name, w.w - 200, Size::Small, true), Size::Small, true, INK);
        if let Some(l) = &self.loading {
            let pct = if l.dur > 0 { (l.pos * 100 / (48 * l.dur)).min(100) } else { 0 };
            let msg = format!("Opening the song... {}%", pct);
            c.fill_rect(w.x, w.y, w.w, w.h, WAVE_BG);
            c.text_centered(w.x + w.w / 2, w.y + w.h / 2 - 8, &msg, Size::Medium, true, WHITE);
            return;
        }
        if let Some(p) = &self.problem {
            c.fill_rect(w.x, w.y, w.w, w.h, WAVE_BG);
            c.text_centered(w.x + w.w / 2, w.y + w.h / 2 - 8, p, Size::Small, true, WHITE);
            return;
        }
        if !has {
            c.fill_rect(w.x, w.y, w.w, w.h, WAVE_BG);
            c.text_centered(w.x + w.w / 2, w.y + w.h / 2 - 20, "Open a song to change it", Size::Medium, true, WHITE);
            c.text_centered(w.x + w.w / 2, w.y + w.h / 2 + 14, "Changes are saved as a copy: the original stays as it is.", Size::Small, false, rgb(180, 188, 200));
            return;
        }
        self.draw_wave(c, w);
        self.draw_overview(c, self.overview(r));
        // Status line.
        let y = self.overview(r).y + 40;
        let info = match self.sel {
            Some((a, b)) if b > a => format!("Selected {} to {} ({})", time_text(a), time_text(b), time_text(b - a)),
            _ => format!("Cursor at {}  |  Song {}", time_text(self.play.as_ref().map(|p| p.pos).unwrap_or(self.cursor)), time_text(self.frames())),
        };
        c.text(w.x, y, &info, Size::Small, false, DIM);
        let hint = "Drag across the waveform to choose a part; the mouse wheel zooms";
        c.text(w.x + w.w - text_width(hint, Size::Small, false), y, hint, Size::Small, false, DIM);
        if let Some((m, _)) = &self.message {
            let tw = text_width(m, Size::Small, true) + 40;
            c.round_rect(r.x + (r.w - tw) / 2, w.y + 12, tw, 34, 17, rgb(36, 40, 50));
            c.text_centered(r.x + r.w / 2, w.y + 21, m, Size::Small, true, WHITE);
        }
    }

    fn draw_wave(&self, c: &mut Canvas, w: Rect) {
        c.fill_rect(w.x, w.y, w.w, w.h, WAVE_BG);
        let sel = self.sel.filter(|s| s.1 > s.0);
        if let Some((a, b)) = sel {
            let (x0, x1) = (self.x_of(a).max(w.x), self.x_of(b).min(w.x + w.w));
            if x1 > x0 {
                c.fill_rect(x0, w.y, x1 - x0, w.h, SEL_BG);
            }
        }
        // Time marks along the top.
        let secs_per_px = self.zoom / RATE as f64;
        let step = [0.01, 0.05, 0.1, 0.5, 1.0, 5.0, 10.0, 30.0, 60.0, 300.0, 600.0].into_iter().find(|&s| s / secs_per_px >= 90.0).unwrap_or(1200.0);
        let first = libm::ceil(self.view as f64 / RATE as f64 / step) * step;
        let mut t = first;
        while t * RATE as f64 <= self.view as f64 + w.w as f64 * self.zoom {
            let x = self.x_of((t * RATE as f64) as usize);
            c.fill_rect(x, w.y, 1, w.h, rgb(40, 46, 58));
            let s = t as u64;
            let label = if step < 1.0 { format!("{}:{:02}.{:02}", s / 60, s % 60, libm::round((t - s as f64) * 100.0) as u64) } else { format!("{}:{:02}", s / 60, s % 60) };
            if x + 4 + text_width(&label, Size::Small, false) < w.x + w.w {
                c.text(x + 4, w.y + 4, &label, Size::Small, false, rgb(130, 140, 156));
            }
            t += step;
        }
        // Two channels, one above the other.
        let lane = w.h / 2;
        for ch in 0..2 {
            let mid = w.y + lane * ch as i32 + lane / 2;
            let half = (lane / 2 - 4).max(1) as i64;
            c.fill_rect(w.x, mid, w.w, 1, rgb(60, 68, 84));
            for px in 0..w.w {
                let f0 = (self.view as f64 + px as f64 * self.zoom) as usize;
                let f1 = ((self.view as f64 + (px + 1) as f64 * self.zoom) as usize).max(f0 + 1).min(self.frames());
                if f0 >= self.frames() {
                    break;
                }
                let (lo, hi) = self.range_peak(f0, f1, ch);
                let y0 = mid - (hi as i64 * half / 32768) as i32;
                let y1 = mid - (lo as i64 * half / 32768) as i32;
                let col = if sel.is_some_and(|(a, b)| f0 >= a && f0 < b) { WAVE_SEL } else { WAVE };
                c.fill_rect(w.x + px, y0, 1, (y1 - y0).max(1), col);
            }
        }
        c.fill_rect(w.x, w.y + lane, w.w, 1, rgb(48, 54, 68));
        // The cursor and the playhead.
        let cx = self.x_of(self.cursor);
        if cx >= w.x && cx < w.x + w.w && self.play.is_none() {
            c.fill_rect(cx, w.y, 2, w.h, WHITE);
        }
        if let Some(p) = &self.play {
            let px = self.x_of(p.pos);
            if px >= w.x && px < w.x + w.w {
                c.fill_rect(px, w.y, 2, w.h, rgb(255, 90, 90));
            }
        }
    }

    /// The lowest and highest sample of channel `ch` in frames f0..f1.
    fn range_peak(&self, f0: usize, f1: usize, ch: usize) -> (i16, i16) {
        let (mut lo, mut hi) = (i16::MAX, i16::MIN);
        let mut f = f0;
        while f < f1 {
            if f % PEAK == 0 && f + PEAK <= f1 && f / PEAK < self.peaks.len() {
                let p = self.peaks[f / PEAK][ch];
                lo = lo.min(p.0);
                hi = hi.max(p.1);
                f += PEAK;
            } else {
                let v = self.pcm[f * 2 + ch];
                lo = lo.min(v);
                hi = hi.max(v);
                f += 1;
            }
        }
        if lo > hi {
            (0, 0)
        } else {
            (lo, hi)
        }
    }

    /// The whole song small, with the part shown marked: drag in it to move around.
    fn draw_overview(&self, c: &mut Canvas, o: Rect) {
        c.round_rect(o.x, o.y, o.w, o.h, 6, rgb(222, 226, 234));
        let n = self.frames().max(1);
        let mid = o.y + o.h / 2;
        for px in 0..o.w {
            let k0 = px as usize * self.peaks.len() / o.w.max(1) as usize;
            let k1 = ((px + 1) as usize * self.peaks.len() / o.w.max(1) as usize).max(k0 + 1).min(self.peaks.len());
            let mut m = 0i32;
            for k in k0..k1 {
                m = m.max((self.peaks[k][0].1 as i32).max(-(self.peaks[k][0].0 as i32)));
            }
            let h = (m * (o.h / 2 - 2) / 32768).max(1);
            c.fill_rect(o.x + px, mid - h, 1, h * 2, rgb(150, 160, 176));
        }
        let w = self.wave_rect(self.body);
        let a = o.x + (self.view as u64 * o.w as u64 / n as u64) as i32;
        let b = o.x + (((self.view as f64 + w.w as f64 * self.zoom) as u64).min(n as u64) * o.w as u64 / n as u64) as i32;
        c.fill_rect_alpha(a, o.y, (b - a).max(3), o.h, ACCENT, 70);
        c.fill_rect(a, o.y, 2, o.h, ACCENT);
        c.fill_rect((b - 2).max(a), o.y, 2, o.h, ACCENT);
    }

    fn draw_choose(&self, c: &mut Canvas, r: Rect, n: usize) {
        c.text(r.x + 20, r.y + 18, "Choose a song to open", Size::Medium, true, INK);
        let cancel = Rect::new(r.x + r.w - 130, r.y + 12, 110, 36);
        if !self.pcm.is_empty() {
            c.round_rect(cancel.x, cancel.y, cancel.w, cancel.h, 8, WHITE);
            c.text_centered(cancel.x + cancel.w / 2, cancel.y + 10, "Cancel", Size::Small, true, INK);
        }
        let area = Rect::new(r.x, r.y + Self::top_h(), r.w, r.h - Self::top_h());
        if n == 0 {
            c.text_centered(r.x + r.w / 2, area.y + 60, "No songs found. Songs in Solon storage, on USB drives and on phones show up here.", Size::Small, false, DIM);
            return;
        }
        c.set_clip(area);
        let list = self.choosing.as_ref().unwrap();
        for (k, (name, place, _, _)) in list.iter().enumerate() {
            let y = area.y + 8 + k as i32 * 40 - self.choose_scroll;
            if y + 40 < area.y || y > area.y + area.h {
                continue;
            }
            if k % 2 == 1 {
                c.fill_rect(r.x, y, r.w, 40, rgb(240, 241, 245));
            }
            c.text(r.x + 24, y + 11, &ellipsize(name, r.w - 300, Size::Small, true), Size::Small, true, INK);
            c.text(r.x + r.w - 240, y + 11, &ellipsize(place, 220, Size::Small, false), Size::Small, false, DIM);
        }
        c.reset_clip();
    }

    // ---- input -----------------------------------------------------------------------------------

    pub fn scroll_info(&self, r: Rect) -> Option<(Rect, i32, i32)> {
        let list = self.choosing.as_ref()?;
        let area = Rect::new(r.x, r.y + Self::top_h(), r.w, r.h - Self::top_h());
        let max = (list.len() as i32 * 40 + 16 - area.h).max(0);
        (max > 0).then_some((area, max, self.choose_scroll))
    }
    pub fn set_scroll(&mut self, v: i32) {
        self.choose_scroll = v.max(0);
    }

    pub fn on_wheel(&mut self, wheel: i8, x: i32, r: Rect) -> bool {
        self.body = r;
        if let Some(list) = &self.choosing {
            let area_h = r.h - Self::top_h();
            let max = (list.len() as i32 * 40 + 16 - area_h).max(0);
            self.choose_scroll = (self.choose_scroll - wheel as i32 * 80).clamp(0, max);
            return true;
        }
        if self.pcm.is_empty() {
            return false;
        }
        self.zoom_at(if wheel > 0 { 0.7 } else { 1.0 / 0.7 }, Some(x));
        true
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, storage: &mut Storage, usb: &mut Usb) -> EditAction {
        self.body = r;
        let mut io = LibIo { storage, usb };
        if self.choosing.is_some() {
            let cancel = Rect::new(r.x + r.w - 130, r.y + 12, 110, 36);
            if cancel.contains(x, y) && !self.pcm.is_empty() {
                self.choosing = None;
                return EditAction::Redraw;
            }
            let area_y = r.y + Self::top_h();
            if y > area_y {
                let k = (y - area_y - 8 + self.choose_scroll) / 40;
                let pick = self.choosing.as_ref().and_then(|l| l.get(k.max(0) as usize).cloned());
                if let Some((name, _, src, _)) = pick.filter(|_| k >= 0) {
                    self.open(src, name, &mut io);
                    return EditAction::Redraw;
                }
            }
            return EditAction::None;
        }
        for (rect, b, _) in self.buttons(r) {
            if rect.contains(x, y) && rect.x + rect.w <= r.x + r.w - 8 {
                return self.button(b, now, &mut io);
            }
        }
        if self.pcm.is_empty() || self.loading.is_some() {
            return EditAction::None;
        }
        let w = self.wave_rect(r);
        if w.contains(x, y) {
            let f = self.frame_at(x);
            self.cursor = f;
            self.sel = None;
            self.drag = Some((1, f));
            if self.play.is_some() {
                self.stop();
            }
            return EditAction::Redraw;
        }
        if self.overview(r).contains(x, y) {
            self.drag = Some((2, 0));
            self.overview_to(x);
            return EditAction::Redraw;
        }
        EditAction::None
    }

    fn overview_to(&mut self, x: i32) {
        let o = self.overview(self.body);
        let w = self.wave_rect(self.body).w as f64;
        let centre = (x - o.x).clamp(0, o.w) as f64 / o.w.max(1) as f64 * self.frames() as f64;
        self.view = (centre - w * self.zoom / 2.0).max(0.0) as usize;
        self.clamp_view();
    }

    pub fn on_drag(&mut self, x: i32, _y: i32, r: Rect) -> bool {
        self.body = r;
        match self.drag {
            Some((1, start)) => {
                let w = self.wave_rect(r);
                // Dragging past either edge scrolls.
                if x < w.x {
                    self.view = self.view.saturating_sub((self.zoom * 20.0) as usize);
                } else if x > w.x + w.w {
                    self.view += (self.zoom * 20.0) as usize;
                    self.clamp_view();
                }
                let f = self.frame_at(x.clamp(w.x, w.x + w.w));
                self.sel = Some((start.min(f), start.max(f)));
                true
            }
            Some((2, _)) => {
                self.overview_to(x);
                true
            }
            _ => false,
        }
    }

    pub fn on_release(&mut self) -> bool {
        let d = self.drag.take();
        // A click without a drag: just the cursor.
        if let Some((a, b)) = self.sel {
            if b.saturating_sub(a) < (self.zoom * 3.0) as usize {
                self.sel = None;
            }
        }
        d.is_some()
    }

    pub fn on_key(&mut self, sc: u8, storage: &mut Storage, usb: &mut Usb) -> EditAction {
        use crate::keys::Key;
        let Some(k) = self.keys.feed(sc) else { return EditAction::None };
        if self.choosing.is_some() {
            if k == Key::Esc && !self.pcm.is_empty() {
                self.choosing = None;
                return EditAction::Redraw;
            }
            return EditAction::None;
        }
        let now = TICKS.load(Relaxed);
        let mut io = LibIo { storage, usb };
        let b = match k {
            Key::Char(' ') => Btn::Play,
            Key::Ctrl('z') => Btn::Undo,
            Key::Ctrl('y') => Btn::Redo,
            Key::Ctrl('x') => Btn::Cut,
            Key::Ctrl('c') => Btn::Copy,
            Key::Ctrl('v') => Btn::Paste,
            Key::Ctrl('a') => Btn::All,
            Key::Ctrl('s') => Btn::SaveFlac,
            Key::Ctrl('o') => Btn::Open,
            Key::Delete | Key::Backspace => Btn::Delete,
            Key::Char('+') | Key::Char('=') => Btn::ZoomIn,
            Key::Char('-') => Btn::ZoomOut,
            Key::Home => {
                self.cursor = 0;
                self.view = 0;
                return EditAction::Redraw;
            }
            Key::End => {
                self.cursor = self.frames();
                self.view = self.frames();
                self.clamp_view();
                return EditAction::Redraw;
            }
            Key::Esc => {
                self.sel = None;
                return EditAction::Redraw;
            }
            _ => return EditAction::None,
        };
        self.button(b, now, &mut io)
    }
}

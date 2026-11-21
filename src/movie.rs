//! The video editor's engine: a movie project (a sequence of photos and videos with
//! transitions, titles on top, up to two music tracks), drawing any moment of it,
//! mixing its sound, and exporting it as an MP4 (H.264 video, FLAC sound).

use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use crate::gfx::Size;
use crate::image::Image;
use crate::photos::Source;
use crate::video::avi::{Avi, Sound};
use crate::video::{flac, h264, mp4};

pub const RATE: u32 = 48_000;

/// Reads media files (from Solon storage or the boot drive).
pub trait Io {
    fn read(&mut self, src: &Source, off: u64, len: usize) -> Option<Vec<u8>>;
    fn read_all(&mut self, src: &Source) -> Option<Vec<u8>>;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Kind {
    Photo,
    Video,
    Music,
}

pub struct Media {
    pub kind: Kind,
    pub name: String,
    /// Where it is, as saved in the project ("Photos/2026-11-19 Trip/IMG_1.JPG", or
    /// "boot:name" for the boot drive).
    pub path: String,
    pub src: Source,
    pub size: u64,
    /// Length (videos, music), in ms.
    pub len_ms: u64,
    pub avi: Option<Avi>,
    pub sound: Option<Sound>,
    pub thumb: Option<Image>,
    /// Couldn't be read.
    pub broken: Option<&'static str>,
}

impl Media {
    pub fn new(kind: Kind, name: String, path: String, src: Source, size: u64) -> Media {
        Media { kind, name, path, src, size, len_ms: 0, avi: None, sound: None, thumb: None, broken: None }
    }

    /// Read the index of a video / music file (once).
    pub fn open(&mut self, io: &mut dyn Io) -> bool {
        if self.broken.is_some() {
            return false;
        }
        let ready = match self.kind {
            Kind::Photo => true,
            Kind::Video => self.avi.is_some(),
            Kind::Music => self.sound.is_some(),
        };
        if ready {
            return true;
        }
        let src = self.src.clone();
        let size = self.size;
        let mut read = |off: u64, len: usize| io.read(&src, off, len);
        match self.kind {
            Kind::Video => match Avi::parse(&mut read, size) {
                Ok(a) => {
                    self.len_ms = a.duration_ms();
                    self.sound = a.sound.clone();
                    self.avi = Some(a);
                }
                Err(e) => self.broken = Some(e),
            },
            Kind::Music => match crate::video::avi::parse_wav(&mut read, size) {
                Ok(s) => {
                    self.len_ms = s.frames() * 1000 / s.rate.max(1) as u64;
                    self.sound = Some(s);
                }
                Err(e) => self.broken = Some(e),
            },
            Kind::Photo => {}
        }
        self.broken.is_none()
    }
}

/// Is this a file the editor can use, and as what?
pub fn media_kind(name: &str) -> Option<Kind> {
    let n = name.to_ascii_lowercase();
    if crate::import::is_photo(&n) {
        Some(Kind::Photo)
    } else if n.ends_with(".avi") {
        Some(Kind::Video)
    } else if n.ends_with(".wav") {
        Some(Kind::Music)
    } else {
        None
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Motion {
    Still,
    ZoomIn,
    ZoomOut,
    PanLeft,
    PanRight,
}

pub const MOTIONS: [(Motion, &str); 5] = [(Motion::Still, "Still"), (Motion::ZoomIn, "Zoom in"), (Motion::ZoomOut, "Zoom out"), (Motion::PanLeft, "Pan left"), (Motion::PanRight, "Pan right")];

/// How a clip in the main track begins.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Trans {
    Cut,
    Crossfade,
    Black,
}

pub const TRANSITIONS: [(Trans, &str); 3] = [(Trans::Cut, "Cut"), (Trans::Crossfade, "Crossfade"), (Trans::Black, "Dip to black")];
const TRANS_MS: u64 = 1000;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Track {
    Video,
    Titles,
    Music1,
    Music2,
}

pub const TRACKS: [(Track, &str); 4] = [(Track::Video, "Video"), (Track::Titles, "Titles"), (Track::Music1, "Music 1"), (Track::Music2, "Music 2")];

pub const STYLES: [&str; 3] = ["Middle", "Bottom", "On black"];

#[derive(Clone, Debug)]
pub struct Clip {
    pub track: Track,
    /// Index into the project's media (None for titles).
    pub media: Option<usize>,
    /// Start on the timeline in ms (worked out for the Video track).
    pub start: u64,
    pub len: u64,
    /// Where in the video / music the clip starts, ms.
    pub offset: u64,
    pub motion: Motion,
    pub trans: Trans,
    /// Percent.
    pub volume: u32,
    pub text: String,
    pub style: usize,
    /// Music: fade in at the start and out at the end.
    pub fades: bool,
}

impl Clip {
    pub fn end(&self) -> u64 {
        self.start + self.len
    }
    pub fn new(track: Track, media: Option<usize>, start: u64, len: u64) -> Clip {
        Clip { track, media, start, len, offset: 0, motion: Motion::Still, trans: Trans::Cut, volume: 100, text: String::new(), style: 0, fades: true }
    }
}

pub struct Project {
    pub media: Vec<Media>,
    /// Main track (photos and videos, one after another), then titles and music.
    pub clips: Vec<Clip>,
}

pub const PHOTO_MS: u64 = 5000;
pub const MIN_MS: u64 = 500;

impl Project {
    pub fn new() -> Project {
        Project { media: Vec::new(), clips: Vec::new() }
    }

    /// Lay the main track out: each clip after the one before, overlapping by the
    /// crossfade. Call after any change.
    pub fn layout(&mut self) {
        let mut t = 0u64;
        let mut prev_len = 0;
        for c in self.clips.iter_mut().filter(|c| c.track == Track::Video) {
            let x = if c.trans == Trans::Crossfade && prev_len > 0 { TRANS_MS.min(c.len / 2).min(prev_len / 2) } else { 0 };
            c.start = t.saturating_sub(x);
            t = c.start + c.len;
            prev_len = c.len;
        }
        // Keep the main track first and in order, others after.
        self.clips.sort_by_key(|c| (c.track != Track::Video, if c.track == Track::Video { 0 } else { c.start }));
    }

    pub fn duration(&self) -> u64 {
        self.clips.iter().map(|c| c.end()).max().unwrap_or(0)
    }

    pub fn video_indices(&self) -> Vec<usize> {
        (0..self.clips.len()).filter(|&i| self.clips[i].track == Track::Video).collect()
    }

    /// Add (or find) a media item, returning its index.
    pub fn add_media(&mut self, m: Media) -> usize {
        if let Some(i) = self.media.iter().position(|x| x.path == m.path) {
            return i;
        }
        self.media.push(m);
        self.media.len() - 1
    }

    /// Where a photo/video goes when added: at the end of the main track.
    pub fn append(&mut self, media: usize, io: &mut dyn Io) -> Option<usize> {
        let m = &mut self.media[media];
        let len = match m.kind {
            Kind::Photo => PHOTO_MS,
            Kind::Video => {
                if !m.open(io) {
                    return None;
                }
                m.len_ms.max(MIN_MS)
            }
            Kind::Music => return None,
        };
        let mut c = Clip::new(Track::Video, Some(media), 0, len);
        if self.clips.iter().any(|c| c.track == Track::Video) {
            c.trans = Trans::Crossfade;
        }
        if m.kind == Kind::Photo {
            // Gentle movement by default, alternating so a slideshow isn't monotonous.
            let n = self.clips.iter().filter(|c| c.track == Track::Video).count();
            c.motion = [Motion::ZoomIn, Motion::PanRight, Motion::ZoomOut, Motion::PanLeft][n % 4];
        }
        self.clips.push(c);
        self.layout();
        self.video_indices().last().copied()
    }

    /// Music goes at `at` on the first music track with room.
    pub fn add_music(&mut self, media: usize, at: u64, io: &mut dyn Io) -> Option<usize> {
        let m = &mut self.media[media];
        if !m.open(io) {
            return None;
        }
        let len = m.len_ms.max(MIN_MS);
        for track in [Track::Music1, Track::Music2] {
            let busy = self.clips.iter().any(|c| c.track == track && c.start < at + len && at < c.end());
            if !busy {
                let c = Clip::new(track, Some(media), at, len);
                self.clips.push(c);
                self.layout();
                return self.clips.iter().position(|c| c.track == track && c.start == at && c.media == Some(media));
            }
        }
        // Both busy: after everything on Music 1.
        let end = self.clips.iter().filter(|c| c.track == Track::Music1).map(|c| c.end()).max().unwrap_or(0);
        self.clips.push(Clip::new(Track::Music1, Some(media), end, len));
        self.layout();
        self.clips.iter().position(|c| c.track == Track::Music1 && c.start == end && c.media == Some(media))
    }

    pub fn add_title(&mut self, at: u64, text: &str) -> usize {
        let mut c = Clip::new(Track::Titles, None, at, 4000);
        c.text = String::from(text);
        self.clips.push(c);
        self.layout();
        self.clips.iter().position(|c| c.track == Track::Titles && c.start == at && c.text == text).unwrap_or(0)
    }

    /// Split clip `i` at timeline time `t`. Returns the index of the second half.
    pub fn split(&mut self, i: usize, t: u64) -> Option<usize> {
        let c = self.clips[i].clone();
        if t <= c.start + MIN_MS / 2 || t + MIN_MS / 2 >= c.end() {
            return None;
        }
        let first = t - c.start;
        let mut second = c.clone();
        second.len = c.len - first;
        second.offset = c.offset + first;
        second.trans = Trans::Cut;
        second.start = t;
        self.clips[i].len = first;
        if c.track == Track::Video {
            // Crossfades shift starts; a split mustn't move anything.
            self.clips.insert(i + 1, second);
        } else {
            self.clips.push(second);
        }
        self.layout();
        self.clips.iter().position(|x| x.track == c.track && x.start == t && x.offset == c.offset + first)
    }

    pub fn max_len(&self, c: &Clip) -> u64 {
        match c.media.map(|m| &self.media[m]) {
            Some(m) if m.kind != Kind::Photo => m.len_ms.saturating_sub(c.offset).max(MIN_MS),
            _ => 10 * 60 * 1000,
        }
    }

    // ---- saving --------------------------------------------------------------------------

    pub fn to_text(&self) -> String {
        let mut s = String::from("solon-movie 1\n");
        for m in &self.media {
            let k = match m.kind {
                Kind::Photo => "photo",
                Kind::Video => "video",
                Kind::Music => "music",
            };
            s += &format!("media {} {}\n", k, m.path);
        }
        for c in &self.clips {
            let track = TRACKS.iter().position(|t| t.0 == c.track).unwrap_or(0);
            let motion = MOTIONS.iter().position(|m| m.0 == c.motion).unwrap_or(0);
            let trans = TRANSITIONS.iter().position(|m| m.0 == c.trans).unwrap_or(0);
            let media = c.media.map(|m| m as i64).unwrap_or(-1);
            s += &format!("clip {} {} {} {} {} {} {} {} {} {} {}\n", track, media, c.start, c.len, c.offset, motion, trans, c.volume, c.style, c.fades as u8, c.text);
        }
        s
    }

    /// Read a saved project. `find` turns a saved path into a source and its size.
    pub fn from_text(text: &str, find: &mut dyn FnMut(&str) -> Option<(Source, u64)>) -> Project {
        let mut p = Project::new();
        // Saved media index -> our index (missing files are dropped with their clips).
        let mut map: Vec<Option<usize>> = Vec::new();
        for line in text.lines() {
            let mut parts = line.splitn(3, ' ');
            match parts.next() {
                Some("media") => {
                    let kind = match parts.next() {
                        Some("photo") => Kind::Photo,
                        Some("video") => Kind::Video,
                        Some("music") => Kind::Music,
                        _ => {
                            map.push(None);
                            continue;
                        }
                    };
                    let path = parts.next().unwrap_or("");
                    match find(path) {
                        Some((src, size)) => {
                            let base = path.strip_prefix("boot:").unwrap_or(path);
                            let name = String::from(base.rsplit('/').next().unwrap_or(base));
                            p.media.push(Media::new(kind, name, String::from(path), src, size));
                            map.push(Some(p.media.len() - 1));
                        }
                        None => map.push(None),
                    }
                }
                Some("clip") => {
                    // clip track media start len offset motion trans volume style fades text...
                    let rest = line.splitn(12, ' ').skip(1).collect::<Vec<_>>();
                    if rest.len() < 10 {
                        continue;
                    }
                    let n = |i: usize| rest[i].parse::<i64>().unwrap_or(0);
                    let track = TRACKS.get(n(0) as usize).map(|t| t.0).unwrap_or(Track::Video);
                    let media = if n(1) < 0 { None } else { map.get(n(1) as usize).copied().flatten() };
                    if track != Track::Titles && media.is_none() {
                        continue; // its file is gone
                    }
                    let mut c = Clip::new(track, media, n(2) as u64, (n(3) as u64).max(MIN_MS));
                    c.offset = n(4) as u64;
                    c.motion = MOTIONS.get(n(5) as usize).map(|m| m.0).unwrap_or(Motion::Still);
                    c.trans = TRANSITIONS.get(n(6) as usize).map(|m| m.0).unwrap_or(Trans::Cut);
                    c.volume = n(7).clamp(0, 400) as u32;
                    c.style = (n(8) as usize).min(STYLES.len() - 1);
                    c.fades = n(9) != 0;
                    c.text = String::from(rest.get(10).copied().unwrap_or(""));
                    p.clips.push(c);
                }
                _ => {}
            }
        }
        p.layout();
        p
    }
}

impl Clone for Media {
    fn clone(&self) -> Media {
        Media {
            kind: self.kind,
            name: self.name.clone(),
            path: self.path.clone(),
            src: self.src.clone(),
            size: self.size,
            len_ms: self.len_ms,
            avi: self.avi.clone(),
            sound: self.sound.clone(),
            thumb: None,
            broken: self.broken,
        }
    }
}

// ---- drawing a moment of the movie -----------------------------------------------------------

/// A picture of size w x h (0x00RRGGBB).
pub struct Frame {
    pub w: usize,
    pub h: usize,
    pub px: Vec<u32>,
}

/// Draws frames at one size, keeping decoded pictures around.
pub struct Renderer {
    pub w: usize,
    pub h: usize,
    /// Photos, shrunk for this size: (media, picture), most recent last.
    photos: Vec<(usize, Image)>,
    /// The last frame decoded from each video: (media, frame number, picture).
    frames: Vec<(usize, usize, Image)>,
    /// Title text pictures: (text, style, alpha mask w, h, mask).
    titles: Vec<(String, usize, usize, usize, Vec<u8>)>,
}

/// Bilinear sample of an image at (fx, fy) in 1/256 pixel units.
#[inline]
fn sample(img: &Image, fx: i32, fy: i32) -> u32 {
    let x = (fx >> 8).clamp(0, img.w as i32 - 1) as usize;
    let y = (fy >> 8).clamp(0, img.h as i32 - 1) as usize;
    let x1 = (x + 1).min(img.w - 1);
    let y1 = (y + 1).min(img.h - 1);
    let ax = (fx & 0xFF) as u32;
    let ay = (fy & 0xFF) as u32;
    let p = |xx: usize, yy: usize| img.px[yy * img.w + xx];
    let (a, b, c, d) = (p(x, y), p(x1, y), p(x, y1), p(x1, y1));
    let ch = |s: u32| {
        let top = ((a >> s) & 0xFF) * (256 - ax) + ((b >> s) & 0xFF) * ax;
        let bot = ((c >> s) & 0xFF) * (256 - ax) + ((d >> s) & 0xFF) * ax;
        ((top * (256 - ay) + bot * ay) >> 16) & 0xFF
    };
    ch(16) << 16 | ch(8) << 8 | ch(0)
}

#[inline]
fn blend(dst: u32, src: u32, a: u32) -> u32 {
    if a >= 255 {
        return src;
    }
    let ch = |s: u32| ((((dst >> s) & 0xFF) * (255 - a) + ((src >> s) & 0xFF) * a) / 255) << s;
    ch(16) | ch(8) | ch(0)
}

#[inline]
fn darken(p: u32, k: u32) -> u32 {
    let ch = |s: u32| ((((p >> s) & 0xFF) * k) / 255) << s;
    ch(16) | ch(8) | ch(0)
}

impl Renderer {
    pub fn new(w: usize, h: usize) -> Renderer {
        Renderer { w, h, photos: Vec::new(), frames: Vec::new(), titles: Vec::new() }
    }

    /// Decode and keep a photo, sized for this output (a little bigger, for zooming).
    pub fn photo(&mut self, p: &Project, m: usize, io: &mut dyn Io) -> Option<usize> {
        if let Some(i) = self.photos.iter().position(|e| e.0 == m) {
            return Some(i);
        }
        let media = &p.media[m];
        let data = io.read_all(&media.src)?;
        let img = crate::image::decode(&data).ok()?;
        let small = img.shrink_to_fit(self.w * 5 / 4, self.h * 5 / 4);
        if self.photos.len() >= 6 {
            self.photos.remove(0);
        }
        self.photos.push((m, small));
        Some(self.photos.len() - 1)
    }

    fn video_frame(&mut self, p: &Project, m: usize, n: usize, io: &mut dyn Io) -> Option<usize> {
        if let Some(i) = self.frames.iter().position(|e| e.0 == m && e.1 == n) {
            return Some(i);
        }
        let media = &p.media[m];
        let avi = media.avi.as_ref()?;
        let src = media.src.clone();
        let mut read = |off: u64, len: usize| io.read(&src, off, len);
        let jpeg = avi.frame_jpeg(&mut read, n)?;
        let img = crate::image::decode(&jpeg).ok()?;
        let img = img.shrink_to_fit(self.w, self.h);
        self.frames.retain(|e| e.0 != m);
        if self.frames.len() >= 4 {
            self.frames.remove(0);
        }
        self.frames.push((m, n, img));
        Some(self.frames.len() - 1)
    }

    /// Draw the movie at `t` ms.
    pub fn render(&mut self, p: &mut Project, t: u64, io: &mut dyn Io, out: &mut Frame) {
        out.px.fill(0);
        let vids = p.video_indices();
        let active: Vec<usize> = vids.iter().copied().filter(|&i| p.clips[i].start <= t && t < p.clips[i].end()).collect();
        for (k, &i) in active.iter().enumerate() {
            let c = p.clips[i].clone();
            // Crossfade: the later clip fades in over the overlap.
            let mut alpha = 255u32;
            if k > 0 && c.trans == Trans::Crossfade {
                let prev = &p.clips[active[k - 1]];
                let overlap = prev.end().saturating_sub(c.start).max(1);
                alpha = ((t - c.start) * 255 / overlap).min(255) as u32;
            }
            // Dip to black: fade in at the start of this clip, out at the end of the one before.
            let mut bright = 255u32;
            if c.trans == Trans::Black && t - c.start < TRANS_MS / 2 {
                bright = ((t - c.start) * 255 / (TRANS_MS / 2)) as u32;
            }
            let pos = vids.iter().position(|&v| v == i).unwrap();
            if let Some(&next) = vids.get(pos + 1) {
                if p.clips[next].trans == Trans::Black && c.end() - t < TRANS_MS / 2 {
                    bright = bright.min(((c.end() - t) * 255 / (TRANS_MS / 2)) as u32);
                }
            }
            self.draw_clip(p, &c, t - c.start, io, out, alpha, bright);
        }
        // Titles on top.
        let titles: Vec<Clip> = p.clips.iter().filter(|c| c.track == Track::Titles && c.start <= t && t < c.end()).cloned().collect();
        for c in titles {
            self.draw_title(&c, t - c.start, out);
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn draw_clip(&mut self, p: &mut Project, c: &Clip, local: u64, io: &mut dyn Io, out: &mut Frame, alpha: u32, bright: u32) {
        let Some(m) = c.media else { return };
        let (w, h) = (out.w as i64, out.h as i64);
        match p.media[m].kind {
            Kind::Photo => {
                let Some(i) = self.photo(p, m, io) else { return };
                let img = &self.photos[i].1;
                // Progress through the clip, eased, 0..=1000.
                let t = (local * 1000 / c.len.max(1)).min(1000) as i64;
                let e = t * t * (3000 - 2 * t) / 1_000_000; // smoothstep, 0..1000
                let (zoom, pan) = match c.motion {
                    Motion::Still => (1000, 0),
                    Motion::ZoomIn => (1000 + 180 * e / 1000, 0),
                    Motion::ZoomOut => (1180 - 180 * e / 1000, 0),
                    Motion::PanLeft => (1150, 70 - 140 * e / 1000),
                    Motion::PanRight => (1150, -70 + 140 * e / 1000),
                };
                // Scale (x1000) to fit the picture in the frame, times the zoom.
                let fit = (w * 1000 / img.w as i64).min(h * 1000 / img.h as i64);
                let s = fit * zoom / 1000; // output pixels per 1000 image pixels
                let dw = img.w as i64 * s / 1000;
                let dh = img.h as i64 * s / 1000;
                let ox = (w - dw) / 2 + pan * w / 1000;
                let oy = (h - dh) / 2;
                self.blit(img, ox, oy, dw, dh, out, alpha, bright);
            }
            Kind::Video => {
                if !p.media[m].open(io) {
                    return;
                }
                let Some(a) = p.media[m].avi.as_ref() else { return };
                let n = (((c.offset + local) * a.rate as u64) / (1000 * a.scale.max(1) as u64)) as usize;
                let n = n.min(a.frames.len().saturating_sub(1));
                // Fit using the video's own size (the decoded picture may be smaller).
                let (vw, vh) = (a.width.max(1) as i64, a.height.max(1) as i64);
                let Some(i) = self.video_frame(p, m, n, io) else { return };
                let img = &self.frames[i].2;
                let fit = (w * 1000 / vw).min(h * 1000 / vh);
                let dw = vw * fit / 1000;
                let dh = vh * fit / 1000;
                self.blit(img, (w - dw) / 2, (h - dh) / 2, dw, dh, out, alpha, bright);
            }
            Kind::Music => {}
        }
    }

    /// Draw `img` scaled to (dw, dh) at (ox, oy).
    #[allow(clippy::too_many_arguments)]
    fn blit(&self, img: &Image, ox: i64, oy: i64, dw: i64, dh: i64, out: &mut Frame, alpha: u32, bright: u32) {
        if dw <= 0 || dh <= 0 {
            return;
        }
        let x0 = ox.max(0) as usize;
        let x1 = (ox + dw).min(out.w as i64).max(0) as usize;
        let y0 = oy.max(0) as usize;
        let y1 = (oy + dh).min(out.h as i64).max(0) as usize;
        // Image position per output pixel, 1/256 pixel units (pixel centres).
        let sx = (img.w as i64 * 256 * 256 / dw) as i32;
        let sy = (img.h as i64 * 256 * 256 / dh) as i32;
        for y in y0..y1 {
            let fy = (((y as i64 - oy) * 2 + 1) * sy as i64 / 512) as i32 - 128;
            let row = &mut out.px[y * out.w..(y + 1) * out.w];
            for x in x0..x1 {
                let fx = (((x as i64 - ox) * 2 + 1) * sx as i64 / 512) as i32 - 128;
                let mut p = sample(img, fx.max(0), fy.max(0));
                if bright < 255 {
                    p = darken(p, bright);
                }
                row[x] = blend(row[x], p, alpha);
            }
        }
    }

    /// The text of a title as an alpha mask (white-on-nothing), lines wrapped to fit.
    fn title_mask(&mut self, text: &str, style: usize) -> usize {
        if let Some(i) = self.titles.iter().position(|t| t.0 == text && t.1 == style) {
            return i;
        }
        // Draw with the 32 px font, then scale up to the size wanted.
        let glyph_w = crate::gfx::text_width("M", Size::Large, true).max(1) as usize;
        let target_h = if style == 1 { self.h / 16 } else { self.h / 11 };
        let scale256 = (target_h * 256 / 32).max(64);
        let max_chars = ((self.w * 9 / 10) * 256 / scale256 / glyph_w).max(4);
        let mut lines: Vec<String> = Vec::new();
        for word in text.split(' ') {
            match lines.last_mut() {
                Some(l) if l.chars().count() + 1 + word.chars().count() <= max_chars => {
                    l.push(' ');
                    l.push_str(word);
                }
                _ => lines.push(String::from(word)),
            }
        }
        if lines.is_empty() {
            lines.push(String::new());
        }
        let cols = lines.iter().map(|l| l.chars().count()).max().unwrap_or(1).max(1);
        let (sw, sh) = (cols * glyph_w, lines.len() * 36);
        let mut small = crate::gfx::Canvas::new(sw, sh);
        for (k, l) in lines.iter().enumerate() {
            let lw = crate::gfx::text_width(l, Size::Large, true) as usize;
            small.text(((sw - lw) / 2) as i32, (k * 36 + 2) as i32, l, Size::Large, true, 0xFFFFFF);
        }
        // Scale the mask up (bilinear on the green channel).
        let (mw, mh) = ((sw * scale256 / 256).max(1), (sh * scale256 / 256).max(1));
        let mut mask = vec![0u8; mw * mh];
        let src = Image { w: sw, h: sh, px: small.px, opaque: true };
        for y in 0..mh {
            for x in 0..mw {
                let fx = ((x * 2 + 1) * sw * 256 / (mw * 2)) as i32 - 128;
                let fy = ((y * 2 + 1) * sh * 256 / (mh * 2)) as i32 - 128;
                mask[y * mw + x] = ((sample(&src, fx.max(0), fy.max(0)) >> 8) & 0xFF) as u8;
            }
        }
        if self.titles.len() >= 8 {
            self.titles.remove(0);
        }
        self.titles.push((String::from(text), style, mw, mh, mask));
        self.titles.len() - 1
    }

    fn draw_title(&mut self, c: &Clip, local: u64, out: &mut Frame) {
        // Fade in and out over 0.4 s.
        let f = 400u64;
        let fade = (local.min(c.len.saturating_sub(local)) * 255 / f).min(255) as u32;
        let (w, h) = (out.w, out.h);
        if c.style == 2 {
            // On black: covers the picture.
            for p in out.px.iter_mut() {
                *p = darken(*p, 255 - fade);
            }
        }
        if c.text.trim().is_empty() {
            return;
        }
        let i = self.title_mask(&c.text, c.style);
        let (mw, mh) = (self.titles[i].2, self.titles[i].3);
        let x0 = (w as i64 - mw as i64) / 2;
        let y0 = if c.style == 1 { (h * 80 / 100) as i64 - mh as i64 / 2 } else { (h as i64 - mh as i64) / 2 };
        if c.style == 1 {
            // A dark band behind lower titles.
            let pad = (h / 40) as i64;
            let (by0, by1) = ((y0 - pad).max(0) as usize, ((y0 + mh as i64 + pad) as usize).min(h));
            for y in by0..by1 {
                for x in 0..w {
                    let p = &mut out.px[y * w + x];
                    *p = darken(*p, 255 - fade * 110 / 255);
                }
            }
        }
        let mask = &self.titles[i].4;
        let shadow = (h / 240).max(1) as i64;
        for pass in 0..2 {
            let (dx, dy, color, k) = if pass == 0 { (shadow, shadow, 0u32, 150u32) } else { (0, 0, 0xFFFFFF, 255) };
            for my in 0..mh {
                let y = y0 + my as i64 + dy;
                if y < 0 || y >= h as i64 {
                    continue;
                }
                for mx in 0..mw {
                    let a = mask[my * mw + mx] as u32;
                    if a == 0 {
                        continue;
                    }
                    let x = x0 + mx as i64 + dx;
                    if x < 0 || x >= w as i64 {
                        continue;
                    }
                    let p = &mut out.px[y as usize * w + x as usize];
                    *p = blend(*p, color, a * k / 255 * fade / 255);
                }
            }
        }
    }
}

// ---- sound -------------------------------------------------------------------------------------

/// Mix the movie's sound for `n` frames at 48 kHz starting at frame `from`.
pub fn mix(p: &mut Project, from: u64, n: usize, io: &mut dyn Io) -> Vec<i16> {
    let mut acc = vec![0i32; n * 2];
    let t0 = from * 1000 / RATE as u64;
    let t1 = (from + n as u64) * 1000 / RATE as u64 + 1;
    let vids = p.video_indices();
    for ci in 0..p.clips.len() {
        let c = p.clips[ci].clone();
        let Some(m) = c.media else { continue };
        if c.end() <= t0 || c.start >= t1 || c.volume == 0 {
            continue;
        }
        if !p.media[m].open(io) {
            continue;
        }
        let Some(snd) = p.media[m].sound.clone() else { continue };
        let src = p.media[m].src.clone();
        // Output frames covered by this clip.
        let cs = c.start * RATE as u64 / 1000;
        let ce = c.end() * RATE as u64 / 1000;
        let a = from.max(cs);
        let b = (from + n as u64).min(ce);
        if a >= b {
            continue;
        }
        // Source frames needed (at the source's own rate).
        let srate = snd.rate.max(1) as u64;
        let pos = |k: u64| -> u64 { (c.offset * RATE as u64 / 1000 + (k - cs)) * srate * 256 / RATE as u64 };
        let s0 = pos(a) >> 8;
        let s1 = (pos(b) >> 8) + 2;
        let mut read = |off: u64, len: usize| io.read(&src, off, len);
        let pcm = snd.read_frames(&mut read, s0, (s1 - s0) as usize);
        // Gain envelope (x1024): volume, crossfades, music fades.
        let vol = c.volume as i64 * 1024 / 100;
        let pos_in_vids = vids.iter().position(|&v| v == ci);
        let fade_in_ms = match (c.track, pos_in_vids) {
            (Track::Video, Some(k)) if k > 0 && c.trans == Trans::Crossfade => p.clips[vids[k - 1]].end().saturating_sub(c.start),
            (Track::Video, _) if c.trans == Trans::Black => TRANS_MS / 2,
            (Track::Music1 | Track::Music2, _) if c.fades => 1000.min(c.len / 3),
            _ => 0,
        };
        let fade_out_ms = match (c.track, pos_in_vids) {
            (Track::Video, Some(k)) => match vids.get(k + 1).map(|&nx| &p.clips[nx]) {
                Some(nx) if nx.trans == Trans::Crossfade => c.end().saturating_sub(nx.start),
                Some(nx) if nx.trans == Trans::Black => TRANS_MS / 2,
                _ => 0,
            },
            (Track::Music1 | Track::Music2, _) if c.fades => 2000.min(c.len / 3),
            _ => 0,
        };
        let fin = fade_in_ms * RATE as u64 / 1000;
        let fout = fade_out_ms * RATE as u64 / 1000;
        for k in a..b {
            let mut g = vol;
            if fin > 0 && k - cs < fin {
                g = g * (k - cs) as i64 / fin as i64;
            }
            if fout > 0 && ce - k < fout {
                g = g * (ce - k) as i64 / fout as i64;
            }
            let sp = pos(k);
            let si = ((sp >> 8) - s0) as usize;
            let fr = (sp & 0xFF) as i64;
            let o = (k - from) as usize * 2;
            for ch in 0..2 {
                let x0 = *pcm.get(si * 2 + ch).unwrap_or(&0) as i64;
                let x1 = *pcm.get(si * 2 + 2 + ch).unwrap_or(&(x0 as i16)) as i64;
                let v = (x0 * (256 - fr) + x1 * fr) >> 8;
                acc[o + ch] += (v * g / 1024) as i32;
            }
        }
    }
    acc.iter().map(|&v| v.clamp(-32768, 32767) as i16).collect()
}

// ---- export -------------------------------------------------------------------------------------

pub const FPS: u64 = 30;

pub struct Export {
    pub w: usize,
    pub h: usize,
    pub frame: u64,
    pub frames: u64,
    renderer: Renderer,
    out: Frame,
    enc: h264::Encoder,
    fl: flac::Encoder,
    mux: mp4::Muxer,
    audio_frames: u64,
    has_sound: bool,
}

impl Export {
    pub fn new(p: &Project, w: usize, h: usize) -> Export {
        let enc = h264::Encoder::new(w, h, 24);
        let mux = mp4::Muxer::new(w as u32, h as u32, FPS as u32 * 1000, 1000, enc.sps(), enc.pps(), None);
        let has_sound = p.clips.iter().any(|c| c.media.is_some_and(|m| p.media[m].kind != Kind::Photo) && c.volume > 0);
        let frames = (p.duration() * FPS).div_ceil(1000).max(1);
        Export { w, h, frame: 0, frames, renderer: Renderer::new(w, h), out: Frame { w, h, px: vec![0; w * h] }, enc, fl: flac::Encoder::new(RATE), mux, audio_frames: 0, has_sound }
    }

    pub fn percent(&self) -> u32 {
        (self.frame * 100 / self.frames.max(1)) as u32
    }

    pub fn done(&self) -> bool {
        self.frame >= self.frames
    }

    /// Make the next frame (and the sound that goes with it). Returns false when finished.
    pub fn step(&mut self, p: &mut Project, io: &mut dyn Io) -> bool {
        if self.done() {
            return false;
        }
        let t = self.frame * 1000 / FPS;
        self.renderer.render(p, t, io, &mut self.out);
        // A scene change (a clip starting with a cut) begins a new key frame.
        let cut = p.clips.iter().any(|c| c.track == Track::Video && c.trans != Trans::Crossfade && c.start == t && t > 0);
        let (nal, key) = self.enc.encode(&self.out.px, cut);
        self.mux.add_video(&[&nal], key);
        self.frame += 1;
        // Sound up to the end of this frame, in whole FLAC blocks (the last may be short).
        if self.has_sound {
            let until = self.frame * RATE as u64 / FPS;
            let last = self.done();
            while self.audio_frames + flac::BLOCK as u64 <= until || (last && self.audio_frames < until) {
                let n = (flac::BLOCK as u64).min(until - self.audio_frames) as usize;
                let pcm = mix(p, self.audio_frames, n, io);
                let data = self.fl.frame(&pcm);
                self.mux.add_audio(&data, n as u32);
                self.audio_frames += n as u64;
            }
        }
        true
    }

    /// The finished file: (header, payload). Call when `done()`.
    pub fn finish(mut self) -> (Vec<u8>, Vec<u8>) {
        if self.has_sound {
            self.mux.audio = Some(mp4::AudioFormat { rate: RATE, channels: 2, bits: 16, streaminfo: self.fl.streaminfo() });
        }
        let head = self.mux.header();
        (head, self.mux.take_payload())
    }
}

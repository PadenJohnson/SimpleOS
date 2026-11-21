//! One video being played: the picture at any moment (decoding forward from the right key
//! frame when jumping around) and the sound, as 48 kHz stereo.

use alloc::boxed::Box;
use alloc::collections::{BinaryHeap, VecDeque};
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Reverse;

use super::dec::{self, AudioDec, Pic, VideoDec};
use super::{Demux, Read};

/// Reads are cached in blocks: containers read lots of small headers.
const BLOCK: u64 = 64 * 1024;
const BLOCKS: usize = 48;

struct Cache {
    blocks: VecDeque<(u64, Vec<u8>)>,
}

impl Cache {
    fn read(&mut self, inner: &mut Read, off: u64, len: usize) -> Option<Vec<u8>> {
        if len as u64 >= BLOCK * 4 {
            return inner(off, len);
        }
        let mut out = Vec::with_capacity(len);
        let end = off + len as u64;
        let mut pos = off;
        while pos < end {
            let bi = pos / BLOCK;
            let k = match self.blocks.iter().position(|b| b.0 == bi) {
                Some(k) => k,
                None => {
                    let data = inner(bi * BLOCK, BLOCK as usize)?;
                    if self.blocks.len() >= BLOCKS {
                        self.blocks.pop_front();
                    }
                    self.blocks.push_back((bi, data));
                    self.blocks.len() - 1
                }
            };
            let b = &self.blocks[k].1;
            let s = (pos - bi * BLOCK) as usize;
            if s >= b.len() {
                break; // end of the file
            }
            let e = ((end - bi * BLOCK) as usize).min(b.len());
            out.extend_from_slice(&b[s..e]);
            pos = bi * BLOCK + e as u64;
            if e < BLOCK as usize && e == b.len() {
                break;
            }
        }
        Some(out)
    }
}

/// The container and its read cache (kept together so the borrow checker lets them work at once).
struct Src {
    demux: Box<dyn Demux>,
    cache: Cache,
}

impl Src {
    fn with<R>(&mut self, inner: &mut Read, f: impl FnOnce(&mut dyn Demux, &mut Read) -> R) -> R {
        let Src { demux, cache } = self;
        let mut r = |o: u64, l: usize| cache.read(inner, o, l);
        f(demux.as_mut(), &mut r)
    }
}

pub struct Engine {
    src: Src,
    vdec: Option<Box<dyn VideoDec>>,
    adec: Option<Box<dyn AudioDec>>,
    /// Shown size (after turning) and how much to turn.
    pub width: usize,
    pub height: usize,
    rotation: u32,
    pub duration_ms: u64,
    /// Why the pictures, or the sound, can't be played.
    pub problem: Option<String>,
    pub sound_problem: Option<String>,
    // Pictures.
    started: bool,
    pending: BinaryHeap<Reverse<i64>>,
    frames: VecDeque<(i64, Pic)>,
    current: Option<(i64, Pic)>,
    /// The last key frame fed to the decoder, and the last packet's time.
    fed_key: i64,
    last_pts: i64,
    video_end: bool,
    flushed: bool,
    // Sound.
    abuf: Vec<i16>,
    /// Position of abuf[0], in frames at `arate` from the start (None = nothing yet).
    abuf_start: Option<i64>,
    arate: u32,
    a_next: u64,
    a_end: bool,
    a_started: bool,
}

impl Engine {
    pub fn open(read: &mut Read, size: u64) -> Result<Engine, String> {
        let mut cache = Cache { blocks: VecDeque::new() };
        let demux = {
            let mut r = |o: u64, l: usize| cache.read(read, o, l);
            super::open_demux(&mut r, size)?
        };
        let info = demux.info();
        let duration_ms = info.duration_ms;
        let (mut vdec, mut adec, mut problem, mut sound_problem) = (None, None, None, None);
        let (mut width, mut height, mut rotation) = (0, 0, 0);
        match &info.video {
            Some(v) => {
                rotation = v.rotation;
                (width, height) = if rotation == 90 || rotation == 270 { (v.height as usize, v.width as usize) } else { (v.width as usize, v.height as usize) };
                match dec::video(v) {
                    Ok(d) => vdec = Some(d),
                    Err(e) => problem = Some(e),
                }
                kprintln!("media: video {} {}x{}", v.codec.name(), v.width, v.height);
            }
            None => problem = Some(String::from("This file has no pictures")),
        }
        if let Some(a) = &info.audio {
            match dec::audio(a) {
                Ok(d) => adec = Some(d),
                Err(e) => sound_problem = Some(e),
            }
            kprintln!("media: sound {} {} Hz, {} channel(s)", a.codec.name(), a.rate, a.channels);
        }
        let need_size = vdec.is_some() && (width == 0 || height == 0);
        let mut e = Engine {
            src: Src { demux, cache },
            vdec,
            adec,
            width: width.max(1),
            height: height.max(1),
            rotation,
            duration_ms,
            problem,
            sound_problem,
            started: false,
            pending: BinaryHeap::new(),
            frames: VecDeque::new(),
            current: None,
            fed_key: 0,
            last_pts: 0,
            video_end: false,
            flushed: false,
            abuf: Vec::new(),
            abuf_start: None,
            arate: 0,
            a_next: 0,
            a_end: false,
            a_started: false,
        };
        if need_size {
            e.learn_size(read);
        }
        Ok(e)
    }

    /// Streams that don't say their picture size: decode the first picture to find out.
    fn learn_size(&mut self, read: &mut Read) {
        self.seek(read, 0);
        for _ in 0..300 {
            if !self.feed(read) || !self.frames.is_empty() {
                break;
            }
        }
        let size = self.frames.front().map(|(_, p)| match p {
            Pic::Yuv(f) => (f.width, f.height),
            Pic::Rgb(i) => (i.w, i.h),
        });
        if let Some((w, h)) = size {
            (self.width, self.height) = if self.rotation == 90 || self.rotation == 270 { (h, w) } else { (w, h) };
        }
        self.started = false; // the first picture() starts from the beginning again
    }

    pub fn has_sound(&self) -> bool {
        self.adec.is_some()
    }

    fn seek(&mut self, read: &mut Read, target: i64) {
        let k = self.src.with(read, |d, r| d.seek_video(r, target));
        if let Some(v) = self.vdec.as_mut() {
            v.reset();
        }
        self.pending.clear();
        self.frames.clear();
        self.current = None;
        self.fed_key = k;
        self.last_pts = k;
        self.video_end = false;
        self.flushed = false;
        self.started = true;
    }

    /// Feed one more packet (or flush at the end). False when nothing is left.
    fn feed(&mut self, read: &mut Read) -> bool {
        let Some(v) = self.vdec.as_mut() else { return false };
        if self.video_end {
            if self.flushed {
                return false;
            }
            v.flush();
            self.flushed = true;
        } else {
            match self.src.with(read, |d, r| d.next_video(r)) {
                Some(p) => {
                    let pts = p.pts.unwrap_or(self.last_pts + 33_333);
                    self.last_pts = pts;
                    if p.key {
                        self.fed_key = self.fed_key.max(pts);
                    }
                    if !p.data.is_empty() {
                        self.pending.push(Reverse(pts));
                        v.decode(&p.data);
                    }
                }
                None => self.video_end = true,
            }
        }
        while let Some(pic) = v.pop() {
            let pts = self.pending.pop().map(|r| r.0).unwrap_or(self.last_pts);
            self.frames.push_back((pts, pic));
        }
        // A decoder that skipped a damaged frame would leave its time behind.
        while self.pending.len() > 32 {
            self.pending.pop();
        }
        true
    }

    /// The picture showing at `ms`, as 0x00RRGGBB at w x h, and an id for it. If the id
    /// equals `unchanged`, no pixels are made (the caller keeps what it has).
    /// `budget`: the most packets to decode now (small while playing).
    pub fn picture(&mut self, read: &mut Read, ms: u64, w: usize, h: usize, budget: usize, unchanged: Option<i64>) -> Option<(i64, Option<Vec<u32>>)> {
        self.vdec.as_ref()?;
        let target = ms as i64 * 1000;
        let behind = self.current.as_ref().is_some_and(|c| c.0 > target + 1000);
        let key = self.src.demux.key_before(target);
        let ahead = match key {
            Some(k) => k > self.fed_key,
            None => target > self.last_pts + 4_000_000, // files without an index: far ahead
        };
        // At the end with nothing to show (a jump past the last picture): start again.
        let stuck = self.video_end && self.frames.is_empty() && (self.current.is_none() || self.current.as_ref().is_some_and(|c| c.0 + 1_000_000 < target || c.0 > target));
        if !self.started || behind || ahead || stuck {
            self.seek(read, target);
        }
        let mut fed = 0;
        loop {
            while self.frames.front().is_some_and(|f| f.0 <= target) {
                self.current = self.frames.pop_front();
            }
            if !self.frames.is_empty() {
                break;
            }
            if fed >= budget || !self.feed(read) {
                break;
            }
            fed += 1;
        }
        if self.current.is_none() {
            self.current = self.frames.pop_front();
        }
        let (pts, pic) = self.current.as_ref()?;
        if unchanged == Some(*pts) {
            return Some((*pts, None));
        }
        Some((*pts, Some(pic_rgb(pic, self.rotation, w, h))))
    }

    /// `n` stereo frames of sound at 48 kHz, starting `from` frames into the video.
    pub fn sound(&mut self, read: &mut Read, from: u64, n: usize) -> Vec<i16> {
        if self.adec.is_none() {
            return vec![0; n * 2];
        }
        if !self.a_started || from != self.a_next {
            let t = (from as i64) * 1_000_000 / 48_000;
            self.src.with(read, |d, r| d.seek_audio(r, t));
            if let Some(a) = self.adec.as_mut() {
                a.reset();
            }
            self.abuf.clear();
            self.abuf_start = None;
            self.a_end = false;
            self.a_started = true;
            self.a_next = from;
        }
        let want_end_us = ((from + n as u64) as i64) * 1_000_000 / 48_000 + 20_000;
        while !self.a_end {
            if let Some(s) = self.abuf_start {
                let end_us = (s + (self.abuf.len() / 2) as i64) * 1_000_000 / self.arate.max(1) as i64;
                if end_us >= want_end_us {
                    break;
                }
            }
            let Some(p) = self.src.with(read, |d, r| d.next_audio(r)) else {
                self.a_end = true;
                break;
            };
            let Some(a) = self.adec.as_mut() else { break };
            let pcm = a.decode(&p.data);
            if pcm.is_empty() {
                continue;
            }
            if self.abuf_start.is_none() {
                self.arate = a.rate().max(1);
                let t = p.pts.unwrap_or((from as i64) * 1_000_000 / 48_000);
                self.abuf_start = Some(t * self.arate as i64 / 1_000_000);
            }
            self.abuf.extend_from_slice(&pcm);
        }
        let mut out = Vec::with_capacity(n * 2);
        let Some(start) = self.abuf_start else {
            self.a_next = from + n as u64;
            return vec![0; n * 2];
        };
        let rate = self.arate as i64;
        let frames = (self.abuf.len() / 2) as i64;
        for k in 0..n as i64 {
            // Position in the decoded sound, in 1/256 frames.
            let p = (from as i64 + k) * rate * 256 / 48_000 - start * 256;
            let i = p >> 8;
            let f = (p & 255) as i32;
            for ch in 0..2usize {
                let s = |j: i64| if j >= 0 && j < frames { self.abuf[j as usize * 2 + ch] as i32 } else { 0 };
                out.push(((s(i) * (256 - f) + s(i + 1) * f) >> 8) as i16);
            }
        }
        // Drop what's been played.
        let used = ((from as i64 + n as i64) * rate / 48_000 - start - 1).clamp(0, frames);
        if used > 0 {
            self.abuf.drain(..used as usize * 2);
            self.abuf_start = Some(start + used);
        }
        self.a_next = from + n as u64;
        out
    }
}

/// A picture to 0x00RRGGBB at w x h, turned `rotation` degrees clockwise.
fn pic_rgb(pic: &Pic, rotation: u32, w: usize, h: usize) -> Vec<u32> {
    match pic {
        Pic::Yuv(f) => crate::video::mp4src::frame_rgb(f, rotation, w, h),
        Pic::Rgb(img) => {
            let mut px = vec![0u32; w * h];
            for y in 0..h {
                let sy = (y * img.h / h).min(img.h - 1);
                for x in 0..w {
                    px[y * w + x] = img.px[sy * img.w + (x * img.w / w).min(img.w - 1)] & 0xFF_FFFF;
                }
            }
            px
        }
    }
}

//! A phone/camera MP4 opened for playback or editing: the picture at any moment (decoding
//! from the nearest key frame when jumping around) and the sound.

use alloc::collections::{BinaryHeap, VecDeque};
use alloc::vec::Vec;
use core::cmp::Reverse;

use super::aac::Aac;
use super::h264d::{Decoder, Frame};
use super::mp4read::{Codec, Movie, ReadFn};

pub struct Mp4Source {
    pub movie: Movie,
    /// Shown size (after cropping and rotation) and rotation in degrees.
    pub width: usize,
    pub height: usize,
    pub rotation: u32,
    pub duration_ms: u64,
    dec: Option<Decoder>,
    next: usize,
    /// Presentation times of samples fed to the decoder but not yet out.
    pending: BinaryHeap<Reverse<i64>>,
    frames: VecDeque<(i64, Frame)>,
    current: Option<(i64, Frame)>,
    /// Earliest presentation time (the movie's time zero).
    pts0: i64,
    aac: Option<Aac>,
    asc: Vec<u8>,
    aac_next: usize,
    audio_cache: VecDeque<(usize, Vec<i16>)>,
    pub audio_rate: u32,
    /// Why the pictures can't be shown, if they can't.
    pub problem: Option<&'static str>,
}

impl Mp4Source {
    pub fn open(read: &mut ReadFn, file_len: u64) -> Result<Mp4Source, &'static str> {
        let movie = Movie::parse(read, file_len)?;
        let duration_ms = movie.duration_ms();
        let mut s = Mp4Source {
            width: 0,
            height: 0,
            rotation: 0,
            duration_ms,
            dec: None,
            next: 0,
            pending: BinaryHeap::new(),
            frames: VecDeque::new(),
            current: None,
            pts0: 0,
            aac: None,
            asc: Vec::new(),
            aac_next: usize::MAX,
            audio_cache: VecDeque::new(),
            audio_rate: 0,
            problem: None,
            movie,
        };
        if let Some(v) = &s.movie.video {
            s.rotation = v.rotation;
            s.pts0 = v.samples.iter().map(|x| x.dts as i64 + x.cts as i64).min().unwrap_or(0);
            match &v.codec {
                Codec::H264 { avcc } => {
                    let mut d = Decoder::new();
                    d.configure(avcc)?;
                    let (w, h) = d.size().unwrap_or((v.width as usize, v.height as usize));
                    s.dec = Some(d);
                    if s.rotation == 90 || s.rotation == 270 {
                        (s.width, s.height) = (h, w);
                    } else {
                        (s.width, s.height) = (w, h);
                    }
                }
                Codec::Other(name) => {
                    s.problem = Some(if name == "hvc1" || name == "hev1" {
                        "This video uses HEVC (\"High Efficiency\"), which Solon can't play yet"
                    } else {
                        "This video's picture format isn't supported"
                    });
                    (s.width, s.height) = (v.width as usize, v.height as usize);
                }
                _ => s.problem = Some("This video's picture format isn't supported"),
            }
        }
        if let Some(a) = &s.movie.audio {
            if let Codec::Aac { asc } = &a.codec {
                if let Ok(d) = Aac::new(asc) {
                    s.audio_rate = d.rate;
                    s.asc = asc.clone();
                    s.aac = Some(d);
                }
            }
        }
        Ok(s)
    }

    /// Presentation time of the picture last returned by frame_at.
    pub fn current_pts(&self) -> Option<i64> {
        self.current.as_ref().map(|c| c.0)
    }

    pub fn has_video(&self) -> bool {
        self.dec.is_some()
    }

    fn timescale(&self) -> i64 {
        self.movie.video.as_ref().map(|v| v.timescale.max(1) as i64).unwrap_or(1000)
    }

    /// Start decoding again from the last key frame at or before `target` (track time).
    fn seek(&mut self, target: i64) {
        let Some(v) = &self.movie.video else { return };
        let mut k = 0;
        for (i, s) in v.samples.iter().enumerate() {
            if s.dts as i64 > target {
                break;
            }
            if s.key {
                k = i;
            }
        }
        if let Some(d) = self.dec.as_mut() {
            d.reset();
        }
        self.next = k;
        self.pending.clear();
        self.frames.clear();
        self.current = None;
    }

    /// Feed one more sample (or flush at the end). Returns false when nothing is left.
    fn feed(&mut self, read: &mut ReadFn) -> bool {
        let Some(v) = &self.movie.video else { return false };
        let Some(d) = self.dec.as_mut() else { return false };
        if self.next < v.samples.len() {
            let s = v.samples[self.next];
            self.next += 1;
            self.pending.push(Reverse(s.dts as i64 + s.cts as i64));
            if let Some(data) = read(s.offset, s.size as usize) {
                let _ = d.decode_sample(&data);
            }
        } else if !self.pending.is_empty() {
            d.flush();
        } else {
            return false;
        }
        while let Some(f) = d.pop() {
            let pts = self.pending.pop().map(|r| r.0).unwrap_or(0);
            self.frames.push_back((pts, f));
        }
        if self.next >= v.samples.len() && self.frames.is_empty() {
            // Flushed and still nothing: drop the bookkeeping.
            self.pending.clear();
        }
        true
    }

    /// The picture showing at `ms` into the movie. `budget`: most samples to decode now
    /// (playback keeps it small; jumping around may need more).
    pub fn frame_at(&mut self, read: &mut ReadFn, ms: u64, budget: usize) -> Option<&Frame> {
        self.dec.as_ref()?;
        let ts = self.timescale();
        let target = self.pts0 + ms as i64 * ts / 1000;
        let behind = self.current.as_ref().is_some_and(|c| c.0 > target);
        // A key frame after where the decoder is, but not after the target: jump to it.
        let ahead = self.key_index(target).is_some_and(|k| k > self.next);
        if behind || ahead {
            self.seek(target);
        }
        let _ = ts;
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
            // Before the first frame: show it.
            self.current = self.frames.pop_front();
        }
        self.current.as_ref().map(|c| &c.1)
    }

    /// The last key frame sample at or before `target`.
    fn key_index(&self, target: i64) -> Option<usize> {
        let v = self.movie.video.as_ref()?;
        let mut k = None;
        for (i, s) in v.samples.iter().enumerate() {
            if s.dts as i64 > target {
                break;
            }
            if s.key {
                k = Some(i);
            }
        }
        k
    }

    /// Sound frames [from, from + n) at `audio_rate`, interleaved stereo.
    pub fn audio(&mut self, read: &mut ReadFn, from: u64, n: usize) -> Vec<i16> {
        let mut out = Vec::with_capacity(n * 2);
        if self.aac.is_none() {
            out.resize(n * 2, 0);
            return out;
        }
        let total = self.movie.audio.as_ref().map(|a| a.samples.len()).unwrap_or(0);
        let mut pos = from;
        while out.len() < n * 2 {
            let p = (pos / 1024) as usize;
            if p >= total {
                break;
            }
            let pcm = self.packet(read, p);
            let start = (pos % 1024) as usize;
            let take = (1024 - start).min(n - out.len() / 2);
            out.extend_from_slice(&pcm[start * 2..(start + take) * 2]);
            pos += take as u64;
        }
        out.resize(n * 2, 0);
        out
    }

    fn packet(&mut self, read: &mut ReadFn, p: usize) -> Vec<i16> {
        if let Some((_, pcm)) = self.audio_cache.iter().find(|e| e.0 == p) {
            return pcm.clone();
        }
        // Each packet overlaps the one before: start one early after a jump.
        if self.aac_next != p {
            if let Ok(d) = Aac::new(&self.asc) {
                self.aac = Some(d);
            }
            self.aac_next = p.saturating_sub(1);
        }
        let mut last = Vec::new();
        while self.aac_next <= p {
            let idx = self.aac_next;
            let Some(s) = self.movie.audio.as_ref().and_then(|a| a.samples.get(idx).copied()) else { break };
            let pcm = read(s.offset, s.size as usize).and_then(|d| self.aac.as_mut().and_then(|a| a.decode(&d).ok())).unwrap_or_else(|| alloc::vec![0; 2048]);
            self.aac_next += 1;
            if self.audio_cache.len() >= 16 {
                self.audio_cache.pop_front();
            }
            self.audio_cache.push_back((idx, pcm.clone()));
            last = pcm;
        }
        if last.len() < 2048 {
            last.resize(2048, 0);
        }
        last
    }

    pub fn audio_frames(&self) -> u64 {
        self.movie.audio.as_ref().map(|a| a.samples.len() as u64 * 1024).unwrap_or(0)
    }
}

/// Convert a decoded frame to 0x00RRGGBB at w x h, turned by `rotation` degrees clockwise.
pub fn frame_rgb(f: &Frame, rotation: u32, w: usize, h: usize) -> Vec<u32> {
    if rotation == 0 {
        return f.to_rgb(w, h);
    }
    // Convert at the unrotated size, then turn.
    let (sw, sh) = if rotation == 180 { (w, h) } else { (h, w) };
    let src = f.to_rgb(sw, sh);
    let mut out = alloc::vec![0u32; w * h];
    for oy in 0..h {
        for ox in 0..w {
            let (sx, sy) = match rotation {
                90 => (oy, sh - 1 - ox),
                180 => (sw - 1 - ox, sh - 1 - oy),
                _ => (sw - 1 - oy, ox),
            };
            out[oy * w + ox] = src[sy * sw + sx];
        }
    }
    out
}

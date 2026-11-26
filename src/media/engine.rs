//! One video being played: the picture at any moment (decoding forward from the right key
//! frame when jumping around) and the sound, as 48 kHz stereo.
//!
//! The container is read on the first core (it reads the disk); the pictures can be decoded
//! on another core (`decode_elsewhere`, while a video plays), so the first core stays free for
//! the mouse, the sound and the screen. The two meet in a `Pipe`: packets go in, pictures in
//! display order come out, a few ahead of time.

use alloc::boxed::Box;
use alloc::collections::{BinaryHeap, VecDeque};
use alloc::string::String;
use alloc::sync::Arc;
use alloc::vec;
use alloc::vec::Vec;
use core::cmp::Reverse;
use core::sync::atomic::{AtomicU64, Ordering::SeqCst};
use spin::Mutex;

use super::dec::{self, AudioDec, Pic, VideoDec};
use super::{Demux, Packet, Read};

/// Pictures decoded ahead of time, and packets waiting for the decoder.
const MAX_FRAMES: usize = 8;
const MAX_PACKETS: usize = 8;

/// Packets for the video decoder and the pictures it made, shared with the core decoding them.
struct Pipe {
    st: Mutex<PipeState>,
    /// Bumped whenever the player adds packets, takes pictures, jumps or stops: what the
    /// decoding core sleeps on.
    bell: AtomicU64,
    /// Time spent decoding (microseconds) and packets decoded, for the log.
    busy_us: AtomicU64,
    decoded: AtomicU64,
}

struct PipeState {
    /// Bumped on every jump: what was decoded before it is thrown away.
    gen: u64,
    packets: VecDeque<Packet>,
    /// The last packet is in: the decoder lets out the pictures it holds back.
    end: bool,
    /// Pictures in display order, with their times.
    frames: VecDeque<(i64, Pic)>,
    /// Decoding a packet right now (its pictures may still come).
    busy: bool,
    /// Everything up to the end is decoded.
    drained: bool,
    stop: bool,
}

impl Pipe {
    fn ring(&self) {
        self.bell.fetch_add(1, SeqCst);
    }
}

/// The video decoder and what it needs to time its pictures.
struct Decoding {
    dec: Box<dyn VideoDec>,
    timed: bool,
    /// Times of packets whose pictures haven't come out yet (decoders that don't time them).
    pending: BinaryHeap<Reverse<i64>>,
    last_pts: i64,
    gen: u64,
    flushed: bool,
}

// A decoder belongs to one core at a time (moved to the decoding core whole, never shared);
// the reference-counted pictures inside it never leave it (pictures come out as copies).
unsafe impl Send for Decoding {}

impl Decoding {
    /// Decode one packet (or let out the last pictures at the end). False when there's
    /// nothing to do now (no packets, or enough pictures waiting).
    fn step(&mut self, pipe: &Pipe) -> bool {
        let pkt = {
            let mut st = pipe.st.lock();
            if st.gen != self.gen {
                self.gen = st.gen;
                self.dec.reset();
                self.pending.clear();
                self.flushed = false;
            }
            if st.frames.len() >= MAX_FRAMES {
                return false;
            }
            let p = st.packets.pop_front();
            if p.is_none() && !(st.end && !self.flushed) {
                return false;
            }
            st.busy = true;
            p
        };
        let gen = self.gen;
        let t0 = crate::cpu::now_us();
        match &pkt {
            Some(p) if self.timed => {
                if let Some(t) = p.pts {
                    self.last_pts = t;
                }
                self.dec.decode_at(&p.data, p.pts);
            }
            Some(p) => {
                let pts = p.pts.unwrap_or(self.last_pts);
                self.last_pts = pts;
                self.pending.push(Reverse(pts));
                self.dec.decode(&p.data);
            }
            None => {
                self.dec.flush();
                self.flushed = true;
            }
        }
        let mut out = Vec::new();
        while let Some((pts, pic)) = if self.timed { self.dec.pop_at() } else { None } {
            out.push((pts, pic));
        }
        while let Some(pic) = self.dec.pop() {
            let pts = self.pending.pop().map(|r| r.0).unwrap_or(self.last_pts);
            out.push((pts, pic));
        }
        // A decoder that skipped a damaged frame would leave its time behind.
        while self.pending.len() > 32 {
            self.pending.pop();
        }
        pipe.busy_us.fetch_add(crate::cpu::now_us().saturating_sub(t0), SeqCst);
        pipe.decoded.fetch_add(1, SeqCst);
        let mut st = pipe.st.lock();
        st.busy = false;
        if st.gen == gen {
            st.frames.extend(out);
            if self.flushed {
                st.drained = true;
            }
        }
        true
    }
}

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
    /// The video decoder while it decodes on this core (None while another core has it, or
    /// when there are no pictures), and where its packets and pictures meet.
    here: Option<Decoding>,
    pipe: Option<Arc<Pipe>>,
    timed: bool,
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
    current: Option<(i64, Pic)>,
    /// The last key frame given to the decoder, and the last packet's time.
    fed_key: i64,
    last_pts: i64,
    /// The container has no more pictures (from where it was last sent).
    video_end: bool,
    // Sound.
    abuf: Vec<i16>,
    /// Position of abuf[0], in frames at `arate` from the start (None = nothing yet).
    abuf_start: Option<i64>,
    arate: u32,
    a_next: u64,
    a_end: bool,
    a_started: bool,
}

impl Drop for Engine {
    fn drop(&mut self) {
        // A decoding core lets go of the decoder and the pictures.
        if let Some(p) = &self.pipe {
            p.st.lock().stop = true;
            p.ring();
        }
    }
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
        let timed = vdec.as_ref().is_some_and(|d| d.timed());
        let pipe = vdec.as_ref().map(|_| {
            Arc::new(Pipe {
                st: Mutex::new(PipeState { gen: 0, packets: VecDeque::new(), end: false, frames: VecDeque::new(), busy: false, drained: false, stop: false }),
                bell: AtomicU64::new(0),
                busy_us: AtomicU64::new(0),
                decoded: AtomicU64::new(0),
            })
        });
        let here = vdec.map(|dec| Decoding { dec, timed, pending: BinaryHeap::new(), last_pts: 0, gen: 0, flushed: false });
        let mut e = Engine {
            src: Src { demux, cache },
            here,
            pipe,
            timed,
            adec,
            width: width.max(1),
            height: height.max(1),
            rotation,
            duration_ms,
            problem,
            sound_problem,
            started: false,
            current: None,
            fed_key: 0,
            last_pts: 0,
            video_end: false,
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
        self.advance(read, 0, 300);
        let size = self.current.as_ref().map(|(_, p)| match p {
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

    /// Time spent decoding pictures (microseconds) and packets decoded since the last call
    /// (on whichever core decodes them), for the log.
    pub fn take_decode_stats(&self) -> (u64, u64) {
        match &self.pipe {
            Some(p) => (p.busy_us.swap(0, SeqCst), p.decoded.swap(0, SeqCst)),
            None => (0, 0),
        }
    }

    /// Decode the pictures on a core of their own from now on (while the video plays), if
    /// one is free. The first core then only reads the file and shows what's ready.
    pub fn decode_elsewhere(&mut self) -> bool {
        if self.here.is_none() || !crate::smp::can_spawn() {
            return false;
        }
        let (Some(mut d), Some(pipe)) = (self.here.take(), self.pipe.clone()) else { return false };
        let task = Box::new(move || loop {
            let seen = pipe.bell.load(SeqCst);
            if pipe.st.lock().stop {
                break;
            }
            if !d.step(&pipe) {
                crate::smp::sleep_on(&pipe.bell, seen);
            }
        });
        crate::smp::spawn(task).is_ok()
    }

    fn seek(&mut self, read: &mut Read, target: i64) {
        let k = self.src.with(read, |d, r| d.seek_video(r, target));
        if let Some(p) = &self.pipe {
            let mut st = p.st.lock();
            st.gen += 1;
            st.packets.clear();
            st.frames.clear();
            st.end = false;
            st.drained = false;
            drop(st);
            p.ring();
        }
        self.current = None;
        self.fed_key = k;
        self.last_pts = k;
        self.video_end = false;
        self.started = true;
    }

    /// Read picture packets from the container into the pipe while it has room.
    fn fill(&mut self, read: &mut Read, pipe: &Pipe) {
        while !self.video_end {
            {
                let st = pipe.st.lock();
                if st.packets.len() >= MAX_PACKETS || st.packets.len() + st.frames.len() >= MAX_PACKETS + MAX_FRAMES {
                    return;
                }
            }
            match self.src.with(read, |d, r| d.next_video(r)) {
                Some(p) if self.timed => {
                    if let Some(t) = p.pts {
                        self.last_pts = t;
                    }
                    if p.key {
                        self.fed_key = self.fed_key.max(self.last_pts);
                    }
                    pipe.st.lock().packets.push_back(p);
                }
                Some(mut p) => {
                    let pts = p.pts.unwrap_or(self.last_pts + 33_333);
                    self.last_pts = pts;
                    if p.key {
                        self.fed_key = self.fed_key.max(pts);
                    }
                    if p.data.is_empty() {
                        continue;
                    }
                    p.pts = Some(pts);
                    pipe.st.lock().packets.push_back(p);
                }
                None => {
                    self.video_end = true;
                    pipe.st.lock().end = true;
                }
            }
            pipe.ring();
        }
    }

    /// The picture showing at `ms`, as 0x00RRGGBB at w x h, and an id for it. If the id
    /// equals `unchanged`, no pixels are made (the caller keeps what it has).
    /// `budget`: the most packets to decode now (small while playing).
    pub fn picture(&mut self, read: &mut Read, ms: u64, w: usize, h: usize, budget: usize, unchanged: Option<i64>) -> Option<(i64, Option<Vec<u32>>)> {
        self.pipe.as_ref()?;
        self.advance(read, ms, budget);
        let (pts, pic) = self.current.as_ref()?;
        if unchanged == Some(*pts) {
            return Some((*pts, None));
        }
        Some((*pts, Some(pic_rgb(pic, self.rotation, w, h))))
    }

    /// The picture shown now: its time (microseconds) and the picture itself.
    pub fn current(&self) -> Option<(i64, &Pic)> {
        self.current.as_ref().map(|(t, p)| (*t, p))
    }

    /// How much the picture is turned (0, 90, 180 or 270 degrees).
    pub fn rotation(&self) -> u32 {
        self.rotation
    }

    /// The current picture at w x h (0x00RRGGBB), on one core (Videos has a faster way).
    pub fn current_rgb(&self, w: usize, h: usize) -> Option<Vec<u32>> {
        self.current.as_ref().map(|(_, p)| pic_rgb(p, self.rotation, w, h))
    }

    /// Move towards the picture shown at `ms`: read packets for the decoder, take the
    /// pictures that are due, and (when decoding on this core) decode at most `budget`
    /// packets. True when the current picture is the one for `ms` (or no later one is coming).
    pub fn advance(&mut self, read: &mut Read, ms: u64, budget: usize) -> bool {
        let Some(pipe) = self.pipe.clone() else { return true };
        let target = ms as i64 * 1000;
        let behind = self.current.as_ref().is_some_and(|c| c.0 > target + 1000);
        let key = self.src.demux.key_before(target);
        let ahead = match key {
            Some(k) => k > self.fed_key,
            None => target > self.last_pts + 4_000_000, // files without an index: far ahead
        };
        // At the end with nothing to show (a jump past the last picture): start again.
        let all_shown = {
            let st = pipe.st.lock();
            st.drained && st.frames.is_empty() && st.packets.is_empty() && !st.busy
        };
        let stuck = all_shown && (self.current.is_none() || self.current.as_ref().is_some_and(|c| c.0 + 1_000_000 < target || c.0 > target));
        if !self.started || behind || ahead || stuck {
            self.seek(read, target);
        }
        let mut fed = 0;
        loop {
            self.fill(read, &pipe);
            let (more, done) = {
                let mut st = pipe.st.lock();
                let before = st.frames.len();
                while st.frames.front().is_some_and(|f| f.0 <= target) {
                    self.current = st.frames.pop_front();
                }
                if self.current.is_none() {
                    self.current = st.frames.pop_front();
                }
                if st.frames.len() != before {
                    pipe.ring(); // room for more pictures
                }
                (!st.frames.is_empty(), st.drained && st.packets.is_empty() && !st.busy)
            };
            if more || done {
                return true;
            }
            let Some(d) = self.here.as_mut() else { return false };
            if fed >= budget || !d.step(&pipe) {
                return false;
            }
            fed += 1;
        }
    }

    /// Where the sound ends (in 48 kHz frames from the start), once it's been decoded to
    /// the end: a player can start the next song right after it.
    pub fn sound_end(&self) -> Option<u64> {
        if !self.a_end {
            return None;
        }
        match self.abuf_start {
            Some(s) => Some(((s + (self.abuf.len() / 2) as i64).max(0) as u64) * 48_000 / self.arate.max(1) as u64),
            None => Some(self.a_next),
        }
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
                // To the nearest frame (times are whole microseconds).
                self.abuf_start = Some((t * self.arate as i64 + 500_000).div_euclid(1_000_000));
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
        let at = |j: i64, ch: usize| if j >= 0 && j < frames { self.abuf[j as usize * 2 + ch] as f32 } else { 0.0 };
        if rate == 48_000 {
            // Already 48 kHz: the samples as they are.
            for k in 0..n as i64 {
                let i = from as i64 + k - start;
                out.push(at(i, 0) as i16);
                out.push(at(i, 1) as i16);
            }
        } else {
            // Any other rate: a windowed-sinc filter (see `resampler`), its weights in between
            // two of its phases mixed for where each output sample falls.
            let f = resampler(self.arate);
            for k in 0..n as i64 {
                let num = (from as i64 + k) * rate; // in 1/48000 input frames
                let i = num.div_euclid(48_000) - start;
                let pf = num.rem_euclid(48_000) as f32 * (PHASES as f32 / 48_000.0);
                let p0 = (pf as usize).min(PHASES - 1);
                let a = pf - p0 as f32;
                let (c0, c1) = (&f.coef[p0 * TAPS..(p0 + 1) * TAPS], &f.coef[(p0 + 1) * TAPS..(p0 + 2) * TAPS]);
                let first = i - (TAPS as i64 / 2 - 1);
                let (mut l, mut r) = (0f32, 0f32);
                if first >= 0 && first + TAPS as i64 <= frames {
                    let src = &self.abuf[first as usize * 2..(first as usize + TAPS) * 2];
                    for t in 0..TAPS {
                        let w = c0[t] + a * (c1[t] - c0[t]);
                        l += src[t * 2] as f32 * w;
                        r += src[t * 2 + 1] as f32 * w;
                    }
                } else {
                    for t in 0..TAPS {
                        let w = c0[t] + a * (c1[t] - c0[t]);
                        l += at(first + t as i64, 0) * w;
                        r += at(first + t as i64, 1) * w;
                    }
                }
                out.push(libm::roundf(l).clamp(-32768.0, 32767.0) as i16);
                out.push(libm::roundf(r).clamp(-32768.0, 32767.0) as i16);
            }
        }
        // Drop what's been played (keeping what the filter still needs behind the next sample).
        let used = ((from as i64 + n as i64) * rate / 48_000 - start - TAPS as i64 / 2 - 1).clamp(0, frames);
        if used > 0 {
            self.abuf.drain(..used as usize * 2);
            self.abuf_start = Some(start + used);
        }
        self.a_next = from + n as u64;
        out
    }
}

/// Sound at other rates is changed to 48 kHz by a windowed-sinc filter: TAPS input samples per
/// output sample, its weights worked out for PHASES + 1 places between two input samples.
const TAPS: usize = 64;
const PHASES: usize = 256;

struct Resampler {
    rate: u32,
    coef: Vec<f32>,
}

/// The filter for `rate` (made once per rate): a sinc cut off just below the lower of the two
/// rates' highest frequencies, shaped by a Kaiser window (about 80 dB stopband), each phase
/// scaled to pass steady levels unchanged.
fn resampler(rate: u32) -> Arc<Resampler> {
    static CACHE: Mutex<Vec<Arc<Resampler>>> = Mutex::new(Vec::new());
    let mut cache = CACHE.lock();
    if let Some(r) = cache.iter().find(|r| r.rate == rate) {
        return r.clone();
    }
    let fc = 0.5 * (48_000.0f64 / rate as f64).min(1.0) * 0.92; // cycles per input sample
    let beta = 8.0;
    let i0 = |x: f64| {
        // The zeroth-order modified Bessel function, by its series.
        let (mut sum, mut term, mut k) = (1.0f64, 1.0f64, 1.0f64);
        while term > 1e-12 * sum {
            term *= (x / (2.0 * k)) * (x / (2.0 * k));
            sum += term;
            k += 1.0;
        }
        sum
    };
    let norm = i0(beta);
    let mut coef = vec![0f32; (PHASES + 1) * TAPS];
    for p in 0..=PHASES {
        let frac = p as f64 / PHASES as f64;
        let mut row = [0f64; TAPS];
        let mut sum = 0.0;
        for (t, v) in row.iter_mut().enumerate() {
            let d = (t as f64 - (TAPS / 2 - 1) as f64) - frac;
            let x = 2.0 * fc * d;
            let sinc = if x.abs() < 1e-9 { 1.0 } else { libm::sin(core::f64::consts::PI * x) / (core::f64::consts::PI * x) };
            let w = d / (TAPS as f64 / 2.0);
            let win = if w.abs() >= 1.0 { 0.0 } else { i0(beta * libm::sqrt(1.0 - w * w)) / norm };
            *v = sinc * win;
            sum += *v;
        }
        for t in 0..TAPS {
            coef[p * TAPS + t] = (row[t] / sum) as f32;
        }
    }
    let r = Arc::new(Resampler { rate, coef });
    cache.push(r.clone());
    r
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

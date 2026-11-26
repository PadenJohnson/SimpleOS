//! Sound-only files: MP3 (and MP2), AAC with ADTS headers, FLAC, Ogg (Vorbis, Opus and FLAC
//! inside), WAV, AIFF and AMR. Each is a `Demux` with sound and no pictures. Tags (title,
//! artist, cover...) are read separately, in `tags`.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::flac::{self, StreamInfo};
use super::{ACodec, AudioInfo, Demux, Info, Packet, Read};

/// Recognise a sound-only file from its first bytes. `strong`: only by a signature that
/// can't be mistaken (checked before video containers); otherwise also by frame syncs
/// (MP3, ADTS), checked last.
pub fn open(read: &mut Read, size: u64, strong: bool) -> Option<Result<Box<dyn Demux>, String>> {
    let head = read(0, 64)?;
    let boxed = |r: Result<Box<dyn Demux>, String>| Some(r);
    if head.len() >= 4 && &head[0..4] == b"OggS" {
        return boxed(OggFile::open(read, size).map(|d| Box::new(d) as Box<dyn Demux>));
    }
    if head.len() >= 4 && &head[0..4] == b"fLaC" {
        return boxed(FlacFile::open(read, size, 0).map(|d| Box::new(d) as Box<dyn Demux>));
    }
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"WAVE" {
        return boxed(PcmFile::open_wav(read, size).map(|d| Box::new(d) as Box<dyn Demux>));
    }
    if head.len() >= 12 && &head[0..4] == b"FORM" && (&head[8..12] == b"AIFF" || &head[8..12] == b"AIFC") {
        return boxed(PcmFile::open_aiff(read, size).map(|d| Box::new(d) as Box<dyn Demux>));
    }
    if head.starts_with(b"#!AMR") {
        return boxed(AmrFile::open(read, size).map(|d| Box::new(d) as Box<dyn Demux>));
    }
    // An ID3v2 tag in front: MP3, AAC or FLAC behind it.
    let mut start = 0u64;
    if head.len() >= 10 && &head[0..3] == b"ID3" {
        start = id3_size(&head);
        if let Some(h) = read(start, 8) {
            if h.starts_with(b"fLaC") {
                return boxed(FlacFile::open(read, size, start).map(|d| Box::new(d) as Box<dyn Demux>));
            }
        }
    } else if strong {
        return None;
    }
    let probe = read(start, 16 * 1024)?;
    if let Some(off) = sync_run(&probe, adts_header, 3) {
        return boxed(AdtsFile::open(read, size, start + off as u64).map(|d| Box::new(d) as Box<dyn Demux>));
    }
    if let Some(off) = sync_run(&probe, |d| mpa_header(d).map(|h| h.len), 3) {
        return boxed(MpaFile::open(read, size, start + off as u64).map(|d| Box::new(d) as Box<dyn Demux>));
    }
    if start > 0 {
        return boxed(Err(String::from("Solon can't read this kind of sound file yet")));
    }
    None
}

/// An ID3v2 tag's whole size (header, frames and footer).
pub fn id3_size(h: &[u8]) -> u64 {
    if h.len() < 10 || &h[0..3] != b"ID3" {
        return 0;
    }
    let n = h[6..10].iter().fold(0u64, |a, &b| a << 7 | (b & 0x7F) as u64);
    10 + n + if h[5] & 0x10 != 0 { 10 } else { 0 }
}

/// Where `n` frames in a row start (each where the last ends), searching from the start.
fn sync_run(d: &[u8], len_at: impl Fn(&[u8]) -> Option<usize>, n: usize) -> Option<usize> {
    'outer: for i in 0..d.len().saturating_sub(8) {
        if d[i] != 0xFF {
            continue;
        }
        let mut p = i;
        for k in 0..n {
            match len_at(&d[p..]) {
                Some(l) if l >= 8 => p += l,
                _ => continue 'outer,
            }
            if p + 8 > d.len() {
                // Ran off the probe after at least one whole frame: good enough.
                if k > 0 {
                    return Some(i);
                }
                continue 'outer;
            }
        }
        return Some(i);
    }
    None
}

fn micros(samples: u64, rate: u32) -> i64 {
    (samples as u128 * 1_000_000 / rate.max(1) as u128) as i64
}

// ---- MPEG audio (MP3, MP2) -----------------------------------------------------------------

#[derive(Clone, Copy)]
pub struct MpaHeader {
    pub len: usize,
    pub rate: u32,
    pub spf: u32,
    pub kbps: u32,
    pub channels: u16,
    pub mpeg1: bool,
    pub layer: u8,
}

/// An MPEG audio frame header (4 bytes), with the frame's length.
pub fn mpa_header(d: &[u8]) -> Option<MpaHeader> {
    if d.len() < 4 || d[0] != 0xFF || d[1] & 0xE0 != 0xE0 {
        return None;
    }
    let ver = (d[1] >> 3) & 3; // 0: 2.5, 2: 2, 3: 1
    let layer = 4 - ((d[1] >> 1) & 3); // 1..3
    let bri = (d[2] >> 4) as usize;
    let sri = ((d[2] >> 2) & 3) as usize;
    if ver == 1 || layer == 4 || bri == 0 || bri == 15 || sri == 3 {
        return None;
    }
    let mpeg1 = ver == 3;
    const B1: [[u32; 15]; 3] = [
        [0, 32, 64, 96, 128, 160, 192, 224, 256, 288, 320, 352, 384, 416, 448],
        [0, 32, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320, 384],
        [0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320],
    ];
    const B2: [[u32; 15]; 2] = [[0, 32, 48, 56, 64, 80, 96, 112, 128, 144, 160, 176, 192, 224, 256], [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160]];
    let kbps = if mpeg1 { B1[layer as usize - 1][bri] } else { B2[(layer as usize - 1).min(1)][bri] };
    let rate = [44100, 48000, 32000][sri] >> match ver {
        3 => 0,
        2 => 1,
        _ => 2,
    };
    let pad = ((d[2] >> 1) & 1) as usize;
    let (len, spf) = match layer {
        1 => ((12 * kbps as usize * 1000 / rate as usize + pad) * 4, 384),
        2 => (144 * kbps as usize * 1000 / rate as usize + pad, 1152),
        _ if mpeg1 => (144 * kbps as usize * 1000 / rate as usize + pad, 1152),
        _ => (72 * kbps as usize * 1000 / rate as usize + pad, 576),
    };
    let channels = if d[3] >> 6 == 3 { 1 } else { 2 };
    Some(MpaHeader { len, rate, spf, kbps, channels, mpeg1, layer })
}

struct MpaFile {
    info: Info,
    /// The first audio frame, and where the audio ends (before an ID3v1 or APE tag).
    first: u64,
    end: u64,
    pos: u64,
    /// The next frame's first sample (counted from the start of the sound, after the delay).
    sample: i64,
    rate: u32,
    /// The Xing table of contents (0-255 per percent of the duration) and the audio's size.
    toc: Option<([u8; 100], u64)>,
    kbps: u32,
    delay: i64,
}

impl MpaFile {
    fn open(read: &mut Read, size: u64, first: u64) -> Result<MpaFile, String> {
        let d = read(first, 4096).ok_or("Can't read the file")?;
        let h = mpa_header(&d).ok_or("This sound file can't be read")?;
        let mut end = size;
        if let Some(t) = read(size.saturating_sub(128), 128) {
            if t.starts_with(b"TAG") {
                end -= 128;
            }
        }
        if let Some(t) = read(end.saturating_sub(32), 32) {
            if t.starts_with(b"APETAGEX") {
                let n = u32::from_le_bytes([t[12], t[13], t[14], t[15]]) as u64;
                let header = if t[23] & 0x80 != 0 { 32 } else { 0 };
                end = end.saturating_sub(n + header);
            }
        }
        // A Xing/Info or VBRI frame first: the frame count (and a seek table); it holds no sound.
        let side = match (h.mpeg1, h.channels) {
            (true, 2) => 32,
            (true, _) => 17,
            (false, 2) => 17,
            _ => 9,
        };
        let mut frames = None;
        let mut toc = None;
        let mut audio_start = first;
        // Samples at the start that are the encoder's and decoder's delay, not sound (the LAME
        // tag says how many): skipped, so the sound starts on time and albums run without gaps.
        let mut delay = 0u64;
        let x = 4 + side;
        if d.len() >= x + 160 && (&d[x..x + 4] == b"Xing" || &d[x..x + 4] == b"Info") {
            let flags = u32::from_be_bytes([d[x + 4], d[x + 5], d[x + 6], d[x + 7]]);
            let mut p = x + 8;
            let be32 = |o: usize| u32::from_be_bytes([d[o], d[o + 1], d[o + 2], d[o + 3]]);
            if flags & 1 != 0 {
                frames = Some(be32(p) as u64);
                p += 4;
            }
            let mut bytes = end.saturating_sub(first);
            if flags & 2 != 0 {
                bytes = be32(p) as u64;
                p += 4;
            }
            if flags & 4 != 0 {
                let mut t = [0u8; 100];
                t.copy_from_slice(&d[p..p + 100]);
                toc = Some((t, bytes));
                p += 100;
            }
            if flags & 8 != 0 {
                p += 4;
            }
            if p + 24 <= d.len() && (&d[p..p + 4] == b"LAME" || &d[p..p + 4] == b"Lavc" || &d[p..p + 4] == b"Lavf") {
                let enc = (d[p + 21] as u64) << 4 | (d[p + 22] as u64) >> 4;
                delay = enc + 529;
            }
            audio_start = first + h.len as u64;
        } else if d.len() >= 36 + 26 && &d[36..40] == b"VBRI" {
            frames = Some(u32::from_be_bytes([d[50], d[51], d[52], d[53]]) as u64);
            audio_start = first + h.len as u64;
        }
        let duration_ms = match frames {
            Some(f) if f > 0 => f * h.spf as u64 * 1000 / h.rate as u64,
            _ => end.saturating_sub(first) * 8 / h.kbps.max(1) as u64,
        };
        let info = Info { duration_ms, video: None, audio: Some(AudioInfo { codec: ACodec::MpegAudio, rate: h.rate, channels: h.channels }) };
        Ok(MpaFile { info, first: audio_start, end, pos: audio_start, sample: -(delay as i64), rate: h.rate, toc, kbps: h.kbps, delay: delay as i64 })
    }

    /// The next frame start at or after `pos` (two frames in a row agreeing).
    fn resync(&self, read: &mut Read, pos: u64) -> Option<u64> {
        let d = read(pos, 16 * 1024)?;
        sync_run(&d, |d| mpa_header(d).map(|h| h.len), 2).map(|o| pos + o as u64)
    }
}

impl Demux for MpaFile {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next_video(&mut self, _read: &mut Read) -> Option<Packet> {
        None
    }
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet> {
        for _ in 0..4 {
            if self.pos + 4 > self.end {
                return None;
            }
            let hd = read(self.pos, 4)?;
            match mpa_header(&hd) {
                Some(h) if self.pos + h.len as u64 <= self.end => {
                    let data = read(self.pos, h.len)?;
                    let pts = self.sample * 1_000_000 / self.rate.max(1) as i64;
                    self.pos += h.len as u64;
                    self.sample += h.spf as i64;
                    return Some(Packet { pts: Some(pts), data, key: true });
                }
                Some(_) => return None,
                None => self.pos = self.resync(read, self.pos + 1)?,
            }
        }
        None
    }
    fn seek_video(&mut self, _read: &mut Read, us: i64) -> i64 {
        us
    }
    fn seek_audio(&mut self, read: &mut Read, us: i64) {
        let us = us.max(0) as u64;
        let dur = self.info.duration_ms.max(1) * 1000;
        let span = self.end.saturating_sub(self.first);
        let off = match &self.toc {
            Some((t, bytes)) => {
                let pct = (us as f64 * 100.0 / dur as f64).clamp(0.0, 99.999);
                let i = pct as usize;
                let a = t[i] as f64;
                let b = if i < 99 { t[i + 1] as f64 } else { 256.0 };
                ((a + (b - a) * (pct - i as f64)) / 256.0 * *bytes as f64) as u64
            }
            None => us * self.kbps as u64 / 8000,
        };
        let target = self.first + off.min(span);
        self.pos = if us == 0 { self.first } else { self.resync(read, target).unwrap_or(self.end) };
        self.sample = if us == 0 { -self.delay } else { (us * self.rate as u64 / 1_000_000) as i64 };
    }
}

// ---- AAC with ADTS headers -----------------------------------------------------------------

const AAC_RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];

fn adts_header(d: &[u8]) -> Option<usize> {
    if d.len() < 7 || d[0] != 0xFF || d[1] & 0xF6 != 0xF0 || (d[2] >> 2) & 15 > 12 {
        return None;
    }
    let len = ((d[3] as usize & 3) << 11) | (d[4] as usize) << 3 | (d[5] as usize) >> 5;
    if len < 7 {
        return None;
    }
    Some(len)
}

struct AdtsFile {
    info: Info,
    first: u64,
    end: u64,
    pos: u64,
    sample: u64,
    rate: u32,
    /// Average bytes per second (from the first frames), for jumping.
    bps: u64,
}

impl AdtsFile {
    fn open(read: &mut Read, size: u64, first: u64) -> Result<AdtsFile, String> {
        let d = read(first, 64 * 1024).ok_or("Can't read the file")?;
        let rate = AAC_RATES[((d[2] >> 2) & 15) as usize];
        let channels = (((d[2] & 1) << 2) | (d[3] >> 6)) as u16;
        // Bytes per frame over the first frames: the file's length in time.
        let (mut p, mut n) = (0, 0u64);
        while let Some(l) = adts_header(&d[p..]) {
            if p + l > d.len() {
                break;
            }
            p += l;
            n += 1;
        }
        let per_frame = if n > 0 { p as u64 / n } else { 400 };
        let frames = size.saturating_sub(first) / per_frame.max(1);
        let duration_ms = frames * 1024 * 1000 / rate as u64;
        let bps = per_frame * rate as u64 / 1024;
        let info = Info { duration_ms, video: None, audio: Some(AudioInfo { codec: ACodec::AacAdts, rate, channels: channels.max(1) }) };
        Ok(AdtsFile { info, first, end: size, pos: first, sample: 0, rate, bps })
    }
}

impl Demux for AdtsFile {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next_video(&mut self, _read: &mut Read) -> Option<Packet> {
        None
    }
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet> {
        for _ in 0..4 {
            let hd = read(self.pos, 7)?;
            match adts_header(&hd) {
                Some(l) if self.pos + l as u64 <= self.end => {
                    let data = read(self.pos, l)?;
                    let blocks = (hd[6] & 3) as u64 + 1;
                    let pts = micros(self.sample, self.rate);
                    self.pos += l as u64;
                    self.sample += 1024 * blocks;
                    return Some(Packet { pts: Some(pts), data, key: true });
                }
                Some(_) => return None,
                None => {
                    let d = read(self.pos + 1, 16 * 1024)?;
                    self.pos += 1 + sync_run(&d, adts_header, 2)? as u64;
                }
            }
        }
        None
    }
    fn seek_video(&mut self, _read: &mut Read, us: i64) -> i64 {
        us
    }
    fn seek_audio(&mut self, read: &mut Read, us: i64) {
        let us = us.max(0) as u64;
        let target = (self.first + us * self.bps / 1_000_000).min(self.end);
        self.pos = match read(target, 16 * 1024).and_then(|d| sync_run(&d, adts_header, 2)) {
            Some(o) if us > 0 => target + o as u64,
            _ if us == 0 => self.first,
            _ => self.end,
        };
        self.sample = us * self.rate as u64 / 1_000_000;
    }
}

// ---- FLAC ------------------------------------------------------------------------------------

/// The metadata blocks of a FLAC stream after "fLaC": (STREAMINFO bytes, where the frames
/// start, the seek table as (sample, offset from the first frame)).
fn flac_metadata(read: &mut Read, at: u64) -> Option<(Vec<u8>, u64, Vec<(u64, u64)>)> {
    let mut pos = at + 4;
    let mut info = Vec::new();
    let mut seek = Vec::new();
    loop {
        let h = read(pos, 4)?;
        if h.len() < 4 {
            return None;
        }
        let last = h[0] & 0x80 != 0;
        let kind = h[0] & 0x7F;
        let len = (h[1] as u64) << 16 | (h[2] as u64) << 8 | h[3] as u64;
        if kind == 0 {
            info = read(pos + 4, len as usize)?;
        } else if kind == 3 {
            let d = read(pos + 4, len as usize)?;
            for e in d.chunks_exact(18) {
                let s = u64::from_be_bytes(e[0..8].try_into().ok()?);
                if s != u64::MAX {
                    seek.push((s, u64::from_be_bytes(e[8..16].try_into().ok()?)));
                }
            }
        }
        pos += 4 + len;
        if last || kind == 127 {
            break;
        }
    }
    if info.len() < 34 {
        return None;
    }
    Some((info, pos, seek))
}

struct FlacFile {
    info: Info,
    si: StreamInfo,
    first: u64,
    size: u64,
    pos: u64,
    seek: Vec<(u64, u64)>,
}

impl FlacFile {
    fn open(read: &mut Read, size: u64, at: u64) -> Result<FlacFile, String> {
        let (info, first, seek) = flac_metadata(read, at).ok_or("This FLAC file can't be read")?;
        let si = StreamInfo::parse(&info).ok_or("This FLAC file can't be read")?;
        let duration_ms = if si.total > 0 { si.total * 1000 / si.rate.max(1) as u64 } else { 0 };
        let info = Info { duration_ms, video: None, audio: Some(AudioInfo { codec: ACodec::Flac { info }, rate: si.rate, channels: si.channels as u16 }) };
        Ok(FlacFile { info, si, first, size, pos: first, seek })
    }

    /// The first frame start at or after `pos`, and its first sample.
    fn frame_at(&self, read: &mut Read, pos: u64) -> Option<(u64, u64)> {
        let d = read(pos, 64 * 1024)?;
        for i in 0..d.len().saturating_sub(16) {
            if d[i] == 0xFF && d[i + 1] & 0xFE == 0xF8 {
                if let Some(h) = flac::frame_header(&d[i..], &self.si) {
                    // Check the frame decodes to its end (the CRC) when the next one is near.
                    if flac::frame_len(&d[i..], &self.si, false).is_some() || d.len() - i < 64 * 1024 / 2 {
                        return Some((pos + i as u64, h.first_sample(self.si.max_block)));
                    }
                }
            }
        }
        None
    }
}

impl Demux for FlacFile {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next_video(&mut self, _read: &mut Read) -> Option<Packet> {
        None
    }
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet> {
        if self.pos >= self.size {
            return None;
        }
        let want = (self.si.max_frame as usize).clamp(16 * 1024, 1 << 20) * 2 + 64;
        let d = read(self.pos, want)?;
        let at_end = self.pos + d.len() as u64 >= self.size;
        let h = flac::frame_header(&d, &self.si);
        let len = match h.and_then(|_| flac::frame_len(&d, &self.si, at_end)) {
            Some(l) => l,
            None => {
                // Lost (damaged data): find the next frame.
                let (p, _) = self.frame_at(read, self.pos + 1)?;
                self.pos = p;
                return self.next_audio(read);
            }
        };
        let h = h?;
        let pts = micros(h.first_sample(self.si.max_block), h.rate);
        let data = d[..len].to_vec();
        self.pos += len as u64;
        Some(Packet { pts: Some(pts), data, key: true })
    }
    fn seek_video(&mut self, _read: &mut Read, us: i64) -> i64 {
        us
    }
    fn seek_audio(&mut self, read: &mut Read, us: i64) {
        let target = us.max(0) as u64 * self.si.rate as u64 / 1_000_000;
        if target == 0 {
            self.pos = self.first;
            return;
        }
        // The seek table's last point at or before the target, else a guess by position;
        // then step back by halves until a frame starts at or before the target.
        let (mut lo, mut hi) = (self.first, self.size);
        if let Some(&(_, off)) = self.seek.iter().rev().find(|(s, _)| *s <= target) {
            lo = self.first + off;
        }
        if let Some(&(_, off)) = self.seek.iter().find(|(s, _)| *s > target) {
            hi = self.first + off;
        }
        // Bisect on frame positions.
        for _ in 0..24 {
            if hi <= lo + 64 * 1024 {
                break;
            }
            let mid = lo + (hi - lo) / 2;
            match self.frame_at(read, mid) {
                Some((p, s)) if s <= target => lo = p,
                Some((p, _)) => hi = p,
                None => hi = mid,
            }
        }
        self.pos = self.frame_at(read, lo).map(|f| f.0).unwrap_or(lo);
    }
}

// ---- Ogg ---------------------------------------------------------------------------------------

/// One Ogg page: its header fields, the packet pieces (lacing) and where it ends.
struct Page {
    flags: u8,
    granule: i64,
    serial: u32,
    /// Segment lengths, and the data.
    lacing: Vec<u8>,
    data: Vec<u8>,
    len: u64,
}

fn read_page(read: &mut Read, pos: u64) -> Option<Page> {
    let h = read(pos, 27)?;
    if h.len() < 27 || &h[0..4] != b"OggS" {
        return None;
    }
    let n = h[26] as usize;
    let lacing = read(pos + 27, n)?;
    if lacing.len() < n {
        return None;
    }
    let body: usize = lacing.iter().map(|&l| l as usize).sum();
    let data = read(pos + 27 + n as u64, body)?;
    if data.len() < body {
        return None;
    }
    Some(Page {
        flags: h[5],
        granule: i64::from_le_bytes(h[6..14].try_into().ok()?),
        serial: u32::from_le_bytes(h[14..18].try_into().ok()?),
        lacing,
        data,
        len: 27 + n as u64 + body as u64,
    })
}

/// The next page start at or after `pos`.
fn find_page(read: &mut Read, pos: u64, size: u64) -> Option<u64> {
    let mut p = pos;
    while p < size {
        let d = read(p, 64 * 1024)?;
        if d.len() < 4 {
            return None;
        }
        if let Some(i) = d.windows(4).position(|w| w == b"OggS") {
            return Some(p + i as u64);
        }
        p += d.len() as u64 - 3;
    }
    None
}

#[derive(Clone, Copy, PartialEq)]
enum OggKind {
    Vorbis,
    Opus,
    Flac,
}

struct OggFile {
    info: Info,
    kind: OggKind,
    serial: u32,
    size: u64,
    /// Where the sound pages start (after the headers).
    data_start: u64,
    rate: u32,
    /// Samples to take off the granule positions (Opus's pre-skip).
    preskip: i64,
    pos: u64,
    /// Packets taken from pages but not handed out yet, and a packet being put together
    /// from pieces across pages.
    queue: alloc::collections::VecDeque<Packet>,
    partial: Vec<u8>,
    /// The granule position at the end of the last page read (time of the next packet).
    last_granule: Option<i64>,
}

impl OggFile {
    fn open(read: &mut Read, size: u64) -> Result<OggFile, String> {
        let mut pos = 0;
        let mut kind = None;
        let mut serial = 0;
        let mut headers: Vec<Vec<u8>> = Vec::new();
        let mut partial = Vec::new();
        let mut rate = 48000;
        let mut channels = 2;
        let mut preskip = 0;
        // Read pages until the chosen stream's headers are complete.
        for _ in 0..200 {
            let Some(pg) = read_page(read, pos) else { break };
            pos += pg.len;
            if kind.is_none() && pg.flags & 2 != 0 {
                let d = &pg.data;
                let k = if d.starts_with(b"\x01vorbis") {
                    Some(OggKind::Vorbis)
                } else if d.starts_with(b"OpusHead") {
                    Some(OggKind::Opus)
                } else if d.starts_with(b"\x7FFLAC") {
                    Some(OggKind::Flac)
                } else {
                    None
                };
                if let Some(k) = k {
                    kind = Some(k);
                    serial = pg.serial;
                }
            }
            let Some(k) = kind else { continue };
            if pg.serial != serial {
                continue;
            }
            let mut off = 0;
            for &l in &pg.lacing {
                partial.extend_from_slice(&pg.data[off..off + l as usize]);
                off += l as usize;
                if l < 255 {
                    headers.push(core::mem::take(&mut partial));
                }
            }
            let need = match k {
                OggKind::Vorbis => 3,
                OggKind::Opus => 2,
                OggKind::Flac => {
                    // The first packet says how many header packets follow.
                    headers.first().filter(|h| h.len() >= 9).map(|h| 1 + u16::from_be_bytes([h[7], h[8]]) as usize).unwrap_or(2)
                }
            };
            if headers.len() >= need {
                break;
            }
        }
        let kind = kind.ok_or("This Ogg file has no sound Solon can play")?;
        let codec = match kind {
            OggKind::Vorbis => {
                let h = headers.first().ok_or("This Ogg file can't be read")?;
                if h.len() >= 16 {
                    channels = h[11] as u16;
                    rate = u32::from_le_bytes([h[12], h[13], h[14], h[15]]);
                }
                // The three headers, laced the Xiph way (as Matroska stores them).
                let mut blob = vec![2u8];
                for h in headers.iter().take(2) {
                    let mut n = h.len();
                    while n >= 255 {
                        blob.push(255);
                        n -= 255;
                    }
                    blob.push(n as u8);
                }
                for h in headers.iter().take(3) {
                    blob.extend_from_slice(h);
                }
                ACodec::Vorbis { headers: blob }
            }
            OggKind::Opus => {
                let h = headers.first().ok_or("This Ogg file can't be read")?;
                if h.len() >= 12 {
                    channels = h[9] as u16;
                    preskip = u16::from_le_bytes([h[10], h[11]]) as i64;
                }
                rate = 48000;
                ACodec::Opus { head: h.clone() }
            }
            OggKind::Flac => {
                let h = headers.first().ok_or("This Ogg file can't be read")?;
                // 0x7F "FLAC" major minor count(2) "fLaC" STREAMINFO block (4-byte header + 34)
                let info = h.get(17..17 + 34).ok_or("This Ogg FLAC file can't be read")?.to_vec();
                let si = StreamInfo::parse(&info).ok_or("This Ogg FLAC file can't be read")?;
                rate = si.rate;
                channels = si.channels as u16;
                ACodec::Flac { info }
            }
        };
        // The last page's granule position: how long it is.
        let mut duration_ms = 0;
        let mut back = size.saturating_sub(64 * 1024);
        loop {
            let mut p = back;
            let mut last = None;
            while let Some(q) = find_page(read, p, size) {
                if let Some(pg) = read_page(read, q) {
                    if pg.serial == serial && pg.granule > 0 {
                        last = Some(pg.granule);
                    }
                    p = q + pg.len;
                } else {
                    p = q + 4;
                }
            }
            if let Some(g) = last {
                duration_ms = ((g - preskip).max(0) as u64) * 1000 / rate.max(1) as u64;
                break;
            }
            if back == 0 {
                break;
            }
            back = back.saturating_sub(256 * 1024);
        }
        let info = Info { duration_ms, video: None, audio: Some(AudioInfo { codec, rate, channels: channels.max(1) }) };
        Ok(OggFile { info, kind, serial, size, data_start: pos, rate, preskip, pos, queue: Default::default(), partial: Vec::new(), last_granule: Some(0) })
    }

    /// The granule position of the first page of our stream at or after `pos`, and where it is.
    fn granule_at(&self, read: &mut Read, pos: u64) -> Option<(u64, i64)> {
        let mut p = pos;
        for _ in 0..64 {
            let q = find_page(read, p, self.size)?;
            match read_page(read, q) {
                Some(pg) if pg.serial == self.serial && pg.granule >= 0 => return Some((q, pg.granule)),
                Some(pg) => p = q + pg.len,
                None => p = q + 4,
            }
        }
        None
    }
}

impl Demux for OggFile {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next_video(&mut self, _read: &mut Read) -> Option<Packet> {
        None
    }
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet> {
        while self.queue.is_empty() {
            let pg = loop {
                if self.pos >= self.size {
                    return None;
                }
                match read_page(read, self.pos) {
                    Some(pg) => {
                        self.pos += pg.len;
                        if pg.serial == self.serial {
                            break pg;
                        }
                    }
                    None => self.pos = find_page(read, self.pos + 1, self.size)?,
                }
            };
            // A page starting mid-packet right after a jump: drop the piece.
            if pg.flags & 1 != 0 && self.last_granule.is_none() {
                self.partial.clear();
            }
            let mut off = 0;
            let mut first = true;
            for &l in &pg.lacing {
                self.partial.extend_from_slice(&pg.data[off..off + l as usize]);
                off += l as usize;
                if l < 255 {
                    let data = core::mem::take(&mut self.partial);
                    // The first packet that ends on this page starts where the last page ended.
                    let pts = if first { self.last_granule.map(|g| micros((g - self.preskip).max(0) as u64, self.rate)) } else { None };
                    first = false;
                    if !data.is_empty() {
                        self.queue.push_back(Packet { pts, data, key: true });
                    }
                }
            }
            if pg.granule >= 0 {
                self.last_granule = Some(pg.granule);
            }
        }
        self.queue.pop_front()
    }
    fn seek_video(&mut self, _read: &mut Read, us: i64) -> i64 {
        us
    }
    fn seek_audio(&mut self, read: &mut Read, us: i64) {
        self.queue.clear();
        self.partial.clear();
        let target = us.max(0) as i64 * self.rate as i64 / 1_000_000 + self.preskip;
        if us <= 0 {
            self.pos = self.data_start;
            self.last_granule = Some(0);
            return;
        }
        // Bisect for the last page ending before the target; play from the page after it.
        let (mut lo, mut hi) = (self.data_start, self.size);
        for _ in 0..40 {
            if hi <= lo + 8 * 1024 {
                break;
            }
            let mid = lo + (hi - lo) / 2;
            match self.granule_at(read, mid) {
                Some((p, g)) if g < target => lo = p,
                Some((p, _)) => hi = p.max(lo + 1),
                None => hi = mid,
            }
        }
        match self.granule_at(read, lo) {
            Some((p, g)) if g < target && p > self.data_start => {
                // Start after this page: its granule is the next packet's time.
                let len = read_page(read, p).map(|pg| pg.len).unwrap_or(0);
                self.pos = p + len;
                self.last_granule = Some(g);
            }
            _ => {
                self.pos = self.data_start;
                self.last_granule = Some(0);
            }
        }
        let _ = self.kind;
    }
}

// ---- WAV and AIFF --------------------------------------------------------------------------

struct PcmFile {
    info: Info,
    data: u64,
    data_len: u64,
    /// Bytes per frame (all channels).
    frame: u64,
    rate: u32,
    pos: u64,
}

impl PcmFile {
    fn new(codec: ACodec, rate: u32, channels: u16, bits: u16, data: u64, data_len: u64) -> Result<PcmFile, String> {
        if rate == 0 || channels == 0 || bits == 0 {
            return Err(String::from("This sound file can't be read"));
        }
        let frame = (bits as u64).div_ceil(8) * channels as u64;
        let duration_ms = data_len / frame * 1000 / rate as u64;
        let info = Info { duration_ms, video: None, audio: Some(AudioInfo { codec, rate, channels }) };
        Ok(PcmFile { info, data, data_len, frame, rate, pos: 0 })
    }

    fn open_wav(read: &mut Read, size: u64) -> Result<PcmFile, String> {
        let mut pos = 12;
        let mut fmt: Option<Vec<u8>> = None;
        while pos + 8 <= size {
            let h = read(pos, 8).ok_or("Can't read the file")?;
            let id = &h[0..4];
            let len = u32::from_le_bytes([h[4], h[5], h[6], h[7]]) as u64;
            if id == b"fmt " {
                fmt = read(pos + 8, len.min(64) as usize);
            } else if id == b"data" {
                let f = fmt.ok_or("This sound file can't be read")?;
                if f.len() < 16 {
                    return Err(String::from("This sound file can't be read"));
                }
                let mut tag = u16::from_le_bytes([f[0], f[1]]);
                let channels = u16::from_le_bytes([f[2], f[3]]);
                let rate = u32::from_le_bytes([f[4], f[5], f[6], f[7]]);
                let bits = u16::from_le_bytes([f[14], f[15]]);
                if tag == 0xFFFE && f.len() >= 26 {
                    tag = u16::from_le_bytes([f[24], f[25]]); // the sub-format GUID's first part
                }
                let len = len.min(size - (pos + 8));
                let codec = match (tag, bits) {
                    (1, 8 | 16 | 24 | 32) => ACodec::Pcm { bits, big_endian: false, float: false },
                    (3, 32) => ACodec::Pcm { bits, big_endian: false, float: true },
                    (0x55, _) => return MpaInWav::err(),
                    (2 | 0x11, _) => return Err(String::from("This WAV file is ADPCM, which Solon can't play yet")),
                    _ => return Err(format!("This WAV file's sound (format {}) can't be played yet", tag)),
                };
                return PcmFile::new(codec, rate, channels, bits, pos + 8, len);
            }
            pos += 8 + len + (len & 1);
        }
        Err(String::from("This sound file has no sound in it"))
    }

    fn open_aiff(read: &mut Read, size: u64) -> Result<PcmFile, String> {
        let h = read(0, 12).ok_or("Can't read the file")?;
        let aifc = &h[8..12] == b"AIFC";
        let mut pos = 12;
        let mut comm: Option<Vec<u8>> = None;
        while pos + 8 <= size {
            let h = read(pos, 8).ok_or("Can't read the file")?;
            let len = u32::from_be_bytes([h[4], h[5], h[6], h[7]]) as u64;
            match &h[0..4] {
                b"COMM" => comm = read(pos + 8, len.min(64) as usize),
                b"SSND" => {
                    let c = comm.ok_or("This sound file can't be read")?;
                    if c.len() < 18 {
                        return Err(String::from("This sound file can't be read"));
                    }
                    let channels = u16::from_be_bytes([c[0], c[1]]);
                    let bits = u16::from_be_bytes([c[6], c[7]]);
                    // The rate is an 80-bit extended float.
                    let exp = (u16::from_be_bytes([c[8], c[9]]) & 0x7FFF) as i32 - 16383;
                    let mant = u64::from_be_bytes(c[10..18].try_into().unwrap());
                    let rate = if (0..64).contains(&exp) { (mant >> (63 - exp)) as u32 } else { 0 };
                    let kind = if aifc && c.len() >= 22 { [c[18], c[19], c[20], c[21]] } else { *b"NONE" };
                    let codec = match &kind {
                        b"NONE" | b"twos" => ACodec::Pcm { bits, big_endian: true, float: false },
                        b"sowt" => ACodec::Pcm { bits, big_endian: false, float: false },
                        b"fl32" | b"FL32" => ACodec::Pcm { bits: 32, big_endian: true, float: true },
                        _ => return Err(format!("This AIFF file's sound ({}) can't be played yet", String::from_utf8_lossy(&kind))),
                    };
                    let off = read(pos + 8, 4).map(|o| u32::from_be_bytes([o[0], o[1], o[2], o[3]]) as u64).unwrap_or(0);
                    let start = pos + 16 + off;
                    let len = (len.saturating_sub(8 + off)).min(size.saturating_sub(start));
                    return PcmFile::new(codec, rate, channels, bits, start, len);
                }
                _ => {}
            }
            pos += 8 + len + (len & 1);
        }
        Err(String::from("This sound file has no sound in it"))
    }
}

/// MP3 inside a WAV file (rare): not handled yet.
struct MpaInWav;
impl MpaInWav {
    fn err() -> Result<PcmFile, String> {
        Err(String::from("This WAV file holds MP3 sound, which Solon can't play from a WAV file yet"))
    }
}

impl Demux for PcmFile {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next_video(&mut self, _read: &mut Read) -> Option<Packet> {
        None
    }
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet> {
        if self.pos >= self.data_len {
            return None;
        }
        // About a tenth of a second at a time, in whole frames.
        let n = ((self.rate as u64 / 10).max(1) * self.frame).min(self.data_len - self.pos);
        let n = n - n % self.frame;
        if n == 0 {
            return None;
        }
        let data = read(self.data + self.pos, n as usize)?;
        let pts = micros(self.pos / self.frame, self.rate);
        self.pos += n;
        Some(Packet { pts: Some(pts), data, key: true })
    }
    fn seek_video(&mut self, _read: &mut Read, us: i64) -> i64 {
        us
    }
    fn seek_audio(&mut self, _read: &mut Read, us: i64) {
        let f = us.max(0) as u64 * self.rate as u64 / 1_000_000;
        self.pos = (f * self.frame).min(self.data_len);
    }
}

// ---- AMR ---------------------------------------------------------------------------------------

struct AmrFile {
    info: Info,
    wide: bool,
    first: u64,
    size: u64,
    pos: u64,
    frame_no: u64,
}

const AMR_NB: [u8; 16] = [12, 13, 15, 17, 19, 20, 26, 31, 5, 0, 0, 0, 0, 0, 0, 0];
const AMR_WB: [u8; 16] = [17, 23, 32, 36, 40, 46, 50, 58, 60, 5, 0, 0, 0, 0, 0, 0];

impl AmrFile {
    fn open(read: &mut Read, size: u64) -> Result<AmrFile, String> {
        let h = read(0, 16).ok_or("Can't read the file")?;
        let (wide, first) = if h.starts_with(b"#!AMR-WB\n") {
            (true, 9)
        } else if h.starts_with(b"#!AMR\n") {
            (false, 6)
        } else {
            return Err(String::from("This AMR file has more than one channel, which Solon can't play yet"));
        };
        // Frames are 20 ms; their sizes vary with the mode, so count from an average.
        let d = read(first, 4096).unwrap_or_default();
        let (mut p, mut n) = (0usize, 0u64);
        while p < d.len() {
            let ft = ((d[p] >> 3) & 15) as usize;
            p += 1 + if wide { AMR_WB[ft] } else { AMR_NB[ft] } as usize;
            n += 1;
        }
        let per = if n > 0 { (p as u64 / n).max(1) } else { 32 };
        let duration_ms = size.saturating_sub(first) / per * 20;
        let rate = if wide { 16000 } else { 8000 };
        let info = Info { duration_ms, video: None, audio: Some(AudioInfo { codec: ACodec::Amr { wide }, rate, channels: 1 }) };
        Ok(AmrFile { info, wide, first, size, pos: first, frame_no: 0 })
    }
}

impl Demux for AmrFile {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next_video(&mut self, _read: &mut Read) -> Option<Packet> {
        None
    }
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet> {
        // Ten frames (200 ms) per packet.
        let d = read(self.pos, 10 * 64)?;
        let mut p = 0;
        let mut n = 0;
        while n < 10 && p < d.len() {
            let ft = ((d[p] >> 3) & 15) as usize;
            let len = 1 + if self.wide { AMR_WB[ft] } else { AMR_NB[ft] } as usize;
            if p + len > d.len() {
                break;
            }
            p += len;
            n += 1;
        }
        if p == 0 || self.pos >= self.size {
            return None;
        }
        let pts = self.frame_no as i64 * 20_000;
        self.pos += p as u64;
        self.frame_no += n;
        Some(Packet { pts: Some(pts), data: d[..p].to_vec(), key: true })
    }
    fn seek_video(&mut self, _read: &mut Read, us: i64) -> i64 {
        us
    }
    fn seek_audio(&mut self, read: &mut Read, us: i64) {
        // Walk the frames from the start (they're small; files are short).
        let want = us.max(0) as u64 / 20_000;
        self.pos = self.first;
        self.frame_no = 0;
        while self.frame_no < want {
            let Some(d) = read(self.pos, 1) else { break };
            let Some(&b) = d.first() else { break };
            let ft = ((b >> 3) & 15) as usize;
            self.pos += 1 + if self.wide { AMR_WB[ft] } else { AMR_NB[ft] } as u64;
            self.frame_no += 1;
            if self.pos >= self.size {
                break;
            }
        }
    }
}

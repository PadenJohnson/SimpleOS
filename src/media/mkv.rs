//! Matroska (.mkv) and WebM (.webm): EBML elements, clusters of blocks, and the Cues index
//! for jumping around. Picture and sound read the same clusters; the last few parsed
//! clusters are kept so they don't have to be read twice.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use super::{ACodec, AudioInfo, Demux, Info, Packet, Read, VCodec, VideoInfo};

const SEGMENT: u32 = 0x1853_8067;
const CLUSTER: u32 = 0x1F43_B675;
const INFO: u32 = 0x1549_A966;
const TRACKS: u32 = 0x1654_AE6B;
const CUES: u32 = 0x1C53_BB6B;
const SEEKHEAD: u32 = 0x114D_9B74;
/// Top-level (segment child) elements: a cluster of unknown size ends at one of these.
const LEVEL1: [u32; 8] = [CLUSTER, INFO, TRACKS, CUES, SEEKHEAD, 0x1254_C367, 0x1043_A770, 0x1941_A469];
const UNKNOWN: u64 = u64::MAX;

/// An element ID (with its marker bits) at `p`.
fn read_id(b: &[u8], p: &mut usize) -> Option<u32> {
    let first = *b.get(*p)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 4 || *p + len > b.len() {
        return None;
    }
    let mut v = 0u32;
    for i in 0..len {
        v = (v << 8) | b[*p + i] as u32;
    }
    *p += len;
    Some(v)
}

/// A size (variable-length integer without its marker); all ones = unknown.
fn read_size(b: &[u8], p: &mut usize) -> Option<u64> {
    let first = *b.get(*p)?;
    let len = first.leading_zeros() as usize + 1;
    if len > 8 || *p + len > b.len() {
        return None;
    }
    let mut v = (first as u64) & (0xFF >> len);
    let mut all_ones = v == (0xFF >> len) as u64;
    for i in 1..len {
        v = (v << 8) | b[*p + i] as u64;
        all_ones &= b[*p + i] == 0xFF;
    }
    *p += len;
    Some(if all_ones { UNKNOWN } else { v })
}

fn uint(d: &[u8]) -> u64 {
    d.iter().fold(0, |a, &b| (a << 8) | b as u64)
}

fn float(d: &[u8]) -> f64 {
    match d.len() {
        4 => f32::from_bits(uint(d) as u32) as f64,
        8 => f64::from_bits(uint(d)),
        _ => 0.0,
    }
}

/// The child elements of `d`: (id, body).
fn children(d: &[u8]) -> Vec<(u32, &[u8])> {
    let mut out = Vec::new();
    let mut p = 0;
    while p < d.len() {
        let Some(id) = read_id(d, &mut p) else { break };
        let Some(size) = read_size(d, &mut p) else { break };
        let end = if size == UNKNOWN { d.len() } else { (p + size as usize).min(d.len()) };
        out.push((id, &d[p..end]));
        p = end;
    }
    out
}

fn child<'a>(d: &'a [u8], id: u32) -> Option<&'a [u8]> {
    children(d).into_iter().find(|c| c.0 == id).map(|c| c.1)
}

struct Track {
    number: u64,
    /// Bytes to put back at the start of every frame ("header stripping").
    prefix: Vec<u8>,
}

/// A parsed cluster: its packets for the picture track and for the sound track.
struct Parsed {
    pos: u64,
    next: u64,
    video: Vec<Packet>,
    audio: Vec<Packet>,
}

struct Cursor {
    /// File offset of the next cluster to read.
    pos: u64,
    queue: VecDeque<Packet>,
    /// After a jump: drop packets before this time (µs).
    skip_before: i64,
}

pub struct MkvDemux {
    info: Info,
    vtrack: Option<Track>,
    atrack: Option<Track>,
    /// Nanoseconds per timestamp unit.
    scale: u64,
    /// Where the segment's data starts and ends, and the first cluster.
    seg_start: u64,
    seg_end: u64,
    first_cluster: u64,
    /// (time µs, cluster position) from Cues, for the picture track (or any track).
    cues: Vec<(i64, u64)>,
    parsed: VecDeque<Parsed>,
    v: Cursor,
    a: Cursor,
}

impl MkvDemux {
    pub fn open(read: &mut Read, size: u64) -> Result<MkvDemux, String> {
        // EBML header, then the Segment.
        let head = read(0, 4096).ok_or("Can't read the file")?;
        let mut p = 0;
        let _ = read_id(&head, &mut p);
        let hsize = read_size(&head, &mut p).ok_or("Not a Matroska file")?;
        p += hsize as usize;
        let mut q = p;
        if read_id(&head, &mut q) != Some(SEGMENT) {
            return Err(String::from("Not a Matroska file"));
        }
        let seg_size = read_size(&head, &mut q).ok_or("Not a Matroska file")?;
        let seg_start = q as u64;
        let seg_end = if seg_size == UNKNOWN { size } else { (seg_start + seg_size).min(size) };
        // Walk the segment's top level until the first cluster (and find Cues via SeekHead).
        let mut pos = seg_start;
        let (mut info_el, mut tracks_el, mut cues_pos) = (None, None, None);
        let mut first_cluster = 0;
        let mut guard = 0;
        while pos < seg_end && guard < 10_000 {
            guard += 1;
            let h = read(pos, 16).ok_or("Can't read the file")?;
            let mut hp = 0;
            let Some(id) = read_id(&h, &mut hp) else { break };
            let Some(sz) = read_size(&h, &mut hp) else { break };
            let body = pos + hp as u64;
            match id {
                CLUSTER => {
                    first_cluster = pos;
                    if tracks_el.is_some() {
                        break;
                    }
                }
                INFO => info_el = read(body, sz.min(1 << 20) as usize),
                TRACKS => tracks_el = read(body, sz.min(4 << 20) as usize),
                CUES => cues_pos = Some(pos),
                SEEKHEAD => {
                    if let Some(sh) = read(body, sz.min(1 << 20) as usize) {
                        for (sid, seek) in children(&sh) {
                            if sid != 0x4DBB {
                                continue;
                            }
                            let target = child(seek, 0x53AB).map(uint);
                            let at = child(seek, 0x53AC).map(uint);
                            if let (Some(t), Some(a)) = (target, at) {
                                if t == CUES as u64 {
                                    cues_pos = Some(seg_start + a);
                                }
                            }
                        }
                    }
                }
                _ => {}
            }
            if sz == UNKNOWN {
                break;
            }
            pos = body + sz;
        }
        let tracks_el = tracks_el.ok_or("This file has no list of tracks")?;
        let scale = info_el.as_deref().and_then(|i| child(i, 0x2AD7B1)).map(uint).filter(|&s| s > 0).unwrap_or(1_000_000);
        let duration = info_el.as_deref().and_then(|i| child(i, 0x4489)).map(float).unwrap_or(0.0);
        let duration_ms = (duration * scale as f64 / 1_000_000.0) as u64;
        let (mut video, mut audio, mut vtrack, mut atrack) = (None, None, None, None);
        for (id, t) in children(&tracks_el) {
            if id != 0xAE {
                continue;
            }
            let number = child(t, 0xD7).map(uint).unwrap_or(0);
            let kind = child(t, 0x83).map(uint).unwrap_or(0);
            let codec = child(t, 0x86).map(|c| String::from_utf8_lossy(c).trim_end_matches('\0').into()).unwrap_or_else(String::new);
            let private = child(t, 0x63A2).unwrap_or(&[]).to_vec();
            let prefix = header_strip(t);
            if kind == 1 && video.is_none() {
                let v = child(t, 0xE0).unwrap_or(&[]);
                let w = child(v, 0xB0).map(uint).unwrap_or(0) as u32;
                let h = child(v, 0xBA).map(uint).unwrap_or(0) as u32;
                video = Some(VideoInfo { codec: video_codec(&codec, &private), width: w, height: h, rotation: 0 });
                vtrack = Some(Track { number, prefix });
            } else if kind == 2 && audio.is_none() {
                let a = child(t, 0xE1).unwrap_or(&[]);
                let rate = child(a, 0xB5).map(float).unwrap_or(8000.0) as u32;
                let channels = child(a, 0x9F).map(uint).unwrap_or(1) as u16;
                let bits = child(a, 0x6264).map(uint).unwrap_or(16) as u16;
                audio = Some(AudioInfo { codec: audio_codec(&codec, &private, rate, channels, bits), rate, channels });
                atrack = Some(Track { number, prefix });
            }
        }
        if video.is_none() && audio.is_none() {
            return Err(String::from("This file has no pictures or sound"));
        }
        let mut d = MkvDemux {
            info: Info { duration_ms, video, audio },
            vtrack,
            atrack,
            scale,
            seg_start,
            seg_end,
            first_cluster: if first_cluster == 0 { pos } else { first_cluster },
            cues: Vec::new(),
            parsed: VecDeque::new(),
            v: Cursor { pos: 0, queue: VecDeque::new(), skip_before: i64::MIN },
            a: Cursor { pos: 0, queue: VecDeque::new(), skip_before: i64::MIN },
        };
        d.v.pos = d.first_cluster;
        d.a.pos = d.first_cluster;
        if let Some(cp) = cues_pos {
            d.read_cues(read, cp);
        }
        Ok(d)
    }

    fn read_cues(&mut self, read: &mut Read, pos: u64) {
        let Some(h) = read(pos, 16) else { return };
        let mut hp = 0;
        if read_id(&h, &mut hp) != Some(CUES) {
            return;
        }
        let Some(sz) = read_size(&h, &mut hp) else { return };
        let Some(c) = read(pos + hp as u64, sz.min(32 << 20) as usize) else { return };
        let vnum = self.vtrack.as_ref().map(|t| t.number);
        for (id, cp) in children(&c) {
            if id != 0xBB {
                continue;
            }
            let Some(time) = child(cp, 0xB3).map(uint) else { continue };
            for (tid, tp) in children(cp) {
                if tid != 0xB7 {
                    continue;
                }
                let track = child(tp, 0xF7).map(uint);
                if vnum.is_some() && track != vnum {
                    continue;
                }
                if let Some(cl) = child(tp, 0xF1).map(uint) {
                    self.cues.push(((time * self.scale / 1000) as i64, self.seg_start + cl));
                }
            }
        }
        self.cues.sort();
        self.cues.dedup_by_key(|c| c.1);
    }

    /// Read and parse the cluster at `pos` (kept for the other cursor).
    fn cluster(&mut self, read: &mut Read, pos: u64) -> Option<usize> {
        if let Some(k) = self.parsed.iter().position(|c| c.pos == pos) {
            return Some(k);
        }
        let mut pos = pos;
        // Find a cluster at or after `pos` (skipping Cues, tags and anything else).
        let mut guard = 0;
        let (body, size) = loop {
            guard += 1;
            if pos >= self.seg_end || guard > 1000 {
                return None;
            }
            let h = read(pos, 16)?;
            let mut hp = 0;
            let id = read_id(&h, &mut hp)?;
            let sz = read_size(&h, &mut hp)?;
            if id == CLUSTER {
                break (pos + hp as u64, sz);
            }
            if sz == UNKNOWN {
                return None;
            }
            pos += hp as u64 + sz;
        };
        let data = if size == UNKNOWN {
            // Live recordings: the cluster runs until the next top-level element.
            let d = read(body, (self.seg_end - body).min(16 << 20) as usize)?;
            let mut p = 0;
            let mut end = d.len();
            while p < d.len() {
                let at = p;
                let Some(id) = read_id(&d, &mut p) else { break };
                if LEVEL1.contains(&id) {
                    end = at;
                    break;
                }
                let Some(sz) = read_size(&d, &mut p) else { break };
                if sz == UNKNOWN {
                    continue; // step inside (BlockGroup of unknown size)
                }
                p += sz as usize;
            }
            let mut d = d;
            d.truncate(end);
            d
        } else {
            read(body, size.min(64 << 20) as usize)?
        };
        let next = body + data.len() as u64;
        let mut parsed = Parsed { pos, next, video: Vec::new(), audio: Vec::new() };
        let mut ctime = 0u64;
        let vnum = self.vtrack.as_ref().map(|t| t.number);
        let anum = self.atrack.as_ref().map(|t| t.number);
        for (id, el) in children(&data) {
            match id {
                0xE7 => ctime = uint(el),
                0xA3 => self.block(el, ctime, None, vnum, anum, &mut parsed),
                0xA0 => {
                    // BlockGroup: the Block, and ReferenceBlock if it isn't a key frame.
                    let kids = children(el);
                    let key = !kids.iter().any(|k| k.0 == 0xFB);
                    if let Some(b) = kids.iter().find(|k| k.0 == 0xA1) {
                        self.block(b.1, ctime, Some(key), vnum, anum, &mut parsed);
                    }
                }
                _ => {}
            }
        }
        if self.parsed.len() >= 3 {
            self.parsed.pop_front();
        }
        self.parsed.push_back(parsed);
        Some(self.parsed.len() - 1)
    }

    /// One (Simple)Block: the track's frames (split if laced) with their time. `group_key`:
    /// for a Block in a BlockGroup, whether it's a key frame (SimpleBlocks have a flag).
    fn block(&self, b: &[u8], ctime: u64, group_key: Option<bool>, vnum: Option<u64>, anum: Option<u64>, out: &mut Parsed) {
        let mut p = 0;
        let Some(track) = read_size(b, &mut p) else { return };
        if p + 3 > b.len() {
            return;
        }
        let rel = i16::from_be_bytes([b[p], b[p + 1]]) as i64;
        let flags = b[p + 2];
        p += 3;
        let is_video = Some(track) == vnum;
        if !is_video && Some(track) != anum {
            return;
        }
        let key = group_key.unwrap_or(flags & 0x80 != 0);
        let t = (ctime as i64 + rel) * self.scale as i64 / 1000;
        let prefix = if is_video { &self.vtrack.as_ref().unwrap().prefix } else { &self.atrack.as_ref().unwrap().prefix };
        let frames = laced(&b[p..], (flags >> 1) & 3);
        let n = frames.len().max(1) as i64;
        // Laced sound frames share one time; spread them using the sound's frame duration.
        let dur = if !is_video { self.info.audio.as_ref().map(|a| frame_us(&a.codec, a.rate)).unwrap_or(0) } else { 0 };
        for (k, f) in frames.into_iter().enumerate() {
            let mut data = Vec::with_capacity(prefix.len() + f.len());
            data.extend_from_slice(prefix);
            data.extend_from_slice(f);
            let pkt = Packet { pts: Some(t + k as i64 * dur), data, key: key || !is_video };
            if is_video {
                out.video.push(pkt);
            } else {
                out.audio.push(pkt);
            }
        }
        let _ = n;
    }

    fn next(&mut self, read: &mut Read, video: bool) -> Option<Packet> {
        loop {
            let cur = if video { &mut self.v } else { &mut self.a };
            while let Some(p) = cur.queue.pop_front() {
                if p.pts.unwrap_or(0) >= cur.skip_before {
                    cur.skip_before = i64::MIN;
                    return Some(p);
                }
            }
            let pos = cur.pos;
            let k = self.cluster(read, pos)?;
            let c = &mut self.parsed[k];
            // Each cursor takes its own half; the other half stays for the other cursor.
            let packets: Vec<Packet> = core::mem::take(if video { &mut c.video } else { &mut c.audio });
            let next = c.next;
            let cur = if video { &mut self.v } else { &mut self.a };
            cur.queue.extend(packets);
            cur.pos = next;
        }
    }

    /// The cluster to start from for time `us`: from the Cues, or by walking the clusters.
    fn cluster_for(&mut self, read: &mut Read, us: i64) -> (u64, i64) {
        if !self.cues.is_empty() {
            let k = self.cues.partition_point(|c| c.0 <= us).saturating_sub(1);
            return (self.cues[k].1, self.cues[k].0);
        }
        // No index: walk cluster headers (each only its timestamp) from the start.
        let mut pos = self.first_cluster;
        let mut best = (pos, 0i64);
        let mut guard = 0;
        while pos < self.seg_end && guard < 100_000 {
            guard += 1;
            let Some(h) = read(pos, 32) else { break };
            let mut hp = 0;
            let (Some(id), Some(sz)) = (read_id(&h, &mut hp), read_size(&h, &mut hp)) else { break };
            if id == CLUSTER {
                let mut q = hp;
                if read_id(&h, &mut q) == Some(0xE7) {
                    if let Some(tl) = read_size(&h, &mut q) {
                        let t = uint(h.get(q..q + tl as usize).unwrap_or(&[])) as i64 * self.scale as i64 / 1000;
                        if t > us {
                            break;
                        }
                        best = (pos, t);
                    }
                }
            }
            if sz == UNKNOWN {
                break;
            }
            pos += hp as u64 + sz;
        }
        best
    }
}

impl Demux for MkvDemux {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next_video(&mut self, read: &mut Read) -> Option<Packet> {
        self.vtrack.as_ref()?;
        self.next(read, true)
    }
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet> {
        self.atrack.as_ref()?;
        self.next(read, false)
    }
    fn seek_video(&mut self, read: &mut Read, us: i64) -> i64 {
        let (pos, t) = self.cluster_for(read, us);
        self.parsed.clear();
        self.v = Cursor { pos, queue: VecDeque::new(), skip_before: i64::MIN };
        t
    }
    fn seek_audio(&mut self, read: &mut Read, us: i64) {
        let (pos, _) = self.cluster_for(read, us);
        self.parsed.clear(); // its sound half may have been used already
        self.a = Cursor { pos, queue: VecDeque::new(), skip_before: us - 100_000 };
    }
    fn key_before(&self, us: i64) -> Option<i64> {
        if self.cues.is_empty() {
            return None;
        }
        let k = self.cues.partition_point(|c| c.0 <= us).saturating_sub(1);
        Some(self.cues[k].0)
    }
}

/// Frames in a laced block (0 none, 1 Xiph, 2 fixed size, 3 EBML).
fn laced(d: &[u8], lacing: u8) -> Vec<&[u8]> {
    if lacing == 0 || d.is_empty() {
        return alloc::vec![d];
    }
    let n = d[0] as usize + 1;
    let mut p = 1;
    let mut sizes = Vec::with_capacity(n);
    match lacing {
        1 => {
            for _ in 0..n - 1 {
                let mut s = 0;
                loop {
                    let Some(&b) = d.get(p) else { return Vec::new() };
                    p += 1;
                    s += b as usize;
                    if b != 255 {
                        break;
                    }
                }
                sizes.push(s);
            }
        }
        3 => {
            let Some(first) = read_size(d, &mut p) else { return Vec::new() };
            sizes.push(first as usize);
            for _ in 1..n - 1 {
                let at = p;
                let Some(raw) = read_size(d, &mut p) else { return Vec::new() };
                // Signed: subtract the middle of the range for this length.
                let len = p - at;
                let bias = (1i64 << (7 * len - 1)) - 1;
                let s = *sizes.last().unwrap() as i64 + raw as i64 - bias;
                sizes.push(s.max(0) as usize);
            }
        }
        _ => {
            let each = (d.len() - 1) / n;
            sizes = alloc::vec![each; n - 1];
        }
    }
    let mut out = Vec::with_capacity(n);
    for s in sizes {
        if p + s > d.len() {
            return out;
        }
        out.push(&d[p..p + s]);
        p += s;
    }
    out.push(&d[p.min(d.len())..]);
    out
}

/// ContentEncoding with header stripping: the bytes removed from every frame.
fn header_strip(track: &[u8]) -> Vec<u8> {
    let Some(encs) = child(track, 0x6D80) else { return Vec::new() };
    for (id, enc) in children(encs) {
        if id != 0x6240 {
            continue;
        }
        if let Some(comp) = child(enc, 0x5034) {
            if child(comp, 0x4254).map(uint) == Some(3) {
                return child(comp, 0x4255).unwrap_or(&[]).to_vec();
            }
        }
    }
    Vec::new()
}

/// How long one sound packet lasts, for spacing laced frames.
fn frame_us(c: &ACodec, rate: u32) -> i64 {
    let samples = match c {
        ACodec::MpegAudio => 1152,
        ACodec::Ac3 | ACodec::Eac3 => 1536,
        ACodec::Aac { .. } | ACodec::AacAdts => 1024,
        ACodec::Opus { .. } => 960,
        _ => 0,
    };
    samples * 1_000_000 / rate.max(1) as i64
}

fn video_codec(id: &str, private: &[u8]) -> VCodec {
    match id {
        "V_MPEG4/ISO/AVC" => VCodec::H264 { avcc: private.to_vec() },
        "V_MPEGH/ISO/HEVC" => VCodec::Hevc { hvcc: private.to_vec() },
        "V_MPEG4/ISO/ASP" | "V_MPEG4/ISO/SP" | "V_MPEG4/ISO/AP" => VCodec::Mpeg4 { config: private.to_vec() },
        "V_MPEG1" | "V_MPEG2" => VCodec::Mpeg2 { config: private.to_vec() },
        "V_MJPEG" => VCodec::Mjpeg,
        "V_VP8" => VCodec::Vp8,
        "V_VP9" => VCodec::Vp9,
        "V_AV1" => VCodec::Other(String::from("AV1")),
        "V_MS/VFW/FOURCC" => super::avi::video_codec(private),
        "V_REAL/RV40" | "V_REAL/RV30" => VCodec::Other(String::from("RealVideo")),
        "V_THEORA" => VCodec::Other(String::from("Theora")),
        other => VCodec::Other(String::from(other)),
    }
}

fn audio_codec(id: &str, private: &[u8], rate: u32, channels: u16, bits: u16) -> ACodec {
    match id {
        "A_AAC" => ACodec::Aac { asc: private.to_vec() },
        s if s.starts_with("A_AAC/") => {
            if private.len() >= 2 {
                return ACodec::Aac { asc: private.to_vec() };
            }
            // Old style: build the AudioSpecificConfig from the track settings.
            let rates = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
            let sfi = rates.iter().position(|&r| r == rate).unwrap_or(4) as u8;
            let obj: u8 = if s.contains("SBR") { 5 } else if s.contains("MAIN") { 1 } else { 2 };
            ACodec::Aac { asc: alloc::vec![(obj << 3) | (sfi >> 1), ((sfi & 1) << 7) | ((channels as u8) << 3)] }
        }
        "A_MPEG/L3" | "A_MPEG/L2" | "A_MPEG/L1" => ACodec::MpegAudio,
        "A_AC3" | "A_AC3/BSID9" | "A_AC3/BSID10" => ACodec::Ac3,
        "A_EAC3" => ACodec::Eac3,
        "A_PCM/INT/LIT" => ACodec::Pcm { bits, big_endian: false, float: false },
        "A_PCM/INT/BIG" => ACodec::Pcm { bits, big_endian: true, float: false },
        "A_PCM/FLOAT/IEEE" => ACodec::Pcm { bits, big_endian: false, float: true },
        "A_VORBIS" => ACodec::Vorbis { headers: private.to_vec() },
        "A_OPUS" => ACodec::Opus { head: private.to_vec() },
        "A_DTS" => ACodec::Other(String::from("DTS")),
        "A_FLAC" => ACodec::Other(String::from("FLAC")),
        "A_TRUEHD" => ACodec::Other(String::from("Dolby TrueHD")),
        "A_MS/ACM" => ACodec::Other(String::from("Windows sound format")),
        other => ACodec::Other(String::from(other)),
    }
}

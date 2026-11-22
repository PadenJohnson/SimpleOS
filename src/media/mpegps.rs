//! MPEG program streams (.mpg, .mpeg, .vob from DVDs, .mod/.tod from camcorders): pack
//! headers and PES packets, including the DVD's AC-3 and PCM sound in private stream 1.
//! A picture spans several PES packets; a new one starts with each time stamp.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use super::es::{guess_video, is_key, mpeg2_size};
use super::mpegts::pes_header;
use super::{ACodec, AudioInfo, Demux, Info, Packet, Read, VCodec, VideoInfo};

const CHUNK: usize = 256 * 1024;

/// Which PES stream (and DVD sub-stream) a cursor follows.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Sid {
    id: u8,
    sub: Option<u8>,
}

struct Cursor {
    sid: Sid,
    pos: u64,
    data: Vec<u8>,
    pts: Option<i64>,
    started: bool,
    queue: VecDeque<Packet>,
    done: bool,
    skip_before: i64,
}

impl Cursor {
    fn new(sid: Sid, pos: u64) -> Cursor {
        Cursor { sid, pos, data: Vec::new(), pts: None, started: false, queue: VecDeque::new(), done: false, skip_before: i64::MIN }
    }
}

pub struct PsDemux {
    info: Info,
    vcodec: Option<VCodec>,
    size: u64,
    pts0: i64,
    v: Option<Cursor>,
    a: Option<Cursor>,
}

/// The next PES packet at or after `p` in `d`: (stream id, start, total length).
/// Pack and system headers are skipped. None = needs more data.
fn next_unit(d: &[u8], mut p: usize) -> Option<(u8, usize, usize)> {
    while p + 6 <= d.len() {
        if !(d[p] == 0 && d[p + 1] == 0 && d[p + 2] == 1) {
            p += 1;
            continue;
        }
        let id = d[p + 3];
        match id {
            0xBA => {
                // Pack header: MPEG-2 (14 bytes + stuffing) or MPEG-1 (12 bytes).
                if p + 14 > d.len() {
                    return None;
                }
                p += if d[p + 4] & 0xC0 == 0x40 { 14 + (d[p + 13] & 7) as usize } else { 12 };
            }
            0xB9 => p += 4,
            0xBB..=0xFF => {
                let len = ((d[p + 4] as usize) << 8) | d[p + 5] as usize;
                return Some((id, p, 6 + len));
            }
            _ => p += 4, // a stray start code inside data
        }
    }
    None
}

/// The payload of a PES packet for `sid`, and its time stamp; None if it isn't that stream.
fn unit_payload(d: &[u8], sid: Sid) -> Option<(Option<i64>, &[u8])> {
    if d.get(3) != Some(&sid.id) {
        return None;
    }
    let (pts, hl) = pes_header(d)?;
    let mut body = &d[hl..];
    if let Some(sub) = sid.sub {
        // Private stream 1: a sub-stream number, then its own small header.
        if body.first() != Some(&sub) {
            return None;
        }
        body = match sub {
            0x80..=0x8F => body.get(4..)?,  // AC-3 / DTS: frames, first access unit
            0xA0..=0xAF => body.get(7..)?,  // LPCM: plus the sound format
            _ => body.get(1..)?,
        };
    }
    Some((pts, body))
}

impl PsDemux {
    pub fn open(read: &mut Read, size: u64) -> Result<PsDemux, String> {
        let head = read(0, (2 << 20).min(size as usize)).ok_or("Can't read the file")?;
        let mut d = PsDemux { info: Info { duration_ms: 0, video: None, audio: None }, vcodec: None, size, pts0: i64::MAX, v: None, a: None };
        let mut p = 0;
        let mut vsid = None;
        let mut asid: Option<(Sid, ACodec, u32, u16)> = None;
        let mut first_video = Vec::new();
        while let Some((id, at, len)) = next_unit(&head, p) {
            if at + len > head.len() {
                break;
            }
            let u = &head[at..at + len];
            if let Some((Some(pts), _)) = pes_header(u) {
                d.pts0 = d.pts0.min(pts);
            }
            match id {
                0xE0..=0xEF if vsid.is_none() => {
                    vsid = Some(Sid { id, sub: None });
                    if let Some((_, body)) = unit_payload(u, Sid { id, sub: None }) {
                        first_video.extend_from_slice(body);
                    }
                }
                0xE0..=0xEF if Some(Sid { id, sub: None }) == vsid && first_video.len() < 64 * 1024 => {
                    if let Some((_, body)) = unit_payload(u, Sid { id, sub: None }) {
                        first_video.extend_from_slice(body);
                    }
                }
                0xC0..=0xDF if asid.is_none() => asid = Some((Sid { id, sub: None }, ACodec::MpegAudio, 0, 2)),
                0xBD if asid.is_none() => {
                    if let Some((_, hl)) = pes_header(u) {
                        let body = &u[hl..];
                        match body.first() {
                            Some(&s @ 0x80..=0x87) => asid = Some((Sid { id, sub: Some(s) }, ACodec::Ac3, 48000, 2)),
                            Some(&s @ 0x88..=0x8F) => asid = Some((Sid { id, sub: Some(s) }, ACodec::Dts, 48000, 2)),
                            Some(&s @ 0xA0..=0xA7) if body.len() > 6 => {
                                let f = body[5];
                                let bits = [16, 20, 24, 0][(f >> 6) as usize];
                                let rate = if f & 0x30 == 0x10 { 96000 } else { 48000 };
                                let ch = (f & 7) as u16 + 1;
                                let codec = if bits == 16 { ACodec::Pcm { bits: 16, big_endian: true, float: false } } else { ACodec::Other(String::from("DVD PCM (20/24-bit)")) };
                                asid = Some((Sid { id, sub: Some(s) }, codec, rate, ch));
                            }
                            _ => {}
                        }
                    }
                }
                _ => {}
            }
            p = at + len;
        }
        if vsid.is_none() && asid.is_none() {
            return Err(String::from("No pictures or sound found in this file"));
        }
        if let Some(s) = vsid {
            let codec = guess_video(&first_video);
            let (w, h) = mpeg2_size(&first_video).unwrap_or((0, 0));
            d.vcodec = Some(codec.clone());
            d.info.video = Some(VideoInfo { codec, width: w, height: h, rotation: 0 });
            d.v = Some(Cursor::new(s, 0));
        }
        if let Some((s, codec, rate, ch)) = asid {
            d.info.audio = Some(AudioInfo { codec, rate, channels: ch });
            d.a = Some(Cursor::new(s, 0));
        }
        if d.pts0 == i64::MAX {
            d.pts0 = 0;
        }
        // The length: the last time stamp near the end.
        if let Some(tail) = read(size.saturating_sub(2 << 20), 2 << 20) {
            let mut last = None;
            let mut p = 0;
            while let Some((_, at, len)) = next_unit(&tail, p) {
                if at + len > tail.len() {
                    break;
                }
                if let Some((Some(pts), _)) = pes_header(&tail[at..at + len]) {
                    last = Some(pts);
                }
                p = at + len.max(1);
            }
            if let Some(l) = last {
                d.info.duration_ms = ((l - d.pts0).max(0) / 90) as u64;
            }
        }
        Ok(d)
    }

    fn next(&mut self, read: &mut Read, video: bool) -> Option<Packet> {
        let pts0 = self.pts0;
        let size = self.size;
        let vcodec = self.vcodec.clone();
        let cur = if video { self.v.as_mut()? } else { self.a.as_mut()? };
        let emit = |cur: &mut Cursor| {
            let data = core::mem::take(&mut cur.data);
            if data.is_empty() {
                return;
            }
            let key = !video || vcodec.as_ref().is_some_and(|c| is_key(c, &data));
            cur.queue.push_back(Packet { pts: cur.pts.map(|t| (t - pts0) * 100 / 9), data, key });
        };
        loop {
            while let Some(p) = cur.queue.pop_front() {
                if p.pts.unwrap_or(i64::MAX) >= cur.skip_before {
                    cur.skip_before = i64::MIN;
                    return Some(p);
                }
            }
            if cur.done {
                return None;
            }
            let chunk = read(cur.pos, CHUNK).unwrap_or_default();
            let mut p = 0;
            let mut progressed = false;
            while let Some((_, at, len)) = next_unit(&chunk, p) {
                if at + len > chunk.len() {
                    break;
                }
                progressed = true;
                if let Some((pts, body)) = unit_payload(&chunk[at..at + len], cur.sid) {
                    if video {
                        // A time stamp starts a new picture; the PES packets after it continue it.
                        if pts.is_some() {
                            if cur.started {
                                emit(cur);
                            }
                            cur.pts = pts;
                            cur.started = true;
                        }
                        if cur.started {
                            cur.data.extend_from_slice(body);
                        }
                    } else if !body.is_empty() {
                        // Sound: each PES packet as it is (the decoder joins frames split between them).
                        cur.queue.push_back(Packet { pts: pts.map(|t| (t - pts0) * 100 / 9), data: body.to_vec(), key: true });
                    }
                }
                p = at + len;
            }
            if !progressed || chunk.len() < 64 {
                cur.done = true;
                if cur.started {
                    emit(cur);
                }
                continue;
            }
            cur.pos += p as u64;
            if cur.pos >= size {
                cur.done = true;
                emit(cur);
            }
        }
    }

    fn seek(&mut self, read: &mut Read, us: i64, video: bool) -> i64 {
        let dur = self.info.duration_ms.max(1) as i64 * 1000;
        let mut target = us;
        let mut result = 0;
        for attempt in 0..8 {
            let pos = (self.size as i128 * target.clamp(0, dur) as i128 / dur as i128) as u64;
            let Some(cur) = (if video { self.v.as_mut() } else { self.a.as_mut() }) else { return 0 };
            let sid = cur.sid;
            *cur = Cursor::new(sid, pos);
            let mut found = None;
            for _ in 0..3000 {
                let Some(p) = self.next(read, video) else { break };
                if p.key && p.pts.is_some() {
                    found = Some(p);
                    break;
                }
            }
            let Some(p) = found else {
                // Nothing usable after that point (near the end): aim earlier.
                if target <= 0 {
                    break;
                }
                target -= 2_000_000 * (attempt + 1);
                continue;
            };
            let t = p.pts.unwrap_or(0);
            result = t;
            let cur = if video { self.v.as_mut().unwrap() } else { self.a.as_mut().unwrap() };
            cur.queue.push_front(p);
            if t <= us && us - t < 3_000_000 || target <= 0 {
                break;
            }
            target -= if t > us { t - us + 1_000_000 } else { -(us - t) / 2 };
        }
        result
    }
}

impl Demux for PsDemux {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next_video(&mut self, read: &mut Read) -> Option<Packet> {
        self.next(read, true)
    }
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet> {
        self.next(read, false)
    }
    fn seek_video(&mut self, read: &mut Read, us: i64) -> i64 {
        if us <= 0 {
            if let Some(v) = self.v.as_mut() {
                *v = Cursor::new(v.sid, 0);
            }
            return 0;
        }
        self.seek(read, us, true)
    }
    fn seek_audio(&mut self, read: &mut Read, us: i64) {
        if us <= 0 {
            if let Some(a) = self.a.as_mut() {
                *a = Cursor::new(a.sid, 0);
            }
            return;
        }
        self.seek(read, us, false);
        if let Some(a) = self.a.as_mut() {
            a.skip_before = us - 100_000;
        }
    }
}

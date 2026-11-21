//! MPEG transport streams (.ts, .m2ts, .mts: TV recordings, camcorders, Blu-ray): 188-byte
//! packets (192 in .m2ts), the program tables (PAT/PMT), and PES packets with their times.
//! There's no index: jumping uses the average bit rate, then looks for a key frame.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;

use super::es::is_key;
use super::{ACodec, AudioInfo, Demux, Info, Packet, Read, VCodec, VideoInfo};

const CHUNK: usize = 512;

struct Cursor {
    pid: u16,
    /// File offset of the next transport packet to read.
    pos: u64,
    pes: Vec<u8>,
    pts: Option<i64>,
    rai: bool,
    started: bool,
    queue: VecDeque<Packet>,
    done: bool,
    /// After a jump: drop packets before this time (µs).
    skip_before: i64,
}

impl Cursor {
    fn new(pid: u16, pos: u64) -> Cursor {
        Cursor { pid, pos, pes: Vec::new(), pts: None, rai: false, started: false, queue: VecDeque::new(), done: false, skip_before: i64::MIN }
    }
}

pub struct TsDemux {
    info: Info,
    vcodec: Option<VCodec>,
    size: u64,
    /// Packet size (188 or 192) and where the 0x47 sync byte sits in each.
    psize: u64,
    sync: u64,
    first: u64,
    /// First time stamp (90 kHz): time zero.
    pts0: i64,
    v: Option<Cursor>,
    a: Option<Cursor>,
}

/// A 33-bit PES time stamp.
fn pes_ts(b: &[u8]) -> i64 {
    (((b[0] as i64 >> 1) & 7) << 30) | ((b[1] as i64) << 22) | (((b[2] as i64) >> 1) << 15) | ((b[3] as i64) << 7) | ((b[4] as i64) >> 1)
}

/// A PES packet's header: (time stamp, header length) if `d` starts one.
pub(crate) fn pes_header(d: &[u8]) -> Option<(Option<i64>, usize)> {
    if d.len() < 9 || d[0] != 0 || d[1] != 0 || d[2] != 1 {
        return None;
    }
    if d[6] & 0xC0 == 0x80 {
        // MPEG-2 PES.
        let hl = 9 + d[8] as usize;
        let pts = if d[7] & 0x80 != 0 && d.len() >= 14 { Some(pes_ts(&d[9..14])) } else { None };
        return Some((pts, hl.min(d.len())));
    }
    // MPEG-1 PES (program streams): stuffing, buffer size, then the time stamp.
    let mut p = 6;
    while p < d.len() && d[p] == 0xFF {
        p += 1;
    }
    if p < d.len() && d[p] & 0xC0 == 0x40 {
        p += 2;
    }
    match d.get(p).map(|b| b >> 4) {
        Some(2) if d.len() >= p + 5 => Some((Some(pes_ts(&d[p..p + 5])), p + 5)),
        Some(3) if d.len() >= p + 10 => Some((Some(pes_ts(&d[p..p + 5])), p + 10)),
        _ => Some((None, (p + 1).min(d.len()))),
    }
}

impl TsDemux {
    pub fn open(read: &mut Read, size: u64) -> Result<TsDemux, String> {
        let head = read(0, 192 * 8).ok_or("Can't read the file")?;
        let (psize, sync) = if head.len() > 376 && head[0] == 0x47 && head[188] == 0x47 && head[376] == 0x47 {
            (188, 0)
        } else if head.len() > 388 && head[4] == 0x47 && head[196] == 0x47 && head[388] == 0x47 {
            (192, 4)
        } else {
            return Err(String::from("Not an MPEG transport stream"));
        };
        let mut d = TsDemux { info: Info { duration_ms: 0, video: None, audio: None }, vcodec: None, size, psize, sync, first: 0, pts0: 0, v: None, a: None };
        // Program tables from the first few megabytes.
        let mut pmt_pid = None;
        let mut streams: Vec<(u8, u16, Vec<u8>)> = Vec::new();
        let mut pos = 0u64;
        while pos < size.min(8 << 20) && streams.is_empty() {
            let Some(chunk) = read(pos, (psize as usize) * CHUNK) else { break };
            if chunk.is_empty() {
                break;
            }
            for pk in chunk.chunks_exact(psize as usize) {
                let p = &pk[sync as usize..];
                if p[0] != 0x47 || p[1] & 0x40 == 0 {
                    continue;
                }
                let pid = (((p[1] & 0x1F) as u16) << 8) | p[2] as u16;
                let Some(payload) = payload(p) else { continue };
                if payload.is_empty() {
                    continue;
                }
                let sec = &payload[1 + payload[0] as usize..];
                if pid == 0 && pmt_pid.is_none() && sec.len() >= 12 && sec[0] == 0 {
                    // PAT: the first program with a number other than 0.
                    let len = ((((sec[1] & 15) as usize) << 8) | sec[2] as usize).min(sec.len() - 3);
                    let mut i = 8;
                    while i + 4 <= 3 + len - 4 {
                        let prog = ((sec[i] as u16) << 8) | sec[i + 1] as u16;
                        if prog != 0 {
                            pmt_pid = Some((((sec[i + 2] & 0x1F) as u16) << 8) | sec[i + 3] as u16);
                            break;
                        }
                        i += 4;
                    }
                } else if Some(pid) == pmt_pid && sec.len() >= 12 && sec[0] == 2 {
                    let len = ((((sec[1] & 15) as usize) << 8) | sec[2] as usize).min(sec.len() - 3);
                    let pinfo = (((sec[10] & 15) as usize) << 8) | sec[11] as usize;
                    let mut i = 12 + pinfo;
                    while i + 5 <= 3 + len - 4 {
                        let ty = sec[i];
                        let epid = (((sec[i + 1] & 0x1F) as u16) << 8) | sec[i + 2] as u16;
                        let elen = (((sec[i + 3] & 15) as usize) << 8) | sec[i + 4] as usize;
                        let desc = sec.get(i + 5..(i + 5 + elen).min(sec.len())).unwrap_or(&[]).to_vec();
                        streams.push((ty, epid, desc));
                        i += 5 + elen;
                    }
                    break;
                }
            }
            pos += chunk.len() as u64;
        }
        for (ty, pid, desc) in &streams {
            if d.v.is_none() {
                if let Some(c) = video_type(*ty, desc) {
                    d.vcodec = Some(c.clone());
                    d.info.video = Some(VideoInfo { codec: c, width: 0, height: 0, rotation: 0 });
                    d.v = Some(Cursor::new(*pid, 0));
                    continue;
                }
            }
            if d.a.is_none() {
                let c = match audio_type(*ty, desc) {
                    Some(c) => Some(c),
                    // Private data that doesn't say what it is: look at the data itself.
                    None if *ty == 0x06 => d.sniff_audio(read, *pid),
                    None => None,
                };
                if let Some(c) = c {
                    d.info.audio = Some(AudioInfo { codec: c, rate: 0, channels: 2 });
                    d.a = Some(Cursor::new(*pid, 0));
                }
            }
        }
        if d.v.is_none() && d.a.is_none() {
            return Err(String::from("No pictures or sound found in this stream"));
        }
        // Time zero and the length: the first and last time stamps.
        let pid = d.v.as_ref().or(d.a.as_ref()).unwrap().pid;
        let first = d.scan_pts(read, 0, 4 << 20, pid, true);
        let last = d.scan_pts(read, size.saturating_sub(4 << 20), 4 << 20, pid, false);
        if let Some((pts, at)) = first {
            d.pts0 = pts;
            d.first = at;
        }
        if let (Some((a, _)), Some((b, _))) = (first, last) {
            let mut diff = b - a;
            if diff < 0 {
                diff += 1 << 33;
            }
            d.info.duration_ms = (diff / 90) as u64;
        }
        let start = d.first;
        if let Some(v) = d.v.as_mut() {
            v.pos = start;
        }
        if let Some(a) = d.a.as_mut() {
            a.pos = start;
        }
        Ok(d)
    }

    /// What sound format a stream holds, from the start of its first PES packet.
    fn sniff_audio(&self, read: &mut Read, pid: u16) -> Option<ACodec> {
        let data = read(0, 4 << 20)?;
        for pk in data.chunks_exact(self.psize as usize) {
            let p = &pk[self.sync as usize..];
            if p[0] != 0x47 || p[1] & 0x40 == 0 || ((((p[1] & 0x1F) as u16) << 8) | p[2] as u16) != pid {
                continue;
            }
            let pl = payload(p)?;
            let (_, hl) = pes_header(pl)?;
            let b = pl.get(hl..hl + 6)?;
            return match (b[0], b[1]) {
                (0x0B, 0x77) => Some(if b[5] >> 3 > 10 { ACodec::Eac3 } else { ACodec::Ac3 }),
                (0xFF, x) if x & 0xF6 == 0xF0 => Some(ACodec::AacAdts),
                (0xFF, x) if x & 0xE0 == 0xE0 && x & 0x06 != 0 => Some(ACodec::MpegAudio),
                _ => None,
            };
        }
        None
    }

    /// The first (or last) PES time stamp of `pid` in [from, from + len): (pts, packet offset).
    fn scan_pts(&self, read: &mut Read, from: u64, len: u64, pid: u16, first: bool) -> Option<(i64, u64)> {
        let from = from / self.psize * self.psize;
        let data = read(from, len as usize)?;
        let mut found = None;
        let mut off = 0;
        // Find the sync (the file may be cut mid-packet at the end).
        while off + self.psize as usize * 3 <= data.len() && !(0..3).all(|k| data[off + self.sync as usize + k * self.psize as usize] == 0x47) {
            off += 1;
        }
        for (k, pk) in data[off..].chunks_exact(self.psize as usize).enumerate() {
            let p = &pk[self.sync as usize..];
            if p[0] != 0x47 || p[1] & 0x40 == 0 || ((((p[1] & 0x1F) as u16) << 8) | p[2] as u16) != pid {
                continue;
            }
            if let Some((Some(pts), _)) = payload(p).and_then(pes_header) {
                found = Some((pts, from + off as u64 + k as u64 * self.psize));
                if first {
                    break;
                }
            }
        }
        found
    }

    fn us(&self, pts: i64) -> i64 {
        let mut d = pts - self.pts0;
        if d < -(1 << 32) {
            d += 1 << 33;
        }
        d * 100 / 9
    }

    fn next(&mut self, read: &mut Read, video: bool) -> Option<Packet> {
        let (psize, sync, size) = (self.psize, self.sync, self.size);
        let vcodec = self.vcodec.clone();
        let pts0 = self.pts0;
        let to_us = |pts: i64| {
            let mut d = pts - pts0;
            if d < -(1 << 32) {
                d += 1 << 33;
            }
            d * 100 / 9
        };
        let cur = if video { self.v.as_mut()? } else { self.a.as_mut()? };
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
            let chunk = read(cur.pos, psize as usize * CHUNK).unwrap_or_default();
            if chunk.len() < psize as usize || cur.pos >= size {
                // End: the PES being put together is the last packet.
                cur.done = true;
                if cur.started && !cur.pes.is_empty() {
                    let data = core::mem::take(&mut cur.pes);
                    let key = !video || cur.rai || vcodec.as_ref().is_some_and(|c| is_key(c, &data));
                    cur.queue.push_back(Packet { pts: cur.pts.map(to_us), data, key });
                }
                continue;
            }
            for pk in chunk.chunks_exact(psize as usize) {
                let p = &pk[sync as usize..];
                if p[0] != 0x47 {
                    continue;
                }
                let pid = (((p[1] & 0x1F) as u16) << 8) | p[2] as u16;
                if pid != cur.pid {
                    continue;
                }
                let Some(pl) = payload(p) else { continue };
                let rai = p[3] & 0x20 != 0 && p[4] > 0 && p[5] & 0x40 != 0;
                if p[1] & 0x40 != 0 {
                    if let Some((pts, hl)) = pes_header(pl) {
                        if cur.started && !cur.pes.is_empty() {
                            let data = core::mem::take(&mut cur.pes);
                            let key = !video || cur.rai || vcodec.as_ref().is_some_and(|c| is_key(c, &data));
                            cur.queue.push_back(Packet { pts: cur.pts.map(to_us), data, key });
                        }
                        cur.started = true;
                        cur.pts = pts;
                        cur.rai = rai;
                        cur.pes.extend_from_slice(&pl[hl..]);
                        continue;
                    }
                }
                if cur.started {
                    cur.pes.extend_from_slice(pl);
                }
            }
            cur.pos += chunk.len() as u64 / psize * psize;
        }
    }

    /// Start the cursor near `us`: guess the place from the bit rate, then find the first
    /// packet there (a key frame for pictures) and check its time, a few times.
    fn seek(&mut self, read: &mut Read, us: i64, video: bool) -> i64 {
        let dur = self.info.duration_ms.max(1) as i64 * 1000;
        let span = self.size.saturating_sub(self.first);
        let mut target = us;
        let mut result = 0;
        for attempt in 0..8 {
            let guess = self.first + (span as i128 * target.clamp(0, dur) as i128 / dur as i128) as u64;
            let pos = guess / self.psize * self.psize;
            let Some(cur) = (if video { self.v.as_mut() } else { self.a.as_mut() }) else { return 0 };
            let pid = cur.pid;
            *cur = Cursor::new(pid, pos);
            // Read packets until a key frame (pictures) or any packet (sound).
            let mut found = None;
            for _ in 0..2000 {
                let Some(p) = self.next(read, video) else { break };
                if p.key {
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
            // Too late (or much too early): aim again.
            target -= if t > us { t - us + 1_000_000 } else { -(us - t) / 2 };
        }
        let _ = self.us(0);
        result
    }
}

impl Demux for TsDemux {
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
            let first = self.first;
            if let Some(v) = self.v.as_mut() {
                *v = Cursor::new(v.pid, first);
            }
            return 0;
        }
        self.seek(read, us, true)
    }
    fn seek_audio(&mut self, read: &mut Read, us: i64) {
        if us <= 0 {
            let first = self.first;
            if let Some(a) = self.a.as_mut() {
                *a = Cursor::new(a.pid, first);
            }
            return;
        }
        self.seek(read, us, false);
        if let Some(a) = self.a.as_mut() {
            a.skip_before = us - 100_000;
        }
    }
}

/// A transport packet's payload (after the adaptation field).
fn payload(p: &[u8]) -> Option<&[u8]> {
    let afc = (p[3] >> 4) & 3;
    match afc {
        1 => Some(&p[4..]),
        3 => {
            let s = 5 + p[4] as usize;
            p.get(s..)
        }
        _ => None,
    }
}

fn has_desc(desc: &[u8], tag: u8) -> bool {
    let mut i = 0;
    while i + 2 <= desc.len() {
        if desc[i] == tag {
            return true;
        }
        i += 2 + desc[i + 1] as usize;
    }
    false
}

fn registration(desc: &[u8]) -> Option<[u8; 4]> {
    let mut i = 0;
    while i + 2 <= desc.len() {
        if desc[i] == 5 && desc[i + 1] >= 4 && i + 6 <= desc.len() {
            return Some([desc[i + 2], desc[i + 3], desc[i + 4], desc[i + 5]]);
        }
        i += 2 + desc[i + 1] as usize;
    }
    None
}

fn video_type(ty: u8, desc: &[u8]) -> Option<VCodec> {
    Some(match ty {
        0x01 | 0x02 => VCodec::Mpeg2 { config: Vec::new() },
        0x10 => VCodec::Mpeg4 { config: Vec::new() },
        0x1B => VCodec::H264AnnexB,
        0x24 => VCodec::HevcAnnexB,
        0xEA => VCodec::Other(String::from("VC-1")),
        0x06 if registration(desc) == Some(*b"HEVC") => VCodec::HevcAnnexB,
        _ => return None,
    })
}

fn audio_type(ty: u8, desc: &[u8]) -> Option<ACodec> {
    Some(match ty {
        0x03 | 0x04 => ACodec::MpegAudio,
        0x0F => ACodec::AacAdts,
        0x11 => ACodec::Other(String::from("AAC (LATM)")),
        0x81 => ACodec::Ac3,
        0x87 => ACodec::Eac3,
        0x82 | 0x85 | 0x86 => ACodec::Other(String::from("DTS")),
        0x80 => ACodec::Other(String::from("Blu-ray PCM")),
        0x06 if has_desc(desc, 0x6A) || registration(desc) == Some(*b"AC-3") => ACodec::Ac3,
        0x06 if has_desc(desc, 0x7A) || registration(desc) == Some(*b"EAC3") => ACodec::Eac3,
        0x06 if has_desc(desc, 0x7B) => ACodec::Other(String::from("DTS")),
        _ => return None,
    })
}

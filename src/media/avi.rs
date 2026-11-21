//! AVI (older cameras, Xvid/DivX movies): any picture and sound format, using the file's
//! index (old idx1 or OpenDML) for key frames and jumping around.

use alloc::string::String;
use alloc::vec::Vec;

use super::{le16, le32, ACodec, AudioInfo, Demux, Info, Packet, Read, VCodec, VideoInfo};

struct Stream {
    kind: [u8; 4],
    scale: u32,
    rate: u32,
    sample_size: u32,
    fmt: Vec<u8>,
    /// OpenDML super index: (offset of an ix## chunk, its size).
    super_index: Vec<(u64, u32)>,
}

/// One chunk of a stream: where its data is, how long, key frame, and its time (µs).
#[derive(Clone, Copy)]
struct Chunk {
    off: u64,
    len: u32,
    key: bool,
    pts: i64,
}

pub struct AviDemux {
    info: Info,
    video: Vec<Chunk>,
    audio: Vec<Chunk>,
    vnext: usize,
    anext: usize,
}

fn le64(b: &[u8], o: usize) -> u64 {
    le32(b, o) as u64 | (le32(b, o + 4) as u64) << 32
}

impl AviDemux {
    pub fn open(read: &mut Read, file_len: u64) -> Result<AviDemux, String> {
        let head = read(0, 12).ok_or("Can't read the file")?;
        let mut streams: Vec<Stream> = Vec::new();
        let (mut width, mut height) = (0u32, 0u32);
        let mut movis: Vec<(u64, u64)> = Vec::new();
        let mut idx1: Option<Vec<u8>> = None;
        let mut pos = 12u64;
        let mut riff_end = (8 + le32(&head, 4) as u64).min(file_len);
        let mut guard = 0;
        while pos + 8 <= file_len && guard < 100_000 {
            guard += 1;
            if pos + 8 > riff_end {
                let h = read(pos, 12).ok_or("Can't read the file")?;
                if h.len() < 12 || &h[0..4] != b"RIFF" {
                    break;
                }
                riff_end = (pos + 8 + le32(&h, 4) as u64).min(file_len);
                pos += 12;
                continue;
            }
            let h = read(pos, 12).ok_or("Can't read the file")?;
            if h.len() < 8 {
                break;
            }
            let size = le32(&h, 4) as u64;
            let body = pos + 8;
            if &h[0..4] == b"LIST" && h.len() >= 12 {
                if &h[8..12] == b"hdrl" {
                    let d = read(body + 4, size.saturating_sub(4).min(4 << 20) as usize).ok_or("Can't read the file")?;
                    parse_hdrl(&d, &mut streams, &mut width, &mut height);
                } else if &h[8..12] == b"movi" {
                    movis.push((body + 4, (body + size).min(file_len)));
                }
            } else if &h[0..4] == b"idx1" {
                idx1 = read(body, size.min(64 << 20) as usize);
            }
            pos = body + size + (size & 1);
        }
        let vi = streams.iter().position(|s| &s.kind == b"vids");
        let ai = streams.iter().position(|s| &s.kind == b"auds");
        if vi.is_none() && ai.is_none() {
            return Err(String::from("This video has no pictures or sound"));
        }
        // Chunks of each stream, from the OpenDML index, idx1, or by walking the data.
        let mut lists: [Vec<(u64, u32, bool)>; 2] = [Vec::new(), Vec::new()];
        for (slot, si) in [vi, ai].into_iter().enumerate() {
            if let Some(si) = si {
                read_odml(read, &streams[si].super_index, &mut lists[slot]);
            }
        }
        let tag = |i: usize| [b'0' + (i / 10) as u8, b'0' + (i % 10) as u8];
        if lists[0].is_empty() && lists[1].is_empty() {
            if let (Some(idx), Some(&(movi, _))) = (&idx1, movis.first()) {
                let base = match idx.get(8..12).map(|b| le32(b, 0) as u64) {
                    Some(first) if first >= movi => 0,
                    _ => movi - 4,
                };
                for e in idx.chunks_exact(16) {
                    let t = [e[0], e[1]];
                    let item = (base + le32(e, 8) as u64 + 8, le32(e, 12), le32(e, 4) & 0x10 != 0);
                    if vi.is_some_and(|v| t == tag(v)) && (e[2] == b'd') {
                        lists[0].push(item);
                    } else if ai.is_some_and(|a| t == tag(a)) && &e[2..4] == b"wb" {
                        lists[1].push(item);
                    }
                }
            }
        }
        if lists[0].is_empty() && lists[1].is_empty() {
            for &(start, end) in &movis {
                let mut p = start;
                let mut n = 0;
                while p + 8 <= end && n < 2_000_000 {
                    n += 1;
                    let Some(h) = read(p, 12) else { break };
                    if h.len() < 8 {
                        break;
                    }
                    let size = le32(&h, 4) as u64;
                    if &h[0..4] == b"LIST" {
                        p += 12;
                        continue;
                    }
                    let t = [h[0], h[1]];
                    if vi.is_some_and(|v| t == tag(v)) && h[2] == b'd' {
                        // No index: only the first frame is known to be a key frame.
                        let first = lists[0].is_empty();
                        lists[0].push((p + 8, size as u32, first));
                    } else if ai.is_some_and(|a| t == tag(a)) && &h[2..4] == b"wb" {
                        lists[1].push((p + 8, size as u32, true));
                    }
                    p += 8 + size + (size & 1);
                }
            }
        }
        // Times.
        let mut video = Vec::new();
        let mut vinfo = None;
        if let Some(v) = vi.map(|i| &streams[i]) {
            let (rate, scale) = if v.rate > 0 && v.scale > 0 { (v.rate as i64, v.scale as i64) } else { (25, 1) };
            for (i, &(off, len, key)) in lists[0].iter().enumerate() {
                video.push(Chunk { off, len, key, pts: i as i64 * scale * 1_000_000 / rate });
            }
            if !video.iter().any(|c| c.key) {
                if let Some(c) = video.first_mut() {
                    c.key = true;
                }
            }
            let f = &v.fmt;
            if width == 0 && f.len() >= 12 {
                width = le32(f, 4);
                height = (le32(f, 8) as i32).unsigned_abs();
            }
            vinfo = Some(VideoInfo { codec: video_codec(f), width, height, rotation: 0 });
        }
        let mut audio = Vec::new();
        let mut ainfo = None;
        if let Some(a) = ai.map(|i| &streams[i]) {
            let f = &a.fmt;
            if f.len() >= 16 {
                let avg = le32(f, 8).max(1) as i64;
                let mut bytes = 0i64;
                for (i, &(off, len, _)) in lists[1].iter().enumerate() {
                    let pts = if a.sample_size == 0 && a.rate > 0 {
                        // Variable bit rate: each chunk is `scale` samples.
                        i as i64 * a.scale.max(1) as i64 * 1_000_000 / a.rate as i64
                    } else {
                        bytes * 1_000_000 / avg
                    };
                    bytes += len as i64;
                    if len > 0 {
                        audio.push(Chunk { off, len, key: true, pts });
                    }
                }
                ainfo = Some(AudioInfo { codec: audio_codec(f), rate: le32(f, 4), channels: le16(f, 2) });
            }
        }
        let vdur = video.last().map(|c| c.pts / 1000 + 40).unwrap_or(0);
        let adur = audio.last().map(|c| c.pts / 1000).unwrap_or(0);
        Ok(AviDemux { info: Info { duration_ms: vdur.max(adur) as u64, video: vinfo, audio: ainfo }, video, audio, vnext: 0, anext: 0 })
    }
}

impl Demux for AviDemux {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next_video(&mut self, read: &mut Read) -> Option<Packet> {
        let c = *self.video.get(self.vnext)?;
        self.vnext += 1;
        let data = if c.len == 0 { Vec::new() } else { read(c.off, c.len as usize).unwrap_or_default() };
        Some(Packet { pts: Some(c.pts), data, key: c.key })
    }
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet> {
        let c = *self.audio.get(self.anext)?;
        self.anext += 1;
        Some(Packet { pts: Some(c.pts), data: read(c.off, c.len as usize).unwrap_or_default(), key: true })
    }
    fn seek_video(&mut self, _read: &mut Read, us: i64) -> i64 {
        let n = self.video.partition_point(|c| c.pts <= us);
        let k = (0..n).rev().find(|&i| self.video[i].key).unwrap_or(0);
        self.vnext = k;
        self.video.get(k).map(|c| c.pts).unwrap_or(0)
    }
    fn seek_audio(&mut self, _read: &mut Read, us: i64) {
        self.anext = self.audio.partition_point(|c| c.pts <= us).saturating_sub(1);
    }
    fn key_before(&self, us: i64) -> Option<i64> {
        let n = self.video.partition_point(|c| c.pts <= us);
        (0..n).rev().find(|&i| self.video[i].key).map(|i| self.video[i].pts)
    }
}

fn parse_hdrl(d: &[u8], streams: &mut Vec<Stream>, width: &mut u32, height: &mut u32) {
    let mut p = 0;
    while p + 8 <= d.len() {
        let id = &d[p..p + 4];
        let size = le32(d, p + 4) as usize;
        let body = p + 8;
        let end = (body + size).min(d.len());
        if id == b"avih" && end - body >= 40 {
            *width = le32(d, body + 32);
            *height = le32(d, body + 36);
        } else if id == b"LIST" && end - body >= 4 && &d[body..body + 4] == b"strl" {
            let mut s = Stream { kind: [0; 4], scale: 0, rate: 0, sample_size: 0, fmt: Vec::new(), super_index: Vec::new() };
            let mut q = body + 4;
            while q + 8 <= end {
                let cid = &d[q..q + 4];
                let csize = le32(d, q + 4) as usize;
                let cb = q + 8;
                let ce = (cb + csize).min(end);
                if cid == b"strh" && ce - cb >= 48 {
                    s.kind.copy_from_slice(&d[cb..cb + 4]);
                    s.scale = le32(d, cb + 20);
                    s.rate = le32(d, cb + 24);
                    s.sample_size = le32(d, cb + 44);
                } else if cid == b"strh" && ce - cb >= 28 {
                    s.kind.copy_from_slice(&d[cb..cb + 4]);
                    s.scale = le32(d, cb + 20);
                    s.rate = le32(d, cb + 24);
                } else if cid == b"strf" {
                    s.fmt = d[cb..ce].to_vec();
                } else if cid == b"indx" && ce - cb >= 24 && d[cb + 3] == 0 {
                    let entries = le32(d, cb + 4) as usize;
                    for k in 0..entries {
                        let e = cb + 24 + k * 16;
                        if e + 16 > ce {
                            break;
                        }
                        s.super_index.push((le64(d, e), le32(d, e + 8)));
                    }
                }
                q = cb + csize + (csize & 1);
            }
            streams.push(s);
        }
        p = body + size + (size & 1);
    }
}

/// OpenDML standard index chunks: (data offset, size, key frame).
fn read_odml(read: &mut Read, supers: &[(u64, u32)], out: &mut Vec<(u64, u32, bool)>) {
    for &(off, size) in supers {
        let Some(ix) = read(off, size as usize) else { continue };
        if ix.len() < 32 {
            continue;
        }
        let body = &ix[8..];
        let per = (body[0] as usize * 4).max(8);
        let entries = le32(body, 4) as usize;
        let base = le64(body, 12);
        for k in 0..entries {
            let e = 24 + k * per;
            if e + 8 > body.len() {
                break;
            }
            let sz = le32(body, e + 4);
            out.push((base + le32(body, e) as u64, sz & 0x7FFF_FFFF, sz & 0x8000_0000 == 0));
        }
    }
}

pub(crate) fn video_codec(f: &[u8]) -> VCodec {
    if f.len() < 20 {
        return VCodec::Other(String::from("unknown"));
    }
    let extra = f.get(le32(f, 0).clamp(40, f.len() as u32) as usize..).unwrap_or(&[]).to_vec();
    let fcc = [f[16].to_ascii_uppercase(), f[17].to_ascii_uppercase(), f[18].to_ascii_uppercase(), f[19].to_ascii_uppercase()];
    match &fcc {
        b"MJPG" | b"AVRN" | b"LJPG" | b"JPGL" | b"DMB1" | b"MJPA" => VCodec::Mjpeg,
        b"XVID" | b"DIVX" | b"DX50" | b"FMP4" | b"MP4V" | b"M4S2" | b"3IV2" | b"XVIX" | b"BLZ0" | b"DXGM" | b"RMP4" | b"SEDG" | b"WV1F" | b"UMP4" | b"MP4S" => VCodec::Mpeg4 { config: extra },
        b"H264" | b"X264" | b"AVC1" | b"DAVC" | b"VSSH" => VCodec::H264AnnexB,
        b"HEVC" | b"H265" | b"HVC1" | b"X265" => VCodec::HevcAnnexB,
        b"MPG2" | b"MPEG" | b"MPG1" | b"MMES" | b"PIM1" | b"PIM2" | b"EM2V" => VCodec::Mpeg2 { config: extra },
        b"H263" | b"U263" | b"S263" | b"M263" | b"X263" => VCodec::H263,
        b"VP80" => VCodec::Vp8,
        b"VP90" => VCodec::Vp9,
        b"DIV3" | b"MP43" | b"DIV4" | b"MP42" | b"MPG4" | b"DIV5" | b"DIV6" | b"AP41" => VCodec::Other(String::from("DivX 3 (Microsoft MPEG-4)")),
        b"WMV1" | b"WMV2" | b"WMV3" | b"WVC1" | b"WMVA" => VCodec::Other(String::from("Windows Media Video")),
        _ => VCodec::Other(String::from_utf8_lossy(&f[16..20]).into_owned()),
    }
}

fn audio_codec(f: &[u8]) -> ACodec {
    let tag = le16(f, 0);
    let bits = le16(f, 14);
    let extra = if f.len() >= 18 { f.get(18..(18 + le16(f, 16) as usize).min(f.len())).unwrap_or(&[]).to_vec() } else { Vec::new() };
    match tag {
        1 => ACodec::Pcm { bits, big_endian: false, float: false },
        3 => ACodec::Pcm { bits, big_endian: false, float: true },
        0xFFFE if f.len() >= 26 => match le16(f, 24) {
            1 => ACodec::Pcm { bits, big_endian: false, float: false },
            3 => ACodec::Pcm { bits, big_endian: false, float: true },
            _ => ACodec::Other(String::from("unknown sound")),
        },
        0x50 | 0x55 => ACodec::MpegAudio,
        0x2000 => ACodec::Ac3,
        0xFF | 0x1610 | 0x706D | 0x4143 => {
            if extra.len() >= 2 {
                ACodec::Aac { asc: extra }
            } else {
                ACodec::AacAdts
            }
        }
        0x2001 => ACodec::Other(String::from("DTS")),
        0x160..=0x163 => ACodec::Other(String::from("Windows Media Audio")),
        _ => ACodec::Other(alloc::format!("sound format {:#x}", tag)),
    }
}

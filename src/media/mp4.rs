//! MP4, MOV, M4V and 3GP (phones, cameras, iTunes): uses the index reader in video::mp4read
//! and recognises every picture and sound format it might hold.

use alloc::string::String;
use alloc::vec::Vec;

use super::{be16, ACodec, AudioInfo, Demux, Info, Packet, Read, VCodec, VideoInfo};
use crate::video::mp4read::{find, Codec, Movie, Sample, Track};

struct Cursor {
    samples: Vec<Sample>,
    timescale: i64,
    /// Time zero of the track (earliest presentation time), in its timescale.
    t0: i64,
    next: usize,
    /// Key frames: (presentation time in µs, sample index), in decode order.
    keys: Vec<(i64, usize)>,
    video: bool,
}

impl Cursor {
    fn new(t: &Track) -> Cursor {
        let ts = t.timescale.max(1) as i64;
        let t0 = if t.video { t.samples.iter().map(|s| s.dts as i64 + s.cts as i64).min().unwrap_or(0) } else { t.samples.first().map(|s| s.dts as i64).unwrap_or(0) };
        let mut c = Cursor { samples: t.samples.clone(), timescale: ts, t0, next: 0, keys: Vec::new(), video: t.video };
        c.keys = (0..c.samples.len()).filter(|&i| c.samples[i].key).map(|i| (c.pts(i), i)).collect();
        if c.keys.is_empty() && !c.samples.is_empty() {
            c.keys.push((c.pts(0), 0));
        }
        c
    }
    fn pts(&self, i: usize) -> i64 {
        let s = &self.samples[i];
        let t = s.dts as i64 + if self.video { s.cts as i64 } else { 0 };
        (t - self.t0) * 1_000_000 / self.timescale
    }
    fn next(&mut self, read: &mut Read) -> Option<Packet> {
        let i = self.next;
        let s = *self.samples.get(i)?;
        self.next += 1;
        let data = read(s.offset, s.size as usize).unwrap_or_default();
        Some(Packet { pts: Some(self.pts(i)), data, key: s.key })
    }
    /// The last key frame at or before `us` (index into `keys`).
    fn key_at(&self, us: i64) -> Option<usize> {
        let k = self.keys.partition_point(|&(t, _)| t <= us);
        Some(k.saturating_sub(1)).filter(|_| !self.keys.is_empty())
    }
}

pub struct Mp4Demux {
    info: Info,
    v: Option<Cursor>,
    a: Option<Cursor>,
}

impl Mp4Demux {
    pub fn open(read: &mut Read, size: u64) -> Result<Mp4Demux, String> {
        let movie = Movie::parse(read, size).map_err(String::from)?;
        let duration_ms = movie.duration_ms();
        let video = movie.video.as_ref().map(|t| VideoInfo { codec: video_codec(t), width: t.width, height: t.height, rotation: t.rotation });
        let audio = movie.audio.as_ref().map(|t| AudioInfo { codec: audio_codec(t), rate: t.rate, channels: t.channels });
        Ok(Mp4Demux { info: Info { duration_ms, video, audio }, v: movie.video.as_ref().map(Cursor::new), a: movie.audio.as_ref().map(Cursor::new) })
    }
}

impl Demux for Mp4Demux {
    fn info(&self) -> &Info {
        &self.info
    }
    fn next_video(&mut self, read: &mut Read) -> Option<Packet> {
        self.v.as_mut()?.next(read)
    }
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet> {
        self.a.as_mut()?.next(read)
    }
    fn seek_video(&mut self, _read: &mut Read, us: i64) -> i64 {
        let Some(v) = self.v.as_mut() else { return 0 };
        match v.key_at(us) {
            Some(k) => {
                let (t, i) = v.keys[k];
                v.next = i;
                t
            }
            None => 0,
        }
    }
    fn seek_audio(&mut self, _read: &mut Read, us: i64) {
        if let Some(a) = self.a.as_mut() {
            // The packet that contains `us` (sound packets are in time order).
            let (mut lo, mut hi) = (0, a.samples.len());
            while lo < hi {
                let mid = (lo + hi) / 2;
                if a.pts(mid) <= us {
                    lo = mid + 1;
                } else {
                    hi = mid;
                }
            }
            a.next = lo.saturating_sub(1);
        }
    }
    fn key_before(&self, us: i64) -> Option<i64> {
        let v = self.v.as_ref()?;
        v.key_at(us).map(|k| v.keys[k].0)
    }
}

/// The ES descriptor in an esds box: (object type, decoder-specific info).
fn esds(d: &[u8]) -> Option<(u8, Vec<u8>)> {
    let mut p = 4;
    let mut obj = 0;
    let len_at = |p: &mut usize| -> usize {
        let mut n = 0;
        for _ in 0..4 {
            let Some(&b) = d.get(*p) else { break };
            *p += 1;
            n = (n << 7) | (b & 0x7F) as usize;
            if b & 0x80 == 0 {
                break;
            }
        }
        n
    };
    while p + 2 <= d.len() {
        let tag = d[p];
        p += 1;
        let len = len_at(&mut p);
        match tag {
            0x03 => {
                let flags = *d.get(p + 2)?;
                p += 3;
                if flags & 0x80 != 0 {
                    p += 2;
                }
                if flags & 0x40 != 0 {
                    p += 1 + *d.get(p)? as usize;
                }
                if flags & 0x20 != 0 {
                    p += 2;
                }
            }
            0x04 => {
                obj = *d.get(p)?;
                p += 13;
            }
            0x05 => return Some((obj, d.get(p..p + len)?.to_vec())),
            _ => p += len,
        }
    }
    (obj != 0).then(|| (obj, Vec::new()))
}

fn video_codec(t: &Track) -> VCodec {
    if let Codec::H264 { avcc } = &t.codec {
        return VCodec::H264 { avcc: avcc.clone() };
    }
    let k = &t.kids;
    match &t.fourcc {
        b"hvc1" | b"hev1" => match find(k, b"hvcC") {
            Some(h) => VCodec::Hevc { hvcc: h.to_vec() },
            None => VCodec::HevcAnnexB,
        },
        b"mp4v" => match find(k, b"esds").and_then(esds) {
            Some((0x20, dsi)) => VCodec::Mpeg4 { config: dsi },
            Some((0x60..=0x65 | 0x6A, dsi)) => VCodec::Mpeg2 { config: dsi },
            Some((0x6C, _)) => VCodec::Mjpeg,
            _ => VCodec::Mpeg4 { config: Vec::new() },
        },
        b"s263" | b"h263" | b"H263" => VCodec::H263,
        b"jpeg" | b"mjpa" | b"mjpb" | b"MJPG" => VCodec::Mjpeg,
        b"vp08" => VCodec::Vp8,
        b"vp09" => VCodec::Vp9,
        b"av01" => VCodec::Other(String::from("AV1")),
        b"mp2v" | b"mpg2" | b"xdv2" | b"xdvc" | b"mx5p" => VCodec::Mpeg2 { config: Vec::new() },
        f => VCodec::Other(String::from_utf8_lossy(f).into_owned()),
    }
}

fn audio_codec(t: &Track) -> ACodec {
    if let Codec::Aac { asc } = &t.codec {
        return ACodec::Aac { asc: asc.clone() };
    }
    let k = &t.kids;
    match &t.fourcc {
        b"mp4a" => {
            let e = find(k, b"esds").or_else(|| find(k, b"wave").and_then(|w| find(w, b"esds")));
            match e.and_then(esds) {
                Some((0x69 | 0x6B, _)) => ACodec::MpegAudio,
                Some((0xA5, _)) => ACodec::Ac3,
                Some((0xA6, _)) => ACodec::Eac3,
                _ => ACodec::Other(String::from("AAC (unreadable settings)")),
            }
        }
        b".mp3" | b"ms\x00\x55" => ACodec::MpegAudio,
        b"ac-3" => ACodec::Ac3,
        b"ec-3" => ACodec::Eac3,
        b"sowt" => ACodec::Pcm { bits: 16, big_endian: false, float: false },
        b"twos" => ACodec::Pcm { bits: 16, big_endian: true, float: false },
        b"in24" => ACodec::Pcm { bits: 24, big_endian: !has_little_endian(k), float: false },
        b"in32" => ACodec::Pcm { bits: 32, big_endian: !has_little_endian(k), float: false },
        b"fl32" => ACodec::Pcm { bits: 32, big_endian: !has_little_endian(k), float: true },
        b"raw " => ACodec::Pcm { bits: 8, big_endian: false, float: false },
        b"lpcm" => ACodec::Pcm { bits: 16, big_endian: false, float: false },
        b"Opus" => ACodec::Opus { head: find(k, b"dOps").map(|d| d.to_vec()).unwrap_or_default() },
        b"dtsc" | b"dtsh" | b"dtsl" | b"dtse" => ACodec::Dts,
        b"samr" => ACodec::Amr { wide: false },
        b"sawb" => ACodec::Amr { wide: true },
        b"alac" => ACodec::Other(String::from("Apple Lossless")),
        f => ACodec::Other(String::from_utf8_lossy(f).into_owned()),
    }
}

/// QuickTime marks little-endian PCM with an "enda" box.
fn has_little_endian(k: &[u8]) -> bool {
    find(k, b"wave").and_then(|w| find(w, b"enda")).is_some_and(|e| e.len() >= 2 && be16(e, 0) == 1)
}

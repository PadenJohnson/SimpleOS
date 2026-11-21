//! Reading MP4 / MOV files (phone and camera videos): finds the H.264 video track and the
//! AAC sound track, and where every frame and sound packet is. Only the index ("moov") is
//! read up front; the data is fetched as needed through a `read(offset, len)` callback.

use alloc::string::String;
use alloc::vec::Vec;

pub type ReadFn<'a> = dyn FnMut(u64, usize) -> Option<Vec<u8>> + 'a;

#[derive(Clone, Copy, Debug)]
pub struct Sample {
    pub offset: u64,
    pub size: u32,
    /// Decode time and presentation time offset, in the track's timescale.
    pub dts: u64,
    pub cts: i32,
    pub key: bool,
}

#[derive(Clone, Debug)]
pub enum Codec {
    /// avcC record (SPS/PPS and the NAL length size).
    H264 { avcc: Vec<u8> },
    /// AudioSpecificConfig from esds.
    Aac { asc: Vec<u8> },
    Other(String),
}

#[derive(Clone, Debug)]
pub struct Track {
    pub video: bool,
    pub codec: Codec,
    pub timescale: u32,
    pub duration: u64,
    pub width: u32,
    pub height: u32,
    pub channels: u16,
    pub rate: u32,
    /// Rotation from the track matrix (phones filming upright): 0, 90, 180 or 270.
    pub rotation: u32,
    pub samples: Vec<Sample>,
    /// The sample entry's four-character code and the boxes inside it (codec settings),
    /// for formats this reader doesn't decode itself.
    pub fourcc: [u8; 4],
    pub kids: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct Movie {
    pub video: Option<Track>,
    pub audio: Option<Track>,
}

fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn be64(b: &[u8], o: usize) -> u64 {
    (be32(b, o) as u64) << 32 | be32(b, o + 4) as u64
}

/// Boxes inside `d`: (type, body start, body end).
pub fn boxes(d: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let mut out = Vec::new();
    let mut p = 0;
    while p + 8 <= d.len() {
        let mut size = be32(d, p) as usize;
        let kind = [d[p + 4], d[p + 5], d[p + 6], d[p + 7]];
        let mut hdr = 8;
        if size == 1 && p + 16 <= d.len() {
            size = be64(d, p + 8) as usize;
            hdr = 16;
        } else if size == 0 {
            size = d.len() - p;
        }
        if size < hdr || p + size > d.len() {
            break;
        }
        out.push((kind, p + hdr, p + size));
        p += size;
    }
    out
}

pub fn find<'a>(d: &'a [u8], kind: &[u8; 4]) -> Option<&'a [u8]> {
    boxes(d).into_iter().find(|b| &b.0 == kind).map(|b| &d[b.1..b.2])
}

/// Read an MPEG-4 descriptor length (1-4 bytes of 7 bits).
fn desc_len(d: &[u8], p: &mut usize) -> usize {
    let mut n = 0;
    for _ in 0..4 {
        if *p >= d.len() {
            break;
        }
        let b = d[*p];
        *p += 1;
        n = n << 7 | (b & 0x7F) as usize;
        if b & 0x80 == 0 {
            break;
        }
    }
    n
}

/// The AudioSpecificConfig inside an esds box.
fn esds_asc(d: &[u8]) -> Option<Vec<u8>> {
    let mut p = 4; // version/flags
    while p + 2 <= d.len() {
        let tag = d[p];
        p += 1;
        let len = desc_len(d, &mut p);
        match tag {
            0x03 => {
                // ES_Descriptor: ES_ID, flags (+ optional fields), then children.
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
            0x04 => p += 13, // DecoderConfigDescriptor header, then children
            0x05 => return d.get(p..p + len).map(|s| s.to_vec()),
            _ => p += len,
        }
    }
    None
}

fn parse_trak(t: &[u8]) -> Option<Track> {
    let mdia = find(t, b"mdia")?;
    let hdlr = find(mdia, b"hdlr")?;
    let kind = hdlr.get(8..12)?;
    let video = kind == b"vide";
    if !video && kind != b"soun" {
        return None;
    }
    let mdhd = find(mdia, b"mdhd")?;
    let (timescale, duration) = if mdhd[0] == 1 { (be32(mdhd, 20), be64(mdhd, 24)) } else { (be32(mdhd, 12), be32(mdhd, 16) as u64) };
    let mut rotation = 0;
    if let Some(tkhd) = find(t, b"tkhd") {
        let m = if tkhd[0] == 1 { 48 } else { 40 };
        if tkhd.len() >= m + 36 {
            let (a, b) = (be32(tkhd, m) as i32, be32(tkhd, m + 4) as i32);
            rotation = match (a, b) {
                (0, 0x10000) => 90,
                (-0x10000, 0) => 180,
                (0, -0x10000) => 270,
                _ => 0,
            };
        }
    }
    let stbl = find(find(mdia, b"minf")?, b"stbl")?;
    let stsd = find(stbl, b"stsd")?;
    let entry = boxes(stsd.get(8..)?).into_iter().next()?;
    let e = &stsd[8 + entry.1..8 + entry.2];
    let fourcc = entry.0;
    let mut tr = Track { video, codec: Codec::Other(String::from_utf8_lossy(&fourcc).into_owned()), timescale: timescale.max(1), duration, width: 0, height: 0, channels: 0, rate: 0, rotation, samples: Vec::new(), fourcc, kids: Vec::new() };
    if video {
        if e.len() >= 78 {
            tr.width = be16(e, 24) as u32;
            tr.height = be16(e, 26) as u32;
        }
        tr.kids = e.get(78..).unwrap_or(&[]).to_vec();
        if &fourcc == b"avc1" || &fourcc == b"avc3" {
            if let Some(avcc) = e.get(78..).and_then(|x| find(x, b"avcC")) {
                tr.codec = Codec::H264 { avcc: avcc.to_vec() };
            }
        }
    } else if e.len() >= 28 {
        let version = be16(e, 8);
        tr.channels = be16(e, 16);
        tr.rate = be32(e, 24) >> 16;
        let skip = match version {
            1 => 28 + 16,
            2 => 28 + 36,
            _ => 28,
        };
        tr.kids = e.get(skip..).unwrap_or(&[]).to_vec();
        if &fourcc == b"mp4a" {
            // esds may sit directly inside, or inside a 'wave' box (old QuickTime).
            let kids = e.get(skip..).unwrap_or(&[]);
            let esds = find(kids, b"esds").or_else(|| find(kids, b"wave").and_then(|w| find(w, b"esds")));
            if let Some(asc) = esds.and_then(esds_asc) {
                tr.codec = Codec::Aac { asc };
            }
        }
    }
    // Sample tables.
    let sizes: Vec<u32> = if let Some(stsz) = find(stbl, b"stsz") {
        let fixed = be32(stsz, 4);
        let n = be32(stsz, 8) as usize;
        if fixed != 0 {
            alloc::vec![fixed; n]
        } else {
            (0..n).filter(|i| 12 + i * 4 + 4 <= stsz.len()).map(|i| be32(stsz, 12 + i * 4)).collect()
        }
    } else if let Some(stz2) = find(stbl, b"stz2") {
        let bits = stz2[7];
        let n = be32(stz2, 8) as usize;
        (0..n)
            .map(|i| match bits {
                4 => ((stz2[12 + i / 2] >> if i % 2 == 0 { 4 } else { 0 }) & 15) as u32,
                8 => stz2[12 + i] as u32,
                _ => be16(stz2, 12 + i * 2) as u32,
            })
            .collect()
    } else {
        return Some(tr);
    };
    let chunks: Vec<u64> = if let Some(stco) = find(stbl, b"stco") {
        (0..be32(stco, 4) as usize).map(|i| be32(stco, 8 + i * 4) as u64).collect()
    } else if let Some(co64) = find(stbl, b"co64") {
        (0..be32(co64, 4) as usize).map(|i| be64(co64, 8 + i * 8)).collect()
    } else {
        return Some(tr);
    };
    let stsc: Vec<(u32, u32)> = find(stbl, b"stsc").map(|s| (0..be32(s, 4) as usize).map(|i| (be32(s, 8 + i * 12), be32(s, 12 + i * 12))).collect()).unwrap_or_default();
    let mut deltas = Vec::with_capacity(sizes.len());
    if let Some(stts) = find(stbl, b"stts") {
        for i in 0..be32(stts, 4) as usize {
            let (n, d) = (be32(stts, 8 + i * 8), be32(stts, 12 + i * 8));
            for _ in 0..n.min(sizes.len() as u32 + 1) {
                deltas.push(d);
            }
        }
    }
    let mut ctts = Vec::new();
    if let Some(c) = find(stbl, b"ctts") {
        for i in 0..be32(c, 4) as usize {
            let (n, o) = (be32(c, 8 + i * 8), be32(c, 12 + i * 8) as i32);
            for _ in 0..n.min(sizes.len() as u32 + 1) {
                ctts.push(o);
            }
        }
    }
    let keys: Option<Vec<u32>> = find(stbl, b"stss").map(|s| (0..be32(s, 4) as usize).map(|i| be32(s, 8 + i * 4)).collect());
    let mut samples = Vec::with_capacity(sizes.len());
    let mut si = 0usize;
    let mut dts = 0u64;
    let mut ki = 0usize;
    for (ci, &base) in chunks.iter().enumerate() {
        let cn = ci as u32 + 1;
        let per = stsc.iter().rev().find(|e| e.0 <= cn).map(|e| e.1).unwrap_or(1);
        let mut off = base;
        for _ in 0..per {
            if si >= sizes.len() {
                break;
            }
            let key = match &keys {
                None => true,
                Some(k) => {
                    while ki < k.len() && (k[ki] as usize) < si + 1 {
                        ki += 1;
                    }
                    ki < k.len() && k[ki] as usize == si + 1
                }
            };
            samples.push(Sample { offset: off, size: sizes[si], dts, cts: ctts.get(si).copied().unwrap_or(0), key });
            off += sizes[si] as u64;
            dts += *deltas.get(si).unwrap_or(&deltas.last().copied().unwrap_or(1)) as u64;
            si += 1;
        }
    }
    tr.samples = samples;
    Some(tr)
}

impl Movie {
    /// Find the moov box (anywhere at the top level) and read it.
    pub fn parse(read: &mut ReadFn, file_len: u64) -> Result<Movie, &'static str> {
        let mut pos = 0u64;
        let mut moov = None;
        while pos + 8 <= file_len {
            let h = read(pos, 16).ok_or("Can't read the file")?;
            if h.len() < 8 {
                break;
            }
            let mut size = be32(&h, 0) as u64;
            let kind = [h[4], h[5], h[6], h[7]];
            let mut hdr = 8;
            if size == 1 && h.len() >= 16 {
                size = be64(&h, 8);
                hdr = 16;
            } else if size == 0 {
                size = file_len - pos;
            }
            if size < hdr {
                return Err("Damaged video file");
            }
            if &kind == b"moov" {
                if size > 256 << 20 {
                    return Err("Video index too big");
                }
                moov = read(pos + hdr, (size - hdr) as usize);
                break;
            }
            pos += size;
        }
        let moov = moov.ok_or("Not an MP4 video (or not finished recording)")?;
        let mut m = Movie { video: None, audio: None };
        for b in boxes(&moov) {
            if &b.0 != b"trak" {
                continue;
            }
            if let Some(t) = parse_trak(&moov[b.1..b.2]) {
                if t.video && m.video.is_none() {
                    m.video = Some(t);
                } else if !t.video && m.audio.is_none() {
                    m.audio = Some(t);
                }
            }
        }
        if m.video.is_none() && m.audio.is_none() {
            return Err("No pictures or sound in this file");
        }
        Ok(m)
    }

    pub fn duration_ms(&self) -> u64 {
        let d = |t: &Track| t.duration * 1000 / t.timescale.max(1) as u64;
        self.video.as_ref().map(d).or(self.audio.as_ref().map(d)).unwrap_or(0)
    }
}

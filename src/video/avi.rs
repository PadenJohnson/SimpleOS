//! Readers for RIFF files: AVI videos with Motion-JPEG pictures (digital cameras,
//! camcorders, webcams) and PCM sound, and WAV sound files.
//!
//! Only the index is read up front; frames and sound are fetched as needed through a
//! `read(offset, len)` callback, so big files never have to fit in memory.

use alloc::vec;
use alloc::vec::Vec;

pub type ReadFn<'a> = dyn FnMut(u64, usize) -> Option<Vec<u8>> + 'a;

#[derive(Clone, Debug)]
pub struct Sound {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
    /// Where the sound bytes are: (file offset, length) runs, in order.
    pub runs: Vec<(u64, u32)>,
    pub bytes: u64,
}

impl Sound {
    pub fn frame_bytes(&self) -> u64 {
        (self.channels as u64 * self.bits as u64 / 8).max(1)
    }
    pub fn frames(&self) -> u64 {
        self.bytes / self.frame_bytes()
    }

    /// Sound frames `[from, from + n)` as 48 kHz-independent interleaved stereo i16
    /// at the file's own rate (mono is doubled, 8-bit widened).
    pub fn read_frames(&self, read: &mut ReadFn, from: u64, n: usize) -> Vec<i16> {
        let fb = self.frame_bytes();
        let mut out = Vec::with_capacity(n * 2);
        let mut want_start = from * fb;
        let want_end = (from + n as u64) * fb;
        let mut pos = 0u64; // byte position in the sound stream at the start of each run
        for &(off, len) in &self.runs {
            let run_end = pos + len as u64;
            if run_end > want_start && pos < want_end {
                let a = want_start.max(pos);
                let b = want_end.min(run_end);
                if let Some(data) = read(off + (a - pos), (b - a) as usize) {
                    convert(&data, self.channels, self.bits, &mut out);
                }
                want_start = b;
            }
            pos = run_end;
            if pos >= want_end {
                break;
            }
        }
        out.resize(n * 2, 0);
        out
    }
}

fn convert(data: &[u8], channels: u16, bits: u16, out: &mut Vec<i16>) {
    let bps = (bits / 8).max(1) as usize;
    let fb = bps * channels.max(1) as usize;
    for f in data.chunks_exact(fb) {
        let sample = |c: usize| -> i16 {
            let s = &f[c * bps..c * bps + bps];
            match bps {
                1 => ((s[0] as i16) - 128) << 8,
                2 => i16::from_le_bytes([s[0], s[1]]),
                3 => i16::from_le_bytes([s[1], s[2]]),
                _ => i16::from_le_bytes([s[2], s[3]]),
            }
        };
        let l = sample(0);
        let r = if channels >= 2 { sample(1) } else { l };
        out.push(l);
        out.push(r);
    }
}

#[derive(Clone)]
pub struct Avi {
    pub width: u32,
    pub height: u32,
    /// Frames per second = rate / scale.
    pub rate: u32,
    pub scale: u32,
    /// (file offset, length) of each frame's JPEG data. Length 0 = repeat previous frame.
    pub frames: Vec<(u64, u32)>,
    pub sound: Option<Sound>,
}

fn u16le(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
fn u32le(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
fn u64le(b: &[u8], o: usize) -> u64 {
    u32le(b, o) as u64 | (u32le(b, o + 4) as u64) << 32
}

struct Stream {
    kind: [u8; 4],
    handler: [u8; 4],
    scale: u32,
    rate: u32,
    fmt: Vec<u8>,
    /// OpenDML super index entries: (offset of an ix## chunk, its size).
    super_index: Vec<(u64, u32)>,
}

impl Avi {
    pub fn duration_ms(&self) -> u64 {
        self.frames.len() as u64 * 1000 * self.scale as u64 / self.rate.max(1) as u64
    }

    pub fn parse(read: &mut ReadFn, file_len: u64) -> Result<Avi, &'static str> {
        let head = read(0, 12).ok_or("Can't read the file")?;
        if head.len() < 12 || &head[0..4] != b"RIFF" || &head[8..12] != b"AVI " {
            return Err("Not an AVI video");
        }
        // Walk the top level: hdrl, movi, idx1, and any further RIFF AVIX parts.
        let mut streams: Vec<Stream> = Vec::new();
        let (mut width, mut height) = (0, 0);
        let mut movis: Vec<(u64, u64)> = Vec::new(); // (start of data after "movi", end)
        let mut idx1: Option<Vec<u8>> = None;
        let mut pos = 12u64;
        let mut riff_end = (8 + u32le(&head, 4) as u64).min(file_len);
        let mut guard = 0;
        while pos + 8 <= file_len && guard < 100_000 {
            guard += 1;
            if pos + 8 > riff_end {
                // Next RIFF (OpenDML "AVIX" continuation).
                let h = read(pos, 12).ok_or("Can't read the file")?;
                if h.len() < 12 || &h[0..4] != b"RIFF" {
                    break;
                }
                riff_end = (pos + 8 + u32le(&h, 4) as u64).min(file_len);
                pos += 12;
                continue;
            }
            let h = read(pos, 12).ok_or("Can't read the file")?;
            if h.len() < 8 {
                break;
            }
            let id = [h[0], h[1], h[2], h[3]];
            let size = u32le(&h, 4) as u64;
            let body = pos + 8;
            if &id == b"LIST" && h.len() >= 12 {
                let kind = [h[8], h[9], h[10], h[11]];
                if &kind == b"hdrl" {
                    let data = read(body + 4, (size.saturating_sub(4)).min(4 << 20) as usize).ok_or("Can't read the file")?;
                    parse_hdrl(&data, &mut streams, &mut width, &mut height);
                } else if &kind == b"movi" {
                    movis.push((body + 4, (body + size).min(file_len)));
                }
            } else if &id == b"idx1" {
                idx1 = read(body, size.min(64 << 20) as usize);
            }
            pos = body + size + (size & 1);
        }
        let vi = streams.iter().position(|s| &s.kind == b"vids").ok_or("This video has no pictures")?;
        let v = &streams[vi];
        let mjpeg = matches!(&v.handler.to_ascii_uppercase()[..], b"MJPG" | b"AVRN" | b"LJPG" | b"JPGL" | b"DMB1" | b"MJPA")
            || (v.fmt.len() >= 20 && matches!(&v.fmt[16..20].to_ascii_uppercase()[..], b"MJPG" | b"AVRN" | b"LJPG" | b"JPGL" | b"DMB1" | b"MJPA"));
        if !mjpeg {
            return Err("This video's format isn't supported yet");
        }
        if width == 0 && v.fmt.len() >= 12 {
            width = u32le(&v.fmt, 4);
            height = (u32le(&v.fmt, 8) as i32).unsigned_abs();
        }
        let ai = streams.iter().position(|s| &s.kind == b"auds" && s.fmt.len() >= 16 && (u16le(&s.fmt, 0) == 1 || u16le(&s.fmt, 0) == 0xFFFE));
        let vtag = stream_tag(vi, b"dc");
        let vtag2 = stream_tag(vi, b"db");
        let atag = ai.map(|i| stream_tag(i, b"wb"));
        let mut frames = Vec::new();
        let mut runs = Vec::new();
        // 1. OpenDML indexes.
        if !v.super_index.is_empty() {
            read_odml(read, &v.super_index, &mut frames);
            if let Some(i) = ai {
                read_odml(read, &streams[i].super_index, &mut runs);
            }
        }
        // 2. The old index.
        if frames.is_empty() {
            if let (Some(idx), Some(&(movi, _))) = (&idx1, movis.first()) {
                // Offsets count from the "movi" word, or (some writers) from the file start.
                let base = match idx.get(8..12).map(|b| u32le(b, 0) as u64) {
                    Some(first) if first >= movi => 0,
                    _ => movi - 4,
                };
                for e in idx.chunks_exact(16) {
                    let tag = [e[0], e[1], e[2], e[3]];
                    let off = base + u32le(e, 8) as u64 + 8;
                    let len = u32le(e, 12);
                    if tag == vtag || tag == vtag2 {
                        frames.push((off, len));
                    } else if Some(tag) == atag && len > 0 {
                        runs.push((off, len));
                    }
                }
            }
        }
        // 3. No index: walk the chunks.
        if frames.is_empty() {
            for &(start, end) in &movis {
                let mut p = start;
                let mut n = 0;
                while p + 8 <= end && n < 2_000_000 {
                    n += 1;
                    let Some(h) = read(p, 12) else { break };
                    if h.len() < 8 {
                        break;
                    }
                    let tag = [h[0], h[1], h[2], h[3]];
                    let size = u32le(&h, 4) as u64;
                    if &tag == b"LIST" {
                        p += 12; // "rec " lists: step inside
                        continue;
                    }
                    if tag == vtag || tag == vtag2 {
                        frames.push((p + 8, size as u32));
                    } else if Some(tag) == atag && size > 0 {
                        runs.push((p + 8, size as u32));
                    }
                    p += 8 + size + (size & 1);
                }
            }
        }
        if frames.is_empty() {
            return Err("This video has no pictures");
        }
        let sound = ai.map(|i| {
            let f = &streams[i].fmt;
            let bytes = runs.iter().map(|r| r.1 as u64).sum();
            Sound { rate: u32le(f, 4), channels: u16le(f, 2), bits: u16le(f, 14), runs, bytes }
        });
        let (rate, scale) = if v.rate > 0 && v.scale > 0 { (v.rate, v.scale) } else { (30, 1) };
        Ok(Avi { width, height, rate, scale, frames, sound })
    }

    /// The JPEG for frame `i`, ready to decode (with standard Huffman tables added if the
    /// camera left them out, as Motion-JPEG allows). Empty frames repeat the one before.
    pub fn frame_jpeg(&self, read: &mut ReadFn, i: usize) -> Option<Vec<u8>> {
        let mut k = i.min(self.frames.len() - 1);
        while self.frames[k].1 == 0 && k > 0 {
            k -= 1;
        }
        let (off, len) = self.frames[k];
        let data = read(off, len as usize)?;
        Some(with_huffman_tables(data))
    }
}

fn stream_tag(i: usize, kind: &[u8; 2]) -> [u8; 4] {
    [b'0' + (i / 10) as u8, b'0' + (i % 10) as u8, kind[0], kind[1]]
}

fn parse_hdrl(d: &[u8], streams: &mut Vec<Stream>, width: &mut u32, height: &mut u32) {
    let mut p = 0;
    while p + 8 <= d.len() {
        let id = &d[p..p + 4];
        let size = u32le(d, p + 4) as usize;
        let body = p + 8;
        let end = (body + size).min(d.len());
        if id == b"avih" && end - body >= 40 {
            *width = u32le(d, body + 32);
            *height = u32le(d, body + 36);
        } else if id == b"LIST" && end - body >= 4 && &d[body..body + 4] == b"strl" {
            let mut s = Stream { kind: [0; 4], handler: [0; 4], scale: 0, rate: 0, fmt: Vec::new(), super_index: Vec::new() };
            let mut q = body + 4;
            while q + 8 <= end {
                let cid = &d[q..q + 4];
                let csize = u32le(d, q + 4) as usize;
                let cb = q + 8;
                let ce = (cb + csize).min(end);
                if cid == b"strh" && ce - cb >= 28 {
                    s.kind.copy_from_slice(&d[cb..cb + 4]);
                    s.handler.copy_from_slice(&d[cb + 4..cb + 8]);
                    s.scale = u32le(d, cb + 20);
                    s.rate = u32le(d, cb + 24);
                } else if cid == b"strf" {
                    s.fmt = d[cb..ce].to_vec();
                } else if cid == b"indx" && ce - cb >= 24 {
                    // AVISUPERINDEX: longs per entry, type 0 = index of indexes.
                    let entries = u32le(d, cb + 4) as usize;
                    if d[cb + 3] == 0 {
                        for k in 0..entries {
                            let e = cb + 24 + k * 16;
                            if e + 16 > ce {
                                break;
                            }
                            s.super_index.push((u64le(d, e), u32le(d, e + 8)));
                        }
                    }
                }
                q = cb + csize + (csize & 1);
            }
            streams.push(s);
        }
        p = body + size + (size & 1);
    }
}

/// OpenDML standard index chunks ("ix00"): each entry is an offset from the chunk's base
/// straight to the data, and a size (top bit = not a key frame).
fn read_odml(read: &mut ReadFn, supers: &[(u64, u32)], out: &mut Vec<(u64, u32)>) {
    for &(off, size) in supers {
        let Some(ix) = read(off, size as usize) else { continue };
        if ix.len() < 32 {
            continue;
        }
        let body = &ix[8..];
        let per = body[0] as usize * 4; // longs per entry -> bytes
        let entries = u32le(body, 4) as usize;
        let base = u64le(body, 12);
        let per = per.max(8);
        for k in 0..entries {
            let e = 24 + k * per;
            if e + 8 > body.len() {
                break;
            }
            out.push((base + u32le(body, e) as u64, u32le(body, e + 4) & 0x7FFF_FFFF));
        }
    }
}

// ---- Motion-JPEG's missing Huffman tables ----------------------------------------------

const DC_LUM_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_CHR_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const AC_LUM_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7D];
const AC_CHR_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const AC_LUM_HEAD: [u8; 33] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07, 0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24,
];
const AC_CHR_HEAD: [u8; 43] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71, 0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1,
];

/// The rest of the AC tables are runs of consecutive symbols.
fn ac_values(head: &[u8], runs: &[(u8, u8)]) -> Vec<u8> {
    let mut v = head.to_vec();
    for &(a, b) in runs {
        v.extend(a..=b);
    }
    v
}

pub fn standard_dht() -> Vec<u8> {
    let ac_lum = ac_values(
        &AC_LUM_HEAD,
        &[
            (0x33, 0x33),
            (0x62, 0x62),
            (0x72, 0x72),
            (0x82, 0x82),
            (0x09, 0x0A),
            (0x16, 0x1A),
            (0x25, 0x2A),
            (0x34, 0x3A),
            (0x43, 0x4A),
            (0x53, 0x5A),
            (0x63, 0x6A),
            (0x73, 0x7A),
            (0x83, 0x8A),
            (0x92, 0x9A),
            (0xA2, 0xAA),
            (0xB2, 0xBA),
            (0xC2, 0xCA),
            (0xD2, 0xDA),
            (0xE1, 0xEA),
            (0xF1, 0xFA),
        ],
    );
    let ac_chr = ac_values(
        &AC_CHR_HEAD,
        &[
            (0x17, 0x1A),
            (0x26, 0x2A),
            (0x35, 0x3A),
            (0x43, 0x4A),
            (0x53, 0x5A),
            (0x63, 0x6A),
            (0x73, 0x7A),
            (0x82, 0x8A),
            (0x92, 0x9A),
            (0xA2, 0xAA),
            (0xB2, 0xBA),
            (0xC2, 0xCA),
            (0xD2, 0xDA),
            (0xE2, 0xEA),
            (0xF2, 0xFA),
        ],
    );
    let dc: Vec<u8> = (0..12).collect();
    let tables: [(u8, &[u8; 16], &[u8]); 4] = [(0x00, &DC_LUM_BITS, &dc), (0x10, &AC_LUM_BITS, &ac_lum), (0x01, &DC_CHR_BITS, &dc), (0x11, &AC_CHR_BITS, &ac_chr)];
    let mut body = Vec::new();
    for (class, bits, vals) in tables {
        debug_assert_eq!(bits.iter().map(|&b| b as usize).sum::<usize>(), vals.len());
        body.push(class);
        body.extend_from_slice(bits);
        body.extend_from_slice(vals);
    }
    let mut seg = vec![0xFF, 0xC4];
    seg.extend_from_slice(&((body.len() + 2) as u16).to_be_bytes());
    seg.extend_from_slice(&body);
    seg
}

/// Add the standard tables after SOI if the JPEG has no DHT segment before its scan.
pub fn with_huffman_tables(data: Vec<u8>) -> Vec<u8> {
    if data.len() < 4 || data[0] != 0xFF || data[1] != 0xD8 {
        return data;
    }
    let mut p = 2;
    while p + 4 <= data.len() {
        if data[p] != 0xFF {
            break;
        }
        let m = data[p + 1];
        if m == 0xC4 {
            return data; // has its own tables
        }
        if m == 0xDA {
            break;
        }
        let len = u16::from_be_bytes([data[p + 2], data[p + 3]]) as usize;
        p += 2 + len;
    }
    let mut out = Vec::with_capacity(data.len() + 432);
    out.extend_from_slice(&data[..2]);
    out.extend_from_slice(&standard_dht());
    out.extend_from_slice(&data[2..]);
    out
}

// ---- WAV ----------------------------------------------------------------------------------

/// A WAV file's sound (PCM 8/16/24/32-bit, mono or stereo).
pub fn parse_wav(read: &mut ReadFn, file_len: u64) -> Result<Sound, &'static str> {
    let head = read(0, 12).ok_or("Can't read the file")?;
    if head.len() < 12 || &head[0..4] != b"RIFF" || &head[8..12] != b"WAVE" {
        return Err("Not a WAV sound file");
    }
    let mut pos = 12u64;
    let mut fmt: Option<Vec<u8>> = None;
    while pos + 8 <= file_len {
        let h = read(pos, 8).ok_or("Can't read the file")?;
        if h.len() < 8 {
            break;
        }
        let size = u32le(&h, 4) as u64;
        if &h[0..4] == b"fmt " {
            fmt = read(pos + 8, size.min(64) as usize);
        } else if &h[0..4] == b"data" {
            let f = fmt.ok_or("Damaged WAV file")?;
            if f.len() < 16 {
                return Err("Damaged WAV file");
            }
            let tag = u16le(&f, 0);
            if tag != 1 && tag != 0xFFFE {
                return Err("This WAV file's sound format isn't supported");
            }
            let len = size.min(file_len - pos - 8);
            // Split into runs that fit a u32 and a reasonable read size.
            let mut runs = Vec::new();
            let mut at = 0;
            while at < len {
                let n = (len - at).min(1 << 20);
                runs.push((pos + 8 + at, n as u32));
                at += n;
            }
            return Ok(Sound { rate: u32le(&f, 4), channels: u16le(&f, 2).max(1), bits: u16le(&f, 14), runs, bytes: len });
        }
        pos += 8 + size + (size & 1);
    }
    Err("Damaged WAV file")
}

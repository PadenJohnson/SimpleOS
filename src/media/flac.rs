//! FLAC (Free Lossless Audio Codec) decoding: frames of constant, verbatim, fixed-predictor
//! and LPC subframes with Rice-coded residuals, the three stereo decorrelations, 4 to 32
//! bits per sample, up to 8 channels (mixed down to stereo). Packets are whole frames (from
//! .flac files, Ogg or Matroska); `frame_len` finds where a frame ends in a .flac file.

use alloc::vec;
use alloc::vec::Vec;

use super::dec::AudioDec;

/// What STREAMINFO says (the first metadata block).
#[derive(Clone, Copy, Default)]
pub struct StreamInfo {
    pub min_block: u32,
    pub max_block: u32,
    pub min_frame: u32,
    pub max_frame: u32,
    pub rate: u32,
    pub channels: u32,
    pub bits: u32,
    /// Samples per channel in the whole stream (0 = unknown).
    pub total: u64,
}

impl StreamInfo {
    /// From the 34 bytes of a STREAMINFO block.
    pub fn parse(b: &[u8]) -> Option<StreamInfo> {
        if b.len() < 18 {
            return None;
        }
        let be = |o: usize, n: usize| b[o..o + n].iter().fold(0u64, |a, &x| a << 8 | x as u64);
        let v = be(10, 8);
        Some(StreamInfo {
            min_block: be(0, 2) as u32,
            max_block: be(2, 2) as u32,
            min_frame: be(4, 3) as u32,
            max_frame: be(7, 3) as u32,
            rate: (v >> 44) as u32,
            channels: ((v >> 41) & 7) as u32 + 1,
            bits: ((v >> 36) & 31) as u32 + 1,
            total: v & 0xF_FFFF_FFFF,
        })
    }
}

/// Reads bits, most significant first.
struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl<'a> Bits<'a> {
    fn bit(&mut self) -> Option<u32> {
        let byte = *self.d.get(self.pos >> 3)?;
        let b = (byte >> (7 - (self.pos & 7))) & 1;
        self.pos += 1;
        Some(b as u32)
    }
    fn get(&mut self, n: u32) -> Option<u64> {
        let mut v = 0u64;
        let mut n = n;
        // Whole bytes when lined up (most of a frame's header and verbatim data).
        while n > 0 {
            if self.pos & 7 == 0 && n >= 8 {
                v = v << 8 | *self.d.get(self.pos >> 3)? as u64;
                self.pos += 8;
                n -= 8;
            } else {
                v = v << 1 | self.bit()? as u64;
                n -= 1;
            }
        }
        Some(v)
    }
    fn signed(&mut self, n: u32) -> Option<i64> {
        if n == 0 {
            return Some(0);
        }
        let v = self.get(n)?;
        Some(((v << (64 - n)) as i64) >> (64 - n))
    }
    /// Zeros up to the next one (Rice quotients).
    fn unary(&mut self) -> Option<u32> {
        let mut n = 0;
        loop {
            let byte = *self.d.get(self.pos >> 3)?;
            let rest = byte << (self.pos & 7);
            if rest != 0 {
                let z = rest.leading_zeros();
                self.pos += z as usize + 1;
                return Some(n + z);
            }
            let skip = 8 - (self.pos & 7) as u32;
            n += skip;
            self.pos += skip as usize;
        }
    }
    fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }
}

const CRC8_POLY: u8 = 0x07;

fn crc8(d: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in d {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { c << 1 ^ CRC8_POLY } else { c << 1 };
        }
    }
    c
}

/// CRC-16 (polynomial 0x8005) of a frame, as in its footer.
pub fn crc16(d: &[u8]) -> u16 {
    static TABLE: spin::Once<[u16; 256]> = spin::Once::new();
    let t = TABLE.call_once(|| {
        let mut t = [0u16; 256];
        for (i, e) in t.iter_mut().enumerate() {
            let mut c = (i as u16) << 8;
            for _ in 0..8 {
                c = if c & 0x8000 != 0 { c << 1 ^ 0x8005 } else { c << 1 };
            }
            *e = c;
        }
        t
    });
    d.iter().fold(0u16, |c, &b| c << 8 ^ t[((c >> 8) as u8 ^ b) as usize])
}

/// A frame's header: block size, rate, channels and how they're stored, bits, and the
/// first sample's number (or the frame's number, with fixed block sizes).
#[derive(Clone, Copy)]
pub struct FrameHeader {
    pub block: u32,
    pub rate: u32,
    pub assign: u32,
    pub bits: u32,
    pub variable: bool,
    pub number: u64,
    /// Bytes in the header.
    pub len: usize,
}

impl FrameHeader {
    pub fn channels(&self) -> u32 {
        if self.assign < 8 {
            self.assign + 1
        } else {
            2
        }
    }
    /// The first sample's number, given the stream's fixed block size.
    pub fn first_sample(&self, fixed_block: u32) -> u64 {
        if self.variable {
            self.number
        } else {
            self.number * fixed_block.max(1) as u64
        }
    }
}

/// Parse (and check, by its CRC-8) a frame header at the start of `d`.
pub fn frame_header(d: &[u8], si: &StreamInfo) -> Option<FrameHeader> {
    if d.len() < 6 || d[0] != 0xFF || d[1] & 0xFE != 0xF8 {
        return None;
    }
    let variable = d[1] & 1 != 0;
    let bs_code = d[2] >> 4;
    let sr_code = d[2] & 15;
    let assign = (d[3] >> 4) as u32;
    let ss_code = (d[3] >> 1) & 7;
    if bs_code == 0 || sr_code == 15 || assign > 10 || ss_code == 3 || d[3] & 1 != 0 {
        return None;
    }
    // The coded number: UTF-8 style, up to 7 bytes.
    let first = d[4];
    let extra = match first.leading_ones() {
        0 => 0,
        n @ 2..=7 => n as usize - 1,
        _ => return None,
    };
    let mut number = if extra == 0 { first as u64 } else { (first & (0x7F >> (extra + 1))) as u64 };
    let mut p = 5;
    for _ in 0..extra {
        let b = *d.get(p)?;
        if b & 0xC0 != 0x80 {
            return None;
        }
        number = number << 6 | (b & 0x3F) as u64;
        p += 1;
    }
    let block = match bs_code {
        1 => 192,
        2..=5 => 576 << (bs_code - 2),
        6 => {
            p += 1;
            *d.get(p - 1)? as u32 + 1
        }
        7 => {
            p += 2;
            (*d.get(p - 2)? as u32) << 8 | *d.get(p - 1)? as u32 + 1
        }
        _ => 256 << (bs_code - 8),
    };
    let rate = match sr_code {
        0 => si.rate,
        1 => 88200,
        2 => 176400,
        3 => 192000,
        4 => 8000,
        5 => 16000,
        6 => 22050,
        7 => 24000,
        8 => 32000,
        9 => 44100,
        10 => 48000,
        11 => 96000,
        12 => {
            p += 1;
            *d.get(p - 1)? as u32 * 1000
        }
        13 => {
            p += 2;
            (*d.get(p - 2)? as u32) << 8 | *d.get(p - 1)? as u32
        }
        _ => {
            p += 2;
            ((*d.get(p - 2)? as u32) << 8 | *d.get(p - 1)? as u32) * 10
        }
    };
    let bits = match ss_code {
        0 => si.bits,
        1 => 8,
        2 => 12,
        4 => 16,
        5 => 20,
        6 => 24,
        _ => 32,
    };
    if crc8(d.get(..p)?) != *d.get(p)? || rate == 0 || bits == 0 {
        return None;
    }
    Some(FrameHeader { block, rate, assign, bits, variable, number, len: p + 1 })
}

/// Where the frame starting at `d[0]` ends, if the next frame's start is in `d` (checked by
/// the frame's CRC-16, so a sync pattern inside the audio data can't fool it). `at_end`: `d`
/// runs to the end of the file, so the frame may end with it.
pub fn frame_len(d: &[u8], si: &StreamInfo, at_end: bool) -> Option<usize> {
    let h = frame_header(d, si)?;
    let min = (si.min_frame as usize).max(h.len + 2);
    let mut i = min;
    while i + 2 <= d.len() {
        if d[i] == 0xFF && d[i + 1] & 0xFE == 0xF8 {
            let crc = u16::from_be_bytes([d[i - 2], d[i - 1]]);
            if crc16(&d[..i - 2]) == crc && frame_header(&d[i..], si).is_some() {
                return Some(i);
            }
        }
        i += 1;
    }
    if at_end && d.len() >= h.len + 2 {
        let n = d.len();
        if crc16(&d[..n - 2]) == u16::from_be_bytes([d[n - 2], d[n - 1]]) {
            return Some(n);
        }
    }
    None
}

pub struct FlacDec {
    si: StreamInfo,
    rate: u32,
    chans: Vec<Vec<i32>>,
}

impl FlacDec {
    /// `info`: the STREAMINFO block's 34 bytes (or empty: frames carry what's needed).
    pub fn new(info: &[u8]) -> FlacDec {
        let si = StreamInfo::parse(info).unwrap_or_default();
        FlacDec { si, rate: si.rate.max(1), chans: Vec::new() }
    }

    fn subframe(b: &mut Bits, block: usize, bits: u32, out: &mut Vec<i32>) -> Option<()> {
        out.clear();
        if b.bit()? != 0 {
            return None;
        }
        let kind = b.get(6)? as u32;
        let mut wasted = 0;
        if b.bit()? == 1 {
            wasted = b.unary()? + 1;
        }
        let bits = bits.checked_sub(wasted)?;
        match kind {
            0 => {
                let v = b.signed(bits)? as i32;
                out.resize(block, v);
            }
            1 => {
                for _ in 0..block {
                    out.push(b.signed(bits)? as i32);
                }
            }
            8..=12 => {
                let order = (kind - 8) as usize;
                for _ in 0..order {
                    out.push(b.signed(bits)? as i32);
                }
                Self::residual(b, block, order, out)?;
                let s = out.as_mut_slice();
                for i in order..block {
                    let r = s[i] as i64;
                    let p = match order {
                        0 => 0,
                        1 => s[i - 1] as i64,
                        2 => 2 * s[i - 1] as i64 - s[i - 2] as i64,
                        3 => 3 * s[i - 1] as i64 - 3 * s[i - 2] as i64 + s[i - 3] as i64,
                        _ => 4 * s[i - 1] as i64 - 6 * s[i - 2] as i64 + 4 * s[i - 3] as i64 - s[i - 4] as i64,
                    };
                    s[i] = (p + r) as i32;
                }
            }
            32..=63 => {
                let order = (kind - 31) as usize;
                for _ in 0..order {
                    out.push(b.signed(bits)? as i32);
                }
                let precision = b.get(4)? as u32 + 1;
                if precision == 16 {
                    return None;
                }
                let shift = b.signed(5)?;
                if shift < 0 {
                    return None;
                }
                let mut coef = [0i64; 32];
                for c in coef.iter_mut().take(order) {
                    *c = b.signed(precision)?;
                }
                Self::residual(b, block, order, out)?;
                let s = out.as_mut_slice();
                for i in order..block {
                    let mut sum = 0i64;
                    for j in 0..order {
                        sum += coef[j] * s[i - 1 - j] as i64;
                    }
                    s[i] = (s[i] as i64 + (sum >> shift)) as i32;
                }
            }
            _ => return None,
        }
        if out.len() != block {
            return None;
        }
        if wasted > 0 {
            out.iter_mut().for_each(|v| *v <<= wasted);
        }
        Some(())
    }

    /// The residual after `order` warm-up samples, appended to `out`.
    fn residual(b: &mut Bits, block: usize, order: usize, out: &mut Vec<i32>) -> Option<()> {
        let method = b.get(2)?;
        if method > 1 {
            return None;
        }
        let (pbits, escape) = if method == 0 { (4, 15) } else { (5, 31) };
        let porder = b.get(4)? as u32;
        let parts = 1usize << porder;
        if block % parts != 0 || (block >> porder) < order {
            return None;
        }
        for p in 0..parts {
            let n = (block >> porder) - if p == 0 { order } else { 0 };
            let k = b.get(pbits)? as u32;
            if k == escape {
                let raw = b.get(5)? as u32;
                for _ in 0..n {
                    out.push(b.signed(raw)? as i32);
                }
            } else {
                for _ in 0..n {
                    let q = b.unary()? as u64;
                    let v = q << k | b.get(k)?;
                    out.push(((v >> 1) as i64 ^ -((v & 1) as i64)) as i32);
                }
            }
        }
        Some(())
    }

    fn frame(&mut self, d: &[u8]) -> Option<Vec<i16>> {
        let h = frame_header(d, &self.si)?;
        self.rate = h.rate;
        let block = h.block as usize;
        let n = h.channels() as usize;
        let mut b = Bits { d, pos: h.len * 8 };
        self.chans.resize_with(n, Vec::new);
        for c in 0..n {
            // The side channel has one bit more.
            let extra = match (h.assign, c) {
                (8, 1) | (9, 0) | (10, 1) => 1,
                _ => 0,
            };
            let mut v = core::mem::take(&mut self.chans[c]);
            let ok = Self::subframe(&mut b, block, h.bits + extra, &mut v);
            self.chans[c] = v;
            ok?;
        }
        b.align();
        match h.assign {
            8 => {
                let (l, s) = self.chans.split_at_mut(1);
                for i in 0..block {
                    s[0][i] = l[0][i].wrapping_sub(s[0][i]);
                }
            }
            9 => {
                let (s, r) = self.chans.split_at_mut(1);
                for i in 0..block {
                    s[0][i] = s[0][i].wrapping_add(r[0][i]);
                }
            }
            10 => {
                let (m, s) = self.chans.split_at_mut(1);
                for i in 0..block {
                    let side = s[0][i] as i64;
                    let mid = (m[0][i] as i64) << 1 | (side & 1);
                    m[0][i] = ((mid + side) >> 1) as i32;
                    s[0][i] = ((mid - side) >> 1) as i32;
                }
            }
            _ => {}
        }
        // To 16 bits, mixed down to stereo.
        let shift = h.bits as i32 - 16;
        let to16 = |v: i32| -> i32 {
            if shift > 0 {
                (v + (1 << (shift - 1))) >> shift
            } else {
                v << -shift
            }
        };
        let mut out = vec![0i16; block * 2];
        let c = &self.chans;
        for i in 0..block {
            let (l, r) = match n {
                1 => (to16(c[0][i]), to16(c[0][i])),
                2 => (to16(c[0][i]), to16(c[1][i])),
                _ => {
                    // Front left/right, centre (and back/side pairs at half strength); the
                    // low-frequency channel (4th) is left out.
                    let mut l = to16(c[0][i]) as i64 * 2;
                    let mut r = to16(c[1][i]) as i64 * 2;
                    if n >= 3 {
                        let ctr = to16(c[2][i]) as i64;
                        l += ctr;
                        r += ctr;
                    }
                    let mut k = 4;
                    while k + 1 < n {
                        l += to16(c[k][i]) as i64;
                        r += to16(c[k + 1][i]) as i64;
                        k += 2;
                    }
                    ((l / 3) as i32, (r / 3) as i32)
                }
            };
            out[i * 2] = l.clamp(-32768, 32767) as i16;
            out[i * 2 + 1] = r.clamp(-32768, 32767) as i16;
        }
        Some(out)
    }
}

impl AudioDec for FlacDec {
    fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        self.frame(data).unwrap_or_default()
    }
    fn rate(&self) -> u32 {
        self.rate
    }
    fn reset(&mut self) {}
}

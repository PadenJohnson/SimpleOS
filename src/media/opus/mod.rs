//! Opus sound (RFC 6716), as in WebM and MKV: packet framing, SILK + CELT + hybrid frames,
//! SILK upsampled to 48 kHz, redundancy frames, multistream (surround) packets.
//! Structure follows FFmpeg's opus/dec.c (LGPL 2.1 or later).

mod celt;
mod mdct;
mod rc;
mod silk;
mod tab;

use crate::media::dec::AudioDec;
use alloc::vec;
use alloc::vec::Vec;
use celt::Celt;
use rc::Rc;
use silk::Silk;
use tab::{CELT_BAND_END, CELT_WINDOW2, FRAME_DURATION};

const MODE_SILK: u8 = 0;
const MODE_HYBRID: u8 = 1;
const MODE_CELT: u8 = 2;

const SILK_FRAME_MS: [usize; 16] = [10, 20, 40, 60, 10, 20, 40, 60, 10, 20, 40, 60, 10, 20, 10, 20];
/// SILK delay (input samples) per bandwidth, as FFmpeg feeds its resampler.
const SILK_DELAY: [usize; 5] = [4, 8, 11, 11, 11];

struct Packet {
    config: usize,
    stereo: bool,
    frames: Vec<(usize, usize)>,
    size: usize,
    dur: usize,
    mode: u8,
    bw: usize,
}

fn lacing16(d: &[u8], p: &mut usize) -> Option<usize> {
    let mut v = *d.get(*p)? as usize;
    *p += 1;
    if v >= 252 {
        v += 4 * *d.get(*p)? as usize;
        *p += 1;
    }
    Some(v)
}

fn parse_packet(d: &[u8], self_delim: bool) -> Option<Packet> {
    let toc = *d.first()?;
    let code = toc & 3;
    let stereo = toc & 4 != 0;
    let config = (toc >> 3) as usize;
    let mut p = 1usize;
    let mut end = d.len();
    let mut padding = 0;
    let mut frames = Vec::new();
    match code {
        0 => {
            if self_delim {
                let len = lacing16(d, &mut p)?;
                if len > end - p {
                    return None;
                }
                end = p + len;
            }
            frames.push((p, end - p));
        }
        1 => {
            if self_delim {
                let len = lacing16(d, &mut p)?;
                if 2 * len > end - p {
                    return None;
                }
                end = p + 2 * len;
            }
            let n = end - p;
            if n & 1 != 0 {
                return None;
            }
            frames.push((p, n / 2));
            frames.push((p + n / 2, n / 2));
        }
        2 => {
            let first = lacing16(d, &mut p)?;
            if self_delim {
                let len = lacing16(d, &mut p)?;
                if len + first > end - p {
                    return None;
                }
                end = p + first + len;
            }
            if first > end - p {
                return None;
            }
            frames.push((p, first));
            frames.push((p + first, end - p - first));
        }
        _ => {
            let b = *d.get(p)?;
            p += 1;
            let count = (b & 0x3F) as usize;
            let vbr = b & 0x80 != 0;
            if count == 0 || count > 48 {
                return None;
            }
            if b & 0x40 != 0 {
                loop {
                    let n = *d.get(p)? as usize;
                    p += 1;
                    padding += if n == 255 { 254 } else { n };
                    if n < 255 {
                        break;
                    }
                }
            }
            if vbr {
                let mut sizes = Vec::new();
                let mut total = 0;
                for _ in 0..count - 1 {
                    let s = lacing16(d, &mut p)?;
                    sizes.push(s);
                    total += s;
                }
                if self_delim {
                    let len = lacing16(d, &mut p)?;
                    if len + total + padding > end - p {
                        return None;
                    }
                    end = p + total + len + padding;
                }
                let fb = (end - p).checked_sub(padding)?;
                if total > fb {
                    return None;
                }
                sizes.push(fb - total);
                let mut o = p;
                for s in sizes {
                    frames.push((o, s));
                    o += s;
                }
            } else {
                let fb = if self_delim {
                    let fb = lacing16(d, &mut p)?;
                    if count * fb + padding > end - p {
                        return None;
                    }
                    end = p + count * fb + padding;
                    fb
                } else {
                    let total = (end - p).checked_sub(padding)?;
                    if total % count != 0 {
                        return None;
                    }
                    total / count
                };
                for k in 0..count {
                    frames.push((p + k * fb, fb));
                }
            }
        }
    }
    let dur = FRAME_DURATION[config];
    if dur * frames.len() > 5760 {
        return None;
    }
    let (mode, bw) = if config < 12 {
        (MODE_SILK, config >> 2)
    } else if config < 16 {
        (MODE_HYBRID, 3 + (config >= 14) as usize)
    } else {
        let b = (config - 16) >> 2;
        (MODE_CELT, if b != 0 { b + 1 } else { 0 })
    };
    Some(Packet { config, stereo, frames, size: end, dur, mode, bw })
}

fn bessel_i0(x: f64) -> f64 {
    let (mut sum, mut term) = (1.0, 1.0);
    for k in 1..30 {
        term *= (x / 2.0) * (x / 2.0) / (k * k) as f64;
        sum += term;
    }
    sum
}

/// SILK to 48 kHz: integer-ratio windowed-sinc filter with no lookahead, so the SILK sound
/// comes out `delay` input samples late (the delay FFmpeg's decoder gives it).
struct Resampler {
    rate: u32,
    r: usize,
    taps: Vec<Vec<f32>>,
    hist: [Vec<f32>; 2],
}

impl Resampler {
    fn new(rate: u32, delay: usize) -> Resampler {
        let r = (48000 / rate) as usize;
        let s = delay;
        let beta = 9.0;
        let taps = (0..r)
            .map(|p| {
                let mut t: Vec<f32> = (0..=2 * s)
                    .map(|j| {
                        let tau = p as f64 / r as f64 - s as f64 + j as f64;
                        let x = tau / s as f64;
                        if x.abs() >= 1.0 {
                            return 0.0;
                        }
                        let fc = 0.97;
                        let sinc = if tau == 0.0 { fc } else { libm::sin(core::f64::consts::PI * tau * fc) / (core::f64::consts::PI * tau) };
                        (sinc * bessel_i0(beta * libm::sqrt(1.0 - x * x)) / bessel_i0(beta)) as f32
                    })
                    .collect();
                let sum: f32 = t.iter().sum();
                if sum != 0.0 {
                    t.iter_mut().for_each(|v| *v /= sum);
                }
                t
            })
            .collect();
        Resampler { rate, r, taps, hist: [vec![0.0; 2 * s], vec![0.0; 2 * s]] }
    }

    fn run(&mut self, ch: usize, input: &[f32], out: &mut [f32]) {
        let h = self.hist[ch].len();
        let mut x = Vec::with_capacity(h + input.len());
        x.extend_from_slice(&self.hist[ch]);
        x.extend_from_slice(input);
        for (m, o) in out.iter_mut().enumerate().take(input.len() * self.r) {
            let q = m / self.r;
            let t = &self.taps[m % self.r];
            // Taps j = 0..=2s use input sample q - j (q relative to the frame start).
            let mut acc = 0f32;
            for (j, &c) in t.iter().enumerate() {
                let k = h as isize + q as isize - j as isize;
                if k >= 0 {
                    acc += c * x[k as usize];
                }
            }
            *o = acc;
        }
        let n = x.len();
        self.hist[ch].copy_from_slice(&x[n - h..]);
    }
}

struct Stream {
    out_ch: usize,
    silk: Silk,
    celt: Celt,
    rs: Option<Resampler>,
}

impl Stream {
    fn new(out_ch: usize) -> Stream {
        Stream { out_ch, silk: Silk::new(out_ch), celt: Celt::new(out_ch), rs: None }
    }

    fn reset(&mut self) {
        self.silk.flush();
        self.celt.flush();
        self.rs = None;
    }

    fn redundancy(&mut self, data: &[u8], pkt: &Packet, out: &mut [Vec<f32>]) -> Option<()> {
        let mut rc = Rc::new(data);
        rc.raw_init(data.len(), data.len() as u32);
        self.celt.decode(&mut rc, out, pkt.stereo as usize + 1, 240, 0, CELT_BAND_END[pkt.bw] as usize)
    }

    /// One frame into `out` (per channel, `pkt.dur` samples at 48 kHz).
    fn frame(&mut self, data: &[u8], pkt: &Packet, out: &mut [Vec<f32>]) -> Option<()> {
        let dur = pkt.dur;
        let mut size = data.len();
        let mut rc = Rc::new(data);
        for o in out.iter_mut() {
            o[..dur].iter_mut().for_each(|v| *v = 0.0);
        }
        if pkt.mode != MODE_CELT {
            let rate = if pkt.config < 4 {
                8000
            } else if pkt.config < 8 {
                12000
            } else {
                16000
            };
            if self.rs.as_ref().map(|r| r.rate) != Some(rate) {
                self.rs = Some(Resampler::new(rate, SILK_DELAY[pkt.bw]));
            }
            let mut silk_out = vec![vec![0f32; 960]; self.out_ch];
            let n = self.silk.decode(&mut rc, &mut silk_out, pkt.bw.min(2), pkt.stereo as usize + 1, SILK_FRAME_MS[pkt.config]);
            let rs = self.rs.as_mut()?;
            for (c, o) in out.iter_mut().enumerate() {
                rs.run(c, &silk_out[c][..n], &mut o[..dur]);
            }
        } else {
            self.silk.flush();
            self.rs = None;
        }
        let consumed = rc.tell();
        let mut redundancy = false;
        if pkt.mode == MODE_HYBRID && consumed + 37 <= size as i32 * 8 {
            redundancy = rc.log(12) != 0;
        } else if pkt.mode == MODE_SILK && consumed + 17 <= size as i32 * 8 {
            redundancy = true;
        }
        let mut red_pos = false;
        let mut red_size = 0;
        let mut red = vec![vec![0f32; 240]; self.out_ch];
        if redundancy {
            red_pos = rc.log(1) != 0;
            red_size = if pkt.mode == MODE_HYBRID { rc.uint(256) as usize + 2 } else { size - (consumed as usize + 7) / 8 };
            size = size.checked_sub(red_size)?;
            if red_pos {
                self.redundancy(&data[size..size + red_size], pkt, &mut red)?;
                self.celt.flush();
            }
        }
        if pkt.mode != MODE_SILK {
            rc.raw_init(size, size as u32);
            let mut celt_out = vec![vec![0f32; 960]; self.out_ch];
            self.celt.decode(&mut rc, &mut celt_out, pkt.stereo as usize + 1, dur, if pkt.mode == MODE_HYBRID { 17 } else { 0 }, CELT_BAND_END[pkt.bw] as usize)?;
            for (o, c) in out.iter_mut().zip(celt_out.iter()) {
                for i in 0..dur {
                    o[i] += c[i];
                }
            }
        } else {
            self.celt.flush();
        }
        if redundancy {
            if !red_pos {
                self.celt.flush();
                self.redundancy(&data[size..size + red_size], pkt, &mut red)?;
                for (o, r) in out.iter_mut().zip(red.iter()) {
                    for i in 0..120 {
                        let w = CELT_WINDOW2[i];
                        let at = dur - 120 + i;
                        o[at] = r[120 + i] * w + o[at] * (1.0 - w);
                    }
                }
            } else {
                for (o, r) in out.iter_mut().zip(red.iter()) {
                    o[..120].copy_from_slice(&r[..120]);
                    for i in 0..120 {
                        let w = CELT_WINDOW2[i];
                        o[120 + i] = o[120 + i] * w + r[120 + i] * (1.0 - w);
                    }
                }
            }
        }
        Some(())
    }
}

pub struct OpusDec {
    streams: Vec<Stream>,
    /// Per output channel: (stream, channel) or None for silence.
    maps: Vec<Option<(usize, usize)>>,
    gain: f32,
    preskip: usize,
    skip: usize,
}

impl OpusDec {
    /// `head`: the OpusHead block (MKV/WebM CodecPrivate); may be empty for plain stereo.
    pub fn new(head: &[u8]) -> OpusDec {
        let mut channels = 2usize;
        let mut preskip = 0;
        let mut gain = 1.0;
        let mut streams = vec![Stream::new(2)];
        let mut maps = vec![Some((0, 0)), Some((0, 1))];
        if head.len() >= 19 && &head[..8] == b"OpusHead" {
            channels = head[9].max(1) as usize;
            preskip = u16::from_le_bytes([head[10], head[11]]) as usize;
            let g = i16::from_le_bytes([head[16], head[17]]);
            if g != 0 {
                gain = libm::powf(10.0, g as f32 / (20.0 * 256.0));
            }
            let family = head[18];
            if family == 0 || head.len() < 21 + channels {
                let ch = channels.min(2);
                streams = vec![Stream::new(ch)];
                maps = (0..ch).map(|c| Some((0, c))).collect();
                channels = ch;
            } else {
                let n = head[19] as usize;
                let coupled = head[20] as usize;
                streams = (0..n.max(1)).map(|i| Stream::new(if i < coupled { 2 } else { 1 })).collect();
                maps = head[21..21 + channels]
                    .iter()
                    .map(|&idx| {
                        let idx = idx as usize;
                        if idx == 255 || idx >= n + coupled {
                            None
                        } else if idx < 2 * coupled {
                            Some((idx / 2, idx & 1))
                        } else {
                            Some((idx - coupled, 0))
                        }
                    })
                    .collect();
            }
        }
        let _ = channels;
        OpusDec { streams, maps, gain, preskip, skip: preskip }
    }

    /// All the output channels for one packet (48 kHz).
    fn packet(&mut self, data: &[u8]) -> Option<Vec<Vec<f32>>> {
        let ns = self.streams.len();
        let mut per_stream: Vec<Vec<Vec<f32>>> = Vec::with_capacity(ns);
        let mut d = data;
        let mut total = 0;
        for (i, s) in self.streams.iter_mut().enumerate() {
            let pkt = parse_packet(d, i + 1 != ns)?;
            let n = pkt.frames.len() * pkt.dur;
            if i == 0 {
                total = n;
            } else if n != total {
                return None;
            }
            let mut out = vec![vec![0f32; n]; s.out_ch];
            let mut fbuf = vec![vec![0f32; pkt.dur.max(240)]; s.out_ch];
            for (k, &(off, len)) in pkt.frames.iter().enumerate() {
                if s.frame(&d[off..off + len], &pkt, &mut fbuf).is_none() {
                    fbuf.iter_mut().for_each(|b| b.iter_mut().for_each(|v| *v = 0.0));
                }
                for c in 0..s.out_ch {
                    out[c][k * pkt.dur..(k + 1) * pkt.dur].copy_from_slice(&fbuf[c][..pkt.dur]);
                }
            }
            per_stream.push(out);
            d = &d[pkt.size.min(d.len())..];
        }
        let chans = self
            .maps
            .iter()
            .map(|m| match m {
                Some((s, c)) => per_stream[*s][*c].iter().map(|v| v * self.gain).collect(),
                None => vec![0f32; total],
            })
            .collect();
        Some(chans)
    }
}

/// Opus channel order (as Vorbis), mixed to stereo.
fn mix(chans: &[Vec<f32>], i: usize) -> (f32, f32) {
    const H: f32 = core::f32::consts::FRAC_1_SQRT_2;
    let w: &[(f32, f32)] = match chans.len() {
        1 => return (chans[0][i], chans[0][i]),
        2 => return (chans[0][i], chans[1][i]),
        3 => &[(1.0, 0.0), (H, H), (0.0, 1.0)],
        4 => &[(1.0, 0.0), (0.0, 1.0), (H, 0.0), (0.0, H)],
        5 => &[(1.0, 0.0), (H, H), (0.0, 1.0), (H, 0.0), (0.0, H)],
        6 => &[(1.0, 0.0), (H, H), (0.0, 1.0), (H, 0.0), (0.0, H), (0.0, 0.0)],
        7 => &[(1.0, 0.0), (H, H), (0.0, 1.0), (H, 0.0), (0.0, H), (0.5, 0.5), (0.0, 0.0)],
        _ => &[(1.0, 0.0), (H, H), (0.0, 1.0), (H, 0.0), (0.0, H), (H, 0.0), (0.0, H), (0.0, 0.0)],
    };
    let norm = 1.0 / w.iter().map(|m| m.0).sum::<f32>();
    let (mut l, mut r) = (0f32, 0f32);
    for (c, &(wl, wr)) in w.iter().enumerate().take(chans.len()) {
        l += chans[c][i] * wl;
        r += chans[c][i] * wr;
    }
    (l * norm, r * norm)
}

impl AudioDec for OpusDec {
    fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        let Some(chans) = self.packet(data) else {
            return Vec::new();
        };
        let n = chans.first().map(|c| c.len()).unwrap_or(0);
        let start = self.skip.min(n);
        self.skip -= start;
        let s = |v: f32| -> i16 {
            let x = v * 32768.0;
            (if x >= 0.0 { x + 0.5 } else { x - 0.5 }).clamp(-32768.0, 32767.0) as i16
        };
        let mut pcm = Vec::with_capacity((n - start) * 2);
        for i in start..n {
            let (l, r) = mix(&chans, i);
            pcm.push(s(l));
            pcm.push(s(r));
        }
        pcm
    }
    fn rate(&self) -> u32 {
        48000
    }
    fn reset(&mut self) {
        for s in self.streams.iter_mut() {
            s.reset();
        }
        let _ = self.preskip;
    }
}

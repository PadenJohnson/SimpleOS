//! Decoders behind one interface: packets in, pictures (YUV or RGB) and stereo sound out.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use super::{ACodec, AudioInfo, VCodec, VideoInfo};
use crate::image::Image;
use crate::video::aac::Aac;
use crate::video::h264d::{Decoder as H264, Frame};

/// A decoded picture.
pub enum Pic {
    /// 4:2:0 YUV (most video formats).
    Yuv(Frame),
    /// Already RGB (Motion JPEG).
    Rgb(Image),
}

pub trait VideoDec {
    /// One packet (a frame, or part of the stream).
    fn decode(&mut self, data: &[u8]);
    /// The next finished picture, in display order.
    fn pop(&mut self) -> Option<Pic>;
    /// No more packets: let out the pictures still held back.
    fn flush(&mut self);
    /// Forget everything (after a jump; the next packet is a key frame).
    fn reset(&mut self);
    /// Decoders that find the pictures in the stream themselves (MPEG-1/2 and MPEG-4 streams,
    /// where a packet may hold part of a picture or several) also time them: they keep each
    /// packet's time with the picture that starts in it.
    fn timed(&self) -> bool {
        false
    }
    fn decode_at(&mut self, data: &[u8], _pts: Option<i64>) {
        self.decode(data)
    }
    /// The next picture in display order with its time (microseconds).
    fn pop_at(&mut self) -> Option<(i64, Pic)> {
        None
    }
}

pub trait AudioDec {
    /// One packet; returns interleaved stereo at `rate()` (may be empty).
    fn decode(&mut self, data: &[u8]) -> Vec<i16>;
    fn rate(&self) -> u32;
    fn reset(&mut self);
}

pub fn video(info: &VideoInfo) -> Result<Box<dyn VideoDec>, String> {
    match &info.codec {
        VCodec::H264 { avcc } => {
            let mut d = H264::new();
            d.configure(avcc).map_err(|e| format!("This video's pictures can't be read ({})", e))?;
            Ok(Box::new(H264Dec { d, annexb: false }))
        }
        VCodec::H264AnnexB => Ok(Box::new(H264Dec { d: H264::new(), annexb: true })),
        VCodec::Mjpeg => Ok(Box::new(MjpegDec { pic: None })),
        VCodec::Mpeg2 { config } => Ok(Box::new(super::mpeg2::Mpeg2Dec::new(config))),
        VCodec::Mpeg4 { config } => Ok(Box::new(super::mpeg4::Mpeg4Dec::new(config))),
        VCodec::H263 => Ok(Box::new(super::mpeg4::Mpeg4Dec::new(&[]))),
        VCodec::Hevc { hvcc } => Ok(Box::new(super::hevc::HevcDec::new(hvcc))),
        VCodec::HevcAnnexB => Ok(Box::new(super::hevc::HevcDec::new(&[]))),
        VCodec::Vp9 => Ok(Box::new(super::vp9::Vp9Dec::new())),
        VCodec::Vp8 => Ok(Box::new(super::vp8::Vp8Dec::new())),
        other => Err(unsupported_video(other)),
    }
}

pub fn audio(info: &AudioInfo) -> Result<Box<dyn AudioDec>, String> {
    match &info.codec {
        ACodec::Aac { asc } => {
            let d = Aac::new(asc).map_err(|_| String::from("This video's sound can't be read"))?;
            Ok(Box::new(AacDec { rate: d.rate, d: Some(d), asc: asc.clone(), adts: false }))
        }
        ACodec::AacAdts => Ok(Box::new(AacDec { d: None, asc: Vec::new(), rate: 0, adts: true })),
        ACodec::MpegAudio => Ok(Box::new(super::mpa::MpaDec::new())),
        ACodec::Ac3 => Ok(Box::new(super::ac3::Ac3Dec::new())),
        ACodec::Vorbis { headers } => Ok(Box::new(super::vorbis::VorbisDec::new(headers))),
        ACodec::Opus { head } => Ok(Box::new(super::opus::OpusDec::new(head))),
        &ACodec::Pcm { bits, big_endian, float } => Ok(Box::new(PcmDec { bits, big_endian, float, channels: info.channels.max(1), rate: info.rate.max(1) })),
        other => Err(format!("The sound is {}, which Solon can't play yet", other.name())),
    }
}

pub fn unsupported_video(c: &VCodec) -> String {
    let name = c.name();
    if matches!(c, VCodec::Hevc { .. } | VCodec::HevcAnnexB) {
        format!("This video uses {} (\"High Efficiency\"), which Solon can't play yet", name)
    } else {
        format!("This video's pictures are {}, which Solon can't play yet", name)
    }
}

// ---- H.264 ----

struct H264Dec {
    d: H264,
    annexb: bool,
}

impl VideoDec for H264Dec {
    fn decode(&mut self, data: &[u8]) {
        if self.annexb {
            self.d.decode_annexb(data);
        } else {
            let _ = self.d.decode_sample(data);
        }
    }
    fn pop(&mut self) -> Option<Pic> {
        self.d.pop().map(Pic::Yuv)
    }
    fn flush(&mut self) {
        self.d.flush();
    }
    fn reset(&mut self) {
        self.d.reset();
    }
}

// ---- Motion JPEG ----

struct MjpegDec {
    pic: Option<Image>,
}

impl VideoDec for MjpegDec {
    fn decode(&mut self, data: &[u8]) {
        let jpeg = crate::video::avi::with_huffman_tables(data.to_vec());
        if let Ok(img) = crate::image::decode(&jpeg) {
            self.pic = Some(img);
        }
    }
    fn pop(&mut self) -> Option<Pic> {
        self.pic.take().map(Pic::Rgb)
    }
    fn flush(&mut self) {}
    fn reset(&mut self) {
        self.pic = None;
    }
}

// ---- AAC ----

struct AacDec {
    d: Option<Aac>,
    asc: Vec<u8>,
    rate: u32,
    adts: bool,
}

impl AudioDec for AacDec {
    fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        if !self.adts {
            return self.d.as_mut().and_then(|d| d.decode(data).ok()).unwrap_or_default();
        }
        // ADTS: each frame has a 7 (or 9) byte header saying its format and length.
        let mut out = Vec::new();
        let mut p = 0;
        while p + 7 <= data.len() {
            if data[p] != 0xFF || data[p + 1] & 0xF0 != 0xF0 {
                p += 1;
                continue;
            }
            let protection_absent = data[p + 1] & 1 != 0;
            let profile = (data[p + 2] >> 6) + 1;
            let sfi = (data[p + 2] >> 2) & 15;
            let ch = ((data[p + 2] & 1) << 2) | (data[p + 3] >> 6);
            let len = ((data[p + 3] as usize & 3) << 11) | (data[p + 4] as usize) << 3 | (data[p + 5] as usize >> 5);
            let hdr = if protection_absent { 7 } else { 9 };
            if len < hdr || p + len > data.len() {
                break;
            }
            let asc = alloc::vec![(profile << 3) | (sfi >> 1), ((sfi & 1) << 7) | (ch << 3)];
            if self.d.is_none() || asc != self.asc {
                if let Ok(d) = Aac::new(&asc) {
                    self.rate = d.rate;
                    self.d = Some(d);
                    self.asc = asc;
                }
            }
            if let Some(d) = self.d.as_mut() {
                if let Ok(pcm) = d.decode(&data[p + hdr..p + len]) {
                    out.extend_from_slice(&pcm);
                }
            }
            p += len;
        }
        out
    }
    fn rate(&self) -> u32 {
        self.rate
    }
    fn reset(&mut self) {
        if !self.asc.is_empty() {
            if let Ok(d) = Aac::new(&self.asc) {
                self.d = Some(d);
            }
        }
    }
}

// ---- PCM ----

struct PcmDec {
    bits: u16,
    big_endian: bool,
    float: bool,
    channels: u16,
    rate: u32,
}

impl AudioDec for PcmDec {
    fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        let bytes = (self.bits as usize / 8).max(1);
        let ch = self.channels as usize;
        let frame = bytes * ch;
        let mut out = Vec::with_capacity(data.len() / frame.max(1) * 2);
        let sample = |s: &[u8]| -> i16 {
            let get = |i: usize| if self.big_endian { s[i] } else { s[bytes - 1 - i] }; // most significant first
            match (bytes, self.float) {
                (1, _) => ((s[0] as i16) - 128) << 8,
                (4, true) => {
                    let v = if self.big_endian { u32::from_be_bytes([s[0], s[1], s[2], s[3]]) } else { u32::from_le_bytes([s[0], s[1], s[2], s[3]]) };
                    float_to_i16(v)
                }
                _ => i16::from_be_bytes([get(0), get(1)]),
            }
        };
        for f in data.chunks_exact(frame.max(1)) {
            let l = sample(&f[..bytes]);
            let r = if ch > 1 { sample(&f[bytes..2 * bytes]) } else { l };
            out.push(l);
            out.push(r);
        }
        out
    }
    fn rate(&self) -> u32 {
        self.rate
    }
    fn reset(&mut self) {}
}

/// An IEEE float sample (as bits) to 16 bits, without floating point.
fn float_to_i16(bits: u32) -> i16 {
    let neg = bits >> 31 != 0;
    let exp = ((bits >> 23) & 0xFF) as i32;
    let mant = (bits & 0x7F_FFFF) | 0x80_0000;
    // value = mant * 2^(exp - 150); we want value * 32767.
    let shift = exp - 150;
    let v = if exp == 0 {
        0i64
    } else if shift >= 0 {
        i64::MAX
    } else if shift > -40 {
        (mant as i64 * 32767) >> (-shift)
    } else {
        0
    };
    let v = v.min(32767) as i16;
    if neg {
        -v
    } else {
        v
    }
}

//! Playing videos from phones and computers. A container reader (MP4/MOV, AVI, MKV/WebM,
//! MPEG program and transport streams) turns the file into packets of picture and sound;
//! decoders turn those into pictures and sound; `Engine` puts it together for the Videos app.

// Some codec settings are only used by decoders still to come.
#![allow(dead_code)]

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub mod ac3;
pub mod ac3tab;
pub mod avi;
pub mod dec;
pub mod engine;
pub mod es;
pub mod mkv;
pub mod mp4;
pub mod mc;
pub mod mpa;
pub mod mpatab;
pub mod mpeg12tab;
pub mod hevc;
pub mod mpeg2;
pub mod mpeg4;
pub mod mpeg4tab;
pub mod idct;
pub mod vlc;
pub mod mpegps;
pub mod mpegts;

pub use engine::Engine;

/// Read `len` bytes at `offset` (fewer at the end of the file).
pub type Read<'a> = dyn FnMut(u64, usize) -> Option<Vec<u8>> + 'a;

#[derive(Clone, Debug)]
pub enum VCodec {
    /// H.264 as in MP4/MKV (length-prefixed NAL units), with its avcC record.
    H264 { avcc: Vec<u8> },
    /// H.264 with start codes (MPEG streams, AVI).
    H264AnnexB,
    Hevc { hvcc: Vec<u8> },
    HevcAnnexB,
    /// MPEG-4 Part 2 (Xvid, DivX, older phones and cameras); `config` = headers stored
    /// outside the frames, if any.
    Mpeg4 { config: Vec<u8> },
    H263,
    /// MPEG-1 and MPEG-2 (DVDs, TV recordings).
    Mpeg2 { config: Vec<u8> },
    Mjpeg,
    Vp8,
    Vp9,
    Other(String),
}

#[derive(Clone, Debug)]
pub enum ACodec {
    Aac { asc: Vec<u8> },
    /// AAC with ADTS headers on every frame (MPEG transport streams).
    AacAdts,
    /// MPEG audio layer 1, 2 or 3 (MP3, MP2).
    MpegAudio,
    Ac3,
    Eac3,
    Pcm { bits: u16, big_endian: bool, float: bool },
    Vorbis { headers: Vec<u8> },
    Opus { head: Vec<u8> },
    Other(String),
}

impl VCodec {
    pub fn name(&self) -> String {
        String::from(match self {
            VCodec::H264 { .. } | VCodec::H264AnnexB => "H.264",
            VCodec::Hevc { .. } | VCodec::HevcAnnexB => "HEVC (H.265)",
            VCodec::Mpeg4 { .. } => "MPEG-4 (Xvid/DivX)",
            VCodec::H263 => "H.263",
            VCodec::Mpeg2 { .. } => "MPEG-2",
            VCodec::Mjpeg => "Motion JPEG",
            VCodec::Vp8 => "VP8",
            VCodec::Vp9 => "VP9",
            VCodec::Other(s) => return s.clone(),
        })
    }
}

impl ACodec {
    pub fn name(&self) -> String {
        String::from(match self {
            ACodec::Aac { .. } | ACodec::AacAdts => "AAC",
            ACodec::MpegAudio => "MP3",
            ACodec::Ac3 => "AC-3 (Dolby Digital)",
            ACodec::Eac3 => "E-AC-3 (Dolby Digital Plus)",
            ACodec::Pcm { .. } => "PCM",
            ACodec::Vorbis { .. } => "Vorbis",
            ACodec::Opus { .. } => "Opus",
            ACodec::Other(s) => return s.clone(),
        })
    }
}

pub struct VideoInfo {
    pub codec: VCodec,
    pub width: u32,
    pub height: u32,
    /// Degrees clockwise to turn the picture (phones filming upright).
    pub rotation: u32,
}

pub struct AudioInfo {
    pub codec: ACodec,
    pub rate: u32,
    pub channels: u16,
}

pub struct Info {
    pub duration_ms: u64,
    pub video: Option<VideoInfo>,
    pub audio: Option<AudioInfo>,
}

pub struct Packet {
    /// Presentation time in microseconds from the start (None = the file doesn't say).
    pub pts: Option<i64>,
    pub data: Vec<u8>,
    pub key: bool,
}

/// A container reader. Picture and sound are read separately (each keeps its own place).
pub trait Demux {
    fn info(&self) -> &Info;
    fn next_video(&mut self, read: &mut Read) -> Option<Packet>;
    fn next_audio(&mut self, read: &mut Read) -> Option<Packet>;
    /// Go to the key frame at or before `us` (as near as the file allows); returns its time.
    fn seek_video(&mut self, read: &mut Read, us: i64) -> i64;
    fn seek_audio(&mut self, read: &mut Read, us: i64);
    /// Files with an index: the key frame at or before `us`, without reading anything.
    fn key_before(&self, _us: i64) -> Option<i64> {
        None
    }
}

/// Recognise the container from the first bytes (not the name: files are often misnamed).
pub fn open_demux(read: &mut Read, size: u64) -> Result<Box<dyn Demux>, String> {
    let head = read(0, 64).ok_or_else(|| String::from("Can't read the file"))?;
    if head.len() >= 12 && &head[0..4] == b"RIFF" && &head[8..12] == b"AVI " {
        return avi::AviDemux::open(read, size).map(|d| Box::new(d) as Box<dyn Demux>);
    }
    if head.len() >= 8 && [&b"ftyp"[..], b"moov", b"mdat", b"wide", b"free", b"skip", b"pnot"].contains(&&head[4..8]) {
        return mp4::Mp4Demux::open(read, size).map(|d| Box::new(d) as Box<dyn Demux>);
    }
    if head.len() >= 4 && head[0..4] == [0x1A, 0x45, 0xDF, 0xA3] {
        return mkv::MkvDemux::open(read, size).map(|d| Box::new(d) as Box<dyn Demux>);
    }
    // Transport streams: a 0x47 sync byte every 188 bytes (192 in .m2ts).
    if let Some(more) = read(0, 600) {
        let sync = |s: usize, step: usize| (0..3).all(|k| more.get(s + k * step) == Some(&0x47));
        if sync(0, 188) || sync(4, 192) {
            return mpegts::TsDemux::open(read, size).map(|d| Box::new(d) as Box<dyn Demux>);
        }
    }
    if head.len() >= 4 && head[0..4] == [0, 0, 1, 0xBA] {
        return mpegps::PsDemux::open(read, size).map(|d| Box::new(d) as Box<dyn Demux>);
    }
    Err(format!("Solon can't read this kind of video file yet"))
}

pub(crate) fn be16(b: &[u8], o: usize) -> u16 {
    u16::from_be_bytes([b[o], b[o + 1]])
}
pub(crate) fn be32(b: &[u8], o: usize) -> u32 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
pub(crate) fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
pub(crate) fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}

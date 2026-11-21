//! MP4 (ISO base media file) writer: one H.264 video track and, optionally, one FLAC
//! audio track. The finished file is `header()` (ftyp + moov + the mdat box header)
//! followed by `payload()`, so it can be written front to back ("fast start").

use alloc::vec::Vec;

pub struct AudioFormat {
    pub rate: u32,
    pub channels: u16,
    pub bits: u16,
    /// FLAC STREAMINFO block (34 bytes).
    pub streaminfo: [u8; 34],
}

struct Sample {
    audio: bool,
    size: u32,
    key: bool,
    /// Duration in the track's own timescale.
    dur: u32,
}

pub struct Muxer {
    pub width: u32,
    pub height: u32,
    /// Video timescale (ticks per second) and the length of one frame in ticks.
    pub timescale: u32,
    pub frame_ticks: u32,
    pub sps: Vec<u8>,
    pub pps: Vec<u8>,
    pub audio: Option<AudioFormat>,
    payload: Vec<u8>,
    samples: Vec<Sample>,
}

fn be32(v: &mut Vec<u8>, x: u32) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn be16(v: &mut Vec<u8>, x: u16) {
    v.extend_from_slice(&x.to_be_bytes());
}
fn be64(v: &mut Vec<u8>, x: u64) {
    v.extend_from_slice(&x.to_be_bytes());
}

/// Write a box: `f` fills the body, the size is patched in afterwards.
fn bx(v: &mut Vec<u8>, kind: &[u8; 4], f: impl FnOnce(&mut Vec<u8>)) {
    let at = v.len();
    be32(v, 0);
    v.extend_from_slice(kind);
    f(v);
    let size = (v.len() - at) as u32;
    v[at..at + 4].copy_from_slice(&size.to_be_bytes());
}
fn full(v: &mut Vec<u8>, kind: &[u8; 4], version: u8, flags: u32, f: impl FnOnce(&mut Vec<u8>)) {
    bx(v, kind, |v| {
        be32(v, (version as u32) << 24 | flags);
        f(v);
    });
}

fn matrix(v: &mut Vec<u8>) {
    for x in [0x0001_0000u32, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000] {
        be32(v, x);
    }
}

impl Muxer {
    pub fn new(width: u32, height: u32, timescale: u32, frame_ticks: u32, sps: Vec<u8>, pps: Vec<u8>, audio: Option<AudioFormat>) -> Muxer {
        Muxer { width, height, timescale, frame_ticks, sps, pps, audio, payload: Vec::new(), samples: Vec::new() }
    }

    /// Add one video frame made of NAL units (without start codes).
    pub fn add_video(&mut self, nals: &[&[u8]], key: bool) {
        let mut size = 0;
        for n in nals {
            be32(&mut self.payload, n.len() as u32);
            self.payload.extend_from_slice(n);
            size += 4 + n.len() as u32;
        }
        self.samples.push(Sample { audio: false, size, key, dur: self.frame_ticks });
    }

    /// Add one encoded audio frame lasting `frames` sample frames.
    pub fn add_audio(&mut self, data: &[u8], frames: u32) {
        self.payload.extend_from_slice(data);
        self.samples.push(Sample { audio: true, size: data.len() as u32, key: true, dur: frames });
    }

    /// Hand over the payload (after `header()`), without copying it.
    pub fn take_payload(&mut self) -> Vec<u8> {
        core::mem::take(&mut self.payload)
    }

    #[allow(dead_code)] // used by the host tests (tools/videotest)
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    #[allow(dead_code)]
    pub fn video_frames(&self) -> usize {
        self.samples.iter().filter(|s| !s.audio).count()
    }

    /// Everything before the payload.
    pub fn header(&self) -> Vec<u8> {
        let mut ftyp = Vec::new();
        bx(&mut ftyp, b"ftyp", |v| {
            v.extend_from_slice(b"isom");
            be32(v, 0x200);
            for b in [b"isom", b"iso2", b"avc1", b"mp41"] {
                v.extend_from_slice(b);
            }
        });
        // The moov's size doesn't depend on the offsets in it (they are all 64-bit), so
        // build it once to measure, then again with the real start of the payload.
        let probe = self.moov(0);
        let mdat_hdr = 16u64;
        let base = ftyp.len() as u64 + probe.len() as u64 + mdat_hdr;
        let moov = self.moov(base);
        let mut out = ftyp;
        out.extend_from_slice(&moov);
        be32(&mut out, 1);
        out.extend_from_slice(b"mdat");
        be64(&mut out, mdat_hdr + self.payload.len() as u64);
        out
    }

    fn moov(&self, base: u64) -> Vec<u8> {
        let vdur_ticks: u64 = self.samples.iter().filter(|s| !s.audio).map(|s| s.dur as u64).sum();
        let adur: u64 = self.samples.iter().filter(|s| s.audio).map(|s| s.dur as u64).sum();
        let vdur_ms = vdur_ticks * 1000 / self.timescale as u64;
        let adur_ms = match &self.audio {
            Some(a) => adur * 1000 / a.rate as u64,
            None => 0,
        };
        let movie_ms = vdur_ms.max(adur_ms);
        let mut v = Vec::new();
        bx(&mut v, b"moov", |v| {
            full(v, b"mvhd", 0, 0, |v| {
                be32(v, 0);
                be32(v, 0);
                be32(v, 1000);
                be32(v, movie_ms as u32);
                be32(v, 0x0001_0000);
                be16(v, 0x0100);
                v.extend_from_slice(&[0; 10]);
                matrix(v);
                v.extend_from_slice(&[0; 24]);
                be32(v, if self.audio.is_some() { 3 } else { 2 });
            });
            self.trak(v, base, false, vdur_ms, vdur_ticks);
            if self.audio.is_some() {
                self.trak(v, base, true, adur_ms, adur);
            }
        });
        v
    }

    fn trak(&self, v: &mut Vec<u8>, base: u64, audio: bool, dur_ms: u64, dur_ticks: u64) {
        let timescale = match (&self.audio, audio) {
            (Some(a), true) => a.rate,
            _ => self.timescale,
        };
        bx(v, b"trak", |v| {
            full(v, b"tkhd", 0, 3, |v| {
                be32(v, 0);
                be32(v, 0);
                be32(v, if audio { 2 } else { 1 });
                be32(v, 0);
                be32(v, dur_ms as u32);
                v.extend_from_slice(&[0; 8]);
                be16(v, 0); // layer
                be16(v, if audio { 1 } else { 0 }); // alternate group
                be16(v, if audio { 0x0100 } else { 0 });
                be16(v, 0);
                matrix(v);
                if audio {
                    be32(v, 0);
                    be32(v, 0);
                } else {
                    be32(v, self.width << 16);
                    be32(v, self.height << 16);
                }
            });
            bx(v, b"mdia", |v| {
                full(v, b"mdhd", 0, 0, |v| {
                    be32(v, 0);
                    be32(v, 0);
                    be32(v, timescale);
                    be32(v, dur_ticks as u32);
                    be16(v, 0x55C4); // "und"
                    be16(v, 0);
                });
                full(v, b"hdlr", 0, 0, |v| {
                    be32(v, 0);
                    v.extend_from_slice(if audio { b"soun" } else { b"vide" });
                    v.extend_from_slice(&[0; 12]);
                    v.extend_from_slice(if audio { b"Sound\0" } else { b"Video\0" });
                });
                bx(v, b"minf", |v| {
                    if audio {
                        full(v, b"smhd", 0, 0, |v| be32(v, 0));
                    } else {
                        full(v, b"vmhd", 0, 1, |v| v.extend_from_slice(&[0; 8]));
                    }
                    bx(v, b"dinf", |v| {
                        full(v, b"dref", 0, 0, |v| {
                            be32(v, 1);
                            full(v, b"url ", 0, 1, |_| {});
                        });
                    });
                    bx(v, b"stbl", |v| self.stbl(v, base, audio));
                });
            });
        });
    }

    fn stbl(&self, v: &mut Vec<u8>, base: u64, audio: bool) {
        full(v, b"stsd", 0, 0, |v| {
            be32(v, 1);
            if audio {
                let a = self.audio.as_ref().unwrap();
                bx(v, b"fLaC", |v| {
                    v.extend_from_slice(&[0; 6]);
                    be16(v, 1);
                    v.extend_from_slice(&[0; 8]);
                    be16(v, a.channels);
                    be16(v, a.bits);
                    be32(v, 0);
                    be32(v, a.rate << 16);
                    full(v, b"dfLa", 0, 0, |v| {
                        v.push(0x80); // last metadata block, STREAMINFO
                        v.extend_from_slice(&[0, 0, 34]);
                        v.extend_from_slice(&a.streaminfo);
                    });
                });
            } else {
                bx(v, b"avc1", |v| {
                    v.extend_from_slice(&[0; 6]);
                    be16(v, 1);
                    v.extend_from_slice(&[0; 16]);
                    be16(v, self.width as u16);
                    be16(v, self.height as u16);
                    be32(v, 0x0048_0000);
                    be32(v, 0x0048_0000);
                    be32(v, 0);
                    be16(v, 1); // frame count
                    v.extend_from_slice(&[0; 32]); // compressor name
                    be16(v, 0x18);
                    be16(v, 0xFFFF);
                    bx(v, b"avcC", |v| {
                        v.push(1);
                        v.push(self.sps[1]);
                        v.push(self.sps[2]);
                        v.push(self.sps[3]);
                        v.push(0xFF); // 4-byte lengths
                        v.push(0xE1); // one SPS
                        be16(v, self.sps.len() as u16);
                        v.extend_from_slice(&self.sps);
                        v.push(1);
                        be16(v, self.pps.len() as u16);
                        v.extend_from_slice(&self.pps);
                    });
                });
            }
        });
        let mine: Vec<(usize, &Sample)> = self.samples.iter().enumerate().filter(|(_, s)| s.audio == audio).collect();
        // Time to sample, run-length coded.
        let mut stts: Vec<(u32, u32)> = Vec::new();
        for (_, s) in &mine {
            match stts.last_mut() {
                Some((n, d)) if *d == s.dur => *n += 1,
                _ => stts.push((1, s.dur)),
            }
        }
        full(v, b"stts", 0, 0, |v| {
            be32(v, stts.len() as u32);
            for (n, d) in &stts {
                be32(v, *n);
                be32(v, *d);
            }
        });
        if !audio {
            let keys: Vec<u32> = mine.iter().enumerate().filter(|(_, (_, s))| s.key).map(|(i, _)| i as u32 + 1).collect();
            full(v, b"stss", 0, 0, |v| {
                be32(v, keys.len() as u32);
                for k in &keys {
                    be32(v, *k);
                }
            });
        }
        // Chunks: runs of this track's samples that sit together in the payload.
        let mut chunks: Vec<(u64, u32)> = Vec::new(); // (offset, samples)
        let mut off = base;
        let mut prev: Option<usize> = None;
        let mut offsets = Vec::with_capacity(self.samples.len());
        for s in &self.samples {
            offsets.push(off);
            off += s.size as u64;
        }
        for (i, _) in &mine {
            if *i > 0 && prev == Some(i - 1) && !chunks.is_empty() {
                chunks.last_mut().unwrap().1 += 1;
            } else {
                chunks.push((offsets[*i], 1));
            }
            prev = Some(*i);
        }
        let mut stsc: Vec<(u32, u32)> = Vec::new(); // (first chunk, samples per chunk)
        for (k, (_, n)) in chunks.iter().enumerate() {
            if stsc.last().map(|l| l.1) != Some(*n) {
                stsc.push((k as u32 + 1, *n));
            }
        }
        full(v, b"stsc", 0, 0, |v| {
            be32(v, stsc.len() as u32);
            for (f, n) in &stsc {
                be32(v, *f);
                be32(v, *n);
                be32(v, 1);
            }
        });
        full(v, b"stsz", 0, 0, |v| {
            be32(v, 0);
            be32(v, mine.len() as u32);
            for (_, s) in &mine {
                be32(v, s.size);
            }
        });
        full(v, b"co64", 0, 0, |v| {
            be32(v, chunks.len() as u32);
            for (o, _) in &chunks {
                be64(v, *o);
            }
        });
    }
}

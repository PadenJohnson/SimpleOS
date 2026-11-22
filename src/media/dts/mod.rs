//! DTS sound (DTS Coherent Acoustics): many DVDs, Blu-ray films and MKV/AVI movies. Decodes
//! the core stream (up to 5.1, 32 sub-bands with ADPCM, joint intensity, LFE) step for step as
//! FFmpeg's decoder does (libavcodec/dca_core.c, dcadsp.c, synth_filter.c; LGPL 2.1 or later),
//! and mixes it to stereo. DTS-HD streams play their core (the lossless or high-resolution
//! extensions, and the 6.1 extra channel, are skipped).

use alloc::boxed::Box;
use alloc::vec;
use alloc::vec::Vec;

use super::dec::AudioDec;
use super::vlc::Vlc;
use crate::video::aac::Imdct;
use crate::video::bits::Bits;

mod tab;
use tab::*;

const CHANNELS_MAX: usize = 7;
const SUBBANDS: usize = 32;
const ADPCM: usize = 4;
const LFE_HISTORY: usize = 8;
const MAX_BLOCKS: usize = 128;
const STRIDE: usize = ADPCM + MAX_BLOCKS;

const SAMPLE_RATES: [u32; 16] = [0, 8000, 16000, 32000, 0, 0, 11025, 22050, 44100, 0, 0, 12000, 24000, 48000, 96000, 192000];
const BLOCK_CODE_BITS: [u32; 7] = [7, 10, 12, 13, 15, 17, 19];

// Speakers.
const SP_C: usize = 0;
const SP_L: usize = 1;
const SP_R: usize = 2;
const SP_LS: usize = 3;
const SP_RS: usize = 4;
const SP_LFE: usize = 5;
const SP_CS: usize = 6;

const PRM_TO_SPKR: [[i8; 5]; 10] = [
    [0, -1, -1, -1, -1],
    [1, 2, -1, -1, -1],
    [1, 2, -1, -1, -1],
    [1, 2, -1, -1, -1],
    [1, 2, -1, -1, -1],
    [0, 1, 2, -1, -1],
    [1, 2, 6, -1, -1],
    [0, 1, 2, 6, -1],
    [1, 2, 3, 4, -1],
    [0, 1, 2, 3, 4],
];

fn clip23(a: i64) -> i32 {
    a.clamp(-(1 << 23), (1 << 23) - 1) as i32
}

fn norm(a: i64, bits: i32) -> i32 {
    if bits > 0 {
        ((a + (1i64 << (bits - 1))) >> bits) as i32
    } else {
        a as i32
    }
}

fn mul(a: i32, b: i32, bits: i32) -> i32 {
    norm(a as i64 * b as i64, bits)
}

/// Codes from (symbol, length) pairs listed in code order, symbols offset by `off`.
fn vlc_from(pairs: &[u8], off: i32) -> Vlc {
    let mut acc: u64 = 0;
    let mut codes = Vec::with_capacity(pairs.len() / 2);
    for p in pairs.chunks(2) {
        let l = p[1];
        if l > 0 {
            codes.push(((acc >> (32 - l)) as u32, l, p[0] as i32 + off));
            acc += 1u64 << (32 - l);
        }
    }
    Vlc::new(&codes)
}

struct Tables {
    quant_index: Vec<Vec<Vlc>>,
    bit_allocation: Vec<Vlc>,
    scale_factor: Vec<Vlc>,
    transition_mode: Vec<Vlc>,
}

impl Tables {
    fn new() -> Box<Tables> {
        let mut at = 0;
        let mut take = |n: usize, off: i32| {
            let v = vlc_from(&VLC_SRC[at * 2..(at + n) * 2], off);
            at += n;
            v
        };
        let mut quant_index = Vec::new();
        for i in 0..10 {
            let mut g = Vec::new();
            for _ in 0..QUANT_INDEX_GROUP_SIZE[i] {
                g.push(take(BITALLOC_SIZES[i] as usize, BITALLOC_OFFSETS[i] as i32));
            }
            quant_index.push(g);
        }
        let bit_allocation = (0..5).map(|_| take(12, 1)).collect();
        let scale_factor = (0..5).map(|_| take(129, -64)).collect();
        let transition_mode = (0..4).map(|_| take(4, 0)).collect();
        Box::new(Tables { quant_index, bit_allocation, scale_factor, transition_mode })
    }
}

struct Core {
    // Frame header.
    crc_present: bool,
    npcmblocks: usize,
    frame_size: usize,
    audio_mode: usize,
    sample_rate: u32,
    bit_rate: u32,
    drc_present: bool,
    sync_ssf: bool,
    lfe_present: usize,
    predictor_history: bool,
    filter_perfect: bool,
    sumdiff_front: bool,
    sumdiff_surround: bool,
    // Coding header.
    nsubframes: usize,
    nchannels: usize,
    nsubbands: [usize; CHANNELS_MAX],
    subband_vq_start: [usize; CHANNELS_MAX],
    joint_intensity_index: [usize; CHANNELS_MAX],
    transition_mode_sel: [usize; CHANNELS_MAX],
    scale_factor_sel: [usize; CHANNELS_MAX],
    bit_allocation_sel: [usize; CHANNELS_MAX],
    quant_index_sel: [[usize; 10]; CHANNELS_MAX],
    scale_factor_adj: [[i32; 10]; CHANNELS_MAX],
    // Subframe side information.
    nsubsubframes: [usize; 16],
    prediction_mode: [[bool; SUBBANDS]; CHANNELS_MAX],
    prediction_vq_index: [[usize; SUBBANDS]; CHANNELS_MAX],
    bit_allocation: [[usize; SUBBANDS]; CHANNELS_MAX],
    transition_mode: [[usize; SUBBANDS]; CHANNELS_MAX],
    scale_factors: [[[i32; 2]; SUBBANDS]; CHANNELS_MAX],
    joint_scale_sel: [usize; CHANNELS_MAX],
    joint_scale_factors: [[i32; SUBBANDS]; CHANNELS_MAX],
    // Samples: [channel][band][4 history + blocks].
    subband: Vec<i32>,
    lfe: [i32; LFE_HISTORY + MAX_BLOCKS],
    // Synthesis filter state per channel.
    hist1: [[f32; 512]; CHANNELS_MAX],
    hist2: [[f32; 32]; CHANNELS_MAX],
    offset: [usize; CHANNELS_MAX],
}

pub struct DtsDec {
    buf: Vec<u8>,
    rate: u32,
    t: Box<Tables>,
    c: Box<Core>,
    imdct: Imdct,
}

impl DtsDec {
    pub fn new() -> DtsDec {
        DtsDec {
            buf: Vec::new(),
            rate: 48000,
            t: Tables::new(),
            c: Box::new(Core {
                crc_present: false,
                npcmblocks: 0,
                frame_size: 0,
                audio_mode: 0,
                sample_rate: 48000,
                bit_rate: 0,
                drc_present: false,
                sync_ssf: false,
                lfe_present: 0,
                predictor_history: false,
                filter_perfect: false,
                sumdiff_front: false,
                sumdiff_surround: false,
                nsubframes: 0,
                nchannels: 0,
                nsubbands: [0; CHANNELS_MAX],
                subband_vq_start: [0; CHANNELS_MAX],
                joint_intensity_index: [0; CHANNELS_MAX],
                transition_mode_sel: [0; CHANNELS_MAX],
                scale_factor_sel: [0; CHANNELS_MAX],
                bit_allocation_sel: [0; CHANNELS_MAX],
                quant_index_sel: [[0; 10]; CHANNELS_MAX],
                scale_factor_adj: [[0; 10]; CHANNELS_MAX],
                nsubsubframes: [0; 16],
                prediction_mode: [[false; SUBBANDS]; CHANNELS_MAX],
                prediction_vq_index: [[0; SUBBANDS]; CHANNELS_MAX],
                bit_allocation: [[0; SUBBANDS]; CHANNELS_MAX],
                transition_mode: [[0; SUBBANDS]; CHANNELS_MAX],
                scale_factors: [[[0; 2]; SUBBANDS]; CHANNELS_MAX],
                joint_scale_sel: [0; CHANNELS_MAX],
                joint_scale_factors: [[0; SUBBANDS]; CHANNELS_MAX],
                subband: vec![0; CHANNELS_MAX * SUBBANDS * STRIDE],
                lfe: [0; LFE_HISTORY + MAX_BLOCKS],
                hist1: [[0.0; 512]; CHANNELS_MAX],
                hist2: [[0.0; 32]; CHANNELS_MAX],
                offset: [0; CHANNELS_MAX],
            }),
            imdct: Imdct::new(64),
        }
    }

    /// One core frame (16-bit big-endian words) into stereo samples.
    fn frame(&mut self, f: &[u8], out: &mut Vec<i16>) -> Option<()> {
        let mut b = Bits::new(f);
        self.header(&mut b)?;
        if !self.c.predictor_history {
            for ch in 0..CHANNELS_MAX {
                for band in 0..SUBBANDS {
                    let at = (ch * SUBBANDS + band) * STRIDE;
                    self.c.subband[at..at + ADPCM].fill(0);
                }
            }
        }
        self.coding_header(&mut b)?;
        let mut sub_pos = 0;
        let mut lfe_pos = LFE_HISTORY;
        for sf in 0..self.c.nsubframes {
            self.subframe_header(&mut b, sf)?;
            self.subframe_audio(&mut b, sf, &mut sub_pos, &mut lfe_pos)?;
        }
        let c = &mut *self.c;
        for ch in 0..c.nchannels {
            let mut nsb = c.nsubbands[ch];
            if c.joint_intensity_index[ch] > 0 {
                nsb = nsb.max(c.nsubbands[c.joint_intensity_index[ch] - 1]);
            }
            for band in 0..SUBBANDS {
                let at = (ch * SUBBANDS + band) * STRIDE;
                if band < nsb {
                    c.subband.copy_within(at + c.npcmblocks..at + c.npcmblocks + ADPCM, at);
                } else {
                    c.subband[at..at + ADPCM + c.npcmblocks].fill(0);
                }
            }
        }
        self.rate = c.sample_rate;
        self.synthesize(out);
        Some(())
    }

    fn header(&mut self, b: &mut Bits) -> Option<()> {
        if b.read(32) != 0x7FFE_8001 {
            return None;
        }
        let normal = b.flag();
        let deficit = b.read(5) + 1;
        if deficit != 32 {
            let _ = normal;
            return None;
        }
        let c = &mut *self.c;
        c.crc_present = b.flag();
        c.npcmblocks = b.read(7) as usize + 1;
        if c.npcmblocks & 7 != 0 || c.npcmblocks < 8 {
            return None;
        }
        c.frame_size = b.read(14) as usize + 1;
        if c.frame_size < 96 {
            return None;
        }
        c.audio_mode = b.read(6) as usize;
        if c.audio_mode >= 10 {
            return None;
        }
        let sr = b.read(4) as usize;
        if SAMPLE_RATES[sr] == 0 {
            return None;
        }
        c.sample_rate = SAMPLE_RATES[sr];
        c.bit_rate = BIT_RATES[b.read(5) as usize];
        if b.flag() {
            return None;
        }
        c.drc_present = b.flag();
        b.skip(1); // time stamp
        b.skip(1); // aux data
        b.skip(1); // HDCD
        b.skip(3); // extension type
        b.skip(1); // extension present
        c.sync_ssf = b.flag();
        c.lfe_present = b.read(2) as usize;
        if c.lfe_present == 3 {
            return None;
        }
        c.predictor_history = b.flag();
        if c.crc_present {
            b.skip(16);
        }
        c.filter_perfect = b.flag();
        b.skip(4 + 2); // encoder revision, copy history
        let pcmr = b.read(3);
        if pcmr == 7 {
            return None;
        }
        c.sumdiff_front = b.flag();
        c.sumdiff_surround = b.flag();
        b.skip(4); // dialogue normalisation
        Some(())
    }

    fn coding_header(&mut self, b: &mut Bits) -> Option<()> {
        let c = &mut *self.c;
        c.nsubframes = b.read(4) as usize + 1;
        c.nchannels = b.read(3) as usize + 1;
        if c.nchannels != CHANNELS[c.audio_mode] as usize || c.nchannels > 5 {
            return None;
        }
        let n = c.nchannels;
        for ch in 0..n {
            c.nsubbands[ch] = b.read(5) as usize + 2;
            if c.nsubbands[ch] > SUBBANDS {
                return None;
            }
        }
        for ch in 0..n {
            c.subband_vq_start[ch] = (b.read(5) as usize + 1).min(SUBBANDS);
        }
        for ch in 0..n {
            let j = b.read(3) as usize;
            if j > n {
                return None;
            }
            c.joint_intensity_index[ch] = j;
        }
        for ch in 0..n {
            c.transition_mode_sel[ch] = b.read(2) as usize;
        }
        for ch in 0..n {
            c.scale_factor_sel[ch] = b.read(3) as usize;
            if c.scale_factor_sel[ch] == 7 {
                return None;
            }
        }
        for ch in 0..n {
            c.bit_allocation_sel[ch] = b.read(3) as usize;
            if c.bit_allocation_sel[ch] == 7 {
                return None;
            }
        }
        for k in 0..10 {
            for ch in 0..n {
                c.quant_index_sel[ch][k] = b.read(QUANT_INDEX_SEL_NBITS[k] as u32) as usize;
            }
        }
        for k in 0..10 {
            for ch in 0..n {
                if c.quant_index_sel[ch][k] < QUANT_INDEX_GROUP_SIZE[k] as usize {
                    c.scale_factor_adj[ch][k] = SCALE_FACTOR_ADJ[b.read(2) as usize] as i32;
                }
            }
        }
        if c.crc_present {
            b.skip(16);
        }
        Some(())
    }

    fn parse_scale(&self, b: &mut Bits, index: &mut i32, sel: usize) -> Option<i32> {
        let (table, size): (&[u32], usize) = if sel > 5 { (&SCALE_FACTOR_QUANT7, 128) } else { (&SCALE_FACTOR_QUANT6, 64) };
        if sel < 5 {
            *index += self.t.scale_factor[sel].read(b)?;
        } else {
            *index = b.read(sel as u32 + 1) as i32;
        }
        if *index < 0 || *index as usize >= size {
            return None;
        }
        Some(table[*index as usize] as i32)
    }

    fn subframe_header(&mut self, b: &mut Bits, sf: usize) -> Option<()> {
        let n = self.c.nchannels;
        self.c.nsubsubframes[sf] = b.read(2) as usize + 1;
        b.skip(3);
        for ch in 0..n {
            for band in 0..self.c.nsubbands[ch] {
                self.c.prediction_mode[ch][band] = b.flag();
            }
        }
        for ch in 0..n {
            for band in 0..self.c.nsubbands[ch] {
                if self.c.prediction_mode[ch][band] {
                    self.c.prediction_vq_index[ch][band] = b.read(12) as usize;
                }
            }
        }
        for ch in 0..n {
            let sel = self.c.bit_allocation_sel[ch];
            for band in 0..self.c.subband_vq_start[ch] {
                let abits = if sel < 5 { self.t.bit_allocation[sel].read(b)? } else { b.read(sel as u32 - 1) as i32 };
                if !(0..=26).contains(&abits) {
                    return None;
                }
                self.c.bit_allocation[ch][band] = abits as usize;
            }
        }
        for ch in 0..n {
            self.c.transition_mode[ch] = [0; SUBBANDS];
            if self.c.nsubsubframes[sf] > 1 {
                let sel = self.c.transition_mode_sel[ch];
                for band in 0..self.c.subband_vq_start[ch] {
                    if self.c.bit_allocation[ch][band] != 0 {
                        self.c.transition_mode[ch][band] = self.t.transition_mode[sel].read(b)? as usize;
                    }
                }
            }
        }
        for ch in 0..n {
            let sel = self.c.scale_factor_sel[ch];
            let mut index = 0;
            for band in 0..self.c.subband_vq_start[ch] {
                if self.c.bit_allocation[ch][band] != 0 {
                    let v = self.parse_scale(b, &mut index, sel)?;
                    self.c.scale_factors[ch][band][0] = v;
                    if self.c.transition_mode[ch][band] != 0 {
                        let v = self.parse_scale(b, &mut index, sel)?;
                        self.c.scale_factors[ch][band][1] = v;
                    }
                } else {
                    self.c.scale_factors[ch][band][0] = 0;
                }
            }
            for band in self.c.subband_vq_start[ch]..self.c.nsubbands[ch] {
                let v = self.parse_scale(b, &mut index, sel)?;
                self.c.scale_factors[ch][band][0] = v;
            }
        }
        for ch in 0..n {
            if self.c.joint_intensity_index[ch] > 0 {
                self.c.joint_scale_sel[ch] = b.read(3) as usize;
                if self.c.joint_scale_sel[ch] == 7 {
                    return None;
                }
            }
        }
        for ch in 0..n {
            let j = self.c.joint_intensity_index[ch];
            if j > 0 {
                let src = j - 1;
                let sel = self.c.joint_scale_sel[ch];
                for band in self.c.nsubbands[ch]..self.c.nsubbands[src] {
                    let mut idx = if sel < 5 { self.t.scale_factor[sel].read(b)? } else { b.read(sel as u32 + 1) as i32 };
                    idx += 64;
                    if !(0..129).contains(&idx) {
                        return None;
                    }
                    self.c.joint_scale_factors[ch][band] = JOINT_SCALE_FACTORS[idx as usize] as i32;
                }
            }
        }
        if self.c.drc_present {
            b.skip(8);
        }
        if self.c.crc_present {
            b.skip(16);
        }
        Some(())
    }

    /// One band's 8 samples; returns whether a Huffman code was used.
    fn extract_audio(&self, b: &mut Bits, audio: &mut [i32; 8], abits: usize, ch: usize) -> Option<bool> {
        if abits == 0 {
            *audio = [0; 8];
            return Some(false);
        }
        if abits <= 10 {
            let sel = self.c.quant_index_sel[ch][abits - 1];
            if sel < QUANT_INDEX_GROUP_SIZE[abits - 1] as usize {
                let v = &self.t.quant_index[abits - 1][sel];
                for a in audio.iter_mut() {
                    *a = v.read(b)?;
                }
                return Some(true);
            }
            if abits <= 7 {
                let nb = BLOCK_CODE_BITS[abits - 1];
                let mut code1 = b.read(nb) as i32;
                let mut code2 = b.read(nb) as i32;
                let levels = QUANT_LEVELS[abits] as i32;
                let off = (levels - 1) / 2;
                for n in 0..4 {
                    let d = code1 / levels;
                    audio[n] = code1 - d * levels - off;
                    code1 = d;
                }
                for n in 4..8 {
                    let d = code2 / levels;
                    audio[n] = code2 - d * levels - off;
                    code2 = d;
                }
                if code1 | code2 != 0 {
                    return None;
                }
                return Some(false);
            }
        }
        let n = abits as u32 - 3;
        for a in audio.iter_mut() {
            let v = b.read(n) as i32;
            *a = (v << (32 - n)) >> (32 - n);
        }
        Some(false)
    }

    fn subframe_audio(&mut self, b: &mut Bits, sf: usize, sub_pos: &mut usize, lfe_pos: &mut usize) -> Option<()> {
        let nssf = self.c.nsubsubframes[sf];
        let nsamples = nssf * 8;
        if *sub_pos + nsamples > self.c.npcmblocks {
            return None;
        }
        let n = self.c.nchannels;
        // High-frequency VQ subbands.
        for ch in 0..n {
            let mut vq = [0usize; SUBBANDS];
            for band in self.c.subband_vq_start[ch]..self.c.nsubbands[ch] {
                vq[band] = b.read(10) as usize;
            }
            for band in self.c.subband_vq_start[ch]..self.c.nsubbands[ch] {
                let scale = self.c.scale_factors[ch][band][0] as i64;
                let at = (ch * SUBBANDS + band) * STRIDE + ADPCM + *sub_pos;
                for j in 0..nsamples {
                    let coeff = HIGH_FREQ_VQ[vq[band] * 32 + j] as i64;
                    self.c.subband[at + j] = clip23((coeff * scale + 8) >> 4);
                }
            }
        }
        // LFE.
        if self.c.lfe_present > 0 {
            let nlfe = 2 * self.c.lfe_present * nssf;
            let mut audio = [0i32; 16];
            for a in audio.iter_mut().take(nlfe) {
                let v = b.read(8) as i32;
                *a = (v << 24) >> 24;
            }
            let index = b.read(8) as usize;
            if index >= 128 {
                return None;
            }
            let scale = mul(4697620, SCALE_FACTOR_QUANT7[index] as i32, 23);
            for (k, a) in audio.iter().take(nlfe).enumerate() {
                if *lfe_pos + k < self.c.lfe.len() {
                    self.c.lfe[*lfe_pos + k] = clip23((*a as i64 * scale as i64) >> 4);
                }
            }
            *lfe_pos += nlfe;
        }
        // Audio.
        let mut ofs = *sub_pos;
        for ssf in 0..nssf {
            for ch in 0..n {
                for band in 0..self.c.subband_vq_start[ch] {
                    let abits = self.c.bit_allocation[ch][band];
                    let mut audio = [0i32; 8];
                    let huff = self.extract_audio(b, &mut audio, abits, ch)?;
                    let step = if self.c.bit_rate == 3 { LOSSLESS_QUANT[abits] } else { LOSSY_QUANT[abits] } as i64;
                    let trans = self.c.transition_mode[ch][band];
                    let mut scale = if trans == 0 || ssf < trans { self.c.scale_factors[ch][band][0] } else { self.c.scale_factors[ch][band][1] };
                    if huff {
                        let adj = self.c.scale_factor_adj[ch][abits - 1] as i64;
                        scale = clip23((adj * scale as i64) >> 22);
                    }
                    let mut step_scale = step * scale as i64;
                    let mut shift = 0;
                    if step_scale > (1 << 23) {
                        shift = 63 - ((step_scale >> 23) as u64).leading_zeros() as i32 + 1;
                        step_scale >>= shift;
                    }
                    let at = (ch * SUBBANDS + band) * STRIDE + ADPCM + ofs;
                    for k in 0..8 {
                        self.c.subband[at + k] = clip23(norm(audio[k] as i64 * step_scale, 22 - shift) as i64);
                    }
                }
            }
            if (ssf == nssf - 1 || self.c.sync_ssf) && b.read(16) != 0xFFFF {
                return None;
            }
            ofs += 8;
        }
        // Inverse ADPCM.
        let c = &mut *self.c;
        for ch in 0..n {
            for band in 0..c.nsubbands[ch] {
                if !c.prediction_mode[ch][band] {
                    continue;
                }
                let coeff = &ADPCM_VB[c.prediction_vq_index[ch][band] * 4..];
                let base = (ch * SUBBANDS + band) * STRIDE + ADPCM + *sub_pos;
                for j in 0..nsamples {
                    let p = base + j;
                    let mut pred: i64 = 0;
                    for i in 0..4 {
                        pred += c.subband[p - 1 - i] as i64 * coeff[i] as i64;
                    }
                    let x = clip23(norm(pred, 13) as i64);
                    c.subband[p] = clip23(c.subband[p] as i64 + x as i64);
                }
            }
        }
        // Joint subband coding: higher bands copied from another channel, scaled.
        for ch in 0..n {
            let j = c.joint_intensity_index[ch];
            if j > 0 {
                let src = j - 1;
                for band in c.nsubbands[ch]..c.nsubbands[src] {
                    let scale = c.joint_scale_factors[ch][band];
                    let d = (ch * SUBBANDS + band) * STRIDE + ADPCM + *sub_pos;
                    let s = (src * SUBBANDS + band) * STRIDE + ADPCM + *sub_pos;
                    for k in 0..nsamples {
                        c.subband[d + k] = clip23(mul(c.subband[s + k], scale, 17) as i64);
                    }
                }
            }
        }
        *sub_pos = ofs;
        Some(())
    }

    /// Sub-band synthesis, LFE interpolation and the stereo mix.
    fn synthesize(&mut self, out: &mut Vec<i16>) {
        let c = &mut *self.c;
        let nsamples = c.npcmblocks * 32;
        let mut spk: [Vec<f32>; 7] = core::array::from_fn(|_| Vec::new());
        let filter: &[f32; 512] = if c.filter_perfect { &FIR_PERFECT } else { &FIR_NONPERFECT };
        let scale = 1.0f32 / (1 << 17) as f32;
        for ch in 0..c.nchannels {
            let s = PRM_TO_SPKR[c.audio_mode][ch];
            if s < 0 {
                continue;
            }
            let mut o = vec![0f32; nsamples];
            for j in 0..c.npcmblocks {
                let mut input = [0f32; 32];
                for i in 0..32 {
                    let v = c.subband[(ch * SUBBANDS + i) * STRIDE + ADPCM + j] as f32;
                    input[i] = if (i as i32 - 1) & 2 != 0 { -v } else { v };
                }
                // Inverse MDCT as FFmpeg's half transform (av_tx, scale 1).
                let mut full = [0f32; 64];
                self.imdct.run(&input, &mut full);
                let off = c.offset[ch];
                for i in 0..16 {
                    c.hist1[ch][off + i] = full[15 - i] * 32.0;
                    c.hist1[ch][off + 16 + i] = full[32 + i] * -32.0;
                }
                let sb = &c.hist1[ch];
                let h2 = &mut c.hist2[ch];
                let blk = &mut o[j * 32..j * 32 + 32];
                for i in 0..16 {
                    let mut a = h2[i];
                    let mut bb = h2[i + 16];
                    let mut cc = 0f32;
                    let mut d = 0f32;
                    let mut jj = 0;
                    while jj < 512 - off {
                        a += filter[i + jj] * -sb[off + 15 - i + jj];
                        bb += filter[i + jj + 16] * sb[off + i + jj];
                        cc += filter[i + jj + 32] * sb[off + 16 + i + jj];
                        d += filter[i + jj + 48] * sb[off + 31 - i + jj];
                        jj += 64;
                    }
                    while jj < 512 {
                        a += filter[i + jj] * -sb[off + 15 - i + jj - 512];
                        bb += filter[i + jj + 16] * sb[off + i + jj - 512];
                        cc += filter[i + jj + 32] * sb[off + 16 + i + jj - 512];
                        d += filter[i + jj + 48] * sb[off + 31 - i + jj - 512];
                        jj += 64;
                    }
                    blk[i] = a * scale;
                    blk[i + 16] = bb * scale;
                    h2[i] = cc;
                    h2[i + 16] = d;
                }
                c.offset[ch] = (off + 512 - 32) & 511;
            }
            spk[s as usize] = o;
        }
        if c.lfe_present > 0 {
            let dec = (c.lfe_present == 1) as usize;
            let coeffs: &[f32; 256] = if dec == 1 { &LFE_FIR_128 } else { &LFE_FIR_64 };
            let factor = 64 << dec;
            let ncoeffs = 8 >> dec;
            let nlfe = c.npcmblocks >> (dec + 1);
            let mut o = vec![0f32; nsamples];
            for i in 0..nlfe {
                let base = LFE_HISTORY + i;
                for j in 0..factor / 2 {
                    let (mut a, mut bb) = (0f32, 0f32);
                    for k in 0..ncoeffs {
                        let s = c.lfe[base - k] as f32;
                        a += coeffs[j * ncoeffs + k] * s;
                        bb += coeffs[255 - j * ncoeffs - k] * s;
                    }
                    o[i * factor + j] = a;
                    o[i * factor + factor / 2 + j] = bb;
                }
            }
            for n in (0..LFE_HISTORY).rev() {
                c.lfe[n] = c.lfe[nlfe + n];
            }
            spk[SP_LFE] = o;
        }
        // Sum/difference coding.
        let butterfly = |spk: &mut [Vec<f32>; 7], a: usize, b: usize| {
            if spk[a].len() == nsamples && spk[b].len() == nsamples {
                for i in 0..nsamples {
                    let (x, y) = (spk[a][i], spk[b][i]);
                    spk[a][i] = x + y;
                    spk[b][i] = x - y;
                }
            }
        };
        if (c.sumdiff_front && c.audio_mode > 0) || c.audio_mode == 3 {
            butterfly(&mut spk, SP_L, SP_R);
        }
        if c.sumdiff_surround && c.audio_mode >= 8 {
            butterfly(&mut spk, SP_LS, SP_RS);
        }
        // Stereo mix (as FFmpeg's resampler mixes 5.1 to stereo; LFE left out).
        const H: f32 = core::f32::consts::FRAC_1_SQRT_2;
        let has = |s: usize| spk[s].len() == nsamples;
        let base = out.len();
        out.resize(base + nsamples * 2, 0);
        let o = &mut out[base..];
        if c.audio_mode == 0 {
            for i in 0..nsamples {
                let v = to_i16(spk[SP_C][i]);
                o[2 * i] = v;
                o[2 * i + 1] = v;
            }
            return;
        }
        let (cl, sl, cs) = (if has(SP_C) { H } else { 0.0 }, if has(SP_LS) { H } else { 0.0 }, if has(SP_CS) { H * H } else { 0.0 });
        let norm = 1.0 / (1.0 + cl + sl + cs);
        let get = |s: usize, i: usize| if has(s) { spk[s][i] } else { 0.0 };
        for i in 0..nsamples {
            let mut l = get(SP_L, i);
            let mut r = get(SP_R, i);
            if cl > 0.0 {
                l += cl * get(SP_C, i);
                r += cl * get(SP_C, i);
            }
            if sl > 0.0 {
                l += sl * get(SP_LS, i);
                r += sl * get(SP_RS, i);
            }
            if cs > 0.0 {
                l += cs * get(SP_CS, i);
                r += cs * get(SP_CS, i);
            }
            if norm < 1.0 {
                l *= norm;
                r *= norm;
            }
            o[2 * i] = to_i16(l);
            o[2 * i + 1] = to_i16(r);
        }
    }
}

fn to_i16(x: f32) -> i16 {
    libm::rintf(x * 32768.0).clamp(-32768.0, 32767.0) as i16
}

/// The start of a core frame, in the usual 16-bit big- or little-endian layout. Returns its
/// length in bytes and whether it's little-endian.
fn frame_at(d: &[u8]) -> Option<(usize, bool)> {
    if d.len() < 10 {
        return None;
    }
    let le = match d[0..4] {
        [0x7F, 0xFE, 0x80, 0x01] => false,
        [0xFE, 0x7F, 0x01, 0x80] => true,
        _ => return None,
    };
    let w = |i: usize| if le { [d[i + 1], d[i]] } else { [d[i], d[i + 1]] };
    let h = [w(4), w(6), w(8)];
    let bytes = [h[0][0], h[0][1], h[1][0], h[1][1], h[2][0], h[2][1]];
    // frame size: 14 bits after normal(1) deficit(5) crc(1) blocks(7) = bit 14 of byte 4.
    let v = u64::from_be_bytes([0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4], bytes[5]]);
    let size = ((v >> (48 - 14 - 14)) & 0x3FFF) as usize + 1;
    if size < 96 {
        return None;
    }
    Some((size, le))
}

impl AudioDec for DtsDec {
    fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let mut p = 0;
        while p + 10 <= self.buf.len() {
            let Some((len, le)) = frame_at(&self.buf[p..]) else {
                p += 1;
                continue;
            };
            if p + len > self.buf.len() {
                break;
            }
            let mut f = self.buf[p..p + len].to_vec();
            if le {
                for w in f.chunks_exact_mut(2) {
                    w.swap(0, 1);
                }
            }
            let before = out.len();
            if self.frame(&f, &mut out).is_none() {
                out.truncate(before);
            }
            p += len;
        }
        self.buf.drain(..p);
        if self.buf.len() > 65536 {
            self.buf.clear();
        }
        out
    }
    fn rate(&self) -> u32 {
        self.rate
    }
    fn reset(&mut self) {
        self.buf.clear();
        let c = &mut *self.c;
        c.subband.fill(0);
        c.lfe = [0; LFE_HISTORY + MAX_BLOCKS];
        c.hist1 = [[0.0; 512]; CHANNELS_MAX];
        c.hist2 = [[0.0; 32]; CHANNELS_MAX];
        c.offset = [0; CHANNELS_MAX];
    }
}

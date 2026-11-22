//! AC-3 (Dolby Digital) sound: DVDs, TV recordings and many films. Follows the ATSC A/52
//! standard, checked against FFmpeg's decoder (whose layout this mirrors); surround sound
//! (5.1) is mixed down to stereo the way the stream asks.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::ac3tab::*;
use super::dec::AudioDec;
use crate::video::aac::{kbd, Imdct};
use crate::video::bits::Bits;

/// Channel 0 is the coupling channel; then up to five full channels and the LFE ("bass").
const MAXCH: usize = 7;
const CPL: usize = 0;

const EXP_REUSE: u8 = 0;
const DBA_REUSE: u8 = 0;
const DBA_NEW: u8 = 1;
const DBA_NONE: u8 = 2;

/// Gains in steps of 1.5 dB (+3, +1.5, 0, -1.5, -3, -4.5, -6, off, -9).
const GAIN_LEVELS: [f32; 9] = [1.4142135, 1.1892071, 1.0, 0.8408964, 0.70710677, 0.59460354, 0.5, 0.0, 0.35355338];

/// Default stereo mix per channel layout: gain levels for (left, right).
const DEFAULT_MIX: [[[u8; 2]; 5]; 8] = [
    [[2, 7], [7, 2], [0, 0], [0, 0], [0, 0]],
    [[4, 4], [0, 0], [0, 0], [0, 0], [0, 0]],
    [[2, 7], [7, 2], [0, 0], [0, 0], [0, 0]],
    [[2, 7], [5, 5], [7, 2], [0, 0], [0, 0]],
    [[2, 7], [7, 2], [6, 6], [0, 0], [0, 0]],
    [[2, 7], [5, 5], [7, 2], [8, 8], [0, 0]],
    [[2, 7], [7, 2], [6, 7], [7, 6], [0, 0]],
    [[2, 7], [5, 5], [7, 2], [6, 7], [7, 6]],
];

/// Full-bandwidth channels per channel layout (acmod).
const NFCHANS: [usize; 8] = [2, 1, 2, 3, 3, 4, 4, 5];

const REMATRIX_BANDS: [usize; 5] = [13, 25, 37, 61, 253];

fn symmetric(code: i32, levels: i32) -> i32 {
    ((code - (levels >> 1)) << 24) / levels
}

/// Everything carried from block to block (and frame to frame, as FFmpeg does).
struct St {
    fbw: usize,
    nch: usize,
    lfe: usize,
    acmod: usize,
    block_switch: [bool; MAXCH],
    dither: [bool; MAXCH],
    dynrng: [f32; 2],
    cpl_in_use: bool,
    in_cpl: [bool; MAXCH],
    phase_in_use: bool,
    start: [usize; MAXCH],
    end: [usize; MAXCH],
    ncplbands: usize,
    cpl_band_sizes: [usize; 18],
    cpl_coords: [[i32; 18]; MAXCH],
    phase: [bool; 18],
    nremat: usize,
    remat: [bool; 4],
    expstr: [u8; MAXCH],
    ngrps: [usize; MAXCH],
    dexps: [[u8; 256]; MAXCH],
    sr_code: usize,
    sr_shift: u32,
    slow_decay: i32,
    fast_decay: i32,
    slow_gain: i32,
    db_per_bit: i32,
    floor: i32,
    cpl_fast_leak: i32,
    cpl_slow_leak: i32,
    snr: [i32; MAXCH],
    fast_gain: [i32; MAXCH],
    dba_mode: [u8; MAXCH],
    dba_n: [usize; MAXCH],
    dba_off: [[u8; 8]; MAXCH],
    dba_len: [[u8; 8]; MAXCH],
    dba_val: [[u8; 8]; MAXCH],
    psd: [[i32; 256]; MAXCH],
    band_psd: [[i32; 50]; MAXCH],
    mask: [[i32; 50]; MAXCH],
    bap: [[u8; 256]; MAXCH],
    coef: [[f32; 256]; MAXCH],
    // E-AC-3 (Dolby Digital Plus): per-frame syntax switches and strategies.
    eac3: bool,
    num_blocks: usize,
    frame_type: u32,
    block_switch_syntax: bool,
    dither_syntax: bool,
    bit_alloc_syntax: bool,
    fast_gain_syntax: bool,
    dba_syntax: bool,
    skip_syntax: bool,
    snr_offset_strategy: u32,
    cpl_strategy_exists: [bool; 6],
    cpl_in_use_blk: [bool; 6],
    exp_strategy: [[u8; MAXCH]; 6],
    uses_aht: [bool; MAXCH],
    pre_mantissa: Vec<[[i32; 6]; 256]>,
    first_cpl_coords: [bool; MAXCH],
    first_cpl_leak: bool,
    cpl_band_struct: [u8; 18],
    // Spectral extension: high frequencies rebuilt from lower ones.
    spx_in_use: bool,
    uses_spx: [bool; MAXCH],
    first_spx_coords: [bool; MAXCH],
    spx_dst_start: usize,
    spx_src_start: usize,
    spx_dst_end: usize,
    num_spx_bands: usize,
    spx_band_sizes: [usize; 17],
    spx_band_struct: [u8; 17],
    spx_noise_blend: [[f32; 17]; MAXCH],
    spx_signal_blend: [[f32; 17]; MAXCH],
    spx_atten: [i32; MAXCH],
}

impl St {
    fn new() -> Box<St> {
        Box::new(St {
            fbw: 0,
            nch: 0,
            lfe: 0,
            acmod: 0,
            block_switch: [false; MAXCH],
            dither: [false; MAXCH],
            dynrng: [1.0; 2],
            cpl_in_use: false,
            in_cpl: [false; MAXCH],
            phase_in_use: false,
            start: [0; MAXCH],
            end: [0; MAXCH],
            ncplbands: 0,
            cpl_band_sizes: [0; 18],
            cpl_coords: [[0; 18]; MAXCH],
            phase: [false; 18],
            nremat: 0,
            remat: [false; 4],
            expstr: [0; MAXCH],
            ngrps: [0; MAXCH],
            dexps: [[0; 256]; MAXCH],
            sr_code: 0,
            sr_shift: 0,
            slow_decay: 0,
            fast_decay: 0,
            slow_gain: 0,
            db_per_bit: 0,
            floor: 0,
            cpl_fast_leak: 0,
            cpl_slow_leak: 0,
            snr: [0; MAXCH],
            fast_gain: [0; MAXCH],
            dba_mode: [DBA_NONE; MAXCH],
            dba_n: [0; MAXCH],
            dba_off: [[0; 8]; MAXCH],
            dba_len: [[0; 8]; MAXCH],
            dba_val: [[0; 8]; MAXCH],
            psd: [[0; 256]; MAXCH],
            band_psd: [[0; 50]; MAXCH],
            mask: [[0; 50]; MAXCH],
            bap: [[0; 256]; MAXCH],
            coef: [[0.0; 256]; MAXCH],
            eac3: false,
            num_blocks: 6,
            frame_type: 2,
            block_switch_syntax: true,
            dither_syntax: true,
            bit_alloc_syntax: true,
            fast_gain_syntax: false,
            dba_syntax: true,
            skip_syntax: true,
            snr_offset_strategy: 0,
            cpl_strategy_exists: [false; 6],
            cpl_in_use_blk: [false; 6],
            exp_strategy: [[0; MAXCH]; 6],
            uses_aht: [false; MAXCH],
            pre_mantissa: alloc::vec![[[0; 6]; 256]; MAXCH],
            first_cpl_coords: [true; MAXCH],
            first_cpl_leak: false,
            cpl_band_struct: DEFAULT_CPL_BAND_STRUCT,
            spx_in_use: false,
            uses_spx: [false; MAXCH],
            first_spx_coords: [true; MAXCH],
            spx_dst_start: 0,
            spx_src_start: 0,
            spx_dst_end: 0,
            num_spx_bands: 0,
            spx_band_sizes: [0; 17],
            spx_band_struct: DEFAULT_SPX_BAND_STRUCT,
            spx_noise_blend: [[0.0; 17]; MAXCH],
            spx_signal_blend: [[0.0; 17]; MAXCH],
            spx_atten: [-1; MAXCH],
        })
    }
}

/// FFmpeg's noise generator (a lagged Fibonacci generator), seeded from each frame's bytes
/// as FFmpeg does with "consistent noise", so that tests can compare sample for sample.
struct Noise {
    state: [u32; 64],
    index: u32,
}

impl Noise {
    fn seed(crc_tab: &[u32; 256], data: &[u8]) -> Noise {
        let mut state = [0u32; 64];
        let mut crc: u32 = 1;
        let mut beg = 0;
        for (s, slot) in state.iter_mut().enumerate() {
            let end = (s + 1) * data.len() / 64;
            for &b in &data[beg..end] {
                crc = crc_tab[((crc as u8) ^ b) as usize] ^ (crc >> 8);
            }
            *slot = crc;
            beg = end;
        }
        Noise { state, index: 0 }
    }
    fn next(&mut self) -> u32 {
        let i = self.index;
        let a = self.state[(i.wrapping_sub(24) & 63) as usize].wrapping_add(self.state[(i.wrapping_sub(55) & 63) as usize]);
        self.state[(i & 63) as usize] = a;
        self.index = i.wrapping_add(1);
        a
    }
}

/// Grouped mantissas left over from the last group read (they're shared across channels).
#[derive(Default)]
struct Groups {
    b1: [i32; 2],
    b1n: usize,
    b2: [i32; 2],
    b2n: usize,
    b4: i32,
    b4n: usize,
}

pub struct Ac3Dec {
    buf: Vec<u8>,
    rate: u32,
    long: Imdct,
    short: Imdct,
    window: Vec<f32>,
    delay: [[f32; 128]; 5],
    crc: [u32; 256],
    st: Box<St>,
    bap1: [[i32; 3]; 32],
    bap2: [[i32; 3]; 128],
    bap4: [[i32; 2]; 128],
    bap3: [i32; 8],
    bap5: [i32; 16],
}

impl Ac3Dec {
    pub fn new() -> Ac3Dec {
        // CRC-32 table as FFmpeg builds it (polynomial 0x04C11DB7, kept byte-swapped).
        let mut crc = [0u32; 256];
        for (i, e) in crc.iter_mut().enumerate() {
            let mut c = (i as u32) << 24;
            for _ in 0..8 {
                c = (c << 1) ^ (0x04C1_1DB7 & (((c as i32) >> 31) as u32));
            }
            *e = c.swap_bytes();
        }
        let mut bap1 = [[0; 3]; 32];
        for (i, e) in bap1.iter_mut().enumerate() {
            let i = i as i32;
            *e = [symmetric(i / 9, 3), symmetric((i % 9) / 3, 3), symmetric(i % 3, 3)];
        }
        let mut bap2 = [[0; 3]; 128];
        let mut bap4 = [[0; 2]; 128];
        for i in 0..128 {
            let v = i as i32;
            bap2[i] = [symmetric(v / 25, 5), symmetric((v % 25) / 5, 5), symmetric(v % 5, 5)];
            bap4[i] = [symmetric(v / 11, 11), symmetric(v % 11, 11)];
        }
        let mut bap3 = [0; 8];
        for (i, e) in bap3.iter_mut().take(7).enumerate() {
            *e = symmetric(i as i32, 7);
        }
        let mut bap5 = [0; 16];
        for (i, e) in bap5.iter_mut().take(15).enumerate() {
            *e = symmetric(i as i32, 15);
        }
        Ac3Dec {
            buf: Vec::new(),
            rate: 48000,
            long: Imdct::new(512),
            short: Imdct::new(256),
            window: kbd(512, 5.0),
            delay: [[0.0; 128]; 5],
            crc,
            st: St::new(),
            bap1,
            bap2,
            bap4,
            bap3,
            bap5,
        }
    }

    /// One whole frame (starting with the sync word): 256 stereo samples per block (6 blocks
    /// in AC-3; 1, 2, 3 or 6 in E-AC-3). None for a frame that can't be decoded; Some(false)
    /// for one that is skipped on purpose (an E-AC-3 extra substream).
    fn frame(&mut self, f: &[u8], out: &mut Vec<i16>) -> Option<bool> {
        let mut b = Bits::new(f);
        let bsid = f[5] >> 3;
        let (fscod, acmod, lfeon, cmix, smix, sr_shift, num_blocks, frame_type);
        if bsid > 10 {
            // E-AC-3.
            b.skip(16);
            frame_type = b.read(2);
            let substream = b.read(3);
            b.skip(11); // frame size
            if frame_type == 3 {
                return None;
            }
            if frame_type == 1 || substream != 0 {
                // Dependent (extra channels for 7.1) or additional programmes: the main
                // programme already has all the sound for a stereo mix.
                return Some(false);
            }
            fscod = b.read(2) as usize;
            if fscod == 3 {
                return None; // reduced sample rates (FFmpeg doesn't decode them either)
            }
            num_blocks = [1, 2, 3, 6][b.read(2) as usize];
            acmod = b.read(3) as usize;
            lfeon = b.flag();
            b.skip(5); // bsid
            sr_shift = 0;
            let (c, s) = self.eac3_bsi(&mut b, acmod, lfeon, num_blocks, frame_type);
            cmix = c;
            smix = s;
        } else {
            b.skip(32); // sync word, CRC
            fscod = b.read(2) as usize;
            b.skip(6); // frame size
            b.skip(5); // bsid
            if fscod == 3 {
                return None;
            }
            b.skip(3); // service type
            acmod = b.read(3) as usize;
            let (mut c, mut s) = (5usize, 6usize);
            if acmod & 1 != 0 && acmod != 1 {
                c = [4, 5, 6, 5][b.read(2) as usize];
            }
            if acmod & 4 != 0 {
                s = [4, 6, 7, 6][b.read(2) as usize];
            }
            cmix = c;
            smix = s;
            if acmod == 2 {
                b.skip(2); // Dolby Surround
            }
            lfeon = b.flag();
            for _ in 0..if acmod == 0 { 2 } else { 1 } {
                b.skip(5); // dialogue level
                if b.flag() {
                    b.skip(8); // heavy compression
                }
                if b.flag() {
                    b.skip(8); // language
                }
                if b.flag() {
                    b.skip(7); // mixing level, room type
                }
            }
            b.skip(2); // copyright, original
            if b.flag() {
                b.skip(14);
            }
            if b.flag() {
                b.skip(14);
            }
            if b.flag() {
                let len = b.read(6) as usize;
                b.skip((len + 1) * 8);
            }
            sr_shift = bsid.max(8) as u32 - 8;
            num_blocks = 6;
            frame_type = 2;
        }

        self.rate = [48000, 44100, 32000][fscod] >> sr_shift;
        let fbw = NFCHANS[acmod];
        {
            let s = &mut self.st;
            s.fbw = fbw;
            s.nch = fbw + lfeon as usize;
            s.lfe = if lfeon { fbw + 1 } else { MAXCH };
            s.acmod = acmod;
            s.sr_code = fscod;
            s.sr_shift = sr_shift;
            s.eac3 = bsid > 10;
            s.num_blocks = num_blocks;
            s.frame_type = frame_type;
            if lfeon {
                s.start[s.lfe] = 0;
                s.end[s.lfe] = 7;
                s.ngrps[s.lfe] = 2;
                s.in_cpl[s.lfe] = false;
            }
        }
        if self.st.eac3 {
            self.eac3_frame_header(&mut b, f.len())?;
        } else {
            let s = &mut self.st;
            s.block_switch_syntax = true;
            s.dither_syntax = true;
            s.bit_alloc_syntax = true;
            s.fast_gain_syntax = false;
            s.dba_syntax = true;
            s.skip_syntax = true;
            s.snr_offset_strategy = 0;
            s.uses_aht = [false; MAXCH];
            s.first_cpl_leak = false;
        }

        // The stereo mix (FFmpeg's set_downmix_coeffs).
        let mut mix = [[0f32; 2]; 5];
        for (i, m) in mix.iter_mut().enumerate().take(fbw) {
            *m = [GAIN_LEVELS[DEFAULT_MIX[acmod][i][0] as usize], GAIN_LEVELS[DEFAULT_MIX[acmod][i][1] as usize]];
        }
        if acmod > 1 && acmod & 1 != 0 {
            mix[1] = [GAIN_LEVELS[cmix]; 2];
        }
        if acmod == 4 || acmod == 5 {
            mix[acmod - 2] = [GAIN_LEVELS[smix] * GAIN_LEVELS[4]; 2];
        }
        if acmod == 6 || acmod == 7 {
            let nf = acmod - 4;
            mix[nf][0] = GAIN_LEVELS[smix];
            mix[nf + 1][1] = GAIN_LEVELS[smix];
        }
        let (n0, n1) = mix[..fbw].iter().fold((0f32, 0f32), |(a, c), m| (a + m[0], c + m[1]));
        for m in mix[..fbw].iter_mut() {
            m[0] /= n0;
            m[1] /= n1;
        }

        let mut noise = Noise::seed(&self.crc, f);
        let base = out.len();
        out.resize(base + num_blocks * 256 * 2, 0);
        let mut pcm = [[0f32; 256]; 5];
        for blk in 0..num_blocks {
            self.block(&mut b, blk, &mut noise)?;
            for ch in 0..fbw {
                self.transform(ch, &mut pcm[ch]);
            }
            let o = &mut out[base + blk * 512..base + blk * 512 + 512];
            for i in 0..256 {
                let (mut l, mut r) = (0f32, 0f32);
                for ch in 0..fbw {
                    l += mix[ch][0] * pcm[ch][i];
                    r += mix[ch][1] * pcm[ch][i];
                }
                o[2 * i] = to_i16(l);
                o[2 * i + 1] = to_i16(r);
            }
        }
        Some(true)
    }

    /// The rest of an E-AC-3 frame's stream information; returns the centre and surround mix
    /// levels for the stereo mix.
    fn eac3_bsi(&mut self, b: &mut Bits, acmod: usize, lfeon: bool, num_blocks: usize, frame_type: u32) -> (usize, usize) {
        let (mut cmix, mut smix) = (5usize, 6usize);
        let progs = if acmod == 0 { 2 } else { 1 };
        for _ in 0..progs {
            b.skip(5); // dialogue level
            if b.flag() {
                b.skip(8); // heavy compression
            }
        }
        if frame_type == 1 && b.flag() {
            b.skip(16); // channel map
        }
        if b.flag() {
            // Mixing metadata.
            if acmod > 2 {
                b.skip(2); // preferred mix
                if acmod & 1 != 0 {
                    b.skip(3); // Lt/Rt centre level
                    cmix = b.read(3) as usize;
                }
                if acmod & 4 != 0 {
                    b.skip(3); // Lt/Rt surround level
                    smix = (b.read(3) as usize).clamp(3, 7);
                }
            }
            if lfeon && b.flag() {
                b.skip(5); // LFE mix level
            }
            if frame_type == 0 {
                for _ in 0..progs {
                    if b.flag() {
                        b.skip(6);
                    }
                }
                if b.flag() {
                    b.skip(6);
                }
                match b.read(2) {
                    1 => b.skip(5),
                    2 => b.skip(12),
                    3 => {
                        let n = (b.read(5) as usize + 2) << 3;
                        b.skip(n);
                    }
                    _ => {}
                }
                if acmod < 2 {
                    for _ in 0..progs {
                        if b.flag() {
                            b.skip(14); // pan
                        }
                    }
                }
                if b.flag() {
                    for _ in 0..num_blocks {
                        if num_blocks == 1 || b.flag() {
                            b.skip(5);
                        }
                    }
                }
            }
        }
        if b.flag() {
            // Informational metadata.
            b.skip(3 + 2);
            if acmod == 2 {
                b.skip(4);
            }
            if acmod >= 6 {
                b.skip(2);
            }
            for _ in 0..progs {
                if b.flag() {
                    b.skip(8);
                }
            }
            b.skip(1); // source sample rate
        }
        if frame_type == 0 && num_blocks != 6 {
            b.skip(1); // converter sync
        }
        if frame_type == 2 && (num_blocks == 6 || b.flag()) {
            b.skip(6);
        }
        if b.flag() {
            let n = b.read(6) as usize + 1;
            b.skip(n * 8);
        }
        (cmix, smix)
    }

    /// E-AC-3's audio frame header: syntax switches, coupling and exponent strategies for all
    /// blocks, and which channels use the hybrid transform.
    fn eac3_frame_header(&mut self, b: &mut Bits, frame_size: usize) -> Option<()> {
        let s = &mut *self.st;
        let (fbw, nb) = (s.fbw, s.num_blocks);
        let (ac3_expstr, parse_aht) = if nb == 6 { (b.flag(), b.flag()) } else { (true, false) };
        s.snr_offset_strategy = b.read(2);
        let transient = b.flag();
        s.block_switch_syntax = b.flag();
        if !s.block_switch_syntax {
            s.block_switch = [false; MAXCH];
        }
        s.dither_syntax = b.flag();
        if !s.dither_syntax {
            for ch in 1..=fbw {
                s.dither[ch] = true;
            }
        }
        s.dither[CPL] = false;
        if s.lfe < MAXCH {
            s.dither[s.lfe] = false;
        }
        s.bit_alloc_syntax = b.flag();
        if !s.bit_alloc_syntax {
            s.slow_decay = SLOW_DECAY[2] as i32;
            s.fast_decay = FAST_DECAY[1] as i32;
            s.slow_gain = SLOW_GAIN[1] as i32;
            s.db_per_bit = DB_PER_BIT[2] as i32;
            s.floor = FLOOR[7] as i32;
        }
        s.fast_gain_syntax = b.flag();
        s.dba_syntax = b.flag();
        s.skip_syntax = b.flag();
        let spx_atten = b.flag();
        let mut ncpl = 0;
        if s.acmod > 1 {
            for blk in 0..nb {
                s.cpl_strategy_exists[blk] = blk == 0 || b.flag();
                s.cpl_in_use_blk[blk] = if s.cpl_strategy_exists[blk] { b.flag() } else { s.cpl_in_use_blk[blk - 1] };
                ncpl += s.cpl_in_use_blk[blk] as usize;
            }
        } else {
            s.cpl_in_use_blk = [false; 6];
            s.cpl_strategy_exists = [false; 6];
        }
        if ac3_expstr {
            for blk in 0..nb {
                for ch in (!s.cpl_in_use_blk[blk]) as usize..=fbw {
                    s.exp_strategy[blk][ch] = b.read(2) as u8;
                }
            }
        } else {
            for ch in (!(s.acmod > 1 && ncpl > 0)) as usize..=fbw {
                let code = b.read(5) as usize;
                for blk in 0..6 {
                    s.exp_strategy[blk][ch] = FRM_EXPSTR[code * 6 + blk];
                }
            }
        }
        if s.lfe < MAXCH {
            for blk in 0..nb {
                s.exp_strategy[blk][s.lfe] = b.read(1) as u8;
            }
        }
        if s.frame_type == 0 && (nb == 6 || b.flag()) {
            b.skip(5 * fbw); // converter exponent strategies
        }
        s.uses_aht = [false; MAXCH];
        if parse_aht {
            for ch in (ncpl != 6) as usize..=s.nch {
                let mut ok = true;
                for blk in 1..6 {
                    if s.exp_strategy[blk][ch] != EXP_REUSE || (ch == CPL && s.cpl_strategy_exists[blk]) {
                        ok = false;
                        break;
                    }
                }
                s.uses_aht[ch] = ok && b.flag();
            }
        }
        if s.snr_offset_strategy == 0 {
            let csnr = (b.read(6) as i32 - 15) << 4;
            let snr = (csnr + b.read(4) as i32) << 2;
            for ch in 0..=s.nch {
                s.snr[ch] = snr;
            }
        }
        if transient {
            for _ in 1..=fbw {
                if b.flag() {
                    b.skip(10 + 8);
                }
            }
        }
        for ch in 1..=fbw {
            s.spx_atten[ch] = if spx_atten && b.flag() { b.read(5) as i32 } else { -1 };
        }
        if nb > 1 && b.flag() {
            let log2 = 31 - ((frame_size.max(3) - 2) as u32).leading_zeros();
            b.skip((nb - 1) * (4 + log2 as usize));
        }
        for ch in 1..=fbw {
            s.first_spx_coords[ch] = true;
            s.first_cpl_coords[ch] = true;
        }
        s.first_cpl_leak = true;
        Some(())
    }

    /// One audio block's side information and coefficients.
    fn block(&mut self, b: &mut Bits, blk: usize, noise: &mut Noise) -> Option<()> {
        let s = &mut *self.st;
        let fbw = s.fbw;
        let eac3 = s.eac3;
        if s.block_switch_syntax {
            for ch in 1..=fbw {
                s.block_switch[ch] = b.flag();
            }
        }
        if s.dither_syntax {
            for ch in 1..=fbw {
                s.dither[ch] = b.flag();
            }
        }
        // Dynamic range (quieter loud parts, louder quiet parts).
        let mut i = (s.acmod == 0) as usize;
        loop {
            if b.flag() {
                let v = b.read(8) as i32;
                let e = (v >> 5) - ((v >> 7) << 3) - 5;
                let m = ((v & 31) | 32) as f32;
                s.dynrng[i] = if e >= 0 { m * (1u32 << e) as f32 } else { m / (1u32 << -e) as f32 };
            } else if blk == 0 {
                s.dynrng[i] = 1.0;
            }
            if i == 0 {
                break;
            }
            i -= 1;
        }

        // Spectral extension (E-AC-3).
        if eac3 && (blk == 0 || b.flag()) {
            s.spx_in_use = b.flag();
            if s.spx_in_use {
                s.spx_strategy(b, blk)?;
            }
        }
        if !eac3 || !s.spx_in_use {
            s.spx_in_use = false;
            for ch in 1..=fbw {
                s.uses_spx[ch] = false;
                s.first_spx_coords[ch] = true;
            }
        }
        if s.spx_in_use {
            s.spx_coordinates(b);
        }

        // Coupling strategy.
        let new_cpl = if eac3 { s.cpl_strategy_exists[blk] } else { b.flag() };
        if new_cpl {
            if !eac3 {
                s.cpl_in_use_blk[blk] = b.flag();
            }
            if s.cpl_in_use_blk[blk] {
                if s.acmod < 2 {
                    return None;
                }
                if eac3 && b.flag() {
                    return None; // enhanced coupling (FFmpeg doesn't decode it either)
                }
                if eac3 && s.acmod == 2 {
                    s.in_cpl[1] = true;
                    s.in_cpl[2] = true;
                } else {
                    for ch in 1..=fbw {
                        s.in_cpl[ch] = b.flag();
                    }
                }
                if s.acmod == 2 {
                    s.phase_in_use = b.flag();
                }
                let begin = b.read(4) as usize;
                let end = if s.spx_in_use { (s.spx_src_start - 37) / 12 } else { b.read(4) as usize + 3 };
                if begin >= end {
                    return None;
                }
                s.start[CPL] = begin * 12 + 37;
                s.end[CPL] = end * 12 + 37;
                let (n, sizes) = band_structure(b, blk, eac3, begin, end, &DEFAULT_CPL_BAND_STRUCT, &mut s.cpl_band_struct);
                s.ncplbands = n;
                for (k, v) in sizes.iter().enumerate().take(18) {
                    s.cpl_band_sizes[k] = *v as usize;
                }
            } else {
                for ch in 1..=fbw {
                    s.in_cpl[ch] = false;
                    s.first_cpl_coords[ch] = true;
                }
                s.first_cpl_leak = eac3;
                s.phase_in_use = false;
            }
        } else if !eac3 {
            if blk == 0 {
                return None;
            }
            s.cpl_in_use_blk[blk] = s.cpl_in_use_blk[blk - 1];
        }
        let cpl = s.cpl_in_use_blk[blk];
        s.cpl_in_use = cpl;

        // Coupling coordinates.
        if cpl {
            let mut any = false;
            for ch in 1..=fbw {
                if s.in_cpl[ch] {
                    if (eac3 && s.first_cpl_coords[ch]) || b.flag() {
                        s.first_cpl_coords[ch] = false;
                        any = true;
                        let master = 3 * b.read(2) as i32;
                        for bnd in 0..s.ncplbands {
                            let e = b.read(4) as i32;
                            let m = b.read(4) as i32;
                            let v = if e == 15 { m << 22 } else { (m + 16) << 21 };
                            s.cpl_coords[ch][bnd] = v >> (e + master);
                        }
                    } else if blk == 0 {
                        return None;
                    }
                } else {
                    s.first_cpl_coords[ch] = true;
                }
            }
            if s.acmod == 2 && any {
                for bnd in 0..s.ncplbands {
                    s.phase[bnd] = s.phase_in_use && b.flag();
                }
            }
        }

        // Stereo rematrixing.
        if s.acmod == 2 {
            if (eac3 && blk == 0) || b.flag() {
                s.nremat = 4;
                if cpl && s.start[CPL] <= 61 {
                    s.nremat -= 1 + (s.start[CPL] == 37) as usize;
                } else if s.spx_in_use && s.spx_src_start <= 61 {
                    s.nremat -= 1;
                }
                for bnd in 0..s.nremat {
                    s.remat[bnd] = b.flag();
                }
            } else if blk == 0 {
                s.nremat = 0;
            }
        }

        // Exponent strategies, bandwidths and exponents.
        let first = !cpl as usize;
        let nch = s.nch;
        for ch in first..=nch {
            s.expstr[ch] = if eac3 { s.exp_strategy[blk][ch] } else { b.read(if ch == s.lfe { 1 } else { 2 }) as u8 };
        }
        for ch in 1..=fbw {
            s.start[ch] = 0;
            if s.expstr[ch] != EXP_REUSE {
                if s.in_cpl[ch] {
                    s.end[ch] = s.start[CPL];
                } else if s.uses_spx[ch] {
                    s.end[ch] = s.spx_src_start;
                } else {
                    let code = b.read(6) as usize;
                    if code > 60 {
                        return None;
                    }
                    s.end[ch] = code * 3 + 73;
                }
                let g = 3 << (s.expstr[ch] - 1);
                s.ngrps[ch] = (s.end[ch] + g - 4) / g;
            }
        }
        if cpl && s.expstr[CPL] != EXP_REUSE {
            s.ngrps[CPL] = (s.end[CPL] - s.start[CPL]) / (3 << (s.expstr[CPL] - 1));
        }
        for ch in first..=nch {
            if s.expstr[ch] != EXP_REUSE {
                let abs = (b.read(4) << (ch == CPL) as u32) as i32;
                let at = s.start[ch] + (ch != CPL) as usize;
                s.dexps[ch][0] = abs as u8;
                let size = match s.expstr[ch] {
                    1 => 1,
                    2 => 2,
                    _ => 4,
                };
                let mut prev = abs;
                let mut j = at;
                for _ in 0..s.ngrps[ch] {
                    let acc = b.read(7) as i32;
                    if acc >= 125 {
                        return None;
                    }
                    for d in [acc / 25, (acc % 25) / 5, acc % 5] {
                        prev += d - 2;
                        if !(0..=24).contains(&prev) {
                            return None;
                        }
                        for _ in 0..size {
                            if j < 256 {
                                s.dexps[ch][j] = prev as u8;
                            }
                            j += 1;
                        }
                    }
                }
                if ch != CPL && ch != s.lfe {
                    b.skip(2); // gain range
                }
            }
        }

        // Bit allocation parameters.
        if s.bit_alloc_syntax {
            if b.flag() {
                s.slow_decay = (SLOW_DECAY[b.read(2) as usize] as i32) >> s.sr_shift;
                s.fast_decay = (FAST_DECAY[b.read(2) as usize] as i32) >> s.sr_shift;
                s.slow_gain = SLOW_GAIN[b.read(2) as usize] as i32;
                s.db_per_bit = DB_PER_BIT[b.read(2) as usize] as i32;
                s.floor = FLOOR[b.read(3) as usize] as i32;
            } else if blk == 0 {
                return None;
            }
        }
        if !eac3 || blk == 0 {
            if (!eac3 || s.snr_offset_strategy != 0) && b.flag() {
                let csnr = (b.read(6) as i32 - 15) << 4;
                let mut snr = 0;
                for ch in first..=nch {
                    if ch == first || s.snr_offset_strategy == 2 || !eac3 {
                        snr = (csnr + b.read(4) as i32) << 2;
                    }
                    s.snr[ch] = snr;
                    if !eac3 {
                        s.fast_gain[ch] = FAST_GAIN[b.read(3) as usize] as i32;
                    }
                }
            } else if !eac3 && blk == 0 {
                return None;
            }
        }
        if s.fast_gain_syntax && b.flag() {
            for ch in first..=nch {
                s.fast_gain[ch] = FAST_GAIN[b.read(3) as usize] as i32;
            }
        } else if eac3 && blk == 0 {
            for ch in first..=nch {
                s.fast_gain[ch] = FAST_GAIN[4] as i32;
            }
        }
        if s.frame_type == 0 && b.flag() {
            b.skip(10); // converter SNR offset
        }
        if cpl {
            if s.first_cpl_leak || b.flag() {
                s.cpl_fast_leak = b.read(3) as i32;
                s.cpl_slow_leak = b.read(3) as i32;
            } else if !eac3 && blk == 0 {
                return None;
            }
            s.first_cpl_leak = false;
        }
        if s.dba_syntax && b.flag() {
            for ch in first..=fbw {
                s.dba_mode[ch] = b.read(2) as u8;
                if s.dba_mode[ch] == 3 {
                    return None;
                }
            }
            for ch in first..=fbw {
                if s.dba_mode[ch] == DBA_NEW {
                    s.dba_n[ch] = b.read(3) as usize + 1;
                    for seg in 0..s.dba_n[ch] {
                        s.dba_off[ch][seg] = b.read(5) as u8;
                        s.dba_len[ch][seg] = b.read(4) as u8;
                        s.dba_val[ch][seg] = b.read(3) as u8;
                    }
                }
            }
        } else if blk == 0 {
            for ch in 0..=nch {
                s.dba_mode[ch] = DBA_NONE;
            }
        }
        for ch in first..=nch {
            s.allocate(ch)?;
        }
        if s.skip_syntax && b.flag() {
            let len = b.read(9) as usize;
            b.skip(len * 8); // unused data
        }

        // Mantissas. The coupling channel comes right after the first coupled channel.
        let mut g = Groups::default();
        let mut got_cpl = false;
        for ch in 1..=nch {
            self.coefficients(b, ch, blk, &mut g, noise);
            let s = &mut *self.st;
            let mut end = s.end[ch];
            if s.in_cpl[ch] {
                if !got_cpl {
                    self.coefficients(b, CPL, blk, &mut g, noise);
                    self.st.uncouple();
                    got_cpl = true;
                }
                end = self.st.end[CPL];
            }
            for c in self.st.coef[ch][end.min(256)..].iter_mut() {
                *c = 0.0;
            }
        }
        let s = &mut *self.st;
        // Coupled channels without dither get silence where the coupling channel had no bits.
        for ch in 1..=fbw {
            if !s.dither[ch] && s.in_cpl[ch] {
                for i in s.start[CPL]..s.end[CPL] {
                    if s.bap[CPL][i] == 0 {
                        s.coef[ch][i] = 0.0;
                    }
                }
            }
        }
        if s.acmod == 2 {
            let end = s.end[1].min(s.end[2]);
            for bnd in 0..s.nremat {
                if s.remat[bnd] {
                    for i in REMATRIX_BANDS[bnd]..end.min(REMATRIX_BANDS[bnd + 1]) {
                        let (l, r) = (s.coef[1][i], s.coef[2][i]);
                        s.coef[1][i] = l + r;
                        s.coef[2][i] = l - r;
                    }
                }
            }
        }
        for ch in 1..=nch {
            let audio = if s.acmod == 0 && ch <= 2 { 2 - ch } else { 0 };
            let gain = s.dynrng[audio] * (1.0 / 4_194_304.0);
            for c in s.coef[ch].iter_mut() {
                *c *= gain;
            }
        }
        if s.spx_in_use {
            s.apply_spx(noise);
        }
        Some(())
    }

    /// A channel's coefficients: normal mantissas, or (E-AC-3's hybrid transform) the values
    /// for all six blocks, which come in the first block.
    fn coefficients(&mut self, b: &mut Bits, ch: usize, blk: usize, g: &mut Groups, noise: &mut Noise) {
        if !self.st.uses_aht[ch] {
            self.mantissas(b, ch, g, noise);
            return;
        }
        if blk == 0 {
            self.st.aht_mantissas(b, ch, noise);
        }
        let s = &mut *self.st;
        for f in s.start[ch]..s.end[ch].min(256) {
            s.coef[ch][f] = s.pre_mantissa[ch][f][blk] as f32 * SCALE[s.dexps[ch][f] as usize];
        }
    }

    fn mantissas(&mut self, b: &mut Bits, ch: usize, g: &mut Groups, noise: &mut Noise) {
        let s = &mut *self.st;
        let dither = ch == CPL || s.dither[ch];
        for f in s.start[ch]..s.end[ch].min(256) {
            let bap = s.bap[ch][f];
            let m: i32 = match bap {
                0 => {
                    if dither {
                        ((((noise.next() >> 8) * 181) >> 8) as i32) - 5_931_008
                    } else {
                        0
                    }
                }
                1 => {
                    if g.b1n > 0 {
                        g.b1n -= 1;
                        g.b1[g.b1n]
                    } else {
                        let v = self.bap1[b.read(5) as usize];
                        g.b1 = [v[2], v[1]];
                        g.b1n = 2;
                        v[0]
                    }
                }
                2 => {
                    if g.b2n > 0 {
                        g.b2n -= 1;
                        g.b2[g.b2n]
                    } else {
                        let v = self.bap2[b.read(7) as usize];
                        g.b2 = [v[2], v[1]];
                        g.b2n = 2;
                        v[0]
                    }
                }
                3 => self.bap3[b.read(3) as usize],
                4 => {
                    if g.b4n > 0 {
                        g.b4n = 0;
                        g.b4
                    } else {
                        let v = self.bap4[b.read(7) as usize];
                        g.b4 = v[1];
                        g.b4n = 1;
                        v[0]
                    }
                }
                5 => self.bap5[b.read(4) as usize],
                _ => {
                    let n = QUANT_BITS[bap.min(15) as usize] as u32;
                    let v = b.read(n) as i32;
                    let v = (v << (32 - n)) >> (32 - n); // sign-extend
                    v << (24 - n)
                }
            };
            s.coef[ch][f] = m as f32 * SCALE[s.dexps[ch][f] as usize];
        }
    }

    /// Coefficients to sound for full channel `ch` (0-based): one block of 256 samples.
    fn transform(&mut self, ch: usize, pcm: &mut [f32; 256]) {
        let c = &self.st.coef[ch + 1];
        let mut tmp = [0f32; 256];
        if self.st.block_switch[ch + 1] {
            // Two short transforms: even and odd coefficients.
            let mut x = [0f32; 128];
            let mut full = [0f32; 256];
            for i in 0..128 {
                x[i] = c[2 * i];
            }
            self.short.run(&x, &mut full);
            for i in 0..128 {
                tmp[i] = full[64 + i] * -128.0;
            }
            window(pcm, &self.delay[ch], &tmp, &self.window);
            for i in 0..128 {
                x[i] = c[2 * i + 1];
            }
            self.short.run(&x, &mut full);
            for i in 0..128 {
                self.delay[ch][i] = full[64 + i] * -128.0;
            }
        } else {
            let mut full = [0f32; 512];
            self.long.run(c, &mut full);
            for i in 0..256 {
                tmp[i] = full[128 + i] * -256.0;
            }
            window(pcm, &self.delay[ch], &tmp, &self.window);
            self.delay[ch].copy_from_slice(&tmp[128..]);
        }
    }
}

/// Overlap the saved half of the last block with this one (FFmpeg's vector_fmul_window).
fn window(out: &mut [f32; 256], delay: &[f32; 128], tmp: &[f32], w: &[f32]) {
    for i in 0..128 {
        let (s0, s1) = (delay[i], tmp[127 - i]);
        let (wi, wj) = (w[i], w[255 - i]);
        out[i] = s0 * wj - s1 * wi;
        out[255 - i] = s0 * wi + s1 * wj;
    }
}

fn to_i16(v: f32) -> i16 {
    let x = v * 32768.0;
    let r = if x >= 0.0 { x + 0.5 } else { x - 0.5 };
    r.clamp(-32768.0, 32767.0) as i16
}

/// 2^-exponent.
const SCALE: [f32; 25] = {
    let mut t = [0f32; 25];
    let mut i = 0;
    let mut v = 1.0f32;
    while i < 25 {
        t[i] = v;
        v *= 0.5;
        i += 1;
    }
    t
};

impl St {
    /// Bit allocation for one channel: how many bits each coefficient got.
    fn allocate(&mut self, ch: usize) -> Option<()> {
        let (start, end) = (self.start[ch], self.end[ch].min(256));
        if end <= start {
            return Some(());
        }
        // Exponents to power spectral density, and per band.
        for bin in start..end {
            self.psd[ch][bin] = 3072 - ((self.dexps[ch][bin] as i32) << 7);
        }
        let psd = &self.psd[ch];
        let bpsd = &mut self.band_psd[ch];
        let mut bin = start;
        while bin < end.min(28) {
            bpsd[bin] = psd[bin];
            bin += 1;
        }
        if bin < end {
            let mut band = BIN_TO_BAND[bin] as usize;
            loop {
                let mut v = psd[bin];
                bin += 1;
                let band_end = (BAND_START[band + 1] as usize).min(end);
                while bin < band_end {
                    let max = v.max(psd[bin]);
                    let adr = (max - ((v + psd[bin] + 1) >> 1)).min(255);
                    v = max + LOG_ADD[adr as usize] as i32;
                    bin += 1;
                }
                bpsd[band] = v;
                band += 1;
                if end <= BAND_START[band] as usize {
                    break;
                }
            }
        }

        // Excitation and masking curve.
        let is_lfe = ch == self.lfe;
        let fast_gain = self.fast_gain[ch];
        let bpsd = &self.band_psd[ch];
        let mut excite = [0i32; 50];
        let band_start = BIN_TO_BAND[start] as usize;
        let band_end = BIN_TO_BAND[end - 1] as usize + 1;
        let (mut fastleak, mut slowleak);
        let mut begin;
        if band_start == 0 {
            let mut lowcomp = lowcomp1(0, bpsd[0], bpsd[1], 384);
            excite[0] = bpsd[0] - fast_gain - lowcomp;
            lowcomp = lowcomp1(lowcomp, bpsd[1], bpsd[2], 384);
            excite[1] = bpsd[1] - fast_gain - lowcomp;
            begin = 7;
            fastleak = 0;
            slowleak = 0;
            for band in 2..7 {
                if !(is_lfe && band == 6) {
                    lowcomp = lowcomp1(lowcomp, bpsd[band], bpsd[band + 1], 384);
                }
                fastleak = bpsd[band] - fast_gain;
                slowleak = bpsd[band] - self.slow_gain;
                excite[band] = fastleak - lowcomp;
                if !(is_lfe && band == 6) && bpsd[band] <= bpsd[band + 1] {
                    begin = band + 1;
                    break;
                }
            }
            for band in begin..band_end.min(22) {
                if !(is_lfe && band == 6) {
                    lowcomp = if band < 7 {
                        lowcomp1(lowcomp, bpsd[band], bpsd[band + 1], 384)
                    } else if band < 20 {
                        lowcomp1(lowcomp, bpsd[band], bpsd[band + 1], 320)
                    } else {
                        (lowcomp - 128).max(0)
                    };
                }
                fastleak = (fastleak - self.fast_decay).max(bpsd[band] - fast_gain);
                slowleak = (slowleak - self.slow_decay).max(bpsd[band] - self.slow_gain);
                excite[band] = (fastleak - lowcomp).max(slowleak);
            }
            begin = 22;
        } else {
            begin = band_start;
            fastleak = (self.cpl_fast_leak << 8) + 768;
            slowleak = (self.cpl_slow_leak << 8) + 768;
        }
        for band in begin..band_end {
            fastleak = (fastleak - self.fast_decay).max(bpsd[band] - fast_gain);
            slowleak = (slowleak - self.slow_decay).max(bpsd[band] - self.slow_gain);
            excite[band] = fastleak.max(slowleak);
        }
        let mask = &mut self.mask[ch];
        for band in band_start..band_end {
            let tmp = self.db_per_bit - bpsd[band];
            if tmp > 0 {
                excite[band] += tmp >> 2;
            }
            let hearing = HEARING[(band >> self.sr_shift) * 3 + self.sr_code] as i32;
            mask[band] = hearing.max(excite[band]);
        }
        let mode = self.dba_mode[ch];
        if mode == DBA_REUSE || mode == DBA_NEW {
            let mut band = band_start;
            for seg in 0..self.dba_n[ch].min(8) {
                band += self.dba_off[ch][seg] as usize;
                let len = self.dba_len[ch][seg] as usize;
                if band >= 50 || len > 50 - band {
                    return None;
                }
                let v = self.dba_val[ch][seg] as i32;
                let delta = if v >= 4 { (v - 3) * 128 } else { (v - 4) * 128 };
                for _ in 0..len {
                    mask[band] += delta;
                    band += 1;
                }
            }
        }

        // Bits per coefficient.
        let snr = self.snr[ch];
        let bap = &mut self.bap[ch];
        if snr == -960 {
            *bap = [0; 256];
            return Some(());
        }
        let psd = &self.psd[ch];
        let floor = self.floor;
        let tab: &[u8; 64] = if self.uses_aht[ch] { &HEBAP_TAB } else { &BAP_TAB };
        let mut bin = start;
        let mut band = BIN_TO_BAND[start] as usize;
        loop {
            let m = (((mask[band] - snr - floor).max(0)) & 0x1FE0) + floor;
            band += 1;
            let band_end = (BAND_START[band] as usize).min(end);
            while bin < band_end {
                let adr = ((psd[bin] - m) >> 5).clamp(0, 63);
                bap[bin] = tab[adr as usize];
                bin += 1;
            }
            if end <= band_end {
                break;
            }
        }
        Some(())
    }

    /// Coupled channels: their coefficients are the coupling channel's, scaled per band.
    fn uncouple(&mut self) {
        let mut bin = self.start[CPL];
        for band in 0..self.ncplbands {
            let (bs, be) = (bin, (bin + self.cpl_band_sizes[band]).min(256));
            for ch in 1..=self.fbw {
                if self.in_cpl[ch] {
                    let k = self.cpl_coords[ch][band] as f32 * (1.0 / 8_388_608.0);
                    for i in bs..be {
                        self.coef[ch][i] = self.coef[CPL][i] * k;
                    }
                    if ch == 2 && self.phase[band] {
                        for i in bs..be {
                            self.coef[2][i] = -self.coef[2][i];
                        }
                    }
                }
            }
            bin = be;
        }
    }
}

/// Band layout of a frequency range in 12-bin subbands (coupling, spectral extension): each
/// subband either joins the band before it or starts a new one. Returns the band count and
/// sizes. `state` keeps the layout from block to block (E-AC-3 may reuse it).
fn band_structure(b: &mut Bits, blk: usize, eac3: bool, start: usize, end: usize, default: &[u8], state: &mut [u8]) -> (usize, [u8; 22]) {
    let n_sub = end - start;
    if blk == 0 {
        state.copy_from_slice(default);
    }
    if !eac3 || b.flag() {
        for sub in 0..n_sub - 1 {
            state[start + 1 + sub] = b.flag() as u8;
        }
    }
    let mut sizes = [0u8; 22];
    let mut n = n_sub;
    sizes[0] = 12;
    let mut bnd = 0;
    for sub in 1..n_sub {
        if state[start + sub] != 0 {
            n -= 1;
            sizes[bnd] += 12;
        } else {
            bnd += 1;
            sizes[bnd] = 12;
        }
    }
    (n, sizes)
}

impl St {
    /// E-AC-3 spectral extension: which channels, and the copy and extension ranges.
    fn spx_strategy(&mut self, b: &mut Bits, blk: usize) -> Option<()> {
        if self.acmod == 1 {
            self.uses_spx[1] = true;
        } else {
            let mut v = b.read(self.fbw as u32);
            for ch in (1..=self.fbw).rev() {
                self.uses_spx[ch] = v & 1 != 0;
                v >>= 1;
            }
        }
        let dst = b.read(2) as usize;
        let mut start = b.read(3) as usize + 2;
        if start > 7 {
            start += start - 7;
        }
        let mut end = b.read(3) as usize + 5;
        if end > 7 {
            end += end - 7;
        }
        let dst_start = dst * 12 + 25;
        let src_start = start * 12 + 25;
        if start >= end || dst_start >= src_start {
            return None;
        }
        self.spx_dst_start = dst_start;
        self.spx_src_start = src_start;
        self.spx_dst_end = end * 12 + 25;
        let (n, sizes) = band_structure(b, blk, true, start, end, &DEFAULT_SPX_BAND_STRUCT, &mut self.spx_band_struct);
        self.num_spx_bands = n.min(17);
        for k in 0..17 {
            self.spx_band_sizes[k] = sizes[k] as usize;
        }
        Some(())
    }

    fn spx_coordinates(&mut self, b: &mut Bits) {
        for ch in 1..=self.fbw {
            if !self.uses_spx[ch] {
                self.first_spx_coords[ch] = true;
                continue;
            }
            if self.first_spx_coords[ch] || b.flag() {
                self.first_spx_coords[ch] = false;
                let blend = b.read(5) as f32 * (1.0 / 32.0);
                let master = b.read(2) as i32 * 3;
                let mut bin = self.spx_src_start;
                for bnd in 0..self.num_spx_bands {
                    let bs = self.spx_band_sizes[bnd];
                    let nratio = ((bin + (bs >> 1)) as f32 / self.spx_dst_end as f32 - blend).clamp(0.0, 1.0);
                    let nblend = libm::sqrtf(3.0 * nratio);
                    let sblend = libm::sqrtf(1.0 - nratio);
                    bin += bs;
                    let e = b.read(4) as i32;
                    let mut m = b.read(2) as i32;
                    if e == 15 {
                        m <<= 1;
                    } else {
                        m += 4;
                    }
                    m <<= 25 - e - master;
                    let coord = m as f32 * (1.0 / 8_388_608.0);
                    self.spx_noise_blend[ch][bnd] = nblend * coord;
                    self.spx_signal_blend[ch][bnd] = sblend * coord;
                }
            }
        }
    }

    /// Rebuild the high frequencies from copies of lower ones, scaled and mixed with noise.
    fn apply_spx(&mut self, noise: &mut Noise) {
        let (dst_start, src_start) = (self.spx_dst_start, self.spx_src_start);
        let mut wrap = [false; 17];
        wrap[0] = true;
        let mut copy = [0usize; 40];
        let mut ncopy = 0;
        let mut bin = dst_start;
        for bnd in 0..self.num_spx_bands {
            let bs = self.spx_band_sizes[bnd];
            if bin + bs > src_start {
                copy[ncopy] = bin - dst_start;
                ncopy += 1;
                bin = dst_start;
                wrap[bnd] = true;
            }
            let mut i = 0;
            while i < bs {
                if bin == src_start {
                    copy[ncopy] = bin - dst_start;
                    ncopy += 1;
                    bin = dst_start;
                }
                let n = (bs - i).min(src_start - bin);
                bin += n;
                i += n;
            }
        }
        copy[ncopy] = bin - dst_start;
        ncopy += 1;
        for ch in 1..=self.fbw {
            if !self.uses_spx[ch] {
                continue;
            }
            let c = &mut self.coef[ch];
            let mut bin = src_start;
            for &n in copy.iter().take(ncopy) {
                for k in 0..n {
                    if bin + k < 256 {
                        c[bin + k] = c[dst_start + k];
                    }
                }
                bin += n;
            }
            let mut rms = [0f32; 17];
            let mut bin = src_start;
            for bnd in 0..self.num_spx_bands {
                let bs = self.spx_band_sizes[bnd];
                let mut acc = 0.0f32;
                for _ in 0..bs {
                    let v = c[bin.min(255)];
                    acc += v * v;
                    bin += 1;
                }
                rms[bnd] = libm::sqrtf(acc / bs as f32);
            }
            if self.spx_atten[ch] >= 0 {
                let a = &SPX_ATTEN[self.spx_atten[ch] as usize * 3..];
                let mut bin = src_start - 2;
                for bnd in 0..self.num_spx_bands {
                    if wrap[bnd] && bin + 5 <= 256 {
                        c[bin] *= a[0];
                        c[bin + 1] *= a[1];
                        c[bin + 2] *= a[2];
                        c[bin + 3] *= a[1];
                        c[bin + 4] *= a[0];
                    }
                    bin += self.spx_band_sizes[bnd];
                }
            }
            let mut bin = src_start;
            for bnd in 0..self.num_spx_bands {
                let nscale = self.spx_noise_blend[ch][bnd] * rms[bnd] * (1.0 / -2_147_483_648.0f32);
                let sscale = self.spx_signal_blend[ch][bnd];
                for _ in 0..self.spx_band_sizes[bnd] {
                    let n = nscale * (noise.next() as i32) as f32;
                    if bin < 256 {
                        c[bin] *= sscale;
                        c[bin] += n;
                    }
                    bin += 1;
                }
            }
        }
    }

    /// E-AC-3 hybrid transform: one channel's values for all six blocks, read in block 0.
    fn aht_mantissas(&mut self, b: &mut Bits, ch: usize, noise: &mut Noise) {
        let gaq_mode = b.read(2);
        let end_bap = if gaq_mode < 2 { 12 } else { 17 };
        let (start, end) = (self.start[ch], self.end[ch].min(256));
        let mut gaq = [0i32; 256];
        let mut gs = 0;
        if gaq_mode == 1 || gaq_mode == 2 {
            for bin in start..end {
                let h = self.bap[ch][bin];
                if h > 7 && h < end_bap {
                    gaq[gs] = (b.read(1) as i32) << (gaq_mode - 1);
                    gs += 1;
                }
            }
        } else if gaq_mode == 3 {
            let mut gc = 2;
            for bin in start..end {
                let h = self.bap[ch][bin];
                if h > 7 && h < 17 {
                    if gc == 2 {
                        let code = (b.read(5) as i32).min(26);
                        for v in [code / 9, (code % 9) / 3, code % 3] {
                            if gs < 256 {
                                gaq[gs] = v;
                                gs += 1;
                            }
                        }
                        gc = 0;
                    } else {
                        gc += 1;
                    }
                }
            }
        }
        let vq: [&[i16]; 8] = [&[], &VQ_HEBAP1, &VQ_HEBAP2, &VQ_HEBAP3, &VQ_HEBAP4, &VQ_HEBAP5, &VQ_HEBAP6, &VQ_HEBAP7];
        let sbits = |b: &mut Bits, n: u32| -> i32 {
            if n == 0 {
                return 0;
            }
            let v = b.read(n) as i32;
            (v << (32 - n)) >> (32 - n)
        };
        gs = 0;
        for bin in start..end {
            let h = self.bap[ch][bin] as usize;
            let bits = BITS_VS_HEBAP[h] as u32;
            let pm = &mut self.pre_mantissa[ch][bin];
            if h == 0 {
                for v in pm.iter_mut() {
                    *v = (noise.next() & 0x7F_FFFF) as i32 - 0x40_0000;
                }
            } else if h < 8 {
                let v = b.read(bits) as usize;
                for (k, p) in pm.iter_mut().enumerate() {
                    *p = vq[h][v * 6 + k] as i32 * 256;
                }
            } else {
                let log_gain = if gaq_mode != 0 && h < end_bap as usize {
                    gs += 1;
                    gaq[gs - 1]
                } else {
                    0
                };
                let gbits = bits - log_gain as u32;
                for p in pm.iter_mut() {
                    let mut mant = sbits(b, gbits);
                    if log_gain != 0 && mant == -(1 << (gbits - 1)) {
                        let mbits = bits - (2 - log_gain as u32);
                        mant = sbits(b, mbits);
                        mant = ((mant as u32) << (23 - (mbits - 1))) as i32;
                        let bb = if mant >= 0 { 1 << (23 - log_gain) } else { GAQ_REMAP_2_4_B[(h - 8) * 2 + log_gain as usize - 1] as i32 * 256 };
                        mant += ((GAQ_REMAP_2_4_A[(h - 8) * 2 + log_gain as usize - 1] as i64 * mant as i64) >> 15) as i32 + bb;
                    } else {
                        mant *= 1 << (24 - bits);
                        if log_gain == 0 {
                            mant += ((GAQ_REMAP_1[h - 8] as i64 * mant as i64) >> 15) as i32;
                        }
                    }
                    *p = mant;
                }
            }
            idct6(pm);
        }
    }
}

/// The hybrid transform's six-point inverse DCT (fixed point, as FFmpeg).
fn idct6(m: &mut [i32; 6]) {
    const C0: i64 = 10273905;
    const C1: i64 = 11863283;
    const C2: i64 = 3070444;
    let odd1 = m[1] - m[3] - m[5];
    let mut even2 = ((m[2] as i64 * C0) >> 23) as i32;
    let tmp = ((m[4] as i64 * C1) >> 23) as i32;
    let mut odd0 = (((m[1] + m[5]) as i64 * C2) >> 23) as i32;
    let mut even0 = m[0] + (tmp >> 1);
    let even1 = m[0] - tmp;
    let t = even0;
    even0 = t + even2;
    even2 = t - even2;
    let t = odd0;
    odd0 = t + m[1] + m[3];
    let odd2 = t + m[5] - m[3];
    m[0] = even0 + odd0;
    m[1] = even1 + odd1;
    m[2] = even2 + odd2;
    m[3] = even2 - odd2;
    m[4] = even1 - odd1;
    m[5] = even0 - odd0;
}

fn lowcomp1(a: i32, b0: i32, b1: i32, c: i32) -> i32 {
    if b0 + 256 == b1 {
        c
    } else if b0 > b1 {
        (a - 64).max(0)
    } else {
        a
    }
}

/// Frame length in bytes from the header, if it looks like an AC-3 frame.
fn frame_len(h: &[u8]) -> Option<usize> {
    if h.len() < 6 || h[0] != 0x0B || h[1] != 0x77 {
        return None;
    }
    let bsid = h[5] >> 3;
    if bsid > 16 {
        return None;
    }
    if bsid > 10 {
        // E-AC-3: the size in 16-bit words follows the stream type and substream.
        let words = (((h[2] & 7) as usize) << 8 | h[3] as usize) + 1;
        return if words * 2 >= 7 { Some(words * 2) } else { None };
    }
    let fscod = (h[4] >> 6) as usize;
    let code = (h[4] & 63) as usize;
    if fscod == 3 || code >= 38 {
        return None;
    }
    Some(FRAME_SIZE[code * 3 + fscod] as usize * 2)
}

/// Blocks of 256 samples in a frame (E-AC-3 frames may be shorter than AC-3's six).
fn frame_blocks(h: &[u8]) -> usize {
    if h[5] >> 3 > 10 && h[4] >> 6 != 3 {
        [1, 2, 3, 6][((h[4] >> 4) & 3) as usize]
    } else {
        6
    }
}

impl AudioDec for Ac3Dec {
    fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        self.buf.extend_from_slice(data);
        let mut out = Vec::new();
        let mut p = 0;
        while p + 6 <= self.buf.len() {
            let Some(len) = frame_len(&self.buf[p..]) else {
                p += 1;
                continue;
            };
            if p + len > self.buf.len() {
                break;
            }
            let f = self.buf[p..p + len].to_vec();
            let before = out.len();
            if self.frame(&f, &mut out).is_none() {
                // A damaged frame: silence rather than noise.
                out.truncate(before);
                out.resize(before + frame_blocks(&f) * 256 * 2, 0);
            }
            p += len;
        }
        self.buf.drain(..p);
        if self.buf.len() > 8192 {
            self.buf.clear();
        }
        out
    }
    fn rate(&self) -> u32 {
        self.rate
    }
    fn reset(&mut self) {
        self.buf.clear();
        self.delay = [[0.0; 128]; 5];
        self.st = St::new();
    }
}

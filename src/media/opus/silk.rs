//! SILK, the speech half of Opus (RFC 6716 section 4.2), ported from FFmpeg's opus/silk.c
//! (LGPL 2.1 or later; Andrew D'Addesio, Mozilla).

use alloc::vec;
use alloc::vec::Vec;
use super::rc::{ilog, Rc};
use super::tab::*;

const HISTORY: usize = 322;
const MAX_LAG: usize = 288 + 2;
const LTP_ORDER: usize = 5;

#[derive(Clone)]
struct Frame {
    coded: bool,
    log_gain: i32,
    nlsf: [i16; 16],
    lpc: [f32; 16],
    output: Vec<f32>,
    lpc_history: Vec<f32>,
    primarylag: i32,
    prev_voiced: bool,
}

impl Frame {
    fn new() -> Frame {
        Frame { coded: false, log_gain: 0, nlsf: [0; 16], lpc: [0.0; 16], output: vec![0.0; 2 * HISTORY], lpc_history: vec![0.0; 2 * HISTORY], primarylag: 0, prev_voiced: false }
    }
    fn flush(&mut self) {
        if !self.coded {
            return;
        }
        *self = Frame::new();
    }
}

pub struct Silk {
    output_channels: usize,
    midonly: bool,
    subframes: usize,
    sflength: usize,
    flength: usize,
    nlsf_interp_factor: i32,
    bandwidth: usize,
    wb: bool,
    frame: [Frame; 2],
    prev_stereo_weights: [f32; 2],
    stereo_weights: [f32; 2],
    prev_coded_channels: usize,
}

#[inline]
fn mulh(a: i32, b: i32) -> i32 {
    ((a as i64 * b as i64) >> 32) as i32
}
#[inline]
fn mull(a: i32, b: i32, s: u32) -> i64 {
    (a as i64 * b as i64) >> s
}
#[inline]
fn round_mull(a: i64, b: i64, s: u32) -> i64 {
    (((a * b) >> (s - 1)) + 1) >> 1
}

fn stabilize_lsf(nlsf: &mut [i16; 16], order: usize, min_delta: &[u16]) {
    for _pass in 0..20 {
        let mut k = 0;
        let mut min_diff = 0;
        for i in 0..order + 1 {
            let low = if i != 0 { nlsf[i - 1] as i32 } else { 0 };
            let high = if i != order { nlsf[i] as i32 } else { 32768 };
            let diff = (high - low) - min_delta[i] as i32;
            if diff < min_diff {
                min_diff = diff;
                k = i;
            }
        }
        if min_diff == 0 {
            return;
        }
        if k == 0 {
            nlsf[0] = min_delta[0] as i16;
        } else if k == order {
            nlsf[order - 1] = (32768 - min_delta[order] as i32) as i16;
        } else {
            let mut min_center = 0i32;
            let mut max_center = 32768i32;
            for d in &min_delta[..k] {
                min_center += *d as i32;
            }
            min_center += min_delta[k] as i32 >> 1;
            for i in (k + 1..=order).rev() {
                max_center -= min_delta[i] as i32;
            }
            max_center -= min_delta[k] as i32 >> 1;
            let mut c = nlsf[k - 1] as i32 + nlsf[k] as i32;
            c = (c >> 1) + (c & 1);
            c = c.max(min_center).min(max_center);
            nlsf[k - 1] = (c - (min_delta[k] as i32 >> 1)) as i16;
            nlsf[k] = (nlsf[k - 1] as i32 + min_delta[k] as i32) as i16;
        }
    }
    // Fall back: sort, then push apart.
    for i in 1..order {
        let v = nlsf[i];
        let mut j = i as isize - 1;
        while j >= 0 && nlsf[j as usize] > v {
            nlsf[j as usize + 1] = nlsf[j as usize];
            j -= 1;
        }
        nlsf[(j + 1) as usize] = v;
    }
    if (nlsf[0] as i32) < min_delta[0] as i32 {
        nlsf[0] = min_delta[0] as i16;
    }
    for i in 1..order {
        nlsf[i] = (nlsf[i] as i32).max((nlsf[i - 1] as i32 + min_delta[i] as i32).min(32767)) as i16;
    }
    if nlsf[order - 1] as i32 > 32768 - min_delta[order] as i32 {
        nlsf[order - 1] = (32768 - min_delta[order] as i32) as i16;
    }
    for i in (0..order - 1).rev() {
        if nlsf[i] as i32 > nlsf[i + 1] as i32 - min_delta[i + 1] as i32 {
            nlsf[i] = (nlsf[i + 1] as i32 - min_delta[i + 1] as i32) as i16;
        }
    }
}

fn is_lpc_stable(lpc: &[i16; 16], order: usize) -> bool {
    let mut rows = [[0i32; 16]; 2];
    let mut dc = 0i32;
    let mut totalinvgain: i32 = 1 << 30;
    for k in 0..order {
        dc += lpc[k] as i32;
        rows[0][k] = lpc[k] as i32 * 4096;
    }
    if dc >= 4096 {
        return false;
    }
    let mut cur = 0usize;
    let mut k = order - 1;
    loop {
        let row = rows[cur];
        if row[k].abs() > 16773022 {
            return false;
        }
        let rc = -(row[k] * 128);
        let gaindiv = (1 << 30) - mulh(rc, rc);
        totalinvgain = mulh(totalinvgain, gaindiv) << 2;
        if k == 0 {
            return totalinvgain >= 107374;
        }
        let fbits = ilog(gaindiv as u32) as i32;
        let mut gain = ((1 << 29) - 1) / (gaindiv >> (fbits + 1 - 16));
        let error = (1i32 << 29).wrapping_sub(mull(gaindiv << (15 + 16 - fbits), gain, 16) as i32);
        gain = (gain << 16).wrapping_add(error.wrapping_mul(gain) >> 13);
        let next = k & 1;
        let prev = row;
        for j in 0..k {
            let x = (prev[j] as i64 - round_mull(prev[k - j - 1] as i64, rc as i64, 31) as i32 as i64).clamp(i32::MIN as i64, i32::MAX as i64) as i32;
            let tmp = round_mull(x as i64, gain as i64, fbits as u32);
            if tmp < i32::MIN as i64 || tmp > i32::MAX as i64 {
                return false;
            }
            rows[next][j] = tmp as i32;
        }
        cur = next;
        k -= 1;
    }
}

fn lsp2poly(lsp: &[i32], pol: &mut [i32; 9], half: usize) {
    pol[0] = 65536;
    pol[1] = -lsp[0];
    for i in 1..half {
        pol[i + 1] = (pol[i - 1] as i64 * 2 - round_mull(lsp[2 * i] as i64, pol[i] as i64, 16)) as i32;
        for j in (2..=i).rev() {
            pol[j] = (pol[j] as i64 + pol[j - 2] as i64 - round_mull(lsp[2 * i] as i64, pol[j - 1] as i64, 16)) as i32;
        }
        pol[1] -= lsp[2 * i];
    }
}

fn lsf2lpc(nlsf: &[i16; 16], lpcf: &mut [f32; 16], order: usize) {
    let mut lsp = [0i32; 16];
    let mut p = [0i32; 9];
    let mut q = [0i32; 9];
    let mut lpc32 = [0i32; 16];
    let mut lpc = [0i16; 16];
    for k in 0..order {
        let index = (nlsf[k] >> 8) as usize;
        let offset = (nlsf[k] & 255) as i32;
        let k2 = if order == 10 { SILK_LSF_ORDERING_NBMB[k] } else { SILK_LSF_ORDERING_WB[k] } as usize;
        let mut v = SILK_COSINE[index] as i32 * 256;
        v += (SILK_COSINE[index + 1] as i32 - SILK_COSINE[index] as i32) * offset;
        lsp[k2] = (v + 4) >> 3;
    }
    lsp2poly(&lsp, &mut p, order >> 1);
    lsp2poly(&lsp[1..], &mut q, order >> 1);
    for k in 0..order >> 1 {
        let pt = p[k + 1] + p[k];
        let qt = q[k + 1] - q[k];
        lpc32[k] = -qt - pt;
        lpc32[order - k - 1] = qt - pt;
    }
    let mut i = 0;
    while i < 10 {
        let mut maxabs = 0u32;
        let mut k = 0usize;
        for (j, v) in lpc32.iter().enumerate().take(order) {
            let x = v.unsigned_abs();
            if x > maxabs {
                maxabs = x;
                k = j;
            }
        }
        maxabs = (maxabs + 16) >> 5;
        if maxabs > 32767 {
            maxabs = maxabs.min(163838);
            let chirp_base = 65470 - ((maxabs - 32767) << 14) / ((maxabs * (k as u32 + 1)) >> 2);
            let mut chirp = chirp_base;
            for v in lpc32.iter_mut().take(order) {
                *v = round_mull(*v as i64, chirp as i64, 16) as i32;
                chirp = (chirp_base * chirp + 32768) >> 16;
            }
        } else {
            break;
        }
        i += 1;
    }
    if i == 10 {
        for k in 0..order {
            let x = (lpc32[k] + 16) >> 5;
            lpc[k] = x.clamp(-32768, 32767) as i16;
            lpc32[k] = (lpc[k] as i32) << 5;
        }
    } else {
        for k in 0..order {
            lpc[k] = ((lpc32[k] + 16) >> 5) as i16;
        }
    }
    let mut i = 1;
    while i <= 16 && !is_lpc_stable(&lpc, order) {
        let chirp_base = 65536u32 - (1 << i);
        let mut chirp = chirp_base;
        for k in 0..order {
            lpc32[k] = round_mull(lpc32[k] as i64, chirp as i64, 16) as i32;
            lpc[k] = ((lpc32[k] + 16) >> 5) as i16;
            chirp = ((chirp_base as u64 * chirp as u64 + 32768) >> 16) as u32;
        }
        i += 1;
    }
    for i in 0..order {
        lpcf[i] = lpc[i] as f32 / 4096.0;
    }
}

impl Silk {
    pub fn new(output_channels: usize) -> Silk {
        Silk {
            output_channels,
            midonly: false,
            subframes: 4,
            sflength: 80,
            flength: 320,
            nlsf_interp_factor: 4,
            bandwidth: 2,
            wb: true,
            frame: [Frame::new(), Frame::new()],
            prev_stereo_weights: [0.0; 2],
            stereo_weights: [0.0; 2],
            prev_coded_channels: 0,
        }
    }

    pub fn flush(&mut self) {
        self.frame[0].flush();
        self.frame[1].flush();
        self.prev_stereo_weights = [0.0; 2];
    }

    fn decode_lpc(&mut self, ch: usize, rc: &mut Rc, leadin: &mut [f32; 16], lpc: &mut [f32; 16], voiced: bool) -> (usize, bool) {
        let wb = self.wb;
        let order = if wb { 16 } else { 10 };
        let i1 = rc.cdf(&SILK_MODEL_LSF_S1[((wb as usize) * 2 + voiced as usize) * 33..]);
        let mut i2 = [0i32; 16];
        for i in 0..order {
            let index = if wb { SILK_LSF_S2_MODEL_SEL_WB[i1 * 16 + i] } else { SILK_LSF_S2_MODEL_SEL_NBMB[i1 * 10 + i] } as usize;
            i2[i] = rc.cdf(&SILK_MODEL_LSF_S2[index * 10..]) as i32 - 4;
            if i2[i] == -4 {
                i2[i] -= rc.cdf(&SILK_MODEL_LSF_S2_EXT) as i32;
            } else if i2[i] == 4 {
                i2[i] += rc.cdf(&SILK_MODEL_LSF_S2_EXT) as i32;
            }
        }
        let mut res = [0i16; 16];
        for i in (0..order).rev() {
            let qstep = if wb { 9830 } else { 11796 };
            let mut r = (i2[i] * 1024) as i16 as i32;
            if i2[i] < 0 {
                r += 102;
            } else if i2[i] > 0 {
                r -= 102;
            }
            r = (r as i16 as i32 * qstep) >> 16;
            if i + 1 < order {
                let weight = if wb {
                    SILK_LSF_PRED_WEIGHTS_WB[SILK_LSF_WEIGHT_SEL_WB[i1 * 15 + i] as usize * 15 + i]
                } else {
                    SILK_LSF_PRED_WEIGHTS_NBMB[SILK_LSF_WEIGHT_SEL_NBMB[i1 * 9 + i] as usize * 9 + i]
                } as i32;
                r = r as i16 as i32 + ((res[i + 1] as i32 * weight) >> 8);
            }
            res[i] = r as i16;
        }
        let mut nlsf = [0i16; 16];
        for i in 0..order {
            let (cur, weight) = if wb {
                (SILK_LSF_CODEBOOK_WB[i1 * 16 + i] as i32, SILK_MODEL_LSF_WEIGHT_WB[i1 * 16 + i] as i32)
            } else {
                (SILK_LSF_CODEBOOK_NBMB[i1 * 10 + i] as i32, SILK_MODEL_LSF_WEIGHT_NBMB[i1 * 10 + i] as i32)
            };
            let value = cur * 128 + (res[i] as i32 * 16384) / weight;
            nlsf[i] = value.clamp(0, 32767) as i16;
        }
        stabilize_lsf(&mut nlsf, order, if wb { &SILK_LSF_MIN_SPACING_WB } else { &SILK_LSF_MIN_SPACING_NBMB });
        let mut has_leadin = false;
        let frame = &mut self.frame[ch];
        if self.subframes == 4 {
            let mut offset = rc.cdf(&SILK_MODEL_LSF_INTERPOLATION_OFFSET) as i32;
            if offset != 4 && frame.coded {
                has_leadin = true;
                if offset != 0 {
                    let mut nl = [0i16; 16];
                    for i in 0..order {
                        nl[i] = (frame.nlsf[i] as i32 + ((nlsf[i] as i32 - frame.nlsf[i] as i32) * offset >> 2)) as i16;
                    }
                    lsf2lpc(&nl, leadin, order);
                } else {
                    *leadin = frame.lpc;
                }
            } else {
                offset = 4;
            }
            self.nlsf_interp_factor = offset;
        } else {
            self.nlsf_interp_factor = 4;
        }
        lsf2lpc(&nlsf, lpc, order);
        frame.nlsf[..order].copy_from_slice(&nlsf[..order]);
        frame.lpc[..order].copy_from_slice(&lpc[..order]);
        (order, has_leadin)
    }

    fn count_children(rc: &mut Rc, model: usize, total: i32) -> [i32; 2] {
        if total != 0 {
            let off = (((total - 1 + 5) * (total - 1)) >> 1) as usize;
            let c0 = rc.cdf(&SILK_MODEL_PULSE_LOCATION[model * 168 + off..]) as i32;
            [c0, total - c0]
        } else {
            [0, 0]
        }
    }

    fn excitation(&self, rc: &mut Rc, out: &mut [f32], qoffset_high: usize, active: bool, voiced: bool) {
        let mut seed = rc.cdf(&SILK_MODEL_LCG_SEED) as u32;
        let shellblocks = SILK_SHELL_BLOCKS[self.bandwidth * 2 + (self.subframes >> 2)] as usize;
        let ratelevel = rc.cdf(&SILK_MODEL_EXC_RATE[voiced as usize * 10..]);
        let mut pulsecount = [0usize; 20];
        let mut lsbcount = [0u32; 20];
        let mut exc = [0i32; 320];
        for i in 0..shellblocks {
            pulsecount[i] = rc.cdf(&SILK_MODEL_PULSE_COUNT[ratelevel * 19..]);
            if pulsecount[i] == 17 {
                while pulsecount[i] == 17 {
                    lsbcount[i] += 1;
                    if lsbcount[i] == 10 {
                        break;
                    }
                    pulsecount[i] = rc.cdf(&SILK_MODEL_PULSE_COUNT[9 * 19..]);
                }
                if lsbcount[i] == 10 {
                    pulsecount[i] = rc.cdf(&SILK_MODEL_PULSE_COUNT[10 * 19..]);
                }
            }
        }
        for i in 0..shellblocks {
            if pulsecount[i] != 0 {
                let mut loc = 16 * i;
                let b1 = Self::count_children(rc, 0, pulsecount[i] as i32);
                for &t1 in &b1 {
                    let b2 = Self::count_children(rc, 1, t1);
                    for &t2 in &b2 {
                        let b3 = Self::count_children(rc, 2, t2);
                        for &t3 in &b3 {
                            let l = Self::count_children(rc, 3, t3);
                            exc[loc] = l[0];
                            exc[loc + 1] = l[1];
                            loc += 2;
                        }
                    }
                }
            }
        }
        for i in 0..shellblocks << 4 {
            for _ in 0..lsbcount[i >> 4] {
                exc[i] = (exc[i] << 1) | rc.cdf(&SILK_MODEL_EXCITATION_LSB) as i32;
            }
        }
        let av = active as usize + voiced as usize;
        for i in 0..shellblocks << 4 {
            if exc[i] != 0 {
                let pc = pulsecount[i >> 4].min(6);
                let sign = rc.cdf(&SILK_MODEL_EXCITATION_SIGN[((av * 2 + qoffset_high) * 7 + pc) * 3..]);
                if sign == 0 {
                    exc[i] = -exc[i];
                }
            }
        }
        let qo = SILK_QUANT_OFFSET[voiced as usize * 2 + qoffset_high] as i32;
        for i in 0..shellblocks << 4 {
            let value = exc[i];
            let mut e = (value * 256) | qo;
            if value < 0 {
                e += 20;
            } else if value > 0 {
                e -= 20;
            }
            seed = seed.wrapping_mul(196314165).wrapping_add(907633515);
            if seed & 0x8000_0000 != 0 {
                e = -e;
            }
            seed = seed.wrapping_add(value as u32);
            out[i] = e as f32 / 8388608.0;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn decode_frame(&mut self, rc: &mut Rc, frame_num: usize, ch: usize, coded_channels: usize, active: bool, active1: bool, redundant: bool) {
        let mut residual = [0f32; MAX_LAG + HISTORY];
        let mut lpc_leadin = [0f32; 16];
        let mut lpc_body = [0f32; 16];
        let mut gains = [0f32; 4];
        let mut lags = [0i32; 4];
        let mut taps = [[0f32; 5]; 4];

        if coded_channels == 2 && ch == 0 {
            let n = rc.cdf(&SILK_MODEL_STEREO_S1);
            let mut wi = [0usize; 2];
            let mut ws = [0i32; 2];
            wi[0] = rc.cdf(&SILK_MODEL_STEREO_S2) + 3 * (n / 5);
            ws[0] = rc.cdf(&SILK_MODEL_STEREO_S3) as i32;
            wi[1] = rc.cdf(&SILK_MODEL_STEREO_S2) + 3 * (n % 5);
            ws[1] = rc.cdf(&SILK_MODEL_STEREO_S3) as i32;
            let mut w = [0i32; 2];
            for i in 0..2 {
                let a = SILK_STEREO_WEIGHTS[wi[i]] as i32;
                let b = SILK_STEREO_WEIGHTS[wi[i] + 1] as i32;
                w[i] = a + (((b - a) * 6554) >> 16) * (ws[i] * 2 + 1);
            }
            self.stereo_weights[0] = ((w[0] - w[1]) as f64 / 8192.0) as f32;
            self.stereo_weights[1] = (w[1] as f64 / 8192.0) as f32;
            self.midonly = if active1 { false } else { rc.cdf(&SILK_MODEL_MID_ONLY) != 0 };
        }
        let (qoffset_high, voiced) = if !active {
            (rc.cdf(&SILK_MODEL_FRAME_TYPE_INACTIVE), false)
        } else {
            let t = rc.cdf(&SILK_MODEL_FRAME_TYPE_ACTIVE);
            (t & 1, t >> 1 != 0)
        };
        for i in 0..self.subframes {
            let frame = &mut self.frame[ch];
            let mut log_gain;
            if i == 0 && (frame_num == 0 || !frame.coded) {
                let x = rc.cdf(&SILK_MODEL_GAIN_HIGHBITS[(active as usize + voiced as usize) * 9..]) as i32;
                log_gain = (x << 3) | rc.cdf(&SILK_MODEL_GAIN_LOWBITS) as i32;
                if frame.coded {
                    log_gain = log_gain.max(frame.log_gain - 16);
                }
            } else {
                let d = rc.cdf(&SILK_MODEL_GAIN_DELTA) as i32;
                log_gain = ((d << 1) - 16).max(frame.log_gain + d - 4).clamp(0, 63);
            }
            frame.log_gain = log_gain;
            let lg = (log_gain * 0x1D1C71 >> 16) + 2090;
            let ipart = lg >> 7;
            let fpart = lg & 127;
            let lingain = (1 << ipart) + ((-174 * fpart * (128 - fpart) >> 16) + fpart) * ((1 << ipart) >> 7);
            gains[i] = lingain as f32 / 65536.0;
        }
        let (order, has_leadin) = self.decode_lpc(ch, rc, &mut lpc_leadin, &mut lpc_body, voiced);
        let bw = self.bandwidth;
        if voiced {
            let frame = &mut self.frame[ch];
            let mut lag_absolute = frame_num == 0 || !frame.prev_voiced;
            let mut primarylag = 0;
            if !lag_absolute {
                let delta = rc.cdf(&SILK_MODEL_PITCH_DELTA) as i32;
                if delta != 0 {
                    primarylag = frame.primarylag + delta - 9;
                } else {
                    lag_absolute = true;
                }
            }
            if lag_absolute {
                let high = rc.cdf(&SILK_MODEL_PITCH_HIGHBITS) as i32;
                let low = match bw {
                    0 => rc.cdf(&SILK_MODEL_LCG_SEED),
                    1 => rc.cdf(&SILK_MODEL_PITCH_LOWBITS_MB),
                    _ => rc.cdf(&SILK_MODEL_GAIN_LOWBITS),
                } as i32;
                primarylag = SILK_PITCH_MIN_LAG[bw] as i32 + high * SILK_PITCH_SCALE[bw] as i32 + low;
            }
            frame.primarylag = primarylag;
            let offsets: &[i8] = if self.subframes == 2 {
                if bw == 0 {
                    &SILK_PITCH_OFFSET_NB10MS[rc.cdf(&SILK_MODEL_PITCH_CONTOUR_NB10MS) * 2..]
                } else {
                    &SILK_PITCH_OFFSET_MBWB10MS[rc.cdf(&SILK_MODEL_PITCH_CONTOUR_MBWB10MS) * 2..]
                }
            } else if bw == 0 {
                &SILK_PITCH_OFFSET_NB20MS[rc.cdf(&SILK_MODEL_PITCH_CONTOUR_NB20MS) * 4..]
            } else {
                &SILK_PITCH_OFFSET_MBWB20MS[rc.cdf(&SILK_MODEL_PITCH_CONTOUR_MBWB20MS) * 4..]
            };
            for i in 0..self.subframes {
                lags[i] = (primarylag + offsets[i] as i32).clamp(SILK_PITCH_MIN_LAG[bw] as i32, SILK_PITCH_MAX_LAG[bw] as i32);
            }
            let ltpfilter = rc.cdf(&SILK_MODEL_LTP_FILTER);
            for t in taps.iter_mut().take(self.subframes) {
                let (sel, tab): (&[u16], &[i8]) = match ltpfilter {
                    0 => (&SILK_MODEL_LTP_FILTER0_SEL, &SILK_LTP_FILTER0_TAPS),
                    1 => (&SILK_MODEL_LTP_FILTER1_SEL, &SILK_LTP_FILTER1_TAPS),
                    _ => (&SILK_MODEL_LTP_FILTER2_SEL, &SILK_LTP_FILTER2_TAPS),
                };
                let index = rc.cdf(sel);
                for j in 0..5 {
                    t[j] = tab[index * 5 + j] as f32 / 128.0;
                }
            }
        }
        let ltpscale = if voiced && frame_num == 0 {
            SILK_LTP_SCALE_FACTOR[rc.cdf(&SILK_MODEL_LTP_SCALE_INDEX)] as f32 / 16384.0
        } else {
            15565.0 / 16384.0
        };
        self.excitation(rc, &mut residual[MAX_LAG..], qoffset_high, active, voiced);
        if self.output_channels == ch || redundant {
            return;
        }
        let (sfl, interp) = (self.sflength, self.nlsf_interp_factor);
        let frame = &mut self.frame[ch];
        for i in 0..self.subframes {
            let coef = if i < 2 && has_leadin { &lpc_leadin } else { &lpc_body };
            let dst = HISTORY + i * sfl;
            let res = MAX_LAG + i * sfl;
            if voiced {
                let (out_end, scale) = if i < 2 || interp == 4 { (-((i * sfl) as isize), ltpscale) } else { (-(((i - 2) * sfl) as isize), 1.0) };
                let lag = lags[i] as isize;
                let mut j = -lag - (LTP_ORDER / 2) as isize;
                while j < out_end {
                    let at = (dst as isize + j) as usize;
                    let mut sum = frame.output[at];
                    for k in 0..order {
                        sum -= coef[k] * frame.output[at - k - 1];
                    }
                    residual[(res as isize + j) as usize] = sum.clamp(-1.0, 1.0) * scale / gains[i];
                    j += 1;
                }
                if out_end != 0 {
                    let rescale = gains[i - 1] / gains[i];
                    for j in out_end..0 {
                        residual[(res as isize + j) as usize] *= rescale;
                    }
                }
                for j in 0..sfl {
                    let mut sum = residual[res + j];
                    for k in 0..LTP_ORDER {
                        sum += taps[i][k] * residual[(res as isize + j as isize - lag + (LTP_ORDER / 2) as isize - k as isize) as usize];
                    }
                    residual[res + j] = sum;
                }
            }
            for j in 0..sfl {
                let mut sum = residual[res + j] * gains[i];
                for k in 1..=order {
                    sum += coef[k - 1] * frame.lpc_history[dst + j - k];
                }
                frame.lpc_history[dst + j] = sum;
                frame.output[dst + j] = sum.clamp(-1.0, 1.0);
            }
        }
        frame.prev_voiced = voiced;
        let fl = self.flength;
        frame.lpc_history.copy_within(fl..fl + HISTORY, 0);
        frame.output.copy_within(fl..fl + HISTORY, 0);
        frame.coded = true;
    }

    fn unmix_ms(&mut self, l: &mut [f32], r: &mut [f32]) {
        let fl = self.flength;
        let base = HISTORY - fl;
        let mid = &self.frame[0].output;
        let side = &self.frame[1].output;
        let (w0p, w1p) = (self.prev_stereo_weights[0], self.prev_stereo_weights[1]);
        let (w0, w1) = (self.stereo_weights[0], self.stereo_weights[1]);
        let n1 = SILK_STEREO_INTERP_LEN[self.bandwidth] as usize;
        for i in 0..fl {
            let (i0, i1) = if i < n1 { (w0p + i as f32 * (w0 - w0p) / n1 as f32, w1p + i as f32 * (w1 - w1p) / n1 as f32) } else { (w0, w1) };
            let m = |o: isize| mid[(base as isize + i as isize + o) as usize];
            let s1 = side[base + i - 1];
            let p0 = 0.25 * (m(-2) + 2.0 * m(-1) + m(0));
            l[i] = ((1.0 + i1) * m(-1) + s1 + i0 * p0).clamp(-1.0, 1.0);
            r[i] = ((1.0 - i1) * m(-1) - s1 - i0 * p0).clamp(-1.0, 1.0);
        }
        self.prev_stereo_weights = self.stereo_weights;
    }

    /// One SILK superframe (10 to 60 ms) at 8, 12 or 16 kHz into `out` (one per output
    /// channel); returns the number of samples.
    pub fn decode(&mut self, rc: &mut Rc, out: &mut [Vec<f32>], bandwidth: usize, coded_channels: usize, duration_ms: usize) -> usize {
        let mut active = [[false; 6]; 2];
        let mut redundancy = [0u32; 2];
        let nb_frames = 1 + (duration_ms > 20) as usize + (duration_ms > 40) as usize;
        self.subframes = duration_ms / nb_frames / 5;
        self.sflength = 20 * (bandwidth + 2);
        self.flength = self.sflength * self.subframes;
        self.bandwidth = bandwidth;
        self.wb = bandwidth == 2;
        if coded_channels > self.prev_coded_channels {
            self.frame[1].flush();
        }
        self.prev_coded_channels = coded_channels;
        for i in 0..coded_channels {
            for j in 0..nb_frames {
                active[i][j] = rc.log(1) != 0;
            }
            redundancy[i] = rc.log(1);
        }
        for r in redundancy.iter_mut().take(coded_channels) {
            if *r != 0 && duration_ms > 20 {
                *r = rc.cdf(if duration_ms == 40 { &SILK_MODEL_LBRR_FLAGS_40 } else { &SILK_MODEL_LBRR_FLAGS_60 }) as u32;
            }
        }
        for i in 0..nb_frames {
            for j in 0..coded_channels {
                if redundancy[j] & (1 << i) != 0 {
                    let active1 = !(j == 0 && redundancy[1] & (1 << i) == 0);
                    self.decode_frame(rc, i, j, coded_channels, true, active1, true);
                }
            }
            self.midonly = false;
        }
        let fl = self.flength;
        for i in 0..nb_frames {
            let mut j = 0;
            while j < coded_channels && !self.midonly {
                let active1 = if coded_channels > 1 { active[1][i] } else { false };
                self.decode_frame(rc, i, j, coded_channels, active[j][i], active1, false);
                j += 1;
            }
            if self.midonly && self.frame[1].coded {
                self.frame[1].flush();
            }
            if coded_channels == 1 || self.output_channels == 1 {
                for o in out.iter_mut().take(self.output_channels) {
                    o[i * fl..(i + 1) * fl].copy_from_slice(&self.frame[0].output[HISTORY - fl - 2..HISTORY - 2]);
                }
            } else {
                let (a, b) = out.split_at_mut(1);
                self.unmix_ms(&mut a[0][i * fl..(i + 1) * fl], &mut b[0][i * fl..(i + 1) * fl]);
            }
            self.midonly = false;
        }
        nb_frames * fl
    }
}

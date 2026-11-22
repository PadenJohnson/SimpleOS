//! AMR narrowband (8 kHz speech, 4.75 to 12.2 kbit/s), after FFmpeg's amrnbdec.c.

use super::nbtab::*;
use super::*;

const SUB: usize = 40;
const ORDER: usize = 10;
const DELAY_MAX: usize = 143;
/// Where the current subframe starts in the excitation buffer.
const EXC: usize = DELAY_MAX + ORDER + 1;

const MODE_4K75: usize = 0;
const MODE_5K15: usize = 1;
const MODE_5K9: usize = 2;
const MODE_6K7: usize = 3;
const MODE_7K4: usize = 4;
const MODE_7K95: usize = 5;
const MODE_10K2: usize = 6;
const MODE_12K2: usize = 7;
const MODE_DTX: usize = 8;

const LSF_R_FAC: f64 = 8000.0 / 32768.0;
const MIN_LSF_SPACING: f64 = 50.0488 / 8000.0;
const PRED_FAC_12K2: f64 = 0.65;
const SHARP_MAX: f64 = 0.79449462890625;
const SAMPLE_SCALE: f64 = 2.0 / 32768.0;

/// Field numbers in the unpacked frame.
const fn sf(k: usize, f: usize) -> usize {
    5 + 13 * k + f
}

pub struct Nb {
    frame: [u16; 57],
    mode: usize,
    prev_lsf_r: [i16; ORDER],
    lsp: [[f64; ORDER]; 4],
    prev_lsp_sub4: [f64; ORDER],
    lsf_q: [[f32; ORDER]; 4],
    lsf_avg: [f32; ORDER],
    lpc: [[f32; ORDER]; 4],
    pitch_lag_int: u8,
    exc: [f32; EXC + SUB],
    pitch_vector: [f32; SUB],
    fixed_vector: [f32; SUB],
    prediction_error: [f32; 4],
    pitch_gain: [f32; 5],
    fixed_gain: [f32; 5],
    beta: f32,
    diff_count: u8,
    hang_count: u8,
    prev_sparse_fixed_gain: f32,
    prev_ir_filter_nr: u8,
    ir_filter_onset: u8,
    postfilter_mem: [f32; ORDER],
    tilt_mem: f32,
    postfilter_agc: f32,
    high_pass_mem: [f32; 2],
    samples_in: [f32; ORDER + SUB],
}

impl Nb {
    pub fn new() -> Nb {
        let mut n = Nb {
            frame: [0; 57],
            mode: 0,
            prev_lsf_r: [0; ORDER],
            lsp: [[0.0; ORDER]; 4],
            prev_lsp_sub4: [0.0; ORDER],
            lsf_q: [[0.0; ORDER]; 4],
            lsf_avg: [0.0; ORDER],
            lpc: [[0.0; ORDER]; 4],
            pitch_lag_int: 0,
            exc: [0.0; EXC + SUB],
            pitch_vector: [0.0; SUB],
            fixed_vector: [0.0; SUB],
            prediction_error: [-14.0; 4],
            pitch_gain: [0.0; 5],
            fixed_gain: [0.0; 5],
            beta: 0.0,
            diff_count: 0,
            hang_count: 0,
            prev_sparse_fixed_gain: 0.0,
            prev_ir_filter_nr: 0,
            ir_filter_onset: 0,
            postfilter_mem: [0.0; ORDER],
            tilt_mem: 0.0,
            postfilter_agc: 0.0,
            high_pass_mem: [0.0; 2],
            samples_in: [0.0; ORDER + SUB],
        };
        for i in 0..ORDER {
            n.prev_lsp_sub4[i] = ((LSP_SUB4_INIT[i] as i32 * 1000) as f32 / 32768.0f32) as f64;
            let v = LSP_AVG_INIT[i] as f32 / 32768.0;
            n.lsf_avg[i] = v;
            n.lsf_q[3][i] = v;
        }
        n
    }

    /// One frame (header byte and payload) into 160 samples; returns the bytes used (0 = not
    /// enough data).
    pub fn frame(&mut self, buf: &[u8], out: &mut [f32; 160]) -> usize {
        let mode = (buf[0] >> 3 & 15) as usize;
        let size = if mode < 9 { FRAME_SIZES[mode] as usize + 1 } else { 1 };
        if buf.len() < size {
            return 0;
        }
        if mode >= MODE_DTX {
            // Silent period (comfort noise, or nothing sent).
            out.fill(0.0);
            return size;
        }
        self.mode = mode;
        let order: &[u8] = match mode {
            0 => &ORDER0,
            1 => &ORDER1,
            2 => &ORDER2,
            3 => &ORDER3,
            4 => &ORDER4,
            5 => &ORDER5,
            6 => &ORDER6,
            _ => &ORDER7,
        };
        bit_reorder(&mut self.frame, &buf[1..size], order);
        if mode == MODE_12K2 {
            self.lsf2lsp_5();
        } else {
            self.lsf2lsp_3();
        }
        for i in 0..4 {
            let lsp = self.lsp[i];
            lspd2lpc(&lsp, &mut self.lpc[i], 5);
        }
        for sub in 0..4 {
            self.decode_pitch_vector(sub);
            let mut fs = Fixed::default();
            self.decode_fixed_sparse(&mut fs, sub);
            let factor = self.decode_gains(sub);
            self.pitch_sharpening(sub, &mut fs);
            if fs.pitch_lag == 0 {
                out.fill(0.0);
                return size;
            }
            set_fixed_vector(&mut self.fixed_vector, &fs, 1.0, SUB);
            let fv = self.fixed_vector;
            let me = dot(&fv, &fv, SUB) / SUB as f32;
            self.fixed_gain[4] = amr_set_fixed_gain(factor, me, &mut self.prediction_error, ENERGY_MEAN[mode], &ENERGY_PRED_FAC);
            for i in 0..SUB {
                self.exc[EXC + i] *= self.pitch_gain[4];
            }
            set_fixed_vector(&mut self.exc[EXC..], &fs, self.fixed_gain[4], SUB);
            for i in 0..SUB {
                self.exc[EXC + i] = libm::truncf(self.exc[EXC + i]);
            }
            let lsf = self.lsf_q[sub];
            let avg = self.lsf_avg;
            let synth_gain = self.fixed_gain_smooth(&lsf, &avg);
            let mut spare = [0f32; SUB];
            let use_spare = self.anti_sparseness(&fs, synth_gain, &mut spare);
            let fixed: [f32; SUB] = if use_spare { spare } else { self.fixed_vector };
            let lpc = self.lpc[sub];
            if self.synthesis(&lpc, synth_gain, &fixed, false) {
                self.synthesis(&lpc, synth_gain, &fixed, true);
            }
            let mut o = [0f32; SUB];
            self.postfilter(&lpc, &mut o);
            out[sub * SUB..(sub + 1) * SUB].copy_from_slice(&o);
            clear_fixed_vector(&mut self.fixed_vector, &fs, SUB);
            self.update_state();
        }
        let gain = (0.939819335f32 as f64 * SAMPLE_SCALE) as f32;
        order2(out, &[-2.0, 1.0], &[-1.933105469, 0.935913085], gain, &mut self.high_pass_mem, 160);
        let avg = self.lsf_avg;
        let q3 = self.lsf_q[3];
        weighted_sum(&mut self.lsf_avg, &avg, &q3, 0.84, 0.16, ORDER);
        size
    }

    fn interpolate_lsf(&mut self, new: &[f32; ORDER]) {
        let q3 = self.lsf_q[3];
        for i in 0..4 {
            weighted_sum(&mut self.lsf_q[i], &q3, new, (0.25 * (3 - i) as f64) as f32, (0.25 * (i + 1) as f64) as f32, ORDER);
        }
    }

    fn lsf2lsp_12k2(&mut self, which: usize, no_r: &[f32; ORDER], quant: &[&[i16]; 5], offset: usize, sign: bool, update: bool) {
        let mut r = [0i16; ORDER];
        for i in 0..5 {
            r[2 * i] = quant[i][offset];
            r[2 * i + 1] = quant[i][offset + 1];
        }
        if sign {
            r[4] = -r[4];
            r[5] = -r[5];
        }
        if update {
            self.prev_lsf_r = r;
        }
        let mut q = [0f32; ORDER];
        for i in 0..ORDER {
            q[i] = (r[i] as f64 * (LSF_R_FAC / 8000.0) + no_r[i] as f64 * (1.0 / 8000.0)) as f32;
        }
        set_min_dist(&mut q, MIN_LSF_SPACING, ORDER);
        if update {
            self.interpolate_lsf(&q);
        }
        lsf2lspd(&mut self.lsp[which], &q, ORDER);
    }

    fn lsf2lsp_5(&mut self) {
        let p = self.frame;
        let quant: [&[i16]; 5] = [
            &LSF_5_1[p[0] as usize * 4..],
            &LSF_5_2[p[1] as usize * 4..],
            &LSF_5_3[(p[2] >> 1) as usize * 4..],
            &LSF_5_4[p[3] as usize * 4..],
            &LSF_5_5[p[4] as usize * 4..],
        ];
        let mut no_r = [0f32; ORDER];
        for i in 0..ORDER {
            no_r[i] = (self.prev_lsf_r[i] as f64 * LSF_R_FAC * PRED_FAC_12K2 + LSF_5_MEAN[i] as f64) as f32;
        }
        let sign = p[2] & 1 != 0;
        self.lsf2lsp_12k2(1, &no_r, &quant, 0, sign, false);
        self.lsf2lsp_12k2(3, &no_r, &quant, 2, sign, true);
        for i in 0..ORDER {
            self.lsp[0][i] = 0.5 * self.prev_lsp_sub4[i] + 0.5 * self.lsp[1][i];
            self.lsp[2][i] = 0.5 * self.lsp[1][i] + 0.5 * self.lsp[3][i];
        }
    }

    fn lsf2lsp_3(&mut self) {
        let p = self.frame;
        let mode = self.mode;
        let mut r = [0i16; ORDER];
        let q1: &[i16] = if mode == MODE_7K95 { &LSF_3_1_MODE_7K95 } else { &LSF_3_1 };
        r[0..3].copy_from_slice(&q1[p[0] as usize * 3..p[0] as usize * 3 + 3]);
        let i2 = (p[1] as usize) << (mode <= MODE_5K15) as usize;
        r[3..6].copy_from_slice(&LSF_3_2[i2 * 3..i2 * 3 + 3]);
        let q3: &[i16] = if mode <= MODE_5K15 { &LSF_3_3_MODE_5K15 } else { &LSF_3_3 };
        r[6..10].copy_from_slice(&q3[p[2] as usize * 4..p[2] as usize * 4 + 4]);
        let mut q = [0f32; ORDER];
        for i in 0..ORDER {
            let t = r[i] as f32 + self.prev_lsf_r[i] as f32 * PRED_FAC[i];
            q[i] = (t as f64 * (LSF_R_FAC / 8000.0) + LSF_3_MEAN[i] as f64 * (1.0 / 8000.0)) as f32;
        }
        set_min_dist(&mut q, MIN_LSF_SPACING, ORDER);
        self.interpolate_lsf(&q);
        self.prev_lsf_r = r;
        lsf2lspd(&mut self.lsp[3], &q, ORDER);
        for i in 1..=3 {
            for j in 0..ORDER {
                self.lsp[i - 1][j] = self.prev_lsp_sub4[j] + (self.lsp[3][j] - self.prev_lsp_sub4[j]) * 0.25 * i as f64;
            }
        }
    }

    fn decode_pitch_vector(&mut self, sub: usize) {
        let mode = self.mode;
        let index = self.frame[sf(sub, 0)] as i32;
        let prev = self.pitch_lag_int as i32;
        let (lag_int, lag_frac) = if mode == MODE_12K2 {
            if sub == 0 || sub == 2 {
                if index < 463 {
                    let li = ((index + 107) * 10923) >> 16;
                    (li, index - li * 6 + 105)
                } else {
                    (index - 368, 0)
                }
            } else {
                let li = (((index + 5) * 10923) >> 16) - 1;
                let lf = index - li * 6 - 3;
                (li + (prev - 5).clamp(18, DELAY_MAX as i32 - 9), lf)
            }
        } else {
            let res = if mode <= MODE_6K7 {
                4
            } else if mode == MODE_7K95 {
                5
            } else {
                6
            };
            let (li, lf) = decode_pitch_lag(index, prev, sub, mode != MODE_4K75 && mode != MODE_5K15, res);
            (li, lf * 2)
        };
        self.pitch_lag_int = lag_int as u8;
        let li = lag_int + (lag_frac > 0) as i32;
        let frac = (lag_frac + 6 - 6 * (lag_frac > 0) as i32) as usize;
        interpolate(&mut self.exc, EXC, EXC as isize + 1 - li as isize, &B60_SINC, 6, frac, 10, SUB);
        self.pitch_vector.copy_from_slice(&self.exc[EXC..EXC + SUB]);
    }

    fn decode_fixed_sparse(&self, fs: &mut Fixed, sub: usize) {
        let mode = self.mode;
        let pulses = &self.frame[sf(sub, 3)..sf(sub, 3) + 10];
        if mode == MODE_12K2 {
            // Ten pulses in 35 bits (FFmpeg's ff_decode_10_pulses_35bits).
            fs.n = 10;
            for i in 0..5 {
                let pos1 = GRAY_DECODE[(pulses[2 * i + 1] & 7) as usize] as i32 + i as i32;
                let pos2 = GRAY_DECODE[(pulses[2 * i] & 7) as usize] as i32 + i as i32;
                let sign = if pulses[2 * i + 1] & 8 != 0 { -1.0 } else { 1.0 };
                fs.x[2 * i + 1] = pos1;
                fs.x[2 * i] = pos2;
                fs.y[2 * i + 1] = sign;
                fs.y[2 * i] = if pos2 < pos1 { -sign } else { sign };
            }
        } else if mode == MODE_10K2 {
            let mut pos = [0i32; 8];
            let ten = |code: u16, pos: &mut [i32; 8], i1: usize, i2: usize, i3: usize| {
                let b = (code >> 3) as usize * 3;
                pos[i1] = ((BASE_FIVE[b + 2] as i32) << 1) + (code & 1) as i32;
                pos[i2] = ((BASE_FIVE[b + 1] as i32) << 1) + ((code >> 1) & 1) as i32;
                pos[i3] = ((BASE_FIVE[b] as i32) << 1) + ((code >> 2) & 1) as i32;
            };
            ten(pulses[4], &mut pos, 0, 4, 1);
            ten(pulses[5], &mut pos, 2, 6, 5);
            let temp = (((pulses[6] >> 2) as i32) * 25 + 12) >> 5;
            pos[3] = temp % 5;
            pos[7] = temp / 5;
            if pos[7] & 1 != 0 {
                pos[3] = 4 - pos[3];
            }
            pos[3] = (pos[3] << 1) + (pulses[6] & 1) as i32;
            pos[7] = (pos[7] << 1) + ((pulses[6] >> 1) & 1) as i32;
            fs.n = 8;
            for i in 0..4 {
                let p1 = (pos[i] << 2) + i as i32;
                let p2 = (pos[i + 4] << 2) + i as i32;
                let sign = if pulses[i] != 0 { -1.0 } else { 1.0 };
                fs.x[i] = p1;
                fs.x[i + 4] = p2;
                fs.y[i] = sign;
                fs.y[i + 4] = if p2 < p1 { -sign } else { sign };
            }
        } else {
            let fi = pulses[0] as i32;
            let p = &mut fs.x;
            if mode <= MODE_5K15 {
                let subset = (((fi >> 3) & 8) + ((sub as i32) << 1)) as usize;
                p[0] = (fi & 7) * 5 + TRACK_POSITION[subset] as i32;
                p[1] = ((fi >> 3) & 7) * 5 + TRACK_POSITION[subset + 1] as i32;
                fs.n = 2;
            } else if mode == MODE_5K9 {
                let s = ((fi & 1) << 1) + 1;
                p[0] = ((fi >> 1) & 7) * 5 + s;
                let s = (fi >> 4) & 3;
                p[1] = ((fi >> 6) & 7) * 5 + s + (s == 3) as i32;
                fs.n = if p[0] == p[1] { 1 } else { 2 };
            } else if mode == MODE_6K7 {
                p[0] = (fi & 7) * 5;
                let s = (fi >> 2) & 2;
                p[1] = ((fi >> 4) & 7) * 5 + s + 1;
                let s = (fi >> 6) & 2;
                p[2] = ((fi >> 8) & 7) * 5 + s + 2;
                fs.n = 3;
            } else {
                p[0] = GRAY_DECODE[(fi & 7) as usize] as i32;
                p[1] = GRAY_DECODE[((fi >> 3) & 7) as usize] as i32 + 1;
                p[2] = GRAY_DECODE[((fi >> 6) & 7) as usize] as i32 + 2;
                let s = (fi >> 9) & 1;
                p[3] = GRAY_DECODE[((fi >> 10) & 7) as usize] as i32 + s + 3;
                fs.n = 4;
            }
            for i in 0..fs.n {
                fs.y[i] = if (pulses[1] >> i) & 1 != 0 { 1.0 } else { -1.0 };
            }
        }
    }

    fn pitch_sharpening(&mut self, sub: usize, fs: &mut Fixed) {
        let mode = self.mode;
        if mode == MODE_12K2 {
            self.beta = (self.pitch_gain[4] as f64).min(1.0) as f32;
        }
        fs.pitch_lag = self.pitch_lag_int as i32;
        fs.pitch_fac = self.beta;
        if mode != MODE_4K75 || sub & 1 != 0 {
            self.beta = self.pitch_gain[4].clamp(0.0, SHARP_MAX as f32);
        }
    }

    /// Pitch gain (into pitch_gain[4]) and the fixed gain's correction factor.
    fn decode_gains(&mut self, sub: usize) -> f32 {
        let mode = self.mode;
        if mode == MODE_12K2 || mode == MODE_7K95 {
            self.pitch_gain[4] = (QUA_GAIN_PIT[self.frame[sf(sub, 1)] as usize] as f64 * (1.0 / 16384.0)) as f32;
            (QUA_GAIN_CODE[self.frame[sf(sub, 2)] as usize] as f64 * (1.0 / 2048.0)) as f32
        } else {
            let (a, b) = if mode >= MODE_6K7 {
                let i = self.frame[sf(sub, 1)] as usize * 2;
                (GAINS_HIGH[i], GAINS_HIGH[i + 1])
            } else if mode >= MODE_5K15 {
                let i = self.frame[sf(sub, 1)] as usize * 2;
                (GAINS_LOW[i], GAINS_LOW[i + 1])
            } else {
                let i = (((self.frame[sf(sub & 2, 1)] as usize) << 1) + (sub & 1)) * 2;
                (GAINS_MODE_4K75[i], GAINS_MODE_4K75[i + 1])
            };
            self.pitch_gain[4] = (a as f64 * (1.0 / 16384.0)) as f32;
            (b as f64 * (1.0 / 4096.0)) as f32
        }
    }

    fn fixed_gain_smooth(&mut self, lsf: &[f32; ORDER], avg: &[f32; ORDER]) -> f32 {
        let mut diff = 0.0f32;
        for i in 0..ORDER {
            diff = (diff as f64 + libm::fabs((avg[i] - lsf[i]) as f64) / avg[i] as f64) as f32;
        }
        self.diff_count = self.diff_count.wrapping_add(1);
        if diff as f64 <= 0.65 {
            self.diff_count = 0;
        }
        if self.diff_count > 10 {
            self.hang_count = 0;
            self.diff_count -= 1;
        }
        if self.hang_count < 40 {
            self.hang_count += 1;
        } else if self.mode < MODE_7K4 || self.mode == MODE_10K2 {
            let sm = (4.0 * diff as f64 - 1.6).clamp(0.0, 1.0) as f32;
            let g = &self.fixed_gain;
            let mean = ((g[0] + g[1] + g[2] + g[3] + g[4]) as f64 * 0.2) as f32;
            return ((sm * g[4]) as f64 + (1.0 - sm as f64) * mean as f64) as f32;
        }
        self.fixed_gain[4]
    }

    /// Phase dispersion of the pulses at low rates; true when `out` holds the filtered vector.
    fn anti_sparseness(&mut self, fs: &Fixed, fixed_gain: f32, out: &mut [f32; SUB]) -> bool {
        let pg = self.pitch_gain[4] as f64;
        let mut nr: u8 = if pg < 0.6 {
            0
        } else if pg < 0.9 {
            1
        } else {
            2
        };
        if fixed_gain as f64 > 2.0 * self.prev_sparse_fixed_gain as f64 {
            self.ir_filter_onset = 2;
        } else if self.ir_filter_onset > 0 {
            self.ir_filter_onset -= 1;
        }
        if self.ir_filter_onset == 0 {
            let count = self.pitch_gain.iter().filter(|&&g| (g as f64) < 0.6).count();
            if count > 2 {
                nr = 0;
            }
            if nr > self.prev_ir_filter_nr + 1 {
                nr -= 1;
            }
        } else if nr < 2 {
            nr += 1;
        }
        if (fixed_gain as f64) < 5.0 {
            nr = 2;
        }
        let mut filtered = false;
        if self.mode != MODE_7K4 && self.mode < MODE_10K2 && nr < 2 {
            let filter: &[f32; SUB] = if nr == 1 {
                &IR_FILTER_MEDIUM
            } else if self.mode == MODE_7K95 {
                &IR_FILTER_STRONG_MODE_7K95
            } else {
                &IR_FILTER_STRONG
            };
            apply_ir_filter(out, fs, filter);
            filtered = true;
        }
        self.prev_ir_filter_nr = nr;
        self.prev_sparse_fixed_gain = fixed_gain;
        filtered
    }

    /// Excitation and the synthesis filter; true if the result overflowed.
    fn synthesis(&mut self, lpc: &[f32; ORDER], fixed_gain: f32, fixed: &[f32; SUB], overflow: bool) -> bool {
        if overflow {
            for v in self.pitch_vector.iter_mut() {
                *v = (*v as f64 * 0.25) as f32;
            }
        }
        let mut e = [0f32; SUB];
        let pv = self.pitch_vector;
        weighted_sum(&mut e, &pv, fixed, self.pitch_gain[4], fixed_gain, SUB);
        let pg = self.pitch_gain[4];
        if pg as f64 > 0.5 && !overflow {
            let energy = dot(&e, &e, SUB);
            let f = if self.mode == MODE_12K2 { 0.25 * (pg as f64).min(1.0) } else { 0.5 * (pg as f64).min(SHARP_MAX) };
            let pitch_factor = (pg as f64 * f) as f32;
            for i in 0..SUB {
                e[i] += pitch_factor * pv[i];
            }
            scale_to_energy(&mut e, energy, SUB);
        }
        lp_synthesis(&mut self.samples_in, ORDER, lpc, &e, SUB, ORDER);
        self.samples_in[ORDER..].iter().any(|s| libm::fabsf(*s) > 32768.0)
    }

    fn update_state(&mut self) {
        self.prev_lsp_sub4 = self.lsp[3];
        self.exc.copy_within(SUB..SUB + EXC, 0);
        self.pitch_gain.copy_within(1..5, 0);
        self.fixed_gain.copy_within(1..5, 0);
        self.samples_in.copy_within(SUB..SUB + ORDER, 0);
    }

    fn tilt_factor(&self, lpc_n: &[f32; ORDER], lpc_d: &[f32; ORDER]) -> f32 {
        const R: usize = 22;
        let mut ib = [0f32; ORDER + R];
        ib[ORDER] = 1.0;
        ib[ORDER + 1..ORDER + 1 + ORDER].copy_from_slice(lpc_n);
        let input: [f32; R] = core::array::from_fn(|i| ib[ORDER + i]);
        lp_synthesis(&mut ib, ORDER, lpc_d, &input, R, ORDER);
        let hf = &ib[ORDER..];
        let rh0 = dot(hf, hf, R);
        let rh1 = dot(hf, &hf[1..], R - 1);
        if rh1 as f64 >= 0.0 {
            ((rh1 / rh0) as f64 * 0.8) as f32
        } else {
            0.0
        }
    }

    fn postfilter(&mut self, lpc: &[f32; ORDER], out: &mut [f32; SUB]) {
        let samples: [f32; SUB] = core::array::from_fn(|i| self.samples_in[ORDER + i]);
        let speech_gain = dot(&samples, &samples, SUB);
        let (gn, gd): (&[f32; 10], &[f32; 10]) = if self.mode == MODE_12K2 || self.mode == MODE_10K2 { (&POW_0_7, &POW_0_75) } else { (&POW_0_55, &POW_0_7) };
        let mut lpc_n = [0f32; ORDER];
        let mut lpc_d = [0f32; ORDER];
        for i in 0..ORDER {
            lpc_n[i] = lpc[i] * gn[i];
            lpc_d[i] = lpc[i] * gd[i];
        }
        let mut pole = [0f32; SUB + ORDER];
        pole[..ORDER].copy_from_slice(&self.postfilter_mem);
        lp_synthesis(&mut pole, ORDER, &lpc_d, &samples, SUB, ORDER);
        self.postfilter_mem.copy_from_slice(&pole[SUB..SUB + ORDER]);
        zero_synthesis(out, &lpc_n, &pole, ORDER, SUB, ORDER);
        let tilt = self.tilt_factor(&lpc_n, &lpc_d);
        tilt_compensation(&mut self.tilt_mem, tilt, out, SUB);
        adaptive_gain_control(out, speech_gain, SUB, 0.9, &mut self.postfilter_agc);
    }
}

fn apply_ir_filter(out: &mut [f32; SUB], fs: &Fixed, filter: &[f32; SUB]) {
    let mut f1 = [0f32; SUB];
    let mut f2 = [0f32; SUB];
    let lag = fs.pitch_lag as usize;
    let fac = fs.pitch_fac;
    if lag < SUB {
        circ_add(&mut f1, filter, filter, lag, fac, SUB);
        if lag < SUB >> 1 {
            circ_add(&mut f2, filter, &f1, lag, fac, SUB);
        }
    }
    out.fill(0.0);
    for i in 0..fs.n {
        let x = fs.x[i] as usize;
        let y = fs.y[i];
        let fp: &[f32; SUB] = if x + lag >= SUB {
            filter
        } else if x + 2 * lag >= SUB {
            &f1
        } else {
            &f2
        };
        let cur = *out;
        circ_add(out, &cur, fp, x, y, SUB);
    }
}

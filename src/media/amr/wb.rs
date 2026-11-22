//! AMR wideband (16 kHz speech, 6.6 to 23.85 kbit/s), after FFmpeg's amrwbdec.c. Speech is
//! made at 12.8 kHz, resampled to 16 kHz, and the 6.4-7 kHz band added from shaped noise.

use super::wbtab::*;
use super::*;

const SUB: usize = 64;
const SUB16: usize = 80;
const ORDER: usize = 16;
const ORDER16: usize = 20;
const DELAY_MAX: usize = 231;
const DELAY_MIN: i32 = 34;
const EXC: usize = DELAY_MAX + ORDER + 1;
const HB_FIR: usize = 30;
const UPS_FIR: usize = 12;
const UPS_MEM: usize = 2 * UPS_FIR;

const MODE_6K60: usize = 0;
const MODE_8K85: usize = 1;
const MODE_23K85: usize = 8;
const MODE_SID: usize = 9;

const MIN_ISF_SPACING: f64 = 128.0 / 32768.0;
const PRED_FACTOR: f64 = 1.0 / 3.0;
const PREEMPH_FAC: f32 = 0.68;
const HPF_ZEROS: [f32; 2] = [-2.0, 1.0];
const HPF_31_POLES: [f32; 2] = [-1.978881836, 0.979125977];
const HPF_31_GAIN: f32 = 0.989501953;
const HPF_400_POLES: [f32; 2] = [-1.787109375, 0.864257812];
const HPF_400_GAIN: f32 = 0.893554687;

const fn sf(k: usize, f: usize) -> usize {
    8 + 12 * k + f
}

pub struct Wb {
    frame: [u16; 56],
    mode: usize,
    isf_cur: [f32; ORDER],
    isf_q_past: [f32; ORDER],
    isf_past_final: [f32; ORDER],
    isp: [[f64; ORDER]; 4],
    isp_sub4_past: [f64; ORDER],
    lp_coef: [[f32; ORDER]; 4],
    base_pitch_lag: u8,
    pitch_lag_int: u8,
    exc: [f32; EXC + 1 + SUB],
    pitch_vector: [f32; SUB],
    fixed_vector: [f32; SUB],
    prediction_error: [f32; 4],
    pitch_gain: [f32; 6],
    fixed_gain: [f32; 2],
    tilt_coef: f32,
    prev_ir_filter_nr: u8,
    prev_tr_gain: f32,
    samples_az: [f32; ORDER + SUB],
    samples_up: [f32; UPS_MEM + SUB],
    samples_hb: [f32; ORDER16 + SUB16],
    hpf_31_mem: [f32; 2],
    hpf_400_mem: [f32; 2],
    demph_mem: f32,
    bpf_6_7_mem: [f32; HB_FIR],
    lpf_7_mem: [f32; HB_FIR],
    lfg: [u32; 64],
    lfg_index: u32,
    first_frame: bool,
}

impl Wb {
    pub fn new() -> Wb {
        let mut w = Wb {
            frame: [0; 56],
            mode: 0,
            isf_cur: [0.0; ORDER],
            isf_q_past: [0.0; ORDER],
            isf_past_final: [0.0; ORDER],
            isp: [[0.0; ORDER]; 4],
            isp_sub4_past: [0.0; ORDER],
            lp_coef: [[0.0; ORDER]; 4],
            base_pitch_lag: 0,
            pitch_lag_int: 0,
            exc: [0.0; EXC + 1 + SUB],
            pitch_vector: [0.0; SUB],
            fixed_vector: [0.0; SUB],
            prediction_error: [-14.0; 4],
            pitch_gain: [0.0; 6],
            fixed_gain: [0.0; 2],
            tilt_coef: 0.0,
            prev_ir_filter_nr: 0,
            prev_tr_gain: 0.0,
            samples_az: [0.0; ORDER + SUB],
            samples_up: [0.0; UPS_MEM + SUB],
            samples_hb: [0.0; ORDER16 + SUB16],
            hpf_31_mem: [0.0; 2],
            hpf_400_mem: [0.0; 2],
            demph_mem: 0.0,
            bpf_6_7_mem: [0.0; HB_FIR],
            lpf_7_mem: [0.0; HB_FIR],
            lfg: LFG_INIT,
            lfg_index: 0,
            first_frame: true,
        };
        for i in 0..ORDER {
            w.isf_past_final[i] = ISF_INIT[i] as f32 * (1.0 / 32768.0);
        }
        w
    }

    fn lfg_get(&mut self) -> u32 {
        let i = self.lfg_index;
        let a = self.lfg[(i.wrapping_sub(24) & 63) as usize].wrapping_add(self.lfg[(i.wrapping_sub(55) & 63) as usize]);
        self.lfg[(i & 63) as usize] = a;
        self.lfg_index = i.wrapping_add(1);
        a
    }

    /// One frame into 320 samples; returns the bytes used (0 = not enough data).
    pub fn frame(&mut self, buf: &[u8], out: &mut [f32; 320]) -> usize {
        let mode = (buf[0] >> 3 & 15) as usize;
        let quality = buf[0] & 4 != 0;
        let size = ((CF_SIZES[mode] as usize + 7) >> 3) + 1;
        if buf.len() < size {
            return 0;
        }
        if mode >= MODE_SID || !quality {
            out.fill(0.0);
            return size;
        }
        self.mode = mode;
        let order: &[u16] = match mode {
            0 => &ORDER0,
            1 => &ORDER1,
            2 => &ORDER2,
            3 => &ORDER3,
            4 => &ORDER4,
            5 => &ORDER5,
            6 => &ORDER6,
            7 => &ORDER7,
            _ => &ORDER8,
        };
        bit_reorder(&mut self.frame, &buf[1..size], order);
        let ind: [usize; 7] = core::array::from_fn(|i| self.frame[1 + i] as usize);
        let mut isf = [0f32; ORDER];
        let s = 1.0f32 / 32768.0;
        if mode == MODE_6K60 {
            for i in 0..9 {
                isf[i] = DICO1_ISF[ind[0] * 9 + i] as f32 * s;
            }
            for i in 0..7 {
                isf[i + 9] = DICO2_ISF[ind[1] * 7 + i] as f32 * s;
            }
            for i in 0..5 {
                isf[i] += DICO21_ISF_36B[ind[2] * 5 + i] as f32 * s;
            }
            for i in 0..4 {
                isf[i + 5] += DICO22_ISF_36B[ind[3] * 4 + i] as f32 * s;
            }
            for i in 0..7 {
                isf[i + 9] += DICO23_ISF_36B[ind[4] * 7 + i] as f32 * s;
            }
        } else {
            for i in 0..9 {
                isf[i] = DICO1_ISF[ind[0] * 9 + i] as f32 * s;
            }
            for i in 0..7 {
                isf[i + 9] = DICO2_ISF[ind[1] * 7 + i] as f32 * s;
            }
            for i in 0..3 {
                isf[i] += DICO21_ISF[ind[2] * 3 + i] as f32 * s;
                isf[i + 3] += DICO22_ISF[ind[3] * 3 + i] as f32 * s;
                isf[i + 6] += DICO23_ISF[ind[4] * 3 + i] as f32 * s;
                isf[i + 9] += DICO24_ISF[ind[5] * 3 + i] as f32 * s;
            }
            for i in 0..4 {
                isf[i + 12] += DICO25_ISF[ind[6] * 4 + i] as f32 * s;
            }
        }
        // Add the mean and the prediction from the last frame.
        for i in 0..ORDER {
            let tmp = isf[i];
            isf[i] += ISF_MEAN[i] as f32 * s;
            isf[i] = (isf[i] as f64 + PRED_FACTOR * self.isf_q_past[i] as f64) as f32;
            self.isf_q_past[i] = tmp;
        }
        set_min_dist(&mut isf, MIN_ISF_SPACING, ORDER - 1);
        self.isf_cur = isf;
        let stab_fac = stability_factor(&self.isf_cur, &self.isf_past_final);
        self.isf_cur[ORDER - 1] = (self.isf_cur[ORDER - 1] as f64 * 2.0) as f32;
        let cur = self.isf_cur;
        lsf2lspd(&mut self.isp[3], &cur, ORDER);
        if self.first_frame {
            self.first_frame = false;
            self.isp_sub4_past = self.isp[3];
        }
        for k in 0..3 {
            let c = ISFP_INTER[k];
            for i in 0..ORDER {
                self.isp[k][i] = (1.0 - c as f64) * self.isp_sub4_past[i] + c as f64 * self.isp[3][i];
            }
        }
        for sub in 0..4 {
            let isp = self.isp[sub];
            amrwb_lsp2lpc(&isp, &mut self.lp_coef[sub], ORDER);
        }
        for sub in 0..4 {
            self.decode_pitch_vector(sub);
            self.decode_fixed_vector(sub);
            self.pitch_sharpening();
            let vq = self.frame[sf(sub, 2)] as usize;
            let (g0, g1) = if mode <= MODE_8K85 { (QUA_GAIN_6B[vq * 2], QUA_GAIN_6B[vq * 2 + 1]) } else { (QUA_GAIN_7B[vq * 2], QUA_GAIN_7B[vq * 2 + 1]) };
            self.pitch_gain[0] = g0 as f32 * (1.0 / 16384.0);
            let factor = g1 as f32 * (1.0 / 2048.0);
            let fv = self.fixed_vector;
            let me = dot(&fv, &fv, SUB) / SUB as f32;
            self.fixed_gain[0] = amr_set_fixed_gain(factor, me, &mut self.prediction_error, 30.0, &ENERGY_PRED_FAC);
            let voice_fac = self.voice_factor();
            self.tilt_coef = (voice_fac as f64 * 0.25 + 0.25) as f32;
            for i in 0..SUB {
                let e = &mut self.exc[EXC + i];
                *e *= self.pitch_gain[0];
                *e += self.fixed_gain[0] * self.fixed_vector[i];
                *e = libm::truncf(*e);
            }
            let fg = self.fixed_gain[0];
            let synth_gain = noise_enhancer(fg, &mut self.prev_tr_gain, voice_fac, stab_fac);
            let mut fixed = self.anti_sparseness();
            pitch_enhancer(&mut fixed, voice_fac);
            let lpc = self.lp_coef[sub];
            let synth_exc = self.synthesis(&lpc, synth_gain, &fixed);
            let mut o = [0f32; SUB16];
            self.subframe_output(&mut o, &synth_exc, sub);
            out[sub * SUB16..(sub + 1) * SUB16].copy_from_slice(&o);
        }
        self.isp_sub4_past = self.isp[3];
        self.isf_past_final = self.isf_cur;
        size
    }

    fn decode_pitch_vector(&mut self, sub: usize) {
        let mode = self.mode;
        let index = self.frame[sf(sub, 0)] as i32;
        let (lag_int, lag_frac);
        if mode <= MODE_8K85 {
            if sub == 0 || (sub == 2 && mode != MODE_6K60) {
                if index < 116 {
                    let li = (index + 69) >> 1;
                    lag_int = li;
                    lag_frac = (index - (li << 1) + 68) * 2;
                } else {
                    lag_int = index - 24;
                    lag_frac = 0;
                }
                self.base_pitch_lag = (lag_int - 8 - (lag_frac < 0) as i32).clamp(DELAY_MIN, DELAY_MAX as i32 - 15) as u8;
            } else {
                let li = (index + 1) >> 1;
                lag_frac = (index - (li << 1)) * 2;
                lag_int = li + self.base_pitch_lag as i32;
            }
        } else if sub == 0 || sub == 2 {
            if index < 376 {
                let li = (index + 137) >> 2;
                lag_int = li;
                lag_frac = index - (li << 2) + 136;
            } else if index < 440 {
                let li = (index + 257 - 376) >> 1;
                lag_int = li;
                lag_frac = (index - (li << 1) + 256 - 376) * 2;
            } else {
                lag_int = index - 280;
                lag_frac = 0;
            }
            self.base_pitch_lag = (lag_int - 8 - (lag_frac < 0) as i32).clamp(DELAY_MIN, DELAY_MAX as i32 - 15) as u8;
        } else {
            let li = (index + 1) >> 2;
            lag_frac = index - (li << 2);
            lag_int = li + self.base_pitch_lag as i32;
        }
        self.pitch_lag_int = lag_int as u8;
        let li = lag_int + (lag_frac > 0) as i32;
        let frac = (lag_frac + if lag_frac > 0 { 0 } else { 4 }) as usize;
        interpolate(&mut self.exc, EXC, EXC as isize + 1 - li as isize, &AC_INTER, 4, frac, ORDER, SUB + 1);
        if self.frame[sf(sub, 1)] != 0 {
            self.pitch_vector.copy_from_slice(&self.exc[EXC..EXC + SUB]);
        } else {
            for i in 0..SUB {
                let e = &self.exc;
                self.pitch_vector[i] = (0.18 * e[EXC + i - 1] as f64 + 0.64 * e[EXC + i] as f64 + 0.18 * e[EXC + i + 1] as f64) as f32;
            }
            self.exc[EXC..EXC + SUB].copy_from_slice(&self.pitch_vector);
        }
    }

    fn decode_fixed_vector(&mut self, sub: usize) {
        let mode = self.mode;
        let hi: [i32; 4] = core::array::from_fn(|i| self.frame[sf(sub, 4 + i)] as i32);
        let lo: [i32; 4] = core::array::from_fn(|i| self.frame[sf(sub, 8 + i)] as i32);
        let mut sig = [[0i32; 6]; 4];
        let spacing = if mode == MODE_6K60 { 2 } else { 4 };
        match mode {
            0 => (0..2).for_each(|i| p1(&mut sig[i], lo[i], 5, 1)),
            1 => (0..4).for_each(|i| p1(&mut sig[i], lo[i], 4, 1)),
            2 => (0..4).for_each(|i| p2(&mut sig[i], lo[i], 4, 1)),
            3 => {
                (0..2).for_each(|i| p3(&mut sig[i], lo[i], 4, 1));
                (2..4).for_each(|i| p2(&mut sig[i], lo[i], 4, 1));
            }
            4 => (0..4).for_each(|i| p3(&mut sig[i], lo[i], 4, 1)),
            5 => (0..4).for_each(|i| p4(&mut sig[i], lo[i] + (hi[i] << 14), 4, 1)),
            6 => {
                (0..2).for_each(|i| p5(&mut sig[i], lo[i] + (hi[i] << 10), 4, 1));
                (2..4).for_each(|i| p4(&mut sig[i], lo[i] + (hi[i] << 14), 4, 1));
            }
            _ => (0..4).for_each(|i| p6(&mut sig[i], lo[i] + (hi[i] << 11), 4, 1)),
        }
        self.fixed_vector = [0.0; SUB];
        for i in 0..4 {
            for j in 0..PULSES_NB[mode * 4 + i] as usize {
                let pos = (sig[i][j].abs() - 1) as usize * spacing + i;
                self.fixed_vector[pos] += if sig[i][j] < 0 { -1.0 } else { 1.0 };
            }
        }
    }

    fn pitch_sharpening(&mut self) {
        for i in (1..SUB).rev() {
            self.fixed_vector[i] -= self.fixed_vector[i - 1] * self.tilt_coef;
        }
        for i in self.pitch_lag_int as usize..SUB {
            self.fixed_vector[i] = (self.fixed_vector[i] as f64 + self.fixed_vector[i - self.pitch_lag_int as usize] as f64 * 0.85) as f32;
        }
    }

    fn voice_factor(&self) -> f32 {
        let pv = &self.pitch_vector;
        let fv = &self.fixed_vector;
        let pg = self.pitch_gain[0];
        let fg = self.fixed_gain[0];
        let p = dot(pv, pv, SUB) as f64 * pg as f64 * pg as f64;
        let f = dot(fv, fv, SUB) as f64 * fg as f64 * fg as f64;
        ((p - f) / (p + f + 0.01)) as f32
    }

    fn anti_sparseness(&mut self) -> [f32; SUB] {
        let fv = self.fixed_vector;
        if self.mode > MODE_8K85 {
            return fv;
        }
        let pg = self.pitch_gain[0] as f64;
        let mut nr: u8 = if pg < 0.6 {
            0
        } else if pg < 0.9 {
            1
        } else {
            2
        };
        if self.fixed_gain[0] as f64 > 3.0 * self.fixed_gain[1] as f64 {
            if nr < 2 {
                nr += 1;
            }
        } else {
            let count = self.pitch_gain.iter().filter(|&&g| (g as f64) < 0.6).count();
            if count > 2 {
                nr = 0;
            }
            if nr > self.prev_ir_filter_nr + 1 {
                nr -= 1;
            }
        }
        self.prev_ir_filter_nr = nr;
        nr += (self.mode == MODE_8K85) as u8;
        if nr < 2 {
            let coef: &[f32; SUB] = if nr == 0 { &IR_FILTER_STR } else { &IR_FILTER_MID };
            let mut buf = [0f32; SUB];
            for i in 0..SUB {
                if fv[i] != 0.0 {
                    let cur = buf;
                    circ_add(&mut buf, &cur, coef, i, fv[i], SUB);
                }
            }
            return buf;
        }
        fv
    }

    /// Excitation through the synthesis filter (into samples_az); returns the excitation.
    fn synthesis(&mut self, lpc: &[f32; ORDER], fixed_gain: f32, fixed: &[f32; SUB]) -> [f32; SUB] {
        let mut e = [0f32; SUB];
        let pv = self.pitch_vector;
        let pg = self.pitch_gain[0];
        weighted_sum(&mut e, &pv, fixed, pg, fixed_gain, SUB);
        if pg as f64 > 0.5 && self.mode <= MODE_8K85 {
            let energy = dot(&e, &e, SUB);
            let pf = (0.25 * pg as f64 * pg as f64) as f32;
            for i in 0..SUB {
                e[i] += pf * pv[i];
            }
            scale_to_energy(&mut e, energy, SUB);
        }
        lp_synthesis(&mut self.samples_az, ORDER, lpc, &e, SUB, ORDER);
        e
    }

    fn subframe_output(&mut self, out: &mut [f32; SUB16], synth_exc: &[f32; SUB], sub: usize) {
        // De-emphasis and high-pass at 12.8 kHz.
        let m = PREEMPH_FAC;
        let mut prev = self.samples_az[ORDER] + m * self.demph_mem;
        self.samples_up[UPS_MEM] = prev;
        for i in 1..SUB {
            prev = self.samples_az[ORDER + i] + prev * m;
            self.samples_up[UPS_MEM + i] = prev;
        }
        self.demph_mem = prev;
        order2(&mut self.samples_up[UPS_MEM..], &HPF_ZEROS, &HPF_31_POLES, HPF_31_GAIN, &mut self.hpf_31_mem, SUB);
        // Upsample 5/4 to 16 kHz.
        let base = UPS_FIR; // &samples_up[UPS_FIR_SIZE]
        let in0 = base + 1 - UPS_FIR; // in - UPS_FIR_SIZE + 1
        let mut i = 0;
        let mut int_part = 0;
        for _ in 0..SUB16 / 5 {
            out[i] = self.samples_up[base + int_part];
            i += 1;
            let mut frac = 4;
            for _ in 1..5 {
                let f = &UPSAMPLE_FIR[(4 - frac) * UPS_MEM..(4 - frac + 1) * UPS_MEM];
                out[i] = dot(&self.samples_up[in0 + int_part..], f, UPS_MEM);
                int_part += 1;
                frac -= 1;
                i += 1;
            }
        }
        // High band: noise shaped by the speech's filter.
        let mut hb = [0f32; SUB16];
        hb[..SUB].copy_from_slice(&self.samples_up[UPS_MEM..UPS_MEM + SUB]);
        order2(&mut hb[..SUB], &HPF_ZEROS, &HPF_400_POLES, HPF_400_GAIN, &mut self.hpf_400_mem, SUB);
        let hb_gain = self.find_hb_gain(&hb, self.frame[sf(sub, 3)] as usize, self.frame[0] != 0);
        let mut hb_exc = [0f32; SUB16];
        let energy = dot(synth_exc, synth_exc, SUB);
        for v in hb_exc.iter_mut() {
            *v = (32768.0 - (self.lfg_get() as u16) as f64) as f32;
        }
        scale_to_energy(&mut hb_exc, energy * hb_gain * hb_gain, SUB16);
        self.hb_synthesis(sub, &hb_exc);
        let src: [f32; SUB16] = core::array::from_fn(|i| self.samples_hb[ORDER16 + i]);
        hb_fir(&mut hb, &BPF_6_7_COEF, &mut self.bpf_6_7_mem, &src);
        if self.mode == MODE_23K85 {
            let src = hb;
            hb_fir(&mut hb, &LPF_7_COEF, &mut self.lpf_7_mem, &src);
        }
        for i in 0..SUB16 {
            out[i] = (out[i] + hb[i]) * (1.0 / 32768.0);
        }
        // Next subframe.
        self.exc.copy_within(SUB..SUB + EXC, 0);
        self.pitch_gain.copy_within(0..5, 1);
        self.fixed_gain[1] = self.fixed_gain[0];
        self.samples_az.copy_within(SUB..SUB + ORDER, 0);
        self.samples_up.copy_within(SUB..SUB + UPS_MEM, 0);
        self.samples_hb.copy_within(SUB16..SUB16 + ORDER16, 0);
    }

    fn find_hb_gain(&self, synth: &[f32], idx: usize, vad: bool) -> f32 {
        if self.mode == MODE_23K85 {
            return QUA_HB_GAIN[idx] as f32 * (1.0 / 16384.0);
        }
        let tmp = dot(synth, &synth[1..], SUB - 1);
        let tilt = if tmp > 0.0 { tmp / dot(synth, synth, SUB) } else { 0.0 };
        let wsp = vad as i32 as f64;
        ((1.0 - tilt as f64) * (1.25 - 0.25 * wsp)).clamp(0.1, 1.0) as f32
    }

    fn hb_synthesis(&mut self, sub: usize, exc: &[f32; SUB16]) {
        let mut hb_lpc = [0f32; ORDER16];
        let order;
        if self.mode == MODE_6K60 {
            let mut e_isf = [0f32; ORDER16];
            let w = ISFP_INTER[sub];
            weighted_sum(&mut e_isf, &self.isf_past_final, &self.isf_cur, w, (1.0 - w as f64) as f32, ORDER);
            extrapolate_isf(&mut e_isf);
            e_isf[ORDER16 - 1] = (e_isf[ORDER16 - 1] as f64 * 2.0) as f32;
            let mut e_isp = [0f64; ORDER16];
            lsf2lspd(&mut e_isp, &e_isf, ORDER16);
            amrwb_lsp2lpc(&e_isp, &mut hb_lpc, ORDER16);
            let src = hb_lpc;
            lpc_weighting(&mut hb_lpc, &src, 0.9, ORDER16);
            order = ORDER16;
        } else {
            let src = self.lp_coef[sub];
            lpc_weighting(&mut hb_lpc, &src, 0.6, ORDER);
            order = ORDER;
        }
        lp_synthesis(&mut self.samples_hb, ORDER16, &hb_lpc, exc, SUB16, order);
    }
}

fn stability_factor(isf: &[f32; ORDER], past: &[f32; ORDER]) -> f32 {
    let mut acc = 0.0f32;
    for i in 0..ORDER - 1 {
        acc += (isf[i] - past[i]) * (isf[i] - past[i]);
    }
    (1.25 - acc as f64 * 0.8 * 512.0).max(0.0) as f32
}

fn noise_enhancer(fixed_gain: f32, prev: &mut f32, voice_fac: f32, stab_fac: f32) -> f32 {
    let sm = (0.5 * (1.0f32 - voice_fac) as f64 * stab_fac as f64) as f32;
    let g0 = if fixed_gain < *prev {
        prev.min(fixed_gain + fixed_gain * (6226.0f32 * (1.0 / 32768.0)))
    } else {
        prev.max(fixed_gain * (27536.0f32 * (1.0 / 32768.0)))
    };
    *prev = g0;
    sm * g0 + (1.0 - sm) * fixed_gain
}

fn pitch_enhancer(v: &mut [f32; SUB], voice_fac: f32) {
    let cpe = (0.125 * (1.0f32 + voice_fac) as f64) as f32;
    let mut last = v[0];
    v[0] -= cpe * v[1];
    for i in 1..SUB - 1 {
        let cur = v[i];
        v[i] -= cpe * (last + v[i + 1]);
        last = cur;
    }
    v[SUB - 1] -= cpe * last;
}

fn hb_fir(out: &mut [f32; SUB16], coef: &[f32; 31], mem: &mut [f32; HB_FIR], input: &[f32; SUB16]) {
    let mut data = [0f32; SUB16 + HB_FIR];
    data[..HB_FIR].copy_from_slice(mem);
    data[HB_FIR..].copy_from_slice(input);
    for i in 0..SUB16 {
        let mut v = 0.0f32;
        for j in 0..=HB_FIR {
            v += data[i + j] * coef[j];
        }
        out[i] = v;
    }
    mem.copy_from_slice(&data[SUB16..SUB16 + HB_FIR]);
}

fn lpc_weighting(out: &mut [f32], lpc: &[f32], gamma: f32, n: usize) {
    let mut fac = gamma;
    for i in 0..n {
        out[i] = lpc[i] * fac;
        fac *= gamma;
    }
}

fn auto_correlation(d: &[f32; ORDER - 2], mean: f32, lag: usize) -> f32 {
    let mut sum = 0.0f32;
    for i in 7..ORDER - 2 {
        let p = (d[i] - mean) * (d[i - lag] - mean);
        sum += p * p;
    }
    sum
}

fn extrapolate_isf(isf: &mut [f32; ORDER16]) {
    isf[ORDER16 - 1] = isf[ORDER - 1];
    let mut d = [0f32; ORDER - 2];
    for i in 0..ORDER - 2 {
        d[i] = isf[i + 1] - isf[i];
    }
    let mut mean = 0.0f32;
    for i in 2..ORDER - 2 {
        mean += d[i] * (1.0 / (ORDER - 4) as f32);
    }
    let mut corr = [0f32; 3];
    let mut imax = 0;
    for i in 0..3 {
        corr[i] = auto_correlation(&d, mean, i + 2);
        if corr[i] > corr[imax] {
            imax = i;
        }
    }
    imax += 1;
    for i in ORDER - 1..ORDER16 - 1 {
        isf[i] = isf[i - 1] + isf[i - 1 - imax] - isf[i - 2 - imax];
    }
    let est = (7965.0 + (isf[2] - isf[3] - isf[4]) as f64 / 6.0) as f32;
    let t = est.min(7600.0) - isf[ORDER - 2];
    let scale = (0.5 * t as f64 / (isf[ORDER16 - 2] - isf[ORDER - 2]) as f64) as f32;
    let mut j = 0;
    for i in ORDER - 1..ORDER16 - 1 {
        d[j] = scale * (isf[i] - isf[i - 1]);
        j += 1;
    }
    for i in 1..ORDER16 - ORDER {
        if ((d[i] + d[i - 1]) as f64) < 5.0 {
            if d[i] > d[i - 1] {
                d[i - 1] = (5.0 - d[i] as f64) as f32;
            } else {
                d[i] = (5.0 - d[i - 1] as f64) as f32;
            }
        }
    }
    let mut j = 0;
    for i in ORDER - 1..ORDER16 - 1 {
        isf[i] = isf[i - 1] + d[j] * (1.0 / 32768.0);
        j += 1;
    }
    for v in isf.iter_mut().take(ORDER16 - 1) {
        *v = (*v as f64 * 0.8) as f32;
    }
}

// ---- Pulse positions in the fixed codebook (FFmpeg's decode_Np_track) ----

fn bits(x: i32, lsb: i32, len: i32) -> i32 {
    (x >> lsb) & ((1 << len) - 1)
}

fn bit(x: i32, p: i32) -> i32 {
    (x >> p) & 1
}

fn p1(out: &mut [i32], code: i32, m: i32, off: i32) {
    let pos = bits(code, 0, m) + off;
    out[0] = if bit(code, m) != 0 { -pos } else { pos };
}

fn p2(out: &mut [i32], code: i32, m: i32, off: i32) {
    let pos0 = bits(code, m, m) + off;
    let pos1 = bits(code, 0, m) + off;
    out[0] = if bit(code, 2 * m) != 0 { -pos0 } else { pos0 };
    out[1] = if bit(code, 2 * m) != 0 { -pos1 } else { pos1 };
    if pos0 > pos1 {
        out[1] = -out[1];
    }
}

fn p3(out: &mut [i32], code: i32, m: i32, off: i32) {
    let half = bit(code, 2 * m - 1) << (m - 1);
    p2(out, bits(code, 0, 2 * m - 1), m - 1, off + half);
    p1(&mut out[2..], bits(code, 2 * m, m + 1), m, off);
}

fn p4(out: &mut [i32], code: i32, m: i32, off: i32) {
    let b_off = 1 << (m - 1);
    match bits(code, 4 * m - 2, 2) {
        0 => {
            let half = bit(code, 4 * m - 3) << (m - 1);
            let sub = bit(code, 2 * m - 3) << (m - 2);
            p2(out, bits(code, 0, 2 * m - 3), m - 2, off + half + sub);
            p2(&mut out[2..], bits(code, 2 * m - 2, 2 * m - 1), m - 1, off + half);
        }
        1 => {
            p1(out, bits(code, 3 * m - 2, m), m - 1, off);
            p3(&mut out[1..], bits(code, 0, 3 * m - 2), m - 1, off + b_off);
        }
        2 => {
            p2(out, bits(code, 2 * m - 1, 2 * m - 1), m - 1, off);
            p2(&mut out[2..], bits(code, 0, 2 * m - 1), m - 1, off + b_off);
        }
        _ => {
            p3(out, bits(code, m, 3 * m - 2), m - 1, off);
            p1(&mut out[3..], bits(code, 0, m), m - 1, off + b_off);
        }
    }
}

fn p5(out: &mut [i32], code: i32, m: i32, off: i32) {
    let half = bit(code, 5 * m - 1) << (m - 1);
    p3(out, bits(code, 2 * m + 1, 3 * m - 2), m - 1, off + half);
    p2(&mut out[3..], bits(code, 0, 2 * m + 1), m, off);
}

fn p6(out: &mut [i32], code: i32, m: i32, off: i32) {
    let b_off = 1 << (m - 1);
    let more = bit(code, 6 * m - 5) << (m - 1);
    let other = b_off - more;
    match bits(code, 6 * m - 4, 2) {
        0 => {
            p1(out, bits(code, 0, m), m - 1, off + more);
            p5(&mut out[1..], bits(code, m, 5 * m - 5), m - 1, off + more);
        }
        1 => {
            p1(out, bits(code, 0, m), m - 1, off + other);
            p5(&mut out[1..], bits(code, m, 5 * m - 5), m - 1, off + more);
        }
        2 => {
            p2(out, bits(code, 0, 2 * m - 1), m - 1, off + other);
            p4(&mut out[2..], bits(code, 2 * m - 1, 4 * m - 4), m - 1, off + more);
        }
        _ => {
            p3(out, bits(code, 3 * m - 2, 3 * m - 2), m - 1, off);
            p3(&mut out[3..], bits(code, 0, 3 * m - 2), m - 1, off + b_off);
        }
    }
}

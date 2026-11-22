//! AMR sound (Adaptive Multi-Rate), the speech format of older phones' 3GP videos: narrowband
//! (8 kHz, `samr`) and wideband (16 kHz, `sawb`). Both are CELP speech codecs: a filter made
//! from line spectral pairs, driven by a pitch-repeating part and a few pulses. Follows FFmpeg's
//! floating-point decoders (libavcodec/amrnbdec.c, amrwbdec.c and the shared ACELP/CELP helpers
//! in acelp_*.c, celp_*.c, lsp.c; LGPL 2.1 or later) step for step, so the sound matches.
//! Silent-period frames (SID / comfort noise) play as silence.

use alloc::boxed::Box;
use alloc::vec::Vec;

use super::dec::AudioDec;

mod nb;
mod nbtab;
mod wb;
mod wbtab;

/// One AMR stream (mono; FFmpeg's two-channel AMR is not used by phones).
pub struct AmrDec {
    wide: bool,
    nb: Box<nb::Nb>,
    wb: Box<wb::Wb>,
}

impl AmrDec {
    pub fn new(wide: bool) -> AmrDec {
        AmrDec { wide, nb: Box::new(nb::Nb::new()), wb: Box::new(wb::Wb::new()) }
    }
}

fn to_i16(x: f32) -> i16 {
    let v = libm::rintf(x * 32768.0);
    v.clamp(-32768.0, 32767.0) as i16
}

impl AudioDec for AmrDec {
    fn decode(&mut self, data: &[u8]) -> Vec<i16> {
        let mut out = Vec::new();
        let mut p = 0;
        // A packet may hold several frames, each starting with its own header byte.
        while p < data.len() {
            let used = if self.wide {
                let mut buf = [0f32; 320];
                let n = self.wb.frame(&data[p..], &mut buf);
                if n > 0 {
                    for &s in &buf {
                        let v = to_i16(s);
                        out.push(v);
                        out.push(v);
                    }
                }
                n
            } else {
                let mut buf = [0f32; 160];
                let n = self.nb.frame(&data[p..], &mut buf);
                if n > 0 {
                    for &s in &buf {
                        let v = to_i16(s);
                        out.push(v);
                        out.push(v);
                    }
                }
                n
            };
            if used == 0 {
                break;
            }
            p += used;
        }
        out
    }
    fn rate(&self) -> u32 {
        if self.wide {
            16000
        } else {
            8000
        }
    }
    fn reset(&mut self) {
        *self = AmrDec::new(self.wide);
    }
}

// ---- Shared ACELP / CELP helpers (FFmpeg's acelp_filters.c, acelp_vectors.c, celp_filters.c,
// celp_math.c, lsp.c, acelp_pitch_delay.c) ----

/// Spread pulses through a vector, repeated every `pitch_lag` samples (FFmpeg's AMRFixed).
#[derive(Clone, Copy, Default)]
pub(crate) struct Fixed {
    pub n: usize,
    pub x: [i32; 10],
    pub y: [f32; 10],
    pub pitch_lag: i32,
    pub pitch_fac: f32,
}

pub(crate) fn set_fixed_vector(out: &mut [f32], f: &Fixed, scale: f32, size: usize) {
    for i in 0..f.n {
        let mut x = f.x[i];
        let mut y = f.y[i] * scale;
        if f.pitch_lag > 0 {
            loop {
                out[x as usize] += y;
                y *= f.pitch_fac;
                x += f.pitch_lag;
                if x as usize >= size {
                    break;
                }
            }
        }
    }
}

pub(crate) fn clear_fixed_vector(out: &mut [f32], f: &Fixed, size: usize) {
    for i in 0..f.n {
        let mut x = f.x[i];
        if f.pitch_lag > 0 {
            loop {
                out[x as usize] = 0.0;
                x += f.pitch_lag;
                if x as usize >= size {
                    break;
                }
            }
        }
    }
}

/// Sum of products, added up in order in single precision.
pub(crate) fn dot(a: &[f32], b: &[f32], n: usize) -> f32 {
    let mut p = 0.0f32;
    for i in 0..n {
        p += a[i] * b[i];
    }
    p
}

pub(crate) fn weighted_sum(out: &mut [f32], a: &[f32], b: &[f32], wa: f32, wb: f32, n: usize) {
    for i in 0..n {
        out[i] = wa * a[i] + wb * b[i];
    }
}

/// Interpolate `buf` at a fractional delay: output at `out_at`, input centred at `in_at`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn interpolate(buf: &mut [f32], out_at: usize, in_at: isize, filter: &[f32], precision: usize, frac: usize, filter_len: usize, len: usize) {
    for n in 0..len {
        let mut idx = 0;
        let mut v = 0.0f32;
        let mut i = 0;
        while i < filter_len {
            v += buf[(in_at + n as isize + i as isize) as usize] * filter[idx + frac];
            idx += precision;
            i += 1;
            v += buf[(in_at + n as isize - i as isize) as usize] * filter[idx - frac];
        }
        buf[out_at + n] = v;
    }
}

/// All-pole synthesis filter: `out[at..at+len]` from `input`, with `out[at-order..at]` as the
/// filter's memory. Same arithmetic order as FFmpeg's optimised version (4 samples at a time).
pub(crate) fn lp_synthesis(out: &mut [f32], at: usize, c: &[f32], input: &[f32], len: usize, order: usize) {
    let a = c[0];
    let mut b = c[1];
    let mut cc = c[2];
    b -= c[0] * c[0];
    cc -= c[1] * c[0];
    cc -= c[0] * b;
    let mut o0 = out[at - 4];
    let mut o1 = out[at - 3];
    let mut o2 = out[at - 2];
    let mut o3 = out[at - 1];
    let mut n = 0;
    while n + 4 <= len {
        let base = at + n;
        let mut out0 = input[n];
        let mut out1 = input[n + 1];
        let mut out2 = input[n + 2];
        let mut out3 = input[n + 3];
        out0 -= c[2] * o1;
        out1 -= c[2] * o2;
        out2 -= c[2] * o3;
        out0 -= c[1] * o2;
        out1 -= c[1] * o3;
        out0 -= c[0] * o3;
        let val = c[3];
        out0 -= val * o0;
        out1 -= val * o1;
        out2 -= val * o2;
        out3 -= val * o3;
        let mut i = 5;
        while i < order {
            o3 = out[base - i];
            let val = c[i - 1];
            out0 -= val * o3;
            out1 -= val * o0;
            out2 -= val * o1;
            out3 -= val * o2;
            o2 = out[base - i - 1];
            let val = c[i];
            out0 -= val * o2;
            out1 -= val * o3;
            out2 -= val * o0;
            out3 -= val * o1;
            core::mem::swap(&mut o0, &mut o2);
            o1 = o3;
            i += 2;
        }
        let (t0, t1, t2) = (out0, out1, out2);
        out3 -= a * t2;
        out2 -= a * t1;
        out1 -= a * t0;
        out3 -= b * t1;
        out2 -= b * t0;
        out3 -= cc * t0;
        out[base] = out0;
        out[base + 1] = out1;
        out[base + 2] = out2;
        out[base + 3] = out3;
        o0 = out0;
        o1 = out1;
        o2 = out2;
        o3 = out3;
        n += 4;
    }
    while n < len {
        let mut v = input[n];
        for i in 1..=order {
            v -= c[i - 1] * out[at + n - i];
        }
        out[at + n] = v;
        n += 1;
    }
}

/// All-zero filter: `out[n] = in[n] + sum c[i-1] * in[n-i]`, input at `buf[at..]`.
pub(crate) fn zero_synthesis(out: &mut [f32], c: &[f32], buf: &[f32], at: usize, len: usize, order: usize) {
    for n in 0..len {
        let mut v = buf[at + n];
        for i in 1..=order {
            v += c[i - 1] * buf[at + n - i];
        }
        out[n] = v;
    }
}

pub(crate) fn circ_add(out: &mut [f32], input: &[f32], lagged: &[f32], lag: usize, fac: f32, n: usize) {
    for k in 0..lag.min(n) {
        out[k] = input[k] + fac * lagged[n + k - lag];
    }
    for k in lag..n {
        out[k] = input[k] + fac * lagged[k - lag];
    }
}

pub(crate) fn scale_to_energy(v: &mut [f32], sum_of_squares: f32, n: usize) {
    let mut s = dot(v, v, n);
    if s != 0.0 {
        s = libm::sqrt((sum_of_squares / s) as f64) as f32;
    }
    for x in v.iter_mut().take(n) {
        *x *= s;
    }
}

pub(crate) fn adaptive_gain_control(v: &mut [f32], speech: f32, n: usize, alpha: f32, mem: &mut f32) {
    let post = dot(v, v, n);
    let mut g = 1.0f32;
    if post != 0.0 {
        g = libm::sqrt((speech / post) as f64) as f32;
    }
    g = (g as f64 * (1.0 - alpha as f64)) as f32;
    let mut m = *mem;
    for x in v.iter_mut().take(n) {
        m = alpha * m + g;
        *x *= m;
    }
    *mem = m;
}

pub(crate) fn tilt_compensation(mem: &mut f32, tilt: f32, s: &mut [f32], n: usize) {
    let new_mem = s[n - 1];
    for i in (1..n).rev() {
        s[i] -= tilt * s[i - 1];
    }
    s[0] -= tilt * *mem;
    *mem = new_mem;
}

/// Second-order filter (FFmpeg's acelp_apply_order_2_transfer_function), in place.
pub(crate) fn order2(v: &mut [f32], zeros: &[f32; 2], poles: &[f32; 2], gain: f32, mem: &mut [f32; 2], n: usize) {
    for x in v.iter_mut().take(n) {
        let tmp = gain * *x - poles[0] * mem[0] - poles[1] * mem[1];
        *x = tmp + zeros[0] * mem[0] + zeros[1] * mem[1];
        mem[1] = mem[0];
        mem[0] = tmp;
    }
}

pub(crate) fn set_min_dist(lsf: &mut [f32], min: f64, n: usize) {
    let mut prev = 0.0f32;
    for x in lsf.iter_mut().take(n) {
        let v = (*x as f64).max(prev as f64 + min);
        *x = v as f32;
        prev = *x;
    }
}

pub(crate) fn lsf2lspd(lsp: &mut [f64], lsf: &[f32], n: usize) {
    for i in 0..n {
        lsp[i] = libm::cos(2.0 * core::f64::consts::PI * lsf[i] as f64);
    }
}

/// Polynomial from every other LSP (starting at `lsp[start]`).
fn lsp2polyf(lsp: &[f64], start: usize, f: &mut [f64], half: usize) {
    f[0] = 1.0;
    f[1] = -2.0 * lsp[start];
    for i in 2..=half {
        let val = -2.0 * lsp[start + 2 * i - 2];
        f[i] = val * f[i - 1] + 2.0 * f[i - 2];
        let mut j = i - 1;
        while j > 1 {
            f[j] += f[j - 1] * val + f[j - 2];
            j -= 1;
        }
        f[1] += val;
    }
}

pub(crate) fn lspd2lpc(lsp: &[f64], lpc: &mut [f32], half: usize) {
    let mut pa = [0f64; 11];
    let mut qa = [0f64; 11];
    lsp2polyf(lsp, 0, &mut pa, half);
    lsp2polyf(lsp, 1, &mut qa, half);
    let mut h = half;
    while h > 0 {
        h -= 1;
        let paf = pa[h + 1] + pa[h];
        let qaf = qa[h + 1] - qa[h];
        lpc[h] = (0.5 * (paf + qaf)) as f32;
        lpc[2 * half - 1 - h] = (0.5 * (paf - qaf)) as f32;
    }
}

/// ISP to LPC for AMR-WB (an order-16 or order-20 filter).
pub(crate) fn amrwb_lsp2lpc(lsp: &[f64], lp: &mut [f32], order: usize) {
    let half = order >> 1;
    let mut pa = [0f64; 11];
    let mut buf = [0f64; 11];
    lsp2polyf(lsp, 0, &mut pa, half);
    {
        let (_, qa) = buf.split_at_mut(1);
        lsp2polyf(lsp, 1, qa, half - 1);
    }
    // qa[k] = buf[k + 1], qa[-1] = buf[0] = 0.
    let last = lsp[order - 1];
    let mut j = order - 1;
    for i in 1..half {
        let paf = pa[i] * (1.0 + last);
        let qaf = (buf[i + 1] - buf[i - 1]) * (1.0 - last);
        lp[i - 1] = ((paf + qaf) * 0.5) as f32;
        lp[j - 1] = ((paf - qaf) * 0.5) as f32;
        j -= 1;
    }
    lp[half - 1] = ((1.0 + last) * pa[half] * 0.5) as f32;
    lp[order - 1] = last as f32;
}

fn exp10(x: f64) -> f64 {
    libm::exp2(core::f64::consts::LOG2_10 * x)
}

/// The fixed (pulse) gain from its correction factor and the predicted energy; updates the
/// prediction history.
pub(crate) fn amr_set_fixed_gain(factor: f32, mean_energy: f32, pred_err: &mut [f32; 4], energy_mean: f32, pred: &[f32]) -> f32 {
    let sp = dot(pred, pred_err, 4);
    let e = if mean_energy != 0.0 { mean_energy } else { 1.0 };
    let val = factor as f64 * exp10(0.05 * (sp + energy_mean) as f64) / libm::sqrtf(e) as f64;
    pred_err.copy_within(1..4, 0);
    pred_err[3] = (20.0 * libm::log10f(factor) as f64) as f32;
    val as f32
}

/// Pitch lag from its index (1/3 resolution), FFmpeg's ff_decode_pitch_lag.
pub(crate) fn decode_pitch_lag(pitch_index: i32, prev_lag_int: i32, subframe: usize, third_as_first: bool, resolution: i32) -> (i32, i32) {
    const MIN: i32 = 20;
    const MAX: i32 = 143;
    let mut pi = pitch_index;
    if subframe == 0 || (subframe == 2 && third_as_first) {
        if pi < 197 {
            pi += 59;
        } else {
            pi = 3 * pi - 335;
        }
    } else if resolution == 4 {
        let min = (prev_lag_int - 5).clamp(MIN, MAX - 9);
        if pi < 4 {
            pi = 3 * (pi + min) + 1;
        } else if pi < 12 {
            pi += 3 * min + 7;
        } else {
            pi = 3 * (pi + min - 6) + 1;
        }
    } else {
        pi -= 1;
        if resolution == 5 {
            pi += 3 * (prev_lag_int - 10).clamp(MIN, MAX - 19);
        } else {
            pi += 3 * (prev_lag_int - 5).clamp(MIN, MAX - 9);
        }
    }
    let lag_int = (pi * 10923) >> 15;
    (lag_int, pi - 3 * lag_int - 1)
}

/// Spread a frame's bits into its parameters (FFmpeg's ff_amr_bit_reorder). `order` holds
/// (bit count, field, bit positions...) groups ending with 0.
pub(crate) fn bit_reorder<T: Copy + Into<u32>>(out: &mut [u16], data: &[u8], order: &[T]) {
    out.fill(0);
    let mut k = 0;
    while k < order.len() {
        let n = order[k].into() as usize;
        if n == 0 {
            break;
        }
        let field = order[k + 1].into() as usize;
        let mut v = 0u32;
        for b in 0..n {
            let bit = order[k + 2 + b].into() as usize;
            v = (v << 1) | ((data.get(bit >> 3).copied().unwrap_or(0) >> (bit & 7)) & 1) as u32;
        }
        out[field] = v as u16;
        k += 2 + n;
    }
}

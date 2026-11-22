//! CELT, the music half of Opus (RFC 6716 section 4.3), ported from FFmpeg's opus/celt.c,
//! dec_celt.c and pvq.c (LGPL 2.1 or later; Andrew D'Addesio, Mozilla, Rostislav Pehlivanov).

use alloc::vec;
use alloc::vec::Vec;
use super::mdct::Imdct;
use super::rc::{ilog, Rc};
use super::tab::*;
use core::f32::consts::{FRAC_1_SQRT_2, PI, SQRT_2};

const MAX_BANDS: usize = 21;
const OVERLAP: usize = 120;
const ENERGY_SILENCE: f32 = -28.0;
const SPREAD_NONE: i32 = 0;
const SPREAD_NORMAL: i32 = 2;
const SPREAD_AGGRESSIVE: i32 = 3;
const NORM: usize = 8 * 100;

fn bands(i: usize) -> usize {
    CELT_FREQ_BANDS[i] as usize
}
fn range(i: usize) -> usize {
    CELT_FREQ_RANGE[i] as usize
}
fn window() -> &'static [f32] {
    &CELT_WINDOW_PADDED[8..128]
}

#[derive(Clone)]
struct Block {
    energy: [f32; MAX_BANDS],
    prev_energy: [[f32; MAX_BANDS]; 2],
    collapse_masks: [u8; MAX_BANDS],
    buf: Vec<f32>,
    pf_period_new: usize,
    pf_gains_new: [f32; 3],
    pf_period: usize,
    pf_gains: [f32; 3],
    pf_period_old: usize,
    pf_gains_old: [f32; 3],
    emph_coeff: f32,
}

impl Block {
    fn new() -> Block {
        Block {
            energy: [0.0; MAX_BANDS],
            prev_energy: [[ENERGY_SILENCE; MAX_BANDS]; 2],
            collapse_masks: [0; MAX_BANDS],
            buf: vec![0.0; 2048],
            pf_period_new: 0,
            pf_gains_new: [0.0; 3],
            pf_period: 0,
            pf_gains: [0.0; 3],
            pf_period_old: 0,
            pf_gains_old: [0.0; 3],
            emph_coeff: 0.0,
        }
    }
}

/// Where a vector lives: the coefficients, the folding (norm) buffer or the scratch buffer.
#[derive(Clone, Copy)]
enum Loc {
    N(usize),
    S(usize),
}

pub struct Celt {
    block: [Block; 2],
    imdct: [Imdct; 4],
    output_channels: usize,
    channels: usize,
    size: usize,
    start_band: usize,
    end_band: usize,
    coded_bands: usize,
    transient: bool,
    blocks: usize,
    blocksize: usize,
    anticollapse_needed: i32,
    intensity_stereo: usize,
    dual_stereo: bool,
    flushed: bool,
    seed: u32,
    spread: i32,
    framebits: i32,
    remaining: i32,
    remaining2: i32,
    alloc_trim: i32,
    caps: [i32; MAX_BANDS],
    fine_bits: [i32; MAX_BANDS],
    fine_priority: [i32; MAX_BANDS],
    pulses: [i32; MAX_BANDS],
    tf_change: [i32; MAX_BANDS],
    // Working buffers: coefficients of both channels (second at +960), folding vectors
    // (norm2 at +NORM), scratch.
    coeffs: Vec<f32>,
    norm: Vec<f32>,
    scratch: Vec<f32>,
    qcoeff: [i32; 176],
}

#[inline]
fn round_mul16(a: i32, b: i32) -> i32 {
    (a * b + 16384) >> 15
}

fn celt_cos(x: i32) -> i32 {
    let x = ((x * x + 4096) >> 13) as i16 as i32;
    let x = ((32767 - x) + round_mul16(x, -7651 + round_mul16(x, 8277 + round_mul16(-626, x)))) as i16 as i32;
    (x + 1) as i16 as i32
}

fn celt_log2tan(isin: i32, icos: i32) -> i32 {
    let lc = ilog(icos as u32) as i32;
    let ls = ilog(isin as u32) as i32;
    let icos = icos << (15 - lc);
    let isin = isin << (15 - ls);
    (ls << 11) - (lc << 11) + round_mul16(isin, round_mul16(isin, -2597) + 7932) - round_mul16(icos, round_mul16(icos, -2597) + 7932)
}

fn bits2pulses(cache: &[u8], bits: i32) -> usize {
    let mut low = 0usize;
    let mut high = cache[0] as usize;
    let bits = bits - 1;
    for _ in 0..6 {
        let center = (low + high + 1) >> 1;
        if cache[center] as i32 >= bits {
            high = center;
        } else {
            low = center;
        }
    }
    let lowv = if low == 0 { -1 } else { cache[low] as i32 };
    if bits - lowv <= cache[high] as i32 - bits {
        low
    } else {
        high
    }
}

fn pulses2bits(cache: &[u8], pulses: usize) -> i32 {
    if pulses == 0 {
        0
    } else {
        cache[pulses] as i32 + 1
    }
}

fn pvq_u(n: usize, k: usize) -> u32 {
    CELT_PVQ_U[CELT_PVQ_U_ROW[n.min(k)] + n.max(k)]
}
fn row(n: usize, k: usize) -> u32 {
    CELT_PVQ_U[CELT_PVQ_U_ROW[n] + k]
}

/// Pulse vector from its index (libopus's decoder via FFmpeg); returns the squared norm.
fn cwrsi(mut n: usize, mut k: usize, mut i: u32, y: &mut [i32]) -> f32 {
    let mut norm = 0u64;
    let mut yi = 0;
    while n > 2 {
        if k >= n {
            let p = row(n, k + 1);
            let s: i32 = if i >= p { -1 } else { 0 };
            if s != 0 {
                i -= p;
            }
            let k0 = k;
            let q = row(n, n);
            let mut p;
            if q > i {
                k = n;
                loop {
                    k -= 1;
                    p = row(k, n);
                    if p <= i {
                        break;
                    }
                }
            } else {
                p = row(n, k);
                while p > i {
                    k -= 1;
                    p = row(n, k);
                }
            }
            i -= p;
            let val = ((k0 - k) as i32 + s) ^ s;
            norm += (val * val) as u64;
            y[yi] = val;
        } else {
            let p = row(k, n);
            let q = row(k + 1, n);
            if p <= i && i < q {
                i -= p;
                y[yi] = 0;
            } else {
                let s: i32 = if i >= q { -1 } else { 0 };
                if s != 0 {
                    i -= q;
                }
                let k0 = k;
                let mut p;
                loop {
                    k -= 1;
                    p = row(k, n);
                    if p <= i {
                        break;
                    }
                }
                i -= p;
                let val = ((k0 - k) as i32 + s) ^ s;
                norm += (val * val) as u64;
                y[yi] = val;
            }
        }
        yi += 1;
        n -= 1;
    }
    // N = 2
    let p = 2 * k as u32 + 1;
    let s: i32 = if i >= p { -1 } else { 0 };
    if s != 0 {
        i -= p;
    }
    let k0 = k;
    k = ((i + 1) / 2) as usize;
    if k != 0 {
        i -= 2 * k as u32 - 1;
    }
    let val = ((k0 - k) as i32 + s) ^ s;
    norm += (val * val) as u64;
    y[yi] = val;
    // N = 1
    let s = -(i as i32);
    let val = (k as i32 + s) ^ s;
    norm += (val * val) as u64;
    y[yi + 1] = val;
    norm as f32
}

fn exp_rotation_impl(x: &mut [f32], len: usize, stride: usize, c: f32, s: f32) {
    for i in 0..len - stride {
        let (x1, x2) = (x[i], x[i + stride]);
        x[i + stride] = c * x2 + s * x1;
        x[i] = c * x1 - s * x2;
    }
    if len > 2 * stride {
        for i in (0..len - 2 * stride).rev() {
            let (x1, x2) = (x[i], x[i + stride]);
            x[i + stride] = c * x2 + s * x1;
            x[i] = c * x1 - s * x2;
        }
    }
}

fn exp_rotation(x: &mut [f32], len: usize, stride: usize, k: usize, spread: i32) {
    if 2 * k >= len || spread == SPREAD_NONE {
        return;
    }
    let gain = len as f32 / (len + (20 - 5 * spread) as usize * k) as f32;
    let theta = PI * gain * gain / 4.0;
    let (c, s) = (libm::cosf(theta), libm::sinf(theta));
    let mut stride2 = 0;
    if len >= stride << 3 {
        stride2 = 1;
        while (stride2 * stride2 + stride2) * stride + (stride >> 2) < len {
            stride2 += 1;
        }
    }
    let len = len / stride;
    for i in 0..stride {
        let v = &mut x[i * len..(i + 1) * len];
        if stride2 != 0 {
            exp_rotation_impl(v, len, stride2, s, c);
        }
        exp_rotation_impl(v, len, 1, c, s);
    }
}

fn collapse_mask(iy: &[i32], n: usize, b: usize) -> u32 {
    if b <= 1 {
        return 1;
    }
    let n0 = n / b;
    let mut m = 0;
    for i in 0..b {
        for j in 0..n0 {
            if iy[i * n0 + j] != 0 {
                m |= 1 << i;
            }
        }
    }
    m
}

fn haar1(x: &mut [f32], n0: usize, stride: usize) {
    let n0 = n0 >> 1;
    for i in 0..stride {
        for j in 0..n0 {
            let x0 = x[stride * 2 * j + i];
            let x1 = x[stride * (2 * j + 1) + i];
            x[stride * 2 * j + i] = (x0 + x1) * FRAC_1_SQRT_2;
            x[stride * (2 * j + 1) + i] = (x0 - x1) * FRAC_1_SQRT_2;
        }
    }
}

fn hadamard_order(stride: usize, hadamard: bool) -> &'static [u8] {
    &CELT_HADAMARD_ORDER[if hadamard { stride - 2 } else { 30 }..]
}

fn interleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let order = hadamard_order(stride, hadamard);
    let mut tmp = [0f32; 176];
    for i in 0..stride {
        for j in 0..n0 {
            tmp[j * stride + i] = x[order[i] as usize * n0 + j];
        }
    }
    x[..n0 * stride].copy_from_slice(&tmp[..n0 * stride]);
}

fn deinterleave_hadamard(x: &mut [f32], n0: usize, stride: usize, hadamard: bool) {
    let order = hadamard_order(stride, hadamard);
    let mut tmp = [0f32; 176];
    for i in 0..stride {
        for j in 0..n0 {
            tmp[order[i] as usize * n0 + j] = x[j * stride + i];
        }
    }
    x[..n0 * stride].copy_from_slice(&tmp[..n0 * stride]);
}

fn compute_qn(n: i32, b: i32, offset: i32, pulse_cap: i32, stereo: bool) -> i32 {
    let mut n2 = 2 * n - 1;
    if stereo && n == 2 {
        n2 -= 1;
    }
    let qb = (b - pulse_cap - (4 << 3)).min((b + n2 * offset) / n2).min(8 << 3);
    if qb < (1 << 3 >> 1) {
        1
    } else {
        ((CELT_QN_EXP2[(qb & 7) as usize] as i32 >> (14 - (qb >> 3))) + 1) >> 1 << 1
    }
}

fn renormalize(x: &mut [f32], gain: f32) {
    let mut g = 1e-15f32;
    for v in x.iter() {
        g += v * v;
    }
    let g = gain / libm::sqrtf(g);
    for v in x.iter_mut() {
        *v *= g;
    }
}

impl Celt {
    pub fn new(output_channels: usize) -> Celt {
        let mut c = Celt {
            block: [Block::new(), Block::new()],
            imdct: [Imdct::new(120), Imdct::new(240), Imdct::new(480), Imdct::new(960)],
            output_channels,
            channels: output_channels,
            size: 0,
            start_band: 0,
            end_band: 0,
            coded_bands: 0,
            transient: false,
            blocks: 1,
            blocksize: 0,
            anticollapse_needed: 0,
            intensity_stereo: 0,
            dual_stereo: false,
            flushed: false,
            seed: 0,
            spread: SPREAD_NORMAL,
            framebits: 0,
            remaining: 0,
            remaining2: 0,
            alloc_trim: 5,
            caps: [0; MAX_BANDS],
            fine_bits: [0; MAX_BANDS],
            fine_priority: [0; MAX_BANDS],
            pulses: [0; MAX_BANDS],
            tf_change: [0; MAX_BANDS],
            coeffs: vec![0.0; 2 * 960],
            norm: vec![0.0; 2 * NORM],
            scratch: vec![0.0; 8 * 22],
            qcoeff: [0; 176],
        };
        c.flush();
        c
    }

    pub fn flush(&mut self) {
        if self.flushed {
            return;
        }
        for b in self.block.iter_mut() {
            b.prev_energy = [[ENERGY_SILENCE; MAX_BANDS]; 2];
            b.energy = [0.0; MAX_BANDS];
            b.buf.iter_mut().for_each(|v| *v = 0.0);
            b.pf_gains = [0.0; 3];
            b.pf_gains_old = [0.0; 3];
            b.pf_gains_new = [0.0; 3];
            b.emph_coeff = 0.0;
        }
        self.seed = 0;
        self.flushed = true;
    }

    fn rng(&mut self) -> u32 {
        self.seed = self.seed.wrapping_mul(1664525).wrapping_add(1013904223);
        self.seed
    }

    fn lb(&mut self, l: Loc) -> &mut [f32] {
        match l {
            Loc::N(o) => &mut self.norm[o..],
            Loc::S(o) => &mut self.scratch[o..],
        }
    }

    fn coarse_energy(&mut self, rc: &mut Rc) {
        let mut prev = [0f32; 2];
        let mut alpha = CELT_ALPHA_COEF[self.size];
        let mut beta = CELT_BETA_COEF[self.size];
        let mut model = (self.size * 2) * 42;
        if rc.tell() + 3 <= self.framebits && rc.log(3) != 0 {
            alpha = 0.0;
            beta = 1.0 - 4915.0 / 32768.0;
            model = (self.size * 2 + 1) * 42;
        }
        for i in 0..MAX_BANDS {
            for j in 0..self.channels {
                if i < self.start_band || i >= self.end_band {
                    self.block[j].energy[i] = 0.0;
                    continue;
                }
                let available = self.framebits - rc.tell();
                let value = if available >= 15 {
                    let k = i.min(20) << 1;
                    rc.laplace((CELT_COARSE_ENERGY_DIST[model + k] as u32) << 7, (CELT_COARSE_ENERGY_DIST[model + k + 1] as i32) << 6) as f32
                } else if available >= 2 {
                    let x = rc.cdf(&CELT_MODEL_TAPSET) as i32;
                    ((x >> 1) ^ -(x & 1)) as f32
                } else if available >= 1 {
                    -(rc.log(1) as f32)
                } else {
                    -1.0
                };
                let e = &mut self.block[j].energy[i];
                *e = e.max(-9.0) * alpha + prev[j] + value;
                prev[j] += beta * value;
            }
        }
    }

    fn fine_energy(&mut self, rc: &mut Rc) {
        for i in self.start_band..self.end_band {
            let fb = self.fine_bits[i];
            if fb == 0 {
                continue;
            }
            for j in 0..self.channels {
                let q2 = rc.raw(fb as u32);
                let offset = (q2 as f32 + 0.5) * (1 << (14 - fb)) as f32 / 16384.0 - 0.5;
                self.block[j].energy[i] += offset;
            }
        }
    }

    fn final_energy(&mut self, rc: &mut Rc) {
        let mut bits_left = self.framebits - rc.tell();
        for priority in 0..2 {
            let mut i = self.start_band;
            while i < self.end_band && bits_left >= self.channels as i32 {
                if self.fine_priority[i] != priority || self.fine_bits[i] >= 8 {
                    i += 1;
                    continue;
                }
                for j in 0..self.channels {
                    let q2 = rc.raw(1);
                    let offset = (q2 as f32 - 0.5) * (1 << (14 - self.fine_bits[i] - 1)) as f32 / 16384.0;
                    self.block[j].energy[i] += offset;
                    bits_left -= 1;
                }
                i += 1;
            }
        }
    }

    fn tf_changes(&mut self, rc: &mut Rc) {
        let tr = self.transient as usize;
        let mut bits = if self.transient { 2 } else { 4 };
        let mut consumed = rc.tell();
        let tf_select_bit = (self.size != 0 && consumed + bits + 1 <= self.framebits) as i32;
        let (mut diff, mut changed) = (0i32, 0i32);
        for i in self.start_band..self.end_band {
            if consumed + bits + tf_select_bit <= self.framebits {
                diff ^= rc.log(bits as u32) as i32;
                consumed = rc.tell();
                changed |= diff;
            }
            self.tf_change[i] = diff;
            bits = if self.transient { 4 } else { 5 };
        }
        let sel = |s: usize, c: i32| CELT_TF_SELECT[((self.size * 2 + tr) * 2 + s) * 2 + c as usize];
        let mut tf_select = 0;
        if tf_select_bit != 0 && sel(0, changed) != sel(1, changed) {
            tf_select = rc.log(1) as usize;
        }
        for i in self.start_band..self.end_band {
            self.tf_change[i] = sel(tf_select, self.tf_change[i]) as i32;
        }
    }

    fn normc(&self, bits: i32) -> i32 {
        (bits << (self.channels - 1) << self.size) >> 2
    }

    fn bitalloc(&mut self, rc: &mut Rc) {
        let (sb, eb, ch) = (self.start_band, self.end_band, self.channels as i32);
        let mut skip_startband = sb;
        let mut skip_bit = 0;
        let mut intensitystereo_bit = 0;
        let mut dualstereo_bit = 0;
        let mut dynalloc = 6;
        let mut extrabits = 0;
        let mut boost = [0i32; MAX_BANDS];
        let mut trim_offset = [0i32; MAX_BANDS];
        let mut threshold = [0i32; MAX_BANDS];
        let mut bits1 = [0i32; MAX_BANDS];
        let mut bits2 = [0i32; MAX_BANDS];

        self.spread = if rc.tell() + 4 <= self.framebits { rc.cdf(&CELT_MODEL_SPREAD) as i32 } else { SPREAD_NORMAL };
        for i in 0..MAX_BANDS {
            let cap = CELT_STATIC_CAPS[(self.size * 2 + self.channels - 1) * 21 + i] as i32;
            self.caps[i] = self.normc((cap + 64) * range(i) as i32);
        }
        let mut tbits_8ths = self.framebits << 3;
        for i in sb..eb {
            let mut quanta = (range(i) as i32) << (ch - 1) << self.size;
            let mut b_dynalloc = dynalloc;
            quanta = (quanta << 3).min((6 << 3).max(quanta));
            while rc.tell_frac() + (b_dynalloc << 3) < tbits_8ths && boost[i] < self.caps[i] {
                if rc.log(b_dynalloc as u32) == 0 {
                    break;
                }
                boost[i] += quanta;
                tbits_8ths -= quanta;
                b_dynalloc = 1;
            }
            if boost[i] != 0 {
                dynalloc = (dynalloc - 1).max(2);
            }
        }
        self.alloc_trim = 5;
        if rc.tell_frac() + (6 << 3) <= tbits_8ths {
            self.alloc_trim = rc.cdf(&CELT_MODEL_ALLOC_TRIM) as i32;
        }
        tbits_8ths = (self.framebits << 3) - rc.tell_frac() - 1;
        self.anticollapse_needed = 0;
        if self.transient && self.size >= 2 && tbits_8ths >= ((self.size as i32 + 2) << 3) {
            self.anticollapse_needed = 1 << 3;
        }
        tbits_8ths -= self.anticollapse_needed;
        if tbits_8ths >= 1 << 3 {
            skip_bit = 1 << 3;
        }
        tbits_8ths -= skip_bit;
        if ch == 2 {
            intensitystereo_bit = CELT_LOG2_FRAC[eb - sb] as i32;
            if intensitystereo_bit <= tbits_8ths {
                tbits_8ths -= intensitystereo_bit;
                if tbits_8ths >= 1 << 3 {
                    dualstereo_bit = 1 << 3;
                    tbits_8ths -= 1 << 3;
                }
            } else {
                intensitystereo_bit = 0;
            }
        }
        for i in sb..eb {
            let trim = self.alloc_trim - 5 - self.size as i32;
            let band = range(i) as i32 * (eb - i - 1) as i32;
            let duration = self.size as i32 + 3;
            let scale = duration + ch - 1;
            threshold[i] = ((3 * range(i) as i32) << duration >> 4).max(ch << 3);
            trim_offset[i] = trim * (band << scale) >> 6;
            if range(i) << self.size == 1 {
                trim_offset[i] -= ch << 3;
            }
        }
        let mut low = 1i32;
        let mut high = 10i32;
        while low <= high {
            let center = (low + high) >> 1;
            let (mut done, mut total) = (false, 0);
            for i in (sb..eb).rev() {
                let mut bandbits = self.normc(range(i) as i32 * CELT_STATIC_ALLOC[center as usize * 21 + i] as i32);
                if bandbits != 0 {
                    bandbits = (bandbits + trim_offset[i]).max(0);
                }
                bandbits += boost[i];
                if bandbits >= threshold[i] || done {
                    done = true;
                    total += bandbits.min(self.caps[i]);
                } else if bandbits >= ch << 3 {
                    total += ch << 3;
                }
            }
            if total > tbits_8ths {
                high = center - 1;
            } else {
                low = center + 1;
            }
        }
        high = low;
        low -= 1;
        for i in sb..eb {
            bits1[i] = self.normc(range(i) as i32 * CELT_STATIC_ALLOC[low as usize * 21 + i] as i32);
            bits2[i] = if high >= 11 { self.caps[i] } else { self.normc(range(i) as i32 * CELT_STATIC_ALLOC[high as usize * 21 + i] as i32) };
            if bits1[i] != 0 {
                bits1[i] = (bits1[i] + trim_offset[i]).max(0);
            }
            if bits2[i] != 0 {
                bits2[i] = (bits2[i] + trim_offset[i]).max(0);
            }
            if low != 0 {
                bits1[i] += boost[i];
            }
            bits2[i] += boost[i];
            if boost[i] != 0 {
                skip_startband = i;
            }
            bits2[i] = (bits2[i] - bits1[i]).max(0);
        }
        let (mut low, mut high) = (0i32, 1i32 << 6);
        for _ in 0..6 {
            let center = (low + high) >> 1;
            let (mut done, mut total) = (false, 0);
            for j in (sb..eb).rev() {
                let bandbits = bits1[j] + (center * bits2[j] >> 6);
                if bandbits >= threshold[j] || done {
                    done = true;
                    total += bandbits.min(self.caps[j]);
                } else if bandbits >= ch << 3 {
                    total += ch << 3;
                }
            }
            if total > tbits_8ths {
                high = center;
            } else {
                low = center;
            }
        }
        let (mut done, mut total) = (false, 0);
        for i in (sb..eb).rev() {
            let mut bandbits = bits1[i] + (low * bits2[i] >> 6);
            if bandbits >= threshold[i] || done {
                done = true;
            } else {
                bandbits = if bandbits >= ch << 3 { ch << 3 } else { 0 };
            }
            bandbits = bandbits.min(self.caps[i]);
            self.pulses[i] = bandbits;
            total += bandbits;
        }
        // Band skipping.
        self.coded_bands = eb;
        loop {
            let j = self.coded_bands - 1;
            if j == skip_startband {
                tbits_8ths += skip_bit;
                break;
            }
            let mut remaining = tbits_8ths - total;
            let span = (bands(j + 1) - bands(sb)) as i32;
            let bandbits = remaining / span;
            remaining -= bandbits * span;
            let mut allocation = self.pulses[j] + bandbits * range(j) as i32;
            allocation += (remaining - (bands(j) - bands(sb)) as i32).max(0);
            if allocation >= threshold[j].max((ch + 1) << 3) {
                if rc.log(1) != 0 {
                    break;
                }
                total += 1 << 3;
                allocation -= 1 << 3;
            }
            total -= self.pulses[j];
            if intensitystereo_bit != 0 {
                total -= intensitystereo_bit;
                intensitystereo_bit = CELT_LOG2_FRAC[j - sb] as i32;
                total += intensitystereo_bit;
            }
            self.pulses[j] = if allocation >= ch << 3 { ch << 3 } else { 0 };
            total += self.pulses[j];
            self.coded_bands -= 1;
        }
        self.intensity_stereo = 0;
        self.dual_stereo = false;
        if intensitystereo_bit != 0 {
            self.intensity_stereo = sb + rc.uint((self.coded_bands + 1 - sb) as u32) as usize;
        }
        if self.intensity_stereo <= sb {
            tbits_8ths += dualstereo_bit;
        } else if dualstereo_bit != 0 {
            self.dual_stereo = rc.log(1) != 0;
        }
        let mut remaining = tbits_8ths - total;
        let span = (bands(self.coded_bands) - bands(sb)) as i32;
        let bandbits = remaining / span;
        remaining -= bandbits * span;
        for i in sb..self.coded_bands {
            let bits = remaining.min(range(i) as i32);
            self.pulses[i] += bits + bandbits * range(i) as i32;
            remaining -= bits;
        }
        let mut i = sb;
        while i < self.coded_bands {
            let n = (range(i) << self.size) as i32;
            let prev_extra = extrabits;
            self.pulses[i] += extrabits;
            if n > 1 {
                extrabits = (self.pulses[i] - self.caps[i]).max(0);
                self.pulses[i] -= extrabits;
                let dof = n * ch + (ch == 2 && n > 2 && !self.dual_stereo && i < self.intensity_stereo) as i32;
                let temp = dof * (CELT_LOG_FREQ_RANGE[i] as i32 + ((self.size as i32) << 3));
                let mut offset = (temp >> 1) - dof * 21;
                if n == 2 {
                    offset += dof << 1;
                }
                if self.pulses[i] + offset < 2 * (dof << 3) {
                    offset += temp >> 2;
                } else if self.pulses[i] + offset < 3 * (dof << 3) {
                    offset += temp >> 3;
                }
                let fine_bits = (self.pulses[i] + offset + (dof << 2)) / (dof << 3);
                let max_bits = ((self.pulses[i] >> 3) >> (ch - 1)).min(8).max(0);
                self.fine_bits[i] = fine_bits.clamp(0, max_bits);
                self.fine_priority[i] = (self.fine_bits[i] * (dof << 3) >= self.pulses[i] + offset) as i32;
                self.pulses[i] -= self.fine_bits[i] << (ch - 1) << 3;
            } else {
                extrabits = (self.pulses[i] - (ch << 3)).max(0);
                self.pulses[i] -= extrabits;
                self.fine_bits[i] = 0;
                self.fine_priority[i] = 1;
            }
            if extrabits > 0 {
                let mut fineextra = (extrabits >> (ch + 2)).min(8 - self.fine_bits[i]);
                self.fine_bits[i] += fineextra;
                fineextra <<= ch + 2;
                self.fine_priority[i] = (fineextra >= extrabits - prev_extra) as i32;
                extrabits -= fineextra;
            }
            i += 1;
        }
        self.remaining = extrabits;
        while i < eb {
            self.fine_bits[i] = self.pulses[i] >> (ch - 1) >> 3;
            self.pulses[i] = 0;
            self.fine_priority[i] = (self.fine_bits[i] < 1) as i32;
            i += 1;
        }
    }

    fn quant_bands(&mut self, rc: &mut Rc) {
        let totalbits = (self.framebits << 3) - self.anticollapse_needed;
        let mut update_lowband = true;
        let mut lowband_offset = 0usize;
        let sb = self.start_band;
        for i in sb..self.end_band {
            let mut cm = [(1u32 << self.blocks) - 1; 2];
            let band_offset = bands(i) << self.size;
            let band_size = range(i) << self.size;
            let x = band_offset;
            let y = if self.channels == 2 { Some(960 + band_offset) } else { None };
            let consumed = rc.tell_frac();
            let mut effective_lowband: Option<usize> = None;
            let mut b = 0;
            if i != sb {
                self.remaining -= consumed;
            }
            self.remaining2 = totalbits - consumed - 1;
            if i + 1 <= self.coded_bands {
                let curr_balance = self.remaining / (self.coded_bands - i).min(3) as i32;
                b = (self.remaining2 + 1).min(self.pulses[i] + curr_balance).clamp(0, 16383);
            }
            if (bands(i) as i32 - range(i) as i32 >= bands(sb) as i32 || i == sb + 1) && (update_lowband || lowband_offset == 0) {
                lowband_offset = i;
            }
            if i == sb + 1 {
                let count = (range(i) - range(i - 1)) << self.size;
                self.norm.copy_within(band_offset - count..band_offset, band_offset);
                if self.channels == 2 {
                    self.norm.copy_within(NORM + band_offset - count..NORM + band_offset, NORM + band_offset);
                }
            }
            if lowband_offset != 0 && (self.spread != SPREAD_AGGRESSIVE || self.blocks > 1 || self.tf_change[i] < 0) {
                let eff = (bands(sb) as i32).max(bands(lowband_offset) as i32 - range(i) as i32) as usize;
                effective_lowband = Some(eff);
                let mut foldstart = lowband_offset;
                loop {
                    foldstart -= 1;
                    if bands(foldstart) <= eff {
                        break;
                    }
                }
                let mut foldend = lowband_offset - 1;
                loop {
                    foldend += 1;
                    if !(foldend < i && bands(foldend) < eff + range(i)) {
                        break;
                    }
                }
                cm = [0, 0];
                for j in foldstart..foldend {
                    cm[0] |= self.block[0].collapse_masks[j] as u32;
                    cm[1] |= self.block[self.channels - 1].collapse_masks[j] as u32;
                }
            }
            if self.dual_stereo && i == self.intensity_stereo {
                self.dual_stereo = false;
                for j in bands(sb) << self.size..band_offset {
                    self.norm[j] = (self.norm[j] + self.norm[NORM + j]) / 2.0;
                }
            }
            let loc1 = effective_lowband.map(|e| Loc::N(e << self.size));
            let loc2 = effective_lowband.map(|e| Loc::N(NORM + (e << self.size)));
            if self.dual_stereo {
                cm[0] = self.quant_band(rc, i, x, None, band_size, b >> 1, self.blocks, loc1, self.size as i32, Some(band_offset), 0, 1.0, true, cm[0]);
                cm[1] = self.quant_band(rc, i, y.unwrap_or(x), None, band_size, b >> 1, self.blocks, loc2, self.size as i32, Some(NORM + band_offset), 0, 1.0, true, cm[1]);
            } else {
                cm[0] = self.quant_band(rc, i, x, y, band_size, b, self.blocks, loc1, self.size as i32, Some(band_offset), 0, 1.0, true, cm[0] | cm[1]);
                cm[1] = cm[0];
            }
            self.block[0].collapse_masks[i] = cm[0] as u8;
            self.block[self.channels - 1].collapse_masks[i] = cm[1] as u8;
            self.remaining += self.pulses[i] + consumed;
            update_lowband = b > (band_size << 3) as i32;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn quant_band(
        &mut self,
        rc: &mut Rc,
        band: usize,
        x: usize,
        y: Option<usize>,
        mut n: usize,
        mut b: i32,
        mut blocks: usize,
        mut lowband: Option<Loc>,
        mut duration: i32,
        lowband_out: Option<usize>,
        level: i32,
        gain: f32,
        scratch: bool,
        mut fill: u32,
    ) -> u32 {
        let stereo = y.is_some();
        let mut y = y;
        let mut split = stereo;
        let n0 = n;
        let mut n_b = n / blocks;
        let mut n_b0 = n_b;
        let mut b0 = blocks;
        let mut time_divide = 0;
        let mut recombine = 0;
        let mut inv = false;
        let mut mid = 0f32;
        let longblocks = b0 == 1;
        let mut cm = 0u32;

        if n == 1 {
            let mut at = x;
            for k in 0..=stereo as usize {
                let mut sign = 0;
                if self.remaining2 >= 1 << 3 {
                    sign = rc.raw(1);
                    self.remaining2 -= 1 << 3;
                }
                self.coeffs[at] = 1.0 - 2.0 * sign as f32;
                if k == 0 {
                    if let Some(yy) = y {
                        at = yy;
                    }
                }
            }
            if let Some(o) = lowband_out {
                self.norm[o] = self.coeffs[x];
            }
            return 1;
        }

        if !stereo && level == 0 {
            let mut tf_change = self.tf_change[band];
            if tf_change > 0 {
                recombine = tf_change;
            }
            if let Some(l) = lowband {
                if scratch && (recombine != 0 || ((n_b & 1) == 0 && tf_change < 0) || b0 > 1) {
                    let mut tmp = [0f32; 176];
                    tmp[..n].copy_from_slice(&self.lb(l)[..n]);
                    self.scratch[..n].copy_from_slice(&tmp[..n]);
                    lowband = Some(Loc::S(0));
                }
            }
            for k in 0..recombine {
                if let Some(l) = lowband {
                    haar1(self.lb(l), n >> k, 1 << k);
                }
                fill = CELT_BIT_INTERLEAVE[(fill & 0xF) as usize] as u32 | (CELT_BIT_INTERLEAVE[(fill >> 4) as usize] as u32) << 2;
            }
            blocks >>= recombine;
            n_b <<= recombine;
            while (n_b & 1) == 0 && tf_change < 0 {
                if let Some(l) = lowband {
                    haar1(self.lb(l), n_b, blocks);
                }
                fill |= fill << blocks;
                blocks <<= 1;
                n_b >>= 1;
                time_divide += 1;
                tf_change += 1;
            }
            b0 = blocks;
            n_b0 = n_b;
            if b0 > 1 {
                if let Some(l) = lowband {
                    deinterleave_hadamard(self.lb(l), n_b >> recombine, b0 << recombine, longblocks);
                }
            }
        }

        let cache = &CELT_CACHE_BITS[CELT_CACHE_INDEX[((duration + 1) as usize) * MAX_BANDS + band] as usize..];
        if !stereo && duration >= 0 && b > cache[cache[0] as usize] as i32 + 12 && n > 2 {
            n >>= 1;
            y = Some(x + n);
            split = true;
            duration -= 1;
            if blocks == 1 {
                fill = (fill & 1) | (fill << 1);
            }
            blocks = (blocks + 1) >> 1;
        }

        if split {
            let yy = y.unwrap();
            let pulse_cap = CELT_LOG_FREQ_RANGE[band] as i32 + duration * 8;
            let offset = (pulse_cap >> 1) - if stereo && n == 2 { 16 } else { 4 };
            let qn = if stereo && band >= self.intensity_stereo { 1 } else { compute_qn(n as i32, b, offset, pulse_cap, stereo) };
            let tell = rc.tell_frac();
            let mut itheta = 0i32;
            if qn != 1 {
                itheta = if stereo && n > 2 {
                    rc.uint_step((qn / 2) as u32) as i32
                } else if stereo || b0 > 1 {
                    rc.uint((qn + 1) as u32) as i32
                } else {
                    rc.uint_tri(qn as u32) as i32
                };
                itheta = itheta * 16384 / qn;
            } else if stereo {
                inv = if b > 2 << 3 && self.remaining2 > 2 << 3 { rc.log(2) != 0 } else { false };
            }
            let qalloc = rc.tell_frac() - tell;
            b -= qalloc;
            let orig_fill = fill;
            let (imid, iside, mut delta);
            if itheta == 0 {
                imid = 32767;
                iside = 0;
                fill &= (1u32 << blocks) - 1;
                delta = -16384;
            } else if itheta == 16384 {
                imid = 0;
                iside = 32767;
                fill &= ((1u32 << blocks) - 1) << blocks;
                delta = 16384;
            } else {
                imid = celt_cos(itheta);
                iside = celt_cos(16384 - itheta);
                delta = round_mul16(((n as i32) - 1) << 7, celt_log2tan(iside, imid));
            }
            mid = imid as f32 / 32768.0;
            let side = iside as f32 / 32768.0;

            if n == 2 && stereo {
                let mut mbits = b;
                let sbits = if itheta != 0 && itheta != 16384 { 1 << 3 } else { 0 };
                mbits -= sbits;
                let c = itheta > 8192;
                self.remaining2 -= qalloc + sbits;
                let (x2, y2) = if c { (yy, x) } else { (x, yy) };
                let mut sign = 0;
                if sbits != 0 {
                    sign = rc.raw(1) as i32;
                }
                let sign = (1 - 2 * sign) as f32;
                cm = self.quant_band(rc, band, x2, None, n, mbits, blocks, lowband, duration, lowband_out, level, gain, scratch, orig_fill);
                self.coeffs[y2] = -sign * self.coeffs[x2 + 1];
                self.coeffs[y2 + 1] = sign * self.coeffs[x2];
                self.coeffs[x] *= mid;
                self.coeffs[x + 1] *= mid;
                self.coeffs[yy] *= side;
                self.coeffs[yy + 1] *= side;
                for k in 0..2 {
                    let t = self.coeffs[x + k];
                    self.coeffs[x + k] = t - self.coeffs[yy + k];
                    self.coeffs[yy + k] = t + self.coeffs[yy + k];
                }
            } else {
                if b0 > 1 && !stereo && (itheta & 0x3fff) != 0 {
                    if itheta > 8192 {
                        delta -= delta >> (4 - duration);
                    } else {
                        delta = 0.min(delta + ((n as i32) << 3 >> (5 - duration)));
                    }
                }
                let mut mbits = ((b - delta) / 2).clamp(0, b);
                let mut sbits = b - mbits;
                self.remaining2 -= qalloc;
                let next_lowband2 = match (lowband, stereo) {
                    (Some(Loc::N(o)), false) => Some(Loc::N(o + n)),
                    (Some(Loc::S(o)), false) => Some(Loc::S(o + n)),
                    _ => None,
                };
                let (next_lowband_out1, next_level) = if stereo { (lowband_out, 0) } else { (None, level + 1) };
                let mut rebalance = self.remaining2;
                let shift = (b0 >> 1) & (if stereo { 0 } else { usize::MAX });
                if mbits >= sbits {
                    cm = self.quant_band(rc, band, x, None, n, mbits, blocks, lowband, duration, next_lowband_out1, next_level, if stereo { 1.0 } else { gain * mid }, scratch, fill);
                    rebalance = mbits - (rebalance - self.remaining2);
                    if rebalance > 3 << 3 && itheta != 0 {
                        sbits += rebalance - (3 << 3);
                    }
                    let cmt = self.quant_band(rc, band, yy, None, n, sbits, blocks, next_lowband2, duration, None, next_level, gain * side, false, fill >> blocks);
                    cm |= cmt << shift;
                } else {
                    cm = self.quant_band(rc, band, yy, None, n, sbits, blocks, next_lowband2, duration, None, next_level, gain * side, false, fill >> blocks);
                    cm <<= shift;
                    rebalance = sbits - (rebalance - self.remaining2);
                    if rebalance > 3 << 3 && itheta != 16384 {
                        mbits += rebalance - (3 << 3);
                    }
                    cm |= self.quant_band(rc, band, x, None, n, mbits, blocks, lowband, duration, next_lowband_out1, next_level, if stereo { 1.0 } else { gain * mid }, scratch, fill);
                }
            }
        } else {
            let mut q = bits2pulses(cache, b);
            let mut curr_bits = pulses2bits(cache, q);
            self.remaining2 -= curr_bits;
            while self.remaining2 < 0 && q > 0 {
                self.remaining2 += curr_bits;
                q -= 1;
                curr_bits = pulses2bits(cache, q);
                self.remaining2 -= curr_bits;
            }
            if q != 0 {
                let k = if q < 8 { q } else { (8 + (q & 7)) << ((q >> 3) - 1) };
                let v = (pvq_u(n, k) + pvq_u(n, k + 1)) as u32;
                let idx = rc.uint(v);
                let mut iy = [0i32; 176];
                let norm = cwrsi(n, k, idx, &mut iy);
                let g = gain / libm::sqrtf(norm);
                for i in 0..n {
                    self.coeffs[x + i] = g * iy[i] as f32;
                }
                exp_rotation(&mut self.coeffs[x..x + n], n, blocks, k, self.spread);
                self.qcoeff[..n].copy_from_slice(&iy[..n]);
                cm = collapse_mask(&iy, n, blocks);
            } else {
                let cm_mask = (1u32 << blocks) - 1;
                fill &= cm_mask;
                if fill != 0 {
                    match lowband {
                        None => {
                            for i in 0..n {
                                let r = self.rng();
                                self.coeffs[x + i] = ((r as i32) >> 20) as f32;
                            }
                            cm = cm_mask;
                        }
                        Some(l) => {
                            for i in 0..n {
                                let r = self.rng();
                                let lv = self.lb(l)[i];
                                self.coeffs[x + i] = lv + if r & 0x8000 != 0 { 1.0 / 256.0 } else { -1.0 / 256.0 };
                            }
                            cm = fill;
                        }
                    }
                    renormalize(&mut self.coeffs[x..x + n], gain);
                } else {
                    self.coeffs[x..x + n].iter_mut().for_each(|v| *v = 0.0);
                }
            }
        }

        if stereo {
            let yy = y.unwrap();
            if n > 2 {
                self.stereo_merge(x, yy, mid, n);
            }
            if inv {
                for i in 0..n {
                    self.coeffs[yy + i] = -self.coeffs[yy + i];
                }
            }
        } else if level == 0 {
            if b0 > 1 {
                interleave_hadamard(&mut self.coeffs[x..], n_b >> recombine, b0 << recombine, longblocks);
            }
            n_b = n_b0;
            blocks = b0;
            for _ in 0..time_divide {
                blocks >>= 1;
                n_b <<= 1;
                cm |= cm >> blocks;
                haar1(&mut self.coeffs[x..], n_b, blocks);
            }
            for k in 0..recombine {
                cm = CELT_BIT_DEINTERLEAVE[cm as usize] as u32;
                haar1(&mut self.coeffs[x..], n0 >> k, 1 << k);
            }
            blocks <<= recombine;
            if let Some(o) = lowband_out {
                let s = libm::sqrtf(n0 as f32);
                for i in 0..n0 {
                    self.norm[o + i] = s * self.coeffs[x + i];
                }
            }
            cm &= (1u32 << blocks) - 1;
        }
        cm
    }

    fn stereo_merge(&mut self, x: usize, y: usize, mid: f32, n: usize) {
        let (mut xp, mut side) = (0f32, 0f32);
        for i in 0..n {
            xp += self.coeffs[x + i] * self.coeffs[y + i];
            side += self.coeffs[y + i] * self.coeffs[y + i];
        }
        xp *= mid;
        let e0 = mid * mid + side - 2.0 * xp;
        let e1 = mid * mid + side + 2.0 * xp;
        if e0 < 6e-4 || e1 < 6e-4 {
            for i in 0..n {
                self.coeffs[y + i] = self.coeffs[x + i];
            }
            return;
        }
        let g0 = 1.0 / libm::sqrtf(e0);
        let g1 = 1.0 / libm::sqrtf(e1);
        for i in 0..n {
            let v0 = mid * self.coeffs[x + i];
            let v1 = self.coeffs[y + i];
            self.coeffs[x + i] = g0 * (v0 - v1);
            self.coeffs[y + i] = g1 * (v0 + v1);
        }
    }

    fn anticollapse(&mut self, ch: usize) {
        let base = ch * 960;
        for i in self.start_band..self.end_band {
            let mut renorm = false;
            let n = range(i) << self.size;
            let depth = (1 + self.pulses[i]) / n as i32;
            let thresh = libm::exp2f(-1.0 - 0.125 * depth as f32);
            let sqrt_1 = 1.0 / libm::sqrtf(n as f32);
            let xo = base + (bands(i) << self.size);
            let mut prev = [self.block[ch].prev_energy[0][i], self.block[ch].prev_energy[1][i]];
            if self.channels == 1 {
                prev[0] = prev[0].max(self.block[1].prev_energy[0][i]);
                prev[1] = prev[1].max(self.block[1].prev_energy[1][i]);
            }
            let ediff = (self.block[ch].energy[i] - prev[0].min(prev[1])).max(0.0);
            let mut r = libm::exp2f(1.0 - ediff);
            if self.size == 3 {
                r *= SQRT_2;
            }
            let r = thresh.min(r) * sqrt_1;
            for k in 0..1usize << self.size {
                if self.block[ch].collapse_masks[i] as u32 & (1 << k) == 0 {
                    for j in 0..range(i) {
                        let v = if self.rng() & 0x8000 != 0 { r } else { -r };
                        self.coeffs[xo + (j << self.size) + k] = v;
                    }
                    renorm = true;
                }
            }
            if renorm {
                renormalize(&mut self.coeffs[xo..xo + n], 1.0);
            }
        }
    }

    fn parse_postfilter(&mut self, rc: &mut Rc, mut consumed: i32) -> i32 {
        self.block[0].pf_gains_new = [0.0; 3];
        self.block[1].pf_gains_new = [0.0; 3];
        if self.start_band == 0 && consumed + 16 <= self.framebits {
            if rc.log(1) != 0 {
                let octave = rc.uint(6);
                let period = (16 << octave) + rc.raw(4 + octave) as usize - 1;
                let gain = 0.09375 * (rc.raw(3) + 1) as f32;
                let tapset = if rc.tell() + 2 <= self.framebits { rc.cdf(&CELT_MODEL_TAPSET) } else { 0 };
                for b in self.block.iter_mut() {
                    b.pf_period_new = period.max(15);
                    for k in 0..3 {
                        b.pf_gains_new[k] = gain * CELT_POSTFILTER_TAPS[tapset * 3 + k];
                    }
                }
            }
            consumed = rc.tell();
        }
        consumed
    }

    /// Decode one frame into `out[ch][..frame_size]` (one buffer per output channel).
    pub fn decode(&mut self, rc: &mut Rc, out: &mut [Vec<f32>], channels: usize, frame_size: usize, start_band: usize, end_band: usize) -> Option<()> {
        if !(1..=2).contains(&channels) || start_band > end_band || end_band > MAX_BANDS {
            return None;
        }
        let mut silence = false;
        self.transient = false;
        let mut anticollapse = false;
        self.flushed = false;
        self.channels = channels;
        self.start_band = start_band;
        self.end_band = end_band;
        self.framebits = rc.rb_bytes as i32 * 8;
        self.size = ilog((frame_size / 120) as u32) as usize - 1;
        if self.size > 3 || frame_size != 120 << self.size {
            return None;
        }
        self.coeffs.iter_mut().for_each(|v| *v = 0.0);
        for b in self.block.iter_mut() {
            b.collapse_masks = [0; MAX_BANDS];
        }
        let mut consumed = rc.tell();
        if consumed >= self.framebits {
            silence = true;
        } else if consumed == 1 {
            silence = rc.log(15) != 0;
        }
        if silence {
            consumed = self.framebits;
            rc.total_bits = (rc.total_bits as i32 + self.framebits - rc.tell()) as u32;
        }
        consumed = self.parse_postfilter(rc, consumed);
        if self.size != 0 && consumed + 3 <= self.framebits {
            self.transient = rc.log(3) != 0;
        }
        self.blocks = if self.transient { 1 << self.size } else { 1 };
        self.blocksize = frame_size / self.blocks;
        let tx = if self.transient { 0 } else { self.size };
        if channels == 1 {
            for i in 0..MAX_BANDS {
                self.block[0].energy[i] = self.block[0].energy[i].max(self.block[1].energy[i]);
            }
        }
        self.coarse_energy(rc);
        self.tf_changes(rc);
        self.bitalloc(rc);
        self.fine_energy(rc);
        self.quant_bands(rc);
        if self.anticollapse_needed != 0 {
            anticollapse = rc.raw(1) != 0;
        }
        self.final_energy(rc);
        for ch in 0..channels {
            if anticollapse {
                self.anticollapse(ch);
            }
            // Denormalise.
            for i in start_band..end_band {
                let o = ch * 960 + (bands(i) << self.size);
                let log_norm = self.block[ch].energy[i] + CELT_MEAN_ENERGY[i];
                let norm = libm::exp2f(log_norm.min(32.0));
                for v in &mut self.coeffs[o..o + (range(i) << self.size)] {
                    *v *= norm;
                }
            }
        }
        let mut downmix = false;
        if self.output_channels < channels {
            for i in 0..frame_size {
                self.coeffs[i] += self.coeffs[960 + i];
            }
            downmix = true;
        } else if self.output_channels > channels {
            self.coeffs.copy_within(0..frame_size, 960);
        }
        if silence {
            for b in self.block.iter_mut() {
                b.energy = [ENERGY_SILENCE; MAX_BANDS];
            }
            self.coeffs.iter_mut().for_each(|v| *v = 0.0);
        }
        let win = window();
        for ch in 0..self.output_channels {
            let blocks = self.blocks;
            let bs = self.blocksize;
            for j in 0..blocks {
                let dst = 1024 + j * bs;
                let (imdct, coeffs, block) = (&self.imdct[tx], &self.coeffs, &mut self.block[ch]);
                imdct.run(&coeffs[ch * 960 + j..], blocks, &mut block.buf[dst + OVERLAP / 2..dst + OVERLAP / 2 + bs]);
                // Overlap the previous block's tail with this one's start.
                let d = &mut block.buf[dst..dst + OVERLAP];
                for i in 0..OVERLAP / 2 {
                    let jj = OVERLAP - 1 - i;
                    let s0 = d[i];
                    let s1 = d[jj];
                    let (wi, wj) = (win[i], win[jj]);
                    d[i] = s0 * wj - s1 * wi;
                    d[jj] = s0 * wi + s1 * wj;
                }
            }
            if downmix {
                for v in &mut self.block[ch].buf[1024..1024 + frame_size] {
                    *v *= 0.5;
                }
            }
            self.postfilter(ch, frame_size);
            let b = &mut self.block[ch];
            let c = CELT_EMPH_COEFF;
            let mut coeff = b.emph_coeff;
            let o = &mut out[ch];
            for i in 0..frame_size {
                coeff = b.buf[1024 - frame_size + i] + coeff * c;
                o[i] = coeff;
            }
            b.emph_coeff = if coeff.is_normal() { coeff } else { 0.0 };
        }
        if channels == 1 {
            self.block[1].energy = self.block[0].energy;
        }
        for b in self.block.iter_mut() {
            if !self.transient {
                b.prev_energy[1] = b.prev_energy[0];
                b.prev_energy[0] = b.energy;
            } else {
                for j in 0..MAX_BANDS {
                    b.prev_energy[0][j] = b.prev_energy[0][j].min(b.energy[j]);
                }
            }
            for j in (0..start_band).chain(end_band..MAX_BANDS) {
                b.prev_energy[0][j] = ENERGY_SILENCE;
                b.energy[j] = 0.0;
            }
        }
        self.seed = rc.range;
        Some(())
    }

    fn postfilter(&mut self, ch: usize, len: usize) {
        let b = &mut self.block[ch];
        pf_transition(b, 1024);
        b.pf_period_old = b.pf_period;
        b.pf_gains_old = b.pf_gains;
        b.pf_period = b.pf_period_new;
        b.pf_gains = b.pf_gains_new;
        if len > OVERLAP {
            pf_transition(b, 1024 + OVERLAP);
            let filter_len = len as isize - 2 * OVERLAP as isize;
            if b.pf_gains[0] > f32::EPSILON && filter_len > 0 {
                let (g0, g1, g2) = (b.pf_gains[0], b.pf_gains[1], b.pf_gains[2]);
                let at = 1024 + 2 * OVERLAP;
                let p = b.pf_period;
                let d = &mut b.buf;
                let mut x4 = d[at - p - 2];
                let mut x3 = d[at - p - 1];
                let mut x2 = d[at - p];
                let mut x1 = d[at - p + 1];
                for i in 0..filter_len as usize {
                    let x0 = d[at + i - p + 2];
                    d[at + i] += g0 * x2 + g1 * (x1 + x3) + g2 * (x0 + x4);
                    x4 = x3;
                    x3 = x2;
                    x2 = x1;
                    x1 = x0;
                }
            }
            b.pf_period_old = b.pf_period;
            b.pf_gains_old = b.pf_gains;
        }
        b.buf.copy_within(len..len + 1024 + OVERLAP / 2, 0);
    }
}

fn pf_transition(b: &mut Block, at: usize) {
    let t0 = b.pf_period_old;
    let t1 = b.pf_period;
    if b.pf_gains[0] == 0.0 && b.pf_gains_old[0] == 0.0 {
        return;
    }
    let (g00, g01, g02) = (b.pf_gains_old[0], b.pf_gains_old[1], b.pf_gains_old[2]);
    let (g10, g11, g12) = (b.pf_gains[0], b.pf_gains[1], b.pf_gains[2]);
    let d = &mut b.buf;
    let mut x1 = d[at - t1 + 1];
    let mut x2 = d[at - t1];
    let mut x3 = d[at - t1 - 1];
    let mut x4 = d[at - t1 - 2];
    for i in 0..OVERLAP {
        let w = CELT_WINDOW2[i];
        let x0 = d[at + i - t1 + 2];
        d[at + i] += (1.0 - w) * g00 * d[at + i - t0]
            + (1.0 - w) * g01 * (d[at + i - t0 - 1] + d[at + i - t0 + 1])
            + (1.0 - w) * g02 * (d[at + i - t0 - 2] + d[at + i - t0 + 2])
            + w * g10 * x2
            + w * g11 * (x1 + x3)
            + w * g12 * (x0 + x4);
        x4 = x3;
        x3 = x2;
        x2 = x1;
        x1 = x0;
    }
}

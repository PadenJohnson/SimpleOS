//! AAC-LC decoder (the sound in phone and camera MP4 videos): Huffman spectral data,
//! scale factors, M/S and intensity stereo, noise substitution, TNS, long and short
//! windows (sine and KBD) and the inverse MDCT. HE-AAC files play their AAC-LC core
//! (duller, but audible).
//!
//! Uses f32 (software floating point in the kernel; a few percent of a CPU core).

use alloc::vec;
use alloc::vec::Vec;

use super::bits::Bits;
use super::tables::*;

const RATES: [u32; 13] = [96000, 88200, 64000, 48000, 44100, 32000, 24000, 22050, 16000, 12000, 11025, 8000, 7350];
const TNS_MAX_LONG: [usize; 13] = [31, 31, 34, 40, 42, 51, 46, 46, 42, 42, 42, 39, 39];
const TNS_MAX_SHORT: [usize; 13] = [9, 9, 10, 14, 14, 14, 14, 14, 14, 14, 14, 14, 14];
const PI: f32 = core::f32::consts::PI;

/// A Huffman code book: for each code length, the (code, symbol) pairs sorted by code.
struct Book {
    levels: Vec<Vec<(u32, u16)>>,
    max: u32,
}

impl Book {
    fn new(codes: &[u32], bits: &[u8]) -> Book {
        let max = *bits.iter().max().unwrap_or(&1) as u32;
        let mut levels = vec![Vec::new(); max as usize + 1];
        for (i, (&c, &b)) in codes.iter().zip(bits).enumerate() {
            if b > 0 {
                levels[b as usize].push((c, i as u16));
            }
        }
        for l in levels.iter_mut() {
            l.sort();
        }
        Book { levels, max }
    }
    fn decode(&self, b: &mut Bits) -> Option<usize> {
        let peek = b.peek(self.max);
        for len in 1..=self.max {
            let v = peek >> (self.max - len);
            let lvl = &self.levels[len as usize];
            if let Ok(k) = lvl.binary_search_by_key(&v, |e| e.0) {
                b.skip(len as usize);
                return Some(lvl[k].1 as usize);
            }
        }
        None
    }
}

/// FFT-based inverse MDCT of size n (n/2 coefficients in, n samples out).
///
/// The IMDCT is a DCT-IV of size m = n/2, folded: with u = DCT-IV(X),
/// y = [u[m/2..m], -reverse(u), -u[0..m/2]] (times 2/n). The DCT-IV is done with an
/// n/4-point complex FFT: z_j = (X[2j] - i X[m-1-2j]) e^{i pi j/m}, S = sum z_j e^{+2 pi i pj/l},
/// w_p = S_p e^{i pi (p + 1/4)/m}, u[2p] = Re w_p, u[m-1-2p] = Im w_p.
pub(crate) struct Imdct {
    n: usize,
    pre: Vec<(f32, f32)>,
    post: Vec<(f32, f32)>,
    rev: Vec<usize>,
    wr: Vec<f32>,
    wi: Vec<f32>,
}

impl Imdct {
    pub(crate) fn new(n: usize) -> Imdct {
        let (m, l) = (n / 2, n / 4);
        let pre = (0..l).map(|j| {
            let a = PI * j as f32 / m as f32;
            (libm::cosf(a), libm::sinf(a))
        });
        let post = (0..l).map(|p| {
            let a = PI * (p as f32 + 0.25) / m as f32;
            (libm::cosf(a), libm::sinf(a))
        });
        let bits = l.trailing_zeros();
        let rev = (0..l).map(|i| (i as u32).reverse_bits() as usize >> (32 - bits)).collect();
        let wr = (0..l / 2).map(|i| libm::cosf(2.0 * PI * i as f32 / l as f32)).collect();
        let wi = (0..l / 2).map(|i| libm::sinf(2.0 * PI * i as f32 / l as f32)).collect();
        Imdct { n, pre: pre.collect(), post: post.collect(), rev, wr, wi }
    }

    /// In-place radix-2 FFT (input already in bit-reversed order), e^{+i}.
    fn fft(&self, re: &mut [f32], im: &mut [f32]) {
        let n = re.len();
        let mut size = 2;
        while size <= n {
            let half = size / 2;
            let step = n / size;
            for start in (0..n).step_by(size) {
                for k in 0..half {
                    let (c, s) = (self.wr[k * step], self.wi[k * step]);
                    let (a, b) = (start + k, start + k + half);
                    let tr = re[b] * c - im[b] * s;
                    let ti = re[b] * s + im[b] * c;
                    re[b] = re[a] - tr;
                    im[b] = im[a] - ti;
                    re[a] += tr;
                    im[a] += ti;
                }
            }
            size *= 2;
        }
    }

    pub(crate) fn run(&self, input: &[f32], out: &mut [f32]) {
        let (n, m, l) = (self.n, self.n / 2, self.n / 4);
        let mut re = vec![0f32; l];
        let mut im = vec![0f32; l];
        for j in 0..l {
            let (a, b) = (input[2 * j], input[m - 1 - 2 * j]);
            let (c, s) = self.pre[j];
            let k = self.rev[j];
            re[k] = a * c + b * s;
            im[k] = a * s - b * c;
        }
        self.fft(&mut re, &mut im);
        let scale = 2.0 / n as f32;
        let mut u = vec![0f32; m];
        for p in 0..l {
            let (c, s) = self.post[p];
            u[2 * p] = (re[p] * c - im[p] * s) * scale;
            u[m - 1 - 2 * p] = (re[p] * s + im[p] * c) * scale;
        }
        for i in 0..m / 2 {
            out[i] = u[m / 2 + i];
            out[m + m / 2 + i] = -u[i];
        }
        for i in 0..m {
            out[m / 2 + i] = -u[m - 1 - i];
        }
    }
}

/// Reference inverse MDCT (slow), for tests: x[n] = 2/N sum X[k] cos(2pi/N (n + n0)(k + 1/2)).
pub fn imdct_reference(input: &[f32], n: usize) -> Vec<f32> {
    let n0 = (n as f32 / 2.0 + 1.0) / 2.0;
    (0..n)
        .map(|i| {
            let mut s = 0f32;
            for (k, &x) in input.iter().enumerate() {
                s += x * libm::cosf(2.0 * PI / n as f32 * (i as f32 + n0) * (k as f32 + 0.5));
            }
            s * 2.0 / n as f32
        })
        .collect()
}

pub fn imdct_fast(input: &[f32], n: usize) -> Vec<f32> {
    let m = Imdct::new(n);
    let mut out = vec![0f32; n];
    m.run(input, &mut out);
    out
}

fn bessel_i0(x: f32) -> f32 {
    let mut sum = 1.0f32;
    let mut term = 1.0f32;
    for k in 1..50 {
        term *= (x / 2.0) * (x / 2.0) / (k * k) as f32;
        sum += term;
        if term < 1e-10 * sum {
            break;
        }
    }
    sum
}

/// Rising half of a KBD window of length n (n/2 values).
pub(crate) fn kbd(n: usize, alpha: f32) -> Vec<f32> {
    let half = n / 2;
    let kernel: Vec<f32> = (0..=half)
        .map(|j| {
            let t = (j as f32 - half as f32 / 2.0) / (half as f32 / 2.0);
            bessel_i0(PI * alpha * libm::sqrtf((1.0 - t * t).max(0.0)))
        })
        .collect();
    let total: f32 = kernel.iter().sum();
    let mut acc = 0f32;
    (0..half)
        .map(|i| {
            acc += kernel[i];
            libm::sqrtf(acc / total)
        })
        .collect()
}

#[derive(Clone, Default)]
struct Tns {
    /// Per window: filters (length, order, direction, coefficients).
    filters: Vec<Vec<(usize, usize, bool, Vec<f32>)>>,
}

#[derive(Clone)]
struct Ics {
    seq: u8,
    shape: u8,
    max_sfb: usize,
    groups: Vec<usize>,
    band_type: [[u8; 64]; 8],
    sf: [[i32; 64]; 8],
    spec: [f32; 1024],
    tns: Option<Tns>,
}

impl Ics {
    fn new() -> Ics {
        Ics { seq: 0, shape: 0, max_sfb: 0, groups: vec![1], band_type: [[0; 64]; 8], sf: [[0; 64]; 8], spec: [0.0; 1024], tns: None }
    }
    fn short(&self) -> bool {
        self.seq == 2
    }
}

pub struct Aac {
    pub rate: u32,
    pub channels: usize,
    rate_idx: usize,
    books: Vec<Book>,
    sf_book: Book,
    pow43: Vec<f32>,
    long: Imdct,
    short: Imdct,
    win_long: [Vec<f32>; 2],
    win_short: [Vec<f32>; 2],
    overlap: Vec<Vec<f32>>,
    prev_shape: Vec<u8>,
    seed: u32,
    pub sbr: bool,
}

impl Aac {
    pub fn new(asc: &[u8]) -> Result<Aac, &'static str> {
        let mut b = Bits::new(asc);
        let mut aot = b.read(5);
        if aot == 31 {
            aot = 32 + b.read(6);
        }
        let read_rate = |b: &mut Bits| -> (usize, u32) {
            let i = b.read(4) as usize;
            if i == 15 {
                let r = b.read(24);
                let idx = RATES.iter().enumerate().min_by_key(|(_, &x)| (x as i64 - r as i64).abs()).map(|(k, _)| k).unwrap();
                (idx, r)
            } else {
                (i.min(12), RATES[i.min(12)])
            }
        };
        let (mut idx, mut rate) = read_rate(&mut b);
        let ch = b.read(4) as usize;
        let mut sbr = false;
        if aot == 5 || aot == 29 {
            sbr = true;
            let _ext = read_rate(&mut b);
            aot = b.read(5);
        }
        if aot != 2 && aot != 1 && aot != 4 {
            return Err("this AAC type isn't supported");
        }
        if b.flag() {
            return Err("960-sample AAC frames aren't supported");
        }
        let _ = &mut idx;
        let _ = &mut rate;
        let channels = match ch {
            0 | 2 => 2,
            1 => 1,
            _ => ch.min(8),
        };
        let codes: [(&[u16], &[u8]); 11] = [(&CODES1, &BITS1), (&CODES2, &BITS2), (&CODES3, &BITS3), (&CODES4, &BITS4), (&CODES5, &BITS5), (&CODES6, &BITS6), (&CODES7, &BITS7), (&CODES8, &BITS8), (&CODES9, &BITS9), (&CODES10, &BITS10), (&CODES11, &BITS11)];
        let books = codes.iter().map(|(c, b)| Book::new(&c.iter().map(|&x| x as u32).collect::<Vec<_>>(), b)).collect();
        let sf_book = Book::new(&SF_CODE, &SF_BITS);
        let pow43 = (0..8192).map(|i| libm::powf(i as f32, 4.0 / 3.0)).collect();
        let sine = |n: usize| (0..n / 2).map(|i| libm::sinf(PI / n as f32 * (i as f32 + 0.5))).collect::<Vec<f32>>();
        Ok(Aac {
            rate,
            channels,
            rate_idx: idx,
            books,
            sf_book,
            pow43,
            long: Imdct::new(2048),
            short: Imdct::new(256),
            win_long: [sine(2048), kbd(2048, 4.0)],
            win_short: [sine(256), kbd(256, 6.0)],
            overlap: vec![vec![0.0; 1024]; channels.max(2)],
            prev_shape: vec![0; channels.max(2)],
            seed: 0x1F2E3D4C,
            sbr,
        })
    }

    fn swb(&self, short: bool) -> &'static [u16] {
        let i = self.rate_idx;
        if short {
            match i {
                0..=2 => &SWB_128_96,
                3..=5 => &SWB_128_48,
                6 | 7 => &SWB_128_24,
                8..=10 => &SWB_128_16,
                _ => &SWB_128_8,
            }
        } else {
            match i {
                0 | 1 => &SWB_1024_96,
                2 => &SWB_1024_64,
                3 | 4 => &SWB_1024_48,
                5 => &SWB_1024_32,
                6 | 7 => &SWB_1024_24,
                8..=10 => &SWB_1024_16,
                _ => &SWB_1024_8,
            }
        }
    }

    fn num_swb(&self, short: bool) -> usize {
        if short {
            NUM_SWB_128[self.rate_idx] as usize
        } else {
            NUM_SWB_1024[self.rate_idx] as usize
        }
    }

    fn ics_info(&self, b: &mut Bits, ics: &mut Ics) -> Result<(), &'static str> {
        b.bit();
        ics.seq = b.read(2) as u8;
        ics.shape = b.read(1) as u8;
        if ics.short() {
            ics.max_sfb = b.read(4) as usize;
            let grouping = b.read(7);
            ics.groups = vec![1];
            for k in 0..7 {
                if grouping & (1 << (6 - k)) != 0 {
                    *ics.groups.last_mut().unwrap() += 1;
                } else {
                    ics.groups.push(1);
                }
            }
        } else {
            ics.max_sfb = b.read(6) as usize;
            ics.groups = vec![1];
            if b.flag() {
                return Err("AAC prediction isn't supported");
            }
        }
        if ics.max_sfb > self.num_swb(ics.short()) {
            return Err("bad max_sfb");
        }
        Ok(())
    }

    fn ics(&mut self, b: &mut Bits, ics: &mut Ics, common: bool) -> Result<(), &'static str> {
        let global = b.read(8) as i32;
        if !common {
            self.ics_info(b, ics)?;
        }
        let short = ics.short();
        // Section data.
        let bits = if short { 3 } else { 5 };
        let esc = (1 << bits) - 1;
        for g in 0..ics.groups.len() {
            let mut k = 0;
            while k < ics.max_sfb {
                let cb = b.read(4) as u8;
                if cb == 12 {
                    return Err("bad section codebook");
                }
                let mut len = 0;
                loop {
                    let inc = b.read(bits);
                    len += inc as usize;
                    if inc != esc || b.left() <= 0 {
                        break;
                    }
                }
                if k + len > ics.max_sfb {
                    return Err("bad section length");
                }
                for s in k..k + len {
                    ics.band_type[g][s] = cb;
                }
                k += len;
            }
        }
        // Scale factors.
        let (mut sf, mut is_pos, mut noise) = (global, 0i32, global - 90);
        let mut first_noise = true;
        for g in 0..ics.groups.len() {
            for s in 0..ics.max_sfb {
                match ics.band_type[g][s] {
                    0 => ics.sf[g][s] = 0,
                    14 | 15 => {
                        is_pos += self.sf_book.decode(b).ok_or("bad scale factor")? as i32 - 60;
                        ics.sf[g][s] = is_pos;
                    }
                    13 => {
                        if first_noise {
                            noise += b.read(9) as i32 - 256;
                            first_noise = false;
                        } else {
                            noise += self.sf_book.decode(b).ok_or("bad scale factor")? as i32 - 60;
                        }
                        ics.sf[g][s] = noise;
                    }
                    _ => {
                        sf += self.sf_book.decode(b).ok_or("bad scale factor")? as i32 - 60;
                        ics.sf[g][s] = sf;
                    }
                }
            }
        }
        // Pulse data (long windows).
        let mut pulses: Vec<(usize, i32)> = Vec::new();
        if b.flag() {
            if short {
                return Err("pulses in a short window");
            }
            let n = b.read(2) as usize + 1;
            let start = b.read(6) as usize;
            let swb = self.swb(false);
            let mut pos = *swb.get(start).unwrap_or(&0) as usize;
            for _ in 0..n {
                pos += b.read(5) as usize;
                pulses.push((pos, b.read(4) as i32));
            }
        }
        // TNS.
        ics.tns = None;
        if b.flag() {
            let nwin = if short { 8 } else { 1 };
            let mut t = Tns { filters: Vec::new() };
            for _ in 0..nwin {
                let n_filt = b.read(if short { 1 } else { 2 }) as usize;
                let mut fl = Vec::new();
                if n_filt > 0 {
                    let res = b.read(1);
                    for _ in 0..n_filt {
                        let length = b.read(if short { 4 } else { 6 }) as usize;
                        let order = b.read(if short { 3 } else { 5 }) as usize;
                        if order > 0 {
                            let dir = b.flag();
                            let compress = b.read(1);
                            let cbits = res + 3 - compress;
                            let rbits = res + 3;
                            let iqfac = ((1 << (rbits - 1)) as f32 - 0.5) / (PI / 2.0);
                            let iqfac_m = ((1 << (rbits - 1)) as f32 + 0.5) / (PI / 2.0);
                            let mut tmp = Vec::with_capacity(order);
                            for _ in 0..order {
                                let raw = b.read(cbits) as i32;
                                let v = if raw >= 1 << (cbits - 1) { raw - (1 << cbits) } else { raw };
                                tmp.push(libm::sinf(v as f32 / if v >= 0 { iqfac } else { iqfac_m }));
                            }
                            // Reflection coefficients -> LPC.
                            let mut a = vec![0f32; order + 1];
                            a[0] = 1.0;
                            for m in 1..=order {
                                let prev = a.clone();
                                for i in 1..m {
                                    a[i] = prev[i] + tmp[m - 1] * prev[m - i];
                                }
                                a[m] = tmp[m - 1];
                            }
                            fl.push((length, order, dir, a));
                        } else {
                            fl.push((length, 0, false, Vec::new()));
                        }
                    }
                }
                t.filters.push(fl);
            }
            ics.tns = Some(t);
        }
        if b.flag() {
            return Err("AAC gain control isn't supported");
        }
        // Spectral data.
        let swb = self.swb(short);
        let mut q = [0i32; 1024];
        let mut win0 = 0;
        for (g, &glen) in ics.groups.iter().enumerate() {
            for s in 0..ics.max_sfb {
                let cb = ics.band_type[g][s] as usize;
                let (lo, hi) = (swb[s] as usize, swb[s + 1] as usize);
                if cb == 0 || cb >= 13 {
                    continue;
                }
                let book = &self.books[cb - 1];
                let dim = if cb < 5 { 4 } else { 2 };
                let signed = matches!(cb, 1 | 2 | 5 | 6);
                for w in 0..glen {
                    let base = (win0 + w) * 128;
                    let mut k = lo;
                    while k < hi {
                        let sym = book.decode(b).ok_or("bad spectral code")?;
                        let mut vals = [0i32; 4];
                        if dim == 4 {
                            let (off, m) = if signed { (1, 3) } else { (0, 3) };
                            vals = [(sym / 27) as i32 - off, ((sym / 9) % 3) as i32 - off, ((sym / 3) % 3) as i32 - off, (sym % 3) as i32 - off];
                            let _ = m;
                        } else {
                            let (m, off) = match cb {
                                5 | 6 => (9, 4),
                                7 | 8 => (8, 0),
                                9 | 10 => (13, 0),
                                _ => (17, 0),
                            };
                            vals[0] = (sym / m) as i32 - off;
                            vals[1] = (sym % m) as i32 - off;
                        }
                        if !signed {
                            for v in vals.iter_mut().take(dim) {
                                if *v != 0 && b.bit() == 1 {
                                    *v = -*v;
                                }
                            }
                        }
                        if cb == 11 {
                            for v in vals.iter_mut().take(2) {
                                if v.abs() == 16 {
                                    let mut n = 4;
                                    while b.bit() == 1 && n < 13 {
                                        n += 1;
                                    }
                                    let e = (1 << n) + b.read(n) as i32;
                                    *v = if *v < 0 { -e } else { e };
                                }
                            }
                        }
                        for (j, &v) in vals.iter().enumerate().take(dim) {
                            let pos = if short { base + k + j } else { k + j };
                            if pos < 1024 {
                                q[pos] = v;
                            }
                        }
                        k += dim;
                    }
                }
            }
            win0 += glen;
        }
        for (pos, amp) in pulses {
            if pos < 1024 {
                q[pos] += if q[pos] < 0 { -amp } else { amp };
            }
        }
        // Inverse quantisation and scaling.
        ics.spec = [0.0; 1024];
        let mut win0 = 0;
        for (g, &glen) in ics.groups.iter().enumerate() {
            for s in 0..ics.max_sfb {
                let cb = ics.band_type[g][s];
                let (lo, hi) = (swb[s] as usize, swb[s + 1] as usize);
                for w in 0..glen {
                    let base = if short { (win0 + w) * 128 } else { 0 };
                    if cb == 13 {
                        // Noise substitution.
                        let mut e = 0f32;
                        for k in lo..hi {
                            self.seed = self.seed.wrapping_mul(1664525).wrapping_add(1013904223);
                            let r = (self.seed as i32) as f32 / 2147483648.0;
                            ics.spec[base + k] = r;
                            e += r * r;
                        }
                        let scale = libm::powf(2.0, 0.25 * ics.sf[g][s] as f32) / libm::sqrtf(e.max(1e-9));
                        for k in lo..hi {
                            ics.spec[base + k] *= scale;
                        }
                        continue;
                    }
                    if cb == 0 || cb >= 13 {
                        continue;
                    }
                    let gain = libm::powf(2.0, 0.25 * (ics.sf[g][s] - 100) as f32);
                    for k in lo..hi {
                        let v = q[base + k];
                        if v != 0 {
                            let m = self.pow43[(v.unsigned_abs() as usize).min(8191)] * gain;
                            ics.spec[base + k] = if v < 0 { -m } else { m };
                        }
                    }
                }
            }
            win0 += glen;
        }
        Ok(())
    }

    fn apply_tns(&self, ics: &mut Ics) {
        let Some(t) = ics.tns.clone() else { return };
        let short = ics.short();
        let swb = self.swb(short);
        let nswb = self.num_swb(short);
        let maxb = if short { TNS_MAX_SHORT[self.rate_idx] } else { TNS_MAX_LONG[self.rate_idx] }.min(ics.max_sfb).min(nswb);
        for (w, fl) in t.filters.iter().enumerate() {
            let base = if short { w * 128 } else { 0 };
            let mut top = nswb;
            for (length, order, dir, a) in fl {
                let bottom = top.saturating_sub(*length);
                let (start, end) = (swb[bottom.min(maxb)] as usize, swb[top.min(maxb)] as usize);
                top = bottom;
                if *order == 0 || end <= start {
                    continue;
                }
                let n = end - start;
                let mut state = vec![0f32; *order];
                for i in 0..n {
                    let k = if *dir { end - 1 - i } else { start + i };
                    let mut y = ics.spec[base + k];
                    for j in 0..*order {
                        y -= a[j + 1] * state[j];
                    }
                    for j in (1..*order).rev() {
                        state[j] = state[j - 1];
                    }
                    state[0] = y;
                    ics.spec[base + k] = y;
                }
            }
        }
    }

    /// Spectrum -> windowed, overlapped samples for one channel.
    fn synth(&mut self, ics: &Ics, ch: usize, out: &mut [f32; 1024]) {
        let mut buf = vec![0f32; 2048];
        let prev = self.prev_shape[ch] as usize;
        let cur = ics.shape as usize;
        let (wl_prev, wl_cur) = (&self.win_long[prev], &self.win_long[cur]);
        let (ws_prev, ws_cur) = (&self.win_short[prev], &self.win_short[cur]);
        if ics.short() {
            let mut tmp = vec![0f32; 256];
            for w in 0..8 {
                self.short.run(&ics.spec[w * 128..w * 128 + 128], &mut tmp);
                let left = if w == 0 { ws_prev } else { ws_cur };
                for i in 0..128 {
                    buf[448 + w * 128 + i] += tmp[i] * left[i];
                    buf[448 + w * 128 + 128 + i] += tmp[128 + i] * ws_cur[127 - i];
                }
            }
        } else {
            self.long.run(&ics.spec, &mut buf);
            match ics.seq {
                1 => {
                    for i in 0..1024 {
                        buf[i] *= wl_prev[i];
                    }
                    for i in 1472..1600 {
                        buf[i] *= ws_cur[127 - (i - 1472)];
                    }
                    for v in buf[1600..].iter_mut() {
                        *v = 0.0;
                    }
                }
                3 => {
                    for v in buf[..448].iter_mut() {
                        *v = 0.0;
                    }
                    for i in 448..576 {
                        buf[i] *= ws_prev[i - 448];
                    }
                    for i in 1024..2048 {
                        buf[i] *= wl_cur[2047 - i];
                    }
                }
                _ => {
                    for i in 0..1024 {
                        buf[i] *= wl_prev[i];
                        buf[1024 + i] *= wl_cur[1023 - i];
                    }
                }
            }
        }
        for i in 0..1024 {
            out[i] = buf[i] + self.overlap[ch][i];
            self.overlap[ch][i] = buf[1024 + i];
        }
        self.prev_shape[ch] = ics.shape;
    }

    /// Decode one raw AAC frame (an MP4 sample). Returns 1024 interleaved stereo frames.
    pub fn decode(&mut self, data: &[u8]) -> Result<Vec<i16>, &'static str> {
        let mut b = Bits::new(data);
        let mut chans: Vec<Ics> = Vec::new();
        loop {
            if b.left() < 3 {
                break;
            }
            let id = b.read(3);
            match id {
                0 | 3 => {
                    b.read(4);
                    let mut ics = Ics::new();
                    self.ics(&mut b, &mut ics, false)?;
                    chans.push(ics);
                }
                1 => {
                    b.read(4);
                    let common = b.flag();
                    let mut l = Ics::new();
                    let mut ms = [[false; 64]; 8];
                    let mut ms_present = 0;
                    if common {
                        self.ics_info(&mut b, &mut l)?;
                        ms_present = b.read(2);
                        if ms_present == 1 {
                            for g in ms.iter_mut().take(l.groups.len()) {
                                for v in g.iter_mut().take(l.max_sfb) {
                                    *v = b.flag();
                                }
                            }
                        }
                    }
                    let mut r = l.clone();
                    self.ics(&mut b, &mut l, common)?;
                    self.ics(&mut b, &mut r, common)?;
                    if common {
                        self.stereo(&mut l, &mut r, &ms, ms_present);
                    }
                    chans.push(l);
                    chans.push(r);
                }
                4 => {
                    // Data stream element: skip.
                    b.read(4);
                    let align = b.flag();
                    let mut n = b.read(8) as usize;
                    if n == 255 {
                        n += b.read(8) as usize;
                    }
                    if align {
                        b.align();
                    }
                    b.skip(n * 8);
                }
                5 => return Err("program config elements aren't supported"),
                6 => {
                    // Fill: skip (SBR data lives here).
                    let mut n = b.read(4) as usize;
                    if n == 15 {
                        n += b.read(8) as usize - 1;
                    }
                    b.skip(n * 8);
                }
                7 => break,
                _ => return Err("unsupported AAC element"),
            }
        }
        if chans.is_empty() {
            return Ok(vec![0; 2048]);
        }
        let mut pcm = vec![[0f32; 1024]; 2];
        for (ch, ics) in chans.iter_mut().enumerate().take(2) {
            self.apply_tns(ics);
            let mut o = [0f32; 1024];
            self.synth(ics, ch, &mut o);
            pcm[ch] = o;
        }
        let stereo = chans.len() >= 2;
        let mut out = Vec::with_capacity(2048);
        for i in 0..1024 {
            let l = pcm[0][i];
            let r = if stereo { pcm[1][i] } else { l };
            out.push(l.clamp(-32768.0, 32767.0) as i16);
            out.push(r.clamp(-32768.0, 32767.0) as i16);
        }
        Ok(out)
    }

    fn stereo(&self, l: &mut Ics, r: &mut Ics, ms: &[[bool; 64]; 8], ms_present: u32) {
        let short = l.short();
        let swb = self.swb(short);
        let mut win0 = 0;
        for (g, &glen) in l.groups.iter().enumerate() {
            for s in 0..l.max_sfb {
                let (lo, hi) = (swb[s] as usize, swb[s + 1] as usize);
                let rt = r.band_type[g][s];
                let used = ms_present == 2 || (ms_present == 1 && ms[g][s]);
                for w in 0..glen {
                    let base = if short { (win0 + w) * 128 } else { 0 };
                    if rt == 14 || rt == 15 {
                        let mut scale = libm::powf(0.5, 0.25 * r.sf[g][s] as f32);
                        if rt == 14 {
                            scale = -scale;
                        }
                        if ms_present == 1 && ms[g][s] {
                            scale = -scale;
                        }
                        for k in lo..hi {
                            r.spec[base + k] = l.spec[base + k] * scale;
                        }
                    } else if used && l.band_type[g][s] < 13 && rt < 13 {
                        for k in lo..hi {
                            let (a, b) = (l.spec[base + k], r.spec[base + k]);
                            l.spec[base + k] = a + b;
                            r.spec[base + k] = a - b;
                        }
                    }
                }
            }
            win0 += glen;
        }
    }
}

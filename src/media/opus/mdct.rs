//! Inverse MDCT for CELT's sizes (120 x 2^k coefficients) with a mixed-radix FFT, giving
//! the "half" output FFmpeg's transform gives (the middle N of the 2N samples), scaled
//! by 1/32768.

use alloc::vec;
use alloc::vec::Vec;
use core::f32::consts::PI;

pub struct Imdct {
    n: usize,
    pre: Vec<(f32, f32)>,
    post: Vec<(f32, f32)>,
    fft: Fft,
}

impl Imdct {
    /// `n` coefficients in, `n` samples out.
    pub fn new(n: usize) -> Imdct {
        let full = 2 * n;
        let (m, l) = (full / 2, full / 4);
        let pre = (0..l).map(|j| {
            let a = PI * j as f32 / m as f32;
            (libm::cosf(a), libm::sinf(a))
        });
        let post = (0..l).map(|p| {
            let a = PI * (p as f32 + 0.25) / m as f32;
            (libm::cosf(a), libm::sinf(a))
        });
        Imdct { n, pre: pre.collect(), post: post.collect(), fft: Fft::new(l) }
    }

    /// Coefficients `input[k * stride]` for k in 0..n; writes n samples to `out`.
    pub fn run(&self, input: &[f32], stride: usize, out: &mut [f32]) {
        let m = self.n;
        let l = m / 2;
        let mut z = vec![(0f32, 0f32); l];
        for (j, zj) in z.iter_mut().enumerate() {
            let (a, b) = (input[2 * j * stride], input[(m - 1 - 2 * j) * stride]);
            let (c, s) = self.pre[j];
            *zj = (a * c + b * s, a * s - b * c);
        }
        let mut w = vec![(0f32, 0f32); l];
        self.fft.run(&z, &mut w);
        let scale = 1.0 / 32768.0;
        // u = DCT-IV; the half output is -u reversed.
        for (p, &(re, im)) in w.iter().enumerate() {
            let (c, s) = self.post[p];
            let u0 = (re * c - im * s) * scale; // u[2p]
            let u1 = (re * s + im * c) * scale; // u[m-1-2p]
            out[m - 1 - 2 * p] = -u0;
            out[2 * p] = -u1;
        }
    }
}

/// Complex FFT with e^{+i} twiddles, for sizes made of 2, 3, 4 and 5.
struct Fft {
    n: usize,
    factors: Vec<usize>,
    tw: Vec<(f32, f32)>,
}

impl Fft {
    fn new(n: usize) -> Fft {
        let mut factors = Vec::new();
        let mut r = n;
        for p in [4usize, 2, 3, 5] {
            while r % p == 0 {
                factors.push(p);
                r /= p;
            }
        }
        let tw = (0..n)
            .map(|t| {
                let a = 2.0 * PI * t as f32 / n as f32;
                (libm::cosf(a), libm::sinf(a))
            })
            .collect();
        Fft { n, factors, tw }
    }

    fn run(&self, input: &[(f32, f32)], out: &mut [(f32, f32)]) {
        self.rec(input, 0, 1, out, self.n, 0);
    }

    fn rec(&self, input: &[(f32, f32)], off: usize, stride: usize, out: &mut [(f32, f32)], n: usize, fi: usize) {
        if n == 1 {
            out[0] = input[off];
            return;
        }
        let p = self.factors[fi];
        let m = n / p;
        for j in 0..p {
            self.rec(input, off + j * stride, stride * p, &mut out[j * m..(j + 1) * m], m, fi + 1);
        }
        let step = self.n / n; // twiddle step for W_n
        let pstep = self.n / p;
        let mut t = [(0f32, 0f32); 5];
        for k in 0..m {
            for (j, tj) in t.iter_mut().enumerate().take(p) {
                let (a, b) = out[j * m + k];
                let (c, s) = self.tw[(j * k * step) % self.n];
                *tj = (a * c - b * s, a * s + b * c);
            }
            for q in 0..p {
                let (mut re, mut im) = t[0];
                for (j, &(a, b)) in t.iter().enumerate().take(p).skip(1) {
                    let (c, s) = self.tw[((j * q) % p) * pstep];
                    re += a * c - b * s;
                    im += a * s + b * c;
                }
                out[k + q * m] = (re, im);
            }
        }
    }
}

//! FLAC encoder (lossless audio) for the sound in exported videos: 16-bit stereo,
//! fixed-blocksize frames with the best of the fixed predictors and stereo decorrelation.

use alloc::vec;
use alloc::vec::Vec;

pub const BLOCK: usize = 4096;

struct Bw {
    out: Vec<u8>,
    cur: u64,
    n: u32,
}

impl Bw {
    fn put(&mut self, v: u64, len: u32) {
        if len == 0 {
            return;
        }
        if len > 32 {
            self.put(v >> 32, len - 32);
            self.put(v & 0xFFFF_FFFF, 32);
            return;
        }
        self.cur = (self.cur << len) | (v & ((1u64 << len) - 1));
        self.n += len;
        while self.n >= 8 {
            self.n -= 8;
            self.out.push((self.cur >> self.n) as u8);
        }
    }
    fn signed(&mut self, v: i32, len: u32) {
        self.put(v as u32 as u64 & ((1u64 << len) - 1), len);
    }
    fn align(&mut self) {
        if self.n > 0 {
            let p = 8 - self.n;
            self.put(0, p);
        }
    }
}

fn crc8(d: &[u8]) -> u8 {
    let mut c = 0u8;
    for &b in d {
        c ^= b;
        for _ in 0..8 {
            c = if c & 0x80 != 0 { (c << 1) ^ 0x07 } else { c << 1 };
        }
    }
    c
}

fn crc16(d: &[u8]) -> u16 {
    let mut c = 0u16;
    for &b in d {
        c ^= (b as u16) << 8;
        for _ in 0..8 {
            c = if c & 0x8000 != 0 { (c << 1) ^ 0x8005 } else { c << 1 };
        }
    }
    c
}

/// Residual of fixed predictor `order` for samples from `order` on.
fn residual(x: &[i32], order: usize) -> Vec<i32> {
    let mut r = Vec::with_capacity(x.len());
    for i in order..x.len() {
        let p = match order {
            0 => 0,
            1 => x[i - 1],
            2 => 2 * x[i - 1] - x[i - 2],
            3 => 3 * x[i - 1] - 3 * x[i - 2] + x[i - 3],
            _ => 4 * x[i - 1] - 6 * x[i - 2] + 4 * x[i - 3] - x[i - 4],
        };
        r.push(x[i] - p);
    }
    r
}

#[inline]
fn zz(r: i32) -> u32 {
    ((r << 1) ^ (r >> 31)) as u32
}

/// Best Rice parameter and its cost in bits for some residuals.
fn rice_cost(r: &[i32]) -> (u32, u64) {
    let sum: u64 = r.iter().map(|&v| zz(v) as u64).sum();
    let n = r.len().max(1) as u64;
    // Start near log2(mean) and check neighbours.
    let mean = sum / n;
    let guess = if mean == 0 { 0 } else { 63 - mean.leading_zeros() };
    let mut best = (0, u64::MAX);
    for k in guess.saturating_sub(1)..=(guess + 1).min(14) {
        let bits: u64 = r.iter().map(|&v| (zz(v) >> k) as u64 + 1 + k as u64).sum();
        if bits < best.1 {
            best = (k, bits);
        }
    }
    best
}

/// Choose the partition order and parameters: (order, params, bits).
fn partitions(r: &[i32], block: usize, pred_order: usize) -> (u32, Vec<u32>, u64) {
    let mut best: (u32, Vec<u32>, u64) = (0, Vec::new(), u64::MAX);
    for po in 0..=6u32 {
        let parts = 1usize << po;
        if block % parts != 0 || block / parts <= pred_order {
            break;
        }
        let mut params = Vec::with_capacity(parts);
        let mut bits = 0u64;
        let mut at = 0;
        for p in 0..parts {
            let n = block / parts - if p == 0 { pred_order } else { 0 };
            let (k, b) = rice_cost(&r[at..at + n]);
            params.push(k);
            bits += b + 4;
            at += n;
        }
        if bits < best.2 {
            best = (po, params, bits);
        }
    }
    best
}

struct Plan {
    order: usize,
    res: Vec<i32>,
    po: u32,
    params: Vec<u32>,
    bits: u64,
}

fn plan(x: &[i32], bps: u32) -> Plan {
    let mut best: Option<Plan> = None;
    for order in 0..=4usize.min(x.len().saturating_sub(1)) {
        let res = residual(x, order);
        let (po, params, rb) = partitions(&res, x.len(), order);
        let bits = rb + (order as u64) * bps as u64 + 8 + 6;
        if best.as_ref().map(|b| bits < b.bits).unwrap_or(true) {
            best = Some(Plan { order, res, po, params, bits });
        }
    }
    best.unwrap()
}

fn verbatim_bits(n: usize, bps: u32) -> u64 {
    n as u64 * bps as u64 + 8
}

fn subframe(bw: &mut Bw, x: &[i32], bps: u32, p: &Plan) {
    if x.iter().all(|&v| v == x[0]) {
        bw.put(0, 8); // constant
        bw.signed(x[0], bps);
        return;
    }
    if p.bits >= verbatim_bits(x.len(), bps) {
        bw.put(0b0000_0010, 8);
        for &v in x {
            bw.signed(v, bps);
        }
        return;
    }
    bw.put((0b0000_1000 | p.order as u64) << 1, 8);
    for &v in &x[..p.order] {
        bw.signed(v, bps);
    }
    bw.put(0, 2); // Rice, 4-bit parameters
    bw.put(p.po as u64, 4);
    let parts = 1usize << p.po;
    let mut at = 0;
    for (i, &k) in p.params.iter().enumerate() {
        let n = x.len() / parts - if i == 0 { p.order } else { 0 };
        bw.put(k as u64, 4);
        for &r in &p.res[at..at + n] {
            let u = zz(r);
            let q = u >> k;
            // q zeros then a one, then k low bits.
            let mut q = q;
            while q >= 32 {
                bw.put(0, 32);
                q -= 32;
            }
            bw.put(1, q + 1);
            bw.put((u & ((1 << k) - 1)) as u64, k);
        }
        at += n;
    }
}

fn utf8_num(v: u32) -> Vec<u8> {
    if v < 0x80 {
        return vec![v as u8];
    }
    let mut bytes = Vec::new();
    let mut n = v;
    let mut count = 1;
    while n >= (1 << (7 - count)) {
        bytes.push(0x80 | (n & 0x3F) as u8);
        n >>= 6;
        count += 1;
    }
    let lead = (0xFF00u16 >> count) as u8 | n as u8;
    let mut out = vec![lead];
    out.extend(bytes.iter().rev());
    out
}

pub struct Encoder {
    pub rate: u32,
    frame_no: u32,
    pub min_frame: u32,
    pub max_frame: u32,
    pub total: u64,
}

impl Encoder {
    pub fn new(rate: u32) -> Encoder {
        Encoder { rate, frame_no: 0, min_frame: u32::MAX, max_frame: 0, total: 0 }
    }

    /// Encode one block of interleaved 16-bit stereo (up to BLOCK frames; only the last
    /// block of a stream may be shorter).
    pub fn frame(&mut self, pcm: &[i16]) -> Vec<u8> {
        let n = pcm.len() / 2;
        let l: Vec<i32> = (0..n).map(|i| pcm[i * 2] as i32).collect();
        let r: Vec<i32> = (0..n).map(|i| pcm[i * 2 + 1] as i32).collect();
        let m: Vec<i32> = (0..n).map(|i| (l[i] + r[i]) >> 1).collect();
        let s: Vec<i32> = (0..n).map(|i| l[i] - r[i]).collect();
        let pl = plan(&l, 16);
        let pr = plan(&r, 16);
        let pm = plan(&m, 16);
        let ps = plan(&s, 17);
        // Channel assignment: 1 = L/R, 8 = L/S, 9 = S/R, 10 = M/S.
        let opts = [(1u8, pl.bits + pr.bits), (8, pl.bits + ps.bits), (9, ps.bits + pr.bits), (10, pm.bits + ps.bits)];
        let assign = opts.iter().min_by_key(|o| o.1).unwrap().0;

        let mut bw = Bw { out: Vec::new(), cur: 0, n: 0 };
        bw.put(0xFFF8, 16);
        let bs_code = if n == BLOCK { 0b1100 } else { 0b0111 };
        let sr_code = match self.rate {
            44100 => 0b1001,
            48000 => 0b1010,
            _ => 0b1010,
        };
        bw.put(bs_code, 4);
        bw.put(sr_code, 4);
        bw.put(assign as u64, 4);
        bw.put(0b100, 3); // 16 bits
        bw.put(0, 1);
        for b in utf8_num(self.frame_no) {
            bw.put(b as u64, 8);
        }
        if bs_code == 0b0111 {
            bw.put(n as u64 - 1, 16);
        }
        let c = crc8(&bw.out);
        bw.put(c as u64, 8);
        let (a, b, abps, bbps) = match assign {
            1 => ((&l, &pl), (&r, &pr), 16, 16),
            8 => ((&l, &pl), (&s, &ps), 16, 17),
            9 => ((&s, &ps), (&r, &pr), 17, 16),
            _ => ((&m, &pm), (&s, &ps), 16, 17),
        };
        subframe(&mut bw, a.0, abps, a.1);
        subframe(&mut bw, b.0, bbps, b.1);
        bw.align();
        let c = crc16(&bw.out);
        bw.put(c as u64, 16);
        self.frame_no += 1;
        self.total += n as u64;
        let len = bw.out.len() as u32;
        self.min_frame = self.min_frame.min(len);
        self.max_frame = self.max_frame.max(len);
        bw.out
    }

    /// The STREAMINFO block for everything encoded so far.
    pub fn streaminfo(&self) -> [u8; 34] {
        let mut bw = Bw { out: Vec::new(), cur: 0, n: 0 };
        bw.put(BLOCK as u64, 16);
        bw.put(BLOCK as u64, 16);
        bw.put(if self.min_frame == u32::MAX { 0 } else { self.min_frame as u64 }, 24);
        bw.put(self.max_frame as u64, 24);
        bw.put(self.rate as u64, 20);
        bw.put(1, 3); // channels - 1
        bw.put(15, 5); // bits - 1
        bw.put(self.total, 36);
        bw.put(0, 64); // MD5 unknown
        bw.put(0, 64);
        let mut out = [0u8; 34];
        out.copy_from_slice(&bw.out);
        out
    }
}

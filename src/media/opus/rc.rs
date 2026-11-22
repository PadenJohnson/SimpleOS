//! Opus range decoder (RFC 6716 section 4.1), following FFmpeg's opus/rc.c, plus the raw
//! bits CELT reads backwards from the end of the frame.

pub struct Rc<'a> {
    d: &'a [u8],
    bitpos: usize,
    pub range: u32,
    pub value: u32,
    pub total_bits: u32,
    // Raw bits, read backwards from `rb_pos`.
    rb_pos: usize,
    pub rb_bytes: u32,
    rb_cachelen: u32,
    rb_cacheval: u32,
}

/// Number of bits needed for x (0 for 0).
#[inline]
pub fn ilog(x: u32) -> u32 {
    32 - x.leading_zeros()
}

#[inline]
fn log2(x: u32) -> u32 {
    31 - (x | 1).leading_zeros()
}

impl<'a> Rc<'a> {
    pub fn new(d: &'a [u8]) -> Rc<'a> {
        let mut r = Rc { d, bitpos: 0, range: 128, value: 0, total_bits: 9, rb_pos: d.len(), rb_bytes: 0, rb_cachelen: 0, rb_cacheval: 0 };
        r.value = 127 - r.bits(7);
        r.normalize();
        r
    }

    /// MSB-first bits from the front (zeros past the end).
    fn bits(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self.d.get(self.bitpos >> 3).copied().unwrap_or(0);
            v = (v << 1) | ((byte >> (7 - (self.bitpos & 7))) & 1) as u32;
            self.bitpos += 1;
        }
        v
    }

    fn normalize(&mut self) {
        while self.range <= 1 << 23 {
            let b = self.bits(8);
            self.value = ((self.value << 8) | (b ^ 255)) & ((1 << 31) - 1);
            self.range <<= 8;
            self.total_bits += 8;
        }
    }

    fn update(&mut self, scale: u32, low: u32, high: u32, total: u32) {
        self.value = self.value.wrapping_sub(scale.wrapping_mul(total - high));
        self.range = if low != 0 { scale * (high - low) } else { self.range - scale * (total - high) };
        self.normalize();
    }

    pub fn tell(&self) -> i32 {
        self.total_bits as i32 - log2(self.range) as i32 - 1
    }

    pub fn tell_frac(&self) -> i32 {
        let total = self.total_bits << 3;
        let mut rcbuf = log2(self.range) + 1;
        let mut range = self.range >> (rcbuf - 16);
        for _ in 0..3 {
            range = range * range >> 15;
            let bit = range >> 16;
            rcbuf = rcbuf << 1 | bit;
            range >>= bit;
        }
        total as i32 - rcbuf as i32
    }

    /// Symbol from a table: cdf[0] = total, then cumulative ends.
    pub fn cdf(&mut self, cdf: &[u16]) -> usize {
        let total = cdf[0] as u32;
        let cdf = &cdf[1..];
        let scale = self.range / total;
        let sym = total - (self.value / scale + 1).min(total);
        let mut k = 0;
        while (cdf[k] as u32) <= sym {
            k += 1;
        }
        let high = cdf[k] as u32;
        let low = if k > 0 { cdf[k - 1] as u32 } else { 0 };
        self.update(scale, low, high, total);
        k
    }

    pub fn log(&mut self, bits: u32) -> u32 {
        let scale = self.range >> bits;
        let k = if self.value >= scale {
            self.value -= scale;
            self.range -= scale;
            0
        } else {
            self.range = scale;
            1
        };
        self.normalize();
        k
    }

    pub fn raw_init(&mut self, end: usize, bytes: u32) {
        self.rb_pos = end;
        self.rb_bytes = bytes;
        self.rb_cachelen = 0;
        self.rb_cacheval = 0;
    }

    pub fn raw(&mut self, count: u32) -> u32 {
        while self.rb_bytes > 0 && self.rb_cachelen < count {
            self.rb_pos -= 1;
            self.rb_cacheval |= (self.d.get(self.rb_pos).copied().unwrap_or(0) as u32) << self.rb_cachelen;
            self.rb_cachelen += 8;
            self.rb_bytes -= 1;
        }
        let v = if count >= 32 { self.rb_cacheval } else { self.rb_cacheval & ((1u32 << count) - 1) };
        self.rb_cacheval = if count >= 32 { 0 } else { self.rb_cacheval >> count };
        self.rb_cachelen = self.rb_cachelen.wrapping_sub(count);
        self.total_bits += count;
        v
    }

    pub fn uint(&mut self, size: u32) -> u32 {
        let bits = ilog(size - 1);
        let total = if bits > 8 { ((size - 1) >> (bits - 8)) + 1 } else { size };
        let scale = self.range / total;
        let k = total - (self.value / scale + 1).min(total);
        self.update(scale, k, k + 1, total);
        if bits > 8 {
            let k = k << (bits - 8) | self.raw(bits - 8);
            k.min(size - 1)
        } else {
            k
        }
    }

    pub fn uint_step(&mut self, k0: u32) -> u32 {
        let total = (k0 + 1) * 3 + k0;
        let scale = self.range / total;
        let sym = total - (self.value / scale + 1).min(total);
        let k = if sym < (k0 + 1) * 3 { sym / 3 } else { sym - (k0 + 1) * 2 };
        let (lo, hi) = if k <= k0 { (3 * k, 3 * (k + 1)) } else { ((k - 1 - k0) + 3 * (k0 + 1), (k - k0) + 3 * (k0 + 1)) };
        self.update(scale, lo, hi, total);
        k
    }

    pub fn uint_tri(&mut self, qn: u32) -> u32 {
        let total = ((qn >> 1) + 1) * ((qn >> 1) + 1);
        let scale = self.range / total;
        let center = total - (self.value / scale + 1).min(total);
        let (k, low, sym);
        if center < total >> 1 {
            k = (isqrt(8 * center + 1) - 1) >> 1;
            low = k * (k + 1) >> 1;
            sym = k + 1;
        } else {
            k = (2 * (qn + 1) - isqrt(8 * (total - center - 1) + 1)) >> 1;
            low = total - ((qn + 1 - k) * (qn + 2 - k) >> 1);
            sym = qn + 1 - k;
        }
        self.update(scale, low, low + sym, total);
        k
    }

    pub fn laplace(&mut self, mut symbol: u32, decay: i32) -> i32 {
        let mut value = 0i32;
        let mut low = 0u32;
        let scale = self.range >> 15;
        let center = (1u32 << 15) - (self.value / scale + 1).min(1 << 15);
        if center >= symbol {
            value += 1;
            low = symbol;
            symbol = 1 + ((32768 - 32 - symbol) * (16384 - decay) as u32 >> 15);
            while symbol > 1 && center >= low + 2 * symbol {
                value += 1;
                symbol *= 2;
                low += symbol;
                symbol = (((symbol - 2) as i64 * decay as i64) >> 15) as u32 + 1;
            }
            if symbol <= 1 {
                let distance = (center - low) >> 1;
                value += distance as i32;
                low += 2 * distance;
            }
            if center < low + symbol {
                value = -value;
            } else {
                low += symbol;
            }
        }
        self.update(scale, low, (low + symbol).min(32768), 32768);
        value
    }
}

/// Integer square root (floor).
pub fn isqrt(x: u32) -> u32 {
    let mut r = libm::sqrt(x as f64) as u32;
    while r * r > x {
        r -= 1;
    }
    while (r + 1) * (r + 1) <= x {
        r += 1;
    }
    r
}

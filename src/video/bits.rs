//! Bit reading for H.264 and AAC.

use alloc::vec::Vec;

/// Remove H.264 emulation-prevention bytes (00 00 03 -> 00 00).
pub fn unescape(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        out.push(b);
        zeros = if b == 0 { zeros + 1 } else { 0 };
    }
    out
}

pub struct Bits<'a> {
    d: &'a [u8],
    pub pos: usize,
}

impl<'a> Bits<'a> {
    pub fn new(d: &'a [u8]) -> Bits<'a> {
        Bits { d, pos: 0 }
    }
    #[inline]
    pub fn bit(&mut self) -> u32 {
        let b = self.pos >> 3;
        let v = if b < self.d.len() { (self.d[b] >> (7 - (self.pos & 7))) & 1 } else { 0 };
        self.pos += 1;
        v as u32
    }
    #[inline]
    pub fn flag(&mut self) -> bool {
        self.bit() != 0
    }
    /// The next 32 bits (zeros past the end), without consuming them.
    #[inline]
    fn peek32(&self) -> u32 {
        let b = self.pos >> 3;
        let mut v: u64 = 0;
        if b + 5 <= self.d.len() {
            for k in 0..5 {
                v = (v << 8) | self.d[b + k] as u64;
            }
        } else {
            for k in 0..5 {
                v = (v << 8) | *self.d.get(b + k).unwrap_or(&0) as u64;
            }
        }
        ((v << (self.pos & 7)) >> 8) as u32
    }
    /// Up to 32 bits.
    #[inline]
    pub fn read(&mut self, n: u32) -> u32 {
        if n == 0 {
            return 0;
        }
        let v = self.peek32() >> (32 - n);
        self.pos += n as usize;
        v
    }
    /// Look at the next `n` (<= 32) bits without consuming them.
    #[inline]
    pub fn peek(&self, n: u32) -> u32 {
        if n == 0 {
            0
        } else {
            self.peek32() >> (32 - n)
        }
    }
    pub fn skip(&mut self, n: usize) {
        self.pos += n;
    }
    pub fn ue(&mut self) -> u32 {
        let mut zeros = 0;
        while self.bit() == 0 {
            zeros += 1;
            if zeros > 31 {
                return 0;
            }
        }
        if zeros == 0 {
            return 0;
        }
        ((1u64 << zeros) - 1 + self.read(zeros) as u64) as u32
    }
    pub fn se(&mut self) -> i32 {
        let k = self.ue();
        if k & 1 == 1 {
            k.div_ceil(2) as i32
        } else {
            -((k / 2) as i32)
        }
    }
    pub fn byte_aligned(&self) -> bool {
        self.pos & 7 == 0
    }
    pub fn align(&mut self) {
        self.pos = (self.pos + 7) & !7;
    }
    pub fn left(&self) -> isize {
        (self.d.len() * 8) as isize - self.pos as isize
    }
    /// More data before the RBSP stop bit?
    pub fn more_rbsp_data(&self) -> bool {
        let total = self.d.len() * 8;
        if self.pos >= total {
            return false;
        }
        // Find the last 1 bit (the stop bit).
        let mut last = total;
        for (i, &b) in self.d.iter().enumerate().rev() {
            if b != 0 {
                last = i * 8 + 7 - b.trailing_zeros() as usize;
                break;
            }
        }
        self.pos < last
    }
    pub fn data(&self) -> &'a [u8] {
        self.d
    }
}

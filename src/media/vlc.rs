//! Reading variable-length codes (Huffman-style tables given as code, length and value), with
//! lookup tables in two or more levels so long codes don't need huge tables.

use alloc::vec::Vec;

use crate::video::bits::Bits;

/// Bits looked at in the first table.
const ROOT: u32 = 9;
/// Bits per later level.
const SUB: u32 = 6;

#[derive(Clone, Copy)]
struct Entry {
    /// The value, or (for `len < 0`) where the next table starts.
    sym: i32,
    /// Bits used (> 0), the next table's size in bits (< 0), or 0 for a code that isn't there.
    len: i8,
}

pub struct Vlc {
    table: Vec<Entry>,
}

impl Vlc {
    /// `codes`: (code, length in bits, value); lengths up to 32.
    pub fn new(codes: &[(u32, u8, i32)]) -> Vlc {
        let mut v = Vlc { table: Vec::new() };
        let list: Vec<(u32, u8, i32)> = codes.iter().copied().filter(|c| c.1 > 0).collect();
        v.build(&list, ROOT);
        v
    }

    /// Build a table for codes (with their already-used leading bits removed); returns its start.
    fn build(&mut self, codes: &[(u32, u8, i32)], bits: u32) -> usize {
        let start = self.table.len();
        self.table.extend(core::iter::repeat_n(Entry { sym: 0, len: 0 }, 1 << bits));
        // Short codes fill every entry they're a prefix of.
        for &(code, len, sym) in codes {
            let len = len as u32;
            if len <= bits {
                let first = (code << (bits - len)) as usize;
                for e in &mut self.table[start + first..start + first + (1 << (bits - len))] {
                    *e = Entry { sym, len: len as i8 };
                }
            }
        }
        // Long codes go into a table per leading `bits` bits.
        let mut longs: Vec<(u32, u8, i32)> = codes.iter().copied().filter(|c| c.1 as u32 > bits).collect();
        longs.sort_by_key(|c| c.0 >> (c.1 as u32 - bits));
        let mut i = 0;
        while i < longs.len() {
            let prefix = longs[i].0 >> (longs[i].1 as u32 - bits);
            let mut j = i;
            let mut longest = 0;
            while j < longs.len() && longs[j].0 >> (longs[j].1 as u32 - bits) == prefix {
                longest = longest.max(longs[j].1 as u32 - bits);
                j += 1;
            }
            let sub_bits = longest.min(SUB);
            let rest: Vec<(u32, u8, i32)> = longs[i..j]
                .iter()
                .map(|&(code, len, sym)| {
                    let l = len as u32 - bits;
                    (code & ((1u32 << l) - 1), l as u8, sym)
                })
                .collect();
            let at = self.build(&rest, sub_bits);
            self.table[start + prefix as usize] = Entry { sym: at as i32, len: -(sub_bits as i8) };
            i = j;
        }
        start
    }

    /// The next value, or None for bits that aren't a code.
    #[inline]
    pub fn read(&self, b: &mut Bits) -> Option<i32> {
        let mut base = 0usize;
        let mut bits = ROOT;
        loop {
            let e = self.table[base + b.peek(bits) as usize];
            if e.len > 0 {
                b.skip(e.len as usize);
                return Some(e.sym);
            }
            if e.len == 0 {
                return None;
            }
            b.skip(bits as usize);
            base = e.sym as usize;
            bits = (-e.len) as u32;
        }
    }
}

/// Codes from parallel (code, length) pairs, valued by position.
pub fn from_pairs<T: Copy + Into<u32>>(pairs: &[[T; 2]]) -> Vec<(u32, u8, i32)> {
    pairs.iter().enumerate().map(|(i, p)| (p[0].into(), p[1].into() as u8, i as i32)).collect()
}

/// Codes from separate code and length lists, valued by position.
pub fn from_lists<C: Copy + Into<u32>, L: Copy + Into<u32>>(codes: &[C], lens: &[L]) -> Vec<(u32, u8, i32)> {
    codes.iter().zip(lens).enumerate().map(|(i, (c, l))| ((*c).into(), (*l).into() as u8, i as i32)).collect()
}

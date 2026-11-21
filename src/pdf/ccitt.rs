//! CCITT fax compression (Group 3 one- and two-dimensional, Group 4): the black-and-white
//! pictures many scanned PDFs are made of. Decodes to 1 bit per pixel rows.

use alloc::vec;
use alloc::vec::Vec;

// (code, length, run) — terminating codes are runs 0..63, the rest are make-up codes.
const WHITE: &[(u16, u8, u16)] = &[
    (0b00110101, 8, 0), (0b000111, 6, 1), (0b0111, 4, 2), (0b1000, 4, 3), (0b1011, 4, 4), (0b1100, 4, 5), (0b1110, 4, 6), (0b1111, 4, 7),
    (0b10011, 5, 8), (0b10100, 5, 9), (0b00111, 5, 10), (0b01000, 5, 11), (0b001000, 6, 12), (0b000011, 6, 13), (0b110100, 6, 14), (0b110101, 6, 15),
    (0b101010, 6, 16), (0b101011, 6, 17), (0b0100111, 7, 18), (0b0001100, 7, 19), (0b0001000, 7, 20), (0b0010111, 7, 21), (0b0000011, 7, 22), (0b0000100, 7, 23),
    (0b0101000, 7, 24), (0b0101011, 7, 25), (0b0010011, 7, 26), (0b0100100, 7, 27), (0b0011000, 7, 28), (0b00000010, 8, 29), (0b00000011, 8, 30), (0b00011010, 8, 31),
    (0b00011011, 8, 32), (0b00010010, 8, 33), (0b00010011, 8, 34), (0b00010100, 8, 35), (0b00010101, 8, 36), (0b00010110, 8, 37), (0b00010111, 8, 38), (0b00101000, 8, 39),
    (0b00101001, 8, 40), (0b00101010, 8, 41), (0b00101011, 8, 42), (0b00101100, 8, 43), (0b00101101, 8, 44), (0b00000100, 8, 45), (0b00000101, 8, 46), (0b00001010, 8, 47),
    (0b00001011, 8, 48), (0b01010010, 8, 49), (0b01010011, 8, 50), (0b01010100, 8, 51), (0b01010101, 8, 52), (0b00100100, 8, 53), (0b00100101, 8, 54), (0b01011000, 8, 55),
    (0b01011001, 8, 56), (0b01011010, 8, 57), (0b01011011, 8, 58), (0b01001010, 8, 59), (0b01001011, 8, 60), (0b00110010, 8, 61), (0b00110011, 8, 62), (0b00110100, 8, 63),
    (0b11011, 5, 64), (0b10010, 5, 128), (0b010111, 6, 192), (0b0110111, 7, 256), (0b00110110, 8, 320), (0b00110111, 8, 384), (0b01100100, 8, 448), (0b01100101, 8, 512),
    (0b01101000, 8, 576), (0b01100111, 8, 640), (0b011001100, 9, 704), (0b011001101, 9, 768), (0b011010010, 9, 832), (0b011010011, 9, 896), (0b011010100, 9, 960), (0b011010101, 9, 1024),
    (0b011010110, 9, 1088), (0b011010111, 9, 1152), (0b011011000, 9, 1216), (0b011011001, 9, 1280), (0b011011010, 9, 1344), (0b011011011, 9, 1408), (0b010011000, 9, 1472), (0b010011001, 9, 1536),
    (0b010011010, 9, 1600), (0b011000, 6, 1664), (0b010011011, 9, 1728),
];

const BLACK: &[(u16, u8, u16)] = &[
    (0b0000110111, 10, 0), (0b010, 3, 1), (0b11, 2, 2), (0b10, 2, 3), (0b011, 3, 4), (0b0011, 4, 5), (0b0010, 4, 6), (0b00011, 5, 7),
    (0b000101, 6, 8), (0b000100, 6, 9), (0b0000100, 7, 10), (0b0000101, 7, 11), (0b0000111, 7, 12), (0b00000100, 8, 13), (0b00000111, 8, 14), (0b000011000, 9, 15),
    (0b0000010111, 10, 16), (0b0000011000, 10, 17), (0b0000001000, 10, 18), (0b00001100111, 11, 19), (0b00001101000, 11, 20), (0b00001101100, 11, 21), (0b00000110111, 11, 22), (0b00000101000, 11, 23),
    (0b00000010111, 11, 24), (0b00000011000, 11, 25), (0b000011001010, 12, 26), (0b000011001011, 12, 27), (0b000011001100, 12, 28), (0b000011001101, 12, 29), (0b000001101000, 12, 30), (0b000001101001, 12, 31),
    (0b000001101010, 12, 32), (0b000001101011, 12, 33), (0b000011010010, 12, 34), (0b000011010011, 12, 35), (0b000011010100, 12, 36), (0b000011010101, 12, 37), (0b000011010110, 12, 38), (0b000011010111, 12, 39),
    (0b000001101100, 12, 40), (0b000001101101, 12, 41), (0b000011011010, 12, 42), (0b000011011011, 12, 43), (0b000001010100, 12, 44), (0b000001010101, 12, 45), (0b000001010110, 12, 46), (0b000001010111, 12, 47),
    (0b000001100100, 12, 48), (0b000001100101, 12, 49), (0b000001010010, 12, 50), (0b000001010011, 12, 51), (0b000000100100, 12, 52), (0b000000110111, 12, 53), (0b000000111000, 12, 54), (0b000000100111, 12, 55),
    (0b000000101000, 12, 56), (0b000001011000, 12, 57), (0b000001011001, 12, 58), (0b000000101011, 12, 59), (0b000000101100, 12, 60), (0b000001011010, 12, 61), (0b000001100110, 12, 62), (0b000001100111, 12, 63),
    (0b0000001111, 10, 64), (0b000011001000, 12, 128), (0b000011001001, 12, 192), (0b000001011011, 12, 256), (0b000000110011, 12, 320), (0b000000110100, 12, 384), (0b000000110101, 12, 448), (0b0000001101100, 13, 512),
    (0b0000001101101, 13, 576), (0b0000001001010, 13, 640), (0b0000001001011, 13, 704), (0b0000001001100, 13, 768), (0b0000001001101, 13, 832), (0b0000001110010, 13, 896), (0b0000001110011, 13, 960), (0b0000001110100, 13, 1024),
    (0b0000001110101, 13, 1088), (0b0000001110110, 13, 1152), (0b0000001110111, 13, 1216), (0b0000001010010, 13, 1280), (0b0000001010011, 13, 1344), (0b0000001010100, 13, 1408), (0b0000001010101, 13, 1472), (0b0000001011010, 13, 1536),
    (0b0000001011011, 13, 1600), (0b0000001100100, 13, 1664), (0b0000001100101, 13, 1728),
];

/// Make-up codes shared by both colours.
const EXT: &[(u16, u8, u16)] = &[
    (0b00000001000, 11, 1792), (0b00000001100, 11, 1856), (0b00000001101, 11, 1920), (0b000000010010, 12, 1984), (0b000000010011, 12, 2048), (0b000000010100, 12, 2112), (0b000000010101, 12, 2176),
    (0b000000010110, 12, 2240), (0b000000010111, 12, 2304), (0b000000011100, 12, 2368), (0b000000011101, 12, 2432), (0b000000011110, 12, 2496), (0b000000011111, 12, 2560),
];

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn peek(&self, n: u8) -> u32 {
        let mut v = 0u32;
        for k in 0..n as usize {
            let p = self.pos + k;
            let bit = self.d.get(p / 8).map(|b| (b >> (7 - p % 8)) & 1).unwrap_or(0);
            v = v << 1 | bit as u32;
        }
        v
    }
    fn skip(&mut self, n: u8) {
        self.pos += n as usize;
    }
    fn bit(&mut self) -> u32 {
        let v = self.peek(1);
        self.pos += 1;
        v
    }
    fn done(&self) -> bool {
        self.pos >= self.d.len() * 8
    }
    fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }
    /// Skip an end-of-line code (eleven or more 0s then a 1) if one is here.
    fn eol(&mut self) -> bool {
        if self.peek(11) != 0 {
            return false;
        }
        let mut p = self.pos + 11;
        while p < self.d.len() * 8 {
            let bit = (self.d[p / 8] >> (7 - p % 8)) & 1;
            p += 1;
            if bit == 1 {
                self.pos = p;
                return true;
            }
        }
        self.pos = p;
        true
    }
}

/// One run length of the given colour (make-up codes followed by a terminating code).
fn run(b: &mut Bits, black: bool) -> Option<usize> {
    let table = if black { BLACK } else { WHITE };
    let mut total = 0usize;
    loop {
        let mut found = None;
        'len: for len in 2..=13u8 {
            let v = b.peek(len) as u16;
            for &(code, l, r) in table.iter().chain(EXT) {
                if l == len && code == v {
                    found = Some((l, r));
                    break 'len;
                }
            }
        }
        let (l, r) = found?;
        b.skip(l);
        total += r as usize;
        if r < 64 {
            return Some(total);
        }
    }
}

enum Mode {
    Pass,
    Horiz,
    Vert(i32),
}

fn mode(b: &mut Bits) -> Option<Mode> {
    let m = if b.peek(1) == 1 {
        (1, Mode::Vert(0))
    } else if b.peek(3) == 0b011 {
        (3, Mode::Vert(1))
    } else if b.peek(3) == 0b010 {
        (3, Mode::Vert(-1))
    } else if b.peek(3) == 0b001 {
        (3, Mode::Horiz)
    } else if b.peek(4) == 0b0001 {
        (4, Mode::Pass)
    } else if b.peek(6) == 0b000011 {
        (6, Mode::Vert(2))
    } else if b.peek(6) == 0b000010 {
        (6, Mode::Vert(-2))
    } else if b.peek(7) == 0b0000011 {
        (7, Mode::Vert(3))
    } else if b.peek(7) == 0b0000010 {
        (7, Mode::Vert(-3))
    } else {
        return None;
    };
    b.skip(m.0);
    Some(m.1)
}

/// Decode to rows of `columns` pixels, 8 to a byte, MSB first. A set bit is white unless
/// `black_is_1` (then a set bit is black), as PDF expects.
pub fn decode(data: &[u8], k: i64, columns: usize, rows: usize, black_is_1: bool, byte_align: bool) -> Vec<u8> {
    let columns = columns.clamp(1, 20000);
    let stride = columns.div_ceil(8);
    let max_rows = if rows > 0 { rows } else { 20000 };
    let mut out = Vec::with_capacity(stride * rows.max(1));
    let mut b = Bits { d: data, pos: 0 };
    // Changing elements of the reference line (starts as an all-white line).
    let mut refl: Vec<usize> = vec![columns, columns];
    let mut row = 0;
    while row < max_rows && !b.done() {
        if byte_align && k < 0 {
            b.align();
        }
        // Group 3: optional end-of-line codes; with K > 0 a tag bit says 1D or 2D.
        let mut two_d = k < 0;
        if k >= 0 {
            if byte_align {
                // Fill bits before the EOL are zeros; eol() skips them.
            }
            let had_eol = b.eol();
            if k > 0 {
                two_d = b.bit() == 0;
            }
            if !had_eol && b.done() {
                break;
            }
        } else if b.peek(12) == 1 {
            break; // EOFB
        }
        let mut cur: Vec<usize> = Vec::with_capacity(refl.len());
        let mut ok = true;
        if two_d {
            let mut a0: isize = -1;
            let mut white = true;
            while (a0 as i64) < columns as i64 {
                // b1: first changing element on the reference line right of a0, of the colour
                // opposite to a0's (even indices are where black starts).
                let mut i = 0;
                while i < refl.len() && ((refl[i] as isize) <= a0 || (i % 2 == 0) != white) {
                    i += 1;
                }
                let b1 = refl.get(i).copied().unwrap_or(columns);
                let b2 = refl.get(i + 1).copied().unwrap_or(columns);
                match mode(&mut b) {
                    Some(Mode::Pass) => a0 = b2 as isize,
                    Some(Mode::Horiz) => {
                        let start = a0.max(0) as usize;
                        let (Some(r1), Some(r2)) = (run(&mut b, !white), run(&mut b, white)) else {
                            ok = false;
                            break;
                        };
                        let a1 = (start + r1).min(columns);
                        let a2 = (a1 + r2).min(columns);
                        cur.push(a1);
                        cur.push(a2);
                        a0 = a2 as isize;
                    }
                    Some(Mode::Vert(d)) => {
                        let a1 = (b1 as i64 + d as i64).clamp(0, columns as i64) as usize;
                        cur.push(a1);
                        a0 = a1 as isize;
                        white = !white;
                    }
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
        } else {
            let mut x = 0usize;
            let mut white = true;
            while x < columns {
                let Some(r) = run(&mut b, !white) else {
                    ok = false;
                    break;
                };
                x = (x + r).min(columns);
                cur.push(x);
                white = !white;
            }
        }
        // Paint the row.
        let mut line = vec![0u8; stride];
        let mut x = 0usize;
        let mut black = false;
        for &c in cur.iter().chain(core::iter::once(&columns)) {
            let c = c.min(columns);
            if black {
                for p in x..c {
                    line[p / 8] |= 0x80 >> (p % 8);
                }
            }
            x = x.max(c);
            black = !black;
        }
        // `line` has black set; flip unless black is 1.
        if !black_is_1 {
            for v in line.iter_mut() {
                *v = !*v;
            }
        }
        out.extend_from_slice(&line);
        row += 1;
        if !ok {
            break;
        }
        cur.push(columns);
        cur.push(columns);
        refl = cur;
    }
    // Missing rows stay white.
    while rows > 0 && out.len() < stride * rows {
        out.push(if black_is_1 { 0 } else { 0xFF });
    }
    out
}

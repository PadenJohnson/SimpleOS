//! Lossless WebP (VP8L): prefix codes, LZ77 back references, colour cache, and the four
//! transforms (predictor, cross-colour, subtract-green, colour indexing).

use alloc::vec;
use alloc::vec::Vec;

const DIST: [(i8, i8); 120] = [
    (0, 1), (1, 0), (1, 1), (-1, 1), (0, 2), (2, 0), (1, 2), (-1, 2), (2, 1), (-2, 1), (2, 2), (-2, 2), (0, 3), (3, 0), (1, 3), (-1, 3), (3, 1), (-3, 1), (2, 3), (-2, 3), (3, 2), (-3, 2), (0, 4), (4, 0),
    (1, 4), (-1, 4), (4, 1), (-4, 1), (3, 3), (-3, 3), (2, 4), (-2, 4), (4, 2), (-4, 2), (0, 5), (3, 4), (-3, 4), (4, 3), (-4, 3), (5, 0), (1, 5), (-1, 5), (5, 1), (-5, 1), (2, 5), (-2, 5), (5, 2), (-5, 2),
    (4, 4), (-4, 4), (3, 5), (-3, 5), (5, 3), (-5, 3), (0, 6), (6, 0), (1, 6), (-1, 6), (6, 1), (-6, 1), (2, 6), (-2, 6), (6, 2), (-6, 2), (4, 5), (-4, 5), (5, 4), (-5, 4), (3, 6), (-3, 6), (6, 3), (-6, 3),
    (0, 7), (7, 0), (1, 7), (-1, 7), (5, 5), (-5, 5), (7, 1), (-7, 1), (4, 6), (-4, 6), (6, 4), (-6, 4), (2, 7), (-2, 7), (7, 2), (-7, 2), (3, 7), (-3, 7), (7, 3), (-7, 3), (5, 6), (-5, 6), (6, 5), (-6, 5),
    (8, 0), (4, 7), (-4, 7), (7, 4), (-7, 4), (8, 1), (8, 2), (6, 6), (-6, 6), (8, 3), (5, 7), (-5, 7), (7, 5), (-7, 5), (8, 4), (6, 7), (-6, 7), (7, 6), (-7, 6), (8, 5), (7, 7), (-7, 7), (8, 6), (8, 7),
];
const CL_ORDER: [usize; 19] = [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    #[inline]
    fn bit(&mut self) -> Result<u32, &'static str> {
        let b = *self.d.get(self.pos >> 3).ok_or("Damaged WebP file")?;
        let v = (b >> (self.pos & 7)) & 1;
        self.pos += 1;
        Ok(v as u32)
    }
    fn get(&mut self, n: u32) -> Result<u32, &'static str> {
        let mut v = 0;
        for k in 0..n {
            v |= self.bit()? << k;
        }
        Ok(v)
    }
}

/// A canonical prefix code (DEFLATE-style bit order).
#[derive(Clone, Default)]
struct Huff {
    /// Single symbol (no bits).
    single: Option<u16>,
    count: [u16; 16],
    syms: Vec<u16>,
}

impl Huff {
    fn from_lengths(lens: &[u8]) -> Result<Huff, &'static str> {
        let nz: Vec<usize> = (0..lens.len()).filter(|&i| lens[i] != 0).collect();
        if nz.is_empty() {
            return Err("Damaged WebP file");
        }
        if nz.len() == 1 {
            return Ok(Huff { single: Some(nz[0] as u16), ..Huff::default() });
        }
        let mut h = Huff::default();
        for &l in lens {
            h.count[l as usize] += 1;
        }
        h.count[0] = 0;
        for l in 1..16 {
            for (i, &x) in lens.iter().enumerate() {
                if x as usize == l {
                    h.syms.push(i as u16);
                }
            }
        }
        Ok(h)
    }
    fn simple(syms: &[u16]) -> Huff {
        if syms.len() == 1 {
            Huff { single: Some(syms[0]), ..Huff::default() }
        } else {
            let mut h = Huff::default();
            h.count[1] = 2;
            h.syms = syms.to_vec();
            h
        }
    }
    #[inline]
    fn read(&self, b: &mut Bits) -> Result<u16, &'static str> {
        if let Some(s) = self.single {
            return Ok(s);
        }
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= b.bit()? as i32;
            let count = self.count[len] as i32;
            if code - count < first {
                return self.syms.get((index + (code - first)) as usize).copied().ok_or("Damaged WebP file");
            }
            index += count;
            first += count;
            first <<= 1;
            code <<= 1;
        }
        Err("Damaged WebP file")
    }
}

fn read_code(b: &mut Bits, alphabet: usize) -> Result<Huff, &'static str> {
    if b.bit()? == 1 {
        let n = b.bit()? + 1;
        let first8 = b.bit()?;
        let s0 = b.get(1 + 7 * first8)? as u16;
        if n == 2 {
            let s1 = b.get(8)? as u16;
            return Ok(Huff::simple(&[s0, s1]));
        }
        return Ok(Huff::simple(&[s0]));
    }
    let num = 4 + b.get(4)? as usize;
    let mut cl = [0u8; 19];
    for &k in CL_ORDER.iter().take(num) {
        cl[k] = b.get(3)? as u8;
    }
    let clh = Huff::from_lengths(&cl)?;
    let mut max_symbol = if b.bit()? == 1 {
        let bits = 2 + 2 * b.get(3)?;
        2 + b.get(bits)? as usize
    } else {
        alphabet
    };
    if max_symbol > alphabet {
        return Err("Damaged WebP file");
    }
    let mut lens = vec![0u8; alphabet];
    let mut sym = 0;
    let mut prev = 8u8;
    while sym < alphabet {
        if max_symbol == 0 {
            break;
        }
        max_symbol -= 1;
        let c = clh.read(b)?;
        if c < 16 {
            lens[sym] = c as u8;
            sym += 1;
            if c != 0 {
                prev = c as u8;
            }
        } else {
            let (rep, val) = match c {
                16 => (3 + b.get(2)? as usize, prev),
                17 => (3 + b.get(3)? as usize, 0),
                _ => (11 + b.get(7)? as usize, 0),
            };
            if sym + rep > alphabet {
                return Err("Damaged WebP file");
            }
            for _ in 0..rep {
                lens[sym] = val;
                sym += 1;
            }
        }
    }
    Huff::from_lengths(&lens)
}

fn prefix_value(b: &mut Bits, prefix: u32) -> Result<usize, &'static str> {
    if prefix < 4 {
        return Ok(prefix as usize + 1);
    }
    let extra = (prefix - 2) >> 1;
    let offset = (2 + (prefix & 1)) << extra;
    Ok((offset + b.get(extra)?) as usize + 1)
}

fn div_up(v: usize, bits: u32) -> usize {
    (v + (1 << bits) - 1) >> bits
}

enum Transform {
    Predictor(u32, Vec<u32>),
    Cross(u32, Vec<u32>),
    SubGreen,
    Index(u32, Vec<u32>),
}

/// Entropy-coded pixels of a w x h image (with meta codes only for the main image).
fn entropy_image(b: &mut Bits, w: usize, h: usize, main: bool) -> Result<Vec<u32>, &'static str> {
    let cache_bits = if b.bit()? == 1 {
        let c = b.get(4)?;
        if !(1..=11).contains(&c) {
            return Err("Damaged WebP file");
        }
        c
    } else {
        0
    };
    let mut meta_bits = 0;
    let mut meta: Vec<u32> = Vec::new();
    let mut groups = 1;
    if main && b.bit()? == 1 {
        meta_bits = b.get(3)? + 2;
        let (mw, mh) = (div_up(w, meta_bits), div_up(h, meta_bits));
        meta = entropy_image(b, mw, mh, false)?;
        for m in meta.iter_mut() {
            *m = (*m >> 8) & 0xFFFF;
            groups = groups.max(*m as usize + 1);
        }
    }
    let cache_size = if cache_bits > 0 { 1usize << cache_bits } else { 0 };
    let mut codes: Vec<[Huff; 5]> = Vec::with_capacity(groups);
    for _ in 0..groups {
        let g = 256 + 24 + cache_size;
        codes.push([read_code(b, g)?, read_code(b, 256)?, read_code(b, 256)?, read_code(b, 256)?, read_code(b, 40)?]);
    }
    let mut cache = vec![0u32; cache_size.max(1)];
    let mut out = vec![0u32; w * h];
    let mw = if meta_bits > 0 { div_up(w, meta_bits) } else { 1 };
    let mut i = 0;
    let put_cache = |cache: &mut [u32], c: u32| {
        if cache_bits > 0 {
            cache[(0x1E35_A7BDu32.wrapping_mul(c) >> (32 - cache_bits)) as usize] = c;
        }
    };
    while i < w * h {
        let (x, y) = (i % w, i / w);
        let g = if meta_bits > 0 { meta[(y >> meta_bits) * mw + (x >> meta_bits)] as usize } else { 0 };
        let c = &codes[g];
        let s = c[0].read(b)? as u32;
        if s < 256 {
            let r = c[1].read(b)? as u32;
            let bl = c[2].read(b)? as u32;
            let a = c[3].read(b)? as u32;
            let p = a << 24 | r << 16 | s << 8 | bl;
            out[i] = p;
            put_cache(&mut cache, p);
            i += 1;
        } else if s < 280 {
            let len = prefix_value(b, s - 256)?;
            let dsym = c[4].read(b)? as u32;
            let dcode = prefix_value(b, dsym)?;
            let dist = if dcode > 120 {
                dcode - 120
            } else {
                let (dx, dy) = DIST[dcode - 1];
                (dx as isize + dy as isize * w as isize).max(1) as usize
            };
            if dist > i {
                return Err("Damaged WebP file");
            }
            for _ in 0..len {
                if i >= w * h {
                    break;
                }
                let p = out[i - dist];
                out[i] = p;
                put_cache(&mut cache, p);
                i += 1;
            }
        } else {
            let k = (s - 280) as usize;
            if k >= cache_size {
                return Err("Damaged WebP file");
            }
            let p = cache[k];
            out[i] = p;
            put_cache(&mut cache, p);
            i += 1;
        }
    }
    Ok(out)
}

#[inline]
fn add(a: u32, b: u32) -> u32 {
    let ag = (a & 0xFF00_FF00).wrapping_add(b & 0xFF00_FF00) & 0xFF00_FF00;
    let rb = (a & 0x00FF_00FF).wrapping_add(b & 0x00FF_00FF) & 0x00FF_00FF;
    ag | rb
}
#[inline]
fn avg2(a: u32, b: u32) -> u32 {
    (((a ^ b) & 0xFEFE_FEFE) >> 1) + (a & b)
}
fn ch(p: u32, s: u32) -> i32 {
    ((p >> s) & 255) as i32
}
fn select(l: u32, t: u32, tl: u32) -> u32 {
    let pl: i32 = [24, 16, 8, 0].iter().map(|&s| (ch(t, s) - ch(tl, s)).abs()).sum();
    let pt: i32 = [24, 16, 8, 0].iter().map(|&s| (ch(l, s) - ch(tl, s)).abs()).sum();
    if pl < pt {
        l
    } else {
        t
    }
}
fn clamp_full(a: u32, b: u32, c: u32) -> u32 {
    [24, 16, 8, 0].iter().fold(0, |acc, &s| acc | ((ch(a, s) + ch(b, s) - ch(c, s)).clamp(0, 255) as u32) << s)
}
fn clamp_half(a: u32, b: u32) -> u32 {
    [24, 16, 8, 0].iter().fold(0, |acc, &s| {
        let (x, y) = (ch(a, s), ch(b, s));
        acc | ((x + (x - y) / 2).clamp(0, 255) as u32) << s
    })
}

fn delta(t: u32, c: u32) -> i32 {
    ((t as u8 as i8 as i32) * (c as u8 as i8 as i32)) >> 5
}

/// Decode an image stream of w x h (the main image when `main`; `w` is the full width).
fn image_stream(b: &mut Bits, w: usize, h: usize) -> Result<Vec<u32>, &'static str> {
    let mut transforms = Vec::new();
    let mut xs = w;
    let mut seen = 0u32;
    while b.bit()? == 1 {
        let t = b.get(2)?;
        if seen & (1 << t) != 0 {
            return Err("Damaged WebP file");
        }
        seen |= 1 << t;
        match t {
            0 | 1 => {
                let bits = b.get(3)? + 2;
                let img = entropy_image(b, div_up(xs, bits), div_up(h, bits), false)?;
                transforms.push(if t == 0 { Transform::Predictor(bits, img) } else { Transform::Cross(bits, img) });
            }
            2 => transforms.push(Transform::SubGreen),
            _ => {
                let n = b.get(8)? as usize + 1;
                let mut pal = entropy_image(b, n, 1, false)?;
                for k in 1..n {
                    pal[k] = add(pal[k], pal[k - 1]);
                }
                let bits = if n <= 2 {
                    3
                } else if n <= 4 {
                    2
                } else if n <= 16 {
                    1
                } else {
                    0
                };
                xs = div_up(xs, bits);
                transforms.push(Transform::Index(bits, pal));
            }
        }
    }
    let mut px = entropy_image(b, xs, h, true)?;
    let mut cw = xs;
    for t in transforms.iter().rev() {
        match t {
            Transform::SubGreen => {
                for p in px.iter_mut() {
                    let g = (*p >> 8) & 255;
                    *p = add(*p, g << 16 | g);
                }
            }
            Transform::Cross(bits, img) => {
                let bw = div_up(cw, *bits);
                for y in 0..h {
                    for x in 0..cw {
                        let m = img[(y >> bits) * bw + (x >> bits)];
                        let p = px[y * cw + x];
                        let (g, r, bl) = ((p >> 8) & 255, (p >> 16) & 255, p & 255);
                        let nr = (r as i32 + delta(m, g)) as u32 & 255;
                        let nb = (bl as i32 + delta(m >> 8, g) + delta(m >> 16, nr)) as u32 & 255;
                        px[y * cw + x] = (p & 0xFF00_FF00) | nr << 16 | nb;
                    }
                }
            }
            Transform::Predictor(bits, img) => {
                let bw = div_up(cw, *bits);
                for y in 0..h {
                    for x in 0..cw {
                        let i = y * cw + x;
                        let mode = if y == 0 {
                            if x == 0 {
                                0
                            } else {
                                1
                            }
                        } else if x == 0 {
                            2
                        } else {
                            (img[(y >> bits) * bw + (x >> bits)] >> 8) & 15
                        };
                        let l = if x > 0 { px[i - 1] } else { 0 };
                        let tp = if y > 0 { px[i - cw] } else { 0 };
                        let tl = if x > 0 && y > 0 { px[i - cw - 1] } else { 0 };
                        let tr = if y > 0 {
                            if x + 1 < cw {
                                px[i - cw + 1]
                            } else {
                                px[y * cw]
                            }
                        } else {
                            0
                        };
                        let pred = match mode {
                            0 => 0xFF00_0000,
                            1 => l,
                            2 => tp,
                            3 => tr,
                            4 => tl,
                            5 => avg2(avg2(l, tr), tp),
                            6 => avg2(l, tl),
                            7 => avg2(l, tp),
                            8 => avg2(tl, tp),
                            9 => avg2(tp, tr),
                            10 => avg2(avg2(l, tl), avg2(tp, tr)),
                            11 => select(l, tp, tl),
                            12 => clamp_full(l, tp, tl),
                            13 => clamp_half(avg2(l, tp), tl),
                            _ => return Err("Damaged WebP file"),
                        };
                        px[i] = add(px[i], pred);
                    }
                }
            }
            Transform::Index(bits, pal) => {
                // Only this transform narrows the picture, so it widens it back to `w`.
                let full_w = w;
                let per = 1usize << bits;
                let bpp = 8 >> bits;
                let mask = (1u32 << bpp) - 1;
                let mut out = vec![0u32; full_w * h];
                for y in 0..h {
                    for x in 0..full_w {
                        let packed = px[y * cw + (x >> bits)];
                        let idx = ((packed >> 8) >> ((x % per) * bpp)) & mask;
                        out[y * full_w + x] = pal.get(idx as usize).copied().unwrap_or(0);
                    }
                }
                px = out;
                cw = full_w;
            }
        }
    }
    Ok(px)
}

/// A complete VP8L stream: (width, height, ARGB pixels).
pub fn decode(d: &[u8]) -> Result<(usize, usize, Vec<u32>), &'static str> {
    if d.len() < 5 || d[0] != 0x2F {
        return Err("Not a valid lossless WebP");
    }
    let mut b = Bits { d, pos: 8 };
    let w = b.get(14)? as usize + 1;
    let h = b.get(14)? as usize + 1;
    let _alpha = b.get(1)?;
    let _version = b.get(3)?;
    crate::image::check_size(w, h)?;
    let px = image_stream(&mut b, w, h)?;
    Ok((w, h, px))
}

/// The alpha plane of a lossy WebP stored losslessly (no header; alpha in green).
pub fn decode_alpha(d: &[u8], w: usize, h: usize) -> Result<Vec<u8>, &'static str> {
    let mut b = Bits { d, pos: 0 };
    let px = image_stream(&mut b, w, h)?;
    Ok(px.iter().map(|p| (p >> 8) as u8).collect())
}

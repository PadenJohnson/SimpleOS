//! Shrinking a TrueType font to just the glyphs a PDF uses (glyph numbers stay the same, the
//! unused glyphs become empty), keeping only the tables a PDF reader needs.

use alloc::collections::BTreeSet;
use alloc::vec;
use alloc::vec::Vec;

fn u16be(b: &[u8], o: usize) -> u16 {
    b.get(o..o + 2).map(|s| u16::from_be_bytes([s[0], s[1]])).unwrap_or(0)
}
fn u32be(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map(|s| u32::from_be_bytes([s[0], s[1], s[2], s[3]])).unwrap_or(0)
}

fn tables(font: &[u8]) -> Vec<([u8; 4], usize, usize)> {
    let n = u16be(font, 4) as usize;
    (0..n)
        .filter_map(|i| {
            let r = 12 + i * 16;
            let tag: [u8; 4] = font.get(r..r + 4)?.try_into().ok()?;
            let off = u32be(font, r + 8) as usize;
            let len = u32be(font, r + 12) as usize;
            if off + len <= font.len() {
                Some((tag, off, len))
            } else {
                None
            }
        })
        .collect()
}

fn checksum(d: &[u8]) -> u32 {
    let mut s = 0u32;
    for c in d.chunks(4) {
        let mut w = [0u8; 4];
        w[..c.len()].copy_from_slice(c);
        s = s.wrapping_add(u32::from_be_bytes(w));
    }
    s
}

/// Keep `used` glyphs (and anything they're built from). None if the font can't be read.
pub fn subset(font: &[u8], used: &BTreeSet<u16>) -> Option<Vec<u8>> {
    let t = tables(font);
    let get = |tag: &[u8; 4]| t.iter().find(|x| &x.0 == tag).map(|&(_, o, l)| &font[o..o + l]);
    let head = get(b"head")?;
    let maxp = get(b"maxp")?;
    let loca = get(b"loca")?;
    let glyf = get(b"glyf")?;
    let long = u16be(head, 50) != 0;
    let n = u16be(maxp, 4) as usize;
    let off = |g: usize| -> (usize, usize) {
        if long {
            (u32be(loca, g * 4) as usize, u32be(loca, g * 4 + 4) as usize)
        } else {
            (u16be(loca, g * 2) as usize * 2, u16be(loca, g * 2 + 2) as usize * 2)
        }
    };
    // Glyphs needed, including composite parts.
    let mut keep: BTreeSet<u16> = used.iter().copied().filter(|&g| (g as usize) < n).collect();
    keep.insert(0);
    let mut todo: Vec<u16> = keep.iter().copied().collect();
    while let Some(g) = todo.pop() {
        let (a, b) = off(g as usize);
        if b <= a || b > glyf.len() {
            continue;
        }
        let d = &glyf[a..b];
        if (u16be(d, 0) as i16) < 0 {
            let mut p = 10;
            loop {
                let flags = u16be(d, p);
                let gi = u16be(d, p + 2);
                if keep.insert(gi) {
                    todo.push(gi);
                }
                p += 4 + if flags & 1 != 0 { 4 } else { 2 };
                if flags & 8 != 0 {
                    p += 2;
                } else if flags & 0x40 != 0 {
                    p += 4;
                } else if flags & 0x80 != 0 {
                    p += 8;
                }
                if flags & 0x20 == 0 || p >= d.len() {
                    break;
                }
            }
        }
    }
    let mut new_glyf = Vec::new();
    let mut new_loca = Vec::with_capacity((n + 1) * 4);
    for g in 0..n {
        new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
        if keep.contains(&(g as u16)) {
            let (a, b) = off(g);
            if b > a && b <= glyf.len() {
                new_glyf.extend_from_slice(&glyf[a..b]);
                while new_glyf.len() % 4 != 0 {
                    new_glyf.push(0);
                }
            }
        }
    }
    new_loca.extend_from_slice(&(new_glyf.len() as u32).to_be_bytes());
    let mut new_head = head.to_vec();
    new_head[8..12].fill(0); // checksum adjustment
    new_head[50] = 0;
    new_head[51] = 1; // long loca

    let mut out_tables: Vec<([u8; 4], Vec<u8>)> = Vec::new();
    for tag in [b"cvt ", b"fpgm", b"glyf", b"head", b"hhea", b"hmtx", b"loca", b"maxp", b"prep"] {
        let data = match tag {
            b"glyf" => new_glyf.clone(),
            b"loca" => new_loca.clone(),
            b"head" => new_head.clone(),
            _ => match get(tag) {
                Some(d) => d.to_vec(),
                None => continue,
            },
        };
        out_tables.push((*tag, data));
    }
    let nt = out_tables.len();
    let mut out = vec![0u8; 12 + nt * 16];
    out[0..4].copy_from_slice(&0x0001_0000u32.to_be_bytes());
    out[4..6].copy_from_slice(&(nt as u16).to_be_bytes());
    let mut pow = 1;
    let mut log = 0;
    while pow * 2 <= nt {
        pow *= 2;
        log += 1;
    }
    out[6..8].copy_from_slice(&((pow * 16) as u16).to_be_bytes());
    out[8..10].copy_from_slice(&(log as u16).to_be_bytes());
    out[10..12].copy_from_slice(&((nt * 16 - pow * 16) as u16).to_be_bytes());
    for (i, (tag, data)) in out_tables.iter().enumerate() {
        let off = out.len();
        let r = 12 + i * 16;
        out[r..r + 4].copy_from_slice(tag);
        out[r + 4..r + 8].copy_from_slice(&checksum(data).to_be_bytes());
        out[r + 8..r + 12].copy_from_slice(&(off as u32).to_be_bytes());
        out[r + 12..r + 16].copy_from_slice(&(data.len() as u32).to_be_bytes());
        out.extend_from_slice(data);
        while out.len() % 4 != 0 {
            out.push(0);
        }
    }
    Some(out)
}

//! Stream filters: Flate (with PNG/TIFF predictors), LZW, ASCII85, ASCIIHex, RunLength.
//! Picture filters (DCT = JPEG, JPX, CCITT, JBIG2) are left for the image code.

use alloc::vec;
use alloc::vec::Vec;

use super::obj::{Dict, Obj};

pub fn flate(data: &[u8]) -> Vec<u8> {
    use miniz_oxide::inflate::core::inflate_flags::*;
    use miniz_oxide::inflate::core::{decompress, DecompressorOxide};
    use miniz_oxide::inflate::TINFLStatus;
    // Inflate as much as possible, even from damaged streams.
    let mut flags = TINFL_FLAG_USING_NON_WRAPPING_OUTPUT_BUF;
    let raw = data.len() < 2 || ((data[0] as u16) << 8 | data[1] as u16) % 31 != 0 || data[0] & 0x0F != 8;
    if !raw {
        flags |= TINFL_FLAG_PARSE_ZLIB_HEADER;
    }
    let mut out = vec![0u8; (data.len() * 4).max(1024)];
    let mut st = DecompressorOxide::new();
    let mut inpos = 0;
    let mut outpos = 0;
    loop {
        let (status, inn, outn) = decompress(&mut st, &data[inpos..], &mut out, outpos, flags | TINFL_FLAG_HAS_MORE_INPUT * 0);
        inpos += inn;
        outpos += outn;
        match status {
            TINFLStatus::HasMoreOutput => {
                let n = out.len() * 2;
                out.resize(n, 0);
            }
            _ => break,
        }
        if outpos > 512 * 1024 * 1024 {
            break;
        }
    }
    out.truncate(outpos);
    out
}

fn predict(data: Vec<u8>, parms: Option<&Dict>) -> Vec<u8> {
    let Some(p) = parms else { return data };
    let pred = p.get("Predictor").and_then(|o| o.as_i64()).unwrap_or(1);
    if pred < 2 {
        return data;
    }
    let colors = p.get("Colors").and_then(|o| o.as_i64()).unwrap_or(1).clamp(1, 32) as usize;
    let bpc = p.get("BitsPerComponent").and_then(|o| o.as_i64()).unwrap_or(8).clamp(1, 16) as usize;
    let cols = p.get("Columns").and_then(|o| o.as_i64()).unwrap_or(1).max(1) as usize;
    let bpp = (colors * bpc).div_ceil(8);
    let row = (colors * bpc * cols).div_ceil(8);
    if pred == 2 {
        // TIFF: horizontal differencing (8-bit only).
        let mut d = data;
        if bpc == 8 {
            for r in d.chunks_mut(row) {
                for i in bpp..r.len() {
                    r[i] = r[i].wrapping_add(r[i - bpp]);
                }
            }
        }
        return d;
    }
    // PNG: each row starts with a filter type byte.
    let mut out = Vec::with_capacity(data.len());
    let mut prev = vec![0u8; row];
    for chunk in data.chunks(row + 1) {
        if chunk.is_empty() {
            break;
        }
        let ft = chunk[0];
        let mut cur = chunk[1..].to_vec();
        cur.resize(row, 0);
        for i in 0..row {
            let a = if i >= bpp { cur[i - bpp] as i32 } else { 0 };
            let b = prev[i] as i32;
            let c = if i >= bpp { prev[i - bpp] as i32 } else { 0 };
            let add = match ft {
                1 => a,
                2 => b,
                3 => (a + b) / 2,
                4 => {
                    let p = a + b - c;
                    let (pa, pb, pc) = ((p - a).abs(), (p - b).abs(), (p - c).abs());
                    if pa <= pb && pa <= pc {
                        a
                    } else if pb <= pc {
                        b
                    } else {
                        c
                    }
                }
                _ => 0,
            };
            cur[i] = (cur[i] as i32 + add) as u8;
        }
        out.extend_from_slice(&cur);
        prev = cur;
    }
    out
}

fn lzw(data: &[u8], early: bool) -> Vec<u8> {
    let mut out = Vec::new();
    let mut table: Vec<Vec<u8>> = Vec::new();
    let reset = |t: &mut Vec<Vec<u8>>| {
        t.clear();
        for i in 0..256 {
            t.push(vec![i as u8]);
        }
        t.push(Vec::new());
        t.push(Vec::new());
    };
    reset(&mut table);
    let mut bits = 9;
    let mut buf = 0u32;
    let mut nbuf = 0;
    let mut prev: Option<Vec<u8>> = None;
    for &b in data {
        buf = buf << 8 | b as u32;
        nbuf += 8;
        while nbuf >= bits {
            let code = ((buf >> (nbuf - bits)) & ((1 << bits) - 1)) as usize;
            nbuf -= bits;
            if code == 256 {
                reset(&mut table);
                bits = 9;
                prev = None;
                continue;
            }
            if code == 257 {
                return out;
            }
            let entry = if code < table.len() {
                table[code].clone()
            } else if let Some(p) = &prev {
                let mut e = p.clone();
                e.push(p[0]);
                e
            } else {
                return out;
            };
            out.extend_from_slice(&entry);
            if let Some(p) = prev.take() {
                if table.len() < 4096 {
                    let mut ne = p;
                    ne.push(entry[0]);
                    table.push(ne);
                }
            }
            prev = Some(entry);
            let n = table.len() + if early { 1 } else { 0 };
            bits = if n >= 2048 { 12 } else if n >= 1024 { 11 } else if n >= 512 { 10 } else { 9 };
        }
    }
    out
}

fn ascii85(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut group = [0u32; 5];
    let mut n = 0;
    for &c in data {
        match c {
            b'~' => break,
            b'z' if n == 0 => out.extend_from_slice(&[0, 0, 0, 0]),
            b'!'..=b'u' => {
                group[n] = (c - b'!') as u32;
                n += 1;
                if n == 5 {
                    let v = group.iter().fold(0u32, |a, &d| a.wrapping_mul(85).wrapping_add(d));
                    out.extend_from_slice(&v.to_be_bytes());
                    n = 0;
                }
            }
            _ => {}
        }
    }
    if n > 1 {
        for g in group.iter_mut().skip(n) {
            *g = 84;
        }
        let v = group.iter().fold(0u32, |a, &d| a.wrapping_mul(85).wrapping_add(d));
        out.extend_from_slice(&v.to_be_bytes()[..n - 1]);
    }
    out
}

fn asciihex(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut hi: Option<u8> = None;
    for &c in data {
        let d = match c {
            b'0'..=b'9' => c - b'0',
            b'a'..=b'f' => c - b'a' + 10,
            b'A'..=b'F' => c - b'A' + 10,
            b'>' => break,
            _ => continue,
        };
        match hi.take() {
            Some(h) => out.push(h << 4 | d),
            None => hi = Some(d),
        }
    }
    if let Some(h) = hi {
        out.push(h << 4);
    }
    out
}

fn runlength(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < data.len() {
        let n = data[i];
        i += 1;
        if n == 128 {
            break;
        } else if n < 128 {
            let end = (i + n as usize + 1).min(data.len());
            out.extend_from_slice(&data[i..end]);
            i = end;
        } else if i < data.len() {
            out.extend(core::iter::repeat_n(data[i], 257 - n as usize));
            i += 1;
        }
    }
    out
}

/// The filters of a stream as (name, parameters), in order.
pub fn filters(d: &Dict) -> Vec<(Vec<u8>, Option<Dict>)> {
    let f = d.get("Filter").or_else(|| d.get("F"));
    let p = d.get("DecodeParms").or_else(|| d.get("DP"));
    let names: Vec<Vec<u8>> = match f {
        Some(Obj::Name(n)) => vec![n.clone()],
        Some(Obj::Array(a)) => a.iter().filter_map(|o| o.as_name().map(|n| n.to_vec())).collect(),
        _ => Vec::new(),
    };
    let parms: Vec<Option<Dict>> = match p {
        Some(Obj::Dict(d)) => vec![Some(d.clone())],
        Some(Obj::Array(a)) => a.iter().map(|o| o.as_dict().cloned()).collect(),
        _ => Vec::new(),
    };
    names.into_iter().enumerate().map(|(i, n)| (n, parms.get(i).cloned().flatten())).collect()
}

/// Decode a stream's data through its filters, stopping before image-only filters
/// (returned as the name of the filter left, if any).
pub fn decode(d: &Dict, data: &[u8]) -> (Vec<u8>, Option<(Vec<u8>, Option<Dict>)>) {
    let mut cur = data.to_vec();
    for (name, parms) in filters(d) {
        cur = match name.as_slice() {
            b"FlateDecode" | b"Fl" => predict(flate(&cur), parms.as_ref()),
            b"LZWDecode" | b"LZW" => {
                let early = parms.as_ref().and_then(|p| p.get("EarlyChange")).and_then(|o| o.as_i64()).unwrap_or(1) != 0;
                predict(lzw(&cur, early), parms.as_ref())
            }
            b"ASCII85Decode" | b"A85" => ascii85(&cur),
            b"ASCIIHexDecode" | b"AHx" => asciihex(&cur),
            b"RunLengthDecode" | b"RL" => runlength(&cur),
            b"Crypt" => cur,
            _ => return (cur, Some((name, parms))),
        };
    }
    (cur, None)
}

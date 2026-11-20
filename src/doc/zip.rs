//! ZIP archives (the container of .xlsx files): read stored/deflated members, write
//! deflated ones.

use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::part::crc32;

fn u16le(b: &[u8], o: usize) -> Option<usize> {
    Some(u16::from_le_bytes(b.get(o..o + 2)?.try_into().ok()?) as usize)
}
fn u32le(b: &[u8], o: usize) -> Option<usize> {
    Some(u32::from_le_bytes(b.get(o..o + 4)?.try_into().ok()?) as usize)
}

/// All members: (path, contents). Fails on encrypted, ZIP64 or unknown compression.
pub fn read(b: &[u8]) -> Result<Vec<(String, Vec<u8>)>, &'static str> {
    // End of central directory: last 22..65557 bytes.
    let min = b.len().saturating_sub(22 + 65535);
    let eocd = (min..b.len().saturating_sub(21)).rev().find(|&i| b[i..].starts_with(&[0x50, 0x4B, 0x05, 0x06])).ok_or("Not a zip file")?;
    let count = u16le(b, eocd + 10).ok_or("Damaged zip")?;
    let mut p = u32le(b, eocd + 16).ok_or("Damaged zip")?;
    let mut out = Vec::new();
    for _ in 0..count {
        if !b.get(p..).is_some_and(|s| s.starts_with(&[0x50, 0x4B, 0x01, 0x02])) {
            return Err("Damaged zip directory");
        }
        let flags = u16le(b, p + 8).ok_or("Damaged zip")?;
        let method = u16le(b, p + 10).ok_or("Damaged zip")?;
        let csize = u32le(b, p + 20).ok_or("Damaged zip")?;
        let usize_ = u32le(b, p + 24).ok_or("Damaged zip")?;
        let nlen = u16le(b, p + 28).ok_or("Damaged zip")?;
        let xlen = u16le(b, p + 30).ok_or("Damaged zip")?;
        let clen = u16le(b, p + 32).ok_or("Damaged zip")?;
        let local = u32le(b, p + 42).ok_or("Damaged zip")?;
        let name = String::from_utf8_lossy(b.get(p + 46..p + 46 + nlen).ok_or("Damaged zip")?).into_owned();
        p += 46 + nlen + xlen + clen;
        if flags & 1 != 0 {
            return Err("The file is password protected");
        }
        if csize == 0xFFFF_FFFF || local == 0xFFFF_FFFF {
            return Err("The file is too big");
        }
        if name.ends_with('/') {
            continue;
        }
        let lnl = u16le(b, local + 26).ok_or("Damaged zip")?;
        let lxl = u16le(b, local + 28).ok_or("Damaged zip")?;
        let start = local + 30 + lnl + lxl;
        let data = b.get(start..start + csize).ok_or("Damaged zip")?;
        let bytes = match method {
            0 => data.to_vec(),
            8 => miniz_oxide::inflate::decompress_to_vec_with_limit(data, usize_.max(1) * 2 + 1024).map_err(|_| "Damaged zip data")?,
            _ => return Err("Unsupported zip compression"),
        };
        out.push((name, bytes));
    }
    Ok(out)
}

/// Build a zip with every member deflated.
pub fn write(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    let (time, date) = {
        let t = crate::rtc::fat_timestamp();
        ((t & 0xFFFF) as u16, (t >> 16) as u16)
    };
    for (name, data) in files {
        let packed = miniz_oxide::deflate::compress_to_vec(data, 6);
        let crc = crc32(data);
        let off = out.len() as u32;
        let header = |sig: u32, central_dir: bool, buf: &mut Vec<u8>| {
            buf.extend_from_slice(&sig.to_le_bytes());
            if central_dir {
                buf.extend_from_slice(&20u16.to_le_bytes()); // made by
            }
            buf.extend_from_slice(&20u16.to_le_bytes()); // needed
            buf.extend_from_slice(&0x0800u16.to_le_bytes()); // UTF-8 names
            buf.extend_from_slice(&8u16.to_le_bytes()); // deflate
            buf.extend_from_slice(&time.to_le_bytes());
            buf.extend_from_slice(&date.to_le_bytes());
            buf.extend_from_slice(&crc.to_le_bytes());
            buf.extend_from_slice(&(packed.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(data.len() as u32).to_le_bytes());
            buf.extend_from_slice(&(name.len() as u16).to_le_bytes());
            buf.extend_from_slice(&0u16.to_le_bytes()); // extra
        };
        header(0x0403_4B50, false, &mut out);
        out.extend_from_slice(name.as_bytes());
        out.extend_from_slice(&packed);
        header(0x0201_4B50, true, &mut central);
        central.extend_from_slice(&0u16.to_le_bytes()); // comment
        central.extend_from_slice(&0u16.to_le_bytes()); // disk
        central.extend_from_slice(&0u16.to_le_bytes()); // internal attrs
        central.extend_from_slice(&0u32.to_le_bytes()); // external attrs
        central.extend_from_slice(&off.to_le_bytes());
        central.extend_from_slice(name.as_bytes());
    }
    let cd_off = out.len() as u32;
    out.extend_from_slice(&central);
    out.extend_from_slice(&0x0605_4B50u32.to_le_bytes());
    out.extend_from_slice(&[0, 0, 0, 0]);
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(files.len() as u16).to_le_bytes());
    out.extend_from_slice(&(central.len() as u32).to_le_bytes());
    out.extend_from_slice(&cd_off.to_le_bytes());
    out.extend_from_slice(&0u16.to_le_bytes());
    out
}

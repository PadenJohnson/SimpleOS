//! Partition tables: none ("superfloppy"), MBR, and GPT, plus creating Solon's own
//! GPT partition in free space (the only partition-table write Solon ever does).

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{exfat, fat, le32, le64, utf16_name, BlockDev, FsError};

/// GPT partition type that marks Solon's library. Solon writes to no other partition.
pub const SOLON_TYPE: [u8; 16] = guid(0x5010A1B5, 0x2026, 0x4A4C, [0x9C, 0x4E, 0x53, 0x4F, 0x4C, 0x4F, 0x4E, 0x4C]);
const ENTRY_COUNT: usize = 128;
const ENTRY_SIZE: usize = 128;

/// GUIDs are stored with the first three fields little-endian.
const fn guid(a: u32, b: u16, c: u16, d: [u8; 8]) -> [u8; 16] {
    let a = a.to_le_bytes();
    let b = b.to_le_bytes();
    let c = c.to_le_bytes();
    [a[0], a[1], a[2], a[3], b[0], b[1], c[0], c[1], d[0], d[1], d[2], d[3], d[4], d[5], d[6], d[7]]
}

/// (first block, block count) of each partition worth trying to mount.
pub fn partitions(dev: &mut dyn BlockDev, total: u64) -> Vec<(u64, u64)> {
    let bs = dev.block_size();
    let mut b0 = vec![0u8; bs];
    if dev.read_blocks(0, &mut b0).is_err() {
        return Vec::new();
    }
    // Many USB sticks and cards have a file system right at block 0, no partition table.
    if exfat::is_exfat(&b0) || fat::is_fat(&b0) {
        return vec![(0, total)];
    }
    if b0[510] != 0x55 || b0[511] != 0xAA {
        return Vec::new();
    }

    let mut parts = Vec::new();
    let mut is_gpt = false;
    for i in 0..4 {
        let e = &b0[446 + i * 16..462 + i * 16];
        let ty = e[4];
        let start = le32(e, 8) as u64;
        let len = le32(e, 12) as u64;
        match ty {
            0 => {}
            0xEE => is_gpt = true,
            0x05 | 0x0F | 0x85 => {} // extended partitions: not supported yet
            _ if start > 0 && start < total => parts.push((start, len)),
            _ => {}
        }
    }
    if is_gpt {
        return match read_gpt(dev, total) {
            Some(g) => g.parts.iter().map(|p| (p.first, p.last - p.first + 1)).collect(),
            None => Vec::new(),
        };
    }
    parts
}

#[allow(dead_code)]
pub struct GptPart {
    pub slot: usize,
    pub type_guid: [u8; 16],
    pub first: u64,
    pub last: u64,
    pub name: String,
}

pub struct Gpt {
    header: Vec<u8>,
    entries: Vec<u8>,
    pub parts: Vec<GptPart>,
    pub first_usable: u64,
    pub last_usable: u64,
    entries_lba: u64,
    backup_lba: u64,
    /// Header and entry checksums are correct (we refuse to modify a table that isn't).
    pub valid: bool,
}

pub fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

pub fn read_gpt(dev: &mut dyn BlockDev, total: u64) -> Option<Gpt> {
    let bs = dev.block_size();
    let mut hdr = vec![0u8; bs];
    dev.read_blocks(1, &mut hdr).ok()?;
    if &hdr[0..8] != b"EFI PART" {
        return None;
    }
    let hsize = (le32(&hdr, 12) as usize).clamp(92, bs);
    let entries_lba = le64(&hdr, 72);
    let count = le32(&hdr, 80) as usize;
    let esize = le32(&hdr, 84) as usize;
    if count == 0 || count > 1024 || !(128..=1024).contains(&esize) {
        return None;
    }
    let blocks = (count * esize + bs - 1) / bs;
    let mut entries = vec![0u8; blocks * bs];
    dev.read_blocks(entries_lba, &mut entries).ok()?;
    entries.truncate(count * esize);

    let mut h = hdr[..hsize].to_vec();
    let stored = le32(&h, 16);
    h[16..20].fill(0);
    let valid = crc32(&h) == stored && crc32(&entries) == le32(&hdr, 88);

    let mut parts = Vec::new();
    for i in 0..count {
        let e = &entries[i * esize..(i + 1) * esize];
        if e[0..16].iter().all(|&b| b == 0) {
            continue;
        }
        let first = le64(e, 32);
        let last = le64(e, 40);
        if first == 0 || last < first || last >= total {
            continue;
        }
        let units: Vec<u16> = (0..36).map(|k| u16::from_le_bytes([e[56 + k * 2], e[57 + k * 2]])).collect();
        parts.push(GptPart { slot: i, type_guid: e[0..16].try_into().unwrap(), first, last, name: utf16_name(&units) });
    }
    Some(Gpt {
        header: hdr[..hsize].to_vec(),
        entries,
        parts,
        first_usable: le64(&hdr, 40),
        last_usable: le64(&hdr, 48),
        entries_lba,
        backup_lba: le64(&hdr, 32),
        valid,
    })
}

/// What's on a disk, for deciding whether Solon can make room for its library.
pub enum Layout {
    /// No partition table and nothing recognisable at the start: safe to set up.
    Blank,
    Gpt(Gpt),
    /// MBR or a bare file system: Solon won't modify these.
    Other,
}

pub fn layout(dev: &mut dyn BlockDev, total: u64) -> Layout {
    let bs = dev.block_size();
    let mut b = vec![0u8; bs * 2];
    if dev.read_blocks(0, &mut b).is_err() {
        return Layout::Other;
    }
    if b.iter().all(|&x| x == 0) {
        return Layout::Blank;
    }
    if b[510] == 0x55 && b[511] == 0xAA && (0..4).any(|i| b[446 + i * 16 + 4] == 0xEE) {
        if let Some(g) = read_gpt(dev, total) {
            return Layout::Gpt(g);
        }
    }
    Layout::Other
}

fn align_up(v: u64, a: u64) -> u64 {
    (v + a - 1) / a * a
}

/// Largest free region, aligned to 1 MiB: (first block, block count).
/// `min_start`: nothing before this block is used (see `boot_stick_min_start`).
pub fn largest_free(layout: &Layout, total: u64, bs: usize, min_start: u64) -> Option<(u64, u64)> {
    let align = (1024 * 1024 / bs) as u64;
    let (lo, hi, mut used): (u64, u64, Vec<(u64, u64)>) = match layout {
        Layout::Blank => {
            let entry_blocks = ((ENTRY_COUNT * ENTRY_SIZE + bs - 1) / bs) as u64;
            (2 + entry_blocks, total - 2 - entry_blocks, Vec::new())
        }
        Layout::Gpt(g) if g.valid => (g.first_usable, g.last_usable, g.parts.iter().map(|p| (p.first, p.last)).collect()),
        _ => return None,
    };
    let lo = lo.max(min_start);
    used.sort();
    let mut best: Option<(u64, u64)> = None;
    let mut cursor = lo;
    for (a, b) in used.iter().copied().chain(core::iter::once((hi + 1, hi + 1))) {
        let start = align_up(cursor, align);
        if a > start {
            let len = (a - start) / align * align;
            if len > 0 && best.map_or(true, |(_, l)| len > l) {
                best = Some((start, len));
            }
        }
        cursor = cursor.max(b + 1);
    }
    best
}

fn put_entry(entries: &mut [u8], slot: usize, first: u64, last: u64, unique: [u8; 16]) {
    let e = &mut entries[slot * ENTRY_SIZE..(slot + 1) * ENTRY_SIZE];
    e.fill(0);
    e[0..16].copy_from_slice(&SOLON_TYPE);
    e[16..32].copy_from_slice(&unique);
    e[32..40].copy_from_slice(&first.to_le_bytes());
    e[40..48].copy_from_slice(&last.to_le_bytes());
    for (i, u) in "Solon Library".encode_utf16().enumerate() {
        e[56 + i * 2..58 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
}

fn header_bytes(base: &[u8], me: u64, other: u64, entries_lba: u64, entries_crc: u32) -> Vec<u8> {
    let mut h = base.to_vec();
    h[24..32].copy_from_slice(&me.to_le_bytes());
    h[32..40].copy_from_slice(&other.to_le_bytes());
    h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
    h[88..92].copy_from_slice(&entries_crc.to_le_bytes());
    h[16..20].fill(0);
    let c = crc32(&h);
    h[16..20].copy_from_slice(&c.to_le_bytes());
    h
}

fn write_padded(dev: &mut dyn BlockDev, lba: u64, data: &[u8]) -> Result<(), FsError> {
    let bs = dev.block_size();
    let mut buf = vec![0u8; (data.len() + bs - 1) / bs * bs];
    buf[..data.len()].copy_from_slice(data);
    dev.write_blocks(lba, &buf)
}

/// Create the Solon library partition in the largest free region. Existing partitions are
/// never moved, resized or rewritten. Returns (first block, block count).
pub fn create_solon_partition(dev: &mut dyn BlockDev, total: u64, unique: [u8; 16], min_start: u64) -> Result<(u64, u64), FsError> {
    let bs = dev.block_size();
    let lay = layout(dev, total);
    let (first, len) = largest_free(&lay, total, bs, min_start).ok_or(FsError::Full)?;
    let last = first + len - 1;
    match lay {
        Layout::Blank => {
            let entry_blocks = ((ENTRY_COUNT * ENTRY_SIZE + bs - 1) / bs) as u64;
            let mut entries = vec![0u8; ENTRY_COUNT * ENTRY_SIZE];
            put_entry(&mut entries, 0, first, last, unique);
            let ecrc = crc32(&entries);
            let mut base = vec![0u8; 92];
            base[0..8].copy_from_slice(b"EFI PART");
            base[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
            base[12..16].copy_from_slice(&92u32.to_le_bytes());
            base[40..48].copy_from_slice(&(2 + entry_blocks).to_le_bytes());
            base[48..56].copy_from_slice(&(total - 2 - entry_blocks).to_le_bytes());
            let mut disk_guid = unique;
            disk_guid[0] ^= 0xFF;
            base[56..72].copy_from_slice(&disk_guid);
            base[80..84].copy_from_slice(&(ENTRY_COUNT as u32).to_le_bytes());
            base[84..88].copy_from_slice(&(ENTRY_SIZE as u32).to_le_bytes());
            let backup_entries = total - 1 - entry_blocks;
            // Backup copy first, then primary, then the protective MBR.
            write_padded(dev, backup_entries, &entries)?;
            write_padded(dev, total - 1, &header_bytes(&base, total - 1, 1, backup_entries, ecrc))?;
            write_padded(dev, 2, &entries)?;
            write_padded(dev, 1, &header_bytes(&base, 1, total - 1, 2, ecrc))?;
            let mut mbr = vec![0u8; bs];
            let e = &mut mbr[446..462];
            e[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
            e[4] = 0xEE;
            e[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
            e[8..12].copy_from_slice(&1u32.to_le_bytes());
            e[12..16].copy_from_slice(&((total - 1).min(0xFFFF_FFFF) as u32).to_le_bytes());
            mbr[510] = 0x55;
            mbr[511] = 0xAA;
            dev.write_blocks(0, &mbr)?;
        }
        Layout::Gpt(mut g) => {
            if !g.valid {
                return Err(FsError::Corrupt);
            }
            let esize = le32(&g.header, 84) as usize;
            let count = g.entries.len() / esize;
            let slot = (0..count)
                .find(|&i| g.entries[i * esize..i * esize + 16].iter().all(|&b| b == 0))
                .ok_or(FsError::Full)?;
            // put_entry assumes 128-byte entries; larger entries keep their extra bytes zeroed.
            let e = &mut g.entries[slot * esize..(slot + 1) * esize];
            e.fill(0);
            let mut tmp = vec![0u8; ENTRY_SIZE];
            put_entry(&mut tmp, 0, first, last, unique);
            e[..ENTRY_SIZE].copy_from_slice(&tmp);
            let ecrc = crc32(&g.entries);
            let entry_blocks = ((g.entries.len() + bs - 1) / bs) as u64;
            let backup_entries = g.backup_lba - entry_blocks;
            write_padded(dev, backup_entries, &g.entries)?;
            write_padded(dev, g.backup_lba, &header_bytes(&g.header, g.backup_lba, 1, backup_entries, ecrc))?;
            write_padded(dev, g.entries_lba, &g.entries)?;
            write_padded(dev, 1, &header_bytes(&g.header, 1, g.backup_lba, g.entries_lba, ecrc))?;
        }
        Layout::Other => return Err(FsError::ReadOnly),
    }
    dev.flush()?;
    kprintln!("part: created Solon partition at block {} ({} blocks)", first, len);
    Ok((first, len))
}

/// A disk image written to a bigger stick has its backup GPT in the middle of the stick.
/// Move the backup to the real end and widen the usable area (partitions are untouched).
/// Returns true if the table was changed.
pub fn grow_gpt(dev: &mut dyn BlockDev, total: u64) -> Result<bool, FsError> {
    let Some(g) = read_gpt(dev, total) else { return Ok(false) };
    if !g.valid {
        return Err(FsError::Corrupt);
    }
    if g.backup_lba == total - 1 {
        return Ok(false);
    }
    let bs = dev.block_size();
    let entry_blocks = ((g.entries.len() + bs - 1) / bs) as u64;
    let backup_entries = total - 1 - entry_blocks;
    let last_usable = backup_entries - 1;
    if g.parts.iter().any(|p| p.last > last_usable) {
        return Err(FsError::Corrupt);
    }
    let ecrc = crc32(&g.entries);
    let mut base = g.header.clone();
    base[48..56].copy_from_slice(&last_usable.to_le_bytes());
    write_padded(dev, backup_entries, &g.entries)?;
    write_padded(dev, total - 1, &header_bytes(&base, total - 1, 1, backup_entries, ecrc))?;
    write_padded(dev, 1, &header_bytes(&base, 1, total - 1, g.entries_lba, ecrc))?;
    dev.flush()?;
    kprintln!("part: partition table grown to the full disk ({} blocks)", total);
    Ok(true)
}

/// Solon library partitions on a disk: (first block, block count).
pub fn solon_partitions(dev: &mut dyn BlockDev, total: u64) -> Vec<(u64, u64)> {
    match layout(dev, total) {
        Layout::Gpt(g) => g.parts.iter().filter(|p| p.type_guid == SOLON_TYPE).map(|p| (p.first, p.last - p.first + 1)).collect(),
        _ => Vec::new(),
    }
}

/// On Solon's boot stick, the BIOS bootloader keeps part of itself just past the boot
/// partition (where the image's backup partition table used to be). Keep 2 MiB clear.
pub fn boot_stick_min_start(lay: &Layout, bs: usize) -> u64 {
    match lay {
        Layout::Gpt(g) => g.parts.iter().map(|p| p.last + 1).max().unwrap_or(0) + (2 * 1024 * 1024 / bs) as u64,
        _ => 0,
    }
}

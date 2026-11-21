//! FAT12 / FAT16 / FAT32 with long file names. Reading, plus adding new files and folders
//! ("Copy to USB"); existing files are never changed or deleted.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{le16, le32, utf16_name, BlockDev, DirEntry, FsError, FsKind, VolumeIo};

pub struct Fat {
    kind: FsKind,
    bps: u64,
    spc: u64,
    fat_start: u64,     // sector
    root_start: u64,    // sector (FAT12/16 fixed root directory)
    root_sectors: u64,  // FAT12/16
    data_start: u64,    // sector of cluster 2
    clusters: u32,
    root_cluster: u32,  // FAT32
    num_fats: u64,
    fat_size: u64,      // sectors per FAT copy
    /// Where to start looking for free clusters.
    hint: u32,
    /// FSInfo free count has been invalidated (FAT32) since mounting.
    fsinfo_cleared: bool,
    fsinfo_sector: u64,
}

/// Does this boot sector look like FAT (any variant)?
pub fn is_fat(b: &[u8]) -> bool {
    if b.len() < 512 || b[510] != 0x55 || b[511] != 0xAA {
        return false;
    }
    if !(b[0] == 0xEB || b[0] == 0xE9) {
        return false;
    }
    let bps = le16(b, 11);
    let spc = b[13];
    let rsvd = le16(b, 14);
    let fats = b[16];
    matches!(bps, 512 | 1024 | 2048 | 4096) && spc != 0 && spc.is_power_of_two() && rsvd > 0 && (1..=2).contains(&fats)
}

fn label_from(raw: &[u8]) -> String {
    let s: String = raw.iter().map(|&c| if (0x20..0x7F).contains(&c) { c as char } else { '?' }).collect();
    let s = String::from(s.trim_end());
    if s == "NO NAME" {
        String::new()
    } else {
        s
    }
}

impl Fat {
    pub fn mount(dev: &mut dyn BlockDev, start: u64, b: &[u8]) -> Option<(Fat, VolumeIo, String, u64)> {
        let bps = le16(b, 11) as u64;
        if (bps as usize) < dev.block_size() || bps as usize % dev.block_size() != 0 {
            return None;
        }
        let spc = b[13] as u64;
        let rsvd = le16(b, 14) as u64;
        let fats = b[16] as u64;
        let root_entries = le16(b, 17) as u64;
        let total = if le16(b, 19) != 0 { le16(b, 19) as u64 } else { le32(b, 32) as u64 };
        let fat_size = if le16(b, 22) != 0 { le16(b, 22) as u64 } else { le32(b, 36) as u64 };
        let root_sectors = (root_entries * 32 + bps - 1) / bps;
        let data_start = rsvd + fats * fat_size + root_sectors;
        if total <= data_start || fat_size == 0 {
            return None;
        }
        let clusters = ((total - data_start) / spc) as u32;
        let kind = if clusters < 4085 {
            FsKind::Fat12
        } else if clusters < 65525 {
            FsKind::Fat16
        } else {
            FsKind::Fat32
        };
        let (root_cluster, label_raw) =
            if kind == FsKind::Fat32 { (le32(b, 44), &b[71..82]) } else { (0, &b[43..54]) };
        let mut fs = Fat {
            kind,
            bps,
            spc,
            fat_start: rsvd,
            root_start: rsvd + fats * fat_size,
            root_sectors,
            data_start,
            clusters,
            root_cluster,
            num_fats: fats,
            fat_size,
            hint: 2,
            fsinfo_cleared: false,
            fsinfo_sector: if kind == FsKind::Fat32 { le16(b, 48) as u64 } else { 0 },
        };
        let mut io = VolumeIo::new(start, bps as usize);
        // The label in the root directory wins over the (often stale) boot sector copy.
        let mut label = label_from(label_raw);
        let root = DirEntry::dir(String::new(), fs.root_cluster());
        if let Ok(raw) = fs.dir_bytes(&mut io, dev, &root) {
            for e in raw.chunks_exact(32) {
                if e[0] == 0 {
                    break;
                }
                if e[0] != 0xE5 && e[11] & 0x08 != 0 && e[11] & 0x0F != 0x0F {
                    label = label_from(&e[0..11]);
                    break;
                }
            }
        }
        let _ = &mut fs;
        Some((fs, io, label, total * bps))
    }

    pub fn kind(&self) -> FsKind {
        self.kind
    }

    pub fn root_cluster(&self) -> u32 {
        self.root_cluster
    }

    fn cluster_bytes(&self) -> usize {
        (self.bps * self.spc) as usize
    }

    fn cluster_sector(&self, c: u32) -> u64 {
        self.data_start + (c as u64 - 2) * self.spc
    }

    /// Next cluster in a chain, or None at the end (or on a bad/invalid entry).
    fn next(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, c: u32) -> Result<Option<u32>, FsError> {
        let base = self.fat_start * self.bps;
        let n = match self.kind {
            FsKind::Fat32 => {
                let mut b = [0u8; 4];
                io.read_at(dev, base + c as u64 * 4, &mut b)?;
                let v = u32::from_le_bytes(b) & 0x0FFF_FFFF;
                if v >= 0x0FFF_FFF7 {
                    return Ok(None);
                }
                v
            }
            FsKind::Fat16 => {
                let mut b = [0u8; 2];
                io.read_at(dev, base + c as u64 * 2, &mut b)?;
                let v = u16::from_le_bytes(b) as u32;
                if v >= 0xFFF7 {
                    return Ok(None);
                }
                v
            }
            _ => {
                let mut b = [0u8; 2];
                io.read_at(dev, base + c as u64 + c as u64 / 2, &mut b)?;
                let v = u16::from_le_bytes(b) as u32;
                let v = if c & 1 == 1 { v >> 4 } else { v & 0xFFF };
                if v >= 0xFF7 {
                    return Ok(None);
                }
                v
            }
        };
        if n < 2 || n >= self.clusters + 2 {
            return Ok(None); // free or out of range: treat as end of chain
        }
        Ok(Some(n))
    }

    /// The clusters of a chain, capped so a corrupt (looping) FAT can't hang us.
    fn chain(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, first: u32, max: usize) -> Result<Vec<u32>, FsError> {
        let mut v = Vec::new();
        if first < 2 || first >= self.clusters + 2 {
            return Ok(v);
        }
        let mut c = first;
        loop {
            v.push(c);
            if v.len() > max {
                return Err(FsError::Corrupt);
            }
            match self.next(io, dev, c)? {
                Some(n) => c = n,
                None => return Ok(v),
            }
        }
    }

    /// Read the given clusters, merging runs of consecutive clusters into big reads.
    fn read_clusters(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, chain: &[u32], out: &mut Vec<u8>) -> Result<(), FsError> {
        let cb = self.cluster_bytes();
        let max_run = (crate::msc::MAX_XFER / cb).max(1);
        let mut i = 0;
        while i < chain.len() {
            let mut n = 1;
            while i + n < chain.len() && chain[i + n] == chain[i] + n as u32 && n < max_run {
                n += 1;
            }
            let at = out.len();
            out.resize(at + n * cb, 0);
            io.read_sectors(dev, self.cluster_sector(chain[i]), &mut out[at..])?;
            i += n;
        }
        Ok(())
    }

    fn dir_bytes(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry) -> Result<Vec<u8>, FsError> {
        let mut out = Vec::new();
        if dir.first_cluster == 0 && self.kind != FsKind::Fat32 {
            out.resize((self.root_sectors * self.bps) as usize, 0);
            io.read_sectors(dev, self.root_start, &mut out)?;
        } else {
            let chain = self.chain(io, dev, dir.first_cluster, 65536 * 32 / self.cluster_bytes() + 1)?;
            self.read_clusters(io, dev, &chain, &mut out)?;
        }
        Ok(out)
    }

    pub fn list(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry) -> Result<Vec<DirEntry>, FsError> {
        let raw = self.dir_bytes(io, dev, dir)?;
        let mut out = Vec::new();
        let mut lfn: Vec<(u8, [u16; 13])> = Vec::new();
        let mut lfn_sum = 0u8;
        for e in raw.chunks_exact(32) {
            if e[0] == 0x00 {
                break;
            }
            if e[0] == 0xE5 {
                lfn.clear();
                continue;
            }
            let attr = e[11];
            if attr & 0x3F == 0x0F {
                // Long-name piece: 13 UTF-16 chars, stored last piece first.
                let mut chars = [0u16; 13];
                for (k, off) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].iter().enumerate() {
                    chars[k] = le16(e, *off);
                }
                if e[0] & 0x40 != 0 {
                    lfn.clear();
                }
                lfn_sum = e[13];
                lfn.push((e[0] & 0x1F, chars));
                continue;
            }
            if attr & 0x08 != 0 {
                lfn.clear();
                continue; // volume label
            }
            let short = &e[0..11];
            let mut sum: u8 = 0;
            for &c in short {
                sum = (sum >> 1 | sum << 7).wrapping_add(c);
            }
            let name = if !lfn.is_empty() && sum == lfn_sum {
                lfn.sort_by_key(|(ord, _)| *ord);
                let units: Vec<u16> = lfn.iter().flat_map(|(_, c)| c.iter().copied()).collect();
                utf16_name(&units)
            } else {
                short_name(e)
            };
            lfn.clear();
            if name == "." || name == ".." {
                continue;
            }
            let first_cluster = (le16(e, 20) as u32) << 16 | le16(e, 26) as u32;
            let is_dir = attr & 0x10 != 0;
            out.push(DirEntry { name, is_dir, size: if is_dir { 0 } else { le32(e, 28) as u64 }, first_cluster, contiguous: false, loc: Vec::new(), mtime: (le16(e, 24) as u32) << 16 | le16(e, 22) as u32 });
        }
        Ok(out)
    }

    pub fn read_file(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, f: &DirEntry) -> Result<Vec<u8>, FsError> {
        if f.size == 0 {
            return Ok(Vec::new());
        }
        let need = ((f.size + self.cluster_bytes() as u64 - 1) / self.cluster_bytes() as u64) as usize;
        let chain = self.chain(io, dev, f.first_cluster, need + 1)?;
        if chain.len() < need {
            return Err(FsError::Corrupt);
        }
        let mut out = Vec::with_capacity(need * self.cluster_bytes());
        self.read_clusters(io, dev, &chain[..need], &mut out)?;
        out.truncate(f.size as usize);
        Ok(out)
    }
}

/// "README  TXT" -> "README.TXT", honouring the lowercase flags Windows sets.
fn short_name(e: &[u8]) -> String {
    let lower_base = e[12] & 0x08 != 0;
    let lower_ext = e[12] & 0x10 != 0;
    let conv = |c: u8, lower: bool| -> char {
        let c = if c == 0x05 { 0xE5 } else { c };
        if c >= 0x80 {
            '?'
        } else if lower {
            (c as char).to_ascii_lowercase()
        } else {
            c as char
        }
    };
    let base: String = e[0..8].iter().map(|&c| conv(c, lower_base)).collect();
    let ext: String = e[8..11].iter().map(|&c| conv(c, lower_ext)).collect();
    let (base, ext) = (base.trim_end(), ext.trim_end());
    let mut s = String::from(base);
    if !ext.is_empty() {
        s.push('.');
        s.push_str(ext);
    }
    let _ = vec![0u8; 0];
    s
}

// ---- writing (add-only) --------------------------------------------------------------

fn is_83_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'()-@^_`{}~".contains(c)
}

/// Choose the 8.3 short name stored next to a long name.
/// Returns (short name, needs long-name entries, lowercase flags).
fn make_short_name(long: &str, used: &[[u8; 11]]) -> ([u8; 11], bool, u8) {
    let (base, ext) = match long.rfind('.') {
        Some(i) if i > 0 => (&long[..i], &long[i + 1..]),
        _ => (long, ""),
    };
    let fits = !base.is_empty() && base.len() <= 8 && ext.len() <= 3 && base.chars().chain(ext.chars()).all(is_83_char);
    if fits {
        let mut sn = [b' '; 11];
        for (i, c) in base.to_ascii_uppercase().bytes().enumerate() {
            sn[i] = c;
        }
        for (i, c) in ext.to_ascii_uppercase().bytes().enumerate() {
            sn[8 + i] = c;
        }
        if !used.contains(&sn) {
            let upper = |s: &str| !s.chars().any(|c| c.is_ascii_lowercase());
            let lower = |s: &str| !s.chars().any(|c| c.is_ascii_uppercase());
            if upper(base) && upper(ext) {
                return (sn, false, 0);
            }
            if lower(base) && lower(ext) {
                let b = if base.chars().any(|c| c.is_ascii_alphabetic()) { 0x08 } else { 0 };
                let e = if ext.chars().any(|c| c.is_ascii_alphabetic()) { 0x10 } else { 0 };
                return (sn, false, b | e);
            }
        }
    }
    let clean = |s: &str, n: usize| -> Vec<u8> {
        s.chars().filter(|c| c.is_ascii() && is_83_char(c.to_ascii_uppercase())).map(|c| c.to_ascii_uppercase() as u8).take(n).collect()
    };
    let (b, e) = (clean(base, 6), clean(ext, 3));
    for n in 1..100_000u32 {
        let tail = alloc::format!("~{}", n);
        let mut sn = [b' '; 11];
        let keep = (8 - tail.len()).min(b.len());
        sn[..keep].copy_from_slice(&b[..keep]);
        sn[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        sn[8..8 + e.len()].copy_from_slice(&e);
        if !used.contains(&sn) {
            return (sn, true, 0);
        }
    }
    ([b'_'; 11], true, 0)
}

/// Long-name entries (last piece first) followed by the short entry.
fn dir_entries(name: &str, sn: [u8; 11], lfn: bool, flags: u8, attr: u8, cluster: u32, size: u32) -> Vec<[u8; 32]> {
    let mut out = Vec::new();
    if lfn {
        let mut units: Vec<u16> = name.encode_utf16().collect();
        if units.len() % 13 != 0 {
            units.push(0);
        }
        while units.len() % 13 != 0 {
            units.push(0xFFFF);
        }
        let n = units.len() / 13;
        let mut sum: u8 = 0;
        for &c in &sn {
            sum = (sum >> 1 | sum << 7).wrapping_add(c);
        }
        for seq in (1..=n).rev() {
            let chunk = &units[(seq - 1) * 13..seq * 13];
            let mut e = [0u8; 32];
            e[0] = seq as u8 | if seq == n { 0x40 } else { 0 };
            e[11] = 0x0F;
            e[13] = sum;
            for (k, off) in [1usize, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].iter().enumerate() {
                e[*off..*off + 2].copy_from_slice(&chunk[k].to_le_bytes());
            }
            out.push(e);
        }
    }
    let ts = crate::rtc::fat_timestamp();
    let (time, date) = ((ts & 0xFFFF) as u16, (ts >> 16) as u16);
    let mut e = [0u8; 32];
    e[..11].copy_from_slice(&sn);
    e[11] = attr;
    e[12] = flags;
    e[14..16].copy_from_slice(&time.to_le_bytes());
    e[16..18].copy_from_slice(&date.to_le_bytes());
    e[18..20].copy_from_slice(&date.to_le_bytes());
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[22..24].copy_from_slice(&time.to_le_bytes());
    e[24..26].copy_from_slice(&date.to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    out.push(e);
    out
}

impl Fat {
    fn eoc(&self) -> u32 {
        match self.kind {
            FsKind::Fat32 => 0x0FFF_FFFF,
            FsKind::Fat16 => 0xFFFF,
            _ => 0xFFF,
        }
    }

    /// Raw FAT entry (0 = free).
    fn fat_get(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, c: u32) -> Result<u32, FsError> {
        let base = self.fat_start * self.bps;
        Ok(match self.kind {
            FsKind::Fat32 => {
                let mut b = [0u8; 4];
                io.read_at(dev, base + c as u64 * 4, &mut b)?;
                u32::from_le_bytes(b) & 0x0FFF_FFFF
            }
            FsKind::Fat16 => {
                let mut b = [0u8; 2];
                io.read_at(dev, base + c as u64 * 2, &mut b)?;
                u16::from_le_bytes(b) as u32
            }
            _ => {
                let mut b = [0u8; 2];
                io.read_at(dev, base + c as u64 + c as u64 / 2, &mut b)?;
                let v = u16::from_le_bytes(b) as u32;
                if c & 1 == 1 {
                    v >> 4
                } else {
                    v & 0xFFF
                }
            }
        })
    }

    /// Set a FAT entry in every copy of the FAT.
    fn fat_set(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, c: u32, v: u32) -> Result<(), FsError> {
        for k in 0..self.num_fats {
            let base = (self.fat_start + k * self.fat_size) * self.bps;
            match self.kind {
                FsKind::Fat32 => {
                    let mut b = [0u8; 4];
                    io.read_at(dev, base + c as u64 * 4, &mut b)?;
                    let old = u32::from_le_bytes(b);
                    io.write_at(dev, base + c as u64 * 4, &((old & 0xF000_0000) | (v & 0x0FFF_FFFF)).to_le_bytes())?;
                }
                FsKind::Fat16 => io.write_at(dev, base + c as u64 * 2, &(v as u16).to_le_bytes())?,
                _ => {
                    let off = base + c as u64 + c as u64 / 2;
                    let mut b = [0u8; 2];
                    io.read_at(dev, off, &mut b)?;
                    let old = u16::from_le_bytes(b);
                    let new = if c & 1 == 1 { (old & 0x000F) | ((v as u16) << 4) } else { (old & 0xF000) | (v as u16 & 0x0FFF) };
                    io.write_at(dev, off, &new.to_le_bytes())?;
                }
            }
        }
        Ok(())
    }

    fn find_free(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, n: usize) -> Result<Vec<u32>, FsError> {
        let mut out = Vec::new();
        let end = self.clusters + 2;
        let start = self.hint.clamp(2, end - 1);
        let mut c = start;
        loop {
            if self.fat_get(io, dev, c)? == 0 {
                out.push(c);
                if out.len() == n {
                    self.hint = c + 1;
                    return Ok(out);
                }
            }
            c = if c + 1 >= end { 2 } else { c + 1 };
            if c == start {
                return Err(FsError::Full);
            }
        }
    }

    fn link(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, chain: &[u32]) -> Result<(), FsError> {
        let sets: Vec<(u32, u32)> = chain.iter().enumerate().map(|(i, &c)| (c, chain.get(i + 1).copied().unwrap_or(self.eoc()))).collect();
        self.fat_set_many(io, dev, sets)
    }

    /// Set many table entries: each window of the table is read, changed and written once
    /// (per FAT copy) instead of once per cluster.
    fn fat_set_many(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, mut sets: Vec<(u32, u32)>) -> Result<(), FsError> {
        sets.sort_by_key(|s| s.0);
        let entry_off = |c: u32| -> u64 {
            match self.kind {
                FsKind::Fat32 => c as u64 * 4,
                FsKind::Fat16 => c as u64 * 2,
                _ => c as u64 + c as u64 / 2,
            }
        };
        const WINDOW: u64 = 128 * 1024;
        let mut i = 0;
        while i < sets.len() {
            let start = entry_off(sets[i].0) / self.bps * self.bps;
            let mut j = i;
            while j < sets.len() && entry_off(sets[j].0) + 4 <= start + WINDOW {
                j += 1;
            }
            let end = ((entry_off(sets[j - 1].0) + 4 + self.bps - 1) / self.bps * self.bps).min(self.fat_size * self.bps);
            for k in 0..self.num_fats {
                let base = (self.fat_start + k * self.fat_size) * self.bps;
                let mut buf = vec![0u8; (end - start) as usize];
                io.read_at(dev, base + start, &mut buf)?;
                for &(c, v) in &sets[i..j] {
                    let o = (entry_off(c) - start) as usize;
                    match self.kind {
                        FsKind::Fat32 => {
                            let old = u32::from_le_bytes(buf[o..o + 4].try_into().unwrap());
                            buf[o..o + 4].copy_from_slice(&((old & 0xF000_0000) | (v & 0x0FFF_FFFF)).to_le_bytes());
                        }
                        FsKind::Fat16 => buf[o..o + 2].copy_from_slice(&(v as u16).to_le_bytes()),
                        _ => {
                            let old = u16::from_le_bytes([buf[o], buf[o + 1]]);
                            let new = if c & 1 == 1 { (old & 0x000F) | ((v as u16) << 4) } else { (old & 0xF000) | (v as u16 & 0x0FFF) };
                            buf[o..o + 2].copy_from_slice(&new.to_le_bytes());
                        }
                    }
                }
                io.write_at(dev, base + start, &buf)?;
            }
            i = j;
        }
        Ok(())
    }

    fn write_clusters(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, chain: &[u32], data: &[u8]) -> Result<(), FsError> {
        let cb = self.cluster_bytes();
        let mut i = 0;
        while i < chain.len() {
            // A run of consecutive clusters goes in one write (up to 1 MiB).
            let mut n = 1;
            while i + n < chain.len() && chain[i + n] == chain[i] + n as u32 && (n + 1) * cb <= 1024 * 1024 {
                n += 1;
            }
            let from = (i * cb).min(data.len());
            let to = ((i + n) * cb).min(data.len());
            if to - from == n * cb {
                io.write_sectors(dev, self.cluster_sector(chain[i]), &data[from..to])?;
            } else {
                let mut buf = vec![0u8; n * cb];
                buf[..to - from].copy_from_slice(&data[from..to]);
                io.write_sectors(dev, self.cluster_sector(chain[i]), &buf)?;
            }
            i += n;
        }
        Ok(())
    }

    /// The FSInfo free-space hint would be wrong after we allocate; mark it unknown.
    fn clear_fsinfo(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev) -> Result<(), FsError> {
        if self.kind == FsKind::Fat32 && !self.fsinfo_cleared && self.fsinfo_sector > 0 {
            let off = self.fsinfo_sector * self.bps;
            let mut sig = [0u8; 4];
            io.read_at(dev, off, &mut sig)?;
            if u32::from_le_bytes(sig) == 0x4161_5252 {
                io.write_at(dev, off + 488, &[0xFF; 8])?;
            }
            self.fsinfo_cleared = true;
        }
        Ok(())
    }

    /// Byte offset of directory record `i`, or None if past the end of a fixed root.
    fn record_offset(&self, dir: &DirEntry, chain: &[u32], i: usize) -> Option<u64> {
        if dir.first_cluster == 0 && self.kind != FsKind::Fat32 {
            let o = i as u64 * 32;
            return (o < self.root_sectors * self.bps).then(|| self.root_start * self.bps + o);
        }
        let cb = self.cluster_bytes();
        let byte = i * 32;
        chain.get(byte / cb).map(|&c| self.cluster_sector(c) * self.bps + (byte % cb) as u64)
    }

    fn dir_clusters(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry) -> Result<Vec<u32>, FsError> {
        if dir.first_cluster == 0 && self.kind != FsKind::Fat32 {
            return Ok(Vec::new());
        }
        self.chain(io, dev, dir.first_cluster, 65536 * 32 / self.cluster_bytes() + 1)
    }

    fn add_entries(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry, ents: &[[u8; 32]]) -> Result<(), FsError> {
        for _ in 0..2 {
            let raw = self.dir_bytes(io, dev, dir)?;
            let chain = self.dir_clusters(io, dev, dir)?;
            let mut run = 0;
            for i in 0..raw.len() / 32 {
                let b = raw[i * 32];
                if b == 0x00 || b == 0xE5 {
                    run += 1;
                    if run == ents.len() {
                        let first = i + 1 - ents.len();
                        for (k, e) in ents.iter().enumerate() {
                            let off = self.record_offset(dir, &chain, first + k).ok_or(FsError::Corrupt)?;
                            io.write_at(dev, off, e)?;
                        }
                        return Ok(());
                    }
                } else {
                    run = 0;
                }
            }
            // Full: add a cluster (impossible for the fixed FAT12/16 root folder).
            let last = *chain.last().ok_or(FsError::Full)?;
            let c = self.find_free(io, dev, 1)?[0];
            self.write_clusters(io, dev, &[c], &[])?;
            self.fat_set(io, dev, c, self.eoc())?;
            self.fat_set(io, dev, last, c)?;
        }
        Err(FsError::Full)
    }

    /// Add a new file (`data` = Some) or folder (None) to `dir`. Nothing existing is changed
    /// except that `dir` may grow.
    pub fn create(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry, name: &str, data: Option<&[u8]>) -> Result<DirEntry, FsError> {
        if name.is_empty() || name.len() > 255 || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
            return Err(FsError::Exists);
        }
        let raw = self.dir_bytes(io, dev, dir)?;
        if self.list(io, dev, dir)?.iter().any(|e| e.name.eq_ignore_ascii_case(name)) {
            return Err(FsError::Exists);
        }
        let used: Vec<[u8; 11]> = raw
            .chunks_exact(32)
            .filter(|e| e[0] != 0 && e[0] != 0xE5 && e[11] != 0x0F)
            .map(|e| e[..11].try_into().unwrap())
            .collect();
        let (sn, lfn, flags) = make_short_name(name, &used);
        if data.map_or(false, |d| d.len() as u64 > u32::MAX as u64) {
            return Err(FsError::TooBig);
        }
        self.clear_fsinfo(io, dev)?;
        let cb = self.cluster_bytes();
        let (chain, size, attr) = match data {
            Some(d) => {
                let n = (d.len() + cb - 1) / cb;
                let chain = if n > 0 { self.find_free(io, dev, n)? } else { Vec::new() };
                // Data first, then the FAT chain, then the directory entry.
                self.write_clusters(io, dev, &chain, d)?;
                dev.flush()?;
                if !chain.is_empty() {
                    self.link(io, dev, &chain)?;
                }
                (chain, d.len() as u32, 0x20)
            }
            None => {
                let c = self.find_free(io, dev, 1)?[0];
                let parent = if dir.first_cluster == self.root_cluster && self.kind == FsKind::Fat32 { 0 } else { dir.first_cluster };
                let mut dot = [b' '; 11];
                dot[0] = b'.';
                let mut body = dir_entries(".", dot, false, 0, 0x10, c, 0);
                dot[1] = b'.';
                body.extend(dir_entries("..", dot, false, 0, 0x10, parent, 0));
                let bytes: Vec<u8> = body.concat();
                self.write_clusters(io, dev, &[c], &bytes)?;
                dev.flush()?;
                self.link(io, dev, &[c])?;
                (alloc::vec![c], 0, 0x10)
            }
        };
        dev.flush()?;
        let first = chain.first().copied().unwrap_or(0);
        self.add_entries(io, dev, dir, &dir_entries(name, sn, lfn, flags, attr, first, size))?;
        dev.flush()?;
        Ok(DirEntry { name: String::from(name), is_dir: data.is_none(), size: size as u64, first_cluster: first, contiguous: false, loc: Vec::new(), mtime: crate::rtc::fat_timestamp() })
    }

    /// Free space in bytes (scans the FAT).
    pub fn count_free(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev) -> Result<u64, FsError> {
        let mut free = 0u64;
        for c in 2..self.clusters + 2 {
            if self.fat_get(io, dev, c)? == 0 {
                free += 1;
            }
        }
        Ok(free * self.cluster_bytes() as u64)
    }
}

/// Replacing files in place (for updating Solon's boot files).
impl Fat {
    /// Byte offsets of a file's directory records: (long-name records, short record) and
    /// the short record itself.
    fn records(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry, want: &str) -> Result<Option<(Vec<u64>, u64, [u8; 32])>, FsError> {
        let raw = self.dir_bytes(io, dev, dir)?;
        let chain = self.dir_clusters(io, dev, dir)?;
        let mut lfn: Vec<(usize, u8, [u16; 13])> = Vec::new();
        let mut lfn_sum = 0u8;
        for (i, e) in raw.chunks_exact(32).enumerate() {
            if e[0] == 0x00 {
                break;
            }
            if e[0] == 0xE5 {
                lfn.clear();
                continue;
            }
            if e[11] & 0x3F == 0x0F {
                let mut chars = [0u16; 13];
                for (k, off) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].iter().enumerate() {
                    chars[k] = le16(e, *off);
                }
                if e[0] & 0x40 != 0 {
                    lfn.clear();
                }
                lfn_sum = e[13];
                lfn.push((i, e[0] & 0x1F, chars));
                continue;
            }
            if e[11] & 0x08 != 0 {
                lfn.clear();
                continue;
            }
            let mut sum: u8 = 0;
            for &c in &e[0..11] {
                sum = (sum >> 1 | sum << 7).wrapping_add(c);
            }
            let has_lfn = !lfn.is_empty() && sum == lfn_sum;
            let name = if has_lfn {
                let mut parts = lfn.clone();
                parts.sort_by_key(|(_, ord, _)| *ord);
                let units: Vec<u16> = parts.iter().flat_map(|(_, _, c)| c.iter().copied()).collect();
                utf16_name(&units)
            } else {
                short_name(e)
            };
            if name.eq_ignore_ascii_case(want) {
                let mut offs = Vec::new();
                if has_lfn {
                    for (k, _, _) in &lfn {
                        offs.push(self.record_offset(dir, &chain, *k).ok_or(FsError::Corrupt)?);
                    }
                }
                let short = self.record_offset(dir, &chain, i).ok_or(FsError::Corrupt)?;
                return Ok(Some((offs, short, e.try_into().unwrap())));
            }
            lfn.clear();
        }
        Ok(None)
    }

    fn free_chain(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, first: u32) -> Result<(), FsError> {
        let chain = self.chain(io, dev, first, self.clusters as usize)?;
        if !chain.is_empty() {
            self.fat_set_many(io, dev, chain.into_iter().map(|c| (c, 0)).collect())?;
        }
        Ok(())
    }

    /// Delete a file (not a folder). Ok(false) if there's no such file.
    pub fn delete_file(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry, name: &str) -> Result<bool, FsError> {
        let Some((lfn, short, e)) = self.records(io, dev, dir, name)? else { return Ok(false) };
        if e[11] & 0x10 != 0 {
            return Err(FsError::Exists);
        }
        self.clear_fsinfo(io, dev)?;
        for o in lfn.iter().chain(core::iter::once(&short)) {
            io.write_at(dev, *o, &[0xE5])?;
        }
        dev.flush()?;
        let first = (le16(&e, 20) as u32) << 16 | le16(&e, 26) as u32;
        self.free_chain(io, dev, first)?;
        dev.flush()?;
        Ok(true)
    }

    /// Make file `target` use the data of file `source` (written beforehand), then remove
    /// `source`'s entry and `target`'s old data. The switch is one 32-byte record in one
    /// sector, so after a power cut either the old or the new data is there, never neither.
    pub fn swap_in(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry, target: &str, source: &str) -> Result<(), FsError> {
        let (_, t_short, t) = self.records(io, dev, dir, target)?.ok_or(FsError::Io)?;
        let (s_lfn, s_short, s) = self.records(io, dev, dir, source)?.ok_or(FsError::Io)?;
        self.clear_fsinfo(io, dev)?;
        let old_first = (le16(&t, 20) as u32) << 16 | le16(&t, 26) as u32;
        let mut new = t;
        new[20..22].copy_from_slice(&s[20..22]);
        new[26..28].copy_from_slice(&s[26..28]);
        new[28..32].copy_from_slice(&s[28..32]);
        io.write_at(dev, t_short, &new)?;
        dev.flush()?;
        for o in s_lfn.iter().chain(core::iter::once(&s_short)) {
            io.write_at(dev, *o, &[0xE5])?;
        }
        dev.flush()?;
        self.free_chain(io, dev, old_first)?;
        dev.flush()?;
        Ok(())
    }
}

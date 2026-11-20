//! FAT12 / FAT16 / FAT32, read-only, with long file names.

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
            out.push(DirEntry { name, is_dir, size: if is_dir { 0 } else { le32(e, 28) as u64 }, first_cluster, contiguous: false, loc: Vec::new() });
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

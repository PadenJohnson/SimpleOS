//! exFAT: read support for any drive, write support for Solon's own library partition,
//! and a formatter for setting that partition up.
//!
//! Writes are ordered so a power cut can't corrupt existing files: new data goes to free
//! clusters first, then the allocation bitmap / FAT are updated, and only then is the
//! directory entry written. An interrupted write at worst leaves a few clusters marked
//! used with nothing pointing at them.

use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

use super::{le16, le32, le64, utf16_name, BlockDev, DirEntry, FsError, VolumeIo};

const ATTR_DIR: u16 = 0x10;
const ATTR_ARCHIVE: u16 = 0x20;
const FLAG_ALLOC: u8 = 0x01;
const FLAG_NOFATCHAIN: u8 = 0x02;

struct WriteState {
    bitmap: Vec<u8>,
    bitmap_chain: Vec<u32>,
    upcase: Vec<u16>,
    /// Clusters of the up-case table (in use, but not reachable from any directory).
    upcase_chain: Vec<u32>,
    free: u64,
    hint: u32,
}

pub struct ExFat {
    sector_shift: u32,
    cluster_shift: u32, // sectors per cluster, as a shift
    fat_start: u64,     // sector
    heap_start: u64,    // sector of cluster 2
    clusters: u32,
    root_cluster: u32,
    rw: Option<WriteState>,
}

pub fn is_exfat(b: &[u8]) -> bool {
    b.len() >= 512 && &b[3..11] == b"EXFAT   " && b[510] == 0x55 && b[511] == 0xAA
}

// exFAT checksums: rotate right by one, then add (modulo 2^16 / 2^32).
fn sum16(sum: u16, b: u8) -> u16 {
    sum.rotate_right(1).wrapping_add(b as u16)
}
fn sum32(sum: u32, b: u8) -> u32 {
    sum.rotate_right(1).wrapping_add(b as u32)
}

/// The up-case table Solon writes: ASCII a-z map to A-Z, everything else to itself.
fn default_upcase() -> Vec<u16> {
    (0u16..128).map(|c| if (b'a' as u16..=b'z' as u16).contains(&c) { c - 32 } else { c }).collect()
}

impl ExFat {
    pub fn mount(dev: &mut dyn BlockDev, start: u64, b: &[u8]) -> Option<(ExFat, VolumeIo, String, u64)> {
        let sector_shift = b[108] as u32;
        let cluster_shift = b[109] as u32;
        if !(9..=12).contains(&sector_shift) || sector_shift + cluster_shift > 25 {
            return None;
        }
        if (1usize << sector_shift) < dev.block_size() {
            return None;
        }
        let fs = ExFat {
            sector_shift,
            cluster_shift,
            fat_start: le32(b, 80) as u64,
            heap_start: le32(b, 88) as u64,
            clusters: le32(b, 92),
            root_cluster: le32(b, 96),
            rw: None,
        };
        let size = le64(b, 72) << sector_shift;
        let mut io = VolumeIo::new(start, 1 << sector_shift);
        // Volume label lives in the root directory.
        let mut label = String::new();
        let root = DirEntry::dir(String::new(), fs.root_cluster);
        if let Ok((raw, _)) = fs.dir_bytes(&mut io, dev, &root) {
            for e in raw.chunks_exact(32) {
                match e[0] {
                    0x00 => break,
                    0x83 => {
                        let n = (e[1] as usize).min(11);
                        let units: Vec<u16> = (0..n).map(|i| le16(e, 2 + i * 2)).collect();
                        label = utf16_name(&units);
                        break;
                    }
                    _ => {}
                }
            }
        }
        Some((fs, io, label, size))
    }

    pub fn root_cluster(&self) -> u32 {
        self.root_cluster
    }

    fn cluster_bytes(&self) -> u64 {
        1u64 << (self.sector_shift + self.cluster_shift)
    }

    fn cluster_sector(&self, c: u32) -> u64 {
        self.heap_start + ((c as u64 - 2) << self.cluster_shift)
    }

    /// Byte offset of a cluster within the volume.
    fn cluster_offset(&self, c: u32) -> u64 {
        self.cluster_sector(c) << self.sector_shift
    }

    fn valid(&self, c: u32) -> bool {
        c >= 2 && c < self.clusters + 2
    }

    fn fat_offset(&self, c: u32) -> u64 {
        (self.fat_start << self.sector_shift) + c as u64 * 4
    }

    fn chain(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, first: u32, contiguous: bool, count: usize) -> Result<Vec<u32>, FsError> {
        let mut v = Vec::new();
        if !self.valid(first) {
            return Ok(v);
        }
        if contiguous {
            if first as u64 + count as u64 > self.clusters as u64 + 2 {
                return Err(FsError::Corrupt);
            }
            return Ok((0..count as u32).map(|i| first + i).collect());
        }
        let mut c = first;
        loop {
            v.push(c);
            if v.len() >= count {
                return Ok(v);
            }
            let mut b = [0u8; 4];
            io.read_at(dev, self.fat_offset(c), &mut b)?;
            let n = u32::from_le_bytes(b);
            if !self.valid(n) {
                return Ok(v); // end of chain (0xFFFFFFFF) or bad
            }
            c = n;
        }
    }

    fn read_clusters(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, chain: &[u32], out: &mut Vec<u8>) -> Result<(), FsError> {
        let cb = self.cluster_bytes() as usize;
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

    fn dir_chain(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry) -> Result<Vec<u32>, FsError> {
        let max = if dir.size > 0 {
            ((dir.size + self.cluster_bytes() - 1) / self.cluster_bytes()) as usize
        } else {
            (64 * 1024 * 1024 / self.cluster_bytes() as usize).max(1) // root: no recorded size
        };
        self.chain(io, dev, dir.first_cluster, dir.contiguous, max)
    }

    fn dir_bytes(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry) -> Result<(Vec<u8>, Vec<u32>), FsError> {
        let chain = self.dir_chain(io, dev, dir)?;
        let mut out = Vec::new();
        self.read_clusters(io, dev, &chain, &mut out)?;
        Ok((out, chain))
    }

    /// Volume byte offset of directory record `i` given the directory's cluster chain.
    fn record_offset(&self, chain: &[u32], i: usize) -> u64 {
        let cb = self.cluster_bytes() as usize;
        let byte = i * 32;
        self.cluster_offset(chain[byte / cb]) + (byte % cb) as u64
    }

    pub fn list(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry) -> Result<Vec<DirEntry>, FsError> {
        let (raw, chain) = self.dir_bytes(io, dev, dir)?;
        let ents: Vec<&[u8]> = raw.chunks_exact(32).collect();
        let mut out = Vec::new();
        let mut i = 0;
        while i < ents.len() {
            let e = ents[i];
            match e[0] {
                0x00 => break,
                0x85 => {
                    // File entry set: File, Stream Extension, then File Name entries.
                    let secondary = e[1] as usize;
                    let attrs = le16(e, 4);
                    if secondary < 2 || i + secondary >= ents.len() + 1 || ents.get(i + 1).map(|s| s[0]) != Some(0xC0) {
                        i += 1;
                        continue;
                    }
                    let s = ents[i + 1];
                    let name_len = s[3] as usize;
                    let flags = s[1];
                    let valid_len = le64(s, 8);
                    let first_cluster = le32(s, 20);
                    let data_len = le64(s, 24);
                    let mut units = Vec::with_capacity(name_len);
                    for k in 0..secondary - 1 {
                        let Some(n) = ents.get(i + 2 + k) else { break };
                        if n[0] != 0xC1 {
                            break;
                        }
                        for c in 0..15 {
                            if units.len() < name_len {
                                units.push(le16(n, 2 + c * 2));
                            }
                        }
                    }
                    let is_dir = attrs & ATTR_DIR != 0;
                    let loc = (i..=(i + secondary).min(ents.len() - 1)).map(|k| self.record_offset(&chain, k)).collect();
                    out.push(DirEntry {
                        name: utf16_name(&units),
                        is_dir,
                        size: if is_dir { data_len } else { valid_len },
                        first_cluster,
                        contiguous: flags & FLAG_NOFATCHAIN != 0,
                        loc,
                    });
                    i += 1 + secondary;
                }
                _ => i += 1, // bitmap, up-case table, label, deleted entries...
            }
        }
        Ok(out)
    }

    pub fn read_file(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, f: &DirEntry) -> Result<Vec<u8>, FsError> {
        if f.size == 0 {
            return Ok(Vec::new());
        }
        let cb = self.cluster_bytes();
        let need = ((f.size + cb - 1) / cb) as usize;
        let chain = self.chain(io, dev, f.first_cluster, f.contiguous, need)?;
        if chain.len() < need {
            return Err(FsError::Corrupt);
        }
        let mut out = Vec::with_capacity(need * cb as usize);
        self.read_clusters(io, dev, &chain, &mut out)?;
        out.truncate(f.size as usize);
        Ok(out)
    }

    // ---- writing ----------------------------------------------------------------------

    /// Load the allocation bitmap and up-case table so the volume can be modified.
    pub fn enable_writes(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev) -> Result<(), FsError> {
        let root = DirEntry::dir(String::new(), self.root_cluster);
        let (raw, _) = self.dir_bytes(io, dev, &root)?;
        let mut bitmap_at = None;
        let mut upcase_at = None;
        for e in raw.chunks_exact(32) {
            match e[0] {
                0x00 => break,
                0x81 if e[1] & 1 == 0 => bitmap_at = Some((le32(e, 20), le64(e, 24))),
                0x82 => upcase_at = Some((le32(e, 20), le64(e, 24))),
                _ => {}
            }
        }
        let (bm_first, bm_len) = bitmap_at.ok_or(FsError::Corrupt)?;
        let need = (self.clusters as u64 + 7) / 8;
        if bm_len < need {
            return Err(FsError::Corrupt);
        }
        let cb = self.cluster_bytes();
        let bm_clusters = ((bm_len + cb - 1) / cb) as usize;
        let bitmap_chain = self.chain(io, dev, bm_first, false, bm_clusters)?;
        let mut bitmap = Vec::new();
        self.read_clusters(io, dev, &bitmap_chain, &mut bitmap)?;
        bitmap.truncate(need as usize);

        let mut upcase = default_upcase();
        let mut upcase_chain = Vec::new();
        if let Some((uc_first, uc_len)) = upcase_at {
            let n = ((uc_len + cb - 1) / cb) as usize;
            let chain = self.chain(io, dev, uc_first, false, n)?;
            upcase_chain = chain.clone();
            let mut raw = Vec::new();
            self.read_clusters(io, dev, &chain, &mut raw)?;
            raw.truncate(uc_len as usize);
            // Decode (possibly compressed: 0xFFFF, n = "n identity entries").
            upcase.clear();
            let vals: Vec<u16> = raw.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
            let mut k = 0;
            while k < vals.len() && upcase.len() < 0x10000 {
                if vals[k] == 0xFFFF && k + 1 < vals.len() {
                    for _ in 0..vals[k + 1] {
                        let c = upcase.len() as u16;
                        upcase.push(c);
                    }
                    k += 2;
                } else {
                    upcase.push(vals[k]);
                    k += 1;
                }
            }
        }
        let used: u64 = (0..self.clusters as usize).filter(|&c| bitmap[c / 8] & (1 << (c % 8)) != 0).count() as u64;
        self.rw = Some(WriteState { bitmap, bitmap_chain, upcase, upcase_chain, free: self.clusters as u64 - used, hint: 2 });
        Ok(())
    }

    pub fn free_bytes(&self) -> u64 {
        self.rw.as_ref().map(|w| w.free * self.cluster_bytes()).unwrap_or(0)
    }

    fn up(&self, c: u16) -> u16 {
        let w = self.rw.as_ref();
        w.and_then(|w| w.upcase.get(c as usize).copied()).unwrap_or(c)
    }

    fn is_used(w: &WriteState, c: u32) -> bool {
        let i = (c - 2) as usize;
        w.bitmap[i / 8] & (1 << (i % 8)) != 0
    }

    /// Find `n` free clusters, preferring one contiguous run. Doesn't mark them yet.
    fn find_free(&self, n: usize) -> Result<Vec<u32>, FsError> {
        let w = self.rw.as_ref().ok_or(FsError::ReadOnly)?;
        if (n as u64) > w.free {
            return Err(FsError::Full);
        }
        let end = self.clusters + 2;
        // Contiguous run, starting at the hint and wrapping once.
        let mut run_start = 0u32;
        let mut run = 0usize;
        for pass in 0..2 {
            let (from, to) = if pass == 0 { (w.hint, end) } else { (2, w.hint) };
            run = 0;
            for c in from..to {
                if Self::is_used(w, c) {
                    run = 0;
                } else {
                    if run == 0 {
                        run_start = c;
                    }
                    run += 1;
                    if run == n {
                        return Ok((run_start..run_start + n as u32).collect());
                    }
                }
            }
        }
        let _ = run;
        // Fragmented: any free clusters.
        let v: Vec<u32> = (2..end).filter(|&c| !Self::is_used(w, c)).take(n).collect();
        if v.len() < n {
            return Err(FsError::Full);
        }
        Ok(v)
    }

    /// Mark clusters used/free in memory and on disk.
    fn set_bits(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, clusters: &[u32], used: bool) -> Result<(), FsError> {
        let cb = self.cluster_bytes() as usize;
        let offsets: Vec<(usize, u64)> = {
            let w = self.rw.as_mut().ok_or(FsError::ReadOnly)?;
            let mut touched = Vec::new();
            for &c in clusters {
                let i = (c - 2) as usize;
                let was = w.bitmap[i / 8] & (1 << (i % 8)) != 0;
                if used {
                    w.bitmap[i / 8] |= 1 << (i % 8);
                } else {
                    w.bitmap[i / 8] &= !(1 << (i % 8));
                }
                if was != used {
                    if used {
                        w.free -= 1;
                    } else {
                        w.free += 1;
                    }
                }
                touched.push(i / 8);
                if used {
                    w.hint = c + 1;
                }
            }
            touched.sort_unstable();
            touched.dedup();
            touched.iter().map(|&b| (b, b as u64)).collect()
        };
        // Write each touched byte range (grouped into runs).
        let mut k = 0;
        while k < offsets.len() {
            let mut j = k;
            while j + 1 < offsets.len() && offsets[j + 1].0 == offsets[j].0 + 1 && (offsets[j + 1].0 % cb) != 0 {
                j += 1;
            }
            let (first, last) = (offsets[k].0, offsets[j].0);
            let (data, off) = {
                let w = self.rw.as_ref().unwrap();
                (w.bitmap[first..=last].to_vec(), self.cluster_offset(w.bitmap_chain[first / cb]) + (first % cb) as u64)
            };
            io.write_at(dev, off, &data)?;
            k = j + 1;
        }
        Ok(())
    }

    fn write_fat_chain(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, chain: &[u32]) -> Result<(), FsError> {
        for (i, &c) in chain.iter().enumerate() {
            let next = chain.get(i + 1).copied().unwrap_or(0xFFFF_FFFF);
            io.write_at(dev, self.fat_offset(c), &next.to_le_bytes())?;
        }
        Ok(())
    }

    /// Write data into the given clusters (padding the last one with zeros).
    fn write_clusters(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, chain: &[u32], data: &[u8]) -> Result<(), FsError> {
        let cb = self.cluster_bytes() as usize;
        let max_run = (64 * 1024 / cb).max(1);
        let mut i = 0;
        while i < chain.len() {
            let mut n = 1;
            while i + n < chain.len() && chain[i + n] == chain[i] + n as u32 && n < max_run {
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

    fn set_dirty(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dirty: bool) -> Result<(), FsError> {
        let mut f = [0u8; 2];
        io.read_at(dev, 106, &mut f)?;
        let v = u16::from_le_bytes(f);
        let v = if dirty { v | 2 } else { v & !2 };
        io.write_at(dev, 106, &v.to_le_bytes())
    }

    fn name_hash(&self, units: &[u16]) -> u16 {
        let mut h = 0u16;
        for &u in units {
            let u = self.up(u);
            h = sum16(h, u as u8);
            h = sum16(h, (u >> 8) as u8);
        }
        h
    }

    fn entry_set(&self, units: &[u16], is_dir: bool, first: u32, len: u64, nofat: bool) -> Vec<[u8; 32]> {
        let names = (units.len() + 14) / 15;
        let mut set = Vec::new();
        let mut f = [0u8; 32];
        f[0] = 0x85;
        f[1] = (1 + names) as u8;
        f[4..6].copy_from_slice(&(if is_dir { ATTR_DIR } else { ATTR_ARCHIVE }).to_le_bytes());
        let ts = crate::rtc::fat_timestamp();
        for off in [8usize, 12, 16] {
            f[off..off + 4].copy_from_slice(&ts.to_le_bytes());
        }
        set.push(f);
        let mut s = [0u8; 32];
        s[0] = 0xC0;
        s[1] = FLAG_ALLOC | if nofat { FLAG_NOFATCHAIN } else { 0 };
        s[3] = units.len() as u8;
        s[4..6].copy_from_slice(&self.name_hash(units).to_le_bytes());
        s[8..16].copy_from_slice(&len.to_le_bytes());
        s[20..24].copy_from_slice(&first.to_le_bytes());
        s[24..32].copy_from_slice(&len.to_le_bytes());
        set.push(s);
        for k in 0..names {
            let mut n = [0u8; 32];
            n[0] = 0xC1;
            for i in 0..15 {
                if let Some(&u) = units.get(k * 15 + i) {
                    n[2 + i * 2..4 + i * 2].copy_from_slice(&u.to_le_bytes());
                }
            }
            set.push(n);
        }
        Self::fix_checksum(&mut set);
        set
    }

    fn fix_checksum(set: &mut [[u8; 32]]) {
        let mut sum = 0u16;
        for (i, e) in set.iter().enumerate() {
            for (j, &b) in e.iter().enumerate() {
                if !(i == 0 && (j == 2 || j == 3)) {
                    sum = sum16(sum, b);
                }
            }
        }
        set[0][2..4].copy_from_slice(&sum.to_le_bytes());
    }

    /// Put an entry set into a directory, growing the directory if it's full.
    /// Returns the volume offsets of the written records.
    fn add_to_dir(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry, set: &[[u8; 32]]) -> Result<Vec<u64>, FsError> {
        // The caller's copy may be stale (the folder may have grown since it was listed).
        let mut dir = self.refresh(io, dev, dir)?;
        for _attempt in 0..2 {
            let (raw, chain) = self.dir_bytes(io, dev, &dir)?;
            let n = raw.len() / 32;
            let mut run = 0;
            for i in 0..n {
                if raw[i * 32] & 0x80 == 0 {
                    run += 1;
                    if run == set.len() {
                        let first = i + 1 - set.len();
                        let offs: Vec<u64> = (0..set.len()).map(|k| self.record_offset(&chain, first + k)).collect();
                        // Secondary entries first, the primary (0x85) last: the set only
                        // becomes visible once it's complete.
                        for k in (0..set.len()).rev() {
                            io.write_at(dev, offs[k], &set[k])?;
                        }
                        return Ok(offs);
                    }
                } else {
                    run = 0;
                }
            }
            dir = self.grow_dir(io, dev, &dir, &chain)?;
        }
        Err(FsError::Full)
    }

    /// Re-read a sub-directory's size and chain flag from its entry on disk.
    fn refresh(&self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry) -> Result<DirEntry, FsError> {
        let mut d = dir.clone();
        if let Some(&stream_at) = dir.loc.get(1) {
            let mut s = [0u8; 32];
            io.read_at(dev, stream_at, &mut s)?;
            if s[0] != 0xC0 {
                return Err(FsError::Corrupt);
            }
            d.contiguous = s[1] & FLAG_NOFATCHAIN != 0;
            d.first_cluster = le32(&s, 20);
            d.size = le64(&s, 24);
        }
        Ok(d)
    }

    /// Add one zeroed cluster to a directory. Returns the updated directory entry.
    fn grow_dir(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry, chain: &[u32]) -> Result<DirEntry, FsError> {
        let last = *chain.last().ok_or(FsError::Corrupt)?;
        let want_next = last + 1;
        let next = {
            let w = self.rw.as_ref().ok_or(FsError::ReadOnly)?;
            if self.valid(want_next) && !Self::is_used(w, want_next) {
                want_next
            } else {
                self.find_free(1)?[0]
            }
        };
        let cb = self.cluster_bytes() as usize;
        self.write_clusters(io, dev, &[next], &vec![0u8; cb])?;
        self.set_bits(io, dev, &[next], true)?;
        let mut new_chain = chain.to_vec();
        new_chain.push(next);
        let still_contiguous = dir.contiguous && next == last + 1;
        if !still_contiguous {
            self.write_fat_chain(io, dev, &new_chain)?;
        }
        let mut d = dir.clone();
        d.contiguous = still_contiguous;
        if !dir.loc.is_empty() {
            // Sub-directory: update its size (and chain flag) in the parent.
            d.size = (new_chain.len() * cb) as u64;
            let mut set: Vec<[u8; 32]> = Vec::new();
            for &o in &dir.loc {
                let mut e = [0u8; 32];
                io.read_at(dev, o, &mut e)?;
                set.push(e);
            }
            set[1][1] = FLAG_ALLOC | if still_contiguous { FLAG_NOFATCHAIN } else { 0 };
            set[1][8..16].copy_from_slice(&d.size.to_le_bytes());
            set[1][24..32].copy_from_slice(&d.size.to_le_bytes());
            Self::fix_checksum(&mut set);
            for (k, &o) in dir.loc.iter().enumerate() {
                io.write_at(dev, o, &set[k])?;
            }
        }
        Ok(d)
    }

    /// Create a file (`data` = Some) or an empty folder (`data` = None) in `dir`.
    pub fn create(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry, name: &str, data: Option<&[u8]>) -> Result<DirEntry, FsError> {
        if self.rw.is_none() {
            return Err(FsError::ReadOnly);
        }
        let units: Vec<u16> = name.encode_utf16().collect();
        if units.is_empty() || units.len() > 255 || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
            return Err(FsError::Exists);
        }
        let up: Vec<u16> = units.iter().map(|&u| self.up(u)).collect();
        for e in self.list(io, dev, dir)? {
            let other: Vec<u16> = e.name.encode_utf16().map(|u| self.up(u)).collect();
            if other == up {
                return Err(FsError::Exists);
            }
        }
        let cb = self.cluster_bytes();
        let is_dir = data.is_none();
        let len = match data {
            Some(d) => d.len() as u64,
            None => cb,
        };
        let n = ((len + cb - 1) / cb) as usize;
        self.set_dirty(io, dev, true)?;
        let chain = if n > 0 { self.find_free(n)? } else { Vec::new() };
        let contiguous = chain.windows(2).all(|w| w[1] == w[0] + 1);
        // 1. Data into free clusters.
        match data {
            Some(d) if n > 0 => self.write_clusters(io, dev, &chain, d)?,
            None => self.write_clusters(io, dev, &chain, &vec![0u8; cb as usize])?,
            _ => {}
        }
        dev.flush()?;
        // 2. Allocation.
        if n > 0 {
            self.set_bits(io, dev, &chain, true)?;
            if !contiguous {
                self.write_fat_chain(io, dev, &chain)?;
            }
        }
        dev.flush()?;
        // 3. Directory entry.
        let first = chain.first().copied().unwrap_or(0);
        let set = self.entry_set(&units, is_dir, first, len, n > 0 && contiguous);
        let loc = self.add_to_dir(io, dev, dir, &set)?;
        dev.flush()?;
        self.set_dirty(io, dev, false)?;
        dev.flush()?;
        Ok(DirEntry { name: String::from(name), is_dir, size: len, first_cluster: first, contiguous: n > 0 && contiguous, loc })
    }

    /// Start writing a big file in pieces: reserve its space now (marked in use so nothing
    /// else takes it); `write_part` fills it; `finish` adds it to the folder. If Solon stops
    /// part way, the reserved space shows up in the health check, which can free it.
    pub fn begin(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, dir: &DirEntry, name: &str, len: u64) -> Result<Pending, FsError> {
        if self.rw.is_none() {
            return Err(FsError::ReadOnly);
        }
        let units: Vec<u16> = name.encode_utf16().collect();
        if units.is_empty() || units.len() > 255 || name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
            return Err(FsError::Exists);
        }
        let up: Vec<u16> = units.iter().map(|&u| self.up(u)).collect();
        for e in self.list(io, dev, dir)? {
            let other: Vec<u16> = e.name.encode_utf16().map(|u| self.up(u)).collect();
            if other == up {
                return Err(FsError::Exists);
            }
        }
        let cb = self.cluster_bytes();
        let n = ((len + cb - 1) / cb) as usize;
        let chain = if n > 0 { self.find_free(n)? } else { Vec::new() };
        self.set_dirty(io, dev, true)?;
        if n > 0 {
            self.set_bits(io, dev, &chain, true)?;
        }
        dev.flush()?;
        Ok(Pending { dir: dir.clone(), name: String::from(name), units, len, chain })
    }

    /// Write `data` at `offset` (a multiple of the piece size used, which must be a whole
    /// number of clusters) of a file started with `begin`.
    pub fn write_part(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, p: &Pending, offset: u64, data: &[u8]) -> Result<(), FsError> {
        let cb = self.cluster_bytes();
        if offset % cb != 0 || offset + data.len() as u64 > p.len {
            return Err(FsError::Io);
        }
        let first = (offset / cb) as usize;
        let count = ((data.len() as u64 + cb - 1) / cb) as usize;
        self.write_clusters(io, dev, &p.chain[first..first + count], data)
    }

    /// Add the finished file to its folder.
    pub fn finish(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, p: Pending) -> Result<DirEntry, FsError> {
        dev.flush()?;
        let contiguous = p.chain.windows(2).all(|w| w[1] == w[0] + 1);
        if !p.chain.is_empty() && !contiguous {
            self.write_fat_chain(io, dev, &p.chain)?;
        }
        dev.flush()?;
        let first = p.chain.first().copied().unwrap_or(0);
        let nofat = !p.chain.is_empty() && contiguous;
        let set = self.entry_set(&p.units, false, first, p.len, nofat);
        let loc = self.add_to_dir(io, dev, &p.dir, &set)?;
        dev.flush()?;
        self.set_dirty(io, dev, false)?;
        dev.flush()?;
        Ok(DirEntry { name: p.name, is_dir: false, size: p.len, first_cluster: first, contiguous: nofat, loc })
    }

    /// Give back the space of a file that won't be finished.
    pub fn abort(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, p: Pending) -> Result<(), FsError> {
        if !p.chain.is_empty() {
            self.set_bits(io, dev, &p.chain, false)?;
        }
        dev.flush()?;
        self.set_dirty(io, dev, false)?;
        dev.flush()
    }

    /// Rename and/or move an entry into `dest`, keeping its data where it is: a new
    /// directory entry set is written first, then the old one is hidden. (exFAT folders have
    /// no "." / ".." entries, so folders move the same way as files.)
    pub fn relink(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, e: &DirEntry, dest: &DirEntry, new_name: &str) -> Result<DirEntry, FsError> {
        if self.rw.is_none() {
            return Err(FsError::ReadOnly);
        }
        if e.loc.len() < 2 {
            return Err(FsError::Corrupt);
        }
        let units: Vec<u16> = new_name.encode_utf16().collect();
        if units.is_empty() || units.len() > 255 || new_name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
            return Err(FsError::Exists);
        }
        let up: Vec<u16> = units.iter().map(|&u| self.up(u)).collect();
        for other in self.list(io, dev, dest)? {
            let o: Vec<u16> = other.name.encode_utf16().map(|u| self.up(u)).collect();
            // Renaming to a different case of the same name is fine.
            if o == up && other.loc != e.loc {
                return Err(FsError::Exists);
            }
        }
        // Read the current stream extension to keep the exact lengths.
        let mut s = [0u8; 32];
        io.read_at(dev, e.loc[1], &mut s)?;
        let len = le64(&s, 24);
        let valid = le64(&s, 8);
        self.set_dirty(io, dev, true)?;
        let mut set = self.entry_set(&units, e.is_dir, e.first_cluster, len, e.contiguous);
        set[1][1] = s[1]; // same allocation flags
        set[1][8..16].copy_from_slice(&valid.to_le_bytes());
        Self::fix_checksum(&mut set);
        let loc = self.add_to_dir(io, dev, dest, &set)?;
        dev.flush()?;
        for &o in &e.loc {
            let mut t = [0u8; 1];
            io.read_at(dev, o, &mut t)?;
            io.write_at(dev, o, &[t[0] & 0x7F])?;
        }
        dev.flush()?;
        self.set_dirty(io, dev, false)?;
        dev.flush()?;
        Ok(DirEntry { name: String::from(new_name), loc, ..e.clone() })
    }

    /// Delete a file, or a folder and everything in it.
    pub fn delete_tree(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, e: &DirEntry) -> Result<(), FsError> {
        if e.is_dir {
            for child in self.list(io, dev, e)? {
                self.delete_tree(io, dev, &child)?;
            }
        }
        self.delete(io, dev, e)
    }

    /// Check the volume: every cluster a file or folder uses must be marked in use, and
    /// clusters marked in use must belong to something. With `repair`, space nothing uses
    /// (left behind by a power cut mid-write) is freed and unmarked used space is marked.
    pub fn check(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, repair: bool) -> Result<CheckReport, FsError> {
        let (bitmap_chain, upcase_chain) = {
            let w = self.rw.as_ref().ok_or(FsError::ReadOnly)?;
            (w.bitmap_chain.clone(), w.upcase_chain.clone())
        };
        let n = self.clusters as usize + 2;
        let mut owner = alloc::vec![false; n];
        let mut rep = CheckReport::default();
        let cb = self.cluster_bytes();
        let mark = |c: u32, what: &str, rep: &mut CheckReport, owner: &mut Vec<bool>| {
            if (c as usize) < n {
                if owner[c as usize] && rep.problems.len() < 20 {
                    rep.problems.push(alloc::format!("{} shares space with another item", what));
                }
                owner[c as usize] = true;
            }
        };
        for &c in bitmap_chain.iter().chain(upcase_chain.iter()) {
            mark(c, "system data", &mut rep, &mut owner);
        }
        let root = DirEntry::dir(String::new(), self.root_cluster);
        for c in self.dir_chain(io, dev, &root)? {
            mark(c, "the top folder", &mut rep, &mut owner);
        }
        let mut stack = alloc::vec![root];
        while let Some(dir) = stack.pop() {
            for e in self.list(io, dev, &dir)? {
                let need = ((e.size + cb - 1) / cb) as usize;
                if need > 0 {
                    let chain = self.chain(io, dev, e.first_cluster, e.contiguous, need)?;
                    if chain.len() < need && rep.problems.len() < 20 {
                        rep.problems.push(alloc::format!("\"{}\" is shorter than it should be", e.name));
                    }
                    for c in chain {
                        mark(c, &e.name, &mut rep, &mut owner);
                    }
                }
                if e.is_dir {
                    rep.folders += 1;
                    stack.push(e);
                } else {
                    rep.files += 1;
                }
            }
        }
        let (mut leaked, mut unmarked) = (Vec::new(), Vec::new());
        {
            let w = self.rw.as_ref().unwrap();
            for c in 2..n as u32 {
                let used = Self::is_used(w, c);
                match (used, owner[c as usize]) {
                    (true, false) => leaked.push(c),
                    (false, true) => unmarked.push(c),
                    _ => {}
                }
            }
        }
        rep.leaked_bytes = leaked.len() as u64 * cb;
        rep.unmarked = unmarked.len();
        if repair && (!leaked.is_empty() || !unmarked.is_empty()) {
            self.set_dirty(io, dev, true)?;
            if !leaked.is_empty() {
                self.set_bits(io, dev, &leaked, false)?;
            }
            if !unmarked.is_empty() {
                self.set_bits(io, dev, &unmarked, true)?;
            }
            dev.flush()?;
            self.set_dirty(io, dev, false)?;
            dev.flush()?;
            rep.repaired = true;
        }
        Ok(rep)
    }

    /// Delete a file or empty folder.
    pub fn delete(&mut self, io: &mut VolumeIo, dev: &mut dyn BlockDev, e: &DirEntry) -> Result<(), FsError> {
        if self.rw.is_none() {
            return Err(FsError::ReadOnly);
        }
        if e.loc.is_empty() {
            return Err(FsError::Corrupt);
        }
        if e.is_dir && !self.list(io, dev, e)?.is_empty() {
            return Err(FsError::Exists);
        }
        self.set_dirty(io, dev, true)?;
        // Hide the entry first (clear the in-use bit of every record), then free its clusters.
        for &o in &e.loc {
            let mut t = [0u8; 1];
            io.read_at(dev, o, &mut t)?;
            io.write_at(dev, o, &[t[0] & 0x7F])?;
        }
        dev.flush()?;
        if e.size > 0 && self.valid(e.first_cluster) {
            let cb = self.cluster_bytes();
            let n = ((e.size + cb - 1) / cb) as usize;
            let chain = self.chain(io, dev, e.first_cluster, e.contiguous, n)?;
            self.set_bits(io, dev, &chain, false)?;
        }
        dev.flush()?;
        self.set_dirty(io, dev, false)?;
        dev.flush()
    }
}

/// Format `blocks` device blocks starting at `start` as an empty exFAT volume.
pub fn format(dev: &mut dyn BlockDev, start: u64, blocks: u64, label: &str, serial: u32) -> Result<(), FsError> {
    let bs = dev.block_size();
    let sector_shift = bs.trailing_zeros();
    let bytes = blocks * bs as u64;
    // 32 KiB clusters (4 KiB on small volumes).
    let cluster_bytes: u64 = if bytes >= 256 * 1024 * 1024 { 32 * 1024 } else { 4096 };
    let cluster_bytes = cluster_bytes.max(bs as u64);
    let cluster_shift = (cluster_bytes / bs as u64).trailing_zeros();
    let spc = 1u64 << cluster_shift;
    let align = ((1024 * 1024) / bs as u64).max(spc);
    let fat_offset = align;
    let est_clusters = (blocks - fat_offset) / spc;
    let fat_length = ((est_clusters + 2) * 4 + bs as u64 - 1) / bs as u64;
    let heap_offset = (fat_offset + fat_length + align - 1) / align * align;
    if heap_offset + 4 * spc > blocks {
        return Err(FsError::Full);
    }
    let clusters = ((blocks - heap_offset) / spc).min(0xFFFF_FFF5) as u32;

    // Layout of the heap: allocation bitmap, up-case table, root directory.
    let bitmap_len = (clusters as u64 + 7) / 8;
    let bitmap_clusters = ((bitmap_len + cluster_bytes - 1) / cluster_bytes) as u32;
    let upcase: Vec<u8> = default_upcase().iter().flat_map(|c| c.to_le_bytes()).collect();
    let upcase_cluster = 2 + bitmap_clusters;
    let root_cluster = upcase_cluster + 1;
    let used = root_cluster - 1; // clusters 2..=root_cluster

    let mut io = VolumeIo::new(start, bs);
    let csec = |c: u32| heap_offset + ((c as u64 - 2) << cluster_shift);

    // FAT: zero it all, then chains for the three system structures.
    let zero = vec![0u8; 64 * 1024];
    let mut s = 0u64;
    while s < fat_length {
        let n = ((fat_length - s) as usize * bs).min(zero.len()) / bs;
        io.write_sectors(dev, fat_offset + s, &zero[..n * bs])?;
        s += n as u64;
    }
    let mut fat = vec![0u32; root_cluster as usize + 1];
    fat[0] = 0xFFFF_FFF8;
    fat[1] = 0xFFFF_FFFF;
    for c in 2..2 + bitmap_clusters {
        fat[c as usize] = if c + 1 < 2 + bitmap_clusters { c + 1 } else { 0xFFFF_FFFF };
    }
    fat[upcase_cluster as usize] = 0xFFFF_FFFF;
    fat[root_cluster as usize] = 0xFFFF_FFFF;
    let fat_bytes: Vec<u8> = fat.iter().flat_map(|v| v.to_le_bytes()).collect();
    io.write_at(dev, fat_offset * bs as u64, &fat_bytes)?;

    // Bitmap: zeroed, with the first `used` clusters marked.
    let mut bitmap = vec![0u8; (bitmap_clusters as u64 * cluster_bytes) as usize];
    for i in 0..used as usize {
        bitmap[i / 8] |= 1 << (i % 8);
    }
    for (k, chunk) in bitmap.chunks(64 * 1024).enumerate() {
        let sec = csec(2) + (k * 64 * 1024 / bs) as u64;
        io.write_sectors(dev, sec, chunk)?;
    }

    // Up-case table
    let mut uc = vec![0u8; cluster_bytes as usize];
    uc[..upcase.len()].copy_from_slice(&upcase);
    io.write_sectors(dev, csec(upcase_cluster), &uc)?;
    let mut uc_sum = 0u32;
    for &b in &upcase {
        uc_sum = sum32(uc_sum, b);
    }

    // Root directory: label, bitmap, up-case entries.
    let mut root = vec![0u8; cluster_bytes as usize];
    root[0] = 0x83;
    let lu: Vec<u16> = label.encode_utf16().take(11).collect();
    root[1] = lu.len() as u8;
    for (i, u) in lu.iter().enumerate() {
        root[2 + i * 2..4 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    root[32] = 0x81;
    root[32 + 20..32 + 24].copy_from_slice(&2u32.to_le_bytes());
    root[32 + 24..32 + 32].copy_from_slice(&bitmap_len.to_le_bytes());
    root[64] = 0x82;
    root[64 + 4..64 + 8].copy_from_slice(&uc_sum.to_le_bytes());
    root[64 + 20..64 + 24].copy_from_slice(&upcase_cluster.to_le_bytes());
    root[64 + 24..64 + 32].copy_from_slice(&(upcase.len() as u64).to_le_bytes());
    io.write_sectors(dev, csec(root_cluster), &root)?;

    // Boot regions (main at sector 0, backup at 12), written last.
    let mut region = vec![0u8; 12 * bs];
    {
        let b = &mut region[..bs];
        b[0..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
        b[3..11].copy_from_slice(b"EXFAT   ");
        b[64..72].copy_from_slice(&start.to_le_bytes());
        b[72..80].copy_from_slice(&blocks.to_le_bytes());
        b[80..84].copy_from_slice(&(fat_offset as u32).to_le_bytes());
        b[84..88].copy_from_slice(&(fat_length as u32).to_le_bytes());
        b[88..92].copy_from_slice(&(heap_offset as u32).to_le_bytes());
        b[92..96].copy_from_slice(&clusters.to_le_bytes());
        b[96..100].copy_from_slice(&root_cluster.to_le_bytes());
        b[100..104].copy_from_slice(&serial.to_le_bytes());
        b[104..106].copy_from_slice(&0x0100u16.to_le_bytes());
        b[108] = sector_shift as u8;
        b[109] = cluster_shift as u8;
        b[110] = 1;
        b[111] = 0x80;
        b[112] = 0xFF;
        b[510] = 0x55;
        b[511] = 0xAA;
    }
    for s in 1..9 {
        region[s * bs + bs - 2] = 0x55;
        region[s * bs + bs - 1] = 0xAA;
    }
    let mut sum = 0u32;
    for (i, &b) in region[..11 * bs].iter().enumerate() {
        if i != 106 && i != 107 && i != 112 {
            sum = sum32(sum, b);
        }
    }
    for k in 0..bs / 4 {
        region[11 * bs + k * 4..11 * bs + k * 4 + 4].copy_from_slice(&sum.to_le_bytes());
    }
    io.write_sectors(dev, 12, &region)?;
    io.write_sectors(dev, 0, &region)?;
    dev.flush()?;
    kprintln!("exfat: formatted {} clusters of {} KiB, label \"{}\"", clusters, cluster_bytes / 1024, label);
    Ok(())
}

/// Result of a storage health check.
#[derive(Debug, Default, Clone)]
pub struct CheckReport {
    pub files: usize,
    pub folders: usize,
    /// Space marked in use that no file or folder owns (freed by a repair).
    pub leaked_bytes: u64,
    /// Clusters files use that weren't marked in use (marked by a repair).
    pub unmarked: usize,
    /// Things a repair can't safely fix automatically.
    pub problems: Vec<String>,
    pub repaired: bool,
}

impl CheckReport {
    pub fn healthy(&self) -> bool {
        self.leaked_bytes == 0 && self.unmarked == 0 && self.problems.is_empty()
    }
}

/// A big file being written in pieces (see `ExFat::begin`).
pub struct Pending {
    dir: DirEntry,
    name: String,
    units: Vec<u16>,
    pub len: u64,
    chain: Vec<u32>,
}

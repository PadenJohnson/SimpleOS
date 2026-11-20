//! File systems on block devices: partition tables, FAT12/16/32 and exFAT (read-only),
//! plus exFAT writing for Solon's own library partition.

pub mod exfat;
pub mod fat;
pub mod part;

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec;
use alloc::vec::Vec;

/// Anything that can read whole blocks (a USB drive, an internal disk...).
/// Writing is opt-in: devices that don't implement it (USB drives) can never be written.
pub trait BlockDev {
    fn block_size(&self) -> usize;
    /// Read `buf.len() / block_size` blocks starting at `lba`.
    fn read_blocks(&mut self, lba: u64, buf: &mut [u8]) -> Result<(), FsError>;
    /// Write `buf.len() / block_size` blocks starting at `lba`.
    fn write_blocks(&mut self, _lba: u64, _buf: &[u8]) -> Result<(), FsError> {
        Err(FsError::ReadOnly)
    }
    /// Make sure everything written so far is on stable storage.
    fn flush(&mut self) -> Result<(), FsError> {
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsError {
    Io,
    Corrupt,
    TooBig,
    ReadOnly,
    Full,
    Exists,
}

impl FsError {
    pub fn message(self) -> &'static str {
        match self {
            FsError::Io => "Couldn't read the drive",
            FsError::Corrupt => "The drive's file system looks damaged",
            FsError::TooBig => "File is too large",
            FsError::ReadOnly => "This drive is read-only",
            FsError::Full => "Solon storage is full",
            FsError::Exists => "A file with that name already exists",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FsKind {
    Fat12,
    Fat16,
    Fat32,
    ExFat,
}

impl FsKind {
    pub fn name(self) -> &'static str {
        match self {
            FsKind::Fat12 => "FAT12",
            FsKind::Fat16 => "FAT16",
            FsKind::Fat32 => "FAT32",
            FsKind::ExFat => "exFAT",
        }
    }
}

/// A file or folder. `first_cluster`/`contiguous` locate its data on the volume.
#[derive(Debug, Clone)]
pub struct DirEntry {
    pub name: String,
    pub is_dir: bool,
    pub size: u64,
    pub first_cluster: u32,
    /// exFAT "NoFatChain": data is in consecutive clusters, the FAT isn't used.
    pub contiguous: bool,
    /// Volume byte offsets of this entry's directory records (exFAT; used to update/delete it).
    pub loc: Vec<u64>,
}

impl DirEntry {
    pub fn dir(name: String, first_cluster: u32) -> DirEntry {
        DirEntry { name, is_dir: true, size: 0, first_cluster, contiguous: false, loc: Vec::new() }
    }
}

/// Largest file we'll load into memory.
pub const MAX_FILE: u64 = 128 * 1024 * 1024;

/// Sector-addressed access to one volume (partition) with a small cache.
pub struct VolumeIo {
    /// First device block of the volume.
    pub start: u64,
    /// File system sector size in bytes.
    pub sector: usize,
    cache: VecDeque<(u64, Vec<u8>)>,
}

const CACHE_SECTORS: usize = 64;

impl VolumeIo {
    pub fn new(start: u64, sector: usize) -> Self {
        VolumeIo { start, sector, cache: VecDeque::new() }
    }

    fn dev_lba(&self, dev: &dyn BlockDev, sector: u64) -> u64 {
        self.start + sector * (self.sector / dev.block_size()) as u64
    }

    /// One sector through the cache (for FAT tables and directories).
    pub fn sector(&mut self, dev: &mut dyn BlockDev, n: u64) -> Result<&[u8], FsError> {
        if let Some(i) = self.cache.iter().position(|(s, _)| *s == n) {
            return Ok(&self.cache[i].1);
        }
        let mut buf = vec![0u8; self.sector];
        dev.read_blocks(self.dev_lba(dev, n), &mut buf)?;
        if self.cache.len() == CACHE_SECTORS {
            self.cache.pop_front();
        }
        self.cache.push_back((n, buf));
        Ok(&self.cache.back().unwrap().1)
    }

    /// Bytes at an arbitrary byte offset in the volume (may span sectors).
    pub fn read_at(&mut self, dev: &mut dyn BlockDev, offset: u64, out: &mut [u8]) -> Result<(), FsError> {
        let ss = self.sector as u64;
        let mut done = 0;
        while done < out.len() {
            let pos = offset + done as u64;
            let sec = self.sector(dev, pos / ss)?;
            let within = (pos % ss) as usize;
            let n = (sec.len() - within).min(out.len() - done);
            out[done..done + n].copy_from_slice(&sec[within..within + n]);
            done += n;
        }
        Ok(())
    }

    /// Many consecutive sectors straight into `out`, bypassing the cache (file data).
    pub fn read_sectors(&mut self, dev: &mut dyn BlockDev, first: u64, out: &mut [u8]) -> Result<(), FsError> {
        let lba = self.dev_lba(dev, first);
        dev.read_blocks(lba, out)
    }

    /// Write whole sectors (bypassing the cache, but dropping stale cached copies).
    pub fn write_sectors(&mut self, dev: &mut dyn BlockDev, first: u64, data: &[u8]) -> Result<(), FsError> {
        let n = (data.len() / self.sector) as u64;
        self.cache.retain(|(s, _)| *s < first || *s >= first + n);
        let lba = self.dev_lba(dev, first);
        dev.write_blocks(lba, data)
    }

    /// Write bytes at any offset (read-modify-write of the sectors involved).
    pub fn write_at(&mut self, dev: &mut dyn BlockDev, offset: u64, data: &[u8]) -> Result<(), FsError> {
        let ss = self.sector as u64;
        let mut done = 0;
        while done < data.len() {
            let pos = offset + done as u64;
            let n_sec = pos / ss;
            let within = (pos % ss) as usize;
            let mut sec = self.sector(dev, n_sec)?.to_vec();
            let n = (sec.len() - within).min(data.len() - done);
            sec[within..within + n].copy_from_slice(&data[done..done + n]);
            let lba = self.dev_lba(dev, n_sec);
            dev.write_blocks(lba, &sec)?;
            if let Some(i) = self.cache.iter().position(|(s, _)| *s == n_sec) {
                self.cache[i].1 = sec;
            }
            done += n;
        }
        Ok(())
    }
}

pub enum FsImpl {
    Fat(fat::Fat),
    ExFat(exfat::ExFat),
}

/// A mounted volume. Read-only unless `enable_writes` succeeded (Solon's exFAT library only).
pub struct Volume {
    pub kind: FsKind,
    pub label: String,
    pub size: u64,
    /// `enable_writes` has run (exFAT bitmap loaded).
    pub writes_ready: bool,
    io: VolumeIo,
    fs: FsImpl,
}

impl Volume {
    pub fn root(&self) -> DirEntry {
        let first_cluster = match &self.fs {
            FsImpl::Fat(f) => f.root_cluster(),
            FsImpl::ExFat(e) => e.root_cluster(),
        };
        DirEntry::dir(String::new(), first_cluster)
    }

    /// Prepare for writing (exFAT loads its allocation bitmap; FAT needs nothing).
    /// The device passed to write calls decides whether writing is actually possible.
    pub fn enable_writes(&mut self, dev: &mut dyn BlockDev) -> Result<(), FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.enable_writes(&mut self.io, dev)?,
            FsImpl::Fat(_) => {}
        }
        self.writes_ready = true;
        Ok(())
    }

    pub fn free_bytes(&self) -> u64 {
        match &self.fs {
            FsImpl::ExFat(e) => e.free_bytes(),
            FsImpl::Fat(_) => 0,
        }
    }

    /// Free space, reading the allocation data if needed (may take a moment on big drives).
    pub fn count_free(&mut self, dev: &mut dyn BlockDev) -> Result<u64, FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => {
                if e.free_bytes() == 0 {
                    e.enable_writes(&mut self.io, dev)?;
                }
                Ok(e.free_bytes())
            }
            FsImpl::Fat(f) => f.count_free(&mut self.io, dev),
        }
    }

    /// First device block of the volume.
    pub fn start(&self) -> u64 {
        self.io.start
    }

    /// Rename and/or move an entry (exFAT: Solon storage).
    pub fn relink(&mut self, dev: &mut dyn BlockDev, entry: &DirEntry, dest: &DirEntry, name: &str) -> Result<DirEntry, FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.relink(&mut self.io, dev, entry, dest, name),
            FsImpl::Fat(_) => Err(FsError::ReadOnly),
        }
    }

    /// Delete a file or a whole folder (exFAT: Solon storage).
    pub fn delete_tree(&mut self, dev: &mut dyn BlockDev, entry: &DirEntry) -> Result<(), FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.delete_tree(&mut self.io, dev, entry),
            FsImpl::Fat(_) => Err(FsError::ReadOnly),
        }
    }

    /// Health check (exFAT); `repair` fixes what's safe to fix.
    pub fn check(&mut self, dev: &mut dyn BlockDev, repair: bool) -> Result<exfat::CheckReport, FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.check(&mut self.io, dev, repair),
            FsImpl::Fat(_) => Err(FsError::ReadOnly),
        }
    }

    /// Find an entry by name (case-insensitive) in a directory.
    pub fn find(&mut self, dev: &mut dyn BlockDev, dir: &DirEntry, name: &str) -> Result<Option<DirEntry>, FsError> {
        Ok(self.list(dev, dir)?.into_iter().find(|e| e.name.eq_ignore_ascii_case(name)))
    }

    pub fn create_file(&mut self, dev: &mut dyn BlockDev, dir: &DirEntry, name: &str, data: &[u8]) -> Result<DirEntry, FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.create(&mut self.io, dev, dir, name, Some(data)),
            FsImpl::Fat(f) => f.create(&mut self.io, dev, dir, name, Some(data)),
        }
    }

    pub fn mkdir(&mut self, dev: &mut dyn BlockDev, dir: &DirEntry, name: &str) -> Result<DirEntry, FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.create(&mut self.io, dev, dir, name, None),
            FsImpl::Fat(f) => f.create(&mut self.io, dev, dir, name, None),
        }
    }

    pub fn delete(&mut self, dev: &mut dyn BlockDev, entry: &DirEntry) -> Result<(), FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.delete(&mut self.io, dev, entry),
            FsImpl::Fat(_) => Err(FsError::ReadOnly),
        }
    }

    pub fn list(&mut self, dev: &mut dyn BlockDev, dir: &DirEntry) -> Result<Vec<DirEntry>, FsError> {
        let mut v = match &mut self.fs {
            FsImpl::Fat(f) => f.list(&mut self.io, dev, dir)?,
            FsImpl::ExFat(e) => e.list(&mut self.io, dev, dir)?,
        };
        // Folders first, then by name (case-insensitive, numbers in natural order).
        v.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| natural_cmp(&a.name, &b.name)));
        Ok(v)
    }

    pub fn read_file(&mut self, dev: &mut dyn BlockDev, file: &DirEntry) -> Result<Vec<u8>, FsError> {
        if file.size > MAX_FILE {
            return Err(FsError::TooBig);
        }
        match &mut self.fs {
            FsImpl::Fat(f) => f.read_file(&mut self.io, dev, file),
            FsImpl::ExFat(e) => e.read_file(&mut self.io, dev, file),
        }
    }
}

/// Try to mount a FAT or exFAT volume starting at device block `start`.
pub fn mount(dev: &mut dyn BlockDev, start: u64, size_hint: u64) -> Option<Volume> {
    let bs = dev.block_size();
    let mut boot = vec![0u8; bs];
    dev.read_blocks(start, &mut boot).ok()?;
    if exfat::is_exfat(&boot) {
        let (fs, io, label, size) = exfat::ExFat::mount(dev, start, &boot)?;
        return Some(Volume { kind: FsKind::ExFat, label, size, writes_ready: false, io, fs: FsImpl::ExFat(fs) });
    }
    if fat::is_fat(&boot) {
        let (fs, io, label, size) = fat::Fat::mount(dev, start, &boot)?;
        let kind = fs.kind();
        let _ = size_hint;
        return Some(Volume { kind, label, size, writes_ready: false, io, fs: FsImpl::Fat(fs) });
    }
    None
}

/// Find and mount every readable volume on a device.
pub fn probe(dev: &mut dyn BlockDev, total_blocks: u64) -> Vec<Volume> {
    let mut vols = Vec::new();
    for (start, len) in part::partitions(dev, total_blocks) {
        if let Some(v) = mount(dev, start, len) {
            vols.push(v);
        }
    }
    vols
}

/// "IMG_2.jpg" before "IMG_10.jpg"; case-insensitive.
pub fn natural_cmp(a: &str, b: &str) -> core::cmp::Ordering {
    use core::cmp::Ordering;
    let (mut x, mut y) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (x.peek().copied(), y.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, _) => return Ordering::Less,
            (_, None) => return Ordering::Greater,
            (Some(c), Some(d)) if c.is_ascii_digit() && d.is_ascii_digit() => {
                let mut n1: u64 = 0;
                while let Some(c) = x.peek().copied().filter(|c| c.is_ascii_digit()) {
                    n1 = n1.saturating_mul(10).saturating_add(c as u64 - '0' as u64);
                    x.next();
                }
                let mut n2: u64 = 0;
                while let Some(d) = y.peek().copied().filter(|c| c.is_ascii_digit()) {
                    n2 = n2.saturating_mul(10).saturating_add(d as u64 - '0' as u64);
                    y.next();
                }
                if n1 != n2 {
                    return n1.cmp(&n2);
                }
            }
            (Some(c), Some(d)) => {
                let (lc, ld) = (c.to_ascii_lowercase(), d.to_ascii_lowercase());
                if lc != ld {
                    return lc.cmp(&ld);
                }
                x.next();
                y.next();
            }
        }
    }
}

pub fn le16(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
pub fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
pub fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// Decode UTF-16LE code units, stopping at NUL / 0xFFFF padding.
pub fn utf16_name(units: &[u16]) -> String {
    let end = units.iter().position(|&u| u == 0 || u == 0xFFFF).unwrap_or(units.len());
    char::decode_utf16(units[..end].iter().copied()).map(|r| r.unwrap_or('?')).collect()
}

impl Volume {
    /// Start a big file written in pieces (exFAT only: Solon storage).
    pub fn begin_file(&mut self, dev: &mut dyn BlockDev, dir: &DirEntry, name: &str, len: u64) -> Result<exfat::Pending, FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.begin(&mut self.io, dev, dir, name, len),
            FsImpl::Fat(_) => Err(FsError::ReadOnly),
        }
    }
    pub fn write_part(&mut self, dev: &mut dyn BlockDev, p: &exfat::Pending, offset: u64, data: &[u8]) -> Result<(), FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.write_part(&mut self.io, dev, p, offset, data),
            FsImpl::Fat(_) => Err(FsError::ReadOnly),
        }
    }
    pub fn finish_file(&mut self, dev: &mut dyn BlockDev, p: exfat::Pending) -> Result<DirEntry, FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.finish(&mut self.io, dev, p),
            FsImpl::Fat(_) => Err(FsError::ReadOnly),
        }
    }
    pub fn abort_file(&mut self, dev: &mut dyn BlockDev, p: exfat::Pending) -> Result<(), FsError> {
        match &mut self.fs {
            FsImpl::ExFat(e) => e.abort(&mut self.io, dev, p),
            FsImpl::Fat(_) => Err(FsError::ReadOnly),
        }
    }
}

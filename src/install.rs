//! Installing Solon on an internal disk, from the USB stick it was started from.
//!
//! 1. The stick's boot area is copied block for block: the protective MBR (Limine's BIOS
//!    first stage), the shortened partition array with Limine's BIOS code after it, the boot
//!    partition (Limine for UEFI, its config and the kernel) and the second half of Limine's
//!    BIOS code just past it. So the disk starts on UEFI and old BIOS PCs like the stick does.
//! 2. The disk gets its own partition table (whole disk, boot partition only, new IDs).
//! 3. Solon storage is created after that and everything in the stick's Solon storage is
//!    copied in (photos, documents, settings, the Recycle bin, hidden originals).
//!
//! The whole disk is erased: the user confirms that twice first.

use alloc::collections::VecDeque;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{part, BlockDev, DirEntry, FsError, Volume};
use crate::library::Storage;
use crate::usb::Usb;

/// Blocks copied per step (1 MiB of 512-byte blocks).
const CHUNK: u64 = 2048;
/// Files bigger than this are copied a piece at a time.
const PIECE: usize = 8 * 1024 * 1024;
/// Smallest disk worth installing on.
pub const MIN_DISK: u64 = 2 * 1024 * 1024 * 1024;

enum Stage {
    CopyBoot { next: u64 },
    Table,
    Storage,
    Files,
    Rekey,
    Done,
}

pub struct Install {
    pub disk: usize,
    stick: u32,
    stick_gpt: part::Gpt,
    esp_slot: usize,
    /// Blocks copied from the stick (its boot area).
    copy_end: u64,
    total: u64,
    stage: Stage,
    vol: Option<Volume>,
    /// (file or folder in the stick's storage, folder on the disk)
    queue: VecDeque<(DirEntry, DirEntry)>,
    /// A big file being copied: (source, space on the disk, next offset)
    stream: Option<(DirEntry, crate::fs::exfat::Pending, u64)>,
    /// (old first cluster, size) -> new first cluster, to re-link edited photos' originals.
    moved: Vec<((u32, u64), u32)>,
    pub files_done: usize,
    pub files_failed: usize,
    files_total: usize,
}

/// Why installing isn't possible on disk `i` right now (None = it is).
pub fn blocker(storage: &Storage, usb: &Usb, i: usize) -> Option<&'static str> {
    if usb.boot_drive().is_none() {
        return Some("Solon can only be installed while it runs from its USB stick.");
    }
    let (_, blocks, bs) = storage.disk_info(i)?;
    if bs != 512 {
        return Some("This disk uses 4K sectors, which the installer doesn't support yet.");
    }
    if blocks * (bs as u64) < MIN_DISK {
        return Some("This disk is too small to install Solon on.");
    }
    if storage.library_on_disk(i) {
        return Some("Your Solon storage is on this disk, so it can't be erased. Copy your photos to a USB drive first.");
    }
    None
}

fn random_guid(salt: u64) -> [u8; 16] {
    let mut x = unsafe { core::arch::x86_64::_rdtsc() } ^ salt.rotate_left(17) ^ (crate::rtc::fat_timestamp() as u64) << 32;
    let mut g = [0u8; 16];
    for b in g.iter_mut() {
        // xorshift
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        *b = (x >> 24) as u8;
    }
    g[7] = (g[7] & 0x0F) | 0x40;
    g[8] = (g[8] & 0x3F) | 0x80;
    g
}

impl Install {
    pub fn new(storage: &mut Storage, usb: &mut Usb, disk: usize) -> Result<Install, &'static str> {
        if let Some(why) = blocker(storage, usb, disk) {
            return Err(why);
        }
        let stick = usb.boot_drive().ok_or("Solon's USB stick isn't plugged in")?;
        let (mut dev, stick_total) = usb.boot_disk(stick).ok_or("Solon's USB stick isn't plugged in")?;
        let g = part::read_gpt(&mut dev, stick_total).ok_or("The USB stick's partition table can't be read")?;
        if !g.valid {
            return Err("The USB stick's partition table is damaged");
        }
        let (slot, _first, last) = part::esp(&g).ok_or("The USB stick has no boot partition")?;
        let copy_end = last + 1 + 4096; // the boot partition and the 2 MiB after it
        let total = storage.disk_info(disk).ok_or("disk not found")?.1;
        kprintln!("install: disk {} ({} blocks) from stick (boot area {} blocks)", disk, total, copy_end);
        Ok(Install {
            disk,
            stick,
            stick_gpt: g,
            esp_slot: slot,
            copy_end,
            total,
            stage: Stage::CopyBoot { next: 0 },
            vol: None,
            queue: VecDeque::new(),
            stream: None,
            moved: Vec::new(),
            files_done: 0,
            files_failed: 0,
            files_total: 0,
        })
    }

    pub fn finished(&self) -> bool {
        matches!(self.stage, Stage::Done)
    }

    /// What's happening, and how far along (0..=100).
    pub fn progress(&self) -> (String, u32) {
        match &self.stage {
            Stage::CopyBoot { next } => (String::from("Copying Solon onto the disk..."), (*next * 40 / self.copy_end.max(1)) as u32),
            Stage::Table | Stage::Storage => (String::from("Setting up Solon storage..."), 42),
            Stage::Files | Stage::Rekey => {
                let pct = 45 + (self.files_done * 55 / self.files_total.max(1)) as u32;
                (format!("Copying your photos and files ({} of {})...", self.files_done, self.files_total.max(self.files_done)), pct.min(99))
            }
            Stage::Done => (String::from("Done"), 100),
        }
    }

    /// Do the next piece of work. Err stops the install.
    pub fn step(&mut self, storage: &mut Storage, usb: &mut Usb) -> Result<(), String> {
        match self.stage {
            Stage::CopyBoot { next } => {
                let n = CHUNK.min(self.copy_end - next);
                let mut buf = alloc::vec![0u8; n as usize * 512];
                {
                    let (mut dev, _) = usb.boot_disk(self.stick).ok_or("The USB stick was removed")?;
                    dev.read_blocks(next, &mut buf).map_err(|_| "Couldn't read the USB stick")?;
                }
                storage
                    .with_disk(self.disk, |d| d.write_blocks(next, &buf))
                    .ok_or("disk not found")?
                    .map_err(|_| "Couldn't write to the disk")?;
                let next = next + n;
                self.stage = if next >= self.copy_end { Stage::Table } else { Stage::CopyBoot { next } };
            }
            Stage::Table => {
                let (total, disk_guid, esp_guid) = (self.total, random_guid(1), random_guid(2));
                storage
                    .with_disk(self.disk, |d| part::install_table(d, total, &self.stick_gpt, self.esp_slot, disk_guid, esp_guid))
                    .ok_or("disk not found")?
                    .map_err(|e| format!("Couldn't write the partition table: {}", e.message()))?;
                self.stage = Stage::Storage;
            }
            Stage::Storage => {
                let vol = storage.create_volume_on(self.disk, self.copy_end).map_err(|e| format!("Couldn't create Solon storage: {}", e.message()))?;
                // Everything in the stick's storage goes into the new one.
                if storage.library.is_some() {
                    let src_root = storage.root(usb).map_err(|e| String::from(e.message()))?;
                    let dst_root = vol.root();
                    for e in storage.list(usb, &src_root).unwrap_or_default() {
                        self.queue.push_back((e, dst_root.clone()));
                    }
                    self.files_total = count_files(storage, usb, &src_root);
                }
                kprintln!("install: Solon storage created; {} file(s) to copy", self.files_total);
                self.vol = Some(vol);
                self.stage = Stage::Files;
            }
            Stage::Files => {
                if self.stream.is_some() {
                    return self.step_stream(storage, usb);
                }
                let Some((src, dst)) = self.queue.pop_front() else {
                    self.stage = Stage::Rekey;
                    return Ok(());
                };
                let vol = self.vol.as_mut().unwrap();
                if src.is_dir {
                    // "Photos" was made with the storage; reuse folders that exist.
                    let existing = storage.with_disk(self.disk, |d| vol.find(d, &dst, &src.name)).ok_or("disk not found")?;
                    let made = match existing {
                        Ok(Some(e)) if e.is_dir => e,
                        _ => storage.with_disk(self.disk, |d| vol.mkdir(d, &dst, &src.name)).ok_or("disk not found")?.map_err(|e| format!("Couldn't copy \"{}\": {}", src.name, e.message()))?,
                    };
                    for child in storage.list(usb, &src).unwrap_or_default() {
                        self.queue.push_back((child, made.clone()));
                    }
                } else if src.size as usize > PIECE {
                    let p = storage.with_disk(self.disk, |d| vol.begin_file(d, &dst, &src.name, src.size)).ok_or("disk not found")?;
                    match p {
                        Ok(p) => self.stream = Some((src, p, 0)),
                        Err(e) => {
                            kprintln!("install: couldn't copy \"{}\": {:?}", src.name, e);
                            self.files_failed += 1;
                            if e == FsError::Full {
                                return Err(String::from("The disk is full"));
                            }
                        }
                    }
                } else {
                    let r = storage.read_file(usb, &src).and_then(|data| storage.with_disk(self.disk, |d| vol.create_file(d, &dst, &src.name, &data)).unwrap_or(Err(FsError::Io)));
                    match r {
                        Ok(new) => {
                            self.moved.push(((src.first_cluster, src.size), new.first_cluster));
                            self.files_done += 1;
                        }
                        Err(e) => {
                            kprintln!("install: couldn't copy \"{}\": {:?}", src.name, e);
                            self.files_failed += 1;
                            if e == FsError::Full {
                                return Err(String::from("The disk is full"));
                            }
                        }
                    }
                }
            }
            Stage::Rekey => {
                self.rekey(storage);
                kprintln!("install: finished ({} file(s) copied, {} failed)", self.files_done, self.files_failed);
                self.stage = Stage::Done;
            }
            Stage::Done => {}
        }
        Ok(())
    }

    fn step_stream(&mut self, storage: &mut Storage, usb: &mut Usb) -> Result<(), String> {
        let (src, p, off) = self.stream.take().unwrap();
        let vol = self.vol.as_mut().unwrap();
        let part = match storage.read_part(usb, &src, off, PIECE) {
            Ok(b) if !b.is_empty() => b,
            r => {
                kprintln!("install: reading \"{}\" failed: {:?}", src.name, r.err());
                let _ = storage.with_disk(self.disk, |d| vol.abort_file(d, p));
                self.files_failed += 1;
                return Ok(());
            }
        };
        if let Some(Err(e)) = storage.with_disk(self.disk, |d| vol.write_part(d, &p, off, &part)) {
            kprintln!("install: writing \"{}\" failed: {:?}", src.name, e);
            let _ = storage.with_disk(self.disk, |d| vol.abort_file(d, p));
            self.files_failed += 1;
            return Ok(());
        }
        let next = off + part.len() as u64;
        if next < p.len {
            self.stream = Some((src, p, next));
        } else {
            match storage.with_disk(self.disk, |d| vol.finish_file(d, p)) {
                Some(Ok(new)) => {
                    self.moved.push(((src.first_cluster, src.size), new.first_cluster));
                    self.files_done += 1;
                }
                _ => self.files_failed += 1,
            }
        }
        Ok(())
    }

    /// Edited photos' originals are named after where the photo's data was on the stick;
    /// give them the photo's new place on the disk.
    fn rekey(&mut self, storage: &mut Storage) {
        let vol = self.vol.as_mut().unwrap();
        let root = vol.root();
        let Some(Ok(Some(orig))) = storage.with_disk(self.disk, |d| vol.find(d, &root, "Originals")) else { return };
        let Some(Ok(list)) = storage.with_disk(self.disk, |d| vol.list(d, &orig)) else { return };
        for b in list.iter().filter(|b| !b.is_dir) {
            let Some((key, rest)) = b.name.split_once(' ') else { continue };
            let Some((c, s)) = key.split_once('-') else { continue };
            let (Ok(c), Ok(s)) = (c.parse::<u32>(), s.parse::<u64>()) else { continue };
            let Some(&(_, new_c)) = self.moved.iter().find(|(old, _)| *old == (c, s)) else { continue };
            let name = format!("{}-{} {}", new_c, s, rest);
            if let Some(Err(e)) = storage.with_disk(self.disk, |d| vol.relink(d, b, &orig, &name)) {
                kprintln!("install: couldn't re-link original \"{}\": {:?}", b.name, e);
            }
        }
    }
}

fn count_files(storage: &mut Storage, usb: &mut Usb, dir: &DirEntry) -> usize {
    let mut n = 0;
    let mut stack = Vec::from([dir.clone()]);
    while let Some(d) = stack.pop() {
        for e in storage.list(usb, &d).unwrap_or_default() {
            if e.is_dir {
                stack.push(e);
            } else {
                n += 1;
            }
        }
    }
    n
}

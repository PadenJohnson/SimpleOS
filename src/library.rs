//! Solon's library: an exFAT volume in Solon's own partition, either on an internal disk
//! or on the USB stick Solon booted from. This is the only place in Solon that writes to
//! any disk.
//!
//! Layout:  /Photos/<YYYY-MM-DD> <source>/<files>      (what Photos shows)
//!          /Originals/<folder>/<file>                  (hidden originals of replaced photos)

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::Ordering::Relaxed;

use crate::disk::{self, InternalDisk};
use crate::fs::{self, part, BlockDev, DirEntry, FsError, Volume};
use crate::interrupts::TICKS;
use crate::usb::Usb;

const MIN_SIZE: u64 = 64 * 1024 * 1024;
const LABEL: &str = "SOLON";
/// Recycle bin folder (at the top of the library) and its index of where items came from.
pub const BIN: &str = "Recycle bin";
const BIN_INDEX: &str = "RecycleBin.txt";

/// A partition, for the Disk manager.
pub struct PartView {
    pub start: u64,
    pub blocks: u64,
    pub format: String,
    pub label: String,
    pub free: Option<u64>,
    /// Solon library partition.
    pub solon: bool,
    /// The Solon storage in use right now.
    pub active: bool,
}

/// A disk, for the Disk manager.
pub struct DiskView {
    pub internal: Option<usize>,
    pub usb: Option<u32>,
    pub name: String,
    pub kind: &'static str,
    pub bytes: u64,
    pub blocks: u64,
    pub block: usize,
    pub parts: Vec<PartView>,
    /// Free space Solon storage could be set up in.
    pub can_setup: Option<u64>,
    /// Solon's own boot stick.
    pub boot: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Backend {
    /// Index into the internal disk list.
    Internal(usize),
    /// Solon's own boot stick (USB drive id).
    BootStick(u32),
}

pub struct Library {
    backend: Backend,
    vol: Volume,
    pub disk_name: String,
}

/// A disk with room for a new library partition.
pub struct SetupOption {
    pub disk: usize,
    pub disk_name: String,
    pub bytes: u64,
    /// The disk is completely empty (no partition table yet).
    pub blank: bool,
}

pub struct Storage {
    disks: Vec<InternalDisk>,
    pub library: Option<Library>,
}

#[allow(dead_code)]
pub enum SaveResult {
    Saved(DirEntry),
    /// Same name and size already in the folder.
    AlreadyThere,
}

fn new_guid() -> ([u8; 16], u32) {
    let seed = TICKS.load(Relaxed) as u32 ^ crate::rtc::fat_timestamp();
    let mut unique = [0u8; 16];
    for (i, b) in unique.iter_mut().enumerate() {
        *b = (seed.rotate_left(i as u32 * 5).wrapping_mul(0x9E37_79B9) >> 13) as u8;
    }
    unique[7] = (unique[7] & 0x0F) | 0x40; // version 4 GUID
    unique[8] = (unique[8] & 0x3F) | 0x80;
    (unique, seed)
}

/// Mount (and allow writing to) the first Solon partition on a device.
fn mount_solon(dev: &mut dyn BlockDev, total: u64, name: &str) -> Option<Volume> {
    for (start, len) in part::solon_partitions(dev, total) {
        match fs::mount(dev, start, len) {
            Some(mut vol) => match vol.enable_writes(dev) {
                Ok(()) => {
                    kprintln!("library: on {} ({} of {} free)", name, disk::human(vol.free_bytes()), disk::human(vol.size));
                    return Some(vol);
                }
                Err(e) => kprintln!("library: partition on {} can't be written: {:?}", name, e),
            },
            None => kprintln!("library: Solon partition on {} has no readable file system", name),
        }
    }
    None
}

/// Create the Solon partition in free space, format it, add /Photos.
fn create_on(dev: &mut dyn BlockDev, total: u64, name: &str, min_start: u64) -> Result<Volume, FsError> {
    let (unique, seed) = new_guid();
    let (start, len) = part::create_solon_partition(dev, total, unique, min_start)?;
    fs::exfat::format(dev, start, len, LABEL, seed)?;
    let mut vol = fs::mount(dev, start, len).ok_or(FsError::Corrupt)?;
    vol.enable_writes(dev)?;
    let root = vol.root();
    vol.mkdir(dev, &root, "Photos")?;
    kprintln!("library: created on {} ({})", name, disk::human(vol.size));
    Ok(vol)
}

impl Storage {
    /// Find the library: Solon's own boot stick first (setting it up on first boot),
    /// then internal disks.
    pub fn init(usb: &mut Usb) -> Storage {
        let mut disks = disk::scan();
        let mut library = None;

        if let Some(id) = usb.boot_drive() {
            if let Some((mut dev, total)) = usb.boot_disk(id) {
                let name = "this Solon USB stick";
                let mut vol = mount_solon(&mut dev, total, name);
                if vol.is_none() {
                    // First boot from a freshly written stick: use the rest of it for the library.
                    let grown = part::grow_gpt(&mut dev, total);
                    if let Err(e) = grown {
                        kprintln!("library: can't extend the stick's partition table: {:?}", e);
                    }
                    let lay = part::layout(&mut dev, total);
                    let min_start = part::boot_stick_min_start(&lay, dev.block_size());
                    let room = part::largest_free(&lay, total, dev.block_size(), min_start).map(|(_, l)| l * dev.block_size() as u64);
                    match room {
                        Some(bytes) if bytes >= MIN_SIZE => match create_on(&mut dev, total, name, min_start) {
                            Ok(v) => vol = Some(v),
                            Err(e) => kprintln!("library: couldn't set up the stick: {:?}", e),
                        },
                        _ => kprintln!("library: no room left on the stick for a library"),
                    }
                }
                if let Some(vol) = vol {
                    library = Some(Library { backend: Backend::BootStick(id), vol, disk_name: String::from("Solon USB stick") });
                }
            }
        }

        if library.is_none() {
            for (i, d) in disks.iter_mut().enumerate() {
                let (total, name) = (d.blocks, d.name.clone());
                if let Some(vol) = mount_solon(d, total, &name) {
                    library = Some(Library { backend: Backend::Internal(i), vol, disk_name: name });
                    break;
                }
            }
        }
        if library.is_none() {
            kprintln!("library: none yet");
        }
        Storage { disks, library }
    }

    /// Disks where a library could be created (largest free space first).
    pub fn setup_options(&mut self) -> Vec<SetupOption> {
        let mut out = Vec::new();
        for (i, d) in self.disks.iter_mut().enumerate() {
            let (total, bs) = (d.blocks, d.block_size);
            let lay = part::layout(d, total);
            let blank = matches!(lay, part::Layout::Blank);
            if let Some((_, len)) = part::largest_free(&lay, total, bs, 0) {
                let bytes = len * bs as u64;
                if bytes >= MIN_SIZE {
                    out.push(SetupOption { disk: i, disk_name: d.name.clone(), bytes, blank });
                }
            }
        }
        out.sort_by(|a, b| b.bytes.cmp(&a.bytes));
        out
    }

    /// Create the Solon partition in an internal disk's free space (user confirmed).
    pub fn create_library(&mut self, disk: usize) -> Result<(), FsError> {
        if self.library.is_some() {
            return Err(FsError::Exists);
        }
        let d = self.disks.get_mut(disk).ok_or(FsError::Io)?;
        let (total, name) = (d.blocks, d.name.clone());
        let vol = create_on(d, total, &name, 0)?;
        self.library = Some(Library { backend: Backend::Internal(disk), vol, disk_name: name });
        Ok(())
    }

    /// Run `f` with the library volume and its (writable) device.
    fn with<R>(&mut self, usb: &mut Usb, f: impl FnOnce(&mut Volume, &mut dyn BlockDev) -> Result<R, FsError>) -> Result<R, FsError> {
        let lib = self.library.as_mut().ok_or(FsError::ReadOnly)?;
        match lib.backend {
            Backend::Internal(i) => {
                let d = self.disks.get_mut(i).ok_or(FsError::Io)?;
                f(&mut lib.vol, d)
            }
            Backend::BootStick(id) => {
                let (mut dev, _) = usb.boot_disk(id).ok_or(FsError::Io)?;
                f(&mut lib.vol, &mut dev)
            }
        }
    }

    /// If the library is on Solon's boot stick: (drive id, first block of its volume).
    pub fn on_boot_stick(&self) -> Option<(u32, u64)> {
        match self.library.as_ref()?.backend {
            Backend::BootStick(id) => Some((id, self.library.as_ref()?.vol.start())),
            Backend::Internal(_) => None,
        }
    }

    pub fn free_bytes(&self) -> u64 {
        self.library.as_ref().map(|l| l.vol.free_bytes()).unwrap_or(0)
    }
    pub fn size(&self) -> u64 {
        self.library.as_ref().map(|l| l.vol.size).unwrap_or(0)
    }

    // ---- Files app operations -----------------------------------------------------------

    pub fn root(&mut self, usb: &mut Usb) -> Result<DirEntry, FsError> {
        self.with(usb, |vol, _| Ok(vol.root()))
    }

    pub fn mkdir_in(&mut self, usb: &mut Usb, parent: &DirEntry, name: &str) -> Result<DirEntry, FsError> {
        self.with(usb, |vol, d| vol.mkdir(d, parent, name))
    }

    /// Rename an item in place (`parent` = the folder it's in).
    pub fn rename(&mut self, usb: &mut Usb, e: &DirEntry, parent: &DirEntry, new_name: &str) -> Result<DirEntry, FsError> {
        self.with(usb, |vol, d| vol.relink(d, e, parent, new_name))
    }

    /// Move an item into another folder (renamed " (2)" etc. if the name is taken).
    pub fn move_to(&mut self, usb: &mut Usb, e: &DirEntry, dest: &DirEntry) -> Result<DirEntry, FsError> {
        let existing = self.names_in(usb, dest)?;
        let name = unique_name(&existing, &e.name, u64::MAX).ok_or(FsError::Exists)?;
        self.with(usb, |vol, d| vol.relink(d, e, dest, &name))
    }

    fn bin_dir(&mut self, usb: &mut Usb, create: bool) -> Result<Option<DirEntry>, FsError> {
        self.with(usb, |vol, d| {
            let root = vol.root();
            match vol.find(d, &root, BIN)? {
                Some(e) => Ok(Some(e)),
                None if create => vol.mkdir(d, &root, BIN).map(Some),
                None => Ok(None),
            }
        })
    }

    /// Recycle bin index: lines of "name in the bin<TAB>folder it came from".
    fn bin_index(&mut self, usb: &mut Usb) -> Vec<(String, String)> {
        let text = self.read_root_file(usb, BIN_INDEX).unwrap_or_default();
        core::str::from_utf8(&text)
            .unwrap_or("")
            .lines()
            .filter_map(|l| l.split_once('\t').map(|(a, b)| (String::from(a), String::from(b))))
            .collect()
    }

    fn save_bin_index(&mut self, usb: &mut Usb, idx: &[(String, String)]) -> Result<(), FsError> {
        let text: String = idx.iter().map(|(a, b)| format!("{}\t{}\n", a, b)).collect();
        self.write_root_file(usb, BIN_INDEX, text.as_bytes())
    }

    /// Move an item to the Recycle bin. `from` is the path of the folder it was in
    /// (e.g. "Photos/2026-11-20 VACATION"), used to put it back.
    pub fn recycle(&mut self, usb: &mut Usb, e: &DirEntry, from: &str) -> Result<(), FsError> {
        let bin = self.bin_dir(usb, true)?.ok_or(FsError::Io)?;
        let moved = self.move_to(usb, e, &bin)?;
        let mut idx = self.bin_index(usb);
        idx.retain(|(n, _)| !n.eq_ignore_ascii_case(&moved.name));
        idx.push((moved.name.clone(), String::from(from)));
        self.save_bin_index(usb, &idx)
    }

    /// What's in the Recycle bin: (item, folder it came from).
    pub fn bin_items(&mut self, usb: &mut Usb) -> Vec<(DirEntry, String)> {
        let Ok(Some(bin)) = self.bin_dir(usb, false) else { return Vec::new() };
        let idx = self.bin_index(usb);
        let items = self.list(usb, &bin).unwrap_or_default();
        items
            .into_iter()
            .map(|e| {
                let from = idx.iter().find(|(n, _)| n.eq_ignore_ascii_case(&e.name)).map(|(_, p)| p.clone()).unwrap_or_default();
                (e, from)
            })
            .collect()
    }

    /// Put an item back where it came from (recreating folders if needed).
    pub fn restore(&mut self, usb: &mut Usb, e: &DirEntry, from: &str) -> Result<String, FsError> {
        let mut dir = self.root(usb)?;
        for part in from.split('/').filter(|p| !p.is_empty()) {
            let next = self.with(usb, |vol, d| match vol.find(d, &dir, part)? {
                Some(x) if x.is_dir => Ok(x),
                _ => vol.mkdir(d, &dir, part),
            })?;
            dir = next;
        }
        let moved = self.move_to(usb, e, &dir)?;
        let mut idx = self.bin_index(usb);
        idx.retain(|(n, _)| !n.eq_ignore_ascii_case(&e.name));
        self.save_bin_index(usb, &idx)?;
        Ok(moved.name)
    }

    /// Permanently delete one item from the Recycle bin.
    pub fn delete_forever(&mut self, usb: &mut Usb, e: &DirEntry) -> Result<(), FsError> {
        self.with(usb, |vol, d| vol.delete_tree(d, e))?;
        let mut idx = self.bin_index(usb);
        idx.retain(|(n, _)| !n.eq_ignore_ascii_case(&e.name));
        self.save_bin_index(usb, &idx)
    }

    pub fn empty_bin(&mut self, usb: &mut Usb) -> Result<usize, FsError> {
        let items = self.bin_items(usb);
        for (e, _) in &items {
            self.with(usb, |vol, d| vol.delete_tree(d, e))?;
        }
        self.save_bin_index(usb, &[])?;
        Ok(items.len())
    }

    /// Storage health check; `repair` fixes what's safe to fix.
    pub fn check(&mut self, usb: &mut Usb, repair: bool) -> Result<fs::exfat::CheckReport, FsError> {
        self.with(usb, |vol, d| vol.check(d, repair))
    }

    /// Internal disks for the Disk manager.
    pub fn disk_views(&mut self) -> Vec<DiskView> {
        let lib = self.library.as_ref().map(|l| (l.backend, l.vol.start(), l.vol.free_bytes()));
        let mut out = Vec::new();
        for (i, d) in self.disks.iter_mut().enumerate() {
            let (total, bs) = (d.blocks, d.block_size);
            let solon: Vec<u64> = part::solon_partitions(d, total).iter().map(|p| p.0).collect();
            let mut parts = Vec::new();
            for (start, len) in part::partitions(d, total) {
                let active = matches!(lib, Some((Backend::Internal(li), ls, _)) if li == i && ls == start);
                let mut pv = PartView { start, blocks: len, format: String::from("Unknown"), label: String::new(), free: None, solon: solon.contains(&start), active };
                if let Some(mut v) = fs::mount(d, start, len) {
                    pv.format = String::from(v.kind.name());
                    pv.label = v.label.clone();
                    pv.free = match lib {
                        Some((Backend::Internal(li), ls, lf)) if li == i && ls == start => Some(lf),
                        _ => v.count_free(d).ok(),
                    };
                }
                parts.push(pv);
            }
            let lay = part::layout(d, total);
            let setup = part::largest_free(&lay, total, bs, 0).map(|(_, l)| l * bs as u64).filter(|&b| b >= MIN_SIZE);
            out.push(DiskView { internal: Some(i), usb: None, name: d.name.clone(), kind: d.kind, bytes: d.size_bytes(), blocks: total, block: bs, parts, can_setup: setup, boot: false });
        }
        out
    }

    /// Read a small file from the top of the library (e.g. Settings.txt).
    pub fn read_root_file(&mut self, usb: &mut Usb, name: &str) -> Option<Vec<u8>> {
        self.with(usb, |vol, d| {
            let root = vol.root();
            match vol.find(d, &root, name)? {
                Some(e) if !e.is_dir => vol.read_file(d, &e).map(Some),
                _ => Ok(None),
            }
        })
        .ok()
        .flatten()
    }

    /// Replace a small file at the top of the library.
    pub fn write_root_file(&mut self, usb: &mut Usb, name: &str, data: &[u8]) -> Result<(), FsError> {
        self.with(usb, |vol, d| {
            let root = vol.root();
            if let Some(old) = vol.find(d, &root, name)? {
                vol.delete(d, &old)?;
            }
            vol.create_file(d, &root, name, data).map(|_| ())
        })
    }

    pub fn photos_dir(&mut self, usb: &mut Usb) -> Result<DirEntry, FsError> {
        self.with(usb, |vol, d| {
            let root = vol.root();
            match vol.find(d, &root, "Photos")? {
                Some(e) if e.is_dir => Ok(e),
                _ => vol.mkdir(d, &root, "Photos"),
            }
        })
    }

    /// A folder for one import, e.g. "2026-11-19 VACATION" (reused if it exists).
    pub fn import_folder(&mut self, usb: &mut Usb, source: &str) -> Result<DirEntry, FsError> {
        let photos = self.photos_dir(usb)?;
        let (y, m, day) = crate::rtc::date();
        let clean: String = source.chars().filter(|c| !"/\\:*?\"<>|".contains(*c)).collect();
        let name = format!("{:04}-{:02}-{:02} {}", y, m, day, clean.trim());
        self.with(usb, |vol, d| match vol.find(d, &photos, &name)? {
            Some(e) if e.is_dir => Ok(e),
            _ => vol.mkdir(d, &photos, &name),
        })
    }

    /// Create a file whose name the caller already made unique.
    pub fn create_in(&mut self, usb: &mut Usb, dir: &DirEntry, name: &str, data: &[u8]) -> Result<DirEntry, FsError> {
        self.with(usb, |vol, d| vol.create_file(d, dir, name, data))
    }

    pub fn list(&mut self, usb: &mut Usb, dir: &DirEntry) -> Result<Vec<DirEntry>, FsError> {
        self.with(usb, |vol, d| vol.list(d, dir))
    }

    pub fn read_file(&mut self, usb: &mut Usb, file: &DirEntry) -> Result<Vec<u8>, FsError> {
        let data = self.with(usb, |vol, d| vol.read_file(d, file))?;
        let mut h: u32 = 0x811C_9DC5;
        for &b in &data {
            h = (h ^ b as u32).wrapping_mul(0x0100_0193);
        }
        kprintln!("library: read \"{}\" {} bytes fnv {:08x}", file.name, data.len(), h);
        Ok(data)
    }

    pub fn delete(&mut self, usb: &mut Usb, e: &DirEntry) -> Result<(), FsError> {
        self.with(usb, |vol, d| vol.delete(d, e))
    }

    /// Every photo in Solon storage as (folder, file), except the Recycle bin and the
    /// hidden originals of edited photos. The Photos folder comes first, newest import first.
    pub fn all_photos(&mut self, usb: &mut Usb) -> Vec<(DirEntry, DirEntry)> {
        let mut out = Vec::new();
        let Ok(root) = self.root(usb) else { return out };
        let Ok(mut top) = self.list(usb, &root) else { return out };
        top.retain(|e| e.is_dir && !e.name.eq_ignore_ascii_case(BIN) && !e.name.eq_ignore_ascii_case("Originals"));
        top.sort_by_key(|e| !e.name.eq_ignore_ascii_case("Photos"));
        for d in top {
            self.collect_photos(usb, &d, 0, &mut out);
        }
        out
    }

    fn collect_photos(&mut self, usb: &mut Usb, dir: &DirEntry, depth: u32, out: &mut Vec<(DirEntry, DirEntry)>) {
        let Ok(entries) = self.list(usb, dir) else { return };
        let mut subdirs = Vec::new();
        for e in entries {
            if e.is_dir {
                subdirs.push(e);
            } else if crate::import::is_photo(&e.name) {
                out.push((dir.clone(), e));
            }
        }
        if depth < 8 {
            subdirs.reverse(); // import folder names start with the date: newest first
            for d in subdirs {
                self.collect_photos(usb, &d, depth + 1, out);
            }
        }
    }

    // ---- edited photos ----------------------------------------------------------------

    fn names_in(&mut self, usb: &mut Usb, dir: &DirEntry) -> Result<Vec<(String, u64)>, FsError> {
        Ok(self.list(usb, dir)?.into_iter().map(|e| (e.name, e.size)).collect())
    }

    /// /Originals/<folder> (created when `create`).
    fn originals_for(&mut self, usb: &mut Usb, folder: &str, create: bool) -> Result<Option<DirEntry>, FsError> {
        self.with(usb, |vol, d| {
            let root = vol.root();
            let originals = match vol.find(d, &root, "Originals")? {
                Some(e) => e,
                None if create => vol.mkdir(d, &root, "Originals")?,
                None => return Ok(None),
            };
            match vol.find(d, &originals, folder)? {
                Some(e) => Ok(Some(e)),
                None if create => vol.mkdir(d, &originals, folder).map(Some),
                None => Ok(None),
            }
        })
    }

    /// The hidden original of a replaced photo, if there is one.
    pub fn backup_of(&mut self, usb: &mut Usb, folder: &DirEntry, name: &str) -> Option<(DirEntry, DirEntry)> {
        let dir = self.originals_for(usb, &folder.name, false).ok()??;
        let s = stem(name);
        let found = self.list(usb, &dir).ok()?.into_iter().find(|e| stem(&e.name).eq_ignore_ascii_case(s))?;
        Some((dir, found))
    }

    /// Save an edited copy next to the original (or in "<date> Edited" for photos from
    /// outside the library). Returns (folder, new file).
    pub fn save_copy(&mut self, usb: &mut Usb, folder: Option<&DirEntry>, original: &str, jpeg: &[u8]) -> Result<(DirEntry, DirEntry), FsError> {
        let dir = match folder {
            Some(f) => f.clone(),
            None => self.import_folder(usb, "Edited")?,
        };
        let existing = self.names_in(usb, &dir)?;
        let want = format!("{} (edited).jpg", stem(original));
        let name = unique_name(&existing, &want, u64::MAX).ok_or(FsError::Exists)?;
        let e = self.create_in(usb, &dir, &name, jpeg)?;
        Ok((dir, e))
    }

    /// Replace a library photo with its edited version, keeping the first original hidden
    /// in /Originals so it can be restored.
    pub fn replace(&mut self, usb: &mut Usb, folder: &DirEntry, entry: &DirEntry, jpeg: &[u8]) -> Result<DirEntry, FsError> {
        if self.backup_of(usb, folder, &entry.name).is_none() {
            let original = self.read_file(usb, entry)?;
            let backups = self.originals_for(usb, &folder.name, true)?.ok_or(FsError::Io)?;
            self.create_in(usb, &backups, &entry.name, &original)?;
        }
        self.delete(usb, entry)?;
        let existing = self.names_in(usb, folder)?;
        let name = unique_name(&existing, &format!("{}.jpg", stem(&entry.name)), u64::MAX).ok_or(FsError::Exists)?;
        self.create_in(usb, folder, &name, jpeg)
    }

    /// Put the hidden original back in place of the edited photo.
    pub fn revert(&mut self, usb: &mut Usb, folder: &DirEntry, entry: &DirEntry) -> Result<DirEntry, FsError> {
        let (_, backup) = self.backup_of(usb, folder, &entry.name).ok_or(FsError::Io)?;
        let data = self.read_file(usb, &backup)?;
        self.delete(usb, entry)?;
        let existing = self.names_in(usb, folder)?;
        let name = unique_name(&existing, &backup.name, u64::MAX).ok_or(FsError::Exists)?;
        let restored = self.create_in(usb, folder, &name, &data)?;
        self.delete(usb, &backup)?;
        Ok(restored)
    }
}

/// Pick the name to save under, given what's already in the folder (name, size).
/// None = the same file (name and size) is already there.
pub fn unique_name(existing: &[(String, u64)], name: &str, size: u64) -> Option<String> {
    let (stem, ext) = match name.rfind('.') {
        Some(i) if i > 0 => (&name[..i], &name[i..]),
        _ => (name, ""),
    };
    let mut candidate = String::from(name);
    for n in 2..10_000 {
        match existing.iter().find(|(e, _)| e.eq_ignore_ascii_case(&candidate)) {
            None => return Some(candidate),
            Some((_, s)) if *s == size && n == 2 => return None,
            Some(_) => candidate = format!("{} ({}){}", stem, n, ext),
        }
    }
    None
}

/// "IMG_0001.JPG" -> "IMG_0001"
pub fn stem(name: &str) -> &str {
    match name.rfind('.') {
        Some(i) if i > 0 => &name[..i],
        _ => name,
    }
}

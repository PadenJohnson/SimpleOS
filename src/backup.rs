//! Backup: copies everything in Solon storage to a USB drive, and back again.
//!
//! On the drive everything goes in a "Solon Backup" folder, and Solon only ever adds to it
//! (the rule for every USB drive that isn't Solon's own stick): nothing there is changed or
//! deleted. "Files" holds the files themselves, in the same folders as in Solon, so they open
//! on any computer. A file that changed since the last backup is added beside the old copy
//! with the date in its name ("Letter (2026-11-21).docx"), so older versions stay too.
//! "Index" gets a small text file for every backup, listing each file with its size, change
//! time, checksum and where its copy is. Every file copied is read back and checked.
//!
//! Restoring copies the files of one backup into Solon storage. That only adds as well: a
//! file that is already there is left alone, and if it has changed since, the backup's copy
//! is added beside it as "Name (restored)".

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::fs::{exfat::Pending, DirEntry, FsError, FsKind};
use crate::gfx::{rgb, Canvas, Rect, Size};
use crate::library::Storage;
use crate::settings::Settings;
use crate::usb::Usb;

/// The folder on the USB drive.
pub const FOLDER: &str = "Solon Backup";
/// In Solon storage: when the last backup was made.
pub const RECORD: &str = "Backup.txt";
/// Big files are read and written a piece at a time (whole clusters, so writes line up).
const PIECE: usize = crate::phone::PIECE;
/// FAT drives take files in one go, so the whole file has to fit in memory.
const FAT_MAX: u64 = 128 * 1024 * 1024;
/// Remind after this many days without a backup.
pub const REMIND_DAYS: i64 = 30;
/// Root files that restoring merges line by line instead of keeping both.
const MERGED: [&str; 2] = ["Calendar.txt", "PhoneImports.txt"];
/// Root files that aren't backed up (they'd be wrong after a restore) or restored over.
const NOT_BACKED_UP: [&str; 2] = [RECORD, "RecycleBin.txt"];
const KEEP_CURRENT: [&str; 2] = ["Settings.txt", "Games.txt"];

const INK: u32 = rgb(28, 32, 40);
const DIM: u32 = rgb(106, 112, 124);
const ACCENT: u32 = rgb(0, 122, 204);
const GOOD: u32 = rgb(30, 140, 70);
const WARN: u32 = rgb(214, 120, 20);
const BAD: u32 = rgb(200, 60, 50);

const FNV0: u64 = 0xcbf2_9ce4_8422_2325;

/// FNV-1a: a quick checksum to check copies with.
fn fnv(mut h: u64, data: &[u8]) -> u64 {
    for &b in data {
        h = (h ^ b as u64).wrapping_mul(0x0000_0100_0000_01B3);
    }
    h
}

/// One file in a backup.
#[derive(Clone, Debug)]
pub struct Rec {
    /// Where it is in Solon storage ("Photos/Trip/IMG_1.jpg").
    pub path: String,
    pub size: u64,
    pub mtime: u32,
    pub hash: u64,
    /// Where its copy is, inside "Solon Backup/Files".
    pub stored: String,
}

/// A backup's index file.
fn index_text(date: &str, complete: bool, recs: &[Rec]) -> String {
    let mut s = String::from("Solon backup\nversion\t1\n");
    s += &format!("date\t{}\nstatus\t{}\nfiles\t{}\n", date, if complete { "complete" } else { "stopped" }, recs.len());
    for r in recs {
        s += &format!("F\t{}\t{:08x}\t{:016x}\t{}\t{}\n", r.size, r.mtime, r.hash, r.path, r.stored);
    }
    s
}

fn parse_index(text: &str) -> Vec<Rec> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() == 6 && f[0] == "F" {
            let (Ok(size), Ok(mtime), Ok(hash)) = (f[1].parse(), u32::from_str_radix(f[2], 16), u64::from_str_radix(f[3], 16)) else { continue };
            out.push(Rec { path: f[4].to_string(), size, mtime, hash, stored: f[5].to_string() });
        }
    }
    out
}

/// "a/b/c.txt" -> ("a/b", "c.txt")
fn split(path: &str) -> (&str, &str) {
    match path.rfind('/') {
        Some(i) => (&path[..i], &path[i + 1..]),
        None => ("", path),
    }
}

fn join(dir: &str, name: &str) -> String {
    if dir.is_empty() {
        String::from(name)
    } else {
        format!("{}/{}", dir, name)
    }
}

/// 12345 -> "12,345"
pub fn count(n: usize) -> String {
    let s = n.to_string();
    let mut out = String::new();
    for (i, ch) in s.chars().enumerate() {
        if i > 0 && (s.len() - i) % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

fn plural(n: usize, one: &str, many: &str) -> String {
    format!("{} {}", count(n), if n == 1 { one } else { many })
}

// ---- the last backup, kept in Solon storage --------------------------------------------

#[derive(Clone, Debug)]
pub struct LastBackup {
    pub date: (u16, u8, u8),
    pub time: (u8, u8),
    pub drive: String,
}

impl LastBackup {
    pub fn parse(text: &str) -> Option<LastBackup> {
        for line in text.lines() {
            let f: Vec<&str> = line.split('\t').collect();
            if f.len() >= 4 && f[0] == "last" {
                let d: Vec<u32> = f[1].split('-').filter_map(|x| x.parse().ok()).collect();
                let t: Vec<u32> = f[2].split(':').filter_map(|x| x.parse().ok()).collect();
                if d.len() == 3 && t.len() == 2 {
                    return Some(LastBackup { date: (d[0] as u16, d[1] as u8, d[2] as u8), time: (t[0] as u8, t[1] as u8), drive: f[3].to_string() });
                }
            }
        }
        None
    }
    fn text(&self) -> String {
        format!("last\t{:04}-{:02}-{:02}\t{:02}:{:02}\t{}\n", self.date.0, self.date.1, self.date.2, self.time.0, self.time.1, self.drive)
    }
    /// Days from the backup to `today`.
    pub fn age(&self, today: (u16, u8, u8)) -> i64 {
        crate::calendar::day_number(today.0, today.1, today.2) - crate::calendar::day_number(self.date.0, self.date.1, self.date.2)
    }
}

/// "Saturday, November 21 2026 at 3:30 PM"
fn when(s: &Settings, date: (u16, u8, u8), time: (u8, u8)) -> String {
    format!("{} {} at {}", crate::settings::long_date_text(date.0, date.1, date.2), date.0, s.clock_text(time.0, time.1))
}

/// "2026-11-21 153012.txt" -> its date and time.
fn index_when(name: &str) -> Option<((u16, u8, u8), (u8, u8))> {
    let b = name.as_bytes();
    if b.len() < 17 || b[4] != b'-' || b[7] != b'-' || b[10] != b' ' {
        return None;
    }
    let num = |a: usize, n: usize| name.get(a..a + n)?.parse::<u32>().ok();
    Some(((num(0, 4)? as u16, num(5, 2)? as u8, num(8, 2)? as u8), (num(11, 2)? as u8, num(13, 2)? as u8)))
}

/// Short description of how old the last backup is, for the desktop. None = no reminder.
pub fn reminder(last: Option<&LastBackup>, today: (u16, u8, u8)) -> Option<String> {
    match last {
        None => Some(String::from("Your photos and files aren't backed up yet - open Backup")),
        Some(l) if l.age(today) > REMIND_DAYS => Some(format!("Your last backup was {} days ago - open Backup", l.age(today))),
        Some(_) => None,
    }
}

// ---- folders on either side ------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Side {
    Usb { drive: u32, vol: usize },
    Storage,
}

fn side_list(side: Side, storage: &mut Storage, usb: &mut Usb, dir: &DirEntry) -> Result<Vec<DirEntry>, FsError> {
    match side {
        Side::Usb { drive, vol } => usb.list(drive, vol, dir),
        Side::Storage => storage.list(usb, dir),
    }
}

fn side_mkdir(side: Side, storage: &mut Storage, usb: &mut Usb, parent: &DirEntry, name: &str) -> Result<DirEntry, FsError> {
    match side {
        Side::Usb { drive, vol } => usb.usb_create(drive, vol, parent, name, None),
        Side::Storage => storage.mkdir_in(usb, parent, name),
    }
}

/// Folders under one top folder, listed once and remembered: path -> (folder, contents).
struct Tree {
    side: Side,
    root: DirEntry,
    dirs: BTreeMap<String, (DirEntry, Vec<DirEntry>)>,
}

impl Tree {
    fn new(side: Side, root: DirEntry) -> Tree {
        Tree { side, root, dirs: BTreeMap::new() }
    }

    /// Make sure folder `path` is listed, creating it (and its parents) if `create`.
    /// Ok(false) if it isn't there.
    fn open(&mut self, storage: &mut Storage, usb: &mut Usb, path: &str, create: bool) -> Result<bool, FsError> {
        if self.dirs.contains_key(path) {
            return Ok(true);
        }
        if path.is_empty() {
            let list = side_list(self.side, storage, usb, &self.root)?;
            self.dirs.insert(String::new(), (self.root.clone(), list));
            return Ok(true);
        }
        let (parent, name) = split(path);
        if !self.open(storage, usb, parent, create)? {
            return Ok(false);
        }
        let (pdir, kids) = &self.dirs[parent];
        let entry = match kids.iter().find(|e| e.is_dir && e.name.eq_ignore_ascii_case(name)) {
            Some(e) => e.clone(),
            None if create => {
                let pdir = pdir.clone();
                let e = side_mkdir(self.side, storage, usb, &pdir, name)?;
                self.dirs.get_mut(parent).unwrap().1.push(e.clone());
                e
            }
            None => return Ok(false),
        };
        let list = side_list(self.side, storage, usb, &entry)?;
        self.dirs.insert(String::from(path), (entry, list));
        Ok(true)
    }

    fn dir(&self, path: &str) -> &DirEntry {
        &self.dirs[path].0
    }
    fn kids(&mut self, path: &str) -> &mut Vec<DirEntry> {
        &mut self.dirs.get_mut(path).unwrap().1
    }
    fn file(&self, path: &str) -> Option<DirEntry> {
        let (dir, name) = split(path);
        self.dirs.get(dir)?.1.iter().find(|e| !e.is_dir && e.name.eq_ignore_ascii_case(name)).cloned()
    }
    fn taken(&self, dir: &str, name: &str) -> bool {
        self.dirs.get(dir).map_or(false, |d| d.1.iter().any(|e| e.name.eq_ignore_ascii_case(name)))
    }
}

// ---- the job ---------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Mode {
    Backup,
    Restore,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Phase {
    Scan,
    Copy,
    Finish,
    Done,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Stage {
    /// Backup: checksum the file first, to find copies already on the drive.
    Hash,
    Write,
    /// Backup: read the copy back from the drive.
    Verify,
}

/// A big file being copied a piece at a time over several steps.
struct Big {
    src: DirEntry,
    rec: Rec,
    stage: Stage,
    off: u64,
    hash: u64,
    /// Where the copy goes: folder path and name.
    dir: String,
    name: String,
    pending: Option<Pending>,
    /// FAT drives: the whole file, written at the end.
    buf: Vec<u8>,
    written: Option<DirEntry>,
    /// How much of it the progress bar has counted.
    counted: u64,
}

pub struct Job {
    pub mode: Mode,
    drive: u32,
    vol: usize,
    pub drive_name: String,
    fat: bool,
    phase: Phase,
    /// Backup: Solon storage (source) and "Solon Backup/Files" (copies).
    /// Restore: "Solon Backup/Files" (source) and Solon storage.
    src: Tree,
    dst: Tree,
    index_dir: DirEntry,
    index_names: Vec<String>,
    /// The previous backup, by path; and stored copies by (size, checksum).
    prev: BTreeMap<String, Rec>,
    by_content: BTreeMap<(u64, u64), String>,
    /// Backup: folders still to look through.
    scan: Vec<String>,
    /// Files still to copy: (source entry, record so far).
    todo: VecDeque<(Option<DirEntry>, Rec)>,
    /// Backup: every file in this backup.
    recs: Vec<Rec>,
    big: Option<Big>,
    stamp: ((u16, u8, u8), (u8, u8, u8)),
    // Progress, for the screen.
    pub total: usize,
    pub done: usize,
    pub bytes_total: u64,
    pub bytes_done: u64,
    pub current: String,
    // Results.
    pub new_files: usize,
    pub changed: usize,
    pub same: usize,
    pub failed: usize,
    pub too_big: usize,
    pub kept_both: usize,
    pub calendar_changed: bool,
    pub stopped: bool,
    pub error: Option<String>,
}

impl Job {
    /// Get ready to back up to a USB drive volume.
    pub fn backup(storage: &mut Storage, usb: &mut Usb, drive: u32, vol: usize, drive_name: &str) -> Result<Job, String> {
        let side = Side::Usb { drive, vol };
        let root = usb.source_root(drive, vol).ok_or("The USB drive isn't there any more")?;
        let lib = storage.root(usb).map_err(|_| String::from("Solon storage isn't ready"))?;
        let fail = |e: FsError| -> String {
            match e {
                FsError::ReadOnly => String::from("Solon can't write to this drive"),
                FsError::Full => String::from("The USB drive is full"),
                _ => format!("Couldn't get the USB drive ready ({})", e.message()),
            }
        };
        let mut top = Tree::new(side, root);
        top.open(storage, usb, FOLDER, true).map_err(fail)?;
        let base = top.dir(FOLDER).clone();
        let mut bt = Tree::new(side, base);
        bt.open(storage, usb, "Files", true).map_err(fail)?;
        bt.open(storage, usb, "Index", true).map_err(fail)?;
        if !bt.taken("", "Read me.txt") {
            let _ = usb.usb_create(drive, vol, &bt.root.clone(), "Read me.txt", Some(README.as_bytes()));
        }
        let files = bt.dir("Files").clone();
        let index_dir = bt.dir("Index").clone();
        let index_names: Vec<String> = bt.dirs["Index"].1.iter().filter(|e| !e.is_dir).map(|e| e.name.clone()).collect();
        // The newest backup tells what's already on the drive.
        let mut prev = BTreeMap::new();
        let mut by_content = BTreeMap::new();
        if let Some(newest) = newest_index(&bt.dirs["Index"].1) {
            match usb.read_file(drive, vol, &newest) {
                Ok(text) => {
                    for r in parse_index(&String::from_utf8_lossy(&text)) {
                        by_content.insert((r.size, r.hash), r.stored.clone());
                        prev.insert(r.path.clone(), r);
                    }
                }
                Err(e) => kprintln!("backup: couldn't read the last index: {:?}", e),
            }
        }
        kprintln!("backup: to drive {} vol {}, {} files in the last backup", drive, vol, prev.len());
        let fat = usb.volume_kind(drive, vol) != Some(FsKind::ExFat);
        Ok(Job {
            mode: Mode::Backup,
            drive,
            vol,
            drive_name: String::from(drive_name),
            fat,
            phase: Phase::Scan,
            src: Tree::new(Side::Storage, lib),
            dst: Tree::new(side, files),
            index_dir,
            index_names,
            prev,
            by_content,
            scan: Vec::from([String::new()]),
            todo: VecDeque::new(),
            recs: Vec::new(),
            big: None,
            stamp: (crate::rtc::date(), crate::rtc::time()),
            total: 0,
            done: 0,
            bytes_total: 0,
            bytes_done: 0,
            current: String::new(),
            new_files: 0,
            changed: 0,
            same: 0,
            failed: 0,
            too_big: 0,
            kept_both: 0,
            calendar_changed: false,
            stopped: false,
            error: None,
        })
    }

    /// Get ready to copy backup `index` (a file name in "Solon Backup/Index") into Solon.
    pub fn restore(storage: &mut Storage, usb: &mut Usb, drive: u32, vol: usize, drive_name: &str, index: &str) -> Result<Job, String> {
        let side = Side::Usb { drive, vol };
        let root = usb.source_root(drive, vol).ok_or("The USB drive isn't there any more")?;
        let lib = storage.root(usb).map_err(|_| String::from("Solon storage isn't ready"))?;
        let mut top = Tree::new(side, root);
        let missing = || String::from("This drive has no backups");
        if !top.open(storage, usb, FOLDER, false).map_err(|_| missing())? {
            return Err(missing());
        }
        let mut bt = Tree::new(side, top.dir(FOLDER).clone());
        let ok = bt.open(storage, usb, "Files", false).map_err(|_| missing())? && bt.open(storage, usb, "Index", false).map_err(|_| missing())?;
        if !ok {
            return Err(missing());
        }
        let entry = bt.file(&join("Index", index)).ok_or_else(missing)?;
        let text = usb.read_file(drive, vol, &entry).map_err(|e| format!("Couldn't read the backup's list of files ({})", e.message()))?;
        let recs = parse_index(&String::from_utf8_lossy(&text));
        let mut todo = VecDeque::new();
        let mut bytes = 0;
        for r in recs {
            if !r.path.contains('/') && NOT_BACKED_UP.iter().any(|n| n.eq_ignore_ascii_case(&r.path)) {
                continue;
            }
            bytes += r.size;
            todo.push_back((None, r));
        }
        kprintln!("backup: restoring \"{}\" from drive {}: {} files", index, drive, todo.len());
        let fat = usb.volume_kind(drive, vol) != Some(FsKind::ExFat);
        Ok(Job {
            mode: Mode::Restore,
            drive,
            vol,
            drive_name: String::from(drive_name),
            fat,
            phase: Phase::Copy,
            src: Tree::new(side, bt.dir("Files").clone()),
            dst: Tree::new(Side::Storage, lib),
            index_dir: entry,
            index_names: Vec::new(),
            prev: BTreeMap::new(),
            by_content: BTreeMap::new(),
            scan: Vec::new(),
            total: todo.len(),
            todo,
            recs: Vec::new(),
            big: None,
            stamp: (crate::rtc::date(), crate::rtc::time()),
            done: 0,
            bytes_total: bytes,
            bytes_done: 0,
            current: String::new(),
            new_files: 0,
            changed: 0,
            same: 0,
            failed: 0,
            too_big: 0,
            kept_both: 0,
            calendar_changed: false,
            stopped: false,
            error: None,
        })
    }

    pub fn finished(&self) -> bool {
        self.phase == Phase::Done
    }

    pub fn scanning(&self) -> bool {
        self.phase == Phase::Scan
    }

    /// Stop after giving back any half-written file. A stopped backup still gets an index
    /// for what it copied.
    pub fn stop(&mut self, storage: &mut Storage, usb: &mut Usb) {
        if self.phase == Phase::Done {
            return;
        }
        self.stopped = true;
        if self.phase == Phase::Scan {
            self.phase = Phase::Done;
            return;
        }
        self.drop_big(storage, usb);
        self.abandon();
        self.phase = if self.mode == Mode::Backup { Phase::Finish } else { Phase::Done };
    }

    fn drop_big(&mut self, storage: &mut Storage, usb: &mut Usb) {
        if let Some(b) = self.big.take() {
            if let Some(p) = b.pending {
                let _ = match self.mode {
                    Mode::Backup => usb.usb_abort_file(self.drive, self.vol, p),
                    Mode::Restore => storage.abort_file(usb, p),
                };
            }
            self.keep_previous(&b.rec.path);
        }
    }

    /// Files not got to keep their copies from the last backup.
    fn abandon(&mut self) {
        let rest: Vec<String> = self.todo.drain(..).map(|(_, r)| r.path).collect();
        for p in rest {
            self.keep_previous(&p);
        }
    }

    /// A file that couldn't be copied keeps its copy from the last backup, if there is one.
    fn keep_previous(&mut self, path: &str) {
        if self.mode == Mode::Backup {
            if let Some(r) = self.prev.get(path) {
                self.recs.push(r.clone());
            }
        }
    }

    /// Do one piece of work (one folder, one file, or one piece of a big file).
    pub fn step(&mut self, storage: &mut Storage, usb: &mut Usb) {
        if !usb.has_drive(self.drive) && self.phase != Phase::Done {
            self.error = Some(String::from("The USB drive was unplugged before it finished"));
            self.big = None;
            self.phase = Phase::Done;
            return;
        }
        match self.phase {
            Phase::Scan => self.scan_step(storage, usb),
            Phase::Copy => {
                let r = if self.big.is_some() {
                    self.big_step(storage, usb)
                } else if let Some((src, rec)) = self.todo.pop_front() {
                    self.current = rec.path.clone();
                    match self.mode {
                        Mode::Backup => self.backup_file(storage, usb, src.unwrap(), rec),
                        Mode::Restore => self.restore_file(storage, usb, rec),
                    }
                } else {
                    self.phase = if self.mode == Mode::Backup { Phase::Finish } else { Phase::Done };
                    Ok(())
                };
                if let Err(e) = r {
                    self.fatal(storage, usb, e);
                }
            }
            Phase::Finish => {
                self.finish(storage, usb);
                self.phase = Phase::Done;
            }
            Phase::Done => {}
        }
    }

    /// Errors that end the job.
    fn fatal(&mut self, storage: &mut Storage, usb: &mut Usb, e: FsError) {
        let msg = match (e, self.mode) {
            (FsError::Full, Mode::Backup) => "The USB drive is full",
            (FsError::Full, Mode::Restore) => "Solon storage is full",
            (FsError::ReadOnly, Mode::Backup) => "Solon can't write to this USB drive",
            _ => "Something went wrong reading or writing",
        };
        kprintln!("backup: stopped: {} ({:?})", msg, e);
        self.drop_big(storage, usb);
        self.error = Some(String::from(msg));
        self.abandon();
        self.phase = if self.mode == Mode::Backup { Phase::Finish } else { Phase::Done };
    }

    // ---- backup ----

    fn scan_step(&mut self, storage: &mut Storage, usb: &mut Usb) {
        let Some(path) = self.scan.pop() else {
            // Everything found: is there room?
            self.total = self.todo.len();
            self.bytes_total = self.todo.iter().map(|(_, r)| r.size).sum();
            let free = usb.volume_free(self.drive, self.vol).unwrap_or(0);
            kprintln!("backup: {} unchanged, {} to copy ({} bytes), drive has {} free", self.same, self.total, self.bytes_total, free);
            if self.bytes_total + 4 * 1024 * 1024 > free && self.total > 0 {
                self.error = Some(format!(
                    "There isn't room on this drive: the backup needs {} and the drive has {} free. Use a bigger drive, or one with more free space.",
                    crate::disk::human(self.bytes_total),
                    crate::disk::human(free)
                ));
                self.phase = Phase::Done;
                return;
            }
            self.phase = Phase::Copy;
            return;
        };
        self.current = path.clone();
        let list = match self.src.open(storage, usb, &path, false) {
            Ok(true) => self.src.dirs[&path].1.clone(),
            _ => {
                kprintln!("backup: couldn't list \"{}\"", path);
                return;
            }
        };
        let top = path.is_empty();
        for e in list {
            let p = join(&path, &e.name);
            if e.is_dir {
                if !(top && e.name.eq_ignore_ascii_case(crate::library::BIN)) {
                    self.scan.push(p);
                }
                continue;
            }
            if (top && NOT_BACKED_UP.iter().any(|n| n.eq_ignore_ascii_case(&e.name))) || e.name.ends_with(".saving") {
                continue;
            }
            match self.prev.get(&p) {
                Some(r) if r.size == e.size && r.mtime == e.mtime && e.mtime != 0 => {
                    self.recs.push(r.clone());
                    self.same += 1;
                }
                _ => {
                    let rec = Rec { path: p, size: e.size, mtime: e.mtime, hash: 0, stored: String::new() };
                    self.todo.push_back((Some(e), rec));
                }
            }
        }
    }

    /// A free name for a copy in `dir` of the drive: the file's own name, or with the date.
    fn copy_name(&mut self, storage: &mut Storage, usb: &mut Usb, dir: &str, name: &str) -> Result<String, FsError> {
        self.dst.open(storage, usb, dir, true)?;
        if !self.dst.taken(dir, name) {
            return Ok(String::from(name));
        }
        let (stem, ext) = match name.rfind('.') {
            Some(i) if i > 0 => (&name[..i], &name[i..]),
            _ => (name, ""),
        };
        let (y, m, d) = self.stamp.0;
        let base = format!("{} ({:04}-{:02}-{:02}", stem, y, m, d);
        let mut cand = format!("{}){}", base, ext);
        let mut k = 2;
        while self.dst.taken(dir, &cand) {
            cand = format!("{} {}){}", base, k, ext);
            k += 1;
        }
        Ok(cand)
    }

    /// Note a copied file.
    fn stored(&mut self, rec: Rec) {
        if self.prev.get(&rec.path).map_or(false, |p| p.stored == rec.stored && p.hash == rec.hash) {
            self.same += 1; // only its time changed
        } else if self.prev.contains_key(&rec.path) {
            self.changed += 1;
        } else {
            self.new_files += 1;
        }
        self.by_content.insert((rec.size, rec.hash), rec.stored.clone());
        self.recs.push(rec);
    }

    fn backup_file(&mut self, storage: &mut Storage, usb: &mut Usb, src: DirEntry, mut rec: Rec) -> Result<(), FsError> {
        let (dir, name) = split(&rec.path);
        let (dir, name) = (String::from(dir), String::from(name));
        if src.size > PIECE as u64 {
            if self.fat && src.size > FAT_MAX {
                kprintln!("backup: \"{}\" is too big for a FAT drive", rec.path);
                self.too_big += 1;
                self.keep_previous(&rec.path);
                self.done += 1;
                self.bytes_done += src.size;
                return Ok(());
            }
            self.big = Some(Big { src, rec, stage: Stage::Hash, off: 0, hash: FNV0, dir, name, pending: None, buf: Vec::new(), written: None, counted: 0 });
            return Ok(());
        }
        self.done += 1;
        self.bytes_done += src.size;
        let data = match storage.read_file(usb, &src) {
            Ok(d) => d,
            Err(e) => {
                kprintln!("backup: couldn't read \"{}\": {:?}", rec.path, e);
                self.failed += 1;
                self.keep_previous(&rec.path);
                return Ok(());
            }
        };
        rec.size = data.len() as u64;
        rec.hash = fnv(FNV0, &data);
        if let Some(s) = self.by_content.get(&(rec.size, rec.hash)) {
            // The same file is on the drive already (moved, renamed, or put back).
            rec.stored = s.clone();
            self.stored(rec);
            return Ok(());
        }
        let cname = self.copy_name(storage, usb, &dir, &name)?;
        let folder = self.dst.dir(&dir).clone();
        let made = usb.usb_create(self.drive, self.vol, &folder, &cname, Some(&data))?;
        self.dst.kids(&dir).push(made.clone());
        // Read it back.
        let ok = usb.read_file(self.drive, self.vol, &made).map(|b| b.len() as u64 == rec.size && fnv(FNV0, &b) == rec.hash).unwrap_or(false);
        if !ok {
            kprintln!("backup: the copy of \"{}\" didn't read back right", rec.path);
            self.failed += 1;
            self.keep_previous(&rec.path);
            return Ok(());
        }
        rec.stored = join(&dir, &cname);
        self.stored(rec);
        Ok(())
    }

    fn big_step(&mut self, storage: &mut Storage, usb: &mut Usb) -> Result<(), FsError> {
        let mut b = self.big.take().unwrap();
        let len = b.src.size;
        let r: Result<bool, FsError> = (|| match (self.mode, b.stage) {
            (Mode::Backup, Stage::Hash) => {
                let part = storage.read_part(usb, &b.src, b.off, PIECE)?;
                if part.is_empty() {
                    return Err(FsError::Io);
                }
                b.hash = fnv(b.hash, &part);
                b.off += part.len() as u64;
                b.counted += part.len() as u64 / 2;
                self.bytes_done += part.len() as u64 / 2;
                if b.off >= len {
                    b.rec.hash = b.hash;
                    if let Some(s) = self.by_content.get(&(len, b.hash)) {
                        b.rec.stored = s.clone();
                        return Ok(true);
                    }
                    b.name = self.copy_name(storage, usb, &b.dir.clone(), &b.name.clone())?;
                    if !self.fat {
                        let folder = self.dst.dir(&b.dir).clone();
                        b.pending = Some(usb.usb_begin_file(self.drive, self.vol, &folder, &b.name, len)?);
                    }
                    b.stage = Stage::Write;
                    b.off = 0;
                    b.hash = FNV0;
                }
                Ok(false)
            }
            (Mode::Backup, Stage::Write) => {
                let part = storage.read_part(usb, &b.src, b.off, PIECE)?;
                if part.is_empty() {
                    return Err(FsError::Io);
                }
                b.hash = fnv(b.hash, &part);
                match &b.pending {
                    Some(p) => usb.usb_write_part(self.drive, self.vol, p, b.off, &part)?,
                    None => b.buf.extend_from_slice(&part),
                }
                b.off += part.len() as u64;
                let add = part.len() as u64 - part.len() as u64 / 2;
                b.counted += add;
                self.bytes_done += add;
                if b.off >= len {
                    if b.hash != b.rec.hash {
                        // It changed while being copied.
                        return Err(FsError::Corrupt);
                    }
                    let folder = self.dst.dir(&b.dir).clone();
                    let made = match b.pending.take() {
                        Some(p) => usb.usb_finish_file(self.drive, self.vol, p)?,
                        None => usb.usb_create(self.drive, self.vol, &folder, &b.name, Some(&core::mem::take(&mut b.buf)))?,
                    };
                    self.dst.kids(&b.dir).push(made.clone());
                    b.written = Some(made);
                    b.stage = Stage::Verify;
                    b.off = 0;
                    b.hash = FNV0;
                }
                Ok(false)
            }
            (Mode::Backup, Stage::Verify) => {
                let made = b.written.clone().unwrap();
                let part = if self.fat { usb.read_file(self.drive, self.vol, &made)? } else { usb.drive_read_part(self.drive, self.vol, &made, b.off, PIECE)? };
                if part.is_empty() {
                    return Err(FsError::Corrupt);
                }
                b.hash = fnv(b.hash, &part);
                b.off += part.len() as u64;
                if b.off >= len {
                    if b.hash != b.rec.hash {
                        return Err(FsError::Corrupt);
                    }
                    b.rec.stored = join(&b.dir, &b.name);
                    return Ok(true);
                }
                Ok(false)
            }
            (Mode::Restore, _) => {
                let part = usb.drive_read_part(self.drive, self.vol, &b.src, b.off, PIECE)?;
                if part.is_empty() {
                    return Err(FsError::Corrupt);
                }
                b.hash = fnv(b.hash, &part);
                storage.write_part(usb, b.pending.as_ref().unwrap(), b.off, &part)?;
                b.off += part.len() as u64;
                b.counted += part.len() as u64;
                self.bytes_done += part.len() as u64;
                if b.off >= len {
                    let p = b.pending.take().unwrap();
                    if b.hash != b.rec.hash {
                        let _ = storage.abort_file(usb, p);
                        return Err(FsError::Corrupt);
                    }
                    let made = storage.finish_file(usb, p)?;
                    self.dst.kids(&b.dir).push(made);
                    return Ok(true);
                }
                Ok(false)
            }
        })();
        match r {
            Ok(false) => self.big = Some(b),
            Ok(true) => {
                self.done += 1;
                self.bytes_done += len.saturating_sub(b.counted);
                match self.mode {
                    Mode::Backup => self.stored(b.rec),
                    Mode::Restore => self.new_files += 1,
                }
            }
            Err(e @ (FsError::Full | FsError::ReadOnly)) => {
                self.big = Some(b);
                return Err(e);
            }
            Err(e) => {
                kprintln!("backup: \"{}\" failed: {:?}", b.rec.path, e);
                if let Some(p) = b.pending.take() {
                    let _ = match self.mode {
                        Mode::Backup => usb.usb_abort_file(self.drive, self.vol, p),
                        Mode::Restore => storage.abort_file(usb, p),
                    };
                }
                self.done += 1;
                self.bytes_done += len.saturating_sub(b.counted);
                self.failed += 1;
                self.keep_previous(&b.rec.path);
            }
        }
        Ok(())
    }

    /// Write the index and remember the backup.
    fn finish(&mut self, storage: &mut Storage, usb: &mut Usb) {
        let ((y, m, d), (h, mi, s)) = self.stamp;
        let date = format!("{:04}-{:02}-{:02} {:02}:{:02}:{:02}", y, m, d, h, mi, s);
        let complete = !self.stopped && self.error.is_none();
        self.recs.sort_by(|a, b| a.path.cmp(&b.path));
        let text = index_text(&date, complete, &self.recs);
        let mut name = format!("{:04}-{:02}-{:02} {:02}{:02}{:02}.txt", y, m, d, h, mi, s);
        let mut k = b'b';
        while self.index_names.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
            name = format!("{:04}-{:02}-{:02} {:02}{:02}{:02}{}.txt", y, m, d, h, mi, s, k as char);
            k += 1;
        }
        let dir = self.index_dir.clone();
        if let Err(e) = usb.usb_create(self.drive, self.vol, &dir, &name, Some(text.as_bytes())) {
            kprintln!("backup: couldn't write the index: {:?}", e);
            if self.error.is_none() {
                self.error = Some(String::from("Couldn't finish the backup: the drive may be full"));
            }
            return;
        }
        kprintln!("backup: index \"{}\": {} files, {} new, {} changed, {} same, {} failed, {} too big", name, self.recs.len(), self.new_files, self.changed, self.same, self.failed, self.too_big);
        if complete {
            let last = LastBackup { date: (y, m, d), time: (h, mi), drive: self.drive_name.clone() };
            if let Err(e) = storage.write_root_file(usb, RECORD, last.text().as_bytes()) {
                kprintln!("backup: couldn't remember the backup: {:?}", e);
            }
        }
    }

    // ---- restore ----

    fn restore_file(&mut self, storage: &mut Storage, usb: &mut Usb, rec: Rec) -> Result<(), FsError> {
        let (sdir, _) = split(&rec.stored);
        let (dir, name) = split(&rec.path);
        let (dir, name) = (String::from(dir), String::from(name));
        let src = match self.src.open(storage, usb, sdir, false) {
            Ok(true) => self.src.file(&rec.stored),
            _ => None,
        };
        let Some(src) = src else {
            kprintln!("backup: \"{}\" is missing from the drive", rec.stored);
            self.failed += 1;
            self.done += 1;
            return Ok(());
        };
        self.dst.open(storage, usb, &dir, true)?;
        let existing = self.dst.file(&rec.path);
        let top = dir.is_empty();
        let merged = top && MERGED.iter().any(|n| n.eq_ignore_ascii_case(&name));
        let mut target = name.clone();
        if let Some(cur) = &existing {
            if top && KEEP_CURRENT.iter().any(|n| n.eq_ignore_ascii_case(&name)) {
                return Ok(self.skip(rec.size));
            }
            if cur.size == rec.size && !merged {
                let same = rec.size > PIECE as u64 || storage.read_file(usb, cur).map(|b| fnv(FNV0, &b) == rec.hash).unwrap_or(false);
                if same {
                    return Ok(self.skip(rec.size));
                }
            }
            if !merged {
                let pairs: Vec<(String, u64)> = self.dst.kids(&dir).iter().map(|e| (e.name.clone(), e.size)).collect();
                let (stem, ext) = match name.rfind('.') {
                    Some(i) if i > 0 => (&name[..i], &name[i..]),
                    _ => (&name[..], ""),
                };
                match crate::library::unique_name(&pairs, &format!("{} (restored){}", stem, ext), rec.size) {
                    Some(n) => target = n,
                    None => return Ok(self.skip(rec.size)), // restored before
                }
                self.kept_both += 1;
            }
        }
        if rec.size + 1024 * 1024 > storage.free_bytes() {
            return Err(FsError::Full);
        }
        // Big files from exFAT drives come over a piece at a time.
        if rec.size > PIECE as u64 && !self.fat {
            let folder = self.dst.dir(&dir).clone();
            let p = storage.begin_file(usb, &folder, &target, rec.size)?;
            self.big = Some(Big { src, rec, stage: Stage::Write, off: 0, hash: FNV0, dir, name: target, pending: Some(p), buf: Vec::new(), written: None, counted: 0 });
            return Ok(());
        }
        self.done += 1;
        self.bytes_done += rec.size;
        let data = match usb.read_file(self.drive, self.vol, &src) {
            Ok(d) if d.len() as u64 == rec.size && fnv(FNV0, &d) == rec.hash => d,
            r => {
                kprintln!("backup: the copy of \"{}\" is damaged ({:?})", rec.path, r.err());
                self.failed += 1;
                return Ok(());
            }
        };
        if merged {
            if let Some(cur) = &existing {
                let old = storage.read_file(usb, cur).unwrap_or_default();
                let old = String::from_utf8_lossy(&old).into_owned();
                let mut text = old.clone();
                if !text.is_empty() && !text.ends_with('\n') {
                    text.push('\n');
                }
                let have: Vec<&str> = old.lines().collect();
                let mut added = 0;
                for l in String::from_utf8_lossy(&data).lines() {
                    if !l.is_empty() && !have.contains(&l) {
                        text += l;
                        text.push('\n');
                        added += 1;
                    }
                }
                if added == 0 {
                    self.same += 1;
                    return Ok(());
                }
                storage.write_root_file(usb, &name, text.as_bytes())?;
                self.dst.dirs.remove("");
                self.new_files += 1;
                self.calendar_changed |= name.eq_ignore_ascii_case("Calendar.txt");
                return Ok(());
            }
            self.calendar_changed |= name.eq_ignore_ascii_case("Calendar.txt");
        }
        let folder = self.dst.dir(&dir).clone();
        let made = storage.create_in(usb, &folder, &target, &data)?;
        self.dst.kids(&dir).push(made);
        self.new_files += 1;
        Ok(())
    }

    fn skip(&mut self, size: u64) {
        self.same += 1;
        self.done += 1;
        self.bytes_done += size;
    }

    /// What happened, for the screen: (headline, details, all went well).
    pub fn summary(&self) -> (String, Vec<String>, bool) {
        let mut lines = Vec::new();
        let ok = self.error.is_none() && self.failed == 0 && self.too_big == 0 && !self.stopped;
        let head = match (self.mode, &self.error, self.stopped) {
            (_, Some(e), _) => e.clone(),
            (Mode::Backup, None, true) => String::from("The backup was stopped"),
            (Mode::Restore, None, true) => String::from("Restoring was stopped"),
            (Mode::Backup, None, false) => format!("Backup finished: {} safe on {}", plural(self.recs.len(), "file is", "files are"), self.drive_name),
            (Mode::Restore, None, false) => format!("Restore finished: {} copied into Solon", plural(self.new_files, "file", "files")),
        };
        match self.mode {
            Mode::Backup => {
                if self.new_files + self.changed + self.same > 0 {
                    lines.push(format!("{} new, {} changed, {} already backed up.", count(self.new_files), count(self.changed), count(self.same)));
                }
                if self.new_files + self.changed > 0 {
                    lines.push(String::from("Every file copied was read back and checked."));
                }
                if self.changed > 0 {
                    lines.push(String::from("The earlier copies of changed files are kept too."));
                }
                if self.stopped || (self.error.is_some() && self.phase == Phase::Done && !self.recs.is_empty()) {
                    lines.push(String::from("What was copied is kept. Back up again to finish."));
                }
            }
            Mode::Restore => {
                if self.same > 0 {
                    lines.push(format!("{} already in Solon.", plural(self.same, "file was", "files were")));
                }
                if self.kept_both > 0 {
                    lines.push(format!("{} had changed since, so the backup's copy was added as \"(restored)\".", plural(self.kept_both, "file", "files")));
                }
                lines.push(String::from("Nothing in Solon was deleted or replaced."));
            }
        }
        if self.failed > 0 {
            lines.push(format!("{} couldn't be copied (details in the F12 log).", plural(self.failed, "file", "files")));
        }
        if self.too_big > 0 {
            lines.push(format!(
                "{} too big for this drive's format (FAT). Erasing the drive in Disks fixes that, but deletes what's on it.",
                plural(self.too_big, "file is", "files are")
            ));
        }
        (head, lines, ok)
    }
}

/// The newest index file (names are dates, so the biggest name).
fn newest_index(entries: &[DirEntry]) -> Option<DirEntry> {
    entries.iter().filter(|e| !e.is_dir && index_when(&e.name).is_some()).max_by(|a, b| a.name.cmp(&b.name)).cloned()
}

const README: &str = "This folder holds backups made by Solon.\r\n\
\r\n\
Files: your photos, documents and other files, in the same folders as in Solon. They open on\r\n\
any computer. When a file changed between backups, the older copy is kept too, with the date\r\n\
it was replaced in its name, like \"Letter (2026-11-21).docx\".\r\n\
\r\n\
Index: one list for each backup, saying which files it had.\r\n\
\r\n\
To put the files back on a Solon computer, open Backup, choose this drive and press Restore.\r\n\
Solon only ever adds to this folder: it never changes or deletes what's already here.\r\n";

// ---- the app ---------------------------------------------------------------------------

/// A USB drive volume that can hold backups.
pub struct DriveInfo {
    pub drive: u32,
    pub vol: usize,
    pub name: String,
    pub size: u64,
    pub free: Option<u64>,
    pub kind: &'static str,
    /// Index file names, newest first.
    pub backups: Vec<String>,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum View {
    Drives,
    /// Choosing a backup to restore from drive `0`.
    Restore(usize, usize),
}

pub enum BackupAction {
    None,
    Redraw,
    /// A job ended: Solon storage changed (refresh photos etc.), the calendar changed.
    Finished { storage_changed: bool, calendar_changed: bool, message: String },
}

pub struct BackupApp {
    pub drives: Vec<DriveInfo>,
    view: View,
    pub job: Option<Job>,
    /// The last job's result: (headline, details, ok).
    result: Option<(String, Vec<String>, bool)>,
    pub last: Option<LastBackup>,
    pub last_loaded: bool,
    /// Solon storage has photos, documents or videos.
    pub has_data: bool,
    /// When the progress was last redrawn.
    drawn: u64,
}

impl BackupApp {
    pub fn new() -> BackupApp {
        BackupApp { drives: Vec::new(), view: View::Drives, job: None, result: None, last: None, last_loaded: false, has_data: false, drawn: 0 }
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn load_last(&mut self, storage: &mut Storage, usb: &mut Usb) {
        let t = storage.read_root_file(usb, RECORD);
        self.last = t.and_then(|b| LastBackup::parse(&String::from_utf8_lossy(&b)));
        self.last_loaded = true;
        // Is there anything worth backing up? (No reminders on an empty Solon.)
        self.has_data = self.last.is_some()
            || storage.root(usb).and_then(|root| storage.list(usb, &root)).map_or(false, |top| {
                top.iter().filter(|e| e.is_dir && ["Photos", "Documents", "Videos"].iter().any(|n| e.name.eq_ignore_ascii_case(n))).any(|d| storage.list(usb, d).map_or(false, |l| !l.is_empty()))
            });
    }

    /// Look at the USB drives again.
    pub fn refresh(&mut self, storage: &mut Storage, usb: &mut Usb) {
        let _ = storage;
        let mut out = Vec::new();
        let vols: Vec<(u32, usize, String, u64, &'static str)> = usb
            .drives
            .iter()
            .filter(|d| !d.is_boot)
            .flat_map(|d| {
                let many = d.volumes.len() > 1;
                d.volumes.iter().enumerate().map(move |(i, v)| {
                    let mut name = if v.label.trim().is_empty() { d.name.clone() } else { String::from(v.label.trim()) };
                    if many {
                        name = format!("{} (part {})", name, i + 1);
                    }
                    (d.id, i, name, v.size, v.kind.name())
                })
            })
            .collect();
        for (drive, vol, name, size, kind) in vols {
            let free = usb.volume_free(drive, vol);
            let mut backups = Vec::new();
            if let Some(root) = usb.source_root(drive, vol) {
                let find = |usb: &mut Usb, dir: &DirEntry, n: &str| usb.list(drive, vol, dir).ok()?.into_iter().find(|e| e.is_dir && e.name.eq_ignore_ascii_case(n));
                if let Some(f) = find(usb, &root, FOLDER) {
                    if let Some(ix) = find(usb, &f, "Index") {
                        backups = usb.list(drive, vol, &ix).unwrap_or_default().into_iter().filter(|e| !e.is_dir && index_when(&e.name).is_some()).map(|e| e.name).collect();
                        backups.sort();
                        backups.reverse();
                    }
                }
            }
            out.push(DriveInfo { drive, vol, name, size, free, kind, backups });
        }
        self.drives = out;
        if let View::Restore(i, _) = self.view {
            if i >= self.drives.len() || self.drives[i].backups.is_empty() {
                self.view = View::Drives;
            }
        }
    }

    /// Run the job a step; called every tick while it runs.
    pub fn step(&mut self, storage: &mut Storage, usb: &mut Usb, now: u64) -> BackupAction {
        let Some(job) = &mut self.job else { return BackupAction::None };
        let before = (job.done, job.phase);
        job.step(storage, usb);
        if !job.finished() {
            // Redraw a few times a second, or when something visible changed.
            if before.1 != job.phase || now >= self.drawn + crate::interrupts::HZ / 4 {
                self.drawn = now;
                return BackupAction::Redraw;
            }
            return BackupAction::None;
        }
        let job = self.job.take().unwrap();
        let (head, lines, ok) = job.summary();
        kprintln!("backup: {}", head);
        let storage_changed = job.mode == Mode::Restore && job.new_files > 0;
        let calendar_changed = job.calendar_changed;
        self.result = Some((head.clone(), lines, ok));
        self.view = View::Drives;
        if job.mode == Mode::Backup {
            self.load_last(storage, usb);
        }
        self.refresh(storage, usb);
        BackupAction::Finished { storage_changed, calendar_changed, message: head }
    }

    // ---- layout ----

    fn card(r: Rect, i: usize) -> Rect {
        Rect::new(r.x + 24, r.y + 158 + i as i32 * 96, r.w - 48, 84)
    }
    fn backup_btn(card: Rect) -> Rect {
        Rect::new(card.x + card.w - 176, card.y + 20, 160, 44)
    }
    fn restore_btn(card: Rect) -> Rect {
        Rect::new(card.x + card.w - 176 - 140, card.y + 20, 128, 44)
    }
    fn row(r: Rect, i: usize) -> Rect {
        Rect::new(r.x + 24, r.y + 196 + i as i32 * 50, r.w - 48, 44)
    }
    fn rows_fit(r: Rect) -> usize {
        ((r.h - 196 - 150) / 50).max(1) as usize
    }
    fn bottom_btns(r: Rect) -> (Rect, Rect) {
        let y = r.y + r.h - 70;
        (Rect::new(r.x + 24, y, 230, 46), Rect::new(r.x + 270, y, 120, 46))
    }
    fn stop_btn(r: Rect) -> Rect {
        Rect::new(r.x + 44, r.y + 330, 120, 44)
    }
    fn result_btn(r: Rect, lines: usize) -> Rect {
        Rect::new(r.x + r.w - 24 - 120 - 20, r.y + 122 + 20 + 34 + lines as i32 * 24 + 8, 120, 40)
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect, s: &Settings, today: (u16, u8, u8)) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(250, 250, 252));
        c.text(r.x + 24, r.y + 20, "Backup", Size::Large, true, INK);
        // How things stand.
        let (dot, status) = match &self.last {
            None => (BAD, String::from("Not backed up yet")),
            Some(l) => {
                let age = l.age(today);
                let ago = match age {
                    i64::MIN..=0 => String::from("today"),
                    1 => String::from("yesterday"),
                    n => format!("{} days ago", n),
                };
                (if age > REMIND_DAYS { WARN } else { GOOD }, format!("Last backup {}: {}, to {}", ago, when(s, l.date, l.time), l.drive))
            }
        };
        c.circle(r.x + 31, r.y + 79, 6, dot);
        c.text(r.x + 46, r.y + 69, &crate::photos::ellipsize(&status, r.w - 70, Size::Small, false), Size::Small, false, INK);
        c.fill_rect(r.x + 24, r.y + 106, r.w - 48, 1, rgb(224, 228, 234));

        if let Some(job) = &self.job {
            self.draw_job(c, r, job);
            return;
        }
        let mut y = r.y + 122;
        if let Some((head, lines, ok)) = &self.result {
            let h = 20 + 34 + lines.len() as i32 * 24 + 56;
            c.round_rect(r.x + 24, y, r.w - 48, h, 12, if *ok { rgb(232, 246, 236) } else { rgb(253, 240, 228) });
            let col = if *ok { GOOD } else { WARN };
            let head_lines = crate::docshell::wrap_lines(head, r.w - 100, 2);
            c.text(r.x + 44, y + 18, &head_lines[0], Size::Small, true, col);
            for (k, l) in lines.iter().enumerate() {
                c.text(r.x + 44, y + 52 + k as i32 * 24, &crate::photos::ellipsize(l, r.w - 100, Size::Small, false), Size::Small, false, INK);
            }
            crate::docshell::button(c, Self::result_btn(r, lines.len()), "OK", false, ACCENT);
            return;
        }
        match self.view {
            View::Restore(i, sel) => self.draw_restore(c, r, s, i, sel),
            View::Drives => {
                if self.drives.is_empty() {
                    c.text(r.x + 24, y + 6, "Plug in a USB drive to back up to", Size::Medium, true, INK);
                    y += 56;
                    for (k, l) in [
                        "1.  Plug a USB stick or USB disk into this computer.",
                        "2.  It shows up here after a few seconds.",
                        "3.  Press \"Back up now\". Only new and changed files are copied, so later backups are quick.",
                    ]
                    .iter()
                    .enumerate()
                    {
                        c.text(r.x + 24, y + k as i32 * 32, l, Size::Small, false, INK);
                    }
                    y += 120;
                    for l in crate::docshell::wrap_lines("Tip: keep a drive just for backups, and store it somewhere safe away from the computer. A drive as big as Solon storage holds everything.", r.w - 48, 3) {
                        c.text(r.x + 24, y, &l, Size::Small, false, DIM);
                        y += 24;
                    }
                } else {
                    c.text(r.x + 24, y + 4, "Choose a USB drive", Size::Medium, true, INK);
                    for (i, d) in self.drives.iter().enumerate() {
                        let b = Self::card(r, i);
                        if b.y + b.h > r.y + r.h - 70 {
                            break;
                        }
                        c.round_rect(b.x, b.y, b.w, b.h, 12, rgb(255, 255, 255));
                        drive_icon(c, b.x + 16, b.y + 18);
                        let text_w = Self::restore_btn(b).x - b.x - 84 - 12;
                        c.text(b.x + 76, b.y + 14, &crate::photos::ellipsize(&d.name, text_w, Size::Small, true), Size::Small, true, INK);
                        let free = match d.free {
                            Some(f) => format!("{} free of {}", crate::disk::human(f), crate::disk::human(d.size)),
                            None => crate::disk::human(d.size),
                        };
                        c.text(b.x + 76, b.y + 36, &crate::photos::ellipsize(&format!("{}  |  {}", free, d.kind), text_w, Size::Small, false), Size::Small, false, DIM);
                        let has = match d.backups.first().and_then(|n| index_when(n)) {
                            Some((dt, _)) => format!("{} on it, the latest from {}", plural(d.backups.len(), "backup", "backups"), crate::settings::short_date_text(dt.0, dt.1, dt.2)),
                            None => String::from("No backups on it yet"),
                        };
                        c.text(b.x + 76, b.y + 57, &crate::photos::ellipsize(&has, text_w, Size::Small, false), Size::Small, false, if d.backups.is_empty() { DIM } else { GOOD });
                        crate::docshell::button(c, Self::backup_btn(b), "Back up now", true, ACCENT);
                        if !d.backups.is_empty() {
                            crate::docshell::button(c, Self::restore_btn(b), "Restore...", false, ACCENT);
                        }
                    }
                }
                let foot = "Backing up only adds to the drive: nothing already on it is changed or deleted. Each backup copies what's new or changed, and keeps the older copies.";
                let lines = crate::docshell::wrap_lines(foot, r.w - 48, 2);
                for (k, l) in lines.iter().enumerate() {
                    c.text(r.x + 24, r.y + r.h - 24 - (lines.len() - k) as i32 * 22, l, Size::Small, false, DIM);
                }
            }
        }
    }

    fn draw_restore(&self, c: &mut Canvas, r: Rect, s: &Settings, i: usize, sel: usize) {
        let d = &self.drives[i];
        let y = r.y + 122;
        c.text(r.x + 24, y + 4, &crate::photos::ellipsize(&format!("Restore from {}", d.name), r.w - 48, Size::Medium, true), Size::Medium, true, INK);
        c.text(r.x + 24, y + 42, "Choose which backup to copy back into Solon:", Size::Small, false, DIM);
        let first = sel.saturating_sub(Self::rows_fit(r) - 1);
        for (k, name) in d.backups.iter().enumerate().skip(first).take(Self::rows_fit(r)) {
            let b = Self::row(r, k - first);
            c.round_rect(b.x, b.y, b.w, b.h, 10, if k == sel { rgb(222, 236, 250) } else { rgb(255, 255, 255) });
            let label = match index_when(name) {
                Some((dt, tm)) => when(s, dt, tm),
                None => name.clone(),
            };
            c.text(b.x + 16, b.y + 11, &label, Size::Small, k == sel, INK);
            if k == 0 {
                c.text(b.x + b.w - 90, b.y + 11, "Newest", Size::Small, false, GOOD);
            }
        }
        let note = "Nothing in Solon is deleted or replaced. Files already in Solon are skipped; if one has changed since, the backup's copy is added beside it as \"(restored)\".";
        let lines = crate::docshell::wrap_lines(note, r.w - 48, 3);
        let (go, cancel) = Self::bottom_btns(r);
        for (k, l) in lines.iter().enumerate() {
            c.text(r.x + 24, go.y - 14 - (lines.len() - k) as i32 * 22, l, Size::Small, false, DIM);
        }
        crate::docshell::button(c, go, "Restore this backup", true, ACCENT);
        crate::docshell::button(c, cancel, "Cancel", false, ACCENT);
    }

    fn draw_job(&self, c: &mut Canvas, r: Rect, job: &Job) {
        let x = r.x + 44;
        let y = r.y + 140;
        let title = match job.mode {
            Mode::Backup => format!("Backing up to {}", job.drive_name),
            Mode::Restore => format!("Restoring from {}", job.drive_name),
        };
        c.text(x, y, &crate::photos::ellipsize(&title, r.w - 88, Size::Medium, true), Size::Medium, true, INK);
        let line = if job.scanning() {
            format!("Looking for new and changed files...  {} already backed up", count(job.same))
        } else if job.total == 0 {
            String::from("Finishing...")
        } else {
            format!(
                "{} of {} files  |  {} of {}",
                count(job.done.min(job.total)),
                count(job.total),
                crate::disk::human(job.bytes_done.min(job.bytes_total)),
                crate::disk::human(job.bytes_total)
            )
        };
        c.text(x, y + 46, &line, Size::Small, false, INK);
        let bw = r.w - 88;
        c.round_rect(x, y + 82, bw, 14, 7, rgb(226, 230, 236));
        let frac = if job.scanning() {
            0
        } else if job.bytes_total > 0 {
            ((job.bytes_done.min(job.bytes_total) as u128 * bw as u128) / job.bytes_total as u128) as i32
        } else if job.total > 0 {
            job.done.min(job.total) as i32 * bw / job.total as i32
        } else {
            bw
        };
        if frac > 0 {
            c.round_rect(x, y + 82, frac.max(14), 14, 7, ACCENT);
        }
        c.text(x, y + 112, &crate::photos::ellipsize(&job.current, bw, Size::Small, false), Size::Small, false, DIM);
        crate::docshell::button(c, Self::stop_btn(r), "Stop", false, ACCENT);
        c.text(x + 140, Self::stop_btn(r).y + 12, "You can keep using Solon while this runs.", Size::Small, false, DIM);
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, storage: &mut Storage, usb: &mut Usb) -> BackupAction {
        if let Some(job) = &mut self.job {
            if Self::stop_btn(r).contains(x, y) {
                job.stop(storage, usb);
                return BackupAction::Redraw;
            }
            return BackupAction::None;
        }
        if let Some((_, lines, _)) = &self.result {
            if Self::result_btn(r, lines.len()).contains(x, y) {
                self.result = None;
                return BackupAction::Redraw;
            }
            return BackupAction::None;
        }
        match self.view {
            View::Restore(i, sel) => {
                let (go, cancel) = Self::bottom_btns(r);
                if cancel.contains(x, y) {
                    self.view = View::Drives;
                    return BackupAction::Redraw;
                }
                if go.contains(x, y) {
                    return self.start_restore(storage, usb, i, sel);
                }
                let first = sel.saturating_sub(Self::rows_fit(r) - 1);
                for k in 0..Self::rows_fit(r) {
                    if Self::row(r, k).contains(x, y) && first + k < self.drives[i].backups.len() {
                        self.view = View::Restore(i, first + k);
                        return BackupAction::Redraw;
                    }
                }
            }
            View::Drives => {
                for i in 0..self.drives.len() {
                    let b = Self::card(r, i);
                    if Self::backup_btn(b).contains(x, y) {
                        return self.start_backup(storage, usb, i);
                    }
                    if !self.drives[i].backups.is_empty() && Self::restore_btn(b).contains(x, y) {
                        self.view = View::Restore(i, 0);
                        return BackupAction::Redraw;
                    }
                }
            }
        }
        BackupAction::None
    }

    fn start_backup(&mut self, storage: &mut Storage, usb: &mut Usb, i: usize) -> BackupAction {
        let d = &self.drives[i];
        match Job::backup(storage, usb, d.drive, d.vol, &d.name) {
            Ok(j) => self.job = Some(j),
            Err(e) => self.result = Some((e, Vec::new(), false)),
        }
        BackupAction::Redraw
    }

    fn start_restore(&mut self, storage: &mut Storage, usb: &mut Usb, i: usize, sel: usize) -> BackupAction {
        let d = &self.drives[i];
        let Some(name) = d.backups.get(sel).cloned() else { return BackupAction::None };
        match Job::restore(storage, usb, d.drive, d.vol, &d.name, &name) {
            Ok(j) => self.job = Some(j),
            Err(e) => self.result = Some((e, Vec::new(), false)),
        }
        BackupAction::Redraw
    }

    /// Keys: arrows choose a backup, Enter restores it, Esc goes back. None = not used.
    pub fn on_key(&mut self, sc: u8, storage: &mut Storage, usb: &mut Usb) -> Option<BackupAction> {
        if self.job.is_some() || sc & 0x80 != 0 {
            return None;
        }
        if self.result.is_some() && matches!(sc, 0x1C | 0x01) {
            self.result = None;
            return Some(BackupAction::Redraw);
        }
        let View::Restore(i, sel) = self.view else { return None };
        let n = self.drives[i].backups.len();
        match sc {
            0x48 => self.view = View::Restore(i, sel.saturating_sub(1)),
            0x50 => self.view = View::Restore(i, (sel + 1).min(n.saturating_sub(1))),
            0x1C => return Some(self.start_restore(storage, usb, i, sel)),
            0x01 => self.view = View::Drives,
            _ => return None,
        }
        Some(BackupAction::Redraw)
    }
}

/// A small USB drive picture.
fn drive_icon(c: &mut Canvas, x: i32, y: i32) {
    c.round_rect(x, y + 8, 44, 32, 7, rgb(226, 234, 244));
    c.round_rect(x + 4, y + 12, 36, 24, 5, ACCENT);
    c.fill_rect(x + 44, y + 16, 8, 16, rgb(180, 188, 200));
    c.fill_rect(x + 46, y + 20, 3, 3, rgb(250, 250, 252));
    c.fill_rect(x + 46, y + 26, 3, 3, rgb(250, 250, 252));
    c.circle(x + 12, y + 24, 3, rgb(255, 255, 255));
}

//! mkimg: builds test disk images for Solon's USB drive support.
//!
//! Usage: mkimg <samples folder> <output folder>
//!
//! Writes (all read-only test media, built from the sample photos):
//!   fat32-mbr.img    64 MiB, MBR partition, FAT32 "VACATION"  (long names, deep folders,
//!                    fragmented files, a 300-file folder, lowercase 8.3 names)
//!   exfat-gpt.img    64 MiB, GPT partition, exFAT "SDCARD"     (contiguous + FAT-chained files,
//!                    Unicode names, 300-file folder, empty folder)
//!   fat16-super.img  32 MiB, no partition table, FAT16 "CARD16"
//!   fat12-floppy.img 1.44 MB, no partition table, FAT12 "OLDCAM"
//!   manifest.txt     the log line Solon prints for each file it reads (size + FNV-1a hash)

use std::collections::HashSet;
use std::fs;
use std::path::Path;

const SECTOR: usize = 512;

// ---- file tree -------------------------------------------------------------------

#[derive(Clone)]
enum Node {
    File { name: String, data: Vec<u8>, frag: bool },
    Dir { name: String, children: Vec<Node> },
}

impl Node {
    fn name(&self) -> &str {
        match self {
            Node::File { name, .. } | Node::Dir { name, .. } => name,
        }
    }
}

fn file(name: &str, data: &[u8]) -> Node {
    Node::File { name: name.into(), data: data.to_vec(), frag: false }
}
fn frag_file(name: &str, data: &[u8]) -> Node {
    Node::File { name: name.into(), data: data.to_vec(), frag: true }
}
fn dir(name: &str, children: Vec<Node>) -> Node {
    Node::Dir { name: name.into(), children }
}

fn fnv(data: &[u8]) -> u32 {
    let mut h: u32 = 0x811C_9DC5;
    for &b in data {
        h = (h ^ b as u32).wrapping_mul(0x0100_0193);
    }
    h
}

fn manifest(node: &Node, out: &mut Vec<String>) {
    match node {
        Node::File { name, data, .. } => {
            out.push(format!("usb: read \"{}\" {} bytes fnv {:08x}", name, data.len(), fnv(data)))
        }
        Node::Dir { children, .. } => children.iter().for_each(|c| manifest(c, out)),
    }
}

// ---- FAT -------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq)]
enum FatKind {
    F12,
    F16,
    F32,
}

struct Fat {
    kind: FatKind,
    spc: usize,
    rsvd: usize,
    fat_sectors: usize,
    root_entries: usize,
    data_start: usize,
    clusters: u32,
    img: Vec<u8>,
    fat: Vec<u32>,
}

fn is_83_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || "!#$%&'()-@^_`{}~".contains(c)
}

/// Returns (11-byte short name, needs long name, NT case flags).
fn short_name(long: &str, used: &mut HashSet<[u8; 11]>) -> ([u8; 11], bool, u8) {
    let (base, ext) = match long.rfind('.') {
        Some(i) if i > 0 => (&long[..i], &long[i + 1..]),
        _ => (long, ""),
    };
    let fits = !base.is_empty()
        && base.len() <= 8
        && ext.len() <= 3
        && base.chars().chain(ext.chars()).all(is_83_char);
    if fits {
        let up = |s: &str| s.chars().all(|c| !c.is_ascii_lowercase());
        let low = |s: &str| s.chars().all(|c| !c.is_ascii_uppercase());
        let mut sn = [b' '; 11];
        for (i, c) in base.to_ascii_uppercase().bytes().enumerate() {
            sn[i] = c;
        }
        for (i, c) in ext.to_ascii_uppercase().bytes().enumerate() {
            sn[8 + i] = c;
        }
        if !used.contains(&sn) {
            if up(base) && up(ext) {
                used.insert(sn);
                return (sn, false, 0);
            }
            if low(base) && low(ext) {
                used.insert(sn);
                let flags = (if base.chars().any(|c| c.is_ascii_alphabetic()) { 0x08 } else { 0 })
                    | (if ext.chars().any(|c| c.is_ascii_alphabetic()) { 0x10 } else { 0 });
                return (sn, false, flags);
            }
        }
    }
    let clean = |s: &str, n: usize| -> Vec<u8> {
        s.chars().filter(|&c| is_83_char(c.to_ascii_uppercase()) && c.is_ascii()).map(|c| c.to_ascii_uppercase() as u8).take(n).collect()
    };
    let b = clean(base, 6);
    let e = clean(ext, 3);
    for n in 1..1000 {
        let tail = format!("~{}", n);
        let mut sn = [b' '; 11];
        let keep = (8 - tail.len()).min(b.len());
        sn[..keep].copy_from_slice(&b[..keep]);
        sn[keep..keep + tail.len()].copy_from_slice(tail.as_bytes());
        sn[8..8 + e.len()].copy_from_slice(&e);
        if !used.contains(&sn) {
            used.insert(sn);
            return (sn, true, 0);
        }
    }
    panic!("too many similar names");
}

fn lfn_checksum(sn: &[u8; 11]) -> u8 {
    let mut s: u8 = 0;
    for &c in sn {
        s = (s >> 1 | s << 7).wrapping_add(c);
    }
    s
}

/// Directory entries (LFN pieces + short entry) for one child.
fn fat_entries(name: &str, sn: [u8; 11], lfn: bool, flags: u8, attr: u8, cluster: u32, size: u32) -> Vec<[u8; 32]> {
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
        let sum = lfn_checksum(&sn);
        for seq in (1..=n).rev() {
            let chunk = &units[(seq - 1) * 13..seq * 13];
            let mut e = [0u8; 32];
            e[0] = seq as u8 | if seq == n { 0x40 } else { 0 };
            e[11] = 0x0F;
            e[13] = sum;
            for (k, off) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30].iter().enumerate() {
                e[*off..*off + 2].copy_from_slice(&chunk[k].to_le_bytes());
            }
            out.push(e);
        }
    }
    let mut e = [0u8; 32];
    e[..11].copy_from_slice(&sn);
    e[11] = attr;
    e[12] = flags;
    e[20..22].copy_from_slice(&((cluster >> 16) as u16).to_le_bytes());
    e[26..28].copy_from_slice(&(cluster as u16).to_le_bytes());
    e[28..32].copy_from_slice(&size.to_le_bytes());
    // A fixed timestamp: 2026-11-19 12:00
    let date: u16 = ((2026 - 1980) << 9 | 11 << 5 | 19) as u16;
    let time: u16 = (12 << 11) as u16;
    for off in [14usize, 22] {
        e[off..off + 2].copy_from_slice(&time.to_le_bytes());
        e[off + 2..off + 4].copy_from_slice(&date.to_le_bytes());
    }
    e[18..20].copy_from_slice(&date.to_le_bytes());
    out.push(e);
    out
}

impl Fat {
    fn new(kind: FatKind, total: usize, spc: usize, root_entries: usize, hidden: u32, label: &str) -> Fat {
        let rsvd = if kind == FatKind::F32 { 32 } else { 1 };
        let root_sectors = (root_entries * 32 + SECTOR - 1) / SECTOR;
        // Find the FAT size by iterating to a fixed point.
        let mut fat_sectors = 1;
        loop {
            let data_start = rsvd + 2 * fat_sectors + root_sectors;
            let clusters = (total - data_start) / spc;
            let bytes = match kind {
                FatKind::F12 => (clusters + 2) * 3 / 2 + 1,
                FatKind::F16 => (clusters + 2) * 2,
                FatKind::F32 => (clusters + 2) * 4,
            };
            let need = (bytes + SECTOR - 1) / SECTOR;
            if need <= fat_sectors {
                break;
            }
            fat_sectors = need;
        }
        let data_start = rsvd + 2 * fat_sectors + root_sectors;
        let clusters = ((total - data_start) / spc) as u32;
        let ok = match kind {
            FatKind::F12 => clusters < 4085,
            FatKind::F16 => (4085..65525).contains(&clusters),
            FatKind::F32 => clusters >= 65525,
        };
        assert!(ok, "cluster count {} doesn't match the FAT type", clusters);
        let mut f = Fat {
            kind,
            spc,
            rsvd,
            fat_sectors,
            root_entries,
            data_start,
            clusters,
            img: vec![0u8; total * SECTOR],
            fat: vec![0u32; clusters as usize + 2],
        };
        f.fat[0] = 0x0FFF_FFF8;
        f.fat[1] = 0x0FFF_FFFF;
        f.boot_sector(total, hidden, label);
        f
    }

    fn boot_sector(&mut self, total: usize, hidden: u32, label: &str) {
        let b = &mut self.img[..SECTOR];
        b[0..3].copy_from_slice(&[0xEB, 0x58, 0x90]);
        b[3..11].copy_from_slice(b"SOLONMK ");
        b[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
        b[13] = self.spc as u8;
        b[14..16].copy_from_slice(&(self.rsvd as u16).to_le_bytes());
        b[16] = 2;
        b[17..19].copy_from_slice(&(self.root_entries as u16).to_le_bytes());
        if total < 65536 && self.kind != FatKind::F32 {
            b[19..21].copy_from_slice(&(total as u16).to_le_bytes());
        } else {
            b[32..36].copy_from_slice(&(total as u32).to_le_bytes());
        }
        b[21] = if self.kind == FatKind::F12 && total == 2880 { 0xF0 } else { 0xF8 };
        b[24..26].copy_from_slice(&63u16.to_le_bytes());
        b[26..28].copy_from_slice(&255u16.to_le_bytes());
        b[28..32].copy_from_slice(&hidden.to_le_bytes());
        let mut lab = [b' '; 11];
        for (i, c) in label.bytes().take(11).enumerate() {
            lab[i] = c;
        }
        if self.kind == FatKind::F32 {
            b[36..40].copy_from_slice(&(self.fat_sectors as u32).to_le_bytes());
            b[44..48].copy_from_slice(&2u32.to_le_bytes()); // root cluster
            b[48..50].copy_from_slice(&1u16.to_le_bytes()); // FSInfo sector
            b[50..52].copy_from_slice(&6u16.to_le_bytes()); // backup boot sector
            b[64] = 0x80;
            b[66] = 0x29;
            b[67..71].copy_from_slice(&0x5010_1126u32.to_le_bytes());
            b[71..82].copy_from_slice(&lab);
            b[82..90].copy_from_slice(b"FAT32   ");
        } else {
            b[22..24].copy_from_slice(&(self.fat_sectors as u16).to_le_bytes());
            b[36] = 0x80;
            b[38] = 0x29;
            b[39..43].copy_from_slice(&0x5010_1126u32.to_le_bytes());
            b[43..54].copy_from_slice(&lab);
            b[54..62].copy_from_slice(if self.kind == FatKind::F12 { b"FAT12   " } else { b"FAT16   " });
        }
        b[510] = 0x55;
        b[511] = 0xAA;
    }

    fn cluster_bytes(&self) -> usize {
        self.spc * SECTOR
    }

    /// First-fit allocation. `frag` leaves a one-cluster gap after every cluster.
    fn alloc(&mut self, n: usize, frag: bool) -> Vec<u32> {
        let mut out = Vec::new();
        let mut c = 2u32;
        while out.len() < n {
            assert!(c < self.clusters + 2, "volume full");
            if self.fat[c as usize] == 0 && !out.contains(&c) {
                out.push(c);
                if frag {
                    c += 1; // skip one
                }
            }
            c += 1;
        }
        for w in out.windows(2) {
            self.fat[w[0] as usize] = w[1];
        }
        let eoc = 0x0FFF_FFFF;
        self.fat[*out.last().unwrap() as usize] = eoc;
        out
    }

    fn write_chain(&mut self, chain: &[u32], data: &[u8]) {
        let cb = self.cluster_bytes();
        for (i, &c) in chain.iter().enumerate() {
            let start = (self.data_start + (c as usize - 2) * self.spc) * SECTOR;
            let part = &data[(i * cb).min(data.len())..((i + 1) * cb).min(data.len())];
            self.img[start..start + part.len()].copy_from_slice(part);
        }
    }

    fn entry_count(name: &str, used: &mut HashSet<[u8; 11]>) -> usize {
        let (_, lfn, _) = short_name(name, used);
        1 + if lfn { (name.encode_utf16().count() + 1 + 12) / 13 } else { 0 }
    }

    /// Write a directory's children; returns the encoded entries for this directory.
    fn dir_entries(&mut self, children: &[Node], self_cluster: u32, parent_cluster: u32, is_root: bool, label: &str) -> Vec<[u8; 32]> {
        let mut used = HashSet::new();
        let mut ents = Vec::new();
        if is_root {
            let mut lab = [b' '; 11];
            for (i, c) in label.bytes().take(11).enumerate() {
                lab[i] = c;
            }
            ents.extend(fat_entries("", lab, false, 0, 0x08, 0, 0));
        } else {
            let mut dot = [b' '; 11];
            dot[0] = b'.';
            ents.extend(fat_entries(".", dot, false, 0, 0x10, self_cluster, 0));
            dot[1] = b'.';
            ents.extend(fat_entries("..", dot, false, 0, 0x10, parent_cluster, 0));
        }
        for child in children {
            let (sn, lfn, flags) = short_name(child.name(), &mut used);
            match child {
                Node::File { name, data, frag } => {
                    let cluster = if data.is_empty() {
                        0
                    } else {
                        let n = (data.len() + self.cluster_bytes() - 1) / self.cluster_bytes();
                        let chain = self.alloc(n, *frag);
                        self.write_chain(&chain, data);
                        chain[0]
                    };
                    ents.extend(fat_entries(name, sn, lfn, flags, 0x20, cluster, data.len() as u32));
                }
                Node::Dir { name, children } => {
                    let cl = self.make_dir(children, if is_root { 0 } else { self_cluster });
                    ents.extend(fat_entries(name, sn, lfn, flags, 0x10, cl, 0));
                }
            }
        }
        ents
    }

    fn make_dir(&mut self, children: &[Node], parent: u32) -> u32 {
        let mut used = HashSet::new();
        let count = 2 + children.iter().map(|c| Self::entry_count(c.name(), &mut used)).sum::<usize>();
        let n = (count * 32 + self.cluster_bytes() - 1) / self.cluster_bytes();
        let chain = self.alloc(n, false);
        let ents = self.dir_entries(children, chain[0], parent, false, "");
        let bytes: Vec<u8> = ents.concat();
        self.write_chain(&chain, &bytes);
        chain[0]
    }

    fn build(mut self, root: &[Node], label: &str) -> Vec<u8> {
        let ents = if self.kind == FatKind::F32 {
            let mut used = HashSet::new();
            let count = 1 + root.iter().map(|c| Self::entry_count(c.name(), &mut used)).sum::<usize>();
            let n = (count * 32 + self.cluster_bytes() - 1) / self.cluster_bytes();
            let chain = self.alloc(n, false);
            assert_eq!(chain[0], 2);
            let ents = self.dir_entries(root, 2, 0, true, label);
            let bytes: Vec<u8> = ents.concat();
            self.write_chain(&chain, &bytes);
            ents
        } else {
            let ents = self.dir_entries(root, 0, 0, true, label);
            assert!(ents.len() <= self.root_entries, "root directory full");
            let start = (self.rsvd + 2 * self.fat_sectors) * SECTOR;
            let bytes: Vec<u8> = ents.concat();
            self.img[start..start + bytes.len()].copy_from_slice(&bytes);
            ents
        };
        let _ = ents;
        // FATs
        let mut fatb = vec![0u8; self.fat_sectors * SECTOR];
        for (i, &v) in self.fat.iter().enumerate() {
            match self.kind {
                FatKind::F32 => fatb[i * 4..i * 4 + 4].copy_from_slice(&(v & 0x0FFF_FFFF).to_le_bytes()),
                FatKind::F16 => {
                    let v = if v >= 0x0FFF_FFF8 { 0xFFFF } else { v as u16 };
                    let v = if i == 0 { 0xFFF8 } else { v };
                    fatb[i * 2..i * 2 + 2].copy_from_slice(&v.to_le_bytes());
                }
                FatKind::F12 => {
                    let v = if v >= 0x0FFF_FFF8 { 0xFFF } else { v & 0xFFF };
                    let v = if i == 0 { 0xFF0 | (self.img[21] as u32 & 0xF) } else { v };
                    let off = i + i / 2;
                    if i & 1 == 0 {
                        fatb[off] = v as u8;
                        fatb[off + 1] = (fatb[off + 1] & 0xF0) | (v >> 8) as u8;
                    } else {
                        fatb[off] = (fatb[off] & 0x0F) | ((v << 4) as u8);
                        fatb[off + 1] = (v >> 4) as u8;
                    }
                }
            }
        }
        for k in 0..2 {
            let start = (self.rsvd + k * self.fat_sectors) * SECTOR;
            self.img[start..start + fatb.len()].copy_from_slice(&fatb);
        }
        if self.kind == FatKind::F32 {
            // FSInfo + backup boot sector
            let fsi = &mut self.img[SECTOR..2 * SECTOR];
            fsi[0..4].copy_from_slice(&0x4161_5252u32.to_le_bytes());
            fsi[484..488].copy_from_slice(&0x6141_7272u32.to_le_bytes());
            fsi[488..492].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            fsi[492..496].copy_from_slice(&0xFFFF_FFFFu32.to_le_bytes());
            fsi[508..512].copy_from_slice(&0xAA55_0000u32.to_le_bytes());
            let (boot, rest) = self.img.split_at_mut(SECTOR);
            rest[5 * SECTOR..6 * SECTOR].copy_from_slice(boot);
        }
        self.img
    }
}

// ---- exFAT ---------------------------------------------------------------------------

struct ExFat {
    cluster_shift: u32,
    fat_offset: usize,
    fat_length: usize,
    heap_offset: usize,
    clusters: u32,
    img: Vec<u8>,
    fat: Vec<u32>,
    used: Vec<bool>,
}

fn upcase(c: u16) -> u16 {
    if (b'a' as u16..=b'z' as u16).contains(&c) {
        c - 32
    } else {
        c
    }
}

impl ExFat {
    fn new(total: usize) -> ExFat {
        let cluster_shift = 3; // 4 KiB clusters
        let fat_offset = 128;
        let est = total >> cluster_shift;
        let fat_length = ((est + 2) * 4 + SECTOR - 1) / SECTOR;
        let heap_offset = (fat_offset + fat_length + 63) & !63;
        let clusters = ((total - heap_offset) >> cluster_shift) as u32;
        ExFat {
            cluster_shift,
            fat_offset,
            fat_length,
            heap_offset,
            clusters,
            img: vec![0u8; total * SECTOR],
            fat: vec![0u32; clusters as usize + 2],
            used: vec![false; clusters as usize + 2],
        }
    }

    fn cb(&self) -> usize {
        SECTOR << self.cluster_shift
    }

    /// Allocate n clusters. With `frag`, leave gaps so the file must use the FAT chain.
    /// Returns the chain and whether it is contiguous.
    fn alloc(&mut self, n: usize, frag: bool) -> (Vec<u32>, bool) {
        let mut out = Vec::new();
        let mut c = 2u32;
        if !frag {
            // Find a contiguous run.
            'outer: loop {
                assert!(c as usize + n <= self.clusters as usize + 2, "volume full");
                for k in 0..n as u32 {
                    if self.used[(c + k) as usize] {
                        c += k + 1;
                        continue 'outer;
                    }
                }
                break;
            }
            out = (0..n as u32).map(|k| c + k).collect();
        } else {
            while out.len() < n {
                if !self.used[c as usize] {
                    out.push(c);
                    c += 1;
                }
                c += 1;
            }
        }
        for &c in &out {
            self.used[c as usize] = true;
        }
        let contiguous = out.windows(2).all(|w| w[1] == w[0] + 1);
        (out, contiguous)
    }

    fn chain_fat(&mut self, chain: &[u32]) {
        for w in chain.windows(2) {
            self.fat[w[0] as usize] = w[1];
        }
        self.fat[*chain.last().unwrap() as usize] = 0xFFFF_FFFF;
    }

    fn write(&mut self, chain: &[u32], data: &[u8]) {
        let cb = self.cb();
        for (i, &c) in chain.iter().enumerate() {
            let start = (self.heap_offset + ((c as usize - 2) << self.cluster_shift)) * SECTOR;
            let part = &data[(i * cb).min(data.len())..((i + 1) * cb).min(data.len())];
            self.img[start..start + part.len()].copy_from_slice(part);
        }
    }

    /// File entry set (File + Stream Extension + File Name entries).
    fn entry_set(name: &str, is_dir: bool, first: u32, len: u64, nofatchain: bool) -> Vec<[u8; 32]> {
        let units: Vec<u16> = name.encode_utf16().collect();
        let name_entries = (units.len() + 14) / 15;
        let mut set = Vec::new();
        let mut f = [0u8; 32];
        f[0] = 0x85;
        f[1] = (1 + name_entries) as u8;
        f[4..6].copy_from_slice(&(if is_dir { 0x10u16 } else { 0x20 }).to_le_bytes());
        let ts: u32 = (2026 - 1980) << 25 | 11 << 21 | 19 << 16 | 12 << 11;
        for off in [8usize, 12, 16] {
            f[off..off + 4].copy_from_slice(&ts.to_le_bytes());
        }
        set.push(f);
        let mut s = [0u8; 32];
        s[0] = 0xC0;
        s[1] = 0x01 | if nofatchain { 0x02 } else { 0 };
        s[3] = units.len() as u8;
        let mut hash: u16 = 0;
        for &u in &units {
            let u = upcase(u);
            for b in [u as u8, (u >> 8) as u8] {
                hash = (if hash & 1 != 0 { 0x8000 } else { 0 }) + (hash >> 1) + b as u16;
            }
        }
        s[4..6].copy_from_slice(&hash.to_le_bytes());
        s[8..16].copy_from_slice(&len.to_le_bytes());
        s[20..24].copy_from_slice(&first.to_le_bytes());
        s[24..32].copy_from_slice(&len.to_le_bytes());
        set.push(s);
        for k in 0..name_entries {
            let mut n = [0u8; 32];
            n[0] = 0xC1;
            for i in 0..15 {
                if let Some(&u) = units.get(k * 15 + i) {
                    n[2 + i * 2..4 + i * 2].copy_from_slice(&u.to_le_bytes());
                }
            }
            set.push(n);
        }
        // Set checksum over all bytes except the checksum field itself.
        let mut sum: u16 = 0;
        for (i, e) in set.iter().enumerate() {
            for (j, &b) in e.iter().enumerate() {
                if i == 0 && (j == 2 || j == 3) {
                    continue;
                }
                sum = (if sum & 1 != 0 { 0x8000 } else { 0 }) + (sum >> 1) + b as u16;
            }
        }
        set[0][2..4].copy_from_slice(&sum.to_le_bytes());
        set
    }

    fn set_len(name: &str) -> usize {
        2 + (name.encode_utf16().count() + 14) / 15
    }

    fn children_entries(&mut self, children: &[Node]) -> Vec<[u8; 32]> {
        let mut ents = Vec::new();
        for child in children {
            match child {
                Node::File { name, data, frag } => {
                    let (first, nofat) = if data.is_empty() {
                        (0, false)
                    } else {
                        let n = (data.len() + self.cb() - 1) / self.cb();
                        let (chain, contiguous) = self.alloc(n, *frag);
                        self.write(&chain, data);
                        if !contiguous {
                            self.chain_fat(&chain);
                        }
                        (chain[0], contiguous)
                    };
                    ents.extend(Self::entry_set(name, false, first, data.len() as u64, nofat));
                }
                Node::Dir { name, children } => {
                    let (first, len, nofat) = self.make_dir(children, name.len() % 2 == 0);
                    ents.extend(Self::entry_set(name, true, first, len, nofat));
                }
            }
        }
        ents
    }

    /// Returns (first cluster, byte length, uses NoFatChain).
    fn make_dir(&mut self, children: &[Node], use_fat_chain: bool) -> (u32, u64, bool) {
        let count = children.iter().map(|c| Self::set_len(c.name())).sum::<usize>() + 1;
        let n = ((count * 32 + self.cb() - 1) / self.cb()).max(1);
        let (chain, _) = self.alloc(n, false);
        let ents = self.children_entries(children);
        let mut bytes: Vec<u8> = ents.concat();
        bytes.resize(n * self.cb(), 0);
        self.write(&chain, &bytes);
        if use_fat_chain {
            self.chain_fat(&chain);
        }
        (chain[0], (n * self.cb()) as u64, !use_fat_chain)
    }

    fn build(mut self, root: &[Node], label: &str, part_offset: u64) -> Vec<u8> {
        let total = self.img.len() / SECTOR;
        // Allocation bitmap and up-case table first, then the root directory.
        let bitmap_len = (self.clusters as usize + 7) / 8;
        let (bm_chain, _) = self.alloc((bitmap_len + self.cb() - 1) / self.cb(), false);
        self.chain_fat(&bm_chain);
        let up: Vec<u8> = (0u16..128).flat_map(|c| upcase(c).to_le_bytes()).collect();
        let mut up_sum: u32 = 0;
        for &b in &up {
            up_sum = (if up_sum & 1 != 0 { 0x8000_0000 } else { 0 }) + (up_sum >> 1) + b as u32;
        }
        let (up_chain, _) = self.alloc(1, false);
        self.chain_fat(&up_chain);
        self.write(&up_chain, &up.clone());

        let count = 3 + root.iter().map(|c| Self::set_len(c.name())).sum::<usize>() + 1;
        let n = (count * 32 + self.cb() - 1) / self.cb();
        let (root_chain, _) = self.alloc(n, false);
        self.chain_fat(&root_chain);
        let mut ents = Vec::new();
        let mut lab = [0u8; 32];
        lab[0] = 0x83;
        let lu: Vec<u16> = label.encode_utf16().take(11).collect();
        lab[1] = lu.len() as u8;
        for (i, u) in lu.iter().enumerate() {
            lab[2 + i * 2..4 + i * 2].copy_from_slice(&u.to_le_bytes());
        }
        ents.push(lab);
        let mut bm = [0u8; 32];
        bm[0] = 0x81;
        bm[20..24].copy_from_slice(&bm_chain[0].to_le_bytes());
        bm[24..32].copy_from_slice(&(bitmap_len as u64).to_le_bytes());
        ents.push(bm);
        let mut uc = [0u8; 32];
        uc[0] = 0x82;
        uc[4..8].copy_from_slice(&up_sum.to_le_bytes());
        uc[20..24].copy_from_slice(&up_chain[0].to_le_bytes());
        uc[24..32].copy_from_slice(&(up.len() as u64).to_le_bytes());
        ents.push(uc);
        ents.extend(self.children_entries(root));
        let bytes: Vec<u8> = ents.concat();
        self.write(&root_chain, &bytes);

        // Bitmap contents (written last, after all allocations).
        let mut bitmap = vec![0u8; bitmap_len];
        for c in 2..self.clusters as usize + 2 {
            if self.used[c] {
                bitmap[(c - 2) / 8] |= 1 << ((c - 2) % 8);
            }
        }
        self.write(&bm_chain, &bitmap);

        // FAT
        self.fat[0] = 0xFFFF_FFF8;
        self.fat[1] = 0xFFFF_FFFF;
        let start = self.fat_offset * SECTOR;
        for (i, &v) in self.fat.iter().enumerate() {
            self.img[start + i * 4..start + i * 4 + 4].copy_from_slice(&v.to_le_bytes());
        }

        // Boot region (main at 0, backup at 12).
        let mut region = vec![0u8; 12 * SECTOR];
        {
            let b = &mut region[..SECTOR];
            b[0..3].copy_from_slice(&[0xEB, 0x76, 0x90]);
            b[3..11].copy_from_slice(b"EXFAT   ");
            b[64..72].copy_from_slice(&part_offset.to_le_bytes());
            b[72..80].copy_from_slice(&(total as u64).to_le_bytes());
            b[80..84].copy_from_slice(&(self.fat_offset as u32).to_le_bytes());
            b[84..88].copy_from_slice(&(self.fat_length as u32).to_le_bytes());
            b[88..92].copy_from_slice(&(self.heap_offset as u32).to_le_bytes());
            b[92..96].copy_from_slice(&self.clusters.to_le_bytes());
            b[96..100].copy_from_slice(&root_chain[0].to_le_bytes());
            b[100..104].copy_from_slice(&0x5010_1126u32.to_le_bytes());
            b[104..106].copy_from_slice(&0x0100u16.to_le_bytes());
            b[108] = 9;
            b[109] = self.cluster_shift as u8;
            b[110] = 1;
            b[111] = 0x80;
            b[112] = 0xFF;
            b[510] = 0x55;
            b[511] = 0xAA;
        }
        for s in 1..9 {
            region[s * SECTOR + 510] = 0x55;
            region[s * SECTOR + 511] = 0xAA;
        }
        let mut sum: u32 = 0;
        for (i, &b) in region[..11 * SECTOR].iter().enumerate() {
            if i == 106 || i == 107 || i == 112 {
                continue;
            }
            sum = (if sum & 1 != 0 { 0x8000_0000 } else { 0 }) + (sum >> 1) + b as u32;
        }
        for k in 0..SECTOR / 4 {
            region[11 * SECTOR + k * 4..11 * SECTOR + k * 4 + 4].copy_from_slice(&sum.to_le_bytes());
        }
        self.img[..12 * SECTOR].copy_from_slice(&region);
        self.img[12 * SECTOR..24 * SECTOR].copy_from_slice(&region);
        self.img
    }
}

// ---- partition tables -----------------------------------------------------------------

fn crc32(data: &[u8]) -> u32 {
    let mut c = 0xFFFF_FFFFu32;
    for &b in data {
        c ^= b as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 { (c >> 1) ^ 0xEDB8_8320 } else { c >> 1 };
        }
    }
    !c
}

fn mbr_image(volume: Vec<u8>, ty: u8, start: usize) -> Vec<u8> {
    let mut img = vec![0u8; start * SECTOR];
    let e = &mut img[446..462];
    e[0] = 0;
    e[1..4].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    e[4] = ty;
    e[5..8].copy_from_slice(&[0xFE, 0xFF, 0xFF]);
    e[8..12].copy_from_slice(&(start as u32).to_le_bytes());
    e[12..16].copy_from_slice(&((volume.len() / SECTOR) as u32).to_le_bytes());
    img[510] = 0x55;
    img[511] = 0xAA;
    img.extend(volume);
    img
}

fn guid_bytes(s: &str) -> [u8; 16] {
    let hex: String = s.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    let b: Vec<u8> = (0..16).map(|i| u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).unwrap()).collect();
    let mut g = [0u8; 16];
    g[0..4].copy_from_slice(&[b[3], b[2], b[1], b[0]]);
    g[4..6].copy_from_slice(&[b[5], b[4]]);
    g[6..8].copy_from_slice(&[b[7], b[6]]);
    g[8..16].copy_from_slice(&b[8..16]);
    g
}

fn gpt_image(volume: Vec<u8>, start: usize, name: &str) -> Vec<u8> {
    let vol_sectors = volume.len() / SECTOR;
    let total = start + vol_sectors + 34;
    let mut img = vec![0u8; total * SECTOR];
    // Protective MBR
    let e = &mut img[446..462];
    e[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
    e[4] = 0xEE;
    e[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    e[8..12].copy_from_slice(&1u32.to_le_bytes());
    e[12..16].copy_from_slice(&((total - 1).min(0xFFFF_FFFF) as u32).to_le_bytes());
    img[510] = 0x55;
    img[511] = 0xAA;
    // Partition entries
    let mut entries = vec![0u8; 128 * 128];
    entries[0..16].copy_from_slice(&guid_bytes("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7"));
    entries[16..32].copy_from_slice(&guid_bytes("5010A126-0000-4000-8000-534F4C4F4E31"));
    entries[32..40].copy_from_slice(&(start as u64).to_le_bytes());
    entries[40..48].copy_from_slice(&((start + vol_sectors - 1) as u64).to_le_bytes());
    for (i, u) in name.encode_utf16().take(36).enumerate() {
        entries[56 + i * 2..58 + i * 2].copy_from_slice(&u.to_le_bytes());
    }
    let ecrc = crc32(&entries);
    let header = |me: u64, other: u64, entries_lba: u64| -> Vec<u8> {
        let mut h = vec![0u8; 92];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&me.to_le_bytes());
        h[32..40].copy_from_slice(&other.to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&((total - 34) as u64).to_le_bytes());
        h[56..72].copy_from_slice(&guid_bytes("5010A126-0000-4000-8000-534F4C4F4E30"));
        h[72..80].copy_from_slice(&entries_lba.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&ecrc.to_le_bytes());
        let c = crc32(&h);
        h[16..20].copy_from_slice(&c.to_le_bytes());
        h
    };
    let last = (total - 1) as u64;
    img[SECTOR..SECTOR + 92].copy_from_slice(&header(1, last, 2));
    img[2 * SECTOR..2 * SECTOR + entries.len()].copy_from_slice(&entries);
    let backup_entries = total - 33;
    img[backup_entries * SECTOR..backup_entries * SECTOR + entries.len()].copy_from_slice(&entries);
    img[(total - 1) * SECTOR..(total - 1) * SECTOR + 92].copy_from_slice(&header(last, 1, backup_entries as u64));
    img[start * SECTOR..(start + vol_sectors) * SECTOR].copy_from_slice(&volume);
    img
}

// ---- main ---------------------------------------------------------------------------

// ---- internal-disk test helpers ------------------------------------------------------

fn le32(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes(b[o..o + 4].try_into().unwrap())
}
fn le64(b: &[u8], o: usize) -> u64 {
    u64::from_le_bytes(b[o..o + 8].try_into().unwrap())
}

/// A disk like a PC with Windows: GPT, EFI system partition, a data partition with files,
/// and free space after them.
fn windows_like(path: &Path, mib: usize, samples: &Path) {
    let total = mib * 1024 * 1024 / SECTOR;
    let esp_sectors = 64 * 1024 * 1024 / SECTOR;
    let data_sectors = 80 * 1024 * 1024 / SECTOR;
    let esp_vol = Fat::new(FatKind::F32, esp_sectors, 1, 0, 2048, "ESP")
        .build(&[dir("EFI", vec![dir("Microsoft", vec![file("bootmgfw.efi", b"not really a boot manager")])])], "ESP");
    let photo = fs::read(samples.join("sunset.jpg")).unwrap();
    let data_vol = Fat::new(FatKind::F32, data_sectors, 1, 0, (2048 + esp_sectors) as u32, "WINDOWS")
        .build(&[dir("Users", vec![dir("Someone", vec![file("precious.jpg", &photo)])])], "WINDOWS");
    let mut img = gpt_image_multi(
        total,
        &[
            ("C12A7328-F81F-11D2-BA4B-00A0C93EC93B", 2048, esp_vol, "EFI system partition"),
            ("EBD0A0A2-B9E5-4433-87C0-68B6B72699C7", 2048 + esp_sectors, data_vol, "Basic data partition"),
        ],
    );
    img.resize(total * SECTOR, 0);
    fs::write(path, img).unwrap();
}

fn gpt_image_multi(total: usize, parts: &[(&str, usize, Vec<u8>, &str)]) -> Vec<u8> {
    let mut img = vec![0u8; total * SECTOR];
    let e = &mut img[446..462];
    e[1..4].copy_from_slice(&[0x00, 0x02, 0x00]);
    e[4] = 0xEE;
    e[5..8].copy_from_slice(&[0xFF, 0xFF, 0xFF]);
    e[8..12].copy_from_slice(&1u32.to_le_bytes());
    e[12..16].copy_from_slice(&((total - 1) as u32).to_le_bytes());
    img[510] = 0x55;
    img[511] = 0xAA;
    let mut entries = vec![0u8; 128 * 128];
    for (i, (ty, start, vol, name)) in parts.iter().enumerate() {
        let e = &mut entries[i * 128..(i + 1) * 128];
        e[0..16].copy_from_slice(&guid_bytes(ty));
        e[16..32].copy_from_slice(&guid_bytes(&format!("5010A126-0000-4000-8000-00000000000{}", i + 2)));
        e[32..40].copy_from_slice(&(*start as u64).to_le_bytes());
        e[40..48].copy_from_slice(&((start + vol.len() / SECTOR - 1) as u64).to_le_bytes());
        for (k, u) in name.encode_utf16().enumerate() {
            e[56 + k * 2..58 + k * 2].copy_from_slice(&u.to_le_bytes());
        }
        img[start * SECTOR..start * SECTOR + vol.len()].copy_from_slice(vol);
    }
    let ecrc = crc32(&entries);
    let header = |me: u64, other: u64, elba: u64| -> Vec<u8> {
        let mut h = vec![0u8; 92];
        h[0..8].copy_from_slice(b"EFI PART");
        h[8..12].copy_from_slice(&0x0001_0000u32.to_le_bytes());
        h[12..16].copy_from_slice(&92u32.to_le_bytes());
        h[24..32].copy_from_slice(&me.to_le_bytes());
        h[32..40].copy_from_slice(&other.to_le_bytes());
        h[40..48].copy_from_slice(&34u64.to_le_bytes());
        h[48..56].copy_from_slice(&((total - 34) as u64).to_le_bytes());
        h[56..72].copy_from_slice(&guid_bytes("5010A126-0000-4000-8000-534F4C4F4E39"));
        h[72..80].copy_from_slice(&elba.to_le_bytes());
        h[80..84].copy_from_slice(&128u32.to_le_bytes());
        h[84..88].copy_from_slice(&128u32.to_le_bytes());
        h[88..92].copy_from_slice(&ecrc.to_le_bytes());
        let c = crc32(&h);
        h[16..20].copy_from_slice(&c.to_le_bytes());
        h
    };
    let last = (total - 1) as u64;
    img[SECTOR..SECTOR + 92].copy_from_slice(&header(1, last, 2));
    img[2 * SECTOR..2 * SECTOR + entries.len()].copy_from_slice(&entries);
    let be = total - 33;
    img[be * SECTOR..be * SECTOR + entries.len()].copy_from_slice(&entries);
    img[(total - 1) * SECTOR..(total - 1) * SECTOR + 92].copy_from_slice(&header(last, 1, be as u64));
    img
}

/// Check a GPT disk image: both headers and entry arrays valid and identical, partitions
/// inside the usable area and not overlapping, exFAT boot checksums correct.
/// Prints one line per partition: "part <n> <first> <last> <name> fnv <hash>".
fn check_disk(path: &Path) -> bool {
    let img = fs::read(path).unwrap();
    let total = (img.len() / SECTOR) as u64;
    let mut ok = true;
    let mut fail = |m: String| {
        println!("FAIL {}", m);
        ok = false;
    };
    let hdr = |lba: u64| &img[lba as usize * SECTOR..lba as usize * SECTOR + 92];
    let p = hdr(1);
    if &p[0..8] != b"EFI PART" {
        println!("FAIL no GPT");
        return false;
    }
    let backup_lba = le64(p, 32);
    if backup_lba != total - 1 {
        fail(format!("backup header at {} not at last block {}", backup_lba, total - 1));
    }
    let b = hdr(backup_lba.min(total - 1));
    let mut entries_seen = Vec::new();
    for (name, h, me) in [("primary", p, 1u64), ("backup", b, backup_lba)] {
        let mut hh = h.to_vec();
        let stored = le32(&hh, 16);
        hh[16..20].fill(0);
        if crc32(&hh) != stored {
            fail(format!("{} header CRC", name));
        }
        if le64(h, 24) != me {
            fail(format!("{} header 'my LBA' is {}", name, le64(h, 24)));
        }
        let elba = le64(h, 72) as usize;
        let n = le32(h, 80) as usize * le32(h, 84) as usize;
        let ent = &img[elba * SECTOR..elba * SECTOR + n];
        if crc32(ent) != le32(h, 88) {
            fail(format!("{} entries CRC", name));
        }
        entries_seen.push(ent.to_vec());
    }
    if entries_seen.len() == 2 && entries_seen[0] != entries_seen[1] {
        fail(String::from("primary and backup entries differ"));
    }
    let (first_usable, last_usable) = (le64(p, 40), le64(p, 48));
    let ent = &entries_seen[0];
    let mut ranges = Vec::new();
    for i in 0..le32(p, 80) as usize {
        let e = &ent[i * 128..(i + 1) * 128];
        if e[0..16].iter().all(|&x| x == 0) {
            continue;
        }
        let (f, l) = (le64(e, 32), le64(e, 40));
        let name: String = char::decode_utf16((0..36).map(|k| u16::from_le_bytes([e[56 + k * 2], e[57 + k * 2]])).take_while(|&u| u != 0))
            .map(|r| r.unwrap_or('?'))
            .collect();
        if f < first_usable || l > last_usable || l < f {
            fail(format!("partition {} outside usable area", i));
        }
        let data = &img[f as usize * SECTOR..(l as usize + 1) * SECTOR];
        if &data[3..11] == b"EXFAT   " {
            let mut sum: u32 = 0;
            for (k, &x) in data[..11 * SECTOR].iter().enumerate() {
                if k != 106 && k != 107 && k != 112 {
                    sum = (if sum & 1 != 0 { 0x8000_0000 } else { 0 }) + (sum >> 1) + x as u32;
                }
            }
            if le32(&data[11 * SECTOR..], 0) != sum {
                fail(format!("partition {} exFAT boot checksum", i));
            }
            if data[..12 * SECTOR] != data[12 * SECTOR..24 * SECTOR] {
                fail(format!("partition {} exFAT backup boot region differs", i));
            }
            if le64(data, 64) != f {
                fail(format!("partition {} exFAT PartitionOffset {} != {}", i, le64(data, 64), f));
            }
        }
        println!("part {} {} {} \"{}\" fnv {:08x}", i, f, l, name, fnv(data));
        ranges.push((f, l, i));
    }
    ranges.sort();
    for w in ranges.windows(2) {
        if w[1].0 <= w[0].1 {
            fail(format!("partitions {} and {} overlap", w[0].2, w[1].2));
        }
    }
    if ok {
        println!("GPT OK");
    }
    ok
}

/// Read a host folder into a file tree.
fn tree_from(dirpath: &Path) -> Vec<Node> {
    let mut out = Vec::new();
    let mut entries: Vec<_> = fs::read_dir(dirpath).unwrap().map(|e| e.unwrap()).collect();
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        let name = e.file_name().to_string_lossy().into_owned();
        if e.file_type().unwrap().is_dir() {
            out.push(dir(&name, tree_from(&e.path())));
        } else {
            out.push(file(&name, &fs::read(e.path()).unwrap()));
        }
    }
    out
}

/// Bootable Solon USB image: GPT with one FAT32 EFI system partition holding `espdir`.
fn usb_image(out: &Path, espdir: &Path) {
    let esp_sectors = 64 * 1024 * 1024 / SECTOR;
    let tree = tree_from(espdir);
    let vol = Fat::new(FatKind::F32, esp_sectors, 1, 0, 2048, "SOLONBOOT").build(&tree, "SOLONBOOT");
    let total = 2048 + esp_sectors + 2048; // 1 MiB slack at the end for the backup GPT
    let img = gpt_image_multi(total, &[("C12A7328-F81F-11D2-BA4B-00A0C93EC93B", 2048, vol, "Solon boot")]);
    fs::write(out, img).unwrap();
}

fn find(hay: &[u8], needle: &[u8]) -> Option<usize> {
    hay.windows(needle.len()).position(|w| w == needle)
}

/// Decode PWG Raster pages to PPM files. Returns the number of pages.
fn decode_pwg(data: &[u8], outdir: &Path) -> usize {
    if data.len() < 4 || &data[0..4] != b"RaS2" {
        println!("FAIL document is not PWG Raster (no RaS2 sync word)");
        return 0;
    }
    let be = |b: &[u8], o: usize| u32::from_be_bytes(b[o..o + 4].try_into().unwrap()) as usize;
    let mut i = 4;
    let mut pages = 0;
    while i + 1796 <= data.len() {
        let h = &data[i..i + 1796];
        i += 1796;
        let (w, ht, bpp, bpl, cs) = (be(h, 372), be(h, 376), be(h, 388) / 8, be(h, 392), be(h, 400));
        let name = String::from_utf8_lossy(&h[1732..1796]).trim_end_matches('\0').to_string();
        println!(
            "pwg page {}: {}x{} @ {}x{} dpi, {} bytes/pixel, color space {}, PageSize {}x{} pt, media \"{}\", copies {}, total pages {}",
            pages + 1, w, ht, be(h, 276), be(h, 280), bpp, cs, be(h, 352), be(h, 356), name, be(h, 340), be(h, 452)
        );
        if bpl != w * bpp || !(bpp == 1 || bpp == 3) {
            println!("FAIL inconsistent header");
            return pages;
        }
        let mut img = vec![0u8; w * ht * 3];
        let mut y = 0;
        while y < ht {
            let rep = data[i] as usize + 1;
            i += 1;
            let mut line = Vec::with_capacity(bpl);
            while line.len() < bpl {
                let c = data[i];
                i += 1;
                if c < 128 {
                    let px = &data[i..i + bpp];
                    i += bpp;
                    for _ in 0..c as usize + 1 {
                        line.extend_from_slice(px);
                    }
                } else if c == 128 {
                    line.resize(bpl, 255);
                } else {
                    let n = 257 - c as usize;
                    line.extend_from_slice(&data[i..i + n * bpp]);
                    i += n * bpp;
                }
            }
            if line.len() != bpl {
                println!("FAIL line overflow at row {}", y);
                return pages;
            }
            for _ in 0..rep {
                if y >= ht {
                    break;
                }
                for x in 0..w {
                    let p = &line[x * bpp..x * bpp + bpp];
                    let rgb = if bpp == 3 { [p[0], p[1], p[2]] } else { [p[0]; 3] };
                    img[(y * w + x) * 3..(y * w + x) * 3 + 3].copy_from_slice(&rgb);
                }
                y += 1;
            }
        }
        pages += 1;
        let mut ppm = format!("P6\n{} {}\n255\n", w, ht).into_bytes();
        ppm.extend_from_slice(&img);
        fs::write(outdir.join(format!("pwg-page{}.ppm", pages)), ppm).unwrap();
    }
    if i != data.len() {
        println!("FAIL {} trailing bytes after the last page", data.len() - i);
    }
    pages
}

/// Pull the print job out of a COM2 capture (which starts with firmware console text).
fn extract_job(input: &Path, outdir: &Path) -> bool {
    let raw = fs::read(input).unwrap();
    if let Some(at) = find(&raw, b"\x1b%-12345X") {
        fs::write(outdir.join("job.pcl"), &raw[at..]).unwrap();
        println!("PCL job: {} bytes", raw.len() - at);
        return true;
    }
    let Some(at) = find(&raw, b"POST /ipp/print HTTP/1.1\r\n") else {
        println!("FAIL no print job found");
        return false;
    };
    let http = &raw[at..];
    let head_end = find(http, b"\r\n\r\n").unwrap() + 4;
    let head = String::from_utf8_lossy(&http[..head_end]).to_string();
    let len: usize = head.lines().find_map(|l| l.strip_prefix("Content-Length: ")).unwrap().trim().parse().unwrap();
    let body = &http[head_end..];
    if body.len() < len {
        println!("FAIL body truncated: {} of {} bytes", body.len(), len);
        return false;
    }
    let body = &body[..len];
    println!("IPP request: version {}.{}, operation {:#06x}, request id {}", body[0], body[1], u16::from_be_bytes([body[2], body[3]]), le32(&[body[7], body[6], body[5], body[4]], 0));
    let mut i = 8;
    let mut last = String::new();
    loop {
        let tag = body[i];
        i += 1;
        if tag == 0x03 {
            break;
        }
        if tag < 0x10 {
            println!("  group {:#04x}", tag);
            continue;
        }
        let nl = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
        let name = String::from_utf8_lossy(&body[i + 2..i + 2 + nl]).to_string();
        i += 2 + nl;
        let vl = u16::from_be_bytes([body[i], body[i + 1]]) as usize;
        let val = &body[i + 2..i + 2 + vl];
        i += 2 + vl;
        if !name.is_empty() {
            last = name;
        }
        let shown = if tag == 0x21 && vl == 4 { format!("{}", u32::from_be_bytes(val.try_into().unwrap())) } else { String::from_utf8_lossy(val).to_string() };
        println!("  {} = {}", last, shown);
    }
    decode_pwg(&body[i..], outdir) > 0
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    match args.get(1).map(|s| s.as_str()) {
        Some("printjob") if args.len() == 4 => {
            fs::create_dir_all(&args[3]).unwrap();
            std::process::exit(if extract_job(Path::new(&args[2]), Path::new(&args[3])) { 0 } else { 1 });
        }
        Some("fatstick") if args.len() == 5 => {
            // A stick as Windows formats it (MBR + FAT32) with a folder's files copied on.
            let mib: usize = args[4].parse().unwrap();
            let total = mib * 1024 * 1024 / SECTOR - 2048;
            let vol = Fat::new(FatKind::F32, total, 8, 0, 2048, "USBSTICK").build(&tree_from(Path::new(&args[3])), "USBSTICK");
            fs::write(&args[2], mbr_image(vol, 0x0C, 2048)).unwrap();
            return;
        }
        Some("usbimg") if args.len() == 4 => {
            usb_image(Path::new(&args[2]), Path::new(&args[3]));
            println!("wrote {}", args[2]);
            return;
        }
        Some("blank") if args.len() == 4 => {
            let mib: usize = args[3].parse().unwrap();
            fs::write(&args[2], vec![0u8; mib * 1024 * 1024]).unwrap();
            return;
        }
        Some("windows-like") if args.len() == 5 => {
            windows_like(Path::new(&args[2]), args[3].parse().unwrap(), Path::new(&args[4]));
            return;
        }
        Some("sym") if args.len() == 4 => {
            sym_lookup(Path::new(&args[2]), u64::from_str_radix(args[3].trim_start_matches("0x"), 16).unwrap());
            return;
        }
        Some("fatcheck") if args.len() == 3 => {
            std::process::exit(if fat_check(Path::new(&args[2])) { 0 } else { 1 });
        }
        Some("check") if args.len() == 3 => {
            std::process::exit(if check_disk(Path::new(&args[2])) { 0 } else { 1 });
        }
        _ => {}
    }
    if args.len() != 3 {
        eprintln!("usage: mkimg <samples folder> <output folder>");
        eprintln!("       mkimg blank <file> <MiB>");
        eprintln!("       mkimg windows-like <file> <MiB> <samples folder>");
        eprintln!("       mkimg check <disk image>");
        eprintln!("       mkimg fatcheck <disk image>");
        eprintln!("       mkimg sym <kernel elf> <hex address>");
        std::process::exit(2);
    }
    let src = Path::new(&args[1]);
    let out = Path::new(&args[2]);
    fs::create_dir_all(out).unwrap();
    let read = |n: &str| fs::read(src.join(n)).unwrap_or_else(|e| panic!("{}: {}", n, e));
    let (sunset, forest, portrait, badge, tiny) =
        (read("sunset.jpg"), read("forest-lake.jpg"), read("portrait.jpg"), read("badge.png"), read("tiny.png"));

    let many = |prefix: &str| -> Vec<Node> { (1..=300).map(|i| file(&format!("{}{:03}.png", prefix, i), &tiny)).collect() };
    let mut man = Vec::new();

    // FAT32, MBR, 64 MiB
    let fat32_tree = vec![
        dir(
            "DCIM",
            vec![dir(
                "100CANON",
                vec![
                    file("IMG_0001.JPG", &sunset),
                    frag_file("IMG_0002.JPG", &forest),
                    frag_file("IMG_0003.JPG", &portrait),
                    file("IMG_0004.PNG", &badge),
                ],
            )],
        ),
        dir(
            "Long folder name with spaces",
            vec![dir(
                "Another level",
                vec![file("A photo with a really long file name that needs several LFN entries.png", &badge)],
            )],
        ),
        dir("many photos", many("photo_")),
        file("notes.txt", b"Plain notes file (8.3 name stored with lowercase flags).\n"),
        file("README.TXT", b"Upper-case 8.3 name.\n"),
        file("Mixed Case.Jpg", &tiny),
    ];
    let total = 64 * 1024 * 1024 / SECTOR - 2048;
    let vol = Fat::new(FatKind::F32, total, 1, 0, 2048, "VACATION").build(&fat32_tree, "VACATION");
    fs::write(out.join("fat32-mbr.img"), mbr_image(vol, 0x0C, 2048)).unwrap();
    fat32_tree.iter().for_each(|n| manifest(n, &mut man));

    // exFAT, GPT, 64 MiB
    let exfat_tree = vec![
        dir(
            "DCIM",
            vec![dir(
                "100MEDIA",
                vec![
                    frag_file("sunset (fragmented).jpg", &sunset),
                    file("forest-lake.jpg", &forest),
                    file("portrait.jpg", &portrait),
                ],
            )],
        ),
        dir("Caf\u{e9} photos", vec![file("Caf\u{e9} na\u{ef}ve r\u{e9}sum\u{e9}.png", &badge)]),
        dir("Many photos", many("img_")),
        dir("empty folder", vec![]),
        file("tiny.png", &tiny),
    ];
    let total = 64 * 1024 * 1024 / SECTOR - 2048 - 34;
    let vol = ExFat::new(total).build(&exfat_tree, "SDCARD", 2048);
    fs::write(out.join("exfat-gpt.img"), gpt_image(vol, 2048, "SDCARD")).unwrap();
    exfat_tree.iter().for_each(|n| manifest(n, &mut man));

    // FAT16, no partition table, 32 MiB
    let fat16_tree = vec![
        dir("DCIM", vec![dir("100OLD", vec![file("SUNSET.JPG", &sunset), file("forest.jpg", &forest)])]),
        file("badge16.png", &badge),
    ];
    let vol = Fat::new(FatKind::F16, 32 * 1024 * 1024 / SECTOR, 4, 512, 0, "CARD16").build(&fat16_tree, "CARD16");
    fs::write(out.join("fat16-super.img"), vol).unwrap();
    fat16_tree.iter().for_each(|n| manifest(n, &mut man));

    // FAT12 floppy, 1.44 MB
    let fat12_tree = vec![frag_file("PORTRAIT.JPG", &portrait), file("tiny12.png", &tiny)];
    let vol = Fat::new(FatKind::F12, 2880, 1, 224, 0, "OLDCAM").build(&fat12_tree, "OLDCAM");
    fs::write(out.join("fat12-floppy.img"), vol).unwrap();
    fat12_tree.iter().for_each(|n| manifest(n, &mut man));

    man.sort();
    man.dedup();
    fs::write(out.join("manifest.txt"), man.join("\n") + "\n").unwrap();
    println!("wrote 4 images + manifest.txt to {}", out.display());
}

// ---- FAT consistency check (for drives Solon wrote to) ----------------------------

fn le16f(b: &[u8], o: usize) -> u32 {
    u16::from_le_bytes([b[o], b[o + 1]]) as u32
}

/// Check a FAT12/16/32 volume (whole disk with MBR, or a bare volume): FAT copies agree,
/// every chain is valid and the right length, nothing cross-linked or lost, long names
/// match their short names. Prints every file with its size and FNV-1a hash.
fn fat_check(path: &Path) -> bool {
    let img = fs::read(path).unwrap();
    let mut base = 0usize;
    let bpb_ok = |o: usize| o + 512 <= img.len() && img[o + 510] == 0x55 && img[o + 511] == 0xAA && le16f(&img, o + 11) == 512 && img[o + 13] != 0;
    if !bpb_ok(0) || (img[0] != 0xEB && img[0] != 0xE9) {
        let start = le32(&img, 446 + 8) as usize * SECTOR;
        if !bpb_ok(start) {
            println!("FAIL no FAT volume found");
            return false;
        }
        base = start;
    }
    let b = &img[base..];
    let bps = le16f(b, 11) as usize;
    let spc = b[13] as usize;
    let reserved = le16f(b, 14) as usize;
    let nfats = b[16] as usize;
    let root_entries = le16f(b, 17) as usize;
    let total = if le16f(b, 19) != 0 { le16f(b, 19) as usize } else { le32(b, 32) as usize };
    let fat_sz = if le16f(b, 22) != 0 { le16f(b, 22) as usize } else { le32(b, 36) as usize };
    let root_secs = (root_entries * 32 + bps - 1) / bps;
    let data_start = reserved + nfats * fat_sz + root_secs;
    let clusters = (total - data_start) / spc;
    let bits = if clusters < 4085 { 12 } else if clusters < 65525 { 16 } else { 32 };
    let csize = bps * spc;
    println!("FAT{}: {} clusters of {} bytes, {} FAT copies", bits, clusters, csize, nfats);
    let mut ok = true;
    let fail = |m: String, ok: &mut bool| {
        println!("FAIL {}", m);
        *ok = false;
    };
    let fat0 = &b[reserved * bps..(reserved + fat_sz) * bps];
    for k in 1..nfats {
        let f = &b[(reserved + k * fat_sz) * bps..(reserved + (k + 1) * fat_sz) * bps];
        if f != fat0 {
            fail(format!("FAT copy {} differs from FAT 1", k + 1), &mut ok);
        }
    }
    let get = |c: usize| -> u32 {
        match bits {
            12 => {
                let o = c + c / 2;
                let v = le16f(fat0, o);
                if c & 1 == 1 { v >> 4 } else { v & 0xFFF }
            }
            16 => le16f(fat0, c * 2),
            _ => le32(fat0, c * 4) & 0x0FFF_FFFF,
        }
    };
    let eoc = |v: u32| match bits {
        12 => v >= 0xFF8,
        16 => v >= 0xFFF8,
        _ => v >= 0x0FFF_FFF8,
    };
    let mut owner: Vec<bool> = vec![false; clusters + 2];
    let cl_off = |c: usize| (data_start + (c - 2) * spc) * bps;
    // Follow a chain; returns its clusters.
    let chain = |first: usize, what: &str, owner: &mut Vec<bool>, ok: &mut bool| -> Vec<usize> {
        let mut out = Vec::new();
        let mut c = first;
        loop {
            if c < 2 || c >= clusters + 2 {
                fail(format!("{}: chain points to cluster {}", what, c), ok);
                return out;
            }
            if owner[c] {
                fail(format!("{}: cluster {} is used twice (cross-linked)", what, c), ok);
                return out;
            }
            owner[c] = true;
            out.push(c);
            let v = get(c);
            if eoc(v) {
                return out;
            }
            if v == 0 {
                fail(format!("{}: chain runs into a free cluster after {}", what, c), ok);
                return out;
            }
            c = v as usize;
        }
    };
    let short_sum = |e: &[u8]| -> u8 { e[..11].iter().fold(0u8, |s, &x| s.rotate_right(1).wrapping_add(x)) };
    let root_bytes: Vec<u8> = if bits == 32 {
        let rc = le32(b, 44) as usize;
        let cs = chain(rc, "root folder", &mut owner, &mut ok);
        cs.iter().flat_map(|&c| b[cl_off(c)..cl_off(c) + csize].to_vec()).collect()
    } else {
        b[(reserved + nfats * fat_sz) * bps..data_start * bps].to_vec()
    };
    // (directory bytes, path, own cluster, parent cluster)
    let mut stack = vec![(root_bytes, String::new(), 0usize, 0usize)];
    let (mut files, mut dirs) = (0, 0);
    while let Some((bytes, path, me, parent)) = stack.pop() {
        let mut lfn: Vec<(u8, Vec<u16>)> = Vec::new();
        for (i, e) in bytes.chunks(32).enumerate() {
            if e[0] == 0 {
                break;
            }
            if e[0] == 0xE5 {
                lfn.clear();
                continue;
            }
            if e[11] == 0x0F {
                let mut u = Vec::new();
                for o in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
                    u.push(u16::from_le_bytes([e[o], e[o + 1]]));
                }
                lfn.push((e[13], u));
                continue;
            }
            if e[11] & 0x08 != 0 {
                lfn.clear();
                continue; // volume label
            }
            let short: String = {
                let n = String::from_utf8_lossy(&e[0..8]).trim_end().to_string();
                let x = String::from_utf8_lossy(&e[8..11]).trim_end().to_string();
                if x.is_empty() { n } else { format!("{}.{}", n, x) }
            };
            let first = (le16f(e, 26) | (le16f(e, 20) << 16)) as usize;
            if me != 0 && i < 2 {
                let want = if i == 0 { (".", me) } else { ("..", parent) };
                if short != want.0 || first != want.1 {
                    fail(format!("{}: entry {} is \"{}\"->{}, expected \"{}\"->{}", path, i, short, first, want.0, want.1), &mut ok);
                }
                lfn.clear();
                continue;
            }
            let name = if lfn.is_empty() {
                short.clone()
            } else {
                let sum = short_sum(e);
                if lfn.iter().any(|(s, _)| *s != sum) {
                    fail(format!("{}/{}: long name checksum mismatch", path, short), &mut ok);
                }
                let units: Vec<u16> = lfn.iter().rev().flat_map(|(_, u)| u.clone()).take_while(|&u| u != 0 && u != 0xFFFF).collect();
                String::from_utf16_lossy(&units)
            };
            lfn.clear();
            let full = format!("{}/{}", path, name);
            let size = le32(e, 28) as usize;
            if e[11] & 0x10 != 0 {
                dirs += 1;
                let cs = chain(first, &full, &mut owner, &mut ok);
                let data: Vec<u8> = cs.iter().flat_map(|&c| b[cl_off(c)..cl_off(c) + csize].to_vec()).collect();
                println!("  {}/", full);
                stack.push((data, full, first, me));
            } else {
                files += 1;
                if size == 0 {
                    if first != 0 {
                        fail(format!("{}: empty file owns cluster {}", full, first), &mut ok);
                    }
                    println!("  {} 0 bytes", full);
                    continue;
                }
                let cs = chain(first, &full, &mut owner, &mut ok);
                let need = (size + csize - 1) / csize;
                if cs.len() != need {
                    fail(format!("{}: {} bytes needs {} clusters, chain has {}", full, size, need, cs.len()), &mut ok);
                    continue;
                }
                let data: Vec<u8> = cs.iter().flat_map(|&c| b[cl_off(c)..cl_off(c) + csize].to_vec()).collect();
                println!("  {} {} bytes fnv {:08x}", full, size, fnv(&data[..size]));
            }
        }
    }
    let lost = (2..clusters + 2).filter(|&c| get(c) != 0 && !owner[c]).count();
    if lost > 0 {
        fail(format!("{} lost cluster(s) marked used but owned by nothing", lost), &mut ok);
    }
    println!("{} files, {} folders: {}", files, dirs, if ok { "FAT OK" } else { "PROBLEMS FOUND" });
    ok
}

// ---- symbol lookup (for crash screens from real PCs) ---------------------------------

/// Print the function containing `addr` in an ELF64 kernel (from its symbol table).
fn sym_lookup(path: &Path, addr: u64) {
    let b = fs::read(path).unwrap();
    let u16_ = |o: usize| u16::from_le_bytes([b[o], b[o + 1]]) as usize;
    let u32_ = |o: usize| u32::from_le_bytes(b[o..o + 4].try_into().unwrap()) as usize;
    let u64_ = |o: usize| u64::from_le_bytes(b[o..o + 8].try_into().unwrap());
    let shoff = u64_(0x28) as usize;
    let shentsize = u16_(0x3A);
    let shnum = u16_(0x3C);
    let mut best: Option<(u64, u64, String)> = None;
    for i in 0..shnum {
        let sh = shoff + i * shentsize;
        if u32_(sh + 4) != 2 {
            continue; // SHT_SYMTAB
        }
        let off = u64_(sh + 0x18) as usize;
        let size = u64_(sh + 0x20) as usize;
        let link = u32_(sh + 0x28);
        let strsh = shoff + link * shentsize;
        let stroff = u64_(strsh + 0x18) as usize;
        for s in (off..off + size).step_by(24) {
            let name = u32_(s);
            let value = u64_(s + 8);
            let sz = u64_(s + 16);
            if value <= addr && addr < value + sz.max(1) && best.as_ref().is_none_or(|(v, _, _)| value >= *v) {
                let end = b[stroff + name..].iter().position(|&c| c == 0).unwrap();
                let n = String::from_utf8_lossy(&b[stroff + name..stroff + name + end]).into_owned();
                best = Some((value, sz, n));
            }
        }
    }
    match best {
        Some((v, sz, n)) => println!("{:#x} = {} + {:#x} (size {:#x})", addr, n, addr - v, sz),
        None => println!("{:#x}: no symbol", addr),
    }
}

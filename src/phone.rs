//! Phones and cameras over USB: MTP (Android "File transfer") and PTP (iPhone photos and
//! videos, digital cameras). Both are the same request/response protocol over two bulk
//! endpoints. Solon only reads from phones.
//!
//! Boot option "testphone" adds a pretend phone that answers like a real one (for testing
//! without a phone; QEMU can't emulate one on Windows).

use alloc::collections::BTreeMap;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{DirEntry, FsError};
use crate::memory::Dma;
use crate::xhci::{UsbError, Xhci};

// Operation codes
const GET_DEVICE_INFO: u16 = 0x1001;
const OPEN_SESSION: u16 = 0x1002;
const CLOSE_SESSION: u16 = 0x1003;
const GET_STORAGE_IDS: u16 = 0x1004;
const GET_STORAGE_INFO: u16 = 0x1005;
const GET_OBJECT_HANDLES: u16 = 0x1007;
const GET_OBJECT_INFO: u16 = 0x1008;
const GET_OBJECT: u16 = 0x1009;
const GET_PARTIAL_OBJECT: u16 = 0x101B;
const GET_PARTIAL_OBJECT_64: u16 = 0x95C1; // MTP
const GET_OBJECT_PROP_VALUE: u16 = 0x9803; // MTP
const PROP_OBJECT_SIZE: u32 = 0xDC04;

// Responses
const OK: u16 = 0x2001;
const SESSION_ALREADY_OPEN: u16 = 0x201E;

const FORMAT_FOLDER: u16 = 0x3001;
const ROOT: u32 = 0xFFFF_FFFF;

/// Pieces of a big file fetched at a time.
pub const PIECE: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PhoneKind {
    Android,
    IPhone,
    Camera,
}

#[derive(Clone)]
pub struct PhoneStorage {
    pub id: u32,
    pub label: String,
    pub capacity: u64,
    pub free: u64,
}

/// How we talk to the phone.
enum Link {
    Usb { ctrl: usize, slot: u8, din: u8, dout: u8, buf: Dma },
    Test(TestPhone),
}

pub struct Phone {
    pub id: u32,
    pub name: String,
    pub serial: String,
    pub kind: PhoneKind,
    pub storages: Vec<PhoneStorage>,
    link: Link,
    tid: u32,
    session: bool,
    ops: Vec<u16>,
    /// Object handle -> (size, parent) learned from listings (sizes of big files).
    sizes: BTreeMap<u32, u64>,
    /// Why the phone isn't sharing files, if it isn't.
    pub problem: Option<&'static str>,
}

// ---- data encoding -----------------------------------------------------------------------

struct Rd<'a> {
    b: &'a [u8],
    p: usize,
}

impl<'a> Rd<'a> {
    fn u8(&mut self) -> u8 {
        let v = self.b.get(self.p).copied().unwrap_or(0);
        self.p += 1;
        v
    }
    fn u16(&mut self) -> u16 {
        let v = self.b.get(self.p..self.p + 2).map(|s| u16::from_le_bytes([s[0], s[1]])).unwrap_or(0);
        self.p += 2;
        v
    }
    fn u32(&mut self) -> u32 {
        let v = self.b.get(self.p..self.p + 4).map(|s| u32::from_le_bytes(s.try_into().unwrap())).unwrap_or(0);
        self.p += 4;
        v
    }
    fn u64(&mut self) -> u64 {
        let v = self.b.get(self.p..self.p + 8).map(|s| u64::from_le_bytes(s.try_into().unwrap())).unwrap_or(0);
        self.p += 8;
        v
    }
    /// PTP string: count of UTF-16 units (with the final 0), then the units.
    fn string(&mut self) -> String {
        let n = self.u8() as usize;
        let units: Vec<u16> = (0..n).map(|_| self.u16()).take_while(|&u| u != 0).collect();
        String::from_utf16_lossy(&units)
    }
    fn array16(&mut self) -> Vec<u16> {
        let n = self.u32().min(4096);
        (0..n).map(|_| self.u16()).collect()
    }
    fn array32(&mut self) -> Vec<u32> {
        let n = self.u32().min(1_000_000);
        (0..n).map(|_| self.u32()).collect()
    }
}

fn put_string(out: &mut Vec<u8>, s: &str) {
    let units: Vec<u16> = s.encode_utf16().chain(core::iter::once(0)).take(255).collect();
    if s.is_empty() {
        out.push(0);
        return;
    }
    out.push(units.len() as u8);
    for u in units {
        out.extend_from_slice(&u.to_le_bytes());
    }
}

/// A container: length, type (1 command, 2 data, 3 response), code, transaction, payload.
fn container(kind: u16, code: u16, tid: u32, payload: &[u8]) -> Vec<u8> {
    let mut c = Vec::with_capacity(12 + payload.len());
    c.extend_from_slice(&(12 + payload.len() as u32).to_le_bytes());
    c.extend_from_slice(&kind.to_le_bytes());
    c.extend_from_slice(&code.to_le_bytes());
    c.extend_from_slice(&tid.to_le_bytes());
    c.extend_from_slice(payload);
    c
}

fn params(p: &[u32]) -> Vec<u8> {
    p.iter().flat_map(|v| v.to_le_bytes()).collect()
}

/// What an object is.
struct ObjectInfo {
    storage: u32,
    format: u16,
    size: u32,
    parent: u32,
    name: String,
}

fn parse_object_info(d: &[u8]) -> ObjectInfo {
    let mut r = Rd { b: d, p: 0 };
    let storage = r.u32();
    let format = r.u16();
    let _protection = r.u16();
    let size = r.u32();
    let _thumb_format = r.u16();
    let _thumb_size = r.u32();
    for _ in 0..5 {
        r.u32(); // thumb w/h, image w/h, bit depth
    }
    let parent = r.u32();
    let _assoc_type = r.u16();
    let _assoc_desc = r.u32();
    let _seq = r.u32();
    let name = r.string();
    ObjectInfo { storage, format, size, parent, name }
}

// ---- the protocol ----------------------------------------------------------------------------

impl Phone {
    /// A phone on USB whose bulk endpoints are already configured.
    pub fn usb(id: u32, ctrl: usize, slot: u8, ep_in: u8, ep_out: u8, product: &str, vid: u16) -> Phone {
        let link = Link::Usb { ctrl, slot, din: (ep_in & 0xF) * 2 + 1, dout: (ep_out & 0xF) * 2, buf: Dma::new(1024 * 1024, 4096) };
        let kind = if vid == 0x05AC { PhoneKind::IPhone } else { PhoneKind::Android };
        Phone::with_link(id, link, product, kind)
    }

    /// The pretend phone for testing ("testphone" boot option).
    pub fn test(id: u32) -> Phone {
        Phone::with_link(id, Link::Test(TestPhone::new()), "Test phone", PhoneKind::Android)
    }

    fn with_link(id: u32, link: Link, product: &str, kind: PhoneKind) -> Phone {
        Phone {
            id,
            name: String::from(if product.is_empty() { "Phone" } else { product }),
            serial: String::new(),
            kind,
            storages: Vec::new(),
            link,
            tid: 0,
            session: false,
            ops: Vec::new(),
            sizes: BTreeMap::new(),
            problem: None,
        }
    }

    pub fn ctrl_slot(&self) -> Option<(usize, u8)> {
        match &self.link {
            Link::Usb { ctrl, slot, .. } => Some((*ctrl, *slot)),
            Link::Test(_) => None,
        }
    }

    fn send(&mut self, hcs: &mut [Xhci], data: &[u8]) -> Result<(), &'static str> {
        match &mut self.link {
            Link::Usb { ctrl, slot, dout, buf, .. } => {
                let hc = &mut hcs[*ctrl];
                for chunk in data.chunks(buf.len()) {
                    buf.bytes_mut()[..chunk.len()].copy_from_slice(chunk);
                    hc.bulk(*slot, *dout, buf.phys(), chunk.len(), 10_000).map_err(|e| link_err(e))?;
                }
                // A transfer that's a whole number of packets ends with an empty packet.
                if data.len() % 512 == 0 && !data.is_empty() {
                    let _ = hc.bulk(*slot, *dout, buf.phys(), 0, 2_000);
                }
                Ok(())
            }
            Link::Test(t) => {
                t.receive(data);
                Ok(())
            }
        }
    }

    /// One container from the phone.
    fn recv_container(&mut self, hcs: &mut [Xhci]) -> Result<Vec<u8>, &'static str> {
        match &mut self.link {
            Link::Usb { ctrl, slot, din, buf, .. } => {
                let hc = &mut hcs[*ctrl];
                let mut got: Vec<u8> = Vec::new();
                let mut want = usize::MAX;
                let mut empties = 0;
                while got.len() < want {
                    let n = hc.bulk(*slot, *din, buf.phys(), buf.len(), 30_000).map_err(|e| link_err(e))?;
                    if n == 0 {
                        // Stray empty packet (end of the previous transfer).
                        empties += 1;
                        if empties > 3 {
                            return Err("the phone stopped answering");
                        }
                        continue;
                    }
                    got.extend_from_slice(&buf.bytes()[..n]);
                    if want == usize::MAX && got.len() >= 4 {
                        want = u32::from_le_bytes(got[..4].try_into().unwrap()) as usize;
                        if want < 12 {
                            return Err("the phone sent something Solon doesn't understand");
                        }
                        if want == 0xFFFF_FFFF {
                            want = usize::MAX - 1; // unknown length: read until a short packet
                        }
                    }
                    if n < buf.len() && want == usize::MAX - 1 {
                        break;
                    }
                }
                Ok(got)
            }
            Link::Test(t) => t.reply().ok_or("the phone didn't answer"),
        }
    }

    /// One operation: returns (response code, response parameters, data from the phone).
    fn transact(&mut self, hcs: &mut [Xhci], code: u16, p: &[u32]) -> Result<(u16, Vec<u32>, Vec<u8>), &'static str> {
        let tid = self.tid;
        self.tid = self.tid.wrapping_add(1);
        self.send(hcs, &container(1, code, tid, &params(p)))?;
        let mut data = Vec::new();
        loop {
            let c = self.recv_container(hcs)?;
            if c.len() < 12 {
                return Err("the phone sent a short reply");
            }
            let kind = u16::from_le_bytes([c[4], c[5]]);
            match kind {
                2 => data = c[12..].to_vec(),
                3 => {
                    let resp = u16::from_le_bytes([c[6], c[7]]);
                    let rp = c[12..].chunks(4).filter(|x| x.len() == 4).map(|x| u32::from_le_bytes(x.try_into().unwrap())).collect();
                    return Ok((resp, rp, data));
                }
                _ => {} // events on the bulk pipe are ignored
            }
        }
    }

    fn op(&mut self, hcs: &mut [Xhci], code: u16, p: &[u32]) -> Result<Vec<u8>, &'static str> {
        let (resp, _, data) = self.transact(hcs, code, p)?;
        match resp {
            OK => Ok(data),
            0x2019 => Err("the phone is busy"),
            0x200F | 0x2004 => Err("the phone isn't sharing files (unlock it and allow access)"),
            0x2009 => Err("that file isn't on the phone any more"),
            0x2013 => Err("the phone's storage isn't available"),
            _ => {
                kprintln!("phone: operation {:#06x} failed with {:#06x}", code, resp);
                Err("the phone refused")
            }
        }
    }

    /// Who the phone is, open a session and find its storage. Called once when it's plugged in
    /// (and again by `retry` when it had nothing to share).
    pub fn start(&mut self, hcs: &mut [Xhci]) {
        if self.ops.is_empty() {
            match self.op(hcs, GET_DEVICE_INFO, &[]) {
                Ok(d) => {
                    let mut r = Rd { b: &d, p: 0 };
                    let _std = r.u16();
                    let _vendor_ext = r.u32();
                    let _ver = r.u16();
                    let ext = r.string();
                    let _mode = r.u16();
                    self.ops = r.array16();
                    let _events = r.array16();
                    let _props = r.array16();
                    let _capture = r.array16();
                    let _formats = r.array16();
                    let make = r.string();
                    let model = r.string();
                    let _version = r.string();
                    self.serial = r.string();
                    if !model.is_empty() {
                        self.name = if make.is_empty() || model.to_ascii_lowercase().contains(&make.to_ascii_lowercase()) || make == "Apple Inc." { model } else { format!("{} {}", make, model) };
                    }
                    if make.starts_with("Apple") {
                        self.kind = PhoneKind::IPhone;
                    } else if !ext.contains("microsoft") && !ext.contains("android") && self.kind == PhoneKind::Android && !self.supports(GET_PARTIAL_OBJECT_64) {
                        self.kind = PhoneKind::Camera;
                    }
                    kprintln!("phone: \"{}\" ({:?}), {} operations, extensions \"{}\"", self.name, self.kind, self.ops.len(), ext);
                }
                Err(e) => {
                    kprintln!("phone: device info failed: {}", e);
                    self.problem = Some("The phone didn't answer. Unplug it and plug it in again.");
                    return;
                }
            }
        }
        if !self.session {
            self.tid = 0;
            match self.transact(hcs, OPEN_SESSION, &[1]) {
                Ok((OK, _, _)) | Ok((SESSION_ALREADY_OPEN, _, _)) => self.session = true,
                Ok((r, _, _)) => {
                    kprintln!("phone: open session refused ({:#06x})", r);
                    self.problem = Some(self.locked_hint());
                    return;
                }
                Err(e) => {
                    kprintln!("phone: open session failed: {}", e);
                    self.problem = Some(self.locked_hint());
                    return;
                }
            }
        }
        self.storages.clear();
        let ids = match self.op(hcs, GET_STORAGE_IDS, &[]) {
            Ok(d) => Rd { b: &d, p: 0 }.array32(),
            Err(e) => {
                kprintln!("phone: storage list failed: {}", e);
                Vec::new()
            }
        };
        for id in ids.into_iter().filter(|id| id & 0xFFFF != 0) {
            let Ok(d) = self.op(hcs, GET_STORAGE_INFO, &[id]) else { continue };
            let mut r = Rd { b: &d, p: 0 };
            let _type = r.u16();
            let _fs = r.u16();
            let _access = r.u16();
            let capacity = r.u64();
            let free = r.u64();
            let _free_images = r.u32();
            let desc = r.string();
            let label = r.string();
            let label = if !desc.is_empty() { desc } else if !label.is_empty() { label } else { String::from("Phone storage") };
            kprintln!("phone:   storage {:#010x} \"{}\" {} MB, {} MB free", id, label, capacity >> 20, free >> 20);
            self.storages.push(PhoneStorage { id, label, capacity, free });
        }
        self.problem = if self.storages.is_empty() { Some(self.locked_hint()) } else { None };
    }

    fn locked_hint(&self) -> &'static str {
        match self.kind {
            PhoneKind::IPhone => "Unlock the iPhone and tap Trust (or Allow) to share photos.",
            PhoneKind::Android => "Unlock the phone, then choose \"File transfer\" in its USB notification.",
            PhoneKind::Camera => "Turn the camera on (some need a \"PC connection\" or \"MTP\" setting).",
        }
    }

    /// Ask again (after the user unlocked the phone or allowed access).
    pub fn retry(&mut self, hcs: &mut [Xhci]) -> bool {
        let before = self.storages.len();
        self.start(hcs);
        self.storages.len() != before
    }

    fn supports(&self, op: u16) -> bool {
        self.ops.contains(&op)
    }

    /// The top of storage `vol`.
    pub fn root(&self, vol: usize) -> Option<DirEntry> {
        self.storages.get(vol).map(|_| DirEntry::dir(String::new(), ROOT))
    }

    /// Files and folders in a folder (`first_cluster` holds the phone's object handle).
    pub fn list(&mut self, hcs: &mut [Xhci], vol: usize, dir: &DirEntry) -> Result<Vec<DirEntry>, FsError> {
        let storage = self.storages.get(vol).ok_or(FsError::Io)?.id;
        let handles = match self.op(hcs, GET_OBJECT_HANDLES, &[storage, 0, dir.first_cluster]) {
            Ok(d) => Rd { b: &d, p: 0 }.array32(),
            Err(e) => {
                kprintln!("phone: listing failed: {}", e);
                return Err(FsError::Io);
            }
        };
        let mut out = Vec::new();
        for h in handles {
            let Ok(d) = self.op(hcs, GET_OBJECT_INFO, &[h]) else { continue };
            let info = parse_object_info(&d);
            if info.storage != storage || info.name.is_empty() {
                continue;
            }
            // Cameras without folders list everything at the top.
            let _ = info.parent;
            let mut size = info.size as u64;
            if info.size == u32::MAX && self.supports(GET_OBJECT_PROP_VALUE) {
                if let Ok(v) = self.op(hcs, GET_OBJECT_PROP_VALUE, &[h, PROP_OBJECT_SIZE]) {
                    size = Rd { b: &v, p: 0 }.u64();
                }
            }
            let is_dir = info.format == FORMAT_FOLDER;
            self.sizes.insert(h, size);
            out.push(DirEntry { name: info.name, is_dir, size: if is_dir { 0 } else { size }, first_cluster: h, contiguous: false, loc: Vec::new() });
        }
        out.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| crate::fs::natural_cmp(&a.name, &b.name)));
        Ok(out)
    }

    /// A whole file.
    pub fn read_file(&mut self, hcs: &mut [Xhci], file: &DirEntry) -> Result<Vec<u8>, FsError> {
        if file.size > PIECE as u64 && (self.supports(GET_PARTIAL_OBJECT) || self.supports(GET_PARTIAL_OBJECT_64)) {
            let mut out = Vec::with_capacity(file.size as usize);
            let mut off = 0u64;
            while off < file.size {
                let part = self.read_part(hcs, file, off, PIECE)?;
                if part.is_empty() {
                    return Err(FsError::Io);
                }
                off += part.len() as u64;
                out.extend_from_slice(&part);
            }
            return Ok(out);
        }
        self.op(hcs, GET_OBJECT, &[file.first_cluster]).map_err(|e| {
            kprintln!("phone: reading \"{}\" failed: {}", file.name, e);
            FsError::Io
        })
    }

    /// Up to `len` bytes of a file starting at `offset` (for big files, a piece at a time).
    pub fn read_part(&mut self, hcs: &mut [Xhci], file: &DirEntry, offset: u64, len: usize) -> Result<Vec<u8>, FsError> {
        let len = len.min((file.size - offset.min(file.size)) as usize);
        let r = if self.supports(GET_PARTIAL_OBJECT_64) {
            self.op(hcs, GET_PARTIAL_OBJECT_64, &[file.first_cluster, offset as u32, (offset >> 32) as u32, len as u32])
        } else if self.supports(GET_PARTIAL_OBJECT) && offset + len as u64 <= u32::MAX as u64 {
            self.op(hcs, GET_PARTIAL_OBJECT, &[file.first_cluster, offset as u32, len as u32])
        } else if offset == 0 {
            self.op(hcs, GET_OBJECT, &[file.first_cluster])
        } else {
            Err("the phone can't send part of a file")
        };
        r.map_err(|e| {
            kprintln!("phone: reading \"{}\" at {} failed: {}", file.name, offset, e);
            FsError::Io
        })
    }
}

fn link_err(e: UsbError) -> &'static str {
    match e {
        UsbError::NoDevice => "the phone was unplugged",
        UsbError::Timeout => "the phone didn't answer",
        _ => "lost contact with the phone",
    }
}

// ---- the pretend phone -------------------------------------------------------------------------

struct TestObject {
    parent: u32,
    name: String,
    folder: bool,
    data: Vec<u8>,
}

/// Answers PTP/MTP requests like an Android phone with a few photos, a video and a document.
pub struct TestPhone {
    objects: BTreeMap<u32, TestObject>,
    replies: alloc::collections::VecDeque<Vec<u8>>,
    session: bool,
}

const TEST_STORAGE: u32 = 0x0001_0001;

impl TestPhone {
    fn new() -> TestPhone {
        let mut objects = BTreeMap::new();
        let mut add = |h: u32, parent: u32, name: &str, folder: bool, data: Vec<u8>| {
            objects.insert(h, TestObject { parent, name: String::from(name), folder, data });
        };
        add(1, ROOT, "DCIM", true, Vec::new());
        add(2, 1, "Camera", true, Vec::new());
        add(3, 2, "PXL_20261120_101500.jpg", false, include_bytes!("../samples/sunset.jpg").to_vec());
        add(4, 2, "PXL_20261120_101622.jpg", false, include_bytes!("../samples/forest-lake.jpg").to_vec());
        add(5, 2, "PXL_20261121_093000.jpg", false, include_bytes!("../samples/portrait.jpg").to_vec());
        // A "video" bigger than one piece, filled with a pattern that can be checked.
        let video: Vec<u8> = (0..20 * 1024 * 1024 + 12345u32).map(|i| (i.wrapping_mul(2_654_435_761) >> 24) as u8).collect();
        add(6, 2, "PXL_20261121_093100.mp4", false, video);
        add(7, ROOT, "Documents", true, Vec::new());
        add(8, 7, "shopping.csv", false, Vec::from(&b"Item,Price\r\nMilk,3.49\r\nBread,2.99\r\n"[..]));
        add(9, ROOT, "Download", true, Vec::new());
        TestPhone { objects, replies: Default::default(), session: false }
    }

    fn data(&mut self, code: u16, tid: u32, payload: Vec<u8>) {
        self.replies.push_back(container(2, code, tid, &payload));
    }
    fn resp(&mut self, code: u16, tid: u32, p: &[u32]) {
        self.replies.push_back(container(3, code, tid, &params(p)));
    }

    /// A command container from Solon.
    fn receive(&mut self, c: &[u8]) {
        if c.len() < 12 || u16::from_le_bytes([c[4], c[5]]) != 1 {
            return;
        }
        let code = u16::from_le_bytes([c[6], c[7]]);
        let tid = u32::from_le_bytes(c[8..12].try_into().unwrap());
        let p: Vec<u32> = c[12..].chunks(4).filter(|x| x.len() == 4).map(|x| u32::from_le_bytes(x.try_into().unwrap())).collect();
        let arg = |i: usize| p.get(i).copied().unwrap_or(0);
        match code {
            GET_DEVICE_INFO => {
                let mut d = Vec::new();
                d.extend_from_slice(&100u16.to_le_bytes());
                d.extend_from_slice(&6u32.to_le_bytes());
                d.extend_from_slice(&100u16.to_le_bytes());
                put_string(&mut d, "microsoft.com: 1.0; android.com: 1.0;");
                d.extend_from_slice(&0u16.to_le_bytes());
                let ops = [GET_DEVICE_INFO, OPEN_SESSION, CLOSE_SESSION, GET_STORAGE_IDS, GET_STORAGE_INFO, GET_OBJECT_HANDLES, GET_OBJECT_INFO, GET_OBJECT, GET_PARTIAL_OBJECT, GET_PARTIAL_OBJECT_64, GET_OBJECT_PROP_VALUE];
                d.extend_from_slice(&(ops.len() as u32).to_le_bytes());
                for o in ops {
                    d.extend_from_slice(&o.to_le_bytes());
                }
                for _ in 0..4 {
                    d.extend_from_slice(&0u32.to_le_bytes()); // events, props, capture, formats
                }
                put_string(&mut d, "Google");
                put_string(&mut d, "Pixel Test");
                put_string(&mut d, "1.0");
                put_string(&mut d, "TEST0001");
                self.data(code, tid, d);
                self.resp(OK, tid, &[]);
            }
            OPEN_SESSION => {
                let r = if self.session { SESSION_ALREADY_OPEN } else { OK };
                self.session = true;
                self.resp(r, tid, &[]);
            }
            CLOSE_SESSION => {
                self.session = false;
                self.resp(OK, tid, &[]);
            }
            _ if !self.session => self.resp(0x2003, tid, &[]), // session not open
            GET_STORAGE_IDS => {
                self.data(code, tid, params(&[1, TEST_STORAGE]));
                self.resp(OK, tid, &[]);
            }
            GET_STORAGE_INFO => {
                let mut d = Vec::new();
                d.extend_from_slice(&3u16.to_le_bytes());
                d.extend_from_slice(&2u16.to_le_bytes());
                d.extend_from_slice(&0u16.to_le_bytes());
                d.extend_from_slice(&(128u64 << 30).to_le_bytes());
                d.extend_from_slice(&(61u64 << 30).to_le_bytes());
                d.extend_from_slice(&0u32.to_le_bytes());
                put_string(&mut d, "Internal shared storage");
                put_string(&mut d, "");
                self.data(code, tid, d);
                self.resp(OK, tid, &[]);
            }
            GET_OBJECT_HANDLES => {
                let parent = arg(2);
                let hs: Vec<u32> = self.objects.iter().filter(|(_, o)| o.parent == parent || parent == 0).map(|(h, _)| *h).collect();
                let mut d = (hs.len() as u32).to_le_bytes().to_vec();
                d.extend(hs.iter().flat_map(|h| h.to_le_bytes()));
                self.data(code, tid, d);
                self.resp(OK, tid, &[]);
            }
            GET_OBJECT_INFO => match self.objects.get(&arg(0)) {
                Some(o) => {
                    let mut d = Vec::new();
                    d.extend_from_slice(&TEST_STORAGE.to_le_bytes());
                    d.extend_from_slice(&(if o.folder { FORMAT_FOLDER } else { 0x3000 }).to_le_bytes());
                    d.extend_from_slice(&0u16.to_le_bytes());
                    d.extend_from_slice(&(o.data.len() as u32).to_le_bytes());
                    d.extend_from_slice(&0u16.to_le_bytes());
                    d.extend_from_slice(&[0u8; 4 * 6]);
                    d.extend_from_slice(&o.parent.to_le_bytes());
                    d.extend_from_slice(&(if o.folder { 1u16 } else { 0 }).to_le_bytes());
                    d.extend_from_slice(&[0u8; 8]);
                    let name = o.name.clone();
                    put_string(&mut d, &name);
                    put_string(&mut d, "20261120T101500");
                    put_string(&mut d, "20261120T101500");
                    put_string(&mut d, "");
                    self.data(code, tid, d);
                    self.resp(OK, tid, &[]);
                }
                None => self.resp(0x2009, tid, &[]),
            },
            GET_OBJECT => match self.objects.get(&arg(0)).map(|o| o.data.clone()) {
                Some(d) => {
                    self.data(code, tid, d);
                    self.resp(OK, tid, &[]);
                }
                None => self.resp(0x2009, tid, &[]),
            },
            GET_PARTIAL_OBJECT | GET_PARTIAL_OBJECT_64 => {
                let (off, len) = if code == GET_PARTIAL_OBJECT { (arg(1) as u64, arg(2)) } else { (arg(1) as u64 | (arg(2) as u64) << 32, arg(3)) };
                match self.objects.get(&arg(0)) {
                    Some(o) => {
                        let s = (off as usize).min(o.data.len());
                        let e = (s + len as usize).min(o.data.len());
                        let d = o.data[s..e].to_vec();
                        let n = d.len() as u32;
                        self.data(code, tid, d);
                        self.resp(OK, tid, &[n]);
                    }
                    None => self.resp(0x2009, tid, &[]),
                }
            }
            GET_OBJECT_PROP_VALUE => match self.objects.get(&arg(0)) {
                Some(o) if arg(1) == PROP_OBJECT_SIZE => {
                    let n = o.data.len() as u64;
                    self.data(code, tid, n.to_le_bytes().to_vec());
                    self.resp(OK, tid, &[]);
                }
                _ => self.resp(0x200A, tid, &[]),
            },
            _ => self.resp(0x2005, tid, &[]), // operation not supported
        }
    }

    fn reply(&mut self) -> Option<Vec<u8>> {
        self.replies.pop_front()
    }
}

/// Phones show photos, videos and documents; this tells videos apart.
pub fn is_video(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".mp4", ".mov", ".m4v", ".3gp", ".avi", ".mkv", ".mts", ".webm"].iter().any(|e| n.ends_with(e))
}

/// Photos in formats Solon can't show yet but keeps (iPhone HEIC, RAW, GIF).
pub fn is_other_photo(name: &str) -> bool {
    let n = name.to_ascii_lowercase();
    [".heic", ".heif", ".dng", ".gif", ".webp"].iter().any(|e| n.ends_with(e))
}

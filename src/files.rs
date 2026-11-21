//! Files app: browse Solon storage, the Recycle bin and USB drives; organize Solon storage
//! (new folder, rename, move, delete to the Recycle bin); copy between USB drives and
//! Solon storage. USB drives are only ever added to, never changed.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::desktop::App;
use crate::disk::human;
use crate::fs::{DirEntry, FsError};
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::icons;
use crate::import::is_photo;
use crate::library::{Storage, BIN};
use crate::photos::Source;
use crate::textfield::{TextEvent, TextField};
use crate::usb::Usb;

const SIDE_W: i32 = 210;
const HEADER_H: i32 = 48;
const TOOL_H: i32 = 48;
const ROW_H: i32 = 44;
const DOUBLE_CLICK_TICKS: u64 = 50;

const INK: u32 = rgb(40, 44, 52);
const DIM: u32 = rgb(110, 116, 126);
const ACCENT: u32 = rgb(46, 134, 222);
const BTN: u32 = rgb(230, 233, 238);
const SIDE_BG: u32 = rgb(240, 242, 246);

/// Items at the top of Solon storage that Files doesn't show.
const HIDDEN: [&str; 5] = [BIN, "Originals", "Settings.txt", "RecycleBin.txt", "PhoneImports.txt"];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Place {
    Library,
    Bin,
    Usb { drive: u32, vol: usize },
}

/// Copy `item` (from `from`) into folder `dest` on `to`.
pub struct CopySpec {
    pub from: Place,
    pub item: DirEntry,
    pub to: Place,
    pub dest: DirEntry,
}

pub enum FilesAction {
    None,
    Redraw,
    ShowPhotos { origin: String, items: Vec<(String, Source)>, open_at: Option<usize> },
    /// Open a spreadsheet in Documents.
    OpenDocument(crate::documents::DocSrc),
    Copy(CopySpec),
    /// Solon storage changed (Photos should refresh).
    Changed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum NameFor {
    NewFolder,
    Rename,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum PickFor {
    Move,
    Copy,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Confirm {
    Recycle,
    DeleteForever,
    EmptyBin,
}

struct Picker {
    purpose: PickFor,
    place: Place,
    path: Vec<DirEntry>,
    folders: Vec<DirEntry>,
    selected: Option<usize>,
}

enum Dialog {
    None,
    Name(NameFor, TextField),
    Pick(Picker),
    Confirm(Confirm),
    Message(String),
}

pub struct Files {
    place: Place,
    path: Vec<DirEntry>,
    entries: Vec<DirEntry>,
    /// For the Recycle bin: where each entry came from.
    origins: Vec<String>,
    selected: Option<usize>,
    scroll: i32,
    last_click: Option<(usize, u64)>,
    dialog: Dialog,
}

fn button(c: &mut Canvas, b: Rect, label: &str, primary: bool, enabled: bool) {
    let (bg, fg) = match (primary, enabled) {
        (_, false) => (rgb(244, 245, 247), rgb(190, 194, 200)),
        (true, true) => (ACCENT, rgb(255, 255, 255)),
        (false, true) => (BTN, INK),
    };
    c.round_rect(b.x, b.y, b.w, b.h, 8, bg);
    c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, fg);
}

/// USB drives and phones Files can show (every drive except Solon's own stick).
fn usb_places(usb: &Usb) -> Vec<(Place, String)> {
    usb.sources().into_iter().map(|s| (Place::Usb { drive: s.drive, vol: s.vol }, s.label)).collect()
}

/// Where copies can go: USB drives (phones are only read).
fn copy_targets(usb: &Usb) -> Vec<(Place, String)> {
    usb.sources().into_iter().filter(|s| !s.phone).map(|s| (Place::Usb { drive: s.drive, vol: s.vol }, s.label)).collect()
}

fn list_place(storage: &mut Storage, usb: &mut Usb, place: Place, dir: &DirEntry, top: bool) -> Result<Vec<DirEntry>, &'static str> {
    match place {
        Place::Library => {
            let mut v = storage.list(usb, dir).map_err(|e| e.message())?;
            if top {
                v.retain(|e| !HIDDEN.iter().any(|h| h.eq_ignore_ascii_case(&e.name)));
            }
            Ok(v)
        }
        Place::Bin => Ok(storage.bin_items(usb).into_iter().map(|(e, _)| e).collect()),
        Place::Usb { drive, vol } => usb.list(drive, vol, dir).map_err(|e| e.message()),
    }
}

fn root_of(storage: &mut Storage, usb: &mut Usb, place: Place) -> Option<DirEntry> {
    match place {
        Place::Library | Place::Bin => storage.root(usb).ok(),
        Place::Usb { drive, vol } => usb.source_root(drive, vol),
    }
}

impl Files {
    pub fn new() -> Files {
        Files { place: Place::Library, path: Vec::new(), entries: Vec::new(), origins: Vec::new(), selected: None, scroll: 0, last_click: None, dialog: Dialog::None }
    }

    fn top(&self) -> bool {
        self.path.len() <= 1
    }

    /// The folder path below the top of the current place, e.g. "Photos/Trip".
    fn path_text(&self) -> String {
        let names: Vec<&str> = self.path.iter().skip(1).map(|d| d.name.as_str()).collect();
        names.join("/")
    }

    fn place_name(&self, usb: &Usb) -> String {
        match self.place {
            Place::Library => String::from("Solon storage"),
            Place::Bin => String::from("Recycle bin"),
            Place::Usb { .. } => usb_places(usb).into_iter().find(|(p, _)| *p == self.place).map(|(_, n)| n).unwrap_or_else(|| String::from("USB drive")),
        }
    }

    /// Re-read the current folder (after changes, plug/unplug, opening the window).
    pub fn refresh(&mut self, storage: &mut Storage, usb: &mut Usb) {
        if storage.library.is_none() && self.place == Place::Bin {
            self.place = Place::Library;
            self.path.clear();
        }
        if let Place::Usb { drive, vol } = self.place {
            let present = usb.sources().iter().any(|s| s.drive == drive && s.vol == vol);
            if !present {
                self.place = Place::Library;
                self.path.clear();
            }
        }
        if self.path.is_empty() {
            match root_of(storage, usb, self.place) {
                Some(r) => self.path.push(r),
                None => {
                    self.entries.clear();
                    return;
                }
            }
        }
        let dir = self.path.last().unwrap().clone();
        let top = self.top();
        self.entries = list_place(storage, usb, self.place, &dir, top).unwrap_or_default();
        self.origins = if self.place == Place::Bin { storage.bin_items(usb).into_iter().map(|(_, f)| f).collect() } else { Vec::new() };
        self.selected = self.selected.filter(|&i| i < self.entries.len());
    }

    fn go(&mut self, place: Place, storage: &mut Storage, usb: &mut Usb) {
        self.place = place;
        self.path.clear();
        self.selected = None;
        self.scroll = 0;
        self.dialog = Dialog::None;
        self.refresh(storage, usb);
    }

    // ---- layout -----------------------------------------------------------------

    fn side_item(r: Rect, i: usize) -> Rect {
        let extra = if i >= 2 { 30 } else { 0 }; // gap + "USB drives" heading
        Rect::new(r.x + 10, r.y + 16 + i as i32 * 44 + extra, SIDE_W - 20, 38)
    }
    fn main(r: Rect) -> Rect {
        Rect::new(r.x + SIDE_W, r.y, r.w - SIDE_W, r.h)
    }
    fn up_btn(r: Rect) -> Rect {
        let m = Self::main(r);
        Rect::new(m.x + 12, m.y + 8, 64, 32)
    }
    fn tool(r: Rect, i: usize) -> Rect {
        let m = Self::main(r);
        Rect::new(m.x + 12 + i as i32 * 138, m.y + HEADER_H + 6, 130, 34)
    }
    fn list_area(r: Rect) -> Rect {
        let m = Self::main(r);
        Rect::new(m.x, m.y + HEADER_H + TOOL_H, m.w, m.h - HEADER_H - TOOL_H)
    }
    fn row(&self, r: Rect, i: usize) -> Rect {
        let a = Self::list_area(r);
        Rect::new(a.x + 10, a.y + 6 + i as i32 * ROW_H - self.scroll, a.w - 20, ROW_H - 4)
    }
    fn dialog(r: Rect, tall: bool) -> Rect {
        let w = 560.min(r.w - 40);
        let h = if tall { (r.h - 60).min(460) } else { 210 };
        Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h)
    }
    fn dlg_ok(d: Rect) -> Rect {
        Rect::new(d.x + d.w - 24 - 150, d.y + d.h - 58, 150, 38)
    }
    fn dlg_cancel(d: Rect) -> Rect {
        Rect::new(d.x + d.w - 24 - 150 - 12 - 110, d.y + d.h - 58, 110, 38)
    }

    /// Toolbar buttons: (label, enabled) for the current place.
    fn tools(&self, usb: &Usb) -> Vec<(&'static str, bool)> {
        let sel = self.selected.is_some();
        match self.place {
            Place::Library => Vec::from([
                ("New folder", true),
                ("Rename", sel),
                ("Move", sel),
                ("Copy to USB", sel && !copy_targets(usb).is_empty()),
                ("Delete", sel),
            ]),
            Place::Bin => Vec::from([("Restore", sel), ("Delete forever", sel), ("Empty bin", !self.entries.is_empty())]),
            Place::Usb { .. } => Vec::from([("Copy to Solon", sel)]),
        }
    }

    // ---- drawing ----------------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas, r: Rect, storage: &Storage, usb: &Usb) {
        // Sidebar
        c.fill_rect(r.x, r.y, SIDE_W, r.h, SIDE_BG);
        let mut items: Vec<(Place, String, bool)> = Vec::new();
        let has_lib = storage.library.is_some();
        items.push((Place::Library, String::from("Solon storage"), has_lib));
        items.push((Place::Bin, String::from("Recycle bin"), has_lib));
        for (p, n) in usb_places(usb) {
            items.push((p, n, true));
        }
        for (i, (p, name, enabled)) in items.iter().enumerate() {
            let b = Self::side_item(r, i);
            if *p == self.place {
                c.round_rect(b.x, b.y, b.w, b.h, 8, rgb(214, 228, 246));
            }
            let (gx, gy) = (b.x + 10, b.y + 9);
            match p {
                Place::Library => {
                    c.round_rect(gx, gy, 20, 20, 5, rgb(39, 174, 96));
                    c.fill_rect(gx + 4, gy + 6, 12, 3, rgb(255, 255, 255));
                    c.fill_rect(gx + 4, gy + 11, 12, 3, rgb(255, 255, 255));
                }
                Place::Bin => {
                    c.fill_rect(gx + 3, gy + 2, 14, 3, DIM);
                    c.round_rect(gx + 4, gy + 6, 12, 14, 2, DIM);
                }
                Place::Usb { drive, .. } if usb.is_phone(*drive) => {
                    c.round_rect(gx + 4, gy, 13, 21, 3, rgb(60, 64, 72));
                    c.fill_rect(gx + 6, gy + 3, 9, 13, rgb(150, 190, 240));
                }
                Place::Usb { .. } => {
                    c.round_rect(gx + 5, gy + 6, 10, 14, 2, icons::tile_color(App::Import));
                    c.fill_rect(gx + 7, gy + 1, 6, 5, icons::tile_color(App::Import));
                }
            }
            let label = crate::photos::ellipsize(name, b.w - 50, Size::Small, true);
            c.text(b.x + 40, b.y + 10, &label, Size::Small, true, if *enabled { INK } else { rgb(170, 174, 180) });
            if i == 1 {
                c.text(r.x + 20, Self::side_item(r, 2).y - 24, "USB drives and phones", Size::Small, false, DIM);
            }
        }
        if items.len() == 2 {
            c.text(r.x + 20, Self::side_item(r, 2).y + 4, "None plugged in", Size::Small, false, rgb(160, 164, 170));
        }

        // Header: Up + where we are
        let m = Self::main(r);
        if !self.top() {
            let u = Self::up_btn(r);
            button(c, u, "Up", false, true);
        }
        let mut crumb = self.place_name(usb);
        let p = self.path_text();
        if !p.is_empty() {
            crumb = format!("{} / {}", crumb, p.replace('/', " / "));
        }
        let x = if self.top() { m.x + 16 } else { m.x + 90 };
        let crumb = crate::photos::ellipsize_left(&crumb, m.w - (x - m.x) - 16, Size::Medium, true);
        c.text(x, m.y + 13, &crumb, Size::Medium, true, INK);
        c.fill_rect(m.x, m.y + HEADER_H - 1, m.w, 1, rgb(228, 231, 236));

        // Toolbar
        if storage.library.is_some() || matches!(self.place, Place::Usb { .. }) {
            for (i, (label, enabled)) in self.tools(usb).iter().enumerate() {
                button(c, Self::tool(r, i), label, false, *enabled);
            }
        }

        // List
        let a = Self::list_area(r);
        if storage.library.is_none() && matches!(self.place, Place::Library | Place::Bin) {
            c.text_centered(a.x + a.w / 2, a.y + 60, "Solon storage isn't set up yet.", Size::Medium, true, INK);
            c.text_centered(a.x + a.w / 2, a.y + 92, "Open the Disks app (or Import) to set it up.", Size::Small, false, DIM);
            self.draw_dialog(c, r, usb);
            return;
        }
        if self.entries.is_empty() {
            let msg = if self.place == Place::Bin { "The Recycle bin is empty." } else { "This folder is empty." };
            c.text_centered(a.x + a.w / 2, a.y + 60, msg, Size::Medium, true, DIM);
        }
        c.set_clip(a);
        for (i, e) in self.entries.iter().enumerate() {
            let rr = self.row(r, i);
            if rr.y + rr.h < a.y || rr.y > a.y + a.h {
                continue;
            }
            if self.selected == Some(i) {
                c.round_rect_alpha(rr.x, rr.y, rr.w, rr.h, 8, ACCENT, 40);
            }
            let (gx, gy) = (rr.x + 12, rr.y + 8);
            if e.is_dir {
                c.round_rect(gx, gy + 2, 13, 8, 2, rgb(240, 180, 40));
                c.round_rect(gx, gy + 6, 28, 20, 3, rgb(250, 196, 60));
            } else if is_photo(&e.name) {
                c.round_rect(gx + 2, gy, 24, 26, 4, icons::tile_color(App::Photos));
                c.triangle([(gx + 6, gy + 21), (gx + 12, gy + 12), (gx + 18, gy + 21)], rgb(255, 255, 255));
            } else {
                c.fill_rect(gx + 4, gy, 20, 26, rgb(220, 224, 230));
                for k in 0..3 {
                    c.fill_rect(gx + 8, gy + 8 + k * 5, 12, 2, rgb(180, 186, 196));
                }
            }
            let right = if self.place == Place::Bin {
                let from = self.origins.get(i).map(|s| s.as_str()).unwrap_or("");
                format!("from {}", if from.is_empty() { "Solon storage" } else { from })
            } else if e.is_dir {
                String::from("Folder")
            } else {
                human(e.size)
            };
            let rw = text_width(&right, Size::Small, false);
            let name = crate::photos::ellipsize(&e.name, rr.w - 80 - rw, Size::Small, e.is_dir);
            c.text(rr.x + 54, rr.y + 12, &name, Size::Small, e.is_dir, INK);
            c.text(rr.x + rr.w - 14 - rw, rr.y + 12, &right, Size::Small, false, DIM);
        }
        c.set_clip(r);
        self.draw_dialog(c, r, usb);
    }

    fn draw_dialog(&self, c: &mut Canvas, r: Rect, usb: &Usb) {
        let (tall, title, body, ok): (bool, String, String, &str) = match &self.dialog {
            Dialog::None => return,
            Dialog::Name(NameFor::NewFolder, _) => (false, String::from("New folder"), String::from("Name for the new folder:"), "Create"),
            Dialog::Name(NameFor::Rename, _) => (false, String::from("Rename"), String::from("New name:"), "Rename"),
            Dialog::Pick(p) => (
                true,
                String::from(if p.purpose == PickFor::Move { "Move to..." } else { "Copy to..." }),
                String::new(),
                if p.purpose == PickFor::Move { "Move here" } else { "Copy here" },
            ),
            Dialog::Confirm(Confirm::Recycle) => (false, String::from("Delete?"), String::from("It goes to the Recycle bin, where you can restore it."), "Delete"),
            Dialog::Confirm(Confirm::DeleteForever) => (false, String::from("Delete forever?"), String::from("This can't be undone."), "Delete forever"),
            Dialog::Confirm(Confirm::EmptyBin) => (false, String::from("Empty the Recycle bin?"), String::from("Everything in it is deleted for good. This can't be undone."), "Empty bin"),
            Dialog::Message(m) => (false, String::from("Files"), m.clone(), "OK"),
        };
        c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 110);
        let d = Self::dialog(r, tall);
        c.round_rect_alpha(d.x + 3, d.y + 6, d.w, d.h, 12, 0, 90);
        c.round_rect(d.x, d.y, d.w, d.h, 12, rgb(255, 255, 255));
        c.text(d.x + 24, d.y + 20, &title, Size::Medium, true, INK);
        if !body.is_empty() {
            c.text(d.x + 24, d.y + 60, &body, Size::Small, false, DIM);
        }
        match &self.dialog {
            Dialog::Name(_, f) => f.draw(c, Rect::new(d.x + 24, d.y + 88, d.w - 48, 38)),
            Dialog::Pick(p) => self.draw_picker(c, d, p, usb),
            _ => {}
        }
        let danger = matches!(self.dialog, Dialog::Confirm(Confirm::DeleteForever | Confirm::EmptyBin));
        if !matches!(self.dialog, Dialog::Message(_)) {
            button(c, Self::dlg_cancel(d), "Cancel", false, true);
        }
        let ob = Self::dlg_ok(d);
        c.round_rect(ob.x, ob.y, ob.w, ob.h, 8, if danger { rgb(210, 70, 60) } else { ACCENT });
        c.text_centered(ob.x + ob.w / 2, ob.y + 10, ok, Size::Small, true, rgb(255, 255, 255));
    }

    fn picker_up(d: Rect) -> Rect {
        Rect::new(d.x + 24, d.y + 56, 64, 30)
    }
    fn picker_row(d: Rect, i: usize) -> Rect {
        Rect::new(d.x + 24, d.y + 96 + i as i32 * 40, d.w - 48, 36)
    }
    fn picker_rows(d: Rect) -> usize {
        ((d.h - 96 - 70) / 40).max(1) as usize
    }

    fn other_drive(d: Rect) -> Rect {
        Rect::new(d.x + d.w - 24 - 130, d.y + 56, 130, 30)
    }

    /// Copying to USB with more than one USB drive plugged in: offer to switch drives.
    fn can_switch(p: &Picker, usb: &Usb) -> bool {
        p.purpose == PickFor::Copy && matches!(p.place, Place::Usb { .. }) && copy_targets(usb).len() > 1
    }

    fn draw_picker(&self, c: &mut Canvas, d: Rect, p: &Picker, usb: &Usb) {
        if p.path.len() > 1 {
            button(c, Self::picker_up(d), "Up", false, true);
        }
        let names: Vec<&str> = p.path.iter().skip(1).map(|e| e.name.as_str()).collect();
        let place_name = match p.place {
            Place::Usb { .. } => usb_places(usb).into_iter().find(|(q, _)| *q == p.place).map(|(_, n)| n).unwrap_or_else(|| String::from("USB drive")),
            _ => String::from("Solon storage"),
        };
        let place = place_name.as_str();
        let switch = Self::can_switch(p, usb);
        if switch {
            button(c, Self::other_drive(d), "Other drive", false, true);
        }
        let cx = if p.path.len() > 1 { d.x + 100 } else { d.x + 24 };
        let crumb = if names.is_empty() { String::from(place) } else { format!("{} / {}", place, names.join(" / ")) };
        let right = if switch { Self::other_drive(d).x - 12 } else { d.x + d.w - 24 };
        let crumb = crate::photos::ellipsize_left(&crumb, right - cx, Size::Small, true);
        c.text(cx, d.y + 62, &crumb, Size::Small, true, INK);
        if p.folders.is_empty() {
            c.text(d.x + 24, d.y + 104, "No folders here. Choose \"here\" to use this one.", Size::Small, false, DIM);
        }
        for (i, f) in p.folders.iter().take(Self::picker_rows(d)).enumerate() {
            let rr = Self::picker_row(d, i);
            if p.selected == Some(i) {
                c.round_rect_alpha(rr.x, rr.y, rr.w, rr.h, 6, ACCENT, 40);
            }
            c.round_rect(rr.x + 8, rr.y + 10, 22, 16, 3, rgb(250, 196, 60));
            let n = crate::photos::ellipsize(&f.name, rr.w - 50, Size::Small, true);
            c.text(rr.x + 40, rr.y + 9, &n, Size::Small, true, INK);
        }
    }

    // ---- actions ----------------------------------------------------------------

    fn selected_entry(&self) -> Option<DirEntry> {
        self.selected.and_then(|i| self.entries.get(i).cloned())
    }

    fn open_picker(&mut self, purpose: PickFor, place: Place, storage: &mut Storage, usb: &mut Usb) {
        let Some(root) = root_of(storage, usb, place) else { return };
        let mut p = Picker { purpose, place, path: Vec::from([root]), folders: Vec::new(), selected: None };
        Self::picker_load(&mut p, storage, usb);
        self.dialog = Dialog::Pick(p);
    }

    fn picker_load(p: &mut Picker, storage: &mut Storage, usb: &mut Usb) {
        let dir = p.path.last().unwrap().clone();
        let top = p.path.len() == 1;
        p.folders = list_place(storage, usb, p.place, &dir, top).unwrap_or_default().into_iter().filter(|e| e.is_dir).collect();
        p.selected = None;
    }

    fn photos_here(&self, open: &DirEntry) -> FilesAction {
        let dir = self.path.last().cloned().unwrap();
        let photos: Vec<DirEntry> = self.entries.iter().filter(|e| !e.is_dir && is_photo(&e.name)).cloned().collect();
        let open_at = photos.iter().position(|e| e.name == open.name);
        let items = photos
            .into_iter()
            .map(|e| {
                let src = match self.place {
                    Place::Usb { drive, vol } => Source::Usb { drive, vol, entry: e.clone() },
                    _ => Source::Library { dir: dir.clone(), entry: e.clone() },
                };
                (e.name.clone(), src)
            })
            .collect();
        let p = self.path_text();
        let origin = if p.is_empty() { self.place_name_static() } else { format!("{} / {}", self.place_name_static(), p.replace('/', " / ")) };
        FilesAction::ShowPhotos { origin, items, open_at }
    }

    fn place_name_static(&self) -> String {
        String::from(match self.place {
            Place::Library => "Solon storage",
            Place::Bin => "Recycle bin",
            Place::Usb { .. } => "USB drive",
        })
    }

    fn activate(&mut self, i: usize, storage: &mut Storage, usb: &mut Usb) -> FilesAction {
        let Some(e) = self.entries.get(i).cloned() else { return FilesAction::None };
        if self.place == Place::Bin {
            return FilesAction::None;
        }
        if e.is_dir {
            self.path.push(e);
            self.selected = None;
            self.scroll = 0;
            self.refresh(storage, usb);
            FilesAction::Redraw
        } else if crate::library::is_document(&e.name) || crate::docapps::opens_any(&e.name) {
            let dir = self.path.last().cloned().unwrap();
            FilesAction::OpenDocument(match self.place {
                Place::Usb { drive, vol } => crate::documents::DocSrc::Usb { drive, vol, entry: e },
                _ => crate::documents::DocSrc::Library { dir, entry: e },
            })
        } else if is_photo(&e.name) {
            self.photos_here(&e)
        } else {
            self.dialog = Dialog::Message(String::from("Solon can't open this kind of file yet."));
            FilesAction::Redraw
        }
    }

    fn tool_action(&mut self, label: &str, storage: &mut Storage, usb: &mut Usb) -> FilesAction {
        let sel = self.selected_entry();
        match (label, sel) {
            ("New folder", _) => self.dialog = Dialog::Name(NameFor::NewFolder, TextField::new("New folder", 80)),
            ("Rename", Some(e)) => self.dialog = Dialog::Name(NameFor::Rename, TextField::new(&e.name, 120)),
            ("Move", Some(_)) => self.open_picker(PickFor::Move, Place::Library, storage, usb),
            ("Copy to USB", Some(_)) => {
                if let Some(p) = copy_targets(usb).first().map(|(p, _)| *p) {
                    self.open_picker(PickFor::Copy, p, storage, usb);
                }
            }
            ("Copy to Solon", Some(_)) => {
                if storage.library.is_none() {
                    self.dialog = Dialog::Message(String::from("Set up Solon storage first (in the Disks app)."));
                } else {
                    self.open_picker(PickFor::Copy, Place::Library, storage, usb);
                }
            }
            ("Delete", Some(_)) => self.dialog = Dialog::Confirm(Confirm::Recycle),
            ("Delete forever", Some(_)) => self.dialog = Dialog::Confirm(Confirm::DeleteForever),
            ("Empty bin", _) => self.dialog = Dialog::Confirm(Confirm::EmptyBin),
            ("Restore", Some(e)) => {
                let from = self.selected.and_then(|i| self.origins.get(i).cloned()).unwrap_or_default();
                let r = storage.restore(usb, &e, &from);
                self.refresh(storage, usb);
                return match r {
                    Ok(name) =>  {
                        kprintln!("files: restored \"{}\"", name);
                        FilesAction::Changed(format!("Restored \"{}\"", name))
                    }
                    Err(err) => {
                        self.dialog = Dialog::Message(String::from(err.message()));
                        FilesAction::Redraw
                    }
                };
            }
            _ => return FilesAction::None,
        }
        FilesAction::Redraw
    }

    /// The OK button of the open dialog.
    fn confirm_dialog(&mut self, storage: &mut Storage, usb: &mut Usb) -> FilesAction {
        let dialog = core::mem::replace(&mut self.dialog, Dialog::None);
        let parent = self.path.last().cloned();
        let sel = self.selected_entry();
        let result: Result<String, &'static str> = match dialog {
            Dialog::None | Dialog::Message(_) => return FilesAction::Redraw,
            Dialog::Name(kind, field) => {
                let name = String::from(field.text.trim());
                let parent = parent.unwrap();
                if name.is_empty() {
                    return FilesAction::Redraw;
                }
                match kind {
                    NameFor::NewFolder => storage.mkdir_in(usb, &parent, &name).map(|_| format!("Created \"{}\"", name)).map_err(|e| e.message()),
                    NameFor::Rename => match sel {
                        Some(e) if e.name != name => storage.rename(usb, &e, &parent, &name).map(|_| format!("Renamed to \"{}\"", name)).map_err(|e| e.message()),
                        _ => return FilesAction::Redraw,
                    },
                }
            }
            Dialog::Confirm(Confirm::Recycle) => match sel {
                Some(e) => {
                    let from = self.path_text();
                    storage.recycle(usb, &e, &from).map(|_| format!("\"{}\" moved to the Recycle bin", e.name)).map_err(|e| e.message())
                }
                None => return FilesAction::Redraw,
            },
            Dialog::Confirm(Confirm::DeleteForever) => match sel {
                Some(e) => storage.delete_forever(usb, &e).map(|_| format!("\"{}\" deleted", e.name)).map_err(|e| e.message()),
                None => return FilesAction::Redraw,
            },
            Dialog::Confirm(Confirm::EmptyBin) => storage.empty_bin(usb).map(|n| format!("Recycle bin emptied ({} item{})", n, if n == 1 { "" } else { "s" })).map_err(|e| e.message()),
            Dialog::Pick(p) => {
                let Some(item) = sel else { return FilesAction::Redraw };
                let dest = match p.selected {
                    Some(i) => p.folders[i].clone(),
                    None => p.path.last().unwrap().clone(),
                };
                match p.purpose {
                    PickFor::Move => {
                        if item.is_dir && item.first_cluster == dest.first_cluster {
                            Err("A folder can't be moved into itself")
                        } else {
                            storage.move_to(usb, &item, &dest).map(|m| format!("Moved \"{}\"", m.name)).map_err(|e| e.message())
                        }
                    }
                    PickFor::Copy => return FilesAction::Copy(CopySpec { from: self.place, item, to: p.place, dest }),
                }
            }
        };
        self.selected = None;
        self.refresh(storage, usb);
        match result {
            Ok(msg) => {
                kprintln!("files: {}", msg);
                FilesAction::Changed(msg)
            }
            Err(e) => {
                kprintln!("files: failed: {}", e);
                self.dialog = Dialog::Message(String::from(e));
                FilesAction::Redraw
            }
        }
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, storage: &mut Storage, usb: &mut Usb) -> FilesAction {
        // Dialogs first.
        if !matches!(self.dialog, Dialog::None) {
            let tall = matches!(self.dialog, Dialog::Pick(_));
            let d = Self::dialog(r, tall);
            if Self::dlg_ok(d).contains(x, y) {
                return self.confirm_dialog(storage, usb);
            }
            if Self::dlg_cancel(d).contains(x, y) && !matches!(self.dialog, Dialog::Message(_)) {
                self.dialog = Dialog::None;
                return FilesAction::Redraw;
            }
            if let Dialog::Pick(p) = &mut self.dialog {
                if Self::can_switch(p, usb) && Self::other_drive(d).contains(x, y) {
                    let places: Vec<Place> = copy_targets(usb).into_iter().map(|(q, _)| q).collect();
                    let k = places.iter().position(|q| *q == p.place).map(|k| (k + 1) % places.len()).unwrap_or(0);
                    let to = places[k];
                    if let Some(root) = root_of(storage, usb, to) {
                        p.place = to;
                        p.path = Vec::from([root]);
                        Self::picker_load(p, storage, usb);
                    }
                    return FilesAction::Redraw;
                }
                if p.path.len() > 1 && Self::picker_up(d).contains(x, y) {
                    p.path.pop();
                    Self::picker_load(p, storage, usb);
                    return FilesAction::Redraw;
                }
                for i in 0..p.folders.len().min(Self::picker_rows(d)) {
                    if Self::picker_row(d, i).contains(x, y) {
                        if p.selected == Some(i) {
                            // Second click opens the folder.
                            let f = p.folders[i].clone();
                            p.path.push(f);
                            Self::picker_load(p, storage, usb);
                        } else {
                            p.selected = Some(i);
                        }
                        return FilesAction::Redraw;
                    }
                }
            }
            return FilesAction::None;
        }

        // Sidebar
        let mut places = Vec::from([Place::Library, Place::Bin]);
        places.extend(usb_places(usb).into_iter().map(|(p, _)| p));
        for (i, p) in places.iter().enumerate() {
            if Self::side_item(r, i).contains(x, y) {
                if (*p == Place::Library || *p == Place::Bin) && storage.library.is_none() {
                    return FilesAction::None;
                }
                self.go(*p, storage, usb);
                return FilesAction::Redraw;
            }
        }
        if !self.top() && Self::up_btn(r).contains(x, y) {
            self.path.pop();
            self.selected = None;
            self.scroll = 0;
            self.refresh(storage, usb);
            return FilesAction::Redraw;
        }
        for (i, (label, enabled)) in self.tools(usb).iter().enumerate() {
            if *enabled && Self::tool(r, i).contains(x, y) {
                return self.tool_action(label, storage, usb);
            }
        }
        if Self::list_area(r).contains(x, y) {
            if let Some(i) = (0..self.entries.len()).find(|&i| self.row(r, i).contains(x, y)) {
                let double = matches!(self.last_click, Some((j, t)) if j == i && now - t <= DOUBLE_CLICK_TICKS);
                self.selected = Some(i);
                self.last_click = Some((i, now));
                if double {
                    self.last_click = None;
                    return match self.activate(i, storage, usb) {
                        FilesAction::None => FilesAction::Redraw,
                        a => a,
                    };
                }
                return FilesAction::Redraw;
            }
            self.selected = None;
            return FilesAction::Redraw;
        }
        FilesAction::None
    }

    pub fn on_wheel(&mut self, delta: i8, r: Rect) -> bool {
        let a = Self::list_area(r);
        let max = (self.entries.len() as i32 * ROW_H + 12 - a.h).max(0);
        let old = self.scroll;
        self.scroll = (self.scroll + delta as i32 * 3 * ROW_H).clamp(0, max);
        old != self.scroll
    }

    /// None = not handled (Esc closes the window when no dialog is open).
    pub fn on_key(&mut self, sc: u8, r: Rect, storage: &mut Storage, usb: &mut Usb) -> Option<FilesAction> {
        if let Dialog::Name(_, field) = &mut self.dialog {
            return Some(match field.on_key(sc) {
                TextEvent::Submit => self.confirm_dialog(storage, usb),
                TextEvent::Cancel => {
                    self.dialog = Dialog::None;
                    FilesAction::Redraw
                }
                TextEvent::Changed => FilesAction::Redraw,
                TextEvent::None => FilesAction::None,
            });
        }
        if sc & 0x80 != 0 {
            return Some(FilesAction::None);
        }
        if !matches!(self.dialog, Dialog::None) {
            return Some(match sc {
                0x01 => {
                    self.dialog = Dialog::None;
                    FilesAction::Redraw
                }
                0x1C => self.confirm_dialog(storage, usb),
                _ => FilesAction::None,
            });
        }
        let n = self.entries.len();
        match sc {
            0x48 | 0x50 if n > 0 => {
                let next = match (self.selected, sc) {
                    (None, _) => 0,
                    (Some(i), 0x48) => i.saturating_sub(1),
                    (Some(i), _) => (i + 1).min(n - 1),
                };
                self.selected = Some(next);
                let rr = self.row(r, next);
                let a = Self::list_area(r);
                if rr.y < a.y {
                    self.scroll -= a.y - rr.y + 6;
                } else if rr.y + rr.h > a.y + a.h {
                    self.scroll += rr.y + rr.h - (a.y + a.h) + 6;
                }
                Some(FilesAction::Redraw)
            }
            0x1C => Some(match self.selected {
                Some(i) => self.activate(i, storage, usb),
                None => FilesAction::None,
            }),
            0x0E if !self.top() => {
                self.path.pop();
                self.selected = None;
                self.refresh(storage, usb);
                Some(FilesAction::Redraw)
            }
            0x53 if self.selected.is_some() && self.place == Place::Library => {
                self.dialog = Dialog::Confirm(Confirm::Recycle); // Delete key
                Some(FilesAction::Redraw)
            }
            0x3C if self.selected.is_some() && self.place == Place::Library => {
                let e = self.selected_entry().unwrap();
                self.dialog = Dialog::Name(NameFor::Rename, TextField::new(&e.name, 120)); // F2
                Some(FilesAction::Redraw)
            }
            0x01 => None,
            _ => Some(FilesAction::None),
        }
    }

    /// USB drives came or went.
    pub fn drives_changed(&mut self, storage: &mut Storage, usb: &mut Usb) {
        self.refresh(storage, usb);
    }

    #[allow(dead_code)]
    pub fn place(&self) -> Place {
        self.place
    }
}

// ---- copying between USB drives and Solon storage ------------------------------------

fn list_any(storage: &mut Storage, usb: &mut Usb, place: Place, dir: &DirEntry) -> Result<Vec<DirEntry>, FsError> {
    match place {
        Place::Library | Place::Bin => storage.list(usb, dir),
        Place::Usb { drive, vol } => usb.list(drive, vol, dir),
    }
}

fn read_any(storage: &mut Storage, usb: &mut Usb, place: Place, file: &DirEntry) -> Result<Vec<u8>, FsError> {
    match place {
        Place::Library | Place::Bin => storage.read_file(usb, file),
        Place::Usb { drive, vol } => usb.read_file(drive, vol, file),
    }
}

/// Add a file (`Some(data)`) or folder (`None`). USB drives are only ever added to.
fn create_any(storage: &mut Storage, usb: &mut Usb, place: Place, dir: &DirEntry, name: &str, data: Option<&[u8]>) -> Result<DirEntry, FsError> {
    match (place, data) {
        (Place::Library | Place::Bin, Some(d)) => storage.create_in(usb, dir, name, d),
        (Place::Library | Place::Bin, None) => storage.mkdir_in(usb, dir, name),
        (Place::Usb { drive, vol }, _) => usb.usb_create(drive, vol, dir, name, data),
    }
}

/// A folder name not used in `existing` ("Trip", "Trip (2)", ...).
fn free_folder_name(existing: &[DirEntry], name: &str) -> String {
    let taken = |n: &str| existing.iter().any(|e| e.name.eq_ignore_ascii_case(n));
    if !taken(name) {
        return String::from(name);
    }
    (2..).map(|k| format!("{} ({})", name, k)).find(|n| !taken(n)).unwrap()
}

/// Copies one file or folder per tick so the screen stays responsive.
pub struct CopyJob {
    from: Place,
    pub to: Place,
    /// (source, destination folder)
    queue: alloc::collections::VecDeque<(DirEntry, DirEntry)>,
    pub copied: usize,
    pub skipped: usize,
    pub failed: usize,
    pub top_name: String,
    stop: bool,
    /// A big file from a phone being copied a piece at a time: (space, source, next offset).
    stream: Option<(crate::fs::exfat::Pending, DirEntry, u64)>,
}

impl CopyJob {
    pub fn new(spec: CopySpec) -> CopyJob {
        let top_name = spec.item.name.clone();
        let mut queue = alloc::collections::VecDeque::new();
        queue.push_back((spec.item, spec.dest));
        CopyJob { from: spec.from, to: spec.to, queue, copied: 0, skipped: 0, failed: 0, top_name, stop: false, stream: None }
    }

    pub fn finished(&self) -> bool {
        (self.queue.is_empty() && self.stream.is_none()) || self.stop
    }

    /// Copy the next item. Folders are created (under a free name) and their contents queued.
    pub fn step(&mut self, storage: &mut Storage, usb: &mut Usb) {
        if let Some((pending, src, off)) = self.stream.take() {
            let Place::Usb { drive, .. } = self.from else { return };
            let r = usb.read_part(drive, &src, off, crate::phone::PIECE).and_then(|part| {
                if part.is_empty() {
                    return Err(FsError::Io);
                }
                storage.write_part(usb, &pending, off, &part)?;
                Ok(off + part.len() as u64)
            });
            match r {
                Ok(next) if next < pending.len => self.stream = Some((pending, src, next)),
                Ok(_) => match storage.finish_file(usb, pending) {
                    Ok(_) => self.copied += 1,
                    Err(_) => self.failed += 1,
                },
                Err(e) => {
                    kprintln!("files: copying \"{}\" failed: {:?}", src.name, e);
                    let _ = storage.abort_file(usb, pending);
                    self.failed += 1;
                }
            }
            return;
        }
        let Some((src, dest)) = self.queue.pop_front() else { return };
        let r: Result<(), FsError> = (|| {
            let existing = list_any(storage, usb, self.to, &dest)?;
            if src.is_dir {
                let name = free_folder_name(&existing, &src.name);
                let made = create_any(storage, usb, self.to, &dest, &name, None)?;
                for child in list_any(storage, usb, self.from, &src)? {
                    self.queue.push_back((child, made.clone()));
                }
                return Ok(());
            }
            let pairs: Vec<(String, u64)> = existing.iter().map(|e| (e.name.clone(), e.size)).collect();
            match crate::library::unique_name(&pairs, &src.name, src.size) {
                None => self.skipped += 1, // same name and size already there
                Some(name) if matches!((self.from, self.to), (Place::Usb { drive, .. }, Place::Library) if usb.is_phone(drive)) && src.size > crate::phone::PIECE as u64 => {
                    // Big phone files (videos) go a piece at a time on the next steps.
                    let p = storage.begin_file(usb, &dest, &name, src.size)?;
                    self.stream = Some((p, src.clone(), 0));
                }
                Some(name) => {
                    let data = read_any(storage, usb, self.from, &src)?;
                    create_any(storage, usb, self.to, &dest, &name, Some(&data))?;
                    self.copied += 1;
                }
            }
            Ok(())
        })();
        if let Err(e) = r {
            kprintln!("files: copying \"{}\" failed: {:?}", src.name, e);
            self.failed += 1;
            if matches!(e, FsError::Full | FsError::ReadOnly) {
                self.failed += self.queue.len();
                self.stop = true;
            }
        }
    }

    pub fn summary(&self) -> String {
        let dest = match self.to {
            Place::Usb { .. } => "the USB drive",
            _ => "Solon storage",
        };
        let mut msg = format!("Copied {} file{} to {}", self.copied, if self.copied == 1 { "" } else { "s" }, dest);
        if self.skipped > 0 {
            msg += &format!(", {} already there", self.skipped);
        }
        if self.failed > 0 {
            msg += &format!(", {} failed", self.failed);
        }
        msg
    }
}

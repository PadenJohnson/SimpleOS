//! What Writer, PDF and the Text Editor have in common: the list of files they can open (in
//! Solon storage and on USB drives), a "New" button, opening, saving (in place, or under a
//! new name in Solon's Documents folder), "Save changes?", printing, and a file picker.
//! Each app supplies an editor (the `DocEditor` trait) and a `Kind` describing itself.

use alloc::boxed::Box;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::disk::human;
use crate::documents::{DocSrc, DocsAction};
use crate::fs::DirEntry;
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::library::{stem, Storage};
use crate::print::{Layout, Paper, PrinterInfo};
use crate::textfield::{TextEvent, TextField};
use crate::usb::Usb;

pub const INK: u32 = rgb(32, 36, 44);
pub const DIM: u32 = rgb(106, 112, 124);
pub const BTN: u32 = rgb(234, 237, 242);
const ROW_H: i32 = 46;
const DOUBLE: u64 = 50;

/// What an editor wants from the shell.
pub enum EdAction {
    None,
    Redraw,
    Save,
    /// Save under a new name.
    SaveAs,
    Print,
    /// Back to the list (asks to save first if needed).
    Close,
    Toast(String),
    Message(String),
    /// Let the user pick files of these kinds; each comes back through `picked`.
    Pick { purpose: u8, exts: &'static [&'static str], title: &'static str, many: bool },
    /// Save a copy in another format (asks for a name).
    Export { ext: &'static str, bytes: Vec<u8> },
}

pub trait DocEditor {
    fn draw(&self, c: &mut Canvas, r: Rect);
    fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64) -> EdAction;
    fn on_drag(&mut self, _x: i32, _y: i32, _r: Rect) -> bool {
        false
    }
    fn on_release(&mut self) -> bool {
        false
    }
    fn on_wheel(&mut self, delta: i8, r: Rect) -> bool;
    fn on_key(&mut self, sc: u8, r: Rect) -> EdAction;
    fn dirty(&self) -> bool;
    fn mark_saved(&mut self);
    /// The file's bytes, in the format for this extension.
    fn encode(&mut self, ext: &str) -> Result<Vec<u8>, String>;
    /// Pages to print, drawn at 100 pixels per inch.
    fn print_pages(&mut self, paper: Paper) -> Vec<Canvas>;
    /// A file the user picked after a `Pick`.
    fn picked(&mut self, _purpose: u8, _name: &str, _bytes: Vec<u8>) -> EdAction {
        EdAction::None
    }
    /// The file's name changed (after a save).
    fn renamed(&mut self, _name: &str) {}
    /// For the desktop's scroll bar, if the editor wants one: (area, how far, where).
    fn scroll_info(&self, _r: Rect) -> Option<(Rect, i32, i32)> {
        None
    }
    fn set_scroll(&mut self, _v: i32, _r: Rect) {}
}

/// Describes one document app.
pub struct Kind {
    pub app: &'static str,
    /// Heading of the file list, and the "New" button.
    pub list_title: &'static str,
    pub new_label: &'static str,
    /// What it opens.
    pub exts: &'static [&'static str],
    /// What it saves as when the file isn't already one of `keeps`.
    pub save_ext: &'static str,
    /// Formats it can save back into in place.
    pub keeps: &'static [&'static str],
    pub accent: u32,
    pub empty: [&'static str; 2],
    /// Extra list buttons: (label, purpose). Pressing one starts a new document and asks the
    /// editor's `picked` for files right away (e.g. "PDF from photos").
    pub extra: &'static [(&'static str, u8, &'static [&'static str], &'static str)],
    /// Document icon in the list: draws a small 26x28 icon.
    pub icon: fn(&mut Canvas, i32, i32),
}

pub fn ext_of(name: &str) -> String {
    match name.rfind('.') {
        Some(i) => name[i + 1..].to_ascii_lowercase(),
        None => String::new(),
    }
}

pub fn has_ext(name: &str, exts: &[&str]) -> bool {
    let e = ext_of(name);
    !name.starts_with("~$") && exts.iter().any(|x| *x == e)
}

struct Item {
    name: String,
    place: String,
    size: u64,
    src: DocSrc,
}

enum Origin {
    New,
    Src(DocSrc),
}

struct Open<E> {
    ed: E,
    name: String,
    origin: Origin,
}

struct PrintSetup {
    printers: Vec<PrinterInfo>,
    printer: usize,
    paper: Paper,
    copies: u32,
    color: bool,
}

struct Picker {
    purpose: u8,
    title: &'static str,
    many: bool,
    items: Vec<Item>,
    chosen: Vec<bool>,
    scroll: i32,
}

enum Dialog {
    None,
    /// Name for a new file: what's being saved (None = the document itself).
    SaveName(TextField, Option<(&'static str, Vec<u8>)>),
    SaveChanges,
    Print(PrintSetup),
    Message(String),
    Pick(Picker),
}

pub fn button(c: &mut Canvas, b: Rect, label: &str, primary: bool, accent: u32) {
    c.round_rect(b.x, b.y, b.w, b.h, 9, if primary { accent } else { BTN });
    c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, if primary { rgb(255, 255, 255) } else { INK });
}

/// Files of these kinds on a USB volume (a few folders deep, and not forever).
fn usb_files(usb: &mut Usb, drive: u32, vol: usize, root: DirEntry, label: &str, phone: bool, exts: &[&str], out: &mut Vec<Item>) {
    let mut stack = Vec::from([(root, String::new(), 0u32)]);
    let mut seen = 0;
    while let Some((dir, path, depth)) = stack.pop() {
        let Ok(list) = usb.list(drive, vol, &dir) else { continue };
        for e in list {
            seen += 1;
            if seen > 5000 {
                return;
            }
            if e.is_dir {
                let lower = e.name.to_ascii_lowercase();
                let wanted = !phone || depth > 0 || lower == "documents" || lower.starts_with("download") || lower == "dcim" || lower == "pictures";
                if depth < 4 && wanted && !lower.starts_with('.') {
                    let p = if path.is_empty() { e.name.clone() } else { format!("{}/{}", path, e.name) };
                    stack.push((e, p, depth + 1));
                }
            } else if has_ext(&e.name, exts) {
                let place = if path.is_empty() { String::from(label) } else { format!("{} / {}", label, path.replace('/', " / ")) };
                out.push(Item { name: e.name.clone(), place, size: e.size, src: DocSrc::Usb { drive, vol, entry: e } });
            }
        }
    }
}

/// Every file of these kinds in Solon storage, then on USB drives. Returns (items, how many on USB).
fn find_files(storage: &mut Storage, usb: &mut Usb, exts: &[&str]) -> (Vec<Item>, usize) {
    let mut items = Vec::new();
    for (dir, e) in storage.all_files(usb, &|n| has_ext(n, exts)) {
        let place = if dir.name.is_empty() { String::from("Solon storage") } else { format!("Solon storage / {}", dir.name) };
        items.push(Item { name: e.name.clone(), place, size: e.size, src: DocSrc::Library { dir, entry: e } });
    }
    let lib = items.len();
    let vols: Vec<(u32, usize, DirEntry, String, bool)> = usb.sources().into_iter().filter_map(|s| Some((s.drive, s.vol, usb.source_root(s.drive, s.vol)?, s.label, s.phone))).collect();
    for (drive, vol, root, label, phone) in vols {
        usb_files(usb, drive, vol, root, &label, phone, exts, &mut items);
    }
    let n = items.len() - lib;
    (items, n)
}

fn read_src(src: &DocSrc, storage: &mut Storage, usb: &mut Usb) -> Result<Vec<u8>, String> {
    match src {
        DocSrc::Library { entry, .. } => storage.read_file(usb, entry).map_err(|e| String::from(e.message())),
        DocSrc::Usb { drive, vol, entry } => usb.read_file(*drive, *vol, entry).map_err(|e| String::from(e.message())),
    }
}

fn src_name(src: &DocSrc) -> &str {
    match src {
        DocSrc::Library { entry, .. } | DocSrc::Usb { entry, .. } => &entry.name,
    }
}

pub struct DocShell<E: DocEditor> {
    kind: &'static Kind,
    make_new: fn() -> E,
    load: fn(&str, &[u8]) -> Result<E, String>,
    items: Vec<Item>,
    usb_count: usize,
    selected: Option<usize>,
    scroll: i32,
    last_click: Option<(usize, u64)>,
    doc: Option<Open<E>>,
    dialog: Dialog,
    close_after_save: bool,
}

/// What the desktop needs from any document app.
pub trait DocApp {
    fn draw(&self, c: &mut Canvas, r: Rect);
    fn title(&self) -> Option<String>;
    fn editing(&self) -> bool;
    fn refresh(&mut self, storage: &mut Storage, usb: &mut Usb);
    fn opens(&self, name: &str) -> bool;
    fn open_src(&mut self, src: DocSrc, storage: &mut Storage, usb: &mut Usb) -> DocsAction;
    fn start_print(&mut self, printers: Vec<PrinterInfo>);
    fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, storage: &mut Storage, usb: &mut Usb) -> DocsAction;
    fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool;
    fn on_release(&mut self) -> bool;
    fn on_wheel(&mut self, delta: i8, r: Rect) -> bool;
    fn on_key(&mut self, sc: u8, r: Rect, storage: &mut Storage, usb: &mut Usb) -> Option<DocsAction>;
    /// For the desktop's scroll bar: (area, how far it scrolls, where it is).
    fn scroll_info(&self, r: Rect) -> Option<(Rect, i32, i32)>;
    fn set_scroll(&mut self, v: i32, r: Rect);
}

impl<E: DocEditor> DocShell<E> {
    pub fn new(kind: &'static Kind, make_new: fn() -> E, load: fn(&str, &[u8]) -> Result<E, String>) -> Box<DocShell<E>> {
        Box::new(DocShell { kind, make_new, load, items: Vec::new(), usb_count: 0, selected: None, scroll: 0, last_click: None, doc: None, dialog: Dialog::None, close_after_save: false })
    }

    fn message(&mut self, m: String) -> DocsAction {
        self.dialog = Dialog::Message(m);
        DocsAction::Redraw
    }

    // ---- saving -------------------------------------------------------------------

    fn save(&mut self, storage: &mut Storage, usb: &mut Usb, as_new: bool) -> DocsAction {
        let Some(open) = &mut self.doc else { return DocsAction::None };
        if storage.library.is_none() {
            return self.message(String::from("Set up Solon storage first (in the Disks app) to save documents."));
        }
        let in_place = matches!(&open.origin, Origin::Src(DocSrc::Library { entry, .. }) if has_ext(&entry.name, self.kind.keeps));
        if in_place && !as_new {
            return self.write_in_place(storage, usb);
        }
        let suggested = String::from(stem(&open.name));
        self.dialog = Dialog::SaveName(TextField::new(&suggested, 80), None);
        DocsAction::Redraw
    }

    fn saved(&mut self, msg: String) -> DocsAction {
        if let Some(o) = &mut self.doc {
            o.ed.mark_saved();
        }
        if self.close_after_save {
            self.close_after_save = false;
            self.doc = None;
        }
        DocsAction::Toast(msg)
    }

    fn write_in_place(&mut self, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        let open = self.doc.as_mut().unwrap();
        let Origin::Src(DocSrc::Library { dir, entry }) = &open.origin else { return DocsAction::None };
        let (dir, entry) = (dir.clone(), entry.clone());
        let bytes = match open.ed.encode(&ext_of(&entry.name)) {
            Ok(b) => b,
            Err(e) => return self.message(format!("Couldn't save: {}", e)),
        };
        match storage.save_file(usb, &dir, Some(&entry), &entry.name, &bytes) {
            Ok(new) => {
                kprintln!("{}: saved \"{}\" ({} bytes)", self.kind.app, new.name, bytes.len());
                let msg = format!("Saved \"{}\"", new.name);
                let open = self.doc.as_mut().unwrap();
                open.name = new.name.clone();
                open.origin = Origin::Src(DocSrc::Library { dir, entry: new });
                self.saved(msg)
            }
            Err(e) => self.message(format!("Couldn't save: {}", e.message())),
        }
    }

    /// Save as a new file in Solon's Documents folder: the document itself, or an export.
    fn write_new(&mut self, storage: &mut Storage, usb: &mut Usb, name: &str, export: Option<(&'static str, Vec<u8>)>) -> DocsAction {
        let dir = match storage.documents_dir(usb) {
            Ok(d) => d,
            Err(e) => return self.message(format!("Couldn't save: {}", e.message())),
        };
        let Some(open) = self.doc.as_mut() else { return DocsAction::None };
        let is_doc = export.is_none();
        let (file, bytes) = match export {
            Some((ext, bytes)) => (format!("{}.{}", name, ext), bytes),
            None => {
                let ext = match &open.origin {
                    Origin::Src(s) if has_ext(src_name(s), self.kind.keeps) => ext_of(src_name(s)),
                    _ => String::from(self.kind.save_ext),
                };
                match open.ed.encode(&ext) {
                    Ok(b) => (format!("{}.{}", name, ext), b),
                    Err(e) => return self.message(format!("Couldn't save: {}", e)),
                }
            }
        };
        match storage.save_file(usb, &dir, None, &file, &bytes) {
            Ok(new) => {
                kprintln!("{}: saved new \"{}\" ({} bytes)", self.kind.app, new.name, bytes.len());
                let msg = format!("Saved \"{}\" in Documents", new.name);
                if is_doc {
                    let open = self.doc.as_mut().unwrap();
                    open.name = new.name.clone();
                    open.ed.renamed(&new.name);
                    open.origin = Origin::Src(DocSrc::Library { dir, entry: new });
                    self.saved(msg)
                } else {
                    DocsAction::Toast(msg)
                }
            }
            Err(e) => self.message(format!("Couldn't save: {}", e.message())),
        }
    }

    fn ed_action(&mut self, a: EdAction, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        match a {
            EdAction::None => DocsAction::None,
            EdAction::Redraw => DocsAction::Redraw,
            EdAction::Save => self.save(storage, usb, false),
            EdAction::SaveAs => self.save(storage, usb, true),
            EdAction::Print => DocsAction::WantPrinters,
            EdAction::Toast(m) => DocsAction::Toast(m),
            EdAction::Message(m) => self.message(m),
            EdAction::Close => {
                if self.doc.as_ref().is_some_and(|o| o.ed.dirty()) {
                    self.dialog = Dialog::SaveChanges;
                } else {
                    self.doc = None;
                    self.refresh(storage, usb);
                }
                DocsAction::Redraw
            }
            EdAction::Pick { purpose, exts, title, many } => {
                let (items, _) = find_files(storage, usb, exts);
                if items.is_empty() {
                    return self.message(String::from("There are no files of that kind in Solon storage or on a USB drive."));
                }
                let chosen = alloc::vec![false; items.len()];
                self.dialog = Dialog::Pick(Picker { purpose, title, many, items, chosen, scroll: 0 });
                DocsAction::Redraw
            }
            EdAction::Export { ext, bytes } => {
                if storage.library.is_none() {
                    return self.message(String::from("Set up Solon storage first (in the Disks app) to save documents."));
                }
                let name = self.doc.as_ref().map(|o| String::from(stem(&o.name))).unwrap_or_default();
                self.dialog = Dialog::SaveName(TextField::new(&name, 80), Some((ext, bytes)));
                DocsAction::Redraw
            }
        }
    }

    // ---- layout ---------------------------------------------------------------------

    fn new_btn(&self, r: Rect) -> Rect {
        let w = text_width(self.kind.new_label, Size::Small, true) + 40;
        Rect::new(r.x + r.w - 20 - w, r.y + 16, w, 38)
    }
    fn extra_btn(&self, r: Rect, k: usize) -> Rect {
        let mut x = self.new_btn(r).x;
        let mut rect = Rect::new(0, 0, 0, 0);
        for (i, (label, ..)) in self.kind.extra.iter().enumerate() {
            let w = text_width(label, Size::Small, true) + 40;
            x -= w + 10;
            if i == k {
                rect = Rect::new(x, r.y + 16, w, 38);
            }
        }
        rect
    }
    fn list_top(r: Rect) -> i32 {
        r.y + 70
    }
    fn row(&self, r: Rect, i: usize) -> Rect {
        let extra = if self.usb_count > 0 && i >= self.items.len() - self.usb_count { 34 } else { 0 };
        Rect::new(r.x + 16, Self::list_top(r) + 32 + i as i32 * ROW_H + extra - self.scroll, r.w - 32, ROW_H - 4)
    }
    fn dlg(r: Rect, h: i32) -> Rect {
        let w = 580.min(r.w - 40);
        Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h)
    }
    fn dlg_buttons(&self) -> Vec<(&'static str, bool)> {
        match &self.dialog {
            Dialog::None => Vec::new(),
            Dialog::SaveName(..) => Vec::from([("Save", true), ("Cancel", false)]),
            Dialog::SaveChanges => Vec::from([("Save", true), ("Don't save", false), ("Cancel", false)]),
            Dialog::Print(_) => Vec::from([("Print", true), ("Cancel", false)]),
            Dialog::Message(_) => Vec::from([("OK", true)]),
            Dialog::Pick(p) => {
                if p.many {
                    Vec::from([("Add", true), ("Cancel", false)])
                } else {
                    Vec::from([("Cancel", false)])
                }
            }
        }
    }
    fn dlg_h(&self, r: Rect) -> i32 {
        match &self.dialog {
            Dialog::Print(_) => 330,
            Dialog::Pick(_) => (r.h - 60).min(520),
            _ => 210,
        }
    }
    fn dlg_button_rects(&self, d: Rect) -> Vec<Rect> {
        let mut x = d.x + d.w - 20;
        let mut v = Vec::new();
        for (label, _) in self.dlg_buttons() {
            let w = (text_width(label, Size::Small, true) + 32).max(96);
            x -= w;
            v.push(Rect::new(x, d.y + d.h - 56, w, 38));
            x -= 10;
        }
        v
    }
    fn print_rows(d: Rect) -> [Rect; 5] {
        let x = d.x + 150;
        [Rect::new(x, d.y + 64, d.w - 170, 36), Rect::new(x, d.y + 110, d.w - 170, 36), Rect::new(x, d.y + 156, 40, 36), Rect::new(x + 100, d.y + 156, 40, 36), Rect::new(x, d.y + 202, d.w - 170, 36)]
    }
    fn pick_row(d: Rect, p: &Picker, i: usize) -> Rect {
        Rect::new(d.x + 16, d.y + 64 + i as i32 * 40 - p.scroll, d.w - 32, 36)
    }
    fn pick_area(d: Rect) -> Rect {
        Rect::new(d.x, d.y + 60, d.w, d.h - 60 - 70)
    }

    // ---- drawing ----------------------------------------------------------------

    fn draw_list(&self, c: &mut Canvas, r: Rect) {
        let k = self.kind;
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(250, 250, 252));
        c.text(r.x + 22, r.y + 24, k.list_title, Size::Medium, true, INK);
        button(c, self.new_btn(r), k.new_label, true, k.accent);
        for (i, (label, ..)) in k.extra.iter().enumerate() {
            button(c, self.extra_btn(r, i), label, false, k.accent);
        }
        let top = Self::list_top(r);
        c.fill_rect(r.x, top, r.w, 1, rgb(228, 231, 236));
        if self.items.is_empty() {
            c.text_centered(r.x + r.w / 2, top + 70, k.empty[0], Size::Medium, true, INK);
            c.text_centered(r.x + r.w / 2, top + 102, k.empty[1], Size::Small, false, DIM);
            return;
        }
        let list = Rect::new(r.x, top + 1, r.w, r.y + r.h - top - 1);
        c.set_clip(list);
        let lib = self.items.len() - self.usb_count;
        let head = |c: &mut Canvas, y: i32, t: &str| c.text(r.x + 22, y, t, Size::Small, true, DIM);
        head(c, top + 10 - self.scroll, if lib > 0 { "In Solon storage" } else { "Nothing in Solon storage yet" });
        if self.usb_count > 0 {
            let y = self.row(r, lib).y - 26;
            head(c, y, "On USB drives and phones (saving keeps a copy in Solon storage)");
        }
        for (i, it) in self.items.iter().enumerate() {
            let b = self.row(r, i);
            if b.y + b.h < list.y || b.y > list.y + list.h {
                continue;
            }
            if self.selected == Some(i) {
                c.round_rect_alpha(b.x, b.y, b.w, b.h, 10, k.accent, 40);
            }
            (k.icon)(c, b.x + 10, b.y + 7);
            draw_item(c, b, it);
        }
        c.set_clip(r);
    }

    fn draw_dialog(&self, c: &mut Canvas, r: Rect) {
        if matches!(self.dialog, Dialog::None) {
            return;
        }
        let accent = self.kind.accent;
        c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 110);
        let d = Self::dlg(r, self.dlg_h(r));
        c.shadow(d.x, d.y + 6, d.w, d.h, 14, 22, 100);
        c.round_rect(d.x, d.y, d.w, d.h, 14, rgb(255, 255, 255));
        let line = |c: &mut Canvas, i: i32, t: &str| c.text(d.x + 24, d.y + 62 + i * 24, t, Size::Small, false, DIM);
        match &self.dialog {
            Dialog::SaveName(f, export) => {
                let what = match export {
                    Some((ext, _)) => format!("Save a .{} copy", ext),
                    None => format!("Save {}", self.kind.app),
                };
                c.text(d.x + 24, d.y + 20, &what, Size::Medium, true, INK);
                line(c, 0, "Name (saved in Solon storage / Documents):");
                f.draw(c, Rect::new(d.x + 24, d.y + 92, d.w - 48, 38));
            }
            Dialog::SaveChanges => {
                c.text(d.x + 24, d.y + 20, "Save your changes?", Size::Medium, true, INK);
                let name = self.doc.as_ref().map(|o| o.name.clone()).unwrap_or_default();
                line(c, 0, &crate::photos::ellipsize(&format!("\"{}\" has changes that aren't saved.", name), d.w - 48, Size::Small, false));
            }
            Dialog::Print(p) => {
                c.text(d.x + 24, d.y + 20, "Print", Size::Medium, true, INK);
                let rows = Self::print_rows(d);
                for (i, l) in ["Printer", "Paper", "Copies", "", "Colour"].iter().enumerate() {
                    if !l.is_empty() {
                        c.text(d.x + 24, rows[i].y + 9, l, Size::Small, true, INK);
                    }
                }
                let pr = &p.printers[p.printer];
                let pname = crate::photos::ellipsize(&pr.name, rows[0].w - 30, Size::Small, true);
                button(c, rows[0], &pname, false, accent);
                button(c, rows[1], p.paper.name(), false, accent);
                button(c, rows[2], "-", false, accent);
                c.text_centered(rows[2].x + 70, rows[2].y + 9, &format!("{}", p.copies), Size::Small, true, INK);
                button(c, rows[3], "+", false, accent);
                button(c, rows[4], if p.color { "Colour" } else { "Black and white" }, false, accent);
                c.text(d.x + 24, d.y + 250, "Click a choice to change it.", Size::Small, false, DIM);
            }
            Dialog::Message(m) => {
                c.text(d.x + 24, d.y + 20, self.kind.app, Size::Medium, true, INK);
                for (i, l) in wrap_lines(m, d.w - 48, 3).iter().enumerate() {
                    line(c, i as i32, l);
                }
            }
            Dialog::Pick(p) => {
                c.text(d.x + 24, d.y + 20, p.title, Size::Medium, true, INK);
                let area = Self::pick_area(d);
                c.set_clip(area);
                for (i, it) in p.items.iter().enumerate() {
                    let b = Self::pick_row(d, p, i);
                    if b.y + b.h < area.y || b.y > area.y + area.h {
                        continue;
                    }
                    if p.chosen[i] {
                        c.round_rect_alpha(b.x, b.y, b.w, b.h, 8, accent, 50);
                    } else if i % 2 == 0 {
                        c.round_rect(b.x, b.y, b.w, b.h, 8, rgb(246, 247, 250));
                    }
                    if p.many {
                        c.round_rect(b.x + 10, b.y + 9, 18, 18, 4, if p.chosen[i] { accent } else { rgb(200, 205, 214) });
                        if p.chosen[i] {
                            c.text(b.x + 13, b.y + 9, "+", Size::Small, true, rgb(255, 255, 255));
                        }
                    }
                    let b2 = Rect::new(b.x + 30, b.y - 4, b.w - 30, b.h);
                    draw_item(c, b2, it);
                }
                c.reset_clip();
                c.set_clip(r);
                let n = p.chosen.iter().filter(|&&x| x).count();
                if p.many {
                    c.text(d.x + 24, d.y + d.h - 46, &format!("{} chosen", n), Size::Small, false, DIM);
                }
            }
            Dialog::None => {}
        }
        for ((label, primary), b) in self.dlg_buttons().into_iter().zip(self.dlg_button_rects(d)) {
            button(c, b, label, primary, accent);
        }
    }

    // ---- input ------------------------------------------------------------------

    fn dialog_choice(&mut self, k: usize, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        let dialog = core::mem::replace(&mut self.dialog, Dialog::None);
        match (dialog, k) {
            (Dialog::SaveName(f, export), 0) => {
                let name = String::from(f.text.trim());
                if name.is_empty() {
                    self.dialog = Dialog::SaveName(f, export);
                    return DocsAction::Redraw;
                }
                self.write_new(storage, usb, &name, export)
            }
            (Dialog::SaveChanges, 0) => {
                self.close_after_save = true;
                self.save(storage, usb, false)
            }
            (Dialog::SaveChanges, 1) => {
                self.doc = None;
                self.refresh(storage, usb);
                DocsAction::Redraw
            }
            (Dialog::Print(p), 0) => {
                let Some(open) = &mut self.doc else { return DocsAction::None };
                let pages = open.ed.print_pages(p.paper);
                if pages.is_empty() {
                    return self.message(String::from("There's nothing to print."));
                }
                let layout = Layout { paper: p.paper, per_page: 1, fill: false, color: p.color, copies: p.copies };
                let printer = p.printers[p.printer].clone();
                DocsAction::Print { pages, layout, printer }
            }
            (Dialog::Pick(p), 0) if p.many => {
                let srcs: Vec<(String, DocSrc)> = p.items.iter().zip(&p.chosen).filter(|(_, &c)| c).map(|(it, _)| (it.name.clone(), it.src.clone())).collect();
                self.take_picked(p.purpose, srcs, storage, usb)
            }
            _ => {
                self.close_after_save = false;
                DocsAction::Redraw
            }
        }
    }

    fn take_picked(&mut self, purpose: u8, srcs: Vec<(String, DocSrc)>, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        let mut last = DocsAction::Redraw;
        for (name, src) in srcs {
            match read_src(&src, storage, usb) {
                Ok(bytes) => {
                    let Some(open) = &mut self.doc else { break };
                    let a = open.ed.picked(purpose, &name, bytes);
                    last = self.ed_action(a, storage, usb);
                }
                Err(e) => return self.message(format!("Couldn't read \"{}\": {}", name, e)),
            }
        }
        last
    }

    fn print_click(&mut self, x: i32, y: i32, d: Rect) -> bool {
        let rows = Self::print_rows(d);
        let Dialog::Print(p) = &mut self.dialog else { return false };
        if rows[0].contains(x, y) && p.printers.len() > 1 {
            p.printer = (p.printer + 1) % p.printers.len();
            let pr = &p.printers[p.printer];
            if !pr.papers.contains(&p.paper) {
                p.paper = if pr.papers.contains(&Paper::Letter) { Paper::Letter } else { Paper::A4 };
            }
            p.color = pr.color;
        } else if rows[1].contains(x, y) {
            let next = if p.paper == Paper::Letter { Paper::A4 } else { Paper::Letter };
            if p.printers[p.printer].papers.contains(&next) {
                p.paper = next;
            }
        } else if rows[2].contains(x, y) {
            p.copies = (p.copies - 1).max(1);
        } else if rows[3].contains(x, y) {
            p.copies = (p.copies + 1).min(99);
        } else if rows[4].contains(x, y) && p.printers[p.printer].color {
            p.color = !p.color;
        } else {
            return false;
        }
        true
    }

    fn open_item(&mut self, i: usize, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        let src = self.items[i].src.clone();
        self.open_src_inner(src, storage, usb)
    }

    fn open_src_inner(&mut self, src: DocSrc, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        let name = String::from(src_name(&src));
        let bytes = match read_src(&src, storage, usb) {
            Ok(b) => b,
            Err(e) => return self.message(format!("Couldn't open \"{}\": {}", name, e)),
        };
        match (self.load)(&name, &bytes) {
            Ok(ed) => {
                kprintln!("{}: opened \"{}\" ({} bytes)", self.kind.app, name, bytes.len());
                self.doc = Some(Open { ed, name, origin: Origin::Src(src) });
                DocsAction::Redraw
            }
            Err(e) => {
                kprintln!("{}: couldn't open \"{}\": {}", self.kind.app, name, e);
                self.message(format!("Couldn't open \"{}\": {}", name, e))
            }
        }
    }
}

fn draw_item(c: &mut Canvas, b: Rect, it: &Item) {
    let size = human(it.size);
    let sw = text_width(&size, Size::Small, false);
    let pw = (b.w / 3).min(text_width(&it.place, Size::Small, false));
    let name = crate::photos::ellipsize(&it.name, b.w - 70 - sw - pw - 30, Size::Small, true);
    c.text(b.x + 48, b.y + 13, &name, Size::Small, true, INK);
    let place = crate::photos::ellipsize(&it.place, pw, Size::Small, false);
    c.text(b.x + b.w - 30 - sw - pw, b.y + 13, &place, Size::Small, false, DIM);
    c.text(b.x + b.w - 12 - sw, b.y + 13, &size, Size::Small, false, DIM);
}

/// Break text into at most `max` lines that fit `w` pixels.
pub fn wrap_lines(m: &str, w: i32, max: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut cur = String::new();
    for word in m.split(' ') {
        let t = if cur.is_empty() { String::from(word) } else { format!("{} {}", cur, word) };
        if text_width(&t, Size::Small, false) <= w || cur.is_empty() {
            cur = t;
        } else {
            lines.push(core::mem::replace(&mut cur, String::from(word)));
        }
    }
    if !cur.is_empty() {
        lines.push(cur);
    }
    if lines.len() > max {
        lines.truncate(max);
        let last = lines.last_mut().unwrap();
        *last = crate::photos::ellipsize(&format!("{}...", last), w, Size::Small, false);
    }
    lines
}

impl<E: DocEditor> DocApp for DocShell<E> {
    fn draw(&self, c: &mut Canvas, r: Rect) {
        match &self.doc {
            Some(o) => o.ed.draw(c, r),
            None => self.draw_list(c, r),
        }
        self.draw_dialog(c, r);
    }

    fn title(&self) -> Option<String> {
        self.doc.as_ref().map(|o| format!("{}{} - {}", o.name, if o.ed.dirty() { " (not saved)" } else { "" }, self.kind.app))
    }

    fn editing(&self) -> bool {
        self.doc.is_some()
    }

    fn refresh(&mut self, storage: &mut Storage, usb: &mut Usb) {
        let (items, n) = find_files(storage, usb, self.kind.exts);
        self.items = items;
        self.usb_count = n;
        self.selected = self.selected.filter(|&i| i < self.items.len());
    }

    fn opens(&self, name: &str) -> bool {
        has_ext(name, self.kind.exts)
    }

    fn open_src(&mut self, src: DocSrc, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        if self.doc.as_ref().is_some_and(|o| o.ed.dirty()) {
            return self.message(String::from("Save or close the document that's open first."));
        }
        self.open_src_inner(src, storage, usb)
    }

    fn start_print(&mut self, printers: Vec<PrinterInfo>) {
        if self.doc.is_none() {
            return;
        }
        if printers.is_empty() {
            self.dialog = Dialog::Message(String::from("No printer found. Connect a USB printer and try again."));
            return;
        }
        let paper = if printers[0].papers.contains(&Paper::Letter) { Paper::Letter } else { Paper::A4 };
        let color = printers[0].color;
        self.dialog = Dialog::Print(PrintSetup { printers, printer: 0, paper, copies: 1, color });
    }

    fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        if !matches!(self.dialog, Dialog::None) {
            let d = Self::dlg(r, self.dlg_h(r));
            if let Some(k) = self.dlg_button_rects(d).iter().position(|b| b.contains(x, y)) {
                return self.dialog_choice(k, storage, usb);
            }
            if matches!(self.dialog, Dialog::Print(_)) && self.print_click(x, y, d) {
                return DocsAction::Redraw;
            }
            if let Dialog::Pick(p) = &mut self.dialog {
                if Self::pick_area(d).contains(x, y) {
                    if let Some(i) = (0..p.items.len()).find(|&i| Self::pick_row(d, p, i).contains(x, y)) {
                        if p.many {
                            p.chosen[i] = !p.chosen[i];
                            return DocsAction::Redraw;
                        }
                        let (purpose, name, src) = (p.purpose, p.items[i].name.clone(), p.items[i].src.clone());
                        self.dialog = Dialog::None;
                        return self.take_picked(purpose, Vec::from([(name, src)]), storage, usb);
                    }
                }
            }
            return DocsAction::None;
        }
        if let Some(o) = &mut self.doc {
            let a = o.ed.on_press(x, y, r, now);
            return self.ed_action(a, storage, usb);
        }
        if self.new_btn(r).contains(x, y) {
            self.doc = Some(Open { ed: (self.make_new)(), name: String::from("Untitled"), origin: Origin::New });
            return DocsAction::Redraw;
        }
        for (k, &(_, purpose, exts, title)) in self.kind.extra.iter().enumerate() {
            if self.extra_btn(r, k).contains(x, y) {
                self.doc = Some(Open { ed: (self.make_new)(), name: String::from("Untitled"), origin: Origin::New });
                return self.ed_action(EdAction::Pick { purpose, exts, title, many: true }, storage, usb);
            }
        }
        if y > Self::list_top(r) {
            if let Some(i) = (0..self.items.len()).find(|&i| self.row(r, i).contains(x, y)) {
                let double = matches!(self.last_click, Some((j, t)) if j == i && now - t <= DOUBLE);
                self.selected = Some(i);
                self.last_click = Some((i, now));
                if double {
                    self.last_click = None;
                    return self.open_item(i, storage, usb);
                }
                return DocsAction::Redraw;
            }
        }
        DocsAction::None
    }

    fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        match &mut self.doc {
            Some(o) if matches!(self.dialog, Dialog::None) => o.ed.on_drag(x, y, r),
            _ => false,
        }
    }

    fn on_release(&mut self) -> bool {
        match &mut self.doc {
            Some(o) => o.ed.on_release(),
            None => false,
        }
    }

    fn scroll_info(&self, r: Rect) -> Option<(Rect, i32, i32)> {
        if !matches!(self.dialog, Dialog::None) {
            return None;
        }
        if let Some(o) = &self.doc {
            return o.ed.scroll_info(r);
        }
        let extra = if self.usb_count > 0 { 34 } else { 0 };
        let total = self.items.len() as i32 * ROW_H + 40 + extra;
        let top = Self::list_top(r);
        let max = (total - (r.y + r.h - top)).max(0);
        Some((Rect::new(r.x, top, r.w, r.y + r.h - top), max, self.scroll))
    }

    fn set_scroll(&mut self, v: i32, r: Rect) {
        match &mut self.doc {
            Some(o) => o.ed.set_scroll(v, r),
            None => self.scroll = v.max(0),
        }
    }

    fn on_wheel(&mut self, delta: i8, r: Rect) -> bool {
        if let Dialog::Pick(p) = &mut self.dialog {
            let d = Self::dlg(r, (r.h - 60).min(520));
            let max = (p.items.len() as i32 * 40 - Self::pick_area(d).h + 8).max(0);
            p.scroll = (p.scroll + delta as i32 * 120).clamp(0, max);
            return true;
        }
        if let Some(o) = &mut self.doc {
            return o.ed.on_wheel(delta, r);
        }
        let extra = if self.usb_count > 0 { 34 } else { 0 };
        let total = self.items.len() as i32 * ROW_H + 40 + extra;
        let max = (total - (r.y + r.h - Self::list_top(r))).max(0);
        let old = self.scroll;
        self.scroll = (self.scroll + delta as i32 * 3 * ROW_H).clamp(0, max);
        old != self.scroll
    }

    fn on_key(&mut self, sc: u8, r: Rect, storage: &mut Storage, usb: &mut Usb) -> Option<DocsAction> {
        if let Dialog::SaveName(f, _) = &mut self.dialog {
            return Some(match f.on_key(sc) {
                TextEvent::Submit => self.dialog_choice(0, storage, usb),
                TextEvent::Cancel => {
                    self.dialog = Dialog::None;
                    self.close_after_save = false;
                    DocsAction::Redraw
                }
                TextEvent::Changed => DocsAction::Redraw,
                TextEvent::None => DocsAction::None,
            });
        }
        if !matches!(self.dialog, Dialog::None) {
            if sc & 0x80 != 0 {
                return Some(DocsAction::None);
            }
            return Some(match sc {
                0x1C if !matches!(self.dialog, Dialog::Pick(_)) => self.dialog_choice(0, storage, usb),
                0x01 => {
                    self.dialog = Dialog::None;
                    self.close_after_save = false;
                    DocsAction::Redraw
                }
                _ => DocsAction::None,
            });
        }
        if let Some(o) = &mut self.doc {
            let a = o.ed.on_key(sc, r);
            return Some(self.ed_action(a, storage, usb));
        }
        if sc & 0x80 != 0 {
            return Some(DocsAction::None);
        }
        let n = self.items.len();
        match sc {
            0x48 | 0x50 if n > 0 => {
                self.selected = Some(match (self.selected, sc) {
                    (None, _) => 0,
                    (Some(i), 0x48) => i.saturating_sub(1),
                    (Some(i), _) => (i + 1).min(n - 1),
                });
                Some(DocsAction::Redraw)
            }
            0x1C => match self.selected {
                Some(i) => Some(self.open_item(i, storage, usb)),
                None => Some(DocsAction::None),
            },
            0x01 => None,
            _ => Some(DocsAction::None),
        }
    }
}

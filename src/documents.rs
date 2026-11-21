//! Documents app: the spreadsheets in Solon storage and on USB drives, a "New spreadsheet"
//! button, and the spreadsheet editor. Saving goes to Solon storage (USB drives are only
//! ever added to, so a spreadsheet opened from one is saved into Solon's Documents folder).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::disk::human;
use crate::doc::sheet::Workbook;
use crate::doc::xlsx;
use crate::fs::DirEntry;
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::library::{is_document, stem, Storage};
use crate::print::{Layout, Paper, PrinterInfo};
use crate::sheetui::{render_pages, SheetAction, SheetEditor};
use crate::textfield::{TextEvent, TextField};
use crate::usb::Usb;

const INK: u32 = rgb(40, 44, 52);
const DIM: u32 = rgb(110, 116, 126);
const GREEN: u32 = rgb(33, 115, 70);
const BTN: u32 = rgb(230, 233, 238);
const ROW_H: i32 = 44;
const DOUBLE: u64 = 50;

#[derive(Clone)]
pub enum DocSrc {
    Library { dir: DirEntry, entry: DirEntry },
    Usb { drive: u32, vol: usize, entry: DirEntry },
}

struct Item {
    name: String,
    place: String,
    size: u64,
    src: DocSrc,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Xlsx,
    Csv,
}

enum Origin {
    New,
    Src(DocSrc),
}

struct OpenDoc {
    ed: SheetEditor,
    origin: Origin,
    name: String,
    kind: Kind,
}

struct PrintSetup {
    printers: Vec<PrinterInfo>,
    printer: usize,
    paper: Paper,
    copies: u32,
    color: bool,
    pages: usize,
}

enum Dialog {
    None,
    /// Name for a new file in Solon's Documents folder.
    SaveName(TextField),
    /// A CSV file now has things CSV can't hold.
    CsvChoice,
    /// Closing with unsaved changes.
    SaveChanges,
    Print(PrintSetup),
    Message(String),
}

pub enum DocsAction {
    None,
    Redraw,
    Toast(String),
    /// Ask the desktop for the printer list (it calls `start_print`).
    WantPrinters,
    /// Print these pages (drawn at 100 pixels per inch).
    Print { pages: Vec<Canvas>, layout: Layout, printer: PrinterInfo },
}

pub struct DocsApp {
    items: Vec<Item>,
    usb_count: usize,
    selected: Option<usize>,
    scroll: i32,
    last_click: Option<(usize, u64)>,
    open: Option<OpenDoc>,
    dialog: Dialog,
    close_after_save: bool,
}

fn button(c: &mut Canvas, b: Rect, label: &str, primary: bool) {
    c.round_rect(b.x, b.y, b.w, b.h, 8, if primary { GREEN } else { BTN });
    c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, if primary { rgb(255, 255, 255) } else { INK });
}

fn sheet_icon(c: &mut Canvas, x: i32, y: i32) {
    c.round_rect(x, y, 26, 28, 4, GREEN);
    for k in 0..3 {
        c.fill_rect(x + 5, y + 8 + k * 6, 16, 1, rgb(255, 255, 255));
    }
    c.fill_rect(x + 12, y + 6, 1, 16, rgb(255, 255, 255));
}

/// Spreadsheets on a USB volume (looks a few folders deep, and not forever).
fn usb_documents(usb: &mut Usb, drive: u32, vol: usize, root: DirEntry, label: &str, phone: bool, out: &mut Vec<Item>) {
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
                // On phones only look where documents live (photo folders are huge).
                let lower = e.name.to_ascii_lowercase();
                let wanted = !phone || depth > 0 || lower == "documents" || lower.starts_with("download");
                if depth < 4 && wanted {
                    let p = if path.is_empty() { e.name.clone() } else { format!("{}/{}", path, e.name) };
                    stack.push((e, p, depth + 1));
                }
            } else if is_document(&e.name) {
                let place = if path.is_empty() { String::from(label) } else { format!("{} / {}", label, path.replace('/', " / ")) };
                out.push(Item { name: e.name.clone(), place, size: e.size, src: DocSrc::Usb { drive, vol, entry: e } });
            }
        }
    }
}

impl DocsApp {
    pub fn new() -> DocsApp {
        DocsApp { items: Vec::new(), usb_count: 0, selected: None, scroll: 0, last_click: None, open: None, dialog: Dialog::None, close_after_save: false }
    }

    pub fn editing(&self) -> bool {
        self.open.is_some()
    }

    /// Re-scan Solon storage and USB drives for spreadsheets.
    pub fn refresh(&mut self, storage: &mut Storage, usb: &mut Usb) {
        let mut items = Vec::new();
        for (dir, e) in storage.all_documents(usb) {
            let place = if dir.name.is_empty() { String::from("Solon storage") } else { format!("Solon storage / {}", dir.name) };
            items.push(Item { name: e.name.clone(), place, size: e.size, src: DocSrc::Library { dir, entry: e } });
        }
        let lib_count = items.len();
        let vols: Vec<(u32, usize, DirEntry, String, bool)> = usb.sources().into_iter().filter_map(|s| Some((s.drive, s.vol, usb.source_root(s.drive, s.vol)?, s.label, s.phone))).collect();
        for (drive, vol, root, label, phone) in vols {
            usb_documents(usb, drive, vol, root, &label, phone, &mut items);
        }
        self.usb_count = items.len() - lib_count;
        self.items = items;
        self.selected = self.selected.filter(|&i| i < self.items.len());
    }

    /// Open a spreadsheet (from the list, or from the Files app).
    pub fn open_src(&mut self, src: DocSrc, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        let (entry, bytes) = match &src {
            DocSrc::Library { entry, .. } => (entry.clone(), storage.read_file(usb, entry).map_err(|e| e.message())),
            DocSrc::Usb { drive, vol, entry } => (entry.clone(), usb.read_file(*drive, *vol, entry).map_err(|e| e.message())),
        };
        let kind = if entry.name.to_ascii_lowercase().ends_with(".csv") { Kind::Csv } else { Kind::Xlsx };
        let wb = bytes.and_then(|b| match kind {
            Kind::Csv => Ok(xlsx::read_csv(&b)),
            Kind::Xlsx => xlsx::read(&b),
        });
        match wb {
            Ok(wb) => {
                let cells: usize = wb.sheets.iter().map(|s| s.cells.len()).sum();
                kprintln!("docs: opened \"{}\": {} sheet(s), {} cells", entry.name, wb.sheets.len(), cells);
                self.open = Some(OpenDoc { ed: SheetEditor::new(wb), origin: Origin::Src(src), name: entry.name.clone(), kind });
                DocsAction::Redraw
            }
            Err(e) => {
                kprintln!("docs: couldn't open \"{}\": {}", entry.name, e);
                self.dialog = Dialog::Message(format!("Couldn't open \"{}\": {}", entry.name, e));
                DocsAction::Redraw
            }
        }
    }

    fn new_doc(&mut self) {
        self.open = Some(OpenDoc { ed: SheetEditor::new(Workbook::new()), origin: Origin::New, name: String::from("New spreadsheet"), kind: Kind::Xlsx });
        kprintln!("docs: new spreadsheet");
    }

    /// The title bar text for the open document.
    pub fn title(&self) -> Option<String> {
        self.open.as_ref().map(|o| format!("{}{}", o.name, if o.ed.dirty { " (not saved)" } else { "" }))
    }

    // ---- saving -------------------------------------------------------------------

    fn encode(wb: &Workbook, kind: Kind) -> Vec<u8> {
        match kind {
            Kind::Xlsx => xlsx::write(wb),
            Kind::Csv => xlsx::write_csv(wb),
        }
    }

    fn save(&mut self, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        let Some(open) = &mut self.open else { return DocsAction::None };
        open.ed.finish_edit();
        if storage.library.is_none() {
            self.dialog = Dialog::Message(String::from("Set up Solon storage first (in the Disks app) to save documents."));
            return DocsAction::Redraw;
        }
        match &open.origin {
            Origin::Src(DocSrc::Library { .. }) => {
                if open.kind == Kind::Csv && open.ed.wb.needs_xlsx() {
                    self.dialog = Dialog::CsvChoice;
                    return DocsAction::Redraw;
                }
                self.write_in_place(storage, usb)
            }
            _ => {
                let suggested = String::from(stem(&open.name));
                self.dialog = Dialog::SaveName(TextField::new(&suggested, 80));
                DocsAction::Redraw
            }
        }
    }

    fn saved(&mut self, msg: String) -> DocsAction {
        if let Some(o) = &mut self.open {
            o.ed.dirty = false;
        }
        if self.close_after_save {
            self.close_after_save = false;
            self.open = None;
        }
        DocsAction::Toast(msg)
    }

    /// Overwrite the file it came from (same format).
    fn write_in_place(&mut self, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        let open = self.open.as_mut().unwrap();
        let Origin::Src(DocSrc::Library { dir, entry }) = &open.origin else { return DocsAction::None };
        let bytes = Self::encode(&open.ed.wb, open.kind);
        match storage.save_file(usb, dir, Some(entry), &entry.name, &bytes) {
            Ok(new) => {
                kprintln!("docs: saved \"{}\" ({} bytes)", new.name, bytes.len());
                let msg = format!("Saved \"{}\"", new.name);
                open.name = new.name.clone();
                open.origin = Origin::Src(DocSrc::Library { dir: dir.clone(), entry: new });
                self.saved(msg)
            }
            Err(e) => {
                kprintln!("docs: save failed: {:?}", e);
                self.dialog = Dialog::Message(format!("Couldn't save: {}", e.message()));
                DocsAction::Redraw
            }
        }
    }

    /// Save as a new file `name` (+ extension) in `dir` (Solon's Documents folder if None).
    fn write_new(&mut self, storage: &mut Storage, usb: &mut Usb, dir: Option<DirEntry>, name: &str, kind: Kind) -> DocsAction {
        let dir = match dir.map(Ok).unwrap_or_else(|| storage.documents_dir(usb)) {
            Ok(d) => d,
            Err(e) => {
                self.dialog = Dialog::Message(format!("Couldn't save: {}", e.message()));
                return DocsAction::Redraw;
            }
        };
        let open = self.open.as_mut().unwrap();
        let file = format!("{}.{}", name, if kind == Kind::Csv { "csv" } else { "xlsx" });
        let bytes = Self::encode(&open.ed.wb, kind);
        match storage.save_file(usb, &dir, None, &file, &bytes) {
            Ok(new) => {
                kprintln!("docs: saved new \"{}\" in \"{}\" ({} bytes)", new.name, dir.name, bytes.len());
                let place = if dir.name.is_empty() { String::from("Solon storage") } else { dir.name.clone() };
                let msg = format!("Saved \"{}\" in {}", new.name, place);
                open.name = new.name.clone();
                open.kind = kind;
                open.origin = Origin::Src(DocSrc::Library { dir, entry: new });
                self.saved(msg)
            }
            Err(e) => {
                self.dialog = Dialog::Message(format!("Couldn't save: {}", e.message()));
                DocsAction::Redraw
            }
        }
    }

    pub fn start_print(&mut self, printers: Vec<PrinterInfo>) {
        let Some(open) = &mut self.open else { return };
        open.ed.finish_edit();
        if printers.is_empty() {
            self.dialog = Dialog::Message(String::from("No printer found. Connect a USB printer and try again."));
            return;
        }
        let paper = if printers[0].papers.contains(&Paper::Letter) { Paper::Letter } else { Paper::A4 };
        let color = printers[0].color;
        let mut p = PrintSetup { printers, printer: 0, paper, copies: 1, color, pages: 0 };
        p.pages = self.page_count(&p);
        self.dialog = Dialog::Print(p);
    }

    fn page_size(paper: Paper) -> (i32, i32) {
        let (w, h) = crate::print::page_pixels(paper, 100);
        (w as i32, h as i32)
    }

    fn page_count(&self, p: &PrintSetup) -> usize {
        let Some(open) = &self.open else { return 0 };
        let (w, h) = Self::page_size(p.paper);
        render_pages(&open.ed.wb, open.ed.sheet, w, h).len()
    }

    // ---- layout ---------------------------------------------------------------------

    fn new_btn(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 20 - 190, r.y + 14, 190, 38)
    }
    fn list_top(r: Rect) -> i32 {
        r.y + 66
    }
    fn row(&self, r: Rect, i: usize) -> Rect {
        // A heading row before the USB section.
        let extra = if i >= self.items.len() - self.usb_count && self.usb_count > 0 { 34 } else { 0 };
        Rect::new(r.x + 16, Self::list_top(r) + 30 + i as i32 * ROW_H + extra - self.scroll, r.w - 32, ROW_H - 4)
    }
    fn dlg(r: Rect, h: i32) -> Rect {
        let w = 560.min(r.w - 40);
        Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h)
    }
    /// Dialog buttons, rightmost first: (label, primary).
    fn dlg_buttons(&self) -> Vec<(&'static str, bool)> {
        match &self.dialog {
            Dialog::None => Vec::new(),
            Dialog::SaveName(_) => Vec::from([("Save", true), ("Cancel", false)]),
            Dialog::CsvChoice => Vec::from([("Save as Excel file", true), ("Keep as CSV", false), ("Cancel", false)]),
            Dialog::SaveChanges => Vec::from([("Save", true), ("Don't save", false), ("Cancel", false)]),
            Dialog::Print(_) => Vec::from([("Print", true), ("Cancel", false)]),
            Dialog::Message(_) => Vec::from([("OK", true)]),
        }
    }
    fn dlg_h(&self) -> i32 {
        match &self.dialog {
            Dialog::Print(_) => 330,
            Dialog::CsvChoice => 230,
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
    /// Print dialog rows: printer, paper, copies (-, +), colour.
    fn print_rows(d: Rect) -> [Rect; 5] {
        let x = d.x + 150;
        [
            Rect::new(x, d.y + 64, d.w - 170, 36),
            Rect::new(x, d.y + 110, d.w - 170, 36),
            Rect::new(x, d.y + 156, 40, 36),
            Rect::new(x + 100, d.y + 156, 40, 36),
            Rect::new(x, d.y + 202, d.w - 170, 36),
        ]
    }

    // ---- drawing ----------------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas, r: Rect) {
        if let Some(o) = &self.open {
            o.ed.draw(c, r);
        } else {
            self.draw_list(c, r);
        }
        self.draw_dialog(c, r);
    }

    fn draw_list(&self, c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(250, 250, 252));
        c.text(r.x + 20, r.y + 22, "Spreadsheets", Size::Medium, true, INK);
        button(c, Self::new_btn(r), "New spreadsheet", true);
        let top = Self::list_top(r);
        c.fill_rect(r.x, top, r.w, 1, rgb(228, 231, 236));
        if self.items.is_empty() {
            c.text_centered(r.x + r.w / 2, top + 70, "No spreadsheets yet.", Size::Medium, true, INK);
            c.text_centered(r.x + r.w / 2, top + 102, "Make one with New spreadsheet, or plug in a USB drive that has Excel or CSV files.", Size::Small, false, DIM);
            return;
        }
        let list = Rect::new(r.x, top + 1, r.w, r.y + r.h - top - 1);
        c.set_clip(list);
        let lib = self.items.len() - self.usb_count;
        let head = |c: &mut Canvas, y: i32, t: &str| c.text(r.x + 22, y, t, Size::Small, true, DIM);
        head(c, top + 8 - self.scroll, if lib > 0 { "In Solon storage" } else { "Nothing in Solon storage yet" });
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
                c.round_rect_alpha(b.x, b.y, b.w, b.h, 8, GREEN, 40);
            }
            sheet_icon(c, b.x + 10, b.y + 6);
            let size = human(it.size);
            let sw = text_width(&size, Size::Small, false);
            let pw = (b.w / 3).min(text_width(&it.place, Size::Small, false));
            let name = crate::photos::ellipsize(&it.name, b.w - 70 - sw - pw - 30, Size::Small, true);
            c.text(b.x + 48, b.y + 12, &name, Size::Small, true, INK);
            let place = crate::photos::ellipsize(&it.place, pw, Size::Small, false);
            c.text(b.x + b.w - 30 - sw - pw, b.y + 12, &place, Size::Small, false, DIM);
            c.text(b.x + b.w - 12 - sw, b.y + 12, &size, Size::Small, false, DIM);
        }
        c.set_clip(r);
    }

    fn draw_dialog(&self, c: &mut Canvas, r: Rect) {
        if matches!(self.dialog, Dialog::None) {
            return;
        }
        c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 110);
        let d = Self::dlg(r, self.dlg_h());
        c.round_rect_alpha(d.x + 3, d.y + 6, d.w, d.h, 12, 0, 90);
        c.round_rect(d.x, d.y, d.w, d.h, 12, rgb(255, 255, 255));
        let line = |c: &mut Canvas, i: i32, t: &str| c.text(d.x + 24, d.y + 62 + i * 24, t, Size::Small, false, DIM);
        match &self.dialog {
            Dialog::SaveName(f) => {
                c.text(d.x + 24, d.y + 20, "Save spreadsheet", Size::Medium, true, INK);
                line(c, 0, "Name (saved in Solon storage / Documents):");
                f.draw(c, Rect::new(d.x + 24, d.y + 92, d.w - 48, 38));
            }
            Dialog::CsvChoice => {
                c.text(d.x + 24, d.y + 20, "Save as an Excel file?", Size::Medium, true, INK);
                line(c, 0, "This is a CSV file, which can only keep the values of the");
                line(c, 1, "first sheet. Formulas, formatting and other sheets would be lost.");
                line(c, 2, "An Excel file keeps everything (the CSV file stays too).");
            }
            Dialog::SaveChanges => {
                c.text(d.x + 24, d.y + 20, "Save your changes?", Size::Medium, true, INK);
                let name = self.open.as_ref().map(|o| o.name.clone()).unwrap_or_default();
                line(c, 0, &crate::photos::ellipsize(&format!("\"{}\" has changes that aren't saved.", name), d.w - 48, Size::Small, false));
            }
            Dialog::Print(p) => {
                c.text(d.x + 24, d.y + 20, "Print", Size::Medium, true, INK);
                let pages = format!("{} page{}", p.pages, if p.pages == 1 { "" } else { "s" });
                let pw = text_width(&pages, Size::Small, false);
                c.text(d.x + d.w - 24 - pw, d.y + 26, &pages, Size::Small, false, DIM);
                let rows = Self::print_rows(d);
                let labels = ["Printer", "Paper", "Copies", "", "Colour"];
                for (i, l) in labels.iter().enumerate() {
                    if !l.is_empty() {
                        c.text(d.x + 24, rows[i].y + 9, l, Size::Small, true, INK);
                    }
                }
                let pr = &p.printers[p.printer];
                let pname = crate::photos::ellipsize(&pr.name, rows[0].w - 30, Size::Small, true);
                button(c, rows[0], &pname, false);
                button(c, rows[1], p.paper.name(), false);
                button(c, rows[2], "-", false);
                c.text_centered(rows[2].x + 70, rows[2].y + 9, &format!("{}", p.copies), Size::Small, true, INK);
                button(c, rows[3], "+", false);
                button(c, rows[4], if p.color { "Colour" } else { "Black and white" }, false);
                c.text(d.x + 24, d.y + 250, "Click a choice to change it.", Size::Small, false, DIM);
            }
            Dialog::Message(m) => {
                c.text(d.x + 24, d.y + 20, "Documents", Size::Medium, true, INK);
                let words: Vec<&str> = m.split(' ').collect();
                // Wrap onto two lines if needed.
                let mut l1 = String::new();
                let mut l2 = String::new();
                for w in words {
                    if l2.is_empty() && text_width(&format!("{} {}", l1, w), Size::Small, false) < d.w - 48 {
                        if !l1.is_empty() {
                            l1.push(' ');
                        }
                        l1 += w;
                    } else {
                        if !l2.is_empty() {
                            l2.push(' ');
                        }
                        l2 += w;
                    }
                }
                line(c, 0, &l1);
                line(c, 1, &crate::photos::ellipsize(&l2, d.w - 48, Size::Small, false));
            }
            Dialog::None => {}
        }
        for ((label, primary), b) in self.dlg_buttons().into_iter().zip(self.dlg_button_rects(d)) {
            button(c, b, label, primary);
        }
    }

    // ---- input ------------------------------------------------------------------

    fn dialog_choice(&mut self, k: usize, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        let dialog = core::mem::replace(&mut self.dialog, Dialog::None);
        match (dialog, k) {
            (Dialog::SaveName(f), 0) => {
                let name = String::from(f.text.trim());
                if name.is_empty() {
                    self.dialog = Dialog::SaveName(f);
                    return DocsAction::Redraw;
                }
                let kind = match &self.open {
                    Some(o) if o.kind == Kind::Csv && !o.ed.wb.needs_xlsx() => Kind::Csv,
                    _ => Kind::Xlsx,
                };
                self.write_new(storage, usb, None, &name, kind)
            }
            (Dialog::CsvChoice, 0) => {
                let open = self.open.as_ref().unwrap();
                let (dir, name) = match &open.origin {
                    Origin::Src(DocSrc::Library { dir, entry }) => (Some(dir.clone()), String::from(stem(&entry.name))),
                    _ => (None, String::from(stem(&open.name))),
                };
                self.write_new(storage, usb, dir, &name, Kind::Xlsx)
            }
            (Dialog::CsvChoice, 1) => self.write_in_place(storage, usb),
            (Dialog::SaveChanges, 0) => {
                self.close_after_save = true;
                let a = self.save(storage, usb);
                if !matches!(self.dialog, Dialog::None) {
                    // A follow-up question (name, CSV): closing waits for it.
                    return a;
                }
                a
            }
            (Dialog::SaveChanges, 1) => {
                self.open = None;
                self.refresh(storage, usb);
                DocsAction::Redraw
            }
            (Dialog::Print(p), 0) => {
                let Some(open) = &self.open else { return DocsAction::None };
                let (w, h) = Self::page_size(p.paper);
                let pages = render_pages(&open.ed.wb, open.ed.sheet, w, h);
                if pages.is_empty() {
                    self.dialog = Dialog::Message(String::from("This sheet is empty: there's nothing to print."));
                    return DocsAction::Redraw;
                }
                let layout = Layout { paper: p.paper, per_page: 1, fill: false, color: p.color, copies: p.copies };
                let printer = p.printers[p.printer].clone();
                DocsAction::Print { pages, layout, printer }
            }
            _ => {
                self.close_after_save = false;
                DocsAction::Redraw
            }
        }
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
            let pr = &p.printers[p.printer];
            let next = if p.paper == Paper::Letter { Paper::A4 } else { Paper::Letter };
            if pr.papers.contains(&next) {
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
        let paper = p.paper;
        let (w, h) = Self::page_size(paper);
        let n = self.open.as_ref().map(|o| render_pages(&o.ed.wb, o.ed.sheet, w, h).len()).unwrap_or(0);
        if let Dialog::Print(p) = &mut self.dialog {
            p.pages = n;
        }
        true
    }

    fn sheet_action(&mut self, a: SheetAction, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        match a {
            SheetAction::None => DocsAction::None,
            SheetAction::Redraw => DocsAction::Redraw,
            SheetAction::Save => self.save(storage, usb),
            SheetAction::Print => DocsAction::WantPrinters,
            SheetAction::Close => {
                let dirty = self.open.as_ref().is_some_and(|o| o.ed.dirty);
                if dirty {
                    self.dialog = Dialog::SaveChanges;
                } else {
                    self.open = None;
                    self.refresh(storage, usb);
                }
                DocsAction::Redraw
            }
        }
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, storage: &mut Storage, usb: &mut Usb) -> DocsAction {
        if !matches!(self.dialog, Dialog::None) {
            let d = Self::dlg(r, self.dlg_h());
            if let Some(k) = self.dlg_button_rects(d).iter().position(|b| b.contains(x, y)) {
                return self.dialog_choice(k, storage, usb);
            }
            if matches!(self.dialog, Dialog::Print(_)) && self.print_click(x, y, d) {
                return DocsAction::Redraw;
            }
            return DocsAction::None;
        }
        if let Some(o) = &mut self.open {
            let a = o.ed.on_press(x, y, r, now);
            return self.sheet_action(a, storage, usb);
        }
        if Self::new_btn(r).contains(x, y) {
            self.new_doc();
            return DocsAction::Redraw;
        }
        if y > Self::list_top(r) {
            if let Some(i) = (0..self.items.len()).find(|&i| self.row(r, i).contains(x, y)) {
                let double = matches!(self.last_click, Some((j, t)) if j == i && now - t <= DOUBLE);
                self.selected = Some(i);
                self.last_click = Some((i, now));
                if double {
                    self.last_click = None;
                    let src = self.items[i].src.clone();
                    return self.open_src(src, storage, usb);
                }
                return DocsAction::Redraw;
            }
        }
        DocsAction::None
    }

    pub fn on_drag(&mut self, x: i32, y: i32, r: Rect) -> bool {
        match &mut self.open {
            Some(o) if matches!(self.dialog, Dialog::None) => o.ed.on_drag(x, y, r),
            _ => false,
        }
    }

    pub fn on_release(&mut self) -> bool {
        match &mut self.open {
            Some(o) => o.ed.on_release(),
            None => false,
        }
    }

    pub fn scroll_info(&self, r: Rect) -> Option<(Rect, i32, i32)> {
        if self.open.is_some() || !matches!(self.dialog, Dialog::None) {
            return None;
        }
        let lib_extra = if self.usb_count > 0 { 34 } else { 0 };
        let total = self.items.len() as i32 * ROW_H + 40 + lib_extra;
        let top = Self::list_top(r);
        let max = (total - (r.y + r.h - top)).max(0);
        Some((Rect::new(r.x, top, r.w, r.y + r.h - top), max, self.scroll))
    }
    pub fn set_scroll(&mut self, v: i32) {
        self.scroll = v.max(0);
    }


    pub fn on_wheel(&mut self, delta: i8, r: Rect) -> bool {
        if let Some(o) = &mut self.open {
            return o.ed.on_wheel(delta);
        }
        let lib_extra = if self.usb_count > 0 { 34 } else { 0 };
        let total = self.items.len() as i32 * ROW_H + 40 + lib_extra;
        let max = (total - (r.y + r.h - Self::list_top(r))).max(0);
        let old = self.scroll;
        self.scroll = (self.scroll + delta as i32 * 3 * ROW_H).clamp(0, max);
        old != self.scroll
    }

    /// None = not handled (Esc on the list closes the window).
    pub fn on_key(&mut self, sc: u8, r: Rect, storage: &mut Storage, usb: &mut Usb) -> Option<DocsAction> {
        if let Dialog::SaveName(f) = &mut self.dialog {
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
                0x1C => self.dialog_choice(0, storage, usb),
                0x01 => {
                    self.dialog = Dialog::None;
                    self.close_after_save = false;
                    DocsAction::Redraw
                }
                _ => DocsAction::None,
            });
        }
        if let Some(o) = &mut self.open {
            let a = o.ed.on_key(sc, r);
            return Some(self.sheet_action(a, storage, usb));
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
                Some(i) => {
                    let src = self.items[i].src.clone();
                    Some(self.open_src(src, storage, usb))
                }
                None => Some(DocsAction::None),
            },
            0x01 => None,
            _ => Some(DocsAction::None),
        }
    }
}

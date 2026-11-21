//! Import app: browse USB drives and SD cards (read-only), view their photos, and copy
//! them into Solon's library. Also where Solon storage gets set up.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::desktop::App;
use crate::disk::human;
use crate::fs::DirEntry;
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::icons;
use crate::photos::Source;
use crate::usb::Usb;

const HEADER_H: i32 = 52;
const ROW_H: i32 = 48;
const PAD: i32 = 20;
const WHEEL_STEP: i32 = 3 * ROW_H;
const DOUBLE_CLICK_TICKS: u64 = 50;

const INK: u32 = rgb(40, 44, 52);
const INK_DIM: u32 = rgb(110, 116, 126);
const INK_FAINT: u32 = rgb(160, 166, 176);
const ACCENT: u32 = rgb(46, 134, 222);
const BUTTON: u32 = rgb(46, 134, 222);
const BUTTON_2: u32 = rgb(226, 230, 236);
const GREEN: u32 = rgb(39, 174, 96);

pub enum Action {
    None,
    Redraw,
    /// Show these photos in the Photos app (optionally opening one).
    ShowPhotos { origin: String, items: Vec<(String, Source)>, open_at: Option<usize> },
    /// Copy these files into the library.
    ImportPhotos { source: String, drive: u32, vol: usize, files: Vec<DirEntry> },
    /// Copy every photo and video on a phone that wasn't imported before.
    ImportPhone { source: String, drive: u32, vol: usize },
    /// Create Solon storage on this internal disk (user confirmed).
    SetupStorage(usize),
}

/// What the Import window needs to know about internal storage (filled in by the desktop).
pub struct StorageView {
    /// (disk name, free bytes, total bytes)
    pub library: Option<(String, u64, u64)>,
    /// (disk index, disk name, bytes available, disk is blank)
    pub options: Vec<(usize, String, u64, bool)>,
    /// Import in progress: (done, total)
    pub job: Option<(usize, usize)>,
}

struct Folder {
    drive: u32,
    vol: usize,
    label: String,
    /// Folders from the root down to the current one (root first).
    path: Vec<DirEntry>,
    entries: Vec<DirEntry>,
    /// A phone: the import button takes everything new from the whole phone.
    phone: bool,
}

pub struct Import {
    folder: Option<Folder>,
    selected: Option<usize>,
    scroll: i32,
    last_click: Option<(usize, u64)>,
    message: Option<String>,
    /// Waiting for the user to confirm storage setup: (disk, name, bytes, blank).
    confirm: Option<(usize, String, u64, bool)>,
    /// A phone folder to open on a later tick (after "Reading the phone..." is on screen),
    /// and the tick it was asked for.
    pending: Option<(DirEntry, u64)>,
}

pub fn is_photo(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".jpg") || lower.ends_with(".jpeg") || lower.ends_with(".png")
}

/// One drive/volume row on the Drives screen.
struct DriveRow {
    drive: u32,
    vol: Option<usize>,
    title: String,
    detail: String,
    phone: bool,
}

fn drive_rows(usb: &Usb) -> Vec<DriveRow> {
    let mut rows = Vec::new();
    for d in usb.drives.iter().filter(|d| !d.is_boot) {
        // (Solon's own boot stick is hidden: its library shows up as Solon storage.)
        if d.volumes.is_empty() {
            rows.push(DriveRow {
                drive: d.id,
                vol: None,
                title: d.name.clone(),
                detail: format!("{}  |  Can't read this drive's format", human(d.ms.size_bytes())),
                phone: false,
            });
        }
        for (i, v) in d.volumes.iter().enumerate() {
            let title = if v.label.is_empty() { d.name.clone() } else { format!("{}  ({})", v.label, d.name) };
            rows.push(DriveRow { drive: d.id, vol: Some(i), title, detail: format!("{}  |  {}", v.kind.name(), human(v.size)), phone: false });
        }
    }
    for p in &usb.phones {
        if p.storages.is_empty() {
            let hint = p.problem.unwrap_or("The phone isn't sharing files yet.");
            rows.push(DriveRow { drive: p.id, vol: None, title: p.name.clone(), detail: format!("{}  Then click here.", hint), phone: true });
        }
        for (i, s) in p.storages.iter().enumerate() {
            let title = if p.storages.len() == 1 { p.name.clone() } else { format!("{}  ({})", p.name, s.label) };
            let detail = format!("Phone  |  {}  |  {} free of {}", s.label, human(s.free), human(s.capacity));
            rows.push(DriveRow { drive: p.id, vol: Some(i), title, detail, phone: true });
        }
    }
    rows
}

impl Import {
    pub fn new() -> Self {
        Import { folder: None, selected: None, scroll: 0, last_click: None, message: None, confirm: None, pending: None }
    }

    fn row_count(&self, usb: &Usb) -> usize {
        match &self.folder {
            Some(f) => f.entries.len(),
            None => drive_rows(usb).len() + 1, // + the Solon storage row
        }
    }

    fn list_area(r: Rect) -> Rect {
        Rect::new(r.x, r.y + HEADER_H, r.w, r.h - HEADER_H)
    }

    /// On the Drives screen the storage row sits below the drives, after a section title.
    fn visual_index(&self, i: usize, usb: &Usb) -> usize {
        if self.folder.is_some() {
            return i;
        }
        let n = drive_rows(usb).len();
        if i < n {
            i
        } else {
            n.max(1) + 1
        }
    }

    fn row_rect(&self, i: usize, r: Rect, usb: &Usb) -> Rect {
        let v = self.visual_index(i, usb) as i32;
        Rect::new(r.x + 12, r.y + HEADER_H + 8 + v * ROW_H - self.scroll, r.w - 24, ROW_H - 4)
    }

    pub fn scroll_info(&self, r: Rect, usb: &Usb) -> Option<(Rect, i32, i32)> {
        self.confirm.is_none().then(|| (Self::list_area(r), self.max_scroll(usb, r), self.scroll))
    }
    pub fn set_scroll(&mut self, v: i32) {
        self.scroll = v.max(0);
    }


    fn max_scroll(&self, usb: &Usb, r: Rect) -> i32 {
        let rows = if self.folder.is_some() { self.row_count(usb) } else { drive_rows(usb).len().max(1) + 2 };
        (rows as i32 * ROW_H + 16 - Self::list_area(r).h).max(0)
    }

    fn back_button(r: Rect) -> Rect {
        Rect::new(r.x + 12, r.y + 10, 96, 32)
    }

    fn import_button(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 12 - 250, r.y + 10, 250, 32)
    }

    fn photos_button(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 12 - 250 - 12 - 150, r.y + 10, 150, 32)
    }

    fn dialog(r: Rect) -> Rect {
        let w = 560.min(r.w - 40);
        Rect::new(r.x + (r.w - w) / 2, r.y + r.h / 2 - 110, w, 220)
    }
    fn dialog_ok(r: Rect) -> Rect {
        let d = Self::dialog(r);
        Rect::new(d.x + d.w - 20 - 140, d.y + d.h - 56, 140, 36)
    }
    fn dialog_cancel(r: Rect) -> Rect {
        let d = Self::dialog(r);
        Rect::new(d.x + d.w - 20 - 140 - 12 - 110, d.y + d.h - 56, 110, 36)
    }

    fn photo_files(&self) -> Vec<DirEntry> {
        self.folder.as_ref().map(|f| f.entries.iter().filter(|e| !e.is_dir && is_photo(&e.name)).cloned().collect()).unwrap_or_default()
    }

    /// The drive list changed (plug/unplug).
    pub fn drives_changed(&mut self, usb: &Usb) {
        if let Some(f) = &self.folder {
            if !usb.has_drive(f.drive) {
                self.pending = None;
                self.folder = None;
                self.selected = None;
                self.scroll = 0;
                self.message = Some(String::from("The drive was removed."));
                return;
            }
        }
        if self.folder.is_none() {
            self.selected = self.selected.filter(|&i| i <= drive_rows(usb).len());
        }
    }

    pub fn set_message(&mut self, m: String) {
        self.message = Some(m);
    }

    // ---- drawing ------------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas, r: Rect, usb: &Usb, sv: &StorageView) {
        match &self.folder {
            None => self.draw_drives(c, r, usb, sv),
            Some(f) => self.draw_folder(c, r, f, sv),
        }
        if let Some((_, name, bytes, blank)) = &self.confirm {
            self.draw_confirm(c, r, name, *bytes, *blank);
        }
    }

    fn header_line(c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y + HEADER_H - 1, r.w, 1, rgb(228, 231, 236));
    }

    fn button(c: &mut Canvas, b: Rect, label: &str, primary: bool) {
        c.round_rect(b.x, b.y, b.w, b.h, 8, if primary { BUTTON } else { BUTTON_2 });
        let col = if primary { rgb(255, 255, 255) } else { INK };
        c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, col);
    }

    fn draw_drives(&self, c: &mut Canvas, r: Rect, usb: &Usb, sv: &StorageView) {
        c.text(r.x + PAD, r.y + 16, "Drives", Size::Medium, true, INK);
        let hint = "Files are read from drives and phones. Nothing on them is changed.";
        c.text(r.x + r.w - PAD - text_width(hint, Size::Small, false), r.y + 19, hint, Size::Small, false, INK_DIM);
        Self::header_line(c, r);

        let rows = drive_rows(usb);
        let area = Self::list_area(r);
        c.set_clip(area);
        if rows.is_empty() {
            let y = r.y + HEADER_H + 8 - self.scroll;
            let msg = self.message.as_deref().unwrap_or("No USB drives or SD cards detected. Plug one into this computer.");
            c.text(r.x + 24, y + 14, msg, Size::Small, false, INK_DIM);
        }
        for (i, row) in rows.iter().enumerate() {
            let rr = self.row_rect(i, r, usb);
            if self.selected == Some(i) {
                c.round_rect_alpha(rr.x, rr.y, rr.w, rr.h, 8, ACCENT, 40);
            }
            let (gx, gy) = (rr.x + 12, rr.y + 8);
            if row.phone {
                c.round_rect(gx + 5, gy, 18, 30, 4, rgb(60, 64, 72));
                c.fill_rect(gx + 8, gy + 4, 12, 20, rgb(150, 190, 240));
                c.fill_rect(gx + 12, gy + 26, 4, 2, rgb(200, 204, 210));
            } else {
                c.round_rect(gx, gy, 28, 28, 7, icons::tile_color(App::Import));
                c.round_rect(gx + 9, gy + 9, 10, 15, 2, rgb(255, 255, 255));
                c.fill_rect(gx + 10, gy + 4, 8, 5, rgb(255, 255, 255));
            }
            let color = if row.vol.is_some() { INK } else { INK_FAINT };
            c.text(rr.x + 56, rr.y + 4, &row.title, Size::Small, true, color);
            c.text(rr.x + 56, rr.y + 23, &row.detail, Size::Small, false, INK_DIM);
        }

        // Solon storage section
        let n = rows.len();
        let title_y = r.y + HEADER_H + 8 + (n.max(1) as i32) * ROW_H - self.scroll;
        c.text(r.x + PAD, title_y + 20, "This computer", Size::Small, true, INK_DIM);
        let rr = self.row_rect(n, r, usb);
        if self.selected == Some(n) {
            c.round_rect_alpha(rr.x, rr.y, rr.w, rr.h, 8, ACCENT, 40);
        }
        let (gx, gy) = (rr.x + 12, rr.y + 8);
        c.round_rect(gx, gy, 28, 28, 7, GREEN);
        c.fill_rect(gx + 6, gy + 9, 16, 4, rgb(255, 255, 255));
        c.fill_rect(gx + 6, gy + 16, 16, 4, rgb(255, 255, 255));
        match (&sv.library, sv.options.first()) {
            (Some((disk, free, size)), _) => {
                c.text(rr.x + 56, rr.y + 4, "Solon storage", Size::Small, true, INK);
                let d = format!("{}  |  {} free of {}", disk, human(*free), human(*size));
                c.text(rr.x + 56, rr.y + 23, &d, Size::Small, false, INK_DIM);
            }
            (None, Some((_, disk, bytes, _))) => {
                c.text(rr.x + 56, rr.y + 4, "Set up Solon storage", Size::Small, true, INK);
                let d = format!("Use {} of free space on {} to keep imported photos", human(*bytes), disk);
                c.text(rr.x + 56, rr.y + 23, &d, Size::Small, false, INK_DIM);
            }
            (None, None) => {
                c.text(rr.x + 56, rr.y + 4, "No Solon storage", Size::Small, true, INK_FAINT);
                c.text(rr.x + 56, rr.y + 23, "No internal disk with free space was found.", Size::Small, false, INK_DIM);
            }
        }
        c.set_clip(r);
    }

    fn draw_folder(&self, c: &mut Canvas, r: Rect, f: &Folder, sv: &StorageView) {
        let b = Self::back_button(r);
        c.round_rect(b.x, b.y, b.w, b.h, 8, BUTTON_2);
        c.triangle([(b.x + 14, b.y + 16), (b.x + 22, b.y + 10), (b.x + 22, b.y + 22)], INK);
        c.text(b.x + 30, b.y + 7, "Back", Size::Small, true, INK);

        let photos = self.photo_files().len();
        let mut right = 0;
        if photos > 0 || f.phone {
            let ib = Self::import_button(r);
            match sv.job {
                Some((done, total)) => {
                    c.round_rect(ib.x, ib.y, ib.w, ib.h, 8, BUTTON_2);
                    let w = (ib.w - 8) * done as i32 / total.max(1) as i32;
                    c.round_rect(ib.x + 4, ib.y + 4, w.max(8), ib.h - 8, 6, rgb(170, 205, 240));
                    let label = format!("Importing {} of {}", (done + 1).min(total), total);
                    c.text_centered(ib.x + ib.w / 2, ib.y + 7, &label, Size::Small, true, INK);
                }
                None => {
                    let label = if f.phone {
                        String::from("Import new photos & videos")
                    } else if photos == 1 {
                        String::from("Import 1 photo")
                    } else {
                        format!("Import {} photos", photos)
                    };
                    Self::button(c, ib, &label, sv.library.is_some());
                }
            }
            if photos > 0 {
                Self::button(c, Self::photos_button(r), "View here", false);
                right = r.x + r.w - Self::photos_button(r).x + 12;
            } else {
                right = r.x + r.w - Self::import_button(r).x + 12;
            }
        }

        let mut crumb = if f.label.is_empty() { String::from("Drive") } else { f.label.clone() };
        for d in f.path.iter().skip(1) {
            crumb.push_str(" / ");
            crumb.push_str(&d.name);
        }
        let max = r.w - (b.w + 36) - right - PAD;
        let crumb = crate::photos::ellipsize_left(&crumb, max, Size::Medium, true);
        c.text(b.x + b.w + 16, r.y + 15, &crumb, Size::Medium, true, INK);
        Self::header_line(c, r);

        let area = Self::list_area(r);
        if let Some(m) = &self.message {
            let mw = text_width(m, Size::Small, true) + 32;
            c.round_rect(r.x + (r.w - mw) / 2, r.y + r.h - 44, mw, 30, 8, rgb(255, 243, 205));
            c.text_centered(r.x + r.w / 2, r.y + r.h - 37, m, Size::Small, true, rgb(120, 90, 0));
        }
        if f.entries.is_empty() {
            c.text_centered(r.x + r.w / 2, r.y + r.h / 2 - 10, "This folder is empty.", Size::Medium, true, INK_DIM);
            return;
        }
        c.set_clip(Rect::new(area.x, area.y, area.w, if self.message.is_some() { area.h - 52 } else { area.h }));
        for (i, e) in f.entries.iter().enumerate() {
            let rr = Rect::new(r.x + 12, r.y + HEADER_H + 8 + i as i32 * ROW_H - self.scroll, r.w - 24, ROW_H - 4);
            if rr.y + rr.h < area.y || rr.y > area.y + area.h {
                continue;
            }
            if self.selected == Some(i) {
                c.round_rect_alpha(rr.x, rr.y, rr.w, rr.h, 8, ACCENT, 40);
            }
            let (gx, gy) = (rr.x + 12, rr.y + 9);
            let photo = !e.is_dir && is_photo(&e.name);
            if e.is_dir {
                c.round_rect(gx, gy + 2, 13, 8, 2, rgb(240, 180, 40));
                c.round_rect(gx, gy + 6, 28, 20, 3, rgb(250, 196, 60));
            } else if photo {
                c.round_rect(gx + 2, gy, 24, 26, 4, icons::tile_color(App::Photos));
                c.triangle([(gx + 6, gy + 21), (gx + 12, gy + 12), (gx + 18, gy + 21)], rgb(255, 255, 255));
                c.circle(gx + 19, gy + 8, 2, rgb(255, 255, 255));
            } else {
                c.fill_rect(gx + 4, gy, 20, 26, rgb(220, 224, 230));
                for k in 0..3 {
                    c.fill_rect(gx + 8, gy + 8 + k * 5, 12, 2, rgb(180, 186, 196));
                }
            }
            let color = if e.is_dir || photo { INK } else { INK_FAINT };
            let size_text = if e.is_dir { String::new() } else { human(e.size) };
            let sw = text_width(&size_text, Size::Small, false);
            let name = crate::photos::ellipsize(&e.name, rr.w - 72 - sw - 24, Size::Small, e.is_dir);
            c.text(rr.x + 56, rr.y + 13, &name, Size::Small, e.is_dir, color);
            c.text(rr.x + rr.w - 16 - sw, rr.y + 13, &size_text, Size::Small, false, INK_DIM);
        }
        let max = (f.entries.len() as i32 * ROW_H + 16 - area.h).max(0);
        if max > 0 {
            let track = area.h - 16;
            let th = (track * area.h / (area.h + max)).max(30);
            let y = area.y + 8 + (track - th) * self.scroll / max;
            c.round_rect_alpha(r.x + r.w - 10, y, 6, th, 3, 0, 70);
        }
        c.set_clip(r);
    }

    fn draw_confirm(&self, c: &mut Canvas, r: Rect, disk: &str, bytes: u64, blank: bool) {
        c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 110);
        let d = Self::dialog(r);
        c.round_rect_alpha(d.x + 3, d.y + 6, d.w, d.h, 12, 0, 80);
        c.round_rect(d.x, d.y, d.w, d.h, 12, rgb(255, 255, 255));
        c.text(d.x + 24, d.y + 22, "Set up Solon storage?", Size::Medium, true, INK);
        let l1 = format!("Solon will create a {} partition named \"Solon Library\"", human(bytes));
        let l2 = if blank { format!("on {}, which is empty.", disk) } else { format!("in the free space on {}.", disk) };
        c.text(d.x + 24, d.y + 62, &l1, Size::Small, false, INK);
        c.text(d.x + 24, d.y + 82, &l2, Size::Small, false, INK);
        c.text(d.x + 24, d.y + 112, "Existing partitions and their files are not changed.", Size::Small, true, GREEN);
        Self::button(c, Self::dialog_cancel(r), "Cancel", false);
        Self::button(c, Self::dialog_ok(r), "Set up", true);
    }

    // ---- actions ------------------------------------------------------------

    fn open_drive(&mut self, usb: &mut Usb, drive: u32, vol: usize) {
        let Some(root) = usb.source_root(drive, vol) else { return };
        let label = usb.sources().into_iter().find(|s| s.drive == drive && s.vol == vol).map(|s| s.label).unwrap_or_default();
        let phone = usb.is_phone(drive);
        self.folder = Some(Folder { drive, vol, label, path: Vec::new(), entries: Vec::new(), phone });
        self.enter(usb, root);
    }

    /// Open folder `dir`. Phones can take a while to answer, so for them the folder is
    /// opened on a later tick, with a message on screen meanwhile.
    fn enter(&mut self, usb: &mut Usb, dir: DirEntry) {
        if self.folder.as_ref().is_some_and(|f| f.phone && !f.path.is_empty()) {
            self.message = Some(String::from("Reading the phone... this can take a moment"));
            self.pending = Some((dir, crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed)));
            return;
        }
        self.enter_now(usb, dir);
    }

    /// True while a phone folder is waiting to be read.
    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    /// Read a waiting phone folder (once the message has been drawn). True = redraw.
    pub fn step(&mut self, usb: &mut Usb, now: u64) -> bool {
        match &self.pending {
            Some((_, asked)) if now >= asked + 3 => {
                let (dir, _) = self.pending.take().unwrap();
                self.enter_now(usb, dir);
                true
            }
            _ => false,
        }
    }

    fn enter_now(&mut self, usb: &mut Usb, dir: DirEntry) {
        let Some(f) = &mut self.folder else { return };
        self.message = None;
        match usb.list(f.drive, f.vol, &dir) {
            Ok(entries) => {
                kprintln!("solon: import list \"{}\": {} entries", dir.name, entries.len());
                f.entries = entries;
                f.path.push(dir);
            }
            Err(e) => {
                kprintln!("solon: import list failed: {:?}", e);
                self.message = Some(String::from(e.message()));
                if f.path.is_empty() {
                    f.entries.clear();
                    f.path.push(dir);
                }
            }
        }
        self.selected = None;
        self.scroll = 0;
        self.last_click = None;
    }

    fn back(&mut self, usb: &mut Usb) {
        if self.pending.is_some() {
            return;
        }
        let Some(f) = &mut self.folder else { return };
        if f.path.len() <= 1 {
            self.folder = None;
            self.selected = None;
            self.scroll = 0;
            self.message = None;
            return;
        }
        f.path.pop();
        let parent = f.path.pop().unwrap();
        self.enter(usb, parent);
    }

    fn origin(f: &Folder) -> String {
        let mut origin = if f.label.is_empty() { String::from("USB drive") } else { f.label.clone() };
        for d in f.path.iter().skip(1) {
            origin.push_str(" / ");
            origin.push_str(&d.name);
        }
        origin
    }

    fn photos_here(&self, open_name: Option<&str>) -> Action {
        let Some(f) = &self.folder else { return Action::None };
        let photos = self.photo_files();
        if photos.is_empty() {
            return Action::None;
        }
        let open_at = open_name.and_then(|n| photos.iter().position(|e| e.name == n));
        let items = photos
            .into_iter()
            .map(|e| (e.name.clone(), Source::Usb { drive: f.drive, vol: f.vol, entry: e }))
            .collect();
        Action::ShowPhotos { origin: Self::origin(f), items, open_at }
    }

    fn import_here(&mut self, sv: &StorageView) -> Action {
        if sv.job.is_some() {
            return Action::None;
        }
        if sv.library.is_none() {
            self.message = Some(String::from("Set up Solon storage first (Back, then \"Set up Solon storage\")."));
            return Action::Redraw;
        }
        let Some(f) = &self.folder else { return Action::None };
        let files = self.photo_files();
        if files.is_empty() {
            return Action::None;
        }
        Action::ImportPhotos { source: f.label.clone(), drive: f.drive, vol: f.vol, files }
    }

    fn import_phone(&mut self, sv: &StorageView) -> Action {
        if sv.job.is_some() {
            return Action::None;
        }
        if sv.library.is_none() {
            self.message = Some(String::from("Set up Solon storage first (Back, then \"Set up Solon storage\")."));
            return Action::Redraw;
        }
        let Some(f) = &self.folder else { return Action::None };
        Action::ImportPhone { source: f.label.clone(), drive: f.drive, vol: f.vol }
    }

    fn activate(&mut self, usb: &mut Usb, sv: &StorageView, i: usize) -> Action {
        match &self.folder {
            None => {
                let rows = drive_rows(usb);
                if let Some(row) = rows.get(i) {
                    if let Some(vol) = row.vol {
                        self.open_drive(usb, row.drive, vol);
                    } else if row.phone {
                        // Ask the phone again (the user may have unlocked it by now).
                        usb.retry_phones();
                        self.message = usb.phones.iter().find(|p| p.id == row.drive).and_then(|p| p.problem).map(String::from);
                    } else {
                        self.message = Some(String::from("Solon can't read this drive's format."));
                    }
                } else if i == rows.len() && sv.library.is_none() {
                    if let Some(o) = sv.options.first() {
                        self.confirm = Some(o.clone());
                    }
                }
                Action::Redraw
            }
            Some(_) if self.pending.is_some() => Action::None,
            Some(f) => {
                let Some(e) = f.entries.get(i).cloned() else { return Action::None };
                if e.is_dir {
                    self.enter(usb, e);
                    Action::Redraw
                } else if is_photo(&e.name) {
                    self.photos_here(Some(&e.name))
                } else {
                    Action::None // videos and documents come in later milestones
                }
            }
        }
    }

    fn confirm_action(&mut self) -> Action {
        match self.confirm.take() {
            Some((disk, ..)) => Action::SetupStorage(disk),
            None => Action::None,
        }
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64, usb: &mut Usb, sv: &StorageView) -> Action {
        if self.confirm.is_some() {
            if Self::dialog_ok(r).contains(x, y) {
                return self.confirm_action();
            }
            if Self::dialog_cancel(r).contains(x, y) || !Self::dialog(r).contains(x, y) {
                self.confirm = None;
                return Action::Redraw;
            }
            return Action::None;
        }
        if self.folder.is_some() {
            if Self::back_button(r).contains(x, y) {
                self.back(usb);
                return Action::Redraw;
            }
            let phone = self.folder.as_ref().is_some_and(|f| f.phone);
            if phone && Self::import_button(r).contains(x, y) {
                return self.import_phone(sv);
            }
            if !self.photo_files().is_empty() {
                if Self::import_button(r).contains(x, y) {
                    return self.import_here(sv);
                }
                if Self::photos_button(r).contains(x, y) {
                    return self.photos_here(None);
                }
            }
        }
        if !Self::list_area(r).contains(x, y) {
            return Action::None;
        }
        let n = self.row_count(usb);
        let hit = (0..n).find(|&i| match &self.folder {
            Some(_) => Rect::new(r.x + 12, r.y + HEADER_H + 8 + i as i32 * ROW_H - self.scroll, r.w - 24, ROW_H - 4).contains(x, y),
            None => self.row_rect(i, r, usb).contains(x, y),
        });
        match hit {
            Some(i) => {
                let double = matches!(self.last_click, Some((j, t)) if j == i && now - t <= DOUBLE_CLICK_TICKS);
                self.selected = Some(i);
                self.last_click = Some((i, now));
                if double {
                    self.last_click = None;
                    return match self.activate(usb, sv, i) {
                        Action::None => Action::Redraw,
                        a => a,
                    };
                }
                Action::Redraw
            }
            None => {
                self.last_click = None;
                if self.selected.take().is_some() {
                    Action::Redraw
                } else {
                    Action::None
                }
            }
        }
    }

    pub fn on_wheel(&mut self, delta: i8, r: Rect, usb: &Usb) -> bool {
        let old = self.scroll;
        self.scroll = (self.scroll + delta as i32 * WHEEL_STEP).clamp(0, self.max_scroll(usb, r));
        old != self.scroll
    }

    /// None = not handled (let the desktop close the window on Esc).
    pub fn on_key(&mut self, sc: u8, r: Rect, usb: &mut Usb, sv: &StorageView) -> Option<Action> {
        if sc & 0x80 != 0 {
            return Some(Action::None);
        }
        if self.confirm.is_some() {
            return Some(match sc {
                0x01 => {
                    self.confirm = None;
                    Action::Redraw
                }
                0x1C => self.confirm_action(),
                _ => Action::None,
            });
        }
        let n = self.row_count(usb);
        match sc {
            0x48 | 0x50 if n > 0 => {
                let next = match (self.selected, sc) {
                    (None, _) => 0,
                    (Some(i), 0x48) => i.saturating_sub(1),
                    (Some(i), _) => (i + 1).min(n - 1),
                };
                self.selected = Some(next);
                let rr = self.row_rect(next, r, usb);
                let area = Self::list_area(r);
                if rr.y < area.y {
                    self.scroll -= area.y - rr.y + 8;
                } else if rr.y + rr.h > area.y + area.h {
                    self.scroll += rr.y + rr.h - (area.y + area.h) + 8;
                }
                self.scroll = self.scroll.clamp(0, self.max_scroll(usb, r));
                Some(Action::Redraw)
            }
            0x1C => Some(match self.selected {
                Some(i) => match self.activate(usb, sv, i) {
                    Action::None => Action::Redraw,
                    a => a,
                },
                None => Action::None,
            }),
            0x17 if self.folder.is_some() => Some(self.import_here(sv)), // I = import
            0x0E if self.folder.is_some() => {
                self.back(usb);
                Some(Action::Redraw)
            }
            0x01 => None,
            _ => Some(Action::None),
        }
    }
}

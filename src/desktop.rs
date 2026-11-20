//! The Solon desktop: top bar, app icons, and a single movable app window.
//! There are deliberately no login, account or download features.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::icons;
use crate::fs::{DirEntry, FsError};
use crate::import::{Action, Import, StorageView};
use crate::library::{unique_name, SaveResult, Storage};
use crate::interrupts::HZ;
use crate::edit::Edits;
use crate::print::{self, Layout, PrinterId, PrinterInfo};
use crate::editor::SaveChoice;
use crate::photos::{Bytes, PhotoAction, Photos, Source};
use crate::settings::{self, Settings};
use crate::settingsui::{SettingsAction, SettingsApp};
use crate::files::{CopyJob, Files, FilesAction};
use crate::diskmgr::{DiskAction, DiskMgr};
use crate::usb::Usb;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum App {
    Photos,
    Videos,
    Documents,
    Printers,
    Import,
    Files,
    Disks,
    Settings,
}

impl App {
    pub fn name(self) -> &'static str {
        match self {
            App::Photos => "Photos",
            App::Videos => "Videos",
            App::Documents => "Documents",
            App::Printers => "Printers",
            App::Import => "Import",
            App::Files => "Files",
            App::Disks => "Disks",
            App::Settings => "Settings",
        }
    }

    fn placeholder(self) -> [&'static str; 2] {
        match self {
            App::Photos => ["No photos yet.", "Plug in a USB drive or SD card, then open Import."],
            App::Videos => ["No videos yet.", "Plug in a USB drive or SD card, then open Import."],
            App::Documents => ["No documents yet.", "Plug in a USB drive or SD card, then open Import."],
            App::Printers => ["No printers found.", "Connect a USB printer to this computer."],
            App::Import => ["No USB drives or cameras detected.", "USB support arrives in an upcoming milestone."],
            App::Settings => ["", ""],
            App::Files | App::Disks => ["", ""],
        }
    }
}

const BAR_H: i32 = 32;
const CELL_W: i32 = 112;
const CELL_H: i32 = 112;
const ICON: i32 = 64;
const TITLE_H: i32 = 36;
const WIN_LEFT: i32 = 152; // windows open to the right of the icon column
const DOUBLE_CLICK_TICKS: u64 = 50; // 0.5 s at 100 Hz

const TEXT: u32 = rgb(240, 242, 245);
const TEXT_DIM: u32 = rgb(170, 180, 190);
const WIN_BG: u32 = rgb(250, 250, 252);

struct Icon {
    app: App,
    x: i32,
    y: i32,
    /// Height of its clickable cell (icons squeeze together on short screens).
    h: i32,
}

struct Window {
    app: App,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
}

impl Window {
    fn rect(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, self.h)
    }
    fn title_bar(&self) -> Rect {
        Rect::new(self.x, self.y, self.w, TITLE_H)
    }
    fn close_box(&self) -> Rect {
        Rect::new(self.x + self.w - TITLE_H, self.y, TITLE_H, TITLE_H)
    }
    /// The area the app draws into.
    fn body(&self) -> Rect {
        Rect::new(self.x, self.y + TITLE_H + 1, self.w, self.h - TITLE_H - 1)
    }
}

pub struct Desktop {
    w: i32,
    h: i32,
    icons: Vec<Icon>,
    selected: Option<usize>,
    window: Option<Window>,
    drag: Option<(i32, i32)>,
    clock: (u8, u8),
    last_click: Option<(usize, u64)>,
    photos: Photos,
    import: Import,
    usb: Usb,
    /// Short notice in the top bar, with the tick it disappears at.
    toast: Option<(String, u64)>,
    drive_ids: Vec<u32>,
    storage: Storage,
    /// Cached storage status for the Import window (disk scans are slow-ish).
    storage_view: StorageView,
    job: Option<ImportJob>,
    /// The mouse was pressed inside the Photos window; send it moves until release.
    capture: bool,
    /// F12 diagnostics overlay.
    show_log: bool,
    test_printer: Option<PrinterInfo>,
    pending_print: Option<(Vec<Source>, Layout, PrinterInfo)>,
    settings: Settings,
    settings_app: SettingsApp,
    /// Physical screen size (the desktop itself may be drawn smaller and enlarged).
    screen: (usize, usize),
    date: (u16, u8, u8),
    power_menu: bool,
    confirm: Option<PowerRequest>,
    screen_off: bool,
    files: Files,
    disks: DiskMgr,
    copy: Option<CopyJob>,
    request: Option<PowerRequest>,
}

/// Things only the main loop can do (it owns the screen).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PowerRequest {
    /// Text size changed: redo the display scaling.
    Rescale,
    Restart,
    ShutDown,
}

/// Copying photos from a USB drive into the library, one file per tick.
struct ImportJob {
    drive: u32,
    vol: usize,
    dest: DirEntry,
    files: Vec<DirEntry>,
    /// What the destination folder already holds: (name, size).
    existing: Vec<(String, u64)>,
    done: usize,
    saved: usize,
    skipped: usize,
    failed: usize,
}

impl Desktop {
    /// Library photos as Photos-app items.
    fn library_items(storage: &mut Storage, usb: &mut Usb) -> Vec<(String, Source)> {
        storage.all_photos(usb).into_iter().map(|(dir, e)| (e.name.clone(), Source::Library { dir, entry: e })).collect()
    }

    fn refresh_storage_view(&mut self) {
        let library = self.storage.library.as_ref().map(|l| (l.disk_name.clone(), 0, 0));
        let library = library.map(|(n, _, _)| (n, self.storage.free_bytes(), self.storage.size()));
        let options = if library.is_none() {
            self.storage.setup_options().into_iter().map(|o| (o.disk, o.disk_name, o.bytes, o.blank)).collect()
        } else {
            Vec::new()
        };
        self.storage_view = StorageView { library, options, job: self.job.as_ref().map(|j| (j.done, j.files.len())) };
    }

    /// Show the library in Photos (used at boot and after imports).
    pub fn show_library(&mut self) -> bool {
        let items = Self::library_items(&mut self.storage, &mut self.usb);
        if items.is_empty() {
            return false;
        }
        self.photos.set(String::from("Solon library"), items, None);
        true
    }

    fn layout_icons(w: i32, h: i32) -> Vec<Icon> {
        let _ = w;
        let apps = [App::Photos, App::Videos, App::Documents, App::Printers, App::Import, App::Files, App::Disks, App::Settings];
        // Leave room under the last icon for its name.
        let top = BAR_H + 12;
        let step = ((h - top - 100) / (apps.len() as i32 - 1)).min(CELL_H);
        apps.iter().enumerate().map(|(i, &app)| Icon { app, x: 24, y: top + i as i32 * step, h: step }).collect()
    }

    /// `w`, `h`: the (possibly scaled-down) desktop size; `screen`: the physical screen size.
    pub fn new(w: usize, h: usize, photos: Photos, usb: Usb, storage: Storage, settings: Settings, screen: (usize, usize)) -> Self {
        let icons = Self::layout_icons(w as i32, h as i32);
        let mut d = Self {
            w: w as i32,
            h: h as i32,
            icons,
            selected: None,
            window: None,
            drag: None,
            clock: (0, 0),
            last_click: None,
            photos,
            import: Import::new(),
            drive_ids: usb.drives.iter().map(|d| d.id).collect(),
            usb,
            toast: None,
            storage,
            storage_view: StorageView { library: None, options: Vec::new(), job: None },
            job: None,
            capture: false,
            show_log: false,
            test_printer: None,
            pending_print: None,
            settings,
            settings_app: SettingsApp::new(),
            screen,
            date: crate::rtc::date(),
            power_menu: false,
            confirm: None,
            screen_off: false,
            files: Files::new(),
            disks: DiskMgr::new(),
            copy: None,
            request: None,
        };
        d.refresh_storage_view();
        d
    }

    // ---- settings and power ------------------------------------------------------------

    pub fn settings(&self) -> Settings {
        self.settings
    }

    /// Something only the main loop can do (rescale the display, restart, shut down).
    pub fn take_request(&mut self) -> Option<PowerRequest> {
        self.request.take()
    }

    /// The desktop is now drawn at a new (scaled) size.
    pub fn resize(&mut self, w: usize, h: usize) {
        self.w = w as i32;
        self.h = h as i32;
        self.icons = Self::layout_icons(self.w, self.h);
        if let Some(app) = self.window.as_ref().map(|win| win.app) {
            self.open(app); // re-fit the window
        }
    }

    fn apply_settings(&mut self, n: Settings) {
        let old = self.settings;
        self.settings = n;
        crate::ps2::set_speed(settings::MOUSE[n.mouse].1);
        if n.size != old.size {
            self.request = Some(PowerRequest::Rescale);
        }
        kprintln!("settings: {:?}", n);
        if self.storage.library.is_some() {
            if let Err(e) = self.storage.write_root_file(&mut self.usb, "Settings.txt", n.to_text().as_bytes()) {
                kprintln!("settings: couldn't save: {:?}", e);
            }
        }
    }

    fn power_button(&self) -> Rect {
        Rect::new(self.w - 44, 0, 44, BAR_H)
    }

    fn power_item(&self, i: i32) -> Rect {
        Rect::new(self.w - 236, BAR_H + 10 + i * 46, 220, 42)
    }

    fn confirm_box(&self) -> Rect {
        Rect::new(self.w / 2 - 230, self.h / 2 - 100, 460, 200)
    }
    fn confirm_ok(&self) -> Rect {
        let b = self.confirm_box();
        Rect::new(b.x + b.w - 24 - 150, b.y + b.h - 60, 150, 40)
    }
    fn confirm_cancel(&self) -> Rect {
        let b = self.confirm_box();
        Rect::new(b.x + b.w - 24 - 150 - 12 - 110, b.y + b.h - 60, 110, 40)
    }

    fn draw_power(&self, c: &mut Canvas) {
        // The button: a classic power symbol.
        let b = self.power_button();
        let (cx, cy) = (b.x + b.w / 2, b.y + b.h / 2 + 1);
        if self.power_menu {
            c.fill_rect_alpha(b.x, b.y, b.w, b.h, rgb(255, 255, 255), 40);
        }
        c.circle(cx, cy, 9, TEXT);
        c.circle(cx, cy, 7, rgb(20, 24, 30));
        c.fill_rect(cx - 3, cy - 10, 6, 6, rgb(20, 24, 30));
        c.fill_rect(cx - 1, cy - 11, 2, 9, TEXT);

        if self.power_menu {
            let top = self.power_item(0);
            c.round_rect_alpha(top.x - 6 + 3, top.y - 6 + 5, top.w + 12, 3 * 46 + 8, 10, 0, 80);
            c.round_rect(top.x - 6, top.y - 6, top.w + 12, 3 * 46 + 8, 10, WIN_BG);
            for (i, label) in ["Turn off screen", "Restart", "Shut down"].iter().enumerate() {
                let r = self.power_item(i as i32);
                c.round_rect(r.x, r.y, r.w, r.h, 8, rgb(236, 239, 243));
                c.text(r.x + 16, r.y + 11, label, Size::Small, true, rgb(40, 44, 52));
            }
        }
        if let Some(req) = self.confirm {
            c.fill_rect_alpha(0, 0, self.w, self.h, 0, 120);
            let b = self.confirm_box();
            c.round_rect_alpha(b.x + 3, b.y + 6, b.w, b.h, 12, 0, 90);
            c.round_rect(b.x, b.y, b.w, b.h, 12, WIN_BG);
            let (title, line, ok) = match req {
                PowerRequest::Restart => ("Restart the computer?", "Your photos are saved. Solon will start again.", "Restart"),
                _ => ("Shut down the computer?", "Your photos are saved. The computer will turn off.", "Shut down"),
            };
            c.text(b.x + 24, b.y + 24, title, Size::Medium, true, rgb(40, 44, 52));
            c.text(b.x + 24, b.y + 66, line, Size::Small, false, rgb(100, 106, 116));
            let cb = self.confirm_cancel();
            c.round_rect(cb.x, cb.y, cb.w, cb.h, 8, rgb(226, 230, 236));
            c.text_centered(cb.x + cb.w / 2, cb.y + 11, "Cancel", Size::Small, true, rgb(40, 44, 52));
            let ob = self.confirm_ok();
            c.round_rect(ob.x, ob.y, ob.w, ob.h, 8, rgb(210, 70, 60));
            c.text_centered(ob.x + ob.w / 2, ob.y + 11, ok, Size::Small, true, rgb(255, 255, 255));
        }
    }

    /// Clicks on the power button, its menu or the confirmation. None = not for us.
    fn power_click(&mut self, x: i32, y: i32) -> Option<bool> {
        if let Some(req) = self.confirm {
            if self.confirm_ok().contains(x, y) {
                self.confirm = None;
                kprintln!("power: {:?} confirmed", req);
                self.request = Some(req);
            } else if self.confirm_cancel().contains(x, y) || !self.confirm_box().contains(x, y) {
                self.confirm = None;
            }
            return Some(true);
        }
        if self.power_menu {
            self.power_menu = false;
            match (0..3).find(|&i| self.power_item(i).contains(x, y)) {
                Some(0) => {
                    kprintln!("power: screen off");
                    self.screen_off = true;
                }
                Some(1) => self.confirm = Some(PowerRequest::Restart),
                Some(2) => self.confirm = Some(PowerRequest::ShutDown),
                _ => {}
            }
            return Some(true);
        }
        if self.power_button().contains(x, y) {
            self.power_menu = true;
            return Some(true);
        }
        None
    }

    pub fn screen_off(&self) -> bool {
        self.screen_off
    }

    pub fn set_date(&mut self, d: (u16, u8, u8)) -> bool {
        let changed = self.date != d;
        self.date = d;
        changed
    }

    /// True while there's background work (photos to decode, an import), so the main loop shouldn't sleep.
    pub fn busy(&self) -> bool {
        self.photos.needs_loading() || self.job.is_some() || self.copy.is_some() || self.disks.busy()
    }

    fn step_import(&mut self, now: u64) -> bool {
        let Some(job) = &mut self.job else { return false };
        if job.done < job.files.len() {
            let f = job.files[job.done].clone();
            let r = match unique_name(&job.existing, &f.name, f.size) {
                None => Ok(SaveResult::AlreadyThere),
                Some(name) => self.usb.read_file(job.drive, job.vol, &f).and_then(|data| {
                    let e = self.storage.create_in(&mut self.usb, &job.dest, &name, &data)?;
                    job.existing.push((name, data.len() as u64));
                    Ok(SaveResult::Saved(e))
                }),
            };
            match r {
                Ok(SaveResult::Saved(_)) => job.saved += 1,
                Ok(SaveResult::AlreadyThere) => job.skipped += 1,
                Err(e) => {
                    kprintln!("solon: import {} failed: {:?}", f.name, e);
                    job.failed += 1;
                    if matches!(e, FsError::Full) {
                        job.failed += job.files.len() - job.done - 1;
                        job.done = job.files.len() - 1;
                    }
                }
            }
            job.done += 1;
        }
        if job.done >= job.files.len() {
            let (saved, skipped, failed) = (job.saved, job.skipped, job.failed);
            kprintln!("solon: import finished: {} saved, {} already there, {} failed", saved, skipped, failed);
            let mut msg = format!("Imported {} photo{}", saved, if saved == 1 { "" } else { "s" });
            if skipped > 0 {
                msg += &format!(", {} already in Solon", skipped);
            }
            if failed > 0 {
                msg += &format!(", {} failed", failed);
            }
            self.toast = Some((msg.clone(), now + 6 * HZ));
            self.import.set_message(msg);
            self.job = None;
            if self.photos.origin() == "Solon library" || self.photos.origin().is_empty() || self.photos.origin() == "Boot drive" {
                self.show_library();
            }
        }
        self.refresh_storage_view();
        true
    }

    /// Periodic work: USB plug/unplug and input, loading the next photo. Returns true to redraw.
    pub fn tick(&mut self, now: u64) -> bool {
        let mut redraw = false;
        if self.usb.poll() {
            let ids: Vec<u32> = self.usb.drives.iter().map(|d| d.id).collect();
            if let Some(d) = self.usb.drives.iter().find(|d| !self.drive_ids.contains(&d.id)) {
                let name = d.volumes.first().map(|v| v.label.clone()).filter(|l| !l.is_empty()).unwrap_or(d.name.clone());
                self.toast = Some((format!("{} connected - open Import to see it", name), now + 6 * HZ));
            } else if ids.len() < self.drive_ids.len() {
                self.toast = Some((String::from("Drive removed"), now + 4 * HZ));
            }
            self.drive_ids = ids;
            self.import.drives_changed(&self.usb);
            self.files.drives_changed(&mut self.storage, &mut self.usb);
            if matches!(&self.window, Some(w) if w.app == App::Disks) {
                self.disks.refresh(&mut self.storage, &mut self.usb);
            }
            let usb = &self.usb;
            self.photos.drive_removed(&|id| !usb.has_drive(id));
            redraw = true;
        }
        if matches!(&self.toast, Some((_, end)) if now >= *end) {
            self.toast = None;
            redraw = true;
        }
        if let Some((sources, layout, printer)) = self.pending_print.take() {
            let r = self.run_print(sources, layout, printer);
            match r {
                Ok(msg) => self.toast = Some((msg, now + 6 * HZ)),
                Err(e) => {
                    kprintln!("print: failed: {}", e);
                    self.toast = Some((format!("Printing failed: {}", e), now + 8 * HZ));
                }
            }
            return true;
        }
        if let Some(a) = self.disks.step(&mut self.storage, &mut self.usb) {
            self.disk_action(a, now);
            return true;
        }
        if self.copy.is_some() {
            redraw |= self.step_copy(now);
        } else         if self.job.is_some() {
            redraw |= self.step_import(now);
        } else if self.photos.needs_loading() {
            let usb = &mut self.usb;
            let storage = &mut self.storage;
            redraw |= self.photos.load_next(&mut |src: &Source| match src {
                Source::Memory(b) => Ok(Bytes::Static(b)),
                Source::Usb { drive, vol, entry } => {
                    usb.read_file(*drive, *vol, entry).map(Bytes::Owned).map_err(|e| e.message())
                }
                Source::Library { entry, .. } => storage.read_file(usb, entry).map(Bytes::Owned).map_err(|e| e.message()),
            });
        }
        redraw
    }

    /// F12 diagnostics: the kernel log, for reporting problems on real PCs.
    fn draw_log(&self, c: &mut Canvas) {
        let (w, h) = (self.w, self.h);
        c.fill_rect_alpha(0, 0, w, h, rgb(10, 12, 16), 235);
        c.text(24, 16, "Solon diagnostics", Size::Medium, true, rgb(255, 255, 255));
        let info = format!("Screen {}x{}   |   F12 or Esc to close", w, h);
        c.text(24, 44, &info, Size::Small, false, rgb(160, 170, 180));
        let max = ((h - 90) / 18).max(1) as usize;
        let mut y = 76;
        crate::serial::LOG.lock().last_lines(max, |line| {
            let s = core::str::from_utf8(line).unwrap_or("?");
            let color = if s.contains("fail") || s.contains("error") || s.contains("FAIL") || s.contains("PANIC") {
                rgb(255, 140, 120)
            } else {
                rgb(210, 220, 230)
            };
            c.text(24, y, s, Size::Small, false, color);
            y += 18;
        });
    }

    fn fetch(&mut self, src: &Source) -> Result<Bytes, &'static str> {
        match src {
            Source::Memory(b) => Ok(Bytes::Static(b)),
            Source::Usb { drive, vol, entry } => self.usb.read_file(*drive, *vol, entry).map(Bytes::Owned).map_err(|e| e.message()),
            Source::Library { entry, .. } => self.storage.read_file(&mut self.usb, entry).map(Bytes::Owned).map_err(|e| e.message()),
        }
    }

    fn now() -> u64 {
        crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed)
    }

    /// Carry out what the Photos app asked for (editing needs storage, which we own).
    fn photo_action(&mut self) -> bool {
        let Some(a) = self.photos.take_action() else { return false };
        match a {
            PhotoAction::Edit(i) => {
                let can_save = self.storage.library.is_some();
                let (can_replace, has_original) = match self.photos.source(i).cloned() {
                    Some(Source::Library { dir, entry }) => (true, self.storage.backup_of(&mut self.usb, &dir, &entry.name).is_some()),
                    _ => (false, false),
                };
                self.photos.start_edit(i, can_save, can_save && can_replace, has_original);
            }
            PhotoAction::Save(i, choice, edits) => {
                if let Err(e) = self.save_edit(i, choice, edits) {
                    kprintln!("editor: save failed: {}", e);
                    self.toast = Some((format!("Couldn't save: {}", e), Self::now() + 6 * HZ));
                }
            }
            PhotoAction::Print(i) => {
                let printers = self.printers();
                kprintln!("print: dialog opened, {} printer(s)", printers.len());
                self.photos.start_print(i, printers);
            }
            PhotoAction::DoPrint { indices, layout, printer } => {
                let sources: Vec<Source> = indices.iter().filter_map(|&i| self.photos.source(i).cloned()).collect();
                // Show the notice first; the job runs on the next tick.
                self.toast = Some((format!("Preparing {} page(s) for {}...", (sources.len() + layout.per_page as usize - 1) / layout.per_page as usize, printer.name), Self::now() + 60 * HZ));
                self.pending_print = Some((sources, layout, printer));
            }
            PhotoAction::Revert(i) => {
                let r = match self.photos.source(i).cloned() {
                    Some(Source::Library { dir, entry }) => self.storage.revert(&mut self.usb, &dir, &entry).map(|e| (dir, e)),
                    _ => Err(FsError::ReadOnly),
                };
                match r {
                    Ok((dir, e)) => {
                        kprintln!("editor: reverted to \"{}\"", e.name);
                        self.photos.end_edit();
                        self.photos.replace_item(i, e.name.clone(), Source::Library { dir, entry: e });
                        self.toast = Some((String::from("Original restored"), Self::now() + 5 * HZ));
                    }
                    Err(e) => self.toast = Some((format!("Couldn't revert: {}", e.message()), Self::now() + 6 * HZ)),
                }
                self.refresh_storage_view();
            }
        }
        true
    }

    /// Printers the user can print to right now.
    fn printers(&self) -> Vec<PrinterInfo> {
        let mut v: Vec<PrinterInfo> = self
            .usb
            .printers
            .iter()
            .filter(|p| p.usable())
            .map(|p| PrinterInfo { id: PrinterId::Usb(p.id), name: p.name.clone(), ipp: p.ipp, color: p.color(), papers: p.papers(), dpi: p.dpi() })
            .collect();
        if let Some(t) = &self.test_printer {
            v.push(t.clone());
        }
        v
    }

    pub fn set_test_printer(&mut self, p: Option<PrinterInfo>) {
        if let Some(p) = &p {
            kprintln!("print: test printer \"{}\" (jobs go to COM2)", p.name);
            crate::serial::com2_init();
        }
        self.test_printer = p;
    }

    /// Lay out the pages, encode them for the printer, and send the job.
    fn run_print(&mut self, sources: Vec<Source>, layout: Layout, printer: PrinterInfo) -> Result<String, &'static str> {
        let t0 = Self::now();
        let dpi = printer.dpi;
        let (pw, ph) = print::page_pixels(layout.paper, dpi);
        let per = layout.per_page.max(1) as usize;
        let pages = (sources.len() + per - 1) / per;
        let mut pcl = if printer.ipp { None } else { Some(print::PclJob::start(&layout)) };
        let mut pwg = if printer.ipp { Some(print::PwgJob::start()) } else { None };
        for (n, chunk) in sources.chunks(per).enumerate() {
            let mut page = print::Page::new(pw, ph);
            let cells = page.cells(layout.per_page, dpi);
            for (src, cell) in chunk.iter().zip(cells.iter()) {
                // A photo that can't be read leaves its spot blank instead of failing the job.
                let img = match self.fetch(src).and_then(|bytes| crate::image::decode(&bytes)) {
                    Ok(img) => img,
                    Err(e) => {
                        kprintln!("print: skipping a photo: {}", e);
                        continue;
                    }
                };
                // No need to keep more pixels than the cell can show (either orientation).
                let side = cell.2.max(cell.3);
                let img = if img.w > side || img.h > side { img.shrink_to_fit(side, side) } else { img };
                page.place(&img, *cell, layout.fill);
            }
            if !layout.color {
                for p in page.px.iter_mut() {
                    let (r, g, b) = ((*p >> 16) & 0xFF, (*p >> 8) & 0xFF, *p & 0xFF);
                    let y = (77 * r + 150 * g + 29 * b) >> 8;
                    *p = y << 16 | y << 8 | y;
                }
            }
            if let Some(j) = &mut pcl {
                j.page(&page, layout.color, dpi);
            }
            if let Some(j) = &mut pwg {
                j.page(&page, &layout, dpi, pages as u32);
            }
            kprintln!("print: page {} of {} ready ({}x{} at {} dpi)", n + 1, pages, pw, ph, dpi);
        }
        let (bytes, what) = match (pcl, pwg) {
            (Some(j), _) => (j.finish(), "PCL"),
            (_, Some(j)) => (print::ipp_print_job(&layout, &j.finish()), "IPP/PWG Raster"),
            _ => return Err("No printer language"),
        };
        kprintln!("print: {} job {} bytes, {} page(s), {} cop{}", what, bytes.len(), pages, layout.copies, if layout.copies == 1 { "y" } else { "ies" });
        match printer.id {
            PrinterId::Test => {
                // What would go over USB: raw PCL, or the HTTP request for IPP.
                let wire = if printer.ipp { print::http_post(&bytes) } else { bytes };
                crate::serial::com2_write(&wire);
            }
            PrinterId::Usb(id) if printer.ipp => {
                let resp = self.usb.printer_ipp(id, &bytes)?;
                let info = print::ipp_parse(&resp).ok_or("The printer sent a reply Solon can't read")?;
                kprintln!("print: printer replied status {:#06x} {:?}", info.status, info.state_reasons);
                if info.status >= 0x0100 {
                    return Err("The printer refused the job");
                }
            }
            PrinterId::Usb(id) => self.usb.printer_send(id, &bytes)?,
        }
        kprintln!("print: sent in {} ms", (Self::now() - t0) * 1000 / HZ);
        Ok(format!("Sent {} page{} to {}", pages, if pages == 1 { "" } else { "s" }, printer.name))
    }

    /// Re-decode the original at full resolution, apply the edits, save as JPEG.
    fn save_edit(&mut self, i: usize, choice: SaveChoice, edits: Edits) -> Result<(), &'static str> {
        let src = self.photos.source(i).cloned().ok_or("Photo not found")?;
        let name = String::from(self.photos.name(i).unwrap_or("photo"));
        let t0 = Self::now();
        kprintln!(
            "editor: edits turns {} mirror {} crop {:?} brightness {} contrast {} saturation {} warmth {} filter {:?}",
            edits.turns, edits.flip, edits.crop, edits.brightness, edits.contrast, edits.saturation, edits.warmth, edits.filter
        );
        let full = {
            let bytes = self.fetch(&src)?;
            crate::image::decode(&bytes)?
        };
        let mut out = crate::edit::apply(&full, &edits);
        drop(full);
        let jpeg = crate::edit::encode_jpeg(&mut out, 92)?;
        let (w, h) = (out.w, out.h);
        drop(out);
        let saved = match (choice, &src) {
            (SaveChoice::Replace, Source::Library { dir, entry }) => {
                let e = self.storage.replace(&mut self.usb, dir, entry, &jpeg).map_err(|e| e.message())?;
                kprintln!("editor: replaced \"{}\" with \"{}\" {}x{} {} bytes", entry.name, e.name, w, h, jpeg.len());
                self.photos.end_edit();
                self.photos.replace_item(i, e.name.clone(), Source::Library { dir: dir.clone(), entry: e.clone() });
                e.name
            }
            _ => {
                let folder = match &src {
                    Source::Library { dir, .. } => Some(dir),
                    _ => None,
                };
                let (dir, e) = self.storage.save_copy(&mut self.usb, folder, &name, &jpeg).map_err(|e| e.message())?;
                kprintln!("editor: saved copy \"{}\" in \"{}\" {}x{} {} bytes", e.name, dir.name, w, h, jpeg.len());
                self.photos.end_edit();
                self.photos.insert_after(i, e.name.clone(), Source::Library { dir, entry: e.clone() });
                e.name
            }
        };
        kprintln!("editor: save took {} ms", (Self::now() - t0) * 1000 / HZ);
        self.toast = Some((format!("Saved {}", saved), Self::now() + 5 * HZ));
        self.refresh_storage_view();
        Ok(())
    }

    /// Solon storage changed (Files, Disks): refresh what shows it.
    fn library_changed(&mut self) {
        self.refresh_storage_view();
        let o = self.photos.origin();
        if o == "Solon library" || o.is_empty() || o == "Boot drive" {
            if !self.show_library() {
                self.photos.set(String::from("Solon library"), Vec::new(), None);
            }
        }
    }

    fn files_action(&mut self, a: FilesAction, now: u64) -> bool {
        match a {
            FilesAction::None => false,
            FilesAction::Redraw => true,
            FilesAction::ShowPhotos { origin, items, open_at } => {
                self.photos.set(origin, items, open_at);
                self.open(App::Photos);
                true
            }
            FilesAction::Copy(spec) => {
                kprintln!("files: copy \"{}\" from {:?} to {:?} \"{}\"", spec.item.name, spec.from, spec.to, spec.dest.name);
                let job = CopyJob::new(spec);
                self.toast = Some((format!("Copying \"{}\"...", job.top_name), now + 600 * HZ));
                self.copy = Some(job);
                true
            }
            FilesAction::Changed(msg) => {
                self.toast = Some((msg, now + 5 * HZ));
                self.library_changed();
                true
            }
        }
    }

    fn step_copy(&mut self, now: u64) -> bool {
        let Some(job) = &mut self.copy else { return false };
        job.step(&mut self.storage, &mut self.usb);
        if !job.finished() {
            return false;
        }
        let msg = job.summary();
        let to = job.to;
        kprintln!("files: {}", msg);
        self.copy = None;
        self.toast = Some((msg, now + 6 * HZ));
        self.files.refresh(&mut self.storage, &mut self.usb);
        if to == crate::files::Place::Library {
            self.library_changed();
        }
        true
    }

    fn disk_action(&mut self, a: DiskAction, now: u64) -> bool {
        match a {
            DiskAction::None => false,
            DiskAction::Redraw => true,
            DiskAction::Changed(msg) => {
                self.toast = Some((msg, now + 5 * HZ));
                self.import.drives_changed(&self.usb);
                self.files.refresh(&mut self.storage, &mut self.usb);
                self.library_changed();
                true
            }
        }
    }

    fn apply(&mut self, a: Action) -> bool {
        match a {
            Action::None => false,
            Action::Redraw => true,
            Action::ShowPhotos { origin, items, open_at } => {
                self.photos.set(origin, items, open_at);
                self.open(App::Photos);
                true
            }
            Action::ImportPhotos { source, drive, vol, files } => {
                match self.storage.import_folder(&mut self.usb, if source.is_empty() { "USB drive" } else { &source }) {
                    Ok(dest) => {
                        kprintln!("solon: importing {} photo(s) into \"{}\"", files.len(), dest.name);
                        let existing = self.storage.list(&mut self.usb, &dest).unwrap_or_default().into_iter().map(|e| (e.name, e.size)).collect();
                        self.job = Some(ImportJob { drive, vol, dest, files, existing, done: 0, saved: 0, skipped: 0, failed: 0 });
                        self.refresh_storage_view();
                    }
                    Err(e) => self.import.set_message(String::from(e.message())),
                }
                true
            }
            Action::SetupStorage(disk) => {
                match self.storage.create_library(disk) {
                    Ok(()) => {
                        self.toast = Some((String::from("Solon storage is ready"), crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed) + 5 * HZ));
                    }
                    Err(e) => {
                        kprintln!("solon: storage setup failed: {:?}", e);
                        self.import.set_message(format!("Storage setup failed: {}", e.message()));
                    }
                }
                self.refresh_storage_view();
                true
            }
        }
    }

    fn icon_rect(i: &Icon) -> Rect {
        Rect::new(i.x, i.y, CELL_W, i.h - 8)
    }

    pub fn draw(&mut self, c: &mut Canvas) {
        let (w, h) = (self.w, self.h);

        if self.screen_off {
            c.fill_rect(0, 0, w, h, 0);
            return;
        }

        // Background
        let (_, top, bottom) = settings::BACKGROUNDS[self.settings.background];
        c.gradient_v(0, 0, w, h, top, bottom);
        c.text_centered(w / 2, h - 44, "Solon", Size::Large, true, crate::gfx::mix(top, rgb(160, 180, 200), 80));

        // Top bar
        c.fill_rect_alpha(0, 0, w, BAR_H, 0, 140);
        c.text(16, 6, "Solon", Size::Medium, true, TEXT);
        let clock = self.settings.clock_text(self.clock.0, self.clock.1);
        let cw = text_width(&clock, Size::Medium, true);
        let right = self.power_button().x - 8;
        c.text(right - cw, 6, &clock, Size::Medium, true, TEXT);
        let mut status = String::from("Offline");
        if self.settings.show_date {
            status = format!("{}   |   {}", settings::date_text(self.date.0, self.date.1, self.date.2), status);
        }
        let sw = text_width(&status, Size::Small, false);
        c.text(right - cw - 18 - sw, 8, &status, Size::Small, false, TEXT_DIM);
        if let Some((msg, _)) = &self.toast {
            let tw = text_width(msg, Size::Small, true) + 32;
            c.round_rect(w / 2 - tw / 2, 4, tw, 24, 12, rgb(243, 156, 18));
            c.text_centered(w / 2, 8, msg, Size::Small, true, rgb(30, 30, 30));
        }

        // Icons
        for (n, icon) in self.icons.iter().enumerate() {
            if self.selected == Some(n) {
                let r = Self::icon_rect(icon);
                c.round_rect_alpha(r.x, r.y, r.w, r.h, 10, rgb(255, 255, 255), 50);
            }
            icons::draw(c, icon.app, icon.x + (CELL_W - ICON) / 2, icon.y + 8);
            c.text_centered(icon.x + CELL_W / 2, icon.y + ICON + 16, icon.app.name(), Size::Small, false, TEXT);
        }

        if let Some(win) = &self.window {
            draw_frame(c, win);
            let body = win.body();
            c.set_clip(body);
            match win.app {
                App::Photos => self.photos.draw(c, body),
                App::Import => self.import.draw(c, body, &self.usb, &self.storage_view),
                App::Settings => self.settings_app.draw(c, body, &self.settings, self.screen, self.storage.library.is_some()),
                App::Files => self.files.draw(c, body, &self.storage, &self.usb),
                App::Disks => self.disks.draw(c, body, &self.storage),
                app => draw_placeholder(c, app, body),
            }
            c.reset_clip();
        }
        self.draw_power(c);
        if self.show_log {
            self.draw_log(c);
        }
    }

    fn open(&mut self, app: App) {
        let ww = (self.w - WIN_LEFT - 16).max(560);
        let wh = (self.h - BAR_H - 28).max(360);
        self.window = Some(Window { app, x: WIN_LEFT, y: BAR_H + 12, w: ww, h: wh });
        kprintln!("solon: open {}", app.name());
        if app == App::Settings {
            self.settings_app.reload_time();
        }
        if app == App::Files {
            self.files.refresh(&mut self.storage, &mut self.usb);
        }
        if app == App::Disks {
            self.disks.refresh(&mut self.storage, &mut self.usb);
        }
    }

    fn close(&mut self) {
        if let Some(win) = self.window.take() {
            kprintln!("solon: close {}", win.app.name());
        }
        self.drag = None;
    }

    /// Returns true if the screen needs redrawing.
    pub fn on_mouse(&mut self, x: i32, y: i32, buttons: u8, prev: u8, wheel: i8, now: u64) -> bool {
        let pressed = buttons & 1 != 0 && prev & 1 == 0;
        let released = buttons & 1 == 0 && prev & 1 != 0;
        let mut redraw = false;

        if self.screen_off {
            // Any mouse movement or click wakes the screen.
            self.screen_off = false;
            kprintln!("power: screen on");
            return true;
        }

        if wheel != 0 {
            if let Some(win) = &self.window {
                if win.rect().contains(x, y) {
                    match win.app {
                        App::Photos => redraw |= self.photos.on_wheel(wheel, win.body()),
                        App::Import => redraw |= self.import.on_wheel(wheel, win.body(), &self.usb),
                        App::Files => redraw |= self.files.on_wheel(wheel, win.body()),
                        App::Disks => redraw |= self.disks.on_wheel(wheel, win.body()),
                        _ => {}
                    }
                }
            }
        }

        // A press inside the Photos window keeps getting moves until release (editor drags).
        if self.capture {
            if let Some(win) = &self.window {
                let body = win.body();
                if released {
                    self.capture = false;
                    return redraw | self.photos.on_release();
                }
                if buttons & 1 != 0 {
                    return redraw | self.photos.on_drag(x, y, body);
                }
            }
            self.capture = false;
        }

        if let Some((ox, oy)) = self.drag {
            if released {
                self.drag = None;
                return redraw;
            }
            if let Some(win) = &mut self.window {
                win.x = (x - ox).clamp(-win.w + 80, self.w - 80);
                win.y = (y - oy).clamp(BAR_H, self.h - TITLE_H);
                return true;
            }
        }

        if !pressed {
            return redraw;
        }
        kprintln!("solon: click at {},{}", x, y);
        if let Some(r) = self.power_click(x, y) {
            return r;
        }

        if let Some(win) = &self.window {
            if win.close_box().contains(x, y) {
                self.close();
                return true;
            }
            if win.title_bar().contains(x, y) {
                self.drag = Some((x - win.x, y - win.y));
                return redraw;
            }
            if win.body().contains(x, y) {
                let body = win.body();
                match win.app {
                    App::Photos => {
                        redraw |= self.photos.on_press(x, y, body, now);
                        self.capture = true;
                        redraw |= self.photo_action();
                    }
                    App::Import => {
                        let a = self.import.on_press(x, y, body, now, &mut self.usb, &self.storage_view);
                        redraw |= self.apply(a);
                    }
                    App::Files => {
                        let a = self.files.on_press(x, y, body, now, &mut self.storage, &mut self.usb);
                        redraw |= self.files_action(a, now);
                    }
                    App::Disks => {
                        let a = self.disks.on_press(x, y, body, &self.storage);
                        redraw |= self.disk_action(a, now);
                    }
                    App::Settings => {
                        let screen = self.screen;
                        match self.settings_app.on_press(x, y, body, &self.settings, screen) {
                            SettingsAction::None => {}
                            SettingsAction::Redraw => redraw = true,
                            SettingsAction::Change(n) => {
                                self.apply_settings(n);
                                redraw = true;
                            }
                            SettingsAction::SetClock { y, mo, d, h, mi } => {
                                crate::rtc::set(y, mo, d, h, mi);
                                self.date = (y, mo, d);
                                self.clock = (h, mi);
                                self.toast = Some((String::from("Clock set"), now + 3 * HZ));
                                redraw = true;
                            }
                        }
                    }
                    _ => {}
                }
                return redraw;
            }
            if win.rect().contains(x, y) {
                return redraw;
            }
        }

        let hit = self.icons.iter().position(|i| Self::icon_rect(i).contains(x, y));
        if let Some(n) = hit {
            let double = matches!(self.last_click, Some((m, t)) if m == n && now - t <= DOUBLE_CLICK_TICKS);
            self.last_click = Some((n, now));
            self.selected = Some(n);
            if double {
                self.open(self.icons[n].app);
                self.last_click = None;
            }
            return true;
        }
        self.last_click = None;
        redraw | self.selected.take().is_some()
    }

    /// Scancode set 1. Returns true if the screen needs redrawing.
    pub fn on_key(&mut self, sc: u8) -> bool {
        if sc == 0x58 {
            // F12: diagnostics (kernel log)
            self.show_log = !self.show_log;
            return true;
        }
        if self.show_log {
            if sc == 0x01 {
                self.show_log = false;
                return true;
            }
            return false;
        }
        if self.screen_off {
            if sc & 0x80 == 0 {
                self.screen_off = false;
                kprintln!("power: screen on");
            }
            return true;
        }
        if let Some(req) = self.confirm {
            match sc {
                0x01 => self.confirm = None,
                0x1C => {
                    self.confirm = None;
                    kprintln!("power: {:?} confirmed", req);
                    self.request = Some(req);
                }
                _ => return false,
            }
            return true;
        }
        if self.power_menu {
            if sc == 0x01 {
                self.power_menu = false;
                return true;
            }
            return false;
        }
        if let Some(win) = &self.window {
            let body = win.body();
            match win.app {
                App::Photos => {
                    if let Some(redraw) = self.photos.on_key(sc, body) {
                        return redraw | self.photo_action();
                    }
                }
                App::Import => {
                    if let Some(a) = self.import.on_key(sc, body, &mut self.usb, &self.storage_view) {
                        return self.apply(a);
                    }
                }
                App::Files => {
                    if let Some(a) = self.files.on_key(sc, body, &mut self.storage, &mut self.usb) {
                        return self.files_action(a, Self::now());
                    }
                }
                App::Disks => {
                    if let Some(a) = self.disks.on_key(sc) {
                        return self.disk_action(a, Self::now());
                    }
                }
                _ => {}
            }
            if sc == 0x01 {
                // Esc closes the window
                self.close();
                return true;
            }
            return false;
        }

        match sc {
            0x1C => {
                // Enter: open selected icon
                if let Some(n) = self.selected {
                    self.open(self.icons[n].app);
                    true
                } else {
                    false
                }
            }
            0x48 | 0x50 => {
                // Up / Down: move selection
                let n = self.icons.len();
                self.selected = Some(match (self.selected, sc) {
                    (None, 0x48) => n - 1,
                    (None, _) => 0,
                    (Some(cur), 0x48) => (cur + n - 1) % n,
                    (Some(cur), _) => (cur + 1) % n,
                });
                true
            }
            _ => false,
        }
    }

    pub fn set_clock(&mut self, h: u8, m: u8) -> bool {
        let changed = self.clock != (h, m);
        self.clock = (h, m);
        changed
    }
}

fn draw_frame(c: &mut Canvas, win: &Window) {
    let (x, y, w, h) = (win.x, win.y, win.w, win.h);
    c.round_rect_alpha(x + 4, y + 8, w, h, 10, 0, 90); // shadow
    c.round_rect(x, y, w, h, 10, WIN_BG);
    c.round_rect(x, y, w, TITLE_H + 12, 10, rgb(232, 235, 240));
    c.fill_rect(x, y + TITLE_H, w, 12, WIN_BG);
    c.fill_rect(x, y + TITLE_H, w, 1, rgb(210, 214, 220));

    // Title with a small app-coloured dot
    c.circle(x + 20, y + TITLE_H / 2, 6, icons::tile_color(win.app));
    c.text(x + 34, y + 8, win.app.name(), Size::Medium, true, rgb(40, 44, 52));

    // Close button
    let b = win.close_box();
    let (cx, cy) = (b.x + b.w / 2, b.y + b.h / 2);
    c.circle(cx, cy, 9, rgb(225, 80, 70));
    for d in -4..=4 {
        c.fill_rect(cx + d - 1, cy + d, 2, 1, rgb(255, 255, 255));
        c.fill_rect(cx + d - 1, cy - d, 2, 1, rgb(255, 255, 255));
    }
}

fn draw_placeholder(c: &mut Canvas, app: App, r: Rect) {
    icons::draw(c, app, r.x + r.w / 2 - 32, r.y + r.h / 2 - 110);
    let [a, b] = app.placeholder();
    c.text_centered(r.x + r.w / 2, r.y + r.h / 2 - 20, a, Size::Medium, true, rgb(40, 44, 52));
    c.text_centered(r.x + r.w / 2, r.y + r.h / 2 + 12, b, Size::Small, false, rgb(100, 106, 116));
}

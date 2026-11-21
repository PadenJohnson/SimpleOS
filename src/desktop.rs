//! The Solon desktop: app windows (several at once), and a taskbar with the Applications menu.
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
use crate::documents::{DocSrc, DocsAction, DocsApp};
use crate::movieui::{MovieAction, MovieApp};
use crate::videos::{VideosAction, VideosApp};
use crate::usb::Usb;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum App {
    Photos,
    Videos,
    Movie,
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
            App::Movie => "Video Editor",
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
            App::Files | App::Disks | App::Movie => ["", ""],
        }
    }
}

/// Height of the taskbar along the bottom.
const BAR_H: i32 = 44;
const TITLE_H: i32 = 36;
const DOUBLE_CLICK_TICKS: u64 = 50; // 0.5 s at 100 Hz

const TEXT: u32 = rgb(240, 242, 245);
const TEXT_DIM: u32 = rgb(170, 180, 190);
const WIN_BG: u32 = rgb(250, 250, 252);

/// Every app, in the order the Applications menu shows them.
const APPS: [App; 9] = [App::Photos, App::Documents, App::Files, App::Import, App::Videos, App::Movie, App::Printers, App::Disks, App::Settings];

struct Window {
    app: App,
    x: i32,
    y: i32,
    w: i32,
    h: i32,
    minimized: bool,
    /// Where it was before being maximized.
    restore: Option<Rect>,
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
    fn max_box(&self) -> Rect {
        Rect::new(self.x + self.w - 2 * TITLE_H, self.y, TITLE_H, TITLE_H)
    }
    fn min_box(&self) -> Rect {
        Rect::new(self.x + self.w - 3 * TITLE_H, self.y, TITLE_H, TITLE_H)
    }
    /// The area the app draws into.
    fn body(&self) -> Rect {
        Rect::new(self.x, self.y + TITLE_H + 1, self.w, self.h - TITLE_H - 1)
    }
}

pub struct Desktop {
    w: i32,
    h: i32,
    /// Open windows, bottom to top (the last one not minimized has the focus).
    windows: Vec<Window>,
    /// Apps in the order they were opened (the taskbar buttons).
    order: Vec<App>,
    /// Moving a window: (app, offset of the pointer from its corner).
    drag: Option<(App, i32, i32)>,
    clock: (u8, u8),
    /// Last click on a title bar (double-click maximizes).
    last_title: Option<(App, u64)>,
    /// The Applications menu is open (and which item the keyboard is on).
    apps_menu: Option<usize>,
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
    /// Diagnostics: how many lines up from the newest.
    log_scroll: usize,
    test_printer: Option<PrinterInfo>,
    pending_print: Option<(Vec<Source>, Layout, PrinterInfo)>,
    settings: Settings,
    settings_app: SettingsApp,
    /// Physical screen size (the desktop itself may be drawn smaller and enlarged).
    screen: (usize, usize),
    date: (u16, u8, u8),
    power_menu: bool,
    /// One-time notice: Solon runs from its USB stick and could be installed.
    install_notice: bool,
    /// The internal disk Solon started from (it's installed there), for update notices.
    installed_disk: Option<usize>,
    confirm: Option<PowerRequest>,
    screen_off: bool,
    files: Files,
    disks: DiskMgr,
    copy: Option<CopyJob>,
    docs: DocsApp,
    movie: MovieApp,
    videos: VideosApp,
    /// Spreadsheet pages waiting to be printed (on the next tick, after the notice shows).
    pending_sheet_print: Option<(Vec<Canvas>, Layout, PrinterInfo)>,
    /// A phone import to start on the next tick (after the "looking" notice shows).
    pending_phone_import: Option<(String, u32, usize)>,
    /// An edited photo to save on the next tick (after the "saving" notice shows).
    pending_save: Option<(usize, SaveChoice, Edits)>,
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
    /// From a phone: its serial (to remember what was imported) and the big file in progress.
    serial: Option<String>,
    stream: Option<(crate::fs::exfat::Pending, DirEntry, u64)>,
    keys: Vec<String>,
    /// Phone items left out because they were imported before.
    seen_before: usize,
}

const PHONE_INDEX: &str = "PhoneImports.txt";

/// How an imported phone file is remembered.
fn phone_key(serial: &Option<String>, f: &DirEntry) -> String {
    format!("{}	{}	{}", serial.as_deref().unwrap_or(""), f.name, f.size)
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

    /// `w`, `h`: the (possibly scaled-down) desktop size; `screen`: the physical screen size.
    pub fn new(w: usize, h: usize, photos: Photos, usb: Usb, storage: Storage, settings: Settings, screen: (usize, usize)) -> Self {
        let mut d = Self {
            w: w as i32,
            h: h as i32,
            windows: Vec::new(),
            order: Vec::new(),
            drag: None,
            clock: (0, 0),
            last_title: None,
            apps_menu: None,
            photos,
            import: Import::new(),
            drive_ids: usb.drives.iter().map(|d| d.id).chain(usb.phones.iter().map(|p| p.id)).collect(),
            usb,
            toast: None,
            storage,
            storage_view: StorageView { library: None, options: Vec::new(), job: None },
            job: None,
            capture: false,
            show_log: false,
            log_scroll: 0,
            test_printer: None,
            pending_print: None,
            settings,
            settings_app: SettingsApp::new(),
            screen,
            date: crate::rtc::date(),
            power_menu: false,
            install_notice: false,
            installed_disk: None,
            confirm: None,
            screen_off: false,
            files: Files::new(),
            disks: DiskMgr::new(),
            copy: None,
            docs: DocsApp::new(),
            movie: MovieApp::new(),
            videos: VideosApp::new(),
            pending_sheet_print: None,
            pending_phone_import: None,
            pending_save: None,
            request: None,
        };
        d.refresh_storage_view();
        d.installed_disk = d.usb.boot_disk_id().and_then(|id| d.storage.boot_internal(id));
        d.install_notice = !d.settings.install_notice && (0..d.storage.disk_count()).any(|i| crate::install::blocker(&d.storage, &d.usb, i).is_none());
        if d.install_notice {
            kprintln!("solon: showing the install notice");
        }
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
        self.fit_windows();
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
        Rect::new(self.w - 48, self.h - BAR_H, 48, BAR_H)
    }

    fn power_item(&self, i: i32) -> Rect {
        Rect::new(self.w - 236, self.h - BAR_H - 10 - 3 * 46 + i * 46, 220, 42)
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

    /// Stop USB controllers before the computer is switched off.
    pub fn prepare_power_off(&mut self) {
        self.usb.shutdown();
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
        self.photos.needs_loading() || self.job.is_some() || self.copy.is_some() || self.disks.busy() || self.movie.exporting()
    }

    fn step_import(&mut self, now: u64) -> bool {
        let Some(job) = &mut self.job else { return false };
        // A big file from a phone being copied a piece at a time.
        if let Some((pending, src, off)) = job.stream.take() {
            let r = self.usb.read_part(job.drive, &src, off, crate::phone::PIECE).and_then(|part| {
                if part.is_empty() {
                    return Err(FsError::Io);
                }
                self.storage.write_part(&mut self.usb, &pending, off, &part)?;
                Ok(off + part.len() as u64)
            });
            match r {
                Ok(next) if next < pending.len => job.stream = Some((pending, src, next)),
                Ok(_) => match self.storage.finish_file(&mut self.usb, pending) {
                    Ok(e) => {
                        kprintln!("solon: import \"{}\" done ({} bytes, in pieces)", e.name, e.size);
                        job.existing.push((e.name, e.size));
                        job.saved += 1;
                        job.keys.push(phone_key(&job.serial, &src));
                        job.done += 1;
                    }
                    Err(e) => {
                        kprintln!("solon: import {} failed: {:?}", src.name, e);
                        job.failed += 1;
                        job.done += 1;
                    }
                },
                Err(e) => {
                    kprintln!("solon: import {} failed: {:?}", src.name, e);
                    let _ = self.storage.abort_file(&mut self.usb, pending);
                    job.failed += 1;
                    job.done += 1;
                }
            }
        } else if job.done < job.files.len() {
            let f = job.files[job.done].clone();
            let name = unique_name(&job.existing, &f.name, f.size);
            let big = job.serial.is_some() && f.size > crate::phone::PIECE as u64;
            let r = match name {
                None => Ok(SaveResult::AlreadyThere),
                Some(name) if big => match self.storage.begin_file(&mut self.usb, &job.dest, &name, f.size) {
                    Ok(p) => {
                        kprintln!("solon: import \"{}\" ({} bytes) in pieces", name, f.size);
                        job.stream = Some((p, f.clone(), 0));
                        return true; // the pieces come on the next ticks
                    }
                    Err(e) => Err(e),
                },
                Some(name) => self.usb.read_file(job.drive, job.vol, &f).and_then(|data| {
                    let e = self.storage.create_in(&mut self.usb, &job.dest, &name, &data)?;
                    job.existing.push((name, data.len() as u64));
                    Ok(SaveResult::Saved(e))
                }),
            };
            match r {
                Ok(SaveResult::Saved(_)) => {
                    job.saved += 1;
                    job.keys.push(phone_key(&job.serial, &f));
                }
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
        if job.done >= job.files.len() && job.stream.is_none() {
            let (saved, skipped, failed) = (job.saved, job.skipped + job.seen_before, job.failed);
            let phone = job.serial.is_some();
            let keys = core::mem::take(&mut job.keys);
            kprintln!("solon: import finished: {} saved, {} already there, {} failed", saved, skipped, failed);
            let what = if phone { "photos and videos" } else { "photos" };
            let mut msg = if phone && saved == 1 { String::from("Imported 1 item") } else if saved == 1 { String::from("Imported 1 photo") } else { format!("Imported {} {}", saved, what) };
            if skipped > 0 {
                msg += &format!(", {} already in Solon", skipped);
            }
            if failed > 0 {
                msg += &format!(", {} failed", failed);
            }
            self.toast = Some((msg.clone(), now + 6 * HZ));
            self.import.set_message(msg);
            self.job = None;
            if phone && !keys.is_empty() {
                self.remember_phone_imports(&keys);
            }
            if self.photos.origin() == "Solon library" || self.photos.origin().is_empty() || self.photos.origin() == "Boot drive" {
                self.show_library();
            }
        }
        self.refresh_storage_view();
        true
    }

    /// One-click phone import: everything on the phone that wasn't imported before.
    fn start_phone_import(&mut self, source: &str, drive: u32, vol: usize, now: u64) {
        let serial = self.usb.phones.iter().find(|p| p.id == drive).map(|p| if p.serial.is_empty() { p.name.clone() } else { p.serial.clone() });
        let all = self.phone_media(drive, vol);
        let known = self.phone_imports();
        let total = all.len();
        let files: Vec<DirEntry> = all.into_iter().filter(|f| !known.contains(&phone_key(&serial, f))).collect();
        let seen_before = total - files.len();
        kprintln!("solon: phone has {} photo(s)/video(s), {} new", total, files.len());
        if files.is_empty() {
            let msg = if total == 0 { String::from("No photos or videos found on the phone") } else { format!("Nothing new: all {} photos and videos are already in Solon", total) };
            self.toast = Some((msg.clone(), now + 6 * HZ));
            self.import.set_message(msg);
            return;
        }
        match self.storage.import_folder(&mut self.usb, if source.is_empty() { "Phone" } else { source }) {
            Ok(dest) => {
                let existing = self.storage.list(&mut self.usb, &dest).unwrap_or_default().into_iter().map(|e| (e.name, e.size)).collect();
                self.toast = Some((format!("Importing {} new photos and videos...", files.len()), now + 4 * HZ));
                self.job = Some(ImportJob { drive, vol, dest, files, existing, done: 0, saved: 0, skipped: 0, failed: 0, serial, stream: None, keys: Vec::new(), seen_before });
                self.refresh_storage_view();
            }
            Err(e) => {
                self.toast = None;
                self.import.set_message(String::from(e.message()));
            }
        }
    }

    /// Lines of "phone serial<TAB>file name<TAB>size" already imported from phones.
    fn phone_imports(&mut self) -> Vec<String> {
        let text = self.storage.read_root_file(&mut self.usb, PHONE_INDEX).unwrap_or_default();
        core::str::from_utf8(&text).unwrap_or("").lines().map(String::from).collect()
    }

    fn remember_phone_imports(&mut self, keys: &[String]) {
        let mut all = self.phone_imports();
        all.extend(keys.iter().cloned());
        let text: String = all.iter().map(|l| format!("{}\n", l)).collect();
        if let Err(e) = self.storage.write_root_file(&mut self.usb, PHONE_INDEX, text.as_bytes()) {
            kprintln!("solon: couldn't save the phone import list: {:?}", e);
        }
    }

    /// Every photo and video on a phone storage (skipping app data and thumbnails).
    fn phone_media(&mut self, drive: u32, vol: usize) -> Vec<DirEntry> {
        let Some(root) = self.usb.source_root(drive, vol) else { return Vec::new() };
        let mut out = Vec::new();
        let mut stack = Vec::from([(root, 0u32)]);
        let mut seen = 0;
        while let Some((dir, depth)) = stack.pop() {
            let Ok(list) = self.usb.list(drive, vol, &dir) else { continue };
            for e in list {
                seen += 1;
                if seen > 50_000 {
                    return out;
                }
                let lower = e.name.to_ascii_lowercase();
                if e.is_dir {
                    let skip = lower.starts_with('.') || (depth == 0 && (lower == "android" || lower == "notifications" || lower == "ringtones" || lower == "alarms"));
                    if !skip && depth < 8 {
                        stack.push((e, depth + 1));
                    }
                } else if crate::import::is_photo(&e.name) || crate::phone::is_video(&e.name) || crate::phone::is_other_photo(&e.name) {
                    out.push(e);
                }
            }
        }
        out
    }

    /// Periodic work: USB plug/unplug and input, loading the next photo. Returns true to redraw.
    pub fn tick(&mut self, now: u64) -> bool {
        let mut redraw = false;
        if self.usb.poll() {
            let ids: Vec<u32> = self.usb.drives.iter().map(|d| d.id).chain(self.usb.phones.iter().map(|p| p.id)).collect();
            if let Some(p) = self.usb.phones.iter().find(|p| !self.drive_ids.contains(&p.id)) {
                let msg = match p.problem {
                    Some(hint) => format!("{} connected. {}", p.name, hint),
                    None => format!("{} connected - open Import to copy its photos", p.name),
                };
                self.toast = Some((msg, now + 10 * HZ));
            } else if let Some(d) = self.usb.drives.iter().find(|d| !self.drive_ids.contains(&d.id)) {
                let name = d.volumes.first().map(|v| v.label.clone()).filter(|l| !l.is_empty()).unwrap_or(d.name.clone());
                self.toast = Some((format!("{} connected - open Import to see it", name), now + 6 * HZ));
                if self.installed_disk.is_some() {
                    let cur = crate::update::Build::current();
                    if crate::update::find(&mut self.usb).iter().any(|(c, _)| c.build.id.trim_end_matches('+') != cur.id.trim_end_matches('+') && c.build.time >= cur.time) {
                        self.toast = Some((String::from("A newer Solon is on this USB drive - open Disks and press Update from USB"), now + 12 * HZ));
                    }
                }
            } else if ids.len() < self.drive_ids.len() {
                self.toast = Some((String::from("Drive or phone removed"), now + 4 * HZ));
            }
            self.drive_ids = ids;
            self.import.drives_changed(&self.usb);
            self.files.drives_changed(&mut self.storage, &mut self.usb);
            if self.is_open(App::Disks) {
                self.disks.refresh(&mut self.storage, &mut self.usb);
            }
            if self.is_open(App::Documents) && !self.docs.editing() {
                self.docs.refresh(&mut self.storage, &mut self.usb);
            }
            let usb = &self.usb;
            self.photos.drive_removed(&|id| !usb.has_drive(id));
            redraw = true;
        }
        if matches!(&self.toast, Some((_, end)) if now >= *end) {
            self.toast = None;
            redraw = true;
        }
        // Video editor: playback, previews, thumbnails, saving, exporting.
        if let Some(win) = self.windows.iter().find(|w| w.app == App::Movie) {
            let visible = !win.minimized;
            let a = self.movie.tick(now, &mut self.storage, &mut self.usb, visible);
            redraw |= self.movie_action(a, now);
        }
        if let Some(win) = self.windows.iter().find(|w| w.app == App::Videos) {
            let visible = !win.minimized;
            redraw |= matches!(self.videos.tick(now, &mut self.storage, &mut self.usb, visible), VideosAction::Redraw);
        }
        if let Some((i, choice, edits)) = self.pending_save.take() {
            match self.save_edit(i, choice, edits) {
                Ok(()) => {
                    if matches!(&self.toast, Some((m, _)) if m.starts_with("Saving")) {
                        self.toast = Some((String::from("Photo saved"), now + 3 * HZ));
                    }
                }
                Err(e) => {
                    kprintln!("editor: save failed: {}", e);
                    self.toast = Some((format!("Couldn't save: {}", e), now + 6 * HZ));
                }
            }
            return true;
        }
        if let Some((source, drive, vol)) = self.pending_phone_import.take() {
            self.start_phone_import(&source, drive, vol, now);
            return true;
        }
        if let Some((pages, layout, printer)) = self.pending_sheet_print.take() {
            match self.run_print_canvases(pages, layout, printer) {
                Ok(msg) => self.toast = Some((msg, now + 6 * HZ)),
                Err(e) => {
                    kprintln!("print: failed: {}", e);
                    self.toast = Some((format!("Printing failed: {}", e), now + 8 * HZ));
                }
            }
            return true;
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
        let lines = crate::serial::LOG.lock().lines();
        let max = ((h - 90) / 18).max(1) as usize;
        let end = lines.len() - self.log_scroll.min(lines.len().saturating_sub(max.min(lines.len())));
        let start = end.saturating_sub(max);
        let info = format!(
            "Screen {}x{}   |   Build {}   |   Lines {}-{} of {}   |   Up/Down, Page Up/Down, Home/End to scroll   |   F12 or Esc to close",
            w, h, env!("SOLON_BUILD"), start + 1, end, lines.len()
        );
        c.text(24, 44, &info, Size::Small, false, rgb(160, 170, 180));
        let mut y = 76;
        for line in &lines[start..end] {
            let s = core::str::from_utf8(line).unwrap_or("?");
            let color = if s.contains("fail") || s.contains("error") || s.contains("FAIL") || s.contains("PANIC") {
                rgb(255, 140, 120)
            } else {
                rgb(210, 220, 230)
            };
            c.text(24, y, s, Size::Small, false, color);
            y += 18;
        }
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
                    Some(Source::Library { dir, entry }) => (true, self.storage.backup_of(&mut self.usb, &dir, &entry).is_some()),
                    _ => (false, false),
                };
                self.photos.start_edit(i, can_save, can_save && can_replace, has_original);
            }
            PhotoAction::Save(i, choice, edits) => {
                // Show the notice first; the (slow, full-size) save runs on the next tick.
                self.toast = Some((String::from("Saving your photo..."), Self::now() + 120 * HZ));
                self.pending_save = Some((i, choice, edits));
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
        let dpi = printer.dpi;
        let (pw, ph) = print::page_pixels(layout.paper, dpi);
        let per = layout.per_page.max(1) as usize;
        let pages = (sources.len() + per - 1) / per;
        let (storage, usb) = (&mut self.storage, &mut self.usb);
        let job = build_job(pages, &layout, &printer, &mut |n| {
            let mut page = print::Page::new(pw, ph);
            let cells = page.cells(layout.per_page, dpi);
            for (src, cell) in sources[n * per..((n + 1) * per).min(sources.len())].iter().zip(cells.iter()) {
                let bytes: Result<Bytes, &'static str> = match src {
                    Source::Memory(b) => Ok(Bytes::Static(b)),
                    Source::Usb { drive, vol, entry } => usb.read_file(*drive, *vol, entry).map(Bytes::Owned).map_err(|e| e.message()),
                    Source::Library { entry, .. } => storage.read_file(usb, entry).map(Bytes::Owned).map_err(|e| e.message()),
                };
                // A photo that can't be read leaves its spot blank instead of failing the job.
                let img = match bytes.and_then(|bytes| crate::image::decode(&bytes)) {
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
            page
        })?;
        self.send_job(job, pages, &layout, &printer)
    }

    /// Print pages drawn at 100 pixels per inch (spreadsheets), enlarged to the printer's resolution.
    fn run_print_canvases(&mut self, canvases: Vec<Canvas>, layout: Layout, printer: PrinterInfo) -> Result<String, &'static str> {
        let (pw, ph) = print::page_pixels(layout.paper, printer.dpi);
        let job = build_job(canvases.len(), &layout, &printer, &mut |n| {
            let src = &canvases[n];
            let mut page = print::Page::new(pw, ph);
            for y in 0..ph {
                let sy = (y * src.h / ph).min(src.h - 1);
                let row = &src.px[sy * src.w..(sy + 1) * src.w];
                let out = &mut page.px[y * pw..(y + 1) * pw];
                for (x, p) in out.iter_mut().enumerate() {
                    *p = row[(x * src.w / pw).min(src.w - 1)] & 0x00FF_FFFF;
                }
            }
            page
        })?;
        self.send_job(job, canvases.len(), &layout, &printer)
    }

    /// Send a finished job (PCL, or an IPP request) to the printer.
    fn send_job(&mut self, bytes: Vec<u8>, pages: usize, layout: &Layout, printer: &PrinterInfo) -> Result<String, &'static str> {
        let t0 = Self::now();
        kprintln!("print: {} job {} bytes, {} page(s), {} cop{}", if printer.ipp { "IPP/PWG Raster" } else { "PCL" }, bytes.len(), pages, layout.copies, if layout.copies == 1 { "y" } else { "ies" });
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
        kprintln!("editor: edits {:?}", edits);
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

    /// Test hook (boot option "selftest=originals"): replace a library photo, then rename,
    /// move, recycle and restore it, checking its original stays linked; then revert.
    pub fn self_test_originals(&mut self) {
        let r = self.storage.self_test_originals(&mut self.usb);
        kprintln!("selftest: originals {}", if r.is_ok() { "PASSED" } else { "FAILED" });
        if let Err(e) = r {
            kprintln!("selftest: {}", e);
        }
        self.library_changed();
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
            FilesAction::OpenDocument(src) => self.open_document(src, now),
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

    fn docs_action(&mut self, a: DocsAction, now: u64) -> bool {
        match a {
            DocsAction::None => false,
            DocsAction::Redraw => true,
            DocsAction::Toast(msg) => {
                self.toast = Some((msg, now + 5 * HZ));
                if !self.docs.editing() {
                    self.docs.refresh(&mut self.storage, &mut self.usb);
                }
                self.refresh_storage_view();
                true
            }
            DocsAction::WantPrinters => {
                let printers = self.printers();
                kprintln!("print: sheet dialog opened, {} printer(s)", printers.len());
                self.docs.start_print(printers);
                true
            }
            DocsAction::Print { pages, layout, printer } => {
                self.toast = Some((format!("Preparing {} page(s) for {}...", pages.len(), printer.name), now + 60 * HZ));
                self.pending_sheet_print = Some((pages, layout, printer));
                true
            }
        }
    }

    /// Open a spreadsheet from the Files app.
    fn open_document(&mut self, src: DocSrc, now: u64) -> bool {
        self.open(App::Documents);
        let a = self.docs.open_src(src, &mut self.storage, &mut self.usb);
        self.docs_action(a, now)
    }

    fn disk_action(&mut self, a: DiskAction, now: u64) -> bool {
        match a {
            DiskAction::None => false,
            DiskAction::Redraw => true,
            DiskAction::Restart => {
                kprintln!("power: restart after installing");
                self.request = Some(PowerRequest::Restart);
                true
            }
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
                        self.job = Some(ImportJob { drive, vol, dest, files, existing, done: 0, saved: 0, skipped: 0, failed: 0, serial: None, stream: None, keys: Vec::new(), seen_before: 0 });
                        self.refresh_storage_view();
                    }
                    Err(e) => self.import.set_message(String::from(e.message())),
                }
                true
            }
            Action::ImportPhone { source, drive, vol } => {
                self.toast = Some((String::from("Looking for new photos and videos on the phone..."), Self::now() + 120 * HZ));
                self.pending_phone_import = Some((source, drive, vol));
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

    // ---- "install on this computer" notice ------------------------------------------------

    fn notice_box(&self) -> Rect {
        let w = 600.min(self.w - 40);
        Rect::new((self.w - w) / 2, (self.h - 230) / 2, w, 230)
    }
    fn notice_open(&self) -> Rect {
        let b = self.notice_box();
        Rect::new(b.x + b.w - 24 - 150, b.y + b.h - 58, 150, 38)
    }
    fn notice_later(&self) -> Rect {
        let o = self.notice_open();
        Rect::new(o.x - 12 - 110, o.y, 110, 38)
    }

    fn draw_install_notice(&self, c: &mut Canvas) {
        c.fill_rect_alpha(0, 0, self.w, self.h, 0, 110);
        let b = self.notice_box();
        c.round_rect_alpha(b.x + 3, b.y + 6, b.w, b.h, 12, 0, 90);
        c.round_rect(b.x, b.y, b.w, b.h, 12, rgb(255, 255, 255));
        c.text(b.x + 24, b.y + 20, "Solon is running from the USB stick", Size::Medium, true, rgb(40, 44, 52));
        let lines = [
            "You can install Solon on this computer's disk, so it starts",
            "without the stick. Installing erases that disk, and copies your",
            "photos and files from the stick. It's in the Disks app.",
        ];
        for (i, l) in lines.iter().enumerate() {
            c.text(b.x + 24, b.y + 62 + i as i32 * 24, l, Size::Small, false, rgb(110, 116, 126));
        }
        let o = self.notice_open();
        c.round_rect(o.x, o.y, o.w, o.h, 8, rgb(46, 134, 222));
        c.text_centered(o.x + o.w / 2, o.y + 10, "Open Disks", Size::Small, true, rgb(255, 255, 255));
        let l = self.notice_later();
        c.round_rect(l.x, l.y, l.w, l.h, 8, rgb(230, 233, 238));
        c.text_centered(l.x + l.w / 2, l.y + 10, "Not now", Size::Small, true, rgb(40, 44, 52));
    }

    /// Either way the notice isn't shown again (the Disks app keeps the button).
    fn dismiss_install_notice(&mut self, open_disks: bool) {
        self.install_notice = false;
        let mut s = self.settings;
        s.install_notice = true;
        self.apply_settings(s);
        if open_disks {
            self.open(App::Disks);
        }
    }

    // ---- windows ----------------------------------------------------------------------

    /// The area above the taskbar.
    fn work(&self) -> Rect {
        Rect::new(0, 0, self.w, self.h - BAR_H)
    }

    fn is_open(&self, app: App) -> bool {
        self.windows.iter().any(|w| w.app == app)
    }

    fn index_of(&self, app: App) -> Option<usize> {
        self.windows.iter().position(|w| w.app == app)
    }

    /// The window with the keyboard focus: the top one that isn't minimized.
    fn focused(&self) -> Option<usize> {
        self.windows.iter().rposition(|w| !w.minimized)
    }

    fn focused_app(&self) -> Option<App> {
        self.focused().map(|i| self.windows[i].app)
    }

    /// Bring a window to the top (showing it again if it was minimized).
    fn raise(&mut self, app: App) {
        if let Some(i) = self.index_of(app) {
            let mut w = self.windows.remove(i);
            w.minimized = false;
            self.windows.push(w);
        }
    }

    fn set_maximized(&mut self, app: App, max: bool) {
        let work = self.work();
        let Some(i) = self.index_of(app) else { return };
        let w = &mut self.windows[i];
        match (max, w.restore) {
            (true, None) => {
                w.restore = Some(w.rect());
                (w.x, w.y, w.w, w.h) = (work.x, work.y, work.w, work.h);
            }
            (false, Some(r)) => {
                w.restore = None;
                (w.x, w.y, w.w, w.h) = (r.x, r.y, r.w, r.h);
            }
            _ => {}
        }
    }

    /// Keep windows on screen after the desktop size changes.
    fn fit_windows(&mut self) {
        let work = self.work();
        for w in &mut self.windows {
            if w.restore.is_some() {
                (w.x, w.y, w.w, w.h) = (work.x, work.y, work.w, work.h);
                continue;
            }
            w.w = w.w.min(work.w);
            w.h = w.h.min(work.h);
            w.x = w.x.clamp(0, (work.w - w.w).max(0));
            w.y = w.y.clamp(0, (work.h - w.h).max(0));
        }
    }

    /// Open an app's window, or bring it forward if it's already open.
    fn open(&mut self, app: App) {
        self.apps_menu = None;
        if self.is_open(app) {
            self.raise(app);
            kprintln!("solon: switch to {}", app.name());
            return;
        }
        let work = self.work();
        let n = self.windows.len() as i32;
        let w = (work.w * 82 / 100).clamp(560.min(work.w), work.w);
        let h = (work.h * 86 / 100).clamp(360.min(work.h), work.h);
        let x = ((work.w - w) / 2 + (n % 6) * 28 - 70).clamp(0, (work.w - w).max(0));
        let y = ((work.h - h) / 2 + (n % 6) * 28 - 50).clamp(0, (work.h - h).max(0));
        self.windows.push(Window { app, x, y, w, h, minimized: false, restore: None });
        self.order.push(app);
        // The editors want all the room they can get.
        if matches!(app, App::Photos | App::Documents | App::Movie | App::Videos) {
            self.set_maximized(app, true);
        }
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
        if app == App::Documents && !self.docs.editing() {
            self.docs.refresh(&mut self.storage, &mut self.usb);
        }
        if app == App::Movie {
            self.movie.opened(&mut self.storage, &mut self.usb);
        }
        if app == App::Videos {
            self.videos.opened(&mut self.storage, &mut self.usb);
        }
    }

    fn close_app(&mut self, app: App) {
        if app == App::Movie {
            if self.movie.exporting() {
                self.toast = Some((String::from("Still making your video - wait for it to finish"), Self::now() + 4 * HZ));
                return;
            }
            self.movie.closed(&mut self.storage, &mut self.usb);
        }
        if app == App::Videos {
            self.videos.closed();
        }
        self.windows.retain(|w| w.app != app);
        self.order.retain(|a| *a != app);
        if matches!(self.drag, Some((a, ..)) if a == app) {
            self.drag = None;
        }
        self.capture = false;
        kprintln!("solon: close {}", app.name());
    }

    // ---- taskbar and Applications menu -------------------------------------------------

    fn apps_button(&self) -> Rect {
        Rect::new(6, self.h - BAR_H + 5, 150, BAR_H - 10)
    }

    /// Taskbar buttons for open windows (in opening order), squeezed to fit.
    fn task_buttons(&self) -> Vec<(App, Rect)> {
        let start = self.apps_button().x + self.apps_button().w + 10;
        let end = self.power_button().x - 230;
        let n = self.order.len().max(1) as i32;
        let w = ((end - start) / n - 6).clamp(44, 190);
        self.order.iter().enumerate().map(|(i, &a)| (a, Rect::new(start + i as i32 * (w + 6), self.h - BAR_H + 5, w, BAR_H - 10))).collect()
    }

    /// Columns in the Applications menu: 2, or 4 when the screen is too short for that.
    fn menu_cols(&self) -> usize {
        if 64 + 4 * 100 + BAR_H + 24 > self.h { 4 } else { 2 }
    }

    fn menu_box(&self) -> Rect {
        let cols = self.menu_cols() as i32;
        let rows = APPS.len().div_ceil(cols as usize) as i32;
        Rect::new(6, self.h - BAR_H - 12 - (64 + rows * 100), cols * 170 + 32, 64 + rows * 100)
    }

    fn menu_item(&self, i: usize) -> Rect {
        let b = self.menu_box();
        let cols = self.menu_cols();
        Rect::new(b.x + 16 + (i % cols) as i32 * 170, b.y + 52 + (i / cols) as i32 * 100, 166, 96)
    }

    fn draw_taskbar(&self, c: &mut Canvas) {
        let (w, h) = (self.w, self.h);
        let y = h - BAR_H;
        c.fill_rect_alpha(0, y, w, BAR_H, rgb(12, 16, 22), 215);
        c.fill_rect_alpha(0, y, w, 1, rgb(255, 255, 255), 30);
        // Applications button: a 3x3 grid of dots.
        let a = self.apps_button();
        let open = self.apps_menu.is_some();
        c.round_rect_alpha(a.x, a.y, a.w, a.h, 8, rgb(255, 255, 255), if open { 70 } else { 30 });
        for r in 0..3 {
            for k in 0..3 {
                c.round_rect(a.x + 12 + k * 7, a.y + 9 + r * 7, 5, 5, 1, TEXT);
            }
        }
        c.text(a.x + 44, a.y + 8, "Applications", Size::Small, true, TEXT);
        // Open windows.
        let focus = self.focused_app();
        for (app, b) in self.task_buttons() {
            let minimized = self.windows.iter().any(|w| w.app == app && w.minimized);
            let alpha = if focus == Some(app) { 70 } else if minimized { 12 } else { 30 };
            c.round_rect_alpha(b.x, b.y, b.w, b.h, 8, rgb(255, 255, 255), alpha);
            if focus == Some(app) {
                c.fill_rect(b.x + 10, b.y + b.h - 3, b.w - 20, 3, icons::tile_color(app));
            }
            c.round_rect(b.x + 8, b.y + 8, 18, 18, 5, icons::tile_color(app));
            if b.w > 70 {
                let name = crate::photos::ellipsize(app.name(), b.w - 40, Size::Small, false);
                c.text(b.x + 34, b.y + 8, &name, Size::Small, false, if minimized { TEXT_DIM } else { TEXT });
            }
        }
        // Clock, date, power.
        let clock = self.settings.clock_text(self.clock.0, self.clock.1);
        let cw = text_width(&clock, Size::Medium, true);
        let right = self.power_button().x - 10;
        c.text(right - cw, y + 11, &clock, Size::Medium, true, TEXT);
        if self.settings.show_date {
            let d = settings::date_text(self.date.0, self.date.1, self.date.2);
            let dw = text_width(&d, Size::Small, false);
            c.text(right - cw - 16 - dw, y + 13, &d, Size::Small, false, TEXT_DIM);
        }
    }

    fn draw_apps_menu(&self, c: &mut Canvas) {
        let Some(sel) = self.apps_menu else { return };
        let b = self.menu_box();
        c.round_rect_alpha(b.x + 3, b.y + 6, b.w, b.h, 14, 0, 100);
        c.round_rect(b.x, b.y, b.w, b.h, 14, rgb(246, 247, 250));
        c.text(b.x + 20, b.y + 16, "Applications", Size::Medium, true, rgb(40, 44, 52));
        for (i, &app) in APPS.iter().enumerate() {
            let r = self.menu_item(i);
            if i == sel {
                c.round_rect(r.x, r.y, r.w, r.h, 10, rgb(222, 232, 246));
            }
            icons::draw(c, app, r.x + (r.w - 64) / 2, r.y + 2);
            c.text_centered(r.x + r.w / 2, r.y + 72, app.name(), Size::Small, true, rgb(40, 44, 52));
        }
    }

    // ---- drawing ------------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas) {
        let (w, h) = (self.w, self.h);

        if self.screen_off {
            c.fill_rect(0, 0, w, h, 0);
            return;
        }

        // Background
        let (_, top, bottom) = settings::BACKGROUNDS[self.settings.background];
        c.gradient_v(0, 0, w, h, top, bottom);
        c.text_centered(w / 2, (h - BAR_H) / 2 - 16, "Solon", Size::Large, true, crate::gfx::mix(top, rgb(160, 180, 200), 80));
        if self.windows.is_empty() {
            c.text_centered(w / 2, (h - BAR_H) / 2 + 30, "Press Applications (bottom left) to start", Size::Small, false, crate::gfx::mix(top, rgb(200, 210, 220), 140));
        }

        // Windows, bottom to top.
        let focus = self.focused();
        for i in 0..self.windows.len() {
            let win = &self.windows[i];
            if win.minimized {
                continue;
            }
            let title = if win.app == App::Documents { self.docs.title() } else { None };
            draw_frame(c, win, title.as_deref().unwrap_or(win.app.name()), focus == Some(i));
            let body = win.body();
            let app = win.app;
            c.set_clip(body);
            match app {
                App::Photos => self.photos.draw(c, body),
                App::Import => self.import.draw(c, body, &self.usb, &self.storage_view),
                App::Settings => self.settings_app.draw(c, body, &self.settings, self.screen, self.storage.library.is_some()),
                App::Files => self.files.draw(c, body, &self.storage, &self.usb),
                App::Disks => self.disks.draw(c, body, &self.storage),
                App::Documents => self.docs.draw(c, body),
                App::Movie => self.movie.draw(c, body),
                App::Videos => self.videos.draw(c, body),
                app => draw_placeholder(c, app, body),
            }
            c.reset_clip();
        }

        self.draw_taskbar(c);
        if let Some((msg, _)) = &self.toast {
            let tw = text_width(msg, Size::Small, true) + 32;
            let y = h - BAR_H - 40;
            c.round_rect_alpha(w / 2 - tw / 2 + 2, y + 3, tw, 28, 14, 0, 80);
            c.round_rect(w / 2 - tw / 2, y, tw, 28, 14, rgb(243, 156, 18));
            c.text_centered(w / 2, y + 6, msg, Size::Small, true, rgb(30, 30, 30));
        }
        self.draw_apps_menu(c);
        if self.install_notice {
            self.draw_install_notice(c);
        }
        self.draw_power(c);
        if self.show_log {
            self.draw_log(c);
        }
    }

    // ---- input --------------------------------------------------------------------------

    fn app_wheel(&mut self, app: App, wheel: i8, x: i32, y: i32, body: Rect) -> bool {
        match app {
            App::Photos => self.photos.on_wheel(wheel, body),
            App::Import => self.import.on_wheel(wheel, body, &self.usb),
            App::Files => self.files.on_wheel(wheel, body),
            App::Disks => self.disks.on_wheel(wheel, body),
            App::Documents => self.docs.on_wheel(wheel, body),
            App::Movie => self.movie.on_wheel(wheel, x, y, body),
            App::Videos => self.videos.on_wheel(wheel, body),
            _ => false,
        }
    }

    fn app_press(&mut self, app: App, x: i32, y: i32, body: Rect, now: u64) -> bool {
        let mut redraw = false;
        match app {
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
                let a = self.disks.on_press(x, y, body, &self.storage, &self.usb);
                redraw |= self.disk_action(a, now);
            }
            App::Documents => {
                let a = self.docs.on_press(x, y, body, now, &mut self.storage, &mut self.usb);
                self.capture = true;
                redraw |= self.docs_action(a, now);
            }
            App::Movie => {
                let a = self.movie.on_press(x, y, body, now, &mut self.storage, &mut self.usb);
                self.capture = true;
                redraw |= self.movie_action(a, now);
            }
            App::Videos => {
                let a = self.videos.on_press(x, y, body, now, &mut self.storage, &mut self.usb);
                self.capture = true;
                redraw |= matches!(a, VideosAction::Redraw);
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
        redraw
    }

    fn movie_action(&mut self, a: MovieAction, now: u64) -> bool {
        match a {
            MovieAction::None => false,
            MovieAction::Redraw => true,
            MovieAction::Toast(msg) => {
                self.toast = Some((msg, now + 5 * HZ));
                true
            }
        }
    }

    /// A key for an app. None = the app didn't want it.
    fn app_key(&mut self, app: App, sc: u8, body: Rect) -> Option<bool> {
        match app {
            App::Photos => self.photos.on_key(sc, body).map(|r| r | self.photo_action()),
            App::Import => self.import.on_key(sc, body, &mut self.usb, &self.storage_view).map(|a| self.apply(a)),
            App::Files => self.files.on_key(sc, body, &mut self.storage, &mut self.usb).map(|a| self.files_action(a, Self::now())),
            App::Disks => self.disks.on_key(sc).map(|a| self.disk_action(a, Self::now())),
            App::Documents => self.docs.on_key(sc, body, &mut self.storage, &mut self.usb).map(|a| self.docs_action(a, Self::now())),
            App::Movie => self.movie.on_key(sc).map(|a| self.movie_action(a, Self::now())),
            App::Videos => self.videos.on_key(sc).map(|a| matches!(a, VideosAction::Redraw)),
            _ => None,
        }
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

        // The wheel scrolls whichever window is under the pointer.
        if wheel != 0 && self.apps_menu.is_none() {
            if let Some(i) = self.windows.iter().rposition(|w| !w.minimized && w.rect().contains(x, y)) {
                let (app, body) = (self.windows[i].app, self.windows[i].body());
                redraw |= self.app_wheel(app, wheel, x, y, body);
            }
        }

        // A press inside Photos/Documents keeps getting moves until release (editor drags).
        if self.capture {
            if let Some(i) = self.focused() {
                let (app, body) = (self.windows[i].app, self.windows[i].body());
                if released {
                    self.capture = false;
                    return redraw | match app {
                        App::Documents => self.docs.on_release(),
                        App::Movie => self.movie.on_release(),
                        App::Videos => self.videos.on_release(),
                        _ => self.photos.on_release(),
                    };
                }
                if buttons & 1 != 0 {
                    return redraw | match app {
                        App::Documents => self.docs.on_drag(x, y, body),
                        App::Movie => self.movie.on_drag(x, y, body),
                        App::Videos => self.videos.on_drag(x, y, body),
                        _ => self.photos.on_drag(x, y, body),
                    };
                }
            }
            self.capture = false;
        }

        if let Some((app, ox, oy)) = self.drag {
            if released {
                self.drag = None;
                return redraw;
            }
            let work = self.work();
            if let Some(i) = self.index_of(app) {
                let win = &mut self.windows[i];
                win.x = (x - ox).clamp(-win.w + 120, work.w - 120);
                win.y = (y - oy).clamp(0, work.h - TITLE_H);
                return true;
            }
        }

        if !pressed {
            return redraw;
        }
        kprintln!("solon: click at {},{}", x, y);
        if self.install_notice {
            let (open, later) = (self.notice_open(), self.notice_later());
            if open.contains(x, y) || later.contains(x, y) {
                self.dismiss_install_notice(open.contains(x, y));
            }
            return true;
        }
        if let Some(r) = self.power_click(x, y) {
            self.apps_menu = None;
            return r;
        }

        // Applications menu and taskbar.
        if self.apps_menu.is_some() {
            if let Some(i) = (0..APPS.len()).find(|&i| self.menu_item(i).contains(x, y)) {
                self.open(APPS[i]);
                return true;
            }
            let on_menu = self.menu_box().contains(x, y);
            let on_button = self.apps_button().contains(x, y);
            self.apps_menu = None;
            if on_menu || on_button {
                return true;
            }
            redraw = true;
        } else if self.apps_button().contains(x, y) {
            self.apps_menu = Some(0);
            return true;
        }
        if y >= self.h - BAR_H {
            if let Some((app, _)) = self.task_buttons().into_iter().find(|(_, b)| b.contains(x, y)) {
                if self.focused_app() == Some(app) {
                    if let Some(i) = self.index_of(app) {
                        self.windows[i].minimized = true;
                        self.capture = false;
                    }
                } else {
                    self.raise(app);
                }
            }
            return true;
        }

        // Windows, top first.
        let Some(i) = self.windows.iter().rposition(|w| !w.minimized && w.rect().contains(x, y)) else { return redraw };
        let app = self.windows[i].app;
        if self.focused_app() != Some(app) {
            self.raise(app);
            redraw = true;
        }
        let i = self.index_of(app).unwrap();
        let win = &self.windows[i];
        if win.close_box().contains(x, y) {
            self.close_app(app);
            return true;
        }
        if win.max_box().contains(x, y) {
            let max = win.restore.is_none();
            self.set_maximized(app, max);
            return true;
        }
        if win.min_box().contains(x, y) {
            self.windows[i].minimized = true;
            return true;
        }
        if win.title_bar().contains(x, y) {
            let double = matches!(self.last_title, Some((a, t)) if a == app && now - t <= DOUBLE_CLICK_TICKS);
            self.last_title = Some((app, now));
            if double {
                let max = win.restore.is_none();
                self.set_maximized(app, max);
                self.last_title = None;
                return true;
            }
            if win.restore.is_none() {
                self.drag = Some((app, x - win.x, y - win.y));
            }
            return redraw;
        }
        let body = win.body();
        if body.contains(x, y) {
            redraw |= self.app_press(app, x, y, body, now);
        }
        redraw
    }

    /// Scancode set 1. Returns true if the screen needs redrawing.
    pub fn on_key(&mut self, sc: u8) -> bool {
        if self.install_notice && sc & 0x80 == 0 {
            match sc {
                0x1C => self.dismiss_install_notice(true),
                0x01 => self.dismiss_install_notice(false),
                _ => return false,
            }
            return true;
        }
        if sc == 0x58 {
            // F12: diagnostics (kernel log)
            self.show_log = !self.show_log;
            self.log_scroll = 0;
            return true;
        }
        if self.show_log {
            let page = ((self.h - 90) / 18).max(1) as usize;
            match sc {
                0x01 => self.show_log = false,
                0x48 => self.log_scroll += 1,
                0x50 => self.log_scroll = self.log_scroll.saturating_sub(1),
                0x49 => self.log_scroll += page - 1,
                0x51 => self.log_scroll = self.log_scroll.saturating_sub(page - 1),
                0x47 => self.log_scroll = usize::MAX / 2,
                0x4F => self.log_scroll = 0,
                _ => return false,
            }
            return true;
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
        // Applications menu: arrows move, Enter opens, Esc closes.
        if let Some(sel) = self.apps_menu {
            let n = APPS.len();
            match sc {
                0x01 => self.apps_menu = None,
                0x1C => self.open(APPS[sel]),
                0x4B => self.apps_menu = Some((sel + n - 1) % n),
                0x4D => self.apps_menu = Some((sel + 1) % n),
                0x48 => self.apps_menu = Some((sel + n - self.menu_cols()) % n),
                0x50 => self.apps_menu = Some((sel + self.menu_cols()) % n),
                _ => return false,
            }
            return true;
        }
        if let Some(i) = self.focused() {
            let (app, body) = (self.windows[i].app, self.windows[i].body());
            if let Some(r) = self.app_key(app, sc, body) {
                return r;
            }
            if sc == 0x01 {
                // Esc closes the window
                self.close_app(app);
                return true;
            }
            return false;
        }
        // Nothing open: Enter or Space opens the Applications menu.
        if sc == 0x1C || sc == 0x39 {
            self.apps_menu = Some(0);
            return true;
        }
        false
    }

    pub fn set_clock(&mut self, h: u8, m: u8) -> bool {
        let changed = self.clock != (h, m);
        self.clock = (h, m);
        changed
    }
}

fn draw_frame(c: &mut Canvas, win: &Window, title: &str, focused: bool) {
    let (x, y, w, h) = (win.x, win.y, win.w, win.h);
    let round = if win.restore.is_some() { 0 } else { 10 };
    if round > 0 {
        c.round_rect_alpha(x + 4, y + 8, w, h, 10, 0, if focused { 100 } else { 60 }); // shadow
    }
    c.round_rect(x, y, w, h, round, WIN_BG);
    let bar = if focused { rgb(226, 232, 242) } else { rgb(238, 240, 243) };
    c.round_rect(x, y, w, TITLE_H + 12, round, bar);
    c.fill_rect(x, y + TITLE_H, w, 12, WIN_BG);
    c.fill_rect(x, y + TITLE_H, w, 1, rgb(210, 214, 220));

    // Title with a small app-coloured dot
    c.circle(x + 20, y + TITLE_H / 2, 6, icons::tile_color(win.app));
    let title = crate::photos::ellipsize(title, w - 3 * TITLE_H - 50, Size::Medium, true);
    c.text(x + 34, y + 8, &title, Size::Medium, true, if focused { rgb(30, 34, 42) } else { rgb(120, 126, 136) });

    let ink = rgb(70, 76, 86);
    // Minimize: a line.
    let m = win.min_box();
    c.fill_rect(m.x + m.w / 2 - 7, m.y + m.h / 2 + 4, 14, 2, ink);
    // Maximize / restore: a square (two when maximized).
    let b = win.max_box();
    let (bx, by) = (b.x + b.w / 2 - 7, b.y + b.h / 2 - 6);
    if win.restore.is_some() {
        c.outline_rect(bx + 3, by - 2, 11, 10, ink);
        c.fill_rect(bx, by + 2, 11, 10, bar);
        c.outline_rect(bx, by + 2, 11, 10, ink);
    } else {
        c.outline_rect(bx, by, 14, 12, ink);
        c.fill_rect(bx, by, 14, 2, ink);
    }
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

/// Compose `pages` pages (made one at a time by `make`) into a PCL or IPP job.
fn build_job(pages: usize, layout: &Layout, printer: &PrinterInfo, make: &mut dyn FnMut(usize) -> print::Page) -> Result<Vec<u8>, &'static str> {
    let dpi = printer.dpi;
    let mut pcl = if printer.ipp { None } else { Some(print::PclJob::start(layout)) };
    let mut pwg = if printer.ipp { Some(print::PwgJob::start()) } else { None };
    for n in 0..pages {
        let mut page = make(n);
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
            j.page(&page, layout, dpi, pages as u32);
        }
        kprintln!("print: page {} of {} ready ({}x{} at {} dpi)", n + 1, pages, page.w, page.h, dpi);
    }
    match (pcl, pwg) {
        (Some(j), _) => Ok(j.finish()),
        (_, Some(j)) => Ok(print::ipp_print_job(layout, &j.finish())),
        _ => Err("No printer language"),
    }
}

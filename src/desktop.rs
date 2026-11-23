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
use crate::settingsui::{Page, SettingsAction, SettingsApp};
use crate::files::{CopyJob, Files, FilesAction};
use crate::diskmgr::{DiskAction, DiskMgr};
use crate::documents::{DocSrc, DocsAction, DocsApp};
use crate::movieui::{MovieAction, MovieApp};
use crate::videos::{VideosAction, VideosApp};
use crate::usb::Usb;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum App {
    Photos,
    PhotoFrame,
    Videos,
    Movie,
    Documents,
    Printers,
    Backup,
    Import,
    Files,
    Disks,
    Settings,
    Mines,
    Solitaire,
    FreeCell,
    Sudoku,
    Pinball,
    Shooter,
    Writer,
    Pdf,
    TextEdit,
    Calculator,
    Calendar,
    Paint,
}

impl App {
    pub fn name(self) -> &'static str {
        match self {
            App::Photos => "Photos",
            App::PhotoFrame => "Photo Frame",
            App::Videos => "Videos",
            App::Movie => "Video Editor",
            App::Documents => "Spreadsheets",
            App::Writer => "Writer",
            App::Pdf => "PDF",
            App::TextEdit => "Text Editor",
            App::Calculator => "Calculator",
            App::Calendar => "Calendar",
            App::Paint => "Paint",
            App::Printers => "Printers",
            App::Backup => "Backup",
            App::Import => "Import",
            App::Files => "Files",
            App::Disks => "Disks",
            App::Settings => "Settings",
            App::Mines => "Mines",
            App::Solitaire => "Solitaire",
            App::FreeCell => "FreeCell",
            App::Sudoku => "Sudoku",
            App::Pinball => "Star Pinball",
            App::Shooter => "Starbase Breach",
        }
    }


    /// The game this app is, if it's one.
    pub fn game(self) -> Option<crate::games::Game> {
        use crate::games::Game;
        Some(match self {
            App::Mines => Game::Mines,
            App::Solitaire => Game::Solitaire,
            App::FreeCell => Game::FreeCell,
            App::Sudoku => Game::Sudoku,
            App::Pinball => Game::Pinball,
            App::Shooter => Game::Shooter,
            _ => return None,
        })
    }
    fn placeholder(self) -> [&'static str; 2] {
        match self {
            App::Photos => ["No photos yet.", "Plug in a USB drive or SD card, then open Import."],
            App::Videos => ["No videos yet.", "Plug in a USB drive or SD card, then open Import."],
            App::Documents => ["No documents yet.", "Plug in a USB drive or SD card, then open Import."],
            App::Printers => ["No printers found.", "Connect a USB printer to this computer."],
            App::Import => ["No USB drives or cameras detected.", "USB support arrives in an upcoming milestone."],
            App::Settings => ["", ""],
            _ => ["", ""],
        }
    }
}

/// Room kept along the bottom for the dock (windows stay above it).
const BAR_H: i32 = 66;
/// The dock: a floating rounded bar centred at the bottom.
const DOCK_H: i32 = 56;
const DOCK_ICON: i32 = 42;
const DOCK_GAP: i32 = 8;
const TITLE_H: i32 = 38;
const DOUBLE_CLICK_TICKS: u64 = 50; // 0.5 s at 100 Hz

const INK: u32 = rgb(28, 32, 40);
const INK_DIM: u32 = rgb(96, 104, 116);
const WIN_BG: u32 = rgb(251, 251, 253);

/// The Applications menu, in order (folders open in place).
const APPS: [MenuEntry; 15] = [
    MenuEntry::App(App::Photos),
    MenuEntry::App(App::PhotoFrame),
    MenuEntry::Folder(Folder::Documents),
    MenuEntry::App(App::Files),
    MenuEntry::App(App::Import),
    MenuEntry::App(App::Videos),
    MenuEntry::App(App::Movie),
    MenuEntry::App(App::Paint),
    MenuEntry::Folder(Folder::Games),
    MenuEntry::App(App::Calendar),
    MenuEntry::App(App::Calculator),
    MenuEntry::App(App::Printers),
    MenuEntry::App(App::Backup),
    MenuEntry::App(App::Disks),
    MenuEntry::App(App::Settings),
];

/// Always in the dock (after the Applications button).
const PINNED: [MenuEntry; 8] = [
    MenuEntry::App(App::Photos),
    MenuEntry::App(App::Videos),
    MenuEntry::Folder(Folder::Documents),
    MenuEntry::App(App::Files),
    MenuEntry::Folder(Folder::Games),
    MenuEntry::App(App::Calendar),
    MenuEntry::App(App::Calculator),
    MenuEntry::App(App::Settings),
];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Folder {
    Games,
    Documents,
}

impl Folder {
    fn name(self) -> &'static str {
        match self {
            Folder::Games => "Games",
            Folder::Documents => "Documents",
        }
    }
    fn apps(self) -> &'static [App] {
        match self {
            Folder::Games => &[App::Solitaire, App::Mines, App::FreeCell, App::Sudoku, App::Pinball, App::Shooter],
            Folder::Documents => &[App::Writer, App::Documents, App::Pdf, App::TextEdit],
        }
    }
    fn icon(self) -> icons::Icon {
        match self {
            Folder::Games => icons::Icon::Games,
            Folder::Documents => icons::Icon::Documents,
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum MenuEntry {
    App(App),
    Folder(Folder),
    Back,
}

impl MenuEntry {
    fn icon(self) -> icons::Icon {
        match self {
            MenuEntry::App(a) => icons::Icon::App(a),
            MenuEntry::Folder(f) => f.icon(),
            MenuEntry::Back => icons::Icon::Back,
        }
    }
    fn name(self) -> &'static str {
        match self {
            MenuEntry::App(a) => a.name(),
            MenuEntry::Folder(f) => f.name(),
            MenuEntry::Back => "Back",
        }
    }
}

/// What's in the dock.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum DockItem {
    Apps,
    Entry(MenuEntry),
    Power,
}

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

/// A phone's newest document, offered to continue on this computer.
struct Handoff {
    phone: u32,
    phone_name: String,
    vol: usize,
    entry: DirEntry,
}

/// Kinds of files offered to continue working on (the documents Solon's apps edit).
const HANDOFF_KINDS: [&str; 6] = ["docx", "xlsx", "csv", "pdf", "txt", "md"];

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
    /// The right-click menu on the desktop: where it opened, and the item under the pointer.
    desk_menu: Option<(i32, i32, Option<usize>)>,
    /// "Continue working on your last document?" for a phone's newest document, and the
    /// phones already looked at (until they're unplugged).
    handoff: Option<Handoff>,
    handoff_seen: Vec<u32>,
    /// The Applications menu is showing a folder.
    folder: Option<Folder>,
    /// Icons drawn small, for the dock (42 px) and title bars (22 px).
    dock_icons: Vec<(icons::Icon, crate::image::Image)>,
    title_icons: Vec<(icons::Icon, crate::image::Image)>,
    /// The wallpaper, drawn once per size and choice.
    wallpaper: Option<Wall>,
    /// The user's own desktop picture (file bytes), once looked for in Solon storage.
    custom_wallpaper: Option<Vec<u8>>,
    custom_checked: bool,
    games: crate::games::Games,
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
    /// A new screen resolution being tried: the one to go back to, and when.
    display_trial: Option<(Option<(u32, u32)>, u64)>,
    display_checked: bool,
    /// The internal disk Solon started from (it's installed there), for update notices.
    installed_disk: Option<usize>,
    confirm: Option<PowerRequest>,
    screen_off: bool,
    files: Files,
    disks: DiskMgr,
    copy: Option<CopyJob>,
    docs: DocsApp,
    /// Writer, PDF and the Text Editor.
    doc_apps: Vec<(App, alloc::boxed::Box<dyn crate::docshell::DocApp>)>,
    movie: MovieApp,
    calculator: crate::calculator::Calculator,
    printers_app: crate::printers::PrintersApp,
    backup: crate::backup::BackupApp,
    frame_app: crate::frame::FrameApp,
    /// The photo frame / screensaver, while it's showing.
    slideshow: Option<crate::frame::Slideshow>,
    /// A scroll bar being dragged: the app, and where on the bar it was grabbed.
    sb_drag: Option<(App, i32)>,
    /// When the mouse or keyboard was last used, and where the pointer was.
    last_input: u64,
    pointer: (i32, i32),
    calendar: crate::calendar::CalendarApp,
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

/// The user's own desktop picture in Solon storage (JPEG or PNG).
const CUSTOM_NAMES: [&str; 2] = ["Desktop picture.jpg", "Desktop picture.png"];

/// The desktop wallpaper at one size, with the colours for text drawn on it.
struct Wall {
    bg: usize,
    w: i32,
    h: i32,
    px: Vec<u32>,
    /// Clock and date colour, and whether they need a shadow; the hint along the bottom.
    ink: (u32, bool),
    hint: (u32, bool),
}

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
            desk_menu: None,
            handoff: None,
            handoff_seen: Vec::new(),
            folder: None,
            dock_icons: Vec::new(),
            title_icons: Vec::new(),
            wallpaper: None,
            custom_wallpaper: None,
            custom_checked: false,
            games: crate::games::Games::new(),
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
            display_trial: None,
            display_checked: false,
            installed_disk: None,
            confirm: None,
            screen_off: false,
            files: Files::new(),
            disks: DiskMgr::new(),
            copy: None,
            docs: DocsApp::new(),
            doc_apps: crate::docapps::all(),
            movie: MovieApp::new(),
            calculator: crate::calculator::Calculator::new(),
            printers_app: crate::printers::PrintersApp::new(),
            backup: crate::backup::BackupApp::new(),
            frame_app: crate::frame::FrameApp::new(),
            slideshow: None,
            sb_drag: None,
            last_input: 0,
            pointer: (-1, -1),
            calendar: crate::calendar::CalendarApp::new(),
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
    /// The resolution the desktop is now shown at.
    pub fn set_screen(&mut self, res: (usize, usize)) {
        self.screen = res;
    }

    pub fn resize(&mut self, w: usize, h: usize) {
        self.w = w as i32;
        self.h = h as i32;
        self.fit_windows();
    }

    fn apply_settings(&mut self, n: Settings) {
        let old = self.settings;
        self.settings = n;
        crate::ps2::set_speed(settings::MOUSE[n.mouse].1);
        crate::spell::set_mode(n.spelling);
        if n.size != old.size || n.resolution != old.resolution {
            self.request = Some(PowerRequest::Rescale);
        }
        kprintln!("settings: {:?}", n);
        if self.storage.library.is_some() {
            if let Err(e) = self.storage.write_root_file(&mut self.usb, "Settings.txt", n.to_text().as_bytes()) {
                kprintln!("settings: couldn't save: {:?}", e);
            }
        }
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
        // (The button itself is drawn with the dock.)
        if self.power_menu {
            let top = self.power_item(0);
            let (x, y, w, h) = (top.x - 6, top.y - 6, top.w + 12, 3 * 46 + 8);
            c.shadow(x, y + 4, w, h, 14, 18, 80);
            c.round_rect(x, y, w, h, 14, WIN_BG);
            for (i, label) in ["Turn off screen", "Restart", "Shut down"].iter().enumerate() {
                let r = self.power_item(i as i32);
                c.round_rect(r.x, r.y, r.w, r.h, 10, rgb(238, 241, 246));
                c.text(r.x + 16, r.y + 12, label, Size::Small, true, INK);
            }
        }
        if let Some(req) = self.confirm {
            c.fill_rect_alpha(0, 0, self.w, self.h, 0, 110);
            let b = self.confirm_box();
            c.shadow(b.x, b.y + 8, b.w, b.h, 16, 28, 110);
            c.round_rect(b.x, b.y, b.w, b.h, 16, WIN_BG);
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

    /// No mouse pointer: the screen is off or the photo frame is showing.
    pub fn hide_pointer(&self) -> bool {
        self.screen_off || self.slideshow.is_some()
    }

    fn start_slideshow(&mut self, manual: bool) {
        self.apps_menu = None;
        self.folder = None;
        self.capture = false;
        self.slideshow = Some(crate::frame::Slideshow::new(&mut self.storage, &mut self.usb, &self.settings, manual));
        self.last_input = Self::now();
    }

    fn end_slideshow(&mut self) {
        self.slideshow = None;
        self.last_input = Self::now();
        kprintln!("frame: stopped");
    }

    pub fn set_date(&mut self, d: (u16, u8, u8)) -> bool {
        let changed = self.date != d;
        self.date = d;
        changed
    }

    /// True while there's background work (photos to decode, an import), so the main loop shouldn't sleep.
    pub fn busy(&self) -> bool {
        self.photos.needs_loading() || self.job.is_some() || self.copy.is_some() || self.disks.busy() || self.movie.exporting() || self.backup.busy() || self.import.busy() || self.slideshow.as_ref().map_or(false, |s| s.busy())
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
            if self.is_open(App::Videos) && !self.videos.playing() {
                self.videos.opened(&mut self.storage, &mut self.usb);
            }
            if !self.backup.busy() {
                let before: Vec<(u32, usize)> = self.backup.drives.iter().map(|d| (d.drive, d.vol)).collect();
                self.backup.refresh(&mut self.storage, &mut self.usb);
                // A backup drive was plugged in and the last backup is a week old: suggest one.
                let due = self.backup.last.as_ref().map_or(true, |l| l.age(self.date) >= 7);
                if let Some(d) = self.backup.drives.iter().find(|d| !d.backups.is_empty() && !before.contains(&(d.drive, d.vol))) {
                    if due && self.storage.library.is_some() && !self.is_open(App::Backup) {
                        self.toast = Some((format!("{} has your backups - open Backup to back up again", d.name), now + 10 * HZ));
                    }
                }
            }
            if self.is_open(App::Documents) && !self.docs.editing() {
                self.docs.refresh(&mut self.storage, &mut self.usb);
            }
            let usb = &self.usb;
            self.photos.drive_removed(&|id| !usb.has_drive(id));
            redraw = true;
        }
        if self.slideshow.is_none() && !self.screen_off && self.display_trial.is_none() && !self.videos.playing() && !self.movie.playing() {
            if let Some(wait) = crate::frame::wait_ticks(&self.settings) {
                if now >= self.last_input + wait {
                    self.start_slideshow(false);
                    redraw = true;
                }
            }
        }
        if let Some(ss) = &mut self.slideshow {
            redraw |= ss.tick(now, &mut self.storage, &mut self.usb);
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
            let visible = !win.minimized || self.videos.fullscreen();
            redraw |= matches!(self.videos.tick(now, &mut self.storage, &mut self.usb, visible), VideosAction::Redraw);
        }
        // Started in a resolution being tried? Ask whether to keep it; go back if nobody answers.
        // Load the calendar once Solon storage is there (for today's events on the desktop).
        if !self.calendar.loaded() && self.storage.library.is_some() {
            let t = self.storage.read_root_file(&mut self.usb, "Calendar.txt");
            self.calendar.load(t.as_deref().and_then(|b| core::str::from_utf8(b).ok()));
            redraw = true;
        }
        // A phone that's ready: offer to continue its newest document here.
        let phone_ids: Vec<u32> = self.usb.phones.iter().map(|p| p.id).collect();
        self.handoff_seen.retain(|id| phone_ids.contains(id));
        if self.handoff.as_ref().is_some_and(|h| !phone_ids.contains(&h.phone)) {
            self.handoff = None;
            redraw = true;
        }
        let ready = self.usb.phones.iter().find(|p| p.problem.is_none() && !p.storages.is_empty() && !self.handoff_seen.contains(&p.id)).map(|p| (p.id, p.name.clone()));
        if let Some((id, name)) = ready {
            self.handoff_seen.push(id);
            let wanted = |n: &str| crate::docshell::has_ext(n, &HANDOFF_KINDS);
            if let Some((vol, entry)) = self.usb.phone_latest_document(id, &wanted) {
                kprintln!("handoff: \"{}\" on {} is the newest document", entry.name, name);
                self.handoff = Some(Handoff { phone: id, phone_name: name, vol, entry });
                redraw = true;
            }
        }
        // Words added to the spelling dictionary are kept in Dictionary.txt.
        if self.storage.library.is_some() && crate::spell::take_dirty() {
            if let Err(e) = self.storage.write_root_file(&mut self.usb, "Dictionary.txt", crate::spell::user_text().as_bytes()) {
                kprintln!("spell: couldn't save the dictionary: {:?}", e);
            }
        }
        // The user's own desktop picture, for its preview in Settings.
        if !self.custom_checked && self.storage.library.is_some() {
            let _ = self.custom_image();
        }
        if !self.backup.last_loaded && self.storage.library.is_some() {
            self.backup.load_last(&mut self.storage, &mut self.usb);
            redraw = true;
        }
        if self.import.busy() {
            redraw |= self.import.step(&mut self.usb, now);
        }
        if self.backup.busy() {
            let a = self.backup.step(&mut self.storage, &mut self.usb, now);
            redraw |= self.backup_action(a, now);
        }
        if !self.display_checked {
            self.display_checked = true;
            if let Ok(text) = crate::display::read_config(&mut self.storage, &mut self.usb) {
                if let Some(prev) = crate::display::trial(&text) {
                    kprintln!("display: trying a new resolution (previous {:?})", prev);
                    self.display_trial = Some((prev, now + 15 * HZ));
                    redraw = true;
                }
            }
        }
        if let Some((_, end)) = self.display_trial {
            if now >= end {
                self.end_display_trial(false);
            }
            if now % HZ == 0 {
                redraw = true;
            }
        }
        // Games: timers, animations, pinball physics, sound; save new best scores.
        let visible: Vec<crate::games::Game> = self.windows.iter().filter(|w| !w.minimized).filter_map(|w| w.app.game()).collect();
        let focused_game = self.focused_app().and_then(|a| a.game());
        if !visible.is_empty() || focused_game.is_some() {
            redraw |= self.games.tick(&visible, focused_game, now);
        }
        if let Some(text) = self.games.take_save() {
            if self.storage.library.is_some() {
                if let Err(e) = self.storage.write_root_file(&mut self.usb, "Games.txt", text.as_bytes()) {
                    kprintln!("games: couldn't save scores: {:?}", e);
                }
            }
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
    /// F12, then S: the log (and what the NVIDIA driver read: VBIOS, registers) to the Solon
    /// stick's boot partition, to read on another computer.
    fn save_diagnostics(&mut self) {
        let mut text = String::new();
        for line in crate::serial::LOG.lock().lines() {
            text.push_str(&String::from_utf8_lossy(&line));
            text.push_str("\r\n");
        }
        let fw_edid = crate::display::INFO.lock().as_ref().map(|i| i.edid.clone()).unwrap_or_default();
        let report = crate::nvidia::REPORT.lock();
        let edid_names: Vec<String> = (0..16).map(|k| format!("NVEDID{:X}.BIN", k)).collect();
        let mut files: Vec<(&str, &[u8])> = Vec::from([("SOLONLOG.TXT", text.as_bytes())]);
        if !fw_edid.is_empty() {
            files.push(("FWEDID.BIN", &fw_edid));
        }
        if let Some(r) = report.as_ref() {
            if !r.vbios.is_empty() {
                files.push(("NVVBIOS.ROM", &r.vbios));
            }
            files.push(("NVREGS.BIN", &r.regs));
            for (k, c) in r.connectors.iter().enumerate().take(16) {
                if !c.edid.is_empty() {
                    files.push((&edid_names[k], &c.edid));
                }
            }
        }
        let names: Vec<&str> = files.iter().map(|f| f.0).collect();
        // USB sticks sometimes fail one transfer; a second try usually works.
        let saved = crate::display::write_boot_files(&mut self.storage, &mut self.usb, &files).or_else(|e| {
            kprintln!("diagnostics: {} (trying again)", e);
            crate::display::write_boot_files(&mut self.storage, &mut self.usb, &files)
        });
        let msg = match saved {
            Ok(()) => format!("Saved {} on the Solon stick", names.join(", ")),
            Err(e) => format!("Couldn't save to the Solon stick: {}", e),
        };
        drop(report);
        kprintln!("diagnostics: {}", msg);
        self.toast = Some((msg, Self::now() + 8 * HZ));
    }

    fn draw_log(&self, c: &mut Canvas) {
        let (w, h) = (self.w, self.h);
        c.fill_rect_alpha(0, 0, w, h, rgb(10, 12, 16), 235);
        c.text(24, 16, "Solon diagnostics", Size::Medium, true, rgb(255, 255, 255));
        let lines = crate::serial::LOG.lock().lines();
        let max = ((h - 90) / 18).max(1) as usize;
        let end = lines.len() - self.log_scroll.min(lines.len().saturating_sub(max.min(lines.len())));
        let start = end.saturating_sub(max);
        let info = format!(
            "Screen {}x{}   |   Build {}   |   Lines {}-{} of {}   |   Up/Down, Page Up/Down, Home/End to scroll   |   S: save to the stick   |   F12 or Esc to close",
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
            PhotoAction::Wallpaper(i) => self.use_as_wallpaper(i),
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
            .map(|p| PrinterInfo { id: PrinterId::Usb(p.id), name: p.name.clone(), ipp: p.ipp, lang: p.lang.unwrap_or(print::Lang::Pcl), color: p.color(), papers: p.papers(), dpi: p.dpi() })
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
        kprintln!("print: {:?} job {} bytes, {} page(s), {} cop{}", printer.lang, bytes.len(), pages, layout.copies, if layout.copies == 1 { "y" } else { "ies" });
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

    /// Re-decode the original at full resolution, apply the edits, save as JPEG (PNG if it has transparency).
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
        // Photos with transparent parts (PNG, GIF, WebP...) stay PNG; the rest become JPEG.
        let (jpeg, ext) = if out.opaque { (crate::edit::encode_jpeg(&mut out, 92)?, "jpg") } else { (crate::image::encode_png(&out), "png") };
        let (w, h) = (out.w, out.h);
        drop(out);
        let saved = match (choice, &src) {
            (SaveChoice::Replace, Source::Library { dir, entry }) => {
                let e = self.storage.replace(&mut self.usb, dir, entry, &jpeg, ext).map_err(|e| e.message())?;
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
                let (dir, e) = self.storage.save_copy(&mut self.usb, folder, &name, &jpeg, ext).map_err(|e| e.message())?;
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

    /// The wallpaper for background `bg` at w x h, and the text colours that suit it.
    fn make_wallpaper(&mut self, bg: usize, w: i32, h: i32) -> Wall {
        use crate::wallpaper::{CUSTOM, FIRST_PHOTO, PHOTOS};
        let (wu, hu) = (w as usize, h as usize);
        if bg < FIRST_PHOTO {
            let ink = crate::wallpaper::ink(bg);
            return Wall { bg, w, h, px: crate::wallpaper::make(bg, wu, hu), ink: (ink, false), hint: (ink, false) };
        }
        let img = if bg == CUSTOM { self.custom_image() } else { None };
        let img = img.or_else(|| crate::image::decode(PHOTOS[if bg < CUSTOM { bg - FIRST_PHOTO } else { 0 }].1).ok());
        let px = match &img {
            Some(img) => crate::wallpaper::photo(img, wu, hu),
            None => crate::wallpaper::make(0, wu, hu),
        };
        let big = (h / 9).clamp(56, 120);
        let top = h / 5 + big;
        let mut px = px;
        let ink = crate::wallpaper::ink_for(&px, wu, hu, (w / 4, top - big, w / 2, big + big / 2 + 40));
        if ink.1 {
            // White clock: darken softly behind it so it reads on any photo.
            crate::wallpaper::soft_shade(&mut px, wu, hu, w / 2, top - big / 4, w / 3, big * 3 / 2, 90);
        }
        let hint = crate::wallpaper::ink_for(&px, wu, hu, (w / 4, h - BAR_H - 52, w / 2, 28));
        kprintln!("solon: wallpaper {} at {}x{}", crate::wallpaper::name(bg), w, h);
        Wall { bg, w, h, px, ink, hint }
    }

    /// The user's own desktop picture, decoded (read from Solon storage the first time).
    fn custom_image(&mut self) -> Option<crate::image::Image> {
        if self.custom_wallpaper.is_none() && !self.custom_checked && self.storage.library.is_some() {
            self.custom_checked = true;
            self.custom_wallpaper = CUSTOM_NAMES.iter().find_map(|n| self.storage.read_root_file(&mut self.usb, n));
        }
        let img = crate::image::decode(self.custom_wallpaper.as_ref()?).ok()?;
        self.settings_app.set_custom_thumb(Some(crate::image::Image { w: 192, h: 120, px: crate::frame::cover(&img, 192, 120), opaque: true }));
        Some(img)
    }

    /// Photos: "Desktop picture" on photo `i`.
    fn use_as_wallpaper(&mut self, i: usize) {
        let bytes = match self.photos.source(i).cloned() {
            Some(Source::Memory(b)) => Ok(b.to_vec()),
            Some(Source::Usb { drive, vol, entry }) => self.usb.read_file(drive, vol, &entry),
            Some(Source::Library { entry, .. }) => self.storage.read_file(&mut self.usb, &entry),
            None => return,
        };
        let Ok(bytes) = bytes else {
            self.toast = Some((String::from("Couldn't read that photo"), Self::now() + 5 * HZ));
            return;
        };
        if crate::image::decode(&bytes).is_err() {
            self.toast = Some((String::from("That photo can't be used as a desktop picture"), Self::now() + 5 * HZ));
            return;
        }
        // Kept in Solon storage so it's there after a restart (only one at a time).
        if self.storage.library.is_some() {
            let name = if crate::image::sniff(&bytes) == Some(crate::image::Format::Png) { CUSTOM_NAMES[1] } else { CUSTOM_NAMES[0] };
            for other in CUSTOM_NAMES.iter().filter(|n| **n != name) {
                let _ = self.storage.remove_root_file(&mut self.usb, other);
            }
            if let Err(e) = self.storage.write_root_file(&mut self.usb, name, &bytes) {
                kprintln!("solon: couldn't save the desktop picture: {:?}", e);
            }
        }
        self.custom_wallpaper = Some(bytes);
        self.custom_checked = true;
        self.wallpaper = None;
        let mut s = self.settings;
        s.background = crate::wallpaper::CUSTOM;
        self.apply_settings(s);
        self.toast = Some((String::from("Desktop picture changed - you'll see it when you close Photos"), Self::now() + 6 * HZ));
    }

    fn backup_action(&mut self, a: crate::backup::BackupAction, now: u64) -> bool {
        use crate::backup::BackupAction;
        match a {
            BackupAction::None => false,
            BackupAction::Redraw => true,
            BackupAction::Finished { storage_changed, calendar_changed, message } => {
                self.toast = Some((message, now + 8 * HZ));
                if storage_changed {
                    self.library_changed();
                    self.files.refresh(&mut self.storage, &mut self.usb);
                    // A restore merges the backed-up spelling words in.
                    if let Some(t) = self.storage.read_root_file(&mut self.usb, "Dictionary.txt") {
                        crate::spell::load_user(core::str::from_utf8(&t).unwrap_or(""));
                    }
                }
                if calendar_changed {
                    let t = self.storage.read_root_file(&mut self.usb, "Calendar.txt");
                    self.calendar.load(t.as_deref().and_then(|b| core::str::from_utf8(b).ok()));
                }
                true
            }
        }
    }

    /// The backup reminder on the desktop, under the clock: (text, where).
    fn backup_nudge(&self) -> Option<(String, Rect)> {
        if self.storage.library.is_none() || !self.backup.last_loaded || self.backup.busy() {
            return None;
        }
        // Don't nag about an empty Solon.
        if !self.backup.has_data {
            return None;
        }
        let msg = crate::backup::reminder(self.backup.last.as_ref(), self.date)?;
        let (w, h) = (self.w, self.h);
        let big = (h / 9).clamp(56, 120);
        let top = h / 5 + big;
        let dpx = (big / 4).max(18);
        let (y, m, d) = self.date;
        let n = crate::calendar::today_lines(&self.calendar.events, y, m, d, &self.settings).len().min(4) as i32;
        let lw = text_width(&msg, Size::Small, true) + 54;
        Some((msg, Rect::new(w / 2 - lw / 2, top + dpx * 2 + 24 + n * 40 + 8, lw, 34)))
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

    /// The Calendar's events changed: keep them in Solon storage.
    fn cal_action(&mut self, a: crate::calendar::CalAction) -> bool {
        use crate::calendar::CalAction;
        match a {
            CalAction::None => false,
            CalAction::Redraw => true,
            CalAction::Save(text) => {
                if self.storage.library.is_some() {
                    if let Err(e) = self.storage.write_root_file(&mut self.usb, "Calendar.txt", text.as_bytes()) {
                        kprintln!("calendar: couldn't save: {:?}", e);
                        self.toast = Some((String::from("Couldn't save the calendar"), Self::now() + 5 * HZ));
                    }
                } else {
                    self.toast = Some((String::from("Events are kept until the computer is turned off (no Solon storage)"), Self::now() + 6 * HZ));
                }
                true
            }
        }
    }

    /// Every printer for the Printers app, usable or not.
    fn printer_views(&self) -> Vec<crate::printers::PrinterView> {
        let mut v: Vec<crate::printers::PrinterView> = self
            .usb
            .printers
            .iter()
            .map(|p| crate::printers::PrinterView {
                id: PrinterId::Usb(p.id),
                name: p.name.clone(),
                make: p.make.clone(),
                lang: p.lang,
                problem: p.problem,
                color: p.color(),
                papers: p.papers(),
                status: p.info.as_ref().filter(|_| p.ipp).map(|i| crate::printers::status_text(i.state, &i.state_reasons)),
            })
            .collect();
        if let Some(t) = &self.test_printer {
            v.push(crate::printers::PrinterView { id: t.id, name: t.name.clone(), make: String::new(), lang: Some(t.lang), problem: None, color: t.color, papers: t.papers.clone(), status: None });
        }
        v
    }

    fn print_test_page(&mut self, id: PrinterId, now: u64) -> bool {
        let Some(info) = self.printers().into_iter().find(|p| p.id == id) else { return false };
        let paper = if info.papers.contains(&print::Paper::Letter) { print::Paper::Letter } else { info.papers.first().copied().unwrap_or(print::Paper::A4) };
        let (w, h) = print::page_pixels(paper, 100);
        let date = settings::long_date_text(self.date.0, self.date.1, self.date.2);
        let page = crate::printers::test_page(w, h, &info.name, info.lang, &date);
        let layout = Layout { paper, per_page: 1, fill: false, color: info.color, copies: 1 };
        self.toast = Some((format!("Printing a test page on {}...", info.name), now + 60 * HZ));
        self.pending_sheet_print = Some((alloc::vec![page], layout, info));
        true
    }

    fn doc_index(&self, app: App) -> Option<usize> {        self.doc_apps.iter().position(|(a, _)| *a == app)
    }

    /// What Writer, PDF or the Text Editor asked for.
    fn doc_app_action(&mut self, k: usize, a: DocsAction, now: u64) -> bool {
        match a {
            DocsAction::None => false,
            DocsAction::Redraw => true,
            DocsAction::Toast(msg) => {
                self.toast = Some((msg, now + 5 * HZ));
                if !self.doc_apps[k].1.editing() {
                    self.doc_apps[k].1.refresh(&mut self.storage, &mut self.usb);
                }
                self.refresh_storage_view();
                true
            }
            DocsAction::WantPrinters => {
                let printers = self.printers();
                kprintln!("print: {} dialog opened, {} printer(s)", self.doc_apps[k].0.name(), printers.len());
                self.doc_apps[k].1.start_print(printers);
                true
            }
            DocsAction::Print { pages, layout, printer } => {
                self.toast = Some((format!("Preparing {} page(s) for {}...", pages.len(), printer.name), now + 60 * HZ));
                self.pending_sheet_print = Some((pages, layout, printer));
                true
            }
        }
    }

    /// Open a document from the Files app, in whichever app handles it.
    fn open_document(&mut self, src: DocSrc, now: u64) -> bool {
        let name = match &src {
            DocSrc::Library { entry, .. } | DocSrc::Usb { entry, .. } => entry.name.clone(),
        };
        if let Some(k) = self.doc_apps.iter().position(|(_, d)| d.opens(&name)) {
            let app = self.doc_apps[k].0;
            self.open(app);
            let a = self.doc_apps[k].1.open_src(src, &mut self.storage, &mut self.usb);
            return self.doc_app_action(k, a, now);
        }
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

    // ---- screen resolution ---------------------------------------------------------------

    /// Write a new resolution into the boot settings as a trial (restart follows).
    fn try_resolution(&mut self, m: Option<(u32, u32)>) -> Result<(), String> {
        let text = crate::display::read_config(&mut self.storage, &mut self.usb)?;
        let previous = crate::display::configured(&text);
        let new = crate::display::with_resolution(&text, m, Some(previous));
        crate::display::write_config(&mut self.storage, &mut self.usb, &new)?;
        kprintln!("display: trying {:?} (was {:?}), restarting", m, previous);
        Ok(())
    }

    /// Keep the resolution being tried, or go back to the old one (and restart).
    fn end_display_trial(&mut self, keep: bool) {
        let Some((previous, _)) = self.display_trial.take() else { return };
        let r = crate::display::read_config(&mut self.storage, &mut self.usb).and_then(|text| {
            let res = if keep { crate::display::configured(&text) } else { previous };
            crate::display::write_config(&mut self.storage, &mut self.usb, &crate::display::with_resolution(&text, res, None))
        });
        match r {
            Ok(()) if keep => self.toast = Some((String::from("Resolution kept"), Self::now() + 3 * HZ)),
            Ok(()) => {
                kprintln!("display: going back to {:?}", previous);
                self.request = Some(PowerRequest::Restart);
            }
            Err(e) => self.toast = Some((format!("Couldn't save the resolution: {}", e), Self::now() + 8 * HZ)),
        }
    }

    fn trial_buttons(&self) -> (Rect, Rect) {
        let (w, h) = (460, 190);
        let (x, y) = ((self.w - w) / 2, (self.h - BAR_H - h) / 2);
        (Rect::new(x + w - 300, y + h - 58, 140, 40), Rect::new(x + w - 150, y + h - 58, 130, 40))
    }

    fn draw_display_trial(&self, c: &mut Canvas) {
        let Some((_, end)) = self.display_trial else { return };
        let (w, h) = (460, 190);
        let (x, y) = ((self.w - w) / 2, (self.h - BAR_H - h) / 2);
        c.fill_rect_alpha(0, 0, self.w, self.h, 0, 120);
        c.round_rect(x, y, w, h, 14, rgb(250, 250, 252));
        c.text(x + 24, y + 22, "Keep this resolution?", Size::Medium, true, rgb(40, 44, 52));
        let left = end.saturating_sub(Self::now()).div_ceil(HZ);
        c.text(x + 24, y + 62, &format!("{} x {}. Going back in {} seconds.", self.screen.0, self.screen.1, left), Size::Small, false, rgb(90, 96, 106));
        let (keep, back) = self.trial_buttons();
        c.round_rect(keep.x, keep.y, keep.w, keep.h, 8, rgb(46, 134, 222));
        c.text_centered(keep.x + keep.w / 2, keep.y + 12, "Keep it", Size::Small, true, rgb(255, 255, 255));
        c.round_rect(back.x, back.y, back.w, back.h, 8, rgb(226, 230, 236));
        c.text_centered(back.x + back.w / 2, back.y + 12, "Go back", Size::Small, true, rgb(40, 44, 52));
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

    // ---- the desktop's right-click menu ------------------------------------------------

    /// Its items: label, and the Settings page it opens (with the background chooser).
    const DESK_MENU: [(&'static str, Page, bool); 7] = [
        ("Change background", Page::Appearance, true),
        ("Display and resolution", Page::Display, false),
        ("Screensaver", Page::Display, false),
        ("Date and time", Page::DateTime, false),
        ("Text and icon size", Page::Appearance, false),
        ("Spelling", Page::Spelling, false),
        ("All settings", Page::Appearance, false),
    ];

    fn desk_menu_box(&self) -> Option<Rect> {
        let (x, y, _) = self.desk_menu?;
        let (w, h) = (250, Self::DESK_MENU.len() as i32 * 38 + 12 + 9);
        let work = self.work();
        Some(Rect::new(x.min(work.w - w - 4).max(4), y.min(work.h - h - 4).max(4), w, h))
    }

    fn desk_menu_item(&self, i: usize) -> Rect {
        let b = self.desk_menu_box().unwrap_or(Rect::new(0, 0, 0, 0));
        // A gap before "All settings".
        let gap = if i + 1 == Self::DESK_MENU.len() { 9 } else { 0 };
        Rect::new(b.x + 6, b.y + 6 + i as i32 * 38 + gap, b.w - 12, 38)
    }

    fn draw_desk_menu(&self, c: &mut Canvas) {
        let Some(b) = self.desk_menu_box() else { return };
        let hover = self.desk_menu.and_then(|m| m.2);
        c.shadow(b.x, b.y + 4, b.w, b.h, 12, 18, 80);
        c.round_rect(b.x, b.y, b.w, b.h, 12, rgb(252, 252, 253));
        c.outline_rect(b.x + 8, b.y, b.w - 16, 1, rgb(226, 229, 234));
        for (i, (label, _, _)) in Self::DESK_MENU.iter().enumerate() {
            let r = self.desk_menu_item(i);
            if i + 1 == Self::DESK_MENU.len() {
                c.fill_rect(b.x + 12, r.y - 5, b.w - 24, 1, rgb(226, 229, 234));
            }
            if hover == Some(i) {
                c.round_rect(r.x, r.y, r.w, r.h, 8, rgb(46, 134, 222));
            }
            let col = if hover == Some(i) { rgb(255, 255, 255) } else { rgb(32, 36, 44) };
            c.text(r.x + 14, r.y + 10, label, Size::Small, i + 1 == Self::DESK_MENU.len(), col);
        }
    }

    // ---- "Continue working on your last document?" (a phone's newest document) ----------

    fn handoff_box(&self) -> Rect {
        let (w, h) = (440, 172);
        let work = self.work();
        Rect::new(work.w - w - 20, work.h - h - 16, w, h)
    }
    /// The "Not now" and "Open" buttons.
    fn handoff_buttons(&self) -> (Rect, Rect) {
        let b = self.handoff_box();
        (Rect::new(b.x + b.w - 300, b.y + b.h - 54, 120, 38), Rect::new(b.x + b.w - 170, b.y + b.h - 54, 150, 38))
    }

    fn draw_handoff(&self, c: &mut Canvas) {
        let Some(h) = &self.handoff else { return };
        let b = self.handoff_box();
        c.shadow(b.x, b.y + 4, b.w, b.h, 14, 20, 90);
        c.round_rect(b.x, b.y, b.w, b.h, 14, rgb(252, 252, 253));
        // A page with a phone corner.
        let (ix, iy) = (b.x + 20, b.y + 20);
        c.round_rect(ix, iy, 34, 42, 5, rgb(46, 134, 222));
        for k in 0..3 {
            c.fill_rect(ix + 7, iy + 12 + k * 8, if k == 2 { 12 } else { 20 }, 3, rgb(255, 255, 255));
        }
        c.text(b.x + 70, b.y + 16, "Continue working on your last document?", Size::Small, true, rgb(32, 36, 44));
        let name = crate::photos::ellipsize(&h.entry.name, b.w - 90, Size::Small, false);
        c.text(b.x + 70, b.y + 40, &name, Size::Small, false, rgb(32, 36, 44));
        let m = h.entry.mtime;
        let (y, mo, d, hh, mi) = (((m >> 25) + 1980) as u16, ((m >> 21) & 15) as u8, ((m >> 16) & 31) as u8, ((m >> 11) & 31) as u8, ((m >> 5) & 63) as u8);
        let from = crate::photos::ellipsize(&format!("From {}", h.phone_name), b.w - 90, Size::Small, false);
        c.text(b.x + 70, b.y + 62, &from, Size::Small, false, rgb(106, 112, 124));
        let when = format!("Changed {} at {}", settings::short_date_text(y, mo, d), self.settings.clock_text(hh, mi));
        c.text(b.x + 70, b.y + 84, &when, Size::Small, false, rgb(106, 112, 124));
        let (later, open) = self.handoff_buttons();
        c.round_rect(later.x, later.y, later.w, later.h, 8, rgb(232, 235, 240));
        c.text_centered(later.x + later.w / 2, later.y + 10, "Not now", Size::Small, true, rgb(32, 36, 44));
        c.round_rect(open.x, open.y, open.w, open.h, 8, rgb(46, 134, 222));
        c.text_centered(open.x + open.w / 2, open.y + 10, "Open", Size::Small, true, rgb(255, 255, 255));
    }

    /// A click on the card: Some(redraw) if it was on it.
    fn handoff_click(&mut self, x: i32, y: i32, now: u64) -> Option<bool> {
        self.handoff.as_ref()?;
        if !self.handoff_box().contains(x, y) {
            return None;
        }
        let (later, open) = self.handoff_buttons();
        if later.contains(x, y) {
            self.handoff = None;
            return Some(true);
        }
        if open.contains(x, y) {
            let h = self.handoff.take()?;
            kprintln!("handoff: opening \"{}\" from {}", h.entry.name, h.phone_name);
            let src = DocSrc::Usb { drive: h.phone, vol: h.vol, entry: h.entry };
            return Some(self.open_document(src, now) | true);
        }
        Some(false)
    }

    /// Open Settings at a page (from the desktop's menu).
    fn open_settings(&mut self, page: Page, bg_picker: bool) {
        self.open(App::Settings);
        if self.minimized(App::Settings) {
            self.raise(App::Settings);
        }
        self.settings_app.show(page, bg_picker);
    }

    fn minimized(&self, app: App) -> bool {
        self.windows.iter().any(|w| w.app == app && w.minimized)
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
        let (w, h) = if app == App::Calculator {
            (720.min(work.w), 600.min(work.h))
        } else {
            ((work.w * 82 / 100).clamp(560.min(work.w), work.w), (work.h * 86 / 100).clamp(360.min(work.h), work.h))
        };
        let x = ((work.w - w) / 2 + (n % 6) * 28 - 70).clamp(0, (work.w - w).max(0));
        let y = ((work.h - h) / 2 + (n % 6) * 28 - 50).clamp(0, (work.h - h).max(0));
        self.windows.push(Window { app, x, y, w, h, minimized: false, restore: None });
        self.order.push(app);
        // The editors want all the room they can get.
        if matches!(app, App::Photos | App::Documents | App::Movie | App::Videos | App::Pinball | App::Shooter | App::Writer | App::Pdf | App::TextEdit | App::Calendar | App::Paint) {
            self.set_maximized(app, true);
        }
        kprintln!("solon: open {}", app.name());
        if app == App::Calendar {
            if !self.calendar.loaded() {
                let t = self.storage.read_root_file(&mut self.usb, "Calendar.txt");
                self.calendar.load(t.as_deref().and_then(|b| core::str::from_utf8(b).ok()));
            }
            self.calendar.opened(self.date);
        }
        if app == App::Settings {
            self.settings_app.reload_time();
        }
        if app == App::Files {
            self.files.refresh(&mut self.storage, &mut self.usb);
        }
        if app == App::Disks {
            self.disks.refresh(&mut self.storage, &mut self.usb);
        }
        if app == App::PhotoFrame {
            self.frame_app.photos = self.storage.all_photos(&mut self.usb).len();
        }
        if app == App::Backup && !self.backup.busy() {
            self.backup.refresh(&mut self.storage, &mut self.usb);
        }
        if let Some(k) = self.doc_index(app) {
            if !self.doc_apps[k].1.editing() {
                self.doc_apps[k].1.refresh(&mut self.storage, &mut self.usb);
            }
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
        if app.game().is_some() && !self.games.loaded {
            let t = self.storage.read_root_file(&mut self.usb, "Games.txt");
            self.games.load(t.as_deref().and_then(|b| core::str::from_utf8(b).ok()));
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
        if app == App::Backup && self.backup.busy() {
            self.toast = Some((String::from("Still working - press Stop first if you want to close Backup"), Self::now() + 4 * HZ));
            return;
        }
        if let Some(g) = app.game() {
            self.games.closed(g);
        }
        self.windows.retain(|w| w.app != app);
        self.order.retain(|a| *a != app);
        if matches!(self.drag, Some((a, ..)) if a == app) {
            self.drag = None;
        }
        self.capture = false;
        kprintln!("solon: close {}", app.name());
    }

    // ---- dock and Applications menu -------------------------------------------------------

    /// What the dock shows: the Applications button, the pinned apps and folders, then any
    /// other open apps.
    fn dock_items(&self) -> Vec<DockItem> {
        let mut v = alloc::vec![DockItem::Apps];
        v.extend(PINNED.iter().map(|&e| DockItem::Entry(e)));
        for &a in &self.order {
            if !PINNED.contains(&MenuEntry::App(a)) {
                v.push(DockItem::Entry(MenuEntry::App(a)));
            }
        }
        v
    }

    fn clock_strings(&self) -> (String, String) {
        let clock = self.settings.clock_text(self.clock.0, self.clock.1);
        let date = if self.settings.show_date { settings::short_date_text(self.date.0, self.date.1, self.date.2) } else { String::new() };
        (clock, date)
    }

    fn dock_clock_w(&self) -> i32 {
        let (clock, date) = self.clock_strings();
        text_width(&clock, Size::Medium, true).max(text_width(&date, Size::Small, false)) + 8
    }

    /// The dock and where everything in it is. The power button is last.
    fn dock_layout(&self) -> (Rect, Vec<(DockItem, Rect)>) {
        let items = self.dock_items();
        let n = items.len() as i32;
        let clock_w = self.dock_clock_w();
        let fixed = 12 + 17 + clock_w + 10 + 40 + 10;
        let step = (DOCK_ICON + DOCK_GAP).min((self.w - 24 - fixed) / n.max(1)).max(24);
        let width = 12 + n * step - DOCK_GAP + fixed;
        let dock = Rect::new((self.w - width) / 2, self.h - DOCK_H - (BAR_H - DOCK_H) / 2, width, DOCK_H);
        let iy = dock.y + (DOCK_H - DOCK_ICON) / 2 - 1;
        let mut v: Vec<(DockItem, Rect)> = items.iter().enumerate().map(|(i, &it)| (it, Rect::new(dock.x + 12 + i as i32 * step, iy, DOCK_ICON, DOCK_ICON))).collect();
        v.push((DockItem::Power, Rect::new(dock.x + dock.w - 10 - 40, dock.y + (DOCK_H - 40) / 2, 40, 40)));
        (dock, v)
    }

    fn apps_button(&self) -> Rect {
        self.dock_layout().1[0].1
    }

    fn power_button(&self) -> Rect {
        self.dock_layout().1.last().unwrap().1
    }

    fn power_item(&self, i: i32) -> Rect {
        let p = self.power_button();
        let (dock, _) = self.dock_layout();
        Rect::new((p.x + p.w - 220).min(self.w - 236), dock.y - 12 - 3 * 46 + i * 46, 220, 42)
    }

    /// Columns in the Applications menu.
    fn menu_cols(&self) -> usize {
        if self.w >= 5 * 124 + 48 { 5 } else { 4 }
    }

    /// What the Applications menu shows: the apps and folders, or a folder's apps.
    fn menu_entries(&self) -> Vec<MenuEntry> {
        match self.folder {
            Some(f) => core::iter::once(MenuEntry::Back).chain(f.apps().iter().map(|&a| MenuEntry::App(a))).collect(),
            None => APPS.to_vec(),
        }
    }

    fn activate(&mut self, e: MenuEntry) {
        match e {
            MenuEntry::App(a) => {
                self.folder = None;
                self.open(a);
            }
            MenuEntry::Folder(f) => {
                self.folder = Some(f);
                self.apps_menu = Some(0);
            }
            MenuEntry::Back => {
                self.folder = None;
                self.apps_menu = Some(0);
            }
        }
    }

    fn menu_box(&self) -> Rect {
        let cols = self.menu_cols() as i32;
        let rows = self.menu_entries().len().div_ceil(cols as usize) as i32;
        let (w, h) = (cols * 124 + 40, 66 + rows * 112 + 12);
        let (dock, _) = self.dock_layout();
        Rect::new((self.w - w) / 2, dock.y - 14 - h, w, h)
    }

    fn menu_item(&self, i: usize) -> Rect {
        let b = self.menu_box();
        let cols = self.menu_cols();
        Rect::new(b.x + 20 + (i % cols) as i32 * 124, b.y + 60 + (i / cols) as i32 * 112, 120, 108)
    }

    fn icon_img(list: &[(icons::Icon, crate::image::Image)], icon: icons::Icon) -> Option<&crate::image::Image> {
        list.iter().find(|(i, _)| *i == icon).map(|(_, img)| img)
    }

    fn make_icons(&mut self) {
        let mut all: Vec<icons::Icon> = APPS.iter().map(|e| e.icon()).collect();
        for f in [Folder::Games, Folder::Documents] {
            all.extend(f.apps().iter().map(|&a| icons::Icon::App(a)));
        }
        all.push(icons::Icon::Back);
        self.dock_icons = all.iter().map(|&i| (i, icons::image(i, DOCK_ICON as u32))).collect();
        self.title_icons = all.iter().map(|&i| (i, icons::image(i, 22))).collect();
    }

    fn draw_dock(&self, c: &mut Canvas) {
        let (dock, items) = self.dock_layout();
        c.shadow(dock.x, dock.y + 4, dock.w, dock.h, 20, 16, 70);
        c.round_rect(dock.x, dock.y, dock.w, dock.h, 20, rgb(249, 250, 253));
        let focus = self.focused_app();
        for &(item, r) in &items {
            match item {
                DockItem::Apps => {
                    let open = self.apps_menu.is_some();
                    c.round_rect(r.x, r.y, r.w, r.h, 12, if open { rgb(30, 100, 210) } else { rgb(46, 124, 232) });
                    for row in 0..3 {
                        for k in 0..3 {
                            c.round_rect(r.x + 10 + k * 8, r.y + 10 + row * 8, 6, 6, 2, rgb(255, 255, 255));
                        }
                    }
                }
                DockItem::Entry(e) => {
                    if let Some(img) = Self::icon_img(&self.dock_icons, e.icon()) {
                        c.draw_image(img, r.x, r.y);
                    }
                    if let MenuEntry::App(app) = e {
                        if self.is_open(app) {
                            let focused = focus == Some(app);
                            let w = if focused { 16 } else { 6 };
                            c.round_rect(r.x + r.w / 2 - w / 2, dock.y + dock.h - 6, w, 4, 2, if focused { rgb(46, 124, 232) } else { INK_DIM });
                        }
                    }
                }
                DockItem::Power => {
                    let bg = if self.power_menu { rgb(214, 220, 230) } else { rgb(234, 237, 242) };
                    c.round_rect(r.x, r.y, r.w, r.h, 12, bg);
                    let (cx, cy) = (r.x + r.w / 2, r.y + r.h / 2 + 1);
                    c.circle(cx, cy, 9, INK);
                    c.circle(cx, cy, 7, bg);
                    c.fill_rect(cx - 3, cy - 11, 6, 7, bg);
                    c.round_rect(cx - 1, cy - 12, 3, 10, 1, INK);
                }
            }
        }
        // Separator, then the clock.
        let clock_w = self.dock_clock_w();
        let px = items.last().unwrap().1.x;
        let cx = px - 10 - clock_w;
        c.fill_rect_alpha(cx - 9, dock.y + 12, 1, dock.h - 24, 0, 40);
        let (clock, date) = self.clock_strings();
        if date.is_empty() {
            c.text_centered(cx + clock_w / 2, dock.y + (dock.h - 20) / 2, &clock, Size::Medium, true, INK);
        } else {
            c.text_centered(cx + clock_w / 2, dock.y + 7, &clock, Size::Medium, true, INK);
            c.text_centered(cx + clock_w / 2, dock.y + 30, &date, Size::Small, false, INK_DIM);
        }
    }

    fn draw_apps_menu(&self, c: &mut Canvas) {
        let Some(sel) = self.apps_menu else { return };
        let b = self.menu_box();
        c.shadow(b.x, b.y + 6, b.w, b.h, 18, 24, 90);
        c.round_rect(b.x, b.y, b.w, b.h, 18, rgb(249, 250, 253));
        let title = self.folder.map(|f| f.name()).unwrap_or("Applications");
        c.text(b.x + 24, b.y + 20, title, Size::Medium, true, INK);
        for (i, &e) in self.menu_entries().iter().enumerate() {
            let r = self.menu_item(i);
            if i == sel {
                c.round_rect(r.x, r.y, r.w, r.h, 14, rgb(226, 235, 250));
            }
            icons::draw_icon(c, e.icon(), r.x + (r.w - 64) / 2, r.y + 8);
            let name = crate::photos::ellipsize(e.name(), r.w - 8, Size::Small, false);
            c.text_centered(r.x + r.w / 2, r.y + 80, &name, Size::Small, false, INK);
        }
    }

    // ---- drawing ------------------------------------------------------------------------

    pub fn draw(&mut self, c: &mut Canvas) {
        let (w, h) = (self.w, self.h);

        if self.screen_off {
            c.fill_rect(0, 0, w, h, 0);
            return;
        }
        if let Some(ss) = &mut self.slideshow {
            ss.time = self.clock;
            ss.date = self.date;
            ss.draw(c, &self.settings);
            return;
        }
        if self.videos.fullscreen() {
            self.videos.draw(c, Rect::new(0, 0, w, h));
            return;
        }

        if self.dock_icons.is_empty() {
            self.make_icons();
        }
        // Wallpaper (drawn once), with a big clock.
        let bg = self.settings.background;
        if !matches!(&self.wallpaper, Some(wp) if wp.bg == bg && wp.w == w && wp.h == h) {
            self.wallpaper = Some(self.make_wallpaper(bg, w, h));
        }
        let (ink, shadow, hint_ink) = self.wallpaper.as_ref().map_or((INK, false, INK), |wp| (wp.ink.0, wp.ink.1, wp.hint.0));
        if let Some(Wall { px, .. }) = &self.wallpaper {
            if c.w == w as usize && c.px.len() == px.len() {
                c.px.copy_from_slice(px);
            }
        }
        let maximized = self.windows.iter().any(|w| !w.minimized && w.restore.is_some());
        if !maximized {
            use crate::font::{width, Face};
            let clock = self.settings.clock_text(self.clock.0, self.clock.1);
            let big = (h / 9).clamp(56, 120) as u16;
            let top = h / 5 + big as i32;
            c.text_face((w / 2 - width(&clock, Face::Sans, big) / 2) * 64, top, &clock, Face::Sans, big, ink);
            let date = settings::long_date_text(self.date.0, self.date.1, self.date.2);
            let dpx = (big / 4).max(18);
            c.text_face((w / 2 - width(&date, Face::Sans, dpx) / 2) * 64, top + dpx as i32 * 2, &date, Face::Sans, dpx, ink);
            // Today's holiday and events, from the Calendar.
            let (y, m, d) = self.date;
            let lines = crate::calendar::today_lines(&self.calendar.events, y, m, d, &self.settings);
            let mut ly = top + dpx as i32 * 2 + 24;
            for l in lines.iter().take(4) {
                let l = crate::photos::ellipsize(l, w - 80, Size::Medium, false);
                let lw = text_width(&l, Size::Medium, false) + 36;
                c.round_rect_alpha(w / 2 - lw / 2, ly, lw, 32, 16, 0xFF_FFFF, 150);
                c.text_centered(w / 2, ly + 6, &l, Size::Medium, false, if shadow { rgb(30, 34, 42) } else { ink });
                ly += 40;
            }
            if let Some((msg, b)) = self.backup_nudge() {
                c.round_rect_alpha(b.x, b.y, b.w, b.h, 17, 0xFF_FFFF, 225);
                c.circle(b.x + 20, b.y + 17, 5, rgb(214, 120, 20));
                c.text(b.x + 34, b.y + 7, &msg, Size::Small, true, rgb(40, 44, 52));
            }
            if self.windows.is_empty() {
                let hint = "Click the blue button at the bottom to see all your apps";
                if hint_ink == rgb(255, 255, 255) {
                    c.text_centered(w / 2 + 1, h - BAR_H - 47, hint, Size::Small, false, rgb(0, 0, 0));
                }
                c.text_centered(w / 2, h - BAR_H - 48, hint, Size::Small, false, hint_ink);
            }
        }

        // Windows, bottom to top.
        let focus = self.focused();
        for i in 0..self.windows.len() {
            let win = &self.windows[i];
            if win.minimized {
                continue;
            }
            let title = if win.app == App::Documents { self.docs.title() } else { self.doc_index(win.app).and_then(|k| self.doc_apps[k].1.title()) };
            let icon = Self::icon_img(&self.title_icons, icons::Icon::App(win.app));
            draw_frame(c, win, title.as_deref().unwrap_or(win.app.name()), focus == Some(i), icon);
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
                app if self.doc_index(app).is_some() => {
                    let k = self.doc_index(app).unwrap();
                    self.doc_apps[k].1.draw(c, body)
                }
                App::Movie => self.movie.draw(c, body),
                App::Videos => self.videos.draw(c, body),
                App::Calculator => self.calculator.draw(c, body),
                App::Printers => {
                    let v = self.printer_views();
                    self.printers_app.draw(c, body, &v)
                }
                App::Calendar => {
                    self.calendar.set_today(self.date);
                    self.calendar.draw(c, body, &self.settings)
                }
                App::Backup => self.backup.draw(c, body, &self.settings, self.date),
                App::PhotoFrame => self.frame_app.draw(c, body, &self.settings),
                app if app.game().is_some() => self.games.draw(app.game().unwrap(), c, body),
                app => draw_placeholder(c, app, body),
            }
            if let Some((area, max, scroll)) = self.scroll_info(app, body) {
                crate::scrollbar::draw(c, area, max, scroll);
            }
            c.reset_clip();
        }

        self.draw_dock(c);
        if let Some((msg, _)) = &self.toast {
            let tw = text_width(msg, Size::Small, true) + 40;
            let y = h - BAR_H - 46;
            c.shadow(w / 2 - tw / 2, y + 3, tw, 34, 17, 12, 70);
            c.round_rect(w / 2 - tw / 2, y, tw, 34, 17, rgb(36, 40, 50));
            c.text_centered(w / 2, y + 9, msg, Size::Small, true, rgb(255, 255, 255));
        }
        self.draw_handoff(c);
        self.draw_apps_menu(c);
        self.draw_desk_menu(c);
        if self.install_notice {
            self.draw_install_notice(c);
        }
        self.draw_display_trial(c);
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
            app if self.doc_index(app).is_some() => {
                let k = self.doc_index(app).unwrap();
                self.doc_apps[k].1.on_wheel(wheel, body)
            }
            App::Movie => self.movie.on_wheel(wheel, x, y, body),
            App::Videos => self.videos.on_wheel(wheel, body),
            App::Calendar => self.calendar.on_wheel(wheel),
            App::Settings => self.settings_app.on_wheel(wheel, body, &self.settings, self.screen),
            _ => false,
        }
    }

    /// Where an app's scrolling list is, how far it scrolls and where it is now (for the
    /// scroll bar the desktop draws). None = no list to scroll right now.
    fn scroll_info(&self, app: App, body: Rect) -> Option<(Rect, i32, i32)> {
        match app {
            App::Photos => self.photos.scroll_info(body),
            App::Import => self.import.scroll_info(body, &self.usb),
            App::Files => self.files.scroll_info(body),
            App::Disks => self.disks.scroll_info(body),
            App::Documents => self.docs.scroll_info(body),
            App::Videos => self.videos.scroll_info(body),
            App::Settings => self.settings_app.scroll_info(body, &self.settings, self.screen),
            app => self.doc_index(app).and_then(|k| self.doc_apps[k].1.scroll_info(body)),
        }
    }

    fn set_scroll(&mut self, app: App, body: Rect, v: i32) {
        match app {
            App::Photos => self.photos.set_scroll(v),
            App::Import => self.import.set_scroll(v),
            App::Files => self.files.set_scroll(v),
            App::Disks => self.disks.set_scroll(v),
            App::Documents => self.docs.set_scroll(v),
            App::Videos => self.videos.set_scroll(v),
            App::Settings => self.settings_app.set_scroll(v),
            app => {
                if let Some(k) = self.doc_index(app) {
                    self.doc_apps[k].1.set_scroll(v, body);
                }
            }
        }
    }

    fn app_press(&mut self, app: App, x: i32, y: i32, body: Rect, now: u64) -> bool {
        let mut redraw = false;
        if let Some((area, max, scroll)) = self.scroll_info(app, body) {
            if max > 0 && crate::scrollbar::hit(area).contains(x, y) {
                let (v, grab) = crate::scrollbar::press(area, max, scroll, y);
                self.set_scroll(app, body, v);
                self.sb_drag = Some((app, grab));
                return true;
            }
        }
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
            app if self.doc_index(app).is_some() => {
                let k = self.doc_index(app).unwrap();
                let a = self.doc_apps[k].1.on_press(x, y, body, now, &mut self.storage, &mut self.usb);
                self.capture = true;
                redraw |= self.doc_app_action(k, a, now);
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
            App::Printers => {
                let v = self.printer_views();
                match self.printers_app.on_press(x, y, body, &v) {
                    crate::printers::PrintersAction::None => {}
                    crate::printers::PrintersAction::Redraw => redraw = true,
                    crate::printers::PrintersAction::TestPage(id) => redraw |= self.print_test_page(id, now),
                }
            }
            App::Calculator => {
                redraw |= self.calculator.on_press(x, y, body);
                self.capture = true;
            }
            App::Calendar => {
                let a = self.calendar.on_press(x, y, body, now);
                redraw |= self.cal_action(a);
            }
            App::PhotoFrame => match self.frame_app.on_press(x, y, body, &self.settings) {
                crate::frame::FrameAction::None => {}
                crate::frame::FrameAction::Change(n) => {
                    self.apply_settings(n);
                    redraw = true;
                }
                crate::frame::FrameAction::Start => {
                    self.start_slideshow(true);
                    return true;
                }
            },
            App::Backup => {
                let a = self.backup.on_press(x, y, body, &mut self.storage, &mut self.usb);
                redraw |= self.backup_action(a, now);
            }
            app if app.game().is_some() => {
                redraw |= self.games.press(app.game().unwrap(), x, y, body, now, false);
                self.capture = true;
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
                    SettingsAction::Screensaver => {
                        self.open(App::PhotoFrame);
                        redraw = true;
                    }
                    SettingsAction::Resolution(Some(m)) if crate::display::change_for(m) != crate::display::Change::Restart => {
                        // At once: the card switches, or the desktop is scaled to the screen.
                        let boot = crate::display::INFO.lock().as_ref().map(|i| i.boot).unwrap_or((0, 0));
                        let mut n = self.settings;
                        n.resolution = if m == boot { (0, 0) } else { m };
                        if !n.size_fits(n.size, m.0 as usize, m.1 as usize) {
                            n.size = 0;
                        }
                        kprintln!("display: {}x{} ({:?})", m.0, m.1, crate::display::change_for(m));
                        self.apply_settings(n);
                        self.toast = Some((format!("Resolution {} x {}", m.0, m.1), now + 3 * HZ));
                        redraw = true;
                    }
                    SettingsAction::Resolution(m) => {
                        redraw = true;
                        match self.try_resolution(m) {
                            Ok(()) => self.request = Some(PowerRequest::Restart),
                            Err(e) => {
                                kprintln!("display: {}", e);
                                self.toast = Some((format!("Couldn't change the resolution: {}", e), now + 8 * HZ));
                            }
                        }
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
            app if self.doc_index(app).is_some() => {
                let k = self.doc_index(app).unwrap();
                let a = self.doc_apps[k].1.on_key(sc, body, &mut self.storage, &mut self.usb)?;
                Some(self.doc_app_action(k, a, Self::now()))
            }
            App::Movie => self.movie.on_key(sc).map(|a| self.movie_action(a, Self::now())),
            App::Videos => self.videos.on_key(sc).map(|a| matches!(a, VideosAction::Redraw)),
            App::Calculator => self.calculator.on_key(sc),
            App::Printers => {
                let n = self.printer_views().len();
                self.printers_app.on_key(sc, n)
            }
            App::Calendar => self.calendar.on_key(sc).map(|a| self.cal_action(a)),
            App::Settings if self.settings_app.on_key(sc) => Some(true),
            App::Backup => self.backup.on_key(sc, &mut self.storage, &mut self.usb).map(|a| self.backup_action(a, Self::now())),
            app if app.game().is_some() => self.games.key(app.game().unwrap(), sc, Self::now()),
            _ => None,
        }
    }

    /// Returns true if the screen needs redrawing.
    pub fn on_mouse(&mut self, x: i32, y: i32, buttons: u8, prev: u8, wheel: i8, now: u64) -> bool {
        let pressed = buttons & 1 != 0 && prev & 1 == 0;
        let released = buttons & 1 == 0 && prev & 1 != 0;
        let mut redraw = false;

        let moved = (x, y) != self.pointer;
        self.pointer = (x, y);
        if moved || buttons != prev || wheel != 0 {
            if self.slideshow.is_some() {
                // Not straight after starting it (the click on Start).
                if Self::now() > self.last_input + HZ && (moved || pressed || wheel != 0) {
                    self.end_slideshow();
                    return true;
                }
                return false;
            }
            self.last_input = now;
        }
        if self.videos.fullscreen() && !self.screen_off {
            let full = Rect::new(0, 0, self.w, self.h);
            let released = buttons & 1 == 0 && prev & 1 != 0;
            let mut redraw = moved && self.videos.on_move();
            if wheel != 0 {
                redraw |= self.videos.on_wheel(wheel, full);
            }
            if pressed {
                let a = self.videos.on_press(x, y, full, now, &mut self.storage, &mut self.usb);
                self.capture = true;
                redraw |= matches!(a, VideosAction::Redraw);
            } else if released && self.capture {
                self.capture = false;
                redraw |= self.videos.on_release();
            } else if buttons & 1 != 0 && self.capture {
                redraw |= self.videos.on_drag(x, y, full);
            }
            // Leaving full screen (Back or Exit full) shows the desktop again.
            return redraw || !self.videos.fullscreen();
        }
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


        // Right button: games use it (flags in Mines), editors show spelling suggestions.
        let rpressed = buttons & 2 != 0 && prev & 2 == 0;
        if rpressed && self.apps_menu.is_none() && !self.power_menu && !self.install_notice {
            let on_window = self.windows.iter().any(|w| !w.minimized && w.rect().contains(x, y));
            if !on_window && self.work().contains(x, y) {
                self.desk_menu = Some((x, y, None));
                return true;
            }
            if self.desk_menu.take().is_some() {
                redraw = true;
            }
        }
        // The pointer over the desktop menu highlights an item.
        if moved && self.desk_menu.is_some() {
            let hover = (0..Self::DESK_MENU.len()).find(|&i| self.desk_menu_item(i).contains(x, y));
            if let Some(m) = &mut self.desk_menu {
                if m.2 != hover {
                    m.2 = hover;
                    redraw = true;
                }
            }
        }
        if rpressed && self.apps_menu.is_none() {
            if let Some(i) = self.windows.iter().rposition(|w| !w.minimized && w.rect().contains(x, y)) {
                let (app, body) = (self.windows[i].app, self.windows[i].body());
                if let Some(g) = app.game() {
                    if body.contains(x, y) {
                        self.raise(app);
                        return true | self.games.press(g, x, y, body, now, true);
                    }
                }
                if let Some(k) = self.doc_index(app) {
                    if body.contains(x, y) {
                        self.raise(app);
                        let a = self.doc_apps[k].1.on_right_press(x, y, body, &mut self.storage, &mut self.usb);
                        return true | self.doc_app_action(k, a, now);
                    }
                }
            }
        }
        // A press inside Photos/Documents keeps getting moves until release (editor drags).
        if let Some((app, grab)) = self.sb_drag {
            if released || buttons & 1 == 0 {
                self.sb_drag = None;
                return true;
            }
            if let Some(i) = self.index_of(app) {
                let body = self.windows[i].body();
                if let Some((area, max, _)) = self.scroll_info(app, body) {
                    let v = crate::scrollbar::drag_to(area, max, y, grab);
                    self.set_scroll(app, body, v);
                    return true;
                }
            }
            self.sb_drag = None;
            return redraw;
        }
        if self.capture {
            if let Some(i) = self.focused() {
                let (app, body) = (self.windows[i].app, self.windows[i].body());
                if released {
                    self.capture = false;
                    return redraw | match app {
                        App::Documents => self.docs.on_release(),
                        app if self.doc_index(app).is_some() => {
                            let k = self.doc_index(app).unwrap();
                            self.doc_apps[k].1.on_release()
                        }
                        App::Movie => self.movie.on_release(),
                        App::Videos => self.videos.on_release(),
                        App::Calculator => self.calculator.on_release(),
                        app if app.game().is_some() => self.games.release(app.game().unwrap(), body),
                        _ => self.photos.on_release(),
                    };
                }
                if buttons & 1 != 0 {
                    return redraw | match app {
                        App::Documents => self.docs.on_drag(x, y, body),
                        app if self.doc_index(app).is_some() => {
                            let k = self.doc_index(app).unwrap();
                            self.doc_apps[k].1.on_drag(x, y, body)
                        }
                        App::Movie => self.movie.on_drag(x, y, body),
                        App::Videos => self.videos.on_drag(x, y, body),
                        app if app.game().is_some() => self.games.drag(app.game().unwrap(), x, y),
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
        if self.display_trial.is_some() {
            let (keep, back) = self.trial_buttons();
            if keep.contains(x, y) {
                self.end_display_trial(true);
            } else if back.contains(x, y) {
                self.end_display_trial(false);
            }
            return true;
        }
        if self.install_notice {
            let (open, later) = (self.notice_open(), self.notice_later());
            if open.contains(x, y) || later.contains(x, y) {
                self.dismiss_install_notice(open.contains(x, y));
            }
            return true;
        }
        if self.desk_menu.is_some() {
            let picked = (0..Self::DESK_MENU.len()).find(|&i| self.desk_menu_item(i).contains(x, y));
            let inside = self.desk_menu_box().is_some_and(|b| b.contains(x, y));
            self.desk_menu = None;
            if let Some(i) = picked {
                let (_, page, bg) = Self::DESK_MENU[i];
                self.open_settings(page, bg);
            }
            if inside || picked.is_some() {
                return true;
            }
            redraw = true;
        }
        if let Some(r) = self.handoff_click(x, y, now) {
            return r;
        }
        if let Some(r) = self.power_click(x, y) {
            self.apps_menu = None;
            return r;
        }

        // Applications menu and dock.
        let shown_folder = if self.apps_menu.is_some() { self.folder } else { None };
        if self.apps_menu.is_some() {
            let entries = self.menu_entries();
            if let Some(i) = (0..entries.len()).find(|&i| self.menu_item(i).contains(x, y)) {
                self.activate(entries[i]);
                return true;
            }
            let on_menu = self.menu_box().contains(x, y);
            let on_button = self.apps_button().contains(x, y);
            self.apps_menu = None;
            self.folder = None;
            if on_menu || on_button {
                return true;
            }
            redraw = true;
        } else if self.apps_button().contains(x, y) {
            self.apps_menu = Some(0);
            self.folder = None;
            return true;
        }
        if y >= self.h - BAR_H {
            let (_, items) = self.dock_layout();
            match items.into_iter().find(|(_, b)| b.contains(x, y)).map(|(it, _)| it) {
                Some(DockItem::Entry(MenuEntry::App(app))) => {
                    if !self.is_open(app) {
                        self.open(app);
                    } else if self.focused_app() == Some(app) {
                        if let Some(i) = self.index_of(app) {
                            self.windows[i].minimized = true;
                            self.capture = false;
                        }
                    } else {
                        self.raise(app);
                    }
                }
                Some(DockItem::Entry(MenuEntry::Folder(f))) if shown_folder != Some(f) => {
                    self.folder = Some(f);
                    self.apps_menu = Some(0);
                }
                None => {
                    // The clock and date open the Calendar.
                    let (dock, items) = self.dock_layout();
                    let px = items.last().map(|i| i.1.x).unwrap_or(dock.x + dock.w);
                    let cx = px - 10 - self.dock_clock_w();
                    if x >= cx - 4 && x < px - 4 && dock.contains(x, y) {
                        self.open(App::Calendar);
                    }
                }
                _ => {}
            }
            return true;
        }

        // The backup reminder on the desktop.
        if !self.windows.iter().any(|w| !w.minimized && (w.restore.is_some() || w.rect().contains(x, y))) {
            if matches!(self.backup_nudge(), Some((_, b)) if b.contains(x, y)) {
                self.open(App::Backup);
                return true;
            }
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
        self.last_input = Self::now();
        if let Some(ss) = &mut self.slideshow {
            if sc & 0x80 != 0 {
                return false;
            }
            if ss.manual && sc == 0x4D {
                ss.skip(Self::now());
                return true;
            }
            self.end_slideshow();
            return true;
        }
        if self.videos.fullscreen() {
            return self.videos.on_key(sc).is_some_and(|a| matches!(a, VideosAction::Redraw));
        }
        if self.display_trial.is_some() && sc & 0x80 == 0 {
            match sc {
                0x1C => self.end_display_trial(true),
                0x01 => self.end_display_trial(false),
                _ => return false,
            }
            return true;
        }
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
                0x1F => self.save_diagnostics(),
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
        if self.desk_menu.is_some() && sc & 0x80 == 0 {
            self.desk_menu = None;
            return true;
        }
        // Applications menu: arrows move, Enter opens, Esc closes.
        if let Some(sel) = self.apps_menu {
            let entries = self.menu_entries();
            let n = entries.len();
            match sc {
                0x01 if self.folder.is_some() => self.activate(MenuEntry::Back),
                0x01 => self.apps_menu = None,
                0x1C => self.activate(entries[sel.min(n - 1)]),
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
            self.folder = None;
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

fn draw_frame(c: &mut Canvas, win: &Window, title: &str, focused: bool, icon: Option<&crate::image::Image>) {
    let (x, y, w, h) = (win.x, win.y, win.w, win.h);
    let round = if win.restore.is_some() { 0 } else { 12 };
    if round > 0 {
        c.shadow(x, y + 6, w, h, round, if focused { 26 } else { 16 }, if focused { 95 } else { 55 });
        // A hairline edge so light windows stand apart from light wallpaper.
        c.round_rect(x - 1, y - 1, w + 2, h + 2, round + 1, rgb(206, 211, 220));
    }
    c.round_rect(x, y, w, h, round, WIN_BG);
    let bar = if focused { rgb(240, 243, 248) } else { WIN_BG };
    c.round_rect(x, y, w, TITLE_H + 12, round, bar);
    c.fill_rect(x, y + TITLE_H, w, 12, WIN_BG);
    c.fill_rect(x, y + TITLE_H, w, 1, rgb(222, 226, 232));

    // App icon and title.
    if let Some(img) = icon {
        c.draw_image(img, x + 12, y + (TITLE_H - 22) / 2);
    }
    let title = crate::photos::ellipsize(title, w - 3 * TITLE_H - 60, Size::Small, true);
    c.text(x + 44, y + (TITLE_H - 16) / 2, &title, Size::Small, true, if focused { INK } else { rgb(130, 136, 146) });

    let ink = if focused { rgb(60, 66, 78) } else { rgb(140, 146, 156) };
    // Minimize: a line.
    let m = win.min_box();
    c.round_rect(m.x + m.w / 2 - 7, m.y + m.h / 2, 14, 2, 1, ink);
    // Maximize / restore: a rounded square (two when maximized).
    let b = win.max_box();
    let (bx, by) = (b.x + b.w / 2 - 6, b.y + b.h / 2 - 6);
    if win.restore.is_some() {
        c.round_rect(bx + 3, by - 2, 10, 10, 2, ink);
        c.round_rect(bx + 4, by - 1, 8, 8, 1, bar);
        c.round_rect(bx, by + 2, 10, 10, 2, ink);
        c.round_rect(bx + 1, by + 3, 8, 8, 1, bar);
    } else {
        c.round_rect(bx, by, 12, 12, 3, ink);
        c.round_rect(bx + 2, by + 2, 8, 8, 2, bar);
    }
    // Close: a red circle with a white cross.
    let b = win.close_box();
    let (cx, cy) = (b.x + b.w / 2, b.y + b.h / 2);
    c.circle(cx, cy, 10, if focused { rgb(232, 72, 72) } else { rgb(214, 150, 150) });
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
    use crate::printlang::{PclXlJob, PsJob, UrfJob};
    enum Job {
        Pcl(print::PclJob),
        Pwg(print::PwgJob),
        Urf(UrfJob),
        Xl(PclXlJob),
        Ps(PsJob),
    }
    let dpi = printer.dpi;
    let mut job = match printer.lang {
        print::Lang::Pwg => Job::Pwg(print::PwgJob::start()),
        print::Lang::Urf => Job::Urf(UrfJob::start()),
        print::Lang::Pcl => Job::Pcl(print::PclJob::start(layout)),
        print::Lang::PclXl => Job::Xl(PclXlJob::start(layout, dpi)),
        print::Lang::PostScript => Job::Ps(PsJob::start(layout)),
    };
    for n in 0..pages {
        let mut page = make(n);
        if !layout.color {
            for p in page.px.iter_mut() {
                let (r, g, b) = ((*p >> 16) & 0xFF, (*p >> 8) & 0xFF, *p & 0xFF);
                let y = (77 * r + 150 * g + 29 * b) >> 8;
                *p = y << 16 | y << 8 | y;
            }
        }
        match &mut job {
            Job::Pcl(j) => j.page(&page, layout.color, dpi),
            Job::Pwg(j) => j.page(&page, layout, dpi, pages as u32),
            Job::Urf(j) => j.page(&page, layout, dpi),
            Job::Xl(j) => j.page(&page, layout.color, dpi),
            Job::Ps(j) => j.page(&page, layout.color, dpi),
        }
        kprintln!("print: page {} of {} ready ({}x{} at {} dpi)", n + 1, pages, page.w, page.h, dpi);
    }
    Ok(match job {
        Job::Pcl(j) => j.finish(),
        Job::Pwg(j) => print::ipp_print_job(layout, &j.finish(), "image/pwg-raster"),
        Job::Urf(j) => print::ipp_print_job(layout, &j.finish(), "image/urf"),
        Job::Xl(j) => j.finish(),
        Job::Ps(j) => j.finish(),
    })
}

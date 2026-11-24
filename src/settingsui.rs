//! The Settings window: a sidebar of sections (Appearance, Date & time, Display, Mouse,
//! Spelling, This PC, About), each a page of cards with switches, segmented buttons and
//! drop-down lists. This PC shows the processor, motherboard, memory and sensors, live. The desktop can open it at a given page (its right-click menu does).
//!
//! Each page is laid out once per draw or click as a list of items (where, and what a click
//! does), so drawing and clicking always agree.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{mix, rgb, text_width, Canvas, Rect, Size};
use crate::settings::{days_in_month, weekday, Settings, BACKGROUNDS, MOUSE, SAVERS, SAVER_WAIT, SIZES, SLIDE_TIME};
use crate::wallpaper::{CUSTOM, FIRST_PHOTO, PHOTOS};

const INK: u32 = rgb(32, 36, 44);
const DIM: u32 = rgb(108, 114, 126);
const ON: u32 = rgb(46, 134, 222);
const OFF: u32 = rgb(232, 235, 240);
const LINE: u32 = rgb(226, 229, 234);
const PAGE_BG: u32 = rgb(246, 247, 249);
const SIDE_BG: u32 = rgb(238, 241, 245);
const SIDEBAR: i32 = 220;
const ROW: i32 = 60;

pub enum SettingsAction {
    None,
    Redraw,
    Change(Settings),
    SetClock { y: u16, mo: u8, d: u8, h: u8, mi: u8 },
    /// Restart in this resolution (None = the firmware's choice).
    Resolution(Option<(u32, u32)>),
    /// Open the Photo Frame app (more screensaver settings).
    Screensaver,
}

/// The sections in the sidebar.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Page {
    Appearance,
    DateTime,
    Display,
    Mouse,
    Spelling,
    ThisPc,
    About,
}

impl Page {
    pub const ALL: [Page; 7] = [Page::Appearance, Page::DateTime, Page::Display, Page::Mouse, Page::Spelling, Page::ThisPc, Page::About];
    pub fn name(self) -> &'static str {
        match self {
            Page::Appearance => "Appearance",
            Page::DateTime => "Date & time",
            Page::Display => "Display",
            Page::Mouse => "Mouse",
            Page::Spelling => "Spelling",
            Page::ThisPc => "This PC",
            Page::About => "About",
        }
    }
    fn blurb(self) -> &'static str {
        match self {
            Page::Appearance => "Background, text size",
            Page::DateTime => "Clock, date, time",
            Page::Display => "Resolution, screensaver",
            Page::Mouse => "Pointer speed",
            Page::Spelling => "Language, your words",
            Page::ThisPc => "CPU, memory, sensors",
            Page::About => "Version, screen, graphics",
        }
    }
}

/// Drop-down lists.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drop {
    Hour,
    Minute,
    Resolution,
    Saver,
    SaverWait,
    SlideTime,
}

/// What a click on an item does.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Act {
    None,
    Page(Page),
    BgPicker,
    Size(usize),
    Clock24(bool),
    ShowDate,
    /// Show the previous (-1) or next (1) month in the date picker.
    Month(i32),
    Day(u8),
    Open(Drop),
    Pm(bool),
    SetClock,
    ResetClock,
    FrameApp,
    Mouse(usize),
    Spell(usize),
    Remove(usize),
}

/// One thing on a page.
enum It {
    Card,
    Text { s: String, size: Size, bold: bool, col: u32 },
    Button { label: String, primary: bool, act: Act },
    /// One part of a segmented button; `ends`: (first, last).
    Seg { label: String, on: bool, ok: bool, ends: (bool, bool), act: Act },
    Toggle { on: bool, act: Act },
    Drop { label: String, act: Act },
    Day { label: String, sel: bool, today: bool, act: Act },
    BgPreview,
    Side { page: Page, on: bool },
}

impl It {
    fn act(&self) -> Act {
        match self {
            It::Button { act, .. } | It::Toggle { act, .. } | It::Drop { act, .. } | It::Day { act, .. } => *act,
            It::Seg { act, ok: true, .. } => *act,
            It::Side { page, .. } => Act::Page(*page),
            It::BgPreview => Act::BgPicker,
            _ => Act::None,
        }
    }
}

/// Builds a page top to bottom: cards holding rows of a label (and a line of help under
/// it) on the left and controls on the right.
struct Ui {
    items: Vec<(Rect, It)>,
    x: i32,
    w: i32,
    y: i32,
    card: usize,
}

impl Ui {
    fn text(&mut self, x: i32, y: i32, s: &str, size: Size, bold: bool, col: u32) {
        self.items.push((Rect::new(x, y, text_width(s, size, bold), 20), It::Text { s: String::from(s), size, bold, col }));
    }
    fn card(&mut self, title: &str) {
        self.text(self.x + 4, self.y, title, Size::Small, true, DIM);
        self.y += 28;
        self.card = self.items.len();
        self.items.push((Rect::new(self.x, self.y, self.w, 0), It::Card));
        self.y += 6;
    }
    fn end_card(&mut self) {
        let top = self.items[self.card].0.y;
        self.items[self.card].0.h = self.y + 6 - top;
        self.y += 6 + 26;
    }
    /// A row with a label (and help under it); returns where its controls go, from the right.
    fn row(&mut self, label: &str, help: &str, h: i32) -> Rect {
        let (x, y) = (self.x + 20, self.y);
        if !label.is_empty() {
            let ly = if help.is_empty() { y + (h - 20) / 2 } else { y + h / 2 - 21 };
            self.text(x, ly, label, Size::Small, true, INK);
            if !help.is_empty() {
                self.text(x, ly + 22, help, Size::Small, false, DIM);
            }
        }
        // A thin line between rows of a card.
        if self.items.len() > self.card + 1 && self.y > self.items[self.card].0.y + 8 {
            self.items.push((Rect::new(self.x + 20, y, self.w - 40, 1), It::Text { s: String::new(), size: Size::Small, bold: false, col: LINE }));
        }
        self.y += h;
        Rect::new(self.x + 20, y, self.w - 40, h)
    }
    /// A row with a label on the left and a value on the right.
    fn info(&mut self, label: &str, value: &str) {
        let row = self.row(label, "", ROW - 8);
        let room = row.w - text_width(label, Size::Small, true) - 40;
        let v = crate::photos::ellipsize(value, room.max(80), Size::Small, false);
        self.text(row.x + row.w - text_width(&v, Size::Small, false), row.y + (row.h - 20) / 2, &v, Size::Small, false, INK);
    }
    /// A row whose value is long: label, then the value, then a line of help under it.
    fn stacked(&mut self, label: &str, value: &str, help: &str) {
        let row = self.row("", "", ROW + 30);
        self.text(row.x, row.y + 12, label, Size::Small, true, INK);
        let v = crate::photos::ellipsize(value, row.w, Size::Small, false);
        self.text(row.x, row.y + 36, &v, Size::Small, false, INK);
        let h = crate::photos::ellipsize(help, row.w, Size::Small, false);
        self.text(row.x, row.y + 60, &h, Size::Small, false, DIM);
    }

    fn button(&mut self, r: Rect, label: &str, primary: bool, act: Act) {
        self.items.push((r, It::Button { label: String::from(label), primary, act }));
    }
    /// Segmented buttons ending at the right edge of `row`.
    fn seg(&mut self, row: Rect, options: &[(&str, bool, bool, Act)]) {
        let widths: Vec<i32> = options.iter().map(|o| text_width(o.0, Size::Small, true) + 32).collect();
        let total: i32 = widths.iter().sum();
        let mut x = row.x + row.w - total;
        for (k, (o, w)) in options.iter().zip(&widths).enumerate() {
            let ends = (k == 0, k + 1 == options.len());
            self.items.push((Rect::new(x, row.y + (row.h - 36) / 2, *w, 36), It::Seg { label: String::from(o.0), on: o.1, ok: o.2, ends, act: o.3 }));
            x += w;
        }
    }
    fn drop(&mut self, r: Rect, label: &str, act: Act) {
        self.items.push((r, It::Drop { label: String::from(label), act }));
    }
    /// A drop-down at the right edge of `row`.
    fn drop_right(&mut self, row: Rect, w: i32, label: &str, act: Act) {
        self.drop(Rect::new(row.x + row.w - w, row.y + (row.h - 38) / 2, w, 38), label, act);
    }
}

pub struct SettingsApp {
    page: Page,
    /// Date and time being edited: year, month, day, hour, minute.
    t: (u16, u8, u8, u8, u8),
    edited: bool,
    /// The month shown in the date picker.
    month: (u16, u8),
    /// An open drop-down list and the box it drops from.
    open: Option<(Drop, Rect)>,
    /// The background chooser is open.
    bg_picker: bool,
    /// How far the page is scrolled.
    scroll: i32,
    /// Previews of the built-in photos, and of the user's own desktop picture.
    thumbs: Vec<crate::image::Image>,
    custom_thumb: Option<crate::image::Image>,
}

/// The resolutions offered: the PC's modes that are big enough for Solon's desktop.
fn res_modes() -> Vec<(u32, u32)> {
    let info = crate::display::INFO.lock().clone().unwrap_or_default();
    info.modes.into_iter().filter(|&m| crate::display::usable(m)).collect()
}

fn twelve(h: u8) -> u8 {
    if h % 12 == 0 {
        12
    } else {
        h % 12
    }
}

fn clock_label(h: u8, mi: u8, h24: bool) -> String {
    if h24 {
        format!("{:02}:{:02}", h, mi)
    } else {
        format!("{}:{:02} {}", twelve(h), mi, if h < 12 { "AM" } else { "PM" })
    }
}

const LONG_MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];

impl SettingsApp {
    pub fn new() -> SettingsApp {
        let thumbs = PHOTOS.iter().filter_map(|p| crate::image::decode(p.2).ok()).collect();
        let mut s = SettingsApp { page: Page::Appearance, t: (2026, 1, 1, 0, 0), edited: false, month: (2026, 1), open: None, bg_picker: false, scroll: 0, thumbs, custom_thumb: None };
        s.reload_time();
        s
    }

    /// Start editing from the current clock (when the window opens).
    pub fn reload_time(&mut self) {
        let (y, mo, d) = crate::rtc::date();
        let (h, mi, _) = crate::rtc::time();
        self.t = (y, mo, d, h, mi);
        self.month = (y, mo);
        self.edited = false;
        self.bg_picker = false;
        self.open = None;
    }

    /// Show this page (the desktop's right-click menu), with the background chooser open
    /// if asked.
    pub fn show(&mut self, page: Page, bg_picker: bool) {
        self.page = page;
        self.scroll = 0;
        self.open = None;
        self.bg_picker = bg_picker;
    }

    /// The user's own desktop picture changed (None = there isn't one).
    pub fn set_custom_thumb(&mut self, img: Option<crate::image::Image>) {
        self.custom_thumb = img;
    }

    fn content(r: Rect) -> Rect {
        Rect::new(r.x + SIDEBAR, r.y, r.w - SIDEBAR, r.h)
    }

    // ---- layout -------------------------------------------------------------------------

    fn items(&self, r: Rect, s: &Settings, screen: (usize, usize), saved: bool) -> Vec<(Rect, It)> {
        let mut items: Vec<(Rect, It)> = Vec::new();
        // Sidebar.
        for (k, p) in Page::ALL.iter().enumerate() {
            items.push((Rect::new(r.x + 12, r.y + 70 + k as i32 * 58, SIDEBAR - 24, 52), It::Side { page: *p, on: *p == self.page }));
        }
        let cr = Self::content(r);
        let w = (cr.w - 64).clamp(300, 780);
        let mut ui = Ui { items, x: cr.x + 32, w, y: cr.y + 76 - self.scroll, card: 0 };
        match self.page {
            Page::Appearance => self.appearance(&mut ui, s, screen),
            Page::DateTime => self.date_time(&mut ui, s),
            Page::Display => self.display(&mut ui, s),
            Page::Mouse => {
                ui.card("Mouse");
                let row = ui.row("Pointer speed", "How far the pointer moves for a move of the mouse", ROW + 8);
                let opts: Vec<(&str, bool, bool, Act)> = MOUSE.iter().enumerate().map(|(i, m)| (m.0, s.mouse == i, true, Act::Mouse(i))).collect();
                ui.seg(row, &opts);
                ui.end_card();
                ui.card("Tips");
                ui.row("Right-click the desktop", "for quick ways to the background, display and other settings", ROW + 8);
                ui.row("Right-click a word marked in red", "for spelling suggestions (or press F7)", ROW + 8);
                ui.end_card();
            }
            Page::Spelling => self.spelling(&mut ui, s),
            Page::ThisPc => self.this_pc(&mut ui),
            Page::About => {
                ui.card("Solon");
                let row = ui.row("Version", "", ROW);
                let v = crate::update::Build::current().label();
                ui.text(row.x + row.w - text_width(&v, Size::Small, false), row.y + 20, &v, Size::Small, false, INK);
                let row = ui.row("Screen", "", ROW);
                let info = crate::display::INFO.lock().clone().unwrap_or_default();
                let sc = format!("{} x {}", info.current.0.max(screen.0 as u32), info.current.1.max(screen.1 as u32));
                ui.text(row.x + row.w - text_width(&sc, Size::Small, false), row.y + 20, &sc, Size::Small, false, INK);
                let row = ui.row("Graphics", "", ROW);
                let g: Vec<String> = crate::gpu::GPUS.lock().iter().map(|g| g.name.clone()).collect();
                let g = if g.is_empty() { String::from("None found") } else { g.join(", ") };
                let g = crate::photos::ellipsize(&g, row.w - 120, Size::Small, false);
                ui.text(row.x + row.w - text_width(&g, Size::Small, false), row.y + 20, &g, Size::Small, false, INK);
                let row = ui.row("Settings", "", ROW);
                let note = if saved { "Saved in Solon storage, kept after a restart" } else { "Kept until the computer is turned off (no Solon storage yet)" };
                ui.text(row.x + row.w - text_width(note, Size::Small, false), row.y + 20, note, Size::Small, false, INK);
                ui.end_card();
            }
        }
        ui.items
    }

    /// The page shows readings that change (the desktop redraws it every second).
    pub fn live(&self) -> bool {
        self.page == Page::ThisPc && !self.bg_picker
    }

    fn this_pc(&self, ui: &mut Ui) {
        let ghz = |mhz: u32| format!("{}.{:02} GHz", mhz / 1000, (mhz % 1000 + 5) / 10 % 100);
        let cpu = crate::cpu::info();
        let now = crate::cpu::latest();
        ui.card("Processor");
        ui.info("Name", if cpu.brand.is_empty() { "Not named by the processor" } else { &cpu.brand });
        let n = |k: u32, one: &str, many: &str| format!("{} {}", k, if k == 1 { one } else { many });
        ui.info("Cores", &format!("{}, {} (Solon uses one for now)", n(cpu.cores, "core", "cores"), n(cpu.threads, "thread", "threads")));
        let base = cpu.pstates.first().copied().unwrap_or(cpu.tsc_mhz);
        let speed = match now.mhz {
            Some(m) => format!("{} while busy (base {})", ghz(m), ghz(base)),
            None => format!("Base {}", ghz(base)),
        };
        ui.info("Speed", &speed);
        if let Some(t) = now.temp {
            let mut v = crate::cpu::celsius(t);
            for &(c, t) in &now.ccd {
                v.push_str(&format!(", chiplet {}: {}", c + 1, crate::cpu::celsius(t)));
            }
            ui.info("Temperature", &v);
        }
        if let Some(mw) = now.mw {
            ui.info("Power", &format!("{}.{} W for the whole package", mw / 1000, mw % 1000 / 100));
        }
        ui.end_card();
        if let Some(b) = crate::smbios::get() {
            let join = |parts: &[&str]| parts.iter().filter(|p| !p.is_empty()).copied().collect::<Vec<_>>().join(" ");
            ui.card("Motherboard");
            let model = join(&[&b.board_maker, &b.board_product]);
            let model = if model.is_empty() { join(&[&b.system_maker, &b.system_product]) } else { model };
            ui.info("Model", if model.is_empty() { "Not named by the firmware" } else { &model });
            let bios = join(&[&b.bios_vendor, &b.bios_version]);
            ui.info("BIOS", &if b.bios_date.is_empty() { bios } else { format!("{}, {}", bios, b.bios_date) });
            if !b.socket.is_empty() {
                ui.info("Processor socket", &b.socket);
            }
            ui.end_card();
            let fitted: Vec<&crate::smbios::Dimm> = b.dimms.iter().filter(|d| d.mib > 0).collect();
            if !fitted.is_empty() {
                ui.card("Memory");
                let total: u64 = fitted.iter().map(|d| d.mib).sum();
                let slots = b.dimms.len();
                ui.info("Installed", &format!("{} GB in {} of {} {}", total / 1024, fitted.len(), slots, if slots == 1 { "slot" } else { "slots" }));
                for d in fitted {
                    let mut v = format!("{} GB", d.mib / 1024);
                    if !d.kind.is_empty() {
                        v.push_str(&format!(" {}", d.kind));
                    }
                    if d.configured > 0 || d.speed > 0 {
                        v.push_str(&format!(", {} MT/s", if d.configured > 0 { d.configured } else { d.speed }));
                    }
                    let who = join(&[&d.maker, &d.part]);
                    if !who.is_empty() {
                        v.push_str(&format!(", {}", who));
                    }
                    ui.info(if d.slot.is_empty() { "Module" } else { &d.slot }, &v);
                }
                ui.end_card();
            }
        }
        match crate::hwmon::latest() {
            Some(r) => {
                ui.card(&format!("Sensors ({})", r.chip));
                for (name, t) in &r.temps {
                    ui.info(name, &crate::cpu::celsius(*t));
                }
                for (n, rpm) in &r.fans {
                    ui.info(n, &if *rpm == 0 { String::from("Not turning (or nothing connected)") } else { format!("{} RPM", rpm) });
                }
                for &(name, mv) in &r.volts {
                    ui.info(name, &format!("{}.{:03} V", mv / 1000, mv % 1000));
                }
                ui.end_card();
            }
            None => {
                ui.card("Sensors");
                ui.row("No sensor chip Solon can read", "Fans, board temperatures and voltages show for Nuvoton NCT6798D and NCT6799D chips", ROW + 8);
                ui.end_card();
            }
        }
    }

    fn appearance(&self, ui: &mut Ui, s: &Settings, screen: (usize, usize)) {
        ui.card("Background");
        let row = ui.row("", "", 90);
        ui.items.push((Rect::new(row.x, row.y + 15, 96, 60), It::BgPreview));
        ui.text(row.x + 116, row.y + 24, crate::wallpaper::name(s.background), Size::Small, true, INK);
        ui.text(row.x + 116, row.y + 46, "The picture or colour behind your windows", Size::Small, false, DIM);
        ui.button(Rect::new(row.x + row.w - 190, row.y + 26, 190, 38), "Change background", false, Act::BgPicker);
        ui.end_card();
        ui.card("Text and icons");
        let row = ui.row("Size", "Bigger sizes need a bigger screen", ROW + 8);
        let opts: Vec<(&str, bool, bool, Act)> = SIZES.iter().enumerate().map(|(i, z)| (z.0, s.size == i, s.size_fits(i, screen.0, screen.1), Act::Size(i))).collect();
        ui.seg(row, &opts);
        ui.end_card();
    }

    fn date_time(&self, ui: &mut Ui, s: &Settings) {
        ui.card("Clock");
        let row = ui.row("Time format", "", ROW);
        ui.seg(row, &[("12-hour", !s.clock24, true, Act::Clock24(false)), ("24-hour", s.clock24, true, Act::Clock24(true))]);
        let row = ui.row("Show the date", "Next to the clock at the bottom of the screen", ROW + 8);
        ui.items.push((Rect::new(row.x + row.w - 52, row.y + (row.h - 28) / 2, 52, 28), It::Toggle { on: s.show_date, act: Act::ShowDate }));
        ui.end_card();

        ui.card("Date and time");
        let (y, mo, d, h, mi) = self.t;
        let top = ui.y + 14;
        let x0 = ui.x + 20;
        // The month: header with arrows, weekday letters, then the days.
        let (my, mm) = self.month;
        let cell = (44, 36);
        let head = format!("{} {}", LONG_MONTHS[(mm as usize).clamp(1, 12) - 1], my);
        ui.text(x0 + 6, top + 8, &head, Size::Small, true, INK);
        ui.button(Rect::new(x0 + 7 * cell.0 - 80, top, 38, 34), "<", false, Act::Month(-1));
        ui.button(Rect::new(x0 + 7 * cell.0 - 38, top, 38, 34), ">", false, Act::Month(1));
        for (k, l) in ["S", "M", "T", "W", "T", "F", "S"].iter().enumerate() {
            ui.text(x0 + k as i32 * cell.0 + 17, top + 46, l, Size::Small, true, DIM);
        }
        let first = weekday(my, mm, 1) as i32;
        let today = crate::rtc::date();
        for day in 1..=days_in_month(my, mm) {
            let k = first + day as i32 - 1;
            let b = Rect::new(x0 + (k % 7) * cell.0, top + 72 + (k / 7) * cell.1, cell.0 - 4, cell.1 - 4);
            let sel = (y, mo, d) == (my, mm, day);
            ui.items.push((b, It::Day { label: format!("{}", day), sel, today: today == (my, mm, day), act: Act::Day(day) }));
        }
        // The time, right of the month (under it if the window is narrow).
        let side = ui.w >= 7 * cell.0 + 40 + 330;
        let (tx, ty) = if side { (x0 + 7 * cell.0 + 48, top) } else { (x0, top + 72 + 6 * cell.1 + 10) };
        ui.text(tx, ty + 8, "Time", Size::Small, true, INK);
        let hour = if s.clock24 { format!("{:02}", h) } else { format!("{}", twelve(h)) };
        ui.drop(Rect::new(tx, ty + 40, 92, 40), &hour, Act::Open(Drop::Hour));
        ui.text(tx + 100, ty + 49, ":", Size::Small, true, INK);
        ui.drop(Rect::new(tx + 114, ty + 40, 92, 40), &format!("{:02}", mi), Act::Open(Drop::Minute));
        if !s.clock24 {
            let row = Rect::new(tx + 220, ty + 40, 108, 40);
            ui.seg(row, &[("AM", h < 12, true, Act::Pm(false)), ("PM", h >= 12, true, Act::Pm(true))]);
        }
        let when = format!("{}, {} {}, {}", DAYS[weekday(y, mo, d)], LONG_MONTHS[mo as usize - 1], d, y);
        ui.text(tx, ty + 100, &when, Size::Small, false, DIM);
        ui.text(tx, ty + 122, &clock_label(h, mi, s.clock24), Size::Small, false, DIM);
        let by = ty + 156;
        ui.button(Rect::new(tx, by, 200, 40), "Set date and time", self.edited, if self.edited { Act::SetClock } else { Act::None });
        if self.edited {
            ui.button(Rect::new(tx + 210, by, 120, 40), "Undo", false, Act::ResetClock);
        }
        let bottom = (top + 72 + 6 * cell.1).max(by + 40);
        let bottom = if side { bottom } else { by + 40 };
        ui.y = bottom + 14;
        ui.end_card();
    }

    fn display(&self, ui: &mut Ui, s: &Settings) {
        let info = crate::display::INFO.lock().clone().unwrap_or_default();
        ui.card("Screen");
        let modes = res_modes();
        let help = if modes.len() <= 1 {
            String::from("This computer offers only one resolution")
        } else if crate::display::vbe_present() {
            String::from("Changes at once")
        } else if modes.iter().any(|&m| crate::display::change_for(m) == crate::display::Change::Restart) {
            format!("Changes at once up to {} x {}; bigger ones need a restart", info.boot.0, info.boot.1)
        } else {
            String::from("Changes at once (lower ones are scaled to fill the screen)")
        };
        let row = ui.row("Resolution", &help, ROW + 8);
        let mut label = format!("{} x {}", info.current.0, info.current.1);
        if info.native == Some(info.current) {
            label += " (best)";
        }
        ui.drop_right(row, 210, &label, if modes.len() > 1 { Act::Open(Drop::Resolution) } else { Act::None });
        // Graphics and monitors: the value has a line of its own (it can be long).
        let mut graphics = crate::gpu::main_name().unwrap_or_else(|| String::from("Unknown"));
        if let Some(r) = crate::nvidia::REPORT.lock().as_ref() {
            graphics = format!("{}: {}", graphics, r.summary);
        }
        ui.stacked("Graphics", &graphics, crate::gpu::support());
        let n = crate::display::screens();
        let monitors: Option<String> = crate::nvidia::REPORT.lock().as_ref().filter(|r| !r.connectors.is_empty()).map(|r| {
            let on: Vec<String> = r
                .connectors
                .iter()
                .filter(|c| c.plugged == Some(true) || c.monitor.is_some())
                .map(|c| match (&c.monitor, c.mode) {
                    (Some(m), Some((w, h, hz))) => format!("{} on {} ({}x{}, {} Hz)", m, c.label, w, h, (hz + 50) / 100),
                    (Some(m), None) => format!("{} on {}", m, c.label),
                    _ => format!("A monitor on {}", c.label),
                })
                .collect();
            if on.is_empty() {
                format!("None found on the card's {} connectors", r.connectors.len())
            } else {
                on.join(", ")
            }
        });
        let help = if n > 1 {
            String::from("Solon uses the first; using more is being worked on")
        } else if monitors.is_some() {
            format!("Solon shows {} of them for now; lighting the others is the driver's next stage", n)
        } else {
            String::from("A second monitor needs a driver for this graphics card")
        };
        ui.stacked("Monitors", &monitors.unwrap_or_else(|| format!("{} switched on by the PC", n)), &help);
        let row = ui.row("Monitor", "", ROW);
        let mut m = info.monitor.clone().unwrap_or_else(|| String::from("Unknown"));
        if let Some(hz) = info.refresh {
            m += &format!(", {} Hz", (hz + 50) / 100);
        }
        if let Some((w, h)) = info.native {
            if (w, h) != info.current {
                m += &format!(", best at {} x {}", w, h);
            }
        }
        ui.text(row.x + row.w - text_width(&m, Size::Small, false), row.y + 20, &m, Size::Small, false, INK);
        ui.end_card();

        ui.card("When the computer isn't used");
        let row = ui.row("Show", "The screensaver", ROW + 8);
        ui.drop_right(row, 210, SAVERS[s.saver.min(SAVERS.len() - 1)], Act::Open(Drop::Saver));
        let row = ui.row("Start after", "", ROW);
        ui.drop_right(row, 210, SAVER_WAIT[s.saver_wait.min(SAVER_WAIT.len() - 1)].0, Act::Open(Drop::SaverWait));
        let row = ui.row("Each photo for", "In the photo screensaver and Photo Frame", ROW + 8);
        ui.drop_right(row, 210, SLIDE_TIME[s.slide_time.min(SLIDE_TIME.len() - 1)].0, Act::Open(Drop::SlideTime));
        let row = ui.row("More", "Which photos, in what order", ROW + 8);
        ui.button(Rect::new(row.x + row.w - 210, row.y + (row.h - 38) / 2, 210, 38), "Open Photo Frame", false, Act::FrameApp);
        ui.end_card();
    }

    fn spelling(&self, ui: &mut Ui, s: &Settings) {
        ui.card("Spelling");
        let row = ui.row("Check spelling in", "Writer, Text Editor, spreadsheets, calendar...", ROW + 8);
        let opts: Vec<(&str, bool, bool, Act)> = crate::spell::CHOICES.iter().enumerate().map(|(i, c)| (*c, s.spelling == i, true, Act::Spell(i))).collect();
        ui.seg(row, &opts);
        ui.end_card();
        ui.card("Words you added");
        let words = crate::spell::user_words();
        if words.is_empty() {
            ui.row("None yet", "Right-click a word marked in red and choose Add to dictionary", ROW + 8);
        }
        for (i, w) in words.iter().enumerate() {
            let row = ui.row(w, "", 50);
            ui.button(Rect::new(row.x + row.w - 100, row.y + 7, 100, 36), "Remove", false, Act::Remove(i));
        }
        ui.end_card();
    }

    // ---- drop-down lists ----------------------------------------------------------------

    /// A list's choices, how many columns to show them in, and which is chosen.
    fn options(&self, d: Drop, s: &Settings) -> (Vec<String>, usize, Option<usize>) {
        let (_, _, _, h, mi) = self.t;
        match d {
            Drop::Hour if s.clock24 => ((0..24).map(|h| format!("{:02}", h)).collect(), 6, Some(h as usize)),
            Drop::Hour => ((0..12).map(|k| format!("{}", if k == 0 { 12 } else { k })).collect(), 4, Some(h as usize % 12)),
            Drop::Minute => ((0..60).map(|m| format!("{:02}", m)).collect(), 6, Some(mi as usize)),
            Drop::Resolution => {
                let info = crate::display::INFO.lock().clone().unwrap_or_default();
                let modes = res_modes();
                let v = modes
                    .iter()
                    .map(|&(w, h)| {
                        let mut l = format!("{} x {}", w, h);
                        if info.native == Some((w, h)) {
                            l += " (best)";
                        }
                        if crate::display::change_for((w, h)) == crate::display::Change::Restart {
                            l += " (restart)";
                        }
                        l
                    })
                    .collect();
                (v, 1, modes.iter().position(|&m| m == info.current))
            }
            Drop::Saver => (SAVERS.iter().map(|x| String::from(*x)).collect(), 1, Some(s.saver)),
            Drop::SaverWait => (SAVER_WAIT.iter().map(|x| String::from(x.0)).collect(), 1, Some(s.saver_wait)),
            Drop::SlideTime => (SLIDE_TIME.iter().map(|x| String::from(x.0)).collect(), 1, Some(s.slide_time)),
        }
    }

    /// Where the open list's choices are, below its box (above it if there's no room).
    fn option_rects(&self, r: Rect, s: &Settings) -> Vec<Rect> {
        let Some((d, anchor)) = self.open else { return Vec::new() };
        let (opts, mut cols, _) = self.options(d, s);
        let ch = 34;
        // A long list (resolutions) flows into more columns so it fits in the window.
        let fit = ((r.h - 40) / ch).max(4) as usize;
        let mut cw = 54;
        let down = cols == 1; // a list fills each column top to bottom
        if cols == 1 {
            cols = opts.len().div_ceil(fit).max(1);
            let widest = opts.iter().map(|o| text_width(o, Size::Small, false) + 40).max().unwrap_or(0);
            cw = if cols == 1 { anchor.w.max(widest) } else { widest };
        }
        let rows = opts.len().div_ceil(cols) as i32;
        let (w, h) = (cols as i32 * cw + 12, rows * ch + 12);
        let x = anchor.x.min(r.x + r.w - w - 8).max(r.x + 8);
        let y = if anchor.y + anchor.h + 4 + h <= r.y + r.h { anchor.y + anchor.h + 4 } else { (anchor.y - 4 - h).max(r.y + 8) };
        let rows = rows as usize;
        (0..opts.len())
            .map(|i| {
                let (col, row) = if down { (i / rows, i % rows) } else { (i % cols, i / cols) };
                Rect::new(x + 6 + col as i32 * cw, y + 6 + row as i32 * ch, cw, ch)
            })
            .collect()
    }

    fn choose(&mut self, d: Drop, i: usize, s: &Settings) -> SettingsAction {
        let (y, mo, dd, h, mi) = self.t;
        let mut n = *s;
        match d {
            Drop::Hour => {
                let h = if s.clock24 { i as u8 } else { i as u8 + if h >= 12 { 12 } else { 0 } };
                self.t = (y, mo, dd, h, mi);
                self.edited = true;
            }
            Drop::Minute => {
                self.t = (y, mo, dd, h, i as u8);
                self.edited = true;
            }
            Drop::Resolution => {
                let info = crate::display::INFO.lock().clone().unwrap_or_default();
                if let Some(&m) = res_modes().get(i) {
                    if m != info.current {
                        return SettingsAction::Resolution(Some(m));
                    }
                }
            }
            Drop::Saver => n.saver = i,
            Drop::SaverWait => n.saver_wait = i,
            Drop::SlideTime => n.slide_time = i,
        }
        if n != *s {
            SettingsAction::Change(n)
        } else {
            SettingsAction::Redraw
        }
    }

    // ---- drawing ------------------------------------------------------------------------

    /// `screen` = physical screen size (decides which text sizes fit); `saved` says
    /// whether settings are kept across restarts.
    pub fn draw(&self, c: &mut Canvas, r: Rect, s: &Settings, screen: (usize, usize), saved: bool) {
        c.fill_rect(r.x, r.y, r.w, r.h, PAGE_BG);
        c.fill_rect(r.x, r.y, SIDEBAR, r.h, SIDE_BG);
        c.fill_rect(r.x + SIDEBAR - 1, r.y, 1, r.h, LINE);
        c.text(r.x + 24, r.y + 20, "Settings", Size::Medium, true, INK);
        let cr = Self::content(r);
        if self.bg_picker {
            self.draw_sidebar_only(c, r, s);
            self.draw_bg_picker(c, cr, s);
            return;
        }
        let items = self.items(r, s, screen, saved);
        // Sidebar items aren't scrolled; the page is, under its title.
        for (b, it) in items.iter().filter(|(_, it)| matches!(it, It::Side { .. })) {
            Self::draw_item(self, c, *b, it, s);
        }
        c.text(r.x + 24, r.y + r.h - 34, if saved { "Changes are saved" } else { "Changes last until turned off" }, Size::Small, false, DIM);
        let body = Rect::new(cr.x, cr.y + 64, cr.w, cr.h - 64);
        c.set_clip(body);
        for (b, it) in items.iter().filter(|(_, it)| !matches!(it, It::Side { .. })) {
            Self::draw_item(self, c, *b, it, s);
        }
        c.reset_clip();
        c.set_clip(r);
        c.fill_rect(cr.x, cr.y, cr.w, 64, PAGE_BG);
        c.text(cr.x + 32, cr.y + 22, self.page.name(), Size::Medium, true, INK);
        // An open list goes over everything.
        if let Some((d, _)) = self.open {
            let rects = self.option_rects(cr, s);
            let (opts, _, sel) = self.options(d, s);
            if let (Some(f), Some(l)) = (rects.first(), rects.last()) {
                let (x0, y0) = (rects.iter().map(|b| b.x).min().unwrap_or(f.x) - 6, f.y - 6);
                let (x1, y1) = (rects.iter().map(|b| b.x + b.w).max().unwrap_or(l.x + l.w) + 6, l.y + l.h + 6);
                c.shadow(x0, y0, x1 - x0, y1 - y0, 10, 14, 70);
                c.round_rect(x0, y0, x1 - x0, y1 - y0, 10, rgb(255, 255, 255));
                c.outline_rect(x0, y0, x1 - x0, y1 - y0, LINE);
            }
            for (i, b) in rects.iter().enumerate() {
                let on = sel == Some(i);
                if on {
                    c.round_rect(b.x + 2, b.y + 2, b.w - 4, b.h - 4, 6, ON);
                }
                let col = if on { rgb(255, 255, 255) } else { INK };
                if b.w <= 60 {
                    c.text_centered(b.x + b.w / 2, b.y + 8, &opts[i], Size::Small, on, col);
                } else {
                    c.text(b.x + 14, b.y + 8, &opts[i], Size::Small, on, col);
                }
            }
        }
    }

    fn draw_sidebar_only(&self, c: &mut Canvas, r: Rect, s: &Settings) {
        for (k, p) in Page::ALL.iter().enumerate() {
            let b = Rect::new(r.x + 12, r.y + 70 + k as i32 * 58, SIDEBAR - 24, 52);
            self.draw_item(c, b, &It::Side { page: *p, on: *p == self.page }, s);
        }
    }

    fn draw_item(&self, c: &mut Canvas, b: Rect, it: &It, s: &Settings) {
        match it {
            It::Card => {
                c.round_rect(b.x, b.y, b.w, b.h, 12, rgb(255, 255, 255));
                c.outline_rect(b.x + 6, b.y, b.w - 12, 1, LINE);
                c.outline_rect(b.x + 6, b.y + b.h - 1, b.w - 12, 1, LINE);
            }
            It::Text { s: t, size, bold, col } => {
                if t.is_empty() {
                    c.fill_rect(b.x, b.y, b.w, b.h, *col);
                } else {
                    c.text(b.x, b.y, t, *size, *bold, *col);
                }
            }
            It::Button { label, primary, act } => {
                let enabled = *act != Act::None;
                let (bg, fg) = match (primary, enabled) {
                    (true, true) => (ON, rgb(255, 255, 255)),
                    (_, false) => (rgb(240, 242, 245), rgb(176, 182, 190)),
                    _ => (OFF, INK),
                };
                c.round_rect(b.x, b.y, b.w, b.h, 8, bg);
                c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, fg);
            }
            It::Seg { label, on, ok, ends, .. } => {
                let bg = if *on { ON } else { OFF };
                let fg = if *on { rgb(255, 255, 255) } else if *ok { INK } else { rgb(180, 186, 194) };
                // Rounded only at the outer ends.
                c.round_rect(b.x, b.y, b.w, b.h, 8, bg);
                if !ends.0 {
                    c.fill_rect(b.x, b.y, 10, b.h, bg);
                }
                if !ends.1 {
                    c.fill_rect(b.x + b.w - 10, b.y, 10, b.h, bg);
                    c.fill_rect(b.x + b.w - 1, b.y + 8, 1, b.h - 16, if *on { bg } else { rgb(210, 214, 220) });
                }
                c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, fg);
            }
            It::Toggle { on, .. } => {
                c.round_rect(b.x, b.y, b.w, b.h, b.h / 2, if *on { ON } else { rgb(204, 208, 214) });
                let k = b.h - 6;
                let kx = if *on { b.x + b.w - 3 - k } else { b.x + 3 };
                c.circle(kx + k / 2, b.y + b.h / 2, k / 2, rgb(255, 255, 255));
            }
            It::Drop { label, act } => {
                let enabled = *act != Act::None;
                let open = matches!((self.open, act), (Some((d, _)), Act::Open(e)) if d == *e);
                c.round_rect(b.x, b.y, b.w, b.h, 8, if open { ON } else { rgb(214, 218, 224) });
                c.round_rect(b.x + 1, b.y + 1, b.w - 2, b.h - 2, 7, rgb(255, 255, 255));
                c.text(b.x + 14, b.y + b.h / 2 - 9, label, Size::Small, false, if enabled { INK } else { DIM });
                if enabled {
                    let (ax, ay) = (b.x + b.w - 22, b.y + b.h / 2 - 2);
                    c.triangle([(ax - 5, ay - 2), (ax + 5, ay - 2), (ax, ay + 4)], DIM);
                }
            }
            It::Day { label, sel, today, .. } => {
                if *sel {
                    c.round_rect(b.x, b.y, b.w, b.h, 8, ON);
                } else if *today {
                    c.round_rect(b.x, b.y, b.w, b.h, 8, rgb(222, 234, 252));
                }
                c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, *sel || *today, if *sel { rgb(255, 255, 255) } else { INK });
            }
            It::BgPreview => self.draw_bg_tile(c, b, s.background, false),
            It::Side { page, on } => {
                if *on {
                    c.round_rect(b.x, b.y, b.w, b.h, 10, rgb(255, 255, 255));
                    c.round_rect(b.x + 4, b.y + 12, 4, b.h - 24, 2, ON);
                }
                c.text(b.x + 18, b.y + 6, page.name(), Size::Small, true, if *on { ON } else { INK });
                c.text(b.x + 18, b.y + 27, page.blurb(), Size::Small, false, DIM);
            }
        }
    }

    // ---- input --------------------------------------------------------------------------

    fn step_month(&mut self, delta: i32) {
        let (y, m) = self.month;
        let k = y as i32 * 12 + m as i32 - 1 + delta;
        self.month = ((k / 12).clamp(2000, 2099) as u16, (k % 12 + 1) as u8);
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, s: &Settings, screen: (usize, usize)) -> SettingsAction {
        let cr = Self::content(r);
        if self.bg_picker {
            for (k, p) in Page::ALL.iter().enumerate() {
                if Rect::new(r.x + 12, r.y + 70 + k as i32 * 58, SIDEBAR - 24, 52).contains(x, y) {
                    self.show(*p, false);
                    return SettingsAction::Redraw;
                }
            }
            return self.bg_picker_press(x, y, cr, s);
        }
        if let Some((d, _)) = self.open {
            let picked = self.option_rects(cr, s).iter().position(|b| b.contains(x, y));
            self.open = None;
            return match picked {
                Some(i) => self.choose(d, i, s),
                None => SettingsAction::Redraw,
            };
        }
        let items = self.items(r, s, screen, true);
        let body = Rect::new(cr.x, cr.y + 64, cr.w, cr.h - 64);
        let hit = items.iter().rev().find(|(b, it)| b.contains(x, y) && it.act() != Act::None && (matches!(it, It::Side { .. }) || body.contains(x, y)));
        let Some((b, it)) = hit else { return SettingsAction::None };
        let (b, act) = (*b, it.act());
        let (yy, mo, d, h, mi) = self.t;
        let mut n = *s;
        match act {
            Act::None => return SettingsAction::None,
            Act::Page(p) => {
                if p != self.page {
                    self.show(p, false);
                }
                return SettingsAction::Redraw;
            }
            Act::BgPicker => {
                self.bg_picker = true;
                return SettingsAction::Redraw;
            }
            Act::Size(i) => n.size = i,
            Act::Clock24(v) => n.clock24 = v,
            Act::ShowDate => n.show_date = !n.show_date,
            Act::Month(k) => {
                self.step_month(k);
                return SettingsAction::Redraw;
            }
            Act::Day(day) => {
                self.t = (self.month.0, self.month.1, day, h, mi);
                self.edited = true;
                return SettingsAction::Redraw;
            }
            Act::Open(dr) => {
                self.open = Some((dr, b));
                return SettingsAction::Redraw;
            }
            Act::Pm(pm) => {
                let h = h % 12 + if pm { 12 } else { 0 };
                self.t = (yy, mo, d, h, mi);
                self.edited = true;
                return SettingsAction::Redraw;
            }
            Act::SetClock => {
                self.edited = false;
                return SettingsAction::SetClock { y: yy, mo, d, h, mi };
            }
            Act::ResetClock => {
                self.reload_time();
                return SettingsAction::Redraw;
            }
            Act::FrameApp => return SettingsAction::Screensaver,
            Act::Mouse(i) => n.mouse = i,
            Act::Spell(i) => n.spelling = i,
            Act::Remove(i) => {
                if let Some(w) = crate::spell::user_words().get(i) {
                    crate::spell::remove(w);
                }
                return SettingsAction::Redraw;
            }
        }
        if n != *s {
            SettingsAction::Change(n)
        } else {
            SettingsAction::Redraw
        }
    }

    /// How far the page can scroll (0 when it fits).
    fn max_scroll(&self, r: Rect, s: &Settings, screen: (usize, usize)) -> i32 {
        let unscrolled = SettingsApp { scroll: 0, thumbs: Vec::new(), custom_thumb: None, ..*self };
        let bottom = unscrolled.items(r, s, screen, true).iter().filter(|(_, it)| !matches!(it, It::Side { .. })).map(|(b, _)| b.y + b.h).max().unwrap_or(0);
        (bottom + 20 - (r.y + r.h)).max(0)
    }

    /// Scroll a long page.
    pub fn on_wheel(&mut self, delta: i8, r: Rect, s: &Settings, screen: (usize, usize)) -> bool {
        if self.bg_picker || self.open.is_some() {
            return false;
        }
        let old = self.scroll;
        self.scroll = (old + delta as i32 * 60).clamp(0, self.max_scroll(r, s, screen));
        self.scroll != old
    }

    /// For the desktop's scroll bar: (area, how far, where), when the page doesn't fit.
    pub fn scroll_info(&self, r: Rect, s: &Settings, screen: (usize, usize)) -> Option<(Rect, i32, i32)> {
        if self.bg_picker {
            return None;
        }
        let max = self.max_scroll(r, s, screen);
        let cr = Self::content(r);
        (max > 0).then_some((Rect::new(cr.x, cr.y + 64, cr.w, cr.h - 64), max, self.scroll))
    }

    pub fn set_scroll(&mut self, v: i32) {
        self.scroll = v.max(0);
    }

    /// Esc closes an open list or the background chooser.
    pub fn on_key(&mut self, sc: u8) -> bool {
        if sc == 0x01 && (self.open.is_some() || self.bg_picker) {
            self.open = None;
            self.bg_picker = false;
            return true;
        }
        false
    }
}

// ---- background chooser (drawn over the page) -------------------------------------------

impl SettingsApp {
    fn tile_w(r: Rect) -> i32 {
        ((r.w - 64 - 4 * 16) / 5).min(170)
    }
    fn photo_tile(r: Rect, i: usize) -> Rect {
        let tw = Self::tile_w(r);
        Rect::new(r.x + 32 + i as i32 * (tw + 16), r.y + 112, tw, tw * 5 / 8)
    }
    fn colour_tile(r: Rect, i: usize) -> Rect {
        let tw = Self::tile_w(r);
        Rect::new(r.x + 32 + i as i32 * (tw + 16), Self::photo_tile(r, 0).y + tw * 5 / 8 + 72, tw, 56)
    }
    fn custom_tile(r: Rect) -> Rect {
        let tw = Self::tile_w(r);
        Rect::new(r.x + 32, Self::colour_tile(r, 0).y + 56 + 50, tw, tw * 5 / 8)
    }
    fn bg_done(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 148, r.y + 16, 116, 38)
    }

    /// A small picture of background `bg` in `b`.
    fn draw_bg_tile(&self, c: &mut Canvas, b: Rect, bg: usize, selected: bool) {
        if selected {
            c.round_rect(b.x - 4, b.y - 4, b.w + 8, b.h + 8, 10, ON);
        }
        if bg < FIRST_PHOTO {
            let (_, top, bottom) = BACKGROUNDS[bg];
            for k in 0..b.h {
                c.fill_rect(b.x, b.y + k, b.w, 1, mix(top, bottom, (k * 255 / b.h.max(1)) as u32));
            }
            return;
        }
        let img = if bg < CUSTOM { self.thumbs.get(bg - FIRST_PHOTO) } else { self.custom_thumb.as_ref() };
        match img {
            Some(img) => {
                let px = crate::frame::cover(img, b.w as usize, b.h as usize);
                c.draw_image(&crate::image::Image { w: b.w as usize, h: b.h as usize, px, opaque: true }, b.x, b.y);
            }
            None => {
                c.fill_rect(b.x, b.y, b.w, b.h, rgb(236, 238, 242));
                c.outline_rect(b.x, b.y, b.w, b.h, rgb(210, 214, 220));
                c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, "None yet", Size::Small, false, DIM);
            }
        }
    }

    fn draw_bg_picker(&self, c: &mut Canvas, r: Rect, s: &Settings) {
        c.text(r.x + 32, r.y + 22, "Choose a background", Size::Medium, true, INK);
        let b = Self::bg_done(r);
        c.round_rect(b.x, b.y, b.w, b.h, 8, ON);
        c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, "Done", Size::Small, true, rgb(255, 255, 255));
        c.text(r.x + 32, Self::photo_tile(r, 0).y - 32, "Nature photos", Size::Small, true, DIM);
        for (i, p) in PHOTOS.iter().enumerate() {
            let b = Self::photo_tile(r, i);
            self.draw_bg_tile(c, b, FIRST_PHOTO + i, s.background == FIRST_PHOTO + i);
            c.text_centered(b.x + b.w / 2, b.y + b.h + 8, p.0, Size::Small, false, INK);
        }
        c.text(r.x + 32, Self::colour_tile(r, 0).y - 32, "Colours", Size::Small, true, DIM);
        for (i, (name, _, _)) in BACKGROUNDS.iter().enumerate() {
            let b = Self::colour_tile(r, i);
            self.draw_bg_tile(c, b, i, s.background == i);
            c.text_centered(b.x + b.w / 2, b.y + 17, name, Size::Small, true, crate::wallpaper::ink(i));
        }
        let b = Self::custom_tile(r);
        c.text(r.x + 32, b.y - 32, "Your photo", Size::Small, true, DIM);
        self.draw_bg_tile(c, b, CUSTOM, s.background == CUSTOM && self.custom_thumb.is_some());
        let x = b.x + b.w + 24;
        c.text(x, b.y + 10, "To use one of your own photos, open it in Photos", Size::Small, false, INK);
        c.text(x, b.y + 34, "and press \"Desktop picture\" at the top.", Size::Small, false, INK);
        if self.custom_thumb.is_some() && s.background != CUSTOM {
            c.text(x, b.y + 66, "Or click the picture on the left to use it again.", Size::Small, false, DIM);
        }
    }

    fn bg_picker_press(&mut self, x: i32, y: i32, r: Rect, s: &Settings) -> SettingsAction {
        if Self::bg_done(r).contains(x, y) {
            self.bg_picker = false;
            return SettingsAction::Redraw;
        }
        let mut n = *s;
        for i in 0..PHOTOS.len() {
            if Self::photo_tile(r, i).contains(x, y) {
                n.background = FIRST_PHOTO + i;
            }
        }
        for i in 0..BACKGROUNDS.len() {
            if Self::colour_tile(r, i).contains(x, y) {
                n.background = i;
            }
        }
        if self.custom_thumb.is_some() && Self::custom_tile(r).contains(x, y) {
            n.background = CUSTOM;
        }
        if n != *s {
            SettingsAction::Change(n)
        } else {
            SettingsAction::None
        }
    }
}

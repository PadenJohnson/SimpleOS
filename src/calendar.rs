//! Calendar: a month at a time with today marked, holidays, and your own events —
//! birthdays (every year), appointments with times, things that happen every week or month.
//! Events are kept in Solon storage ("Calendar.txt"); the desktop shows today's under the clock.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::settings::{days_in_month, weekday, Settings, DAYS};
use crate::textfield::{TextEvent, TextField};

const INK: u32 = rgb(28, 32, 40);
const DIM: u32 = rgb(110, 116, 126);
const ACCENT: u32 = rgb(214, 64, 64);
const LONG_MONTHS: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
const COLORS: [u32; 6] = [rgb(46, 124, 232), rgb(214, 64, 64), rgb(30, 150, 80), rgb(150, 70, 200), rgb(230, 130, 20), rgb(90, 100, 116)];

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Repeat {
    Never,
    Weekly,
    Monthly,
    Yearly,
}

impl Repeat {
    const ALL: [Repeat; 4] = [Repeat::Never, Repeat::Weekly, Repeat::Monthly, Repeat::Yearly];
    fn name(self) -> &'static str {
        match self {
            Repeat::Never => "Just once",
            Repeat::Weekly => "Every week",
            Repeat::Monthly => "Every month",
            Repeat::Yearly => "Every year",
        }
    }
    fn code(self) -> &'static str {
        match self {
            Repeat::Never => "once",
            Repeat::Weekly => "weekly",
            Repeat::Monthly => "monthly",
            Repeat::Yearly => "yearly",
        }
    }
}

#[derive(Clone, Debug)]
pub struct Event {
    pub date: (u16, u8, u8),
    pub time: Option<(u8, u8)>,
    pub repeat: Repeat,
    pub color: u8,
    pub title: String,
}

/// "2026-11-21|14:30|yearly|1|Tom's birthday" per line ("-" for no time).
pub fn parse(text: &str) -> Vec<Event> {
    let mut out = Vec::new();
    for line in text.lines() {
        let f: Vec<&str> = line.splitn(5, '|').collect();
        if f.len() < 5 {
            continue;
        }
        let d: Vec<u32> = f[0].split('-').filter_map(|x| x.parse().ok()).collect();
        if d.len() != 3 || !(1..=12).contains(&d[1]) || !(1..=31).contains(&d[2]) {
            continue;
        }
        let time = f[1].split_once(':').and_then(|(h, m)| Some((h.parse::<u8>().ok()?.min(23), m.parse::<u8>().ok()?.min(59))));
        let repeat = Repeat::ALL.iter().copied().find(|r| r.code() == f[2]).unwrap_or(Repeat::Never);
        out.push(Event { date: (d[0] as u16, d[1] as u8, d[2] as u8), time, repeat, color: f[3].parse().unwrap_or(0).min(5), title: String::from(f[4]) });
    }
    out
}

pub fn to_text(events: &[Event]) -> String {
    let mut s = String::new();
    for e in events {
        let t = e.time.map(|(h, m)| format!("{:02}:{:02}", h, m)).unwrap_or_else(|| String::from("-"));
        s += &format!("{:04}-{:02}-{:02}|{}|{}|{}|{}\n", e.date.0, e.date.1, e.date.2, t, e.repeat.code(), e.color, e.title.replace('\n', " "));
    }
    s
}

fn day_number(y: u16, m: u8, d: u8) -> i64 {
    // Days since a fixed point (for comparing dates).
    let (y, m) = if m <= 2 { (y as i64 - 1, m as i64 + 12) } else { (y as i64, m as i64) };
    365 * y + y / 4 - y / 100 + y / 400 + (153 * (m - 3) + 2) / 5 + d as i64
}

impl Event {
    pub fn on(&self, y: u16, m: u8, d: u8) -> bool {
        let (sy, sm, sd) = self.date;
        if day_number(y, m, d) < day_number(sy, sm, sd) {
            return false;
        }
        match self.repeat {
            Repeat::Never => (sy, sm, sd) == (y, m, d),
            Repeat::Weekly => weekday(y, m, d) == weekday(sy, sm, sd),
            // On the 31st every month: the last day of shorter months.
            Repeat::Monthly => d == sd.min(days_in_month(y, m)),
            Repeat::Yearly => m == sm && (d == sd || (sm == 2 && sd == 29 && d == 28 && days_in_month(y, 2) == 28)),
        }
    }

    /// Years since the first one (for birthdays and anniversaries).
    fn years(&self, y: u16) -> Option<u16> {
        if self.repeat == Repeat::Yearly && y > self.date.0 {
            Some(y - self.date.0)
        } else {
            None
        }
    }
}

/// Events on a day, earliest first (all-day ones first).
pub fn on_day(events: &[Event], y: u16, m: u8, d: u8) -> Vec<usize> {
    let mut v: Vec<usize> = (0..events.len()).filter(|&i| events[i].on(y, m, d)).collect();
    v.sort_by_key(|&i| events[i].time.map(|(h, mm)| h as u32 * 60 + mm as u32 + 1).unwrap_or(0));
    v
}

/// The n-th (1-based; 5 = last) weekday `wd` (0 = Sunday) of a month.
fn nth_weekday(y: u16, m: u8, wd: usize, n: u8) -> u8 {
    let first = weekday(y, m, 1);
    let mut d = 1 + ((wd + 7 - first) % 7) as u8 + (n - 1) * 7;
    while d > days_in_month(y, m) {
        d -= 7;
    }
    d
}

fn easter(y: u16) -> (u8, u8) {
    let y = y as i32;
    let a = y % 19;
    let b = y / 100;
    let c = y % 100;
    let d = b / 4;
    let e = b % 4;
    let f = (b + 8) / 25;
    let g = (b - f + 1) / 3;
    let h = (19 * a + b - d - g + 15) % 30;
    let i = c / 4;
    let k = c % 4;
    let l = (32 + 2 * e + 2 * i - h - k) % 7;
    let m = (a + 11 * h + 22 * l) / 451;
    let month = (h + l - 7 * m + 114) / 31;
    let day = (h + l - 7 * m + 114) % 31 + 1;
    (month as u8, day as u8)
}

/// Holidays and special days (United States).
pub fn holiday(y: u16, m: u8, d: u8) -> Option<&'static str> {
    let wd = |n: u8, w: usize| nth_weekday(y, m, w, n) == d;
    let name = match (m, d) {
        (1, 1) => "New Year's Day",
        (2, 14) => "Valentine's Day",
        (3, 17) => "St. Patrick's Day",
        (6, 19) => "Juneteenth",
        (7, 4) => "Independence Day",
        (10, 31) => "Halloween",
        (11, 11) => "Veterans Day",
        (12, 24) => "Christmas Eve",
        (12, 25) => "Christmas Day",
        (12, 31) => "New Year's Eve",
        _ => {
            if m == 1 && wd(3, 1) {
                "Martin Luther King Jr. Day"
            } else if m == 2 && wd(3, 1) {
                "Presidents' Day"
            } else if m == 5 && wd(2, 0) {
                "Mother's Day"
            } else if m == 5 && wd(5, 1) {
                "Memorial Day"
            } else if m == 6 && wd(3, 0) {
                "Father's Day"
            } else if m == 9 && wd(1, 1) {
                "Labor Day"
            } else if m == 10 && wd(2, 1) {
                "Columbus Day"
            } else if m == 11 && wd(4, 4) {
                "Thanksgiving"
            } else if easter(y) == (m, d) {
                "Easter"
            } else {
                return None;
            }
        }
    };
    Some(name)
}

/// What the desktop shows under the clock for today.
pub fn today_lines(events: &[Event], y: u16, m: u8, d: u8, s: &Settings) -> Vec<String> {
    let mut v = Vec::new();
    if let Some(h) = holiday(y, m, d) {
        v.push(String::from(h));
    }
    for i in on_day(events, y, m, d) {
        let e = &events[i];
        let t = e.time.map(|(h, mm)| format!("{}  ", s.clock_text(h, mm))).unwrap_or_default();
        let years = e.years(y).map(|n| format!(" ({})", n)).unwrap_or_default();
        v.push(format!("{}{}{}", t, e.title, years));
    }
    v
}

struct EventDialog {
    editing: Option<usize>,
    date: (u16, u8, u8),
    title: TextField,
    all_day: bool,
    hour: u8,
    minute: u8,
    repeat: Repeat,
    color: u8,
}

pub enum CalAction {
    None,
    Redraw,
    /// The events changed: save this text.
    Save(String),
}

pub struct CalendarApp {
    pub events: Vec<Event>,
    year: u16,
    month: u8,
    sel: (u16, u8, u8),
    today: (u16, u8, u8),
    dialog: Option<EventDialog>,
    last_click: Option<((u16, u8, u8), u64)>,
    loaded: bool,
}

impl CalendarApp {
    pub fn new() -> CalendarApp {
        CalendarApp { events: Vec::new(), year: 2026, month: 1, sel: (2026, 1, 1), today: (2026, 1, 1), dialog: None, last_click: None, loaded: false }
    }

    pub fn loaded(&self) -> bool {
        self.loaded
    }

    pub fn load(&mut self, text: Option<&str>) {
        self.events = text.map(parse).unwrap_or_default();
        self.loaded = true;
    }

    /// Opening: show this month with today chosen.
    pub fn opened(&mut self, today: (u16, u8, u8)) {
        self.today = today;
        self.year = today.0;
        self.month = today.1;
        self.sel = today;
    }

    pub fn set_today(&mut self, today: (u16, u8, u8)) {
        self.today = today;
    }

    // ---- layout ---------------------------------------------------------------------------

    fn side_w(r: Rect) -> i32 {
        (r.w / 3).clamp(240, 340)
    }
    fn grid(r: Rect) -> Rect {
        Rect::new(r.x + 12, r.y + 98, r.w - Self::side_w(r) - 24, r.h - 110)
    }
    fn nav(r: Rect) -> [Rect; 3] {
        let x = r.x + r.w - Self::side_w(r) - 12;
        [Rect::new(x - 90 - 10 - 40 - 6 - 40, r.y + 16, 40, 38), Rect::new(x - 90 - 10 - 40, r.y + 16, 40, 38), Rect::new(x - 90, r.y + 16, 90, 38)]
    }
    fn cell(r: Rect, i: usize) -> Rect {
        let g = Self::grid(r);
        let (cw, ch) = (g.w / 7, g.h / 6);
        Rect::new(g.x + (i % 7) as i32 * cw, g.y + (i / 7) as i32 * ch, cw, ch)
    }
    /// Grid index -> date (cells before the 1st show the previous month, after the end the next).
    fn date_at(&self, i: usize) -> (u16, u8, u8) {
        let first = weekday(self.year, self.month, 1);
        let day = i as i32 - first as i32 + 1;
        let dim = days_in_month(self.year, self.month) as i32;
        if day < 1 {
            let (py, pm) = if self.month == 1 { (self.year - 1, 12) } else { (self.year, self.month - 1) };
            (py, pm, (days_in_month(py, pm) as i32 + day) as u8)
        } else if day > dim {
            let (ny, nm) = if self.month == 12 { (self.year + 1, 1) } else { (self.year, self.month + 1) };
            (ny, nm, (day - dim) as u8)
        } else {
            (self.year, self.month, day as u8)
        }
    }
    fn add_btn(r: Rect) -> Rect {
        let x = r.x + r.w - Self::side_w(r);
        Rect::new(x + 16, r.y + r.h - 56, Self::side_w(r) - 32, 42)
    }
    fn agenda_row(r: Rect, k: usize) -> Rect {
        let x = r.x + r.w - Self::side_w(r);
        Rect::new(x + 12, r.y + 110 + k as i32 * 58, Self::side_w(r) - 24, 52)
    }

    fn dlg(r: Rect) -> Rect {
        let w = 560.min(r.w - 40);
        Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - 420) / 2, w, 420)
    }
    /// Dialog controls: title, all-day, hour -/+, minute -/+, 4 repeats, 6 colours, Save, Delete, Cancel.
    fn dlg_parts(r: Rect) -> Vec<Rect> {
        let d = Self::dlg(r);
        let x = d.x + 130;
        let mut v = Vec::from([
            Rect::new(d.x + 24, d.y + 84, d.w - 48, 38),
            Rect::new(x, d.y + 140, 110, 36),
            Rect::new(x + 124, d.y + 140, 36, 36),
            Rect::new(x + 224, d.y + 140, 36, 36),
            Rect::new(x + 276, d.y + 140, 36, 36),
            Rect::new(x + 356, d.y + 140, 36, 36),
        ]);
        for k in 0..4 {
            v.push(Rect::new(x + (k % 2) as i32 * 186, d.y + 196 + (k / 2) as i32 * 44, 176, 36));
        }
        for k in 0..6 {
            v.push(Rect::new(x + k as i32 * 44, d.y + 296, 34, 34));
        }
        v.push(Rect::new(d.x + d.w - 24 - 110, d.y + d.h - 58, 110, 40));
        v.push(Rect::new(d.x + 24, d.y + d.h - 58, 100, 40));
        v.push(Rect::new(d.x + d.w - 24 - 110 - 10 - 100, d.y + d.h - 58, 100, 40));
        v
    }

    fn button(c: &mut Canvas, b: Rect, label: &str, on: bool, color: u32) {
        c.round_rect(b.x, b.y, b.w, b.h, 8, if on { color } else { rgb(236, 238, 243) });
        c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, if on { rgb(255, 255, 255) } else { INK });
    }

    fn time_text(s: &Settings, h: u8, m: u8) -> String {
        s.clock_text(h, m)
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect, s: &Settings) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(250, 250, 252));
        // Month title and navigation.
        let title = format!("{} {}", LONG_MONTHS[self.month as usize - 1], self.year);
        c.text(r.x + 22, r.y + 20, &title, Size::Large, true, INK);
        let [prev, next, today] = Self::nav(r);
        Self::button(c, prev, "<", false, ACCENT);
        Self::button(c, next, ">", false, ACCENT);
        Self::button(c, today, "Today", false, ACCENT);
        let g = Self::grid(r);
        let cw = g.w / 7;
        for (k, name) in DAYS.iter().enumerate() {
            c.text_centered(g.x + k as i32 * cw + cw / 2, r.y + 72, &name[..3], Size::Small, true, if k == 0 || k == 6 { ACCENT } else { DIM });
        }
        // Days.
        for i in 0..42 {
            let cr = Self::cell(r, i);
            let date = self.date_at(i);
            let this_month = date.1 == self.month;
            let sel = date == self.sel;
            c.fill_rect(cr.x, cr.y, cr.w, cr.h, if sel { rgb(228, 236, 250) } else if this_month { rgb(255, 255, 255) } else { rgb(246, 247, 249) });
            c.fill_rect(cr.x, cr.y, cr.w, 1, rgb(226, 229, 235));
            c.fill_rect(cr.x, cr.y, 1, cr.h, rgb(226, 229, 235));
            let num = format!("{}", date.2);
            let nx = cr.x + 10;
            if date == self.today {
                c.circle(nx + 10, cr.y + 16, 13, ACCENT);
                c.text_centered(nx + 10, cr.y + 8, &num, Size::Small, true, rgb(255, 255, 255));
            } else {
                c.text(nx + 10 - text_width(&num, Size::Small, true) / 2, cr.y + 8, &num, Size::Small, true, if this_month { INK } else { rgb(170, 175, 185) });
            }
            // Chips: the holiday, then events, as many as fit.
            let mut y = cr.y + 34;
            let room = |y: i32| y + 18 <= cr.y + cr.h - 2;
            let mut chips: Vec<(String, u32)> = Vec::new();
            if let Some(h) = holiday(date.0, date.1, date.2) {
                chips.push((String::from(h), rgb(200, 60, 60)));
            }
            for ei in on_day(&self.events, date.0, date.1, date.2) {
                let e = &self.events[ei];
                chips.push((e.title.clone(), COLORS[e.color as usize % COLORS.len()]));
            }
            let fit = (0..chips.len()).take_while(|&k| room(y + k as i32 * 20)).count();
            for (k, (t, col)) in chips.iter().enumerate().take(fit) {
                let more = chips.len() - fit;
                if k + 1 == fit && more > 0 {
                    c.text(cr.x + 8, y, &format!("+{} more", more + 1), Size::Small, false, DIM);
                    break;
                }
                c.round_rect(cr.x + 4, y, cr.w - 8, 18, 4, crate::gfx::mix(*col, 0xFF_FFFF, 200));
                c.fill_rect(cr.x + 4, y, 3, 18, *col);
                let t = crate::photos::ellipsize(t, cr.w - 18, Size::Small, false);
                c.text(cr.x + 10, y + 1, &t, Size::Small, false, INK);
                y += 20;
            }
        }
        // The chosen day.
        let sw = Self::side_w(r);
        let sx = r.x + r.w - sw;
        c.fill_rect(sx, r.y, sw, r.h, rgb(242, 244, 248));
        c.fill_rect(sx, r.y, 1, r.h, rgb(220, 224, 232));
        let (y, m, d) = self.sel;
        c.text(sx + 18, r.y + 22, DAYS[weekday(y, m, d)], Size::Medium, true, INK);
        c.text(sx + 18, r.y + 52, &format!("{} {}, {}", LONG_MONTHS[m as usize - 1], d, y), Size::Small, false, DIM);
        let mut k = 0;
        if let Some(h) = holiday(y, m, d) {
            let ar = Self::agenda_row(r, k);
            c.round_rect(ar.x, ar.y, ar.w, ar.h, 8, rgb(255, 255, 255));
            c.fill_rect(ar.x, ar.y + 8, 4, ar.h - 16, rgb(200, 60, 60));
            c.text(ar.x + 14, ar.y + 8, "Holiday", Size::Small, false, DIM);
            c.text(ar.x + 14, ar.y + 27, h, Size::Small, true, INK);
            k += 1;
        }
        let list = on_day(&self.events, y, m, d);
        for &ei in &list {
            let ar = Self::agenda_row(r, k);
            if ar.y + ar.h > Self::add_btn(r).y - 6 {
                break;
            }
            let e = &self.events[ei];
            c.round_rect(ar.x, ar.y, ar.w, ar.h, 8, rgb(255, 255, 255));
            c.fill_rect(ar.x, ar.y + 8, 4, ar.h - 16, COLORS[e.color as usize % COLORS.len()]);
            let when = match e.time {
                Some((h, mm)) => Self::time_text(s, h, mm),
                None => String::from("All day"),
            };
            let extra = match (e.repeat, e.years(y)) {
                (Repeat::Yearly, Some(n)) => format!("  -  {} year{}", n, if n == 1 { "" } else { "s" }),
                (Repeat::Never, _) => String::new(),
                (r, _) => format!("  -  {}", r.name().to_lowercase()),
            };
            c.text(ar.x + 14, ar.y + 8, &format!("{}{}", when, extra), Size::Small, false, DIM);
            c.text(ar.x + 14, ar.y + 27, &crate::photos::ellipsize(&e.title, ar.w - 24, Size::Small, true), Size::Small, true, INK);
            k += 1;
        }
        if k == 0 {
            c.text(sx + 18, r.y + 116, "Nothing on this day.", Size::Small, false, DIM);
        }
        Self::button(c, Self::add_btn(r), "Add an event", true, ACCENT);
        // Event dialog.
        if let Some(dl) = &self.dialog {
            c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 110);
            let d = Self::dlg(r);
            c.shadow(d.x, d.y + 6, d.w, d.h, 14, 22, 100);
            c.round_rect(d.x, d.y, d.w, d.h, 14, rgb(255, 255, 255));
            let (y, m, dd) = dl.date;
            let head = if dl.editing.is_some() { "Change event" } else { "New event" };
            c.text(d.x + 24, d.y + 20, head, Size::Medium, true, INK);
            c.text(d.x + 24, d.y + 52, &format!("{}, {} {}, {}", DAYS[weekday(y, m, dd)], LONG_MONTHS[m as usize - 1], dd, y), Size::Small, false, DIM);
            let p = Self::dlg_parts(r);
            dl.title.draw(c, p[0]);
            if dl.title.text.is_empty() {
                c.text(p[0].x + 12, p[0].y + 11, "What is it? (for example: Tom's birthday)", Size::Small, false, rgb(160, 166, 176));
            }
            c.text(d.x + 24, p[1].y + 9, "Time", Size::Small, true, INK);
            Self::button(c, p[1], "All day", dl.all_day, ACCENT);
            if !dl.all_day {
                Self::button(c, p[2], "-", false, ACCENT);
                let t = Self::time_text(s, dl.hour, dl.minute);
                c.text_centered(p[2].x + 82, p[2].y + 9, &t, Size::Small, true, INK);
                Self::button(c, p[3], "+", false, ACCENT);
                Self::button(c, p[4], "-15", false, ACCENT);
                Self::button(c, p[5], "+15", false, ACCENT);
            } else {
                c.text(p[2].x, p[2].y + 9, "(no set time)", Size::Small, false, DIM);
            }
            c.text(d.x + 24, p[6].y + 9, "Happens", Size::Small, true, INK);
            for (k, rp) in Repeat::ALL.iter().enumerate() {
                Self::button(c, p[6 + k], rp.name(), dl.repeat == *rp, ACCENT);
            }
            c.text(d.x + 24, p[10].y + 8, "Colour", Size::Small, true, INK);
            for k in 0..6 {
                let b = p[10 + k];
                if dl.color as usize == k {
                    c.round_rect(b.x - 3, b.y - 3, b.w + 6, b.h + 6, 9, INK);
                }
                c.round_rect(b.x, b.y, b.w, b.h, 7, COLORS[k]);
            }
            Self::button(c, p[16], "Save", true, ACCENT);
            if dl.editing.is_some() {
                Self::button(c, p[17], "Delete", false, ACCENT);
            }
            Self::button(c, p[18], "Cancel", false, ACCENT);
        }
    }

    fn open_dialog(&mut self, editing: Option<usize>, date: (u16, u8, u8)) {
        let d = match editing {
            Some(i) => {
                let e = &self.events[i];
                EventDialog { editing, date: e.date, title: TextField::any(&e.title, 80, false), all_day: e.time.is_none(), hour: e.time.map(|t| t.0).unwrap_or(9), minute: e.time.map(|t| t.1).unwrap_or(0), repeat: e.repeat, color: e.color }
            }
            None => EventDialog { editing, date, title: TextField::any("", 80, false), all_day: true, hour: 9, minute: 0, repeat: Repeat::Never, color: 0 },
        };
        self.dialog = Some(d);
    }

    fn save_dialog(&mut self) -> CalAction {
        let Some(d) = self.dialog.take() else { return CalAction::None };
        let title = String::from(d.title.text.trim());
        if title.is_empty() {
            self.dialog = Some(d);
            return CalAction::Redraw;
        }
        let ev = Event { date: d.date, time: if d.all_day { None } else { Some((d.hour, d.minute)) }, repeat: d.repeat, color: d.color, title };
        match d.editing {
            Some(i) if i < self.events.len() => self.events[i] = ev,
            _ => self.events.push(ev),
        }
        CalAction::Save(to_text(&self.events))
    }

    fn shift_month(&mut self, delta: i32) {
        let mi = self.year as i32 * 12 + self.month as i32 - 1 + delta;
        self.year = (mi / 12).clamp(1900, 2200) as u16;
        self.month = (mi % 12 + 1) as u8;
        let d = self.sel.2.min(days_in_month(self.year, self.month));
        self.sel = (self.year, self.month, d);
    }

    fn select(&mut self, date: (u16, u8, u8)) {
        self.sel = date;
        self.year = date.0;
        self.month = date.1;
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, now: u64) -> CalAction {
        if let Some(d) = &mut self.dialog {
            let p = Self::dlg_parts(r);
            let hit = |k: usize| p[k].contains(x, y);
            if hit(1) {
                d.all_day = !d.all_day;
            } else if !d.all_day && hit(2) {
                d.hour = (d.hour + 23) % 24;
            } else if !d.all_day && hit(3) {
                d.hour = (d.hour + 1) % 24;
            } else if !d.all_day && hit(4) {
                let t = (d.hour as i32 * 60 + d.minute as i32 - 15).rem_euclid(1440);
                d.hour = (t / 60) as u8;
                d.minute = (t % 60) as u8;
            } else if !d.all_day && hit(5) {
                let t = (d.hour as i32 * 60 + d.minute as i32 + 15).rem_euclid(1440);
                d.hour = (t / 60) as u8;
                d.minute = (t % 60) as u8;
            } else if let Some(k) = (0..4).find(|&k| hit(6 + k)) {
                d.repeat = Repeat::ALL[k];
            } else if let Some(k) = (0..6).find(|&k| hit(10 + k)) {
                d.color = k as u8;
            } else if hit(16) {
                return self.save_dialog();
            } else if hit(17) && d.editing.is_some() {
                let i = d.editing.unwrap();
                self.dialog = None;
                if i < self.events.len() {
                    self.events.remove(i);
                }
                return CalAction::Save(to_text(&self.events));
            } else if hit(18) {
                self.dialog = None;
            }
            return CalAction::Redraw;
        }
        let [prev, next, today] = Self::nav(r);
        if prev.contains(x, y) {
            self.shift_month(-1);
            return CalAction::Redraw;
        }
        if next.contains(x, y) {
            self.shift_month(1);
            return CalAction::Redraw;
        }
        if today.contains(x, y) {
            let t = self.today;
            self.select(t);
            return CalAction::Redraw;
        }
        if Self::add_btn(r).contains(x, y) {
            let s = self.sel;
            self.open_dialog(None, s);
            return CalAction::Redraw;
        }
        for i in 0..42 {
            if Self::cell(r, i).contains(x, y) {
                let date = self.date_at(i);
                let double = matches!(self.last_click, Some((dd, t)) if dd == date && now - t <= 45);
                self.last_click = Some((date, now));
                self.select(date);
                if double {
                    self.open_dialog(None, date);
                }
                return CalAction::Redraw;
            }
        }
        // An event in the day list.
        let (y0, m0, d0) = self.sel;
        let offset = if holiday(y0, m0, d0).is_some() { 1 } else { 0 };
        let list = on_day(&self.events, y0, m0, d0);
        for (k, &ei) in list.iter().enumerate() {
            if Self::agenda_row(r, k + offset).contains(x, y) {
                self.open_dialog(Some(ei), self.sel);
                return CalAction::Redraw;
            }
        }
        CalAction::None
    }

    pub fn on_wheel(&mut self, delta: i8) -> bool {
        self.shift_month(if delta > 0 { 1 } else { -1 });
        true
    }

    /// None = not handled (Esc closes the window).
    pub fn on_key(&mut self, sc: u8) -> Option<CalAction> {
        if let Some(d) = &mut self.dialog {
            return Some(match d.title.on_key(sc) {
                TextEvent::Submit => self.save_dialog(),
                TextEvent::Cancel => {
                    self.dialog = None;
                    CalAction::Redraw
                }
                TextEvent::Changed => CalAction::Redraw,
                TextEvent::None => CalAction::None,
            });
        }
        if sc & 0x80 != 0 {
            return Some(CalAction::None);
        }
        let (y, m, d) = self.sel;
        let step = |days: i64| -> (u16, u8, u8) {
            // Walk day by day (short distances only).
            let (mut y, mut m, mut d) = (y, m, d as i64);
            d += days;
            while d < 1 {
                if m == 1 {
                    y -= 1;
                    m = 12;
                } else {
                    m -= 1;
                }
                d += days_in_month(y, m) as i64;
            }
            while d > days_in_month(y, m) as i64 {
                d -= days_in_month(y, m) as i64;
                if m == 12 {
                    y += 1;
                    m = 1;
                } else {
                    m += 1;
                }
            }
            (y, m, d as u8)
        };
        match sc {
            0x4B => self.select(step(-1)),
            0x4D => self.select(step(1)),
            0x48 => self.select(step(-7)),
            0x50 => self.select(step(7)),
            0x49 => self.shift_month(-1),
            0x51 => self.shift_month(1),
            0x14 => {
                let t = self.today;
                self.select(t);
            }
            0x1C => {
                let s = self.sel;
                self.open_dialog(None, s);
            }
            0x01 => return None,
            _ => return Some(CalAction::None),
        }
        Some(CalAction::Redraw)
    }
}

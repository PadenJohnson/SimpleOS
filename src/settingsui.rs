//! The Settings window.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{mix, rgb, Canvas, Rect, Size};
use crate::settings::{days_in_month, Settings, BACKGROUNDS, MONTHS, MOUSE, SIZES};

const INK: u32 = rgb(40, 44, 52);
const DIM: u32 = rgb(110, 116, 126);
const ON: u32 = rgb(46, 134, 222);
const OFF: u32 = rgb(230, 233, 238);
const LABEL_W: i32 = 190;
const ROW: i32 = 76;

pub enum SettingsAction {
    None,
    Redraw,
    Change(Settings),
    SetClock { y: u16, mo: u8, d: u8, h: u8, mi: u8 },
    /// Restart in this resolution (None = the firmware's choice).
    Resolution(Option<(u32, u32)>),
}

pub struct SettingsApp {
    /// Date and time being edited: year, month, day, hour, minute.
    t: (u16, u8, u8, u8, u8),
    edited: bool,
    /// The resolution list is open.
    picker: bool,
}

fn button(c: &mut Canvas, b: Rect, label: &str, on: bool, enabled: bool) {
    let (bg, fg) = match (on, enabled) {
        (true, _) => (ON, rgb(255, 255, 255)),
        (false, true) => (OFF, INK),
        (false, false) => (rgb(244, 245, 247), rgb(190, 194, 200)),
    };
    c.round_rect(b.x, b.y, b.w, b.h, 8, bg);
    c.text_centered(b.x + b.w / 2, b.y + b.h / 2 - 9, label, Size::Small, true, fg);
}

impl SettingsApp {
    pub fn new() -> SettingsApp {
        let mut s = SettingsApp { t: (2026, 1, 1, 0, 0), edited: false, picker: false };
        s.reload_time();
        s
    }

    /// Start editing from the current clock (when the window opens).
    pub fn reload_time(&mut self) {
        let (y, mo, d) = crate::rtc::date();
        let (h, mi, _) = crate::rtc::time();
        self.t = (y, mo, d, h, mi);
        self.edited = false;
    }

    // ---- layout ----------------------------------------------------------------

    fn row_y(r: Rect, i: i32) -> i32 {
        // The date and time row is taller (its "Set clock" button sits underneath).
        r.y + 70 + i * ROW + if i >= 4 { 44 } else { 0 }
    }
    fn x0(r: Rect) -> i32 {
        r.x + 24 + LABEL_W
    }
    fn swatch(r: Rect, i: usize) -> Rect {
        Rect::new(Self::x0(r) + i as i32 * 96, Self::row_y(r, 0), 84, 44)
    }
    fn size_btn(r: Rect, i: usize) -> Rect {
        Rect::new(Self::x0(r) + i as i32 * 158, Self::row_y(r, 1) + 4, 148, 36)
    }
    fn clock_btn(r: Rect, i: usize) -> Rect {
        Rect::new(Self::x0(r) + i as i32 * 128, Self::row_y(r, 2) + 4, 118, 36)
    }
    fn date_btn(r: Rect) -> Rect {
        Rect::new(Self::x0(r) + 2 * 128 + 24, Self::row_y(r, 2) + 4, 170, 36)
    }
    /// Stepper for field f (0 hour, 1 minute, 2 day, 3 month, 4 year): (minus, value, plus).
    fn stepper(r: Rect, f: usize) -> (Rect, Rect, Rect) {
        let x = Self::x0(r) + f as i32 * 122;
        let y = Self::row_y(r, 3) + 22;
        (Rect::new(x, y, 30, 34), Rect::new(x + 30, y, 54, 34), Rect::new(x + 84, y, 30, 34))
    }
    fn set_clock_btn(r: Rect) -> Rect {
        Rect::new(Self::x0(r), Self::row_y(r, 3) + 66, 120, 32)
    }
    fn mouse_btn(r: Rect, i: usize) -> Rect {
        Rect::new(Self::x0(r) + i as i32 * 128, Self::row_y(r, 4) + 22, 118, 36)
    }

    // ---- drawing ----------------------------------------------------------------

    /// `screen` = physical screen size (decides which text sizes fit); `saved` says
    /// whether settings are kept across restarts.
    pub fn draw(&self, c: &mut Canvas, r: Rect, s: &Settings, screen: (usize, usize), saved: bool) {
        c.text(r.x + 24, r.y + 20, "Settings", Size::Medium, true, INK);
        c.fill_rect(r.x, r.y + 56, r.w, 1, rgb(228, 231, 236));

        let label = |c: &mut Canvas, i: i32, t: &str| c.text(r.x + 24, Self::row_y(r, i) + 12, t, Size::Small, true, DIM);

        label(c, 0, "Background");
        for (i, (name, top, bottom)) in BACKGROUNDS.iter().enumerate() {
            let b = Self::swatch(r, i);
            if s.background == i {
                c.round_rect(b.x - 4, b.y - 4, b.w + 8, b.h + 8, 10, ON);
            }
            for k in 0..b.h {
                let col = mix(*top, *bottom, (k * 255 / b.h.max(1)) as u32);
                c.fill_rect(b.x, b.y + k, b.w, 1, col);
            }
            c.text_centered(b.x + b.w / 2, b.y + 12, name, Size::Small, true, rgb(240, 242, 245));
        }

        label(c, 1, "Text and icon size");
        for (i, (name, _)) in SIZES.iter().enumerate() {
            button(c, Self::size_btn(r, i), name, s.size == i, s.size_fits(i, screen.0, screen.1));
        }

        label(c, 2, "Clock");
        button(c, Self::clock_btn(r, 0), "12-hour", !s.clock24, true);
        button(c, Self::clock_btn(r, 1), "24-hour", s.clock24, true);
        button(c, Self::date_btn(r), if s.show_date { "Date shown" } else { "Date hidden" }, s.show_date, true);

        label(c, 3, "Date and time");
        let (y, mo, d, h, mi) = self.t;
        let hour = if s.clock24 {
            format!("{:02}", h)
        } else {
            format!("{}{}", if h % 12 == 0 { 12 } else { h % 12 }, if h < 12 { "am" } else { "pm" })
        };
        let values = [hour, format!("{:02}", mi), format!("{}", d), String::from(MONTHS[(mo as usize).clamp(1, 12) - 1]), format!("{}", y)];
        for (f, name) in ["Hour", "Minute", "Day", "Month", "Year"].iter().enumerate() {
            let (minus, val, plus) = Self::stepper(r, f);
            c.text(minus.x, minus.y - 20, name, Size::Small, false, DIM);
            button(c, minus, "-", false, true);
            c.round_rect(val.x + 2, val.y, val.w - 4, val.h, 6, rgb(255, 255, 255));
            c.outline_rect(val.x + 2, val.y, val.w - 4, val.h, rgb(214, 218, 224));
            c.text_centered(val.x + val.w / 2, val.y + 8, &values[f], Size::Small, true, INK);
            button(c, plus, "+", false, true);
        }
        button(c, Self::set_clock_btn(r), "Set clock", self.edited, self.edited);

        label(c, 4, "Mouse speed");
        for (i, (name, _)) in MOUSE.iter().enumerate() {
            button(c, Self::mouse_btn(r, i), name, s.mouse == i, true);
        }

        label(c, 5, "Screen");
        self.draw_screen_row(c, r);

        let note = if saved {
            "Changes are saved and kept after a restart."
        } else {
            "Changes last until the computer is turned off (no Solon storage to save them in)."
        };
        c.text(r.x + 24, Self::row_y(r, 5) + 64, note, Size::Small, false, DIM);
        if self.picker {
            self.draw_picker(c, r);
        }
    }

    // ---- input ------------------------------------------------------------------

    fn step(&mut self, f: usize, delta: i32) {
        let (mut y, mut mo, mut d, mut h, mut mi) = self.t;
        match f {
            0 => h = ((h as i32 + delta).rem_euclid(24)) as u8,
            1 => mi = ((mi as i32 + delta).rem_euclid(60)) as u8,
            2 => {
                let n = days_in_month(y, mo) as i32;
                d = ((d as i32 - 1 + delta).rem_euclid(n) + 1) as u8;
            }
            3 => mo = ((mo as i32 - 1 + delta).rem_euclid(12) + 1) as u8,
            _ => y = (y as i32 + delta).clamp(2000, 2099) as u16,
        }
        d = d.min(days_in_month(y, mo));
        self.t = (y, mo, d, h, mi);
        self.edited = true;
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, s: &Settings, screen: (usize, usize)) -> SettingsAction {
        if self.picker {
            if Self::picker_close(r).contains(x, y) {
                self.picker = false;
                return SettingsAction::Redraw;
            }
            let current = crate::display::INFO.lock().as_ref().map(|i| i.current);
            for (m, b) in Self::picker_items(r) {
                if b.contains(x, y) {
                    self.picker = false;
                    if m == current {
                        return SettingsAction::Redraw;
                    }
                    return SettingsAction::Resolution(m);
                }
            }
            return SettingsAction::None;
        }
        if Self::res_btn(r).contains(x, y) {
            self.picker = true;
            return SettingsAction::Redraw;
        }
        let mut n = *s;
        for i in 0..BACKGROUNDS.len() {
            if Self::swatch(r, i).contains(x, y) {
                n.background = i;
            }
        }
        for i in 0..SIZES.len() {
            if Self::size_btn(r, i).contains(x, y) && s.size_fits(i, screen.0, screen.1) {
                n.size = i;
            }
        }
        if Self::clock_btn(r, 0).contains(x, y) {
            n.clock24 = false;
        }
        if Self::clock_btn(r, 1).contains(x, y) {
            n.clock24 = true;
        }
        if Self::date_btn(r).contains(x, y) {
            n.show_date = !n.show_date;
        }
        for i in 0..MOUSE.len() {
            if Self::mouse_btn(r, i).contains(x, y) {
                n.mouse = i;
            }
        }
        if n != *s {
            return SettingsAction::Change(n);
        }
        for f in 0..5 {
            let (minus, _, plus) = Self::stepper(r, f);
            if minus.contains(x, y) {
                self.step(f, -1);
                return SettingsAction::Redraw;
            }
            if plus.contains(x, y) {
                self.step(f, 1);
                return SettingsAction::Redraw;
            }
        }
        if self.edited && Self::set_clock_btn(r).contains(x, y) {
            let (yy, mo, d, h, mi) = self.t;
            self.edited = false;
            return SettingsAction::SetClock { y: yy, mo, d, h, mi };
        }
        SettingsAction::None
    }
}

// ---- screen resolution ----------------------------------------------------------------

impl SettingsApp {
    fn res_btn(r: Rect) -> Rect {
        Rect::new(Self::x0(r), Self::row_y(r, 5) + 4, 190, 36)
    }

    fn draw_screen_row(&self, c: &mut Canvas, r: Rect) {
        button(c, Self::res_btn(r), "Change resolution", false, true);
        let info = crate::display::INFO.lock().clone().unwrap_or_default();
        let mut line = format!("{} x {}", info.current.0, info.current.1);
        if let Some(hz) = info.refresh {
            line += &format!(", {} Hz", (hz + 50) / 100);
        }
        if let Some(name) = &info.monitor {
            line += &format!(", {}", name);
        }
        c.text(Self::x0(r) + 206, Self::row_y(r, 5) + 6, &line, Size::Small, true, INK);
        let sub = match info.native {
            Some((w, h)) if (w, h) != info.current => format!("The monitor's best: {} x {}. Refresh rate is set by the monitor.", w, h),
            _ => String::from("Refresh rate is set by the monitor and can't be changed here."),
        };
        c.text(Self::x0(r) + 206, Self::row_y(r, 5) + 26, &sub, Size::Small, false, DIM);
    }

    /// The list of resolutions: (mode, button).
    fn picker_items(r: Rect) -> Vec<(Option<(u32, u32)>, Rect)> {
        let info = crate::display::INFO.lock().clone().unwrap_or_default();
        let cols = ((r.w - 48) / 170).max(1);
        let mut v: Vec<(Option<(u32, u32)>, Rect)> = Vec::new();
        for (k, &m) in info.modes.iter().take(24).enumerate() {
            let k = k as i32;
            v.push((Some(m), Rect::new(r.x + 24 + (k % cols) * 170, r.y + 130 + (k / cols) * 46, 160, 38)));
        }
        v
    }
    fn picker_close(r: Rect) -> Rect {
        Rect::new(r.x + r.w - 140, r.y + 16, 116, 36)
    }

    fn draw_picker(&self, c: &mut Canvas, r: Rect) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(250, 250, 252));
        c.text(r.x + 24, r.y + 20, "Choose a screen resolution", Size::Medium, true, INK);
        button(c, Self::picker_close(r), "Cancel", false, true);
        c.text(r.x + 24, r.y + 62, "Solon restarts in the new resolution and asks whether to keep it.", Size::Small, false, DIM);
        c.text(r.x + 24, r.y + 84, "If the screen stays dark, just wait: after 15 seconds the old one comes back.", Size::Small, false, DIM);
        let info = crate::display::INFO.lock().clone().unwrap_or_default();
        for (m, b) in Self::picker_items(r) {
            let Some((w, h)) = m else { continue };
            let mut label = format!("{} x {}", w, h);
            if info.native == Some((w, h)) {
                label += " (best)";
            }
            button(c, b, &label, info.current == (w, h), true);
        }
        if info.modes.len() <= 1 {
            c.text(r.x + 24, r.y + 130, "This computer only offers one resolution.", Size::Small, true, INK);
        }
    }
}

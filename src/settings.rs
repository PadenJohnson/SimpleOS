//! User settings, saved as a small text file ("Settings.txt") in the Solon library.

use alloc::format;
use alloc::string::String;

use crate::gfx::rgb;

/// Desktop background colours: (name, top of gradient, bottom of gradient).
pub const BACKGROUNDS: [(&str, u32, u32); 5] = [
    ("Blue", rgb(28, 64, 88), rgb(14, 26, 40)),
    ("Green", rgb(34, 78, 62), rgb(14, 34, 28)),
    ("Purple", rgb(64, 44, 92), rgb(26, 18, 42)),
    ("Gray", rgb(70, 74, 80), rgb(28, 30, 34)),
    ("Warm", rgb(110, 70, 48), rgb(44, 26, 20)),
];

/// Text and icon sizes and their scale (in halves: 2 = 1x, 3 = 1.5x, 4 = 2x).
pub const SIZES: [(&str, u32); 3] = [("Normal", 2), ("Large", 3), ("Extra large", 4)];

/// Mouse speeds (percent of normal).
pub const MOUSE: [(&str, i32); 3] = [("Slow", 50), ("Normal", 100), ("Fast", 180)];

/// The desktop needs at least this many (scaled) pixels for every window to fit.
pub const MIN_W: usize = 1000;
pub const MIN_H: usize = 620;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Settings {
    pub background: usize,
    pub size: usize,
    pub clock24: bool,
    pub show_date: bool,
    pub mouse: usize,
}

impl Settings {
    /// First-boot defaults: bigger text on very high-resolution screens.
    pub fn defaults(screen_w: usize, screen_h: usize) -> Settings {
        let mut s = Settings { background: 0, size: 0, clock24: false, show_date: true, mouse: 1 };
        for i in (0..SIZES.len()).rev() {
            if screen_w >= 2400 && s.size_fits(i, screen_w, screen_h) {
                s.size = i;
                break;
            }
        }
        s
    }

    pub fn scale(&self) -> u32 {
        SIZES[self.size.min(SIZES.len() - 1)].1
    }

    /// Would size `i` leave enough room on this screen?
    pub fn size_fits(&self, i: usize, screen_w: usize, screen_h: usize) -> bool {
        let s = SIZES[i].1 as usize;
        screen_w * 2 / s >= MIN_W && screen_h * 2 / s >= MIN_H
    }

    pub fn to_text(&self) -> String {
        format!(
            "background={}\nsize={}\nclock24={}\nshow_date={}\nmouse={}\n",
            self.background, self.size, self.clock24 as u8, self.show_date as u8, self.mouse
        )
    }

    pub fn from_text(text: &str, screen_w: usize, screen_h: usize) -> Settings {
        let mut s = Settings::defaults(screen_w, screen_h);
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let n: usize = v.trim().parse().unwrap_or(usize::MAX);
            match k.trim() {
                "background" if n < BACKGROUNDS.len() => s.background = n,
                "size" if n < SIZES.len() => s.size = n,
                "clock24" if n <= 1 => s.clock24 = n == 1,
                "show_date" if n <= 1 => s.show_date = n == 1,
                "mouse" if n < MOUSE.len() => s.mouse = n,
                _ => {}
            }
        }
        if !s.size_fits(s.size, screen_w, screen_h) {
            s.size = 0;
        }
        s
    }

    /// "3:07 PM" or "15:07".
    pub fn clock_text(&self, h: u8, m: u8) -> String {
        if self.clock24 {
            format!("{:02}:{:02}", h, m)
        } else {
            let h12 = if h % 12 == 0 { 12 } else { h % 12 };
            format!("{}:{:02} {}", h12, m, if h < 12 { "AM" } else { "PM" })
        }
    }
}

const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
pub const MONTHS: [&str; 12] = ["Jan", "Feb", "Mar", "Apr", "May", "Jun", "Jul", "Aug", "Sep", "Oct", "Nov", "Dec"];

/// Day of the week (0 = Sunday), Sakamoto's method.
pub fn weekday(y: u16, m: u8, d: u8) -> usize {
    const T: [i32; 12] = [0, 3, 2, 5, 0, 3, 5, 1, 4, 6, 2, 4];
    let mut y = y as i32;
    if m < 3 {
        y -= 1;
    }
    ((y + y / 4 - y / 100 + y / 400 + T[(m as usize - 1) % 12] + d as i32) % 7) as usize
}

/// "Thursday, Nov 20".
pub fn date_text(y: u16, m: u8, d: u8) -> String {
    format!("{}, {} {}", DAYS[weekday(y, m, d)], MONTHS[(m as usize).clamp(1, 12) - 1], d)
}

pub fn days_in_month(y: u16, m: u8) -> u8 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

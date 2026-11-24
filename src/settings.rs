//! User settings, saved as a small text file ("Settings.txt") in the Solon library.

use alloc::format;
use alloc::string::String;

use crate::gfx::rgb;

/// Desktop background colours: (name, top of gradient, bottom of gradient).
pub const BACKGROUNDS: [(&str, u32, u32); 5] = [
    ("Blue", rgb(214, 230, 252), rgb(140, 176, 236)),
    ("Green", rgb(222, 244, 230), rgb(120, 196, 166)),
    ("Purple", rgb(238, 228, 252), rgb(176, 150, 232)),
    ("Gray", rgb(242, 244, 247), rgb(180, 188, 200)),
    ("Warm", rgb(254, 236, 220), rgb(240, 170, 140)),
];

/// Text and icon sizes and their scale (in halves: 2 = 1x, 3 = 1.5x, 4 = 2x).
pub const SIZES: [(&str, u32); 3] = [("Normal", 2), ("Large", 3), ("Extra large", 4)];

/// Mouse speeds (percent of normal).
pub const MOUSE: [(&str, i32); 3] = [("Slow", 50), ("Normal", 100), ("Fast", 180)];

/// What the screen shows when the computer isn't used.
pub const SAVERS: [&str; 3] = ["Show photos", "Show the clock", "Do nothing"];
/// How loud everything is (percent), in Settings > Sound.
pub const VOLUMES: [(&str, u8); 4] = [("Quiet", 25), ("Medium", 50), ("Loud", 75), ("Full", 100)];
/// Minutes without use before it starts.
pub const SAVER_WAIT: [(&str, u64); 6] = [("1 minute", 1), ("5 minutes", 5), ("10 minutes", 10), ("15 minutes", 15), ("30 minutes", 30), ("1 hour", 60)];
/// Seconds each photo stays on screen.
pub const SLIDE_TIME: [(&str, u64); 5] = [("5 seconds", 5), ("10 seconds", 10), ("20 seconds", 20), ("30 seconds", 30), ("1 minute", 60)];

/// How two screens are used: one desktop across both, the same on both, or only the main one.
pub const SCREENS: [&str; 3] = ["Extend", "Mirror", "Main screen only"];
/// Where the second screen is, next to the first (the one the PC starts on).
pub const SIDES: [&str; 4] = ["Left", "Right", "Above", "Below"];
/// How the two line up where they meet: side by side, or one above the other.
pub const ALIGNS_SIDE: [&str; 3] = ["Tops", "Middles", "Bottoms"];
pub const ALIGNS_STACKED: [&str; 3] = ["Left edges", "Middles", "Right edges"];

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
    /// The "install Solon on this computer" notice was shown.
    pub install_notice: bool,
    /// Screensaver: index into SAVERS.
    pub saver: usize,
    pub saver_wait: usize,
    pub slide_time: usize,
    /// Photos in a mixed-up order (else in folder order).
    pub shuffle: bool,
    /// Spelling: index into spell::CHOICES (English (US), English (UK), off).
    pub spelling: usize,
    /// The resolution the desktop is shown at; (0, 0) = the mode the screen started in.
    pub resolution: (u32, u32),
    /// Two screens: how they're used (SCREENS), which is the main one (0: the screen the PC
    /// starts on, 1: the second), where the second is next to the first (SIDES, as they stand
    /// on the desk, whichever is the main one) and how they line up (0: tops or left edges,
    /// 1: middles, 2: bottoms or right edges).
    pub screens: usize,
    pub main_screen: usize,
    pub side: usize,
    pub align: usize,
    /// The taskbar (dock) on every screen of an extended desktop, not only the main one.
    pub dock_all: bool,
    /// Where sound plays (crate::hda: AUTO, ALL_JACKS or an output's key), and how loud
    /// (percent).
    pub sound_out: u32,
    pub volume: u8,
}

impl Settings {
    /// First-boot defaults: bigger text on very high-resolution screens.
    pub fn defaults(screen_w: usize, screen_h: usize) -> Settings {
        let mut s = Settings {
            background: crate::wallpaper::DEFAULT,
            size: 0,
            clock24: false,
            show_date: true,
            mouse: 1,
            install_notice: false,
            saver: 0,
            saver_wait: 1,
            slide_time: 1,
            shuffle: true,
            spelling: 0,
            resolution: (0, 0),
            screens: 0,
            main_screen: 0,
            side: 1,
            align: 1,
            dock_all: true,
            sound_out: crate::hda::AUTO,
            volume: 100,
        };
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

    /// An extended desktop: the second screen next to the first as `side` and `align` say.
    /// Returns the desktop's size and each screen's place in it (x, y, w, h), for screens of
    /// `one` and `two` pixels.
    pub fn arrange(&self, one: (usize, usize), two: (usize, usize)) -> ((usize, usize), [(usize, usize, usize, usize); 2]) {
        let (w1, h1, w2, h2) = (one.0 as i64, one.1 as i64, two.0 as i64, two.1 as i64);
        let line = |len1: i64, len2: i64| match self.align {
            0 => 0,
            2 => len1 - len2,
            _ => (len1 - len2) / 2,
        };
        let (x2, y2) = match self.side {
            0 => (-w2, line(h1, h2)),
            2 => (line(w1, w2), -h2),
            3 => (line(w1, w2), h1),
            _ => (w1, line(h1, h2)),
        };
        let (x0, y0) = (x2.min(0), y2.min(0));
        let size = ((w1.max(x2 + w2) - x0) as usize, (h1.max(y2 + h2) - y0) as usize);
        (size, [((-x0) as usize, (-y0) as usize, one.0, one.1), ((x2 - x0) as usize, (y2 - y0) as usize, two.0, two.1)])
    }

    pub fn to_text(&self) -> String {
        format!(
            "background={}\nsize={}\nclock24={}\nshow_date={}\nmouse={}\ninstall_notice={}\nsaver={}\nsaver_wait={}\nslide_time={}\nshuffle={}\nspelling={}\nresolution={}x{}\nscreens={}\nmain_screen={}\nside={}\nalign={}\ndock_all={}\nsound_out={}\nvolume={}\n",
            self.background, self.size, self.clock24 as u8, self.show_date as u8, self.mouse, self.install_notice as u8, self.saver, self.saver_wait, self.slide_time, self.shuffle as u8, self.spelling, self.resolution.0, self.resolution.1,
            self.screens, self.main_screen, self.side, self.align, self.dock_all as u8, self.sound_out, self.volume
        )
    }

    pub fn from_text(text: &str, screen_w: usize, screen_h: usize) -> Settings {
        let mut s = Settings::defaults(screen_w, screen_h);
        for line in text.lines() {
            let Some((k, v)) = line.split_once('=') else { continue };
            let n: usize = v.trim().parse().unwrap_or(usize::MAX);
            if k.trim() == "resolution" {
                let mut it = v.trim().split('x').map(|p| p.trim().parse::<u32>().unwrap_or(0));
                if let (Some(rw), Some(rh)) = (it.next(), it.next()) {
                    s.resolution = (rw, rh);
                }
                continue;
            }
            match k.trim() {
                "background" if n < crate::wallpaper::COUNT => s.background = n,
                "size" if n < SIZES.len() => s.size = n,
                "clock24" if n <= 1 => s.clock24 = n == 1,
                "show_date" if n <= 1 => s.show_date = n == 1,
                "mouse" if n < MOUSE.len() => s.mouse = n,
                "install_notice" if n <= 1 => s.install_notice = n == 1,
                "saver" if n < SAVERS.len() => s.saver = n,
                "saver_wait" if n < SAVER_WAIT.len() => s.saver_wait = n,
                "slide_time" if n < SLIDE_TIME.len() => s.slide_time = n,
                "shuffle" if n <= 1 => s.shuffle = n == 1,
                "spelling" if n < crate::spell::CHOICES.len() => s.spelling = n,
                "screens" if n < SCREENS.len() => s.screens = n,
                "main_screen" if n <= 1 => s.main_screen = n,
                "side" if n < SIDES.len() => s.side = n,
                "align" if n < ALIGNS_SIDE.len() => s.align = n,
                "dock_all" if n <= 1 => s.dock_all = n == 1,
                "sound_out" if n <= u32::MAX as usize => s.sound_out = n as u32,
                "volume" if (5..=100).contains(&n) => s.volume = n as u8,
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

pub const DAYS: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
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
pub fn days_in_month(y: u16, m: u8) -> u8 {
    match m {
        2 if (y % 4 == 0 && y % 100 != 0) || y % 400 == 0 => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// "Sat, Nov 21", for the dock.
pub fn short_date_text(y: u16, m: u8, d: u8) -> String {
    let day = DAYS[weekday(y, m, d)];
    format!("{}, {} {}", &day[..3.min(day.len())], MONTHS[(m as usize).clamp(1, 12) - 1], d)
}

/// "Saturday, November 21", for the desktop.
pub fn long_date_text(y: u16, m: u8, d: u8) -> String {
    const LONG: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
    format!("{}, {} {}", DAYS[weekday(y, m, d)], LONG[(m as usize).clamp(1, 12) - 1], d)
}

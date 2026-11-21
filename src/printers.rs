//! The Printers app: which printers are plugged in, how Solon talks to each one, whether it
//! prints in colour and on what paper, its status, and a test page. Printers Solon can't use
//! say why, in plain words.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::print::{Lang, Paper, PrinterId};

const INK: u32 = rgb(28, 32, 40);
const DIM: u32 = rgb(106, 112, 124);
const ACCENT: u32 = rgb(142, 68, 173);
const GOOD: u32 = rgb(30, 140, 70);
const BAD: u32 = rgb(200, 60, 50);

/// What the app shows about one printer.
pub struct PrinterView {
    pub id: PrinterId,
    pub name: String,
    pub make: String,
    pub lang: Option<Lang>,
    pub problem: Option<&'static str>,
    pub color: bool,
    pub papers: Vec<Paper>,
    /// From the printer itself (driverless printers): "Ready", "Out of paper", ...
    pub status: Option<String>,
}

pub enum PrintersAction {
    None,
    Redraw,
    TestPage(PrinterId),
}

pub struct PrintersApp {
    selected: usize,
}

/// Printer status words from IPP printer-state-reasons.
pub fn status_text(state: u32, reasons: &[String]) -> String {
    let mut words: Vec<&str> = Vec::new();
    for r in reasons {
        let r = r.trim_end_matches("-error").trim_end_matches("-warning").trim_end_matches("-report");
        let w = match r {
            "media-empty" | "media-needed" => "Out of paper",
            "media-jam" => "Paper jam",
            "toner-low" | "marker-supply-low" => "Low on ink or toner",
            "toner-empty" | "marker-supply-empty" => "Out of ink or toner",
            "door-open" | "cover-open" => "A door or cover is open",
            "offline" => "Offline",
            "paused" => "Paused",
            _ => continue,
        };
        if !words.contains(&w) {
            words.push(w);
        }
    }
    if !words.is_empty() {
        return words.join(", ");
    }
    String::from(match state {
        4 => "Printing",
        5 => "Needs attention",
        _ => "Ready",
    })
}

fn printer_icon(c: &mut Canvas, x: i32, y: i32, ok: bool) {
    let col = if ok { ACCENT } else { rgb(150, 156, 166) };
    c.round_rect(x, y, 44, 44, 10, col);
    c.fill_rect(x + 14, y + 8, 16, 9, rgb(255, 255, 255));
    c.round_rect(x + 7, y + 16, 30, 14, 3, rgb(255, 255, 255));
    c.fill_rect(x + 13, y + 25, 18, 2, col);
    c.fill_rect(x + 14, y + 29, 16, 9, rgb(255, 255, 255));
}

impl PrintersApp {
    pub fn new() -> PrintersApp {
        PrintersApp { selected: 0 }
    }

    fn list_w(r: Rect) -> i32 {
        (r.w * 2 / 5).clamp(280, 380)
    }
    fn card(r: Rect, i: usize) -> Rect {
        Rect::new(r.x + 16, r.y + 76 + i as i32 * 74, Self::list_w(r) - 32, 66)
    }
    fn test_btn(r: Rect) -> Rect {
        let x = r.x + Self::list_w(r) + 24;
        Rect::new(x, r.y + r.h - 64, 220, 44)
    }

    pub fn draw(&self, c: &mut Canvas, r: Rect, printers: &[PrinterView]) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(250, 250, 252));
        c.text(r.x + 22, r.y + 20, "Printers", Size::Large, true, INK);
        if printers.is_empty() {
            self.draw_none(c, r);
            return;
        }
        let lw = Self::list_w(r);
        c.fill_rect(r.x + lw, r.y + 64, 1, r.h - 64, rgb(224, 228, 234));
        for (i, p) in printers.iter().enumerate() {
            let b = Self::card(r, i);
            if b.y + b.h > r.y + r.h {
                break;
            }
            let sel = i == self.selected.min(printers.len() - 1);
            c.round_rect(b.x, b.y, b.w, b.h, 12, if sel { rgb(236, 226, 244) } else { rgb(255, 255, 255) });
            printer_icon(c, b.x + 12, b.y + 11, p.lang.is_some());
            let name = crate::photos::ellipsize(&p.name, b.w - 80, Size::Small, true);
            c.text(b.x + 68, b.y + 12, &name, Size::Small, true, INK);
            let (st, col) = match (p.lang, &p.status) {
                (None, _) => (String::from("Can't be used"), BAD),
                (Some(_), Some(s)) => (s.clone(), if s == "Ready" { GOOD } else { rgb(200, 120, 20) }),
                (Some(_), None) => (String::from("Ready"), GOOD),
            };
            c.circle(b.x + 74, b.y + 44, 4, col);
            c.text(b.x + 84, b.y + 35, &st, Size::Small, false, DIM);
        }
        // Details.
        let p = &printers[self.selected.min(printers.len() - 1)];
        let x = r.x + lw + 24;
        let w = r.x + r.w - x - 24;
        c.text(x, r.y + 80, &crate::photos::ellipsize(&p.name, w, Size::Medium, true), Size::Medium, true, INK);
        let mut y = r.y + 124;
        let row = |c: &mut Canvas, y: &mut i32, label: &str, value: &str| {
            c.text(x, *y, label, Size::Small, true, DIM);
            for (k, l) in crate::docshell::wrap_lines(value, w - 150, 3).iter().enumerate() {
                c.text(x + 150, *y + k as i32 * 22, l, Size::Small, false, INK);
            }
            *y += 22 * crate::docshell::wrap_lines(value, w - 150, 3).len().max(1) as i32 + 14;
        };
        if !p.make.is_empty() {
            row(c, &mut y, "Made by", &p.make);
        }
        row(c, &mut y, "Connected", if p.id == PrinterId::Test { "Test printer (jobs go to the serial port)" } else { "By USB cable" });
        match p.lang {
            Some(l) => {
                row(c, &mut y, "Solon prints with", l.describe());
                row(c, &mut y, "Colour", if p.color { "Yes" } else { "Black and white" });
                let papers: Vec<&str> = p.papers.iter().map(|q| q.name()).collect();
                row(c, &mut y, "Paper", &papers.join(", "));
                if let Some(s) = &p.status {
                    row(c, &mut y, "Status", s);
                }
                crate::docshell::button(c, Self::test_btn(r), "Print a test page", true, ACCENT);
                c.text(x + 236, Self::test_btn(r).y + 13, "Prints a page of colours and words to check it works.", Size::Small, false, DIM);
            }
            None => {
                let msg = p.problem.unwrap_or("Solon can't use this printer.");
                let lines = crate::docshell::wrap_lines(msg, w - 40, 5);
                let bh = lines.len() as i32 * 22 + 32;
                c.round_rect(x, y, w, bh, 10, rgb(252, 236, 234));
                for (k, l) in lines.iter().enumerate() {
                    c.text(x + 18, y + 16 + k as i32 * 22, l, Size::Small, false, rgb(130, 40, 30));
                }
                y += bh + 18;
                for (k, l) in crate::docshell::wrap_lines("Tip: printers that say \"AirPrint\", \"Mopria\" or \"IPP Everywhere\" on the box, and laser printers with PCL or PostScript, work with Solon.", w, 3).iter().enumerate() {
                    c.text(x, y + k as i32 * 22, l, Size::Small, false, DIM);
                }
            }
        }
    }

    fn draw_none(&self, c: &mut Canvas, r: Rect) {
        let x = r.x + 40;
        let mut y = r.y + 90;
        printer_icon(c, x, y, false);
        c.text(x + 64, y + 10, "No printer is connected", Size::Medium, true, INK);
        y += 76;
        for (k, s) in ["1.  Plug the printer's USB cable into this computer.", "2.  Turn the printer on.", "3.  It shows up here after a few seconds."].iter().enumerate() {
            c.text(x, y + k as i32 * 30, s, Size::Small, false, INK);
        }
        y += 120;
        c.text(x, y, "Printers that work with Solon", Size::Small, true, INK);
        y += 30;
        let kinds = [
            "Driverless printers: \"AirPrint\", \"Mopria\" or \"IPP Everywhere\" (most printers made since 2016, from HP, Canon, Epson, Brother and others).",
            "HP and other printers that speak PCL (most LaserJets and many DeskJets and OfficeJets).",
            "Laser printers with PCL 6 or PostScript: Brother, Kyocera, Lexmark, Ricoh, Xerox, Samsung, Konica Minolta, Canon imageCLASS.",
        ];
        let w = r.w - 80;
        for k in kinds {
            for l in crate::docshell::wrap_lines(k, w - 20, 3) {
                c.text(x + 16, y, &l, Size::Small, false, DIM);
                y += 22;
            }
            y += 8;
        }
        let _ = text_width;
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, printers: &[PrinterView]) -> PrintersAction {
        for i in 0..printers.len() {
            if Self::card(r, i).contains(x, y) {
                self.selected = i;
                return PrintersAction::Redraw;
            }
        }
        if let Some(p) = printers.get(self.selected.min(printers.len().saturating_sub(1))) {
            if p.lang.is_some() && Self::test_btn(r).contains(x, y) {
                return PrintersAction::TestPage(p.id);
            }
        }
        PrintersAction::None
    }

    pub fn on_key(&mut self, sc: u8, n: usize) -> Option<bool> {
        match sc {
            0x48 => {
                self.selected = self.selected.saturating_sub(1);
                Some(true)
            }
            0x50 => {
                self.selected = (self.selected + 1).min(n.saturating_sub(1));
                Some(true)
            }
            0x01 => None,
            _ if sc & 0x80 != 0 => Some(false),
            _ => Some(false),
        }
    }
}

/// The test page, drawn at 100 pixels per inch for `w` x `h` (the printable area).
pub fn test_page(w: usize, h: usize, printer: &str, lang: Lang, date: &str) -> Canvas {
    use crate::font::Face;
    let mut c = Canvas::new(w, h);
    c.px.fill(0xFF_FFFF);
    let (wi, hi) = (w as i32, h as i32);
    c.text_face(40 * 64, 80, "Solon test page", Face::SansBold, 36, rgb(40, 44, 52));
    c.text_face(40 * 64, 120, printer, Face::Sans, 18, rgb(40, 44, 52));
    c.text_face(40 * 64, 146, &format!("{}  -  {}", lang.describe(), date), Face::Sans, 14, rgb(100, 106, 116));
    // Colour bars.
    let colors = [rgb(0, 0, 0), rgb(0, 174, 239), rgb(236, 0, 140), rgb(255, 242, 0), rgb(237, 28, 36), rgb(0, 166, 81), rgb(46, 49, 146)];
    let bw = (wi - 80) / colors.len() as i32;
    for (k, &col) in colors.iter().enumerate() {
        c.fill_rect(40 + k as i32 * bw, 180, bw - 6, 90, col);
    }
    // Grey steps.
    for k in 0..11 {
        let v = (k * 255 / 10) as u8;
        c.fill_rect(40 + k * (wi - 80) / 11, 290, (wi - 80) / 11, 50, rgb(v, v, v));
    }
    // A smooth gradient.
    for x in 0..(wi - 80) {
        let t = x * 255 / (wi - 80);
        let col = crate::gfx::mix(rgb(255, 120, 40), rgb(40, 90, 255), t as u32);
        c.fill_rect(40 + x, 360, 1, 50, col);
    }
    // Fine lines and circles (sharpness).
    for k in 0..20 {
        c.fill_rect(40 + k * 12, 440, 1 + k / 7, 120, 0);
    }
    for k in 0..6 {
        c.circle(wi - 140, 500, 60 - k * 10, if k % 2 == 0 { 0 } else { 0xFF_FFFF });
    }
    // Text sizes.
    let mut y = 620;
    for px in [10u16, 12, 14, 18, 24, 32] {
        c.text_face(40 * 64, y, &format!("{} px: The quick brown fox jumps.", px), Face::Serif, px, 0);
        y += px as i32 + 14;
    }
    c.text_face(40 * 64, hi - 30, "If the colours, greys and words look right, your printer is ready to use.", Face::Sans, 14, rgb(100, 106, 116));
    c
}

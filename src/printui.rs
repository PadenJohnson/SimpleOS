//! The Print dialog (inside the Photos window).

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::gfx::{rgb, Canvas, Rect, Size};
use crate::image::Image;
use crate::print::{self, Layout, Page, Paper, PrinterInfo};

const INK: u32 = rgb(40, 44, 52);
const DIM: u32 = rgb(110, 116, 126);
const ON: u32 = rgb(46, 134, 222);
const OFF: u32 = rgb(230, 233, 238);
const ROW_H: i32 = 48;
const LABEL_W: i32 = 110;

pub enum PrintAction {
    None,
    Redraw,
    Cancel,
    Print { indices: Vec<usize>, layout: Layout, printer: PrinterInfo },
}

pub struct PrintDialog {
    printers: Vec<PrinterInfo>,
    printer: usize,
    current: usize,
    total: usize,
    all: bool,
    layout: Layout,
    preview: Option<(Layout, bool, Image)>,
}

/// Buttons in one row: (label, selected, enabled).
struct Row {
    label: &'static str,
    buttons: Vec<(String, bool, bool)>,
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

impl PrintDialog {
    pub fn new(current: usize, total: usize, printers: Vec<PrinterInfo>) -> PrintDialog {
        let mut d = PrintDialog {
            printers,
            printer: 0,
            current,
            total,
            all: false,
            layout: Layout { paper: Paper::Letter, per_page: 1, fill: false, color: true, copies: 1 },
            preview: None,
        };
        d.fix_layout();
        d
    }

    fn printer_info(&self) -> Option<&PrinterInfo> {
        self.printers.get(self.printer)
    }

    /// Keep choices valid for the selected printer.
    fn fix_layout(&mut self) {
        if let Some(p) = self.printers.get(self.printer) {
            if !p.papers.contains(&self.layout.paper) {
                self.layout.paper = *p.papers.first().unwrap_or(&Paper::Letter);
            }
            if !p.color {
                self.layout.color = false;
            }
        }
        if !self.all {
            self.layout.per_page = 1;
        }
    }

    fn count(&self) -> usize {
        if self.all {
            self.total
        } else {
            1
        }
    }

    fn pages(&self) -> usize {
        (self.count() + self.layout.per_page as usize - 1) / self.layout.per_page as usize
    }

    // ---- layout ----------------------------------------------------------------

    fn dialog(r: Rect) -> Rect {
        let w = 760.min(r.w - 40);
        let h = 560.min(r.h - 30);
        Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h)
    }

    fn rows(&self) -> Vec<Row> {
        let p = self.printer_info();
        let papers = p.map(|p| p.papers.clone()).unwrap_or_default();
        let color_ok = p.map(|p| p.color).unwrap_or(false);
        let l = &self.layout;
        Vec::from([
            Row {
                label: "Photos",
                buttons: Vec::from([
                    (String::from("This photo"), !self.all, true),
                    (format!("All {} photos", self.total), self.all, self.total > 1),
                ]),
            },
            Row {
                label: "Paper",
                buttons: Paper::ALL.iter().map(|pp| (String::from(pp.name()), l.paper == *pp, papers.contains(pp))).collect(),
            },
            Row {
                label: "Per page",
                buttons: [1u32, 4, 9].iter().map(|&n| (format!("{}", n), l.per_page == n, self.all || n == 1)).collect(),
            },
            Row {
                label: "Photo",
                buttons: Vec::from([(String::from("Fit whole photo"), !l.fill, true), (String::from("Fill the space"), l.fill, true)]),
            },
            Row {
                label: "Colors",
                buttons: Vec::from([(String::from("Color"), l.color, color_ok), (String::from("Black & white"), !l.color, true)]),
            },
        ])
    }

    fn row_button(d: Rect, row: usize, i: usize, n: usize) -> Rect {
        let x0 = d.x + 24 + LABEL_W;
        let avail = d.w - 24 - LABEL_W - 300;
        let w = ((avail - (n as i32 - 1) * 8) / n as i32).min(150);
        Rect::new(x0 + i as i32 * (w + 8), d.y + 104 + row as i32 * ROW_H, w, 36)
    }

    fn copies_btn(d: Rect, plus: bool) -> Rect {
        let y = d.y + 104 + 5 * ROW_H;
        Rect::new(d.x + 24 + LABEL_W + if plus { 104 } else { 0 }, y, 40, 36)
    }

    fn printer_btn(d: Rect) -> Rect {
        Rect::new(d.x + 24 + LABEL_W, d.y + 56, d.w - 24 - LABEL_W - 300, 36)
    }

    fn preview_area(d: Rect) -> Rect {
        Rect::new(d.x + d.w - 280, d.y + 56, 256, d.h - 56 - 80)
    }

    fn print_btn(d: Rect) -> Rect {
        Rect::new(d.x + d.w - 24 - 140, d.y + d.h - 60, 140, 40)
    }
    fn cancel_btn(d: Rect) -> Rect {
        Rect::new(d.x + d.w - 24 - 140 - 12 - 110, d.y + d.h - 60, 110, 40)
    }

    // ---- drawing ----------------------------------------------------------------

    /// `thumb(i)` gives photo i's thumbnail, if loaded.
    pub fn draw(&mut self, c: &mut Canvas, r: Rect, thumb: &dyn Fn(usize) -> Option<Image>) {
        c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 130);
        let d = Self::dialog(r);
        c.round_rect_alpha(d.x + 3, d.y + 6, d.w, d.h, 12, 0, 90);
        c.round_rect(d.x, d.y, d.w, d.h, 12, rgb(255, 255, 255));
        c.text(d.x + 24, d.y + 18, "Print", Size::Medium, true, INK);

        c.text(d.x + 24, d.y + 64, "Printer", Size::Small, true, DIM);
        let pb = Self::printer_btn(d);
        match self.printer_info() {
            Some(p) => {
                let label = if self.printers.len() > 1 { format!("{}  (click to change)", p.name) } else { p.name.clone() };
                let label = crate::photos::ellipsize(&label, pb.w - 16, Size::Small, true);
                button(c, pb, &label, false, true);
            }
            None => {
                c.text(pb.x, pb.y + 2, "No printer connected.", Size::Small, true, rgb(180, 60, 50));
                c.text(pb.x, pb.y + 20, "Plug a USB printer into this computer.", Size::Small, false, DIM);
            }
        }

        for (ri, row) in self.rows().iter().enumerate() {
            c.text(d.x + 24, d.y + 104 + ri as i32 * ROW_H + 9, row.label, Size::Small, true, DIM);
            let n = row.buttons.len();
            for (i, (label, on, enabled)) in row.buttons.iter().enumerate() {
                button(c, Self::row_button(d, ri, i, n), label, *on, *enabled);
            }
        }
        let y = d.y + 104 + 5 * ROW_H;
        c.text(d.x + 24, y + 9, "Copies", Size::Small, true, DIM);
        button(c, Self::copies_btn(d, false), "-", false, self.layout.copies > 1);
        c.text_centered(d.x + 24 + LABEL_W + 72, y + 8, &format!("{}", self.layout.copies), Size::Medium, true, INK);
        button(c, Self::copies_btn(d, true), "+", false, self.layout.copies < 99);

        let pages = self.pages();
        let summary = format!(
            "{} page{}{}",
            pages,
            if pages == 1 { "" } else { "s" },
            if self.layout.copies > 1 { format!(", {} copies each", self.layout.copies) } else { String::new() }
        );
        c.text(d.x + 24, d.y + d.h - 50, &summary, Size::Small, false, DIM);

        // Preview of the first page.
        let pa = Self::preview_area(d);
        let stale = !matches!(&self.preview, Some((l, a, _)) if *l == self.layout && *a == self.all);
        if stale {
            let dpi = 40;
            let (pw, ph) = print::page_pixels(self.layout.paper, dpi);
            let mut page = Page::new(pw, ph);
            let cells = page.cells(self.layout.per_page, dpi);
            let first: Vec<usize> = if self.all { (0..self.total).take(cells.len()).collect() } else { Vec::from([self.current]) };
            for (i, cell) in first.iter().zip(cells.iter()) {
                if let Some(t) = thumb(*i) {
                    let mut t = t;
                    if !self.layout.color {
                        crate::edit::color(&mut t, &crate::edit::Edits { bw: true, ..Default::default() });
                    }
                    page.place(&t, *cell, self.layout.fill);
                } else {
                    let (x, y, w, h) = *cell;
                    for yy in y..y + h {
                        page.px[yy * pw + x..yy * pw + x + w].fill(0xDDDDDD);
                    }
                }
            }
            let img = Image { w: pw, h: ph, px: page.px.iter().map(|p| p | 0xFF00_0000).collect(), opaque: true };
            self.preview = Some((self.layout, self.all, img));
        }
        if let Some((_, _, img)) = &self.preview {
            // Sheet = printable area + margins, scaled to fit the preview box.
            let (sw_mils, sh_mils) = self.layout.paper.mils();
            let scale_num = (pa.w as i64 * 1000).min(pa.h as i64 * 1000 * sw_mils as i64 / sh_mils as i64);
            let sheet_w = (scale_num / 1000) as i32;
            let sheet_h = (sheet_w as i64 * sh_mils as i64 / sw_mils as i64) as i32;
            let sx = pa.x + (pa.w - sheet_w) / 2;
            let sy = pa.y + (pa.h - sheet_h) / 2;
            c.fill_rect_alpha(sx + 3, sy + 4, sheet_w, sheet_h, 0, 50);
            c.fill_rect(sx, sy, sheet_w, sheet_h, rgb(255, 255, 255));
            c.outline_rect(sx, sy, sheet_w, sheet_h, rgb(210, 214, 220));
            let shown = img.shrink_to_fit((sheet_w * 94 / 100).max(1) as usize, (sheet_h * 94 / 100).max(1) as usize);
            c.draw_image(&shown, sx + (sheet_w - shown.w as i32) / 2, sy + (sheet_h - shown.h as i32) / 2);
        }

        button(c, Self::cancel_btn(d), "Cancel", false, true);
        button(c, Self::print_btn(d), "Print", self.printer_info().is_some(), self.printer_info().is_some());
    }

    // ---- input ----------------------------------------------------------------

    fn print_action(&self) -> PrintAction {
        let Some(p) = self.printer_info() else { return PrintAction::None };
        let indices = if self.all { (0..self.total).collect() } else { Vec::from([self.current]) };
        PrintAction::Print { indices, layout: self.layout, printer: p.clone() }
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect) -> PrintAction {
        let d = Self::dialog(r);
        if Self::cancel_btn(d).contains(x, y) {
            return PrintAction::Cancel;
        }
        if Self::print_btn(d).contains(x, y) {
            return self.print_action();
        }
        if Self::printer_btn(d).contains(x, y) && self.printers.len() > 1 {
            self.printer = (self.printer + 1) % self.printers.len();
            self.fix_layout();
            return PrintAction::Redraw;
        }
        if Self::copies_btn(d, false).contains(x, y) {
            self.layout.copies = self.layout.copies.saturating_sub(1).max(1);
            return PrintAction::Redraw;
        }
        if Self::copies_btn(d, true).contains(x, y) {
            self.layout.copies = (self.layout.copies + 1).min(99);
            return PrintAction::Redraw;
        }
        let rows = self.rows();
        for (ri, row) in rows.iter().enumerate() {
            let n = row.buttons.len();
            for i in 0..n {
                if !Self::row_button(d, ri, i, n).contains(x, y) || !row.buttons[i].2 {
                    continue;
                }
                match ri {
                    0 => self.all = i == 1,
                    1 => self.layout.paper = Paper::ALL[i],
                    2 => self.layout.per_page = [1, 4, 9][i],
                    3 => self.layout.fill = i == 1,
                    _ => self.layout.color = i == 0,
                }
                self.fix_layout();
                return PrintAction::Redraw;
            }
        }
        PrintAction::None
    }

    pub fn on_key(&mut self, sc: u8) -> PrintAction {
        match sc {
            0x01 => PrintAction::Cancel,
            0x1C | 0x19 => self.print_action(), // Enter / P
            0x1E => {
                // A: all photos
                if self.total > 1 {
                    self.all = !self.all;
                    self.fix_layout();
                }
                PrintAction::Redraw
            }
            0x0D | 0x4E => {
                self.layout.copies = (self.layout.copies + 1).min(99); // + / keypad +
                PrintAction::Redraw
            }
            0x0C | 0x4A => {
                self.layout.copies = self.layout.copies.saturating_sub(1).max(1);
                PrintAction::Redraw
            }
            _ => PrintAction::None,
        }
    }
}

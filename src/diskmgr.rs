//! Disks app: what's inside the computer and plugged into it, how each disk is divided
//! up, setting up Solon storage, checking its health, and erasing USB drives.
//! Internal disks are never erased; Solon's own USB stick is never erased.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::disk::human;
use crate::gfx::{rgb, text_width, Canvas, Rect, Size};
use crate::library::{DiskView, PartView, Storage};
use crate::usb::Usb;

const INK: u32 = rgb(40, 44, 52);
const DIM: u32 = rgb(110, 116, 126);
const ACCENT: u32 = rgb(46, 134, 222);
const DANGER: u32 = rgb(210, 70, 60);
const SOLON: u32 = rgb(39, 174, 96);
const FREE: u32 = rgb(226, 229, 234);
const PART_COLORS: [u32; 4] = [rgb(52, 152, 219), rgb(155, 89, 182), rgb(230, 126, 34), rgb(26, 188, 156)];

const PAD: i32 = 16;
const HEAD_H: i32 = 64;
const BAR_H: i32 = 30;
const LINE_H: i32 = 28;

pub enum DiskAction {
    None,
    Redraw,
    /// Storage or drives changed; show this notice.
    Changed(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Op {
    Setup(usize),
    Check(bool),
    Erase(u32),
}

enum Dialog {
    None,
    /// Create Solon storage on internal disk i.
    Setup { disk: usize, name: String, bytes: u64 },
    Report { lines: Vec<String>, can_repair: bool },
    Erase1 { drive: u32, name: String, bytes: u64 },
    Erase2 { drive: u32, name: String, bytes: u64 },
    Busy(String),
    Message(String),
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Btn {
    Setup,
    Check,
    Erase,
}

pub struct DiskMgr {
    views: Vec<DiskView>,
    scroll: i32,
    dialog: Dialog,
    pending: Option<Op>,
}

fn card_h(v: &DiskView) -> i32 {
    HEAD_H + BAR_H + 14 + LINE_H * v.parts.len().max(1) as i32 + PAD
}

fn part_name(p: &PartView, i: usize) -> String {
    if p.active {
        String::from("Solon storage")
    } else if p.solon {
        String::from("Solon storage (not in use)")
    } else if !p.label.is_empty() {
        p.label.clone()
    } else {
        format!("Part {}", i + 1)
    }
}

fn part_color(p: &PartView, i: usize) -> u32 {
    if p.solon {
        SOLON
    } else {
        PART_COLORS[i % PART_COLORS.len()]
    }
}

/// USB drives as disk views (their volumes stand in for partitions).
fn usb_views(storage: &Storage, usb: &mut Usb) -> Vec<DiskView> {
    let lib = storage.on_boot_stick();
    let lib_free = storage.free_bytes();
    let ids: Vec<u32> = usb.drives.iter().map(|d| d.id).collect();
    let mut out = Vec::new();
    for id in ids {
        let (name, boot, bs, blocks, vols): (String, bool, usize, u64, Vec<(u64, u64, String, String)>) = {
            let d = usb.drives.iter().find(|d| d.id == id).unwrap();
            let bs = d.ms.block_size as usize;
            let vols = d.volumes.iter().map(|v| (v.start(), v.size / bs as u64, String::from(v.kind.name()), v.label.clone())).collect();
            (d.name.clone(), d.is_boot, bs, d.ms.blocks, vols)
        };
        let mut parts = Vec::new();
        for (i, (start, len, format, label)) in vols.into_iter().enumerate() {
            let solon = matches!(lib, Some((lid, ls)) if lid == id && ls == start);
            let free = if solon { Some(lib_free) } else { usb.volume_free(id, i) };
            parts.push(PartView { start, blocks: len, format, label, free, solon, active: solon });
        }
        // Solon storage made on the stick after it was first scanned.
        if let Some((lid, ls)) = lib {
            if lid == id && !parts.iter().any(|p| p.solon) {
                let blocks = storage.size() / bs as u64;
                parts.push(PartView { start: ls, blocks, format: String::from("exFAT"), label: String::new(), free: Some(lib_free), solon: true, active: true });
                parts.sort_by_key(|p| p.start);
            }
        }
        out.push(DiskView { internal: None, usb: Some(id), name, kind: "USB", bytes: blocks * bs as u64, blocks, block: bs, parts, can_setup: None, boot });
    }
    out
}

impl DiskMgr {
    pub fn new() -> DiskMgr {
        DiskMgr { views: Vec::new(), scroll: 0, dialog: Dialog::None, pending: None }
    }

    /// Re-read every disk (opening the window, plug/unplug, after changes).
    pub fn refresh(&mut self, storage: &mut Storage, usb: &mut Usb) {
        let mut v = storage.disk_views();
        v.extend(usb_views(storage, usb));
        self.views = v;
    }

    pub fn busy(&self) -> bool {
        self.pending.is_some()
    }

    fn holds_library(v: &DiskView) -> bool {
        v.parts.iter().any(|p| p.active)
    }

    /// Buttons on a disk's card, right to left.
    fn buttons(v: &DiskView, has_library: bool) -> Vec<(Btn, &'static str)> {
        let mut b = Vec::new();
        if has_library && Self::holds_library(v) {
            b.push((Btn::Check, "Check health"));
        }
        if !has_library && v.internal.is_some() && v.can_setup.is_some() {
            b.push((Btn::Setup, "Set up Solon storage"));
        }
        if v.usb.is_some() && !v.boot {
            b.push((Btn::Erase, "Erase and format..."));
        }
        b
    }

    fn card(&self, r: Rect, i: usize) -> Rect {
        let mut y = r.y + PAD - self.scroll;
        for v in &self.views[..i] {
            y += card_h(v) + 12;
        }
        Rect::new(r.x + PAD, y, r.w - 2 * PAD, card_h(&self.views[i]))
    }

    fn btn_rects(card: Rect, btns: &[(Btn, &'static str)]) -> Vec<Rect> {
        let mut out = Vec::new();
        let mut x = card.x + card.w - PAD;
        for (_, label) in btns {
            let w = text_width(label, Size::Small, true) + 32;
            x -= w;
            out.push(Rect::new(x, card.y + 14, w, 36));
            x -= 10;
        }
        out
    }

    // ---- drawing ----------------------------------------------------------------

    pub fn draw(&self, c: &mut Canvas, r: Rect, storage: &Storage) {
        c.fill_rect(r.x, r.y, r.w, r.h, rgb(240, 242, 246));
        if self.views.is_empty() {
            c.text_centered(r.x + r.w / 2, r.y + 80, "No disks found.", Size::Medium, true, INK);
        }
        let has_lib = storage.library.is_some();
        for (i, v) in self.views.iter().enumerate() {
            let k = self.card(r, i);
            if k.y > r.y + r.h || k.y + k.h < r.y {
                continue;
            }
            c.round_rect(k.x, k.y, k.w, k.h, 12, rgb(255, 255, 255));
            // Heading
            let kind = match (v.usb, v.boot) {
                (Some(_), true) => String::from("Solon's own USB stick (the computer started from it)"),
                (Some(_), false) => String::from("USB drive"),
                (None, _) => format!("Inside this computer ({})", v.kind),
            };
            let btns = Self::buttons(v, has_lib);
            let rects = Self::btn_rects(k, &btns);
            let text_right = rects.last().map(|b| b.x - 12).unwrap_or(k.x + k.w - PAD);
            let title = format!("{}  -  {}", v.name, human(v.bytes));
            let title = crate::photos::ellipsize(&title, text_right - k.x - PAD, Size::Medium, true);
            c.text(k.x + PAD, k.y + 12, &title, Size::Medium, true, INK);
            let kind = crate::photos::ellipsize(&kind, text_right - k.x - PAD, Size::Small, false);
            c.text(k.x + PAD, k.y + 38, &kind, Size::Small, false, DIM);
            for ((b, label), br) in btns.iter().zip(rects.iter()) {
                let (bg, fg) = match b {
                    Btn::Setup => (ACCENT, rgb(255, 255, 255)),
                    Btn::Check => (rgb(230, 233, 238), INK),
                    Btn::Erase => (rgb(252, 232, 230), DANGER),
                };
                c.round_rect(br.x, br.y, br.w, br.h, 8, bg);
                c.text_centered(br.x + br.w / 2, br.y + 9, label, Size::Small, true, fg);
            }

            // Bar: partitions in place, free space grey.
            let bar = Rect::new(k.x + PAD, k.y + HEAD_H, k.w - 2 * PAD, BAR_H);
            c.round_rect(bar.x, bar.y, bar.w, bar.h, 6, FREE);
            let total = v.blocks.max(1);
            for (j, p) in v.parts.iter().enumerate() {
                let x0 = bar.x + ((p.start as u128 * bar.w as u128) / total as u128) as i32;
                let x1 = bar.x + (((p.start + p.blocks) as u128 * bar.w as u128) / total as u128) as i32;
                let w = (x1 - x0).max(4).min(bar.x + bar.w - x0);
                c.fill_rect(x0, bar.y, w, bar.h, part_color(p, j));
                if let Some(f) = p.free {
                    // Lighter part = free space inside the partition.
                    let size = p.blocks * v.block as u64;
                    if size > 0 && w > 8 {
                        let fw = ((f.min(size) as u128 * (w - 2) as u128) / size as u128) as i32;
                        c.fill_rect_alpha(x0 + w - 1 - fw, bar.y + 3, fw, bar.h - 6, rgb(255, 255, 255), 120);
                    }
                }
            }

            // Legend
            let mut y = bar.y + BAR_H + 12;
            if v.parts.is_empty() {
                let msg = if v.can_setup.is_some() { "Empty - nothing stored on this disk yet." } else { "Nothing Solon can read on this disk." };
                c.text(k.x + PAD, y + 4, msg, Size::Small, false, DIM);
            }
            for (j, p) in v.parts.iter().enumerate() {
                c.round_rect(k.x + PAD, y + 5, 14, 14, 3, part_color(p, j));
                let name = part_name(p, j);
                c.text(k.x + PAD + 24, y + 3, &name, Size::Small, true, INK);
                let fmt = if p.format == "Unknown" { String::from("Can't be read by Solon") } else { p.format.clone() };
                let mut info = format!("{}   {}", fmt, human(p.blocks * v.block as u64));
                if let Some(f) = p.free {
                    info += &format!("   {} free", human(f));
                }
                let x = k.x + PAD + 24 + (text_width(&name, Size::Small, true) + 16).max(170);
                c.text(x, y + 3, &info, Size::Small, false, DIM);
                y += LINE_H;
            }
        }
        self.draw_dialog(c, r);
    }

    fn dialog_rect(r: Rect, lines: usize) -> Rect {
        let w = 600.min(r.w - 40);
        let h = (150 + lines as i32 * 24).min(r.h - 40);
        Rect::new(r.x + (r.w - w) / 2, r.y + (r.h - h) / 2, w, h)
    }

    /// (title, body lines, buttons: (label, danger)) of the open dialog.
    fn dialog_parts(&self) -> Option<(String, Vec<String>, Vec<(&'static str, bool)>)> {
        Some(match &self.dialog {
            Dialog::None => return None,
            Dialog::Setup { name, bytes, .. } => (
                String::from("Set up Solon storage?"),
                Vec::from([
                    format!("Solon will use {} of empty space on {}", human(*bytes), name),
                    String::from("for your photos. Nothing already on the disk is changed."),
                ]),
                Vec::from([("Set up", false), ("Cancel", false)]),
            ),
            Dialog::Report { lines, can_repair } => {
                let mut b = Vec::new();
                if *can_repair {
                    b.push(("Repair", false));
                }
                b.push(("Close", false));
                (String::from("Storage health"), lines.clone(), b)
            }
            Dialog::Erase1 { name, bytes, .. } => (
                format!("Erase {}?", name),
                Vec::from([
                    format!("Everything on this {} USB drive will be deleted,", human(*bytes)),
                    String::from("and it will be set up empty, ready to use."),
                    String::from("Copy anything you want to keep first."),
                ]),
                Vec::from([("Continue", true), ("Cancel", false)]),
            ),
            Dialog::Erase2 { name, bytes, .. } => (
                String::from("Are you completely sure?"),
                Vec::from([format!("All files on {} ({}) will be gone forever.", name, human(*bytes)), String::from("This can't be undone.")]),
                Vec::from([("Erase now", true), ("Cancel", false)]),
            ),
            Dialog::Busy(msg) => (String::from("Please wait"), Vec::from([msg.clone(), String::from("Don't unplug anything.")]), Vec::new()),
            Dialog::Message(msg) => (String::from("Disks"), Vec::from([msg.clone()]), Vec::from([("OK", false)])),
        })
    }

    fn dialog_buttons(d: Rect, btns: &[(&'static str, bool)]) -> Vec<Rect> {
        let mut x = d.x + d.w - 24;
        let mut out = Vec::new();
        for (label, _) in btns {
            let w = (text_width(label, Size::Small, true) + 40).max(110);
            x -= w;
            out.push(Rect::new(x, d.y + d.h - 58, w, 38));
            x -= 12;
        }
        out
    }

    fn draw_dialog(&self, c: &mut Canvas, r: Rect) {
        let Some((title, lines, btns)) = self.dialog_parts() else { return };
        c.fill_rect_alpha(r.x, r.y, r.w, r.h, 0, 110);
        let d = Self::dialog_rect(r, lines.len());
        c.round_rect_alpha(d.x + 3, d.y + 6, d.w, d.h, 12, 0, 90);
        c.round_rect(d.x, d.y, d.w, d.h, 12, rgb(255, 255, 255));
        c.text(d.x + 24, d.y + 20, &title, Size::Medium, true, INK);
        for (i, l) in lines.iter().enumerate() {
            let l = crate::photos::ellipsize(l, d.w - 48, Size::Small, false);
            c.text(d.x + 24, d.y + 62 + i as i32 * 24, &l, Size::Small, false, DIM);
        }
        for (k, ((label, danger), b)) in btns.iter().zip(Self::dialog_buttons(d, &btns)).enumerate() {
            let (bg, fg) = match (k, danger) {
                (_, true) => (DANGER, rgb(255, 255, 255)),
                (0, false) => (ACCENT, rgb(255, 255, 255)),
                _ => (rgb(230, 233, 238), INK),
            };
            c.round_rect(b.x, b.y, b.w, b.h, 8, bg);
            c.text_centered(b.x + b.w / 2, b.y + 10, label, Size::Small, true, fg);
        }
    }

    // ---- input ------------------------------------------------------------------

    /// The dialog's button `k` (0 = the main one, drawn rightmost) was chosen.
    fn dialog_choice(&mut self, k: usize) -> DiskAction {
        let dialog = core::mem::replace(&mut self.dialog, Dialog::None);
        match (dialog, k) {
            (Dialog::Setup { disk, .. }, 0) => self.start(Op::Setup(disk), "Setting up Solon storage..."),
            (Dialog::Report { can_repair: true, .. }, 0) => self.start(Op::Check(true), "Repairing Solon storage..."),
            (Dialog::Erase1 { drive, name, bytes }, 0) => {
                self.dialog = Dialog::Erase2 { drive, name, bytes };
                DiskAction::Redraw
            }
            (Dialog::Erase2 { drive, name, .. }, 0) => self.start(Op::Erase(drive), &format!("Erasing {}...", name)),
            (Dialog::Busy(m), _) => {
                self.dialog = Dialog::Busy(m);
                DiskAction::None
            }
            _ => DiskAction::Redraw,
        }
    }

    fn start(&mut self, op: Op, msg: &str) -> DiskAction {
        self.pending = Some(op);
        self.dialog = Dialog::Busy(String::from(msg));
        DiskAction::Redraw
    }

    /// Run the pending operation (called from the desktop's tick, after "Please wait" is shown).
    pub fn step(&mut self, storage: &mut Storage, usb: &mut Usb) -> Option<DiskAction> {
        let op = self.pending.take()?;
        let action = match op {
            Op::Setup(disk) => match storage.create_library(disk) {
                Ok(()) => {
                    kprintln!("disks: Solon storage created on disk {}", disk);
                    self.dialog = Dialog::Message(String::from("Solon storage is ready."));
                    DiskAction::Changed(String::from("Solon storage is ready"))
                }
                Err(e) => {
                    kprintln!("disks: setup failed: {:?}", e);
                    self.dialog = Dialog::Message(format!("Setup failed: {}", e.message()));
                    DiskAction::Redraw
                }
            },
            Op::Check(repair) => match storage.check(usb, repair) {
                Ok(rep) => {
                    kprintln!(
                        "disks: health check{}: {} files, {} folders, {} leaked bytes, {} unmarked, {} problems",
                        if repair { " + repair" } else { "" },
                        rep.files,
                        rep.folders,
                        rep.leaked_bytes,
                        rep.unmarked,
                        rep.problems.len()
                    );
                    let mut lines = Vec::from([format!(
                        "Checked {} file{} in {} folder{}.",
                        rep.files,
                        if rep.files == 1 { "" } else { "s" },
                        rep.folders,
                        if rep.folders == 1 { "" } else { "s" }
                    )]);
                    let fixable = rep.leaked_bytes > 0 || rep.unmarked > 0;
                    if rep.repaired && fixable {
                        lines.push(String::from("Repaired. Your storage is healthy again."));
                    } else if rep.healthy() {
                        lines.push(String::from("Everything looks good."));
                    }
                    if !rep.repaired {
                        if rep.leaked_bytes > 0 {
                            lines.push(format!("{} is marked as used but no file uses it.", human(rep.leaked_bytes)));
                        }
                        if rep.unmarked > 0 {
                            lines.push(format!("{} piece(s) of files aren't marked as used.", rep.unmarked));
                        }
                        if fixable {
                            lines.push(String::from("Repair fixes this without touching your photos."));
                        }
                    }
                    for p in rep.problems.iter().take(4) {
                        lines.push(p.clone());
                    }
                    self.dialog = Dialog::Report { lines, can_repair: fixable && !rep.repaired };
                    if repair {
                        DiskAction::Changed(String::from("Storage repaired"))
                    } else {
                        DiskAction::Redraw
                    }
                }
                Err(e) => {
                    self.dialog = Dialog::Message(format!("The check couldn't finish: {}", e.message()));
                    DiskAction::Redraw
                }
            },
            Op::Erase(drive) => match usb.format_drive(drive, "USB DRIVE") {
                Ok(()) => {
                    self.dialog = Dialog::Message(String::from("The USB drive is erased and ready to use."));
                    DiskAction::Changed(String::from("USB drive erased"))
                }
                Err(e) => {
                    kprintln!("disks: erase failed: {:?}", e);
                    self.dialog = Dialog::Message(format!("Erasing failed: {}", e.message()));
                    DiskAction::Redraw
                }
            },
        };
        self.refresh(storage, usb);
        Some(action)
    }

    pub fn on_press(&mut self, x: i32, y: i32, r: Rect, storage: &Storage) -> DiskAction {
        if let Some((_, lines, btns)) = self.dialog_parts() {
            let d = Self::dialog_rect(r, lines.len());
            for (k, b) in Self::dialog_buttons(d, &btns).iter().enumerate() {
                if b.contains(x, y) {
                    return self.dialog_choice(k);
                }
            }
            return DiskAction::None;
        }
        let has_lib = storage.library.is_some();
        for i in 0..self.views.len() {
            let k = self.card(r, i);
            let v = &self.views[i];
            let btns = Self::buttons(v, has_lib);
            for ((b, _), br) in btns.iter().zip(Self::btn_rects(k, &btns)) {
                if !br.contains(x, y) {
                    continue;
                }
                self.dialog = match b {
                    Btn::Setup => Dialog::Setup { disk: v.internal.unwrap(), name: v.name.clone(), bytes: v.can_setup.unwrap_or(0) },
                    Btn::Check => {
                        return self.start(Op::Check(false), "Checking Solon storage...");
                    }
                    Btn::Erase => Dialog::Erase1 { drive: v.usb.unwrap(), name: v.name.clone(), bytes: v.bytes },
                };
                return DiskAction::Redraw;
            }
        }
        DiskAction::None
    }

    pub fn on_wheel(&mut self, delta: i8, r: Rect) -> bool {
        let total: i32 = self.views.iter().map(|v| card_h(v) + 12).sum::<i32>() + 2 * PAD;
        let max = (total - r.h).max(0);
        let old = self.scroll;
        self.scroll = (self.scroll + delta as i32 * 60).clamp(0, max);
        old != self.scroll
    }

    /// None = not handled (Esc closes the window when no dialog is open).
    pub fn on_key(&mut self, sc: u8) -> Option<DiskAction> {
        if sc & 0x80 != 0 {
            return Some(DiskAction::None);
        }
        match (&self.dialog, sc) {
            (Dialog::None, _) => None,
            (Dialog::Busy(_), _) => Some(DiskAction::None),
            // Erasing needs a click, never just a key.
            (Dialog::Erase1 { .. } | Dialog::Erase2 { .. }, 0x1C) => Some(DiskAction::None),
            (_, 0x1C) => Some(self.dialog_choice(0)),
            (_, 0x01) => {
                self.dialog = Dialog::None;
                Some(DiskAction::Redraw)
            }
            _ => Some(DiskAction::None),
        }
    }
}

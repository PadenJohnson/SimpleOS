//! The screen: the resolutions the PC's firmware offers, what the monitor says about itself
//! (its name, best resolution and refresh rate, from EDID), and choosing a resolution by
//! writing it into the boot loader's settings (limine.conf) for the next start.
//!
//! A change is tried first: limine.conf gets a "# solon-trial" note, and after the restart
//! Solon asks "Keep this resolution?". If nobody answers, the old one goes back.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::fs::{self, part};
use crate::library::Storage;
use crate::usb::Usb;

#[derive(Clone, Default)]
pub struct DisplayInfo {
    /// Resolutions the firmware offers (32-bit colour), biggest first.
    pub modes: Vec<(u32, u32)>,
    pub current: (u32, u32),
    /// Refresh rate in hundredths of a hertz, from the monitor's preferred timing.
    pub refresh: Option<u32>,
    pub monitor: Option<String>,
    /// The monitor's own (best) resolution.
    pub native: Option<(u32, u32)>,
}

pub static INFO: Mutex<Option<DisplayInfo>> = Mutex::new(None);

pub fn init(modes: &[(u32, u32, u16)], current: (u32, u32), edid: Option<&[u8]>) {
    let mut m: Vec<(u32, u32)> = modes.iter().filter(|x| x.2 == 32 && x.0 >= 640 && x.1 >= 480).map(|x| (x.0, x.1)).collect();
    m.push(current);
    m.sort_by(|a, b| (b.0 * b.1, b.0).cmp(&(a.0 * a.1, a.0)));
    m.dedup();
    let mut info = DisplayInfo { modes: m, current, ..DisplayInfo::default() };
    if let Some(e) = edid {
        parse_edid(e, &mut info);
    }
    kprintln!(
        "display: {}x{}, {} modes, monitor {:?} best {:?} at {}",
        current.0,
        current.1,
        info.modes.len(),
        info.monitor,
        info.native,
        info.refresh.map(|r| format!("{}.{:02} Hz", r / 100, r % 100)).unwrap_or_else(|| String::from("? Hz"))
    );
    *INFO.lock() = Some(info);
}

fn parse_edid(e: &[u8], info: &mut DisplayInfo) {
    if e.len() < 128 || e[0..8] != [0, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0xFF, 0] {
        return;
    }
    for k in 0..4 {
        let d = &e[54 + k * 18..54 + k * 18 + 18];
        let clock = u16::from_le_bytes([d[0], d[1]]) as u64 * 10_000; // Hz
        if clock != 0 {
            if k == 0 {
                let ha = d[2] as u64 | ((d[4] as u64 >> 4) << 8);
                let hb = d[3] as u64 | ((d[4] as u64 & 15) << 8);
                let va = d[5] as u64 | ((d[7] as u64 >> 4) << 8);
                let vb = d[6] as u64 | ((d[7] as u64 & 15) << 8);
                let total = (ha + hb) * (va + vb);
                if total > 0 {
                    info.refresh = Some((clock * 100 / total) as u32);
                    info.native = Some((ha as u32, va as u32));
                }
            }
        } else if d[3] == 0xFC {
            let name: String = d[5..18].iter().take_while(|&&b| b != 0x0A).map(|&b| b as char).collect();
            let name = String::from(name.trim());
            if !name.is_empty() {
                info.monitor = Some(name);
            }
        }
    }
}

/// Work on the boot partition Solon started from (internal disk or its USB stick).
fn with_boot_esp<R>(storage: &mut Storage, usb: &mut Usb, f: impl FnOnce(&mut fs::Volume, &mut dyn fs::BlockDev) -> Result<R, String>) -> Result<R, String> {
    let id = usb.boot_disk_id().ok_or("Solon can't tell which disk it started from")?;
    if let Some(i) = storage.boot_internal(id) {
        let total = storage.disk_info(i).map(|d| d.1).unwrap_or(0);
        return storage.with_disk(i, |dev| on_esp(dev, total, f)).unwrap_or_else(|| Err(String::from("disk not found")));
    }
    let drive = usb.boot_drive().ok_or("the Solon USB stick isn't reachable")?;
    let (mut dev, total) = usb.boot_disk(drive).ok_or("the Solon USB stick isn't reachable")?;
    on_esp(&mut dev, total, f)
}

fn on_esp<R>(dev: &mut dyn fs::BlockDev, total: u64, f: impl FnOnce(&mut fs::Volume, &mut dyn fs::BlockDev) -> Result<R, String>) -> Result<R, String> {
    let g = part::read_gpt(dev, total).ok_or("the start-up disk's partition table can't be read")?;
    let (_, first, last) = part::esp(&g).ok_or("the start-up disk has no boot partition")?;
    let mut vol = fs::mount(dev, first, last - first + 1).ok_or("the boot partition can't be read")?;
    f(&mut vol, dev)
}

/// The boot loader settings, and the folder they're in ("boot" or the root).
pub fn read_config(storage: &mut Storage, usb: &mut Usb) -> Result<String, String> {
    with_boot_esp(storage, usb, |vol, dev| {
        let root = vol.root();
        let dir = match vol.find(dev, &root, "boot").map_err(|e| String::from(e.message()))? {
            Some(d) if d.is_dir => d,
            _ => root.clone(),
        };
        let file = vol.find(dev, &dir, "limine.conf").map_err(|e| String::from(e.message()))?.ok_or("limine.conf is missing")?;
        let data = vol.read_file(dev, &file).map_err(|e| String::from(e.message()))?;
        Ok(String::from_utf8_lossy(&data).into_owned())
    })
}

/// Replace the boot loader settings safely: write a new file, check it, then switch it in.
pub fn write_config(storage: &mut Storage, usb: &mut Usb, text: &str) -> Result<(), String> {
    let data = text.as_bytes();
    with_boot_esp(storage, usb, |vol, dev| {
        vol.enable_writes(dev).map_err(|e| String::from(e.message()))?;
        let root = vol.root();
        let dir = match vol.find(dev, &root, "boot").map_err(|e| String::from(e.message()))? {
            Some(d) if d.is_dir => d,
            _ => root.clone(),
        };
        let _ = vol.fat_delete(dev, &dir, "limine.new");
        let staged = vol.create_file(dev, &dir, "limine.new", data).map_err(|e| format!("writing the new settings: {}", e.message()))?;
        if vol.read_file(dev, &staged).map_err(|e| String::from(e.message()))? != data {
            let _ = vol.fat_delete(dev, &dir, "limine.new");
            return Err(String::from("the new settings didn't read back correctly"));
        }
        vol.fat_swap_in(dev, &dir, "limine.conf", "limine.new").map_err(|e| format!("switching the settings: {}", e.message()))?;
        Ok(())
    })
}

/// The resolution limine.conf asks for (None = the firmware's choice).
pub fn configured(text: &str) -> Option<(u32, u32)> {
    text.lines().find_map(|l| l.trim().strip_prefix("resolution:")).and_then(parse_res)
}

fn parse_res(s: &str) -> Option<(u32, u32)> {
    let mut it = s.trim().split('x');
    Some((it.next()?.trim().parse().ok()?, it.next()?.trim().parse().ok()?))
}

/// A resolution being tried: the one to go back to (None = the firmware's choice).
pub fn trial(text: &str) -> Option<Option<(u32, u32)>> {
    let p = text.lines().find_map(|l| l.trim().strip_prefix("# solon-trial previous="))?;
    Some(parse_res(p))
}

/// limine.conf with a new resolution (None = let the firmware choose), optionally marked as
/// a trial that can go back to `previous`.
pub fn with_resolution(text: &str, res: Option<(u32, u32)>, trial_previous: Option<Option<(u32, u32)>>) -> String {
    let mut out = String::new();
    for l in text.lines() {
        let t = l.trim();
        if t.starts_with("resolution:") || t.starts_with("# solon-trial") {
            continue;
        }
        out.push_str(l);
        out.push('\n');
    }
    while out.ends_with("\n\n") {
        out.pop();
    }
    if let Some((w, h)) = res {
        out.push_str(&format!("    resolution: {}x{}\n", w, h));
    }
    if let Some(prev) = trial_previous {
        match prev {
            Some((w, h)) => out.push_str(&format!("# solon-trial previous={}x{}\n", w, h)),
            None => out.push_str("# solon-trial previous=default\n"),
        }
    }
    out
}

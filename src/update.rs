//! Updating an installed Solon from a USB drive: a newly written Solon stick, or any USB
//! drive with the Solon files (the `boot` folder from `dist\solon-usb`) copied onto it.
//!
//! Only the kernel (`boot/kernel` in the boot partition) is replaced. Photos, documents and
//! settings live in Solon storage and aren't touched. The new kernel is written next to the
//! old one, read back and compared, and then the directory entry is switched over in one
//! sector write, so a power cut leaves either the old or the new kernel, never neither.
//! (The bootloader itself stays: a new bootloader would need reinstalling.)

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

use crate::fs::{self, part};
use crate::library::Storage;
use crate::usb::Usb;

/// A kernel's build (commit) and commit time.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Build {
    pub id: String,
    pub time: u64,
}

impl Build {
    pub fn current() -> Build {
        Build { id: String::from(env!("SOLON_BUILD")), time: env!("SOLON_BUILD_TIME").parse().unwrap_or(0) }
    }

    /// "a93a5c0 (2026-11-20)"
    pub fn label(&self) -> String {
        if self.time == 0 {
            return self.id.clone();
        }
        let (y, m, d) = crate::doc::sheet::date_parts(self.time as f64 / 86400.0 + 25569.0);
        format!("{} ({}-{:02}-{:02})", self.id, y, m, d)
    }
}

/// A Solon kernel found on a USB drive.
#[derive(Clone)]
pub struct Candidate {
    pub label: String,
    pub build: Build,
}

/// The build marker inside a kernel file, if it is a Solon kernel.
pub fn kernel_build(data: &[u8]) -> Option<Build> {
    if data.len() < 64 || &data[..4] != b"\x7FELF" {
        return None;
    }
    let tag = b"SOLON-KERNEL:";
    let at = data.windows(tag.len()).position(|w| w == tag)? + tag.len();
    let end = data[at..].iter().take(80).position(|&b| b == b';')? + at;
    let text = core::str::from_utf8(&data[at..end]).ok()?;
    let (id, time) = text.rsplit_once(':')?;
    Some(Build { id: String::from(id), time: time.parse().ok()? })
}

/// Look for Solon kernels on the plugged-in USB drives (not phones).
pub fn find(usb: &mut Usb) -> Vec<(Candidate, Vec<u8>)> {
    let mut out = Vec::new();
    for s in usb.sources().into_iter().filter(|s| !s.phone) {
        let Some(root) = usb.source_root(s.drive, s.vol) else { continue };
        let Ok(top) = usb.list(s.drive, s.vol, &root) else { continue };
        let Some(boot) = top.into_iter().find(|e| e.is_dir && e.name.eq_ignore_ascii_case("boot")) else { continue };
        let Ok(files) = usb.list(s.drive, s.vol, &boot) else { continue };
        let Some(k) = files.into_iter().find(|e| !e.is_dir && e.name.eq_ignore_ascii_case("kernel") && e.size < 64 * 1024 * 1024) else { continue };
        let Ok(data) = usb.read_file(s.drive, s.vol, &k) else { continue };
        if let Some(build) = kernel_build(&data) {
            kprintln!("update: Solon {} on \"{}\"", build.label(), s.label);
            out.push((Candidate { label: s.label, build }, data));
        }
    }
    out
}

/// Replace the kernel in the boot partition of internal disk `disk` with `data`.
pub fn apply(storage: &mut Storage, disk: usize, data: &[u8]) -> Result<(), String> {
    crate::display::forget_boot_volume();
    if kernel_build(data).is_none() {
        return Err(String::from("That file isn't a Solon kernel"));
    }
    let total = storage.disk_info(disk).ok_or("disk not found")?.1;
    let r = storage.with_disk(disk, |dev| -> Result<(), String> {
        let g = part::read_gpt(dev, total).ok_or("the disk's partition table can't be read")?;
        let (_, first, last) = part::esp(&g).ok_or("the disk has no boot partition")?;
        let mut vol = fs::mount(dev, first, last - first + 1).ok_or("the boot partition can't be read")?;
        vol.enable_writes(dev).map_err(|e| e.message())?;
        let root = vol.root();
        let boot = vol.find(dev, &root, "boot").map_err(|e| e.message())?.filter(|e| e.is_dir).ok_or("the boot partition has no boot folder")?;
        // A leftover from an update that stopped part way.
        let _ = vol.fat_delete(dev, &boot, "kernel.new");
        let staged = vol.create_file(dev, &boot, "kernel.new", data).map_err(|e| format!("writing the new version: {}", e.message()))?;
        if vol.read_file(dev, &staged).map_err(|e| e.message())? != data {
            let _ = vol.fat_delete(dev, &boot, "kernel.new");
            return Err(String::from("the new version didn't read back correctly"));
        }
        vol.fat_swap_in(dev, &boot, "kernel", "kernel.new").map_err(|e| format!("switching to the new version: {}", e.message()))?;
        let now = vol.find(dev, &boot, "kernel").map_err(|e| e.message())?.ok_or("the kernel is missing")?;
        if vol.read_file(dev, &now).map_err(|e| e.message())? != data {
            return Err(String::from("the switched kernel doesn't match"));
        }
        Ok(())
    });
    match r {
        Some(Ok(())) => {
            kprintln!("update: kernel replaced on disk {} ({} bytes)", disk, data.len());
            Ok(())
        }
        Some(Err(e)) => Err(e),
        None => Err(String::from("disk not found")),
    }
}

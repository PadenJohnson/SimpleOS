#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

#[macro_use]
mod serial; // must come first so kprintln! is visible to later modules
mod crash;
mod cursor;
mod ahci;
mod desktop;
mod disk;
mod display;
mod doc;
mod diskmgr;
mod documents;
mod docapps;
mod docshell;
mod editor;
mod edit;
mod ehci;
mod files;
mod font;
mod fs;
mod games;
mod gfx;
mod hda;
mod hc;
mod icons;
mod import;
mod image;
mod install;
mod interrupts;
mod keys;
mod library;
mod memory;
mod movieui;
mod movie;
mod msc;
mod nvme;
mod pci;
mod pdf;
mod pdfui;
mod raster;
mod phone;
mod photos;
mod power;
mod print;
mod printui;
mod ps2;
mod rtc;
mod screen;
mod settings;
mod settingsui;
mod sheetui;
mod textedit;
mod textfield;
mod usb;
mod update;
mod usbprint;
mod videos;
mod wallpaper;
mod writer;
mod video;
mod xhci;

use core::panic::PanicInfo;
use core::sync::atomic::Ordering::Relaxed;

use alloc::string::String;
use limine::request::{
    ExecutableCmdlineRequest, ExecutableFileRequest, FramebufferRequest, HhdmRequest, MemoryMapRequest, ModuleRequest, RequestsEndMarker, RequestsStartMarker, RsdpRequest,
    StackSizeRequest,
};
use limine::BaseRevision;
use x86_64::instructions::{hlt, interrupts as cpu_irq};

use cursor::Cursor;
use desktop::{Desktop, PowerRequest};
use settings::Settings;
use gfx::Canvas;
use interrupts::{HZ, TICKS};
use photos::{Photos, Source};
use library::Storage;
use usb::Usb;
use ps2::Event;
use screen::Screen;

// ---- Limine boot protocol requests ---------------------------------------

#[used]
#[link_section = ".requests"]
static BASE_REVISION: BaseRevision = BaseRevision::new();

#[used]
#[link_section = ".requests"]
static FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest::new();

#[used]
#[link_section = ".requests"]
static MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest::new();

#[used]
#[link_section = ".requests"]
static HHDM_REQUEST: HhdmRequest = HhdmRequest::new();

/// Image decoders use a fair amount of stack; Limine's default is only 64 KiB.
#[used]
#[link_section = ".requests"]
static STACK_SIZE_REQUEST: StackSizeRequest = StackSizeRequest::new().with_size(1024 * 1024);

/// Marks this file as a Solon kernel, with its build and commit time (seconds since 1970),
/// so an update on a USB drive can be recognised and compared.
#[used]
pub static KERNEL_MARK: &str = concat!("SOLON-KERNEL:", env!("SOLON_BUILD"), ":", env!("SOLON_BUILD_TIME"), ";");

/// Boot command line (from limine.conf "cmdline:"), e.g. "crashtest".
#[used]
#[link_section = ".requests"]
static EXECUTABLE_CMDLINE_REQUEST: ExecutableCmdlineRequest = ExecutableCmdlineRequest::new();

/// ACPI tables (for shutting down and restarting).
#[used]
#[link_section = ".requests"]
static RSDP_REQUEST: RsdpRequest = RsdpRequest::new();

/// The kernel file itself, which tells us which disk we booted from.
#[used]
#[link_section = ".requests"]
static EXECUTABLE_FILE_REQUEST: ExecutableFileRequest = ExecutableFileRequest::new();

/// Files the bootloader loaded for us (the photos on the boot drive, for now).
#[used]
#[link_section = ".requests"]
static MODULE_REQUEST: ModuleRequest = ModuleRequest::new();

#[used]
#[link_section = ".requests_start_marker"]
static _REQUESTS_START: RequestsStartMarker = RequestsStartMarker::new();

#[used]
#[link_section = ".requests_end_marker"]
static _REQUESTS_END: RequestsEndMarker = RequestsEndMarker::new();

// ---- Kernel entry ---------------------------------------------------------

#[no_mangle]
extern "C" fn kmain() -> ! {
    serial::init();
    kprintln!("solon: kernel entered (build {})", env!("SOLON_BUILD"));

    assert!(BASE_REVISION.is_supported(), "bootloader does not support our protocol revision");

    let mmap = MEMORY_MAP_REQUEST.get_response().expect("no memory map");
    let hhdm = HHDM_REQUEST.get_response().expect("no HHDM").offset();
    let (heap, total) = memory::init(mmap.entries(), hhdm);
    kprintln!("solon: heap {} MiB (of {} MiB usable RAM)", heap >> 20, total >> 20);

    let screen = FRAMEBUFFER_REQUEST
        .get_response()
        .and_then(|resp| resp.framebuffers().next())
        .and_then(|fb| Screen::new(&fb));
    let Some(screen) = screen else {
        kprintln!("solon: no usable 32-bpp framebuffer");
        halt();
    };
    let (w, h) = (screen.width, screen.height);
    screen.register_for_crashes();
    kprintln!("solon: framebuffer {}x{}", w, h);
    // The screen modes the firmware offers, and what the monitor says about itself.
    if let Some(fb) = FRAMEBUFFER_REQUEST.get_response().and_then(|resp| resp.framebuffers().next()) {
        let modes: alloc::vec::Vec<(u32, u32, u16)> = fb.modes().unwrap_or(&[]).iter().map(|m| (m.width as u32, m.height as u32, m.bpp)).collect();
        display::init(&modes, (w as u32, h as u32), fb.edid());
    }

    // Which disk did we boot from? (Its GPT disk ID; lets Solon keep its library on its own stick.)
    let boot_disk: Option<[u8; 16]> = EXECUTABLE_FILE_REQUEST.get_response().and_then(|r| r.file().gpt_disk_id()).map(|u| {
        let mut g = [0u8; 16];
        g[0..4].copy_from_slice(&u.a.to_le_bytes());
        g[4..6].copy_from_slice(&u.b.to_le_bytes());
        g[6..8].copy_from_slice(&u.c.to_le_bytes());
        g[8..16].copy_from_slice(&u.d);
        g
    });
    match boot_disk {
        Some(g) => kprintln!("solon: booted from GPT disk {:02x}{:02x}{:02x}{:02x}-...", g[3], g[2], g[1], g[0]),
        None => kprintln!("solon: boot disk has no GPT id"),
    }

    interrupts::init();
    ps2::set_bounds(w, h);
    let mouse_ok = ps2::init();
    kprintln!(
        "solon: ps2 mouse init {}{}",
        if mouse_ok { "ok" } else { "FAILED" },
        if ps2::has_wheel() { " (scroll wheel)" } else { "" }
    );
    kprintln!("solon: stack {}", if STACK_SIZE_REQUEST.get_response().is_some() { "1 MiB" } else { "default (64 KiB)" });
    cpu_irq::enable(); // the timer drives timeouts from here on
    // Boot option "straytest": fire the interrupts real PCs send unasked (spurious IRQ 7/15,
    // a stray APIC vector, the APIC spurious vector, an NMI); Solon must carry on.
    if EXECUTABLE_CMDLINE_REQUEST.get_response().and_then(|r| r.cmdline().to_str().ok()).is_some_and(|c| c.split_whitespace().any(|w| w == "straytest")) {
        unsafe {
            x86_64::instructions::interrupts::software_interrupt::<39>();
            x86_64::instructions::interrupts::software_interrupt::<47>();
            x86_64::instructions::interrupts::software_interrupt::<0x50>();
            x86_64::instructions::interrupts::software_interrupt::<0xFF>();
            x86_64::instructions::interrupts::software_interrupt::<2>();
        }
        kprintln!("solon: stray interrupt test survived ({} ignored)", interrupts::STRAY.load(Relaxed));
    }

    // Everything (timeouts, double-clicks, the clock) depends on the timer: make sure it ticks.
    let start = TICKS.load(Relaxed);
    let mut spins: u64 = 0;
    while TICKS.load(Relaxed) < start + 2 {
        spins += 1;
        if spins > 2_000_000_000 {
            panic!("The system timer (PIT) isn't sending interrupts on this PC, so Solon can't run yet.");
        }
        core::hint::spin_loop();
    }

    // USB: controllers, keyboards/mice, and drives already plugged in.
    let t0 = TICKS.load(Relaxed);
    // Internal disks first: if Solon started from one, it isn't waiting for a USB stick.
    let mut disks = disk::scan();
    let boot_internal = boot_disk.is_some_and(|id| {
        disks.iter_mut().any(|d| {
            let total = d.blocks;
            fs::part::disk_guid(d, total) == Some(id)
        })
    });
    let mut usb = Usb::init(boot_disk, boot_disk.is_some() && !boot_internal);
    // Boot option "testphone": a pretend phone (QEMU can't emulate one here).
    if EXECUTABLE_CMDLINE_REQUEST.get_response().and_then(|r| r.cmdline().to_str().ok()).is_some_and(|c| c.split_whitespace().any(|w| w == "testphone")) {
        usb.add_test_phone();
    }
    let mut storage = Storage::init(&mut usb, disks);
    kprintln!("solon: usb ready in {} ms, {} drive(s)", (TICKS.load(Relaxed) - t0) * 1000 / HZ, usb.drives.len());

    // Sound card (Intel HD Audio).
    hda::init(&pci::scan());
    if EXECUTABLE_CMDLINE_REQUEST.get_response().and_then(|r| r.cmdline().to_str().ok()).is_some_and(|c| c.split_whitespace().any(|w| w == "soundtest")) {
        hda::self_test();
    }

    // Power off / restart need the ACPI tables.
    let acpi = RSDP_REQUEST.get_response().and_then(|r| {
        let a = r.address() as u64;
        power::init(if a >= hhdm { a - hhdm } else { a })
    });

    // Settings (saved in the library), then the display scale they ask for.
    let settings = match storage.read_root_file(&mut usb, "Settings.txt") {
        Some(t) => Settings::from_text(core::str::from_utf8(&t).unwrap_or(""), w, h),
        None => Settings::defaults(w, h),
    };
    kprintln!("settings: {:?}", settings);
    ps2::set_speed(settings::MOUSE[settings.mouse].1);
    let mut scale = settings.scale();
    let logical = |s: u32| (w * 2 / s as usize, h * 2 / s as usize);
    let (mut lw, mut lh) = logical(scale);
    ps2::set_bounds(lw, lh);

    // Photos on the boot drive (loaded into memory by the bootloader); decoded in the background.
    let files = MODULE_REQUEST.get_response().map(|r| r.modules()).unwrap_or(&[]);
    let mut photos = Photos::new(w, h);
    // Everything on the boot drive is available to the video editor; photos also go to Photos.
    for f in files.iter() {
        let path = f.path().to_str().unwrap_or("?");
        let name = String::from(path.rsplit('/').next().unwrap_or(path));
        let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(f.addr(), f.size() as usize) };
        movieui::BOOT_FILES.lock().push((name, bytes));
    }
    let files: alloc::vec::Vec<_> = files.iter().filter(|f| !matches!(movie::media_kind(f.path().to_str().unwrap_or("")), Some(movie::Kind::Video | movie::Kind::Music))).collect();
    if !files.is_empty() {
        let items = files
            .iter()
            .map(|f| {
                let path = f.path().to_str().unwrap_or("?");
                let name = String::from(path.rsplit('/').next().unwrap_or(path));
                let bytes: &'static [u8] = unsafe { core::slice::from_raw_parts(f.addr(), f.size() as usize) };
                (name, Source::Memory(bytes))
            })
            .collect();
        photos.set(String::from("Boot drive"), items, None);
    }

    let mut canvas = Canvas::new(lw, lh);
    let mut desktop = Desktop::new(lw, lh, photos, usb, storage, settings, (w, h));
    desktop.show_library();
    let (hh, mm, _) = rtc::time();
    desktop.set_clock(hh, mm);
    desktop.draw(&mut canvas);
    screen.blit_scaled(&canvas, scale);

    // The mouse moves in desktop (logical) pixels; the cursor is drawn on the real screen.
    let mut cursor = Cursor::new();
    cursor.set_zoom(if scale >= 3 { 2 } else { 1 });
    let to_screen = |v: i32, s: u32| v as usize * s as usize / 2;
    let (mut mx, mut my) = ps2::mouse_pos();
    cursor.show_at(&screen, to_screen(mx, scale), to_screen(my, scale));
    kprintln!("solon: display {}x{}, desktop {}x{} (scale {}%)", w, h, lw, lh, scale * 50);
    kprintln!("solon: desktop ready");
    let crash_test = EXECUTABLE_CMDLINE_REQUEST
        .get_response()
        .map(|r| r.cmdline().to_bytes().windows(9).any(|w| w == b"crashtest"))
        .unwrap_or(false);
    let crash_at = TICKS.load(Relaxed) + 2 * HZ;
    // Boot option "testprinter=ipp|pcl|pclmono": a printer whose jobs come out of COM2 (for testing).
    let cmdline = EXECUTABLE_CMDLINE_REQUEST.get_response().and_then(|r| r.cmdline().to_str().ok()).unwrap_or("");
    if let Some(kind) = cmdline.split_whitespace().find_map(|w| w.strip_prefix("testprinter=")) {
        desktop.set_test_printer(print::test_printer(kind));
    }
    // Boot option "selftest=originals": check edited-photo originals survive rename/move/bin.
    if cmdline.split_whitespace().any(|w| w == "selftest=originals") {
        desktop.self_test_originals();
    }

    let mut buttons = 0u8;
    let mut next_clock_check = 0u64;
    loop {
        // Sleep until an interrupt (keyboard, mouse or the 100 Hz timer),
        // without losing a wakeup that arrives in between.
        // While photos are decoding, don't sleep: keep working between input events.
        cpu_irq::disable();
        if ps2::pending() || desktop.busy() {
            cpu_irq::enable();
        } else {
            cpu_irq::enable_and_hlt();
        }

        let now = TICKS.load(Relaxed);
        let mut redraw = false;
        let mut moved = false;
        while let Some(ev) = ps2::pop() {
            match ev {
                Event::Mouse { x, y, buttons: b, wheel } => {
                    redraw |= desktop.on_mouse(x, y, b, buttons, wheel, now);
                    buttons = b;
                    moved |= (x, y) != (mx, my);
                    mx = x;
                    my = y;
                }
                Event::Key(sc) => redraw |= desktop.on_key(sc),
            }
        }

        redraw |= desktop.tick(now);
        if crash_test && now >= crash_at {
            // Boot option "crashtest": show the crash screen on purpose (for checking it works).
            panic!("crash test requested on the boot command line");
        }

        if now >= next_clock_check {
            next_clock_check = now + HZ;
            let (hh, mm, _) = rtc::time();
            redraw |= desktop.set_clock(hh, mm);
            redraw |= desktop.set_date(rtc::date());
        }

        match desktop.take_request() {
            Some(PowerRequest::Rescale) => {
                // Text size changed: draw the desktop at a new logical size.
                scale = desktop.settings().scale();
                (lw, lh) = logical(scale);
                canvas = Canvas::new(lw, lh);
                desktop.resize(lw, lh);
                ps2::set_bounds(lw, lh);
                (mx, my) = ps2::mouse_pos();
                cursor.hide(&screen);
                cursor.set_zoom(if scale >= 3 { 2 } else { 1 });
                kprintln!("solon: desktop {}x{} (scale {}%)", lw, lh, scale * 50);
                redraw = true;
            }
            Some(req @ (PowerRequest::ShutDown | PowerRequest::Restart)) => {
                cursor.hide(&screen);
                let shutting = req == PowerRequest::ShutDown;
                message(&mut canvas, &screen, scale, if shutting { "Turning off..." } else { "Restarting..." }, "");
                if shutting {
                    desktop.prepare_power_off();
                    power::shutdown(acpi.as_ref());
                    // Still here: the PC didn't switch itself off.
                    message(&mut canvas, &screen, scale, "It's now safe to turn off your computer.", "Hold the power button for a few seconds.");
                    halt();
                }
                power::restart(acpi.as_ref());
            }
            None => {}
        }

        if redraw {
            desktop.draw(&mut canvas);
            cursor.hide(&screen);
            screen.blit_scaled(&canvas, scale);
            if !desktop.screen_off() {
                cursor.show_at(&screen, to_screen(mx, scale), to_screen(my, scale));
            }
        } else if moved && !desktop.screen_off() {
            cursor.show_at(&screen, to_screen(mx, scale), to_screen(my, scale));
        }
    }
}

/// A full-screen message (shutting down, restarting...).
fn message(c: &mut Canvas, screen: &Screen, scale: u32, title: &str, line: &str) {
    let (w, h) = (c.w as i32, c.h as i32);
    c.reset_clip();
    c.fill_rect(0, 0, w, h, gfx::rgb(14, 20, 28));
    c.text_centered(w / 2, h / 2 - 30, title, gfx::Size::Large, true, gfx::rgb(240, 242, 245));
    c.text_centered(w / 2, h / 2 + 20, line, gfx::Size::Small, false, gfx::rgb(170, 180, 190));
    screen.blit_scaled(c, scale);
}

fn halt() -> ! {
    loop {
        cpu_irq::disable();
        hlt();
    }
}

#[panic_handler]
fn panic(info: &PanicInfo) -> ! {
    kprintln!("KERNEL PANIC: {}", info);
    crash::show(info);
    halt()
}

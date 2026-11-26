//! Solon, the SimpleOS kernel: the entry point. Limine starts `kmain`, which sets up memory,
//! interrupts, the screens and every driver (graphics, USB, disks, sound), reads the settings,
//! lights a second monitor if there is one, and then runs the desktop's main loop: sleep until
//! an interrupt, handle input, let the apps and drivers do their work, and put what changed on
//! the screens. See README.md in this folder for a map of the source.

#![no_std]
#![no_main]
#![feature(abi_x86_interrupt)]

extern crate alloc;

#[macro_use]
mod serial; // must come first so kprintln! is visible to later modules
mod backup;
mod calculator;
mod calendar;
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
mod frame;
mod media;
mod fs;
mod games;
mod gfx;
mod gpu;
mod hda;
mod hc;
mod icons;
mod import;
mod image;
mod imgfmt;
mod install;
mod interrupts;
mod keys;
mod library;
mod memory;
mod movieui;
mod movie;
mod music;
mod soundedit;
mod msc;
mod cpu;
mod hwmon;
mod nvme;
mod nvidia;
mod pci;
mod paint;
mod pdf;
mod pdfui;
mod raster;
mod phone;
mod photos;
mod power;
mod print;
mod printers;
mod printlang;
mod printui;
mod ps2;
mod rtc;
mod screen;
mod scrollbar;
mod settings;
mod settingsui;
mod smbios;
mod smp;
mod spell;
mod sheetui;
mod textedit;
mod textfield;
mod usb;
mod update;
mod usbaudio;
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
    ExecutableCmdlineRequest, ExecutableFileRequest, FramebufferRequest, HhdmRequest, MemoryMapRequest, ModuleRequest, MpRequest, RequestsEndMarker, RequestsStartMarker,
    RsdpRequest, SmbiosRequest, StackSizeRequest,
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

/// The other processor cores: Limine starts them and parks them for Solon to take over.
#[used]
#[link_section = ".requests"]
static MP_REQUEST: MpRequest = MpRequest::new();

/// The SMBIOS tables (what the motherboard, BIOS and memory are).
#[used]
#[link_section = ".requests"]
static SMBIOS_REQUEST: SmbiosRequest = SmbiosRequest::new();

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

/// Where Limine starts the kernel: switch on the floating-point and vector unit (SSE) before
/// any compiled code can use it, line the stack up the way compiled code expects, then go on
/// in `kernel_main`. (AVX is switched on later, by `cpu::enable_simd`.)
#[unsafe(naked)]
#[no_mangle]
extern "C" fn kmain() -> ! {
    core::arch::naked_asm!(
        "mov rax, cr0",
        "and rax, -13", // EM and TS off
        "or rax, 2",    // MP on
        "mov cr0, rax",
        "mov rax, cr4",
        "or rax, 0x600", // OSFXSR, OSXMMEXCPT
        "mov cr4, rax",
        "and rsp, -16",
        "call {main}",
        "ud2",
        main = sym kernel_main,
    )
}

extern "C" fn kernel_main() -> ! {
    cpu::enable_simd();
    serial::init();
    kprintln!("solon: kernel entered (build {})", env!("SOLON_BUILD"));

    assert!(BASE_REVISION.is_supported(), "bootloader does not support our protocol revision");

    let mmap = MEMORY_MAP_REQUEST.get_response().expect("no memory map");
    let hhdm = HHDM_REQUEST.get_response().expect("no HHDM").offset();
    let (heap, total) = memory::init(mmap.entries(), hhdm);
    kprintln!("solon: heap {} MiB (of {} MiB usable RAM)", heap >> 20, total >> 20);
    kprintln!("cpu: floating-point and vector unit {}", cpu::simd_text());

    let screen = FRAMEBUFFER_REQUEST
        .get_response()
        .and_then(|resp| resp.framebuffers().next())
        .and_then(|fb| Screen::new(&fb));
    let Some(mut screen) = screen else {
        kprintln!("solon: no usable 32-bpp framebuffer");
        halt();
    };
    // Boot option "secondtest" (QEMU has no second monitor): the right part of the screen plays
    // one, which "lights" 3 seconds after start; the PC's screen is the rest.
    let second_test = boot_option("secondtest").then(|| {
        let (fw, fh, base, pitch) = (screen.width, screen.height, screen.base_addr(), screen.pitch());
        let w1 = fw * 5 / 8;
        screen.set_mode(base as *mut u8, w1, fh, pitch);
        let right = base + w1 as u64 * 4;
        Screen::raw(right as *mut u8, fw - w1, fh, pitch, (16, 8, 0)).clear();
        (right, (fw - w1) as u32, (fh * 2 / 3) as u32, pitch as u32)
    });
    let (w, h) = (screen.width, screen.height);
    screen.register_for_crashes();
    kprintln!("solon: framebuffer {}x{}", w, h);
    kprintln!("display: framebuffer memory is cached {:?}, PAT {:#018x}", memory::caching_of(screen.base_addr()), unsafe { x86_64::registers::model_specific::Msr::new(0x277).read() });
    // The screen modes the firmware offers, and what the monitor says about itself.
    if let Some(fb) = FRAMEBUFFER_REQUEST.get_response().and_then(|resp| resp.framebuffers().next()) {
        let modes: alloc::vec::Vec<(u32, u32, u16)> = fb.modes().unwrap_or(&[]).iter().map(|m| (m.width as u32, m.height as u32, m.bpp)).collect();
        display::init(&modes, (w as u32, h as u32), fb.edid());
    }
    gpu::init(screen.base_addr().wrapping_sub(hhdm));
    // NVIDIA driver, stage 1 (only reads). Boot option "nonvidia" skips it.
    if !EXECUTABLE_CMDLINE_REQUEST.get_response().and_then(|r| r.cmdline().to_str().ok()).is_some_and(|c| c.split_whitespace().any(|w| w == "nonvidia")) {
        let nv = gpu::GPUS.lock().iter().find(|g| g.vendor == gpu::Vendor::Nvidia).map(|g| (g.bars.first().copied(), g.bars.get(1).copied(), g.at));
        if let Some((Some(bar0), bar1, at)) = nv {
            nvidia::probe(bar0, bar1, screen.base_addr().wrapping_sub(hhdm), at, screen.base_addr(), screen.pitch());
        }
    }
    // Monitors the firmware switched on: each gets its own picture. (With one, a second
    // monitor needs a native driver for the card; see docs/graphics.md.)
    if let Some(resp) = FRAMEBUFFER_REQUEST.get_response() {
        let fbs: alloc::vec::Vec<_> = resp.framebuffers().collect();
        kprintln!("display: the firmware set up {} screen(s)", fbs.len());
        for (i, fb) in fbs.iter().enumerate() {
            kprintln!("display:   screen {}: {}x{}, {} bpp, monitor info {}", i + 1, fb.width(), fb.height(), fb.bpp(), if fb.edid().is_some() { "yes" } else { "no" });
        }
        display::set_screens(fbs.len());
    }
    // Boot option "novbe": leave QEMU's graphics alone, as on a real PC (for testing scaling).
    if !EXECUTABLE_CMDLINE_REQUEST.get_response().and_then(|r| r.cmdline().to_str().ok()).is_some_and(|c| c.split_whitespace().any(|w| w == "novbe")) {
        display::init_driver();
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

    // The processor, the motherboard and its sensors (Settings, This PC, and the log).
    kprintln!("cpu: {}", cpu::describe());
    // Boot option "msrtest": a register no processor has must read as "not there", not crash.
    if EXECUTABLE_CMDLINE_REQUEST.get_response().and_then(|r| r.cmdline().to_str().ok()).is_some_and(|c| c.split_whitespace().any(|w| w == "msrtest")) {
        let apic = unsafe { x86_64::registers::model_specific::Msr::new(0x1b).read() };
        kprintln!(
            "cpu: probes: missing {:?}, APIC base {:x?} (direct {:#x}), x2APIC ID {:x?}, MPERF {:x?}, P-state 0 {:x?}",
            interrupts::rdmsr_safe(0x0bad_c0de), interrupts::rdmsr_safe(0x1b), apic, interrupts::rdmsr_safe(0x802), interrupts::rdmsr_safe(0xe7), interrupts::rdmsr_safe(0xc001_0064)
        );
    }
    if let Some(e) = SMBIOS_REQUEST.get_response().and_then(|r| r.entry_64().or(r.entry_32())) {
        let a = e.get() as u64;
        smbios::init(if a >= hhdm { a - hhdm } else { a });
    }
    kprintln!("hwmon: {}", hwmon::init());
    // Boot option "onecore": leave the other cores alone (Limine still starts them; they sleep).
    let one_core = EXECUTABLE_CMDLINE_REQUEST.get_response().and_then(|r| r.cmdline().to_str().ok()).is_some_and(|c| c.split_whitespace().any(|w| w == "onecore"));
    if let Some(mp) = MP_REQUEST.get_response() {
        if one_core {
            smp::park(mp);
        } else {
            smp::init(mp);
        }
    }
    smp::bench();
    cpu::sample(); // starts the clock and power counters
    if let Some(r) = hwmon::read() {
        kprintln!("hwmon: {}", hwmon::describe(&r));
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
        hda::self_test(&mut || {
            usb.poll();
        });
    }

    // Power off / restart need the ACPI tables.
    let acpi = RSDP_REQUEST.get_response().and_then(|r| {
        let a = r.address() as u64;
        power::init(if a >= hhdm { a - hhdm } else { a })
    });

    // Settings (saved in the library), then the display scale they ask for.
    let mut settings = match storage.read_root_file(&mut usb, "Settings.txt") {
        Some(t) => Settings::from_text(core::str::from_utf8(&t).unwrap_or(""), w, h),
        None => Settings::defaults(w, h),
    };
    kprintln!("settings: {:?}", settings);
    ps2::set_speed(settings::MOUSE[settings.mouse].1);
    spell::set_mode(settings.spelling);
    hda::set_volume(settings.volume);
    hda::choose(settings.sound_out);
    if let Some(t) = storage.read_root_file(&mut usb, "Dictionary.txt") {
        spell::load_user(core::str::from_utf8(&t).unwrap_or(""));
    }
    // The resolution to show the desktop at (switching the card, or scaled up).
    let mut view = apply_view(&mut screen, (w, h), settings.resolution);
    if !settings.size_fits(settings.size, view.res.0, view.res.1) {
        settings.size = 0;
    }
    let mut scale = settings.scale();
    let (mut lw, mut lh) = view.logical(scale);
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

    // A second monitor: lit by Solon's NVIDIA driver now (unless Settings say to use the main
    // screen only, or boot option "nolight"), when Settings ask for it later, or from the
    // diagnostics (F12, then L: its test picture shows for 3 seconds before the desktop).
    let no_light = boot_option("nolight");
    let mut second: Option<Screen> = None;
    let mut second_lit_at: Option<u64> = None;
    // Tries so far (a monitor that doesn't answer gets one more, when Settings change).
    let mut light_tries = 0;
    if !no_light && settings.screens != 2 && nvidia::light::waiting().is_some() {
        light_tries += 1;
        kprintln!("solon: {}", nvidia::light::light_second());
        second = nvidia::light::second_screen().map(|found| use_second(found, false));
        // Its sound may be the one chosen.
        hda::choose(settings.sound_out);
    }

    let mut canvas = Canvas::new(lw, lh);
    let mut desktop = Desktop::new(lw, lh, photos, usb, storage, settings, view.res);
    let mut lay = spread(&mut desktop, &mut canvas, (lw, lh), second.as_ref(), scale);
    blank_unused(&lay, &screen, view.dst, second.as_ref());
    let (cx, cy) = lay.main_centre();
    ps2::warp(cx, cy);
    desktop.show_library();
    let (hh, mm, _) = rtc::time();
    desktop.set_clock(hh, mm);
    desktop.draw(&mut canvas);
    present_all(&canvas, &lay, scale, &screen, view.dst, second.as_ref(), None);

    // The mouse moves in desktop (logical) pixels; each screen draws the pointer when it's on
    // that screen's part of the desktop.
    let mut cursor = Cursor::new();
    let mut cursor2 = Cursor::new();
    cursor.set_zoom(lay.cursor_zoom(0, &screen, view.dst));
    let (mut mx, mut my) = ps2::mouse_pos();
    place_pointer(&mut cursor, &screen, &lay, 0, mx, my, view.dst);
    if let Some(s2) = &second {
        cursor2.set_zoom(lay.cursor_zoom(1, s2, full_of(s2)));
        place_pointer(&mut cursor2, s2, &lay, 1, mx, my, full_of(s2));
    }
    kprintln!("solon: display {}x{}, desktop {}x{} (scale {}%)", view.res.0, view.res.1, lay.w, lay.h, scale * 50);
    kprintln!("solon: desktop ready");
    kprintln!("cpu: {}", cpu::describe_sample(&cpu::sample()));
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
    // Redraws, fast ones, drawing and sending time (us), since (us): logged while a video plays.
    let mut draw_stats = (0u64, 0u64, 0u64, 0u64, 0u64);
    let mut next_clock_check = 0u64;
    let mut next_traffic_log = TICKS.load(Relaxed) + 60 * HZ;
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

        if now >= next_traffic_log {
            next_traffic_log = now + 60 * HZ;
            let (sent, full) = screen.traffic();
            if full > 0 {
                kprintln!("display: sent {} of {} pixels to the screen ({}%)", sent, full, sent * 100 / full);
            }
        }
        if now >= next_clock_check {
            next_clock_check = now + HZ;
            let (hh, mm, _) = rtc::time();
            redraw |= desktop.set_clock(hh, mm);
            redraw |= desktop.set_date(rtc::date());
        }

        match desktop.take_request() {
            Some(PowerRequest::Rescale) => {
                // Text size, resolution or how the screens are used changed: lay the desktop
                // out again.
                cursor.hide(&screen);
                if let Some(s2) = &second {
                    cursor2.hide(s2);
                }
                let s = desktop.settings();
                view = apply_view(&mut screen, (w, h), s.resolution);
                scale = s.scale();
                (lw, lh) = view.logical(scale);
                desktop.set_screen(view.res);
                // Both screens wanted, and a monitor waiting that wasn't lit at start.
                if second.is_none() && s.screens != 2 && !no_light && light_tries < 2 && nvidia::light::waiting().is_some() {
                    light_tries += 1;
                    kprintln!("solon: {}", nvidia::light::light_second());
                    second = nvidia::light::second_screen().map(|found| use_second(found, false));
                    hda::choose(s.sound_out);
                }
                let (old, at) = (lay, ps2::mouse_pos());
                lay = spread(&mut desktop, &mut canvas, (lw, lh), second.as_ref(), scale);
                carry_pointer(&old, &lay, at);
                blank_unused(&lay, &screen, view.dst, second.as_ref());
                (mx, my) = ps2::mouse_pos();
                cursor.set_zoom(lay.cursor_zoom(0, &screen, view.dst));
                if let Some(s2) = &second {
                    cursor2.set_zoom(lay.cursor_zoom(1, s2, full_of(s2)));
                }
                kprintln!("solon: display {}x{}, desktop {}x{} (scale {}%)", view.res.0, view.res.1, lay.w, lay.h, scale * 50);
                redraw = true;
            }
            Some(req @ (PowerRequest::ShutDown | PowerRequest::Restart)) => {
                cursor.hide(&screen);
                let shutting = req == PowerRequest::ShutDown;
                let say = |title: &str, line: &str| {
                    message(&screen, view.logical(scale), scale, view.dst, title, line);
                    if let Some(s2) = &second {
                        message(s2, (s2.width * 2 / scale as usize, s2.height * 2 / scale as usize), scale, full_of(s2), title, line);
                    }
                };
                say(if shutting { "Turning off..." } else { "Restarting..." }, "");
                if shutting {
                    desktop.prepare_power_off();
                    power::shutdown(acpi.as_ref());
                    // Still here: the PC didn't switch itself off.
                    say("It's now safe to turn off your computer.", "Hold the power button for a few seconds.");
                    halt();
                }
                power::restart(acpi.as_ref());
            }
            None => {}
        }

        if second.is_none() {
            // A monitor lit from the diagnostics (or the test one): the desktop spreads onto it
            // once its test picture has shown for 3 seconds.
            if let Some(found) = second_test.or_else(nvidia::light::second_screen) {
                let lit = *second_lit_at.get_or_insert(now);
                if now >= lit + 3 * HZ {
                    cursor.hide(&screen);
                    let s2 = use_second(found, second_test.is_some());
                    // Its sound may be the one chosen.
                    hda::choose(desktop.settings().sound_out);
                    let (old, at) = (lay, ps2::mouse_pos());
                    lay = spread(&mut desktop, &mut canvas, (lw, lh), Some(&s2), scale);
                    carry_pointer(&old, &lay, at);
                    blank_unused(&lay, &screen, view.dst, Some(&s2));
                    (mx, my) = ps2::mouse_pos();
                    cursor.set_zoom(lay.cursor_zoom(0, &screen, view.dst));
                    cursor2.set_zoom(lay.cursor_zoom(1, &s2, full_of(&s2)));
                    second = Some(s2);
                    redraw = true;
                }
            }
        }
        // The video player: when only its picture or controls changed and nothing covers them,
        // just that area is drawn and sent.
        let video = desktop.tick_videos(now);
        let area = if video && !redraw { desktop.video_dirty() } else { None };
        redraw |= video;
        if redraw {
            let t0 = cpu::now_us();
            match area {
                Some(_) => desktop.draw_fast(&mut canvas),
                None => desktop.draw(&mut canvas),
            }
            let t1 = cpu::now_us();
            cursor.hide(&screen);
            if let Some(s2) = &second {
                cursor2.hide(s2);
            }
            present_all(&canvas, &lay, scale, &screen, view.dst, second.as_ref(), area);
            let t2 = cpu::now_us();
            // While a video plays: how long drawing and sending take, every 10 seconds.
            draw_stats.0 += 1;
            draw_stats.1 += area.is_some() as u64;
            draw_stats.2 += t1 - t0;
            draw_stats.3 += t2 - t1;
            if !desktop.video_playing() {
                draw_stats = (0, 0, 0, 0, t2);
            } else if t2 >= draw_stats.4 + 10_000_000 {
                let n = draw_stats.0.max(1);
                kprintln!(
                    "display: last 10 s of video: {} redraws ({} just the video), drawing {} ms and sending {} ms each",
                    draw_stats.0, draw_stats.1, draw_stats.2 / n / 1000, draw_stats.3 / n / 1000
                );
                draw_stats = (0, 0, 0, 0, t2);
            }
        }
        if (redraw || moved) && !desktop.hide_pointer() {
            place_pointer(&mut cursor, &screen, &lay, 0, mx, my, view.dst);
            if let Some(s2) = &second {
                place_pointer(&mut cursor2, s2, &lay, 1, mx, my, full_of(s2));
            }
        }
    }
}

/// A word on the kernel command line (boot options, mostly for testing).
fn boot_option(name: &str) -> bool {
    EXECUTABLE_CMDLINE_REQUEST.get_response().and_then(|r| r.cmdline().to_str().ok()).is_some_and(|c| c.split_whitespace().any(|w| w == name))
}

/// The second monitor (the NVIDIA driver's, or the test one: CPU address, width, height,
/// bytes a row), cleared, ready for the desktop.
fn use_second(found: (u64, u32, u32, u32), test: bool) -> Screen {
    let (virt, w2, h2, pitch2) = found;
    let s2 = Screen::raw(virt as *mut u8, w2 as usize, h2 as usize, pitch2 as usize, (16, 8, 0));
    s2.clear();
    let name = if test { String::from("Test screen") } else { nvidia::light::second_name().unwrap_or_else(|| String::from("Second monitor")) };
    kprintln!("solon: second monitor \"{}\" ({}x{}): showing the desktop", name, w2, h2);
    display::set_second(name, (w2, h2));
    s2
}

/// All of a screen.
fn full_of(s: &Screen) -> (usize, usize, usize, usize) {
    (0, 0, s.width, s.height)
}

/// How the desktop is spread over the screens.
#[derive(Clone, Copy)]
struct Layout {
    /// The whole desktop (the canvas), in desktop pixels.
    w: usize,
    h: usize,
    /// What each screen shows (x, y, w, h in the desktop): [0] the screen the PC started on,
    /// [1] the second monitor. None: nothing (it's black).
    parts: [Option<(usize, usize, usize, usize)>; 2],
    /// The main screen (0 or 1): the dock, the clock, new windows.
    main: usize,
    /// Both screens show all of the desktop (the other one scaled to fit).
    mirror: bool,
}

impl Layout {
    /// `one`, `two`: the screens' sizes in desktop pixels (`two`: None without a second).
    fn new(s: &Settings, one: (usize, usize), two: Option<(usize, usize)>) -> Layout {
        let Some(two) = two else {
            return Layout { w: one.0, h: one.1, parts: [Some((0, 0, one.0, one.1)), None], main: 0, mirror: false };
        };
        let main = s.main_screen.min(1);
        if s.screens == 0 {
            let ((w, h), [p1, p2]) = s.arrange(one, two);
            return Layout { w, h, parts: [Some(p1), Some(p2)], main, mirror: false };
        }
        // Mirror, or the main screen only: the desktop is the main screen's size.
        let (w, h) = if main == 0 { one } else { two };
        let mut parts = [None, None];
        parts[main] = Some((0, 0, w, h));
        if s.screens == 1 {
            parts[1 - main] = Some((0, 0, w, h));
        }
        Layout { w, h, parts, main, mirror: s.screens == 1 }
    }

    /// The screens as the desktop sees them (which monitor, where), and the main one's index.
    fn desktop_screens(&self) -> (alloc::vec::Vec<(usize, gfx::Rect)>, usize) {
        let rect = |p: (usize, usize, usize, usize)| gfx::Rect::new(p.0 as i32, p.1 as i32, p.2 as i32, p.3 as i32);
        match self.parts {
            [Some(a), Some(b)] if !self.mirror => (alloc::vec![(0, rect(a)), (1, rect(b))], self.main),
            _ => (alloc::vec![(self.main, rect((0, 0, self.w, self.h)))], 0),
        }
    }

    /// Where on screen `k` its part goes: `full` (all of it; for the first screen, the place
    /// the resolution setting gives), or scaled to fit when it mirrors the main screen.
    fn dst(&self, k: usize, s: &Screen, full: (usize, usize, usize, usize)) -> (usize, usize, usize, usize) {
        if self.mirror && k != self.main {
            s.fit(self.w, self.h)
        } else {
            full
        }
    }

    /// Where the desktop point (x, y) shows on screen `k` (its part shown in `dst`), if it does.
    fn pointer_on(&self, k: usize, x: i32, y: i32, dst: (usize, usize, usize, usize)) -> Option<(usize, usize)> {
        let (px, py, pw, ph) = self.parts[k]?;
        let (x, y) = (x as i64 - px as i64, y as i64 - py as i64);
        if x < 0 || y < 0 || x >= pw as i64 || y >= ph as i64 {
            return None;
        }
        Some((dst.0 + x as usize * dst.2 / pw, dst.1 + y as usize * dst.3 / ph))
    }

    /// A bigger pointer on a screen that enlarges the desktop 1.5 times or more.
    fn cursor_zoom(&self, k: usize, s: &Screen, full: (usize, usize, usize, usize)) -> usize {
        match self.parts[k] {
            Some(p) if self.dst(k, s, full).2 * 2 >= p.2 * 3 => 2,
            _ => 1,
        }
    }

    /// The middle of the main screen.
    fn main_centre(&self) -> (i32, i32) {
        let (x, y, w, h) = self.parts[self.main].unwrap_or((0, 0, self.w, self.h));
        ((x + w / 2) as i32, (y + h / 2) as i32)
    }

    /// The screen showing the desktop point (x, y) (the main one if both do), and where on it.
    fn locate(&self, x: i32, y: i32) -> Option<(usize, i64, i64, usize, usize)> {
        [self.main, 1 - self.main].into_iter().find_map(|k| {
            let (px, py, pw, ph) = self.parts[k]?;
            let (dx, dy) = (x as i64 - px as i64, y as i64 - py as i64);
            (dx >= 0 && dy >= 0 && dx < pw as i64 && dy < ph as i64).then_some((k, dx, dy, pw, ph))
        })
    }
}

/// Spread the desktop over the screens as Settings say: a new canvas, the desktop's screens,
/// where the pointer can go. `one`: the first screen's size in desktop pixels.
fn spread(desktop: &mut Desktop, canvas: &mut Canvas, one: (usize, usize), second: Option<&Screen>, scale: u32) -> Layout {
    let s = desktop.settings();
    let two = second.map(|s2| (s2.width * 2 / scale as usize, s2.height * 2 / scale as usize));
    let lay = Layout::new(&s, one, two);
    *canvas = Canvas::new(lay.w, lay.h);
    let (screens, main) = lay.desktop_screens();
    let rects: alloc::vec::Vec<(i32, i32, i32, i32)> = screens.iter().map(|(_, r)| (r.x, r.y, r.w, r.h)).collect();
    desktop.set_layout(lay.w, lay.h, screens, main);
    ps2::set_area(lay.w, lay.h, &rects);
    if second.is_some() {
        kprintln!(
            "solon: screens: {}, main screen {}, desktop {}x{}, first {:?}, second {:?}",
            settings::SCREENS[s.screens.min(2)], lay.main + 1, lay.w, lay.h, lay.parts[0], lay.parts[1]
        );
    }
    lay
}

/// After the layout changed: the pointer (at `at` before the change) stays at the same place
/// on the same screen, or goes to the middle of the main one if that screen shows nothing now.
fn carry_pointer(old: &Layout, new: &Layout, at: (i32, i32)) {
    let (x, y) = at;
    let to = old.locate(x, y).and_then(|(k, dx, dy, w, h)| {
        let (px, py, pw, ph) = new.parts[k]?;
        Some(((px as i64 + dx * pw as i64 / w.max(1) as i64) as i32, (py as i64 + dy * ph as i64 / h.max(1) as i64) as i32))
    });
    let (x, y) = to.unwrap_or_else(|| new.main_centre());
    ps2::warp(x, y);
}

/// Black on a screen that shows nothing (the other one, with "Main screen only"), and around
/// the picture on one that shows it smaller (mirroring a screen of another shape).
fn blank_unused(lay: &Layout, screen: &Screen, first_full: (usize, usize, usize, usize), second: Option<&Screen>) {
    if lay.parts[0].is_none() || lay.dst(0, screen, first_full) != first_full {
        screen.clear();
    }
    if let Some(s2) = second {
        if lay.parts[1].is_none() || lay.dst(1, s2, full_of(s2)) != full_of(s2) {
            s2.clear();
        }
    }
}

/// Put each screen's part of the desktop on it (when only `area` of the desktop changed, only
/// that is looked at).
fn present_all(canvas: &Canvas, lay: &Layout, scale: u32, screen: &Screen, first_full: (usize, usize, usize, usize), second: Option<&Screen>, area: Option<gfx::Rect>) {
    let area = area.map(|a| (a.x.max(0) as usize, a.y.max(0) as usize, a.w.max(0) as usize, a.h.max(0) as usize));
    if let Some(p) = lay.parts[0] {
        screen.present_area(canvas, p, scale, lay.dst(0, screen, first_full), area);
    }
    if let (Some(s2), Some(p)) = (second, lay.parts[1]) {
        s2.present_area(canvas, p, scale, lay.dst(1, s2, full_of(s2)), area);
    }
}

/// Draw the pointer on screen `k` where it shows the desktop point (x, y), or take it off that
/// screen if it doesn't.
fn place_pointer(cursor: &mut Cursor, s: &Screen, lay: &Layout, k: usize, x: i32, y: i32, full: (usize, usize, usize, usize)) {
    match lay.pointer_on(k, x, y, lay.dst(k, s, full)) {
        Some((sx, sy)) => cursor.show_at(s, sx, sy),
        None => cursor.hide(s),
    }
}

/// Where the desktop goes on the screen.
struct View {
    /// The resolution it's shown at.
    res: (usize, usize),
    /// Where on the screen (x, y, w, h): all of it, or a scaled-up part with black bars.
    dst: (usize, usize, usize, usize),
}

impl View {
    /// The desktop's size in its own (text-size scaled) pixels.
    fn logical(&self, scale: u32) -> (usize, usize) {
        (self.res.0 * 2 / scale as usize, self.res.1 * 2 / scale as usize)
    }
}

/// Show the desktop at resolution `want` ((0, 0) = the mode the screen started in, `boot`):
/// switch the graphics card if Solon can, else scale it up from the start-up mode.
fn apply_view(screen: &mut Screen, boot: (usize, usize), want: (u32, u32)) -> View {
    let target = if want == (0, 0) { boot } else { (want.0 as usize, want.1 as usize) };
    if display::vbe_present() && (screen.width, screen.height) != target {
        match display::vbe_switch(target.0 as u32, target.1 as u32) {
            Some((base, pitch)) => {
                screen.set_mode(base, target.0, target.1, pitch);
                screen.register_for_crashes();
                kprintln!("display: switched to {}x{}", target.0, target.1);
            }
            None => kprintln!("display: couldn't switch to {}x{}", target.0, target.1),
        }
    }
    let (fw, fh) = (screen.width, screen.height);
    let res = if target.0 <= fw && target.1 <= fh && display::usable((target.0 as u32, target.1 as u32)) { target } else { (fw, fh) };
    display::set_current((res.0 as u32, res.1 as u32));
    if res == (fw, fh) {
        return View { res, dst: (0, 0, fw, fh) };
    }
    // As big as fits with the same shape, in the middle; black around it.
    let (dw, dh) = if fw * res.1 <= fh * res.0 { (fw, res.1 * fw / res.0) } else { (res.0 * fh / res.1, fh) };
    screen.clear();
    View { res, dst: ((fw - dw) / 2, (fh - dh) / 2, dw, dh) }
}

/// A full-screen message (shutting down, restarting...) on a screen whose desktop pixels
/// measure `size`.
fn message(screen: &Screen, size: (usize, usize), scale: u32, dst: (usize, usize, usize, usize), title: &str, line: &str) {
    let mut c = Canvas::new(size.0, size.1);
    let (w, h) = (c.w as i32, c.h as i32);
    c.fill_rect(0, 0, w, h, gfx::rgb(14, 20, 28));
    c.text_centered(w / 2, h / 2 - 30, title, gfx::Size::Large, true, gfx::rgb(240, 242, 245));
    c.text_centered(w / 2, h / 2 + 20, line, gfx::Size::Small, false, gfx::rgb(170, 180, 190));
    screen.present(&c, scale, dst);
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

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
mod editor;
mod edit;
mod fs;
mod gfx;
mod icons;
mod import;
mod image;
mod interrupts;
mod library;
mod memory;
mod msc;
mod nvme;
mod pci;
mod photos;
mod print;
mod printui;
mod ps2;
mod rtc;
mod screen;
mod usb;
mod usbprint;
mod xhci;

use core::panic::PanicInfo;
use core::sync::atomic::Ordering::Relaxed;

use alloc::string::String;
use limine::request::{
    ExecutableCmdlineRequest, ExecutableFileRequest, FramebufferRequest, HhdmRequest, MemoryMapRequest, ModuleRequest, RequestsEndMarker, RequestsStartMarker,
    StackSizeRequest,
};
use limine::BaseRevision;
use x86_64::instructions::{hlt, interrupts as cpu_irq};

use cursor::Cursor;
use desktop::Desktop;
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

/// Boot command line (from limine.conf "cmdline:"), e.g. "crashtest".
#[used]
#[link_section = ".requests"]
static EXECUTABLE_CMDLINE_REQUEST: ExecutableCmdlineRequest = ExecutableCmdlineRequest::new();

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
    kprintln!("solon: kernel entered");

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
    let mut usb = Usb::init(boot_disk);
    let storage = Storage::init(&mut usb);
    kprintln!("solon: usb ready in {} ms, {} drive(s)", (TICKS.load(Relaxed) - t0) * 1000 / HZ, usb.drives.len());

    // Photos on the boot drive (loaded into memory by the bootloader); decoded in the background.
    let files = MODULE_REQUEST.get_response().map(|r| r.modules()).unwrap_or(&[]);
    let mut photos = Photos::new(w, h);
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

    let mut canvas = Canvas::new(w, h);
    let mut desktop = Desktop::new(w, h, photos, usb, storage);
    desktop.show_library();
    let (hh, mm, _) = rtc::time();
    desktop.set_clock(hh, mm);
    desktop.draw(&mut canvas);
    screen.blit(&canvas);

    let mut cursor = Cursor::new();
    let (mut mx, mut my) = ps2::mouse_pos();
    cursor.show_at(&screen, mx as usize, my as usize);
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
        }

        if redraw {
            desktop.draw(&mut canvas);
            cursor.hide(&screen);
            screen.blit(&canvas);
            cursor.show_at(&screen, mx as usize, my as usize);
        } else if moved {
            cursor.show_at(&screen, mx as usize, my as usize);
        }
    }
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

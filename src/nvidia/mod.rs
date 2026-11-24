//! NVIDIA GeForce driver, stage 1: look, don't touch.
//!
//! The plan (docs/graphics.md) is a display driver for NVIDIA's "nvdisplay" engine (Volta and
//! newer: RTX 20, 30 and 40 series): any screen mode and several monitors. Unlike 3D, the
//! display engine works without NVIDIA's signed firmware, which is how the open nouveau
//! driver drove these cards' screens. Register offsets here follow nouveau (MIT licence,
//! drivers/gpu/drm/nouveau/nvkm in Linux), credited with each.
//!
//! Stage 1 reads: the chip and video memory, the display engine's size, what each head shows
//! and through which output, and from the card's VBIOS tables every connector and whether a
//! monitor is plugged into it. Reading the VBIOS moves the BAR0 "PRAMIN" window over its copy
//! in video memory and back (as nouveau does), which doesn't affect the picture.
//!
//! Stage 2 talks to the monitors over DisplayPort AUX (nouveau i2c/auxgm200.c, padgm200.c):
//! their DisplayPort capabilities (DPCD) and EDID (name and modes). That only switches the
//! connector's pins to AUX mode if they aren't already; the picture isn't touched.
//!
//! Stage 3a reads video memory through the PRAMIN window (moved and put back): the display
//! engine's object table, to find where the firmware's picture is (its "DAVE" context DMA),
//! and the area with BAR1's page tables, to work out where a second picture can go.
//!
//! Stage 3b, only when asked (F12, then T), starts the display engine's core channel with a
//! command buffer in system memory, sends one command that leaves everything as it is, checks
//! the engine read it, and stops the channel (nouveau gv100_disp_core_init/fini).
//!
//! Stage 3c, only when asked (F12, then L), lights a second monitor with the card's own
//! display engine (`light.rs`), running the VBIOS's scripts for it (`bios.rs`).
//!
//! Everything goes to the log, and the VBIOS, the monitors' EDIDs, the register snapshot and
//! the video memory read in stage 3a can be saved to the Solon stick (F12, then S) to build the
//! next stage from. Boot option "nonvidia" skips it all.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

pub mod bios;
pub mod audio;
pub mod light;

/// A connector on the card, from the VBIOS, and what's plugged into it.
pub struct Conn {
    /// "DisplayPort 2", "HDMI 1".
    pub label: String,
    /// A monitor on its hot-plug pin (None = can't tell).
    pub plugged: Option<bool>,
    /// Its DisplayPort AUX channel.
    pub aux: Option<u8>,
    /// From the monitor's EDID: its name and best mode (width, height, refresh in 1/100 Hz).
    pub monitor: Option<String>,
    pub mode: Option<(u32, u32, u32)>,
    /// From its DPCD: "DisplayPort 1.4, 4 lanes at 8.1 Gbit/s".
    pub link: Option<String>,
    pub edid: Vec<u8>,
}

/// What the driver found, kept for Settings and for saving to the stick.
pub struct Report {
    /// One line for Settings ("GA102, 10 GiB, 4 heads").
    pub summary: String,
    pub connectors: Vec<Conn>,
    pub vbios: Vec<u8>,
    /// Display registers: for each range, start and count (u32 each), then the values.
    pub regs: Vec<u8>,
    /// Video memory read in stage 3a: (name for the file, VRAM address, bytes).
    pub vram: Vec<(&'static str, u64, Vec<u8>)>,
}

/// The card, kept for stages 3b and 3c (run later, from the diagnostics screen).
struct Card {
    bar0: u64,
    /// PCI location (bus, device, function).
    at: (u8, u8, u8),
    /// The firmware's picture as the CPU sees it (virtual address), and its row length.
    fb: u64,
    pitch: usize,
    chipset: u32,
    vram_mb: u32,
    /// BAR1 (video memory window): physical address and size.
    bar1: Option<(u64, u64)>,
}

static CARD: Mutex<Option<Card>> = Mutex::new(None);

/// The driver found an NVIDIA card it works with (for the diagnostics screen's key list).
pub fn present() -> bool {
    CARD.lock().is_some()
}

pub static REPORT: Mutex<Option<Report>> = Mutex::new(None);

pub struct Bar0 {
    base: u64,
}

impl Bar0 {
    pub fn rd(&self, off: u32) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + off as u64) as *const u32) }
    }
    pub fn wr(&self, off: u32, v: u32) {
        unsafe { core::ptr::write_volatile((self.base + off as u64) as *mut u32, v) }
    }
    /// Read-modify-write (nouveau nvkm_mask): clears `mask`, sets `val`; returns the old value.
    pub fn mask(&self, off: u32, mask: u32, val: u32) -> u32 {
        let old = self.rd(off);
        self.wr(off, (old & !mask) | val);
        old
    }
    pub fn rd8(&self, off: u32) -> u8 {
        unsafe { core::ptr::read_volatile((self.base + off as u64) as *const u8) }
    }
    pub fn wr8(&self, off: u32, v: u8) {
        unsafe { core::ptr::write_volatile((self.base + off as u64) as *mut u8, v) }
    }
}

/// Chip names by the chipset number in PMC_BOOT_0 (bits 20-28), nouveau's device list.
fn chip_name(chipset: u32) -> &'static str {
    match chipset {
        0x140 => "GV100",
        0x162 => "TU102",
        0x164 => "TU104",
        0x166 => "TU106",
        0x167 => "TU117",
        0x168 => "TU116",
        0x170 => "GA100",
        0x172 => "GA102",
        0x173 => "GA103",
        0x174 => "GA104",
        0x176 => "GA106",
        0x177 => "GA107",
        0x192 => "AD102",
        0x193 => "AD103",
        0x194 => "AD104",
        0x196 => "AD106",
        0x197 => "AD107",
        c if c >= 0x1a0 => "Blackwell",
        c if c >= 0x130 => "Pascal",
        c if c >= 0x110 => "Maxwell",
        c if c >= 0x0e0 => "Kepler",
        _ => "older",
    }
}

/// Display register ranges saved to the stick: (start, number of 32-bit registers).
const SNAPSHOT: [(u32, u32); 21] = [
    (0x000000, 0x40),   // PMC: chip identification
    (0xb80f00, 0x40),   // BAR1/BAR2 instance blocks and modes (Turing and newer)
    (0x001700, 0x10),   // PRAMIN window and BAR1/BAR2 setup
    (0x00d900, 0x200),  // AUX channels and I2C pads
    (0x00e800, 0x40),   // DisplayPort spread spectrum (0xe86c)
    (0x00e900, 0x40),   // VPLL enables
    (0x00ec00, 0x40),   // SOR clocks
    (0x00ef00, 0x100),  // VPLLs (pixel clocks)
    (0x021200, 0x40),   // GPIO lines (bit 14: the pin's level)
    (0x088000, 0x40),   // PCI configuration (mirror)
    (0x100c00, 0x40),   // memory controller: flushes, local memory range (0x100ce0)
    (0x1fa7c0, 0x20),   // protected video memory: WPR2, MMU lock, VPR
    (0x610000, 0x800),  // display: capabilities, channels, interrupts
    (0x612000, 0x800),  // SOR clocks (0x612300 + SOR * 0x800) and pad-link routing
    (0x616000, 0x800),  // heads: capabilities, raster generator
    (0x61c000, 0x800),  // SORs (outputs): capabilities, links
    (0x625000, 0x400),  // display: ownership, VBIOS image location
    (0x640000, 0x400),  // capabilities the driver publishes to the engine
    (0x680000, 0x4000), // core channel state: assembly and armed (what's on screen)
    (0x690000, 0x2000), // window channels 0-7: state (assembly, armed at +0x800)
    (0x6b0000, 0x1000), // window-immediate channels 0-3: state
];

/// Run stages 1, 2 and 3a on an NVIDIA card. `bar0`: its register window; `bar1`: its video
/// memory window; `fb`: where the firmware's picture is (physical).
pub fn probe(bar0: (u64, u64), bar1: Option<(u64, u64)>, fb: u64, at: (u8, u8, u8), fb_virt: u64, pitch: usize) {
    if bar0.1 < 0x0100_0000 {
        kprintln!("nvidia: register window too small ({:#x})", bar0.1);
        return;
    }
    let r = Bar0 { base: crate::memory::map_mmio(bar0.0, 0x0100_0000) };
    let boot0 = r.rd(0x000000);
    let chipset = (boot0 & 0x1ff0_0000) >> 20;
    let name = chip_name(chipset);
    kprintln!("nvidia: chip {} (chipset {:#x}, revision {:#x}, PMC_BOOT_0 {:#010x})", name, chipset, boot0 & 0xff, boot0);
    // Video memory in MiB (Ampere: nouveau ga102_fb_vidmem_size).
    let vram_mb = if chipset >= 0x170 { r.rd(0x1183a4) } else { 0 };
    *CARD.lock() = Some(Card { bar0: r.base, at, fb: fb_virt, pitch, chipset, vram_mb, bar1 });
    if vram_mb != 0 {
        kprintln!("nvidia: {} MiB video memory", vram_mb);
    }
    if chipset < 0x140 {
        kprintln!("nvidia: pre-Volta display engine: stage 1 covers Volta and newer only");
        return;
    }

    // nvdisplay counts (nouveau gv100_head_cnt / gv100_sor_cnt / gv100_disp_wndw_cnt).
    let (m0, cnt) = (r.rd(0x610060), r.rd(0x610074));
    let (heads_mask, heads) = (m0 & 0xff, cnt & 0xf);
    let (sors_mask, sors) = ((m0 >> 8) & 0xff, (cnt >> 8) & 0xf);
    let (wndw_mask, wndws) = (r.rd(0x610064), (cnt >> 20) & 0x3f);
    let owned_by_firmware = r.rd(0x6254e8) & 2 != 0;
    kprintln!(
        "nvidia: display engine: {} heads (mask {:#x}), {} outputs (mask {:#x}), {} windows (mask {:#x}); {}",
        heads, heads_mask, sors, sors_mask, wndws, wndw_mask,
        if owned_by_firmware { "run by the firmware (VBIOS/GOP)" } else { "not owned by the firmware" }
    );

    // What each head shows: armed state (nouveau gv100_head_state).
    let mut lit = 0;
    for id in 0..8u32 {
        if heads_mask & (1 << id) == 0 {
            continue;
        }
        let hoff = 0x8000 + id * 0x400;
        let total = r.rd(0x682064 + hoff);
        let blank_end = r.rd(0x68206c + hoff);
        let blank_start = r.rd(0x682070 + hoff);
        let hz = r.rd(0x68200c + hoff);
        let (htotal, vtotal) = (total & 0xffff, total >> 16);
        let (w, h) = ((blank_start & 0xffff).saturating_sub(blank_end & 0xffff), (blank_start >> 16).saturating_sub(blank_end >> 16));
        if htotal == 0 || vtotal == 0 || hz == 0 {
            kprintln!("nvidia:   head {}: off", id);
            continue;
        }
        lit += 1;
        let refresh = hz as u64 * 100 / (htotal as u64 * vtotal as u64);
        kprintln!("nvidia:   head {}: {}x{} at {}.{:02} Hz (pixel clock {} kHz, total {}x{})", id, w, h, refresh / 100, refresh % 100, hz / 1000, htotal, vtotal);
    }
    // Which output drives which head (nouveau gv100_sor_state, armed).
    for id in 0..8u32 {
        if sors_mask & (1 << id) == 0 {
            continue;
        }
        let ctrl = r.rd(0x680300 + 0x8000 + id * 0x20);
        let proto = match (ctrl >> 8) & 0xf {
            0 => "LVDS",
            1 => "TMDS (HDMI/DVI) link A",
            2 => "TMDS link B",
            5 => "TMDS dual link",
            8 => "DisplayPort link A",
            9 => "DisplayPort link B",
            _ => "unknown",
        };
        let head = ctrl & 0xff;
        if head == 0 {
            kprintln!("nvidia:   output {}: not in use", id);
        } else {
            kprintln!("nvidia:   output {}: {} to head mask {:#x}", id, proto, head);
        }
    }

    let vbios = read_vbios(&r, chipset);
    let mut connectors = if vbios.is_empty() { Vec::new() } else { connectors(&r, &vbios) };
    let plugged = connectors.iter().filter(|c| c.plugged == Some(true)).count();
    if !connectors.is_empty() {
        kprintln!("nvidia: {} connectors, monitors plugged into {}", connectors.len(), plugged);
    }
    // Stage 2: ask each DisplayPort monitor about itself.
    for c in connectors.iter_mut().filter(|c| c.plugged != Some(false)) {
        if let Some(ch) = c.aux {
            talk_to_monitor(&r, ch, c);
        }
    }

    let vram = if chipset >= 0x170 { find_picture(&r, bar1, fb) } else { Vec::new() };

    // The register snapshot.
    let mut regs = Vec::new();
    for &(start, count) in SNAPSHOT.iter() {
        regs.extend_from_slice(&start.to_le_bytes());
        regs.extend_from_slice(&count.to_le_bytes());
        for k in 0..count {
            regs.extend_from_slice(&r.rd(start + k * 4).to_le_bytes());
        }
    }
    let summary = format!("{}{}, {} heads, {} lit", name, if vram_mb != 0 { format!(", {} GiB", vram_mb / 1024) } else { String::new() }, heads, lit);
    *REPORT.lock() = Some(Report { summary, connectors, vbios, regs, vram });
}

/// The VBIOS: the copy the card keeps in video memory (through the PRAMIN window, nouveau
/// shadowramin.c), else the ROM chip (PROM, shadowrom.c). Empty if neither works.
fn read_vbios(r: &Bar0, chipset: u32) -> Vec<u8> {
    let mut out = Vec::new();
    // PRAMIN: the image pointer needs the display engine (GA100+: 0x820c04 bit 0 = disabled).
    let disabled = if chipset >= 0x170 { r.rd(0x820c04) } else { r.rd(0x021c04) } & 1 != 0;
    let loc = r.rd(0x625f04);
    if !disabled && loc & 8 != 0 && loc & 3 == 1 {
        let mut addr = ((loc & 0xffff_ff00) as u64) << 8;
        if addr == 0 {
            addr = ((r.rd(0x001700) as u64) << 16) + 0xf0000;
        }
        let saved = r.rd(0x001700);
        r.wr(0x001700, (addr >> 16) as u32);
        for k in 0..0x40000u32 {
            out.extend_from_slice(&r.rd(0x700000 + k * 4).to_le_bytes());
        }
        r.wr(0x001700, saved);
        if out.starts_with(&[0x55, 0xAA]) {
            kprintln!("nvidia: VBIOS read from video memory at {:#x}", addr);
            return trim_rom(out);
        }
        kprintln!("nvidia: no VBIOS in video memory (starts {:02x?})", &out[..4]);
        out.clear();
    } else {
        kprintln!("nvidia: VBIOS copy in video memory not available ({:#x}, disabled {})", loc, disabled);
    }
    // PROM: the ROM chip at 0x300000 (with the PCI ROM shadow off: config register 0x50 bit 0,
    // mirrored at BAR0 0x088050), possibly behind an IFR header ("NVGI").
    let shadow = r.rd(0x088050);
    r.wr(0x088050, shadow & !1);
    // Reads stay inside the 1 MiB ROM window whatever the header says.
    let prom = |off: u32| if off < 0x10_0000 { r.rd(0x300000 + (off & !3)) } else { 0 };
    let mut start = 0u32;
    if prom(0) == 0x4947_564E {
        let fixed1 = prom(4);
        start = match (fixed1 >> 8) & 0xff {
            1 | 2 => prom(((fixed1 >> 16) & 0x7fff).saturating_add(4)),
            3 => {
                let dir = prom(prom(8) & 0x000f_ffff).saturating_add(4096);
                if prom(dir) == 0x4452_4652 {
                    prom(dir.saturating_add(8))
                } else {
                    0x10_0000
                }
            }
            _ => 0x10_0000,
        };
    }
    if start < 0x10_0000 {
        for k in 0..(0x10_0000 - start) / 4 {
            out.extend_from_slice(&r.rd(0x300000 + start + k * 4).to_le_bytes());
        }
    }
    r.wr(0x088050, shadow);
    if out.starts_with(&[0x55, 0xAA]) {
        kprintln!("nvidia: VBIOS read from the ROM chip (image at {:#x})", start);
        return trim_rom(out);
    }
    kprintln!("nvidia: couldn't read the VBIOS");
    Vec::new()
}

/// Keep the chain of ROM images (each "55 AA" with a "PCIR" structure; the last one has bit 7
/// of its indicator set), not the empty space after it.
fn trim_rom(mut b: Vec<u8>) -> Vec<u8> {
    let mut at = 0usize;
    let mut end = b.len();
    let byte = |b: &[u8], o: usize| b.get(o).copied().unwrap_or(0);
    for _ in 0..8 {
        if b.get(at..at + 2) != Some(&[0x55, 0xAA]) {
            break;
        }
        let pcir = at + u16::from_le_bytes([byte(&b, at + 0x18), byte(&b, at + 0x19)]) as usize;
        if b.get(pcir..pcir + 4) != Some(b"PCIR") {
            break;
        }
        let len = u16::from_le_bytes([byte(&b, pcir + 0x10), byte(&b, pcir + 0x11)]) as usize * 512;
        let last = byte(&b, pcir + 0x15) & 0x80 != 0;
        at += len.max(512);
        end = at.min(b.len());
        if last {
            break;
        }
    }
    b.truncate(end);
    b
}

/// The connectors from the VBIOS's DCB, connector and GPIO tables (nouveau bios/dcb.c, conn.c,
/// gpio.c; engine/disp/conn.c), each with whether its hot-plug pin says a monitor is there.
fn connectors(r: &Bar0, b: &[u8]) -> Vec<Conn> {
    let rd8 = |o: usize| b.get(o).copied().unwrap_or(0);
    let rd16 = |o: usize| u16::from_le_bytes([rd8(o), rd8(o + 1)]) as usize;
    let rd32 = |o: usize| u32::from_le_bytes([rd8(o), rd8(o + 1), rd8(o + 2), rd8(o + 3)]);
    let dcb = rd16(0x36);
    let ver = rd8(dcb);
    if dcb == 0 || !(0x30..0x42).contains(&ver) || rd32(dcb + 6) != 0x4edc_bdcb {
        kprintln!("nvidia: no display table (DCB) in the VBIOS (at {:#x}, version {:#x})", dcb, ver);
        return Vec::new();
    }
    let (hdr, cnt, len) = (rd8(dcb + 1) as usize, rd8(dcb + 2) as usize, rd8(dcb + 3) as usize);
    kprintln!("nvidia: DCB {}.{} at {:#x}: {} entries", ver >> 4, ver & 15, dcb, cnt);
    // I2C/AUX table (nouveau bios/i2c.c, version 4.1: drive in bits 0-4, AUX channel 5-9).
    let mut aux_of_bus: Vec<Option<u8>> = Vec::new();
    let it = rd16(dcb + 4);
    if it != 0 && rd8(it) >= 0x41 {
        let (ih, ic, il) = (rd8(it + 1) as usize, rd8(it + 2) as usize, rd8(it + 3) as usize);
        for k in 0..ic {
            let e = rd16(it + ih + k * il);
            let auxch = ((e >> 5) & 0x1f) as u8;
            aux_of_bus.push(if auxch == 0x1f { None } else { Some(auxch) });
        }
    }
    // Each connector's AUX channel: from its DisplayPort output entry's bus.
    let mut aux_of_conn: [Option<u8>; 16] = [None; 16];
    // Outputs (just logged: which output resource and connector each uses).
    for i in 0..cnt {
        let e = dcb + hdr + i * len;
        let d = rd32(e);
        let ty = d & 0xf;
        if ty == 0xe {
            break;
        }
        if ty == 0xf {
            continue;
        }
        let kind = match ty {
            0 => "analog",
            1 => "TV",
            2 => "TMDS",
            3 => "LVDS",
            6 => "DisplayPort",
            _ => "other",
        };
        kprintln!("nvidia:   output entry {}: {} on connector {}, output resources {:#x}, heads {:#x}, bus {}", i, kind, (d >> 12) & 0xf, (d >> 24) & 0xf, (d >> 8) & 0xf, (d >> 4) & 0xf);
        if ty == 6 {
            let conn = ((d >> 12) & 0xf) as usize;
            if aux_of_conn[conn].is_none() {
                aux_of_conn[conn] = aux_of_bus.get(((d >> 4) & 0xf) as usize).copied().flatten();
            }
        }
    }
    // GPIO table: hot-plug functions and their lines.
    let mut gpios: Vec<(u8, u8, u8)> = Vec::new(); // (func, line, logical level of "on")
    if hdr >= 0x0c {
        let g = rd16(dcb + 0x0a);
        let gv = rd8(g);
        if g != 0 && (0x30..=0x41).contains(&gv) {
            let (gh, gc, gl) = (rd8(g + 1) as usize, rd8(g + 2) as usize, rd8(g + 3) as usize);
            for k in 0..gc {
                let e = g + gh + k * gl;
                let info = rd32(e);
                let (line, func, log1) = if gv < 0x41 {
                    ((info & 0x1f) as u8, ((info >> 8) & 0xff) as u8, ((info >> 29) & 3) as u8)
                } else {
                    ((info & 0x3f) as u8, ((info >> 8) & 0xff) as u8, ((rd8(e + 4) >> 6) & 3) as u8)
                };
                gpios.push((func, line, log1));
            }
        }
    }
    // Connector table.
    let mut out = Vec::new();
    if hdr < 0x16 {
        return out;
    }
    let c = rd16(dcb + 0x14);
    let cv = rd8(c);
    if c == 0 || !(cv == 0x30 || cv == 0x40) {
        kprintln!("nvidia: no connector table (at {:#x}, version {:#x})", c, cv);
        return out;
    }
    let (ch, cc, cl) = (rd8(c + 1) as usize, rd8(c + 2) as usize, rd8(c + 3) as usize);
    const HPD_FUNC: [u8; 7] = [0x07, 0x08, 0x51, 0x52, 0x5e, 0x5f, 0x60];
    let mut counts: Vec<(&str, usize)> = Vec::new();
    for k in 0..cc {
        let e = c + ch + k * cl;
        let ty = rd8(e);
        if ty == 0xff {
            continue;
        }
        let mut hpd = ((rd8(e + 1) & 0x30) >> 4) as u32;
        if cl >= 4 {
            hpd |= ((rd8(e + 2) & 0x03) as u32) << 2;
            hpd |= ((rd8(e + 3) & 0x07) as u32) << 4;
        }
        let kind = match ty {
            0x00 => "VGA",
            0x30 | 0x31 => "DVI",
            0x46 => "DisplayPort",
            0x47 => "eDP",
            0x48 => "Mini DisplayPort",
            0x60 | 0x61 => "HDMI",
            0x63 => "Mini HDMI",
            0x71 => "USB-C",
            _ => "connector",
        };
        let n = match counts.iter_mut().find(|x| x.0 == kind) {
            Some(x) => {
                x.1 += 1;
                x.1
            }
            None => {
                counts.push((kind, 1));
                1
            }
        };
        let label = format!("{} {}", kind, n);
        // The hot-plug pin: lowest set bit picks the GPIO function; its line's level.
        let state = if hpd != 0 && (hpd.trailing_zeros() as usize) < HPD_FUNC.len() {
            let func = HPD_FUNC[hpd.trailing_zeros() as usize];
            gpios.iter().find(|g| g.0 == func).map(|&(_, line, log1)| {
                let level = r.rd(0x021200 + line as u32 * 4) & 0x4000 != 0;
                level == (log1 & 1 == 1)
            })
        } else {
            None
        };
        let aux = aux_of_conn.get(k).copied().flatten();
        kprintln!(
            "nvidia:   {} (type {:#04x}, entry {}{}): {}",
            label, ty, k,
            aux.map(|a| format!(", AUX {}", a)).unwrap_or_default(),
            match state {
                Some(true) => "monitor plugged in",
                Some(false) => "nothing plugged in",
                None => "can't tell",
            }
        );
        out.push(Conn { label, plugged: state, aux, monitor: None, mode: None, link: None, edid: Vec::new() });
    }
    out
}

// ---- stage 2: DisplayPort AUX ------------------------------------------------------------

/// Spend about `n` microseconds (each register read over PCIe takes roughly one).
fn spin(r: &Bar0, n: u32) {
    for _ in 0..n {
        let _ = r.rd(0x000000);
    }
}

/// Wait (polling, about 1 microsecond per try) until `f` says yes, at most `tries` times.
/// AUX waits are short; see `poll_ms` for longer ones.
fn poll(r: &Bar0, tries: u32, f: impl FnMut(&Bar0) -> bool) -> bool {
    poll_ms(r, tries, 50, f)
}

/// Like `poll`, giving up after about `ms` milliseconds however slow the reads are (once the
/// timer runs; before that, after `tries` reads).
fn poll_ms(r: &Bar0, tries: u32, ms: u64, mut f: impl FnMut(&Bar0) -> bool) -> bool {
    let hz = crate::interrupts::HZ;
    let start = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
    for _ in 0..tries {
        if f(r) {
            return true;
        }
        if crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed) > start + ms * hz / 1000 {
            break;
        }
    }
    f(r)
}

/// Busy-wait about `us` microseconds, timed by the processor's time-stamp counter (calibrated
/// once against the 100 Hz timer).
pub fn udelay(us: u64) {
    let tsc = || unsafe { core::arch::x86_64::_rdtsc() };
    let end = tsc() + us * crate::cpu::tsc_per_us();
    while tsc() < end {
        core::hint::spin_loop();
    }
}

pub fn mdelay(ms: u64) {
    udelay(ms * 1000);
}

/// One AUX transaction on channel `ch` (nouveau gm200_i2c_aux_xfer): `ty` 8 = native write,
/// 9 = native read, 0/1 = I2C write/read (+4 = more to follow), up to 16 bytes at `addr`.
/// Returns the sink's reply code (0 = done) and the bytes read.
fn aux(r: &Bar0, ch: u8, ty: u8, addr: u32, data: &[u8], size: usize) -> Result<(u32, Vec<u8>), &'static str> {
    let base = ch as u32 * 0x50;
    let ctl = 0x00d954 + base;
    let mask = |a: u32, m: u32, v: u32| {
        let old = r.rd(a);
        r.wr(a, (old & !m) | v);
    };
    let fini = || mask(ctl, 0x0071_0000, 0);
    // Wait for any earlier transaction, then claim the channel.
    if !poll(r, 1000, |r| r.rd(ctl) & 0x0701_0000 == 0) {
        return Err("the AUX channel stays busy");
    }
    mask(ctl, 0x0070_0000, 0x0010_0000);
    if !poll(r, 1000, |r| r.rd(ctl) & 0x0700_0000 == 0x0100_0000) {
        fini();
        return Err("the AUX channel didn't answer");
    }
    if r.rd(0x00d958 + base) & 0x1000_0000 == 0 {
        fini();
        return Err("no monitor on the AUX channel");
    }
    mask(0x00d968 + base, 0x0001_0000, 0); // automatic DPCD reads off meanwhile
    if ty & 1 == 0 {
        let mut x = [0u8; 16];
        x[..data.len().min(16)].copy_from_slice(&data[..data.len().min(16)]);
        for i in 0..4 {
            r.wr(0x00d930 + base + i * 4, u32::from_le_bytes([x[i as usize * 4], x[i as usize * 4 + 1], x[i as usize * 4 + 2], x[i as usize * 4 + 3]]));
        }
    }
    let mut c = r.rd(ctl) & !0x0001_f1ff;
    c |= (ty as u32) << 12;
    c |= if size > 0 { (size as u32 - 1) & 0xf } else { 0x100 };
    r.wr(0x00d950 + base, addr);
    let mut stat = 0;
    let mut result: Result<(), &'static str> = Ok(());
    // Like nouveau: try again (up to 32 times) after a deferral, no reply or an error.
    for attempt in 0..32 {
        r.wr(ctl, 0x8000_0000 | c);
        r.wr(ctl, c);
        if attempt > 0 {
            spin(r, 400);
        }
        r.wr(ctl, 0x0001_0000 | c);
        if !poll(r, 2000, |r| r.rd(ctl) & 0x0001_0000 == 0) {
            result = Err("the AUX transaction timed out");
            break;
        }
        stat = r.rd(0x00d958 + base);
        r.wr(0x00d958 + base, stat);
        result = if stat & 0x100 != 0 {
            Err("the monitor didn't reply")
        } else if stat & 0xe00 != 0 {
            Err("AUX error")
        } else if matches!(stat & 0x000f_0000, 0x0008_0000 | 0x0002_0000) {
            Err("the monitor kept deferring")
        } else {
            Ok(())
        };
        if result.is_ok() {
            break;
        }
    }
    let mut out = Vec::new();
    if result.is_ok() && ty & 1 == 1 {
        for i in 0..4 {
            out.extend_from_slice(&r.rd(0x00d940 + base + i * 4).to_le_bytes());
        }
        out.truncate(((stat & 0x1f) as usize).min(size));
    }
    mask(0x00d968 + base, 0x0001_0000, 0x0001_0000);
    fini();
    result.map(|_| ((stat >> 16) & 0xf, out))
}

/// Write bytes to the monitor's DisplayPort configuration (DPCD) at `addr`.
pub fn dpcd_write(r: &Bar0, ch: u8, addr: u32, data: &[u8]) -> Result<(), &'static str> {
    let mut done = 0;
    while done < data.len() {
        let k = (data.len() - done).min(16);
        let (reply, _) = aux(r, ch, 8, addr + done as u32, &data[done..done + k], k)?;
        if reply != 0 {
            return Err("the monitor refused the DPCD write");
        }
        done += k;
    }
    Ok(())
}

/// Read `n` bytes of the monitor's DisplayPort configuration (DPCD) at `addr`.
pub fn dpcd(r: &Bar0, ch: u8, addr: u32, n: usize) -> Result<Vec<u8>, &'static str> {
    let mut out = Vec::new();
    while out.len() < n {
        let k = (n - out.len()).min(16);
        let (reply, got) = aux(r, ch, 9, addr + out.len() as u32, &[], k)?;
        if reply != 0 || got.is_empty() {
            return Err("the monitor refused the DPCD read");
        }
        out.extend_from_slice(&got);
    }
    Ok(out)
}

/// The monitor's EDID (128 bytes, and the first extension if there is one), over I2C-over-AUX
/// at address 0x50, as one I2C session per block (nouveau aux.c, the DisplayPort standard):
/// address-only start, the offset, then 16-byte reads with "more to follow" (MOT, +4) set on
/// all but the last.
pub fn edid(r: &Bar0, ch: u8) -> Result<Vec<u8>, &'static str> {
    let mut out: Vec<u8> = Vec::new();
    let mut want = 128;
    while out.len() < want && out.len() < 256 {
        let _ = aux(r, ch, 0x4, 0x50, &[], 0); // start
        let (reply, _) = aux(r, ch, 0x4, 0x50, &[out.len() as u8], 1)?;
        if reply != 0 {
            let _ = aux(r, ch, 0x1, 0x50, &[], 0);
            return Err("the monitor refused the EDID offset");
        }
        let block_end = out.len() + 128;
        while out.len() < block_end {
            let last = out.len() + 16 >= block_end;
            let (reply, got) = aux(r, ch, if last { 0x1 } else { 0x5 }, 0x50, &[], 16)?;
            if reply != 0 || got.is_empty() {
                let _ = aux(r, ch, 0x1, 0x50, &[], 0);
                return Err("the monitor refused the EDID read");
            }
            out.extend_from_slice(&got);
        }
        if out.len() == 128 && out.get(126).copied().unwrap_or(0) > 0 {
            want = 256;
        }
    }
    if out.len() < 128 || out[..8] != [0, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0] {
        return Err("the EDID isn't valid");
    }
    Ok(out)
}

/// The monitor's name and best mode from its EDID (descriptor 0xFC; first detailed timing).
fn parse_edid(e: &[u8]) -> (Option<String>, Option<(u32, u32, u32)>) {
    let mut name = None;
    let mut mode = None;
    for k in 0..4 {
        let d = &e[54 + k * 18..54 + k * 18 + 18];
        let clock = u16::from_le_bytes([d[0], d[1]]) as u64 * 10_000;
        if clock != 0 {
            if mode.is_none() {
                let ha = d[2] as u64 | ((d[4] as u64 >> 4) << 8);
                let hb = d[3] as u64 | ((d[4] as u64 & 15) << 8);
                let va = d[5] as u64 | ((d[7] as u64 >> 4) << 8);
                let vb = d[6] as u64 | ((d[7] as u64 & 15) << 8);
                if (ha + hb) * (va + vb) > 0 {
                    mode = Some((ha as u32, va as u32, (clock * 100 / ((ha + hb) * (va + vb))) as u32));
                }
            }
        } else if d[3] == 0xFC {
            let n: String = d[5..18].iter().take_while(|&&b| b != 0x0A).map(|&b| b as char).collect();
            if !n.trim().is_empty() {
                name = Some(String::from(n.trim()));
            }
        }
    }
    (name, mode)
}

/// Stage 2 for one connector: switch its pins to AUX if needed, then read the monitor's
/// DisplayPort capabilities and EDID.
fn talk_to_monitor(r: &Bar0, ch: u8, c: &mut Conn) {
    // Pad in AUX mode (nouveau gm200_i2c_pad_mode): only written if it isn't already.
    let pad = ch as u32 * 0x50;
    let (mode, off) = (r.rd(0x00d970 + pad), r.rd(0x00d97c + pad));
    let switched = mode & 0xc003 != 0x0002 || off & 1 != 0;
    if switched {
        kprintln!("nvidia:   {}: pins to AUX mode (were {:#x}, {:#x})", c.label, mode, off);
        r.wr(0x00d970 + pad, (mode & !0xc003) | 0x0002);
        r.wr(0x00d97c + pad, off & !1);
        spin(r, 1000);
    }
    match dpcd(r, ch, 0, 16) {
        Ok(caps) => {
            let rate = match caps[1] {
                0x06 => "1.62",
                0x0a => "2.7",
                0x14 => "5.4",
                0x1e => "8.1",
                _ => "?",
            };
            let link = format!("DisplayPort {}.{}, {} lanes at {} Gbit/s", caps[0] >> 4, caps[0] & 15, caps[2] & 0x1f, rate);
            kprintln!("nvidia:   {}: {} (DPCD {:02x?})", c.label, link, &caps[..]);
            c.link = Some(link);
        }
        Err(e) => kprintln!("nvidia:   {}: DPCD: {}", c.label, e),
    }
    match edid(r, ch) {
        Ok(e) => {
            let (name, mode) = parse_edid(&e);
            kprintln!(
                "nvidia:   {}: monitor \"{}\", best {}",
                c.label,
                name.as_deref().unwrap_or("?"),
                mode.map(|(w, h, hz)| format!("{}x{} at {}.{:02} Hz", w, h, hz / 100, hz % 100)).unwrap_or_else(|| String::from("?"))
            );
            c.monitor = name;
            c.mode = mode;
            c.edid = e;
        }
        Err(e) => kprintln!("nvidia:   {}: EDID: {}", c.label, e),
    }
    // Leave the pins as the firmware had them, so nothing looks different to it next start.
    if switched {
        r.wr(0x00d970 + pad, mode);
        r.wr(0x00d97c + pad, off);
    }
}

// ---- stage 3a: video memory ----------------------------------------------------------------

/// Read video memory through the BAR0 PRAMIN window (1 MiB at 0x700000, positioned in 64 KiB
/// steps by 0x001700), putting the window back afterwards.
pub fn vram_read(r: &Bar0, addr: u64, len: usize) -> Vec<u8> {
    let saved = r.rd(0x001700);
    let mut out = Vec::with_capacity(len);
    let mut at = addr;
    while out.len() < len {
        let base = at & !0xffff;
        r.wr(0x001700, (base >> 16) as u32);
        let mut off = (at - base) as u32;
        while off < 0x10_0000 && out.len() < len {
            out.extend_from_slice(&r.rd(0x700000 + off).to_le_bytes());
            off += 4;
        }
        at = base + 0x10_0000;
    }
    r.wr(0x001700, saved);
    out.truncate(len);
    out
}

/// Write video memory through the PRAMIN window (32-bit words), putting the window back.
pub fn vram_write(r: &Bar0, addr: u64, words: &[u32]) {
    let saved = r.rd(0x001700);
    let mut at = addr;
    let mut k = 0;
    while k < words.len() {
        let base = at & !0xffff;
        r.wr(0x001700, (base >> 16) as u32);
        let mut off = (at - base) as u32;
        while off < 0x10_0000 && k < words.len() {
            r.wr(0x700000 + off, words[k]);
            off += 4;
            k += 1;
        }
        at = base + 0x10_0000;
    }
    let _ = r.rd(0x700000);
    r.wr(0x001700, saved);
}

fn le32(b: &[u8], o: usize) -> u32 {
    b.get(o..o + 4).map(|x| u32::from_le_bytes([x[0], x[1], x[2], x[3]])).unwrap_or(0)
}

/// Where the firmware's picture is: the window's context DMA ("DAVE") in the display engine's
/// object table (RAMHT: 1024 entries of handle and context at the start of its instance
/// memory, nouveau core/ramht.c; the object is flags, start >> 8, limit >> 8, nouveau
/// dma/usergv100.c). Returns the video memory read, for saving.
fn find_picture(r: &Bar0, bar1: Option<(u64, u64)>, fb: u64) -> Vec<(&'static str, u64, Vec<u8>)> {
    let mut out = Vec::new();
    if let Some((b, size)) = bar1 {
        kprintln!("nvidia: BAR1 at {:#x}, {} MiB; the firmware's picture is {:#x} into it", b, size >> 20, fb.wrapping_sub(b));
    }
    // The window's context DMA handle for the picture (core window 0, SET_CONTEXT_DMA_ISO).
    let handle = r.rd(0x690240);
    let (target, inst) = (r.rd(0x610010), (r.rd(0x610014) as u64) << 16);
    kprintln!("nvidia: display instance memory at {:#x} (target {:#x}); picture handle {:#010x}", inst, target, handle);
    let table = vram_read(r, inst, 0x10000);
    let mut found = None;
    let mut entries = 0;
    for k in 0..1024 {
        let (h, ctx) = (le32(&table, k * 8), le32(&table, k * 8 + 4));
        if h == 0 && ctx == 0 {
            continue;
        }
        entries += 1;
        let obj = ((ctx >> 9) & 0xffff) as usize;
        kprintln!("nvidia:   object table entry {}: handle {:#010x}, channel {}, object at {:#x}", k, h, ctx >> 25, obj);
        if h == handle && found.is_none() {
            found = Some(obj);
        }
    }
    kprintln!("nvidia: {} objects in the display engine's table", entries);
    if let Some(obj) = found {
        let flags = le32(&table, obj);
        let start = ((le32(&table, obj + 8) as u64) << 32 | le32(&table, obj + 4) as u64) << 8;
        let limit = ((le32(&table, obj + 16) as u64) << 32 | le32(&table, obj + 12) as u64) << 8 | 0xff;
        kprintln!(
            "nvidia: the firmware's picture: video memory {:#x} to {:#x} ({} KiB), flags {:#x} (target {})",
            start, limit, (limit - start + 1) >> 10, flags,
            match flags & 3 { 1 => "video memory", 2 => "system memory", 3 => "system memory, no snoop", _ => "?" }
        );
    }
    out.push(("NVVRAM0.BIN", inst, table));
    // BAR1's instance block and the area around it (its page tables, at the top of video memory).
    let b1 = r.rd(0x001704);
    let block = ((b1 & 0x0fff_ffff) as u64) << 12;
    kprintln!("nvidia: BAR1 instance block at {:#x} ({:#010x})", block, b1);
    if block != 0 {
        let area = block & !0xf_ffff;
        out.push(("NVVRAM1.BIN", area, vram_read(r, area, 0x10_0000)));
    }
    out
}

// ---- stage 3b: the core channel ----------------------------------------------------------------

/// F12, then T: checks for stage 3, all put back afterwards:
/// 1. BAR1's mode, and whether the PRAMIN window and BAR1 see the same video memory (the
///    firmware's picture read both ways), which tells where a second picture can go;
/// 2. the core channel: with bus mastering on (the firmware turns it off), start it with a
///    command buffer in system memory, send one command that changes nothing, check the
///    engine read it, stop it (nouveau gv100_disp_core_init/fini).
/// Returns what happened, for the screen and the log.
pub fn channel_test() -> String {
    let card = CARD.lock();
    let Some(c) = card.as_ref() else { return String::from("No NVIDIA card (or the driver is off)") };
    let r = Bar0 { base: c.bar0 };
    // 1. BAR1 and PRAMIN.
    kprintln!("nvidia: BAR1 mode {:#010x}, BAR2 {:#010x}, status {:#010x} (bit 31 = page tables in use)", r.rd(0xb80f40), r.rd(0xb80f48), r.rd(0xb80f50));
    let rows = 8usize;
    let via_pramin = vram_read(&r, 0, rows * c.pitch);
    let mut same = 0usize;
    let mut first_diff = None;
    for k in 0..via_pramin.len() / 4 {
        let cpu = unsafe { core::ptr::read_volatile((c.fb + k as u64 * 4) as *const u32) };
        if cpu == le32(&via_pramin, k * 4) {
            same += 1;
        } else if first_diff.is_none() {
            first_diff = Some((k * 4, cpu, le32(&via_pramin, k * 4)));
        }
    }
    let words = via_pramin.len() / 4;
    let identity = same == words && via_pramin.iter().any(|&b| b != 0);
    kprintln!(
        "nvidia: the picture's first {} rows read through PRAMIN at video memory 0: {} of {} words match what the CPU sees{}",
        rows, same, words,
        first_diff.map(|(o, a, b)| format!(" (first difference at {:#x}: CPU {:#010x}, PRAMIN {:#010x})", o, a, b)).unwrap_or_default()
    );
    let mut summary = String::from(if identity { "BAR1 maps video memory directly. " } else { "BAR1 and PRAMIN differ. " });

    // 2. The core channel.
    let (orig_ctl, orig_state) = (r.rd(0x6104e0), r.rd(0x610630));
    let orig_push = [r.rd(0x610b20), r.rd(0x610b24), r.rd(0x610b28), r.rd(0x610b2c)];
    kprintln!("nvidia: core channel test: control {:#x}, state {:#x}, push {:08x?}", orig_ctl, orig_state, orig_push);
    if (orig_state & 0x001f_0000) == 0x000b_0000 {
        return summary + "The core channel is already running (left alone)";
    }
    // Bus mastering lets the card read system memory (PCI command register bit 2).
    let (bus, dev, func) = c.at;
    let orig_cmd = crate::pci::read32(bus, dev, func, 0x04);
    if orig_cmd & 4 == 0 {
        crate::pci::write32(bus, dev, func, 0x04, (orig_cmd & 0xffff) | 4);
    }
    kprintln!("nvidia: PCI command {:#06x} -> {:#06x}", orig_cmd & 0xffff, crate::pci::read32(bus, dev, func, 0x04) & 0xffff);
    // A command buffer in system memory: target 3 (coherent), address >> 8.
    let push = crate::memory::Dma::new(0x1000, 0x1000);
    let phys = push.phys();
    let value = 3 | (phys >> 8);
    r.wr(0x610b24, value as u32);
    r.wr(0x610b20, (value >> 32) as u32);
    r.wr(0x610b28, 1);
    r.wr(0x610b2c, 0x40);
    r.wr(0x6104e0, orig_ctl | 0x10);
    r.wr(0x680000, 0); // PUT
    r.wr(0x6104e0, 0x13);
    let up = poll_ms(&r, 2_000_000, 2000, |r| r.rd(0x610630) & 0x001f_0000 == 0x000b_0000);
    kprintln!("nvidia: core channel {} (state {:#x}, GET {:#x}, buffer at {:#x})", if up { "running" } else { "didn't start" }, r.rd(0x610630), r.rd(0x680004), phys);
    if up {
        // One method, rewriting the value the engine already has (method 0x0218, its state at
        // 0x680218), so nothing changes; no UPDATE, so nothing is applied anyway.
        let m = 0x0218u32;
        let v = r.rd(0x680000 + m);
        unsafe {
            let p = push.ptr::<u32>();
            core::ptr::write_volatile(p, (1 << 18) | m);
            core::ptr::write_volatile(p.add(1), v);
        }
        core::sync::atomic::fence(core::sync::atomic::Ordering::SeqCst);
        r.wr(0x680000, 8);
        let read = poll_ms(&r, 2_000_000, 1000, |r| r.rd(0x680004) == 8);
        kprintln!("nvidia: core channel {} the command (GET {:#x}, state {:#x})", if read { "read" } else { "didn't read" }, r.rd(0x680004), r.rd(0x610630));
        if !read {
            kprintln!(
                "nvidia:   interrupts {:#x}, exceptions {:#x}, supervisor {:#x}, core exception {:#x} {:#x}, push readback {:#x} {:#x}",
                r.rd(0x611ec0), r.rd(0x611854), r.rd(0x611c30), r.rd(0x611020), r.rd(0x611024), r.rd(0x610b20), r.rd(0x610b24)
            );
        }
        summary += if read { "The display engine ran a command from Solon: the command path works" } else { "The core channel started but didn't read Solon's command" };
    } else {
        summary += "The core channel didn't start";
    }
    // Stop it (nouveau gv100_disp_core_fini) and put everything back as it was.
    let ctl = r.rd(0x6104e0);
    r.wr(0x6104e0, ctl & !0x10);
    poll_ms(&r, 2_000_000, 1000, |r| r.rd(0x610630) & 0x001f_0000 == 0x000b_0000);
    let ctl = r.rd(0x6104e0);
    r.wr(0x6104e0, ctl & !0x2);
    r.wr(0x610b20, orig_push[0]);
    r.wr(0x610b24, orig_push[1]);
    r.wr(0x610b28, orig_push[2]);
    r.wr(0x610b2c, orig_push[3]);
    r.wr(0x6104e0, orig_ctl);
    crate::pci::write32(bus, dev, func, 0x04, orig_cmd & 0xffff);
    kprintln!("nvidia: core channel stopped and put back (control {:#x}, state {:#x}, PCI command {:#06x})", r.rd(0x6104e0), r.rd(0x610630), crate::pci::read32(bus, dev, func, 0x04) & 0xffff);
    drop(push);
    summary
}

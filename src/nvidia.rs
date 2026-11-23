//! NVIDIA GeForce driver, stage 1: look, don't touch.
//!
//! The plan (docs/graphics.md) is a display driver for NVIDIA's "nvdisplay" engine (Volta and
//! newer: RTX 20, 30 and 40 series): any screen mode and several monitors. Unlike 3D, the
//! display engine works without NVIDIA's signed firmware, which is how the open nouveau
//! driver drove these cards' screens. Register offsets here follow nouveau (MIT licence,
//! drivers/gpu/drm/nouveau/nvkm in Linux), credited with each.
//!
//! Stage 1 only reads: the chip and video memory, the display engine's size, what each head
//! shows and through which output, and from the card's VBIOS tables every connector and
//! whether a monitor is plugged into it. Reading the VBIOS moves the BAR0 "PRAMIN" window over
//! its copy in video memory and back (as nouveau does), which doesn't affect the picture.
//! Everything goes to the log, and the VBIOS and a snapshot of the display registers can be
//! saved to the Solon stick (F12, then S) to build the next stage from. Boot option
//! "nonvidia" skips it.

use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

/// What stage 1 found, kept for Settings and for saving to the stick.
pub struct Report {
    /// One line for Settings ("GA102, 10 GiB, 4 heads").
    pub summary: String,
    /// Connectors from the VBIOS: name and whether a monitor is plugged in (None = unknown).
    pub connectors: Vec<(String, Option<bool>)>,
    pub vbios: Vec<u8>,
    /// Display registers: for each range, start and count (u32 each), then the values.
    pub regs: Vec<u8>,
}

pub static REPORT: Mutex<Option<Report>> = Mutex::new(None);

struct Bar0 {
    base: u64,
}

impl Bar0 {
    fn rd(&self, off: u32) -> u32 {
        unsafe { core::ptr::read_volatile((self.base + off as u64) as *const u32) }
    }
    fn wr(&self, off: u32, v: u32) {
        unsafe { core::ptr::write_volatile((self.base + off as u64) as *mut u32, v) }
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
const SNAPSHOT: [(u32, u32); 9] = [
    (0x000000, 0x40),   // PMC: chip identification
    (0x021200, 0x40),   // GPIO lines (bit 14: the pin's level)
    (0x610000, 0x100),  // display: capabilities, counts
    (0x6254e0, 0x10),   // display ownership
    (0x625f00, 0x10),   // VBIOS image location
    (0x616000, 0x800),  // heads: capabilities, raster generator
    (0x61c000, 0x800),  // SORs (outputs): capabilities, links
    (0x640000, 0x400),  // capabilities the driver publishes to the engine
    (0x680000, 0x4000), // core channel state: assembly and armed (what's on screen)
];

/// Run stage 1 on an NVIDIA card (BAR0 = its register window).
pub fn probe(bar0: (u64, u64)) {
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
    let connectors = if vbios.is_empty() { Vec::new() } else { connectors(&r, &vbios) };
    let plugged = connectors.iter().filter(|c| c.1 == Some(true)).count();
    if !connectors.is_empty() {
        kprintln!("nvidia: {} connectors, monitors plugged into {}", connectors.len(), plugged);
    }

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
    *REPORT.lock() = Some(Report { summary, connectors, vbios, regs });
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
fn connectors(r: &Bar0, b: &[u8]) -> Vec<(String, Option<bool>)> {
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
        kprintln!("nvidia:   output entry {}: {} on connector {}, output resources {:#x}, heads {:#x}", i, kind, (d >> 12) & 0xf, (d >> 24) & 0xf, (d >> 8) & 0xf);
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
        kprintln!(
            "nvidia:   {} (type {:#04x}, entry {}): {}",
            label, ty, k,
            match state {
                Some(true) => "monitor plugged in",
                Some(false) => "nothing plugged in",
                None => "can't tell",
            }
        );
        out.push((label, state));
    }
    out
}

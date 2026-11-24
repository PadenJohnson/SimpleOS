//! Stage 3c: light another monitor with the card's own display engine (RTX 30 series, GA10x),
//! when Solon starts (unless Settings say to use the main screen only), or from the
//! diagnostics screen (F12, then L).
//!
//! Ported from nouveau (MIT licence), in nouveau's order:
//! 1. Pick a DisplayPort monitor the firmware left dark (one that answers on AUX), a free head,
//!    its first window and a free output resource (SOR) (nvkm/engine/disp/outp.c).
//! 2. Set up the display engine as tu102_disp_init does, with Solon's own instance memory in
//!    video memory: an object table (core/ramht.c) and a memory handle for the picture
//!    (dma/usergv100.c). The video memory is tested first and written through BAR1: on the
//!    RTX 3080, the table written through the PRAMIN window at 256 MiB read back as
//!    0xbad0acXX, and the window channel stalled (most likely on the picture's handle).
//! 3. Start the core, window and window-immediate channels (gv100.c), their command buffers in
//!    system memory.
//! 4. Route the SOR to the connector's pads and train the DisplayPort link (dp.c, ga102.c,
//!    gm200.c, gm107.c, g94.c, the VBIOS's scripts and nouveau_dp.c's choice of link).
//! 5. Core channel: the output, the head's mode and window ownership, then UPDATE
//!    (dispnv50 corec57d.c, headc57d.c, headc37d.c, sorc37d.c, disp.c); answer the engine's three
//!    "supervisor" requests: pixel clock (devinit/ga100.c, clk/pllgt215.c), the VBIOS's output
//!    scripts and DisplayPort stream settings (nv50.c, gv100.c).
//! 6. Draw a picture into video memory (through BAR1) and point the window at it (wndwc67e.c,
//!    wndwc57e.c, wndwc37e.c, wimmc37b.c).
//! The firmware's monitor (head 0, its window and output) isn't touched. Everything goes to the
//! log, so F12 then S saves what happened.

use super::bios::{self, Bios, DcbOut, DpOut, Init};
use super::{Bar0, CARD, REPORT};
use crate::memory::Dma;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::sync::atomic::{fence, Ordering::SeqCst};
use spin::Mutex;

/// Solon's block of video memory (chosen and tested at run time): the display engine's
/// instance memory (64 KiB), the window's input LUT and the head's output LUT after it, and
/// the picture 1 MiB in.
const ILUT_AT: u64 = 0x1_0000;
const OLUT_AT: u64 = 0x1_4000;
const LUT_BYTES: usize = 0x2100;
const PIC_AT: u64 = 0x10_0000;
/// Object handles: the picture's memory (nouveau NV50_DISP_HANDLE_WNDW_CTX(0)), and the
/// firmware's own ("DAVE"), added back so the picture it shows keeps resolving.
const H_VRAM: u32 = 0xfb00_0000;
const H_FIRMWARE: u32 = 0x4556_4144;
/// The client number in object table entries, as nouveau writes it (and gives each channel
/// at 0x610b2c).
const CLIENT: u32 = 0x40;

/// A monitor Solon lit, kept for drawing to it later.
#[allow(dead_code)]
pub struct Lit {
    /// The monitor's name, from its EDID.
    pub name: String,
    pub head: u32,
    pub sor: u32,
    pub w: u32,
    pub h: u32,
    pub pitch: u32,
    /// The picture as the CPU sees it (write-combined, through BAR1).
    pub virt: u64,
    chans: Vec<Chan>,
}

pub static LIT: Mutex<Option<Lit>> = Mutex::new(None);

/// The second monitor's picture, once lit: (CPU address, width, height, bytes a row). Its
/// pixels are A8R8G8B8.
pub fn second_screen() -> Option<(u64, u32, u32, u32)> {
    LIT.lock().as_ref().map(|l| (l.virt, l.w, l.h, l.pitch))
}

/// The lit monitor's name.
pub fn second_name() -> Option<String> {
    LIT.lock().as_ref().map(|l| l.name.clone())
}

/// A monitor plugged into the card that nothing shows a picture on yet (more are plugged in
/// than the firmware lit): its name and best resolution, for Settings and for lighting it at
/// start.
pub fn waiting() -> Option<(String, (u32, u32))> {
    if LIT.lock().is_some() || MODE_SET.load(core::sync::atomic::Ordering::Relaxed) || !super::present() {
        return None;
    }
    let rep = REPORT.lock();
    let plugged: Vec<&super::Conn> = rep.as_ref()?.connectors.iter().filter(|c| c.plugged == Some(true)).collect();
    if plugged.len() <= crate::display::screens() {
        return None;
    }
    // The firmware's monitor is the one it told Solon about; the waiting one is another.
    let first = crate::display::INFO.lock().as_ref().and_then(|i| i.monitor.clone());
    let c = plugged.iter().rev().find(|c| c.monitor.is_some() && c.monitor != first).or(plugged.last())?;
    let name = c.monitor.clone().unwrap_or_else(|| format!("The monitor on {}", c.label));
    Some((name, c.mode.map_or((0, 0), |m| (m.0, m.1))))
}

/// A try got as far as setting the monitor's mode; the head keeps it, so another try in the
/// same session finds no dark monitor. Say so instead.
static MODE_SET: core::sync::atomic::AtomicBool = core::sync::atomic::AtomicBool::new(false);

/// A display channel and its command buffer.
struct Chan {
    name: &'static str,
    /// Control index: 0 core, 1 + window, 33 + window (immediate).
    ctrl: u32,
    push: Dma,
    put: usize,
    running: bool,
}

impl Chan {
    fn new(name: &'static str, ctrl: u32) -> Chan {
        Chan { name, ctrl, push: Dma::new(0x1000, 0x1000), put: 0, running: false }
    }

    /// The channel's user area: PUT, then GET.
    fn user(&self) -> u32 {
        if self.ctrl == 0 {
            0x680000
        } else {
            0x690000 + (self.ctrl - 1) * 0x1000
        }
    }

    /// Start it (nouveau gv100_disp_core_init / gv100_disp_dmac_init).
    fn start(&mut self, r: &Bar0) -> Result<(), String> {
        let (poff, coff) = (self.ctrl * 0x10, self.ctrl * 4);
        let value = 3 | (self.push.phys() >> 8); // target 3: system memory, coherent
        r.wr(0x610b24 + poff, value as u32);
        r.wr(0x610b20 + poff, (value >> 32) as u32);
        r.wr(0x610b28 + poff, 1);
        r.wr(0x610b2c + poff, 0x40);
        r.mask(0x6104e0 + coff, 0x10, 0x10);
        r.wr(self.user(), 0);
        r.wr(0x6104e0 + coff, 0x13);
        self.put = 0;
        let ok = self.idle(r);
        kprintln!("nvidia:   {} channel {} (state {:#x})", self.name, if ok { "running" } else { "didn't start" }, self.state(r));
        self.running = ok;
        if ok {
            Ok(())
        } else {
            Err(format!("the {} channel didn't start", self.name))
        }
    }

    fn state(&self, r: &Bar0) -> u32 {
        r.rd(self.state_reg())
    }

    /// The channel's status register (NV_PDISP_FE_CHNSTATUS_CORE/WIN/WINIM).
    fn state_reg(&self) -> u32 {
        if self.ctrl == 0 {
            0x610630
        } else {
            0x610664 + (self.ctrl - 1) * 4
        }
    }

    /// nouveau gv100_disp_core_idle / gv100_disp_dmac_idle.
    fn idle(&self, r: &Bar0) -> bool {
        let core = self.ctrl == 0;
        super::poll_ms(r, 50_000_000, 2000, |r| {
            if core {
                r.rd(0x610630) & 0x001f_0000 == 0x000b_0000
            } else {
                r.rd(0x610664 + (self.ctrl - 1) * 4) & 0x000f_0000 == 0x0004_0000
            }
        })
    }

    /// Stop it (nouveau gv100_disp_core_fini / gv100_disp_dmac_fini).
    fn stop(&mut self, r: &Bar0) {
        if !self.running {
            return;
        }
        let coff = self.ctrl * 4;
        r.mask(0x6104e0 + coff, 0x10, 0);
        self.idle(r);
        r.mask(0x6104e0 + coff, 0x2, 0);
        self.running = false;
        kprintln!("nvidia:   {} channel stopped (state {:#x})", self.name, self.state(r));
    }

    /// Queue a method and its values (one header, incrementing methods). When the buffer is
    /// full, wait for the engine to catch up and jump back to the start (nouveau
    /// nv50_dmac_wind).
    fn mthd(&mut self, r: &Bar0, m: u32, data: &[u32]) -> Result<(), String> {
        let bytes = 4 * (1 + data.len());
        if self.put + bytes + 4 > self.push.len() {
            self.kick(r);
            if !self.wait(r, 2000) {
                return Err(format!("the {} channel stopped reading commands", self.name));
            }
            self.write(self.put, 0x2000_0000); // JUMP to 0
            self.put = 0;
        }
        self.write(self.put, (data.len() as u32) << 18 | m);
        for (k, &d) in data.iter().enumerate() {
            self.write(self.put + 4 + 4 * k, d);
        }
        self.put += bytes;
        Ok(())
    }

    fn write(&self, at: usize, v: u32) {
        unsafe { core::ptr::write_volatile(self.push.ptr::<u32>().add(at / 4), v) }
    }

    fn kick(&self, r: &Bar0) {
        fence(SeqCst);
        r.wr(self.user(), self.put as u32);
    }

    /// Wait until the engine has read everything queued.
    fn wait(&self, r: &Bar0, ms: u64) -> bool {
        let (user, put) = (self.user(), self.put as u32);
        super::poll_ms(r, 50_000_000, ms, |r| r.rd(user + 4) == put)
    }
}

/// A display mode (nouveau nv50_head_mode is computed from this).
#[derive(Clone, Copy, Debug)]
struct Mode {
    khz: u32,
    hdisplay: u32,
    hsync_start: u32,
    hsync_end: u32,
    htotal: u32,
    vdisplay: u32,
    vsync_start: u32,
    vsync_end: u32,
    vtotal: u32,
    nhsync: bool,
    nvsync: bool,
}

impl Mode {
    /// Refresh rate in 1/100 Hz.
    fn refresh(&self) -> u32 {
        (self.khz as u64 * 100_000 / (self.htotal as u64 * self.vtotal as u64).max(1)) as u32
    }
}

/// The detailed timings in an EDID (base block and a CTA extension), interlaced ones left out.
fn edid_modes(e: &[u8]) -> Vec<Mode> {
    let mut out = Vec::new();
    let mut dtd = |d: &[u8]| {
        let khz = u16::from_le_bytes([d[0], d[1]]) as u32 * 10;
        if khz == 0 || d[17] & 0x80 != 0 {
            return;
        }
        let ha = d[2] as u32 | (d[4] as u32 >> 4) << 8;
        let hb = d[3] as u32 | (d[4] as u32 & 15) << 8;
        let va = d[5] as u32 | (d[7] as u32 >> 4) << 8;
        let vb = d[6] as u32 | (d[7] as u32 & 15) << 8;
        let hso = d[8] as u32 | ((d[11] as u32 >> 6) & 3) << 8;
        let hsw = d[9] as u32 | ((d[11] as u32 >> 4) & 3) << 8;
        let vso = (d[10] as u32 >> 4) | ((d[11] as u32 >> 2) & 3) << 4;
        let vsw = (d[10] as u32 & 15) | (d[11] as u32 & 3) << 4;
        // Digital separate sync: bit 1 = +hsync, bit 2 = +vsync.
        let separate = d[17] & 0x18 == 0x18;
        let m = Mode {
            khz,
            hdisplay: ha,
            hsync_start: ha + hso,
            hsync_end: ha + hso + hsw,
            htotal: ha + hb,
            vdisplay: va,
            vsync_start: va + vso,
            vsync_end: va + vso + vsw,
            vtotal: va + vb,
            nhsync: separate && d[17] & 2 == 0,
            nvsync: separate && d[17] & 4 == 0,
        };
        if ha > 0 && va > 0 && m.hsync_end <= m.htotal && m.vsync_end <= m.vtotal {
            out.push(m);
        }
    };
    if e.len() >= 128 {
        for k in 0..4 {
            dtd(&e[54 + k * 18..72 + k * 18]);
        }
    }
    if e.len() >= 256 && e[128] == 2 {
        let mut o = e[130] as usize;
        while o >= 4 && o + 18 <= 127 && (e[128 + o] | e[129 + o]) != 0 {
            dtd(&e[128 + o..146 + o]);
            o += 18;
        }
    }
    out
}

/// Whether the monitor takes audio (EDID CTA extension: basic audio, or an audio block), which
/// nouveau uses to choose an output resource with audio.
fn edid_audio(e: &[u8]) -> bool {
    if e.len() < 256 || e[128] != 2 {
        return false;
    }
    if e[131] & 0x40 != 0 {
        return true;
    }
    let end = (e[130] as usize).min(127);
    let mut o = 4;
    while o < end {
        let (tag, len) = (e[128 + o] >> 5, (e[128 + o] & 31) as usize);
        if tag == 1 {
            return true;
        }
        o += 1 + len;
    }
    false
}

/// DisplayPort link training state (nouveau struct lt_state).
#[derive(Default)]
struct Lt {
    stat: [u8; 6],
    conf: [u8; 4],
    pc2: bool,
    pc2stat: u8,
    pc2conf: [u8; 2],
}

/// What was changed, so a failure can put things back.
#[derive(Default)]
struct Undo {
    pci_cmd: Option<u32>,
    regs: Vec<(u32, u32)>,
    routed: bool,
    trained: bool,
    updated: bool,
}

struct Light<'a, 'b> {
    r: &'a Bar0,
    bios: &'a Bios<'b>,
    at: (u8, u8, u8),
    vram: u64,
    /// The chosen monitor's output path, its DP table entry and AUX channel.
    outp: DcbOut,
    dpout: DpOut,
    aux: u8,
    dpcd: [u8; 16],
    head: u32,
    wndw: u32,
    sor: u32,
    /// BAR1 (physical): video memory as the CPU sees it (mapped one to one).
    bar1: u64,
    /// Solon's block of video memory: instance memory and input LUT, picture at PIC_AT.
    inst: u64,
    /// The trained link: rate (0.27 Gbit/s units), lanes, enhanced framing.
    bw: u8,
    nr: u8,
    ef: bool,
    lt: Lt,
    mode: Mode,
    /// The monitor's name, from its EDID.
    name: String,
    chans: Vec<Chan>,
    undo: Undo,
}

/// Light a DisplayPort monitor the firmware left dark (at start, or F12 then L). Returns what
/// happened.
pub fn light_second() -> String {
    if LIT.lock().is_some() {
        return String::from("Solon already lit a second monitor");
    }
    if MODE_SET.load(core::sync::atomic::Ordering::Relaxed) {
        return String::from("The first try already set up the second monitor; restart the PC to try again (press S to save the first try's log)");
    }
    let card = CARD.lock();
    let Some(c) = card.as_ref() else { return String::from("No NVIDIA card (or the driver is off)") };
    if !(0x172..=0x177).contains(&c.chipset) {
        return format!("Lighting more monitors works on RTX 30-series cards (GA10x) so far; this card is chipset {:#x}", c.chipset);
    }
    let vbios = REPORT.lock().as_ref().map(|r| r.vbios.clone()).unwrap_or_default();
    if vbios.is_empty() {
        return String::from("The card's VBIOS couldn't be read, so its output scripts aren't available");
    }
    let Some((bar1, bar1_size)) = c.bar1 else { return String::from("The card's video memory window (BAR1) wasn't found") };
    let r = Bar0 { base: c.bar0 };
    let bios = Bios { b: &vbios };
    kprintln!("nvidia: lighting another monitor (stage 3c)");
    let mut l = match Light::choose(&r, &bios, c.at, c.vram_mb as u64 * 1024 * 1024) {
        Ok(l) => l,
        Err(e) => {
            kprintln!("nvidia: {}", e);
            return e;
        }
    };
    let result = l.light(bar1, bar1_size);
    match result {
        Ok((msg, lit)) => {
            kprintln!("nvidia: {}", msg);
            *LIT.lock() = Some(lit);
            msg
        }
        Err(e) => {
            kprintln!("nvidia: lighting failed: {}", e);
            if l.undo.updated {
                MODE_SET.store(true, core::sync::atomic::Ordering::Relaxed);
            }
            l.put_back();
            format!("Couldn't light the monitor: {}", e)
        }
    }
}

impl<'a, 'b> Light<'a, 'b> {
    /// Step 1: the monitor, head, window and output resource.
    fn choose(r: &'a Bar0, bios: &'a Bios<'b>, at: (u8, u8, u8), vram: u64) -> Result<Light<'a, 'b>, String> {
        let (m0, cnt) = (r.rd(0x610060), r.rd(0x610074));
        let (heads, sors) = (m0 & 0xff, (m0 >> 8) & 0xff);
        let wndws = r.rd(0x610064);
        let nsor = (cnt >> 8) & 0xf;
        // A dark head (its armed raster and pixel clock are 0) whose first window exists.
        let head = (0..8)
            .filter(|&h| heads & (1 << h) != 0 && wndws & (1 << (h * 2)) != 0)
            .find(|&h| r.rd(0x68a064 + h * 0x400) == 0 || r.rd(0x68a00c + h * 0x400) == 0)
            .ok_or_else(|| String::from("every head is in use"))?;
        let wndw = head * 2;
        // Which SOR drives which head (armed), and which pad link each DP output is routed to.
        let armed_heads = |s: u32| r.rd(0x688300 + s * 0x20) & 0xff;
        for s in 0..nsor {
            kprintln!("nvidia:   output {}: armed {:#x}, assembly {:#x}", s, r.rd(0x688300 + s * 0x20), r.rd(0x680300 + s * 0x20));
        }
        for m in 0..8u32 {
            kprintln!("nvidia:   pad link {} ({}): route {:#x}", m / 2, if m & 1 == 0 { "A" } else { "B" }, r.rd(0x612308 + m * 0x80));
        }
        let outs = bios.dcb_outputs();
        // A DP output whose monitor answers on AUX and that no head is showing.
        let mut chosen = None;
        for o in outs.iter().filter(|o| o.ty == 6 && o.location == 0) {
            let Some(aux) = bios.aux_channel(o.i2c_index) else { continue };
            if let Some(s) = route_get(r, o) {
                if s < 8 && armed_heads(s) != 0 {
                    kprintln!("nvidia:   DCB output {} (connector {}) is lit by the firmware through output {}", o.index, o.connector, s);
                    continue;
                }
            }
            aux_pad_on(r, aux);
            // A monitor asleep since start can take a while to answer on AUX at all (the LG
            // UltraGear needed a second try): keep asking a plugged-in one for a second or so.
            let plugged = REPORT.lock().as_ref().is_some_and(|rep| rep.connectors.iter().any(|c| c.aux == Some(aux) && c.plugged == Some(true)));
            let mut answer = super::dpcd(r, aux, 0, 16);
            let mut tries = 1;
            while plugged && !matches!(&answer, Ok(d) if d[0] != 0) && tries < 15 {
                super::mdelay(100);
                answer = super::dpcd(r, aux, 0, 16);
                tries += 1;
            }
            if tries > 1 {
                kprintln!("nvidia:   DCB output {} (AUX {}): asked {} times", o.index, aux, tries);
            }
            match answer {
                Ok(d) if d[0] != 0 => {
                    kprintln!("nvidia:   DCB output {} (connector {}, AUX {}): a monitor answers (DPCD {:02x?})", o.index, o.connector, aux, &d[..]);
                    let mut dpcd = [0u8; 16];
                    dpcd.copy_from_slice(&d[..16]);
                    chosen = Some((*o, aux, dpcd));
                    break;
                }
                _ => kprintln!("nvidia:   DCB output {} (connector {}, AUX {}): no monitor answers", o.index, o.connector, aux),
            }
        }
        let (outp, aux, dpcd) = chosen.ok_or_else(|| String::from("no dark DisplayPort monitor answered"))?;
        let dpout = bios
            .dpout_match(outp.hasht, outp.hashm)
            .ok_or_else(|| format!("the VBIOS has no DisplayPort data for output {} ({:#x}:{:#x})", outp.index, outp.hasht, outp.hashm))?;
        kprintln!("nvidia:   DP table {:#x}: scripts {:04x?}, link-rate scripts {:#x}, flags {:#x}", dpout.addr, dpout.script, dpout.lnkcmp, dpout.flags);
        // The monitor's modes; the largest at no more than 61 Hz that the link can carry.
        let e = super::edid(r, aux).map_err(|e| format!("the monitor's EDID: {}", e))?;
        let max_bw = dpcd[1].min(outp.dp_bw);
        let max_nr = (dpcd[2] & 0x1f).min(outp.dp_nr);
        let capacity = max_bw as u64 * 27_000 * max_nr as u64; // kB/s
        let modes = edid_modes(&e);
        for m in &modes {
            kprintln!("nvidia:   mode {}x{} at {}.{:02} Hz ({} kHz)", m.hdisplay, m.vdisplay, m.refresh() / 100, m.refresh() % 100, m.khz);
        }
        let mode = modes
            .iter()
            .filter(|m| m.refresh() <= 6100 && (m.khz as u64 * 24 + 7) / 8 <= capacity)
            .max_by_key(|m| (m.hdisplay * m.vdisplay, m.refresh()))
            .or_else(|| modes.iter().min_by_key(|m| m.khz))
            .copied()
            .ok_or_else(|| String::from("the monitor's EDID lists no usable mode"))?;
        // The output resource, as nouveau chooses one: free (no head), with audio if the
        // monitor takes audio.
        let hda_caps = r.rd(0x08a15c);
        let want_hda = edid_audio(&e);
        let free = |s: &u32| sors & (1 << s) != 0 && armed_heads(*s) == 0;
        let sor = (0..nsor)
            .filter(free)
            .find(|&s| (hda_caps & (1 << s) != 0) == want_hda)
            .or_else(|| (0..nsor).find(free))
            .ok_or_else(|| String::from("no free output resource"))?;
        kprintln!(
            "nvidia:   chose head {}, window {}, output {} (audio outputs {:#x}, monitor audio {}), {}x{} at {}.{:02} Hz",
            head, wndw, sor, hda_caps & 0xff, want_hda, mode.hdisplay, mode.vdisplay, mode.refresh() / 100, mode.refresh() % 100
        );
        Ok(Light {
            r,
            bios,
            at,
            vram,
            outp,
            dpout,
            aux,
            dpcd,
            head,
            wndw,
            sor,
            bar1: 0,
            inst: 0,
            bw: 0,
            nr: 0,
            ef: false,
            lt: Lt::default(),
            mode,
            name: super::parse_edid(&e).0.unwrap_or_else(|| String::from("Second monitor")),
            chans: Vec::new(),
            undo: Undo::default(),
        })
    }

    /// The sub-link the output uses on its SOR: 1 = A, 2 = B (nouveau ior->asy.link).
    fn link(&self) -> u32 {
        self.outp.link as u32
    }
    fn soff(&self) -> u32 {
        self.sor * 0x800
    }
    /// nouveau nv50_sor_link.
    fn loff(&self) -> u32 {
        self.soff() + if self.link() == 2 { 0x80 } else { 0 }
    }

    fn init(&self, or: i32, link: i32, head: i32) -> Init<'a, 'b> {
        Init::new(self.r, self.bios, Some(self.outp), Some(self.aux), or, link, head)
    }

    /// Run a VBIOS script for the output (on its SOR and sub-link).
    fn script(&self, addr: u16, what: &str) -> Result<(), String> {
        self.init(self.sor as i32, self.link() as i32, -1).run(addr, what)
    }

    /// Write a register, remembering its old value to put back on failure.
    fn set(&mut self, reg: u32, v: u32) {
        let old = self.r.rd(reg);
        if !self.undo.regs.iter().any(|&(r, _)| r == reg) {
            self.undo.regs.push((reg, old));
        }
        self.r.wr(reg, v);
    }

    fn light(&mut self, bar1: u64, bar1_size: u64) -> Result<(String, Lit), String> {
        let r = self.r;
        let m = self.mode;
        let (w, h) = (m.hdisplay, m.vdisplay);
        let pitch = (w * 4 + 255) & !255;
        let size = pitch as u64 * h as u64;
        self.bar1 = bar1;
        self.choose_memory(bar1_size, size)?;
        // PCI: bus mastering on (the channels read their commands from system memory), and
        // the card's legacy interrupt off (Solon polls).
        let (bus, dev, func) = self.at;
        let cmd = crate::pci::read32(bus, dev, func, 0x04);
        self.undo.pci_cmd = Some(cmd & 0xffff);
        crate::pci::write32(bus, dev, func, 0x04, (cmd & 0xffff) | 0x4 | 0x400);

        // The object table first, then point the engine at it.
        self.instance_memory()?;
        self.disp_init();
        // Step 3: channels.
        let mut core = Chan::new("core", 0);
        let mut wndw = Chan::new("window", 1 + self.wndw);
        let mut wimm = Chan::new("window-immediate", 33 + self.wndw);
        let started = core.start(r).and_then(|_| wndw.start(r)).and_then(|_| wimm.start(r));
        self.chans.push(core);
        self.chans.push(wndw);
        self.chans.push(wimm);
        started?;
        self.log_window_state();

        // Step 4: the link.
        self.sor_power();
        self.acquire()?;
        self.train()?;

        // Step 5: the mode.
        self.modeset()?;
        self.supervise()?;
        let raster = r.rd(0x68a064 + self.head * 0x400);
        let sor_ctrl = r.rd(0x688300 + self.sor * 0x20);
        kprintln!("nvidia:   armed: head {} raster {:#x}, output {} control {:#x}", self.head, raster, self.sor, sor_ctrl);
        if raster != (m.vtotal << 16 | m.htotal) || sor_ctrl & (1 << self.head) == 0 {
            return Err(String::from("the display engine didn't take the new mode"));
        }

        // Step 6: the picture.
        let pic = self.inst + PIC_AT;
        let virt = crate::memory::map_wc(bar1 + pic, size);
        draw_test_picture(virt, w, h, pitch, &self.describe());
        // Spot checks that the picture landed (rows at the top, middle and bottom).
        let wrong = [0u64, h as u64 / 2, h as u64 - 1]
            .iter()
            .filter(|&&y| {
                let p = (virt + y * pitch as u64 + 8) as *mut u32;
                unsafe {
                    let v = core::ptr::read_volatile(p);
                    core::ptr::write_volatile(p, v ^ 0x00ff_ffff);
                    let back = core::ptr::read_volatile(p);
                    core::ptr::write_volatile(p, v);
                    back != v ^ 0x00ff_ffff
                }
            })
            .count();
        kprintln!(
            "nvidia:   picture at video memory {:#x} ({} KiB, pitch {}), mapped {:?}, {} of 3 spot checks wrong",
            pic, size >> 10, pitch, crate::memory::caching_of(virt), wrong
        );
        let needs_olut = self.show_window(w, h, pitch)?;
        let olut = if !needs_olut {
            ""
        } else {
            match self.output_lut() {
                Ok(()) => "",
                Err(e) => {
                    kprintln!("nvidia:   output LUT: {}", e);
                    " (without its output LUT)"
                }
            }
        };
        let vpll = r.rd(0xef18 + self.head * 0x40);
        let msg = format!(
            "Lit the second monitor{}: {}x{} at {}.{:02} Hz on head {} through output {}, DisplayPort {} lanes at {}.{} Gbit/s (pixel clock {:#x})",
            olut, w, h, m.refresh() / 100, m.refresh() % 100, self.head, self.sor, self.nr, self.bw as u32 * 27 / 100, self.bw as u32 * 27 / 10 % 10, vpll
        );
        let chans = core::mem::take(&mut self.chans);
        Ok((msg, Lit { name: self.name.clone(), head: self.head, sor: self.sor, w, h, pitch, virt, chans }))
    }

    /// One line about the monitor and link, for the test picture.
    fn describe(&self) -> String {
        format!(
            "{}x{} at {}.{:02} Hz  |  head {}, output {}  |  DisplayPort, {} lanes at {}.{} Gbit/s",
            self.mode.hdisplay, self.mode.vdisplay, self.mode.refresh() / 100, self.mode.refresh() % 100, self.head, self.sor, self.nr,
            self.bw as u32 * 27 / 100, self.bw as u32 * 27 / 10 % 10
        )
    }

    /// Step 2a: nouveau tu102_disp_init (the firmware did most of this already; differences
    /// are logged), with Solon's instance memory.
    fn disp_init(&mut self) {
        let r = self.r;
        let (m0, cnt) = (r.rd(0x610060), r.rd(0x610074));
        let (heads, nsor, wmask, nwndw) = (m0 & 0xff, (cnt >> 8) & 0xf, r.rd(0x610064), (cnt >> 20) & 0x3f);
        if r.rd(0x6254e8) & 2 != 0 {
            r.mask(0x6254e8, 1, 0);
            let ok = super::poll_ms(r, 50_000_000, 2000, |r| r.rd(0x6254e8) & 2 == 0);
            kprintln!("nvidia:   took the display from the firmware: {}", ok);
        }
        let mut writes: Vec<(u32, u32)> = Vec::from([(0x640008, 0x21)]);
        let mut caps0 = r.rd(0x640000);
        for i in 0..nsor {
            caps0 |= 0x100 << i;
            writes.push((0x640144 + i * 8, r.rd(0x61c000 + i * 0x800)));
        }
        writes.push((0x640000, caps0));
        for id in (0..8).filter(|h| heads & (1 << h) != 0) {
            writes.push((0x640048 + id * 0x20, r.rd(0x616300 + id * 0x800)));
            for j in 0..5 {
                writes.push((0x640680 + id * 0x20 + j * 4, r.rd(0x616140 + id * 0x800 + j * 4)));
            }
        }
        let mut caps4 = r.rd(0x640004);
        for i in 0..nwndw {
            caps4 |= 1 << i;
            for j in 0..6 {
                writes.push((0x640780 + i * 0x20 + j * 4, r.rd(0x630100 + i * 0x800 + j * 4)));
            }
        }
        writes.push((0x640004, caps4));
        if nwndw > 0 {
            writes.push((0x64000c, r.rd(0x64000c) | 0x100));
        }
        for i in 0..3 {
            writes.push((0x640010 + i * 4, r.rd(0x62e000 + i * 4)));
        }
        writes.push((0x610078, r.rd(0x610078) | 1));
        let changed: Vec<String> = writes.iter().filter(|&&(a, v)| r.rd(a) != v).map(|&(a, v)| format!("{:#x}: {:#x} -> {:#x}", a, r.rd(a), v)).collect();
        kprintln!("nvidia:   display setup: {} of {} capability values differ from the firmware's{}", changed.len(), writes.len(), if changed.is_empty() { String::new() } else { format!(" ({})", changed.join(", ")) });
        for (a, v) in writes {
            self.set(a, v);
        }
        // Instance memory in video memory (target 1).
        self.set(0x610010, 0x8 | 1);
        self.set(0x610014, (self.inst >> 16) as u32);
        // Interrupt status Solon polls (CTRL_DISP: awaken, error, supervisor 1-3), exceptions
        // reported but not enabled, as nouveau.
        self.set(0x611cf0, 0x187);
        self.set(0x611db0, 0x187);
        self.set(0x611cec, heads << 16 | 1);
        self.set(0x611dac, 0);
        self.set(0x611ce8, wmask);
        self.set(0x611da8, 0);
        self.set(0x611ce4, wmask);
        self.set(0x611da4, 0);
        for id in (0..8).filter(|h| heads & (1 << h) != 0) {
            self.set(0x611cc0 + id * 4, 4);
            self.set(0x611d80 + id * 4, 0);
        }
        self.set(0x611cf4, 0);
        self.set(0x611db4, 0);
    }

    /// Step 1b: Solon's block of video memory. What the firmware's screen uses (its picture,
    /// input and output LUTs, cursor: head 0 and window 0's armed state) is kept clear, and
    /// each candidate is tested through BAR1 (a value written and read back, the old one put
    /// back) before it's used.
    fn choose_memory(&mut self, bar1_size: u64, pic_size: u64) -> Result<(), String> {
        let r = self.r;
        const MIB: u64 = 0x10_0000;
        let need = PIC_AT + pic_size;
        let (w0, h0) = (0x690800, 0x68a000);
        let fw_pic = Some((r.rd(w0 + 0x260) as u64) << 8).filter(|&a| a < self.vram).unwrap_or(0);
        let fw_size = r.rd(w0 + 0x224);
        let fw_pitch = (r.rd(w0 + 0x230) & 0x1fff) as u64 * 64;
        let fw_end = fw_pic + fw_pitch.max(0x4000) * ((fw_size >> 16) as u64).max(2160);
        let mut used: Vec<(&str, u64, u64)> = Vec::from([("picture", fw_pic, fw_end)]);
        let mut lut = |what: &'static str, handle: u32, offset: u32, len: u64| {
            if handle != 0 {
                let at = (offset as u64) << 8;
                used.push((what, at, at + len));
            }
        };
        lut("input LUT", r.rd(w0 + 0x444), r.rd(w0 + 0x448), 0x4000);
        lut("output LUT", r.rd(h0 + 0x288), r.rd(h0 + 0x28c), 0x4000);
        lut("cursor", r.rd(h0 + 0x088), r.rd(h0 + 0x090), 0x4_0000);
        kprintln!(
            "nvidia:   the firmware's screen uses: {}",
            used.iter().map(|(w, a, b)| format!("{} {:#x}..{:#x}", w, a, b)).collect::<Vec<_>>().join(", ")
        );
        self.log_fb_regions();
        let top = bar1_size.min(self.vram).saturating_sub(64 * MIB);
        let mut cands: Vec<u64> = Vec::from([((fw_end + 16 * MIB) & !(16 * MIB - 1)).max(32 * MIB), 64 * MIB, 128 * MIB, 256 * MIB, 512 * MIB, 1024 * MIB, 2048 * MIB, 4096 * MIB]);
        cands.sort_unstable();
        cands.dedup();
        let mut chosen = None;
        for base in cands {
            if base + need > top {
                continue;
            }
            if let Some((what, _, _)) = used.iter().find(|&&(_, a, b)| base < b + MIB && a < base + need + MIB) {
                kprintln!("nvidia:   video memory {:#x}: next to the firmware's {}, skipped", base, what);
                continue;
            }
            let points = [base, base + ILUT_AT, base + PIC_AT, (base + need / 2) & !3, base + need - 4];
            let mut bad = Vec::new();
            for (k, &p) in points.iter().enumerate() {
                let pattern = 0x5a00_0000 | (k as u32) << 16 | (p >> 20) as u32 & 0xffff;
                let (old, back) = self.bar1_test(p, pattern);
                if back != pattern {
                    bad.push(format!("{:#x}: wrote {:#x}, read {:#x} (was {:#x})", p, pattern, back, old));
                }
            }
            let pramin = super::vram_read(r, base, 4);
            kprintln!(
                "nvidia:   video memory {:#x}: BAR1 write test {}{}; PRAMIN reads {:02x?}",
                base, if bad.is_empty() { "passed" } else { "failed" },
                if bad.is_empty() { String::new() } else { format!(" ({})", bad.join(", ")) }, pramin
            );
            if bad.is_empty() && chosen.is_none() {
                chosen = Some(base);
            }
        }
        let base = chosen.ok_or_else(|| String::from("no video memory passed the write test"))?;
        // For the record: whether the PRAMIN window sees the same memory there (a value written
        // through BAR1, read through PRAMIN), and only then whether a PRAMIN write lands.
        let p = crate::memory::map_mmio(self.bar1 + base, 4) as *mut u32;
        let tag = (base >> 20) as u32 & 0xffff;
        let (one, two) = (0x3c3c_0000 | tag, 0xa5a5_0000 | tag);
        unsafe {
            let old = core::ptr::read_volatile(p);
            core::ptr::write_volatile(p, one);
            let seen = super::vram_read(r, base, 4);
            let seen = u32::from_le_bytes([seen[0], seen[1], seen[2], seen[3]]);
            let mut note = format!("BAR1 wrote {:#x}, PRAMIN read {:#x}", one, seen);
            if seen == one {
                super::vram_write(r, base, &[two]);
                note.push_str(&format!("; PRAMIN wrote {:#x}, BAR1 read {:#x}", two, core::ptr::read_volatile(p)));
            }
            core::ptr::write_volatile(p, old);
            kprintln!("nvidia:   using video memory {:#x}; PRAMIN check: {}", base, note);
        }
        self.inst = base;
        Ok(())
    }

    /// Write a value into video memory through BAR1 and read it back, then put the old value
    /// back. Returns (old value, value read back).
    fn bar1_test(&self, addr: u64, pattern: u32) -> (u32, u32) {
        let p = crate::memory::map_mmio(self.bar1 + addr, 4) as *mut u32;
        unsafe {
            let old = core::ptr::read_volatile(p);
            core::ptr::write_volatile(p, pattern);
            let back = core::ptr::read_volatile(p);
            core::ptr::write_volatile(p, old);
            (old, back)
        }
    }

    /// The memory controller's protected ranges, for the log (WPR2 and the MMU lock, as
    /// open-gpu-kernel-modules reads them: bits 31:4 are the address >> 12).
    fn log_fb_regions(&self) {
        let r = self.r;
        let at = |v: u32| ((v & !0xf) as u64) << 8;
        let (w2lo, w2hi, lklo, lkhi) = (r.rd(0x1fa824), r.rd(0x1fa828), r.rd(0x1fa82c), r.rd(0x1fa830));
        kprintln!(
            "nvidia:   protected video memory: WPR2 {:#x}..{:#x}, locked {:#x}..{:#x} (lock PLM {:#x}, VPR status {:#x}, local memory range {:#x})",
            at(w2lo), at(w2hi) | 0xfff, at(lklo), at(lkhi) | 0xfff, r.rd(0x1fa7c8), r.rd(0x1fa80c), r.rd(0x100ce0)
        );
        let raw: Vec<String> = (0..16).map(|k| format!("{:08x}", r.rd(0x1fa800 + k * 4))).collect();
        kprintln!("nvidia:   0x1fa800: {}", raw.join(" "));
    }

    /// Step 2b: the object table (1024 entries of handle and context, nouveau core/ramht.c)
    /// and two context DMAs covering all of video memory (target video memory, read-write,
    /// pitch: flags 0x5, nouveau gv100_dmaobj_bind), one for the new window's channel and one
    /// under the firmware's handle for window 0's (the core channel gets the first one too);
    /// then the identity LUTs nouveau gives every window and head on these chips
    /// (wndwc57e_ilut_load, headc57d_olut_load). Written through BAR1 and read back.
    fn instance_memory(&self) -> Result<(), String> {
        let mut img = alloc::vec![0u32; (OLUT_AT as usize + LUT_BYTES) / 4];
        let limit = (self.vram.max(1) - 1) >> 8;
        let objects = [(0x2000u32, 1 + self.wndw, H_VRAM), (0x2000u32, 0, H_VRAM), (0x2020u32, 1, H_FIRMWARE)];
        for &(obj, chid, handle) in &objects {
            let o = obj as usize / 4;
            img[o] = 0x5;
            img[o + 1] = 0;
            img[o + 2] = 0;
            img[o + 3] = limit as u32;
            img[o + 4] = (limit >> 32) as u32;
            // At nouveau's hash and at NVIDIA's (which also folds in the client number), in
            // case the engine looks in only one place.
            let ctx = chid << 25 | CLIENT | obj << 9;
            let mut slots = Vec::new();
            for start in [ramht_hash(chid, handle), ramht_hash_rm(chid, CLIENT, handle)] {
                let mut slot = start as usize;
                while (img[slot * 2] != 0 || img[slot * 2 + 1] != 0) && (img[slot * 2], img[slot * 2 + 1]) != (handle, ctx) {
                    slot = (slot + 1) % 1024;
                }
                img[slot * 2] = handle;
                img[slot * 2 + 1] = ctx;
                slots.push(slot);
            }
            kprintln!("nvidia:   object table slots {:?}: handle {:#010x}, channel {}, memory object at {:#x}", slots, handle, chid, obj);
        }
        // The LUTs: a 32-byte header, then 1025 entries of red, green and blue: FP16 for the
        // window's input LUT, 16-bit fractions for the head's output LUT (headc57d_olut_load).
        for i in 0..=1024usize {
            let v = (i.min(1023) * 64) as u32;
            for (at, x) in [(ILUT_AT, fp16_of_unorm16(v) as u32), (OLUT_AT, v)] {
                let o = (at as usize + 0x20 + i * 8) / 4;
                img[o] = x | x << 16;
                img[o + 1] = x;
            }
        }
        let p = crate::memory::map_wc(self.bar1 + self.inst, (img.len() * 4) as u64) as *mut u32;
        for (k, &v) in img.iter().enumerate() {
            unsafe { core::ptr::write_volatile(p.add(k), v) };
        }
        unsafe { core::arch::x86_64::_mm_sfence() };
        let wrong = img.iter().enumerate().filter(|&(k, &v)| unsafe { core::ptr::read_volatile(p.add(k)) } != v).count();
        let pramin = super::vram_read(self.r, self.inst + 0x2000, 20);
        kprintln!(
            "nvidia:   instance memory at video memory {:#x}, written through BAR1: {} of {} words read back wrong; PRAMIN sees the first object as {:02x?}",
            self.inst, wrong, img.len(), pramin
        );
        if wrong != 0 {
            return Err(format!("{} words of the display engine's object table didn't stick", wrong));
        }
        Ok(())
    }

    /// The new window's state after its channel started (whether the engine reset it, or it
    /// still holds what it had since power-on), against window 0's.
    fn log_window_state(&self) {
        let r = self.r;
        let base = 0x690000 + self.wndw * 0x1000;
        let mut diffs = Vec::new();
        for off in (0x200..0x600).step_by(4) {
            let (a, b) = (r.rd(base + off), r.rd(0x690000 + off));
            if a != b {
                diffs.push(format!("{:#x}: {:#x} (window 0: {:#x})", off, a, b));
            }
        }
        kprintln!("nvidia:   window {} state differs from window 0's in {} places{}", self.wndw, diffs.len(), if diffs.is_empty() { String::new() } else { format!(": {}", diffs.iter().take(24).cloned().collect::<Vec<_>>().join(", ")) });
    }

    /// nouveau nv50_sor_power (normal state powered up), as nvkm_disp_init does for each SOR.
    fn sor_power(&self) {
        let (r, soff) = (self.r, self.soff());
        let wait = |r: &Bar0| super::poll_ms(r, 50_000_000, 2000, |r| r.rd(0x61c004 + soff) & 0x8000_0000 == 0);
        wait(r);
        r.mask(0x61c004 + soff, 0x8000_0001, 0x8000_0001);
        wait(r);
        super::poll_ms(r, 50_000_000, 2000, |r| r.rd(0x61c030 + soff) & 0x1000_0000 == 0);
        kprintln!("nvidia:   output {} powered ({:#x}, {:#x})", self.sor, r.rd(0x61c004 + soff), r.rd(0x61c030 + soff));
    }

    /// Route the SOR to the output's pad links (nouveau nvkm_outp_init's clean-up of a link the
    /// firmware left routed, then gm200_sor_route_set).
    fn acquire(&mut self) -> Result<(), String> {
        let r = self.r;
        if let Some(s) = route_get(r, &self.outp) {
            let ctrl = r.rd(0x688300 + s * 0x20);
            if ctrl & 0xff == 0 {
                // nouveau runs the DisableLT script on it, with the link from its armed protocol.
                let link = match (ctrl >> 8) & 0xf {
                    2 | 9 => 2,
                    5 => 3,
                    _ => 1,
                };
                kprintln!("nvidia:   the firmware left this output routed to output {}: disabling that link first", s);
                Init::new(r, self.bios, Some(self.outp), Some(self.aux), s as i32, link, -1).run(self.dpout.script[4], "disable link")?;
            }
        }
        route_set(r, &self.outp, Some((self.sor, self.link())));
        self.undo.routed = true;
        kprintln!("nvidia:   routed output {} to pad link {} ({:#x})", self.sor, self.outp.or.trailing_zeros(), route_get(r, &self.outp).map(|s| s as i64).unwrap_or(-1));
        Ok(())
    }

    /// DisplayPort link training, nouveau nouveau_dp_train: wake the monitor, then the most
    /// lanes, fastest rate first, down to what the mode still fits in.
    fn train(&mut self) -> Result<(), String> {
        let min_rate = (self.mode.khz as u64 * 24 + 7) / 8;
        let pwr = super::dpcd(self.r, self.aux, 0x600, 1).map(|v| v[0]).unwrap_or(0);
        if pwr & 3 != 1 {
            let _ = super::dpcd_write(self.r, self.aux, 0x600, &[(pwr & !3) | 1]);
            kprintln!("nvidia:   monitor woken (power state {:#x})", pwr);
        }
        let max_nr = (self.dpcd[2] & 0x1f).min(self.outp.dp_nr).max(1);
        let max_bw = self.dpcd[1].min(self.outp.dp_bw);
        let rates: Vec<u8> = [0x1e, 0x14, 0x0a, 0x06].iter().copied().filter(|&b| b <= max_bw).collect();
        let mut nr = max_nr;
        while nr > 0 {
            for &bw in &rates {
                if bw as u64 * 27_000 * nr as u64 >= min_rate {
                    match self.dp_train(bw, nr) {
                        Ok(()) => {
                            kprintln!("nvidia:   link trained: {} lanes at {:#x}", nr, bw);
                            self.undo.trained = true;
                            return Ok(());
                        }
                        Err(e) => kprintln!("nvidia:   training {} lanes at {:#x} failed: {}", nr, bw, e),
                    }
                }
            }
            nr >>= 1;
        }
        Err(String::from("DisplayPort link training failed at every link rate"))
    }

    /// nouveau nvkm_dp_train (not a retrain).
    fn dp_train(&mut self, bw: u8, nr: u8) -> Result<(), String> {
        self.bw = bw;
        self.nr = nr;
        self.ef = self.dpcd[2] & 0x80 != 0;
        kprintln!("nvidia:   training {} lanes at {:#x} (enhanced framing {})", nr, bw, self.ef);
        // nvkm_dp_train_init: spread spectrum on or off, as the monitor allows.
        let spread = if self.dpcd[3] & 1 != 0 { self.dpout.script[2] } else { self.dpout.script[3] };
        self.script(spread, "spread spectrum")?;
        let r = self.train_links();
        // nvkm_dp_train_fini.
        let fini = self.script(self.dpout.script[1], "after link training");
        r.and(fini)
    }

    /// nouveau nvkm_dp_train_links (with AMPERE_IED_HACK: the before-training script table is
    /// by link rate).
    fn train_links(&mut self) -> Result<(), String> {
        let b = self.bios;
        let by_rate = |mut table: u32| -> u16 {
            for _ in 0..16 {
                if self.bw >= b.rd8(table) {
                    break;
                }
                table += 3;
            }
            b.rd16(table + 1)
        };
        if self.dpout.script[0] != 0 {
            self.script(by_rate(self.dpout.script[0] as u32), "before link training")?;
        }
        if self.dpout.lnkcmp != 0 {
            if b.dp_version() < 0x30 {
                return Err(String::from("old DP table"));
            }
            self.script(by_rate(self.dpout.lnkcmp as u32), "link rate")?;
        }
        self.sor_dp_links()?;
        self.sor_dp_power();
        self.train_link()
    }

    /// nouveau ga102_sor_dp_links.
    fn sor_dp_links(&self) -> Result<(), String> {
        let (r, soff, loff) = (self.r, self.soff(), self.loff());
        let clksor = match self.bw {
            0x06 => 0,
            0x0a => 0x40000,
            0x14 => 0x80000,
            0x1e => 0xc0000,
            0x08 => 0x100000,
            0x09 => 0x140000,
            0x0c => 0x180000,
            0x10 => 0x1c0000,
            _ => return Err(format!("link rate {:#x} not supported", self.bw)),
        };
        let mut dpctrl = ((1u32 << self.nr) - 1) << 16;
        if self.ef {
            dpctrl |= 0x4000;
        }
        r.mask(0x612300 + soff, 0x007c_0000, clksor);
        super::mdelay(40);
        r.mask(0x612300 + soff, 0x0003_0000, 0x0001_0000);
        r.mask(0x61c10c + loff, 3, 1);
        r.mask(0x61c10c + loff, 0x401f_4000, dpctrl);
        kprintln!("nvidia:     link set: clock {:#x}, control {:#x}", r.rd(0x612300 + soff), r.rd(0x61c10c + loff));
        Ok(())
    }

    /// nouveau g94_sor_dp_power.
    fn sor_dp_power(&self) {
        let (r, soff, loff) = (self.r, self.soff(), self.loff());
        r.mask(0x61c130 + loff, 0xf, (1 << self.nr) - 1);
        r.mask(0x61c034 + soff, 0x8000_0000, 0x8000_0000);
        super::poll_ms(r, 50_000_000, 2000, |r| r.rd(0x61c034 + soff) & 0x8000_0000 == 0);
    }

    /// nouveau gm107_sor_dp_pattern.
    fn sor_dp_pattern(&self, pattern: u8) {
        let data = match pattern {
            0 => 0x1010_1010,
            1 => 0x0101_0101,
            2 => 0x0202_0202,
            3 => 0x0303_0303,
            _ => 0x1b1b_1b1b,
        };
        let reg = if self.link() & 1 != 0 { 0x61c110 } else { 0x61c12c };
        self.r.mask(reg + self.soff(), 0x1f1f_1f1f, data);
    }

    /// nouveau gm200_sor_dp_drive (lanes map one to one).
    fn sor_dp_drive(&self, ln: usize, pc: u8, dc: u8, pe: u8, pu: u8) {
        let (r, loff) = (self.r, self.loff());
        let shift = ln as u32 * 8;
        let pu = (pu & 0x0f) as u32;
        let d0 = r.rd(0x61c118 + loff) & !(0xff << shift);
        let d1 = r.rd(0x61c120 + loff) & !(0xff << shift);
        let mut d2 = r.rd(0x61c130 + loff);
        if (d2 & 0xf00) < (pu << 8) || ln == 0 {
            d2 = (d2 & !0xf00) | (pu << 8);
        }
        r.wr(0x61c118 + loff, d0 | (dc as u32) << shift);
        r.wr(0x61c120 + loff, d1 | (pe as u32) << shift);
        r.wr(0x61c130 + loff, d2);
        let d3 = r.rd(0x61c13c + loff) & !(0xff << shift);
        r.wr(0x61c13c + loff, d3 | (pc as u32) << shift);
    }

    fn rdaux(&self, addr: u32, n: usize) -> Result<Vec<u8>, String> {
        super::dpcd(self.r, self.aux, addr, n).map_err(String::from)
    }
    fn wraux(&self, addr: u32, data: &[u8]) -> Result<(), String> {
        super::dpcd_write(self.r, self.aux, addr, data).map_err(String::from)
    }

    /// nouveau nvkm_dp_train_link (no repeaters).
    fn train_link(&mut self) -> Result<(), String> {
        let sink = [self.bw, self.nr | if self.ef { 0x80 } else { 0 }];
        self.wraux(0x100, &sink)?;
        self.lt = Lt { pc2: self.dpcd[2] & 0x40 != 0, ..Lt::default() };
        let mut ret = self.train_cr();
        if ret.is_ok() {
            ret = self.train_eq();
        }
        self.train_pattern(0);
        ret
    }

    /// nouveau nvkm_dp_train_sense.
    fn train_sense(&mut self, pc: bool, delay: u64) -> Result<(), String> {
        super::udelay(delay);
        let s = self.rdaux(0x202, 3)?;
        self.lt.stat[..3].copy_from_slice(&s);
        let a = self.rdaux(0x206, 2)?;
        self.lt.stat[4..6].copy_from_slice(&a);
        if pc {
            self.lt.pc2stat = self.rdaux(0x20c, 1).map(|v| v[0]).unwrap_or(0);
        }
        kprintln!("nvidia:     status {:02x?} post-cursor {:#x}", self.lt.stat, self.lt.pc2stat);
        Ok(())
    }

    /// nouveau nvkm_dp_train_drive: the drive levels the monitor asks for, with the VBIOS's
    /// settings for each.
    fn train_drive(&mut self, pc: bool) -> Result<(), String> {
        for i in 0..self.nr as usize {
            let lane = (self.lt.stat[4 + (i >> 1)] >> ((i & 1) * 4)) & 0xf;
            let mut lpc2 = (self.lt.pc2stat >> (i * 2)) & 3;
            let mut lpre = (lane & 0x0c) >> 2;
            let mut lvsw = lane & 3;
            let hivs = 3 - lpre;
            if lpc2 >= 3 {
                lpc2 = 3 | 0x04;
            }
            if lpre >= 3 {
                lpre = 3 | 0x04; // "yes.": shifted into place this is MAX_PRE_EMPHASIS_REACHED
                lvsw = 3 - (lpre & 3);
            } else if lvsw >= hivs {
                lvsw = hivs | 0x04;
            }
            self.lt.conf[i] = (lpre << 3) | lvsw;
            self.lt.pc2conf[i >> 1] |= lpc2 << ((i & 1) * 4);
            if let Some((cpc, dc, pe, pu)) = self.bios.dpcfg_match(&self.dpout, lpc2 & 3, lvsw & 3, lpre & 3) {
                self.sor_dp_drive(i, cpc, dc, pe, pu);
            }
        }
        kprintln!("nvidia:     drive {:02x?} post-cursor {:02x?}", self.lt.conf, self.lt.pc2conf);
        let conf = self.lt.conf;
        self.wraux(0x103, &conf)?;
        if pc {
            let pc2 = self.lt.pc2conf;
            self.wraux(0x10f, &pc2)?;
        }
        Ok(())
    }

    /// nouveau nvkm_dp_train_pattern.
    fn train_pattern(&self, pattern: u8) {
        self.sor_dp_pattern(pattern);
        let mut tp = self.rdaux(0x102, 1).map(|v| v[0]).unwrap_or(0);
        tp &= !0x0f;
        tp |= if pattern != 4 { pattern } else { 7 };
        if pattern != 0 {
            tp |= 0x20;
        } else {
            tp &= !0x20;
        }
        let _ = self.wraux(0x102, &[tp]);
    }

    /// nouveau nvkm_dp_train_cr: clock recovery.
    fn train_cr(&mut self) -> Result<(), String> {
        let mut voltage = self.lt.conf[0] & 3;
        let (mut tries, mut cr_done, mut abort) = (0, false, false);
        self.train_pattern(1);
        let usec = if self.dpcd[0] < 0x14 { (self.dpcd[0x0e] & 0x7f) as u64 * 4000 } else { 0 };
        loop {
            if self.train_drive(false).is_err() || self.train_sense(false, if usec > 0 { usec } else { 100 }).is_err() {
                break;
            }
            cr_done = true;
            for i in 0..self.nr as usize {
                let lane = (self.lt.stat[i >> 1] >> ((i & 1) * 4)) & 0xf;
                if lane & 1 == 0 {
                    cr_done = false;
                    if self.lt.conf[i] & 0x04 != 0 {
                        abort = true;
                    }
                    break;
                }
            }
            if self.lt.conf[0] & 3 != voltage {
                voltage = self.lt.conf[0] & 3;
                tries = 0;
            }
            if cr_done || abort {
                break;
            }
            tries += 1;
            if tries >= 5 {
                break;
            }
        }
        if cr_done {
            Ok(())
        } else {
            Err(String::from("clock recovery failed"))
        }
    }

    /// nouveau nvkm_dp_train_eq: channel equalisation.
    fn train_eq(&mut self) -> Result<(), String> {
        let d = self.dpcd;
        let (mut eq_done, mut cr_done, mut tries) = (false, true, 0);
        let pattern = if d[0] >= 0x14 && d[3] & 0x80 != 0 {
            4
        } else if d[0] >= 0x12 && d[2] & 0x40 != 0 {
            3
        } else {
            2
        };
        self.train_pattern(pattern);
        let usec = (d[0x0e] & 0x7f) as u64 * 4000;
        let pc = self.lt.pc2;
        loop {
            if (tries > 0 && self.train_drive(pc).is_err()) || self.train_sense(pc, if usec > 0 { usec } else { 400 }).is_err() {
                break;
            }
            eq_done = self.lt.stat[2] & 0x01 != 0;
            for i in 0..self.nr as usize {
                if !eq_done {
                    break;
                }
                let lane = (self.lt.stat[i >> 1] >> ((i & 1) * 4)) & 0xf;
                if lane & 1 == 0 {
                    cr_done = false;
                }
                if lane & 2 == 0 || lane & 4 == 0 {
                    eq_done = false;
                }
            }
            if eq_done || !cr_done {
                break;
            }
            tries += 1;
            if tries > 5 {
                break;
            }
        }
        if eq_done {
            Ok(())
        } else {
            Err(String::from("channel equalisation failed"))
        }
    }

    /// Step 5a: the core channel's methods for the new head, output and window, then UPDATE
    /// (nouveau corec57d_init for the window, nv50_sor_update, nv50_head_flush_set,
    /// corec37d_wndw_owner, corec37d_update).
    fn modeset(&mut self) -> Result<(), String> {
        let r = self.r;
        let m = self.mode;
        let (i, wn) = (self.head, self.wndw);
        let hoff = i * 0x400;
        let woff = wn * 0x80;
        // nouveau nv50_head_atomic_check_mode: the hardware counts from one past the sync start.
        let h_synce = m.hsync_end - m.hsync_start - 1;
        let h_blanke = m.htotal - m.hsync_start - 1;
        let h_blanks = h_blanke + m.hdisplay;
        let v_synce = m.vsync_end - m.vsync_start - 1;
        let v_blanke = m.vtotal - m.vsync_start - 1;
        let v_blanks = v_blanke + m.vdisplay;
        let hz = m.khz * 1000;
        let mut cmds: Vec<(u32, Vec<u32>)> = Vec::new();
        // The window's limits (corec57d_init): packed RGB formats; up to 0x7fff pixels a line,
        // input LUT allowed, 2-tap scaler, no upscaling.
        cmds.push((0x1004 + woff, Vec::from([0xf, 0])));
        cmds.push((0x1010 + woff, Vec::from([0x7fff | 1 << 16 | 1 << 20])));
        // The output: owned by the head, DisplayPort on its sub-link (sorc37d_ctrl).
        let proto = if self.link() & 1 != 0 { 8 } else { 9 };
        cmds.push((0x300 + self.sor * 0x20, Vec::from([1 << i | proto << 8])));
        // The head (headc37d_view, headc57d_mode, headc37d_dither, headc57d_procamp,
        // headc57d_or).
        cmds.push((0x204c + hoff, Vec::from([m.vdisplay << 16 | m.hdisplay])));
        cmds.push((0x2058 + hoff, Vec::from([m.vdisplay << 16 | m.hdisplay])));
        cmds.push((0x2064 + hoff, Vec::from([m.vtotal << 16 | m.htotal, v_synce << 16 | h_synce, v_blanke << 16 | h_blanke, v_blanks << 16 | h_blanks])));
        cmds.push((0x2074 + hoff, Vec::from([1]))); // second blank (interlaced only): end 0, start 1
        cmds.push((0x2008 + hoff, Vec::from([0]))); // progressive
        cmds.push((0x200c + hoff, Vec::from([hz])));
        cmds.push((0x2028 + hoff, Vec::from([hz])));
        cmds.push((0x2030 + hoff, Vec::from([4 | 1 << 4 | 1 << 12 | 1 << 8]))); // cursor 256, output LUT allowed, 2 taps, upscaling
        cmds.push((0x2018 + hoff, Vec::from([0]))); // no dithering
        cmds.push((0x2000 + hoff, Vec::from([0]))); // RGB, VESA range
        let or = 0x3f << 26 | 4 << 4 | (m.nvsync as u32) << 3 | (m.nhsync as u32) << 2; // 24 bits a pixel
        cmds.push((0x2004 + hoff, Vec::from([or])));
        // The window belongs to the head, in an update with no window interlock.
        cmds.push((0x1000 + woff, Vec::from([i])));
        cmds.push((0x218, Vec::from([0, 0])));
        cmds.push((0x200, Vec::from([1])));
        let core = &mut self.chans[0];
        for (mthd, data) in &cmds {
            kprintln!("nvidia:     core {:#06x} = {:08x?}", mthd, data);
            core.mthd(r, *mthd, data)?;
        }
        core.kick(r);
        self.undo.updated = true;
        Ok(())
    }

    /// Step 5b: answer the display engine's supervisor requests until the update is done
    /// (nouveau gv100_disp_intr_ctrl_disp and gv100_disp_super).
    fn supervise(&mut self) -> Result<(), String> {
        let r = self.r;
        let hz = crate::interrupts::HZ;
        let start = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let mut seen = 0;
        loop {
            if let Some(e) = self.exceptions() {
                kprintln!("nvidia:   display engine exception: {}", e);
            }
            let stat = r.rd(0x611c30);
            if stat & 0x80 != 0 {
                let err = r.mask(0x611848, 0, 0);
                kprintln!("nvidia:   display engine error {:#x}", err);
            }
            let pending = stat & 7;
            if pending != 0 {
                r.wr(0x611860, pending);
                self.super_step(pending)?;
                seen |= pending;
                if pending & 4 != 0 {
                    break;
                }
                continue;
            }
            let now = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
            if now > start + 3 * hz {
                let core = &self.chans[0];
                return Err(format!(
                    "the display engine didn't finish the mode change (supervisor steps seen {:#x}; interrupts {:#x}, core GET {:#x} of {:#x}, state {:#x})",
                    seen, r.rd(0x611ec0), r.rd(0x680004), core.put, r.rd(0x610630)
                ));
            }
        }
        if !self.chans[0].wait(r, 1000) {
            kprintln!("nvidia:   the core channel hasn't read everything (GET {:#x})", r.rd(0x680004));
        }
        Ok(())
    }

    /// One supervisor step for our head.
    fn super_step(&mut self, pending: u32) -> Result<(), String> {
        let r = self.r;
        let heads = r.rd(0x610060) & 0xff;
        let stat = r.rd(0x6107a8);
        let masks: Vec<u32> = (0..8).map(|h| if heads & (1 << h) != 0 { r.rd(0x6107ac + h * 4) } else { 0 }).collect();
        kprintln!("nvidia:   supervisor {}: status {:#x}, heads {:x?}", pending.trailing_zeros() + 1, stat, &masks[..]);
        for h in (0..8).filter(|&h| h != self.head && masks[h as usize] & 0x11000 != 0) {
            kprintln!("nvidia:   head {} is changing too ({:#x}): left alone", h, masks[h as usize]);
        }
        let mask = masks[self.head as usize];
        let hoff = self.head * 0x400;
        let khz = r.rd(0x68200c + hoff) / 1000;
        let result = if pending & 1 != 0 {
            // 1.0: the output that was on this head (none: it was dark).
            Ok(())
        } else if pending & 2 != 0 {
            let mut res = Ok(());
            if mask & 0x10000 != 0 {
                res = self.vpll(khz);
            }
            if res.is_ok() && mask & 0x1000 != 0 {
                res = self.super_2_2(khz);
            }
            res
        } else {
            if mask & 0x1000 != 0 {
                self.ied_on(1, khz)
            } else {
                Ok(())
            }
        };
        for h in (0..8).filter(|h| heads & (1 << h) != 0) {
            r.wr(0x6107ac + h * 4, 0);
        }
        r.wr(0x6107a8, 0x8000_0000);
        result
    }

    /// Supervisor 2.1: the head's pixel clock (nouveau ga100_devinit_pll_set).
    fn vpll(&self, khz: u32) -> Result<(), String> {
        let r = self.r;
        let info = self.bios.pll(0x80 + self.head as u8).ok_or_else(|| String::from("no pixel clock limits in the VBIOS"))?;
        let (n, f, m, p) = bios::pll_calc(&info, khz).ok_or_else(|| format!("no pixel clock settings for {} kHz", khz))?;
        let o = self.head * 0x40;
        kprintln!("nvidia:   pixel clock {} kHz: N {} fN {:#x} M {} P {} (limits {:?}; was {:#x} {:#x} {:#x})", khz, n, f, m, p, info, r.rd(0xef00 + o), r.rd(0xef18 + o), r.rd(0xef04 + o));
        r.wr(0xef00 + o, 0x0208_0004);
        r.wr(0xef18 + o, n << 16 | f);
        r.wr(0xef04 + o, p << 16 | m);
        r.wr(0xe9c0 + self.head * 4, 1);
        kprintln!("nvidia:   pixel clock set: {:#x} {:#x} {:#x}", r.rd(0xef00 + o), r.rd(0xef18 + o), r.rd(0xef04 + o));
        Ok(())
    }

    /// Run the output's OnInt2 (id 0) or OnInt3 (id 1) script for the pixel clock (nouveau
    /// nv50_disp_super_ied_on).
    fn ied_on(&self, id: usize, khz: u32) -> Result<(), String> {
        let o = &self.outp;
        let l = if o.link == 0 { 0 } else { o.link.trailing_zeros() + 1 }; // ffs
        let mask = (0x100u16 << self.head) | (l as u16) << 6 | o.or as u16;
        let Some(iedt) = self.bios.outp_match(o.hasht, mask) else {
            kprintln!("nvidia:   no output scripts for {:#x}:{:#x}", o.hasht, mask);
            return Ok(());
        };
        let proto = (self.r.rd(0x680300 + self.sor * 0x20) >> 8) & 0xf;
        let flags = if self.link() == 3 { 1 } else { 0 };
        let Some(clkcmp) = self.bios.ocfg_match(&iedt, proto as u8, flags) else {
            kprintln!("nvidia:   no output script settings for protocol {:#x}", proto);
            return Ok(());
        };
        let script = self.bios.oclk_match(clkcmp[id], khz);
        if script == 0 {
            kprintln!("nvidia:   no output script {} for {} kHz", id, khz);
            return Ok(());
        }
        self.init(self.sor as i32, self.link() as i32, self.head as i32).run(script, if id == 0 { "output on (2)" } else { "output on (3)" })
    }

    /// Supervisor 2.2 (nouveau nv50_disp_super_2_2 and _2_2_dp, ga102_sor_clock).
    fn super_2_2(&self, khz: u32) -> Result<(), String> {
        let r = self.r;
        self.ied_on(0, khz)?;
        // The raster generator's clock divider (gf119_head_rgclk): no division.
        r.mask(0x612200 + self.head * 0x800, 0xf, 0);
        // DisplayPort stream settings (no "activesym" on these chips: the transfer unit is 64).
        let hoff = self.head * 0x400;
        let (total, bend, bstart) = (r.rd(0x682064 + hoff), r.rd(0x68206c + hoff), r.rd(0x682070 + hoff));
        let (htotal, hblanke, hblanks) = ((total & 0xffff) as i64, (bend & 0xffff) as i64, (bstart & 0xffff) as i64);
        let (vblanke, vblanks) = ((bend >> 16) as i64, (bstart >> 16) as i64);
        let depth = match (r.rd(0x682004 + hoff) >> 4) & 0xf {
            5 => 30,
            4 => 24,
            _ => 18,
        };
        let (khz, link_kbps, symbol) = (khz.max(1) as i64, self.bw as i64 * 27_000, 100_000i64);
        let (ef, nr) = (self.ef as i64, self.nr.max(1) as i64);
        let mut h = hblanke + htotal - hblanks - 7;
        h = h * link_kbps / khz;
        h = h - 3 * ef - 12 / nr;
        let mut v = vblanks - vblanke - 25;
        v = v * link_kbps / khz;
        v = v - (36 / nr + 3) - 1;
        let s = self.head * 0x800;
        r.mask(0x616568 + s, 0xffff, (h as u32) & 0xffff);
        r.mask(0x61656c + s, 0xff_ffff, (v as u32) & 0xff_ffff);
        let link_data_rate = (khz * depth / 8) / nr;
        let link_ratio = link_data_rate * symbol / link_kbps;
        let mut unk = (symbol - link_ratio) * 64;
        unk *= link_ratio;
        unk = unk / symbol / symbol + 6;
        r.mask(0x616550 + s, 0x0c00_003f, 0x0800_0000 | (unk as u32 & 0x3f));
        kprintln!("nvidia:   DisplayPort stream: hblank symbols {}, vblank symbols {}, watermark {} (head SF {:#x})", h, v, unk, r.rd(0x616540 + s));
        // The output's clock (ga102_sor_clock: DisplayPort, no division).
        r.wr(0x00ec08 + self.sor * 0x10, 0);
        r.wr(0x00ec04 + self.sor * 0x10, 0);
        Ok(())
    }

    /// Exceptions the display engine reported (nouveau gv100_disp_intr_exc_*), cleared.
    fn exceptions(&self) -> Option<String> {
        let r = self.r;
        let intr = r.rd(0x611ec0);
        if intr & 0xe00 == 0 {
            return None;
        }
        let mut out = Vec::new();
        let mut exc = |chid: u32| {
            let stat = r.rd(0x611020 + chid * 12);
            let reason = match (stat >> 12) & 7 {
                0 => "none",
                1 => "command buffer",
                2 => "trap",
                3 => "reserved method",
                4 => "invalid argument",
                5 => "invalid state",
                7 => "unknown handle",
                _ => "?",
            };
            let (data, code) = (r.rd(0x611024 + chid * 12), r.rd(0x611028 + chid * 12));
            out.push(format!("channel {}: {} (method {:#x}, data {:#x}, code {:#x})", chid, reason, (stat & 0xfff) << 2, data, code));
            r.wr(0x611020 + chid * 12, 0x9000_0000);
        };
        if intr & 0x200 != 0 {
            let s = r.rd(0x61184c);
            for w in (0..32).filter(|w| s & (1 << w) != 0) {
                exc(1 + w);
            }
            r.wr(0x61184c, s);
        }
        if intr & 0x400 != 0 {
            let s = r.rd(0x611850);
            for w in (0..32).filter(|w| s & (1 << w) != 0) {
                exc(33 + w);
            }
            r.wr(0x611850, s);
        }
        if intr & 0x800 != 0 {
            let s = r.rd(0x611854);
            if s & 1 != 0 {
                exc(0);
            }
            for h in (0..8).filter(|h| s & (0x10000 << h) != 0) {
                exc(73 + h);
            }
            r.wr(0x611854, s);
        }
        Some(out.join("; "))
    }

    /// Send methods to a channel one at a time; the engine must finish each before the next,
    /// so a stall names its method. `which`: 0 core, 1 window, 2 window-immediate.
    fn send(&mut self, which: usize, cmds: &[(u32, &[u32], &str)]) -> Result<(), String> {
        let r = self.r;
        let status = self.chans[which].state_reg();
        for &(m, d, what) in cmds {
            let c = &mut self.chans[which];
            c.mthd(r, m, d)?;
            c.kick(r);
            let (user, put) = (c.user(), c.put as u32);
            // Done: everything read, and the channel idle or no method running.
            let done = super::poll_ms(r, 50_000_000, 300, |r| {
                let s = r.rd(status);
                r.rd(user + 4) == put && ((s >> 16) & 0xf == 4 || s & 0x8000_0000 == 0)
            });
            if !done {
                let name = self.chans[which].name;
                kprintln!("nvidia:   {} channel stuck on method {:#x} ({}): status {}, GET {:#x} of {:#x}", name, m, what, chan_status(r.rd(status)), r.rd(user + 4), put);
                return Err(format!("the {} channel got stuck on method {:#x} ({})", name, m, what));
            }
        }
        Ok(())
    }

    /// Step 6: point the window at the picture, then try colour pipelines until the engine
    /// takes one: first the firmware's own (composition bypass, no input LUT: how it runs its
    /// screen on this card), then NVIDIA's driver's (opaque blending, identity input LUT, the
    /// colour conversion stages off: nvkms EvoFlipC5Common), then opaque without the LUT.
    /// An update the engine rejects raises an exception, which is logged and cleared before
    /// the next try. Returns whether the head needs the output LUT.
    fn show_window(&mut self, w: u32, h: u32, pitch: u32) -> Result<bool, String> {
        let r = self.r;
        let size = h << 16 | w;
        let pic = ((self.inst + PIC_AT) >> 8) as u32;
        let ilut = ((self.inst + ILUT_AT) >> 8) as u32;
        let chid = 1 + self.wndw;
        let status = 0x610664 + self.wndw * 4;
        let armed = 0x690000 + self.wndw * 0x1000 + 0x800;
        // The window's position first, through the window-immediate channel on its own (not
        // interlocked), so window updates don't wait on it.
        let imm = 0x690000 + (32 + self.wndw) * 0x1000;
        kprintln!("nvidia:   window position before: {:#x} (armed {:#x})", r.rd(imm + 0x208), r.rd(imm + 0x808));
        self.send(2, &[(0x208, &[0], "position 0,0"), (0x200, &[1], "update")])?;
        let placed = super::poll_ms(r, 50_000_000, 500, |r| r.rd(imm + 0x808) == 0);
        kprintln!("nvidia:   window position: {:#x} (armed {:#x}{}), status {}", r.rd(imm + 0x208), r.rd(imm + 0x808), if placed { "" } else { ", not taken" }, chan_status(r.rd(0x6106e4 + self.wndw * 4)));
        // The surface: the same for every try.
        let surface: [(u32, &[u32], &str); 7] = [
            (0x218, &[0, 0], "no semaphore, no notifier"),
            (0x224, &[size, 0, 0xcf, pitch >> 6], "size, storage, A8R8G8B8, pitch"),
            (0x240, &[H_VRAM], "the picture's memory handle"),
            (0x260, &[pic], "the picture's offset"),
            (0x290, &[0], "source position"),
            (0x298, &[size], "source size"),
            (0x2a4, &[size], "output size"),
        ];
        if let Err(e) = self.send(1, &surface) {
            self.window_report(&surface);
            return Err(e);
        }
        kprintln!("nvidia:   window channel took the surface");
        let fw_fmt: [u32; 12] = [0xfffc, 0, 0, 0, 0, 0xfffc, 0, 0, 0, 0, 0xfffc, 0];
        let identity: [u32; 12] = [0x10000, 0, 0, 0, 0, 0x10000, 0, 0, 0, 0, 0x10000, 0];
        let ilut_ctl = (4 + 1024 + 1) << 8 | 2 << 2; // 4 header entries + 1025, 10-bit direct
        let interlock = 1 << self.wndw;
        let firmware: [(u32, &[u32], &str); 5] = [
            (0x308, &[0], "present: as soon as possible"),
            (0x2ec, &[0x1_0000, 0, 1], "composition bypass"),
            (0x440, &[0, 0, 0], "no input LUT"),
            (0x400, &fw_fmt, "format conversion (the firmware's)"),
            (0x370, &[0, interlock], "interlocks: this window only"),
        ];
        let nvidia: [(u32, &[u32], &str); 12] = [
            (0x308, &[1], "present at vertical blank"),
            (0x2ec, &[0xff << 4, 0, 0x11], "opaque blending"),
            (0x440, &[ilut_ctl, H_VRAM, ilut], "identity input LUT"),
            (0x400, &identity, "format conversion: identity"),
            (0x45c, &[0], "colour conversion 00 off"),
            (0x4a0, &[0], "colour LUT 0 off"),
            (0x4bc, &[0], "colour conversion 01 off"),
            (0x528, &[0, 0], "no tone-mapping LUT"),
            (0x53c, &[0], "colour conversion 10 off"),
            (0x580, &[0], "colour LUT 1 off"),
            (0x59c, &[0], "colour conversion 11 off"),
            (0x370, &[0, interlock], "interlocks: this window only"),
        ];
        let plain: [(u32, &[u32], &str); 2] = [(0x440, &[0, 0, 0], "no input LUT"), (0x370, &[0, interlock], "interlocks: this window only")];
        let tries: [(&str, &[(u32, &[u32], &str)], bool); 3] = [
            ("composition bypass, no input LUT (the firmware's way)", &firmware, false),
            ("opaque, identity input LUT (NVIDIA's driver's way)", &nvidia, true),
            ("opaque, no input LUT", &plain, false),
        ];
        for (k, &(what, cmds, olut)) in tries.iter().enumerate() {
            if let Err(e) = self.send(1, cmds) {
                self.window_report(cmds);
                return Err(e);
            }
            {
                let wn = &mut self.chans[1];
                wn.mthd(r, 0x200, &[1])?;
                wn.kick(r);
            }
            let mut outcome = 0; // 1: shown, 2: rejected
            super::poll_ms(r, 50_000_000, 1000, |r| {
                if r.rd(armed + 0x224) == size && r.rd(armed + 0x260) == pic {
                    outcome = 1;
                } else if (r.rd(status) >> 8) & 0xf == 0xb || r.rd(0x61184c) & (1 << self.wndw) != 0 {
                    outcome = 2;
                }
                outcome != 0
            });
            let stat = r.rd(0x611020 + chid * 12);
            let (data, code) = (r.rd(0x611024 + chid * 12), r.rd(0x611028 + chid * 12));
            kprintln!(
                "nvidia:   try {} ({}): {} (window status {}, exception state {:#x}, data {:#x}, code {:#x}; armed size {:#x}, offset {:#x})",
                k + 1, what, match outcome { 1 => "showing the picture", 2 => "rejected", _ => "no answer" },
                chan_status(r.rd(status)), stat, data, code, r.rd(armed + 0x224), r.rd(armed + 0x260)
            );
            if outcome == 1 {
                return Ok(olut);
            }
            if outcome == 2 {
                // Clear the exception (nouveau gv100_disp_exception) and wait for the channel.
                r.wr(0x611020 + chid * 12, 0x9000_0000);
                r.wr(0x61184c, 1 << self.wndw);
                let back = super::poll_ms(r, 50_000_000, 500, |r| (r.rd(status) >> 8) & 0xf != 0xb);
                kprintln!("nvidia:   exception cleared; window status {}", chan_status(r.rd(status)));
                if !back {
                    break;
                }
            } else {
                break;
            }
        }
        self.window_report(&surface);
        Err(String::from("the window didn't take the picture"))
    }

    /// Step 7: the head's identity output LUT (nouveau headc57d_olut_set: 10-bit direct,
    /// interpolated, 1029 entries), in a core update of its own once the picture shows, so a
    /// problem here can't keep the picture away. A LUT change shouldn't need the supervisor;
    /// any request that comes anyway is answered with nothing to do.
    fn output_lut(&mut self) -> Result<(), String> {
        let r = self.r;
        let hoff = self.head * 0x400;
        let olut = ((self.inst + OLUT_AT) >> 8) as u32;
        let ctl = 1 | 2 << 2 | (4 + 1024 + 1) << 8;
        let cmds: [(u32, &[u32]); 3] = [(0x2280 + hoff, &[ctl, 0xffff_ffff, H_VRAM, olut]), (0x218, &[0, 0]), (0x200, &[1])];
        let put = {
            let core = &mut self.chans[0];
            for (m, d) in cmds {
                core.mthd(r, m, d)?;
            }
            core.kick(r);
            core.put as u32
        };
        let hz = crate::interrupts::HZ;
        let start = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        loop {
            let pending = r.rd(0x611c30) & 7;
            if pending != 0 {
                r.wr(0x611860, pending);
                let heads = r.rd(0x610060) & 0xff;
                kprintln!("nvidia:   output LUT: supervisor {} came (status {:#x}, head {} {:#x}); nothing to do", pending.trailing_zeros() + 1, r.rd(0x6107a8), self.head, r.rd(0x6107ac + self.head * 4));
                for h in (0..8).filter(|h| heads & (1 << h) != 0) {
                    r.wr(0x6107ac + h * 4, 0);
                }
                r.wr(0x6107a8, 0x8000_0000);
            }
            if r.rd(0x680004) == put && r.rd(0x610630) & 0x1f_0000 == 0xb_0000 && r.rd(0x688000 + 0x228c + hoff) == olut {
                kprintln!("nvidia:   output LUT on (armed control {:#x}, handle {:#x}, offset {:#x})", r.rd(0x688000 + 0x2280 + hoff), r.rd(0x688000 + 0x2288 + hoff), r.rd(0x688000 + 0x228c + hoff));
                return Ok(());
            }
            if crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed) > start + hz {
                if let Some(e) = self.exceptions() {
                    kprintln!("nvidia:   display engine exception: {}", e);
                }
                return Err(format!(
                    "the core channel didn't take it (GET {:#x} of {:#x}, status {:#x}, armed LUT offset {:#x}, exception state {:#x})",
                    r.rd(0x680004), put, r.rd(0x610630), r.rd(0x688000 + 0x228c + hoff), r.rd(0x611020)
                ));
            }
        }
    }

    /// After a window failure: exceptions (read directly, whether or not they raised an
    /// interrupt), the interrupt state, and which of the commands the window's state holds.
    fn window_report(&self, cmds: &[(u32, &[u32], &str)]) {
        let r = self.r;
        if let Some(e) = self.exceptions() {
            kprintln!("nvidia:   display engine exception: {}", e);
        }
        for chid in [0, 1 + self.wndw, 33 + self.wndw] {
            let stat = r.rd(0x611020 + chid * 12);
            if stat != 0 {
                kprintln!(
                    "nvidia:   channel {} exception state {:#x} (reason {}, method {:#x}), data {:#x}, code {:#x}",
                    chid, stat, (stat >> 12) & 7, (stat & 0xfff) << 2, r.rd(0x611024 + chid * 12), r.rd(0x611028 + chid * 12)
                );
            }
        }
        kprintln!(
            "nvidia:   interrupts {:#x}, control {:#x}, error {:#x}, window exceptions {:#x}, immediate {:#x}, other {:#x}",
            r.rd(0x611ec0), r.rd(0x611c30), r.rd(0x611848), r.rd(0x61184c), r.rd(0x611850), r.rd(0x611854)
        );
        let asy = 0x690000 + self.wndw * 0x1000;
        let mut held = Vec::new();
        for &(m, d, _) in cmds {
            let got: Vec<u32> = (0..d.len() as u32).map(|k| r.rd(asy + m + k * 4)).collect();
            let same = got.iter().zip(d.iter()).filter(|(a, b)| a == b).count();
            held.push(format!("{:#x} {}/{}", m, same, d.len()));
        }
        kprintln!("nvidia:   window state holds (method: values that match): {}", held.join(", "));
    }

    /// After a failure: stop the channels, turn the link off, unroute, and put back the
    /// registers changed. A head that already took its new mode stays as it is.
    fn put_back(&mut self) {
        let r = self.r;
        if let Some(e) = self.exceptions() {
            kprintln!("nvidia:   display engine exception: {}", e);
        }
        for c in self.chans.iter_mut().rev() {
            c.stop(r);
        }
        if self.undo.updated {
            kprintln!("nvidia:   the mode change was sent; the head keeps whatever it took");
        } else {
            if self.undo.routed {
                if let Err(e) = self.script(self.dpout.script[4], "disable link") {
                    kprintln!("nvidia:   {}", e);
                }
                route_set(r, &self.outp, None);
            }
            for &(reg, v) in self.undo.regs.iter().rev() {
                r.wr(reg, v);
            }
            kprintln!("nvidia:   put back {} registers", self.undo.regs.len());
        }
        if let Some(cmd) = self.undo.pci_cmd {
            let (bus, dev, func) = self.at;
            crate::pci::write32(bus, dev, func, 0x04, cmd);
        }
        let _ = self.undo.trained;
    }
}

/// nouveau nvkm_ramht_hash for a 1024-entry table.
fn ramht_hash(chid: u32, handle: u32) -> u32 {
    let mut hash = 0;
    let mut h = handle;
    while h != 0 {
        hash ^= h & 0x3ff;
        h >>= 10;
    }
    (hash ^ chid << 6) & 0x3ff
}

/// The display object table's hash as NVIDIA's driver computes it, client number included
/// (open-gpu-kernel-modules instmemHashFunc_v03_00).
fn ramht_hash_rm(chid: u32, client: u32, handle: u32) -> u32 {
    (handle & 0x3ff)
        ^ ((handle >> 10) & 0x3ff)
        ^ ((handle >> 20) & 0x3ff)
        ^ (((client & 0xff) << 2) | (handle >> 30))
        ^ (((chid & 0xf) << 6) | ((client >> 8) & 0x3f))
        ^ ((chid >> 4) & 7)
}

/// A 0..1 value in 16 bits as a half float (nouveau fixedU0_16_FP16).
fn fp16_of_unorm16(v: u32) -> u16 {
    let mut fixed = v & 0xffff;
    if fixed == 0 {
        return 0;
    }
    let mut exp: i32 = 0;
    loop {
        exp -= 1;
        if fixed & 0x8000 != 0 {
            break;
        }
        fixed = (fixed << 1) & 0xffff;
    }
    let man = ((fixed << 1) & 0xffc0) >> 6;
    (((exp + 15) as u32) << 10 | man) as u16
}

/// A display channel's status register in words (open-gpu-kernel-modules dev_disp.h v03_00,
/// NV_PDISP_FE_CHNSTATUS_WIN: state 19:16, update state 11:8, method executing bit 31).
fn chan_status(s: u32) -> String {
    let state = match (s >> 16) & 0xf {
        0 => "deallocated",
        1 => "unconnected",
        2 | 3 => "starting",
        4 => "idle",
        5 => "busy",
        6 | 7 => "shutting down",
        _ => "?",
    };
    let upd = match (s >> 8) & 0xf {
        0 => "init",
        1 => "idle",
        2 => "wait block",
        3 => "wait MPI",
        4 => "wait interlock 1",
        5 => "wait state check",
        6 => "wait ready to flip",
        7 => "wait interlock 2",
        8 => "check pending loadv",
        9 => "send update",
        10 => "wait PRM",
        11 => "exception",
        12 => "wait interlock abort",
        _ => "?",
    };
    format!("{:#x} ({}, update {}, {})", s, state, upd, if s & 0x8000_0000 != 0 { "executing a method" } else { "no method running" })
}

/// The SOR an output's pad links are routed to (nouveau gm200_sor_route_get).
fn route_get(r: &Bar0, o: &DcbOut) -> Option<u32> {
    let mut m = o.or.trailing_zeros() * 2;
    let mut sor = [0u32; 2];
    for s in 0..2 {
        if o.link & (1 << s) != 0 {
            let data = r.rd(0x612308 + m * 0x80);
            sor[s] = data & 0xf;
            if sor[s] == 0 {
                return None;
            }
        }
        m += 1;
    }
    if o.link == 3 && sor[0] != sor[1] {
        return None;
    }
    Some(if o.link & 1 != 0 { sor[0] } else { sor[1] } - 1)
}

/// Route a SOR (and its sub-link) to an output's pad links, or unroute (nouveau
/// gm200_sor_route_set).
fn route_set(r: &Bar0, o: &DcbOut, ior: Option<(u32, u32)>) {
    let moff = o.or.trailing_zeros() * 0x100;
    let sor = ior.map(|(id, _)| id + 1).unwrap_or(0);
    let mut link = ior.map(|(_, l)| (l == 2) as u32).unwrap_or(0);
    if o.link & 1 != 0 {
        r.mask(0x612308 + moff, 0x1f, link << 4 | sor);
        link += 1;
    }
    if o.link & 2 != 0 {
        r.mask(0x612388 + moff, 0x1f, link << 4 | sor);
    }
}

/// The AUX channel's pins in AUX mode, powered (nouveau gm200_i2c_pad_mode, and "aux power
/// always" while the link is in use).
fn aux_pad_on(r: &Bar0, ch: u8) {
    let pad = ch as u32 * 0x50;
    let (mode, off) = (r.rd(0x00d970 + pad), r.rd(0x00d97c + pad));
    if mode & 0xc003 != 0x0002 || off & 1 != 0 {
        r.wr(0x00d970 + pad, (mode & !0xc003) | 0x0002);
        r.wr(0x00d97c + pad, off & !1);
        super::udelay(1000);
    }
}

/// A picture to show the monitor works: a gradient, a border to check the edges, colour bars
/// and what's driving it.
fn draw_test_picture(virt: u64, w: u32, h: u32, pitch: u32, info: &str) {
    use crate::font::Face;
    use crate::gfx::{rgb, Canvas};
    let mut c = Canvas::new(w as usize, h as usize);
    let (wi, hi) = (w as i32, h as i32);
    c.gradient_v(0, 0, wi, hi, rgb(18, 32, 64), rgb(10, 90, 110));
    let b = 6;
    c.fill_rect(0, 0, wi, b, rgb(255, 255, 255));
    c.fill_rect(0, hi - b, wi, b, rgb(255, 255, 255));
    c.fill_rect(0, 0, b, hi, rgb(255, 255, 255));
    c.fill_rect(wi - b, 0, b, hi, rgb(255, 255, 255));
    let title = "Solon";
    let tpx = (hi / 6).clamp(48, 220) as u16;
    let tw = crate::font::width(title, Face::SansBold, tpx);
    c.text_face((wi / 2 - tw / 2) * 64, hi / 3, title, Face::SansBold, tpx, rgb(255, 255, 255));
    let sub = "Your second monitor, lit by Solon's own NVIDIA driver";
    let spx = (hi / 24).clamp(18, 64) as u16;
    let sw = crate::font::width(sub, Face::Sans, spx);
    c.text_face((wi / 2 - sw / 2) * 64, hi / 3 + spx as i32 * 2, sub, Face::Sans, spx, rgb(220, 235, 245));
    let ipx = (hi / 40).clamp(14, 40) as u16;
    let iw = crate::font::width(info, Face::Sans, ipx);
    c.text_face((wi / 2 - iw / 2) * 64, hi / 3 + spx as i32 * 2 + ipx as i32 * 3, info, Face::Sans, ipx, rgb(170, 200, 215));
    let bars = [rgb(255, 255, 255), rgb(255, 255, 0), rgb(0, 255, 255), rgb(0, 255, 0), rgb(255, 0, 255), rgb(255, 0, 0), rgb(0, 0, 255), rgb(0, 0, 0)];
    let (by, bh) = (hi * 2 / 3, hi / 8);
    let bw = (wi - 2 * hi / 10) / bars.len() as i32;
    let x0 = (wi - bw * bars.len() as i32) / 2;
    for (k, &col) in bars.iter().enumerate() {
        c.fill_rect(x0 + k as i32 * bw, by, bw, bh, col);
    }
    for k in 0..wi - 2 * x0 {
        let g = (k * 255 / (wi - 2 * x0).max(1)) as u8;
        c.fill_rect(x0 + k, by + bh + 8, 1, bh / 3, rgb(g, g, g));
    }
    for y in 0..h as usize {
        let src = &c.px[y * w as usize..(y + 1) * w as usize];
        let dst = (virt + y as u64 * pitch as u64) as *mut u32;
        for (x, &p) in src.iter().enumerate() {
            unsafe { core::ptr::write_volatile(dst.add(x), p) };
        }
    }
    unsafe { core::arch::x86_64::_mm_sfence() };
}

//! Sound: Intel High Definition Audio controllers (every PC since about 2005) and their
//! codecs: the motherboard's jacks (speakers, headphones, optical) and the monitors on an
//! NVIDIA graphics card (DisplayPort sound, switched on by nvidia::audio). Also the front for
//! USB sound devices (usbaudio.rs). Plays 48 kHz 16-bit stereo through the output chosen in
//! Settings > Sound, or automatically: a USB sound device, else a jack with something plugged
//! in (headphones first), else built-in speakers, else a monitor's speakers, else every jack at
//! once. Polled: the player tops the ring buffer up a few times a second.
//!
//! Every controller found is set up. AMD and NVIDIA controllers only read the processor's
//! latest writes when told to snoop (Linux hda_intel: AMD's PCI register 0x42, NVIDIA's
//! 0x4c-0x4e); the sound ring is also flushed from the caches after each write, so stale
//! (silent) memory can't play either way.

use crate::interrupts::sleep_ms;
use crate::memory::{map_mmio, Dma};
use crate::pci;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;
use core::ptr::{read_volatile, write_volatile};
use spin::Mutex;

pub const RATE: u32 = 48_000;
/// Ring buffer: one second of sound, in two halves.
const RING_FRAMES: usize = RATE as usize;
const RING_BYTES: usize = RING_FRAMES * 4;
/// Our stream tag (any of 1-15).
const TAG: u32 = 1;
/// 48 kHz, 16 bits, 2 channels.
const FORMAT: u16 = 0x0011;

/// Settings > Sound: chosen automatically.
pub const AUTO: u32 = 0;
/// Every jack on the computer at once.
pub const ALL_JACKS: u32 = 1;
/// Keys of monitors' outputs have this bit, and USB sound devices' this one (with their id).
const MONITOR: u32 = 1 << 31;
const USB: u32 = 1 << 30;

/// An output, as Settings shows it.
#[derive(Clone)]
pub struct OutputInfo {
    pub key: u32,
    pub name: String,
    /// Where it is.
    pub place: String,
    /// Something plugged in (None: the jack can't tell, or it's a monitor).
    pub plugged: Option<bool>,
    pub monitor: bool,
    /// A USB sound device.
    pub usb: bool,
}

/// One analog or optical output of a codec: a jack or built-in speakers.
#[derive(Clone)]
struct Out {
    key: u32,
    codec: u32,
    pin: u32,
    name: String,
    place: &'static str,
    /// Pin, ..., converter: each widget and the connection it takes (the converter: None).
    path: Vec<(u32, Option<u32>)>,
    hp: bool,
    eapd: bool,
    /// The jack tells whether something is plugged in, and needs telling to look first
    /// (pin capability "trigger required", as Linux's read_pin_sense does).
    detect: bool,
    trigger: bool,
    /// Built into the computer (a laptop's or all-in-one's speakers).
    builtin: bool,
    /// An S/PDIF output (its converter needs "digital on").
    digital: bool,
    /// Order among jacks with something plugged in, for the automatic choice (lower first).
    rank: u32,
}

/// A digital pin of a graphics card's codec (HDMI or DisplayPort), for its monitors.
struct DigiPin {
    codec: u32,
    pin: u32,
    convs: Vec<u32>,
}

struct Codec {
    addr: u32,
    afg_caps: (u32, u32),
    types: Vec<(u32, u32)>,
}

/// One controller.
struct Ctl {
    base: u64,
    vendor: u16,
    /// PCI location: bus << 8 | device << 3 | function.
    bdf: u32,
    corb: Dma,
    rirb: Dma,
    corb_wp: u16,
    rirb_rp: u16,
    /// Offset of our output stream descriptor's registers.
    sd: u64,
    bdl: Dma,
    ring: Dma,
    running: bool,
    /// Frames written into the ring since the stream started (absolute), and frames the
    /// hardware had played at the last look (absolute).
    written: u64,
    played: u64,
    last_lpib: u32,
    codecs: Vec<Codec>,
    outs: Vec<Out>,
    digi: Vec<DigiPin>,
}

/// Every controller, and what plays where.
pub struct Sound {
    ctls: Vec<Ctl>,
    /// The controller playing, or a USB sound device (usbaudio::STREAM).
    active: usize,
    usb_out: bool,
    /// What Settings ask for, and what's set up now (a key, or ALL_JACKS).
    chosen: u32,
    using: Option<u32>,
    /// Software volume, 0..=256.
    volume: u32,
    /// A test sound plays until this frame; then the chosen output comes back.
    test_end: Option<u64>,
    last_look: u64,
}

pub static SOUND: Mutex<Option<Sound>> = Mutex::new(None);

fn key_of(bdf: u32, codec: u32, pin: u32) -> u32 {
    bdf << 11 | codec << 7 | pin
}

/// Write a cache line back to memory (the controller may not see the caches).
fn flush_line(addr: usize) {
    unsafe { core::arch::asm!("clflush [{0}]", in(reg) addr, options(nostack, preserves_flags)) };
}

fn fence() {
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) };
}

impl Ctl {
    fn r8(&self, off: u64) -> u8 {
        unsafe { read_volatile((self.base + off) as *const u8) }
    }
    fn r16(&self, off: u64) -> u16 {
        unsafe { read_volatile((self.base + off) as *const u16) }
    }
    fn r32(&self, off: u64) -> u32 {
        unsafe { read_volatile((self.base + off) as *const u32) }
    }
    fn w8(&self, off: u64, v: u8) {
        unsafe { write_volatile((self.base + off) as *mut u8, v) }
    }
    fn w16(&self, off: u64, v: u16) {
        unsafe { write_volatile((self.base + off) as *mut u16, v) }
    }
    fn w32(&self, off: u64, v: u32) {
        unsafe { write_volatile((self.base + off) as *mut u32, v) }
    }

    /// Wait (up to `ms`) for `f` to be true.
    fn wait(&self, ms: u32, f: impl Fn(&Ctl) -> bool) -> bool {
        for _ in 0..ms * 100 {
            if f(self) {
                return true;
            }
            for _ in 0..2000 {
                core::hint::spin_loop();
            }
        }
        f(self)
    }

    fn init(dev: &pci::Device) -> Option<Ctl> {
        let (phys, size) = dev.bar(0)?;
        dev.enable();
        match dev.vendor {
            // Intel: clear TCSEL (traffic class) so sound isn't starved on some chipsets.
            0x8086 => dev.mask8(0x44, 0x07, 0),
            // AMD and ATI: snoop the processor's caches (Linux: ATI_SB450_HDAUDIO_MISC_CNTR2).
            0x1022 | 0x1002 => dev.mask8(0x42, 0x07, 0x02),
            // NVIDIA: coherent transfers (Linux: NVIDIA_HDA_TRANSREG, _OSTRM_COH, _ISTRM_COH).
            0x10de => {
                dev.mask8(0x4e, 0x0f, 0x0f);
                dev.mask8(0x4d, 0x01, 0x01);
                dev.mask8(0x4c, 0x01, 0x01);
            }
            _ => {}
        }
        let base = map_mmio(phys, size.max(0x4000));
        let mut h = Ctl {
            base,
            vendor: dev.vendor,
            bdf: (dev.bus as u32) << 8 | (dev.dev as u32) << 3 | dev.func as u32,
            corb: Dma::new(1024, 1024),
            rirb: Dma::new(2048, 2048),
            corb_wp: 0,
            rirb_rp: 0,
            sd: 0,
            bdl: Dma::new(256, 128),
            ring: Dma::new(RING_BYTES, 4096),
            running: false,
            written: 0,
            played: 0,
            last_lpib: 0,
            codecs: Vec::new(),
            outs: Vec::new(),
            digi: Vec::new(),
        };
        let gcap = h.r16(0x00);
        let iss = ((gcap >> 8) & 0xF) as u64;
        let oss = ((gcap >> 12) & 0xF) as u64;
        if oss == 0 {
            kprintln!("hda: {:04x}:{:04x} has no output streams", dev.vendor, dev.device);
            return None;
        }
        // Reset the controller.
        h.w32(0x20, 0); // no interrupts
        h.w8(0x4C, 0);
        h.w8(0x5C, 0);
        h.w32(0x08, h.r32(0x08) & !1);
        if !h.wait(100, |h| h.r32(0x08) & 1 == 0) {
            kprintln!("hda: controller won't enter reset");
        }
        sleep_ms(1);
        h.w32(0x08, h.r32(0x08) | 1);
        if !h.wait(100, |h| h.r32(0x08) & 1 == 1) {
            kprintln!("hda: controller won't leave reset");
            return None;
        }
        sleep_ms(20); // codecs announce themselves within 521 us; be generous
        let codecs = h.r16(0x0E);
        h.w16(0x0E, codecs); // clear
        if codecs & 0x7FFF == 0 {
            kprintln!("hda: {:04x}:{:04x}: no codecs", dev.vendor, dev.device);
            return None;
        }
        h.setup_rings();
        // Our stream: the first output stream descriptor.
        h.sd = 0x80 + 0x20 * iss;
        for addr in 0..15u32 {
            if codecs & (1 << addr) != 0 {
                h.setup_codec(addr);
            }
        }
        if h.outs.is_empty() && h.digi.is_empty() {
            kprintln!("hda: {:04x}:{:04x}: no outputs", dev.vendor, dev.device);
            return None;
        }
        h.setup_stream();
        let names: Vec<String> = h.outs.iter().map(|o| format!("{} (pin {})", o.name, o.pin)).collect();
        kprintln!("hda: {:04x}:{:04x} ready: {} jack(s) {:?}, {} monitor pin(s)", dev.vendor, dev.device, h.outs.len(), names, h.digi.len());
        Some(h)
    }

    fn setup_rings(&mut self) {
        // CORB: stop, size 256 if possible, base, reset read pointer.
        self.w8(0x4C, 0);
        self.wait(10, |h| h.r8(0x4C) & 2 == 0);
        let cap = self.r8(0x4E);
        self.w8(0x4E, if cap & 0x40 != 0 { 2 } else if cap & 0x20 != 0 { 1 } else { 0 });
        let p = self.corb.phys();
        self.w32(0x40, p as u32);
        self.w32(0x44, (p >> 32) as u32);
        // (AMD and NVIDIA clear the reset bit by themselves.)
        self.w16(0x4A, 0x8000);
        self.wait(10, |h| h.r16(0x4A) & 0x8000 != 0);
        self.w16(0x4A, 0);
        self.wait(10, |h| h.r16(0x4A) & 0x8000 == 0);
        self.w16(0x48, 0);
        self.corb_wp = 0;
        // RIRB.
        self.w8(0x5C, 0);
        self.wait(10, |h| h.r8(0x5C) & 2 == 0);
        let cap = self.r8(0x5E);
        self.w8(0x5E, if cap & 0x40 != 0 { 2 } else if cap & 0x20 != 0 { 1 } else { 0 });
        let p = self.rirb.phys();
        self.w32(0x50, p as u32);
        self.w32(0x54, (p >> 32) as u32);
        self.w16(0x58, 0x8000);
        self.w16(0x5A, 0xFF);
        self.rirb_rp = 0;
        self.w8(0x4C, 2);
        self.w8(0x5C, 2);
    }

    fn ring_entries(sizereg: u8) -> u16 {
        match sizereg & 3 {
            2 => 256,
            1 => 16,
            _ => 2,
        }
    }

    /// Send one verb, wait for its answer.
    fn cmd(&mut self, codec: u32, nid: u32, verb: u32) -> Option<u32> {
        let n = Self::ring_entries(self.r8(0x4E));
        let wp = (self.corb_wp + 1) % n;
        let slot = unsafe { self.corb.ptr::<u32>().add(wp as usize) };
        unsafe { write_volatile(slot, codec << 28 | nid << 20 | verb) };
        flush_line(slot as usize);
        fence();
        self.corb_wp = wp;
        self.w16(0x48, wp);
        let rn = Self::ring_entries(self.r8(0x5E));
        for _ in 0..2 {
            if !self.wait(20, |h| h.r16(0x58) & 0xFF != h.rirb_rp) {
                break;
            }
            while self.r16(0x58) & 0xFF != self.rirb_rp {
                self.rirb_rp = (self.rirb_rp + 1) % rn;
                let e = unsafe { read_volatile(self.rirb.ptr::<u32>().add(self.rirb_rp as usize * 2)) };
                let ex = unsafe { read_volatile(self.rirb.ptr::<u32>().add(self.rirb_rp as usize * 2 + 1)) };
                if ex & 0x10 == 0 {
                    // Drain anything else that arrived, then answer.
                    self.rirb_rp = self.r16(0x58) & 0xFF;
                    self.w8(0x5D, 0x05);
                    return Some(e);
                }
            }
        }
        None
    }

    fn param(&mut self, codec: u32, nid: u32, p: u32) -> u32 {
        self.cmd(codec, nid, 0xF00 << 8 | p).unwrap_or(0)
    }

    fn connections(&mut self, codec: u32, nid: u32) -> Vec<u32> {
        let len = self.param(codec, nid, 0x0E);
        let long = len & 0x80 != 0;
        let count = len & 0x7F;
        let mut out = Vec::new();
        let per = if long { 2 } else { 4 };
        let mut i = 0;
        while i < count {
            let r = self.cmd(codec, nid, 0xF02 << 8 | i).unwrap_or(0);
            for k in 0..per {
                if i + k >= count {
                    break;
                }
                let e = if long { (r >> (16 * k)) & 0xFFFF } else { (r >> (8 * k)) & 0xFF };
                let (range, id) = if long { (e & 0x8000 != 0, e & 0x7FFF) } else { (e & 0x80 != 0, e & 0x7F) };
                if range {
                    if let Some(&prev) = out.last() {
                        for n in prev + 1..=id {
                            out.push(n);
                        }
                    }
                } else {
                    out.push(id);
                }
            }
            i += per;
        }
        out
    }

    /// Unmute an amplifier at 0 dB. `output` = output amp, else input amp `index`.
    fn unmute(&mut self, codec: u32, nid: u32, output: bool, index: u32, afg_caps: (u32, u32)) {
        let wcaps = self.param(codec, nid, 0x09);
        let has = if output { wcaps & (1 << 2) != 0 } else { wcaps & (1 << 1) != 0 };
        if !has {
            return;
        }
        // Amp capabilities: the widget's own if it overrides them, else the AFG's.
        let caps = if wcaps & (1 << 3) != 0 { self.param(codec, nid, if output { 0x12 } else { 0x0D }) } else if output { afg_caps.1 } else { afg_caps.0 };
        let steps = (caps >> 8) & 0x7F;
        let zero_db = (caps & 0x7F).min(steps);
        let payload = if output { 0xB000 } else { 0x7000 | index << 8 } | zero_db;
        self.cmd(codec, nid, 0x3 << 16 | payload);
    }

    /// Find the codec's outputs (nothing is switched on yet).
    fn setup_codec(&mut self, codec: u32) {
        let vendor = self.param(codec, 0, 0x00);
        let sub = self.param(codec, 0, 0x04);
        kprintln!("hda: codec {} is {:08x}", codec, vendor);
        for fg in (sub >> 16) & 0xFF..((sub >> 16) & 0xFF) + (sub & 0xFF) {
            if self.param(codec, fg, 0x05) & 0xFF != 1 {
                continue; // not an audio function group
            }
            self.cmd(codec, fg, 0x705 << 8); // power on (D0)
            sleep_ms(10);
            if vendor >> 16 == 0x10ec {
                self.realtek_init(codec, vendor);
            }
            let afg_caps = (self.param(codec, fg, 0x0D), self.param(codec, fg, 0x12));
            let ws = self.param(codec, fg, 0x04);
            let first = (ws >> 16) & 0xFF;
            let count = ws & 0xFF;
            let mut types = Vec::new();
            for nid in first..first + count {
                types.push((nid, (self.param(codec, nid, 0x09) >> 20) & 0xF));
            }
            for &(nid, kind) in &types {
                if kind != 4 {
                    continue;
                }
                let pincap = self.param(codec, nid, 0x0C);
                if pincap & (1 << 4) == 0 {
                    continue; // can't output
                }
                let config = self.cmd(codec, nid, 0xF1C << 8).unwrap_or(0);
                let connectivity = config >> 30;
                let device = (config >> 20) & 0xF;
                if connectivity == 1 {
                    continue; // nothing connected to this pin
                }
                // HDMI / DisplayPort pins (a graphics card's): set up per monitor when chosen.
                if pincap & (1 << 7 | 1 << 24) != 0 && device == 5 {
                    let convs = self.connections(codec, nid);
                    self.digi.push(DigiPin { codec, pin: nid, convs });
                    continue;
                }
                let Some((name, place, rank)) = describe(config) else { continue };
                // Find a path to a converter (depth-first, a few hops).
                let mut path = Vec::new();
                if !self.find_dac(codec, nid, &types, &mut path, 0) {
                    kprintln!("hda: no route from pin {} ({}) to a converter", nid, name);
                    continue;
                }
                let detect = pincap & (1 << 2) != 0 && (config >> 8) & 1 == 0;
                kprintln!("hda:   {} ({}) on pin {} via {:?}{}", name, place, nid, path, if detect { ", tells when plugged in" } else { "" });
                self.outs.push(Out {
                    key: key_of(self.bdf, codec, nid),
                    codec,
                    pin: nid,
                    name,
                    place,
                    path,
                    hp: device == 2,
                    eapd: pincap & (1 << 16) != 0,
                    detect,
                    trigger: pincap & (1 << 1) != 0,
                    builtin: connectivity == 2,
                    digital: device == 4,
                    rank,
                });
            }
            self.codecs.push(Codec { addr: codec, afg_caps, types });
        }
    }

    /// What Linux's Realtek driver does after a reset (realtek.c alc_fill_eapd_coef, alc882.c
    /// alc_fix_pll_init) on these codecs, through their coefficient widget 0x20: amplifier
    /// control off coefficient 7 bit 1 (ALC1220 and kin), and the PLL fix, coefficient 0x0a
    /// bit 10 (ALC883 and its variants, which include the 10ec:1168 "ALC1220").
    fn realtek_init(&mut self, codec: u32, vendor: u32) {
        let clear = |h: &mut Ctl, idx: u32, bit: u32| {
            h.cmd(codec, 0x20, 0x500 << 8 | idx);
            if let Some(v) = h.cmd(codec, 0x20, 0xC00 << 8) {
                h.cmd(codec, 0x20, 0x500 << 8 | idx);
                h.cmd(codec, 0x20, 0x400 << 8 | (v & !bit & 0xffff));
                kprintln!("hda: Realtek coefficient {:#x}: {:#06x} -> {:#06x}", idx, v & 0xffff, v & !bit & 0xffff);
            }
        };
        if matches!(vendor, 0x10ec0899 | 0x10ec0900 | 0x10ec0b00 | 0x10ec1168 | 0x10ec1220) {
            clear(self, 0x07, 1 << 1);
        }
        if matches!(vendor, 0x10ec0883 | 0x10ec0887 | 0x10ec0888 | 0x10ec0889 | 0x10ec0899 | 0x10ec1168) {
            clear(self, 0x0a, 1 << 10);
        }
    }

    /// Path from widget `nid` to a converter: (widget, connection index used) pairs, ending at
    /// the converter (whose index is None).
    fn find_dac(&mut self, codec: u32, nid: u32, types: &[(u32, u32)], path: &mut Vec<(u32, Option<u32>)>, depth: u32) -> bool {
        let kind = types.iter().find(|t| t.0 == nid).map(|t| t.1);
        if kind == Some(0) {
            path.push((nid, None));
            return true;
        }
        if depth > 5 || kind.is_none() || path.iter().any(|p| p.0 == nid) {
            return false;
        }
        let conns = self.connections(codec, nid);
        // Prefer a converter directly connected.
        let mut order: Vec<(usize, u32)> = conns.iter().copied().enumerate().collect();
        order.sort_by_key(|&(_, c)| if types.iter().any(|t| t.0 == c && t.1 == 0) { 0 } else { 1 });
        for (i, c) in order {
            path.push((nid, Some(i as u32)));
            if self.find_dac(codec, c, types, path, depth + 1) {
                return true;
            }
            path.pop();
        }
        false
    }

    /// Something plugged into this jack (None: it can't tell).
    fn plugged(&mut self, o: usize) -> Option<bool> {
        let (codec, pin, detect, trigger) = (self.outs[o].codec, self.outs[o].pin, self.outs[o].detect, self.outs[o].trigger);
        if !detect {
            return None;
        }
        if trigger {
            self.cmd(codec, pin, 0x709 << 8); // look now
        }
        self.cmd(codec, pin, 0xF09 << 8).map(|v| v & 0x8000_0000 != 0)
    }

    /// Switch one jack on: every widget on its path, the converter on our stream.
    fn jack_on(&mut self, o: usize) {
        let out = self.outs[o].clone();
        let Some(c) = self.codecs.iter().find(|c| c.addr == out.codec) else { return };
        let (afg_caps, types) = (c.afg_caps, c.types.clone());
        let codec = out.codec;
        for (i, &(w, sel)) in out.path.iter().enumerate() {
            self.cmd(codec, w, 0x705 << 8);
            let kind = types.iter().find(|t| t.0 == w).map(|t| t.1).unwrap_or(0);
            if let Some(idx) = sel {
                if kind != 2 {
                    self.cmd(codec, w, 0x701 << 8 | idx); // selector / pin input choice
                }
                self.unmute(codec, w, false, idx, afg_caps);
            }
            self.unmute(codec, w, true, 0, afg_caps);
            if i == out.path.len() - 1 {
                // The converter.
                self.cmd(codec, w, 0x706 << 8 | TAG << 4);
                self.cmd(codec, w, 0x2 << 16 | FORMAT as u32);
                if out.digital {
                    self.cmd(codec, w, 0x70D << 8 | 0x01); // digital on
                }
            }
        }
        self.cmd(codec, out.pin, 0x707 << 8 | 0x40 | if out.hp { 0x80 } else { 0 });
        if out.eapd {
            self.cmd(codec, out.pin, 0x70C << 8 | 0x02); // external amplifier on
        }
    }

    /// Everything quiet: every pin's output off, every converter off our stream.
    fn all_off(&mut self) {
        let outs: Vec<(u32, u32, Option<u32>)> = self.outs.iter().map(|o| (o.codec, o.pin, o.path.last().map(|p| p.0))).collect();
        for (codec, pin, conv) in outs {
            self.cmd(codec, pin, 0x707 << 8);
            if let Some(cv) = conv {
                self.cmd(codec, cv, 0x706 << 8);
            }
        }
        let digi: Vec<(u32, u32, Vec<u32>)> = self.digi.iter().map(|d| (d.codec, d.pin, d.convs.clone())).collect();
        for (codec, pin, convs) in digi {
            self.cmd(codec, pin, 0x707 << 8);
            for cv in convs {
                self.cmd(codec, cv, 0x706 << 8);
            }
        }
    }

    /// Play to a monitor on digital pin `d`, device entry `dev` (its head): the converter, the
    /// channel slots, the audio infoframe and the pin (Linux hdmi.c, nvhdmi.c).
    fn monitor_on(&mut self, d: usize, dev: u32) {
        let (codec, pin) = (self.digi[d].codec, self.digi[d].pin);
        let Some(&conv) = self.digi[d].convs.first() else { return };
        // No other pin may take this converter's stream (nvhdmi_not_share_assigned_cvt).
        let others: Vec<(u32, Vec<u32>)> = self.digi.iter().filter(|o| o.pin != pin).map(|o| (o.pin, o.convs.clone())).collect();
        for (p, convs) in others {
            let devices = (self.param(codec, p, 0x15) & 0x3f) + 1;
            for e in 0..devices.min(8) {
                self.cmd(codec, p, 0x735 << 8 | e);
                let cur = self.cmd(codec, p, 0xF01 << 8).unwrap_or(0) as usize;
                if convs.get(cur) == Some(&conv) {
                    if let Some(i) = convs.iter().position(|&c| c != conv) {
                        self.cmd(codec, p, 0x701 << 8 | i as u32);
                    }
                }
            }
        }
        self.cmd(codec, pin, 0x735 << 8 | dev);
        self.cmd(codec, pin, 0x701 << 8);
        let afg_caps = self.codecs.iter().find(|c| c.addr == codec).map(|c| c.afg_caps).unwrap_or((0, 0));
        self.unmute(codec, pin, true, 0, afg_caps);
        // Converter channel 0 to the front left slot, 1 to front right, the others unused.
        for v in [0x00u32, 0x11, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7] {
            self.cmd(codec, pin, 0x734 << 8 | v);
        }
        // The audio infoframe for 2 channels, laid out as NVIDIA's hardware expects for
        // DisplayPort (like HDMI's, version 0x1b).
        let mut ai = [0x84u8, 0x1b, 0x44, 0, 0x01, 0, 0, 0, 0];
        ai[3] = 0u8.wrapping_sub(ai.iter().fold(0u8, |s, &b| s.wrapping_add(b)));
        self.cmd(codec, pin, 0x730 << 8);
        self.cmd(codec, pin, 0x732 << 8); // stop sending it
        self.cmd(codec, pin, 0x730 << 8);
        for b in ai {
            self.cmd(codec, pin, 0x731 << 8 | b as u32);
        }
        self.cmd(codec, pin, 0x730 << 8);
        self.cmd(codec, pin, 0x732 << 8 | 0xC0); // send it, best effort
        self.cmd(codec, pin, 0x707 << 8 | 0x40);
        // The converter: two channels, digital on, our stream and format.
        self.cmd(codec, conv, 0x705 << 8);
        self.cmd(codec, conv, 0x72D << 8 | 1);
        self.cmd(codec, conv, 0x70D << 8 | 0x01);
        self.cmd(codec, conv, 0x706 << 8 | TAG << 4);
        self.cmd(codec, conv, 0x2 << 16 | FORMAT as u32);
        let sense = self.cmd(codec, pin, 0xF09 << 8).unwrap_or(0);
        kprintln!(
            "hda: monitor pin {} device {} via converter {}: {}, {}",
            pin, dev, conv, if sense & 0x8000_0000 != 0 { "monitor present" } else { "no monitor present" }, if sense & 0x4000_0000 != 0 { "description (ELD) valid" } else { "no description (ELD)" }
        );
    }

    fn setup_stream(&mut self) {
        let sd = self.sd;
        // Reset the stream.
        self.w8(sd, self.r8(sd) | 1);
        self.wait(10, |h| h.r8(sd) & 1 != 0);
        self.w8(sd, self.r8(sd) & !1);
        self.wait(10, |h| h.r8(sd) & 1 == 0);
        // Two buffer descriptors: the two halves of the ring.
        let half = (RING_BYTES / 2) as u64;
        let p = self.ring.phys();
        let bdl = self.bdl.ptr::<u32>();
        for i in 0..2u64 {
            let a = p + i * half;
            unsafe {
                write_volatile(bdl.add(i as usize * 4), a as u32);
                write_volatile(bdl.add(i as usize * 4 + 1), (a >> 32) as u32);
                write_volatile(bdl.add(i as usize * 4 + 2), half as u32);
                write_volatile(bdl.add(i as usize * 4 + 3), 0);
            }
        }
        flush_line(bdl as usize);
        fence();
        let b = self.bdl.phys();
        self.w32(sd + 0x18, b as u32);
        self.w32(sd + 0x1C, (b >> 32) as u32);
        self.w32(sd + 0x08, RING_BYTES as u32);
        self.w16(sd + 0x0C, 1);
        self.w16(sd + 0x12, FORMAT);
        // Stream tag in bits 23:20 of the control register (byte 2).
        self.w8(sd + 2, (TAG << 4) as u8);
    }

    fn lpib(&self) -> u32 {
        self.r32(self.sd + 0x04)
    }

    fn update_played(&mut self) {
        if !self.running {
            return;
        }
        let l = self.lpib();
        let d = (l + RING_BYTES as u32 - self.last_lpib) % RING_BYTES as u32;
        self.last_lpib = l;
        self.played += d as u64 / 4;
    }

    /// Start the stream with the clock at `played` frames and `written` frames queued (to
    /// carry on after a change of output).
    fn start_at(&mut self, played: u64, written: u64) {
        self.ring.bytes_mut().fill(0);
        let base = self.ring.ptr::<u8>() as usize;
        for off in (0..RING_BYTES).step_by(64) {
            flush_line(base + off);
        }
        fence();
        self.written = written.max(played);
        self.played = played;
        self.w8(self.sd + 3, 0x1C); // clear status
        self.w8(self.sd, self.r8(self.sd) | 2);
        self.running = true;
        self.last_lpib = self.lpib();
    }

    fn stop(&mut self) {
        self.w8(self.sd, self.r8(self.sd) & !2);
        self.running = false;
    }

    /// Copy frames into the ring at absolute frame position `at`, and write them back from
    /// the caches.
    fn put(&mut self, at: u64, pcm: &[i16], vol: u32) {
        let ring = self.ring.ptr::<i16>();
        for (i, s) in pcm.iter().enumerate() {
            let f = (at as usize * 2 + i) % (RING_FRAMES * 2);
            let v = if vol >= 256 { *s } else { ((*s as i32 * vol as i32) >> 8) as i16 };
            unsafe { write_volatile(ring.add(f), v) };
        }
        let base = self.ring.ptr::<u8>() as usize;
        let (start, len) = ((at as usize % RING_FRAMES) * 4, (pcm.len() * 2).min(RING_BYTES));
        let mut a = start & !63;
        while a < start + len {
            flush_line(base + a % RING_BYTES);
            a += 64;
        }
        fence();
    }
}

/// What a jack is, from its pin's configuration: a name, where it is, and its order for the
/// automatic choice. None for jacks Solon doesn't play to.
fn describe(config: u32) -> Option<(String, &'static str, u32)> {
    let device = (config >> 20) & 0xF;
    let connectivity = config >> 30;
    let (gross, geo) = ((config >> 28) & 3, (config >> 24) & 0xF);
    let place = if connectivity == 2 || gross == 1 {
        "Inside the computer"
    } else {
        match geo {
            1 => "Back of the computer",
            2 => "Front of the computer",
            5 => "Top of the computer",
            _ => "On the computer",
        }
    };
    let colour = match (config >> 12) & 0xF {
        1 => "black",
        2 => "grey",
        3 => "blue",
        4 => "green",
        5 => "red",
        6 => "orange",
        7 => "yellow",
        8 => "purple",
        9 => "pink",
        0xE => "white",
        _ => "",
    };
    let (name, rank) = match device {
        2 => (String::from("Headphones"), 0),
        1 if connectivity == 2 => (String::from("Built-in speakers"), 2),
        1 => (String::from("Speakers"), 2),
        0 => match colour {
            "green" => (String::from("Speakers (green jack)"), 1),
            "black" => (String::from("Rear speakers (black jack)"), 3),
            "orange" => (String::from("Centre and subwoofer (orange jack)"), 4),
            "grey" => (String::from("Side speakers (grey jack)"), 4),
            "" => (String::from("Line out"), 3),
            c => (format!("Line out ({} jack)", c), 3),
        },
        4 => (String::from(if (config >> 16) & 0xF == 5 { "Optical (S/PDIF)" } else { "Digital (S/PDIF)" }), 5),
        _ => return None,
    };
    Some((name, place, rank))
}

impl Sound {
    /// Every monitor that takes sound and the controller and pin that play to it.
    fn monitors(&self) -> Vec<(u32, usize, usize, crate::nvidia::audio::AudioMonitor)> {
        let mut v = Vec::new();
        for m in crate::nvidia::audio::monitors() {
            for (ci, c) in self.ctls.iter().enumerate() {
                if c.vendor != 0x10de {
                    continue;
                }
                if let Some(di) = c.digi.iter().position(|d| d.pin == m.sor + 4) {
                    let key = MONITOR | m.head << 27 | key_of(c.bdf, c.digi[di].codec, c.digi[di].pin);
                    v.push((key, ci, di, m.clone()));
                    break;
                }
            }
        }
        v
    }

    /// The USB sound devices plugged in: (key, name, kind).
    fn usb_devices(&self) -> Vec<(u32, String, &'static str)> {
        crate::usbaudio::DEVICES.lock().iter().map(|(id, name, kind)| (USB | id, name.clone(), *kind)).collect()
    }

    /// The jack with `key`: (controller, output).
    fn jack(&self, key: u32) -> Option<(usize, usize)> {
        self.ctls.iter().enumerate().find_map(|(ci, c)| c.outs.iter().position(|o| o.key == key).map(|oi| (ci, oi)))
    }

    /// What to play through for `want` (AUTO, ALL_JACKS or a key): a key or ALL_JACKS.
    fn target(&mut self, want: u32) -> Option<u32> {
        let any_jack = self.ctls.iter().any(|c| !c.outs.is_empty());
        if want == ALL_JACKS && any_jack {
            return Some(ALL_JACKS);
        }
        if want & MONITOR != 0 && self.monitors().iter().any(|m| m.0 == want) {
            return Some(want);
        }
        if want & MONITOR == 0 && want & USB != 0 && self.usb_devices().iter().any(|d| d.0 == want) {
            return Some(want);
        }
        if want > ALL_JACKS && want & (MONITOR | USB) == 0 && self.jack(want).is_some() {
            return Some(want);
        }
        // Automatic: a USB sound device (plugged in to be used), then a jack with something
        // plugged in, headphones first.
        if let Some(d) = self.usb_devices().first() {
            return Some(d.0);
        }
        let mut best: Option<(u32, u32)> = None;
        for ci in 0..self.ctls.len() {
            for oi in 0..self.ctls[ci].outs.len() {
                let (rank, key, builtin) = (self.ctls[ci].outs[oi].rank, self.ctls[ci].outs[oi].key, self.ctls[ci].outs[oi].builtin);
                let plugged = self.ctls[ci].plugged(oi) == Some(true);
                if plugged && !builtin && best.is_none_or(|b| rank < b.0) {
                    best = Some((rank, key));
                }
            }
        }
        if let Some((_, key)) = best {
            return Some(key);
        }
        // Built-in speakers, then a monitor's speakers, then every jack.
        if let Some(o) = self.ctls.iter().flat_map(|c| c.outs.iter()).find(|o| o.builtin) {
            return Some(o.key);
        }
        let monitors = self.monitors();
        if let Some(m) = monitors.iter().find(|m| m.3.speakers) {
            return Some(m.0);
        }
        if any_jack {
            return Some(ALL_JACKS);
        }
        monitors.first().map(|m| m.0)
    }

    /// What plays now: (running, frames played, frames written).
    fn clock(&mut self) -> (bool, u64, u64) {
        if self.usb_out {
            let st = crate::usbaudio::STREAM.lock();
            return (st.running, st.played, st.written);
        }
        match self.ctls.get_mut(self.active) {
            Some(c) => {
                c.update_played();
                (c.running, c.played, c.written)
            }
            None => (false, 0, 0),
        }
    }

    fn stop_out(&mut self) {
        if self.usb_out {
            crate::usbaudio::STREAM.lock().running = false;
        } else if let Some(c) = self.ctls.get_mut(self.active) {
            if c.running {
                c.stop();
            }
        }
    }

    fn start_out(&mut self, played: u64, written: u64) {
        if self.usb_out {
            crate::usbaudio::STREAM.lock().start_at(played, written);
        } else if let Some(c) = self.ctls.get_mut(self.active) {
            c.start_at(played, written);
        }
    }

    /// Queue frames at `at` (the next to be written) on whatever plays now.
    fn put_out(&mut self, pcm: &[i16]) {
        let vol = self.volume;
        if self.usb_out {
            crate::usbaudio::STREAM.lock().queue(pcm, vol);
            return;
        }
        let Some(c) = self.ctls.get_mut(self.active) else { return };
        let at = c.written;
        c.put(at, pcm, vol);
        c.written += (pcm.len() / 2) as u64;
        // Clear a little beyond, so that running dry plays silence rather than old sound.
        let w = c.written;
        let silence = [0i16; 2048];
        let mut clear = (RATE as usize / 5) * 2;
        let mut pos = w;
        while clear > 0 {
            let n = clear.min(silence.len());
            c.put(pos, &silence[..n], 256);
            pos += (n / 2) as u64;
            clear -= n;
        }
    }

    /// Play through `target` (from `target()`), carrying on whatever is playing.
    fn route(&mut self, target: u32) {
        if self.using == Some(target) {
            return;
        }
        let (running, played, written) = self.clock();
        if running {
            self.stop_out();
        }
        for c in self.ctls.iter_mut() {
            c.all_off();
        }
        self.usb_out = false;
        crate::usbaudio::STREAM.lock().chosen = None;
        let mut name = String::new();
        if target & MONITOR == 0 && target & USB != 0 {
            crate::usbaudio::STREAM.lock().chosen = Some(target & !USB);
            self.usb_out = true;
            name = self.usb_devices().into_iter().find(|d| d.0 == target).map(|d| d.1).unwrap_or_default();
        } else if target == ALL_JACKS {
            if let Some(ci) = self.ctls.iter().position(|c| !c.outs.is_empty()) {
                for oi in 0..self.ctls[ci].outs.len() {
                    self.ctls[ci].jack_on(oi);
                }
                self.active = ci;
                name = String::from("every jack");
            }
        } else if target & MONITOR != 0 {
            if let Some((_, ci, di, m)) = self.monitors().into_iter().find(|m| m.0 == target) {
                match crate::nvidia::audio::switch_on(&m) {
                    Ok(msg) => kprintln!("hda: monitor sound on: {}", msg),
                    Err(e) => kprintln!("hda: monitor sound: {}", e),
                }
                self.ctls[ci].monitor_on(di, m.head);
                self.active = ci;
                name = m.name.clone();
            }
        } else if let Some((ci, oi)) = self.jack(target) {
            self.ctls[ci].jack_on(oi);
            self.active = ci;
            name = format!("{} ({})", self.ctls[ci].outs[oi].name, self.ctls[ci].outs[oi].place);
        }
        self.using = Some(target);
        kprintln!("hda: playing through {}", name);
        if running {
            self.start_out(played, written);
        }
    }

    fn name_of(&self, key: u32) -> String {
        if key == ALL_JACKS {
            return String::from("Every jack at once");
        }
        if let Some((ci, oi)) = self.jack(key) {
            return self.ctls[ci].outs[oi].name.clone();
        }
        if let Some(d) = self.usb_devices().into_iter().find(|d| d.0 == key) {
            return d.1;
        }
        self.monitors().into_iter().find(|m| m.0 == key).map(|m| m.3.name).unwrap_or_default()
    }
}

// ---- the interface the apps use ------------------------------------------------------

pub fn init(devices: &[pci::Device]) {
    let ctls: Vec<Ctl> = devices.iter().filter(|d| d.class == 0x04 && d.subclass == 0x03).filter_map(Ctl::init).collect();
    if ctls.is_empty() {
        kprintln!("hda: no HD Audio sound (USB sound devices can still play)");
    }
    let mut s = Sound { ctls, active: 0, usb_out: false, chosen: AUTO, using: None, volume: 256, test_end: None, last_look: 0 };
    if let Some(t) = s.target(AUTO) {
        s.route(t);
    }
    *SOUND.lock() = Some(s);
}

pub fn available() -> bool {
    SOUND.lock().as_ref().is_some_and(|s| s.using.is_some())
}

/// Every output, for Settings: jacks (with whether something is plugged in), monitors and
/// USB sound devices.
pub fn outputs() -> Vec<OutputInfo> {
    let mut g = SOUND.lock();
    let Some(s) = g.as_mut() else { return Vec::new() };
    let mut v = Vec::new();
    for (key, name, kind) in s.usb_devices() {
        v.push(OutputInfo { key, name, place: format!("USB {}", kind), plugged: None, monitor: false, usb: true });
    }
    for ci in 0..s.ctls.len() {
        let mut order: Vec<usize> = (0..s.ctls[ci].outs.len()).collect();
        order.sort_by_key(|&oi| s.ctls[ci].outs[oi].rank);
        for oi in order {
            let plugged = s.ctls[ci].plugged(oi);
            let o = &s.ctls[ci].outs[oi];
            v.push(OutputInfo { key: o.key, name: o.name.clone(), place: String::from(o.place), plugged, monitor: false, usb: false });
        }
    }
    for (key, _, _, m) in s.monitors() {
        let place = if m.speakers { "Its speakers, through the DisplayPort cable" } else { "Its headphone jack, through the DisplayPort cable" };
        v.push(OutputInfo { key, name: m.name, place: String::from(place), plugged: None, monitor: true, usb: false });
    }
    v
}

/// Play through `key` (AUTO, ALL_JACKS or an output's key) from now on.
pub fn choose(key: u32) {
    let mut g = SOUND.lock();
    let Some(s) = g.as_mut() else { return };
    s.chosen = key;
    if s.test_end.is_none() {
        if let Some(t) = s.target(key) {
            s.route(t);
        }
    }
}

/// The output playing now (its name), and whether it was chosen automatically.
pub fn using() -> Option<(String, bool)> {
    let g = SOUND.lock();
    let s = g.as_ref()?;
    Some((s.name_of(s.using?), s.chosen == AUTO))
}

/// How loud everything is, in percent.
pub fn set_volume(percent: u8) {
    if let Some(s) = SOUND.lock().as_mut() {
        s.volume = (percent.min(100) as u32 * 256).div_ceil(100);
    }
}

/// A short chime through `key` (Settings' Test button), unless something is playing.
pub fn test(key: u32) {
    let mut g = SOUND.lock();
    let Some(s) = g.as_mut() else { return };
    if s.clock().0 && s.test_end.is_none() {
        return;
    }
    let Some(t) = s.target(key) else { return };
    s.stop_out();
    s.route(t);
    // Two notes, a fifth apart, fading out.
    let mut pcm = Vec::new();
    let total = RATE as u64 * 45 / 100;
    for k in 0..total {
        let f = if k < RATE as u64 * 15 / 100 { 660.0 } else { 990.0 };
        let fade = 1.0 - k as f64 / total as f64;
        let v = (libm::sin(k as f64 * f * 2.0 * core::f64::consts::PI / RATE as f64) * 9000.0 * fade) as i16;
        pcm.push(v);
        pcm.push(v);
    }
    s.start_out(0, 0);
    s.put_out(&pcm);
    s.test_end = Some(total + RATE as u64 / 10);
}

/// Called often (the desktop's tick): ends a test sound, and every second follows what's
/// plugged in (when chosen automatically, or when the chosen USB device was unplugged).
pub fn poll(now: u64) {
    let mut g = SOUND.lock();
    let Some(s) = g.as_mut() else { return };
    if let Some(end) = s.test_end {
        let (running, played, _) = s.clock();
        if played >= end || !running {
            s.stop_out();
            s.test_end = None;
            if let Some(t) = s.target(s.chosen) {
                s.route(t);
            }
        }
        return;
    }
    if now >= s.last_look + crate::interrupts::HZ {
        s.last_look = now;
        let gone = s.usb_out && s.using.is_some_and(|k| !s.usb_devices().iter().any(|d| d.0 == k));
        if s.chosen == AUTO || gone || s.using.is_none() {
            if let Some(t) = s.target(s.chosen) {
                if s.using != Some(t) {
                    s.route(t);
                }
            }
        }
    }
}

/// How far ahead of the speakers the player should keep the buffer filled.
pub const LEAD: usize = RATE as usize * 3 / 10;

/// Start playing (silence until sound is queued).
pub fn start() {
    if let Some(s) = SOUND.lock().as_mut() {
        if s.test_end.take().is_some() {
            s.stop_out();
            if let Some(t) = s.target(s.chosen) {
                s.route(t);
            }
        }
        if !s.clock().0 {
            s.start_out(0, 0);
        }
    }
}

pub fn stop() {
    if let Some(s) = SOUND.lock().as_mut() {
        if s.test_end.is_none() {
            s.stop_out();
        }
    }
}

/// Frames the speakers have played since `start` (the clock for keeping pictures in step).
pub fn played() -> u64 {
    match SOUND.lock().as_mut() {
        Some(s) => s.clock().1,
        None => 0,
    }
}

/// How many frames the player may queue now (to stay `LEAD` ahead).
pub fn wanted() -> usize {
    let mut g = SOUND.lock();
    let Some(s) = g.as_mut() else { return 0 };
    if s.test_end.is_some() {
        return 0;
    }
    let (running, played, written) = s.clock();
    if !running {
        return 0;
    }
    // Fell behind (e.g. a long pause): skip ahead.
    let written = if written < played {
        if s.usb_out {
            crate::usbaudio::STREAM.lock().written = played;
        } else if let Some(c) = s.ctls.get_mut(s.active) {
            c.written = played;
        }
        played
    } else {
        written
    };
    LEAD.saturating_sub((written - played) as usize)
}

/// Queue interleaved stereo frames after what's already queued.
pub fn queue(pcm: &[i16]) {
    if let Some(s) = SOUND.lock().as_mut() {
        if s.test_end.is_some() || !s.clock().0 {
            return;
        }
        s.put_out(pcm);
    }
}

/// Boot option "soundtest": half a second of a 440 Hz tone, then 0.3 s of 880 Hz. `pump`
/// keeps USB sound devices fed (usb.rs's poll); it gives up after a few seconds.
pub fn self_test(pump: &mut dyn FnMut()) {
    if !available() {
        kprintln!("hda: sound test: no sound device");
        return;
    }
    start();
    let mut t = 0u64;
    let total = RATE as u64 * 8 / 10;
    let give_up = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed) + 5 * crate::interrupts::HZ;
    while t < total && crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed) < give_up {
        pump();
        let n = wanted().min((total - t) as usize);
        if n > 0 {
            let mut buf = Vec::with_capacity(n * 2);
            for i in 0..n as u64 {
                let k = t + i;
                let f = if k < RATE as u64 / 2 { 440.0 } else { 880.0 };
                let v = (libm::sin(k as f64 * f * 2.0 * core::f64::consts::PI / RATE as f64) * 12000.0) as i16;
                buf.push(v);
                buf.push(v);
            }
            queue(&buf);
            t += n as u64;
        }
        sleep_ms(20);
    }
    // Let it finish playing, with a little silence after (sound still in flight).
    let end = t + RATE as u64 / 3;
    for _ in 0..200 {
        pump();
        if played() >= end {
            break;
        }
        let n = wanted();
        if n > 0 {
            queue(&alloc::vec![0i16; n * 2]);
        }
        sleep_ms(10);
    }
    let p = played();
    stop();
    kprintln!("hda: sound test played {} frames", p);
}

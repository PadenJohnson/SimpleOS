//! Sound: Intel High Definition Audio controllers (every PC since about 2005) and their
//! codecs. Plays 48 kHz 16-bit stereo through every output the codec has (speakers,
//! headphones, line out). Polled: the player tops the ring buffer up a few times a second.

use crate::interrupts::sleep_ms;
use crate::memory::{map_mmio, Dma};
use crate::pci;
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

pub struct Hda {
    base: u64,
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
    /// Software volume, 0..=256.
    pub volume: u32,
    pub outputs: Vec<&'static str>,
}

pub static SOUND: Mutex<Option<Hda>> = Mutex::new(None);

impl Hda {
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
    fn wait(&self, ms: u32, f: impl Fn(&Hda) -> bool) -> bool {
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

    fn init(dev: &pci::Device) -> Option<Hda> {
        let (phys, size) = dev.bar(0)?;
        dev.enable();
        // Intel: clear TCSEL (traffic class) so sound isn't starved on some chipsets.
        if dev.vendor == 0x8086 {
            let v = dev.read(0x44);
            dev.write(0x44, v & !0x07);
        }
        let base = map_mmio(phys, size.max(0x4000));
        let mut h = Hda {
            base,
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
            volume: 256,
            outputs: Vec::new(),
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
            kprintln!("hda: no codecs");
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
        if h.outputs.is_empty() {
            kprintln!("hda: no usable outputs");
            return None;
        }
        h.setup_stream();
        kprintln!("hda: {:04x}:{:04x} ready, outputs: {:?}", dev.vendor, dev.device, h.outputs);
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
        unsafe { write_volatile(self.corb.ptr::<u32>().add(wp as usize), codec << 28 | nid << 20 | verb) };
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
                let name = match device {
                    0 => "line out",
                    1 => "speaker",
                    2 => "headphones",
                    4 | 5 => "digital out",
                    _ => continue,
                };
                if device == 4 || device == 5 {
                    continue; // S/PDIF / HDMI need more setup; skip
                }
                // Find a path to a DAC (depth-first, a few hops).
                let mut path = Vec::new();
                if !self.find_dac(codec, nid, &types, &mut path, 0) {
                    kprintln!("hda: no route from pin {} ({}) to a DAC", nid, name);
                    continue;
                }
                kprintln!("hda: {} on pin {} via {:?}", name, nid, path);
                // Configure every widget on the path.
                for (i, &(w, sel)) in path.iter().enumerate() {
                    self.cmd(codec, w, 0x705 << 8);
                    let kind = types.iter().find(|t| t.0 == w).map(|t| t.1).unwrap_or(0);
                    if let Some(idx) = sel {
                        if kind != 2 {
                            self.cmd(codec, w, 0x701 << 8 | idx); // selector / pin input choice
                        }
                        self.unmute(codec, w, false, idx, afg_caps);
                    }
                    self.unmute(codec, w, true, 0, afg_caps);
                    if i == path.len() - 1 {
                        // The DAC.
                        self.cmd(codec, w, 0x706 << 8 | TAG << 4);
                        self.cmd(codec, w, 0x2 << 16 | FORMAT as u32);
                    }
                }
                let hp = if device == 2 { 0x80 } else { 0 };
                self.cmd(codec, nid, 0x707 << 8 | 0x40 | hp);
                if pincap & (1 << 16) != 0 {
                    self.cmd(codec, nid, 0x70C << 8 | 0x02); // external amplifier on
                }
                self.outputs.push(name);
            }
        }
    }

    /// Path from widget `nid` to a DAC: (widget, connection index used) pairs, ending at
    /// the DAC (whose index is None).
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
        // Prefer a DAC directly connected.
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

    fn start(&mut self) {
        let ring = self.ring.bytes_mut();
        ring.fill(0);
        self.written = 0;
        self.played = 0;
        self.last_lpib = 0;
        self.w8(self.sd + 3, 0x1C); // clear status
        self.w8(self.sd, self.r8(self.sd) | 2);
        self.running = true;
        self.last_lpib = self.lpib();
    }

    fn stop(&mut self) {
        self.w8(self.sd, self.r8(self.sd) & !2);
        self.running = false;
    }

    /// Copy frames into the ring at absolute frame position `at`.
    fn put(&mut self, at: u64, pcm: &[i16]) {
        let vol = self.volume as i32;
        let ring = self.ring.ptr::<i16>();
        for (i, s) in pcm.iter().enumerate() {
            let f = (at as usize * 2 + i) % (RING_FRAMES * 2);
            let v = if vol >= 256 { *s } else { ((*s as i32 * vol) >> 8) as i16 };
            unsafe { write_volatile(ring.add(f), v) };
        }
    }
}

// ---- the interface the apps use ------------------------------------------------------

pub fn init(devices: &[pci::Device]) {
    for d in devices.iter().filter(|d| d.class == 0x04 && d.subclass == 0x03) {
        if let Some(h) = Hda::init(d) {
            *SOUND.lock() = Some(h);
            return;
        }
    }
}

pub fn available() -> bool {
    SOUND.lock().is_some()
}

/// How far ahead of the speakers the player should keep the buffer filled.
pub const LEAD: usize = RATE as usize * 3 / 10;

/// Start playing (silence until sound is queued).
pub fn start() {
    if let Some(h) = SOUND.lock().as_mut() {
        if !h.running {
            h.start();
        }
    }
}

pub fn stop() {
    if let Some(h) = SOUND.lock().as_mut() {
        if h.running {
            h.stop();
        }
    }
}

/// Frames the speakers have played since `start` (the clock for keeping pictures in step).
pub fn played() -> u64 {
    match SOUND.lock().as_mut() {
        Some(h) => {
            h.update_played();
            h.played
        }
        None => 0,
    }
}

/// How many frames the player may queue now (to stay `LEAD` ahead).
pub fn wanted() -> usize {
    match SOUND.lock().as_mut() {
        Some(h) if h.running => {
            h.update_played();
            // Fell behind (e.g. a long pause): skip ahead.
            if h.written < h.played {
                h.written = h.played;
            }
            let ahead = (h.written - h.played) as usize;
            LEAD.saturating_sub(ahead)
        }
        _ => 0,
    }
}

/// Queue interleaved stereo frames after what's already queued, and clear a little
/// beyond them so that running dry plays silence rather than old sound.
pub fn queue(pcm: &[i16]) {
    if let Some(h) = SOUND.lock().as_mut() {
        if !h.running {
            return;
        }
        let at = h.written;
        h.put(at, pcm);
        h.written += (pcm.len() / 2) as u64;
        let w = h.written;
        let silence = [0i16; 2048];
        let mut clear = (RATE as usize / 5) * 2;
        let mut pos = w;
        while clear > 0 {
            let n = clear.min(silence.len());
            h.put(pos, &silence[..n]);
            pos += (n / 2) as u64;
            clear -= n;
        }
    }
}

/// Boot option "soundtest": half a second of a 440 Hz tone, then 0.3 s of 880 Hz.
pub fn self_test() {
    if !available() {
        kprintln!("hda: sound test: no sound device");
        return;
    }
    start();
    let mut t = 0u64;
    let total = RATE as u64 * 8 / 10;
    while t < total {
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

//! USB sound devices: USB Audio Class 1.0 and 2.0 (headsets, USB speakers and headphones,
//! headphone DACs, microphones with a headphone socket). Sound goes out through the
//! device's isochronous OUT endpoint on a USB 3 (xHCI) controller, as Linux's snd-usb-audio
//! plays it: a 48 kHz PCM format is chosen (stereo and 16-bit when offered), its interface
//! setting selected, the rate set (Audio Class 1: on the endpoint; 2: on the clock source),
//! the device unmuted, and a few tens of milliseconds of packets kept queued, one per
//! service interval.
//!
//! The player writes sound into `STREAM` (through hda.rs, which also lists these devices in
//! Settings > Sound); usb.rs calls `Device::pump` from its poll, which turns it into packets.
//! The device's own feedback (for "asynchronous" devices) isn't read: the rate is the
//! nominal 48 kHz.

use alloc::collections::VecDeque;
use alloc::string::String;
use alloc::vec::Vec;
use spin::Mutex;

use crate::hc::Hc;
use crate::memory::Dma;
use crate::xhci::dma_buf;

/// The sample rate Solon plays at.
const RATE: u32 = 48_000;
/// Sound kept queued: about this many milliseconds (so a busy moment in the main loop doesn't
/// leave the device without sound), in groups of about 4 ms; at most this many packets (the
/// controller's ring holds 1024).
const QUEUED_MS: usize = 160;
const GROUP_MS: usize = 4;
const MAX_PACKETS: usize = 900;

/// A playback format: one alternate setting of a streaming interface.
#[derive(Clone, Debug)]
pub struct Format {
    pub iface: u8,
    pub alt: u8,
    pub channels: u8,
    /// Bytes per sample (2, 3 or 4) and the bits used.
    pub bytes: u8,
    pub bits: u8,
    /// The isochronous OUT endpoint: address, attributes, max packet size, bInterval and
    /// extra packets per interval (high-bandwidth).
    pub ep: u8,
    pub attr: u8,
    pub max_packet: u16,
    pub interval: u8,
    pub burst: u8,
    /// Sample rates offered (Audio Class 1; empty: a range, or Audio Class 2).
    pub rates: Vec<u32>,
}

/// What a USB sound device offers, from its configuration descriptor.
#[derive(Clone, Debug)]
pub struct Info {
    pub ac_iface: u8,
    /// USB Audio Class 2.0 (else 1.0).
    pub v2: bool,
    /// Audio Class 2: the clock source that sets the rate.
    pub clock: Option<u8>,
    /// Feature units with a mute control (unmuted when sound starts).
    pub mute: Vec<u8>,
    /// What plays the sound: "headset", "headphones", "speakers"...
    pub kind: &'static str,
    pub format: Format,
    /// Every interface of the audio function (so usb.rs doesn't look for other drivers).
    pub ifaces: Vec<u8>,
}

fn le16(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes([b[i], b[i + 1]])
}

/// Find a USB sound device's playback format in a configuration descriptor.
pub fn parse(cfg: &[u8]) -> Option<Info> {
    let (mut ac_iface, mut v2, mut clock, mut mute, mut kind) = (None, false, None, Vec::new(), "sound device");
    let mut ifaces = Vec::new();
    let mut formats: Vec<Format> = Vec::new();
    // The interface being read: (number, alt, subclass), and its format so far.
    let mut cur: Option<(u8, u8, u8)> = None;
    let mut pcm = false;
    let mut fmt: Option<Format> = None;
    let mut i = 0;
    while i + 2 <= cfg.len() {
        let len = cfg[i] as usize;
        if len < 2 || i + len > cfg.len() {
            break;
        }
        let b = &cfg[i..i + len];
        match b[1] {
            4 if len >= 9 => {
                if let Some(f) = fmt.take() {
                    if pcm && f.ep != 0 {
                        formats.push(f);
                    }
                }
                pcm = false;
                cur = (b[5] == 1).then_some((b[2], b[3], b[6]));
                if let Some((num, alt, sub)) = cur {
                    if !ifaces.contains(&num) {
                        ifaces.push(num);
                    }
                    if sub == 1 && ac_iface.is_none() {
                        ac_iface = Some(num);
                    }
                    if sub == 2 && alt != 0 {
                        fmt = Some(Format { iface: num, alt, channels: 0, bytes: 0, bits: 0, ep: 0, attr: 0, max_packet: 0, interval: 1, burst: 0, rates: Vec::new() });
                    }
                }
            }
            // Class-specific interface descriptors.
            0x24 if len >= 3 => match (cur, b[2]) {
                (Some((_, _, 1)), 1) if len >= 5 => v2 = le16(b, 3) >= 0x0200,
                (Some((_, _, 1)), 3) if len >= 6 => {
                    kind = match le16(b, 4) {
                        0x0301 | 0x0304 | 0x0305 | 0x0306 => "speakers",
                        0x0302 => "headphones",
                        0x0402 | 0x0404 | 0x0405 => "headset",
                        0x0403 => "speakerphone",
                        _ => kind,
                    };
                }
                (Some((_, _, 1)), 6) if len >= 7 => {
                    let has_mute = if v2 { len >= 9 && b[5] & 3 == 3 } else { b[5] >= 1 && b[6] & 1 != 0 };
                    if has_mute {
                        mute.push(b[3]);
                    }
                }
                (Some((_, _, 1)), 0x0A) if v2 && len >= 6 && clock.is_none() => clock = Some(b[3]),
                (Some((_, _, 2)), 1) => {
                    // AS_GENERAL: PCM? (Class 1: wFormatTag 1; class 2: bmFormats bit 0.)
                    if v2 && len >= 11 {
                        pcm = b[6] & 1 != 0;
                        if let Some(f) = fmt.as_mut() {
                            f.channels = b[10];
                        }
                    } else if len >= 7 {
                        pcm = le16(b, 5) == 1;
                    }
                }
                (Some((_, _, 2)), 2) if b.get(3) == Some(&1) => {
                    // FORMAT_TYPE I.
                    if let Some(f) = fmt.as_mut() {
                        if v2 && len >= 6 {
                            (f.bytes, f.bits) = (b[4], b[5]);
                        } else if len >= 8 {
                            (f.channels, f.bytes, f.bits) = (b[4], b[5], b[6]);
                            let n = b[7] as usize;
                            for k in 0..n {
                                let o = 8 + 3 * k;
                                if o + 3 <= len {
                                    f.rates.push(b[o] as u32 | (b[o + 1] as u32) << 8 | (b[o + 2] as u32) << 16);
                                }
                            }
                            if n == 0 && len >= 14 {
                                let lo = b[8] as u32 | (b[9] as u32) << 8 | (b[10] as u32) << 16;
                                let hi = b[11] as u32 | (b[12] as u32) << 8 | (b[13] as u32) << 16;
                                if !(lo..=hi).contains(&RATE) {
                                    pcm = false;
                                }
                            }
                        }
                    }
                }
                _ => {}
            },
            // The streaming endpoint: isochronous, OUT, data (not feedback).
            5 if len >= 7 => {
                if let Some(f) = fmt.as_mut() {
                    let (addr, attr) = (b[2], b[3]);
                    if addr & 0x80 == 0 && attr & 3 == 1 && (attr >> 4) & 3 != 1 {
                        let mp = le16(b, 4);
                        (f.ep, f.attr, f.max_packet, f.interval, f.burst) = (addr, attr, mp & 0x7FF, b[6].max(1), ((mp >> 11) & 3) as u8);
                    }
                }
            }
            _ => {}
        }
        i += len;
    }
    if let Some(f) = fmt.take() {
        if pcm && f.ep != 0 {
            formats.push(f);
        }
    }
    // Usable: 48 kHz (listed, or not listed at all), 1 or more channels, 16, 24 or 32-bit.
    let usable = |f: &&Format| f.channels >= 1 && matches!(f.bytes, 2 | 3 | 4) && (f.rates.is_empty() || f.rates.contains(&RATE));
    let score = |f: &&Format| (f.channels == 2) as u32 * 8 + (f.channels == 1) as u32 * 2 + (f.bytes == 2) as u32 * 4 + (f.bytes == 4) as u32;
    let format = formats.iter().filter(usable).max_by_key(score)?.clone();
    Some(Info { ac_iface: ac_iface?, v2, clock, mute, kind, format, ifaces })
}

/// Sound on its way to the chosen USB device: written by the player (via hda.rs), read by
/// `Device::pump`.
pub struct Stream {
    /// The device chosen to play through (its id), and whether something is playing.
    pub chosen: Option<u32>,
    pub running: bool,
    /// Stereo 16-bit frames, a second's worth in a ring.
    ring: Vec<i16>,
    /// Frames written by the player, put into packets, and in packets the device has played
    /// (all counted from the start of playing).
    pub written: u64,
    sent: u64,
    pub played: u64,
}

const RING_FRAMES: usize = RATE as usize;

pub static STREAM: Mutex<Stream> = Mutex::new(Stream { chosen: None, running: false, ring: Vec::new(), written: 0, sent: 0, played: 0 });

/// The USB sound devices plugged in: (id, name, kind), for Settings > Sound.
pub static DEVICES: Mutex<Vec<(u32, String, &'static str)>> = Mutex::new(Vec::new());

impl Stream {
    /// Start counting from `played` frames, with `written` already queued (they're not in the
    /// ring: silence until new sound comes).
    pub fn start_at(&mut self, played: u64, written: u64) {
        if self.ring.len() != RING_FRAMES * 2 {
            self.ring = alloc::vec![0; RING_FRAMES * 2];
        } else {
            self.ring.fill(0);
        }
        self.played = played;
        self.written = written.max(played);
        self.sent = self.written;
        self.running = true;
    }

    /// Add frames after those queued; `vol` 0..=256.
    pub fn queue(&mut self, pcm: &[i16], vol: u32) {
        if self.ring.len() != RING_FRAMES * 2 {
            return;
        }
        for (i, &s) in pcm.iter().enumerate() {
            let at = (self.written as usize * 2 + i) % (RING_FRAMES * 2);
            self.ring[at] = if vol >= 256 { s } else { ((s as i32 * vol as i32) >> 8) as i16 };
        }
        self.written += (pcm.len() / 2) as u64;
    }
}

/// A plugged-in USB sound device (owned by usb.rs).
pub struct Device {
    pub id: u32,
    pub ctrl: usize,
    pub slot: u8,
    pub dci: u8,
    pub name: String,
    info: Info,
    full_speed: bool,
    /// Set up to play (its streaming setting chosen and the rate set).
    on: bool,
    /// Packet buffers, used in turn.
    bufs: Dma,
    slot_bytes: usize,
    slots: usize,
    next: usize,
    /// Groups of packets queued: (address of the last packet's TRB, frames of real sound).
    groups: VecDeque<(u64, u64)>,
    frames_per_packet: usize,
    packets_per_group: usize,
    target_groups: usize,
    /// When a group last finished (ticks): a quiet ring for long means it ran dry.
    last_done: u64,
}

impl Device {
    pub fn new(id: u32, ctrl: usize, slot: u8, name: String, info: Info, full_speed: bool) -> Device {
        let f = &info.format;
        // One packet per service interval: 2^(bInterval-1) frames (1 ms) or microframes.
        let period_us = (1usize << (f.interval.clamp(1, 16) - 1)) * if full_speed { 1000 } else { 125 };
        let frames_per_packet = (RATE as usize * period_us / 1_000_000).max(1);
        let packets_per_group = (GROUP_MS * 1000 / period_us).max(1);
        let target_groups = (QUEUED_MS / GROUP_MS).min(MAX_PACKETS / packets_per_group).max(2);
        let frame_bytes = f.channels as usize * f.bytes as usize;
        let slot_bytes = (frames_per_packet * frame_bytes).next_multiple_of(64);
        let slots = packets_per_group * (target_groups + 2);
        kprintln!(
            "usb audio: \"{}\": Audio Class {}, {} channel(s) of {} bits, endpoint {:#x} every {} us ({} frames a packet, max {} bytes)",
            name, if info.v2 { 2 } else { 1 }, f.channels, f.bits, f.ep, period_us, frames_per_packet, f.max_packet
        );
        Device {
            id,
            ctrl,
            slot,
            dci: (f.ep & 0xF) * 2,
            name,
            full_speed,
            on: false,
            bufs: Dma::new(slots * slot_bytes, 4096),
            slot_bytes,
            slots,
            next: 0,
            groups: VecDeque::new(),
            frames_per_packet,
            packets_per_group,
            target_groups,
            last_done: 0,
            info,
        }
    }

    /// What the device is ("headset", "speakers"...).
    pub fn info_kind(&self) -> &'static str {
        self.info.kind
    }

    /// The endpoint to set up when the device is configured.
    pub fn endpoint(&self) -> crate::xhci::EpConfig {
        let f = &self.info.format;
        crate::xhci::EpConfig { dci: self.dci, kind: crate::xhci::EpType::IsochOut, max_packet: f.max_packet, interval: f.interval, max_burst: f.burst }
    }

    /// Choose the streaming setting, set the rate and unmute (Linux: snd_usb_init_sample_rate,
    /// and the mixer's master mute).
    fn switch_on(&mut self, hc: &mut Hc) {
        let f = self.info.format.clone();
        let _ = hc.control(self.slot, 0x01, 0x0B, f.alt as u16, f.iface as u16, None); // SET_INTERFACE
        let mut buf = dma_buf(4);
        buf.bytes_mut()[..4].copy_from_slice(&RATE.to_le_bytes());
        let rate = if self.info.v2 {
            // Audio Class 2: the clock source's frequency (CUR, CS_SAM_FREQ_CONTROL).
            match self.info.clock {
                Some(clk) => hc.control(self.slot, 0x21, 0x01, 0x0100, (clk as u16) << 8 | self.info.ac_iface as u16, Some((&buf, 4))).is_ok(),
                None => false,
            }
        } else {
            // Audio Class 1: on the endpoint (SET_CUR, SAMPLING_FREQ_CONTROL), 3 bytes.
            hc.control(self.slot, 0x22, 0x01, 0x0100, f.ep as u16, Some((&buf, 3))).is_ok()
        };
        let mut off = dma_buf(1);
        off.bytes_mut()[0] = 0;
        for &unit in &self.info.mute.clone() {
            let _ = hc.control(self.slot, 0x21, 0x01, 0x0100, (unit as u16) << 8 | self.info.ac_iface as u16, Some((&off, 1)));
        }
        self.on = true;
        self.groups.clear();
        self.last_done = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        kprintln!("usb audio: playing through \"{}\" (setting {}, rate {}{})", self.name, f.alt, RATE, if rate { "" } else { " assumed" });
    }

    fn switch_off(&mut self, hc: &mut Hc) {
        let f = &self.info.format;
        let _ = hc.control(self.slot, 0x01, 0x0B, 0, f.iface as u16, None);
        self.on = false;
        self.groups.clear();
        kprintln!("usb audio: \"{}\" stopped", self.name);
    }

    /// Called from usb.rs's poll: follow the choice in Settings, count the packets the device
    /// has played, and keep about QUEUED_MS of sound (or silence) queued.
    pub fn pump(&mut self, hc: &mut Hc) {
        let want = STREAM.lock().chosen == Some(self.id);
        if want != self.on {
            if want {
                self.switch_on(hc);
            } else {
                self.switch_off(hc);
            }
        }
        if !self.on {
            return;
        }
        let now = crate::interrupts::TICKS.load(core::sync::atomic::Ordering::Relaxed);
        let mut st = STREAM.lock();
        // Packets played: every group up to the one an event is about.
        for c in hc.take_completions(self.slot, self.dci) {
            if let Some(k) = self.groups.iter().position(|g| g.0 == c.trb) {
                for _ in 0..=k {
                    if let Some((_, real)) = self.groups.pop_front() {
                        st.played += real;
                    }
                }
                self.last_done = now;
            }
        }
        // Nothing back for a fifth of a second: the device ran dry and stopped; start afresh.
        if !self.groups.is_empty() && now > self.last_done + crate::interrupts::HZ / 5 {
            for (_, real) in self.groups.drain(..) {
                st.played += real;
            }
            self.last_done = now;
        }
        let f = &self.info.format;
        let (ch, bytes) = (f.channels as usize, f.bytes as usize);
        while self.groups.len() < self.target_groups {
            let mut packets = Vec::with_capacity(self.packets_per_group);
            let mut real = 0u64;
            for _ in 0..self.packets_per_group {
                let off = self.next * self.slot_bytes;
                self.next = (self.next + 1) % self.slots;
                let out = &mut self.bufs.bytes_mut()[off..off + self.slot_bytes];
                for k in 0..self.frames_per_packet {
                    // A frame of real sound if there's one waiting, else silence.
                    let (l, r) = if st.running && st.sent < st.written && st.ring.len() == RING_FRAMES * 2 {
                        let at = (st.sent as usize % RING_FRAMES) * 2;
                        st.sent += 1;
                        real += 1;
                        (st.ring[at], st.ring[at + 1])
                    } else {
                        (0, 0)
                    };
                    for c in 0..ch {
                        let s = match (ch, c) {
                            (1, _) => ((l as i32 + r as i32) / 2) as i16,
                            (_, 0) => l,
                            (_, 1) => r,
                            _ => 0,
                        };
                        let o = (k * ch + c) * bytes;
                        match bytes {
                            2 => out[o..o + 2].copy_from_slice(&s.to_le_bytes()),
                            3 => out[o..o + 3].copy_from_slice(&((s as i32) << 8).to_le_bytes()[..3]),
                            _ => out[o..o + 4].copy_from_slice(&((s as i32) << 16).to_le_bytes()),
                        }
                    }
                }
                let len = (self.frames_per_packet * ch * bytes) as u32;
                packets.push((self.bufs.phys() + off as u64, len));
            }
            // The packets are read by the controller: out of the caches first.
            fence_buffers(&self.bufs);
            match hc.isoch(self.slot, self.dci, &packets) {
                Ok(trb) => self.groups.push_back((trb, real)),
                Err(_) => break,
            }
        }
        let _ = self.full_speed;
    }
}

/// Write a buffer back from the processor's caches (the controller may not snoop them).
fn fence_buffers(d: &Dma) {
    let base = d.ptr::<u8>() as usize;
    let mut a = base & !63;
    while a < base + d.len() {
        unsafe { core::arch::asm!("clflush [{0}]", in(reg) a, options(nostack, preserves_flags)) };
        a += 64;
    }
    unsafe { core::arch::asm!("mfence", options(nostack, preserves_flags)) };
}

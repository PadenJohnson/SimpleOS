//! Sound to the monitors: DisplayPort audio for each monitor the card shows a picture on (the
//! firmware's and the one Solon lit), as nouveau switches it on (nvkm uoutp hda_eld: gv100
//! sor dp_audio and hda device entry, gf119 sor hda_hpd and hda_eld). The card's own sound
//! function, an HDA codec (hda.rs), then plays to the monitor; its pin for an output is the
//! output's number + 4 and its device entry the head's number (Linux nvhdmi).
//!
//! The monitor's description for the sound function (its ELD) is made from its EDID as Linux's
//! drm_edid_to_eld does. The firmware turns DisplayPort audio on but leaves the stream no room
//! for it (audio symbols 0), so those are set from the head's timing and the link, as in
//! nouveau nv50_disp_super_2_2_dp.

use super::{Bar0, CARD, REPORT};
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

/// A monitor that takes sound over DisplayPort.
#[derive(Clone)]
pub struct AudioMonitor {
    pub name: String,
    /// The output resource (SOR) and the head showing it.
    pub sor: u32,
    pub head: u32,
    /// It has speakers (its EDID says where they are), not only a headphone jack.
    pub speakers: bool,
    eld: Vec<u8>,
}

/// The monitors on the card that take sound (RTX 20 series and newer).
pub fn monitors() -> Vec<AudioMonitor> {
    let card = CARD.lock();
    let Some(c) = card.as_ref() else { return Vec::new() };
    if c.chipset < 0x160 {
        return Vec::new();
    }
    let r = Bar0 { base: c.bar0 };
    let lit = super::light::LIT.lock().as_ref().map(|l| (l.head, l.name.clone()));
    let first = crate::display::INFO.lock().as_ref().and_then(|i| i.monitor.clone());
    let rep = REPORT.lock();
    let nsor = (r.rd(0x610074) >> 8) & 0xf;
    let mut v = Vec::new();
    for head in 0..4u32 {
        if r.rd(0x68a064 + head * 0x400) == 0 {
            continue; // dark
        }
        let Some(sor) = (0..nsor).find(|&s| r.rd(0x688300 + s * 0x20) & (1 << head) != 0) else { continue };
        let proto = (r.rd(0x688300 + sor * 0x20) >> 8) & 0xf;
        if proto != 8 && proto != 9 {
            continue; // DisplayPort only for now (HDMI needs its own packets)
        }
        let name = match &lit {
            Some((h, n)) if *h == head => n.clone(),
            _ => first.clone().unwrap_or_else(|| String::from("Monitor")),
        };
        let edid = rep.as_ref().and_then(|r| r.connectors.iter().find(|c| c.monitor.as_deref() == Some(name.as_str())).map(|c| c.edid.clone())).unwrap_or_default();
        if let Some((eld, speakers)) = eld_from_edid(&edid) {
            v.push(AudioMonitor { name, sor, head, speakers, eld });
        }
    }
    v
}

/// Switch sound on for a monitor. Returns what was done, for the log.
pub fn switch_on(m: &AudioMonitor) -> Result<String, String> {
    let card = CARD.lock();
    let c = card.as_ref().ok_or_else(|| String::from("no NVIDIA card"))?;
    let r = Bar0 { base: c.bar0 };
    let (hoff, soff) = (m.head * 0x800, 0x30 * m.sor + m.head * 4);
    let mut notes = Vec::new();
    // Room for sound in the DisplayPort stream.
    if r.rd(0x616568 + hoff) & 0xffff == 0 {
        notes.push(audio_symbols(&r, m)?);
    }
    // DisplayPort audio on (gv100_sor_dp_audio).
    let before = r.rd(0x616560 + hoff);
    r.mask(0x616560 + hoff, 0x8000_000d, 0x8000_0001);
    let mut waited = 0;
    while r.rd(0x616560 + hoff) & 0x8000_0000 != 0 && waited < 200 {
        super::mdelay(1);
        waited += 1;
    }
    notes.push(format!("DisplayPort audio {:#x} -> {:#x}{}", before, r.rd(0x616560 + hoff), if waited >= 200 { " (didn't settle)" } else { "" }));
    // "Plugged in" to the sound function on this head's device entry, then the monitor's ELD.
    r.mask(0x616528 + hoff, 0x70, m.head << 4);
    r.mask(0x10ec10 + soff, 0x8000_0001, 0x8000_0001);
    for i in 0..0x60u32 {
        r.wr(0x10ec00 + soff, (i << 8) | *m.eld.get(i as usize).unwrap_or(&0) as u32);
    }
    r.mask(0x10ec10 + soff, 0x8000_0002, 0x8000_0002);
    notes.push(format!("ELD {} bytes, sound function state {:#x}", m.eld.len(), r.rd(0x10ec10 + soff)));
    Ok(format!("{} (head {}, output {}): {}", m.name, m.head, m.sor, notes.join("; ")))
}

/// The DisplayPort stream's room for sound in the blanking periods (nouveau
/// nv50_disp_super_2_2_dp), from the head's armed timing and the output's link.
fn audio_symbols(r: &Bar0, m: &AudioMonitor) -> Result<String, String> {
    let h4 = m.head * 0x400;
    let (total, bend, bstart) = (r.rd(0x68a064 + h4), r.rd(0x68a06c + h4), r.rd(0x68a070 + h4));
    let khz = (r.rd(0x68a00c + h4) / 1000).max(1) as i64;
    let soff = m.sor * 0x800;
    let link_b = (r.rd(0x688300 + m.sor * 0x20) >> 8) & 0xf == 9;
    let ctrl = r.rd(0x61c10c + soff + if link_b { 0x80 } else { 0 });
    let nr = ((ctrl >> 16) & 0xf).count_ones().max(1) as i64;
    let ef = ((ctrl >> 14) & 1) as i64;
    let bw: i64 = match (r.rd(0x612300 + soff) >> 18) & 0x1f {
        0 => 0x06,
        1 => 0x0a,
        2 => 0x14,
        3 => 0x1e,
        4 => 0x08,
        5 => 0x09,
        6 => 0x0c,
        7 => 0x10,
        x => return Err(format!("unknown link rate setting {:#x}", x)),
    };
    let link_kbps = bw * 27_000;
    let (htotal, hblanke, hblanks) = ((total & 0xffff) as i64, (bend & 0xffff) as i64, (bstart & 0xffff) as i64);
    let (vblanke, vblanks) = ((bend >> 16) as i64, (bstart >> 16) as i64);
    let h = (hblanke + htotal - hblanks - 7) * link_kbps / khz - 3 * ef - 12 / nr;
    let v = (vblanks - vblanke - 25) * link_kbps / khz - (36 / nr + 3) - 1;
    if h <= 0 || v <= 0 {
        return Err(format!("no room for sound in this mode (hblank symbols {}, vblank symbols {})", h, v));
    }
    let hoff = m.head * 0x800;
    r.mask(0x616568 + hoff, 0xffff, h as u32);
    r.mask(0x61656c + hoff, 0xff_ffff, v as u32);
    Ok(format!("audio symbols {} and {} ({} kHz, {} lanes at {}.{} Gbit/s)", h, v, khz, nr, bw * 27 / 100, bw * 27 / 10 % 10))
}

/// The monitor's description for the sound function (ELD, version 2), from its EDID's CEA
/// extension: its name, maker and product codes, the sound it takes and its speakers.
/// None if it takes no sound. Also says whether it has speakers.
fn eld_from_edid(e: &[u8]) -> Option<(Vec<u8>, bool)> {
    if e.len() < 256 || e[128] != 2 {
        return None;
    }
    let c = &e[128..256];
    let dtd = (c[2] as usize).clamp(4, 127);
    let (mut sads, mut speakers) = (Vec::new(), None);
    let mut i = 4;
    while i < dtd {
        let (tag, len) = (c[i] >> 5, (c[i] & 31) as usize);
        let blk = &c[(i + 1).min(dtd)..(i + 1 + len).min(dtd)];
        match tag {
            1 => sads.extend(blk.chunks_exact(3).flatten().copied()),
            4 if !blk.is_empty() => speakers = Some(blk[0]),
            _ => {}
        }
        i += 1 + len;
    }
    if sads.is_empty() {
        if c[3] & 0x40 == 0 {
            return None; // no sound at all
        }
        sads.extend_from_slice(&[0x09, 0x07, 0x07]); // basic audio: 2-channel PCM, 32-48 kHz
    }
    let sad_count = (sads.len() / 3).min(15);
    let mut name = Vec::new();
    for k in 0..4 {
        let d = &e[54 + k * 18..72 + k * 18];
        if d[0] == 0 && d[1] == 0 && d[3] == 0xfc {
            name = d[5..18].iter().take_while(|&&b| b != 0x0a).copied().collect();
        }
    }
    while name.last() == Some(&b' ') {
        name.pop();
    }
    name.truncate(16);
    let mut eld = alloc::vec![0u8; 20 + name.len() + sad_count * 3];
    eld[0] = 2 << 3; // CEA-861-D
    eld[4] = (c[1].min(7) << 5) | name.len() as u8;
    eld[5] = (sad_count as u8) << 4 | 1 << 2; // connection: DisplayPort
    eld[7] = speakers.unwrap_or(1); // front left and right
    eld[16..20].copy_from_slice(&e[8..12]); // maker and product codes
    eld[20..20 + name.len()].copy_from_slice(&name);
    eld[20 + name.len()..].copy_from_slice(&sads[..sad_count * 3]);
    let baseline = (eld.len() - 4).div_ceil(4);
    eld[2] = baseline as u8;
    eld.resize(4 + baseline * 4, 0);
    Some((eld, speakers.is_some()))
}

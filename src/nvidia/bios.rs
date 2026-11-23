//! The card's VBIOS: the tables the display driver needs and an interpreter for the small
//! scripts the card keeps for switching outputs on and training DisplayPort links.
//!
//! Ported from nouveau (MIT licence): nvkm/subdev/bios/bit.c, dcb.c, conn.c, dp.c, disp.c,
//! pll.c and init.c. Only what the display engine uses is here; an unknown script opcode stops
//! the script with an error rather than guessing.

use super::Bar0;
use alloc::format;
use alloc::string::String;
use alloc::vec::Vec;

pub struct Bios<'a> {
    pub b: &'a [u8],
}

/// A display output path from the DCB (nouveau struct dcb_output).
#[derive(Clone, Copy, Debug)]
pub struct DcbOut {
    pub index: u8,
    /// 0 analog, 1 TV, 2 TMDS, 3 LVDS, 6 DisplayPort.
    pub ty: u8,
    /// Output resources it may use (a bit per SOR; the lowest is its pad macro).
    pub or: u8,
    pub location: u8,
    pub connector: u8,
    pub heads: u8,
    /// Its I2C/AUX table entry.
    pub i2c_index: u8,
    /// Sub-links (pad links) it uses: 1 = A, 2 = B, 3 = both.
    pub link: u8,
    /// DisplayPort: the most the board supports (link rate in 0.27 Gbit/s units, lanes).
    pub dp_bw: u8,
    pub dp_nr: u8,
    pub hasht: u16,
    pub hashm: u16,
}

/// DisplayPort output data from the DP table (nouveau struct nvbios_dpout).
#[derive(Clone, Copy, Debug)]
pub struct DpOut {
    pub addr: u32,
    pub flags: u8,
    /// [0] before link training (on Ampere: a table of scripts by link rate), [1] after link
    /// training, [2] spread spectrum on, [3] spread spectrum off, [4] disable the link.
    pub script: [u16; 5],
    /// Scripts by link rate, run before training.
    pub lnkcmp: u16,
}

/// Output script data from the display table (nouveau struct nvbios_outp / nvbios_ocfg).
#[derive(Clone, Copy, Debug)]
pub struct Outp {
    pub addr: u32,
    /// The output-off scripts (for a head that was showing something; unused so far).
    #[allow(dead_code)]
    pub script: [u16; 3],
    sub: u8,
    count: u8,
}

/// PLL limits (nouveau struct nvbios_pll), in kHz.
#[derive(Clone, Copy, Debug)]
pub struct Pll {
    pub refclk: u32,
    #[allow(dead_code)] // logged
    pub vco_min: u32,
    pub vco_max: u32,
    pub in_min: u32,
    pub in_max: u32,
    pub m_min: u32,
    pub m_max: u32,
    pub n_min: u32,
    pub n_max: u32,
    pub p_min: u32,
    pub p_max: u32,
}

impl<'a> Bios<'a> {
    pub fn rd8(&self, o: u32) -> u8 {
        self.b.get(o as usize).copied().unwrap_or(0)
    }
    pub fn rd16(&self, o: u32) -> u16 {
        u16::from_le_bytes([self.rd8(o), self.rd8(o + 1)])
    }
    pub fn rd32(&self, o: u32) -> u32 {
        u32::from_le_bytes([self.rd8(o), self.rd8(o + 1), self.rd8(o + 2), self.rd8(o + 3)])
    }

    /// A BIT directory entry: (version, length, offset) (nouveau bios/bit.c).
    pub fn bit(&self, id: u8) -> Option<(u8, u16, u32)> {
        let at = self.b.windows(5).position(|w| w == b"\xff\xb8BIT")? as u32;
        let (count, size) = (self.rd8(at + 10) as u32, self.rd8(at + 9) as u32);
        (0..count).map(|k| at + 12 + k * size).find(|&e| self.rd8(e) == id).map(|e| (self.rd8(e + 1), self.rd16(e + 2), self.rd16(e + 4) as u32))
    }

    /// The DCB's output entries, unused ones skipped (nouveau dcb_outp_parse).
    pub fn dcb_outputs(&self) -> Vec<DcbOut> {
        let mut out = Vec::new();
        let dcb = self.rd16(0x36) as u32;
        let ver = self.rd8(dcb);
        if dcb == 0 || ver < 0x40 {
            return out;
        }
        let (hdr, cnt, len) = (self.rd8(dcb + 1) as u32, self.rd8(dcb + 2) as u32, self.rd8(dcb + 3) as u32);
        for idx in 0..cnt {
            let e = dcb + hdr + idx * len;
            let conn = self.rd32(e);
            if conn == 0 || conn == 0xffff_ffff {
                break;
            }
            let ty = (conn & 0xf) as u8;
            if ty == 0xe {
                break;
            }
            if ty == 0xf {
                continue;
            }
            let conf = self.rd32(e + 4);
            let mut o = DcbOut {
                index: idx as u8,
                ty,
                or: ((conn >> 24) & 0xf) as u8,
                location: ((conn >> 20) & 3) as u8,
                connector: ((conn >> 12) & 0xf) as u8,
                heads: ((conn >> 8) & 0xf) as u8,
                i2c_index: ((conn >> 4) & 0xf) as u8,
                link: 0,
                dp_bw: 0,
                dp_nr: 0,
                hasht: 0,
                hashm: 0,
            };
            if ty == 6 {
                o.dp_bw = match conf & 0x00e0_0000 {
                    0x0000_0000 => 0x06,
                    0x0020_0000 => 0x0a,
                    0x0040_0000 => 0x14,
                    _ => 0x1e,
                };
                o.dp_nr = match (conf >> 24) & 0xf {
                    0xf | 0x4 => 4,
                    0x3 | 0x2 => 2,
                    _ => 1,
                };
            }
            if matches!(ty, 2 | 3 | 6) {
                o.link = ((conf >> 4) & 3) as u8;
            }
            // extdev is 0 for on-chip outputs (location 0), the only kind used here.
            o.hasht = (o.location as u16) << 4 | ty as u16;
            o.hashm = (o.heads as u16) << 8 | (o.link as u16) << 6 | o.or as u16;
            out.push(o);
        }
        out
    }

    /// The DisplayPort AUX channel of an I2C/AUX table entry (nouveau bios/i2c.c, version 4.1:
    /// bits 5-9; 0x1f = none).
    pub fn aux_channel(&self, i2c_index: u8) -> Option<u8> {
        let dcb = self.rd16(0x36) as u32;
        let it = self.rd16(dcb + 4) as u32;
        if dcb == 0 || it == 0 || self.rd8(it) < 0x41 {
            return None;
        }
        let (ih, ic, il) = (self.rd8(it + 1) as u32, self.rd8(it + 2) as u32, self.rd8(it + 3) as u32);
        if i2c_index as u32 >= ic {
            return None;
        }
        let ch = ((self.rd16(it + ih + i2c_index as u32 * il) >> 5) & 0x1f) as u8;
        (ch != 0x1f).then_some(ch)
    }

    /// The connector table's type for connector `idx` (nouveau nvbios_connEp): 0x46
    /// DisplayPort, 0x47 eDP, 0x61 HDMI...
    pub fn conn_type(&self, idx: u8) -> u8 {
        let dcb = self.rd16(0x36) as u32;
        if dcb == 0 || self.rd8(dcb + 1) < 0x16 {
            return 0xff;
        }
        let c = self.rd16(dcb + 0x14) as u32;
        if c == 0 || !matches!(self.rd8(c), 0x30 | 0x40) {
            return 0xff;
        }
        let (hdr, cnt, len) = (self.rd8(c + 1) as u32, self.rd8(c + 2) as u32, self.rd8(c + 3) as u32);
        if idx as u32 >= cnt {
            return 0xff;
        }
        self.rd8(c + hdr + idx as u32 * len)
    }

    /// The DP table: (address, version, header, count, entry length) (nouveau nvbios_dp_table).
    fn dp_table(&self) -> Option<(u32, u8, u32, u32, u32)> {
        let (v, l, off) = self.bit(b'd')?;
        if v != 1 || l < 2 {
            return None;
        }
        let data = self.rd16(off) as u32;
        let ver = self.rd8(data);
        if data == 0 || !matches!(ver, 0x40..=0x42) {
            return None;
        }
        Some((data, ver, self.rd8(data + 1) as u32, self.rd8(data + 3) as u32, self.rd8(data + 2) as u32))
    }

    /// The DP table entry for an output (nouveau nvbios_dpout_match, versions 4.x).
    pub fn dpout_match(&self, hasht: u16, hashm: u16) -> Option<DpOut> {
        let (dp, _ver, hdr, cnt, len) = self.dp_table()?;
        for idx in 0..cnt {
            let o = self.rd16(dp + hdr + idx * len) as u32;
            if o == 0 {
                continue;
            }
            let (ty, mask) = (self.rd16(o), self.rd16(o + 2));
            if ty == hasht && mask & hashm == hashm {
                return Some(DpOut {
                    addr: o,
                    flags: self.rd8(o + 4),
                    script: [self.rd16(o + 5), self.rd16(o + 7), self.rd16(o + 0x0b), self.rd16(o + 0x0d), self.rd16(o + 0x0f)],
                    lnkcmp: self.rd16(o + 9),
                });
            }
        }
        None
    }

    /// The DisplayPort table's version (0x40-0x42).
    pub fn dp_version(&self) -> u8 {
        self.dp_table().map(|t| t.1).unwrap_or(0)
    }

    /// Whether the DP table says the board can do multi-stream (nouveau nvkm_dp_new).
    #[allow(dead_code)]
    pub fn dp_mst(&self) -> bool {
        self.dp_table().map(|(d, v, ..)| v >= 0x40 && self.rd8(d + 8) & 4 != 0).unwrap_or(false)
    }

    /// Drive settings for a lane: (post-cursor2, drive current, pre-emphasis, TX power-up)
    /// for the requested post-cursor2, voltage swing and pre-emphasis levels (nouveau
    /// nvbios_dpcfg_match, versions 4.x).
    pub fn dpcfg_match(&self, dpout: &DpOut, pc: u8, vs: u8, pe: u8) -> Option<(u8, u8, u8, u8)> {
        let (dp, ver, hdr, cnt, len) = self.dp_table()?;
        const VSOFF: [u32; 4] = [0, 4, 7, 9];
        let mut idx = pc as u32 * 10 + VSOFF[(vs & 3) as usize] + pe as u32;
        let out_hdr = self.rd8(dp + 4) as u32;
        if (0x40..=0x41).contains(&ver) && out_hdr >= 0x12 {
            idx += self.rd8(dpout.addr + 0x11) as u32 * 40;
        } else if ver >= 0x42 {
            idx += self.rd8(dpout.addr + 0x11) as u32 * 10;
        }
        let cfg_hdr = hdr + len * cnt;
        let cfg_len = self.rd8(dp + 6) as u32;
        let cfg_cnt = self.rd8(dp + 7) as u32 * self.rd8(dp + 5) as u32;
        if idx >= cfg_cnt {
            return None;
        }
        let d = dp + cfg_hdr + idx * cfg_len;
        Some(if ver >= 0x42 {
            (0, self.rd8(d), self.rd8(d + 1), self.rd8(d + 2))
        } else {
            (self.rd8(d), self.rd8(d + 1), self.rd8(d + 2), self.rd8(d + 3))
        })
    }

    /// The display table entry for an output (nouveau nvbios_outp_match).
    pub fn outp_match(&self, ty: u16, mask: u16) -> Option<Outp> {
        let (v, _, off) = self.bit(b'U')?;
        if v != 1 {
            return None;
        }
        let dt = self.rd16(off) as u32;
        let ver = self.rd8(dt);
        if dt == 0 || !matches!(ver, 0x20..=0x22) {
            return None;
        }
        let (hdr, len, cnt, sub) = (self.rd8(dt + 1) as u32, self.rd8(dt + 2) as u32, self.rd8(dt + 3) as u32, self.rd8(dt + 4));
        if len < 2 || sub < 0x0a {
            return None;
        }
        for idx in 0..cnt {
            let data = self.rd16(dt + hdr + idx * len) as u32;
            if data == 0 {
                continue;
            }
            let t = self.rd16(data);
            let mut m = self.rd32(data + 2);
            if ver <= 0x20 {
                m |= 0xc0;
            }
            if t == ty && m & mask as u32 == mask as u32 {
                return Some(Outp {
                    addr: data,
                    script: [self.rd16(data + 6), self.rd16(data + 8), if sub >= 0x0c { self.rd16(data + 0x0a) } else { 0 }],
                    sub,
                    count: self.rd8(data + 5),
                });
            }
        }
        None
    }

    /// The output's clock-compare tables for a protocol and flags (nouveau nvbios_ocfg_match).
    pub fn ocfg_match(&self, o: &Outp, proto: u8, flags: u8) -> Option<[u16; 2]> {
        (0..o.count as u32).map(|k| o.addr + o.sub as u32 + k * 6).find_map(|d| {
            let (p, f) = (self.rd8(d), self.rd8(d + 1));
            ((p == proto || p == 0xff) && f == flags).then(|| [self.rd16(d + 2), self.rd16(d + 4)])
        })
    }

    /// The script for a pixel clock from a clock-compare table (nouveau nvbios_oclk_match).
    pub fn oclk_match(&self, mut cmp: u16, khz: u32) -> u16 {
        for _ in 0..64 {
            if cmp == 0 {
                break;
            }
            if khz / 10 >= self.rd16(cmp as u32) as u32 {
                return self.rd16(cmp as u32 + 2);
            }
            cmp += 4;
        }
        0
    }

    /// PLL limits for a PLL type (0x80 + head for the pixel clocks), from the table BIT 'C'
    /// points at (nouveau nvbios_pll_parse; table version 0x50, Volta and newer).
    pub fn pll(&self, ty: u8) -> Option<Pll> {
        let (v, l, off) = self.bit(b'C')?;
        let data = match v {
            1 if l >= 10 => self.rd16(off + 8) as u32,
            2 if l >= 4 => self.rd32(off),
            _ => 0,
        };
        if data == 0 || self.rd8(data) != 0x50 {
            return None;
        }
        let (hdr, len, cnt) = (self.rd8(data + 1) as u32, self.rd8(data + 2) as u32, self.rd8(data + 3) as u32);
        let d = (0..cnt).map(|k| data + hdr + k * len).find(|&d| self.rd8(d) == ty)?;
        Some(Pll {
            refclk: self.rd16(d + 1) as u32 * 1000,
            vco_min: self.rd16(d + 5) as u32 * 1000,
            vco_max: self.rd16(d + 7) as u32 * 1000,
            in_min: self.rd16(d + 9) as u32 * 1000,
            in_max: self.rd16(d + 11) as u32 * 1000,
            m_min: self.rd8(d + 13) as u32,
            m_max: self.rd8(d + 14) as u32,
            n_min: self.rd8(d + 15) as u32,
            n_max: self.rd8(d + 16) as u32,
            p_min: self.rd8(d + 17) as u32,
            p_max: self.rd8(d + 18) as u32,
        })
    }

    /// The init tables' pointers (BIT 'I'): 0 scripts, 2 macro index, 4 macros, 6 conditions,
    /// 8 I/O conditions, 0x10 translation tables (nouveau init_table_).
    fn init_table(&self, offset: u32) -> u32 {
        match self.bit(b'I') {
            Some((_, len, off)) if len as u32 >= offset + 2 => self.rd16(off + offset) as u32,
            _ => 0,
        }
    }
}

/// PLL coefficients for a frequency in kHz: (N, fN, M, P) (nouveau gt215_pll_calc, with the
/// fractional part).
pub fn pll_calc(info: &Pll, freq: u32) -> Option<(u32, u32, u32, u32)> {
    let mut p = info.vco_max / freq.max(1);
    if p > info.p_max {
        p = info.p_max;
    }
    if p < info.p_min {
        p = info.p_min;
    }
    if p == 0 {
        p = 1;
    }
    let refclk = info.refclk.max(1);
    let lm = ((refclk + info.in_max) / info.in_max.max(1)).max(info.m_min);
    let hm = ((refclk + info.in_min) / info.in_min.max(1)).min(info.m_max);
    let lm = lm.min(hm);
    for m in lm..=hm {
        let tmp = freq as u64 * p as u64 * m as u64;
        let mut n = (tmp / refclk as u64) as u32;
        let rem = (tmp % refclk as u64) as u32;
        if rem < refclk / 2 {
            n = n.wrapping_sub(1);
        }
        let fnum = tmp as i64 - n as i64 * refclk as i64;
        if n < info.n_min {
            continue;
        }
        if n > info.n_max {
            break;
        }
        let fn_ = ((((fnum as u64) << 13) + refclk as u64 / 2) / refclk as u64) as u32;
        return Some((n, fn_.wrapping_sub(4096) & 0xffff, m, p));
    }
    None
}

/// Running one script: the output it's for and what it may touch (nouveau struct nvbios_init).
pub struct Init<'a, 'b> {
    pub r: &'a Bar0,
    pub bios: &'a Bios<'b>,
    pub outp: Option<DcbOut>,
    /// The output's AUX channel and connector type (for AUX opcodes and conditions).
    pub aux: Option<u8>,
    pub conn_type: u8,
    pub or: i32,
    pub link: i32,
    pub head: i32,
    /// Log every register and AUX write.
    pub trace: bool,
    execute: u8,
    nested: u32,
    steps: u32,
    pub writes: u32,
}

impl<'a, 'b> Init<'a, 'b> {
    pub fn new(r: &'a Bar0, bios: &'a Bios<'b>, outp: Option<DcbOut>, aux: Option<u8>, or: i32, link: i32, head: i32) -> Self {
        let conn_type = outp.map(|o| bios.conn_type(o.connector)).unwrap_or(0xff);
        Init { r, bios, outp, aux, conn_type, or, link, head, trace: true, execute: 1, nested: 0, steps: 0, writes: 0 }
    }

    /// Run the script at `addr` (nouveau nvbios_init / nvbios_exec).
    pub fn run(&mut self, addr: u16, what: &str) -> Result<(), String> {
        if addr == 0 {
            return Ok(());
        }
        kprintln!("nvidia:     script {:#06x} ({}), output {}, link {}, head {}", addr, what, self.or, self.link, self.head);
        self.execute = 1;
        self.nested = 0;
        self.steps = 0;
        let r = self.exec(addr as u32);
        if let Err(e) = &r {
            kprintln!("nvidia:     script {:#06x} stopped: {}", addr, e);
        }
        r
    }

    fn exec_on(&self) -> bool {
        self.execute == 1 || self.execute & 5 == 5
    }
    fn exec_set(&mut self, on: bool) {
        if on {
            self.execute &= 0xfd;
        } else {
            self.execute |= 0x02;
        }
    }

    /// A script's register address, with the display bits applied (nouveau init_nvreg):
    /// 0x80000000 adds the head's offset, 0x40000000 the output's, 0x20000000 its sub-link's.
    fn nvreg(&self, mut reg: u32) -> Result<u32, String> {
        reg &= !3;
        if reg & 0x8000_0000 != 0 {
            if self.exec_on() && self.head < 0 {
                return Err(String::from("script needs a head"));
            }
            reg = (reg & !0x8000_0000).wrapping_add(self.head.max(0) as u32 * 0x800);
        }
        if reg & 0x4000_0000 != 0 {
            if self.exec_on() && self.or < 0 {
                return Err(String::from("script needs an output"));
            }
            reg = (reg & !0x4000_0000).wrapping_add(self.or.max(0) as u32 * 0x800);
            if reg & 0x2000_0000 != 0 {
                if self.exec_on() && self.link == 0 {
                    return Err(String::from("script needs a sub-link"));
                }
                reg = (reg & !0x2000_0000).wrapping_add((self.link == 2) as u32 * 0x80);
            }
        }
        if reg & !0x00ff_fffc != 0 {
            return Err(format!("register {:#010x} out of range", reg));
        }
        Ok(reg)
    }

    fn rd32(&self, reg: u32) -> Result<u32, String> {
        let reg = self.nvreg(reg)?;
        Ok(if self.exec_on() { self.r.rd(reg) } else { 0 })
    }
    fn wr32(&mut self, reg: u32, val: u32) -> Result<(), String> {
        let reg = self.nvreg(reg)?;
        if self.exec_on() {
            if self.trace {
                kprintln!("nvidia:       R[{:#08x}] = {:#010x} (was {:#010x})", reg, val, self.r.rd(reg));
            }
            self.r.wr(reg, val);
            self.writes += 1;
        }
        Ok(())
    }
    fn mask(&mut self, reg: u32, mask: u32, val: u32) -> Result<u32, String> {
        let reg = self.nvreg(reg)?;
        if self.exec_on() {
            let tmp = self.r.rd(reg);
            let new = (tmp & !mask) | val;
            if self.trace {
                kprintln!("nvidia:       R[{:#08x}] = {:#010x} (was {:#010x})", reg, new, tmp);
            }
            self.r.wr(reg, new);
            self.writes += 1;
            return Ok(tmp);
        }
        Ok(0)
    }

    /// VGA CRTC registers (port 0x3d4/0x3d5), mirrored at BAR0 0x601000 (nouveau nvkm_rdvgai).
    fn rdvgai(&self, port: u16, index: u8) -> u8 {
        if !self.exec_on() {
            return 0;
        }
        self.r.wr8(0x601000 + port as u32, index);
        self.r.rd8(0x601000 + port as u32 + 1)
    }
    fn wrvgai(&mut self, port: u16, index: u8, value: u8) {
        if self.exec_on() {
            if self.trace {
                kprintln!("nvidia:       VGA {:#06x}[{:#04x}] = {:#04x}", port, index, value);
            }
            self.r.wr8(0x601000 + port as u32, index);
            self.r.wr8(0x601000 + port as u32 + 1, value);
            self.writes += 1;
        }
    }

    fn rdauxr(&self, addr: u32) -> u8 {
        match (self.aux, self.exec_on()) {
            (Some(ch), true) => super::dpcd(self.r, ch, addr, 1).ok().and_then(|v| v.first().copied()).unwrap_or(0),
            _ => 0,
        }
    }
    fn wrauxr(&mut self, addr: u32, data: u8) {
        if let (Some(ch), true) = (self.aux, self.exec_on()) {
            if self.trace {
                kprintln!("nvidia:       DPCD[{:#x}] = {:#04x}", addr, data);
            }
            if let Err(e) = super::dpcd_write(self.r, ch, addr, &[data]) {
                kprintln!("nvidia:       DPCD write failed: {}", e);
            }
            self.writes += 1;
        }
    }

    fn condition_met(&self, cond: u8) -> Result<bool, String> {
        let table = self.bios.init_table(6);
        if table == 0 {
            return Ok(false);
        }
        let e = table + cond as u32 * 12;
        let (reg, msk, val) = (self.bios.rd32(e), self.bios.rd32(e + 4), self.bios.rd32(e + 8));
        Ok(self.rd32(reg)? & msk == val)
    }

    fn script_table(&self, index: u8) -> u32 {
        let t = self.bios.init_table(0);
        if t == 0 {
            0
        } else {
            self.bios.rd16(t + index as u32 * 2) as u32
        }
    }

    /// nouveau nvbios_exec: run opcodes until INIT_DONE.
    fn exec(&mut self, mut off: u32) -> Result<(), String> {
        let b = self.bios;
        self.nested += 1;
        if self.nested > 8 {
            return Err(String::from("scripts nested too deeply"));
        }
        while off != 0 {
            self.steps += 1;
            if self.steps > 20_000 {
                return Err(String::from("script runs too long"));
            }
            let op = b.rd8(off);
            match op {
                0x71 => off = 0, // DONE
                0x72 => {
                    // RESUME
                    off += 1;
                    self.exec_set(true);
                }
                0x38 => {
                    // NOT
                    off += 1;
                    self.execute ^= 0x02;
                }
                0x3a => {
                    // GENERIC_CONDITION
                    let (cond, size) = (b.rd8(off + 1), b.rd8(off + 2));
                    off += 3;
                    match cond {
                        0 => {
                            // CONDITION_ID_INT_DP: an eDP panel
                            if self.exec_on() && self.conn_type != 0x47 {
                                self.exec_set(false);
                            }
                        }
                        1 | 2 => {
                            // CONDITION_ID_USE_SPPLL0/1
                            let found = self.outp.and_then(|o| b.dpout_match(6, o.or as u16 | (o.link as u16) << 6));
                            match found {
                                Some(info) => {
                                    if info.flags & cond == 0 {
                                        self.exec_set(false);
                                    }
                                }
                                None => {
                                    if self.exec_on() {
                                        kprintln!("nvidia:       script needs DisplayPort table data");
                                    }
                                }
                            }
                        }
                        5 => {
                            // CONDITION_ID_ASSR_SUPPORT
                            if self.rdauxr(0x0d) & 1 == 0 {
                                self.exec_set(false);
                            }
                        }
                        7 => self.exec_set(false), // CONDITION_ID_NO_PANEL_SEQ_DELAYS
                        _ => {
                            kprintln!("nvidia:       GENERIC_CONDITION {:#x} unknown, skipped", cond);
                            off += size as u32;
                        }
                    }
                }
                0x47 => {
                    // ANDN_REG
                    let (reg, mask) = (b.rd32(off + 1), b.rd32(off + 5));
                    off += 9;
                    self.mask(reg, mask, 0)?;
                }
                0x48 => {
                    // OR_REG
                    let (reg, mask) = (b.rd32(off + 1), b.rd32(off + 5));
                    off += 9;
                    self.mask(reg, 0, mask)?;
                }
                0x52 => {
                    // CR
                    let (addr, mask, data) = (b.rd8(off + 1), b.rd8(off + 2), b.rd8(off + 3));
                    off += 4;
                    let val = self.rdvgai(0x3d4, addr) & mask;
                    self.wrvgai(0x3d4, addr, val | data);
                }
                0x53 => {
                    // ZM_CR
                    let (addr, data) = (b.rd8(off + 1), b.rd8(off + 2));
                    off += 3;
                    self.wrvgai(0x3d4, addr, data);
                }
                0x54 => {
                    // ZM_CR_GROUP
                    let count = b.rd8(off + 1) as u32;
                    off += 2;
                    for _ in 0..count {
                        let (addr, data) = (b.rd8(off), b.rd8(off + 1));
                        off += 2;
                        self.wrvgai(0x3d4, addr, data);
                    }
                }
                0x56 => {
                    // CONDITION_TIME: wait (20 ms steps) for a condition
                    let (cond, retry) = (b.rd8(off + 1), b.rd8(off + 2) as u32);
                    off += 3;
                    if self.exec_on() {
                        let mut wait = (retry * 50).min(100);
                        let mut met = false;
                        while wait > 0 {
                            if self.condition_met(cond)? {
                                met = true;
                                break;
                            }
                            super::mdelay(20);
                            wait -= 1;
                        }
                        if !met {
                            kprintln!("nvidia:       condition {} never met", cond);
                            self.exec_set(false);
                        }
                    }
                }
                0x57 => {
                    // LTIME
                    let ms = b.rd16(off + 1) as u64;
                    off += 3;
                    if self.exec_on() {
                        super::mdelay(ms);
                    }
                }
                0x58 => {
                    // ZM_REG_SEQUENCE
                    let (mut base, count) = (b.rd32(off + 1), b.rd8(off + 5) as u32);
                    off += 6;
                    for _ in 0..count {
                        let data = b.rd32(off);
                        off += 4;
                        self.wr32(base, data)?;
                        base = base.wrapping_add(4);
                    }
                }
                0x5b => {
                    // SUB_DIRECT
                    let addr = b.rd16(off + 1) as u32;
                    if self.exec_on() {
                        self.exec(addr)?;
                    }
                    off += 3;
                }
                0x5c => {
                    // JUMP
                    let addr = b.rd16(off + 1) as u32;
                    off = if self.exec_on() { addr } else { off + 3 };
                }
                0x5f => {
                    // COPY_NV_REG
                    let (sreg, shift, smask, sxor) = (b.rd32(off + 1), b.rd8(off + 5), b.rd32(off + 6), b.rd32(off + 10));
                    let (dreg, dmask) = (b.rd32(off + 14), b.rd32(off + 18));
                    off += 22;
                    let v = self.rd32(sreg)?;
                    let data = if shift < 0x80 { v >> shift } else { v << (0x100 - shift as u32) };
                    self.mask(dreg, !dmask, (data & smask) ^ sxor)?;
                }
                0x6b => {
                    // SUB
                    let index = b.rd8(off + 1);
                    let addr = self.script_table(index);
                    if addr != 0 && self.exec_on() {
                        self.exec(addr)?;
                    }
                    off += 2;
                }
                0x6e => {
                    // NV_REG: R = (R & mask) | data
                    let (reg, mask, data) = (b.rd32(off + 1), b.rd32(off + 5), b.rd32(off + 9));
                    off += 13;
                    self.mask(reg, !mask, data)?;
                }
                0x6f => {
                    // MACRO
                    let m = b.rd8(off + 1) as u32;
                    let table = b.init_table(4);
                    if table != 0 {
                        let (addr, data) = (b.rd32(table + m * 8), b.rd32(table + m * 8 + 4));
                        self.wr32(addr, data)?;
                    }
                    off += 2;
                }
                0x73 => {
                    // STRAP_CONDITION
                    let (mask, value) = (b.rd32(off + 1), b.rd32(off + 5));
                    off += 9;
                    if self.rd32(0x101000)? & mask != value {
                        self.exec_set(false);
                    }
                }
                0x74 => {
                    // TIME (microseconds)
                    let us = b.rd16(off + 1) as u64;
                    off += 3;
                    if self.exec_on() {
                        if us < 1000 {
                            super::udelay(us);
                        } else {
                            super::mdelay((us + 900) / 1000);
                        }
                    }
                }
                0x75 => {
                    // CONDITION
                    let cond = b.rd8(off + 1);
                    off += 2;
                    if !self.condition_met(cond)? {
                        self.exec_set(false);
                    }
                }
                0x77 => {
                    // ZM_REG16
                    let (addr, data) = (b.rd32(off + 1), b.rd16(off + 5) as u32);
                    off += 7;
                    self.wr32(addr, data)?;
                }
                0x7a => {
                    // ZM_REG
                    let (addr, mut data) = (b.rd32(off + 1), b.rd32(off + 5));
                    off += 9;
                    if addr == 0x200 {
                        data |= 1;
                    }
                    self.wr32(addr, data)?;
                }
                0x90 => {
                    // COPY_ZM_REG
                    let (sreg, dreg) = (b.rd32(off + 1), b.rd32(off + 5));
                    off += 9;
                    let v = self.rd32(sreg)?;
                    self.wr32(dreg, v)?;
                }
                0x91 => {
                    // ZM_REG_GROUP
                    let (addr, count) = (b.rd32(off + 1), b.rd8(off + 5) as u32);
                    off += 6;
                    for _ in 0..count {
                        let data = b.rd32(off);
                        off += 4;
                        self.wr32(addr, data)?;
                    }
                }
                0x96 => {
                    // XLAT
                    let (saddr, sshift, smask, index) = (b.rd32(off + 1), b.rd8(off + 5), b.rd8(off + 6), b.rd8(off + 7));
                    let (daddr, dmask, shift) = (b.rd32(off + 8), b.rd32(off + 12), b.rd8(off + 16));
                    off += 17;
                    let v = self.rd32(saddr)?;
                    let data = (if sshift < 0x80 { v >> sshift } else { v << (0x100 - sshift as u32) }) & smask as u32;
                    let table = b.init_table(0x10);
                    let x = if table != 0 {
                        let d = b.rd16(table + index as u32 * 2) as u32;
                        if d != 0 {
                            b.rd8(d + data) as u32
                        } else {
                            0
                        }
                    } else {
                        0
                    };
                    self.mask(daddr, !dmask, x << shift)?;
                }
                0x97 => {
                    // ZM_MASK_ADD
                    let (addr, mask, add) = (b.rd32(off + 1), b.rd32(off + 5), b.rd32(off + 9));
                    off += 13;
                    let v = self.rd32(addr)?;
                    self.wr32(addr, (v & mask) | (v.wrapping_add(add) & !mask))?;
                }
                0x98 => {
                    // AUXCH: DPCD &= mask |= data
                    let (addr, count) = (b.rd32(off + 1), b.rd8(off + 5) as u32);
                    off += 6;
                    for _ in 0..count {
                        let (mask, data) = (b.rd8(off), b.rd8(off + 1));
                        off += 2;
                        let v = self.rdauxr(addr) & mask;
                        self.wrauxr(addr, v | data);
                    }
                }
                0x99 => {
                    // ZM_AUXCH
                    let (addr, count) = (b.rd32(off + 1), b.rd8(off + 5) as u32);
                    off += 6;
                    for _ in 0..count {
                        let data = b.rd8(off);
                        off += 1;
                        self.wrauxr(addr, data);
                    }
                }
                _ => return Err(format!("unknown opcode {:#04x} at {:#06x}", op, off)),
            }
        }
        self.nested -= 1;
        Ok(())
    }
}

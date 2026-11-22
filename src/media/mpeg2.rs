//! MPEG-1 and MPEG-2 pictures (DVDs, TV recordings, older camcorders and computer videos).
//! Follows ISO/IEC 13818-2 (and 11172-2), checked against FFmpeg. The stream may arrive in any
//! pieces; pictures are found by their start codes, timed from the packets' time stamps, and
//! given out in display order. 4:2:0 only (what DVDs, TV and consumer cameras use).

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use super::dec::{Pic as OutPic, VideoDec};
use super::idct;
use super::mc::{half, Plane};
use super::mpeg12tab::*;
use super::vlc::{from_lists, from_pairs, Vlc};
use crate::video::bits::Bits;
use crate::video::h264d::Frame;

// Macroblock type flags.
const INTRA: i32 = 1;
const PAT: i32 = 2;
const MB: i32 = 4; // backward motion
const MF: i32 = 8; // forward motion
const QUANT: i32 = 16;

const I_TYPE: u8 = 1;
const P_TYPE: u8 = 2;
const B_TYPE: u8 = 3;

const TOP: u8 = 1;
const BOTTOM: u8 = 2;
const FRAME: u8 = 3;

#[derive(Clone, Copy, PartialEq)]
enum Motion {
    Frame,
    Field,
    F16x8,
    Dual,
}

struct Tables {
    addr: Vlc,
    mb_i: Vlc,
    mb_p: Vlc,
    mb_b: Vlc,
    pat: Vlc,
    mv: Vlc,
    dc_lum: Vlc,
    dc_chroma: Vlc,
    b14: Vlc,
    b15: Vlc,
}

impl Tables {
    fn new() -> Box<Tables> {
        let codes = |list: &[(&str, i32)]| -> Vlc {
            let v: Vec<(u32, u8, i32)> = list.iter().map(|(s, f)| (u32::from_str_radix(s, 2).unwrap_or(0), s.len() as u8, *f)).collect();
            Vlc::new(&v)
        };
        Box::new(Tables {
            addr: Vlc::new(&from_pairs(&MB_ADDR_INCR)),
            mb_i: codes(&[("1", INTRA), ("01", INTRA | QUANT)]),
            mb_p: codes(&[
                ("1", MF | PAT),
                ("01", PAT),
                ("001", MF),
                ("00011", INTRA),
                ("00010", MF | PAT | QUANT),
                ("00001", PAT | QUANT),
                ("000001", INTRA | QUANT),
            ]),
            mb_b: codes(&[
                ("10", MF | MB),
                ("11", MF | MB | PAT),
                ("010", MB),
                ("011", MB | PAT),
                ("0010", MF),
                ("0011", MF | PAT),
                ("00011", INTRA),
                ("00010", MF | MB | PAT | QUANT),
                ("000011", MF | PAT | QUANT),
                ("000010", MB | PAT | QUANT),
                ("000001", INTRA | QUANT),
            ]),
            pat: Vlc::new(&from_pairs(&MB_PAT)),
            mv: Vlc::new(&from_pairs(&MV_TAB)),
            dc_lum: Vlc::new(&from_lists(&DC_LUM_CODE, &DC_LUM_BITS)),
            dc_chroma: Vlc::new(&from_lists(&DC_CHROMA_CODE, &DC_CHROMA_BITS)),
            b14: Vlc::new(&from_pairs(&MPEG1_VLC)),
            b15: Vlc::new(&from_pairs(&MPEG2_VLC)),
        })
    }
}

#[derive(Clone)]
struct Seq {
    w: usize,
    h: usize,
    mbw: usize,
    mbh: usize,
    mpeg1: bool,
    progressive: bool,
    period_us: i64,
    intra_q: [u8; 64],
    inter_q: [u8; 64],
    bt709: bool,
    chroma420: bool,
}

#[derive(Clone)]
struct Hdr {
    kind: u8,
    full_pel: [bool; 2],
    f_code: [[u32; 2]; 2],
    dc_prec: u32,
    structure: u8,
    tff: bool,
    fpfd: bool,
    concealment: bool,
    q_scale_type: bool,
    intra_vlc: bool,
    alt_scan: bool,
    rff: bool,
    pts: Option<i64>,
}

/// A decoded (or being decoded) frame; planes are macroblock-aligned.
#[derive(Clone)]
struct Pic {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
    pts: Option<i64>,
    dur: i64,
    reference: bool,
}

struct Cur {
    pic: Pic,
    first: u8,
    fields: u8,
}

/// The motion of the last macroblock (skipped B macroblocks repeat it).
#[derive(Clone, Copy)]
struct MbMotion {
    flags: i32,
    kind: Motion,
    mv: [[[i32; 2]; 4]; 2],
    fs: [[usize; 2]; 2],
}

pub struct Mpeg2Dec {
    t: Box<Tables>,
    config: Vec<u8>,
    buf: Vec<u8>,
    base: u64,
    scanned: usize,
    marks: VecDeque<(u64, i64)>,
    seq: Option<Seq>,
    hdr: Option<Hdr>,
    in_slices: bool,
    closed_gop: bool,
    cur: Option<Cur>,
    snap: Option<Pic>,
    skip_pic: bool,
    prev_ref: Option<Pic>,
    last_ref: Option<Pic>,
    last_emitted: bool,
    out: VecDeque<(i64, Frame)>,
    last_out: Option<(i64, i64)>,
    // Slice state.
    qscale: i32,
    dc_pred: [i32; 3],
    pmv: [[[i32; 2]; 2]; 2],
    last: MbMotion,
}

impl Mpeg2Dec {
    pub fn new(config: &[u8]) -> Mpeg2Dec {
        let mut d = Mpeg2Dec {
            t: Tables::new(),
            config: config.to_vec(),
            buf: Vec::new(),
            base: 0,
            scanned: 0,
            marks: VecDeque::new(),
            seq: None,
            hdr: None,
            in_slices: false,
            closed_gop: false,
            cur: None,
            snap: None,
            skip_pic: false,
            prev_ref: None,
            last_ref: None,
            last_emitted: true,
            out: VecDeque::new(),
            last_out: None,
            qscale: 0,
            dc_pred: [0; 3],
            pmv: [[[0; 2]; 2]; 2],
            last: MbMotion { flags: 0, kind: Motion::Frame, mv: [[[0; 2]; 4]; 2], fs: [[0; 2]; 2] },
        };
        if !d.config.is_empty() {
            let c = d.config.clone();
            d.push(&c, None);
            d.base = 0;
        }
        d
    }

    /// Take in stream bytes; whole units (start code to start code) are decoded.
    fn push(&mut self, data: &[u8], pts: Option<i64>) {
        if let Some(t) = pts {
            self.marks.push_back((self.base + self.buf.len() as u64, t));
            if self.marks.len() > 64 {
                self.marks.pop_front();
            }
        }
        self.buf.extend_from_slice(data);
        self.units(false);
    }

    fn units(&mut self, end: bool) {
        let mut starts = Vec::new();
        let mut i = self.scanned;
        while i + 3 < self.buf.len() {
            if self.buf[i] == 0 && self.buf[i + 1] == 0 && self.buf[i + 2] == 1 {
                starts.push(i);
                i += 3;
            } else {
                i += 1;
            }
        }
        // Units already found but not finished begin at buf[0] (when it is a start code).
        let mut all = Vec::new();
        if self.buf.len() >= 3 && self.buf[0] == 0 && self.buf[1] == 0 && self.buf[2] == 1 && starts.first() != Some(&0) {
            all.push(0);
        }
        all.extend(starts);
        self.scanned = self.buf.len().saturating_sub(3).max(self.scanned);
        if all.is_empty() {
            if self.buf.len() > (1 << 20) {
                self.drop_front(self.buf.len() - 3);
            }
            return;
        }
        let mut done = all[0];
        for k in 0..all.len() {
            let s = all[k];
            let e = if k + 1 < all.len() { all[k + 1] } else if end { self.buf.len() } else { break };
            let unit = self.buf[s + 3..e].to_vec();
            let abs = self.base + s as u64;
            self.unit(&unit, abs);
            done = e;
        }
        self.drop_front(done);
    }

    fn drop_front(&mut self, n: usize) {
        self.buf.drain(..n);
        self.base += n as u64;
        self.scanned = self.scanned.saturating_sub(n);
    }

    fn unit(&mut self, u: &[u8], abs: u64) {
        let Some(&code) = u.first() else { return };
        let body = &u[1..];
        match code {
            0x00 | 0xB3 | 0xB8 | 0xB7 => {
                if self.in_slices {
                    self.end_picture();
                }
                match code {
                    0x00 => self.picture_header(body, abs),
                    0xB3 => self.sequence_header(body),
                    0xB8 => self.closed_gop = body.len() > 3 && body[3] & 0x40 != 0,
                    _ => {}
                }
            }
            0xB5 => self.extension(body),
            0x01..=0xAF => {
                if self.hdr.is_some() && self.seq.is_some() {
                    if !self.in_slices {
                        self.begin_picture();
                    }
                    if !self.skip_pic {
                        self.slice(body, code as usize);
                    }
                }
            }
            _ => {}
        }
    }

    fn sequence_header(&mut self, d: &[u8]) {
        let mut b = Bits::new(d);
        let w = b.read(12) as usize;
        let h = b.read(12) as usize;
        b.skip(4); // aspect ratio
        let rate = b.read(4) as usize;
        b.skip(18 + 1 + 10 + 1); // bit rate, marker, buffer size, constrained
        let mut intra_q = DEFAULT_INTRA;
        let mut inter_q = [16u8; 64];
        if b.flag() {
            for &z in ZIGZAG.iter() {
                intra_q[z as usize] = b.read(8) as u8;
            }
        }
        if b.flag() {
            for &z in ZIGZAG.iter() {
                inter_q[z as usize] = b.read(8) as u8;
            }
        }
        if w == 0 || h == 0 {
            return;
        }
        let (num, den) = match rate {
            1 => (24000, 1001),
            2 => (24, 1),
            3 => (25, 1),
            4 => (30000, 1001),
            5 => (30, 1),
            6 => (50, 1),
            7 => (60000, 1001),
            8 => (60, 1),
            _ => (25, 1),
        };
        let keep709 = self.seq.as_ref().is_some_and(|s| s.bt709 && s.w == w);
        let mbw = w.div_ceil(16);
        self.seq = Some(Seq {
            w,
            h,
            mbw,
            mbh: h.div_ceil(16),
            mpeg1: true, // until a sequence extension says otherwise
            progressive: true,
            period_us: 1_000_000 * den / num,
            intra_q,
            inter_q,
            bt709: keep709 || h > 576,
            chroma420: true,
        });
    }

    fn extension(&mut self, d: &[u8]) {
        let mut b = Bits::new(d);
        let id = b.read(4);
        match id {
            1 => {
                // Sequence extension: MPEG-2.
                let Some(s) = self.seq.as_mut() else { return };
                b.skip(8); // profile and level
                s.progressive = b.flag();
                let chroma = b.read(2);
                s.chroma420 = chroma == 1;
                let hx = b.read(2) as usize;
                let vx = b.read(2) as usize;
                s.w |= hx << 12;
                s.h |= vx << 12;
                b.skip(12 + 1 + 8 + 1); // bit rate ext, marker, buffer ext, low delay
                let n = b.read(2) as i64 + 1;
                let dd = b.read(5) as i64 + 1;
                s.period_us = s.period_us * dd / n;
                s.mpeg1 = false;
                s.mbw = s.w.div_ceil(16);
                s.mbh = if s.progressive { s.h.div_ceil(16) } else { 2 * s.h.div_ceil(32) };
            }
            2 => {
                // Sequence display extension: colour description.
                let Some(s) = self.seq.as_mut() else { return };
                b.skip(3);
                if b.flag() {
                    b.skip(16);
                    let matrix = b.read(8);
                    s.bt709 = matrix == 1 || (matrix == 2 && s.h > 576);
                }
            }
            3 => {
                let Some(s) = self.seq.as_mut() else { return };
                if b.flag() {
                    for &z in ZIGZAG.iter() {
                        s.intra_q[z as usize] = b.read(8) as u8;
                    }
                }
                if b.flag() {
                    for &z in ZIGZAG.iter() {
                        s.inter_q[z as usize] = b.read(8) as u8;
                    }
                }
            }
            8 => {
                // Picture coding extension.
                let Some(h) = self.hdr.as_mut() else { return };
                h.f_code = [[b.read(4), b.read(4)], [b.read(4), b.read(4)]];
                h.dc_prec = b.read(2);
                h.structure = b.read(2) as u8;
                h.tff = b.flag();
                h.fpfd = b.flag();
                h.concealment = b.flag();
                h.q_scale_type = b.flag();
                h.intra_vlc = b.flag();
                h.alt_scan = b.flag();
                h.rff = b.flag();
                if h.structure == 0 {
                    h.structure = FRAME;
                }
            }
            _ => {}
        }
    }

    fn picture_header(&mut self, d: &[u8], abs: u64) {
        let mut b = Bits::new(d);
        b.skip(10); // temporal reference
        let kind = b.read(3) as u8;
        b.skip(16); // vbv delay
        let mut full_pel = [false; 2];
        let mut f_code = [[1u32; 2]; 2];
        if kind == P_TYPE || kind == B_TYPE {
            full_pel[0] = b.flag();
            let f = b.read(3);
            f_code[0] = [f, f];
        }
        if kind == B_TYPE {
            full_pel[1] = b.flag();
            let f = b.read(3);
            f_code[1] = [f, f];
        }
        // The time stamp of the packet this picture starts in.
        let mut pts = None;
        while let Some(&(at, t)) = self.marks.front() {
            if at > abs {
                break;
            }
            pts = Some(t);
            self.marks.pop_front();
        }
        self.hdr = Some(Hdr {
            kind,
            full_pel,
            f_code,
            dc_prec: 0,
            structure: FRAME,
            tff: true,
            fpfd: true,
            concealment: false,
            q_scale_type: false,
            intra_vlc: false,
            alt_scan: false,
            rff: false,
            pts,
        });
    }

    fn begin_picture(&mut self) {
        self.in_slices = true;
        self.skip_pic = false;
        let (Some(h), Some(s)) = (self.hdr.clone(), self.seq.clone()) else { return };
        if !(I_TYPE..=B_TYPE).contains(&h.kind) || !s.chroma420 {
            self.skip_pic = true;
            return;
        }
        // Pictures that need pictures we don't have (just after a jump) are left out.
        if (h.kind == P_TYPE && self.last_ref.is_none()) || (h.kind == B_TYPE && (self.last_ref.is_none() || (self.prev_ref.is_none() && !self.closed_gop))) {
            if !(h.kind == P_TYPE && h.structure != FRAME && self.cur.as_ref().is_some_and(|c| c.fields == 1)) {
                self.skip_pic = true;
                return;
            }
        }
        let size = s.mbw * 16 * s.mbh * 16;
        let dur = if s.progressive {
            s.period_us * if h.rff { if h.tff { 3 } else { 2 } } else { 1 }
        } else {
            s.period_us * if h.rff { 3 } else { 2 } / 2
        };
        let second = h.structure != FRAME && self.cur.as_ref().is_some_and(|c| c.fields == 1 && c.first != h.structure);
        if second {
            // The second field of a frame; a P field may use the first field as reference.
            if h.kind == P_TYPE {
                self.snap = self.cur.as_ref().map(|c| c.pic.clone());
            }
            return;
        }
        if self.cur.is_some() {
            self.finish_frame();
        }
        self.cur = Some(Cur {
            pic: Pic { y: vec![0; size], u: vec![128; size / 4], v: vec![128; size / 4], pts: h.pts, dur, reference: h.kind != B_TYPE },
            first: h.structure,
            fields: 0,
        });
    }

    fn end_picture(&mut self) {
        self.in_slices = false;
        if self.skip_pic {
            return;
        }
        let Some(h) = self.hdr.as_ref() else { return };
        let frame = h.structure == FRAME;
        if let Some(c) = self.cur.as_mut() {
            c.fields += if frame { 2 } else { 1 };
            if c.fields >= 2 {
                self.finish_frame();
            }
        }
        self.snap = None;
    }

    fn finish_frame(&mut self) {
        let Some(c) = self.cur.take() else { return };
        if c.pic.reference {
            if let Some(last) = self.last_ref.take() {
                if !self.last_emitted {
                    self.emit(last.clone());
                }
                self.prev_ref = Some(last);
            }
            self.last_ref = Some(c.pic);
            self.last_emitted = false;
        } else {
            self.emit(c.pic);
        }
    }

    fn emit(&mut self, p: Pic) {
        let Some(s) = self.seq.as_ref() else { return };
        let pts = match (p.pts, self.last_out) {
            (Some(t), _) => t,
            (None, Some((t, d))) => t + d,
            (None, None) => 0,
        };
        self.last_out = Some((pts, p.dur));
        let stride = s.mbw * 16;
        self.out.push_back((
            pts,
            Frame { width: s.w, height: s.h, y: p.y, u: p.u, v: p.v, stride, crop_x: 0, crop_y: 0, bt709: s.bt709, full_range: false, poc: 0 },
        ));
    }

    // ---- Slices and macroblocks ----

    fn slice(&mut self, d: &[u8], vpos: usize) {
        let s = self.seq.clone().unwrap();
        let h = self.hdr.clone().unwrap();
        let mut b = Bits::new(d);
        if s.h > 2800 {
            b.skip(3);
        }
        let mut code = b.read(5) as i32;
        self.qscale = qscale(&s, &h, code);
        if !s.mpeg1 && b.peek(1) == 1 {
            b.skip(1 + 1 + 7);
            while b.flag() {
                b.skip(8);
            }
        } else {
            while b.flag() {
                b.skip(8); // extra information
            }
        }
        let field = h.structure != FRAME;
        let rows = if field { s.mbh / 2 } else { s.mbh };
        let total = s.mbw * rows;
        self.reset_dc(&h);
        self.pmv = [[[0; 2]; 2]; 2];
        let mut addr: isize = (vpos as isize - 1) * s.mbw as isize - 1;
        let mut first = true;
        loop {
            // macroblock_address_increment
            let mut inc = 0isize;
            loop {
                match self.t.addr.read(&mut b) {
                    Some(v @ 0..=32) => {
                        inc += v as isize + 1;
                        break;
                    }
                    Some(33) => inc += 33,
                    Some(34) => {}
                    _ => return,
                }
            }
            if !first {
                // Skipped macroblocks.
                for k in 1..inc {
                    let a = addr + k;
                    if a < 0 || a as usize >= total {
                        return;
                    }
                    self.skipped(&s, &h, a as usize % s.mbw, a as usize / s.mbw);
                }
            }
            addr += inc;
            first = false;
            if addr < 0 || addr as usize >= total {
                return;
            }
            let (mx, my) = (addr as usize % s.mbw, addr as usize / s.mbw);
            if self.macroblock(&mut b, &s, &h, mx, my, &mut code).is_none() {
                return;
            }
            if b.pos >= d.len() * 8 || b.peek(23) == 0 {
                return;
            }
        }
    }

    fn reset_dc(&mut self, h: &Hdr) {
        self.dc_pred = [1 << (7 + h.dc_prec); 3];
    }

    fn skipped(&mut self, s: &Seq, h: &Hdr, mx: usize, my: usize) {
        self.reset_dc(h);
        if h.kind == P_TYPE {
            self.pmv = [[[0; 2]; 2]; 2];
            let mut m = MbMotion { flags: MF, kind: if h.structure == FRAME { Motion::Frame } else { Motion::Field }, mv: [[[0; 2]; 4]; 2], fs: [[0; 2]; 2] };
            if h.structure != FRAME {
                m.fs[0][0] = (h.structure - 1) as usize;
            }
            self.predict(s, h, mx, my, &m);
            self.last = m;
        } else if h.kind == B_TYPE {
            // Same directions as the last macroblock, with the predicted vectors, as one
            // frame (or same-parity field) prediction.
            let mut m = MbMotion { flags: self.last.flags & (MF | MB), kind: if h.structure == FRAME { Motion::Frame } else { Motion::Field }, mv: [[[0; 2]; 4]; 2], fs: [[0; 2]; 2] };
            for dir in 0..2 {
                let k = if h.full_pel[dir] { 2 } else { 1 };
                m.mv[dir][0] = [self.pmv[0][dir][0] * k, self.pmv[0][dir][1] * k];
                if h.structure != FRAME {
                    m.fs[dir][0] = (h.structure - 1) as usize;
                }
            }
            self.predict(s, h, mx, my, &m);
        }
    }

    fn macroblock(&mut self, b: &mut Bits, s: &Seq, h: &Hdr, mx: usize, my: usize, code: &mut i32) -> Option<()> {
        let flags = match h.kind {
            I_TYPE => self.t.mb_i.read(b)?,
            P_TYPE => self.t.mb_p.read(b)?,
            _ => self.t.mb_b.read(b)?,
        };
        let frame = h.structure == FRAME;
        let intra = flags & INTRA != 0;
        let moving = flags & (MF | MB) != 0;
        let mut kind = if frame { Motion::Frame } else { Motion::Field };
        if moving {
            if frame {
                if !h.fpfd {
                    kind = [Motion::Frame, Motion::Field, Motion::Frame, Motion::Dual][b.read(2) as usize];
                }
            } else {
                kind = [Motion::Field, Motion::Field, Motion::F16x8, Motion::Dual][b.read(2) as usize];
            }
        }
        let field_dct = frame && !h.fpfd && (intra || flags & PAT != 0) && b.flag();
        if flags & QUANT != 0 {
            *code = b.read(5) as i32;
            self.qscale = qscale(s, h, *code);
        }
        let mut m = MbMotion { flags, kind, mv: [[[0; 2]; 4]; 2], fs: [[0; 2]; 2] };
        if intra {
            if h.concealment {
                // Concealment vectors only update the predictors.
                let mut cm = MbMotion { flags: MF, kind: if frame { Motion::Frame } else { Motion::Field }, mv: [[[0; 2]; 4]; 2], fs: [[0; 2]; 2] };
                self.vectors(b, s, h, 0, &mut cm)?;
                b.skip(1);
            } else {
                self.pmv = [[[0; 2]; 2]; 2];
            }
        } else {
            self.reset_dc(h);
            if flags & MF != 0 {
                self.vectors(b, s, h, 0, &mut m)?;
            }
            if flags & MB != 0 {
                self.vectors(b, s, h, 1, &mut m)?;
            }
            if h.kind == P_TYPE && flags & MF == 0 {
                // "No motion": zero vector from the same field.
                self.pmv = [[[0; 2]; 2]; 2];
                m.flags |= MF;
                m.kind = if frame { Motion::Frame } else { Motion::Field };
                if !frame {
                    m.fs[0][0] = (h.structure - 1) as usize;
                }
            }
        }
        let cbp = if flags & PAT != 0 {
            self.t.pat.read(b)?
        } else if intra {
            63
        } else {
            0
        };
        if !intra {
            self.predict(s, h, mx, my, &m);
            self.last = m;
        }
        // Blocks: four luma, then Cb and Cr.
        let stride = s.mbw * 16;
        let parity = if frame { 0 } else { (h.structure - 1) as usize };
        let (ls, lbase) = if frame { (stride, my * 16 * stride) } else { (stride * 2, parity * stride + my * 16 * stride * 2) };
        let cstride = stride / 2;
        let (cs, cbase) = if frame { (cstride, my * 8 * cstride) } else { (cstride * 2, parity * cstride + my * 8 * cstride * 2) };
        let mut blk = [0i16; 64];
        for k in 0..6 {
            if cbp & (32 >> k) == 0 {
                continue;
            }
            blk = [0; 64];
            self.block(b, s, h, intra, if k < 4 { 0 } else { k - 3 }, &mut blk)?;
            let cur = &mut self.cur.as_mut()?.pic;
            if k < 4 {
                let (bx, by) = (mx * 16 + (k & 1) * 8, k >> 1);
                let (at, st) = if field_dct { (lbase + by * ls + bx, ls * 2) } else { (lbase + by * 8 * ls + bx, ls) };
                if intra {
                    idct::put(&mut blk, &mut cur.y, at, st);
                } else {
                    idct::add(&mut blk, &mut cur.y, at, st);
                }
            } else {
                let at = cbase + mx * 8;
                let plane = if k == 4 { &mut cur.u } else { &mut cur.v };
                if intra {
                    idct::put(&mut blk, plane, at, cs);
                } else {
                    idct::add(&mut blk, plane, at, cs);
                }
            }
        }
        let _ = blk;
        Some(())
    }

    /// motion_vectors(s) for direction `dir`.
    fn vectors(&mut self, b: &mut Bits, _s: &Seq, h: &Hdr, dir: usize, m: &mut MbMotion) -> Option<()> {
        let frame = h.structure == FRAME;
        let fc = h.f_code[dir];
        match m.kind {
            Motion::Frame => {
                let x = self.mv_comp(b, fc[0], self.pmv[0][dir][0])?;
                let y = self.mv_comp(b, fc[1], self.pmv[0][dir][1])?;
                self.pmv[0][dir] = [x, y];
                self.pmv[1][dir] = [x, y];
                let k = if h.full_pel[dir] { 2 } else { 1 };
                m.mv[dir][0] = [x * k, y * k];
            }
            Motion::Field if frame => {
                // Two field vectors (vertical predicted at half scale).
                for r in 0..2 {
                    m.fs[dir][r] = b.read(1) as usize;
                    let x = self.mv_comp(b, fc[0], self.pmv[r][dir][0])?;
                    let y = self.mv_comp(b, fc[1], self.pmv[r][dir][1] >> 1)?;
                    self.pmv[r][dir] = [x, y * 2];
                    m.mv[dir][r] = [x, y];
                }
            }
            Motion::Field => {
                m.fs[dir][0] = b.read(1) as usize;
                let x = self.mv_comp(b, fc[0], self.pmv[0][dir][0])?;
                let y = self.mv_comp(b, fc[1], self.pmv[0][dir][1])?;
                self.pmv[0][dir] = [x, y];
                self.pmv[1][dir] = [x, y];
                m.mv[dir][0] = [x, y];
            }
            Motion::F16x8 => {
                for r in 0..2 {
                    m.fs[dir][r] = b.read(1) as usize;
                    let x = self.mv_comp(b, fc[0], self.pmv[r][dir][0])?;
                    let y = self.mv_comp(b, fc[1], self.pmv[r][dir][1])?;
                    self.pmv[r][dir] = [x, y];
                    m.mv[dir][r] = [x, y];
                }
            }
            Motion::Dual => {
                let shift = frame as u32;
                let x = self.mv_comp(b, fc[0], self.pmv[0][dir][0])?;
                let dmx = dmv(b);
                let y = self.mv_comp(b, fc[1], self.pmv[0][dir][1] >> shift)?;
                let dmy = dmv(b);
                self.pmv[0][dir] = [x, y << shift];
                self.pmv[1][dir] = [x, y << shift];
                m.mv[dir][0] = [x, y];
                m.mv[dir][1] = [x, y];
                let up = |v: i32, k: i32| (v * k + (v > 0) as i32) >> 1;
                if frame {
                    let k = if h.tff { 1 } else { 3 };
                    m.mv[dir][2] = [up(x, k) + dmx, up(y, k) + dmy - 1];
                    let k = 4 - k;
                    m.mv[dir][3] = [up(x, k) + dmx, up(y, k) + dmy + 1];
                } else {
                    let e = if h.structure == TOP { -1 } else { 1 };
                    m.mv[dir][2] = [up(x, 1) + dmx, up(y, 1) + dmy + e];
                }
            }
        }
        Some(())
    }

    fn mv_comp(&self, b: &mut Bits, f_code: u32, pred: i32) -> Option<i32> {
        let mut code = self.t.mv.read(b)?;
        if code != 0 && b.flag() {
            code = -code;
        }
        let r = f_code.max(1) - 1;
        let delta = if r == 0 || code == 0 {
            code
        } else {
            let d = ((code.abs() - 1) << r) + b.read(r) as i32 + 1;
            if code < 0 {
                -d
            } else {
                d
            }
        };
        let mut v = pred + delta;
        let (low, high, range) = (-(16 << r), (16 << r) - 1, 32 << r);
        if v < low {
            v += range;
        } else if v > high {
            v -= range;
        }
        Some(v)
    }

    /// Form the prediction for a non-intra macroblock in the current picture.
    fn predict(&mut self, s: &Seq, h: &Hdr, mx: usize, my: usize, m: &MbMotion) {
        let frame = h.structure == FRAME;
        let parity = if frame { 0 } else { (h.structure - 1) as usize };
        let second = !frame && self.cur.as_ref().is_some_and(|c| c.fields == 1);
        let Some(cur) = self.cur.as_mut() else { return };
        let stride = s.mbw * 16;
        let height = s.mbh * 16;
        let mut avg = false;
        for dir in 0..2 {
            if m.flags & (if dir == 0 { MF } else { MB }) == 0 {
                continue;
            }
            // Which picture each reference field comes from.
            let pick = |fs: usize| -> Option<&Pic> {
                if h.kind == B_TYPE {
                    if dir == 0 {
                        self.prev_ref.as_ref().or(self.last_ref.as_ref())
                    } else {
                        self.last_ref.as_ref()
                    }
                } else if second && fs != parity {
                    self.snap.as_ref()
                } else {
                    self.last_ref.as_ref()
                }
            };
            let mv = m.mv[dir];
            match m.kind {
                Motion::Frame => {
                    let Some(r) = pick(0) else { continue };
                    part(&mut cur.pic, r, stride, height, None, None, mx * 16, my * 16, mv[0], 16, 16, avg);
                }
                Motion::Field if frame => {
                    for f in 0..2 {
                        let Some(r) = pick(m.fs[dir][f]) else { continue };
                        part(&mut cur.pic, r, stride, height, Some(f), Some(m.fs[dir][f]), mx * 16, my * 8, mv[f], 16, 8, avg);
                    }
                }
                Motion::Field => {
                    let Some(r) = pick(m.fs[dir][0]) else { continue };
                    part(&mut cur.pic, r, stride, height, Some(parity), Some(m.fs[dir][0]), mx * 16, my * 16, mv[0], 16, 16, avg);
                }
                Motion::F16x8 => {
                    for k in 0..2 {
                        let Some(r) = pick(m.fs[dir][k]) else { continue };
                        part(&mut cur.pic, r, stride, height, Some(parity), Some(m.fs[dir][k]), mx * 16, my * 16 + k * 8, mv[k], 16, 8, avg);
                    }
                }
                Motion::Dual => {
                    if frame {
                        // Each field: same-parity prediction averaged with opposite-parity.
                        for (i, pass) in [(0usize, false), (1, true)] {
                            for f in 0..2 {
                                let src_field = f ^ i;
                                let Some(r) = pick(src_field) else { continue };
                                part(&mut cur.pic, r, stride, height, Some(f), Some(src_field), mx * 16, my * 8, mv[2 * i + f], 16, 8, pass);
                            }
                        }
                    } else {
                        for (i, pass) in [(0usize, false), (1, true)] {
                            let src_field = if i == 0 { parity } else { parity ^ 1 };
                            let Some(r) = pick(src_field) else { continue };
                            part(&mut cur.pic, r, stride, height, Some(parity), Some(src_field), mx * 16, my * 16, mv[2 * i], 16, 16, pass);
                        }
                    }
                }
            }
            avg = true;
        }
    }

    fn block(&mut self, b: &mut Bits, s: &Seq, h: &Hdr, intra: bool, comp: usize, blk: &mut [i16; 64]) -> Option<()> {
        let scan = if h.alt_scan { &ALT_SCAN } else { &ZIGZAG };
        let q = self.qscale;
        let mut i;
        let mut sum: i32 = 0;
        if intra {
            let size = if comp == 0 { self.t.dc_lum.read(b)? } else { self.t.dc_chroma.read(b)? } as u32;
            let diff = if size == 0 {
                0
            } else {
                let v = b.read(size) as i32;
                if v < 1 << (size - 1) {
                    v - (1 << size) + 1
                } else {
                    v
                }
            };
            self.dc_pred[comp] += diff;
            let dc = if s.mpeg1 { self.dc_pred[comp] * 8 } else { self.dc_pred[comp] << (3 - h.dc_prec) };
            blk[0] = dc as i16;
            sum = dc;
            i = 1;
        } else {
            i = 0;
        }
        let table = if intra && h.intra_vlc { &self.t.b15 } else { &self.t.b14 };
        let wq = if intra { &s.intra_q } else { &s.inter_q };
        let mut firstcoef = !intra;
        loop {
            let (run, level): (usize, i32);
            if firstcoef && b.peek(1) == 1 {
                // First coefficient of a non-intra block: '1s' is run 0, level 1.
                b.skip(1);
                run = 0;
                level = if b.flag() { -1 } else { 1 };
            } else {
                let idx = table.read(b)? as usize;
                if idx == 112 {
                    break; // end of block
                }
                if idx == 111 {
                    run = b.read(6) as usize;
                    level = if s.mpeg1 {
                        let l = ((b.read(8) as i32) << 24) >> 24;
                        if l == 0 {
                            b.read(8) as i32
                        } else if l == -128 {
                            b.read(8) as i32 - 256
                        } else {
                            l
                        }
                    } else {
                        ((b.read(12) as i32) << 20) >> 20
                    };
                } else {
                    run = RUN[idx] as usize;
                    let l = LEVEL[idx] as i32;
                    level = if b.flag() { -l } else { l };
                }
            }
            firstcoef = false;
            i += run;
            if i > 63 {
                return None;
            }
            let j = scan[i] as usize;
            let w = wq[j] as i32;
            let a = level.abs();
            let mut v = if s.mpeg1 {
                let v = if intra { (a * q * w) >> 3 } else { ((2 * a + 1) * q * w) >> 4 };
                if v != 0 {
                    (v - 1) | 1
                } else {
                    0
                }
            } else if intra {
                (a * q * w) >> 4
            } else {
                ((2 * a + 1) * q * w) >> 5
            };
            if level < 0 {
                v = -v;
            }
            // No saturation: FFmpeg keeps the full value (stored in 16 bits), and so do we.
            blk[j] = v as i16;
            sum ^= v;
            i += 1;
        }
        if !s.mpeg1 && sum & 1 == 0 {
            blk[63] ^= 1;
        }
        Some(())
    }
}

fn qscale(s: &Seq, h: &Hdr, code: i32) -> i32 {
    if s.mpeg1 {
        code
    } else if h.q_scale_type {
        NON_LINEAR_QSCALE[code as usize & 31] as i32
    } else {
        code * 2
    }
}

/// dmvector: 0, +1 or -1.
fn dmv(b: &mut Bits) -> i32 {
    if !b.flag() {
        0
    } else if b.flag() {
        -1
    } else {
        1
    }
}

/// Predict one part of a macroblock (luma `w` x `h`, chroma half that) from `r`.
/// `dst_field`: write every other line of that field (field prediction) or the frame (None);
/// `src_field`: read that field of `r` (or the frame). (x, y) is the part's top-left in
/// destination coordinates (field lines for fields); `mv` in half pixels.
#[allow(clippy::too_many_arguments)]
fn part(cur: &mut Pic, r: &Pic, stride: usize, height: usize, dst_field: Option<usize>, src_field: Option<usize>, x: usize, y: usize, mv: [i32; 2], w: usize, h: usize, avg: bool) {
    let cstride = stride / 2;
    let (src_y, src_u, src_v) = match src_field {
        Some(f) => (Plane::field(&r.y, stride, height, f), Plane::field(&r.u, cstride, height / 2, f), Plane::field(&r.v, cstride, height / 2, f)),
        None => (Plane::frame(&r.y, stride, height), Plane::frame(&r.u, cstride, height / 2), Plane::frame(&r.v, cstride, height / 2)),
    };
    let (dl, dls, dc, dcs) = match dst_field {
        Some(f) => (f * stride + y * stride * 2 + x, stride * 2, f * cstride + (y / 2) * cstride * 2 + x / 2, cstride * 2),
        None => (y * stride + x, stride, (y / 2) * cstride + x / 2, cstride),
    };
    let (mx, my) = (mv[0], mv[1]);
    half(&src_y, x as i32 + (mx >> 1), y as i32 + (my >> 1), mx & 1 != 0, my & 1 != 0, w, h, 0, &mut cur.y, dl, dls, avg);
    let (cx, cy) = (mx / 2, my / 2);
    let (bx, by) = ((x / 2) as i32 + (cx >> 1), (y / 2) as i32 + (cy >> 1));
    half(&src_u, bx, by, cx & 1 != 0, cy & 1 != 0, w / 2, h / 2, 0, &mut cur.u, dc, dcs, avg);
    half(&src_v, bx, by, cx & 1 != 0, cy & 1 != 0, w / 2, h / 2, 0, &mut cur.v, dc, dcs, avg);
}

impl VideoDec for Mpeg2Dec {
    fn decode(&mut self, data: &[u8]) {
        self.push(data, None);
    }
    fn pop(&mut self) -> Option<OutPic> {
        self.out.pop_front().map(|(_, f)| OutPic::Yuv(f))
    }
    fn flush(&mut self) {
        self.units(true);
        if self.in_slices {
            self.end_picture();
        }
        if let Some(c) = self.cur.as_ref() {
            if c.fields > 0 {
                self.finish_frame();
            }
        }
        if !self.last_emitted {
            if let Some(l) = self.last_ref.clone() {
                self.emit(l);
            }
            self.last_emitted = true;
        }
    }
    fn reset(&mut self) {
        self.buf.clear();
        self.base = 0;
        self.scanned = 0;
        self.marks.clear();
        self.hdr = None;
        self.in_slices = false;
        self.cur = None;
        self.snap = None;
        self.prev_ref = None;
        self.last_ref = None;
        self.last_emitted = true;
        self.out.clear();
        self.last_out = None;
        if !self.config.is_empty() {
            let c = self.config.clone();
            self.push(&c, None);
        }
    }
    fn timed(&self) -> bool {
        true
    }
    fn decode_at(&mut self, data: &[u8], pts: Option<i64>) {
        self.push(data, pts);
    }
    fn pop_at(&mut self) -> Option<(i64, OutPic)> {
        self.out.pop_front().map(|(t, f)| (t, OutPic::Yuv(f)))
    }
}

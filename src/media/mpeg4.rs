//! MPEG-4 Part 2 pictures: Xvid, DivX 4/5, older phones and cameras ("Simple" and "Advanced
//! Simple" profiles). Follows ISO/IEC 14496-2, checked against FFmpeg: I/P/B pictures, 4
//! vectors per macroblock, quarter-pixel motion, AC/DC prediction, both quantiser types,
//! interlaced coding, video packets (resync markers), and DivX's "packed" B-frames.
//! Not supported: data partitioning, global motion (treated as no motion), shapes.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use super::dec::{Pic as OutPic, VideoDec};
use super::idct;
use super::mc::{half, qpel, Op, Plane};
use super::mpeg12tab::{ALT_SCAN, ZIGZAG};
use super::mpeg4tab::*;
use super::vlc::{from_lists, from_pairs, Vlc};
use crate::video::bits::Bits;
use crate::video::h264d::Frame;

const I_VOP: u8 = 0;
const P_VOP: u8 = 1;
const B_VOP: u8 = 2;
const S_VOP: u8 = 3;

/// Index of the first "last" entry in each coefficient table.
const INTRA_LAST: usize = 67;
const INTER_LAST: usize = 58;

/// Per-macroblock kind, kept for the B pictures that follow a P picture.
const MB_INTRA: u8 = 1;
const MB_SKIP: u8 = 2;
const MB_8X8: u8 = 4;
const MB_FIELD: u8 = 8;

struct Rl {
    vlc: Vlc,
    run: &'static [u8],
    level: &'static [u8],
    last: usize,
    max_level: [[u8; 65]; 2],
    max_run: [[u8; 65]; 2],
}

impl Rl {
    fn new(codes: &[[u16; 2]], run: &'static [u8], level: &'static [u8], last: usize) -> Rl {
        let mut max_level = [[0u8; 65]; 2];
        let mut max_run = [[0u8; 65]; 2];
        for i in 0..run.len() {
            let l = (i >= last) as usize;
            let (r, v) = (run[i] as usize, level[i] as usize);
            max_level[l][r] = max_level[l][r].max(v as u8);
            max_run[l][v] = max_run[l][v].max(r as u8);
        }
        Rl { vlc: Vlc::new(&from_pairs(codes)), run, level, last, max_level, max_run }
    }
}

struct Tables {
    dc_lum: Vlc,
    dc_chrom: Vlc,
    intra: Rl,
    inter: Rl,
    mcbpc_i: Vlc,
    mcbpc_p: Vlc,
    cbpy: Vlc,
    mv: Vlc,
    mb_b: Vlc,
    sprite: Vlc,
}

impl Tables {
    fn new() -> Box<Tables> {
        // Sprite trajectory lengths: codes assigned in order from the lengths.
        let lens = [2u8, 3, 3, 3, 3, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
        let mut code = 0u32;
        let mut prev = lens[0];
        let mut sprite = Vec::new();
        for (i, &l) in lens.iter().enumerate() {
            code <<= l - prev;
            prev = l;
            sprite.push((code, l, i as i32));
            code += 1;
        }
        Box::new(Tables {
            dc_lum: Vlc::new(&from_pairs(&DC_LUM)),
            dc_chrom: Vlc::new(&from_pairs(&DC_CHROM)),
            intra: Rl::new(&INTRA_VLC, &INTRA_RUN, &INTRA_LEVEL, INTRA_LAST),
            inter: Rl::new(&INTER_VLC, &INTER_RUN, &INTER_LEVEL, INTER_LAST),
            mcbpc_i: Vlc::new(&from_lists(&INTRA_MCBPC_CODE, &INTRA_MCBPC_BITS)),
            mcbpc_p: Vlc::new(&from_lists(&INTER_MCBPC_CODE, &INTER_MCBPC_BITS)),
            cbpy: Vlc::new(&from_pairs(&CBPY)),
            mv: Vlc::new(&from_pairs(&MVTAB)),
            mb_b: Vlc::new(&[(1, 1, 0), (1, 2, 1), (1, 3, 2), (1, 4, 3)]),
            sprite: Vlc::new(&sprite),
        })
    }
}

#[derive(Clone)]
struct Vol {
    w: usize,
    h: usize,
    mbw: usize,
    mbh: usize,
    time_res: i64,
    time_bits: u32,
    progressive: bool,
    sprite: u32,
    warp_points: usize,
    quant_bits: u32,
    mpeg_quant: bool,
    intra_q: [u8; 64],
    inter_q: [u8; 64],
    qpel: bool,
    resync: bool,
    partitioned: bool,
    low_delay: bool,
}

#[derive(Clone)]
struct Pic {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

/// A reference picture with the motion the next B pictures need.
#[derive(Clone)]
struct Ref {
    pic: Pic,
    /// Vector per 8x8 luma block.
    mv: Vec<[i32; 2]>,
    kind: Vec<u8>,
    field_mv: Vec<[[i32; 2]; 2]>,
    field_sel: Vec<[u8; 2]>,
}

/// One macroblock's motion.
#[derive(Clone, Copy, Default)]
struct Motion {
    fwd: bool,
    bwd: bool,
    /// 0 = one vector, 1 = four (8x8), 2 = two field vectors.
    kind: u8,
    mv: [[[i32; 2]; 4]; 2],
    fs: [[usize; 2]; 2],
}

/// Per-picture state for prediction between macroblocks.
struct Side {
    dc: Vec<i32>,
    ac: Vec<[i32; 16]>,
    qs: Vec<u8>,
    packet: Vec<u32>,
}

#[derive(Clone, Copy)]
struct VopHdr {
    kind: u8,
    rounding: bool,
    dc_thr: i32,
    tff: bool,
    alt_scan: bool,
    qscale: i32,
    fcode: u32,
    bcode: u32,
}

pub struct Mpeg4Dec {
    t: Box<Tables>,
    config: Vec<u8>,
    vol: Option<Vol>,
    packed: bool,
    prev_ref: Option<Ref>,
    last_ref: Option<Ref>,
    last_emitted: bool,
    out: VecDeque<Frame>,
    // VOP times (in time_res units).
    time_base: i64,
    last_time_base: i64,
    last_non_b_time: i64,
    pp_time: i64,
    pb_time: i64,
    t_frame: i64,
    pp_field_time: i64,
    pb_field_time: i64,
    // Decoding state for the current picture.
    qscale: i32,
    resync_x: usize,
    resync_y: usize,
    first_line: bool,
    packet_id: u32,
    last_mv: [[[i32; 2]; 2]; 2],
}

impl Mpeg4Dec {
    pub fn new(config: &[u8]) -> Mpeg4Dec {
        let mut d = Mpeg4Dec {
            t: Tables::new(),
            config: config.to_vec(),
            vol: None,
            packed: false,
            prev_ref: None,
            last_ref: None,
            last_emitted: true,
            out: VecDeque::new(),
            time_base: 0,
            last_time_base: 0,
            last_non_b_time: 0,
            pp_time: 0,
            pb_time: 0,
            t_frame: 0,
            pp_field_time: 0,
            pb_field_time: 0,
            qscale: 1,
            resync_x: 0,
            resync_y: 0,
            first_line: true,
            packet_id: 0,
            last_mv: [[[0; 2]; 2]; 2],
        };
        let c = d.config.clone();
        d.packet(&c, false);
        d
    }

    /// One packet: the units in it (headers, pictures).
    fn packet(&mut self, data: &[u8], count: bool) {
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 3 < data.len() {
            if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                starts.push(i);
                i += 3;
            } else {
                i += 1;
            }
        }
        let vops = starts.iter().filter(|&&s| data[s + 3] == 0xB6).count();
        if count && vops >= 2 {
            self.packed = true;
        }
        for (k, &s) in starts.iter().enumerate() {
            let e = starts.get(k + 1).copied().unwrap_or(data.len());
            let code = data[s + 3];
            let body = &data[s + 4..e];
            match code {
                0x20..=0x2F => self.vol_header(body),
                0xB2 => self.user_data(body),
                0xB3 => self.gov(body),
                0xB6 => self.vop(body),
                _ => {}
            }
        }
    }

    fn user_data(&mut self, d: &[u8]) {
        // DivX writes "DivX503b1393p": a trailing 'p' means packed B-frames.
        if d.len() > 5 && &d[..4] == b"DivX" {
            let s: Vec<u8> = d.iter().copied().take_while(|&c| c != 0).collect();
            if s.last() == Some(&b'p') && s.iter().any(|&c| c == b'b') {
                self.packed = true;
            }
        }
    }

    fn gov(&mut self, d: &[u8]) {
        let mut b = Bits::new(d);
        let hours = b.read(5) as i64;
        let minutes = b.read(6) as i64;
        b.skip(1);
        let seconds = b.read(6) as i64;
        self.time_base = seconds + 60 * (minutes + 60 * hours);
    }

    fn vol_header(&mut self, d: &[u8]) {
        let mut b = Bits::new(d);
        b.skip(1); // random access
        let vo_type = b.read(8);
        let ver = if b.flag() {
            let v = b.read(4);
            b.skip(3);
            v
        } else {
            1
        };
        if b.read(4) == 15 {
            b.skip(16); // pixel aspect
        }
        let mut low_delay = matches!(vo_type, 1 | 17);
        if b.flag() {
            b.skip(2); // chroma format
            low_delay = b.flag();
            if b.flag() {
                b.skip(15 + 1 + 15 + 1 + 15 + 1 + 3 + 11 + 1 + 15 + 1);
            }
        }
        let shape = b.read(2);
        if shape != 0 {
            return; // only rectangular pictures
        }
        b.skip(1);
        let time_res = b.read(16) as i64;
        if time_res == 0 {
            return;
        }
        let time_bits = (64 - (time_res as u64 - 1).leading_zeros()).max(1);
        b.skip(1);
        if b.flag() {
            b.skip(time_bits as usize); // fixed vop rate
        }
        b.skip(1);
        let w = b.read(13) as usize;
        b.skip(1);
        let h = b.read(13) as usize;
        b.skip(1);
        let progressive = !b.flag();
        b.skip(1); // OBMC disable
        let sprite = if ver == 1 { b.read(1) } else { b.read(2) };
        let mut warp_points = 0;
        if sprite == 1 || sprite == 2 {
            if sprite == 1 {
                b.skip(13 + 1 + 13 + 1 + 13 + 1 + 13 + 1);
            }
            warp_points = b.read(6) as usize;
            b.skip(2 + 1); // accuracy, brightness change
            if sprite == 1 {
                b.skip(1);
            }
        }
        let mut quant_bits = 5;
        if b.flag() {
            quant_bits = b.read(4);
            b.skip(4);
            if !(3..=9).contains(&quant_bits) {
                quant_bits = 5;
            }
        }
        let mpeg_quant = b.flag();
        let mut intra_q = DEFAULT_INTRA;
        let mut inter_q = DEFAULT_INTER;
        if mpeg_quant {
            for m in [&mut intra_q, &mut inter_q] {
                if b.flag() {
                    let mut last = 0u8;
                    let mut i = 0;
                    while i < 64 {
                        let v = b.read(8) as u8;
                        if v == 0 {
                            break;
                        }
                        last = v;
                        m[ZIGZAG[i] as usize] = v;
                        i += 1;
                    }
                    while i < 64 {
                        m[ZIGZAG[i] as usize] = last;
                        i += 1;
                    }
                }
            }
        }
        let qpel = ver != 1 && b.flag();
        // Complexity estimation header: rarely present; skip it if it is.
        if !b.flag() {
            let method = b.read(2);
            if method < 2 {
                let groups: [&[u32]; 2] = [&[6, 4], &[4, 6]];
                for g in groups {
                    for &n in g {
                        if !b.flag() {
                            b.skip(n as usize);
                        }
                    }
                    b.skip(1);
                }
                if method == 1 {
                    b.skip(2);
                }
            }
        }
        let resync = !b.flag();
        let partitioned = b.flag();
        if partitioned {
            b.skip(1);
        }
        if w == 0 || h == 0 {
            return;
        }
        self.vol = Some(Vol {
            w,
            h,
            mbw: w.div_ceil(16),
            mbh: h.div_ceil(16),
            time_res,
            time_bits,
            progressive,
            sprite,
            warp_points,
            quant_bits,
            mpeg_quant,
            intra_q,
            inter_q,
            qpel,
            resync,
            partitioned,
            low_delay,
        });
    }

    /// A picture.
    fn vop(&mut self, d: &[u8]) {
        let Some(vol) = self.vol.clone() else { return };
        let mut b = Bits::new(d);
        let kind = b.read(2) as u8;
        let mut incr = 0;
        while b.flag() {
            incr += 1;
            if incr > 60 {
                return;
            }
        }
        b.skip(1);
        let time_inc = b.read(vol.time_bits) as i64;
        if kind != B_VOP {
            self.last_time_base = self.time_base;
            self.time_base += incr;
            let time = self.time_base * vol.time_res + time_inc;
            self.pp_time = time - self.last_non_b_time;
            self.last_non_b_time = time;
        } else {
            let time = (self.last_time_base + incr) * vol.time_res + time_inc;
            self.pb_time = self.pp_time - (self.last_non_b_time - time);
            if self.t_frame == 0 {
                self.t_frame = self.pb_time.max(1);
            }
            let rd = |a: i64, b: i64| if a >= 0 { (a + b / 2) / b } else { (a - b / 2) / b };
            let base = self.last_non_b_time - self.pp_time;
            self.pp_field_time = (rd(self.last_non_b_time, self.t_frame) - rd(base, self.t_frame)) * 2;
            self.pb_field_time = (rd(time, self.t_frame) - rd(base, self.t_frame)) * 2;
            if self.pp_field_time <= self.pb_field_time || self.pb_field_time <= 1 {
                self.pb_field_time = 2;
                self.pp_field_time = 4;
            }
        }
        b.skip(1);
        if !b.flag() {
            // Not coded: in packed streams a placeholder; otherwise the picture repeats.
            if !self.packed {
                if let Some(last) = self.last_ref.clone() {
                    let mut r = last;
                    for k in r.kind.iter_mut() {
                        *k = MB_SKIP;
                    }
                    for m in r.mv.iter_mut() {
                        *m = [0, 0];
                    }
                    self.finish(r, false);
                }
            }
            return;
        }
        let rounding = (kind == P_VOP || (kind == S_VOP && vol.sprite == 2)) && b.flag();
        let dc_thr = DC_THRESHOLD[b.read(3) as usize] as i32;
        let (mut tff, mut alt_scan) = (false, false);
        if !vol.progressive {
            tff = b.flag();
            alt_scan = b.flag();
        }
        if kind == S_VOP && (vol.sprite == 1 || vol.sprite == 2) {
            // Global motion isn't supported: read past the trajectory.
            for _ in 0..vol.warp_points {
                for _ in 0..2 {
                    let len = self.t.sprite.read(&mut b).unwrap_or(0) as u32;
                    b.skip(len as usize);
                    b.skip(1);
                }
            }
        }
        let qscale = b.read(vol.quant_bits) as i32;
        if qscale == 0 {
            return;
        }
        let fcode = if kind != I_VOP { b.read(3) } else { 1 };
        let bcode = if kind == B_VOP { b.read(3) } else { 1 };
        if fcode == 0 || bcode == 0 {
            return;
        }
        let h = VopHdr { kind, rounding, dc_thr, tff, alt_scan, qscale, fcode, bcode };

        // References: P needs one, B two; after a jump some are missing.
        if kind != I_VOP && self.last_ref.is_none() {
            return;
        }
        if kind == B_VOP {
            let ok = self.prev_ref.is_some() && self.pp_time > 0 && self.pb_time > 0 && self.pb_time < self.pp_time;
            if !ok || vol.partitioned {
                // Show the next picture in its place (keeps one picture per packet).
                if let Some(r) = self.last_ref.as_ref() {
                    let p = r.pic.clone();
                    self.emit(p);
                }
                return;
            }
        }
        if vol.partitioned {
            // Data partitioning isn't supported: repeat the last picture.
            if let Some(r) = self.last_ref.clone() {
                self.finish(r, false);
            }
            return;
        }
        let pos = b.pos;
        let r = self.decode_vop(d, pos, &vol, &h);
        if kind == B_VOP {
            self.emit(r.pic);
        } else {
            self.finish(r, true);
        }
    }

    /// A new reference picture is ready: the previous one can be shown.
    fn finish(&mut self, r: Ref, _decoded: bool) {
        if let Some(last) = self.last_ref.take() {
            if !self.last_emitted {
                let p = last.pic.clone();
                self.emit(p);
            }
            self.prev_ref = Some(last);
        }
        self.last_ref = Some(r);
        self.last_emitted = false;
    }

    fn emit(&mut self, p: Pic) {
        let Some(v) = self.vol.as_ref() else { return };
        self.out.push_back(Frame { width: v.w, height: v.h, y: p.y, u: p.u, v: p.v, stride: v.mbw * 16, crop_x: 0, crop_y: 0, bt709: v.h > 576, full_range: false, poc: 0 });
    }

    fn decode_vop(&mut self, d: &[u8], pos: usize, vol: &Vol, h: &VopHdr) -> Ref {
        let (mbw, mbh) = (vol.mbw, vol.mbh);
        let size = mbw * 16 * mbh * 16;
        let mut cur = Ref {
            pic: Pic { y: vec![0; size], u: vec![128; size / 4], v: vec![128; size / 4] },
            mv: vec![[0, 0]; mbw * 2 * mbh * 2],
            kind: vec![0; mbw * mbh],
            field_mv: vec![[[0; 2]; 2]; mbw * mbh],
            field_sel: vec![[0; 2]; mbw * mbh],
        };
        let mut side = Side { dc: vec![1024; mbw * 2 * mbh * 2 + 2 * mbw * mbh], ac: vec![[0; 16]; mbw * 2 * mbh * 2 + 2 * mbw * mbh], qs: vec![0; mbw * mbh], packet: vec![u32::MAX; mbw * mbh] };
        let mut b = Bits::new(d);
        b.pos = pos;
        self.qscale = h.qscale;
        self.resync_x = 0;
        self.resync_y = 0;
        self.first_line = true;
        self.packet_id = 0;
        self.last_mv = [[[0; 2]; 2]; 2];
        let total = mbw * mbh;
        let mut idx = 0;
        while idx < total {
            let (mx, my) = (idx % mbw, idx / mbw);
            if self.resync_x == mx && self.resync_y + 1 == my {
                self.first_line = false;
            }
            if h.kind == B_VOP && mx == 0 {
                self.last_mv = [[[0; 2]; 2]; 2];
            }
            if self.macroblock(&mut b, vol, h, mx, my, &mut cur, &mut side).is_none() {
                // Damaged: the rest of the picture keeps the last picture's content.
                if let Some(r) = self.last_ref.as_ref() {
                    copy_rest(&mut cur.pic, &r.pic, mbw, mbh, idx);
                }
                break;
            }
            side.qs[idx] = self.qscale as u8;
            side.packet[idx] = self.packet_id;
            // A video packet may start after this macroblock.
            let next = self.resync_at(&b, d.len() * 8, vol, h);
            if next != 0 && idx + 1 >= next {
                if next >= total {
                    break;
                }
                match self.packet_header(&mut b, vol, h) {
                    Some(n) => {
                        idx = n;
                        continue;
                    }
                    None => break,
                }
            }
            idx += 1;
        }
        cur
    }

    /// The macroblock number of a video packet starting here (or the total at the picture's
    /// end), else 0.
    fn resync_at(&self, b: &Bits, total_bits: usize, vol: &Vol, h: &VopHdr) -> usize {
        let data = b.data();
        let mut c = Bits::new(data);
        c.pos = b.pos;
        let code = match h.kind {
            I_VOP => 1,
            B_VOP => 3,
            _ => 2,
        };
        let mut v = c.peek(16);
        while v <= 0xFF {
            if h.kind == B_VOP || v >> (8 - code) != 1 {
                break;
            }
            c.skip(8 + code as usize);
            v = c.peek(16);
        }
        let mb_num = vol.mbw * vol.mbh;
        if c.pos + 8 >= total_bits {
            let mut v = v >> 8;
            v |= 0x7F >> (7 - (c.pos & 7));
            if v == 0x7F {
                return mb_num;
            }
            return 0;
        }
        const PREFIX: [u32; 8] = [0x7F00, 0x7E00, 0x7C00, 0x7800, 0x7000, 0x6000, 0x4000, 0x0000];
        if !vol.resync || v != PREFIX[c.pos & 7] {
            return 0;
        }
        c.skip(1);
        c.align();
        let mut len = 0;
        while len < 32 && !c.flag() {
            len += 1;
        }
        let bits = (usize::BITS - (mb_num - 1).leading_zeros()).max(1);
        let n = c.read(bits) as usize;
        if len >= prefix_len(h) {
            if n == 0 || n > mb_num {
                return mb_num;
            }
            return n;
        }
        0
    }

    /// Read a video packet header; returns the macroblock it starts at.
    fn packet_header(&mut self, b: &mut Bits, vol: &Vol, h: &VopHdr) -> Option<usize> {
        // Skip the stuffing and any stuffing macroblock codes before it.
        let code = match h.kind {
            I_VOP => 1,
            B_VOP => 3,
            _ => 2,
        };
        while h.kind != B_VOP && b.peek(16) <= 0xFF && b.peek(16) >> (8 - code) == 1 {
            b.skip(8 + code as usize);
        }
        b.skip(1);
        b.align();
        let mut len = 0;
        while len < 32 && !b.flag() {
            len += 1;
        }
        if len != prefix_len(h) {
            return None;
        }
        let mb_num = vol.mbw * vol.mbh;
        let bits = (usize::BITS - (mb_num - 1).leading_zeros()).max(1);
        let n = b.read(bits) as usize;
        if n == 0 || n >= mb_num {
            return None;
        }
        let q = b.read(vol.quant_bits) as i32;
        if q != 0 {
            self.qscale = q;
        }
        if b.flag() {
            while b.flag() {}
            b.skip(1 + vol.time_bits as usize + 1 + 2 + 3);
            if h.kind == S_VOP && vol.sprite == 2 {
                for _ in 0..vol.warp_points {
                    for _ in 0..2 {
                        let len = self.t.sprite.read(b).unwrap_or(0) as u32;
                        b.skip(len as usize + 1);
                    }
                }
            }
            if h.kind != I_VOP {
                b.skip(3);
            }
            if h.kind == B_VOP {
                b.skip(3);
            }
        }
        self.resync_x = n % vol.mbw;
        self.resync_y = n / vol.mbw;
        self.first_line = true;
        self.packet_id += 1;
        self.last_mv = [[[0; 2]; 2]; 2];
        Some(n)
    }

    #[allow(clippy::too_many_arguments)]
    fn macroblock(&mut self, b: &mut Bits, vol: &Vol, h: &VopHdr, mx: usize, my: usize, cur: &mut Ref, side: &mut Side) -> Option<()> {
        let mbw = vol.mbw;
        let xy = my * mbw + mx;
        let mut blocks = [[0i16; 64]; 6];
        let mut intra = false;
        let mut cbp: u32 = 0;
        let mut field_dct = false;
        let mut motion = Motion::default();
        let mut ac_pred = false;
        let mut use_dc_vlc = false;
        if h.kind == P_VOP || h.kind == S_VOP {
            let cbpc;
            loop {
                if b.flag() {
                    // Not coded: copied from the reference.
                    cur.kind[xy] = MB_SKIP;
                    motion.fwd = true;
                    set_mv16(cur, mbw, mx, my, [0, 0]);
                    self.predict(vol, h, mx, my, &motion, cur);
                    return Some(());
                }
                let c = self.t.mcbpc_p.read(b)?;
                if c != 20 {
                    cbpc = c as u32;
                    break;
                }
            }
            intra = cbpc & 4 != 0;
            let dquant = cbpc & 8 != 0;
            if !intra {
                if h.kind == S_VOP && vol.sprite == 2 && cbpc & 16 == 0 {
                    b.skip(1); // mcsel (global motion: treated as no motion)
                }
                let cbpy = (self.t.cbpy.read(b)? as u32) ^ 0x0F;
                cbp = (cbpc & 3) | (cbpy << 2);
                if dquant {
                    self.dquant(b);
                }
                if !vol.progressive && cbp != 0 {
                    field_dct = b.flag();
                }
                motion.fwd = true;
                if cbpc & 16 == 0 {
                    if !vol.progressive && b.flag() {
                        motion.kind = 2;
                        motion.fs[0] = [b.read(1) as usize, b.read(1) as usize];
                        let (px, py) = self.pred_mv(cur, mbw, mx, my, 0);
                        for i in 0..2 {
                            let x = self.mv_comp(b, px, h.fcode)?;
                            let y = self.mv_comp(b, py / 2, h.fcode)?;
                            motion.mv[0][i] = [x, y];
                        }
                        let sx = motion.mv[0][0][0] + motion.mv[0][1][0];
                        let sy = motion.mv[0][0][1] + motion.mv[0][1][1];
                        set_mv16(cur, mbw, mx, my, [(sx >> 1) | (sx & 1), sy]);
                        cur.kind[xy] = MB_FIELD;
                        cur.field_mv[xy] = [motion.mv[0][0], motion.mv[0][1]];
                        cur.field_sel[xy] = [motion.fs[0][0] as u8, motion.fs[0][1] as u8];
                    } else {
                        let (px, py) = self.pred_mv(cur, mbw, mx, my, 0);
                        let x = self.mv_comp(b, px, h.fcode)?;
                        let y = self.mv_comp(b, py, h.fcode)?;
                        motion.mv[0][0] = [x, y];
                        set_mv16(cur, mbw, mx, my, [x, y]);
                    }
                } else {
                    motion.kind = 1;
                    cur.kind[xy] = MB_8X8;
                    for i in 0..4 {
                        let (px, py) = self.pred_mv(cur, mbw, mx, my, i);
                        let x = self.mv_comp(b, px, h.fcode)?;
                        let y = self.mv_comp(b, py, h.fcode)?;
                        motion.mv[0][i] = [x, y];
                        cur.mv[blk_index(mbw, mx, my, i)] = [x, y];
                    }
                }
            } else {
                ac_pred = b.flag();
                let cbpy = self.t.cbpy.read(b)? as u32;
                cbp = (cbpc & 3) | (cbpy << 2);
                use_dc_vlc = self.qscale < h.dc_thr;
                if dquant {
                    self.dquant(b);
                }
                if !vol.progressive {
                    field_dct = b.flag();
                }
            }
        } else if h.kind == B_VOP {
            let next = self.last_ref.as_ref()?;
            if next.kind[xy] & MB_SKIP != 0 {
                // The next picture didn't change here: copy from the previous one.
                motion.fwd = true;
                self.predict(vol, h, mx, my, &motion, cur);
                return Some(());
            }
            let direct;
            let mut direct_skip = false;
            if b.flag() {
                direct = true;
                direct_skip = true;
            } else {
                let modb2 = b.flag();
                let t = self.t.mb_b.read(b)?;
                direct = t == 0;
                motion.fwd = t == 1 || t == 3;
                motion.bwd = t == 1 || t == 2;
                if !modb2 {
                    cbp = b.read(6);
                }
                if !direct && cbp != 0 && b.flag() {
                    let q = self.qscale + if b.flag() { 2 } else { -2 };
                    self.qscale = q.clamp(1, 31);
                }
                let mut field = false;
                if !vol.progressive {
                    if cbp != 0 {
                        field_dct = b.flag();
                    }
                    if !direct && b.flag() {
                        field = true;
                        if motion.fwd {
                            motion.fs[0] = [b.read(1) as usize, b.read(1) as usize];
                        }
                        if motion.bwd {
                            motion.fs[1] = [b.read(1) as usize, b.read(1) as usize];
                        }
                    }
                }
                if !direct {
                    let codes = [h.fcode, h.bcode];
                    for dir in 0..2 {
                        if !(if dir == 0 { motion.fwd } else { motion.bwd }) {
                            continue;
                        }
                        if !field {
                            let x = self.mv_comp(b, self.last_mv[dir][0][0], codes[dir])?;
                            let y = self.mv_comp(b, self.last_mv[dir][0][1], codes[dir])?;
                            self.last_mv[dir] = [[x, y], [x, y]];
                            motion.mv[dir][0] = [x, y];
                        } else {
                            motion.kind = 2;
                            for i in 0..2 {
                                let x = self.mv_comp(b, self.last_mv[dir][i][0], codes[dir])?;
                                let y = self.mv_comp(b, self.last_mv[dir][i][1] / 2, codes[dir])?;
                                self.last_mv[dir][i] = [x, y * 2];
                                motion.mv[dir][i] = [x, y];
                            }
                        }
                    }
                }
            }
            if direct {
                // Direct mode: vectors scaled from the next picture's, plus a small delta.
                let delta = if direct_skip { [0, 0] } else { [self.mv_comp(b, 0, 1)?, self.mv_comp(b, 0, 1)?] };
                self.direct(vol, h, xy, mx, my, delta, &mut motion);
            }
            self.predict(vol, h, mx, my, &motion, cur);
        } else {
            // I picture.
            let cbpc;
            loop {
                let c = self.t.mcbpc_i.read(b)?;
                if c != 8 {
                    cbpc = c as u32;
                    break;
                }
            }
            intra = true;
            ac_pred = b.flag();
            let cbpy = self.t.cbpy.read(b)? as u32;
            cbp = (cbpc & 3) | (cbpy << 2);
            use_dc_vlc = self.qscale < h.dc_thr;
            if cbpc & 4 != 0 {
                self.dquant(b);
            }
            if !vol.progressive {
                field_dct = b.flag();
            }
        }
        if intra {
            cur.kind[xy] = MB_INTRA;
            set_mv16(cur, mbw, mx, my, [0, 0]);
        } else if h.kind != B_VOP {
            self.predict(vol, h, mx, my, &motion, cur);
        }
        // Coefficients.
        let scan_inter: &[u8; 64] = if h.alt_scan { &ALT_SCAN } else { &ZIGZAG };
        for n in 0..6 {
            let coded = cbp & (32 >> n) != 0;
            if intra {
                self.intra_block(b, vol, h, mx, my, n, coded, ac_pred, use_dc_vlc, &mut blocks[n], side)?;
            } else if coded {
                self.inter_block(b, vol, &mut blocks[n], scan_inter)?;
            }
        }
        // Reconstruct.
        let stride = mbw * 16;
        let cs = stride / 2;
        for n in 0..6 {
            if !intra && cbp & (32 >> n) == 0 {
                continue;
            }
            let blk = &mut blocks[n];
            if n < 4 {
                let (bx, by) = (mx * 16 + (n & 1) * 8, my * 16);
                let (at, st) = if field_dct { ((by + (n >> 1)) * stride + bx, stride * 2) } else { ((by + (n >> 1) * 8) * stride + bx, stride) };
                if intra {
                    idct::put(blk, &mut cur.pic.y, at, st);
                } else {
                    idct::add(blk, &mut cur.pic.y, at, st);
                }
            } else {
                let at = my * 8 * cs + mx * 8;
                let plane = if n == 4 { &mut cur.pic.u } else { &mut cur.pic.v };
                if intra {
                    idct::put(blk, plane, at, cs);
                } else {
                    idct::add(blk, plane, at, cs);
                }
            }
        }
        Some(())
    }

    fn dquant(&mut self, b: &mut Bits) {
        const TAB: [i32; 4] = [-1, -2, 1, 2];
        self.qscale = (self.qscale + TAB[b.read(2) as usize]).clamp(1, 31);
    }

    fn mv_comp(&self, b: &mut Bits, pred: i32, f_code: u32) -> Option<i32> {
        let code = self.t.mv.read(b)?;
        if code == 0 {
            return Some(pred);
        }
        let sign = b.flag();
        let shift = f_code - 1;
        let mut val = code;
        if shift > 0 {
            val = ((val - 1) << shift) | b.read(shift) as i32;
            val += 1;
        }
        if sign {
            val = -val;
        }
        val += pred;
        // Wrap into range (5 + f_code bits, signed).
        let bits = 5 + f_code;
        Some((val << (32 - bits)) >> (32 - bits))
    }

    /// Motion vector prediction for luma block `blk` (median of left, above, above-right).
    fn pred_mv(&self, cur: &mut Ref, mbw: usize, mx: usize, my: usize, blk: usize) -> (i32, i32) {
        let w = mbw * 2;
        let (bx, by) = ((mx * 2 + (blk & 1)) as isize, (my * 2 + (blk >> 1)) as isize);
        let get = |cur: &Ref, x: isize, y: isize| -> [i32; 2] {
            if x < 0 || y < 0 || x >= w as isize || y as usize >= cur.mv.len() / w {
                [0, 0]
            } else {
                cur.mv[y as usize * w + x as usize]
            }
        };
        let off: [isize; 4] = [2, 1, 1, -1];
        let a = get(cur, bx - 1, by);
        let mid = |a: i32, b: i32, c: i32| a.max(b).min(a.min(b).max(c));
        if self.first_line && blk < 3 {
            match blk {
                0 => {
                    if mx == self.resync_x {
                        (0, 0)
                    } else if mx + 1 == self.resync_x {
                        let c = get(cur, bx + off[0] - (bx & 1) * 0, by - 1);
                        if mx == 0 {
                            (c[0], c[1])
                        } else {
                            (mid(a[0], 0, c[0]), mid(a[1], 0, c[1]))
                        }
                    } else {
                        (a[0], a[1])
                    }
                }
                1 => {
                    if mx + 1 == self.resync_x {
                        let c = get(cur, bx + off[1], by - 1);
                        (mid(a[0], 0, c[0]), mid(a[1], 0, c[1]))
                    } else {
                        (a[0], a[1])
                    }
                }
                _ => {
                    let bb = get(cur, bx, by - 1);
                    let c = get(cur, bx + off[2], by - 1);
                    let mut a = a;
                    if mx == self.resync_x {
                        a = [0, 0];
                        if bx > 0 {
                            cur.mv[by as usize * w + bx as usize - 1] = [0, 0];
                        }
                    }
                    (mid(a[0], bb[0], c[0]), mid(a[1], bb[1], c[1]))
                }
            }
        } else {
            let bb = get(cur, bx, by - 1);
            let c = get(cur, bx + off[blk], by - 1);
            (mid(a[0], bb[0], c[0]), mid(a[1], bb[1], c[1]))
        }
    }

    /// Form the motion-compensated prediction of a macroblock.
    fn predict(&self, vol: &Vol, h: &VopHdr, mx: usize, my: usize, m: &Motion, cur: &mut Ref) {
        let stride = vol.mbw * 16;
        let height = vol.mbh * 16;
        let mut op = if h.rounding { Op::PutNoRnd } else { Op::Put };
        for dir in 0..2 {
            if !(if dir == 0 { m.fwd } else { m.bwd }) {
                continue;
            }
            let r = if h.kind == B_VOP {
                if dir == 0 {
                    self.prev_ref.as_ref()
                } else {
                    self.last_ref.as_ref()
                }
            } else {
                self.last_ref.as_ref()
            };
            let Some(r) = r else { continue };
            let p = &r.pic;
            let mv = &m.mv[dir];
            match m.kind {
                0 => {
                    if vol.qpel {
                        mc_qpel(cur, p, stride, height, mx, my, mv[0], op);
                    } else {
                        mc_hpel(cur, p, stride, height, mx, my, mv[0], op);
                    }
                }
                1 => mc_4mv(cur, p, stride, height, mx, my, mv, vol.qpel, op),
                _ => {
                    for f in 0..2 {
                        mc_field(cur, p, stride, height, mx, my, f, m.fs[dir][f], mv[f], vol.qpel, op);
                    }
                }
            }
            op = Op::Avg;
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn intra_block(&mut self, b: &mut Bits, vol: &Vol, h: &VopHdr, mx: usize, my: usize, n: usize, coded: bool, ac_pred: bool, use_dc_vlc: bool, blk: &mut [i16; 64], side: &mut Side) -> Option<()> {
        let mbw = vol.mbw;
        let q = self.qscale;
        let scale = if n < 4 { Y_DC_SCALE[q as usize] } else { C_DC_SCALE[q as usize] } as i32;
        // DC prediction (from left, above-left and above).
        let (a, bb, c, here) = self.dc_neighbours(vol, mx, my, n, side);
        let (pred, dir_top) = if (a - bb).abs() < (bb - c).abs() { (c, true) } else { (a, false) };
        let mut level = [0i32; 64];
        let mut i: isize;
        if use_dc_vlc {
            let size = if n < 4 { self.t.dc_lum.read(b)? } else { self.t.dc_chrom.read(b)? } as u32;
            let mut v = 0;
            if size > 0 {
                let x = b.read(size) as i32;
                v = if x >> (size - 1) == 0 { x - (1 << size) + 1 } else { x };
                if size > 8 {
                    b.skip(1);
                }
            }
            level[0] = v;
            i = 0;
        } else {
            i = -1;
        }
        let scan: &[u8; 64] = if ac_pred {
            if dir_top {
                if h.alt_scan {
                    &ALT_SCAN
                } else {
                    &ALT_H_SCAN
                }
            } else {
                &ALT_SCAN
            }
        } else if h.alt_scan {
            &ALT_SCAN
        } else {
            &ZIGZAG
        };
        if coded {
            let rl = &self.t.intra;
            loop {
                let (run, lv, last) = read_coef(b, rl, 1, 0)?;
                i += run as isize + 1;
                if i > 63 {
                    return None;
                }
                level[scan[i as usize] as usize] = lv;
                if last {
                    break;
                }
            }
        }
        // DC: the coded difference plus the prediction (in quantiser steps).
        let pq = (pred + (scale >> 1)) / scale;
        let dc = level[0] + pq;
        let mut stored = dc * scale;
        if stored & !2047 != 0 {
            stored = if stored < 0 { 0 } else { 2047 };
        }
        side.dc[here] = stored;
        level[0] = dc;
        // AC prediction: the first row or column from the neighbour, rescaled if its
        // quantiser differs.
        let acn = self.ac_neighbour(vol, mx, my, n, dir_top, side);
        if ac_pred {
            if let Some((idx, nq)) = acn {
                let src = side.ac[idx];
                let rescale = nq != 0 && nq as i32 != q;
                for k in 1..8 {
                    let v = if dir_top { src[8 + k] } else { src[k] };
                    let v = if rescale { rounded_div(v * nq as i32, q) } else { v };
                    if dir_top {
                        level[k] += v;
                    } else {
                        level[k * 8] += v;
                    }
                }
            }
        }
        let mut save = [0i32; 16];
        for k in 1..8 {
            save[k] = level[k * 8];
            save[8 + k] = level[k];
        }
        side.ac[here] = save;
        // Dequantise.
        let mut sum: i32 = -1;
        blk[0] = (level[0] * scale) as i16;
        sum += level[0] * scale;
        let (qmul, qadd) = (q * 2, (q - 1) | 1);
        for j in 1..64 {
            let l = level[j];
            if l == 0 {
                continue;
            }
            let v = if vol.mpeg_quant {
                let w = vol.intra_q[j] as i32;
                let a = (l.abs() * q * w) >> 3;
                if l < 0 {
                    -a
                } else {
                    a
                }
            } else if l < 0 {
                l * qmul - qadd
            } else {
                l * qmul + qadd
            };
            blk[j] = v as i16;
            sum += v;
        }
        if vol.mpeg_quant {
            blk[63] ^= (sum & 1) as i16;
        }
        let _ = mbw;
        Some(())
    }

    /// DC values left (a), above-left (b) and above (c) of block n, and where this block's goes.
    fn dc_neighbours(&self, vol: &Vol, mx: usize, my: usize, n: usize, side: &Side) -> (i32, i32, i32, usize) {
        let (gw, gh, base, bx, by) = if n < 4 {
            (vol.mbw * 2, vol.mbh * 2, 0, (mx * 2 + (n & 1)) as isize, (my * 2 + (n >> 1)) as isize)
        } else {
            let lum = vol.mbw * 2 * vol.mbh * 2;
            (vol.mbw, vol.mbh, lum + (n - 4) * vol.mbw * vol.mbh, mx as isize, my as isize)
        };
        let at = |x: isize, y: isize| -> i32 {
            if x < 0 || y < 0 || x >= gw as isize || y >= gh as isize {
                1024
            } else {
                side.dc[base + y as usize * gw + x as usize]
            }
        };
        let mut a = at(bx - 1, by);
        let mut b = at(bx - 1, by - 1);
        let mut c = at(bx, by - 1);
        if self.first_line && n != 3 {
            if n != 2 {
                b = 1024;
                c = 1024;
            }
            if n != 1 && mx == self.resync_x {
                b = 1024;
                a = 1024;
            }
        }
        if mx == self.resync_x && my == self.resync_y + 1 && (n == 0 || n == 4 || n == 5) {
            b = 1024;
        }
        (a, b, c, base + by as usize * gw + bx as usize)
    }

    /// The block to predict AC from (its index and its macroblock's quantiser; 0 = same MB).
    fn ac_neighbour(&self, vol: &Vol, mx: usize, my: usize, n: usize, top: bool, side: &Side) -> Option<(usize, u8)> {
        let mbw = vol.mbw;
        // Inside this macroblock (no rescaling), or in the one to the left / above.
        let inside = if top { n == 2 || n == 3 } else { n == 1 || n == 3 };
        let (nx, ny) = if inside {
            (mx, my)
        } else if top {
            if my == 0 {
                return None;
            }
            (mx, my - 1)
        } else {
            if mx == 0 {
                return None;
            }
            (mx - 1, my)
        };
        let nb = ny * mbw + nx;
        // Blocks from an earlier video packet don't count (FFmpeg clears them at a resync).
        if !inside && side.packet[nb] != self.packet_id {
            return None;
        }
        let idx = if n < 4 {
            let (bx, by) = (mx * 2 + (n & 1), my * 2 + (n >> 1));
            let (bx, by) = if top { (bx, by - 1) } else { (bx - 1, by) };
            by * mbw * 2 + bx
        } else {
            let base = mbw * 2 * vol.mbh * 2 + (n - 4) * mbw * vol.mbh;
            base + ny * mbw + nx
        };
        Some((idx, if inside { 0 } else { side.qs[nb] }))
    }

    /// Direct mode vectors from the next picture's motion at this macroblock (scaled by the
    /// pictures' time distances) plus `delta`.
    #[allow(clippy::too_many_arguments)]
    fn direct(&self, vol: &Vol, h: &VopHdr, xy: usize, mx: usize, my: usize, delta: [i32; 2], m: &mut Motion) {
        let Some(next) = self.last_ref.as_ref() else { return };
        m.fwd = true;
        m.bwd = true;
        let (pp, pb) = (self.pp_time.max(1), self.pb_time);
        let one = |p: i32, d: i32| -> (i32, i32) {
            let f = (p as i64 * pb / pp) as i32 + d;
            let b = if d != 0 { f - p } else { (p as i64 * (pb - pp) / pp) as i32 };
            (f, b)
        };
        let kind = next.kind[xy];
        if kind & MB_8X8 != 0 {
            m.kind = 1;
            for i in 0..4 {
                let p = next.mv[blk_index(vol.mbw, mx, my, i)];
                let (fx, bx) = one(p[0], delta[0]);
                let (fy, by) = one(p[1], delta[1]);
                m.mv[0][i] = [fx, fy];
                m.mv[1][i] = [bx, by];
            }
        } else if kind & MB_FIELD != 0 {
            m.kind = 2;
            for i in 0..2 {
                let fsel = next.field_sel[xy][i] as i64;
                m.fs[0][i] = fsel as usize;
                m.fs[1][i] = i;
                let k = i as i64;
                let (tpp, tpb) = if h.tff { (self.pp_field_time - fsel + k, self.pb_field_time - fsel + k) } else { (self.pp_field_time + fsel - k, self.pb_field_time + fsel - k) };
                let tpp = tpp.max(1);
                let p = next.field_mv[xy][i];
                for c in 0..2 {
                    let f = (p[c] as i64 * tpb / tpp) as i32 + delta[c];
                    let b = if delta[c] != 0 { f - p[c] } else { (p[c] as i64 * (tpb - tpp) / tpp) as i32 };
                    m.mv[0][i][c] = f;
                    m.mv[1][i][c] = b;
                }
            }
        } else {
            let p = next.mv[blk_index(vol.mbw, mx, my, 0)];
            let (fx, bx) = one(p[0], delta[0]);
            let (fy, by) = one(p[1], delta[1]);
            // With quarter pixels FFmpeg treats it as four equal vectors (chroma differs).
            m.kind = if vol.qpel { 1 } else { 0 };
            for i in 0..4 {
                m.mv[0][i] = [fx, fy];
                m.mv[1][i] = [bx, by];
            }
        }
    }

    fn inter_block(&mut self, b: &mut Bits, vol: &Vol, blk: &mut [i16; 64], scan: &[u8; 64]) -> Option<()> {
        let q = self.qscale;
        let (qmul, qadd) = if vol.mpeg_quant { (1, 0) } else { (q * 2, (q - 1) | 1) };
        let rl = &self.t.inter;
        let mut i: isize = -1;
        let mut sum: i32 = -1;
        loop {
            let (run, lv, last) = read_coef(b, rl, qmul, qadd)?;
            i += run as isize + 1;
            if i > 63 {
                return None;
            }
            let j = scan[i as usize] as usize;
            let v = if vol.mpeg_quant {
                let w = vol.inter_q[j] as i32;
                let a = ((2 * lv.abs() + 1) * q * w) >> 4;
                if lv < 0 {
                    -a
                } else {
                    a
                }
            } else {
                lv
            };
            blk[j] = v as i16;
            sum += v;
            if last {
                break;
            }
        }
        if vol.mpeg_quant {
            blk[63] ^= (sum & 1) as i16;
        }
        Some(())
    }
}


/// Minimum zero-run of a video packet's resync marker.
fn prefix_len(h: &VopHdr) -> usize {
    match h.kind {
        I_VOP => 16,
        B_VOP => (h.fcode.max(h.bcode).max(2) + 15) as usize,
        _ => (h.fcode + 15) as usize,
    }
}

fn rounded_div(a: i32, b: i32) -> i32 {
    if a >= 0 {
        (a + (b >> 1)) / b
    } else {
        (a - (b >> 1)) / b
    }
}

/// One coefficient: (run, level, last). `qmul`/`qadd` dequantise H.263-style (inter blocks).
fn read_coef(b: &mut Bits, rl: &Rl, qmul: i32, qadd: i32) -> Option<(usize, i32, bool)> {
    let deq = |l: i32| if l > 0 { l * qmul + qadd } else if l < 0 { l * qmul - qadd } else { 0 };
    let idx = rl.vlc.read(b)? as usize;
    if idx < rl.run.len() {
        let last = idx >= rl.last;
        let l = rl.level[idx] as i32;
        let l = if b.flag() { -l } else { l };
        return Some((rl.run[idx] as usize, deq(l), last));
    }
    // Escape.
    if b.peek(1) == 0 {
        // Type 1: level offset.
        b.skip(1);
        let idx = rl.vlc.read(b)? as usize;
        if idx >= rl.run.len() {
            return None;
        }
        let last = idx >= rl.last;
        let run = rl.run[idx] as usize;
        let l = rl.level[idx] as i32 + rl.max_level[last as usize][run] as i32;
        let l = if b.flag() { -l } else { l };
        return Some((run, deq(l), last));
    }
    if b.peek(2) == 2 {
        // Type 2: run offset.
        b.skip(2);
        let idx = rl.vlc.read(b)? as usize;
        if idx >= rl.run.len() {
            return None;
        }
        let last = idx >= rl.last;
        let l = rl.level[idx] as i32;
        let run = rl.run[idx] as usize + rl.max_run[last as usize][l as usize] as usize + 1;
        let l = if b.flag() { -l } else { l };
        return Some((run, deq(l), last));
    }
    // Type 3: fixed length.
    b.skip(2);
    let last = b.flag();
    let run = b.read(6) as usize;
    b.skip(1);
    let l = ((b.read(12) as i32) << 20) >> 20;
    b.skip(1);
    let v = deq(l).clamp(-2048, 2047);
    Some((run, v, last))
}

fn blk_index(mbw: usize, mx: usize, my: usize, i: usize) -> usize {
    (my * 2 + (i >> 1)) * mbw * 2 + mx * 2 + (i & 1)
}

fn set_mv16(cur: &mut Ref, mbw: usize, mx: usize, my: usize, mv: [i32; 2]) {
    for i in 0..4 {
        cur.mv[blk_index(mbw, mx, my, i)] = mv;
    }
}

fn copy_rest(cur: &mut Pic, r: &Pic, mbw: usize, mbh: usize, from: usize) {
    let stride = mbw * 16;
    for idx in from..mbw * mbh {
        let (mx, my) = (idx % mbw, idx / mbw);
        for row in 0..16 {
            let o = (my * 16 + row) * stride + mx * 16;
            cur.y[o..o + 16].copy_from_slice(&r.y[o..o + 16]);
        }
        for row in 0..8 {
            let o = (my * 8 + row) * stride / 2 + mx * 8;
            cur.u[o..o + 8].copy_from_slice(&r.u[o..o + 8]);
            cur.v[o..o + 8].copy_from_slice(&r.v[o..o + 8]);
        }
    }
}

/// Planes of a picture as motion compensation sources.
fn planes(p: &Pic, stride: usize, height: usize, field: Option<usize>) -> (Plane<'_>, Plane<'_>, Plane<'_>) {
    let cs = stride / 2;
    match field {
        Some(f) => (Plane::field(&p.y, stride, height, f), Plane::field(&p.u, cs, height / 2, f), Plane::field(&p.v, cs, height / 2, f)),
        None => (Plane::frame(&p.y, stride, height), Plane::frame(&p.u, cs, height / 2), Plane::frame(&p.v, cs, height / 2)),
    }
}

fn hop(op: Op) -> (u32, bool) {
    match op {
        Op::Put => (0, false),
        Op::PutNoRnd => (1, false),
        Op::Avg => (0, true),
    }
}

/// One vector, half-pixel.
#[allow(clippy::too_many_arguments)]
fn mc_hpel(cur: &mut Ref, r: &Pic, stride: usize, height: usize, mx: usize, my: usize, mv: [i32; 2], op: Op) {
    let (sy, su, sv) = planes(r, stride, height, None);
    let (rnd, avg) = hop(op);
    let (x, y) = ((mx * 16) as i32 + (mv[0] >> 1), (my * 16) as i32 + (mv[1] >> 1));
    half(&sy, x, y, mv[0] & 1 != 0, mv[1] & 1 != 0, 16, 16, rnd, &mut cur.pic.y, my * 16 * stride + mx * 16, stride, avg);
    // Chroma: H.263 rounding (a quarter or three quarters counts as half).
    let (cx, cy) = (x >> 1, y >> 1);
    let (hx, hy) = (mv[0] & 3 != 0, mv[1] & 3 != 0);
    let cs = stride / 2;
    let at = my * 8 * cs + mx * 8;
    half(&su, cx, cy, hx, hy, 8, 8, rnd, &mut cur.pic.u, at, cs, avg);
    half(&sv, cx, cy, hx, hy, 8, 8, rnd, &mut cur.pic.v, at, cs, avg);
}

/// One vector, quarter-pixel.
#[allow(clippy::too_many_arguments)]
fn mc_qpel(cur: &mut Ref, r: &Pic, stride: usize, height: usize, mx: usize, my: usize, mv: [i32; 2], op: Op) {
    let (sy, su, sv) = planes(r, stride, height, None);
    let (x, y) = ((mx * 16) as i32 + (mv[0] >> 2), (my * 16) as i32 + (mv[1] >> 2));
    qpel(&sy, x, y, (mv[0] & 3) as u32, (mv[1] & 3) as u32, 16, op, &mut cur.pic.y, my * 16 * stride + mx * 16, stride);
    let c = |v: i32| {
        let v = v / 2;
        (v >> 1) | (v & 1)
    };
    let (cmx, cmy) = (c(mv[0]), c(mv[1]));
    let (rnd, avg) = hop(op);
    let cs = stride / 2;
    let at = my * 8 * cs + mx * 8;
    let (cx, cy) = ((mx * 8) as i32 + (cmx >> 1), (my * 8) as i32 + (cmy >> 1));
    half(&su, cx, cy, cmx & 1 != 0, cmy & 1 != 0, 8, 8, rnd, &mut cur.pic.u, at, cs, avg);
    half(&sv, cx, cy, cmx & 1 != 0, cmy & 1 != 0, 8, 8, rnd, &mut cur.pic.v, at, cs, avg);
}

/// Four vectors (one per 8x8 luma block); chroma from their sum.
#[allow(clippy::too_many_arguments)]
fn mc_4mv(cur: &mut Ref, r: &Pic, stride: usize, height: usize, mx: usize, my: usize, mv: &[[i32; 2]; 4], qp: bool, op: Op) {
    let (sy, su, sv) = planes(r, stride, height, None);
    let (rnd, avg) = hop(op);
    let (mut sx, mut sy_) = (0, 0);
    for (i, v) in mv.iter().enumerate() {
        let (bx, by) = (mx * 16 + (i & 1) * 8, my * 16 + (i >> 1) * 8);
        let at = by * stride + bx;
        if qp {
            qpel(&sy, bx as i32 + (v[0] >> 2), by as i32 + (v[1] >> 2), (v[0] & 3) as u32, (v[1] & 3) as u32, 8, op, &mut cur.pic.y, at, stride);
            sx += v[0] / 2;
            sy_ += v[1] / 2;
        } else {
            half(&sy, bx as i32 + (v[0] >> 1), by as i32 + (v[1] >> 1), v[0] & 1 != 0, v[1] & 1 != 0, 8, 8, rnd, &mut cur.pic.y, at, stride, avg);
            sx += v[0];
            sy_ += v[1];
        }
    }
    const ROUND: [i32; 16] = [0, 0, 0, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 1, 1];
    let rc = |x: i32| ROUND[(x & 15) as usize] + (x >> 3);
    let (cmx, cmy) = (rc(sx), rc(sy_));
    let cs = stride / 2;
    let at = my * 8 * cs + mx * 8;
    let (cx, cy) = ((mx * 8) as i32 + (cmx >> 1), (my * 8) as i32 + (cmy >> 1));
    half(&su, cx, cy, cmx & 1 != 0, cmy & 1 != 0, 8, 8, rnd, &mut cur.pic.u, at, cs, avg);
    half(&sv, cx, cy, cmx & 1 != 0, cmy & 1 != 0, 8, 8, rnd, &mut cur.pic.v, at, cs, avg);
}

/// Field `f` (0 top, 1 bottom) of a macroblock from field `fs` of the reference.
#[allow(clippy::too_many_arguments)]
fn mc_field(cur: &mut Ref, r: &Pic, stride: usize, height: usize, mx: usize, my: usize, f: usize, fs: usize, mv: [i32; 2], qp: bool, op: Op) {
    let (sy, su, sv) = planes(r, stride, height, Some(fs));
    let (rnd, avg) = hop(op);
    let cs = stride / 2;
    let dl = (my * 16 + f) * stride + mx * 16;
    let dc = (my * 8 + f) * cs + mx * 8;
    if qp {
        let (x, y) = ((mx * 16) as i32 + (mv[0] >> 2), (my * 8) as i32 + (mv[1] >> 2));
        for k in 0..2 {
            qpel(&sy, x + 8 * k, y, (mv[0] & 3) as u32, (mv[1] & 3) as u32, 8, op, &mut cur.pic.y, dl + 8 * k as usize, stride * 2);
        }
        let cmx = mv[0] / 2;
        let cmy = mv[1] >> 1;
        let (cmx, cmy) = ((cmx >> 1) | (cmx & 1), (cmy >> 1) | (cmy & 1));
        let (cx, cy) = ((mx * 8) as i32 + (cmx >> 1), (my * 4) as i32 + (cmy >> 1));
        half(&su, cx, cy, cmx & 1 != 0, cmy & 1 != 0, 8, 4, rnd, &mut cur.pic.u, dc, cs * 2, avg);
        half(&sv, cx, cy, cmx & 1 != 0, cmy & 1 != 0, 8, 4, rnd, &mut cur.pic.v, dc, cs * 2, avg);
    } else {
        let (x, y) = ((mx * 16) as i32 + (mv[0] >> 1), (my * 8) as i32 + (mv[1] >> 1));
        half(&sy, x, y, mv[0] & 1 != 0, mv[1] & 1 != 0, 16, 8, rnd, &mut cur.pic.y, dl, stride * 2, avg);
        let (cx, cy) = (x >> 1, y >> 1);
        let (hx, hy) = (mv[0] & 3 != 0, mv[1] & 3 != 0);
        half(&su, cx, cy, hx, hy, 8, 4, rnd, &mut cur.pic.u, dc, cs * 2, avg);
        half(&sv, cx, cy, hx, hy, 8, 4, rnd, &mut cur.pic.v, dc, cs * 2, avg);
    }
}

impl VideoDec for Mpeg4Dec {
    fn decode(&mut self, data: &[u8]) {
        self.packet(data, true);
    }
    fn pop(&mut self) -> Option<OutPic> {
        self.out.pop_front().map(OutPic::Yuv)
    }
    fn flush(&mut self) {
        if !self.last_emitted {
            if let Some(r) = self.last_ref.as_ref() {
                let p = r.pic.clone();
                self.emit(p);
            }
            self.last_emitted = true;
        }
    }
    fn reset(&mut self) {
        self.prev_ref = None;
        self.last_ref = None;
        self.last_emitted = true;
        self.out.clear();
        self.t_frame = 0;
    }
}

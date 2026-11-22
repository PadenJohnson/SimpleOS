//! DivX 3 pictures (Microsoft MPEG-4 version 3, "DIV3"/"MP43" in old AVI films). An early
//! MPEG-4 relative built on H.263: I and P pictures, one half-pixel vector per macroblock,
//! its own code tables, DC/AC prediction and slices made of whole macroblock rows. Follows
//! FFmpeg's decoder (libavcodec/msmpeg4dec.c) so pictures come out the same.

use alloc::boxed::Box;
use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use super::dec::{Pic as OutPic, VideoDec};
use super::idct;
use super::mc::{half, Plane};
use super::mpeg12tab::ZIGZAG;
use super::mpeg4tab::{ALT_H_SCAN, INTER_LEVEL, INTER_RUN, INTER_VLC, INTRA_LEVEL, INTRA_RUN, INTRA_VLC};
use super::mpeg12tab::ALT_SCAN;
use super::msmpeg4tab::*;
use super::vlc::Vlc;
use crate::video::bits::Bits;
use crate::video::h264d::Frame;

/// Coefficient table: codes, run and level per entry, and where the "last" entries start.
struct Rl {
    vlc: Vlc,
    run: &'static [u8],
    level: &'static [u8],
    last: usize,
    max_level: [[u8; 65]; 2],
    max_run: [[u8; 65]; 2],
}

impl Rl {
    fn new(pairs: &[u16], run: &'static [u8], level: &'static [u8], last: usize) -> Rl {
        let codes: Vec<(u32, u8, i32)> = pairs.chunks(2).enumerate().map(|(i, p)| (p[0] as u32, p[1] as u8, i as i32)).collect();
        let mut max_level = [[0u8; 65]; 2];
        let mut max_run = [[0u8; 65]; 2];
        for i in 0..run.len() {
            let l = (i >= last) as usize;
            let (r, v) = (run[i] as usize, level[i] as usize);
            max_level[l][r] = max_level[l][r].max(v as u8);
            max_run[l][v] = max_run[l][v].max(r as u8);
        }
        Rl { vlc: Vlc::new(&codes), run, level, last, max_level, max_run }
    }
    fn from_rows(rows: &[[u16; 2]], run: &'static [u8], level: &'static [u8], last: usize) -> Rl {
        let flat: Vec<u16> = rows.iter().flat_map(|r| [r[0], r[1]]).collect();
        Rl::new(&flat, run, level, last)
    }
}

/// Codes given only by their lengths, in code order (FFmpeg's "from lengths" tables).
fn from_lengths(syms: &[u16], lens: &[u8]) -> Vlc {
    let mut acc: u64 = 0;
    let mut codes = Vec::with_capacity(syms.len());
    for (&s, &l) in syms.iter().zip(lens) {
        if l > 0 {
            codes.push(((acc >> (32 - l)) as u32, l, s as i32));
            acc += 1u64 << (32 - l);
        }
    }
    Vlc::new(&codes)
}

struct Tables {
    /// 0-2 intra luma, 3-5 intra chroma and inter.
    rl: [Rl; 6],
    dc: [[Vlc; 2]; 2],
    mv: [Vlc; 2],
    mb_i: Vlc,
    mb_p: Vlc,
}

impl Tables {
    fn new() -> Box<Tables> {
        let pairs32 = |t: &[u32]| -> Vec<(u32, u8, i32)> { t.chunks(2).enumerate().map(|(i, p)| (p[0], p[1] as u8, i as i32)).collect() };
        let dc = |i: usize, j: usize| {
            let at = (i * 2 + j) * 240;
            Vlc::new(&pairs32(&DC[at..at + 240]))
        };
        let mb_i: Vec<(u32, u8, i32)> = MB_I.chunks(2).enumerate().map(|(i, p)| (p[0] as u32, p[1] as u8, i as i32)).collect();
        Box::new(Tables {
            rl: [
                Rl::new(&T0_VLC, &T0_RUN, &T0_LEVEL, 85),
                Rl::new(&T2_VLC, &T2_RUN, &T2_LEVEL, 119),
                Rl::from_rows(&INTRA_VLC, &INTRA_RUN, &INTRA_LEVEL, 67),
                Rl::new(&T1_VLC, &T1_RUN, &T1_LEVEL, 81),
                Rl::new(&T4_VLC, &T4_RUN, &T4_LEVEL, 99),
                Rl::from_rows(&INTER_VLC, &INTER_RUN, &INTER_LEVEL, 58),
            ],
            dc: [[dc(0, 0), dc(0, 1)], [dc(1, 0), dc(1, 1)]],
            mv: [from_lengths(&MV0, &MV0_LENS), from_lengths(&MV1, &MV1_LENS)],
            mb_i: Vlc::new(&mb_i),
            mb_p: Vlc::new(&pairs32(&MB_P)),
        })
    }
}

#[derive(Clone)]
struct Pic {
    y: Vec<u8>,
    u: Vec<u8>,
    v: Vec<u8>,
}

/// Settings from the picture header.
#[derive(Clone, Copy, Default)]
struct Hdr {
    intra: bool,
    q: i32,
    rl_luma: usize,
    rl_chroma: usize,
    dc_table: usize,
    mv_table: usize,
    skip_code: bool,
}

pub struct Msmpeg4Dec {
    t: Box<Tables>,
    w: usize,
    h: usize,
    mbw: usize,
    mbh: usize,
    last: Option<Pic>,
    out: VecDeque<Frame>,
    slice_height: usize,
    flipflop: bool,
    no_rounding: bool,
    // Prediction state for the picture being decoded (with a border row and column).
    dc: Vec<i32>,
    ac: Vec<[i32; 16]>,
    coded: Vec<u8>,
    mv: Vec<[i32; 2]>,
    first_line: bool,
}

impl Msmpeg4Dec {
    pub fn new(w: u32, h: u32) -> Msmpeg4Dec {
        let (w, h) = (w.clamp(16, 4096) as usize, h.clamp(16, 4096) as usize);
        let (mbw, mbh) = (w.div_ceil(16), h.div_ceil(16));
        let lum = (2 * mbw + 1) * (2 * mbh + 1);
        let chr = (mbw + 1) * (mbh + 1);
        Msmpeg4Dec {
            t: Tables::new(),
            w,
            h,
            mbw,
            mbh,
            last: None,
            out: VecDeque::new(),
            slice_height: mbh,
            flipflop: false,
            no_rounding: false,
            dc: vec![1024; lum + 2 * chr],
            ac: vec![[0; 16]; lum + 2 * chr],
            coded: vec![0; lum],
            mv: vec![[0; 2]; mbw * mbh],
            first_line: true,
        }
    }

    /// Where block n (0-3 luma, 4-5 chroma) of a macroblock keeps its DC/AC values, and the
    /// distance to the block above.
    fn blk(&self, mx: usize, my: usize, n: usize) -> (usize, usize) {
        if n < 4 {
            let wrap = 2 * self.mbw + 1;
            ((2 * my + (n >> 1) + 1) * wrap + 2 * mx + (n & 1) + 1, wrap)
        } else {
            let lum = (2 * self.mbw + 1) * (2 * self.mbh + 1);
            let wrap = self.mbw + 1;
            (lum + (n - 4) * wrap * (self.mbh + 1) + (my + 1) * wrap + mx + 1, wrap)
        }
    }

    fn picture(&mut self, data: &[u8]) {
        let mut b = Bits::new(data);
        if (data.len() as u64) * 64 < (self.mbw * self.mbh) as u64 {
            return;
        }
        let kind = b.read(2);
        if kind > 1 {
            return;
        }
        let mut h = Hdr { intra: kind == 0, q: b.read(5) as i32, ..Hdr::default() };
        if h.q == 0 {
            return;
        }
        if h.intra {
            let code = b.read(5) as usize;
            if code < 0x17 {
                return;
            }
            self.slice_height = (self.mbh / (code - 0x16)).max(1);
            h.rl_chroma = decode012(&mut b);
            h.rl_luma = decode012(&mut b);
            h.dc_table = b.read(1) as usize;
            self.no_rounding = true;
        } else {
            if self.last.is_none() {
                return;
            }
            h.skip_code = b.flag();
            h.rl_luma = decode012(&mut b);
            h.rl_chroma = h.rl_luma;
            h.dc_table = b.read(1) as usize;
            h.mv_table = b.read(1) as usize;
            self.no_rounding = if self.flipflop { !self.no_rounding } else { false };
        }
        let stride = self.mbw * 16;
        let rows = self.mbh * 16;
        let mut cur = Pic { y: vec![0; stride * rows], u: vec![128; stride * rows / 4], v: vec![128; stride * rows / 4] };
        self.dc.fill(1024);
        self.ac.fill([0; 16]);
        self.coded.fill(0);
        self.mv.fill([0; 2]);
        let mut done = 0;
        'rows: for my in 0..self.mbh {
            if my % self.slice_height == 0 {
                self.first_line = true;
                if my > 0 {
                    self.clean_above(my);
                }
            } else {
                self.first_line = false;
            }
            for mx in 0..self.mbw {
                if self.macroblock(&mut b, &h, mx, my, &mut cur).is_none() {
                    break 'rows;
                }
                done += 1;
            }
        }
        // Whatever couldn't be read stays as it was.
        if done < self.mbw * self.mbh {
            if let Some(r) = self.last.as_ref() {
                copy_rest(&mut cur, r, self.mbw, self.mbh, done);
            }
        }
        if h.intra {
            // An extra header after the picture: frame rate, bit rate and rounding switching.
            let left = data.len() as isize * 8 - b.pos as isize;
            if (17..25).contains(&left) {
                b.skip(5 + 11);
                self.flipflop = b.flag();
            } else if left < 17 {
                self.flipflop = false;
            }
        }
        self.out.push_back(Frame { width: self.w, height: self.h, y: cur.y.clone(), u: cur.u.clone(), v: cur.v.clone(), stride, crop_x: 0, crop_y: 0, bt709: false, full_range: false, poc: 0 });
        self.last = Some(cur);
    }

    /// A new slice: forget the AC values of the row above.
    fn clean_above(&mut self, my: usize) {
        let wrap = 2 * self.mbw + 1;
        let row = 2 * my; // the row above, counting the border row
        self.ac[row * wrap..(row + 1) * wrap].fill([0; 16]);
        for n in 4..6 {
            let (at, cw) = self.blk(0, my - 1, n);
            self.ac[at - 1..at + cw].fill([0; 16]);
        }
    }

    fn macroblock(&mut self, b: &mut Bits, h: &Hdr, mx: usize, my: usize, cur: &mut Pic) -> Option<()> {
        if b.left() <= 0 {
            return None;
        }
        let stride = self.mbw * 16;
        let intra;
        let mut cbp;
        let mut ac_pred = false;
        let mut mv = [0i32; 2];
        if !h.intra {
            if h.skip_code && b.flag() {
                // Skipped: copied from the last picture.
                self.inter_done(mx, my, [0, 0]);
                self.mc(cur, mx, my, [0, 0]);
                return Some(());
            }
            let code = self.t.mb_p.read(b)?;
            intra = code & 0x40 == 0;
            cbp = (code & 0x3F) as u32;
        } else {
            intra = true;
            let code = self.t.mb_i.read(b)? as u32;
            cbp = 0;
            for i in 0..6 {
                let mut val = (code >> (5 - i)) & 1;
                if i < 4 {
                    let (xy, wrap) = self.blk(mx, my, i);
                    let lw = wrap;
                    let (a, bb, c) = (self.coded[xy - 1], self.coded[xy - 1 - lw], self.coded[xy - lw]);
                    let pred = if bb == c { a } else { c };
                    val ^= pred as u32;
                    self.coded[xy] = val as u8;
                }
                cbp |= val << (5 - i);
            }
        }
        if !intra {
            let p = self.pred_mv(mx, my);
            let sym = self.t.mv[h.mv_table].read(b)?;
            let (mut x, mut y) = if sym != 0 { (sym >> 8, sym & 0xFF) } else { (b.read(6) as i32, b.read(6) as i32) };
            x += p[0] - 32;
            y += p[1] - 32;
            let wrap = |v: i32| if v <= -64 { v + 64 } else if v >= 64 { v - 64 } else { v };
            mv = [wrap(x), wrap(y)];
        } else {
            ac_pred = b.flag();
        }
        let mut blocks = [[0i16; 64]; 6];
        for n in 0..6 {
            let coded = (cbp >> (5 - n)) & 1 != 0;
            if intra {
                self.intra_block(b, h, mx, my, n, coded, ac_pred, &mut blocks[n])?;
            } else if coded {
                self.inter_block(b, h, &mut blocks[n])?;
            }
        }
        if intra {
            self.mv[my * self.mbw + mx] = [0, 0];
            for n in 0..6 {
                let (plane, at, s) = dest(cur, stride, mx, my, n);
                idct::put(&mut blocks[n], plane, at, s);
            }
        } else {
            self.inter_done(mx, my, mv);
            self.mc(cur, mx, my, mv);
            for n in 0..6 {
                if (cbp >> (5 - n)) & 1 != 0 {
                    let (plane, at, s) = dest(cur, stride, mx, my, n);
                    idct::add(&mut blocks[n], plane, at, s);
                }
            }
        }
        Some(())
    }

    /// A macroblock without intra coding: its prediction values go back to the defaults.
    fn inter_done(&mut self, mx: usize, my: usize, mv: [i32; 2]) {
        self.mv[my * self.mbw + mx] = mv;
        for n in 0..6 {
            let (xy, _) = self.blk(mx, my, n);
            self.dc[xy] = 1024;
            self.ac[xy] = [0; 16];
            if n < 4 {
                self.coded[xy] = 0;
            }
        }
    }

    /// Vector prediction: the median of left, above and above-right (H.263).
    fn pred_mv(&self, mx: usize, my: usize) -> [i32; 2] {
        let get = |x: isize, y: isize| -> [i32; 2] {
            if x < 0 || y < 0 || x as usize >= self.mbw {
                [0, 0]
            } else {
                self.mv[y as usize * self.mbw + x as usize]
            }
        };
        let (x, y) = (mx as isize, my as isize);
        let a = get(x - 1, y);
        if self.first_line {
            return if mx == 0 { [0, 0] } else { a };
        }
        let bb = get(x, y - 1);
        let c = get(x + 1, y - 1);
        [median(a[0], bb[0], c[0]), median(a[1], bb[1], c[1])]
    }

    fn mc(&self, cur: &mut Pic, mx: usize, my: usize, mv: [i32; 2]) {
        let Some(r) = self.last.as_ref() else { return };
        let stride = self.mbw * 16;
        let height = self.mbh * 16;
        let rnd = self.no_rounding as u32;
        let sy = Plane::frame(&r.y, stride, height);
        let (x, y) = ((mx * 16) as i32 + (mv[0] >> 1), (my * 16) as i32 + (mv[1] >> 1));
        half(&sy, x, y, mv[0] & 1 != 0, mv[1] & 1 != 0, 16, 16, rnd, &mut cur.y, my * 16 * stride + mx * 16, stride, false);
        let cs = stride / 2;
        let (cx, cy) = (x >> 1, y >> 1);
        let (hx, hy) = (mv[0] & 3 != 0, mv[1] & 3 != 0);
        let at = my * 8 * cs + mx * 8;
        half(&Plane::frame(&r.u, cs, height / 2), cx, cy, hx, hy, 8, 8, rnd, &mut cur.u, at, cs, false);
        half(&Plane::frame(&r.v, cs, height / 2), cx, cy, hx, hy, 8, 8, rnd, &mut cur.v, at, cs, false);
    }

    #[allow(clippy::too_many_arguments)]
    fn intra_block(&mut self, b: &mut Bits, h: &Hdr, mx: usize, my: usize, n: usize, coded: bool, ac_pred: bool, blk: &mut [i16; 64]) -> Option<()> {
        let q = h.q;
        let scale = if n < 4 { Y_DC_SCALE[q as usize] } else { C_DC_SCALE[q as usize] } as i32;
        // DC difference.
        let mut level = self.t.dc[h.dc_table][(n >= 4) as usize].read(b)?;
        if level == 119 {
            level = b.read(8) as i32;
            if b.flag() {
                level = -level;
            }
        } else if level != 0 && b.flag() {
            level = -level;
        }
        // DC prediction from left (a), above-left (b) and above (c).
        let (xy, wrap) = self.blk(mx, my, n);
        let mut a = self.dc[xy - 1];
        let mut bb = self.dc[xy - 1 - wrap];
        let mut c = self.dc[xy - wrap];
        if self.first_line && n & 2 == 0 {
            bb = 1024;
            c = 1024;
        }
        a = (a + (scale >> 1)) / scale;
        bb = (bb + (scale >> 1)) / scale;
        c = (c + (scale >> 1)) / scale;
        let top = (a - bb).abs() <= (bb - c).abs();
        level += if top { c } else { a };
        self.dc[xy] = level * scale;
        let mut lv = [0i32; 64];
        lv[0] = level;
        if coded {
            let scan: &[u8; 64] = if ac_pred {
                if top {
                    &ALT_H_SCAN
                } else {
                    &ALT_SCAN
                }
            } else {
                &ZIGZAG
            };
            let rl = if n < 4 { h.rl_luma } else { 3 + h.rl_chroma };
            read_block(b, &self.t.rl[rl], 1, 0, 0, 0, scan, &mut lv)?;
        }
        // AC prediction: the first column (from the left) or row (from above).
        if ac_pred {
            let src = if top { self.ac[xy - wrap] } else { self.ac[xy - 1] };
            for i in 1..8 {
                if top {
                    lv[i] += src[8 + i];
                } else {
                    lv[i * 8] += src[i];
                }
            }
        }
        let mut save = [0i32; 16];
        for i in 1..8 {
            save[i] = lv[i * 8];
            save[8 + i] = lv[i];
        }
        self.ac[xy] = save;
        // Dequantise (H.263 style).
        let (qmul, qadd) = (q * 2, (q - 1) | 1);
        blk[0] = (lv[0] * scale) as i16;
        for i in 1..64 {
            let l = lv[i];
            if l != 0 {
                blk[i] = (if l < 0 { l * qmul - qadd } else { l * qmul + qadd }) as i16;
            }
        }
        Some(())
    }

    fn inter_block(&mut self, b: &mut Bits, h: &Hdr, blk: &mut [i16; 64]) -> Option<()> {
        let q = h.q;
        let mut lv = [0i32; 64];
        read_block(b, &self.t.rl[3 + h.rl_luma], q * 2, (q - 1) | 1, 1, -1, &ZIGZAG, &mut lv)?;
        for i in 0..64 {
            blk[i] = lv[i] as i16;
        }
        Some(())
    }
}

/// Read a block's coefficients into `lv` (in raster order), `qmul`/`qadd` dequantising.
/// `start` is the position before the first coefficient (-1 for inter, 0 after an intra DC).
#[allow(clippy::too_many_arguments)]
fn read_block(b: &mut Bits, rl: &Rl, qmul: i32, qadd: i32, run_diff: isize, start: isize, scan: &[u8; 64], lv: &mut [i32; 64]) -> Option<()> {
    let deq = |l: i32| if l > 0 { l * qmul + qadd } else { l * qmul - qadd };
    let mut i = start;
    loop {
        let idx = rl.vlc.read(b)? as usize;
        let (level, last);
        if idx < rl.run.len() {
            last = idx >= rl.last;
            i += rl.run[idx] as isize + 1;
            let l = deq(rl.level[idx] as i32);
            level = if b.flag() { -l } else { l };
        } else if b.peek(1) == 1 {
            // Escape 1: the level goes on from the biggest the table has for this run.
            b.skip(1);
            let idx = rl.vlc.read(b)? as usize;
            if idx >= rl.run.len() {
                return None;
            }
            last = idx >= rl.last;
            let run = rl.run[idx] as usize;
            i += run as isize + 1;
            let l = deq(rl.level[idx] as i32) + rl.max_level[last as usize][run] as i32 * qmul;
            level = if b.flag() { -l } else { l };
        } else if b.peek(2) == 1 {
            // Escape 2: the run goes on from the longest the table has for this level.
            b.skip(2);
            let idx = rl.vlc.read(b)? as usize;
            if idx >= rl.run.len() {
                return None;
            }
            last = idx >= rl.last;
            let l = rl.level[idx] as usize;
            i += rl.run[idx] as isize + 1 + rl.max_run[last as usize][l] as isize + run_diff;
            let l = deq(l as i32);
            level = if b.flag() { -l } else { l };
        } else {
            // Escape 3: last, run and level written out.
            b.skip(2);
            last = b.flag();
            let run = b.read(6) as isize;
            let l = (b.read(8) as u8 as i8) as i32;
            level = deq(l);
            i += run + 1;
        }
        if last || i > 62 {
            // FFmpeg drops a coefficient past the end (and ends the block there).
            if last && (0..64).contains(&i) {
                lv[scan[i as usize] as usize] = level;
            }
            return Some(());
        }
        lv[scan[i as usize] as usize] = level;
    }
}

fn decode012(b: &mut Bits) -> usize {
    if !b.flag() {
        0
    } else {
        1 + b.read(1) as usize
    }
}

fn median(a: i32, b: i32, c: i32) -> i32 {
    a.max(b).min(a.min(b).max(c))
}

fn dest(cur: &mut Pic, stride: usize, mx: usize, my: usize, n: usize) -> (&mut [u8], usize, usize) {
    match n {
        0..=3 => (&mut cur.y[..], (my * 16 + (n >> 1) * 8) * stride + mx * 16 + (n & 1) * 8, stride),
        4 => (&mut cur.u[..], my * 8 * stride / 2 + mx * 8, stride / 2),
        _ => (&mut cur.v[..], my * 8 * stride / 2 + mx * 8, stride / 2),
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

impl VideoDec for Msmpeg4Dec {
    fn decode(&mut self, data: &[u8]) {
        if !data.is_empty() {
            self.picture(data);
        }
    }
    fn pop(&mut self) -> Option<OutPic> {
        self.out.pop_front().map(OutPic::Yuv)
    }
    fn flush(&mut self) {}
    fn reset(&mut self) {
        self.last = None;
        self.out.clear();
    }
}

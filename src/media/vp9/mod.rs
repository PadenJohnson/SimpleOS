//! VP9 pictures (WebM, MKV, MP4; YouTube's format). Profile 0 (8-bit 4:2:0), checked
//! against FFmpeg. Structure follows FFmpeg's vp9.c / vp9block.c / vp9recon.c
//! (LGPL 2.1 or later; Ronald S. Bultje, Clement Boesch): frame header, tiles, 64x64
//! superblocks, intra and motion-compensated prediction, inverse transforms, loop filter,
//! backward probability adaptation.

mod block;
mod dsp;
mod lf;
mod prob;
mod recon;
mod tab;

use crate::media::dec::{Pic as OutPic, VideoDec};
use crate::video::h264d::Frame;
use alloc::collections::VecDeque;
use alloc::rc::Rc;
use alloc::vec;
use alloc::vec::Vec;
use prob::{Counts, Probs};
use tab::{AC_QLOOKUP, DC_QLOOKUP, MODEL_PARETO8, NORM_SHIFT};

pub const FILTER_SWITCHABLE: u8 = 4;
pub const TX_SWITCHABLE: u8 = 4;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct Mv {
    pub x: i16,
    pub y: i16,
}

#[derive(Clone, Copy)]
pub struct MvRef {
    pub mv: [Mv; 2],
    pub rf: [i8; 2],
}

const ZERO_MVREF: MvRef = MvRef { mv: [Mv { x: 0, y: 0 }; 2], rf: [0, 0] };

/// A decoded picture with what later pictures need from it.
pub struct Pic {
    pub y: Vec<u16>,
    pub u: Vec<u16>,
    pub v: Vec<u16>,
    pub stride: usize,
    pub uv_stride: usize,
    pub w: usize,
    pub h: usize,
    pub seg: Vec<u8>,
    pub mv: Vec<MvRef>,
}

/// The VP9 boolean (range) decoder.
pub struct Rac {
    buf: Vec<u8>,
    pos: usize,
    high: u32,
    bits: i32,
    code: u32,
    end_reached: u32,
}

impl Rac {
    pub fn new(d: &[u8]) -> Rac {
        let mut buf = d.to_vec();
        buf.extend_from_slice(&[0, 0, 0, 0]);
        let code = (buf[0] as u32) << 16 | (buf[1] as u32) << 8 | buf[2] as u32;
        Rac { buf, pos: 3, high: 255, bits: -16, code, end_reached: 0 }
    }
    fn end(&self) -> usize {
        self.buf.len() - 4
    }
    #[inline]
    fn renorm(&mut self) -> u32 {
        let shift = NORM_SHIFT[self.high as usize] as u32;
        self.high <<= shift;
        let mut code = self.code << shift;
        self.bits += shift as i32;
        if self.bits >= 0 && self.pos < self.end() {
            code |= ((self.buf[self.pos] as u32) << 8 | self.buf[self.pos + 1] as u32) << self.bits;
            self.pos += 2;
            self.bits -= 16;
        }
        code
    }
    #[inline]
    pub fn prob(&mut self, p: u8) -> bool {
        let code = self.renorm();
        let low = 1 + (((self.high - 1) * p as u32) >> 8);
        let ls = low << 16;
        if code >= ls {
            self.high -= low;
            self.code = code - ls;
            true
        } else {
            self.high = low;
            self.code = code;
            false
        }
    }
    pub fn bit(&mut self) -> bool {
        self.prob(128)
    }
    pub fn uint(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.bit() as u32;
        }
        v
    }
    /// Tree decode: positive entries are the next node, others are -(leaf).
    pub fn tree(&mut self, t: &[[i8; 2]], probs: &[u8]) -> usize {
        let mut i = 0i32;
        loop {
            i = t[i as usize][self.prob(probs[i as usize]) as usize] as i32;
            if i <= 0 {
                return (-i) as usize;
            }
        }
    }
    pub fn is_end(&mut self) -> bool {
        if self.end() <= self.pos && self.bits >= 0 {
            self.end_reached += 1;
        }
        self.end_reached > 10
    }
}

struct Bits<'a> {
    d: &'a [u8],
    pos: usize,
}

impl Bits<'_> {
    fn get(&mut self, n: u32) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            let b = self.d.get(self.pos >> 3).copied().unwrap_or(0);
            v = (v << 1) | ((b >> (7 - (self.pos & 7))) & 1) as u32;
            self.pos += 1;
        }
        v
    }
    fn flag(&mut self) -> bool {
        self.get(1) != 0
    }
    fn sbits_inv(&mut self, n: u32) -> i32 {
        let v = self.get(n) as i32;
        if self.flag() {
            -v
        } else {
            v
        }
    }
}

#[derive(Clone, Copy, Default)]
pub struct SegFeat {
    q_enabled: bool,
    lf_enabled: bool,
    ref_enabled: bool,
    skip_enabled: bool,
    ref_val: u8,
    q_val: i16,
    lf_val: i8,
    qmul: [[i16; 2]; 2],
    lflvl: [[u8; 2]; 4],
}

#[derive(Clone, Copy, Default)]
pub struct Header {
    profile: u8,
    keyframe: bool,
    invisible: bool,
    errorres: bool,
    intraonly: bool,
    resetctx: u8,
    refreshrefmask: u8,
    highprecisionmvs: bool,
    filtermode: u8,
    allowcompinter: bool,
    refreshctx: bool,
    parallelmode: bool,
    framectxid: usize,
    use_last_frame_mvs: bool,
    refidx: [usize; 3],
    signbias: [u8; 3],
    fixcompref: u8,
    varcompref: [u8; 2],
    filter_level: u8,
    sharpness: i8,
    lf_enabled: bool,
    lf_mode: [i8; 2],
    lf_ref: [i8; 4],
    yac_qi: u8,
    ydc_qdelta: i8,
    uvdc_qdelta: i8,
    uvac_qdelta: i8,
    lossless: bool,
    seg_enabled: bool,
    seg_temporal: bool,
    seg_absolute: bool,
    seg_update_map: bool,
    seg_prob: [u8; 7],
    seg_pred_prob: [u8; 3],
    feat: [SegFeat; 8],
    txfmmode: u8,
    comppredmode: u8,
    log2_tile_cols: u32,
    log2_tile_rows: u32,
}

#[derive(Clone, Copy, Default)]
pub struct Block {
    seg_id: usize,
    intra: bool,
    comp: bool,
    rf: [usize; 2],
    mode: [usize; 4],
    uvmode: usize,
    skip: bool,
    filter: usize,
    mv: [[Mv; 2]; 4],
    bs: usize,
    tx: usize,
    uvtx: usize,
    bl: usize,
    bp: usize,
}

/// Loop filter levels and edge masks for one 64x64 superblock.
#[derive(Clone)]
pub struct Filter {
    level: [u8; 64],
    /// [plane 0=y 1=uv][0=col 1=row][row 0..8][0=16 1=8 2=4 3=inner 4].
    mask: [[[[u8; 4]; 8]; 2]; 2],
}

pub struct Vp9Dec {
    h: Header,
    last_keyframe: bool,
    w: usize,
    hgt: usize,
    sb_cols: usize,
    sb_rows: usize,
    cols: usize,
    rows: usize,
    lim_lut: [u8; 64],
    mblim_lut: [u8; 64],
    prob_ctx: [(Probs, [u8; 1728]); 4],
    p: Probs,
    /// Forward-updated coefficient probabilities [4][2][2][6][6][11].
    coef: Vec<u8>,
    counts: Counts,
    // Above contexts.
    above_partition: Vec<u8>,
    above_mode: Vec<u8>,
    above_y_nnz: Vec<u8>,
    above_uv_nnz: [Vec<u8>; 2],
    above_skip: Vec<u8>,
    above_txfm: Vec<u8>,
    above_segpred: Vec<u8>,
    above_intra: Vec<u8>,
    above_comp: Vec<u8>,
    above_ref: Vec<u8>,
    above_filter: Vec<u8>,
    above_mv: Vec<[Mv; 2]>,
    intra_pred: [Vec<u16>; 3],
    lflvl: Vec<Filter>,
    // Left contexts and tile state.
    left_y_nnz: [u8; 16],
    left_mode: [u8; 16],
    left_mv: [[Mv; 2]; 16],
    left_uv_nnz: [[u8; 16]; 2],
    left_partition: [u8; 8],
    left_skip: [u8; 8],
    left_txfm: [u8; 8],
    left_segpred: [u8; 8],
    left_intra: [u8; 8],
    left_comp: [u8; 8],
    left_ref: [u8; 8],
    left_filter: [u8; 8],
    tiles: Vec<Rac>,
    tc: usize,
    tile_col_start: usize,
    row: usize,
    col: usize,
    min_mv: (i32, i32),
    max_mv: (i32, i32),
    b: Block,
    block: Vec<i32>,
    uvblock: [Vec<i32>; 2],
    eob: [u16; 256],
    uveob: [[u16; 64]; 2],
    // Pictures.
    cur: Option<Pic>,
    refs: [Option<Rc<Pic>>; 8],
    last: Option<Rc<Pic>>,
    segmap_ref: Option<Rc<Pic>>,
    mvpair_ref: Option<Rc<Pic>>,
    full_range: bool,
    bt709: bool,
    /// Bits per sample (8, 10 or 12) and the colour space code (5 = BT.2020).
    pub(super) bpp: u32,
    colorspace: u32,
    /// Per reference: position scale (1/16384 steps; 0 = same size, -1 = unusable) and step.
    pub(super) mvscale: [[i32; 2]; 3],
    pub(super) mvstep: [[i32; 2]; 3],
    out: VecDeque<Frame>,
    error: bool,
}

const BLANK_FILTER: Filter = Filter { level: [0; 64], mask: [[[[0; 4]; 8]; 2]; 2] };

impl Vp9Dec {
    pub fn new() -> Vp9Dec {
        let d = (Probs::defaults(), prob::default_coef());
        Vp9Dec {
            h: Header { sharpness: -1, ..Header::default() },
            last_keyframe: false,
            w: 0,
            hgt: 0,
            sb_cols: 0,
            sb_rows: 0,
            cols: 0,
            rows: 0,
            lim_lut: [0; 64],
            mblim_lut: [0; 64],
            prob_ctx: [d, d, d, d],
            p: Probs::defaults(),
            coef: vec![0; 576 * 11],
            counts: Counts::new(),
            above_partition: Vec::new(),
            above_mode: Vec::new(),
            above_y_nnz: Vec::new(),
            above_uv_nnz: [Vec::new(), Vec::new()],
            above_skip: Vec::new(),
            above_txfm: Vec::new(),
            above_segpred: Vec::new(),
            above_intra: Vec::new(),
            above_comp: Vec::new(),
            above_ref: Vec::new(),
            above_filter: Vec::new(),
            above_mv: Vec::new(),
            intra_pred: [Vec::new(), Vec::new(), Vec::new()],
            lflvl: Vec::new(),
            left_y_nnz: [0; 16],
            left_mode: [0; 16],
            left_mv: [[Mv::default(); 2]; 16],
            left_uv_nnz: [[0; 16]; 2],
            left_partition: [0; 8],
            left_skip: [0; 8],
            left_txfm: [0; 8],
            left_segpred: [0; 8],
            left_intra: [0; 8],
            left_comp: [0; 8],
            left_ref: [0; 8],
            left_filter: [0; 8],
            tiles: Vec::new(),
            tc: 0,
            tile_col_start: 0,
            row: 0,
            col: 0,
            min_mv: (0, 0),
            max_mv: (0, 0),
            b: Block::default(),
            block: vec![0; 64 * 64],
            uvblock: [vec![0; 32 * 32], vec![0; 32 * 32]],
            eob: [0; 256],
            uveob: [[0; 64]; 2],
            cur: None,
            refs: Default::default(),
            last: None,
            segmap_ref: None,
            mvpair_ref: None,
            full_range: false,
            bt709: false,
            bpp: 8,
            colorspace: 0,
            mvscale: [[0; 2]; 3],
            mvstep: [[0; 2]; 3],
            out: VecDeque::new(),
            error: false,
        }
    }

    fn update_size(&mut self, w: usize, h: usize) {
        if w == self.w && h == self.hgt && !self.intra_pred[0].is_empty() {
            return;
        }
        self.w = w;
        self.hgt = h;
        self.sb_cols = w.div_ceil(64);
        self.sb_rows = h.div_ceil(64);
        self.cols = w.div_ceil(8);
        self.rows = h.div_ceil(8);
        let n8 = self.sb_cols * 8;
        self.intra_pred = [vec![0; n8 * 8 + 64], vec![0; n8 * 4 + 64], vec![0; n8 * 4 + 64]];
        self.above_y_nnz = vec![0; self.sb_cols * 16 + 16];
        self.above_mode = vec![0; self.sb_cols * 16 + 16];
        self.above_mv = vec![[Mv::default(); 2]; self.sb_cols * 16 + 16];
        self.above_uv_nnz = [vec![0; self.sb_cols * 16 + 16], vec![0; self.sb_cols * 16 + 16]];
        for v in [
            &mut self.above_partition,
            &mut self.above_skip,
            &mut self.above_txfm,
            &mut self.above_segpred,
            &mut self.above_intra,
            &mut self.above_comp,
            &mut self.above_ref,
            &mut self.above_filter,
        ] {
            *v = vec![0; n8 + 8];
        }
        self.lflvl = vec![BLANK_FILTER; self.sb_cols];
    }

    fn update_prob(c: &mut Rac, p: u8) -> u8 {
        const INV_MAP: [u8; 255] = [
            7, 20, 33, 46, 59, 72, 85, 98, 111, 124, 137, 150, 163, 176, 189, 202, 215, 228, 241, 254, 1, 2, 3, 4, 5, 6, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 21, 22, 23, 24, 25, 26, 27, 28,
            29, 30, 31, 32, 34, 35, 36, 37, 38, 39, 40, 41, 42, 43, 44, 45, 47, 48, 49, 50, 51, 52, 53, 54, 55, 56, 57, 58, 60, 61, 62, 63, 64, 65, 66, 67, 68, 69, 70, 71, 73, 74, 75, 76, 77, 78,
            79, 80, 81, 82, 83, 84, 86, 87, 88, 89, 90, 91, 92, 93, 94, 95, 96, 97, 99, 100, 101, 102, 103, 104, 105, 106, 107, 108, 109, 110, 112, 113, 114, 115, 116, 117, 118, 119, 120, 121,
            122, 123, 125, 126, 127, 128, 129, 130, 131, 132, 133, 134, 135, 136, 138, 139, 140, 141, 142, 143, 144, 145, 146, 147, 148, 149, 151, 152, 153, 154, 155, 156, 157, 158, 159, 160,
            161, 162, 164, 165, 166, 167, 168, 169, 170, 171, 172, 173, 174, 175, 177, 178, 179, 180, 181, 182, 183, 184, 185, 186, 187, 188, 190, 191, 192, 193, 194, 195, 196, 197, 198, 199,
            200, 201, 203, 204, 205, 206, 207, 208, 209, 210, 211, 212, 213, 214, 216, 217, 218, 219, 220, 221, 222, 223, 224, 225, 226, 227, 229, 230, 231, 232, 233, 234, 235, 236, 237, 238,
            239, 240, 242, 243, 244, 245, 246, 247, 248, 249, 250, 251, 252, 253, 253,
        ];
        let d = if !c.bit() {
            c.uint(4) as usize
        } else if !c.bit() {
            c.uint(4) as usize + 16
        } else if !c.bit() {
            c.uint(5) as usize + 32
        } else {
            let mut d = c.uint(7) as usize;
            if d >= 65 {
                d = (d << 1) - 65 + c.bit() as usize;
            }
            d + 64
        };
        let inv = |v: i32, m: i32| -> i32 {
            if v > 2 * m {
                v
            } else if v & 1 != 0 {
                m - ((v + 1) >> 1)
            } else {
                m + (v >> 1)
            }
        };
        let t = INV_MAP[d.min(254)] as i32;
        let p = p as i32;
        (if p <= 128 { 1 + inv(t, p - 1) } else { 255 - inv(t, 255 - p) }) as u8
    }

    /// Parse the frame header; returns the offset of the tile data, or Err(Some(i)) for
    /// "show existing frame i", Err(None) on a broken header.
    fn header(&mut self, data: &[u8]) -> Result<usize, Option<usize>> {
        let mut g = Bits { d: data, pos: 0 };
        if g.get(2) != 2 {
            return Err(None);
        }
        let mut profile = g.get(1) as u8;
        profile |= (g.get(1) as u8) << 1;
        if profile == 3 {
            profile += g.get(1) as u8;
        }
        if profile & 1 != 0 {
            // Profiles 1 and 3 are 4:4:4 / 4:2:2 / 4:4:0, which aren't supported.
            self.error = true;
            return Err(None);
        }
        self.h.profile = profile;
        if g.flag() {
            return Err(Some(g.get(3) as usize));
        }
        let h = &mut self.h;
        self.last_keyframe = h.keyframe;
        h.keyframe = !g.flag();
        let last_invisible = h.invisible;
        h.invisible = !g.flag();
        h.errorres = g.flag();
        h.use_last_frame_mvs = !h.errorres && !last_invisible;
        let (w, hh);
        if h.keyframe {
            if g.get(24) != 0x498342 {
                return Err(None);
            }
            // Bit depth (profile 2: 10 or 12), colour space and range; always 4:2:0 here.
            self.bpp = if profile >= 2 { 10 + 2 * g.get(1) } else { 8 };
            let cs = g.get(3);
            if cs == 7 {
                self.error = true;
                return Err(None);
            }
            self.colorspace = cs;
            self.bt709 = cs == 2 || cs == 5;
            self.full_range = g.flag();
            h.refreshrefmask = 0xff;
            w = g.get(16) as usize + 1;
            hh = g.get(16) as usize + 1;
            if g.flag() {
                g.get(32);
            }
        } else {
            h.intraonly = if h.invisible { g.flag() } else { false };
            h.resetctx = if h.errorres { 0 } else { g.get(2) as u8 };
            if h.intraonly {
                if g.get(24) != 0x498342 {
                    return Err(None);
                }
                if profile >= 2 {
                    self.bpp = 10 + 2 * g.get(1);
                    let cs = g.get(3);
                    if cs == 7 {
                        self.error = true;
                        return Err(None);
                    }
                    self.colorspace = cs;
                    self.bt709 = cs == 2 || cs == 5;
                    self.full_range = g.flag();
                } else {
                    self.bpp = 8;
                }
                h.refreshrefmask = g.get(8) as u8;
                w = g.get(16) as usize + 1;
                hh = g.get(16) as usize + 1;
                if g.flag() {
                    g.get(32);
                }
            } else {
                h.refreshrefmask = g.get(8) as u8;
                for i in 0..3 {
                    h.refidx[i] = g.get(3) as usize;
                    h.signbias[i] = (g.flag() && !h.errorres) as u8;
                }
                if h.refidx.iter().any(|&r| self.refs[r].is_none()) {
                    return Err(None);
                }
                let size_of = |r: &Option<Rc<Pic>>| r.as_ref().map(|p| (p.w, p.h)).unwrap_or((1, 1));
                if g.flag() {
                    (w, hh) = size_of(&self.refs[h.refidx[0]]);
                } else if g.flag() {
                    (w, hh) = size_of(&self.refs[h.refidx[1]]);
                } else if g.flag() {
                    (w, hh) = size_of(&self.refs[h.refidx[2]]);
                } else {
                    w = g.get(16) as usize + 1;
                    hh = g.get(16) as usize + 1;
                }
                h.use_last_frame_mvs &= self.last.as_ref().map(|p| p.w == w && p.h == hh).unwrap_or(false);
                if g.flag() {
                    g.get(32);
                }
                h.highprecisionmvs = g.flag();
                h.filtermode = if g.flag() { FILTER_SWITCHABLE } else { g.get(2) as u8 };
                h.allowcompinter = h.signbias[0] != h.signbias[1] || h.signbias[0] != h.signbias[2];
                if h.allowcompinter {
                    if h.signbias[0] == h.signbias[1] {
                        h.fixcompref = 2;
                        h.varcompref = [0, 1];
                    } else if h.signbias[0] == h.signbias[2] {
                        h.fixcompref = 1;
                        h.varcompref = [0, 2];
                    } else {
                        h.fixcompref = 0;
                        h.varcompref = [1, 2];
                    }
                }
            }
        }
        h.refreshctx = if h.errorres { false } else { g.flag() };
        h.parallelmode = if h.errorres { true } else { g.flag() };
        let c = g.get(2) as usize;
        h.framectxid = if h.keyframe || h.intraonly { 0 } else { c };
        if h.keyframe || h.errorres || h.intraonly {
            h.lf_ref = [1, 0, -1, -1];
            h.lf_mode = [0, 0];
            h.feat = [SegFeat::default(); 8];
        }
        h.filter_level = g.get(6) as u8;
        let sharp = g.get(3) as i8;
        if h.sharpness != sharp {
            for i in 1..64usize {
                let mut limit = i as i32;
                if sharp > 0 {
                    limit >>= (sharp as i32 + 3) >> 2;
                    limit = limit.min(9 - sharp as i32);
                }
                limit = limit.max(1);
                self.lim_lut[i] = limit as u8;
                self.mblim_lut[i] = (2 * (i as i32 + 2) + limit) as u8;
            }
        }
        h.sharpness = sharp;
        h.lf_enabled = g.flag();
        if h.lf_enabled && g.flag() {
            for i in 0..4 {
                if g.flag() {
                    h.lf_ref[i] = g.sbits_inv(6) as i8;
                }
            }
            for i in 0..2 {
                if g.flag() {
                    h.lf_mode[i] = g.sbits_inv(6) as i8;
                }
            }
        }
        h.yac_qi = g.get(8) as u8;
        h.ydc_qdelta = if g.flag() { g.sbits_inv(4) as i8 } else { 0 };
        h.uvdc_qdelta = if g.flag() { g.sbits_inv(4) as i8 } else { 0 };
        h.uvac_qdelta = if g.flag() { g.sbits_inv(4) as i8 } else { 0 };
        h.lossless = h.yac_qi == 0 && h.ydc_qdelta == 0 && h.uvdc_qdelta == 0 && h.uvac_qdelta == 0;
        h.seg_enabled = g.flag();
        if h.seg_enabled {
            h.seg_update_map = g.flag();
            if h.seg_update_map {
                for i in 0..7 {
                    h.seg_prob[i] = if g.flag() { g.get(8) as u8 } else { 255 };
                }
                h.seg_temporal = g.flag();
                if h.seg_temporal {
                    for i in 0..3 {
                        h.seg_pred_prob[i] = if g.flag() { g.get(8) as u8 } else { 255 };
                    }
                }
            }
            if g.flag() {
                h.seg_absolute = g.flag();
                for f in h.feat.iter_mut() {
                    f.q_enabled = g.flag();
                    if f.q_enabled {
                        f.q_val = g.sbits_inv(8) as i16;
                    }
                    f.lf_enabled = g.flag();
                    if f.lf_enabled {
                        f.lf_val = g.sbits_inv(6) as i8;
                    }
                    f.ref_enabled = g.flag();
                    if f.ref_enabled {
                        f.ref_val = g.get(2) as u8;
                    }
                    f.skip_enabled = g.flag();
                }
            }
        } else {
            h.seg_temporal = false;
            h.seg_update_map = false;
        }
        let bpp = self.bpp;
        let clip8 = |v: i32| v.clamp(0, 255) as usize;
        let clip6 = |v: i32| v.clamp(0, 63) as u8;
        for i in 0..if h.seg_enabled { 8 } else { 1 } {
            let f = h.feat[i];
            let qyac0 = if h.seg_enabled && f.q_enabled {
                if h.seg_absolute {
                    clip8(f.q_val as i32) as i32
                } else {
                    clip8(h.yac_qi as i32 + f.q_val as i32) as i32
                }
            } else {
                h.yac_qi as i32
            };
            let qydc = clip8(qyac0 + h.ydc_qdelta as i32);
            let quvdc = clip8(qyac0 + h.uvdc_qdelta as i32);
            let quvac = clip8(qyac0 + h.uvac_qdelta as i32);
            let qyac = clip8(qyac0);
            let ff = &mut h.feat[i];
            let bi = 256 * ((bpp - 8) / 2) as usize;
            ff.qmul = [[DC_QLOOKUP[qydc + bi], AC_QLOOKUP[qyac + bi]], [DC_QLOOKUP[quvdc + bi], AC_QLOOKUP[quvac + bi]]];
            let sh = (h.filter_level >= 32) as i32;
            let lflvl = if h.seg_enabled && f.lf_enabled {
                if h.seg_absolute {
                    clip6(f.lf_val as i32) as i32
                } else {
                    clip6(h.filter_level as i32 + f.lf_val as i32) as i32
                }
            } else {
                h.filter_level as i32
            };
            if h.lf_enabled {
                let v = clip6(lflvl + h.lf_ref[0] as i32 * (1 << sh));
                ff.lflvl[0] = [v, v];
                for j in 1..4 {
                    ff.lflvl[j][0] = clip6(lflvl + (h.lf_ref[j] as i32 + h.lf_mode[0] as i32) * (1 << sh));
                    ff.lflvl[j][1] = clip6(lflvl + (h.lf_ref[j] as i32 + h.lf_mode[1] as i32) * (1 << sh));
                }
            } else {
                ff.lflvl = [[lflvl as u8; 2]; 4];
            }
        }
        // Tiling.
        self.update_size(w, hh);
        let h = &mut self.h;
        h.log2_tile_cols = 0;
        while self.sb_cols > (64 << h.log2_tile_cols) {
            h.log2_tile_cols += 1;
        }
        let mut max = 0;
        while (self.sb_cols >> max) >= 4 {
            max += 1;
        }
        let max = (max as i32 - 1).max(0) as u32;
        while max > h.log2_tile_cols {
            if g.flag() {
                h.log2_tile_cols += 1;
            } else {
                break;
            }
        }
        h.log2_tile_rows = if g.flag() { 1 + g.get(1) } else { 0 };
        // Reference scaling.
        if !h.keyframe && !h.intraonly {
            for i in 0..3 {
                let r = self.refs[h.refidx[i]].as_ref().unwrap();
                if r.w == w && r.h == hh {
                    self.mvscale[i] = [0, 0];
                } else if w * 2 < r.w || hh * 2 < r.h || w > 16 * r.w || hh > 16 * r.h {
                    self.mvscale[i] = [-1, -1];
                } else {
                    let s = [((r.w << 14) / w) as i32, ((r.h << 14) / hh) as i32];
                    self.mvscale[i] = s;
                    self.mvstep[i] = [(16 * s[0]) >> 14, (16 * s[1]) >> 14];
                }
            }
        }
        if h.keyframe || h.errorres || (h.intraonly && h.resetctx == 3) {
            let d = (Probs::defaults(), prob::default_coef());
            self.prob_ctx = [d, d, d, d];
        } else if h.intraonly && h.resetctx == 2 {
            self.prob_ctx[c] = (Probs::defaults(), prob::default_coef());
        }
        let size2 = g.get(16) as usize;
        let data2 = g.pos.div_ceil(8);
        if size2 == 0 || data2 + size2 > data.len() {
            return Err(None);
        }
        let mut rc = Rac::new(&data[data2..data2 + size2]);
        if rc.bit() {
            return Err(None);
        }
        let keyframe = h.keyframe || h.intraonly;
        let mut counts = Counts::new();
        if keyframe {
            // Only the coefficient counts reset on intra frames (as FFmpeg).
            let old = core::mem::replace(&mut self.counts, Counts::new());
            counts = old;
            counts.coef.iter_mut().for_each(|v| *v = [0; 3]);
            counts.eob.iter_mut().for_each(|v| *v = [0; 2]);
        }
        self.counts = counts;
        let cid = c;
        self.p = self.prob_ctx[cid].0;
        let h = &mut self.h;
        let p = &mut self.p;
        if h.lossless {
            h.txfmmode = 0;
        } else {
            h.txfmmode = rc.uint(2) as u8;
            if h.txfmmode == 3 {
                h.txfmmode += rc.bit() as u8;
            }
            if h.txfmmode == TX_SWITCHABLE {
                for i in 0..2 {
                    if rc.prob(252) {
                        p.tx8p[i] = Self::update_prob(&mut rc, p.tx8p[i]);
                    }
                }
                for i in 0..2 {
                    for j in 0..2 {
                        if rc.prob(252) {
                            p.tx16p[i][j] = Self::update_prob(&mut rc, p.tx16p[i][j]);
                        }
                    }
                }
                for i in 0..2 {
                    for j in 0..3 {
                        if rc.prob(252) {
                            p.tx32p[i][j] = Self::update_prob(&mut rc, p.tx32p[i][j]);
                        }
                    }
                }
            }
        }
        for i in 0..4usize {
            let update = rc.bit();
            for j in 0..2 {
                for k in 0..2 {
                    for l in 0..6 {
                        for m in 0..6 {
                            if update && m >= 3 && l == 0 {
                                break;
                            }
                            if !update && m > 3 && l == 0 {
                                break;
                            }
                            let ix = prob::cidx(i, j, k, l, m);
                            let r = &self.prob_ctx[cid].1[ix * 3..ix * 3 + 3];
                            let pp = &mut self.coef[ix * 11..ix * 11 + 11];
                            for n in 0..3 {
                                pp[n] = if update && rc.prob(252) { Self::update_prob(&mut rc, r[n]) } else { r[n] };
                            }
                            let pr = pp[2] as usize;
                            pp[3..11].copy_from_slice(&MODEL_PARETO8[pr * 8..pr * 8 + 8]);
                        }
                    }
                }
            }
            if h.txfmmode as usize == i {
                break;
            }
        }
        for i in 0..3 {
            if rc.prob(252) {
                p.skip[i] = Self::update_prob(&mut rc, p.skip[i]);
            }
        }
        if !keyframe {
            for i in 0..7 {
                for j in 0..3 {
                    if rc.prob(252) {
                        p.mv_mode[i][j] = Self::update_prob(&mut rc, p.mv_mode[i][j]);
                    }
                }
            }
            if h.filtermode == FILTER_SWITCHABLE {
                for i in 0..4 {
                    for j in 0..2 {
                        if rc.prob(252) {
                            p.filter[i][j] = Self::update_prob(&mut rc, p.filter[i][j]);
                        }
                    }
                }
            }
            for i in 0..4 {
                if rc.prob(252) {
                    p.intra[i] = Self::update_prob(&mut rc, p.intra[i]);
                }
            }
            if h.allowcompinter {
                h.comppredmode = rc.bit() as u8;
                if h.comppredmode != 0 {
                    h.comppredmode += rc.bit() as u8;
                }
                if h.comppredmode == 2 {
                    for i in 0..5 {
                        if rc.prob(252) {
                            p.comp[i] = Self::update_prob(&mut rc, p.comp[i]);
                        }
                    }
                }
            } else {
                h.comppredmode = 0;
            }
            if h.comppredmode != 1 {
                for i in 0..5 {
                    for j in 0..2 {
                        if rc.prob(252) {
                            p.single_ref[i][j] = Self::update_prob(&mut rc, p.single_ref[i][j]);
                        }
                    }
                }
            }
            if h.comppredmode != 0 {
                for i in 0..5 {
                    if rc.prob(252) {
                        p.comp_ref[i] = Self::update_prob(&mut rc, p.comp_ref[i]);
                    }
                }
            }
            for i in 0..4 {
                for j in 0..9 {
                    if rc.prob(252) {
                        p.y_mode[i][j] = Self::update_prob(&mut rc, p.y_mode[i][j]);
                    }
                }
            }
            for i in 0..4 {
                for j in 0..4 {
                    for k in 0..3 {
                        if rc.prob(252) {
                            p.partition[3 - i][j][k] = Self::update_prob(&mut rc, p.partition[3 - i][j][k]);
                        }
                    }
                }
            }
            let upd = |rc: &mut Rac, v: &mut u8| {
                if rc.prob(252) {
                    *v = ((rc.uint(7) << 1) | 1) as u8;
                }
            };
            for i in 0..3 {
                upd(&mut rc, &mut p.mv_joint[i]);
            }
            for i in 0..2 {
                let m = &mut p.mv_comp[i];
                upd(&mut rc, &mut m.sign);
                for j in 0..10 {
                    upd(&mut rc, &mut m.classes[j]);
                }
                upd(&mut rc, &mut m.class0);
                for j in 0..10 {
                    upd(&mut rc, &mut m.bits[j]);
                }
            }
            for i in 0..2 {
                let m = &mut p.mv_comp[i];
                for j in 0..2 {
                    for k in 0..3 {
                        upd(&mut rc, &mut m.class0_fp[j][k]);
                    }
                }
                for j in 0..3 {
                    upd(&mut rc, &mut m.fp[j]);
                }
            }
            if h.highprecisionmvs {
                for i in 0..2 {
                    let m = &mut p.mv_comp[i];
                    upd(&mut rc, &mut m.class0_hp);
                    upd(&mut rc, &mut m.hp);
                }
            }
        }
        Ok(data2 + size2)
    }

    fn new_pic(&self) -> Pic {
        let stride = self.sb_cols * 64 + 64;
        let uv_stride = stride / 2;
        let rows = self.sb_rows * 64 + 16;
        let n = self.sb_cols * self.sb_rows * 64;
        Pic {
            y: vec![0; stride * rows],
            u: vec![1 << (self.bpp - 1); uv_stride * rows / 2],
            v: vec![1 << (self.bpp - 1); uv_stride * rows / 2],
            stride,
            uv_stride,
            w: self.w,
            h: self.hgt,
            seg: vec![0; n],
            mv: vec![ZERO_MVREF; n],
        }
    }

    fn decode_frame(&mut self, data: &[u8]) {
        let retain_segmap = self.segmap_ref.is_some() && (!self.h.seg_enabled || !self.h.seg_update_map);
        let off = match self.header(data) {
            Ok(o) => o,
            Err(Some(r)) => {
                if let Some(p) = self.refs[r].clone() {
                    self.emit(&p);
                }
                return;
            }
            Err(None) => return,
        };
        let h = self.h;
        let src = if !h.keyframe && !h.intraonly && !h.errorres { self.last.clone() } else { None };
        if !retain_segmap || h.keyframe || h.intraonly {
            self.segmap_ref = src.clone();
        }
        self.mvpair_ref = src;
        let cur = self.new_pic();
        if self.mvpair_ref.as_ref().map(|p| p.w != cur.w || p.h != cur.h).unwrap_or(true) {
            self.segmap_ref = None;
        }
        self.cur = Some(cur);
        // Above contexts.
        let cols = self.cols;
        self.above_partition[..cols].iter_mut().for_each(|v| *v = 0);
        self.above_skip[..cols].iter_mut().for_each(|v| *v = 0);
        if h.keyframe || h.intraonly {
            self.above_mode[..cols * 2].iter_mut().for_each(|v| *v = 2);
        } else {
            self.above_mode[..cols].iter_mut().for_each(|v| *v = 10);
        }
        self.above_y_nnz.iter_mut().for_each(|v| *v = 0);
        self.above_uv_nnz[0].iter_mut().for_each(|v| *v = 0);
        self.above_uv_nnz[1].iter_mut().for_each(|v| *v = 0);
        self.above_segpred[..cols].iter_mut().for_each(|v| *v = 0);
        if h.refreshctx && h.parallelmode {
            for i in 0..4usize {
                for ix in prob::cidx(i, 0, 0, 0, 0)..prob::cidx(i, 0, 0, 0, 0) + 144 {
                    for k in 0..3 {
                        self.prob_ctx[h.framectxid].1[ix * 3 + k] = self.coef[ix * 11 + k];
                    }
                }
                if h.txfmmode as usize == i {
                    break;
                }
            }
            self.prob_ctx[h.framectxid].0 = self.p;
        }
        self.error = false;
        self.decode_tiles(&data[off..]);
        if h.refreshctx && !h.parallelmode {
            let fc = h.framectxid;
            let (pc, cc) = &mut self.prob_ctx[fc];
            prob::adapt_probs(pc, cc, &self.p, &self.counts, h.keyframe, h.intraonly, self.last_keyframe, h.comppredmode, h.txfmmode == TX_SWITCHABLE, h.filtermode == FILTER_SWITCHABLE, h.highprecisionmvs);
        }
        let pic = Rc::new(self.cur.take().unwrap());
        for i in 0..8 {
            if h.refreshrefmask & (1 << i) != 0 {
                self.refs[i] = Some(pic.clone());
            }
        }
        if !h.invisible {
            self.emit(&pic);
        }
        self.last = Some(pic);
    }

    fn decode_tiles(&mut self, mut data: &[u8]) {
        let tile_cols = 1usize << self.h.log2_tile_cols;
        let tile_rows = 1usize << self.h.log2_tile_rows;
        let tile_off = |idx: usize, log2: u32, n: usize| -> (usize, usize) {
            let s = (idx * n) >> log2;
            let e = ((idx + 1) * n) >> log2;
            (s.min(n) << 3, e.min(n) << 3)
        };
        for tile_row in 0..tile_rows {
            let (rs, re) = tile_off(tile_row, self.h.log2_tile_rows, self.sb_rows);
            self.tiles.clear();
            for tile_col in 0..tile_cols {
                let size = if tile_col == tile_cols - 1 && tile_row == tile_rows - 1 {
                    data.len()
                } else {
                    if data.len() < 4 {
                        return;
                    }
                    let s = u32::from_be_bytes([data[0], data[1], data[2], data[3]]) as usize;
                    data = &data[4..];
                    s
                };
                if size > data.len() || size == 0 {
                    return;
                }
                let mut rc = Rac::new(&data[..size]);
                if rc.bit() {
                    return;
                }
                self.tiles.push(rc);
                data = &data[size..];
            }
            let mut row = rs;
            while row < re {
                for tile_col in 0..tile_cols {
                    let (cs, ce) = tile_off(tile_col, self.h.log2_tile_cols, self.sb_cols);
                    self.tile_col_start = cs;
                    self.left_partition = [0; 8];
                    self.left_skip = [0; 8];
                    if self.h.keyframe || self.h.intraonly {
                        self.left_mode = [2; 16];
                    } else {
                        self.left_mode[..8].iter_mut().for_each(|v| *v = 10);
                    }
                    self.left_y_nnz = [0; 16];
                    self.left_uv_nnz = [[0; 16]; 2];
                    self.left_segpred = [0; 8];
                    self.tc = tile_col;
                    let mut col = cs;
                    while col < ce {
                        self.lflvl[col >> 3].mask = [[[[0; 4]; 8]; 2]; 2];
                        if self.tiles[tile_col].is_end() {
                            return;
                        }
                        self.decode_sb(row, col, col >> 3, 0);
                        col += 8;
                    }
                }
                // Keep the unfiltered last line for the next row's intra prediction.
                let cur = self.cur.as_ref().unwrap();
                if row + 8 < self.rows {
                    let y0 = (row * 8 + 63) * cur.stride;
                    let n = 8 * self.cols;
                    self.intra_pred[0][..n].copy_from_slice(&cur.y[y0..y0 + n]);
                    let c0 = (row * 4 + 31) * cur.uv_stride;
                    let n = 4 * self.cols;
                    self.intra_pred[1][..n].copy_from_slice(&cur.u[c0..c0 + n]);
                    self.intra_pred[2][..n].copy_from_slice(&cur.v[c0..c0 + n]);
                }
                if self.h.filter_level != 0 {
                    let mut col = 0;
                    while col < self.cols {
                        self.loopfilter_sb(col >> 3, row, col);
                        col += 8;
                    }
                }
                row += 8;
            }
        }
    }

    fn emit(&mut self, p: &Pic) {
        let (w, h) = (p.w, p.h);
        let ow = w.next_multiple_of(2);
        let oh = h.next_multiple_of(2);
        let shift = self.bpp - 8;
        // 10/12-bit: rounded down to 8 bits; BT.2020 (HDR on YouTube) gets a gentle tone map.
        let hdr = shift > 0 && self.colorspace == 5;
        let conv = |v: u16| -> u8 {
            if shift == 0 {
                v as u8
            } else {
                ((v as u32 + (1 << (shift - 1))) >> shift).min(255) as u8
            }
        };
        let mut y = vec![0u8; ow * oh];
        let mut u = vec![128u8; ow * oh / 4];
        let mut v = vec![128u8; ow * oh / 4];
        for r in 0..h {
            for c in 0..w {
                let s = conv(p.y[r * p.stride + c]);
                y[r * ow + c] = if hdr { super::hevc::tone(s) } else { s };
            }
        }
        let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
        for r in 0..ch {
            for c in 0..cw {
                u[r * (ow / 2) + c] = conv(p.u[r * p.uv_stride + c]);
                v[r * (ow / 2) + c] = conv(p.v[r * p.uv_stride + c]);
            }
        }
        self.out.push_back(Frame { width: w, height: h, y, u, v, stride: ow, crop_x: 0, crop_y: 0, bt709: self.bt709, full_range: self.full_range, poc: 0 });
    }
}

/// Split a WebM superframe (several frames in one packet, index at the end).
fn superframe(d: &[u8]) -> Vec<&[u8]> {
    if let Some(&last) = d.last() {
        if last & 0xe0 == 0xc0 {
            let n = (last & 7) as usize + 1;
            let mag = ((last >> 3) & 3) as usize + 1;
            let idx = 2 + mag * n;
            if d.len() >= idx && d[d.len() - idx] == last {
                let mut out = Vec::new();
                let mut p = d.len() - idx + 1;
                let mut off = 0;
                for _ in 0..n {
                    let mut sz = 0usize;
                    for k in 0..mag {
                        sz |= (d[p + k] as usize) << (8 * k);
                    }
                    p += mag;
                    if off + sz > d.len() - idx {
                        break;
                    }
                    out.push(&d[off..off + sz]);
                    off += sz;
                }
                return out;
            }
        }
    }
    vec![d]
}

impl VideoDec for Vp9Dec {
    fn decode(&mut self, data: &[u8]) {
        for f in superframe(data) {
            if !f.is_empty() {
                self.decode_frame(f);
            }
        }
    }
    fn pop(&mut self) -> Option<OutPic> {
        self.out.pop_front().map(OutPic::Yuv)
    }
    fn flush(&mut self) {}
    fn reset(&mut self) {
        self.out.clear();
    }
}

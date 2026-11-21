//! H.264 decoder (Baseline, Main and High profile; progressive 8-bit 4:2:0, which is what
//! phones and cameras record). CAVLC and CABAC, I/P/B slices, 4x4 and 8x8 transforms,
//! scaling matrices, weighted prediction, spatial and temporal direct, long-term
//! references, and the deblocking filter.
//!
//! Feed it one access unit (the NAL units of one picture, as stored in MP4) at a time;
//! pictures come out in display order.

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use super::bits::{unescape, Bits};
use super::h264::ZIGZAG;

pub(crate) const ZIGZAG8: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, 17, 24, 32, 25, 18, 11, 4, 5, 12, 19, 26, 33, 40, 48, 41, 34, 27, 20, 13, 6, 7, 14, 21, 28, 35, 42, 49, 56, 57, 50, 43, 36, 29, 22, 15, 23, 30, 37, 44, 51, 58, 59, 52,
    45, 38, 31, 39, 46, 53, 60, 61, 54, 47, 55, 62, 63,
];

const QPC: [u8; 52] = [
    0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16, 17, 18, 19, 20, 21, 22, 23, 24, 25, 26, 27, 28, 29, 29, 30, 31, 32, 32, 33, 34, 34, 35, 35, 36, 36, 37, 37, 37, 38, 38, 38, 39, 39, 39,
    39,
];

const DEFAULT_4X4: [[u8; 16]; 2] = [[6, 13, 13, 20, 20, 20, 28, 28, 28, 28, 32, 32, 32, 37, 37, 42], [10, 14, 14, 20, 20, 20, 24, 24, 24, 24, 27, 27, 27, 30, 30, 34]];
const DEFAULT_8X8: [[u8; 64]; 2] = [
    [
        6, 10, 10, 13, 11, 13, 16, 16, 16, 16, 18, 18, 18, 18, 18, 23, 23, 23, 23, 23, 23, 25, 25, 25, 25, 25, 25, 25, 27, 27, 27, 27, 27, 27, 27, 27, 29, 29, 29, 29, 29, 29, 29, 31, 31, 31, 31, 31,
        31, 33, 33, 33, 33, 33, 36, 36, 36, 36, 38, 38, 38, 40, 40, 42,
    ],
    [
        9, 13, 13, 15, 13, 15, 17, 17, 17, 17, 19, 19, 19, 19, 19, 21, 21, 21, 21, 21, 21, 22, 22, 22, 22, 22, 22, 22, 24, 24, 24, 24, 24, 24, 24, 24, 25, 25, 25, 25, 25, 25, 25, 27, 27, 27, 27, 27,
        27, 28, 28, 28, 28, 28, 30, 30, 30, 30, 32, 32, 32, 33, 33, 35,
    ],
];
const V4: [[i32; 3]; 6] = [[10, 16, 13], [11, 18, 14], [13, 20, 16], [14, 23, 18], [16, 25, 20], [18, 29, 23]];
const V8: [[i32; 6]; 6] = [[20, 18, 32, 19, 25, 24], [22, 19, 35, 21, 28, 26], [26, 23, 42, 24, 33, 31], [28, 25, 45, 26, 35, 33], [32, 28, 51, 30, 40, 38], [36, 32, 58, 34, 46, 43]];

const ALPHA: [u8; 52] = [
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 4, 4, 5, 6, 7, 8, 9, 10, 12, 13, 15, 17, 20, 22, 25, 28, 32, 36, 40, 45, 50, 56, 63, 71, 80, 90, 101, 113, 127, 144, 162, 182, 203, 226, 255, 255,
];
const BETA: [u8; 52] = [0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12, 13, 13, 14, 14, 15, 15, 16, 16, 17, 17, 18, 18];
const TC0: [[u8; 3]; 52] = [
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 0],
    [0, 0, 1],
    [0, 0, 1],
    [0, 0, 1],
    [0, 0, 1],
    [0, 1, 1],
    [0, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 1],
    [1, 1, 2],
    [1, 1, 2],
    [1, 1, 2],
    [1, 1, 2],
    [1, 2, 3],
    [1, 2, 3],
    [2, 2, 3],
    [2, 2, 4],
    [2, 3, 4],
    [2, 3, 4],
    [3, 3, 5],
    [3, 4, 6],
    [3, 4, 6],
    [4, 5, 7],
    [4, 5, 8],
    [4, 6, 9],
    [5, 7, 10],
    [6, 8, 11],
    [6, 8, 13],
    [7, 10, 14],
    [8, 11, 16],
    [9, 12, 18],
    [10, 13, 20],
    [11, 15, 23],
    [13, 17, 25],
];

/// 4x4 block index in decoding (z) order -> (x, y) in 4x4 units within the macroblock.
pub(crate) const BLK_XY: [(usize, usize); 16] = [(0, 0), (1, 0), (0, 1), (1, 1), (2, 0), (3, 0), (2, 1), (3, 1), (0, 2), (1, 2), (0, 3), (1, 3), (2, 2), (3, 2), (2, 3), (3, 3)];
/// (x, y) in 4x4 units -> z-order index.
pub(crate) const BLK_Z: [[usize; 4]; 4] = [[0, 1, 4, 5], [2, 3, 6, 7], [8, 9, 12, 13], [10, 11, 14, 15]];

#[inline]
fn clip(v: i32) -> u8 {
    v.clamp(0, 255) as u8
}

// ---- parameter sets ------------------------------------------------------------------------

#[derive(Clone)]
pub struct Sps {
    pub profile: u32,
    pub chroma: u32,
    pub log2_max_frame_num: u32,
    pub poc_type: u32,
    pub log2_max_poc_lsb: u32,
    pub delta_always_zero: bool,
    pub off_non_ref: i32,
    pub off_t2b: i32,
    pub off_ref: Vec<i32>,
    pub max_refs: u32,
    pub gaps: bool,
    pub mbw: usize,
    pub mbh: usize,
    pub frame_mbs_only: bool,
    pub direct8x8: bool,
    pub crop: [u32; 4],
    pub scaling4: [[u8; 16]; 6],
    pub scaling8: [[u8; 64]; 2],
    pub reorder: Option<u32>,
    /// Colour matrix from the VUI (1 = BT.709, 5/6 = BT.601), if given.
    pub matrix: Option<u32>,
    pub full_range: bool,
    pub bit_depth: u32,
}

#[derive(Clone)]
pub struct Pps {
    pub sps: usize,
    pub cabac: bool,
    pub bottom_poc: bool,
    pub num_ref: [u32; 2],
    pub weighted: bool,
    pub bipred: u32,
    pub qp: i32,
    pub cqp: [i32; 2],
    pub deblock_ctrl: bool,
    pub constrained_intra: bool,
    pub redundant: bool,
    pub t8x8: bool,
    pub scaling4: [[u8; 16]; 6],
    pub scaling8: [[u8; 64]; 2],
    pub slice_groups: u32,
}

fn scaling_list(b: &mut Bits, list: &mut [u8], default: &[u8]) -> bool {
    let (mut last, mut next) = (8i32, 8i32);
    for j in 0..list.len() {
        if next != 0 {
            let delta = b.se();
            next = (last + delta + 256) % 256;
            if j == 0 && next == 0 {
                list.copy_from_slice(default);
                return true;
            }
        }
        list[j] = if next == 0 { last as u8 } else { next as u8 };
        last = list[j] as i32;
    }
    true
}

/// Read the 6 + 2 scaling lists with fall-back rule A (sps) or B (pps, falling back to `base`).
fn scaling_matrices(b: &mut Bits, n8: usize, s4: &mut [[u8; 16]; 6], s8: &mut [[u8; 64]; 2], base: Option<(&[[u8; 16]; 6], &[[u8; 64]; 2])>) {
    for i in 0..6 + n8 {
        let present = b.flag();
        if i < 6 {
            if present {
                let mut l = [0u8; 16];
                scaling_list(b, &mut l, &DEFAULT_4X4[i / 3]);
                s4[i] = l;
            } else if i == 0 || i == 3 {
                s4[i] = match base {
                    Some((b4, _)) => b4[i],
                    None => DEFAULT_4X4[i / 3],
                };
            } else {
                s4[i] = s4[i - 1];
            }
        } else {
            let k = i - 6;
            if k >= 2 {
                // 4:4:4 chroma 8x8 lists: read and ignore.
                if present {
                    let mut l = [0u8; 64];
                    scaling_list(b, &mut l, &DEFAULT_8X8[k % 2]);
                }
                continue;
            }
            if present {
                let mut l = [0u8; 64];
                scaling_list(b, &mut l, &DEFAULT_8X8[k]);
                s8[k] = l;
            } else {
                s8[k] = match base {
                    Some((_, b8)) => b8[k],
                    None => DEFAULT_8X8[k],
                };
            }
        }
    }
}

fn hrd(b: &mut Bits) {
    let n = b.ue() + 1;
    b.read(8);
    for _ in 0..n.min(32) {
        b.ue();
        b.ue();
        b.flag();
    }
    b.read(20);
}

pub fn parse_sps(d: &[u8]) -> Result<(usize, Sps), &'static str> {
    let mut b = Bits::new(d);
    b.read(8); // NAL header
    let profile = b.read(8);
    b.read(16);
    let id = b.ue() as usize;
    if id > 31 {
        return Err("bad SPS");
    }
    let mut s = Sps {
        profile,
        chroma: 1,
        log2_max_frame_num: 4,
        poc_type: 0,
        log2_max_poc_lsb: 4,
        delta_always_zero: false,
        off_non_ref: 0,
        off_t2b: 0,
        off_ref: Vec::new(),
        max_refs: 1,
        gaps: false,
        mbw: 0,
        mbh: 0,
        frame_mbs_only: true,
        direct8x8: true,
        crop: [0; 4],
        scaling4: [[16; 16]; 6],
        scaling8: [[16; 64]; 2],
        reorder: None,
        matrix: None,
        full_range: false,
        bit_depth: 8,
    };
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        s.chroma = b.ue();
        if s.chroma == 3 {
            b.flag();
        }
        s.bit_depth = b.ue() + 8;
        let bdc = b.ue() + 8;
        if bdc != s.bit_depth {
            s.bit_depth = bdc.max(s.bit_depth);
        }
        b.flag(); // transform bypass
        if b.flag() {
            let n8 = if s.chroma == 3 { 6 } else { 2 };
            let (mut s4, mut s8) = (s.scaling4, s.scaling8);
            scaling_matrices(&mut b, n8, &mut s4, &mut s8, None);
            s.scaling4 = s4;
            s.scaling8 = s8;
        }
    }
    s.log2_max_frame_num = b.ue() + 4;
    s.poc_type = b.ue();
    if s.poc_type == 0 {
        s.log2_max_poc_lsb = b.ue() + 4;
    } else if s.poc_type == 1 {
        s.delta_always_zero = b.flag();
        s.off_non_ref = b.se();
        s.off_t2b = b.se();
        let n = b.ue().min(255);
        for _ in 0..n {
            s.off_ref.push(b.se());
        }
    }
    s.max_refs = b.ue();
    s.gaps = b.flag();
    s.mbw = b.ue() as usize + 1;
    s.mbh = b.ue() as usize + 1;
    s.frame_mbs_only = b.flag();
    if !s.frame_mbs_only {
        b.flag();
    }
    s.direct8x8 = b.flag();
    if b.flag() {
        for k in 0..4 {
            s.crop[k] = b.ue();
        }
    }
    if b.flag() {
        // VUI
        if b.flag() && b.read(8) == 255 {
            b.read(32);
        }
        if b.flag() {
            b.flag();
        }
        if b.flag() {
            b.read(3);
            s.full_range = b.flag();
            if b.flag() {
                b.read(16);
                s.matrix = Some(b.read(8));
            }
        }
        if b.flag() {
            b.ue();
            b.ue();
        }
        if b.flag() {
            b.read(32);
            b.read(32);
            b.flag();
        }
        let nal = b.flag();
        if nal {
            hrd(&mut b);
        }
        let vcl = b.flag();
        if vcl {
            hrd(&mut b);
        }
        if nal || vcl {
            b.flag();
        }
        b.flag(); // pic_struct_present
        if b.flag() {
            b.flag();
            b.ue();
            b.ue();
            b.ue();
            b.ue();
            s.reorder = Some(b.ue());
            b.ue();
        }
    }
    if s.mbw > 512 || s.mbh > 512 {
        return Err("video too large");
    }
    Ok((id, s))
}

pub fn parse_pps(d: &[u8], spss: &[Option<Sps>]) -> Result<(usize, Pps), &'static str> {
    let mut b = Bits::new(d);
    b.read(8);
    let id = b.ue() as usize;
    let sps_id = b.ue() as usize;
    if id > 255 || sps_id > 31 {
        return Err("bad PPS");
    }
    let sps = spss[sps_id].as_ref().ok_or("PPS before its SPS")?;
    let mut p = Pps {
        sps: sps_id,
        cabac: b.flag(),
        bottom_poc: b.flag(),
        num_ref: [0; 2],
        weighted: false,
        bipred: 0,
        qp: 26,
        cqp: [0; 2],
        deblock_ctrl: false,
        constrained_intra: false,
        redundant: false,
        t8x8: false,
        scaling4: sps.scaling4,
        scaling8: sps.scaling8,
        slice_groups: 1,
    };
    p.slice_groups = b.ue() + 1;
    if p.slice_groups > 1 {
        return Err("slice groups aren't supported");
    }
    p.num_ref = [b.ue() + 1, b.ue() + 1];
    p.weighted = b.flag();
    p.bipred = b.read(2);
    p.qp = 26 + b.se();
    b.se();
    p.cqp[0] = b.se();
    p.cqp[1] = p.cqp[0];
    p.deblock_ctrl = b.flag();
    p.constrained_intra = b.flag();
    p.redundant = b.flag();
    if b.more_rbsp_data() {
        p.t8x8 = b.flag();
        if b.flag() {
            let n8 = if p.t8x8 { if sps.chroma == 3 { 6 } else { 2 } } else { 0 };
            let (mut s4, mut s8) = (p.scaling4, p.scaling8);
            scaling_matrices(&mut b, n8, &mut s4, &mut s8, Some((&sps.scaling4, &sps.scaling8)));
            p.scaling4 = s4;
            if n8 > 0 {
                p.scaling8 = s8;
            }
        }
        p.cqp[1] = b.se();
    }
    Ok((id, p))
}

// ---- slices ---------------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SliceKind {
    P,
    B,
    I,
}

#[derive(Clone, Default)]
pub(crate) struct Weights {
    pub luma_log2: u32,
    pub chroma_log2: u32,
    /// [list][ref] -> (weight, offset) for Y, Cb, Cr.
    pub w: [Vec<[(i32, i32); 3]>; 2],
}

#[derive(Clone)]
pub(crate) struct Slice {
    pub kind: SliceKind,
    pub nal_ref: u32,
    pub idr: bool,
    pub first_mb: usize,
    pub pps: usize,
    pub frame_num: u32,
    pub poc_lsb: u32,
    pub delta_bottom: i32,
    pub delta_poc: [i32; 2],
    pub direct_spatial: bool,
    pub num_ref: [usize; 2],
    pub mods: [Vec<(u32, u32)>; 2],
    pub weights: Option<Weights>,
    pub long_term_ref: bool,
    pub adaptive: bool,
    pub mmco: Vec<(u32, u32, u32)>,
    pub cabac_init: usize,
    pub qp: i32,
    pub deblock: u32,
    pub alpha: i32,
    pub beta: i32,
}

// ---- pictures -------------------------------------------------------------------------------

pub(crate) struct Pic {
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub poc: i32,
    pub frame_num: u32,
    pub frame_num_wrap: i32,
    pub short: bool,
    pub long: bool,
    pub lt_idx: u32,
    pub output: bool,
    pub id: u32,
    pub busy: bool,
    /// Motion per 4x4 block (for direct prediction and deblocking).
    pub mv: [Vec<[i16; 2]>; 2],
    pub refi: [Vec<i8>; 2],
    /// Unique id of the picture each 4x4 block refers to (-1 = none), and whether it is long-term.
    pub refid: [Vec<i32>; 2],
    pub refpoc: [Vec<i32>; 2],
    pub reflong: [Vec<bool>; 2],
    pub intra: Vec<bool>,
}

impl Pic {
    fn new(mbw: usize, mbh: usize) -> Pic {
        let n4 = mbw * mbh * 16;
        Pic {
            y: vec![0; mbw * mbh * 256],
            u: vec![128; mbw * mbh * 64],
            v: vec![128; mbw * mbh * 64],
            poc: 0,
            frame_num: 0,
            frame_num_wrap: 0,
            short: false,
            long: false,
            lt_idx: 0,
            output: false,
            id: 0,
            busy: false,
            mv: [vec![[0; 2]; n4], vec![[0; 2]; n4]],
            refi: [vec![-1; n4], vec![-1; n4]],
            refid: [vec![-1; n4], vec![-1; n4]],
            refpoc: [vec![0; n4], vec![0; n4]],
            reflong: [vec![false; n4], vec![false; n4]],
            intra: vec![false; mbw * mbh],
        }
    }
    fn is_ref(&self) -> bool {
        self.short || self.long
    }
}

/// A finished picture, in display order.
pub struct Frame {
    pub width: usize,
    pub height: usize,
    /// Planes with their strides (full macroblock-aligned size); the visible picture starts at
    /// (crop_x, crop_y).
    pub y: Vec<u8>,
    pub u: Vec<u8>,
    pub v: Vec<u8>,
    pub stride: usize,
    pub crop_x: usize,
    pub crop_y: usize,
    pub bt709: bool,
    pub full_range: bool,
    pub poc: i32,
}

impl Frame {
    /// Convert (and shrink by area averaging / nearest) to 0x00RRGGBB at w x h.
    pub fn to_rgb(&self, w: usize, h: usize) -> Vec<u32> {
        let mut out = vec![0u32; w * h];
        let cs = self.stride / 2;
        // Fixed-point YCbCr -> RGB (x1024).
        let (cr_r, cb_g, cr_g, cb_b) = if self.bt709 { (1613, 192, 479, 1900) } else { (1436, 352, 731, 1815) };
        let (ys, yo) = if self.full_range { (1024, 0) } else { (1192, 16) };
        let (cr_r, cb_g, cr_g, cb_b) = if self.full_range { (cr_r, cb_g, cr_g, cb_b) } else { (cr_r * 1167 / 1024, cb_g * 1167 / 1024, cr_g * 1167 / 1024, cb_b * 1167 / 1024) };
        for oy in 0..h {
            let sy = self.crop_y + oy * self.height / h;
            for ox in 0..w {
                let sx = self.crop_x + ox * self.width / w;
                let yv = (self.y[sy * self.stride + sx] as i32 - yo) * ys;
                let ci = (sy / 2) * cs + sx / 2;
                let cb = self.u[ci] as i32 - 128;
                let cr = self.v[ci] as i32 - 128;
                let r = (yv + cr_r * cr + 512) >> 10;
                let g = (yv - cb_g * cb - cr_g * cr + 512) >> 10;
                let b = (yv + cb_b * cb + 512) >> 10;
                out[oy * w + ox] = (clip(r) as u32) << 16 | (clip(g) as u32) << 8 | clip(b) as u32;
            }
        }
        out
    }
}

// ---- macroblocks ----------------------------------------------------------------------------

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum MbType {
    I4x4,
    I8x8,
    I16x16,
    IPcm,
    PSkip,
    P16x16,
    P16x8,
    P8x16,
    P8x8,
    P8x8Ref0,
    BSkip,
    BDirect,
    B16x16(u8),
    B16x8(u8, u8),
    B8x16(u8, u8),
    B8x8,
}

impl MbType {
    pub fn intra(self) -> bool {
        matches!(self, MbType::I4x4 | MbType::I8x8 | MbType::I16x16 | MbType::IPcm)
    }
}

/// Per macroblock state kept for the whole picture (neighbours, deblocking).
#[derive(Clone, Copy)]
pub(crate) struct MbInfo {
    /// Slice number + 1 (0: not decoded yet).
    pub slice: u32,
    pub ty: MbType,
    pub cbp: u8,
    pub qp: u8,
    pub t8x8: bool,
    pub chroma_mode: u8,
    /// Intra 4x4 / 8x8 prediction modes per 4x4 block (raster order); -1 = not NxN intra.
    pub ipred: [i8; 16],
    /// Non-zero coefficient counts: 16 luma (raster), then 4 Cb, 4 Cr (raster).
    pub nz: [u8; 24],
    /// CABAC coded_block_flag bits: 0-15 luma 4x4 (raster), 16 luma DC, 17/18 Cb/Cr DC,
    /// 19-22 Cb AC, 23-26 Cr AC.
    pub cbf: u32,
    pub mvd: [[[i16; 2]; 16]; 2],
    pub qp_nz: bool,
    pub deblock: u32,
    pub alpha: i8,
    pub beta: i8,
    /// 4x4 blocks predicted in direct mode (B_Skip / direct), bit per raster block.
    pub direct: u16,
    /// Reference indices per 4x4 block (raster), for CABAC contexts.
    pub refs: [[i8; 16]; 2],
    /// 4x4 luma blocks with coefficients (8x8 transform: the whole 8x8), for deblocking.
    pub nzmask: u16,
}

impl Default for MbInfo {
    fn default() -> MbInfo {
        MbInfo { slice: 0, ty: MbType::PSkip, cbp: 0, qp: 0, t8x8: false, chroma_mode: 0, ipred: [-1; 16], nz: [0; 24], cbf: 0, mvd: [[[0; 2]; 16]; 2], qp_nz: false, deblock: 0, alpha: 0, beta: 0, direct: 0, refs: [[-1; 16]; 2], nzmask: 0 }
    }
}

/// One parsed macroblock.
pub(crate) struct Mb {
    pub ty: MbType,
    pub i16_mode: u8,
    pub cbp: u8,
    pub t8x8: bool,
    /// Intra NxN: per block (z order for 4x4, 0..3 for 8x8): None = predicted mode,
    /// Some(rem) = rem_intra_pred_mode.
    pub ipred: [Option<u8>; 16],
    pub chroma_mode: u8,
    pub qp_delta: i32,
    pub sub: [u8; 4],
    /// ref_idx and mvd per 4x4 block (raster), filled for each partition.
    pub refs: [[i8; 16]; 2],
    pub mvd: [[[i16; 2]; 16]; 2],
    /// Coefficients in raster order: 16 4x4 luma blocks (raster block order) or 4 8x8.
    pub luma: [[i32; 16]; 16],
    pub luma8: [[i32; 64]; 4],
    pub dc: [i32; 16],
    pub cdc: [[i32; 4]; 2],
    pub cac: [[i32; 16]; 8],
    pub nz: [u8; 24],
    pub cbf: u32,
    pub pcm: Vec<u8>,
}

impl Mb {
    pub fn new(ty: MbType) -> Mb {
        Mb {
            ty,
            i16_mode: 0,
            cbp: 0,
            t8x8: false,
            ipred: [None; 16],
            chroma_mode: 0,
            qp_delta: 0,
            sub: [0; 4],
            refs: [[-1; 16]; 2],
            mvd: [[[0; 2]; 16]; 2],
            luma: [[0; 16]; 16],
            luma8: [[0; 64]; 4],
            dc: [0; 16],
            cdc: [[0; 4]; 2],
            cac: [[0; 16]; 8],
            nz: [0; 24],
            cbf: 0,
            pcm: Vec::new(),
        }
    }
    /// Start a new macroblock. Coefficient arrays are left as they are: only blocks the
    /// coded_block_pattern says are coded get written (and read).
    pub fn reset(&mut self, ty: MbType) {
        self.ty = ty;
        self.i16_mode = 0;
        self.cbp = 0;
        self.t8x8 = false;
        self.ipred = [None; 16];
        self.chroma_mode = 0;
        self.qp_delta = 0;
        self.sub = [0; 4];
        self.refs = [[-1; 16]; 2];
        self.mvd = [[[0; 2]; 16]; 2];
        self.nz = [0; 24];
        self.cbf = 0;
    }

}

/// Partitions of an inter macroblock: (x, y, w, h) in 4x4 units, prediction lists
/// (1 = L0, 2 = L1, 3 = both), and whether it's direct.
pub(crate) fn partitions(ty: MbType, sub: &[u8; 4], b_slice: bool) -> Vec<(usize, usize, usize, usize, u8, bool)> {
    let mut v = Vec::new();
    match ty {
        MbType::P16x16 => v.push((0, 0, 4, 4, 1, false)),
        MbType::P16x8 => {
            v.push((0, 0, 4, 2, 1, false));
            v.push((0, 2, 4, 2, 1, false));
        }
        MbType::P8x16 => {
            v.push((0, 0, 2, 4, 1, false));
            v.push((2, 0, 2, 4, 1, false));
        }
        MbType::B16x16(p) => v.push((0, 0, 4, 4, p, false)),
        MbType::B16x8(a, b) => {
            v.push((0, 0, 4, 2, a, false));
            v.push((0, 2, 4, 2, b, false));
        }
        MbType::B8x16(a, b) => {
            v.push((0, 0, 2, 4, a, false));
            v.push((2, 0, 2, 4, b, false));
        }
        MbType::P8x8 | MbType::P8x8Ref0 | MbType::B8x8 => {
            for (k, &s) in sub.iter().enumerate() {
                let (x0, y0) = ((k & 1) * 2, (k >> 1) * 2);
                let (pred, shape, direct) = if b_slice { b_sub(s) } else { (1, s, false) };
                match shape {
                    0 => v.push((x0, y0, 2, 2, pred, direct)),
                    1 => {
                        v.push((x0, y0, 2, 1, pred, direct));
                        v.push((x0, y0 + 1, 2, 1, pred, direct));
                    }
                    2 => {
                        v.push((x0, y0, 1, 2, pred, direct));
                        v.push((x0 + 1, y0, 1, 2, pred, direct));
                    }
                    _ => {
                        for j in 0..4 {
                            v.push((x0 + (j & 1), y0 + (j >> 1), 1, 1, pred, direct));
                        }
                    }
                }
            }
        }
        _ => {}
    }
    v
}

/// B sub-macroblock type -> (lists, shape 0=8x8 1=8x4 2=4x8 3=4x4, direct).
pub(crate) fn b_sub(s: u8) -> (u8, u8, bool) {
    match s {
        0 => (3, 0, true),
        1 => (1, 0, false),
        2 => (2, 0, false),
        3 => (3, 0, false),
        4 => (1, 1, false),
        5 => (1, 2, false),
        6 => (2, 1, false),
        7 => (2, 2, false),
        8 => (3, 1, false),
        9 => (3, 2, false),
        10 => (1, 3, false),
        11 => (2, 3, false),
        _ => (3, 3, false),
    }
}

/// B mb_type 1..21 -> MbType.
pub(crate) fn b_type(t: u32) -> MbType {
    const P: [(u8, u8); 18] = [(1, 1), (1, 1), (2, 2), (2, 2), (1, 2), (1, 2), (2, 1), (2, 1), (1, 3), (1, 3), (2, 3), (2, 3), (3, 1), (3, 1), (3, 2), (3, 2), (3, 3), (3, 3)];
    match t {
        0 => MbType::BDirect,
        1 => MbType::B16x16(1),
        2 => MbType::B16x16(2),
        3 => MbType::B16x16(3),
        4..=21 => {
            let (a, b) = P[(t - 4) as usize];
            if t % 2 == 0 {
                MbType::B16x8(a, b)
            } else {
                MbType::B8x16(a, b)
            }
        }
        _ => MbType::B8x8,
    }
}

// ---- the decoder ----------------------------------------------------------------------------

/// What the entropy decoders need to know about the slice and the neighbours.
pub(crate) struct Ctx<'a> {
    pub info: &'a [MbInfo],
    pub mbw: usize,
    pub mbx: usize,
    pub mby: usize,
    pub slice: u32,
    pub kind: SliceKind,
    pub num_ref: [usize; 2],
    pub t8x8_mode: bool,
    pub direct8x8: bool,
    pub constrained_intra: bool,
}

impl Ctx<'_> {
    pub fn left(&self) -> Option<&MbInfo> {
        if self.mbx == 0 {
            return None;
        }
        let m = &self.info[self.mby * self.mbw + self.mbx - 1];
        if m.slice == self.slice {
            Some(m)
        } else {
            None
        }
    }
    pub fn top(&self) -> Option<&MbInfo> {
        if self.mby == 0 {
            return None;
        }
        let m = &self.info[(self.mby - 1) * self.mbw + self.mbx];
        if m.slice == self.slice {
            Some(m)
        } else {
            None
        }
    }
    /// The neighbour of 4x4 block (x, y) (raster within the MB) to the left / above:
    /// None = outside the picture/slice, Some((None, x', y')) = in the current MB,
    /// Some((Some(info), x', y')) = in a neighbouring MB.
    pub fn left_blk(&self, x: usize, y: usize) -> Option<(Option<&MbInfo>, usize, usize)> {
        if x > 0 {
            Some((None, x - 1, y))
        } else {
            self.left().map(|m| (Some(m), 3, y))
        }
    }
    pub fn top_blk(&self, x: usize, y: usize) -> Option<(Option<&MbInfo>, usize, usize)> {
        if y > 0 {
            Some((None, x, y - 1))
        } else {
            self.top().map(|m| (Some(m), x, 3))
        }
    }
}

pub struct Decoder {
    sps: Vec<Option<Sps>>,
    pps: Vec<Option<Pps>>,
    nal_len: usize,
    pics: Vec<Pic>,
    cur: Option<usize>,
    cur_slice: Option<Slice>,
    first_slice: Option<Slice>,
    info: Vec<MbInfo>,
    mbw: usize,
    mbh: usize,
    next_id: u32,
    slice_num: u32,
    // Picture order count state.
    prev_poc_msb: i32,
    prev_poc_lsb: i32,
    prev_frame_num_offset: i32,
    prev_frame_num: u32,
    prev_ref_frame_num: u32,
    frame_num_offset: i32,
    max_lt_idx: Option<u32>,
    ref_lists: [Vec<usize>; 2],
    /// (id, poc, long-term) of each reference list entry.
    list_info: [Vec<(i32, i32, bool)>; 2],
    out: VecDeque<Frame>,
    active_sps: usize,
    active_pps: usize,
    pub(crate) cabac: super::h264cabac::Cabac,
    pub errors: u32,
    /// Test statistics: I, P, B slices; 8x8-transform, intra 8x8, direct/skip-B macroblocks; CABAC slices; weighted slices.
    pub stats: [u32; 8],
    ls_cache: Option<(usize, i32, LevelScale)>,
    /// Skip the deblocking filter (only for measuring speed).
    pub no_deblock: bool,
    mbuf: Option<alloc::boxed::Box<Mb>>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

impl Decoder {
    pub fn new() -> Decoder {
        Decoder {
            sps: vec![None; 32],
            pps: vec![None; 256],
            nal_len: 4,
            pics: Vec::new(),
            cur: None,
            cur_slice: None,
            first_slice: None,
            info: Vec::new(),
            mbw: 0,
            mbh: 0,
            next_id: 1,
            slice_num: 0,
            prev_poc_msb: 0,
            prev_poc_lsb: 0,
            prev_frame_num_offset: 0,
            prev_frame_num: 0,
            prev_ref_frame_num: 0,
            frame_num_offset: 0,
            max_lt_idx: None,
            ref_lists: [Vec::new(), Vec::new()],
            list_info: [Vec::new(), Vec::new()],
            out: VecDeque::new(),
            active_sps: 0,
            active_pps: 0,
            cabac: super::h264cabac::Cabac::new(),
            errors: 0,
            stats: [0; 8],
            ls_cache: None,
            no_deblock: false,
            mbuf: None,
        }
    }

    /// Read the avcC record from MP4 (parameter sets and NAL length size).
    pub fn configure(&mut self, avcc: &[u8]) -> Result<(), &'static str> {
        if avcc.len() < 7 {
            return Err("bad avcC");
        }
        self.nal_len = (avcc[4] & 3) as usize + 1;
        let mut p = 5;
        let nsps = (avcc[p] & 31) as usize;
        p += 1;
        for _ in 0..nsps {
            let n = u16::from_be_bytes([avcc[p], avcc[p + 1]]) as usize;
            self.nal(&avcc[p + 2..p + 2 + n])?;
            p += 2 + n;
        }
        let npps = *avcc.get(p).ok_or("bad avcC")? as usize;
        p += 1;
        for _ in 0..npps {
            let n = u16::from_be_bytes([avcc[p], avcc[p + 1]]) as usize;
            self.nal(&avcc[p + 2..p + 2 + n])?;
            p += 2 + n;
        }
        Ok(())
    }

    pub fn size(&self) -> Option<(usize, usize)> {
        let s = self.sps.iter().flatten().next()?;
        Some((s.mbw * 16 - (s.crop[0] + s.crop[1]) as usize * 2, s.mbh * 16 - (s.crop[2] + s.crop[3]) as usize * 2))
    }

    /// Decode one MP4 sample (length-prefixed NAL units).
    pub fn decode_sample(&mut self, data: &[u8]) -> Result<(), &'static str> {
        let mut p = 0;
        while p + self.nal_len <= data.len() {
            let mut n = 0usize;
            for k in 0..self.nal_len {
                n = n << 8 | data[p + k] as usize;
            }
            p += self.nal_len;
            if n == 0 || p + n > data.len() {
                break;
            }
            if let Err(e) = self.nal(&data[p..p + n]) {
                self.errors += 1;
                if self.errors < 20 {
                    kprintln!("h264: {}", e);
                }
            }
            p += n;
        }
        // An MP4 sample is a whole picture.
        self.finish_picture();
        Ok(())
    }

    /// Decode an Annex B stream chunk (start codes), for tests.
    pub fn decode_annexb(&mut self, data: &[u8]) {
        let mut starts = Vec::new();
        let mut i = 0;
        while i + 3 <= data.len() {
            if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
                starts.push(i + 3);
                i += 3;
            } else {
                i += 1;
            }
        }
        for (k, &s) in starts.iter().enumerate() {
            let mut e = if k + 1 < starts.len() { starts[k + 1] - 3 } else { data.len() };
            while e > s && data[e - 1] == 0 {
                e -= 1;
            }
            let _ = self.nal(&data[s..e]);
        }
    }

    /// No more input: everything left comes out.
    pub fn flush(&mut self) {
        self.finish_picture();
        self.bump(0);
    }

    /// Forget everything (before seeking to a key frame).
    pub fn reset(&mut self) {
        self.finish_picture();
        for p in self.pics.iter_mut() {
            p.short = false;
            p.long = false;
            p.output = false;
        }
        self.out.clear();
    }

    pub fn pop(&mut self) -> Option<Frame> {
        self.out.pop_front()
    }

    fn nal(&mut self, raw: &[u8]) -> Result<(), &'static str> {
        if raw.is_empty() {
            return Ok(());
        }
        let ty = raw[0] & 31;
        let nal_ref = (raw[0] >> 5) as u32 & 3;
        match ty {
            7 => {
                let d = unescape(raw);
                let (id, s) = parse_sps(&d)?;
                self.sps[id] = Some(s);
            }
            8 => {
                let d = unescape(raw);
                let (id, p) = parse_pps(&d, &self.sps)?;
                self.ls_cache = None;
                self.pps[id] = Some(p);
            }
            1 | 5 => {
                let d = unescape(raw);
                self.slice(&d, ty == 5, nal_ref)?;
            }
            _ => {}
        }
        Ok(())
    }

    fn slice_header(&self, b: &mut Bits, idr: bool, nal_ref: u32) -> Result<Slice, &'static str> {
        b.read(8);
        let first_mb = b.ue() as usize;
        let st = b.ue() % 5;
        let kind = match st {
            0 => SliceKind::P,
            1 => SliceKind::B,
            2 => SliceKind::I,
            _ => return Err("SP/SI slices aren't supported"),
        };
        let pps_id = b.ue() as usize;
        let pps = self.pps.get(pps_id).and_then(|p| p.as_ref()).ok_or("slice without PPS")?;
        let sps = self.sps[pps.sps].as_ref().ok_or("slice without SPS")?;
        if !sps.frame_mbs_only {
            return Err("interlaced video isn't supported yet");
        }
        if sps.chroma != 1 || sps.bit_depth != 8 {
            return Err("only 8-bit 4:2:0 video is supported");
        }
        let frame_num = b.read(sps.log2_max_frame_num);
        let mut s = Slice {
            kind,
            nal_ref,
            idr,
            first_mb,
            pps: pps_id,
            frame_num,
            poc_lsb: 0,
            delta_bottom: 0,
            delta_poc: [0; 2],
            direct_spatial: false,
            num_ref: [pps.num_ref[0] as usize, pps.num_ref[1] as usize],
            mods: [Vec::new(), Vec::new()],
            weights: None,
            long_term_ref: false,
            adaptive: false,
            mmco: Vec::new(),
            cabac_init: 0,
            qp: 0,
            deblock: 0,
            alpha: 0,
            beta: 0,
        };
        if idr {
            b.ue();
        }
        if sps.poc_type == 0 {
            s.poc_lsb = b.read(sps.log2_max_poc_lsb);
            if pps.bottom_poc {
                s.delta_bottom = b.se();
            }
        }
        if sps.poc_type == 1 && !sps.delta_always_zero {
            s.delta_poc[0] = b.se();
            if pps.bottom_poc {
                s.delta_poc[1] = b.se();
            }
        }
        if pps.redundant {
            b.ue();
        }
        if kind == SliceKind::B {
            s.direct_spatial = b.flag();
        }
        if kind != SliceKind::I && b.flag() {
            s.num_ref[0] = b.ue() as usize + 1;
            if kind == SliceKind::B {
                s.num_ref[1] = b.ue() as usize + 1;
            }
        }
        if s.num_ref[0] > 32 || s.num_ref[1] > 32 {
            return Err("bad reference count");
        }
        if kind != SliceKind::I {
            for l in 0..if kind == SliceKind::B { 2 } else { 1 } {
                if b.flag() {
                    loop {
                        let idc = b.ue();
                        if idc == 3 || b.left() <= 0 {
                            break;
                        }
                        let v = b.ue();
                        s.mods[l].push((idc, v));
                        if s.mods[l].len() > 64 {
                            break;
                        }
                    }
                }
            }
        }
        if (pps.weighted && kind == SliceKind::P) || (pps.bipred == 1 && kind == SliceKind::B) {
            let mut w = Weights { luma_log2: b.ue(), chroma_log2: b.ue(), w: [Vec::new(), Vec::new()] };
            for l in 0..if kind == SliceKind::B { 2 } else { 1 } {
                for _ in 0..s.num_ref[l] {
                    let mut e = [(1 << w.luma_log2, 0), (1 << w.chroma_log2, 0), (1 << w.chroma_log2, 0)];
                    if b.flag() {
                        e[0] = (b.se(), b.se());
                    }
                    if b.flag() {
                        e[1] = (b.se(), b.se());
                        e[2] = (b.se(), b.se());
                    }
                    w.w[l].push(e);
                }
            }
            s.weights = Some(w);
        }
        if nal_ref != 0 {
            if idr {
                b.flag();
                s.long_term_ref = b.flag();
            } else {
                s.adaptive = b.flag();
                if s.adaptive {
                    loop {
                        let op = b.ue();
                        if op == 0 || b.left() <= 0 || s.mmco.len() > 64 {
                            break;
                        }
                        let (mut a, mut c) = (0, 0);
                        if op == 1 || op == 3 {
                            a = b.ue();
                        }
                        if op == 2 {
                            a = b.ue();
                        }
                        if op == 3 || op == 6 {
                            c = b.ue();
                        }
                        if op == 4 {
                            a = b.ue();
                        }
                        s.mmco.push((op, a, c));
                    }
                }
            }
        }
        if pps.cabac && kind != SliceKind::I {
            s.cabac_init = (b.ue() as usize).min(2);
        }
        s.qp = pps.qp + b.se();
        if pps.deblock_ctrl {
            s.deblock = b.ue();
            if s.deblock != 1 {
                s.alpha = b.se() * 2;
                s.beta = b.se() * 2;
            }
        }
        Ok(s)
    }

    fn slice(&mut self, d: &[u8], idr: bool, nal_ref: u32) -> Result<(), &'static str> {
        let mut b = Bits::new(d);
        let s = self.slice_header(&mut b, idr, nal_ref)?;
        let pps = self.pps[s.pps].clone().unwrap();
        let sps = self.sps[pps.sps].clone().unwrap();
        // A new picture starts with its first macroblock.
        if s.first_mb == 0 || self.cur.is_none() {
            self.finish_picture();
            self.start_picture(&s, &sps)?;
        }
        self.active_pps = s.pps;
        self.active_sps = pps.sps;
        self.slice_num += 1;
        self.build_lists(&s, &sps);
        self.cur_slice = Some(s.clone());
        self.stats[s.kind as usize] += 1;
        self.stats[6] += pps.cabac as u32;
        self.stats[7] += s.weights.is_some() as u32 + (s.kind == SliceKind::B && pps.bipred == 2) as u32;
        self.decode_slice_data(&mut b, &s, &sps, &pps)
    }

    fn alloc_pic(&mut self) -> usize {
        let (mbw, mbh) = (self.mbw, self.mbh);
        if let Some(i) = self.pics.iter().position(|p| !p.is_ref() && !p.output && !p.busy && p.y.len() == mbw * mbh * 256) {
            return i;
        }
        // Drop pictures of another size.
        self.pics.retain(|p| p.is_ref() || p.output || p.busy);
        self.pics.push(Pic::new(mbw, mbh));
        self.pics.len() - 1
    }

    fn start_picture(&mut self, s: &Slice, sps: &Sps) -> Result<(), &'static str> {
        if sps.mbw != self.mbw || sps.mbh != self.mbh {
            self.flush();
            self.pics.clear();
            self.mbw = sps.mbw;
            self.mbh = sps.mbh;
        }
        let max_frame_num = 1u32 << sps.log2_max_frame_num;
        if s.idr {
            // Output everything still waiting, then forget all references.
            self.bump(0);
            for p in self.pics.iter_mut() {
                p.short = false;
                p.long = false;
            }
            self.prev_ref_frame_num = 0;
        } else if s.frame_num != self.prev_ref_frame_num && s.frame_num != (self.prev_ref_frame_num + 1) % max_frame_num {
            // Frames missing: fill in with copies of the last picture so references line up.
            let mut fnum = (self.prev_ref_frame_num + 1) % max_frame_num;
            let mut guard = 0;
            while fnum != s.frame_num && guard < 16 {
                guard += 1;
                let i = self.alloc_pic();
                if let Some(last) = self.pics.iter().enumerate().filter(|(_, p)| p.short).max_by_key(|(_, p)| p.frame_num_wrap).map(|(k, _)| k) {
                    let (y, u, v) = (self.pics[last].y.clone(), self.pics[last].u.clone(), self.pics[last].v.clone());
                    let p = &mut self.pics[i];
                    p.y = y;
                    p.u = u;
                    p.v = v;
                }
                self.sliding_window(sps);
                let id = self.next_id;
                self.next_id += 1;
                let p = &mut self.pics[i];
                p.frame_num = fnum;
                p.short = true;
                p.long = false;
                p.output = false;
                p.id = id;
                p.poc = i32::MIN / 2;
                self.prev_ref_frame_num = fnum;
                fnum = (fnum + 1) % max_frame_num;
            }
        }
        let i = self.alloc_pic();
        let poc = self.compute_poc(s, sps);
        let id = self.next_id;
        self.next_id += 1;
        {
            let p = &mut self.pics[i];
            p.poc = poc;
            p.frame_num = s.frame_num;
            p.short = false;
            p.long = false;
            p.output = true;
            p.busy = true;
            p.id = id;
            for v in p.intra.iter_mut() {
                *v = false;
            }
            for l in 0..2 {
                p.refi[l].fill(-1);
                p.refid[l].fill(-1);
                p.mv[l].fill([0, 0]);
            }
        }
        self.cur = Some(i);
        self.first_slice = Some(s.clone());
        self.info.clear();
        self.info.resize(self.mbw * self.mbh, MbInfo::default());
        self.slice_num = 0;
        Ok(())
    }

    fn compute_poc(&mut self, s: &Slice, sps: &Sps) -> i32 {
        let max_frame_num = 1i32 << sps.log2_max_frame_num;
        match sps.poc_type {
            0 => {
                if s.idr {
                    self.prev_poc_msb = 0;
                    self.prev_poc_lsb = 0;
                }
                let max = 1i32 << sps.log2_max_poc_lsb;
                let lsb = s.poc_lsb as i32;
                let msb = if lsb < self.prev_poc_lsb && self.prev_poc_lsb - lsb >= max / 2 {
                    self.prev_poc_msb + max
                } else if lsb > self.prev_poc_lsb && lsb - self.prev_poc_lsb > max / 2 {
                    self.prev_poc_msb - max
                } else {
                    self.prev_poc_msb
                };
                let top = msb + lsb;
                let bottom = top + s.delta_bottom;
                if s.nal_ref != 0 {
                    self.prev_poc_msb = msb;
                    self.prev_poc_lsb = lsb;
                }
                top.min(bottom)
            }
            _ => {
                let fno = if s.idr {
                    0
                } else if self.prev_frame_num > s.frame_num {
                    self.prev_frame_num_offset + max_frame_num
                } else {
                    self.prev_frame_num_offset
                };
                self.frame_num_offset = fno;
                self.prev_frame_num_offset = fno;
                self.prev_frame_num = s.frame_num;
                if sps.poc_type == 2 {
                    let t = if s.idr {
                        0
                    } else if s.nal_ref == 0 {
                        2 * (fno + s.frame_num as i32) - 1
                    } else {
                        2 * (fno + s.frame_num as i32)
                    };
                    return t;
                }
                let n = sps.off_ref.len() as i32;
                let mut abs = if n != 0 { fno + s.frame_num as i32 } else { 0 };
                if s.nal_ref == 0 && abs > 0 {
                    abs -= 1;
                }
                let mut expected = 0;
                if abs > 0 {
                    let cycle = (abs - 1) / n;
                    let inc = (abs - 1) % n;
                    let delta: i32 = sps.off_ref.iter().sum();
                    expected = cycle * delta + sps.off_ref[..=inc as usize].iter().sum::<i32>();
                }
                if s.nal_ref == 0 {
                    expected += sps.off_non_ref;
                }
                let top = expected + s.delta_poc[0];
                let bottom = top + sps.off_t2b + s.delta_poc[1];
                top.min(bottom)
            }
        }
    }

    fn sliding_window(&mut self, sps: &Sps) {
        let max = sps.max_refs.max(1) as usize;
        let n = self.pics.iter().filter(|p| p.is_ref() && !p.busy).count();
        if n >= max {
            if let Some(k) = self.pics.iter().enumerate().filter(|(_, p)| p.short && !p.busy).min_by_key(|(_, p)| p.frame_num_wrap).map(|(k, _)| k) {
                self.pics[k].short = false;
            }
        }
    }

    /// Picture numbers relative to the current frame_num.
    fn update_wraps(&mut self, frame_num: u32, max_frame_num: u32) {
        for p in self.pics.iter_mut() {
            if p.short {
                p.frame_num_wrap = if p.frame_num > frame_num { p.frame_num as i32 - max_frame_num as i32 } else { p.frame_num as i32 };
            }
        }
    }

    fn build_lists(&mut self, s: &Slice, sps: &Sps) {
        let max_frame_num = 1u32 << sps.log2_max_frame_num;
        self.update_wraps(s.frame_num, max_frame_num);
        let cur = self.cur.unwrap();
        let cur_poc = self.pics[cur].poc;
        let refs: Vec<usize> = (0..self.pics.len()).filter(|&i| i != cur && self.pics[i].is_ref()).collect();
        let mut shorts: Vec<usize> = refs.iter().copied().filter(|&i| self.pics[i].short).collect();
        let mut longs: Vec<usize> = refs.iter().copied().filter(|&i| self.pics[i].long).collect();
        longs.sort_by_key(|&i| self.pics[i].lt_idx);
        self.ref_lists = [Vec::new(), Vec::new()];
        self.list_info = [Vec::new(), Vec::new()];
        match s.kind {
            SliceKind::I => return,
            SliceKind::P => {
                shorts.sort_by_key(|&i| -self.pics[i].frame_num_wrap);
                self.ref_lists[0] = shorts.into_iter().chain(longs).collect();
            }
            SliceKind::B => {
                let mut before: Vec<usize> = shorts.iter().copied().filter(|&i| self.pics[i].poc < cur_poc).collect();
                let mut after: Vec<usize> = shorts.iter().copied().filter(|&i| self.pics[i].poc > cur_poc).collect();
                before.sort_by_key(|&i| -self.pics[i].poc);
                after.sort_by_key(|&i| self.pics[i].poc);
                self.ref_lists[0] = before.iter().chain(after.iter()).chain(longs.iter()).copied().collect();
                self.ref_lists[1] = after.iter().chain(before.iter()).chain(longs.iter()).copied().collect();
                if self.ref_lists[1].len() > 1 && self.ref_lists[0] == self.ref_lists[1] {
                    self.ref_lists[1].swap(0, 1);
                }
            }
        }
        for l in 0..2 {
            let n = s.num_ref[l];
            let mut list = core::mem::take(&mut self.ref_lists[l]);
            list.truncate(n);
            // Modification.
            let mut pred = s.frame_num as i32;
            let max = max_frame_num as i32;
            for (k, &(idc, v)) in s.mods[l].iter().enumerate() {
                let target = match idc {
                    0 | 1 => {
                        let d = v as i32 + 1;
                        let no_wrap = if idc == 0 {
                            if pred - d < 0 {
                                pred - d + max
                            } else {
                                pred - d
                            }
                        } else if pred + d >= max {
                            pred + d - max
                        } else {
                            pred + d
                        };
                        pred = no_wrap;
                        let num = if no_wrap > s.frame_num as i32 { no_wrap - max } else { no_wrap };
                        self.pics.iter().position(|p| p.short && p.frame_num_wrap == num && !p.busy)
                    }
                    2 => self.pics.iter().position(|p| p.long && p.lt_idx == v && !p.busy),
                    _ => None,
                };
                if let Some(t) = target {
                    if k > list.len() {
                        continue;
                    }
                    list.insert(k, t);
                    // Remove the later duplicate.
                    let mut j = k + 1;
                    while j < list.len() {
                        if list[j] == t {
                            list.remove(j);
                        } else {
                            j += 1;
                        }
                    }
                }
            }
            list.truncate(n);
            // Missing entries: use the first (concealment).
            while list.len() < n {
                match list.first().copied().or_else(|| refs.first().copied()) {
                    Some(x) => list.push(x),
                    None => break,
                }
            }
            self.list_info[l] = list.iter().map(|&pi| (self.pics[pi].id as i32, self.pics[pi].poc, self.pics[pi].long)).collect();
            self.ref_lists[l] = list;
        }
    }

    /// Output pictures (in POC order) while more than `keep` are waiting.
    fn bump(&mut self, keep: usize) {
        loop {
            let waiting: Vec<usize> = (0..self.pics.len()).filter(|&i| self.pics[i].output && !self.pics[i].busy).collect();
            if waiting.len() <= keep {
                break;
            }
            let i = *waiting.iter().min_by_key(|&&i| self.pics[i].poc).unwrap();
            self.pics[i].output = false;
            let sps = self.sps[self.active_sps].as_ref();
            let (crop, bt709, full) = match sps {
                // Unlabelled colours: BT.601, as Windows does.
                Some(s) => (s.crop, s.matrix == Some(1), s.full_range),
                None => ([0; 4], true, false),
            };
            let p = &self.pics[i];
            let w = self.mbw * 16;
            self.out.push_back(Frame {
                width: w - (crop[0] + crop[1]) as usize * 2,
                height: self.mbh * 16 - (crop[2] + crop[3]) as usize * 2,
                y: p.y.clone(),
                u: p.u.clone(),
                v: p.v.clone(),
                stride: w,
                crop_x: crop[0] as usize * 2,
                crop_y: crop[2] as usize * 2,
                bt709,
                full_range: full,
                poc: p.poc,
            });
        }
    }

    fn finish_picture(&mut self) {
        let Some(cur) = self.cur.take() else { return };
        let s = self.first_slice.take().unwrap();
        let sps = self.sps[self.active_sps].clone().unwrap();
        let pps = self.pps[self.active_pps].clone().unwrap();
        if !self.no_deblock {
            let t0 = now();
            self.deblock(cur, &pps);
            add_time(4, t0);
        }
        // Reference marking.
        if s.nal_ref != 0 {
            let max_frame_num = 1u32 << sps.log2_max_frame_num;
            self.update_wraps(s.frame_num, max_frame_num);
            let mut mmco5 = false;
            if s.idr {
                if s.long_term_ref {
                    self.pics[cur].long = true;
                    self.pics[cur].lt_idx = 0;
                    self.max_lt_idx = Some(0);
                } else {
                    self.pics[cur].short = true;
                    self.max_lt_idx = None;
                }
            } else {
                let mut made_long = false;
                if s.adaptive {
                    for &(op, a, c) in &s.mmco {
                        let pic_num = |d: u32| s.frame_num as i32 - (d as i32 + 1);
                        match op {
                            1 => {
                                let n = pic_num(a);
                                if let Some(p) = self.pics.iter_mut().find(|p| p.short && !p.busy && p.frame_num_wrap == n) {
                                    p.short = false;
                                }
                            }
                            2 => {
                                if let Some(p) = self.pics.iter_mut().find(|p| p.long && !p.busy && p.lt_idx == a) {
                                    p.long = false;
                                }
                            }
                            3 => {
                                let n = pic_num(a);
                                for p in self.pics.iter_mut().filter(|p| p.long && p.lt_idx == c) {
                                    p.long = false;
                                }
                                if let Some(p) = self.pics.iter_mut().find(|p| p.short && !p.busy && p.frame_num_wrap == n) {
                                    p.short = false;
                                    p.long = true;
                                    p.lt_idx = c;
                                }
                            }
                            4 => {
                                self.max_lt_idx = if a == 0 { None } else { Some(a - 1) };
                                for p in self.pics.iter_mut().filter(|p| p.long) {
                                    if self.max_lt_idx.is_none_or(|m| p.lt_idx > m) {
                                        p.long = false;
                                    }
                                }
                            }
                            5 => {
                                for p in self.pics.iter_mut().filter(|p| !p.busy) {
                                    p.short = false;
                                    p.long = false;
                                }
                                self.max_lt_idx = None;
                                mmco5 = true;
                            }
                            6 => {
                                for p in self.pics.iter_mut().filter(|p| p.long && p.lt_idx == c) {
                                    p.long = false;
                                }
                                self.pics[cur].long = true;
                                self.pics[cur].lt_idx = c;
                                made_long = true;
                            }
                            _ => {}
                        }
                    }
                } else {
                    self.sliding_window(&sps);
                }
                if !made_long {
                    self.pics[cur].short = true;
                }
            }
            self.pics[cur].frame_num_wrap = s.frame_num as i32;
            self.prev_ref_frame_num = if mmco5 { 0 } else { s.frame_num };
            if mmco5 {
                // Pictures before this one all come out first; this one counts from 0.
                self.pics[cur].busy = false;
                let poc = self.pics[cur].poc;
                self.pics[cur].output = false;
                self.bump(0);
                self.pics[cur].output = true;
                self.pics[cur].poc = 0;
                self.prev_poc_msb = 0;
                self.prev_poc_lsb = 0;
                let _ = poc;
                self.pics[cur].frame_num = 0;
                self.prev_frame_num_offset = 0;
                self.prev_frame_num = 0;
            }
        } else if sps.poc_type != 0 {
            self.prev_frame_num = s.frame_num;
        }
        self.pics[cur].busy = false;
        let keep = sps.reorder.map(|r| r as usize).unwrap_or(match sps.profile {
            66 => 0,
            _ => (sps.max_refs as usize).clamp(1, 16),
        });
        let t0 = now();
        self.bump(keep);
        add_time(5, t0);
    }

    // ---- slice data ----------------------------------------------------------------------

    fn decode_slice_data(&mut self, b: &mut Bits, s: &Slice, sps: &Sps, pps: &Pps) -> Result<(), &'static str> {
        let total = self.mbw * self.mbh;
        let mut addr = s.first_mb;
        let mut qp = s.qp;
        let mut prev_qp_nz = false;
        if pps.cabac {
            b.align();
            self.cabac.init_slice(s.kind, s.cabac_init, s.qp);
            self.cabac.start(b);
        }
        let mut skip_run: i64 = -1;
        while addr < total {
            let (mbx, mby) = (addr % self.mbw, addr / self.mbw);
            let ctx = Ctx {
                info: &self.info,
                mbw: self.mbw,
                mbx,
                mby,
                slice: self.slice_num,
                kind: s.kind,
                num_ref: s.num_ref,
                t8x8_mode: pps.t8x8,
                direct8x8: sps.direct8x8,
                constrained_intra: pps.constrained_intra,
            };
            let t0 = now();
            let mut mbuf = self.mbuf.take().unwrap_or_else(|| alloc::boxed::Box::new(Mb::new(MbType::PSkip)));
            let mb = &mut *mbuf;
            let parsed = if s.kind != SliceKind::I {
                let skipped = if pps.cabac {
                    self.cabac.skip_flag(b, &ctx)
                } else {
                    if skip_run < 0 {
                        skip_run = b.ue() as i64;
                    }
                    if skip_run > 0 {
                        skip_run -= 1;
                        true
                    } else {
                        skip_run = -1;
                        false
                    }
                };
                if skipped {
                    mb.reset(if s.kind == SliceKind::P { MbType::PSkip } else { MbType::BSkip });
                    Ok(())
                } else if pps.cabac {
                    self.cabac.parse_mb(b, &ctx, prev_qp_nz, mb)
                } else {
                    super::h264cavlc::parse_mb(b, &ctx, mb)
                }
            } else if pps.cabac {
                self.cabac.parse_mb(b, &ctx, prev_qp_nz, mb)
            } else {
                super::h264cavlc::parse_mb(b, &ctx, mb)
            };
            if let Err(e) = parsed {
                self.mbuf = Some(mbuf);
                return Err(e);
            }
            if mb.ty == MbType::IPcm && pps.cabac {
                // PCM samples were read raw; restart the arithmetic decoder.
                self.cabac.start(b);
            }
            add_time(0, t0);
            qp = (qp + mb.qp_delta + 52) % 52;
            prev_qp_nz = mb.qp_delta != 0;
            self.decode_mb(mb, addr, qp, s, pps);
            self.mbuf = Some(mbuf);
            addr += 1;
            // End of slice?
            if pps.cabac {
                if self.cabac.terminate(b) {
                    break;
                }
            } else if skip_run <= 0 && !b.more_rbsp_data() {
                break;
            }
            if b.left() < -64 {
                return Err("slice data ran out");
            }
        }
        Ok(())
    }

    // ---- reconstruction --------------------------------------------------------------------

    fn decode_mb(&mut self, mb: &Mb, addr: usize, qp: i32, s: &Slice, pps: &Pps) {
        let (mbx, mby) = (addr % self.mbw, addr / self.mbw);
        let cur = self.cur.unwrap();
        self.stats[3] += mb.t8x8 as u32;
        self.stats[4] += (mb.ty == MbType::I8x8) as u32;
        self.stats[5] += matches!(mb.ty, MbType::BSkip | MbType::BDirect) as u32;
        let mut info = MbInfo { slice: self.slice_num, ty: mb.ty, cbp: mb.cbp, qp: qp as u8, t8x8: mb.t8x8, chroma_mode: mb.chroma_mode, nz: mb.nz, cbf: mb.cbf, mvd: mb.mvd, qp_nz: mb.qp_delta != 0, deblock: s.deblock, alpha: s.alpha as i8, beta: s.beta as i8, refs: mb.refs, ..MbInfo::default() };
        for k in 0..16 {
            let (x, y) = (k % 4, k / 4);
            let any = if mb.t8x8 { (0..4).any(|j| mb.nz[((y / 2) * 2 + j / 2) * 4 + (x / 2) * 2 + j % 2] != 0) } else { mb.nz[k] != 0 };
            if any {
                info.nzmask |= 1 << k;
            }
        }
        if mb.ty.intra() {
            self.pics[cur].intra[addr] = true;
        }
        // Intra NxN modes.
        if matches!(mb.ty, MbType::I4x4 | MbType::I8x8) {
            let ctx = Ctx { info: &self.info, mbw: self.mbw, mbx, mby, slice: self.slice_num, kind: s.kind, num_ref: s.num_ref, t8x8_mode: pps.t8x8, direct8x8: true, constrained_intra: pps.constrained_intra };
            let step = if mb.ty == MbType::I8x8 { 4 } else { 1 };
            let mut k = 0;
            while k < 16 {
                let (bx, by) = BLK_XY[k];
                let mode_of = |n: Option<(Option<&MbInfo>, usize, usize)>, info: &MbInfo| -> Option<i32> {
                    match n {
                        None => None,
                        Some((None, x, y)) => Some(info.ipred[y * 4 + x] as i32),
                        Some((Some(m), x, y)) => {
                            if !m.ty.intra() && pps.constrained_intra {
                                None
                            } else if matches!(m.ty, MbType::I4x4 | MbType::I8x8) {
                                Some(m.ipred[y * 4 + x] as i32)
                            } else {
                                Some(2)
                            }
                        }
                    }
                };
                let a = mode_of(ctx.left_blk(bx, by), &info);
                let b = mode_of(ctx.top_blk(bx, by), &info);
                let pred = match (a, b) {
                    (Some(a), Some(b)) => a.min(b),
                    _ => 2,
                };
                let idx = if step == 4 { k / 4 } else { k };
                let mode = match mb.ipred[idx] {
                    None => pred,
                    Some(r) if (r as i32) < pred => r as i32,
                    Some(r) => r as i32 + 1,
                };
                if step == 4 {
                    for j in 0..4 {
                        let (x, y) = BLK_XY[k + j];
                        info.ipred[y * 4 + x] = mode as i8;
                    }
                } else {
                    info.ipred[by * 4 + bx] = mode as i8;
                }
                k += step;
            }
        }
        // Inter: motion vectors.
        let is_inter = !mb.ty.intra();
        if is_inter {
            let t0 = now();
            self.motion(mb, &mut info, mbx, mby, s);
            add_time(1, t0);
        } else {
            let p = &mut self.pics[cur];
            let w4 = self.mbw * 4;
            for by in 0..4 {
                for bx in 0..4 {
                    let i = (mby * 4 + by) * w4 + mbx * 4 + bx;
                    for l in 0..2 {
                        p.refi[l][i] = -1;
                        p.refid[l][i] = -1;
                        p.mv[l][i] = [0, 0];
                    }
                }
            }
        }
        self.info[addr] = info;
        if is_inter {
            let t0 = now();
            self.inter_pred(mbx, mby, s);
            add_time(2, t0);
        }
        let t0 = now();
        self.reconstruct(mb, mbx, mby, qp, pps, is_inter);
        add_time(3, t0);
    }

    /// Work out motion vectors and references of every 4x4 block of an inter macroblock.
    fn motion(&mut self, mb: &Mb, info: &mut MbInfo, mbx: usize, mby: usize, s: &Slice) {
        let cur = self.cur.unwrap();
        let w4 = self.mbw * 4;
        let base = mby * 4 * w4 + mbx * 4;
        // Per 4x4 (raster) of this MB: mv, ref for both lists; done flags for availability.
        let mut mv = [[[0i16; 2]; 16]; 2];
        let mut rf = [[-1i8; 16]; 2];
        let mut done = [false; 16];
        let b_slice = s.kind == SliceKind::B;
        match mb.ty {
            MbType::PSkip => {
                let (m, r) = self.pskip_mv(mbx, mby);
                for k in 0..16 {
                    mv[0][k] = m;
                    rf[0][k] = r;
                }
            }
            MbType::BSkip | MbType::BDirect => {
                self.direct(mbx, mby, s, &mut mv, &mut rf, 0b1111);
                info.direct = 0xFFFF;
            }
            _ => {
                let parts = partitions(mb.ty, &mb.sub, b_slice);
                let mut direct_mask = 0u8;
                for &(x, y, _, _, _, direct) in &parts {
                    if direct {
                        direct_mask |= 1 << ((y / 2) * 2 + x / 2);
                    }
                }
                if direct_mask != 0 {
                    self.direct(mbx, mby, s, &mut mv, &mut rf, direct_mask);
                    for k8 in 0..4 {
                        if direct_mask & (1 << k8) != 0 {
                            for j in 0..4 {
                                let (x, y) = ((k8 & 1) * 2 + (j & 1), (k8 >> 1) * 2 + (j >> 1));
                                done[y * 4 + x] = true;
                                info.direct |= 1 << (y * 4 + x);
                            }
                        }
                    }
                }
                for &(x, y, w, h, pred, direct) in &parts {
                    if direct {
                        continue;
                    }
                    for l in 0..2 {
                        if pred & (1 << l) == 0 {
                            continue;
                        }
                        let r = mb.refs[l][y * 4 + x].max(0);
                        let p = self.mv_pred(mbx, mby, x, y, w, h, l, r, &mv, &rf, &done);
                        let d = mb.mvd[l][y * 4 + x];
                        let m = [p[0].wrapping_add(d[0]), p[1].wrapping_add(d[1])];
                        for yy in y..y + h {
                            for xx in x..x + w {
                                mv[l][yy * 4 + xx] = m;
                                rf[l][yy * 4 + xx] = r;
                            }
                        }
                    }
                    for yy in y..y + h {
                        for xx in x..x + w {
                            done[yy * 4 + xx] = true;
                        }
                    }
                }
            }
        }
        // (id, poc, long-term) of each list entry.
        let linfo = core::mem::take(&mut self.list_info);
        let p = &mut self.pics[cur];
        for by in 0..4 {
            for bx in 0..4 {
                let i = base + by * w4 + bx;
                for l in 0..2 {
                    let r = rf[l][by * 4 + bx];
                    let e = if r >= 0 { linfo[l].get(r as usize).copied() } else { None };
                    match e {
                        Some((id, poc, long)) => {
                            p.refi[l][i] = r;
                            p.mv[l][i] = mv[l][by * 4 + bx];
                            p.refid[l][i] = id;
                            p.refpoc[l][i] = poc;
                            p.reflong[l][i] = long;
                        }
                        None => {
                            p.refi[l][i] = -1;
                            p.mv[l][i] = [0, 0];
                            p.refid[l][i] = -1;
                        }
                    }
                }
            }
        }
        self.list_info = linfo;
    }

    /// Motion of a neighbouring 4x4 block at (x, y) relative to the MB (may be outside):
    /// None = not available; Some((ref, mv)) with ref -1 for intra / unused list.
    #[allow(clippy::too_many_arguments)]
    fn nb_motion(&self, mbx: usize, mby: usize, x: isize, y: isize, l: usize, mv: &[[[i16; 2]; 16]; 2], rf: &[[i8; 16]; 2], done: &[bool; 16]) -> Option<(i8, [i16; 2])> {
        if (0..4).contains(&x) && (0..4).contains(&y) {
            let k = y as usize * 4 + x as usize;
            if !done[k] {
                return None;
            }
            return Some((rf[l][k], if rf[l][k] >= 0 { mv[l][k] } else { [0, 0] }));
        }
        let gx = mbx as isize * 4 + x;
        let gy = mby as isize * 4 + y;
        if gx < 0 || gy < 0 || gx >= (self.mbw * 4) as isize || y >= 4 {
            return None;
        }
        // Only MBs above, left, above-left, above-right (already decoded) can be used.
        let (nmx, nmy) = (gx as usize / 4, gy as usize / 4);
        if nmy == mby && nmx > mbx {
            return None;
        }
        let a = nmy * self.mbw + nmx;
        if self.info[a].slice != self.slice_num {
            return None;
        }
        let cur = self.cur.unwrap();
        let i = gy as usize * self.mbw * 4 + gx as usize;
        let p = &self.pics[cur];
        let r = p.refi[l][i];
        Some((r, if r >= 0 { p.mv[l][i] } else { [0, 0] }))
    }

    #[allow(clippy::too_many_arguments)]
    fn mv_pred(&self, mbx: usize, mby: usize, x: usize, y: usize, w: usize, h: usize, l: usize, r: i8, mv: &[[[i16; 2]; 16]; 2], rf: &[[i8; 16]; 2], done: &[bool; 16]) -> [i16; 2] {
        let (x, y, w, h) = (x as isize, y as isize, w as isize, h as isize);
        let a = self.nb_motion(mbx, mby, x - 1, y, l, mv, rf, done);
        let b = self.nb_motion(mbx, mby, x, y - 1, l, mv, rf, done);
        let mut c = self.nb_motion(mbx, mby, x + w, y - 1, l, mv, rf, done);
        if c.is_none() {
            c = self.nb_motion(mbx, mby, x - 1, y - 1, l, mv, rf, done);
        }
        // Directional predictions for 16x8 and 8x16.
        if w == 4 && h == 2 {
            if y == 0 {
                if let Some((rb, m)) = b {
                    if rb == r {
                        return m;
                    }
                }
            } else if let Some((ra, m)) = a {
                if ra == r {
                    return m;
                }
            }
        } else if w == 2 && h == 4 {
            if x == 0 {
                if let Some((ra, m)) = a {
                    if ra == r {
                        return m;
                    }
                }
            } else if let Some((rc, m)) = c {
                if rc == r {
                    return m;
                }
            }
        }
        median_pred(a, b, c, r)
    }

    fn pskip_mv(&self, mbx: usize, mby: usize) -> ([i16; 2], i8) {
        let mv = [[[0i16; 2]; 16]; 2];
        let rf = [[-1i8; 16]; 2];
        let done = [false; 16];
        let a = self.nb_motion(mbx, mby, -1, 0, 0, &mv, &rf, &done);
        let b = self.nb_motion(mbx, mby, 0, -1, 0, &mv, &rf, &done);
        match (a, b) {
            (None, _) | (_, None) => ([0, 0], 0),
            (Some((0, [0, 0])), _) | (_, Some((0, [0, 0]))) => ([0, 0], 0),
            _ => (self.mv_pred(mbx, mby, 0, 0, 4, 4, 0, 0, &mv, &rf, &done), 0),
        }
    }

    /// Direct prediction for the 8x8 blocks in `mask`.
    fn direct(&self, mbx: usize, mby: usize, s: &Slice, mv: &mut [[[i16; 2]; 16]; 2], rf: &mut [[i8; 16]; 2], mask: u8) {
        let cur = self.cur.unwrap();
        let Some(&col_i) = self.ref_lists[1].first() else { return };
        let col = &self.pics[col_i];
        let w4 = self.mbw * 4;
        let sps = self.sps[self.active_sps].as_ref().unwrap();
        let d8 = sps.direct8x8;
        let col_blk = |bx: usize, by: usize| -> usize {
            let (x, y) = if d8 { ((bx / 2) * 3, (by / 2) * 3) } else { (bx, by) };
            (mby * 4 + y) * w4 + mbx * 4 + x
        };
        // Colocated motion: L0 if used, else L1.
        let col_motion = |i: usize| -> (i8, [i16; 2], i32, i32) {
            if col.intra[i / w4 / 4 * self.mbw + (i % w4) / 4] {
                return (-1, [0, 0], -1, 0);
            }
            if col.refi[0][i] >= 0 {
                (col.refi[0][i], col.mv[0][i], col.refid[0][i], col.refpoc[0][i])
            } else {
                (col.refi[1][i], col.mv[1][i], col.refid[1][i], col.refpoc[1][i])
            }
        };
        if s.direct_spatial {
            let zero = [[[0i16; 2]; 16]; 2];
            let none = [[-1i8; 16]; 2];
            let nodone = [false; 16];
            let mut refs = [-1i8; 2];
            let mut preds = [[0i16; 2]; 2];
            for l in 0..2 {
                let a = self.nb_motion(mbx, mby, -1, 0, l, &zero, &none, &nodone);
                let b = self.nb_motion(mbx, mby, 0, -1, l, &zero, &none, &nodone);
                let mut c = self.nb_motion(mbx, mby, 4, -1, l, &zero, &none, &nodone);
                if c.is_none() {
                    c = self.nb_motion(mbx, mby, -1, -1, l, &zero, &none, &nodone);
                }
                let mp = |x: Option<(i8, [i16; 2])>| x.map(|v| v.0).unwrap_or(-1);
                let minp = |p: i8, q: i8| if p >= 0 && q >= 0 { p.min(q) } else { p.max(q) };
                refs[l] = minp(mp(a), minp(mp(b), mp(c)));
                if refs[l] >= 0 {
                    preds[l] = median_pred(a, b, c, refs[l]);
                }
            }
            let zero_pred = refs[0] < 0 && refs[1] < 0;
            if zero_pred {
                refs = [0, 0];
            }
            let col_short = !col.long;
            for k8 in 0..4 {
                if mask & (1 << k8) == 0 {
                    continue;
                }
                for j in 0..4 {
                    let (bx, by) = ((k8 & 1) * 2 + (j & 1), (k8 >> 1) * 2 + (j >> 1));
                    let k = by * 4 + bx;
                    let (cr, cm, _, _) = col_motion(col_blk(bx, by));
                    let col_zero = col_short && cr == 0 && (-1..=1).contains(&cm[0]) && (-1..=1).contains(&cm[1]);
                    for l in 0..2 {
                        rf[l][k] = refs[l];
                        mv[l][k] = if zero_pred || refs[l] < 0 || (refs[l] == 0 && col_zero) { [0, 0] } else { preds[l] };
                    }
                }
            }
            let _ = cur;
        } else {
            // Temporal.
            let cur_poc = self.pics[cur].poc;
            let poc1 = col.poc;
            for k8 in 0..4 {
                if mask & (1 << k8) == 0 {
                    continue;
                }
                for j in 0..4 {
                    let (bx, by) = ((k8 & 1) * 2 + (j & 1), (k8 >> 1) * 2 + (j >> 1));
                    let k = by * 4 + bx;
                    let (cr, cm, cid, cpoc) = col_motion(col_blk(bx, by));
                    let (r0, m0, m1) = if cr < 0 {
                        (0i8, [0i16; 2], [0i16; 2])
                    } else {
                        // The picture the colocated block used, in our list 0.
                        let r0 = self.ref_lists[0].iter().position(|&pi| self.pics[pi].id as i32 == cid).unwrap_or(0) as i8;
                        let pi0 = self.ref_lists[0].get(r0 as usize).copied();
                        let long = pi0.map(|p| self.pics[p].long).unwrap_or(false);
                        let poc0 = pi0.map(|p| self.pics[p].poc).unwrap_or(cpoc);
                        let tb = (cur_poc - poc0).clamp(-128, 127);
                        let td = (poc1 - poc0).clamp(-128, 127);
                        if long || td == 0 {
                            (r0, cm, [0, 0])
                        } else {
                            let tx = (16384 + (td / 2).abs()) / td;
                            let dsf = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
                            let m0 = [((dsf * cm[0] as i32 + 128) >> 8) as i16, ((dsf * cm[1] as i32 + 128) >> 8) as i16];
                            (r0, m0, [m0[0] - cm[0], m0[1] - cm[1]])
                        }
                    };
                    rf[0][k] = r0;
                    rf[1][k] = 0;
                    mv[0][k] = m0;
                    mv[1][k] = m1;
                }
            }
        }
    }

    /// Motion-compensated prediction of the macroblock, in the biggest blocks that share
    /// the same motion (16x16, 8x8 or 4x4).
    fn inter_pred(&mut self, mbx: usize, mby: usize, s: &Slice) {
        let cur = self.cur.unwrap();
        let w4 = self.mbw * 4;
        let key = |p: &Pic, bx: usize, by: usize| {
            let i = (mby * 4 + by) * w4 + mbx * 4 + bx;
            (p.refi[0][i], p.refi[1][i], p.mv[0][i], p.mv[1][i])
        };
        let p = &self.pics[cur];
        let k0 = key(p, 0, 0);
        if (0..16).all(|k| key(p, k % 4, k / 4) == k0) {
            self.pred_block(mbx, mby, 0, 0, 4, s);
            return;
        }
        for k8 in 0..4 {
            let (bx, by) = ((k8 & 1) * 2, (k8 >> 1) * 2);
            let p = &self.pics[cur];
            let k = key(p, bx, by);
            if key(p, bx + 1, by) == k && key(p, bx, by + 1) == k && key(p, bx + 1, by + 1) == k {
                self.pred_block(mbx, mby, bx, by, 2, s);
            } else {
                for j in 0..4 {
                    self.pred_block(mbx, mby, bx + (j & 1), by + (j >> 1), 1, s);
                }
            }
        }
    }

    /// Predict an n x n (4x4 units) block whose 4x4 blocks all share motion.
    fn pred_block(&mut self, mbx: usize, mby: usize, bx: usize, by: usize, n: usize, s: &Slice) {
        let cur = self.cur.unwrap();
        let w4 = self.mbw * 4;
        let stride = self.mbw * 16;
        let cstride = stride / 2;
        let ph = (self.mbh * 16) as isize;
        let pw = stride as isize;
        let bw = n * 4;
        let i = (mby * 4 + by) * w4 + mbx * 4 + bx;
        let x = (mbx * 16 + bx * 4) as isize;
        let y = (mby * 16 + by * 4) as isize;
        let mut luma = [[0u8; 256]; 2];
        let mut cb = [[0u8; 64]; 2];
        let mut cr = [[0u8; 64]; 2];
        let mut used = [false; 2];
        let mut refidx = [0usize; 2];
        for l in 0..2 {
            let r = self.pics[cur].refi[l][i];
            if r < 0 {
                continue;
            }
            let Some(&pi) = self.ref_lists[l].get(r as usize) else { continue };
            used[l] = true;
            refidx[l] = r as usize;
            let m = self.pics[cur].mv[l][i];
            let refp = &self.pics[pi];
            luma_mc(&refp.y, stride, pw, ph, x, y, m, bw, &mut luma[l]);
            chroma_mc(&refp.u, cstride, pw / 2, ph / 2, x / 2, y / 2, m, bw / 2, &mut cb[l]);
            chroma_mc(&refp.v, cstride, pw / 2, ph / 2, x / 2, y / 2, m, bw / 2, &mut cr[l]);
        }
        if !used[0] && !used[1] {
            return; // broken stream: keep whatever is there
        }
        // Weights.
        let pps = self.pps[self.active_pps].as_ref().unwrap();
        let implicit = s.kind == SliceKind::B && pps.bipred == 2;
        let (mut w, mut o, mut logwd) = ([[1i32; 3]; 2], [[0i32; 3]; 2], [0u32; 3]);
        let mut weighted = false;
        let explicit = s.weights.is_some();
        if let Some(wt) = &s.weights {
            weighted = true;
            logwd = [wt.luma_log2, wt.chroma_log2, wt.chroma_log2];
            for l in 0..2 {
                if used[l] {
                    if let Some(e) = wt.w[l].get(refidx[l]) {
                        for c in 0..3 {
                            w[l][c] = e[c].0;
                            o[l][c] = e[c].1;
                        }
                    }
                }
            }
        } else if implicit && used[0] && used[1] {
            let p0 = self.ref_lists[0][refidx[0]];
            let p1 = self.ref_lists[1][refidx[1]];
            let (poc0, poc1) = (self.pics[p0].poc, self.pics[p1].poc);
            let cur_poc = self.pics[cur].poc;
            let tb = (cur_poc - poc0).clamp(-128, 127);
            let td = (poc1 - poc0).clamp(-128, 127);
            let (mut w0, mut w1) = (32, 32);
            if td != 0 && !self.pics[p0].long && !self.pics[p1].long {
                let tx = (16384 + (td / 2).abs()) / td;
                let dsf = ((tb * tx + 32) >> 6).clamp(-1024, 1023);
                if (-64..=128).contains(&(dsf >> 2)) {
                    w0 = 64 - (dsf >> 2);
                    w1 = dsf >> 2;
                }
            }
            weighted = true;
            w = [[w0; 3], [w1; 3]];
            logwd = [5; 3];
        }
        let combine = |p0: &[u8], p1: &[u8], c: usize, out: &mut [u8]| {
            if used[0] && used[1] {
                if weighted {
                    let lw = logwd[c] as i32;
                    let off = (o[0][c] + o[1][c] + 1) >> 1;
                    for k in 0..out.len() {
                        out[k] = clip(((p0[k] as i32 * w[0][c] + p1[k] as i32 * w[1][c] + (1 << lw)) >> (lw + 1)) + off);
                    }
                } else {
                    for k in 0..out.len() {
                        out[k] = ((p0[k] as u32 + p1[k] as u32 + 1) >> 1) as u8;
                    }
                }
            } else {
                let (l, src) = if used[0] { (0, p0) } else { (1, p1) };
                if weighted && explicit {
                    let lw = logwd[c] as i32;
                    for k in 0..out.len() {
                        let x = src[k] as i32;
                        out[k] = clip(if lw >= 1 { ((x * w[l][c] + (1 << (lw - 1))) >> lw) + o[l][c] } else { x * w[l][c] + o[l][c] });
                    }
                } else {
                    out.copy_from_slice(&src[..out.len()]);
                }
            }
        };
        let (n2, nc) = (bw * bw, (bw / 2) * (bw / 2));
        let mut ly = [0u8; 256];
        let mut lu = [0u8; 64];
        let mut lv = [0u8; 64];
        combine(&luma[0], &luma[1], 0, &mut ly[..n2]);
        combine(&cb[0], &cb[1], 1, &mut lu[..nc]);
        combine(&cr[0], &cr[1], 2, &mut lv[..nc]);
        let p = &mut self.pics[cur];
        let (x0, y0) = (x as usize, y as usize);
        for yy in 0..bw {
            p.y[(y0 + yy) * stride + x0..(y0 + yy) * stride + x0 + bw].copy_from_slice(&ly[yy * bw..yy * bw + bw]);
        }
        let cw = bw / 2;
        for yy in 0..cw {
            let o = (y0 / 2 + yy) * cstride + x0 / 2;
            p.u[o..o + cw].copy_from_slice(&lu[yy * cw..yy * cw + cw]);
            p.v[o..o + cw].copy_from_slice(&lv[yy * cw..yy * cw + cw]);
        }
    }

    fn reconstruct(&mut self, mb: &Mb, mbx: usize, mby: usize, qp: i32, pps: &Pps, inter: bool) {
        let cur = self.cur.unwrap();
        let stride = self.mbw * 16;
        let cstride = stride / 2;
        let (x0, y0) = (mbx * 16, mby * 16);
        if mb.ty == MbType::IPcm {
            let p = &mut self.pics[cur];
            for y in 0..16 {
                for x in 0..16 {
                    p.y[(y0 + y) * stride + x0 + x] = *mb.pcm.get(y * 16 + x).unwrap_or(&128);
                }
            }
            for y in 0..8 {
                for x in 0..8 {
                    p.u[(y0 / 2 + y) * cstride + x0 / 2 + x] = *mb.pcm.get(256 + y * 8 + x).unwrap_or(&128);
                    p.v[(y0 / 2 + y) * cstride + x0 / 2 + x] = *mb.pcm.get(320 + y * 8 + x).unwrap_or(&128);
                }
            }
            return;
        }
        if self.ls_cache.as_ref().map(|c| (c.0, c.1)) != Some((self.active_pps, qp)) {
            self.ls_cache = Some((self.active_pps, qp, LevelScale::new(pps, qp)));
        }
        let cache = self.ls_cache.take().unwrap();
        let ls = &cache.2;
        let avail = self.intra_avail(mbx, mby, pps.constrained_intra);
        let info = self.info[mby * self.mbw + mbx];
        // Luma.
        match mb.ty {
            MbType::I16x16 => {
                self.pred16(mbx, mby, mb.i16_mode, avail);
                let dc = ls.luma_dc(&mb.dc, inter);
                for k in 0..16 {
                    let (bx, by) = (k % 4, k / 4);
                    let mut c = if mb.cbp & 15 != 0 { ls.dequant4(&mb.luma[k], 0, inter, true) } else { [0; 16] };
                    c[0] = dc[k];
                    self.add4(cur, x0 + bx * 4, y0 + by * 4, &c, stride, 0);
                }
            }
            MbType::I4x4 => {
                for z in 0..16 {
                    let (bx, by) = BLK_XY[z];
                    let mode = info.ipred[by * 4 + bx];
                    self.pred_nxn(mbx, mby, bx, by, 4, mode as u8, avail);
                    let k = by * 4 + bx;
                    if mb.cbp & (1 << ((by / 2) * 2 + bx / 2)) != 0 {
                        let c = ls.dequant4(&mb.luma[k], 0, false, false);
                        self.add4(cur, x0 + bx * 4, y0 + by * 4, &c, stride, 0);
                    }
                }
            }
            MbType::I8x8 => {
                for k8 in 0..4 {
                    let (bx, by) = ((k8 & 1) * 2, (k8 >> 1) * 2);
                    let mode = info.ipred[by * 4 + bx];
                    self.pred_nxn(mbx, mby, bx, by, 8, mode as u8, avail);
                    if mb.cbp & (1 << k8) != 0 {
                        let c = ls.dequant8(&mb.luma8[k8], false);
                        self.add8(cur, x0 + bx * 4, y0 + by * 4, &c, stride);
                    }
                }
            }
            _ => {
                // Inter residual.
                for k8 in 0..4 {
                    if mb.cbp & (1 << k8) == 0 {
                        continue;
                    }
                    let (bx, by) = ((k8 & 1) * 2, (k8 >> 1) * 2);
                    if mb.t8x8 {
                        let c = ls.dequant8(&mb.luma8[k8], true);
                        self.add8(cur, x0 + bx * 4, y0 + by * 4, &c, stride);
                    } else {
                        for j in 0..4 {
                            let (x, y) = (bx + (j & 1), by + (j >> 1));
                            let c = ls.dequant4(&mb.luma[y * 4 + x], 0, true, false);
                            self.add4(cur, x0 + x * 4, y0 + y * 4, &c, stride, 0);
                        }
                    }
                }
            }
        }
        // Chroma.
        if mb.ty.intra() {
            self.pred_chroma(mbx, mby, mb.chroma_mode, avail);
        }
        if mb.cbp >> 4 != 0 {
            for c in 0..2 {
                let dc = ls.chroma_dc(&mb.cdc[c], c, inter);
                for k in 0..4 {
                    let mut co = if mb.cbp >> 4 == 2 { ls.dequant4(&mb.cac[c * 4 + k], c + 1, inter, true) } else { [0; 16] };
                    co[0] = dc[k];
                    self.add4(cur, x0 / 2 + (k % 2) * 4, y0 / 2 + (k / 2) * 4, &co, cstride, c + 1);
                }
            }
        }
        self.ls_cache = Some(cache);
    }

    /// Add an inverse-transformed 4x4 block to plane `plane` (0 Y, 1 Cb, 2 Cr).
    fn add4(&mut self, cur: usize, x: usize, y: usize, c: &[i32; 16], stride: usize, plane: usize) {
        if c.iter().all(|&v| v == 0) {
            return;
        }
        let r = idct4(c);
        let p = &mut self.pics[cur];
        let buf = match plane {
            0 => &mut p.y,
            1 => &mut p.u,
            _ => &mut p.v,
        };
        for yy in 0..4 {
            for xx in 0..4 {
                let o = (y + yy) * stride + x + xx;
                buf[o] = clip(buf[o] as i32 + r[yy * 4 + xx]);
            }
        }
    }

    fn add8(&mut self, cur: usize, x: usize, y: usize, c: &[i32; 64], stride: usize) {
        if c.iter().all(|&v| v == 0) {
            return;
        }
        let r = idct8(c);
        let p = &mut self.pics[cur];
        for yy in 0..8 {
            for xx in 0..8 {
                let o = (y + yy) * stride + x + xx;
                p.y[o] = clip(p.y[o] as i32 + r[yy * 8 + xx]);
            }
        }
    }

    /// Neighbour MBs usable for intra prediction: (left, top, top-left, top-right).
    fn intra_avail(&self, mbx: usize, mby: usize, constrained: bool) -> [bool; 4] {
        let ok = |x: isize, y: isize| -> bool {
            if x < 0 || y < 0 || x >= self.mbw as isize {
                return false;
            }
            let m = &self.info[y as usize * self.mbw + x as usize];
            m.slice == self.slice_num && (!constrained || m.ty.intra())
        };
        let (x, y) = (mbx as isize, mby as isize);
        [ok(x - 1, y), ok(x, y - 1), ok(x - 1, y - 1), ok(x + 1, y - 1)]
    }

    fn pred16(&mut self, mbx: usize, mby: usize, mode: u8, avail: [bool; 4]) {
        let cur = self.cur.unwrap();
        let stride = self.mbw * 16;
        let p = &mut self.pics[cur];
        let (x0, y0) = (mbx * 16, mby * 16);
        let top: [i32; 16] = core::array::from_fn(|i| if avail[1] { p.y[(y0 - 1) * stride + x0 + i] as i32 } else { 0 });
        let left: [i32; 16] = core::array::from_fn(|i| if avail[0] { p.y[(y0 + i) * stride + x0 - 1] as i32 } else { 0 });
        let tl = if avail[2] { p.y[(y0 - 1) * stride + x0 - 1] as i32 } else { 128 };
        let mut out = [0u8; 256];
        match mode {
            0 if avail[1] => {
                for y in 0..16 {
                    for x in 0..16 {
                        out[y * 16 + x] = top[x] as u8;
                    }
                }
            }
            1 if avail[0] => {
                for y in 0..16 {
                    for x in 0..16 {
                        out[y * 16 + x] = left[y] as u8;
                    }
                }
            }
            3 if avail[0] && avail[1] && avail[2] => {
                let t = |i: isize| if i < 0 { tl } else { top[i as usize] };
                let l = |i: isize| if i < 0 { tl } else { left[i as usize] };
                let mut h = 0;
                let mut v = 0;
                for k in 0..8isize {
                    h += (k + 1) as i32 * (t(8 + k) - t(6 - k));
                    v += (k + 1) as i32 * (l(8 + k) - l(6 - k));
                }
                let a = 16 * (left[15] + top[15]);
                let b = (5 * h + 32) >> 6;
                let c = (5 * v + 32) >> 6;
                for y in 0..16 {
                    for x in 0..16 {
                        out[y * 16 + x] = clip((a + b * (x as i32 - 7) + c * (y as i32 - 7) + 16) >> 5);
                    }
                }
            }
            _ => {
                let dc = match (avail[0], avail[1]) {
                    (true, true) => (top.iter().sum::<i32>() + left.iter().sum::<i32>() + 16) >> 5,
                    (true, false) => (left.iter().sum::<i32>() + 8) >> 4,
                    (false, true) => (top.iter().sum::<i32>() + 8) >> 4,
                    _ => 128,
                };
                out.fill(dc as u8);
            }
        }
        for y in 0..16 {
            p.y[(y0 + y) * stride + x0..(y0 + y) * stride + x0 + 16].copy_from_slice(&out[y * 16..y * 16 + 16]);
        }
    }

    /// Intra 4x4 / 8x8 prediction of the block at (bx, by) (4x4 units).
    #[allow(clippy::too_many_arguments)]
    fn pred_nxn(&mut self, mbx: usize, mby: usize, bx: usize, by: usize, n: usize, mode: u8, avail: [bool; 4]) {
        let cur = self.cur.unwrap();
        let stride = self.mbw * 16;
        let (x0, y0) = (mbx * 16 + bx * 4, mby * 16 + by * 4);
        let nb = n / 4; // block size in 4x4 units
        let has_left = bx > 0 || avail[0];
        let has_top = by > 0 || avail[1];
        let has_tl = match (bx > 0, by > 0) {
            (true, true) => true,
            (false, false) => avail[2],
            (false, true) => avail[0],
            (true, false) => avail[1],
        };
        let has_tr = if by == 0 {
            if bx + nb < 4 {
                avail[1]
            } else {
                avail[3]
            }
        } else if bx + nb >= 4 {
            false
        } else {
            BLK_Z[by - 1][bx + nb] < BLK_Z[by][bx]
        };
        let p = &mut self.pics[cur];
        // top[0] = corner, top[1..=2n] = above row; left[0] = corner, left[1..=n] = left column.
        let mut top = [128i32; 17];
        let mut left = [128i32; 9];
        if has_top {
            for i in 0..n {
                top[1 + i] = p.y[(y0 - 1) * stride + x0 + i] as i32;
            }
            for i in n..2 * n {
                top[1 + i] = if has_tr { p.y[(y0 - 1) * stride + x0 + i] as i32 } else { top[n] };
            }
        }
        if has_left {
            for i in 0..n {
                left[1 + i] = p.y[(y0 + i) * stride + x0 - 1] as i32;
            }
        }
        if has_tl {
            top[0] = p.y[(y0 - 1) * stride + x0 - 1] as i32;
            left[0] = top[0];
        }
        if n == 8 {
            // Reference sample filtering.
            let (t, l) = (top, left);
            if has_top {
                top[1] = if has_tl { (t[0] + 2 * t[1] + t[2] + 2) >> 2 } else { (3 * t[1] + t[2] + 2) >> 2 };
                for i in 2..16 {
                    top[i] = (t[i - 1] + 2 * t[i] + t[i + 1] + 2) >> 2;
                }
                top[16] = (t[15] + 3 * t[16] + 2) >> 2;
            }
            if has_tl {
                let c = match (has_top, has_left) {
                    (true, true) => (t[1] + 2 * t[0] + l[1] + 2) >> 2,
                    (true, false) => (3 * t[0] + t[1] + 2) >> 2,
                    (false, true) => (3 * t[0] + l[1] + 2) >> 2,
                    _ => t[0],
                };
                top[0] = c;
                left[0] = c;
            }
            if has_left {
                left[1] = if has_tl { (l[0] + 2 * l[1] + l[2] + 2) >> 2 } else { (3 * l[1] + l[2] + 2) >> 2 };
                for i in 2..8 {
                    left[i] = (l[i - 1] + 2 * l[i] + l[i + 1] + 2) >> 2;
                }
                left[8] = (l[7] + 3 * l[8] + 2) >> 2;
            }
        }
        let t = |i: i32| top[(i + 1) as usize]; // p[i, -1], i >= -1
        let l = |i: i32| left[(i + 1) as usize]; // p[-1, i], i >= -1
        let ni = n as i32;
        let mut out = [0u8; 64];
        for y in 0..ni {
            for x in 0..ni {
                let v = match mode {
                    0 => t(x),
                    1 => l(y),
                    3 => {
                        if x == ni - 1 && y == ni - 1 {
                            (t(2 * ni - 2) + 3 * t(2 * ni - 1) + 2) >> 2
                        } else {
                            (t(x + y) + 2 * t(x + y + 1) + t(x + y + 2) + 2) >> 2
                        }
                    }
                    4 => {
                        if x > y {
                            (t(x - y - 2) + 2 * t(x - y - 1) + t(x - y) + 2) >> 2
                        } else if x < y {
                            (l(y - x - 2) + 2 * l(y - x - 1) + l(y - x) + 2) >> 2
                        } else {
                            (t(0) + 2 * t(-1) + l(0) + 2) >> 2
                        }
                    }
                    5 => {
                        let z = 2 * x - y;
                        if z >= 0 && z % 2 == 0 {
                            (t(x - (y >> 1) - 1) + t(x - (y >> 1)) + 1) >> 1
                        } else if z > 0 {
                            (t(x - (y >> 1) - 2) + 2 * t(x - (y >> 1) - 1) + t(x - (y >> 1)) + 2) >> 2
                        } else if z == -1 {
                            (l(0) + 2 * l(-1) + t(0) + 2) >> 2
                        } else {
                            (l(y - 2 * x - 1) + 2 * l(y - 2 * x - 2) + l(y - 2 * x - 3) + 2) >> 2
                        }
                    }
                    6 => {
                        let z = 2 * y - x;
                        if z >= 0 && z % 2 == 0 {
                            (l(y - (x >> 1) - 1) + l(y - (x >> 1)) + 1) >> 1
                        } else if z > 0 {
                            (l(y - (x >> 1) - 2) + 2 * l(y - (x >> 1) - 1) + l(y - (x >> 1)) + 2) >> 2
                        } else if z == -1 {
                            (l(0) + 2 * l(-1) + t(0) + 2) >> 2
                        } else {
                            (t(x - 2 * y - 1) + 2 * t(x - 2 * y - 2) + t(x - 2 * y - 3) + 2) >> 2
                        }
                    }
                    7 => {
                        if y % 2 == 0 {
                            (t(x + (y >> 1)) + t(x + (y >> 1) + 1) + 1) >> 1
                        } else {
                            (t(x + (y >> 1)) + 2 * t(x + (y >> 1) + 1) + t(x + (y >> 1) + 2) + 2) >> 2
                        }
                    }
                    8 => {
                        let z = x + 2 * y;
                        let lim = 2 * ni - 3;
                        if z < lim && z % 2 == 0 {
                            (l(y + (x >> 1)) + l(y + (x >> 1) + 1) + 1) >> 1
                        } else if z < lim {
                            (l(y + (x >> 1)) + 2 * l(y + (x >> 1) + 1) + l(y + (x >> 1) + 2) + 2) >> 2
                        } else if z == lim {
                            (l(ni - 2) + 3 * l(ni - 1) + 2) >> 2
                        } else {
                            l(ni - 1)
                        }
                    }
                    _ => {
                        // DC
                        let st: i32 = (0..ni).map(t).sum();
                        let sl: i32 = (0..ni).map(l).sum();
                        let sh = if n == 8 { 3 } else { 2 };
                        match (has_top, has_left) {
                            (true, true) => (st + sl + ni) >> (sh + 1),
                            (true, false) => (st + ni / 2) >> sh,
                            (false, true) => (sl + ni / 2) >> sh,
                            _ => 128,
                        }
                    }
                };
                out[(y * ni + x) as usize] = clip(v);
            }
        }
        for y in 0..n {
            p.y[(y0 + y) * stride + x0..(y0 + y) * stride + x0 + n].copy_from_slice(&out[y * n..y * n + n]);
        }
    }

    fn pred_chroma(&mut self, mbx: usize, mby: usize, mode: u8, avail: [bool; 4]) {
        let cur = self.cur.unwrap();
        let cs = self.mbw * 8;
        let (x0, y0) = (mbx * 8, mby * 8);
        let p = &mut self.pics[cur];
        for plane in 0..2 {
            let buf = if plane == 0 { &mut p.u } else { &mut p.v };
            let top: [i32; 8] = core::array::from_fn(|i| if avail[1] { buf[(y0 - 1) * cs + x0 + i] as i32 } else { 128 });
            let left: [i32; 8] = core::array::from_fn(|i| if avail[0] { buf[(y0 + i) * cs + x0 - 1] as i32 } else { 128 });
            let tl = if avail[2] { buf[(y0 - 1) * cs + x0 - 1] as i32 } else { 128 };
            let mut out = [0u8; 64];
            match mode {
                1 if avail[0] => {
                    for y in 0..8 {
                        for x in 0..8 {
                            out[y * 8 + x] = left[y] as u8;
                        }
                    }
                }
                2 if avail[1] => {
                    for y in 0..8 {
                        for x in 0..8 {
                            out[y * 8 + x] = top[x] as u8;
                        }
                    }
                }
                3 if avail[0] && avail[1] && avail[2] => {
                    let t = |i: isize| if i < 0 { tl } else { top[i as usize] };
                    let l = |i: isize| if i < 0 { tl } else { left[i as usize] };
                    let (mut h, mut v) = (0, 0);
                    for k in 0..4isize {
                        h += (k + 1) as i32 * (t(4 + k) - t(2 - k));
                        v += (k + 1) as i32 * (l(4 + k) - l(2 - k));
                    }
                    let a = 16 * (left[7] + top[7]);
                    let b = (34 * h + 32) >> 6;
                    let c = (34 * v + 32) >> 6;
                    for y in 0..8 {
                        for x in 0..8 {
                            out[y * 8 + x] = clip((a + b * (x as i32 - 3) + c * (y as i32 - 3) + 16) >> 5);
                        }
                    }
                }
                _ => {
                    for blk in 0..4 {
                        let (bx, by) = ((blk % 2) * 4, (blk / 2) * 4);
                        let st: i32 = top[bx..bx + 4].iter().sum();
                        let sl: i32 = left[by..by + 4].iter().sum();
                        let (ht, hl) = (avail[1], avail[0]);
                        let v = match blk {
                            0 | 3 => match (ht, hl) {
                                (true, true) => (st + sl + 4) >> 3,
                                (true, false) => (st + 2) >> 2,
                                (false, true) => (sl + 2) >> 2,
                                _ => 128,
                            },
                            1 => {
                                if ht {
                                    (st + 2) >> 2
                                } else if hl {
                                    (sl + 2) >> 2
                                } else {
                                    128
                                }
                            }
                            _ => {
                                if hl {
                                    (sl + 2) >> 2
                                } else if ht {
                                    (st + 2) >> 2
                                } else {
                                    128
                                }
                            }
                        };
                        for y in 0..4 {
                            for x in 0..4 {
                                out[(by + y) * 8 + bx + x] = v as u8;
                            }
                        }
                    }
                }
            }
            for y in 0..8 {
                buf[(y0 + y) * cs + x0..(y0 + y) * cs + x0 + 8].copy_from_slice(&out[y * 8..y * 8 + 8]);
            }
        }
    }

    // ---- deblocking ------------------------------------------------------------------------

    fn deblock(&mut self, cur: usize, pps: &Pps) {
        let (mbw, mbh) = (self.mbw, self.mbh);
        let stride = mbw * 16;
        let cs = stride / 2;
        let w4 = mbw * 4;
        let pic = &mut self.pics[cur];
        let info = &self.info;
        let qpc = |qp: u8, c: usize| -> i32 { QPC[(qp as i32 + pps.cqp[c]).clamp(0, 51) as usize] as i32 };
        for mby in 0..mbh {
            for mbx in 0..mbw {
                let addr = mby * mbw + mbx;
                let m = info[addr];
                if m.slice == 0 || m.deblock == 1 {
                    continue;
                }
                let qp_of = |mm: &MbInfo| if mm.ty == MbType::IPcm { 0u8 } else { mm.qp };
                for dir in 0..2 {
                    // dir 0: vertical edges (filter horizontally), 1: horizontal edges.
                    for e in 0..4 {
                        if e > 0 && m.t8x8 && e % 2 == 1 {
                            continue;
                        }
                        let n = if e == 0 {
                            let (nx, ny) = if dir == 0 { (mbx as isize - 1, mby as isize) } else { (mbx as isize, mby as isize - 1) };
                            if nx < 0 || ny < 0 {
                                continue;
                            }
                            let na = ny as usize * mbw + nx as usize;
                            if m.deblock == 2 && info[na].slice != m.slice {
                                continue;
                            }
                            na
                        } else {
                            addr
                        };
                        let nm = info[n];
                        // Boundary strength per 4-sample segment.
                        let mut bs = [0u8; 4];
                        for k in 0..4 {
                            let (qx, qy) = if dir == 0 { (e, k) } else { (k, e) };
                            let (px, py) = if dir == 0 { ((e + 3) % 4, k) } else { (k, (e + 3) % 4) };
                            let qi = (mby * 4 + qy) * w4 + mbx * 4 + qx;
                            let pi = if e == 0 {
                                if dir == 0 {
                                    (mby * 4 + py) * w4 + (mbx - 1) * 4 + px
                                } else {
                                    ((mby - 1) * 4 + py) * w4 + mbx * 4 + px
                                }
                            } else {
                                (mby * 4 + py) * w4 + mbx * 4 + px
                            };
                            bs[k] = if m.ty.intra() || nm.ty.intra() {
                                if e == 0 {
                                    4
                                } else {
                                    3
                                }
                            } else if m.nzmask & (1 << (qy * 4 + qx)) != 0 || nm.nzmask & (1 << (py * 4 + px)) != 0 {
                                2
                            } else {
                                motion_bs(pic, pi, qi)
                            };
                        }
                        if bs == [0; 4] {
                            continue;
                        }
                        let qpav = (qp_of(&m) as i32 + qp_of(&nm) as i32 + 1) >> 1;
                        let ia = (qpav + m.alpha as i32).clamp(0, 51) as usize;
                        let ib = (qpav + m.beta as i32).clamp(0, 51) as usize;
                        let (alpha, beta) = (ALPHA[ia] as i32, BETA[ib] as i32);
                        // Luma: 16 samples along the edge.
                        let (step, pitch, base) = if dir == 0 { (1isize, stride as isize, (mby * 16) * stride + mbx * 16 + e * 4) } else { (stride as isize, 1isize, (mby * 16 + e * 4) * stride + mbx * 16) };
                        for s in 0..16 {
                            let b = bs[s / 4];
                            if b == 0 {
                                continue;
                            }
                            let o = base as isize + s as isize * pitch;
                            filter_edge(&mut pic.y, o, step, b, alpha, beta, TC0[ia], true);
                        }
                        // Chroma: edges 0 and 2 (luma 0 and 8) only.
                        if e % 2 == 0 {
                            for c in 0..2 {
                                let qav = (qpc(qp_of(&m), c) + qpc(qp_of(&nm), c) + 1) >> 1;
                                let ia = (qav + m.alpha as i32).clamp(0, 51) as usize;
                                let ib = (qav + m.beta as i32).clamp(0, 51) as usize;
                                let (alpha, beta) = (ALPHA[ia] as i32, BETA[ib] as i32);
                                let (step, pitch, base) = if dir == 0 { (1isize, cs as isize, (mby * 8) * cs + mbx * 8 + e * 2) } else { (cs as isize, 1isize, (mby * 8 + e * 2) * cs + mbx * 8) };
                                let buf = if c == 0 { &mut pic.u } else { &mut pic.v };
                                for s in 0..8 {
                                    let b = bs[s / 2];
                                    if b == 0 {
                                        continue;
                                    }
                                    let o = base as isize + s as isize * pitch;
                                    filter_edge(buf, o, step, b, alpha, beta, TC0[ia], false);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}

fn median_pred(a: Option<(i8, [i16; 2])>, b: Option<(i8, [i16; 2])>, c: Option<(i8, [i16; 2])>, r: i8) -> [i16; 2] {
    let (mut a, mut b, mut c) = (a, b, c);
    if b.is_none() && c.is_none() && a.is_some() {
        b = a;
        c = a;
    }
    let ra = a.map(|v| v.0).unwrap_or(-1);
    let rb = b.map(|v| v.0).unwrap_or(-1);
    let rc = c.map(|v| v.0).unwrap_or(-1);
    let ma = a.map(|v| v.1).unwrap_or([0, 0]);
    let mb = b.map(|v| v.1).unwrap_or([0, 0]);
    let mc = c.map(|v| v.1).unwrap_or([0, 0]);
    let hits = (ra == r) as u32 + (rb == r) as u32 + (rc == r) as u32;
    if hits == 1 {
        return if ra == r {
            ma
        } else if rb == r {
            mb
        } else {
            mc
        };
    }
    a = None;
    let _ = a;
    let med = |x: i16, y: i16, z: i16| x.max(y).min(x.min(y).max(z));
    [med(ma[0], mb[0], mc[0]), med(ma[1], mb[1], mc[1])]
}

/// Boundary strength 1 or 0 from the motion of the blocks on either side.
fn motion_bs(pic: &Pic, p: usize, q: usize) -> u8 {
    let refs = |i: usize| -> ([i32; 2], [[i16; 2]; 2]) { ([pic.refid[0][i], pic.refid[1][i]], [pic.mv[0][i], pic.mv[1][i]]) };
    let (rp, mp) = refs(p);
    let (rq, mq) = refs(q);
    let far = |a: [i16; 2], b: [i16; 2]| (a[0] as i32 - b[0] as i32).abs() >= 4 || (a[1] as i32 - b[1] as i32).abs() >= 4;
    let np = (rp[0] >= 0) as u32 + (rp[1] >= 0) as u32;
    let nq = (rq[0] >= 0) as u32 + (rq[1] >= 0) as u32;
    if np != nq {
        return 1;
    }
    if np == 1 {
        let (a, ma) = if rp[0] >= 0 { (rp[0], mp[0]) } else { (rp[1], mp[1]) };
        let (b, mb) = if rq[0] >= 0 { (rq[0], mq[0]) } else { (rq[1], mq[1]) };
        return (a != b || far(ma, mb)) as u8;
    }
    if np == 0 {
        return 0;
    }
    // Two motion vectors each.
    let same_set = (rp[0] == rq[0] && rp[1] == rq[1]) || (rp[0] == rq[1] && rp[1] == rq[0]);
    if !same_set {
        return 1;
    }
    if rp[0] != rp[1] {
        // Two different pictures: compare the vectors that point at the same picture.
        if rp[0] == rq[0] {
            (far(mp[0], mq[0]) || far(mp[1], mq[1])) as u8
        } else {
            (far(mp[0], mq[1]) || far(mp[1], mq[0])) as u8
        }
    } else {
        ((far(mp[0], mq[0]) || far(mp[1], mq[1])) && (far(mp[0], mq[1]) || far(mp[1], mq[0]))) as u8
    }
}

#[allow(clippy::too_many_arguments)]
fn filter_edge(buf: &mut [u8], o: isize, step: isize, bs: u8, alpha: i32, beta: i32, tc0: [u8; 3], luma: bool) {
    let at = |k: isize| (o + k * step) as usize;
    let (p0, p1, q0, q1) = (buf[at(-1)] as i32, buf[at(-2)] as i32, buf[at(0)] as i32, buf[at(1)] as i32);
    if (p0 - q0).abs() >= alpha || (p1 - p0).abs() >= beta || (q1 - q0).abs() >= beta {
        return;
    }
    if luma {
        let (p2, q2) = (buf[at(-3)] as i32, buf[at(2)] as i32);
        let ap = (p2 - p0).abs();
        let aq = (q2 - q0).abs();
        if bs < 4 {
            let tc0 = tc0[bs as usize - 1] as i32;
            let tc = tc0 + (ap < beta) as i32 + (aq < beta) as i32;
            let delta = ((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
            buf[at(-1)] = clip(p0 + delta);
            buf[at(0)] = clip(q0 - delta);
            if ap < beta {
                buf[at(-2)] = (p1 + ((p2 + ((p0 + q0 + 1) >> 1) - (p1 << 1)) >> 1).clamp(-tc0, tc0)) as u8;
            }
            if aq < beta {
                buf[at(1)] = (q1 + ((q2 + ((p0 + q0 + 1) >> 1) - (q1 << 1)) >> 1).clamp(-tc0, tc0)) as u8;
            }
        } else {
            let (p3, q3) = (buf[at(-4)] as i32, buf[at(3)] as i32);
            let strong = (p0 - q0).abs() < (alpha >> 2) + 2;
            if ap < beta && strong {
                buf[at(-1)] = ((p2 + 2 * p1 + 2 * p0 + 2 * q0 + q1 + 4) >> 3) as u8;
                buf[at(-2)] = ((p2 + p1 + p0 + q0 + 2) >> 2) as u8;
                buf[at(-3)] = ((2 * p3 + 3 * p2 + p1 + p0 + q0 + 4) >> 3) as u8;
            } else {
                buf[at(-1)] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
            }
            if aq < beta && strong {
                buf[at(0)] = ((p1 + 2 * p0 + 2 * q0 + 2 * q1 + q2 + 4) >> 3) as u8;
                buf[at(1)] = ((p0 + q0 + q1 + q2 + 2) >> 2) as u8;
                buf[at(2)] = ((2 * q3 + 3 * q2 + q1 + q0 + p0 + 4) >> 3) as u8;
            } else {
                buf[at(0)] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
            }
        }
    } else if bs < 4 {
        let tc = tc0[bs as usize - 1] as i32 + 1;
        let delta = ((((q0 - p0) << 2) + (p1 - q1) + 4) >> 3).clamp(-tc, tc);
        buf[at(-1)] = clip(p0 + delta);
        buf[at(0)] = clip(q0 - delta);
    } else {
        buf[at(-1)] = ((2 * p1 + p0 + q1 + 2) >> 2) as u8;
        buf[at(0)] = ((2 * q1 + q0 + p1 + 2) >> 2) as u8;
    }
}

// ---- motion compensation ---------------------------------------------------------------------

/// Luma prediction of a bw x bw block at (x, y) + mv (quarter samples), picture edges
/// extended. `out` has stride bw.
#[allow(clippy::too_many_arguments)]
fn luma_mc(src: &[u8], stride: usize, w: isize, h: isize, x: isize, y: isize, mv: [i16; 2], bw: usize, out: &mut [u8]) {
    let ix = x + (mv[0] as isize >> 2);
    let iy = y + (mv[1] as isize >> 2);
    let fx = (mv[0] & 3) as usize;
    let fy = (mv[1] & 3) as usize;
    let inside = ix >= 2 && iy >= 2 && ix + bw as isize + 3 <= w && iy + bw as isize + 3 <= h;
    if fx == 0 && fy == 0 {
        for r in 0..bw {
            if inside {
                let o = (iy as usize + r) * stride + ix as usize;
                out[r * bw..r * bw + bw].copy_from_slice(&src[o..o + bw]);
            } else {
                let yy = (iy + r as isize).clamp(0, h - 1) as usize;
                for c in 0..bw {
                    out[r * bw + c] = src[yy * stride + (ix + c as isize).clamp(0, w - 1) as usize];
                }
            }
        }
        return;
    }
    // Window of (bw + 5) x (bw + 5) samples starting at (ix - 2, iy - 2).
    let ww = bw + 5;
    let mut win = [0i32; 21 * 21];
    for r in 0..ww {
        if inside {
            let o = (iy as usize + r - 2) * stride + ix as usize - 2;
            for (c, v) in src[o..o + ww].iter().enumerate() {
                win[r * ww + c] = *v as i32;
            }
        } else {
            let yy = (iy + r as isize - 2).clamp(0, h - 1) as usize;
            for c in 0..ww {
                win[r * ww + c] = src[yy * stride + (ix + c as isize - 2).clamp(0, w - 1) as usize] as i32;
            }
        }
    }
    #[inline(always)]
    fn tap(a: i32, b: i32, c: i32, d: i32, e: i32, f: i32) -> i32 {
        (a + f) - 5 * (b + e) + 20 * (c + d)
    }
    const S: usize = 17;
    // Planes in output coordinates, stride S: G (full), B (horizontal half), H (vertical
    // half), J (centre). B is kept raw for J.
    let need_b = fx != 0;
    let need_h = fy != 0 && fx != 2;
    let need_j = (fx == 2 && fy != 0) || (fy == 2 && fx != 0);
    let mut braw = [0i32; 21 * S];
    let mut bp = [0i32; S * S];
    let mut hp = [0i32; S * S];
    let mut jp = [0i32; S * S];
    if need_b {
        // Rows -2..bw+3 of raw horizontal half samples (window rows), columns 0..bw.
        let rows = if need_j { ww } else { bw + 3 };
        let r0 = if need_j { 0 } else { 2 };
        for r in r0..rows {
            let row = &win[r * ww..r * ww + ww];
            for c in 0..bw {
                braw[r * S + c] = tap(row[c], row[c + 1], row[c + 2], row[c + 3], row[c + 4], row[c + 5]);
            }
        }
        for r in 0..=bw {
            if r + 2 >= ww {
                break;
            }
            for c in 0..bw {
                bp[r * S + c] = ((braw[(r + 2) * S + c] + 16) >> 5).clamp(0, 255);
            }
        }
    }
    if need_h {
        for r in 0..bw {
            for c in 0..=bw {
                let col = c + 2;
                let a = |k: usize| win[(r + k) * ww + col];
                hp[r * S + c] = ((tap(a(0), a(1), a(2), a(3), a(4), a(5)) + 16) >> 5).clamp(0, 255);
            }
        }
    }
    if need_j {
        for r in 0..bw {
            for c in 0..bw {
                let a = |k: usize| braw[(r + k) * S + c];
                jp[r * S + c] = ((tap(a(0), a(1), a(2), a(3), a(4), a(5)) + 512) >> 10).clamp(0, 255);
            }
        }
    }
    let g = |r: usize, c: usize| win[(r + 2) * ww + c + 2];
    // Each case: one plane, or the average of two (with row/column offsets).
    #[derive(Clone, Copy)]
    enum P {
        G,
        B,
        H,
        J,
    }
    let (a, ao, b, bo): (P, (usize, usize), Option<P>, (usize, usize)) = match (fx, fy) {
        (0, 1) => (P::G, (0, 0), Some(P::H), (0, 0)),
        (0, 2) => (P::H, (0, 0), None, (0, 0)),
        (0, 3) => (P::G, (1, 0), Some(P::H), (0, 0)),
        (1, 0) => (P::G, (0, 0), Some(P::B), (0, 0)),
        (2, 0) => (P::B, (0, 0), None, (0, 0)),
        (3, 0) => (P::G, (0, 1), Some(P::B), (0, 0)),
        (1, 1) => (P::B, (0, 0), Some(P::H), (0, 0)),
        (3, 1) => (P::B, (0, 0), Some(P::H), (0, 1)),
        (1, 3) => (P::H, (0, 0), Some(P::B), (1, 0)),
        (3, 3) => (P::H, (0, 1), Some(P::B), (1, 0)),
        (2, 2) => (P::J, (0, 0), None, (0, 0)),
        (2, 1) => (P::B, (0, 0), Some(P::J), (0, 0)),
        (2, 3) => (P::J, (0, 0), Some(P::B), (1, 0)),
        (1, 2) => (P::H, (0, 0), Some(P::J), (0, 0)),
        _ => (P::J, (0, 0), Some(P::H), (0, 1)),
    };
    let get = |p: P, r: usize, c: usize| -> i32 {
        match p {
            P::G => g(r, c),
            P::B => bp[r * S + c],
            P::H => hp[r * S + c],
            P::J => jp[r * S + c],
        }
    };
    match b {
        None => {
            for r in 0..bw {
                for c in 0..bw {
                    out[r * bw + c] = get(a, r + ao.0, c + ao.1) as u8;
                }
            }
        }
        Some(b) => {
            for r in 0..bw {
                for c in 0..bw {
                    out[r * bw + c] = ((get(a, r + ao.0, c + ao.1) + get(b, r + bo.0, c + bo.1) + 1) >> 1) as u8;
                }
            }
        }
    }
}

/// Chroma prediction (4:2:0) of a cw x cw block at chroma (x, y) for luma mv (eighth chroma
/// samples). `out` has stride cw.
#[allow(clippy::too_many_arguments)]
fn chroma_mc(src: &[u8], stride: usize, w: isize, h: isize, x: isize, y: isize, mv: [i16; 2], cw: usize, out: &mut [u8]) {
    let ix = x + (mv[0] as isize >> 3);
    let iy = y + (mv[1] as isize >> 3);
    let fx = (mv[0] & 7) as i32;
    let fy = (mv[1] & 7) as i32;
    let inside = ix >= 0 && iy >= 0 && ix + cw as isize + 1 <= w && iy + cw as isize + 1 <= h;
    let (wa, wb, wc, wd) = ((8 - fx) * (8 - fy), fx * (8 - fy), (8 - fx) * fy, fx * fy);
    for r in 0..cw as isize {
        for c in 0..cw as isize {
            let (a, b, cc, d) = if inside {
                let o = (iy + r) as usize * stride + (ix + c) as usize;
                (src[o] as i32, src[o + 1] as i32, src[o + stride] as i32, src[o + stride + 1] as i32)
            } else {
                let px = |cx: isize, ry: isize| src[ry.clamp(0, h - 1) as usize * stride + cx.clamp(0, w - 1) as usize] as i32;
                (px(ix + c, iy + r), px(ix + c + 1, iy + r), px(ix + c, iy + r + 1), px(ix + c + 1, iy + r + 1))
            };
            out[(r * cw as isize + c) as usize] = ((wa * a + wb * b + wc * cc + wd * d + 32) >> 6) as u8;
        }
    }
}

// ---- transforms and dequantisation -------------------------------------------------------------

pub(crate) fn idct4(d: &[i32; 16]) -> [i32; 16] {
    let mut t = [0i32; 16];
    for i in 0..4 {
        let (a, b, c, e) = (d[i * 4], d[i * 4 + 1], d[i * 4 + 2], d[i * 4 + 3]);
        let (p, q, r, s) = (a + c, a - c, (b >> 1) - e, b + (e >> 1));
        t[i * 4] = p + s;
        t[i * 4 + 1] = q + r;
        t[i * 4 + 2] = q - r;
        t[i * 4 + 3] = p - s;
    }
    let mut out = [0i32; 16];
    for j in 0..4 {
        let (a, b, c, e) = (t[j], t[4 + j], t[8 + j], t[12 + j]);
        let (p, q, r, s) = (a + c, a - c, (b >> 1) - e, b + (e >> 1));
        out[j] = (p + s + 32) >> 6;
        out[4 + j] = (q + r + 32) >> 6;
        out[8 + j] = (q - r + 32) >> 6;
        out[12 + j] = (p - s + 32) >> 6;
    }
    out
}

fn idct8_1d(d: [i32; 8]) -> [i32; 8] {
    let a0 = d[0] + d[4];
    let a4 = d[0] - d[4];
    let a2 = (d[2] >> 1) - d[6];
    let a6 = d[2] + (d[6] >> 1);
    let b0 = a0 + a6;
    let b2 = a4 + a2;
    let b4 = a4 - a2;
    let b6 = a0 - a6;
    let a1 = -d[3] + d[5] - d[7] - (d[7] >> 1);
    let a3 = d[1] + d[7] - d[3] - (d[3] >> 1);
    let a5 = -d[1] + d[7] + d[5] + (d[5] >> 1);
    let a7 = d[3] + d[5] + d[1] + (d[1] >> 1);
    let b1 = a1 + (a7 >> 2);
    let b7 = a7 - (a1 >> 2);
    let b3 = a3 + (a5 >> 2);
    let b5 = (a3 >> 2) - a5;
    [b0 + b7, b2 + b5, b4 + b3, b6 + b1, b6 - b1, b4 - b3, b2 - b5, b0 - b7]
}

pub(crate) fn idct8(d: &[i32; 64]) -> [i32; 64] {
    let mut t = [0i32; 64];
    for i in 0..8 {
        let r = idct8_1d(core::array::from_fn(|k| d[i * 8 + k]));
        t[i * 8..i * 8 + 8].copy_from_slice(&r);
    }
    let mut out = [0i32; 64];
    for j in 0..8 {
        let c = idct8_1d(core::array::from_fn(|k| t[k * 8 + j]));
        for k in 0..8 {
            out[k * 8 + j] = (c[k] + 32) >> 6;
        }
    }
    out
}

#[derive(Clone)]
struct LevelScale {
    qp: [i32; 3],
    s4: [[i32; 16]; 6],
    s8: [[i32; 64]; 2],
}

impl LevelScale {
    fn new(pps: &Pps, qp: i32) -> LevelScale {
        let qpc = |c: usize| QPC[(qp + pps.cqp[c]).clamp(0, 51) as usize] as i32;
        let qps = [qp, qpc(0), qpc(1)];
        let mut s4 = [[0i32; 16]; 6];
        // Lists: 0-2 intra Y/Cb/Cr, 3-5 inter Y/Cb/Cr.
        for (li, list) in s4.iter_mut().enumerate() {
            let q = qps[li % 3];
            for k in 0..16 {
                let pos = ZIGZAG[k];
                let (i, j) = (pos / 4, pos % 4);
                let cls = if i % 2 == 0 && j % 2 == 0 {
                    0
                } else if i % 2 == 1 && j % 2 == 1 {
                    1
                } else {
                    2
                };
                list[pos] = pps.scaling4[li][k] as i32 * V4[(q % 6) as usize][cls];
            }
        }
        let mut s8 = [[0i32; 64]; 2];
        for (li, list) in s8.iter_mut().enumerate() {
            for k in 0..64 {
                let pos = ZIGZAG8[k];
                let (i, j) = (pos / 8, pos % 8);
                let cls = if i % 4 == 0 && j % 4 == 0 {
                    0
                } else if i % 2 == 1 && j % 2 == 1 {
                    1
                } else if i % 4 == 2 && j % 4 == 2 {
                    2
                } else if (i % 4 == 0 && j % 2 == 1) || (i % 2 == 1 && j % 4 == 0) {
                    3
                } else if (i % 4 == 0 && j % 4 == 2) || (i % 4 == 2 && j % 4 == 0) {
                    4
                } else {
                    5
                };
                list[pos] = pps.scaling8[li][k] as i32 * V8[(qp % 6) as usize][cls];
            }
        }
        LevelScale { qp: qps, s4, s8 }
    }

    /// Dequantise a 4x4 block (raster). `skip_dc`: DC handled separately.
    fn dequant4(&self, c: &[i32; 16], comp: usize, inter: bool, skip_dc: bool) -> [i32; 16] {
        let q = self.qp[comp];
        let ls = &self.s4[comp + if inter { 3 } else { 0 }];
        let mut out = [0i32; 16];
        for k in (skip_dc as usize)..16 {
            if c[k] == 0 {
                continue;
            }
            out[k] = if q >= 24 { (c[k] * ls[k]) << (q / 6 - 4) } else { (c[k] * ls[k] + (1 << (3 - q / 6))) >> (4 - q / 6) };
        }
        out
    }

    fn dequant8(&self, c: &[i32; 64], inter: bool) -> [i32; 64] {
        let q = self.qp[0];
        let ls = &self.s8[inter as usize];
        let mut out = [0i32; 64];
        for k in 0..64 {
            if c[k] == 0 {
                continue;
            }
            out[k] = if q >= 36 { (c[k] * ls[k]) << (q / 6 - 6) } else { (c[k] * ls[k] + (1 << (5 - q / 6))) >> (6 - q / 6) };
        }
        out
    }

    /// Intra 16x16 DC: inverse Hadamard and scaling. Input/outputs raster by block position.
    fn luma_dc(&self, c: &[i32; 16], inter: bool) -> [i32; 16] {
        let q = self.qp[0];
        let mut t = [0i32; 16];
        for i in 0..4 {
            let (a, b, cc, d) = (c[i * 4], c[i * 4 + 1], c[i * 4 + 2], c[i * 4 + 3]);
            t[i * 4] = a + b + cc + d;
            t[i * 4 + 1] = a + b - cc - d;
            t[i * 4 + 2] = a - b - cc + d;
            t[i * 4 + 3] = a - b + cc - d;
        }
        let mut f = [0i32; 16];
        for j in 0..4 {
            let (a, b, cc, d) = (t[j], t[4 + j], t[8 + j], t[12 + j]);
            f[j] = a + b + cc + d;
            f[4 + j] = a + b - cc - d;
            f[8 + j] = a - b - cc + d;
            f[12 + j] = a - b + cc - d;
        }
        let ls = self.s4[if inter { 3 } else { 0 }][0];
        let mut out = [0i32; 16];
        for k in 0..16 {
            out[k] = if q >= 36 { (f[k] * ls) << (q / 6 - 6) } else { (f[k] * ls + (1 << (5 - q / 6))) >> (6 - q / 6) };
        }
        out
    }

    fn chroma_dc(&self, c: &[i32; 4], comp: usize, inter: bool) -> [i32; 4] {
        let q = self.qp[comp + 1];
        let f = [c[0] + c[1] + c[2] + c[3], c[0] - c[1] + c[2] - c[3], c[0] + c[1] - c[2] - c[3], c[0] - c[1] - c[2] + c[3]];
        let ls = self.s4[comp + 1 + if inter { 3 } else { 0 }][0];
        core::array::from_fn(|k| ((f[k] * ls) << (q / 6)) >> 5)
    }
}

// ---- optional timing (host tests only) ----------------------------------------------------

/// A clock (nanoseconds) the host test tool can install to see where time goes.
pub static mut CLOCK: Option<fn() -> u64> = None;
/// Time spent in: 0 parsing, 1 motion vectors, 2 motion compensation, 3 reconstruction,
/// 4 deblocking, 5 output.
pub static mut TIMES: [u64; 6] = [0; 6];

#[inline]
fn now() -> u64 {
    unsafe { (*core::ptr::addr_of!(CLOCK)).map(|c| c()).unwrap_or(0) }
}

#[inline]
fn add_time(k: usize, t0: u64) {
    unsafe {
        if (*core::ptr::addr_of!(CLOCK)).is_some() {
            (*core::ptr::addr_of_mut!(TIMES))[k] += now() - t0;
        }
    }
}

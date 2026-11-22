//! VP9 superblock partitions, block modes, motion vectors and coefficient tokens
//! (FFmpeg's vp9.c decode_sb, vp9block.c, vp9mvs.c).

use super::prob::cidx;
use super::tab::*;
use super::{Mv, Rac, Vp9Dec, FILTER_SWITCHABLE, TX_SWITCHABLE};

pub const BS_8X8: usize = 9;
pub const BS_8X4: usize = 10;
pub const BS_4X8: usize = 11;
pub const NEARESTMV: usize = 10;
pub const NEARMV: usize = 11;
pub const ZEROMV: usize = 12;
pub const NEWMV: usize = 13;

const PARTITION_TREE: [[i8; 2]; 3] = [[0, 1], [-1, 2], [-2, -3]];
const SEGMENTATION_TREE: [[i8; 2]; 7] = [[1, 2], [3, 4], [5, 6], [0, -1], [-2, -3], [-4, -5], [-6, -7]];
const INTRAMODE_TREE: [[i8; 2]; 9] = [[-2, 1], [-9, 2], [0, 3], [4, 6], [-1, 5], [-4, -5], [-3, 7], [-7, 8], [-6, -8]];
const INTER_MODE_TREE: [[i8; 2]; 3] = [[-12, 1], [-10, 2], [-11, -13]];
const FILTER_TREE: [[i8; 2]; 2] = [[0, 1], [-1, -2]];
const MV_JOINT_TREE: [[i8; 2]; 3] = [[0, 1], [-1, 2], [-2, -3]];
const MV_CLASS_TREE: [[i8; 2]; 10] = [[0, 1], [-1, 2], [3, 4], [-2, -3], [5, 6], [-4, -5], [-6, 7], [8, 9], [-7, -8], [-9, -10]];
const MV_FP_TREE: [[i8; 2]; 3] = [[0, 1], [-1, 2], [-2, -3]];
const FILTER_LUT: [usize; 3] = [1, 0, 2];
/// Transform type for each intra mode (0 DCT_DCT, 1 DCT_ADST, 2 ADST_DCT, 3 ADST_ADST).
pub const INTRA_TXFM_TYPE: [usize; 14] = [2, 1, 0, 0, 3, 2, 1, 2, 1, 3, 0, 0, 0, 0];

/// Block width/height in 4 px (t = 0) or 8 px (t = 1) units.
#[inline]
pub fn bwh(t: usize, bs: usize, i: usize) -> usize {
    BWH_TAB[(t * 13 + bs) * 2 + i] as usize
}

fn scans(tx: usize, txtp: usize) -> (&'static [i16], &'static [i16]) {
    match tx {
        0 => match txtp {
            1 => (&COL_SCAN_4X4, &COL_SCAN_4X4_NB),
            2 => (&ROW_SCAN_4X4, &ROW_SCAN_4X4_NB),
            _ => (&DEFAULT_SCAN_4X4, &DEFAULT_SCAN_4X4_NB),
        },
        1 => match txtp {
            1 => (&COL_SCAN_8X8, &COL_SCAN_8X8_NB),
            2 => (&ROW_SCAN_8X8, &ROW_SCAN_8X8_NB),
            _ => (&DEFAULT_SCAN_8X8, &DEFAULT_SCAN_8X8_NB),
        },
        2 => match txtp {
            1 => (&COL_SCAN_16X16, &COL_SCAN_16X16_NB),
            2 => (&ROW_SCAN_16X16, &ROW_SCAN_16X16_NB),
            _ => (&DEFAULT_SCAN_16X16, &DEFAULT_SCAN_16X16_NB),
        },
        3 => (&DEFAULT_SCAN_32X32, &DEFAULT_SCAN_32X32_NB),
        _ => (&DEFAULT_SCAN_4X4, &DEFAULT_SCAN_4X4_NB),
    }
}

/// One transform block's tokens. Returns the end of block (number of coefficients read).
#[allow(clippy::too_many_arguments)]
fn decode_coeffs_b(c: &mut Rac, coef: &mut [i16], n_coeffs: usize, is32: bool, cnt: &mut [[u32; 3]], eobc: &mut [[u32; 2]], probs: &[u8], mut nnz: usize, scan: &[i16], nb: &[i16], band_counts: &[usize; 6], qmul: [i16; 2]) -> usize {
    let mut i = 0usize;
    let mut band = 0usize;
    let mut band_left = band_counts[0];
    let mut cache = [0u8; 1024];
    let tp = |band: usize, nnz: usize| (band * 6 + nnz) * 11;
    let mut t = tp(0, nnz);
    'outer: loop {
        let val = c.prob(probs[t]);
        eobc[band * 6 + nnz][val as usize] += 1;
        if !val {
            break;
        }
        loop {
            // skip_eob
            if !c.prob(probs[t + 1]) {
                cnt[band * 6 + nnz][0] += 1;
                band_left -= 1;
                if band_left == 0 {
                    band += 1;
                    band_left = band_counts[band.min(5)];
                }
                cache[scan[i] as usize] = 0;
                nnz = (1 + cache[nb[i * 2] as usize] as usize + cache[nb[i * 2 + 1] as usize] as usize) >> 1;
                t = tp(band, nnz);
                i += 1;
                if i == n_coeffs {
                    break 'outer;
                }
                continue;
            }
            break;
        }
        let rc = scan[i] as usize;
        let val: i32;
        if !c.prob(probs[t + 2]) {
            cnt[band * 6 + nnz][1] += 1;
            val = 1;
            cache[rc] = 1;
        } else {
            cnt[band * 6 + nnz][2] += 1;
            if !c.prob(probs[t + 3]) {
                if !c.prob(probs[t + 4]) {
                    val = 2;
                    cache[rc] = 2;
                } else {
                    val = 3 + c.prob(probs[t + 5]) as i32;
                    cache[rc] = 3;
                }
            } else if !c.prob(probs[t + 6]) {
                cache[rc] = 4;
                if !c.prob(probs[t + 7]) {
                    val = c.prob(159) as i32 + 5;
                } else {
                    let mut v = ((c.prob(165) as i32) << 1) + 7;
                    v += c.prob(145) as i32;
                    val = v;
                }
            } else {
                cache[rc] = 5;
                if !c.prob(probs[t + 8]) {
                    if !c.prob(probs[t + 9]) {
                        let mut v = 11 + ((c.prob(173) as i32) << 2);
                        v += (c.prob(148) as i32) << 1;
                        v += c.prob(140) as i32;
                        val = v;
                    } else {
                        let mut v = 19 + ((c.prob(176) as i32) << 3);
                        v += (c.prob(155) as i32) << 2;
                        v += (c.prob(140) as i32) << 1;
                        v += c.prob(135) as i32;
                        val = v;
                    }
                } else if !c.prob(probs[t + 10]) {
                    let mut v = ((c.prob(180) as i32) << 4) + 35;
                    v += (c.prob(157) as i32) << 3;
                    v += (c.prob(141) as i32) << 2;
                    v += (c.prob(134) as i32) << 1;
                    v += c.prob(130) as i32;
                    val = v;
                } else {
                    let mut v = 67;
                    for (k, p) in [254u8, 254, 254, 252, 249, 243, 230, 196, 177, 153, 140, 133, 130, 129].iter().enumerate() {
                        v += (c.prob(*p) as i32) << (13 - k);
                    }
                    val = v;
                }
            }
        }
        band_left -= 1;
        if band_left == 0 {
            band += 1;
            band_left = band_counts[band.min(5)];
        }
        let q = qmul[(i != 0) as usize] as i32;
        let sv = if c.bit() { -val } else { val };
        coef[rc] = if is32 { (sv.wrapping_mul(q)) / 2 } else { sv.wrapping_mul(q) } as i16;
        nnz = (1 + cache[nb[i * 2] as usize] as usize + cache[nb[i * 2 + 1] as usize] as usize) >> 1;
        t = tp(band, nnz);
        i += 1;
        if i >= n_coeffs {
            break;
        }
    }
    i
}

impl Vp9Dec {
    pub(super) fn decode_sb(&mut self, row: usize, col: usize, lf: usize, bl: usize) {
        let c = ((self.above_partition[col] >> (3 - bl)) & 1) as usize | ((((self.left_partition[row & 7] >> (3 - bl)) & 1) as usize) << 1);
        let kf = self.h.keyframe || self.h.intraonly;
        let p: [u8; 3] = if kf {
            let o = (bl * 4 + c) * 3;
            [KF_PARTITION_PROBS[o], KF_PARTITION_PROBS[o + 1], KF_PARTITION_PROBS[o + 2]]
        } else {
            self.p.partition[bl][c]
        };
        let hbs = 4 >> bl;
        let bp;
        if bl == 3 {
            bp = self.tiles[self.tc].tree(&PARTITION_TREE, &p);
            self.decode_block(row, col, lf, bl, bp);
        } else if col + hbs < self.cols {
            if row + hbs < self.rows {
                bp = self.tiles[self.tc].tree(&PARTITION_TREE, &p);
                match bp {
                    0 => self.decode_block(row, col, lf, bl, bp),
                    1 => {
                        self.decode_block(row, col, lf, bl, bp);
                        self.decode_block(row + hbs, col, lf, bl, bp);
                    }
                    2 => {
                        self.decode_block(row, col, lf, bl, bp);
                        self.decode_block(row, col + hbs, lf, bl, bp);
                    }
                    _ => {
                        self.decode_sb(row, col, lf, bl + 1);
                        self.decode_sb(row, col + hbs, lf, bl + 1);
                        self.decode_sb(row + hbs, col, lf, bl + 1);
                        self.decode_sb(row + hbs, col + hbs, lf, bl + 1);
                    }
                }
            } else if self.tiles[self.tc].prob(p[1]) {
                bp = 3;
                self.decode_sb(row, col, lf, bl + 1);
                self.decode_sb(row, col + hbs, lf, bl + 1);
            } else {
                bp = 1;
                self.decode_block(row, col, lf, bl, bp);
            }
        } else if row + hbs < self.rows {
            if self.tiles[self.tc].prob(p[2]) {
                bp = 3;
                self.decode_sb(row, col, lf, bl + 1);
                self.decode_sb(row + hbs, col, lf, bl + 1);
            } else {
                bp = 2;
                self.decode_block(row, col, lf, bl, bp);
            }
        } else {
            bp = 3;
            self.decode_sb(row, col, lf, bl + 1);
        }
        self.counts.partition[bl][c][bp] += 1;
    }

    fn decode_block(&mut self, row: usize, col: usize, lf: usize, bl: usize, bp: usize) {
        let bs = bl * 3 + bp;
        let w4 = bwh(1, bs, 0);
        let h4 = bwh(1, bs, 1);
        self.row = row;
        self.col = col;
        self.min_mv = (-(128 + col as i32 * 64), -(128 + row as i32 * 64));
        self.max_mv = (128 + (self.cols as i32 - col as i32 - w4 as i32) * 64, 128 + (self.rows as i32 - row as i32 - h4 as i32) * 64);
        self.b.bs = bs;
        self.b.bl = bl;
        self.b.bp = bp;
        self.decode_mode();
        let b = &mut self.b;
        b.uvtx = b.tx - ((w4 * 2 == (1 << b.tx)) || (h4 * 2 == (1 << b.tx))) as usize;
        if !self.b.skip {
            let has_coeffs = self.decode_coeffs();
            if !has_coeffs && self.b.bs <= BS_8X8 && !self.b.intra {
                self.b.skip = true;
                self.above_skip[col..col + w4].iter_mut().for_each(|v| *v = 1);
                self.left_skip[row & 7..(row & 7) + h4].iter_mut().for_each(|v| *v = 1);
            }
        } else {
            let r7 = row & 7;
            self.above_y_nnz[col * 2..col * 2 + w4 * 2].iter_mut().for_each(|v| *v = 0);
            for pl in 0..2 {
                self.above_uv_nnz[pl][col..col + w4].iter_mut().for_each(|v| *v = 0);
            }
            let n = (h4 * 2).min(16 - r7 * 2);
            self.left_y_nnz[r7 * 2..r7 * 2 + n].iter_mut().for_each(|v| *v = 0);
            for pl in 0..2 {
                let n = h4.min(16 - r7);
                self.left_uv_nnz[pl][r7..r7 + n].iter_mut().for_each(|v| *v = 0);
            }
        }
        if self.b.intra {
            self.intra_recon();
        } else {
            self.inter_recon();
        }
        // Loop filter level and edges.
        let b = self.b;
        if self.h.filter_level != 0 {
            let lvl = self.h.feat[b.seg_id].lflvl[if b.intra { 0 } else { b.rf[0] + 1 }][(b.mode[3] != ZEROMV) as usize];
            if lvl > 0 {
                let x_end = (self.cols - col).min(w4);
                let y_end = (self.rows - row).min(h4);
                let skip_inter = !b.intra && b.skip;
                let (c7, r7) = (col & 7, row & 7);
                let f = &mut self.lflvl[lf];
                for y in 0..h4 {
                    for x in 0..w4 {
                        if (r7 + y) < 8 && (c7 + x) < 8 {
                            f.level[(r7 + y) * 8 + c7 + x] = lvl;
                        }
                    }
                }
                mask_edges(&mut f.mask[0], 0, 0, r7, c7, x_end, y_end, 0, 0, b.tx, skip_inter);
                let col_end = if self.cols & 1 != 0 && col + w4 >= self.cols { self.cols & 7 } else { 0 };
                let row_end = if self.rows & 1 != 0 && row + h4 >= self.rows { self.rows & 7 } else { 0 };
                mask_edges(&mut f.mask[1], 1, 1, r7, c7, x_end, y_end, col_end, row_end, b.uvtx, skip_inter);
            }
        }
    }

    fn decode_mode(&mut self) {
        const LEFT_CTX: [u8; 13] = [0x0, 0x8, 0x0, 0x8, 0xc, 0x8, 0xc, 0xe, 0xc, 0xe, 0xf, 0xe, 0xf];
        const ABOVE_CTX: [u8; 13] = [0x0, 0x0, 0x8, 0x8, 0x8, 0xc, 0xc, 0xc, 0xe, 0xe, 0xe, 0xf, 0xf];
        const MAX_TX: [usize; 13] = [3, 3, 3, 3, 2, 2, 2, 1, 1, 1, 0, 0, 0];
        let (row, col) = (self.row, self.col);
        let r7 = row & 7;
        let bs = self.b.bs;
        let max_tx = MAX_TX[bs];
        let bw4 = bwh(1, bs, 0);
        let bh4 = bwh(1, bs, 1);
        let w4 = (self.cols - col).min(bw4);
        let h4 = (self.rows - row).min(bh4);
        let have_a = row > 0;
        let have_l = col > self.tile_col_start;
        let h = self.h;
        let kf = h.keyframe || h.intraonly;
        let c = &mut self.tiles[self.tc];
        let b = &mut self.b;

        // Segment.
        if !h.seg_enabled {
            b.seg_id = 0;
        } else if kf {
            b.seg_id = if h.seg_update_map { c.tree(&SEGMENTATION_TREE, &h.seg_prob) } else { 0 };
        } else if !h.seg_update_map || (h.seg_temporal && c.prob(h.seg_pred_prob[(self.above_segpred[col] + self.left_segpred[r7]) as usize])) {
            if !h.errorres {
                if let Some(rs) = self.segmap_ref.as_ref() {
                    let mut pred = 8u8;
                    for y in 0..h4 {
                        let base = (y + row) * 8 * self.sb_cols + col;
                        for x in 0..w4 {
                            pred = pred.min(rs.seg[base + x]);
                        }
                    }
                    b.seg_id = pred.min(7) as usize;
                } else {
                    b.seg_id = 0;
                }
            } else {
                b.seg_id = 0;
            }
            self.above_segpred[col..col + w4].iter_mut().for_each(|v| *v = 1);
            self.left_segpred[r7..r7 + h4].iter_mut().for_each(|v| *v = 1);
        } else {
            b.seg_id = c.tree(&SEGMENTATION_TREE, &h.seg_prob);
            self.above_segpred[col..col + w4].iter_mut().for_each(|v| *v = 0);
            self.left_segpred[r7..r7 + h4].iter_mut().for_each(|v| *v = 0);
        }
        if h.seg_enabled && (h.seg_update_map || kf) {
            let cur = self.cur.as_mut().unwrap();
            let s8 = 8 * self.sb_cols;
            for y in 0..bh4 {
                let o = (row + y) * s8 + col;
                if o + bw4 <= cur.seg.len() {
                    cur.seg[o..o + bw4].iter_mut().for_each(|v| *v = b.seg_id as u8);
                }
            }
        }
        let feat = h.feat[b.seg_id];
        b.skip = h.seg_enabled && feat.skip_enabled;
        if !b.skip {
            let ctx = (self.left_skip[r7] + self.above_skip[col]) as usize;
            b.skip = c.prob(self.p.skip[ctx]);
            self.counts.skip[ctx][b.skip as usize] += 1;
        }
        if kf {
            b.intra = true;
        } else if h.seg_enabled && feat.ref_enabled {
            b.intra = feat.ref_val == 0;
        } else {
            let ctx = if have_a && have_l {
                let v = (self.above_intra[col] + self.left_intra[r7]) as usize;
                v + (v == 2) as usize
            } else if have_a {
                2 * self.above_intra[col] as usize
            } else if have_l {
                2 * self.left_intra[r7] as usize
            } else {
                0
            };
            let bit = c.prob(self.p.intra[ctx]);
            self.counts.intra[ctx][bit as usize] += 1;
            b.intra = !bit;
        }
        if (b.intra || !b.skip) && h.txfmmode == TX_SWITCHABLE {
            let ctx = if have_a {
                if have_l {
                    let a = if self.above_skip[col] != 0 { max_tx } else { self.above_txfm[col] as usize };
                    let l = if self.left_skip[r7] != 0 { max_tx } else { self.left_txfm[r7] as usize };
                    (a + l > max_tx) as usize
                } else if self.above_skip[col] != 0 {
                    1
                } else {
                    (self.above_txfm[col] as usize * 2 > max_tx) as usize
                }
            } else if have_l {
                if self.left_skip[r7] != 0 {
                    1
                } else {
                    (self.left_txfm[r7] as usize * 2 > max_tx) as usize
                }
            } else {
                1
            };
            let p = &self.p;
            match max_tx {
                3 => {
                    b.tx = c.prob(p.tx32p[ctx][0]) as usize;
                    if b.tx != 0 {
                        b.tx += c.prob(p.tx32p[ctx][1]) as usize;
                        if b.tx == 2 {
                            b.tx += c.prob(p.tx32p[ctx][2]) as usize;
                        }
                    }
                    self.counts.tx32p[ctx][b.tx] += 1;
                }
                2 => {
                    b.tx = c.prob(p.tx16p[ctx][0]) as usize;
                    if b.tx != 0 {
                        b.tx += c.prob(p.tx16p[ctx][1]) as usize;
                    }
                    self.counts.tx16p[ctx][b.tx] += 1;
                }
                1 => {
                    b.tx = c.prob(p.tx8p[ctx]) as usize;
                    self.counts.tx8p[ctx][b.tx] += 1;
                }
                _ => b.tx = 0,
            }
        } else {
            b.tx = max_tx.min(h.txfmmode as usize);
        }

        let mut filter_id = 0usize;
        let mut vref = 0usize;
        if kf {
            let a = col * 2;
            let l = r7 << 1;
            b.comp = false;
            let kp = |a: u8, l: u8| -> &'static [u8] {
                let o = (a as usize * 10 + l as usize) * 9;
                &KF_YMODE_PROBS[o..o + 9]
            };
            if bs > BS_8X8 {
                b.mode[0] = c.tree(&INTRAMODE_TREE, kp(self.above_mode[a], self.left_mode[l]));
                self.above_mode[a] = b.mode[0] as u8;
                if bs != BS_8X4 {
                    b.mode[1] = c.tree(&INTRAMODE_TREE, kp(self.above_mode[a + 1], b.mode[0] as u8));
                } else {
                    b.mode[1] = b.mode[0];
                }
                self.left_mode[l] = b.mode[1] as u8;
                self.above_mode[a + 1] = b.mode[1] as u8;
                if bs != BS_4X8 {
                    b.mode[2] = c.tree(&INTRAMODE_TREE, kp(self.above_mode[a], self.left_mode[l + 1]));
                    self.above_mode[a] = b.mode[2] as u8;
                    if bs != BS_8X4 {
                        b.mode[3] = c.tree(&INTRAMODE_TREE, kp(self.above_mode[a + 1], b.mode[2] as u8));
                    } else {
                        b.mode[3] = b.mode[2];
                    }
                    self.left_mode[l + 1] = b.mode[3] as u8;
                    self.above_mode[a + 1] = b.mode[3] as u8;
                } else {
                    b.mode[2] = b.mode[0];
                    b.mode[3] = b.mode[1];
                    self.left_mode[l + 1] = b.mode[1] as u8;
                    self.above_mode[a + 1] = b.mode[1] as u8;
                }
            } else {
                b.mode[0] = c.tree(&INTRAMODE_TREE, kp(self.above_mode[a], self.left_mode[l]));
                b.mode = [b.mode[0]; 4];
                let n = bwh(0, bs, 0);
                self.above_mode[a..a + n].iter_mut().for_each(|v| *v = b.mode[0] as u8);
                let n = bwh(0, bs, 1).min(16 - l);
                self.left_mode[l..l + n].iter_mut().for_each(|v| *v = b.mode[0] as u8);
            }
            let o = b.mode[3] * 9;
            b.uvmode = c.tree(&INTRAMODE_TREE, &KF_UVMODE_PROBS[o..o + 9]);
        } else if b.intra {
            b.comp = false;
            let p = &self.p;
            if bs > BS_8X8 {
                b.mode[0] = c.tree(&INTRAMODE_TREE, &p.y_mode[0]);
                self.counts.y_mode[0][b.mode[0]] += 1;
                if bs != BS_8X4 {
                    b.mode[1] = c.tree(&INTRAMODE_TREE, &p.y_mode[0]);
                    self.counts.y_mode[0][b.mode[1]] += 1;
                } else {
                    b.mode[1] = b.mode[0];
                }
                if bs != BS_4X8 {
                    b.mode[2] = c.tree(&INTRAMODE_TREE, &p.y_mode[0]);
                    self.counts.y_mode[0][b.mode[2]] += 1;
                    if bs != BS_8X4 {
                        b.mode[3] = c.tree(&INTRAMODE_TREE, &p.y_mode[0]);
                        self.counts.y_mode[0][b.mode[3]] += 1;
                    } else {
                        b.mode[3] = b.mode[2];
                    }
                } else {
                    b.mode[2] = b.mode[0];
                    b.mode[3] = b.mode[1];
                }
            } else {
                const SIZE_GROUP: [usize; 10] = [3, 3, 3, 3, 2, 2, 2, 1, 1, 1];
                let sz = SIZE_GROUP[bs];
                b.mode[0] = c.tree(&INTRAMODE_TREE, &p.y_mode[sz]);
                b.mode = [b.mode[0]; 4];
                self.counts.y_mode[sz][b.mode[3]] += 1;
            }
            b.uvmode = c.tree(&INTRAMODE_TREE, &p.uv_mode[b.mode[3]]);
            self.counts.uv_mode[b.mode[3]][b.uvmode] += 1;
        } else {
            const INTER_MODE_CTX: [[u8; 14]; 14] = [
                [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
                [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
                [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
                [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
                [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
                [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
                [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
                [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
                [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
                [6, 6, 6, 6, 6, 6, 6, 6, 6, 6, 5, 5, 5, 5],
                [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 2, 2, 1, 3],
                [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 2, 2, 1, 3],
                [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 1, 1, 0, 3],
                [5, 5, 5, 5, 5, 5, 5, 5, 5, 5, 3, 3, 3, 4],
            ];
            let (ai, ac, ar) = (self.above_intra[col] != 0, self.above_comp[col] != 0, self.above_ref[col]);
            let (li, lc, lr) = (self.left_intra[r7] != 0, self.left_comp[r7] != 0, self.left_ref[r7]);
            let fix = h.fixcompref;
            let var = h.varcompref;
            if h.seg_enabled && feat.ref_enabled {
                b.comp = false;
                b.rf[0] = feat.ref_val as usize - 1;
            } else {
                if h.comppredmode != 2 {
                    b.comp = h.comppredmode == 1;
                } else {
                    let ctx = if have_a {
                        if have_l {
                            if ac && lc {
                                4
                            } else if ac {
                                2 + (li || lr == fix) as usize
                            } else if lc {
                                2 + (ai || ar == fix) as usize
                            } else {
                                ((!ai && ar == fix) ^ (!li && lr == fix)) as usize
                            }
                        } else if ac {
                            3
                        } else {
                            (!ai && ar == fix) as usize
                        }
                    } else if have_l {
                        if lc {
                            3
                        } else {
                            (!li && lr == fix) as usize
                        }
                    } else {
                        1
                    };
                    b.comp = c.prob(self.p.comp[ctx]);
                    self.counts.comp[ctx][b.comp as usize] += 1;
                }
                if b.comp {
                    let fix_idx = h.signbias[fix as usize] as usize;
                    let var_idx = 1 - fix_idx;
                    b.rf[fix_idx] = fix as usize;
                    let ctx = if have_a {
                        if have_l {
                            if ai {
                                if li {
                                    2
                                } else {
                                    1 + 2 * (lr != var[1]) as usize
                                }
                            } else if li {
                                1 + 2 * (ar != var[1]) as usize
                            } else {
                                let (refl, refa) = (lr, ar);
                                if refl == refa && refa == var[1] {
                                    0
                                } else if !lc && !ac {
                                    if (refa == fix && refl == var[0]) || (refl == fix && refa == var[0]) {
                                        4
                                    } else if refa == refl {
                                        3
                                    } else {
                                        1
                                    }
                                } else if !lc {
                                    if refa == var[1] && refl != var[1] {
                                        1
                                    } else if refl == var[1] && refa != var[1] {
                                        2
                                    } else {
                                        4
                                    }
                                } else if !ac {
                                    if refl == var[1] && refa != var[1] {
                                        1
                                    } else if refa == var[1] && refl != var[1] {
                                        2
                                    } else {
                                        4
                                    }
                                } else if refl == refa {
                                    4
                                } else {
                                    2
                                }
                            }
                        } else if ai {
                            2
                        } else if ac {
                            4 * (ar != var[1]) as usize
                        } else {
                            3 * (ar != var[1]) as usize
                        }
                    } else if have_l {
                        if li {
                            2
                        } else if lc {
                            4 * (lr != var[1]) as usize
                        } else {
                            3 * (lr != var[1]) as usize
                        }
                    } else {
                        2
                    };
                    let bit = c.prob(self.p.comp_ref[ctx]);
                    b.rf[var_idx] = var[bit as usize] as usize;
                    self.counts.comp_ref[ctx][bit as usize] += 1;
                } else {
                    let ctx = if have_a && !ai {
                        if have_l && !li {
                            if lc {
                                if ac {
                                    1 + (fix == 0 || lr == 0 || ar == 0) as usize
                                } else {
                                    3 * (ar == 0) as usize + (fix == 0 || lr == 0) as usize
                                }
                            } else if ac {
                                3 * (lr == 0) as usize + (fix == 0 || ar == 0) as usize
                            } else {
                                2 * (lr == 0) as usize + 2 * (ar == 0) as usize
                            }
                        } else if ai {
                            2
                        } else if ac {
                            1 + (fix == 0 || ar == 0) as usize
                        } else {
                            4 * (ar == 0) as usize
                        }
                    } else if have_l && !li {
                        if li {
                            2
                        } else if lc {
                            1 + (fix == 0 || lr == 0) as usize
                        } else {
                            4 * (lr == 0) as usize
                        }
                    } else {
                        2
                    };
                    let bit = c.prob(self.p.single_ref[ctx][0]);
                    self.counts.single_ref[ctx][0][bit as usize] += 1;
                    if !bit {
                        b.rf[0] = 0;
                    } else {
                        let ctx = if have_a {
                            if have_l {
                                if li {
                                    if ai {
                                        2
                                    } else if ac {
                                        1 + 2 * (fix == 1 || ar == 1) as usize
                                    } else if ar == 0 {
                                        3
                                    } else {
                                        4 * (ar == 1) as usize
                                    }
                                } else if ai {
                                    if li {
                                        2
                                    } else if lc {
                                        1 + 2 * (fix == 1 || lr == 1) as usize
                                    } else if lr == 0 {
                                        3
                                    } else {
                                        4 * (lr == 1) as usize
                                    }
                                } else if ac {
                                    if lc {
                                        if lr == ar {
                                            3 * (fix == 1 || lr == 1) as usize
                                        } else {
                                            2
                                        }
                                    } else if lr == 0 {
                                        1 + 2 * (fix == 1 || ar == 1) as usize
                                    } else {
                                        3 * (lr == 1) as usize + (fix == 1 || ar == 1) as usize
                                    }
                                } else if lc {
                                    if ar == 0 {
                                        1 + 2 * (fix == 1 || lr == 1) as usize
                                    } else {
                                        3 * (ar == 1) as usize + (fix == 1 || lr == 1) as usize
                                    }
                                } else if ar == 0 {
                                    if lr == 0 {
                                        3
                                    } else {
                                        4 * (lr == 1) as usize
                                    }
                                } else if lr == 0 {
                                    4 * (ar == 1) as usize
                                } else {
                                    2 * (lr == 1) as usize + 2 * (ar == 1) as usize
                                }
                            } else if ai || (!ac && ar == 0) {
                                2
                            } else if ac {
                                3 * (fix == 1 || ar == 1) as usize
                            } else {
                                4 * (ar == 1) as usize
                            }
                        } else if have_l {
                            if li || (!lc && lr == 0) {
                                2
                            } else if lc {
                                3 * (fix == 1 || lr == 1) as usize
                            } else {
                                4 * (lr == 1) as usize
                            }
                        } else {
                            2
                        };
                        let bit = c.prob(self.p.single_ref[ctx][1]);
                        self.counts.single_ref[ctx][1][bit as usize] += 1;
                        b.rf[0] = 1 + bit as usize;
                    }
                }
            }
            if bs <= BS_8X8 {
                if h.seg_enabled && feat.skip_enabled {
                    b.mode = [ZEROMV; 4];
                } else {
                    const OFF: [usize; 10] = [3, 0, 0, 1, 0, 0, 0, 0, 0, 0];
                    let ctx = INTER_MODE_CTX[self.above_mode[col + OFF[bs]] as usize][self.left_mode[r7 + OFF[bs]] as usize] as usize;
                    b.mode[0] = c.tree(&INTER_MODE_TREE, &self.p.mv_mode[ctx]);
                    b.mode = [b.mode[0]; 4];
                    self.counts.mv_mode[ctx][b.mode[0] - 10] += 1;
                }
            }
            if h.filtermode == FILTER_SWITCHABLE {
                let ctx = if have_a && self.above_mode[col] >= NEARESTMV as u8 {
                    if have_l && self.left_mode[r7] >= NEARESTMV as u8 {
                        if self.above_filter[col] == self.left_filter[r7] {
                            self.left_filter[r7] as usize
                        } else {
                            3
                        }
                    } else {
                        self.above_filter[col] as usize
                    }
                } else if have_l && self.left_mode[r7] >= NEARESTMV as u8 {
                    self.left_filter[r7] as usize
                } else {
                    3
                };
                filter_id = c.tree(&FILTER_TREE, &self.p.filter[ctx]);
                self.counts.filter[ctx][filter_id] += 1;
                b.filter = FILTER_LUT[filter_id];
            } else {
                b.filter = h.filtermode as usize;
            }
            if bs > BS_8X8 {
                let ctx = INTER_MODE_CTX[self.above_mode[col] as usize][self.left_mode[r7] as usize] as usize;
                let rd = |s: &mut Vp9Dec| -> usize {
                    let c = &mut s.tiles[s.tc];
                    let m = c.tree(&INTER_MODE_TREE, &s.p.mv_mode[ctx]);
                    s.counts.mv_mode[ctx][m - 10] += 1;
                    m
                };
                let m0 = rd(self);
                self.b.mode[0] = m0;
                let mut mv = self.b.mv[0];
                self.fill_mv(&mut mv, m0, 0);
                self.b.mv[0] = mv;
                if bs != BS_8X4 {
                    let m1 = rd(self);
                    self.b.mode[1] = m1;
                    let mut mv = self.b.mv[1];
                    self.fill_mv(&mut mv, m1, 1);
                    self.b.mv[1] = mv;
                } else {
                    self.b.mode[1] = self.b.mode[0];
                    self.b.mv[1] = self.b.mv[0];
                }
                if bs != BS_4X8 {
                    let m2 = rd(self);
                    self.b.mode[2] = m2;
                    let mut mv = self.b.mv[2];
                    self.fill_mv(&mut mv, m2, 2);
                    self.b.mv[2] = mv;
                    if bs != BS_8X4 {
                        let m3 = rd(self);
                        self.b.mode[3] = m3;
                        let mut mv = self.b.mv[3];
                        self.fill_mv(&mut mv, m3, 3);
                        self.b.mv[3] = mv;
                    } else {
                        self.b.mode[3] = self.b.mode[2];
                        self.b.mv[3] = self.b.mv[2];
                    }
                } else {
                    self.b.mode[2] = self.b.mode[0];
                    self.b.mv[2] = self.b.mv[0];
                    self.b.mode[3] = self.b.mode[1];
                    self.b.mv[3] = self.b.mv[1];
                }
            } else {
                let mut mv = self.b.mv[0];
                let m0 = self.b.mode[0];
                self.fill_mv(&mut mv, m0, -1);
                self.b.mv = [mv; 4];
            }
            let b = &self.b;
            vref = b.rf[if b.comp { h.signbias[h.varcompref[0] as usize] as usize } else { 0 }];
        }
        let b = self.b;
        let kf = h.keyframe || h.intraonly;
        let set = |v: &mut [u8], off: usize, n: usize, val: u8| v[off..off + n].iter_mut().for_each(|x| *x = val);
        let (aw, lh) = (bwh(1, bs, 0), bwh(1, bs, 1));
        set(&mut self.above_skip, col, aw, b.skip as u8);
        set(&mut self.above_txfm, col, aw, b.tx as u8);
        set(&mut self.above_partition, col, aw, ABOVE_CTX[bs]);
        let lh8 = lh.min(8 - r7);
        set(&mut self.left_skip, r7, lh8, b.skip as u8);
        set(&mut self.left_txfm, r7, lh8, b.tx as u8);
        set(&mut self.left_partition, r7, lh8, LEFT_CTX[bs]);
        if !kf {
            set(&mut self.above_intra, col, aw, b.intra as u8);
            set(&mut self.above_comp, col, aw, b.comp as u8);
            set(&mut self.above_mode, col, aw, b.mode[3] as u8);
            set(&mut self.left_intra, r7, lh8, b.intra as u8);
            set(&mut self.left_comp, r7, lh8, b.comp as u8);
            set(&mut self.left_mode, r7, lh8, b.mode[3] as u8);
            if !b.intra {
                set(&mut self.above_ref, col, aw, vref as u8);
                set(&mut self.left_ref, r7, lh8, vref as u8);
                if h.filtermode == FILTER_SWITCHABLE {
                    set(&mut self.above_filter, col, aw, filter_id as u8);
                    set(&mut self.left_filter, r7, lh8, filter_id as u8);
                }
            }
            if bs > BS_8X8 {
                let mv3 = b.mv[3];
                self.left_mv[r7 * 2] = b.mv[1];
                self.left_mv[r7 * 2 + 1] = mv3;
                self.above_mv[col * 2] = b.mv[2];
                self.above_mv[col * 2 + 1] = mv3;
            } else {
                let mv3 = b.mv[3];
                for n in 0..w4 * 2 {
                    self.above_mv[col * 2 + n] = mv3;
                }
                for n in 0..(h4 * 2).min(16 - r7 * 2) {
                    self.left_mv[r7 * 2 + n] = mv3;
                }
            }
        }
        let cur = self.cur.as_mut().unwrap();
        for y in 0..h4 {
            let o = (row + y) * self.sb_cols * 8 + col;
            for x in 0..w4 {
                let m = &mut cur.mv[o + x];
                if b.intra {
                    m.rf = [-1, -1];
                } else if b.comp {
                    m.rf = [b.rf[0] as i8, b.rf[1] as i8];
                    m.mv = b.mv[3];
                } else {
                    m.rf = [b.rf[0] as i8, -1];
                    m.mv[0] = b.mv[3][0];
                }
            }
        }
    }

    fn clamp_mv(&self, m: Mv) -> Mv {
        Mv { x: (m.x as i32).clamp(self.min_mv.0, self.max_mv.0) as i16, y: (m.y as i32).clamp(self.min_mv.1, self.max_mv.1) as i16 }
    }

    /// Candidate MV for reference `rf` (FFmpeg's find_ref_mvs, libvpx-exact).
    #[allow(unused_assignments)]
    fn find_ref_mvs(&self, rf: usize, z: usize, idx: bool, sb: i32) -> Mv {
        const OFFS: [[[i8; 2]; 8]; 13] = [
            [[3, -1], [-1, 3], [4, -1], [-1, 4], [-1, -1], [0, -1], [-1, 0], [6, -1]],
            [[0, -1], [-1, 0], [4, -1], [-1, 2], [-1, -1], [0, -3], [-3, 0], [2, -1]],
            [[-1, 0], [0, -1], [-1, 4], [2, -1], [-1, -1], [-3, 0], [0, -3], [-1, 2]],
            [[1, -1], [-1, 1], [2, -1], [-1, 2], [-1, -1], [0, -3], [-3, 0], [-3, -3]],
            [[0, -1], [-1, 0], [2, -1], [-1, -1], [-1, 1], [0, -3], [-3, 0], [-3, -3]],
            [[-1, 0], [0, -1], [-1, 2], [-1, -1], [1, -1], [-3, 0], [0, -3], [-3, -3]],
            [[0, -1], [-1, 0], [1, -1], [-1, 1], [-1, -1], [0, -3], [-3, 0], [-3, -3]],
            [[0, -1], [-1, 0], [1, -1], [-1, -1], [0, -2], [-2, 0], [-2, -1], [-1, -2]],
            [[-1, 0], [0, -1], [-1, 1], [-1, -1], [-2, 0], [0, -2], [-1, -2], [-2, -1]],
            [[0, -1], [-1, 0], [-1, -1], [0, -2], [-2, 0], [-1, -2], [-2, -1], [-2, -2]],
            [[0, -1], [-1, 0], [-1, -1], [0, -2], [-2, 0], [-1, -2], [-2, -1], [-2, -2]],
            [[0, -1], [-1, 0], [-1, -1], [0, -2], [-2, 0], [-1, -2], [-2, -1], [-2, -2]],
            [[0, -1], [-1, 0], [-1, -1], [0, -2], [-2, 0], [-1, -2], [-2, -1], [-2, -2]],
        ];
        const INVALID: Mv = Mv { x: i16::MIN, y: i16::MIN };
        let b = &self.b;
        let (row, col, r7) = (self.row, self.col, self.row & 7);
        let p = &OFFS[b.bs];
        let rf8 = rf as i8;
        let mut mem = INVALID;
        let mut mem_sub8x8 = INVALID;
        let cur = self.cur.as_ref().unwrap();
        let sbc8 = self.sb_cols * 8;

        // Returns Some(mv) when the search ends.
        macro_rules! ret_direct {
            ($m:expr) => {{
                let m: Mv = $m;
                if !idx {
                    return m;
                } else if mem == INVALID {
                    mem = m;
                } else if m != mem {
                    return m;
                }
            }};
        }
        macro_rules! ret_mv {
            ($m:expr) => {{
                let m: Mv = $m;
                if sb > 0 {
                    if mem_sub8x8 == INVALID {
                        let t = self.clamp_mv(m);
                        if t != mem {
                            return t;
                        }
                        mem_sub8x8 = m;
                    } else if mem_sub8x8 != m {
                        let t = self.clamp_mv(m);
                        return if t != mem { t } else { Mv::default() };
                    }
                } else if !idx {
                    return self.clamp_mv(m);
                } else if mem == INVALID {
                    mem = m;
                } else if m != mem {
                    return self.clamp_mv(m);
                }
            }};
        }
        macro_rules! ret_scale {
            ($m:expr, $scale:expr) => {{
                let m: Mv = $m;
                if $scale {
                    ret_mv!(Mv { x: m.x.wrapping_neg(), y: m.y.wrapping_neg() });
                } else {
                    ret_mv!(m);
                }
            }};
        }

        let mut i = 0;
        if sb >= 0 {
            if sb == 2 || sb == 1 {
                ret_direct!(b.mv[0][z]);
            } else if sb == 3 {
                ret_direct!(b.mv[2][z]);
                ret_direct!(b.mv[1][z]);
                ret_direct!(b.mv[0][z]);
            }
            if row > 0 {
                let m = &cur.mv[(row - 1) * sbc8 + col];
                if m.rf[0] == rf8 {
                    ret_mv!(self.above_mv[2 * col + (sb & 1) as usize][0]);
                } else if m.rf[1] == rf8 {
                    ret_mv!(self.above_mv[2 * col + (sb & 1) as usize][1]);
                }
            }
            if col > self.tile_col_start {
                let m = &cur.mv[row * sbc8 + col - 1];
                if m.rf[0] == rf8 {
                    ret_mv!(self.left_mv[2 * r7 + (sb >> 1) as usize][0]);
                } else if m.rf[1] == rf8 {
                    ret_mv!(self.left_mv[2 * r7 + (sb >> 1) as usize][1]);
                }
            }
            i = 2;
        }
        let inside = |c: i32, r: i32| c >= self.tile_col_start as i32 && c < self.cols as i32 && r >= 0 && r < self.rows as i32;
        while i < 8 {
            let c = p[i][0] as i32 + col as i32;
            let r = p[i][1] as i32 + row as i32;
            if inside(c, r) {
                let m = cur.mv[r as usize * sbc8 + c as usize];
                if m.rf[0] == rf8 {
                    ret_mv!(m.mv[0]);
                } else if m.rf[1] == rf8 {
                    ret_mv!(m.mv[1]);
                }
            }
            i += 1;
        }
        let prev = if self.h.use_last_frame_mvs { self.mvpair_ref.as_ref().map(|p| p.mv[row * sbc8 + col]) } else { None };
        if let Some(m) = prev {
            if m.rf[0] == rf8 {
                ret_mv!(m.mv[0]);
            } else if m.rf[1] == rf8 {
                ret_mv!(m.mv[1]);
            }
        }
        let sbias = |r: i8| self.h.signbias[r as usize];
        for i in 0..8 {
            let c = p[i][0] as i32 + col as i32;
            let r = p[i][1] as i32 + row as i32;
            if inside(c, r) {
                let m = cur.mv[r as usize * sbc8 + c as usize];
                if m.rf[0] != rf8 && m.rf[0] >= 0 {
                    ret_scale!(m.mv[0], sbias(m.rf[0]) != sbias(rf8));
                }
                if m.rf[1] != rf8 && m.rf[1] >= 0 && m.mv[0] != m.mv[1] {
                    ret_scale!(m.mv[1], sbias(m.rf[1]) != sbias(rf8));
                }
            }
        }
        if let Some(m) = prev {
            if m.rf[0] != rf8 && m.rf[0] >= 0 {
                ret_scale!(m.mv[0], sbias(m.rf[0]) != sbias(rf8));
            }
            if m.rf[1] != rf8 && m.rf[1] >= 0 && m.mv[0] != m.mv[1] {
                ret_scale!(m.mv[1], sbias(m.rf[1]) != sbias(rf8));
            }
        }
        let _ = mem_sub8x8;
        self.clamp_mv(Mv::default())
    }

    fn read_mv_component(&mut self, idx: usize, hp: bool) -> i32 {
        let c = &mut self.tiles[self.tc];
        let p = &self.p.mv_comp[idx];
        let cnt = &mut self.counts.mv_comp[idx];
        let sign = c.prob(p.sign);
        let cl = c.tree(&MV_CLASS_TREE, &p.classes);
        cnt.sign[sign as usize] += 1;
        cnt.classes[cl] += 1;
        let mut n: i32;
        if cl != 0 {
            n = 0;
            for m in 0..cl {
                let bit = c.prob(p.bits[m]);
                n |= (bit as i32) << m;
                cnt.bits[m][bit as usize] += 1;
            }
            n <<= 3;
            let bit = c.tree(&MV_FP_TREE, &p.fp);
            n |= (bit as i32) << 1;
            cnt.fp[bit] += 1;
            if hp {
                let bit = c.prob(p.hp);
                cnt.hp[bit as usize] += 1;
                n |= bit as i32;
            } else {
                n |= 1;
                cnt.hp[1] += 1;
            }
            n += 8 << cl;
        } else {
            n = c.prob(p.class0) as i32;
            cnt.class0[n as usize] += 1;
            let bit = c.tree(&MV_FP_TREE, &p.class0_fp[n as usize]);
            cnt.class0_fp[n as usize][bit] += 1;
            n = (n << 3) | ((bit as i32) << 1);
            if hp {
                let bit = c.prob(p.class0_hp);
                cnt.class0_hp[bit as usize] += 1;
                n |= bit as i32;
            } else {
                n |= 1;
                cnt.class0_hp[1] += 1;
            }
        }
        if sign {
            -(n + 1)
        } else {
            n + 1
        }
    }

    fn fill_mv(&mut self, mv: &mut [Mv; 2], mode: usize, sb: i32) {
        if mode == ZEROMV {
            *mv = [Mv::default(); 2];
            return;
        }
        let comp = self.b.comp;
        for z in 0..1 + comp as usize {
            let rf = self.b.rf[z];
            let mut m = self.find_ref_mvs(rf, z, mode == NEARMV, if mode == NEWMV { -1 } else { sb });
            let mut hp = false;
            if mode == NEWMV || sb == -1 {
                hp = self.h.highprecisionmvs && (m.x as i32).abs() < 64 && (m.y as i32).abs() < 64;
                if !hp {
                    if m.y & 1 != 0 {
                        m.y += if m.y < 0 { 1 } else { -1 };
                    }
                    if m.x & 1 != 0 {
                        m.x += if m.x < 0 { 1 } else { -1 };
                    }
                }
            }
            if mode == NEWMV {
                let c = &mut self.tiles[self.tc];
                let j = c.tree(&MV_JOINT_TREE, &self.p.mv_joint);
                self.counts.mv_joint[j] += 1;
                if j >= 2 {
                    m.y = (m.y as i32 + self.read_mv_component(0, hp)) as i16;
                }
                if j & 1 != 0 {
                    m.x = (m.x as i32 + self.read_mv_component(1, hp)) as i16;
                }
            }
            mv[z] = m;
        }
    }

    /// Coefficient tokens for the whole block; true if any are non-zero.
    fn decode_coeffs(&mut self) -> bool {
        const BAND_COUNTS: [[usize; 6]; 4] = [[1, 2, 3, 4, 3, 16 - 13], [1, 2, 3, 4, 11, 64 - 21], [1, 2, 3, 4, 11, 256 - 21], [1, 2, 3, 4, 11, 1024 - 21]];
        let b = self.b;
        let (row, col) = (self.row, self.col);
        let rf = !b.intra as usize;
        let mut w4 = bwh(1, b.bs, 0) << 1;
        let mut h4 = bwh(1, b.bs, 1) << 1;
        let mut end_x = (2 * (self.cols - col)).min(w4);
        let mut end_y = (2 * (self.rows - row)).min(h4);
        let qmul = self.h.feat[b.seg_id].qmul;
        let lossless = self.h.lossless;
        let tx = if lossless { 4 } else { b.tx };
        let mut total = false;
        let Vp9Dec { tiles, tc, coef, counts, block, uvblock, eob, uveob, above_y_nnz, left_y_nnz, above_uv_nnz, left_uv_nnz, .. } = self;
        let c = &mut tiles[*tc];

        // Luma.
        {
            let base = cidx(b.tx, 0, rf, 0, 0);
            let probs = &coef[base * 11..(base + 36) * 11];
            let (cnt, eobc) = (&mut counts.coef[base..base + 36], &mut counts.eob[base..base + 36]);
            let a = &mut above_y_nnz[col * 2..col * 2 + 16];
            let l = &mut left_y_nnz[(row & 7) * 2..];
            let step = 1usize << b.tx;
            merge(a, end_x, step);
            merge(l, end_y, step);
            let mut n = 0;
            let mut y = 0;
            while y < end_y {
                let mut x = 0;
                while x < end_x {
                    let mode = b.mode[if b.tx == 0 && b.bs > BS_8X8 { n } else { 0 }];
                    let txtp = INTRA_TXFM_TYPE[mode];
                    let (scan, nb) = scans(tx, txtp);
                    let ret = decode_coeffs_b(c, &mut block[16 * n..], 16 * step * step, b.tx == 3, cnt, eobc, probs, (a[x] + l[y]) as usize, scan, nb, &BAND_COUNTS[b.tx], qmul[0]);
                    a[x] = (ret != 0) as u8;
                    l[y] = (ret != 0) as u8;
                    total |= ret != 0;
                    eob[n] = ret as u16;
                    x += step;
                    n += step * step;
                }
                y += step;
            }
            splat(a, end_x, step, end_x == w4);
            splat(l, end_y, step, end_y == h4);
        }
        // Chroma.
        w4 >>= 1;
        end_x >>= 1;
        h4 >>= 1;
        end_y >>= 1;
        let base = cidx(b.uvtx, 1, rf, 0, 0);
        let probs = &coef[base * 11..(base + 36) * 11];
        let (scan, nb) = scans(if lossless { 4 } else { b.uvtx }, 0);
        let step = 1usize << b.uvtx;
        for pl in 0..2 {
            let (cnt, eobc) = (&mut counts.coef[base..base + 36], &mut counts.eob[base..base + 36]);
            let a = &mut above_uv_nnz[pl][col..col + 16];
            let l = &mut left_uv_nnz[pl][row & 7..];
            merge(a, end_x, step);
            merge(l, end_y, step);
            let mut n = 0;
            let mut y = 0;
            while y < end_y {
                let mut x = 0;
                while x < end_x {
                    let ret = decode_coeffs_b(c, &mut uvblock[pl][16 * n..], 16 * step * step, b.uvtx == 3, cnt, eobc, probs, (a[x] + l[y]) as usize, scan, nb, &BAND_COUNTS[b.uvtx], qmul[1]);
                    a[x] = (ret != 0) as u8;
                    l[y] = (ret != 0) as u8;
                    total |= ret != 0;
                    uveob[pl][n] = ret as u16;
                    x += step;
                    n += step * step;
                }
                y += step;
            }
            splat(a, end_x, step, end_x == w4);
            splat(l, end_y, step, end_y == h4);
        }
        total
    }
}

/// Combine the per-4px non-zero flags into one per transform block.
fn merge(la: &mut [u8], end: usize, step: usize) {
    if step == 1 {
        return;
    }
    let mut n = 0;
    while n < end {
        let any = la[n..(n + step).min(la.len())].iter().any(|&v| v != 0);
        la[n] = any as u8;
        n += step;
    }
}

fn splat(la: &mut [u8], end: usize, step: usize, full: bool) {
    if step == 1 {
        return;
    }
    let mut n = 0;
    if step == 2 {
        n = 1;
        while n < end {
            la[n] = la[n - 1];
            n += step;
        }
        return;
    }
    while n < end {
        let v = la[n];
        let cnt = if full { step - 1 } else { (end - n - 1).min(step - 1) };
        let lim = (n + 1 + cnt).min(la.len());
        la[n + 1..lim].iter_mut().for_each(|x| *x = v);
        n += step;
    }
}

/// Mark the loop filter edges of a block (FFmpeg's mask_edges).
#[allow(clippy::too_many_arguments)]
fn mask_edges(mask: &mut [[[u8; 4]; 8]; 2], ss_h: usize, ss_v: usize, r7: usize, c7: usize, mut w: usize, mut h: usize, col_end: usize, row_end: usize, tx: usize, skip_inter: bool) {
    const WIDE_COL: [u32; 2] = [0x11, 0x01];
    const WIDE_ROW: [usize; 2] = [0x03, 0x07];
    if tx == 0 && (ss_v | ss_h) != 0 {
        if h == ss_v {
            if r7 & 1 != 0 {
                return;
            }
            if row_end == 0 {
                h += 1;
            }
        }
        if w == ss_h {
            if c7 & 1 != 0 {
                return;
            }
            if col_end == 0 {
                w += 1;
            }
        }
    }
    let t = 1u32 << c7;
    let m_col = (t << w).wrapping_sub(t);
    if tx == 0 && !skip_inter {
        let m_row_8 = m_col & WIDE_COL[ss_h];
        let m_row_4 = m_col - m_row_8;
        for y in r7..h + r7 {
            if y >= 8 {
                break;
            }
            let col_mask_id = 2 - ((y & WIDE_ROW[ss_v]) == 0) as usize;
            mask[0][y][1] |= m_row_8 as u8;
            mask[0][y][2] |= m_row_4 as u8;
            if (ss_h & ss_v) != 0 && (col_end & 1) != 0 && (y & 1) != 0 {
                mask[1][y][col_mask_id] |= ((t << (w - 1)).wrapping_sub(t)) as u8;
            } else {
                mask[1][y][col_mask_id] |= m_col as u8;
            }
            if ss_h == 0 {
                mask[0][y][3] |= m_col as u8;
            }
            if ss_v == 0 {
                if ss_h != 0 && (col_end & 1) != 0 {
                    mask[1][y][3] |= ((t << (w - 1)).wrapping_sub(t)) as u8;
                } else {
                    mask[1][y][3] |= m_col as u8;
                }
            }
        }
    } else if !skip_inter {
        let mask_id = (tx == 1) as usize;
        const MASKS: [u32; 4] = [0xff, 0x55, 0x11, 0x01];
        let l2 = tx + ss_h - 1;
        let m_row = m_col & MASKS[l2];
        if ss_h != 0 && tx > 1 && (w ^ (w - 1)) == 1 {
            let m_row_16 = ((t << (w - 1)).wrapping_sub(t)) & MASKS[l2];
            let m_row_8 = m_row - m_row_16;
            for y in r7..(h + r7).min(8) {
                mask[0][y][0] |= m_row_16 as u8;
                mask[0][y][1] |= m_row_8 as u8;
            }
        } else {
            for y in r7..(h + r7).min(8) {
                mask[0][y][mask_id] |= m_row as u8;
            }
        }
        let l2 = tx + ss_v - 1;
        let step1d = 1 << l2;
        if ss_v != 0 && tx > 1 && (h ^ (h - 1)) == 1 {
            let mut y = r7;
            while y < h + r7 - 1 {
                if y < 8 {
                    mask[1][y][0] |= m_col as u8;
                }
                y += step1d;
            }
            if y - r7 == h - 1 && y < 8 {
                mask[1][y][1] |= m_col as u8;
            }
        } else {
            let mut y = r7;
            while y < h + r7 {
                if y < 8 {
                    mask[1][y][mask_id] |= m_col as u8;
                }
                y += step1d;
            }
        }
    } else if tx != 0 {
        let mask_id = ((tx == 1) || (h == ss_v)) as usize;
        mask[1][r7][mask_id] |= m_col as u8;
        let mask_id = ((tx == 1) || (w == ss_h)) as usize;
        for y in r7..(h + r7).min(8) {
            mask[0][y][mask_id] |= t as u8;
        }
    } else {
        let t8 = t & WIDE_COL[ss_h];
        let t4 = t - t8;
        for y in r7..(h + r7).min(8) {
            mask[0][y][2] |= t4 as u8;
            mask[0][y][1] |= t8 as u8;
        }
        mask[1][r7][2 - ((r7 & WIDE_ROW[ss_v]) == 0) as usize] |= m_col as u8;
    }
}

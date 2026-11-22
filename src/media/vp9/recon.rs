//! VP9 reconstruction: intra prediction edges, motion compensation, residuals
//! (FFmpeg's vp9recon.c and vp9_mc_template.c).

use super::block::{bwh, BS_4X8, BS_8X4, BS_8X8, INTRA_TXFM_TYPE};
use super::dsp::*;
use super::tab::SUBPEL_FILTERS_RSS;
use super::{Mv, Pic, Vp9Dec};

/// The 8-tap filter `f` (0 smooth, 1 regular, 2 sharp) for sixteenth position `frac`.
#[inline]
fn subpel(f: usize, frac: usize) -> &'static [i16] {
    // The table is stored regular, sharp, smooth (each block led by a stray number).
    let blk = [2, 0, 1][f];
    let o = 1 + blk * 129 + frac * 8;
    &SUBPEL_FILTERS_RSS[o..o + 8]
}

/// Which plane of a picture.
#[derive(Clone, Copy)]
enum Pl {
    Y,
    U,
    V,
}

fn plane(p: &Pic, pl: Pl) -> (&[u16], usize) {
    match pl {
        Pl::Y => (&p.y, p.stride),
        Pl::U => (&p.u, p.uv_stride),
        Pl::V => (&p.v, p.uv_stride),
    }
}

/// Predict a bw x bh block from `r` (picture size w x h in this plane) at integer position
/// (x, y) plus sixteenth fraction (mx, my), into `dst` (put, or average when `avg`).
#[allow(clippy::too_many_arguments)]
fn mc(dst: &mut [u16], doff: usize, ds: usize, r: &[u16], rs: usize, w: i32, h: i32, x: i32, y: i32, mx: usize, my: usize, bw: usize, bh: usize, filter: usize, avg: bool, pmax: i32) {
    // Source window: 3 before, 4 after when filtering.
    let (ex0, ex1) = if mx != 0 { (3, 4) } else { (0, 0) };
    let (ey0, ey1) = if my != 0 { (3, 4) } else { (0, 0) };
    let sw = bw + ex0 + ex1;
    let sh = bh + ey0 + ey1;
    let (sx, sy) = (x - ex0 as i32, y - ey0 as i32);
    let mut win = [0u16; 72 * 72];
    let inside = sx >= 0 && sy >= 0 && sx + sw as i32 <= w && sy + sh as i32 <= h;
    let (src, sst, so): (&[u16], usize, usize) = if inside {
        (r, rs, sy as usize * rs + sx as usize)
    } else {
        for j in 0..sh {
            let yy = (sy + j as i32).clamp(0, h - 1) as usize * rs;
            for i in 0..sw {
                win[j * 72 + i] = r[yy + (sx + i as i32).clamp(0, w - 1) as usize];
            }
        }
        (&win, 72, 0)
    };
    let put = |d: &mut u16, v: u16| *d = if avg { ((*d as u32 + v as u32 + 1) >> 1) as u16 } else { v };
    let base = so + ey0 * sst + ex0;
    if mx == 0 && my == 0 {
        for j in 0..bh {
            let s = &src[base + j * sst..base + j * sst + bw];
            let d = &mut dst[doff + j * ds..doff + j * ds + bw];
            for i in 0..bw {
                put(&mut d[i], s[i]);
            }
        }
        return;
    }
    if filter == 3 {
        // Bilinear.
        let bil = |a: u16, b: u16, f: usize| (a as i32 + ((f as i32 * (b as i32 - a as i32) + 8) >> 4)) as u16;
        if my == 0 {
            for j in 0..bh {
                for i in 0..bw {
                    let o = base + j * sst + i;
                    put(&mut dst[doff + j * ds + i], bil(src[o], src[o + 1], mx));
                }
            }
        } else if mx == 0 {
            for j in 0..bh {
                for i in 0..bw {
                    let o = base + j * sst + i;
                    put(&mut dst[doff + j * ds + i], bil(src[o], src[o + sst], my));
                }
            }
        } else {
            let mut tmp = [0u16; 64 * 65];
            for j in 0..bh + 1 {
                for i in 0..bw {
                    let o = base + j * sst + i;
                    tmp[j * 64 + i] = bil(src[o], src[o + 1], mx);
                }
            }
            for j in 0..bh {
                for i in 0..bw {
                    put(&mut dst[doff + j * ds + i], bil(tmp[j * 64 + i], tmp[(j + 1) * 64 + i], my));
                }
            }
        }
        return;
    }
    let tap = |s: &[u16], o: usize, st: usize, f: &[i16]| -> u16 {
        let mut a = 64i32;
        for k in 0..8 {
            a += f[k] as i32 * s[o + k * st - 3 * st] as i32;
        }
        (a >> 7).clamp(0, pmax) as u16
    };
    if my == 0 {
        let f = subpel(filter, mx);
        for j in 0..bh {
            for i in 0..bw {
                put(&mut dst[doff + j * ds + i], tap(src, base + j * sst + i, 1, f));
            }
        }
    } else if mx == 0 {
        let f = subpel(filter, my);
        for j in 0..bh {
            for i in 0..bw {
                put(&mut dst[doff + j * ds + i], tap(src, base + j * sst + i, sst, f));
            }
        }
    } else {
        let fx = subpel(filter, mx);
        let fy = subpel(filter, my);
        let mut tmp = [0u16; 64 * 71];
        let b0 = base - 3 * sst;
        for j in 0..bh + 7 {
            for i in 0..bw {
                tmp[j * 64 + i] = tap(src, b0 + j * sst + i, 1, fx);
            }
        }
        for j in 0..bh {
            for i in 0..bw {
                put(&mut dst[doff + j * ds + i], tap(&tmp, (j + 3) * 64 + i, 64, fy));
            }
        }
    }
}

/// Prediction from a reference of another size (FFmpeg's scaled 8-tap / bilinear MC): the
/// source position advances `dx`/`dy` sixteenths per output pixel. (x, y) and (mx, my) are
/// the integer and sixteenth parts of the first sample's position.
#[allow(clippy::too_many_arguments)]
fn mc_scaled(dst: &mut [u16], doff: usize, ds: usize, r: &[u16], rs: usize, w: i32, h: i32, x: i32, y: i32, mx: usize, my: usize, bw: usize, bh: usize, filter: usize, avg: bool, dx: usize, dy: usize, pmax: i32) {
    let refbw = ((bw - 1) * dx + mx) >> 4;
    let refbh = ((bh - 1) * dy + my) >> 4;
    let sw = refbw + 8;
    let sh = refbh + 8;
    // Window from (x - 3, y - 3), the picture's edges repeated beyond it.
    let mut win = alloc::vec![0u16; sw * sh];
    for j in 0..sh {
        let yy = (y - 3 + j as i32).clamp(0, h - 1) as usize * rs;
        for i in 0..sw {
            win[j * sw + i] = r[yy + (x - 3 + i as i32).clamp(0, w - 1) as usize];
        }
    }
    let put = |d: &mut u16, v: u16| *d = if avg { ((*d as u32 + v as u32 + 1) >> 1) as u16 } else { v };
    let mut tmp = alloc::vec![0u16; 64 * 136];
    if filter == 3 {
        let bil = |a: u16, b: u16, f: usize| (a as i32 + ((f as i32 * (b as i32 - a as i32) + 8) >> 4)) as u16;
        let tmp_h = (((bh - 1) * dy + my) >> 4) + 2;
        for j in 0..tmp_h {
            let (mut imx, mut ioff) = (mx, 0);
            for xx in 0..bw {
                let o = (j + 3) * sw + 3 + ioff;
                tmp[j * 64 + xx] = bil(win[o], win[o + 1], imx);
                imx += dx;
                ioff += imx >> 4;
                imx &= 15;
            }
        }
        let (mut tp, mut m) = (0, my);
        for jj in 0..bh {
            for xx in 0..bw {
                put(&mut dst[doff + jj * ds + xx], bil(tmp[tp + xx], tmp[tp + 64 + xx], m));
            }
            m += dy;
            tp += (m >> 4) * 64;
            m &= 15;
        }
        return;
    }
    let tmp_h = (((bh - 1) * dy + my) >> 4) + 8;
    for j in 0..tmp_h {
        let (mut imx, mut ioff) = (mx, 0);
        for xx in 0..bw {
            let f = subpel(filter, imx);
            let o = j * sw + 3 + ioff;
            let mut a = 64i32;
            for k in 0..8 {
                a += f[k] as i32 * win[o + k - 3] as i32;
            }
            tmp[j * 64 + xx] = (a >> 7).clamp(0, pmax) as u16;
            imx += dx;
            ioff += imx >> 4;
            imx &= 15;
        }
    }
    let (mut tp, mut m) = (64 * 3, my);
    for jj in 0..bh {
        let f = subpel(filter, m);
        for xx in 0..bw {
            let mut a = 64i32;
            for k in 0..8 {
                a += f[k] as i32 * tmp[tp + xx + k * 64 - 3 * 64] as i32;
            }
            put(&mut dst[doff + jj * ds + xx], (a >> 7).clamp(0, pmax) as u16);
        }
        m += dy;
        tp += (m >> 4) * 64;
        m &= 15;
    }
}

fn rounded_div(a: i32, b: i32) -> i32 {
    if a >= 0 {
        (a + (b >> 1)) / b
    } else {
        (a - (b >> 1)) / b
    }
}
fn avg2(a: Mv, b: Mv) -> Mv {
    Mv { x: rounded_div(a.x as i32 + b.x as i32, 2) as i16, y: rounded_div(a.y as i32 + b.y as i32, 2) as i16 }
}
fn avg4(a: Mv, b: Mv, c: Mv, d: Mv) -> Mv {
    Mv { x: rounded_div(a.x as i32 + b.x as i32 + c.x as i32 + d.x as i32, 4) as i16, y: rounded_div(a.y as i32 + b.y as i32 + c.y as i32 + d.y as i32, 4) as i16 }
}

impl Vp9Dec {
    /// Luma block: position (x, y) in pixels, mv in eighths.
    #[allow(clippy::too_many_arguments)]
    fn mc_luma(&mut self, rf: &Pic, ri: usize, x: usize, y: usize, mv: Mv, pw: usize, ph: usize, px: usize, py: usize, bw: usize, bh: usize, avg: bool) {
        let filter = self.b.filter;
        let pmax = (1i32 << self.bpp) - 1;
        let (r, rs) = plane(rf, Pl::Y);
        let scale = self.mvscale[ri];
        let step = self.mvstep[ri];
        let cols = self.cols as i32;
        let cur = self.cur.as_mut().unwrap();
        let ds = cur.stride;
        if scale == [0, 0] {
            let xx = x as i32 + (mv.x as i32 >> 3);
            let yy = y as i32 + (mv.y as i32 >> 3);
            let mx = ((mv.x as i32 & 7) << 1) as usize;
            let my = ((mv.y as i32 & 7) << 1) as usize;
            mc(&mut cur.y, y * ds + x, ds, r, rs, rf.w as i32, rf.h as i32, xx, yy, mx, my, bw, bh, filter, avg, pmax);
            return;
        }
        // A reference of another size: positions scaled (as libvpx, each part separately).
        let (xi, yi, pxi, pyi, pwi, phi) = (x as i32, y as i32, px as i32, py as i32, pw as i32, ph as i32);
        let rows = self.rows as i32;
        let mvx = (mv.x as i32).clamp(-(xi + pwi - pxi + 4) * 8, (cols * 8 - xi + pxi + 3) * 8);
        let mvy = (mv.y as i32).clamp(-(yi + phi - pyi + 4) * 8, (rows * 8 - yi + pyi + 3) * 8);
        let sm = |n: i32, d: usize| ((n as i64 * scale[d] as i64) >> 14) as i32;
        let mx = sm(mvx * 2, 0) + sm(xi * 16, 0);
        let my = sm(mvy * 2, 1) + sm(yi * 16, 1);
        mc_scaled(&mut cur.y, y * ds + x, ds, r, rs, rf.w as i32, rf.h as i32, mx >> 4, my >> 4, (mx & 15) as usize, (my & 15) as usize, bw, bh, filter, avg, step[0] as usize, step[1] as usize, pmax);
    }

    /// Chroma block (4:2:0): position in chroma pixels, mv in luma eighths.
    #[allow(clippy::too_many_arguments)]
    fn mc_chroma(&mut self, rf: &Pic, ri: usize, x: usize, y: usize, mv: Mv, pw: usize, ph: usize, px: usize, py: usize, bw: usize, bh: usize, avg: bool) {
        let filter = self.b.filter;
        let pmax = (1i32 << self.bpp) - 1;
        let (w, h) = (rf.w.div_ceil(2) as i32, rf.h.div_ceil(2) as i32);
        let scale = self.mvscale[ri];
        let step = self.mvstep[ri];
        let (cols, rows) = (self.cols as i32, self.rows as i32);
        let cur = self.cur.as_mut().unwrap();
        let ds = cur.uv_stride;
        let (ru, rs) = plane(rf, Pl::U);
        let (rv, _) = plane(rf, Pl::V);
        if scale == [0, 0] {
            let xx = x as i32 + (mv.x as i32 >> 4);
            let yy = y as i32 + (mv.y as i32 >> 4);
            let mx = (mv.x as i32 & 15) as usize;
            let my = (mv.y as i32 & 15) as usize;
            mc(&mut cur.u, y * ds + x, ds, ru, rs, w, h, xx, yy, mx, my, bw, bh, filter, avg, pmax);
            mc(&mut cur.v, y * ds + x, ds, rv, rs, w, h, xx, yy, mx, my, bw, bh, filter, avg, pmax);
            return;
        }
        let (xi, yi, pxi, pyi, pwi, phi) = (x as i32, y as i32, px as i32, py as i32, pw as i32, ph as i32);
        let mvx = (mv.x as i32).clamp(-(xi + pwi - pxi + 4) * 16, (cols * 4 - xi + pxi + 3) * 16);
        let mvy = (mv.y as i32).clamp(-(yi + phi - pyi + 4) * 16, (rows * 4 - yi + pyi + 3) * 16);
        let sm = |n: i32, d: usize| ((n as i64 * scale[d] as i64) >> 14) as i32;
        let mx = sm(mvx, 0) + (sm(xi * 16, 0) & !15) + (sm(xi * 32, 0) & 15);
        let my = sm(mvy, 1) + (sm(yi * 16, 1) & !15) + (sm(yi * 32, 1) & 15);
        let (ix, iy, fx, fy) = (mx >> 4, my >> 4, (mx & 15) as usize, (my & 15) as usize);
        mc_scaled(&mut cur.u, y * ds + x, ds, ru, rs, w, h, ix, iy, fx, fy, bw, bh, filter, avg, step[0] as usize, step[1] as usize, pmax);
        mc_scaled(&mut cur.v, y * ds + x, ds, rv, rs, w, h, ix, iy, fx, fy, bw, bh, filter, avg, step[0] as usize, step[1] as usize, pmax);
    }

    pub(super) fn inter_recon(&mut self) {
        let b = self.b;
        let bpp = self.bpp;
        let (row, col) = (self.row, self.col);
        let ri = [b.rf[0], b.rf[1]];
        let refs: [Option<alloc::rc::Rc<Pic>>; 2] = [self.refs[self.h.refidx[b.rf[0]]].clone(), if b.comp { self.refs[self.h.refidx[b.rf[1]]].clone() } else { None }];
        let nref = if b.comp { 2 } else { 1 };
        if (0..nref).any(|i| self.mvscale[ri[i]][0] < 0) {
            return; // a reference of a size that can't be used
        }
        let scaled = (0..nref).any(|i| self.mvscale[ri[i]] != [0, 0]);
        for (i, r) in refs.iter().enumerate() {
            let Some(r) = r.as_ref() else { continue };
            let avg = i == 1;
            let k = ri[i];
            let (y0, x0) = (row * 8, col * 8);
            let mvs: [Mv; 4] = [b.mv[0][i], b.mv[1][i], b.mv[2][i], b.mv[3][i]];
            if b.bs > BS_8X8 {
                if scaled {
                    // With scaled references every sub-8x8 block is done as four 4x4 blocks.
                    self.mc_luma(r, k, x0, y0, mvs[0], 8, 8, 0, 0, 4, 4, avg);
                    self.mc_luma(r, k, x0 + 4, y0, mvs[1], 8, 8, 4, 0, 4, 4, avg);
                    self.mc_luma(r, k, x0, y0 + 4, mvs[2], 8, 8, 0, 4, 4, 4, avg);
                    self.mc_luma(r, k, x0 + 4, y0 + 4, mvs[3], 8, 8, 4, 4, 4, 4, avg);
                    self.mc_chroma(r, k, col * 4, row * 4, avg4(mvs[0], mvs[1], mvs[2], mvs[3]), 4, 4, 0, 0, 4, 4, avg);
                } else if b.bs == BS_8X4 {
                    self.mc_luma(r, k, x0, y0, mvs[0], 0, 0, 0, 0, 8, 4, avg);
                    self.mc_luma(r, k, x0, y0 + 4, mvs[2], 0, 0, 0, 0, 8, 4, avg);
                    self.mc_chroma(r, k, col * 4, row * 4, avg2(mvs[0], mvs[2]), 0, 0, 0, 0, 4, 4, avg);
                } else if b.bs == BS_4X8 {
                    self.mc_luma(r, k, x0, y0, mvs[0], 0, 0, 0, 0, 4, 8, avg);
                    self.mc_luma(r, k, x0 + 4, y0, mvs[1], 0, 0, 0, 0, 4, 8, avg);
                    self.mc_chroma(r, k, col * 4, row * 4, avg2(mvs[0], mvs[1]), 0, 0, 0, 0, 4, 4, avg);
                } else {
                    self.mc_luma(r, k, x0, y0, mvs[0], 0, 0, 0, 0, 4, 4, avg);
                    self.mc_luma(r, k, x0 + 4, y0, mvs[1], 0, 0, 0, 0, 4, 4, avg);
                    self.mc_luma(r, k, x0, y0 + 4, mvs[2], 0, 0, 0, 0, 4, 4, avg);
                    self.mc_luma(r, k, x0 + 4, y0 + 4, mvs[3], 0, 0, 0, 0, 4, 4, avg);
                    self.mc_chroma(r, k, col * 4, row * 4, avg4(mvs[0], mvs[1], mvs[2], mvs[3]), 0, 0, 0, 0, 4, 4, avg);
                }
            } else {
                let bw = bwh(0, b.bs, 0) * 4;
                let bh = bwh(0, b.bs, 1) * 4;
                let uvbw = bwh(1, b.bs, 0) * 4;
                let uvbh = bwh(1, b.bs, 1) * 4;
                self.mc_luma(r, k, x0, y0, mvs[0], bw, bh, 0, 0, bw, bh, avg);
                self.mc_chroma(r, k, col * 4, row * 4, mvs[0], uvbw, uvbh, 0, 0, uvbw, uvbh, avg);
            }
        }
        if b.skip {
            return;
        }
        let tx = if self.h.lossless { 4 } else { b.tx };
        let uvtx = if self.h.lossless { 4 } else { b.uvtx };
        let w4 = bwh(1, b.bs, 0) << 1;
        let h4 = bwh(1, b.bs, 1) << 1;
        let end_x = (2 * (self.cols - col)).min(w4);
        let end_y = (2 * (self.rows - row)).min(h4);
        let cur = self.cur.as_mut().unwrap();
        let step1d = 1 << b.tx;
        let step = 1 << (b.tx * 2);
        let ys = cur.stride;
        let mut n = 0;
        let mut y = 0;
        while y < end_y {
            let mut x = 0;
            while x < end_x {
                let eob = self.eob[n] as usize;
                if eob != 0 {
                    let o = (row * 8 + y * 4) * ys + col * 8 + x * 4;
                    itxfm_add(tx, 0, &mut cur.y, o, ys, &mut self.block[16 * n..], eob, bpp);
                }
                x += step1d;
                n += step;
            }
            y += step1d;
        }
        let (end_x, end_y) = (end_x >> 1, end_y >> 1);
        let uvstep1d = 1 << b.uvtx;
        let step = 1 << (b.uvtx * 2);
        let us = cur.uv_stride;
        for p in 0..2 {
            let mut n = 0;
            let mut y = 0;
            while y < end_y {
                let mut x = 0;
                while x < end_x {
                    let eob = self.uveob[p][n] as usize;
                    if eob != 0 {
                        let o = (row * 4 + y * 4) * us + col * 4 + x * 4;
                        let d = if p == 0 { &mut cur.u } else { &mut cur.v };
                        itxfm_add(uvtx, 0, d, o, us, &mut self.uvblock[p][16 * n..], eob, bpp);
                    }
                    x += uvstep1d;
                    n += step;
                }
                y += uvstep1d;
            }
        }
    }

    /// Edges for intra prediction of one transform block (FFmpeg's check_intra_mode); returns
    /// the mode actually used and fills `a` (a[0] top-left) and `l`.
    #[allow(clippy::too_many_arguments)]
    fn intra_edges(&self, mode: usize, a: &mut [u16; 72], l: &mut [u16; 32], pl: usize, px: usize, py: usize, x: usize, w: usize, y: usize, tx: usize) -> usize {
        const CONV: [[[usize; 2]; 2]; 10] = [
            [[DC_127, VERT], [DC_127, VERT]],
            [[DC_129, DC_129], [HOR, HOR]],
            [[DC_128, TOP_DC], [LEFT_DC, DC]],
            [[DC_127, DIAG_DOWN_LEFT], [DC_127, DIAG_DOWN_LEFT]],
            [[DIAG_DOWN_RIGHT; 2]; 2],
            [[VERT_RIGHT; 2]; 2],
            [[HOR_DOWN; 2]; 2],
            [[DC_127, VERT_LEFT], [DC_127, VERT_LEFT]],
            [[DC_129, DC_129], [HOR_UP, HOR_UP]],
            [[DC_129, VERT], [HOR, TM]],
        ];
        // needs: left, top, topleft, topright, invert_left
        const EDGES: [(bool, bool, bool, bool, bool); 15] = [
            (false, true, false, false, false),
            (true, false, false, false, false),
            (true, true, false, false, false),
            (false, true, false, true, false),
            (true, true, true, false, false),
            (true, true, true, false, false),
            (true, true, true, false, false),
            (false, true, false, true, false),
            (true, false, false, false, true),
            (true, true, true, false, false),
            (true, false, false, false, false),
            (false, true, false, false, false),
            (false, false, false, false, false),
            (false, false, false, false, false),
            (false, false, false, false, false),
        ];
        let (row, col) = (self.row, self.col);
        let ss = (pl != 0) as usize;
        let have_top = row > 0 || y > 0;
        let have_left = col > self.tile_col_start || x > 0;
        let have_right = x < w - 1;
        let mode = CONV[mode][have_left as usize][have_top as usize];
        let (nl, nt, ntl, ntr, inv) = EDGES[mode];
        let base = 1u16 << (self.bpp - 1);
        let cur = self.cur.as_ref().unwrap();
        let (pic, stride): (&[u16], usize) = match pl {
            0 => (&cur.y, cur.stride),
            1 => (&cur.u, cur.uv_stride),
            _ => (&cur.v, cur.uv_stride),
        };
        let n = 4 << tx;
        if nt {
            let n_have = ((((self.cols - col) << (1 - ss)) as isize - x as isize) * 4).max(0) as usize;
            let n_tr = if tx == 0 && ntr && have_right { 4 } else { 0 };
            // Row above: the saved unfiltered line at the top of a superblock row.
            let from_saved = row & 7 == 0 && y == 0;
            let top_at = |i: isize| -> u16 {
                if from_saved {
                    self.intra_pred[pl][((col * (8 >> ss) + x * 4) as isize + i) as usize]
                } else {
                    pic[((py - 1) * stride) as isize as usize + (px as isize + i) as usize]
                }
            };
            if have_top {
                if n <= n_have {
                    for i in 0..n {
                        a[1 + i] = top_at(i as isize);
                    }
                } else {
                    for i in 0..n_have {
                        a[1 + i] = top_at(i as isize);
                    }
                    let v = a[n_have];
                    for i in n_have..n {
                        a[1 + i] = v;
                    }
                }
            } else {
                for i in 0..n {
                    a[1 + i] = base - 1;
                }
            }
            if ntl {
                a[0] = if have_left && have_top { top_at(-1) } else if have_top { base + 1 } else { base - 1 };
            }
            if tx == 0 && ntr {
                if have_top && have_right && n + n_tr <= n_have {
                    for i in 4..8 {
                        a[1 + i] = top_at(i as isize);
                    }
                } else {
                    let v = a[4];
                    for i in 4..8 {
                        a[1 + i] = v;
                    }
                }
            }
        }
        if nl {
            if have_left {
                let n_have = ((((self.rows - row) << (1 - ss)) as isize - y as isize) * 4).max(0) as usize;
                let at = |i: usize| pic[(py + i) * stride + px - 1];
                if inv {
                    let m = n.min(n_have);
                    for i in 0..m {
                        l[i] = at(i);
                    }
                    if n > n_have {
                        let v = l[n_have - 1];
                        for i in n_have..n {
                            l[i] = v;
                        }
                    }
                } else {
                    let m = n.min(n_have);
                    for i in 0..m {
                        l[n - 1 - i] = at(i);
                    }
                    if n > n_have {
                        let v = l[n - n_have];
                        for i in 0..n - n_have {
                            l[i] = v;
                        }
                    }
                }
            } else {
                for i in 0..n {
                    l[i] = base + 1;
                }
            }
        }
        mode
    }

    pub(super) fn intra_recon(&mut self) {
        let bpp = self.bpp;
        let b = self.b;
        let (row, col) = (self.row, self.col);
        let mut w4 = bwh(1, b.bs, 0) << 1;
        let h4 = bwh(1, b.bs, 1) << 1;
        let mut end_x = (2 * (self.cols - col)).min(w4);
        let mut end_y = (2 * (self.rows - row)).min(h4);
        let tx = if self.h.lossless { 4 } else { b.tx };
        let uvtx = if self.h.lossless { 4 } else { b.uvtx };
        let mut a = [0u16; 72];
        let mut l = [0u16; 32];
        let step1d = 1 << b.tx;
        let step = 1 << (b.tx * 2);
        let size = 4 << b.tx;
        let mut n = 0;
        let mut y = 0;
        while y < end_y {
            let mut x = 0;
            while x < end_x {
                let mode0 = b.mode[if b.bs > BS_8X8 && b.tx == 0 { y * 2 + x } else { 0 }];
                let txtp = INTRA_TXFM_TYPE[mode0];
                let eob = if b.skip { 0 } else { self.eob[n] as usize };
                let (px, py) = (col * 8 + x * 4, row * 8 + y * 4);
                let mode = self.intra_edges(mode0, &mut a, &mut l, 0, px, py, x, w4, y, b.tx);
                let cur = self.cur.as_mut().unwrap();
                let s = cur.stride;
                intra_pred(mode, size, &mut cur.y, py * s + px, s, &l, &a, bpp);
                if eob != 0 {
                    itxfm_add(tx, txtp, &mut cur.y, py * s + px, s, &mut self.block[16 * n..], eob, bpp);
                }
                x += step1d;
                n += step;
            }
            y += step1d;
        }
        w4 >>= 1;
        end_x >>= 1;
        end_y >>= 1;
        let uvstep1d = 1 << b.uvtx;
        let step = 1 << (b.uvtx * 2);
        let size = 4 << b.uvtx;
        for p in 0..2 {
            let mut n = 0;
            let mut y = 0;
            while y < end_y {
                let mut x = 0;
                while x < end_x {
                    let eob = if b.skip { 0 } else { self.uveob[p][n] as usize };
                    let (px, py) = (col * 4 + x * 4, row * 4 + y * 4);
                    let mode = self.intra_edges(b.uvmode, &mut a, &mut l, p + 1, px, py, x, w4, y, b.uvtx);
                    let cur = self.cur.as_mut().unwrap();
                    let s = cur.uv_stride;
                    let d = if p == 0 { &mut cur.u } else { &mut cur.v };
                    intra_pred(mode, size, d, py * s + px, s, &l, &a, bpp);
                    if eob != 0 {
                        itxfm_add(uvtx, 0, d, py * s + px, s, &mut self.uvblock[p][16 * n..], eob, bpp);
                    }
                    x += uvstep1d;
                    n += step;
                }
                y += uvstep1d;
            }
        }
    }
}

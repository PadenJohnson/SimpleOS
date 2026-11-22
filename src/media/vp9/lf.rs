//! VP9 loop filter over one 64x64 superblock (FFmpeg's vp9lpf.c).

use super::dsp::loop_filter;
use super::Vp9Dec;

const WD: [u32; 3] = [4, 8, 16];

struct Lf<'a> {
    d: &'a mut [u16],
    ls: usize,
    bd: u32,
    lim: &'a [u8; 64],
    mblim: &'a [u8; 64],
}

impl Lf<'_> {
    /// Edge between columns (filter runs across x), 8 rows.
    fn h8(&mut self, at: usize, wd: usize, e: i32, i: i32, h: i32) {
        loop_filter(self.d, at, e, i, h, self.ls as isize, 1, WD[wd], self.bd);
    }
    /// Edge between rows (filter runs across y), 8 columns.
    fn v8(&mut self, at: usize, wd: usize, e: i32, i: i32, h: i32) {
        loop_filter(self.d, at, e, i, h, 1, self.ls as isize, WD[wd], self.bd);
    }
    fn params(&self, l: u8) -> (i32, i32, i32) {
        (self.mblim[l as usize] as i32, self.lim[l as usize] as i32, (l >> 4) as i32)
    }

    fn plane_cols(&mut self, col: usize, ss_h: usize, ss_v: usize, lvl: &[u8; 64], mask: &[[u8; 4]; 8], mut dst: usize) {
        let ls = self.ls;
        let mut lv = 0usize;
        let mut y = 0;
        while y < 8 {
            let mut ptr = dst;
            let mut l = lv;
            let hm1v = mask[y];
            let hm2v = mask[y + 1 + ss_v];
            let hm1 = (hm1v[0] | hm1v[1] | hm1v[2]) as u32;
            let hm13 = hm1v[3] as u32;
            let hm2 = (hm2v[1] | hm2v[2]) as u32;
            let hm23 = hm2v[3] as u32;
            let hm = hm1 | hm2 | hm13 | hm23;
            let mut x = 1u32;
            while hm & !(x - 1) != 0 {
                if col != 0 || x > 1 {
                    if hm1 & x != 0 {
                        let (e, i, h) = self.params(lvl[l]);
                        if hm1v[0] as u32 & x != 0 {
                            if hm2v[0] as u32 & x != 0 {
                                self.h8(ptr, 2, e, i, h);
                                self.h8(ptr + 8 * ls, 2, e, i, h);
                            } else {
                                self.h8(ptr, 2, e, i, h);
                            }
                        } else if hm2 & x != 0 {
                            let (e2, i2, h2) = self.params(lvl[l + (8 << ss_v)]);
                            self.h8(ptr, (hm1v[1] as u32 & x != 0) as usize, e, i, h);
                            self.h8(ptr + 8 * ls, (hm2v[1] as u32 & x != 0) as usize, e2, i2, h2);
                        } else {
                            self.h8(ptr, (hm1v[1] as u32 & x != 0) as usize, e, i, h);
                        }
                    } else if hm2 & x != 0 {
                        let (e, i, h) = self.params(lvl[l + (8 << ss_v)]);
                        self.h8(ptr + 8 * ls, (hm2v[1] as u32 & x != 0) as usize, e, i, h);
                    }
                }
                if ss_h != 0 {
                    if x & 0xAA != 0 {
                        l += 2;
                    }
                } else {
                    if hm13 & x != 0 {
                        let (e, i, h) = self.params(lvl[l]);
                        if hm23 & x != 0 {
                            let (e2, i2, h2) = self.params(lvl[l + (8 << ss_v)]);
                            self.h8(ptr + 4, 0, e, i, h);
                            self.h8(ptr + 4 + 8 * ls, 0, e2, i2, h2);
                        } else {
                            self.h8(ptr + 4, 0, e, i, h);
                        }
                    } else if hm23 & x != 0 {
                        let (e, i, h) = self.params(lvl[l + (8 << ss_v)]);
                        self.h8(ptr + 8 * ls + 4, 0, e, i, h);
                    }
                    l += 1;
                }
                x <<= 1;
                ptr += 8 >> ss_h;
            }
            y += 2 << ss_v;
            dst += 16 * ls;
            lv += 16 << ss_v;
        }
    }

    fn plane_rows(&mut self, row: usize, ss_h: usize, ss_v: usize, lvl: &[u8; 64], mask: &[[u8; 4]; 8], mut dst: usize) {
        let ls = self.ls;
        let mut lv = 0usize;
        for y in 0..8 {
            let mut ptr = dst;
            let mut l = lv;
            let vmask = mask[y];
            let vm = (vmask[0] | vmask[1] | vmask[2]) as u32;
            let vm3 = vmask[3] as u32;
            let mut x = 1u32;
            let nx = 1 + ss_h;
            while vm & !(x - 1) != 0 {
                if row != 0 || y != 0 {
                    if vm & x != 0 {
                        let (e, i, h) = self.params(lvl[l]);
                        if vmask[0] as u32 & x != 0 {
                            if vmask[0] as u32 & (x << nx) != 0 {
                                self.v8(ptr, 2, e, i, h);
                                self.v8(ptr + 8, 2, e, i, h);
                            } else {
                                self.v8(ptr, 2, e, i, h);
                            }
                        } else if vm & (x << nx) != 0 {
                            let (e2, i2, h2) = self.params(lvl[l + nx]);
                            self.v8(ptr, (vmask[1] as u32 & x != 0) as usize, e, i, h);
                            self.v8(ptr + 8, (vmask[1] as u32 & (x << nx) != 0) as usize, e2, i2, h2);
                        } else {
                            self.v8(ptr, (vmask[1] as u32 & x != 0) as usize, e, i, h);
                        }
                    } else if vm & (x << nx) != 0 {
                        let (e, i, h) = self.params(lvl[l + nx]);
                        self.v8(ptr + 8, (vmask[1] as u32 & (x << nx) != 0) as usize, e, i, h);
                    }
                }
                if ss_v == 0 {
                    if vm3 & x != 0 {
                        let (e, i, h) = self.params(lvl[l]);
                        if vm3 & (x << nx) != 0 {
                            let (e2, i2, h2) = self.params(lvl[l + nx]);
                            self.v8(ptr + ls * 4, 0, e, i, h);
                            self.v8(ptr + ls * 4 + 8, 0, e2, i2, h2);
                        } else {
                            self.v8(ptr + ls * 4, 0, e, i, h);
                        }
                    } else if vm3 & (x << nx) != 0 {
                        let (e, i, h) = self.params(lvl[l + nx]);
                        self.v8(ptr + ls * 4 + 8, 0, e, i, h);
                    }
                }
                x <<= 2 << ss_h;
                ptr += 16;
                l += 2 << ss_h;
            }
            if ss_v != 0 {
                if y & 1 != 0 {
                    lv += 16;
                }
            } else {
                lv += 8;
            }
            dst += (8 * ls) >> ss_v;
        }
    }
}

impl Vp9Dec {
    pub(super) fn loopfilter_sb(&mut self, lf: usize, row: usize, col: usize) {
        let f = self.lflvl[lf].clone();
        let cur = self.cur.as_mut().unwrap();
        let (ys, us) = (cur.stride, cur.uv_stride);
        let mut l = Lf { d: &mut cur.y, ls: ys, bd: self.bpp, lim: &self.lim_lut, mblim: &self.mblim_lut };
        let yoff = row * 8 * ys + col * 8;
        l.plane_cols(col, 0, 0, &f.level, &f.mask[0][0], yoff);
        l.plane_rows(row, 0, 0, &f.level, &f.mask[0][1], yoff);
        let uvoff = row * 4 * us + col * 4;
        for p in 0..2 {
            let d = if p == 0 { &mut cur.u } else { &mut cur.v };
            let mut l = Lf { d, ls: us, bd: self.bpp, lim: &self.lim_lut, mblim: &self.mblim_lut };
            l.plane_cols(col, 1, 1, &f.level, &f.mask[1][0], uvoff);
            l.plane_rows(row, 1, 1, &f.level, &f.mask[1][1], uvoff);
        }
    }
}

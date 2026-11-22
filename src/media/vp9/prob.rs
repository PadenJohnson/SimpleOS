//! VP9 probabilities: the default context, symbol counts and backward adaptation
//! (FFmpeg's vp9prob.c).

use super::tab::{DEFAULT_COEF_PROBS, DEFAULT_PROBS};

#[derive(Clone, Copy, Default)]
pub struct MvProbs {
    pub sign: u8,
    pub classes: [u8; 10],
    pub class0: u8,
    pub bits: [u8; 10],
    pub class0_fp: [[u8; 3]; 2],
    pub fp: [u8; 3],
    pub class0_hp: u8,
    pub hp: u8,
}

#[derive(Clone, Copy, Default)]
pub struct Probs {
    pub y_mode: [[u8; 9]; 4],
    pub uv_mode: [[u8; 9]; 10],
    pub filter: [[u8; 2]; 4],
    pub mv_mode: [[u8; 3]; 7],
    pub intra: [u8; 4],
    pub comp: [u8; 5],
    pub single_ref: [[u8; 2]; 5],
    pub comp_ref: [u8; 5],
    pub tx32p: [[u8; 3]; 2],
    pub tx16p: [[u8; 2]; 2],
    pub tx8p: [u8; 2],
    pub skip: [u8; 3],
    pub mv_joint: [u8; 3],
    pub mv_comp: [MvProbs; 2],
    pub partition: [[[u8; 3]; 4]; 4],
}

impl Probs {
    pub fn defaults() -> Probs {
        let mut it = DEFAULT_PROBS.iter().copied();
        let mut n = || it.next().unwrap_or(128);
        let mut p = Probs::default();
        for r in p.y_mode.iter_mut() {
            r.iter_mut().for_each(|v| *v = n());
        }
        for r in p.uv_mode.iter_mut() {
            r.iter_mut().for_each(|v| *v = n());
        }
        for r in p.filter.iter_mut() {
            r.iter_mut().for_each(|v| *v = n());
        }
        for r in p.mv_mode.iter_mut() {
            r.iter_mut().for_each(|v| *v = n());
        }
        p.intra.iter_mut().for_each(|v| *v = n());
        p.comp.iter_mut().for_each(|v| *v = n());
        for r in p.single_ref.iter_mut() {
            r.iter_mut().for_each(|v| *v = n());
        }
        p.comp_ref.iter_mut().for_each(|v| *v = n());
        for r in p.tx32p.iter_mut() {
            r.iter_mut().for_each(|v| *v = n());
        }
        for r in p.tx16p.iter_mut() {
            r.iter_mut().for_each(|v| *v = n());
        }
        p.tx8p.iter_mut().for_each(|v| *v = n());
        p.skip.iter_mut().for_each(|v| *v = n());
        p.mv_joint.iter_mut().for_each(|v| *v = n());
        for m in p.mv_comp.iter_mut() {
            m.sign = n();
            m.classes.iter_mut().for_each(|v| *v = n());
            m.class0 = n();
            m.bits.iter_mut().for_each(|v| *v = n());
            for r in m.class0_fp.iter_mut() {
                r.iter_mut().for_each(|v| *v = n());
            }
            m.fp.iter_mut().for_each(|v| *v = n());
            m.class0_hp = n();
            m.hp = n();
        }
        for a in p.partition.iter_mut() {
            for r in a.iter_mut() {
                r.iter_mut().for_each(|v| *v = n());
            }
        }
        p
    }
}

/// Index into the [4][2][2][6][6] coefficient context arrays.
#[inline]
pub fn cidx(tx: usize, pl: usize, rf: usize, band: usize, ctx: usize) -> usize {
    (((tx * 2 + pl) * 2 + rf) * 6 + band) * 6 + ctx
}

/// The default coefficient probabilities ([4][2][2][6][6][3]; FFmpeg's table leaves out
/// the unused contexts 3..5 of band 0).
pub fn default_coef() -> [u8; 1728] {
    let mut out = [0u8; 1728];
    let mut src = DEFAULT_COEF_PROBS.iter().copied();
    for g in 0..16 {
        for band in 0..6 {
            let n = if band == 0 { 3 } else { 6 };
            for ctx in 0..n {
                for k in 0..3 {
                    out[(g * 36 + band * 6 + ctx) * 3 + k] = src.next().unwrap_or(0);
                }
            }
        }
    }
    out
}

#[derive(Clone, Copy)]
pub struct MvCounts {
    pub sign: [u32; 2],
    pub classes: [u32; 11],
    pub class0: [u32; 2],
    pub bits: [[u32; 2]; 10],
    pub class0_fp: [[u32; 4]; 2],
    pub fp: [u32; 4],
    pub class0_hp: [u32; 2],
    pub hp: [u32; 2],
}

#[derive(Clone)]
pub struct Counts {
    pub y_mode: [[u32; 10]; 4],
    pub uv_mode: [[u32; 10]; 10],
    pub filter: [[u32; 3]; 4],
    pub mv_mode: [[u32; 4]; 7],
    pub intra: [[u32; 2]; 4],
    pub comp: [[u32; 2]; 5],
    pub single_ref: [[[u32; 2]; 2]; 5],
    pub comp_ref: [[u32; 2]; 5],
    pub tx32p: [[u32; 4]; 2],
    pub tx16p: [[u32; 3]; 2],
    pub tx8p: [[u32; 2]; 2],
    pub skip: [[u32; 2]; 3],
    pub mv_joint: [u32; 4],
    pub mv_comp: [MvCounts; 2],
    pub partition: [[[u32; 4]; 4]; 4],
    /// [4][2][2][6][6][3] and [..][2].
    pub coef: alloc::vec::Vec<[u32; 3]>,
    pub eob: alloc::vec::Vec<[u32; 2]>,
}

impl Counts {
    pub fn new() -> Counts {
        let z = MvCounts { sign: [0; 2], classes: [0; 11], class0: [0; 2], bits: [[0; 2]; 10], class0_fp: [[0; 4]; 2], fp: [0; 4], class0_hp: [0; 2], hp: [0; 2] };
        Counts {
            y_mode: [[0; 10]; 4],
            uv_mode: [[0; 10]; 10],
            filter: [[0; 3]; 4],
            mv_mode: [[0; 4]; 7],
            intra: [[0; 2]; 4],
            comp: [[0; 2]; 5],
            single_ref: [[[0; 2]; 2]; 5],
            comp_ref: [[0; 2]; 5],
            tx32p: [[0; 4]; 2],
            tx16p: [[0; 3]; 2],
            tx8p: [[0; 2]; 2],
            skip: [[0; 2]; 3],
            mv_joint: [0; 4],
            mv_comp: [z, z],
            partition: [[[0; 4]; 4]; 4],
            coef: alloc::vec![[0; 3]; 576],
            eob: alloc::vec![[0; 2]; 576],
        }
    }
}

fn adapt(p: &mut u8, ct0: u32, ct1: u32, max_count: u32, update_factor: u32) {
    let ct = ct0 + ct1;
    if ct == 0 {
        return;
    }
    let uf = (update_factor * ct.min(max_count) / max_count) as i32;
    let p1 = *p as i32;
    let p2 = (((((ct0 as u64) << 8) + (ct >> 1) as u64) / ct as u64) as i32).clamp(1, 255);
    *p = (p1 + (((p2 - p1) * uf + 128) >> 8)) as u8;
}

/// Backward adaptation of the frame context `p` / `coef` from the counts.
#[allow(clippy::too_many_arguments)]
pub fn adapt_probs(p: &mut Probs, coef: &mut [u8; 1728], cur: &Probs, c: &Counts, keyframe: bool, intraonly: bool, last_keyframe: bool, comppredmode: u8, txswitchable: bool, filterswitchable: bool, hp: bool) {
    let uf = if keyframe || intraonly || !last_keyframe { 112 } else { 128 };
    for i in 0..4 {
        for j in 0..2 {
            for k in 0..2 {
                for l in 0..6 {
                    for m in 0..6 {
                        if l == 0 && m >= 3 {
                            break;
                        }
                        let ix = cidx(i, j, k, l, m);
                        let (e, cc) = (c.eob[ix], c.coef[ix]);
                        let pp = &mut coef[ix * 3..ix * 3 + 3];
                        adapt(&mut pp[0], e[0], e[1], 24, uf);
                        adapt(&mut pp[1], cc[0], cc[1] + cc[2], 24, uf);
                        adapt(&mut pp[2], cc[1], cc[2], 24, uf);
                    }
                }
            }
        }
    }
    if keyframe || intraonly {
        p.skip = cur.skip;
        p.tx32p = cur.tx32p;
        p.tx16p = cur.tx16p;
        p.tx8p = cur.tx8p;
        return;
    }
    for i in 0..3 {
        adapt(&mut p.skip[i], c.skip[i][0], c.skip[i][1], 20, 128);
    }
    for i in 0..4 {
        adapt(&mut p.intra[i], c.intra[i][0], c.intra[i][1], 20, 128);
    }
    if comppredmode == 2 {
        for i in 0..5 {
            adapt(&mut p.comp[i], c.comp[i][0], c.comp[i][1], 20, 128);
        }
    }
    if comppredmode != 0 {
        for i in 0..5 {
            adapt(&mut p.comp_ref[i], c.comp_ref[i][0], c.comp_ref[i][1], 20, 128);
        }
    }
    if comppredmode != 1 {
        for i in 0..5 {
            let cc = c.single_ref[i];
            adapt(&mut p.single_ref[i][0], cc[0][0], cc[0][1], 20, 128);
            adapt(&mut p.single_ref[i][1], cc[1][0], cc[1][1], 20, 128);
        }
    }
    for i in 0..4 {
        for j in 0..4 {
            let cc = c.partition[i][j];
            let pp = &mut p.partition[i][j];
            adapt(&mut pp[0], cc[0], cc[1] + cc[2] + cc[3], 20, 128);
            adapt(&mut pp[1], cc[1], cc[2] + cc[3], 20, 128);
            adapt(&mut pp[2], cc[2], cc[3], 20, 128);
        }
    }
    if txswitchable {
        for i in 0..2 {
            let (c16, c32) = (c.tx16p[i], c.tx32p[i]);
            adapt(&mut p.tx8p[i], c.tx8p[i][0], c.tx8p[i][1], 20, 128);
            adapt(&mut p.tx16p[i][0], c16[0], c16[1] + c16[2], 20, 128);
            adapt(&mut p.tx16p[i][1], c16[1], c16[2], 20, 128);
            adapt(&mut p.tx32p[i][0], c32[0], c32[1] + c32[2] + c32[3], 20, 128);
            adapt(&mut p.tx32p[i][1], c32[1], c32[2] + c32[3], 20, 128);
            adapt(&mut p.tx32p[i][2], c32[2], c32[3], 20, 128);
        }
    }
    if filterswitchable {
        for i in 0..4 {
            let cc = c.filter[i];
            adapt(&mut p.filter[i][0], cc[0], cc[1] + cc[2], 20, 128);
            adapt(&mut p.filter[i][1], cc[1], cc[2], 20, 128);
        }
    }
    for i in 0..7 {
        let cc = c.mv_mode[i];
        let pp = &mut p.mv_mode[i];
        adapt(&mut pp[0], cc[2], cc[1] + cc[0] + cc[3], 20, 128);
        adapt(&mut pp[1], cc[0], cc[1] + cc[3], 20, 128);
        adapt(&mut pp[2], cc[1], cc[3], 20, 128);
    }
    {
        let cc = c.mv_joint;
        adapt(&mut p.mv_joint[0], cc[0], cc[1] + cc[2] + cc[3], 20, 128);
        adapt(&mut p.mv_joint[1], cc[1], cc[2] + cc[3], 20, 128);
        adapt(&mut p.mv_joint[2], cc[2], cc[3], 20, 128);
    }
    for i in 0..2 {
        let mc = &c.mv_comp[i];
        let mp = &mut p.mv_comp[i];
        adapt(&mut mp.sign, mc.sign[0], mc.sign[1], 20, 128);
        let cc = mc.classes;
        let mut sum: u32 = cc[1..11].iter().sum();
        adapt(&mut mp.classes[0], cc[0], sum, 20, 128);
        sum -= cc[1];
        adapt(&mut mp.classes[1], cc[1], sum, 20, 128);
        sum -= cc[2] + cc[3];
        adapt(&mut mp.classes[2], cc[2] + cc[3], sum, 20, 128);
        adapt(&mut mp.classes[3], cc[2], cc[3], 20, 128);
        sum -= cc[4] + cc[5];
        adapt(&mut mp.classes[4], cc[4] + cc[5], sum, 20, 128);
        adapt(&mut mp.classes[5], cc[4], cc[5], 20, 128);
        sum -= cc[6];
        adapt(&mut mp.classes[6], cc[6], sum, 20, 128);
        adapt(&mut mp.classes[7], cc[7] + cc[8], cc[9] + cc[10], 20, 128);
        adapt(&mut mp.classes[8], cc[7], cc[8], 20, 128);
        adapt(&mut mp.classes[9], cc[9], cc[10], 20, 128);
        adapt(&mut mp.class0, mc.class0[0], mc.class0[1], 20, 128);
        for j in 0..10 {
            adapt(&mut mp.bits[j], mc.bits[j][0], mc.bits[j][1], 20, 128);
        }
        for j in 0..2 {
            let cc = mc.class0_fp[j];
            adapt(&mut mp.class0_fp[j][0], cc[0], cc[1] + cc[2] + cc[3], 20, 128);
            adapt(&mut mp.class0_fp[j][1], cc[1], cc[2] + cc[3], 20, 128);
            adapt(&mut mp.class0_fp[j][2], cc[2], cc[3], 20, 128);
        }
        let cc = mc.fp;
        adapt(&mut mp.fp[0], cc[0], cc[1] + cc[2] + cc[3], 20, 128);
        adapt(&mut mp.fp[1], cc[1], cc[2] + cc[3], 20, 128);
        adapt(&mut mp.fp[2], cc[2], cc[3], 20, 128);
        if hp {
            adapt(&mut mp.class0_hp, mc.class0_hp[0], mc.class0_hp[1], 20, 128);
            adapt(&mut mp.hp, mc.hp[0], mc.hp[1], 20, 128);
        }
    }
    let modes = |pp: &mut [u8; 9], cc: &[u32; 10]| {
        // Mode numbers: V 0, H 1, DC 2, D45 3, D135 4, D117 5, D153 6, D63 7, D27 8, TM 9.
        let mut sum = cc[0] + cc[1] + cc[3] + cc[4] + cc[5] + cc[6] + cc[7] + cc[8] + cc[9];
        adapt(&mut pp[0], cc[2], sum, 20, 128);
        sum -= cc[9];
        adapt(&mut pp[1], cc[9], sum, 20, 128);
        sum -= cc[0];
        adapt(&mut pp[2], cc[0], sum, 20, 128);
        let mut s2 = cc[1] + cc[4] + cc[5];
        sum -= s2;
        adapt(&mut pp[3], s2, sum, 20, 128);
        s2 -= cc[1];
        adapt(&mut pp[4], cc[1], s2, 20, 128);
        adapt(&mut pp[5], cc[4], cc[5], 20, 128);
        sum -= cc[3];
        adapt(&mut pp[6], cc[3], sum, 20, 128);
        sum -= cc[7];
        adapt(&mut pp[7], cc[7], sum, 20, 128);
        adapt(&mut pp[8], cc[6], cc[8], 20, 128);
    };
    for i in 0..4 {
        modes(&mut p.y_mode[i], &c.y_mode[i]);
    }
    for i in 0..10 {
        modes(&mut p.uv_mode[i], &c.uv_mode[i]);
    }
}

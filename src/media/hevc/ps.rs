//! HEVC parameter sets (SPS, PPS) and slice headers. Main and Main 10 profiles (4:2:0 and
//! 4:0:0; the range-extension tools aren't supported).

use alloc::vec;
use alloc::vec::Vec;

use super::tab::{DEFAULT_SL_INTER, DEFAULT_SL_INTRA};
use crate::video::bits::Bits;

#[derive(Clone, Copy, Default)]
pub struct StRps {
    pub num_negative: usize,
    pub num_delta: usize,
    pub delta_poc: [i32; 32],
    pub used: u32,
}

#[derive(Clone)]
pub struct ScalingList {
    /// [size 4/8/16/32][matrix] in raster order (8x8 for the larger sizes).
    pub sl: [[[u8; 64]; 6]; 4],
    /// DC values for 16x16 and 32x32.
    pub dc: [[u8; 6]; 2],
}

impl ScalingList {
    pub fn default_lists() -> ScalingList {
        let mut s = ScalingList { sl: [[[16; 64]; 6]; 4], dc: [[16; 6]; 2] };
        for size in 1..4 {
            for m in 0..6 {
                s.sl[size][m] = if m < 3 { DEFAULT_SL_INTRA } else { DEFAULT_SL_INTER };
            }
        }
        s
    }

    fn parse(&mut self, b: &mut Bits) {
        for size in 0..4 {
            let mut m = 0;
            while m < 6 {
                if !b.flag() {
                    let delta = b.ue() as usize * if size == 3 { 3 } else { 1 };
                    if delta != 0 && delta <= m {
                        self.sl[size][m] = self.sl[size][m - delta];
                        if size > 1 {
                            self.dc[size - 2][m] = self.dc[size - 2][m - delta];
                        }
                    } else if delta == 0 {
                        // Default list.
                        self.sl[size][m] = if size == 0 { [16; 64] } else if m < 3 { DEFAULT_SL_INTRA } else { DEFAULT_SL_INTER };
                        if size > 1 {
                            self.dc[size - 2][m] = 16;
                        }
                    }
                } else {
                    let mut next: i32 = 8;
                    let num = if size == 0 { 16 } else { 64 };
                    if size > 1 {
                        next = b.se() + 8;
                        self.dc[size - 2][m] = next as u8;
                    }
                    for i in 0..num {
                        let pos = if size == 0 { let (x, y) = diag_xy(4, i); y * 4 + x } else { let (x, y) = diag_xy(8, i); y * 8 + x };
                        next = (next + b.se() + 256).rem_euclid(256);
                        self.sl[size][m][pos] = next as u8;
                    }
                }
                m += if size == 3 { 3 } else { 1 };
            }
        }
        // 32x32 chroma lists (4:4:4 only) copy the 16x16 ones.
        for m in [1, 2, 4, 5] {
            self.sl[3][m] = self.sl[2][m];
            self.dc[1][m] = self.dc[0][m];
        }
    }
}

/// Up-right diagonal scan of an n x n block: position i -> (x, y).
pub fn diag_xy(n: usize, i: usize) -> (usize, usize) {
    let mut k = 0;
    let (mut x, mut y) = (0usize, 0usize);
    loop {
        // Walk diagonals: start at (0, d), go up-right.
        for d in 0..2 * n - 1 {
            let mut yy = d.min(n - 1) as isize;
            let mut xx = d as isize - yy;
            while yy >= 0 && (xx as usize) < n {
                if k == i {
                    x = xx as usize;
                    y = yy as usize;
                    return (x, y);
                }
                k += 1;
                yy -= 1;
                xx += 1;
            }
        }
        return (x, y);
    }
}

#[derive(Clone)]
pub struct Sps {
    pub chroma: u32,
    pub width: usize,
    pub height: usize,
    pub conf: [usize; 4],
    pub bit_depth: u32,
    pub bit_depth_c: u32,
    pub log2_max_poc_lsb: u32,
    pub max_dec_pic_buffering: usize,
    pub num_reorder: usize,
    pub log2_min_cb: u32,
    pub log2_ctb: u32,
    pub log2_min_tb: u32,
    pub log2_max_tb: u32,
    pub max_th_depth_inter: u32,
    pub max_th_depth_intra: u32,
    pub scaling: Option<ScalingList>,
    pub amp: bool,
    pub sao: bool,
    pub pcm: bool,
    pub pcm_bd: u32,
    pub pcm_bd_c: u32,
    pub log2_min_pcm: u32,
    pub log2_max_pcm: u32,
    pub pcm_loop_filter_disabled: bool,
    pub st_rps: Vec<StRps>,
    pub long_term: bool,
    pub lt_poc_lsb: Vec<u32>,
    pub lt_used: Vec<bool>,
    pub temporal_mvp: bool,
    pub strong_intra_smoothing: bool,
    pub transfer: u32,
    pub matrix: u32,
    pub full_range: bool,
    // Derived.
    pub ctb: usize,
    pub ctb_w: usize,
    pub ctb_h: usize,
    pub min_cb_w: usize,
    pub min_cb_h: usize,
    pub qp_bd_offset: i32,
}

fn skip_ptl(b: &mut Bits, max_sub_layers: usize) {
    b.skip(96); // general profile, tier, flags, level
    let mut prof = [false; 8];
    let mut lev = [false; 8];
    for i in 0..max_sub_layers - 1 {
        prof[i] = b.flag();
        lev[i] = b.flag();
    }
    if max_sub_layers > 1 {
        for _ in max_sub_layers - 1..8 {
            b.skip(2);
        }
    }
    for i in 0..max_sub_layers - 1 {
        if prof[i] {
            b.skip(88);
        }
        if lev[i] {
            b.skip(8);
        }
    }
}

pub fn parse_st_rps(b: &mut Bits, list: &[StRps], idx: usize, slice: bool) -> Option<StRps> {
    let mut rps = StRps::default();
    let predict = idx != 0 && b.flag();
    if predict {
        let ref_idx = if slice {
            let delta_idx = b.ue() as usize + 1;
            if delta_idx > idx {
                return None;
            }
            idx - delta_idx
        } else {
            idx - 1
        };
        let r = *list.get(ref_idx)?;
        let sign = b.flag();
        let abs = b.ue() as i32 + 1;
        let delta_rps = if sign { -abs } else { abs };
        let mut used = [false; 33];
        let (mut k, mut k0) = (0usize, 0usize);
        for i in 0..=r.num_delta {
            let u = b.flag();
            let use_delta = if !u { b.flag() } else { true };
            if u || use_delta {
                if k >= 32 {
                    return None;
                }
                let dp = if i < r.num_delta { delta_rps + r.delta_poc[i] } else { delta_rps };
                used[k] = u;
                rps.delta_poc[k] = dp;
                if dp < 0 {
                    k0 += 1;
                }
                k += 1;
            }
        }
        rps.num_delta = k;
        rps.num_negative = k0;
        // Sort by delta (insertion sort, as FFmpeg).
        for i in 1..k {
            let dp = rps.delta_poc[i];
            let u = used[i];
            let mut j = i as isize - 1;
            while j >= 0 {
                let t = rps.delta_poc[j as usize];
                if dp < t {
                    rps.delta_poc[j as usize + 1] = t;
                    used[j as usize + 1] = used[j as usize];
                    rps.delta_poc[j as usize] = dp;
                    used[j as usize] = u;
                }
                j -= 1;
            }
        }
        // Negative ones nearest first.
        let n = rps.num_negative;
        for i in 0..n / 2 {
            rps.delta_poc.swap(i, n - 1 - i);
            used.swap(i, n - 1 - i);
        }
        for (i, &u) in used.iter().enumerate().take(32) {
            if u {
                rps.used |= 1 << i;
            }
        }
    } else {
        let neg = b.ue() as usize;
        let pos = b.ue() as usize;
        if neg >= 16 || pos >= 16 {
            return None;
        }
        rps.num_negative = neg;
        rps.num_delta = neg + pos;
        let mut prev = 0i32;
        for i in 0..neg {
            prev -= b.ue() as i32 + 1;
            rps.delta_poc[i] = prev;
            if b.flag() {
                rps.used |= 1 << i;
            }
        }
        prev = 0;
        for i in 0..pos {
            prev += b.ue() as i32 + 1;
            rps.delta_poc[neg + i] = prev;
            if b.flag() {
                rps.used |= 1 << (neg + i);
            }
        }
    }
    Some(rps)
}

pub fn parse_sps(d: &[u8]) -> Option<(usize, Sps)> {
    let mut b = Bits::new(d);
    b.skip(4); // vps id
    let max_sub_layers = b.read(3) as usize + 1;
    if max_sub_layers > 7 {
        return None;
    }
    b.skip(1);
    skip_ptl(&mut b, max_sub_layers);
    let id = b.ue() as usize;
    let chroma = b.ue();
    if chroma == 3 {
        b.skip(1);
    }
    let width = b.ue() as usize;
    let height = b.ue() as usize;
    let mut conf = [0; 4];
    if b.flag() {
        let (sw, sh) = if chroma == 1 { (2, 2) } else if chroma == 2 { (2, 1) } else { (1, 1) };
        conf = [b.ue() as usize * sw, b.ue() as usize * sw, b.ue() as usize * sh, b.ue() as usize * sh];
    }
    let bit_depth = b.ue() + 8;
    let bit_depth_c = b.ue() + 8;
    let log2_max_poc_lsb = b.ue() + 4;
    let ordering = b.flag();
    let start = if ordering { 0 } else { max_sub_layers - 1 };
    let (mut mdpb, mut reorder) = (1, 0);
    for _ in start..max_sub_layers {
        mdpb = b.ue() as usize + 1;
        reorder = b.ue() as usize;
        b.ue();
    }
    let log2_min_cb = b.ue() + 3;
    let log2_ctb = log2_min_cb + b.ue();
    let log2_min_tb = b.ue() + 2;
    let log2_max_tb = log2_min_tb + b.ue();
    let max_th_depth_inter = b.ue();
    let max_th_depth_intra = b.ue();
    let mut scaling = None;
    if b.flag() {
        let mut s = ScalingList::default_lists();
        if b.flag() {
            s.parse(&mut b);
        }
        scaling = Some(s);
    }
    let amp = b.flag();
    let sao = b.flag();
    let pcm = b.flag();
    let (mut pcm_bd, mut pcm_bd_c, mut log2_min_pcm, mut log2_max_pcm, mut pcm_lf) = (8, 8, 3, 3, false);
    if pcm {
        pcm_bd = b.read(4) + 1;
        pcm_bd_c = b.read(4) + 1;
        log2_min_pcm = b.ue() + 3;
        log2_max_pcm = log2_min_pcm + b.ue();
        pcm_lf = b.flag();
    }
    let n_rps = b.ue() as usize;
    if n_rps > 64 {
        return None;
    }
    let mut st_rps = Vec::new();
    for i in 0..n_rps {
        let r = parse_st_rps(&mut b, &st_rps, i, false)?;
        st_rps.push(r);
    }
    let long_term = b.flag();
    let (mut lt_poc_lsb, mut lt_used) = (Vec::new(), Vec::new());
    if long_term {
        let n = b.ue() as usize;
        for _ in 0..n.min(32) {
            lt_poc_lsb.push(b.read(log2_max_poc_lsb));
            lt_used.push(b.flag());
        }
    }
    let temporal_mvp = b.flag();
    let strong_intra_smoothing = b.flag();
    let (mut transfer, mut matrix, mut full_range) = (2, 2, false);
    if b.flag() {
        // VUI: only the colour description is needed.
        if b.flag() && b.read(8) == 255 {
            b.skip(32);
        }
        if b.flag() {
            b.skip(1);
        }
        if b.flag() {
            b.skip(3);
            full_range = b.flag();
            if b.flag() {
                b.skip(8);
                transfer = b.read(8);
                matrix = b.read(8);
            }
        }
    }
    if width == 0 || height == 0 || log2_ctb > 6 || log2_ctb < 4 || bit_depth > 12 || chroma > 1 {
        return None;
    }
    let ctb = 1usize << log2_ctb;
    Some((
        id,
        Sps {
            chroma,
            width,
            height,
            conf,
            bit_depth,
            bit_depth_c,
            log2_max_poc_lsb,
            max_dec_pic_buffering: mdpb,
            num_reorder: reorder,
            log2_min_cb,
            log2_ctb,
            log2_min_tb,
            log2_max_tb,
            max_th_depth_inter,
            max_th_depth_intra,
            scaling,
            amp,
            sao,
            pcm,
            pcm_bd,
            pcm_bd_c,
            log2_min_pcm,
            log2_max_pcm,
            pcm_loop_filter_disabled: pcm_lf,
            st_rps,
            long_term,
            lt_poc_lsb,
            lt_used,
            temporal_mvp,
            strong_intra_smoothing,
            transfer,
            matrix,
            full_range,
            ctb,
            ctb_w: width.div_ceil(ctb),
            ctb_h: height.div_ceil(ctb),
            min_cb_w: width >> log2_min_cb,
            min_cb_h: height >> log2_min_cb,
            qp_bd_offset: 6 * (bit_depth as i32 - 8),
        },
    ))
}

#[derive(Clone)]
pub struct Pps {
    pub sps_id: usize,
    pub dependent_slices: bool,
    pub output_flag_present: bool,
    pub extra_slice_header_bits: u32,
    pub sign_hiding: bool,
    pub cabac_init_present: bool,
    pub num_ref_idx_default: [usize; 2],
    pub init_qp: i32,
    pub constrained_intra_pred: bool,
    pub transform_skip: bool,
    pub cu_qp_delta: bool,
    pub diff_cu_qp_delta_depth: u32,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub slice_chroma_qp_offsets: bool,
    pub weighted_pred: bool,
    pub weighted_bipred: bool,
    pub transquant_bypass: bool,
    pub tiles: bool,
    pub wpp: bool,
    pub col_w: Vec<usize>,
    pub row_h: Vec<usize>,
    pub lf_across_tiles: bool,
    pub lf_across_slices: bool,
    pub deblock_override: bool,
    pub disable_dbf: bool,
    pub beta_offset: i32,
    pub tc_offset: i32,
    pub scaling: Option<ScalingList>,
    pub lists_modification: bool,
    pub log2_parallel_merge: u32,
    pub slice_header_ext: bool,
}

pub fn parse_pps(d: &[u8], spss: &[Option<Sps>]) -> Option<(usize, Pps)> {
    let mut b = Bits::new(d);
    let id = b.ue() as usize;
    let sps_id = b.ue() as usize;
    let sps = spss.get(sps_id)?.as_ref()?;
    let dependent_slices = b.flag();
    let output_flag_present = b.flag();
    let extra_slice_header_bits = b.read(3);
    let sign_hiding = b.flag();
    let cabac_init_present = b.flag();
    let num_ref_idx_default = [b.ue() as usize + 1, b.ue() as usize + 1];
    let init_qp = 26 + b.se();
    let constrained_intra_pred = b.flag();
    let transform_skip = b.flag();
    let cu_qp_delta = b.flag();
    let diff_cu_qp_delta_depth = if cu_qp_delta { b.ue() } else { 0 };
    let cb_qp_offset = b.se();
    let cr_qp_offset = b.se();
    let slice_chroma_qp_offsets = b.flag();
    let weighted_pred = b.flag();
    let weighted_bipred = b.flag();
    let transquant_bypass = b.flag();
    let tiles = b.flag();
    let wpp = b.flag();
    let mut col_w = vec![sps.ctb_w];
    let mut row_h = vec![sps.ctb_h];
    let mut lf_across_tiles = true;
    if tiles {
        let cols = b.ue() as usize + 1;
        let rows = b.ue() as usize + 1;
        if cols > sps.ctb_w || rows > sps.ctb_h {
            return None;
        }
        if b.flag() {
            col_w = (0..cols).map(|i| (i + 1) * sps.ctb_w / cols - i * sps.ctb_w / cols).collect();
            row_h = (0..rows).map(|i| (i + 1) * sps.ctb_h / rows - i * sps.ctb_h / rows).collect();
        } else {
            col_w.clear();
            let mut sum = 0;
            for _ in 0..cols - 1 {
                let w = b.ue() as usize + 1;
                sum += w;
                col_w.push(w);
            }
            if sum >= sps.ctb_w {
                return None;
            }
            col_w.push(sps.ctb_w - sum);
            row_h.clear();
            sum = 0;
            for _ in 0..rows - 1 {
                let h = b.ue() as usize + 1;
                sum += h;
                row_h.push(h);
            }
            if sum >= sps.ctb_h {
                return None;
            }
            row_h.push(sps.ctb_h - sum);
        }
        lf_across_tiles = b.flag();
    }
    let lf_across_slices = b.flag();
    let (mut deblock_override, mut disable_dbf, mut beta_offset, mut tc_offset) = (false, false, 0, 0);
    if b.flag() {
        deblock_override = b.flag();
        disable_dbf = b.flag();
        if !disable_dbf {
            beta_offset = 2 * b.se();
            tc_offset = 2 * b.se();
        }
    }
    let mut scaling = None;
    if b.flag() {
        let mut s = ScalingList::default_lists();
        s.parse(&mut b);
        scaling = Some(s);
    }
    let lists_modification = b.flag();
    let log2_parallel_merge = b.ue() + 2;
    let slice_header_ext = b.flag();
    Some((
        id,
        Pps {
            sps_id,
            dependent_slices,
            output_flag_present,
            extra_slice_header_bits,
            sign_hiding,
            cabac_init_present,
            num_ref_idx_default,
            init_qp,
            constrained_intra_pred,
            transform_skip,
            cu_qp_delta,
            diff_cu_qp_delta_depth,
            cb_qp_offset,
            cr_qp_offset,
            slice_chroma_qp_offsets,
            weighted_pred,
            weighted_bipred,
            transquant_bypass,
            tiles,
            wpp,
            col_w,
            row_h,
            lf_across_tiles,
            lf_across_slices,
            deblock_override,
            disable_dbf,
            beta_offset,
            tc_offset,
            scaling,
            lists_modification,
            log2_parallel_merge,
            slice_header_ext,
        },
    ))
}

pub const SLICE_B: u32 = 0;
pub const SLICE_P: u32 = 1;
pub const SLICE_I: u32 = 2;

#[derive(Clone, Default)]
pub struct Slice {
    pub first_in_pic: bool,
    pub no_output_of_prior: bool,
    pub pps_id: usize,
    pub dependent: bool,
    pub segment_addr: usize,
    pub slice_addr: usize,
    pub kind: u32,
    pub pic_output: bool,
    pub poc_lsb: u32,
    pub st_rps: StRps,
    pub lt_poc: Vec<i32>,
    pub lt_used: Vec<bool>,
    pub lt_msb_present: Vec<bool>,
    /// For long-term pictures given with their full POC: DeltaPocMsbCycleLt.
    pub lt_delta_msb: Vec<i64>,
    pub temporal_mvp: bool,
    pub sao_luma: bool,
    pub sao_chroma: bool,
    pub num_ref: [usize; 2],
    pub list_entry: [[usize; 16]; 2],
    pub rpl_mod: [bool; 2],
    pub mvd_l1_zero: bool,
    pub cabac_init: bool,
    pub col_from_l0: bool,
    pub col_ref_idx: usize,
    pub luma_log2_wd: u32,
    pub chroma_log2_wd: u32,
    /// [list][ref] weight, offset (luma), and chroma [cb/cr].
    pub luma_w: [[i32; 16]; 2],
    pub luma_o: [[i32; 16]; 2],
    pub chroma_w: [[[i32; 2]; 16]; 2],
    pub chroma_o: [[[i32; 2]; 16]; 2],
    pub weighted: bool,
    pub max_merge: usize,
    pub qp: i32,
    pub cb_qp_offset: i32,
    pub cr_qp_offset: i32,
    pub disable_dbf: bool,
    pub beta_offset: i32,
    pub tc_offset: i32,
    pub lf_across_slices: bool,
    pub entry_points: Vec<usize>,
    /// Byte offset of the slice data in the (unescaped) NAL payload.
    pub data_offset: usize,
}

/// `nal_type`: the NAL unit type; `irap`, `idr` from it. Returns None for unusable headers.
pub fn parse_slice(d: &[u8], nal_type: u32, spss: &[Option<Sps>], ppss: &[Option<Pps>], prev: Option<&Slice>) -> Option<Slice> {
    let mut b = Bits::new(d);
    let irap = (16..=23).contains(&nal_type);
    let idr = nal_type == 19 || nal_type == 20;
    let mut s = Slice { first_in_pic: b.flag(), ..Default::default() };
    if irap {
        s.no_output_of_prior = b.flag();
    }
    s.pps_id = b.ue() as usize;
    let pps = ppss.get(s.pps_id)?.as_ref()?;
    let sps = spss.get(pps.sps_id)?.as_ref()?;
    if !s.first_in_pic {
        if pps.dependent_slices {
            s.dependent = b.flag();
        }
        let n = sps.ctb_w * sps.ctb_h;
        let bits = usize::BITS - (n - 1).leading_zeros();
        s.segment_addr = b.read(bits) as usize;
        if s.segment_addr >= n {
            return None;
        }
    }
    if s.dependent {
        // Everything but the address and entry points comes from the previous slice.
        let p = prev?;
        let addr = s.segment_addr;
        s = Slice { dependent: true, segment_addr: addr, first_in_pic: false, ..p.clone() };
    } else {
        s.slice_addr = s.segment_addr;
        b.skip(pps.extra_slice_header_bits as usize);
        s.kind = b.ue();
        if s.kind > 2 {
            return None;
        }
        s.pic_output = !pps.output_flag_present || b.flag();
        if !idr {
            s.poc_lsb = b.read(sps.log2_max_poc_lsb);
            if !b.flag() {
                s.st_rps = parse_st_rps(&mut b, &sps.st_rps, sps.st_rps.len(), true)?;
            } else {
                let n = sps.st_rps.len();
                if n == 0 {
                    return None;
                }
                let bits = if n > 1 { usize::BITS - (n - 1).leading_zeros() } else { 0 };
                let idx = b.read(bits) as usize;
                s.st_rps = *sps.st_rps.get(idx)?;
            }
            if sps.long_term {
                let nb_sps = if !sps.lt_poc_lsb.is_empty() { b.ue() as usize } else { 0 };
                let nb_sh = b.ue() as usize;
                if nb_sps > sps.lt_poc_lsb.len() || nb_sps + nb_sh > 32 {
                    return None;
                }
                let mut prev_msb = 0i64;
                for i in 0..nb_sps + nb_sh {
                    let (poc, used);
                    if i < nb_sps {
                        let n = sps.lt_poc_lsb.len();
                        let idx = if n > 1 { b.read(usize::BITS - (n - 1).leading_zeros()) as usize } else { 0 };
                        poc = sps.lt_poc_lsb[idx] as i64;
                        used = sps.lt_used[idx];
                    } else {
                        poc = b.read(sps.log2_max_poc_lsb) as i64;
                        used = b.flag();
                    }
                    let msb = b.flag();
                    if msb {
                        let mut delta = b.ue() as i64;
                        if i != 0 && i != nb_sps {
                            delta += prev_msb;
                        }
                        s.lt_delta_msb.push(delta);
                        prev_msb = delta;
                    } else {
                        s.lt_delta_msb.push(0);
                    }
                    s.lt_poc.push(poc as i32);
                    s.lt_used.push(used);
                    s.lt_msb_present.push(msb);
                }
            }
            s.temporal_mvp = sps.temporal_mvp && b.flag();
        }
        if sps.sao {
            s.sao_luma = b.flag();
            if sps.chroma != 0 {
                s.sao_chroma = b.flag();
            }
        }
        if s.kind != SLICE_I {
            s.num_ref[0] = pps.num_ref_idx_default[0];
            if s.kind == SLICE_B {
                s.num_ref[1] = pps.num_ref_idx_default[1];
            }
            if b.flag() {
                s.num_ref[0] = b.ue() as usize + 1;
                if s.kind == SLICE_B {
                    s.num_ref[1] = b.ue() as usize + 1;
                }
            }
            if s.num_ref[0] > 16 || s.num_ref[1] > 16 {
                return None;
            }
            let nb_refs = (0..s.st_rps.num_delta).filter(|&i| s.st_rps.used & (1 << i) != 0).count() + s.lt_used.iter().filter(|&&u| u).count();
            if nb_refs == 0 {
                return None;
            }
            if pps.lists_modification && nb_refs > 1 {
                let bits = usize::BITS - (nb_refs - 1).leading_zeros();
                for l in 0..if s.kind == SLICE_B { 2 } else { 1 } {
                    s.rpl_mod[l] = b.flag();
                    if s.rpl_mod[l] {
                        for i in 0..s.num_ref[l] {
                            s.list_entry[l][i] = b.read(bits) as usize;
                        }
                    }
                }
            }
            if s.kind == SLICE_B {
                s.mvd_l1_zero = b.flag();
            }
            s.cabac_init = pps.cabac_init_present && b.flag();
            s.col_from_l0 = true;
            if s.temporal_mvp {
                if s.kind == SLICE_B {
                    s.col_from_l0 = b.flag();
                }
                let l = if s.col_from_l0 { 0 } else { 1 };
                if s.num_ref[l] > 1 {
                    s.col_ref_idx = b.ue() as usize;
                }
            }
            if (pps.weighted_pred && s.kind == SLICE_P) || (pps.weighted_bipred && s.kind == SLICE_B) {
                s.weighted = true;
                s.luma_log2_wd = b.ue().min(7);
                s.chroma_log2_wd = if sps.chroma != 0 { (s.luma_log2_wd as i32 + b.se()).clamp(0, 7) as u32 } else { 0 };
                for l in 0..if s.kind == SLICE_B { 2 } else { 1 } {
                    let n = s.num_ref[l];
                    let lf = b.read(n as u32);
                    let cf = if sps.chroma != 0 { b.read(n as u32) } else { 0 };
                    for i in 0..n {
                        let bit = 1 << (n - 1 - i);
                        s.luma_w[l][i] = 1 << s.luma_log2_wd;
                        s.luma_o[l][i] = 0;
                        if lf & bit != 0 {
                            s.luma_w[l][i] += b.se();
                            s.luma_o[l][i] = b.se();
                        }
                        for j in 0..2 {
                            s.chroma_w[l][i][j] = 1 << s.chroma_log2_wd;
                            s.chroma_o[l][i][j] = 0;
                        }
                        if cf & bit != 0 {
                            for j in 0..2 {
                                let dw = b.se();
                                let doff = b.se();
                                let w = (1 << s.chroma_log2_wd) + dw;
                                s.chroma_w[l][i][j] = w;
                                s.chroma_o[l][i][j] = (doff - ((128 * w) >> s.chroma_log2_wd) + 128).clamp(-128, 127);
                            }
                        }
                    }
                }
            }
            s.max_merge = 5usize.saturating_sub(b.ue() as usize).max(1);
        }
        s.qp = pps.init_qp + b.se();
        if pps.slice_chroma_qp_offsets {
            s.cb_qp_offset = b.se();
            s.cr_qp_offset = b.se();
        }
        s.disable_dbf = pps.disable_dbf;
        s.beta_offset = pps.beta_offset;
        s.tc_offset = pps.tc_offset;
        if pps.deblock_override && b.flag() {
            s.disable_dbf = b.flag();
            if !s.disable_dbf {
                s.beta_offset = 2 * b.se();
                s.tc_offset = 2 * b.se();
            }
        }
        s.lf_across_slices = pps.lf_across_slices;
        if pps.lf_across_slices && (s.sao_luma || s.sao_chroma || !s.disable_dbf) {
            s.lf_across_slices = b.flag();
        }
    }
    s.entry_points.clear();
    if pps.tiles || pps.wpp {
        let n = b.ue() as usize;
        if n > 4096 {
            return None;
        }
        if n > 0 {
            let len = b.ue() + 1;
            if len > 32 {
                return None;
            }
            for _ in 0..n {
                s.entry_points.push(b.read(len) as usize + 1);
            }
        }
    }
    if pps.slice_header_ext {
        let len = b.ue() as usize;
        b.skip(len * 8);
    }
    b.skip(1); // byte_alignment: a 1 then zeros
    b.align();
    s.data_offset = b.pos / 8;
    Some(s)
}

//! HEVC (H.265) pictures: iPhones (default since 2017), newer Android phones, 4K cameras.
//! Main and Main 10 profiles, 4:2:0, with tiles, wavefront rows and multiple slices.
//! Follows ITU-T H.265, checked against FFmpeg. 10-bit pictures are reduced to 8 bits for
//! display (HDR ones tone-mapped to ordinary brightness).

mod cabac;
mod filter;
mod inter;
mod pred;
mod ps;
mod tab;
mod xform;

use alloc::collections::VecDeque;
use alloc::vec;
use alloc::vec::Vec;

use super::dec::{Pic as OutPic, VideoDec};
use crate::video::bits::{unescape, Bits};
use crate::video::h264d::Frame;
use cabac::*;
use ps::{Pps, Slice, Sps, SLICE_B, SLICE_I, SLICE_P};

const PF_INTRA: u8 = 0;
const PF_L0: u8 = 1;
const PF_L1: u8 = 2;
const PF_BI: u8 = 3;

#[derive(Clone, Copy, Default, PartialEq)]
struct Mvf {
    pred: u8,
    mv: [[i32; 2]; 2],
    refi: [i8; 2],
}

/// A reference list entry: POC, long-term, and the DPB slot.
#[derive(Clone, Copy, Default)]
struct RefEntry {
    poc: i32,
    lt: bool,
    slot: usize,
}

#[derive(Clone, Default)]
struct RefLists {
    l: [Vec<RefEntry>; 2],
}

struct DecPic {
    y: Vec<u16>,
    u: Vec<u16>,
    v: Vec<u16>,
    poc: i32,
    /// 0 unused, 1 short-term, 2 long-term.
    reference: u8,
    output: bool,
    mvf: Vec<Mvf>,
    /// Per CTB: index into `lists`.
    ctb_slice: Vec<u16>,
    lists: Vec<RefLists>,
    in_use: bool,
    sps: Option<Sps>,
}

/// Per-slice values the filters need afterwards.
#[derive(Clone, Copy)]
struct SliceInfo {
    addr: usize,
    disable_dbf: bool,
    lf_across: bool,
    beta: i8,
    tc: i8,
}

/// The picture being decoded and its side information.
struct Cur {
    slot: usize,
    stride: usize,
    cstride: usize,
    /// 4x4 block grid width/height.
    bw: usize,
    bh: usize,
    ipm: Vec<u8>,
    recon: Vec<bool>,
    cbf: Vec<bool>,
    qp: Vec<i8>,
    keep: Vec<bool>,
    /// Edge flags per 4x4 block: bit 0 TU left, bit 1 PU left, bit 2 TU top, bit 3 PU top.
    edge: Vec<u8>,
    ct_depth: Vec<u8>,
    skip: Vec<bool>,
    ctb_slice_addr: Vec<i32>,
    ctb_info: Vec<u16>,
    slices: Vec<SliceInfo>,
    sao: Vec<[filter::Sao; 3]>,
    tile_id: Vec<u16>,
    rs_to_ts: Vec<usize>,
    ts_to_rs: Vec<usize>,
    col_start: Vec<usize>,
    row_start: Vec<usize>,
}

/// Per-CU/TU decoding state.
#[derive(Default)]
struct Cu {
    x: usize,
    y: usize,
    log2: u32,
    intra: bool,
    skip: bool,
    part: u8,
    bypass: bool,
    intra_split: bool,
    max_trafo_depth: u32,
    merge: bool,
    ipm: [u32; 4],
    ipm_c: u32,
}

pub struct HevcDec {
    nal_len: usize,
    spss: Vec<Option<Sps>>,
    ppss: Vec<Option<Pps>>,
    dpb: Vec<DecPic>,
    out: VecDeque<Frame>,
    poc_tid0: i32,
    no_rasl_output: bool,
    first_pic: bool,
    cur: Option<Cur>,
    sh: Option<Slice>,
    lists: RefLists,
    poc: i32,
    // Slice decoding state.
    cabac: Cabac,
    wpp_saved: Option<Cabac>,
    qp_y: i32,
    qpy_pred: i32,
    first_qp_group: bool,
    cu_qp_delta: i32,
    is_cu_qp_delta_coded: bool,
    ct_depth: u32,
    coeffs: Vec<i32>,
    rps: Rps,
    scratch: Scratch,
    cur_sps: Option<Sps>,
    cur_pps: Option<Pps>,
}

fn sps_of<'a>(spss: &'a [Option<Sps>], ppss: &'a [Option<Pps>], pps_id: usize) -> Option<(&'a Sps, &'a Pps)> {
    let p = ppss.get(pps_id)?.as_ref()?;
    let s = spss.get(p.sps_id)?.as_ref()?;
    Some((s, p))
}

impl HevcDec {
    /// `config`: an hvcC record (MP4/MKV), or empty for Annex B streams.
    pub fn new(config: &[u8]) -> HevcDec {
        let mut d = HevcDec {
            nal_len: 0,
            spss: (0..16).map(|_| None).collect(),
            ppss: (0..64).map(|_| None).collect(),
            dpb: Vec::new(),
            out: VecDeque::new(),
            poc_tid0: 0,
            no_rasl_output: true,
            first_pic: true,
            cur: None,
            sh: None,
            lists: RefLists::default(),
            poc: 0,
            cabac: Cabac::new(),
            wpp_saved: None,
            qp_y: 0,
            qpy_pred: 0,
            first_qp_group: true,
            cu_qp_delta: 0,
            is_cu_qp_delta_coded: false,
            ct_depth: 0,
            coeffs: vec![0; 32 * 32],
            rps: Rps::default(),
            scratch: Scratch::new(),
            cur_sps: None,
            cur_pps: None,
        };
        if config.len() > 22 && config[0] == 1 {
            d.nal_len = (config[21] & 3) as usize + 1;
            let mut p = 23;
            for _ in 0..config[22] {
                if p + 3 > config.len() {
                    break;
                }
                let n = u16::from_be_bytes([config[p + 1], config[p + 2]]) as usize;
                p += 3;
                for _ in 0..n {
                    if p + 2 > config.len() {
                        break;
                    }
                    let len = u16::from_be_bytes([config[p], config[p + 1]]) as usize;
                    p += 2;
                    if p + len <= config.len() {
                        let nal = config[p..p + len].to_vec();
                        d.nal(&nal);
                    }
                    p += len;
                }
            }
        }
        d
    }

    fn packet(&mut self, data: &[u8]) {
        if self.nal_len > 0 {
            let mut p = 0;
            while p + self.nal_len <= data.len() {
                let mut len = 0usize;
                for k in 0..self.nal_len {
                    len = (len << 8) | data[p + k] as usize;
                }
                p += self.nal_len;
                if len == 0 || p + len > data.len() {
                    break;
                }
                self.nal(&data[p..p + len]);
                p += len;
            }
        } else {
            // Annex B: start codes.
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
                self.nal(&data[s..e]);
            }
        }
    }

    fn nal(&mut self, raw: &[u8]) {
        if raw.len() < 3 {
            return;
        }
        let kind = ((raw[0] >> 1) & 63) as u32;
        let layer = ((raw[0] & 1) << 5) | (raw[1] >> 3);
        let tid = (raw[1] & 7) as i32 - 1;
        if layer != 0 {
            return;
        }
        let rbsp = unescape(&raw[2..]);
        match kind {
            33 => {
                if let Some((id, s)) = ps::parse_sps(&rbsp) {
                    if id < 16 {
                        self.spss[id] = Some(s);
                    }
                }
            }
            34 => {
                if let Some((id, p)) = ps::parse_pps(&rbsp, &self.spss) {
                    if id < 64 {
                        self.ppss[id] = Some(p);
                    }
                }
            }
            36 | 37 => {
                // End of sequence / stream.
                self.finish_picture();
                self.no_rasl_output = true;
                self.first_pic = true;
            }
            0..=9 | 16..=21 => self.slice(&rbsp, kind, tid),
            _ => {}
        }
    }

    fn slice(&mut self, d: &[u8], kind: u32, tid: i32) {
        let Some(sh) = ps::parse_slice(d, kind, &self.spss, &self.ppss, self.sh.as_ref()) else { return };
        let Some((sps, pps)) = sps_of(&self.spss, &self.ppss, sh.pps_id) else { return };
        let (sps, pps) = (sps.clone(), pps.clone());
        if sh.first_in_pic {
            self.finish_picture();
            let irap = (16..=23).contains(&kind);
            if irap {
                // IDR/BLA, or a CRA that starts the stream: pictures before it can't be used.
                self.no_rasl_output = kind != 21 || self.first_pic;
            }
            // RASL pictures after a CRA we started at have missing references: skip them.
            if (kind == 8 || kind == 9) && self.no_rasl_output {
                self.sh = None;
                return;
            }
            if !self.begin_picture(&sh, &sps, &pps, kind, tid) {
                self.sh = None;
                return;
            }
            self.first_pic = false;
        }
        if self.cur.is_none() {
            return;
        }
        self.sh = Some(sh.clone());
        self.decode_slice(d, &sh, &sps, &pps);
    }

    /// POC, reference picture set, DPB, a new picture buffer.
    fn begin_picture(&mut self, sh: &Slice, sps: &Sps, pps: &Pps, kind: u32, tid: i32) -> bool {
        let irap = (16..=23).contains(&kind);
        let idr = kind == 19 || kind == 20;
        // Picture order count.
        let max_lsb = 1i32 << sps.log2_max_poc_lsb;
        let lsb = sh.poc_lsb as i32;
        let msb = if irap && self.no_rasl_output {
            0
        } else {
            let prev_lsb = self.poc_tid0 & (max_lsb - 1);
            let prev_msb = self.poc_tid0 - prev_lsb;
            if lsb < prev_lsb && prev_lsb - lsb >= max_lsb / 2 {
                prev_msb + max_lsb
            } else if lsb > prev_lsb && lsb - prev_lsb > max_lsb / 2 {
                prev_msb - max_lsb
            } else {
                prev_msb
            }
        };
        let poc = if idr { 0 } else { msb + lsb };
        // The previous TemporalId-0 picture that isn't RADL, RASL or a sub-layer non-reference one.
        if tid == 0 && !matches!(kind, 6..=9) && !(kind <= 14 && kind % 2 == 0) {
            self.poc_tid0 = poc;
        }
        self.poc = poc;
        self.cur_sps = Some(sps.clone());
        self.cur_pps = Some(pps.clone());
        // Output everything waiting at a new coded video sequence.
        if irap && self.no_rasl_output {
            self.bump_all(!sh.no_output_of_prior);
            for p in self.dpb.iter_mut() {
                p.reference = 0;
            }
        }
        // Reference picture set.
        let mut keep = vec![0u8; self.dpb.len()];
        let mut st_before = Vec::new();
        let mut st_after = Vec::new();
        let mut lt_curr = Vec::new();
        if !idr {
            let r = &sh.st_rps;
            for i in 0..r.num_delta {
                let want = poc + r.delta_poc[i];
                let used = r.used & (1 << i) != 0;
                let slot = self.dpb.iter().position(|p| p.in_use && p.reference == 1 && p.poc == want);
                let slot = match slot {
                    Some(s) => s,
                    None => self.missing(sps, want),
                };
                if keep.len() < self.dpb.len() {
                    keep.resize(self.dpb.len(), 0);
                }
                keep[slot] = 1;
                if used {
                    if i < r.num_negative {
                        st_before.push(slot);
                    } else {
                        st_after.push(slot);
                    }
                }
            }
            for i in 0..sh.lt_poc.len() {
                let mut want = sh.lt_poc[i];
                let full = sh.lt_msb_present[i];
                if full {
                    want = want + poc - sh.lt_delta_msb[i] as i32 * max_lsb - lsb;
                }
                let slot = self.dpb.iter().position(|p| p.in_use && p.reference != 0 && if full { p.poc == want } else { p.poc & (max_lsb - 1) == want });
                let slot = match slot {
                    Some(s) => s,
                    None => self.missing(sps, want),
                };
                if keep.len() < self.dpb.len() {
                    keep.resize(self.dpb.len(), 0);
                }
                keep[slot] = 2;
                if sh.lt_used[i] {
                    lt_curr.push(slot);
                }
            }
        }
        keep.resize(self.dpb.len(), 0);
        for (i, p) in self.dpb.iter_mut().enumerate() {
            p.reference = keep[i];
            if !p.output && p.reference == 0 {
                p.in_use = false;
            }
        }
        let _ = (&st_before, &st_after, &lt_curr);
        self.rps = Rps { st_before, st_after, lt_curr };
        // A buffer for the new picture.
        let stride = sps.ctb_w * sps.ctb;
        let height = sps.ctb_h * sps.ctb;
        let cstride = stride / 2;
        let slot = match self.dpb.iter().position(|p| !p.in_use) {
            Some(s) => s,
            None => {
                self.dpb.push(DecPic { y: Vec::new(), u: Vec::new(), v: Vec::new(), poc: 0, reference: 0, output: false, mvf: Vec::new(), ctb_slice: Vec::new(), lists: Vec::new(), in_use: false, sps: None });
                self.dpb.len() - 1
            }
        };
        let bw = stride / 4;
        let bh = height / 4;
        {
            let p = &mut self.dpb[slot];
            p.y = vec![1 << (sps.bit_depth - 1); stride * height];
            p.u = vec![1 << (sps.bit_depth - 1); cstride * height / 2];
            p.v = vec![1 << (sps.bit_depth - 1); cstride * height / 2];
            p.poc = poc;
            p.reference = 1;
            p.output = sh.pic_output;
            p.in_use = true;
            p.mvf = vec![Mvf::default(); bw * bh];
            p.ctb_slice = vec![0; sps.ctb_w * sps.ctb_h];
            p.lists.clear();
            p.sps = Some(sps.clone());
        }
        // Tile scan.
        let (cw, ch) = (sps.ctb_w, sps.ctb_h);
        let mut col_start = vec![0];
        for &w in &pps.col_w {
            col_start.push(col_start.last().unwrap() + w);
        }
        let mut row_start = vec![0];
        for &h in &pps.row_h {
            row_start.push(row_start.last().unwrap() + h);
        }
        let mut ts_to_rs = Vec::with_capacity(cw * ch);
        let mut tile_id = vec![0u16; cw * ch];
        let mut tid_n = 0u16;
        for tr in 0..pps.row_h.len() {
            for tc in 0..pps.col_w.len() {
                for y in row_start[tr]..row_start[tr + 1] {
                    for x in col_start[tc]..col_start[tc + 1] {
                        ts_to_rs.push(y * cw + x);
                        tile_id[y * cw + x] = tid_n;
                    }
                }
                tid_n += 1;
            }
        }
        let mut rs_to_ts = vec![0; cw * ch];
        for (ts, &rs) in ts_to_rs.iter().enumerate() {
            rs_to_ts[rs] = ts;
        }
        self.cur = Some(Cur {
            slot,
            stride,
            cstride,
            bw,
            bh,
            ipm: vec![1; bw * bh],
            recon: vec![false; bw * bh],
            cbf: vec![false; bw * bh],
            qp: vec![0; bw * bh],
            keep: vec![false; bw * bh],
            edge: vec![0; bw * bh],
            ct_depth: vec![0; bw * bh],
            skip: vec![false; bw * bh],
            ctb_slice_addr: vec![-1; cw * ch],
            ctb_info: vec![0; cw * ch],
            slices: Vec::new(),
            sao: vec![[filter::Sao::default(); 3]; cw * ch],
            tile_id,
            rs_to_ts,
            ts_to_rs,
            col_start,
            row_start,
        });
        let _ = irap;
        true
    }

    /// A reference that isn't there (after a jump or a damaged stream): a grey picture.
    fn missing(&mut self, sps: &Sps, poc: i32) -> usize {
        let stride = sps.ctb_w * sps.ctb;
        let height = sps.ctb_h * sps.ctb;
        let g = 1u16 << (sps.bit_depth - 1);
        let bw = stride / 4;
        let p = DecPic {
            y: vec![g; stride * height],
            u: vec![g; stride * height / 4],
            v: vec![g; stride * height / 4],
            poc,
            reference: 1,
            output: false,
            mvf: vec![Mvf::default(); bw * (height / 4)],
            ctb_slice: vec![0; sps.ctb_w * sps.ctb_h],
            lists: vec![RefLists::default()],
            in_use: true,
            sps: Some(sps.clone()),
        };
        if let Some(i) = self.dpb.iter().position(|d| !d.in_use) {
            self.dpb[i] = p;
            i
        } else {
            self.dpb.push(p);
            self.dpb.len() - 1
        }
    }

    /// Build the slice's reference lists.
    fn build_lists(&mut self, sh: &Slice) {
        let mut temp: Vec<(usize, bool)> = Vec::new();
        let total = self.rps.st_before.len() + self.rps.st_after.len() + self.rps.lt_curr.len();
        self.lists = RefLists::default();
        if total == 0 {
            return;
        }
        for l in 0..if sh.kind == SLICE_B { 2 } else if sh.kind == SLICE_P { 1 } else { 0 } {
            temp.clear();
            let n = sh.num_ref[l].max(total);
            while temp.len() < n {
                let (a, b) = if l == 0 { (&self.rps.st_before, &self.rps.st_after) } else { (&self.rps.st_after, &self.rps.st_before) };
                for &s in a.iter().chain(b.iter()) {
                    if temp.len() < n {
                        temp.push((s, false));
                    }
                }
                for &s in &self.rps.lt_curr {
                    if temp.len() < n {
                        temp.push((s, true));
                    }
                }
            }
            for i in 0..sh.num_ref[l] {
                let (slot, lt) = temp[if sh.rpl_mod[l] { sh.list_entry[l][i].min(temp.len() - 1) } else { i }];
                self.lists.l[l].push(RefEntry { poc: self.dpb[slot].poc, lt, slot });
            }
        }
    }

    fn bump_all(&mut self, output: bool) {
        loop {
            let next = self.dpb.iter().enumerate().filter(|(_, p)| p.in_use && p.output).min_by_key(|(_, p)| p.poc).map(|(i, _)| i);
            let Some(i) = next else { break };
            if output {
                self.emit(i);
            }
            self.dpb[i].output = false;
            if self.dpb[i].reference == 0 {
                self.dpb[i].in_use = false;
            }
        }
    }

    fn emit(&mut self, slot: usize) {
        let p = &self.dpb[slot];
        let Some(sps) = p.sps.as_ref() else { return };
        let stride = sps.ctb_w * sps.ctb;
        let cs = stride / 2;
        let (l, r, t, b) = (sps.conf[0], sps.conf[1], sps.conf[2], sps.conf[3]);
        let w = sps.width - l - r;
        let h = sps.height - t - b;
        let ow = w.next_multiple_of(2);
        let oh = h.next_multiple_of(2);
        let shift = sps.bit_depth - 8;
        let hdr = sps.transfer == 16 || sps.transfer == 18;
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
        for yy in 0..h {
            for xx in 0..w {
                let s = conv(p.y[(yy + t) * stride + xx + l]);
                y[yy * ow + xx] = if hdr { tone(s) } else { s };
            }
        }
        for yy in 0..h / 2 {
            for xx in 0..w / 2 {
                let o = (yy + t / 2) * cs + xx + l / 2;
                u[yy * (ow / 2) + xx] = conv(p.u[o]);
                v[yy * (ow / 2) + xx] = conv(p.v[o]);
            }
        }
        let bt709 = sps.matrix == 1 || sps.matrix == 9 || (sps.matrix == 2 && sps.height > 576);
        self.out.push_back(Frame { width: w, height: h, y, u, v, stride: ow, crop_x: 0, crop_y: 0, bt709, full_range: sps.full_range, poc: p.poc });
    }

    /// The picture is complete: loop filters, then output what may be shown.
    fn finish_picture(&mut self) {
        let Some(cur) = self.cur.take() else { return };
        let Some(sps) = self.cur_sps.clone() else { return };
        let Some(pps) = self.cur_pps.clone() else { return };
        self.filters(&cur, &sps, &pps);
        // Output in POC order once more than num_reorder are waiting.
        loop {
            let waiting = self.dpb.iter().filter(|p| p.in_use && p.output).count();
            let dpb_full = self.dpb.iter().filter(|p| p.in_use).count() > sps.max_dec_pic_buffering.max(1) + 1;
            if waiting <= sps.num_reorder && !dpb_full {
                break;
            }
            let next = self.dpb.iter().enumerate().filter(|(_, p)| p.in_use && p.output).min_by_key(|(_, p)| p.poc).map(|(i, _)| i);
            let Some(i) = next else { break };
            self.emit(i);
            self.dpb[i].output = false;
            if self.dpb[i].reference == 0 {
                self.dpb[i].in_use = false;
            }
        }
    }
}

/// Simple HDR (PQ/HLG) to SDR brightness mapping of 8-bit luma: lifts the mid-tones that
/// HDR keeps dark for highlights.
pub(crate) fn tone(v: u8) -> u8 {
    // Limited range 16..235: normalise, apply a soft curve, back.
    let x = ((v as i32 - 16).clamp(0, 219)) as u32;
    let y = (x * (219 * 2) / (x + 219)).min(219);
    (y + 16) as u8
}

#[derive(Default)]
struct Rps {
    st_before: Vec<usize>,
    st_after: Vec<usize>,
    lt_curr: Vec<usize>,
}

include!("slice.rs");

/// Work buffers for motion compensation, kept between blocks.
struct Scratch {
    pl: [Vec<i16>; 2],
    pc: [[Vec<i16>; 2]; 2],
    src: Vec<u16>,
    tmp: Vec<i32>,
}

impl Scratch {
    fn new() -> Scratch {
        Scratch { pl: [vec![0; 64 * 64], vec![0; 64 * 64]], pc: [[vec![0; 32 * 32], vec![0; 32 * 32]], [vec![0; 32 * 32], vec![0; 32 * 32]]], src: vec![0; 71 * 71], tmp: vec![0; 71 * 64] }
    }
    fn empty() -> Scratch {
        Scratch { pl: [Vec::new(), Vec::new()], pc: [[Vec::new(), Vec::new()], [Vec::new(), Vec::new()]], src: Vec::new(), tmp: Vec::new() }
    }
}

// HEVC slice data: coding tree units, coding units, prediction and transform units,
// reconstruction, and the loop filter glue. (Included into hevc/mod.rs.)

const LEVEL_SCALE: [i64; 6] = [40, 45, 51, 57, 64, 72];

fn wrap16(v: i32) -> i32 {
    ((v + 32768) & 0xFFFF) - 32768
}

fn mv_scale(mv: [i32; 2], td: i32, tb: i32) -> [i32; 2] {
    let td = td.clamp(-128, 127);
    let tb = tb.clamp(-128, 127);
    if td == 0 {
        return mv;
    }
    let tx = (0x4000 + (td / 2).abs()) / td;
    let sf = ((tb * tx + 32) >> 6).clamp(-4096, 4095);
    let s = |v: i32| {
        let p = sf * v;
        ((p + 127 + (p < 0) as i32) >> 8).clamp(-32768, 32767)
    };
    [s(mv[0]), s(mv[1])]
}

impl HevcDec {
    fn decode_slice(&mut self, d: &[u8], sh: &Slice, sps: &Sps, pps: &Pps) {
        if !sh.dependent {
            self.build_lists(sh);
        }
        let Some(cur) = self.cur.as_mut() else { return };
        if !sh.dependent || cur.slices.is_empty() {
            cur.slices.push(SliceInfo { addr: sh.slice_addr, disable_dbf: sh.disable_dbf, lf_across: sh.lf_across_slices, beta: sh.beta_offset as i8, tc: sh.tc_offset as i8 });
            let lists = self.lists.clone();
            self.dpb[cur.slot].lists.push(lists);
        }
        let slice_idx = (cur.slices.len() - 1) as u16;
        let total = sps.ctb_w * sps.ctb_h;
        if sh.segment_addr >= total {
            return;
        }
        let mut b = Bits::new(d);
        b.pos = sh.data_offset * 8;
        let mut ts = cur.rs_to_ts[sh.segment_addr];
        let init_type = if sh.kind == SLICE_I { 0 } else if (sh.kind == SLICE_P) ^ sh.cabac_init { 1 } else { 2 };
        let ctb_w = sps.ctb_w;
        let first_rs = cur.ts_to_rs[ts];
        if !sh.dependent {
            self.cabac.init_contexts(init_type, sh.qp);
            self.qp_y = sh.qp;
            self.first_qp_group = true;
            self.qpy_pred = sh.qp;
        } else if pps.wpp && first_rs % ctb_w == 0 {
            match self.wpp_saved.clone() {
                Some(s) if ctb_w > 1 => {
                    self.cabac.state = s.state;
                    self.cabac.mps = s.mps;
                }
                _ => self.cabac.init_contexts(init_type, sh.qp),
            }
            self.first_qp_group = true;
        } else if pps.tiles && ts > 0 && cur.tile_id[first_rs] != cur.tile_id[cur.ts_to_rs[ts - 1]] {
            self.cabac.init_contexts(init_type, sh.qp);
            self.first_qp_group = true;
        }
        self.cabac.start(&mut b);
        self.cur_sps = Some(sps.clone());
        self.cur_pps = Some(pps.clone());
        loop {
            let cur = self.cur.as_mut().unwrap();
            let rs = cur.ts_to_rs[ts];
            cur.ctb_slice_addr[rs] = sh.slice_addr as i32;
            cur.ctb_info[rs] = slice_idx;
            let slot = cur.slot;
            self.dpb[slot].ctb_slice[rs] = slice_idx;
            let (cx, cy) = (rs % ctb_w, rs / ctb_w);
            if sps.sao && (sh.sao_luma || sh.sao_chroma) {
                self.sao_syntax(&mut b, sh, sps, cx, cy);
            }
            if self.quadtree(&mut b, sh, sps, pps, cx * sps.ctb, cy * sps.ctb, sps.log2_ctb, 0).is_none() {
                return;
            }
            let end = self.cabac.terminate(&mut b);
            if pps.wpp && cx == 1 {
                self.wpp_saved = Some(self.cabac.clone());
            }
            ts += 1;
            if end || ts >= total {
                break;
            }
            let cur = self.cur.as_ref().unwrap();
            let nrs = cur.ts_to_rs[ts];
            let new_tile = pps.tiles && cur.tile_id[nrs] != cur.tile_id[rs];
            let new_row = pps.wpp && nrs % ctb_w == 0;
            if new_tile || new_row {
                self.cabac.terminate(&mut b);
                self.cabac.finish(&mut b);
                self.cabac.start(&mut b);
                if new_tile || ctb_w == 1 {
                    self.cabac.init_contexts(init_type, sh.qp);
                } else {
                    match self.wpp_saved.as_ref() {
                        Some(s) => {
                            self.cabac.state = s.state;
                            self.cabac.mps = s.mps;
                        }
                        None => self.cabac.init_contexts(init_type, sh.qp),
                    }
                }
                self.first_qp_group = true;
            }
        }
    }

    // ---- Availability ----

    /// Whether the sample at luma (x, y) is in the picture and in the same slice and tile as
    /// the CTB containing (cx, cy).
    fn same_region(&self, sps: &Sps, x: isize, y: isize, cx: usize, cy: usize) -> bool {
        if x < 0 || y < 0 || x as usize >= sps.width || y as usize >= sps.height {
            return false;
        }
        let cur = self.cur.as_ref().unwrap();
        let w = sps.ctb_w;
        let a = (y as usize >> sps.log2_ctb) * w + (x as usize >> sps.log2_ctb);
        let b = (cy >> sps.log2_ctb) * w + (cx >> sps.log2_ctb);
        cur.ctb_slice_addr[a] == cur.ctb_slice_addr[b] && cur.tile_id[a] == cur.tile_id[b]
    }

    fn blk(&self, x: usize, y: usize) -> usize {
        let cur = self.cur.as_ref().unwrap();
        (y >> 2) * cur.bw + (x >> 2)
    }

    fn mvf_at(&self, x: usize, y: usize) -> Mvf {
        let cur = self.cur.as_ref().unwrap();
        self.dpb[cur.slot].mvf[(y >> 2) * cur.bw + (x >> 2)]
    }

    // ---- SAO syntax ----

    fn sao_syntax(&mut self, b: &mut Bits, sh: &Slice, sps: &Sps, cx: usize, cy: usize) {
        let c = &mut self.cabac;
        let cur = self.cur.as_ref().unwrap();
        let w = sps.ctb_w;
        let rs = cy * w + cx;
        let ok = |n: usize| cur.ctb_slice_addr[n] == sh.slice_addr as i32 && cur.tile_id[n] == cur.tile_id[rs];
        let mut merge_left = false;
        let mut merge_up = false;
        if cx > 0 && ok(rs - 1) {
            merge_left = c.bin(b, SAO_MERGE) != 0;
        }
        if cy > 0 && !merge_left && ok(rs - w) {
            merge_up = c.bin(b, SAO_MERGE) != 0;
        }
        let mut p = [filter::Sao::default(); 3];
        if merge_left || merge_up {
            p = cur.sao[if merge_left { rs - 1 } else { rs - w }];
        } else {
            for ci in 0..if sps.chroma != 0 { 3 } else { 1 } {
                let on = if ci == 0 { sh.sao_luma } else { sh.sao_chroma };
                if !on {
                    continue;
                }
                if ci == 2 {
                    p[2].kind = p[1].kind;
                    p[2].class = p[1].class;
                } else {
                    p[ci].kind = if c.bin(b, SAO_TYPE) == 0 { 0 } else if c.bypass(b) == 0 { 1 } else { 2 };
                }
                if p[ci].kind == 0 {
                    continue;
                }
                let bd = if ci == 0 { sps.bit_depth } else { sps.bit_depth_c };
                let cmax = (1 << (bd.min(10) - 5)) - 1;
                let mut abs = [0i32; 4];
                for a in abs.iter_mut() {
                    while *a < cmax && c.bypass(b) != 0 {
                        *a += 1;
                    }
                }
                let scale = bd - bd.min(10);
                if p[ci].kind == 1 {
                    for a in abs.iter_mut() {
                        if *a != 0 && c.bypass(b) != 0 {
                            *a = -*a;
                        }
                    }
                    p[ci].band = c.bypass_bits(b, 5) as u8;
                } else {
                    abs[2] = -abs[2];
                    abs[3] = -abs[3];
                    if ci != 2 {
                        p[ci].class = c.bypass_bits(b, 2) as u8;
                    }
                }
                p[ci].offset = [0, abs[0] << scale, abs[1] << scale, abs[2] << scale, abs[3] << scale];
            }
        }
        if !sh.sao_luma {
            p[0].kind = 0;
        }
        if !sh.sao_chroma {
            p[1].kind = 0;
            p[2].kind = 0;
        }
        self.cur.as_mut().unwrap().sao[rs] = p;
    }

    // ---- Coding tree ----

    #[allow(clippy::too_many_arguments)]
    fn quadtree(&mut self, b: &mut Bits, sh: &Slice, sps: &Sps, pps: &Pps, x0: usize, y0: usize, log2: u32, depth: u32) -> Option<bool> {
        let size = 1usize << log2;
        self.ct_depth = depth;
        let split = if x0 + size <= sps.width && y0 + size <= sps.height && log2 > sps.log2_min_cb {
            let mut inc = 0;
            let cur = self.cur.as_ref().unwrap();
            if self.same_region(sps, x0 as isize - 1, y0 as isize, x0, y0) && cur.ct_depth[self.blk(x0 - 1, y0)] as u32 > depth {
                inc += 1;
            }
            if self.same_region(sps, x0 as isize, y0 as isize - 1, x0, y0) && cur.ct_depth[self.blk(x0, y0 - 1)] as u32 > depth {
                inc += 1;
            }
            self.cabac.bin(b, SPLIT_CU + inc) != 0
        } else {
            log2 > sps.log2_min_cb
        };
        if pps.cu_qp_delta && log2 >= sps.log2_ctb - pps.diff_cu_qp_delta_depth {
            self.is_cu_qp_delta_coded = false;
            self.cu_qp_delta = 0;
        }
        let qg_mask = (1usize << (sps.log2_ctb - pps.diff_cu_qp_delta_depth)) - 1;
        if split {
            let h = size / 2;
            let (x1, y1) = (x0 + h, y0 + h);
            let mut more = self.quadtree(b, sh, sps, pps, x0, y0, log2 - 1, depth + 1)?;
            if more && x1 < sps.width {
                more = self.quadtree(b, sh, sps, pps, x1, y0, log2 - 1, depth + 1)?;
            }
            if more && y1 < sps.height {
                more = self.quadtree(b, sh, sps, pps, x0, y1, log2 - 1, depth + 1)?;
            }
            if more && x1 < sps.width && y1 < sps.height {
                more = self.quadtree(b, sh, sps, pps, x1, y1, log2 - 1, depth + 1)?;
            }
            if (x0 + size) & qg_mask == 0 && (y0 + size) & qg_mask == 0 {
                self.qpy_pred = self.qp_y;
            }
            return Some(more);
        }
        self.coding_unit(b, sh, sps, pps, x0, y0, log2)?;
        Some(true)
    }

    fn set_qpy(&mut self, sps: &Sps, pps: &Pps, sh: &Slice, xb: usize, yb: usize) {
        let ctb_mask = sps.ctb - 1;
        let qg_mask = (1usize << (sps.log2_ctb - pps.diff_cu_qp_delta_depth)) - 1;
        let xq = xb - (xb & qg_mask);
        let yq = yb - (yb & qg_mask);
        let avail_a = (xb & ctb_mask) != 0 && (xq & ctb_mask) != 0;
        let avail_b = (yb & ctb_mask) != 0 && (yq & ctb_mask) != 0;
        let pred = if self.first_qp_group || (xq == 0 && yq == 0) {
            self.first_qp_group = !self.is_cu_qp_delta_coded;
            sh.qp
        } else {
            self.qpy_pred
        };
        let cur = self.cur.as_ref().unwrap();
        let qa = if avail_a { cur.qp[self.blk(xq - 1, yq)] as i32 } else { pred };
        let qb = if avail_b { cur.qp[self.blk(xq, yq - 1)] as i32 } else { pred };
        let q = (qa + qb + 1) >> 1;
        self.qp_y = if self.cu_qp_delta != 0 {
            let off = sps.qp_bd_offset;
            (q + self.cu_qp_delta + 52 + 2 * off).rem_euclid(52 + off) - off
        } else {
            q
        };
    }

    #[allow(clippy::too_many_arguments)]
    fn coding_unit(&mut self, b: &mut Bits, sh: &Slice, sps: &Sps, pps: &Pps, x0: usize, y0: usize, log2: u32) -> Option<()> {
        let size = 1usize << log2;
        let mut cu = Cu { x: x0, y: y0, log2, intra: true, ..Default::default() };
        cu.ipm = [1; 4];
        if pps.transquant_bypass {
            cu.bypass = self.cabac.bin(b, TRANSQUANT_BYPASS) != 0;
        }
        if sh.kind != SLICE_I {
            let mut inc = 0;
            let cur = self.cur.as_ref().unwrap();
            if self.same_region(sps, x0 as isize - 1, y0 as isize, x0, y0) && cur.skip[self.blk(x0 - 1, y0)] {
                inc += 1;
            }
            if self.same_region(sps, x0 as isize, y0 as isize - 1, x0, y0) && cur.skip[self.blk(x0, y0 - 1)] {
                inc += 1;
            }
            cu.skip = self.cabac.bin(b, SKIP + inc) != 0;
            cu.intra = false;
        }
        // Mark the CU's area (depth, skip) now: later CUs read these.
        self.fill(x0, y0, size, size, |c, i| {
            c.ct_depth[i] = 0;
            c.skip[i] = false;
        });
        let depth = self.ct_depth as u8;
        let skip = cu.skip;
        self.fill(x0, y0, size, size, |c, i| {
            c.ct_depth[i] = depth;
            c.skip[i] = skip;
        });
        let mut pcm = false;
        if cu.skip {
            self.set_intra_default(x0, y0, size, false);
            cu.merge = true;
            self.prediction_unit(b, sh, sps, pps, &cu, x0, y0, size, size, 0)?;
            self.mark_tu(x0, y0, size, size);
        } else {
            if sh.kind != SLICE_I {
                cu.intra = self.cabac.bin(b, PRED_MODE) != 0;
            }
            cu.part = 0;
            if !cu.intra || log2 == sps.log2_min_cb {
                cu.part = self.cabac.part_mode(b, cu.intra, log2, sps.log2_min_cb, sps.amp);
                cu.intra_split = cu.part == 3 && cu.intra;
            }
            if cu.intra {
                if cu.part == 0 && sps.pcm && log2 >= sps.log2_min_pcm && log2 <= sps.log2_max_pcm {
                    pcm = self.cabac.terminate(b);
                }
                if pcm {
                    self.set_intra_default(x0, y0, size, true);
                    self.pcm_sample(b, sps, x0, y0, log2);
                    if sps.pcm_loop_filter_disabled {
                        self.fill(x0, y0, size, size, |c, i| c.keep[i] = true);
                    }
                } else {
                    self.intra_modes(b, sps, &mut cu, x0, y0, log2);
                }
            } else {
                self.set_intra_default(x0, y0, size, false);
                let (s, h) = (size, size / 2);
                let q = size / 4;
                let parts: &[(usize, usize, usize, usize)] = match cu.part {
                    0 => &[(0, 0, s, s)],
                    1 => &[(0, 0, s, h), (0, h, s, h)],
                    2 => &[(0, 0, h, s), (h, 0, h, s)],
                    4 => &[(0, 0, s, q), (0, q, s, s - q)],
                    5 => &[(0, 0, s, s - q), (0, s - q, s, q)],
                    6 => &[(0, 0, q, s), (q, 0, s - q, s)],
                    7 => &[(0, 0, s - q, s), (s - q, 0, q, s)],
                    _ => &[(0, 0, h, h), (h, 0, h, h), (0, h, h, h), (h, h, h, h)],
                };
                for (i, &(px, py, pw, ph)) in parts.iter().enumerate() {
                    let merge = self.cabac.bin(b, MERGE_FLAG) != 0;
                    cu.merge = merge;
                    self.prediction_unit(b, sh, sps, pps, &cu, x0 + px, y0 + py, pw, ph, i)?;
                }
            }
            if !pcm {
                let root_cbf = if !cu.intra && !(cu.part == 0 && cu.merge) { self.cabac.bin(b, NO_RESIDUAL) != 0 } else { true };
                if root_cbf {
                    cu.max_trafo_depth = if cu.intra { sps.max_th_depth_intra + cu.intra_split as u32 } else { sps.max_th_depth_inter };
                    self.transform_tree(b, sh, sps, pps, &cu, x0, y0, x0, y0, log2, 0, 0, [false, false])?;
                } else {
                    self.mark_tu(x0, y0, size, size);
                }
            }
        }
        if pps.cu_qp_delta && !self.is_cu_qp_delta_coded {
            self.set_qpy(sps, pps, sh, x0, y0);
        }
        let qp = self.qp_y as i8;
        let bypass = cu.bypass;
        self.fill(x0, y0, size, size, |c, i| {
            c.qp[i] = qp;
            c.recon[i] = true;
            if bypass {
                c.keep[i] = true;
            }
        });
        let qg_mask = (1usize << (sps.log2_ctb - pps.diff_cu_qp_delta_depth)) - 1;
        if (x0 + size) & qg_mask == 0 && (y0 + size) & qg_mask == 0 {
            self.qpy_pred = self.qp_y;
        }
        Some(())
    }

    /// Apply `f` to every 4x4 block of the area that lies in the picture grid.
    fn fill(&mut self, x0: usize, y0: usize, w: usize, h: usize, mut f: impl FnMut(&mut Cur, usize)) {
        let cur = self.cur.as_mut().unwrap();
        for by in (y0 >> 2)..((y0 + h) >> 2).min(cur.bh) {
            for bx in (x0 >> 2)..((x0 + w) >> 2).min(cur.bw) {
                let i = by * cur.bw + bx;
                f(cur, i);
            }
        }
    }

    fn set_mvf(&mut self, x0: usize, y0: usize, w: usize, h: usize, m: Mvf) {
        let cur = self.cur.as_ref().unwrap();
        let (slot, bw, bh) = (cur.slot, cur.bw, cur.bh);
        let mvf = &mut self.dpb[slot].mvf;
        for by in (y0 >> 2)..((y0 + h) >> 2).min(bh) {
            for bx in (x0 >> 2)..((x0 + w) >> 2).min(bw) {
                mvf[by * bw + bx] = m;
            }
        }
    }

    fn set_intra_default(&mut self, x0: usize, y0: usize, size: usize, intra: bool) {
        self.fill(x0, y0, size, size, |c, i| c.ipm[i] = 1);
        if intra {
            self.set_mvf(x0, y0, size, size, Mvf::default());
        }
    }

    /// Mark a transform block's left and top edges (for deblocking).
    fn mark_tu(&mut self, x0: usize, y0: usize, w: usize, h: usize) {
        let cur = self.cur.as_mut().unwrap();
        if x0 % 8 == 0 {
            for by in (y0 >> 2)..((y0 + h) >> 2).min(cur.bh) {
                cur.edge[by * cur.bw + (x0 >> 2)] |= 1;
            }
        }
        if y0 % 8 == 0 {
            for bx in (x0 >> 2)..((x0 + w) >> 2).min(cur.bw) {
                cur.edge[(y0 >> 2) * cur.bw + bx] |= 4;
            }
        }
    }

    fn mark_pu(&mut self, x0: usize, y0: usize, w: usize, h: usize) {
        let cur = self.cur.as_mut().unwrap();
        if x0 % 8 == 0 {
            for by in (y0 >> 2)..((y0 + h) >> 2).min(cur.bh) {
                cur.edge[by * cur.bw + (x0 >> 2)] |= 2;
            }
        }
        if y0 % 8 == 0 {
            for bx in (x0 >> 2)..((x0 + w) >> 2).min(cur.bw) {
                cur.edge[(y0 >> 2) * cur.bw + bx] |= 8;
            }
        }
    }

    fn pcm_sample(&mut self, b: &mut Bits, sps: &Sps, x0: usize, y0: usize, log2: u32) {
        self.cabac.finish(b);
        let size = 1usize << log2;
        let cur = self.cur.as_ref().unwrap();
        let (slot, stride, cs) = (cur.slot, cur.stride, cur.cstride);
        let p = &mut self.dpb[slot];
        for yy in 0..size {
            for xx in 0..size {
                p.y[(y0 + yy) * stride + x0 + xx] = (b.read(sps.pcm_bd) << (sps.bit_depth - sps.pcm_bd)) as u16;
            }
        }
        if sps.chroma != 0 {
            for plane in [&mut p.u, &mut p.v] {
                for yy in 0..size / 2 {
                    for xx in 0..size / 2 {
                        plane[(y0 / 2 + yy) * cs + x0 / 2 + xx] = (b.read(sps.pcm_bd_c) << (sps.bit_depth_c - sps.pcm_bd_c)) as u16;
                    }
                }
            }
        }
        self.cabac.start(b);
        self.mark_tu(x0, y0, size, size);
    }

    // ---- Intra modes ----

    fn intra_modes(&mut self, b: &mut Bits, sps: &Sps, cu: &mut Cu, x0: usize, y0: usize, log2: u32) {
        let split = cu.part == 3;
        let pb = (1usize << log2) >> split as u32;
        let side = split as usize + 1;
        let mut prev = [false; 4];
        for p in prev.iter_mut().take(side * side) {
            *p = self.cabac.bin(b, PREV_INTRA_LUMA) != 0;
        }
        for i in 0..side {
            for j in 0..side {
                let k = 2 * i + j;
                let (mut mpm, mut rem) = (0, 0);
                if prev[k] {
                    while mpm < 2 && self.cabac.bypass(b) != 0 {
                        mpm += 1;
                    }
                } else {
                    rem = self.cabac.bypass_bits(b, 5);
                }
                let (px, py) = (x0 + pb * j, y0 + pb * i);
                let (left, up, slot) = {
                    let cur = self.cur.as_ref().unwrap();
                    let left = if self.same_region(sps, px as isize - 1, py as isize, px, py) { cur.ipm[self.blk(px - 1, py)] as u32 } else { 1 };
                    let ctb_top = (py >> sps.log2_ctb) << sps.log2_ctb;
                    let up = if py > ctb_top && self.same_region(sps, px as isize, py as isize - 1, px, py) { cur.ipm[self.blk(px, py - 1)] as u32 } else { 1 };
                    (left, up, cur.slot)
                };
                let mut cand = if left == up {
                    if left < 2 {
                        [0, 1, 26]
                    } else {
                        [left, 2 + ((left - 2 + 31) & 31), 2 + ((left - 2 + 1) & 31)]
                    }
                } else {
                    let third = if left != 0 && up != 0 { 0 } else if left != 1 && up != 1 { 1 } else { 26 };
                    [left, up, third]
                };
                let mode = if prev[k] {
                    cand[mpm]
                } else {
                    cand.sort_unstable();
                    let mut m = rem;
                    for &c in &cand {
                        if m >= c {
                            m += 1;
                        }
                    }
                    m
                };
                cu.ipm[k] = mode;
                self.fill(px, py, pb.max(4), pb.max(4), |c, i| c.ipm[i] = mode as u8);
                self.set_mvf(px, py, pb.max(4), pb.max(4), Mvf::default());
                let _ = slot;
            }
        }
        if sps.chroma != 0 {
            let cm = if self.cabac.bin(b, CHROMA_PRED) == 0 { 4 } else { self.cabac.bypass_bits(b, 2) };
            const TABLE: [u32; 4] = [0, 26, 10, 1];
            cu.ipm_c = if cm == 4 {
                cu.ipm[0]
            } else if cu.ipm[0] == TABLE[cm as usize] {
                34
            } else {
                TABLE[cm as usize]
            };
        }
    }

    // ---- Transform tree ----

    #[allow(clippy::too_many_arguments)]
    fn transform_tree(&mut self, b: &mut Bits, sh: &Slice, sps: &Sps, pps: &Pps, cu: &Cu, x0: usize, y0: usize, xb: usize, yb: usize, log2: u32, depth: u32, blk: usize, parent: [bool; 2]) -> Option<()> {
        let split = if log2 <= sps.log2_max_tb && log2 > sps.log2_min_tb && depth < cu.max_trafo_depth && !(cu.intra_split && depth == 0) {
            self.cabac.bin(b, SPLIT_TRANSFORM + 5 - log2 as usize) != 0
        } else {
            let inter_split = sps.max_th_depth_inter == 0 && !cu.intra && cu.part != 0 && depth == 0;
            log2 > sps.log2_max_tb || (cu.intra_split && depth == 0) || inter_split
        };
        let mut cbf = parent;
        if sps.chroma != 0 && log2 > 2 {
            if depth == 0 || parent[0] {
                cbf[0] = self.cabac.bin(b, CBF_CHROMA + depth as usize) != 0;
            } else {
                cbf[0] = false;
            }
            if depth == 0 || parent[1] {
                cbf[1] = self.cabac.bin(b, CBF_CHROMA + depth as usize) != 0;
            } else {
                cbf[1] = false;
            }
        }
        if split {
            let h = 1usize << (log2 - 1);
            for (i, (dx, dy)) in [(0, 0), (h, 0), (0, h), (h, h)].iter().enumerate() {
                self.transform_tree(b, sh, sps, pps, cu, x0 + dx, y0 + dy, x0, y0, log2 - 1, depth + 1, i, cbf)?;
            }
            return Some(());
        }
        let cbf_luma = if cu.intra || depth != 0 || cbf[0] || cbf[1] { self.cabac.bin(b, CBF_LUMA + (depth == 0) as usize) != 0 } else { true };
        self.transform_unit(b, sh, sps, pps, cu, x0, y0, xb, yb, log2, blk, cbf_luma, cbf)?;
        let size = 1usize << log2;
        self.fill(x0, y0, size, size, |c, i| {
            c.recon[i] = true;
            if cbf_luma {
                c.cbf[i] = true;
            }
        });
        self.mark_tu(x0, y0, size, size);
        Some(())
    }

    #[allow(clippy::too_many_arguments)]
    fn transform_unit(&mut self, b: &mut Bits, sh: &Slice, sps: &Sps, pps: &Pps, cu: &Cu, x0: usize, y0: usize, xb: usize, yb: usize, log2: u32, blk: usize, cbf_luma: bool, cbf: [bool; 2]) -> Option<()> {
        let mode = if cu.intra_split { cu.ipm[blk] } else { cu.ipm[0] };
        let mode_c = cu.ipm_c;
        if cu.intra {
            self.intra_pred(sps, pps, x0, y0, log2, 0, mode);
        }
        let any = cbf_luma || cbf[0] || cbf[1];
        if any && pps.cu_qp_delta && !self.is_cu_qp_delta_coded {
            let mut d = self.cabac.cu_qp_delta_abs(b);
            if d != 0 && self.cabac.bypass(b) != 0 {
                d = -d;
            }
            self.cu_qp_delta = d;
            self.is_cu_qp_delta_coded = true;
            self.set_qpy(sps, pps, sh, cu.x, cu.y);
        }
        let (mut scan, mut scan_c) = (0u8, 0u8);
        if cu.intra && log2 < 4 {
            scan = if (6..=14).contains(&mode) { 2 } else if (22..=30).contains(&mode) { 1 } else { 0 };
            scan_c = if (6..=14).contains(&mode_c) { 2 } else if (22..=30).contains(&mode_c) { 1 } else { 0 };
        }
        if cbf_luma {
            self.residual_block(b, sh, sps, pps, cu, x0, y0, log2, scan, 0, mode);
        }
        if sps.chroma == 0 {
            return Some(());
        }
        if log2 > 2 {
            for c in 1..3 {
                if cu.intra {
                    self.intra_pred(sps, pps, x0, y0, log2 - 1, c, mode_c);
                }
                if cbf[c - 1] {
                    self.residual_block(b, sh, sps, pps, cu, x0, y0, log2 - 1, scan_c, c, mode_c);
                }
            }
        } else if blk == 3 {
            for c in 1..3 {
                if cu.intra {
                    self.intra_pred(sps, pps, xb, yb, log2, c, mode_c);
                }
                if cbf[c - 1] {
                    self.residual_block(b, sh, sps, pps, cu, xb, yb, log2, scan_c, c, mode_c);
                }
            }
        }
        Some(())
    }

    /// Intra prediction of a transform block (luma at (x0,y0), chroma at its 4:2:0 position).
    #[allow(clippy::too_many_arguments)]
    fn intra_pred(&mut self, sps: &Sps, pps: &Pps, x0: usize, y0: usize, log2: u32, c: usize, mode: u32) {
        let shift = if c == 0 { 0 } else { 1 };
        let (x, y) = (x0 >> shift, y0 >> shift);
        let (stride, slot) = {
            let cur = self.cur.as_ref().unwrap();
            (if c == 0 { cur.stride } else { cur.cstride }, cur.slot)
        };
        let cip = pps.constrained_intra_pred;
        // Take the plane out while predicting (the availability check reads the rest).
        let mut plane = core::mem::take(match c {
            0 => &mut self.dpb[slot].y,
            1 => &mut self.dpb[slot].u,
            _ => &mut self.dpb[slot].v,
        });
        {
            let cur = self.cur.as_ref().unwrap();
            let avail = |dx: isize, dy: isize| -> bool {
                let (lx, ly) = ((x as isize + dx) << shift, (y as isize + dy) << shift);
                if !self.same_region(sps, lx, ly, x0, y0) {
                    return false;
                }
                let i = (ly as usize >> 2) * cur.bw + (lx as usize >> 2);
                cur.recon[i] && (!cip || self.dpb[slot].mvf[i].pred == PF_INTRA)
            };
            let p = pred::Params { log2, luma: c == 0, mode, bit_depth: if c == 0 { sps.bit_depth } else { sps.bit_depth_c }, strong_smoothing: sps.strong_intra_smoothing };
            pred::intra(&mut plane, stride, x, y, &p, &avail);
        }
        *match c {
            0 => &mut self.dpb[slot].y,
            1 => &mut self.dpb[slot].u,
            _ => &mut self.dpb[slot].v,
        } = plane;
    }

    #[allow(clippy::too_many_arguments)]
    fn residual_block(&mut self, b: &mut Bits, sh: &Slice, sps: &Sps, pps: &Pps, cu: &Cu, x0: usize, y0: usize, log2: u32, scan: u8, c: usize, _mode: u32) {
        let bd = if c == 0 { sps.bit_depth } else { sps.bit_depth_c };
        let qp = if c == 0 {
            self.qp_y + sps.qp_bd_offset
        } else {
            let off = if c == 1 { pps.cb_qp_offset + sh.cb_qp_offset } else { pps.cr_qp_offset + sh.cr_qp_offset };
            let qpi = (self.qp_y + off).clamp(-sps.qp_bd_offset, 57);
            filter::chroma_qp(qpi) + sps.qp_bd_offset
        };
        let matrix = sps.scaling.as_ref().map(|s| pps.scaling.as_ref().unwrap_or(s)).map(|sl| {
            let id = 3 * (!cu.intra) as usize + c;
            let m = sl.sl[log2 as usize - 2][id];
            let dc = if log2 >= 4 { sl.dc[log2 as usize - 4][id] } else { 16 };
            (m, dc)
        });
        let r = ResidualCtx {
            log2,
            c_idx: c,
            scan,
            sign_hiding: pps.sign_hiding,
            bypass: cu.bypass,
            transform_skip_allowed: pps.transform_skip && log2 <= 2 && !cu.bypass,
            intra: cu.intra,
            scale: LEVEL_SCALE[qp.rem_euclid(6) as usize] << (qp / 6),
            shift: bd + log2 - 5,
            matrix,
        };
        let mut coeffs = core::mem::take(&mut self.coeffs);
        let (ts, _max_xy) = residual(&mut self.cabac, b, &r, &mut coeffs);
        if !cu.bypass {
            if ts {
                xform::skip(&mut coeffs, log2, bd);
            } else {
                xform::inverse(&mut coeffs, log2, cu.intra && c == 0 && log2 == 2, bd);
            }
        }
        let cur = self.cur.as_ref().unwrap();
        let (slot, stride, cs) = (cur.slot, cur.stride, cur.cstride);
        let p = &mut self.dpb[slot];
        match c {
            0 => xform::add(&mut p.y, y0 * stride + x0, stride, &coeffs, log2, bd),
            1 => xform::add(&mut p.u, (y0 / 2) * cs + x0 / 2, cs, &coeffs, log2, bd),
            _ => xform::add(&mut p.v, (y0 / 2) * cs + x0 / 2, cs, &coeffs, log2, bd),
        }
        self.coeffs = coeffs;
    }

    // ---- Prediction units ----

    #[allow(clippy::too_many_arguments)]
    fn prediction_unit(&mut self, b: &mut Bits, sh: &Slice, sps: &Sps, pps: &Pps, cu: &Cu, x0: usize, y0: usize, w: usize, h: usize, part: usize) -> Option<()> {
        let mut mv = Mvf { pred: 0, mv: [[0; 2]; 2], refi: [-1, -1] };
        if cu.merge {
            let idx = if sh.max_merge > 1 {
                let mut i = self.cabac.bin(b, MERGE_IDX) as usize;
                if i != 0 {
                    while i < sh.max_merge - 1 && self.cabac.bypass(b) != 0 {
                        i += 1;
                    }
                }
                i
            } else {
                0
            };
            mv = self.merge(sh, sps, pps, cu, x0, y0, w, h, part, idx);
        } else {
            let mut idc = 0; // L0
            if sh.kind == SLICE_B {
                idc = if w + h == 12 {
                    self.cabac.bin(b, INTER_PRED_IDC + 4)
                } else if self.cabac.bin(b, INTER_PRED_IDC + self.ct_depth as usize) != 0 {
                    2
                } else {
                    self.cabac.bin(b, INTER_PRED_IDC + 4)
                };
            }
            for l in 0..2 {
                if (l == 0 && idc == 1) || (l == 1 && idc == 0) {
                    continue;
                }
                let ri = if sh.num_ref[l] > 1 { self.cabac.ref_idx(b, sh.num_ref[l]) } else { 0 };
                mv.refi[l] = ri as i8;
                let mvd = if l == 1 && sh.mvd_l1_zero && idc == 2 { [0, 0] } else { self.cabac.mvd(b) };
                let flag = self.cabac.bin(b, MVP_FLAG) as usize;
                mv.pred |= 1 << l;
                let p = self.amvp(sh, sps, x0, y0, w, h, l, ri, flag);
                mv.mv[l] = [wrap16(p[0] + mvd[0]), wrap16(p[1] + mvd[1])];
            }
        }
        // Store the motion.
        self.set_mvf(x0, y0, w, h, mv);
        self.mark_pu(x0, y0, w, h);
        self.motion_comp(sh, sps, x0, y0, w, h, &mv);
        Some(())
    }

    fn avail_pu(&self, sps: &Sps, x: isize, y: isize, x0: usize, y0: usize) -> bool {
        self.same_region(sps, x, y, x0, y0) && self.mvf_at(x as usize, y as usize).pred != PF_INTRA
    }

    #[allow(clippy::too_many_arguments)]
    fn merge(&self, sh: &Slice, sps: &Sps, pps: &Pps, cu: &Cu, mut x0: usize, mut y0: usize, mut w: usize, mut h: usize, mut part: usize, idx: usize) -> Mvf {
        let (w2, h2) = (w, h);
        let single = pps.log2_parallel_merge > 2 && cu.log2 == 3;
        if single {
            x0 = cu.x;
            y0 = cu.y;
            w = 8;
            h = 8;
            part = 0;
        }
        let pl = pps.log2_parallel_merge;
        let diff_mer = |xn: isize, yn: isize| (xn >> pl) == (x0 as isize >> pl) && (yn >> pl) == (y0 as isize >> pl);
        let (x0i, y0i, wi, hi) = (x0 as isize, y0 as isize, w as isize, h as isize);
        let a1 = (x0i - 1, y0i + hi - 1);
        let b1 = (x0i + wi - 1, y0i - 1);
        let b0 = (x0i + wi, y0i - 1);
        let a0 = (x0i - 1, y0i + hi);
        let b2 = (x0i - 1, y0i - 1);
        let get = |p: (isize, isize)| self.mvf_at(p.0 as usize, p.1 as usize);
        let av = |p: (isize, isize)| self.avail_pu(sps, p.0, p.1, x0, y0);
        let mut list: Vec<Mvf> = Vec::with_capacity(5);
        let done = |list: &Vec<Mvf>| list.len() > idx;
        let avail_a1 = !(!single && part == 1 && matches!(cu.part, 2 | 6 | 7)) && !diff_mer(a1.0, a1.1) && av(a1);
        if avail_a1 {
            list.push(get(a1));
        }
        let result = |list: &Vec<Mvf>| -> Option<Mvf> {
            if done(list) {
                let mut m = list[idx];
                if m.pred == PF_BI && w2 + h2 == 12 {
                    m.pred = PF_L0;
                    m.refi[1] = -1;
                }
                Some(m)
            } else {
                None
            }
        };
        if let Some(m) = result(&list) {
            return m;
        }
        let avail_b1 = !(!single && part == 1 && matches!(cu.part, 1 | 4 | 5)) && !diff_mer(b1.0, b1.1) && av(b1);
        if avail_b1 && !(avail_a1 && get(b1) == get(a1)) {
            list.push(get(b1));
        }
        if let Some(m) = result(&list) {
            return m;
        }
        let avail_b0 = (b0.0 as usize) < sps.width && av(b0) && !diff_mer(b0.0, b0.1);
        if avail_b0 && !(avail_b1 && get(b0) == get(b1)) {
            list.push(get(b0));
        }
        if let Some(m) = result(&list) {
            return m;
        }
        let avail_a0 = (a0.1 as usize) < sps.height && av(a0) && !diff_mer(a0.0, a0.1);
        if avail_a0 && !(avail_a1 && get(a0) == get(a1)) {
            list.push(get(a0));
        }
        if let Some(m) = result(&list) {
            return m;
        }
        let avail_b2 = av(b2) && !diff_mer(b2.0, b2.1);
        if avail_b2 && !(avail_a1 && get(b2) == get(a1)) && !(avail_b1 && get(b2) == get(b1)) && list.len() != 4 {
            list.push(get(b2));
        }
        if let Some(m) = result(&list) {
            return m;
        }
        if sh.temporal_mvp && list.len() < sh.max_merge {
            let c0 = self.temporal(sh, sps, x0, y0, w, h, 0, 0);
            let c1 = if sh.kind == SLICE_B { self.temporal(sh, sps, x0, y0, w, h, 0, 1) } else { None };
            if c0.is_some() || c1.is_some() {
                let mut m = Mvf { pred: 0, mv: [[0; 2]; 2], refi: [-1, -1] };
                if let Some(v) = c0 {
                    m.pred |= PF_L0;
                    m.mv[0] = v;
                    m.refi[0] = 0;
                }
                if let Some(v) = c1 {
                    m.pred |= PF_L1;
                    m.mv[1] = v;
                    m.refi[1] = 0;
                }
                list.push(m);
            }
        }
        if let Some(m) = result(&list) {
            return m;
        }
        let orig = list.len();
        if sh.kind == SLICE_B && orig > 1 && orig < sh.max_merge {
            let mut comb = 0;
            while list.len() < sh.max_merge && comb < orig * (orig - 1) {
                let l0 = list[tab::L0_L1_CAND_IDX[comb * 2] as usize];
                let l1 = list[tab::L0_L1_CAND_IDX[comb * 2 + 1] as usize];
                if l0.pred & PF_L0 != 0 && l1.pred & PF_L1 != 0 {
                    let p0 = self.lists.l[0][l0.refi[0] as usize].poc;
                    let p1 = self.lists.l[1][l1.refi[1] as usize].poc;
                    if p0 != p1 || l0.mv[0] != l1.mv[1] {
                        list.push(Mvf { pred: PF_BI, mv: [l0.mv[0], l1.mv[1]], refi: [l0.refi[0], l1.refi[1]] });
                    }
                }
                comb += 1;
            }
        }
        if let Some(m) = result(&list) {
            return m;
        }
        let nb_refs = if sh.kind == SLICE_P { sh.num_ref[0] } else { sh.num_ref[0].min(sh.num_ref[1]) };
        let mut zero = 0;
        while list.len() < sh.max_merge.max(idx + 1) {
            let r = if zero < nb_refs { zero as i8 } else { 0 };
            list.push(Mvf { pred: if sh.kind == SLICE_B { PF_BI } else { PF_L0 }, mv: [[0; 2]; 2], refi: [r, if sh.kind == SLICE_B { r } else { -1 }] });
            zero += 1;
        }
        result(&list).unwrap_or_default()
    }

    /// Temporal motion vector candidate for list `x` and reference `ref_idx`.
    #[allow(clippy::too_many_arguments)]
    fn temporal(&self, sh: &Slice, sps: &Sps, x0: usize, y0: usize, w: usize, h: usize, ref_idx: usize, x: usize) -> Option<[i32; 2]> {
        let cl = if sh.col_from_l0 { 0 } else { 1 };
        let col = *self.lists.l[cl].get(sh.col_ref_idx)?;
        let colp = &self.dpb[col.slot];
        let try_at = |xx: usize, yy: usize| -> Option<[i32; 2]> {
            let (xx, yy) = (xx & !15, yy & !15);
            let cur = self.cur.as_ref()?;
            let m = *colp.mvf.get((yy >> 2) * cur.bw + (xx >> 2))?;
            if m.pred == PF_INTRA {
                return None;
            }
            let ctb = (yy >> sps.log2_ctb) * sps.ctb_w + (xx >> sps.log2_ctb);
            let cl_lists = colp.lists.get(*colp.ctb_slice.get(ctb)? as usize)?;
            let list_col = if m.pred & PF_L0 == 0 {
                1
            } else if m.pred == PF_L0 {
                0
            } else {
                let no_backward = self.lists.l.iter().all(|l| l.iter().all(|e| e.poc <= self.poc));
                if no_backward {
                    x
                } else if sh.col_from_l0 {
                    1
                } else {
                    0
                }
            };
            let cref = *cl_lists.l[list_col].get(m.refi[list_col].max(0) as usize)?;
            let cur_ref = *self.lists.l[x].get(ref_idx)?;
            if cur_ref.lt != cref.lt {
                return None;
            }
            let col_diff = colp.poc - cref.poc;
            let cur_diff = self.poc - cur_ref.poc;
            let mv = m.mv[list_col];
            Some(if cur_ref.lt || col_diff == cur_diff || col_diff == 0 { mv } else { mv_scale(mv, col_diff, cur_diff) })
        };
        let (xb, yb) = (x0 + w, y0 + h);
        if (y0 >> sps.log2_ctb) == (yb >> sps.log2_ctb) && yb < sps.height && xb < sps.width {
            if let Some(v) = try_at(xb, yb) {
                return Some(v);
            }
        }
        try_at(x0 + w / 2, y0 + h / 2)
    }

    /// Advanced motion vector prediction: the predictor for list `lx`.
    #[allow(clippy::too_many_arguments)]
    fn amvp(&self, sh: &Slice, sps: &Sps, x0: usize, y0: usize, w: usize, h: usize, lx: usize, ref_idx: usize, flag: usize) -> [i32; 2] {
        let (x0i, y0i, wi, hi) = (x0 as isize, y0 as isize, w as isize, h as isize);
        let cur_ref = self.lists.l[lx].get(ref_idx).copied().unwrap_or_default();
        let av = |p: (isize, isize)| self.avail_pu(sps, p.0, p.1, x0, y0);
        let get = |p: (isize, isize)| self.mvf_at(p.0 as usize, p.1 as usize);
        let refpoc = |m: &Mvf, l: usize| self.lists.l[l].get(m.refi[l].max(0) as usize).copied().unwrap_or_default();
        // Same reference picture, no scaling.
        let mx = |p: (isize, isize), l: usize| -> Option<[i32; 2]> {
            let m = get(p);
            if m.pred & (1 << l) != 0 && refpoc(&m, l).poc == cur_ref.poc {
                Some(m.mv[l])
            } else {
                None
            }
        };
        // Any reference of the same kind (long-term or not), scaled by distance.
        let mx_lt = |p: (isize, isize), l: usize| -> Option<[i32; 2]> {
            let m = get(p);
            if m.pred & (1 << l) == 0 {
                return None;
            }
            let r = refpoc(&m, l);
            if r.lt != cur_ref.lt {
                return None;
            }
            let mut v = m.mv[l];
            if !cur_ref.lt && r.poc != cur_ref.poc {
                let mut d = self.poc - r.poc;
                if d == 0 {
                    d = 1;
                }
                v = mv_scale(v, d, self.poc - cur_ref.poc);
            }
            Some(v)
        };
        let (l0, l1) = (lx, 1 - lx);
        let a0 = (x0i - 1, y0i + hi);
        let a1 = (x0i - 1, y0i + hi - 1);
        let avail_a0 = av(a0) && (a0.1 as usize) < sps.height;
        let avail_a1 = av(a1);
        let scaled = avail_a0 || avail_a1;
        let mut ma: Option<[i32; 2]> = None;
        for (ok, p) in [(avail_a0, a0), (avail_a1, a1)] {
            if ma.is_none() && ok {
                ma = mx(p, l0).or_else(|| mx(p, l1));
            }
        }
        for (ok, p) in [(avail_a0, a0), (avail_a1, a1)] {
            if ma.is_none() && ok {
                ma = mx_lt(p, l0).or_else(|| mx_lt(p, l1));
            }
        }
        let b0 = (x0i + wi, y0i - 1);
        let b1 = (x0i + wi - 1, y0i - 1);
        let b2 = (x0i - 1, y0i - 1);
        let avail_b0 = (b0.0 as usize) < sps.width && av(b0);
        let avail_b1 = av(b1);
        let avail_b2 = av(b2);
        let mut mb: Option<[i32; 2]> = None;
        for (ok, p) in [(avail_b0, b0), (avail_b1, b1), (avail_b2, b2)] {
            if mb.is_none() && ok {
                mb = mx(p, l0).or_else(|| mx(p, l1));
            }
        }
        if !scaled {
            if mb.is_some() {
                ma = mb;
            }
            mb = None;
            for (ok, p) in [(avail_b0, b0), (avail_b1, b1), (avail_b2, b2)] {
                if mb.is_none() && ok {
                    mb = mx_lt(p, l0).or_else(|| mx_lt(p, l1));
                }
            }
        }
        let mut cands: Vec<[i32; 2]> = Vec::with_capacity(3);
        if let Some(a) = ma {
            cands.push(a);
        }
        if let Some(bb) = mb {
            if ma != Some(bb) {
                cands.push(bb);
            }
        }
        if cands.len() < 2 && sh.temporal_mvp && flag == cands.len() {
            if let Some(t) = self.temporal(sh, sps, x0, y0, w, h, ref_idx, lx) {
                cands.push(t);
            }
        }
        cands.get(flag).copied().unwrap_or([0, 0])
    }

    fn motion_comp(&mut self, sh: &Slice, sps: &Sps, x0: usize, y0: usize, w: usize, h: usize, mv: &Mvf) {
        let cur = self.cur.as_ref().unwrap();
        let (slot, stride, cs) = (cur.slot, cur.stride, cur.cstride);
        let mut sc = core::mem::replace(&mut self.scratch, Scratch::empty());
        let Scratch { pl, pc, src, tmp } = &mut sc;
        for l in 0..2 {
            if mv.pred & (1 << l) == 0 {
                continue;
            }
            let Some(e) = self.lists.l[l].get(mv.refi[l].max(0) as usize) else { continue };
            let r = &self.dpb[e.slot];
            let m = mv.mv[l];
            let rp = inter::RefPlane { d: &r.y, stride, w: sps.width, h: sps.height };
            inter::predict(&rp, x0 as isize + (m[0] >> 2) as isize, y0 as isize + (m[1] >> 2) as isize, (m[0] & 3) as usize, (m[1] & 3) as usize, w, h, true, sps.bit_depth, &mut pl[l], src, tmp);
            if sps.chroma != 0 {
                for (k, cpl) in [&r.u, &r.v].iter().enumerate() {
                    let rp = inter::RefPlane { d: cpl, stride: cs, w: sps.width / 2, h: sps.height / 2 };
                    inter::predict(&rp, (x0 / 2) as isize + (m[0] >> 3) as isize, (y0 / 2) as isize + (m[1] >> 3) as isize, (m[0] & 7) as usize, (m[1] & 7) as usize, w / 2, h / 2, false, sps.bit_depth_c, &mut pc[l][k], src, tmp);
                }
            }
        }
        let p = &mut self.dpb[slot];
        let bd = sps.bit_depth;
        let scale = 1 << (bd - 8);
        if mv.pred == PF_BI {
            let r0 = mv.refi[0].max(0) as usize;
            let r1 = mv.refi[1].max(0) as usize;
            let wt = if sh.weighted { Some((sh.luma_w[0][r0], sh.luma_o[0][r0] * scale, sh.luma_w[1][r1], sh.luma_o[1][r1] * scale, sh.luma_log2_wd)) } else { None };
            inter::put_bi(&mut p.y, y0 * stride + x0, stride, &pl[0], &pl[1], w, h, bd, wt);
            if sps.chroma != 0 {
                for k in 0..2 {
                    let wt = if sh.weighted { Some((sh.chroma_w[0][r0][k], sh.chroma_o[0][r0][k] * scale, sh.chroma_w[1][r1][k], sh.chroma_o[1][r1][k] * scale, sh.chroma_log2_wd)) } else { None };
                    let dst = if k == 0 { &mut p.u } else { &mut p.v };
                    inter::put_bi(dst, (y0 / 2) * cs + x0 / 2, cs, &pc[0][k], &pc[1][k], w / 2, h / 2, sps.bit_depth_c, wt);
                }
            }
        } else {
            let l = if mv.pred == PF_L0 { 0 } else { 1 };
            let r = mv.refi[l].max(0) as usize;
            let wt = if sh.weighted { Some((sh.luma_w[l][r], sh.luma_o[l][r] * scale, sh.luma_log2_wd)) } else { None };
            inter::put_uni(&mut p.y, y0 * stride + x0, stride, &pl[l], w, h, bd, wt);
            if sps.chroma != 0 {
                for k in 0..2 {
                    let wt = if sh.weighted { Some((sh.chroma_w[l][r][k], sh.chroma_o[l][r][k] * scale, sh.chroma_log2_wd)) } else { None };
                    let dst = if k == 0 { &mut p.u } else { &mut p.v };
                    inter::put_uni(dst, (y0 / 2) * cs + x0 / 2, cs, &pc[l][k], w / 2, h / 2, sps.bit_depth_c, wt);
                }
            }
        }
        self.scratch = sc;
    }

    // ---- Loop filters ----

    fn filters(&mut self, cur: &Cur, sps: &Sps, pps: &Pps) {
        let (bw, bh) = (cur.bw, cur.bh);
        let mvf = core::mem::take(&mut self.dpb[cur.slot].mvf);
        let lists = core::mem::take(&mut self.dpb[cur.slot].lists);
        let ctb_of = |bx: usize, by: usize| ((by * 4) >> sps.log2_ctb) * sps.ctb_w + ((bx * 4) >> sps.log2_ctb);
        let slice_of = |bx: usize, by: usize| cur.slices.get(cur.ctb_info[ctb_of(bx, by)] as usize).copied();
        let mut bs_v = vec![0u8; bw * bh];
        let mut bs_h = vec![0u8; bw * bh];
        let mut offsets = vec![(0i8, 0i8); bw * bh];
        let wb = sps.width.div_ceil(4);
        let hb = sps.height.div_ceil(4);
        for by in 0..hb {
            for bx in 0..wb {
                let i = by * bw + bx;
                let Some(sq) = slice_of(bx, by) else { continue };
                offsets[i] = (sq.beta, sq.tc);
                if sq.disable_dbf {
                    continue;
                }
                for dir in 0..2 {
                    let (tu, pu) = if dir == 0 { (1, 2) } else { (4, 8) };
                    let e = cur.edge[i];
                    if e & (tu | pu) == 0 {
                        continue;
                    }
                    let (nx, ny) = if dir == 0 {
                        if bx == 0 {
                            continue;
                        }
                        (bx - 1, by)
                    } else {
                        if by == 0 {
                            continue;
                        }
                        (bx, by - 1)
                    };
                    let (cq, cp) = (ctb_of(bx, by), ctb_of(nx, ny));
                    if cq != cp {
                        if cur.ctb_slice_addr[cq] != cur.ctb_slice_addr[cp] && !sq.lf_across {
                            continue;
                        }
                        if cur.tile_id[cq] != cur.tile_id[cp] && !pps.lf_across_tiles {
                            continue;
                        }
                    }
                    let j = ny * bw + nx;
                    let (q, p) = (mvf[i], mvf[j]);
                    let bs = if q.pred == PF_INTRA || p.pred == PF_INTRA {
                        2
                    } else if e & tu != 0 && (cur.cbf[i] || cur.cbf[j]) {
                        1
                    } else {
                        let lq = &lists[cur.ctb_info[cq] as usize];
                        let lp = &lists[cur.ctb_info[cp] as usize];
                        motion_bs(&q, &p, lq, lp)
                    };
                    if dir == 0 {
                        bs_v[i] = bs;
                    } else {
                        bs_h[i] = bs;
                    }
                }
            }
        }
        let slot = cur.slot;
        let (ys, cs) = (cur.stride, cur.cstride);
        {
            let p = &mut self.dpb[slot];
            let (mut y, mut u, mut v) = (core::mem::take(&mut p.y), core::mem::take(&mut p.u), core::mem::take(&mut p.v));
            let info = filter::Info { w: sps.width, h: sps.height, bw, bs_v: &bs_v, bs_h: &bs_h, qp: &cur.qp, keep: &cur.keep, offsets: &offsets, cb_qp_offset: pps.cb_qp_offset, cr_qp_offset: pps.cr_qp_offset, bit_depth: sps.bit_depth, chroma: sps.chroma != 0 };
            filter::deblock(&mut y, &mut u, &mut v, ys, cs, &info);
            if sps.sao {
                for c in 0..if sps.chroma != 0 { 3 } else { 1 } {
                    let sh = if c == 0 { 0 } else { 1 };
                    let ctb = sps.ctb >> sh;
                    let (plane, stride) = match c {
                        0 => (&mut y, ys),
                        1 => (&mut u, cs),
                        _ => (&mut v, cs),
                    };
                    let params = |x: usize, yy: usize| cur.sao[yy * sps.ctb_w + x][c];
                    let ctb_at = |x: usize, yy: usize| ((yy << sh) >> sps.log2_ctb) * sps.ctb_w + ((x << sh) >> sps.log2_ctb);
                    let same = |x: usize, yy: usize, xn: isize, yn: isize| -> bool {
                        let (a, bb) = (ctb_at(x, yy), ctb_at(xn as usize, yn as usize));
                        if a == bb {
                            return true;
                        }
                        if cur.tile_id[a] != cur.tile_id[bb] && !pps.lf_across_tiles {
                            return false;
                        }
                        let (sa, sb) = (cur.ctb_slice_addr[a], cur.ctb_slice_addr[bb]);
                        if sa != sb {
                            let flag = if sb < sa { cur.slices.get(cur.ctb_info[a] as usize) } else { cur.slices.get(cur.ctb_info[bb] as usize) };
                            if !flag.is_some_and(|s| s.lf_across) {
                                return false;
                            }
                        }
                        true
                    };
                    let keep = |x: usize, yy: usize| cur.keep[((yy << sh) >> 2) * bw + ((x << sh) >> 2)];
                    filter::sao(plane, stride, sps.width >> sh, sps.height >> sh, ctb, if c == 0 { sps.bit_depth } else { sps.bit_depth_c }, &params, &same, &keep);
                }
            }
            p.y = y;
            p.u = u;
            p.v = v;
        }
        self.dpb[slot].mvf = mvf;
        self.dpb[slot].lists = lists;
    }
}

/// Boundary strength from motion (both sides inter).
fn motion_bs(q: &Mvf, p: &Mvf, lq: &RefLists, lp: &RefLists) -> u8 {
    let r = |m: &Mvf, l: &RefLists, k: usize| l.l[k].get(m.refi[k].max(0) as usize).map(|e| e.slot).unwrap_or(usize::MAX);
    let far = |a: [i32; 2], b: [i32; 2]| (a[0] - b[0]).abs() >= 4 || (a[1] - b[1]).abs() >= 4;
    if q.pred == PF_BI && p.pred == PF_BI {
        let (q0, q1, p0, p1) = (r(q, lq, 0), r(q, lq, 1), r(p, lp, 0), r(p, lp, 1));
        if q0 == p0 && q0 == q1 && p0 == p1 {
            ((far(p.mv[0], q.mv[0]) || far(p.mv[1], q.mv[1])) && (far(p.mv[1], q.mv[0]) || far(p.mv[0], q.mv[1]))) as u8
        } else if p0 == q0 && p1 == q1 {
            (far(p.mv[0], q.mv[0]) || far(p.mv[1], q.mv[1])) as u8
        } else if p1 == q0 && p0 == q1 {
            (far(p.mv[1], q.mv[0]) || far(p.mv[0], q.mv[1])) as u8
        } else {
            1
        }
    } else if q.pred != PF_BI && p.pred != PF_BI {
        let (a, ra) = if q.pred & 1 != 0 { (q.mv[0], r(q, lq, 0)) } else { (q.mv[1], r(q, lq, 1)) };
        let (b, rb) = if p.pred & 1 != 0 { (p.mv[0], r(p, lp, 0)) } else { (p.mv[1], r(p, lp, 1)) };
        if ra == rb {
            far(a, b) as u8
        } else {
            1
        }
    } else {
        1
    }
}

impl VideoDec for HevcDec {
    fn decode(&mut self, data: &[u8]) {
        self.packet(data);
    }
    fn pop(&mut self) -> Option<OutPic> {
        self.out.pop_front().map(OutPic::Yuv)
    }
    fn flush(&mut self) {
        self.finish_picture();
        self.bump_all(true);
    }
    fn reset(&mut self) {
        self.cur = None;
        self.sh = None;
        self.dpb.clear();
        self.out.clear();
        self.no_rasl_output = true;
        self.first_pic = true;
    }
}

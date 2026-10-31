use super::{
    AC_QLOOKUP, ADJUSTED_TX_SIZE, ADST_FLIPADST, BR_CDF_SIZE, COEFF_BASE_CTX_OFFSET, COEFF_BASE_POS_CTX_OFFSET, COEFF_BASE_RANGE, DC_QLOOKUP, DCT_DCT,
    DCT_FLIPADST, DEFAULT_SCAN_4X4, DEFAULT_SCAN_4X8, DEFAULT_SCAN_4X16, DEFAULT_SCAN_8X4, DEFAULT_SCAN_8X8, DEFAULT_SCAN_8X16, DEFAULT_SCAN_8X32,
    DEFAULT_SCAN_16X4, DEFAULT_SCAN_16X8, DEFAULT_SCAN_16X16, DEFAULT_SCAN_16X32, DEFAULT_SCAN_32X8, DEFAULT_SCAN_32X16, DEFAULT_SCAN_32X32,
    FILTER_INTRA_MODE_TO_INTRA_DIR, FLIPADST_ADST, FLIPADST_DCT, FLIPADST_FLIPADST, H_ADST, H_DCT, H_FLIPADST, IDTX, MAG_REF_OFFSET_WITH_TX_CLASS,
    MCOL_SCAN_4X4, MCOL_SCAN_4X8, MCOL_SCAN_4X16, MCOL_SCAN_8X4, MCOL_SCAN_8X8, MCOL_SCAN_8X16, MCOL_SCAN_16X4, MCOL_SCAN_16X8, MCOL_SCAN_16X16, MODE_TO_TXFM,
    MROW_SCAN_4X4, MROW_SCAN_4X8, MROW_SCAN_4X16, MROW_SCAN_8X4, MROW_SCAN_8X8, MROW_SCAN_8X16, MROW_SCAN_16X4, MROW_SCAN_16X8, MROW_SCAN_16X16,
    NUM_BASE_LEVELS, QM_OFFSET, QUANTIZER_MATRIX, SIG_REF_DIFF_OFFSET, SIG_REF_DIFF_OFFSET_NUM, TX_4X4, TX_4X8, TX_4X16, TX_8X4, TX_8X8, TX_8X16, TX_8X32,
    TX_16X4, TX_16X8, TX_16X16, TX_16X32, TX_16X64, TX_32X8, TX_32X16, TX_32X32, TX_32X64, TX_64X16, TX_64X32, TX_64X64, TX_CLASS_2D, TX_CLASS_HORIZ,
    TX_CLASS_VERT, TX_HEIGHT, TX_HEIGHT_LOG2, TX_SET_DCTONLY, TX_SET_INTER_1, TX_SET_INTER_2, TX_SET_INTER_3, TX_SET_INTRA_1, TX_SET_INTRA_2, TX_SIZE_SQR,
    TX_SIZE_SQR_UP, TX_TYPE_IN_SET_INTER, TX_TYPE_IN_SET_INTRA, TX_TYPE_INTER_INV_SET1, TX_TYPE_INTER_INV_SET2, TX_TYPE_INTER_INV_SET3, TX_TYPE_INTRA_INV_SET1,
    TX_TYPE_INTRA_INV_SET2, TX_WIDTH, TX_WIDTH_LOG2, TileDecoder, V_ADST, V_DCT, V_FLIPADST, block_height, block_width, inverse_transform_2d,
};

/// Position and size (in 4x4 units) of the transform block being coded.
#[derive(Clone, Copy)]
struct TxbPos {
    x4: usize,
    y4: usize,
    w4: usize,
    h4: usize,
}

/// Transform size, class and CDF selectors shared by the coefficient reading steps.
#[derive(Clone, Copy)]
struct TxbClass {
    tx_sz: usize,
    tx_class: usize,
    tx_sz_ctx: usize,
    ptype: usize,
}

impl<'a, 'f> TileDecoder<'a, 'f> {
    // ---- coefficients -----------------------------------------------------------------------

    fn get_tx_set(&self, tx_sz: usize) -> usize {
        let sqr = TX_SIZE_SQR[tx_sz] as usize;
        let sqr_up = TX_SIZE_SQR_UP[tx_sz] as usize;
        if sqr_up > TX_32X32 {
            return TX_SET_DCTONLY;
        }
        let reduced = self.fs.fh.reduced_tx_set;
        if self.b.is_inter {
            if reduced || sqr_up == TX_32X32 {
                TX_SET_INTER_3
            } else if sqr == TX_16X16 {
                TX_SET_INTER_2
            } else {
                TX_SET_INTER_1
            }
        } else if sqr_up == TX_32X32 {
            TX_SET_DCTONLY
        } else if reduced || sqr == TX_16X16 {
            TX_SET_INTRA_2
        } else {
            TX_SET_INTRA_1
        }
    }

    fn is_tx_type_in_set(&self, set: usize, tx_type: usize) -> bool {
        if self.b.is_inter { TX_TYPE_IN_SET_INTER[set][tx_type] == 1 } else { TX_TYPE_IN_SET_INTRA[set][tx_type] == 1 }
    }

    fn compute_tx_type(&self, plane: usize, tx_sz: usize, block_x: usize, block_y: usize) -> usize {
        let sqr_up = TX_SIZE_SQR_UP[tx_sz] as usize;
        if self.b.lossless || sqr_up > TX_32X32 {
            return DCT_DCT;
        }
        let set = self.get_tx_set(tx_sz);
        let mi = &self.t.mi;
        if plane == 0 {
            return mi.tx_type[mi.idx(block_y, block_x)] as usize;
        }
        if self.b.is_inter {
            let x4 = self.b.mi_col.max(block_x << self.fs.ssx);
            let y4 = self.b.mi_row.max(block_y << self.fs.ssy);
            let t = mi.tx_type[mi.idx(y4, x4)] as usize;
            if !self.is_tx_type_in_set(set, t) {
                return DCT_DCT;
            }
            return t;
        }
        let t = MODE_TO_TXFM[self.b.uv_mode] as usize;
        if !self.is_tx_type_in_set(set, t) {
            return DCT_DCT;
        }
        t
    }

    fn transform_type(&mut self, x4: usize, y4: usize, tx_sz: usize) {
        let set = self.get_tx_set(tx_sz);
        let qidx =
            if self.fs.fh.seg.enabled { self.fs.fh.get_qindex(true, self.b.segment_id, self.current_q_index) } else { self.fs.fh.quant.base_q_idx as i32 };
        let tx_type = if set > 0 && qidx > 0 {
            let sqr = TX_SIZE_SQR[tx_sz] as usize;
            if self.b.is_inter {
                match set {
                    TX_SET_INTER_1 => TX_TYPE_INTER_INV_SET1[self.sd.read_symbol(&mut self.cdf.inter_tx_type_set1[sqr])] as usize,
                    TX_SET_INTER_2 => TX_TYPE_INTER_INV_SET2[self.sd.read_symbol(&mut self.cdf.inter_tx_type_set2)] as usize,
                    _ => TX_TYPE_INTER_INV_SET3[self.sd.read_symbol(&mut self.cdf.inter_tx_type_set3[sqr])] as usize,
                }
            } else {
                let intra_dir = if self.b.use_filter_intra { FILTER_INTRA_MODE_TO_INTRA_DIR[self.b.filter_intra_mode] as usize } else { self.b.y_mode };
                if set == TX_SET_INTRA_1 {
                    TX_TYPE_INTRA_INV_SET1[self.sd.read_symbol(&mut self.cdf.intra_tx_type_set1[sqr][intra_dir])] as usize
                } else {
                    TX_TYPE_INTRA_INV_SET2[self.sd.read_symbol(&mut self.cdf.intra_tx_type_set2[sqr][intra_dir])] as usize
                }
            }
        } else {
            DCT_DCT
        };
        self.set_tx_types(x4, y4, tx_sz, tx_type);
    }

    fn set_tx_types(&mut self, x4: usize, y4: usize, tx_sz: usize, tx_type: usize) {
        let mi = &mut self.t.mi;
        for i in 0..(TX_WIDTH[tx_sz] as usize >> 2) {
            for j in 0..(TX_HEIGHT[tx_sz] as usize >> 2) {
                if y4 + j < mi.rows && x4 + i < mi.cols {
                    let k = mi.idx(y4 + j, x4 + i);
                    mi.tx_type[k] = tx_type as u8;
                }
            }
        }
    }

    fn get_scan(&self, tx_sz: usize) -> &'static [u16] {
        if tx_sz == TX_16X64 {
            return &DEFAULT_SCAN_16X32;
        }
        if tx_sz == TX_64X16 {
            return &DEFAULT_SCAN_32X16;
        }
        if TX_SIZE_SQR_UP[tx_sz] as usize == TX_64X64 {
            return &DEFAULT_SCAN_32X32;
        }
        let t = self.plane_tx_type;
        if t == IDTX {
            return default_scan(tx_sz);
        }
        let prefer_row = t == V_DCT || t == V_ADST || t == V_FLIPADST;
        let prefer_col = t == H_DCT || t == H_ADST || t == H_FLIPADST;
        if prefer_row {
            match tx_sz {
                TX_4X4 => &MROW_SCAN_4X4,
                TX_4X8 => &MROW_SCAN_4X8,
                TX_8X4 => &MROW_SCAN_8X4,
                TX_8X8 => &MROW_SCAN_8X8,
                TX_8X16 => &MROW_SCAN_8X16,
                TX_16X8 => &MROW_SCAN_16X8,
                TX_16X16 => &MROW_SCAN_16X16,
                TX_4X16 => &MROW_SCAN_4X16,
                _ => &MROW_SCAN_16X4,
            }
        } else if prefer_col {
            match tx_sz {
                TX_4X4 => &MCOL_SCAN_4X4,
                TX_4X8 => &MCOL_SCAN_4X8,
                TX_8X4 => &MCOL_SCAN_8X4,
                TX_8X8 => &MCOL_SCAN_8X8,
                TX_8X16 => &MCOL_SCAN_8X16,
                TX_16X8 => &MCOL_SCAN_16X8,
                TX_16X16 => &MCOL_SCAN_16X16,
                TX_4X16 => &MCOL_SCAN_4X16,
                _ => &MCOL_SCAN_16X4,
            }
        } else {
            default_scan(tx_sz)
        }
    }

    /// coeffs( ): returns eob.
    pub(super) fn coeffs(&mut self, plane: usize, start_x: usize, start_y: usize, tx_sz: usize) -> usize {
        let pos = TxbPos { x4: start_x >> 2, y4: start_y >> 2, w4: TX_WIDTH[tx_sz] as usize >> 2, h4: TX_HEIGHT[tx_sz] as usize >> 2 };
        let tx_sz_ctx = (TX_SIZE_SQR[tx_sz] as usize + TX_SIZE_SQR_UP[tx_sz] as usize + 1) >> 1;
        let mut eob = 0usize;
        let mut cul_level = 0u32;
        let mut dc_category = 0u8;
        let ctx = self.all_zero_ctx(plane, tx_sz, pos.x4, pos.y4, pos.w4, pos.h4);
        let all_zero = self.sd.read_symbol(&mut self.cdf.txb_skip[tx_sz_ctx][ctx]) == 1;
        if all_zero {
            if plane == 0 {
                self.set_tx_types(pos.x4, pos.y4, tx_sz, DCT_DCT);
            }
        } else {
            if plane == 0 {
                self.transform_type(pos.x4, pos.y4, tx_sz);
            }
            self.plane_tx_type = self.compute_tx_type(plane, tx_sz, pos.x4, pos.y4);
            let scan = self.get_scan(tx_sz);
            let tc = TxbClass { tx_sz, tx_class: get_tx_class(self.plane_tx_type), tx_sz_ctx, ptype: (plane > 0) as usize };
            eob = self.read_eob(&tc);
            self.read_levels(&tc, scan, eob);
            (cul_level, dc_category) = self.read_signs_and_remainders(plane, scan, eob, pos);
        }
        self.store_level_context(plane, pos, cul_level, dc_category);
        eob
    }

    /// Reads eob_pt, eob_extra and the extra eob bits, returning the end-of-block position.
    fn read_eob(&mut self, tc: &TxbClass) -> usize {
        let TxbClass { tx_sz, tx_class, tx_sz_ctx, ptype } = *tc;
        let eob_multisize = (TX_WIDTH_LOG2[tx_sz] as usize).min(5) + (TX_HEIGHT_LOG2[tx_sz] as usize).min(5) - 4;
        let ectx = (tx_class != TX_CLASS_2D) as usize;
        let eob_pt = 1 + match eob_multisize {
            0 => self.sd.read_symbol(&mut self.cdf.eob_pt_16[ptype][ectx]),
            1 => self.sd.read_symbol(&mut self.cdf.eob_pt_32[ptype][ectx]),
            2 => self.sd.read_symbol(&mut self.cdf.eob_pt_64[ptype][ectx]),
            3 => self.sd.read_symbol(&mut self.cdf.eob_pt_128[ptype][ectx]),
            4 => self.sd.read_symbol(&mut self.cdf.eob_pt_256[ptype][ectx]),
            5 => self.sd.read_symbol(&mut self.cdf.eob_pt_512[ptype]),
            _ => self.sd.read_symbol(&mut self.cdf.eob_pt_1024[ptype]),
        };
        let mut eob = if eob_pt < 2 { eob_pt } else { (1 << (eob_pt - 2)) + 1 };
        let eob_shift = eob_pt as i32 - 3;
        if eob_shift >= 0 {
            let extra = self.sd.read_symbol(&mut self.cdf.eob_extra[tx_sz_ctx][ptype][eob_pt - 3]);
            if extra == 1 {
                eob += 1 << eob_shift;
            }
            for i in 1..(eob_pt as i32 - 2).max(0) {
                let sh = (eob_pt as i32 - 2).max(0) - 1 - i;
                if self.sd.read_literal(1) == 1 {
                    eob += 1 << sh;
                }
            }
        }
        eob
    }

    /// Reads the base and range levels of every coefficient in reverse scan order into `self.quant`.
    fn read_levels(&mut self, tc: &TxbClass, scan: &[u16], eob: usize) {
        let TxbClass { tx_sz, tx_class, tx_sz_ctx, ptype } = *tc;
        let adj = ADJUSTED_TX_SIZE[tx_sz] as usize;
        let bwl = TX_WIDTH_LOG2[adj] as usize;
        let height = TX_HEIGHT[adj] as usize;
        let width = 1usize << bwl;
        // Levels with TX_PAD zero columns / rows past the right / bottom edge, so the
        // neighbourhood sums need no bounds checks (out-of-range neighbours count as 0).
        let stride = width + TX_PAD;
        let mut lev = std::mem::take(&mut self.levels);
        lev[..(height + TX_PAD) * stride].fill(0);
        let sig = &SIG_REF_DIFF_OFFSET[tx_class];
        let sig_off: [usize; SIG_REF_DIFF_OFFSET_NUM] = std::array::from_fn(|i| sig[i][0] as usize * stride + sig[i][1] as usize);
        let magr = &MAG_REF_OFFSET_WITH_TX_CLASS[tx_class];
        let mag_off: [usize; 3] = std::array::from_fn(|i| magr[i][0] as usize * stride + magr[i][1] as usize);
        for c in (0..eob).rev() {
            let pos = scan[c] as usize;
            let row = pos >> bwl;
            let col = pos & (width - 1);
            let li = row * stride + col;
            let mut level = if c == eob - 1 {
                let ctx = coeff_base_eob_ctx(c, bwl, height);
                self.sd.read_symbol(&mut self.cdf.coeff_base_eob[tx_sz_ctx][ptype][ctx]) as i32 + 1
            } else {
                let mut mag = 0u32;
                for o in sig_off {
                    mag += (lev[li + o] as u32).min(3);
                }
                let ctx = coeff_base_ctx(tx_sz, tx_class, row, col, mag);
                self.sd.read_symbol(&mut self.cdf.coeff_base[tx_sz_ctx][ptype][ctx]) as i32
            };
            if level > NUM_BASE_LEVELS as i32 {
                let mut mag = 0u32;
                for o in mag_off {
                    mag += (lev[li + o] as u32).min((COEFF_BASE_RANGE + NUM_BASE_LEVELS + 1) as u32);
                }
                let ctx = coeff_br_ctx(tx_class, row, col, pos, mag);
                level += self.read_range_level(tx_sz_ctx, ptype, ctx);
            }
            self.quant[pos] = level;
            lev[li] = level as u8;
        }
        self.levels = lev;
    }

    /// Reads the coeff_br increments that raise a base level past NUM_BASE_LEVELS.
    fn read_range_level(&mut self, tx_sz_ctx: usize, ptype: usize, ctx: usize) -> i32 {
        let mut total = 0;
        for _ in 0..(COEFF_BASE_RANGE / (BR_CDF_SIZE - 1)) {
            let br = self.sd.read_symbol(&mut self.cdf.coeff_br[tx_sz_ctx.min(TX_32X32)][ptype][ctx]) as i32;
            total += br;
            if br < (BR_CDF_SIZE - 1) as i32 {
                break;
            }
        }
        total
    }

    /// Reads the Exp-Golomb coded remainder of a coefficient above NUM_BASE_LEVELS + COEFF_BASE_RANGE.
    fn read_golomb_level(&mut self) -> i32 {
        let mut length = 0;
        loop {
            length += 1;
            if self.sd.read_literal(1) == 1 {
                break;
            }
            if length > 32 {
                break;
            }
        }
        let mut x: u32 = 1;
        for _ in (0..length - 1).rev() {
            x = (x << 1) | self.sd.read_literal(1);
        }
        (x as i64 + COEFF_BASE_RANGE as i64 + NUM_BASE_LEVELS as i64).min(i32::MAX as i64) as i32
    }

    /// Reads signs and Golomb remainders in scan order, finishing `self.quant`; returns (culLevel, dcCategory).
    fn read_signs_and_remainders(&mut self, plane: usize, scan: &[u16], eob: usize, p: TxbPos) -> (u32, u8) {
        let ptype = (plane > 0) as usize;
        let mut cul_level = 0u32;
        let mut dc_category = 0u8;
        for (c, &s) in scan.iter().enumerate().take(eob) {
            let pos = s as usize;
            let sign = if self.quant[pos] == 0 {
                false
            } else if c == 0 {
                let ctx = self.dc_sign_ctx(plane, p.x4, p.y4, p.w4, p.h4);
                self.sd.read_symbol(&mut self.cdf.dc_sign[ptype][ctx]) == 1
            } else {
                self.sd.read_literal(1) == 1
            };
            if self.quant[pos] > (NUM_BASE_LEVELS + COEFF_BASE_RANGE) as i32 {
                self.quant[pos] = self.read_golomb_level();
            }
            if pos == 0 && self.quant[pos] > 0 {
                dc_category = if sign { 1 } else { 2 };
            }
            self.quant[pos] &= 0xFFFFF;
            cul_level += self.quant[pos] as u32;
            if sign {
                self.quant[pos] = -self.quant[pos];
            }
        }
        (cul_level.min(63), dc_category)
    }

    /// Records the level and DC category of the coded block in the above / left context arrays.
    fn store_level_context(&mut self, plane: usize, p: TxbPos, cul_level: u32, dc_category: u8) {
        for i in 0..p.w4 {
            if p.x4 + i < self.t.above_level[plane].len() {
                self.t.above_level[plane][p.x4 + i] = cul_level as u8;
                self.t.above_dc[plane][p.x4 + i] = dc_category;
            }
        }
        for i in 0..p.h4 {
            if p.y4 + i < self.t.left_level[plane].len() {
                self.t.left_level[plane][p.y4 + i] = cul_level as u8;
                self.t.left_dc[plane][p.y4 + i] = dc_category;
            }
        }
    }

    fn all_zero_ctx(&self, plane: usize, tx_sz: usize, x4: usize, y4: usize, w4: usize, h4: usize) -> usize {
        let mut max_x4 = self.fs.fh.mi_cols as usize;
        let mut max_y4 = self.fs.fh.mi_rows as usize;
        if plane > 0 {
            max_x4 >>= self.fs.ssx;
            max_y4 >>= self.fs.ssy;
        }
        let w = TX_WIDTH[tx_sz] as usize;
        let h = TX_HEIGHT[tx_sz] as usize;
        let bsize = self.fs.plane_residual_size(self.b.mi_size, plane);
        let bw = block_width(bsize);
        let bh = block_height(bsize);
        let al = &self.t.above_level[plane];
        let ll = &self.t.left_level[plane];
        if plane == 0 {
            let mut top = 0u32;
            let mut left = 0u32;
            for k in 0..w4 {
                if x4 + k < max_x4 {
                    top = top.max(al[x4 + k] as u32);
                }
            }
            for k in 0..h4 {
                if y4 + k < max_y4 {
                    left = left.max(ll[y4 + k] as u32);
                }
            }
            top = top.min(255);
            left = left.min(255);
            if bw == w && bh == h {
                0
            } else if top == 0 && left == 0 {
                1
            } else if top == 0 || left == 0 {
                2 + (top.max(left) > 3) as usize
            } else if top.max(left) <= 3 {
                4
            } else if top.min(left) <= 3 {
                5
            } else {
                6
            }
        } else {
            let ad = &self.t.above_dc[plane];
            let ld = &self.t.left_dc[plane];
            let mut above = 0u8;
            let mut left = 0u8;
            for i in 0..w4 {
                if x4 + i < max_x4 {
                    above |= al[x4 + i];
                    above |= ad[x4 + i];
                }
            }
            for i in 0..h4 {
                if y4 + i < max_y4 {
                    left |= ll[y4 + i];
                    left |= ld[y4 + i];
                }
            }
            let mut ctx = (above != 0) as usize + (left != 0) as usize;
            ctx += 7;
            if bw * bh > w * h {
                ctx += 3;
            }
            ctx
        }
    }

    fn dc_sign_ctx(&self, plane: usize, x4: usize, y4: usize, w4: usize, h4: usize) -> usize {
        let mut max_x4 = self.fs.fh.mi_cols as usize;
        let mut max_y4 = self.fs.fh.mi_rows as usize;
        if plane > 0 {
            max_x4 >>= self.fs.ssx;
            max_y4 >>= self.fs.ssy;
        }
        let mut dc_sign = 0i32;
        for k in 0..w4 {
            if x4 + k < max_x4 {
                match self.t.above_dc[plane][x4 + k] {
                    1 => dc_sign -= 1,
                    2 => dc_sign += 1,
                    _ => {}
                }
            }
        }
        for k in 0..h4 {
            if y4 + k < max_y4 {
                match self.t.left_dc[plane][y4 + k] {
                    1 => dc_sign -= 1,
                    2 => dc_sign += 1,
                    _ => {}
                }
            }
        }
        match dc_sign.signum() {
            -1 => 1,
            1 => 2,
            _ => 0,
        }
    }

    /// Reconstruct process (7.12.3).
    pub(super) fn reconstruct(&mut self, plane: usize, x: usize, y: usize, tx_sz: usize, dc_only: bool) {
        // dqDenom (7.12.3) is 1, 2 or 4 and the dividend is non-negative.
        let dq_shift = match tx_sz {
            TX_32X32 | TX_16X32 | TX_32X16 | TX_16X64 | TX_64X16 => 1,
            TX_64X64 | TX_32X64 | TX_64X32 => 2,
            _ => 0,
        };
        let log2w = TX_WIDTH_LOG2[tx_sz] as u32;
        let log2h = TX_HEIGHT_LOG2[tx_sz] as u32;
        let w = 1usize << log2w;
        let h = 1usize << log2h;
        let tw = w.min(32);
        let th = h.min(32);
        let t = self.plane_tx_type;
        let flip_ud = matches!(t, FLIPADST_DCT | FLIPADST_ADST | V_FLIPADST | FLIPADST_FLIPADST);
        let flip_lr = matches!(t, DCT_FLIPADST | ADST_FLIPADST | H_FLIPADST | FLIPADST_FLIPADST);
        let bd = self.fs.bit_depth;
        let fh = &self.fs.fh;
        let qindex = fh.get_qindex(false, self.b.segment_id, self.current_q_index);
        let (dc_delta, ac_delta) = match plane {
            0 => (fh.quant.delta_q_y_dc, 0),
            1 => (fh.quant.delta_q_u_dc, fh.quant.delta_q_u_ac),
            _ => (fh.quant.delta_q_v_dc, fh.quant.delta_q_v_ac),
        };
        let bd_idx = ((bd - 8) >> 1) as usize;
        let dc_q = DC_QLOOKUP[bd_idx][(qindex + dc_delta).clamp(0, 255) as usize] as i64;
        let ac_q = AC_QLOOKUP[bd_idx][(qindex + ac_delta).clamp(0, 255) as usize] as i64;
        let qm_level = if fh.quant.using_qmatrix { fh.seg_qm_level[plane][self.b.segment_id] } else { 15 };
        let use_qm = fh.quant.using_qmatrix && t < IDTX && qm_level < 15;
        let lim = 1i64 << (7 + bd);
        // inverse_transform_2d reads only the top-left tw x th coefficients (stride 64)
        for i in 0..th {
            self.dequant[i * 64..i * 64 + tw].fill(0);
        }
        for i in 0..th {
            for j in 0..tw {
                let qv = std::mem::take(&mut self.quant[i * tw + j]);
                if qv == 0 {
                    continue;
                }
                let q = if i == 0 && j == 0 { dc_q } else { ac_q };
                let q2 = if use_qm {
                    let m = QUANTIZER_MATRIX[qm_level as usize][(plane > 0) as usize][QM_OFFSET[tx_sz] as usize + i * tw + j] as i64;
                    (q * m + 16) >> 5
                } else {
                    q
                };
                let dq = qv as i64 * q2;
                let mag = (dq.abs() & 0xFFFFFF) >> dq_shift;
                let dq2 = if dq < 0 { -mag } else { mag };
                self.dequant[i * 64 + j] = dq2.clamp(-lim, lim - 1) as i32;
            }
        }
        let lossless = self.b.lossless;
        inverse_transform_2d(&self.dequant, &mut self.residual, tx_sz, t, lossless, bd, dc_only);
        let pl = &mut self.t.cur.planes[plane];
        let max = (1i32 << bd) - 1;
        for i in 0..h {
            let yy = if flip_ud { h - i - 1 } else { i };
            let row = &mut pl.row_from_mut(y + yy, x)[..w];
            let res = &self.residual[i * w..i * w + w];
            if flip_lr {
                for (o, &r) in row.iter_mut().rev().zip(res) {
                    *o = (*o as i32 + r).max(0).min(max) as u16;
                }
            } else {
                for (o, &r) in row.iter_mut().zip(res) {
                    *o = (*o as i32 + r).max(0).min(max) as u16;
                }
            }
        }
    }
}

pub(super) fn inverse_recenter(r: i32, v: i32) -> i32 {
    if v > 2 * r {
        v
    } else if v & 1 == 1 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

fn default_scan(tx_sz: usize) -> &'static [u16] {
    match tx_sz {
        TX_4X4 => &DEFAULT_SCAN_4X4,
        TX_4X8 => &DEFAULT_SCAN_4X8,
        TX_8X4 => &DEFAULT_SCAN_8X4,
        TX_8X8 => &DEFAULT_SCAN_8X8,
        TX_8X16 => &DEFAULT_SCAN_8X16,
        TX_16X8 => &DEFAULT_SCAN_16X8,
        TX_16X16 => &DEFAULT_SCAN_16X16,
        TX_16X32 => &DEFAULT_SCAN_16X32,
        TX_32X16 => &DEFAULT_SCAN_32X16,
        TX_4X16 => &DEFAULT_SCAN_4X16,
        TX_16X4 => &DEFAULT_SCAN_16X4,
        TX_8X32 => &DEFAULT_SCAN_8X32,
        TX_32X8 => &DEFAULT_SCAN_32X8,
        _ => &DEFAULT_SCAN_32X32,
    }
}

fn get_tx_class(t: usize) -> usize {
    if t == V_DCT || t == V_ADST || t == V_FLIPADST {
        TX_CLASS_VERT
    } else if t == H_DCT || t == H_ADST || t == H_FLIPADST {
        TX_CLASS_HORIZ
    } else {
        TX_CLASS_2D
    }
}

/// Coefficient base context from the neighbourhood magnitude `mag` (get_coeff_base_ctx).
#[inline(always)]
fn coeff_base_ctx(tx_sz: usize, tx_class: usize, row: usize, col: usize, mag: u32) -> usize {
    let ctx = ((mag + 1) >> 1).min(4) as usize;
    if tx_class == TX_CLASS_2D {
        if row == 0 && col == 0 {
            return 0;
        }
        return ctx + COEFF_BASE_CTX_OFFSET[tx_sz][row.min(4)][col.min(4)] as usize;
    }
    let idx = if tx_class == TX_CLASS_VERT { row } else { col };
    ctx + COEFF_BASE_POS_CTX_OFFSET[idx.min(2)] as usize
}

/// coeff_br context from the neighbourhood magnitude `mag`.
#[inline(always)]
fn coeff_br_ctx(tx_class: usize, row: usize, col: usize, pos: usize, mag: u32) -> usize {
    let mag = ((mag + 1) >> 1).min(6) as usize;
    if pos == 0 {
        mag
    } else if tx_class == 0 {
        if row < 2 && col < 2 { mag + 7 } else { mag + 14 }
    } else if tx_class == 1 {
        if col == 0 { mag + 7 } else { mag + 14 }
    } else if row == 0 {
        mag + 7
    } else {
        mag + 14
    }
}

/// Zero padding (columns and rows) of the coefficient level buffer.
pub(super) const TX_PAD: usize = 4;

fn coeff_base_eob_ctx(c: usize, bwl: usize, height: usize) -> usize {
    if c == 0 {
        return 0;
    }
    if c <= (height << bwl) / 8 {
        return 1;
    }
    if c <= (height << bwl) / 4 {
        return 2;
    }
    3
}

pub(super) fn ceil_log2(x: u32) -> u32 {
    if x < 2 {
        return 0;
    }
    let mut i = 1;
    let mut p = 2;
    while p < x {
        i += 1;
        p <<= 1;
    }
    i
}

pub(super) fn neg_deinterleave(diff: i32, r: i32, max: i32) -> i32 {
    if r == 0 {
        return diff;
    }
    if r >= max - 1 {
        return max - diff - 1;
    }
    if 2 * r < max {
        if diff <= 2 * r {
            if diff & 1 == 1 {
                return r + ((diff + 1) >> 1);
            } else {
                return r - (diff >> 1);
            }
        }
        diff
    } else {
        if diff <= 2 * (max - r - 1) {
            if diff & 1 == 1 {
                return r + ((diff + 1) >> 1);
            } else {
                return r - (diff >> 1);
            }
        }
        max - (diff + 1)
    }
}

use super::*;

impl<'a, S: Sample> SliceDecoder<'a, S> {
    pub(super) fn transform_tree(&mut self, x0: i32, y0: i32, xb: i32, yb: i32, log2: u32, depth: u32, blk_idx: u32, parent_cbf_c: [bool; 2]) -> Result<()> {
        let split = if log2 <= self.sps.log2_max_tb && log2 > self.sps.log2_min_tb && depth < self.max_trafo_depth && !(self.intra_split && depth == 0) {
            self.dd(SPLIT_TRANSFORM + (5 - log2 as usize)) == 1
        } else {
            // interSplitFlag (7.4.9.8)
            let inter_split = self.sps.max_th_depth_inter == 0 && !self.cu_intra && self.cu_part != PartMode::P2Nx2N && depth == 0;
            log2 > self.sps.log2_max_tb || (self.intra_split && depth == 0) || inter_split
        };
        let mut cbf_c = [false, false];
        if log2 > 2 {
            for (c, cbf) in cbf_c.iter_mut().enumerate() {
                if depth == 0 || parent_cbf_c[c] {
                    *cbf = self.dd(CBF_CHROMA + depth as usize) == 1;
                }
            }
        } else {
            // 4x4 luma blocks: chroma cbfs are those of the parent (coded at blkIdx 3)
            cbf_c = parent_cbf_c;
            if depth == 0 {
                cbf_c = [false, false];
            }
        }
        if split {
            let h = 1 << (log2 - 1);
            self.transform_tree(x0, y0, x0, y0, log2 - 1, depth + 1, 0, cbf_c)?;
            self.transform_tree(x0 + h, y0, x0, y0, log2 - 1, depth + 1, 1, cbf_c)?;
            self.transform_tree(x0, y0 + h, x0, y0, log2 - 1, depth + 1, 2, cbf_c)?;
            self.transform_tree(x0 + h, y0 + h, x0, y0, log2 - 1, depth + 1, 3, cbf_c)?;
            return Ok(());
        }
        let cbf_luma = if self.cu_intra || depth != 0 || cbf_c[0] || cbf_c[1] { self.dd(CBF_LUMA + if depth == 0 { 1 } else { 0 }) == 1 } else { true };
        let n = 1 << log2;
        if depth > 0 {
            self.mark_edges(x0, y0, n, n, E_TU_V, E_TU_H);
        }
        self.transform_unit(x0, y0, xb, yb, log2, blk_idx, cbf_luma, cbf_c)
    }

    fn transform_unit(&mut self, x0: i32, y0: i32, xb: i32, yb: i32, log2: u32, blk_idx: u32, cbf_luma: bool, cbf_c: [bool; 2]) -> Result<()> {
        let n = 1i32 << log2;
        let chroma_here = log2 > 2 || blk_idx == 3;
        let cbf_chroma = cbf_c[0] || cbf_c[1];
        if (cbf_luma || cbf_chroma) && self.pps.cu_qp_delta_enabled && !self.is_cu_qp_delta_coded {
            // cu_qp_delta_abs: prefix TU(5) with ctx 0,1,1,1,1, suffix EG0
            let mut v = 0;
            while v < 5 && self.dd(CU_QP_DELTA_ABS + if v == 0 { 0 } else { 1 }) == 1 {
                v += 1;
            }
            if v == 5 {
                let mut k = 0;
                while self.bypass() == 1 {
                    v += 1 << k;
                    k += 1;
                    ensure!(k < 32, "invalid cu_qp_delta_abs");
                }
                v += self.bypass_bits(k) as i32;
            }
            if v != 0 && self.bypass() == 1 {
                v = -v;
            }
            let off = 6 * (self.pic.bd_y as i32 - 8);
            ensure!(v >= -(26 + off / 2) && v <= 25 + off / 2, "CuQpDeltaVal out of range");
            self.is_cu_qp_delta_coded = true;
            self.cu_qp_delta = v;
            self.update_qp();
        }
        // luma
        if self.cu_intra {
            let mode = self.pic.blk_at(x0, y0).ipm as u32;
            self.intra_predict(x0, y0, log2, 0, mode);
        }
        if cbf_luma {
            let qp_bd = 6 * (self.pic.bd_y as i32 - 8);
            let qp = self.qp_y + qp_bd;
            self.residual(x0, y0, log2, 0, qp)?;
            self.fill_blk(x0, y0, n, n, |b| b.flags |= F_CBF);
        }
        if !chroma_here {
            return Ok(());
        }
        let (xc, yc, log2c) = if log2 > 2 { (x0 / 2, y0 / 2, log2 - 1) } else { (xb / 2, yb / 2, 2) };
        for c in 1..3 {
            if self.cu_intra {
                let mode = self.intra_chroma_mode;
                self.intra_predict(xc, yc, log2c, c, mode);
            }
            if cbf_c[c - 1] {
                let qp = if c == 1 { self.qp_cb } else { self.qp_cr };
                self.residual(xc, yc, log2c, c, qp)?;
            }
        }
        Ok(())
    }

    /// Intra prediction of an n x n block of component `c` at component position (x0, y0).
    fn intra_predict(&mut self, x0: i32, y0: i32, log2: u32, c: usize, mode: u32) {
        let n = 1usize << log2;
        let sc = if c == 0 { 1 } else { 2 };
        let (xl, yl) = (x0 * sc, y0 * sc);
        let unit = 4 / sc as usize; // component samples per 4x4 luma block
        let stride = if c == 0 { self.pic.width } else { self.pic.cwidth };
        let constrained = self.pps.constrained_intra_pred;
        let mut al = [false; 129];
        let mut at = [false; 129];
        let addr = self.slice_addr;
        let avail =
            |pic: &PicState<S>, xn: i32, yn: i32| -> bool { pic.zavail(xl, yl, xn, yn, addr) && (!constrained || pic.blk_at(xn, yn).flags & F_INTRA != 0) };
        let plane = &self.pic.planes[c];
        let r = &mut self.refs_buf;
        // corner
        if avail(self.pic, xl - 1, yl - 1) {
            al[0] = true;
            at[0] = true;
            let v = plane[(y0 - 1) as usize * stride + (x0 - 1) as usize].to_u16();
            r.left[0] = v;
            r.top[0] = v;
        }
        let mut k = 0;
        while k < 2 * n {
            let yn = y0 + k as i32;
            if avail(self.pic, xl - 1, yn * sc) {
                for i in 0..unit {
                    al[1 + k + i] = true;
                    r.left[1 + k + i] = plane[(yn as usize + i) * stride + (x0 - 1) as usize].to_u16();
                }
            }
            let xn = x0 + k as i32;
            if avail(self.pic, xn * sc, yl - 1) {
                let o = (y0 - 1) as usize * stride + xn as usize;
                for i in 0..unit {
                    at[1 + k + i] = true;
                    r.top[1 + k + i] = plane[o + i].to_u16();
                }
            }
            k += unit;
        }
        let bd = if c == 0 { self.pic.bd_y } else { self.pic.bd_c };
        intra::substitute(r, &al, &at, n, bd);
        if c == 0 {
            intra::filter(r, mode, n, self.sps.strong_intra_smoothing, true, bd);
        }
        let off = y0 as usize * stride + x0 as usize;
        intra::predict(r, mode, n, c == 0, bd, &mut self.pic.planes[c][off..], stride);
    }

    /// residual_coding() + scaling + transform + reconstruction for one transform block.
    fn residual(&mut self, x0: i32, y0: i32, log2: u32, c: usize, qp: i32) -> Result<()> {
        let n = 1usize << log2;
        let bypass = self.cu_transquant_bypass;
        let ts = if self.pps.transform_skip && !bypass && log2 == 2 { self.dd(TRANSFORM_SKIP + (c > 0) as usize) == 1 } else { false };
        // last significant coefficient
        let cmax = (log2 << 1) - 1;
        let (off, shift) = if c == 0 { (3 * (log2 - 2) + ((log2 - 1) >> 2), (log2 + 1) >> 2) } else { (15, log2 - 2) };
        let mut px = 0;
        while px < cmax && self.dd(LAST_X_PREFIX + (off + (px >> shift)) as usize) == 1 {
            px += 1;
        }
        let mut py = 0;
        while py < cmax && self.dd(LAST_Y_PREFIX + (off + (py >> shift)) as usize) == 1 {
            py += 1;
        }
        let mut lx = px;
        if px > 3 {
            let nb = (px >> 1) - 1;
            lx = (1 << nb) * (2 + (px & 1)) + self.bypass_bits(nb);
        }
        let mut ly = py;
        if py > 3 {
            let nb = (py >> 1) - 1;
            ly = (1 << nb) * (2 + (py & 1)) + self.bypass_bits(nb);
        }
        // scanIdx (7.4.9.11)
        let scan_idx = if self.cu_intra && (log2 == 2 || (log2 == 3 && c == 0)) {
            let m = if c == 0 { self.pic.blk_at(x0, y0).ipm as u32 } else { self.intra_chroma_mode };
            if (6..=14).contains(&m) {
                2
            } else if (22..=30).contains(&m) {
                1
            } else {
                0
            }
        } else {
            0
        };
        if scan_idx == 2 {
            std::mem::swap(&mut lx, &mut ly);
        }
        ensure!((lx as usize) < n && (ly as usize) < n, "last significant coefficient outside the block");
        let log2sb = log2 - 2;
        let sb_scan = scan_order(log2sb, scan_idx);
        let pos_scan = &SCAN_4[scan_idx];
        let sb_w = 1usize << log2sb;
        let last_sb = sb_scan.iter().position(|&(x, y)| x as u32 == lx >> 2 && y as u32 == ly >> 2).unwrap_or(0);
        let last_pos = pos_scan.iter().position(|&(x, y)| x as u32 == lx & 3 && y as u32 == ly & 3).unwrap_or(0);
        self.coeffs[..n * n].fill(0);
        let mut csbf = [0u8; 64];
        let mut prev_g1: Option<u32> = None;
        let sdh = self.pps.sign_data_hiding && !bypass;
        let (mut max_x, mut max_y) = (0usize, 0usize);
        let c_off_sig = if c == 0 { 0 } else { 27 };
        for i in (0..=last_sb).rev() {
            let (xs, ys) = (sb_scan[i].0 as usize, sb_scan[i].1 as usize);
            let mut infer_dc = false;
            let right = if xs + 1 < sb_w { csbf[ys * 8 + xs + 1] } else { 0 };
            let below = if ys + 1 < sb_w { csbf[(ys + 1) * 8 + xs] } else { 0 };
            if i < last_sb && i > 0 {
                let inc = (right | below).min(1) as usize + if c > 0 { 2 } else { 0 };
                csbf[ys * 8 + xs] = self.c.decode_decision(CODED_SUB_BLOCK + inc) as u8;
                infer_dc = true;
            } else {
                csbf[ys * 8 + xs] = 1;
            }
            // significance map
            let mut sig = [0u8; 16]; // scan positions n of significant coefficients, descending
            let mut nsig = 0;
            let start: i32 = if i == last_sb {
                sig[0] = last_pos as u8;
                nsig = 1;
                last_pos as i32 - 1
            } else {
                15
            };
            if csbf[ys * 8 + xs] != 0 {
                let prev_csbf = right + (below << 1);
                let mut np = start;
                while np >= 0 {
                    let (xp, yp) = (pos_scan[np as usize].0 as usize, pos_scan[np as usize].1 as usize);
                    let (xc, yc) = (xs * 4 + xp, ys * 4 + yp);
                    if np > 0 || !infer_dc {
                        let sig_ctx = if log2 == 2 {
                            CTX_IDX_MAP[(yc << 2) + xc] as usize
                        } else if xc + yc == 0 {
                            0
                        } else {
                            let mut s = match prev_csbf {
                                0 => {
                                    if xp + yp == 0 {
                                        2
                                    } else if xp + yp < 3 {
                                        1
                                    } else {
                                        0
                                    }
                                }
                                1 => {
                                    if yp == 0 {
                                        2
                                    } else if yp == 1 {
                                        1
                                    } else {
                                        0
                                    }
                                }
                                2 => {
                                    if xp == 0 {
                                        2
                                    } else if xp == 1 {
                                        1
                                    } else {
                                        0
                                    }
                                }
                                _ => 2,
                            };
                            if c == 0 {
                                if xs + ys > 0 {
                                    s += 3;
                                }
                                s += if log2 == 3 { if scan_idx == 0 { 9 } else { 15 } } else { 21 };
                            } else {
                                s += if log2 == 3 { 9 } else { 12 };
                            }
                            s
                        };
                        if self.c.decode_decision(SIG_COEFF + c_off_sig + sig_ctx) == 1 {
                            sig[nsig] = np as u8;
                            nsig += 1;
                            infer_dc = false;
                        }
                    } else {
                        // np == 0 with inferSbDcSigCoeffFlag
                        sig[nsig] = 0;
                        nsig += 1;
                    }
                    np -= 1;
                }
            }
            if nsig == 0 {
                continue;
            }
            // greater1 / greater2 flags
            let mut ctx_set = if i == 0 || c > 0 { 0 } else { 2 };
            if let Some(g) = prev_g1
                && g == 0
            {
                ctx_set += 1;
            }
            let mut g1ctx = 1u32;
            let mut gt1 = [0u8; 16];
            let mut first_g1: Option<usize> = None;
            let c_off_g1 = if c > 0 { 16 } else { 0 };
            for k in 0..nsig.min(8) {
                let inc = ctx_set * 4 + g1ctx.min(3) as usize + c_off_g1;
                let f = self.c.decode_decision(GT1 + inc);
                gt1[k] = f as u8;
                if f == 1 {
                    g1ctx = 0;
                    if first_g1.is_none() {
                        first_g1 = Some(k);
                    }
                } else if g1ctx > 0 {
                    g1ctx += 1;
                }
            }
            prev_g1 = Some(g1ctx);
            let mut gt2 = 0u8;
            if let Some(k) = first_g1 {
                gt2 = self.c.decode_decision(GT2 + ctx_set + if c > 0 { 4 } else { 0 }) as u8;
                let _ = k;
            }
            // signs
            let last_sig_pos = sig[0] as i32;
            let first_sig_pos = sig[nsig - 1] as i32;
            let sign_hidden = sdh && last_sig_pos - first_sig_pos > 3;
            let nsigns = if sign_hidden { nsig - 1 } else { nsig };
            let mut signs = if nsigns > 0 { self.bypass_bits(nsigns as u32) << (32 - nsigns) } else { 0 };
            // remaining levels
            let mut rice = 0u32;
            let mut sum_abs = 0i32;
            for k in 0..nsig {
                let mut base = 1 + gt1[k] as i32;
                if Some(k) == first_g1 {
                    base += gt2 as i32;
                }
                let thresh = if k < 8 { if Some(k) == first_g1 { 3 } else { 2 } } else { 1 };
                let mut level = base;
                if base == thresh {
                    let rem = self.coeff_abs_level_remaining(rice)?;
                    level = base + rem;
                    if level > 3 * (1 << rice) {
                        rice = (rice + 1).min(4);
                    }
                }
                let np = sig[k] as usize;
                let (xc, yc) = (xs * 4 + pos_scan[np].0 as usize, ys * 4 + pos_scan[np].1 as usize);
                let mut v = level;
                if !(sign_hidden && k == nsig - 1) {
                    if signs & 0x8000_0000 != 0 {
                        v = -v;
                    }
                    signs <<= 1;
                }
                if sign_hidden {
                    sum_abs += level;
                    if k == nsig - 1 && sum_abs & 1 == 1 {
                        v = -v;
                    }
                }
                self.coeffs[yc * n + xc] = v;
                max_x = max_x.max(xc);
                max_y = max_y.max(yc);
            }
        }
        self.reconstruct(x0, y0, log2, c, qp, ts, max_x, max_y)
    }

    fn coeff_abs_level_remaining(&mut self, rice: u32) -> Result<i32> {
        let mut prefix = 0u32;
        while prefix < 32 && self.bypass() == 1 {
            prefix += 1;
        }
        ensure!(prefix < 32, "invalid coeff_abs_level_remaining");
        if prefix <= 3 {
            Ok(((prefix << rice) + self.bypass_bits(rice)) as i32)
        } else {
            let nb = prefix - 3 + rice;
            ensure!(nb <= 24, "coeff_abs_level_remaining too large");
            Ok(((((1u32 << (prefix - 3)) + 2) << rice) + self.bypass_bits(nb)) as i32)
        }
    }

    /// Scaling, transform and addition to the prediction.
    fn reconstruct(&mut self, x0: i32, y0: i32, log2: u32, c: usize, qp: i32, ts: bool, max_x: usize, max_y: usize) -> Result<()> {
        let n = 1usize << log2;
        let bd = if c == 0 { self.pic.bd_y } else { self.pic.bd_c };
        let coeffs = &mut self.coeffs[..n * n];
        let mut dc_only = false;
        if !self.cu_transquant_bypass {
            // scaling (8.6.3)
            let bd_shift = bd + log2 - 5;
            let round = 1i64 << (bd_shift - 1);
            let scale = (LEVEL_SCALE[(qp % 6) as usize] as i64) << (qp / 6);
            let m: Option<&[u8]> = match &self.pic.scaling {
                Some(s) if !(ts && n > 4) => {
                    let size_id = (log2 - 2) as usize;
                    let matrix_id = if self.cu_intra { c } else { 3 + c };
                    Some(&s[size_id * 6 + matrix_id])
                }
                _ => None,
            };
            for y in 0..=max_y {
                for x in 0..=max_x {
                    let v = coeffs[y * n + x];
                    if v != 0 {
                        let f = m.map_or(16, |m| m[y * n + x] as i64);
                        coeffs[y * n + x] = ((v as i64 * f * scale + round) >> bd_shift).clamp(-32768, 32767) as i32;
                    }
                }
            }
            let bd_shift = 20 - bd;
            if ts {
                transform::transform_skip(coeffs, n, bd_shift);
            } else if max_x == 0 && max_y == 0 && !(self.cu_intra && c == 0 && n == 4) {
                let v = transform::inverse_dc(coeffs[0], bd_shift);
                coeffs.fill(v);
                dc_only = true;
            } else {
                let dst = self.cu_intra && c == 0 && n == 4;
                transform::inverse_transform(coeffs, n, dst, max_x, max_y, bd_shift);
            }
        }
        let _ = dc_only;
        let (stride, max) = (if c == 0 { self.pic.width } else { self.pic.cwidth }, (1i32 << bd) - 1);
        let plane = &mut self.pic.planes[c];
        for y in 0..n {
            let row = &mut plane[(y0 as usize + y) * stride + x0 as usize..][..n];
            for (x, s) in row.iter_mut().enumerate() {
                *s = S::from_i32((s.to_i32() + coeffs[y * n + x]).clamp(0, max));
            }
        }
        Ok(())
    }
}

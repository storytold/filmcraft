use super::*;

impl<'a, S: Sample> SliceDecoder<'a, S> {
    /// prediction_unit(): parse, derive motion, store it and predict. Returns merge_flag.
    pub(super) fn prediction_unit(
        &mut self,
        xc: i32,
        yc: i32,
        ncb: i32,
        xp: i32,
        yp: i32,
        w: i32,
        h: i32,
        part_idx: usize,
        part: PartMode,
        skip: bool,
    ) -> Result<bool> {
        let merge = if skip { true } else { self.dd(MERGE_FLAG) == 1 };
        let mvf = if merge {
            let idx = if self.sh.max_num_merge_cand > 1 {
                let mut i = 0;
                if self.dd(MERGE_IDX) == 1 {
                    i = 1;
                    while i < self.sh.max_num_merge_cand - 1 && self.bypass() == 1 {
                        i += 1;
                    }
                }
                i
            } else {
                0
            };
            self.derive_merge(xc, yc, ncb, xp, yp, w, h, part_idx, part, idx as usize)
        } else {
            let pred_idc = if self.sh.is_b() {
                if w + h != 12 {
                    let depth = self.pic.blk_at(xc, yc).depth as usize;
                    if self.dd(INTER_PRED_IDC + depth) == 1 { 2 } else { self.dd(INTER_PRED_IDC + 4) }
                } else {
                    self.dd(INTER_PRED_IDC + 4)
                }
            } else {
                0
            };
            let mut ref_idx = [-1i8; 2];
            let mut mvd = [[0i32; 2]; 2];
            let mut mvp_flag = [0u32; 2];
            for l in 0..2 {
                let used = if l == 0 { pred_idc != 1 } else { pred_idc != 0 };
                if !used {
                    continue;
                }
                let nref = self.sh.num_ref_idx[l];
                let mut ri = 0;
                if nref > 1 {
                    while ri < nref - 1 {
                        let bin = if ri < 2 { self.dd(REF_IDX + ri as usize) } else { self.bypass() };
                        if bin == 0 {
                            break;
                        }
                        ri += 1;
                    }
                }
                ref_idx[l] = ri as i8;
                if l == 1 && self.sh.mvd_l1_zero && pred_idc == 2 {
                    mvd[1] = [0, 0];
                } else {
                    mvd[l] = self.mvd_coding()?;
                }
                mvp_flag[l] = self.dd(MVP_FLAG);
            }
            let mut f = MvField { mv: [[0; 2]; 2], ref_idx };
            for l in 0..2 {
                if ref_idx[l] < 0 {
                    continue;
                }
                let mvp = self.derive_amvp(xc, yc, ncb, xp, yp, w, h, part_idx, l, ref_idx[l] as usize, mvp_flag[l] as usize);
                // mvLX = (mvp + mvd) wrapped to 16 bits (8-94)
                f.mv[l] = [(mvp[0] as i32 + mvd[l][0]) as i16, (mvp[1] as i32 + mvd[l][1]) as i16];
            }
            f
        };
        for l in 0..2 {
            if mvf.ref_idx[l] >= 0 {
                ensure!((mvf.ref_idx[l] as usize) < self.refs[l].len(), "reference index out of range");
            }
        }
        // store motion
        let w4 = self.pic.w4;
        for by in (yp >> 2)..((yp + h) >> 2) {
            let o = by as usize * w4;
            self.pic.mvf[o + (xp >> 2) as usize..o + ((xp + w) >> 2) as usize].fill(mvf);
        }
        self.motion_compensate(xp, yp, w as usize, h as usize, &mvf);
        Ok(merge)
    }

    fn mvd_coding(&mut self) -> Result<[i32; 2]> {
        let g0 = [self.dd(ABS_MVD_GT0), self.dd(ABS_MVD_GT0)];
        let mut g1 = [0, 0];
        for k in 0..2 {
            if g0[k] == 1 {
                g1[k] = self.dd(ABS_MVD_GT1);
            }
        }
        let mut out = [0i32; 2];
        for k in 0..2 {
            if g0[k] == 1 {
                let mut v = 1i32;
                if g1[k] == 1 {
                    // EG1
                    let mut kk = 1;
                    let mut val = 0i32;
                    while self.bypass() == 1 {
                        val += 1 << kk;
                        kk += 1;
                        ensure!(kk < 31, "invalid abs_mvd_minus2");
                    }
                    val += self.bypass_bits(kk) as i32;
                    v = val + 2;
                }
                if self.bypass() == 1 {
                    v = -v;
                }
                out[k] = v;
            }
        }
        Ok(out)
    }

    /// Motion compensation + weighted prediction of one PU into the picture.
    fn motion_compensate(&mut self, xp: i32, yp: i32, w: usize, h: usize, f: &MvField) {
        let weighted = match self.sh.slice_type {
            SliceType::P => self.pps.weighted_pred,
            SliceType::B => self.pps.weighted_bipred,
            SliceType::I => false,
        };
        let bi = f.pred(0) && f.pred(1);
        let pwt = self.sh.pwt.as_ref().filter(|_| weighted);
        for c in 0..3 {
            let (x, y, bw, bh, bd) = if c == 0 { (xp, yp, w, h, self.pic.bd_y) } else { (xp / 2, yp / 2, w / 2, h / 2, self.pic.bd_c) };
            // Explicit weights equal to the default ones (weight 2^denom and offset 0 for every
            // list used, as signalled for most slices of a weighted-prediction stream) give exactly
            // the default prediction: with log2WD = denom + shift1 the weighted sums of 8-265 /
            // 8-266 are those of 8-262 / 8-263 multiplied by 2^denom.
            let pwt = pwt.filter(|pwt| !default_weights(pwt, f, c));
            // Uni-prediction at a full-sample position without weighting: the prediction
            // ((s << shift3) + off) >> shift3 (8-228, 8-262) is the reference sample itself.
            if !bi && pwt.is_none() {
                let l = if f.pred(0) { 0 } else { 1 };
                let frac = if c == 0 { 3 } else { 7 };
                let mv = f.mv[l];
                if f.pred(l) && mv[0] & frac == 0 && mv[1] & frac == 0 {
                    let sh = if c == 0 { 2 } else { 3 };
                    let stride = if c == 0 { self.pic.width } else { self.pic.cwidth };
                    let frame = &self.refs[l][f.ref_idx[l] as usize].frame;
                    let dst = &mut self.pic.planes[c][y as usize * stride + x as usize..];
                    frame.copy_block(c, x + (mv[0] as i32 >> sh), y + (mv[1] as i32 >> sh), bw, bh, dst, stride);
                    continue;
                }
            }
            let mut lists = [false; 2];
            for l in 0..2 {
                if !f.pred(l) {
                    continue;
                }
                lists[l] = true;
                let frame = &self.refs[l][f.ref_idx[l] as usize].frame;
                let buf = if l == 0 || !bi { &mut self.pred0 } else { &mut self.pred1 };
                if c == 0 {
                    inter::mc_luma::<S>(frame, x, y, f.mv[l], bw, bh, buf, &mut self.mc);
                } else {
                    inter::mc_chroma::<S>(frame, c - 1, x, y, f.mv[l], bw, bh, buf, &mut self.mc);
                }
            }
            let stride = if c == 0 { self.pic.width } else { self.pic.cwidth };
            let dst = &mut self.pic.planes[c][y as usize * stride + x as usize..];
            let Some(pwt) = pwt else {
                if bi {
                    inter::put_bi(&self.pred0, &self.pred1, bw, bh, bd, dst, stride);
                } else {
                    inter::put_uni(&self.pred0, bw, bh, bd, dst, stride);
                }
                continue;
            };
            let shift1 = 14 - bd;
            let log2wd = if c == 0 { pwt.luma_log2_denom } else { pwt.chroma_log2_denom } + shift1;
            let get = |l: usize| -> (i32, i32) {
                let e = &pwt.l[l][f.ref_idx[l] as usize];
                let (wt, o) = if c == 0 { e.luma } else { e.chroma[c - 1] };
                (wt, o << (bd - 8))
            };
            if bi {
                let (w0, o0) = get(0);
                let (w1, o1) = get(1);
                inter::put_weighted_bi(&self.pred0, &self.pred1, bw, bh, bd, log2wd, w0, w1, o0, o1, dst, stride);
            } else {
                let l = if lists[0] { 0 } else { 1 };
                let (wt, o) = get(l);
                inter::put_weighted_uni(&self.pred0, bw, bh, bd, log2wd, wt, o, dst, stride);
            }
        }
    }
}

/// Whether the explicit weights of the lists `f` predicts from are the default ones for component
/// `c` (weight 2^denom, offset 0).
fn default_weights(pwt: &crate::slice::PredWeightTable, f: &MvField, c: usize) -> bool {
    let denom = if c == 0 { pwt.luma_log2_denom } else { pwt.chroma_log2_denom };
    (0..2).filter(|&l| f.pred(l)).all(|l| {
        pwt.l[l].get(f.ref_idx[l] as usize).is_some_and(|e| {
            let (wt, o) = if c == 0 { e.luma } else { e.chroma[c - 1] };
            wt == 1 << denom && o == 0
        })
    })
}

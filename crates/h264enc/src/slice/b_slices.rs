use super::*;

impl<'a, 'b> SliceEnc<'a, 'b> {
    // ------------------------------------------------------------------ B slices

    /// Spatial direct prediction (§8.4.1.2.2) with direct_8x8_inference. Returns per-8x8 refs and mvs.
    fn direct_spatial(&self, mx: usize, my: usize) -> ([[i8; 4]; 2], [[Mv; 4]; 2]) {
        let cur = MbInfo::default();
        let mut refs = [-1i8; 2];
        let mut mvp = [Mv::ZERO; 2];
        for l in 0..2 {
            let a = self.nb_part(&cur, 0, mx, my, -1, 0, l).map_or(-1, |v| v.0);
            let b = self.nb_part(&cur, 0, mx, my, 0, -1, l).map_or(-1, |v| v.0);
            let c = self.nb_part(&cur, 0, mx, my, 4, -1, l).or_else(|| self.nb_part(&cur, 0, mx, my, -1, -1, l)).map_or(-1, |v| v.0);
            let minp = |x: i8, y: i8| if x >= 0 && y >= 0 { x.min(y) } else { x.max(y) };
            refs[l] = minp(a, minp(b, c));
        }
        let zero = refs[0] < 0 && refs[1] < 0;
        if zero {
            refs = [0, 0];
        } else {
            for l in 0..2 {
                if refs[l] >= 0 {
                    mvp[l] = self.mvp(&cur, 0, mx, my, 0, 0, 4, l, refs[l], Shape::Normal);
                }
            }
        }
        let Some(l1) = self.f.l1 else { return ([[0; 4]; 2], [[Mv::ZERO; 4]; 2]) };
        let col = &l1.mbs[my * self.f.p.mbw + mx];
        let mut out_ref = [[-1i8; 4]; 2];
        let mut out_mv = [[Mv::ZERO; 4]; 2];
        for q in 0..4 {
            let corner = [0usize, 3, 12, 15][q];
            let col_zero = if col.kind.is_intra() {
                false
            } else {
                let (rc, mvc) = if col.ref_idx[0][q] >= 0 { (col.ref_idx[0][q], col.mv[0][corner]) } else { (col.ref_idx[1][q], col.mv[1][corner]) };
                rc == 0 && mvc.x.abs() <= 1 && mvc.y.abs() <= 1
            };
            for l in 0..2 {
                out_ref[l][q] = refs[l];
                if refs[l] >= 0 {
                    out_mv[l][q] = if zero || (refs[l] == 0 && col_zero) { Mv::ZERO } else { mvp[l] };
                }
            }
        }
        (out_ref, out_mv)
    }

    /// B 16x8 / 8x16 candidate: search each partition in both lists, choose L0/L1/Bi per partition, then derive
    /// the motion vector predictors in decoding order from the chosen directions. With `beat`, a
    /// candidate whose cost does not come below it is `None` before its residual is coded.
    fn b_partition(&self, mx: usize, my: usize, part: Part, src: &Src, qp: u8, lam: u32, seeds: [Mv; 2], beat: Option<u64>) -> Option<Box<Cand>> {
        let f = self.f;
        let rp = [f.l0?, f.l1?];
        let (px, py) = (mx * 16, my * 16);
        let mut mvs = [[Mv::ZERO; 4]; 2];
        for l in 0..2 {
            mvs[l] = self.search_part(mx, my, rp[l], part, l, src, lam, seeds[l]).0;
        }
        let mut preds = [([0u8; 256], [0u8; 64], [0u8; 64]), ([0u8; 256], [0u8; 64], [0u8; 64])];
        for l in 0..2 {
            let (a, b, c) = &mut preds[l];
            for pi in 0..2 {
                self.pred_block(rp[l], px, py, part.rect(pi), mvs[l][pi], a, b, c);
            }
        }
        let mut bi = preds[0];
        avg_into(&mut bi.0, &preds[1].0);
        avg_into(&mut bi.1, &preds[1].1);
        avg_into(&mut bi.2, &preds[1].2);
        let srcs = [&preds[0], &preds[1], &bi];
        let (mut py_, mut pu, mut pv) = ([0u8; 256], [0u8; 64], [0u8; 64]);
        let mut dirs = [0u8; 4];
        let mut cost = ((lam * 8) >> 8) as u64;
        for pi in 0..2 {
            let (x, y, w, h) = part.rect(pi);
            let mut bestd = (u64::MAX, 0usize);
            for (d, s) in srcs.iter().enumerate() {
                let bits = if d == 2 { 14 } else { 7 };
                let c = satd(&src.y[y * 16 + x..], 16, &s.0[y * 16 + x..], 16, w, h) as u64 + ((lam * bits) >> 8) as u64;
                if c < bestd.0 {
                    bestd = (c, d);
                }
            }
            cost += bestd.0;
            dirs[pi] = bestd.1 as u8;
            if beat.is_some_and(|b| cost >= b) {
                return None;
            }
            let s = srcs[bestd.1];
            for r in y..y + h {
                py_[r * 16 + x..r * 16 + x + w].copy_from_slice(&s.0[r * 16 + x..r * 16 + x + w]);
            }
            for r in y / 2..(y + h) / 2 {
                let o = r * 8 + x / 2;
                pu[o..o + w / 2].copy_from_slice(&s.1[o..o + w / 2]);
                pv[o..o + w / 2].copy_from_slice(&s.2[o..o + w / 2]);
            }
        }
        // predictors in decoding order with the actual per-list references
        let mut info = MbInfo::default();
        let mut refs = [[-1i8; 4]; 2];
        let mut fmvs = [[Mv::ZERO; 4]; 2];
        let mut mvds = [[Mv::ZERO; 4]; 2];
        for pi in 0..2 {
            let (x, y, w, h) = part.rect(pi);
            let mask = Self::mask_of(part, pi);
            for l in 0..2 {
                if dirs[pi] == 2 || dirs[pi] as usize == l {
                    let mvp = self.mvp(&info, mask, mx, my, (x / 4) as i32, (y / 4) as i32, (w / 4) as i32, l, 0, Self::shape_of(part, pi));
                    let mv = mvs[l][pi];
                    mvds[l][pi] = Mv::new(mv.x as i32 - mvp.x as i32, mv.y as i32 - mvp.y as i32);
                    refs[l][pi] = 0;
                    fmvs[l][pi] = mv;
                }
            }
            for l in 0..2 {
                for by in y / 4..(y + h) / 4 {
                    for bx in x / 4..(x + w) / 4 {
                        info.mv[l][by * 4 + bx] = fmvs[l][pi];
                    }
                }
                for by8 in y / 8..(y + h).div_ceil(8) {
                    for bx8 in x / 8..(x + w).div_ceil(8) {
                        info.ref_idx[l][by8 * 2 + bx8] = refs[l][pi];
                    }
                }
            }
        }
        let mut c = Cand::new();
        c.code.kind = MbKind::BInter;
        c.code.part = part;
        c.code.bdir = dirs;
        c.code.mvd = mvds;
        Self::cand_inter_info(&mut c, part, refs, fmvs, mvds);
        self.code_inter(src, &py_, &pu, &pv, qp, &mut c);
        c.cost = cost;
        Some(c)
    }

    pub(super) fn analyse_b(&self, mx: usize, my: usize, qp: u8, av: Avail, e: &MbEdges, src: &Src) -> Box<Cand> {
        let f = self.f;
        // B slices always have both references; without them the macroblock is coded intra.
        let (Some(r0), Some(r1)) = (f.l0, f.l1) else { return self.analyse_intra(mx, my, qp, av, e, src, u64::MAX) };
        let lam = f.lam.sad[qp as usize];
        let (px, py) = (mx * 16, my * 16);
        // --- direct
        let (dref, dmv) = self.direct_spatial(mx, my);
        let mut direct_ok = true;
        for q in 0..4 {
            let rect = Part::P8x8.rect(q);
            for l in 0..2 {
                if dref[l][q] >= 0 && !self.mv_ok(px + rect.0, py + rect.1, 8, 8, dmv[l][q]) {
                    direct_ok = false;
                }
            }
        }
        let mut best: Option<Box<Cand>> = None;
        if direct_ok {
            let mut y = [0u8; 256];
            let mut u = [0u8; 64];
            let mut v = [0u8; 64];
            let mut ty = [0u8; 256];
            let mut tu = [0u8; 64];
            let mut tv = [0u8; 64];
            for q in 0..4 {
                let rect = Part::P8x8.rect(q);
                let (u0, u1) = (dref[0][q] >= 0, dref[1][q] >= 0);
                if u0 {
                    self.pred_block(r0, px, py, rect, dmv[0][q], &mut y, &mut u, &mut v);
                }
                if u1 {
                    if u0 {
                        self.pred_block(r1, px, py, rect, dmv[1][q], &mut ty, &mut tu, &mut tv);
                        let (x, yy, w, h) = rect;
                        for rr in 0..h {
                            avg_into(&mut y[(yy + rr) * 16 + x..(yy + rr) * 16 + x + w], &ty[(yy + rr) * 16 + x..(yy + rr) * 16 + x + w]);
                        }
                        for rr in 0..h / 2 {
                            let o = (yy / 2 + rr) * 8 + x / 2;
                            avg_into(&mut u[o..o + w / 2], &tu[o..o + w / 2]);
                            avg_into(&mut v[o..o + w / 2], &tv[o..o + w / 2]);
                        }
                    } else {
                        self.pred_block(r1, px, py, rect, dmv[1][q], &mut y, &mut u, &mut v);
                    }
                }
            }
            let mut c = Cand::new();
            c.code.kind = MbKind::BDirect;
            c.code.part = Part::P8x8;
            // store per-8x8 refs/mvs
            let refs = [[dref[0][0], dref[0][1], dref[0][2], dref[0][3]], [dref[1][0], dref[1][1], dref[1][2], dref[1][3]]];
            Self::cand_inter_info(&mut c, Part::P8x8, refs, dmv, [[Mv::ZERO; 4]; 2]);
            c.code.part = Part::P16x16;
            c.cost = satd(&src.y, 16, &y, 16, 16, 16) as u64 + ((lam * 2) >> 8) as u64;
            self.code_inter(src, &y, &u, &v, qp, &mut c);
            if c.code.cbp == 0 {
                c.code.kind = MbKind::BSkip;
                c.info.t8x8 = false;
                // a skip is cheap; accept immediately when the prediction is good
                let thresh = (f.qt.v4[(qp % 6) as usize][0] << (qp / 6)) as u64 * 24;
                if sad16(&src.y, 16, &y, 16, 16) as u64 <= thresh {
                    return c;
                }
            }
            best = Some(c);
        }
        // --- L0 / L1 16x16
        let mut res = [(Mv::ZERO, Mv::ZERO, u32::MAX); 2];
        for (l, rp) in [(0usize, r0), (1usize, r1)] {
            let (m, p, c) = self.search_part(mx, my, rp, Part::P16x16, l, src, lam, if l == 0 { dmv[0][0] } else { dmv[1][0] });
            res[l] = (m[0], p[0], c);
        }
        let mut preds = [([0u8; 256], [0u8; 64], [0u8; 64]), ([0u8; 256], [0u8; 64], [0u8; 64])];
        for (l, rp) in [(0usize, r0), (1usize, r1)] {
            let (a, b, c) = &mut preds[l];
            self.pred_block(rp, px, py, (0, 0, 16, 16), res[l].0, a, b, c);
        }
        let mut bi = preds[0];
        avg_into(&mut bi.0, &preds[1].0);
        avg_into(&mut bi.1, &preds[1].1);
        avg_into(&mut bi.2, &preds[1].2);
        let bits = |l: usize| mv_bits(res[l].0, res[l].1);
        let costs = [
            res[0].2 as u64 + ((lam * 3) >> 8) as u64,
            res[1].2 as u64 + ((lam * 3) >> 8) as u64,
            satd(&src.y, 16, &bi.0, 16, 16, 16) as u64 + ((lam * (bits(0) + bits(1) + 5)) >> 8) as u64,
        ];
        let dir = (0..3).min_by_key(|&d| costs[d]).unwrap_or(0);
        let mut ci = Cand::new();
        ci.code.kind = MbKind::BInter;
        ci.code.part = Part::P16x16;
        ci.code.bdir[0] = dir as u8;
        let mut refs = [[-1i8; 4]; 2];
        let mut mvs = [[Mv::ZERO; 4]; 2];
        let mut mvds = [[Mv::ZERO; 4]; 2];
        for l in 0..2 {
            if dir == 2 || dir == l {
                refs[l] = [0; 4];
                mvs[l] = [res[l].0; 4];
                mvds[l][0] = Mv::new(res[l].0.x as i32 - res[l].1.x as i32, res[l].0.y as i32 - res[l].1.y as i32);
            }
        }
        ci.code.mvd = mvds;
        Self::cand_inter_info(&mut ci, Part::P16x16, refs, mvs, mvds);
        let (py_, pu, pv) = if dir == 2 { bi } else { preds[dir] };
        ci.cost = costs[dir];
        let better_than_direct = best.as_ref().is_none_or(|b| ci.cost < b.cost);
        // Without RD only costs decide from here on: the 16x16 candidate's residual is coded at
        // the end, if it is still the best then.
        let mut deferred = false;
        if better_than_direct || f.p.rd {
            if f.p.rd {
                self.code_inter(src, &py_, &pu, &pv, qp, &mut ci);
            } else {
                deferred = true;
            }
            best = Some(match best {
                None => ci,
                Some(b) => {
                    if f.p.rd {
                        if self.rd_cost(mx, my, &ci, src, qp) < self.rd_cost(mx, my, &b, src, qp) { ci } else { b }
                    } else {
                        ci
                    }
                }
            });
        }
        let mut best = best.unwrap_or_else(|| self.analyse_intra(mx, my, qp, av, e, src, u64::MAX));
        // --- 16x8 / 8x16 partitions with a per-partition prediction direction
        if f.p.partitions && best.cost > 256 * 4 {
            for part in [Part::P16x8, Part::P8x16] {
                // without RD only the cost decides, so a losing candidate's residual is not coded
                let beat = (!f.p.rd).then_some(best.cost);
                if let Some(c) = self.b_partition(mx, my, part, src, qp, lam, [res[0].0, res[1].0], beat) {
                    let better = if f.p.rd { self.rd_cost(mx, my, &c, src, qp) < self.rd_cost(mx, my, &best, src, qp) } else { c.cost < best.cost };
                    if better {
                        best = c;
                        deferred = false;
                    }
                }
            }
        }
        let need_intra = f.p.always_intra || best.cost > (256 * 3 + 16 * lam as u64 / 256) * 3;
        if need_intra {
            let ic = self.analyse_intra(mx, my, qp, av, e, src, best.cost);
            if f.p.rd {
                if self.rd_cost(mx, my, &ic, src, qp) < self.rd_cost(mx, my, &best, src, qp) {
                    return ic;
                }
            } else if ic.cost < best.cost {
                return ic;
            }
        }
        if deferred {
            self.code_inter(src, &py_, &pu, &pv, qp, &mut best);
        }
        best
    }
}

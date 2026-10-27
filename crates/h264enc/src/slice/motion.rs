use super::*;

impl<'a, 'b> SliceEnc<'a, 'b> {
    // ------------------------------------------------------------------ motion estimation

    /// Integer + sub-pel search for a w x h block at picture (bx, by). Returns (mv, cost, satd).
    fn search(&self, r: &RefPic, bx: usize, by: usize, w: usize, h: usize, src: &[u8], sstride: usize, mvp: Mv, cands: &[Mv], lam: u32) -> (Mv, u32) {
        let pl = &r.frame.y;
        let p = self.f.p;
        let fcost = |mv: Mv| -> u32 {
            let i = pl.idx(bx as isize + (mv.x as isize >> 2), by as isize + (mv.y as isize >> 2));
            let d = if w == 16 { sad16(src, sstride, &pl.data[i..], pl.stride, h) } else { sad(src, sstride, &pl.data[i..], pl.stride, w, h) };
            d + ((lam * mv_bits(mv, mvp)) >> 8)
        };
        let mut best = Mv::new((mvp.x as i32 + 2) & !3, (mvp.y as i32 + 2) & !3);
        if !self.mv_ok(bx, by, w, h, best) {
            best = Mv::ZERO;
        }
        let mut bcost = fcost(best);
        for &c in cands {
            let c = Mv::new((c.x as i32 + 2) & !3, (c.y as i32 + 2) & !3);
            if c != best && self.mv_ok(bx, by, w, h, c) {
                let k = fcost(c);
                if k < bcost {
                    bcost = k;
                    best = c;
                }
            }
        }
        // hexagon search (full-pel units = 4 quarter samples)
        let range = p.me_range;
        let centre0 = best;
        const HEX: [(i32, i32); 6] = [(-2, 0), (2, 0), (-1, -2), (1, -2), (-1, 2), (1, 2)];
        for _ in 0..range {
            let mut moved = false;
            let c = best;
            for &(dx, dy) in &HEX {
                let m = Mv::new(c.x as i32 + dx * 4, c.y as i32 + dy * 4);
                if ((m.x as i32 - centre0.x as i32).abs() >> 2) > range || ((m.y as i32 - centre0.y as i32).abs() >> 2) > range || !self.mv_ok(bx, by, w, h, m)
                {
                    continue;
                }
                let k = fcost(m);
                if k < bcost {
                    bcost = k;
                    best = m;
                    moved = true;
                }
            }
            if !moved {
                break;
            }
        }
        // small diamond / square refinement
        for _ in 0..2 {
            let c = best;
            for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                let m = Mv::new(c.x as i32 + dx * 4, c.y as i32 + dy * 4);
                if !self.mv_ok(bx, by, w, h, m) {
                    continue;
                }
                let k = fcost(m);
                if k < bcost {
                    bcost = k;
                    best = m;
                }
            }
            if best == c {
                break;
            }
        }
        // sub-pel refinement with SATD
        let mut buf = [0u8; 256];
        let scost = |mv: Mv, buf: &mut [u8; 256]| -> u32 {
            r.mc_luma(bx, by, mv, w, h, buf, 16);
            satd(src, sstride, buf, 16, w, h) + ((lam * mv_bits(mv, mvp)) >> 8)
        };
        let mut bs = scost(best, &mut buf);
        if p.subpel >= 1 {
            for step in [2, 1] {
                if step == 1 && p.subpel < 2 {
                    break;
                }
                let rounds = if step == 1 { 1 + p.subpel_rounds as usize } else { 1 };
                for _ in 0..rounds {
                    let c = best;
                    for (dx, dy) in [(-1, 0), (1, 0), (0, -1), (0, 1), (-1, -1), (1, -1), (-1, 1), (1, 1)] {
                        let m = Mv::new(c.x as i32 + dx * step, c.y as i32 + dy * step);
                        if !self.mv_ok(bx, by, w, h, m) {
                            continue;
                        }
                        let k = scost(m, &mut buf);
                        if k < bs {
                            bs = k;
                            best = m;
                        }
                    }
                    if best == c {
                        break;
                    }
                }
            }
        }
        (best, bs)
    }

    pub(super) fn pred_block(
        &self,
        r: &RefPic,
        px: usize,
        py: usize,
        rect: (usize, usize, usize, usize),
        mv: Mv,
        y: &mut [u8; 256],
        u: &mut [u8; 64],
        v: &mut [u8; 64],
    ) {
        let (x, yy, w, h) = rect;
        r.mc_luma(px + x, py + yy, mv, w, h, &mut y[yy * 16 + x..], 16);
        let co = (yy / 2) * 8 + x / 2;
        r.mc_chroma((px + x) / 2, (py + yy) / 2, mv, w / 2, h / 2, &mut u[co..], &mut v[co..], 8);
    }

    pub(super) fn cand_inter_info(c: &mut Cand, part: Part, refs: [[i8; 4]; 2], mvs: [[Mv; 4]; 2], mvds: [[Mv; 4]; 2]) {
        for p in 0..part.count() {
            let (x, y, w, h) = part.rect(p);
            for by in y / 4..(y + h) / 4 {
                for bx in x / 4..(x + w) / 4 {
                    let r = by * 4 + bx;
                    for l in 0..2 {
                        c.info.mv[l][r] = if refs[l][p] >= 0 { mvs[l][p] } else { Mv::ZERO };
                        let d = if refs[l][p] >= 0 { mvds[l][p] } else { Mv::ZERO };
                        c.info.mvd[l][r] = [d.x.unsigned_abs().min(127) as u8, d.y.unsigned_abs().min(127) as u8];
                    }
                }
            }
            for by8 in y / 8..(y + h).div_ceil(8) {
                for bx8 in x / 8..(x + w).div_ceil(8) {
                    for l in 0..2 {
                        c.info.ref_idx[l][by8 * 2 + bx8] = refs[l][p];
                    }
                }
            }
        }
    }

    pub(super) fn mask_of(part: Part, upto: usize) -> u16 {
        let mut m = 0u16;
        for p in 0..upto {
            let (x, y, w, h) = part.rect(p);
            for by in y / 4..(y + h) / 4 {
                for bx in x / 4..(x + w) / 4 {
                    m |= 1 << (by * 4 + bx);
                }
            }
        }
        m
    }

    pub(super) fn shape_of(part: Part, p: usize) -> Shape {
        match (part, p) {
            (Part::P16x8, 0) => Shape::Top16x8,
            (Part::P16x8, _) => Shape::Bottom16x8,
            (Part::P8x16, 0) => Shape::Left8x16,
            (Part::P8x16, _) => Shape::Right8x16,
            _ => Shape::Normal,
        }
    }

    /// Search each partition of `part` for list `list` sequentially (mvp depends on earlier partitions).
    pub(super) fn search_part(&self, mx: usize, my: usize, r: &RefPic, part: Part, list: usize, src: &Src, lam: u32, seed: Mv) -> ([Mv; 4], [Mv; 4], u32) {
        let (px, py) = (mx * 16, my * 16);
        let mut info = MbInfo::default();
        let mut mvs = [Mv::ZERO; 4];
        let mut mvps = [Mv::ZERO; 4];
        let mut total = 0;
        for p in 0..part.count() {
            let (x, y, w, h) = part.rect(p);
            let mask = Self::mask_of(part, p);
            let mvp = self.mvp(&info, mask, mx, my, (x / 4) as i32, (y / 4) as i32, (w / 4) as i32, list, 0, Self::shape_of(part, p));
            let (mv, cost) = self.search(r, px + x, py + y, w, h, &src.y[y * 16 + x..], 16, mvp, &[seed, mvs[0]], lam);
            mvs[p] = mv;
            mvps[p] = mvp;
            total += cost;
            // record in scratch info for the next partition's predictor
            for by in y / 4..(y + h) / 4 {
                for bx in x / 4..(x + w) / 4 {
                    info.mv[list][by * 4 + bx] = mv;
                }
            }
            for by8 in y / 8..(y + h).div_ceil(8) {
                for bx8 in x / 8..(x + w).div_ceil(8) {
                    info.ref_idx[list][by8 * 2 + bx8] = 0;
                }
            }
        }
        (mvs, mvps, total)
    }

    pub(super) fn analyse_p(&self, mx: usize, my: usize, qp: u8, av: Avail, e: &MbEdges, src: &Src) -> Box<Cand> {
        let f = self.f;
        // P slices always have a reference; without one the macroblock is coded intra.
        let Some(r) = f.l0 else { return self.analyse_intra(mx, my, qp, av, e, src, u64::MAX) };
        let lam = f.lam.sad[qp as usize];
        let (px, py) = (mx * 16, my * 16);
        let skip_mv = self.pskip_mv(mx, my);
        let skip_ok = self.mv_ok(px, py, 16, 16, skip_mv);
        // --- try P_Skip early
        let mut skip_pred = None;
        if skip_ok {
            let mut y = [0u8; 256];
            let mut u = [0u8; 64];
            let mut v = [0u8; 64];
            self.pred_block(r, px, py, (0, 0, 16, 16), skip_mv, &mut y, &mut u, &mut v);
            let s = sad16(&src.y, 16, &y, 16, 16);
            let qstep_thresh = (f.qt.v4[(qp % 6) as usize][0] << (qp / 6)) as u32 * 16;
            if s < qstep_thresh * 2 {
                let mut c = Cand::new();
                c.code.kind = MbKind::PInter;
                c.code.part = Part::P16x16;
                self.code_inter(src, &y, &u, &v, qp, &mut c);
                if c.code.cbp == 0 {
                    c.code.kind = MbKind::PSkip;
                    Self::cand_inter_info(&mut c, Part::P16x16, [[0; 4], [-1; 4]], [[skip_mv; 4], [Mv::ZERO; 4]], [[Mv::ZERO; 4]; 2]);
                    c.info.nnz = [0; 16];
                    c.info.nnz_c = [0; 8];
                    c.info.dc_cbf = 0;
                    c.info.t8x8 = false;
                    c.ry = y;
                    c.ru = u;
                    c.rv = v;
                    return c;
                }
            }
            skip_pred = Some((y, u, v));
        }
        // --- 16x16 search
        let mut cands = vec![skip_mv, Mv::ZERO];
        let colo = r.mbs[my * f.p.mbw + mx];
        if colo.ref_idx[0][0] >= 0 {
            cands.push(colo.mv[0][0]);
        }
        let (mv16, mvp16, cost16) = {
            let (m, p, c) = self.search_part(mx, my, r, Part::P16x16, 0, src, lam, skip_mv);
            (m, p, c)
        };
        let _ = cands;
        let mut best_part = Part::P16x16;
        let mut best_mvs = mv16;
        let mut best_mvps = mvp16;
        let mut best_cost = cost16 as u64 + (lam >> 8) as u64;
        if f.p.partitions {
            let big = cost16 as u64 > (256 * 4) as u64;
            if big {
                for part in [Part::P16x8, Part::P8x16, Part::P8x8] {
                    let (m, p, c) = self.search_part(mx, my, r, part, 0, src, lam, mv16[0]);
                    let extra = match part {
                        Part::P8x8 => 10,
                        _ => 3,
                    };
                    let c = c as u64 + ((lam * extra) >> 8) as u64;
                    if c < best_cost {
                        best_cost = c;
                        best_part = part;
                        best_mvs = m;
                        best_mvps = p;
                    }
                }
            }
        }
        // --- build inter candidate
        let mut ci = Cand::new();
        ci.code.kind = MbKind::PInter;
        ci.code.part = best_part;
        let mut py_ = [0u8; 256];
        let mut pu = [0u8; 64];
        let mut pv = [0u8; 64];
        if let (Part::P16x16, true, Some((a, b, c))) = (best_part, best_mvs[0] == skip_mv, skip_pred) {
            py_ = a;
            pu = b;
            pv = c;
        } else {
            for p in 0..best_part.count() {
                self.pred_block(r, px, py, best_part.rect(p), best_mvs[p], &mut py_, &mut pu, &mut pv);
            }
        }
        let mut mvds = [[Mv::ZERO; 4]; 2];
        for p in 0..best_part.count() {
            mvds[0][p] = Mv::new(best_mvs[p].x as i32 - best_mvps[p].x as i32, best_mvs[p].y as i32 - best_mvps[p].y as i32);
        }
        ci.code.mvd = mvds;
        Self::cand_inter_info(&mut ci, best_part, [[0; 4], [-1; 4]], [best_mvs, [Mv::ZERO; 4]], mvds);
        self.code_inter(src, &py_, &pu, &pv, qp, &mut ci);
        ci.cost = best_cost;
        if ci.code.cbp == 0 && best_part == Part::P16x16 && best_mvs[0] == skip_mv && skip_ok {
            ci.code.kind = MbKind::PSkip;
            ci.info.t8x8 = false;
            ci.info.mvd = [[[0; 2]; 16]; 2];
            return ci;
        }
        // --- intra
        let need_intra = f.p.always_intra || best_cost > (256 * 3 + 16 * lam as u64 / 256) * 2;
        if need_intra {
            let ic = self.analyse_intra(mx, my, qp, av, e, src, best_cost);
            if f.p.rd {
                if self.rd_cost(mx, my, &ic, src, qp) < self.rd_cost(mx, my, &ci, src, qp) {
                    return ic;
                }
            } else if ic.cost < best_cost {
                return ic;
            }
        }
        if f.p.rd && skip_ok && ci.code.cbp != 0 {
            // RD check against P_Skip
            if let Some((y, u, v)) = skip_pred {
                let mut sc = Cand::new();
                sc.code.kind = MbKind::PSkip;
                Self::cand_inter_info(&mut sc, Part::P16x16, [[0; 4], [-1; 4]], [[skip_mv; 4], [Mv::ZERO; 4]], [[Mv::ZERO; 4]; 2]);
                sc.ry = y;
                sc.ru = u;
                sc.rv = v;
                if self.rd_cost(mx, my, &sc, src, qp) < self.rd_cost(mx, my, &ci, src, qp) {
                    return sc;
                }
            }
        }
        ci
    }
}

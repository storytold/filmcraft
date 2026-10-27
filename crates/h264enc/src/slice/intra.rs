use super::*;

impl<'a, 'b> SliceEnc<'a, 'b> {
    // ------------------------------------------------------------------ intra

    fn pred_intra_mode(&self, mx: usize, my: usize, av: Avail, local: &[i8; 16], bx: usize, by: usize) -> u8 {
        let a = if bx > 0 {
            Some(local[by * 4 + bx - 1])
        } else if av.left {
            Some(self.mb(mx - 1, my).imodes[by * 4 + 3])
        } else {
            None
        };
        let b = if by > 0 {
            Some(local[(by - 1) * 4 + bx])
        } else if av.top {
            Some(self.mb(mx, my - 1).imodes[12 + bx])
        } else {
            None
        };
        match (a, b) {
            (Some(a), Some(b)) => {
                let a = if a < 0 { 2 } else { a };
                let b = if b < 0 { 2 } else { b };
                a.min(b) as u8
            }
            _ => 2,
        }
    }

    /// Luma I16x16 coding. Returns luma cbp (0 or 15).
    fn code_i16(&self, src: &[u8; 256], pred: &[u8; 256], qp: u8, c: &mut Cand) -> u8 {
        let qt = self.f.qt;
        let mut coefs = [[0i32; 16]; 16];
        let mut dc = [0i32; 16];
        for blk in 0..16 {
            let (bx, by) = BLK4_XY[blk];
            let mut d = [0i32; 16];
            for y in 0..4 {
                for x in 0..4 {
                    let i = (by * 4 + y) * 16 + bx * 4 + x;
                    d[y * 4 + x] = src[i] as i32 - pred[i] as i32;
                }
            }
            let mut t = fdct4(&d);
            dc[by * 4 + bx] = t[0];
            quant4(&mut t, qt, qp, Deadzone::INTRA, true);
            if self.f.p.cavlc_clamp {
                for v in t.iter_mut() {
                    *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
                }
            }
            coefs[blk] = t;
        }
        let (mut dcl, _) = quant_luma_dc(&dc, qt, qp, Deadzone::INTRA);
        if self.f.p.cavlc_clamp {
            for v in dcl.iter_mut() {
                *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
            }
        }
        for k in 0..16 {
            c.code.luma_dc[k] = dcl[ZIGZAG4[k]] as i16;
        }
        let ac_any = coefs.iter().any(|t| t[1..].iter().any(|&v| v != 0));
        let dcq = dequant_luma_dc(&dcl, qt, qp);
        c.info.dc_cbf = (c.info.dc_cbf & !1) | dcl.iter().any(|&v| v != 0) as u8;
        for blk in 0..16 {
            let (bx, by) = BLK4_XY[blk];
            let mut t = coefs[blk];
            let mut scan = [0i16; 16];
            for k in 1..16 {
                scan[k] = t[ZIGZAG4[k]] as i16;
            }
            c.code.luma4[blk] = if ac_any { scan } else { [0; 16] };
            let nz = if ac_any { scan.iter().filter(|&&v| v != 0).count() as u8 } else { 0 };
            c.info.nnz[by * 4 + bx] = nz;
            let off = by * 4 * 16 + bx * 4;
            if nz == 0 {
                let dcv = (dcq[by * 4 + bx] + 32) >> 6;
                for y in 0..4 {
                    for x in 0..4 {
                        let i = off + y * 16 + x;
                        c.ry[i] = (pred[i] as i32 + dcv).clamp(0, 255) as u8;
                    }
                }
            } else {
                dequant4(&mut t, qt, qp, true);
                t[0] = dcq[by * 4 + bx];
                let r = idct4(&t);
                for y in 0..4 {
                    for x in 0..4 {
                        let i = off + y * 16 + x;
                        c.ry[i] = (pred[i] as i32 + r[y * 4 + x]).clamp(0, 255) as u8;
                    }
                }
            }
        }
        if ac_any { 15 } else { 0 }
    }

    fn best_chroma_mode(&self, e: &MbEdges, av: Avail, src: &Src, lam: u32) -> (u8, [u8; 64], [u8; 64]) {
        let mut best = (u32::MAX, 0u8, [0u8; 64], [0u8; 64]);
        for mode in 0..4u8 {
            let ok = match mode {
                0 => true,
                1 => av.left,
                2 => av.top,
                _ => av.left && av.top && av.tl,
            };
            if !ok {
                continue;
            }
            let mut pu = [0u8; 64];
            let mut pv = [0u8; 64];
            pred_chroma(mode, &e.ctop[0], &e.cleft[0], e.ctl[0], av.top, av.left, &mut pu);
            pred_chroma(mode, &e.ctop[1], &e.cleft[1], e.ctl[1], av.top, av.left, &mut pv);
            let cost = satd(&src.u, 8, &pu, 8, 8, 8) + satd(&src.v, 8, &pv, 8, 8, 8) + ((lam * (1 + 2 * mode as u32)) >> 8);
            if cost < best.0 {
                best = (cost, mode, pu, pv);
            }
        }
        (best.1, best.2, best.3)
    }

    fn edge4(&self, e: &MbEdges, av: Avail, rec: &[u8; 256], bx: usize, by: usize) -> Edge4 {
        let mut top = [0u8; 8];
        let mut left = [0u8; 4];
        let has_top = by > 0 || av.top;
        let has_left = bx > 0 || av.left;
        let has_tl = if bx > 0 && by > 0 {
            true
        } else if by == 0 && bx > 0 {
            av.top
        } else if bx == 0 && by > 0 {
            av.left
        } else {
            av.tl
        };
        let tr_avail =
            if by == 0 { if bx < 3 { av.top } else { av.tr } } else { bx < 3 && RASTER_TO_BLK4[(by - 1) * 4 + bx + 1] < RASTER_TO_BLK4[by * 4 + bx] };
        if has_top {
            for i in 0..4 {
                top[i] = if by > 0 { rec[(by * 4 - 1) * 16 + bx * 4 + i] } else { e.top[bx * 4 + i] };
            }
            for i in 4..8 {
                top[i] = if tr_avail { if by > 0 { rec[(by * 4 - 1) * 16 + bx * 4 + i] } else { e.top[bx * 4 + i] } } else { top[3] };
            }
        }
        if has_left {
            for i in 0..4 {
                left[i] = if bx > 0 { rec[(by * 4 + i) * 16 + bx * 4 - 1] } else { e.left[by * 4 + i] };
            }
        }
        let tl = if !has_tl {
            0
        } else if bx > 0 && by > 0 {
            rec[(by * 4 - 1) * 16 + bx * 4 - 1]
        } else if by == 0 && bx > 0 {
            e.top[bx * 4 - 1]
        } else if bx == 0 && by > 0 {
            e.left[by * 4 - 1]
        } else {
            e.tl
        };
        Edge4 { top, left, tl, has_top, has_left, has_tl }
    }

    fn edge8(&self, e: &MbEdges, av: Avail, rec: &[u8; 256], b8: usize) -> Edge8 {
        let (x8, y8) = (b8 & 1, b8 >> 1);
        let mut top = [0u8; 16];
        let mut left = [0u8; 8];
        let has_top = y8 > 0 || av.top;
        let has_left = x8 > 0 || av.left;
        let has_tl = match b8 {
            0 => av.tl,
            1 => av.top,
            2 => av.left,
            _ => true,
        };
        let tr_avail = match b8 {
            0 => av.top,
            1 => av.tr,
            2 => true,
            _ => false,
        };
        if has_top {
            for i in 0..8 {
                top[i] = if y8 > 0 { rec[7 * 16 + x8 * 8 + i] } else { e.top[x8 * 8 + i] };
            }
            for i in 8..16 {
                top[i] = if tr_avail { if y8 > 0 { rec[7 * 16 + x8 * 8 + i] } else { e.top[x8 * 8 + i] } } else { top[7] };
            }
        }
        if has_left {
            for i in 0..8 {
                left[i] = if x8 > 0 { rec[(y8 * 8 + i) * 16 + 7] } else { e.left[y8 * 8 + i] };
            }
        }
        let tl = if !has_tl {
            0
        } else {
            match b8 {
                0 => e.tl,
                1 => e.top[7],
                2 => e.left[7],
                _ => rec[7 * 16 + 7],
            }
        };
        Edge8 { top, left, tl, has_top, has_left, has_tl }
    }

    /// Intra 4x4 analysis + coding. Returns cost (SATD domain) or None if it exceeded `limit`.
    fn intra4x4(&self, mx: usize, my: usize, qp: u8, av: Avail, e: &MbEdges, src: &Src, c: &mut Cand, limit: u64) -> Option<u64> {
        let qt = self.f.qt;
        let lam = self.f.lam.sad[qp as usize];
        let mut modes = [-1i8; 16];
        let mut total = ((lam * 24) >> 8) as u64;
        for blk in 0..16 {
            let (bx, by) = BLK4_XY[blk];
            let ed = self.edge4(e, av, &c.ry, bx, by);
            let pm = self.pred_intra_mode(mx, my, av, &modes, bx, by);
            let mut srcb = [0u8; 16];
            for y in 0..4 {
                srcb[y * 4..y * 4 + 4].copy_from_slice(&src.y[(by * 4 + y) * 16 + bx * 4..(by * 4 + y) * 16 + bx * 4 + 4]);
            }
            let mut best = (u32::MAX, 2u8, [0u8; 16]);
            for mode in 0..9u8 {
                if !mode_available(mode, ed.has_top, ed.has_left, ed.has_tl) {
                    continue;
                }
                let mut p = [0u8; 16];
                pred4x4(mode, &ed, &mut p);
                let bits = if mode == pm { 1 } else { 4 };
                let cost = satd(&srcb, 4, &p, 4, 4, 4) + ((lam * bits) >> 8);
                if cost < best.0 {
                    best = (cost, mode, p);
                }
            }
            total += best.0 as u64;
            if total > limit {
                return None;
            }
            let (mode, p) = (best.1, best.2);
            modes[by * 4 + bx] = mode as i8;
            c.code.ipred[blk] = if mode == pm {
                -1
            } else if mode < pm {
                mode as i8
            } else {
                mode as i8 - 1
            };
            // code block
            let mut d = [0i32; 16];
            for i in 0..16 {
                d[i] = srcb[i] as i32 - p[i] as i32;
            }
            let mut t = fdct4(&d);
            let nz = quant4(&mut t, qt, qp, Deadzone::INTRA, false);
            if self.f.p.cavlc_clamp {
                for v in t.iter_mut() {
                    *v = (*v).clamp(-cavlc::MAX_LEVEL, cavlc::MAX_LEVEL);
                }
            }
            let mut scan = [0i16; 16];
            for k in 0..16 {
                scan[k] = t[ZIGZAG4[k]] as i16;
            }
            c.code.luma4[blk] = scan;
            c.info.nnz[by * 4 + bx] = nz as u8;
            let off = by * 4 * 16 + bx * 4;
            if nz == 0 {
                for y in 0..4 {
                    c.ry[off + y * 16..off + y * 16 + 4].copy_from_slice(&p[y * 4..y * 4 + 4]);
                }
            } else {
                dequant4(&mut t, qt, qp, false);
                let r = idct4(&t);
                for y in 0..4 {
                    for x in 0..4 {
                        c.ry[off + y * 16 + x] = (p[y * 4 + x] as i32 + r[y * 4 + x]).clamp(0, 255) as u8;
                    }
                }
            }
        }
        c.info.imodes = modes;
        let mut cbp = 0;
        for b8 in 0..4 {
            if (0..4).any(|i| c.code.luma4[b8 * 4 + i].iter().any(|&v| v != 0)) {
                cbp |= 1 << b8;
            }
        }
        c.code.cbp = cbp;
        c.code.kind = MbKind::I4x4;
        c.code.t8x8 = false;
        c.info.t8x8 = false;
        Some(total)
    }

    /// Intra 8x8 analysis + coding.
    fn intra8x8(&self, mx: usize, my: usize, qp: u8, av: Avail, e: &MbEdges, src: &Src, c: &mut Cand, limit: u64) -> Option<u64> {
        let qt = self.f.qt;
        let lam = self.f.lam.sad[qp as usize];
        let mut modes = [-1i8; 16];
        let mut total = ((lam * 12) >> 8) as u64;
        for b8 in 0..4 {
            let (x8, y8) = (b8 & 1, b8 >> 1);
            let ed = self.edge8(e, av, &c.ry, b8);
            let fe = filter8(&ed);
            let pm = self.pred_intra_mode(mx, my, av, &modes, x8 * 2, y8 * 2);
            let mut srcb = [0u8; 64];
            for y in 0..8 {
                srcb[y * 8..y * 8 + 8].copy_from_slice(&src.y[(y8 * 8 + y) * 16 + x8 * 8..(y8 * 8 + y) * 16 + x8 * 8 + 8]);
            }
            let mut best = (u32::MAX, 2u8, [0u8; 64]);
            for mode in 0..9u8 {
                if !mode_available(mode, ed.has_top, ed.has_left, ed.has_tl) {
                    continue;
                }
                let mut p = [0u8; 64];
                pred8x8(mode, &fe, &mut p);
                let bits = if mode == pm { 1 } else { 4 };
                let cost = satd(&srcb, 8, &p, 8, 8, 8) + ((lam * bits) >> 8);
                if cost < best.0 {
                    best = (cost, mode, p);
                }
            }
            total += best.0 as u64;
            if total > limit {
                return None;
            }
            let (mode, p) = (best.1, best.2);
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                modes[(y8 * 2 + dy) * 4 + x8 * 2 + dx] = mode as i8;
            }
            c.code.ipred[b8] = if mode == pm {
                -1
            } else if mode < pm {
                mode as i8
            } else {
                mode as i8 - 1
            };
            let mut d = [0i32; 64];
            for i in 0..64 {
                d[i] = srcb[i] as i32 - p[i] as i32;
            }
            let mut t = fdct8(&d);
            let nz = quant8(&mut t, qt, qp, Deadzone::INTRA);
            let mut scan = [0i16; 64];
            for k in 0..64 {
                scan[k] = t[ZIGZAG8[k]] as i16;
            }
            c.code.luma8[b8] = scan;
            for (dx, dy) in [(0, 0), (1, 0), (0, 1), (1, 1)] {
                c.info.nnz[(y8 * 2 + dy) * 4 + x8 * 2 + dx] = nz as u8;
            }
            let off = y8 * 8 * 16 + x8 * 8;
            if nz == 0 {
                for y in 0..8 {
                    c.ry[off + y * 16..off + y * 16 + 8].copy_from_slice(&p[y * 8..y * 8 + 8]);
                }
            } else {
                dequant8(&mut t, qt, qp);
                let r = idct8(&t);
                for y in 0..8 {
                    for x in 0..8 {
                        c.ry[off + y * 16 + x] = (p[y * 8 + x] as i32 + r[y * 8 + x]).clamp(0, 255) as u8;
                    }
                }
            }
        }
        c.info.imodes = modes;
        let mut cbp = 0;
        for b8 in 0..4 {
            if c.code.luma8[b8].iter().any(|&v| v != 0) {
                cbp |= 1 << b8;
            }
        }
        c.code.cbp = cbp;
        c.code.kind = MbKind::I8x8;
        c.code.t8x8 = true;
        c.info.t8x8 = true;
        Some(total)
    }

    /// Full intra analysis; returns the best coded intra candidate. `inter_cost` lets P/B skip expensive modes.
    pub(super) fn analyse_intra(&self, mx: usize, my: usize, qp: u8, av: Avail, e: &MbEdges, src: &Src, inter_cost: u64) -> Box<Cand> {
        let lam = self.f.lam.sad[qp as usize];
        let p = self.f.p;
        // I16x16 mode
        let mut top16 = [0u8; 16];
        top16.copy_from_slice(&e.top[..16]);
        let mut b16 = (u32::MAX, 2u8, [0u8; 256]);
        for mode in 0..4u8 {
            let ok = match mode {
                0 => av.top,
                1 => av.left,
                2 => true,
                _ => av.top && av.left && av.tl,
            };
            if !ok {
                continue;
            }
            let mut pr = [0u8; 256];
            pred16x16(mode, &top16, &e.left, e.tl, av.top, av.left, &mut pr);
            let cost = satd(&src.y, 16, &pr, 16, 16, 16);
            if cost < b16.0 {
                b16 = (cost, mode, pr);
            }
        }
        let cost16 = b16.0 as u64 + ((lam * 6) >> 8) as u64;
        let (cmode, pu, pv) = self.best_chroma_mode(e, av, src, lam);
        let mk16 = || {
            let mut c = Cand::new();
            c.code.kind = MbKind::I16x16;
            c.code.i16_mode = b16.1;
            c.code.chroma_mode = cmode;
            let cbpl = self.code_i16(&src.y, &b16.2, qp, &mut c);
            let cbpc = self.code_chroma(src, &pu, &pv, qp, true, &mut c);
            c.code.cbp = cbpl | (cbpc << 4);
            c.info.chroma_mode = cmode;
            c.info.imodes = [-1; 16];
            c.cost = cost16;
            c
        };
        let mut best: Option<Box<Cand>> = None;
        let try_nxn = p.i4x4 && (inter_cost == u64::MAX || cost16 < inter_cost + inter_cost / 4 || p.always_intra);
        if !try_nxn {
            return mk16();
        }
        let limit = cost16.min(inter_cost.saturating_add(inter_cost / 8));
        let mut limit = if p.rd { cost16 + cost16 / 4 } else { limit };
        if p.i8x8 && p.t8x8 {
            let mut c = Cand::new();
            if let Some(cost) = self.intra8x8(mx, my, qp, av, e, src, &mut c, limit) {
                c.cost = cost;
                limit = limit.min(cost);
                best = Some(c);
            }
        }
        {
            let mut c = Cand::new();
            if let Some(cost) = self.intra4x4(mx, my, qp, av, e, src, &mut c, limit) {
                c.cost = cost;
                if best.as_ref().is_none_or(|b| cost < b.cost) {
                    best = Some(c);
                }
            }
        }
        let finish = |mut c: Box<Cand>| {
            c.code.chroma_mode = cmode;
            c.info.chroma_mode = cmode;
            let cbpc = self.code_chroma(src, &pu, &pv, qp, true, &mut c);
            c.code.cbp = (c.code.cbp & 15) | (cbpc << 4);
            c
        };
        match best {
            None => mk16(),
            Some(c) => {
                let c = finish(c);
                if p.rd {
                    let c16 = mk16();
                    let (j1, j2) = (self.rd_cost(mx, my, &c, src, qp), self.rd_cost(mx, my, &c16, src, qp));
                    let mut w = if j1 <= j2 { c } else { c16 };
                    w.cost = w.cost.min(cost16);
                    w
                } else if c.cost < cost16 {
                    c
                } else {
                    mk16()
                }
            }
        }
    }
}

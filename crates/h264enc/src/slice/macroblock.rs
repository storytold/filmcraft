use super::{BitWriter, CabacEstimator, Cand, MbCtx, MbKind, NbCtx, SliceEnc, SliceType, Src, cabac_mb, cavlc, ssd};

impl<'a, 'b> SliceEnc<'a, 'b> {
    // ------------------------------------------------------------------ macroblock

    pub(super) fn encode_mb(&mut self, mx: usize, my: usize) {
        let f = self.f;
        let mbw = f.p.mbw;
        let aq = if f.aq.is_empty() { 0 } else { f.aq[my * mbw + mx] as i32 };
        let qp = (f.qp as i32 + aq).clamp(0, 51) as u8;
        let av = self.avail(mx, my);
        let (px, py) = (mx * 16, my * 16);
        let mut src_y = [0u8; 256];
        let mut src_u = [0u8; 64];
        let mut src_v = [0u8; 64];
        {
            let s = &f.src.y;
            for r in 0..16 {
                let i = s.idx(px as isize, (py + r) as isize);
                src_y[r * 16..r * 16 + 16].copy_from_slice(&s.data[i..i + 16]);
            }
            for r in 0..8 {
                let i = f.src.u.idx((px / 2) as isize, (py / 2 + r) as isize);
                src_u[r * 8..r * 8 + 8].copy_from_slice(&f.src.u.data[i..i + 8]);
                let i = f.src.v.idx((px / 2) as isize, (py / 2 + r) as isize);
                src_v[r * 8..r * 8 + 8].copy_from_slice(&f.src.v.data[i..i + 8]);
            }
        }
        let src = Src { y: src_y, u: src_u, v: src_v };
        let edges = self.edges(mx, my, av);
        let best = match f.st {
            SliceType::I => self.analyse_intra(&MbCtx { mx, my, qp, av, e: &edges, src: &src }, u64::MAX),
            SliceType::P => self.analyse_p(mx, my, qp, av, &edges, &src),
            SliceType::B => self.analyse_b(mx, my, qp, av, &edges, &src),
        };
        self.commit(mx, my, qp, *best);
    }

    fn commit(&mut self, mx: usize, my: usize, qp: u8, mut c: Cand) {
        let (px, py) = (mx * 16, my * 16);
        // reconstruction
        for r in 0..16 {
            let i = self.ry(px, py + r);
            self.band.y[i..i + 16].copy_from_slice(&c.ry[r * 16..r * 16 + 16]);
        }
        for r in 0..8 {
            let i = self.rc(px / 2, py / 2 + r);
            self.band.u[i..i + 8].copy_from_slice(&c.ru[r * 8..r * 8 + 8]);
            self.band.v[i..i + 8].copy_from_slice(&c.rv[r * 8..r * 8 + 8]);
        }
        let kind = c.code.kind;
        let has_dqp = !kind.is_skip() && (c.code.cbp != 0 || kind == MbKind::I16x16);
        let prev_dqp_nz = self.prev_dqp_nz;
        if has_dqp {
            let mut d = qp as i32 - self.last_qp as i32;
            if d < -26 {
                d += 52;
            } else if d > 25 {
                d -= 52;
            }
            c.code.qp_delta = d;
            self.prev_dqp_nz = d != 0;
            self.last_qp = qp;
        } else {
            c.code.qp_delta = 0;
            self.prev_dqp_nz = false;
        }
        c.info.qp = self.last_qp;
        c.info.slice = self.slice_no + 1;
        c.info.kind = kind;
        c.info.cbp = if kind.is_skip() { 0 } else { c.code.cbp };
        self.stats.mbs += 1;
        self.stats.qp_sum += c.info.qp as u64;
        if kind.is_intra() {
            self.stats.intra += 1;
        } else if kind.is_skip() {
            self.stats.skip += 1;
        } else {
            self.stats.inter += 1;
        }
        let idx = (my - self.y0) * self.f.p.mbw + mx;
        self.mbs[idx] = c.info;
        // entropy coding
        let av = self.avail(mx, my);
        let left = if av.left { Some(&self.mbs[idx - 1]) } else { None };
        let top = if av.top { Some(&self.mbs[idx - self.f.p.mbw]) } else { None };
        let nb = NbCtx { left, top, cur: &self.mbs[idx], slice_type: self.f.st, prev_dqp_nz, t8x8_mode: self.f.p.t8x8 };
        if let Some(cab) = self.cabac.as_mut() {
            if nb.slice_type != SliceType::I {
                cabac_mb::write_skip_flag(cab, &nb, kind.is_skip());
            }
            if !kind.is_skip() {
                cabac_mb::write_mb(cab, &nb, &c.code);
            }
        } else if let Some(w) = self.cavlc.as_mut() {
            if kind.is_skip() {
                self.skip_run += 1;
            } else {
                if nb.slice_type != SliceType::I {
                    w.write_ue(self.skip_run);
                    self.skip_run = 0;
                }
                cavlc::write_mb(w, &nb, &c.code);
            }
        }
    }

    /// Estimated bits (1/256 units) of coding candidate `c` at the current entropy state.
    fn est_bits(&self, mx: usize, my: usize, c: &Cand, qp: u8) -> u64 {
        let av = self.avail(mx, my);
        let idx = (my - self.y0) * self.f.p.mbw + mx;
        let left = if av.left { Some(&self.mbs[idx - 1]) } else { None };
        let top = if av.top { Some(&self.mbs[idx - self.f.p.mbw]) } else { None };
        let mut info = c.info;
        info.kind = c.code.kind;
        info.cbp = c.code.cbp;
        let mut code = c.code.clone();
        code.qp_delta = qp as i32 - self.last_qp as i32;
        let nb = NbCtx { left, top, cur: &info, slice_type: self.f.st, prev_dqp_nz: self.prev_dqp_nz, t8x8_mode: self.f.p.t8x8 };
        if let Some(cab) = self.cabac.as_ref() {
            let mut est = CabacEstimator::new(cab.ctx.clone(), self.f.cost_tab);
            if nb.slice_type != SliceType::I {
                cabac_mb::write_skip_flag(&mut est, &nb, code.kind.is_skip());
            }
            if !code.kind.is_skip() {
                cabac_mb::write_mb(&mut est, &nb, &code);
            }
            est.cost
        } else {
            if code.kind.is_skip() {
                return 256;
            }
            let mut w = BitWriter::new();
            if nb.slice_type != SliceType::I {
                w.write_ue(self.skip_run);
            }
            cavlc::write_mb(&mut w, &nb, &code);
            w.bit_len() as u64 * 256
        }
    }

    pub(super) fn rd_cost(&self, mx: usize, my: usize, c: &Cand, src: &Src, qp: u8) -> u64 {
        let d = ssd(&src.y, 16, &c.ry, 16, 16, 16) + ssd(&src.u, 8, &c.ru, 8, 8, 8) + ssd(&src.v, 8, &c.rv, 8, 8, 8);
        let bits = self.est_bits(mx, my, c, qp);
        let lam = self.f.lam.ssd[qp as usize];
        (d as f64 * 256.0 + lam * bits as f64) as u64
    }
}

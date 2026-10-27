use super::*;

impl<'a, S: Sample> SliceDecoder<'a, S> {
    pub(super) fn coding_quadtree(&mut self, x0: i32, y0: i32, log2: u32, depth: u32) -> Result<()> {
        let size = 1i32 << log2;
        let (w, h) = (self.pic.width as i32, self.pic.height as i32);
        let split = if x0 + size <= w && y0 + size <= h && log2 > self.sps.log2_min_cb {
            // ctxInc from neighbouring depths (9.3.4.2.2)
            let mut inc = 0;
            if self.pic.zavail(x0, y0, x0 - 1, y0, self.slice_addr) && self.pic.blk_at(x0 - 1, y0).depth as u32 > depth {
                inc += 1;
            }
            if self.pic.zavail(x0, y0, x0, y0 - 1, self.slice_addr) && self.pic.blk_at(x0, y0 - 1).depth as u32 > depth {
                inc += 1;
            }
            self.dd(SPLIT_CU + inc) == 1
        } else {
            log2 > self.sps.log2_min_cb
        };
        if self.pps.cu_qp_delta_enabled && log2 >= self.log2_min_cu_qp_delta {
            self.is_cu_qp_delta_coded = false;
            self.cu_qp_delta = 0;
        }
        if split {
            let half = size >> 1;
            let (x1, y1) = (x0 + half, y0 + half);
            self.coding_quadtree(x0, y0, log2 - 1, depth + 1)?;
            if x1 < w {
                self.coding_quadtree(x1, y0, log2 - 1, depth + 1)?;
            }
            if y1 < h {
                self.coding_quadtree(x0, y1, log2 - 1, depth + 1)?;
            }
            if x1 < w && y1 < h {
                self.coding_quadtree(x1, y1, log2 - 1, depth + 1)?;
            }
            Ok(())
        } else {
            self.coding_unit(x0, y0, log2, depth)
        }
    }

    /// Set per-4x4 state of a rectangle (luma coordinates).
    pub(super) fn fill_blk(&mut self, x0: i32, y0: i32, w: i32, h: i32, f: impl Fn(&mut Blk)) {
        let (bx0, by0) = ((x0 >> 2) as usize, (y0 >> 2) as usize);
        let bx1 = (((x0 + w) as usize).min(self.pic.width).div_ceil(4)).max(bx0);
        let by1 = (((y0 + h) as usize).min(self.pic.height).div_ceil(4)).max(by0);
        for by in by0..by1 {
            for b in &mut self.pic.blk[by * self.pic.w4 + bx0..by * self.pic.w4 + bx1] {
                f(b);
            }
        }
    }

    /// qPY_PRED derivation (8.6.1) at the start of a quantization group.
    fn derive_qp_pred(&mut self, xq: i32, yq: i32) {
        let prev = self.last_qp;
        let s = self.sps.log2_ctb;
        let cur_ctb = ((yq >> s), (xq >> s));
        let qa = if self.pic.zavail(xq, yq, xq - 1, yq, self.slice_addr) && ((yq >> s), ((xq - 1) >> s)) == cur_ctb {
            self.pic.blk_at(xq - 1, yq).qp as i32
        } else {
            prev
        };
        let qb = if self.pic.zavail(xq, yq, xq, yq - 1, self.slice_addr) && (((yq - 1) >> s), (xq >> s)) == cur_ctb {
            self.pic.blk_at(xq, yq - 1).qp as i32
        } else {
            prev
        };
        self.qp_y_pred = (qa + qb + 1) >> 1;
    }

    pub(super) fn update_qp(&mut self) {
        let off = 6 * (self.pic.bd_y as i32 - 8);
        self.qp_y = ((self.qp_y_pred + self.cu_qp_delta + 52 + 2 * off) % (52 + off)) - off;
        let offc = 6 * (self.pic.bd_c as i32 - 8);
        let qpi_cb = (self.qp_y + self.pps.cb_qp_offset + self.sh.cb_qp_offset).clamp(-offc, 57);
        let qpi_cr = (self.qp_y + self.pps.cr_qp_offset + self.sh.cr_qp_offset).clamp(-offc, 57);
        self.qp_cb = qpc_420(qpi_cb) + offc;
        self.qp_cr = qpc_420(qpi_cr) + offc;
    }

    fn coding_unit(&mut self, x0: i32, y0: i32, log2: u32, depth: u32) -> Result<()> {
        let n = 1i32 << log2;
        // quantization group start
        let qg_mask = (1 << self.log2_min_cu_qp_delta) - 1;
        if (x0 & qg_mask) == 0 && (y0 & qg_mask) == 0 {
            self.derive_qp_pred(x0, y0);
        }
        self.update_qp();
        self.cu_transquant_bypass = if self.pps.transquant_bypass { self.dd(CU_TRANSQUANT_BYPASS) == 1 } else { false };
        let mut skip = false;
        if !self.sh.is_intra() {
            let mut inc = 0;
            if self.pic.zavail(x0, y0, x0 - 1, y0, self.slice_addr) && self.pic.blk_at(x0 - 1, y0).flags & F_SKIP != 0 {
                inc += 1;
            }
            if self.pic.zavail(x0, y0, x0, y0 - 1, self.slice_addr) && self.pic.blk_at(x0, y0 - 1).flags & F_SKIP != 0 {
                inc += 1;
            }
            skip = self.dd(CU_SKIP + inc) == 1;
        }
        // CU-level block state: depth, skip, coded; CU edges are transform and prediction edges
        let bypass = self.cu_transquant_bypass;
        let qp = self.qp_y as i8;
        self.fill_blk(x0, y0, n, n, |b| {
            b.depth = depth as u8;
            b.flags = F_CODED | if skip { F_SKIP } else { 0 } | if bypass { F_NOFILTER } else { 0 };
            b.qp = qp;
            b.ipm = 1;
            b.edges = 0;
        });
        self.mark_edges(x0, y0, n, n, E_TU_V | E_PU_V, E_TU_H | E_PU_H);
        if skip {
            self.cu_intra = false;
            self.prediction_unit(x0, y0, n, x0, y0, n, n, 0, PartMode::P2Nx2N, true)?;
            self.finish_cu(x0, y0, n);
            return Ok(());
        }
        let intra = if !self.sh.is_intra() { self.dd(PRED_MODE) == 1 } else { true };
        self.cu_intra = intra;
        let mut part = PartMode::P2Nx2N;
        if !intra || log2 == self.sps.log2_min_cb {
            part = self.parse_part_mode(intra, log2)?;
        }
        self.cu_part = part;
        if intra {
            self.fill_blk(x0, y0, n, n, |b| b.flags |= F_INTRA);
            self.intra_split = part == PartMode::PNxN;
            let pcm =
                part == PartMode::P2Nx2N && self.sps.pcm && log2 >= self.sps.log2_min_pcm && log2 <= self.sps.log2_max_pcm && self.c.decode_terminate() == 1;
            if pcm {
                self.pcm_sample(x0, y0, log2)?;
                if self.sps.pcm_loop_filter_disabled {
                    self.fill_blk(x0, y0, n, n, |b| b.flags |= F_NOFILTER);
                }
                self.finish_cu(x0, y0, n);
                return Ok(());
            }
            self.parse_intra_modes(x0, y0, n, part)?;
            self.max_trafo_depth = self.sps.max_th_depth_intra + self.intra_split as u32;
            self.transform_tree(x0, y0, x0, y0, log2, 0, 0, [true, true])?;
        } else {
            self.intra_split = false;
            let mut merge2nx2n = false;
            let h = n / 2;
            let q = n / 4;
            let parts: &[(i32, i32, i32, i32)] = match part {
                PartMode::P2Nx2N => &[(0, 0, n, n)],
                PartMode::P2NxN => &[(0, 0, n, h), (0, h, n, h)],
                PartMode::PNx2N => &[(0, 0, h, n), (h, 0, h, n)],
                PartMode::P2NxnU => &[(0, 0, n, q), (0, q, n, n - q)],
                PartMode::P2NxnD => &[(0, 0, n, n - q), (0, n - q, n, q)],
                PartMode::PnLx2N => &[(0, 0, q, n), (q, 0, n - q, n)],
                PartMode::PnRx2N => &[(0, 0, n - q, n), (n - q, 0, q, n)],
                PartMode::PNxN => &[(0, 0, h, h), (h, 0, h, h), (0, h, h, h), (h, h, h, h)],
            };
            for (i, &(px, py, pw, ph)) in parts.iter().enumerate() {
                let merge = self.prediction_unit(x0, y0, n, x0 + px, y0 + py, pw, ph, i, part, false)?;
                if part == PartMode::P2Nx2N {
                    merge2nx2n = merge;
                }
                if px != 0 {
                    self.mark_edges(x0 + px, y0 + py, pw, ph, E_PU_V, 0);
                }
                if py != 0 {
                    self.mark_edges(x0 + px, y0 + py, pw, ph, 0, E_PU_H);
                }
            }
            let root_cbf = if !merge2nx2n { self.dd(RQT_ROOT_CBF) == 1 } else { true };
            if root_cbf {
                self.max_trafo_depth = self.sps.max_th_depth_inter;
                self.transform_tree(x0, y0, x0, y0, log2, 0, 0, [true, true])?;
            }
        }
        self.finish_cu(x0, y0, n);
        Ok(())
    }

    /// Store the final QpY of the CU (may have changed with cu_qp_delta).
    fn finish_cu(&mut self, x0: i32, y0: i32, n: i32) {
        let qp = self.qp_y as i8;
        self.fill_blk(x0, y0, n, n, |b| b.qp = qp);
        self.last_qp = self.qp_y;
    }

    /// Mark the left (vertical) and top (horizontal) edges of a rectangle.
    pub(super) fn mark_edges(&mut self, x0: i32, y0: i32, w: i32, h: i32, v: u8, hz: u8) {
        let w4 = self.pic.w4;
        let (bx0, by0) = ((x0 >> 2) as usize, (y0 >> 2) as usize);
        if v != 0 {
            let by1 = ((y0 + h) as usize).min(self.pic.height).div_ceil(4);
            for by in by0..by1 {
                self.pic.blk[by * w4 + bx0].edges |= v;
            }
        }
        if hz != 0 {
            let bx1 = ((x0 + w) as usize).min(self.pic.width).div_ceil(4);
            for b in &mut self.pic.blk[by0 * w4 + bx0..by0 * w4 + bx1] {
                b.edges |= hz;
            }
        }
    }

    fn parse_part_mode(&mut self, intra: bool, log2: u32) -> Result<PartMode> {
        if intra {
            return Ok(if self.dd(PART_MODE) == 1 { PartMode::P2Nx2N } else { PartMode::PNxN });
        }
        if self.dd(PART_MODE) == 1 {
            return Ok(PartMode::P2Nx2N);
        }
        let min = log2 == self.sps.log2_min_cb;
        if min {
            if self.dd(PART_MODE + 1) == 1 {
                return Ok(PartMode::P2NxN);
            }
            if log2 == 3 {
                return Ok(PartMode::PNx2N);
            }
            return Ok(if self.dd(PART_MODE + 2) == 1 { PartMode::PNx2N } else { PartMode::PNxN });
        }
        let amp = self.sps.amp;
        let hor = self.dd(PART_MODE + 1) == 1;
        if !amp {
            return Ok(if hor { PartMode::P2NxN } else { PartMode::PNx2N });
        }
        let no_amp = self.dd(PART_MODE + 3) == 1;
        if no_amp {
            return Ok(if hor { PartMode::P2NxN } else { PartMode::PNx2N });
        }
        let b = self.bypass();
        Ok(match (hor, b) {
            (true, 0) => PartMode::P2NxnU,
            (true, _) => PartMode::P2NxnD,
            (false, 0) => PartMode::PnLx2N,
            (false, _) => PartMode::PnRx2N,
        })
    }

    fn pcm_sample(&mut self, x0: i32, y0: i32, log2: u32) -> Result<()> {
        // the PCM samples start at the next byte boundary after the terminate bin
        let start = self.c.bit_pos().div_ceil(8);
        let data = self.c.data();
        let n = 1usize << log2;
        let (bl, bc) = (self.sps.pcm_bit_depth_luma, self.sps.pcm_bit_depth_chroma);
        let total_bits = n * n * bl as usize + 2 * (n / 2) * (n / 2) * bc as usize;
        ensure!(start + total_bits.div_ceil(8) <= data.len(), "PCM samples truncated");
        let mut r = filmcraft_bitstream::BitReader::new(&data[start..]);
        let (sy, sc) = (self.pic.bd_y - bl, self.pic.bd_c - bc);
        let w = self.pic.width;
        for y in 0..n {
            for x in 0..n {
                let v = r.read_bits(bl)? as u16;
                self.pic.planes[0][(y0 as usize + y) * w + x0 as usize + x] = S::from_u16(v << sy);
            }
        }
        let cw = self.pic.cwidth;
        for c in 1..3 {
            for y in 0..n / 2 {
                for x in 0..n / 2 {
                    let v = r.read_bits(bc)? as u16;
                    self.pic.planes[c][(y0 as usize / 2 + y) * cw + x0 as usize / 2 + x] = S::from_u16(v << sc);
                }
            }
        }
        let end = start + r.position().div_ceil(8);
        self.c.set_byte_pos(end);
        self.c.init_engine()
    }

    fn parse_intra_modes(&mut self, x0: i32, y0: i32, n: i32, part: PartMode) -> Result<()> {
        let nb = if part == PartMode::PNxN { 2 } else { 1 };
        let pb = n / nb;
        let mut prev = [false; 4];
        for (i, p) in prev.iter_mut().enumerate().take((nb * nb) as usize) {
            let _ = i;
            *p = self.dd(PREV_INTRA_LUMA) == 1;
        }
        for j in 0..nb {
            for i in 0..nb {
                let k = (j * nb + i) as usize;
                let (xp, yp) = (x0 + i * pb, y0 + j * pb);
                // candidate modes (8.4.2)
                let cand_a = if self.pic.zavail(xp, yp, xp - 1, yp, self.slice_addr) {
                    let b = self.pic.blk_at(xp - 1, yp);
                    if b.flags & F_INTRA != 0 { b.ipm as u32 } else { 1 }
                } else {
                    1
                };
                let cand_b = if self.pic.zavail(xp, yp, xp, yp - 1, self.slice_addr) && yp > ((yp >> self.sps.log2_ctb) << self.sps.log2_ctb) {
                    let b = self.pic.blk_at(xp, yp - 1);
                    if b.flags & F_INTRA != 0 { b.ipm as u32 } else { 1 }
                } else {
                    1
                };
                let mut list = if cand_a == cand_b {
                    if cand_a < 2 { [0, 1, 26] } else { [cand_a, 2 + ((cand_a + 29) % 32), 2 + ((cand_a - 2 + 1) % 32)] }
                } else {
                    let c = if cand_a != 0 && cand_b != 0 {
                        0
                    } else if cand_a != 1 && cand_b != 1 {
                        1
                    } else {
                        26
                    };
                    [cand_a, cand_b, c]
                };
                let mode = if prev[k] {
                    let idx = if self.bypass() == 0 {
                        0
                    } else if self.bypass() == 0 {
                        1
                    } else {
                        2
                    };
                    list[idx]
                } else {
                    let mut m = self.bypass_bits(5);
                    list.sort_unstable();
                    for &c in &list {
                        if m >= c {
                            m += 1;
                        }
                    }
                    m
                };
                self.fill_blk(xp, yp, pb, pb, |b| b.ipm = mode as u8);
            }
        }
        // chroma mode (4:2:0: one per CU, derived from the first PU's luma mode)
        let icpm = if self.dd(INTRA_CHROMA) == 0 { 4 } else { self.bypass_bits(2) };
        let luma = self.pic.blk_at(x0, y0).ipm as u32;
        self.intra_chroma_mode = match icpm {
            4 => luma,
            _ => {
                let m = [0, 26, 10, 1][icpm as usize];
                if m == luma { 34 } else { m }
            }
        };
        Ok(())
    }
}

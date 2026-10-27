use super::tx_size::find_tx_size;
use super::*;

impl<'a, 'f> TileDecoder<'a, 'f> {
    // ---- prediction and residual ----------------------------------------------------------

    /// compute_prediction( ) (5.11.33)
    pub(super) fn compute_prediction(&mut self) -> Result<()> {
        if !self.b.is_inter {
            return Ok(());
        }
        let sb_mask = if self.fs.seq.use_128x128_superblock { 31 } else { 15 };
        let sub_row = self.b.mi_row & sb_mask;
        let sub_col = self.b.mi_col & sb_mask;
        let (ssx, ssy) = (self.fs.ssx, self.fs.ssy);
        let is_inter_intra = self.b.ref_frame[1] == INTRA_FRAME as i8;
        for plane in 0..(1 + 2 * self.b.has_chroma as usize) {
            let plane_sz = self.fs.plane_residual_size(self.b.mi_size, plane);
            let n4w = NUM_4X4_BLOCKS_WIDE[plane_sz] as usize;
            let n4h = NUM_4X4_BLOCKS_HIGH[plane_sz] as usize;
            let log2w = 2 + MI_WIDTH_LOG2[plane_sz] as u32;
            let log2h = 2 + MI_HEIGHT_LOG2[plane_sz] as u32;
            let sub_x = if plane > 0 { ssx } else { 0 };
            let sub_y = if plane > 0 { ssy } else { 0 };
            let base_x = (self.b.mi_col >> sub_x) * 4;
            let base_y = (self.b.mi_row >> sub_y) * 4;
            let mut cand_row = (self.b.mi_row >> sub_y) << sub_y;
            let mut cand_col = (self.b.mi_col >> sub_x) << sub_x;
            if is_inter_intra {
                let mode = match self.b.interintra_mode {
                    II_DC_PRED => DC_PRED,
                    II_V_PRED => V_PRED,
                    II_H_PRED => H_PRED,
                    _ => SMOOTH_PRED,
                };
                let sbr = (sub_row >> sub_y) as isize;
                let sbc = (sub_col >> sub_x) as isize;
                let max_x = ((self.fs.fh.mi_cols as usize * 4) >> sub_x) as i32 - 1;
                let max_y = ((self.fs.fh.mi_rows as usize * 4) >> sub_y) as i32 - 1;
                let params = IntraParams {
                    plane,
                    x: base_x,
                    y: base_y,
                    have_left: if plane == 0 { self.b.avail_l } else { self.b.avail_l_chroma },
                    have_above: if plane == 0 { self.b.avail_u } else { self.b.avail_u_chroma },
                    have_above_right: self.block_decoded_at(plane, sbr - 1, sbc + n4w as isize),
                    have_below_left: self.block_decoded_at(plane, sbr + n4h as isize, sbc - 1),
                    mode,
                    log2w,
                    log2h,
                    max_x,
                    max_y,
                    bit_depth: self.fs.bit_depth,
                    angle_delta: 0,
                    use_filter_intra: false,
                    filter_intra_mode: 0,
                    enable_intra_edge_filter: self.fs.seq.enable_intra_edge_filter,
                    filter_type: if is_directional_mode(mode) && self.fs.seq.enable_intra_edge_filter { self.get_filter_type(plane) } else { false },
                };
                predict_intra(&mut self.t.cur.planes[plane], &params);
            }
            let mut pred_w = block_width(self.b.mi_size) >> sub_x;
            let mut pred_h = block_height(self.b.mi_size) >> sub_y;
            let mut some_use_intra = false;
            for r in 0..(n4h << sub_y) {
                for c in 0..(n4w << sub_x) {
                    let (rr, cc) = (cand_row + r, cand_col + c);
                    if rr < self.t.mi.rows && cc < self.t.mi.cols && self.t.mi.ref_frame[self.t.mi.idx(rr, cc)][0] == INTRA_FRAME as i8 {
                        some_use_intra = true;
                    }
                }
            }
            if some_use_intra {
                pred_w = n4w * 4;
                pred_h = n4h * 4;
                cand_row = self.b.mi_row;
                cand_col = self.b.mi_col;
            }
            let mut r = 0;
            let mut y = 0;
            while y < n4h * 4 {
                let mut c = 0;
                let mut x = 0;
                while x < n4w * 4 {
                    self.predict_inter(plane, base_x + x, base_y + y, pred_w, pred_h, cand_row + r, cand_col + c);
                    x += pred_w;
                    c += 1;
                }
                y += pred_h;
                r += 1;
            }
        }
        Ok(())
    }

    fn get_tx_size(&self, plane: usize, tx_sz: usize) -> usize {
        if plane == 0 {
            return tx_sz;
        }
        let uv_tx = MAX_TX_SIZE_RECT[self.fs.plane_residual_size(self.b.mi_size, plane)] as usize;
        if TX_WIDTH[uv_tx] == 64 || TX_HEIGHT[uv_tx] == 64 {
            if TX_WIDTH[uv_tx] == 16 {
                return TX_16X32;
            }
            if TX_HEIGHT[uv_tx] == 16 {
                return TX_32X16;
            }
            return TX_32X32;
        }
        uv_tx
    }

    pub(super) fn residual(&mut self) -> Result<()> {
        let sb_mask = if self.fs.seq.use_128x128_superblock { 31 } else { 15 };
        let ms = self.b.mi_size;
        let width_chunks = (block_width(ms) >> 6).max(1);
        let height_chunks = (block_height(ms) >> 6).max(1);
        let mi_size_chunk = if width_chunks > 1 || height_chunks > 1 { BLOCK_64X64 } else { ms };
        let (ssx, ssy) = (self.fs.ssx, self.fs.ssy);
        for chunk_y in 0..height_chunks {
            for chunk_x in 0..width_chunks {
                let mi_row_chunk = self.b.mi_row + (chunk_y << 4);
                let mi_col_chunk = self.b.mi_col + (chunk_x << 4);
                let _ = (mi_row_chunk & sb_mask, mi_col_chunk & sb_mask);
                for plane in 0..(1 + 2 * self.b.has_chroma as usize) {
                    let tx_sz = if self.b.lossless { TX_4X4 } else { self.get_tx_size(plane, self.b.tx_size) };
                    let step_x = TX_WIDTH[tx_sz] as usize >> 2;
                    let step_y = TX_HEIGHT[tx_sz] as usize >> 2;
                    let plane_sz = self.fs.plane_residual_size(mi_size_chunk, plane);
                    let num4x4_w = NUM_4X4_BLOCKS_WIDE[plane_sz] as usize;
                    let num4x4_h = NUM_4X4_BLOCKS_HIGH[plane_sz] as usize;
                    let sub_x = if plane > 0 { ssx } else { 0 };
                    let sub_y = if plane > 0 { ssy } else { 0 };
                    let base_x = (mi_col_chunk >> sub_x) * 4;
                    let base_y = (mi_row_chunk >> sub_y) * 4;
                    if self.b.is_inter && !self.b.lossless && plane == 0 {
                        self.transform_tree(base_x, base_y, num4x4_w * 4, num4x4_h * 4)?;
                    } else {
                        let base_x_block = (self.b.mi_col >> sub_x) * 4;
                        let base_y_block = (self.b.mi_row >> sub_y) * 4;
                        let mut y = 0;
                        while y < num4x4_h {
                            let mut x = 0;
                            while x < num4x4_w {
                                self.transform_block(plane, base_x_block, base_y_block, tx_sz, x + ((chunk_x << 4) >> sub_x), y + ((chunk_y << 4) >> sub_y))?;
                                x += step_x;
                            }
                            y += step_y;
                        }
                    }
                }
            }
        }
        Ok(())
    }

    fn transform_tree(&mut self, start_x: usize, start_y: usize, w: usize, h: usize) -> Result<()> {
        let max_x = self.fs.fh.mi_cols as usize * 4;
        let max_y = self.fs.fh.mi_rows as usize * 4;
        if start_x >= max_x || start_y >= max_y {
            return Ok(());
        }
        let row = start_y >> 2;
        let col = start_x >> 2;
        let mi = &self.t.mi;
        let luma_tx = mi.inter_tx_size[mi.idx(row, col)] as usize;
        let lw = TX_WIDTH[luma_tx] as usize;
        let lh = TX_HEIGHT[luma_tx] as usize;
        if w <= lw && h <= lh {
            let tx_sz = find_tx_size(w, h);
            self.transform_block(0, start_x, start_y, tx_sz, 0, 0)?;
        } else if w > h {
            self.transform_tree(start_x, start_y, w / 2, h)?;
            self.transform_tree(start_x + w / 2, start_y, w / 2, h)?;
        } else if w < h {
            self.transform_tree(start_x, start_y, w, h / 2)?;
            self.transform_tree(start_x, start_y + h / 2, w, h / 2)?;
        } else {
            self.transform_tree(start_x, start_y, w / 2, h / 2)?;
            self.transform_tree(start_x + w / 2, start_y, w / 2, h / 2)?;
            self.transform_tree(start_x, start_y + h / 2, w / 2, h / 2)?;
            self.transform_tree(start_x + w / 2, start_y + h / 2, w / 2, h / 2)?;
        }
        Ok(())
    }

    fn transform_block(&mut self, plane: usize, base_x: usize, base_y: usize, tx_sz: usize, x: usize, y: usize) -> Result<()> {
        let start_x = base_x + 4 * x;
        let start_y = base_y + 4 * y;
        let (ssx, ssy) = (self.fs.ssx, self.fs.ssy);
        let sub_x = if plane > 0 { ssx } else { 0 };
        let sub_y = if plane > 0 { ssy } else { 0 };
        let row = (start_y << sub_y) >> 2;
        let col = (start_x << sub_x) >> 2;
        let sb_mask = if self.fs.seq.use_128x128_superblock { 31 } else { 15 };
        let sub_block_mi_row = row & sb_mask;
        let sub_block_mi_col = col & sb_mask;
        let step_x = TX_WIDTH[tx_sz] as usize >> 2;
        let step_y = TX_HEIGHT[tx_sz] as usize >> 2;
        let max_x = (self.fs.fh.mi_cols as usize * 4) >> sub_x;
        let max_y = (self.fs.fh.mi_rows as usize * 4) >> sub_y;
        if start_x >= max_x || start_y >= max_y {
            return Ok(());
        }
        if !self.b.is_inter {
            let palette = if plane == 0 { self.b.palette_size_y } else { self.b.palette_size_uv };
            if palette > 0 {
                self.predict_palette(plane, start_x, start_y, x, y, tx_sz);
            } else {
                let is_cfl = plane > 0 && self.b.uv_mode == UV_CFL_PRED;
                let mode = if plane == 0 {
                    self.b.y_mode
                } else if is_cfl {
                    DC_PRED
                } else {
                    self.b.uv_mode
                };
                let log2w = TX_WIDTH_LOG2[tx_sz] as u32;
                let log2h = TX_HEIGHT_LOG2[tx_sz] as u32;
                let have_left = (if plane == 0 { self.b.avail_l } else { self.b.avail_l_chroma }) || x > 0;
                let have_above = (if plane == 0 { self.b.avail_u } else { self.b.avail_u_chroma }) || y > 0;
                let sbr = (sub_block_mi_row >> sub_y) as isize;
                let sbc = (sub_block_mi_col >> sub_x) as isize;
                let have_above_right = self.block_decoded_at(plane, sbr - 1, sbc + step_x as isize);
                let have_below_left = self.block_decoded_at(plane, sbr + step_y as isize, sbc - 1);
                let filter_type = if is_directional_mode(mode) && self.fs.seq.enable_intra_edge_filter { self.get_filter_type(plane) } else { false };
                let params = IntraParams {
                    plane,
                    x: start_x,
                    y: start_y,
                    have_left,
                    have_above,
                    have_above_right,
                    have_below_left,
                    mode,
                    log2w,
                    log2h,
                    max_x: max_x as i32 - 1,
                    max_y: max_y as i32 - 1,
                    bit_depth: self.fs.bit_depth,
                    angle_delta: if plane == 0 { self.b.angle_delta_y } else { self.b.angle_delta_uv },
                    use_filter_intra: self.b.use_filter_intra,
                    filter_intra_mode: self.b.filter_intra_mode,
                    enable_intra_edge_filter: self.fs.seq.enable_intra_edge_filter,
                    filter_type,
                };
                predict_intra(&mut self.t.cur.planes[plane], &params);
                if is_cfl {
                    self.predict_cfl(plane, start_x, start_y, tx_sz);
                }
            }
            if plane == 0 {
                self.b.max_luma_w = start_x + step_x * 4;
                self.b.max_luma_h = start_y + step_y * 4;
            }
        }
        if !self.b.skip {
            let eob = self.coeffs(plane, start_x, start_y, tx_sz);
            if eob > 0 {
                self.reconstruct(plane, start_x, start_y, tx_sz);
            }
        }
        for i in 0..step_y {
            for j in 0..step_x {
                let rr = (row >> sub_y) + i;
                let cc = (col >> sub_x) + j;
                self.t.set_lf_tx_size(plane, rr, cc, tx_sz as u8);
                let by = (sub_block_mi_row >> sub_y) + i;
                let bx = (sub_block_mi_col >> sub_x) + j;
                if by + 1 < 35 && bx + 1 < 35 {
                    self.block_decoded[plane][by + 1][bx + 1] = true;
                }
            }
        }
        Ok(())
    }

    pub(crate) fn get_filter_type(&self, plane: usize) -> bool {
        let (ssx, ssy) = (self.fs.ssx, self.fs.ssy);
        let b = &self.b;
        let mi = &self.t.mi;
        let is_smooth = |row: usize, col: usize| -> bool {
            let i = mi.idx(row, col);
            let mode = if plane == 0 {
                mi.y_mode[i] as usize
            } else {
                if mi.ref_frame[i][0] > INTRA_FRAME as i8 {
                    return false;
                }
                mi.uv_mode[i] as usize
            };
            mode == SMOOTH_PRED || mode == SMOOTH_V_PRED || mode == SMOOTH_H_PRED
        };
        let mut above_smooth = false;
        let mut left_smooth = false;
        if if plane == 0 { b.avail_u } else { b.avail_u_chroma } {
            let mut r = b.mi_row as isize - 1;
            let mut c = b.mi_col;
            if plane > 0 {
                if ssx == 1 && (b.mi_col & 1) == 0 {
                    c += 1;
                }
                if ssy == 1 && (b.mi_row & 1) == 1 {
                    r -= 1;
                }
            }
            above_smooth = is_smooth(r as usize, c);
        }
        if if plane == 0 { b.avail_l } else { b.avail_l_chroma } {
            let mut r = b.mi_row;
            let mut c = b.mi_col as isize - 1;
            if plane > 0 {
                if ssx == 1 && (b.mi_col & 1) == 1 {
                    c -= 1;
                }
                if ssy == 1 && (b.mi_row & 1) == 0 {
                    r += 1;
                }
            }
            left_smooth = is_smooth(r, c as usize);
        }
        above_smooth || left_smooth
    }

    fn predict_palette(&mut self, plane: usize, start_x: usize, start_y: usize, x: usize, y: usize, tx_sz: usize) {
        let w = TX_WIDTH[tx_sz] as usize;
        let h = TX_HEIGHT[tx_sz] as usize;
        let palette = match plane {
            0 => self.b.palette_colors_y,
            1 => self.b.palette_colors_u,
            _ => self.b.palette_colors_v,
        };
        let map = if plane == 0 { &self.color_map_y } else { &self.color_map_uv };
        let pl = &mut self.t.cur.planes[plane];
        for i in 0..h {
            for j in 0..w {
                let idx = map[(y * 4 + i) * 64 + x * 4 + j] as usize;
                pl.set(start_x + j, start_y + i, palette[idx]);
            }
        }
    }

    fn predict_cfl(&mut self, plane: usize, start_x: usize, start_y: usize, tx_sz: usize) {
        let w = TX_WIDTH[tx_sz] as usize;
        let h = TX_HEIGHT[tx_sz] as usize;
        let (sub_x, sub_y) = (self.fs.ssx, self.fs.ssy);
        let alpha = if plane == 1 { self.b.cfl_alpha_u } else { self.b.cfl_alpha_v };
        let bd = self.fs.bit_depth;
        let mut l = [0i32; 32 * 32];
        let mut avg: i64 = 0;
        {
            let luma = &self.t.cur.planes[0];
            for i in 0..h {
                let ly = ((start_y + i) << sub_y).min(self.b.max_luma_h - (1 << sub_y));
                for j in 0..w {
                    let lx = ((start_x + j) << sub_x).min(self.b.max_luma_w - (1 << sub_x));
                    let mut t = 0i32;
                    for dy in 0..=sub_y {
                        for dx in 0..=sub_x {
                            t += luma.at(lx + dx, ly + dy) as i32;
                        }
                    }
                    let v = t << (3 - sub_x - sub_y);
                    l[i * w + j] = v;
                    avg += v as i64;
                }
            }
        }
        let shift = TX_WIDTH_LOG2[tx_sz] as u32 + TX_HEIGHT_LOG2[tx_sz] as u32;
        let avg = ((avg + (1i64 << (shift - 1))) >> shift) as i32;
        let pl = &mut self.t.cur.planes[plane];
        for i in 0..h {
            for j in 0..w {
                let dc = pl.at(start_x + j, start_y + i) as i32;
                let scaled = round2_signed(alpha * (l[i * w + j] - avg), 6);
                pl.set(start_x + j, start_y + i, (dc + scaled).clamp(0, (1 << bd) - 1) as u16);
            }
        }
    }
}

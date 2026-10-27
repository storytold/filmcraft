use super::coeffs::ceil_log2;
use super::*;

impl<'a, 'f> TileDecoder<'a, 'f> {
    // ---- palette -------------------------------------------------------------------------

    pub(crate) fn palette_mode_info(&mut self) {
        let ms = self.b.mi_size;
        let bsize_ctx = MI_WIDTH_LOG2[ms] as usize + MI_HEIGHT_LOG2[ms] as usize - 2;
        let bd = self.fs.bit_depth;
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        if self.b.y_mode == DC_PRED {
            let mi = &self.t.mi;
            let mut ctx = 0;
            if self.b.avail_u && mi.palette_size[0][mi.idx(r - 1, c)] > 0 {
                ctx += 1;
            }
            if self.b.avail_l && mi.palette_size[0][mi.idx(r, c - 1)] > 0 {
                ctx += 1;
            }
            if self.sd.read_symbol(&mut self.cdf.palette_y_mode[bsize_ctx][ctx]) == 1 {
                let n = self.sd.read_symbol(&mut self.cdf.palette_y_size[bsize_ctx]) + 2;
                self.b.palette_size_y = n;
                let cache = self.get_palette_cache(0);
                let mut colors = [0u16; 8];
                let mut idx = 0;
                for &cv in &cache {
                    if idx >= n {
                        break;
                    }
                    if self.sd.read_literal(1) == 1 {
                        colors[idx] = cv;
                        idx += 1;
                    }
                }
                if idx < n {
                    colors[idx] = self.sd.read_literal(bd) as u16;
                    idx += 1;
                }
                let mut palette_bits = 0;
                if idx < n {
                    let min_bits = bd - 3;
                    palette_bits = min_bits + self.sd.read_literal(2);
                }
                while idx < n {
                    let delta = self.sd.read_literal(palette_bits) as i32 + 1;
                    colors[idx] = (colors[idx - 1] as i32 + delta).clamp(0, (1 << bd) - 1) as u16;
                    let range = (1i32 << bd) - colors[idx] as i32 - 1;
                    palette_bits = palette_bits.min(ceil_log2(range as u32));
                    idx += 1;
                }
                colors[..n].sort_unstable();
                self.b.palette_colors_y = colors;
            }
        }
        if self.b.has_chroma && self.b.uv_mode == DC_PRED {
            let ctx = (self.b.palette_size_y > 0) as usize;
            if self.sd.read_symbol(&mut self.cdf.palette_uv_mode[ctx]) == 1 {
                let n = self.sd.read_symbol(&mut self.cdf.palette_uv_size[bsize_ctx]) + 2;
                self.b.palette_size_uv = n;
                let cache = self.get_palette_cache(1);
                let mut colors = [0u16; 8];
                let mut idx = 0;
                for &cv in &cache {
                    if idx >= n {
                        break;
                    }
                    if self.sd.read_literal(1) == 1 {
                        colors[idx] = cv;
                        idx += 1;
                    }
                }
                if idx < n {
                    colors[idx] = self.sd.read_literal(bd) as u16;
                    idx += 1;
                }
                let mut palette_bits = 0;
                if idx < n {
                    palette_bits = bd - 3 + self.sd.read_literal(2);
                }
                while idx < n {
                    let delta = self.sd.read_literal(palette_bits) as i32;
                    colors[idx] = (colors[idx - 1] as i32 + delta).clamp(0, (1 << bd) - 1) as u16;
                    let range = (1i32 << bd) - colors[idx] as i32;
                    palette_bits = palette_bits.min(ceil_log2(range as u32));
                    idx += 1;
                }
                colors[..n].sort_unstable();
                self.b.palette_colors_u = colors;
                let mut v = [0u16; 8];
                if self.sd.read_literal(1) == 1 {
                    let min_bits = bd - 4;
                    let max_val = 1i32 << bd;
                    let bits = min_bits + self.sd.read_literal(2);
                    v[0] = self.sd.read_literal(bd) as u16;
                    for idx in 1..n {
                        let mut delta = self.sd.read_literal(bits) as i32;
                        if delta != 0 && self.sd.read_literal(1) == 1 {
                            delta = -delta;
                        }
                        let mut val = v[idx - 1] as i32 + delta;
                        if val < 0 {
                            val += max_val;
                        }
                        if val >= max_val {
                            val -= max_val;
                        }
                        v[idx] = val.clamp(0, (1 << bd) - 1) as u16;
                    }
                } else {
                    for item in v.iter_mut().take(n) {
                        *item = self.sd.read_literal(bd) as u16;
                    }
                }
                self.b.palette_colors_v = v;
            }
        }
    }

    fn get_palette_cache(&self, plane: usize) -> Vec<u16> {
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let mi = &self.t.mi;
        let above_n = if (r * 4) % 64 != 0 { mi.palette_size[plane][mi.idx(r - 1, c)] as usize } else { 0 };
        let left_n = if self.b.avail_l { mi.palette_size[plane][mi.idx(r, c - 1)] as usize } else { 0 };
        let above: &[u16] = if above_n > 0 { &mi.palette_colors[plane][mi.idx(r - 1, c)][..above_n] } else { &[] };
        let left: &[u16] = if left_n > 0 { &mi.palette_colors[plane][mi.idx(r, c - 1)][..left_n] } else { &[] };
        let (mut ai, mut li) = (0, 0);
        let mut out: Vec<u16> = Vec::with_capacity(16);
        while ai < above_n && li < left_n {
            let (ac, lc) = (above[ai], left[li]);
            if lc < ac {
                if out.last() != Some(&lc) {
                    out.push(lc);
                }
                li += 1;
            } else {
                if out.last() != Some(&ac) {
                    out.push(ac);
                }
                ai += 1;
                if lc == ac {
                    li += 1;
                }
            }
        }
        for &v in &above[ai..above_n] {
            if out.last() != Some(&v) {
                out.push(v);
            }
        }
        for &v in &left[li..left_n] {
            if out.last() != Some(&v) {
                out.push(v);
            }
        }
        out
    }

    pub(super) fn palette_tokens(&mut self) {
        let ms = self.b.mi_size;
        let bh = block_height(ms);
        let bw = block_width(ms);
        let (mi_rows, mi_cols) = (self.fs.fh.mi_rows as usize, self.fs.fh.mi_cols as usize);
        let on_h = bh.min((mi_rows - self.b.mi_row) * 4);
        let on_w = bw.min((mi_cols - self.b.mi_col) * 4);
        if self.b.palette_size_y > 0 {
            let n = self.b.palette_size_y;
            let mut map = std::mem::take(&mut self.color_map_y);
            self.read_color_map(&mut map, n, bw, bh, on_w, on_h, 0);
            self.color_map_y = map;
        }
        if self.b.palette_size_uv > 0 {
            let n = self.b.palette_size_uv;
            let (ssx, ssy) = (self.fs.ssx, self.fs.ssy);
            let (mut bw, mut bh, mut on_w, mut on_h) = (bw >> ssx, bh >> ssy, on_w >> ssx, on_h >> ssy);
            if bw < 4 {
                bw += 2;
                on_w += 2;
            }
            if bh < 4 {
                bh += 2;
                on_h += 2;
            }
            let mut map = std::mem::take(&mut self.color_map_uv);
            self.read_color_map(&mut map, n, bw, bh, on_w, on_h, 1);
            self.color_map_uv = map;
        }
    }

    /// Color index map (stride 64).
    #[allow(clippy::too_many_arguments)]
    fn read_color_map(&mut self, map: &mut [u8], n: usize, bw: usize, bh: usize, on_w: usize, on_h: usize, plane: usize) {
        map[0] = self.sd.read_ns(n as u32) as u8;
        for i in 1..(on_h + on_w - 1) {
            let jmax = i.min(on_w - 1);
            let jmin = (i as isize - on_h as isize + 1).max(0) as usize;
            let mut j = jmax as isize;
            while j >= jmin as isize {
                let (rr, cc) = (i - j as usize, j as usize);
                let (order, hash) = palette_color_context(map, rr, cc, n);
                let ctx = PALETTE_COLOR_CONTEXT[hash] as usize;
                let cdf: &mut [u16] = match (plane, n) {
                    (0, 2) => &mut self.cdf.palette_2_y_color[ctx],
                    (0, 3) => &mut self.cdf.palette_3_y_color[ctx],
                    (0, 4) => &mut self.cdf.palette_4_y_color[ctx],
                    (0, 5) => &mut self.cdf.palette_5_y_color[ctx],
                    (0, 6) => &mut self.cdf.palette_6_y_color[ctx],
                    (0, 7) => &mut self.cdf.palette_7_y_color[ctx],
                    (0, _) => &mut self.cdf.palette_8_y_color[ctx],
                    (_, 2) => &mut self.cdf.palette_2_uv_color[ctx],
                    (_, 3) => &mut self.cdf.palette_3_uv_color[ctx],
                    (_, 4) => &mut self.cdf.palette_4_uv_color[ctx],
                    (_, 5) => &mut self.cdf.palette_5_uv_color[ctx],
                    (_, 6) => &mut self.cdf.palette_6_uv_color[ctx],
                    (_, 7) => &mut self.cdf.palette_7_uv_color[ctx],
                    _ => &mut self.cdf.palette_8_uv_color[ctx],
                };
                let s = self.sd.read_symbol(cdf);
                map[rr * 64 + cc] = order[s];
                j -= 1;
            }
        }
        for i in 0..on_h {
            for j in on_w..bw {
                map[i * 64 + j] = map[i * 64 + on_w - 1];
            }
        }
        for i in on_h..bh {
            for j in 0..bw {
                map[i * 64 + j] = map[(on_h - 1) * 64 + j];
            }
        }
    }
}

/// get_palette_color_context( ): returns (ColorOrder, ColorContextHash).
fn palette_color_context(map: &[u8], r: usize, c: usize, n: usize) -> ([u8; 8], usize) {
    let mut scores = [0i32; PALETTE_COLORS];
    let mut order = [0u8, 1, 2, 3, 4, 5, 6, 7];
    if c > 0 {
        scores[map[r * 64 + c - 1] as usize] += 2;
    }
    if r > 0 && c > 0 {
        scores[map[(r - 1) * 64 + c - 1] as usize] += 1;
    }
    if r > 0 {
        scores[map[(r - 1) * 64 + c] as usize] += 2;
    }
    for i in 0..PALETTE_NUM_NEIGHBORS {
        let mut max_score = scores[i];
        let mut max_idx = i;
        for j in i + 1..n {
            if scores[j] > max_score {
                max_score = scores[j];
                max_idx = j;
            }
        }
        if max_idx != i {
            let max_score = scores[max_idx];
            let max_order = order[max_idx];
            for k in (i + 1..=max_idx).rev() {
                scores[k] = scores[k - 1];
                order[k] = order[k - 1];
            }
            scores[i] = max_score;
            order[i] = max_order;
        }
    }
    let mut hash = 0usize;
    for i in 0..PALETTE_NUM_NEIGHBORS {
        hash += scores[i] as usize * PALETTE_COLOR_HASH_MULTIPLIERS[i] as usize;
    }
    (order, hash)
}

//! Motion vector prediction processes (spec 7.10): find_mv_stack, has_overlappable_candidates
//! and find_warp_samples.

use crate::spec_tables::{
    GLOBAL_GLOBALMV, GLOBALMV, IDENTITY, INTRA_FRAME, LEAST_SQUARES_SAMPLES_MAX, MAX_REF_MV_STACK_SIZE, MV_BORDER, NEAR_NEWMV, NEAREST_NEWMV, NEW_NEARESTMV,
    NEW_NEARMV, NEW_NEWMV, NEWMV, NUM_4X4_BLOCKS_HIGH, NUM_4X4_BLOCKS_WIDE, REF_CAT_LEVEL, TRANSLATION, WARPEDMODEL_PREC_BITS,
};
use crate::tile::TileDecoder;

pub(crate) type MvP = [i32; 2];
pub(crate) const INVALID_MV: i32 = -1 << 15;

/// Output of find_mv_stack plus its working state.
#[derive(Clone, Default)]
pub(crate) struct MvStack {
    pub ref_stack_mv: [[MvP; 2]; MAX_REF_MV_STACK_SIZE + 1],
    pub weight_stack: [u32; MAX_REF_MV_STACK_SIZE + 1],
    pub num_mv_found: usize,
    pub new_mv_count: usize,
    pub global_mvs: [MvP; 2],
    pub found_match: bool,
    pub close_matches: usize,
    pub total_matches: usize,
    pub new_mv_context: usize,
    pub ref_mv_context: usize,
    pub zero_mv_context: usize,
    pub drl_ctx_stack: [usize; MAX_REF_MV_STACK_SIZE + 1],
    ref_id_count: [usize; 2],
    ref_diff_count: [usize; 2],
    ref_id_mvs: [[MvP; 2]; 2],
    ref_diff_mvs: [[MvP; 2]; 2],
    // warp samples
    pub num_samples: usize,
    pub num_samples_scanned: usize,
    pub cand_list: [[i32; 4]; LEAST_SQUARES_SAMPLES_MAX],
}

fn block_width(bs: usize) -> i32 {
    4 * NUM_4X4_BLOCKS_WIDE[bs] as i32
}
fn block_height(bs: usize) -> i32 {
    4 * NUM_4X4_BLOCKS_HIGH[bs] as i32
}

#[inline]
pub(crate) fn round2_signed64(x: i64, n: u32) -> i64 {
    if n == 0 {
        return x;
    }
    if x >= 0 { (x + (1 << (n - 1))) >> n } else { -((-x + (1 << (n - 1))) >> n) }
}

pub(crate) fn has_newmv(mode: usize) -> bool {
    matches!(mode, NEWMV | NEW_NEWMV | NEAR_NEWMV | NEW_NEARMV | NEAREST_NEWMV | NEW_NEARESTMV)
}

impl TileDecoder<'_, '_> {
    /// lower_mv_precision( candMv ) (7.10.2.10)
    pub(crate) fn lower_mv_precision(&self, mv: &mut MvP) {
        let fh = &self.fs.fh;
        if fh.allow_high_precision_mv {
            return;
        }
        for c in mv.iter_mut() {
            if fh.force_integer_mv {
                let a = c.abs();
                let a_int = (a + 3) >> 3;
                *c = if *c > 0 { a_int << 3 } else { -(a_int << 3) };
            } else if *c & 1 != 0 {
                *c += if *c > 0 { -1 } else { 1 };
            }
        }
    }

    /// Setup global MV process (7.10.2.1).
    pub(crate) fn setup_global_mv(&self, ref_list: usize) -> MvP {
        let rf = self.b.ref_frame[ref_list];
        let fh = &self.fs.fh;
        let mut mv: MvP = [0, 0];
        if rf > INTRA_FRAME as i8 {
            let rf = rf as usize;
            let typ = fh.gm_type[rf] as usize;
            let gm = &fh.gm_params[rf];
            if typ == TRANSLATION {
                mv = [gm[0] >> (WARPEDMODEL_PREC_BITS - 3), gm[1] >> (WARPEDMODEL_PREC_BITS - 3)];
            } else if typ != IDENTITY {
                let bw = block_width(self.b.mi_size) as i64;
                let bh = block_height(self.b.mi_size) as i64;
                let x = self.b.mi_col as i64 * 4 + bw / 2 - 1;
                let y = self.b.mi_row as i64 * 4 + bh / 2 - 1;
                let p = 1i64 << WARPEDMODEL_PREC_BITS;
                let xc = (gm[2] as i64 - p) * x + gm[3] as i64 * y + gm[0] as i64;
                let yc = gm[4] as i64 * x + (gm[5] as i64 - p) * y + gm[1] as i64;
                if fh.allow_high_precision_mv {
                    mv = [round2_signed64(yc, WARPEDMODEL_PREC_BITS as u32 - 3) as i32, round2_signed64(xc, WARPEDMODEL_PREC_BITS as u32 - 3) as i32];
                } else {
                    mv = [round2_signed64(yc, WARPEDMODEL_PREC_BITS as u32 - 2) as i32 * 2, round2_signed64(xc, WARPEDMODEL_PREC_BITS as u32 - 2) as i32 * 2];
                }
            }
        }
        self.lower_mv_precision(&mut mv);
        mv
    }

    /// Find MV stack process (7.10.2).
    pub(crate) fn find_mv_stack(&mut self, is_compound: bool) {
        let bw4 = self.b.bw4 as isize;
        let bh4 = self.b.bh4 as isize;
        let mut s = MvStack { global_mvs: [self.setup_global_mv(0), [0, 0]], ..Default::default() };
        if is_compound {
            s.global_mvs[1] = self.setup_global_mv(1);
        }
        self.mvs = s;
        self.mvs.found_match = false;
        self.scan_row(-1, is_compound);
        let mut found_above = self.mvs.found_match;
        self.mvs.found_match = false;
        self.scan_col(-1, is_compound);
        let mut found_left = self.mvs.found_match;
        self.mvs.found_match = false;
        if bw4.max(bh4) <= 16 {
            self.scan_point(-1, bw4, is_compound);
        }
        if self.mvs.found_match {
            found_above = true;
        }
        self.mvs.close_matches = found_above as usize + found_left as usize;
        let num_nearest = self.mvs.num_mv_found;
        let num_new = self.mvs.new_mv_count;
        for idx in 0..num_nearest {
            self.mvs.weight_stack[idx] += REF_CAT_LEVEL as u32;
        }
        self.mvs.zero_mv_context = 0;
        if self.fs.fh.use_ref_frame_mvs {
            self.temporal_scan(is_compound);
        }
        self.scan_point(-1, -1, is_compound);
        if self.mvs.found_match {
            found_above = true;
        }
        self.mvs.found_match = false;
        self.scan_row(-3, is_compound);
        if self.mvs.found_match {
            found_above = true;
        }
        self.mvs.found_match = false;
        self.scan_col(-3, is_compound);
        if self.mvs.found_match {
            found_left = true;
        }
        self.mvs.found_match = false;
        if bh4 > 1 {
            self.scan_row(-5, is_compound);
        }
        if self.mvs.found_match {
            found_above = true;
        }
        self.mvs.found_match = false;
        if bw4 > 1 {
            self.scan_col(-5, is_compound);
        }
        if self.mvs.found_match {
            found_left = true;
        }
        self.mvs.total_matches = found_above as usize + found_left as usize;
        let n = self.mvs.num_mv_found;
        self.sort_stack(0, num_nearest, is_compound);
        self.sort_stack(num_nearest, n, is_compound);
        if self.mvs.num_mv_found < 2 {
            self.extra_search(is_compound);
        }
        self.context_and_clamping(is_compound, num_new);
    }

    fn scan_row(&mut self, delta_row: isize, is_compound: bool) {
        let bw4 = self.b.bw4 as isize;
        let (mi_row, mi_col) = (self.b.mi_row as isize, self.b.mi_col as isize);
        let mi_cols = self.fs.fh.mi_cols as isize;
        let end4 = bw4.min(mi_cols - mi_col).min(16);
        let mut delta_col = 0;
        let use_step16 = bw4 >= 16;
        let mut delta_row = delta_row;
        if delta_row.abs() > 1 {
            delta_row += mi_row & 1;
            delta_col = 1 - (mi_col & 1);
        }
        let mut i = 0;
        while i < end4 {
            let mv_row = mi_row + delta_row;
            let mv_col = mi_col + delta_col + i;
            if !self.inside(mv_row, mv_col) {
                break;
            }
            let mi = &self.t.mi;
            let mut len = bw4.min(NUM_4X4_BLOCKS_WIDE[mi.mi_size[mi.idx(mv_row as usize, mv_col as usize)] as usize] as isize);
            if delta_row.abs() > 1 {
                len = len.max(2);
            }
            if use_step16 {
                len = len.max(4);
            }
            let weight = len as u32 * 2;
            self.add_ref_mv_candidate(mv_row as usize, mv_col as usize, is_compound, weight);
            i += len;
        }
    }

    fn scan_col(&mut self, delta_col: isize, is_compound: bool) {
        let bh4 = self.b.bh4 as isize;
        let (mi_row, mi_col) = (self.b.mi_row as isize, self.b.mi_col as isize);
        let mi_rows = self.fs.fh.mi_rows as isize;
        let end4 = bh4.min(mi_rows - mi_row).min(16);
        let mut delta_row = 0;
        let use_step16 = bh4 >= 16;
        let mut delta_col = delta_col;
        if delta_col.abs() > 1 {
            delta_row = 1 - (mi_row & 1);
            delta_col += mi_col & 1;
        }
        let mut i = 0;
        while i < end4 {
            let mv_row = mi_row + delta_row + i;
            let mv_col = mi_col + delta_col;
            if !self.inside(mv_row, mv_col) {
                break;
            }
            let mi = &self.t.mi;
            let mut len = bh4.min(NUM_4X4_BLOCKS_HIGH[mi.mi_size[mi.idx(mv_row as usize, mv_col as usize)] as usize] as isize);
            if delta_col.abs() > 1 {
                len = len.max(2);
            }
            if use_step16 {
                len = len.max(4);
            }
            let weight = len as u32 * 2;
            self.add_ref_mv_candidate(mv_row as usize, mv_col as usize, is_compound, weight);
            i += len;
        }
    }

    fn scan_point(&mut self, delta_row: isize, delta_col: isize, is_compound: bool) {
        let mv_row = self.b.mi_row as isize + delta_row;
        let mv_col = self.b.mi_col as isize + delta_col;
        if self.inside(mv_row, mv_col) && self.t.mi.written[self.t.mi.idx(mv_row as usize, mv_col as usize)] {
            self.add_ref_mv_candidate(mv_row as usize, mv_col as usize, is_compound, 4);
        }
    }

    fn temporal_scan(&mut self, is_compound: bool) {
        let bw4 = self.b.bw4 as isize;
        let bh4 = self.b.bh4 as isize;
        let step_w4 = if bw4 >= 16 { 4 } else { 2 };
        let step_h4 = if bh4 >= 16 { 4 } else { 2 };
        let mut dr = 0;
        while dr < bh4.min(16) {
            let mut dc = 0;
            while dc < bw4.min(16) {
                self.add_tpl_ref_mv(dr, dc, is_compound);
                dc += step_w4;
            }
            dr += step_h4;
        }
        let allow_extension = (2..16).contains(&bh4) && (2..16).contains(&bw4);
        if allow_extension {
            let pos = [(bh4, -2), (bh4, bw4), (bh4 - 2, bw4)];
            for &(dr, dc) in &pos {
                let row = (self.b.mi_row as isize & 15) + dr;
                let col = (self.b.mi_col as isize & 15) + dc;
                if (0..16).contains(&row) && (0..16).contains(&col) {
                    self.add_tpl_ref_mv(dr, dc, is_compound);
                }
            }
        }
    }

    fn add_tpl_ref_mv(&mut self, delta_row: isize, delta_col: isize, is_compound: bool) {
        let mv_row = (self.b.mi_row as isize + delta_row) | 1;
        let mv_col = (self.b.mi_col as isize + delta_col) | 1;
        if !self.inside(mv_row, mv_col) {
            return;
        }
        let x8 = (mv_col >> 1) as usize;
        let y8 = (mv_row >> 1) as usize;
        let w8 = self.fs.fh.mi_cols as usize >> 1;
        if delta_row == 0 && delta_col == 0 {
            self.mvs.zero_mv_context = 1;
        }
        let rf0 = self.b.ref_frame[0] as usize;
        if !is_compound {
            let mut cand = self.fs.motion_field[rf0][y8 * w8 + x8];
            if cand[0] == INVALID_MV {
                return;
            }
            self.lower_mv_precision(&mut cand);
            if delta_row == 0 && delta_col == 0 {
                let g = self.mvs.global_mvs[0];
                self.mvs.zero_mv_context = ((cand[0] - g[0]).abs() >= 16 || (cand[1] - g[1]).abs() >= 16) as usize;
            }
            let n = self.mvs.num_mv_found;
            let idx = (0..n).find(|&i| self.mvs.ref_stack_mv[i][0] == cand);
            if let Some(i) = idx {
                self.mvs.weight_stack[i] += 2;
            } else if n < MAX_REF_MV_STACK_SIZE {
                self.mvs.ref_stack_mv[n][0] = cand;
                self.mvs.weight_stack[n] = 2;
                self.mvs.num_mv_found += 1;
            }
        } else {
            let rf1 = self.b.ref_frame[1] as usize;
            let mut c0 = self.fs.motion_field[rf0][y8 * w8 + x8];
            if c0[0] == INVALID_MV {
                return;
            }
            let mut c1 = self.fs.motion_field[rf1][y8 * w8 + x8];
            if c1[0] == INVALID_MV {
                return;
            }
            self.lower_mv_precision(&mut c0);
            self.lower_mv_precision(&mut c1);
            if delta_row == 0 && delta_col == 0 {
                let g = self.mvs.global_mvs;
                self.mvs.zero_mv_context =
                    ((c0[0] - g[0][0]).abs() >= 16 || (c0[1] - g[0][1]).abs() >= 16 || (c1[0] - g[1][0]).abs() >= 16 || (c1[1] - g[1][1]).abs() >= 16) as usize;
            }
            let n = self.mvs.num_mv_found;
            let idx = (0..n).find(|&i| self.mvs.ref_stack_mv[i][0] == c0 && self.mvs.ref_stack_mv[i][1] == c1);
            if let Some(i) = idx {
                self.mvs.weight_stack[i] += 2;
            } else if n < MAX_REF_MV_STACK_SIZE {
                self.mvs.ref_stack_mv[n] = [c0, c1];
                self.mvs.weight_stack[n] = 2;
                self.mvs.num_mv_found += 1;
            }
        }
    }

    fn add_ref_mv_candidate(&mut self, mv_row: usize, mv_col: usize, is_compound: bool, weight: u32) {
        let mi = &self.t.mi;
        let i = mi.idx(mv_row, mv_col);
        if !mi.is_inter[i] {
            return;
        }
        let rf = mi.ref_frame[i];
        if !is_compound {
            for cand_list in 0..2 {
                if rf[cand_list] == self.b.ref_frame[0] {
                    self.search_stack(mv_row, mv_col, cand_list, weight);
                }
            }
        } else if rf[0] == self.b.ref_frame[0] && rf[1] == self.b.ref_frame[1] {
            self.compound_search_stack(mv_row, mv_col, weight);
        }
    }

    fn search_stack(&mut self, mv_row: usize, mv_col: usize, cand_list: usize, weight: u32) {
        let mi = &self.t.mi;
        let i = mi.idx(mv_row, mv_col);
        let cand_mode = mi.y_mode[i] as usize;
        let cand_size = mi.mi_size[i] as usize;
        let large = block_width(cand_size).min(block_height(cand_size)) >= 8;
        let gm0 = self.fs.fh.gm_type[self.b.ref_frame[0] as usize] as usize;
        let mut cand = if (cand_mode == GLOBALMV || cand_mode == GLOBAL_GLOBALMV) && gm0 > TRANSLATION && large {
            self.mvs.global_mvs[0]
        } else {
            let m = mi.mv[i][cand_list];
            [m.row as i32, m.col as i32]
        };
        self.lower_mv_precision(&mut cand);
        if has_newmv(cand_mode) {
            self.mvs.new_mv_count += 1;
        }
        self.mvs.found_match = true;
        let n = self.mvs.num_mv_found;
        if let Some(idx) = (0..n).find(|&k| self.mvs.ref_stack_mv[k][0] == cand) {
            self.mvs.weight_stack[idx] += weight;
        } else if n < MAX_REF_MV_STACK_SIZE {
            self.mvs.ref_stack_mv[n][0] = cand;
            self.mvs.weight_stack[n] = weight;
            self.mvs.num_mv_found += 1;
        }
    }

    fn compound_search_stack(&mut self, mv_row: usize, mv_col: usize, weight: u32) {
        let mi = &self.t.mi;
        let i = mi.idx(mv_row, mv_col);
        let mut cand = [[mi.mv[i][0].row as i32, mi.mv[i][0].col as i32], [mi.mv[i][1].row as i32, mi.mv[i][1].col as i32]];
        let cand_mode = mi.y_mode[i] as usize;
        if cand_mode == GLOBAL_GLOBALMV {
            for (rl, c) in cand.iter_mut().enumerate() {
                if self.fs.fh.gm_type[self.b.ref_frame[rl] as usize] as usize > TRANSLATION {
                    *c = self.mvs.global_mvs[rl];
                }
            }
        }
        for c in cand.iter_mut() {
            self.lower_mv_precision(c);
        }
        self.mvs.found_match = true;
        let n = self.mvs.num_mv_found;
        if let Some(idx) = (0..n).find(|&k| self.mvs.ref_stack_mv[k][0] == cand[0] && self.mvs.ref_stack_mv[k][1] == cand[1]) {
            self.mvs.weight_stack[idx] += weight;
        } else if n < MAX_REF_MV_STACK_SIZE {
            self.mvs.ref_stack_mv[n] = cand;
            self.mvs.weight_stack[n] = weight;
            self.mvs.num_mv_found += 1;
        }
        if has_newmv(cand_mode) {
            self.mvs.new_mv_count += 1;
        }
    }

    fn sort_stack(&mut self, start: usize, end: usize, is_compound: bool) {
        let mut end = end;
        let s = &mut self.mvs;
        while end > start {
            let mut new_end = start;
            for idx in start + 1..end {
                if s.weight_stack[idx - 1] < s.weight_stack[idx] {
                    s.weight_stack.swap(idx - 1, idx);
                    for list in 0..1 + is_compound as usize {
                        let t = s.ref_stack_mv[idx - 1][list];
                        s.ref_stack_mv[idx - 1][list] = s.ref_stack_mv[idx][list];
                        s.ref_stack_mv[idx][list] = t;
                    }
                    new_end = idx;
                }
            }
            end = new_end;
        }
    }

    fn extra_search(&mut self, is_compound: bool) {
        self.mvs.ref_id_count = [0; 2];
        self.mvs.ref_diff_count = [0; 2];
        let (mi_row, mi_col) = (self.b.mi_row as isize, self.b.mi_col as isize);
        let mut w4 = (self.b.bw4 as isize).min(16);
        let mut h4 = (self.b.bh4 as isize).min(16);
        w4 = w4.min(self.fs.fh.mi_cols as isize - mi_col);
        h4 = h4.min(self.fs.fh.mi_rows as isize - mi_row);
        let num4x4 = w4.min(h4);
        for pass in 0..2 {
            let mut idx = 0;
            while idx < num4x4 && self.mvs.num_mv_found < 2 {
                let (mv_row, mv_col) = if pass == 0 { (mi_row - 1, mi_col + idx) } else { (mi_row + idx, mi_col - 1) };
                if !self.inside(mv_row, mv_col) {
                    break;
                }
                self.add_extra_mv_candidate(mv_row as usize, mv_col as usize, is_compound);
                let mi = &self.t.mi;
                let sz = mi.mi_size[mi.idx(mv_row as usize, mv_col as usize)] as usize;
                idx += if pass == 0 { NUM_4X4_BLOCKS_WIDE[sz] } else { NUM_4X4_BLOCKS_HIGH[sz] } as isize;
            }
        }
        if is_compound {
            let mut combined = [[[0i32; 2]; 2]; 2];
            for list in 0..2 {
                let mut comp_count = 0;
                for idx in 0..self.mvs.ref_id_count[list] {
                    combined[comp_count][list] = self.mvs.ref_id_mvs[list][idx];
                    comp_count += 1;
                }
                let mut idx = 0;
                while idx < self.mvs.ref_diff_count[list] && comp_count < 2 {
                    combined[comp_count][list] = self.mvs.ref_diff_mvs[list][idx];
                    comp_count += 1;
                    idx += 1;
                }
                while comp_count < 2 {
                    combined[comp_count][list] = self.mvs.global_mvs[list];
                    comp_count += 1;
                }
            }
            let n = self.mvs.num_mv_found;
            if n == 1 {
                if combined[0][0] == self.mvs.ref_stack_mv[0][0] && combined[0][1] == self.mvs.ref_stack_mv[0][1] {
                    self.mvs.ref_stack_mv[n] = combined[1];
                } else {
                    self.mvs.ref_stack_mv[n] = combined[0];
                }
                self.mvs.weight_stack[n] = 2;
                self.mvs.num_mv_found += 1;
            } else {
                for c in combined {
                    let n = self.mvs.num_mv_found;
                    self.mvs.ref_stack_mv[n] = c;
                    self.mvs.weight_stack[n] = 2;
                    self.mvs.num_mv_found += 1;
                }
            }
        } else {
            for idx in self.mvs.num_mv_found..2 {
                self.mvs.ref_stack_mv[idx][0] = self.mvs.global_mvs[0];
            }
        }
    }

    fn add_extra_mv_candidate(&mut self, mv_row: usize, mv_col: usize, is_compound: bool) {
        let mi = &self.t.mi;
        let i = mi.idx(mv_row, mv_col);
        let rfs = mi.ref_frame[i];
        let mvs = mi.mv[i];
        let bias = &self.fs.fh.ref_frame_sign_bias;
        if is_compound {
            for cand_list in 0..2 {
                let cand_ref = rfs[cand_list];
                if cand_ref > INTRA_FRAME as i8 {
                    for list in 0..2 {
                        let mut cand = [mvs[cand_list].row as i32, mvs[cand_list].col as i32];
                        if cand_ref == self.b.ref_frame[list] && self.mvs.ref_id_count[list] < 2 {
                            let k = self.mvs.ref_id_count[list];
                            self.mvs.ref_id_mvs[list][k] = cand;
                            self.mvs.ref_id_count[list] += 1;
                        } else if self.mvs.ref_diff_count[list] < 2 {
                            if bias[cand_ref as usize] != bias[self.b.ref_frame[list] as usize] {
                                cand = [-cand[0], -cand[1]];
                            }
                            let k = self.mvs.ref_diff_count[list];
                            self.mvs.ref_diff_mvs[list][k] = cand;
                            self.mvs.ref_diff_count[list] += 1;
                        }
                    }
                }
            }
        } else {
            for cand_list in 0..2 {
                let cand_ref = rfs[cand_list];
                if cand_ref > INTRA_FRAME as i8 {
                    let mut cand = [mvs[cand_list].row as i32, mvs[cand_list].col as i32];
                    if bias[cand_ref as usize] != bias[self.b.ref_frame[0] as usize] {
                        cand = [-cand[0], -cand[1]];
                    }
                    let n = self.mvs.num_mv_found;
                    if !(0..n).any(|k| self.mvs.ref_stack_mv[k][0] == cand) {
                        self.mvs.ref_stack_mv[n][0] = cand;
                        self.mvs.weight_stack[n] = 2;
                        self.mvs.num_mv_found += 1;
                    }
                }
            }
        }
    }

    fn context_and_clamping(&mut self, is_compound: bool, num_new: usize) {
        let bw = block_width(self.b.mi_size);
        let bh = block_height(self.b.mi_size);
        let n = self.mvs.num_mv_found;
        for idx in 0..n {
            let mut z = 0;
            if idx + 1 < n {
                let w0 = self.mvs.weight_stack[idx];
                let w1 = self.mvs.weight_stack[idx + 1];
                if w0 >= REF_CAT_LEVEL as u32 {
                    if w1 < REF_CAT_LEVEL as u32 {
                        z = 1;
                    }
                } else {
                    z = 2;
                }
            }
            self.mvs.drl_ctx_stack[idx] = z;
        }
        for list in 0..1 + is_compound as usize {
            for idx in 0..n {
                let mut mv = self.mvs.ref_stack_mv[idx][list];
                mv[0] = self.clamp_mv_row(mv[0], MV_BORDER as i32 + bh * 8);
                mv[1] = self.clamp_mv_col(mv[1], MV_BORDER as i32 + bw * 8);
                self.mvs.ref_stack_mv[idx][list] = mv;
            }
        }
        let (close, total) = (self.mvs.close_matches, self.mvs.total_matches);
        if close == 0 {
            self.mvs.new_mv_context = total.min(1);
            self.mvs.ref_mv_context = total;
        } else if close == 1 {
            self.mvs.new_mv_context = 3 - num_new.min(1);
            self.mvs.ref_mv_context = 2 + total;
        } else {
            self.mvs.new_mv_context = 5 - num_new.min(1);
            self.mvs.ref_mv_context = 5;
        }
    }

    pub(crate) fn clamp_mv_row(&self, mvec: i32, border: i32) -> i32 {
        let bh4 = self.b.bh4 as i32;
        let to_top = -((self.b.mi_row as i32 * 4) * 8);
        let to_bottom = ((self.fs.fh.mi_rows as i32 - bh4 - self.b.mi_row as i32) * 4) * 8;
        mvec.clamp(to_top - border, to_bottom + border)
    }

    pub(crate) fn clamp_mv_col(&self, mvec: i32, border: i32) -> i32 {
        let bw4 = self.b.bw4 as i32;
        let to_left = -((self.b.mi_col as i32 * 4) * 8);
        let to_right = ((self.fs.fh.mi_cols as i32 - bw4 - self.b.mi_col as i32) * 4) * 8;
        mvec.clamp(to_left - border, to_right + border)
    }

    /// has_overlappable_candidates( ) (7.10.3)
    pub(crate) fn has_overlappable_candidates(&self) -> bool {
        let mi = &self.t.mi;
        let (mi_rows, mi_cols) = (self.fs.fh.mi_rows as usize, self.fs.fh.mi_cols as usize);
        if self.b.avail_u {
            let mut x4 = self.b.mi_col;
            while x4 < mi_cols.min(self.b.mi_col + self.b.bw4) {
                let c = (x4 | 1).min(mi_cols - 1);
                if mi.ref_frame[mi.idx(self.b.mi_row - 1, c)][0] > INTRA_FRAME as i8 {
                    return true;
                }
                x4 += 2;
            }
        }
        if self.b.avail_l {
            let mut y4 = self.b.mi_row;
            while y4 < mi_rows.min(self.b.mi_row + self.b.bh4) {
                let r = (y4 | 1).min(mi_rows - 1);
                if mi.ref_frame[mi.idx(r, self.b.mi_col - 1)][0] > INTRA_FRAME as i8 {
                    return true;
                }
                y4 += 2;
            }
        }
        false
    }

    /// Find warp samples process (7.10.4).
    pub(crate) fn find_warp_samples(&mut self) {
        self.mvs.num_samples = 0;
        self.mvs.num_samples_scanned = 0;
        let w4 = self.b.bw4 as isize;
        let h4 = self.b.bh4 as isize;
        let (mi_row, mi_col) = (self.b.mi_row as isize, self.b.mi_col as isize);
        let (mi_rows, mi_cols) = (self.fs.fh.mi_rows as isize, self.fs.fh.mi_cols as isize);
        let mut do_top_left = true;
        let mut do_top_right = true;
        if self.b.avail_u {
            let mi = &self.t.mi;
            let src_w = NUM_4X4_BLOCKS_WIDE[mi.mi_size[mi.idx(mi_row as usize - 1, mi_col as usize)] as usize] as isize;
            if w4 <= src_w {
                let col_offset = -(mi_col & (src_w - 1));
                if col_offset < 0 {
                    do_top_left = false;
                }
                if col_offset + src_w > w4 {
                    do_top_right = false;
                }
                self.add_sample(-1, 0);
            } else {
                let mut i = 0;
                while i < w4.min(mi_cols - mi_col) {
                    let mi = &self.t.mi;
                    let src_w = NUM_4X4_BLOCKS_WIDE[mi.mi_size[mi.idx(mi_row as usize - 1, (mi_col + i) as usize)] as usize] as isize;
                    let step = w4.min(src_w);
                    self.add_sample(-1, i);
                    i += step;
                }
            }
        }
        if self.b.avail_l {
            let mi = &self.t.mi;
            let src_h = NUM_4X4_BLOCKS_HIGH[mi.mi_size[mi.idx(mi_row as usize, mi_col as usize - 1)] as usize] as isize;
            if h4 <= src_h {
                let row_offset = -(mi_row & (src_h - 1));
                if row_offset < 0 {
                    do_top_left = false;
                }
                self.add_sample(0, -1);
            } else {
                let mut i = 0;
                while i < h4.min(mi_rows - mi_row) {
                    let mi = &self.t.mi;
                    let src_h = NUM_4X4_BLOCKS_HIGH[mi.mi_size[mi.idx((mi_row + i) as usize, mi_col as usize - 1)] as usize] as isize;
                    let step = h4.min(src_h);
                    self.add_sample(i, -1);
                    i += step;
                }
            }
        }
        if do_top_left {
            self.add_sample(-1, -1);
        }
        if do_top_right && w4.max(h4) <= 16 {
            self.add_sample(-1, w4);
        }
        if self.mvs.num_samples == 0 && self.mvs.num_samples_scanned > 0 {
            self.mvs.num_samples = 1;
        }
    }

    fn add_sample(&mut self, delta_row: isize, delta_col: isize) {
        if self.mvs.num_samples_scanned >= LEAST_SQUARES_SAMPLES_MAX {
            return;
        }
        let mv_row = self.b.mi_row as isize + delta_row;
        let mv_col = self.b.mi_col as isize + delta_col;
        if !self.inside(mv_row, mv_col) {
            return;
        }
        let mi = &self.t.mi;
        let i = mi.idx(mv_row as usize, mv_col as usize);
        if !mi.written[i] {
            return;
        }
        if mi.ref_frame[i][0] != self.b.ref_frame[0] || mi.ref_frame[i][1] != -1 {
            return;
        }
        let cand_sz = mi.mi_size[i] as usize;
        let cand_w4 = NUM_4X4_BLOCKS_WIDE[cand_sz] as isize;
        let cand_h4 = NUM_4X4_BLOCKS_HIGH[cand_sz] as isize;
        let cand_row = mv_row & !(cand_h4 - 1);
        let cand_col = mv_col & !(cand_w4 - 1);
        let mid_y = cand_row * 4 + cand_h4 * 2 - 1;
        let mid_x = cand_col * 4 + cand_w4 * 2 - 1;
        let threshold = block_width(self.b.mi_size).max(block_height(self.b.mi_size)).clamp(16, 112);
        let cm = mi.mv[mi.idx(cand_row as usize, cand_col as usize)][0];
        let mv_diff_row = (cm.row as i32 - self.b.mv_i[0][0]).abs();
        let mv_diff_col = (cm.col as i32 - self.b.mv_i[0][1]).abs();
        let valid = mv_diff_row + mv_diff_col <= threshold;
        let cand = [mid_y as i32 * 8, mid_x as i32 * 8, mid_y as i32 * 8 + cm.row as i32, mid_x as i32 * 8 + cm.col as i32];
        self.mvs.num_samples_scanned += 1;
        if !valid && self.mvs.num_samples_scanned > 1 {
            return;
        }
        self.mvs.cand_list[self.mvs.num_samples] = cand;
        if valid {
            self.mvs.num_samples += 1;
        }
    }

    #[inline]
    pub(crate) fn inside(&self, r: isize, c: isize) -> bool {
        c >= self.mi_col_start as isize && c < self.mi_col_end as isize && r >= self.mi_row_start as isize && r < self.mi_row_end as isize
    }
}

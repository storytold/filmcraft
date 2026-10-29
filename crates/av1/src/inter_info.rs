//! Inter frame mode info syntax (5.11.7 / 5.11.18 – 5.11.33) with its CDF selection (8.3.2).

use crate::Result;
use crate::header::{EIGHTTAP, SWITCHABLE};
use crate::mvpred::MvP;
use crate::spec_tables::{
    ALTREF_FRAME, ALTREF2_FRAME, BLOCK_8X8, BLOCK_32X32, BLOCK_64X64, BLOCK_128X128, BWDREF_FRAME, CLASS0_SIZE, COMP_NEWMV_CTXS, COMPOUND_AVERAGE,
    COMPOUND_DIFFWTD, COMPOUND_DISTANCE, COMPOUND_INTRA, COMPOUND_MODE_CTX_MAP, COMPOUND_REFERENCE, COMPOUND_WEDGE, GLOBAL_GLOBALMV, GLOBALMV, GOLDEN_FRAME,
    INTRA_FRAME, INTRABC_DELAY_PIXELS, LAST_FRAME, LAST2_FRAME, LAST3_FRAME, LOCALWARP, MV_CLASS_0, MV_INTRABC_CONTEXT, MV_JOINT_HNZVNZ, MV_JOINT_HNZVZ,
    MV_JOINT_HZVNZ, NEAR_NEARMV, NEAR_NEWMV, NEAREST_NEARESTMV, NEAREST_NEWMV, NEARESTMV, NEARMV, NEW_NEARESTMV, NEW_NEARMV, NEW_NEWMV, NEWMV,
    NUM_4X4_BLOCKS_HIGH, NUM_4X4_BLOCKS_WIDE, OBMC, REF_SCALE_SHIFT, SEG_LVL_GLOBALMV, SEG_LVL_REF_FRAME, SEG_LVL_SKIP, SIMPLE, SINGLE_REFERENCE, SIZE_GROUP,
    TRANSLATION, UNIDIR_COMP_REFERENCE, UV_CFL_PRED, WEDGE_BITS,
};
use crate::tile::TileDecoder;

fn block_width(bs: usize) -> usize {
    4 * NUM_4X4_BLOCKS_WIDE[bs] as usize
}
fn block_height(bs: usize) -> usize {
    4 * NUM_4X4_BLOCKS_HIGH[bs] as usize
}

/// Neighbour reference frame summary (LeftRefFrame / AboveRefFrame and the derived flags).
#[derive(Clone, Copy, Default)]
pub(crate) struct Neighbours {
    pub left: [i8; 2],
    pub above: [i8; 2],
    pub left_intra: bool,
    pub above_intra: bool,
    pub left_single: bool,
    pub above_single: bool,
}

impl TileDecoder<'_, '_> {
    pub(crate) fn inter_frame_mode_info_impl(&mut self) -> Result<()> {
        self.b.use_intrabc = false;
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let mi = &self.t.mi;
        let mut n = Neighbours {
            left: if self.b.avail_l { mi.ref_frame[mi.idx(r, c - 1)] } else { [INTRA_FRAME as i8, -1] },
            above: if self.b.avail_u { mi.ref_frame[mi.idx(r - 1, c)] } else { [INTRA_FRAME as i8, -1] },
            ..Default::default()
        };
        if !self.b.avail_l {
            n.left[1] = -1;
        }
        if !self.b.avail_u {
            n.above[1] = -1;
        }
        n.left_intra = n.left[0] <= INTRA_FRAME as i8;
        n.above_intra = n.above[0] <= INTRA_FRAME as i8;
        n.left_single = n.left[1] <= INTRA_FRAME as i8;
        n.above_single = n.above[1] <= INTRA_FRAME as i8;
        self.nb = n;
        self.b.skip = false;
        self.inter_segment_id(true);
        self.read_skip_mode();
        if self.b.skip_mode {
            self.b.skip = true;
        } else {
            self.read_skip();
        }
        if !self.fs.fh.seg.seg_id_pre_skip {
            self.inter_segment_id(false);
        }
        self.b.lossless = self.fs.fh.lossless_array[self.b.segment_id];
        self.read_cdef();
        self.read_delta_qindex();
        self.read_delta_lf();
        self.read_deltas = false;
        self.read_is_inter();
        if self.b.is_inter { self.inter_block_mode_info() } else { self.intra_block_mode_info() }
    }

    fn get_segment_id(&self) -> usize {
        let (mi_rows, mi_cols) = (self.fs.fh.mi_rows as usize, self.fs.fh.mi_cols as usize);
        let x_mis = (mi_cols - self.b.mi_col).min(self.b.bw4);
        let y_mis = (mi_rows - self.b.mi_row).min(self.b.bh4);
        let mut seg = 7u8;
        for y in 0..y_mis {
            for x in 0..x_mis {
                let i = (self.b.mi_row + y) * self.t.mi.cols + self.b.mi_col + x;
                seg = seg.min(self.fs.prev_segment_ids[i]);
            }
        }
        seg as usize
    }

    fn set_seg_pred_context(&mut self, v: u8) {
        for i in 0..self.b.bw4 {
            if self.b.mi_col + i < self.t.above_seg_pred.len() {
                self.t.above_seg_pred[self.b.mi_col + i] = v;
            }
        }
        for i in 0..self.b.bh4 {
            if self.b.mi_row + i < self.t.left_seg_pred.len() {
                self.t.left_seg_pred[self.b.mi_row + i] = v;
            }
        }
    }

    fn inter_segment_id(&mut self, pre_skip: bool) {
        let seg = &self.fs.fh.seg;
        if !seg.enabled {
            self.b.segment_id = 0;
            return;
        }
        let predicted = self.get_segment_id();
        if seg.update_map {
            if pre_skip && !seg.seg_id_pre_skip {
                self.b.segment_id = 0;
                return;
            }
            if !pre_skip && self.b.skip {
                self.set_seg_pred_context(0);
                self.read_segment_id();
                return;
            }
            if seg.temporal_update {
                let ctx = self.t.left_seg_pred[self.b.mi_row] as usize + self.t.above_seg_pred[self.b.mi_col] as usize;
                let pred_flag = self.sd.read_symbol(&mut self.cdf.segment_id_predicted[ctx]);
                if pred_flag == 1 {
                    self.b.segment_id = predicted;
                } else {
                    self.read_segment_id();
                }
                self.set_seg_pred_context(pred_flag as u8);
            } else {
                self.read_segment_id();
            }
        } else {
            self.b.segment_id = predicted;
        }
    }

    fn read_skip_mode(&mut self) {
        let ms = self.b.mi_size;
        if self.seg_active(SEG_LVL_SKIP)
            || self.seg_active(SEG_LVL_REF_FRAME)
            || self.seg_active(SEG_LVL_GLOBALMV)
            || !self.fs.fh.skip_mode_present
            || block_width(ms) < 8
            || block_height(ms) < 8
        {
            self.b.skip_mode = false;
        } else {
            let (r, c) = (self.b.mi_row, self.b.mi_col);
            let mi = &self.t.mi;
            let mut ctx = 0;
            if self.b.avail_u {
                ctx += mi.skip_mode[mi.idx(r - 1, c)] as usize;
            }
            if self.b.avail_l {
                ctx += mi.skip_mode[mi.idx(r, c - 1)] as usize;
            }
            self.b.skip_mode = self.sd.read_symbol(&mut self.cdf.skip_mode[ctx]) == 1;
        }
    }

    pub(crate) fn seg_active(&self, feature: usize) -> bool {
        self.fs.fh.seg.enabled && self.fs.fh.seg.features.enabled[self.b.segment_id][feature]
    }

    fn read_is_inter(&mut self) {
        if self.b.skip_mode {
            self.b.is_inter = true;
        } else if self.seg_active(SEG_LVL_REF_FRAME) {
            self.b.is_inter = self.fs.fh.seg.features.data[self.b.segment_id][SEG_LVL_REF_FRAME] != INTRA_FRAME as i32;
        } else if self.seg_active(SEG_LVL_GLOBALMV) {
            self.b.is_inter = true;
        } else {
            let n = self.nb;
            let ctx = if self.b.avail_u && self.b.avail_l {
                if n.left_intra && n.above_intra { 3 } else { (n.left_intra || n.above_intra) as usize }
            } else if self.b.avail_u || self.b.avail_l {
                2 * (if self.b.avail_u { n.above_intra } else { n.left_intra }) as usize
            } else {
                0
            };
            self.b.is_inter = self.sd.read_symbol(&mut self.cdf.is_inter[ctx]) == 1;
        }
    }

    fn intra_block_mode_info(&mut self) -> Result<()> {
        self.b.ref_frame = [INTRA_FRAME as i8, -1];
        let ctx = SIZE_GROUP[self.b.mi_size] as usize;
        self.b.y_mode = self.sd.read_symbol(&mut self.cdf.y_mode[ctx]);
        self.intra_angle_info_y();
        if self.b.has_chroma {
            self.read_uv_mode();
            if self.b.uv_mode == UV_CFL_PRED {
                self.read_cfl_alphas();
            }
            self.intra_angle_info_uv();
        }
        self.b.palette_size_y = 0;
        self.b.palette_size_uv = 0;
        let ms = self.b.mi_size;
        if ms >= BLOCK_8X8 && block_width(ms) <= 64 && block_height(ms) <= 64 && self.fs.fh.allow_screen_content_tools {
            self.palette_mode_info();
        }
        self.filter_intra_mode_info();
        Ok(())
    }

    fn count_refs(&self, frame_type: i8) -> usize {
        let n = self.nb;
        let mut c = 0;
        if self.b.avail_u {
            c += (n.above[0] == frame_type) as usize + (n.above[1] == frame_type) as usize;
        }
        if self.b.avail_l {
            c += (n.left[0] == frame_type) as usize + (n.left[1] == frame_type) as usize;
        }
        c
    }

    fn ref_count_ctx(c0: usize, c1: usize) -> usize {
        match c0.cmp(&c1) {
            std::cmp::Ordering::Less => 0,
            std::cmp::Ordering::Equal => 1,
            std::cmp::Ordering::Greater => 2,
        }
    }

    fn ctx_comp_ref(&self) -> usize {
        Self::ref_count_ctx(
            self.count_refs(LAST_FRAME as i8) + self.count_refs(LAST2_FRAME as i8),
            self.count_refs(LAST3_FRAME as i8) + self.count_refs(GOLDEN_FRAME as i8),
        )
    }
    fn ctx_comp_ref_p1(&self) -> usize {
        Self::ref_count_ctx(self.count_refs(LAST_FRAME as i8), self.count_refs(LAST2_FRAME as i8))
    }
    fn ctx_comp_ref_p2(&self) -> usize {
        Self::ref_count_ctx(self.count_refs(LAST3_FRAME as i8), self.count_refs(GOLDEN_FRAME as i8))
    }
    fn ctx_comp_bwdref(&self) -> usize {
        Self::ref_count_ctx(self.count_refs(BWDREF_FRAME as i8) + self.count_refs(ALTREF2_FRAME as i8), self.count_refs(ALTREF_FRAME as i8))
    }
    fn ctx_comp_bwdref_p1(&self) -> usize {
        Self::ref_count_ctx(self.count_refs(BWDREF_FRAME as i8), self.count_refs(ALTREF2_FRAME as i8))
    }
    fn ctx_single_ref_p1(&self) -> usize {
        let fwd =
            self.count_refs(LAST_FRAME as i8) + self.count_refs(LAST2_FRAME as i8) + self.count_refs(LAST3_FRAME as i8) + self.count_refs(GOLDEN_FRAME as i8);
        let bwd = self.count_refs(BWDREF_FRAME as i8) + self.count_refs(ALTREF2_FRAME as i8) + self.count_refs(ALTREF_FRAME as i8);
        Self::ref_count_ctx(fwd, bwd)
    }

    fn read_ref_frames(&mut self) {
        if self.b.skip_mode {
            let f = self.fs.fh.skip_mode_frame;
            self.b.ref_frame = [f[0] as i8, f[1] as i8];
        } else if self.seg_active(SEG_LVL_REF_FRAME) {
            self.b.ref_frame = [self.fs.fh.seg.features.data[self.b.segment_id][SEG_LVL_REF_FRAME] as i8, -1];
        } else if self.seg_active(SEG_LVL_SKIP) || self.seg_active(SEG_LVL_GLOBALMV) {
            self.b.ref_frame = [LAST_FRAME as i8, -1];
        } else {
            let comp_mode = if self.fs.fh.reference_select && self.b.bw4.min(self.b.bh4) >= 2 {
                let ctx = self.comp_mode_ctx();
                self.sd.read_symbol(&mut self.cdf.comp_mode[ctx])
            } else {
                SINGLE_REFERENCE
            };
            if comp_mode == COMPOUND_REFERENCE {
                let ctx = self.comp_ref_type_ctx();
                let comp_ref_type = self.sd.read_symbol(&mut self.cdf.comp_ref_type[ctx]);
                if comp_ref_type == UNIDIR_COMP_REFERENCE {
                    let ctx = self.ctx_single_ref_p1();
                    if self.sd.read_symbol(&mut self.cdf.uni_comp_ref[ctx][0]) == 1 {
                        self.b.ref_frame = [BWDREF_FRAME as i8, ALTREF_FRAME as i8];
                    } else {
                        let ctx1 =
                            Self::ref_count_ctx(self.count_refs(LAST2_FRAME as i8), self.count_refs(LAST3_FRAME as i8) + self.count_refs(GOLDEN_FRAME as i8));
                        if self.sd.read_symbol(&mut self.cdf.uni_comp_ref[ctx1][1]) == 1 {
                            let ctx2 = self.ctx_comp_ref_p2();
                            if self.sd.read_symbol(&mut self.cdf.uni_comp_ref[ctx2][2]) == 1 {
                                self.b.ref_frame = [LAST_FRAME as i8, GOLDEN_FRAME as i8];
                            } else {
                                self.b.ref_frame = [LAST_FRAME as i8, LAST3_FRAME as i8];
                            }
                        } else {
                            self.b.ref_frame = [LAST_FRAME as i8, LAST2_FRAME as i8];
                        }
                    }
                } else {
                    let ctx = self.ctx_comp_ref();
                    if self.sd.read_symbol(&mut self.cdf.comp_ref[ctx][0]) == 0 {
                        let ctx = self.ctx_comp_ref_p1();
                        let p1 = self.sd.read_symbol(&mut self.cdf.comp_ref[ctx][1]);
                        self.b.ref_frame[0] = if p1 == 1 { LAST2_FRAME } else { LAST_FRAME } as i8;
                    } else {
                        let ctx = self.ctx_comp_ref_p2();
                        let p2 = self.sd.read_symbol(&mut self.cdf.comp_ref[ctx][2]);
                        self.b.ref_frame[0] = if p2 == 1 { GOLDEN_FRAME } else { LAST3_FRAME } as i8;
                    }
                    let ctx = self.ctx_comp_bwdref();
                    if self.sd.read_symbol(&mut self.cdf.comp_bwd_ref[ctx][0]) == 0 {
                        let ctx = self.ctx_comp_bwdref_p1();
                        let p1 = self.sd.read_symbol(&mut self.cdf.comp_bwd_ref[ctx][1]);
                        self.b.ref_frame[1] = if p1 == 1 { ALTREF2_FRAME } else { BWDREF_FRAME } as i8;
                    } else {
                        self.b.ref_frame[1] = ALTREF_FRAME as i8;
                    }
                }
            } else {
                let ctx = self.ctx_single_ref_p1();
                if self.sd.read_symbol(&mut self.cdf.single_ref[ctx][0]) == 1 {
                    let ctx = self.ctx_comp_bwdref();
                    if self.sd.read_symbol(&mut self.cdf.single_ref[ctx][1]) == 0 {
                        let ctx = self.ctx_comp_bwdref_p1();
                        let p6 = self.sd.read_symbol(&mut self.cdf.single_ref[ctx][5]);
                        self.b.ref_frame[0] = if p6 == 1 { ALTREF2_FRAME } else { BWDREF_FRAME } as i8;
                    } else {
                        self.b.ref_frame[0] = ALTREF_FRAME as i8;
                    }
                } else {
                    let ctx = self.ctx_comp_ref();
                    if self.sd.read_symbol(&mut self.cdf.single_ref[ctx][2]) == 1 {
                        let ctx = self.ctx_comp_ref_p2();
                        let p5 = self.sd.read_symbol(&mut self.cdf.single_ref[ctx][4]);
                        self.b.ref_frame[0] = if p5 == 1 { GOLDEN_FRAME } else { LAST3_FRAME } as i8;
                    } else {
                        let ctx = self.ctx_comp_ref_p1();
                        let p4 = self.sd.read_symbol(&mut self.cdf.single_ref[ctx][3]);
                        self.b.ref_frame[0] = if p4 == 1 { LAST2_FRAME } else { LAST_FRAME } as i8;
                    }
                }
                self.b.ref_frame[1] = -1;
            }
        }
    }

    fn check_backward(r: i8) -> bool {
        r >= BWDREF_FRAME as i8 && r <= ALTREF_FRAME as i8
    }

    fn comp_mode_ctx(&self) -> usize {
        let n = self.nb;
        let (au, al) = (self.b.avail_u, self.b.avail_l);
        if au && al {
            if n.above_single && n.left_single {
                (Self::check_backward(n.above[0]) ^ Self::check_backward(n.left[0])) as usize
            } else if n.above_single {
                2 + (Self::check_backward(n.above[0]) || n.above_intra) as usize
            } else if n.left_single {
                2 + (Self::check_backward(n.left[0]) || n.left_intra) as usize
            } else {
                4
            }
        } else if au {
            if n.above_single { Self::check_backward(n.above[0]) as usize } else { 3 }
        } else if al {
            if n.left_single { Self::check_backward(n.left[0]) as usize } else { 3 }
        } else {
            1
        }
    }

    fn comp_ref_type_ctx(&self) -> usize {
        let n = self.nb;
        let (au, al) = (self.b.avail_u, self.b.avail_l);
        let samedir = |a: i8, b: i8| (a >= BWDREF_FRAME as i8) == (b >= BWDREF_FRAME as i8);
        let (above0, above1, left0, left1) = (n.above[0], n.above[1], n.left[0], n.left[1]);
        let above_comp_inter = au && !n.above_intra && !n.above_single;
        let left_comp_inter = al && !n.left_intra && !n.left_single;
        let above_uni = above_comp_inter && samedir(above0, above1);
        let left_uni = left_comp_inter && samedir(left0, left1);
        if au && !n.above_intra && al && !n.left_intra {
            let sd = samedir(above0, left0) as usize;
            if !above_comp_inter && !left_comp_inter {
                1 + 2 * sd
            } else if !above_comp_inter {
                if !left_uni { 1 } else { 3 + sd }
            } else if !left_comp_inter {
                if !above_uni { 1 } else { 3 + sd }
            } else if !above_uni && !left_uni {
                0
            } else if !above_uni || !left_uni {
                2
            } else {
                3 + ((above0 == BWDREF_FRAME as i8) == (left0 == BWDREF_FRAME as i8)) as usize
            }
        } else if au && al {
            if above_comp_inter {
                1 + 2 * above_uni as usize
            } else if left_comp_inter {
                1 + 2 * left_uni as usize
            } else {
                2
            }
        } else if above_comp_inter {
            4 * above_uni as usize
        } else if left_comp_inter {
            4 * left_uni as usize
        } else {
            2
        }
    }

    fn inter_block_mode_info(&mut self) -> Result<()> {
        self.b.palette_size_y = 0;
        self.b.palette_size_uv = 0;
        self.read_ref_frames();
        let is_compound = self.b.ref_frame[1] > INTRA_FRAME as i8;
        self.find_mv_stack(is_compound);
        if self.b.skip_mode {
            self.b.y_mode = NEAREST_NEARESTMV;
        } else if self.seg_active(SEG_LVL_SKIP) || self.seg_active(SEG_LVL_GLOBALMV) {
            self.b.y_mode = GLOBALMV;
        } else if is_compound {
            let ctx = COMPOUND_MODE_CTX_MAP[self.mvs.ref_mv_context >> 1][self.mvs.new_mv_context.min(COMP_NEWMV_CTXS - 1)] as usize;
            self.b.y_mode = NEAREST_NEARESTMV + self.sd.read_symbol(&mut self.cdf.compound_mode[ctx]);
        } else {
            let new_mv = self.sd.read_symbol(&mut self.cdf.new_mv[self.mvs.new_mv_context]);
            if new_mv == 0 {
                self.b.y_mode = NEWMV;
            } else {
                let zero_mv = self.sd.read_symbol(&mut self.cdf.zero_mv[self.mvs.zero_mv_context]);
                if zero_mv == 0 {
                    self.b.y_mode = GLOBALMV;
                } else {
                    let ref_mv = self.sd.read_symbol(&mut self.cdf.ref_mv[self.mvs.ref_mv_context]);
                    self.b.y_mode = if ref_mv == 0 { NEARESTMV } else { NEARMV };
                }
            }
        }
        self.b.ref_mv_idx = 0;
        let ym = self.b.y_mode;
        if ym == NEWMV || ym == NEW_NEWMV {
            for idx in 0..2 {
                if self.mvs.num_mv_found > idx + 1 {
                    let ctx = self.mvs.drl_ctx_stack[idx];
                    let drl = self.sd.read_symbol(&mut self.cdf.drl_mode[ctx]);
                    if drl == 0 {
                        self.b.ref_mv_idx = idx;
                        break;
                    }
                    self.b.ref_mv_idx = idx + 1;
                }
            }
        } else if matches!(ym, NEARMV | NEAR_NEARMV | NEAR_NEWMV | NEW_NEARMV) {
            self.b.ref_mv_idx = 1;
            for idx in 1..3 {
                if self.mvs.num_mv_found > idx + 1 {
                    let ctx = self.mvs.drl_ctx_stack[idx];
                    let drl = self.sd.read_symbol(&mut self.cdf.drl_mode[ctx]);
                    if drl == 0 {
                        self.b.ref_mv_idx = idx;
                        break;
                    }
                    self.b.ref_mv_idx = idx + 1;
                }
            }
        }
        self.assign_mv(is_compound)?;
        self.read_interintra_mode(is_compound);
        self.read_motion_mode(is_compound);
        self.read_compound_type(is_compound);
        if self.fs.fh.interpolation_filter == SWITCHABLE {
            let dual = self.fs.seq.enable_dual_filter;
            for dir in 0..if dual { 2 } else { 1 } {
                if self.needs_interp_filter() {
                    let ctx = self.interp_filter_ctx(dir);
                    self.b.interp_filter[dir] = self.sd.read_symbol(&mut self.cdf.interp_filter[ctx]) as u8;
                } else {
                    self.b.interp_filter[dir] = EIGHTTAP;
                }
            }
            if !dual {
                self.b.interp_filter[1] = self.b.interp_filter[0];
            }
        } else {
            self.b.interp_filter = [self.fs.fh.interpolation_filter; 2];
        }
        Ok(())
    }

    fn needs_interp_filter(&self) -> bool {
        let ms = self.b.mi_size;
        let large = block_width(ms).min(block_height(ms)) >= 8;
        let gm = &self.fs.fh.gm_type;
        if self.b.skip_mode || self.b.motion_mode as usize == LOCALWARP {
            false
        } else if large && self.b.y_mode == GLOBALMV {
            gm[self.b.ref_frame[0] as usize] as usize == TRANSLATION
        } else if large && self.b.y_mode == GLOBAL_GLOBALMV {
            gm[self.b.ref_frame[0] as usize] as usize == TRANSLATION || gm[self.b.ref_frame[1] as usize] as usize == TRANSLATION
        } else {
            true
        }
    }

    fn interp_filter_ctx(&self, dir: usize) -> usize {
        let mut ctx = ((dir & 1) * 2 + (self.b.ref_frame[1] > INTRA_FRAME as i8) as usize) * 4;
        let mut left_type = 3u8;
        let mut above_type = 3u8;
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let mi = &self.t.mi;
        let rf0 = self.b.ref_frame[0];
        if self.b.avail_l {
            let i = mi.idx(r, c - 1);
            if mi.ref_frame[i][0] == rf0 || mi.ref_frame[i][1] == rf0 {
                left_type = mi.interp_filter[i][dir];
            }
        }
        if self.b.avail_u {
            let i = mi.idx(r - 1, c);
            if mi.ref_frame[i][0] == rf0 || mi.ref_frame[i][1] == rf0 {
                above_type = mi.interp_filter[i][dir];
            }
        }
        ctx += if left_type == above_type {
            left_type as usize
        } else if left_type == 3 {
            above_type as usize
        } else if above_type == 3 {
            left_type as usize
        } else {
            3
        };
        ctx
    }

    fn get_mode(&self, ref_list: usize) -> usize {
        let ym = self.b.y_mode;
        if ref_list == 0 {
            if ym < NEAREST_NEARESTMV {
                ym
            } else if matches!(ym, NEW_NEWMV | NEW_NEARESTMV | NEW_NEARMV) {
                NEWMV
            } else if matches!(ym, NEAREST_NEARESTMV | NEAREST_NEWMV) {
                NEARESTMV
            } else if matches!(ym, NEAR_NEARMV | NEAR_NEWMV) {
                NEARMV
            } else {
                GLOBALMV
            }
        } else if matches!(ym, NEW_NEWMV | NEAREST_NEWMV | NEAR_NEWMV) {
            NEWMV
        } else if matches!(ym, NEAREST_NEARESTMV | NEW_NEARESTMV) {
            NEARESTMV
        } else if matches!(ym, NEAR_NEARMV | NEW_NEARMV) {
            NEARMV
        } else {
            GLOBALMV
        }
    }

    pub(crate) fn assign_mv(&mut self, is_compound: bool) -> Result<()> {
        for i in 0..1 + is_compound as usize {
            let comp_mode = if self.b.use_intrabc { NEWMV } else { self.get_mode(i) };
            let pred: MvP = if self.b.use_intrabc {
                let mut p = self.mvs.ref_stack_mv[0][0];
                if p == [0, 0] {
                    p = self.mvs.ref_stack_mv[1][0];
                }
                if p == [0, 0] {
                    let sb_size = if self.fs.seq.use_128x128_superblock { BLOCK_128X128 } else { BLOCK_64X64 };
                    let sb4 = NUM_4X4_BLOCKS_HIGH[sb_size] as i32;
                    if (self.b.mi_row as i32) - sb4 < self.mi_row_start as i32 {
                        p = [0, -(sb4 * 4 + INTRABC_DELAY_PIXELS as i32) * 8];
                    } else {
                        p = [-(sb4 * 4 * 8), 0];
                    }
                }
                p
            } else if comp_mode == GLOBALMV {
                self.mvs.global_mvs[i]
            } else {
                let mut pos = if comp_mode == NEARESTMV { 0 } else { self.b.ref_mv_idx };
                if comp_mode == NEWMV && self.mvs.num_mv_found <= 1 {
                    pos = 0;
                }
                self.mvs.ref_stack_mv[pos][i]
            };
            self.b.pred_mv[i] = pred;
            if comp_mode == NEWMV {
                self.read_mv(i);
            } else {
                self.b.mv_i[i] = pred;
            }
        }
        Ok(())
    }

    fn read_mv(&mut self, r: usize) {
        let mut diff = [0i32; 2];
        let ctx = if self.b.use_intrabc { MV_INTRABC_CONTEXT } else { 0 };
        let joint = self.sd.read_symbol(&mut self.cdf.mv_joint[ctx]);
        if joint == MV_JOINT_HZVNZ || joint == MV_JOINT_HNZVNZ {
            diff[0] = self.read_mv_component(ctx, 0);
        }
        if joint == MV_JOINT_HNZVZ || joint == MV_JOINT_HNZVNZ {
            diff[1] = self.read_mv_component(ctx, 1);
        }
        self.b.mv_i[r] = [self.b.pred_mv[r][0] + diff[0], self.b.pred_mv[r][1] + diff[1]];
    }

    fn read_mv_component(&mut self, ctx: usize, comp: usize) -> i32 {
        let fh = &self.fs.fh;
        let (fim, hp) = (fh.force_integer_mv, fh.allow_high_precision_mv);
        let sign = self.sd.read_symbol(&mut self.cdf.mv_sign[ctx][comp]);
        let class = self.sd.read_symbol(&mut self.cdf.mv_class[ctx][comp]);
        let mag = if class == MV_CLASS_0 {
            let b0 = self.sd.read_symbol(&mut self.cdf.mv_class0_bit[ctx][comp]) as i32;
            let fr = if fim { 3 } else { self.sd.read_symbol(&mut self.cdf.mv_class0_fr[ctx][comp][b0 as usize]) as i32 };
            let h = if hp { self.sd.read_symbol(&mut self.cdf.mv_class0_hp[ctx][comp]) as i32 } else { 1 };
            ((b0 << 3) | (fr << 1) | h) + 1
        } else {
            let mut d = 0i32;
            for i in 0..class {
                d |= (self.sd.read_symbol(&mut self.cdf.mv_bit[ctx][comp][i]) as i32) << i;
            }
            let mut mag = (CLASS0_SIZE as i32) << (class + 2);
            let fr = if fim { 3 } else { self.sd.read_symbol(&mut self.cdf.mv_fr[ctx][comp]) as i32 };
            let h = if hp { self.sd.read_symbol(&mut self.cdf.mv_hp[ctx][comp]) as i32 } else { 1 };
            mag += ((d << 3) | (fr << 1) | h) + 1;
            mag
        };
        if sign == 1 { -mag } else { mag }
    }

    fn read_interintra_mode(&mut self, is_compound: bool) {
        let ms = self.b.mi_size;
        self.b.interintra = false;
        if !self.b.skip_mode && self.fs.seq.enable_interintra_compound && !is_compound && (BLOCK_8X8..=BLOCK_32X32).contains(&ms) {
            let ctx = SIZE_GROUP[ms] as usize - 1;
            self.b.interintra = self.sd.read_symbol(&mut self.cdf.inter_intra[ctx]) == 1;
            if self.b.interintra {
                self.b.interintra_mode = self.sd.read_symbol(&mut self.cdf.inter_intra_mode[ctx]);
                self.b.ref_frame[1] = INTRA_FRAME as i8;
                self.b.angle_delta_y = 0;
                self.b.angle_delta_uv = 0;
                self.b.use_filter_intra = false;
                self.b.wedge_interintra = self.sd.read_symbol(&mut self.cdf.wedge_inter_intra[ms]) == 1;
                if self.b.wedge_interintra {
                    self.b.wedge_index = self.sd.read_symbol(&mut self.cdf.wedge_index[ms]);
                    self.b.wedge_sign = 0;
                }
            }
        }
    }

    fn is_scaled(&self, rf: i8) -> bool {
        let fh = &self.fs.fh;
        let ri = &self.fs.ref_info[fh.ref_frame_idx[rf as usize - LAST_FRAME]];
        let fw = fh.frame_width;
        let fhh = fh.frame_height;
        let xs = ((ri.upscaled_width << REF_SCALE_SHIFT) + fw / 2) / fw;
        let ys = ((ri.frame_height << REF_SCALE_SHIFT) + fhh / 2) / fhh;
        let no = 1 << REF_SCALE_SHIFT;
        xs != no || ys != no
    }

    fn read_motion_mode(&mut self, is_compound: bool) {
        self.b.motion_mode = SIMPLE as u8;
        let fh = &self.fs.fh;
        if self.b.skip_mode || !fh.is_motion_mode_switchable {
            return;
        }
        let ms = self.b.mi_size;
        if block_width(ms).min(block_height(ms)) < 8 {
            return;
        }
        if !fh.force_integer_mv
            && (self.b.y_mode == GLOBALMV || self.b.y_mode == GLOBAL_GLOBALMV)
            && fh.gm_type[self.b.ref_frame[0] as usize] as usize > TRANSLATION
        {
            return;
        }
        if is_compound || self.b.ref_frame[1] == INTRA_FRAME as i8 || !self.has_overlappable_candidates() {
            return;
        }
        self.find_warp_samples();
        let fh = &self.fs.fh;
        if fh.force_integer_mv || self.mvs.num_samples == 0 || !fh.allow_warped_motion || self.is_scaled(self.b.ref_frame[0]) {
            let use_obmc = self.sd.read_symbol(&mut self.cdf.use_obmc[ms]);
            self.b.motion_mode = if use_obmc == 1 { OBMC as u8 } else { SIMPLE as u8 };
        } else {
            self.b.motion_mode = self.sd.read_symbol(&mut self.cdf.motion_mode[ms]) as u8;
        }
    }

    fn read_compound_type(&mut self, is_compound: bool) {
        self.b.comp_group_idx = 0;
        self.b.compound_idx = 1;
        if self.b.skip_mode {
            self.b.compound_type = COMPOUND_AVERAGE;
            return;
        }
        if is_compound {
            let ms = self.b.mi_size;
            let n = WEDGE_BITS[ms];
            if self.fs.seq.enable_masked_compound {
                let ctx = self.comp_group_idx_ctx();
                self.b.comp_group_idx = self.sd.read_symbol(&mut self.cdf.comp_group_idx[ctx]) as u8;
            }
            if self.b.comp_group_idx == 0 {
                if self.fs.seq.enable_jnt_comp {
                    let ctx = self.compound_idx_ctx();
                    self.b.compound_idx = self.sd.read_symbol(&mut self.cdf.compound_idx[ctx]) as u8;
                    self.b.compound_type = if self.b.compound_idx == 1 { COMPOUND_AVERAGE } else { COMPOUND_DISTANCE };
                } else {
                    self.b.compound_type = COMPOUND_AVERAGE;
                }
            } else if n == 0 {
                self.b.compound_type = COMPOUND_DIFFWTD;
            } else {
                self.b.compound_type = self.sd.read_symbol(&mut self.cdf.compound_type[ms]);
            }
            if self.b.compound_type == COMPOUND_WEDGE {
                self.b.wedge_index = self.sd.read_symbol(&mut self.cdf.wedge_index[ms]);
                self.b.wedge_sign = self.sd.read_literal(1) as usize;
            } else if self.b.compound_type == COMPOUND_DIFFWTD {
                self.b.mask_type = self.sd.read_literal(1) as usize;
            }
        } else if self.b.interintra {
            self.b.compound_type = if self.b.wedge_interintra { COMPOUND_WEDGE } else { COMPOUND_INTRA };
        } else {
            self.b.compound_type = COMPOUND_AVERAGE;
        }
    }

    fn comp_group_idx_ctx(&self) -> usize {
        let n = self.nb;
        let mi = &self.t.mi;
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let mut ctx = 0;
        if self.b.avail_u {
            if !n.above_single {
                ctx += mi.comp_group_idx[mi.idx(r - 1, c)] as usize;
            } else if n.above[0] == ALTREF_FRAME as i8 {
                ctx += 3;
            }
        }
        if self.b.avail_l {
            if !n.left_single {
                ctx += mi.comp_group_idx[mi.idx(r, c - 1)] as usize;
            } else if n.left[0] == ALTREF_FRAME as i8 {
                ctx += 3;
            }
        }
        ctx.min(5)
    }

    fn compound_idx_ctx(&self) -> usize {
        let fh = &self.fs.fh;
        let seq = &self.fs.seq;
        let fwd = crate::header::relative_dist(seq, fh.order_hints[self.b.ref_frame[0] as usize], fh.order_hint).abs();
        let bck = crate::header::relative_dist(seq, fh.order_hints[self.b.ref_frame[1] as usize], fh.order_hint).abs();
        let mut ctx = if fwd == bck { 3 } else { 0 };
        let n = self.nb;
        let mi = &self.t.mi;
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        if self.b.avail_u {
            if !n.above_single {
                ctx += mi.compound_idx[mi.idx(r - 1, c)] as usize;
            } else if n.above[0] == ALTREF_FRAME as i8 {
                ctx += 1;
            }
        }
        if self.b.avail_l {
            if !n.left_single {
                ctx += mi.compound_idx[mi.idx(r, c - 1)] as usize;
            } else if n.left[0] == ALTREF_FRAME as i8 {
                ctx += 1;
            }
        }
        ctx
    }
}

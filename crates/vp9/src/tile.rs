//! Tile decoding: partition trees, mode info (6.4), and block reconstruction. One `TileDecoder`
//! decodes all tiles of one tile column (tile columns are independent of each other; tile rows
//! of a column are decoded in order).

use crate::boolcoder::BoolDecoder;
use crate::frame::{Frame, MiGrid, MiInfo, Mv};
use crate::header::{FrameHeader, Segmentation};
use crate::probs::{Counts, FrameContext};
use crate::tables::{
    ALTREF_FRAME, B_HEIGHT_LOG2, B_WIDTH_LOG2, BLOCK_8X8, BLOCK_64X64, BLOCK_INVALID, BORDERINPIXELS, COMPANDED_MVREF_THRESH, COUNTER_TO_CONTEXT, DC_PRED,
    GOLDEN_FRAME, IDX_N_COLUMN_TO_SUBBLOCK, INTER_MODE_TREE, INTERP_EXTEND, INTERP_FILTER_TREE, INTRA_FRAME, INTRA_MODE_TREE, KF_PARTITION_PROBS,
    KF_UV_MODE_PROBS, KF_Y_MODE_PROBS, LAST_FRAME, MAX_TXSIZE, MI_WIDTH_LOG2, MODE_2_COUNTER, MV_BORDER, MV_CLASS_TREE, MV_FR_TREE, MV_JOINT_TREE,
    MV_REF_BLOCKS, NEARESTMV, NEARMV, NEWMV, NONE, NUM_4X4_HIGH, NUM_4X4_WIDE, NUM_8X8_HIGH, NUM_8X8_WIDE, PARTITION_TREE, SEG_LVL_REF_FRAME, SEG_LVL_SKIP,
    SEGMENT_TREE, SIZE_GROUP, SS_SIZE_LOOKUP, SUBSIZE_LOOKUP, SWITCHABLE, TX_MODE_SELECT, TX_MODE_TO_BIGGEST_TX_SIZE, TX_SIZE_8_TREE, TX_SIZE_16_TREE,
    TX_SIZE_32_TREE, ZEROMV,
};

mod recon;

/// A reference frame as used by the current frame, with its scale factors (8.5.2.3).
pub struct RefUse<'a> {
    pub frame: &'a Frame,
    pub x_scale: i32,
    pub y_scale: i32,
    pub x_step: i32,
    pub y_step: i32,
}

/// Read-only data shared by all tile columns of a frame.
pub struct FrameShared<'a> {
    pub h: &'a FrameHeader,
    pub fc: &'a FrameContext,
    pub seg: &'a Segmentation,
    /// PrevSegmentIds (MiRows x MiCols).
    pub prev_seg_ids: &'a [u8],
    /// Mode info of the previous frame when UsePrevFrameMvs is 1.
    pub prev_mi: Option<&'a MiGrid>,
    /// References for LAST, GOLDEN, ALTREF.
    pub refs: [Option<RefUse<'a>>; 3],
    /// Quantizers per segment: [segment][plane > 0][dc = 0 / ac = 1].
    pub seg_q: [[[i32; 2]; 2]; 8],
    /// Whether syntax elements need to be counted (backward adaptation enabled).
    pub counting: bool,
    pub pools: &'a crate::frame::Pools,
}

/// Output of one tile column: samples, mode info and segment ids of its columns.
pub struct Strip {
    pub mi_col_start: usize,
    pub mi_col_end: usize,
    /// Mode info, (mi_col_end - mi_col_start) x MiRows.
    pub mi: Vec<MiInfo>,
    pub seg_ids: Vec<u8>,
    pub planes: [Vec<u16>; 3],
    pub strides: [usize; 3],
    /// Plane x coordinate of the first column of each plane buffer.
    pub x_off: [usize; 3],
    pub counts: Box<Counts>,
    /// Statistics.
    pub compound_blocks: u64,
    pub scaled_blocks: u64,
    pub intra_blocks: u64,
    pub inter_blocks: u64,
}

/// Per-block state (the variables of 6.4.4 - 6.4.20).
#[derive(Clone, Copy, Default)]
struct Blk {
    r: usize,
    c: usize,
    size: u8,
    avail_u: bool,
    avail_l: bool,
    seg_id: u8,
    skip: bool,
    tx_size: u8,
    is_inter: bool,
    y_mode: u8,
    sub_modes: [u8; 4],
    uv_mode: u8,
    ref_frame: [i8; 2],
    interp_filter: u8,
    block_mvs: [[Mv; 4]; 2],
    left_ref: [i8; 2],
    above_ref: [i8; 2],
    left_intra: bool,
    above_intra: bool,
    left_single: bool,
    above_single: bool,
    eob_total: u32,
}

pub struct TileDecoder<'a> {
    s: &'a FrameShared<'a>,
    h: &'a FrameHeader,
    fc: &'a FrameContext,
    pub strip: Strip,
    mi_cols: usize,
    mi_rows: usize,
    ss_x: usize,
    ss_y: usize,
    bit_depth: u8,
    // Contexts (indexed by absolute position).
    above_nonzero: [Vec<u8>; 3],
    left_nonzero: [Vec<u8>; 3],
    above_partition: Vec<u8>,
    left_partition: Vec<u8>,
    above_seg_pred: Vec<u8>,
    left_seg_pred: Vec<u8>,
    // MV prediction state.
    ref_list_mv: [Mv; 2],
    ref_mv_count: usize,
    mode_context: [u8; 4],
    nearest_mv: [Mv; 2],
    near_mv: [Mv; 2],
    best_mv: [Mv; 2],
    // Scratch.
    coefs: Vec<i32>,
    token_cache: Vec<u8>,
    pred: Vec<u16>,
    pred2: Vec<u16>,
    mc_tmp: Vec<u16>,
    mc_win: Vec<u16>,
    /// First error met while decoding (e.g. a block referencing an unusable reference frame).
    pub error: Option<crate::error::Error>,
}

impl<'a> TileDecoder<'a> {
    pub fn new(s: &'a FrameShared<'a>, mi_col_start: usize, mi_col_end: usize) -> Self {
        let h = s.h;
        let (mi_cols, mi_rows) = (h.mi_cols as usize, h.mi_rows as usize);
        let (ss_x, ss_y) = (h.color.subsampling_x as usize, h.color.subsampling_y as usize);
        let sb_rows = h.sb64_rows as usize;
        let sb_cols = h.sb64_cols as usize;
        let strip_w = (mi_col_end - mi_col_start).div_ceil(8) * 64;
        let strip_h = sb_rows * 64;
        let x0 = mi_col_start * 8;
        let uv_len = (strip_w >> ss_x) * (strip_h >> ss_y);
        let planes = [s.pools.samples.take(strip_w * strip_h, 0), s.pools.samples.take(uv_len, 0), s.pools.samples.take(uv_len, 0)];
        let mi_w = mi_col_end - mi_col_start;
        let strip = Strip {
            mi_col_start,
            mi_col_end,
            mi: s.pools.mi.take(mi_w * mi_rows, MiInfo::default()),
            seg_ids: vec![0; mi_w * mi_rows],
            planes,
            strides: [strip_w, strip_w >> ss_x, strip_w >> ss_x],
            x_off: [x0, x0 >> ss_x, x0 >> ss_x],
            counts: Box::default(),
            compound_blocks: 0,
            scaled_blocks: 0,
            intra_blocks: 0,
            inter_blocks: 0,
        };
        let an = sb_cols * 16 + 16;
        let ln = sb_rows * 16 + 16;
        TileDecoder {
            s,
            h,
            fc: s.fc,
            strip,
            mi_cols,
            mi_rows,
            ss_x,
            ss_y,
            bit_depth: h.color.bit_depth,
            above_nonzero: [vec![0; an], vec![0; an], vec![0; an]],
            left_nonzero: [vec![0; ln], vec![0; ln], vec![0; ln]],
            above_partition: vec![0; sb_cols * 8 + 8],
            left_partition: vec![0; sb_rows * 8 + 8],
            above_seg_pred: vec![0; sb_cols * 8 + 8],
            left_seg_pred: vec![0; sb_rows * 8 + 8],
            ref_list_mv: [Mv::ZERO; 2],
            ref_mv_count: 0,
            mode_context: [0; 4],
            nearest_mv: [Mv::ZERO; 2],
            near_mv: [Mv::ZERO; 2],
            best_mv: [Mv::ZERO; 2],
            coefs: vec![0; 1024],
            token_cache: vec![0; 1024],
            pred: vec![0; 64 * 64],
            pred2: vec![0; 64 * 64],
            // Scaled prediction reads up to 5x the block height (8.5.2.3), unscaled h + 7 rows.
            mc_tmp: vec![0; if s.refs.iter().flatten().any(|r| r.x_scale != 1 << 14 || r.y_scale != 1 << 14) { 64 * (64 * 5 + 16) } else { 64 * 71 }],
            mc_win: vec![0; 71 * 71],
            error: None,
        }
    }

    #[inline]
    fn mi_at(&self, r: usize, c: usize) -> &MiInfo {
        &self.strip.mi[r * (self.strip.mi_col_end - self.strip.mi_col_start) + c - self.strip.mi_col_start]
    }

    /// decode_tile (6.4.2) for one tile of this column. Returns false if the data ran out badly.
    pub fn decode_tile(&mut self, data: &[u8], mi_row_start: usize, mi_row_end: usize) -> bool {
        let mut bd = BoolDecoder::new(data);
        let (cs, ce) = (self.strip.mi_col_start, self.strip.mi_col_end);
        for r in (mi_row_start..mi_row_end).step_by(8) {
            // clear_left_context
            for p in 0..3 {
                let sy = if p > 0 { self.ss_y } else { 0 };
                let s = (r * 2) >> sy;
                let e = (s + (16 >> sy)).min(self.left_nonzero[p].len());
                self.left_nonzero[p][s..e].fill(0);
            }
            self.left_partition[r..r + 8].fill(0);
            self.left_seg_pred[r..r + 8].fill(0);
            for c in (cs..ce).step_by(8) {
                self.decode_partition(&mut bd, r, c, BLOCK_64X64);
            }
        }
        true
    }

    fn decode_partition(&mut self, bd: &mut BoolDecoder, r: usize, c: usize, bsize: u8) {
        if r >= self.mi_rows || c >= self.mi_cols {
            return;
        }
        let num8x8 = NUM_8X8_WIDE[bsize as usize] as usize;
        let half = num8x8 >> 1;
        let has_rows = (r + half) < self.mi_rows;
        let has_cols = (c + half) < self.mi_cols;
        let bsl = MI_WIDTH_LOG2[bsize as usize] as usize;
        let boffset = 3 - bsl;
        let mut above = 0u8;
        let mut left = 0u8;
        for i in 0..num8x8 {
            above |= self.above_partition[c + i];
            left |= self.left_partition[r + i];
        }
        let above = (above >> boffset) & 1;
        let left = (left >> boffset) & 1;
        let ctx = bsl * 4 + left as usize * 2 + above as usize;
        let probs: &[u8] = if self.h.frame_is_intra { &KF_PARTITION_PROBS[ctx * 3..ctx * 3 + 3] } else { &self.fc.partition[ctx] };
        let partition = if has_rows && has_cols {
            bd.read_tree(&PARTITION_TREE, probs)
        } else if has_cols {
            if bd.read_bool(probs[1]) { 3 } else { 1 }
        } else if has_rows {
            if bd.read_bool(probs[2]) { 3 } else { 2 }
        } else {
            3
        };
        if self.s.counting {
            self.strip.counts.partition[ctx][partition as usize] += 1;
        }
        let subsize = SUBSIZE_LOOKUP[partition as usize][bsize as usize];
        if subsize < BLOCK_8X8 || partition == 0 {
            self.decode_block(bd, r, c, subsize);
        } else if partition == 1 {
            self.decode_block(bd, r, c, subsize);
            if has_rows {
                self.decode_block(bd, r + half, c, subsize);
            }
        } else if partition == 2 {
            self.decode_block(bd, r, c, subsize);
            if has_cols {
                self.decode_block(bd, r, c + half, subsize);
            }
        } else {
            self.decode_partition(bd, r, c, subsize);
            self.decode_partition(bd, r, c + half, subsize);
            self.decode_partition(bd, r + half, c, subsize);
            self.decode_partition(bd, r + half, c + half, subsize);
        }
        if bsize == BLOCK_8X8 || partition != 3 {
            let a = 15 >> B_WIDTH_LOG2[subsize as usize];
            let l = 15 >> B_HEIGHT_LOG2[subsize as usize];
            self.above_partition[c..c + num8x8].fill(a);
            self.left_partition[r..r + num8x8].fill(l);
        }
    }

    fn decode_block(&mut self, bd: &mut BoolDecoder, r: usize, c: usize, size: u8) {
        let mut b = Blk { r, c, size, avail_u: r > 0, avail_l: c > self.strip.mi_col_start, ..Default::default() };
        // Chroma block size must be valid (7.4.3); treat violations as a damaged stream.
        if size >= BLOCK_8X8 && SS_SIZE_LOOKUP[size as usize][self.ss_x][self.ss_y] == BLOCK_INVALID {
            b.size = BLOCK_8X8;
        }
        if self.h.frame_is_intra {
            self.intra_frame_mode_info(bd, &mut b);
        } else {
            self.inter_frame_mode_info(bd, &mut b);
        }
        if b.is_inter {
            self.strip.inter_blocks += 1;
        } else {
            self.strip.intra_blocks += 1;
        }
        self.residual(bd, &mut b);
        if b.is_inter && b.size >= BLOCK_8X8 && b.eob_total == 0 {
            b.skip = true;
        }
        let info = MiInfo {
            sb_size: b.size,
            skip: b.skip,
            tx_size: b.tx_size,
            y_mode: b.y_mode,
            sub_modes: b.sub_modes,
            seg_id: b.seg_id,
            ref_frame: b.ref_frame,
            interp_filter: b.interp_filter,
            mv: b.block_mvs,
        };
        let bh = NUM_8X8_HIGH[b.size as usize] as usize;
        let bw = NUM_8X8_WIDE[b.size as usize] as usize;
        let mi_w = self.strip.mi_col_end - self.strip.mi_col_start;
        let (y1, x1) = ((r + bh).min(self.mi_rows), (c + bw).min(self.strip.mi_col_end));
        for y in r..y1 {
            let row = y * mi_w;
            for x in c..x1 {
                self.strip.mi[row + x - self.strip.mi_col_start] = info;
                self.strip.seg_ids[row + x - self.strip.mi_col_start] = b.seg_id;
            }
        }
    }

    // ---------------------------------------------------------------- mode info (6.4.5 - 6.4.20)

    fn intra_frame_mode_info(&mut self, bd: &mut BoolDecoder, b: &mut Blk) {
        // intra_segment_id
        b.seg_id = if self.s.seg.enabled && self.s.seg.update_map { bd.read_tree(&SEGMENT_TREE, &self.s.seg.tree_probs) } else { 0 };
        self.read_skip(bd, b);
        self.read_tx_size(bd, b, true);
        b.ref_frame = [INTRA_FRAME, NONE];
        b.is_inter = false;
        let above_mi = if b.avail_u { Some(*self.mi_at(b.r - 1, b.c)) } else { None };
        let left_mi = if b.avail_l { Some(*self.mi_at(b.r, b.c - 1)) } else { None };
        if b.size >= BLOCK_8X8 {
            let above = above_mi.map_or(DC_PRED, |m| m.sub_modes[2]);
            let left = left_mi.map_or(DC_PRED, |m| m.sub_modes[1]);
            let o = (above as usize * 10 + left as usize) * 9;
            let mode = bd.read_tree(&INTRA_MODE_TREE, &KF_Y_MODE_PROBS[o..o + 9]);
            b.y_mode = mode;
            b.sub_modes = [mode; 4];
        } else {
            let n4w = NUM_4X4_WIDE[b.size as usize] as usize;
            let n4h = NUM_4X4_HIGH[b.size as usize] as usize;
            let mut mode = 0;
            let mut idy = 0;
            while idy < 2 {
                let mut idx = 0;
                while idx < 2 {
                    let above = if idy > 0 { b.sub_modes[idx] } else { above_mi.map_or(DC_PRED, |m| m.sub_modes[2 + idx]) };
                    let left = if idx > 0 { b.sub_modes[idy * 2] } else { left_mi.map_or(DC_PRED, |m| m.sub_modes[1 + idy * 2]) };
                    let o = (above as usize * 10 + left as usize) * 9;
                    mode = bd.read_tree(&INTRA_MODE_TREE, &KF_Y_MODE_PROBS[o..o + 9]);
                    for y2 in 0..n4h {
                        for x2 in 0..n4w {
                            b.sub_modes[(idy + y2) * 2 + idx + x2] = mode;
                        }
                    }
                    idx += n4w;
                }
                idy += n4h;
            }
            b.y_mode = mode;
        }
        let o = b.y_mode as usize * 9;
        b.uv_mode = bd.read_tree(&INTRA_MODE_TREE, &KF_UV_MODE_PROBS[o..o + 9]);
    }

    fn read_skip(&mut self, bd: &mut BoolDecoder, b: &mut Blk) {
        if self.s.seg.feature_active(b.seg_id, SEG_LVL_SKIP) {
            b.skip = true;
        } else {
            let mut ctx = 0;
            if b.avail_u {
                ctx += self.mi_at(b.r - 1, b.c).skip as usize;
            }
            if b.avail_l {
                ctx += self.mi_at(b.r, b.c - 1).skip as usize;
            }
            b.skip = bd.read_bool(self.fc.skip[ctx]);
            if self.s.counting {
                self.strip.counts.skip[ctx][b.skip as usize] += 1;
            }
        }
    }

    fn read_tx_size(&mut self, bd: &mut BoolDecoder, b: &mut Blk, allow_select: bool) {
        let max_tx = MAX_TXSIZE[b.size as usize];
        if allow_select && self.h.tx_mode == TX_MODE_SELECT && b.size >= BLOCK_8X8 {
            let mut above = max_tx;
            let mut left = max_tx;
            if b.avail_u {
                let m = self.mi_at(b.r - 1, b.c);
                if !m.skip {
                    above = m.tx_size;
                }
            }
            if b.avail_l {
                let m = self.mi_at(b.r, b.c - 1);
                if !m.skip {
                    left = m.tx_size;
                }
            }
            if !b.avail_l {
                left = above;
            }
            if !b.avail_u {
                above = left;
            }
            let ctx = ((above + left) > max_tx) as usize;
            let tree: &[i8] = match max_tx {
                3 => &TX_SIZE_32_TREE,
                2 => &TX_SIZE_16_TREE,
                _ => &TX_SIZE_8_TREE,
            };
            b.tx_size = bd.read_tree(tree, &self.fc.tx[max_tx as usize][ctx]);
            if self.s.counting {
                self.strip.counts.tx[max_tx as usize][ctx][b.tx_size as usize] += 1;
            }
        } else {
            b.tx_size = max_tx.min(TX_MODE_TO_BIGGEST_TX_SIZE[self.h.tx_mode as usize]);
        }
    }

    fn inter_frame_mode_info(&mut self, bd: &mut BoolDecoder, b: &mut Blk) {
        b.left_ref = if b.avail_l { self.mi_at(b.r, b.c - 1).ref_frame } else { [INTRA_FRAME, NONE] };
        b.above_ref = if b.avail_u { self.mi_at(b.r - 1, b.c).ref_frame } else { [INTRA_FRAME, NONE] };
        b.left_intra = b.left_ref[0] <= INTRA_FRAME;
        b.above_intra = b.above_ref[0] <= INTRA_FRAME;
        b.left_single = b.left_ref[1] <= NONE;
        b.above_single = b.above_ref[1] <= NONE;
        self.inter_segment_id(bd, b);
        self.read_skip(bd, b);
        self.read_is_inter(bd, b);
        let allow = !b.skip || !b.is_inter;
        self.read_tx_size(bd, b, allow);
        if b.is_inter {
            self.inter_block_mode_info(bd, b);
        } else {
            self.intra_block_mode_info(bd, b);
        }
    }

    fn get_segment_id(&self, b: &Blk) -> u8 {
        let bw = NUM_8X8_WIDE[b.size as usize] as usize;
        let bh = NUM_8X8_HIGH[b.size as usize] as usize;
        let xmis = (self.mi_cols - b.c).min(bw);
        let ymis = (self.mi_rows - b.r).min(bh);
        let mut seg = 7;
        let prev = self.s.prev_seg_ids;
        if prev.is_empty() {
            return 0;
        }
        for y in 0..ymis {
            for x in 0..xmis {
                seg = seg.min(prev[(b.r + y) * self.mi_cols + b.c + x]);
            }
        }
        seg
    }

    fn inter_segment_id(&mut self, bd: &mut BoolDecoder, b: &mut Blk) {
        let seg = self.s.seg;
        if !seg.enabled {
            b.seg_id = 0;
            return;
        }
        let predicted = self.get_segment_id(b);
        if !seg.update_map {
            b.seg_id = predicted;
            return;
        }
        if seg.temporal_update {
            let ctx = (self.left_seg_pred[b.r] + self.above_seg_pred[b.c]) as usize;
            let pred_flag = bd.read_bool(seg.pred_probs[ctx]);
            b.seg_id = if pred_flag { predicted } else { bd.read_tree(&SEGMENT_TREE, &seg.tree_probs) };
            let bw = NUM_8X8_WIDE[b.size as usize] as usize;
            let bh = NUM_8X8_HIGH[b.size as usize] as usize;
            self.above_seg_pred[b.c..b.c + bw].fill(pred_flag as u8);
            self.left_seg_pred[b.r..b.r + bh].fill(pred_flag as u8);
        } else {
            b.seg_id = bd.read_tree(&SEGMENT_TREE, &seg.tree_probs);
        }
    }

    fn read_is_inter(&mut self, bd: &mut BoolDecoder, b: &mut Blk) {
        if self.s.seg.feature_active(b.seg_id, SEG_LVL_REF_FRAME) {
            b.is_inter = self.s.seg.feature_data[b.seg_id as usize][SEG_LVL_REF_FRAME] != INTRA_FRAME as i16;
        } else {
            let ctx = if b.avail_u && b.avail_l {
                if b.left_intra && b.above_intra { 3 } else { (b.left_intra || b.above_intra) as usize }
            } else if b.avail_u || b.avail_l {
                2 * (if b.avail_u { b.above_intra } else { b.left_intra }) as usize
            } else {
                0
            };
            b.is_inter = bd.read_bool(self.fc.is_inter[ctx]);
            if self.s.counting {
                self.strip.counts.is_inter[ctx][b.is_inter as usize] += 1;
            }
        }
    }

    fn intra_block_mode_info(&mut self, bd: &mut BoolDecoder, b: &mut Blk) {
        b.ref_frame = [INTRA_FRAME, NONE];
        if b.size >= BLOCK_8X8 {
            let ctx = SIZE_GROUP[b.size as usize] as usize;
            let mode = bd.read_tree(&INTRA_MODE_TREE, &self.fc.y_mode[ctx]);
            if self.s.counting {
                self.strip.counts.intra_mode[ctx][mode as usize] += 1;
            }
            b.y_mode = mode;
            b.sub_modes = [mode; 4];
        } else {
            let n4w = NUM_4X4_WIDE[b.size as usize] as usize;
            let n4h = NUM_4X4_HIGH[b.size as usize] as usize;
            let mut mode = 0;
            let mut idy = 0;
            while idy < 2 {
                let mut idx = 0;
                while idx < 2 {
                    mode = bd.read_tree(&INTRA_MODE_TREE, &self.fc.y_mode[0]);
                    if self.s.counting {
                        self.strip.counts.intra_mode[0][mode as usize] += 1;
                    }
                    for y2 in 0..n4h {
                        for x2 in 0..n4w {
                            b.sub_modes[(idy + y2) * 2 + idx + x2] = mode;
                        }
                    }
                    idx += n4w;
                }
                idy += n4h;
            }
            b.y_mode = mode;
        }
        b.uv_mode = bd.read_tree(&INTRA_MODE_TREE, &self.fc.uv_mode[b.y_mode as usize]);
        if self.s.counting {
            self.strip.counts.uv_mode[b.y_mode as usize][b.uv_mode as usize] += 1;
        }
    }

    fn inter_block_mode_info(&mut self, bd: &mut BoolDecoder, b: &mut Blk) {
        self.read_ref_frames(bd, b);
        let is_compound = b.ref_frame[1] > INTRA_FRAME;
        if is_compound {
            self.strip.compound_blocks += 1;
        }
        for j in 0..1 + is_compound as usize {
            let rf = b.ref_frame[j];
            self.find_mv_refs(b, rf, -1);
            self.find_best_ref_mvs(b, j);
        }
        b.sub_modes = [DC_PRED; 4];
        if self.s.seg.feature_active(b.seg_id, SEG_LVL_SKIP) {
            b.y_mode = ZEROMV;
        } else if b.size >= BLOCK_8X8 {
            let ctx = self.mode_context[b.ref_frame[0] as usize] as usize;
            let inter_mode = bd.read_tree(&INTER_MODE_TREE, &self.fc.inter_mode[ctx.min(6)]);
            if self.s.counting {
                self.strip.counts.inter_mode[ctx.min(6)][inter_mode as usize] += 1;
            }
            b.y_mode = NEARESTMV + inter_mode;
        }
        b.interp_filter = if self.h.interp_filter == SWITCHABLE {
            let left = if b.avail_l && b.left_ref[0] > INTRA_FRAME { self.mi_at(b.r, b.c - 1).interp_filter } else { 3 };
            let above = if b.avail_u && b.above_ref[0] > INTRA_FRAME { self.mi_at(b.r - 1, b.c).interp_filter } else { 3 };
            let ctx = if left == above {
                left
            } else if left == 3 && above != 3 {
                above
            } else if left != 3 && above == 3 {
                left
            } else {
                3
            } as usize;
            let f = bd.read_tree(&INTERP_FILTER_TREE, &self.fc.interp_filter[ctx]);
            if self.s.counting {
                self.strip.counts.interp_filter[ctx][f as usize] += 1;
            }
            f
        } else {
            self.h.interp_filter
        };
        b.block_mvs = [[Mv::ZERO; 4]; 2];
        if b.size < BLOCK_8X8 {
            let n4w = NUM_4X4_WIDE[b.size as usize] as usize;
            let n4h = NUM_4X4_HIGH[b.size as usize] as usize;
            let mut idy = 0;
            while idy < 2 {
                let mut idx = 0;
                while idx < 2 {
                    let ctx = self.mode_context[b.ref_frame[0] as usize] as usize;
                    let inter_mode = bd.read_tree(&INTER_MODE_TREE, &self.fc.inter_mode[ctx.min(6)]);
                    if self.s.counting {
                        self.strip.counts.inter_mode[ctx.min(6)][inter_mode as usize] += 1;
                    }
                    b.y_mode = NEARESTMV + inter_mode;
                    if b.y_mode == NEARESTMV || b.y_mode == NEARMV {
                        for j in 0..1 + is_compound as usize {
                            self.append_sub8x8_mvs(b, (idy * 2 + idx) as i32, j);
                        }
                    }
                    let mv = self.assign_mv(bd, b, is_compound);
                    for y2 in 0..n4h {
                        for x2 in 0..n4w {
                            let blk = (idy + y2) * 2 + idx + x2;
                            for l in 0..1 + is_compound as usize {
                                b.block_mvs[l][blk] = mv[l];
                            }
                        }
                    }
                    idx += n4w;
                }
                idy += n4h;
            }
        } else {
            let mv = self.assign_mv(bd, b, is_compound);
            for l in 0..1 + is_compound as usize {
                b.block_mvs[l] = [mv[l]; 4];
            }
        }
    }

    fn read_ref_frames(&mut self, bd: &mut BoolDecoder, b: &mut Blk) {
        let seg = self.s.seg;
        if seg.feature_active(b.seg_id, SEG_LVL_REF_FRAME) {
            b.ref_frame = [(seg.feature_data[b.seg_id as usize][SEG_LVL_REF_FRAME] as i8).clamp(LAST_FRAME, ALTREF_FRAME), NONE];
            return;
        }
        let h = self.h;
        let comp_mode = if h.reference_mode == 2 {
            let ctx = self.comp_mode_ctx(b);
            let v = bd.read_bool(self.fc.comp_mode[ctx]);
            if self.s.counting {
                self.strip.counts.comp_mode[ctx][v as usize] += 1;
            }
            v as u8
        } else {
            h.reference_mode
        };
        if comp_mode == 1 {
            let idx = h.ref_frame_sign_bias[h.comp_fixed_ref as usize] as usize;
            let ctx = self.comp_ref_ctx(b);
            let comp_ref = bd.read_bool(self.fc.comp_ref[ctx]);
            if self.s.counting {
                self.strip.counts.comp_ref[ctx][comp_ref as usize] += 1;
            }
            b.ref_frame[idx] = h.comp_fixed_ref;
            b.ref_frame[1 - idx] = h.comp_var_ref[comp_ref as usize];
        } else {
            let ctx = self.single_ref_p1_ctx(b);
            let p1 = bd.read_bool(self.fc.single_ref[ctx][0]);
            if self.s.counting {
                self.strip.counts.single_ref[ctx][0][p1 as usize] += 1;
            }
            b.ref_frame[0] = if p1 {
                let ctx2 = self.single_ref_p2_ctx(b);
                let p2 = bd.read_bool(self.fc.single_ref[ctx2][1]);
                if self.s.counting {
                    self.strip.counts.single_ref[ctx2][1][p2 as usize] += 1;
                }
                if p2 { ALTREF_FRAME } else { GOLDEN_FRAME }
            } else {
                LAST_FRAME
            };
            b.ref_frame[1] = NONE;
        }
    }

    fn comp_mode_ctx(&self, b: &Blk) -> usize {
        let fixed = self.h.comp_fixed_ref;
        (if b.avail_u && b.avail_l {
            if b.above_single && b.left_single {
                ((b.above_ref[0] == fixed) ^ (b.left_ref[0] == fixed)) as u8
            } else if b.above_single {
                2 + (b.above_ref[0] == fixed || b.above_intra) as u8
            } else if b.left_single {
                2 + (b.left_ref[0] == fixed || b.left_intra) as u8
            } else {
                4
            }
        } else if b.avail_u {
            if b.above_single { (b.above_ref[0] == fixed) as u8 } else { 3 }
        } else if b.avail_l {
            if b.left_single { (b.left_ref[0] == fixed) as u8 } else { 3 }
        } else {
            1
        }) as usize
    }

    fn comp_ref_ctx(&self, b: &Blk) -> usize {
        let h = self.h;
        let fix_ref_idx = h.ref_frame_sign_bias[h.comp_fixed_ref as usize] as usize;
        let var_ref_idx = 1 - fix_ref_idx;
        let var0 = h.comp_var_ref[0];
        let var1 = h.comp_var_ref[1];
        let fixed = h.comp_fixed_ref;
        (if b.avail_u && b.avail_l {
            if b.above_intra && b.left_intra {
                2
            } else if b.left_intra {
                if b.above_single { 1 + 2 * (b.above_ref[0] != var1) as u8 } else { 1 + 2 * (b.above_ref[var_ref_idx] != var1) as u8 }
            } else if b.above_intra {
                if b.left_single { 1 + 2 * (b.left_ref[0] != var1) as u8 } else { 1 + 2 * (b.left_ref[var_ref_idx] != var1) as u8 }
            } else {
                let vrfa = if b.above_single { b.above_ref[0] } else { b.above_ref[var_ref_idx] };
                let vrfl = if b.left_single { b.left_ref[0] } else { b.left_ref[var_ref_idx] };
                if vrfa == vrfl && var1 == vrfa {
                    0
                } else if b.left_single && b.above_single {
                    if (vrfa == fixed && vrfl == var0) || (vrfl == fixed && vrfa == var0) {
                        4
                    } else if vrfa == vrfl {
                        3
                    } else {
                        1
                    }
                } else if b.left_single || b.above_single {
                    let vrfc = if b.left_single { vrfa } else { vrfl };
                    let rfs = if b.above_single { vrfa } else { vrfl };
                    if vrfc == var1 && rfs != var1 {
                        1
                    } else if rfs == var1 && vrfc != var1 {
                        2
                    } else {
                        4
                    }
                } else if vrfa == vrfl {
                    4
                } else {
                    2
                }
            }
        } else if b.avail_u {
            if b.above_intra {
                2
            } else if b.above_single {
                3 * (b.above_ref[0] != var1) as u8
            } else {
                4 * (b.above_ref[var_ref_idx] != var1) as u8
            }
        } else if b.avail_l {
            if b.left_intra {
                2
            } else if b.left_single {
                3 * (b.left_ref[0] != var1) as u8
            } else {
                4 * (b.left_ref[var_ref_idx] != var1) as u8
            }
        } else {
            2
        }) as usize
    }

    fn single_ref_p1_ctx(&self, b: &Blk) -> usize {
        let (a, l) = (b.above_ref, b.left_ref);
        (if b.avail_u && b.avail_l {
            if b.above_intra && b.left_intra {
                2
            } else if b.left_intra {
                if b.above_single { 4 * (a[0] == LAST_FRAME) as u8 } else { 1 + (a[0] == LAST_FRAME || a[1] == LAST_FRAME) as u8 }
            } else if b.above_intra {
                if b.left_single { 4 * (l[0] == LAST_FRAME) as u8 } else { 1 + (l[0] == LAST_FRAME || l[1] == LAST_FRAME) as u8 }
            } else if b.above_single && b.left_single {
                2 * (a[0] == LAST_FRAME) as u8 + 2 * (l[0] == LAST_FRAME) as u8
            } else if !b.above_single && !b.left_single {
                1 + (a[0] == LAST_FRAME || a[1] == LAST_FRAME || l[0] == LAST_FRAME || l[1] == LAST_FRAME) as u8
            } else {
                let rfs = if b.above_single { a[0] } else { l[0] };
                let crf1 = if b.above_single { l[0] } else { a[0] };
                let crf2 = if b.above_single { l[1] } else { a[1] };
                if rfs == LAST_FRAME { 3 + (crf1 == LAST_FRAME || crf2 == LAST_FRAME) as u8 } else { (crf1 == LAST_FRAME || crf2 == LAST_FRAME) as u8 }
            }
        } else if b.avail_u {
            if b.above_intra {
                2
            } else if b.above_single {
                4 * (a[0] == LAST_FRAME) as u8
            } else {
                1 + (a[0] == LAST_FRAME || a[1] == LAST_FRAME) as u8
            }
        } else if b.avail_l {
            if b.left_intra {
                2
            } else if b.left_single {
                4 * (l[0] == LAST_FRAME) as u8
            } else {
                1 + (l[0] == LAST_FRAME || l[1] == LAST_FRAME) as u8
            }
        } else {
            2
        }) as usize
    }

    fn single_ref_p2_ctx(&self, b: &Blk) -> usize {
        let (a, l) = (b.above_ref, b.left_ref);
        const G: i8 = GOLDEN_FRAME;
        const L: i8 = LAST_FRAME;
        (if b.avail_u && b.avail_l {
            if b.above_intra && b.left_intra {
                2
            } else if b.left_intra {
                if b.above_single { if a[0] == L { 3 } else { 4 * (a[0] == G) as u8 } } else { 1 + 2 * (a[0] == G || a[1] == G) as u8 }
            } else if b.above_intra {
                if b.left_single { if l[0] == L { 3 } else { 4 * (l[0] == G) as u8 } } else { 1 + 2 * (l[0] == G || l[1] == G) as u8 }
            } else if b.above_single && b.left_single {
                if a[0] == L && l[0] == L {
                    3
                } else if a[0] == L {
                    4 * (l[0] == G) as u8
                } else if l[0] == L {
                    4 * (a[0] == G) as u8
                } else {
                    2 * (a[0] == G) as u8 + 2 * (l[0] == G) as u8
                }
            } else if !b.above_single && !b.left_single {
                if a[0] == l[0] && a[1] == l[1] { 3 * (a[0] == G || a[1] == G) as u8 } else { 2 }
            } else {
                let rfs = if b.above_single { a[0] } else { l[0] };
                let crf1 = if b.above_single { l[0] } else { a[0] };
                let crf2 = if b.above_single { l[1] } else { a[1] };
                if rfs == G {
                    3 + (crf1 == G || crf2 == G) as u8
                } else if rfs == ALTREF_FRAME {
                    (crf1 == G || crf2 == G) as u8
                } else {
                    1 + 2 * (crf1 == G || crf2 == G) as u8
                }
            }
        } else if b.avail_u {
            if b.above_intra || (a[0] == L && b.above_single) {
                2
            } else if b.above_single {
                4 * (a[0] == G) as u8
            } else {
                3 * (a[0] == G || a[1] == G) as u8
            }
        } else if b.avail_l {
            if b.left_intra || (l[0] == L && b.left_single) {
                2
            } else if b.left_single {
                4 * (l[0] == G) as u8
            } else {
                3 * (l[0] == G || l[1] == G) as u8
            }
        } else {
            2
        }) as usize
    }

    /// assign_mv (6.4.18): returns Mv[0..2].
    fn assign_mv(&mut self, bd: &mut BoolDecoder, b: &Blk, is_compound: bool) -> [Mv; 2] {
        let mut mv = [Mv::ZERO; 2];
        for i in 0..1 + is_compound as usize {
            mv[i] = match b.y_mode {
                NEWMV => self.read_mv(bd, i),
                NEARESTMV => self.nearest_mv[i],
                NEARMV => self.near_mv[i],
                _ => Mv::ZERO,
            };
        }
        mv
    }

    fn read_mv(&mut self, bd: &mut BoolDecoder, r: usize) -> Mv {
        let use_hp = self.h.allow_high_precision_mv && use_mv_hp(self.best_mv[r]);
        let joint = bd.read_tree(&MV_JOINT_TREE, &self.fc.mv_joint);
        if self.s.counting {
            self.strip.counts.mv_joint[joint as usize] += 1;
        }
        let mut diff = [0i32; 2];
        if joint == 2 || joint == 3 {
            diff[0] = self.read_mv_component(bd, 0, use_hp);
        }
        if joint == 1 || joint == 3 {
            diff[1] = self.read_mv_component(bd, 1, use_hp);
        }
        Mv::new(self.best_mv[r].row as i32 + diff[0], self.best_mv[r].col as i32 + diff[1])
    }

    fn read_mv_component(&mut self, bd: &mut BoolDecoder, comp: usize, use_hp: bool) -> i32 {
        let fc = self.fc;
        let counting = self.s.counting;
        let counts = &mut self.strip.counts;
        let sign = bd.read_bool(fc.mv_sign[comp]);
        let class = bd.read_tree(&MV_CLASS_TREE, &fc.mv_class[comp]) as usize;
        let mag = if class == 0 {
            let c0 = bd.read_bool(fc.mv_class0_bit[comp]) as usize;
            let fr = bd.read_tree(&MV_FR_TREE, &fc.mv_class0_fr[comp][c0]) as usize;
            let hp = if use_hp { bd.read_bool(fc.mv_class0_hp[comp]) as usize } else { 1 };
            if counting {
                counts.mv_class0_bit[comp][c0] += 1;
                counts.mv_class0_fr[comp][c0][fr] += 1;
                counts.mv_class0_hp[comp][hp] += 1;
            }
            ((c0 << 3) | (fr << 1) | hp) + 1
        } else {
            let mut d = 0usize;
            for i in 0..class {
                let bit = bd.read_bool(fc.mv_bits[comp][i]) as usize;
                if counting {
                    counts.mv_bits[comp][i][bit] += 1;
                }
                d |= bit << i;
            }
            let base = 2usize << (class + 2);
            let fr = bd.read_tree(&MV_FR_TREE, &fc.mv_fr[comp]) as usize;
            let hp = if use_hp { bd.read_bool(fc.mv_hp[comp]) as usize } else { 1 };
            if counting {
                counts.mv_fr[comp][fr] += 1;
                counts.mv_hp[comp][hp] += 1;
            }
            base + ((d << 3) | (fr << 1) | hp) + 1
        };
        if counting {
            counts.mv_sign[comp][sign as usize] += 1;
            counts.mv_class[comp][class] += 1;
        }
        if sign { -(mag as i32) } else { mag as i32 }
    }

    // ------------------------------------------------------------ motion vector prediction (6.5)

    #[inline]
    fn is_inside(&self, r: isize, c: isize) -> bool {
        r >= 0 && (r as usize) < self.mi_rows && c >= self.strip.mi_col_start as isize && (c as usize) < self.strip.mi_col_end
    }

    #[inline]
    fn add_mv_ref_list(&mut self, mv: Mv) {
        if self.ref_mv_count >= 2 {
            return;
        }
        if self.ref_mv_count > 0 && mv == self.ref_list_mv[0] {
            return;
        }
        self.ref_list_mv[self.ref_mv_count] = mv;
        self.ref_mv_count += 1;
    }

    fn if_same_ref_frame_add(&mut self, m: &MiInfo, ref_frame: i8) {
        for j in 0..2 {
            if m.ref_frame[j] == ref_frame {
                self.add_mv_ref_list(m.mv[j][3]);
                return;
            }
        }
    }

    fn if_diff_ref_frame_add(&mut self, m: &MiInfo, ref_frame: i8) {
        let bias = &self.h.ref_frame_sign_bias;
        let mvs = [m.mv[0][3], m.mv[1][3]];
        let same = mvs[0] == mvs[1];
        let scale = |f: i8, mv: Mv| -> Mv { if bias[f as usize] != bias[ref_frame as usize] { Mv::new(-(mv.row as i32), -(mv.col as i32)) } else { mv } };
        if m.ref_frame[0] > INTRA_FRAME && m.ref_frame[0] != ref_frame {
            let mv = scale(m.ref_frame[0], mvs[0]);
            self.add_mv_ref_list(mv);
        }
        if m.ref_frame[1] > INTRA_FRAME && m.ref_frame[1] != ref_frame && !same {
            let mv = scale(m.ref_frame[1], mvs[1]);
            self.add_mv_ref_list(mv);
        }
    }

    fn clamp_mv_row(&self, b: &Blk, v: i32, border: i32) -> i32 {
        let bh = NUM_8X8_HIGH[b.size as usize] as i32;
        let to_top = -((b.r as i32 * 8) * 8);
        let to_bottom = ((self.mi_rows as i32 - bh - b.r as i32) * 8) * 8;
        v.clamp(to_top - border, (to_bottom + border).max(to_top - border))
    }

    fn clamp_mv_col(&self, b: &Blk, v: i32, border: i32) -> i32 {
        let bw = NUM_8X8_WIDE[b.size as usize] as i32;
        let to_left = -((b.c as i32 * 8) * 8);
        let to_right = ((self.mi_cols as i32 - bw - b.c as i32) * 8) * 8;
        v.clamp(to_left - border, (to_right + border).max(to_left - border))
    }

    /// find_mv_refs (6.5.1) for `ref_frame`; `block` is -1 or the sub-8x8 block index.
    fn find_mv_refs(&mut self, b: &Blk, ref_frame: i8, block: i32) {
        self.ref_mv_count = 0;
        self.ref_list_mv = [Mv::ZERO; 2];
        let mut different_ref_found = false;
        let mut context_counter = 0usize;
        let search = &MV_REF_BLOCKS[b.size as usize * 16..b.size as usize * 16 + 16];
        let (r, c) = (b.r as isize, b.c as isize);
        for i in 0..2 {
            let (cr, cc) = (r + search[i * 2] as isize, c + search[i * 2 + 1] as isize);
            if self.is_inside(cr, cc) {
                let m = *self.mi_at(cr as usize, cc as usize);
                different_ref_found = true;
                context_counter += MODE_2_COUNTER[m.y_mode as usize] as usize;
                for j in 0..2 {
                    if m.ref_frame[j] == ref_frame {
                        let idx = if block >= 0 { IDX_N_COLUMN_TO_SUBBLOCK[block as usize][(search[i * 2 + 1] == 0) as usize] as usize } else { 3 };
                        self.add_mv_ref_list(m.mv[j][idx]);
                        break;
                    }
                }
            }
        }
        for i in 2..8 {
            let (cr, cc) = (r + search[i * 2] as isize, c + search[i * 2 + 1] as isize);
            if self.is_inside(cr, cc) {
                different_ref_found = true;
                let m = *self.mi_at(cr as usize, cc as usize);
                self.if_same_ref_frame_add(&m, ref_frame);
            }
        }
        let prev = self.s.prev_mi.map(|g| *g.at(b.r, b.c));
        if let Some(p) = &prev {
            self.if_same_ref_frame_add(p, ref_frame);
        }
        if different_ref_found {
            for i in 0..8 {
                let (cr, cc) = (r + search[i * 2] as isize, c + search[i * 2 + 1] as isize);
                if self.is_inside(cr, cc) {
                    let m = *self.mi_at(cr as usize, cc as usize);
                    self.if_diff_ref_frame_add(&m, ref_frame);
                }
            }
        }
        if let Some(p) = &prev {
            self.if_diff_ref_frame_add(p, ref_frame);
        }
        self.mode_context[ref_frame as usize] = COUNTER_TO_CONTEXT[context_counter.min(18)];
        for i in 0..2 {
            let mv = self.ref_list_mv[i];
            self.ref_list_mv[i] = Mv::new(self.clamp_mv_row(b, mv.row as i32, MV_BORDER), self.clamp_mv_col(b, mv.col as i32, MV_BORDER));
        }
    }

    /// find_best_ref_mvs (6.5.12).
    fn find_best_ref_mvs(&mut self, b: &Blk, ref_list: usize) {
        for i in 0..2 {
            let mv = self.ref_list_mv[i];
            let (mut dr, mut dc) = (mv.row as i32, mv.col as i32);
            if !self.h.allow_high_precision_mv || !use_mv_hp(mv) {
                if dr & 1 != 0 {
                    dr += if dr > 0 { -1 } else { 1 };
                }
                if dc & 1 != 0 {
                    dc += if dc > 0 { -1 } else { 1 };
                }
            }
            let border = (BORDERINPIXELS - INTERP_EXTEND) << 3;
            self.ref_list_mv[i] = Mv::new(self.clamp_mv_row(b, dr, border), self.clamp_mv_col(b, dc, border));
        }
        self.nearest_mv[ref_list] = self.ref_list_mv[0];
        self.near_mv[ref_list] = self.ref_list_mv[1];
        self.best_mv[ref_list] = self.ref_list_mv[0];
    }

    /// append_sub8x8_mvs (6.5.14).
    fn append_sub8x8_mvs(&mut self, b: &Blk, block: i32, ref_list: usize) {
        self.find_mv_refs(b, b.ref_frame[ref_list], block);
        let mut sub = [Mv::ZERO; 2];
        let mut dst;
        let bm = &b.block_mvs[ref_list];
        if block == 0 {
            sub = self.ref_list_mv;
            dst = 2;
        } else if block <= 2 {
            sub[0] = bm[0];
            dst = 1;
        } else {
            sub[0] = bm[2];
            dst = 1;
            for idx in [1usize, 0] {
                if dst < 2 && bm[idx] != sub[0] {
                    sub[dst] = bm[idx];
                    dst += 1;
                }
            }
        }
        for n in 0..2 {
            if dst < 2 && self.ref_list_mv[n] != sub[0] {
                sub[dst] = self.ref_list_mv[n];
                dst += 1;
            }
        }
        if dst < 2 {
            sub[dst] = Mv::ZERO;
        }
        self.nearest_mv[ref_list] = sub[0];
        self.near_mv[ref_list] = sub[1];
    }
}

#[inline]
pub fn use_mv_hp(mv: Mv) -> bool {
    ((mv.row as i32).abs() >> 3) < COMPANDED_MVREF_THRESH && ((mv.col as i32).abs() >> 3) < COMPANDED_MVREF_THRESH
}

//! Tile decoding: partitions, mode info, residual coefficients, prediction and reconstruction
//! (spec 5.11 syntax with the 8.3 CDF selection and 7.11 / 7.12 / 7.13 processes).

use crate::Result;
use crate::cdf::Cdfs;
use crate::frame::{MiInfo, Mv};
use crate::header::{FrameHeader, NONE, SequenceHeader};
use crate::intra::{IntraParams, is_directional_mode, predict_intra, round2_signed};
use crate::spec_tables::*;
use crate::state::{FrameShared, TileState};
use crate::symbol::SymbolDecoder;
use crate::transform::inverse_transform_2d;

/// Per-block state (the block-level variables of the syntax).
#[derive(Clone, Default)]
pub(crate) struct Block {
    pub mi_row: usize,
    pub mi_col: usize,
    pub mi_size: usize,
    pub bw4: usize,
    pub bh4: usize,
    pub has_chroma: bool,
    pub avail_u: bool,
    pub avail_l: bool,
    pub avail_u_chroma: bool,
    pub avail_l_chroma: bool,
    pub skip: bool,
    pub skip_mode: bool,
    pub segment_id: usize,
    pub lossless: bool,
    pub is_inter: bool,
    pub use_intrabc: bool,
    pub y_mode: usize,
    pub uv_mode: usize,
    pub angle_delta_y: i32,
    pub angle_delta_uv: i32,
    pub use_filter_intra: bool,
    pub filter_intra_mode: usize,
    pub cfl_alpha_u: i32,
    pub cfl_alpha_v: i32,
    pub palette_size_y: usize,
    pub palette_size_uv: usize,
    pub palette_colors_y: [u16; 8],
    pub palette_colors_u: [u16; 8],
    pub palette_colors_v: [u16; 8],
    pub tx_size: usize,
    pub ref_frame: [i8; 2],
    pub interp_filter: [u8; 2],
    pub motion_mode: u8,
    pub comp_group_idx: u8,
    pub compound_idx: u8,
    pub max_luma_w: usize,
    pub max_luma_h: usize,
    /// Mv[ ] / PredMv[ ] (row, col) for the two reference lists.
    pub mv_i: [[i32; 2]; 2],
    pub pred_mv: [[i32; 2]; 2],
    pub ref_mv_idx: usize,
    pub interintra: bool,
    pub interintra_mode: usize,
    pub wedge_interintra: bool,
    pub wedge_index: usize,
    pub wedge_sign: usize,
    pub mask_type: usize,
    pub compound_type: usize,
    pub local_valid: bool,
    pub local_warp_params: [i32; 6],
}

/// Decoder for one tile.
pub(crate) struct TileDecoder<'a, 'f> {
    pub fs: &'f FrameShared,
    pub t: &'f mut TileState,
    pub sd: SymbolDecoder<'a>,
    pub cdf: Box<Cdfs>,
    pub mi_row_start: usize,
    pub mi_row_end: usize,
    pub mi_col_start: usize,
    pub mi_col_end: usize,
    pub current_q_index: i32,
    pub delta_lf: [i32; 4],
    pub read_deltas: bool,
    pub ref_sgr_xqd: [[i32; 2]; 3],
    pub ref_lr_wiener: [[[i32; 3]; 2]; 3],
    /// BlockDecoded[ plane ][ y + 1 ][ x + 1 ] for y, x in -1..=32.
    pub block_decoded: [[[bool; 35]; 35]; 3],
    pub b: Block,
    pub mvs: crate::mvpred::MvStack,
    pub nb: crate::inter_info::Neighbours,
    /// Quant[] for the current transform block (raster order, stride per Adjusted size).
    quant: Vec<i32>,
    dequant: Vec<i32>,
    residual: Vec<i32>,
    color_map_y: Vec<u8>,
    color_map_uv: Vec<u8>,
    plane_tx_type: usize,
    pub pred_buf: [Vec<i32>; 2],
    pub mask: Vec<i32>,
    /// Intermediate rows of the sub-pixel filters (up to 2 x 128 + 8 rows of 128 with 2:1 scaled references).
    pub mc_tmp: Vec<i32>,
    /// Coefficient levels of the current transform block, padded (see TX_PAD).
    levels: Vec<u8>,
}

const SB_MAX: usize = 32; // superblock size in 4x4 units (128 / 4)

fn block_width(bs: usize) -> usize {
    4 * NUM_4X4_BLOCKS_WIDE[bs] as usize
}
fn block_height(bs: usize) -> usize {
    4 * NUM_4X4_BLOCKS_HIGH[bs] as usize
}

impl<'a, 'f> TileDecoder<'a, 'f> {
    pub fn new(fs: &'f FrameShared, t: &'f mut TileState, data: &'a [u8], cdf: Box<Cdfs>, tile_row: usize, tile_col: usize) -> Self {
        let ti = &fs.fh.tile_info;
        let (rs, re) = (ti.mi_row_starts[tile_row] as usize, ti.mi_row_starts[tile_row + 1] as usize);
        let (cs, ce) = (ti.mi_col_starts[tile_col] as usize, ti.mi_col_starts[tile_col + 1] as usize);
        let disable = fs.fh.disable_cdf_update;
        let q = fs.fh.quant.base_q_idx as i32;
        TileDecoder {
            fs,
            t,
            sd: SymbolDecoder::new(data, disable),
            cdf,
            mi_row_start: rs,
            mi_row_end: re,
            mi_col_start: cs,
            mi_col_end: ce,
            current_q_index: q,
            delta_lf: [0; 4],
            read_deltas: false,
            ref_sgr_xqd: [[0; 2]; 3],
            ref_lr_wiener: [[[0; 3]; 2]; 3],
            block_decoded: [[[false; 35]; 35]; 3],
            b: Block::default(),
            mvs: Default::default(),
            nb: Default::default(),
            quant: vec![0; 1024],
            dequant: vec![0; 64 * 64],
            residual: vec![0; 64 * 64],
            color_map_y: vec![0; 64 * 64],
            color_map_uv: vec![0; 64 * 64],
            plane_tx_type: 0,
            pred_buf: [vec![0; 128 * 128], vec![0; 128 * 128]],
            mask: vec![0; 128 * 128],
            mc_tmp: vec![0; (2 * 128 + 8) * 128],
            levels: vec![0; (32 + TX_PAD) * (32 + TX_PAD)],
        }
    }

    fn seq(&self) -> &SequenceHeader {
        &self.fs.seq
    }
    fn fh(&self) -> &FrameHeader {
        &self.fs.fh
    }

    #[inline]
    fn is_inside(&self, r: isize, c: isize) -> bool {
        c >= self.mi_col_start as isize && c < self.mi_col_end as isize && r >= self.mi_row_start as isize && r < self.mi_row_end as isize
    }

    /// decode_tile( )
    pub fn decode_tile(&mut self) -> Result<()> {
        // clear_above_context( )
        let t = &mut *self.t;
        for p in 0..3 {
            t.above_level[p].fill(0);
            t.above_dc[p].fill(0);
        }
        t.above_seg_pred.fill(0);
        self.delta_lf = [0; 4];
        for plane in 0..self.fs.num_planes {
            for pass in 0..2 {
                self.ref_sgr_xqd[plane][pass] = [-32, 31][pass];
                self.ref_lr_wiener[plane][pass] = [3, -7, 15];
            }
        }
        let sb128 = self.seq().use_128x128_superblock;
        let sb_size = if sb128 { BLOCK_128X128 } else { BLOCK_64X64 };
        let sb4 = NUM_4X4_BLOCKS_WIDE[sb_size] as usize;
        let mut r = self.mi_row_start;
        while r < self.mi_row_end {
            // clear_left_context( )
            let t = &mut *self.t;
            for p in 0..3 {
                t.left_level[p].fill(0);
                t.left_dc[p].fill(0);
            }
            t.left_seg_pred.fill(0);
            let mut c = self.mi_col_start;
            while c < self.mi_col_end {
                self.read_deltas = self.fh().delta_q_present;
                self.clear_cdef(r, c);
                self.clear_block_decoded_flags(r, c, sb4);
                self.read_lr(r, c, sb_size);
                self.decode_partition(r, c, sb_size)?;
                c += sb4;
            }
            r += sb4;
        }
        Ok(())
    }

    fn clear_cdef(&mut self, r: usize, c: usize) {
        let t = &mut *self.t;
        t.set_cdef_idx(r, c, -1);
        if self.fs.seq.use_128x128_superblock {
            t.set_cdef_idx(r, c + 16, -1);
            t.set_cdef_idx(r + 16, c, -1);
            t.set_cdef_idx(r + 16, c + 16, -1);
        }
    }

    fn clear_block_decoded_flags(&mut self, r: usize, c: usize, sb4: usize) {
        let (ssx, ssy) = (self.fs.ssx, self.fs.ssy);
        for plane in 0..self.fs.num_planes {
            let sub_x = if plane > 0 { ssx } else { 0 };
            let sub_y = if plane > 0 { ssy } else { 0 };
            let sb_w4 = (self.mi_col_end as isize - c as isize) >> sub_x;
            let sb_h4 = (self.mi_row_end as isize - r as isize) >> sub_y;
            let bd = &mut self.block_decoded[plane];
            for y in -1..=((sb4 >> sub_y) as isize) {
                for x in -1..=((sb4 >> sub_x) as isize) {
                    let v = if y < 0 && x < sb_w4 { true } else { x < 0 && y < sb_h4 };
                    bd[(y + 1) as usize][(x + 1) as usize] = v;
                }
            }
            bd[(sb4 >> sub_y) + 1][0] = false;
        }
    }

    #[inline]
    pub(crate) fn block_decoded_at(&self, plane: usize, y: isize, x: isize) -> bool {
        let (yy, xx) = (y + 1, x + 1);
        if yy < 0 || xx < 0 || yy > SB_MAX as isize + 1 || xx > SB_MAX as isize + 1 {
            return false;
        }
        self.block_decoded[plane][yy as usize][xx as usize]
    }

    fn decode_partition(&mut self, r: usize, c: usize, bsize: usize) -> Result<()> {
        let (mi_rows, mi_cols) = (self.fs.fh.mi_rows as usize, self.fs.fh.mi_cols as usize);
        if r >= mi_rows || c >= mi_cols {
            return Ok(());
        }
        let avail_u = self.is_inside(r as isize - 1, c as isize);
        let avail_l = self.is_inside(r as isize, c as isize - 1);
        let num4x4 = NUM_4X4_BLOCKS_WIDE[bsize] as usize;
        let half = num4x4 >> 1;
        let quarter = half >> 1;
        let has_rows = (r + half) < mi_rows;
        let has_cols = (c + half) < mi_cols;
        let partition = if bsize < BLOCK_8X8 {
            PARTITION_NONE
        } else {
            let bsl = MI_WIDTH_LOG2[bsize] as usize;
            let mi = &self.t.mi;
            let above = avail_u && (MI_WIDTH_LOG2[mi.mi_size[mi.idx(r - 1, c)] as usize] as usize) < bsl;
            let left = avail_l && (MI_HEIGHT_LOG2[mi.mi_size[mi.idx(r, c - 1)] as usize] as usize) < bsl;
            let ctx = left as usize * 2 + above as usize;
            if has_rows && has_cols {
                let cdf: &mut [u16] = match bsl {
                    1 => &mut self.cdf.partition_w8[ctx],
                    2 => &mut self.cdf.partition_w16[ctx],
                    3 => &mut self.cdf.partition_w32[ctx],
                    4 => &mut self.cdf.partition_w64[ctx],
                    _ => &mut self.cdf.partition_w128[ctx],
                };
                self.sd.read_symbol(cdf)
            } else if has_cols || has_rows {
                let pcdf: Vec<u16> = match bsl {
                    2 => self.cdf.partition_w16[ctx].to_vec(),
                    3 => self.cdf.partition_w32[ctx].to_vec(),
                    4 => self.cdf.partition_w64[ctx].to_vec(),
                    _ => self.cdf.partition_w128[ctx].to_vec(),
                };
                // pcdf holds cumulative values; P(k) = cdf[k] - cdf[k-1] (cdf[-1] = 0)
                let pr = |k: usize| pcdf[k] as i32 - if k == 0 { 0 } else { pcdf[k - 1] as i32 };
                let psum = if has_cols {
                    // split_or_horz
                    let mut s = pr(PARTITION_VERT) + pr(PARTITION_SPLIT) + pr(PARTITION_HORZ_A) + pr(PARTITION_VERT_A) + pr(PARTITION_VERT_B);
                    if bsize != BLOCK_128X128 {
                        s += pr(PARTITION_VERT_4);
                    }
                    s
                } else {
                    let mut s = pr(PARTITION_HORZ) + pr(PARTITION_SPLIT) + pr(PARTITION_HORZ_A) + pr(PARTITION_HORZ_B) + pr(PARTITION_VERT_A);
                    if bsize != BLOCK_128X128 {
                        s += pr(PARTITION_HORZ_4);
                    }
                    s
                };
                // The spec's inverse-CDF convention: cdf values count down from 32768.
                let _ = psum;
                let bit = self.read_split_bool(&pcdf, has_cols, bsize);
                if has_cols {
                    if bit { PARTITION_SPLIT } else { PARTITION_HORZ }
                } else if bit {
                    PARTITION_SPLIT
                } else {
                    PARTITION_VERT
                }
            } else {
                PARTITION_SPLIT
            }
        };
        let sub_size = PARTITION_SUBSIZE[partition][bsize] as usize;
        let split_size = PARTITION_SUBSIZE[PARTITION_SPLIT][bsize] as usize;
        match partition {
            PARTITION_NONE => self.decode_block(r, c, sub_size)?,
            PARTITION_HORZ => {
                self.decode_block(r, c, sub_size)?;
                if has_rows {
                    self.decode_block(r + half, c, sub_size)?;
                }
            }
            PARTITION_VERT => {
                self.decode_block(r, c, sub_size)?;
                if has_cols {
                    self.decode_block(r, c + half, sub_size)?;
                }
            }
            PARTITION_SPLIT => {
                self.decode_partition(r, c, sub_size)?;
                self.decode_partition(r, c + half, sub_size)?;
                self.decode_partition(r + half, c, sub_size)?;
                self.decode_partition(r + half, c + half, sub_size)?;
            }
            PARTITION_HORZ_A => {
                self.decode_block(r, c, split_size)?;
                self.decode_block(r, c + half, split_size)?;
                self.decode_block(r + half, c, sub_size)?;
            }
            PARTITION_HORZ_B => {
                self.decode_block(r, c, sub_size)?;
                self.decode_block(r + half, c, split_size)?;
                self.decode_block(r + half, c + half, split_size)?;
            }
            PARTITION_VERT_A => {
                self.decode_block(r, c, split_size)?;
                self.decode_block(r + half, c, split_size)?;
                self.decode_block(r, c + half, sub_size)?;
            }
            PARTITION_VERT_B => {
                self.decode_block(r, c, sub_size)?;
                self.decode_block(r, c + half, split_size)?;
                self.decode_block(r + half, c + half, split_size)?;
            }
            PARTITION_HORZ_4 => {
                for k in 0..4 {
                    let rr = r + quarter * k;
                    if k < 3 || rr < mi_rows {
                        self.decode_block(rr, c, sub_size)?;
                    }
                }
            }
            _ => {
                for k in 0..4 {
                    let cc = c + quarter * k;
                    if k < 3 || cc < mi_cols {
                        self.decode_block(r, cc, sub_size)?;
                    }
                }
            }
        }
        Ok(())
    }

    /// split_or_horz / split_or_vert: a binary symbol whose probability is derived from the
    /// partition CDF (8.3.2). Returns true for PARTITION_SPLIT.
    fn read_split_bool(&mut self, pcdf: &[u16], split_or_horz: bool, bsize: usize) -> bool {
        // The spec gives cdf values as P(X <= k) * 32768 (increasing); the probability of
        // partition k is cdf[k] - cdf[k - 1].
        let pr = |k: usize| pcdf[k] as i32 - if k == 0 { 0 } else { pcdf[k - 1] as i32 };
        let mut psum = if split_or_horz {
            pr(PARTITION_VERT) + pr(PARTITION_SPLIT) + pr(PARTITION_HORZ_A) + pr(PARTITION_VERT_A) + pr(PARTITION_VERT_B)
        } else {
            pr(PARTITION_HORZ) + pr(PARTITION_SPLIT) + pr(PARTITION_HORZ_A) + pr(PARTITION_HORZ_B) + pr(PARTITION_VERT_A)
        };
        if bsize != BLOCK_128X128 {
            psum += if split_or_horz { pr(PARTITION_VERT_4) } else { pr(PARTITION_HORZ_4) };
        }
        let mut cdf = [((1i32 << 15) - psum) as u16, 1 << 15, 0];
        // no adaptation: the array is constructed for this read only
        let save = self.sd.disable_update;
        self.sd.disable_update = true;
        let s = self.sd.read_symbol(&mut cdf);
        self.sd.disable_update = save;
        s == 1
    }

    fn decode_block(&mut self, r: usize, c: usize, sub_size: usize) -> Result<()> {
        let (ssx, ssy) = (self.fs.ssx, self.fs.ssy);
        let num_planes = self.fs.num_planes;
        let mut b = Block { mi_row: r, mi_col: c, mi_size: sub_size, ..Default::default() };
        b.bw4 = NUM_4X4_BLOCKS_WIDE[sub_size] as usize;
        b.bh4 = NUM_4X4_BLOCKS_HIGH[sub_size] as usize;
        b.has_chroma = if b.bh4 == 1 && ssy == 1 && (r & 1) == 0 || b.bw4 == 1 && ssx == 1 && (c & 1) == 0 { false } else { num_planes > 1 };
        b.avail_u = self.is_inside(r as isize - 1, c as isize);
        b.avail_l = self.is_inside(r as isize, c as isize - 1);
        b.avail_u_chroma = b.avail_u;
        b.avail_l_chroma = b.avail_l;
        if b.has_chroma {
            if ssy == 1 && b.bh4 == 1 {
                b.avail_u_chroma = self.is_inside(r as isize - 2, c as isize);
            }
            if ssx == 1 && b.bw4 == 1 {
                b.avail_l_chroma = self.is_inside(r as isize, c as isize - 2);
            }
        } else {
            b.avail_u_chroma = false;
            b.avail_l_chroma = false;
        }
        b.ref_frame = [INTRA_FRAME as i8, NONE];
        self.b = b;
        if self.fs.fh.frame_is_intra {
            self.intra_frame_mode_info()?;
        } else {
            self.inter_frame_mode_info()?;
        }
        self.palette_tokens();
        self.read_block_tx_size();
        if self.b.skip {
            self.reset_block_context();
        }
        let b = &self.b;
        let is_compound = b.ref_frame[1] > INTRA_FRAME as i8;
        {
            let mi = &mut self.t.mi;
            let rows = b.bh4.min(mi.rows.saturating_sub(r));
            let cols = b.bw4.min(mi.cols.saturating_sub(c));
            let set_uv = b.ref_frame[0] == INTRA_FRAME as i8 && b.has_chroma;
            let mvs = [Mv::new(b.mv_i[0][0], b.mv_i[0][1]), Mv::new(b.mv_i[1][0], b.mv_i[1][1])];
            for y in 0..rows {
                let i = mi.idx(r + y, c);
                let span = i..i + cols;
                mi.y_mode[span.clone()].fill(b.y_mode as u8);
                if set_uv {
                    mi.uv_mode[span.clone()].fill(b.uv_mode as u8);
                }
                mi.ref_frame[span.clone()].fill(b.ref_frame);
                mi.written[span.clone()].fill(true);
                if b.is_inter {
                    if !b.use_intrabc {
                        mi.comp_group_idx[span.clone()].fill(b.comp_group_idx);
                        mi.compound_idx[span.clone()].fill(b.compound_idx);
                    }
                    mi.interp_filter[span.clone()].fill(b.interp_filter);
                    for m in mi.mv[span].iter_mut() {
                        m[0] = mvs[0];
                        if is_compound {
                            m[1] = mvs[1];
                        }
                    }
                }
            }
        }
        self.compute_prediction()?;
        self.residual()?;
        let b = &self.b;
        let delta_lf = self.delta_lf;
        let mi = &mut self.t.mi;
        let rows = b.bh4.min(mi.rows.saturating_sub(r));
        let cols = b.bw4.min(mi.cols.saturating_sub(c));
        let dlf = [delta_lf[0] as i8, delta_lf[1] as i8, delta_lf[2] as i8, delta_lf[3] as i8];
        for y in 0..rows {
            let i = mi.idx(r + y, c);
            let span = i..i + cols;
            mi.is_inter[span.clone()].fill(b.is_inter);
            mi.skip_mode[span.clone()].fill(b.skip_mode);
            mi.skip[span.clone()].fill(b.skip);
            mi.tx_size[span.clone()].fill(b.tx_size as u8);
            mi.mi_size[span.clone()].fill(b.mi_size as u8);
            mi.segment_id[span.clone()].fill(b.segment_id as u8);
            if !mi.palette_size[0].is_empty() {
                mi.palette_size[0][span.clone()].fill(b.palette_size_y as u8);
                mi.palette_size[1][span.clone()].fill(b.palette_size_uv as u8);
                mi.palette_colors[0][span.clone()].fill(b.palette_colors_y);
                mi.palette_colors[1][span.clone()].fill(b.palette_colors_u);
            }
            mi.delta_lf[span.clone()].fill(dlf);
            mi.motion_mode[span].fill(b.motion_mode);
        }
        Ok(())
    }

    fn reset_block_context(&mut self) {
        let b = &self.b;
        let (ssx, ssy) = (self.fs.ssx, self.fs.ssy);
        for plane in 0..(1 + 2 * b.has_chroma as usize) {
            let sub_x = if plane > 0 { ssx } else { 0 };
            let sub_y = if plane > 0 { ssy } else { 0 };
            for i in (b.mi_col >> sub_x)..((b.mi_col + b.bw4) >> sub_x) {
                self.t.above_level[plane][i] = 0;
                self.t.above_dc[plane][i] = 0;
            }
            for i in (b.mi_row >> sub_y)..((b.mi_row + b.bh4) >> sub_y) {
                self.t.left_level[plane][i] = 0;
                self.t.left_dc[plane][i] = 0;
            }
        }
    }

    fn intra_frame_mode_info(&mut self) -> Result<()> {
        self.b.skip = false;
        let pre_skip = self.fs.fh.seg.seg_id_pre_skip;
        if pre_skip {
            self.intra_segment_id();
        }
        self.b.skip_mode = false;
        self.read_skip();
        if !pre_skip {
            self.intra_segment_id();
        }
        self.read_cdef();
        self.read_delta_qindex();
        self.read_delta_lf();
        self.read_deltas = false;
        self.b.ref_frame = [INTRA_FRAME as i8, NONE];
        self.b.use_intrabc = if self.fs.fh.allow_intrabc { self.sd.read_symbol(&mut self.cdf.intrabc) == 1 } else { false };
        if self.b.use_intrabc {
            self.b.is_inter = true;
            self.b.y_mode = DC_PRED;
            self.b.uv_mode = DC_PRED;
            self.b.motion_mode = SIMPLE as u8;
            self.b.compound_type = COMPOUND_AVERAGE;
            self.b.palette_size_y = 0;
            self.b.palette_size_uv = 0;
            self.b.interp_filter = [crate::header::BILINEAR; 2];
            self.find_mv_stack(false);
            self.assign_mv(false)?;
            return Ok(());
        }
        self.b.is_inter = false;
        let mi = &self.t.mi;
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let above = if self.b.avail_u { mi.y_mode[mi.idx(r - 1, c)] as usize } else { DC_PRED };
        let left = if self.b.avail_l { mi.y_mode[mi.idx(r, c - 1)] as usize } else { DC_PRED };
        let actx = INTRA_MODE_CONTEXT[above] as usize;
        let lctx = INTRA_MODE_CONTEXT[left] as usize;
        let mut cdf = self.t.intra_frame_y_mode_cdf[actx][lctx];
        self.b.y_mode = self.sd.read_symbol(&mut cdf);
        self.t.intra_frame_y_mode_cdf[actx][lctx] = cdf;
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

    pub(crate) fn read_uv_mode(&mut self) {
        let ms = self.b.mi_size;
        let cfl_allowed = if self.b.lossless && self.fs.plane_residual_size(ms, 1) == BLOCK_4X4 {
            true
        } else {
            !self.b.lossless && block_width(ms).max(block_height(ms)) <= 32
        };
        let y = self.b.y_mode;
        self.b.uv_mode =
            if cfl_allowed { self.sd.read_symbol(&mut self.cdf.uv_mode_cfl_allowed[y]) } else { self.sd.read_symbol(&mut self.cdf.uv_mode_cfl_not_allowed[y]) };
    }

    pub(crate) fn intra_angle_info_y(&mut self) {
        self.b.angle_delta_y = 0;
        if self.b.mi_size >= BLOCK_8X8 && is_directional_mode(self.b.y_mode) {
            let v = self.sd.read_symbol(&mut self.cdf.angle_delta[self.b.y_mode - V_PRED]);
            self.b.angle_delta_y = v as i32 - MAX_ANGLE_DELTA as i32;
        }
    }

    pub(crate) fn intra_angle_info_uv(&mut self) {
        self.b.angle_delta_uv = 0;
        if self.b.mi_size >= BLOCK_8X8 && is_directional_mode(self.b.uv_mode) {
            let v = self.sd.read_symbol(&mut self.cdf.angle_delta[self.b.uv_mode - V_PRED]);
            self.b.angle_delta_uv = v as i32 - MAX_ANGLE_DELTA as i32;
        }
    }

    pub(crate) fn read_cfl_alphas(&mut self) {
        let signs = self.sd.read_symbol(&mut self.cdf.cfl_sign);
        let sign_u = (signs + 1) / 3;
        let sign_v = (signs + 1) % 3;
        self.b.cfl_alpha_u = if sign_u != CFL_SIGN_ZERO {
            let ctx = (sign_u - 1) * 3 + sign_v;
            let a = 1 + self.sd.read_symbol(&mut self.cdf.cfl_alpha[ctx]) as i32;
            if sign_u == CFL_SIGN_NEG { -a } else { a }
        } else {
            0
        };
        self.b.cfl_alpha_v = if sign_v != CFL_SIGN_ZERO {
            let ctx = (sign_v - 1) * 3 + sign_u;
            let a = 1 + self.sd.read_symbol(&mut self.cdf.cfl_alpha[ctx]) as i32;
            if sign_v == CFL_SIGN_NEG { -a } else { a }
        } else {
            0
        };
    }

    fn intra_segment_id(&mut self) {
        if self.fs.fh.seg.enabled {
            self.read_segment_id();
        } else {
            self.b.segment_id = 0;
        }
        self.b.lossless = self.fs.fh.lossless_array[self.b.segment_id];
    }

    pub(crate) fn read_segment_id(&mut self) {
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let mi = &self.t.mi;
        let prev_ul = if self.b.avail_u && self.b.avail_l { mi.segment_id[mi.idx(r - 1, c - 1)] as i32 } else { -1 };
        let prev_u = if self.b.avail_u { mi.segment_id[mi.idx(r - 1, c)] as i32 } else { -1 };
        let prev_l = if self.b.avail_l { mi.segment_id[mi.idx(r, c - 1)] as i32 } else { -1 };
        let pred = if prev_u == -1 {
            if prev_l == -1 { 0 } else { prev_l }
        } else if prev_l == -1 || prev_ul == prev_u {
            prev_u
        } else {
            prev_l
        };
        if self.b.skip {
            self.b.segment_id = pred as usize;
        } else {
            let ctx = if prev_ul < 0 {
                0
            } else if prev_ul == prev_u && prev_ul == prev_l {
                2
            } else if prev_ul == prev_u || prev_ul == prev_l || prev_u == prev_l {
                1
            } else {
                0
            };
            let s = self.sd.read_symbol(&mut self.cdf.segment_id[ctx]) as i32;
            let max = self.fs.fh.seg.last_active_seg_id as i32 + 1;
            self.b.segment_id = neg_deinterleave(s, pred, max).clamp(0, 7) as usize;
        }
    }

    fn seg_feature_active(&self, feature: usize) -> bool {
        self.fs.fh.seg.enabled && self.fs.fh.seg.features.enabled[self.b.segment_id][feature]
    }

    pub(crate) fn read_skip(&mut self) {
        if self.fs.fh.seg.seg_id_pre_skip && self.seg_feature_active(SEG_LVL_SKIP) {
            self.b.skip = true;
        } else {
            let (r, c) = (self.b.mi_row, self.b.mi_col);
            let mi = &self.t.mi;
            let mut ctx = 0;
            if self.b.avail_u {
                ctx += mi.skip[mi.idx(r - 1, c)] as usize;
            }
            if self.b.avail_l {
                ctx += mi.skip[mi.idx(r, c - 1)] as usize;
            }
            self.b.skip = self.sd.read_symbol(&mut self.cdf.skip[ctx]) == 1;
        }
    }

    pub(crate) fn read_cdef(&mut self) {
        let fh = &self.fs.fh;
        if self.b.skip || fh.coded_lossless || !self.fs.seq.enable_cdef || fh.allow_intrabc {
            return;
        }
        let cdef_size4 = NUM_4X4_BLOCKS_WIDE[BLOCK_64X64] as usize;
        let mask = !(cdef_size4 - 1);
        let r = self.b.mi_row & mask;
        let c = self.b.mi_col & mask;
        if self.t.cdef_idx(r, c) == -1 {
            let bits = fh.cdef.bits;
            let v = self.sd.read_literal(bits) as i8;
            let w4 = self.b.bw4;
            let h4 = self.b.bh4;
            let mut i = r;
            while i < r + h4 {
                let mut j = c;
                while j < c + w4 {
                    self.t.set_cdef_idx(i, j, v);
                    j += cdef_size4;
                }
                i += cdef_size4;
            }
        }
    }

    pub(crate) fn read_delta_qindex(&mut self) {
        let sb_size = if self.fs.seq.use_128x128_superblock { BLOCK_128X128 } else { BLOCK_64X64 };
        if self.b.mi_size == sb_size && self.b.skip {
            return;
        }
        if self.read_deltas {
            let mut abs = self.sd.read_symbol(&mut self.cdf.delta_q) as i32;
            if abs == DELTA_Q_SMALL as i32 {
                let rem_bits = self.sd.read_literal(3) + 1;
                let abs_bits = self.sd.read_literal(rem_bits) as i32;
                abs = abs_bits + (1 << rem_bits) + 1;
            }
            if abs != 0 {
                let sign = self.sd.read_literal(1);
                let reduced = if sign == 1 { -abs } else { abs };
                self.current_q_index = (self.current_q_index + (reduced << self.fs.fh.delta_q_res)).clamp(1, 255);
            }
        }
    }

    pub(crate) fn read_delta_lf(&mut self) {
        let sb_size = if self.fs.seq.use_128x128_superblock { BLOCK_128X128 } else { BLOCK_64X64 };
        if self.b.mi_size == sb_size && self.b.skip {
            return;
        }
        let fh = &self.fs.fh;
        if self.read_deltas && fh.delta_lf_present {
            let count = if fh.delta_lf_multi { if self.fs.num_planes > 1 { FRAME_LF_COUNT } else { FRAME_LF_COUNT - 2 } } else { 1 };
            let multi = fh.delta_lf_multi;
            let res = fh.delta_lf_res;
            for i in 0..count {
                let mut abs = if multi { self.sd.read_symbol(&mut self.cdf.delta_lf_multi[i]) } else { self.sd.read_symbol(&mut self.cdf.delta_lf) } as i32;
                if abs == DELTA_LF_SMALL as i32 {
                    let n = self.sd.read_literal(3) + 1;
                    let bits = self.sd.read_literal(n) as i32;
                    abs = bits + (1 << n) + 1;
                }
                if abs != 0 {
                    let sign = self.sd.read_literal(1);
                    let reduced = if sign == 1 { -abs } else { abs };
                    self.delta_lf[i] = (self.delta_lf[i] + (reduced << res)).clamp(-(MAX_LOOP_FILTER as i32), MAX_LOOP_FILTER as i32);
                }
            }
        }
    }

    pub(crate) fn filter_intra_mode_info(&mut self) {
        self.b.use_filter_intra = false;
        let ms = self.b.mi_size;
        if self.fs.seq.enable_filter_intra && self.b.y_mode == DC_PRED && self.b.palette_size_y == 0 && block_width(ms).max(block_height(ms)) <= 32 {
            self.b.use_filter_intra = self.sd.read_symbol(&mut self.cdf.filter_intra[ms]) == 1;
            if self.b.use_filter_intra {
                self.b.filter_intra_mode = self.sd.read_symbol(&mut self.cdf.filter_intra_mode);
            }
        }
    }

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

    fn palette_tokens(&mut self) {
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

    // ---- transform size ---------------------------------------------------------------------

    fn read_block_tx_size(&mut self) {
        let b = &self.b;
        let (bw4, bh4) = (b.bw4, b.bh4);
        let fh = &self.fs.fh;
        if fh.tx_mode == TX_MODE_SELECT as u8 && b.mi_size > BLOCK_4X4 && b.is_inter && !b.skip && !b.lossless {
            let max_tx = MAX_TX_SIZE_RECT[b.mi_size] as usize;
            let tw4 = TX_WIDTH[max_tx] as usize / 4;
            let th4 = TX_HEIGHT[max_tx] as usize / 4;
            let (r0, c0) = (b.mi_row, b.mi_col);
            let mut row = r0;
            while row < r0 + bh4 {
                let mut col = c0;
                while col < c0 + bw4 {
                    self.read_var_tx_size(row, col, max_tx, 0);
                    col += tw4;
                }
                row += th4;
            }
        } else {
            let allow = !b.skip || !b.is_inter;
            self.read_tx_size(allow);
            let (r0, c0, ts) = (self.b.mi_row, self.b.mi_col, self.b.tx_size as u8);
            let mi = &mut self.t.mi;
            for row in r0..(r0 + bh4).min(mi.rows) {
                for col in c0..(c0 + bw4).min(mi.cols) {
                    let i = mi.idx(row, col);
                    mi.inter_tx_size[i] = ts;
                }
            }
        }
    }

    fn read_tx_size(&mut self, allow_select: bool) {
        if self.b.lossless {
            self.b.tx_size = TX_4X4;
            return;
        }
        let ms = self.b.mi_size;
        let max_rect = MAX_TX_SIZE_RECT[ms] as usize;
        let max_depth = MAX_TX_DEPTH_TABLE[ms] as usize;
        self.b.tx_size = max_rect;
        if ms > BLOCK_4X4 && allow_select && self.fs.fh.tx_mode == TX_MODE_SELECT as u8 {
            let ctx = self.tx_depth_ctx(max_rect);
            let depth = match max_depth {
                4 => self.sd.read_symbol(&mut self.cdf.tx_64x64[ctx]),
                3 => self.sd.read_symbol(&mut self.cdf.tx_32x32[ctx]),
                2 => self.sd.read_symbol(&mut self.cdf.tx_16x16[ctx]),
                _ => self.sd.read_symbol(&mut self.cdf.tx_8x8[ctx]),
            };
            for _ in 0..depth {
                self.b.tx_size = SPLIT_TX_SIZE[self.b.tx_size] as usize;
            }
        }
    }

    fn tx_depth_ctx(&self, max_rect: usize) -> usize {
        let (r, c) = (self.b.mi_row, self.b.mi_col);
        let mi = &self.t.mi;
        let max_w = TX_WIDTH[max_rect] as usize;
        let max_h = TX_HEIGHT[max_rect] as usize;
        let above_w = if self.b.avail_u && mi.is_inter[mi.idx(r - 1, c)] {
            block_width(mi.mi_size[mi.idx(r - 1, c)] as usize)
        } else if self.b.avail_u {
            self.get_above_tx_width(r, c)
        } else {
            0
        };
        let left_h = if self.b.avail_l && mi.is_inter[mi.idx(r, c - 1)] {
            block_height(mi.mi_size[mi.idx(r, c - 1)] as usize)
        } else if self.b.avail_l {
            self.get_left_tx_height(r, c)
        } else {
            0
        };
        (above_w >= max_w) as usize + (left_h >= max_h) as usize
    }

    fn get_above_tx_width(&self, row: usize, col: usize) -> usize {
        let mi = &self.t.mi;
        if row == self.b.mi_row {
            if !self.b.avail_u {
                return 64;
            } else if mi.skip[mi.idx(row - 1, col)] && mi.is_inter[mi.idx(row - 1, col)] {
                return block_width(mi.mi_size[mi.idx(row - 1, col)] as usize);
            }
        }
        TX_WIDTH[mi.inter_tx_size[mi.idx(row - 1, col)] as usize] as usize
    }

    fn get_left_tx_height(&self, row: usize, col: usize) -> usize {
        let mi = &self.t.mi;
        if col == self.b.mi_col {
            if !self.b.avail_l {
                return 64;
            } else if mi.skip[mi.idx(row, col - 1)] && mi.is_inter[mi.idx(row, col - 1)] {
                return block_height(mi.mi_size[mi.idx(row, col - 1)] as usize);
            }
        }
        TX_HEIGHT[mi.inter_tx_size[mi.idx(row, col - 1)] as usize] as usize
    }

    fn read_var_tx_size(&mut self, row: usize, col: usize, tx_sz: usize, depth: usize) {
        let (mi_rows, mi_cols) = (self.fs.fh.mi_rows as usize, self.fs.fh.mi_cols as usize);
        if row >= mi_rows || col >= mi_cols {
            return;
        }
        let split = if tx_sz == TX_4X4 || depth == MAX_VARTX_DEPTH {
            false
        } else {
            let above = self.get_above_tx_width(row, col) < TX_WIDTH[tx_sz] as usize;
            let left = self.get_left_tx_height(row, col) < TX_HEIGHT[tx_sz] as usize;
            let ms = self.b.mi_size;
            let size = 64.min(block_width(ms).max(block_height(ms)));
            let max_tx = find_tx_size(size, size);
            let sqr_up = TX_SIZE_SQR_UP[tx_sz] as usize;
            let ctx = (sqr_up != max_tx) as usize * 3 + (TX_SIZES - 1 - max_tx) * 6 + above as usize + left as usize;
            self.sd.read_symbol(&mut self.cdf.txfm_split[ctx]) == 1
        };
        let w4 = TX_WIDTH[tx_sz] as usize / 4;
        let h4 = TX_HEIGHT[tx_sz] as usize / 4;
        if split {
            let sub = SPLIT_TX_SIZE[tx_sz] as usize;
            let sw = TX_WIDTH[sub] as usize / 4;
            let sh = TX_HEIGHT[sub] as usize / 4;
            let mut i = 0;
            while i < h4 {
                let mut j = 0;
                while j < w4 {
                    self.read_var_tx_size(row + i, col + j, sub, depth + 1);
                    j += sw;
                }
                i += sh;
            }
        } else {
            let mi = &mut self.t.mi;
            for i in 0..h4 {
                for j in 0..w4 {
                    if row + i < mi.rows && col + j < mi.cols {
                        let k = mi.idx(row + i, col + j);
                        mi.inter_tx_size[k] = tx_sz as u8;
                    }
                }
            }
            self.b.tx_size = tx_sz;
        }
    }

    // ---- prediction and residual ----------------------------------------------------------

    /// compute_prediction( ) (5.11.33)
    fn compute_prediction(&mut self) -> Result<()> {
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

    fn residual(&mut self) -> Result<()> {
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

    // ---- coefficients -----------------------------------------------------------------------

    fn get_tx_set(&self, tx_sz: usize) -> usize {
        let sqr = TX_SIZE_SQR[tx_sz] as usize;
        let sqr_up = TX_SIZE_SQR_UP[tx_sz] as usize;
        if sqr_up > TX_32X32 {
            return TX_SET_DCTONLY;
        }
        let reduced = self.fs.fh.reduced_tx_set;
        if self.b.is_inter {
            if reduced || sqr_up == TX_32X32 {
                TX_SET_INTER_3
            } else if sqr == TX_16X16 {
                TX_SET_INTER_2
            } else {
                TX_SET_INTER_1
            }
        } else if sqr_up == TX_32X32 {
            TX_SET_DCTONLY
        } else if reduced || sqr == TX_16X16 {
            TX_SET_INTRA_2
        } else {
            TX_SET_INTRA_1
        }
    }

    fn is_tx_type_in_set(&self, set: usize, tx_type: usize) -> bool {
        if self.b.is_inter { TX_TYPE_IN_SET_INTER[set][tx_type] == 1 } else { TX_TYPE_IN_SET_INTRA[set][tx_type] == 1 }
    }

    fn compute_tx_type(&self, plane: usize, tx_sz: usize, block_x: usize, block_y: usize) -> usize {
        let sqr_up = TX_SIZE_SQR_UP[tx_sz] as usize;
        if self.b.lossless || sqr_up > TX_32X32 {
            return DCT_DCT;
        }
        let set = self.get_tx_set(tx_sz);
        let mi = &self.t.mi;
        if plane == 0 {
            return mi.tx_type[mi.idx(block_y, block_x)] as usize;
        }
        if self.b.is_inter {
            let x4 = self.b.mi_col.max(block_x << self.fs.ssx);
            let y4 = self.b.mi_row.max(block_y << self.fs.ssy);
            let t = mi.tx_type[mi.idx(y4, x4)] as usize;
            if !self.is_tx_type_in_set(set, t) {
                return DCT_DCT;
            }
            return t;
        }
        let t = MODE_TO_TXFM[self.b.uv_mode] as usize;
        if !self.is_tx_type_in_set(set, t) {
            return DCT_DCT;
        }
        t
    }

    fn transform_type(&mut self, x4: usize, y4: usize, tx_sz: usize) {
        let set = self.get_tx_set(tx_sz);
        let qidx =
            if self.fs.fh.seg.enabled { self.fs.fh.get_qindex(true, self.b.segment_id, self.current_q_index) } else { self.fs.fh.quant.base_q_idx as i32 };
        let tx_type = if set > 0 && qidx > 0 {
            let sqr = TX_SIZE_SQR[tx_sz] as usize;
            if self.b.is_inter {
                match set {
                    TX_SET_INTER_1 => TX_TYPE_INTER_INV_SET1[self.sd.read_symbol(&mut self.cdf.inter_tx_type_set1[sqr])] as usize,
                    TX_SET_INTER_2 => TX_TYPE_INTER_INV_SET2[self.sd.read_symbol(&mut self.cdf.inter_tx_type_set2)] as usize,
                    _ => TX_TYPE_INTER_INV_SET3[self.sd.read_symbol(&mut self.cdf.inter_tx_type_set3[sqr])] as usize,
                }
            } else {
                let intra_dir = if self.b.use_filter_intra { FILTER_INTRA_MODE_TO_INTRA_DIR[self.b.filter_intra_mode] as usize } else { self.b.y_mode };
                if set == TX_SET_INTRA_1 {
                    TX_TYPE_INTRA_INV_SET1[self.sd.read_symbol(&mut self.cdf.intra_tx_type_set1[sqr][intra_dir])] as usize
                } else {
                    TX_TYPE_INTRA_INV_SET2[self.sd.read_symbol(&mut self.cdf.intra_tx_type_set2[sqr][intra_dir])] as usize
                }
            }
        } else {
            DCT_DCT
        };
        self.set_tx_types(x4, y4, tx_sz, tx_type);
    }

    fn set_tx_types(&mut self, x4: usize, y4: usize, tx_sz: usize, tx_type: usize) {
        let mi = &mut self.t.mi;
        for i in 0..(TX_WIDTH[tx_sz] as usize >> 2) {
            for j in 0..(TX_HEIGHT[tx_sz] as usize >> 2) {
                if y4 + j < mi.rows && x4 + i < mi.cols {
                    let k = mi.idx(y4 + j, x4 + i);
                    mi.tx_type[k] = tx_type as u8;
                }
            }
        }
    }

    fn get_scan(&self, tx_sz: usize) -> &'static [u16] {
        if tx_sz == TX_16X64 {
            return &DEFAULT_SCAN_16X32;
        }
        if tx_sz == TX_64X16 {
            return &DEFAULT_SCAN_32X16;
        }
        if TX_SIZE_SQR_UP[tx_sz] as usize == TX_64X64 {
            return &DEFAULT_SCAN_32X32;
        }
        let t = self.plane_tx_type;
        if t == IDTX {
            return default_scan(tx_sz);
        }
        let prefer_row = t == V_DCT || t == V_ADST || t == V_FLIPADST;
        let prefer_col = t == H_DCT || t == H_ADST || t == H_FLIPADST;
        if prefer_row {
            match tx_sz {
                TX_4X4 => &MROW_SCAN_4X4,
                TX_4X8 => &MROW_SCAN_4X8,
                TX_8X4 => &MROW_SCAN_8X4,
                TX_8X8 => &MROW_SCAN_8X8,
                TX_8X16 => &MROW_SCAN_8X16,
                TX_16X8 => &MROW_SCAN_16X8,
                TX_16X16 => &MROW_SCAN_16X16,
                TX_4X16 => &MROW_SCAN_4X16,
                _ => &MROW_SCAN_16X4,
            }
        } else if prefer_col {
            match tx_sz {
                TX_4X4 => &MCOL_SCAN_4X4,
                TX_4X8 => &MCOL_SCAN_4X8,
                TX_8X4 => &MCOL_SCAN_8X4,
                TX_8X8 => &MCOL_SCAN_8X8,
                TX_8X16 => &MCOL_SCAN_8X16,
                TX_16X8 => &MCOL_SCAN_16X8,
                TX_16X16 => &MCOL_SCAN_16X16,
                TX_4X16 => &MCOL_SCAN_4X16,
                _ => &MCOL_SCAN_16X4,
            }
        } else {
            default_scan(tx_sz)
        }
    }

    /// coeffs( ): returns eob.
    fn coeffs(&mut self, plane: usize, start_x: usize, start_y: usize, tx_sz: usize) -> usize {
        let x4 = start_x >> 2;
        let y4 = start_y >> 2;
        let w4 = TX_WIDTH[tx_sz] as usize >> 2;
        let h4 = TX_HEIGHT[tx_sz] as usize >> 2;
        let tx_sz_ctx = (TX_SIZE_SQR[tx_sz] as usize + TX_SIZE_SQR_UP[tx_sz] as usize + 1) >> 1;
        let ptype = (plane > 0) as usize;
        let seg_eob = if tx_sz == TX_16X64 || tx_sz == TX_64X16 { 512 } else { 1024.min(TX_WIDTH[tx_sz] as usize * TX_HEIGHT[tx_sz] as usize) };
        self.quant[..seg_eob].iter_mut().for_each(|q| *q = 0);
        let mut eob = 0usize;
        let mut cul_level = 0u32;
        let mut dc_category = 0u8;
        let ctx = self.all_zero_ctx(plane, tx_sz, x4, y4, w4, h4);
        let all_zero = self.sd.read_symbol(&mut self.cdf.txb_skip[tx_sz_ctx][ctx]) == 1;
        if all_zero {
            if plane == 0 {
                self.set_tx_types(x4, y4, tx_sz, DCT_DCT);
            }
        } else {
            if plane == 0 {
                self.transform_type(x4, y4, tx_sz);
            }
            self.plane_tx_type = self.compute_tx_type(plane, tx_sz, x4, y4);
            let scan = self.get_scan(tx_sz);
            let eob_multisize = (TX_WIDTH_LOG2[tx_sz] as usize).min(5) + (TX_HEIGHT_LOG2[tx_sz] as usize).min(5) - 4;
            let tx_class = get_tx_class(self.plane_tx_type);
            let ectx = (tx_class != TX_CLASS_2D) as usize;
            let eob_pt = 1 + match eob_multisize {
                0 => self.sd.read_symbol(&mut self.cdf.eob_pt_16[ptype][ectx]),
                1 => self.sd.read_symbol(&mut self.cdf.eob_pt_32[ptype][ectx]),
                2 => self.sd.read_symbol(&mut self.cdf.eob_pt_64[ptype][ectx]),
                3 => self.sd.read_symbol(&mut self.cdf.eob_pt_128[ptype][ectx]),
                4 => self.sd.read_symbol(&mut self.cdf.eob_pt_256[ptype][ectx]),
                5 => self.sd.read_symbol(&mut self.cdf.eob_pt_512[ptype]),
                _ => self.sd.read_symbol(&mut self.cdf.eob_pt_1024[ptype]),
            };
            eob = if eob_pt < 2 { eob_pt } else { (1 << (eob_pt - 2)) + 1 };
            let eob_shift = eob_pt as i32 - 3;
            if eob_shift >= 0 {
                let extra = self.sd.read_symbol(&mut self.cdf.eob_extra[tx_sz_ctx][ptype][eob_pt - 3]);
                if extra == 1 {
                    eob += 1 << eob_shift;
                }
                for i in 1..(eob_pt as i32 - 2).max(0) {
                    let sh = (eob_pt as i32 - 2).max(0) - 1 - i;
                    if self.sd.read_literal(1) == 1 {
                        eob += 1 << sh;
                    }
                }
            }
            let adj = ADJUSTED_TX_SIZE[tx_sz] as usize;
            let bwl = TX_WIDTH_LOG2[adj] as usize;
            let height = TX_HEIGHT[adj] as usize;
            let width = 1usize << bwl;
            // Levels with TX_PAD zero columns / rows past the right / bottom edge, so the
            // neighbourhood sums need no bounds checks (out-of-range neighbours count as 0).
            let stride = width + TX_PAD;
            let mut lev = std::mem::take(&mut self.levels);
            lev[..(height + TX_PAD) * stride].fill(0);
            let sig = &SIG_REF_DIFF_OFFSET[tx_class];
            let sig_off: [usize; SIG_REF_DIFF_OFFSET_NUM] = std::array::from_fn(|i| sig[i][0] as usize * stride + sig[i][1] as usize);
            let magr = &MAG_REF_OFFSET_WITH_TX_CLASS[tx_class];
            let mag_off: [usize; 3] = std::array::from_fn(|i| magr[i][0] as usize * stride + magr[i][1] as usize);
            for c in (0..eob).rev() {
                let pos = scan[c] as usize;
                let row = pos >> bwl;
                let col = pos & (width - 1);
                let li = row * stride + col;
                let mut level = if c == eob - 1 {
                    let ctx = coeff_base_eob_ctx(c, bwl, height);
                    self.sd.read_symbol(&mut self.cdf.coeff_base_eob[tx_sz_ctx][ptype][ctx]) as i32 + 1
                } else {
                    let mut mag = 0u32;
                    for o in sig_off {
                        mag += (lev[li + o] as u32).min(3);
                    }
                    let ctx = coeff_base_ctx(tx_sz, tx_class, row, col, mag);
                    self.sd.read_symbol(&mut self.cdf.coeff_base[tx_sz_ctx][ptype][ctx]) as i32
                };
                if level > NUM_BASE_LEVELS as i32 {
                    let mut mag = 0u32;
                    for o in mag_off {
                        mag += (lev[li + o] as u32).min((COEFF_BASE_RANGE + NUM_BASE_LEVELS + 1) as u32);
                    }
                    let ctx = coeff_br_ctx(tx_class, row, col, pos, mag);
                    for _ in 0..(COEFF_BASE_RANGE / (BR_CDF_SIZE - 1)) {
                        let br = self.sd.read_symbol(&mut self.cdf.coeff_br[tx_sz_ctx.min(TX_32X32)][ptype][ctx]) as i32;
                        level += br;
                        if br < (BR_CDF_SIZE - 1) as i32 {
                            break;
                        }
                    }
                }
                self.quant[pos] = level;
                lev[li] = level as u8;
            }
            self.levels = lev;
            for c in 0..eob {
                let pos = scan[c] as usize;
                let sign = if self.quant[pos] != 0 {
                    if c == 0 {
                        let ctx = self.dc_sign_ctx(plane, x4, y4, w4, h4);
                        self.sd.read_symbol(&mut self.cdf.dc_sign[ptype][ctx]) == 1
                    } else {
                        self.sd.read_literal(1) == 1
                    }
                } else {
                    false
                };
                if self.quant[pos] > (NUM_BASE_LEVELS + COEFF_BASE_RANGE) as i32 {
                    let mut length = 0;
                    loop {
                        length += 1;
                        if self.sd.read_literal(1) == 1 {
                            break;
                        }
                        if length > 32 {
                            break;
                        }
                    }
                    let mut x: u32 = 1;
                    for _ in (0..length - 1).rev() {
                        x = (x << 1) | self.sd.read_literal(1);
                    }
                    self.quant[pos] = (x as i64 + COEFF_BASE_RANGE as i64 + NUM_BASE_LEVELS as i64).min(i32::MAX as i64) as i32;
                }
                if pos == 0 && self.quant[pos] > 0 {
                    dc_category = if sign { 1 } else { 2 };
                }
                self.quant[pos] &= 0xFFFFF;
                cul_level += self.quant[pos] as u32;
                if sign {
                    self.quant[pos] = -self.quant[pos];
                }
            }
            cul_level = cul_level.min(63);
        }
        for i in 0..w4 {
            if x4 + i < self.t.above_level[plane].len() {
                self.t.above_level[plane][x4 + i] = cul_level as u8;
                self.t.above_dc[plane][x4 + i] = dc_category;
            }
        }
        for i in 0..h4 {
            if y4 + i < self.t.left_level[plane].len() {
                self.t.left_level[plane][y4 + i] = cul_level as u8;
                self.t.left_dc[plane][y4 + i] = dc_category;
            }
        }
        eob
    }

    fn all_zero_ctx(&self, plane: usize, tx_sz: usize, x4: usize, y4: usize, w4: usize, h4: usize) -> usize {
        let mut max_x4 = self.fs.fh.mi_cols as usize;
        let mut max_y4 = self.fs.fh.mi_rows as usize;
        if plane > 0 {
            max_x4 >>= self.fs.ssx;
            max_y4 >>= self.fs.ssy;
        }
        let w = TX_WIDTH[tx_sz] as usize;
        let h = TX_HEIGHT[tx_sz] as usize;
        let bsize = self.fs.plane_residual_size(self.b.mi_size, plane);
        let bw = block_width(bsize);
        let bh = block_height(bsize);
        let al = &self.t.above_level[plane];
        let ll = &self.t.left_level[plane];
        if plane == 0 {
            let mut top = 0u32;
            let mut left = 0u32;
            for k in 0..w4 {
                if x4 + k < max_x4 {
                    top = top.max(al[x4 + k] as u32);
                }
            }
            for k in 0..h4 {
                if y4 + k < max_y4 {
                    left = left.max(ll[y4 + k] as u32);
                }
            }
            top = top.min(255);
            left = left.min(255);
            if bw == w && bh == h {
                0
            } else if top == 0 && left == 0 {
                1
            } else if top == 0 || left == 0 {
                2 + (top.max(left) > 3) as usize
            } else if top.max(left) <= 3 {
                4
            } else if top.min(left) <= 3 {
                5
            } else {
                6
            }
        } else {
            let ad = &self.t.above_dc[plane];
            let ld = &self.t.left_dc[plane];
            let mut above = 0u8;
            let mut left = 0u8;
            for i in 0..w4 {
                if x4 + i < max_x4 {
                    above |= al[x4 + i];
                    above |= ad[x4 + i];
                }
            }
            for i in 0..h4 {
                if y4 + i < max_y4 {
                    left |= ll[y4 + i];
                    left |= ld[y4 + i];
                }
            }
            let mut ctx = (above != 0) as usize + (left != 0) as usize;
            ctx += 7;
            if bw * bh > w * h {
                ctx += 3;
            }
            ctx
        }
    }

    fn dc_sign_ctx(&self, plane: usize, x4: usize, y4: usize, w4: usize, h4: usize) -> usize {
        let mut max_x4 = self.fs.fh.mi_cols as usize;
        let mut max_y4 = self.fs.fh.mi_rows as usize;
        if plane > 0 {
            max_x4 >>= self.fs.ssx;
            max_y4 >>= self.fs.ssy;
        }
        let mut dc_sign = 0i32;
        for k in 0..w4 {
            if x4 + k < max_x4 {
                match self.t.above_dc[plane][x4 + k] {
                    1 => dc_sign -= 1,
                    2 => dc_sign += 1,
                    _ => {}
                }
            }
        }
        for k in 0..h4 {
            if y4 + k < max_y4 {
                match self.t.left_dc[plane][y4 + k] {
                    1 => dc_sign -= 1,
                    2 => dc_sign += 1,
                    _ => {}
                }
            }
        }
        match dc_sign.signum() {
            -1 => 1,
            1 => 2,
            _ => 0,
        }
    }

    /// Reconstruct process (7.12.3).
    fn reconstruct(&mut self, plane: usize, x: usize, y: usize, tx_sz: usize) {
        let dq_denom: i64 = match tx_sz {
            TX_32X32 | TX_16X32 | TX_32X16 | TX_16X64 | TX_64X16 => 2,
            TX_64X64 | TX_32X64 | TX_64X32 => 4,
            _ => 1,
        };
        let log2w = TX_WIDTH_LOG2[tx_sz] as u32;
        let log2h = TX_HEIGHT_LOG2[tx_sz] as u32;
        let w = 1usize << log2w;
        let h = 1usize << log2h;
        let tw = w.min(32);
        let th = h.min(32);
        let t = self.plane_tx_type;
        let flip_ud = matches!(t, FLIPADST_DCT | FLIPADST_ADST | V_FLIPADST | FLIPADST_FLIPADST);
        let flip_lr = matches!(t, DCT_FLIPADST | ADST_FLIPADST | H_FLIPADST | FLIPADST_FLIPADST);
        let bd = self.fs.bit_depth;
        let fh = &self.fs.fh;
        let qindex = fh.get_qindex(false, self.b.segment_id, self.current_q_index);
        let (dc_delta, ac_delta) = match plane {
            0 => (fh.quant.delta_q_y_dc, 0),
            1 => (fh.quant.delta_q_u_dc, fh.quant.delta_q_u_ac),
            _ => (fh.quant.delta_q_v_dc, fh.quant.delta_q_v_ac),
        };
        let bd_idx = ((bd - 8) >> 1) as usize;
        let dc_q = DC_QLOOKUP[bd_idx][(qindex + dc_delta).clamp(0, 255) as usize] as i64;
        let ac_q = AC_QLOOKUP[bd_idx][(qindex + ac_delta).clamp(0, 255) as usize] as i64;
        let qm_level = if fh.quant.using_qmatrix { fh.seg_qm_level[plane][self.b.segment_id] } else { 15 };
        let use_qm = fh.quant.using_qmatrix && t < IDTX && qm_level < 15;
        let lim = 1i64 << (7 + bd);
        // inverse_transform_2d reads only the top-left tw x th coefficients (stride 64)
        for i in 0..th {
            self.dequant[i * 64..i * 64 + tw].fill(0);
        }
        for i in 0..th {
            for j in 0..tw {
                let qv = self.quant[i * tw + j];
                if qv == 0 {
                    continue;
                }
                let q = if i == 0 && j == 0 { dc_q } else { ac_q };
                let q2 = if use_qm {
                    let m = QUANTIZER_MATRIX[qm_level as usize][(plane > 0) as usize][QM_OFFSET[tx_sz] as usize + i * tw + j] as i64;
                    (q * m + 16) >> 5
                } else {
                    q
                };
                let dq = qv as i64 * q2;
                let mag = (dq.abs() & 0xFFFFFF) / dq_denom;
                let dq2 = if dq < 0 { -mag } else { mag };
                self.dequant[i * 64 + j] = dq2.clamp(-lim, lim - 1) as i32;
            }
        }
        let lossless = self.b.lossless;
        inverse_transform_2d(&self.dequant, &mut self.residual, tx_sz, t, lossless, bd);
        let pl = &mut self.t.cur.planes[plane];
        let max = (1i32 << bd) - 1;
        for i in 0..h {
            let yy = if flip_ud { h - i - 1 } else { i };
            let row = &mut pl.row_from_mut(y + yy, x)[..w];
            let res = &self.residual[i * w..i * w + w];
            if flip_lr {
                for (o, &r) in row.iter_mut().rev().zip(res) {
                    *o = (*o as i32 + r).max(0).min(max) as u16;
                }
            } else {
                for (o, &r) in row.iter_mut().zip(res) {
                    *o = (*o as i32 + r).max(0).min(max) as u16;
                }
            }
        }
    }

    // ---- loop restoration syntax -------------------------------------------------------------

    fn read_lr(&mut self, r: usize, c: usize, bsize: usize) {
        if self.fs.fh.allow_intrabc {
            return;
        }
        let w = NUM_4X4_BLOCKS_WIDE[bsize] as usize;
        let h = NUM_4X4_BLOCKS_HIGH[bsize] as usize;
        for plane in 0..self.fs.num_planes {
            if self.fs.fh.lr.frame_restoration_type[plane] == RESTORE_NONE as u8 {
                continue;
            }
            let sub_x = if plane == 0 { 0 } else { self.fs.ssx };
            let sub_y = if plane == 0 { 0 } else { self.fs.ssy };
            let unit_size = self.fs.fh.lr.loop_restoration_size[plane] as usize;
            let (unit_rows, unit_cols) = (self.fs.lr_unit_rows[plane], self.fs.lr_unit_cols[plane]);
            let unit_row_start = (r * (4 >> sub_y)).div_ceil(unit_size);
            let unit_row_end = unit_rows.min(((r + h) * (4 >> sub_y)).div_ceil(unit_size));
            let (numerator, denominator) =
                if self.fs.fh.use_superres { ((4 >> sub_x) * self.fs.fh.superres_denom as usize, unit_size * SUPERRES_NUM) } else { (4 >> sub_x, unit_size) };
            let unit_col_start = (c * numerator).div_ceil(denominator);
            let unit_col_end = unit_cols.min(((c + w) * numerator).div_ceil(denominator));
            for unit_row in unit_row_start..unit_row_end {
                for unit_col in unit_col_start..unit_col_end {
                    self.read_lr_unit(plane, unit_row, unit_col);
                }
            }
        }
    }

    fn read_lr_unit(&mut self, plane: usize, unit_row: usize, unit_col: usize) {
        let frt = self.fs.fh.lr.frame_restoration_type[plane];
        let rtype = if frt == RESTORE_WIENER as u8 {
            if self.sd.read_symbol(&mut self.cdf.use_wiener) == 1 { RESTORE_WIENER as u8 } else { RESTORE_NONE as u8 }
        } else if frt == RESTORE_SGRPROJ as u8 {
            if self.sd.read_symbol(&mut self.cdf.use_sgrproj) == 1 { RESTORE_SGRPROJ as u8 } else { RESTORE_NONE as u8 }
        } else {
            // restoration_type symbol: 0 = NONE, 1 = WIENER, 2 = SGRPROJ
            match self.sd.read_symbol(&mut self.cdf.restoration_type) {
                0 => RESTORE_NONE as u8,
                1 => RESTORE_WIENER as u8,
                _ => RESTORE_SGRPROJ as u8,
            }
        };
        let idx = unit_row * self.fs.lr_unit_cols[plane] + unit_col;
        self.t.lr.lr_type[plane][idx] = rtype;
        self.t.lr_written[plane].push(idx);
        if rtype == RESTORE_WIENER as u8 {
            const MIN: [i32; 3] = [-5, -23, -17];
            const MAX: [i32; 3] = [10, 8, 46];
            const K: [u32; 3] = [1, 2, 3];
            for pass in 0..2 {
                let first = if plane > 0 {
                    self.t.lr.lr_wiener[plane][idx][pass][0] = 0;
                    1
                } else {
                    0
                };
                for j in first..3 {
                    let v = self.decode_signed_subexp_with_ref_bool(MIN[j], MAX[j] + 1, K[j], self.ref_lr_wiener[plane][pass][j]);
                    self.t.lr.lr_wiener[plane][idx][pass][j] = v as i8;
                    self.ref_lr_wiener[plane][pass][j] = v;
                }
            }
        } else if rtype == RESTORE_SGRPROJ as u8 {
            let set = self.sd.read_literal(SGRPROJ_PARAMS_BITS as u32) as usize;
            self.t.lr.lr_sgr_set[plane][idx] = set as u8;
            const MIN: [i32; 2] = [-96, -32];
            const MAX: [i32; 2] = [31, 95];
            for i in 0..2 {
                let radius = SGR_PARAMS[set][i * 2];
                let v = if radius != 0 {
                    self.decode_signed_subexp_with_ref_bool(MIN[i], MAX[i] + 1, SGRPROJ_PRJ_SUBEXP_K as u32, self.ref_sgr_xqd[plane][i])
                } else if i == 1 {
                    ((1 << SGRPROJ_PRJ_BITS) - self.ref_sgr_xqd[plane][0]).clamp(MIN[i], MAX[i])
                } else {
                    0
                };
                self.t.lr.lr_sgr_xqd[plane][idx][i] = v as i8;
                self.ref_sgr_xqd[plane][i] = v;
            }
        }
    }

    fn decode_signed_subexp_with_ref_bool(&mut self, low: i32, high: i32, k: u32, r: i32) -> i32 {
        let x = self.decode_unsigned_subexp_with_ref_bool(high - low, k, r - low);
        x + low
    }

    fn decode_unsigned_subexp_with_ref_bool(&mut self, mx: i32, k: u32, r: i32) -> i32 {
        let v = self.decode_subexp_bool(mx, k);
        if (r << 1) <= mx { inverse_recenter(r, v) } else { mx - 1 - inverse_recenter(mx - 1 - r, v) }
    }

    fn decode_subexp_bool(&mut self, num_syms: i32, k: u32) -> i32 {
        let mut i = 0u32;
        let mut mk = 0i32;
        loop {
            let b2 = if i > 0 { k + i - 1 } else { k };
            let a = 1i32 << b2;
            if num_syms <= mk + 3 * a {
                return self.sd.read_ns((num_syms - mk) as u32) as i32 + mk;
            } else if self.sd.read_literal(1) == 1 {
                i += 1;
                mk += a;
            } else {
                return self.sd.read_literal(b2) as i32 + mk;
            }
        }
    }

    fn inter_frame_mode_info(&mut self) -> Result<()> {
        self.inter_frame_mode_info_impl()
    }
}

fn inverse_recenter(r: i32, v: i32) -> i32 {
    if v > 2 * r {
        v
    } else if v & 1 == 1 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

fn default_scan(tx_sz: usize) -> &'static [u16] {
    match tx_sz {
        TX_4X4 => &DEFAULT_SCAN_4X4,
        TX_4X8 => &DEFAULT_SCAN_4X8,
        TX_8X4 => &DEFAULT_SCAN_8X4,
        TX_8X8 => &DEFAULT_SCAN_8X8,
        TX_8X16 => &DEFAULT_SCAN_8X16,
        TX_16X8 => &DEFAULT_SCAN_16X8,
        TX_16X16 => &DEFAULT_SCAN_16X16,
        TX_16X32 => &DEFAULT_SCAN_16X32,
        TX_32X16 => &DEFAULT_SCAN_32X16,
        TX_4X16 => &DEFAULT_SCAN_4X16,
        TX_16X4 => &DEFAULT_SCAN_16X4,
        TX_8X32 => &DEFAULT_SCAN_8X32,
        TX_32X8 => &DEFAULT_SCAN_32X8,
        _ => &DEFAULT_SCAN_32X32,
    }
}

fn get_tx_class(t: usize) -> usize {
    if t == V_DCT || t == V_ADST || t == V_FLIPADST {
        TX_CLASS_VERT
    } else if t == H_DCT || t == H_ADST || t == H_FLIPADST {
        TX_CLASS_HORIZ
    } else {
        TX_CLASS_2D
    }
}

/// Coefficient base context from the neighbourhood magnitude `mag` (get_coeff_base_ctx).
#[inline(always)]
fn coeff_base_ctx(tx_sz: usize, tx_class: usize, row: usize, col: usize, mag: u32) -> usize {
    let ctx = ((mag + 1) >> 1).min(4) as usize;
    if tx_class == TX_CLASS_2D {
        if row == 0 && col == 0 {
            return 0;
        }
        return ctx + COEFF_BASE_CTX_OFFSET[tx_sz][row.min(4)][col.min(4)] as usize;
    }
    let idx = if tx_class == TX_CLASS_VERT { row } else { col };
    ctx + COEFF_BASE_POS_CTX_OFFSET[idx.min(2)] as usize
}

/// coeff_br context from the neighbourhood magnitude `mag`.
#[inline(always)]
fn coeff_br_ctx(tx_class: usize, row: usize, col: usize, pos: usize, mag: u32) -> usize {
    let mag = ((mag + 1) >> 1).min(6) as usize;
    if pos == 0 {
        mag
    } else if tx_class == 0 {
        if row < 2 && col < 2 { mag + 7 } else { mag + 14 }
    } else if tx_class == 1 {
        if col == 0 { mag + 7 } else { mag + 14 }
    } else if row == 0 {
        mag + 7
    } else {
        mag + 14
    }
}

/// Zero padding (columns and rows) of the coefficient level buffer.
const TX_PAD: usize = 4;

fn coeff_base_eob_ctx(c: usize, bwl: usize, height: usize) -> usize {
    if c == 0 {
        return 0;
    }
    if c <= (height << bwl) / 8 {
        return 1;
    }
    if c <= (height << bwl) / 4 {
        return 2;
    }
    3
}

pub(crate) fn find_tx_size(w: usize, h: usize) -> usize {
    (0..TX_SIZES_ALL).find(|&t| TX_WIDTH[t] as usize == w && TX_HEIGHT[t] as usize == h).unwrap_or(TX_4X4)
}

fn ceil_log2(x: u32) -> u32 {
    if x < 2 {
        return 0;
    }
    let mut i = 1;
    let mut p = 2;
    while p < x {
        i += 1;
        p <<= 1;
    }
    i
}

fn neg_deinterleave(diff: i32, r: i32, max: i32) -> i32 {
    if r == 0 {
        return diff;
    }
    if r >= max - 1 {
        return max - diff - 1;
    }
    if 2 * r < max {
        if diff <= 2 * r {
            if diff & 1 == 1 {
                return r + ((diff + 1) >> 1);
            } else {
                return r - (diff >> 1);
            }
        }
        diff
    } else {
        if diff <= 2 * (max - r - 1) {
            if diff & 1 == 1 {
                return r + ((diff + 1) >> 1);
            } else {
                return r - (diff >> 1);
            }
        }
        max - (diff + 1)
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

#[allow(dead_code)]
fn _unused(_: &MiInfo) {}

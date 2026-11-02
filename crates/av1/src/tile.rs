//! Tile decoding: partitions, mode info, residual coefficients, prediction and reconstruction
//! (spec 5.11 syntax with the 8.3 CDF selection and 7.11 / 7.12 / 7.13 processes).

use crate::Result;
use crate::cdf::Cdfs;
use crate::frame::{MiInfo, Mv};
use crate::header::{FrameHeader, NONE, SequenceHeader};
use crate::intra::{IntraParams, is_directional_mode, predict_intra, round2_signed};
use crate::spec_tables::{
    AC_QLOOKUP, ADJUSTED_TX_SIZE, ADST_FLIPADST, BLOCK_4X4, BLOCK_8X8, BLOCK_64X64, BLOCK_128X128, BLOCK_INVALID, BR_CDF_SIZE, CFL_SIGN_NEG, CFL_SIGN_ZERO,
    COEFF_BASE_CTX_OFFSET, COEFF_BASE_POS_CTX_OFFSET, COEFF_BASE_RANGE, COMPOUND_AVERAGE, DC_PRED, DC_QLOOKUP, DCT_DCT, DCT_FLIPADST, DEFAULT_SCAN_4X4,
    DEFAULT_SCAN_4X8, DEFAULT_SCAN_4X16, DEFAULT_SCAN_8X4, DEFAULT_SCAN_8X8, DEFAULT_SCAN_8X16, DEFAULT_SCAN_8X32, DEFAULT_SCAN_16X4, DEFAULT_SCAN_16X8,
    DEFAULT_SCAN_16X16, DEFAULT_SCAN_16X32, DEFAULT_SCAN_32X8, DEFAULT_SCAN_32X16, DEFAULT_SCAN_32X32, DELTA_LF_SMALL, DELTA_Q_SMALL,
    FILTER_INTRA_MODE_TO_INTRA_DIR, FLIPADST_ADST, FLIPADST_DCT, FLIPADST_FLIPADST, FRAME_LF_COUNT, H_ADST, H_DCT, H_FLIPADST, H_PRED, IDTX, II_DC_PRED,
    II_H_PRED, II_V_PRED, INTRA_FRAME, INTRA_MODE_CONTEXT, MAG_REF_OFFSET_WITH_TX_CLASS, MAX_ANGLE_DELTA, MAX_LOOP_FILTER, MAX_TX_DEPTH_TABLE,
    MAX_TX_SIZE_RECT, MAX_VARTX_DEPTH, MCOL_SCAN_4X4, MCOL_SCAN_4X8, MCOL_SCAN_4X16, MCOL_SCAN_8X4, MCOL_SCAN_8X8, MCOL_SCAN_8X16, MCOL_SCAN_16X4,
    MCOL_SCAN_16X8, MCOL_SCAN_16X16, MI_HEIGHT_LOG2, MI_WIDTH_LOG2, MODE_TO_TXFM, MROW_SCAN_4X4, MROW_SCAN_4X8, MROW_SCAN_4X16, MROW_SCAN_8X4, MROW_SCAN_8X8,
    MROW_SCAN_8X16, MROW_SCAN_16X4, MROW_SCAN_16X8, MROW_SCAN_16X16, NUM_4X4_BLOCKS_HIGH, NUM_4X4_BLOCKS_WIDE, NUM_BASE_LEVELS, PALETTE_COLOR_CONTEXT,
    PALETTE_COLOR_HASH_MULTIPLIERS, PALETTE_COLORS, PALETTE_NUM_NEIGHBORS, PARTITION_HORZ, PARTITION_HORZ_4, PARTITION_HORZ_A, PARTITION_HORZ_B,
    PARTITION_NONE, PARTITION_SPLIT, PARTITION_SUBSIZE, PARTITION_VERT, PARTITION_VERT_4, PARTITION_VERT_A, PARTITION_VERT_B, QM_OFFSET, QUANTIZER_MATRIX,
    RESTORE_NONE, RESTORE_SGRPROJ, RESTORE_WIENER, SEG_LVL_SKIP, SGR_PARAMS, SGRPROJ_PARAMS_BITS, SGRPROJ_PRJ_BITS, SGRPROJ_PRJ_SUBEXP_K, SIG_REF_DIFF_OFFSET,
    SIG_REF_DIFF_OFFSET_NUM, SIMPLE, SMOOTH_H_PRED, SMOOTH_PRED, SMOOTH_V_PRED, SPLIT_TX_SIZE, SUPERRES_NUM, TX_4X4, TX_4X8, TX_4X16, TX_8X4, TX_8X8, TX_8X16,
    TX_8X32, TX_16X4, TX_16X8, TX_16X16, TX_16X32, TX_16X64, TX_32X8, TX_32X16, TX_32X32, TX_32X64, TX_64X16, TX_64X32, TX_64X64, TX_CLASS_2D, TX_CLASS_HORIZ,
    TX_CLASS_VERT, TX_HEIGHT, TX_HEIGHT_LOG2, TX_MODE_SELECT, TX_SET_DCTONLY, TX_SET_INTER_1, TX_SET_INTER_2, TX_SET_INTER_3, TX_SET_INTRA_1, TX_SET_INTRA_2,
    TX_SIZE_SQR, TX_SIZE_SQR_UP, TX_SIZES, TX_SIZES_ALL, TX_TYPE_IN_SET_INTER, TX_TYPE_IN_SET_INTRA, TX_TYPE_INTER_INV_SET1, TX_TYPE_INTER_INV_SET2,
    TX_TYPE_INTER_INV_SET3, TX_TYPE_INTRA_INV_SET1, TX_TYPE_INTRA_INV_SET2, TX_WIDTH, TX_WIDTH_LOG2, UV_CFL_PRED, V_ADST, V_DCT, V_FLIPADST, V_PRED,
};
use crate::state::{FrameShared, TileState};
use crate::symbol::SymbolDecoder;
use crate::transform::inverse_transform_2d;

mod coeffs;
mod loop_restoration;
mod palette;
mod predict;
mod tx_size;

use coeffs::{TX_PAD, neg_deinterleave};

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
    pub mv: [Mv; 2],
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
    /// Quant[] for the current transform block (raster order, stride per Adjusted size); all
    /// zero between transform blocks (`reconstruct` clears what it reads).
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
        // Conformance requirement (4:2:2 has no chroma size for 4x16 and similar blocks); indexing the size tables with BLOCK_INVALID would panic.
        if num_planes > 1 && self.fs.plane_residual_size(sub_size, 1) == BLOCK_INVALID {
            return Err(crate::Error::Invalid("block size has no chroma size at this subsampling"));
        }
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

    fn inter_frame_mode_info(&mut self) -> Result<()> {
        self.inter_frame_mode_info_impl()
    }
}

#[allow(dead_code)]
fn _unused(_: &MiInfo) {}

//! Per-frame decoding state shared by the tiles and the post-filters.
//!
//! [`FrameShared`] holds what tiles only read (headers, references, projected motion field);
//! [`TileState`] what a tile writes (reconstruction, mode info, per-block filter parameters,
//! entropy contexts). With one tile the frame's own buffers move into the tile state and back;
//! with several, each tile decodes into private buffers covering its area (tiles never read
//! outside themselves), which are then copied into the frame.

use crate::decoder::RefFrame;
use crate::frame::{FrameBuf, MiInfo};
use crate::header::{FrameHeader, RefInfo, SequenceHeader};
use crate::spec_tables::{DEFAULT_INTRA_FRAME_Y_MODE_CDF, NUM_REF_FRAMES, RESTORE_NONE, SUBSAMPLED_SIZE};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;

/// Read-only (during tile decoding) frame state.
pub(crate) struct FrameShared {
    pub seq: SequenceHeader,
    pub fh: FrameHeader,
    pub num_planes: usize,
    pub ssx: usize,
    pub ssy: usize,
    pub bit_depth: u32,
    pub lr_unit_rows: [usize; 3],
    pub lr_unit_cols: [usize; 3],
    pub cdef_cols: usize,
    /// Reference slots (FrameStore and saved state) used by this frame.
    pub refs: [Option<Arc<RefFrame>>; NUM_REF_FRAMES],
    pub ref_info: [RefInfo; NUM_REF_FRAMES],
    /// MotionFieldMvs[ ref ][ y8 * w8 + x8 ] (7.9); empty when unused.
    pub motion_field: [Vec<[i32; 2]>; 8],
    /// PrevSegmentIds (mi units).
    pub prev_segment_ids: Vec<u8>,
}

impl FrameShared {
    pub fn plane_residual_size(&self, subsize: usize, plane: usize) -> usize {
        let sx = if plane > 0 { self.ssx } else { 0 };
        let sy = if plane > 0 { self.ssy } else { 0 };
        SUBSAMPLED_SIZE[subsize][sx][sy] as usize
    }
}

/// A byte per 4x4 unit of a plane (LoopfilterTxSizes), possibly for a region only.
#[derive(Clone, Default)]
pub(crate) struct Grid8 {
    pub data: Vec<u8>,
    pub ox: usize,
    pub oy: usize,
    pub stride: usize,
    pub rows: usize,
}

impl Grid8 {
    fn new(ox: usize, oy: usize, stride: usize, rows: usize) -> Grid8 {
        Grid8 { data: vec![0; stride * rows], ox, oy, stride, rows }
    }
    #[inline]
    pub fn get(&self, row: usize, col: usize) -> u8 {
        self.data[(row - self.oy) * self.stride + col - self.ox]
    }
    /// Stores outside the grid are dropped (blocks reaching past the allocated margin).
    #[inline]
    pub fn set(&mut self, row: usize, col: usize, v: u8) {
        if col >= self.ox && col < self.ox + self.stride && row >= self.oy && row < self.oy + self.rows {
            self.data[(row - self.oy) * self.stride + col - self.ox] = v;
        }
    }
    fn copy_into(&self, dst: &mut Grid8) {
        for y in 0..self.rows {
            let d = (self.oy + y - dst.oy) * dst.stride + self.ox - dst.ox;
            dst.data[d..d + self.stride].copy_from_slice(&self.data[y * self.stride..(y + 1) * self.stride]);
        }
    }
}

/// Loop restoration parameters per unit.
#[derive(Clone, Default)]
pub(crate) struct LrUnits {
    pub lr_type: [Vec<u8>; 3],
    pub lr_wiener: [Vec<[[i8; 3]; 2]>; 3],
    pub lr_sgr_set: [Vec<u8>; 3],
    pub lr_sgr_xqd: [Vec<[i8; 2]>; 3],
}

/// What tile decoding writes.
pub(crate) struct TileState {
    pub cur: FrameBuf,
    pub mi: MiInfo,
    /// cdef_idx per 64x64 block (-1: not coded).
    pub cdef: Vec<i8>,
    /// LoopfilterTxSizes[ plane ][ row ][ col ] in 4x4 units of each plane.
    pub lf_tx_size: [Grid8; 3],
    pub lr: LrUnits,
    /// LR units this tile coded (per plane), for merging.
    pub lr_written: [Vec<usize>; 3],
    pub above_level: [Vec<u8>; 3],
    pub above_dc: [Vec<u8>; 3],
    pub above_seg_pred: Vec<u8>,
    pub left_level: [Vec<u8>; 3],
    pub left_dc: [Vec<u8>; 3],
    pub left_seg_pred: Vec<u8>,
    /// TileIntraFrameYModeCdf (reset per tile).
    pub intra_frame_y_mode_cdf: [[[u16; 14]; 5]; 5],
    cdef_cols: usize,
}

impl TileState {
    fn with(cur: FrameBuf, mi: MiInfo, cdef: Vec<i8>, lf_tx_size: [Grid8; 3], lr: LrUnits, cdef_cols: usize) -> TileState {
        let (mi_cols, mi_rows) = (mi.cols, mi.rows);
        TileState {
            cur,
            mi,
            cdef,
            lf_tx_size,
            lr,
            lr_written: Default::default(),
            above_level: std::array::from_fn(|_| vec![0; mi_cols + 64]),
            above_dc: std::array::from_fn(|_| vec![0; mi_cols + 64]),
            above_seg_pred: vec![0; mi_cols + 64],
            left_level: std::array::from_fn(|_| vec![0; mi_rows + 64]),
            left_dc: std::array::from_fn(|_| vec![0; mi_rows + 64]),
            left_seg_pred: vec![0; mi_rows + 64],
            intra_frame_y_mode_cdf: DEFAULT_INTRA_FRAME_Y_MODE_CDF,
            cdef_cols,
        }
    }

    #[inline]
    pub fn cdef_idx(&self, r: usize, c: usize) -> i8 {
        self.cdef[(r >> 4) * self.cdef_cols + (c >> 4)]
    }

    #[inline]
    pub fn set_cdef_idx(&mut self, r: usize, c: usize, v: i8) {
        let i = (r >> 4) * self.cdef_cols + (c >> 4);
        if i < self.cdef.len() {
            self.cdef[i] = v;
        }
    }

    #[inline]
    pub fn set_lf_tx_size(&mut self, plane: usize, row: usize, col: usize, v: u8) {
        self.lf_tx_size[plane].set(row, col, v);
    }
}

pub(crate) struct FrameState {
    pub sh: FrameShared,
    pub cur: FrameBuf,
    pub mi: MiInfo,
    cdef: Vec<i8>,
    lf_tx_size: [Grid8; 3],
    pub lr: LrUnits,
}

impl Deref for FrameState {
    type Target = FrameShared;
    fn deref(&self) -> &FrameShared {
        &self.sh
    }
}

impl DerefMut for FrameState {
    fn deref_mut(&mut self) -> &mut FrameShared {
        &mut self.sh
    }
}

fn count_units_in_frame(unit_size: usize, frame_size: usize) -> usize {
    ((frame_size + (unit_size >> 1)) / unit_size).max(1)
}

impl FrameState {
    pub fn new(seq: &SequenceHeader, fh: &FrameHeader) -> FrameState {
        let c = &seq.color;
        let (ssx, ssy) = (c.subsampling_x as usize, c.subsampling_y as usize);
        let mi_cols = fh.mi_cols as usize;
        let mi_rows = fh.mi_rows as usize;
        let cur = FrameBuf::new(fh.frame_width as usize, fh.frame_height as usize, c.num_planes, ssx, ssy, c.bit_depth);
        let cdef_cols = mi_cols.div_ceil(16) + 2;
        let cdef_rows = mi_rows.div_ceil(16) + 2;
        let mut lr_unit_rows = [0; 3];
        let mut lr_unit_cols = [0; 3];
        let mut lr = LrUnits::default();
        for plane in 0..c.num_planes {
            if fh.lr.frame_restoration_type[plane] == RESTORE_NONE as u8 {
                continue;
            }
            let sub_x = if plane == 0 { 0 } else { ssx };
            let sub_y = if plane == 0 { 0 } else { ssy };
            let unit = fh.lr.loop_restoration_size[plane] as usize;
            lr_unit_rows[plane] = count_units_in_frame(unit, (fh.frame_height as usize + sub_y) >> sub_y);
            lr_unit_cols[plane] = count_units_in_frame(unit, (fh.upscaled_width as usize + sub_x) >> sub_x);
            let n = lr_unit_rows[plane] * lr_unit_cols[plane];
            lr.lr_type[plane] = vec![RESTORE_NONE as u8; n];
            lr.lr_wiener[plane] = vec![[[0; 3]; 2]; n];
            lr.lr_sgr_set[plane] = vec![0; n];
            lr.lr_sgr_xqd[plane] = vec![[0; 2]; n];
        }
        FrameState {
            sh: FrameShared {
                seq: seq.clone(),
                fh: fh.clone(),
                num_planes: c.num_planes,
                ssx,
                ssy,
                bit_depth: c.bit_depth as u32,
                lr_unit_rows,
                lr_unit_cols,
                cdef_cols,
                refs: Default::default(),
                ref_info: Default::default(),
                motion_field: Default::default(),
                prev_segment_ids: vec![0; mi_cols * mi_rows],
            },
            cur,
            mi: MiInfo::new(mi_cols, mi_rows, fh.allow_screen_content_tools),
            cdef: vec![-1; cdef_cols * cdef_rows],
            lf_tx_size: std::array::from_fn(|_| Grid8::new(0, 0, mi_cols + 32, mi_rows + 32)),
            lr,
        }
    }

    #[inline]
    pub fn cdef_idx(&self, r: usize, c: usize) -> i8 {
        self.cdef[(r >> 4) * self.cdef_cols + (c >> 4)]
    }

    #[inline]
    pub fn lf_tx_size(&self, plane: usize, row: usize, col: usize) -> usize {
        self.lf_tx_size[plane].get(row, col) as usize
    }

    /// Move the frame's buffers into a tile state covering the whole frame.
    pub fn take_tile_state(&mut self) -> TileState {
        TileState::with(
            std::mem::take(&mut self.cur),
            std::mem::take(&mut self.mi),
            std::mem::take(&mut self.cdef),
            std::mem::take(&mut self.lf_tx_size),
            std::mem::take(&mut self.lr),
            self.cdef_cols,
        )
    }

    /// Return the buffers taken by [`FrameState::take_tile_state`].
    pub fn put_tile_state(&mut self, t: TileState) {
        self.cur = t.cur;
        self.mi = t.mi;
        self.cdef = t.cdef;
        self.lf_tx_size = t.lf_tx_size;
        self.lr = t.lr;
    }

    /// Private buffers for the tile covering mode info rows [r0, r1) x columns [c0, c1); tiles
    /// on the right / bottom frame edge also cover the allocation margins.
    pub fn tile_state(&self, r0: usize, r1: usize, c0: usize, c1: usize) -> TileState {
        let (mi_cols, mi_rows) = (self.mi.cols, self.mi.rows);
        let x1 = if c1 >= mi_cols { usize::MAX } else { c1 * 4 };
        let y1 = if r1 >= mi_rows { usize::MAX } else { r1 * 4 };
        let cur = self.cur.region_like(c0 * 4, r0 * 4, x1, y1);
        let mi = MiInfo::region(mi_cols, mi_rows, c0, r0, c1 - c0, r1 - r0, !self.mi.palette_size[0].is_empty());
        let lf = std::array::from_fn(|p| {
            let (sx, sy) = if p == 0 { (0, 0) } else { (self.ssx, self.ssy) };
            let full = &self.lf_tx_size[p];
            let (gx0, gy0) = (c0 >> sx, r0 >> sy);
            let gx1 = if c1 >= mi_cols { full.stride } else { c1 >> sx };
            let gy1 = if r1 >= mi_rows { full.rows } else { r1 >> sy };
            Grid8::new(gx0, gy0, gx1 - gx0, gy1 - gy0)
        });
        TileState::with(cur, mi, vec![-1; self.cdef.len()], lf, self.lr.clone(), self.cdef_cols)
    }

    /// Copy a tile's private buffers into the frame.
    pub fn merge_tile_state(&mut self, t: &TileState) {
        for p in 0..self.num_planes {
            t.cur.planes[p].copy_into(&mut self.cur.planes[p]);
            t.lf_tx_size[p].copy_into(&mut self.lf_tx_size[p]);
            for &u in &t.lr_written[p] {
                self.lr.lr_type[p][u] = t.lr.lr_type[p][u];
                self.lr.lr_wiener[p][u] = t.lr.lr_wiener[p][u];
                self.lr.lr_sgr_set[p][u] = t.lr.lr_sgr_set[p][u];
                self.lr.lr_sgr_xqd[p][u] = t.lr.lr_sgr_xqd[p][u];
            }
        }
        t.mi.copy_into(&mut self.mi);
        for (d, &s) in self.cdef.iter_mut().zip(&t.cdef) {
            if s != -1 {
                *d = s;
            }
        }
    }
}

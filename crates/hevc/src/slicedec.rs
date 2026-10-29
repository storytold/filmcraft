//! Slice segment data decoding (7.3.8) with reconstruction: CTU / coding quadtree / coding unit /
//! prediction unit / transform tree parsing (CABAC, 9.3) interleaved with intra prediction, motion
//! compensation, scaling and inverse transforms. In-loop filters and row publication live in
//! `filter.rs`.

use crate::cabac::{Cabac, Contexts, init_contexts};
use crate::error::{Result, ensure, invalid};
use crate::inter;
use crate::intra::{self, Refs};
use crate::params::{Layout, Pps, Sps};
use crate::picture::{FrameRef, RefPic, Sample};
use crate::slice::{SliceHeader, SliceType};
use crate::spec_tables::{
    ABS_MVD_GT0, ABS_MVD_GT1, CBF_CHROMA, CBF_LUMA, CODED_SUB_BLOCK, CU_QP_DELTA_ABS, CU_SKIP, CU_TRANSQUANT_BYPASS, GT1, GT2, INTER_PRED_IDC, INTRA_CHROMA,
    LAST_X_PREFIX, LAST_Y_PREFIX, MERGE_FLAG, MERGE_IDX, MVP_FLAG, PART_MODE, PRED_MODE, PREV_INTRA_LUMA, REF_IDX, RQT_ROOT_CBF, SAO_MERGE, SAO_TYPE,
    SIG_COEFF, SPLIT_CU, SPLIT_TRANSFORM, TRANSFORM_SKIP,
};
use crate::tables::{CTX_IDX_MAP, LEVEL_SCALE, SCAN_4, qpc_420, scan_order};
use crate::transform;
use std::sync::Arc;

mod coding_unit;
mod prediction_unit;
mod transform_tree;

pub const F_INTRA: u8 = 1;
pub const F_SKIP: u8 = 2;
/// PCM with pcm_loop_filter_disabled_flag, or cu_transquant_bypass: in-loop filters leave the samples alone.
pub const F_NOFILTER: u8 = 4;
/// The luma transform block covering this 4x4 block has non-zero coefficients.
pub const F_CBF: u8 = 8;
/// Block has been decoded.
pub const F_CODED: u8 = 16;

pub const E_TU_V: u8 = 1;
pub const E_PU_V: u8 = 2;
pub const E_TU_H: u8 = 4;
pub const E_PU_H: u8 = 8;

/// Per 4x4 luma block state.
#[derive(Clone, Copy, Default)]
pub struct Blk {
    pub flags: u8,
    pub depth: u8,
    pub qp: i8,
    /// IntraPredModeY (DC for non-intra / PCM blocks, as used by the MPM derivation).
    pub ipm: u8,
    pub edges: u8,
}

/// Motion of a 4x4 block.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct MvField {
    pub mv: [[i16; 2]; 2],
    pub ref_idx: [i8; 2],
}

impl Default for MvField {
    fn default() -> Self {
        MvField { mv: [[0; 2]; 2], ref_idx: [-1, -1] }
    }
}

impl MvField {
    #[inline]
    pub fn pred(&self, l: usize) -> bool {
        self.ref_idx[l] >= 0
    }
}

/// SAO parameters of one CTB.
#[derive(Clone, Copy, Default, Debug)]
pub struct SaoParams {
    pub type_idx: [u8; 3],
    pub band_pos: [u8; 3],
    pub eo_class: [u8; 3],
    /// SaoOffsetVal[1..4].
    pub offsets: [[i16; 4]; 3],
}

/// Slice-level data needed after parsing (in-loop filters, collocated motion).
pub struct SliceInfo {
    pub addr_rs: u32,
    pub deblock_disabled: bool,
    pub beta_offset: i32,
    pub tc_offset: i32,
    pub lf_across: bool,
    pub sao_luma: bool,
    pub sao_chroma: bool,
    /// Frame ids, POCs and long-term flags of RefPicList0/1.
    pub ref_ids: [Vec<u32>; 2],
    pub ref_pocs: [Vec<i32>; 2],
    pub ref_lt: [Vec<bool>; 2],
}

/// One slice segment of a picture job.
pub struct SliceJob {
    pub sh: SliceHeader,
    pub pps: Arc<Pps>,
    pub sps: Arc<Sps>,
    pub rbsp: Vec<u8>,
    pub refs: [Vec<RefPic>; 2],
}

/// Per-picture decoding state.
pub struct PicState<S> {
    pub frame: FrameRef,
    #[allow(dead_code)]
    pub sps: Arc<Sps>,
    pub pps: Arc<Pps>,
    pub layout: Arc<Layout>,
    pub poc: i32,
    pub width: usize,
    pub height: usize,
    pub cwidth: usize,
    pub cheight: usize,
    pub w4: usize,
    pub h4: usize,
    pub log2_ctb: u32,
    pub wctb: usize,
    pub hctb: usize,
    pub bd_y: u32,
    pub bd_c: u32,
    /// Reconstructed (pre-SAO) planes.
    pub planes: [Vec<S>; 3],
    pub blk: Vec<Blk>,
    pub mvf: Vec<MvField>,
    /// Index into `slices` per CTB (u32::MAX = not decoded).
    pub ctb_slice: Vec<u32>,
    /// SliceAddrRs per CTB (u32::MAX = not decoded).
    pub ctb_addr: Vec<u32>,
    pub sao: Vec<SaoParams>,
    pub slices: Vec<SliceInfo>,
    pub row_done: Vec<u32>,
    pub rows_complete: usize,
    pub rows_deblocked: usize,
    pub rows_published: usize,
    pub wpp_ctx: Option<Box<Contexts>>,
    pub ds_ctx: Option<(Box<Contexts>, i32)>,
    /// Scaling factors [sizeId][matrixId] when scaling lists are enabled.
    pub scaling: Option<Arc<Vec<Vec<u8>>>>,
    pub deblocking_enabled_anywhere: bool,
    /// Draft mode: no deblocking or SAO.
    pub draft: bool,
}

impl<S: Sample> PicState<S> {
    pub fn new(frame: FrameRef, sps: Arc<Sps>, pps: Arc<Pps>, layout: Arc<Layout>, scaling: Option<Arc<Vec<Vec<u8>>>>) -> Self {
        let (width, height) = (sps.width as usize, sps.height as usize);
        let (cwidth, cheight) = (width / 2, height / 2);
        let (w4, h4) = (width.div_ceil(4), height.div_ceil(4));
        let log2_ctb = sps.log2_ctb;
        let (wctb, hctb) = (sps.pic_width_in_ctbs() as usize, sps.pic_height_in_ctbs() as usize);
        let (bd_y, bd_c) = (sps.bit_depth_luma, sps.bit_depth_chroma);
        let gy = S::from_u16(1u16 << (bd_y - 1));
        let gc = S::from_u16(1u16 << (bd_c - 1));
        PicState {
            poc: frame.poc,
            frame,
            sps,
            pps,
            layout,
            width,
            height,
            cwidth,
            cheight,
            w4,
            h4,
            log2_ctb,
            wctb,
            hctb,
            bd_y,
            bd_c,
            planes: [vec![gy; width * height], vec![gc; cwidth * cheight], vec![gc; cwidth * cheight]],
            blk: vec![Blk::default(); w4 * h4],
            mvf: vec![MvField::default(); w4 * h4],
            ctb_slice: vec![u32::MAX; wctb * hctb],
            ctb_addr: vec![u32::MAX; wctb * hctb],
            sao: vec![SaoParams::default(); wctb * hctb],
            slices: Vec::new(),
            row_done: vec![0; hctb],
            rows_complete: 0,
            rows_deblocked: 0,
            rows_published: 0,
            wpp_ctx: None,
            ds_ctx: None,
            scaling,
            deblocking_enabled_anywhere: false,
            draft: false,
        }
    }

    /// z-scan order availability (6.4.1) of luma location (xn, yn) for the block at (xc, yc) in the slice
    /// with SliceAddrRs `addr`.
    #[inline]
    pub fn zavail(&self, xc: i32, yc: i32, xn: i32, yn: i32, addr: u32) -> bool {
        if xn < 0 || yn < 0 || xn >= self.width as i32 || yn >= self.height as i32 {
            return false;
        }
        let s = self.log2_ctb;
        let ctb_n = (yn >> s) as usize * self.wctb + (xn >> s) as usize;
        let ctb_c = (yc >> s) as usize * self.wctb + (xc >> s) as usize;
        if ctb_n != ctb_c {
            let tsn = self.layout.rs_to_ts[ctb_n];
            let tsc = self.layout.rs_to_ts[ctb_c];
            tsn < tsc && self.ctb_addr[ctb_n] == addr && self.layout.tile_id[tsn as usize] == self.layout.tile_id[tsc as usize]
        } else {
            let m = (1 << s) - 1;
            morton(((xn & m) >> 2) as u32, ((yn & m) >> 2) as u32) <= morton(((xc & m) >> 2) as u32, ((yc & m) >> 2) as u32)
        }
    }

    #[inline]
    pub fn blk_at(&self, x: i32, y: i32) -> &Blk {
        &self.blk[(y as usize >> 2) * self.w4 + (x as usize >> 2)]
    }
}

#[inline]
fn morton(x: u32, y: u32) -> u32 {
    let mut r = 0;
    for i in 0..5 {
        r |= ((x >> i) & 1) << (2 * i);
        r |= ((y >> i) & 1) << (2 * i + 1);
    }
    r
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum PartMode {
    P2Nx2N,
    P2NxN,
    PNx2N,
    PNxN,
    P2NxnU,
    P2NxnD,
    PnLx2N,
    PnRx2N,
}

/// Decoder of one slice segment.
pub struct SliceDecoder<'a, S> {
    pub pic: &'a mut PicState<S>,
    pub sh: &'a SliceHeader,
    pub sps: &'a Sps,
    pub pps: &'a Pps,
    pub refs: &'a [Vec<RefPic>; 2],
    pub c: Cabac<'a>,
    pub slice_idx: u32,
    pub slice_addr: u32,
    pub slice_qp: i32,
    pub init_type: usize,
    // quantization state
    pub qp_y: i32,
    pub qp_y_pred: i32,
    pub last_qp: i32,
    pub is_cu_qp_delta_coded: bool,
    pub cu_qp_delta: i32,
    pub log2_min_cu_qp_delta: u32,
    pub qp_cb: i32,
    pub qp_cr: i32,
    // current CU
    pub cu_transquant_bypass: bool,
    pub cu_intra: bool,
    pub intra_chroma_mode: u32,
    pub max_trafo_depth: u32,
    pub intra_split: bool,
    pub cu_part: PartMode,
    // scratch
    pub coeffs: Vec<i32>,
    pub pred0: Vec<i16>,
    pub pred1: Vec<i16>,
    pub mc: crate::inter::McScratch,
    pub refs_buf: Refs,
    /// Collocated picture for TMVP.
    pub col: Option<RefPic>,
    /// NoBackwardPredFlag.
    pub no_backward_pred: bool,
    pub ctb_x: i32,
    pub ctb_y: i32,
}

impl<'a, S: Sample> SliceDecoder<'a, S> {
    #[inline(always)]
    fn dd(&mut self, ctx: usize) -> u32 {
        self.c.decode_decision(ctx)
    }
    #[inline(always)]
    fn bypass(&mut self) -> u32 {
        self.c.decode_bypass()
    }
    #[inline]
    fn bypass_bits(&mut self, n: u32) -> u32 {
        let mut v = 0u32;
        let mut left = n;
        while left > 16 {
            v = (v << 16) | self.c.decode_bypass_bits(16);
            left -= 16;
        }
        // (bits beyond 32 shift out, as the bin-by-bin loop did)
        v.checked_shl(left).unwrap_or(0) | self.c.decode_bypass_bits(left)
    }

    /// Decode the slice segment `job` into `pic`.
    pub fn decode(pic: &'a mut PicState<S>, job: &'a SliceJob) -> Result<()> {
        let sh = &job.sh;
        let (sps, pps) = (&*job.sps, &*job.pps);
        ensure!(sh.data_offset < job.rbsp.len(), "empty slice segment data");
        let slice_qp = pps.init_qp + sh.qp_delta;
        let init_type = match sh.slice_type {
            SliceType::I => 0,
            SliceType::P => {
                if sh.cabac_init_flag {
                    2
                } else {
                    1
                }
            }
            SliceType::B => {
                if sh.cabac_init_flag {
                    1
                } else {
                    2
                }
            }
        };
        // SliceAddrRs: address of the first CTB of the (independent) slice
        let slice_addr = if sh.dependent {
            match pic.slices.last() {
                Some(s) => s.addr_rs,
                None => return invalid("dependent slice segment without a preceding slice"),
            }
        } else {
            sh.segment_address
        };
        if !sh.dependent {
            let ids = |l: usize| job.refs[l].iter().map(|r| r.frame.id).collect::<Vec<_>>();
            let pocs = |l: usize| job.refs[l].iter().map(|r| r.poc).collect::<Vec<_>>();
            let lts = |l: usize| job.refs[l].iter().map(|r| r.long_term).collect::<Vec<_>>();
            pic.slices.push(SliceInfo {
                addr_rs: slice_addr,
                deblock_disabled: sh.deblocking_disabled,
                beta_offset: sh.beta_offset_div2 * 2,
                tc_offset: sh.tc_offset_div2 * 2,
                lf_across: sh.loop_filter_across_slices,
                sao_luma: sh.sao_luma,
                sao_chroma: sh.sao_chroma,
                ref_ids: [ids(0), ids(1)],
                ref_pocs: [pocs(0), pocs(1)],
                ref_lt: [lts(0), lts(1)],
            });
            if !sh.deblocking_disabled {
                pic.deblocking_enabled_anywhere = true;
            }
        }
        let slice_idx = (pic.slices.len() - 1) as u32;
        let col = if sh.temporal_mvp && !sh.is_intra() {
            let l = if sh.is_b() && !sh.collocated_from_l0 { 1 } else { 0 };
            job.refs[l].get(sh.collocated_ref_idx as usize).cloned()
        } else {
            None
        };
        let no_backward_pred = job.refs.iter().flatten().all(|r| r.poc <= pic.poc);
        let log2_min_cu_qp_delta = sps.log2_ctb - pps.diff_cu_qp_delta_depth;
        let c = Cabac::new(&job.rbsp, sh.data_offset, slice_qp, init_type)?;
        let mut d = SliceDecoder {
            pic,
            sh,
            sps,
            pps,
            refs: &job.refs,
            c,
            slice_idx,
            slice_addr,
            slice_qp,
            init_type,
            qp_y: slice_qp,
            qp_y_pred: slice_qp,
            last_qp: slice_qp,
            is_cu_qp_delta_coded: false,
            cu_qp_delta: 0,
            log2_min_cu_qp_delta,
            qp_cb: 0,
            qp_cr: 0,
            cu_transquant_bypass: false,
            cu_intra: false,
            intra_chroma_mode: 0,
            max_trafo_depth: 0,
            intra_split: false,
            cu_part: PartMode::P2Nx2N,
            coeffs: vec![0; 32 * 32],
            pred0: vec![0; 64 * 64],
            pred1: vec![0; 64 * 64],
            mc: Default::default(),
            refs_buf: Refs::default(),
            col,
            no_backward_pred,
            ctb_x: 0,
            ctb_y: 0,
        };
        d.decode_data()
    }

    fn decode_data(&mut self) -> Result<()> {
        let layout = self.pic.layout.clone();
        let wctb = self.pic.wctb as u32;
        let npic = (self.pic.wctb * self.pic.hctb) as u32;
        let mut ts = layout.rs_to_ts[self.sh.segment_address as usize];
        let mut first = true;
        loop {
            let rs = layout.ts_to_rs[ts as usize];
            ensure!(self.pic.ctb_slice[rs as usize] == u32::MAX, "CTB {rs} decoded twice");
            let (rx, ry) = ((rs % wctb) as i32, (rs / wctb) as i32);
            let ctb = 1i32 << self.sps.log2_ctb;
            self.ctb_x = rx * ctb;
            self.ctb_y = ry * ctb;
            self.pic.ctb_slice[rs as usize] = self.slice_idx;
            self.pic.ctb_addr[rs as usize] = self.slice_addr;
            // context initialisation / synchronisation (9.3.1, 9.3.2.1)
            let first_in_tile = ts == 0 || layout.tile_id[ts as usize] != layout.tile_id[ts as usize - 1];
            let wpp_row_start = self.pps.entropy_coding_sync
                && (rs.is_multiple_of(wctb) || layout.tile_id[ts as usize] != layout.tile_id[layout.rs_to_ts[rs as usize - 1] as usize]);
            if first_in_tile {
                if !first {
                    init_contexts(&mut self.c.ctx, self.slice_qp, self.init_type);
                }
                self.last_qp = self.slice_qp;
            } else if wpp_row_start {
                let (x0, y0) = (self.ctb_x, self.ctb_y);
                let avail = self.pic.zavail(x0, y0, x0 + ctb, y0 - ctb, self.slice_addr);
                match (&self.pic.wpp_ctx, avail) {
                    (Some(saved), true) => self.c.ctx = **saved,
                    _ => init_contexts(&mut self.c.ctx, self.slice_qp, self.init_type),
                }
                self.last_qp = self.slice_qp;
            } else if first && self.sh.dependent {
                match &self.pic.ds_ctx {
                    Some((saved, qp)) => {
                        self.c.ctx = **saved;
                        self.last_qp = *qp;
                    }
                    None => return invalid("dependent slice segment without stored contexts"),
                }
            }
            first = false;
            if self.sh.sao_luma || self.sh.sao_chroma {
                self.parse_sao(rx, ry, rs)?;
            }
            self.coding_quadtree(self.ctb_x, self.ctb_y, self.sps.log2_ctb, 0)?;
            let end = self.c.decode_terminate() == 1;
            ensure!(!self.c.overrun(), "slice data overrun");
            // WPP storage after the second CTB of a row in a tile (9.3.1)
            if self.pps.entropy_coding_sync
                && (rs % wctb == 1 || (rs > 1 && layout.tile_id[ts as usize] != layout.tile_id[layout.rs_to_ts[rs as usize - 2] as usize]))
            {
                self.pic.wpp_ctx = Some(Box::new(self.c.ctx));
            }
            crate::filter::ctb_decoded(self.pic, ry as usize);
            ts += 1;
            if end {
                if self.pps.dependent_slice_segments_enabled {
                    self.pic.ds_ctx = Some((Box::new(self.c.ctx), self.last_qp));
                }
                break;
            }
            ensure!(ts < npic, "slice data continues past the last CTB");
            let nrs = layout.ts_to_rs[ts as usize];
            let new_tile = self.pps.tiles_enabled && layout.tile_id[ts as usize] != layout.tile_id[ts as usize - 1];
            let new_row = self.pps.entropy_coding_sync
                && (nrs.is_multiple_of(wctb) || layout.tile_id[ts as usize] != layout.tile_id[layout.rs_to_ts[nrs as usize - 1] as usize]);
            if new_tile || new_row {
                ensure!(self.c.decode_terminate() == 1, "end_of_subset_one_bit is 0");
                let pos = self.c.bit_pos().div_ceil(8);
                self.c.set_byte_pos(pos);
                self.c.init_engine()?;
            }
        }
        Ok(())
    }

    fn parse_sao(&mut self, rx: i32, ry: i32, rs: u32) -> Result<()> {
        let layout = self.pic.layout.clone();
        let wctb = self.pic.wctb as u32;
        let ts = layout.rs_to_ts[rs as usize];
        let mut merge_left = false;
        let mut merge_up = false;
        if rx > 0 {
            let left_in_slice = self.pic.ctb_addr[rs as usize - 1] == self.slice_addr;
            let left_in_tile = layout.tile_id[ts as usize] == layout.tile_id[layout.rs_to_ts[rs as usize - 1] as usize];
            if left_in_slice && left_in_tile {
                merge_left = self.dd(SAO_MERGE) == 1;
            }
        }
        if ry > 0 && !merge_left {
            let up = rs - wctb;
            let up_in_slice = self.pic.ctb_addr[up as usize] == self.slice_addr;
            let up_in_tile = layout.tile_id[ts as usize] == layout.tile_id[layout.rs_to_ts[up as usize] as usize];
            if up_in_slice && up_in_tile {
                merge_up = self.dd(SAO_MERGE) == 1;
            }
        }
        if merge_left {
            self.pic.sao[rs as usize] = self.pic.sao[rs as usize - 1];
            return Ok(());
        }
        if merge_up {
            self.pic.sao[rs as usize] = self.pic.sao[(rs - wctb) as usize];
            return Ok(());
        }
        let mut p = SaoParams::default();
        for c in 0..3 {
            let enabled = if c == 0 { self.sh.sao_luma } else { self.sh.sao_chroma };
            if !enabled {
                continue;
            }
            if c == 2 {
                p.type_idx[2] = p.type_idx[1];
                p.eo_class[2] = p.eo_class[1];
            } else {
                p.type_idx[c] = if self.dd(SAO_TYPE) == 0 {
                    0
                } else if self.bypass() == 0 {
                    1
                } else {
                    2
                };
            }
            if p.type_idx[c] == 0 {
                continue;
            }
            let bd = if c == 0 { self.pic.bd_y } else { self.pic.bd_c };
            let cmax = (1u32 << (bd.min(10) - 5)) - 1;
            let mut abs = [0i16; 4];
            for a in abs.iter_mut() {
                let mut v = 0;
                while v < cmax && self.bypass() == 1 {
                    v += 1;
                }
                *a = v as i16;
            }
            if p.type_idx[c] == 1 {
                for a in abs.iter_mut() {
                    if *a != 0 && self.bypass() == 1 {
                        *a = -*a;
                    }
                }
                p.band_pos[c] = self.bypass_bits(5) as u8;
                p.offsets[c] = abs;
            } else {
                p.offsets[c] = [abs[0], abs[1], -abs[2], -abs[3]];
                if c == 0 {
                    p.eo_class[0] = self.bypass_bits(2) as u8;
                }
                if c == 1 {
                    p.eo_class[1] = self.bypass_bits(2) as u8;
                }
            }
        }
        self.pic.sao[rs as usize] = p;
        Ok(())
    }
}

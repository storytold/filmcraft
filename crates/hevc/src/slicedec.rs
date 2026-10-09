//! Slice segment data decoding (7.3.8) with reconstruction: CTU / coding quadtree / coding unit /
//! prediction unit / transform tree parsing (CABAC, 9.3) interleaved with intra prediction, motion
//! compensation, scaling and inverse transforms. In-loop filters and row publication live in
//! `filter.rs`.

use crate::cabac::{Cabac, Contexts, init_contexts};
use crate::error::{Result, ensure, invalid};
use crate::inter;
use crate::intra::{self, Refs};
use crate::params::{Layout, Pps, Sps};
use crate::picture::{FrameRef, RefPic};
use crate::slice::{SliceHeader, SliceType};
use crate::spec_tables::*;
use crate::tables::{CTX_IDX_MAP, LEVEL_SCALE, SCAN_4, qpc_420, scan_order};
use crate::transform;
use std::sync::Arc;

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
    /// The NAL unit as in the stream (kept only with an accelerator).
    pub nal: Vec<u8>,
    pub refs: [Vec<RefPic>; 2],
}

/// Per-picture decoding state.
pub struct PicState {
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
    pub planes: [Vec<u16>; 3],
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

impl PicState {
    pub fn new(frame: FrameRef, sps: Arc<Sps>, pps: Arc<Pps>, layout: Arc<Layout>, scaling: Option<Arc<Vec<Vec<u8>>>>) -> Self {
        let (width, height) = (sps.width as usize, sps.height as usize);
        let (cwidth, cheight) = (width / 2, height / 2);
        let (w4, h4) = (width.div_ceil(4), height.div_ceil(4));
        let log2_ctb = sps.log2_ctb;
        let (wctb, hctb) = (sps.pic_width_in_ctbs() as usize, sps.pic_height_in_ctbs() as usize);
        let (bd_y, bd_c) = (sps.bit_depth_luma, sps.bit_depth_chroma);
        let gy = 1u16 << (bd_y - 1);
        let gc = 1u16 << (bd_c - 1);
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
pub struct SliceDecoder<'a> {
    pub pic: &'a mut PicState,
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

impl<'a> SliceDecoder<'a> {
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
    pub fn decode(pic: &'a mut PicState, job: &'a SliceJob) -> Result<()> {
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

    fn coding_quadtree(&mut self, x0: i32, y0: i32, log2: u32, depth: u32) -> Result<()> {
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
    fn fill_blk(&mut self, x0: i32, y0: i32, w: i32, h: i32, f: impl Fn(&mut Blk)) {
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

    fn update_qp(&mut self) {
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
    fn mark_edges(&mut self, x0: i32, y0: i32, w: i32, h: i32, v: u8, hz: u8) {
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
                self.pic.planes[0][(y0 as usize + y) * w + x0 as usize + x] = v << sy;
            }
        }
        let cw = self.pic.cwidth;
        for c in 1..3 {
            for y in 0..n / 2 {
                for x in 0..n / 2 {
                    let v = r.read_bits(bc)? as u16;
                    self.pic.planes[c][(y0 as usize / 2 + y) * cw + x0 as usize / 2 + x] = v << sc;
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

    fn transform_tree(&mut self, x0: i32, y0: i32, xb: i32, yb: i32, log2: u32, depth: u32, blk_idx: u32, parent_cbf_c: [bool; 2]) -> Result<()> {
        let split = if log2 <= self.sps.log2_max_tb && log2 > self.sps.log2_min_tb && depth < self.max_trafo_depth && !(self.intra_split && depth == 0) {
            self.dd(SPLIT_TRANSFORM + (5 - log2 as usize)) == 1
        } else {
            // interSplitFlag (7.4.9.8)
            let inter_split = self.sps.max_th_depth_inter == 0 && !self.cu_intra && self.cu_part != PartMode::P2Nx2N && depth == 0;
            log2 > self.sps.log2_max_tb || (self.intra_split && depth == 0) || inter_split
        };
        let mut cbf_c = [false, false];
        if log2 > 2 {
            for (c, cbf) in cbf_c.iter_mut().enumerate() {
                if depth == 0 || parent_cbf_c[c] {
                    *cbf = self.dd(CBF_CHROMA + depth as usize) == 1;
                }
            }
        } else {
            // 4x4 luma blocks: chroma cbfs are those of the parent (coded at blkIdx 3)
            cbf_c = parent_cbf_c;
            if depth == 0 {
                cbf_c = [false, false];
            }
        }
        if split {
            let h = 1 << (log2 - 1);
            self.transform_tree(x0, y0, x0, y0, log2 - 1, depth + 1, 0, cbf_c)?;
            self.transform_tree(x0 + h, y0, x0, y0, log2 - 1, depth + 1, 1, cbf_c)?;
            self.transform_tree(x0, y0 + h, x0, y0, log2 - 1, depth + 1, 2, cbf_c)?;
            self.transform_tree(x0 + h, y0 + h, x0, y0, log2 - 1, depth + 1, 3, cbf_c)?;
            return Ok(());
        }
        let cbf_luma = if self.cu_intra || depth != 0 || cbf_c[0] || cbf_c[1] { self.dd(CBF_LUMA + if depth == 0 { 1 } else { 0 }) == 1 } else { true };
        let n = 1 << log2;
        if depth > 0 {
            self.mark_edges(x0, y0, n, n, E_TU_V, E_TU_H);
        }
        self.transform_unit(x0, y0, xb, yb, log2, blk_idx, cbf_luma, cbf_c)
    }

    fn transform_unit(&mut self, x0: i32, y0: i32, xb: i32, yb: i32, log2: u32, blk_idx: u32, cbf_luma: bool, cbf_c: [bool; 2]) -> Result<()> {
        let n = 1i32 << log2;
        let chroma_here = log2 > 2 || blk_idx == 3;
        let cbf_chroma = cbf_c[0] || cbf_c[1];
        if (cbf_luma || cbf_chroma) && self.pps.cu_qp_delta_enabled && !self.is_cu_qp_delta_coded {
            // cu_qp_delta_abs: prefix TU(5) with ctx 0,1,1,1,1, suffix EG0
            let mut v = 0;
            while v < 5 && self.dd(CU_QP_DELTA_ABS + if v == 0 { 0 } else { 1 }) == 1 {
                v += 1;
            }
            if v == 5 {
                let mut k = 0;
                while self.bypass() == 1 {
                    v += 1 << k;
                    k += 1;
                    ensure!(k < 32, "invalid cu_qp_delta_abs");
                }
                v += self.bypass_bits(k) as i32;
            }
            if v != 0 && self.bypass() == 1 {
                v = -v;
            }
            let off = 6 * (self.pic.bd_y as i32 - 8);
            ensure!(v >= -(26 + off / 2) && v <= 25 + off / 2, "CuQpDeltaVal out of range");
            self.is_cu_qp_delta_coded = true;
            self.cu_qp_delta = v;
            self.update_qp();
        }
        // luma
        if self.cu_intra {
            let mode = self.pic.blk_at(x0, y0).ipm as u32;
            self.intra_predict(x0, y0, log2, 0, mode);
        }
        if cbf_luma {
            let qp_bd = 6 * (self.pic.bd_y as i32 - 8);
            let qp = self.qp_y + qp_bd;
            self.residual(x0, y0, log2, 0, qp)?;
            self.fill_blk(x0, y0, n, n, |b| b.flags |= F_CBF);
        }
        if !chroma_here {
            return Ok(());
        }
        let (xc, yc, log2c) = if log2 > 2 { (x0 / 2, y0 / 2, log2 - 1) } else { (xb / 2, yb / 2, 2) };
        for c in 1..3 {
            if self.cu_intra {
                let mode = self.intra_chroma_mode;
                self.intra_predict(xc, yc, log2c, c, mode);
            }
            if cbf_c[c - 1] {
                let qp = if c == 1 { self.qp_cb } else { self.qp_cr };
                self.residual(xc, yc, log2c, c, qp)?;
            }
        }
        Ok(())
    }

    /// Intra prediction of an n x n block of component `c` at component position (x0, y0).
    fn intra_predict(&mut self, x0: i32, y0: i32, log2: u32, c: usize, mode: u32) {
        let n = 1usize << log2;
        let sc = if c == 0 { 1 } else { 2 };
        let (xl, yl) = (x0 * sc, y0 * sc);
        let unit = 4 / sc as usize; // component samples per 4x4 luma block
        let stride = if c == 0 { self.pic.width } else { self.pic.cwidth };
        let constrained = self.pps.constrained_intra_pred;
        let mut al = [false; 129];
        let mut at = [false; 129];
        let addr = self.slice_addr;
        let avail =
            |pic: &PicState, xn: i32, yn: i32| -> bool { pic.zavail(xl, yl, xn, yn, addr) && (!constrained || pic.blk_at(xn, yn).flags & F_INTRA != 0) };
        let plane = &self.pic.planes[c];
        let r = &mut self.refs_buf;
        // corner
        if avail(self.pic, xl - 1, yl - 1) {
            al[0] = true;
            at[0] = true;
            let v = plane[(y0 - 1) as usize * stride + (x0 - 1) as usize];
            r.left[0] = v;
            r.top[0] = v;
        }
        let mut k = 0;
        while k < 2 * n {
            let yn = y0 + k as i32;
            if avail(self.pic, xl - 1, yn * sc) {
                for i in 0..unit {
                    al[1 + k + i] = true;
                    r.left[1 + k + i] = plane[(yn as usize + i) * stride + (x0 - 1) as usize];
                }
            }
            let xn = x0 + k as i32;
            if avail(self.pic, xn * sc, yl - 1) {
                let o = (y0 - 1) as usize * stride + xn as usize;
                for i in 0..unit {
                    at[1 + k + i] = true;
                    r.top[1 + k + i] = plane[o + i];
                }
            }
            k += unit;
        }
        let bd = if c == 0 { self.pic.bd_y } else { self.pic.bd_c };
        intra::substitute(r, &al, &at, n, bd);
        if c == 0 {
            intra::filter(r, mode, n, self.sps.strong_intra_smoothing, true, bd);
        }
        let off = y0 as usize * stride + x0 as usize;
        intra::predict(r, mode, n, c == 0, bd, &mut self.pic.planes[c][off..], stride);
    }

    /// residual_coding() + scaling + transform + reconstruction for one transform block.
    fn residual(&mut self, x0: i32, y0: i32, log2: u32, c: usize, qp: i32) -> Result<()> {
        let n = 1usize << log2;
        let bypass = self.cu_transquant_bypass;
        let ts = if self.pps.transform_skip && !bypass && log2 == 2 { self.dd(TRANSFORM_SKIP + (c > 0) as usize) == 1 } else { false };
        // last significant coefficient
        let cmax = (log2 << 1) - 1;
        let (off, shift) = if c == 0 { (3 * (log2 - 2) + ((log2 - 1) >> 2), (log2 + 1) >> 2) } else { (15, log2 - 2) };
        let mut px = 0;
        while px < cmax && self.dd(LAST_X_PREFIX + (off + (px >> shift)) as usize) == 1 {
            px += 1;
        }
        let mut py = 0;
        while py < cmax && self.dd(LAST_Y_PREFIX + (off + (py >> shift)) as usize) == 1 {
            py += 1;
        }
        let mut lx = px;
        if px > 3 {
            let nb = (px >> 1) - 1;
            lx = (1 << nb) * (2 + (px & 1)) + self.bypass_bits(nb);
        }
        let mut ly = py;
        if py > 3 {
            let nb = (py >> 1) - 1;
            ly = (1 << nb) * (2 + (py & 1)) + self.bypass_bits(nb);
        }
        // scanIdx (7.4.9.11)
        let scan_idx = if self.cu_intra && (log2 == 2 || (log2 == 3 && c == 0)) {
            let m = if c == 0 { self.pic.blk_at(x0, y0).ipm as u32 } else { self.intra_chroma_mode };
            if (6..=14).contains(&m) {
                2
            } else if (22..=30).contains(&m) {
                1
            } else {
                0
            }
        } else {
            0
        };
        if scan_idx == 2 {
            std::mem::swap(&mut lx, &mut ly);
        }
        ensure!((lx as usize) < n && (ly as usize) < n, "last significant coefficient outside the block");
        let log2sb = log2 - 2;
        let sb_scan = scan_order(log2sb, scan_idx);
        let pos_scan = &SCAN_4[scan_idx];
        let sb_w = 1usize << log2sb;
        let last_sb = sb_scan.iter().position(|&(x, y)| x as u32 == lx >> 2 && y as u32 == ly >> 2).unwrap_or(0);
        let last_pos = pos_scan.iter().position(|&(x, y)| x as u32 == lx & 3 && y as u32 == ly & 3).unwrap_or(0);
        self.coeffs[..n * n].fill(0);
        let mut csbf = [0u8; 64];
        let mut prev_g1: Option<u32> = None;
        let sdh = self.pps.sign_data_hiding && !bypass;
        let (mut max_x, mut max_y) = (0usize, 0usize);
        let c_off_sig = if c == 0 { 0 } else { 27 };
        for i in (0..=last_sb).rev() {
            let (xs, ys) = (sb_scan[i].0 as usize, sb_scan[i].1 as usize);
            let mut infer_dc = false;
            let right = if xs + 1 < sb_w { csbf[ys * 8 + xs + 1] } else { 0 };
            let below = if ys + 1 < sb_w { csbf[(ys + 1) * 8 + xs] } else { 0 };
            if i < last_sb && i > 0 {
                let inc = (right | below).min(1) as usize + if c > 0 { 2 } else { 0 };
                csbf[ys * 8 + xs] = self.c.decode_decision(CODED_SUB_BLOCK + inc) as u8;
                infer_dc = true;
            } else {
                csbf[ys * 8 + xs] = 1;
            }
            // significance map
            let mut sig = [0u8; 16]; // scan positions n of significant coefficients, descending
            let mut nsig = 0;
            let start: i32 = if i == last_sb {
                sig[0] = last_pos as u8;
                nsig = 1;
                last_pos as i32 - 1
            } else {
                15
            };
            if csbf[ys * 8 + xs] != 0 {
                let prev_csbf = right + (below << 1);
                let mut np = start;
                while np >= 0 {
                    let (xp, yp) = (pos_scan[np as usize].0 as usize, pos_scan[np as usize].1 as usize);
                    let (xc, yc) = (xs * 4 + xp, ys * 4 + yp);
                    if np > 0 || !infer_dc {
                        let sig_ctx = if log2 == 2 {
                            CTX_IDX_MAP[(yc << 2) + xc] as usize
                        } else if xc + yc == 0 {
                            0
                        } else {
                            let mut s = match prev_csbf {
                                0 => {
                                    if xp + yp == 0 {
                                        2
                                    } else if xp + yp < 3 {
                                        1
                                    } else {
                                        0
                                    }
                                }
                                1 => {
                                    if yp == 0 {
                                        2
                                    } else if yp == 1 {
                                        1
                                    } else {
                                        0
                                    }
                                }
                                2 => {
                                    if xp == 0 {
                                        2
                                    } else if xp == 1 {
                                        1
                                    } else {
                                        0
                                    }
                                }
                                _ => 2,
                            };
                            if c == 0 {
                                if xs + ys > 0 {
                                    s += 3;
                                }
                                s += if log2 == 3 { if scan_idx == 0 { 9 } else { 15 } } else { 21 };
                            } else {
                                s += if log2 == 3 { 9 } else { 12 };
                            }
                            s
                        };
                        if self.c.decode_decision(SIG_COEFF + c_off_sig + sig_ctx) == 1 {
                            sig[nsig] = np as u8;
                            nsig += 1;
                            infer_dc = false;
                        }
                    } else {
                        // np == 0 with inferSbDcSigCoeffFlag
                        sig[nsig] = 0;
                        nsig += 1;
                    }
                    np -= 1;
                }
            }
            if nsig == 0 {
                continue;
            }
            // greater1 / greater2 flags
            let mut ctx_set = if i == 0 || c > 0 { 0 } else { 2 };
            if let Some(g) = prev_g1
                && g == 0
            {
                ctx_set += 1;
            }
            let mut g1ctx = 1u32;
            let mut gt1 = [0u8; 16];
            let mut first_g1: Option<usize> = None;
            let c_off_g1 = if c > 0 { 16 } else { 0 };
            for k in 0..nsig.min(8) {
                let inc = ctx_set * 4 + g1ctx.min(3) as usize + c_off_g1;
                let f = self.c.decode_decision(GT1 + inc);
                gt1[k] = f as u8;
                if f == 1 {
                    g1ctx = 0;
                    if first_g1.is_none() {
                        first_g1 = Some(k);
                    }
                } else if g1ctx > 0 {
                    g1ctx += 1;
                }
            }
            prev_g1 = Some(g1ctx);
            let mut gt2 = 0u8;
            if let Some(k) = first_g1 {
                gt2 = self.c.decode_decision(GT2 + ctx_set + if c > 0 { 4 } else { 0 }) as u8;
                let _ = k;
            }
            // signs
            let last_sig_pos = sig[0] as i32;
            let first_sig_pos = sig[nsig - 1] as i32;
            let sign_hidden = sdh && last_sig_pos - first_sig_pos > 3;
            let nsigns = if sign_hidden { nsig - 1 } else { nsig };
            let mut signs = if nsigns > 0 { self.bypass_bits(nsigns as u32) << (32 - nsigns) } else { 0 };
            // remaining levels
            let mut rice = 0u32;
            let mut sum_abs = 0i32;
            for k in 0..nsig {
                let mut base = 1 + gt1[k] as i32;
                if Some(k) == first_g1 {
                    base += gt2 as i32;
                }
                let thresh = if k < 8 { if Some(k) == first_g1 { 3 } else { 2 } } else { 1 };
                let mut level = base;
                if base == thresh {
                    let rem = self.coeff_abs_level_remaining(rice)?;
                    level = base + rem;
                    if level > 3 * (1 << rice) {
                        rice = (rice + 1).min(4);
                    }
                }
                let np = sig[k] as usize;
                let (xc, yc) = (xs * 4 + pos_scan[np].0 as usize, ys * 4 + pos_scan[np].1 as usize);
                let mut v = level;
                if !(sign_hidden && k == nsig - 1) {
                    if signs & 0x8000_0000 != 0 {
                        v = -v;
                    }
                    signs <<= 1;
                }
                if sign_hidden {
                    sum_abs += level;
                    if k == nsig - 1 && sum_abs & 1 == 1 {
                        v = -v;
                    }
                }
                self.coeffs[yc * n + xc] = v;
                max_x = max_x.max(xc);
                max_y = max_y.max(yc);
            }
        }
        self.reconstruct(x0, y0, log2, c, qp, ts, max_x, max_y)
    }

    fn coeff_abs_level_remaining(&mut self, rice: u32) -> Result<i32> {
        let mut prefix = 0u32;
        while prefix < 32 && self.bypass() == 1 {
            prefix += 1;
        }
        ensure!(prefix < 32, "invalid coeff_abs_level_remaining");
        if prefix <= 3 {
            Ok(((prefix << rice) + self.bypass_bits(rice)) as i32)
        } else {
            let nb = prefix - 3 + rice;
            ensure!(nb <= 24, "coeff_abs_level_remaining too large");
            Ok(((((1u32 << (prefix - 3)) + 2) << rice) + self.bypass_bits(nb)) as i32)
        }
    }

    /// Scaling, transform and addition to the prediction.
    fn reconstruct(&mut self, x0: i32, y0: i32, log2: u32, c: usize, qp: i32, ts: bool, max_x: usize, max_y: usize) -> Result<()> {
        let n = 1usize << log2;
        let bd = if c == 0 { self.pic.bd_y } else { self.pic.bd_c };
        let coeffs = &mut self.coeffs[..n * n];
        let mut dc_only = false;
        if !self.cu_transquant_bypass {
            // scaling (8.6.3)
            let bd_shift = bd + log2 - 5;
            let round = 1i64 << (bd_shift - 1);
            let scale = (LEVEL_SCALE[(qp % 6) as usize] as i64) << (qp / 6);
            let m: Option<&[u8]> = match &self.pic.scaling {
                Some(s) if !(ts && n > 4) => {
                    let size_id = (log2 - 2) as usize;
                    let matrix_id = if self.cu_intra { c } else { 3 + c };
                    Some(&s[size_id * 6 + matrix_id])
                }
                _ => None,
            };
            for y in 0..=max_y {
                for x in 0..=max_x {
                    let v = coeffs[y * n + x];
                    if v != 0 {
                        let f = m.map_or(16, |m| m[y * n + x] as i64);
                        coeffs[y * n + x] = ((v as i64 * f * scale + round) >> bd_shift).clamp(-32768, 32767) as i32;
                    }
                }
            }
            let bd_shift = 20 - bd;
            if ts {
                transform::transform_skip(coeffs, n, bd_shift);
            } else if max_x == 0 && max_y == 0 && !(self.cu_intra && c == 0 && n == 4) {
                let v = transform::inverse_dc(coeffs[0], bd_shift);
                coeffs.fill(v);
                dc_only = true;
            } else {
                let dst = self.cu_intra && c == 0 && n == 4;
                transform::inverse_transform(coeffs, n, dst, max_x, max_y, bd_shift);
            }
        }
        let _ = dc_only;
        let (stride, max) = (if c == 0 { self.pic.width } else { self.pic.cwidth }, (1i32 << bd) - 1);
        let plane = &mut self.pic.planes[c];
        for y in 0..n {
            let row = &mut plane[(y0 as usize + y) * stride + x0 as usize..][..n];
            for (x, s) in row.iter_mut().enumerate() {
                *s = (*s as i32 + coeffs[y * n + x]).clamp(0, max) as u16;
            }
        }
        Ok(())
    }

    /// prediction_unit(): parse, derive motion, store it and predict. Returns merge_flag.
    fn prediction_unit(&mut self, xc: i32, yc: i32, ncb: i32, xp: i32, yp: i32, w: i32, h: i32, part_idx: usize, part: PartMode, skip: bool) -> Result<bool> {
        let merge = if skip { true } else { self.dd(MERGE_FLAG) == 1 };
        let mvf = if merge {
            let idx = if self.sh.max_num_merge_cand > 1 {
                let mut i = 0;
                if self.dd(MERGE_IDX) == 1 {
                    i = 1;
                    while i < self.sh.max_num_merge_cand - 1 && self.bypass() == 1 {
                        i += 1;
                    }
                }
                i
            } else {
                0
            };
            self.derive_merge(xc, yc, ncb, xp, yp, w, h, part_idx, part, idx as usize)
        } else {
            let pred_idc = if self.sh.is_b() {
                if w + h != 12 {
                    let depth = self.pic.blk_at(xc, yc).depth as usize;
                    if self.dd(INTER_PRED_IDC + depth) == 1 { 2 } else { self.dd(INTER_PRED_IDC + 4) }
                } else {
                    self.dd(INTER_PRED_IDC + 4)
                }
            } else {
                0
            };
            let mut ref_idx = [-1i8; 2];
            let mut mvd = [[0i32; 2]; 2];
            let mut mvp_flag = [0u32; 2];
            for l in 0..2 {
                let used = if l == 0 { pred_idc != 1 } else { pred_idc != 0 };
                if !used {
                    continue;
                }
                let nref = self.sh.num_ref_idx[l];
                let mut ri = 0;
                if nref > 1 {
                    while ri < nref - 1 {
                        let bin = if ri < 2 { self.dd(REF_IDX + ri as usize) } else { self.bypass() };
                        if bin == 0 {
                            break;
                        }
                        ri += 1;
                    }
                }
                ref_idx[l] = ri as i8;
                if l == 1 && self.sh.mvd_l1_zero && pred_idc == 2 {
                    mvd[1] = [0, 0];
                } else {
                    mvd[l] = self.mvd_coding()?;
                }
                mvp_flag[l] = self.dd(MVP_FLAG);
            }
            let mut f = MvField { mv: [[0; 2]; 2], ref_idx };
            for l in 0..2 {
                if ref_idx[l] < 0 {
                    continue;
                }
                let mvp = self.derive_amvp(xc, yc, ncb, xp, yp, w, h, part_idx, l, ref_idx[l] as usize, mvp_flag[l] as usize);
                // mvLX = (mvp + mvd) wrapped to 16 bits (8-94)
                f.mv[l] = [(mvp[0] as i32 + mvd[l][0]) as i16, (mvp[1] as i32 + mvd[l][1]) as i16];
            }
            f
        };
        for l in 0..2 {
            if mvf.ref_idx[l] >= 0 {
                ensure!((mvf.ref_idx[l] as usize) < self.refs[l].len(), "reference index out of range");
            }
        }
        // store motion
        let w4 = self.pic.w4;
        for by in (yp >> 2)..((yp + h) >> 2) {
            let o = by as usize * w4;
            self.pic.mvf[o + (xp >> 2) as usize..o + ((xp + w) >> 2) as usize].fill(mvf);
        }
        self.motion_compensate(xp, yp, w as usize, h as usize, &mvf);
        Ok(merge)
    }

    fn mvd_coding(&mut self) -> Result<[i32; 2]> {
        let g0 = [self.dd(ABS_MVD_GT0), self.dd(ABS_MVD_GT0)];
        let mut g1 = [0, 0];
        for k in 0..2 {
            if g0[k] == 1 {
                g1[k] = self.dd(ABS_MVD_GT1);
            }
        }
        let mut out = [0i32; 2];
        for k in 0..2 {
            if g0[k] == 1 {
                let mut v = 1i32;
                if g1[k] == 1 {
                    // EG1
                    let mut kk = 1;
                    let mut val = 0i32;
                    while self.bypass() == 1 {
                        val += 1 << kk;
                        kk += 1;
                        ensure!(kk < 31, "invalid abs_mvd_minus2");
                    }
                    val += self.bypass_bits(kk) as i32;
                    v = val + 2;
                }
                if self.bypass() == 1 {
                    v = -v;
                }
                out[k] = v;
            }
        }
        Ok(out)
    }

    /// Motion compensation + weighted prediction of one PU into the picture.
    fn motion_compensate(&mut self, xp: i32, yp: i32, w: usize, h: usize, f: &MvField) {
        let weighted = match self.sh.slice_type {
            SliceType::P => self.pps.weighted_pred,
            SliceType::B => self.pps.weighted_bipred,
            SliceType::I => false,
        };
        let bi = f.pred(0) && f.pred(1);
        for c in 0..3 {
            let (x, y, bw, bh, bd) = if c == 0 { (xp, yp, w, h, self.pic.bd_y) } else { (xp / 2, yp / 2, w / 2, h / 2, self.pic.bd_c) };
            let mut lists = [false; 2];
            for l in 0..2 {
                if !f.pred(l) {
                    continue;
                }
                lists[l] = true;
                let frame = &self.refs[l][f.ref_idx[l] as usize].frame;
                let buf = if l == 0 || !bi { &mut self.pred0 } else { &mut self.pred1 };
                if c == 0 {
                    inter::mc_luma(frame, x, y, f.mv[l], bw, bh, buf, &mut self.mc);
                } else {
                    inter::mc_chroma(frame, c - 1, x, y, f.mv[l], bw, bh, buf, &mut self.mc);
                }
            }
            let stride = if c == 0 { self.pic.width } else { self.pic.cwidth };
            let dst = &mut self.pic.planes[c][y as usize * stride + x as usize..];
            let Some(pwt) = self.sh.pwt.as_ref().filter(|_| weighted) else {
                if bi {
                    inter::put_bi(&self.pred0, &self.pred1, bw, bh, bd, dst, stride);
                } else {
                    inter::put_uni(&self.pred0, bw, bh, bd, dst, stride);
                }
                continue;
            };
            let shift1 = 14 - bd;
            let log2wd = if c == 0 { pwt.luma_log2_denom } else { pwt.chroma_log2_denom } + shift1;
            let get = |l: usize| -> (i32, i32) {
                let e = &pwt.l[l][f.ref_idx[l] as usize];
                let (wt, o) = if c == 0 { e.luma } else { e.chroma[c - 1] };
                (wt, o << (bd - 8))
            };
            if bi {
                let (w0, o0) = get(0);
                let (w1, o1) = get(1);
                inter::put_weighted_bi(&self.pred0, &self.pred1, bw, bh, bd, log2wd, w0, w1, o0, o1, dst, stride);
            } else {
                let l = if lists[0] { 0 } else { 1 };
                let (wt, o) = get(l);
                inter::put_weighted_uni(&self.pred0, bw, bh, bd, log2wd, wt, o, dst, stride);
            }
        }
    }
}

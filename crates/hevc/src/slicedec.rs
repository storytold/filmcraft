//! Slice segment data decoding (7.3.8) with reconstruction: CTU / coding quadtree / coding unit /
//! prediction unit / transform tree parsing (CABAC, 9.3) interleaved with intra prediction, motion
//! compensation, scaling and inverse transforms. In-loop filters and row publication live in
//! `filter.rs`.

use crate::cabac::{Cabac, CabacState, Contexts, init_contexts};
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
#[derive(Clone)]
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
    /// Emulation prevention bytes removed from the NAL unit payload: for each, the number of
    /// `rbsp` bytes before it (to locate the entry points, which count those bytes).
    #[cfg_attr(not(feature = "threads"), allow(dead_code))]
    pub epb: Vec<u32>,
    pub refs: [Vec<RefPic>; 2],
}

#[cfg_attr(not(feature = "threads"), allow(dead_code))] // used by `parallel` (thread pool only)
impl SliceJob {
    /// `rbsp` positions of the substreams of the slice segment data (the first at `data_offset`,
    /// then one per `entry_point_offset_minus1`, 7.4.7.1). None when one lies outside the data.
    pub fn substream_starts(&self) -> Option<Vec<usize>> {
        let d = self.sh.data_offset;
        // escaped (NAL payload) position of the slice data and of every substream
        let mut e = d.checked_add(self.epb.iter().take_while(|&&p| (p as usize) < d).count())?;
        let mut out = Vec::with_capacity(self.sh.entry_points.len() + 1);
        out.push(d);
        // removed bytes before escaped position e: the j-th lies at escaped position epb[j] + j
        let mut j = 0;
        for &off in &self.sh.entry_points {
            e = e.checked_add(off as usize)?;
            while self.epb.get(j).is_some_and(|&p| (p as usize).saturating_add(j) < e) {
                j += 1;
            }
            let pos = e - j;
            if pos >= self.rbsp.len() {
                return None;
            }
            out.push(pos);
        }
        Some(out)
    }
}

/// Index of a slice segment's slice in [`PicState::slices`] and its SliceAddrRs.
#[derive(Clone, Copy, Debug)]
pub struct SliceMeta {
    pub idx: u32,
    pub addr: u32,
}

/// Register the slices of a picture in `pic.slices` (one [`SliceInfo`] per independent slice
/// segment, in decoding order) and return each segment's slice (None: a dependent segment
/// without a preceding slice).
pub fn prepare_slices<S: Sample>(pic: &mut PicState<S>, jobs: &[SliceJob]) -> Vec<Option<SliceMeta>> {
    let mut out = Vec::with_capacity(jobs.len());
    for job in jobs {
        let sh = &job.sh;
        if !sh.dependent {
            let ids = |l: usize| job.refs[l].iter().map(|r| r.frame.id).collect::<Vec<_>>();
            let pocs = |l: usize| job.refs[l].iter().map(|r| r.poc).collect::<Vec<_>>();
            let lts = |l: usize| job.refs[l].iter().map(|r| r.long_term).collect::<Vec<_>>();
            pic.slices.push(SliceInfo {
                addr_rs: sh.segment_address,
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
        out.push(pic.slices.len().checked_sub(1).and_then(|i| Some(SliceMeta { idx: i as u32, addr: pic.slices.get(i)?.addr_rs })));
    }
    out
}

/// Called around each CTU decoded by a unit (wavefront row synchronisation).
pub trait CtuHooks<S> {
    /// Before CTU (rx, ry): copy in what it reads of the row above; false when that is not
    /// decoded yet (the decoder then stops and returns where to resume).
    fn before(&self, pic: &mut PicState<S>, rx: usize, ry: usize) -> bool;
    /// After CTU (rx, ry); `stored_wpp`: its contexts were stored for the next row (9.3.2.3).
    fn after(&self, pic: &mut PicState<S>, rx: usize, ry: usize, stored_wpp: bool);
}

/// Where a suspended substream continues: CTB `ts`, the CABAC engine and contexts, qPY_PREV.
#[cfg_attr(not(feature = "threads"), allow(dead_code))] // used by `parallel` (thread pool only)
#[derive(Clone)]
pub struct Resume {
    pub ts: u32,
    cabac: CabacState,
    last_qp: i32,
    /// No CTB of the substream decoded yet.
    first: bool,
}

/// Where a [`SliceDecoder::decode_unit`] starts.
#[cfg_attr(not(feature = "threads"), allow(dead_code))] // used by `parallel` (thread pool only)
#[allow(clippy::large_enum_variant)] // short-lived
pub enum Start {
    /// A substream at `rbsp` byte `pos` with CTB `ts`.
    At { pos: usize, ts: u32 },
    /// A suspended substream.
    Resume(Resume),
}

/// What a [`SliceDecoder`] decodes.
#[cfg_attr(not(feature = "threads"), allow(dead_code))] // used by `parallel` (thread pool only)
#[derive(Clone, Copy)]
pub enum Mode<'h, S> {
    /// The rest of the slice segment into the whole picture, filtering rows as they complete.
    Picture,
    /// One substream of a unit (a WPP row or a tile) into a window, up to CTB `end_ts` (tile
    /// scan, exclusive) or the end of the slice segment.
    Unit { end_ts: u32, hooks: Option<&'h dyn CtuHooks<S>> },
}

/// The large buffers of a [`PicState`], kept for re-use.
pub struct WinBufs<S> {
    planes: [Vec<S>; 3],
    blk: Vec<Blk>,
    mvf: Vec<MvField>,
}

/// Per-picture decoding state.
pub struct PicState<S> {
    pub frame: FrameRef,
    #[cfg_attr(not(feature = "threads"), allow(dead_code))]
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
    /// First luma line held by `planes`, `blk` and `mvf` (a multiple of 4): 0 for a whole picture,
    /// 4 lines above the first CTB row of a window decoded on a worker thread (see
    /// [`PicState::window`]). Chroma planes start at line `y_base / 2`.
    pub y_base: usize,
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
    /// Which deblocking the row filters do (all of it unless units deblock themselves).
    pub stage_deblock: crate::filter::StageDeblock,
}

impl<S: Sample> PicState<S> {
    pub fn new(frame: FrameRef, sps: Arc<Sps>, pps: Arc<Pps>, layout: Arc<Layout>, scaling: Option<Arc<Vec<Vec<u8>>>>) -> Self {
        let hctb = sps.pic_height_in_ctbs() as usize;
        Self::window(frame, sps, pps, layout, scaling, 0, hctb)
    }

    /// State holding only CTB rows `r0..r1` of the picture (plus the bottom 4 luma lines of row
    /// `r0 - 1`, which intra prediction, motion vector prediction and context selection read): the
    /// private state of a unit (a WPP row or a tile) decoded on a worker thread. Coordinates stay
    /// picture coordinates; per-CTB arrays cover the whole picture. `r0 = 0, r1 = hctb` is the
    /// whole picture.
    pub fn window(frame: FrameRef, sps: Arc<Sps>, pps: Arc<Pps>, layout: Arc<Layout>, scaling: Option<Arc<Vec<Vec<u8>>>>, r0: usize, r1: usize) -> Self {
        let (width, height) = (sps.width as usize, sps.height as usize);
        let (cwidth, cheight) = (width / 2, height / 2);
        let (w4, h4) = (width.div_ceil(4), height.div_ceil(4));
        let log2_ctb = sps.log2_ctb;
        let (wctb, hctb) = (sps.pic_width_in_ctbs() as usize, sps.pic_height_in_ctbs() as usize);
        let (bd_y, bd_c) = (sps.bit_depth_luma, sps.bit_depth_chroma);
        let mut pic = PicState {
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
            y_base: 0,
            planes: [Vec::new(), Vec::new(), Vec::new()],
            blk: Vec::new(),
            mvf: Vec::new(),
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
            stage_deblock: crate::filter::StageDeblock::Full,
        };
        pic.reset_window(r0, r1, true);
        pic
    }

    /// Set this state up for CTB rows `r0..r1` (see [`PicState::window`]): no CTB of rows
    /// `r0 - 1 .. r1` decoded, no stored contexts. `fresh`: grey samples and no decoded blocks;
    /// otherwise a re-used window keeps what it held (only reads of decoded CTBs follow: every
    /// neighbour read is gated by [`PicState::zavail`] or the CTB slice index, and only decoded
    /// CTBs leave a window).
    pub fn reset_window(&mut self, r0: usize, r1: usize, fresh: bool) {
        let ctb = 1usize << self.log2_ctb;
        let y_end = r1.saturating_mul(ctb).min(self.height);
        self.y_base = if r0 > 0 { (r0 * ctb).saturating_sub(4).min(y_end) } else { 0 };
        let lines = y_end - self.y_base;
        let gy = S::from_u16(1u16 << (self.bd_y - 1));
        let gc = S::from_u16(1u16 << (self.bd_c - 1));
        let fill = |v: &mut Vec<S>, n: usize, g: S| {
            if fresh {
                v.clear();
            }
            v.resize(n, g);
        };
        fill(&mut self.planes[0], self.width * lines, gy);
        fill(&mut self.planes[1], self.cwidth * (lines / 2), gc);
        fill(&mut self.planes[2], self.cwidth * (lines / 2), gc);
        let rows4 = y_end.div_ceil(4) - self.y_base / 4;
        if fresh {
            self.blk.clear();
            self.mvf.clear();
        }
        self.blk.resize(self.w4 * rows4, Blk::default());
        self.mvf.resize(self.w4 * rows4, MvField::default());
        let rows = r0.saturating_sub(1).min(self.hctb)..r1.min(self.hctb);
        let ctbs = rows.start * self.wctb..rows.end * self.wctb;
        for v in [&mut self.ctb_slice, &mut self.ctb_addr] {
            if let Some(s) = v.get_mut(ctbs.clone()) {
                s.fill(u32::MAX);
            }
        }
        self.wpp_ctx = None;
        self.ds_ctx = None;
    }

    /// Move the sample planes and block arrays out (leaving them empty), to re-use them.
    #[cfg_attr(not(feature = "threads"), allow(dead_code))]
    pub fn take_bufs(&mut self) -> WinBufs<S> {
        WinBufs { planes: std::mem::take(&mut self.planes), blk: std::mem::take(&mut self.blk), mvf: std::mem::take(&mut self.mvf) }
    }

    /// Use `b` (from [`PicState::take_bufs`]) as storage; call [`PicState::reset_window`] next.
    #[cfg_attr(not(feature = "threads"), allow(dead_code))]
    pub fn put_bufs(&mut self, b: WinBufs<S>) {
        (self.planes, self.blk, self.mvf) = (b.planes, b.blk, b.mvf);
    }

    /// Index into `blk` / `mvf` of the 4x4 block containing luma sample (x, y).
    #[inline(always)]
    pub fn bidx(&self, x: i32, y: i32) -> usize {
        ((y as usize).wrapping_sub(self.y_base) >> 2) * self.w4 + (x as usize >> 2)
    }

    /// First line of plane `c` held by `planes[c]`.
    #[inline(always)]
    pub fn line0(&self, c: usize) -> usize {
        if c == 0 { self.y_base } else { self.y_base / 2 }
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
        &self.blk[self.bidx(x, y)]
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

    /// Decode the slice segment `job` (slice `meta`, from [`prepare_slices`]) into the whole picture `pic`.
    pub fn decode(pic: &'a mut PicState<S>, job: &'a SliceJob, meta: Option<SliceMeta>) -> Result<()> {
        let Some(meta) = meta else { return invalid("dependent slice segment without a preceding slice") };
        ensure!(job.sh.data_offset < job.rbsp.len(), "empty slice segment data");
        let ts = *pic.layout.rs_to_ts.get(job.sh.segment_address as usize).ok_or(crate::Error::Invalid("slice segment address out of range".into()))?;
        let mut d = Self::new(pic, job, meta, None, job.sh.data_offset)?;
        d.decode_data(ts, Mode::Picture, true).map(|_| ())
    }

    /// Decode a substream of slice segment `job` (from `start`, tile scan) into the window `pic`,
    /// up to the end of the unit or of the segment. Returns where to resume when a hook said to
    /// stop.
    #[cfg_attr(not(feature = "threads"), allow(dead_code))]
    pub fn decode_unit(pic: &'a mut PicState<S>, job: &'a SliceJob, meta: Option<SliceMeta>, start: Start, mode: Mode<'_, S>) -> Result<Option<Resume>> {
        let Some(meta) = meta else { return invalid("dependent slice segment without a preceding slice") };
        match start {
            Start::At { pos, ts } => {
                ensure!(pos < job.rbsp.len(), "empty substream");
                let mut d = Self::new(pic, job, meta, None, pos)?;
                d.decode_data(ts, mode, true)
            }
            Start::Resume(r) => {
                let mut d = Self::new(pic, job, meta, Some(&r.cabac), 0)?;
                d.last_qp = r.last_qp;
                d.decode_data(r.ts, mode, r.first)
            }
        }
    }

    /// A decoder of slice segment `job` reading from `rbsp` byte `pos` with initialised contexts,
    /// or continuing from `resume`.
    fn new(pic: &'a mut PicState<S>, job: &'a SliceJob, meta: SliceMeta, resume: Option<&CabacState>, pos: usize) -> Result<Self> {
        let sh = &job.sh;
        let (sps, pps) = (&*job.sps, &*job.pps);
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
        let (slice_idx, slice_addr) = (meta.idx, meta.addr);
        let col = if sh.temporal_mvp && !sh.is_intra() {
            let l = if sh.is_b() && !sh.collocated_from_l0 { 1 } else { 0 };
            job.refs[l].get(sh.collocated_ref_idx as usize).cloned()
        } else {
            None
        };
        let no_backward_pred = job.refs.iter().flatten().all(|r| r.poc <= pic.poc);
        let log2_min_cu_qp_delta = sps.log2_ctb - pps.diff_cu_qp_delta_depth;
        let c = match resume {
            Some(s) => Cabac::resume(&job.rbsp, s),
            None => Cabac::new(&job.rbsp, pos, slice_qp, init_type)?,
        };
        Ok(SliceDecoder {
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
        })
    }

    /// Decode CTBs from `ts` (tile scan) on, as `mode` says (`first`: no CTB of the substream
    /// decoded yet). Returns where to resume when a hook said to stop.
    fn decode_data(&mut self, mut ts: u32, mode: Mode<'_, S>, mut first: bool) -> Result<Option<Resume>> {
        let layout = self.pic.layout.clone();
        let wctb = self.pic.wctb as u32;
        let npic = (self.pic.wctb * self.pic.hctb) as u32;
        // first CTB of the slice segment (dependent segments restore contexts there)
        let seg_start = layout.rs_to_ts.get(self.sh.segment_address as usize).copied();
        loop {
            ensure!(ts < npic, "CTB address out of range");
            let rs = layout.ts_to_rs[ts as usize];
            ensure!(self.pic.ctb_slice[rs as usize] == u32::MAX, "CTB {rs} decoded twice");
            let (rx, ry) = ((rs % wctb) as i32, (rs / wctb) as i32);
            let ctb = 1i32 << self.sps.log2_ctb;
            self.ctb_x = rx * ctb;
            self.ctb_y = ry * ctb;
            if let Mode::Unit { hooks: Some(h), .. } = mode
                && !h.before(self.pic, rx as usize, ry as usize)
            {
                return Ok(Some(Resume { ts, cabac: self.c.save(), last_qp: self.last_qp, first }));
            }
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
            } else if first && self.sh.dependent && seg_start == Some(ts) {
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
            let store = self.pps.entropy_coding_sync
                && (rs % wctb == 1 || (rs > 1 && layout.tile_id[ts as usize] != layout.tile_id[layout.rs_to_ts[rs as usize - 2] as usize]));
            if store {
                self.pic.wpp_ctx = Some(Box::new(self.c.ctx));
            }
            match mode {
                Mode::Picture => crate::filter::ctb_decoded(self.pic, ry as usize),
                Mode::Unit { hooks: Some(h), .. } => h.after(self.pic, rx as usize, ry as usize, store),
                Mode::Unit { hooks: None, .. } => {}
            }
            ts += 1;
            if end {
                if self.pps.dependent_slice_segments_enabled {
                    self.pic.ds_ctx = Some((Box::new(self.c.ctx), self.last_qp));
                }
                break;
            }
            if let Mode::Unit { end_ts, .. } = mode
                && ts >= end_ts
            {
                // the substream ends with the unit (a WPP row or a tile)
                ensure!(self.c.decode_terminate() == 1, "end_of_subset_one_bit is 0");
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
        Ok(None)
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

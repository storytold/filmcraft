//! Top-level decoder: NAL dispatch, picture boundaries, POC, DPB and output.
//!
//! The calling thread parses headers and does all DPB / reference list bookkeeping (which only needs
//! header information). Macroblock decoding of each picture runs as a job — on a thread pool with the
//! `threads` feature — that publishes finished macroblock rows into the picture's shared [`Frame`];
//! jobs of later pictures block per row on the reference data they need, so several pictures decode
//! concurrently (frame-level parallelism).

use crate::accel::{AccelOutput, AccelPicture, AccelRef, AccelSlice, Accelerator};
use crate::dpb::{Dpb, Output, OutputMeta};
use crate::error::{Error, Result, ensure, invalid, unsupported};
use crate::params::{Pps, Sps};
use crate::picture::{Frame, FrameRef, MbKind, MbState, Planes, RefPic};
use crate::slice::{NalHeader, Poc, PocState, SliceHeader, SliceType, nal_type};
use crate::slicedec::{PicState, SliceDecoder};
use crate::transform::LevelScale;
use crate::{ColorInfo, Picture};
use filmcraft_bitstream::{BitReader, annexb_nals, length_prefixed_nals, unescape_rbsp};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// One slice of a picture job.
struct SliceJob {
    sh: SliceHeader,
    pps: Arc<Pps>,
    sps: Arc<Sps>,
    rbsp: Vec<u8>,
    /// The NAL unit as in the stream (kept only with an accelerator).
    nal: Vec<u8>,
    refs: [Vec<RefPic>; 2],
    ls: Arc<LevelScale>,
}

/// A picture whose slices are being collected.
struct PendingPic {
    frame: FrameRef,
    sps: Arc<Sps>,
    first: SliceHeader,
    poc: Poc,
    pts: i64,
    key: bool,
    slices: Vec<SliceJob>,
}

/// Counters describing what the decoder has seen (useful to check test coverage).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecodeStats {
    pub pictures: u64,
    pub slices_cavlc: u64,
    pub slices_cabac: u64,
    pub slices_i: u64,
    pub slices_p: u64,
    pub slices_b: u64,
    pub mb_i4x4: u64,
    pub mb_i8x8: u64,
    pub mb_i16x16: u64,
    pub mb_pcm: u64,
    pub mb_p_skip: u64,
    pub mb_b_skip: u64,
    pub mb_b_direct16x16: u64,
    pub mb_inter: u64,
    /// Inter macroblocks using the 8x8 transform.
    pub mb_inter_8x8_transform: u64,
    pub mmco_ops: u64,
    pub long_term_marks: u64,
    pub frame_num_gaps: u64,
    pub weighted_slices: u64,
    pub temporal_direct_slices: u64,
    /// Non-reference pictures decoded in draft mode (deblocking skipped).
    pub draft_pictures: u64,
}

/// State shared with decoding jobs.
#[derive(Default)]
struct Shared {
    error: Mutex<Option<Error>>,
    stats: Mutex<DecodeStats>,
    /// Recycled picture buffers.
    pool: Mutex<Vec<PicState>>,
}

/// H.264 decoder.
pub struct Decoder {
    spss: Vec<Option<Arc<Sps>>>,
    ppss: Vec<Option<Arc<Pps>>>,
    nal_length_size: Option<usize>,
    dpb: Dpb,
    poc_state: PocState,
    prev_ref_frame_num: u32,
    pending: Option<PendingPic>,
    next_id: u32,
    active_sps: Option<Arc<Sps>>,
    ls_cache: Vec<(Arc<Pps>, Arc<LevelScale>)>,
    /// Pictures leaving the DPB, converted once their decoding job has finished.
    out_queue: VecDeque<Output>,
    shared: Arc<Shared>,
    #[cfg(feature = "threads")]
    pool: Option<rayon::ThreadPool>,
    in_flight: VecDeque<FrameRef>,
    max_in_flight: usize,
    threads: usize,
    /// Draft mode: skip deblocking of non-reference pictures ([`Decoder::set_draft`]).
    draft: bool,
    /// Hardware acceleration: pictures go to it instead of being reconstructed here.
    accel: Option<Box<dyn Accelerator>>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

/// Is `sh` the first slice of a new primary coded picture relative to `prev` (7.4.1.2.4)?
fn is_new_picture(prev: &SliceHeader, sh: &SliceHeader, sps: &Sps) -> bool {
    if sh.frame_num != prev.frame_num
        || sh.pps_id != prev.pps_id
        || sh.field_pic != prev.field_pic
        || sh.bottom_field != prev.bottom_field
        || (sh.nal_ref_idc == 0) != (prev.nal_ref_idc == 0)
        || sh.idr != prev.idr
        || (sh.idr && prev.idr && sh.idr_pic_id != prev.idr_pic_id)
    {
        return true;
    }
    if sps.pic_order_cnt_type == 0 && (sh.pic_order_cnt_lsb != prev.pic_order_cnt_lsb || sh.delta_pic_order_cnt_bottom != prev.delta_pic_order_cnt_bottom) {
        return true;
    }
    if sps.pic_order_cnt_type == 1 && sh.delta_pic_order_cnt != prev.delta_pic_order_cnt {
        return true;
    }
    false
}

/// The worker threads [`Decoder::new`] uses: the available cores (at most 16) with the `threads`
/// feature on native targets, 1 otherwise.
pub fn default_threads() -> usize {
    #[cfg(all(feature = "threads", not(target_arch = "wasm32")))]
    {
        std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1).min(16)
    }
    #[cfg(not(all(feature = "threads", not(target_arch = "wasm32"))))]
    {
        1
    }
}

impl Decoder {
    /// A decoder using all available cores (with the `threads` feature).
    pub fn new() -> Self {
        Self::with_threads(default_threads())
    }

    /// A decoder using up to `threads` worker threads (1 = decode on the calling thread).
    pub fn with_threads(threads: usize) -> Self {
        crate::cavlc::init_tables();
        #[cfg(feature = "threads")]
        let pool = if threads > 1 && cfg!(not(target_arch = "wasm32")) {
            rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|i| format!("h264-{i}")).build().ok()
        } else {
            None
        };
        #[cfg(feature = "threads")]
        let workers = if pool.is_some() { threads } else { 1 };
        #[cfg(not(feature = "threads"))]
        let workers = 1;
        Decoder {
            spss: vec![None; 32],
            ppss: vec![None; 256],
            nal_length_size: None,
            dpb: Dpb::new(),
            poc_state: PocState::default(),
            prev_ref_frame_num: 0,
            pending: None,
            next_id: 1,
            active_sps: None,
            ls_cache: Vec::new(),
            out_queue: VecDeque::new(),
            shared: Arc::new(Shared::default()),
            #[cfg(feature = "threads")]
            pool,
            in_flight: VecDeque::new(),
            max_in_flight: threads.max(1) + 2,
            threads: workers,
            draft: false,
            accel: None,
        }
    }

    /// A decoder whose pictures are decoded by `accel` ([`crate::accel`]): use
    /// [`Decoder::decode_accel`] / [`Decoder::flush_accel`] instead of `decode` / `flush`.
    pub fn with_accelerator(accel: Box<dyn Accelerator>) -> Self {
        let mut d = Self::with_threads(1);
        d.accel = Some(accel);
        d
    }

    /// [`Decoder::decode`] with an accelerator: pictures that became ready for output, by id, in
    /// output order.
    pub fn decode_accel(&mut self, data: &[u8], pts: i64) -> Result<Vec<AccelOutput>> {
        if self.accel.is_none() {
            return invalid("decode_accel without an accelerator");
        }
        let nals = match self.nal_length_size {
            Some(n) => length_prefixed_nals(data, n)?,
            None => annexb_nals(data),
        };
        let mut result = Ok(());
        for nal in nals {
            if let Err(e) = self.handle_nal(nal, pts) {
                result = Err(e);
                break;
            }
        }
        self.submit_pending();
        if let Some(e) = self.take_error() {
            result = result.and(Err(e));
        }
        result?;
        Ok(self.out_queue.drain(..).map(|o| accel_output(&o)).collect())
    }

    /// [`Decoder::flush`] with an accelerator.
    pub fn flush_accel(&mut self) -> Vec<AccelOutput> {
        self.submit_pending();
        let mut outs = Vec::new();
        self.dpb.flush(&mut outs);
        self.emit(outs);
        self.out_queue.drain(..).map(|o| accel_output(&o)).collect()
    }

    /// Worker threads decoding pictures (1: on the calling thread).
    pub fn threads(&self) -> usize {
        self.threads
    }

    /// Draft mode (off by default) for reduced-resolution playback: pictures submitted from now
    /// on that are non-reference pictures (`nal_ref_idc` 0) skip the deblocking filter (8.7).
    /// Nothing predicts from a non-reference picture (and deblocking never changes the motion
    /// data used for direct prediction), so every other picture stays bit-exact; the draft
    /// pictures themselves are approximate and flagged [`Picture::draft`]. Never use it for
    /// export or for a frame shown while paused.
    pub fn set_draft(&mut self, on: bool) {
        self.draft = on;
    }

    pub fn draft(&self) -> bool {
        self.draft
    }

    /// Configure from an `avcC` (AVCDecoderConfigurationRecord) box payload.
    pub fn from_avcc(avcc: &[u8]) -> Result<Self> {
        let mut d = Decoder::new();
        d.configure_avcc(avcc)?;
        Ok(d)
    }

    /// Parse an `avcC` record: SPS/PPS and the NAL length size used by [`Decoder::decode`].
    pub fn configure_avcc(&mut self, avcc: &[u8]) -> Result<()> {
        ensure!(avcc.len() >= 7, "avcC too short");
        ensure!(avcc[0] == 1, "unsupported avcC version {}", avcc[0]);
        self.nal_length_size = Some((avcc[4] & 3) as usize + 1);
        let mut pos = 5;
        let read_sets = |pos: &mut usize, count: usize, d: &mut Decoder| -> Result<()> {
            for _ in 0..count {
                ensure!(*pos + 2 <= avcc.len(), "avcC truncated");
                let len = u16::from_be_bytes([avcc[*pos], avcc[*pos + 1]]) as usize;
                *pos += 2;
                ensure!(*pos + len <= avcc.len(), "avcC truncated");
                d.handle_nal(&avcc[*pos..*pos + len], 0)?;
                *pos += len;
            }
            Ok(())
        };
        let nsps = (avcc[pos] & 0x1f) as usize;
        pos += 1;
        read_sets(&mut pos, nsps, self)?;
        ensure!(pos < avcc.len(), "avcC truncated");
        let npps = avcc[pos] as usize;
        pos += 1;
        read_sets(&mut pos, npps, self)
    }

    /// Length of NAL length prefixes (from avcC), or None for Annex-B input.
    pub fn nal_length_size(&self) -> Option<usize> {
        self.nal_length_size
    }

    /// Decode one access unit (all NAL units of one picture, Annex-B or length-prefixed per
    /// configuration; a byte stream with several complete access units is accepted too). Returns
    /// pictures that became ready for output, in output order; `pts` travels with the picture of
    /// this access unit through reordering.
    pub fn decode(&mut self, data: &[u8], pts: i64) -> Result<Vec<Picture>> {
        let nals = match self.nal_length_size {
            Some(n) => length_prefixed_nals(data, n)?,
            None => annexb_nals(data),
        };
        let mut result = Ok(());
        for nal in nals {
            if let Err(e) = self.handle_nal(nal, pts) {
                result = Err(e);
                break;
            }
        }
        // The access unit is complete: hand its picture to a decoding job.
        self.submit_pending();
        if let Some(e) = self.take_error() {
            result = result.and(Err(e));
        }
        result?;
        // Return finished pictures without blocking on frames still being decoded (bounded latency).
        let max_queue = self.max_in_flight * 2;
        let mut pics = Vec::new();
        while let Some(o) = self.out_queue.front() {
            if !o.frame.is_published(o.frame.mb_h() - 1) && self.out_queue.len() <= max_queue {
                break;
            }
            let Some(o) = self.out_queue.pop_front() else { break };
            pics.push(make_picture(&o));
        }
        Ok(pics)
    }

    /// Output all remaining pictures (end of stream).
    pub fn flush(&mut self) -> Vec<Picture> {
        self.submit_pending();
        let mut outs = Vec::new();
        self.dpb.flush(&mut outs);
        self.emit(outs);
        let pics = self.out_queue.drain(..).map(|o| make_picture(&o)).collect();
        while let Some(f) = self.in_flight.pop_front() {
            f.wait_complete();
        }
        pics
    }

    /// Statistics accumulated so far (macroblock counts include finished pictures only).
    pub fn stats(&self) -> DecodeStats {
        self.shared.stats.lock().map(|s| s.clone()).unwrap_or_default()
    }

    fn stat(&self, f: impl FnOnce(&mut DecodeStats)) {
        if let Ok(mut s) = self.shared.stats.lock() {
            f(&mut s);
        }
    }

    /// First error reported by a decoding job since the last call (also returned by
    /// [`Decoder::decode`]); useful after [`Decoder::flush`].
    pub fn take_error(&mut self) -> Option<Error> {
        self.shared.error.lock().ok().and_then(|mut e| e.take())
    }

    fn handle_nal(&mut self, nal: &[u8], pts: i64) -> Result<()> {
        if nal.is_empty() {
            return Ok(());
        }
        let hdr = NalHeader::parse(nal[0])?;
        match hdr.nal_unit_type {
            nal_type::SPS => {
                let sps = Sps::parse(&unescape_rbsp(&nal[1..]))?;
                let id = sps.id as usize;
                self.spss[id] = Some(Arc::new(sps));
            }
            nal_type::PPS => {
                let rbsp = unescape_rbsp(&nal[1..]);
                let spss: Vec<Option<Sps>> = self.spss.iter().map(|s| s.as_ref().map(|s| (**s).clone())).collect();
                let pps = Pps::parse(&rbsp, &spss)?;
                let id = pps.id as usize;
                self.ppss[id] = Some(Arc::new(pps));
            }
            nal_type::SLICE | nal_type::IDR => self.handle_slice(nal, hdr, pts)?,
            nal_type::SLICE_DPA | nal_type::SLICE_DPB | nal_type::SLICE_DPC => {
                return unsupported("data partitioning (Extended profile)");
            }
            nal_type::END_SEQ | nal_type::END_STREAM => self.submit_pending(),
            // SEI, AUD, filler, SPS extension, prefix NAL, subset SPS, auxiliary and extension slices: skipped.
            _ => {}
        }
        Ok(())
    }

    fn level_scale(&mut self, pps: &Arc<Pps>) -> Arc<LevelScale> {
        if let Some((_, ls)) = self.ls_cache.iter().find(|(p, _)| Arc::ptr_eq(p, pps)) {
            return ls.clone();
        }
        let ls: Arc<LevelScale> = Arc::from(LevelScale::new(&pps.scaling));
        self.ls_cache.retain(|(p, _)| self.ppss.iter().flatten().any(|q| Arc::ptr_eq(p, q)));
        self.ls_cache.push((pps.clone(), ls.clone()));
        ls
    }

    fn handle_slice(&mut self, nal: &[u8], hdr: NalHeader, pts: i64) -> Result<()> {
        let rbsp = unescape_rbsp(&nal[1..]);
        let ppss = &self.ppss;
        let spss = &self.spss;
        let mut found: Option<(Arc<Pps>, Arc<Sps>)> = None;
        let (sh, _, _) = SliceHeader::parse(&rbsp, hdr, |id| {
            let pps = ppss[id as usize].as_ref().ok_or_else(|| Error::MissingParameterSet(format!("PPS {id}")))?;
            let sps = spss[pps.sps_id as usize].as_ref().ok_or_else(|| Error::MissingParameterSet(format!("SPS {}", pps.sps_id)))?;
            found = Some((pps.clone(), sps.clone()));
            Ok((&**pps, &**sps))
        })?;
        let Some((pps, sps)) = found else {
            return Err(Error::MissingParameterSet("PPS".into()));
        };
        sps.check_supported()?;
        if pps.num_slice_groups > 1 {
            return unsupported("slice groups (FMO)");
        }
        if sh.field_pic {
            return unsupported("field pictures");
        }
        if matches!(sh.slice_type, SliceType::Sp | SliceType::Si) {
            return unsupported("SP/SI slices");
        }
        if sh.redundant_pic_cnt > 0 {
            return Ok(()); // redundant slices are ignored
        }
        let new_pic = match &self.pending {
            None => true,
            Some(p) => is_new_picture(&p.first, &sh, &sps) || sh.first_mb_in_slice == 0 || !Arc::ptr_eq(&p.sps, &sps),
        };
        if new_pic {
            self.submit_pending();
            if sh.first_mb_in_slice != 0 && self.active_sps.is_none() {
                return invalid("stream does not start with the first slice of a picture");
            }
            self.start_picture(&sh, &sps, pts)?;
        }
        let ls = self.level_scale(&pps);
        let Some(pending) = self.pending.as_mut() else {
            return invalid("slice without a started picture");
        };
        let refs = self.dpb.build_ref_lists(&sh, pending.poc.frame(), sps.max_frame_num())?;
        if !sh.slice_type.is_intra() {
            ensure!(!refs[0].is_empty(), "no reference pictures available for inter slice");
            if sh.slice_type.is_b() {
                ensure!(!refs[1].is_empty(), "empty RefPicList1 in B slice");
            }
        }
        let weighted = (pps.weighted_pred && sh.slice_type.is_p()) || (pps.weighted_bipred_idc != 0 && sh.slice_type.is_b());
        let temporal = sh.slice_type.is_b() && !sh.direct_spatial_mv_pred;
        let (cabac, st, mmcos, lt) = (
            pps.entropy_coding_mode,
            sh.slice_type,
            sh.mmcos.len() as u64,
            sh.long_term_reference as u64 + sh.mmcos.iter().filter(|m| m.op == 3 || m.op == 6).count() as u64,
        );
        let nal = if self.accel.is_some() { nal.to_vec() } else { Vec::new() };
        pending.slices.push(SliceJob { sh, pps, sps, rbsp, nal, refs, ls });
        self.stat(|s| {
            if cabac {
                s.slices_cabac += 1;
            } else {
                s.slices_cavlc += 1;
            }
            match st {
                SliceType::I | SliceType::Si => s.slices_i += 1,
                SliceType::P | SliceType::Sp => s.slices_p += 1,
                SliceType::B => s.slices_b += 1,
            }
            s.weighted_slices += weighted as u64;
            s.temporal_direct_slices += temporal as u64;
            s.mmco_ops += mmcos;
            s.long_term_marks += lt;
        });
        Ok(())
    }

    fn start_picture(&mut self, sh: &SliceHeader, sps: &Arc<Sps>, pts: i64) -> Result<()> {
        // Activate SPS; a resolution change flushes the DPB.
        let changed = match &self.active_sps {
            None => true,
            Some(a) => a.width() != sps.width() || a.height() != sps.height() || a.max_dpb_frames() != sps.max_dpb_frames(),
        };
        if changed && self.active_sps.is_some() {
            let mut outs = Vec::new();
            self.dpb.flush(&mut outs);
            self.emit(outs);
            self.dpb.entries.clear();
        }
        self.active_sps = Some(sps.clone());
        self.dpb.capacity = sps.max_dpb_frames();
        self.dpb.max_reorder = sps.max_num_reorder_frames().min(self.dpb.capacity);
        let mb_w = sps.pic_width_in_mbs as usize;
        let mb_h = sps.frame_height_in_mbs() as usize;
        if sh.idr {
            let mut outs = Vec::new();
            self.dpb.idr(sh.no_output_of_prior_pics, &mut outs);
            self.emit(outs);
            self.prev_ref_frame_num = 0;
        } else {
            let max = sps.max_frame_num();
            if sh.frame_num != self.prev_ref_frame_num && sh.frame_num != (self.prev_ref_frame_num + 1) % max {
                if self.accel.is_some() {
                    return unsupported("frame_num gaps with hardware decoding");
                }
                // frame_num gap (8.2.5.2): insert "non-existing" frames. Their samples are never used by
                // conforming streams; copy the latest decoded frame for concealment.
                let meta = Arc::new(output_meta(sps, pts, false));
                let next_id = &mut self.next_id;
                let poc_state = &mut self.poc_state;
                let last = self.dpb.entries.iter().filter(|e| !e.non_existing).max_by_key(|e| e.frame.id).map(|e| e.frame.clone());
                let mut make = |_fnum: u32| {
                    let id = *next_id;
                    *next_id += 1;
                    let planes = match &last {
                        Some(f) if f.mb_w == mb_w && f.mb_h() == mb_h => {
                            let mut p = Planes::new(mb_w * 16, mb_h * 16);
                            let (y, u, v) = f.copy_cropped((0, 0, mb_w * 16, mb_h * 16));
                            p.y = y;
                            p.cb = u;
                            p.cr = v;
                            p
                        }
                        _ => Planes::gray(mb_w * 16, mb_h * 16),
                    };
                    (Arc::new(Frame::from_planes(id, 0, &planes)), meta.clone())
                };
                let mut upd = |fnum: u32| poc_state.update_gap_frame(fnum, sps);
                self.shared.stats.lock().map(|mut s| s.frame_num_gaps += 1).ok();
                self.dpb.fill_frame_num_gap(self.prev_ref_frame_num, sh.frame_num, max, sps.max_num_ref_frames as usize, &mut make, &mut upd);
                self.prev_ref_frame_num = (sh.frame_num + max - 1) % max;
            }
        }
        let poc = self.poc_state.compute(sh, sps);
        // MMCO5: the picture's POC becomes relative to itself (tempPicOrderCnt, 8.2.1).
        let frame_poc = if sh.has_mmco5() { 0 } else { poc.frame() };
        let id = self.next_id;
        self.next_id += 1;
        let frame = Arc::new(Frame::new(id, frame_poc, mb_w, mb_h));
        self.pending = Some(PendingPic { frame, sps: sps.clone(), first: sh.clone(), poc, pts, key: sh.idr, slices: Vec::new() });
        Ok(())
    }

    /// Start decoding the pending picture and do its reference marking / DPB insertion.
    fn submit_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        let PendingPic { frame, sps, first, poc, pts, key, slices } = p;
        let draft = self.draft && first.nal_ref_idc == 0;
        if self.accel.is_some() {
            if let Err(e) = self.accelerate(&frame, &first, &sps, poc, &slices)
                && let Ok(mut err) = self.shared.error.lock()
            {
                err.get_or_insert(Error::Invalid(format!("hardware decoding: {e}")));
            }
        } else {
            self.dispatch(frame.clone(), slices, draft);
        }
        self.poc_state.update(&first, &poc);
        if first.nal_ref_idc != 0 {
            self.prev_ref_frame_num = if first.has_mmco5() { 0 } else { first.frame_num };
        }
        let meta = Arc::new(OutputMeta { draft, ..output_meta(&sps, pts, key) });
        let mut outs = Vec::new();
        let fpoc = frame.poc;
        self.dpb.store_picture(&first, frame, fpoc, sps.max_frame_num(), sps.max_num_ref_frames as usize, meta, &mut outs);
        self.emit(outs);
        if let Some(a) = self.accel.as_mut() {
            let live: Vec<u32> = self.dpb.entries.iter().map(|e| e.frame.id).chain(self.out_queue.iter().map(|o| o.frame.id)).collect();
            a.retain(&live);
        }
    }

    /// Hand one picture to the accelerator.
    fn accelerate(&mut self, frame: &FrameRef, first: &SliceHeader, sps: &Arc<Sps>, poc: Poc, slices: &[SliceJob]) -> std::result::Result<(), String> {
        let as_ref = |id: u32, poc: i32, long: bool| -> AccelRef {
            let e = self.dpb.entries.iter().find(|e| e.frame.id == id);
            AccelRef { id, poc, frame_num: e.map_or(0, |e| e.frame_num), long_term_frame_idx: e.map_or(0, |e| e.long_term_frame_idx), long_term: long }
        };
        let refs: Vec<AccelRef> = self
            .dpb
            .entries
            .iter()
            .filter(|e| e.mark != crate::dpb::RefMark::Unused)
            .map(|e| as_ref(e.frame.id, e.poc, e.mark == crate::dpb::RefMark::Long))
            .collect();
        let pps = slices.first().map(|s| s.pps.clone()).ok_or("picture without slices")?;
        let accel_slices: Vec<AccelSlice> = slices
            .iter()
            .map(|s| AccelSlice {
                header: &s.sh,
                pps: &s.pps,
                nal: &s.nal,
                data_bit_offset: 8 + s.sh.header_bits,
                lists: [0, 1].map(|l| s.refs[l].iter().map(|r| as_ref(r.id(), r.poc(), r.long_term)).collect()),
            })
            .collect();
        let pic = AccelPicture {
            id: frame.id,
            poc: (poc.top, poc.bottom),
            frame_num: first.frame_num,
            reference: first.nal_ref_idc != 0,
            idr: first.idr,
            sps,
            pps: &pps,
            refs,
            slices: accel_slices,
        };
        let Some(a) = self.accel.as_mut() else { return Err("no accelerator".into()) };
        a.decode_picture(&pic)
    }

    fn dispatch(&mut self, frame: FrameRef, slices: Vec<SliceJob>, draft: bool) {
        let shared = self.shared.clone();
        #[cfg(feature = "threads")]
        if let Some(pool) = &self.pool {
            self.in_flight.retain(|f| !f.is_published(f.mb_h() - 1));
            while self.in_flight.len() >= self.max_in_flight {
                if let Some(f) = self.in_flight.pop_front() {
                    f.wait_complete();
                }
            }
            self.in_flight.push_back(frame.clone());
            pool.spawn_fifo(move || run_job(frame, slices, &shared, draft));
            return;
        }
        run_job(frame, slices, &shared, draft);
    }

    fn emit(&mut self, outs: Vec<Output>) {
        self.out_queue.extend(outs);
    }
}

/// Decode all slices of one picture and publish its rows.
fn run_job(frame: FrameRef, slices: Vec<SliceJob>, shared: &Shared, draft: bool) {
    let reuse = shared.pool.lock().ok().and_then(|mut p| p.pop());
    let mut pic = match reuse {
        Some(mut p) if p.mb_w == frame.mb_w && p.mb_h == frame.mb_h() => {
            p.reset(frame.clone());
            p
        }
        _ => PicState::new(frame.clone()),
    };
    pic.skip_deblock = draft;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut first_err = None;
        for s in &slices {
            if let Err(e) = decode_slice(s, &mut pic) {
                first_err.get_or_insert(e);
            }
        }
        pic.flush_rows(true);
        first_err
    }));
    let err = match result {
        Ok(e) => e,
        Err(_) => {
            // Make sure readers never block on this frame.
            let fallback = Planes::gray(frame.width, frame.height);
            let blank = vec![MbState::default(); frame.mb_w * frame.mb_h()];
            for r in 0..frame.mb_h() {
                if !frame.is_published(r) {
                    frame.publish(r, Frame::make_row(&fallback, r, &blank, &|_, _, _| u32::MAX));
                }
            }
            pic = PicState::new(frame.clone());
            Some(Error::Invalid("internal error while decoding a picture".into()))
        }
    };
    if let Ok(mut s) = shared.stats.lock() {
        s.pictures += 1;
        s.draft_pictures += draft as u64;
        for st in &pic.mbs {
            match st.kind {
                MbKind::I4x4 => s.mb_i4x4 += 1,
                MbKind::I8x8 => s.mb_i8x8 += 1,
                MbKind::I16x16 => s.mb_i16x16 += 1,
                MbKind::IPcm => s.mb_pcm += 1,
                MbKind::PSkip => s.mb_p_skip += 1,
                MbKind::BSkip => s.mb_b_skip += 1,
                MbKind::BDirect16x16 => s.mb_b_direct16x16 += 1,
                MbKind::Inter => s.mb_inter += 1,
                MbKind::None => {}
            }
            if st.transform_8x8 && !st.kind.is_intra() {
                s.mb_inter_8x8_transform += 1;
            }
        }
    }
    if let (Some(e), Ok(mut slot)) = (err, shared.error.lock()) {
        slot.get_or_insert(e);
    }
    if let Ok(mut p) = shared.pool.lock()
        && p.len() < 4
    {
        p.push(pic);
    }
}

fn decode_slice(s: &SliceJob, pic: &mut PicState) -> Result<()> {
    let mut sd = SliceDecoder::new(&s.sh, &s.pps, &s.sps, pic, &s.refs, &s.ls)?;
    if s.pps.entropy_coding_mode {
        sd.decode_cabac(&s.rbsp)
    } else {
        let mut r = BitReader::new(&s.rbsp);
        r.seek_bits(s.sh.header_bits);
        sd.decode_cavlc(&mut r)
    }
}

fn output_meta(sps: &Sps, pts: i64, key: bool) -> OutputMeta {
    let vui = sps.vui.clone().unwrap_or_default();
    OutputMeta {
        pts,
        key,
        crop: sps.crop_rect(),
        full_range: vui.full_range,
        colour_primaries: vui.colour_primaries,
        transfer_characteristics: vui.transfer_characteristics,
        matrix_coefficients: vui.matrix_coefficients,
        sar: vui.sar,
        draft: false,
    }
}

fn accel_output(o: &Output) -> AccelOutput {
    let m = &o.meta;
    AccelOutput {
        id: o.frame.id,
        pts: m.pts,
        key: m.key,
        crop: m.crop,
        full_range: m.full_range,
        colour_primaries: m.colour_primaries,
        transfer_characteristics: m.transfer_characteristics,
        matrix_coefficients: m.matrix_coefficients,
        sar: m.sar,
    }
}

fn make_picture(o: &Output) -> Picture {
    let (cx, cy, cw, ch) = o.meta.crop;
    let (cx, cy, cw, ch) = (cx as usize, cy as usize, cw as usize, ch as usize);
    let (y, u, v) = o.frame.copy_cropped((cx, cy, cw, ch));
    let (ccw, cch) = (cw.div_ceil(2), ch.div_ceil(2));
    Picture {
        width: cw as u32,
        height: ch as u32,
        chroma_width: ccw as u32,
        chroma_height: cch as u32,
        y,
        u,
        v,
        y_stride: cw,
        uv_stride: ccw,
        pts: o.meta.pts,
        poc: o.poc,
        key: o.meta.key,
        color: ColorInfo {
            full_range: o.meta.full_range,
            primaries: o.meta.colour_primaries,
            transfer: o.meta.transfer_characteristics,
            matrix: o.meta.matrix_coefficients,
        },
        sar: o.meta.sar,
        draft: o.meta.draft,
    }
}

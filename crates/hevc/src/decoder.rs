//! Top-level decoder: NAL dispatch, picture boundaries, POC / RPS / DPB management and output.
//!
//! The calling thread parses parameter sets and slice headers and does all POC, RPS and DPB
//! bookkeeping. The CTU layer of each picture runs as a job (on a thread pool with the `threads`
//! feature) that publishes finished CTB rows into the picture's shared [`Frame`]; jobs of later pictures
//! block per row on the reference data they read, so several pictures decode concurrently.

use crate::accel::{AccelOutput, AccelPicture, AccelRef, AccelSlice, Accelerator};
use crate::dpb::{Dpb, Output, OutputMeta, PocState, RefPicSet, build_ref_lists};
use crate::error::{Error, Result, ensure, invalid, unsupported};
use crate::params::{Layout, Pps, Sps, Vps};
use crate::picture::{Frame, FrameRef};
use crate::slice::{NalHeader, SliceHeader, nal_type};
use crate::slicedec::{F_INTRA, F_SKIP, PicState, SliceDecoder, SliceJob};
use crate::{ColorInfo, Picture, Plane};
use filmcraft_bitstream::{annexb_nals, length_prefixed_nals, unescape_rbsp};
use std::collections::VecDeque;
use std::sync::{Arc, Mutex};

/// A picture whose slices are being collected.
struct PendingPic {
    frame: FrameRef,
    sps: Arc<Sps>,
    pps: Arc<Pps>,
    layout: Arc<Layout>,
    /// Header of the latest independent slice segment (for dependent slice segments).
    last_sh: SliceHeader,
    first_sh: SliceHeader,
    rps: RefPicSet,
    poc: i32,
    output: bool,
    meta: Arc<OutputMeta>,
    slices: Vec<SliceJob>,
}

/// Counters describing what the decoder has seen (useful to check test coverage).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DecodeStats {
    pub pictures: u64,
    pub slices_i: u64,
    pub slices_p: u64,
    pub slices_b: u64,
    pub dependent_slices: u64,
    pub idr: u64,
    pub cra: u64,
    pub skipped_rasl: u64,
    pub intra_4x4_blocks: u64,
    pub skip_4x4_blocks: u64,
    pub inter_4x4_blocks: u64,
    pub weighted_slices: u64,
    pub tmvp_slices: u64,
    pub sao_slices: u64,
    pub deblock_disabled_slices: u64,
    pub tiles_pictures: u64,
    pub wpp_pictures: u64,
    pub scaling_list_pictures: u64,
    pub transform_skip_pictures: u64,
    pub long_term_slices: u64,
    pub missing_refs: u64,
    /// Pictures decoded in draft mode without in-loop filters ([`Decoder::set_draft`]).
    pub draft_pictures: u64,
}

#[derive(Default)]
struct Shared {
    error: Mutex<Option<Error>>,
    stats: Mutex<DecodeStats>,
}

/// H.265 / HEVC decoder.
pub struct Decoder {
    vpss: Vec<Option<Arc<Vps>>>,
    spss: Vec<Option<Arc<Sps>>>,
    ppss: Vec<Option<Arc<Pps>>>,
    nal_length_size: Option<usize>,
    dpb: Dpb,
    poc_state: PocState,
    pending: Option<PendingPic>,
    next_id: u32,
    active_sps: Option<Arc<Sps>>,
    first_picture: bool,
    after_eos: bool,
    /// RASL pictures associated with the last IRAP are skipped (it had NoRaslOutputFlag = 1).
    skip_rasl: bool,
    layout_cache: Vec<(Arc<Pps>, Arc<Sps>, Arc<Layout>, Option<Arc<Vec<Vec<u8>>>>)>,
    out_queue: VecDeque<Output>,
    shared: Arc<Shared>,
    #[cfg(feature = "threads")]
    pool: Option<rayon::ThreadPool>,
    in_flight: VecDeque<FrameRef>,
    max_in_flight: usize,
    draft: bool,
    /// Hardware acceleration: pictures go to it instead of being reconstructed here.
    accel: Option<Box<dyn Accelerator>>,
}

impl Default for Decoder {
    fn default() -> Self {
        Self::new()
    }
}

fn default_threads() -> usize {
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
        #[cfg(feature = "threads")]
        let pool = if threads > 1 && cfg!(not(target_arch = "wasm32")) {
            rayon::ThreadPoolBuilder::new().num_threads(threads).thread_name(|i| format!("hevc-{i}")).build().ok()
        } else {
            None
        };
        Decoder {
            vpss: vec![None; 16],
            spss: vec![None; 16],
            ppss: vec![None; 64],
            nal_length_size: None,
            dpb: Dpb::default(),
            poc_state: PocState::default(),
            pending: None,
            next_id: 1,
            active_sps: None,
            first_picture: true,
            after_eos: false,
            skip_rasl: false,
            layout_cache: Vec::new(),
            out_queue: VecDeque::new(),
            shared: Arc::new(Shared::default()),
            #[cfg(feature = "threads")]
            pool,
            in_flight: VecDeque::new(),
            max_in_flight: threads.max(1) + 2,
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
        self.out_queue.extend(outs);
        self.first_picture = true;
        self.out_queue.drain(..).map(|o| accel_output(&o)).collect()
    }

    /// Configure from an `hvcC` (HEVCDecoderConfigurationRecord) box payload.
    pub fn from_hvcc(hvcc: &[u8]) -> Result<Self> {
        let mut d = Decoder::new();
        d.configure_hvcc(hvcc)?;
        Ok(d)
    }

    /// Parse an `hvcC` record: parameter-set arrays and the NAL length size used by [`Decoder::decode`].
    pub fn configure_hvcc(&mut self, hvcc: &[u8]) -> Result<()> {
        ensure!(hvcc.len() >= 23, "hvcC too short");
        ensure!(hvcc[0] == 1, "unsupported hvcC version {}", hvcc[0]);
        self.nal_length_size = Some((hvcc[21] & 3) as usize + 1);
        let num_arrays = hvcc[22] as usize;
        let mut pos = 23;
        for _ in 0..num_arrays {
            ensure!(pos + 3 <= hvcc.len(), "hvcC truncated");
            let n = u16::from_be_bytes([hvcc[pos + 1], hvcc[pos + 2]]) as usize;
            pos += 3;
            for _ in 0..n {
                ensure!(pos + 2 <= hvcc.len(), "hvcC truncated");
                let len = u16::from_be_bytes([hvcc[pos], hvcc[pos + 1]]) as usize;
                pos += 2;
                ensure!(pos + len <= hvcc.len(), "hvcC truncated");
                self.handle_nal(&hvcc[pos..pos + len], 0)?;
                pos += len;
            }
        }
        Ok(())
    }

    /// Length of NAL length prefixes (from hvcC), or None for Annex-B input.
    pub fn nal_length_size(&self) -> Option<usize> {
        self.nal_length_size
    }

    /// Decode one access unit (all NAL units of one picture, Annex-B or length-prefixed per
    /// configuration; a byte stream with several complete access units is accepted too). Returns
    /// pictures that became ready for output, in output order; `pts` travels with the picture of this
    /// access unit through reordering.
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
        self.submit_pending();
        if let Some(e) = self.take_error() {
            result = result.and(Err(e));
        }
        result?;
        let max_queue = self.max_in_flight * 2;
        let mut pics = Vec::new();
        while let Some(o) = self.out_queue.front() {
            if !o.frame.is_complete() && self.out_queue.len() <= max_queue {
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
        self.out_queue.extend(outs);
        let pics = self.out_queue.drain(..).map(|o| make_picture(&o)).collect();
        while let Some(f) = self.in_flight.pop_front() {
            f.wait_complete();
        }
        self.first_picture = true;
        pics
    }

    /// Statistics accumulated so far.
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
    /// Draft mode for reduced-resolution playback (off by default): sub-layer non-reference
    /// pictures of the highest temporal sub-layer (pictures nothing predicts from) skip
    /// deblocking and SAO and come out flagged [`Picture::draft`]; every other picture is
    /// unchanged.
    pub fn set_draft(&mut self, on: bool) {
        self.draft = on;
    }

    pub fn take_error(&mut self) -> Option<Error> {
        self.shared.error.lock().ok().and_then(|mut e| e.take())
    }

    fn handle_nal(&mut self, nal: &[u8], pts: i64) -> Result<()> {
        if nal.len() < 2 {
            return Ok(());
        }
        let hdr = NalHeader::parse(nal)?;
        if hdr.layer_id != 0 {
            return Ok(());
        }
        match hdr.nal_type {
            nal_type::VPS => {
                let v = Vps::parse(&unescape_rbsp(&nal[2..]))?;
                let id = v.id as usize;
                self.vpss[id] = Some(Arc::new(v));
            }
            nal_type::SPS => {
                let s = Sps::parse(&unescape_rbsp(&nal[2..]))?;
                let id = s.id as usize;
                self.spss[id] = Some(Arc::new(s));
            }
            nal_type::PPS => {
                let p = Pps::parse(&unescape_rbsp(&nal[2..]))?;
                let id = p.id as usize;
                self.ppss[id] = Some(Arc::new(p));
            }
            nal_type::EOS | nal_type::EOB => {
                self.submit_pending();
                self.after_eos = true;
            }
            0..=9 | 16..=21 => self.handle_slice(nal, hdr, pts)?,
            // AUD, SEI, filler, reserved and unspecified NAL units are skipped
            _ => {}
        }
        Ok(())
    }

    fn layout_for(&mut self, pps: &Arc<Pps>, sps: &Arc<Sps>) -> Result<(Arc<Layout>, Option<Arc<Vec<Vec<u8>>>>)> {
        if let Some((_, _, l, s)) = self.layout_cache.iter().find(|(p, s, _, _)| Arc::ptr_eq(p, pps) && Arc::ptr_eq(s, sps)) {
            return Ok((l.clone(), s.clone()));
        }
        let layout = Arc::new(Layout::new(sps, pps)?);
        let scaling = if sps.scaling_list_enabled {
            let list = pps.scaling_list.as_ref().or(sps.scaling_list.as_ref()).cloned().unwrap_or_else(crate::params::ScalingList::default_lists);
            let mut v = Vec::with_capacity(24);
            for size in 0..4 {
                for m in 0..6 {
                    v.push(list.factor(size, m));
                }
            }
            Some(Arc::new(v))
        } else {
            None
        };
        if self.layout_cache.len() > 8 {
            self.layout_cache.remove(0);
        }
        self.layout_cache.push((pps.clone(), sps.clone(), layout.clone(), scaling.clone()));
        Ok((layout, scaling))
    }

    fn handle_slice(&mut self, nal: &[u8], hdr: NalHeader, pts: i64) -> Result<()> {
        let rbsp = unescape_rbsp(&nal[2..]);
        // a first_slice_segment_in_pic_flag of 1 starts a new picture
        let first = rbsp.first().is_some_and(|b| b & 0x80 != 0);
        if first {
            self.submit_pending();
        }
        if hdr.is_rasl() && (self.skip_rasl || self.first_picture) {
            if first {
                self.stat(|s| s.skipped_rasl += 1);
            }
            return Ok(());
        }
        let ppss = &self.ppss;
        let spss = &self.spss;
        let prev = self.pending.as_ref().map(|p| &p.last_sh);
        let (sh, pps, sps) = SliceHeader::parse(
            &rbsp,
            hdr,
            |id| {
                let pps = ppss[id as usize].as_ref().ok_or_else(|| Error::MissingParameterSet(format!("PPS {id}")))?;
                let sps = spss[pps.sps_id as usize].as_ref().ok_or_else(|| Error::MissingParameterSet(format!("SPS {}", pps.sps_id)))?;
                Ok((pps.clone(), sps.clone()))
            },
            prev,
        )?;
        sps.check_supported()?;
        pps.check_supported()?;
        if first {
            self.start_picture(&sh, &sps, &pps, pts)?;
        } else {
            let Some(p) = &self.pending else { return invalid("slice segment without the first slice of its picture") };
            ensure!(Arc::ptr_eq(&p.pps, &pps), "PPS changed within a picture");
        }
        let Some(pending) = self.pending.as_mut() else {
            return invalid("slice segment without the first slice of its picture");
        };
        let refs = build_ref_lists(&sh, &pending.rps)?;
        if !sh.dependent {
            pending.last_sh = sh.clone();
        }
        let (st, dependent, weighted, tmvp, sao, dbk_off, lt) =
            (sh.slice_type, sh.dependent, sh.pwt.is_some(), sh.temporal_mvp, sh.sao_luma || sh.sao_chroma, sh.deblocking_disabled, !sh.lt.is_empty());
        let nal = if self.accel.is_some() { nal.to_vec() } else { Vec::new() };
        pending.slices.push(SliceJob { sh, pps, sps, rbsp, nal, refs });
        self.stat(|s| {
            match st {
                crate::slice::SliceType::I => s.slices_i += 1,
                crate::slice::SliceType::P => s.slices_p += 1,
                crate::slice::SliceType::B => s.slices_b += 1,
            }
            s.dependent_slices += dependent as u64;
            s.weighted_slices += weighted as u64;
            s.tmvp_slices += tmvp as u64;
            s.sao_slices += sao as u64;
            s.deblock_disabled_slices += dbk_off as u64;
            s.long_term_slices += lt as u64;
        });
        Ok(())
    }

    fn start_picture(&mut self, sh: &SliceHeader, sps: &Arc<Sps>, pps: &Arc<Pps>, pts: i64) -> Result<()> {
        ensure!(sh.first_slice_segment_in_pic && !sh.dependent, "invalid first slice segment");
        let hdr = sh.nal;
        let changed = match &self.active_sps {
            None => true,
            Some(a) => {
                a.width != sps.width
                    || a.height != sps.height
                    || a.bit_depth_luma != sps.bit_depth_luma
                    || a.bit_depth_chroma != sps.bit_depth_chroma
                    || a.log2_ctb != sps.log2_ctb
            }
        };
        let mut outs = Vec::new();
        if changed && self.active_sps.is_some() {
            self.dpb.flush(&mut outs);
        }
        self.active_sps = Some(sps.clone());
        self.dpb.max_dec_pic_buffering = sps.max_dec_pic_buffering as usize;
        self.dpb.max_num_reorder = sps.max_num_reorder as usize;
        self.dpb.max_latency = if sps.max_latency_increase_plus1 != 0 { sps.max_num_reorder + sps.max_latency_increase_plus1 - 1 } else { 0 };
        let irap_no_rasl = hdr.is_irap() && (hdr.is_idr() || hdr.is_bla() || self.first_picture || self.after_eos);
        if hdr.is_irap() {
            self.skip_rasl = irap_no_rasl;
            self.stat(|s| {
                if hdr.is_idr() {
                    s.idr += 1;
                } else {
                    s.cra += 1;
                }
            });
        }
        let max_lsb = sps.max_poc_lsb();
        let poc = self.poc_state.compute(sh, max_lsb, irap_no_rasl);
        let (w, h) = (sps.width as usize, sps.height as usize);
        let template = Frame::new(0, 0, w, h, sps.log2_ctb, sps.bit_depth_luma, sps.bit_depth_chroma);
        let next_id = &mut self.next_id;
        let mut missing = 0u64;
        let mut make_missing = |p: i32| -> FrameRef {
            missing += 1;
            let id = *next_id;
            *next_id += 1;
            Arc::new(Frame::gray(id, p, &template))
        };
        let rps = self.dpb.apply_rps(sh, poc, max_lsb, irap_no_rasl, &mut make_missing)?;
        if missing > 0 && self.accel.is_some() {
            return unsupported("missing reference pictures with hardware decoding");
        }
        if missing > 0 {
            self.stat(|s| s.missing_refs += missing);
        }
        if irap_no_rasl && !self.first_picture {
            if sh.no_output_of_prior_pics && hdr.is_idr() {
                self.dpb.clear();
            } else {
                self.dpb.flush(&mut outs);
            }
        } else {
            self.dpb.bump_before_decode(&mut outs);
        }
        self.out_queue.extend(outs);
        let (layout, scaling) = self.layout_for(pps, sps)?;
        let _ = scaling;
        let id = self.next_id;
        self.next_id += 1;
        let frame = Arc::new(Frame::new(id, poc, w, h, sps.log2_ctb, sps.bit_depth_luma, sps.bit_depth_chroma));
        let output = sh.pic_output && !(hdr.is_rasl() && self.skip_rasl);
        // Draft mode: a sub-layer non-reference picture of the highest sub-layer is never used
        // for prediction, so its in-loop filters can be left out without changing anything else.
        let draft = self.draft && hdr.is_sub_layer_non_ref() && hdr.temporal_id as u32 >= sps.max_sub_layers_minus1;
        if draft {
            self.stat(|s| s.draft_pictures += 1);
        }
        let meta = Arc::new(output_meta(sps, pts, hdr.is_irap(), draft));
        self.pending = Some(PendingPic {
            frame,
            sps: sps.clone(),
            pps: pps.clone(),
            layout,
            last_sh: sh.clone(),
            first_sh: sh.clone(),
            rps,
            poc,
            output,
            meta,
            slices: Vec::new(),
        });
        self.first_picture = false;
        self.after_eos = false;
        self.stat(|s| {
            s.tiles_pictures += pps.tiles_enabled as u64;
            s.wpp_pictures += pps.entropy_coding_sync as u64;
            s.scaling_list_pictures += sps.scaling_list_enabled as u64;
            s.transform_skip_pictures += pps.transform_skip as u64;
        });
        Ok(())
    }

    /// Start decoding the pending picture and insert it into the DPB.
    fn submit_pending(&mut self) {
        let Some(p) = self.pending.take() else { return };
        let PendingPic { frame, sps, pps, layout, first_sh, poc, output, meta, slices, rps, .. } = p;
        if self.accel.is_some() {
            if let Err(e) = self.accelerate(&frame, &first_sh, &sps, &pps, poc, &rps, &slices)
                && let Ok(mut err) = self.shared.error.lock()
            {
                err.get_or_insert(Error::Invalid(format!("hardware decoding: {e}")));
            }
        } else {
            let scaling = self.layout_cache.iter().find(|(pp, ss, _, _)| Arc::ptr_eq(pp, &pps) && Arc::ptr_eq(ss, &sps)).and_then(|(_, _, _, s)| s.clone());
            self.dispatch(frame.clone(), sps, pps, layout, scaling, slices, meta.draft);
        }
        self.poc_state.update(&first_sh, poc);
        let mut outs = Vec::new();
        self.dpb.insert(frame, poc, output, meta, &mut outs);
        self.out_queue.extend(outs);
        if let Some(a) = self.accel.as_mut() {
            let live: Vec<u32> = self.dpb.entries.iter().map(|e| e.frame.id).chain(self.out_queue.iter().map(|o| o.frame.id)).collect();
            a.retain(&live);
        }
    }

    /// Hand one picture to the accelerator.
    #[allow(clippy::too_many_arguments)]
    fn accelerate(
        &mut self,
        frame: &FrameRef,
        first: &SliceHeader,
        sps: &Arc<Sps>,
        pps: &Arc<Pps>,
        poc: i32,
        rps: &RefPicSet,
        slices: &[SliceJob],
    ) -> std::result::Result<(), String> {
        let r = |x: &crate::picture::RefPic| AccelRef { id: x.frame.id, poc: x.poc, long_term: x.long_term };
        let refs: Vec<AccelRef> = self
            .dpb
            .entries
            .iter()
            .filter(|e| e.marking != crate::dpb::Marking::Unused && e.frame.id != frame.id)
            .map(|e| AccelRef { id: e.frame.id, poc: e.poc, long_term: e.marking == crate::dpb::Marking::Long })
            .collect();
        let accel_slices: Vec<AccelSlice> = slices
            .iter()
            .map(|s| AccelSlice {
                header: &s.sh,
                nal: &s.nal,
                data_byte_offset: 2 + s.sh.data_offset,
                header_emulation_bytes: crate::accel::emulation_bytes_before(s.nal.get(2..).unwrap_or(&[]), s.sh.data_offset),
                lists: [0, 1].map(|l| s.refs[l].iter().map(r).collect()),
            })
            .collect();
        let pic = AccelPicture {
            id: frame.id,
            poc,
            sps,
            pps,
            irap: first.nal.is_irap(),
            idr: first.nal.is_idr(),
            intra: slices.iter().all(|s| s.sh.slice_type == crate::slice::SliceType::I),
            st_curr_before: rps.st_curr_before.iter().map(r).collect(),
            st_curr_after: rps.st_curr_after.iter().map(r).collect(),
            lt_curr: rps.lt_curr.iter().map(r).collect(),
            refs,
            slices: accel_slices,
        };
        let Some(a) = self.accel.as_mut() else { return Err("no accelerator".into()) };
        a.decode_picture(&pic)
    }

    #[allow(clippy::too_many_arguments)]
    fn dispatch(
        &mut self,
        frame: FrameRef,
        sps: Arc<Sps>,
        pps: Arc<Pps>,
        layout: Arc<Layout>,
        scaling: Option<Arc<Vec<Vec<u8>>>>,
        slices: Vec<SliceJob>,
        draft: bool,
    ) {
        let shared = self.shared.clone();
        #[cfg(feature = "threads")]
        if let Some(pool) = &self.pool {
            self.in_flight.retain(|f| !f.is_complete());
            while self.in_flight.len() >= self.max_in_flight {
                if let Some(f) = self.in_flight.pop_front() {
                    f.wait_complete();
                }
            }
            self.in_flight.push_back(frame.clone());
            pool.spawn_fifo(move || run_job(frame, sps, pps, layout, scaling, slices, draft, &shared));
            return;
        }
        run_job(frame, sps, pps, layout, scaling, slices, draft, &shared);
    }
}

/// Decode all slices of one picture and publish its rows.
#[allow(clippy::too_many_arguments)]
fn run_job(
    frame: FrameRef,
    sps: Arc<Sps>,
    pps: Arc<Pps>,
    layout: Arc<Layout>,
    scaling: Option<Arc<Vec<Vec<u8>>>>,
    slices: Vec<SliceJob>,
    draft: bool,
    shared: &Shared,
) {
    let mut pic = PicState::new(frame.clone(), sps, pps, layout, scaling);
    pic.draft = draft;
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let mut first_err = None;
        for s in &slices {
            if let Err(e) = SliceDecoder::decode(&mut pic, s) {
                first_err.get_or_insert(e);
            }
        }
        crate::filter::finish(&mut pic);
        first_err
    }));
    let err = match result {
        Ok(e) => e,
        Err(_) => {
            // make sure readers never block on this frame
            let gray = Frame::gray(0, frame.poc, &frame);
            for r in 0..frame.num_rows() {
                if !frame.is_published(r) {
                    let row = gray.row(r);
                    frame.publish(r, crate::picture::FrameRow { y: row.y.clone(), cb: row.cb.clone(), cr: row.cr.clone(), col: row.col.clone() });
                }
            }
            Some(Error::Invalid("internal error while decoding a picture".into()))
        }
    };
    if let Ok(mut s) = shared.stats.lock() {
        s.pictures += 1;
        for b in &pic.blk {
            if b.flags & F_INTRA != 0 {
                s.intra_4x4_blocks += 1;
            } else if b.flags & F_SKIP != 0 {
                s.skip_4x4_blocks += 1;
            } else if b.flags != 0 {
                s.inter_4x4_blocks += 1;
            }
        }
    }
    if let (Some(e), Ok(mut slot)) = (err, shared.error.lock()) {
        slot.get_or_insert(e);
    }
}

fn output_meta(sps: &Sps, pts: i64, key: bool, draft: bool) -> OutputMeta {
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
        bit_depth: sps.bit_depth_luma,
        draft,
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
        bit_depth: m.bit_depth,
    }
}

fn make_picture(o: &Output) -> Picture {
    let (cx, cy, cw, ch) = o.meta.crop;
    let (cx, cy, cw, ch) = (cx as usize, cy as usize, cw as usize, ch as usize);
    let (y, u, v) = o.frame.copy_cropped((cx, cy, cw, ch));
    let (ccw, cch) = (cw.div_ceil(2), ch.div_ceil(2));
    let bd = o.meta.bit_depth;
    let conv = |p: Vec<u16>| if bd <= 8 { Plane::U8(p.into_iter().map(|v| v as u8).collect()) } else { Plane::U16(p) };
    Picture {
        width: cw as u32,
        height: ch as u32,
        chroma_width: ccw as u32,
        chroma_height: cch as u32,
        bit_depth: bd,
        y: conv(y),
        u: conv(u),
        v: conv(v),
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

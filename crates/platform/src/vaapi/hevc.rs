//! The HEVC side of the VA-API decoder, like [`super::h264`]: the software decoder's own parsers
//! (`filmcraft_hevc::params`, `slice`) and decoded picture buffer (`filmcraft_hevc::dpb`: POC,
//! reference picture sets, reference lists, output order) drive a stateless hardware decoder, so
//! both decoders make the same decisions and output the same pictures in the same order, RASL
//! pictures of a CRA the run starts at left out as in software.
//!
//! What the hardware path does not take is an error, so the hybrid decoder replays the run in
//! software: missing reference pictures (the software decoder conceals them), more than 15
//! reference frames, stream changes the surfaces were not made for, range extensions.
//!
//! Safe code; compiled on every target so its logic is tested everywhere.

use std::sync::Arc;

use filmcraft_bitstream::{length_prefixed_nals, unescape_rbsp};
use filmcraft_codecs::{CodecError, DecodedFrame, Result};
use filmcraft_hevc::dpb::{Dpb, Marking, Output, OutputMeta, PocState, Ref, RefPicSet, build_ref_lists};
use filmcraft_hevc::params::{Layout, Pps, ScalingList, Sps};
use filmcraft_hevc::slice::{NalHeader, SliceHeader, SliceType, nal_type};

use super::Accel;
use super::ffi::{
    self, VAIQMatrixBufferHEVC, VAPictureHEVC, VAPictureParameterBufferHEVC, VASliceParameterBufferHEVC, hevc_pic_bits, hevc_slice_bits,
    hevc_slice_parsing_bits,
};

/// A picture in the DPB: its surface.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Surface {
    pub index: usize,
}

/// A reference picture that was never decoded (8.3.3): not handed to the hardware.
const MISSING: usize = usize::MAX;

/// The picture whose slice segments are being collected.
struct Pending {
    target: usize,
    sps: Arc<Sps>,
    pps: Arc<Pps>,
    first: SliceHeader,
    /// The latest independent slice segment's header (dependent ones continue it).
    last: SliceHeader,
    rps: RefPicSet<Surface>,
    /// The surfaces of `pic.ReferenceFrames`, in order.
    ref_frames: Vec<usize>,
    pic: VAPictureParameterBufferHEVC,
    poc: i32,
    output: bool,
    meta: Arc<OutputMeta>,
    slices: Vec<(VASliceParameterBufferHEVC, Vec<u8>)>,
    all_intra: bool,
    any_b: bool,
}

/// HEVC front end of a stateless hardware decoder (see the module docs).
pub struct Front<A: Accel> {
    accel: A,
    length_size: usize,
    /// Coded size (luma samples) and bit depth the surfaces were made for.
    size: (u32, u32),
    bits: u32,
    spss: Vec<Option<Arc<Sps>>>,
    ppss: Vec<Option<Arc<Pps>>>,
    dpb: Dpb<Surface>,
    poc_state: PocState,
    active_sps: Option<Arc<Sps>>,
    pending: Option<Pending>,
    first_picture: bool,
    skip_rasl: bool,
    after_eos: bool,
}

fn decode_err(e: impl std::fmt::Display) -> CodecError {
    CodecError::Decode(e.to_string())
}

fn unsupported(what: &str) -> CodecError {
    CodecError::Decode(format!("the hardware decoder does not take {what}"))
}

impl<A: Accel> Front<A> {
    /// A front end for samples with `length_size`-byte NAL length prefixes whose sample entry
    /// carried `parameter_sets` (VPS / SPS / PPS NAL units), decoding into `accel`'s surfaces of
    /// `size` luma samples and `bits` bits.
    pub fn new(accel: A, length_size: usize, size: (u32, u32), bits: u32, parameter_sets: &[Vec<u8>]) -> Result<Self> {
        let mut f = Self {
            accel,
            length_size,
            size,
            bits,
            spss: vec![None; 16],
            ppss: vec![None; 64],
            dpb: Dpb::default(),
            poc_state: PocState::default(),
            active_sps: None,
            pending: None,
            first_picture: true,
            skip_rasl: false,
            after_eos: false,
        };
        let mut frames = Vec::new();
        for nal in parameter_sets {
            f.handle_nal(nal, 0, &mut frames)?;
        }
        Ok(f)
    }

    pub fn accel(&self) -> &A {
        &self.accel
    }

    /// Decode one access unit; pictures that left the DPB come back in output order.
    pub fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let nals = length_prefixed_nals(sample, self.length_size).map_err(decode_err)?;
        let mut frames = Vec::new();
        for nal in nals {
            self.handle_nal(nal, pts, &mut frames)?;
        }
        self.submit_pending(&mut frames)?;
        Ok(frames)
    }

    /// Output every picture still held for reordering; what follows starts like a new stream
    /// (a CRA's RASL pictures are left out), as in software.
    pub fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        let mut frames = Vec::new();
        self.submit_pending(&mut frames)?;
        let mut outs = Vec::new();
        self.dpb.flush(&mut outs);
        self.read_out(outs, &mut frames)?;
        self.first_picture = true;
        Ok(frames)
    }

    /// Forget every picture (after a seek).
    pub fn reset(&mut self) {
        self.dpb = Dpb::default();
        self.poc_state = PocState::default();
        self.active_sps = None;
        self.pending = None;
        self.first_picture = true;
        self.skip_rasl = false;
        self.after_eos = false;
    }

    /// Read back the pictures the DPB output, right away: a surface leaving the DPB is reused.
    fn read_out(&mut self, outs: Vec<Output<Surface>>, frames: &mut Vec<DecodedFrame>) -> Result<()> {
        for o in outs {
            let frame = self.accel.read(o.frame.index).map_err(decode_err)?;
            frames.push(DecodedFrame { pts: o.meta.pts, frame, draft: false });
        }
        Ok(())
    }

    fn handle_nal(&mut self, nal: &[u8], pts: i64, frames: &mut Vec<DecodedFrame>) -> Result<()> {
        if nal.len() < 2 {
            return Ok(());
        }
        let hdr = NalHeader::parse(nal).map_err(decode_err)?;
        if hdr.layer_id != 0 {
            return Ok(());
        }
        let rbsp = || unescape_rbsp(nal.get(2..).unwrap_or_default());
        match hdr.nal_type {
            nal_type::SPS => {
                let sps = Sps::parse(&rbsp()).map_err(decode_err)?;
                let slot = self.spss.get_mut(sps.id as usize).ok_or_else(|| decode_err("SPS id out of range"))?;
                *slot = Some(Arc::new(sps));
            }
            nal_type::PPS => {
                let pps = Pps::parse(&rbsp()).map_err(decode_err)?;
                let slot = self.ppss.get_mut(pps.id as usize).ok_or_else(|| decode_err("PPS id out of range"))?;
                *slot = Some(Arc::new(pps));
            }
            nal_type::EOS | nal_type::EOB => {
                self.submit_pending(frames)?;
                self.after_eos = true;
            }
            0..=9 | 16..=21 => self.handle_slice(nal, hdr, pts, frames)?,
            // VPS, AUD, SEI, filler, reserved: nothing to decode
            _ => {}
        }
        Ok(())
    }

    fn handle_slice(&mut self, nal: &[u8], hdr: NalHeader, pts: i64, frames: &mut Vec<DecodedFrame>) -> Result<()> {
        let rbsp = unescape_rbsp(nal.get(2..).unwrap_or_default());
        let first = rbsp.first().is_some_and(|b| b & 0x80 != 0);
        if first {
            self.submit_pending(frames)?;
        }
        if hdr.is_rasl() && (self.skip_rasl || self.first_picture) {
            return Ok(());
        }
        let (ppss, spss) = (&self.ppss, &self.spss);
        let prev = self.pending.as_ref().map(|p| &p.last);
        let (sh, pps, sps) = SliceHeader::parse(
            &rbsp,
            hdr,
            |id| {
                let missing = |what: String| filmcraft_hevc::Error::MissingParameterSet(what);
                let pps = ppss.get(id as usize).and_then(Option::as_ref).ok_or_else(|| missing(format!("PPS {id}")))?;
                let sps = spss.get(pps.sps_id as usize).and_then(Option::as_ref).ok_or_else(|| missing(format!("SPS {}", pps.sps_id)))?;
                Ok((pps.clone(), sps.clone()))
            },
            prev,
        )
        .map_err(decode_err)?;
        sps.check_supported().map_err(decode_err)?;
        pps.check_supported().map_err(decode_err)?;
        if first {
            self.start_picture(&sh, &sps, &pps, pts, frames)?;
        }
        let Some(p) = self.pending.as_mut() else { return Err(decode_err("slice segment without the first slice of its picture")) };
        if !Arc::ptr_eq(&p.pps, &pps) {
            return Err(decode_err("PPS changed within a picture"));
        }
        let refs = build_ref_lists(&sh, &p.rps).map_err(decode_err)?;
        let params = slice_params(&sh, nal, &refs, &p.ref_frames)?;
        if !sh.dependent {
            p.last = sh.clone();
        }
        p.all_intra &= sh.is_intra();
        p.any_b |= sh.is_b();
        p.slices.push((params, nal.to_vec()));
        Ok(())
    }

    fn start_picture(&mut self, sh: &SliceHeader, sps: &Arc<Sps>, pps: &Arc<Pps>, pts: i64, frames: &mut Vec<DecodedFrame>) -> Result<()> {
        if !sh.first_slice_segment_in_pic || sh.dependent {
            return Err(decode_err("invalid first slice segment"));
        }
        if (sps.width, sps.height) != self.size || sps.bit_depth_luma != self.bits || sps.bit_depth_chroma != self.bits || sps.chroma_format_idc != 1 {
            return Err(unsupported("a picture size or format change"));
        }
        if sps.max_dec_pic_buffering as usize >= self.accel.surfaces().len() {
            return Err(unsupported("a DPB larger than the decoder's surfaces"));
        }
        let hdr = sh.nal;
        let changed = self.active_sps.as_ref().is_some_and(|a| a.log2_ctb != sps.log2_ctb);
        let mut outs = Vec::new();
        if changed {
            self.dpb.flush(&mut outs);
        }
        self.active_sps = Some(sps.clone());
        self.dpb.max_dec_pic_buffering = sps.max_dec_pic_buffering as usize;
        self.dpb.max_num_reorder = sps.max_num_reorder as usize;
        self.dpb.max_latency = if sps.max_latency_increase_plus1 != 0 { sps.max_num_reorder + sps.max_latency_increase_plus1 - 1 } else { 0 };
        let irap_no_rasl = hdr.is_irap() && (hdr.is_idr() || hdr.is_bla() || self.first_picture || self.after_eos);
        if hdr.is_irap() {
            self.skip_rasl = irap_no_rasl;
        }
        let max_lsb = sps.max_poc_lsb();
        let poc = self.poc_state.compute(sh, max_lsb, irap_no_rasl);
        let mut make_missing = |_| Surface { index: MISSING };
        let rps = self.dpb.apply_rps(sh, poc, max_lsb, irap_no_rasl, &mut make_missing).map_err(decode_err)?;
        if [&rps.st_curr_before, &rps.st_curr_after, &rps.lt_curr].iter().any(|l| l.iter().any(|r| r.frame.index == MISSING)) {
            // the software decoder conceals them with mid-gray pictures
            return Err(unsupported("missing reference pictures"));
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
        self.read_out(outs, frames)?;
        let surfaces = self.accel.surfaces();
        let target = (0..surfaces.len()).find(|&i| !self.dpb.entries.iter().any(|e| e.frame.index == i)).ok_or_else(|| decode_err("no free surface"))?;
        let target_id = surfaces.get(target).copied().ok_or_else(|| decode_err("surface out of range"))?;
        let mut pic = picture_params(sps, pps, sh)?;
        pic.CurrPic = VAPictureHEVC { picture_id: target_id, pic_order_cnt: poc, flags: 0, va_reserved: [0; ffi::VA_PADDING_LOW] };
        // the reference frames: every picture the RPS kept, flagged with its part of the RPS
        let rps_flag = |index: usize| {
            let in_list = |l: &[Ref<Surface>]| l.iter().any(|r| r.frame.index == index);
            if in_list(&rps.st_curr_before) {
                ffi::VA_PICTURE_HEVC_RPS_ST_CURR_BEFORE
            } else if in_list(&rps.st_curr_after) {
                ffi::VA_PICTURE_HEVC_RPS_ST_CURR_AFTER
            } else if in_list(&rps.lt_curr) {
                ffi::VA_PICTURE_HEVC_RPS_LT_CURR
            } else {
                0
            }
        };
        let refs: Vec<_> = self.dpb.entries.iter().filter(|e| e.marking != Marking::Unused).collect();
        if refs.len() > pic.ReferenceFrames.len() {
            return Err(unsupported("more than 15 reference frames"));
        }
        let mut ref_frames = Vec::with_capacity(refs.len());
        for (slot, e) in pic.ReferenceFrames.iter_mut().zip(&refs) {
            let long = if e.marking == Marking::Long { ffi::VA_PICTURE_HEVC_LONG_TERM_REFERENCE } else { 0 };
            let picture_id = surfaces.get(e.frame.index).copied().ok_or_else(|| decode_err("surface out of range"))?;
            *slot = VAPictureHEVC { picture_id, pic_order_cnt: e.poc, flags: long | rps_flag(e.frame.index), va_reserved: [0; ffi::VA_PADDING_LOW] };
            ref_frames.push(e.frame.index);
        }
        let output = sh.pic_output && !(hdr.is_rasl() && self.skip_rasl);
        let meta = Arc::new(output_meta(sps, pts, hdr.is_irap()));
        self.pending = Some(Pending {
            target,
            sps: sps.clone(),
            pps: pps.clone(),
            first: sh.clone(),
            last: sh.clone(),
            rps,
            ref_frames,
            pic,
            poc,
            output,
            meta,
            slices: Vec::new(),
            all_intra: true,
            any_b: false,
        });
        self.first_picture = false;
        self.after_eos = false;
        Ok(())
    }

    /// Decode the collected picture, then insert it into the DPB.
    fn submit_pending(&mut self, frames: &mut Vec<DecodedFrame>) -> Result<()> {
        let Some(mut p) = self.pending.take() else { return Ok(()) };
        let Some(last) = p.slices.last_mut() else { return Err(decode_err("a picture without slices")) };
        last.0.LongSliceFlags |= 1 << hevc_slice_bits::LAST_SLICE_OF_PIC;
        p.pic.slice_parsing_fields |= u32::from(p.all_intra) << hevc_slice_parsing_bits::INTRA_PIC;
        p.pic.pic_fields |= u32::from(!p.any_b) << hevc_pic_bits::NO_BI_PRED;
        let iq = if p.sps.scaling_list_enabled { Some(iq_matrix(p.pps.scaling_list.as_ref().or(p.sps.scaling_list.as_ref()))) } else { None };
        self.accel.decode_hevc(p.target, &p.pic, iq.as_ref(), &p.slices).map_err(decode_err)?;
        self.poc_state.update(&p.first, p.poc);
        let mut outs = Vec::new();
        self.dpb.insert(Surface { index: p.target }, p.poc, p.output, p.meta, &mut outs);
        self.read_out(outs, frames)
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
        bit_depth: sps.bit_depth_luma,
        draft: false,
    }
}

fn flag(on: bool, bit: u32) -> u32 {
    u32::from(on) << bit
}

fn u8_of(v: u32) -> u8 {
    u8::try_from(v).unwrap_or(u8::MAX)
}

fn i8_of(v: i32) -> i8 {
    i8::try_from(v).unwrap_or(0)
}

/// The picture parameters of a picture coded with `sps` / `pps` whose first slice segment is `sh`
/// (the current and reference pictures, `IntraPicFlag` and `NoBiPredFlag` are filled in by the caller).
pub fn picture_params(sps: &Sps, pps: &Pps, sh: &SliceHeader) -> Result<VAPictureParameterBufferHEVC> {
    use hevc_pic_bits as p;
    use hevc_slice_parsing_bits as s;
    let pic_fields = (sps.chroma_format_idc & 3) << p::CHROMA_FORMAT_IDC
        | flag(sps.separate_colour_plane, p::SEPARATE_COLOUR_PLANE)
        | flag(sps.pcm, p::PCM_ENABLED)
        | flag(sps.scaling_list_enabled, p::SCALING_LIST_ENABLED)
        | flag(pps.transform_skip, p::TRANSFORM_SKIP_ENABLED)
        | flag(sps.amp, p::AMP_ENABLED)
        | flag(sps.strong_intra_smoothing, p::STRONG_INTRA_SMOOTHING_ENABLED)
        | flag(pps.sign_data_hiding, p::SIGN_DATA_HIDING_ENABLED)
        | flag(pps.constrained_intra_pred, p::CONSTRAINED_INTRA_PRED)
        | flag(pps.cu_qp_delta_enabled, p::CU_QP_DELTA_ENABLED)
        | flag(pps.weighted_pred, p::WEIGHTED_PRED)
        | flag(pps.weighted_bipred, p::WEIGHTED_BIPRED)
        | flag(pps.transquant_bypass, p::TRANSQUANT_BYPASS_ENABLED)
        | flag(pps.tiles_enabled, p::TILES_ENABLED)
        | flag(pps.entropy_coding_sync, p::ENTROPY_CODING_SYNC_ENABLED)
        | flag(pps.loop_filter_across_slices, p::PPS_LOOP_FILTER_ACROSS_SLICES_ENABLED)
        | flag(pps.loop_filter_across_tiles, p::LOOP_FILTER_ACROSS_TILES_ENABLED)
        | flag(sps.pcm_loop_filter_disabled, p::PCM_LOOP_FILTER_DISABLED)
        | flag(sps.max_num_reorder == 0, p::NO_PIC_REORDERING);
    let slice_parsing_fields = flag(pps.lists_modification_present, s::LISTS_MODIFICATION_PRESENT)
        | flag(sps.long_term_refs_present, s::LONG_TERM_REF_PICS_PRESENT)
        | flag(sps.temporal_mvp, s::SPS_TEMPORAL_MVP_ENABLED)
        | flag(pps.cabac_init_present, s::CABAC_INIT_PRESENT)
        | flag(pps.output_flag_present, s::OUTPUT_FLAG_PRESENT)
        | flag(pps.dependent_slice_segments_enabled, s::DEPENDENT_SLICE_SEGMENTS_ENABLED)
        | flag(pps.slice_chroma_qp_offsets_present, s::PPS_SLICE_CHROMA_QP_OFFSETS_PRESENT)
        | flag(sps.sao, s::SAMPLE_ADAPTIVE_OFFSET_ENABLED)
        | flag(pps.deblocking_override_enabled, s::DEBLOCKING_FILTER_OVERRIDE_ENABLED)
        | flag(pps.deblocking_disabled, s::PPS_DISABLE_DEBLOCKING_FILTER)
        | flag(pps.slice_header_extension_present, s::SLICE_SEGMENT_HEADER_EXTENSION_PRESENT)
        | flag(sh.nal.is_irap(), s::RAP_PIC)
        | flag(sh.nal.is_idr(), s::IDR_PIC);
    // tile sizes in CTBs, uniform spacing resolved
    let layout = Layout::new(sps, pps).map_err(decode_err)?;
    let mut column_width_minus1 = [0u16; 19];
    let mut row_height_minus1 = [0u16; 21];
    if pps.tiles_enabled {
        let sizes = |bd: &[u32], out: &mut [u16]| -> Result<()> {
            let n = bd.len().saturating_sub(1);
            if n > out.len() {
                return Err(unsupported("that many tiles"));
            }
            for (o, w) in out.iter_mut().zip(bd.windows(2)) {
                *o = u16::try_from(w[1].saturating_sub(w[0]).saturating_sub(1)).unwrap_or(0);
            }
            Ok(())
        };
        sizes(&layout.col_bd, &mut column_width_minus1)?;
        sizes(&layout.row_bd, &mut row_height_minus1)?;
    }
    let narrow = |v: u32| u16::try_from(v).map_err(|_| decode_err("picture too large"));
    Ok(VAPictureParameterBufferHEVC {
        CurrPic: VAPictureHEVC::INVALID,
        ReferenceFrames: [VAPictureHEVC::INVALID; 15],
        pic_width_in_luma_samples: narrow(sps.width)?,
        pic_height_in_luma_samples: narrow(sps.height)?,
        pic_fields,
        sps_max_dec_pic_buffering_minus1: u8_of(sps.max_dec_pic_buffering.saturating_sub(1)),
        bit_depth_luma_minus8: u8_of(sps.bit_depth_luma.saturating_sub(8)),
        bit_depth_chroma_minus8: u8_of(sps.bit_depth_chroma.saturating_sub(8)),
        pcm_sample_bit_depth_luma_minus1: u8_of(sps.pcm_bit_depth_luma.saturating_sub(1)),
        pcm_sample_bit_depth_chroma_minus1: u8_of(sps.pcm_bit_depth_chroma.saturating_sub(1)),
        log2_min_luma_coding_block_size_minus3: u8_of(sps.log2_min_cb.saturating_sub(3)),
        log2_diff_max_min_luma_coding_block_size: u8_of(sps.log2_ctb.saturating_sub(sps.log2_min_cb)),
        log2_min_transform_block_size_minus2: u8_of(sps.log2_min_tb.saturating_sub(2)),
        log2_diff_max_min_transform_block_size: u8_of(sps.log2_max_tb.saturating_sub(sps.log2_min_tb)),
        log2_min_pcm_luma_coding_block_size_minus3: u8_of(sps.log2_min_pcm.saturating_sub(3)),
        log2_diff_max_min_pcm_luma_coding_block_size: u8_of(sps.log2_max_pcm.saturating_sub(sps.log2_min_pcm)),
        max_transform_hierarchy_depth_intra: u8_of(sps.max_th_depth_intra),
        max_transform_hierarchy_depth_inter: u8_of(sps.max_th_depth_inter),
        init_qp_minus26: i8_of(pps.init_qp - 26),
        diff_cu_qp_delta_depth: u8_of(pps.diff_cu_qp_delta_depth),
        pps_cb_qp_offset: i8_of(pps.cb_qp_offset),
        pps_cr_qp_offset: i8_of(pps.cr_qp_offset),
        log2_parallel_merge_level_minus2: u8_of(pps.log2_parallel_merge_level.saturating_sub(2)),
        num_tile_columns_minus1: u8_of(pps.num_tile_columns.saturating_sub(1)),
        num_tile_rows_minus1: u8_of(pps.num_tile_rows.saturating_sub(1)),
        column_width_minus1,
        row_height_minus1,
        slice_parsing_fields,
        log2_max_pic_order_cnt_lsb_minus4: u8_of(sps.log2_max_poc_lsb.saturating_sub(4)),
        num_short_term_ref_pic_sets: u8_of(u32::try_from(sps.st_rps.len()).unwrap_or(u32::MAX)),
        num_long_term_ref_pic_sps: u8_of(u32::try_from(sps.lt_ref_pics.len()).unwrap_or(u32::MAX)),
        num_ref_idx_l0_default_active_minus1: u8_of(pps.num_ref_idx_l0_default.saturating_sub(1)),
        num_ref_idx_l1_default_active_minus1: u8_of(pps.num_ref_idx_l1_default.saturating_sub(1)),
        pps_beta_offset_div2: i8_of(pps.beta_offset_div2),
        pps_tc_offset_div2: i8_of(pps.tc_offset_div2),
        num_extra_slice_header_bits: u8_of(pps.num_extra_slice_header_bits),
        st_rps_bits: u32::try_from(sh.st_rps_bits).unwrap_or(0),
        va_reserved: [0; ffi::VA_PADDING_MEDIUM],
    })
}

/// The 8x8 base list `coded` (up-right diagonal order, as coded and stored) in raster order.
fn raster8(list: &ScalingList, size_id: usize, matrix_id: usize) -> [u8; 64] {
    let mut l = list.clone();
    let row = |s: usize| l.lists.get(s).and_then(|m| m.get(matrix_id)).copied();
    if let Some(src) = row(size_id)
        && let Some(dst) = l.lists.get_mut(1).and_then(|m| m.get_mut(matrix_id))
    {
        *dst = src;
    }
    let mut out = [16u8; 64];
    for (o, v) in out.iter_mut().zip(l.factor(1, matrix_id)) {
        *o = v;
    }
    out
}

/// The scaling lists in force (`list`, or the default lists), in raster order.
pub fn iq_matrix(list: Option<&ScalingList>) -> VAIQMatrixBufferHEVC {
    let defaults = ScalingList::default_lists();
    let l = list.unwrap_or(&defaults);
    let mut iq = VAIQMatrixBufferHEVC {
        ScalingList4x4: [[16; 16]; 6],
        ScalingList8x8: [[16; 64]; 6],
        ScalingList16x16: [[16; 64]; 6],
        ScalingList32x32: [[16; 64]; 2],
        ScalingListDC16x16: [16; 6],
        ScalingListDC32x32: [16; 2],
        va_reserved: [0; ffi::VA_PADDING_LOW],
    };
    for m in 0..6 {
        for (o, v) in iq.ScalingList4x4[m].iter_mut().zip(l.factor(0, m)) {
            *o = v;
        }
        iq.ScalingList8x8[m] = raster8(l, 1, m);
        iq.ScalingList16x16[m] = raster8(l, 2, m);
        iq.ScalingListDC16x16[m] = l.dc[0][m];
    }
    // 32x32: matrixId 0 (intra) and 3 (inter)
    for (i, m) in [0, 3].into_iter().enumerate() {
        iq.ScalingList32x32[i] = raster8(l, 3, m);
        iq.ScalingListDC32x32[i] = l.dc[1][m];
    }
    iq
}

/// Emulation prevention bytes among the first `rbsp_len` RBSP bytes of a NAL unit payload.
fn emulation_bytes_before(payload: &[u8], rbsp_len: usize) -> usize {
    let (mut zeros, mut out, mut ep) = (0usize, 0usize, 0usize);
    for &b in payload {
        if out >= rbsp_len {
            break;
        }
        if zeros >= 2 && b == 3 {
            ep += 1;
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out += 1;
    }
    ep
}

/// The slice parameters of the slice segment `sh` (NAL unit `nal`) with reference lists `refs`,
/// whose pictures are `ref_frames` (the surfaces of `ReferenceFrames`, in order).
pub fn slice_params(sh: &SliceHeader, nal: &[u8], refs: &[Vec<Ref<Surface>>; 2], ref_frames: &[usize]) -> Result<VASliceParameterBufferHEVC> {
    use hevc_slice_bits as b;
    let slice_type = match sh.slice_type {
        SliceType::B => 0,
        SliceType::P => 1,
        SliceType::I => 2,
    };
    let flags = flag(sh.dependent, b::DEPENDENT_SLICE_SEGMENT)
        | slice_type << b::SLICE_TYPE
        | flag(sh.sao_luma, b::SLICE_SAO_LUMA)
        | flag(sh.sao_chroma, b::SLICE_SAO_CHROMA)
        | flag(sh.mvd_l1_zero, b::MVD_L1_ZERO)
        | flag(sh.cabac_init_flag, b::CABAC_INIT)
        | flag(sh.temporal_mvp, b::SLICE_TEMPORAL_MVP_ENABLED)
        | flag(sh.deblocking_disabled, b::SLICE_DEBLOCKING_FILTER_DISABLED)
        | flag(sh.collocated_from_l0, b::COLLOCATED_FROM_L0)
        | flag(sh.loop_filter_across_slices, b::SLICE_LOOP_FILTER_ACROSS_SLICES_ENABLED);
    let header_bytes = sh.data_offset.checked_add(2).ok_or_else(|| decode_err("slice header too long"))?;
    let ep = emulation_bytes_before(nal.get(2..).unwrap_or_default(), sh.data_offset);
    let mut s = VASliceParameterBufferHEVC {
        slice_data_size: u32::try_from(nal.len()).map_err(|_| decode_err("slice too large"))?,
        slice_data_offset: 0,
        slice_data_flag: ffi::VA_SLICE_DATA_FLAG_ALL,
        slice_data_byte_offset: u32::try_from(header_bytes).map_err(|_| decode_err("slice header too long"))?,
        slice_segment_address: sh.segment_address,
        RefPicList: [[0xFF; 15]; 2],
        LongSliceFlags: flags,
        collocated_ref_idx: u8_of(sh.collocated_ref_idx),
        num_ref_idx_l0_active_minus1: u8_of(sh.num_ref_idx[0].saturating_sub(1)),
        num_ref_idx_l1_active_minus1: u8_of(sh.num_ref_idx[1].saturating_sub(1)),
        slice_qp_delta: i8_of(sh.qp_delta),
        slice_cb_qp_offset: i8_of(sh.cb_qp_offset),
        slice_cr_qp_offset: i8_of(sh.cr_qp_offset),
        slice_beta_offset_div2: i8_of(sh.beta_offset_div2),
        slice_tc_offset_div2: i8_of(sh.tc_offset_div2),
        luma_log2_weight_denom: 0,
        delta_chroma_log2_weight_denom: 0,
        delta_luma_weight_l0: [0; 15],
        luma_offset_l0: [0; 15],
        delta_chroma_weight_l0: [[0; 2]; 15],
        ChromaOffsetL0: [[0; 2]; 15],
        delta_luma_weight_l1: [0; 15],
        luma_offset_l1: [0; 15],
        delta_chroma_weight_l1: [[0; 2]; 15],
        ChromaOffsetL1: [[0; 2]; 15],
        five_minus_max_num_merge_cand: u8_of(5u32.saturating_sub(sh.max_num_merge_cand)),
        num_entry_point_offsets: u16::try_from(sh.entry_points.len()).map_err(|_| decode_err("too many entry points"))?,
        entry_offset_to_subset_array: 0,
        slice_data_num_emu_prevn_bytes: u16::try_from(ep).unwrap_or(u16::MAX),
        va_reserved: [0; ffi::VA_PADDING_LOW - 2],
    };
    for (l, list) in refs.iter().enumerate() {
        if list.len() > 15 {
            return Err(decode_err("reference list longer than 15"));
        }
        for (i, r) in list.iter().enumerate() {
            let idx = ref_frames.iter().position(|&f| f == r.frame.index).ok_or_else(|| decode_err("a reference is not among the reference frames"))?;
            s.RefPicList[l][i] = u8::try_from(idx).unwrap_or(0xFF);
        }
    }
    if let Some(w) = &sh.pwt {
        let luma_denom = i32::try_from(w.luma_log2_denom).unwrap_or(0);
        let chroma_denom = i32::try_from(w.chroma_log2_denom).unwrap_or(0);
        s.luma_log2_weight_denom = u8_of(w.luma_log2_denom);
        s.delta_chroma_log2_weight_denom = i8_of(chroma_denom - luma_denom);
        let lists = [
            (&w.l[0], &mut s.delta_luma_weight_l0, &mut s.luma_offset_l0, &mut s.delta_chroma_weight_l0, &mut s.ChromaOffsetL0),
            (&w.l[1], &mut s.delta_luma_weight_l1, &mut s.luma_offset_l1, &mut s.delta_chroma_weight_l1, &mut s.ChromaOffsetL1),
        ];
        for (entries, lw, lo, cw, co) in lists {
            for (i, e) in entries.iter().take(15).enumerate() {
                lw[i] = i8_of(e.luma.0 - (1 << luma_denom));
                lo[i] = i8_of(e.luma.1);
                for c in 0..2 {
                    cw[i][c] = i8_of(e.chroma[c].0 - (1 << chroma_denom));
                    co[i][c] = i8_of(e.chroma[c].1);
                }
            }
        }
    }
    Ok(s)
}

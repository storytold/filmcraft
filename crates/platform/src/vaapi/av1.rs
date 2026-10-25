//! The AV1 side of the VA-API decoder, like [`super::hevc`]: the software decoder's own parser
//! (`filmcraft_av1::Parser`: OBUs, sequence and frame headers, tile payloads and the reference
//! state they depend on) drives a stateless hardware decoder, and this front end keeps the eight
//! reference slots as surfaces. AV1 has no output reordering: a shown frame is read back as soon
//! as it is decoded, a `show_existing_frame` reads its slot's surface again.
//!
//! What the hardware path does not take is an error, so the hybrid decoder replays the run in
//! software: film grain (the software decoder synthesises it), scalable streams (operating points
//! with several layers), frames of another size than the stream's (the surfaces and the read-back
//! are made for one size), references of another size (scaled motion compensation), missing
//! reference frames.
//!
//! Safe code; compiled on every target so its logic is tested everywhere.

use std::ffi::c_int;

use filmcraft_av1::header::{FrameHeader, SequenceHeader};
use filmcraft_av1::{Event, Frame, Parser};
use filmcraft_codecs::{CodecError, DecodedFrame, Result};

use super::Accel;
use super::ffi::{
    self, VADecPictureParameterBufferAV1, VASliceParameterBufferAV1, VAWarpedMotionParamsAV1, av1_lf_bits, av1_lr_bits, av1_mode_bits, av1_pic_bits,
    av1_qm_bits, av1_seg_bits, av1_seq_bits,
};

/// `primary_ref_frame` when there is none.
const PRIMARY_REF_NONE: usize = 7;
/// `frame_type` of a key frame.
const KEY_FRAME: u8 = 0;
/// Reference slots (NUM_REF_FRAMES).
const SLOTS: usize = 8;

fn decode_err(e: impl std::fmt::Display) -> CodecError {
    CodecError::Decode(e.to_string())
}

fn unsupported(what: &str) -> CodecError {
    CodecError::Decode(format!("the hardware decoder does not take {what}"))
}

/// AV1 front end of a stateless hardware decoder (see the module docs).
pub struct Front<A: Accel> {
    accel: A,
    parser: Parser,
    /// The `av1C` configuration OBUs, parsed before the first sample and after every reset.
    config_obus: Vec<u8>,
    primed: bool,
    /// The surface (index into [`Accel::surfaces`]) of each reference slot.
    slots: [Option<usize>; SLOTS],
    /// Picture size (luma samples) and bit depth the surfaces were made for.
    size: (u32, u32),
    bits: u32,
}

impl<A: Accel> Front<A> {
    /// A front end for a stream whose sample entry carried `config_obus`, decoding into `accel`'s
    /// surfaces of `size` luma samples and `bits` bits (at least nine surfaces: the eight slots and
    /// the frame being decoded).
    pub fn new(accel: A, config_obus: Vec<u8>, size: (u32, u32), bits: u32) -> Result<Self> {
        if accel.surfaces().len() <= SLOTS {
            return Err(decode_err(format!("{} surfaces are too few for AV1", accel.surfaces().len())));
        }
        Ok(Self { accel, parser: Parser::new(), config_obus, primed: false, slots: [None; SLOTS], size, bits })
    }

    pub fn accel(&self) -> &A {
        &self.accel
    }

    /// Decode one temporal unit; the frames it shows come back in order.
    pub fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        if !self.primed {
            self.primed = true;
            let mut events = Vec::new();
            self.parser.parse(&self.config_obus, &mut events).map_err(decode_err)?;
        }
        let mut events = Vec::new();
        let parsed = self.parser.parse(sample, &mut events);
        let mut frames = Vec::new();
        for e in events {
            match e {
                Event::Frame { frame, .. } => self.decode_frame(*frame, pts, &mut frames)?,
                Event::ShowExisting { header, info, .. } => {
                    if header.film_grain.apply_grain {
                        return Err(unsupported("film grain"));
                    }
                    if (info.upscaled_width, info.frame_height) != self.size {
                        return Err(unsupported(&format!("a {}x{} frame in a {}x{} stream", info.upscaled_width, info.frame_height, self.size.0, self.size.1)));
                    }
                    let surface =
                        self.slots.get(header.frame_to_show_map_idx).copied().flatten().ok_or_else(|| decode_err("show_existing_frame of an empty slot"))?;
                    let frame = self.accel.read(surface).map_err(decode_err)?;
                    frames.push(DecodedFrame { pts, frame, draft: false });
                    if header.frame_type == KEY_FRAME {
                        // a shown key frame refreshes every slot from its own (7.21, 7.20)
                        self.slots = [Some(surface); SLOTS];
                    }
                }
            }
        }
        parsed.map_err(decode_err)?;
        Ok(frames)
    }

    /// Nothing is held for reordering.
    pub fn flush(&mut self) -> Result<Vec<DecodedFrame>> {
        Ok(Vec::new())
    }

    /// Forget every frame (after a seek).
    pub fn reset(&mut self) {
        self.parser = Parser::new();
        self.primed = false;
        self.slots = [None; SLOTS];
    }

    fn decode_frame(&mut self, f: Frame, pts: i64, frames: &mut Vec<DecodedFrame>) -> Result<()> {
        let (seq, fh) = (&f.seq, &f.header);
        if seq.op_idc != 0 {
            return Err(unsupported("scalable AV1 streams"));
        }
        let c = &seq.color;
        if u32::from(c.bit_depth) != self.bits || c.mono_chrome || (c.subsampling_x, c.subsampling_y) != (1, 1) {
            return Err(unsupported(&format!(
                "a {}-bit {} sequence in a {}-bit 4:2:0 stream",
                c.bit_depth,
                if c.mono_chrome { "monochrome" } else { "colour" },
                self.bits
            )));
        }
        if fh.film_grain.apply_grain {
            return Err(unsupported("film grain"));
        }
        if (fh.upscaled_width, fh.frame_height) != self.size {
            return Err(unsupported(&format!("a {}x{} frame in a {}x{} stream", fh.upscaled_width, fh.frame_height, self.size.0, self.size.1)));
        }
        let mut used: Vec<usize> = if fh.frame_is_intra { Vec::new() } else { fh.ref_frame_idx.to_vec() };
        if fh.primary_ref_frame != PRIMARY_REF_NONE {
            used.push(*fh.ref_frame_idx.get(fh.primary_ref_frame).ok_or_else(|| decode_err("primary_ref_frame"))?);
        }
        for &i in &used {
            if self.slots.get(i).copied().flatten().is_none() {
                return Err(decode_err("missing reference frame"));
            }
            let r = f.refs.get(i).ok_or_else(|| decode_err("ref_frame_idx"))?;
            if !fh.frame_is_intra && (r.upscaled_width, r.frame_height) != self.size {
                return Err(unsupported("references of another size"));
            }
        }
        let surfaces = self.accel.surfaces();
        let target = (0..surfaces.len()).find(|i| !self.slots.contains(&Some(*i))).ok_or_else(|| decode_err("no free surface"))?;
        let mut pic = picture_params(seq, fh)?;
        pic.current_frame = *surfaces.get(target).ok_or_else(|| decode_err("surface out of range"))?;
        pic.current_display_picture = pic.current_frame;
        for (m, s) in pic.ref_frame_map.iter_mut().zip(self.slots) {
            *m = s.and_then(|s| surfaces.get(s).copied()).unwrap_or(ffi::VA_INVALID_SURFACE);
        }
        let slices = tile_params(fh, f.tiles)?;
        self.accel.decode_av1(target, &pic, &slices).map_err(decode_err)?;
        for (i, s) in self.slots.iter_mut().enumerate() {
            if (fh.refresh_frame_flags >> i) & 1 == 1 {
                *s = Some(target);
            }
        }
        if fh.show_frame {
            let frame = self.accel.read(target).map_err(decode_err)?;
            frames.push(DecodedFrame { pts, frame, draft: false });
        }
        Ok(())
    }
}

fn flag(on: bool, bit: u32) -> u32 {
    u32::from(on) << bit
}

fn u8_of(v: u32) -> u8 {
    u8::try_from(v).unwrap_or(u8::MAX)
}

fn u16_of(v: u32) -> u16 {
    u16::try_from(v).unwrap_or(u16::MAX)
}

fn i8_of(v: i32) -> i8 {
    i8::try_from(v.clamp(i32::from(i8::MIN), i32::from(i8::MAX))).unwrap_or(0)
}

/// A CDEF secondary strength as coded: the spec turns a coded 3 into 4.
fn cdef_sec(v: u32) -> u8 {
    if v == 4 { 3 } else { u8_of(v & 3) }
}

/// The picture parameters of frame `fh` of sequence `seq`, without the surfaces (`current_frame`,
/// `current_display_picture`, `ref_frame_map`), which the caller fills in.
pub fn picture_params(seq: &SequenceHeader, fh: &FrameHeader) -> Result<VADecPictureParameterBufferAV1> {
    let c = &seq.color;
    let mut p = VADecPictureParameterBufferAV1 {
        profile: seq.profile,
        order_hint_bits_minus_1: if seq.enable_order_hint { u8_of(seq.order_hint_bits.saturating_sub(1)) } else { 0 },
        bit_depth_idx: match c.bit_depth {
            8 => 0,
            10 => 1,
            12 => 2,
            b => return Err(unsupported(&format!("{b}-bit AV1"))),
        },
        matrix_coefficients: c.matrix_coefficients,
        ..Default::default()
    };
    p.seq_info_fields = flag(seq.still_picture, av1_seq_bits::STILL_PICTURE)
        | flag(seq.use_128x128_superblock, av1_seq_bits::USE_128X128_SUPERBLOCK)
        | flag(seq.enable_filter_intra, av1_seq_bits::ENABLE_FILTER_INTRA)
        | flag(seq.enable_intra_edge_filter, av1_seq_bits::ENABLE_INTRA_EDGE_FILTER)
        | flag(seq.enable_interintra_compound, av1_seq_bits::ENABLE_INTERINTRA_COMPOUND)
        | flag(seq.enable_masked_compound, av1_seq_bits::ENABLE_MASKED_COMPOUND)
        | flag(seq.enable_dual_filter, av1_seq_bits::ENABLE_DUAL_FILTER)
        | flag(seq.enable_order_hint, av1_seq_bits::ENABLE_ORDER_HINT)
        | flag(seq.enable_jnt_comp, av1_seq_bits::ENABLE_JNT_COMP)
        | flag(seq.enable_cdef, av1_seq_bits::ENABLE_CDEF)
        | flag(c.mono_chrome, av1_seq_bits::MONO_CHROME)
        | flag(c.color_range, av1_seq_bits::COLOR_RANGE)
        | flag(c.subsampling_x == 1, av1_seq_bits::SUBSAMPLING_X)
        | flag(c.subsampling_y == 1, av1_seq_bits::SUBSAMPLING_Y)
        | flag(seq.film_grain_params_present, av1_seq_bits::FILM_GRAIN_PARAMS_PRESENT);
    p.frame_width_minus1 = u16_of(fh.upscaled_width.saturating_sub(1));
    p.frame_height_minus1 = u16_of(fh.frame_height.saturating_sub(1));
    for (d, s) in p.ref_frame_idx.iter_mut().zip(fh.ref_frame_idx) {
        *d = u8::try_from(s).unwrap_or(0);
    }
    p.primary_ref_frame = u8::try_from(fh.primary_ref_frame).unwrap_or(PRIMARY_REF_NONE as u8);
    p.order_hint = u8_of(fh.order_hint);

    // segmentation
    let seg = &fh.seg;
    if seg.enabled {
        p.seg_info.segment_info_fields = flag(true, av1_seg_bits::ENABLED)
            | flag(seg.update_map, av1_seg_bits::UPDATE_MAP)
            | flag(seg.temporal_update, av1_seg_bits::TEMPORAL_UPDATE)
            | flag(seg.update_data, av1_seg_bits::UPDATE_DATA);
        for (s, (enabled, data)) in seg.features.enabled.iter().zip(&seg.features.data).enumerate() {
            let (Some(mask), Some(out)) = (p.seg_info.feature_mask.get_mut(s), p.seg_info.feature_data.get_mut(s)) else { break };
            for (j, (&on, &v)) in enabled.iter().zip(data).enumerate() {
                if on && j < 8 {
                    *mask |= 1 << j;
                }
                if let Some(o) = out.get_mut(j) {
                    *o = i16::try_from(v).unwrap_or(0);
                }
            }
        }
    }

    // tiles
    let t = &fh.tile_info;
    let sb_mi = if seq.use_128x128_superblock { 32 } else { 16 };
    p.tile_cols = u8::try_from(t.cols).map_err(|_| decode_err("tile columns"))?;
    p.tile_rows = u8::try_from(t.rows).map_err(|_| decode_err("tile rows"))?;
    let sizes = |starts: &[u32], out: &mut [u16; 63]| {
        for (o, w) in out.iter_mut().zip(starts.windows(2)) {
            let mi = w[1].saturating_sub(w[0]);
            *o = u16_of(mi.div_ceil(sb_mi).saturating_sub(1));
        }
    };
    sizes(&t.mi_col_starts, &mut p.width_in_sbs_minus_1);
    sizes(&t.mi_row_starts, &mut p.height_in_sbs_minus_1);
    p.context_update_tile_id = u16_of(t.context_update_tile_id);

    p.pic_info_fields = (u32::from(fh.frame_type) & 3) << av1_pic_bits::FRAME_TYPE
        | flag(fh.show_frame, av1_pic_bits::SHOW_FRAME)
        | flag(fh.showable_frame, av1_pic_bits::SHOWABLE_FRAME)
        | flag(fh.error_resilient_mode, av1_pic_bits::ERROR_RESILIENT_MODE)
        | flag(fh.disable_cdf_update, av1_pic_bits::DISABLE_CDF_UPDATE)
        | flag(fh.allow_screen_content_tools, av1_pic_bits::ALLOW_SCREEN_CONTENT_TOOLS)
        | flag(fh.force_integer_mv, av1_pic_bits::FORCE_INTEGER_MV)
        | flag(fh.allow_intrabc, av1_pic_bits::ALLOW_INTRABC)
        | flag(fh.use_superres, av1_pic_bits::USE_SUPERRES)
        | flag(fh.allow_high_precision_mv, av1_pic_bits::ALLOW_HIGH_PRECISION_MV)
        | flag(fh.is_motion_mode_switchable, av1_pic_bits::IS_MOTION_MODE_SWITCHABLE)
        | flag(fh.use_ref_frame_mvs, av1_pic_bits::USE_REF_FRAME_MVS)
        | flag(fh.disable_frame_end_update_cdf, av1_pic_bits::DISABLE_FRAME_END_UPDATE_CDF)
        | flag(t.uniform, av1_pic_bits::UNIFORM_TILE_SPACING_FLAG)
        | flag(fh.allow_warped_motion, av1_pic_bits::ALLOW_WARPED_MOTION);
    p.superres_scale_denominator = if fh.use_superres { u8_of(fh.superres_denom) } else { 8 };
    p.interp_filter = fh.interpolation_filter;

    // loop filter
    let lf = &fh.lf;
    p.filter_level = [u8_of(lf.level[0]), u8_of(lf.level[1])];
    p.filter_level_u = u8_of(lf.level[2]);
    p.filter_level_v = u8_of(lf.level[3]);
    p.loop_filter_info_fields = (u8_of(lf.sharpness) & 7) << av1_lf_bits::SHARPNESS_LEVEL
        | u8::from(lf.delta_enabled) << av1_lf_bits::MODE_REF_DELTA_ENABLED
        | u8::from(lf.delta_update) << av1_lf_bits::MODE_REF_DELTA_UPDATE;
    for (d, &s) in p.ref_deltas.iter_mut().zip(&lf.deltas.ref_deltas) {
        *d = i8_of(s);
    }
    for (d, &s) in p.mode_deltas.iter_mut().zip(&lf.deltas.mode_deltas) {
        *d = i8_of(s);
    }

    // quantization
    let q = &fh.quant;
    p.base_qindex = u8_of(q.base_q_idx);
    p.y_dc_delta_q = i8_of(q.delta_q_y_dc);
    p.u_dc_delta_q = i8_of(q.delta_q_u_dc);
    p.u_ac_delta_q = i8_of(q.delta_q_u_ac);
    p.v_dc_delta_q = i8_of(q.delta_q_v_dc);
    p.v_ac_delta_q = i8_of(q.delta_q_v_ac);
    if q.using_qmatrix {
        let qm = |v: u32| u16::from(u8_of(v) & 15);
        p.qmatrix_fields =
            1 << av1_qm_bits::USING_QMATRIX | qm(q.qm_y) << av1_qm_bits::QM_Y | qm(q.qm_u) << av1_qm_bits::QM_U | qm(q.qm_v) << av1_qm_bits::QM_V;
    }
    p.mode_control_fields = flag(fh.delta_q_present, av1_mode_bits::DELTA_Q_PRESENT_FLAG)
        | (fh.delta_q_res & 3) << av1_mode_bits::LOG2_DELTA_Q_RES
        | flag(fh.delta_lf_present, av1_mode_bits::DELTA_LF_PRESENT_FLAG)
        | (fh.delta_lf_res & 3) << av1_mode_bits::LOG2_DELTA_LF_RES
        | flag(fh.delta_lf_multi, av1_mode_bits::DELTA_LF_MULTI)
        | (u32::from(fh.tx_mode) & 3) << av1_mode_bits::TX_MODE
        | flag(fh.reference_select, av1_mode_bits::REFERENCE_SELECT)
        | flag(fh.reduced_tx_set, av1_mode_bits::REDUCED_TX_SET_USED)
        | flag(fh.skip_mode_present, av1_mode_bits::SKIP_MODE_PRESENT);

    // CDEF
    let cd = &fh.cdef;
    p.cdef_damping_minus_3 = u8_of(cd.damping.saturating_sub(3));
    p.cdef_bits = u8_of(cd.bits);
    for i in 0..8 {
        let (yp, ys, up, us) = (cd.y_pri[i], cd.y_sec[i], cd.uv_pri[i], cd.uv_sec[i]);
        p.cdef_y_strengths[i] = (u8_of(yp) & 15) << 2 | cdef_sec(ys);
        p.cdef_uv_strengths[i] = (u8_of(up) & 15) << 2 | cdef_sec(us);
    }

    // loop restoration (FrameRestorationType; sizes as the coded shifts)
    let lr = &fh.lr;
    let ty = |i: usize| u16::from(lr.frame_restoration_type[i] & 3);
    let mut lrf =
        ty(0) << av1_lr_bits::YFRAME_RESTORATION_TYPE | ty(1) << av1_lr_bits::CBFRAME_RESTORATION_TYPE | ty(2) << av1_lr_bits::CRFRAME_RESTORATION_TYPE;
    if lr.uses_lr {
        let log2 = |v: u32| v.max(1).trailing_zeros();
        let (l0, l1) = (log2(lr.loop_restoration_size[0]), log2(lr.loop_restoration_size[1]));
        let unit_shift = u16::try_from(l0.saturating_sub(6) & 3).unwrap_or(0);
        let uv_shift = u16::try_from(l0.saturating_sub(l1) & 1).unwrap_or(0);
        lrf |= unit_shift << av1_lr_bits::LR_UNIT_SHIFT | uv_shift << av1_lr_bits::LR_UV_SHIFT;
    }
    p.loop_restoration_fields = lrf;

    // global motion of LAST_FRAME..=ALTREF_FRAME
    for (i, wm) in p.wm.iter_mut().enumerate() {
        let r = i + 1;
        let (Some(&ty), Some(params)) = (fh.gm_type.get(r), fh.gm_params.get(r)) else { break };
        let mut wmmat = [0i32; 8];
        wmmat[..6].copy_from_slice(params);
        *wm = VAWarpedMotionParamsAV1 {
            wmtype: c_int::from(ty),
            wmmat,
            invalid: u8::from(!filmcraft_av1::warp_valid(params)),
            va_reserved: [0; ffi::VA_PADDING_LOW],
        };
    }
    Ok(p)
}

/// One slice parameter + data buffer per tile.
pub fn tile_params(fh: &FrameHeader, tiles: Vec<(usize, Vec<u8>)>) -> Result<Vec<(VASliceParameterBufferAV1, Vec<u8>)>> {
    let cols = fh.tile_info.cols.max(1);
    let last = fh.tile_info.cols.saturating_mul(fh.tile_info.rows).saturating_sub(1);
    tiles
        .into_iter()
        .map(|(n, data)| {
            let s = VASliceParameterBufferAV1 {
                slice_data_size: u32::try_from(data.len()).map_err(|_| decode_err("tile too large"))?,
                slice_data_offset: 0,
                slice_data_flag: ffi::VA_SLICE_DATA_FLAG_ALL,
                tile_row: u16::try_from(n / cols).map_err(|_| decode_err("tile row"))?,
                tile_column: u16::try_from(n % cols).map_err(|_| decode_err("tile column"))?,
                tg_start: 0,
                tg_end: u16::try_from(last).unwrap_or(u16::MAX),
                ..Default::default()
            };
            Ok((s, data))
        })
        .collect()
}

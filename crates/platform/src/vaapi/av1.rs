//! AV1 on VA-API: `filmcraft_av1`'s accelerated front end ([`filmcraft_av1::accel`]) with an
//! [`Accelerator`] that sends each frame to the GPU. OBUs, the sequence and frame headers, tile
//! groups, reference slots and `show_existing_frame` are the software decoder's own; the GPU does
//! entropy decoding (with its own CDFs), reconstruction and the loop filters.
//!
//! Main profile (8- and 10-bit 4:2:0). Film grain, a frame whose size differs from the session's,
//! a missing reference or a driver error fail over to software in [`crate::HybridDecoder`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_av1::accel::{AccelDecoder, AccelFrame, AccelOutput, Accelerator};
use filmcraft_codecs::hw::FrameStreamInfo;
use filmcraft_codecs::{CodecError, DecodedFrame, Result, VideoDecoder};

use super::device::{Display, Session};
use super::ffi::*;
use crate::biplanar::Geometry;

/// The eight reference slots, the frame being decoded and room for the one being read back.
const SURFACES: usize = 12;

fn rt_format(bits: u32) -> u32 {
    if bits > 8 { VA_RT_FORMAT_YUV420_10 } else { VA_RT_FORMAT_YUV420 }
}

struct State {
    display: Option<Display>,
    session: Option<Session>,
    slots: HashMap<u32, usize>,
}

impl State {
    fn session_for(&mut self, bits: u32, w: u32, h: u32, key: bool) -> std::result::Result<(), String> {
        let fits = self.session.as_ref().is_some_and(|s| s.width == w && s.height == h && s.bits == bits);
        if fits {
            return Ok(());
        }
        // a new size starts over, which only a key frame can do here (an inter frame of another
        // size would need its references scaled from the old surfaces)
        if !key && self.session.is_some() {
            return Err(format!("AV1 frame size changed to {w}x{h} at an inter frame"));
        }
        self.slots.clear();
        let display = match self.display.take() {
            Some(d) => d,
            None => {
                self.session = None;
                Display::open_for(VAProfileAV1Profile0, rt_format(bits))?
            }
        };
        self.session = None;
        self.session = Some(Session::new(display, VAProfileAV1Profile0, bits, w, h, SURFACES)?);
        Ok(())
    }
}

struct VaAccel {
    state: Arc<Mutex<State>>,
}

fn i8_of(v: i32) -> i8 {
    v.clamp(i8::MIN as i32, i8::MAX as i32) as i8
}

fn u8_of(v: u32) -> u8 {
    v.min(u8::MAX as u32) as u8
}

/// log2 of a power of two (0 for anything else).
fn log2(v: u32) -> u32 {
    if v.is_power_of_two() { v.trailing_zeros() } else { 0 }
}

/// Tile widths / heights in superblocks minus 1, from the tile start positions (in mode info
/// units, the last entry the frame's end).
fn tile_sizes(starts: &[u32], sb_mi: u32) -> [u16; 63] {
    let mut out = [0u16; 63];
    for (o, w) in out.iter_mut().zip(starts.windows(2)) {
        if let [a, b] = w {
            *o = b.saturating_sub(*a).div_ceil(sb_mi).saturating_sub(1).min(u16::MAX as u32) as u16;
        }
    }
    out
}

impl Accelerator for VaAccel {
    fn decode_frame(&mut self, f: &AccelFrame<'_>) -> std::result::Result<(), String> {
        let (seq, h) = (f.seq, f.header);
        let c = &seq.color;
        if seq.profile != 0 || c.mono_chrome || c.subsampling_x != 1 || c.subsampling_y != 1 || !matches!(c.bit_depth, 8 | 10) {
            return Err("only 8- and 10-bit 4:2:0 AV1 (Main profile) is decoded in hardware".into());
        }
        if h.film_grain.apply_grain {
            return Err("AV1 film grain is applied by the software decoder".into());
        }
        let width = u16::try_from(h.frame_width.saturating_sub(1)).map_err(|_| "frame too wide")?;
        let height = u16::try_from(h.frame_height.saturating_sub(1)).map_err(|_| "frame too tall")?;
        let bits = u32::from(c.bit_depth);
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.session_for(bits, h.upscaled_width, h.frame_height, h.frame_is_intra)?;
        let st = &mut *guard;
        let session = st.session.as_ref().ok_or("no session")?;
        let mut ref_frame_map = [VA_INVALID_SURFACE; 8];
        for (r, id) in ref_frame_map.iter_mut().zip(&f.slots) {
            if let Some(s) = id.and_then(|id| st.slots.get(&id)).and_then(|s| session.surface_id(*s)) {
                *r = s;
            }
        }
        let used: Vec<usize> = st.slots.values().copied().collect();
        let slot = (0..session.surface_count()).find(|i| !used.contains(i)).ok_or("no free surface")?;
        st.slots.insert(f.id, slot);
        let cur = session.surface_id(slot).ok_or("no surface")?;

        let seg = &h.seg;
        let mut feature_data = [[0i16; 8]; 8];
        let mut feature_mask = [0u8; 8];
        if seg.enabled {
            for (i, (d, m)) in feature_data.iter_mut().zip(feature_mask.iter_mut()).enumerate() {
                for j in 0..8 {
                    if seg.features.enabled[i][j] {
                        *m |= 1 << j;
                        d[j] = seg.features.data[i][j].clamp(i16::MIN as i32, i16::MAX as i32) as i16;
                    }
                }
            }
        }
        let t = &h.tile_info;
        let sb_mi = if seq.use_128x128_superblock { 32 } else { 16 };
        let cdef = &h.cdef;
        // (the coded secondary strength: the header maps 3 to 4)
        let strength = |pri: u32, sec: u32| u8_of((pri << 2) | if sec == 4 { 3 } else { sec & 3 });
        let lr = &h.lr;
        let (lr_unit_shift, lr_uv_shift) = if lr.uses_lr {
            let y = lr.loop_restoration_size[0];
            (log2(y).saturating_sub(6), log2(y).saturating_sub(log2(lr.loop_restoration_size[1])))
        } else {
            (0, 0)
        };
        let lf = &h.lf;
        let q = &h.quant;
        let wm: [VAWarpedMotionParamsAV1; 7] = std::array::from_fn(|i| {
            let r = i + 1;
            let mut wmmat = [0i32; 8];
            wmmat[..6].copy_from_slice(&h.gm_params[r]);
            VAWarpedMotionParamsAV1 { wmtype: u32::from(h.gm_type[r]), wmmat, invalid: u8::from(!f.gm_valid[r]), va_reserved: [0; 4] }
        });
        let pic = VADecPictureParameterBufferAV1 {
            profile: seq.profile,
            order_hint_bits_minus_1: u8_of(seq.order_hint_bits.saturating_sub(1)),
            bit_depth_idx: (c.bit_depth - 8) / 2,
            matrix_coefficients: c.matrix_coefficients,
            seq_info_fields: pack_bits(&[
                (u32::from(seq.still_picture), 1),
                (u32::from(seq.use_128x128_superblock), 1),
                (u32::from(seq.enable_filter_intra), 1),
                (u32::from(seq.enable_intra_edge_filter), 1),
                (u32::from(seq.enable_interintra_compound), 1),
                (u32::from(seq.enable_masked_compound), 1),
                (u32::from(seq.enable_dual_filter), 1),
                (u32::from(seq.enable_order_hint), 1),
                (u32::from(seq.enable_jnt_comp), 1),
                (u32::from(seq.enable_cdef), 1),
                (u32::from(c.mono_chrome), 1),
                (u32::from(c.color_range), 1),
                (u32::from(c.subsampling_x), 1),
                (u32::from(c.subsampling_y), 1),
                (0, 1),
                (u32::from(seq.film_grain_params_present), 1),
            ]),
            current_frame: cur,
            current_display_picture: cur,
            anchor_frames_num: 0,
            anchor_frames_list: 0,
            frame_width_minus1: width,
            frame_height_minus1: height,
            output_frame_width_in_tiles_minus_1: 0,
            output_frame_height_in_tiles_minus_1: 0,
            ref_frame_map,
            ref_frame_idx: std::array::from_fn(|i| h.ref_frame_idx.get(i).map_or(0, |&r| u8_of(r as u32))),
            primary_ref_frame: u8_of(h.primary_ref_frame as u32),
            order_hint: u8_of(h.order_hint),
            seg_info: VASegmentationStructAV1 {
                segment_info_fields: pack_bits(&[
                    (u32::from(seg.enabled), 1),
                    (u32::from(seg.update_map), 1),
                    (u32::from(seg.temporal_update), 1),
                    (u32::from(seg.update_data), 1),
                ]),
                feature_data,
                feature_mask,
                va_reserved: [0; 4],
            },
            film_grain_info: VAFilmGrainStructAV1 {
                film_grain_info_fields: 0,
                grain_seed: 0,
                num_y_points: 0,
                point_y_value: [0; 14],
                point_y_scaling: [0; 14],
                num_cb_points: 0,
                point_cb_value: [0; 10],
                point_cb_scaling: [0; 10],
                num_cr_points: 0,
                point_cr_value: [0; 10],
                point_cr_scaling: [0; 10],
                ar_coeffs_y: [0; 24],
                ar_coeffs_cb: [0; 25],
                ar_coeffs_cr: [0; 25],
                cb_mult: 0,
                cb_luma_mult: 0,
                cb_offset: 0,
                cr_mult: 0,
                cr_luma_mult: 0,
                cr_offset: 0,
                va_reserved: [0; 4],
            },
            tile_cols: u8::try_from(t.cols).map_err(|_| "too many tile columns")?,
            tile_rows: u8::try_from(t.rows).map_err(|_| "too many tile rows")?,
            width_in_sbs_minus_1: tile_sizes(&t.mi_col_starts, sb_mi),
            height_in_sbs_minus_1: tile_sizes(&t.mi_row_starts, sb_mi),
            tile_count_minus_1: 0,
            context_update_tile_id: u16::try_from(t.context_update_tile_id).map_err(|_| "context_update_tile_id")?,
            pic_info_fields: pack_bits(&[
                (u32::from(h.frame_type), 2),
                (u32::from(h.show_frame), 1),
                (u32::from(h.showable_frame), 1),
                (u32::from(h.error_resilient_mode), 1),
                (u32::from(h.disable_cdf_update), 1),
                (u32::from(h.allow_screen_content_tools), 1),
                (u32::from(h.force_integer_mv), 1),
                (u32::from(h.allow_intrabc), 1),
                (u32::from(h.use_superres), 1),
                (u32::from(h.allow_high_precision_mv), 1),
                (u32::from(h.is_motion_mode_switchable), 1),
                (u32::from(h.use_ref_frame_mvs), 1),
                (u32::from(h.disable_frame_end_update_cdf), 1),
                (0, 1),
                (u32::from(h.allow_warped_motion), 1),
                (0, 1),
            ]),
            superres_scale_denominator: u8_of(h.superres_denom),
            interp_filter: h.interpolation_filter,
            filter_level: [u8_of(lf.level[0]), u8_of(lf.level[1])],
            filter_level_u: u8_of(lf.level[2]),
            filter_level_v: u8_of(lf.level[3]),
            loop_filter_info_fields: pack_bits(&[(lf.sharpness.min(7), 3), (u32::from(lf.delta_enabled), 1), (u32::from(lf.delta_update), 1)]) as u8,
            ref_deltas: lf.deltas.ref_deltas.map(i8_of),
            mode_deltas: lf.deltas.mode_deltas.map(i8_of),
            base_qindex: u8_of(q.base_q_idx),
            y_dc_delta_q: i8_of(q.delta_q_y_dc),
            u_dc_delta_q: i8_of(q.delta_q_u_dc),
            u_ac_delta_q: i8_of(q.delta_q_u_ac),
            v_dc_delta_q: i8_of(q.delta_q_v_dc),
            v_ac_delta_q: i8_of(q.delta_q_v_ac),
            qmatrix_fields: pack_bits(&[(u32::from(q.using_qmatrix), 1), (q.qm_y.min(15), 4), (q.qm_u.min(15), 4), (q.qm_v.min(15), 4)]) as u16,
            mode_control_fields: pack_bits(&[
                (u32::from(h.delta_q_present), 1),
                (h.delta_q_res.min(3), 2),
                (u32::from(h.delta_lf_present), 1),
                (h.delta_lf_res.min(3), 2),
                (u32::from(h.delta_lf_multi), 1),
                (u32::from(h.tx_mode).min(3), 2),
                (u32::from(h.reference_select), 1),
                (u32::from(h.reduced_tx_set), 1),
                (u32::from(h.skip_mode_present), 1),
            ]),
            cdef_damping_minus_3: u8_of(cdef.damping.saturating_sub(3)),
            cdef_bits: u8_of(cdef.bits),
            cdef_y_strengths: std::array::from_fn(|i| strength(cdef.y_pri[i], cdef.y_sec[i])),
            cdef_uv_strengths: std::array::from_fn(|i| strength(cdef.uv_pri[i], cdef.uv_sec[i])),
            loop_restoration_fields: pack_bits(&[
                (u32::from(lr.frame_restoration_type[0]).min(3), 2),
                (u32::from(lr.frame_restoration_type[1]).min(3), 2),
                (u32::from(lr.frame_restoration_type[2]).min(3), 2),
                (lr_unit_shift.min(3), 2),
                (lr_uv_shift.min(1), 1),
            ]) as u16,
            wm,
            va_reserved: [0; 8],
        };
        let mut slices = Vec::with_capacity(f.tiles.len());
        for tile in f.tiles {
            let u16_of = |v: usize| u16::try_from(v).map_err(|_| "tile index");
            slices.push(VASliceParameterBufferAV1 {
                slice_data_size: u32::try_from(tile.size).map_err(|_| "tile too large")?,
                slice_data_offset: u32::try_from(tile.offset).map_err(|_| "tile offset")?,
                slice_data_flag: VA_SLICE_DATA_FLAG_ALL,
                tile_row: u16_of(tile.row)?,
                tile_column: u16_of(tile.col)?,
                tg_start: u16_of(tile.tg_start)?,
                tg_end: u16_of(tile.tg_end)?,
                anchor_frame_idx: 0,
                tile_idx_in_tile_list: 0,
                va_reserved: [0; 4],
            });
        }
        let bufs = vec![session.params(VAPictureParameterBufferType, &[pic])?, session.params(VASliceParameterBufferType, &slices)?, session.data(f.data)?];
        session.decode(slot, &bufs)
    }

    fn retain(&mut self, live: &[u32]) {
        let mut st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        st.slots.retain(|id, _| live.contains(id));
    }
}

/// A VA-API AV1 decoder behind FilmCraft's [`VideoDecoder`].
pub struct VaAv1Decoder {
    dec: AccelDecoder,
    state: Arc<Mutex<State>>,
    info: FrameStreamInfo,
}

impl VaAv1Decoder {
    /// A decoder for the stream `info` describes (primed with its `av1C` sequence header); fails
    /// when this system cannot decode it in hardware.
    pub fn new(info: FrameStreamInfo) -> std::result::Result<Self, String> {
        let display = Display::open_for(VAProfileAV1Profile0, rt_format(info.bit_depth))?;
        let state = Arc::new(Mutex::new(State { display: Some(display), session: None, slots: HashMap::new() }));
        let mut dec = AccelDecoder::new(Box::new(VaAccel { state: state.clone() }));
        if !info.config_obus.is_empty() {
            dec.decode(&info.config_obus, 0).map_err(|e| e.to_string())?;
        }
        Ok(VaAv1Decoder { dec, state, info })
    }

    fn frames(&mut self, outs: Vec<AccelOutput>) -> Result<Vec<DecodedFrame>> {
        let st = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        let color = self.info.color.unwrap_or(filmcraft_color::ColorInfo::REC709);
        let bits = self.info.bit_depth;
        let mut v = Vec::with_capacity(outs.len());
        for o in outs {
            if o.film_grain.apply_grain {
                return Err(CodecError::Decode("AV1 film grain is applied by the software decoder".into()));
            }
            let session = st.session.as_ref().ok_or_else(|| CodecError::Decode("no hardware session".into()))?;
            let slot = st.slots.get(&o.id).copied().ok_or_else(|| CodecError::Decode(format!("frame {} has no surface", o.id)))?;
            let g = Geometry { crop: (0, 0, o.width, o.height), bits, color, par: (1, 1) };
            let frame = session.read(slot, &g).map_err(CodecError::Decode)?;
            v.push(DecodedFrame { pts: o.pts, frame, draft: false });
        }
        Ok(v)
    }
}

impl VideoDecoder for VaAv1Decoder {
    fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let outs = self.dec.decode(sample, pts).map_err(|e| CodecError::Decode(e.to_string()))?;
        self.frames(outs)
    }

    fn flush(&mut self) -> Vec<DecodedFrame> {
        Vec::new()
    }

    fn reset(&mut self) {
        self.dec.reset();
        self.state.lock().unwrap_or_else(PoisonError::into_inner).slots.clear();
    }

    fn name(&self) -> &str {
        "VA-API AV1"
    }

    fn is_random_access(&self, sample: &[u8]) -> Option<bool> {
        Some(self.info.is_random_access(sample))
    }
}

//! The host side of stateless AV1 decoding: [`filmcraft_av1::HostParser`] reads the headers the way
//! the software decoder does, and this module turns each frame into VA-API's picture and tile
//! parameters and keeps the eight reference slots as surfaces. Safe code; the driver is behind
//! [`Accel`].
//!
//! Frames the session was not set up for (another size or bit depth, reference scaling included)
//! are errors, which `HybridDecoder` answers by continuing in software.

use std::collections::HashSet;

use filmcraft_av1::{FrameHeader, HostEvent, HostFrame, HostParser, SequenceHeader, global_warp_valid};
use filmcraft_codecs::{CodecError, DecodedFrame, Result};

use super::Accel;
use super::ffi::{self, VADecPictureParameterBufferAV1, VAFilmGrainStructAV1, VASegmentationStructAV1, VASliceParameterBufferAV1, VAWarpedMotionParamsAV1};

/// Surfaces a stream needs: the eight reference slots, the frame being decoded, its copy with film
/// grain, and one spare.
pub const SURFACES: usize = 11;

fn decode_err(e: impl std::fmt::Display) -> CodecError {
    CodecError::Decode(format!("VA-API AV1: {e}"))
}

/// Packs `(value, width)` bit-fields, first one in the lowest bits (the C layout on every target
/// libva supports; checked against the header by the abi tests).
fn bits(fields: &[(u32, u32)]) -> u32 {
    let mut word = 0u32;
    let mut at = 0u32;
    for &(value, width) in fields {
        let mask = 1u32.checked_shl(width).map_or(u32::MAX, |m| m.wrapping_sub(1));
        word |= (value & mask).checked_shl(at).unwrap_or(0);
        at = at.saturating_add(width);
    }
    word
}

/// The first `N` values converted, the rest zero; what does not fit the C type becomes zero.
fn fill<const N: usize, S: Copy, T: Copy + Default>(from: &[S], to: impl Fn(S) -> Option<T>) -> [T; N] {
    let mut out = [T::default(); N];
    for (o, &v) in out.iter_mut().zip(from) {
        *o = to(v).unwrap_or_default();
    }
    out
}

fn film_grain(h: &FrameHeader) -> VAFilmGrainStructAV1 {
    let g = &h.film_grain;
    let u8s = |v: u32| u8::try_from(v).unwrap_or(u8::MAX);
    let signed = |v: u8| i8::try_from(i16::from(v) - 128).ok();
    let same = |v: u8| Some(v);
    let (ny, ncb, ncr) = (g.num_y_points.min(14), g.num_cb_points.min(10), g.num_cr_points.min(10));
    VAFilmGrainStructAV1 {
        film_grain_info_fields: if g.apply_grain {
            bits(&[
                (1, 1),
                (u32::from(g.chroma_scaling_from_luma), 1),
                (g.grain_scaling_minus_8, 2),
                (g.ar_coeff_lag, 2),
                (g.ar_coeff_shift_minus_6, 2),
                (g.grain_scale_shift, 2),
                (u32::from(g.overlap_flag), 1),
                (u32::from(g.clip_to_restricted_range), 1),
            ])
        } else {
            0
        },
        grain_seed: u16::try_from(g.grain_seed).unwrap_or(0),
        num_y_points: u8::try_from(ny).unwrap_or(0),
        point_y_value: fill(g.point_y_value.get(..ny).unwrap_or_default(), same),
        point_y_scaling: fill(g.point_y_scaling.get(..ny).unwrap_or_default(), same),
        num_cb_points: u8::try_from(ncb).unwrap_or(0),
        point_cb_value: fill(g.point_cb_value.get(..ncb).unwrap_or_default(), same),
        point_cb_scaling: fill(g.point_cb_scaling.get(..ncb).unwrap_or_default(), same),
        num_cr_points: u8::try_from(ncr).unwrap_or(0),
        point_cr_value: fill(g.point_cr_value.get(..ncr).unwrap_or_default(), same),
        point_cr_scaling: fill(g.point_cr_scaling.get(..ncr).unwrap_or_default(), same),
        ar_coeffs_y: fill(&g.ar_coeffs_y_plus_128, signed),
        ar_coeffs_cb: fill(&g.ar_coeffs_cb_plus_128, signed),
        ar_coeffs_cr: fill(&g.ar_coeffs_cr_plus_128, signed),
        cb_mult: u8s(g.cb_mult),
        cb_luma_mult: u8s(g.cb_luma_mult),
        cb_offset: u16::try_from(g.cb_offset).unwrap_or(0),
        cr_mult: u8s(g.cr_mult),
        cr_luma_mult: u8s(g.cr_luma_mult),
        cr_offset: u16::try_from(g.cr_offset).unwrap_or(0),
        va_reserved: [0; 4],
    }
}

fn segmentation(h: &FrameHeader) -> VASegmentationStructAV1 {
    let s = &h.seg;
    let (mut feature_data, mut feature_mask) = ([[0i16; 8]; 8], [0u8; 8]);
    if s.enabled {
        for (segment, (data, mask)) in feature_data.iter_mut().zip(&mut feature_mask).enumerate() {
            for (feature, value) in data.iter_mut().enumerate() {
                if s.features.enabled.get(segment).and_then(|f| f.get(feature)).copied().unwrap_or(false) {
                    *mask |= 1u8.checked_shl(feature as u32).unwrap_or(0);
                    *value = s.features.data.get(segment).and_then(|f| f.get(feature)).and_then(|&v| i16::try_from(v).ok()).unwrap_or(0);
                }
            }
        }
    }
    VASegmentationStructAV1 {
        segment_info_fields: bits(&[(u32::from(s.enabled), 1), (u32::from(s.update_map), 1), (u32::from(s.temporal_update), 1), (u32::from(s.update_data), 1)]),
        feature_data,
        feature_mask,
        va_reserved: [0; 4],
    }
}

fn global_motion(h: &FrameHeader) -> [VAWarpedMotionParamsAV1; 7] {
    std::array::from_fn(|i| {
        // reference i is LAST_FRAME + i
        let params = h.gm_params.get(i + 1).copied().unwrap_or_default();
        VAWarpedMotionParamsAV1 {
            wmtype: h.gm_type.get(i + 1).copied().map_or(0, i32::from),
            wmmat: fill(&params, Some),
            invalid: u8::from(!global_warp_valid(&params)),
            va_reserved: [0; 4],
        }
    })
}

/// Tile sizes in superblocks minus one, from the tile starts in mode info units.
fn tile_sizes(starts: &[u32], sb_mi: u32) -> [u16; 63] {
    let sizes: Vec<u16> = starts.windows(2).map(|w| u16::try_from(w[1].saturating_sub(w[0]).div_ceil(sb_mi).saturating_sub(1)).unwrap_or(0)).collect();
    fill(&sizes, Some)
}

/// The CDEF strength as VA-API packs it. The header holds the secondary strength as the spec's
/// variable, where a coded 3 means 4.
fn cdef_strength(pri: u32, sec: u32) -> u8 {
    u8::try_from((pri << 2) | (if sec == 4 { 3 } else { sec } & 3)).unwrap_or(0)
}

fn i8s(v: i32) -> i8 {
    i8::try_from(v).unwrap_or(0)
}

/// VA-API's parameters for a frame, to be decoded into `current` and shown from `display`
/// (`current` unless grain is applied), with `refs` the surfaces of the reference slots.
pub(crate) fn picture_parameters(
    seq: &SequenceHeader,
    h: &FrameHeader,
    current: ffi::VASurfaceID,
    display: ffi::VASurfaceID,
    refs: [ffi::VASurfaceID; 8],
) -> VADecPictureParameterBufferAV1 {
    let flag = u32::from;
    let c = &seq.color;
    let sb_mi = if seq.use_128x128_superblock { 32 } else { 16 };
    let lr = &h.lr;
    let (lr_unit_shift, lr_uv_shift) = if lr.uses_lr {
        let [luma, chroma, _] = lr.loop_restoration_size;
        (luma.checked_ilog2().unwrap_or(6).saturating_sub(6), u32::from(chroma != 0 && chroma < luma))
    } else {
        (0, 0)
    };
    let [ry, rcb, rcr] = lr.frame_restoration_type;
    let t = &h.tile_info;
    VADecPictureParameterBufferAV1 {
        profile: seq.profile,
        order_hint_bits_minus_1: u8::try_from(seq.order_hint_bits.saturating_sub(1)).unwrap_or(0),
        bit_depth_idx: match c.bit_depth {
            10 => 1,
            12 => 2,
            _ => 0,
        },
        matrix_coefficients: c.matrix_coefficients,
        seq_info_fields: bits(&[
            (flag(seq.still_picture), 1),
            (flag(seq.use_128x128_superblock), 1),
            (flag(seq.enable_filter_intra), 1),
            (flag(seq.enable_intra_edge_filter), 1),
            (flag(seq.enable_interintra_compound), 1),
            (flag(seq.enable_masked_compound), 1),
            (flag(seq.enable_dual_filter), 1),
            (flag(seq.enable_order_hint), 1),
            (flag(seq.enable_jnt_comp), 1),
            (flag(seq.enable_cdef), 1),
            (flag(c.mono_chrome), 1),
            (flag(c.color_range), 1),
            (u32::from(c.subsampling_x), 1),
            (u32::from(c.subsampling_y), 1),
            (u32::from(c.chroma_sample_position), 1),
            (flag(seq.film_grain_params_present), 1),
        ]),
        current_frame: current,
        current_display_picture: display,
        anchor_frames_num: 0,
        anchor_frames_list: std::ptr::null_mut(),
        frame_width_minus1: u16::try_from(h.upscaled_width.saturating_sub(1)).unwrap_or(0),
        frame_height_minus1: u16::try_from(h.frame_height.saturating_sub(1)).unwrap_or(0),
        output_frame_width_in_tiles_minus_1: 0,
        output_frame_height_in_tiles_minus_1: 0,
        ref_frame_map: refs,
        ref_frame_idx: fill(&h.ref_frame_idx, |v| u8::try_from(v).ok()),
        primary_ref_frame: u8::try_from(h.primary_ref_frame).unwrap_or(7),
        order_hint: u8::try_from(h.order_hint).unwrap_or(0),
        seg_info: segmentation(h),
        film_grain_info: film_grain(h),
        tile_cols: u8::try_from(t.cols).unwrap_or(0),
        tile_rows: u8::try_from(t.rows).unwrap_or(0),
        width_in_sbs_minus_1: tile_sizes(&t.mi_col_starts, sb_mi),
        height_in_sbs_minus_1: tile_sizes(&t.mi_row_starts, sb_mi),
        tile_count_minus_1: 0,
        context_update_tile_id: u16::try_from(t.context_update_tile_id).unwrap_or(0),
        // uniform_tile_spacing_flag stays 0: the sizes above are always given
        pic_info_fields: bits(&[
            (u32::from(h.frame_type), 2),
            (flag(h.show_frame), 1),
            (flag(h.showable_frame), 1),
            (flag(h.error_resilient_mode), 1),
            (flag(h.disable_cdf_update), 1),
            (flag(h.allow_screen_content_tools), 1),
            (flag(h.force_integer_mv), 1),
            (flag(h.allow_intrabc), 1),
            (flag(h.use_superres), 1),
            (flag(h.allow_high_precision_mv), 1),
            (flag(h.is_motion_mode_switchable), 1),
            (flag(h.use_ref_frame_mvs), 1),
            (flag(h.disable_frame_end_update_cdf), 1),
            (0, 1),
            (flag(h.allow_warped_motion), 1),
            (0, 1),
        ]),
        superres_scale_denominator: if h.use_superres { u8::try_from(h.superres_denom).unwrap_or(8) } else { 8 },
        interp_filter: h.interpolation_filter,
        filter_level: [u8::try_from(h.lf.level[0]).unwrap_or(0), u8::try_from(h.lf.level[1]).unwrap_or(0)],
        filter_level_u: u8::try_from(h.lf.level[2]).unwrap_or(0),
        filter_level_v: u8::try_from(h.lf.level[3]).unwrap_or(0),
        loop_filter_info_fields: u8::try_from(bits(&[(h.lf.sharpness, 3), (flag(h.lf.delta_enabled), 1), (flag(h.lf.delta_update), 1)])).unwrap_or(0),
        ref_deltas: fill(&h.lf.deltas.ref_deltas, |v| Some(i8s(v))),
        mode_deltas: fill(&h.lf.deltas.mode_deltas, |v| Some(i8s(v))),
        base_qindex: u8::try_from(h.quant.base_q_idx).unwrap_or(0),
        y_dc_delta_q: i8s(h.quant.delta_q_y_dc),
        u_dc_delta_q: i8s(h.quant.delta_q_u_dc),
        u_ac_delta_q: i8s(h.quant.delta_q_u_ac),
        v_dc_delta_q: i8s(h.quant.delta_q_v_dc),
        v_ac_delta_q: i8s(h.quant.delta_q_v_ac),
        qmatrix_fields: u16::try_from(bits(&[(flag(h.quant.using_qmatrix), 1), (h.quant.qm_y, 4), (h.quant.qm_u, 4), (h.quant.qm_v, 4)])).unwrap_or(0),
        mode_control_fields: bits(&[
            (flag(h.delta_q_present), 1),
            (h.delta_q_res, 2),
            (flag(h.delta_lf_present), 1),
            (h.delta_lf_res, 2),
            (flag(h.delta_lf_multi), 1),
            (u32::from(h.tx_mode), 2),
            (flag(h.reference_select), 1),
            (flag(h.reduced_tx_set), 1),
            (flag(h.skip_mode_present), 1),
        ]),
        cdef_damping_minus_3: u8::try_from(h.cdef.damping.saturating_sub(3)).unwrap_or(0),
        cdef_bits: u8::try_from(h.cdef.bits).unwrap_or(0),
        cdef_y_strengths: std::array::from_fn(|i| cdef_strength(h.cdef.y_pri[i], h.cdef.y_sec[i])),
        cdef_uv_strengths: std::array::from_fn(|i| cdef_strength(h.cdef.uv_pri[i], h.cdef.uv_sec[i])),
        loop_restoration_fields: u16::try_from(bits(&[(u32::from(ry), 2), (u32::from(rcb), 2), (u32::from(rcr), 2), (lr_unit_shift, 2), (lr_uv_shift, 1)]))
            .unwrap_or(0),
        wm: global_motion(h),
        va_reserved: [0; 8],
    }
}

/// VA-API's parameters and data of each tile.
fn tile_parameters(frame: &HostFrame) -> Result<Vec<(VASliceParameterBufferAV1, Vec<u8>)>> {
    let cols = frame.header.tile_info.cols.max(1);
    frame
        .tiles
        .iter()
        .map(|t| {
            let params = VASliceParameterBufferAV1 {
                slice_data_size: u32::try_from(t.data.len()).map_err(decode_err)?,
                tile_row: u16::try_from(t.number / cols).map_err(decode_err)?,
                tile_column: u16::try_from(t.number % cols).map_err(decode_err)?,
                ..Default::default()
            };
            Ok((params, t.data.clone()))
        })
        .collect()
}

/// The host side of one stream's decoder.
pub struct Front<A: Accel> {
    accel: A,
    parser: HostParser,
    config: Vec<u8>,
    /// The surface (index) in each reference slot.
    slots: [Option<usize>; 8],
    size: (u32, u32),
    bits: u32,
}

impl<A: Accel> Front<A> {
    /// A decoder on `accel`, whose surfaces hold `size` pictures of `bits` bits, primed with the
    /// sample entry's configuration OBUs.
    pub fn new(accel: A, size: (u32, u32), bits: u32, config: &[u8]) -> Result<Self> {
        let mut parser = HostParser::new();
        parser.parse(config).map_err(decode_err)?;
        Ok(Self { accel, parser, config: config.to_vec(), slots: [None; 8], size, bits })
    }

    /// Forget the stream, for a seek: decoding restarts at a key frame.
    pub fn reset(&mut self) {
        self.parser = HostParser::new();
        // the same OBUs parsed in `new`; if they fail now, the next frame is an error
        let _ = self.parser.parse(&self.config);
        self.slots = [None; 8];
    }

    /// Decode a temporal unit; the pictures it shows come back at once.
    pub fn decode(&mut self, sample: &[u8], pts: i64) -> Result<Vec<DecodedFrame>> {
        let mut out = Vec::new();
        for event in self.parser.parse(sample).map_err(decode_err)? {
            match event {
                HostEvent::Frame(frame) => {
                    if let Some(frame) = self.decode_frame(&frame)? {
                        out.push(DecodedFrame { pts, frame, draft: false });
                    }
                }
                HostEvent::ShowExisting { slot, key_frame, grain } => {
                    if grain {
                        // VA-API applies grain only as part of decoding a frame
                        return Err(decode_err("film grain on a repeated frame"));
                    }
                    let surface = self.slots.get(slot).copied().flatten().ok_or_else(|| decode_err("show_existing_frame of an empty slot"))?;
                    out.push(DecodedFrame { pts, frame: self.accel.read(surface).map_err(decode_err)?, draft: false });
                    if key_frame {
                        self.slots = [Some(surface); 8];
                    }
                }
            }
        }
        Ok(out)
    }

    fn decode_frame(&mut self, frame: &HostFrame) -> Result<Option<filmcraft_frame::VideoFrame>> {
        let (seq, h) = (&frame.sequence, &frame.header);
        if (h.upscaled_width, h.frame_height) != self.size || u32::from(seq.color.bit_depth) != self.bits {
            return Err(decode_err(format!(
                "a {}x{} {}-bit frame in a {}x{} {}-bit session",
                h.upscaled_width, h.frame_height, seq.color.bit_depth, self.size.0, self.size.1, self.bits
            )));
        }
        if !h.frame_is_intra && h.ref_frame_idx.iter().any(|&i| self.slots.get(i).copied().flatten().is_none()) {
            return Err(decode_err("missing reference frame"));
        }
        let held: HashSet<usize> = self.slots.iter().flatten().copied().collect();
        let mut free = (0..self.accel.surfaces().len()).filter(|i| !held.contains(i));
        let target = free.next().ok_or_else(|| decode_err("no free surface"))?;
        let grain = seq.film_grain_params_present && h.film_grain.apply_grain;
        let display = if grain { free.next().ok_or_else(|| decode_err("no free surface"))? } else { target };
        let surfaces = self.accel.surfaces();
        let id = |i: usize| surfaces.get(i).copied().ok_or_else(|| decode_err("surface out of range"));
        let mut refs = [ffi::VA_INVALID_SURFACE; 8];
        for (r, slot) in refs.iter_mut().zip(&self.slots) {
            if let Some(i) = slot {
                *r = id(*i)?;
            }
        }
        let pic = picture_parameters(seq, h, id(target)?, id(display)?, refs);
        let tiles = tile_parameters(frame)?;
        self.accel.decode_av1(target, &pic, &tiles).map_err(decode_err)?;
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if (h.refresh_frame_flags >> i) & 1 == 1 {
                *slot = Some(target);
            }
        }
        if !h.show_frame {
            return Ok(None);
        }
        self.accel.read(display).map(Some).map_err(decode_err)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bit_fields_are_packed_from_the_low_end() {
        assert_eq!(bits(&[(1, 1), (0, 1), (3, 2)]), 0b1101);
        assert_eq!(bits(&[(0xff, 2)]), 3, "a value is cut to its width");
        assert_eq!(bits(&[(1, 31), (1, 1), (1, 1)]), 0x8000_0001, "past 32 bits nothing is written and nothing panics");
    }

    #[test]
    fn tile_sizes_round_a_partial_last_superblock_up() {
        let sizes = tile_sizes(&[0, 16, 32, 40], 16);
        assert_eq!(sizes.get(..3), Some(&[0u16, 0, 0][..]));
        assert_eq!(tile_sizes(&[0, 48, 50], 16).get(..2), Some(&[2u16, 0][..]));
        assert_eq!(tile_sizes(&[5, 2], 16).first(), Some(&0), "hostile starts do not wrap");
    }

    #[test]
    fn a_secondary_strength_of_four_is_coded_as_three() {
        assert_eq!(cdef_strength(5, 4), (5 << 2) | 3);
        assert_eq!(cdef_strength(5, 2), (5 << 2) | 2);
        assert_eq!(cdef_strength(1000, 0), 0, "out of range is not truncated into a wrong strength");
    }
}

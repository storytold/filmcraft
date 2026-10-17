//! Sequence header (5.5) and uncompressed frame header (5.9) parsing.

use crate::bits::BitReader;
use crate::spec_tables::*;
use crate::{Error, Result};

pub const EIGHTTAP: u8 = 0;
pub const BILINEAR: u8 = 3;
pub const SWITCHABLE: u8 = 4;
/// `NONE` reference frame.
pub const NONE: i8 = -1;

/// Color configuration (5.5.2).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ColorConfig {
    pub bit_depth: u8,
    pub mono_chrome: bool,
    pub num_planes: usize,
    pub color_primaries: u8,
    pub transfer_characteristics: u8,
    pub matrix_coefficients: u8,
    pub color_range: bool,
    pub subsampling_x: u8,
    pub subsampling_y: u8,
    pub chroma_sample_position: u8,
    pub separate_uv_delta_q: bool,
}

/// A parsed sequence header OBU.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SequenceHeader {
    pub profile: u8,
    pub still_picture: bool,
    pub reduced_still_picture_header: bool,
    pub timing_info_present: bool,
    pub decoder_model_info_present: bool,
    pub equal_picture_interval: bool,
    pub buffer_removal_time_length_minus_1: u32,
    pub frame_presentation_time_length_minus_1: u32,
    pub operating_points_cnt_minus_1: usize,
    pub operating_point_idc: [u32; 32],
    pub seq_level_idx: [u8; 32],
    pub decoder_model_present_for_this_op: [bool; 32],
    /// OperatingPointIdc of the chosen operating point (0).
    pub op_idc: u32,
    pub frame_width_bits: u32,
    pub frame_height_bits: u32,
    pub max_frame_width: u32,
    pub max_frame_height: u32,
    pub frame_id_numbers_present: bool,
    pub delta_frame_id_length_minus_2: u32,
    pub additional_frame_id_length_minus_1: u32,
    pub use_128x128_superblock: bool,
    pub enable_filter_intra: bool,
    pub enable_intra_edge_filter: bool,
    pub enable_interintra_compound: bool,
    pub enable_masked_compound: bool,
    pub enable_warped_motion: bool,
    pub enable_dual_filter: bool,
    pub enable_order_hint: bool,
    pub enable_jnt_comp: bool,
    pub enable_ref_frame_mvs: bool,
    pub seq_force_screen_content_tools: u32,
    pub seq_force_integer_mv: u32,
    pub order_hint_bits: u32,
    pub enable_superres: bool,
    pub enable_cdef: bool,
    pub enable_restoration: bool,
    pub color: ColorConfig,
    pub film_grain_params_present: bool,
}

impl SequenceHeader {
    pub fn parse(data: &[u8]) -> Result<SequenceHeader> {
        let mut r = BitReader::new(data);
        Self::parse_bits(&mut r)
    }

    /// Parse a bounded sequence OBU payload, including its required trailing bits (5.3.4).
    /// The ordinary decoder parser remains tolerant of trailing bits for existing callers.
    pub fn parse_checked(data: &[u8]) -> Result<SequenceHeader> {
        if data.len() > 65_536 {
            return Err(Error::Unsupported("sequence header exceeds 64 KiB inspection limit"));
        }
        let mut r = BitReader::new(data);
        let h = Self::parse_bits(&mut r)?;
        if !r.flag()? {
            return Err(Error::Invalid("sequence trailing_one_bit"));
        }
        while r.position() < data.len().saturating_mul(8) {
            if r.flag()? {
                return Err(Error::Invalid("sequence trailing_zero_bit"));
            }
        }
        if h.reduced_still_picture_header && !h.still_picture {
            return Err(Error::Invalid("reduced still header without still_picture"));
        }
        Ok(h)
    }

    fn parse_bits(r: &mut BitReader) -> Result<SequenceHeader> {
        let mut s = SequenceHeader { profile: r.f(3)? as u8, ..Default::default() };
        if s.profile > 2 {
            return Err(Error::Unsupported("seq_profile > 2"));
        }
        s.still_picture = r.flag()?;
        s.reduced_still_picture_header = r.flag()?;
        if s.reduced_still_picture_header {
            s.seq_level_idx[0] = r.f(5)? as u8;
        } else {
            s.timing_info_present = r.flag()?;
            let mut buffer_delay_length_minus_1 = 0;
            if s.timing_info_present {
                r.f(32)?; // num_units_in_display_tick
                r.f(32)?; // time_scale
                s.equal_picture_interval = r.flag()?;
                if s.equal_picture_interval {
                    r.uvlc()?;
                }
                s.decoder_model_info_present = r.flag()?;
                if s.decoder_model_info_present {
                    buffer_delay_length_minus_1 = r.f(5)?;
                    r.f(32)?; // num_units_in_decoding_tick
                    s.buffer_removal_time_length_minus_1 = r.f(5)?;
                    s.frame_presentation_time_length_minus_1 = r.f(5)?;
                }
            }
            let initial_display_delay_present = r.flag()?;
            s.operating_points_cnt_minus_1 = r.f(5)? as usize;
            for i in 0..=s.operating_points_cnt_minus_1 {
                s.operating_point_idc[i] = r.f(12)?;
                s.seq_level_idx[i] = r.f(5)? as u8;
                if s.seq_level_idx[i] > 7 {
                    r.f(1)?; // seq_tier
                }
                if s.decoder_model_info_present {
                    s.decoder_model_present_for_this_op[i] = r.flag()?;
                    if s.decoder_model_present_for_this_op[i] {
                        let n = buffer_delay_length_minus_1 + 1;
                        r.f(n)?;
                        r.f(n)?;
                        r.f(1)?;
                    }
                }
                if initial_display_delay_present && r.flag()? {
                    r.f(4)?;
                }
            }
        }
        s.op_idc = s.operating_point_idc[0];
        s.frame_width_bits = r.f(4)? + 1;
        s.frame_height_bits = r.f(4)? + 1;
        s.max_frame_width = r.f(s.frame_width_bits)? + 1;
        s.max_frame_height = r.f(s.frame_height_bits)? + 1;
        s.frame_id_numbers_present = if s.reduced_still_picture_header { false } else { r.flag()? };
        if s.frame_id_numbers_present {
            s.delta_frame_id_length_minus_2 = r.f(4)?;
            s.additional_frame_id_length_minus_1 = r.f(3)?;
        }
        s.use_128x128_superblock = r.flag()?;
        s.enable_filter_intra = r.flag()?;
        s.enable_intra_edge_filter = r.flag()?;
        if s.reduced_still_picture_header {
            s.seq_force_screen_content_tools = SELECT_SCREEN_CONTENT_TOOLS as u32;
            s.seq_force_integer_mv = SELECT_INTEGER_MV as u32;
        } else {
            s.enable_interintra_compound = r.flag()?;
            s.enable_masked_compound = r.flag()?;
            s.enable_warped_motion = r.flag()?;
            s.enable_dual_filter = r.flag()?;
            s.enable_order_hint = r.flag()?;
            if s.enable_order_hint {
                s.enable_jnt_comp = r.flag()?;
                s.enable_ref_frame_mvs = r.flag()?;
            }
            s.seq_force_screen_content_tools = if r.flag()? { SELECT_SCREEN_CONTENT_TOOLS as u32 } else { r.f(1)? };
            if s.seq_force_screen_content_tools > 0 {
                s.seq_force_integer_mv = if r.flag()? { SELECT_INTEGER_MV as u32 } else { r.f(1)? };
            } else {
                s.seq_force_integer_mv = SELECT_INTEGER_MV as u32;
            }
            if s.enable_order_hint {
                s.order_hint_bits = r.f(3)? + 1;
            }
        }
        s.enable_superres = r.flag()?;
        s.enable_cdef = r.flag()?;
        s.enable_restoration = r.flag()?;
        s.color = parse_color_config(r, s.profile)?;
        s.film_grain_params_present = r.flag()?;
        Ok(s)
    }
}

fn parse_color_config(r: &mut BitReader, profile: u8) -> Result<ColorConfig> {
    let mut c = ColorConfig::default();
    let high = r.flag()?;
    c.bit_depth = if profile == 2 && high {
        if r.flag()? { 12 } else { 10 }
    } else if high {
        10
    } else {
        8
    };
    c.mono_chrome = if profile == 1 { false } else { r.flag()? };
    c.num_planes = if c.mono_chrome { 1 } else { 3 };
    if r.flag()? {
        c.color_primaries = r.f(8)? as u8;
        c.transfer_characteristics = r.f(8)? as u8;
        c.matrix_coefficients = r.f(8)? as u8;
    } else {
        c.color_primaries = 2;
        c.transfer_characteristics = 2;
        c.matrix_coefficients = 2;
    }
    if c.mono_chrome {
        c.color_range = r.flag()?;
        c.subsampling_x = 1;
        c.subsampling_y = 1;
        return Ok(c);
    } else if c.color_primaries == 1 && c.transfer_characteristics == 13 && c.matrix_coefficients == 0 {
        c.color_range = true;
    } else {
        c.color_range = r.flag()?;
        match profile {
            0 => {
                c.subsampling_x = 1;
                c.subsampling_y = 1;
            }
            1 => {}
            _ => {
                if c.bit_depth == 12 {
                    c.subsampling_x = r.f(1)? as u8;
                    c.subsampling_y = if c.subsampling_x == 1 { r.f(1)? as u8 } else { 0 };
                } else {
                    c.subsampling_x = 1;
                }
            }
        }
        if c.subsampling_x == 1 && c.subsampling_y == 1 {
            c.chroma_sample_position = r.f(2)? as u8;
        }
    }
    c.separate_uv_delta_q = r.flag()?;
    Ok(c)
}

/// Tile layout (5.9.15).
#[derive(Debug, Clone, Default)]
pub struct TileInfo {
    pub cols: usize,
    pub rows: usize,
    pub cols_log2: u32,
    pub rows_log2: u32,
    pub mi_col_starts: Vec<u32>,
    pub mi_row_starts: Vec<u32>,
    pub context_update_tile_id: u32,
    pub tile_size_bytes: u32,
}

#[derive(Debug, Clone, Default)]
pub struct QuantParams {
    pub base_q_idx: u32,
    pub delta_q_y_dc: i32,
    pub delta_q_u_dc: i32,
    pub delta_q_u_ac: i32,
    pub delta_q_v_dc: i32,
    pub delta_q_v_ac: i32,
    pub using_qmatrix: bool,
    pub qm_y: u32,
    pub qm_u: u32,
    pub qm_v: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SegmentationFeatures {
    pub enabled: [[bool; SEG_LVL_MAX]; MAX_SEGMENTS],
    pub data: [[i32; SEG_LVL_MAX]; MAX_SEGMENTS],
}

#[derive(Debug, Clone, Default)]
pub struct Segmentation {
    pub enabled: bool,
    pub update_map: bool,
    pub temporal_update: bool,
    pub update_data: bool,
    pub features: SegmentationFeatures,
    pub seg_id_pre_skip: bool,
    pub last_active_seg_id: u32,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct LoopFilterDeltas {
    pub ref_deltas: [i32; TOTAL_REFS_PER_FRAME],
    pub mode_deltas: [i32; 2],
}

impl LoopFilterDeltas {
    pub fn defaults() -> Self {
        LoopFilterDeltas { ref_deltas: [1, 0, 0, 0, -1, 0, -1, -1], mode_deltas: [0, 0] }
    }
}

#[derive(Debug, Clone, Default)]
pub struct LoopFilterParams {
    pub level: [u32; 4],
    pub sharpness: u32,
    pub delta_enabled: bool,
    pub delta_update: bool,
    pub deltas: LoopFilterDeltas,
}

#[derive(Debug, Clone, Default)]
pub struct CdefParams {
    pub damping: u32,
    pub bits: u32,
    pub y_pri: [u32; 8],
    pub y_sec: [u32; 8],
    pub uv_pri: [u32; 8],
    pub uv_sec: [u32; 8],
}

#[derive(Debug, Clone, Default)]
pub struct LrParams {
    pub frame_restoration_type: [u8; 3],
    pub uses_lr: bool,
    pub loop_restoration_size: [u32; 3],
}

/// Film grain parameters (5.9.30).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FilmGrainParams {
    pub apply_grain: bool,
    pub grain_seed: u32,
    pub update_grain: bool,
    pub num_y_points: usize,
    pub point_y_value: [u8; 16],
    pub point_y_scaling: [u8; 16],
    pub chroma_scaling_from_luma: bool,
    pub num_cb_points: usize,
    pub point_cb_value: [u8; 16],
    pub point_cb_scaling: [u8; 16],
    pub num_cr_points: usize,
    pub point_cr_value: [u8; 16],
    pub point_cr_scaling: [u8; 16],
    pub grain_scaling_minus_8: u32,
    pub ar_coeff_lag: u32,
    pub ar_coeffs_y_plus_128: [u8; 24],
    pub ar_coeffs_cb_plus_128: [u8; 25],
    pub ar_coeffs_cr_plus_128: [u8; 25],
    pub ar_coeff_shift_minus_6: u32,
    pub grain_scale_shift: u32,
    pub cb_mult: u32,
    pub cb_luma_mult: u32,
    pub cb_offset: u32,
    pub cr_mult: u32,
    pub cr_luma_mult: u32,
    pub cr_offset: u32,
    pub overlap_flag: bool,
    pub clip_to_restricted_range: bool,
}

/// The values of one reference slot that the frame header needs (7.20).
#[derive(Debug, Clone, Default)]
pub struct RefInfo {
    pub valid: bool,
    pub frame_id: u32,
    pub upscaled_width: u32,
    pub frame_width: u32,
    pub frame_height: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub mi_cols: u32,
    pub mi_rows: u32,
    pub frame_type: u8,
    pub order_hint: u32,
    pub saved_order_hints: [u32; 8],
    pub gm_params: [[i32; 6]; 8],
    pub lf_deltas: LoopFilterDeltas,
    pub seg_features: SegmentationFeatures,
    pub grain: FilmGrainParams,
}

/// A parsed uncompressed frame header with its derived variables.
#[derive(Debug, Clone, Default)]
pub struct FrameHeader {
    pub show_existing_frame: bool,
    pub frame_to_show_map_idx: usize,
    pub frame_type: u8,
    pub frame_is_intra: bool,
    pub show_frame: bool,
    pub showable_frame: bool,
    pub error_resilient_mode: bool,
    pub disable_cdf_update: bool,
    pub allow_screen_content_tools: bool,
    pub force_integer_mv: bool,
    pub current_frame_id: u32,
    pub frame_size_override_flag: bool,
    pub order_hint: u32,
    pub primary_ref_frame: usize,
    pub refresh_frame_flags: u32,
    pub allow_intrabc: bool,
    pub ref_frame_idx: [usize; REFS_PER_FRAME],
    pub allow_high_precision_mv: bool,
    pub interpolation_filter: u8,
    pub is_motion_mode_switchable: bool,
    pub use_ref_frame_mvs: bool,
    /// OrderHints[ LAST_FRAME..ALTREF_FRAME ] (index 0 unused).
    pub order_hints: [u32; 8],
    pub ref_frame_sign_bias: [bool; 8],
    pub disable_frame_end_update_cdf: bool,
    pub frame_width: u32,
    pub frame_height: u32,
    pub upscaled_width: u32,
    pub render_width: u32,
    pub render_height: u32,
    pub use_superres: bool,
    pub superres_denom: u32,
    pub mi_cols: u32,
    pub mi_rows: u32,
    pub tile_info: TileInfo,
    pub quant: QuantParams,
    pub seg: Segmentation,
    pub delta_q_present: bool,
    pub delta_q_res: u32,
    pub delta_lf_present: bool,
    pub delta_lf_res: u32,
    pub delta_lf_multi: bool,
    pub coded_lossless: bool,
    pub all_lossless: bool,
    pub lossless_array: [bool; MAX_SEGMENTS],
    pub seg_qm_level: [[u32; MAX_SEGMENTS]; 3],
    pub lf: LoopFilterParams,
    pub cdef: CdefParams,
    pub lr: LrParams,
    pub tx_mode: u8,
    pub reference_select: bool,
    pub skip_mode_present: bool,
    pub skip_mode_frame: [usize; 2],
    pub allow_warped_motion: bool,
    pub reduced_tx_set: bool,
    pub gm_type: [u8; 8],
    pub gm_params: [[i32; 6]; 8],
    pub prev_gm_params: [[i32; 6]; 8],
    pub film_grain: FilmGrainParams,
    /// Temporal / spatial id of the OBU carrying the header.
    pub temporal_id: u32,
    pub spatial_id: u32,
}

impl FrameHeader {
    pub fn get_relative_dist(&self, seq: &SequenceHeader, a: u32, b: u32) -> i32 {
        relative_dist(seq, a, b)
    }

    /// get_qindex( ignoreDeltaQ, segmentId ) with CurrentQIndex.
    pub fn get_qindex(&self, ignore_delta_q: bool, segment_id: usize, current_q_index: i32) -> i32 {
        let base = self.quant.base_q_idx as i32;
        if self.seg.enabled && self.seg.features.enabled[segment_id][SEG_LVL_ALT_Q] {
            let data = self.seg.features.data[segment_id][SEG_LVL_ALT_Q];
            let mut q = base + data;
            if !ignore_delta_q && self.delta_q_present {
                q = current_q_index + data;
            }
            q.clamp(0, 255)
        } else if !ignore_delta_q && self.delta_q_present {
            current_q_index
        } else {
            base
        }
    }
}

pub(crate) fn relative_dist(seq: &SequenceHeader, a: u32, b: u32) -> i32 {
    if !seq.enable_order_hint {
        return 0;
    }
    let diff = a as i32 - b as i32;
    let m = 1i32 << (seq.order_hint_bits - 1);
    (diff & (m - 1)) - (diff & m)
}

/// Decoder state the frame header reads and updates across frames.
pub struct HeaderState<'a> {
    pub seq: &'a SequenceHeader,
    pub refs: &'a mut [RefInfo; NUM_REF_FRAMES],
    /// Loop filter deltas carried from the previous frame (setup_past_independence / load_previous).
    pub lf_deltas: LoopFilterDeltas,
    pub seg_features: SegmentationFeatures,
    pub prev_gm_params: [[i32; 6]; 8],
    pub current_frame_id: u32,
}

fn tile_log2(blk: u32, target: u32) -> u32 {
    let mut k = 0;
    while (blk << k) < target {
        k += 1;
    }
    k
}

impl FrameHeader {
    /// uncompressed_header( ). `st` holds the cross-frame state.
    pub(crate) fn parse(r: &mut BitReader, st: &mut HeaderState, temporal_id: u32, spatial_id: u32) -> Result<FrameHeader> {
        let seq = st.seq;
        let mut h = FrameHeader { temporal_id, spatial_id, ..Default::default() };
        let id_len = if seq.frame_id_numbers_present { seq.additional_frame_id_length_minus_1 + seq.delta_frame_id_length_minus_2 + 3 } else { 0 };
        let all_frames = (1u32 << NUM_REF_FRAMES) - 1;
        if seq.reduced_still_picture_header {
            h.frame_type = KEY_FRAME as u8;
            h.frame_is_intra = true;
            h.show_frame = true;
        } else {
            h.show_existing_frame = r.flag()?;
            if h.show_existing_frame {
                h.frame_to_show_map_idx = r.f(3)? as usize;
                if seq.decoder_model_info_present && !seq.equal_picture_interval {
                    r.f(seq.frame_presentation_time_length_minus_1 + 1)?;
                }
                if seq.frame_id_numbers_present {
                    r.f(id_len)?;
                }
                let rs = &st.refs[h.frame_to_show_map_idx];
                if !rs.valid {
                    return Err(Error::Invalid("show_existing_frame of an empty slot"));
                }
                h.frame_type = rs.frame_type;
                if h.frame_type == KEY_FRAME as u8 {
                    h.refresh_frame_flags = all_frames;
                }
                if seq.film_grain_params_present {
                    h.film_grain = rs.grain.clone();
                }
                return Ok(h);
            }
            h.frame_type = r.f(2)? as u8;
            h.frame_is_intra = h.frame_type == INTRA_ONLY_FRAME as u8 || h.frame_type == KEY_FRAME as u8;
            h.show_frame = r.flag()?;
            if h.show_frame && seq.decoder_model_info_present && !seq.equal_picture_interval {
                r.f(seq.frame_presentation_time_length_minus_1 + 1)?;
            }
            h.showable_frame = if h.show_frame { h.frame_type != KEY_FRAME as u8 } else { r.flag()? };
            h.error_resilient_mode = if h.frame_type == SWITCH_FRAME as u8 || (h.frame_type == KEY_FRAME as u8 && h.show_frame) { true } else { r.flag()? };
        }
        if h.frame_type == KEY_FRAME as u8 && h.show_frame {
            for rs in st.refs.iter_mut() {
                rs.valid = false;
                rs.order_hint = 0;
            }
            for i in 0..REFS_PER_FRAME {
                h.order_hints[LAST_FRAME + i] = 0;
            }
        }
        h.disable_cdf_update = r.flag()?;
        h.allow_screen_content_tools =
            if seq.seq_force_screen_content_tools == SELECT_SCREEN_CONTENT_TOOLS as u32 { r.flag()? } else { seq.seq_force_screen_content_tools != 0 };
        if h.allow_screen_content_tools {
            h.force_integer_mv = if seq.seq_force_integer_mv == SELECT_INTEGER_MV as u32 { r.flag()? } else { seq.seq_force_integer_mv != 0 };
        }
        if h.frame_is_intra {
            h.force_integer_mv = true;
        }
        if seq.frame_id_numbers_present {
            h.current_frame_id = r.f(id_len)?;
            // mark_ref_frames( idLen )
            let diff_len = seq.delta_frame_id_length_minus_2 + 2;
            let cur = h.current_frame_id;
            for rs in st.refs.iter_mut() {
                if cur > (1 << diff_len) {
                    if rs.frame_id > cur || rs.frame_id < cur - (1 << diff_len) {
                        rs.valid = false;
                    }
                } else if rs.frame_id > cur && rs.frame_id < (1 << id_len) + cur - (1 << diff_len) {
                    rs.valid = false;
                }
            }
            st.current_frame_id = cur;
        }
        h.frame_size_override_flag = if h.frame_type == SWITCH_FRAME as u8 {
            true
        } else if seq.reduced_still_picture_header {
            false
        } else {
            r.flag()?
        };
        h.order_hint = r.f(seq.order_hint_bits)?;
        h.primary_ref_frame = if h.frame_is_intra || h.error_resilient_mode { PRIMARY_REF_NONE } else { r.f(3)? as usize };
        if seq.decoder_model_info_present && r.flag()? {
            for op in 0..=seq.operating_points_cnt_minus_1 {
                if seq.decoder_model_present_for_this_op[op] {
                    let idc = seq.operating_point_idc[op];
                    let in_t = (idc >> temporal_id) & 1;
                    let in_s = (idc >> (spatial_id + 8)) & 1;
                    if idc == 0 || (in_t == 1 && in_s == 1) {
                        r.f(seq.buffer_removal_time_length_minus_1 + 1)?;
                    }
                }
            }
        }
        h.refresh_frame_flags = if h.frame_type == SWITCH_FRAME as u8 || (h.frame_type == KEY_FRAME as u8 && h.show_frame) { all_frames } else { r.f(8)? };
        if (!h.frame_is_intra || h.refresh_frame_flags != all_frames) && h.error_resilient_mode && seq.enable_order_hint {
            for rs in st.refs.iter_mut() {
                let roh = r.f(seq.order_hint_bits)?;
                if roh != rs.order_hint {
                    rs.valid = false;
                }
            }
        }
        if h.frame_is_intra {
            h.frame_size(r, seq)?;
            h.render_size(r)?;
            if h.allow_screen_content_tools && h.upscaled_width == h.frame_width {
                h.allow_intrabc = r.flag()?;
            }
        } else {
            let mut short = false;
            if seq.enable_order_hint {
                short = r.flag()?;
                if short {
                    let last = r.f(3)? as usize;
                    let gold = r.f(3)? as usize;
                    h.set_frame_refs(seq, st.refs, last, gold);
                }
            }
            for i in 0..REFS_PER_FRAME {
                if !short {
                    h.ref_frame_idx[i] = r.f(3)? as usize;
                }
                if seq.frame_id_numbers_present {
                    r.f(seq.delta_frame_id_length_minus_2 + 2)?;
                }
            }
            for i in 0..REFS_PER_FRAME {
                if !st.refs[h.ref_frame_idx[i]].valid {
                    return Err(Error::Invalid("reference to an empty slot"));
                }
            }
            if h.frame_size_override_flag && !h.error_resilient_mode {
                // frame_size_with_refs( )
                let mut found = false;
                for i in 0..REFS_PER_FRAME {
                    if r.flag()? {
                        let rs = &st.refs[h.ref_frame_idx[i]];
                        h.upscaled_width = rs.upscaled_width;
                        h.frame_width = h.upscaled_width;
                        h.frame_height = rs.frame_height;
                        h.render_width = rs.render_width;
                        h.render_height = rs.render_height;
                        found = true;
                        break;
                    }
                }
                if !found {
                    h.frame_size(r, seq)?;
                    h.render_size(r)?;
                } else {
                    h.superres_params(r, seq)?;
                    h.compute_image_size();
                }
            } else {
                h.frame_size(r, seq)?;
                h.render_size(r)?;
            }
            h.allow_high_precision_mv = if h.force_integer_mv { false } else { r.flag()? };
            h.interpolation_filter = if r.flag()? { SWITCHABLE } else { r.f(2)? as u8 };
            h.is_motion_mode_switchable = r.flag()?;
            h.use_ref_frame_mvs = if h.error_resilient_mode || !seq.enable_ref_frame_mvs { false } else { r.flag()? };
            for i in 0..REFS_PER_FRAME {
                let rf = LAST_FRAME + i;
                let hint = st.refs[h.ref_frame_idx[i]].order_hint;
                h.order_hints[rf] = hint;
                h.ref_frame_sign_bias[rf] = seq.enable_order_hint && relative_dist(seq, hint, h.order_hint) > 0;
            }
        }
        h.disable_frame_end_update_cdf = if seq.reduced_still_picture_header || h.disable_cdf_update { true } else { r.flag()? };
        if h.primary_ref_frame == PRIMARY_REF_NONE {
            // setup_past_independence( )
            st.seg_features = SegmentationFeatures::default();
            st.prev_gm_params = default_gm_params();
            st.lf_deltas = LoopFilterDeltas::defaults();
            h.lf.delta_enabled = true;
        } else {
            // load_previous( )
            let prev = &st.refs[h.ref_frame_idx[h.primary_ref_frame]];
            st.prev_gm_params = prev.gm_params;
            st.lf_deltas = prev.lf_deltas;
            st.seg_features = prev.seg_features;
        }
        h.prev_gm_params = st.prev_gm_params;
        h.tile_info(r, seq)?;
        h.quantization_params(r, seq)?;
        h.segmentation_params(r, st)?;
        // delta_q_params( )
        if h.quant.base_q_idx > 0 {
            h.delta_q_present = r.flag()?;
        }
        if h.delta_q_present {
            h.delta_q_res = r.f(2)?;
        }
        // delta_lf_params( )
        if h.delta_q_present {
            if !h.allow_intrabc {
                h.delta_lf_present = r.flag()?;
            }
            if h.delta_lf_present {
                h.delta_lf_res = r.f(2)?;
                h.delta_lf_multi = r.flag()?;
            }
        }
        h.coded_lossless = true;
        for seg_id in 0..MAX_SEGMENTS {
            let q = h.get_qindex(true, seg_id, 0);
            let ll = q == 0
                && h.quant.delta_q_y_dc == 0
                && h.quant.delta_q_u_ac == 0
                && h.quant.delta_q_u_dc == 0
                && h.quant.delta_q_v_ac == 0
                && h.quant.delta_q_v_dc == 0;
            h.lossless_array[seg_id] = ll;
            if !ll {
                h.coded_lossless = false;
            }
            if h.quant.using_qmatrix {
                if ll {
                    h.seg_qm_level[0][seg_id] = 15;
                    h.seg_qm_level[1][seg_id] = 15;
                    h.seg_qm_level[2][seg_id] = 15;
                } else {
                    h.seg_qm_level[0][seg_id] = h.quant.qm_y;
                    h.seg_qm_level[1][seg_id] = h.quant.qm_u;
                    h.seg_qm_level[2][seg_id] = h.quant.qm_v;
                }
            }
        }
        h.all_lossless = h.coded_lossless && h.frame_width == h.upscaled_width;
        h.loop_filter_params(r, seq, st)?;
        h.cdef_params(r, seq)?;
        h.lr_params(r, seq)?;
        // read_tx_mode( )
        h.tx_mode = if h.coded_lossless {
            ONLY_4X4 as u8
        } else if r.flag()? {
            TX_MODE_SELECT as u8
        } else {
            TX_MODE_LARGEST as u8
        };
        h.reference_select = if h.frame_is_intra { false } else { r.flag()? };
        h.skip_mode_params(r, seq, st.refs)?;
        h.allow_warped_motion = if h.frame_is_intra || h.error_resilient_mode || !seq.enable_warped_motion { false } else { r.flag()? };
        h.reduced_tx_set = r.flag()?;
        h.global_motion_params(r)?;
        h.film_grain_params(r, seq, st.refs)?;
        Ok(h)
    }

    fn superres_params(&mut self, r: &mut BitReader, seq: &SequenceHeader) -> Result<()> {
        self.use_superres = if seq.enable_superres { r.flag()? } else { false };
        self.superres_denom = if self.use_superres { r.f(SUPERRES_DENOM_BITS as u32)? + SUPERRES_DENOM_MIN as u32 } else { SUPERRES_NUM as u32 };
        self.upscaled_width = self.frame_width;
        self.frame_width = (self.upscaled_width * SUPERRES_NUM as u32 + self.superres_denom / 2) / self.superres_denom;
        Ok(())
    }

    fn compute_image_size(&mut self) {
        self.mi_cols = 2 * ((self.frame_width + 7) >> 3);
        self.mi_rows = 2 * ((self.frame_height + 7) >> 3);
    }

    fn frame_size(&mut self, r: &mut BitReader, seq: &SequenceHeader) -> Result<()> {
        if self.frame_size_override_flag {
            self.frame_width = r.f(seq.frame_width_bits)? + 1;
            self.frame_height = r.f(seq.frame_height_bits)? + 1;
        } else {
            self.frame_width = seq.max_frame_width;
            self.frame_height = seq.max_frame_height;
        }
        self.superres_params(r, seq)?;
        self.compute_image_size();
        Ok(())
    }

    fn render_size(&mut self, r: &mut BitReader) -> Result<()> {
        if r.flag()? {
            self.render_width = r.f(16)? + 1;
            self.render_height = r.f(16)? + 1;
        } else {
            self.render_width = self.upscaled_width;
            self.render_height = self.frame_height;
        }
        Ok(())
    }

    /// Set frame refs process (7.8).
    fn set_frame_refs(&mut self, seq: &SequenceHeader, refs: &[RefInfo; NUM_REF_FRAMES], last: usize, gold: usize) {
        let mut idx = [-1i32; REFS_PER_FRAME];
        idx[0] = last as i32;
        idx[GOLDEN_FRAME - LAST_FRAME] = gold as i32;
        let mut used = [false; NUM_REF_FRAMES];
        used[last] = true;
        used[gold] = true;
        let cur = 1i32 << (seq.order_hint_bits - 1);
        let shifted: Vec<i32> = (0..NUM_REF_FRAMES).map(|i| cur + relative_dist(seq, refs[i].order_hint, self.order_hint)).collect();
        // ALTREF: latest backward
        {
            let mut rf = -1i32;
            let mut latest = 0;
            for i in 0..NUM_REF_FRAMES {
                let hint = shifted[i];
                if !used[i] && hint >= cur && (rf < 0 || hint >= latest) {
                    rf = i as i32;
                    latest = hint;
                }
            }
            if rf >= 0 {
                idx[ALTREF_FRAME - LAST_FRAME] = rf;
                used[rf as usize] = true;
            }
        }
        // BWDREF then ALTREF2: earliest backward
        for target in [BWDREF_FRAME, ALTREF2_FRAME] {
            let mut rf = -1i32;
            let mut earliest = 0;
            for i in 0..NUM_REF_FRAMES {
                let hint = shifted[i];
                if !used[i] && hint >= cur && (rf < 0 || hint < earliest) {
                    rf = i as i32;
                    earliest = hint;
                }
            }
            if rf >= 0 {
                idx[target - LAST_FRAME] = rf;
                used[rf as usize] = true;
            }
        }
        for &rf_frame in REF_FRAME_LIST.iter() {
            let rf_frame = rf_frame as usize;
            if idx[rf_frame - LAST_FRAME] < 0 {
                let mut rf = -1i32;
                let mut latest = 0;
                for i in 0..NUM_REF_FRAMES {
                    let hint = shifted[i];
                    if !used[i] && hint < cur && (rf < 0 || hint >= latest) {
                        rf = i as i32;
                        latest = hint;
                    }
                }
                if rf >= 0 {
                    idx[rf_frame - LAST_FRAME] = rf;
                    used[rf as usize] = true;
                }
            }
        }
        let mut rf = -1i32;
        let mut earliest = 0;
        for i in 0..NUM_REF_FRAMES {
            let hint = shifted[i];
            if rf < 0 || hint < earliest {
                rf = i as i32;
                earliest = hint;
            }
        }
        for i in 0..REFS_PER_FRAME {
            if idx[i] < 0 {
                idx[i] = rf;
            }
            self.ref_frame_idx[i] = idx[i] as usize;
        }
    }

    fn tile_info(&mut self, r: &mut BitReader, seq: &SequenceHeader) -> Result<()> {
        let sb128 = seq.use_128x128_superblock;
        let sb_cols = if sb128 { (self.mi_cols + 31) >> 5 } else { (self.mi_cols + 15) >> 4 };
        let sb_rows = if sb128 { (self.mi_rows + 31) >> 5 } else { (self.mi_rows + 15) >> 4 };
        let sb_shift = if sb128 { 5 } else { 4 };
        let sb_size = sb_shift + 2;
        let max_tile_width_sb = MAX_TILE_WIDTH as u32 >> sb_size;
        let mut max_tile_area_sb = MAX_TILE_AREA as u32 >> (2 * sb_size);
        let min_log2_tile_cols = tile_log2(max_tile_width_sb, sb_cols);
        let max_log2_tile_cols = tile_log2(1, sb_cols.min(MAX_TILE_COLS as u32));
        let max_log2_tile_rows = tile_log2(1, sb_rows.min(MAX_TILE_ROWS as u32));
        let min_log2_tiles = min_log2_tile_cols.max(tile_log2(max_tile_area_sb, sb_rows * sb_cols));
        let t = &mut self.tile_info;
        t.mi_col_starts.clear();
        t.mi_row_starts.clear();
        // At most one start per superblock plus the final boundary. Reserve fallibly before
        // parsing either layout so header inspection never relies on an infallible Vec growth.
        let columns = usize::try_from(sb_cols).ok().and_then(|n| n.checked_add(1)).ok_or(Error::Invalid("tile column count"))?;
        let rows = usize::try_from(sb_rows).ok().and_then(|n| n.checked_add(1)).ok_or(Error::Invalid("tile row count"))?;
        t.mi_col_starts.try_reserve(columns).map_err(|_| Error::Invalid("tile column allocation failed"))?;
        t.mi_row_starts.try_reserve(rows).map_err(|_| Error::Invalid("tile row allocation failed"))?;
        if r.flag()? {
            t.cols_log2 = min_log2_tile_cols;
            while t.cols_log2 < max_log2_tile_cols {
                if r.flag()? {
                    t.cols_log2 += 1;
                } else {
                    break;
                }
            }
            let tile_width_sb = (sb_cols + (1 << t.cols_log2) - 1) >> t.cols_log2;
            let mut start = 0;
            while start < sb_cols {
                t.mi_col_starts.push(start << sb_shift);
                start += tile_width_sb;
            }
            t.cols = t.mi_col_starts.len();
            t.mi_col_starts.push(self.mi_cols);
            let min_log2_tile_rows = min_log2_tiles.saturating_sub(t.cols_log2);
            t.rows_log2 = min_log2_tile_rows;
            while t.rows_log2 < max_log2_tile_rows {
                if r.flag()? {
                    t.rows_log2 += 1;
                } else {
                    break;
                }
            }
            let tile_height_sb = (sb_rows + (1 << t.rows_log2) - 1) >> t.rows_log2;
            let mut start = 0;
            while start < sb_rows {
                t.mi_row_starts.push(start << sb_shift);
                start += tile_height_sb;
            }
            t.rows = t.mi_row_starts.len();
            t.mi_row_starts.push(self.mi_rows);
        } else {
            let mut widest = 0;
            let mut start = 0;
            while start < sb_cols {
                t.mi_col_starts.push(start << sb_shift);
                let max_width = (sb_cols - start).min(max_tile_width_sb);
                let size = r.ns(max_width)? + 1;
                widest = widest.max(size);
                start += size;
            }
            t.cols = t.mi_col_starts.len();
            t.mi_col_starts.push(self.mi_cols);
            t.cols_log2 = tile_log2(1, t.cols as u32);
            if min_log2_tiles > 0 {
                max_tile_area_sb = (sb_rows * sb_cols) >> (min_log2_tiles + 1);
            } else {
                max_tile_area_sb = sb_rows * sb_cols;
            }
            let max_tile_height_sb = (max_tile_area_sb / widest.max(1)).max(1);
            let mut start = 0;
            while start < sb_rows {
                t.mi_row_starts.push(start << sb_shift);
                let max_height = (sb_rows - start).min(max_tile_height_sb);
                let size = r.ns(max_height)? + 1;
                start += size;
            }
            t.rows = t.mi_row_starts.len();
            t.mi_row_starts.push(self.mi_rows);
            t.rows_log2 = tile_log2(1, t.rows as u32);
        }
        if t.cols_log2 > 0 || t.rows_log2 > 0 {
            t.context_update_tile_id = r.f(t.rows_log2 + t.cols_log2)?;
            t.tile_size_bytes = r.f(2)? + 1;
        } else {
            t.context_update_tile_id = 0;
        }
        if t.context_update_tile_id as usize >= t.cols * t.rows {
            return Err(Error::Invalid("context_update_tile_id"));
        }
        Ok(())
    }

    fn quantization_params(&mut self, r: &mut BitReader, seq: &SequenceHeader) -> Result<()> {
        let q = &mut self.quant;
        q.base_q_idx = r.f(8)?;
        let read_delta_q = |r: &mut BitReader| -> Result<i32> { if r.flag()? { r.su(7) } else { Ok(0) } };
        q.delta_q_y_dc = read_delta_q(r)?;
        if seq.color.num_planes > 1 {
            let diff_uv_delta = if seq.color.separate_uv_delta_q { r.flag()? } else { false };
            q.delta_q_u_dc = read_delta_q(r)?;
            q.delta_q_u_ac = read_delta_q(r)?;
            if diff_uv_delta {
                q.delta_q_v_dc = read_delta_q(r)?;
                q.delta_q_v_ac = read_delta_q(r)?;
            } else {
                q.delta_q_v_dc = q.delta_q_u_dc;
                q.delta_q_v_ac = q.delta_q_u_ac;
            }
        }
        q.using_qmatrix = r.flag()?;
        if q.using_qmatrix {
            q.qm_y = r.f(4)?;
            q.qm_u = r.f(4)?;
            q.qm_v = if !seq.color.separate_uv_delta_q { q.qm_u } else { r.f(4)? };
        }
        Ok(())
    }

    fn segmentation_params(&mut self, r: &mut BitReader, st: &mut HeaderState) -> Result<()> {
        const BITS: [u32; SEG_LVL_MAX] = [8, 6, 6, 6, 6, 3, 0, 0];
        const SIGNED: [bool; SEG_LVL_MAX] = [true, true, true, true, true, false, false, false];
        const MAX: [i32; SEG_LVL_MAX] = [255, 63, 63, 63, 63, 7, 0, 0];
        let s = &mut self.seg;
        s.enabled = r.flag()?;
        if s.enabled {
            if self.primary_ref_frame == PRIMARY_REF_NONE {
                s.update_map = true;
                s.temporal_update = false;
                s.update_data = true;
            } else {
                s.update_map = r.flag()?;
                if s.update_map {
                    s.temporal_update = r.flag()?;
                }
                s.update_data = r.flag()?;
            }
            if s.update_data {
                for i in 0..MAX_SEGMENTS {
                    for j in 0..SEG_LVL_MAX {
                        let en = r.flag()?;
                        st.seg_features.enabled[i][j] = en;
                        let mut v = 0;
                        if en {
                            let bits = BITS[j];
                            let limit = MAX[j];
                            if SIGNED[j] {
                                v = r.su(1 + bits)?.clamp(-limit, limit);
                            } else {
                                v = (r.f(bits)? as i32).clamp(0, limit);
                            }
                        }
                        st.seg_features.data[i][j] = v;
                    }
                }
            }
        } else {
            st.seg_features = SegmentationFeatures::default();
        }
        s.features = st.seg_features;
        s.seg_id_pre_skip = false;
        s.last_active_seg_id = 0;
        for i in 0..MAX_SEGMENTS {
            for j in 0..SEG_LVL_MAX {
                if s.features.enabled[i][j] {
                    s.last_active_seg_id = i as u32;
                    if j >= SEG_LVL_REF_FRAME {
                        s.seg_id_pre_skip = true;
                    }
                }
            }
        }
        Ok(())
    }

    fn loop_filter_params(&mut self, r: &mut BitReader, seq: &SequenceHeader, st: &mut HeaderState) -> Result<()> {
        let lf = &mut self.lf;
        if self.coded_lossless || self.allow_intrabc {
            lf.level = [0; 4];
            st.lf_deltas = LoopFilterDeltas::defaults();
            lf.deltas = st.lf_deltas;
            return Ok(());
        }
        lf.level[0] = r.f(6)?;
        lf.level[1] = r.f(6)?;
        if seq.color.num_planes > 1 && (lf.level[0] != 0 || lf.level[1] != 0) {
            lf.level[2] = r.f(6)?;
            lf.level[3] = r.f(6)?;
        }
        lf.sharpness = r.f(3)?;
        lf.delta_enabled = r.flag()?;
        if lf.delta_enabled {
            lf.delta_update = r.flag()?;
            if lf.delta_update {
                for i in 0..TOTAL_REFS_PER_FRAME {
                    if r.flag()? {
                        st.lf_deltas.ref_deltas[i] = r.su(7)?;
                    }
                }
                for i in 0..2 {
                    if r.flag()? {
                        st.lf_deltas.mode_deltas[i] = r.su(7)?;
                    }
                }
            }
        }
        lf.deltas = st.lf_deltas;
        Ok(())
    }

    fn cdef_params(&mut self, r: &mut BitReader, seq: &SequenceHeader) -> Result<()> {
        let c = &mut self.cdef;
        if self.coded_lossless || self.allow_intrabc || !seq.enable_cdef {
            c.bits = 0;
            c.y_pri[0] = 0;
            c.y_sec[0] = 0;
            c.uv_pri[0] = 0;
            c.uv_sec[0] = 0;
            c.damping = 3;
            return Ok(());
        }
        c.damping = r.f(2)? + 3;
        c.bits = r.f(2)?;
        for i in 0..(1 << c.bits) {
            c.y_pri[i] = r.f(4)?;
            c.y_sec[i] = r.f(2)?;
            if c.y_sec[i] == 3 {
                c.y_sec[i] += 1;
            }
            if seq.color.num_planes > 1 {
                c.uv_pri[i] = r.f(4)?;
                c.uv_sec[i] = r.f(2)?;
                if c.uv_sec[i] == 3 {
                    c.uv_sec[i] += 1;
                }
            }
        }
        Ok(())
    }

    fn lr_params(&mut self, r: &mut BitReader, seq: &SequenceHeader) -> Result<()> {
        const REMAP: [u8; 4] = [RESTORE_NONE as u8, RESTORE_SWITCHABLE as u8, RESTORE_WIENER as u8, RESTORE_SGRPROJ as u8];
        let l = &mut self.lr;
        if self.all_lossless || self.allow_intrabc || !seq.enable_restoration {
            l.frame_restoration_type = [RESTORE_NONE as u8; 3];
            l.uses_lr = false;
            return Ok(());
        }
        let mut uses_chroma = false;
        for i in 0..seq.color.num_planes {
            let t = REMAP[r.f(2)? as usize];
            l.frame_restoration_type[i] = t;
            if t != RESTORE_NONE as u8 {
                l.uses_lr = true;
                if i > 0 {
                    uses_chroma = true;
                }
            }
        }
        if l.uses_lr {
            let mut shift;
            if seq.use_128x128_superblock {
                shift = r.f(1)?;
                shift += 1;
            } else {
                shift = r.f(1)?;
                if shift != 0 {
                    shift += r.f(1)?;
                }
            }
            l.loop_restoration_size[0] = (RESTORATION_TILESIZE_MAX as u32) >> (2 - shift);
            let uv_shift = if seq.color.subsampling_x == 1 && seq.color.subsampling_y == 1 && uses_chroma { r.f(1)? } else { 0 };
            l.loop_restoration_size[1] = l.loop_restoration_size[0] >> uv_shift;
            l.loop_restoration_size[2] = l.loop_restoration_size[0] >> uv_shift;
        }
        Ok(())
    }

    fn skip_mode_params(&mut self, r: &mut BitReader, seq: &SequenceHeader, refs: &[RefInfo; NUM_REF_FRAMES]) -> Result<()> {
        let mut allowed = false;
        if !(self.frame_is_intra || !self.reference_select || !seq.enable_order_hint) {
            let mut fwd = -1i32;
            let mut bwd = -1i32;
            let (mut fwd_hint, mut bwd_hint) = (0u32, 0u32);
            for i in 0..REFS_PER_FRAME {
                let ref_hint = refs[self.ref_frame_idx[i]].order_hint;
                if relative_dist(seq, ref_hint, self.order_hint) < 0 {
                    if fwd < 0 || relative_dist(seq, ref_hint, fwd_hint) > 0 {
                        fwd = i as i32;
                        fwd_hint = ref_hint;
                    }
                } else if relative_dist(seq, ref_hint, self.order_hint) > 0 && (bwd < 0 || relative_dist(seq, ref_hint, bwd_hint) < 0) {
                    bwd = i as i32;
                    bwd_hint = ref_hint;
                }
            }
            if fwd < 0 {
                allowed = false;
            } else if bwd >= 0 {
                allowed = true;
                self.skip_mode_frame = [LAST_FRAME + fwd.min(bwd) as usize, LAST_FRAME + fwd.max(bwd) as usize];
            } else {
                let mut second = -1i32;
                let mut second_hint = 0u32;
                for i in 0..REFS_PER_FRAME {
                    let ref_hint = refs[self.ref_frame_idx[i]].order_hint;
                    if relative_dist(seq, ref_hint, fwd_hint) < 0 && (second < 0 || relative_dist(seq, ref_hint, second_hint) > 0) {
                        second = i as i32;
                        second_hint = ref_hint;
                    }
                }
                if second >= 0 {
                    allowed = true;
                    self.skip_mode_frame = [LAST_FRAME + fwd.min(second) as usize, LAST_FRAME + fwd.max(second) as usize];
                }
            }
        }
        self.skip_mode_present = if allowed { r.flag()? } else { false };
        Ok(())
    }

    fn global_motion_params(&mut self, r: &mut BitReader) -> Result<()> {
        for rf in LAST_FRAME..=ALTREF_FRAME {
            self.gm_type[rf] = IDENTITY as u8;
            self.gm_params[rf] = default_gm_params()[rf];
        }
        if self.frame_is_intra {
            return Ok(());
        }
        for rf in LAST_FRAME..=ALTREF_FRAME {
            let typ = if r.flag()? {
                if r.flag()? {
                    ROTZOOM
                } else if r.flag()? {
                    TRANSLATION
                } else {
                    AFFINE
                }
            } else {
                IDENTITY
            };
            self.gm_type[rf] = typ as u8;
            if typ >= ROTZOOM {
                self.read_global_param(r, typ, rf, 2)?;
                self.read_global_param(r, typ, rf, 3)?;
                if typ == AFFINE {
                    self.read_global_param(r, typ, rf, 4)?;
                    self.read_global_param(r, typ, rf, 5)?;
                } else {
                    self.gm_params[rf][4] = -self.gm_params[rf][3];
                    self.gm_params[rf][5] = self.gm_params[rf][2];
                }
            }
            if typ >= TRANSLATION {
                self.read_global_param(r, typ, rf, 0)?;
                self.read_global_param(r, typ, rf, 1)?;
            }
        }
        Ok(())
    }

    fn read_global_param(&mut self, r: &mut BitReader, typ: usize, rf: usize, idx: usize) -> Result<()> {
        let mut abs_bits = GM_ABS_ALPHA_BITS as u32;
        let mut prec_bits = GM_ALPHA_PREC_BITS as u32;
        if idx < 2 {
            if typ == TRANSLATION {
                let hp = !self.allow_high_precision_mv as u32;
                abs_bits = GM_ABS_TRANS_ONLY_BITS as u32 - hp;
                prec_bits = GM_TRANS_ONLY_PREC_BITS as u32 - hp;
            } else {
                abs_bits = GM_ABS_TRANS_BITS as u32;
                prec_bits = GM_TRANS_PREC_BITS as u32;
            }
        }
        let prec_diff = WARPEDMODEL_PREC_BITS as u32 - prec_bits;
        let round = if idx % 3 == 2 { 1i32 << WARPEDMODEL_PREC_BITS } else { 0 };
        let sub = if idx % 3 == 2 { 1i32 << prec_bits } else { 0 };
        let mx = 1i32 << abs_bits;
        let rr = (self.prev_gm_params[rf][idx] >> prec_diff) - sub;
        let v = decode_signed_subexp_with_ref(r, -mx, mx + 1, rr)?;
        self.gm_params[rf][idx] = (v << prec_diff) + round;
        Ok(())
    }

    fn film_grain_params(&mut self, r: &mut BitReader, seq: &SequenceHeader, refs: &[RefInfo; NUM_REF_FRAMES]) -> Result<()> {
        if !seq.film_grain_params_present || (!self.show_frame && !self.showable_frame) {
            self.film_grain = FilmGrainParams::default();
            return Ok(());
        }
        let mut g = FilmGrainParams { apply_grain: r.flag()?, ..Default::default() };
        if !g.apply_grain {
            self.film_grain = FilmGrainParams::default();
            return Ok(());
        }
        g.grain_seed = r.f(16)?;
        g.update_grain = if self.frame_type == INTER_FRAME as u8 { r.flag()? } else { true };
        if !g.update_grain {
            let idx = r.f(3)? as usize;
            let seed = g.grain_seed;
            g = refs[idx].grain.clone();
            g.grain_seed = seed;
            self.film_grain = g;
            return Ok(());
        }
        g.num_y_points = r.f(4)? as usize;
        if g.num_y_points > 14 {
            return Err(Error::Invalid("num_y_points"));
        }
        for i in 0..g.num_y_points {
            g.point_y_value[i] = r.f(8)? as u8;
            g.point_y_scaling[i] = r.f(8)? as u8;
        }
        g.chroma_scaling_from_luma = if seq.color.mono_chrome { false } else { r.flag()? };
        if seq.color.mono_chrome || g.chroma_scaling_from_luma || (seq.color.subsampling_x == 1 && seq.color.subsampling_y == 1 && g.num_y_points == 0) {
            g.num_cb_points = 0;
            g.num_cr_points = 0;
        } else {
            g.num_cb_points = r.f(4)? as usize;
            if g.num_cb_points > 10 {
                return Err(Error::Invalid("num_cb_points"));
            }
            for i in 0..g.num_cb_points {
                g.point_cb_value[i] = r.f(8)? as u8;
                g.point_cb_scaling[i] = r.f(8)? as u8;
            }
            g.num_cr_points = r.f(4)? as usize;
            if g.num_cr_points > 10 {
                return Err(Error::Invalid("num_cr_points"));
            }
            for i in 0..g.num_cr_points {
                g.point_cr_value[i] = r.f(8)? as u8;
                g.point_cr_scaling[i] = r.f(8)? as u8;
            }
        }
        g.grain_scaling_minus_8 = r.f(2)?;
        g.ar_coeff_lag = r.f(2)?;
        let num_pos_luma = 2 * g.ar_coeff_lag * (g.ar_coeff_lag + 1);
        let num_pos_chroma = if g.num_y_points > 0 {
            for i in 0..num_pos_luma as usize {
                g.ar_coeffs_y_plus_128[i] = r.f(8)? as u8;
            }
            num_pos_luma + 1
        } else {
            num_pos_luma
        };
        if g.chroma_scaling_from_luma || g.num_cb_points > 0 {
            for i in 0..num_pos_chroma as usize {
                g.ar_coeffs_cb_plus_128[i] = r.f(8)? as u8;
            }
        }
        if g.chroma_scaling_from_luma || g.num_cr_points > 0 {
            for i in 0..num_pos_chroma as usize {
                g.ar_coeffs_cr_plus_128[i] = r.f(8)? as u8;
            }
        }
        g.ar_coeff_shift_minus_6 = r.f(2)?;
        g.grain_scale_shift = r.f(2)?;
        if g.num_cb_points > 0 {
            g.cb_mult = r.f(8)?;
            g.cb_luma_mult = r.f(8)?;
            g.cb_offset = r.f(9)?;
        }
        if g.num_cr_points > 0 {
            g.cr_mult = r.f(8)?;
            g.cr_luma_mult = r.f(8)?;
            g.cr_offset = r.f(9)?;
        }
        g.overlap_flag = r.flag()?;
        g.clip_to_restricted_range = r.flag()?;
        self.film_grain = g;
        Ok(())
    }
}

pub(crate) fn default_gm_params() -> [[i32; 6]; 8] {
    let mut p = [[0i32; 6]; 8];
    for row in p.iter_mut() {
        row[2] = 1 << WARPEDMODEL_PREC_BITS;
        row[5] = 1 << WARPEDMODEL_PREC_BITS;
    }
    p
}

fn inverse_recenter(r: i32, v: i32) -> i32 {
    if v > 2 * r {
        v
    } else if v & 1 == 1 {
        r - ((v + 1) >> 1)
    } else {
        r + (v >> 1)
    }
}

fn decode_signed_subexp_with_ref(r: &mut BitReader, low: i32, high: i32, rr: i32) -> Result<i32> {
    let x = decode_unsigned_subexp_with_ref(r, high - low, rr - low)?;
    Ok(x + low)
}

fn decode_unsigned_subexp_with_ref(r: &mut BitReader, mx: i32, rr: i32) -> Result<i32> {
    let v = decode_subexp(r, mx)?;
    Ok(if (rr << 1) <= mx { inverse_recenter(rr, v) } else { mx - 1 - inverse_recenter(mx - 1 - rr, v) })
}

fn decode_subexp(r: &mut BitReader, num_syms: i32) -> Result<i32> {
    let mut i = 0;
    let mut mk = 0;
    let k = 3;
    loop {
        let b2 = if i > 0 { k + i - 1 } else { k };
        let a = 1 << b2;
        if num_syms <= mk + 3 * a {
            return Ok(r.ns((num_syms - mk) as u32)? as i32 + mk);
        } else if r.flag()? {
            i += 1;
            mk += a;
        } else {
            return Ok(r.f(b2 as u32)? as i32 + mk);
        }
    }
}

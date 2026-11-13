//! VPS/SPS/PPS writers, the `hvcC` record and level selection (H.265 §7.3.2, §7.3.3,
//! §7.3.7, Annex A/E.2; ISO/IEC 14496-15 §8.3.3.1.2).
//!
//! Phase 1 writes Main-profile, 8-bit, 4:2:0 parameter sets for frame coding: one
//! short-term RPS (a single previous picture), no tiles, WPP, SAO, PCM, scaling
//! lists, long-term references or temporal MVP. Field orders below mirror the spec
//! sections named above.

use filmcraft_bitstream::{BitWriter, escape_rbsp, unescape_rbsp};

use crate::tables::LEVELS_MAIN;
use crate::{ColorConfig, RateControl};

/// NAL unit types the encoder emits. `IDR_N_LP`, `AUD` and `SEI_PREFIX` are consumed
/// by the slice encoder landing later in this PR.
#[allow(dead_code)]
pub const NAL_IDR_N_LP: u8 = 20;
pub const NAL_VPS: u8 = 32;
pub const NAL_SPS: u8 = 33;
pub const NAL_PPS: u8 = 34;
#[allow(dead_code)]
pub const NAL_AUD: u8 = 35;
#[allow(dead_code)]
pub const NAL_SEI_PREFIX: u8 = 39;

/// Legal `general_level_idc` values (H.265 Table A.8: levels 1–6.2, `30 * level`).
pub fn is_legal_level_idc(idc: u8) -> bool {
    LEVELS_MAIN.iter().any(|&(l, ..)| l == idc)
}

/// Pick the lowest Main-tier level whose picture size, sample rate and (for
/// bitrate-targeted modes) bit rate limits cover the stream (Annex A, Main tier,
/// `BrVclFactor = CpbVclFactor * HbrFactor = 1000 * 1`). Falls back to 6.2.
pub fn pick_level(width: u32, height: u32, fps_num: u32, fps_den: u32, kbps: Option<u32>) -> u8 {
    let samples = u64::from(width) * u64::from(height);
    let rate = samples * u64::from(fps_num) / u64::from(fps_den.max(1));
    for &(idc, max_ps, max_sr, max_br, _max_cpb) in LEVELS_MAIN.iter() {
        let max_ps = u64::from(max_ps);
        let br_ok = kbps.is_none_or(|k| u64::from(k) <= u64::from(max_br));
        let dim_ok = u64::from(width) * u64::from(width) <= 8 * max_ps && u64::from(height) * u64::from(height) <= 8 * max_ps;
        if samples <= max_ps && rate <= u64::from(max_sr) && br_ok && dim_ok {
            return idc;
        }
    }
    186
}

/// Bitrate target in kbit/s for level selection (`None` for quality-fixed modes).
pub fn target_kbps(rate: RateControl) -> Option<u32> {
    match rate {
        RateControl::Cbr { kbps } => Some(kbps),
        RateControl::Vbr { target_kbps, .. } => Some(target_kbps),
        RateControl::Crf(_) | RateControl::Qp(_) => None,
    }
}

/// `log2_max_pic_order_cnt_lsb_minus4` sized so the POC window clears twice the
/// keyframe interval (closed-GOP IDR pictures reset the count anyway).
pub fn poc_lsb_minus4(keyint: u32) -> u32 {
    let need = 2u64 * u64::from(keyint) + 2;
    (64 - (need - 1).leading_zeros()).saturating_sub(4).min(8)
}

/// Profile/tier/level for Main (`profilePresentFlag = 1`, no sub-layers): 12 bytes.
fn write_ptl(w: &mut BitWriter, level_idc: u8) {
    w.write_bits(0, 2); // general_profile_space
    w.write_bit(false); // general_tier_flag: Main tier
    w.write_bits(1, 5); // general_profile_idc: Main
    for j in 0..32 {
        w.write_bit(j == 1); // general_profile_compatibility_flag[j]
    }
    w.write_bit(true); // general_progressive_source_flag
    w.write_bit(false); // general_interlaced_source_flag
    w.write_bit(true); // general_non_packed_constraint_flag
    w.write_bit(true); // general_frame_only_constraint_flag
    w.write_bits(0, 32); // general_reserved_zero_43bits (in two writes: n <= 32)
    w.write_bits(0, 11);
    w.write_bit(false); // general_inbld_flag
    w.write_bits(u32::from(level_idc), 8);
}

/// Video usability information (Annex E.2): SAR, BT.709 colour, timing, restrictions.
fn write_vui(w: &mut BitWriter, sar: (u16, u16), color: ColorConfig, fps_num: u32, fps_den: u32) {
    if sar == (0, 0) {
        w.write_bit(false); // aspect_ratio_info_present_flag
    } else {
        w.write_bit(true);
        if sar == (1, 1) {
            w.write_bits(1, 8); // aspect_ratio_idc: 1:1 (square)
        } else {
            w.write_bits(255, 8); // EXTENDED_SAR
            w.write_bits(u32::from(sar.0), 16);
            w.write_bits(u32::from(sar.1), 16);
        }
    }
    w.write_bit(false); // overscan_info_present_flag
    w.write_bit(true); // video_signal_type_present_flag
    w.write_bits(5, 3); // video_format: unspecified
    w.write_bit(color.full_range);
    w.write_bit(true); // colour_description_present_flag
    w.write_bits(u32::from(color.primaries), 8);
    w.write_bits(u32::from(color.transfer), 8);
    w.write_bits(u32::from(color.matrix), 8);
    w.write_bit(false); // chroma_loc_info_present_flag
    w.write_bit(false); // neutral_chroma_indication_flag
    w.write_bit(false); // field_seq_flag
    w.write_bit(false); // frame_field_info_present_flag
    w.write_bit(false); // default_display_window_flag
    w.write_bit(true); // vui_timing_info_present_flag
    w.write_bits(fps_den, 32);
    w.write_bits(fps_num, 32);
    w.write_bit(false); // vui_poc_proportional_to_timing_flag
    w.write_bit(false); // vui_hrd_parameters_present_flag
    w.write_bit(true); // bitstream_restriction_flag
    w.write_bit(false); // tiles_fixed_structure_flag
    w.write_bit(true); // motion_vectors_over_pic_boundaries_flag
    w.write_bit(true); // restricted_ref_pic_lists_flag: slices always use the SPS RPS
    w.write_ue(0); // min_spatial_segmentation_idc
    w.write_ue(0); // max_bytes_per_pic_denom: no limit (not enforced by the encoder)
    w.write_ue(0); // max_bits_per_min_cu_denom: no limit
    w.write_ue(8); // log2_max_mv_length_horizontal (±256 luma samples)
    w.write_ue(8); // log2_max_mv_length_vertical
}

/// Video parameter set: single layer, present in this bitstream (§7.3.2.1).
pub struct Vps {
    pub level_idc: u8,
}

impl Vps {
    pub fn rbsp(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(0, 4); // vps_video_parameter_set_id
        w.write_bit(true); // vps_base_layer_internal_flag: base layer is in this bitstream
        w.write_bit(true); // vps_base_layer_available_flag
        w.write_bits(0, 6); // vps_max_layers_minus1
        w.write_bits(0, 3); // vps_max_sub_layers_minus1
        w.write_bit(true); // vps_temporal_id_nesting_flag
        w.write_bits(0xFFFF, 16); // vps_reserved_0xffff_16bits
        write_ptl(&mut w, self.level_idc);
        w.write_bit(true); // vps_sub_layer_ordering_info_present_flag
        w.write_ue(4); // vps_max_dec_pic_buffering_minus1 (capacity 5, within every MaxDpbSize)
        w.write_ue(0); // vps_max_num_reorder_pics
        w.write_ue(0); // vps_max_latency_increase_plus1
        w.write_bits(0, 6); // vps_max_layer_id
        w.write_ue(0); // vps_num_layer_sets_minus1
        w.write_bit(false); // vps_timing_info_present_flag (timing lives in the VUI)
        w.write_bit(false); // vps_extension_flag
        w.rbsp_trailing();
        w.finish()
    }
}

/// Sequence parameter set: Main, 4:2:0 8-bit, CTU 64, one short-term RPS (§7.3.2.2.1).
pub struct Sps {
    pub width: u32,
    pub height: u32,
    pub sar: (u16, u16),
    pub color: ColorConfig,
    pub fps_num: u32,
    pub fps_den: u32,
    pub level_idc: u8,
    pub poc_lsb_minus4: u32,
}

impl Sps {
    /// Coded size (CTU-aligned) and right/bottom conformance offsets in crop units.
    fn coded_and_crop(&self) -> (u32, u32, u32, u32) {
        let coded_w = self.width.div_ceil(64) * 64;
        let coded_h = self.height.div_ceil(64) * 64;
        (coded_w, coded_h, (coded_w - self.width) / 2, (coded_h - self.height) / 2)
    }

    pub fn rbsp(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(0, 4); // sps_video_parameter_set_id
        w.write_bits(0, 3); // sps_max_sub_layers_minus1
        w.write_bit(true); // sps_temporal_id_nesting_flag
        write_ptl(&mut w, self.level_idc);
        w.write_ue(0); // sps_seq_parameter_set_id
        w.write_ue(1); // chroma_format_idc: 4:2:0
        // The signalled size is the CTU-aligned coded size (a multiple of
        // MinCbSizeY, as §7.4.3.2.1 requires); the window crops to the visible size.
        let (coded_w, coded_h, right, bottom) = self.coded_and_crop();
        w.write_ue(coded_w); // pic_width_in_luma_samples
        w.write_ue(coded_h); // pic_height_in_luma_samples
        w.write_bit(right != 0 || bottom != 0); // conformance_window_flag
        if right != 0 || bottom != 0 {
            w.write_ue(0);
            w.write_ue(right);
            w.write_ue(0);
            w.write_ue(bottom);
        }
        w.write_ue(0); // bit_depth_luma_minus8
        w.write_ue(0); // bit_depth_chroma_minus8
        w.write_ue(self.poc_lsb_minus4); // log2_max_pic_order_cnt_lsb_minus4
        w.write_bit(true); // sps_sub_layer_ordering_info_present_flag
        w.write_ue(4); // sps_max_dec_pic_buffering_minus1
        w.write_ue(0); // sps_max_num_reorder_pics
        w.write_ue(0); // sps_max_latency_increase_plus1
        w.write_ue(0); // log2_min_luma_coding_block_size_minus3 (8x8)
        w.write_ue(3); // log2_diff_max_min_luma_coding_block_size (up to 64x64)
        w.write_ue(0); // log2_min_luma_transform_block_size_minus2 (4x4)
        w.write_ue(3); // log2_diff_max_min_luma_transform_block_size (up to 32x32)
        w.write_ue(3); // max_transform_hierarchy_depth_inter
        w.write_ue(3); // max_transform_hierarchy_depth_intra
        w.write_bit(false); // scaling_list_enabled_flag
        w.write_bit(false); // amp_enabled_flag
        w.write_bit(false); // sample_adaptive_offset_enabled_flag
        w.write_bit(false); // pcm_enabled_flag
        w.write_ue(1); // num_short_term_ref_pic_sets
        // st_ref_pic_set(0): one negative picture, delta POC 1, used by the current picture.
        w.write_ue(1); // num_negative_pics
        w.write_ue(0); // num_positive_pics
        w.write_ue(0); // delta_poc_s0_minus1[0]
        w.write_bit(true); // used_by_curr_pic_s0_flag[0]
        w.write_bit(false); // long_term_ref_pics_present_flag
        w.write_bit(false); // sps_temporal_mvp_enabled_flag
        w.write_bit(true); // strong_intra_smoothing_enabled_flag
        w.write_bit(true); // vui_parameters_present_flag
        write_vui(&mut w, self.sar, self.color, self.fps_num, self.fps_den);
        w.write_bit(false); // sps_extension_present_flag
        w.rbsp_trailing();
        w.finish()
    }
}

/// Picture parameter set: one reference, CU-level QP deltas for AQ (§7.3.2.3.1).
pub struct Pps {
    pub init_qp: i32,
}

impl Pps {
    pub fn rbsp(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_ue(0); // pps_pic_parameter_set_id
        w.write_ue(0); // pps_seq_parameter_set_id
        w.write_bit(false); // dependent_slice_segments_enabled_flag
        w.write_bit(false); // output_flag_present_flag
        w.write_bits(0, 3); // num_extra_slice_header_bits
        w.write_bit(false); // sign_data_hiding_enabled_flag
        w.write_bit(false); // cabac_init_present_flag
        w.write_ue(0); // num_ref_idx_l0_default_active_minus1 (one reference)
        w.write_ue(0); // num_ref_idx_l1_default_active_minus1
        w.write_se(self.init_qp - 26); // init_qp_minus26
        w.write_bit(false); // constrained_intra_pred_flag
        w.write_bit(false); // transform_skip_enabled_flag
        w.write_bit(true); // cu_qp_delta_enabled_flag (CTU-granularity AQ)
        w.write_ue(0); // diff_cu_qp_delta_depth
        w.write_se(0); // pps_cb_qp_offset
        w.write_se(0); // pps_cr_qp_offset
        w.write_bit(false); // pps_slice_chroma_qp_offsets_present_flag
        w.write_bit(false); // weighted_pred_flag
        w.write_bit(false); // weighted_bipred_flag
        w.write_bit(false); // transquant_bypass_enabled_flag
        w.write_bit(false); // tiles_enabled_flag
        w.write_bit(false); // entropy_coding_sync_enabled_flag
        w.write_bit(true); // pps_loop_filter_across_slices_enabled_flag
        w.write_bit(false); // deblocking_filter_control_present_flag (defaults)
        w.write_bit(false); // pps_scaling_list_data_present_flag
        w.write_bit(false); // lists_modification_present_flag
        w.write_ue(0); // log2_parallel_merge_level_minus2 (4x4 regions)
        w.write_bit(false); // slice_segment_header_extension_present_flag
        w.write_bit(false); // pps_extension_present_flag
        w.rbsp_trailing();
        w.finish()
    }
}

/// Two-byte NAL header: `forbidden_zero_bit = 0`, layer 0, `temporal_id_plus1 = 1`.
pub fn nal_header(nal_type: u8) -> [u8; 2] {
    [(nal_type << 1) & 0x7E, 1]
}

/// A complete NAL unit (header plus emulation-prevented RBSP), without framing.
pub fn assemble(nal_type: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut nal = Vec::with_capacity(rbsp.len() + 2);
    nal.extend_from_slice(&nal_header(nal_type));
    nal.extend_from_slice(&escape_rbsp(rbsp));
    nal
}

/// Frame complete NAL units with `00 00 00 01` start codes (Annex B).
/// Consumed by the slice encoder landing later in this PR.
#[allow(dead_code)]
pub fn frame_annexb(nals: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        out.extend_from_slice(&[0, 0, 0, 1]);
        out.extend_from_slice(nal);
    }
    out
}

/// Frame complete NAL units with 4-byte big-endian lengths (MP4 samples).
/// Consumed by the slice encoder landing later in this PR.
#[allow(dead_code)]
pub fn frame_length_prefixed(nals: &[&[u8]]) -> Vec<u8> {
    let mut out = Vec::new();
    for nal in nals {
        out.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

/// `HEVCDecoderConfigurationRecord` (ISO/IEC 14496-15) over complete VPS/SPS/PPS
/// NAL units (headers included, start codes excluded).
pub fn hvcc(vps: &[u8], sps: &[u8], pps: &[u8], level_idc: u8) -> Vec<u8> {
    let ptl = {
        let rbsp = unescape_rbsp(sps.get(2..).unwrap_or_default());
        rbsp.get(1..13).unwrap_or_default().to_vec()
    };
    let mut out = Vec::with_capacity(23 + vps.len() + sps.len() + pps.len());
    out.push(1); // configurationVersion
    out.push(1); // profile_space 0, tier 0, profile_idc 1 (Main)
    out.extend_from_slice(&0x4000_0000u32.to_be_bytes()); // compatibility: Main only
    let mut constraints = [0u8; 6];
    if ptl.len() == 12 {
        constraints.copy_from_slice(&ptl[5..11]);
    }
    out.extend_from_slice(&constraints);
    out.push(level_idc);
    out.extend_from_slice(&[0xF0, 0x00]); // reserved + min_spatial_segmentation_idc 0
    out.push(0xFC); // reserved + parallelismType 0
    out.push(0xFD); // reserved + chromaFormat 1 (4:2:0)
    out.push(0xF8); // reserved + bitDepthLumaMinus8 0
    out.push(0xF8); // reserved + bitDepthChromaMinus8 0
    out.extend_from_slice(&[0, 0]); // avgFrameRate: unspecified
    out.push(0x0F); // constant 0, 1 temporal layer, nested, lengthSizeMinusOne 3
    out.push(3); // numOfArrays
    for (nal_type, nal) in [(NAL_VPS, vps), (NAL_SPS, sps), (NAL_PPS, pps)] {
        out.push(0x80 | nal_type); // array_completeness + NAL_unit_type
        out.extend_from_slice(&1u16.to_be_bytes()); // numNalus
        out.extend_from_slice(&(nal.len() as u16).to_be_bytes());
        out.extend_from_slice(nal);
    }
    out
}

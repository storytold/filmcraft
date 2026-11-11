//! NAL unit and parameter-set/slice-header generation (§7.3), SEI and the AVCDecoderConfigurationRecord.

use filmcraft_bitstream::{BitWriter, escape_rbsp};

pub const NAL_SLICE: u8 = 1;
pub const NAL_IDR: u8 = 5;
pub const NAL_SEI: u8 = 6;
pub const NAL_SPS: u8 = 7;
pub const NAL_PPS: u8 = 8;
pub const NAL_AUD: u8 = 9;

/// Build a NAL unit (header + escaped RBSP), without start code.
pub fn nal(nal_ref_idc: u8, nal_type: u8, rbsp: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(rbsp.len() + rbsp.len() / 64 + 2);
    out.push((nal_ref_idc << 5) | nal_type);
    out.extend_from_slice(&escape_rbsp(rbsp));
    out
}

#[derive(Clone, Debug)]
pub struct Vui {
    pub sar: (u16, u16),
    pub video_full_range: bool,
    pub colour_primaries: u8,
    pub transfer: u8,
    pub matrix: u8,
    pub num_units_in_tick: u32,
    pub time_scale: u32,
    pub max_num_reorder_frames: u32,
    pub max_dec_frame_buffering: u32,
}

#[derive(Clone, Debug)]
pub struct Sps {
    pub profile_idc: u8,
    pub constraint_flags: u8,
    pub level_idc: u8,
    pub width_mbs: u32,
    pub height_mbs: u32,
    pub crop_right: u32,
    pub crop_bottom: u32,
    pub log2_max_frame_num: u32,
    pub log2_max_poc_lsb: u32,
    pub max_num_ref_frames: u32,
    /// BitDepthY = BitDepthC (8 to 10).
    pub bit_depth: u8,
    pub vui: Vui,
}

impl Sps {
    pub fn rbsp(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_bits(self.profile_idc as u32, 8);
        w.write_bits(self.constraint_flags as u32, 8);
        w.write_bits(self.level_idc as u32, 8);
        w.write_ue(0); // seq_parameter_set_id
        if matches!(self.profile_idc, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
            w.write_ue(1); // chroma_format_idc 4:2:0
            w.write_ue(self.bit_depth.saturating_sub(8) as u32); // bit_depth_luma_minus8
            w.write_ue(self.bit_depth.saturating_sub(8) as u32); // bit_depth_chroma_minus8
            w.write_bit(false); // qpprime_y_zero_transform_bypass_flag
            w.write_bit(false); // seq_scaling_matrix_present_flag
        }
        w.write_ue(self.log2_max_frame_num - 4);
        w.write_ue(0); // pic_order_cnt_type
        w.write_ue(self.log2_max_poc_lsb - 4);
        w.write_ue(self.max_num_ref_frames);
        w.write_bit(false); // gaps_in_frame_num_value_allowed_flag
        w.write_ue(self.width_mbs - 1);
        w.write_ue(self.height_mbs - 1);
        w.write_bit(true); // frame_mbs_only_flag
        w.write_bit(true); // direct_8x8_inference_flag
        let crop = self.crop_right > 0 || self.crop_bottom > 0;
        w.write_bit(crop);
        if crop {
            w.write_ue(0);
            w.write_ue(self.crop_right / 2);
            w.write_ue(0);
            w.write_ue(self.crop_bottom / 2);
        }
        w.write_bit(true); // vui_parameters_present_flag
        let v = &self.vui;
        let has_sar = v.sar != (0, 0);
        w.write_bit(has_sar);
        if has_sar {
            if v.sar == (1, 1) {
                w.write_bits(1, 8);
            } else {
                w.write_bits(255, 8); // Extended_SAR
                w.write_bits(v.sar.0 as u32, 16);
                w.write_bits(v.sar.1 as u32, 16);
            }
        }
        w.write_bit(false); // overscan_info_present_flag
        w.write_bit(true); // video_signal_type_present_flag
        w.write_bits(5, 3); // video_format: unspecified
        w.write_bit(v.video_full_range);
        w.write_bit(true); // colour_description_present_flag
        w.write_bits(v.colour_primaries as u32, 8);
        w.write_bits(v.transfer as u32, 8);
        w.write_bits(v.matrix as u32, 8);
        w.write_bit(false); // chroma_loc_info_present_flag
        w.write_bit(true); // timing_info_present_flag
        w.write_bits(v.num_units_in_tick, 32);
        w.write_bits(v.time_scale, 32);
        w.write_bit(true); // fixed_frame_rate_flag
        w.write_bit(false); // nal_hrd_parameters_present_flag
        w.write_bit(false); // vcl_hrd_parameters_present_flag
        w.write_bit(false); // pic_struct_present_flag
        w.write_bit(true); // bitstream_restriction_flag
        w.write_bit(true); // motion_vectors_over_pic_boundaries_flag
        w.write_ue(0); // max_bytes_per_pic_denom
        w.write_ue(0); // max_bits_per_mb_denom
        w.write_ue(11); // log2_max_mv_length_horizontal (±512 samples)
        w.write_ue(11); // log2_max_mv_length_vertical
        w.write_ue(v.max_num_reorder_frames);
        w.write_ue(v.max_dec_frame_buffering);
        w.rbsp_trailing();
        w.finish()
    }
}

#[derive(Clone, Debug)]
pub struct Pps {
    pub cabac: bool,
    pub pic_init_qp: i32,
    pub chroma_qp_offset: i32,
    pub transform_8x8: bool,
    pub high: bool,
}

impl Pps {
    pub fn rbsp(&self) -> Vec<u8> {
        let mut w = BitWriter::new();
        w.write_ue(0); // pic_parameter_set_id
        w.write_ue(0); // seq_parameter_set_id
        w.write_bit(self.cabac);
        w.write_bit(false); // bottom_field_pic_order_in_frame_present_flag
        w.write_ue(0); // num_slice_groups_minus1
        w.write_ue(0); // num_ref_idx_l0_default_active_minus1
        w.write_ue(0); // num_ref_idx_l1_default_active_minus1
        w.write_bit(false); // weighted_pred_flag
        w.write_bits(0, 2); // weighted_bipred_idc
        w.write_se(self.pic_init_qp - 26);
        w.write_se(0); // pic_init_qs_minus26
        w.write_se(self.chroma_qp_offset);
        w.write_bit(true); // deblocking_filter_control_present_flag
        w.write_bit(false); // constrained_intra_pred_flag
        w.write_bit(false); // redundant_pic_cnt_present_flag
        if self.high {
            w.write_bit(self.transform_8x8);
            w.write_bit(false); // pic_scaling_matrix_present_flag
            w.write_se(self.chroma_qp_offset); // second_chroma_qp_index_offset
        }
        w.rbsp_trailing();
        w.finish()
    }
}

/// Access unit delimiter RBSP. `primary_pic_type`: 0 = I, 1 = I/P, 2 = I/P/B.
pub fn aud_rbsp(primary_pic_type: u8) -> Vec<u8> {
    let mut w = BitWriter::new();
    w.write_bits(primary_pic_type as u32, 3);
    w.rbsp_trailing();
    w.finish()
}

/// user_data_unregistered SEI carrying an encoder identification string.
pub fn sei_user_data_rbsp(text: &str) -> Vec<u8> {
    const UUID: [u8; 16] = [0x7a, 0x3c, 0x1f, 0x52, 0xd4, 0x0b, 0x4e, 0x8e, 0x9d, 0x61, 0x2f, 0xc4, 0x93, 0x05, 0xb7, 0x1a];
    let mut payload = UUID.to_vec();
    payload.extend_from_slice(text.as_bytes());
    let mut w = BitWriter::new();
    w.write_bits(5, 8); // payloadType user_data_unregistered
    let mut size = payload.len();
    while size >= 255 {
        w.write_bits(255, 8);
        size -= 255;
    }
    w.write_bits(size as u32, 8);
    w.write_bytes(&payload);
    w.rbsp_trailing();
    w.finish()
}

/// SEI with HDR static metadata: mastering_display_colour_volume (payloadType 137, the 24-byte
/// ST 2086 payload) and/or content_light_level_info (payloadType 144), H.264 Annex D.
pub fn sei_hdr_rbsp(mastering: Option<&[u8; 24]>, cll: Option<(u16, u16)>) -> Vec<u8> {
    let mut w = BitWriter::new();
    if let Some(m) = mastering {
        w.write_bits(137, 8);
        w.write_bits(24, 8);
        w.write_bytes(m);
    }
    if let Some((a, b)) = cll {
        w.write_bits(144, 8);
        w.write_bits(4, 8);
        w.write_bits(a as u32, 16);
        w.write_bits(b as u32, 16);
    }
    w.rbsp_trailing();
    w.finish()
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SliceType {
    P = 0,
    B = 1,
    I = 2,
}

#[derive(Clone, Debug)]
pub struct SliceHeader {
    pub first_mb: u32,
    pub slice_type: SliceType,
    pub nal_ref_idc: u8,
    pub idr: bool,
    pub idr_pic_id: u32,
    pub frame_num: u32,
    pub log2_max_frame_num: u32,
    pub poc_lsb: u32,
    pub log2_max_poc_lsb: u32,
    /// Reorder RefPicList0 so that this picNum difference (abs_diff_pic_num_minus1, subtract) comes first.
    pub l0_modification: Option<u32>,
    pub cabac: bool,
    pub slice_qp: i32,
    pub pic_init_qp: i32,
    pub disable_deblocking: u32,
    pub alpha_offset_div2: i32,
    pub beta_offset_div2: i32,
}

impl SliceHeader {
    /// Write the slice header into `w`; for CABAC the caller must byte-align with one-bits afterwards.
    pub fn write(&self, w: &mut BitWriter) {
        w.write_ue(self.first_mb);
        w.write_ue(self.slice_type as u32);
        w.write_ue(0); // pic_parameter_set_id
        w.write_bits(self.frame_num, self.log2_max_frame_num);
        if self.idr {
            w.write_ue(self.idr_pic_id);
        }
        w.write_bits(self.poc_lsb, self.log2_max_poc_lsb);
        if self.slice_type == SliceType::B {
            w.write_bit(true); // direct_spatial_mv_pred_flag
        }
        if self.slice_type != SliceType::I {
            w.write_bit(false); // num_ref_idx_active_override_flag
            // ref_pic_list_modification
            match self.l0_modification {
                Some(d) => {
                    w.write_bit(true);
                    w.write_ue(0); // modification_of_pic_nums_idc: subtract
                    w.write_ue(d);
                    w.write_ue(3);
                }
                None => w.write_bit(false),
            }
            if self.slice_type == SliceType::B {
                w.write_bit(false);
            }
        }
        if self.nal_ref_idc != 0 {
            if self.idr {
                w.write_bit(false); // no_output_of_prior_pics_flag
                w.write_bit(false); // long_term_reference_flag
            } else {
                w.write_bit(false); // adaptive_ref_pic_marking_mode_flag
            }
        }
        if self.cabac && self.slice_type != SliceType::I {
            w.write_ue(0); // cabac_init_idc
        }
        w.write_se(self.slice_qp - self.pic_init_qp);
        w.write_ue(self.disable_deblocking);
        if self.disable_deblocking != 1 {
            w.write_se(self.alpha_offset_div2);
            w.write_se(self.beta_offset_div2);
        }
    }
}

/// AVCDecoderConfigurationRecord (ISO/IEC 14496-15 §5.3.3.1) with 4-byte NAL lengths.
pub fn avcc(sps_nal: &[u8], pps_nal: &[u8], sps: &Sps) -> Vec<u8> {
    let profile_idc = sps.profile_idc;
    let mut v = vec![1, profile_idc, sps.constraint_flags, sps.level_idc, 0xFF, 0xE1];
    v.extend_from_slice(&(sps_nal.len() as u16).to_be_bytes());
    v.extend_from_slice(sps_nal);
    v.push(1);
    v.extend_from_slice(&(pps_nal.len() as u16).to_be_bytes());
    v.extend_from_slice(pps_nal);
    if matches!(profile_idc, 100 | 110 | 122 | 144) {
        let bd = sps.bit_depth.saturating_sub(8) & 7;
        v.push(0xFC | 1); // chroma_format 4:2:0
        v.push(0xF8 | bd); // bit_depth_luma_minus8
        v.push(0xF8 | bd); // bit_depth_chroma_minus8
        v.push(0); // numOfSequenceParameterSetExt
    }
    v
}

/// The requested level when it is at least `needed` (and a valid level_idc), else `needed`.
/// Level 1b (`11` with constraint_set3) is not distinguished; 9 is not a level here.
pub fn level_at_least(requested: u8, needed: u8) -> u8 {
    const VALID: [u8; 19] = [10, 11, 12, 13, 20, 21, 22, 30, 31, 32, 40, 41, 42, 50, 51, 52, 60, 61, 62];
    if VALID.contains(&requested) && requested >= needed { requested } else { needed }
}

/// Pick the lowest level satisfying frame size, macroblock rate, DPB size and (optionally) bitrate.
/// `br_factor` is the profile's cpbBrVclFactor / 1000 in thousandths (1000 Baseline/Main, 1250 High,
/// 3000 High 10).
pub fn pick_level(width_mbs: u32, height_mbs: u32, fps: f64, dpb_frames: u32, kbps: Option<u32>, br_factor: u64) -> u8 {
    // (level_idc, MaxMBPS, MaxFS, MaxDpbMbs, MaxBR kbit/s)
    const LEVELS: [(u8, u64, u64, u64, u64); 17] = [
        (10, 1485, 99, 396, 64),
        (11, 3000, 396, 900, 192),
        (12, 6000, 396, 2376, 384),
        (13, 11880, 396, 2376, 768),
        (20, 11880, 396, 2376, 2000),
        (21, 19800, 792, 4752, 4000),
        (22, 20250, 1620, 8100, 4000),
        (30, 40500, 1620, 8100, 10000),
        (31, 108000, 3600, 18000, 14000),
        (32, 216000, 5120, 20480, 20000),
        (40, 245760, 8192, 32768, 20000),
        (41, 245760, 8192, 32768, 50000),
        (42, 522240, 8704, 34816, 50000),
        (50, 589824, 22080, 110400, 135000),
        (51, 983040, 36864, 184320, 240000),
        (52, 2073600, 36864, 184320, 240000),
        (62, 16711680, 139264, 696320, 800000),
    ];
    let fs = (width_mbs * height_mbs) as u64;
    let mbps = (fs as f64 * fps).ceil() as u64;
    for &(l, max_mbps, max_fs, max_dpb, max_br) in &LEVELS {
        let br_ok = kbps.is_none_or(|k| (k as u64) * 1000 <= max_br * br_factor);
        let dim_ok = (width_mbs as u64).pow(2) <= 8 * max_fs && (height_mbs as u64).pow(2) <= 8 * max_fs;
        if fs <= max_fs && mbps <= max_mbps && fs * dpb_frames as u64 <= max_dpb && br_ok && dim_ok {
            return l;
        }
    }
    62
}

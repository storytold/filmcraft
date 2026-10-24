//! OBU (Open Bitstream Unit) formatting according to AV1 spec 5.3.

use filmcraft_bitstream::BitWriter;

pub fn select_seq_level_idx(width: u32, height: u32) -> u8 {
    if width > 1920 || height > 1080 { 12 } else { 8 }
}

pub const OBU_SEQUENCE_HEADER: u8 = 1;
pub const OBU_TEMPORAL_DELIMITER: u8 = 2;
pub const OBU_FRAME_HEADER: u8 = 3;
pub const OBU_TILE_GROUP: u8 = 4;
pub const OBU_FRAME: u8 = 6;

/// Write an LEB128 unsigned integer.
pub fn write_leb128(val: usize, out: &mut Vec<u8>) {
    let mut v = val;
    loop {
        let mut byte = (v & 0x7f) as u8;
        v >>= 7;
        if v != 0 {
            byte |= 0x80;
        }
        out.push(byte);
        if v == 0 {
            break;
        }
    }
}

/// Pack an OBU with header, optional length, and payload.
pub fn write_obu(obu_type: u8, payload: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(1 + 8 + payload.len());
    // Header byte:
    // obu_forbidden_bit (0), obu_type (4 bits), obu_extension_flag (0), obu_has_size_field (1), obu_reserved_1bit (0)
    let header = ((obu_type & 0xf) << 3) | 0x02; // obu_has_size_field = 1
    out.push(header);
    write_leb128(payload.len(), &mut out);
    out.extend_from_slice(payload);
    out
}

/// Generate a Temporal Delimiter OBU (empty payload).
pub fn write_temporal_delimiter() -> Vec<u8> {
    write_obu(OBU_TEMPORAL_DELIMITER, &[])
}

/// Sequence Header configuration.
#[derive(Clone, Debug)]
pub struct SequenceConfig {
    pub width: u32,
    pub height: u32,
    pub bit_depth: u8,
    pub mono_chrome: bool,
    pub enable_cdef: bool,
}

/// Write a Sequence Header OBU (spec 5.5).
pub fn write_sequence_header(cfg: &SequenceConfig) -> Vec<u8> {
    let mut w = BitWriter::new();
    // seq_profile = 0 (Main profile)
    w.write_bits(0, 3);
    // still_picture = 0
    w.write_bit(false);
    // reduced_still_picture_header = 0
    w.write_bit(false);
    // timing_info_present_flag = 0
    w.write_bit(false);
    // initial_display_delay_present_flag = 0
    w.write_bit(false);
    // operating_points_cnt_minus_1 = 0
    w.write_bits(0, 5);
    // operating_point_idc[0] = 0
    w.write_bits(0, 12);
    // seq_level_idx[0] = 8 (Level 4.0 - up to 1080p, or 12 for 4K)
    let level = select_seq_level_idx(cfg.width, cfg.height);
    w.write_bits(level as u32, 5);
    if level > 7 {
        w.write_bit(false); // seq_tier = 0
    }

    // frame_width_bits_minus_1 and frame_height_bits_minus_1
    // 16 bits maximum representation
    let w_bits = 32 - cfg.width.leading_zeros();
    let h_bits = 32 - cfg.height.leading_zeros();
    w.write_bits(w_bits.saturating_sub(1), 4);
    w.write_bits(h_bits.saturating_sub(1), 4);
    // max_frame_width_minus_1, max_frame_height_minus_1
    w.write_bits(cfg.width - 1, w_bits);
    w.write_bits(cfg.height - 1, h_bits);

    // frame_id_numbers_present_flag = 0
    w.write_bit(false);
    // use_128x128_superblock = 0 (use 64x64 superblocks)
    w.write_bit(false);
    // enable_filter_intra = 0
    w.write_bit(false);
    // enable_intra_edge_filter = 0
    w.write_bit(false);
    // enable_interintra_compound = 0
    w.write_bit(false);
    // enable_masked_compound = 0
    w.write_bit(false);
    // enable_warped_motion = 0
    w.write_bit(false);
    // enable_dual_filter = 0
    w.write_bit(false);
    // enable_order_hint = 1
    w.write_bit(true);
    // enable_jnt_comp = 0
    w.write_bit(false);
    // enable_ref_frame_mvs = 0
    w.write_bit(false);
    // seq_choose_screen_content_tools = 0, seq_force_screen_content_tools = 0
    w.write_bit(false);
    w.write_bits(0, 1);
    // seq_force_screen_content_tools == 0 => seq_force_integer_mv is SELECT_INTEGER_MV without writing bits
    // order_hint_bits_minus_1 = 6 (7 bits)
    w.write_bits(6, 3);
    // enable_superres = 0
    w.write_bit(false);
    // enable_cdef
    w.write_bit(cfg.enable_cdef);
    // enable_restoration = 0
    w.write_bit(false);

    // Color config:
    // high_bitdepth = 0
    w.write_bit(cfg.bit_depth > 8);
    // mono_chrome = 0
    w.write_bit(cfg.mono_chrome);
    // color_description_present_flag = 1
    w.write_bit(true);
    // color_primaries (1 = BT.709)
    w.write_bits(1, 8);
    // transfer_characteristics (1 = BT.709)
    w.write_bits(1, 8);
    // matrix_coefficients (1 = BT.709)
    w.write_bits(1, 8);
    // color_range (0 = Studio/Limited, 1 = Full)
    w.write_bit(false);
    // subsampling_x = 1, subsampling_y = 1 (4:2:0)
    // For profile 0: implied 4:2:0, chroma_sample_position (2 bits)
    w.write_bits(0, 2);
    // separate_uv_delta_q = 0
    w.write_bit(false);

    // film_grain_params_present = 0
    w.write_bit(false);

    // Trailing bit
    w.rbsp_trailing();
    write_obu(OBU_SEQUENCE_HEADER, &w.finish())
}

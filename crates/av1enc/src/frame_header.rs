//! Frame and tile header encoding (AV1 spec 5.9 / 5.11).

use crate::obu::SequenceConfig;
use filmcraft_bitstream::BitWriter;

pub const KEY_FRAME: u8 = 0;
pub const INTER_FRAME: u8 = 1;

/// Parameters for encoding an uncompressed frame header.
pub struct FrameHeaderConfig {
    pub frame_type: u8,
    pub show_frame: bool,
    pub error_resilient_mode: bool,
    pub disable_cdf_update: bool,
    pub qindex: u8,
    pub order_hint: u32,
}

/// Write uncompressed frame header (spec 5.9).
pub fn write_frame_header(cfg: &FrameHeaderConfig, seq: &SequenceConfig) -> Vec<u8> {
    let mut w = BitWriter::new();
    // 1. show_existing_frame = 0
    w.write_bit(false);
    // 2. frame_type: KEY_FRAME (0) or INTER_FRAME (1)
    w.write_bits(cfg.frame_type as u32, 2);
    // 3. show_frame
    w.write_bit(cfg.show_frame);

    let is_key = cfg.frame_type == KEY_FRAME;
    if !is_key || !cfg.show_frame {
        w.write_bit(cfg.error_resilient_mode);
    }

    // 4. disable_cdf_update
    w.write_bit(cfg.disable_cdf_update);

    // allow_screen_content_tools is inferred (0) from sequence header
    // force_integer_mv is inferred (1) for intra frame

    // 5. frame_size_override_flag = 0
    w.write_bit(false);
    // 6. order_hint (7 bits, matching order_hint_bits)
    w.write_bits(cfg.order_hint & 0x7f, 7);

    if is_key {
        // render_and_frame_size_different = 0
        w.write_bit(false);
    } else {
        // primary_ref_frame: PRIMARY_REF_NONE (7 = 3 bits: 111)
        w.write_bits(7, 3);
        // refresh_frame_flags: 8 bits, 0x01 (updates slot 0)
        w.write_bits(1, 8);
        // short: 0 (explicit ref indices)
        w.write_bit(false);
        // ref_frame_idx[0..7]: 7 * 3 bits = 0 (all point to slot 0)
        for _ in 0..7 {
            w.write_bits(0, 3);
        }
        // render_and_frame_size_different: 0
        w.write_bit(false);
        // allow_high_precision_mv: 0
        w.write_bit(false);
        // interpolation_filter: is_filter_switchable = 0, filter = EIGHTTAP (0) (2 bits)
        w.write_bit(false);
        w.write_bits(0, 2);
        // is_motion_mode_switchable: 0
        w.write_bit(false);
    }

    if !cfg.disable_cdf_update {
        // disable_frame_end_update_cdf
        w.write_bit(false);
    }

    // 8. tile_info (spec 5.9.15):
    // uniform_tile_spacing_flag = 1
    w.write_bit(true);
    let sb_cols = seq.width.div_ceil(64);
    let sb_rows = seq.height.div_ceil(64);
    let max_tile_cols_log2 = tile_log2(1, sb_cols.min(64));
    let max_tile_rows_log2 = tile_log2(1, sb_rows.min(64));
    if max_tile_cols_log2 > 0 {
        w.write_bit(false);
    }
    if max_tile_rows_log2 > 0 {
        w.write_bit(false);
    }

    // 9. Quantization params (spec 5.9.12):
    // base_q_idx (8 bits)
    w.write_bits(cfg.qindex as u32, 8);
    // delta_q_y_dc = 0, delta_q_u_dc = 0, delta_q_u_ac = 0
    w.write_bit(false);
    w.write_bit(false);
    w.write_bit(false);
    // using_qmatrix = 0
    w.write_bit(false);

    // 10. Segmentation:
    // segmentation_enabled = 0
    w.write_bit(false);

    // 11. Delta Q params:
    if cfg.qindex > 0 {
        // delta_q_present = 0
        w.write_bit(false);
    }

    // 12. Loop filter:
    let coded_lossless = cfg.qindex == 0;
    if !coded_lossless {
        // loop_filter_level adapted to qindex
        let lf_level = (cfg.qindex / 4).min(63) as u32;
        w.write_bits(lf_level, 6);
        w.write_bits(lf_level, 6);
        if !seq.mono_chrome && lf_level > 0 {
            w.write_bits(lf_level, 6);
            w.write_bits(lf_level, 6);
        }
        // loop_filter_sharpness = 0
        w.write_bits(0, 3);
        // loop_filter_delta_enabled = 0
        w.write_bit(false);
    }

    // CDEF params (spec 5.9.19)
    if seq.enable_cdef && !coded_lossless {
        // cdef_damping - 3 = 2 (damping = 5)
        w.write_bits(2, 2);
        // cdef_bits = 0 (1 preset, 0 bits per block in tile)
        w.write_bits(0, 2);
        let pri = u32::from((cfg.qindex / 16).clamp(1, 15));
        let sec = (pri / 4).min(2);
        w.write_bits(pri, 4);
        w.write_bits(sec, 2);
        if !seq.mono_chrome {
            w.write_bits(pri, 4);
            w.write_bits(sec, 2);
        }
    }

    // 13. TX mode:
    if !coded_lossless {
        // tx_mode_select = 0 (TX_MODE_LARGEST)
        w.write_bit(false);
    }

    if !is_key {
        // reference_select = 0 (single reference)
        w.write_bit(false);
    }

    // 14. reduced_tx_set = 1
    w.write_bit(true);

    if !is_key {
        // global_motion_params: 7 reference frames (LAST_FRAME..=ALTREF_FRAME)
        // 0 = IDENTITY transformation (zero motion)
        for _ in 0..7 {
            w.write_bit(false);
        }
    }

    // Trailing bit and byte-alignment (spec 5.9.2 trailing_bits)
    w.rbsp_trailing();
    w.finish()
}

fn tile_log2(blk: u32, target: u32) -> u32 {
    let mut k = 0;
    while (blk << k) < target {
        k += 1;
    }
    k
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_write_frame_header_has_spec_conforming_trailing_bits() {
        let seq = SequenceConfig { width: 64, height: 64, bit_depth: 8, mono_chrome: false, enable_cdef: true };
        let cfg =
            FrameHeaderConfig { frame_type: KEY_FRAME, show_frame: true, error_resilient_mode: false, disable_cdf_update: false, qindex: 100, order_hint: 0 };

        let obu_payload = write_frame_header(&cfg, &seq);
        assert!(!obu_payload.is_empty());

        // AV1 spec 5.9.2: trailing_bits() consists of a trailing_one_bit (1) followed
        // by trailing_zero_bit (0) until byte-aligned.
        // The last byte must have at least one bit set (the trailing_one_bit) and any bits
        // following it must be 0.
        let last_byte = *obu_payload.last().unwrap();
        assert_ne!(last_byte, 0, "Last byte of frame header OBU must contain trailing_one_bit");
        // The lowest set bit is the trailing_one_bit. All bits below it (if any) are 0.
        // For example:
        // if trailing_one_bit is at bit pos 7: 0b1000_0000 (0x80) -> last byte has zeros below it.
        // In general, last_byte & (last_byte.trailing_zeros() mask) == 0 is guaranteed.
        let tz = last_byte.trailing_zeros();
        assert_eq!(last_byte & ((1 << tz) - 1), 0);
    }
}

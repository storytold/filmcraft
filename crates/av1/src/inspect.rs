//! Checked restart evidence from our own uncompressed-header parser (spec 5.9, 7.5).
use crate::bits::BitReader;
use crate::header::{HeaderState, LoopFilterDeltas, RefInfo, SegmentationFeatures, default_gm_params};
use crate::{Error, FrameHeader, Result, SequenceHeader};

/// An independently restartable, shown key frame's declared geometry.
/// This proves the uncompressed header, not its compressed tile payload.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct IndependentFrameInfo {
    pub size: (u32, u32),
    /// Uncompressed-header bits consumed, excluding OBU trailing bits / byte alignment.
    pub header_bits: usize,
}

/// Inspect a base-layer shown key frame with fresh reference state, without tile decoding or
/// workers. Unsupported restart forms return `None`; malformed supported headers return errors.
/// Inspection is limited to 16384 samples per dimension, 128 Mi pixels and 14 size syntax bits.
pub fn independent_frame_header_info(sequence: &SequenceHeader, payload: &[u8], temporal_id: u8, spatial_id: u8) -> Result<Option<IndependentFrameInfo>> {
    let s = sequence;
    // SequenceHeader is public: validate its numeric fields before calling parser arithmetic.
    if s.profile > 2
        || !(1..=14).contains(&s.frame_width_bits)
        || !(1..=14).contains(&s.frame_height_bits)
        || s.max_frame_width == 0
        || s.max_frame_height == 0
        || s.max_frame_width > 16_384
        || s.max_frame_height > 16_384
        || s.max_frame_width.checked_mul(s.max_frame_height).is_none_or(|n| n > 128 * 1024 * 1024)
        || s.max_frame_width > (1 << s.frame_width_bits)
        || s.max_frame_height > (1 << s.frame_height_bits)
        || s.operating_points_cnt_minus_1 >= 32
        || s.order_hint_bits > 8
        || (s.enable_order_hint && s.order_hint_bits == 0)
        || s.seq_force_screen_content_tools > 2
        || s.seq_force_integer_mv > 2
        || !matches!(s.color.bit_depth, 8 | 10 | 12)
        || s.color.num_planes != if s.color.mono_chrome { 1 } else { 3 }
        || s.color.subsampling_x > 1
        || s.color.subsampling_y > 1
        || s.color.chroma_sample_position > 3
        || (s.reduced_still_picture_header && !s.still_picture)
    {
        return Err(Error::Invalid("independent header sequence bounds"));
    }
    if temporal_id != 0
        || spatial_id != 0
        || s.operating_points_cnt_minus_1 != 0
        || s.op_idc != 0
        || s.operating_point_idc.first().copied() != Some(0)
        || s.timing_info_present
        || s.decoder_model_info_present
        || s.frame_id_numbers_present
    {
        return Ok(None);
    }
    // Decline dependent forms before reference-dependent parsing. This prefix is only a filter:
    // accepting a key frame always requires the entire uncompressed header below.
    if !s.reduced_still_picture_header {
        let mut prefix = BitReader::new(payload);
        if prefix.flag()? {
            return Ok(None);
        }
        if prefix.f(2)? != 0 || !prefix.flag()? {
            return Ok(None);
        }
    }
    let mut refs: [RefInfo; 8] = std::array::from_fn(|_| RefInfo::default());
    let mut state = HeaderState {
        seq: s,
        refs: &mut refs,
        lf_deltas: LoopFilterDeltas::defaults(),
        seg_features: SegmentationFeatures::default(),
        prev_gm_params: default_gm_params(),
        current_frame_id: 0,
    };
    // Entropy in a FRAME OBU may be large. Only a bounded prefix can be uncompressed header.
    let prefix = payload.get(..payload.len().min(65_536)).ok_or(Error::Truncated)?;
    let mut r = BitReader::new(prefix);
    let h = FrameHeader::parse(&mut r, &mut state, 0, 0)?;
    if h.show_existing_frame || h.frame_type != 0 || !h.show_frame || h.use_superres || h.render_width != h.upscaled_width || h.render_height != h.frame_height
    {
        return Ok(None);
    }
    if h.frame_width > s.max_frame_width || h.frame_height > s.max_frame_height {
        return Err(Error::Invalid("independent frame exceeds sequence maximum"));
    }
    Ok(Some(IndependentFrameInfo { size: (h.upscaled_width, h.frame_height), header_bits: r.position() }))
}

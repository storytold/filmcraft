//! Bounded checked AV1 parameter evidence. No hardware acceptance policy lives here.
use super::{FrameCodec, FrameStreamInfo, av1_color};
use crate::{CodecError, Result};
use filmcraft_av1::SequenceHeader;

const MAX_CONFIGURATION: usize = 65_536;
const MAX_OBUS: usize = 4096;

/// The picture-format difference declared by in-band AV1 sequence headers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Av1ParameterDifference {
    Unchanged,
    /// Only colour primaries, transfer characteristics and matrix coefficients differ.
    ColorDescription {
        before: (u8, u8, u8),
        after: (u8, u8, u8),
    },
    Structural,
    /// A present sequence header failed parsing; it is never treated as an absent header.
    InvalidHeader,
}

fn invalid(message: &'static str) -> CodecError {
    CodecError::Decode(message.into())
}

#[derive(Clone, Copy)]
struct Obu<'a> {
    kind: u8,
    payload: &'a [u8],
    temporal_id: u8,
    spatial_id: u8,
    has_size: bool,
}

/// Zero-allocation low-overhead OBU walker. Every advertised payload and the complete tail
/// are checked; only the final size-less OBU may consume the remaining container sample.
struct Obus<'a> {
    data: &'a [u8],
    offset: usize,
    count: usize,
}
impl<'a> Obus<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, offset: 0, count: 0 }
    }
    fn next(&mut self) -> Result<Option<Obu<'a>>> {
        let Some(&header) = self.data.get(self.offset) else {
            return Ok(None);
        };
        if self.count >= MAX_OBUS {
            return Err(invalid("AV1 sample exceeds 4096 OBU inspection limit"));
        }
        self.count += 1;
        if header & 0x81 != 0 {
            return Err(invalid("invalid AV1 OBU forbidden/reserved bit"));
        }
        let mut offset = self.offset.checked_add(1).ok_or_else(|| invalid("AV1 OBU offset overflow"))?;
        let (temporal_id, spatial_id) = if header & 4 != 0 {
            let extension = *self.data.get(offset).ok_or_else(|| invalid("truncated AV1 OBU extension"))?;
            if extension & 7 != 0 {
                return Err(invalid("invalid AV1 OBU extension reserved bits"));
            }
            offset = offset.checked_add(1).ok_or_else(|| invalid("AV1 OBU offset overflow"))?;
            (extension >> 5, (extension >> 3) & 3)
        } else {
            (0, 0)
        };
        let has_size = header & 2 != 0;
        let size = if has_size {
            let mut value = 0u64;
            let mut terminated = false;
            for index in 0..8 {
                let byte = *self.data.get(offset).ok_or_else(|| invalid("truncated AV1 OBU length"))?;
                offset = offset.checked_add(1).ok_or_else(|| invalid("AV1 OBU offset overflow"))?;
                value |= u64::from(byte & 127) << (index * 7);
                if byte & 128 == 0 {
                    terminated = true;
                    break;
                }
            }
            if !terminated {
                return Err(invalid("unterminated AV1 OBU length"));
            }
            let value = u32::try_from(value).map_err(|_| invalid("AV1 OBU length exceeds 32 bits"))?;
            usize::try_from(value).map_err(|_| invalid("AV1 OBU length exceeds address space"))?
        } else {
            self.data.len().checked_sub(offset).ok_or_else(|| invalid("truncated AV1 OBU header"))?
        };
        let end = offset.checked_add(size).ok_or_else(|| invalid("AV1 OBU payload length overflow"))?;
        let payload = self.data.get(offset..end).ok_or_else(|| invalid("truncated AV1 OBU payload"))?;
        self.offset = end;
        Ok(Some(Obu { kind: (header >> 3) & 15, payload, temporal_id, spatial_id, has_size }))
    }
}

fn description(h: &SequenceHeader) -> (u8, u8, u8) {
    (h.color.color_primaries, h.color.transfer_characteristics, h.color.matrix_coefficients)
}
fn difference(before: &SequenceHeader, after: &SequenceHeader) -> Av1ParameterDifference {
    let mut normalized_after = after.clone();
    normalized_after.color.color_primaries = before.color.color_primaries;
    normalized_after.color.transfer_characteristics = before.color.transfer_characteristics;
    normalized_after.color.matrix_coefficients = before.color.matrix_coefficients;
    if before != &normalized_after {
        Av1ParameterDifference::Structural
    } else if description(before) != description(after) {
        Av1ParameterDifference::ColorDescription { before: description(before), after: description(after) }
    } else {
        Av1ParameterDifference::Unchanged
    }
}

fn checked_sequence(payload: &[u8]) -> Result<SequenceHeader> {
    if payload.len() > MAX_CONFIGURATION {
        return Err(invalid("AV1 sequence header exceeds 64 KiB inspection limit"));
    }
    SequenceHeader::parse_checked(payload).map_err(|e| CodecError::Decode(e.to_string()))
}

/// Specified values must have an existing software interpretation; unspecified (2) deliberately
/// uses that same software default. Container nclx precedence is outside this API.
fn supported_color(h: &SequenceHeader) -> bool {
    let (p, t, m) = description(h);
    matches!(p, 1 | 2 | 5 | 6 | 9 | 12)
        && (t == 2 || filmcraft_color::Transfer::from_code(t).is_some())
        && (m == 2 || filmcraft_color::Matrix::from_code(m).is_some())
}

fn copy_bounded(bytes: &[u8]) -> Result<Vec<u8>> {
    if bytes.len() > MAX_CONFIGURATION {
        return Err(invalid("AV1 configuration exceeds 64 KiB inspection limit"));
    }
    let mut copy = Vec::new();
    copy.try_reserve_exact(bytes.len()).map_err(|_| invalid("AV1 configuration allocation failed"))?;
    copy.extend_from_slice(bytes);
    Ok(copy)
}

fn effective_configuration(payload: &[u8]) -> Result<Vec<u8>> {
    let mut length = payload.len();
    let mut size_bytes = [0u8; 3];
    let mut used = 0usize;
    loop {
        let byte = (length & 127) as u8;
        length >>= 7;
        let slot = size_bytes.get_mut(used).ok_or_else(|| invalid("AV1 configuration length overflow"))?;
        *slot = byte | if length == 0 { 0 } else { 128 };
        used = used.checked_add(1).ok_or_else(|| invalid("AV1 configuration length overflow"))?;
        if length == 0 {
            break;
        }
    }
    let count = payload.len().checked_add(used).and_then(|n| n.checked_add(1)).ok_or_else(|| invalid("AV1 configuration length overflow"))?;
    if count > MAX_CONFIGURATION {
        return Err(invalid("AV1 effective configuration exceeds 64 KiB inspection limit"));
    }
    let mut out = Vec::new();
    out.try_reserve_exact(count).map_err(|_| invalid("AV1 configuration allocation failed"))?;
    out.push(0x0a);
    out.extend_from_slice(size_bytes.get(..used).ok_or_else(|| invalid("AV1 configuration length overflow"))?);
    out.extend_from_slice(payload);
    Ok(out)
}

fn check_frame_suffix(payload: &[u8], bits: usize, frame_obu: bool) -> Result<()> {
    if !frame_obu && payload.len() > MAX_CONFIGURATION {
        return Err(invalid("AV1 frame header exceeds 64 KiB inspection limit"));
    }
    let end = if frame_obu { bits.checked_add(7).map(|n| n & !7) } else { payload.len().checked_mul(8) }
        .ok_or_else(|| invalid("AV1 frame header length overflow"))?;
    if !frame_obu && bits >= end {
        return Err(invalid("missing AV1 frame header trailing bits"));
    }
    for position in bits..end {
        let byte = *payload.get(position / 8).ok_or_else(|| invalid("truncated AV1 frame header suffix"))?;
        let bit = (byte >> (7 - position % 8)) & 1;
        if bit != u8::from(!frame_obu && position == bits) {
            return Err(invalid("invalid AV1 frame header trailing/alignment bits"));
        }
    }
    Ok(())
}

impl FrameStreamInfo {
    /// Classify every present sequence header using checked OBU framing and bounded parsing.
    /// Structural/invalid differences take precedence over colour differences. Existing hardware
    /// `parameters_changed` callers retain their current conservative comparison policy.
    pub fn av1_parameter_difference(&self, sample: &[u8]) -> Result<Av1ParameterDifference> {
        if self.codec != FrameCodec::Av1 {
            return Err(CodecError::Unsupported("AV1 parameter comparison for another codec".into()));
        }
        let before = self.sequence_header.as_deref().map(checked_sequence).transpose();
        let mut result = Av1ParameterDifference::Unchanged;
        let mut walker = Obus::new(sample);
        while let Some(obu) = walker.next()? {
            if obu.kind != 1 {
                continue;
            }
            if obu.payload.len() > MAX_CONFIGURATION {
                return Err(invalid("AV1 sequence header exceeds 64 KiB inspection limit"));
            }
            let after = checked_sequence(obu.payload);
            let next = match (&before, after) {
                (Ok(Some(before)), Ok(after)) => difference(before, &after),
                _ => Av1ParameterDifference::InvalidHeader,
            };
            result = match (result, next) {
                (Av1ParameterDifference::InvalidHeader, _) | (_, Av1ParameterDifference::InvalidHeader) => Av1ParameterDifference::InvalidHeader,
                (Av1ParameterDifference::Structural, _) | (_, Av1ParameterDifference::Structural) => Av1ParameterDifference::Structural,
                (Av1ParameterDifference::Unchanged, changed) => changed,
                (changed, Av1ParameterDifference::Unchanged) => changed,
                (a, b) if a == b => a,
                _ => Av1ParameterDifference::Structural,
            };
        }
        Ok(result)
    }

    /// Prepare effective metadata for a colour-only change before a fully checked shown key
    /// frame. The original info is immutable. This prepares evidence; it never adopts a native
    /// change itself. Metadata OBUs and special restart forms conservatively decline.
    pub fn initial_av1_color_candidate(&self, sample: &[u8]) -> Result<Option<Self>> {
        if self.codec != FrameCodec::Av1 {
            return Ok(None);
        }
        if self.config_obus.len() > MAX_CONFIGURATION {
            return Err(invalid("AV1 configuration exceeds 64 KiB inspection limit"));
        }
        let before = self.sequence_header.as_deref().map(checked_sequence).transpose()?;
        let mut sequence: Option<(SequenceHeader, &[u8])> = None;
        let mut frame = None;
        let mut decline = false;
        let mut sample_obus = Obus::new(sample);
        while let Some(obu) = sample_obus.next()? {
            if obu.temporal_id != 0 || obu.spatial_id != 0 {
                decline = true;
            }
            match obu.kind {
                1 => {
                    let h = checked_sequence(obu.payload)?;
                    if frame.is_some() {
                        decline = true;
                    }
                    if let Some((previous, _)) = &sequence {
                        if *previous != h {
                            decline = true;
                        }
                    } else {
                        sequence = Some((h, obu.payload));
                    }
                }
                3 | 6 => {
                    if frame.is_none() {
                        frame = Some(obu);
                        if sequence.is_none() {
                            decline = true;
                        }
                    } else {
                        decline = true;
                    } // first bounded form: one frame per temporal unit
                }
                2 => {
                    if !obu.payload.is_empty() {
                        return Err(invalid("nonempty AV1 temporal delimiter"));
                    }
                }
                4 => {
                    if frame.is_none() {
                        decline = true;
                    }
                } // tile payload: framing checked, entropy not inspected
                15 => {} // padding
                _ => decline = true,
            }
        }
        // Validate stored configuration too: do not discard unexamined metadata or bad tails.
        let mut configuration = Obus::new(&self.config_obus);
        let mut config_sequence = None;
        while let Some(obu) = configuration.next()? {
            if obu.kind == 1 {
                let h = checked_sequence(obu.payload)?;
                if config_sequence.is_some() {
                    decline = true;
                }
                config_sequence = Some(h);
                if !obu.has_size || obu.temporal_id != 0 || obu.spatial_id != 0 {
                    decline = true;
                }
            } else {
                decline = true;
            }
        }
        let (Some(before), Some(config_sequence), Some((after, payload)), Some(frame)) = (before, config_sequence, sequence, frame) else {
            return Ok(None);
        };
        if decline || config_sequence != before {
            return Ok(None);
        }
        if !matches!(difference(&before, &after), Av1ParameterDifference::ColorDescription { .. }) || !supported_color(&after) {
            return Ok(None);
        }
        // Public fields must still agree with the stored session configuration.
        if self.profile != before.profile
            || self.size != (before.max_frame_width, before.max_frame_height)
            || self.bit_depth != u32::from(before.color.bit_depth)
            || self.subsampling != (before.color.subsampling_x, before.color.subsampling_y)
            || self.mono != before.color.mono_chrome
        {
            return Ok(None);
        }
        let inspected = filmcraft_av1::independent_frame_header_info(&after, frame.payload, frame.temporal_id, frame.spatial_id)
            .map_err(|e| CodecError::Decode(e.to_string()))?;
        let Some(inspected) = inspected else {
            return Ok(None);
        };
        if inspected.size != self.size {
            return Ok(None);
        }
        check_frame_suffix(frame.payload, inspected.header_bits, frame.kind == 6)?;
        let cc = &after.color;
        Ok(Some(Self {
            codec: self.codec,
            profile: self.profile,
            bit_depth: self.bit_depth,
            subsampling: self.subsampling,
            mono: self.mono,
            size: self.size,
            config_obus: effective_configuration(payload)?,
            sequence_header: Some(copy_bounded(payload)?),
            color: Some(av1_color(self.size.0, self.size.1, cc.matrix_coefficients, cc.transfer_characteristics, cc.color_primaries, cc.color_range)),
            transfer: self.transfer,
            primaries: self.primaries,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn non_color_sequence_changes(base: &SequenceHeader) -> Vec<(String, SequenceHeader)> {
        let mut changes = Vec::new();
        macro_rules! changed {
            ($field:ident $(.$nested:ident)?, $value:expr) => {{
                let mut header = base.clone();
                header.$field $(.$nested)? = $value;
                changes.push((stringify!($field $(.$nested)?).to_owned(), header));
            }};
        }
        changed!(profile, 1);
        changed!(still_picture, true);
        changed!(reduced_still_picture_header, true);
        changed!(timing_info_present, true);
        changed!(decoder_model_info_present, true);
        changed!(equal_picture_interval, true);
        changed!(buffer_removal_time_length_minus_1, 1);
        changed!(frame_presentation_time_length_minus_1, 1);
        changed!(operating_points_cnt_minus_1, 1);
        for index in 0..32 {
            let mut header = base.clone();
            header.operating_point_idc[index] = 1;
            changes.push((format!("operating_point_idc[{index}]"), header));
            let mut header = base.clone();
            header.seq_level_idx[index] = 1;
            changes.push((format!("seq_level_idx[{index}]"), header));
            let mut header = base.clone();
            header.decoder_model_present_for_this_op[index] = true;
            changes.push((format!("decoder_model_present_for_this_op[{index}]"), header));
        }
        changed!(op_idc, 1);
        changed!(frame_width_bits, 1);
        changed!(frame_height_bits, 1);
        changed!(max_frame_width, 1);
        changed!(max_frame_height, 1);
        changed!(frame_id_numbers_present, true);
        changed!(delta_frame_id_length_minus_2, 1);
        changed!(additional_frame_id_length_minus_1, 1);
        changed!(use_128x128_superblock, true);
        changed!(enable_filter_intra, true);
        changed!(enable_intra_edge_filter, true);
        changed!(enable_interintra_compound, true);
        changed!(enable_masked_compound, true);
        changed!(enable_warped_motion, true);
        changed!(enable_dual_filter, true);
        changed!(enable_order_hint, true);
        changed!(enable_jnt_comp, true);
        changed!(enable_ref_frame_mvs, true);
        changed!(seq_force_screen_content_tools, 1);
        changed!(seq_force_integer_mv, 1);
        changed!(order_hint_bits, 1);
        changed!(enable_superres, true);
        changed!(enable_cdef, true);
        changed!(enable_restoration, true);
        changed!(color.bit_depth, 8);
        changed!(color.mono_chrome, true);
        changed!(color.num_planes, 3);
        changed!(color.color_range, true);
        changed!(color.subsampling_x, 1);
        changed!(color.subsampling_y, 1);
        changed!(color.chroma_sample_position, 1);
        changed!(color.separate_uv_delta_q, true);
        changed!(film_grain_params_present, true);
        changes
    }

    #[test]
    fn av1_format_comparison_checks_every_stored_non_color_sequence_field() {
        let base = SequenceHeader::default();
        let missed: Vec<_> = non_color_sequence_changes(&base)
            .into_iter()
            .filter_map(|(field, changed)| (difference(&base, &changed) != Av1ParameterDifference::Structural).then_some(field))
            .collect();
        assert!(missed.is_empty(), "non-colour sequence changes were not structural: {missed:?}");
    }

    #[test]
    fn av1_format_comparison_gives_structural_changes_precedence_over_color() {
        let base = SequenceHeader::default();
        let missed: Vec<_> = non_color_sequence_changes(&base)
            .into_iter()
            .filter_map(|(field, mut changed)| {
                changed.color.color_primaries = 9;
                changed.color.transfer_characteristics = 16;
                changed.color.matrix_coefficients = 9;
                (difference(&base, &changed) != Av1ParameterDifference::Structural).then_some(field)
            })
            .collect();
        assert!(missed.is_empty(), "structural changes were hidden by colour changes: {missed:?}");
    }

    #[test]
    fn av1_format_comparison_exempts_only_the_three_color_description_codes() {
        let mut base = SequenceHeader::default();
        base.color.color_primaries = 1;
        base.color.transfer_characteristics = 1;
        base.color.matrix_coefficients = 1;
        assert_eq!(difference(&base, &base), Av1ParameterDifference::Unchanged);
        for after in [(9, 1, 1), (1, 16, 1), (1, 1, 9), (9, 16, 9)] {
            let mut changed = base.clone();
            changed.color.color_primaries = after.0;
            changed.color.transfer_characteristics = after.1;
            changed.color.matrix_coefficients = after.2;
            assert_eq!(difference(&base, &changed), Av1ParameterDifference::ColorDescription { before: (1, 1, 1), after });
        }
    }

    #[test]
    fn bounded_configuration_copy_errors_before_oversized_allocation() {
        assert!(copy_bounded(&vec![0; MAX_CONFIGURATION + 1]).is_err());
        assert_eq!(copy_bounded(&[1, 2, 3]).unwrap(), [1, 2, 3]);
        assert!(effective_configuration(&vec![0; MAX_CONFIGURATION]).is_err());
    }

    #[test]
    fn av1_format_comparison_checks_each_chroma_and_plane_field() {
        let base = SequenceHeader::default();
        let mut planes = base.clone();
        planes.color.num_planes = 3;
        let mut subsampling_x = base.clone();
        subsampling_x.color.subsampling_x = 1;
        let mut subsampling_y = base.clone();
        subsampling_y.color.subsampling_y = 1;
        for changed in [planes, subsampling_x, subsampling_y] {
            assert_eq!(difference(&base, &changed), Av1ParameterDifference::Structural);
        }
    }
}

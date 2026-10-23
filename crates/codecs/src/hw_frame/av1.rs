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

/// Whether an accepted AV1 sequence needs a new bounded configuration for future replay.
/// This tracks coding tools without granting a new colour/structural-format accommodation.
#[derive(Debug)]
pub enum Av1ReplayConfigurationRefresh {
    NotNeeded,
    Compatible(FrameStreamInfo),
    /// A changed sequence lacks the supported independent-frame proof. Native replay must
    /// decline this input rather than retain configuration which would no longer describe it.
    Unsupported,
}

#[derive(Clone, Copy)]
enum RestartCandidate {
    InitialColor,
    ReplayRefresh,
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
    has_extension: bool,
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
        let kind = (header >> 3) & 15;
        let has_extension = header & 4 != 0;
        if has_extension && matches!(kind, 1 | 2) {
            return Err(invalid("AV1 sequence/temporal delimiter OBU cannot have an extension"));
        }
        let mut offset = self.offset.checked_add(1).ok_or_else(|| invalid("AV1 OBU offset overflow"))?;
        let (temporal_id, spatial_id) = if has_extension {
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
        Ok(Some(Obu { kind, payload, temporal_id, spatial_id, has_size, has_extension }))
    }
}

fn description(h: &SequenceHeader) -> (u8, u8, u8) {
    (h.color.color_primaries, h.color.transfer_characteristics, h.color.matrix_coefficients)
}
fn difference(before: &SequenceHeader, after: &SequenceHeader) -> Av1ParameterDifference {
    let a = &before.color;
    let b = &after.color;
    if before.profile != after.profile
        || before.max_frame_width != after.max_frame_width
        || before.max_frame_height != after.max_frame_height
        || a.bit_depth != b.bit_depth
        || a.mono_chrome != b.mono_chrome
        || a.num_planes != b.num_planes
        || a.subsampling_x != b.subsampling_x
        || a.subsampling_y != b.subsampling_y
        || a.color_range != b.color_range
        || a.chroma_sample_position != b.chroma_sample_position
        || a.separate_uv_delta_q != b.separate_uv_delta_q
    {
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
        let mut op_idc = before.as_ref().ok().and_then(|header| header.as_ref()).map(|header| header.op_idc);
        let mut result = Av1ParameterDifference::Unchanged;
        let mut walker = Obus::new(sample);
        while let Some(obu) = walker.next()? {
            if obu.has_extension && op_idc == Some(0) {
                return Err(invalid("AV1 OBU extension is forbidden when operating point idc is zero"));
            }
            if obu.kind != 1 {
                continue;
            }
            if obu.payload.len() > MAX_CONFIGURATION {
                return Err(invalid("AV1 sequence header exceeds 64 KiB inspection limit"));
            }
            let after = checked_sequence(obu.payload);
            if let Ok(header) = &after {
                op_idc = Some(header.op_idc);
            }
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
        self.checked_av1_restart_candidate(sample, RestartCandidate::InitialColor)
    }

    /// Refresh the current accepted replay configuration for a coding-tool-only restart.
    /// This is used only after native metadata adoption made original av1C insufficient for
    /// replay. Equal raw sequence headers and headerless continuation require no refresh.
    /// A different sequence must preserve every existing coded-format/colour field and pass
    /// the same complete supported shown-key-frame proof as initial colour accommodation.
    /// Malformed framing returns an error; unsupported changed forms must fall back.
    /// Sequence/configuration copies remain fallible and capped at 64 KiB; no container/native
    /// policy is changed, and this method does not authorize another colour adoption window.
    pub fn av1_replay_configuration_refresh(&self, sample: &[u8]) -> Result<Av1ReplayConfigurationRefresh> {
        if self.codec != FrameCodec::Av1 {
            return Ok(Av1ReplayConfigurationRefresh::Unsupported);
        }
        if self.config_obus.len() > MAX_CONFIGURATION {
            return Err(invalid("AV1 configuration exceeds 64 KiB inspection limit"));
        }
        let Some(current) = self.sequence_header.as_deref() else {
            return Ok(Av1ReplayConfigurationRefresh::Unsupported);
        };
        let mut op_idc = checked_sequence(current)?.op_idc;
        let mut changed = false;
        let mut walker = Obus::new(sample);
        while let Some(obu) = walker.next()? {
            if obu.has_extension && op_idc == 0 {
                return Err(invalid("AV1 OBU extension is forbidden when operating point idc is zero"));
            }
            if obu.kind == 1 {
                op_idc = checked_sequence(obu.payload)?.op_idc;
                changed |= obu.payload != current;
            }
        }
        if !changed {
            return Ok(Av1ReplayConfigurationRefresh::NotNeeded);
        }
        Ok(match self.checked_av1_restart_candidate(sample, RestartCandidate::ReplayRefresh)? {
            Some(candidate) => Av1ReplayConfigurationRefresh::Compatible(candidate),
            None => Av1ReplayConfigurationRefresh::Unsupported,
        })
    }

    fn checked_av1_restart_candidate(&self, sample: &[u8], kind: RestartCandidate) -> Result<Option<Self>> {
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
            if obu.has_extension || obu.temporal_id != 0 || obu.spatial_id != 0 {
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
                4 | 15 => {} // tile payload / padding: framing checked, entropy not inspected
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
                if !obu.has_size || obu.has_extension || obu.temporal_id != 0 || obu.spatial_id != 0 {
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
        let compatible = matches!(
            (kind, difference(&before, &after)),
            (RestartCandidate::InitialColor, Av1ParameterDifference::ColorDescription { .. })
                | (RestartCandidate::ReplayRefresh, Av1ParameterDifference::Unchanged)
        );
        if !compatible || !supported_color(&after) {
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

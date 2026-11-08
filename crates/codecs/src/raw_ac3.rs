//! Bounded raw AC-3/E-AC-3 import, retaining raw-stream priming and final frame padding.
use craft_audio::ChannelLayout;
use craft_audio::ac3::{ProgrammeDecoder, parse_header};

use crate::{CodecError, Result};

pub(crate) fn decode(bytes: &[u8]) -> Result<(u32, Vec<Vec<f32>>)> {
    let first = parse_header(bytes).map_err(|e| CodecError::Decode(e.to_string()))?;
    let mut legacy = filmcraft_ac3::Decoder::new();
    let mut programme = ProgrammeDecoder::new(ChannelLayout::SURROUND_7_1);
    let mut expected_channels = None;
    let mut position = 0usize;
    let mut output: Vec<Vec<f32>> = Vec::new();
    let mut retained = 0usize;
    while position < bytes.len() {
        let header = parse_header(&bytes[position..]).map_err(|e| CodecError::Decode(e.to_string()))?;
        if !header.is_primary() || header.sample_rate != first.sample_rate || header.is_eac3() != first.is_eac3() {
            return Err(CodecError::Decode("raw AC-3/E-AC-3 changed programme or sample rate".into()));
        }
        if header.acmod != first.acmod || header.lfeon != first.lfeon {
            return Err(CodecError::Decode("raw AC-3/E-AC-3 changed speaker layout".into()));
        }
        let mut end =
            position.checked_add(header.frame_bytes).filter(|end| *end <= bytes.len()).ok_or_else(|| CodecError::Decode("truncated AC-3 syncframe".into()))?;
        let dependent = if end < bytes.len() {
            let next = parse_header(&bytes[end..]).map_err(|e| CodecError::Decode(e.to_string()))?;
            if next.is_eac3() && next.strmtyp == 1 {
                end = end
                    .checked_add(next.frame_bytes)
                    .filter(|end| *end <= bytes.len())
                    .ok_or_else(|| CodecError::Decode("truncated E-AC-3 dependent frame".into()))?;
                true
            } else {
                false
            }
        } else {
            false
        };
        let channels = if dependent || expected_channels == Some(8) {
            programme.decode(&bytes[position..end]).map_err(|e| CodecError::Decode(e.to_string()))?.channels
        } else {
            legacy.decode(&bytes[position..end]).map_err(|e| CodecError::Decode(e.to_string()))?.channels
        };
        if expected_channels.is_some_and(|count| count != channels.len()) {
            return Err(CodecError::Decode("raw AC-3/E-AC-3 changed speaker layout".into()));
        }
        expected_channels = Some(channels.len());
        let added = channels.iter().map(Vec::len).sum::<usize>();
        crate::audio::check_decoded_budget(retained, added)?;
        retained += added;
        if output.is_empty() {
            output.resize(channels.len(), Vec::new());
        }
        for (destination, channel) in output.iter_mut().zip(channels) {
            destination.extend(channel);
        }
        position = end;
    }
    Ok((first.sample_rate, output))
}

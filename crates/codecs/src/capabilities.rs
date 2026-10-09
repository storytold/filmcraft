//! Pure-Rust decoder implementation coverage for the media V1 subset.

use crate::{BuiltinVideoDecoder, CodecError, Result};
use filmcraft_media::capabilities::{CapabilityStatus, CodecCapability, FormatCapability, Operation, VideoCodec, VideoProfile};

pub fn software_decoder_capabilities() -> Result<Vec<CodecCapability>> {
    let definitions: [(VideoCodec, &str, BuiltinVideoDecoder, Vec<FormatCapability>, &str); 3] = [
        (
            VideoCodec::H264,
            "filmcraft-h264",
            BuiltinVideoDecoder::H264,
            vec![
                FormatCapability::new(VideoProfile::H264ConstrainedBaseline, 8),
                FormatCapability::new(VideoProfile::H264Main, 8),
                FormatCapability::new(VideoProfile::H264High, 8),
            ],
            "8-bit 4:2:0 frame coding; no FMO, SP/SI, data partitioning, MVC/SVC or lossless. Validate stream features.",
        ),
        (
            VideoCodec::Hevc,
            "filmcraft-hevc",
            BuiltinVideoDecoder::Hevc,
            vec![
                FormatCapability::new(VideoProfile::HevcMain, 8),
                FormatCapability::new(VideoProfile::HevcMain10, 8),
                FormatCapability::new(VideoProfile::HevcMain10, 10),
            ],
            "V1 Main/Main 10 4:2:0 subset. Validate stream features and allocation limits.",
        ),
        (
            VideoCodec::Av1,
            "filmcraft-av1",
            BuiltinVideoDecoder::Av1,
            vec![FormatCapability::new(VideoProfile::Av1Main, 8), FormatCapability::new(VideoProfile::Av1Main, 10)],
            "V1 Main 8/10-bit 4:2:0 subset; large-scale tile/tile-list OBUs excluded. Validate stream features.",
        ),
    ];
    let mut records = Vec::with_capacity(3);
    for (codec, backend, decoder, formats, note) in definitions {
        let mut record = CodecCapability::software(codec, Operation::Decode, backend, formats);
        record.reason = Some(note.into());
        if codec == VideoCodec::H264 {
            for format in &mut record.formats {
                format.limits.progressive_only = Some(true);
            }
        }
        if !crate::builtin_video_decoder_registered(decoder) {
            record.status = CapabilityStatus::Unavailable;
            record.evidence.clear();
            record.formats.clear();
            record.requires_configuration_validation = false;
            record.reason = Some("built-in software decoder is not registered".into());
        }
        record.validate().map_err(|e| CodecError::Unsupported(format!("software decoder capabilities: {e}")))?;
        records.push(record);
    }
    Ok(records)
}

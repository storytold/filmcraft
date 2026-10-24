//! Pure-Rust encoder implementation coverage for media V1.
//! This reports CPU coverage independently of registered hardware factories.

use crate::{ExportError, Format, Result};
use filmcraft_media::capabilities::{
    AccelerationMode, CapabilityLimits, CapabilityStatus, CodecCapability, EncoderControl, FormatCapability, InclusiveRange, Operation, RationalRate,
    VideoCodec, VideoProfile,
};

/// Return validated software encoder coverage independently of hardware factories.
/// H.264 is a configuration-dependent candidate; HEVC and AV1 are not implemented.
pub fn software_encoder_capabilities() -> Result<Vec<CodecCapability>> {
    // A compile-time connection to the actual built-in encoder factory.
    let _: crate::EncoderFactory = crate::h264_factory;
    let formats = [VideoProfile::H264ConstrainedBaseline, VideoProfile::H264Main, VideoProfile::H264High]
        .into_iter()
        .map(|profile| {
            let mut f = FormatCapability::new(profile, 8);
            f.limits = CapabilityLimits {
                width: Some(InclusiveRange { min: 2, max: 16384, step: 2 }),
                height: Some(InclusiveRange { min: 2, max: 16384, step: 2 }),
                max_frame_rate: Some(RationalRate { num: 1000, den: 1 }),
                progressive_only: Some(true),
            };
            f.encoder_controls = Some(vec![EncoderControl::Cbr, EncoderControl::VbrOnePass, EncoderControl::VbrTwoPass, EncoderControl::KeyframeInterval]);
            // Existing 8-bit H.264 can signal HDR. Complete HDR guarantees stay
            // unknown here; do not label it SDR-only or infer 10-bit output.
            f
        })
        .collect();
    let mut h264 = CodecCapability::software(VideoCodec::H264, Operation::Encode, "filmcraft-h264enc", formats);
    h264.reason = Some("Existing H.264 export subset; validate full export settings, level and resources.".into());
    if !Format::H264.has_builtin_encoder() {
        h264.status = CapabilityStatus::Unavailable;
        h264.evidence.clear();
        h264.formats.clear();
        h264.requires_configuration_validation = false;
        h264.reason = Some("built-in H.264 encoder is unavailable".into());
    }
    let missing = |codec, backend: &str, reason: &str| CodecCapability {
        codec,
        operation: Operation::Encode,
        backend_id: backend.into(),
        mode: AccelerationMode::Software,
        adapter_id: None,
        status: CapabilityStatus::NotImplemented,
        evidence: Vec::new(),
        formats: Vec::new(),
        requires_configuration_validation: false,
        reason: Some(reason.into()),
    };
    let records = vec![
        h264,
        missing(VideoCodec::Hevc, "filmcraft-hevcenc", "FilmCraft has no CPU HEVC encoder."),
        missing(VideoCodec::Av1, "filmcraft-av1enc", "FilmCraft has no CPU AV1 encoder."),
    ];
    for record in &records {
        record.validate().map_err(|e| ExportError::Unsupported(format!("software encoder capabilities: {e}")))?;
    }
    Ok(records)
}

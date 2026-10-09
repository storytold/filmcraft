//! Metadata for the real-world media round-trip corpus.
//!
//! The media itself stays outside git (and can be supplied by CI or a workstation). Keeping the
//! cases typed and named makes it possible for import → edit → export tests to share coverage
//! without silently collapsing back to synthetic-only fixtures.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MediaKind {
    PhoneVfr,
    CameraLongGop,
    ScreenCapture,
    DamagedContainer,
    OddContainer,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct RoundTripCase {
    pub id: &'static str,
    pub kind: MediaKind,
    pub extension: &'static str,
    pub requires_audio: bool,
}

pub const REAL_WORLD_CASES: &[RoundTripCase] = &[
    RoundTripCase { id: "phone-vfr-h264-aac", kind: MediaKind::PhoneVfr, extension: "mp4", requires_audio: true },
    RoundTripCase { id: "camera-long-gop-10bit", kind: MediaKind::CameraLongGop, extension: "mov", requires_audio: true },
    RoundTripCase { id: "screen-capture-vfr", kind: MediaKind::ScreenCapture, extension: "mkv", requires_audio: false },
    RoundTripCase { id: "truncated-mp4", kind: MediaKind::DamagedContainer, extension: "mp4", requires_audio: false },
    RoundTripCase { id: "matroska-h264-opus", kind: MediaKind::OddContainer, extension: "mkv", requires_audio: true },
];

pub fn case(id: &str) -> Option<RoundTripCase> {
    REAL_WORLD_CASES.iter().copied().find(|item| item.id == id)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_covers_import_risk_classes() {
        assert_eq!(REAL_WORLD_CASES.len(), 5);
        for kind in [MediaKind::PhoneVfr, MediaKind::CameraLongGop, MediaKind::ScreenCapture, MediaKind::DamagedContainer, MediaKind::OddContainer] {
            assert!(REAL_WORLD_CASES.iter().any(|item| item.kind == kind));
        }
        assert_eq!(case("phone-vfr-h264-aac").map(|item| item.extension), Some("mp4"));
    }
}

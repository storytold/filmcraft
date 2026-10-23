//! Compact original-container context; never retains or copies profile/media bytes.

use filmcraft_codecs::hw::{FrameStreamInfo, sample_entry_output_color_override};
use filmcraft_color::ColorInfo;
use filmcraft_isobmff::{ColorInfo as Colr, SampleEntry};

#[derive(Clone, Copy, Default)]
pub(super) struct Av1ContainerPolicy {
    known_entry: bool,
    output_override: Option<ColorInfo>,
    has_icc: bool,
    has_hdr_static: bool,
    has_unresolved_aperture: bool,
    has_gamma: bool,
}

impl Av1ContainerPolicy {
    pub(super) fn from_entry(entry: &SampleEntry, size: (u32, u32)) -> Self {
        let Some(video) = entry.video.as_ref() else { return Self::default() };
        Self {
            known_entry: true,
            output_override: sample_entry_output_color_override(entry, size.0, size.1),
            has_icc: matches!(video.color, Some(Colr::Icc { .. })),
            has_hdr_static: video.mastering_display.is_some() || video.content_light.is_some(),
            has_unresolved_aperture: video.clean_aperture.is_some(),
            has_gamma: video.gamma.is_some(),
        }
    }

    pub(super) fn permits(&self, candidate: &FrameStreamInfo) -> bool {
        self.known_entry
            && !self.has_icc
            && !self.has_hdr_static
            && !self.has_unresolved_aperture
            && !self.has_gamma
            && candidate.color.is_some()
            && self.output_override.is_none_or(|color| candidate.color == Some(color))
    }
}

//! Raw development settings ([`ProcessOptions`]): what a "Raw settings" panel can change about how
//! a DNG is developed. `ProcessOptions::default()` develops exactly as the camera file says (the
//! as-shot white balance, no exposure change, highlights clipped, the high-quality demosaic,
//! scene-linear Rec. 709, full resolution). Feature `serde` derives Serialize / Deserialize (missing
//! fields take their defaults).

/// White balance.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum WhiteBalance {
    /// The camera's AsShotNeutral / AsShotWhiteXY (D65 without either).
    #[default]
    AsShot,
    /// A white of `temperature` kelvin (1 000–15 000, on the Planckian locus) and `tint`
    /// (positive makes the picture more magenta, negative greener, as in common raw converters;
    /// ±150 is the useful range, D65 is about +10, see [`crate::color::TINT_PER_DUV`]). Needs the file's ColorMatrix; a DNG without one has no
    /// colour calibration to place the white in and keeps its as-shot balance.
    Custom { temperature: f64, tint: f64 },
}

/// What happens to values the sensor clipped.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum Highlights {
    /// White-balanced samples are clipped at the sensor's white, so clipped highlights stay
    /// neutral (no magenta or cyan casts).
    #[default]
    Clip,
    /// Reconstruct clipped channels from the unclipped ones. **Not implemented yet: develops
    /// exactly like [`Highlights::Clip`]**, so the setting can already be stored and offered.
    Recover,
}

/// Demosaic quality (CFA images only).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum DemosaicQuality {
    /// Bilinear interpolation: softer, a little faster (playback, scrubbing).
    Fast,
    /// Malvar–He–Cutler gradient-corrected interpolation for 2 × 2 Bayer patterns.
    #[default]
    High,
}

/// The colour space of developed pictures.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
pub enum OutputSpace {
    /// Scene-linear light with Rec. 709 / sRGB primaries and a D65 white; a white surface the
    /// sensor saw at its clipping level is 1.0 × 2^(BaselineExposure + exposure).
    #[default]
    SceneLinearRec709,
}

/// How to develop a DNG ([`crate::Dng::develop_with`]).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize, serde::Deserialize))]
#[cfg_attr(feature = "serde", serde(default))]
pub struct ProcessOptions {
    pub white_balance: WhiteBalance,
    /// Exposure change in stops, added to the file's BaselineExposure (the sum is limited to
    /// ±16 stops; a non-finite value counts as 0).
    pub exposure: f64,
    pub highlights: Highlights,
    pub demosaic: DemosaicQuality,
    pub output: OutputSpace,
    /// The smallest picture (width, height) the caller needs, e.g. a monitor or a proxy size.
    /// `None` develops at full resolution. With a size, the decoder may develop a smaller
    /// picture (half resolution when both dimensions still cover the target) that is never
    /// smaller than the target; [`crate::Image`]'s `width` and `height` give the size made,
    /// [`crate::Info`] still the full one.
    pub target_size: Option<(usize, usize)>,
    /// Apply the file's OpcodeList1–3 (lens shading, warps, bad pixels, trims). **Off by default**:
    /// the opcode interpretations have not yet been compared with real camera files (see the
    /// README), so a file's lists are ignored unless this is set.
    pub apply_opcodes: bool,
}

impl ProcessOptions {
    /// The exposure change in stops (0 for a non-finite value).
    pub fn exposure_stops(&self) -> f64 {
        if self.exposure.is_finite() { self.exposure } else { 0.0 }
    }
}

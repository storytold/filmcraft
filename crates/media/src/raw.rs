//! Camera RAW development settings (Clip ▸ Source Settings… and Project Settings ▸ Camera RAW).
//!
//! The picture of a camera RAW clip is developed from the sensor data every time it is decoded,
//! so these are decode settings, not colour corrections. For R3D they are RED's IPP2 pipeline as
//! the RED SDK exposes it (`ImageProcessingSettings`): the RAW adjustments applied before
//! development (ISO, white balance, exposure), the output colour space and gamma curve, and for a
//! display-referred gamma the output tone map, highlight rolloff and HDR peak; then the
//! processing options (chroma noise reduction, flashing pixel adjustment, the clip's metadata
//! curves, creative LUT and CDL) and the decode quality.
//!
//! A clip either decodes with the project's Camera RAW settings (Decode Using ▸ Project, the
//! default of a clip that has none of its own) or with its own (Decode Using ▸ Clip). The
//! project's settings of a project made before they existed are [`RawDevelop::default`] (scene
//! linear Rec. 709, as shot), which is what every clip got then; a new project starts from
//! [`RawDevelop::STANDARD`] (Rec. 709, BT.1886).

use serde::{Deserialize, Serialize};

use filmcraft_color::{ColorSpace, Curve, LogCurve};

/// The decoder's colour space for the developed picture (the RGB primaries; RED IPP2's
/// `ImageColorSpace`). Only the spaces the compositor can interpret are offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RawColorSpace {
    #[default]
    Rec709,
    Rec2020,
    #[serde(rename = "p3d65")]
    P3D65,
    /// REDWideGamutRGB, the camera's wide gamut for grading.
    #[serde(rename = "redWideGamutRgb")]
    RedWideGamutRgb,
}

/// The decoder's gamma curve (RED IPP2's `ImageGammaCurve`). Only curves the compositor can
/// interpret are offered.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RawGamma {
    /// Scene-linear light, no output transform (the SDK's half-float output).
    #[default]
    Linear,
    /// The BT.1886 display curve, with the output tone map and highlight rolloff.
    #[serde(rename = "bt1886")]
    Bt1886,
    /// The sRGB display curve, with the output tone map and highlight rolloff.
    Srgb,
    /// RED Log3G10 for grading (REDWideGamutRGB only).
    #[serde(rename = "log3g10")]
    Log3G10,
    /// SMPTE ST 2084 (PQ) for HDR, with the HDR peak (Rec. 2020 only).
    #[serde(rename = "st2084")]
    St2084,
    /// Hybrid Log-Gamma for HDR (Rec. 2020 only).
    Hlg,
}

/// Output tone mapping of a display-referred RAW output.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RawToneMap {
    /// The setting recorded in the clip (or its RMD sidecar).
    #[default]
    Clip,
    None,
    Low,
    Medium,
    High,
}

/// Highlight rolloff of a display-referred RAW output (no effect on a log or linear output).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RawRollOff {
    /// The setting recorded in the clip (or its RMD sidecar).
    #[default]
    Clip,
    None,
    Hard,
    Medium,
    Soft,
    VerySoft,
}

/// Flashing pixel adjustment (RED SDK `FlashingPixelAdjust`; full and half premium decodes).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RawFlashing {
    #[default]
    Off,
    Mild,
    Medium,
    Strong,
}

/// Decode quality (RED SDK `VideoDecodeMode`): the largest decode a preview may use. A full-size
/// request (an export, a monitor at 100 %) always decodes at full resolution, premium; a preview
/// decodes at the smallest size that covers it, but never larger than this. The SDK has one decode
/// mode per request and does not tell playback from export, so neither does this setting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RawQuality {
    /// Full resolution, premium (a half-size preview decodes at half, premium).
    #[default]
    FullPremium,
    HalfPremium,
    /// Half resolution, good: faster than premium.
    HalfGood,
    QuarterGood,
    EighthGood,
    SixteenthGood,
}

/// Where a clip's development settings come from.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum DecodeUsing {
    /// The clip's own settings. What a clip with settings of its own from before this choice
    /// existed reads as.
    #[default]
    Clip,
    /// The project's Camera RAW settings (Project Settings ▸ Camera RAW).
    Project,
}

/// What a camera RAW decoder outputs: see the module documentation. Copy and comparable, so a
/// setting is a cache key and an undo comparison. Decoders ignore what they have no equivalent
/// for. [`RawDevelop::default`] is what every project made before these settings existed got.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RawDevelop {
    /// Clip settings only: decode with these (Clip) or with the project's (Project).
    pub decode_using: DecodeUsing,
    /// None = the project's decode quality (in the project's own settings: full, premium).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub quality: Option<RawQuality>,
    pub color_space: RawColorSpace,
    pub gamma: RawGamma,
    pub tone_map: RawToneMap,
    pub rolloff: RawRollOff,
    /// Peak of an ST 2084 output in cd/m² ([`RAW_HDR_PEAK_RANGE`]); None = the clip's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hdr_peak_nits: Option<u32>,
    /// Chroma noise reduction (full and half premium decodes); None = as recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub chroma_noise_reduction: Option<bool>,
    /// Flashing pixel adjustment; None = as recorded.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub flashing_pixel: Option<RawFlashing>,
    /// Apply the luma and RGB curves recorded in the clip (or its RMD).
    pub metadata_curves: bool,
    /// Apply the creative 3D LUT the clip (or its RMD) carries. Display and log gammas only.
    pub creative_lut: bool,
    /// Apply the clip's (or its RMD's) ASC CDL. Display and log gammas only.
    pub cdl: bool,
    /// ISO the sensor data is developed at, one of [`RAW_ISO_VALUES`]; None = as shot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub iso: Option<u32>,
    /// White balance colour temperature in Kelvin ([`RAW_KELVIN_RANGE`]); None = as shot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kelvin: Option<u32>,
    /// White balance tint ([`RAW_TINT_RANGE`], hundredths); None = as shot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub tint: Option<Hundredths>,
    /// Exposure adjustment in stops ([`RAW_EXPOSURE_RANGE`], hundredths); None = as shot.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exposure: Option<Hundredths>,
}

impl Default for RawDevelop {
    /// Scene-linear light in Rec. 709 primaries, as shot, no look: what every clip developed
    /// before these settings existed, and the Camera RAW settings of a project from then.
    fn default() -> Self {
        RawDevelop {
            decode_using: DecodeUsing::Clip,
            quality: None,
            color_space: RawColorSpace::Rec709,
            gamma: RawGamma::Linear,
            tone_map: RawToneMap::Clip,
            rolloff: RawRollOff::Clip,
            hdr_peak_nits: None,
            chroma_noise_reduction: None,
            flashing_pixel: None,
            metadata_curves: true,
            creative_lut: false,
            cdl: false,
            iso: None,
            kelvin: None,
            tint: None,
            exposure: None,
        }
    }
}

/// The output choice of Source Settings before colour space and gamma were separate lists
/// (project files and `clip.sourceSettings {output}` of early builds of this feature). Still read.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum RawOutput {
    LinearRec709,
    Rec709,
    #[serde(rename = "log3g10")]
    Log3G10,
}

impl RawOutput {
    pub const ALL: [RawOutput; 3] = [RawOutput::LinearRec709, RawOutput::Rec709, RawOutput::Log3G10];
    pub fn id(self) -> &'static str {
        match self {
            RawOutput::LinearRec709 => "linearRec709",
            RawOutput::Rec709 => "rec709",
            RawOutput::Log3G10 => "log3g10",
        }
    }
    /// The colour space and gamma curve the output stood for.
    pub fn space_gamma(self) -> (RawColorSpace, RawGamma) {
        match self {
            RawOutput::LinearRec709 => (RawColorSpace::Rec709, RawGamma::Linear),
            RawOutput::Rec709 => (RawColorSpace::Rec709, RawGamma::Bt1886),
            RawOutput::Log3G10 => (RawColorSpace::RedWideGamutRgb, RawGamma::Log3G10),
        }
    }
    pub fn parse(s: &str) -> Option<RawOutput> {
        Self::ALL.into_iter().find(|o| o.id().eq_ignore_ascii_case(s))
    }
}

/// How a [`RawDevelop`] reads: every field optional, and the `output` of the first version
/// mapped onto colour space and gamma when those are absent.
#[derive(Deserialize)]
#[serde(default, rename_all = "camelCase")]
struct RawDevelopIn {
    decode_using: DecodeUsing,
    quality: Option<RawQuality>,
    output: Option<RawOutput>,
    color_space: Option<RawColorSpace>,
    gamma: Option<RawGamma>,
    tone_map: RawToneMap,
    rolloff: RawRollOff,
    hdr_peak_nits: Option<u32>,
    chroma_noise_reduction: Option<bool>,
    flashing_pixel: Option<RawFlashing>,
    metadata_curves: Option<bool>,
    creative_lut: bool,
    cdl: bool,
    iso: Option<u32>,
    kelvin: Option<u32>,
    tint: Option<Hundredths>,
    exposure: Option<Hundredths>,
}

impl Default for RawDevelopIn {
    fn default() -> Self {
        let d = RawDevelop::default();
        RawDevelopIn {
            decode_using: d.decode_using,
            quality: None,
            output: None,
            color_space: None,
            gamma: None,
            tone_map: d.tone_map,
            rolloff: d.rolloff,
            hdr_peak_nits: None,
            chroma_noise_reduction: None,
            flashing_pixel: None,
            metadata_curves: None,
            creative_lut: false,
            cdl: false,
            iso: None,
            kelvin: None,
            tint: None,
            exposure: None,
        }
    }
}

impl<'de> Deserialize<'de> for RawDevelop {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let w = RawDevelopIn::deserialize(deserializer)?;
        let (space, gamma) = w.output.map(RawOutput::space_gamma).unwrap_or_default();
        Ok(RawDevelop {
            decode_using: w.decode_using,
            quality: w.quality,
            color_space: w.color_space.unwrap_or(space),
            gamma: w.gamma.unwrap_or(gamma),
            tone_map: w.tone_map,
            rolloff: w.rolloff,
            hdr_peak_nits: w.hdr_peak_nits,
            chroma_noise_reduction: w.chroma_noise_reduction,
            flashing_pixel: w.flashing_pixel,
            metadata_curves: w.metadata_curves.unwrap_or(true),
            creative_lut: w.creative_lut,
            cdl: w.cdl,
            iso: w.iso,
            kelvin: w.kelvin,
            tint: w.tint,
            exposure: w.exposure,
        })
    }
}

/// The ISO values a RAW decoder develops at (RED SDK 9.2.1 `ImageProcessingLimits::ISOList`; the
/// helper checks a request against the SDK's own list, so a future SDK's additions are refused
/// here first). The Nikon ZR records 64 to 25600.
pub const RAW_ISO_VALUES: [u32; 28] = [
    50, 64, 80, 100, 125, 160, 200, 250, 320, 400, 500, 640, 800, 1000, 1280, 1600, 2000, 2500, 3200, 4000, 5000, 6400, 8000, 10000, 12800, 16000, 20000, 25600,
];
/// White balance colour temperatures a RAW decoder accepts, in Kelvin (RED SDK `KelvinMin..Max`).
pub const RAW_KELVIN_RANGE: std::ops::RangeInclusive<u32> = 1700..=100_000;
/// White balance tints a RAW decoder accepts (RED SDK `TintMin..Max`).
pub const RAW_TINT_RANGE: std::ops::RangeInclusive<f64> = -100.0..=100.0;
/// Exposure adjustments a RAW decoder accepts, in stops (RED SDK `ExposureAdjustMin..Max`).
pub const RAW_EXPOSURE_RANGE: std::ops::RangeInclusive<f64> = -8.0..=8.0;
/// Peaks of an ST 2084 output, in cd/m² (the PQ curve's own range; the helper checks the SDK's
/// `HdrPeakNitsMin..Max` as well).
pub const RAW_HDR_PEAK_RANGE: std::ops::RangeInclusive<u32> = 100..=10_000;

/// A decimal with two places kept as an integer count of hundredths, so a setting that holds one
/// stays exactly comparable and hashable (a cache key, an undo comparison). It reads and writes as
/// a plain number (`-0.33`); anything that is not a finite number within ±1 000 000 is refused.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Hundredths(pub i32);

impl Hundredths {
    /// The nearest hundredth of `v`, None unless `v` is finite and within ±1 000 000.
    pub fn from_f64(v: f64) -> Option<Self> {
        if !v.is_finite() || v.abs() > 1_000_000.0 {
            return None;
        }
        Some(Hundredths((v * 100.0).round() as i32))
    }
    pub fn as_f64(self) -> f64 {
        f64::from(self.0) / 100.0
    }
}

impl Serialize for Hundredths {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error> {
        serializer.serialize_f64(self.as_f64())
    }
}

impl<'de> Deserialize<'de> for Hundredths {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> std::result::Result<Self, D::Error> {
        let v = f64::deserialize(deserializer)?;
        Hundredths::from_f64(v).ok_or_else(|| serde::de::Error::custom(format!("{v} is not a finite decimal within range")))
    }
}

/// The development values recorded in a RAW clip (or in its RMD sidecar, which overrides them),
/// which its decoder uses until Source Settings override them: ISO, white balance colour
/// temperature (Kelvin) and tint, and the exposure adjustment in stops (a camera ISO without an
/// SDK equivalent is recorded as the nearest one plus a small adjustment).
#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RawAsShot {
    pub iso: u32,
    pub kelvin: f32,
    pub tint: f32,
    pub exposure: f32,
}

/// What a RAW clip records beyond [`RawAsShot`], as its decoder reports it: the recorded output
/// choices and processing options (what "as recorded" means), whether it carries a creative LUT,
/// a CDL, curves or an HDRx track, its RMD sidecar, and the decoder's own ranges and defaults.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct RawClipInfo {
    /// The recorded colour space and gamma when they are ones offered here.
    pub color_space: Option<RawColorSpace>,
    pub gamma: Option<RawGamma>,
    /// The recorded tone map and rolloff (never `Clip`).
    pub tone_map: Option<RawToneMap>,
    pub rolloff: Option<RawRollOff>,
    pub hdr_peak_nits: Option<u32>,
    pub chroma_noise_reduction: bool,
    pub flashing_pixel: RawFlashing,
    /// The clip (or its RMD) carries a creative 3D LUT, and whether it asks for it applied.
    pub lut: bool,
    pub lut_enabled: bool,
    /// Whether the clip asks for its CDL applied, and the CDL: saturation, then slope, offset and
    /// power of red, green and blue.
    pub cdl_enabled: bool,
    pub cdl: [f32; 10],
    /// The clip records curves that are not the identity.
    pub curves: bool,
    /// The clip has an HDRx (second exposure) track to blend.
    pub hdrx: bool,
    /// The RMD sidecar the decoder read the clip's settings from, if there was one.
    pub rmd: Option<String>,
    /// Where the clip's RMD sidecar is (or would be written).
    pub rmd_path: String,
    /// The decoder's range and default of the HDR peak, and its default tone map and rolloff.
    pub hdr_peak_range: (u32, u32),
    pub hdr_peak_default: u32,
    pub tone_map_default: Option<RawToneMap>,
    pub rolloff_default: Option<RawRollOff>,
}

macro_rules! choice {
    ($ty:ident, $all:expr, $( $v:ident => $id:literal, $label:literal ;)*) => {
        impl $ty {
            pub const ALL: [$ty; $all] = [$($ty::$v),*];
            /// Stable id used in commands and project files.
            pub fn id(self) -> &'static str {
                match self { $($ty::$v => $id),* }
            }
            pub fn label(self) -> &'static str {
                match self { $($ty::$v => $label),* }
            }
            pub fn parse(s: &str) -> Option<$ty> {
                Self::ALL.into_iter().find(|o| o.id().eq_ignore_ascii_case(s))
            }
        }
    };
}

choice!(RawColorSpace, 4,
    Rec709 => "rec709", "Rec. 709";
    Rec2020 => "rec2020", "Rec. 2020";
    P3D65 => "p3d65", "P3 D65";
    RedWideGamutRgb => "redWideGamutRgb", "REDWideGamutRGB";
);
choice!(RawGamma, 6,
    Linear => "linear", "Linear";
    Bt1886 => "bt1886", "BT.1886";
    Srgb => "srgb", "sRGB";
    Log3G10 => "log3g10", "Log3G10";
    St2084 => "st2084", "ST 2084 (PQ)";
    Hlg => "hlg", "Hybrid Log-Gamma";
);
choice!(RawToneMap, 5,
    Clip => "clip", "As recorded";
    None => "none", "None";
    Low => "low", "Low";
    Medium => "medium", "Medium";
    High => "high", "High";
);
choice!(RawRollOff, 6,
    Clip => "clip", "As recorded";
    None => "none", "None";
    Hard => "hard", "Hard";
    Medium => "medium", "Medium";
    Soft => "soft", "Soft";
    VerySoft => "verySoft", "Very Soft";
);
choice!(RawFlashing, 4,
    Off => "off", "None";
    Mild => "mild", "Mild";
    Medium => "medium", "Medium";
    Strong => "strong", "Strong";
);
choice!(RawQuality, 6,
    FullPremium => "fullPremium", "Full Res. Premium";
    HalfPremium => "halfPremium", "Half Res. Premium";
    HalfGood => "halfGood", "Half Res. Good";
    QuarterGood => "quarterGood", "Quarter Res. Good";
    EighthGood => "eighthGood", "Eighth Res. Good";
    SixteenthGood => "sixteenthGood", "Sixteenth Res. Good";
);
choice!(DecodeUsing, 2,
    Clip => "clip", "Clip";
    Project => "project", "Project";
);

impl RawQuality {
    /// The smallest divisor a preview at this quality may decode at (1 full, 2 half, …).
    pub fn divisor(self) -> u32 {
        match self {
            RawQuality::FullPremium => 1,
            RawQuality::HalfPremium | RawQuality::HalfGood => 2,
            RawQuality::QuarterGood => 4,
            RawQuality::EighthGood => 8,
            RawQuality::SixteenthGood => 16,
        }
    }
    /// A half-resolution decode at this quality is the faster "good" one.
    pub fn half_good(self) -> bool {
        self == RawQuality::HalfGood
    }
}

impl RawGamma {
    /// A display gamma, which the output tone map and highlight rolloff shape.
    pub fn is_display(self) -> bool {
        matches!(self, RawGamma::Bt1886 | RawGamma::Srgb | RawGamma::St2084 | RawGamma::Hlg)
    }
    /// The curve the developed code values are linearised with (the compositor's own, so its
    /// interpretation and the decoder's output agree).
    pub fn curve(self) -> Curve {
        match self {
            RawGamma::Linear => Curve::Linear,
            RawGamma::Bt1886 => Curve::Sdr,
            RawGamma::Srgb => Curve::Srgb,
            RawGamma::Log3G10 => Curve::Log(LogCurve::Log3G10),
            RawGamma::St2084 => Curve::Pq,
            RawGamma::Hlg => Curve::Hlg,
        }
    }
}

impl RawDevelop {
    /// The project Camera RAW settings of a new project: IPP2 into Rec. 709 with the BT.1886
    /// display curve, the SDK's default output tone map and highlight rolloff (Medium and Soft
    /// in RED SDK 9.2.1, which the helper reports and a real-clip test checks), as shot.
    pub const STANDARD: RawDevelop = RawDevelop {
        decode_using: DecodeUsing::Clip,
        quality: None,
        color_space: RawColorSpace::Rec709,
        gamma: RawGamma::Bt1886,
        tone_map: RawToneMap::Medium,
        rolloff: RawRollOff::Soft,
        hdr_peak_nits: None,
        chroma_noise_reduction: None,
        flashing_pixel: None,
        metadata_curves: true,
        creative_lut: false,
        cdl: false,
        iso: None,
        kelvin: None,
        tint: None,
        exposure: None,
    };

    /// The settings a clip decodes with: its own (Decode Using ▸ Clip), or the project's
    /// (Decode Using ▸ Project, or no settings of its own), with the clip's decode quality when
    /// it has one, else the project's.
    pub fn effective(project: &RawDevelop, clip: Option<&RawDevelop>) -> RawDevelop {
        match clip {
            Some(c) if c.decode_using == DecodeUsing::Clip => RawDevelop { quality: c.quality.or(project.quality), ..*c },
            Some(c) => RawDevelop { decode_using: DecodeUsing::Project, quality: c.quality.or(project.quality), ..*project },
            None => RawDevelop { decode_using: DecodeUsing::Project, ..*project },
        }
    }

    /// The decode quality in effect (full, premium unless set).
    pub fn quality(&self) -> RawQuality {
        self.quality.unwrap_or_default()
    }

    /// A short stable text of the settings, for cache keys (`"rec709/bt1886/medium/soft"`, then
    /// whatever else is not the default: `"/iso800/k5600/t0/e-33"`, `"/q:halfGood"`…).
    pub fn key(&self) -> String {
        let mut key = format!("{}/{}/{}/{}", self.color_space.id(), self.gamma.id(), self.tone_map.id(), self.rolloff.id());
        if let Some(q) = self.quality {
            key.push_str(&format!("/q:{}", q.id()));
        }
        if let Some(p) = self.hdr_peak_nits {
            key.push_str(&format!("/peak{p}"));
        }
        if let Some(c) = self.chroma_noise_reduction {
            key.push_str(if c { "/cnr1" } else { "/cnr0" });
        }
        if let Some(f) = self.flashing_pixel {
            key.push_str(&format!("/fpa:{}", f.id()));
        }
        if !self.metadata_curves {
            key.push_str("/nocurves");
        }
        if self.creative_lut {
            key.push_str("/lut");
        }
        if self.cdl {
            key.push_str("/cdl");
        }
        if let Some(iso) = self.iso {
            key.push_str(&format!("/iso{iso}"));
        }
        if let Some(k) = self.kelvin {
            key.push_str(&format!("/k{k}"));
        }
        if let Some(t) = self.tint {
            key.push_str(&format!("/t{}", t.0));
        }
        if let Some(e) = self.exposure {
            key.push_str(&format!("/e{}", e.0));
        }
        key
    }
    /// Whether any RAW adjustment (ISO, Kelvin, tint, exposure) overrides the clip's own value.
    pub fn adjusts_raw(&self) -> bool {
        self.iso.is_some() || self.kelvin.is_some() || self.tint.is_some() || self.exposure.is_some()
    }
    /// The RAW adjustments back to as shot; the other settings stay.
    pub fn with_as_shot(self) -> Self {
        RawDevelop { iso: None, kelvin: None, tint: None, exposure: None, ..self }
    }
    /// Whether the decoder can develop into `space` with `gamma` and the compositor interpret the
    /// result: display curves need display primaries, Log3G10 REDWideGamutRGB, the HDR curves
    /// Rec. 2020; linear light goes with any of them.
    pub fn supports(space: RawColorSpace, gamma: RawGamma) -> bool {
        match gamma {
            RawGamma::Linear => true,
            RawGamma::Bt1886 | RawGamma::Srgb => space != RawColorSpace::RedWideGamutRgb,
            RawGamma::Log3G10 => space == RawColorSpace::RedWideGamutRgb,
            RawGamma::St2084 | RawGamma::Hlg => space == RawColorSpace::Rec2020,
        }
    }
    /// A gamma curve that goes with `space`, for a colour space chosen on its own: Log3G10 for
    /// REDWideGamutRGB, BT.1886 for the display spaces.
    pub fn gamma_for(space: RawColorSpace) -> RawGamma {
        if space == RawColorSpace::RedWideGamutRgb { RawGamma::Log3G10 } else { RawGamma::Bt1886 }
    }
    /// A colour space that goes with `gamma`, for a gamma curve chosen on its own:
    /// REDWideGamutRGB for Log3G10, Rec. 2020 for the HDR curves, Rec. 709 otherwise.
    pub fn space_for(gamma: RawGamma) -> RawColorSpace {
        match gamma {
            RawGamma::Log3G10 => RawColorSpace::RedWideGamutRgb,
            RawGamma::St2084 | RawGamma::Hlg => RawColorSpace::Rec2020,
            _ => RawColorSpace::Rec709,
        }
    }
    /// The colour space interpretation the compositor needs for frames developed with these
    /// settings, which the decoder hands over as linear light in the developed primaries; None
    /// when the frames' own linear Rec. 709 tag says it (no interpretation, the fast path).
    pub fn interpretation(&self) -> Option<ColorSpace> {
        match (self.color_space, self.gamma) {
            (RawColorSpace::Rec709, _) => None,
            (RawColorSpace::Rec2020, RawGamma::St2084) => Some(ColorSpace::Rec2100Pq),
            (RawColorSpace::Rec2020, RawGamma::Hlg) => Some(ColorSpace::Rec2100Hlg),
            (RawColorSpace::Rec2020, _) => Some(ColorSpace::Rec2020),
            (RawColorSpace::P3D65, _) => Some(ColorSpace::P3D65),
            // scene-linear camera gamut: the compositor tone maps it as log media (the frames are
            // linear already, so only the gamut and tone stages apply)
            (RawColorSpace::RedWideGamutRgb, _) => Some(ColorSpace::Log3G10Rwg),
        }
    }
    /// Every interpretation [`interpretation`](Self::interpretation) can give (so an engine can
    /// tell one it set from a person's own Interpret Footage choice).
    pub const INTERPRETATIONS: [ColorSpace; 5] =
        [ColorSpace::Rec2100Pq, ColorSpace::Rec2100Hlg, ColorSpace::Rec2020, ColorSpace::P3D65, ColorSpace::Log3G10Rwg];
    /// The creative LUT and the CDL apply (display and log gammas: the SDK's linear half-float
    /// output ignores them).
    pub fn grades_apply(&self) -> bool {
        self.gamma != RawGamma::Linear
    }
    /// Every set value is one the decoder accepts ([`RAW_ISO_VALUES`] and the ranges above) and
    /// the colour space and gamma go together ([`supports`](Self::supports)); the first offending
    /// one otherwise. Project files and command parameters are checked with this before a decoder
    /// sees them.
    pub fn validate(&self) -> std::result::Result<(), String> {
        if !Self::supports(self.color_space, self.gamma) {
            return Err(format!("the {} gamma curve does not go with the {} colour space", self.gamma.label(), self.color_space.label()));
        }
        if let Some(iso) = self.iso
            && !RAW_ISO_VALUES.contains(&iso)
        {
            return Err(format!("ISO {iso} is not one of the decoder's ISO values ({})", RAW_ISO_VALUES.map(|v| v.to_string()).join(", ")));
        }
        if let Some(k) = self.kelvin
            && !RAW_KELVIN_RANGE.contains(&k)
        {
            return Err(format!("colour temperature {k} K is outside {}..{} K", RAW_KELVIN_RANGE.start(), RAW_KELVIN_RANGE.end()));
        }
        if let Some(t) = self.tint
            && !RAW_TINT_RANGE.contains(&t.as_f64())
        {
            return Err(format!("tint {} is outside {}..{}", t.as_f64(), RAW_TINT_RANGE.start(), RAW_TINT_RANGE.end()));
        }
        if let Some(e) = self.exposure
            && !RAW_EXPOSURE_RANGE.contains(&e.as_f64())
        {
            return Err(format!("exposure adjustment {} is outside {}..{} stops", e.as_f64(), RAW_EXPOSURE_RANGE.start(), RAW_EXPOSURE_RANGE.end()));
        }
        if let Some(p) = self.hdr_peak_nits
            && !RAW_HDR_PEAK_RANGE.contains(&p)
        {
            return Err(format!("HDR peak {p} cd/m² is outside {}..{}", RAW_HDR_PEAK_RANGE.start(), RAW_HDR_PEAK_RANGE.end()));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    /// The JSON names are the command's ids, so a project file and `clip.sourceSettings` agree.
    #[test]
    fn json_names_are_the_command_ids() {
        fn check<T: Serialize + Copy + std::fmt::Debug + PartialEq>(all: &[T], id: fn(T) -> &'static str, parse: fn(&str) -> Option<T>) {
            for v in all {
                assert_eq!(serde_json::to_value(v).unwrap(), serde_json::Value::String(id(*v).into()), "{v:?}");
                assert_eq!(parse(id(*v)), Some(*v));
            }
        }
        check(&RawColorSpace::ALL, RawColorSpace::id, RawColorSpace::parse);
        check(&RawGamma::ALL, RawGamma::id, RawGamma::parse);
        check(&RawToneMap::ALL, RawToneMap::id, RawToneMap::parse);
        check(&RawRollOff::ALL, RawRollOff::id, RawRollOff::parse);
        check(&RawFlashing::ALL, RawFlashing::id, RawFlashing::parse);
        check(&RawQuality::ALL, RawQuality::id, RawQuality::parse);
        check(&DecodeUsing::ALL, DecodeUsing::id, DecodeUsing::parse);
        let d = RawDevelop {
            color_space: RawColorSpace::RedWideGamutRgb,
            gamma: RawGamma::Log3G10,
            tone_map: RawToneMap::High,
            rolloff: RawRollOff::VerySoft,
            ..Default::default()
        };
        assert_eq!(
            serde_json::to_value(d).unwrap(),
            json!({"decodeUsing": "clip", "colorSpace": "redWideGamutRgb", "gamma": "log3g10", "toneMap": "high", "rolloff": "verySoft", "metadataCurves": true, "creativeLut": false, "cdl": false})
        );
        assert_eq!(serde_json::from_value::<RawDevelop>(serde_json::to_value(d).unwrap()).unwrap(), d);
        assert!(serde_json::from_str::<RawDevelop>(r#"{"gamma": "rec2020"}"#).is_err());
        assert_eq!(d.key(), "redWideGamutRgb/log3g10/high/verySoft");
    }

    /// Projects from before colour space and gamma were separate keep their output: `output`
    /// maps onto them, and a develop without either is scene-linear Rec. 709 as before.
    #[test]
    fn the_first_versions_output_still_reads() {
        for (text, space, gamma) in [
            (r#"{"output": "linearRec709", "toneMap": "clip", "rolloff": "clip"}"#, RawColorSpace::Rec709, RawGamma::Linear),
            (r#"{"output": "rec709", "toneMap": "medium"}"#, RawColorSpace::Rec709, RawGamma::Bt1886),
            (r#"{"output": "log3g10"}"#, RawColorSpace::RedWideGamutRgb, RawGamma::Log3G10),
            (r#"{"iso": 800}"#, RawColorSpace::Rec709, RawGamma::Linear),
        ] {
            let d: RawDevelop = serde_json::from_str(text).unwrap();
            assert_eq!((d.color_space, d.gamma, d.decode_using), (space, gamma, DecodeUsing::Clip), "{text}");
            assert!(d.metadata_curves && !d.creative_lut && !d.cdl, "{text}");
        }
        let explicit: RawDevelop = serde_json::from_str(r#"{"output": "rec709", "gamma": "srgb"}"#).unwrap();
        assert_eq!((explicit.color_space, explicit.gamma), (RawColorSpace::Rec709, RawGamma::Srgb), "the new fields win");
        assert!(serde_json::from_str::<RawDevelop>(r#"{"output": "rec2020"}"#).is_err());
        assert_eq!(serde_json::from_str::<RawDevelop>("{}").unwrap(), RawDevelop::default());
    }

    /// The RAW adjustments read and write as plain numbers (two decimals kept exactly), are left
    /// out when as shot, extend the cache key, and are range-checked; hostile numbers are refused.
    #[test]
    fn raw_adjustments_round_trip_as_numbers_and_are_range_checked() {
        let d = RawDevelop { iso: Some(800), kelvin: Some(5600), tint: Some(Hundredths(-1250)), exposure: Hundredths::from_f64(-0.333), ..Default::default() };
        let v = serde_json::to_value(d).unwrap();
        assert_eq!(v["iso"], json!(800));
        assert_eq!(v["kelvin"], json!(5600));
        assert_eq!(v["tint"], json!(-12.5));
        assert_eq!(v["exposure"], json!(-0.33));
        assert_eq!(serde_json::from_value::<RawDevelop>(v).unwrap(), d);
        assert_eq!(d.key(), "rec709/linear/clip/clip/iso800/k5600/t-1250/e-33");
        assert!(d.adjusts_raw() && !RawDevelop::default().adjusts_raw());
        assert_eq!(d.with_as_shot(), RawDevelop::default());
        assert!(!serde_json::to_string(&RawDevelop::default()).unwrap().contains("iso"), "as shot is written as nothing");
        assert!(d.validate().is_ok() && RawDevelop::default().validate().is_ok() && RawDevelop::STANDARD.validate().is_ok());
        for bad in [
            RawDevelop { iso: Some(1250), ..Default::default() },
            RawDevelop { iso: Some(0), ..Default::default() },
            RawDevelop { kelvin: Some(1699), ..Default::default() },
            RawDevelop { kelvin: Some(100_001), ..Default::default() },
            RawDevelop { tint: Some(Hundredths(10_001)), ..Default::default() },
            RawDevelop { exposure: Some(Hundredths(-801)), ..Default::default() },
            RawDevelop { hdr_peak_nits: Some(99), ..Default::default() },
            RawDevelop { hdr_peak_nits: Some(10_001), ..Default::default() },
            RawDevelop { gamma: RawGamma::Log3G10, ..Default::default() },
            RawDevelop { color_space: RawColorSpace::RedWideGamutRgb, gamma: RawGamma::Bt1886, ..Default::default() },
            RawDevelop { color_space: RawColorSpace::P3D65, gamma: RawGamma::St2084, ..Default::default() },
        ] {
            assert!(bad.validate().is_err(), "{bad:?}");
        }
        assert_eq!(Hundredths::from_f64(12.345), Some(Hundredths(1235)));
        assert_eq!(Hundredths::from_f64(-0.005), Some(Hundredths(-1)));
        for hostile in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, 1e9, -1e300] {
            assert_eq!(Hundredths::from_f64(hostile), None);
        }
        for text in [r#"{"tint": "warm"}"#, r#"{"tint": 1e300}"#, r#"{"exposure": null, "iso": -1}"#, r#"{"kelvin": 5600.5}"#, r#"{"hdrPeakNits": -1}"#] {
            assert!(serde_json::from_str::<RawDevelop>(text).is_err(), "{text}");
        }
        let as_shot: RawDevelop = serde_json::from_str(r#"{"iso": null, "tint": 0}"#).unwrap();
        assert_eq!(as_shot, RawDevelop { tint: Some(Hundredths(0)), ..Default::default() }, "an explicit 0 tint is an override, null is as shot");
        assert!(RAW_ISO_VALUES.windows(2).all(|w| w[0] < w[1]));
    }

    /// Decode Using picks the clip's settings or the project's; the clip's decode quality applies
    /// either way, else the project's.
    #[test]
    fn decode_using_picks_the_clip_or_the_project() {
        let project = RawDevelop { quality: Some(RawQuality::HalfGood), ..RawDevelop::STANDARD };
        let own = RawDevelop { gamma: RawGamma::Srgb, iso: Some(400), ..Default::default() };
        let e = RawDevelop::effective(&project, Some(&own));
        assert_eq!((e.gamma, e.iso, e.quality(), e.decode_using), (RawGamma::Srgb, Some(400), RawQuality::HalfGood, DecodeUsing::Clip));
        let follows = RawDevelop { decode_using: DecodeUsing::Project, quality: Some(RawQuality::QuarterGood), ..own };
        let e = RawDevelop::effective(&project, Some(&follows));
        assert_eq!((e.gamma, e.iso, e.quality(), e.decode_using), (RawGamma::Bt1886, None, RawQuality::QuarterGood, DecodeUsing::Project));
        let e = RawDevelop::effective(&project, None);
        assert_eq!(e, RawDevelop { decode_using: DecodeUsing::Project, ..project });
        assert_eq!(RawDevelop::effective(&RawDevelop::default(), None).gamma, RawGamma::Linear, "an old project keeps linear light");
        assert_eq!(RawQuality::HalfGood.divisor(), 2);
        assert!(RawQuality::HalfGood.half_good() && !RawQuality::HalfPremium.half_good());
    }

    /// Each supported combination has an interpretation the compositor knows, and every one of
    /// those is listed in INTERPRETATIONS; Rec. 709 needs none.
    #[test]
    fn every_supported_output_has_an_interpretation() {
        for space in RawColorSpace::ALL {
            for gamma in RawGamma::ALL {
                let d = RawDevelop { color_space: space, gamma, ..Default::default() };
                assert_eq!(d.validate().is_ok(), RawDevelop::supports(space, gamma));
                if !RawDevelop::supports(space, gamma) {
                    continue;
                }
                match d.interpretation() {
                    None => assert_eq!(space, RawColorSpace::Rec709),
                    Some(cs) => assert!(RawDevelop::INTERPRETATIONS.contains(&cs), "{space:?} {gamma:?} {cs:?}"),
                }
            }
        }
        assert_eq!(
            RawDevelop { color_space: RawColorSpace::Rec2020, gamma: RawGamma::St2084, ..Default::default() }.interpretation(),
            Some(ColorSpace::Rec2100Pq)
        );
    }
}

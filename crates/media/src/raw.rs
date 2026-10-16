//! Camera raw source settings: how DNG footage (stills, CinemaDNG folders and CinemaDNG in MXF) is
//! developed. The parity reference is Premiere's Source Settings for camera raw formats (white
//! balance, exposure, highlights, decode quality, output colour space), taken from Adobe's public
//! help only. A project stores them per media item (`Interpretation::raw`); the media pool opens
//! the item's source and asks it for a copy developed with them
//! ([`crate::MediaSource::with_raw_settings`]).
//!
//! Reduced resolution: a [`crate::FrameRequest`] scale below 1 asks the DNG decoder for a smaller
//! picture ([`scale_level`]: the largest power-of-two fraction of the full size that still covers
//! the request), so playback at ½ or ¼ resolution develops less.

use serde::{Deserialize, Serialize};

pub use filmcraft_dng::{DemosaicQuality, Highlights, OutputSpace, WhiteBalance};

/// Exposure change limit in stops (either way).
pub const MAX_EXPOSURE: f64 = 8.0;
/// White balance temperature range in kelvin.
pub const TEMPERATURE_RANGE: (f64, f64) = (2000.0, 15_000.0);
/// White balance tint range (positive towards magenta).
pub const TINT_RANGE: (f64, f64) = (-150.0, 150.0);

/// Per-item camera raw settings. The default develops exactly as the file says.
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RawSettings {
    pub white_balance: WhiteBalance,
    /// Exposure change in stops, added to the file's BaselineExposure.
    pub exposure: f64,
    pub highlights: Highlights,
    pub demosaic: DemosaicQuality,
    pub output: OutputSpace,
}

impl RawSettings {
    /// The decoder options for these settings, developing at reduced-resolution `level`
    /// ([`scale_level`]) of a `width` × `height` picture.
    pub fn options(&self, width: u32, height: u32, level: u8) -> filmcraft_dng::ProcessOptions {
        filmcraft_dng::ProcessOptions {
            white_balance: self.white_balance,
            exposure: if self.exposure.is_finite() { self.exposure.clamp(-MAX_EXPOSURE, MAX_EXPOSURE) } else { 0.0 },
            highlights: self.highlights,
            demosaic: self.demosaic,
            output: self.output,
            target_size: target_size(width, height, level),
        }
    }

    /// Whether every value is in range (finite, exposure within ±[`MAX_EXPOSURE`], white balance
    /// within [`TEMPERATURE_RANGE`] and [`TINT_RANGE`]).
    pub fn validate(&self) -> Result<(), String> {
        if !self.exposure.is_finite() || self.exposure.abs() > MAX_EXPOSURE {
            return Err(format!("exposure must be between -{MAX_EXPOSURE} and {MAX_EXPOSURE} stops"));
        }
        if let WhiteBalance::Custom { temperature, tint } = self.white_balance {
            if !temperature.is_finite() || temperature < TEMPERATURE_RANGE.0 || temperature > TEMPERATURE_RANGE.1 {
                return Err(format!("temperature must be between {} and {} K", TEMPERATURE_RANGE.0, TEMPERATURE_RANGE.1));
            }
            if !tint.is_finite() || tint < TINT_RANGE.0 || tint > TINT_RANGE.1 {
                return Err(format!("tint must be between {} and {}", TINT_RANGE.0, TINT_RANGE.1));
            }
        }
        Ok(())
    }
}

/// What a camera raw source reports for its Source Settings.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct RawInfo {
    /// The as-shot white balance as (temperature in kelvin, tint), where the file has the colour
    /// calibration to say (the starting point of a custom white balance).
    pub as_shot: Option<(f64, f64)>,
    /// The settings the source develops with.
    pub settings: RawSettings,
}

/// The reduced-resolution level for a request at `scale` of full size: 0 = full resolution, `k` =
/// 1 / 2^k (at most 1/16), the smallest power-of-two fraction that still covers `scale`.
pub fn scale_level(scale: f32) -> u8 {
    if !scale.is_finite() || scale >= 1.0 {
        return 0;
    }
    let mut k = 0u8;
    while k < 4 && scale <= 1.0 / f32::from(1u8 << (k + 1)) {
        k += 1;
    }
    k
}

/// The smallest picture the decoder must make at `level` of a `width` × `height` picture (None =
/// full resolution).
pub fn target_size(width: u32, height: u32, level: u8) -> Option<(usize, usize)> {
    if level == 0 {
        return None;
    }
    let d = 1u32 << level.min(4);
    Some((width.div_ceil(d).max(1) as usize, height.div_ceil(d).max(1) as usize))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn levels_cover_the_request() {
        assert_eq!(scale_level(1.0), 0);
        assert_eq!(scale_level(2.0), 0);
        assert_eq!(scale_level(f32::NAN), 0);
        assert_eq!(scale_level(0.75), 0);
        assert_eq!(scale_level(0.5), 1);
        assert_eq!(scale_level(0.3), 1);
        assert_eq!(scale_level(0.25), 2);
        assert_eq!(scale_level(0.01), 4);
        assert_eq!(scale_level(-1.0), 4);
        assert_eq!(target_size(4096, 2160, 0), None);
        assert_eq!(target_size(4096, 2160, 1), Some((2048, 1080)));
        assert_eq!(target_size(15, 9, 2), Some((4, 3)));
        assert_eq!(target_size(1, 1, 9), Some((1, 1)));
    }

    #[test]
    fn settings_map_to_decoder_options() {
        let s = RawSettings { exposure: 1.5, demosaic: DemosaicQuality::Fast, ..Default::default() };
        let o = s.options(100, 50, 1);
        assert_eq!((o.exposure, o.demosaic, o.target_size), (1.5, DemosaicQuality::Fast, Some((50, 25))));
        assert_eq!(RawSettings::default().options(8, 8, 0), filmcraft_dng::ProcessOptions::default());
        // hostile values never reach the decoder
        assert_eq!(RawSettings { exposure: f64::INFINITY, ..Default::default() }.options(8, 8, 0).exposure, 0.0);
        assert_eq!(RawSettings { exposure: 99.0, ..Default::default() }.options(8, 8, 0).exposure, MAX_EXPOSURE);
    }

    #[test]
    fn validation() {
        assert!(RawSettings::default().validate().is_ok());
        assert!(RawSettings { exposure: f64::NAN, ..Default::default() }.validate().is_err());
        assert!(RawSettings { exposure: -9.0, ..Default::default() }.validate().is_err());
        let wb = |temperature, tint| RawSettings { white_balance: WhiteBalance::Custom { temperature, tint }, ..Default::default() };
        assert!(wb(5600.0, 10.0).validate().is_ok());
        assert!(wb(500.0, 0.0).validate().is_err());
        assert!(wb(5600.0, 151.0).validate().is_err());
        assert!(wb(f64::NAN, 0.0).validate().is_err());
    }

    #[test]
    fn serde_defaults_and_shape() {
        let s: RawSettings = serde_json::from_str("{}").unwrap();
        assert_eq!(s, RawSettings::default());
        let s = RawSettings { white_balance: WhiteBalance::Custom { temperature: 3200.0, tint: -5.0 }, exposure: 0.5, ..Default::default() };
        let j = serde_json::to_string(&s).unwrap();
        assert_eq!(serde_json::from_str::<RawSettings>(&j).unwrap(), s);
    }
}

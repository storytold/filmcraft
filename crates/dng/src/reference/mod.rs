//! The decoder and pipeline of PR #1068, kept unchanged (feature `writer`, tests and benchmarks
//! only) so the faster implementation can be checked for bit-identical output and benchmarked
//! against it in the same run.

#![allow(clippy::too_many_arguments)]

pub mod demosaic;
pub mod ljpeg;
pub mod process;
pub mod raw;

use crate::options::{ProcessOptions, WhiteBalance};
use crate::{Dng, Error, Image, Result, color};

impl Dng<'_> {
    /// The raw samples as PR #1068 read them.
    pub fn reference_raw(&self) -> Result<crate::raw::RawImage> {
        raw::read(&self.tiff, self.raw_ifd())
    }

    /// [`Dng::develop_with`] as PR #1068 (plus the `ProcessOptions` commit) developed.
    pub fn reference_develop(&self, o: &ProcessOptions) -> Result<Image> {
        let info = self.info()?;
        let p = self.params()?;
        let raw = self.reference_raw()?;
        let mut calibration = self.calibration()?;
        calibration.baseline_exposure += o.exposure_stops();
        if let WhiteBalance::Custom { temperature, tint } = o.white_balance
            && calibration.color_matrix.iter().any(Option::is_some)
        {
            let xy = color::temperature_tint_to_xy(temperature, tint).ok_or(Error::Unsupported("a non-finite white balance".into()))?;
            calibration.as_shot_neutral = None;
            calibration.as_shot_white_xy = Some(xy);
        }
        let solved = if p.cfa.is_none() && raw.samples_per_pixel == 1 {
            let e = 2f64.powf(calibration.baseline_exposure.clamp(-16.0, 16.0));
            color::Solved { neutral: [1.0; 3], matrix: color::scale(&color::IDENTITY, e), white_xy: color::D65 }
        } else {
            calibration.solve()?
        };
        let d = process::develop_with(&raw, &p, &solved.neutral, &solved.matrix, o)?;
        Ok(Image { info, width: d.width, height: d.height, rgba: d.rgba })
    }
}

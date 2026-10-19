//! Cached 16-bit alpha in source-image space. Run-length encoding keeps flat background and
//! foreground cheap to persist; Arc sharing keeps undo snapshots cheap. No file or model runtime
//! is needed to display a completed mask. Times are exact source-media ticks, never timeline time.

use std::sync::Arc;

use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};

pub const MAX_FRAMES: usize = 120;
pub const MAX_PIXELS: usize = 4_194_304;
pub const MAX_RUNS: usize = 1_000_000;

/// (pixel count, alpha / 65535), in row-major order.
pub type AlphaRun = (u32, u16);

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct RasterFrame {
    pub time: Tick,
    #[serde(deserialize_with = "read_runs")]
    pub runs: Arc<Vec<AlphaRun>>,
}

/// Deserialize alpha runs with a hard count limit before allocating the complete vector.
fn read_runs<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Arc<Vec<AlphaRun>>, D::Error> {
    struct Runs;
    impl<'de> serde::de::Visitor<'de> for Runs {
        type Value = Arc<Vec<AlphaRun>>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bounded alpha runs")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut runs = Vec::new();
            while let Some(run) = seq.next_element()? {
                if runs.len() >= MAX_RUNS {
                    return Err(serde::de::Error::custom("too many alpha runs"));
                }
                runs.push(run);
            }
            Ok(Arc::new(runs))
        }
    }
    d.deserialize_seq(Runs)
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct RasterMask {
    pub width: u32,
    pub height: u32,
    pub end: Tick,
    pub quality: bool,
    pub frames: Vec<RasterFrame>,
}

impl<'de> Deserialize<'de> for RasterMask {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Wire {
            width: u32,
            height: u32,
            end: Tick,
            quality: bool,
            #[serde(deserialize_with = "read_frames")]
            frames: Vec<RasterFrame>,
        }
        let w = Wire::deserialize(d)?;
        let mask = Self { width: w.width, height: w.height, end: w.end, quality: w.quality, frames: w.frames };
        mask.validate().map_err(serde::de::Error::custom)?;
        Ok(mask)
    }
}

/// Deserialize source frames while bounding frame count and aggregate encoded runs.
fn read_frames<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Vec<RasterFrame>, D::Error> {
    struct Frames;
    impl<'de> serde::de::Visitor<'de> for Frames {
        type Value = Vec<RasterFrame>;
        fn expecting(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
            f.write_str("bounded raster frames")
        }
        fn visit_seq<A: serde::de::SeqAccess<'de>>(self, mut seq: A) -> Result<Self::Value, A::Error> {
            let mut frames: Vec<RasterFrame> = Vec::new();
            let mut runs = 0usize;
            while let Some(frame) = seq.next_element::<RasterFrame>()? {
                runs = runs.saturating_add(frame.runs.len());
                if frames.len() >= MAX_FRAMES || runs > 4_000_000 {
                    return Err(serde::de::Error::custom("raster mask exceeds the frame/run budget"));
                }
                frames.push(frame);
            }
            Ok(frames)
        }
    }
    d.deserialize_seq(Frames)
}

impl RasterMask {
    /// Check ordered source times, dimensions, exact run coverage and aggregate storage budgets.
    pub fn validate(&self) -> Result<(), String> {
        let n = (self.width as usize).checked_mul(self.height as usize).ok_or("invalid alpha dimensions")?;
        if self.width == 0 || self.height == 0 || n > MAX_PIXELS || self.frames.is_empty() || self.frames.len() > MAX_FRAMES {
            return Err("raster mask has invalid dimensions or frame count".into());
        }
        if n.checked_mul(self.frames.len()).is_none_or(|n| n > 40_000_000) {
            return Err("raster mask exceeds the pixel budget".into());
        }
        let mut previous = None;
        let mut total_runs = 0usize;
        for frame in &self.frames {
            if previous.is_some_and(|t| t >= frame.time) || frame.time >= self.end {
                return Err("alpha frame times must be increasing and before the range end".into());
            }
            previous = Some(frame.time);
            total_runs = total_runs.saturating_add(frame.runs.len());
            let mut pixels = 0usize;
            for &(count, _) in frame.runs.iter() {
                if count == 0 {
                    return Err("alpha runs must have positive length".into());
                }
                pixels = pixels.checked_add(count as usize).ok_or("alpha run overflow")?;
                if pixels > n {
                    return Err("alpha runs exceed the image dimensions".into());
                }
            }
            if pixels != n || frame.runs.len() > MAX_RUNS || total_runs > 4_000_000 {
                return Err("alpha runs do not match the image/run budget".into());
            }
        }
        Ok(())
    }

    /// Outside the processed interval, return None so the original picture remains visible.
    pub fn alpha_at(&self, time: Tick) -> Option<Vec<f32>> {
        let first = self.frames.first()?;
        if time < first.time || time >= self.end {
            return None;
        }
        let i = self.frames.partition_point(|f| f.time <= time).saturating_sub(1);
        let frame = self.frames.get(i)?;
        let n = (self.width as usize).checked_mul(self.height as usize)?;
        if n == 0 || n > MAX_PIXELS {
            return None;
        }
        let mut alpha = Vec::with_capacity(n);
        for &(count, value) in frame.runs.iter() {
            let end = alpha.len().checked_add(count as usize)?;
            if count == 0 || end > n {
                return None;
            }
            alpha.resize(end, value as f32 / 65535.0);
        }
        (alpha.len() == n).then_some(alpha)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn validates_runs_and_source_time_boundaries() {
        let mut mask = RasterMask {
            width: 2,
            height: 1,
            end: Tick(20),
            quality: true,
            frames: vec![RasterFrame { time: Tick(10), runs: Arc::new(vec![(1, 0), (1, 32768)]) }],
        };
        mask.validate().unwrap();
        assert!(mask.alpha_at(Tick(9)).is_none());
        assert!(mask.alpha_at(Tick(20)).is_none());
        assert!((mask.alpha_at(Tick(15)).unwrap()[1] - 0.5).abs() < 0.00002);
        mask.frames[0].runs = Arc::new(vec![(3, 0)]);
        assert!(mask.validate().is_err());
        assert!(serde_json::from_str::<RasterMask>(&serde_json::to_string(&mask).unwrap()).is_err());
    }
}

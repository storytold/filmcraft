//! Native model inference. Frame decoding, editor commands and rendering live above this crate.
#![deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable)]

use filmcraft_time::Tick;

#[cfg(all(feature = "inference", not(target_arch = "wasm32")))]
pub mod models;
pub mod pixels;
#[cfg(all(feature = "inference", not(target_arch = "wasm32")))]
pub mod runtime;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Include,
    Exclude,
    Refine,
}

#[derive(Clone, Debug)]
pub struct Point {
    pub frame: usize,
    pub x: f32,
    pub y: f32,
    pub kind: Kind,
}

pub struct Frame<'a> {
    pub time: Tick,
    pub rgba: &'a [u8],
}

pub struct Input<'a> {
    pub width: usize,
    pub height: usize,
    pub end: Tick,
    pub frames: Vec<Frame<'a>>,
    pub points: Vec<Point>,
    pub quality: bool,
    pub band: usize,
    pub base_alpha: Option<&'a [u16]>,
}

impl Input<'_> {
    pub fn validate(&self) -> Result<usize, String> {
        let pixels = self.width.checked_mul(self.height).ok_or("frame dimensions overflow")?;
        if self.width == 0
            || self.height == 0
            || self.width.max(self.height) > 720
            || pixels > 4_194_304
            || self.frames.is_empty()
            || self.frames.len() > 120
            || pixels.checked_mul(self.frames.len()).is_none_or(|n| n > 40_000_000)
            || !(1..=32).contains(&self.band)
            || self.points.len() > 512
        {
            return Err("Native rotoscope input exceeds its dimensions/frame/pixel/point budget".into());
        }
        let bytes = pixels.checked_mul(4).ok_or("RGBA size overflow")?;
        let mut last = None;
        for frame in &self.frames {
            if frame.rgba.len() != bytes || frame.time.0 < 0 || frame.time >= self.end || last.is_some_and(|t| t >= frame.time) {
                return Err("Invalid source frames or times".into());
            }
            last = Some(frame.time);
        }
        let mut prompted = std::collections::BTreeSet::new();
        for point in &self.points {
            if point.frame >= self.frames.len()
                || !point.x.is_finite()
                || !point.y.is_finite()
                || point.x < 0.0
                || point.y < 0.0
                || point.x >= self.width as f32
                || point.y >= self.height as f32
            {
                return Err("Object point lies outside the processed frames".into());
            }
            if point.kind != Kind::Refine {
                prompted.insert(point.frame);
            }
        }
        if prompted.len() > 16 {
            return Err("Native tracking currently supports points on at most 16 frames; remove some prompted frames".into());
        }
        if let Some(alpha) = self.base_alpha
            && (self.frames.len() != 1 || alpha.len() != pixels || !self.quality)
        {
            return Err("Existing alpha must match one Quality frame".into());
        }
        if self.base_alpha.is_none() && !self.points.iter().any(|p| p.kind == Kind::Include) {
            return Err("Add an Include point before inference".into());
        }
        Ok(pixels)
    }
}

#[cfg(test)]
mod tests;

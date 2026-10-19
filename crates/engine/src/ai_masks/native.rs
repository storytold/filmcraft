//! In-process Rust inference. No HTTP, frame files, Python or foreign ML runtime.
use std::path::PathBuf;
use std::sync::{Arc, Mutex, atomic::Ordering};

use filmcraft_project::raster_mask::RasterMask;
use filmcraft_rotoscope::{Frame, Kind, Point, runtime::Runtime};

use super::{Backend, Input, Pick};

struct Native {
    directory: PathBuf,
    runtime: Mutex<Option<Runtime>>,
}

/// Create a lazily loaded native backend using the configured local model directory.
pub(super) fn from_env() -> Arc<dyn Backend> {
    let directory = std::env::var_os("FILMCRAFT_AI_ROTO_MODELS").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("target/rotoscope/models/native"));
    Arc::new(Native { directory, runtime: Mutex::new(None) })
}

impl Backend for Native {
    /// Translate engine prompts to bounded native inputs and reuse the locked inference runtime.
    fn infer(&self, input: &Input, progress: &filmcraft_export::Progress) -> Result<RasterMask, String> {
        if input.frames.is_empty() || input.frames.len() > 120 {
            return Err("AI inference requires 1..120 source frames".into());
        }
        let mut points = Vec::with_capacity(input.points.len().min(512));
        if input.points.len() > 512 {
            return Err("AI selection exceeds the 512-point budget".into());
        }
        for point in &input.points {
            // Only prompts on processed source frames participate. Source ticks, including
            // variable-rate decoding, come directly from the engine's prepared frame list.
            let frame = input.frames.partition_point(|frame| frame.time <= point.time).saturating_sub(1);
            if input.frames.first().is_none_or(|frame| point.time < frame.time) || point.time >= input.end {
                continue;
            }
            let kind = match point.kind {
                Pick::Foreground => Kind::Include,
                Pick::Background => Kind::Exclude,
                Pick::Refine => Kind::Refine,
                Pick::Off => return Err("Invalid AI point kind".into()),
            };
            points.push(Point { frame, x: point.x as f32, y: point.y as f32, kind });
        }
        let native = filmcraft_rotoscope::Input {
            width: input.width as usize,
            height: input.height as usize,
            end: input.end,
            frames: input.frames.iter().map(|frame| Frame { time: frame.time, rgba: &frame.rgba }).collect(),
            points,
            quality: input.quality,
            band: input.band as usize,
            base_alpha: input.base_alpha.as_deref(),
        };
        native.validate()?;
        let mut runtime = self.runtime.lock().unwrap_or_else(|error| error.into_inner());
        if progress.cancel.load(Ordering::Relaxed) {
            return Err("Cancelled: nothing was changed".into());
        }
        if runtime.is_none() {
            *progress.status.lock().unwrap_or_else(|error| error.into_inner()) = "Loading native Rust models…".into();
            *runtime = Some(Runtime::load(&self.directory)?);
        }
        let runtime = runtime.as_mut().ok_or("Native model runtime unavailable")?;
        runtime.infer(&native, &progress.cancel, |done, status| {
            progress.done.store((input.frames.len() + done).min(input.frames.len() * 3) as u64, Ordering::Relaxed);
            *progress.status.lock().unwrap_or_else(|error| error.into_inner()) = status.to_string();
        })
    }
}

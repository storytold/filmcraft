//! Bounded shared observations for subspace analysis and spatial mesh planning.
//! Coordinates are in analysis pixels, with pixel centres at integer coordinates.

use std::sync::Arc;

use super::trajectory_index::TrajectoryIndex;

pub(super) const MAX_FRAMES: usize = 20_000;
pub(super) const GRID_COLS: usize = 16;
pub(super) const GRID_ROWS: usize = 9;
pub(super) const GRID_CELLS: usize = GRID_COLS * GRID_ROWS;

// One contract shared by the collector, index and factoriser: the collector produces under
// these bounds and the other modules validate against them.
pub(super) const MAX_ACTIVE_TRACKS: usize = 160;
pub(super) const MAX_COORDINATE: f32 = 1.0e9;
pub(super) const MIN_TRACK_LENGTH: usize = 20;
const MAX_DIMENSION: usize = 100_000_000;

#[derive(Clone, Debug)]
pub(super) struct FeatureTrajectory {
    pub start: usize,
    pub points: Vec<[f32; 2]>,
    /// Dynamic-scene outliers must not constrain the subspace or mesh.
    pub rejected: bool,
}

#[derive(Clone, Debug)]
pub(super) struct TrajectorySet {
    pub width: usize,
    pub height: usize,
    pub frames: usize,
    pub tracks: Vec<FeatureTrajectory>,
    /// Includes zero; no trajectory or temporal filtering crosses a discontinuity.
    pub segment_starts: Vec<usize>,
    /// Per-cell display-luma standard deviation plus the paper's 0.5 floor.
    pub saliency: Vec<[f32; GRID_CELLS]>,
    /// Frame-local active trajectory IDs in CSR form.
    pub(super) index: Arc<TrajectoryIndex>,
}

impl TrajectorySet {
    /// Construct a bounded, indexed observation set.
    ///
    /// `saliency` may be empty for callers that do not use mesh planning; when supplied it must
    /// have exactly one finite row per source frame. The constructor deliberately returns `None`
    /// rather than exposing a partially validated set to asynchronous analysis.
    pub(super) fn new(
        width: usize,
        height: usize,
        frames: usize,
        tracks: Vec<FeatureTrajectory>,
        segment_starts: Vec<usize>,
        saliency: Vec<[f32; GRID_CELLS]>,
    ) -> Option<Self> {
        if width == 0 || height == 0 || width > MAX_DIMENSION || height > MAX_DIMENSION || frames > MAX_FRAMES {
            return None;
        }
        if !saliency.is_empty() && saliency.len() != frames {
            return None;
        }
        if saliency.iter().flatten().any(|value| !value.is_finite()) {
            return None;
        }
        if segment_starts.len() > frames.saturating_add(1) {
            return None;
        }
        if frames == 0 {
            if !segment_starts.is_empty() && (segment_starts.len() != 1 || segment_starts.first().copied() != Some(0)) {
                return None;
            }
        } else {
            if segment_starts.first().copied().is_some_and(|start| start != 0) {
                return None;
            }
            for pair in segment_starts.windows(2) {
                if pair[0] >= pair[1] || pair[1] > frames {
                    return None;
                }
            }
            if segment_starts.iter().any(|start| *start > frames) {
                return None;
            }
        }

        let max_observations = frames.saturating_mul(MAX_ACTIVE_TRACKS);
        let mut observations = 0usize;
        if tracks.len() > max_observations {
            return None;
        }
        for track in &tracks {
            if track.start > frames {
                return None;
            }
            let end = track.start.checked_add(track.points.len())?;
            if end > frames {
                return None;
            }
            if track.points.iter().flatten().any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE) {
                return None;
            }
            observations = observations.checked_add(track.points.len())?;
            if observations > max_observations {
                return None;
            }
        }

        let index = Arc::new(TrajectoryIndex::new(&tracks, frames)?);
        Some(Self { width, height, frames, tracks, segment_starts, saliency, index })
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) struct PointConstraint {
    pub source: [f64; 2],
    pub target: [f64; 2],
    /// Feature lifetime fade-in/out from Liu et al. 2009, section 4.1.4.
    pub weight: f64,
}

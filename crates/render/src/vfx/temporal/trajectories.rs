//! Persistent point trajectories shared by the global and subspace stabilizers.
//!
//! Detection is supplied by the sequential analysis loop.  This collector only
//! associates those corners with already-active tracks, runs the pyramidal
//! forward/backward LK pass once, and keeps the resulting observations for all
//! later fits.

use filmcraft_project::TrackMethod;

use crate::track::{MAX_FEATURES, Prepared, Step, fit_tracked_points, track_points};

use super::epipolar::reject_dynamic_tracks;
use super::trajectory_data::{FeatureTrajectory, GRID_CELLS, GRID_COLS, GRID_ROWS, MAX_FRAMES, MIN_TRACK_LENGTH, TrajectorySet};

const MAX_ACTIVE: usize = MAX_FEATURES;
const CORNER_SPACING: f32 = 5.0;

struct ActiveTrack {
    id: usize,
    trajectory: FeatureTrajectory,
    last: [f32; 2],
}

/// Bounded sequential trajectory state.
///
/// The collector owns no decoded image data.  Prepared pyramids are borrowed
/// for each call and discarded by the caller after the analysis batch.
pub(super) struct TrajectoryCollector {
    width: usize,
    height: usize,
    frames: usize,
    next_frame: usize,
    next_id: usize,
    observations: usize,
    observation_limit: usize,
    active: Vec<ActiveTrack>,
    finished: Vec<(usize, FeatureTrajectory)>,
    segment_starts: Vec<usize>,
    saliency: Vec<[f32; GRID_CELLS]>,
}

impl TrajectoryCollector {
    pub(super) fn new(width: usize, height: usize, frames: usize) -> Option<Self> {
        if width == 0 || height == 0 || frames == 0 || frames > MAX_FRAMES || width > f32::MAX as usize || height > f32::MAX as usize {
            return None;
        }
        let observation_limit = frames.checked_mul(MAX_ACTIVE)?;
        Some(Self {
            width,
            height,
            frames,
            next_frame: 0,
            next_id: 0,
            observations: 0,
            observation_limit,
            active: Vec::with_capacity(MAX_ACTIVE),
            finished: Vec::new(),
            segment_starts: vec![0],
            saliency: vec![[0.5; GRID_CELLS]; frames],
        })
    }

    /// Consume one sequential analysis frame.
    ///
    /// `corners` are corners detected in `current`.  On frame zero they seed
    /// the initial identities; on subsequent frames they are used only to
    /// replenish slots left by ended tracks.  No new corner is ever matched
    /// retroactively to an old trajectory.
    pub(super) fn step(
        &mut self,
        frame: usize,
        previous: Option<&Prepared>,
        current: Option<&Prepared>,
        corners: &[[f32; 2]],
        method: TrackMethod,
    ) -> Option<Step> {
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        if frame >= self.frames || frame < self.next_frame {
            return None;
        }
        let sequential = frame == self.next_frame;
        if !sequential {
            self.end_active();
            self.mark_segment(frame);
            self.next_frame = frame;
        }
        let current_dims = current.and_then(Prepared::dimensions);
        let current_valid = current_dims == Some((self.width, self.height));
        if current_valid && let Some(current) = current {
            self.collect_saliency(frame, current);
        }
        if filmcraft_media::cancel::cancelled() {
            return None;
        }

        // A missing frame or a malformed/dimension-changing image cuts every
        // identity.  If the current image is usable, corners start a fresh
        // segment at this exact frame; otherwise the next usable frame does.
        if frame == 0 || !sequential || !current_valid || previous.is_none() || current.is_none() {
            self.end_active();
            if current_valid {
                self.mark_segment(frame);
                self.seed_corners(frame, corners);
            } else if frame > 0 {
                self.mark_segment(frame.saturating_add(1).min(self.frames.saturating_sub(1)));
            }
            self.next_frame = frame.saturating_add(1);
            return None;
        }
        let previous = previous?;
        let current = current?;
        if previous.dimensions() != Some((self.width, self.height)) {
            self.end_active();
            self.mark_segment(frame);
            self.seed_corners(frame, corners);
            self.next_frame = frame.saturating_add(1);
            return None;
        }

        let source_points: Vec<[f32; 2]> = self.active.iter().map(|track| track.last).collect();
        let tracked = track_points(previous, current, &source_points);
        let fit = fit_tracked_points(&source_points, &tracked, method);
        let old_count = self.active.len();
        let active = std::mem::take(&mut self.active);
        let mut next_active = Vec::with_capacity(MAX_ACTIVE);
        for (index, mut track) in active.into_iter().enumerate() {
            let Some(point) = tracked.get(index).and_then(|point| *point) else {
                self.finish_track(track);
                continue;
            };
            if !valid_point(point, self.width, self.height) || self.observations >= self.observation_limit {
                self.finish_track(track);
                continue;
            }
            track.last = point;
            track.trajectory.points.push(point);
            self.observations = self.observations.saturating_add(1);
            next_active.push(track);
        }
        self.active = next_active;
        if old_count > 0 && self.active.is_empty() {
            self.mark_segment(frame);
        }
        self.replenish(frame, corners);
        self.next_frame = frame.saturating_add(1);
        fit
    }

    pub(super) fn finish(mut self) -> Option<TrajectorySet> {
        self.end_active();
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        self.finished.sort_by_key(|(id, _)| *id);
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        let mut tracks = Vec::with_capacity(self.finished.len());
        for (_, track) in self.finished {
            if filmcraft_media::cancel::cancelled() {
                return None;
            }
            tracks.push(track);
        }
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        let mut data = TrajectorySet::new(self.width, self.height, self.frames, tracks, self.segment_starts, self.saliency)?;
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        if !reject_dynamic_tracks(&mut data.tracks, data.width, data.height, data.frames, data.index.as_ref()) {
            return None;
        }
        Some(data)
    }

    fn collect_saliency(&mut self, frame: usize, prepared: &Prepared) {
        let Some(slot) = self.saliency.get_mut(frame) else { return };
        let gray = prepared.base();
        if gray.px.len() != self.width.saturating_mul(self.height) {
            return;
        }
        let mut sum = [0.0f64; GRID_CELLS];
        let mut sum_sq = [0.0f64; GRID_CELLS];
        let mut count = [0u32; GRID_CELLS];
        for y in 0..self.height {
            if filmcraft_media::cancel::cancelled() {
                return;
            }
            let row = y.saturating_mul(self.width);
            let gy = y.saturating_mul(GRID_ROWS) / self.height;
            for x in 0..self.width {
                if filmcraft_media::cancel::cancelled() {
                    return;
                }
                let value = gray.px[row + x];
                if !value.is_finite() {
                    continue;
                }
                let gx = x.saturating_mul(GRID_COLS) / self.width;
                let cell = gy.min(GRID_ROWS - 1) * GRID_COLS + gx.min(GRID_COLS - 1);
                sum[cell] += value as f64;
                sum_sq[cell] += (value as f64) * (value as f64);
                count[cell] = count[cell].saturating_add(1);
            }
        }
        for cell in 0..GRID_CELLS {
            if filmcraft_media::cancel::cancelled() {
                return;
            }
            let n = count[cell] as f64;
            let variance = if n > 0.0 { (sum_sq[cell] / n - (sum[cell] / n).powi(2)).max(0.0) } else { 0.0 };
            slot[cell] = variance.sqrt() as f32 + 0.5;
        }
    }

    fn seed_corners(&mut self, frame: usize, corners: &[[f32; 2]]) {
        self.replenish(frame, corners);
    }

    fn replenish(&mut self, frame: usize, corners: &[[f32; 2]]) {
        if self.active.len() >= MAX_ACTIVE {
            return;
        }
        let spacing_sq = CORNER_SPACING * CORNER_SPACING;
        for &corner in corners {
            if self.active.len() >= MAX_ACTIVE || self.observations >= self.observation_limit {
                break;
            }
            if !valid_point(corner, self.width, self.height) {
                continue;
            }
            let separated = self.active.iter().all(|track| {
                let dx = track.last[0] - corner[0];
                let dy = track.last[1] - corner[1];
                dx * dx + dy * dy >= spacing_sq
            });
            if !separated {
                continue;
            }
            let id = self.next_id;
            self.next_id = self.next_id.saturating_add(1);
            let trajectory = FeatureTrajectory { start: frame, points: vec![corner], rejected: false };
            self.observations = self.observations.saturating_add(1);
            self.active.push(ActiveTrack { id, trajectory, last: corner });
        }
    }

    fn finish_track(&mut self, track: ActiveTrack) {
        if track.trajectory.points.len() >= MIN_TRACK_LENGTH {
            self.finished.push((track.id, track.trajectory));
        } else {
            self.observations = self.observations.saturating_sub(track.trajectory.points.len());
        }
    }

    fn end_active(&mut self) {
        let active = std::mem::take(&mut self.active);
        for track in active {
            self.finish_track(track);
        }
    }

    fn mark_segment(&mut self, frame: usize) {
        if frame >= self.frames {
            return;
        }
        if self.segment_starts.last().copied() != Some(frame) {
            self.segment_starts.push(frame);
        }
    }
}

fn valid_point(point: [f32; 2], width: usize, height: usize) -> bool {
    point[0].is_finite() && point[1].is_finite() && point[0] >= 0.0 && point[1] >= 0.0 && point[0] < width as f32 && point[1] < height as f32
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::track::Gray;

    #[test]
    fn missing_frames_split_segments_and_do_not_stitch_entries() {
        let mut collector = TrajectoryCollector::new(24, 24, 25).expect("valid bounded collector");
        let image = Prepared::new(Gray::new(24, 24, vec![0.0; 24 * 24]));
        collector.step(0, None, Some(&image), &[[2.0, 2.0]], TrackMethod::Position);
        collector.step(1, None, None, &[], TrackMethod::Position);
        collector.step(2, None, Some(&image), &[[4.0, 4.0]], TrackMethod::Position);
        let set = collector.finish().expect("completed collection");
        assert!(set.tracks.is_empty());
        assert_eq!(set.segment_starts, vec![0, 2]);
    }

    #[test]
    fn malformed_collector_inputs_are_bounded() {
        assert!(TrajectoryCollector::new(0, 24, 25).is_none());
        assert!(TrajectoryCollector::new(24, 24, MAX_FRAMES + 1).is_none());
        let mut collector = TrajectoryCollector::new(24, 24, 2).expect("valid bounded collector");
        let malformed = Prepared::new(Gray { w: 24, h: 24, px: Vec::new() });
        assert!(collector.step(0, None, Some(&malformed), &[[f32::NAN, -1.0]], TrackMethod::Position).is_none());
        assert!(collector.finish().expect("completed malformed collection").tracks.is_empty());
    }
}

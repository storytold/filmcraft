//! Bounded CSR index of trajectory observations by source frame.
//!
//! The tracker retains at most 160 simultaneous trajectories. Keeping the frame-to-track relation
//! in compressed sparse-row form lets factorisation and mesh planning visit only observations that
//! can exist at a frame instead of rescanning the complete clip history.

use super::trajectory_data::{FeatureTrajectory, MAX_ACTIVE_TRACKS, MAX_COORDINATE, MAX_FRAMES};

#[derive(Clone, Debug)]
pub(super) struct TrajectoryIndex {
    offsets: Vec<usize>,
    ids: Vec<u32>,
    frames: usize,
}

impl TrajectoryIndex {
    /// Build a checked frame-to-track CSR index.
    pub(super) fn new(tracks: &[FeatureTrajectory], frames: usize) -> Option<Self> {
        if frames > MAX_FRAMES || tracks.len() > frames.saturating_mul(MAX_ACTIVE_TRACKS) {
            return None;
        }
        let mut counts = vec![0usize; frames];
        let mut total = 0usize;
        for track in tracks {
            if filmcraft_media::cancel::cancelled() {
                return None;
            }
            if track.start > frames {
                return None;
            }
            if track.points.is_empty() {
                continue;
            }
            if track.points.iter().flatten().any(|value| !value.is_finite() || value.abs() > MAX_COORDINATE) {
                return None;
            }
            let end = track.start.checked_add(track.points.len())?;
            if end > frames {
                return None;
            }
            total = total.checked_add(track.points.len())?;
            if total > frames.saturating_mul(MAX_ACTIVE_TRACKS) {
                return None;
            }
            for frame in track.start..end {
                let count = counts.get_mut(frame)?;
                *count = count.checked_add(1)?;
                if *count > MAX_ACTIVE_TRACKS {
                    return None;
                }
            }
        }

        let mut offsets: Vec<usize> = Vec::with_capacity(frames.saturating_add(1));
        offsets.push(0);
        for count in counts {
            let next = offsets.last().copied()?.checked_add(count)?;
            offsets.push(next);
        }
        let mut ids = Vec::with_capacity(total);
        ids.resize(offsets.last().copied()?, 0);
        let mut cursors = offsets.get(..frames)?.to_vec();
        // Filling by trajectory ID preserves deterministic sorted rows.
        for (track_id, track) in tracks.iter().enumerate() {
            if filmcraft_media::cancel::cancelled() {
                return None;
            }
            let track_id = u32::try_from(track_id).ok()?;
            let end = track.start.checked_add(track.points.len())?;
            if end > frames {
                return None;
            }
            for frame in track.start..end {
                let cursor = cursors.get_mut(frame)?;
                let slot = ids.get_mut(*cursor)?;
                *slot = track_id;
                *cursor = cursor.checked_add(1)?;
            }
        }

        Some(Self { offsets, ids, frames })
    }

    #[inline]
    pub(super) fn at(&self, frame: usize) -> &[u32] {
        let Some(&start) = self.offsets.get(frame) else {
            return self.ids.get(0..0).unwrap_or(&[]);
        };
        let Some(&end) = self.offsets.get(frame.saturating_add(1)) else {
            return self.ids.get(0..0).unwrap_or(&[]);
        };
        self.ids.get(start..end).unwrap_or(&[])
    }

    #[inline]
    pub(super) const fn frames(&self) -> usize {
        self.frames
    }
}

#[cfg(test)]
mod tests {
    use super::TrajectoryIndex;
    use crate::vfx::temporal::trajectory_data::FeatureTrajectory;

    #[test]
    fn csr_rows_are_sorted_and_lifetime_checked() {
        let tracks = vec![
            FeatureTrajectory { start: 1, points: vec![[1.0, 2.0]; 3], rejected: false },
            FeatureTrajectory { start: 0, points: vec![[3.0, 4.0]; 2], rejected: false },
        ];
        let index = TrajectoryIndex::new(&tracks, 4).expect("bounded observations");
        assert_eq!(index.at(0), &[1]);
        assert_eq!(index.at(1), &[0, 1]);
        assert!(index.at(4).is_empty());
        assert!(TrajectoryIndex::new(&[FeatureTrajectory { start: 3, points: vec![[f32::NAN, 0.0]], rejected: false }], 4).is_none());
    }
}

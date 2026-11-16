//! What is known of the Warp Stabilizer analyses of one source: ranges of its frames with the
//! motion from each frame to the next.
//!
//! A step depends only on the two frames it joins, so the steps a clip needs can come from the
//! analysis of any clip that read the same frames of the same source the same way. Trimming a
//! clip, cutting it in two or moving its in point by whole frames reads a part of what was read
//! already, and extending it only needs the new frames.

use filmcraft_geom::Affine;

use super::StabilizerPath;

/// Most steps kept for one source (about 4.8 hours of 60 fps frames, 48 MB).
pub(crate) const MAX_SOURCE_STEPS: usize = 1 << 20;

/// Frames `start ..= start + steps.len()` of a source's frame grid, and the motion from each
/// frame to the next.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Range {
    pub start: i64,
    pub steps: Vec<Affine>,
}

impl Range {
    /// The last frame, `None` when it lies beyond `i64`.
    pub fn last(&self) -> Option<i64> {
        self.start.checked_add(i64::try_from(self.steps.len()).ok()?)
    }
}

/// The analysed ranges of one source's frame grid, sorted and sharing no frame, with the size of
/// its frames at analysis scale and the frame rate they were read at.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct Ranges {
    pub dims: Option<(usize, usize)>,
    pub fps: f64,
    pub ranges: Vec<Range>,
}

/// The clip frames `0..n` from grid frame `first` that `r` holds, as `from..to` (empty when
/// none).
fn overlap(r: &Range, first: i64, n: usize) -> (usize, usize) {
    let (Some(last), Ok(n64)) = (r.last(), i64::try_from(n)) else { return (0, 0) };
    let Some(end) = first.checked_add(n64) else { return (0, 0) };
    let (from, to) = (r.start.max(first), last.saturating_add(1).min(end));
    if from >= to {
        return (0, 0);
    }
    ((from - first) as usize, (to - first) as usize)
}

impl Ranges {
    pub fn steps(&self) -> usize {
        self.ranges.iter().map(|r| r.steps.len()).sum()
    }

    /// The path of the clip whose `n` frames start at grid frame `first`, when one range holds
    /// them all.
    pub fn path(&self, first: i64, n: usize) -> Option<StabilizerPath> {
        let (w, h) = self.dims?;
        let steps = if n <= 1 {
            Vec::new()
        } else {
            let r = self.ranges.iter().find(|r| overlap(r, first, n) == (0, n))?;
            let from = usize::try_from(first.checked_sub(r.start)?).ok()?;
            r.steps.get(from..from.checked_add(n - 1)?)?.to_vec()
        };
        Some(StabilizerPath { w, h, steps, fps: self.fps })
    }

    /// How many of the clip's frames are analysed.
    pub fn known(&self, first: i64, n: usize) -> usize {
        self.ranges
            .iter()
            .map(|r| {
                let (a, b) = overlap(r, first, n);
                b - a
            })
            .sum::<usize>()
            .min(n)
    }

    /// The runs of clip frames (`a..=b`) to read so that every step of the clip is known: each
    /// starts at the last known frame before a missing step and ends at the first known one after.
    pub fn gaps(&self, first: i64, n: usize) -> Vec<(usize, usize)> {
        if n <= 1 {
            return if self.dims.is_some() { Vec::new() } else { vec![(0, 0)] };
        }
        // step i joins clip frames i and i + 1
        let mut known = vec![false; n - 1];
        for r in &self.ranges {
            let (a, b) = overlap(r, first, n);
            for k in known.iter_mut().take(b.saturating_sub(1)).skip(a) {
                *k = true;
            }
        }
        let mut gaps = Vec::new();
        let mut i = 0;
        while i < n - 1 {
            if known[i] {
                i += 1;
                continue;
            }
            let start = i;
            while i < n - 1 && !known[i] {
                i += 1;
            }
            gaps.push((start, i));
        }
        gaps
    }

    /// Add `new`, joining it with the ranges it shares a frame with. A range without a step adds
    /// nothing. When the source would hold more than [`MAX_SOURCE_STEPS`], the other ranges go.
    pub fn insert(&mut self, new: Range) {
        let Some(new_last) = new.last() else { return };
        if new.steps.is_empty() || new.steps.len() > MAX_SOURCE_STEPS {
            return;
        }
        let (joined, kept): (Vec<Range>, Vec<Range>) =
            std::mem::take(&mut self.ranges).into_iter().partition(|r| r.last().is_some_and(|last| r.start <= new_last && last >= new.start));
        self.ranges = kept;
        let start = joined.iter().map(|r| r.start).fold(new.start, i64::min);
        let last = joined.iter().filter_map(Range::last).fold(new_last, i64::max);
        let len = usize::try_from(last - start).unwrap_or(usize::MAX);
        let merged = if len > MAX_SOURCE_STEPS {
            new
        } else {
            let mut steps: Vec<Option<Affine>> = vec![None; len];
            for r in joined.iter().chain(std::iter::once(&new)) {
                let at = (r.start - start) as usize;
                for (slot, s) in steps.iter_mut().skip(at).zip(&r.steps) {
                    *slot = Some(*s);
                }
            }
            // ranges that share a frame leave no step between them unknown
            Range { start, steps: steps.into_iter().map(|s| s.unwrap_or(Affine::IDENTITY)).collect() }
        };
        while self.steps() + merged.steps.len() > MAX_SOURCE_STEPS && !self.ranges.is_empty() {
            // the range farthest from the new one goes first
            let far = self.ranges.iter().enumerate().max_by_key(|(_, r)| r.start.abs_diff(merged.start)).map(|(i, _)| i);
            if let Some(i) = far {
                self.ranges.remove(i);
            }
        }
        let at = self.ranges.partition_point(|r| r.start < merged.start);
        self.ranges.insert(at, merged);
    }
}

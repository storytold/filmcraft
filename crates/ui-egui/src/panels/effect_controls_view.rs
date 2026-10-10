//! The Effect Controls time viewport. Times remain ticks; floating point is only screen geometry.

use egui::Rect;
use filmcraft_time::{Tick, TimeRange};
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct EffectControlsView {
    pub start: Tick,
    /// Zero requests a fit to the selected clip.
    pub duration: Tick,
    #[serde(skip)]
    context: Option<(Option<u64>, u64, TimeRange)>,
}

impl EffectControlsView {
    pub(crate) fn bind(&mut self, sequence: Option<u64>, clip: u64, full: TimeRange, frame: Tick) {
        match self.context {
            Some((s, c, previous)) if s == sequence && c == clip => {
                // A move carries the view with the clip. A trim retains the visible sequence
                // times and only clamps them to the new bounds.
                if previous.duration == full.duration {
                    self.start += full.start - previous.start;
                }
            }
            _ => self.duration = Tick::ZERO,
        }
        self.context = Some((sequence, clip, full));
        self.normalize(full, frame);
    }

    pub(crate) fn normalize(&mut self, full: TimeRange, frame: Tick) {
        let maximum = full.duration.max(Tick(1));
        let minimum = frame.max(Tick(1)).min(maximum);
        if self.duration.0 <= 0 {
            self.start = full.start;
            self.duration = maximum;
        } else {
            self.duration = self.duration.clamp(minimum, maximum);
            self.start = self.start.clamp(full.start, (full.end() - self.duration).max(full.start));
        }
    }

    pub(crate) fn range(&self, fallback: TimeRange) -> TimeRange {
        if self.duration.0 > 0 { TimeRange::new(self.start, self.duration) } else { TimeRange::new(fallback.start, fallback.duration.max(Tick(1))) }
    }

    pub(crate) fn zoom(&mut self, factor: f64, anchor: Tick, full: TimeRange, frame: Tick) {
        if !factor.is_finite() || factor <= 0.0 {
            return;
        }
        self.normalize(full, frame);
        let anchor = anchor.clamp(self.start, self.start + self.duration);
        let denominator = (factor.clamp(0.000_001, 1_000_000.0) * 1_000_000.0).round() as i64;
        let duration =
            self.duration.mul_ratio(1_000_000, denominator.max(1)).clamp(frame.max(Tick(1)).min(full.duration.max(Tick(1))), full.duration.max(Tick(1)));
        self.start = anchor - (anchor - self.start).mul_ratio(duration.0, self.duration.0);
        self.duration = duration;
        self.normalize(full, frame);
    }
}

pub(crate) fn x_at(range: TimeRange, rect: Rect, time: Tick) -> f32 {
    rect.min.x + ((time - range.start).0 as f64 / range.duration.0.max(1) as f64) as f32 * rect.width()
}

/// Convert a screen fraction to ticks with bounded integer arithmetic. Signed fractions pan;
/// mouse coordinates themselves are clamped by `tick_at` before becoming an edit time.
pub(crate) fn scaled_ticks(duration: Tick, fraction: f64) -> Tick {
    if !fraction.is_finite() {
        return Tick::ZERO;
    }
    let numerator = (fraction.abs().min(1_000_000.0) * 1_000_000_000.0).round() as i64;
    let ticks = duration.mul_ratio(numerator, 1_000_000_000);
    if fraction < 0.0 { -ticks } else { ticks }
}

pub(crate) fn tick_at(range: TimeRange, rect: Rect, x: f32) -> Tick {
    if !x.is_finite() || !rect.width().is_finite() || rect.width() <= 0.0 {
        return range.start;
    }
    range.start + scaled_ticks(range.duration, ((x - rect.min.x) as f64 / rect.width() as f64).clamp(0.0, 1.0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn zoom_keeps_the_anchor_and_clamps_to_a_frame() {
        let full = TimeRange::new(Tick(100), Tick(1000));
        let mut v = EffectControlsView::default();
        v.bind(Some(1), 2, full, Tick(10));
        v.zoom(2.0, Tick(350), full, Tick(10));
        assert_eq!((v.start, v.duration), (Tick(225), Tick(500)));
        v.zoom(1e20, Tick(350), full, Tick(10));
        assert_eq!(v.duration, Tick(10));
        v.zoom(1e-20, Tick(350), full, Tick(10));
        assert_eq!((v.start, v.duration), (Tick(100), Tick(1000)));
    }

    #[test]
    fn clip_movement_preserves_the_view_and_selection_changes_fit() {
        let mut v = EffectControlsView::default();
        v.bind(Some(1), 2, TimeRange::new(Tick(100), Tick(1000)), Tick(10));
        v.start = Tick(400);
        v.duration = Tick(200);
        v.bind(Some(1), 2, TimeRange::new(Tick(200), Tick(1000)), Tick(10));
        assert_eq!((v.start, v.duration), (Tick(500), Tick(200)));
        v.bind(Some(1), 2, TimeRange::new(Tick(200), Tick(250)), Tick(10));
        assert_eq!((v.start, v.duration), (Tick(250), Tick(200)));
        v.bind(Some(1), 3, TimeRange::new(Tick(0), Tick(700)), Tick(10));
        assert_eq!((v.start, v.duration), (Tick(0), Tick(700)));
    }

    #[test]
    fn an_in_point_trim_preserves_visible_sequence_times() {
        let mut v = EffectControlsView::default();
        v.bind(Some(1), 2, TimeRange::new(Tick(0), Tick(1000)), Tick(10));
        v.start = Tick(400);
        v.duration = Tick(200);
        v.bind(Some(1), 2, TimeRange::new(Tick(200), Tick(800)), Tick(10));
        assert_eq!((v.start, v.duration), (Tick(400), Tick(200)));
    }

    #[test]
    fn hostile_ranges_and_geometry_are_bounded() {
        for start in [i64::MIN, 0, i64::MAX] {
            for duration in [i64::MIN, -1, 0, 1, i64::MAX] {
                let full = TimeRange::new(Tick(start), Tick(duration));
                let mut v = EffectControlsView { start: Tick(i64::MAX), duration: Tick(duration), ..Default::default() };
                v.normalize(full, Tick(10));
                for factor in [f64::NAN, f64::INFINITY, -1.0, 0.0, 1e300, 1e-300] {
                    v.zoom(factor, Tick(start), full, Tick(10));
                    assert!(v.duration.0 > 0);
                    for width in [0.0, -1.0, f32::NAN, f32::INFINITY] {
                        let rect = Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(width, 20.0));
                        assert_eq!(tick_at(v.range(full), rect, f32::NAN), v.start);
                    }
                }
            }
        }
        let range = TimeRange::new(Tick(100), Tick(1000));
        let rect = Rect::from_min_size(egui::pos2(10.0, 0.0), egui::vec2(200.0, 20.0));
        assert_eq!(tick_at(range, rect, 60.0), Tick(350));
        assert_eq!(x_at(range, rect, Tick(350)), 60.0);
    }
}

//! Time Remapping: a clip's playback speed as a curve over its media.
//!
//! When the Speed of a clip's intrinsic Time Remapping effect is keyframed, it sets how fast each
//! part of the media plays. Like every effect keyframe, a speed keyframe sits at a media time, so a
//! speed change stays on the source frame it was set on through trims, razor cuts and slips.
//!
//! [`TimeMap`] turns the curve into the clip's timing: Φ(m), the timeline time it takes to play
//! the media up to media time `m`, and its inverse. Φ is the integral of 1 / speed over the media,
//! so it is built once per curve as a table of knots and read by straight interpolation between
//! them: increasing in both directions, so every media time has exactly one timeline time and back.
//! A clip shows media `m` at `start + Φ(m) − Φ(source_in)` (reversed: counted back from its out
//! point), and its length is `Φ(source_out) − Φ(source_in)`.

use filmcraft_time::Tick;

use crate::keyframe::{Interpolation, Keyframe, Param};

/// Slowest Time Remapping speed, in percent. Lower values (and zero or negative ones, which would
/// stop or reverse the clip) play at this speed.
pub const MIN_SPEED_PERCENT: f64 = 1.0;
/// Fastest Time Remapping speed, in percent.
pub const MAX_SPEED_PERCENT: f64 = 10_000.0;

/// Straight pieces an eased segment between two keyframes is split into.
const STEPS: usize = 64;
/// Most pieces in one curve, so a project with a great many keyframes stays cheap to play.
const MAX_PIECES: usize = 16_384;

/// The speed (media per timeline tick) a Time Remapping value in percent plays at.
pub fn rate_of_percent(percent: f64) -> f64 {
    if percent.is_finite() { percent.clamp(MIN_SPEED_PERCENT, MAX_SPEED_PERCENT) / 100.0 } else { 1.0 }
}

/// Timing of a clip whose Time Remapping speed is keyframed (see the module docs).
#[derive(Clone, Debug, PartialEq)]
pub struct TimeMap {
    /// (media time, Φ): Φ is the timeline time to play the media from the first knot. Both
    /// strictly increase.
    knots: Vec<(f64, f64)>,
    /// Speed before the first knot and after the last one (the first and last keyframes' values).
    head_rate: f64,
    tail_rate: f64,
}

impl TimeMap {
    /// The map of a keyframed speed parameter (values in percent, keyframes at media times), or
    /// None when the parameter has no keyframes.
    pub fn from_param(p: &Param) -> Option<TimeMap> {
        if p.keyframes.is_empty() {
            return None;
        }
        // a project file can hold keyframes out of order or at one time twice: read a sorted copy
        let sorted;
        let p = if p.keyframes.windows(2).all(|w| w[0].time < w[1].time) {
            p
        } else {
            let mut keys: Vec<Keyframe> = p.keyframes.clone();
            keys.sort_by_key(|k| k.time);
            keys.dedup_by_key(|k| k.time);
            sorted = Param { value: p.value.clone(), keyframes: keys };
            &sorted
        };
        let keys = &p.keyframes;
        let rate = |m: f64| rate_of_percent(p.scalar_at(Tick(m.round() as i64)));
        let first = keys.first()?;
        let last = keys.last()?;
        let segments = keys.len() - 1;
        let steps = (MAX_PIECES / segments.max(1)).clamp(1, STEPS);
        let mut knots = Vec::with_capacity(segments * steps + 1);
        let mut phi = 0.0;
        knots.push((first.time.0 as f64, phi));
        for w in keys.windows(2) {
            let (a, b) = (&w[0], &w[1]);
            let (ma, mb) = (a.time.0 as f64, b.time.0 as f64);
            let constant = a.interp == Interpolation::Hold || !a.value.interpolates() || a.value == b.value;
            if constant {
                phi += (mb - ma) / rate(ma);
                knots.push((mb, phi));
                continue;
            }
            for j in 1..=steps {
                let m0 = ma + (mb - ma) * (j - 1) as f64 / steps as f64;
                let m1 = if j == steps { mb } else { ma + (mb - ma) * j as f64 / steps as f64 };
                // Simpson's rule for the integral of 1 / speed over the piece
                phi += (m1 - m0) / 6.0 * (1.0 / rate(m0) + 4.0 / rate((m0 + m1) / 2.0) + 1.0 / rate(m1));
                knots.push((m1, phi));
            }
        }
        // tiny pieces can round to no step in media or in Φ: keep the knots strictly increasing
        knots.dedup_by(|b, a| b.0 <= a.0 || b.1 <= a.1);
        Some(TimeMap { knots, head_rate: rate(first.time.0 as f64 - 1.0), tail_rate: rate(last.time.0 as f64 + 1.0) })
    }

    /// Φ(m): timeline time to play the media up to media time `m`, from the first keyframe
    /// (negative before it).
    pub fn timeline_at(&self, m: f64) -> f64 {
        let k = &self.knots;
        let (Some(&(m0, p0)), Some(&(mn, pn))) = (k.first(), k.last()) else { return m / self.head_rate };
        if m <= m0 {
            return p0 + (m - m0) / self.head_rate;
        }
        if m >= mn {
            return pn + (m - mn) / self.tail_rate;
        }
        let i = k.partition_point(|kn| kn.0 <= m);
        match (k.get(i.wrapping_sub(1)), k.get(i)) {
            (Some(&(ma, pa)), Some(&(mb, pb))) => pa + (pb - pa) * (m - ma) / (mb - ma),
            _ => pn,
        }
    }

    /// The media time reached at Φ = `y` (the inverse of [`TimeMap::timeline_at`]).
    pub fn media_at(&self, y: f64) -> f64 {
        let k = &self.knots;
        let (Some(&(m0, p0)), Some(&(mn, pn))) = (k.first(), k.last()) else { return y * self.head_rate };
        if y <= p0 {
            return m0 + (y - p0) * self.head_rate;
        }
        if y >= pn {
            return mn + (y - pn) * self.tail_rate;
        }
        let i = k.partition_point(|kn| kn.1 <= y);
        match (k.get(i.wrapping_sub(1)), k.get(i)) {
            (Some(&(ma, pa)), Some(&(mb, pb))) => ma + (mb - ma) * (y - pa) / (pb - pa),
            _ => mn,
        }
    }

    /// The media time reached by playing `d` of timeline (backwards when negative) from media
    /// time `from`.
    pub fn advance(&self, from: Tick, d: Tick) -> Tick {
        Tick(self.media_at(self.timeline_at(from.0 as f64) + d.0 as f64).round() as i64)
    }

    /// Timeline time to play the media from `a` to `b` (negative when `b` comes first).
    pub fn span(&self, a: Tick, b: Tick) -> f64 {
        self.timeline_at(b.0 as f64) - self.timeline_at(a.0 as f64)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::keyframe::ParamValue;

    const S: i64 = filmcraft_time::TICKS_PER_SECOND;

    fn curve(keys: &[(i64, f64, Interpolation)]) -> Param {
        let mut p = Param::new(ParamValue::Float(100.0));
        for &(t, v, interp) in keys {
            p.put_keyframe(Tick(t), ParamValue::Float(v));
            if let Ok(i) = p.keyframes.binary_search_by_key(&Tick(t), |k| k.time) {
                p.keyframes[i].interp = interp;
            }
        }
        p
    }

    #[test]
    fn a_static_parameter_has_no_map() {
        assert!(TimeMap::from_param(&Param::new(ParamValue::Float(400.0))).is_none());
    }

    #[test]
    fn one_keyframe_is_a_constant_speed() {
        let m = TimeMap::from_param(&curve(&[(S, 400.0, Interpolation::Linear)])).unwrap();
        // two seconds of timeline play eight seconds of media, on either side of the keyframe
        assert_eq!(m.advance(Tick(0), Tick(2 * S)), Tick(8 * S));
        assert_eq!(m.advance(Tick(10 * S), Tick(-2 * S)), Tick(2 * S));
        assert!((m.span(Tick(0), Tick(8 * S)) - (2 * S) as f64).abs() < 1.0);
    }

    #[test]
    fn held_segments_change_speed_at_the_keyframe() {
        // 100 % for the first second of media, then 200 %
        let m = TimeMap::from_param(&curve(&[(0, 100.0, Interpolation::Hold), (S, 200.0, Interpolation::Hold)])).unwrap();
        assert_eq!(m.advance(Tick(0), Tick(S)), Tick(S));
        // the next second of timeline plays two seconds of media
        assert_eq!(m.advance(Tick(0), Tick(2 * S)), Tick(3 * S));
        assert!((m.span(Tick(0), Tick(3 * S)) - (2 * S) as f64).abs() < 1.0);
    }

    #[test]
    fn a_linear_ramp_matches_its_integral() {
        // speed rises linearly from 100 % to 300 % over two seconds of media: playing it takes
        // ∫ dm / v(m) = 2 s · ln(3) / (3 − 1) of timeline
        let m = TimeMap::from_param(&curve(&[(0, 100.0, Interpolation::Linear), (2 * S, 300.0, Interpolation::Linear)])).unwrap();
        let want = 2.0 * S as f64 * 3f64.ln() / 2.0;
        let got = m.span(Tick(0), Tick(2 * S));
        assert!((got - want).abs() / want < 1e-6, "{got} vs {want}");
    }

    #[test]
    fn eased_ramps_play_through_every_frame_in_order() {
        let m = TimeMap::from_param(&curve(&[(S, 50.0, Interpolation::Bezier), (3 * S, 800.0, Interpolation::Bezier), (4 * S, 100.0, Interpolation::Linear)]))
            .unwrap();
        let mut last = Tick::MIN;
        for i in 0..2000 {
            let t = Tick(i * S / 200);
            let at = m.advance(Tick(0), t);
            assert!(at > last, "media goes forward at {t:?}");
            last = at;
            // and the inverse brings it back to the same timeline time
            assert!((m.span(Tick(0), at) - t.0 as f64).abs() < 2.0);
        }
    }

    #[test]
    fn out_of_range_and_hostile_values_stay_finite() {
        for v in [0.0, -500.0, f64::NAN, f64::INFINITY, 1e300] {
            let m = TimeMap::from_param(&curve(&[(0, v, Interpolation::Linear), (S, 100.0, Interpolation::Linear)])).unwrap();
            let at = m.advance(Tick(0), Tick(S));
            assert!(at > Tick(0), "{v}: {at:?}");
            assert!(m.span(Tick(0), Tick(S)).is_finite());
        }
        // keyframes out of order and on one time (only a project file can hold them)
        let mut p = curve(&[(0, 100.0, Interpolation::Linear), (S, 200.0, Interpolation::Linear)]);
        p.keyframes.reverse();
        p.keyframes.push(p.keyframes[0].clone());
        let m = TimeMap::from_param(&p).unwrap();
        assert!(m.advance(Tick(0), Tick(S)) > Tick(S));
        // extreme media times saturate instead of wrapping
        let _ = m.advance(Tick::MAX, Tick::MAX);
        let _ = m.advance(Tick::MIN, Tick::MIN);
    }

    #[test]
    fn many_keyframes_stay_bounded() {
        let keys: Vec<(i64, f64, Interpolation)> = (0..50_000).map(|i| (i * 1000, 50.0 + (i % 7) as f64 * 40.0, Interpolation::Bezier)).collect();
        let m = TimeMap::from_param(&curve(&keys)).unwrap();
        assert!(m.knots.len() <= MAX_PIECES + keys.len());
    }
}

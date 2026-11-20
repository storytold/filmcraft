//! Local seam-level matching; no global compression, tempo or stereo changes.
use super::Plan;

#[derive(Clone, Debug, PartialEq)]
pub struct JoinLevel {
    pub before: f32,
    pub after: f32,
    pub correlation: f32,
}

/// Span uses the caller's units, like Plan. Gains only attenuate (up to 12 dB).
#[derive(Clone, Debug, PartialEq)]
pub struct LevelMatch {
    pub span: i64,
    pub joins: Vec<JoinLevel>,
}

impl LevelMatch {
    pub fn to_text(&self) -> String {
        let mut text = self.span.to_string();
        for j in &self.joins {
            text.push_str(&format!(";{},{},{}", j.before, j.after, j.correlation));
        }
        text
    }

    pub fn from_text(text: &str) -> Option<Self> {
        if text.len() > 8192 {
            return None;
        }
        let mut parts = text.split(';');
        let span: i64 = parts.next()?.parse().ok()?;
        if !(1..=i64::MAX / 1024).contains(&span) {
            return None;
        }
        let mut joins = Vec::new();
        for part in parts {
            if joins.len() >= 32 {
                return None;
            }
            let mut values = part.split(',');
            let before: f32 = values.next()?.parse().ok()?;
            let after: f32 = values.next()?.parse().ok()?;
            let correlation: f32 = values.next()?.parse().ok()?;
            if values.next().is_some()
                || !before.is_finite()
                || !after.is_finite()
                || !correlation.is_finite()
                || !(0.25..=1.0).contains(&before)
                || !(0.25..=1.0).contains(&after)
                || !(0.0..=1.0).contains(&correlation)
            {
                return None;
            }
            joins.push(JoinLevel { before, after, correlation });
        }
        Some(Self { span, joins })
    }
}

/// Measure short-term RMS on the audible sides of each edit, outside the fade.
/// The common stereo gain is held near the joint and smoothly released in dB.
pub fn match_levels(plan: &Plan, sr: u32, period: f64, read: &mut dyn FnMut(i64, usize) -> Option<Vec<Vec<f32>>>) -> Option<LevelMatch> {
    if !(1..=384_000).contains(&sr)
        || !period.is_finite()
        || period < 1.0
        || plan.pieces.len() > 33
        || plan.xfade < 0
        || plan.pieces.iter().any(|p| p.src < 0 || p.len <= 0 || p.src.checked_add(p.len).is_none())
    {
        return None;
    }
    let span = (period * 2.0).clamp(f64::from(sr) * 0.8, f64::from(sr) * 2.0).round() as i64;
    let width = (period * 0.35).clamp(f64::from(sr) * 0.12, f64::from(sr) * 0.25).round() as i64;
    let guard = plan.xfade.min(i64::from(sr) / 10);
    let rms = |audio: &[Vec<f32>]| {
        let mut sum = 0f64;
        let mut count = 0usize;
        for x in audio.iter().flatten().filter(|x| x.is_finite()) {
            sum += f64::from(*x).powi(2);
            count += 1;
        }
        (sum / count.max(1) as f64).sqrt()
    };
    let mut joins = Vec::new();
    for pair in plan.pieces.windows(2) {
        let out = pair[0].src.checked_add(pair[0].len)?;
        let incoming = pair[1].src;
        let width = width.min(pair[0].len / 3).min(pair[1].len / 3).max(1);
        let left = read(out.saturating_sub(guard).saturating_sub(width).max(0), width as usize)?;
        let right = read(incoming.checked_add(guard)?, width as usize)?;
        let (l, r) = (rms(&left), rms(&right));
        let (mut before, mut after) = (1.0, 1.0);
        if l > 1e-4 && r > 1e-4 && (20.0 * (r / l).log10()).abs() >= 0.75 {
            if l > r {
                before = (r / l).clamp(0.25, 1.0) as f32;
            } else {
                after = (l / r).clamp(0.25, 1.0) as f32;
            }
        }
        // Positive correlation can make an equal-power fade peak by 3 dB.
        // Correct only that surplus; negative correlation never triggers a boost.
        let n = plan.xfade.min(i64::from(sr) / 10).max(0) as usize;
        let (mut xy, mut xx, mut yy) = (0f64, 0f64, 0f64);
        if n > 0 {
            let x = read(out.saturating_sub(n as i64 / 2).max(0), n)?;
            let y = read(incoming.saturating_sub(n as i64 / 2).max(0), n)?;
            for (a, b) in x.iter().zip(&y).flat_map(|(a, b)| a.iter().zip(b)) {
                if a.is_finite() && b.is_finite() {
                    xy += f64::from(*a) * f64::from(*b);
                    xx += f64::from(*a).powi(2);
                    yy += f64::from(*b).powi(2);
                }
            }
        }
        let correlation = if xx * yy > 1e-20 { (xy / (xx * yy).sqrt()).clamp(0.0, 1.0) as f32 } else { 0.0 };
        joins.push(JoinLevel { before, after, correlation });
    }
    Some(LevelMatch { span, joins })
}

pub(super) fn envelope(log_gain: f64, distance: i64, radius: i64) -> f64 {
    if log_gain == 0.0 || distance >= radius {
        return 1.0;
    }
    let radius = radius.max(1) as f64;
    let hold = radius / 3.0;
    let weight = ((radius - distance.max(0) as f64) / (radius - hold)).clamp(0.0, 1.0);
    (log_gain * weight * weight * (3.0 - 2.0 * weight)).exp()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::remix::{Piece, render, render_with_levels};
    fn source(start: i64, n: usize) -> Vec<Vec<f32>> {
        let left: Vec<_> = (0..n)
            .map(|i| {
                let t = start + i as i64;
                (std::f64::consts::TAU * t as f64 / 10.0).sin() as f32 * if t < 4000 { 0.2 } else { 0.5 }
            })
            .collect();
        let right = left.iter().map(|x| x * 0.5).collect();
        vec![left, right]
    }
    fn plan() -> Plan {
        Plan { pieces: vec![Piece { src: 0, len: 2000 }, Piece { src: 5000, len: 2000 }], xfade: 20 }
    }
    #[test]
    fn matches_an_eight_db_seam_without_stereo_change_or_crossfade_bump() {
        let p = plan();
        let levels = match_levels(&p, 1000, 500.0, &mut |s, n| Some(source(s, n))).unwrap();
        let j = &levels.joins[0];
        assert!((j.after - 0.4).abs() < 1e-5);
        assert_eq!(j.before, 1.0);
        assert!(j.correlation > 0.99);
        let out = render_with_levels(&p, Some(&levels), 0, 4000, 2, &mut source);
        let rms = |s: &[f32]| (s.iter().map(|x| f64::from(*x).powi(2)).sum::<f64>() / s.len() as f64).sqrt();
        let before = rms(&out[0][1780..1980]);
        let after = rms(&out[0][2020..2220]);
        assert!((20.0 * (after / before).log10()).abs() < 0.01);
        for i in 1800..2200 {
            let expected = 0.2 * (std::f64::consts::TAU * i as f64 / 10.0).sin() as f32;
            assert!((out[0][i] - expected).abs() < 1e-5, "crossfade surplus at {i}");
            assert!((out[1][i] * 2.0 - out[0][i]).abs() < 1e-6);
        }
        assert_eq!(out[0][3501], source(6501, 1)[0][0], "original dynamics resume beyond the local ramp");
        let old = render(&p, 0, 4000, 2, &mut source);
        assert!(20.0 * (rms(&old[0][2020..2220]) / rms(&old[0][1780..1980])).log10() > 7.9);
        let mut blocks = vec![Vec::new(), Vec::new()];
        for start in (0..4000).step_by(137) {
            let block = render_with_levels(&p, Some(&levels), start as i64, (4000 - start).min(137), 2, &mut source);
            for c in 0..2 {
                blocks[c].extend_from_slice(&block[c]);
            }
        }
        assert_eq!(out, blocks, "seeks and block sizes cannot reset the gain envelope");
    }
    #[test]
    fn silence_and_invalid_level_data_cannot_trigger_a_boost() {
        let levels = match_levels(&plan(), 1000, 500.0, &mut |_, n| Some(vec![vec![0.0; n]])).unwrap();
        assert_eq!(levels.joins[0].before, 1.0);
        assert_eq!(levels.joins[0].after, 1.0);
        assert_eq!(LevelMatch::from_text(&levels.to_text()), Some(levels));
        for text in ["0;1,1,0", "100;NaN,1,0", "100;1,inf,0", "100;1,1,-1", "100;0,1,0", "100;1,1,0,2"] {
            assert!(LevelMatch::from_text(text).is_none(), "{text}");
        }
        assert!(match_levels(&plan(), 0, 500.0, &mut |_, _| None).is_none());
        assert!(match_levels(&plan(), 1000, f64::NAN, &mut |_, _| None).is_none());
    }
}

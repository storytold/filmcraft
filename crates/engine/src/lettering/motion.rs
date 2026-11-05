//! Text-motion presets for `lettering.apply` (`"preset": id`).
//!
//! A preset is data: the granularity it moves at (block, line, word), a base duration, a stagger,
//! an easing and a list of tracks (a property going from one offset to rest). Everything here is
//! pure numbers, so it is testable without a font or a session.
//!
//! The presets react to the speech instead of replaying one fixed timing:
//! - **speed**: fast talkers get shorter entrances, slow ones longer ([`Reaction::duration_k`]);
//! - **emphasis**: a hero word travels further, blurs more and takes a little longer
//!   ([`Reaction::hero_k`]);
//! - **pauses**: a word that follows a pause starts from further away, like a fresh start
//!   ([`Reaction::pause_k`]).
//!
//! Easings that the Bezier handles cannot draw (back, elastic, hold) are sampled into short linear
//! keyframes, one every ~33 ms, so the render reproduces the curve exactly.

use filmcraft_project::{Interpolation, Keyframe, ParamValue};
use filmcraft_time::Tick;

/// What a preset moves as one unit.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    Block,
    Line,
    Word,
}

/// An easing curve, `t` in 0..1 → progress (may leave 0..1 for back and elastic).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Ease {
    Linear,
    OutQuad,
    OutCubic,
    OutQuart,
    OutQuint,
    OutExpo,
    InOutQuad,
    InOutCubic,
    InOutSine,
    OutBack,
    Elastic,
    /// Jumps to the end at the start (the opacity of a "keyed" reveal).
    Hold,
}

/// Progress of easing `e` at `t`. Non-finite or out-of-range `t` is clamped, so it never panics.
pub fn ease(e: Ease, t: f64) -> f64 {
    let t = if t.is_finite() { t.clamp(0.0, 1.0) } else { 1.0 };
    match e {
        Ease::Linear => t,
        Ease::OutQuad => 1.0 - (1.0 - t).powi(2),
        Ease::OutCubic => 1.0 - (1.0 - t).powi(3),
        Ease::OutQuart => 1.0 - (1.0 - t).powi(4),
        Ease::OutQuint => 1.0 - (1.0 - t).powi(5),
        Ease::OutExpo => {
            if t >= 1.0 {
                1.0
            } else {
                1.0 - 2f64.powf(-10.0 * t)
            }
        }
        Ease::InOutQuad => {
            if t < 0.5 {
                2.0 * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(2) / 2.0
            }
        }
        Ease::InOutCubic => {
            if t < 0.5 {
                4.0 * t * t * t
            } else {
                1.0 - (-2.0 * t + 2.0).powi(3) / 2.0
            }
        }
        Ease::InOutSine => -((std::f64::consts::PI * t).cos() - 1.0) / 2.0,
        Ease::OutBack => {
            let (c1, c3) = (1.70158, 2.70158);
            1.0 + c3 * (t - 1.0).powi(3) + c1 * (t - 1.0).powi(2)
        }
        Ease::Elastic => {
            if t <= 0.0 {
                0.0
            } else if t >= 1.0 {
                1.0
            } else {
                let p = 0.45;
                2f64.powf(-10.0 * t) * ((t - p / 4.0) * std::f64::consts::TAU / p).sin() + 1.0
            }
        }
        Ease::Hold => {
            if t > 0.0 {
                1.0
            } else {
                0.0
            }
        }
    }
}

/// What a track changes. Offsets are relative to the word's resting place.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Prop {
    X,
    Y,
    /// Percent; 100 = rest.
    Scale,
    /// Degrees.
    Rotation,
    /// 0..1.
    Opacity,
    /// Layer pixels at 1920 px frame width.
    Blur,
}

/// One property travelling from `from` to its resting value.
#[derive(Clone, Copy, Debug)]
pub struct Track {
    pub prop: Prop,
    pub from: f64,
    /// Easing of this track when it differs from the preset's.
    pub ease: Option<Ease>,
    /// Opacity only: flicker through 0 → 1 → .4 → 1 in the first third.
    pub blink: bool,
    /// Which lines carry it: 0 all, 1 odd lines only, 2 even lines only.
    pub lines: u8,
}

const fn tr(prop: Prop, from: f64) -> Track {
    Track { prop, from, ease: None, blink: false, lines: 0 }
}
const fn tr_e(prop: Prop, from: f64, e: Ease) -> Track {
    Track { prop, from, ease: Some(e), blink: false, lines: 0 }
}

#[derive(Clone, Copy, Debug)]
pub struct Preset {
    pub id: &'static str,
    pub name: &'static str,
    pub scope: Scope,
    pub ms: f64,
    pub stagger_ms: f64,
    pub ease: Ease,
    pub tracks: &'static [Track],
}

use Ease::*;
use Prop::*;

/// The text-only presets of the spec. 09 (a continuous slide) and the decorated ones (badges,
/// marker, glow, water) are not here: they need more than a motion on the words.
pub const PRESETS: &[Preset] = &[
    Preset { id: "partial-blur-in-up", name: "Partial Blur In / Up", scope: Scope::Block, ms: 600.0, stagger_ms: 0.0, ease: OutCubic, tracks: &[tr(Y, 40.0), tr(Opacity, 0.0), tr(Blur, 16.0)] },
    Preset { id: "delay-up-slide", name: "Delay Up Slide", scope: Scope::Line, ms: 500.0, stagger_ms: 120.0, ease: OutQuint, tracks: &[tr(Y, 50.0), tr(Opacity, 0.0)] },
    Preset { id: "up-smooth", name: "Up Smooth", scope: Scope::Block, ms: 700.0, stagger_ms: 0.0, ease: InOutQuad, tracks: &[tr(Y, 60.0), tr(Opacity, 0.0)] },
    Preset {
        id: "blink-right-up-combined",
        name: "Blink / Right Slide / Up Smooth",
        scope: Scope::Line,
        ms: 800.0,
        stagger_ms: 150.0,
        ease: OutExpo,
        tracks: &[tr(X, -40.0), tr(Y, 30.0), Track { prop: Opacity, from: 0.0, ease: Some(Linear), blink: true, lines: 0 }],
    },
    Preset {
        id: "blinks-spinning-word",
        name: "Blinks Spinning",
        scope: Scope::Word,
        ms: 400.0,
        stagger_ms: 80.0,
        ease: OutBack,
        tracks: &[tr(Rotation, -90.0), tr_e(Opacity, 0.0, Hold)],
    },
    Preset { id: "left-smooth-word", name: "Left Smooth", scope: Scope::Word, ms: 450.0, stagger_ms: 70.0, ease: OutCubic, tracks: &[tr(X, -40.0), tr(Opacity, 0.0)] },
    Preset { id: "bouncy-follow-up", name: "Bouncy Follow Up", scope: Scope::Block, ms: 750.0, stagger_ms: 0.0, ease: Elastic, tracks: &[tr(Y, 80.0), tr_e(Opacity, 0.0, OutCubic)] },
    Preset { id: "up-smooth-script-word", name: "Up Smooth (Script)", scope: Scope::Word, ms: 550.0, stagger_ms: 100.0, ease: InOutQuad, tracks: &[tr(Y, 35.0), tr(Opacity, 0.0)] },
    Preset {
        id: "left-up-smooth-combined",
        name: "Left Smooth / Up Smooth",
        scope: Scope::Line,
        ms: 600.0,
        stagger_ms: 100.0,
        ease: OutCubic,
        tracks: &[
            Track { prop: X, from: -50.0, ease: None, blink: false, lines: 1 },
            Track { prop: Y, from: 40.0, ease: None, blink: false, lines: 2 },
            tr(Opacity, 0.0),
        ],
    },
    Preset { id: "caption-fade-scale", name: "Smooth Caption (Fade & Scale)", scope: Scope::Word, ms: 400.0, stagger_ms: 60.0, ease: OutQuart, tracks: &[tr(Scale, 90.0), tr(Opacity, 0.0), tr(Blur, 4.0)] },
];

pub fn preset(id: &str) -> Option<&'static Preset> {
    PRESETS.iter().find(|p| p.id == id)
}

pub fn preset_ids() -> Vec<&'static str> {
    PRESETS.iter().map(|p| p.id).collect()
}

/// How the speech bends a preset. All factors are 1.0 for a neutral voice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Reaction {
    /// Scales every entrance duration: <1 for fast speech, >1 for slow.
    pub duration_k: f64,
    /// Distance, blur and duration multiplier for hero words.
    pub hero_k: f64,
}

impl Reaction {
    pub const NEUTRAL: Reaction = Reaction { duration_k: 1.0, hero_k: 1.0 };
    /// Hero words travel this much further and take this much longer.
    pub const HERO: f64 = 1.3;
}

/// Median gap between the starts of consecutive words, seconds. `None` with fewer than two timed
/// words or no usable gaps.
pub fn median_gap(starts: &[f64]) -> Option<f64> {
    let mut gaps: Vec<f64> = starts.windows(2).map(|w| w[1] - w[0]).filter(|g| g.is_finite() && *g > 0.0).collect();
    if gaps.is_empty() {
        return None;
    }
    gaps.sort_by(|a, b| a.total_cmp(b));
    gaps.get(gaps.len() / 2).copied()
}

/// Entrance-duration factor for a median word gap: 0.30 s between words (a lively pace) is
/// neutral; the factor follows the pace and stays between 0.55 and 1.25.
pub fn duration_factor(gap: Option<f64>) -> f64 {
    gap.map_or(1.0, |g| (g / 0.30).clamp(0.55, 1.25))
}

/// Extra distance for a word that follows a pause of `pause` seconds (0 → none, ≥1 s → +30%).
pub fn pause_factor(pause: f64) -> f64 {
    if !pause.is_finite() || pause <= 0.25 {
        1.0
    } else {
        1.0 + 0.3 * ((pause - 0.25) / 0.75).clamp(0.0, 1.0)
    }
}

/// Sampled keyframes of `from → rest` over `dur` seconds from `t0`, easing `e`.
/// `scale_px` converts the spec's pixel offsets; `rest` is the resting value; `from` is the value
/// at the start (already absolute). Capped at [`MAX_SAMPLES`] keyframes.
pub const MAX_SAMPLES: usize = 40;

pub fn sample(t0: f64, dur: f64, e: Ease, from: f64, rest: f64, mk: impl Fn(f64) -> ParamValue) -> Vec<Keyframe> {
    let dur = if dur.is_finite() { dur.max(1e-3) } else { 1e-3 };
    let key = |t: f64, v: f64, interp: Interpolation| {
        let mut k = Keyframe::new(Tick::from_seconds_f64(t), mk(v));
        k.interp = interp;
        k
    };
    if e == Ease::Hold {
        return vec![key(t0, from, Interpolation::Hold), key(t0 + 1e-3, rest, Interpolation::Linear)];
    }
    let n = ((dur * 30.0).ceil() as usize).clamp(3, MAX_SAMPLES);
    (0..=n)
        .map(|i| {
            let u = i as f64 / n as f64;
            key(t0 + dur * u, from + (rest - from) * ease(e, u), Interpolation::Linear)
        })
        .collect()
}

/// Opacity flicker 0 → 1 → .4 → 1 across `dur` (the "blink").
pub fn blink_keys(t0: f64, dur: f64, mk: impl Fn(f64) -> ParamValue) -> Vec<Keyframe> {
    let dur = if dur.is_finite() { dur.max(1e-3) } else { 1e-3 };
    [(0.0, 0.0), (0.10, 1.0), (0.18, 0.4), (0.30, 1.0), (1.0, 1.0)]
        .iter()
        .map(|&(u, v)| {
            let mut k = Keyframe::new(Tick::from_seconds_f64(t0 + dur * u), mk(v));
            k.interp = Interpolation::Linear;
            k
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_easing_starts_at_zero_ends_at_one_and_never_panics() {
        for e in [Linear, OutQuad, OutCubic, OutQuart, OutQuint, OutExpo, InOutQuad, InOutCubic, InOutSine, OutBack, Elastic] {
            assert!(ease(e, 0.0).abs() < 1e-9, "{e:?} start");
            assert!((ease(e, 1.0) - 1.0).abs() < 1e-9, "{e:?} end");
            for t in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -3.0, 9.0] {
                assert!(ease(e, t).is_finite(), "{e:?} {t}");
            }
        }
        assert_eq!(ease(Hold, 0.0), 0.0);
        assert_eq!(ease(Hold, 0.01), 1.0);
    }

    #[test]
    fn back_and_elastic_overshoot_and_settle() {
        assert!((0..100).any(|i| ease(OutBack, i as f64 / 100.0) > 1.02));
        assert!((0..100).any(|i| ease(Elastic, i as f64 / 100.0) > 1.02));
    }

    #[test]
    fn preset_ids_are_unique_and_values_sane() {
        let mut ids = preset_ids();
        ids.sort_unstable();
        ids.dedup();
        assert_eq!(ids.len(), PRESETS.len());
        for p in PRESETS {
            assert!(p.ms > 0.0 && p.stagger_ms >= 0.0 && !p.tracks.is_empty(), "{}", p.id);
            assert!(preset(p.id).is_some());
        }
        assert!(preset("nope").is_none());
    }

    #[test]
    fn faster_speech_shortens_and_slower_lengthens_within_bounds() {
        let fast = duration_factor(median_gap(&[0.0, 0.12, 0.24, 0.36]));
        let slow = duration_factor(median_gap(&[0.0, 0.9, 1.8, 2.7]));
        assert!(fast < 1.0 && slow > 1.0);
        assert!((0.55..=1.25).contains(&fast) && (0.55..=1.25).contains(&slow));
        assert_eq!(duration_factor(None), 1.0);
        assert_eq!(median_gap(&[1.0]), None);
        assert_eq!(median_gap(&[f64::NAN, f64::NAN]), None);
    }

    #[test]
    fn a_pause_adds_distance_only_when_it_is_long() {
        assert_eq!(pause_factor(0.1), 1.0);
        assert_eq!(pause_factor(f64::NAN), 1.0);
        assert!(pause_factor(0.6) > 1.0 && pause_factor(5.0) <= 1.3 + 1e-9);
    }

    #[test]
    fn samples_are_bounded_and_end_on_rest() {
        let ks = sample(1.0, 1e9, OutCubic, 40.0, 0.0, ParamValue::Float);
        assert!(ks.len() <= MAX_SAMPLES + 1);
        let ks = sample(1.0, f64::NAN, Elastic, 40.0, 0.0, ParamValue::Float);
        assert!(ks.len() >= 3);
        let ks = sample(0.0, 0.5, OutCubic, 10.0, 0.0, ParamValue::Float);
        assert!(matches!(ks.last().map(|k| &k.value), Some(ParamValue::Float(v)) if v.abs() < 1e-9));
        let hold = sample(0.0, 0.4, Hold, 0.0, 1.0, ParamValue::Float);
        assert_eq!(hold.len(), 2);
    }
}

//! Lumetri ▸ Basic Correction ▸ **Auto Correct** (`lumetri.auto`): white balance and exposure
//! measured from the source picture, solved in Lumetri's own Basic Correction parameters so the
//! result is an ordinary, editable grade. A documented, non-ML method:
//!
//! 1. **Samples.** The analysed frames (the frame at the playhead, or a few spread over the clip)
//!    are read as straight-alpha linear RGB. Transparent pixels and pixels with a non-finite
//!    component are dropped; at most [`MAX_SAMPLES`] pixels are kept (an even stride over the
//!    rest), so a batch of 6K frames costs the same as one small one.
//! 2. **White balance** by a robust grey-world estimate. Clipped pixels (a channel above
//!    [`CLIP_LEVEL`]) and near-black pixels (below [`BLACK_LEVEL`]) are rejected outright, since
//!    neither carries the illuminant. Strongly saturated pixels (a coloured object, not a neutral
//!    surface) are down-weighted by their saturation *after* a provisional balance, in two
//!    passes, so a heavy cast does not make every neutral surface look saturated and get
//!    rejected. A second, independent estimate takes the bright but unclipped pixels (the 85th to
//!    99th luma percentile: lit neutral surfaces, usually the illuminant's colour). The two are
//!    blended in log space ([`BRIGHT_WEIGHT`]) and their disagreement feeds the confidence.
//! 3. **Exposure** from luma percentiles of every opaque pixel, after the white balance gains:
//!    the 50th percentile is brought to the target mid grey ([`Options::target_grey`], 18 %
//!    linear) and the result is capped so the 99th percentile stays under
//!    [`Options::highlight_ceiling`] — a correction never pushes highlights into clipping.
//! 4. **Confidence** (0…1) multiplies simple terms: how much of the picture was usable, how many
//!    pixels were measured, how well the two illuminant estimates agree, whether the signal was
//!    dark or flat, and whether the solved values had to be clamped to the sliders' ranges.
//!
//! The solve inverts [`crate::effects::white_balance_gains`], the gain model of Lumetri's
//! Temperature and Tint sliders, and Lumetri's Exposure (stops): the desired per-channel gains
//! `(ḡ/r̄, 1, ḡ/b̄)` fix the red/blue ratio (Temperature) and the green level against the mean of
//! red and blue (Tint); the overall scale goes into Exposure.

use serde::Serialize;

use crate::Image;
use crate::effects::white_balance_gains;

/// Most pixels analysed per call; larger inputs are strided.
pub const MAX_SAMPLES: usize = 1 << 20;
/// A channel at or above this (linear) is treated as clipped and never used for white balance.
pub const CLIP_LEVEL: f32 = 0.92;
/// A pixel whose brightest channel is below this carries noise, not colour.
pub const BLACK_LEVEL: f32 = 0.01;
/// Saturation (max − min) / max, after the provisional balance, above which a pixel no longer
/// counts as a neutral surface.
pub const MAX_NEUTRAL_SATURATION: f32 = 0.6;
/// Weight of the bright-surface illuminant estimate against grey world (log space).
pub const BRIGHT_WEIGHT: f32 = 0.25;
/// The sliders' ranges, as the Lumetri definition declares them.
pub const SLIDER_RANGE: f32 = 100.0;
pub const EXPOSURE_RANGE: f32 = 5.0;

/// What to measure and where to put the mid tones.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Options {
    pub white_balance: bool,
    pub exposure: bool,
    /// Linear luma the 50th percentile is brought to (0.18 = 18 % grey).
    pub target_grey: f32,
    /// The 99th luma percentile stays at or below this after the correction.
    pub highlight_ceiling: f32,
}

impl Default for Options {
    fn default() -> Self {
        Self { white_balance: true, exposure: true, target_grey: 0.18, highlight_ceiling: 0.9 }
    }
}

impl Options {
    /// Hostile values bounded: a target in 1…80 % and a ceiling in 10…100 %, ceiling above target.
    pub fn sanitized(self) -> Self {
        let target = if self.target_grey.is_finite() { self.target_grey.clamp(0.01, 0.8) } else { 0.18 };
        let ceiling = if self.highlight_ceiling.is_finite() { self.highlight_ceiling.clamp(0.1, 1.0) } else { 0.9 };
        Self { target_grey: target, highlight_ceiling: ceiling.max(target), ..self }
    }
}

/// The measurement and the Basic Correction values that correct it. Every number is finite.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    /// Opaque, finite pixels measured (after striding).
    pub pixels: usize,
    /// Pixels that counted towards the white balance (unclipped, not black, near neutral).
    pub usable: usize,
    /// Grey-world mean of the usable pixels (linear RGB).
    pub mean: [f32; 3],
    /// Mean of the bright unclipped pixels (linear RGB), when there were enough of them.
    pub bright: Option<[f32; 3]>,
    /// The per-channel gains that would neutralise the cast, green = 1.
    pub gains: [f32; 3],
    /// Red / blue of the measured cast (> 1: warm, < 1: cool).
    pub cast: f32,
    /// Luma percentiles (linear) after the white balance gains, before exposure.
    pub p1: f32,
    pub p50: f32,
    pub p99: f32,
    /// Lumetri Temperature and Tint sliders (−100…100) and Exposure (stops, −5…5).
    pub temperature: f32,
    pub tint: f32,
    pub exposure: f32,
    /// 0…1 for each measurement, and their minimum for the ones that were asked for.
    pub white_balance_confidence: f32,
    pub exposure_confidence: f32,
    pub confidence: f32,
    /// Whether a solved value hit a slider's end (the correction is partial).
    pub clamped: bool,
}

impl Analysis {
    fn empty() -> Self {
        Self {
            pixels: 0,
            usable: 0,
            mean: [0.0; 3],
            bright: None,
            gains: [1.0; 3],
            cast: 1.0,
            p1: 0.0,
            p50: 0.0,
            p99: 0.0,
            temperature: 0.0,
            tint: 0.0,
            exposure: 0.0,
            white_balance_confidence: 0.0,
            exposure_confidence: 0.0,
            confidence: 0.0,
            clamped: false,
        }
    }
}

/// Straight-alpha linear RGB of the opaque, finite pixels of `frames`, at most [`MAX_SAMPLES`].
pub fn samples(frames: &[&Image]) -> Vec<[f32; 3]> {
    let total: usize = frames.iter().map(|f| f.px.len() / 4).sum();
    let stride = total.div_ceil(MAX_SAMPLES).max(1);
    let mut out = Vec::with_capacity(total.min(MAX_SAMPLES));
    let mut i = 0usize;
    for f in frames {
        for p in f.px.as_chunks::<4>().0 {
            i = i.wrapping_add(1);
            let opaque = p[3].is_finite() && p[3] > 1e-3;
            if !i.is_multiple_of(stride) || !opaque {
                continue;
            }
            let c = [p[0] / p[3], p[1] / p[3], p[2] / p[3]];
            if c.iter().all(|v| v.is_finite()) {
                out.push(c.map(|v| v.max(0.0)));
            }
        }
    }
    out
}

fn luma(c: [f32; 3]) -> f32 {
    filmcraft_color::luma709(c[0], c[1], c[2])
}

/// The `q` quantile (0…1) of `v`, by partial sort; 0 for an empty slice.
fn quantile(v: &mut [f32], q: f32) -> f32 {
    if v.is_empty() {
        return 0.0;
    }
    let i = ((v.len() - 1) as f32 * q.clamp(0.0, 1.0)).round() as usize;
    let i = i.min(v.len() - 1);
    let (_, x, _) = v.select_nth_unstable_by(i, f32::total_cmp);
    *x
}

fn sat_after(c: [f32; 3], g: [f32; 3]) -> f32 {
    let b = [c[0] * g[0], c[1] * g[1], c[2] * g[2]];
    let (mx, mn) = (b[0].max(b[1]).max(b[2]), b[0].min(b[1]).min(b[2]));
    if mx <= 0.0 { 0.0 } else { (mx - mn) / mx }
}

fn gains_of(mean: [f32; 3]) -> [f32; 3] {
    let g = mean[1].max(1e-6);
    [(g / mean[0].max(1e-6)).clamp(0.25, 4.0), 1.0, (g / mean[2].max(1e-6)).clamp(0.25, 4.0)]
}

struct Wb {
    mean: [f32; 3],
    bright: Option<[f32; 3]>,
    gains: [f32; 3],
    usable: usize,
    /// Fraction of the pixels' weight that counted.
    coverage: f32,
    /// Disagreement of the two estimates' red/blue ratios, in stops.
    disagreement: f32,
}

/// Robust grey world (see the module doc), `None` when nothing usable was found.
fn white_balance(px: &[[f32; 3]]) -> Option<Wb> {
    let candidate = |c: &[f32; 3]| {
        let mx = c[0].max(c[1]).max(c[2]);
        (BLACK_LEVEL..CLIP_LEVEL).contains(&mx)
    };
    // pass 1: plain grey world over the candidates (only wildly saturated pixels left out)
    let mut acc = [0f64; 3];
    let mut n = 0usize;
    for c in px.iter().filter(|c| candidate(c)) {
        if sat_after(*c, [1.0; 3]) <= 0.85 {
            for k in 0..3 {
                acc[k] += c[k] as f64;
            }
            n += 1;
        }
    }
    if n == 0 {
        return None;
    }
    let g1 = gains_of(acc.map(|v| (v / n as f64) as f32));
    // pass 2: weight by neutrality after the provisional balance
    let mut acc = [0f64; 3];
    let mut wsum = 0f64;
    let mut usable = 0usize;
    let mut lumas: Vec<f32> = Vec::new();
    for c in px.iter().filter(|c| candidate(c)) {
        let w = (1.0 - sat_after(*c, g1) / MAX_NEUTRAL_SATURATION).max(0.0);
        if w <= 0.0 {
            continue;
        }
        for k in 0..3 {
            acc[k] += (c[k] * w) as f64;
        }
        wsum += w as f64;
        usable += 1;
        lumas.push(luma([c[0] * g1[0], c[1] * g1[1], c[2] * g1[2]]));
    }
    if usable == 0 || wsum <= 0.0 {
        return None;
    }
    let mean = acc.map(|v| (v / wsum) as f32);
    // the bright unclipped surfaces, as a second opinion on the illuminant
    let (lo, hi) = (quantile(&mut lumas, 0.85), quantile(&mut lumas, 0.99));
    let mut bacc = [0f64; 3];
    let mut bn = 0usize;
    for c in px.iter().filter(|c| candidate(c)) {
        if sat_after(*c, g1) / MAX_NEUTRAL_SATURATION >= 1.0 {
            continue;
        }
        let l = luma([c[0] * g1[0], c[1] * g1[1], c[2] * g1[2]]);
        if l >= lo && l <= hi {
            for k in 0..3 {
                bacc[k] += c[k] as f64;
            }
            bn += 1;
        }
    }
    let bright = (bn >= 50).then(|| bacc.map(|v| (v / bn as f64) as f32));
    let (mut gains, mut disagreement) = (gains_of(mean), 0.0);
    if let Some(b) = bright {
        let gb = gains_of(b);
        let ratio = |g: [f32; 3]| (g[0] / g[2]).max(1e-6).log2();
        disagreement = (ratio(gains) - ratio(gb)).abs();
        gains = [0, 1, 2].map(|k| (gains[k].ln() * (1.0 - BRIGHT_WEIGHT) + gb[k].ln() * BRIGHT_WEIGHT).exp());
    }
    Some(Wb { mean, bright, gains, usable, coverage: (wsum / px.len().max(1) as f64) as f32, disagreement })
}

/// Lumetri Temperature and Tint (slider units) whose gains are proportional to `gains`
/// (red, green, blue; the overall scale is left to Exposure). The second value says whether a
/// slider end was hit.
pub fn solve_white_balance(gains: [f32; 3]) -> ((f32, f32), bool) {
    let (dr, db) = (gains[0].max(1e-6), gains[2].max(1e-6));
    // (1 + 0.35 t) / (1 − 0.35 t) = dr / db
    let rho = dr / db;
    let t = (rho - 1.0) / (0.35 * (rho + 1.0));
    // the green gain against the mean of red and blue (which is 1 in Lumetri's model)
    let n = (1.0 - 2.0 / (dr + db)) / 0.3;
    let (ts, ns) = (t * SLIDER_RANGE, n * SLIDER_RANGE);
    let clamp = |v: f32| if v.is_finite() { v.clamp(-SLIDER_RANGE, SLIDER_RANGE) } else { 0.0 };
    let (tc, nc) = (clamp(ts), clamp(ns));
    ((tc, nc), (tc - ts).abs() > 1e-3 || (nc - ns).abs() > 1e-3)
}

/// Measure `frames` and solve the correction (see the module doc). Never fails: frames without
/// usable pixels give zero confidence and neutral values.
pub fn analyse(frames: &[&Image], opts: Options) -> Analysis {
    let opts = opts.sanitized();
    let px = samples(frames);
    let mut a = Analysis { pixels: px.len(), ..Analysis::empty() };
    if px.is_empty() {
        return a;
    }
    let mut gains = [1.0f32; 3];
    if opts.white_balance
        && let Some(wb) = white_balance(&px)
    {
        let ((t, n), clamped) = solve_white_balance(wb.gains);
        a.usable = wb.usable;
        a.mean = wb.mean;
        a.bright = wb.bright;
        a.gains = wb.gains;
        a.cast = (wb.gains[2] / wb.gains[0].max(1e-6)).clamp(1.0 / 16.0, 16.0);
        a.temperature = (t * 10.0).round() / 10.0;
        a.tint = (n * 10.0).round() / 10.0;
        a.clamped |= clamped;
        let coverage = (wb.coverage / 0.25).min(1.0);
        let count = (wb.usable as f32 / 1000.0).min(1.0);
        let agreement = (1.0 - wb.disagreement).clamp(0.0, 1.0);
        a.white_balance_confidence = coverage * count * agreement * if clamped { 0.5 } else { 1.0 };
        gains = white_balance_gains(a.temperature / SLIDER_RANGE, a.tint / SLIDER_RANGE);
    }
    // luma after the balance, every opaque pixel (clipped ones included: they are what the
    // highlight cap looks at)
    let mut lumas: Vec<f32> = px.iter().map(|c| luma([c[0] * gains[0], c[1] * gains[1], c[2] * gains[2]])).collect();
    a.p1 = quantile(&mut lumas, 0.01);
    a.p50 = quantile(&mut lumas, 0.5);
    a.p99 = quantile(&mut lumas, 0.99);
    if opts.exposure {
        let mut stops = if a.p50 > 1e-6 { (opts.target_grey / a.p50).log2() } else { 0.0 };
        if a.p99 > 1e-6 {
            stops = stops.min((opts.highlight_ceiling / a.p99).log2());
        }
        let clamped = if stops.is_finite() { stops.clamp(-EXPOSURE_RANGE, EXPOSURE_RANGE) } else { 0.0 };
        let hit = (clamped - stops).abs() > 1e-3;
        a.clamped |= hit;
        a.exposure = (clamped * 100.0).round() / 100.0;
        let signal = (a.p50 / 0.002).min(1.0);
        let range = ((a.p99 - a.p1) / 0.02).clamp(0.0, 1.0);
        let count = (px.len() as f32 / 1000.0).min(1.0);
        a.exposure_confidence = signal * range * count * if hit { 0.5 } else { 1.0 };
    }
    a.confidence = match (opts.white_balance, opts.exposure) {
        (true, true) => a.white_balance_confidence.min(a.exposure_confidence),
        (true, false) => a.white_balance_confidence,
        (false, true) => a.exposure_confidence,
        (false, false) => 0.0,
    };
    for v in [&mut a.confidence, &mut a.white_balance_confidence, &mut a.exposure_confidence] {
        *v = if v.is_finite() { (*v * 100.0).round() / 100.0 } else { 0.0 };
    }
    a
}

/// The colour temperature a RAW developer should assume so that a picture developed at
/// `current_kelvin` with the measured `cast` (red / blue, see [`Analysis::cast`]) comes out
/// neutral. A simple mired model: one stop of red/blue ratio ≈ [`MIRED_PER_STOP`] mired (tungsten
/// 3200 K against daylight 5600 K is about 134 mired and a ratio of about 2.1). Bounded to
/// 2000…50 000 K.
pub fn kelvin_for_cast(cast: f32, current_kelvin: f32) -> f32 {
    let k0 = if current_kelvin.is_finite() { current_kelvin.clamp(2000.0, 50_000.0) } else { 5600.0 };
    let cast = if cast.is_finite() { cast.clamp(1.0 / 16.0, 16.0) } else { 1.0 };
    let mired = 1.0e6 / k0 + MIRED_PER_STOP * cast.log2();
    (1.0e6 / mired.max(20.0)).clamp(2000.0, 50_000.0).round()
}

/// Mired shift per stop of red/blue ratio in the model of [`kelvin_for_cast`].
pub const MIRED_PER_STOP: f32 = 124.0;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::effects::{FxCtx, apply};
    use filmcraft_project::{ParamValue, find_effect};
    use filmcraft_time::Tick;

    /// A neutral scene: a luma ramp with mild, symmetric colour bands (they average to grey), a
    /// black border and a small clipped highlight.
    fn scene() -> Image {
        let (w, h) = (160, 90);
        let mut img = Image::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let t = 0.03 + 0.6 * (x as f32 / w as f32);
                let mut c = [t * t, t * t, t * t];
                if y < h / 3 {
                    c = [c[0] * 1.2, c[1], c[2] * 0.8];
                } else if y > 2 * h / 3 {
                    c = [c[0] * 0.8, c[1], c[2] * 1.2];
                }
                if x < 4 || y < 2 {
                    c = [0.0; 3];
                }
                if (140..150).contains(&x) && (40..46).contains(&y) {
                    c = [1.0; 3];
                }
                let i = (y * w + x) * 4;
                img.px[i..i + 4].copy_from_slice(&[c[0], c[1], c[2], 1.0]);
            }
        }
        img
    }

    fn with(img: &Image, cast: [f32; 3], stops: f32) -> Image {
        let mut o = img.clone();
        let k = 2f32.powf(stops);
        o.map_rgb(|c, _, _| [(c[0] * cast[0] * k).min(1.0), (c[1] * cast[1] * k).min(1.0), (c[2] * cast[2] * k).min(1.0)]);
        o
    }

    fn corrected(img: &Image, a: &Analysis) -> Image {
        let mut e = find_effect("lumetri").unwrap().instance();
        for (k, v) in [("temperature", a.temperature), ("tint", a.tint), ("exposure", a.exposure)] {
            e.params.get_mut(k).unwrap().value = ParamValue::Float(v as f64);
        }
        let cx = FxCtx {
            t: Tick::ZERO,
            px_scale: 0.1,
            seconds: 0.0,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            frame_rate: 24.0,
            working: filmcraft_color::WorkingSpace::Rec709,
        };
        let mut g = img.clone();
        apply(&mut g, &e, &cx);
        g
    }

    fn neutral_mean(img: &Image) -> [f32; 3] {
        let px = samples(&[img]);
        let mut m = [0f64; 3];
        let mut n = 0;
        for c in px.iter().filter(|c| c.iter().all(|v| *v > 0.02 && *v < 0.9)) {
            for k in 0..3 {
                m[k] += c[k] as f64;
            }
            n += 1;
        }
        m.map(|v| (v / n.max(1) as f64) as f32)
    }

    #[test]
    fn a_warm_underexposed_scene_comes_out_neutral_and_mid_grey() {
        let src = with(&scene(), [1.35, 1.0, 0.7], -1.5);
        let a = analyse(&[&src], Options::default());
        assert!(a.temperature < -20.0, "{a:?}");
        assert!(a.exposure > 0.8 && a.exposure < 2.5, "{a:?}");
        assert!(a.confidence > 0.5, "{a:?}");
        let out = corrected(&src, &a);
        let m = neutral_mean(&out);
        assert!((m[0] / m[1] - 1.0).abs() < 0.04 && (m[2] / m[1] - 1.0).abs() < 0.04, "cast left: {m:?} ({a:?})");
        let mut l: Vec<f32> = samples(&[&out]).iter().map(|c| luma(*c)).collect();
        let p50 = quantile(&mut l, 0.5);
        let p99 = quantile(&mut l, 0.99);
        assert!((p50 - 0.18).abs() < 0.03 || p99 > 0.85, "p50 {p50} p99 {p99} ({a:?})");
        assert!(p99 <= 0.95, "highlights clipped: p99 {p99}");
    }

    #[test]
    fn a_cool_green_overexposed_scene_is_pulled_down() {
        let src = with(&scene(), [0.8, 1.15, 1.3], 1.2);
        let a = analyse(&[&src], Options::default());
        assert!(a.temperature > 15.0 && a.tint > 5.0, "{a:?}");
        assert!(a.exposure < -0.3, "{a:?}");
        let out = corrected(&src, &a);
        let m = neutral_mean(&out);
        assert!((m[0] / m[1] - 1.0).abs() < 0.05 && (m[2] / m[1] - 1.0).abs() < 0.05, "cast left: {m:?} ({a:?})");
        let mut l: Vec<f32> = samples(&[&out]).iter().map(|c| luma(*c)).collect();
        assert!(quantile(&mut l, 0.99) <= 0.92, "{a:?}");
    }

    #[test]
    fn a_neutral_scene_is_left_nearly_alone() {
        let src = scene();
        let a = analyse(&[&src], Options::default());
        assert!(a.temperature.abs() < 6.0 && a.tint.abs() < 6.0, "{a:?}");
        let b = analyse(&[&src], Options { white_balance: false, ..Options::default() });
        assert_eq!((b.temperature, b.tint), (0.0, 0.0));
        assert_eq!(b.white_balance_confidence, 0.0);
        let c = analyse(&[&src], Options { exposure: false, ..Options::default() });
        assert_eq!(c.exposure, 0.0);
    }

    #[test]
    fn sampled_frames_pool_their_pixels() {
        let warm = with(&scene(), [1.3, 1.0, 0.75], 0.0);
        let one = analyse(&[&warm], Options::default());
        let three = analyse(&[&warm, &warm, &warm], Options::default());
        assert!((one.temperature - three.temperature).abs() < 1.0, "{one:?} vs {three:?}");
        assert_eq!(three.pixels, 3 * one.pixels);
    }

    #[test]
    fn empty_transparent_and_nan_frames_do_not_panic() {
        let empty = Image::new(0, 0);
        let a = analyse(&[&empty], Options::default());
        assert_eq!((a.pixels, a.confidence, a.temperature, a.exposure), (0, 0.0, 0.0, 0.0));
        assert_eq!(analyse(&[], Options::default()).pixels, 0);
        let clear = Image::new(8, 8);
        assert_eq!(analyse(&[&clear], Options::default()).pixels, 0);
        let mut nan = Image::filled(8, 8, [f32::NAN, 0.5, f32::INFINITY, 1.0]);
        nan.px[7] = f32::NAN; // alpha
        let a = analyse(&[&nan], Options::default());
        assert_eq!(a.pixels, 0);
        assert!(a.confidence == 0.0 && a.exposure.is_finite());
        // a flat black frame: nothing to balance, exposure left alone
        let black = Image::filled(16, 16, [0.0, 0.0, 0.0, 1.0]);
        let a = analyse(&[&black], Options::default());
        assert_eq!((a.usable, a.exposure, a.confidence), (0, 0.0, 0.0));
        // a flat white (clipped) frame: pulled down, never up
        let white = Image::filled(16, 16, [1.0, 1.0, 1.0, 1.0]);
        let a = analyse(&[&white], Options::default());
        assert!(a.exposure < 0.0 && a.usable == 0, "{a:?}");
        // hostile options
        let o = Options { target_grey: f32::NAN, highlight_ceiling: -3.0, ..Options::default() }.sanitized();
        assert_eq!((o.target_grey, o.highlight_ceiling), (0.18, 0.18));
    }

    #[test]
    fn large_frames_are_strided_to_the_sample_cap() {
        let big = Image::filled(2048, 1024, [0.2, 0.2, 0.2, 1.0]);
        let px = samples(&[&big]);
        assert!(px.len() <= MAX_SAMPLES && px.len() >= MAX_SAMPLES / 2, "{}", px.len());
    }

    #[test]
    fn white_balance_solve_inverts_the_gain_model() {
        for (t, n) in [(0.0, 0.0), (40.0, -20.0), (-70.0, 55.0), (100.0, 100.0)] {
            let g = white_balance_gains(t / 100.0, n / 100.0);
            // the desired gains undo g, up to scale
            let want = [g[1] / g[0], 1.0, g[1] / g[2]];
            let ((ts, ns), clamped) = solve_white_balance(want);
            assert!(!clamped, "{t} {n}");
            // the red/blue ratio inverts exactly; the solved gains are proportional to `want`
            assert!((ts + t).abs() < 0.2, "({t}, {n}) → ({ts}, {ns})");
            let got = white_balance_gains(ts / 100.0, ns / 100.0);
            let k = got[1] / want[1];
            assert!((0..3).all(|i| (got[i] / want[i] / k - 1.0).abs() < 0.01), "({t}, {n}) → {got:?} vs {want:?}");
        }
        let ((t, _), clamped) = solve_white_balance([4.0, 1.0, 0.25]);
        assert!(clamped && t == 100.0, "{t}");
        let ((t, n), _) = solve_white_balance([f32::NAN, 1.0, f32::INFINITY]);
        assert!(t.is_finite() && n.is_finite());
    }

    #[test]
    fn kelvin_model_is_monotonic_and_bounded() {
        assert_eq!(kelvin_for_cast(1.0, 5600.0), 5600.0);
        let warm = kelvin_for_cast(2.1, 5600.0);
        assert!((3100.0..3400.0).contains(&warm), "{warm}");
        let cool = kelvin_for_cast(0.5, 5600.0);
        assert!(cool > 5600.0 && cool < 50_000.0, "{cool}");
        assert_eq!(kelvin_for_cast(f32::NAN, f32::INFINITY), 5600.0);
        assert_eq!(kelvin_for_cast(1e9, 2000.0), 2000.0);
        assert_eq!(kelvin_for_cast(1e-9, 50_000.0), 50_000.0);
    }
}

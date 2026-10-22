//! Stable 2-D similarity-path smoothing for Warp Stabilizer.
//!
//! The analysed path is represented as a cumulative camera transform.  We decompose that
//! transform about the analysis-frame centre, unwrap its rotation, and solve one banded
//! least-squares problem for each of the four similarity coordinates.  Keeping the four right
//! hand sides together makes the solve linear in the number of frames without ever materialising
//! a dense matrix.

use std::f64::consts::{PI, TAU};

use filmcraft_geom::Affine;

use super::StabilizerPath;

// Analysis paths are normally a few hundred or a few thousand frames and transforms are close to
// identity.  These bounds are only for malformed input: they keep products, exp and inverse
// arithmetic finite while leaving several orders of magnitude above ordinary pixel coordinates.
const AFFINE_LIMIT: f64 = 1.0e12;
const MAX_SCALE: f64 = 5.0e11;
const MIN_SCALE: f64 = 1.0e-9;
const LOG_SCALE_LIMIT: f64 = 27.0;
const ANGLE_LIMIT: f64 = 1.0e6;
const PARAM_LIMIT: f64 = AFFINE_LIMIT;
const PIVOT_MIN: f64 = 1.0e-14;

/// Raw cumulative transforms, with element zero representing frame 0.
///
/// Composition follows [`Affine::then_apply`]: step `k` is composed after the cumulative
/// transform through frame `k`, so the result is the raw frame-0 → frame-`k` camera path.
pub(super) fn camera_path(path: &StabilizerPath) -> Vec<Affine> {
    let n = path.steps.len().saturating_add(1);
    let mut cameras = Vec::with_capacity(n);
    cameras.push(Affine::IDENTITY);
    for k in 1..n {
        // `k - 1` is in bounds because n is steps.len() + 1 for every realizable Vec.
        let step = sanitise_affine(path.steps[k - 1]);
        let previous = cameras[k - 1];
        cameras.push(compose_affine(step, previous));
    }
    cameras
}

/// Per-frame correction transforms (current frame px → stabilised px, analysis pixels).
///
/// `smooth_frames` is the characteristic frame wavelength.  We use
/// `lambda = 0.216 * smooth_frames^4`; for the continuous approximation this puts the fourth-order
/// low-pass response's -3 dB point at roughly `1.18 / smooth_frames` radians/frame.  Unlike a
/// truncated Gaussian, the corresponding second-difference objective has an exact affine null
/// space, so constant-velocity pans are unchanged, including at the endpoints.
pub(super) fn corrections(path: &StabilizerPath, cameras: &[Affine], smooth_frames: f64, no_motion: bool, preserve_scale: bool) -> Vec<Affine> {
    let n = path.steps.len().saturating_add(1);
    let mut result = vec![Affine::IDENTITY; n];
    if n == 0 || (!no_motion && (!smooth_frames.is_finite() || smooth_frames <= 0.0)) {
        // Smooth Motion treats zero/nonfinite smoothness as the identity path, not an
        // almost-zero blur.  No Motion intentionally bypasses smoothness entirely and still
        // computes its fixed mean camera.
        return result;
    }

    let (to_centre, from_centre) = centre_transforms(path);
    let mut raw_params = camera_parameters(cameras, &to_centre, &from_centre);
    if raw_params.is_empty() {
        return result;
    }

    // A finite, bounded path is an invariant of the rest of this module.  This also protects the
    // cumulative angle from growing without bound on a hostile, very long path.
    sanitise_parameters(&mut raw_params);

    let mut target = if no_motion {
        let mean = parameter_mean(&raw_params);
        vec![mean; raw_params.len()]
    } else {
        smooth_parameters(&raw_params, smooth_frames)
    };

    // Preserve Scale means preserve the analysed scale coordinate frame-by-frame while the other
    // coordinates follow the requested smoothing/no-motion mode.
    if preserve_scale {
        for (smoothed, raw) in target.iter_mut().zip(raw_params.iter()) {
            smoothed[3] = raw[3];
        }
    }
    sanitise_parameters(&mut target);

    for k in 0..n {
        let Some(raw) = raw_params.get(k).copied() else { break };
        let Some(smoothed) = target.get(k).copied() else { break };

        // This fast path gives an exactly identity correction for an exactly affine camera path,
        // instead of retaining the round-off from multiplying a transform by its inverse.
        if parameters_equal(raw, smoothed) {
            result[k] = Affine::IDENTITY;
            continue;
        }

        let raw_camera = compose_parameters(raw);
        let smoothed_camera = compose_parameters(smoothed);
        let Some(raw_inverse) = inverse_affine(raw_camera) else {
            result[k] = Affine::IDENTITY;
            continue;
        };
        let centred_correction = compose_affine(smoothed_camera, raw_inverse);
        let correction = compose_affine(from_centre, compose_affine(centred_correction, to_centre));
        result[k] = if affine_is_finite(correction) { correction } else { Affine::IDENTITY };
    }

    result
}

fn centre_transforms(path: &StabilizerPath) -> (Affine, Affine) {
    let cx = bounded(path.w as f64 * 0.5, 0.0);
    let cy = bounded(path.h as f64 * 0.5, 0.0);
    (Affine::translate(-cx, -cy), Affine::translate(cx, cy))
}

/// Decompose cumulative cameras in coordinates centred on the analysis frame and unwrap angle.
fn camera_parameters(cameras: &[Affine], to_centre: &Affine, from_centre: &Affine) -> Vec<[f64; 4]> {
    let mut params = Vec::with_capacity(cameras.len());
    let mut previous_wrapped = 0.0;
    let mut unwrapped = 0.0;
    for (k, camera) in cameras.iter().copied().enumerate() {
        let centred = compose_affine(*to_centre, compose_affine(camera, *from_centre));
        let mut p = decompose_parameters(centred);
        let wrapped = p[2];
        if k == 0 {
            unwrapped = wrapped;
        } else {
            let delta = wrap_angle(wrapped - previous_wrapped);
            unwrapped = bounded(unwrapped + delta, unwrapped);
        }
        previous_wrapped = wrapped;
        p[2] = unwrapped;
        params.push(p);
    }
    params
}

fn decompose_parameters(m: Affine) -> [f64; 4] {
    let m = sanitise_affine(m);
    let scale = m.a.hypot(m.b);
    let scale = if scale.is_finite() && scale > 0.0 { scale.clamp(MIN_SCALE, MAX_SCALE) } else { 1.0 };
    let angle = if scale > 0.0 {
        let angle = m.b.atan2(m.a);
        if angle.is_finite() { angle } else { 0.0 }
    } else {
        0.0
    };
    [bounded(m.e, 0.0), bounded(m.f, 0.0), bounded(angle, 0.0), bounded(scale.ln(), 0.0).clamp(-LOG_SCALE_LIMIT, LOG_SCALE_LIMIT)]
}

fn compose_parameters(p: [f64; 4]) -> Affine {
    let tx = bounded(p[0], 0.0);
    let ty = bounded(p[1], 0.0);
    let angle = bounded(p[2], 0.0).clamp(-ANGLE_LIMIT, ANGLE_LIMIT);
    let log_scale = bounded(p[3], 0.0).clamp(-LOG_SCALE_LIMIT, LOG_SCALE_LIMIT);
    let scale = log_scale.exp().clamp(MIN_SCALE, MAX_SCALE);
    let (sin, cos) = angle.sin_cos();
    sanitise_affine(Affine { a: scale * cos, b: scale * sin, c: -scale * sin, d: scale * cos, e: tx, f: ty })
}

fn smooth_parameters(raw: &[[f64; 4]], smooth_frames: f64) -> Vec<[f64; 4]> {
    let n = raw.len();
    if n <= 2 {
        return raw.to_vec();
    }
    let scale = smooth_frames.min(512.0);
    let lambda = (0.216 * scale * scale * scale * scale).min(1.0e8);
    if !lambda.is_finite() || lambda <= 0.0 {
        return raw.to_vec();
    }

    let (main, first, second) = second_difference_bands(n, lambda);
    let Some(factor) = factor_bands(&main, &first, &second) else {
        return raw.to_vec();
    };

    // One row contains all four coordinates: the same factorisation solves four RHS in one O(N)
    // forward/diagonal/backward pass.
    let mut solved = raw.to_vec();
    if !solve_bands(&factor, &mut solved) {
        return raw.to_vec();
    }

    // The null space of the second-difference penalty is affine-in-time.  The solve already
    // preserves it mathematically; assigning these channels back also removes endpoint round-off
    // and makes the invariant observable to consumers using exact comparisons.
    for coordinate in 0..4 {
        if is_linear_coordinate(raw, coordinate) {
            for k in 0..n {
                solved[k][coordinate] = raw[k][coordinate];
            }
        }
    }
    sanitise_parameters(&mut solved);
    solved
}

/// Build `I + lambda D^T D`, where each row of D is `[1, -2, 1]`.
///
/// `first[i]` is the sub-diagonal at `(i, i - 1)` and `second[i]` the sub-sub-diagonal at
/// `(i, i - 2)`.  The arrays therefore store only the two non-zero bands.
fn second_difference_bands(n: usize, lambda: f64) -> (Vec<f64>, Vec<f64>, Vec<f64>) {
    let mut main = vec![1.0; n];
    let mut first = vec![0.0; n];
    let mut second = vec![0.0; n];
    for row in 0..n.saturating_sub(2) {
        let next = row + 1;
        let last = row + 2;
        main[row] += lambda;
        main[next] += 4.0 * lambda;
        main[last] += lambda;
        first[next] += -2.0 * lambda;
        first[last] += -2.0 * lambda;
        second[last] += lambda;
    }
    (main, first, second)
}

struct BandedFactor {
    diagonal: Vec<f64>,
    first: Vec<f64>,
    second: Vec<f64>,
}

/// LDLᵀ factorisation of a symmetric pentadiagonal matrix in O(N) storage and time.
fn factor_bands(main: &[f64], first: &[f64], second: &[f64]) -> Option<BandedFactor> {
    let n = main.len();
    if first.len() != n || second.len() != n {
        return None;
    }
    let mut diagonal = vec![0.0_f64; n];
    let mut l_first = vec![0.0; n];
    let mut l_second = vec![0.0; n];

    for i in 0..n {
        if i >= 2 {
            let d = diagonal[i - 2];
            if !d.is_finite() || d <= PIVOT_MIN {
                return None;
            }
            l_second[i] = second[i] / d;
        }
        if i >= 1 {
            let d = diagonal[i - 1];
            if !d.is_finite() || d <= PIVOT_MIN {
                return None;
            }
            let coupling = if i >= 2 { l_second[i] * l_first[i - 1] * diagonal[i - 2] } else { 0.0 };
            l_first[i] = (first[i] - coupling) / d;
        }
        let mut pivot = main[i];
        if i >= 1 {
            pivot -= l_first[i] * l_first[i] * diagonal[i - 1];
        }
        if i >= 2 {
            pivot -= l_second[i] * l_second[i] * diagonal[i - 2];
        }
        if !pivot.is_finite() || pivot <= PIVOT_MIN {
            return None;
        }
        diagonal[i] = pivot;
    }

    Some(BandedFactor { diagonal, first: l_first, second: l_second })
}

fn solve_bands(factor: &BandedFactor, rhs: &mut [[f64; 4]]) -> bool {
    let n = rhs.len();
    if factor.diagonal.len() != n || factor.first.len() != n || factor.second.len() != n {
        return false;
    }

    for i in 0..n {
        for coordinate in 0..4 {
            let mut value = rhs[i][coordinate];
            if i >= 1 {
                value -= factor.first[i] * rhs[i - 1][coordinate];
            }
            if i >= 2 {
                value -= factor.second[i] * rhs[i - 2][coordinate];
            }
            if !value.is_finite() {
                return false;
            }
            rhs[i][coordinate] = value;
        }
    }
    for i in 0..n {
        let diagonal = factor.diagonal[i];
        if !diagonal.is_finite() || diagonal <= PIVOT_MIN {
            return false;
        }
        for coordinate in 0..4 {
            let value = rhs[i][coordinate] / diagonal;
            if !value.is_finite() {
                return false;
            }
            rhs[i][coordinate] = value;
        }
    }
    for i in (0..n).rev() {
        for coordinate in 0..4 {
            let mut value = rhs[i][coordinate];
            if i + 1 < n {
                value -= factor.first[i + 1] * rhs[i + 1][coordinate];
            }
            if i + 2 < n {
                value -= factor.second[i + 2] * rhs[i + 2][coordinate];
            }
            if !value.is_finite() {
                return false;
            }
            rhs[i][coordinate] = bounded(value, 0.0);
        }
    }
    true
}

fn parameter_mean(params: &[[f64; 4]]) -> [f64; 4] {
    let mut mean = [0.0; 4];
    for (index, p) in params.iter().enumerate() {
        let count = (index + 1) as f64;
        for coordinate in 0..4 {
            let value = bounded(p[coordinate], 0.0);
            mean[coordinate] = bounded(mean[coordinate] + (value - mean[coordinate]) / count, mean[coordinate]);
        }
    }
    mean
}

fn is_linear_coordinate(values: &[[f64; 4]], coordinate: usize) -> bool {
    if values.len() <= 2 {
        return true;
    }
    let mut previous_delta = values[1][coordinate] - values[0][coordinate];
    if !previous_delta.is_finite() {
        return false;
    }
    for k in 2..values.len() {
        let delta = values[k][coordinate] - values[k - 1][coordinate];
        if !delta.is_finite() {
            return false;
        }
        let tolerance = 1.0e-11 * (1.0 + previous_delta.abs() + delta.abs() + values[k][coordinate].abs());
        if (delta - previous_delta).abs() > tolerance {
            return false;
        }
        previous_delta = delta;
    }
    true
}

fn sanitise_parameters(params: &mut [[f64; 4]]) {
    for p in params {
        p[0] = bounded(p[0], 0.0);
        p[1] = bounded(p[1], 0.0);
        p[2] = bounded(p[2], 0.0).clamp(-ANGLE_LIMIT, ANGLE_LIMIT);
        p[3] = bounded(p[3], 0.0).clamp(-LOG_SCALE_LIMIT, LOG_SCALE_LIMIT);
    }
}

fn parameters_equal(a: [f64; 4], b: [f64; 4]) -> bool {
    (0..4).all(|coordinate| a[coordinate].to_bits() == b[coordinate].to_bits())
}

fn wrap_angle(angle: f64) -> f64 {
    if !angle.is_finite() {
        return 0.0;
    }
    let wrapped = (angle + PI).rem_euclid(TAU) - PI;
    if wrapped.is_finite() { wrapped } else { 0.0 }
}

fn clamp_finite(value: f64, fallback: f64, limit: f64) -> f64 {
    if value.is_finite() { value.clamp(-limit, limit) } else { fallback.clamp(-limit, limit) }
}

fn bounded(value: f64, fallback: f64) -> f64 {
    clamp_finite(value, fallback, PARAM_LIMIT)
}

fn sanitise_component(value: f64, fallback: f64) -> f64 {
    clamp_finite(value, fallback, AFFINE_LIMIT)
}

fn sanitise_affine(m: Affine) -> Affine {
    Affine {
        a: sanitise_component(m.a, 1.0),
        b: sanitise_component(m.b, 0.0),
        c: sanitise_component(m.c, 0.0),
        d: sanitise_component(m.d, 1.0),
        e: sanitise_component(m.e, 0.0),
        f: sanitise_component(m.f, 0.0),
    }
}

fn compose_affine(lhs: Affine, rhs: Affine) -> Affine {
    let lhs = sanitise_affine(lhs);
    let rhs = sanitise_affine(rhs);
    sanitise_affine(Affine {
        a: lhs.a * rhs.a + lhs.c * rhs.b,
        b: lhs.b * rhs.a + lhs.d * rhs.b,
        c: lhs.a * rhs.c + lhs.c * rhs.d,
        d: lhs.b * rhs.c + lhs.d * rhs.d,
        e: lhs.a * rhs.e + lhs.c * rhs.f + lhs.e,
        f: lhs.b * rhs.e + lhs.d * rhs.f + lhs.f,
    })
}

fn inverse_affine(m: Affine) -> Option<Affine> {
    let m = sanitise_affine(m);
    let determinant = m.a * m.d - m.b * m.c;
    if !determinant.is_finite() || determinant.abs() < 1.0e-24 {
        return None;
    }
    let inverse = 1.0 / determinant;
    let a = m.d * inverse;
    let b = -m.b * inverse;
    let c = -m.c * inverse;
    let d = m.a * inverse;
    let e = -(a * m.e + c * m.f);
    let f = -(b * m.e + d * m.f);
    let result = sanitise_affine(Affine { a, b, c, d, e, f });
    if affine_is_finite(result) { Some(result) } else { None }
}

fn affine_is_finite(m: Affine) -> bool {
    [m.a, m.b, m.c, m.d, m.e, m.f].into_iter().all(f64::is_finite)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_geom::Vec2;

    fn corrections(path: &StabilizerPath, smooth: f64, fixed: bool, preserve: bool) -> Vec<Affine> {
        super::corrections(path, &camera_path(path), smooth, fixed, preserve)
    }

    fn path(steps: Vec<Affine>) -> StabilizerPath {
        StabilizerPath { w: 160, h: 90, steps, fps: 30.0 }
    }

    fn centred_params(path: &StabilizerPath) -> Vec<[f64; 4]> {
        let cameras = camera_path(path);
        let (to_centre, from_centre) = centre_transforms(path);
        camera_parameters(&cameras, &to_centre, &from_centre)
    }

    #[test]
    fn linear_pan_is_not_bent_at_endpoints() {
        let steps = (0..7).map(|_| Affine::translate(2.5, -1.25)).collect();
        let p = path(steps);
        let corrections = corrections(&p, 12.0, false, false);
        assert!(corrections.iter().all(Affine::is_identity));
    }

    #[test]
    fn no_motion_ignores_zero_smoothness() {
        let p = path(vec![Affine::translate(2.0, 0.0), Affine::translate(4.0, 0.0), Affine::translate(6.0, 0.0)]);
        let cameras = camera_path(&p);
        let corrections = corrections(&p, 0.0, true, false);
        assert!(corrections.iter().any(|correction| !correction.is_identity()));
        let fixed = corrections[0].then_apply(&cameras[0]);
        for (camera, correction) in cameras.iter().zip(corrections.iter()) {
            let got = correction.then_apply(camera);
            assert!((got.e - fixed.e).abs() < 1.0e-9);
            assert!((got.f - fixed.f).abs() < 1.0e-9);
            assert!((got.a - fixed.a).abs() < 1.0e-9);
            assert!((got.d - fixed.d).abs() < 1.0e-9);
        }
    }

    #[test]
    fn jitter_is_attenuated_by_second_difference_solve() {
        let steps = vec![
            Affine::translate(2.0, 0.0),
            Affine::translate(-3.0, 0.0),
            Affine::translate(4.0, 0.0),
            Affine::translate(-3.0, 0.0),
            Affine::translate(2.0, 0.0),
        ];
        let p = path(steps);
        let raw = centred_params(&p);
        let smoothed = smooth_parameters(&raw, 5.0);
        let raw_span = raw.iter().map(|v| v[0]).fold(f64::NEG_INFINITY, f64::max) - raw.iter().map(|v| v[0]).fold(f64::INFINITY, f64::min);
        let smooth_span = smoothed.iter().map(|v| v[0]).fold(f64::NEG_INFINITY, f64::max) - smoothed.iter().map(|v| v[0]).fold(f64::INFINITY, f64::min);
        assert!(smooth_span < raw_span);
    }

    #[test]
    fn angle_unwrap_crosses_pi_without_a_turnaround() {
        let p = path(vec![Affine::rotate_deg(179.0), Affine::rotate_deg(4.0), Affine::rotate_deg(4.0)]);
        let params = centred_params(&p);
        assert!(params[1][2] > params[0][2]);
        assert!(params[2][2] > params[1][2]);
        assert!((params[2][2] - params[1][2]).abs() < PI / 12.0);
    }

    #[test]
    fn camera_composition_matches_then_apply_convention() {
        let first = Affine::translate(3.0, 4.0);
        let second = Affine::rotate_deg(90.0);
        let p = path(vec![first, second]);
        let cameras = camera_path(&p);
        let point = Vec2::new(11.0, -2.0);
        let got = cameras[2].apply(point);
        assert!((got.x + 2.0).abs() < 1.0e-10);
        assert!((got.y - 14.0).abs() < 1.0e-10);
    }

    #[test]
    fn preserve_scale_keeps_each_raw_scale() {
        let p = path(vec![Affine::scale(1.4, 1.4), Affine::scale(0.7, 0.7), Affine::scale(1.3, 1.3)]);
        let cameras = camera_path(&p);
        let raw = centred_params(&p);
        let corrections = corrections(&p, 8.0, false, true);
        for (k, correction) in corrections.iter().enumerate() {
            let expected_scale = raw[k][3].exp();
            let transformed = correction.then_apply(&cameras[k]);
            let scale = transformed.a.hypot(transformed.b);
            assert!((scale - expected_scale).abs() < expected_scale * 1.0e-9 + 1.0e-9);
        }
    }

    #[test]
    fn one_frame_and_hostile_affines_stay_finite_and_sized() {
        let single = path(Vec::new());
        assert_eq!(corrections(&single, 4.0, false, false).len(), 1);
        let hostile = path(vec![
            Affine { a: f64::NAN, b: f64::INFINITY, c: -f64::INFINITY, d: 1.0e308, e: f64::NAN, f: f64::NEG_INFINITY },
            Affine::scale(1.0e-300, 1.0e-300),
            Affine::translate(1.0e308, -1.0e308),
        ]);
        let cameras = camera_path(&hostile);
        let corrections = corrections(&hostile, 20.0, false, false);
        assert_eq!(cameras.len(), hostile.steps.len() + 1);
        assert_eq!(corrections.len(), hostile.steps.len() + 1);
        assert!(cameras.iter().copied().all(affine_is_finite));
        assert!(corrections.iter().copied().all(affine_is_finite));
    }
}

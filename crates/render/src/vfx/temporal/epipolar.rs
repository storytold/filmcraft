//! Fundamental-matrix filtering for persistent feature trajectories.
//!
//! The tracker intentionally does not use a similarity fit to decide whether a
//! point belongs to the camera motion: depth-dependent parallax is a valid
//! observation.  Every fifth-frame correspondence window is instead checked
//! against a normalized eight-point fundamental matrix.  A trajectory is
//! rejected only when more than one third of its available checks violate the
//! model.

use super::trajectory_data::FeatureTrajectory;
use super::trajectory_index::TrajectoryIndex;

const CHECK_STRIDE: usize = 5;
const MIN_POINTS: usize = 8;
const RANSAC_ITERS: usize = 128;
const EPSILON: f64 = 1.0e-12;

enum RansacResult {
    Cancelled,
    NoModel,
    Model([[f64; 3]; 3]),
}

/// Mark dynamic trajectories without changing their observations.
///
/// Windows with too few points, insufficient spatial spread, a degenerate
/// motion, or no usable eight-point model are skipped.  This is deliberately
/// a late pass: all LK observations remain available to the subspace model and
/// only the `rejected` bit controls later camera/mesh constraints.
pub(super) fn reject_dynamic_tracks(tracks: &mut [FeatureTrajectory], width: usize, height: usize, frames: usize, index: &TrajectoryIndex) -> bool {
    if filmcraft_media::cancel::cancelled() {
        return false;
    }
    if tracks.is_empty() || width == 0 || height == 0 || frames <= CHECK_STRIDE {
        return true;
    }
    let mut checks = vec![0u16; tracks.len()];
    let mut violations = vec![0u16; tracks.len()];
    for end in (CHECK_STRIDE..frames).step_by(CHECK_STRIDE) {
        if filmcraft_media::cancel::cancelled() {
            return false;
        }
        let start = end - CHECK_STRIDE;
        let active = index.at(start);
        let mut ids = Vec::with_capacity(active.len().min(160));
        let mut pairs = Vec::with_capacity(active.len().min(160));
        for &track_id in active {
            if filmcraft_media::cancel::cancelled() {
                return false;
            }
            let index = track_id as usize;
            let Some(track) = tracks.get(index) else { continue };
            if track.rejected {
                continue;
            }
            let Some(source) = point_at(track, start) else { continue };
            let Some(target) = point_at(track, end) else { continue };
            if valid_point(source, width, height) && valid_point(target, width, height) {
                ids.push(index);
                pairs.push(([source[0] as f64, source[1] as f64], [target[0] as f64, target[1] as f64]));
            }
        }
        if pairs.len() < MIN_POINTS {
            continue;
        }
        let model = match ransac(&pairs, width, height) {
            RansacResult::Cancelled => return false,
            RansacResult::NoModel => continue,
            RansacResult::Model(model) => model,
        };
        let threshold = pixel_threshold(width, height);
        for (pair_index, &track_index) in ids.iter().enumerate() {
            if filmcraft_media::cancel::cancelled() {
                return false;
            }
            let Some(pair) = pairs.get(pair_index) else { continue };
            let Some(check) = checks.get_mut(track_index) else { continue };
            *check = check.saturating_add(1);
            let error = sampson_error(&model, pair.0, pair.1).unwrap_or(f64::INFINITY);
            if (!error.is_finite() || error > threshold)
                && let Some(violation) = violations.get_mut(track_index)
            {
                *violation = violation.saturating_add(1);
            }
        }
    }
    if filmcraft_media::cancel::cancelled() {
        return false;
    }
    for (index, track) in tracks.iter_mut().enumerate() {
        if filmcraft_media::cancel::cancelled() {
            return false;
        }
        let Some(&checked) = checks.get(index) else { continue };
        let Some(&bad) = violations.get(index) else { continue };
        if checked > 0 && (bad as u32).saturating_mul(3) > checked as u32 {
            track.rejected = true;
        }
    }
    true
}

fn point_at(track: &FeatureTrajectory, frame: usize) -> Option<[f32; 2]> {
    let offset = frame.checked_sub(track.start)?;
    track.points.get(offset).copied()
}

fn valid_point(point: [f32; 2], width: usize, height: usize) -> bool {
    point[0].is_finite() && point[1].is_finite() && point[0] >= 0.0 && point[1] >= 0.0 && point[0] < width as f32 && point[1] < height as f32
}

fn pixel_threshold(_width: usize, _height: usize) -> f64 {
    // The analysis coordinates are the working image coordinates.  The paper's
    // RANSAC gate is one analysis pixel, independent of source resolution.
    1.0
}

fn ransac(pairs: &[([f64; 2], [f64; 2])], width: usize, height: usize) -> RansacResult {
    if filmcraft_media::cancel::cancelled() {
        return RansacResult::Cancelled;
    }
    if pairs.len() < MIN_POINTS || degenerate_motion(pairs, width, height) {
        return if filmcraft_media::cancel::cancelled() { RansacResult::Cancelled } else { RansacResult::NoModel };
    }
    let threshold = pixel_threshold(width, height);
    let mut rng = Rng(0xD1B5_4A32_9E37_79B9 ^ pairs.len() as u64 ^ ((width as u64) << 17) ^ ((height as u64) << 31));
    let mut best_model = None;
    let mut best_inliers: Vec<usize> = Vec::new();
    for _ in 0..RANSAC_ITERS {
        if filmcraft_media::cancel::cancelled() {
            return RansacResult::Cancelled;
        }
        let sample = sample_indices(pairs.len(), &mut rng);
        let Some(model) = fit_model(pairs, &sample) else {
            if filmcraft_media::cancel::cancelled() {
                return RansacResult::Cancelled;
            }
            continue;
        };
        let Some(inliers) = inliers(&model, pairs, threshold) else {
            return RansacResult::Cancelled;
        };
        if inliers.len() > best_inliers.len() {
            best_inliers = inliers;
            best_model = Some(model);
            if best_inliers.len() == pairs.len() {
                break;
            }
        }
    }
    let Some(mut model) = best_model else {
        return if filmcraft_media::cancel::cancelled() { RansacResult::Cancelled } else { RansacResult::NoModel };
    };
    if best_inliers.len() < MIN_POINTS {
        return RansacResult::NoModel;
    }
    for _ in 0..2 {
        if filmcraft_media::cancel::cancelled() {
            return RansacResult::Cancelled;
        }
        let Some(refined_model) = fit_model(pairs, &best_inliers) else {
            if filmcraft_media::cancel::cancelled() {
                return RansacResult::Cancelled;
            }
            return RansacResult::NoModel;
        };
        model = refined_model;
        let Some(refined) = inliers(&model, pairs, threshold) else {
            return RansacResult::Cancelled;
        };
        if refined.len() <= best_inliers.len() {
            best_inliers = refined;
            break;
        }
        best_inliers = refined;
    }
    if best_inliers.len() < MIN_POINTS {
        return RansacResult::NoModel;
    }
    RansacResult::Model(model)
}

fn degenerate_motion(pairs: &[([f64; 2], [f64; 2])], width: usize, height: usize) -> bool {
    let n = pairs.len() as f64;
    if n < MIN_POINTS as f64 {
        return true;
    }
    let mut dxs = Vec::with_capacity(pairs.len());
    let mut dys = Vec::with_capacity(pairs.len());
    for (source, target) in pairs {
        if filmcraft_media::cancel::cancelled() {
            return true;
        }
        dxs.push(target[0] - source[0]);
        dys.push(target[1] - source[1]);
    }
    dxs.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    dys.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
    let middle = dxs.len() / 2;
    let median = [dxs[middle], dys[middle]];
    let tolerance = (pixel_threshold(width, height) * 0.2).max(0.05);
    let mut translation_inliers = 0usize;
    for (source, target) in pairs {
        if filmcraft_media::cancel::cancelled() {
            return true;
        }
        let dx = target[0] - source[0] - median[0];
        let dy = target[1] - source[1] - median[1];
        if dx * dx + dy * dy <= tolerance * tolerance {
            translation_inliers = translation_inliers.saturating_add(1);
        }
    }
    if translation_inliers.saturating_mul(3) >= pairs.len().saturating_mul(2) {
        return true;
    }
    let mut mean = [0.0; 2];
    for (source, target) in pairs {
        if filmcraft_media::cancel::cancelled() {
            return true;
        }
        mean[0] += target[0] - source[0];
        mean[1] += target[1] - source[1];
    }
    mean[0] /= n;
    mean[1] /= n;
    let mut variance = 0.0;
    for (source, target) in pairs {
        if filmcraft_media::cancel::cancelled() {
            return true;
        }
        let dx = target[0] - source[0] - mean[0];
        let dy = target[1] - source[1] - mean[1];
        variance += dx * dx + dy * dy;
    }
    variance /= n;
    // A pure pan/translation has no finite epipole in the image.  Skip it
    // rather than inventing an arbitrary F; a perspective/depth-dependent
    // displacement has materially larger residual variation.
    let scale = (width.max(height) as f64).max(1.0);
    variance < (0.0005 * scale).powi(2)
}
fn fit_model(pairs: &[([f64; 2], [f64; 2])], indices: &[usize]) -> Option<[[f64; 3]; 3]> {
    if indices.len() < MIN_POINTS {
        return None;
    }
    let mut source = Vec::with_capacity(indices.len());
    let mut target = Vec::with_capacity(indices.len());
    for &index in indices {
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        let pair = pairs.get(index).copied()?;
        source.push(pair.0);
        target.push(pair.1);
    }
    let (src_norm, src_t) = normalize(&source)?;
    let (dst_norm, dst_t) = normalize(&target)?;
    let mut ata = [[0.0f64; 9]; 9];
    for (source, target) in src_norm.iter().zip(dst_norm) {
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        let x = [source[0], source[1], 1.0];
        let xp = [target[0], target[1], 1.0];
        let row = [xp[0] * x[0], xp[0] * x[1], xp[0] * x[2], xp[1] * x[0], xp[1] * x[1], xp[1] * x[2], xp[2] * x[0], xp[2] * x[1], xp[2] * x[2]];
        for r in 0..9 {
            for c in r..9 {
                ata[r][c] += row[r] * row[c];
            }
        }
    }
    for r in 0..9 {
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        for c in 0..r {
            ata[r][c] = ata[c][r];
        }
    }
    let (values, vectors) = super::numerics::symmetric_eigen::<9>(ata)?;
    let mut smallest = 0;
    for i in 1..9 {
        if values[i] < values[smallest] {
            smallest = i;
        }
    }
    let mut f_norm = [[0.0f64; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            f_norm[r][c] = vectors[r * 3 + c][smallest];
        }
    }
    let f_norm = enforce_rank2(f_norm)?;
    let f = mat_mul(mat_mul(transpose(dst_t), f_norm), src_t);
    normalize_matrix(f)
}

fn normalize(points: &[[f64; 2]]) -> Option<(Vec<[f64; 2]>, [[f64; 3]; 3])> {
    if points.len() < MIN_POINTS {
        return None;
    }
    let n = points.len() as f64;
    let mut center = [0.0; 2];
    for point in points {
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        if !point[0].is_finite() || !point[1].is_finite() {
            return None;
        }
        center[0] += point[0];
        center[1] += point[1];
    }
    center[0] /= n;
    center[1] /= n;
    let mut mean_distance = 0.0;
    for point in points {
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        mean_distance += ((point[0] - center[0]).powi(2) + (point[1] - center[1]).powi(2)).sqrt();
    }
    mean_distance /= n;
    if !mean_distance.is_finite() || mean_distance <= EPSILON {
        return None;
    }
    let scale = 2.0f64.sqrt() / mean_distance;
    let transform = [[scale, 0.0, -scale * center[0]], [0.0, scale, -scale * center[1]], [0.0, 0.0, 1.0]];
    let mut normalized = Vec::with_capacity(points.len());
    for point in points {
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        normalized.push([scale * (point[0] - center[0]), scale * (point[1] - center[1])]);
    }
    Some((normalized, transform))
}

fn enforce_rank2(f: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let ft = transpose(f);
    let ata = mat_mul(ft, f);
    let (values, vectors) = super::numerics::symmetric_eigen::<3>(ata)?;
    let mut smallest = 0;
    for i in 1..3 {
        if values[i] < values[smallest] {
            smallest = i;
        }
    }
    let mut v = [0.0; 3];
    for r in 0..3 {
        v[r] = vectors[r][smallest];
    }
    let mut u = [0.0; 3];
    for r in 0..3 {
        u[r] = f[r][0] * v[0] + f[r][1] * v[1] + f[r][2] * v[2];
    }
    let sigma = (u[0] * u[0] + u[1] * u[1] + u[2] * u[2]).sqrt();
    if !sigma.is_finite() {
        return None;
    }
    if sigma <= EPSILON {
        return Some(f);
    }
    for value in &mut u {
        *value /= sigma;
    }
    let mut result = f;
    for r in 0..3 {
        for c in 0..3 {
            result[r][c] -= sigma * u[r] * v[c];
        }
    }
    result.iter().flatten().all(|value| value.is_finite()).then_some(result)
}

fn normalize_matrix(mut matrix: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let norm = matrix.iter().flatten().map(|value| value * value).sum::<f64>().sqrt();
    if !norm.is_finite() || norm <= EPSILON {
        return None;
    }
    for row in &mut matrix {
        for value in row {
            *value /= norm;
        }
    }
    let (mut max_abs, mut sign) = (0.0, 1.0);
    for value in matrix.iter().flatten() {
        if value.abs() > max_abs {
            max_abs = value.abs();
            sign = value.signum();
        }
    }
    if sign < 0.0 {
        for row in &mut matrix {
            for value in row {
                *value = -*value;
            }
        }
    }
    Some(matrix)
}

fn inliers(model: &[[f64; 3]; 3], pairs: &[([f64; 2], [f64; 2])], threshold: f64) -> Option<Vec<usize>> {
    let mut result = Vec::with_capacity(pairs.len());
    for (index, pair) in pairs.iter().enumerate() {
        if filmcraft_media::cancel::cancelled() {
            return None;
        }
        if sampson_error(model, pair.0, pair.1).is_some_and(|error| error <= threshold) {
            result.push(index);
        }
    }
    Some(result)
}

fn sampson_error(model: &[[f64; 3]; 3], source: [f64; 2], target: [f64; 2]) -> Option<f64> {
    let x = [source[0], source[1], 1.0];
    let xp = [target[0], target[1], 1.0];
    let fx = [
        model[0][0] * x[0] + model[0][1] * x[1] + model[0][2],
        model[1][0] * x[0] + model[1][1] * x[1] + model[1][2],
        model[2][0] * x[0] + model[2][1] * x[1] + model[2][2],
    ];
    let ftxp = [
        model[0][0] * xp[0] + model[1][0] * xp[1] + model[2][0],
        model[0][1] * xp[0] + model[1][1] * xp[1] + model[2][1],
        model[0][2] * xp[0] + model[1][2] * xp[1] + model[2][2],
    ];
    let residual = xp[0] * fx[0] + xp[1] * fx[1] + fx[2];
    let denominator = fx[0] * fx[0] + fx[1] * fx[1] + ftxp[0] * ftxp[0] + ftxp[1] * ftxp[1];
    if !residual.is_finite() || !denominator.is_finite() || denominator <= EPSILON {
        return None;
    }
    Some((residual * residual / denominator).sqrt())
}

fn sample_indices(n: usize, rng: &mut Rng) -> Vec<usize> {
    let mut result = Vec::with_capacity(MIN_POINTS);
    let mut guard = 0;
    while result.len() < MIN_POINTS && guard < MIN_POINTS * 8 {
        let index = rng.below(n);
        if !result.contains(&index) {
            result.push(index);
        }
        guard += 1;
    }
    result
}

#[derive(Clone, Copy)]
struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }

    fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next() % bound as u64) as usize
    }
}

fn transpose(matrix: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    [[matrix[0][0], matrix[1][0], matrix[2][0]], [matrix[0][1], matrix[1][1], matrix[2][1]], [matrix[0][2], matrix[1][2], matrix[2][2]]]
}

fn mat_mul(a: [[f64; 3]; 3], b: [[f64; 3]; 3]) -> [[f64; 3]; 3] {
    let mut result = [[0.0; 3]; 3];
    for r in 0..3 {
        for c in 0..3 {
            result[r][c] = (0..3).map(|k| a[r][k] * b[k][c]).sum();
        }
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parallax_is_kept_while_independent_tracks_are_rejected() {
        let mut tracks = Vec::new();
        for index in 0..18 {
            let row = (index / 6) as f64 - 1.0;
            let column = (index % 6) as f64 - 2.5;
            // Two alternating depths make the camera motion genuinely
            // identifiable as parallax rather than a single-plane homography.
            let depth = if index % 2 == 0 { 3.6 } else { 7.2 };
            let base = [column * 0.34, row * 0.42, depth];
            let dynamic = index >= 14;
            let points = (0..16)
                .map(|frame| {
                    let t = frame as f64;
                    let world = if dynamic {
                        [
                            base[0] + 0.9 * (t * 0.83 + index as f64).sin() + 0.15 * (t * 0.27 + index as f64).cos(),
                            base[1] + 0.7 * (t * 0.61 + index as f64 * 0.37).cos() + 0.55 * (t * 0.33 + index as f64 * 0.19).sin(),
                            base[2] + 0.8 * (t * 0.47 + index as f64 * 0.21).sin(),
                        ]
                    } else {
                        base
                    };
                    project(world, frame)
                })
                .collect();
            tracks.push(FeatureTrajectory { start: 0, points, rejected: false });
        }
        let index = TrajectoryIndex::new(&tracks, 16).expect("bounded fixture index");
        assert!(reject_dynamic_tracks(&mut tracks, 640, 480, 16, &index));
        assert!(tracks[..14].iter().all(|track| !track.rejected));
        assert!(tracks[14..].iter().all(|track| track.rejected));
    }

    fn project(world: [f64; 3], frame: usize) -> [f32; 2] {
        let t = frame as f64;
        let camera = [0.02 * t, 0.005 * t, 0.03 * t];
        let angle = 0.01 * t;
        let (sin, cos) = angle.sin_cos();
        let dx = world[0] - camera[0];
        let dy = world[1] - camera[1];
        let dz = world[2] - camera[2];
        let x = cos * dx + sin * dz;
        let z = -sin * dx + cos * dz;
        [((500.0 * x / z) + 320.0) as f32, ((500.0 * dy / z) + 240.0) as f32]
    }

    #[test]
    fn malformed_points_are_skipped_without_a_panic() {
        let mut tracks = vec![
            FeatureTrajectory { start: 0, points: vec![[0.0, 0.0]; 16], rejected: false },
            FeatureTrajectory { start: 0, points: vec![[1.0, 1.0]; 16], rejected: false },
        ];
        let index = TrajectoryIndex::new(&tracks, 16).expect("valid fixture index");
        for point in &mut tracks[0].points {
            *point = [f32::NAN, 0.0];
        }
        for point in &mut tracks[1].points {
            *point = [9999.0, -2.0];
        }
        assert!(reject_dynamic_tracks(&mut tracks, 640, 480, 16, &index));
        assert!(tracks.iter().all(|track| !track.rejected));
    }
}

//! Motion-aligned temporal edge samples. Missing evidence stays transparent, never stretched.
use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::EffectInstance;
use rayon::prelude::*;

use super::analysis::{Analysis, valid_image};
use super::mesh::MeshFrame;
use super::planning::StabilizationPlan;
use super::{FxEnv, Image};

struct Neighbour {
    frame: Image,
    /// Current source-layer coordinates → neighbouring source-layer coordinates.
    from_current: Affine,
    weight: f32,
}

/// Fraction of a bilinear footprint inside the source, independent of source alpha.
fn coverage(q: Vec2, w: usize, h: usize) -> f32 {
    if !q.x.is_finite() || !q.y.is_finite() {
        return 0.0;
    }
    ((q.x + 0.5).clamp(0.0, 1.0) * (w as f64 + 0.5 - q.x).clamp(0.0, 1.0) * (q.y + 0.5).clamp(0.0, 1.0) * (h as f64 + 0.5 - q.y).clamp(0.0, 1.0)) as f32
}

fn difference(a: [f32; 4], b: [f32; 4]) -> f32 {
    a.into_iter().zip(b).map(|(a, b)| (a - b).abs()).sum::<f32>() / 4.0
}

fn agrees(source: &Image, neighbour: &Neighbour, q: Vec2) -> bool {
    // Test the nearby overlap, not the unknown exterior. Reject cuts and local independently
    // moving content. A transparent source sample remains transparent inside its geometry.
    let anchor = Vec2::new(q.x.clamp(0.5, source.w as f64 - 0.5), q.y.clamp(0.5, source.h as f64 - 0.5));
    let mut error = 0.0;
    let mut count = 0;
    for (dx, dy) in [(0.0, 0.0), (-2.0, 0.0), (2.0, 0.0), (0.0, -2.0), (0.0, 2.0)] {
        let p = Vec2::new(anchor.x + dx, anchor.y + dy);
        let r = neighbour.from_current.apply(p);
        if coverage(p, source.w, source.h) >= 0.99999 && coverage(r, neighbour.frame.w, neighbour.frame.h) >= 0.99999 {
            error += difference(source.sample_bilinear(p.x as f32, p.y as f32), neighbour.frame.sample_bilinear(r.x as f32, r.y as f32));
            count += 1;
        }
    }
    count > 0 && error / (count as f32) < 0.08
}

fn overlap_confidence(source: &Image, neighbour: &Neighbour) -> Option<f32> {
    let mut error = 0.0;
    let mut count = 0;
    for y in 1..=6 {
        for x in 1..=6 {
            let p = Vec2::new(source.w as f64 * x as f64 / 7.0, source.h as f64 * y as f64 / 7.0);
            let q = neighbour.from_current.apply(p);
            if coverage(q, neighbour.frame.w, neighbour.frame.h) >= 0.99999 {
                error += difference(source.sample_bilinear(p.x as f32, p.y as f32), neighbour.frame.sample_bilinear(q.x as f32, q.y as f32));
                count += 1;
            }
        }
    }
    if count < 6 {
        return None;
    }
    let error = error / count as f32;
    (error.is_finite() && error < 0.08).then_some((1.0 - error / 0.08).max(0.0))
}

#[allow(clippy::too_many_arguments)]
pub(super) fn warp(
    img: &mut Image,
    matrix: &Affine,
    env: &dyn FxEnv,
    effect: &EffectInstance,
    analysis: &Analysis,
    plan: &StabilizationPlan,
    k: usize,
    now: f64,
    mesh: Option<&MeshFrame>,
) {
    let Some(inv) = matrix.inverse() else { return };
    let corners = [(0.5, 0.5), (img.w as f64 - 0.5, 0.5), (0.5, img.h as f64 - 0.5), (img.w as f64 - 0.5, img.h as f64 - 0.5)];
    let covered = if let Some(mesh) = mesh {
        // Four corners prove an affine inverse only. A piecewise mesh can move an interior
        // edge footprint outside the bilinear-safe inset even when all corners remain covered.
        mesh.safe_sampling_coverage(&inv, (analysis.path.w, analysis.path.h), (img.w, img.h))
    } else {
        corners.iter().all(|&(x, y)| {
            let point = inv.apply(Vec2::new(x, y));
            coverage(point, img.w, img.h) >= 0.99999
        })
    };
    if covered {
        if let Some(mesh) = mesh {
            *img = resample_mesh(img, &inv, mesh, (analysis.path.w, analysis.path.h), &[]);
        } else {
            super::affine_warp(img, matrix);
        }
        return;
    }
    let Some(camera_inv) = plan.cameras.get(k).and_then(Affine::inverse) else { return };
    let (sx, sy) = (img.w as f64 / analysis.path.w as f64, img.h as f64 / analysis.path.h as f64);
    let mut neighbours = Vec::with_capacity(6);
    for distance in 1..=3 {
        for j in [k.checked_sub(distance), k.checked_add(distance)].into_iter().flatten() {
            if j >= plan.cameras.len() {
                continue;
            }
            let (start, end) = (j.min(k), j.max(k));
            if !analysis.valid_steps.get(start..end).is_some_and(|links| links.iter().all(|&valid| valid)) {
                continue;
            }
            let Some(camera) = plan.cameras.get(j) else { continue };
            let Some(frame) = env.frame(j as f64 / analysis.path.fps - now, effect).filter(valid_image) else { continue };
            if (frame.w, frame.h) != (img.w, img.h) {
                continue;
            }
            let from_current = Affine::scale(sx, sy).then_apply(camera).then_apply(&camera_inv).then_apply(&Affine::scale(1.0 / sx, 1.0 / sy));
            let mut neighbour = Neighbour { frame, from_current, weight: 1.0 / distance as f32 };
            if let Some(confidence) = overlap_confidence(img, &neighbour) {
                neighbour.weight *= confidence;
                neighbours.push(neighbour);
            }
        }
    }
    if let Some(mesh) = mesh {
        *img = resample_mesh(img, &inv, mesh, (analysis.path.w, analysis.path.h), &neighbours);
    } else {
        *img = resample(img, &inv, &neighbours);
    }
}

/// Render one frame with a solved subspace mesh and no temporal synthesis.
pub(super) fn warp_single(img: &mut Image, matrix: &Affine, mesh: &MeshFrame, analysis_size: (usize, usize)) {
    let Some(inv) = matrix.inverse() else { return };
    *img = resample_mesh(img, &inv, mesh, analysis_size, &[]);
}

fn mesh_source_point(inv: &Affine, mesh: &MeshFrame, analysis: (usize, usize), layer: (usize, usize), output: Vec2) -> Option<Vec2> {
    let (aw, ah) = (analysis.0 as f64, analysis.1 as f64);
    let (lw, lh) = (layer.0 as f64, layer.1 as f64);
    if !aw.is_finite() || !ah.is_finite() || aw <= 0.0 || ah <= 0.0 || !lw.is_finite() || !lh.is_finite() || lw <= 0.0 || lh <= 0.0 {
        return None;
    }
    let q_layer = inv.apply(output);
    if !q_layer.x.is_finite() || !q_layer.y.is_finite() {
        return None;
    }
    let q_analysis = Vec2::new(q_layer.x * aw / lw, q_layer.y * ah / lh);
    let source_analysis = mesh.inverse(q_analysis)?;
    let source = Vec2::new(source_analysis.x * lw / aw, source_analysis.y * lh / ah);
    (source.x.is_finite() && source.y.is_finite()).then_some(source)
}

fn resample_mesh(img: &Image, inv: &Affine, mesh: &MeshFrame, analysis: (usize, usize), neighbours: &[Neighbour]) -> Image {
    let mut out = Image::new(img.w, img.h);
    let w = img.w;
    out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
            let output = Vec2::new(x as f64 + 0.5, y as f64 + 0.5);
            let q = mesh_source_point(inv, mesh, analysis, (img.w, img.h), output);
            let covered = q.map_or(0.0, |point| coverage(point, img.w, img.h));
            let mut current = q.filter(|_| covered > 0.0).map_or([0.0; 4], |point| img.sample_bilinear(point.x as f32, point.y as f32));
            if covered < 0.99999
                && let Some(q) = q
            {
                let mut fill = [0.0; 4];
                let mut weight = 0.0;
                for neighbour in neighbours {
                    let p = neighbour.from_current.apply(q);
                    if coverage(p, neighbour.frame.w, neighbour.frame.h) < 0.99999 || !agrees(img, neighbour, q) {
                        continue;
                    }
                    let sample = neighbour.frame.sample_bilinear(p.x as f32, p.y as f32);
                    for (acc, value) in fill.iter_mut().zip(sample) {
                        *acc += value * neighbour.weight;
                    }
                    weight += neighbour.weight;
                }
                if weight > 0.0 {
                    for (value, fill) in current.iter_mut().zip(fill) {
                        *value += fill / weight * (1.0 - covered);
                    }
                }
            }
            pixel.copy_from_slice(&current);
        }
    });
    out
}

fn resample(img: &Image, inv: &Affine, neighbours: &[Neighbour]) -> Image {
    // The original buffer remains the immutable source; no unconditional full-image clone.
    let mut out = Image::new(img.w, img.h);
    let w = img.w;
    out.px.par_chunks_mut(w * 4).enumerate().for_each(|(y, row)| {
        for (x, pixel) in row.chunks_exact_mut(4).enumerate() {
            let q = inv.apply(Vec2::new(x as f64 + 0.5, y as f64 + 0.5));
            let covered = coverage(q, img.w, img.h);
            let mut current = if covered > 0.0 { img.sample_bilinear(q.x as f32, q.y as f32) } else { [0.0; 4] };
            if covered < 0.99999 {
                let mut fill = [0.0; 4];
                let mut weight = 0.0;
                for neighbour in neighbours {
                    let p = neighbour.from_current.apply(q);
                    if coverage(p, neighbour.frame.w, neighbour.frame.h) < 0.99999 || !agrees(img, neighbour, q) {
                        continue;
                    }
                    let sample = neighbour.frame.sample_bilinear(p.x as f32, p.y as f32);
                    for (acc, value) in fill.iter_mut().zip(sample) {
                        *acc += value * neighbour.weight;
                    }
                    weight += neighbour.weight;
                }
                if weight > 0.0 {
                    for (value, fill) in current.iter_mut().zip(fill) {
                        *value += fill / weight * (1.0 - covered);
                    }
                }
            }
            pixel.copy_from_slice(&current);
        }
    });
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bilinear_coverage_ignores_alpha_and_accounts_for_partial_edges() {
        assert_eq!(coverage(Vec2::new(0.25, 3.5), 8, 8), 0.75);
        assert_eq!(coverage(Vec2::new(0.0, 0.0), 8, 8), 0.25);
        assert_eq!(coverage(Vec2::new(-0.5, 3.5), 8, 8), 0.0);
        assert_eq!(coverage(Vec2::new(7.75, 3.5), 8, 8), 0.75);
        assert_eq!(coverage(Vec2::new(f64::NAN, 0.5), 8, 8), 0.0);
    }

    #[test]
    fn temporal_samples_restore_new_content_without_stretching_or_filling_real_transparency() {
        let mut source = Image::filled(16, 8, [0.2, 0.2, 0.2, 1.0]);
        source.px[(4 * 16 + 8) * 4..(4 * 16 + 9) * 4].fill(0.0);
        let mut frame = Image::filled(16, 8, [0.2, 0.2, 0.2, 1.0]);
        // The neighbouring camera sees two additional columns to the left of this frame.
        for y in 0..8 {
            for x in 0..2 {
                let i = (y * 16 + x) * 4;
                frame.px[i..i + 4].copy_from_slice(&[0.7, 0.1, 0.1, 1.0]);
            }
        }
        let neighbours = [Neighbour { frame, from_current: Affine::translate(2.0, 0.0), weight: 1.0 }];
        let result = resample(&source, &Affine::translate(-2.0, 0.0), &neighbours);
        assert_eq!(result.get(0, 4), [0.7, 0.1, 0.1, 1.0], "new content, not repeated grey edge pixels");
        assert_eq!(result.get(10, 4), [0.0; 4], "legitimate source transparency survives");
        assert_eq!(result.get(6, 4), [0.2, 0.2, 0.2, 1.0]);

        let partial = resample(&source, &Affine::translate(-0.25, 0.0), &neighbours);
        let p = partial.get(0, 4);
        assert!((p[0] - 0.23125).abs() < 1e-6 && (p[3] - 1.0).abs() < 1e-6, "partial geometric coverage: {p:?}");
    }

    #[test]
    fn mismatched_temporal_content_leaves_an_honest_hole() {
        let source = Image::filled(16, 8, [0.2, 0.2, 0.2, 1.0]);
        let neighbours = [Neighbour { frame: Image::filled(16, 8, [0.9, 0.9, 0.9, 1.0]), from_current: Affine::translate(2.0, 0.0), weight: 1.0 }];
        assert!(overlap_confidence(&source, &neighbours[0]).is_none());
        let result = resample(&source, &Affine::translate(-2.0, 0.0), &neighbours);
        assert_eq!(result.get(0, 4), [0.0; 4], "no foreign frame or clamped-border fallback");
    }

    #[test]
    fn warped_mesh_disables_corner_coverage_shortcut() {
        let saliency = [1.0; super::super::trajectory_data::GRID_CELLS];
        let constraints = [
            super::super::trajectory_data::PointConstraint { source: [20.0, 20.0], target: [10.0, 20.0], weight: 12.0 },
            super::super::trajectory_data::PointConstraint { source: [20.0, 90.0], target: [10.0, 90.0], weight: 12.0 },
            super::super::trajectory_data::PointConstraint { source: [20.0, 160.0], target: [10.0, 160.0], weight: 12.0 },
        ];
        let mesh = MeshFrame::solve(320, 180, Affine::IDENTITY, &constraints, &saliency).expect("warped mesh");
        // The affine four-corner proof is deliberately insufficient for this first-column warp.
        assert!(!mesh.safe_sampling_coverage(&Affine::IDENTITY, (320, 180), (320, 180)));
    }
}

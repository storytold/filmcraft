//! Mask coverage (CPU reference; `filmcraft_gpu::GpuMask` runs the same math in WGSL).
//!
//! Each [`Mask`] is flattened to a polygon in working-image pixels (chord error ≤ 0.05 px). For
//! a pixel centre `p` the signed distance `sd` to the polygon (positive inside, nonzero winding)
//! gives the coverage
//!
//! ```text
//! s  = sd + expansion
//! w  = max(feather, 1)
//! u  = clamp(s / w + 0.5, 0, 1)
//! c  = lerp(u, smoothstep(u), min(feather, 1))
//! m  = opacity · (inverted ? 1 − c : c)
//! ```
//!
//! so a hard mask (feather 0) is the analytic box-filtered coverage of the edge (a one-pixel
//! linear ramp across the boundary: exact for straight edges, antialiased everywhere), and a
//! feathered mask falls off smoothly over `feather` pixels centred on the (expanded) edge — a true
//! distance falloff, so corners round off and the width is the same along the whole contour.
//! Masks combine top to bottom with their [`MaskMode`]. All of this runs in linear light on the
//! premultiplied working image: a masked effect is `lerp(original, effected, coverage)` per
//! channel, an opacity mask scales the layer's premultiplied pixels.

use filmcraft_project::{EffectInstance, Mask, MaskMode};
use filmcraft_time::Tick;
use rayon::prelude::*;

use crate::image::Image;

/// Flattening tolerance in working pixels.
pub const FLATTEN_TOLERANCE: f64 = 0.05;

/// One mask ready for rasterizing, in working-image pixels.
#[derive(Clone, Debug, PartialEq)]
pub struct FlatMask {
    /// Closed polygon (last vertex connects to the first).
    pub pts: Vec<[f32; 2]>,
    pub feather: f32,
    pub expansion: f32,
    /// 0..1.
    pub opacity: f32,
    pub inverted: bool,
    pub mode: MaskMode,
}

/// Flatten the active masks (mode ≠ None) at clip time `t`. `px_scale` = working pixels per clip pixel.
pub fn prepare(masks: &[Mask], t: Tick, px_scale: f32) -> Vec<FlatMask> {
    let s = px_scale as f64;
    masks
        .iter()
        .filter(|m| m.mode != MaskMode::None)
        .map(|m| {
            let path = m.path_at(t);
            let pts = path.flatten(FLATTEN_TOLERANCE / s.max(1e-6)).into_iter().map(|p| [(p.x * s) as f32, (p.y * s) as f32]).collect();
            FlatMask {
                pts,
                feather: (m.feather.f64_at(t).max(0.0) * s) as f32,
                expansion: (m.expansion.f64_at(t) * s) as f32,
                opacity: (m.opacity.f64_at(t) / 100.0).clamp(0.0, 1.0) as f32,
                inverted: m.inverted,
                mode: m.mode,
            }
        })
        .collect()
}

/// The falloff of a signed (expanded) distance for a feather width.
#[inline]
pub fn falloff(s: f32, feather: f32) -> f32 {
    let w = feather.max(1.0);
    let u = (s / w + 0.5).clamp(0.0, 1.0);
    let smooth = u * u * (3.0 - 2.0 * u);
    u + (smooth - u) * feather.min(1.0)
}

/// Distance band beyond which coverage is saturated (0 or 1).
fn band(m: &FlatMask) -> f32 {
    m.expansion.abs() + m.feather.max(1.0) * 0.5 + 1.0
}

#[inline]
fn seg_dist2(px: f32, py: f32, a: [f32; 2], b: [f32; 2]) -> f32 {
    let (dx, dy) = (b[0] - a[0], b[1] - a[1]);
    let (qx, qy) = (px - a[0], py - a[1]);
    let l2 = dx * dx + dy * dy;
    let t = if l2 > 0.0 { ((qx * dx + qy * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
    let (ex, ey) = (qx - dx * t, qy - dy * t);
    ex * ex + ey * ey
}

/// Coverage of one mask for row `y` into `out` (length = width). Reference per-row algorithm:
/// winding from the row's sorted edge crossings, distance only against edges within the band.
fn mask_row(m: &FlatMask, y: usize, out: &mut [f32]) {
    let n = m.pts.len();
    let py = y as f32 + 0.5;
    let b = band(m);
    let value = |sd: f32| {
        let c = falloff(sd + m.expansion, m.feather);
        m.opacity * if m.inverted { 1.0 - c } else { c }
    };
    if n < 3 {
        out.fill(value(-b));
        return;
    }
    // crossings of the scanline with the polygon edges (x, direction)
    let mut cross: Vec<(f32, i32)> = Vec::new();
    // edges within the band of this row: (a, b, min x, max x)
    let mut near: Vec<([f32; 2], [f32; 2], f32, f32)> = Vec::new();
    for i in 0..n {
        let a = m.pts[i];
        let c = m.pts[(i + 1) % n];
        if (a[1] <= py) != (c[1] <= py) {
            let t = (py - a[1]) / (c[1] - a[1]);
            cross.push((a[0] + (c[0] - a[0]) * t, if c[1] > a[1] { 1 } else { -1 }));
        }
        if py >= a[1].min(c[1]) - b && py <= a[1].max(c[1]) + b {
            near.push((a, c, a[0].min(c[0]) - b, a[0].max(c[0]) + b));
        }
    }
    cross.sort_by(|p, q| p.0.total_cmp(&q.0));
    let mut ci = 0;
    let mut wind = 0;
    let b2 = b * b;
    for (x, o) in out.iter_mut().enumerate() {
        let px = x as f32 + 0.5;
        while ci < cross.len() && cross[ci].0 < px {
            wind += cross[ci].1;
            ci += 1;
        }
        let inside = wind != 0;
        let mut d2 = b2;
        for (a, c, x0, x1) in &near {
            if px >= *x0 && px <= *x1 {
                d2 = d2.min(seg_dist2(px, py, *a, *c));
            }
        }
        let d = d2.sqrt();
        *o = value(if inside { d } else { -d });
    }
}

/// Combined coverage of `masks` over a `w`×`h` image, or `None` when there is no active mask
/// (the effect applies everywhere).
pub fn coverage(masks: &[FlatMask], w: usize, h: usize) -> Option<Vec<f32>> {
    let first = masks.iter().find(|m| m.mode != MaskMode::None)?;
    let start = first.mode.start();
    let mut acc = vec![start; w * h];
    if w == 0 {
        return Some(acc);
    }
    acc.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let mut tmp = vec![0.0f32; w];
        for m in masks.iter().filter(|m| m.mode != MaskMode::None) {
            mask_row(m, y, &mut tmp);
            for (a, v) in row.iter_mut().zip(&tmp) {
                *a = m.mode.combine(*a, *v).clamp(0.0, 1.0);
            }
        }
    });
    Some(acc)
}

/// Coverage of an effect's masks on a working image (`px_scale` working px per clip px).
pub fn effect_coverage(masks: &[Mask], t: Tick, px_scale: f32, w: usize, h: usize) -> Option<Vec<f32>> {
    if masks.is_empty() {
        return None;
    }
    coverage(&prepare(masks, t, px_scale), w, h)
}

/// `img = lerp(original, img, cov)` on premultiplied pixels.
pub fn mix(img: &mut Image, original: &Image, cov: &[f32]) {
    img.px.par_chunks_mut(4).zip(original.px.par_chunks(4)).zip(cov.par_iter()).for_each(|((p, o), &c)| {
        for k in 0..4 {
            p[k] = o[k] + (p[k] - o[k]) * c;
        }
    });
}

/// Scale premultiplied pixels by coverage (opacity masks).
pub fn scale_by(img: &mut Image, cov: &[f32]) {
    img.px.par_chunks_mut(4).zip(cov.par_iter()).for_each(|(p, &c)| {
        for v in p {
            *v *= c;
        }
    });
}

/// Apply an effect limited to its masks (Premiere: a masked effect only applies inside the mask).
pub fn apply_effect(img: &mut Image, e: &EffectInstance, cx: &crate::effects::FxCtx) -> crate::Result<()> {
    if !e.enabled || e.masks.is_empty() {
        return crate::effects::apply(img, e, cx);
    }
    let Some(cov) = effect_coverage(&e.masks, cx.t, cx.px_scale, img.w, img.h) else {
        return crate::effects::apply(img, e, cx);
    };
    let original = img.clone();
    crate::effects::apply(img, e, cx)?;
    mix(img, &original, &cov);
    Ok(())
}

/// Apply the masks of an item's Opacity effect to its layer (before Motion), if any.
pub fn apply_opacity_masks(img: &mut Image, item: &filmcraft_project::TrackItem, t: Tick, px_scale: f32) {
    let Some(op) = item.effect("opacity").filter(|e| e.enabled && !e.masks.is_empty()) else { return };
    if let Some(cov) = effect_coverage(&op.masks, t, px_scale, img.w, img.h) {
        scale_by(img, &cov);
    }
}

/// Brute-force coverage of one pixel (every edge; the definition the row algorithm and the WGSL
/// shader implement). For tests and documentation.
pub fn coverage_at(masks: &[FlatMask], x: f32, y: f32) -> Option<f32> {
    let first = masks.iter().find(|m| m.mode != MaskMode::None)?;
    let mut acc = first.mode.start();
    for m in masks.iter().filter(|m| m.mode != MaskMode::None) {
        let n = m.pts.len();
        let mut wind = 0;
        let mut d2 = f32::INFINITY;
        for i in 0..n {
            let a = m.pts[i];
            let c = m.pts[(i + 1) % n];
            if (a[1] <= y) != (c[1] <= y) {
                let t = (y - a[1]) / (c[1] - a[1]);
                if a[0] + (c[0] - a[0]) * t < x {
                    wind += if c[1] > a[1] { 1 } else { -1 };
                }
            }
            d2 = d2.min(seg_dist2(x, y, a, c));
        }
        let d = if n < 3 { band(m) } else { d2.sqrt().min(band(m)) };
        let sd = if wind != 0 && n >= 3 { d } else { -d };
        let c = falloff(sd + m.expansion, m.feather);
        let v = m.opacity * if m.inverted { 1.0 - c } else { c };
        acc = m.mode.combine(acc, v).clamp(0.0, 1.0);
    }
    Some(acc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_geom::Vec2;
    use filmcraft_project::MaskPath;

    fn mask(path: MaskPath, feather: f64) -> Mask {
        let mut m = Mask::new("m", path);
        m.feather.value = filmcraft_project::ParamValue::Float(feather);
        m
    }

    #[test]
    fn hard_rect_is_exact_box_coverage() {
        // rect edges on pixel boundaries → exactly 1 inside, 0 outside; half-pixel edge → 0.5
        let m = mask(MaskPath::rect(2.0, 2.0, 6.5, 6.0), 0.0);
        let c = effect_coverage(&[m], Tick(0), 1.0, 10, 10).unwrap();
        assert_eq!(c[3 * 10 + 3], 1.0);
        assert_eq!(c[3 * 10 + 1], 0.0);
        assert!((c[3 * 10 + 6] - 0.5).abs() < 1e-6, "{}", c[3 * 10 + 6]);
        assert_eq!(c[8 * 10 + 3], 0.0);
    }

    #[test]
    fn ellipse_area_matches_analytic() {
        let (rx, ry) = (37.3, 21.7);
        let m = mask(MaskPath::ellipse(Vec2::new(50.2, 40.1), Vec2::new(rx, ry)), 0.0);
        let c = effect_coverage(&[m], Tick(0), 1.0, 100, 80).unwrap();
        let area: f64 = c.iter().map(|v| *v as f64).sum();
        let exact = std::f64::consts::PI * rx * ry;
        assert!((area - exact).abs() / exact < 0.002, "{area} vs {exact}");
    }

    #[test]
    fn feather_is_a_symmetric_distance_falloff() {
        let m = mask(MaskPath::rect(20.0, -100.0, 200.0, 200.0), 10.0);
        let c = effect_coverage(&[m], Tick(0), 1.0, 60, 4).unwrap();
        // edge at x = 20: 0.5 there, saturated 5 px away, symmetric
        let at = |x: usize| c[x];
        assert!((at(19) + at(20) - 1.0).abs() < 1e-5, "{} {}", at(19), at(20));
        assert!(at(13) < 1e-6 && at(26) > 1.0 - 1e-6, "{} {}", at(13), at(26));
        for x in 14..26 {
            assert!(at(x + 1) >= at(x), "monotonic");
        }
    }

    #[test]
    fn expansion_inverted_opacity_modes() {
        let mut m = mask(MaskPath::rect(10.0, 10.0, 20.0, 20.0), 0.0);
        m.expansion.value = filmcraft_project::ParamValue::Float(5.0);
        let c = effect_coverage(std::slice::from_ref(&m), Tick(0), 1.0, 30, 30).unwrap();
        assert_eq!(c[15 * 30 + 6], 1.0, "expanded by 5");
        assert_eq!(c[15 * 30 + 4], 0.0);
        m.inverted = true;
        m.opacity.value = filmcraft_project::ParamValue::Float(50.0);
        let c = effect_coverage(&[m.clone()], Tick(0), 1.0, 30, 30).unwrap();
        assert_eq!(c[15 * 30 + 15], 0.0);
        assert_eq!(c[2], 0.5);
        // subtract a hole from a big rect
        let mut big = mask(MaskPath::rect(0.0, 0.0, 30.0, 30.0), 0.0);
        big.mode = MaskMode::Add;
        let mut hole = mask(MaskPath::rect(10.0, 10.0, 20.0, 20.0), 0.0);
        hole.mode = MaskMode::Subtract;
        let c = effect_coverage(&[big, hole], Tick(0), 1.0, 30, 30).unwrap();
        assert_eq!(c[15 * 30 + 15], 0.0);
        assert_eq!(c[5 * 30 + 5], 1.0);
    }

    #[test]
    fn row_algorithm_matches_brute_force() {
        let mut a = mask(MaskPath::ellipse(Vec2::new(30.0, 25.0), Vec2::new(20.0, 12.0)), 6.0);
        a.expansion.value = filmcraft_project::ParamValue::Float(-2.0);
        let mut b = mask(MaskPath::polygon(&[Vec2::new(5.0, 5.0), Vec2::new(50.0, 10.0), Vec2::new(20.0, 45.0)]), 0.0);
        b.mode = MaskMode::Difference;
        b.inverted = true;
        let flat = prepare(&[a, b], Tick(0), 0.8);
        let (w, h) = (48, 40);
        let c = coverage(&flat, w, h).unwrap();
        for y in 0..h {
            for x in 0..w {
                let r = coverage_at(&flat, x as f32 + 0.5, y as f32 + 0.5).unwrap();
                assert!((r - c[y * w + x]).abs() < 1e-5, "({x},{y}) {r} vs {}", c[y * w + x]);
            }
        }
    }
}

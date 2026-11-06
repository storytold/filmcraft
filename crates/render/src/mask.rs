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
//!
//! A mask with a colour range multiplies `c` (before inversion) by the HSL Secondary key of the
//! picture the effect receives (display-encoded, as Lumetri keys an SDR picture), Gaussian
//! blurred by Blur (σ up to 20 px at full resolution). Its colour range runs on the CPU only.

use filmcraft_project::{ColorRange, EffectInstance, Mask, MaskMode};
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

/// Coverage of one mask for row `y` into `out` (length = width), times its colour key row if it
/// has one. Reference per-row algorithm: winding from the row's sorted edge crossings, distance
/// only against edges within the band.
fn mask_row(m: &FlatMask, y: usize, out: &mut [f32], key: Option<&[f32]>) {
    let n = m.pts.len();
    let py = y as f32 + 0.5;
    let b = band(m);
    let value = |sd: f32, x: usize| {
        let c = falloff(sd + m.expansion, m.feather) * key.map_or(1.0, |k| k.get(x).copied().unwrap_or(0.0));
        m.opacity * if m.inverted { 1.0 - c } else { c }
    };
    if n < 3 {
        let v = value(-b, 0);
        out.fill(v);
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
        *o = value(if inside { d } else { -d }, x);
    }
}

/// Combined coverage of `masks` over a `w`×`h` image, or `None` when there is no active mask
/// (the effect applies everywhere).
pub fn coverage(masks: &[FlatMask], w: usize, h: usize) -> Option<Vec<f32>> {
    keyed(masks, &[], w, h)
}

/// [`coverage`] with `keys[i]`, when present, the colour key (`w`×`h`) of `masks[i]`.
fn keyed(masks: &[FlatMask], keys: &[Option<Vec<f32>>], w: usize, h: usize) -> Option<Vec<f32>> {
    let first = masks.iter().find(|m| m.mode != MaskMode::None)?;
    let start = first.mode.start();
    let mut acc = vec![start; w * h];
    if w == 0 {
        return Some(acc);
    }
    acc.par_chunks_mut(w).enumerate().for_each(|(y, row)| {
        let mut tmp = vec![0.0f32; w];
        for (i, m) in masks.iter().enumerate().filter(|(_, m)| m.mode != MaskMode::None) {
            let key = keys.get(i).and_then(Option::as_deref).map(|k| k.get(y * w..(y + 1) * w).unwrap_or(&[]));
            mask_row(m, y, &mut tmp, key);
            for (a, v) in row.iter_mut().zip(&tmp) {
                *a = m.mode.combine(*a, *v).clamp(0.0, 1.0);
            }
        }
    });
    Some(acc)
}

/// Coverage of an effect's mask shapes on a working image (`px_scale` working px per clip px);
/// colour ranges need the picture ([`picture_coverage`]) and are left out.
pub fn effect_coverage(masks: &[Mask], t: Tick, px_scale: f32, w: usize, h: usize) -> Option<Vec<f32>> {
    if masks.is_empty() {
        return None;
    }
    coverage(&prepare(masks, t, px_scale), w, h)
}

/// Coverage of an effect's masks, colour ranges keyed on `src` (the picture the effect receives).
pub fn picture_coverage(masks: &[Mask], t: Tick, px_scale: f32, src: &Image) -> Option<Vec<f32>> {
    if masks.is_empty() {
        return None;
    }
    let keys: Vec<Option<Vec<f32>>> =
        masks.iter().filter(|m| m.mode != MaskMode::None).map(|m| m.color.as_deref().map(|c| color_key(src, c, t, px_scale))).collect();
    keyed(&prepare(masks, t, px_scale), &keys, src.w, src.h)
}

/// The colour key (0…1 per pixel) of a colour range on `src`. Values from the project are
/// clamped to the parameter ranges; a non-number takes the default.
pub fn color_key(src: &Image, c: &ColorRange, t: Tick, px_scale: f32) -> Vec<f32> {
    let v = |p: &filmcraft_project::Param, lo: f64, hi: f64, def: f64| {
        let x = p.f64_at(t);
        (if x.is_finite() { x.clamp(lo, hi) } else { def }) as f32
    };
    let hue = c.hue.f64_at(t);
    let hc = (if hue.is_finite() { hue.rem_euclid(360.0) } else { 0.0 }) as f32 / 360.0;
    let hr = v(&c.hue_range, 1.0, 180.0, 30.0) / 360.0;
    let (smin, lmin, lmax) = (v(&c.sat_min, 0.0, 100.0, 10.0) / 100.0, v(&c.luma_min, 0.0, 100.0, 5.0) / 100.0, v(&c.luma_max, 0.0, 100.0, 95.0) / 100.0);
    let soft = (v(&c.soften, 0.0, 100.0, 20.0) / 100.0 * 0.3).max(0.01);
    let key: Vec<f32> = src
        .px
        .par_chunks(4)
        .map(|p| {
            if p[3] <= 1e-6 {
                return 0.0;
            }
            let u = crate::effects::enc(Image::unpremul([p[0], p[1], p[2], p[3]])).map(|q| if q.is_finite() { q.clamp(0.0, 1.0) } else { 0.0 });
            let k = crate::effects::hsl_key(u, hc, hr, smin, lmin, lmax, soft);
            if k.is_finite() { k.clamp(0.0, 1.0) } else { 0.0 }
        })
        .collect();
    let scale = if px_scale.is_finite() { px_scale.max(0.0) } else { 1.0 };
    let sigma = v(&c.blur, 0.0, 100.0, 0.0) / 100.0 * 20.0 * scale;
    if sigma >= 0.3 { crate::vfx::blur_plane(&key, src.w, src.h, sigma) } else { key }
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
pub fn apply_effect(img: &mut Image, e: &EffectInstance, cx: &crate::effects::FxCtx) {
    if !e.enabled || e.masks.is_empty() {
        crate::effects::apply(img, e, cx);
        return;
    }
    let Some(cov) = picture_coverage(&e.masks, cx.t, cx.mask_scale, img) else {
        crate::effects::apply(img, e, cx);
        return;
    };
    let original = img.clone();
    crate::effects::apply(img, e, cx);
    mix(img, &original, &cov);
}

/// Apply the masks of an item's Opacity effect to its layer (before Motion), if any. Colour
/// ranges key on `key_src`, or on the layer itself when it is `None`.
pub fn apply_opacity_masks(img: &mut Image, key_src: Option<&Image>, item: &filmcraft_project::TrackItem, t: Tick, px_scale: f32) {
    let Some(op) = item.effect("opacity").filter(|e| e.enabled && !e.masks.is_empty()) else { return };
    let src = key_src.filter(|s| (s.w, s.h) == (img.w, img.h)).unwrap_or(&*img);
    if let Some(cov) = picture_coverage(&op.masks, t, px_scale, src) {
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

    /// An effect's mask outline is placed by the uncapped ratio, not the capped effect-size scale.
    #[test]
    fn effect_masks_use_the_uncapped_scale() {
        let mut e = filmcraft_project::find_effect("invert").unwrap().instance();
        e.masks.push(mask(MaskPath::rect(0.0, 0.0, 4.0, 8.0), 0.0));
        let cx = crate::effects::FxCtx {
            t: Tick(0),
            px_scale: 0.1,
            mask_scale: 2.0,
            seconds: 0.0,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            frame_rate: 24.0,
            working: filmcraft_color::WorkingSpace::Rec709,
        };
        let mut img = Image::filled(8, 8, [1.0, 1.0, 1.0, 1.0]);
        apply_effect(&mut img, &e, &cx);
        // the 4-wide mask covers all 8 working pixels at 2 working px per clip px
        assert!(img.px.chunks(4).all(|p| p[0] < 0.5), "mask did not cover the whole image");
    }

    /// Left half red, right half blue (opaque), `w`×`h`.
    fn red_blue(w: usize, h: usize) -> Image {
        let mut img = Image::new(w, h);
        let (red, blue) = (crate::effects::dec([0.8, 0.1, 0.1]), crate::effects::dec([0.1, 0.2, 0.8]));
        for y in 0..h {
            for x in 0..w {
                let c = if x < w / 2 { red } else { blue };
                img.px[(y * w + x) * 4..(y * w + x) * 4 + 4].copy_from_slice(&[c[0], c[1], c[2], 1.0]);
            }
        }
        img
    }

    fn whole(w: f64, h: f64) -> Mask {
        let mut m = mask(MaskPath::rect(-w, -h, 2.0 * w, 2.0 * h), 0.0);
        m.color = Some(Box::default());
        m
    }

    #[test]
    fn color_range_limits_a_mask_to_matching_pixels() {
        let img = red_blue(20, 10);
        let at = |c: &[f32], x: usize, y: usize| c[y * 20 + x];
        // the default range is centred on red
        let m = whole(20.0, 10.0);
        let c = picture_coverage(std::slice::from_ref(&m), Tick(0), 1.0, &img).unwrap();
        assert_eq!((at(&c, 2, 5), at(&c, 17, 5)), (1.0, 0.0));
        // the shapes alone ignore the colour range
        assert_eq!(at(&effect_coverage(std::slice::from_ref(&m), Tick(0), 1.0, 20, 10).unwrap(), 17, 5), 1.0);
        // inverted: everything but the range
        let mut inv = m.clone();
        inv.inverted = true;
        let c = picture_coverage(&[inv], Tick(0), 1.0, &img).unwrap();
        assert_eq!((at(&c, 2, 5), at(&c, 17, 5)), (0.0, 1.0));
        // a blue range
        let mut b = m.clone();
        b.param_mut("color_hue").unwrap().value = filmcraft_project::ParamValue::Float(225.0);
        let c = picture_coverage(&[b], Tick(0), 1.0, &img).unwrap();
        assert_eq!((at(&c, 2, 5), at(&c, 17, 5)), (0.0, 1.0));
        // a shape with a colour range: the top of the frame, red only
        let mut top = mask(MaskPath::rect(-20.0, -10.0, 40.0, 5.0), 0.0);
        top.color = Some(Box::default());
        let c = picture_coverage(&[top], Tick(0), 1.0, &img).unwrap();
        assert_eq!((at(&c, 2, 2), at(&c, 2, 8), at(&c, 17, 2)), (1.0, 0.0, 0.0));
        // combined with a plain shape: the left quarter, intersected with red
        let mut quarter = mask(MaskPath::rect(0.0, -10.0, 5.0, 20.0), 0.0);
        quarter.mode = MaskMode::Add;
        let mut red = m.clone();
        red.mode = MaskMode::Intersect;
        let c = picture_coverage(&[quarter, red], Tick(0), 1.0, &img).unwrap();
        assert_eq!((at(&c, 2, 5), at(&c, 7, 5), at(&c, 17, 5)), (1.0, 0.0, 0.0));
        // Blur softens the key edge
        let mut soft = m;
        soft.param_mut("color_blur").unwrap().value = filmcraft_project::ParamValue::Float(10.0);
        let c = picture_coverage(&[soft], Tick(0), 1.0, &img).unwrap();
        assert!(at(&c, 9, 5) < 1.0 && at(&c, 9, 5) > 0.5 && at(&c, 10, 5) > 0.0 && at(&c, 10, 5) < 0.5, "{} {}", at(&c, 9, 5), at(&c, 10, 5));
    }

    /// A masked effect with a colour range changes only the matching pixels.
    #[test]
    fn color_range_masks_an_effect() {
        let img = red_blue(8, 4);
        let mut e = filmcraft_project::find_effect("black_white").unwrap().instance();
        e.masks.push(whole(8.0, 4.0));
        let cx = crate::effects::FxCtx {
            t: Tick::ZERO,
            px_scale: 1.0,
            mask_scale: 1.0,
            seconds: 0.0,
            timecode: "01:00:00:00",
            clip_name: "clip",
            project: None,
            env: None,
            frame_rate: 25.0,
            working: Default::default(),
        };
        let mut out = img.clone();
        apply_effect(&mut out, &e, &cx);
        let p = out.get(1, 1);
        assert!((p[0] - p[1]).abs() < 1e-4 && (p[1] - p[2]).abs() < 1e-4, "red turns grey: {p:?}");
        assert_eq!(out.get(6, 1), img.get(6, 1), "blue stays");
    }

    /// Hostile colour range values, scales and pictures: no panic, coverage in 0…1.
    #[test]
    fn color_range_survives_hostile_input() {
        let mut img = red_blue(5, 3);
        img.px[0] = f32::NAN;
        img.px[7] = 0.0;
        img.px[5] = f32::INFINITY;
        for id in filmcraft_project::mask::COLOR_PARAMS {
            for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY, -1e300, 1e300] {
                let mut m = whole(5.0, 3.0);
                m.param_mut(id).unwrap().value = filmcraft_project::ParamValue::Float(v);
                for s in [1.0, 0.0, 1e6] {
                    let c = picture_coverage(std::slice::from_ref(&m), Tick(0), s, &img).unwrap();
                    assert!(c.iter().all(|q| (0.0..=1.0).contains(q)), "{id}={v} at {s}: {c:?}");
                }
            }
        }
        for (w, h) in [(0, 0), (1, 1), (0, 4)] {
            let c = picture_coverage(&[whole(1.0, 1.0)], Tick(0), 1.0, &Image::new(w, h)).unwrap();
            assert_eq!(c.len(), w * h);
        }
    }
}

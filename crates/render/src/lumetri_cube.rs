//! A clip's Color Grading grade sampled on a 3D lattice, for Color Grading's Export .cube (#4127).
//!
//! The lattice is an image of `size³` pixels holding every grid colour as an encoded (display) RGB
//! value, so that running the grade's own CPU effect on it gives the grade of each colour, and the
//! table applied to an ungraded frame matches the render. What a table cannot hold (the vignette
//! and sharpen, which depend on the pixel's place or its neighbours, and the HSL Secondary
//! denoise and blur) is switched off for the sampling and named in the result.

use filmcraft_color::{ColorPipeline, Lut3d};
use filmcraft_project::{EffectInstance, Param, ParamValue, Project};
use filmcraft_time::Tick;

use crate::effects::{self, FxCtx, dec, enc};
use crate::image::Image;

/// The smallest and largest lattice edge: 65³ is the usual largest `.cube` size.
pub const MIN_SIZE: usize = 2;
pub const MAX_SIZE: usize = 65;

/// The sampled table and the parts of the grade a table cannot hold.
pub struct CubeSample {
    pub lut: Lut3d,
    pub left_out: Vec<&'static str>,
}

fn param_f(e: &EffectInstance, id: &str, t: Tick) -> f64 {
    e.f64_at(id, t)
}

fn set(e: &mut EffectInstance, id: &str, v: ParamValue) {
    let p = e.params.entry(id.to_string()).or_insert_with(|| Param::new(v.clone()));
    p.keyframes.clear();
    p.value = v;
}

/// `e` with what a table cannot hold switched off; adds what was on to `left_out`.
fn tabulable(e: &EffectInstance, t: Tick, left_out: &mut Vec<&'static str>) -> EffectInstance {
    let mut e = e.clone();
    let on = |e: &EffectInstance, id: &str, default: bool| e.param(id).and_then(|p| p.value.as_bool()).unwrap_or(default);
    let mut note = |name: &'static str| {
        if !left_out.contains(&name) {
            left_out.push(name);
        }
    };
    if on(&e, "vignette_on", true) && param_f(&e, "vignette_amount", t).abs() > 1e-4 {
        note("Vignette");
    }
    set(&mut e, "vignette_on", ParamValue::Bool(false));
    if on(&e, "creative_on", true) && param_f(&e, "sharpen", t).abs() > 1e-3 {
        note("Sharpen");
    }
    set(&mut e, "sharpen", ParamValue::Float(0.0));
    if on(&e, "hsl_on", false) && (param_f(&e, "hsl_denoise", t) > 0.0 || param_f(&e, "hsl_blur", t) > 0.0) {
        note("HSL Secondary Denoise and Blur");
    }
    set(&mut e, "hsl_denoise", ParamValue::Float(0.0));
    set(&mut e, "hsl_blur", ParamValue::Float(0.0));
    e
}

/// Sample the Color Grading effects `effects` (in order; disabled ones are skipped) at effect time `t`
/// on a `size³` lattice in the sequence's output encoding. Only Rec. 709 sequences without wide
/// gamut are supported: the HDR and wide-gamut pipelines grade in another signal.
pub fn sample_lumetri(
    effects: &[&EffectInstance],
    project: Option<&Project>,
    t: Tick,
    frame_rate: f64,
    pipe: &ColorPipeline,
    size: usize,
    title: &str,
) -> Result<CubeSample, String> {
    if !pipe.is_plain() {
        return Err("a .cube can be exported from a Rec. 709 sequence without wide gamut; the sequence's color settings use another pipeline".into());
    }
    if !(MIN_SIZE..=MAX_SIZE).contains(&size) {
        return Err(format!("`size` must be between {MIN_SIZE} and {MAX_SIZE}"));
    }
    if effects.iter().any(|e| e.enabled && e.effect == "lumetri" && !e.masks.is_empty()) {
        return Err("Color Grading with masks cannot be exported to a LUT: a LUT has no spatial input".into());
    }
    let mut left_out = Vec::new();
    let graded: Vec<EffectInstance> = effects.iter().filter(|e| e.enabled && e.effect == "lumetri").map(|e| tabulable(e, t, &mut left_out)).collect();
    // one row of `size` pixels per (g, b): red runs fastest, as in a .cube
    let mut img = Image::new(size, size * size);
    if img.px.len() != size * size * size * 4 {
        return Err("the lattice is too large".into());
    }
    let n = (size - 1) as f32;
    for (i, px) in img.px.as_chunks_mut::<4>().0.iter_mut().enumerate() {
        let (r, g, b) = (i % size, (i / size) % size, i / (size * size));
        let lin = dec([r as f32 / n, g as f32 / n, b as f32 / n]);
        px.copy_from_slice(&[lin[0], lin[1], lin[2], 1.0]);
    }
    let cx = FxCtx {
        t,
        px_scale: 1.0,
        mask_scale: 1.0,
        seconds: 0.0,
        timecode: "",
        clip_name: "",
        project,
        env: None,
        frame_rate: if frame_rate.is_finite() && frame_rate > 0.0 { frame_rate } else { 24.0 },
        working: pipe.working,
    };
    for e in &graded {
        effects::apply(&mut img, e, &cx);
    }
    let mut data = Vec::with_capacity(size * size * size);
    for p in img.px.as_chunks::<4>().0 {
        if !p.iter().all(|v| v.is_finite()) {
            return Err("the grade gives a value that is not a number".into());
        }
        data.push(enc([p[0], p[1], p[2]]).map(|v| v.clamp(0.0, 1.0)));
    }
    Ok(CubeSample { lut: Lut3d { title: title.to_string(), size, domain_min: [0.0; 3], domain_max: [1.0; 3], data }, left_out })
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::find_effect;

    fn lumetri(params: &[(&str, ParamValue)]) -> EffectInstance {
        let mut e = find_effect("lumetri").expect("lumetri").instance();
        for (k, v) in params {
            set(&mut e, k, v.clone());
        }
        e
    }

    fn sample(e: &[&EffectInstance], size: usize) -> CubeSample {
        sample_lumetri(e, None, Tick::ZERO, 24.0, &ColorPipeline::REC709, size, "t").expect("sample")
    }

    #[test]
    fn an_ungraded_lumetri_is_the_identity_table() {
        let e = lumetri(&[]);
        let s = sample(&[&e], 9);
        let id = Lut3d::identity(9);
        let worst = s.lut.data.iter().zip(&id.data).flat_map(|(a, b)| (0..3).map(move |k| (a[k] - b[k]).abs())).fold(0.0, f32::max);
        assert!(worst < 1.0 / 255.0, "worst error {worst}");
        assert!(s.left_out.is_empty());
    }

    /// The table applied to a colour gives what the effect gives it.
    #[test]
    fn the_table_applied_to_a_frame_gives_the_render() {
        let e = lumetri(&[
            ("exposure", ParamValue::Float(0.6)),
            ("contrast", ParamValue::Float(25.0)),
            ("saturation", ParamValue::Float(130.0)),
            ("temperature", ParamValue::Float(20.0)),
        ]);
        let s = sample(&[&e], 33);
        let cx = FxCtx {
            t: Tick::ZERO,
            px_scale: 1.0,
            mask_scale: 1.0,
            seconds: 0.0,
            timecode: "",
            clip_name: "",
            project: None,
            env: None,
            frame_rate: 24.0,
            working: ColorPipeline::REC709.working,
        };
        let mut worst = 0.0f32;
        for c in [[0.1, 0.2, 0.3], [0.5, 0.5, 0.5], [0.9, 0.4, 0.2], [0.25, 0.75, 0.6], [0.05, 0.05, 0.9]] {
            let lin = dec(c);
            let mut img = Image::filled(1, 1, [lin[0], lin[1], lin[2], 1.0]);
            effects::apply(&mut img, &e, &cx);
            let want = enc([img.px[0], img.px[1], img.px[2]]);
            let got = s.lut.apply(c);
            worst = (0..3).map(|k| (want[k].clamp(0.0, 1.0) - got[k]).abs()).fold(worst, f32::max);
        }
        assert!(worst < 3.0 / 255.0, "worst error {worst}");
        // and it is a real grade: the table is not the identity
        assert!((s.lut.apply([0.5, 0.5, 0.5])[0] - 0.5).abs() > 0.02);
    }

    #[test]
    fn what_a_table_cannot_hold_is_left_out_and_named() {
        let e = lumetri(&[("vignette_amount", ParamValue::Float(-1.5)), ("sharpen", ParamValue::Float(40.0)), ("exposure", ParamValue::Float(0.5))]);
        let s = sample(&[&e], 5);
        assert_eq!(s.left_out, ["Vignette", "Sharpen"]);
        // the corners are graded like the centre: the vignette is not in the table
        let plain = lumetri(&[("exposure", ParamValue::Float(0.5))]);
        assert_eq!(s.lut.data, sample(&[&plain], 5).lut.data);
        // a vignette that is switched off or at zero is not named
        let off = lumetri(&[("vignette_amount", ParamValue::Float(-1.5)), ("vignette_on", ParamValue::Bool(false))]);
        assert!(sample(&[&off], 5).left_out.is_empty());
    }

    #[test]
    fn disabled_effects_are_skipped_and_hostile_sizes_are_refused() {
        let mut e = lumetri(&[("exposure", ParamValue::Float(2.0))]);
        e.enabled = false;
        let s = sample(&[&e], 5);
        let id = Lut3d::identity(5);
        let worst = s.lut.data.iter().zip(&id.data).flat_map(|(a, b)| (0..3).map(move |k| (a[k] - b[k]).abs())).fold(0.0, f32::max);
        assert!(worst < 1.0 / 255.0, "a disabled Color Grading grades nothing: {worst}");
        for bad in [0, 1, MAX_SIZE + 1, usize::MAX] {
            assert!(sample_lumetri(&[&e], None, Tick::ZERO, 24.0, &ColorPipeline::REC709, bad, "t").is_err(), "{bad}");
        }
        let wide = ColorPipeline { wide_gamut: true, ..ColorPipeline::REC709 };
        assert!(sample_lumetri(&[&e], None, Tick::ZERO, 24.0, &wide, 5, "t").is_err());
    }
}

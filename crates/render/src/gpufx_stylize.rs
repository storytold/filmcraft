//! Typed GPU operations for simple affine and alpha-edge effects.
use filmcraft_geom::Affine;
use filmcraft_project::EffectInstance;

use crate::{effects::FxCtx, image::Image};

#[derive(Clone, Debug, PartialEq)]
pub enum StylizeOp {
    Place { inv: [f32; 6] },
    BevelAlpha { thickness: f32, lx: f32, ly: f32, intensity: f32 },
}

impl StylizeOp {
    pub fn eval(e: &EffectInstance, cx: &FxCtx, w: usize, h: usize) -> Option<Self> {
        if !e.enabled || w == 0 || h == 0 || w > 16_384 || h > 16_384 {
            return None;
        }
        // These ops need the clip envelope; without it progress freezes at 0, so refuse the GPU route.
        if cx.env.is_none() && matches!(e.effect.as_str(), "grow" | "shrink" | "spin" | "move") {
            return None;
        }
        let img = Image { w, h, px: Vec::new() };
        let progress = cx.env.map_or(0.0, |env| {
            let d = env.clip_seconds();
            if d <= 1e-9 { 0.0 } else { (cx.seconds / d).clamp(0.0, 1.0) }
        });
        let ease = |kind: u32, t: f64| match kind {
            1 => t * t,
            2 => 1.0 - (1.0 - t) * (1.0 - t),
            3 => t * t * (3.0 - 2.0 * t),
            _ => t,
        };
        let center = |id: &str| crate::vfx::pv(e, id, cx, &img);
        let offset = |id: &str| crate::vfx::offv(e, id, cx);
        let transform = |c: filmcraft_geom::Vec2, off: filmcraft_geom::Vec2, scale: f64, angle: f64| {
            let m = Affine::translate(c.x + off.x, c.y + off.y)
                .then_apply(&Affine::rotate_deg(angle))
                .then_apply(&Affine::scale(scale, scale))
                .then_apply(&Affine::translate(-c.x, -c.y));
            let inv = m.inverse()?;
            Some(Self::Place { inv: [inv.a as f32, inv.b as f32, inv.c as f32, inv.d as f32, inv.e as f32, inv.f as f32] })
        };
        match e.effect.as_str() {
            "grow" | "shrink" => {
                let p = ease(crate::vfx::chv(e, "easing"), progress);
                let from = crate::vfx::fv(e, "from", cx) as f64;
                let to = crate::vfx::fv(e, "to", cx) as f64;
                transform(center("center"), filmcraft_geom::Vec2::ZERO, ((from + (to - from) * p) / 100.0).max(1e-4), 0.0)
            }
            "spin" => {
                let p = ease(crate::vfx::chv(e, "easing"), progress);
                transform(
                    center("center"),
                    filmcraft_geom::Vec2::ZERO,
                    (crate::vfx::fv(e, "scale", cx) as f64 / 100.0).max(1e-4),
                    crate::vfx::fv(e, "amount", cx) as f64 * p,
                )
            }
            "move" => {
                if crate::vfx::fv(e, "motion_blur", cx) > 0.0 {
                    return None;
                }
                let p = ease(crate::vfx::chv(e, "easing"), progress);
                let from = offset("from");
                let to = offset("to");
                let off = from + (to - from) * p;
                transform(filmcraft_geom::Vec2::new(w as f64 / 2.0, h as f64 / 2.0), off, 1.0, 0.0)
            }
            "wiggle" => {
                let (off, scale, angle) = crate::vfx::wiggle_place(e, cx);
                transform(filmcraft_geom::Vec2::new(w as f64 / 2.0, h as f64 / 2.0), off, scale, angle)
            }
            "camera_shake" => {
                // The motion-blurred form averages five placements, which has no single-pass GPU op.
                if crate::vfx::fv(e, "motion_blur", cx) > 0.0 {
                    return None;
                }
                let (off, scale, angle) = crate::vfx::camera_shake_place(e, cx, cx.seconds);
                transform(filmcraft_geom::Vec2::new(w as f64 / 2.0, h as f64 / 2.0), off, scale, angle)
            }
            "bevel_alpha" => Some(Self::BevelAlpha {
                thickness: crate::vfx::fv(e, "thickness", cx) * cx.px_scale,
                lx: crate::vfx::fv(e, "angle", cx).to_radians().cos(),
                ly: -crate::vfx::fv(e, "angle", cx).to_radians().sin(),
                intensity: crate::vfx::fv(e, "intensity", cx) / 100.0,
            }),
            _ => None,
        }
    }

    pub fn gpu_ok(&self) -> bool {
        match self {
            Self::Place { inv } => {
                let inverse_scale = inv[0].hypot(inv[1]);
                inv.iter().all(|v| v.is_finite())
                    && inv.iter().all(|v| v.abs() <= 1_000_000.0)
                    && (inv[0] * inv[3] - inv[1] * inv[2]).abs() > 1e-12
                    // The CPU recursively box-prefilters at this threshold.
                    && inverse_scale < 2.0
            }
            Self::BevelAlpha { thickness, lx, ly, intensity } => {
                [*thickness, *lx, *ly, *intensity].iter().all(|v| v.is_finite()) && thickness.abs() <= 16_384.0
            }
        }
    }

    pub fn matches_id(&self, id: &str) -> bool {
        match self {
            Self::Place { .. } => matches!(id, "grow" | "shrink" | "move" | "spin" | "wiggle" | "camera_shake"),
            Self::BevelAlpha { .. } => id == "bevel_alpha",
        }
    }

    /// Returns opcode, integer words, and six vec4 float payloads.
    pub fn pack(&self) -> (u32, [u32; 4], [f32; 24]) {
        let mut p = [0.0; 24];
        match self {
            Self::Place { inv } => p[..6].copy_from_slice(inv),
            Self::BevelAlpha { thickness, lx, ly, intensity } => p[..4].copy_from_slice(&[*thickness, *lx, *ly, *intensity]),
        }
        let opcode = match self {
            Self::Place { .. } => 128,
            Self::BevelAlpha { .. } => 129,
        };
        (opcode, [0; 4], p)
    }

    /// CPU oracle for the exact single-pass operation.
    pub fn apply(&self, img: &mut Image) {
        match self {
            Self::Place { inv } => {
                let m = Affine { a: inv[0] as f64, b: inv[1] as f64, c: inv[2] as f64, d: inv[3] as f64, e: inv[4] as f64, f: inv[5] as f64 };
                let mut out = Image::new(img.w, img.h);
                for y in 0..img.h {
                    for x in 0..img.w {
                        let px = x as f64 + 0.5;
                        let py = y as f64 + 0.5;
                        let q = img.sample_bilinear((m.a * px + m.c * py + m.e) as f32, (m.b * px + m.d * py + m.f) as f32);
                        let i = (y * img.w + x) * 4;
                        out.px[i..i + 4].copy_from_slice(&q);
                    }
                }
                *img = out;
            }
            Self::BevelAlpha { thickness, lx, ly, intensity } => {
                let src = img.clone();
                for y in 0..img.h {
                    for x in 0..img.w {
                        let a = |dx, dy| src.sample_bilinear_clamped(x as f32 + 0.5 + dx, y as f32 + 0.5 + dy)[3];
                        let shade = -((a(*thickness, 0.0) - a(-*thickness, 0.0)) * lx + (a(0.0, *thickness) - a(0.0, -*thickness)) * ly) * intensity;
                        let i = (y * img.w + x) * 4;
                        let alpha = img.px[i + 3];
                        for k in 0..3 {
                            img.px[i + k] = (img.px[i + k] + shade * alpha).clamp(0.0, alpha);
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::StylizeOp;
    use crate::{effects::FxCtx, image::Image};
    use filmcraft_geom::Vec2;
    use filmcraft_project::{ParamValue, find_effect};
    use filmcraft_time::Tick;

    struct ClipEnv;

    impl crate::vfx::FxEnv for ClipEnv {
        fn frame(&self, _: f64, _: &filmcraft_project::EffectInstance) -> Option<Image> {
            None
        }
        fn source_frame(&self, _: f64, _: f32) -> Option<Image> {
            None
        }
        fn track(&self, _: usize) -> Option<Image> {
            None
        }
        fn layer_to_output(&self) -> filmcraft_geom::Affine {
            filmcraft_geom::Affine::translate(0.0, 0.0)
        }
        fn clip_seconds(&self) -> f64 {
            10.0
        }
        fn clip_offset(&self) -> f64 {
            0.0
        }
        fn frame_rate(&self) -> f64 {
            24.0
        }
        fn media_timecode(&self) -> String {
            String::new()
        }
        fn file_name(&self) -> String {
            String::new()
        }
        fn sequence_name(&self) -> String {
            String::new()
        }
        fn sequence_size(&self) -> (u32, u32) {
            (7, 5)
        }
        fn source_size(&self) -> (u32, u32) {
            (7, 5)
        }
        fn clip_key(&self) -> u64 {
            0
        }
    }

    static CLIP: ClipEnv = ClipEnv;

    fn context() -> FxCtx<'static> {
        FxCtx {
            t: Tick::ZERO,
            px_scale: 1.0,
            seconds: 0.0,
            timecode: "",
            clip_name: "",
            project: None,
            env: Some(&CLIP),
            frame_rate: 24.0,
            working: filmcraft_color::WorkingSpace::Rec709,
        }
    }

    #[test]
    fn clip_timed_stylize_ops_refuse_gpu_route_without_clip_envelope() {
        for id in ["grow", "shrink", "move", "spin"] {
            let Some(def) = find_effect(id) else { panic!("missing registered effect {id}") };
            let effect = def.instance();
            let no_env = FxCtx { env: None, ..context() };
            assert!(StylizeOp::eval(&effect, &no_env, 7, 5).is_none(), "{id}: frozen GPU op without env");
            assert!(StylizeOp::eval(&effect, &context(), 7, 5).is_some(), "{id}: no GPU op with env");
        }
        for id in ["wiggle", "camera_shake"] {
            let Some(def) = find_effect(id) else { panic!("missing registered effect {id}") };
            let effect = def.instance();
            let no_env = FxCtx { env: None, ..context() };
            assert!(StylizeOp::eval(&effect, &no_env, 7, 5).is_some(), "{id}: time-driven via seconds");
        }
    }

    fn set(e: &mut filmcraft_project::EffectInstance, id: &str, value: ParamValue) {
        if let Some(param) = e.params.get_mut(id) {
            param.value = value;
        }
    }

    fn picture() -> Image {
        let mut image = Image::new(7, 5);
        for y in 0..image.h {
            for x in 0..image.w {
                let a = if (x + 2 * y) % 4 == 0 {
                    0.0
                } else if x == 2 {
                    0.45
                } else {
                    1.0
                };
                let i = (y * image.w + x) * 4;
                image.px[i..i + 4].copy_from_slice(&[0.2 * a, 0.5 * a, 0.8 * a, a]);
            }
        }
        image
    }

    fn bevel_alpha_reference(img: &mut Image, effect: &filmcraft_project::EffectInstance, cx: &FxCtx<'_>) {
        let thickness = crate::vfx::fv(effect, "thickness", cx) * cx.px_scale;
        let angle = (crate::vfx::fv(effect, "angle", cx) as f64).to_radians();
        let intensity = crate::vfx::fv(effect, "intensity", cx) / 100.0;
        let (light_x, light_y) = (angle.cos() as f32, -angle.sin() as f32);
        let source = img.clone();
        for y in 0..img.h {
            for x in 0..img.w {
                let sample_alpha = |dx, dy| source.sample_bilinear_clamped(x as f32 + 0.5 + dx, y as f32 + 0.5 + dy)[3];
                let grad_x = sample_alpha(thickness, 0.0) - sample_alpha(-thickness, 0.0);
                let grad_y = sample_alpha(0.0, thickness) - sample_alpha(0.0, -thickness);
                let shade = -(grad_x * light_x + grad_y * light_y) * intensity;
                let i = (y * img.w + x) * 4;
                let alpha = img.px[i + 3];
                for channel in 0..3 {
                    img.px[i + channel] = (img.px[i + channel] + shade * alpha).clamp(0.0, alpha);
                }
            }
        }
    }

    #[test]
    fn implemented_stylize_ops_match_cpu_oracle() {
        for id in ["grow", "shrink", "move", "spin", "wiggle", "camera_shake"] {
            let Some(def) = find_effect(id) else { panic!("missing registered effect {id}") };
            let mut effect = def.instance();
            match id {
                "grow" | "shrink" => {
                    set(&mut effect, "from", ParamValue::Float(120.0));
                    set(&mut effect, "to", ParamValue::Float(135.0));
                }
                "move" => {
                    set(&mut effect, "from", ParamValue::Vec2(Vec2::new(-1.25, 0.5)));
                    set(&mut effect, "to", ParamValue::Vec2(Vec2::new(3.0, -2.0)));
                }
                "spin" => {
                    set(&mut effect, "amount", ParamValue::Float(35.0));
                }
                _ => {}
            }
            let cx = context();
            let Some(op) = StylizeOp::eval(&effect, &cx, 7, 5) else { panic!("{id} not evaluated") };
            assert!(op.gpu_ok(), "{id}");
            let mut expected = picture();
            let mut actual = expected.clone();
            assert!(crate::vfx::apply(&mut expected, &effect, &cx), "{id}: missing native vfx oracle");
            op.apply(&mut actual);
            for (a, b) in actual.px.iter().zip(expected.px) {
                assert!((a - b).abs() < 2e-5, "{id}: {a} != {b}");
            }
        }
    }

    #[test]
    fn bevel_alpha_matches_independent_reference_for_default_and_custom_parameters() {
        for custom in [false, true] {
            let Some(def) = find_effect("bevel_alpha") else { panic!("missing registered effect bevel_alpha") };
            let mut effect = def.instance();
            if custom {
                set(&mut effect, "thickness", ParamValue::Float(1.5));
                set(&mut effect, "angle", ParamValue::Float(37.0));
                set(&mut effect, "intensity", ParamValue::Float(70.0));
            }
            let cx = context();
            let Some(op) = StylizeOp::eval(&effect, &cx, 7, 5) else { panic!("bevel_alpha not evaluated") };
            assert!(op.gpu_ok(), "bevel_alpha");
            let mut expected = picture();
            let mut actual = expected.clone();
            bevel_alpha_reference(&mut expected, &effect, &cx);
            op.apply(&mut actual);
            for (a, b) in actual.px.iter().zip(expected.px) {
                assert!((a - b).abs() < 2e-5, "bevel_alpha custom={custom}: {a} != {b}");
            }
        }
    }
}

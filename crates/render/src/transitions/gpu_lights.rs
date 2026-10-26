//! Typed GPU light and blur transition parameters.
//!
//! Gaussian `glow` and the intermediate-frame `ray` need multiple full-frame stages;
//! they remain unsupported until the transition coordinator adds those resources.

use filmcraft_project::{EffectInstance, EffectKind, ParamValue, find_effect};
use filmcraft_time::Tick;

use super::TransitionPlanError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum LightKind {
    BurnChroma = 64,
    ChromaLeak = 65,
    CrossZoom = 66,
    DirectionalBlur = 67,
    Flash = 68,
    LensBlur = 69,
    LightLeak = 70,
    LightSweep = 71,
    Phosphor = 72,
    RadialBlur = 73,
    Solarize = 74,
    Stripe = 75,
    ZoomBlur = 76,
    Glow = 77,
    Ray = 78,
    CrossZoomLegacy = 79,
}

/// Fully evaluated family op. Color values use linear light; pixel lengths and center include scale.
#[derive(Clone, Debug, PartialEq)]
pub struct LightOp {
    pub kind: LightKind,
    pub progress: f32,
    pub amount: f32,
    pub angle_radians: f32,
    pub color_linear: [f32; 4],
    pub center: [f32; 2],
    pub center_set: [bool; 2],
    pub radius: f32,
    pub strength: f32,
    pub intensity: f32,
    pub width: f32,
    pub stripes: f32,
    pub length: f32,
    pub decay: f32,
    pub seed: u32,
}

impl LightOp {
    pub fn matches_id(id: &str) -> bool {
        matches!(
            id,
            "burn_chroma"
                | "chroma_leak"
                | "cross_zoom"
                | "directional_blur_transition"
                | "flash"
                | "lens_blur"
                | "light_leak"
                | "light_sweep"
                | "phosphor"
                | "radial_blur"
                | "solarize"
                | "stripe"
                | "zoom_blur"
                | "glow"
                | "ray"
                | "cross_zoom_legacy"
        )
    }

    pub fn eval(e: &EffectInstance, progress: f32, scale: f32) -> Result<Option<Self>, TransitionPlanError> {
        let id = e.effect.clone();
        let Some(def) = find_effect(&id).filter(|d| d.kind == EffectKind::VideoTransition) else {
            return Err(TransitionPlanError::UnknownId { id });
        };
        let Some(kind) = Self::kind(&id) else { return Ok(None) };
        for pd in &def.params {
            let v = e.param(pd.id).map(|p| p.value_at(Tick::ZERO)).unwrap_or_else(|| pd.default.clone());
            let valid = match v {
                ParamValue::Float(v) => v.is_finite() && (v as f32).is_finite(),
                ParamValue::Color(v) => v.iter().all(|x| x.is_finite()),
                ParamValue::Vec2(v) => [v.x, v.y].iter().all(|x| x.is_nan() || (x.is_finite() && (*x as f32).is_finite())),
                ParamValue::Choice(_) | ParamValue::Bool(_) => true,
                _ => false,
            };
            if !valid {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: pd.id });
            }
        }
        let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
        let num = |name: &str| e.f64_at(name, filmcraft_time::Tick::ZERO) as f32;
        let color = |name: &str| {
            let c = e
                .param(name)
                .map(|p| p.value_at(Tick::ZERO))
                .and_then(|v| match v {
                    ParamValue::Color(c) => Some(c),
                    _ => None,
                })
                .or_else(|| {
                    def.param(name).and_then(|p| match &p.default {
                        ParamValue::Color(c) => Some(*c),
                        _ => None,
                    })
                })
                .unwrap_or([1.0; 4]);
            [c[0], c[1], c[2]].map(|v| filmcraft_color::srgb_to_linear(v.clamp(0.0, 1.0)))
        };
        let center = e
            .param("center")
            .map(|p| p.value_at(Tick::ZERO))
            .and_then(|v| match v {
                ParamValue::Vec2(v) => Some([v.x as f32, v.y as f32]),
                _ => None,
            })
            .or_else(|| {
                def.param("center").and_then(|p| match &p.default {
                    ParamValue::Vec2(v) => Some([v.x as f32, v.y as f32]),
                    _ => None,
                })
            })
            .unwrap_or([f32::NAN; 2]);
        let center_set = [center[0].is_finite(), center[1].is_finite()];
        let center = [if center_set[0] { center[0] * scale } else { 0.0 }, if center_set[1] { center[1] * scale } else { 0.0 }];
        let radians = num("angle").to_radians();
        let mut rgb = color("color");
        if id == "chroma_leak" || id == "burn_chroma" || id == "solarize" {
            rgb = [0.0; 3];
        }
        let op = Self {
            kind,
            progress: if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 },
            amount: num("amount") / 100.0,
            angle_radians: radians,
            color_linear: [rgb[0], rgb[1], rgb[2], 1.0],
            center,
            center_set,
            radius: num("radius").max(0.0) * scale,
            strength: num("strength") / 100.0,
            intensity: (num("intensity") / 100.0).clamp(0.0, 1.0),
            width: if id == "directional_blur_transition" { num("blur").max(0.0) * scale } else { num("width").max(0.0) * scale },
            stripes: num("stripes").round().clamp(1.0, 64.0),
            length: num("length") / 100.0,
            decay: num("decay") / 100.0,
            seed: num("seed").max(0.0) as u32,
        };
        if !op.gpu_ok() {
            return Err(TransitionPlanError::InvalidParameter { id, parameter: "scale" });
        }
        Ok(Some(op))
    }

    fn kind(id: &str) -> Option<LightKind> {
        Some(match id {
            "burn_chroma" => LightKind::BurnChroma,
            "chroma_leak" => LightKind::ChromaLeak,
            "cross_zoom" => LightKind::CrossZoom,
            "directional_blur_transition" => LightKind::DirectionalBlur,
            "flash" => LightKind::Flash,
            "lens_blur" => LightKind::LensBlur,
            "light_leak" => LightKind::LightLeak,
            "light_sweep" => LightKind::LightSweep,
            "phosphor" => LightKind::Phosphor,
            "radial_blur" => LightKind::RadialBlur,
            "solarize" => LightKind::Solarize,
            "stripe" => LightKind::Stripe,
            "zoom_blur" => LightKind::ZoomBlur,
            "glow" => LightKind::Glow,
            "ray" => LightKind::Ray,
            "cross_zoom_legacy" => LightKind::CrossZoomLegacy,
            _ => return None,
        })
    }

    pub fn gpu_ok(&self) -> bool {
        [self.progress, self.amount, self.angle_radians, self.radius, self.strength, self.intensity, self.width, self.stripes, self.length, self.decay]
            .iter()
            .all(|v| v.is_finite())
            && self.color_linear.iter().all(|v| v.is_finite())
            && (0..2).all(|i| !self.center_set[i] || self.center[i].is_finite())
    }

    /// Words 0 and 1 are progress and kernel; words 12..31 map to extra0..extra4.
    pub fn pack(&self) -> [u32; 32] {
        let mut w = [0; 32];
        w[0] = self.progress.to_bits();
        w[1] = self.kind as u32;
        for (i, v) in [
            self.angle_radians,
            self.amount,
            self.strength,
            self.intensity,
            self.width,
            self.radius,
            self.stripes,
            self.length,
            self.decay,
            self.center[0],
            self.center[1],
            self.color_linear[0],
            self.color_linear[1],
            self.color_linear[2],
        ]
        .into_iter()
        .enumerate()
        {
            w[i + 12] = v.to_bits();
        }
        w[26] = self.seed;
        w[27] = u32::from(self.center_set[0]) | (u32::from(self.center_set[1]) << 1);
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::find_effect;

    #[test]
    fn supported_defaults_and_bounds_pack_finitely() {
        for id in [
            "burn_chroma",
            "chroma_leak",
            "cross_zoom",
            "directional_blur_transition",
            "flash",
            "lens_blur",
            "light_leak",
            "light_sweep",
            "phosphor",
            "radial_blur",
            "solarize",
            "stripe",
            "zoom_blur",
        ] {
            let e = find_effect(id).expect("registered id").instance();
            let op = LightOp::eval(&e, 0.5, 0.5).expect("valid default").expect("family op");
            assert!(op.gpu_ok(), "{id}");
            assert_eq!(op.pack()[1], op.kind as u32);
            assert_eq!(op.pack()[0], 0.5f32.to_bits());
        }
    }

    #[test]
    fn cpu_oracle_cases_cover_odd_frames_alpha_and_max_strength() {
        let a = crate::image::Image::filled(7, 5, [0.24, 0.08, 0.03, 0.4]);
        let b = crate::image::Image::filled(7, 5, [0.02, 0.18, 0.31, 0.8]);
        for id in [
            "burn_chroma",
            "chroma_leak",
            "cross_zoom",
            "directional_blur_transition",
            "flash",
            "lens_blur",
            "light_leak",
            "light_sweep",
            "phosphor",
            "radial_blur",
            "solarize",
            "stripe",
            "zoom_blur",
        ] {
            let mut e = find_effect(id).expect("registered id").instance();
            if let Some(strength) = e.params.get_mut("strength") {
                strength.value = ParamValue::Float(100.0);
            }
            let first = crate::transitions::apply_scaled(&e, &a, &b, 0.43, 0.5);
            let repeat = crate::transitions::apply_scaled(&e, &a, &b, 0.43, 0.5);
            assert_eq!(first, repeat, "{id} deterministic CPU oracle");
            assert!(first.px.iter().all(|v| v.is_finite()), "{id} finite CPU oracle");
        }
    }

    #[test]
    fn missing_parameters_use_registry_defaults() {
        let mut e = find_effect("light_leak").expect("registered id").instance();
        let expected = LightOp::eval(&e, 0.5, 1.0).expect("valid");
        e.params.clear();
        assert_eq!(LightOp::eval(&e, 0.5, 1.0).expect("registry defaults"), expected);
    }

    #[test]
    fn flash_intensity_matches_cpu_clamp_and_stripe_angles_pack() {
        let (a, b) = (crate::image::Image::filled(7, 5, [0.2, 0.1, 0.0, 0.5]), crate::image::Image::filled(7, 5, [0.0, 0.2, 0.3, 0.75]));
        let mut flash = find_effect("flash").expect("registered id").instance();
        flash.params.get_mut("intensity").expect("intensity param").value = ParamValue::Float(200.0);
        let clipped = LightOp::eval(&flash, 0.5, 1.0).expect("valid flash").expect("light op");
        assert_eq!(clipped.intensity, 1.0);
        let over_range = crate::transitions::apply_scaled(&flash, &a, &b, 0.5, 1.0);
        flash.params.get_mut("intensity").expect("intensity param").value = ParamValue::Float(100.0);
        assert_eq!(over_range, crate::transitions::apply_scaled(&flash, &a, &b, 0.5, 1.0));

        let mut stripe = find_effect("stripe").expect("registered id").instance();
        for (angle, expected) in [(0.0, 0.0), (90.0, std::f32::consts::FRAC_PI_2)] {
            stripe.params.get_mut("angle").expect("angle param").value = ParamValue::Float(angle);
            let op = LightOp::eval(&stripe, 0.5, 1.0).expect("valid stripe").expect("light op");
            assert!((op.angle_radians - expected).abs() < 1e-6);
        }
    }

    #[test]
    fn multipass_defaults_have_dedicated_kernels() {
        for (id, kernel) in [("glow", LightKind::Glow), ("ray", LightKind::Ray), ("cross_zoom_legacy", LightKind::CrossZoomLegacy)] {
            let e = find_effect(id).expect("registered id").instance();
            let op = LightOp::eval(&e, 0.5, 1.0).expect("valid default").expect("family op");
            assert_eq!(op.kind, kernel);
            assert!(op.gpu_ok());
        }
    }
}

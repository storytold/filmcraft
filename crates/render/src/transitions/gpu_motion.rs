//! Typed payloads for motion transition kernels implemented in WGSL.

use filmcraft_project::{EffectInstance, EffectKind, ParamKind, ParamValue, find_effect};
use filmcraft_time::Tick;

use super::TransitionPlanError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum MotionKind {
    Pop = 35,
    Wave = 50,
    Louver = 51,
    Mirror = 52,
    Pull = 53,
    Travel = 54,
    Spin = 55,
    Roll3d = 56,
    Block = 57,
    Spring = 38,
    Tween = 48,
    Slide = 43,
    Split = 44,
    Stretch = 45,
    Roll = 42,
    FilmRoll = 41,
    Camera = 47,
    PageTurn = 59,
    PushLegacy = 60,
    SlideLegacy = 61,
    SplitLegacy = 62,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum MotionPayload {
    LegacyPush { direction: u32 },
    LegacySlide { direction: u32 },
    LegacySplit,
    LegacyPageTurn,
    Camera { zoom_percent: f32, rotation_degrees: f32, blur_percent: f32 },
    Roll { direction: u32, blur_percent: f32 },
    FilmRoll { direction: u32, gap_pixels: f32, blur_percent: f32, gap_color_linear: [f32; 4] },
    Stretch { direction: u32 },
    Slide { direction: u32, blur_percent: f32 },
    Split { orientation: u32, blur_percent: f32 },
    Tween { scale_percent: f32 },
    Spring { direction: u32, bounce_percent: f32, blur_percent: f32 },
    Pop { overshoot_percent: f32, blur_percent: f32 },
    Wave { direction: u32, amplitude_pixels: f32, wavelength_pixels: f32 },
    Louver { orientation: u32, slats: f32 },
    Mirror { direction: u32 },
    Pull { direction: u32, blur_percent: f32 },
    Travel { direction: u32, zoom_percent: f32, blur_percent: f32 },
    Spin { rotation_degrees: f32, spin_direction: u32, blur_percent: f32 },
    Roll3d { direction: u32, perspective_percent: f32 },
    Block { direction: u32, blocks: f32, blur_percent: f32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MotionOp {
    pub kind: MotionKind,
    pub payload: MotionPayload,
    pub progress: f32,
}

pub fn matches_id(id: &str) -> bool {
    matches!(
        id,
        "push_legacy"
            | "slide_legacy"
            | "split_legacy"
            | "page_turn"
            | "motion_camera"
            | "roll"
            | "film_roll"
            | "stretch"
            | "slide"
            | "split"
            | "motion_tween"
            | "spring_motion"
            | "pop_motion"
            | "wave"
            | "louver"
            | "mirror_transition"
            | "pull_motion"
            | "travel_motion"
            | "spin_motion"
            | "roll_3d"
            | "block_motion"
    )
}

pub fn gpu_ok(op: &MotionOp) -> bool {
    matches!(
        op.payload,
        MotionPayload::LegacyPush { .. }
            | MotionPayload::LegacySlide { .. }
            | MotionPayload::LegacySplit
            | MotionPayload::LegacyPageTurn
            | MotionPayload::Camera { .. }
            | MotionPayload::Roll { .. }
            | MotionPayload::FilmRoll { .. }
            | MotionPayload::Stretch { .. }
            | MotionPayload::Slide { .. }
            | MotionPayload::Split { .. }
            | MotionPayload::Tween { .. }
            | MotionPayload::Spring { .. }
            | MotionPayload::Pop { .. }
            | MotionPayload::Wave { .. }
            | MotionPayload::Louver { .. }
            | MotionPayload::Mirror { .. }
            | MotionPayload::Pull { .. }
            | MotionPayload::Travel { .. }
            | MotionPayload::Spin { .. }
            | MotionPayload::Roll3d { .. }
            | MotionPayload::Block { .. }
    )
}

/// Evaluates registry defaults and validates fields consumed by the selected shader route.
pub fn eval(e: &EffectInstance, progress: f32, scale: f32) -> Result<Option<MotionOp>, TransitionPlanError> {
    let id = e.effect.clone();
    let Some(def) = find_effect(&id).filter(|d| d.kind == EffectKind::VideoTransition) else {
        return Err(TransitionPlanError::UnknownId { id });
    };
    let kind = match id.as_str() {
        "push_legacy" => MotionKind::PushLegacy,
        "slide_legacy" => MotionKind::SlideLegacy,
        "split_legacy" => MotionKind::SplitLegacy,
        "page_turn" => MotionKind::PageTurn,
        "motion_camera" => MotionKind::Camera,
        "roll" => MotionKind::Roll,
        "film_roll" => MotionKind::FilmRoll,
        "stretch" => MotionKind::Stretch,
        "slide" => MotionKind::Slide,
        "split" => MotionKind::Split,
        "motion_tween" => MotionKind::Tween,
        "spring_motion" => MotionKind::Spring,
        "pop_motion" => MotionKind::Pop,
        "wave" => MotionKind::Wave,
        "louver" => MotionKind::Louver,
        "mirror_transition" => MotionKind::Mirror,
        "pull_motion" => MotionKind::Pull,
        "travel_motion" => MotionKind::Travel,
        "spin_motion" => MotionKind::Spin,
        "roll_3d" => MotionKind::Roll3d,
        "block_motion" => MotionKind::Block,
        _ => return Ok(None),
    };
    for pd in &def.params {
        let v = e.param(pd.id).map(|p| p.value_at(Tick::ZERO)).unwrap_or_else(|| pd.default.clone());
        let valid = match (&pd.kind, v) {
            (ParamKind::Choice(options), ParamValue::Choice(n)) => (n as usize) < options.len(),
            (ParamKind::Float { min, max, .. }, ParamValue::Float(n)) => n.is_finite() && n >= *min && n <= *max && (n as f32).is_finite(),
            (ParamKind::Angle, ParamValue::Float(n)) => n.is_finite() && (n as f32).is_finite(),
            (ParamKind::Color, ParamValue::Color(c)) => c.iter().all(|n| n.is_finite()),
            _ => false,
        };
        if !valid {
            return Err(TransitionPlanError::InvalidParameter { id, parameter: pd.id });
        }
    }
    let direction = if def.param("direction").is_none() {
        0
    } else {
        match e.param("direction").map(|p| &p.value).or_else(|| def.param("direction").map(|p| &p.default)) {
            Some(ParamValue::Choice(v)) => *v,
            _ => return Err(TransitionPlanError::InvalidParameter { id, parameter: "direction" }),
        }
    };
    let orientation = match e.param("orientation").map(|p| &p.value).or_else(|| def.param("orientation").map(|p| &p.default)) {
        Some(ParamValue::Choice(v)) => *v,
        _ => 0,
    };
    let payload = match kind {
        MotionKind::PushLegacy => MotionPayload::LegacyPush { direction },
        MotionKind::SlideLegacy => MotionPayload::LegacySlide { direction },
        MotionKind::SplitLegacy => MotionPayload::LegacySplit,
        MotionKind::PageTurn => MotionPayload::LegacyPageTurn,
        MotionKind::Camera => {
            let zoom = e.f64_at("zoom", Tick::ZERO) as f32;
            let rotation = e.f64_at("rotation", Tick::ZERO) as f32;
            let blur = e.f64_at("motion_blur", Tick::ZERO) as f32;
            if ![zoom, rotation, blur].iter().all(|v| v.is_finite()) {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "motion_camera" });
            }
            MotionPayload::Camera { zoom_percent: zoom, rotation_degrees: rotation, blur_percent: blur }
        }
        MotionKind::Roll => MotionPayload::Roll { direction, blur_percent: e.f64_at("motion_blur", Tick::ZERO) as f32 },
        MotionKind::FilmRoll => {
            let gap_pixels = e.f64_at("gap", Tick::ZERO) as f32 * if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
            let blur_percent = e.f64_at("motion_blur", Tick::ZERO) as f32;
            let color = e
                .param("gap_color")
                .map(|p| p.value_at(Tick::ZERO))
                .and_then(|v| match v {
                    ParamValue::Color(c) => Some(c),
                    _ => None,
                })
                .or_else(|| {
                    def.param("gap_color").and_then(|p| match &p.default {
                        ParamValue::Color(c) => Some(*c),
                        _ => None,
                    })
                })
                .ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "gap_color" })?;
            if !gap_pixels.is_finite() || !blur_percent.is_finite() || !color.iter().all(|v| v.is_finite()) {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "film_roll" });
            }
            MotionPayload::FilmRoll {
                direction,
                gap_pixels,
                blur_percent,
                gap_color_linear: [
                    filmcraft_color::srgb_to_linear(color[0].clamp(0.0, 1.0)),
                    filmcraft_color::srgb_to_linear(color[1].clamp(0.0, 1.0)),
                    filmcraft_color::srgb_to_linear(color[2].clamp(0.0, 1.0)),
                    1.0,
                ],
            }
        }
        MotionKind::Stretch => MotionPayload::Stretch { direction },
        MotionKind::Slide => MotionPayload::Slide { direction, blur_percent: e.f64_at("motion_blur", Tick::ZERO) as f32 },
        MotionKind::Split => MotionPayload::Split { orientation, blur_percent: e.f64_at("motion_blur", Tick::ZERO) as f32 },
        MotionKind::Tween => MotionPayload::Tween { scale_percent: e.f64_at("scale", Tick::ZERO) as f32 },
        MotionKind::Spring => {
            MotionPayload::Spring { direction, bounce_percent: e.f64_at("bounce", Tick::ZERO) as f32, blur_percent: e.f64_at("motion_blur", Tick::ZERO) as f32 }
        }
        MotionKind::Pop => {
            MotionPayload::Pop { overshoot_percent: e.f64_at("overshoot", Tick::ZERO) as f32, blur_percent: e.f64_at("motion_blur", Tick::ZERO) as f32 }
        }
        MotionKind::Wave => {
            let unit_scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
            let amplitude_pixels = e.f64_at("amplitude", Tick::ZERO) as f32 * unit_scale;
            let wavelength_pixels = (e.f64_at("wavelength", Tick::ZERO) as f32 * unit_scale).max(4.0);
            if !amplitude_pixels.is_finite() || !wavelength_pixels.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "wave" });
            }
            MotionPayload::Wave { direction, amplitude_pixels, wavelength_pixels }
        }
        MotionKind::Louver => MotionPayload::Louver { orientation, slats: e.f64_at("slats", Tick::ZERO) as f32 },
        MotionKind::Mirror => MotionPayload::Mirror { direction },
        MotionKind::Pull => MotionPayload::Pull { direction, blur_percent: e.f64_at("motion_blur", Tick::ZERO) as f32 },
        MotionKind::Travel => {
            MotionPayload::Travel { direction, zoom_percent: e.f64_at("zoom", Tick::ZERO) as f32, blur_percent: e.f64_at("motion_blur", Tick::ZERO) as f32 }
        }
        MotionKind::Spin => MotionPayload::Spin {
            rotation_degrees: e.f64_at("rotation", Tick::ZERO) as f32,
            spin_direction: match e.param("spin").map(|p| &p.value).or_else(|| def.param("spin").map(|p| &p.default)) {
                Some(ParamValue::Choice(v)) => *v,
                _ => return Err(TransitionPlanError::InvalidParameter { id, parameter: "spin" }),
            },
            blur_percent: e.f64_at("motion_blur", Tick::ZERO) as f32,
        },
        MotionKind::Roll3d => MotionPayload::Roll3d { direction, perspective_percent: e.f64_at("perspective", Tick::ZERO) as f32 },
        MotionKind::Block => {
            MotionPayload::Block { direction, blocks: e.f64_at("blocks", Tick::ZERO) as f32, blur_percent: e.f64_at("motion_blur", Tick::ZERO) as f32 }
        }
    };
    let _scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    Ok(Some(MotionOp { kind, payload, progress: if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 } }))
}

impl MotionOp {
    /// Words 0/1 are progress/opcode; direction remains an integer in word 2.
    pub fn pack(&self) -> [u32; 32] {
        let mut w = [0; 32];
        w[0] = self.progress.to_bits();
        w[1] = self.kind as u32;
        w[2] = match self.payload {
            MotionPayload::LegacyPush { direction } | MotionPayload::LegacySlide { direction } => direction,
            MotionPayload::LegacySplit | MotionPayload::LegacyPageTurn | MotionPayload::Camera { .. } => 0,
            MotionPayload::Roll { direction, .. } => direction,
            MotionPayload::FilmRoll { direction, .. } => direction,
            MotionPayload::Stretch { direction } => direction,
            MotionPayload::Slide { direction, .. } => direction,
            MotionPayload::Split { orientation, .. } => orientation,
            MotionPayload::Tween { .. } => 0,
            MotionPayload::Spring { direction, .. } => direction,
            MotionPayload::Pop { .. } => 0,
            MotionPayload::Wave { direction, .. } | MotionPayload::Mirror { direction } => direction,
            MotionPayload::Louver { orientation, .. } => orientation,
            MotionPayload::Pull { direction, .. } | MotionPayload::Travel { direction, .. } => direction,
            MotionPayload::Spin { spin_direction, .. } => spin_direction,
            MotionPayload::Roll3d { direction, .. } => direction,
            MotionPayload::Block { direction, .. } => direction,
        };
        if let MotionPayload::Camera { zoom_percent, rotation_degrees, blur_percent } = self.payload {
            w[12] = zoom_percent.to_bits();
            w[13] = rotation_degrees.to_bits();
            w[14] = blur_percent.to_bits();
        }
        if let MotionPayload::Roll { blur_percent, .. } = self.payload {
            w[12] = blur_percent.to_bits();
        }
        if let MotionPayload::FilmRoll { gap_pixels, blur_percent, gap_color_linear, .. } = self.payload {
            w[12] = gap_pixels.to_bits();
            w[13] = blur_percent.to_bits();
            w[16..20].copy_from_slice(&gap_color_linear.map(f32::to_bits));
        }
        if let MotionPayload::Slide { blur_percent, .. } | MotionPayload::Split { blur_percent, .. } = self.payload {
            w[12] = blur_percent.to_bits();
        }
        if let MotionPayload::Tween { scale_percent } = self.payload {
            w[12] = scale_percent.to_bits();
        }
        if let MotionPayload::Spring { bounce_percent, blur_percent, .. } = self.payload {
            w[12] = bounce_percent.to_bits();
            w[13] = blur_percent.to_bits();
        }
        if let MotionPayload::Pop { overshoot_percent, blur_percent } = self.payload {
            w[12] = overshoot_percent.to_bits();
            w[13] = blur_percent.to_bits();
        }
        match self.payload {
            MotionPayload::Wave { amplitude_pixels, wavelength_pixels, .. } => {
                w[12] = amplitude_pixels.to_bits();
                w[13] = wavelength_pixels.to_bits();
            }
            MotionPayload::Louver { slats, .. } => w[12] = slats.to_bits(),
            MotionPayload::Pull { blur_percent, .. } => w[12] = blur_percent.to_bits(),
            MotionPayload::Travel { zoom_percent, blur_percent, .. } => {
                w[12] = zoom_percent.to_bits();
                w[13] = blur_percent.to_bits();
            }
            MotionPayload::Spin { rotation_degrees, blur_percent, .. } => {
                w[12] = rotation_degrees.to_bits();
                w[13] = blur_percent.to_bits();
            }
            MotionPayload::Roll3d { perspective_percent, .. } => w[12] = perspective_percent.to_bits(),
            MotionPayload::Block { blocks, blur_percent, .. } => {
                w[12] = blocks.to_bits();
                w[13] = blur_percent.to_bits();
            }
            _ => {}
        }
        w
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::find_effect;

    #[test]
    fn legacy_motion_payloads_keep_directions_integer_typed() {
        for (id, opcode) in [
            ("push_legacy", 60),
            ("slide_legacy", 61),
            ("split_legacy", 62),
            ("page_turn", 59),
            ("roll", 42),
            ("film_roll", 41),
            ("stretch", 45),
            ("slide", 43),
            ("split", 44),
            ("motion_tween", 48),
            ("spring_motion", 38),
            ("pop_motion", 35),
        ] {
            let effect = find_effect(id).expect("registered transition").instance();
            let op = eval(&effect, 0.37, 1.0).expect("valid defaults").expect("supported route");
            assert_eq!(op.kind as u32, opcode);
            assert!(gpu_ok(&op));
            assert_eq!(op.pack()[1], opcode);
            if ["push_legacy", "slide_legacy", "roll", "film_roll", "stretch", "slide"].contains(&id) {
                assert!(op.pack()[2] < 4);
            }
        }
    }

    #[test]
    fn camera_parameters_use_registry_defaults_and_float_payload_words() {
        let effect = find_effect("motion_camera").expect("registered transition").instance();
        let op = eval(&effect, 0.25, 0.5).expect("valid defaults").expect("supported route");
        assert!(gpu_ok(&op));
        let packed = op.pack();
        assert_eq!(f32::from_bits(packed[12]), 40.0);
        assert_eq!(f32::from_bits(packed[13]), 8.0);
        assert_eq!(f32::from_bits(packed[14]), 50.0);
        assert_eq!(f32::from_bits(packed[0]), 0.25);
    }

    #[test]
    fn film_roll_keeps_scaled_gap_and_opaque_linear_gap_color() {
        let effect = find_effect("film_roll").expect("registered transition").instance();
        let op = eval(&effect, 0.5, 0.5).expect("valid defaults").expect("supported route");
        let words = op.pack();
        assert_eq!(f32::from_bits(words[12]), 20.0);
        assert_eq!(f32::from_bits(words[13]), 50.0);
        assert_eq!(f32::from_bits(words[19]), 1.0);
    }

    #[test]
    fn tween_payload_uses_registry_scale_default() {
        let effect = find_effect("motion_tween").expect("registered transition").instance();
        let op = eval(&effect, 0.4, 1.0).expect("valid defaults").expect("supported route");
        assert_eq!(f32::from_bits(op.pack()[12]), 15.0);
    }

    #[test]
    fn new_motion_payloads_pack_typed_fields_and_scale_pixel_units() {
        let wave = find_effect("wave").expect("registered transition").instance();
        let op = eval(&wave, 0.4, 0.5).expect("valid defaults").expect("wave route");
        assert_eq!(op.kind as u32, 50);
        assert_eq!(f32::from_bits(op.pack()[12]), 30.0);
        assert_eq!(f32::from_bits(op.pack()[13]), 150.0);

        let louver = find_effect("louver").expect("registered transition").instance();
        let op = eval(&louver, 0.4, 1.0).expect("valid defaults").expect("louver route");
        assert_eq!(op.kind as u32, 51);
        assert_eq!(op.pack()[2], 0);
        assert_eq!(f32::from_bits(op.pack()[12]), 8.0);

        let mirror = find_effect("mirror_transition").expect("registered transition").instance();
        let op = eval(&mirror, 0.4, 1.0).expect("valid defaults").expect("mirror route");
        assert_eq!(op.kind as u32, 52);
    }

    #[test]
    fn pull_travel_and_spin_pack_registry_units_and_direction() {
        let pull = find_effect("pull_motion").expect("registered transition").instance();
        let op = eval(&pull, 0.4, 1.0).expect("valid defaults").expect("pull route");
        assert_eq!(op.kind as u32, 53);
        assert_eq!(f32::from_bits(op.pack()[12]), 50.0);

        let travel = find_effect("travel_motion").expect("registered transition").instance();
        let op = eval(&travel, 0.4, 1.0).expect("valid defaults").expect("travel route");
        assert_eq!(op.kind as u32, 54);
        assert_eq!(f32::from_bits(op.pack()[12]), 40.0);
        assert_eq!(f32::from_bits(op.pack()[13]), 50.0);

        let spin = find_effect("spin_motion").expect("registered transition").instance();
        let op = eval(&spin, 0.4, 1.0).expect("valid defaults").expect("spin route");
        assert_eq!(op.kind as u32, 55);
        assert_eq!(op.pack()[2], 0);
        assert_eq!(f32::from_bits(op.pack()[12]), 360.0);
        assert_eq!(f32::from_bits(op.pack()[13]), 50.0);
    }

    #[test]
    fn roll_3d_keeps_direction_integer_and_perspective_float() {
        let effect = find_effect("roll_3d").expect("registered transition").instance();
        let op = eval(&effect, 0.4, 1.0).expect("valid defaults").expect("roll_3d route");
        assert_eq!(op.kind as u32, 56);
        assert_eq!(op.pack()[2], 3, "roll_3d registry/CPU default is From West");
        assert_eq!(f32::from_bits(op.pack()[12]), 50.0);
    }

    #[test]
    fn block_motion_keeps_count_and_shutter_as_float_words() {
        let effect = find_effect("block_motion").expect("registered transition").instance();
        let op = eval(&effect, 0.4, 1.0).expect("valid defaults").expect("block route");
        assert_eq!(op.kind as u32, 57);
        assert_eq!(op.pack()[2], 3, "block_motion registry/CPU default is From West");
        assert_eq!(f32::from_bits(op.pack()[12]), 6.0);
        assert_eq!(f32::from_bits(op.pack()[13]), 50.0);
    }

    #[test]
    fn unsupported_motion_ids_do_not_claim_gpu_support() {
        let effect = find_effect("flip_motion").expect("registered transition").instance();
        assert_eq!(eval(&effect, 0.5, 1.0).expect("valid ID"), None);
    }
}

//! Typed parameters for transition routes that require a full-frame blur intermediate.

use filmcraft_project::{EffectInstance, EffectKind, ParamKind, ParamValue, find_effect};
use filmcraft_time::Tick;

use super::TransitionPlanError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum BlurKind {
    Dissolve = 108,
    VrSpherical = 109,
    WhipLegacy = 111,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BlurOp {
    pub kind: BlurKind,
    pub progress: f32,
    /// Blur parameter after sequence-to-working-image scale. Shader uses half as sigma.
    pub blur_px: f32,
    /// Direction choice for the legacy whip composite.
    pub direction: u32,
}

pub fn matches_id(id: &str) -> bool {
    matches!(id, "blur_dissolve" | "vr_spherical_blur" | "whip_legacy")
}

pub fn eval(e: &EffectInstance, progress: f32, scale: f32) -> Result<Option<BlurOp>, TransitionPlanError> {
    let id = e.effect.clone();
    let Some(def) = find_effect(&id).filter(|d| d.kind == EffectKind::VideoTransition) else {
        return Err(TransitionPlanError::UnknownId { id });
    };
    let kind = match id.as_str() {
        "blur_dissolve" => BlurKind::Dissolve,
        "vr_spherical_blur" => BlurKind::VrSpherical,
        "whip_legacy" => BlurKind::WhipLegacy,
        _ => return Ok(None),
    };
    if kind == BlurKind::WhipLegacy {
        let pd = def.param("direction").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "direction" })?;
        let value = e.param("direction").map(|p| p.value_at(Tick::ZERO)).unwrap_or_else(|| pd.default.clone());
        let ParamValue::Choice(direction) = value else {
            return Err(TransitionPlanError::InvalidParameter { id, parameter: "direction" });
        };
        let ParamKind::Choice(options) = &pd.kind else {
            return Err(TransitionPlanError::InvalidParameter { id, parameter: "direction" });
        };
        if direction as usize >= options.len() || !progress.is_finite() {
            return Err(TransitionPlanError::InvalidParameter { id, parameter: "direction" });
        }
        return Ok(Some(BlurOp { kind, progress: progress.clamp(0.0, 1.0), blur_px: 0.0, direction }));
    }
    let pd = def.param("blur").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "blur" })?;
    let value = e.param("blur").map(|p| p.value_at(Tick::ZERO)).unwrap_or_else(|| pd.default.clone());
    let ParamValue::Float(value) = value else {
        return Err(TransitionPlanError::InvalidParameter { id, parameter: "blur" });
    };
    let ParamKind::Float { min, max, .. } = &pd.kind else {
        return Err(TransitionPlanError::InvalidParameter { id, parameter: "blur" });
    };
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let blur_px = value as f32 * scale;
    if !value.is_finite() || value < *min || value > *max || !blur_px.is_finite() || !progress.is_finite() {
        return Err(TransitionPlanError::InvalidParameter { id, parameter: "blur" });
    }
    Ok(Some(BlurOp { kind, progress: progress.clamp(0.0, 1.0), blur_px, direction: 0 }))
}

impl BlurOp {
    pub fn gpu_ok(&self) -> bool {
        self.progress.is_finite()
            && (0.0..=1.0).contains(&self.progress)
            && match self.kind {
                BlurKind::WhipLegacy => self.direction < 4,
                _ => self.blur_px.is_finite() && self.blur_px >= 0.0,
            }
    }

    /// Words 0 and 1 are progress and exact transition opcode; word 12 stores scaled blur.
    pub fn pack(&self) -> [u32; 32] {
        let mut words = [0; 32];
        words[0] = self.progress.to_bits();
        words[1] = self.kind as u32;
        words[12] = self.blur_px.to_bits();
        words[13] = self.direction;
        words
    }
}

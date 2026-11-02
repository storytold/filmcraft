//! Typed GPU payloads for page-peel and whip transition kernels.

use filmcraft_project::{EffectInstance, EffectKind, ParamKind, ParamValue, find_effect};
use filmcraft_time::Tick;

use super::TransitionPlanError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum CardKind {
    PagePeel = 94,
    Whip = 95,
    WhipLegacy = 107,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum CardPayload {
    PagePeel { corner: u32, radius_pixels: f32, shadow: bool },
    Whip { direction: u32, blur_percent: f32 },
    WhipLegacy { direction: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CardOp {
    pub kind: CardKind,
    pub payload: CardPayload,
    pub progress: f32,
}

pub fn matches_id(id: &str) -> bool {
    matches!(id, "page_peel" | "whip" | "whip_legacy")
}

pub fn gpu_ok(op: &CardOp) -> bool {
    match (op.kind, op.payload) {
        (CardKind::PagePeel, CardPayload::PagePeel { corner, radius_pixels, .. }) => {
            op.progress.is_finite() && (0.0..=1.0).contains(&op.progress) && corner < 4 && radius_pixels.is_finite() && radius_pixels >= 1.0
        }
        (CardKind::Whip, CardPayload::Whip { direction, blur_percent }) => {
            op.progress.is_finite() && (0.0..=1.0).contains(&op.progress) && direction < 4 && blur_percent.is_finite() && (0.0..=100.0).contains(&blur_percent)
        }
        (CardKind::Whip, _) | (CardKind::WhipLegacy, _) => false,
        _ => false,
    }
}

/// Resolve registry defaults and pixel-valued dimensions at render scale.
pub fn eval(e: &EffectInstance, progress: f32, scale: f32) -> Result<Option<CardOp>, TransitionPlanError> {
    let id = e.effect.clone();
    let Some(def) = find_effect(&id).filter(|d| d.kind == EffectKind::VideoTransition) else {
        return Err(TransitionPlanError::UnknownId { id });
    };
    let Some(kind) = (match id.as_str() {
        "page_peel" => Some(CardKind::PagePeel),
        "whip" => Some(CardKind::Whip),
        "whip_legacy" => Some(CardKind::WhipLegacy),
        _ => None,
    }) else {
        return Ok(None);
    };
    for pd in &def.params {
        let v = e.param(pd.id).map(|p| p.value_at(Tick::ZERO)).unwrap_or_else(|| pd.default.clone());
        let valid = match (&pd.kind, v) {
            (ParamKind::Choice(options), ParamValue::Choice(n)) => (n as usize) < options.len(),
            (ParamKind::Float { min, max, .. }, ParamValue::Float(n)) => n.is_finite() && n >= *min && n <= *max && (n as f32).is_finite(),
            (ParamKind::Bool, ParamValue::Bool(_)) => true,
            _ => false,
        };
        if !valid {
            return Err(TransitionPlanError::InvalidParameter { id, parameter: pd.id });
        }
    }
    let value = |name: &str| e.param(name).map(|p| p.value_at(Tick::ZERO)).or_else(|| def.param(name).map(|p| p.default.clone()));
    let choice = |name: &str| match value(name) {
        Some(ParamValue::Choice(n)) => n,
        _ => 0,
    };
    let number = |name: &str| match value(name) {
        Some(ParamValue::Float(n)) => n as f32,
        _ => 0.0,
    };
    let boolean = |name: &str| matches!(value(name), Some(ParamValue::Bool(true)));
    let payload = match kind {
        CardKind::PagePeel => CardPayload::PagePeel {
            corner: choice("corner"),
            radius_pixels: (number("radius").max(0.0) * if scale.is_finite() && scale > 0.0 { scale } else { 1.0 }).max(1.0),
            shadow: boolean("shadow"),
        },
        CardKind::Whip => CardPayload::Whip { direction: choice("direction"), blur_percent: number("blur") },
        CardKind::WhipLegacy => CardPayload::WhipLegacy { direction: choice("direction") },
    };
    let op = CardOp { kind, payload, progress };
    if !gpu_ok(&op) {
        return Err(TransitionPlanError::InvalidParameter { id, parameter: "transition" });
    }
    Ok(Some(op))
}

impl CardOp {
    pub fn pack(&self) -> [u32; 32] {
        let mut words = [0; 32];
        words[0] = self.progress.to_bits();
        words[1] = self.kind as u32;
        match self.payload {
            CardPayload::PagePeel { corner, radius_pixels, shadow } => {
                words[2] = corner;
                words[12] = radius_pixels.to_bits();
                words[13] = (if shadow { 1.0f32 } else { 0.0f32 }).to_bits();
            }
            CardPayload::Whip { direction, blur_percent } => {
                words[2] = direction;
                words[12] = blur_percent.to_bits();
            }
            CardPayload::WhipLegacy { direction } => words[2] = direction,
        }
        words
    }
}

#[cfg(test)]
mod page_peel_oracle {
    fn origin(corner: u32, width: f32, height: f32) -> (f32, f32) {
        match corner {
            0 => (0.0, 0.0),
            1 => (width, 0.0),
            3 => (0.0, height),
            _ => (width, height),
        }
    }

    fn shadow_gain(progress: f32, d: f32, radius: f32) -> f32 {
        let smooth = |a: f32, b: f32, x: f32| {
            let t = ((x - a) / (b - a)).clamp(0.0, 1.0);
            t * t * (3.0 - 2.0 * t)
        };
        1.0 - 0.45 * (-((d - radius).max(0.0)) / (radius * 1.5)).exp() * smooth(0.0, 0.05, progress) * (1.0 - smooth(0.9, 1.0, progress))
    }

    fn shadow_pixel(b: [f32; 4], gain: f32) -> [f32; 4] {
        [b[0] * gain, b[1] * gain, b[2] * gain, b[3]]
    }

    #[test]
    fn corner_choice_matches_cpu_origin_table() {
        assert_eq!(origin(0, 100.0, 60.0), (0.0, 0.0));
        assert_eq!(origin(1, 100.0, 60.0), (100.0, 0.0));
        assert_eq!(origin(2, 100.0, 60.0), (100.0, 60.0));
        assert_eq!(origin(3, 100.0, 60.0), (0.0, 60.0));
    }

    #[test]
    fn page_peel_shadow_matches_cpu_equation_at_boundaries() {
        assert_eq!(shadow_gain(0.0, 10.0, 10.0), 1.0);
        assert!((shadow_gain(0.5, 10.0, 10.0) - 0.55).abs() < 1e-6);
        assert_eq!(shadow_gain(1.0, 10.0, 10.0), 1.0);
        let far = shadow_gain(0.5, 25.0, 10.0);
        assert!(far > 0.55 && far < 1.0);
        let shaded = shadow_pixel([0.8, 0.4, 0.2, 0.6], 0.55);
        for (actual, expected) in shaded.into_iter().zip([0.44, 0.22, 0.11, 0.6]) {
            assert!((actual - expected).abs() < 1e-6);
        }
    }
}

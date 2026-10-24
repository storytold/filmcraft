//! Typed single-pass grunge routes. Unsupported routes stay ineligible until their shader exists.

use filmcraft_project::{EffectInstance, EffectKind, ParamKind, ParamValue, find_effect};
use filmcraft_time::Tick;

use super::TransitionPlanError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u32)]
pub enum GrungeKind {
    Chaos = 80,
    Earthquake = 81,
    Glass = 82,
    Glitch = 83,
    Grunge = 84,
    TvPower = 85,
    VhsDamage = 86,
    Flicker = 104,
    Kaleidoscope = 105,
    LiquidDistortion = 106,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GrungePayload {
    Chaos { amount: f32, seed: u32 },
    Earthquake { shake_px: f32, seed: u32, shutter: f32, samples: u32 },
    Glass { cell_px: f32, refraction_px: f32, seed: u32 },
    Glitch { amount: f32, seed: u32 },
    Grunge { scale_px: f32, feather_px: f32, edge_px: f32, edge_linear: [f32; 4], seed: u32 },
    TvPower { line_px: f32, glow_linear: [f32; 4] },
    VhsDamage { amount: f32, seed: u32 },
    Flicker { count: f32, seed: u32 },
    Kaleidoscope { segments: f32, zoom: f32 },
    LiquidDistortion { amount_px: f32, scale_px: f32, seed: u32 },
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GrungeOp {
    pub kind: GrungeKind,
    pub payload: GrungePayload,
    pub progress: f32,
}

pub fn matches_id(id: &str) -> bool {
    matches!(id, "chaos" | "earthquake" | "glass" | "glitch" | "grunge" | "tv_power" | "vhs_damage" | "flicker" | "kaleidoscope" | "liquid_distortion")
}

pub fn gpu_ok(op: &GrungeOp) -> bool {
    match (op.kind, op.payload) {
        (GrungeKind::Chaos, GrungePayload::Chaos { amount, .. })
        | (GrungeKind::Glitch, GrungePayload::Glitch { amount, .. })
        | (GrungeKind::VhsDamage, GrungePayload::VhsDamage { amount, .. }) => amount.is_finite() && (0.0..=1.0).contains(&amount),
        (GrungeKind::Earthquake, GrungePayload::Earthquake { shake_px, shutter, samples, .. }) => {
            shake_px.is_finite() && shake_px >= 0.0 && shutter.is_finite() && shutter >= 0.0 && (1..=12).contains(&samples)
        }
        (GrungeKind::Glass, GrungePayload::Glass { cell_px, refraction_px, .. }) => {
            cell_px.is_finite() && cell_px >= 4.0 && refraction_px.is_finite() && refraction_px >= 0.0
        }
        (GrungeKind::Grunge, GrungePayload::Grunge { scale_px, feather_px, edge_px, edge_linear, .. }) => {
            scale_px.is_finite()
                && scale_px >= 4.0
                && feather_px.is_finite()
                && feather_px >= 0.0
                && edge_px.is_finite()
                && edge_px >= 0.0
                && edge_linear.iter().all(|v| v.is_finite())
        }
        (GrungeKind::TvPower, GrungePayload::TvPower { line_px, glow_linear }) => {
            line_px.is_finite() && line_px >= 1.0 && glow_linear.iter().all(|v| v.is_finite())
        }
        (GrungeKind::Flicker, GrungePayload::Flicker { count, .. }) => count.is_finite() && (1.0..=40.0).contains(&count),
        (GrungeKind::Kaleidoscope, GrungePayload::Kaleidoscope { segments, zoom }) => {
            segments.is_finite() && (2.0..=32.0).contains(&segments) && zoom.is_finite() && (0.0..=1.0).contains(&zoom)
        }
        (GrungeKind::LiquidDistortion, GrungePayload::LiquidDistortion { amount_px, scale_px, .. }) => {
            amount_px.is_finite() && amount_px >= 0.0 && scale_px.is_finite() && scale_px >= 4.0
        }
        _ => false,
    }
}

pub fn eval(e: &EffectInstance, progress: f32, scale: f32) -> Result<Option<GrungeOp>, TransitionPlanError> {
    let id = e.effect.clone();
    let Some(def) = find_effect(&id).filter(|d| d.kind == EffectKind::VideoTransition) else {
        return Err(TransitionPlanError::UnknownId { id });
    };
    if !matches_id(&id) {
        return Ok(None);
    }
    for pd in &def.params {
        let v = e.param(pd.id).map(|p| p.value_at(Tick::ZERO)).unwrap_or_else(|| pd.default.clone());
        let valid = match (&pd.kind, v) {
            (ParamKind::Float { min, max, .. }, ParamValue::Float(n)) => n.is_finite() && n >= *min && n <= *max && (n as f32).is_finite(),
            (ParamKind::Angle, ParamValue::Float(n)) => n.is_finite() && (n as f32).is_finite(),
            (ParamKind::Color, ParamValue::Color(c)) => c.iter().all(|n| n.is_finite()),
            (ParamKind::Choice(options), ParamValue::Choice(n)) => (n as usize) < options.len(),
            (ParamKind::Bool, ParamValue::Bool(_)) => true,
            _ => false,
        };
        if !valid {
            return Err(TransitionPlanError::InvalidParameter { id, parameter: pd.id });
        }
    }
    let num = |name: &str| -> f32 {
        e.param(name)
            .map(|v| v.value_at(Tick::ZERO))
            .and_then(|v| match v {
                ParamValue::Float(n) => Some(n as f32),
                _ => None,
            })
            .or_else(|| {
                def.param(name).and_then(|p| match &p.default {
                    ParamValue::Float(n) => Some(*n as f32),
                    _ => None,
                })
            })
            .unwrap_or(0.0)
    };
    let seed = num("seed").max(0.0) as u32;
    let sc = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let op = match id.as_str() {
        "chaos" => GrungeOp { kind: GrungeKind::Chaos, payload: GrungePayload::Chaos { amount: num("amount") / 100.0, seed }, progress },
        "earthquake" => {
            let mb = (num("motion_blur") / 100.0).clamp(0.0, 1.0);
            let samples = if mb <= 0.0 { 1 } else { ((mb * 12.0).ceil() as u32).clamp(2, 12) };
            GrungeOp {
                kind: GrungeKind::Earthquake,
                payload: GrungePayload::Earthquake { shake_px: num("shake").max(0.0) * sc, seed, shutter: 0.03 * mb, samples },
                progress,
            }
        }
        "glass" => GrungeOp {
            kind: GrungeKind::Glass,
            payload: GrungePayload::Glass { cell_px: (num("cell_size").max(4.0) * sc).max(4.0), refraction_px: num("refraction").max(0.0) * sc, seed },
            progress,
        },
        "glitch" => GrungeOp { kind: GrungeKind::Glitch, payload: GrungePayload::Glitch { amount: num("amount") / 100.0, seed }, progress },
        "grunge" => {
            let c = e.param("edge_color").map(|p| p.value_at(Tick::ZERO)).or_else(|| def.param("edge_color").map(|p| p.default.clone()));
            let Some(ParamValue::Color(c)) = c else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "edge_color" });
            };
            let color = [
                filmcraft_color::srgb_to_linear(c[0].clamp(0.0, 1.0)),
                filmcraft_color::srgb_to_linear(c[1].clamp(0.0, 1.0)),
                filmcraft_color::srgb_to_linear(c[2].clamp(0.0, 1.0)),
                1.0,
            ];
            GrungeOp {
                kind: GrungeKind::Grunge,
                payload: GrungePayload::Grunge {
                    scale_px: (num("scale").max(4.0) * sc).max(4.0),
                    feather_px: 2.0 * sc,
                    edge_px: num("edge_width").max(0.0) * sc,
                    edge_linear: color,
                    seed,
                },
                progress,
            }
        }
        "tv_power" => {
            let c = e.param("glow_color").map(|p| p.value_at(Tick::ZERO)).or_else(|| def.param("glow_color").map(|p| p.default.clone()));
            let Some(ParamValue::Color(c)) = c else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "glow_color" });
            };
            let color = [
                filmcraft_color::srgb_to_linear(c[0].clamp(0.0, 1.0)),
                filmcraft_color::srgb_to_linear(c[1].clamp(0.0, 1.0)),
                filmcraft_color::srgb_to_linear(c[2].clamp(0.0, 1.0)),
                1.0,
            ];
            GrungeOp {
                kind: GrungeKind::TvPower,
                payload: GrungePayload::TvPower { line_px: (num("line").max(1.0) * sc).max(1.0), glow_linear: color },
                progress,
            }
        }
        "vhs_damage" => GrungeOp { kind: GrungeKind::VhsDamage, payload: GrungePayload::VhsDamage { amount: num("amount") / 100.0, seed }, progress },
        "flicker" => {
            GrungeOp { kind: GrungeKind::Flicker, payload: GrungePayload::Flicker { count: num("flickers").round().clamp(1.0, 40.0), seed }, progress }
        }
        "kaleidoscope" => GrungeOp {
            kind: GrungeKind::Kaleidoscope,
            payload: GrungePayload::Kaleidoscope { segments: num("segments").round().clamp(2.0, 32.0), zoom: num("zoom") / 100.0 },
            progress,
        },
        "liquid_distortion" => GrungeOp {
            kind: GrungeKind::LiquidDistortion,
            payload: GrungePayload::LiquidDistortion { amount_px: num("amount").max(0.0) * sc, scale_px: (num("scale").max(0.0) * sc).max(4.0), seed },
            progress,
        },
        _ => return Ok(None),
    };
    if !op.progress.is_finite() || !gpu_ok(&op) {
        return Err(TransitionPlanError::InvalidParameter { id, parameter: "grunge" });
    }
    Ok(Some(GrungeOp { progress: progress.clamp(0.0, 1.0), ..op }))
}

impl GrungeOp {
    /// Words 0 and 1 are progress and the exact family opcode.
    pub fn pack(&self) -> [u32; 32] {
        let mut words = [0; 32];
        words[0] = self.progress.to_bits();
        words[1] = self.kind as u32;
        match self.payload {
            GrungePayload::Chaos { amount, seed } | GrungePayload::Glitch { amount, seed } | GrungePayload::VhsDamage { amount, seed } => {
                words[2] = seed;
                words[12] = amount.to_bits();
            }
            GrungePayload::Earthquake { shake_px, seed, shutter, samples } => {
                words[2] = seed;
                words[12] = shake_px.to_bits();
                words[13] = shutter.to_bits();
                words[14] = (samples as f32).to_bits();
            }
            GrungePayload::Glass { cell_px, refraction_px, seed } => {
                words[2] = seed;
                words[12] = cell_px.to_bits();
                words[13] = refraction_px.to_bits();
            }
            GrungePayload::Grunge { scale_px, feather_px, edge_px, edge_linear, seed } => {
                words[2] = seed;
                words[12] = scale_px.to_bits();
                words[13] = feather_px.to_bits();
                words[14] = edge_px.to_bits();
                for (slot, value) in words[16..20].iter_mut().zip(edge_linear) {
                    *slot = value.to_bits();
                }
            }
            GrungePayload::TvPower { line_px, glow_linear } => {
                words[12] = line_px.to_bits();
                for (slot, value) in words[16..20].iter_mut().zip(glow_linear) {
                    *slot = value.to_bits();
                }
            }
            GrungePayload::Flicker { count, seed } => {
                words[2] = seed;
                words[12] = count.to_bits();
            }
            GrungePayload::Kaleidoscope { segments, zoom } => {
                words[12] = segments.to_bits();
                words[13] = zoom.to_bits();
            }
            GrungePayload::LiquidDistortion { amount_px, scale_px, seed } => {
                words[2] = seed;
                words[12] = amount_px.to_bits();
                words[13] = scale_px.to_bits();
            }
        }
        words
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_defaults_are_typed_and_packed_with_exact_opcodes() {
        for (id, opcode) in [
            ("chaos", 80),
            ("earthquake", 81),
            ("glass", 82),
            ("glitch", 83),
            ("grunge", 84),
            ("tv_power", 85),
            ("vhs_damage", 86),
            ("flicker", 104),
            ("kaleidoscope", 105),
            ("liquid_distortion", 106),
        ] {
            let instance = find_effect(id).map(|definition| definition.instance());
            let Some(instance) = instance else { continue };
            let op = eval(&instance, 0.5, 1.0).ok().flatten();
            assert!(op.is_some_and(|op| gpu_ok(&op) && op.pack()[1] == opcode));
        }
    }

    #[test]
    fn all_ten_registry_routes_have_exact_shader_opcodes() {
        for (id, opcode) in [
            ("chaos", 80),
            ("earthquake", 81),
            ("glass", 82),
            ("glitch", 83),
            ("grunge", 84),
            ("tv_power", 85),
            ("vhs_damage", 86),
            ("flicker", 104),
            ("kaleidoscope", 105),
            ("liquid_distortion", 106),
        ] {
            assert!(matches_id(id));
            let Some(instance) = find_effect(id).map(|definition| definition.instance()) else { panic!("missing transition registry entry: {id}") };
            let op = eval(&instance, 0.37, 0.5).expect("registry defaults validate").expect("typed route is available");
            assert!(gpu_ok(&op));
            assert_eq!(op.pack()[1], opcode);
        }
    }
}

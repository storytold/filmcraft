//! Typed transition planning; GPU stages consume evaluated values only.

use crate::plan::ResourceRequirements;
use filmcraft_project::{EffectInstance, EffectKind, ParamDef, ParamKind, ParamValue, find_effect};
use filmcraft_time::Tick;

#[path = "gpu_lights.rs"]
pub mod gpu_lights;
pub use gpu_lights::{LightKind, LightOp};
#[path = "gpu_motion.rs"]
pub mod gpu_motion;
pub use gpu_motion::{MotionKind, MotionOp, MotionPayload};
#[path = "gpu_grunge.rs"]
pub mod gpu_grunge;
pub use gpu_grunge::{GrungeKind, GrungeOp, GrungePayload};
#[path = "gpu_cards.rs"]
pub mod gpu_cards;
pub use gpu_cards::{CardKind, CardOp, CardPayload};
#[path = "gpu_blur.rs"]
pub mod gpu_blur;
pub use gpu_blur::{BlurKind, BlurOp};
#[path = "gpu_3d.rs"]
pub mod gpu_3d;
pub use gpu_3d::ThreeDOp;

#[derive(Clone, Debug, PartialEq)]
pub struct PlannedTransition {
    pub id: String,
    pub op: TransitionOp,
    pub progress: f32,
    pub scale: f32,
    pub requirements: ResourceRequirements,
}

/// Only implemented kernel routes belong here; other registry IDs fail closed.
#[derive(Clone, Debug, PartialEq)]
pub enum TransitionOp {
    CrossDissolve,
    CenterSplit,
    BandSlide,
    TextCells { columns: f32, rows: f32, typewriter: bool, cursor: bool },
    VrRandomBlocks { block_size_px: f32, softness: f32, seed: u32 },
    VrLeaks { amount: f32, seed: u32, chroma: bool },
    VrLightRays { center: [Option<f32>; 2], amount: f32 },
    VrMobiusZoom { zoom: f32, twist_radians: f32 },
    Frame { direction: u32, frame_width_px: f32, frame_color_linear: [f32; 4], content_scale: f32 },
    Grunge(GrungeOp),
    Card(CardOp),
    Blur(BlurOp),
    ThreeD(ThreeDOp),
    AdditiveDissolve,
    FilmDissolve,
    DipToBlack,
    DipToWhite,
    Push(PushParams),
    Flare(FlareParams),
    LinearWipe(LinearWipeParams),
    SoftWipe(SoftWipeParams),
    LegacyWipe { direction: u32 },
    DipToColor { color_linear: [f32; 3], hold: f32 },
    LumaFade { source: u32, softness: f32, invert: bool },
    NonAdditiveDissolve,
    LegacyClockWipe,
    VenetianBlinds,
    CheckerWipe,
    GradientWipe { softness: f32 },
    FieldWipe(FieldWipeParams),
    VrGradientWipe { softness: f32, invert: bool },
    StretchWipe { direction: u32, stretch_px: f32 },
    NeonWipe { angle_degrees: f32, glow_px: f32, amount: f32, color_linear: [f32; 3], antialias: f32 },
    BurnAlpha { softness: f32, seed: u32, color_linear: [f32; 3] },
    Mosaic { block_size_px: f32, dissolve: bool },
    ShapeDissolve { shape: u32, size_px: f32, softness_px: f32, rotation_degrees: f32, order: u32, seed: u32 },
    ShapeFlow { shape: u32, direction: u32, softness_px: f32 },
    Light(LightOp),
    Motion(MotionOp),
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PushDirection {
    FromNorth,
    FromEast,
    FromSouth,
    FromWest,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PushParams {
    pub direction: PushDirection,
    pub motion_blur: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlareParams {
    pub amount: f32,
    pub angle_degrees: f32,
    pub color_linear: [f32; 3],
}

#[derive(Clone, Debug, PartialEq)]
pub struct LinearWipeParams {
    pub angle_degrees: f32,
    pub feather: f32,
    pub border_width: f32,
    pub border_color_linear: [f32; 3],
    pub antialias: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SoftWipeParams {
    pub angle_degrees: f32,
    pub feather: f32,
    pub antialias: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct WipeStyleParams {
    pub feather: f32,
    pub border_width: f32,
    pub border_color_linear: [f32; 3],
    pub antialias: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FieldWipeParams {
    pub kind: FieldWipeKind,
    pub style: WipeStyleParams,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FieldWipeKind {
    Clock { center: [Option<f32>; 2], start_degrees: f32 },
    Radial { corner: u32 },
    Star { center: [Option<f32>; 2], points: u32, inner: f32, rotation_degrees: f32 },
    Panel { direction: u32, panels: u32 },
    Plateau { angle_degrees: f32, plateaus: u32 },
    BarnDoors { center: [Option<f32>; 2], orientation: u32 },
    Inset { corner: u32 },
    Iris { center: [Option<f32>; 2], shape: IrisShape },
    VrIris { center: [Option<f32>; 2] },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IrisShape {
    Round,
    Box,
    Diamond,
    Cross,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TransitionPlanError {
    UnknownId { id: String },
    UnsupportedOperation { id: String },
    InvalidParameter { id: String, parameter: &'static str },
}

fn value(e: &EffectInstance, pd: &ParamDef) -> ParamValue {
    e.param(pd.id).map(|p| p.value_at(Tick::ZERO)).unwrap_or_else(|| pd.default.clone())
}

fn choice(e: &EffectInstance, def: &filmcraft_project::EffectDef, name: &'static str, id: &str) -> Result<u32, TransitionPlanError> {
    let pd = def.param(name).ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.into(), parameter: name })?;
    match value(e, pd) {
        ParamValue::Choice(v) => Ok(v),
        _ => Err(TransitionPlanError::InvalidParameter { id: id.into(), parameter: name }),
    }
}

fn center(e: &EffectInstance, def: &filmcraft_project::EffectDef, scale: f32, id: &str) -> Result<[Option<f32>; 2], TransitionPlanError> {
    let Some(pd) = def.param("center") else { return Ok([None, None]) };
    let ParamValue::Vec2(point) = value(e, pd) else {
        return Err(TransitionPlanError::InvalidParameter { id: id.into(), parameter: "center" });
    };
    let component = |v: f64| -> Result<Option<f32>, ()> {
        if v.is_nan() {
            return Ok(None);
        }
        let scaled = v as f32 * scale;
        if scaled.is_finite() { Ok(Some(scaled)) } else { Err(()) }
    };
    Ok([
        component(point.x).map_err(|_| TransitionPlanError::InvalidParameter { id: id.into(), parameter: "center" })?,
        component(point.y).map_err(|_| TransitionPlanError::InvalidParameter { id: id.into(), parameter: "center" })?,
    ])
}

fn wipe_style(e: &EffectInstance, def: &filmcraft_project::EffectDef, scale: f32, id: &str) -> Result<WipeStyleParams, TransitionPlanError> {
    let number = |name: &'static str| e.f64_at(name, Tick::ZERO) as f32;
    let pd = def.param("border_color").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.into(), parameter: "border_color" })?;
    let ParamValue::Color(color) = value(e, pd) else {
        return Err(TransitionPlanError::InvalidParameter { id: id.into(), parameter: "border_color" });
    };
    let feather = number("feather").max(0.0) * scale;
    let border_width = number("border_width").max(0.0) * scale;
    if !feather.is_finite() {
        return Err(TransitionPlanError::InvalidParameter { id: id.into(), parameter: "feather" });
    }
    if !border_width.is_finite() {
        return Err(TransitionPlanError::InvalidParameter { id: id.into(), parameter: "border_width" });
    }
    let aa = choice(e, def, "antialias", id)?;
    Ok(WipeStyleParams {
        feather,
        border_width,
        border_color_linear: [color[0], color[1], color[2]].map(|v| filmcraft_color::srgb_to_linear(v.clamp(0.0, 1.0))),
        antialias: [0.5, 1.0, 1.75, 2.5][aa as usize] * scale.clamp(0.25, 1.0),
    })
}

/// Metadata only: validates the live registry and never renders or reads pixels.
pub fn evaluate(e: &EffectInstance, progress: f32, scale: f32) -> Result<PlannedTransition, TransitionPlanError> {
    let id = e.effect.clone();
    let def = find_effect(&id).filter(|d| d.kind == EffectKind::VideoTransition).ok_or_else(|| TransitionPlanError::UnknownId { id: id.clone() })?;
    for pd in &def.params {
        let valid = match (&pd.kind, value(e, pd)) {
            (ParamKind::Float { min, max, .. }, ParamValue::Float(v)) => v.is_finite() && v >= *min && v <= *max && (v as f32).is_finite(),
            (ParamKind::Angle, ParamValue::Float(v)) => v.is_finite() && (v as f32).is_finite(),
            (ParamKind::Choice(options), ParamValue::Choice(v)) => (v as usize) < options.len(),
            (ParamKind::Bool, ParamValue::Bool(_)) => true,
            (ParamKind::Color, ParamValue::Color(v)) => v.iter().all(|x| x.is_finite()),
            // Each NaN component is the CPU's frame-centre sentinel; infinity is not authored data.
            (ParamKind::Point, ParamValue::Vec2(v)) => [v.x, v.y].iter().all(|x| x.is_nan() || (x.is_finite() && (*x as f32).is_finite())),
            _ => false,
        };
        if !valid {
            return Err(TransitionPlanError::InvalidParameter { id, parameter: pd.id });
        }
    }
    let number = |name: &str| e.f64_at(name, Tick::ZERO) as f32;
    let scale = if scale.is_finite() && scale > 0.0 { scale } else { 1.0 };
    let progress = if progress.is_finite() { progress.clamp(0.0, 1.0) } else { 0.0 };
    let op = match id.as_str() {
        "vr_random_blocks" => {
            let block_size_px = (number("block_size") * scale).max(2.0);
            if !block_size_px.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "block_size" });
            }
            TransitionOp::VrRandomBlocks {
                block_size_px,
                softness: (number("softness") / 100.0).clamp(0.0, 0.45).max(0.01),
                seed: number("seed").max(0.0) as u32,
            }
        }
        "vr_chroma_leaks" | "vr_light_leaks" => {
            TransitionOp::VrLeaks { amount: number("amount") / 100.0, seed: number("seed").max(0.0) as u32, chroma: id == "vr_chroma_leaks" }
        }
        "vr_light_rays" => TransitionOp::VrLightRays { center: center(e, def, scale, &id)?, amount: number("amount") / 100.0 },
        "vr_mobius_zoom" => TransitionOp::VrMobiusZoom { zoom: number("zoom") / 100.0, twist_radians: number("twist").to_radians() },
        "frame" => {
            let frame_width_px = number("frame_width").max(0.0) * scale;
            let pd = def.param("frame_color").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "frame_color" })?;
            let ParamValue::Color(color) = value(e, pd) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "frame_color" });
            };
            if !frame_width_px.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "frame_width" });
            }
            TransitionOp::Frame {
                // The CPU's Tx::dir() falls back to choice 0 when this parameter is absent.
                direction: if def.param("direction").is_some() { choice(e, def, "direction", &id)? } else { 0 },
                frame_width_px,
                frame_color_linear: [
                    filmcraft_color::srgb_to_linear(color[0].clamp(0.0, 1.0)),
                    filmcraft_color::srgb_to_linear(color[1].clamp(0.0, 1.0)),
                    filmcraft_color::srgb_to_linear(color[2].clamp(0.0, 1.0)),
                    1.0,
                ],
                content_scale: (number("scale") / 100.0).clamp(0.05, 1.0),
            }
        }
        id if gpu_cards::matches_id(id) && id != "whip_legacy" => {
            TransitionOp::Card(gpu_cards::eval(e, progress, scale)?.ok_or_else(|| TransitionPlanError::UnsupportedOperation { id: id.to_owned() })?)
        }
        id if gpu_blur::matches_id(id) => {
            TransitionOp::Blur(gpu_blur::eval(e, progress, scale)?.ok_or_else(|| TransitionPlanError::UnsupportedOperation { id: id.to_owned() })?)
        }
        "flip_motion" | "spin_3d" | "spinback_3d" | "slice" => {
            TransitionOp::ThreeD(gpu_3d::eval(e, progress).ok_or_else(|| TransitionPlanError::UnsupportedOperation { id: id.to_owned() })?)
        }
        "text_animator" | "typewriter" => TransitionOp::TextCells {
            columns: number("columns").round().clamp(2.0, 128.0),
            rows: number("rows").round().clamp(1.0, 64.0),
            typewriter: id == "typewriter",
            cursor: id == "typewriter"
                && value(e, def.param("cursor").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "cursor" })?)
                    == ParamValue::Bool(true),
        },
        "center_split" => TransitionOp::CenterSplit,
        "band_slide" => TransitionOp::BandSlide,
        "cross_dissolve" | "cross_dissolve_legacy" => TransitionOp::CrossDissolve,
        "additive_dissolve" | "additive_dissolve_legacy" => TransitionOp::AdditiveDissolve,
        "film_dissolve" | "film_dissolve_legacy" => TransitionOp::FilmDissolve,
        "dip_to_black" | "dip_to_black_legacy" => TransitionOp::DipToBlack,
        "dip_to_white" | "dip_to_white_legacy" => TransitionOp::DipToWhite,
        "push" => {
            let pd = def.param("direction").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "direction" })?;
            let direction = match value(e, pd) {
                ParamValue::Choice(0) => PushDirection::FromNorth,
                ParamValue::Choice(1) => PushDirection::FromEast,
                ParamValue::Choice(2) => PushDirection::FromSouth,
                _ => PushDirection::FromWest,
            };
            TransitionOp::Push(PushParams { direction, motion_blur: number("motion_blur") / 100.0 })
        }
        "flare" => {
            let pd = def.param("color").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "color" })?;
            let ParamValue::Color(c) = value(e, pd) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "color" });
            };
            TransitionOp::Flare(FlareParams {
                amount: number("amount") / 100.0,
                angle_degrees: number("angle"),
                color_linear: [c[0], c[1], c[2]].map(|v| filmcraft_color::srgb_to_linear(v.clamp(0.0, 1.0))),
            })
        }
        "linear_wipe" => {
            let feather = number("feather").max(0.0) * scale;
            let border_width = number("border_width").max(0.0) * scale;
            if !feather.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "feather" });
            }
            if !border_width.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "border_width" });
            }
            let pd = def.param("border_color").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "border_color" })?;
            let ParamValue::Color(c) = value(e, pd) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "border_color" });
            };
            let aa = def.param("antialias").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "antialias" })?;
            let ParamValue::Choice(antialias) = value(e, aa) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "antialias" });
            };
            TransitionOp::LinearWipe(LinearWipeParams {
                angle_degrees: number("angle"),
                feather,
                border_width,
                border_color_linear: [c[0], c[1], c[2]].map(|v| filmcraft_color::srgb_to_linear(v.clamp(0.0, 1.0))),
                antialias: [0.5, 1.0, 1.75, 2.5][antialias as usize] * scale.clamp(0.25, 1.0),
            })
        }
        "soft_wipe" => {
            let feather = number("feather").max(0.0) * scale;
            if !feather.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "feather" });
            }
            TransitionOp::SoftWipe(SoftWipeParams { angle_degrees: number("angle"), feather, antialias: 0.5 * scale.clamp(0.25, 1.0) })
        }
        "wipe" => {
            let pd = def.param("direction").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "direction" })?;
            let ParamValue::Choice(direction) = value(e, pd) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "direction" });
            };
            TransitionOp::LegacyWipe { direction }
        }
        "dip_to_color" => {
            let pd = def.param("color").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "color" })?;
            let ParamValue::Color(c) = value(e, pd) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "color" });
            };
            TransitionOp::DipToColor {
                color_linear: [c[0], c[1], c[2]].map(|v| filmcraft_color::srgb_to_linear(v.clamp(0.0, 1.0))),
                hold: (number("hold") / 100.0).clamp(0.0, 0.9),
            }
        }
        "luma_fade" => {
            let source = def.param("source").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "source" })?;
            let ParamValue::Choice(source) = value(e, source) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "source" });
            };
            let invert = def.param("invert").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "invert" })?;
            let ParamValue::Bool(invert) = value(e, invert) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "invert" });
            };
            TransitionOp::LumaFade { source, softness: number("softness") / 100.0, invert }
        }
        "non_additive_dissolve" => TransitionOp::NonAdditiveDissolve,
        "clock_wipe_legacy" | "radial_wipe_legacy" => TransitionOp::LegacyClockWipe,
        "venetian_blinds" => TransitionOp::VenetianBlinds,
        "checker_wipe" => TransitionOp::CheckerWipe,
        "gradient_wipe" => TransitionOp::GradientWipe { softness: number("softness") / 100.0 + 0.02 },
        "clock_wipe" => TransitionOp::FieldWipe(FieldWipeParams {
            kind: FieldWipeKind::Clock { center: center(e, def, scale, &id)?, start_degrees: number("start") },
            style: wipe_style(e, def, scale, &id)?,
        }),
        "radial_wipe" => TransitionOp::FieldWipe(FieldWipeParams {
            kind: FieldWipeKind::Radial { corner: choice(e, def, "corner", &id)? },
            style: wipe_style(e, def, scale, &id)?,
        }),
        "star_wipe" => TransitionOp::FieldWipe(FieldWipeParams {
            kind: FieldWipeKind::Star {
                center: center(e, def, scale, &id)?,
                points: number("points").round().clamp(3.0, 24.0) as u32,
                inner: number("inner") / 100.0,
                rotation_degrees: number("rotation"),
            },
            style: wipe_style(e, def, scale, &id)?,
        }),
        "panel_wipe" => TransitionOp::FieldWipe(FieldWipeParams {
            kind: FieldWipeKind::Panel { direction: choice(e, def, "direction", &id)?, panels: number("panels").round().clamp(1.0, 64.0) as u32 },
            style: wipe_style(e, def, scale, &id)?,
        }),
        "plateau_wipe" => TransitionOp::FieldWipe(FieldWipeParams {
            kind: FieldWipeKind::Plateau { angle_degrees: number("angle"), plateaus: number("plateaus").round().clamp(1.0, 64.0) as u32 },
            style: wipe_style(e, def, scale, &id)?,
        }),
        "barn_doors" => TransitionOp::FieldWipe(FieldWipeParams {
            kind: FieldWipeKind::BarnDoors { center: center(e, def, scale, &id)?, orientation: choice(e, def, "orientation", &id)? },
            style: wipe_style(e, def, scale, &id)?,
        }),
        "inset" => TransitionOp::FieldWipe(FieldWipeParams {
            kind: FieldWipeKind::Inset { corner: choice(e, def, "corner", &id)? },
            style: wipe_style(e, def, scale, &id)?,
        }),
        "iris_round" | "iris_box" | "iris_diamond" | "iris_cross" => {
            let shape = match id.as_str() {
                "iris_round" => IrisShape::Round,
                "iris_box" => IrisShape::Box,
                "iris_diamond" => IrisShape::Diamond,
                _ => IrisShape::Cross,
            };
            TransitionOp::FieldWipe(FieldWipeParams {
                kind: FieldWipeKind::Iris { center: center(e, def, scale, &id)?, shape },
                style: wipe_style(e, def, scale, &id)?,
            })
        }
        "vr_iris_wipe" => TransitionOp::FieldWipe(FieldWipeParams {
            kind: FieldWipeKind::VrIris { center: center(e, def, scale, &id)? },
            style: wipe_style(e, def, scale, &id)?,
        }),
        "vr_gradient_wipe" => {
            let pd = def.param("invert").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "invert" })?;
            let ParamValue::Bool(invert) = value(e, pd) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "invert" });
            };
            TransitionOp::VrGradientWipe { softness: (number("softness") / 100.0).clamp(0.005, 1.0) * 0.5, invert }
        }
        "stretch_wipe" => {
            let stretch_px = number("stretch").max(0.0) * scale;
            if !stretch_px.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "stretch" });
            }
            TransitionOp::StretchWipe { direction: choice(e, def, "direction", &id)?, stretch_px: stretch_px.max(1.0) }
        }
        "neon_wipe" => {
            let glow_px = number("glow").max(0.0) * scale;
            if !glow_px.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "glow" });
            }
            let pd = def.param("color").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "color" })?;
            let ParamValue::Color(color) = value(e, pd) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "color" });
            };
            TransitionOp::NeonWipe {
                angle_degrees: number("angle"),
                glow_px: glow_px.max(1.0),
                amount: (number("amount") / 100.0).clamp(0.0, 1.0),
                color_linear: [color[0], color[1], color[2]].map(|v| filmcraft_color::srgb_to_linear(v.clamp(0.0, 1.0))),
                antialias: 0.5 * scale.clamp(0.25, 1.0),
            }
        }
        "burn_alpha" => {
            let pd = def.param("burn_color").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "burn_color" })?;
            let ParamValue::Color(color) = value(e, pd) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "burn_color" });
            };
            TransitionOp::BurnAlpha {
                softness: (number("softness") / 100.0).clamp(0.01, 1.0) * 0.3,
                seed: number("seed").max(0.0) as u32,
                color_linear: [color[0], color[1], color[2]].map(|v| filmcraft_color::srgb_to_linear(v.clamp(0.0, 1.0))),
            }
        }
        "mosaic_transition" => {
            let block_size_px = number("block_size").max(0.0) * scale;
            if !block_size_px.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "block_size" });
            }
            let pd = def.param("dissolve").ok_or_else(|| TransitionPlanError::InvalidParameter { id: id.clone(), parameter: "dissolve" })?;
            let ParamValue::Bool(dissolve) = value(e, pd) else {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "dissolve" });
            };
            TransitionOp::Mosaic { block_size_px: block_size_px.max(1.0), dissolve }
        }
        "shape_dissolve" => {
            let size_px = (number("size").max(0.0) * scale).max(4.0);
            let softness_px = number("softness").max(0.0) * scale;
            if !size_px.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "size" });
            }
            if !softness_px.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "softness" });
            }
            TransitionOp::ShapeDissolve {
                shape: choice(e, def, "shape", &id)?,
                size_px,
                softness_px,
                rotation_degrees: number("rotation"),
                order: choice(e, def, "order", &id)?,
                seed: number("seed").max(0.0) as u32,
            }
        }
        "shape_flow" => {
            let softness_px = number("softness").max(0.0) * scale;
            if !softness_px.is_finite() {
                return Err(TransitionPlanError::InvalidParameter { id, parameter: "softness" });
            }
            TransitionOp::ShapeFlow { shape: choice(e, def, "shape", &id)?, direction: choice(e, def, "direction", &id)?, softness_px }
        }
        id if LightOp::matches_id(id) => {
            TransitionOp::Light(LightOp::eval(e, progress, scale)?.ok_or_else(|| TransitionPlanError::UnsupportedOperation { id: id.into() })?)
        }
        id if gpu_motion::matches_id(id) => {
            TransitionOp::Motion(gpu_motion::eval(e, progress, scale)?.ok_or_else(|| TransitionPlanError::UnsupportedOperation { id: id.into() })?)
        }
        id if gpu_grunge::matches_id(id) => {
            TransitionOp::Grunge(gpu_grunge::eval(e, progress, scale)?.ok_or_else(|| TransitionPlanError::UnsupportedOperation { id: id.into() })?)
        }
        _ => return Err(TransitionPlanError::UnsupportedOperation { id }),
    };
    Ok(PlannedTransition { id, op, progress, scale, requirements: ResourceRequirements { sampled_inputs: 2, ..Default::default() } })
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::{EffectKind, ParamValue, effect_defs, find_effect};

    #[test]
    fn normalizes_progress_and_scale_like_cpu() {
        let e = find_effect("cross_dissolve").unwrap().instance();
        for (p, want) in [(f32::NAN, 0.0), (f32::INFINITY, 0.0), (-1.0, 0.0), (2.0, 1.0), (0.37, 0.37)] {
            for scale in [f32::NAN, f32::INFINITY, -1.0, 0.0, 0.5] {
                let plan = evaluate(&e, p, scale).unwrap();
                assert_eq!(plan.progress, want);
                assert_eq!(plan.scale, if scale == 0.5 { 0.5 } else { 1.0 });
            }
        }
    }

    #[test]
    fn every_registered_transition_has_explicit_metadata() {
        let defs: Vec<_> = effect_defs().iter().filter(|d| d.kind == EffectKind::VideoTransition).collect();
        assert_eq!(defs.len(), 112);
        for def in defs {
            match evaluate(&def.instance(), 0.37, 0.5) {
                Ok(plan) => {
                    assert_eq!(plan.id, def.id);
                    assert_eq!(plan.requirements.sampled_inputs, 2);
                }
                Err(TransitionPlanError::UnsupportedOperation { id }) => assert_eq!(id, def.id),
                other => panic!("{}: {other:?}", def.id),
            }
        }
    }

    #[test]
    fn missing_push_parameters_use_registry_defaults() {
        let mut e = find_effect("push").unwrap().instance();
        let expected = evaluate(&e, 0.5, 1.0).unwrap();
        e.params.clear();
        assert_eq!(evaluate(&e, 0.5, 1.0).unwrap(), expected);
        let TransitionOp::Push(params) = expected.op else { panic!("expected typed push") };
        assert_eq!(params.direction, PushDirection::FromWest);
    }

    #[test]
    fn rejects_invalid_push_parameter_before_dispatch() {
        let mut e = find_effect("push").unwrap().instance();
        for value in [ParamValue::Choice(99), ParamValue::Float(1.0)] {
            e.params.get_mut("direction").unwrap().value = value;
            assert!(matches!(evaluate(&e, 0.5, 1.0), Err(TransitionPlanError::InvalidParameter { parameter: "direction", .. })));
        }
        e.params.get_mut("direction").unwrap().value = ParamValue::Choice(3);
        for value in [f64::NAN, f64::INFINITY, -1.0, 101.0] {
            e.params.get_mut("motion_blur").unwrap().value = ParamValue::Float(value);
            assert!(matches!(evaluate(&e, 0.5, 1.0), Err(TransitionPlanError::InvalidParameter { parameter: "motion_blur", .. })));
        }
    }

    #[test]
    fn rejects_wrong_kind_and_unknown_canonical_id() {
        let mut e = find_effect("cross_dissolve").unwrap().instance();
        for id in ["missing_transition", "Cross Dissolve", "gaussian_blur"] {
            e.effect = id.into();
            assert!(matches!(evaluate(&e, 0.5, 1.0), Err(TransitionPlanError::UnknownId { .. })));
        }
    }

    #[test]
    fn wipe_plans_use_registry_defaults_and_typed_values() {
        let mut linear = find_effect("linear_wipe").unwrap().instance();
        let expected = evaluate(&linear, 0.5, 1.0).unwrap();
        linear.params.clear();
        assert_eq!(evaluate(&linear, 0.5, 1.0).unwrap(), expected);
        let TransitionOp::LinearWipe(params) = expected.op else { panic!("expected linear wipe") };
        assert_eq!(params.angle_degrees, 90.0);
        assert_eq!(params.feather, 0.0);
        assert_eq!(params.border_width, 0.0);
        assert_eq!(params.antialias, 1.0);
        assert_eq!(params.border_color_linear, [0.0; 3]);

        let mut soft = find_effect("soft_wipe").unwrap().instance();
        let expected = evaluate(&soft, 0.5, 1.0).unwrap();
        soft.params.clear();
        assert_eq!(evaluate(&soft, 0.5, 1.0).unwrap(), expected);
        let TransitionOp::SoftWipe(params) = expected.op else { panic!("expected soft wipe") };
        assert_eq!(params.angle_degrees, 90.0);
        assert_eq!(params.feather, 300.0);
        assert_eq!(params.antialias, 0.5);

        let mut legacy = find_effect("wipe").unwrap().instance();
        legacy.params.clear();
        assert_eq!(evaluate(&legacy, 0.5, 1.0).unwrap().op, TransitionOp::LegacyWipe { direction: 3 });
    }

    #[test]
    fn rejects_invalid_wipe_choices() {
        let mut linear = find_effect("linear_wipe").unwrap().instance();
        linear.params.get_mut("antialias").unwrap().value = ParamValue::Choice(4);
        assert!(matches!(evaluate(&linear, 0.5, 1.0), Err(TransitionPlanError::InvalidParameter { parameter: "antialias", .. })));

        let mut legacy = find_effect("wipe").unwrap().instance();
        legacy.params.get_mut("direction").unwrap().value = ParamValue::Choice(4);
        assert!(matches!(evaluate(&legacy, 0.5, 1.0), Err(TransitionPlanError::InvalidParameter { parameter: "direction", .. })));
    }

    #[test]
    fn wipe_pixel_parameters_use_cpu_scale_rules() {
        let mut linear = find_effect("linear_wipe").unwrap().instance();
        linear.params.get_mut("feather").unwrap().value = ParamValue::Float(4.0);
        linear.params.get_mut("border_width").unwrap().value = ParamValue::Float(3.0);
        let TransitionOp::LinearWipe(params) = evaluate(&linear, 0.5, 0.5).unwrap().op else { panic!("expected linear wipe") };
        assert_eq!((params.feather, params.border_width, params.antialias), (2.0, 1.5, 0.5));

        let mut soft = find_effect("soft_wipe").unwrap().instance();
        let TransitionOp::SoftWipe(params) = evaluate(&soft, 0.5, 0.5).unwrap().op else { panic!("expected soft wipe") };
        assert_eq!((params.feather, params.antialias), (150.0, 0.25));
        soft.params.get_mut("feather").unwrap().value = ParamValue::Float(2000.0);
        assert!(matches!(evaluate(&soft, 0.5, f32::MAX), Err(TransitionPlanError::InvalidParameter { parameter: "feather", .. })));
    }

    #[test]
    fn simple_dissolve_and_legacy_wipe_defaults_are_typed() {
        let dip = evaluate(&find_effect("dip_to_color").unwrap().instance(), 0.5, 1.0).unwrap();
        let TransitionOp::DipToColor { color_linear, hold } = dip.op else { panic!("expected dip to color") };
        assert_eq!(color_linear, [0.85, 0.2, 0.2].map(filmcraft_color::srgb_to_linear));
        assert_eq!(hold, 0.0);

        let luma = evaluate(&find_effect("luma_fade").unwrap().instance(), 0.5, 1.0).unwrap();
        let TransitionOp::LumaFade { source, softness, invert } = luma.op else { panic!("expected luma fade") };
        assert_eq!((source, invert), (0, false));
        assert!((softness - 0.2).abs() < 1e-6);

        let gradient = evaluate(&find_effect("gradient_wipe").unwrap().instance(), 0.5, 1.0).unwrap();
        let TransitionOp::GradientWipe { softness } = gradient.op else { panic!("expected gradient wipe") };
        assert!((softness - 0.12).abs() < 1e-6);
        for id in ["clock_wipe_legacy", "radial_wipe_legacy"] {
            assert_eq!(evaluate(&find_effect(id).unwrap().instance(), 0.5, 1.0).unwrap().op, TransitionOp::LegacyClockWipe);
        }
    }

    #[test]
    fn field_wipe_routes_preserve_missing_registry_defaults() {
        for id in [
            "clock_wipe",
            "radial_wipe",
            "star_wipe",
            "panel_wipe",
            "plateau_wipe",
            "barn_doors",
            "inset",
            "iris_round",
            "iris_box",
            "iris_diamond",
            "iris_cross",
            "vr_iris_wipe",
        ] {
            let mut e = find_effect(id).unwrap().instance();
            let expected = evaluate(&e, 0.5, 0.5).unwrap();
            e.params.clear();
            assert_eq!(evaluate(&e, 0.5, 0.5).unwrap(), expected, "{id}");
        }
        let mut clock = find_effect("clock_wipe").unwrap().instance();
        clock.params.get_mut("center").unwrap().value = ParamValue::Vec2(filmcraft_geom::Vec2::new(4.0, 2.0));
        let TransitionOp::FieldWipe(FieldWipeParams { kind: FieldWipeKind::Clock { center, .. }, .. }) = evaluate(&clock, 0.5, 0.5).unwrap().op else {
            panic!("expected typed clock wipe")
        };
        assert_eq!(center, [Some(2.0), Some(1.0)]);
        let vr_gradient = evaluate(&find_effect("vr_gradient_wipe").unwrap().instance(), 0.5, 1.0).unwrap();
        let TransitionOp::VrGradientWipe { softness, invert } = vr_gradient.op else { panic!("expected VR gradient wipe") };
        assert!((softness - 0.1).abs() < 1e-6);
        assert!(!invert);
    }

    #[test]
    fn sampled_wipes_apply_defaults_and_scaled_parameters() {
        let mut stretch = find_effect("stretch_wipe").unwrap().instance();
        let expected = evaluate(&stretch, 0.5, 1.0).unwrap();
        stretch.params.clear();
        assert_eq!(evaluate(&stretch, 0.5, 1.0).unwrap(), expected);
        let TransitionOp::StretchWipe { direction, stretch_px } = evaluate(&stretch, 0.5, 0.5).unwrap().op else { panic!("expected stretch wipe") };
        assert_eq!((direction, stretch_px), (3, 100.0));

        let mut neon = find_effect("neon_wipe").unwrap().instance();
        let TransitionOp::NeonWipe { glow_px, antialias, .. } = evaluate(&neon, 0.5, 0.5).unwrap().op else { panic!("expected neon wipe") };
        assert_eq!((glow_px, antialias), (20.0, 0.25));
        neon.params.clear();
        assert_eq!(
            evaluate(&neon, 0.5, 1.0).unwrap().op,
            TransitionOp::NeonWipe {
                angle_degrees: 90.0,
                glow_px: 40.0,
                amount: 1.0,
                color_linear: [0.2, 0.9, 1.0].map(filmcraft_color::srgb_to_linear),
                antialias: 0.5,
            }
        );
    }

    #[test]
    fn noise_and_mosaic_dissolves_keep_defaults_and_seed() {
        let burn = evaluate(&find_effect("burn_alpha").unwrap().instance(), 0.5, 1.0).unwrap();
        let TransitionOp::BurnAlpha { softness, seed, color_linear } = burn.op else { panic!("expected burn alpha") };
        assert!((softness - 0.075).abs() < 1e-6);
        assert_eq!(seed, 1);
        assert_eq!(color_linear, [1.0, 0.45, 0.1].map(filmcraft_color::srgb_to_linear));

        let mosaic = evaluate(&find_effect("mosaic_transition").unwrap().instance(), 0.5, 0.5).unwrap();
        assert_eq!(mosaic.op, TransitionOp::Mosaic { block_size_px: 32.0, dissolve: true });
    }

    #[test]
    fn smart_shape_transitions_use_scaled_pixel_parameters() {
        let shape = evaluate(&find_effect("shape_dissolve").unwrap().instance(), 0.5, 0.5).unwrap();
        assert_eq!(shape.op, TransitionOp::ShapeDissolve { shape: 0, size_px: 60.0, softness_px: 1.0, rotation_degrees: 0.0, order: 0, seed: 1 });
        let flow = evaluate(&find_effect("shape_flow").unwrap().instance(), 0.5, 0.5).unwrap();
        assert_eq!(flow.op, TransitionOp::ShapeFlow { shape: 0, direction: 3, softness_px: 2.0 });
    }
}

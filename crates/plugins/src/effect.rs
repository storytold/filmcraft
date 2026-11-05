//! Plug-in `effect` contributions as FilmCraft video effects.
//!
//! A [`Surface`] of kind `effect` becomes two entries. The catalog
//! ([`filmcraft_engine::project::register_plugin_effect`]) lists it in the Effects panel and lets
//! `find_effect` resolve it. The render hook ([`filmcraft_engine::render::effects::register_plugin_fx`])
//! runs it: `effects::apply` reaches the hook for any id no built-in handles. Such an effect is
//! not in the GPU effect list, so `plan` draws the layer on the CPU and uploads it as a canvas.
//!
//! The layer image is premultiplied linear `f32`. A contribution that declares `u8` samples or
//! straight alpha gets a converted copy. The `u8` bytes are the linear values scaled to 0..255, as
//! in the EffectCraft port. The host banding, hygiene and fault rules apply
//! ([`craft_plugin_host::process`]).
//!
//! A point parameter is in 0..1 of the layer. A `NaN` ("auto") point is the centre.

use std::sync::Arc;

use craft_command::Owner;
use craft_plugin_abi::descriptor::{Alpha, Sample};
use craft_plugin_abi::manifest::{AlphaMode, Contribution, Kind, ParamSpec, ParamType, SampleType};
use craft_plugin_host::contrib::surface::Surface;
use craft_plugin_host::process::{ImageFormat, ImageJob, ProcessOpts};
use filmcraft_engine::project::{EffectDef, EffectInstance, EffectKind, ParamDef, ParamKind, ParamValue, register_plugin_effect, unregister_plugin_effect};
use filmcraft_engine::render::Image;
use filmcraft_engine::render::effects::{FxCtx, register_plugin_fx, unregister_plugin_fx};
use serde_json::{Map, Value, json};

/// Folder under "Video Effects" for an effect whose contribution names no category.
const DEFAULT_CATEGORY: &str = "Plug-ins";

/// The effect id of a contribution: the plug-in id, plus the contribution id unless it is the
/// only one a v1 shim describes (`effect`).
pub fn effect_id(owner: &Owner, contribution_id: &str) -> String {
    if contribution_id == "effect" { owner.plugin_id.to_string() } else { format!("{}.{contribution_id}", owner.plugin_id) }
}

/// A catalog def is `'static`. A reload leaks the strings of one def per effect.
fn leak(s: &str) -> &'static str {
    Box::leak(s.to_owned().into_boxed_str())
}

fn number(v: &Option<Value>, fallback: f64) -> f64 {
    v.as_ref().and_then(Value::as_f64).unwrap_or(fallback)
}

fn floats<const N: usize>(v: &Option<Value>, fallback: [f64; N]) -> [f64; N] {
    let mut out = fallback;
    for (slot, x) in out.iter_mut().zip(v.iter().filter_map(Value::as_array).flatten()) {
        *slot = x.as_f64().unwrap_or(*slot);
    }
    out
}

fn param_def(id: &str, p: &ParamSpec) -> Result<ParamDef, String> {
    let (kind, default, animatable) = match p.ty {
        ParamType::Number | ParamType::Int => {
            let (min, max) = (p.min.unwrap_or(0.0), p.max.unwrap_or(100.0));
            let decimals = if p.ty == ParamType::Int { 0 } else { p.decimals.map_or(2, |d| d.clamp(0, 6) as u8) };
            (ParamKind::Float { min, max, soft_min: min, soft_max: max, unit: "", decimals }, ParamValue::Float(number(&p.default, min)), true)
        }
        ParamType::Angle => (ParamKind::Angle, ParamValue::Float(number(&p.default, 0.0)), true),
        ParamType::Bool => (ParamKind::Bool, ParamValue::Bool(p.default.as_ref().and_then(Value::as_bool).unwrap_or(false)), false),
        ParamType::Choice => {
            let options = p.options.clone().unwrap_or_default();
            let at = p.default.as_ref().and_then(Value::as_str).and_then(|d| options.iter().position(|o| o == d)).unwrap_or(0) as u32;
            let leaked: Vec<&'static str> = options.iter().map(|o| leak(o)).collect();
            (ParamKind::Choice(Box::leak(leaked.into_boxed_slice())), ParamValue::Choice(at), false)
        }
        ParamType::Point2 => {
            let [x, y] = floats(&p.default, [0.5, 0.5]);
            (ParamKind::Point, ParamValue::Vec2(filmcraft_engine::project::Vec2::new(x, y)), true)
        }
        ParamType::Color => (ParamKind::Color, ParamValue::Color(floats(&p.default, [1.0; 4]).map(|c| c as f32)), true),
        other => return Err(format!("parameter `{id}` is {other:?}, which an effect cannot show")),
    };
    Ok(ParamDef { id: leak(id), label: leak(p.label.as_deref().unwrap_or(id)), kind, default, animatable, group: None })
}

/// A contribution as the Effects panel lists it.
pub fn def_of(surface: &Surface) -> Result<EffectDef, String> {
    let c = &surface.contribution;
    let id = effect_id(&surface.owner, &c.id);
    let params = c.params.iter().flatten().map(|(k, p)| param_def(k, p)).collect::<Result<Vec<_>, String>>()?;
    let category = c.category.as_deref().unwrap_or(DEFAULT_CATEGORY);
    Ok(EffectDef {
        name: leak(c.label.as_deref().unwrap_or(&id)),
        id: leak(&id),
        kind: EffectKind::Video,
        category: Box::leak(vec!["Video Effects", leak(category)].into_boxed_slice()),
        params,
        intrinsic: false,
        accelerated: false,
        float32: true,
        yuv: false,
    })
}

/// The `params` object of a host call, from the instance at the context's time.
fn params_json(def: &EffectDef, e: &EffectInstance, cx: &FxCtx) -> Value {
    let mut out = Map::new();
    for p in &def.params {
        let value = e.param(p.id).map_or_else(|| p.default.clone(), |param| param.value_at(cx.t));
        let json = match (&p.kind, value) {
            (ParamKind::Float { decimals: 0, .. }, ParamValue::Float(v)) => json!(v.round() as i64),
            (_, ParamValue::Float(v)) => json!(v),
            (_, ParamValue::Bool(v)) => json!(v),
            (ParamKind::Choice(options), ParamValue::Choice(i)) => json!(options.get(i as usize).copied().unwrap_or_default()),
            (_, ParamValue::Vec2(v)) => json!([if v.x.is_nan() { 0.5 } else { v.x }, if v.y.is_nan() { 0.5 } else { v.y }]),
            (_, ParamValue::Color(c)) => json!(c),
            _ => continue,
        };
        out.insert(p.id.to_owned(), json);
    }
    Value::Object(out)
}

/// The buffer a plug-in gets: the contribution's samples and alpha.
#[derive(Clone, Copy)]
struct Wire {
    sample: Sample,
    alpha: Alpha,
}

fn wire_of(c: &Contribution) -> Result<Wire, String> {
    let process = c.process.as_ref();
    let samples = process.and_then(|p| p.sample.as_ref());
    let sample = match samples {
        None => Sample::F32,
        Some(s) if s.contains(&SampleType::F32) => Sample::F32,
        Some(s) if s.contains(&SampleType::U8) => Sample::U8,
        Some(s) => return Err(format!("it takes {s:?} samples, and an effect gives f32 or u8")),
    };
    let alpha = match process.and_then(|p| p.alpha) {
        None | Some(AlphaMode::Premultiplied) => Alpha::Premultiplied,
        Some(AlphaMode::Straight) => Alpha::Straight,
        Some(AlphaMode::None) => return Err("it takes images without alpha, and an effect has RGBA".into()),
    };
    Ok(Wire { sample, alpha })
}

fn straight(p: [f32; 4]) -> [f32; 4] {
    if p[3] > 0.0 { [p[0] / p[3], p[1] / p[3], p[2] / p[3], p[3]] } else { [0.0; 4] }
}

fn premultiplied(p: [f32; 4]) -> [f32; 4] {
    [p[0] * p[3], p[1] * p[3], p[2] * p[3], p[3]]
}

fn to_u8(v: f32) -> u8 {
    (v.clamp(0.0, 1.0) * 255.0).round() as u8
}

/// The image as the wire bytes: `f32` or `u8` samples, premultiplied or straight.
fn to_wire(img: &Image, wire: Wire) -> Vec<u8> {
    let pixels = img.px.chunks_exact(4).map(|p| {
        let p = [p[0], p[1], p[2], p[3]];
        if wire.alpha == Alpha::Straight { straight(p) } else { p }
    });
    match wire.sample {
        Sample::F32 => pixels.flat_map(|p| p.into_iter().flat_map(f32::to_ne_bytes)).collect(),
        _ => pixels.flat_map(|p| p.map(to_u8)).collect(),
    }
}

/// Write the wire bytes back into the image, in the same layout [`to_wire`] made.
fn from_wire(img: &mut Image, bytes: &[u8], wire: Wire) {
    let width = if wire.sample == Sample::F32 { 16 } else { 4 };
    for (px, src) in img.px.chunks_exact_mut(4).zip(bytes.chunks_exact(width)) {
        let q: [f32; 4] = if wire.sample == Sample::F32 {
            std::array::from_fn(|i| f32::from_ne_bytes([src[i * 4], src[i * 4 + 1], src[i * 4 + 2], src[i * 4 + 3]]))
        } else {
            std::array::from_fn(|i| f32::from(src[i]) / 255.0)
        };
        px.copy_from_slice(&if wire.alpha == Alpha::Straight { premultiplied(q) } else { q });
    }
}

/// Run `surface` over `img`. On an error the image stays as it was.
fn run(surface: &Surface, def: &EffectDef, wire: Wire, img: &mut Image, e: &EffectInstance, cx: &FxCtx) -> Result<(), String> {
    let format = ImageFormat {
        width: u32::try_from(img.w).map_err(|e| e.to_string())?,
        height: u32::try_from(img.h).map_err(|e| e.to_string())?,
        channels: 4,
        sample: wire.sample,
        alpha: wire.alpha,
        color_mode: 0,
        depth: if wire.sample == Sample::U8 { 8 } else { 32 },
    };
    let mut bytes = to_wire(img, wire);
    let mut job = ImageJob::new(format, &mut bytes);
    job.scale = f64::from(cx.px_scale);
    job.time = cx.seconds;
    surface.process_image(&mut job, &params_json(def, e, cx), &ProcessOpts::default()).map_err(|e| e.to_string())?;
    from_wire(img, &bytes, wire);
    Ok(())
}

/// Put an `effect` surface in the catalog and the render hook. A surface of another kind is ignored.
pub fn register(surface: &Surface) -> Result<(), String> {
    if surface.contribution.kind != Kind::Effect {
        return Ok(());
    }
    let wire = wire_of(&surface.contribution)?;
    let def: &'static EffectDef = Box::leak(Box::new(def_of(surface)?));
    let surface = surface.clone();
    register_plugin_fx(def.id, Arc::new(move |img, e, cx| run(&surface, def, wire, img, e, cx)));
    register_plugin_effect(def);
    Ok(())
}

/// Take an `effect` surface out of the catalog and the render hook.
pub fn unregister(owner: &Owner, contribution_id: &str) {
    let id = effect_id(owner, contribution_id);
    unregister_plugin_effect(&id);
    unregister_plugin_fx(&id);
}

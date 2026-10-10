//! Graphic clips: text and shape layers.
//!
//! A graphic clip is a track item whose project item is an [`ItemKind::Graphic`](crate::ItemKind)
//! source (a transparent canvas the size of the sequence frame). Its **layers** are effect
//! instances of the hidden effects [`TEXT_LAYER`] and [`SHAPE_LAYER`] in the clip's effect list,
//! in paint order (first = back). Being effect instances, layers serialize with the project, get
//! keyframes for every animatable property (Source Text holds), show in Effect Controls, and copy
//! with the clip — like Premiere's Text / Shape components. Standard effects on the clip apply to
//! the composed graphic; Motion and Opacity apply last.
//!
//! [`eval_layer`] evaluates a layer at a clip time into a plain [`LayerSpec`] that renderers and
//! editors use.

use filmcraft_geom::Vec2;
use filmcraft_time::Tick;

use crate::effect::{EffectDef, EffectInstance, EffectKind, ParamDef, ParamKind};
use crate::keyframe::{Param, ParamValue};

pub const TEXT_LAYER: &str = "graphic_text";
pub const SHAPE_LAYER: &str = "graphic_shape";

pub const ALIGN_OPTS: &[&str] = &["Left", "Center", "Right", "Justify"];
pub const CAPS_OPTS: &[&str] = &["Normal", "All Caps", "Small Caps"];
pub const STROKE_OPTS: &[&str] = &["Outer", "Center", "Inner"];
pub const SHAPE_OPTS: &[&str] = &["Rectangle", "Ellipse", "Polygon", "Path"];
pub const FILL_KIND_OPTS: &[&str] = &["Solid", "Linear Gradient"];

/// Whether an effect instance is a graphic layer.
pub fn is_layer_id(id: &str) -> bool {
    id == TEXT_LAYER || id == SHAPE_LAYER
}

pub fn is_layer(e: &EffectInstance) -> bool {
    is_layer_id(&e.effect)
}

fn p(id: &'static str, label: &'static str, kind: ParamKind, default: ParamValue, animatable: bool, group: &'static str) -> ParamDef {
    ParamDef { id, label, kind, default, animatable, group: if group.is_empty() { None } else { Some(group) } }
}
fn fl(id: &'static str, label: &'static str, def: f64, (min, max): (f64, f64), (smin, smax): (f64, f64), unit: &'static str, group: &'static str) -> ParamDef {
    p(id, label, ParamKind::Float { min, max, soft_min: smin, soft_max: smax, unit, decimals: 1 }, ParamValue::Float(def), true, group)
}
fn bo(id: &'static str, label: &'static str, v: bool, group: &'static str) -> ParamDef {
    p(id, label, ParamKind::Bool, ParamValue::Bool(v), false, group)
}
fn co(id: &'static str, label: &'static str, c: [f32; 4], group: &'static str) -> ParamDef {
    p(id, label, ParamKind::Color, ParamValue::Color(c), true, group)
}
fn chc(id: &'static str, label: &'static str, opts: &'static [&'static str], def: u32, group: &'static str) -> ParamDef {
    p(id, label, ParamKind::Choice(opts), ParamValue::Choice(def), false, group)
}
fn tx(id: &'static str, label: &'static str, def: &str, animatable: bool, group: &'static str) -> ParamDef {
    p(id, label, ParamKind::Text, ParamValue::Text(def.into()), animatable, group)
}

fn appearance() -> Vec<ParamDef> {
    const A: &str = "Appearance";
    vec![
        bo("fill", "Fill", true, A),
        co("fill_color", "Fill Color", [1.0, 1.0, 1.0, 1.0], A),
        chc("fill_kind", "Fill Type", FILL_KIND_OPTS, 0, A),
        co("gradient_start", "Gradient Start", [1.0, 1.0, 1.0, 1.0], A),
        co("gradient_end", "Gradient End", [0.15, 0.15, 0.15, 1.0], A),
        p("gradient_angle", "Gradient Angle", ParamKind::Angle, ParamValue::Float(0.0), true, A),
        bo("stroke", "Stroke", false, A),
        co("stroke_color", "Stroke Color", [0.0, 0.0, 0.0, 1.0], A),
        fl("stroke_width", "Stroke Width", 4.0, (0.0, 1000.0), (0.0, 100.0), "", A),
        chc("stroke_type", "Stroke Type", STROKE_OPTS, 0, A),
        bo("stroke2", "Stroke 2", false, A),
        co("stroke2_color", "Stroke 2 Color", [1.0, 0.85, 0.2, 1.0], A),
        fl("stroke2_width", "Stroke 2 Width", 10.0, (0.0, 1000.0), (0.0, 100.0), "", A),
        chc("stroke2_type", "Stroke 2 Type", STROKE_OPTS, 0, A),
        bo("background", "Background", false, A),
        co("background_color", "Background Color", [0.0, 0.0, 0.0, 1.0], A),
        fl("background_opacity", "Background Opacity", 75.0, (0.0, 100.0), (0.0, 100.0), "%", A),
        fl("background_size", "Background Size", 12.0, (0.0, 1000.0), (0.0, 100.0), "", A),
        fl("background_radius", "Background Corner Radius", 0.0, (0.0, 1000.0), (0.0, 100.0), "", A),
        bo("shadow", "Shadow", false, A),
        co("shadow_color", "Shadow Color", [0.0, 0.0, 0.0, 1.0], A),
        fl("shadow_opacity", "Shadow Opacity", 75.0, (0.0, 100.0), (0.0, 100.0), "%", A),
        p("shadow_angle", "Shadow Angle", ParamKind::Angle, ParamValue::Float(135.0), true, A),
        fl("shadow_distance", "Shadow Distance", 10.0, (0.0, 1000.0), (0.0, 100.0), "", A),
        fl("shadow_size", "Shadow Size", 0.0, (0.0, 1000.0), (0.0, 100.0), "", A),
        fl("shadow_blur", "Shadow Blur", 40.0, (0.0, 1000.0), (0.0, 200.0), "", A),
    ]
}

fn transform() -> Vec<ParamDef> {
    const T: &str = "Transform";
    vec![
        p("position", "Position", ParamKind::Point, ParamValue::Vec2(Vec2::new(f64::NAN, f64::NAN)), true, T),
        p("anchor", "Anchor Point", ParamKind::Point, ParamValue::Vec2(Vec2::new(0.0, 0.0)), true, T),
        fl("scale", "Scale", 100.0, (0.0, 100_000.0), (0.0, 400.0), "%", T),
        fl("scale_width", "Scale Width", 100.0, (0.0, 100_000.0), (0.0, 400.0), "%", T),
        bo("uniform_scale", "Uniform Scale", true, T),
        p("rotation", "Rotation", ParamKind::Angle, ParamValue::Float(0.0), true, T),
        fl("opacity", "Opacity", 100.0, (0.0, 100.0), (0.0, 100.0), "%", T),
    ]
}

fn layer_def(id: &'static str, name: &'static str, mut params: Vec<ParamDef>) -> EffectDef {
    params.extend(appearance());
    params.extend(transform());
    EffectDef { id, name, kind: EffectKind::Video, category: &[], params, intrinsic: false, accelerated: true, float32: true, yuv: false }
}

/// Definitions of the two layer kinds (appended to the effect registry; not in the Effects panel).
pub(crate) fn layer_defs() -> Vec<EffectDef> {
    const X: &str = "Text";
    const S: &str = "Shape";
    vec![
        layer_def(
            TEXT_LAYER,
            "Text",
            vec![
                tx("name", "Layer Name", "", false, ""),
                tx("text", "Source Text", "", true, ""),
                tx("font", "Font", "Inter", false, X),
                tx("font_style", "Font Style", "Regular", false, X),
                fl("size", "Font Size", 100.0, (1.0, 2000.0), (6.0, 400.0), "", X),
                chc("align", "Alignment", ALIGN_OPTS, 0, X),
                fl("tracking", "Tracking", 0.0, (-1000.0, 10_000.0), (-200.0, 1000.0), "", X),
                bo("kerning", "Metrics Kerning", true, X),
                bo("ligatures", "Ligatures", true, X),
                fl("leading", "Leading", 0.0, (-5000.0, 5000.0), (-100.0, 400.0), "", X),
                fl("baseline_shift", "Baseline Shift", 0.0, (-5000.0, 5000.0), (-100.0, 100.0), "", X),
                bo("faux_bold", "Faux Bold", false, X),
                bo("faux_italic", "Faux Italic", false, X),
                chc("caps", "Capitalisation", CAPS_OPTS, 0, X),
                bo("underline", "Underline", false, X),
                fl("box_width", "Text Box Width", 0.0, (0.0, 100_000.0), (0.0, 3840.0), "", X),
                fl("box_height", "Text Box Height", 0.0, (0.0, 100_000.0), (0.0, 2160.0), "", X),
                bo("vertical", "Vertical Text", false, X),
            ],
        ),
        layer_def(
            SHAPE_LAYER,
            "Shape",
            vec![
                tx("name", "Layer Name", "", false, ""),
                chc("shape", "Shape", SHAPE_OPTS, 0, S),
                p("size", "Size", ParamKind::Point, ParamValue::Vec2(Vec2::new(400.0, 200.0)), true, S),
                fl("sides", "Polygon Sides", 6.0, (3.0, 64.0), (3.0, 16.0), "", S),
                fl("corner_radius", "Corner Radius", 0.0, (0.0, 10_000.0), (0.0, 200.0), "", S),
                p("points", "Path Points", ParamKind::Curve { hue: false }, ParamValue::Curve(vec![]), false, S),
            ],
        ),
    ]
}

/// A linear fill across the layer's local bounds. Colours are sRGB-encoded straight RGBA.
/// Angle is degrees clockwise on screen (y down); 0 runs left to right.
#[derive(Clone, Debug, PartialEq)]
pub struct LinearGradient {
    pub start: [f32; 4],
    pub end: [f32; 4],
    pub angle: f32,
}

/// Fill / strokes / background / shadow of a layer (colours are sRGB-encoded straight RGBA).
#[derive(Clone, Debug, PartialEq)]
pub struct Appearance {
    pub fill: Option<[f32; 4]>,
    /// Set when Fill Type is Linear Gradient. Replaces the solid fill. A text run with its own
    /// fill colour stays solid; the other characters keep the ramp.
    pub gradient: Option<LinearGradient>,
    /// (colour, width px, type: 0 outer, 1 centre, 2 inner), outermost last.
    pub strokes: Vec<([f32; 4], f32, u32)>,
    /// (colour with opacity in alpha, padding px, corner radius px).
    pub background: Option<([f32; 4], f32, f32)>,
    pub shadow: Option<Shadow>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Shadow {
    /// Colour with opacity in alpha.
    pub color: [f32; 4],
    /// Offset in layer pixels (from angle and distance; 135° casts down-right).
    pub offset: (f32, f32),
    pub size: f32,
    pub blur: f32,
}

/// Layer → graphic-source transform parameters.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerTransform {
    pub position: Vec2,
    pub anchor: Vec2,
    pub scale: Vec2,
    pub rotation: f64,
    pub opacity: f32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct TextProps {
    pub text: String,
    pub font: String,
    pub style: String,
    pub size: f32,
    /// 0 left, 1 centre, 2 right, 3 justify.
    pub align: u32,
    pub tracking: f32,
    pub kerning: bool,
    pub ligatures: bool,
    pub leading: f32,
    pub baseline_shift: f32,
    pub faux_bold: bool,
    pub faux_italic: bool,
    /// 0 normal, 1 all caps, 2 small caps.
    pub caps: u32,
    pub underline: bool,
    /// Paragraph-text box width (0 = point text).
    pub box_width: f32,
    /// Paragraph-text box height: lines that do not fit are not shown (0 = as tall as the text).
    pub box_height: f32,
    /// Vertical text (characters stacked top to bottom; columns right to left).
    pub vertical: bool,
    /// Per-character style overrides as *byte* ranges of `text` (sorted, non-overlapping).
    pub runs: Vec<(std::ops::Range<usize>, crate::graphic_design::CharStyle)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct ShapeProps {
    /// 0 rectangle, 1 ellipse, 2 polygon, 3 path.
    pub shape: u32,
    pub size: (f32, f32),
    pub sides: u32,
    pub corner_radius: f32,
    /// Path vertices in layer pixels (shape 3), closed.
    pub points: Vec<[f32; 2]>,
}

#[derive(Clone, Debug, PartialEq)]
pub enum LayerContent {
    Text(TextProps),
    Shape(ShapeProps),
}

/// A layer evaluated at one time.
#[derive(Clone, Debug, PartialEq)]
pub struct LayerSpec {
    pub name: String,
    pub enabled: bool,
    /// Stable layer id within the clip (0 = none).
    pub uid: u64,
    /// Responsive Design – Position.
    pub pin: Option<crate::graphic_design::Pin>,
    pub content: LayerContent,
    pub appearance: Appearance,
    pub transform: LayerTransform,
}

fn val(e: &EffectInstance, id: &str, t: Tick) -> ParamValue {
    match e.params.get(id) {
        Some(p) => p.value_at(t),
        None => crate::effect::find_effect(&e.effect).and_then(|d| d.param(id)).map(|d| d.default.clone()).unwrap_or(ParamValue::Float(0.0)),
    }
}
fn ff(e: &EffectInstance, id: &str, t: Tick) -> f32 {
    val(e, id, t).as_f64().unwrap_or(0.0) as f32
}
fn bb(e: &EffectInstance, id: &str, t: Tick) -> bool {
    val(e, id, t).as_bool().unwrap_or(false)
}
fn cc(e: &EffectInstance, id: &str, t: Tick) -> [f32; 4] {
    val(e, id, t).as_color().unwrap_or([1.0, 1.0, 1.0, 1.0])
}
fn ch(e: &EffectInstance, id: &str, t: Tick) -> u32 {
    match val(e, id, t) {
        ParamValue::Choice(c) => c,
        v => v.as_f64().unwrap_or(0.0) as u32,
    }
}
fn ss(e: &EffectInstance, id: &str, t: Tick) -> String {
    match val(e, id, t) {
        ParamValue::Text(s) => s,
        _ => String::new(),
    }
}
fn vv(e: &EffectInstance, id: &str, t: Tick) -> Vec2 {
    val(e, id, t).as_vec2().unwrap_or_default()
}

/// Evaluate a layer at clip (media) time `t`. `frame` is the graphic's canvas size; a NaN position
/// means the canvas centre.
pub fn eval_layer(e: &EffectInstance, t: Tick, frame: (u32, u32)) -> Option<LayerSpec> {
    let content = match e.effect.as_str() {
        TEXT_LAYER => LayerContent::Text({
            let text = ss(e, "text", t);
            let runs = byte_runs(&text, e.layer.as_ref().map_or(&[][..], |x| &x.runs[..]));
            TextProps {
                text,
                runs,
                font: ss(e, "font", t),
                style: ss(e, "font_style", t),
                size: ff(e, "size", t).max(0.1),
                align: ch(e, "align", t),
                tracking: ff(e, "tracking", t),
                kerning: bb(e, "kerning", t),
                ligatures: bb(e, "ligatures", t),
                leading: ff(e, "leading", t),
                baseline_shift: ff(e, "baseline_shift", t),
                faux_bold: bb(e, "faux_bold", t),
                faux_italic: bb(e, "faux_italic", t),
                caps: ch(e, "caps", t),
                underline: bb(e, "underline", t),
                box_width: ff(e, "box_width", t).max(0.0),
                box_height: ff(e, "box_height", t).max(0.0),
                vertical: bb(e, "vertical", t),
            }
        }),
        SHAPE_LAYER => {
            let sz = vv(e, "size", t);
            LayerContent::Shape(ShapeProps {
                shape: ch(e, "shape", t),
                size: (sz.x.max(0.0) as f32, sz.y.max(0.0) as f32),
                sides: ff(e, "sides", t).round().clamp(3.0, 64.0) as u32,
                corner_radius: ff(e, "corner_radius", t).max(0.0),
                points: val(e, "points", t).as_curve().map(<[_]>::to_vec).unwrap_or_default(),
            })
        }
        _ => return None,
    };
    let mut strokes = Vec::new();
    for (on, c, w, k) in [("stroke", "stroke_color", "stroke_width", "stroke_type"), ("stroke2", "stroke2_color", "stroke2_width", "stroke2_type")] {
        if bb(e, on, t) && ff(e, w, t) > 0.0 {
            strokes.push((cc(e, c, t), ff(e, w, t), ch(e, k, t)));
        }
    }
    let background = bb(e, "background", t).then(|| {
        let mut c = cc(e, "background_color", t);
        c[3] *= (ff(e, "background_opacity", t) / 100.0).clamp(0.0, 1.0);
        (c, ff(e, "background_size", t).max(0.0), ff(e, "background_radius", t).max(0.0))
    });
    let shadow = bb(e, "shadow", t).then(|| {
        let mut c = cc(e, "shadow_color", t);
        c[3] *= (ff(e, "shadow_opacity", t) / 100.0).clamp(0.0, 1.0);
        // Premiere: 135° casts the shadow down and to the right.
        let a = (ff(e, "shadow_angle", t) - 90.0).to_radians();
        let d = ff(e, "shadow_distance", t);
        Shadow { color: c, offset: (a.cos() * d, a.sin() * d), size: ff(e, "shadow_size", t).max(0.0), blur: ff(e, "shadow_blur", t).max(0.0) }
    });
    let pos = vv(e, "position", t);
    let position = if pos.x.is_nan() || pos.y.is_nan() { Vec2::new(frame.0 as f64 / 2.0, frame.1 as f64 / 2.0) } else { pos };
    let s = ff(e, "scale", t) as f64 / 100.0;
    let sw = if bb(e, "uniform_scale", t) { s } else { ff(e, "scale_width", t) as f64 / 100.0 };
    Some(LayerSpec {
        name: ss(e, "name", t),
        enabled: e.enabled,
        uid: e.layer.as_ref().map_or(0, |x| x.uid),
        pin: e.layer.as_ref().and_then(|x| x.pin.clone()).filter(|p| p.any()),
        content,
        appearance: Appearance {
            fill: bb(e, "fill", t).then(|| cc(e, "fill_color", t)),
            gradient: (bb(e, "fill", t) && ch(e, "fill_kind", t) == 1).then(|| LinearGradient {
                start: cc(e, "gradient_start", t),
                end: cc(e, "gradient_end", t),
                angle: ff(e, "gradient_angle", t),
            }),
            strokes,
            background,
            shadow,
        },
        transform: LayerTransform {
            position,
            anchor: vv(e, "anchor", t),
            scale: Vec2::new(sw, s),
            rotation: ff(e, "rotation", t) as f64,
            opacity: (ff(e, "opacity", t) / 100.0).clamp(0.0, 1.0),
        },
    })
}

/// Character-offset style runs → byte ranges of `text` (clamped; empty runs dropped).
pub fn byte_runs(text: &str, runs: &[crate::graphic_design::StyleRun]) -> Vec<(std::ops::Range<usize>, crate::graphic_design::CharStyle)> {
    if runs.is_empty() {
        return Vec::new();
    }
    let offs: Vec<usize> = text.char_indices().map(|(b, _)| b).chain(std::iter::once(text.len())).collect();
    let at = |c: usize| offs[c.min(offs.len() - 1)];
    runs.iter().map(|r| (at(r.start)..at(r.end), r.style.clone())).filter(|(r, s)| r.start < r.end && !s.is_empty()).collect()
}

/// Character count of a text layer's source text at time `t` (for style-run commands).
pub fn text_chars(e: &EffectInstance, t: Tick) -> usize {
    ss(e, "text", t).chars().count()
}

fn set(e: &mut EffectInstance, id: &str, v: ParamValue) {
    e.params.insert(id.to_string(), Param::new(v));
}

/// A fresh instance of a built-in layer effect (bare, with no parameters, if it were ever
/// missing from the registry; `set` adds the ones the constructors fill in).
fn layer_instance(id: &str) -> EffectInstance {
    crate::effect::find_effect(id).map(crate::effect::EffectDef::instance).unwrap_or_else(|| EffectInstance {
        effect: id.to_string(),
        enabled: true,
        params: Default::default(),
        masks: Vec::new(),
        post_fader: false,
        essential: false,
        layer: None,
    })
}

/// A new text layer at `position` (graphic canvas pixels).
pub fn new_text_layer(text: &str, position: Vec2, size: f64) -> EffectInstance {
    let mut e = layer_instance(TEXT_LAYER);
    set(&mut e, "text", ParamValue::Text(text.into()));
    set(&mut e, "position", ParamValue::Vec2(position));
    set(&mut e, "size", ParamValue::Float(size));
    e
}

/// A new vertical text layer (Graphics ▸ New Layer ▸ Vertical Text).
pub fn new_vertical_text_layer(text: &str, position: Vec2, size: f64) -> EffectInstance {
    let mut e = new_text_layer(text, position, size);
    set(&mut e, "vertical", ParamValue::Bool(true));
    e
}

/// Reset a layer's parameters to their defaults, keeping its content (name, text, shape kind,
/// geometry and path) — Graphics ▸ Reset All Parameters.
pub fn reset_layer_params(e: &mut EffectInstance) {
    const KEEP: &[&str] = &["name", "text", "shape", "size", "sides", "points", "vertical", "box_width", "box_height"];
    let Some(def) = crate::effect::find_effect(&e.effect) else { return };
    let mut fresh = def.instance();
    for k in KEEP {
        if let Some(p) = e.params.remove(*k) {
            fresh.params.insert((*k).to_string(), p);
        }
    }
    e.params = fresh.params;
}

/// A new shape layer centred at `position`.
pub fn new_shape_layer(shape: u32, position: Vec2, size: Vec2, points: Vec<[f32; 2]>) -> EffectInstance {
    let mut e = layer_instance(SHAPE_LAYER);
    set(&mut e, "shape", ParamValue::Choice(shape.min(3)));
    set(&mut e, "position", ParamValue::Vec2(position));
    set(&mut e, "size", ParamValue::Vec2(size));
    set(&mut e, "points", ParamValue::Curve(points));
    // shapes default to a red fill (Premiere's new-shape default is a coloured fill)
    set(&mut e, "fill_color", ParamValue::Color([0.85, 0.2, 0.25, 1.0]));
    e
}

/// Display name of a layer: its name, else the text (first line), else "Shape 01"-style.
pub fn layer_display_name(e: &EffectInstance, index: usize) -> String {
    let name = ss(e, "name", Tick::ZERO);
    if !name.is_empty() {
        return name;
    }
    if e.effect == TEXT_LAYER {
        let t = ss(e, "text", Tick::ZERO);
        let first = t.lines().next().unwrap_or("").trim().to_string();
        if first.is_empty() { "Text".into() } else { first.chars().take(40).collect() }
    } else {
        format!("Shape {:02}", index + 1)
    }
}

/// Indices (into `effects`) of the graphic layers, in paint order.
pub fn layer_indices(effects: &[EffectInstance]) -> Vec<usize> {
    effects.iter().enumerate().filter(|(_, e)| is_layer(e)).map(|(i, _)| i).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn layers_round_trip_and_evaluate() {
        let mut e = new_text_layer("Hello", Vec2::new(100.0, 200.0), 72.0);
        e.params.get_mut("fill_color").unwrap().value = ParamValue::Color([1.0, 0.0, 0.0, 1.0]);
        let json = serde_json::to_string(&e).unwrap();
        let back: EffectInstance = serde_json::from_str(&json).unwrap();
        assert_eq!(back, e);
        let s = eval_layer(&back, Tick::ZERO, (1920, 1080)).unwrap();
        let LayerContent::Text(t) = &s.content else { panic!() };
        assert_eq!(t.text, "Hello");
        assert_eq!(t.size, 72.0);
        assert_eq!(t.font, "Inter");
        assert_eq!(s.appearance.fill, Some([1.0, 0.0, 0.0, 1.0]));
        assert!(s.appearance.gradient.is_none(), "solid fill stays solid");
        assert_eq!(s.transform.position, Vec2::new(100.0, 200.0));
        assert!(s.appearance.strokes.is_empty() && s.appearance.shadow.is_none());
        e.params.get_mut("fill_kind").unwrap().value = ParamValue::Choice(1);
        e.params.get_mut("gradient_start").unwrap().value = ParamValue::Color([1.0, 0.0, 0.0, 1.0]);
        e.params.get_mut("gradient_end").unwrap().value = ParamValue::Color([0.0, 0.0, 1.0, 1.0]);
        e.params.get_mut("gradient_angle").unwrap().value = ParamValue::Float(90.0);
        let g = eval_layer(&e, Tick::ZERO, (1920, 1080)).unwrap().appearance.gradient.expect("linear fill");
        assert_eq!(g.start, [1.0, 0.0, 0.0, 1.0]);
        assert_eq!(g.end, [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(g.angle, 90.0);
        assert!(is_layer(&e));
        assert_eq!(layer_display_name(&e, 0), "Hello");
    }

    #[test]
    fn defaults_centre_and_shadow_direction() {
        let mut e = crate::effect::find_effect(SHAPE_LAYER).unwrap().instance();
        e.params.get_mut("shadow").unwrap().value = ParamValue::Bool(true);
        let s = eval_layer(&e, Tick::ZERO, (1920, 1080)).unwrap();
        assert_eq!(s.transform.position, Vec2::new(960.0, 540.0));
        let sh = s.appearance.shadow.unwrap();
        assert!(sh.offset.0 > 5.0 && sh.offset.1 > 5.0, "135° casts down-right: {:?}", sh.offset);
        assert_eq!(layer_display_name(&e, 2), "Shape 03");
    }

    #[test]
    fn source_text_keyframes_hold() {
        let mut e = new_text_layer("A", Vec2::new(0.0, 0.0), 50.0);
        let p = e.params.get_mut("text").unwrap();
        p.toggle_animation(Tick(0));
        p.set_at(Tick(100), ParamValue::Text("B".into()));
        let at = |t| match eval_layer(&e, Tick(t), (10, 10)).unwrap().content {
            LayerContent::Text(x) => x.text,
            _ => unreachable!(),
        };
        assert_eq!(at(50), "A");
        assert_eq!(at(150), "B");
    }
}

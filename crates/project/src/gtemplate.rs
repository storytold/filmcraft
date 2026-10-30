//! Graphics templates (`.fcgt`): FilmCraft's own format for reusable graphics with editable
//! properties, the counterpart of motion graphics templates in other editors.
//!
//! A template file is UTF-8 JSON ([`GraphicsTemplate`]):
//!
//! ```json
//! {
//!   "format": "filmcraft.graphicsTemplate",
//!   "version": 1,
//!   "id": "user:my-lower-third",
//!   "name": "My Lower Third",
//!   "category": "Lower Thirds",
//!   "description": "", "author": "", "license": "", "tags": [],
//!   "canvas": [1920, 1080],
//!   "duration": 1270080000000,
//!   "layers": [ /* graphic_text / graphic_shape effect instances, as in a project file */ ],
//!   "graphic": { /* roll, intro / outro (GraphicMeta) */ },
//!   "controls": [ { "id": "name", "name": "Name", "kind": "text", "layer": 1, "param": "text" } ],
//!   "resources": { "fonts": [ { "family": "…", "style": "…", "license": "OFL-1.1", "data": "<base64>" } ] }
//! }
//! ```
//!
//! - `version` is the format version; readers refuse newer versions.
//! - Layers are referenced by their [`LayerExtra::uid`](crate::graphic_design::LayerExtra); every
//!   layer of a template has one.
//! - `controls` are the properties a user of the template edits (Essential Graphics ▸ Edit):
//!   text, colour, slider (with `min` / `max`), checkbox, font and position, each bound to one
//!   layer parameter (`enabled` = the layer's visibility).
//! - `resources.fonts` optionally embeds font files (base64) that the template needs; installing
//!   the template registers them. Only embed fonts whose licence allows redistribution.
//!
//! FilmCraft never reads Adobe `.mogrt` files (or any other application's template format);
//! [`GraphicsTemplate::from_bytes`] rejects ZIP archives with a message saying so.
//!
//! The built-in templates ([`builtin_templates`]) are original designs made for FilmCraft in code
//! (no image, font or template files), under the project licence.

use std::collections::{HashMap, HashSet};

use filmcraft_geom::Vec2;
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::{Deserialize, Serialize};

use crate::effect::EffectInstance;
use crate::graphic::{self, new_shape_layer, new_text_layer};
use crate::graphic_design::{GraphicMeta, LayerExtra, Pin, PinTarget, Roll, RollMode};
use crate::keyframe::{Param, ParamValue};

pub const TEMPLATE_FORMAT: &str = "filmcraft.graphicsTemplate";
pub const TEMPLATE_VERSION: u32 = 1;
pub const TEMPLATE_EXTENSION: &str = "fcgt";

/// Kind of an editable template property.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ControlKind {
    #[default]
    Text,
    Color,
    Slider,
    Checkbox,
    Font,
    Position,
}

impl ControlKind {
    pub const ALL: [ControlKind; 6] =
        [ControlKind::Text, ControlKind::Color, ControlKind::Slider, ControlKind::Checkbox, ControlKind::Font, ControlKind::Position];
    pub fn name(self) -> &'static str {
        match self {
            ControlKind::Text => "text",
            ControlKind::Color => "color",
            ControlKind::Slider => "slider",
            ControlKind::Checkbox => "checkbox",
            ControlKind::Font => "font",
            ControlKind::Position => "position",
        }
    }
    pub fn from_name(s: &str) -> Option<ControlKind> {
        let s = s.to_ascii_lowercase();
        let s = if s == "colour" { "color".to_string() } else { s };
        ControlKind::ALL.into_iter().find(|k| k.name() == s)
    }
    /// The kind that edits parameter `param` of a layer.
    pub fn for_param(param: &str) -> ControlKind {
        match param {
            "text" | "name" => ControlKind::Text,
            "font" | "font_style" => ControlKind::Font,
            "position" | "anchor" => ControlKind::Position,
            "enabled" => ControlKind::Checkbox,
            p if p.ends_with("_color") => ControlKind::Color,
            p => match crate::find_effect(graphic::TEXT_LAYER)
                .and_then(|d| d.param(p))
                .or_else(|| crate::find_effect(graphic::SHAPE_LAYER).and_then(|d| d.param(p)))
            {
                Some(d) => match &d.default {
                    ParamValue::Bool(_) => ControlKind::Checkbox,
                    ParamValue::Color(_) => ControlKind::Color,
                    ParamValue::Vec2(_) => ControlKind::Position,
                    ParamValue::Text(_) => ControlKind::Text,
                    _ => ControlKind::Slider,
                },
                None => ControlKind::Slider,
            },
        }
    }
}

/// One editable property of a template (Essential Graphics ▸ Edit).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateControl {
    /// Stable id (`[a-z0-9_-]`), unique in the template.
    pub id: String,
    /// Label shown to the user.
    pub name: String,
    pub kind: ControlKind,
    /// The layer (by uid) and parameter id it edits (`enabled` = layer visibility).
    pub layer: u64,
    pub param: String,
    /// Slider range.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub min: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub max: Option<f64>,
}

/// The template a placed graphic came from (kept on the clip, `GraphicMeta::template`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct TemplateLink {
    pub id: String,
    pub name: String,
    pub controls: Vec<TemplateControl>,
}

/// A font embedded in a template.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct FontResource {
    pub family: String,
    pub style: String,
    /// SPDX id of the font's licence (must allow redistribution).
    pub license: String,
    /// Original file name (informational).
    pub file: String,
    /// The font file, base64.
    pub data: String,
}

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Resources {
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub fonts: Vec<FontResource>,
}

impl Resources {
    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }
}

/// A graphics template (`.fcgt`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GraphicsTemplate {
    pub format: String,
    pub version: u32,
    pub id: String,
    pub name: String,
    pub category: String,
    pub description: String,
    pub author: String,
    pub license: String,
    pub tags: Vec<String>,
    /// Frame size the template was designed for.
    pub canvas: [u32; 2],
    /// Default clip duration.
    pub duration: Tick,
    pub layers: Vec<EffectInstance>,
    /// Roll / responsive time settings (`template` is not stored here).
    pub graphic: GraphicMeta,
    pub controls: Vec<TemplateControl>,
    #[serde(skip_serializing_if = "Resources::is_empty")]
    pub resources: Resources,
}

impl Default for GraphicsTemplate {
    fn default() -> Self {
        GraphicsTemplate {
            format: TEMPLATE_FORMAT.into(),
            version: TEMPLATE_VERSION,
            id: String::new(),
            name: String::new(),
            category: "My Templates".into(),
            description: String::new(),
            author: String::new(),
            license: String::new(),
            tags: Vec::new(),
            canvas: [1920, 1080],
            duration: Tick(5 * TICKS_PER_SECOND),
            layers: Vec::new(),
            graphic: GraphicMeta::default(),
            controls: Vec::new(),
            resources: Resources::default(),
        }
    }
}

/// Problems reading or validating a template.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TemplateError {
    #[error(
        "this file is a ZIP archive (an Adobe .mogrt or another application's package?): FilmCraft does not read motion graphics templates from other applications, only its own .fcgt graphics templates"
    )]
    ForeignPackage,
    #[error("not a FilmCraft graphics template: {0}")]
    NotTemplate(String),
    #[error("this graphics template was made by a newer FilmCraft (format version {0}; this build reads up to {TEMPLATE_VERSION})")]
    TooNew(u32),
    #[error("invalid graphics template: {0}")]
    Invalid(String),
}

/// Lower-case slug of a name (`My Lower Third` → `my-lower-third`).
pub fn slug(name: &str) -> String {
    let mut s = String::new();
    for c in name.chars() {
        if c.is_ascii_alphanumeric() {
            s.push(c.to_ascii_lowercase());
        } else if !s.ends_with('-') && !s.is_empty() {
            s.push('-');
        }
    }
    let s = s.trim_end_matches('-').to_string();
    if s.is_empty() { "template".into() } else { s }
}

impl GraphicsTemplate {
    /// Serialize (pretty JSON).
    pub fn to_json(&self) -> String {
        serde_json::to_string_pretty(self).unwrap_or_default()
    }

    /// Read a template file. ZIP archives (such as `.mogrt` packages) are refused without being
    /// inspected.
    pub fn from_bytes(bytes: &[u8]) -> Result<GraphicsTemplate, TemplateError> {
        if bytes.starts_with(b"PK\x03\x04") || bytes.starts_with(b"PK\x05\x06") {
            return Err(TemplateError::ForeignPackage);
        }
        let text =
            std::str::from_utf8(bytes.strip_prefix(b"\xef\xbb\xbf").unwrap_or(bytes)).map_err(|_| TemplateError::NotTemplate("not UTF-8 text".into()))?;
        let v: serde_json::Value = serde_json::from_str(text).map_err(|e| TemplateError::NotTemplate(e.to_string()))?;
        if v.get("format").and_then(|f| f.as_str()) != Some(TEMPLATE_FORMAT) {
            return Err(TemplateError::NotTemplate(format!("`format` is not \"{TEMPLATE_FORMAT}\"")));
        }
        let ver = v.get("version").and_then(|x| x.as_u64()).unwrap_or(0) as u32;
        if ver > TEMPLATE_VERSION {
            return Err(TemplateError::TooNew(ver));
        }
        if ver == 0 {
            return Err(TemplateError::Invalid("missing `version`".into()));
        }
        let t: GraphicsTemplate = serde_json::from_value(v).map_err(|e| TemplateError::Invalid(e.to_string()))?;
        t.validate()?;
        Ok(t)
    }

    /// Check the template's structure: layers are graphic layers with unique uids, controls
    /// point at existing layers and parameters, ids are unique.
    pub fn validate(&self) -> Result<(), TemplateError> {
        let bad = |m: String| Err(TemplateError::Invalid(m));
        if self.name.trim().is_empty() {
            return bad("the template has no name".into());
        }
        if self.layers.is_empty() {
            return bad("the template has no layers".into());
        }
        if self.canvas[0] == 0 || self.canvas[1] == 0 || self.canvas[0] > 16384 || self.canvas[1] > 16384 {
            return bad(format!("bad canvas size {:?}", self.canvas));
        }
        let mut layers = HashMap::with_capacity(self.layers.len());
        for (i, l) in self.layers.iter().enumerate() {
            if !graphic::is_layer(l) {
                return bad(format!("layer {i} is `{}`, not a graphic layer", l.effect));
            }
            let uid = layer_uid(l);
            if uid == 0 || layers.insert(uid, l).is_some() {
                return bad(format!("layer {i} has no unique uid"));
            }
        }
        let mut ids = HashSet::with_capacity(self.controls.len());
        for c in &self.controls {
            if c.id.is_empty() || !ids.insert(c.id.as_str()) {
                return bad(format!("control `{}` has no unique id", c.name));
            }
            let Some(l) = layers.get(&c.layer) else {
                return bad(format!("control `{}` points at a missing layer", c.name));
            };
            if c.param != "enabled" && !l.params.contains_key(&c.param) {
                return bad(format!("control `{}`: the layer has no parameter `{}`", c.name, c.param));
            }
        }
        for f in &self.resources.fonts {
            if base64_decode(&f.data).is_none() {
                return bad(format!("font `{}` is not valid base64", f.family));
            }
        }
        Ok(())
    }

    /// The layers and settings for a graphic clip on a `canvas`-sized frame. When the frame size
    /// differs from the template's, the layers are scaled uniformly (fit) and centred, and pin
    /// distances scale with them. Both scale axes, including keyframes, are fitted even when
    /// Uniform Scale is off. The result links back to the template.
    pub fn instantiate(&self, canvas: (u32, u32)) -> (Vec<EffectInstance>, GraphicMeta) {
        let (w, h) = (canvas.0 as f64, canvas.1 as f64);
        let (tw, th) = (self.canvas[0] as f64, self.canvas[1] as f64);
        let s = (w / tw).min(h / th);
        let off = Vec2::new((w - tw * s) / 2.0, (h - th * s) / 2.0);
        let mut layers = self.layers.clone();
        if (s - 1.0).abs() > 1e-9 || off.x.abs() > 1e-9 || off.y.abs() > 1e-9 {
            for l in &mut layers {
                // Imported layers can omit parameters and rely on definition defaults. Make
                // both scale axes explicit before fitting so those defaults are scaled too.
                if let Some(def) = l.def() {
                    for key in ["scale", "scale_width"] {
                        if let Some(param) = def.param(key) {
                            l.params.entry(key.to_string()).or_insert_with(|| Param::new(param.default.clone()));
                        }
                    }
                }
                if let Some(p) = l.params.get_mut("position") {
                    p.map_values(|v| match v {
                        ParamValue::Vec2(q) if !q.x.is_nan() => ParamValue::Vec2(Vec2::new(q.x * s + off.x, q.y * s + off.y)),
                        o => o,
                    });
                }
                for key in ["scale", "scale_width"] {
                    if let Some(p) = l.params.get_mut(key) {
                        p.map_values(|v| match v {
                            ParamValue::Float(x) => ParamValue::Float(x * s),
                            o => o,
                        });
                    }
                }
                if let Some(pin) = l.layer.as_mut().and_then(|x| x.pin.as_mut()) {
                    pin.offsets.iter_mut().for_each(|o| *o *= s);
                }
            }
        }
        let mut meta = self.graphic.clone();
        meta.template = Some(TemplateLink { id: self.id.clone(), name: self.name.clone(), controls: self.controls.clone() });
        (layers, meta)
    }
}

/// A layer's uid (0 = none).
pub fn layer_uid(e: &EffectInstance) -> u64 {
    e.layer.as_ref().map_or(0, |x| x.uid)
}

/// Give every layer without a uid a fresh one (unique among `layers`).
pub fn ensure_uids(layers: &mut [EffectInstance]) -> bool {
    graphic::ensure_layer_uids(layers)
}

// ------------------------------------------------------------------------------------------------
// base64 (RFC 4648, standard alphabet, padded)
// ------------------------------------------------------------------------------------------------

const B64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

pub fn base64_encode(data: &[u8]) -> String {
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        for k in 0..4 {
            if k <= c.len() {
                out.push(B64[(n >> (18 - 6 * k) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

pub fn base64_decode(s: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(s.len() / 4 * 3);
    let mut acc = 0u32;
    let mut bits = 0;
    for b in s.bytes().filter(|b| !b.is_ascii_whitespace()) {
        if b == b'=' {
            break;
        }
        let v = B64.iter().position(|&x| x == b)? as u32;
        acc = acc << 6 | v;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

// ------------------------------------------------------------------------------------------------
// Built-in templates (original designs)
// ------------------------------------------------------------------------------------------------

fn set(e: &mut EffectInstance, id: &str, v: ParamValue) {
    e.params.insert(id.to_string(), Param::new(v));
}

fn rgb(hex: u32) -> ParamValue {
    let c = |s: u32| ((hex >> s) & 0xff) as f32 / 255.0;
    ParamValue::Color([c(16), c(8), c(0), 1.0])
}

fn text(uid: u64, name: &str, s: &str, pos: (f64, f64), size: f64, style: &str) -> EffectInstance {
    let mut e = new_text_layer(s, Vec2::new(pos.0, pos.1), size);
    set(&mut e, "name", ParamValue::Text(name.into()));
    set(&mut e, "font", ParamValue::Text("Inter".into()));
    set(&mut e, "font_style", ParamValue::Text(style.into()));
    e.layer = Some(Box::new(LayerExtra { uid, ..Default::default() }));
    e
}

fn shape(uid: u64, name: &str, kind: u32, pos: (f64, f64), size: (f64, f64), color: u32) -> EffectInstance {
    let mut e = new_shape_layer(kind, Vec2::new(pos.0, pos.1), Vec2::new(size.0, size.1), vec![]);
    set(&mut e, "name", ParamValue::Text(name.into()));
    set(&mut e, "fill_color", rgb(color));
    e.layer = Some(Box::new(LayerExtra { uid, ..Default::default() }));
    e
}

fn pin(e: &mut EffectInstance, to: u64, edges: [bool; 4], offsets: [f64; 4]) {
    let p =
        Pin { to: if to == 0 { PinTarget::Frame } else { PinTarget::Layer(to) }, left: edges[0], top: edges[1], right: edges[2], bottom: edges[3], offsets };
    e.layer.get_or_insert_with(Default::default).pin = Some(p);
}

fn ctl(id: &str, name: &str, layer: u64, param: &str) -> TemplateControl {
    TemplateControl { id: id.into(), name: name.into(), kind: ControlKind::for_param(param), layer, param: param.into(), min: None, max: None }
}

fn slider(id: &str, name: &str, layer: u64, param: &str, min: f64, max: f64) -> TemplateControl {
    TemplateControl { min: Some(min), max: Some(max), ..ctl(id, name, layer, param) }
}

fn builtin(
    id: &str,
    name: &str,
    category: &str,
    description: &str,
    seconds: i64,
    layers: Vec<EffectInstance>,
    controls: Vec<TemplateControl>,
) -> GraphicsTemplate {
    GraphicsTemplate {
        id: format!("builtin:{id}"),
        name: name.into(),
        category: category.into(),
        description: description.into(),
        author: "FilmCraft contributors".into(),
        license: "MIT OR Apache-2.0".into(),
        tags: vec![category.to_ascii_lowercase()],
        duration: Tick(seconds * TICKS_PER_SECOND),
        layers,
        controls,
        ..Default::default()
    }
}

/// The built-in templates: original FilmCraft designs (lower thirds, titles, end credits,
/// callouts, a ticker), defined here in code. Designed at 1920×1080 with the bundled Inter font.
pub fn builtin_templates() -> Vec<GraphicsTemplate> {
    let mut v = Vec::new();

    // Lower third: a slab that grows with the name, a small accent square and a role line.
    {
        let mut slab = shape(1, "Slab", 0, (400.0, 900.0), (560.0, 150.0), 0x1d4ed8);
        set(&mut slab, "opacity", ParamValue::Float(92.0));
        pin(&mut slab, 2, [true, false, true, false], [-40.0, 0.0, 48.0, 0.0]);
        let mut accent = shape(4, "Accent", 0, (130.0, 900.0), (12.0, 150.0), 0xfacc15);
        pin(&mut accent, 1, [true, false, false, false], [-12.0, 0.0, 0.0, 0.0]);
        let mut name = text(2, "Name", "Alex Morgan", (160.0, 895.0), 62.0, "SemiBold");
        set(&mut name, "fill_color", rgb(0xffffff));
        let mut role = text(3, "Role", "Field Producer", (162.0, 950.0), 36.0, "Regular");
        set(&mut role, "fill_color", rgb(0xdbeafe));
        v.push(builtin(
            "lower-third-slab",
            "Lower Third – Slab",
            "Lower Thirds",
            "Name and role on a coloured slab that widens with the name.",
            6,
            vec![slab, accent, name, role],
            vec![
                ctl("name", "Name", 2, "text"),
                ctl("role", "Role", 3, "text"),
                ctl("slab_color", "Slab Color", 1, "fill_color"),
                ctl("accent_color", "Accent Color", 4, "fill_color"),
                ctl("text_color", "Name Color", 2, "fill_color"),
                ctl("show_role", "Show Role", 3, "enabled"),
                ctl("font", "Font", 2, "font"),
            ],
        ));
    }
    // Lower third: name with a rule underneath that stretches to the name's width.
    {
        let mut name = text(1, "Name", "Sam Rivera", (140.0, 900.0), 58.0, "Bold");
        let shadow = |e: &mut EffectInstance| {
            set(e, "shadow", ParamValue::Bool(true));
            set(e, "shadow_distance", ParamValue::Float(3.0));
            set(e, "shadow_blur", ParamValue::Float(12.0));
            set(e, "shadow_opacity", ParamValue::Float(60.0));
        };
        shadow(&mut name);
        let mut rule = shape(2, "Rule", 0, (300.0, 920.0), (320.0, 5.0), 0x22d3ee);
        pin(&mut rule, 1, [true, false, true, false], [0.0, 0.0, 0.0, 0.0]);
        let mut role = text(3, "Role", "Lead Engineer", (140.0, 966.0), 34.0, "Medium");
        set(&mut role, "tracking", ParamValue::Float(40.0));
        shadow(&mut role);
        v.push(builtin(
            "lower-third-rule",
            "Lower Third – Rule",
            "Lower Thirds",
            "Name over a thin rule that always matches the name's width.",
            6,
            vec![name, rule, role],
            vec![
                ctl("name", "Name", 1, "text"),
                ctl("role", "Role", 3, "text"),
                ctl("rule_color", "Rule Color", 2, "fill_color"),
                ctl("font", "Font", 1, "font"),
                slider("name_size", "Name Size", 1, "size", 24.0, 140.0),
            ],
        ));
    }
    // Centred title with a subtitle and a short rule between them.
    {
        let mut title = text(1, "Title", "THE LONG WAY HOME", (960.0, 520.0), 112.0, "Bold");
        set(&mut title, "align", ParamValue::Choice(1));
        set(&mut title, "tracking", ParamValue::Float(60.0));
        let mut rule = shape(2, "Rule", 0, (960.0, 556.0), (220.0, 4.0), 0xf97316);
        set(&mut rule, "opacity", ParamValue::Float(90.0));
        let mut sub = text(3, "Subtitle", "a story in three parts", (960.0, 628.0), 44.0, "Italic");
        set(&mut sub, "align", ParamValue::Choice(1));
        set(&mut sub, "fill_color", rgb(0xe5e7eb));
        v.push(builtin(
            "title-centered",
            "Title – Centered",
            "Titles",
            "Large centred title, accent rule and subtitle.",
            5,
            vec![title, rule, sub],
            vec![
                ctl("title", "Title", 1, "text"),
                ctl("subtitle", "Subtitle", 3, "text"),
                slider("title_size", "Title Size", 1, "size", 40.0, 240.0),
                ctl("title_color", "Title Color", 1, "fill_color"),
                ctl("accent", "Accent Color", 2, "fill_color"),
                ctl("font", "Font", 1, "font"),
            ],
        ));
    }
    // Title in a box (text background) with a soft shadow.
    {
        let mut title = text(1, "Title", "Chapter One", (960.0, 560.0), 96.0, "SemiBold");
        set(&mut title, "align", ParamValue::Choice(1));
        set(&mut title, "fill_color", rgb(0x111827));
        set(&mut title, "background", ParamValue::Bool(true));
        set(&mut title, "background_color", rgb(0xfef3c7));
        set(&mut title, "background_opacity", ParamValue::Float(100.0));
        set(&mut title, "background_size", ParamValue::Float(36.0));
        set(&mut title, "background_radius", ParamValue::Float(10.0));
        set(&mut title, "shadow", ParamValue::Bool(true));
        set(&mut title, "shadow_distance", ParamValue::Float(8.0));
        set(&mut title, "shadow_blur", ParamValue::Float(30.0));
        set(&mut title, "shadow_opacity", ParamValue::Float(45.0));
        v.push(builtin(
            "title-boxed",
            "Title – Boxed",
            "Titles",
            "A title on a rounded card with a soft shadow.",
            5,
            vec![title],
            vec![
                ctl("title", "Title", 1, "text"),
                ctl("text_color", "Text Color", 1, "fill_color"),
                ctl("box_color", "Box Color", 1, "background_color"),
                slider("box_opacity", "Box Opacity", 1, "background_opacity", 0.0, 100.0),
                ctl("position", "Position", 1, "position"),
            ],
        ));
    }
    // End credits that roll from below the frame to above it.
    {
        let mut heading = text(1, "Heading", "CREDITS", (960.0, 160.0), 72.0, "Bold");
        set(&mut heading, "align", ParamValue::Choice(1));
        set(&mut heading, "tracking", ParamValue::Float(120.0));
        let body = "Directed by\nJordan Lee\n\nWritten by\nPriya Natarajan\n\nEdited by\nMarco Bianchi\n\nMusic by\nHana Kobayashi\n\nMade with FilmCraft";
        let mut credits = text(2, "Credits", body, (960.0, 300.0), 46.0, "Regular");
        set(&mut credits, "align", ParamValue::Choice(1));
        set(&mut credits, "leading", ParamValue::Float(10.0));
        let mut t = builtin(
            "end-credits-roll",
            "End Credits – Roll",
            "End Credits",
            "Centred credits that roll up from below the frame and leave at the top.",
            12,
            vec![heading, credits],
            vec![
                ctl("heading", "Heading", 1, "text"),
                ctl("credits", "Credits", 2, "text"),
                ctl("font", "Font", 2, "font"),
                ctl("color", "Text Color", 2, "fill_color"),
            ],
        );
        t.graphic.roll = Roll { mode: RollMode::Roll, start_off_screen: true, end_off_screen: true, ..Default::default() };
        v.push(t);
    }
    // Callout: a label on a box with a pointer that follows it.
    {
        let mut label = text(1, "Label", "Look here", (1260.0, 380.0), 48.0, "SemiBold");
        set(&mut label, "fill_color", rgb(0xffffff));
        set(&mut label, "background", ParamValue::Bool(true));
        set(&mut label, "background_color", rgb(0xdc2626));
        set(&mut label, "background_opacity", ParamValue::Float(100.0));
        set(&mut label, "background_size", ParamValue::Float(18.0));
        set(&mut label, "background_radius", ParamValue::Float(6.0));
        let mut dot = shape(2, "Pointer", 1, (1200.0, 470.0), (34.0, 34.0), 0xdc2626);
        set(&mut dot, "stroke", ParamValue::Bool(true));
        set(&mut dot, "stroke_color", rgb(0xffffff));
        set(&mut dot, "stroke_width", ParamValue::Float(5.0));
        pin(&mut dot, 1, [true, true, false, false], [-60.0, 55.0, 0.0, 0.0]);
        v.push(builtin(
            "callout-pointer",
            "Callout – Pointer",
            "Callouts",
            "A boxed label with a round pointer; move the label and the pointer follows.",
            4,
            vec![dot, label],
            vec![
                ctl("label", "Label", 1, "text"),
                ctl("color", "Color", 1, "background_color"),
                ctl("pointer", "Pointer Color", 2, "fill_color"),
                ctl("position", "Position", 1, "position"),
            ],
        ));
    }
    // Callout: a pill-shaped tag around its text, with a status dot.
    {
        let mut tag = shape(1, "Tag", 0, (300.0, 140.0), (300.0, 70.0), 0x0f172a);
        set(&mut tag, "corner_radius", ParamValue::Float(35.0));
        set(&mut tag, "opacity", ParamValue::Float(88.0));
        pin(&mut tag, 2, [true; 4], [-70.0, -18.0, 30.0, 18.0]);
        let mut dot = shape(3, "Dot", 1, (190.0, 140.0), (22.0, 22.0), 0x22c55e);
        pin(&mut dot, 2, [true, false, false, false], [-44.0, 0.0, 0.0, 0.0]);
        let mut label = text(2, "Text", "LIVE", (220.0, 155.0), 40.0, "Bold");
        set(&mut label, "tracking", ParamValue::Float(80.0));
        v.push(builtin(
            "callout-tag",
            "Callout – Tag",
            "Callouts",
            "A rounded tag that fits its text, with a status dot.",
            4,
            vec![tag, dot, label],
            vec![
                ctl("text", "Text", 2, "text"),
                ctl("tag_color", "Tag Color", 1, "fill_color"),
                ctl("dot_color", "Dot Color", 3, "fill_color"),
                ctl("show_dot", "Show Dot", 3, "enabled"),
            ],
        ));
    }
    // Ticker: a crawl from right to left.
    {
        let mut line = text(
            1,
            "Ticker",
            "Breaking: the river festival moves to Saturday  •  Roads close at 6 pm  •  Shuttle buses run every 15 minutes",
            (40.0, 1010.0),
            44.0,
            "Medium",
        );
        set(&mut line, "background", ParamValue::Bool(true));
        set(&mut line, "background_color", rgb(0x111827));
        set(&mut line, "background_opacity", ParamValue::Float(85.0));
        set(&mut line, "background_size", ParamValue::Float(14.0));
        let mut t = builtin(
            "ticker-crawl",
            "Ticker – Crawl",
            "Lower Thirds",
            "A news-style line that crawls in from the right and leaves at the left.",
            15,
            vec![line],
            vec![ctl("text", "Text", 1, "text"), ctl("text_color", "Text Color", 1, "fill_color"), ctl("bar_color", "Bar Color", 1, "background_color")],
        );
        t.graphic.roll = Roll { mode: RollMode::CrawlLeft, start_off_screen: true, end_off_screen: true, ..Default::default() };
        v.push(t);
    }
    v
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn base64_round_trip() {
        for n in 0..20 {
            let data: Vec<u8> = (0..n).map(|i| (i * 37 + 11) as u8).collect();
            assert_eq!(base64_decode(&base64_encode(&data)).unwrap(), data);
        }
        assert_eq!(base64_encode(b"Man"), "TWFu");
        assert_eq!(base64_encode(b"Ma"), "TWE=");
        assert!(base64_decode("@@@").is_none());
    }

    #[test]
    fn builtins_are_valid_and_round_trip() {
        let all = builtin_templates();
        assert!(all.len() >= 8);
        let mut ids: Vec<&str> = all.iter().map(|t| t.id.as_str()).collect();
        ids.dedup();
        assert_eq!(ids.len(), all.len());
        for cat in ["Lower Thirds", "Titles", "End Credits", "Callouts"] {
            assert!(all.iter().any(|t| t.category == cat), "{cat}");
        }
        for t in all {
            t.validate().unwrap_or_else(|e| panic!("{}: {e}", t.name));
            let back = GraphicsTemplate::from_bytes(t.to_json().as_bytes()).unwrap();
            assert_eq!(back, t);
        }
    }

    #[test]
    fn refuses_foreign_and_newer_files() {
        assert_eq!(GraphicsTemplate::from_bytes(b"PK\x03\x04rest"), Err(TemplateError::ForeignPackage));
        assert!(matches!(GraphicsTemplate::from_bytes(b"{\"format\":\"x\"}"), Err(TemplateError::NotTemplate(_))));
        let mut t = builtin_templates().remove(0);
        t.version = 99;
        assert_eq!(GraphicsTemplate::from_bytes(t.to_json().as_bytes()), Err(TemplateError::TooNew(99)));
        let mut t = builtin_templates().remove(0);
        t.controls[0].layer = 999;
        assert!(matches!(t.validate(), Err(TemplateError::Invalid(_))));
    }

    #[test]
    fn instantiates_on_other_frame_sizes() {
        let t = builtin_templates().into_iter().find(|t| t.id == "builtin:title-centered").unwrap();
        let (same, meta) = t.instantiate((1920, 1080));
        assert_eq!(same, t.layers);
        assert_eq!(meta.template.as_ref().unwrap().id, t.id);
        let (half, _) = t.instantiate((960, 540));
        assert_eq!(half[0].params["position"].value, ParamValue::Vec2(Vec2::new(480.0, 260.0)));
        assert_eq!(half[0].params["scale"].value, ParamValue::Float(50.0));
        // a vertical frame: fit to width, centred vertically
        let (tall, _) = t.instantiate((1080, 1920));
        let ParamValue::Vec2(p) = tall[0].params["position"].value else { panic!() };
        assert!((p.x - 540.0).abs() < 1e-9 && (p.y - (520.0 * 0.5625 + (1920.0 - 1080.0 * 0.5625) / 2.0)).abs() < 1e-9);
        assert_eq!(slug("My Lower Third!"), "my-lower-third");
        assert_eq!(ControlKind::for_param("background_opacity"), ControlKind::Slider);
        assert_eq!(ControlKind::for_param("shadow"), ControlKind::Checkbox);
    }

    #[test]
    fn instantiates_nonuniform_layers_with_both_axes_fitted() {
        let position = Vec2::new(1920.0, 1080.0);
        let mut t = GraphicsTemplate {
            canvas: [3840, 2160],
            layers: vec![new_text_layer("Lower third", position, 100.0), new_shape_layer(0, position, Vec2::new(400.0, 100.0), Vec::new())],
            ..Default::default()
        };
        for uniform in [false, true] {
            for layer in &mut t.layers {
                set(layer, "uniform_scale", ParamValue::Bool(uniform));
                set(layer, "scale", ParamValue::Float(50.0));
                set(layer, "scale_width", ParamValue::Float(150.0));
            }
            let original = t.clone();
            for (canvas, fit) in [((3840, 2160), 1.0), ((1920, 1080), 0.5), ((1080, 1920), 0.28125), ((7680, 4320), 2.0)] {
                let (layers, _) = t.instantiate(canvas);
                for layer in &layers {
                    let spec = graphic::eval_layer(layer, Tick::ZERO, canvas).unwrap();
                    let width = if uniform { 0.5 } else { 1.5 };
                    assert_eq!(spec.transform.scale, Vec2::new(width * fit, 0.5 * fit));
                    assert_eq!(spec.transform.position, Vec2::new(canvas.0 as f64 / 2.0, canvas.1 as f64 / 2.0));
                    // Keep the independent width fitted even while Uniform Scale hides it.
                    assert_eq!(layer.params["scale_width"].value, ParamValue::Float(150.0 * fit));
                }
            }
            assert_eq!(t, original);
        }
    }

    #[test]
    fn instantiates_imported_layers_with_omitted_scale_defaults() {
        for omitted in [&["scale"][..], &["scale_width"][..], &["scale", "scale_width"][..]] {
            for uniform in [false, true] {
                let position = Vec2::new(1920.0, 1080.0);
                let mut t = GraphicsTemplate {
                    name: "Imported defaults".into(),
                    canvas: [3840, 2160],
                    layers: vec![new_text_layer("Reveal", position, 100.0), new_shape_layer(0, position, Vec2::new(400.0, 100.0), Vec::new())],
                    ..Default::default()
                };
                ensure_uids(&mut t.layers);
                for layer in &mut t.layers {
                    set(layer, "uniform_scale", ParamValue::Bool(uniform));
                    for (key, start, end) in [("scale", 50.0, 100.0), ("scale_width", 150.0, 200.0)] {
                        if omitted.contains(&key) {
                            layer.params.remove(key);
                        } else {
                            let mut param = Param::new(ParamValue::Float(start));
                            param.put_keyframe(Tick::ZERO, ParamValue::Float(start));
                            param.put_keyframe(Tick(TICKS_PER_SECOND), ParamValue::Float(end));
                            layer.params.insert(key.into(), param);
                        }
                    }
                }
                let imported = GraphicsTemplate::from_bytes(t.to_json().as_bytes()).unwrap();
                assert_eq!(imported, t);
                for (canvas, fit) in [((1920, 1080), 0.5), ((1080, 1920), 0.28125), ((7680, 4320), 2.0)] {
                    let (layers, _) = imported.instantiate(canvas);
                    for (placed, original) in layers.iter().zip(&imported.layers) {
                        for key in omitted {
                            assert!(!original.params.contains_key(*key));
                            assert_eq!(placed.params[*key].value, ParamValue::Float(100.0 * fit));
                            assert!(placed.params[*key].keyframes.is_empty());
                        }
                        for time in [Tick::ZERO, Tick(TICKS_PER_SECOND / 2), Tick(TICKS_PER_SECOND)] {
                            let before = graphic::eval_layer(original, time, (3840, 2160)).unwrap();
                            let after = graphic::eval_layer(placed, time, canvas).unwrap();
                            assert_eq!(after.transform.scale, Vec2::new(before.transform.scale.x * fit, before.transform.scale.y * fit));
                        }
                    }
                }
                assert_eq!(imported, t);
            }
        }
    }

    #[test]
    fn instantiates_animated_width_with_timing_and_interpolation_preserved() {
        use crate::keyframe::{Interpolation, Keyframe};

        let mut layer = new_text_layer("Reveal", Vec2::new(1920.0, 1080.0), 100.0);
        set(&mut layer, "uniform_scale", ParamValue::Bool(false));
        let end = Tick(TICKS_PER_SECOND);
        for interp in [Interpolation::Linear, Interpolation::Hold] {
            let mut first = Keyframe::new(Tick::ZERO, ParamValue::Float(0.0));
            first.interp = interp;
            let last = Keyframe::new(end, ParamValue::Float(200.0));
            let width = Param { value: ParamValue::Float(100.0), keyframes: vec![first, last] };
            layer.params.insert("scale_width".into(), width.clone());
            let t = GraphicsTemplate { canvas: [3840, 2160], layers: vec![layer.clone()], ..Default::default() };
            for (canvas, fit) in [((1920, 1080), 0.5), ((1080, 1920), 0.28125), ((7680, 4320), 2.0)] {
                let (layers, _) = t.instantiate(canvas);
                let placed = &layers[0];
                let scaled = &placed.params["scale_width"];
                assert_eq!(scaled.value, ParamValue::Float(100.0 * fit));
                assert_eq!(scaled.keyframes.len(), width.keyframes.len());
                for (key, original) in scaled.keyframes.iter().zip(&width.keyframes) {
                    let mut restored = key.clone();
                    restored.value = original.value.clone();
                    assert_eq!(&restored, original);
                }
                let midpoint = if interp == Interpolation::Hold { 0.0 } else { fit };
                for (time, expected_width) in [(Tick::ZERO, 0.0), (Tick(TICKS_PER_SECOND / 2), midpoint), (end, 2.0 * fit)] {
                    let spec = graphic::eval_layer(placed, time, canvas).unwrap();
                    assert_eq!(spec.transform.scale, Vec2::new(expected_width, fit));
                }
            }
            assert_eq!(t.layers[0].params["scale_width"], width);
        }
    }
}

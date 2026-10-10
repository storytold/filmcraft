//! Effect definitions (parameter schemas, categories) and effect instances on track items.
//!
//! Definitions are data: the Effects panel tree, the Effect Controls rows and the MCP schema are
//! all generated from [`EffectDef`]. Pixel/audio implementations live in the render/audio crates
//! and are looked up by `EffectDef::id`.

use std::collections::BTreeMap;
use std::sync::OnceLock;

use filmcraft_geom::Vec2;
use serde::{Deserialize, Serialize};

use crate::keyframe::{Param, ParamValue};

mod audio;
use crate::mask::Mask;
pub use audio::{GEQ10_LABELS, GEQ20_LABELS, GEQ30_LABELS, PREMIERE_AUDIO_EFFECTS};

mod vfx;
pub use vfx::{EASINGS, ECHO_OPERATORS, FRAME_LAYOUTS, LIGHT_IDS, SIMPLE_BLEND, TRACK_CHOICES, auto_point, ultra_key_setting};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum EffectKind {
    Video,
    Audio,
    VideoTransition,
    AudioTransition,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub enum ParamKind {
    /// Scalar with a slider range (`soft_*` is the slider range; hard min/max clamp typed values).
    Float {
        min: f64,
        max: f64,
        soft_min: f64,
        soft_max: f64,
        unit: &'static str,
        decimals: u8,
    },
    Point,
    Color,
    Bool,
    Choice(&'static [&'static str]),
    Angle,
    Text,
    /// Tone curve (x in, y out); `hue` curves use a rainbow baseline with y = 0.5 as neutral.
    Curve {
        hue: bool,
    },
    /// Colour wheel offset (x, y in −1..1).
    Wheel,
    /// A mask path (edited on the Program monitor).
    Path,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct ParamDef {
    pub id: &'static str,
    pub label: &'static str,
    pub kind: ParamKind,
    pub default: ParamValue,
    /// Can have keyframes (the stopwatch appears).
    pub animatable: bool,
    /// Group (twirl) label within the effect, e.g. "Basic Correction".
    pub group: Option<&'static str>,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct EffectDef {
    pub id: &'static str,
    pub name: &'static str,
    pub kind: EffectKind,
    /// Effects-panel folder path, e.g. `["Video Effects", "Blur & Sharpen"]`.
    pub category: &'static [&'static str],
    pub params: Vec<ParamDef>,
    /// Intrinsic (fixed) effects appear on every clip and cannot be deleted.
    pub intrinsic: bool,
    /// GPU accelerated badge in the Effects panel.
    pub accelerated: bool,
    /// 32-bit colour badge.
    pub float32: bool,
    /// YUV badge.
    pub yuv: bool,
}

impl EffectDef {
    pub fn param(&self, id: &str) -> Option<&ParamDef> {
        self.params.iter().find(|p| p.id == id)
    }
    pub fn instance(&self) -> EffectInstance {
        EffectInstance {
            effect: self.id.to_string(),
            enabled: true,
            params: self.params.iter().map(|p| (p.id.to_string(), Param::new(p.default.clone()))).collect(),
            masks: Vec::new(),
            post_fader: false,
            essential: false,
            layer: None,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectInstance {
    pub effect: String,
    pub enabled: bool,
    pub params: BTreeMap<String, Param>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub masks: Vec<Mask>,
    /// Track/Mix inserts only: process after the fader instead of before it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub post_fader: bool,
    /// Managed by the Essential Sound panel (see [`crate::essential`]).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub essential: bool,
    /// Graphic layers only: stable uid, per-character styles and responsive pin
    /// ([`crate::graphic_design::LayerExtra`]). Schema v12.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layer: Option<Box<crate::graphic_design::LayerExtra>>,
}

impl EffectInstance {
    pub fn def(&self) -> Option<&'static EffectDef> {
        find_effect(&self.effect)
    }
    pub fn param(&self, id: &str) -> Option<&Param> {
        self.params.get(id)
    }
    pub fn param_mut(&mut self, id: &str) -> Option<&mut Param> {
        self.params.get_mut(id)
    }
    pub fn f64_at(&self, id: &str, t: filmcraft_time::Tick) -> f64 {
        self.params.get(id).map(|p| p.f64_at(t)).or_else(|| self.def().and_then(|d| d.param(id)).and_then(|p| p.default.as_f64())).unwrap_or(0.0)
    }
    pub fn vec2_at(&self, id: &str, t: filmcraft_time::Tick) -> Vec2 {
        self.params.get(id).map(|p| p.vec2_at(t)).unwrap_or_default()
    }
    pub fn is_animated(&self) -> bool {
        self.params.values().any(Param::is_animated) || self.masks.iter().any(Mask::is_animated)
    }
}

pub const BLEND_MODES: &[&str] = &[
    "Normal",
    "Dissolve",
    "Darken",
    "Multiply",
    "Color Burn",
    "Linear Burn",
    "Darker Color",
    "Lighten",
    "Screen",
    "Color Dodge",
    "Linear Dodge (Add)",
    "Lighter Color",
    "Overlay",
    "Soft Light",
    "Hard Light",
    "Vivid Light",
    "Linear Light",
    "Pin Light",
    "Hard Mix",
    "Difference",
    "Exclusion",
    "Subtract",
    "Divide",
    "Hue",
    "Saturation",
    "Color",
    "Luminosity",
];

pub(crate) fn f(id: &'static str, label: &'static str, def: f64, min: f64, max: f64, unit: &'static str) -> ParamDef {
    ParamDef {
        id,
        label,
        kind: ParamKind::Float { min, max, soft_min: min, soft_max: max, unit, decimals: 1 },
        default: ParamValue::Float(def),
        animatable: true,
        group: None,
    }
}
pub(crate) fn fs(
    id: &'static str,
    label: &'static str,
    def: f64,
    (min, max): (f64, f64),
    (smin, smax): (f64, f64),
    unit: &'static str,
    decimals: u8,
) -> ParamDef {
    ParamDef {
        id,
        label,
        kind: ParamKind::Float { min, max, soft_min: smin, soft_max: smax, unit, decimals },
        default: ParamValue::Float(def),
        animatable: true,
        group: None,
    }
}
pub(crate) fn pt(id: &'static str, label: &'static str, x: f64, y: f64) -> ParamDef {
    ParamDef { id, label, kind: ParamKind::Point, default: ParamValue::Vec2(Vec2::new(x, y)), animatable: true, group: None }
}
pub(crate) fn col(id: &'static str, label: &'static str, c: [f32; 4]) -> ParamDef {
    ParamDef { id, label, kind: ParamKind::Color, default: ParamValue::Color(c), animatable: true, group: None }
}
pub(crate) fn b(id: &'static str, label: &'static str, v: bool) -> ParamDef {
    ParamDef { id, label, kind: ParamKind::Bool, default: ParamValue::Bool(v), animatable: false, group: None }
}
pub(crate) fn ch(id: &'static str, label: &'static str, opts: &'static [&'static str], def: u32) -> ParamDef {
    ParamDef { id, label, kind: ParamKind::Choice(opts), default: ParamValue::Choice(def), animatable: false, group: None }
}
pub(crate) fn ang(id: &'static str, label: &'static str, def: f64) -> ParamDef {
    ParamDef { id, label, kind: ParamKind::Angle, default: ParamValue::Float(def), animatable: true, group: None }
}
fn curve(id: &'static str, label: &'static str, hue: bool) -> ParamDef {
    let default = if hue { ParamValue::Curve(vec![]) } else { ParamValue::Curve(vec![[0.0, 0.0], [1.0, 1.0]]) };
    ParamDef { id, label, kind: ParamKind::Curve { hue }, default, animatable: false, group: None }
}
fn wheel(id: &'static str, label: &'static str) -> ParamDef {
    ParamDef { id, label, kind: ParamKind::Wheel, default: ParamValue::Vec2(Vec2::new(0.0, 0.0)), animatable: true, group: None }
}
pub(crate) fn txt(id: &'static str, label: &'static str) -> ParamDef {
    ParamDef { id, label, kind: ParamKind::Text, default: ParamValue::Text(String::new()), animatable: false, group: None }
}
fn grp(mut p: ParamDef, g: &'static str) -> ParamDef {
    p.group = Some(g);
    p
}

fn video(id: &'static str, name: &'static str, cat: &'static [&'static str], params: Vec<ParamDef>) -> EffectDef {
    EffectDef { id, name, kind: EffectKind::Video, category: cat, params, intrinsic: false, accelerated: true, float32: true, yuv: false }
}
fn audio(id: &'static str, name: &'static str, cat: &'static [&'static str], params: Vec<ParamDef>) -> EffectDef {
    EffectDef { id, name, kind: EffectKind::Audio, category: cat, params, intrinsic: false, accelerated: false, float32: true, yuv: false }
}

const ADJUST: &[&str] = &["Video Effects", "Adjust"];
const BLUR: &[&str] = &["Video Effects", "Blur & Sharpen"];
const COLOR_CORR: &[&str] = &["Video Effects", "Color Correction"];
const DISTORT: &[&str] = &["Video Effects", "Distort"];
const GENERATE: &[&str] = &["Video Effects", "Generate"];
const IMAGE_CONTROL: &[&str] = &["Video Effects", "Image Control"];
const KEYING: &[&str] = &["Video Effects", "Keying"];
const NOISE: &[&str] = &["Video Effects", "Noise & Grain"];
const PERSPECTIVE: &[&str] = &["Video Effects", "Perspective"];
const STYLIZE: &[&str] = &["Video Effects", "Stylize"];
const TRANSFORM: &[&str] = &["Video Effects", "Transform"];
const VIDEO: &[&str] = &["Video Effects", "Video"];
const A_AMP: &[&str] = &["Audio Effects", "Amplitude and Compression"];
const A_DELAY: &[&str] = &["Audio Effects", "Delay and Echo"];
const A_FILTER: &[&str] = &["Audio Effects", "Filter and EQ"];
const A_NOISE: &[&str] = &["Audio Effects", "Noise Reduction/Restoration"];
const A_REVERB: &[&str] = &["Audio Effects", "Reverb"];
const A_SPECIAL: &[&str] = &["Audio Effects", "Special"];
const A_STEREO: &[&str] = &["Audio Effects", "Stereo Imagery"];
const A_TIME: &[&str] = &["Audio Effects", "Time and Pitch"];
const A_TRANS: &[&str] = &["Audio Transitions", "Crossfade"];

fn build_effects() -> Vec<EffectDef> {
    let mut v = vec![
        // ---- intrinsic ----
        EffectDef {
            id: "motion",
            name: "Motion",
            kind: EffectKind::Video,
            category: &[],
            params: vec![
                pt("position", "Position", f64::NAN, f64::NAN),
                fs("scale", "Scale", 100.0, (0.0, 10000.0), (0.0, 100.0), "", 1),
                fs("scale_width", "Scale Width", 100.0, (0.0, 10000.0), (0.0, 100.0), "", 1),
                b("uniform_scale", "Uniform Scale", true),
                ang("rotation", "Rotation", 0.0),
                pt("anchor", "Anchor Point", f64::NAN, f64::NAN),
                fs("anti_flicker", "Anti-flicker Filter", 0.0, (0.0, 1.0), (0.0, 1.0), "", 2),
            ],
            intrinsic: true,
            accelerated: true,
            float32: true,
            yuv: true,
        },
        EffectDef {
            id: "opacity",
            name: "Opacity",
            kind: EffectKind::Video,
            category: &[],
            params: vec![fs("opacity", "Opacity", 100.0, (0.0, 100.0), (0.0, 100.0), "%", 1), ch("blend", "Blend Mode", BLEND_MODES, 0)],
            intrinsic: true,
            accelerated: true,
            float32: true,
            yuv: true,
        },
        EffectDef {
            id: "time_remap",
            name: "Time Remapping",
            kind: EffectKind::Video,
            category: &[],
            params: vec![fs("speed", "Speed", 100.0, (-10000.0, 10000.0), (0.0, 200.0), "%", 2)],
            intrinsic: true,
            accelerated: true,
            float32: true,
            yuv: true,
        },
        EffectDef {
            id: "volume",
            name: "Volume",
            kind: EffectKind::Audio,
            category: &[],
            params: vec![b("bypass", "Bypass", false), fs("level", "Level", 0.0, (-287.5, 15.0), (-60.0, 15.0), "dB", 1)],
            intrinsic: true,
            accelerated: false,
            float32: true,
            yuv: false,
        },
        EffectDef {
            id: "channel_volume",
            name: "Channel Volume",
            kind: EffectKind::Audio,
            category: &[],
            params: vec![
                b("bypass", "Bypass", false),
                fs("left", "Left", 0.0, (-287.5, 6.0), (-60.0, 6.0), "dB", 1),
                fs("right", "Right", 0.0, (-287.5, 6.0), (-60.0, 6.0), "dB", 1),
            ],
            intrinsic: true,
            accelerated: false,
            float32: true,
            yuv: false,
        },
        EffectDef {
            id: "panner",
            name: "Panner",
            kind: EffectKind::Audio,
            category: &[],
            params: vec![fs("balance", "Balance", 0.0, (-100.0, 100.0), (-100.0, 100.0), "", 1)],
            intrinsic: true,
            accelerated: false,
            float32: true,
            yuv: false,
        },
        // ---- video effects ----
        video(
            "brightness_contrast",
            "Brightness & Contrast",
            COLOR_CORR,
            vec![f("brightness", "Brightness", 0.0, -100.0, 100.0, ""), f("contrast", "Contrast", 0.0, -100.0, 100.0, "")],
        ),
        video(
            "tint",
            "Tint",
            COLOR_CORR,
            vec![
                col("black", "Map Black To", [0.0, 0.0, 0.0, 1.0]),
                col("white", "Map White To", [1.0, 1.0, 1.0, 1.0]),
                f("amount", "Amount to Tint", 100.0, 0.0, 100.0, "%"),
            ],
        ),
        video(
            "color_balance",
            "Color Balance",
            vfx::OBSOLETE,
            vec![
                f("shadow_r", "Shadow Red Balance", 0.0, -100.0, 100.0, ""),
                f("shadow_g", "Shadow Green Balance", 0.0, -100.0, 100.0, ""),
                f("shadow_b", "Shadow Blue Balance", 0.0, -100.0, 100.0, ""),
                f("mid_r", "Midtone Red Balance", 0.0, -100.0, 100.0, ""),
                f("mid_g", "Midtone Green Balance", 0.0, -100.0, 100.0, ""),
                f("mid_b", "Midtone Blue Balance", 0.0, -100.0, 100.0, ""),
                f("hi_r", "Highlight Red Balance", 0.0, -100.0, 100.0, ""),
                f("hi_g", "Highlight Green Balance", 0.0, -100.0, 100.0, ""),
                f("hi_b", "Highlight Blue Balance", 0.0, -100.0, 100.0, ""),
                b("preserve", "Preserve Luminosity", false),
            ],
        ),
        video(
            "leave_color",
            "Leave Color",
            vfx::OBSOLETE,
            vec![
                f("amount", "Amount to Decolor", 0.0, 0.0, 100.0, "%"),
                col("color", "Color To Leave", [1.0, 0.0, 0.0, 1.0]),
                f("tolerance", "Tolerance", 15.0, 0.0, 100.0, "%"),
                f("softness", "Edge Softness", 0.0, 0.0, 100.0, "%"),
            ],
        ),
        video(
            "change_to_color",
            "Change to Color",
            vfx::OBSOLETE,
            vec![
                col("from", "From", [1.0, 0.0, 0.0, 1.0]),
                col("to", "To", [0.0, 0.0, 1.0, 1.0]),
                f("hue_tol", "Hue Tolerance", 5.0, 0.0, 100.0, "%"),
                f("softness", "Softness", 50.0, 0.0, 100.0, "%"),
            ],
        ),
        video(
            "lumetri",
            "Lumetri Color",
            COLOR_CORR,
            vec![
                grp(b("basic_on", "Basic Correction", true), "Basic Correction"),
                grp(txt("input_lut", "Input LUT"), "Basic Correction"),
                grp(f("temperature", "Temperature", 0.0, -100.0, 100.0, ""), "Basic Correction"),
                grp(f("tint", "Tint", 0.0, -100.0, 100.0, ""), "Basic Correction"),
                grp(fs("exposure", "Exposure", 0.0, (-5.0, 5.0), (-5.0, 5.0), "", 1), "Basic Correction"),
                grp(f("contrast", "Contrast", 0.0, -100.0, 100.0, ""), "Basic Correction"),
                grp(f("highlights", "Highlights", 0.0, -100.0, 100.0, ""), "Basic Correction"),
                grp(f("shadows", "Shadows", 0.0, -100.0, 100.0, ""), "Basic Correction"),
                grp(f("whites", "Whites", 0.0, -100.0, 100.0, ""), "Basic Correction"),
                grp(f("blacks", "Blacks", 0.0, -100.0, 100.0, ""), "Basic Correction"),
                grp(fs("saturation", "Saturation", 100.0, (0.0, 200.0), (0.0, 200.0), "", 1), "Basic Correction"),
                // HDR mode (PQ / HLG sequences): the grading range in cd/m² and the speculars above it
                grp(fs("hdr_white", "HDR White", 1000.0, (100.0, 10_000.0), (100.0, 10_000.0), "nits", 0), "Basic Correction"),
                grp(f("hdr_specular", "HDR Specular", 0.0, -100.0, 100.0, ""), "Basic Correction"),
                grp(b("creative_on", "Creative", true), "Creative"),
                grp(
                    ch(
                        "look",
                        "Look",
                        &["None", "Teal & Orange", "Warm Film", "Cool Blue", "Bleach Bypass", "Faded Matte", "Monochrome", "Golden Hour", "Night"],
                        0,
                    ),
                    "Creative",
                ),
                grp(txt("look_lut", "Look LUT"), "Creative"),
                grp(fs("look_intensity", "Intensity", 100.0, (0.0, 200.0), (0.0, 200.0), "", 1), "Creative"),
                grp(f("faded_film", "Faded Film", 0.0, 0.0, 100.0, ""), "Creative"),
                grp(f("sharpen", "Sharpen", 0.0, -100.0, 100.0, ""), "Creative"),
                grp(f("vibrance", "Vibrance", 0.0, -100.0, 100.0, ""), "Creative"),
                grp(fs("creative_sat", "Saturation", 100.0, (0.0, 200.0), (0.0, 200.0), "", 1), "Creative"),
                grp(col("shadow_tint", "Shadow Tint", [0.5, 0.5, 0.5, 1.0]), "Creative"),
                grp(col("highlight_tint", "Highlight Tint", [0.5, 0.5, 0.5, 1.0]), "Creative"),
                grp(b("curves_on", "Curves", true), "Curves"),
                grp(curve("curve_luma", "Luma Curve", false), "Curves"),
                grp(curve("curve_red", "Red Curve", false), "Curves"),
                grp(curve("curve_green", "Green Curve", false), "Curves"),
                grp(curve("curve_blue", "Blue Curve", false), "Curves"),
                grp(curve("hue_vs_sat", "Hue vs Sat", true), "Curves"),
                grp(curve("hue_vs_hue", "Hue vs Hue", true), "Curves"),
                grp(curve("hue_vs_luma", "Hue vs Luma", true), "Curves"),
                grp(curve("luma_vs_sat", "Luma vs Sat", true), "Curves"),
                grp(curve("sat_vs_sat", "Sat vs Sat", true), "Curves"),
                grp(fs("curves_hdr_range", "HDR Range", 1000.0, (100.0, 10_000.0), (100.0, 10_000.0), "nits", 0), "Curves"),
                grp(b("wheels_on", "Color Wheels & Match", true), "Color Wheels & Match"),
                grp(wheel("wheel_shadows", "Shadows"), "Color Wheels & Match"),
                grp(f("wheel_shadows_l", "Shadows Lightness", 0.0, -100.0, 100.0, ""), "Color Wheels & Match"),
                grp(wheel("wheel_midtones", "Midtones"), "Color Wheels & Match"),
                grp(f("wheel_midtones_l", "Midtones Lightness", 0.0, -100.0, 100.0, ""), "Color Wheels & Match"),
                grp(wheel("wheel_highlights", "Highlights"), "Color Wheels & Match"),
                grp(f("wheel_highlights_l", "Highlights Lightness", 0.0, -100.0, 100.0, ""), "Color Wheels & Match"),
                grp(b("hsl_on", "Enable HSL Secondary", false), "HSL Secondary"),
                grp(fs("hsl_hue", "Hue Center", 0.0, (0.0, 360.0), (0.0, 360.0), "°", 0), "HSL Secondary"),
                grp(fs("hsl_hue_range", "Hue Range", 30.0, (1.0, 180.0), (1.0, 180.0), "°", 0), "HSL Secondary"),
                grp(fs("hsl_sat_min", "Saturation Min", 10.0, (0.0, 100.0), (0.0, 100.0), "", 0), "HSL Secondary"),
                grp(fs("hsl_luma_min", "Luma Min", 5.0, (0.0, 100.0), (0.0, 100.0), "", 0), "HSL Secondary"),
                grp(fs("hsl_luma_max", "Luma Max", 95.0, (0.0, 100.0), (0.0, 100.0), "", 0), "HSL Secondary"),
                grp(f("hsl_soft", "Soften", 20.0, 0.0, 100.0, ""), "HSL Secondary"),
                grp(ch("hsl_show_mask", "Show Mask", &["Off", "Color/Gray", "Color/Black", "White/Black"], 0), "HSL Secondary"),
                grp(f("hsl_denoise", "Denoise", 0.0, 0.0, 100.0, ""), "HSL Secondary"),
                grp(f("hsl_blur", "Blur", 0.0, 0.0, 100.0, ""), "HSL Secondary"),
                grp(f("hsl_temp", "Temperature", 0.0, -100.0, 100.0, ""), "HSL Secondary"),
                grp(f("hsl_tint", "Tint", 0.0, -100.0, 100.0, ""), "HSL Secondary"),
                grp(fs("hsl_sat", "Saturation", 100.0, (0.0, 200.0), (0.0, 200.0), "", 1), "HSL Secondary"),
                grp(f("hsl_hue_shift", "Hue Shift", 0.0, -180.0, 180.0, "°"), "HSL Secondary"),
                grp(b("vignette_on", "Vignette", true), "Vignette"),
                grp(f("vignette_amount", "Amount", 0.0, -5.0, 5.0, ""), "Vignette"),
                grp(f("vignette_midpoint", "Midpoint", 50.0, 0.0, 100.0, ""), "Vignette"),
                grp(f("vignette_roundness", "Roundness", 0.0, -100.0, 100.0, ""), "Vignette"),
                grp(f("vignette_feather", "Feather", 50.0, 0.0, 100.0, ""), "Vignette"),
            ],
        ),
        video(
            "gaussian_blur",
            "Gaussian Blur",
            BLUR,
            vec![
                fs("blurriness", "Blurriness", 0.0, (0.0, 3000.0), (0.0, 100.0), "", 1),
                ch("dimensions", "Blur Dimensions", &["Horizontal and Vertical", "Horizontal", "Vertical"], 0),
                b("repeat_edge", "Repeat Edge Pixels", false),
            ],
        ),
        video(
            "directional_blur",
            "Directional Blur",
            BLUR,
            vec![ang("direction", "Direction", 0.0), fs("length", "Blur Length", 0.0, (0.0, 1000.0), (0.0, 20.0), "", 1)],
        ),
        video("sharpen", "Sharpen", BLUR, vec![fs("amount", "Sharpen Amount", 0.0, (0.0, 4000.0), (0.0, 100.0), "", 0)]),
        video(
            "unsharp_mask",
            "Unsharp Mask",
            BLUR,
            vec![
                fs("amount", "Amount", 50.0, (0.0, 500.0), (0.0, 500.0), "", 0),
                fs("radius", "Radius", 1.0, (0.1, 250.0), (0.1, 250.0), "", 1),
                fs("threshold", "Threshold", 0.0, (0.0, 255.0), (0.0, 255.0), "", 0),
            ],
        ),
        video("camera_blur", "Camera Blur", vfx::LEGACY, vec![f("percent", "Percent Blur", 0.0, 0.0, 100.0, "")]),
        video("black_white", "Black & White", IMAGE_CONTROL, vec![]),
        video(
            "color_pass",
            "Color Pass",
            IMAGE_CONTROL,
            vec![col("color", "Color", [1.0, 0.0, 0.0, 1.0]), f("similarity", "Similarity", 10.0, 0.0, 100.0, ""), b("reverse", "Reverse", false)],
        ),
        video("gamma_correction", "Gamma Correction", IMAGE_CONTROL, vec![fs("gamma", "Gamma", 10.0, (1.0, 28.0), (1.0, 28.0), "", 0)]),
        video(
            "invert",
            "Invert",
            IMAGE_CONTROL,
            vec![ch("channel", "Channel", &["RGB", "Red", "Green", "Blue", "Alpha"], 0), f("blend", "Blend With Original", 0.0, 0.0, 100.0, "%")],
        ),
        video(
            "levels",
            "Levels",
            ADJUST,
            vec![
                fs("in_black", "(RGB) Input Black Level", 0.0, (0.0, 255.0), (0.0, 255.0), "", 0),
                fs("in_white", "(RGB) Input White Level", 255.0, (0.0, 255.0), (0.0, 255.0), "", 0),
                fs("out_black", "(RGB) Output Black Level", 0.0, (0.0, 255.0), (0.0, 255.0), "", 0),
                fs("out_white", "(RGB) Output White Level", 255.0, (0.0, 255.0), (0.0, 255.0), "", 0),
                fs("gamma", "(RGB) Gamma", 100.0, (10.0, 1000.0), (10.0, 300.0), "", 0),
            ],
        ),
        video(
            "proc_amp",
            "ProcAmp",
            ADJUST,
            vec![
                f("brightness", "Brightness", 0.0, -100.0, 100.0, ""),
                fs("contrast", "Contrast", 100.0, (0.0, 200.0), (0.0, 200.0), "", 1),
                ang("hue", "Hue", 0.0),
                fs("saturation", "Saturation", 100.0, (0.0, 200.0), (0.0, 200.0), "", 1),
            ],
        ),
        video(
            "extract",
            "Extract",
            ADJUST,
            vec![
                fs("black", "Black Input Level", 0.0, (0.0, 255.0), (0.0, 255.0), "", 0),
                fs("white", "White Input Level", 255.0, (0.0, 255.0), (0.0, 255.0), "", 0),
                f("softness", "Softness", 0.0, 0.0, 100.0, ""),
                b("invert", "Invert", false),
            ],
        ),
        video("posterize", "Posterize", STYLIZE, vec![fs("levels", "Level", 7.0, (2.0, 255.0), (2.0, 32.0), "", 0)]),
        video(
            "mosaic",
            "Mosaic",
            STYLIZE,
            vec![
                fs("horizontal", "Horizontal Blocks", 10.0, (1.0, 4000.0), (1.0, 200.0), "", 0),
                fs("vertical", "Vertical Blocks", 10.0, (1.0, 4000.0), (1.0, 200.0), "", 0),
                b("sharp", "Sharp Colors", false),
            ],
        ),
        video("find_edges", "Find Edges", STYLIZE, vec![b("invert", "Invert", false), f("blend", "Blend With Original", 0.0, 0.0, 100.0, "%")]),
        video(
            "emboss",
            "Emboss",
            vfx::OBSOLETE,
            vec![
                ang("direction", "Direction", 45.0),
                fs("relief", "Relief", 1.8, (0.0, 10.0), (0.0, 10.0), "", 1),
                f("contrast", "Contrast", 100.0, 0.0, 500.0, ""),
                f("blend", "Blend With Original", 0.0, 0.0, 100.0, "%"),
            ],
        ),
        video("replicate", "Replicate", vfx::LEGACY, vec![fs("count", "Count", 2.0, (2.0, 16.0), (2.0, 16.0), "", 0)]),
        video(
            "strobe",
            "Strobe Light",
            STYLIZE,
            vec![
                col("color", "Strobe Color", [1.0, 1.0, 1.0, 1.0]),
                f("blend", "Blend With Original", 0.0, 0.0, 100.0, "%"),
                fs("duration", "Strobe Duration (secs)", 0.05, (0.0, 30.0), (0.0, 1.0), "", 2),
                fs("period", "Strobe Period (secs)", 0.5, (0.0, 30.0), (0.0, 2.0), "", 2),
            ],
        ),
        video(
            "noise",
            "Noise",
            NOISE,
            vec![f("amount", "Amount of Noise", 0.0, 0.0, 100.0, "%"), b("color", "Use Color Noise", true), b("clip", "Clipping", true)],
        ),
        video("median", "Median", vfx::OBSOLETE, vec![fs("radius", "Radius", 0.0, (0.0, 100.0), (0.0, 20.0), "", 0)]),
        video(
            "crop",
            "Crop",
            vfx::LEGACY,
            vec![
                f("left", "Left", 0.0, 0.0, 100.0, "%"),
                f("top", "Top", 0.0, 0.0, 100.0, "%"),
                f("right", "Right", 0.0, 0.0, 100.0, "%"),
                f("bottom", "Bottom", 0.0, 0.0, 100.0, "%"),
                b("zoom", "Zoom", false),
                fs("feather", "Edge Feather", 0.0, (0.0, 1000.0), (0.0, 100.0), "", 0),
            ],
        ),
        video("horizontal_flip", "Horizontal Flip", TRANSFORM, vec![]),
        video("vertical_flip", "Vertical Flip", TRANSFORM, vec![]),
        video("edge_feather", "Edge Feather", vfx::LEGACY, vec![fs("amount", "Amount", 0.0, (0.0, 100.0), (0.0, 100.0), "", 0)]),
        video(
            "transform",
            "Transform",
            TRANSFORM,
            vec![
                pt("anchor", "Anchor Point", f64::NAN, f64::NAN),
                pt("position", "Position", f64::NAN, f64::NAN),
                b("uniform_scale", "Uniform Scale", true),
                fs("scale_height", "Scale Height", 100.0, (0.0, 10000.0), (0.0, 600.0), "", 1),
                fs("scale_width", "Scale Width", 100.0, (0.0, 10000.0), (0.0, 600.0), "", 1),
                f("skew", "Skew", 0.0, -70.0, 70.0, ""),
                ang("skew_axis", "Skew Axis", 0.0),
                ang("rotation", "Rotation", 0.0),
                f("opacity", "Opacity", 100.0, 0.0, 100.0, ""),
                b("shutter_override", "Use Effect Shutter Angle", false),
                f("shutter_angle", "Shutter Angle", 180.0, 0.0, 360.0, ""),
            ],
        ),
        video("mirror", "Mirror", DISTORT, vec![pt("center", "Reflection Center", f64::NAN, f64::NAN), ang("angle", "Reflection Angle", 0.0)]),
        video("offset", "Offset", TRANSFORM, vec![pt("shift", "Shift Center To", f64::NAN, f64::NAN), f("blend", "Blend With Original", 0.0, 0.0, 100.0, "%")]),
        video(
            "lens_distortion",
            "Lens Distortion",
            DISTORT,
            vec![
                f("curvature", "Curvature", 0.0, -100.0, 100.0, ""),
                f("v_decentering", "Vertical Decentering", 0.0, -100.0, 100.0, ""),
                f("h_decentering", "Horizontal Decentering", 0.0, -100.0, 100.0, ""),
            ],
        ),
        video(
            "twirl",
            "Twirl",
            DISTORT,
            vec![
                ang("angle", "Angle", 50.0),
                fs("radius", "Twirl Radius", 30.0, (0.0, 100.0), (0.0, 100.0), "", 1),
                pt("center", "Twirl Center", f64::NAN, f64::NAN),
            ],
        ),
        video(
            "wave_warp",
            "Wave Warp",
            DISTORT,
            vec![
                fs("height", "Wave Height", 10.0, (-4000.0, 4000.0), (-100.0, 100.0), "", 0),
                fs("width", "Wave Width", 40.0, (1.0, 4000.0), (1.0, 400.0), "", 0),
                ang("direction", "Direction", 90.0),
                fs("speed", "Wave Speed", 1.0, (-100.0, 100.0), (-10.0, 10.0), "", 1),
            ],
        ),
        video(
            "basic_3d",
            "Basic 3D",
            PERSPECTIVE,
            vec![ang("swivel", "Swivel", 0.0), ang("tilt", "Tilt", 0.0), fs("distance", "Distance to Image", 0.0, (-10000.0, 10000.0), (-100.0, 100.0), "", 1)],
        ),
        video(
            "drop_shadow",
            "Drop Shadow",
            PERSPECTIVE,
            vec![
                col("color", "Shadow Color", [0.0, 0.0, 0.0, 1.0]),
                f("opacity", "Opacity", 50.0, 0.0, 100.0, "%"),
                ang("direction", "Direction", 135.0),
                fs("distance", "Distance", 5.0, (0.0, 4000.0), (0.0, 120.0), "", 1),
                fs("softness", "Softness", 0.0, (0.0, 1000.0), (0.0, 250.0), "", 1),
                b("only", "Shadow Only", false),
            ],
        ),
        video(
            "bevel_alpha",
            "Bevel Alpha",
            vfx::OBSOLETE,
            vec![
                fs("thickness", "Edge Thickness", 2.0, (0.0, 200.0), (0.0, 10.0), "", 1),
                ang("angle", "Light Angle", -60.0),
                col("color", "Light Color", [1.0, 1.0, 1.0, 1.0]),
                f("intensity", "Light Intensity", 40.0, 0.0, 100.0, ""),
            ],
        ),
        video(
            "ultra_key",
            "Ultra Key",
            KEYING,
            vec![
                col("key_color", "Key Color", [0.0, 0.8, 0.2, 1.0]),
                ch("output", "Output", &["Composite", "Alpha Channel", "Color Channel"], 0),
                ch("setting", "Setting", &["Default", "Relaxed", "Aggressive", "Custom"], 0),
                grp(f("transparency", "Transparency", 45.0, 0.0, 100.0, ""), "Matte Generation"),
                grp(f("highlight", "Highlight", 10.0, 0.0, 100.0, ""), "Matte Generation"),
                grp(f("shadow", "Shadow", 50.0, 0.0, 100.0, ""), "Matte Generation"),
                grp(f("tolerance", "Tolerance", 50.0, 0.0, 100.0, ""), "Matte Generation"),
                grp(f("pedestal", "Pedestal", 10.0, 0.0, 100.0, ""), "Matte Generation"),
                grp(f("choke", "Choke", 0.0, 0.0, 100.0, ""), "Matte Cleanup"),
                grp(f("soften", "Soften", 0.0, 0.0, 100.0, ""), "Matte Cleanup"),
                grp(f("spill", "Spill", 50.0, 0.0, 100.0, ""), "Spill Suppression"),
            ],
        ),
        video(
            "track_matte",
            "Track Matte Key",
            KEYING,
            vec![
                ch("matte", "Matte", &["None", "Video 1", "Video 2", "Video 3", "Video 4"], 0),
                ch("composite", "Composite Using", &["Matte Alpha", "Matte Luma"], 0),
                b("reverse", "Reverse", false),
            ],
        ),
        video(
            "color_key",
            "Color Key",
            KEYING,
            vec![
                col("color", "Key Color", [0.0, 0.0, 1.0, 1.0]),
                fs("tolerance", "Color Tolerance", 0.0, (0.0, 255.0), (0.0, 255.0), "", 0),
                fs("thin", "Edge Thin", 0.0, (-5.0, 5.0), (-5.0, 5.0), "", 0),
                fs("feather", "Edge Feather", 0.0, (0.0, 50.0), (0.0, 50.0), "", 1),
            ],
        ),
        video("luma_key", "Luma Key", KEYING, vec![f("threshold", "Threshold", 0.0, 0.0, 100.0, "%"), f("cutoff", "Cutoff", 0.0, 0.0, 100.0, "%")]),
        video(
            "four_color_gradient",
            "4-Color Gradient",
            GENERATE,
            vec![
                col("c1", "Color 1", [1.0, 1.0, 0.0, 1.0]),
                col("c2", "Color 2", [0.0, 1.0, 0.0, 1.0]),
                col("c3", "Color 3", [1.0, 0.0, 1.0, 1.0]),
                col("c4", "Color 4", [0.0, 0.0, 1.0, 1.0]),
                f("blend", "Blend", 100.0, 1.0, 1000.0, ""),
                f("opacity", "Opacity", 100.0, 0.0, 100.0, "%"),
            ],
        ),
        video(
            "ramp",
            "Ramp",
            vfx::LEGACY,
            vec![
                pt("start", "Start of Ramp", f64::NAN, 0.0),
                col("start_color", "Start Color", [0.0, 0.0, 0.0, 1.0]),
                pt("end", "End of Ramp", f64::NAN, f64::NAN),
                col("end_color", "End Color", [1.0, 1.0, 1.0, 1.0]),
                ch("shape", "Ramp Shape", &["Linear Ramp", "Radial Ramp"], 0),
                f("blend", "Blend With Original", 0.0, 0.0, 100.0, "%"),
            ],
        ),
        video(
            "circle",
            "Circle",
            vfx::OBSOLETE,
            vec![
                pt("center", "Center", f64::NAN, f64::NAN),
                fs("radius", "Radius", 75.0, (0.0, 4000.0), (0.0, 400.0), "", 1),
                col("color", "Color", [1.0, 1.0, 1.0, 1.0]),
                f("opacity", "Opacity", 100.0, 0.0, 100.0, "%"),
            ],
        ),
        video(
            "grid",
            "Grid",
            vfx::OBSOLETE,
            vec![
                fs("size", "Width", 60.0, (1.0, 4000.0), (1.0, 400.0), "", 0),
                fs("border", "Border", 2.0, (0.0, 100.0), (0.0, 20.0), "", 1),
                col("color", "Color", [1.0, 1.0, 1.0, 1.0]),
                f("opacity", "Opacity", 100.0, 0.0, 100.0, "%"),
            ],
        ),
        video(
            "lens_flare",
            "Lens Flare",
            vfx::LIGHTS,
            vec![
                pt("center", "Flare Center", f64::NAN, f64::NAN),
                f("brightness", "Flare Brightness", 100.0, 0.0, 300.0, "%"),
                f("blend", "Blend With Original", 0.0, 0.0, 100.0, "%"),
            ],
        ),
        video(
            "timecode",
            "Timecode",
            vfx::OBSOLETE,
            vec![
                pt("position", "Position", f64::NAN, f64::NAN),
                fs("size", "Size", 15.0, (1.0, 100.0), (1.0, 50.0), "%", 1),
                f("opacity", "Opacity", 100.0, 0.0, 100.0, "%"),
            ],
        ),
        video(
            "clip_name",
            "Clip Name",
            vfx::OBSOLETE,
            vec![pt("position", "Position", f64::NAN, f64::NAN), fs("size", "Size", 15.0, (1.0, 100.0), (1.0, 50.0), "%", 1)],
        ),
        video("simple_text", "Simple Text", vfx::UTILITY, vec![]),
        // ---- audio effects ----
        // (defined in effect/audio.rs; appended below)
        EffectDef {
            id: "constant_power",
            name: "Constant Power",
            kind: EffectKind::AudioTransition,
            category: A_TRANS,
            params: vec![],
            intrinsic: false,
            accelerated: false,
            float32: true,
            yuv: false,
        },
        EffectDef {
            id: "constant_gain",
            name: "Constant Gain",
            kind: EffectKind::AudioTransition,
            category: A_TRANS,
            params: vec![],
            intrinsic: false,
            accelerated: false,
            float32: true,
            yuv: false,
        },
        EffectDef {
            id: "exponential_fade",
            name: "Exponential Fade",
            kind: EffectKind::AudioTransition,
            category: A_TRANS,
            params: vec![],
            intrinsic: false,
            accelerated: false,
            float32: true,
            yuv: false,
        },
    ];
    v.extend(audio::defs());
    v.extend(vfx::defs());
    vfx::extend_core(&mut v);
    v.extend(crate::vtransition::video_transition_defs());
    v.extend(crate::graphic::layer_defs());
    // YUV badge for the colour/intrinsic set
    for e in &mut v {
        if matches!(e.id, "brightness_contrast" | "proc_amp" | "crop" | "gaussian_blur" | "tint" | "lumetri") {
            e.yuv = true;
        }
    }
    vfx::apply_badges(&mut v);
    v
}

pub fn effect_defs() -> &'static [EffectDef] {
    static DEFS: OnceLock<Vec<EffectDef>> = OnceLock::new();
    DEFS.get_or_init(build_effects)
}

pub fn find_effect(id: &str) -> Option<&'static EffectDef> {
    effect_defs().iter().find(|e| e.id == id)
}

/// Case-insensitive lookup by id or display name.
pub fn find_effect_by_name(name: &str) -> Option<&'static EffectDef> {
    let n = name.to_ascii_lowercase();
    effect_defs().iter().find(|e| e.id == n || e.name.to_ascii_lowercase() == n)
}

/// Intrinsic video effects every video track item carries (Motion, Opacity, Time Remapping).
pub fn intrinsic_video() -> Vec<EffectInstance> {
    ["motion", "opacity", "time_remap"].iter().filter_map(|id| find_effect(id)).map(EffectDef::instance).collect()
}

pub fn intrinsic_audio() -> Vec<EffectInstance> {
    ["volume", "channel_volume", "panner"].iter().filter_map(|id| find_effect(id)).map(EffectDef::instance).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unique_ids_and_defaults() {
        let mut ids = std::collections::HashSet::new();
        for e in effect_defs() {
            assert!(ids.insert(e.id), "duplicate {}", e.id);
            let inst = e.instance();
            assert_eq!(inst.params.len(), e.params.len());
        }
        assert!(effect_defs().len() > 90);
        assert_eq!(find_effect_by_name("Gaussian Blur").unwrap().id, "gaussian_blur");
        assert_eq!(intrinsic_video().len(), 3);
    }
}

//! Generates animated UNIGINE subtitle graphics templates (.fcgt).
//!
//! Creates subtitle banner templates sliding in from four directions (Left, Right, Up, Down)
//! with top-left anchored auto-sizing around Title + Description and manual Box Width / Height sliders.

use std::fs;
use std::path::PathBuf;

use filmcraft_geom::Vec2;
use filmcraft_project::effect::EffectInstance;
use filmcraft_project::graphic::{new_shape_layer, new_text_layer};
use filmcraft_project::graphic_design::{GraphicMeta, LayerExtra, Pin, PinTarget};
use filmcraft_project::gtemplate::{ControlKind, GraphicsTemplate, TEMPLATE_FORMAT, TEMPLATE_VERSION, TemplateControl};
use filmcraft_project::keyframe::{Interpolation, Keyframe, Param, ParamValue};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

const TOTAL_SECONDS: f64 = 6.0;
const DURATION_TICKS: i64 = (TOTAL_SECONDS * TICKS_PER_SECOND as f64) as i64;
const INTRO_SECS: f64 = 1.5;
const OUTRO_SECS: f64 = 1.5;

const PLATE_WIDTH: f64 = 920.0;
const PLATE_HEIGHT: f64 = 160.0;
const ACCENT_WIDTH: f64 = 8.0;
const ACCENT_HEIGHT: f64 = 160.0;

const DEFAULT_TITLE: &str = "Kannala-Brandt Fisheye Model";
const DEFAULT_DESCRIPTION: &str =
    "Calibrated fisheye rendering reproduces real camera lens geometry\nusing intrinsic parameters from calibration data";

/// Converts seconds to timeline ticks.
fn ticks(s: f64) -> Tick {
    Tick((s * TICKS_PER_SECOND as f64).round() as i64)
}

/// Sets a static parameter value on an effect instance.
fn set_param(e: &mut EffectInstance, id: &str, v: ParamValue) {
    e.params.insert(id.to_string(), Param::new(v));
}

/// Sets an animated parameter value with keyframes on an effect instance.
fn set_animated(e: &mut EffectInstance, id: &str, static_val: ParamValue, keyframes: Vec<Keyframe>) {
    e.params.insert(id.to_string(), Param { value: static_val, keyframes });
}

/// Constructs a basic template control binding.
fn ctl(id: &str, name: &str, layer: u64, param: &str) -> TemplateControl {
    TemplateControl { id: id.into(), name: name.into(), kind: ControlKind::for_param(param), layer, param: param.into(), min: None, max: None }
}

/// Constructs a slider template control binding with explicit bounds.
fn slider(id: &str, name: &str, layer: u64, param: &str, min: f64, max: f64) -> TemplateControl {
    TemplateControl { min: Some(min), max: Some(max), ..ctl(id, name, layer, param) }
}

/// Slide direction for the subtitle banner animations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    /// Slides in from the left edge.
    Left,
    /// Slides in from the right edge.
    Right,
    /// Slides up from the bottom edge.
    Up,
    /// Slides down from the top edge.
    Down,
}

impl Direction {
    fn id(self) -> &'static str {
        match self {
            Direction::Left => "unigine-subtitle-slide-left",
            Direction::Right => "unigine-subtitle-slide-right",
            Direction::Up => "unigine-subtitle-slide-up",
            Direction::Down => "unigine-subtitle-slide-down",
        }
    }

    fn name(self) -> &'static str {
        match self {
            Direction::Left => "Unigine Subtitle – Slide Left",
            Direction::Right => "Unigine Subtitle – Slide Right",
            Direction::Up => "Unigine Subtitle – Slide Up",
            Direction::Down => "Unigine Subtitle – Slide Down",
        }
    }

    fn description(self) -> &'static str {
        match self {
            Direction::Left => "Animated subtitle banner: background slides in first from left (0.8s), text reveals after (0.7s).",
            Direction::Right => "Animated subtitle banner: background slides in first from right (0.8s), text reveals after (0.7s).",
            Direction::Up => "Animated subtitle banner: background slides up first from bottom (0.8s), text reveals after (0.7s).",
            Direction::Down => "Animated subtitle banner: background slides down first from top (0.8s), text reveals after (0.7s).",
        }
    }
}

struct SubtitleGeometry {
    plate_rest: Vec2,
    plate_offscreen: Vec2,
    accent_rest: Vec2,
    accent_offscreen: Vec2,
    title_rest: Vec2,
    title_initial: Vec2,
    desc_rest: Vec2,
    desc_initial: Vec2,
}

fn compute_geometry(dir: Direction) -> SubtitleGeometry {
    match dir {
        Direction::Left => SubtitleGeometry {
            plate_rest: Vec2::new(560.0, 920.0),
            plate_offscreen: Vec2::new(-540.0, 920.0),
            accent_rest: Vec2::new(104.0, 920.0),
            accent_offscreen: Vec2::new(-996.0, 920.0),
            title_rest: Vec2::new(132.0, 885.0),
            title_initial: Vec2::new(82.0, 885.0),
            desc_rest: Vec2::new(132.0, 930.0),
            desc_initial: Vec2::new(82.0, 930.0),
        },
        Direction::Right => SubtitleGeometry {
            plate_rest: Vec2::new(1360.0, 920.0),
            plate_offscreen: Vec2::new(2460.0, 920.0),
            accent_rest: Vec2::new(1816.0, 920.0),
            accent_offscreen: Vec2::new(2916.0, 920.0),
            title_rest: Vec2::new(920.0, 885.0),
            title_initial: Vec2::new(970.0, 885.0),
            desc_rest: Vec2::new(920.0, 930.0),
            desc_initial: Vec2::new(970.0, 930.0),
        },
        Direction::Up => SubtitleGeometry {
            plate_rest: Vec2::new(560.0, 920.0),
            plate_offscreen: Vec2::new(560.0, 1200.0),
            accent_rest: Vec2::new(104.0, 920.0),
            accent_offscreen: Vec2::new(104.0, 1200.0),
            title_rest: Vec2::new(132.0, 885.0),
            title_initial: Vec2::new(132.0, 915.0),
            desc_rest: Vec2::new(132.0, 930.0),
            desc_initial: Vec2::new(132.0, 960.0),
        },
        Direction::Down => SubtitleGeometry {
            plate_rest: Vec2::new(560.0, 160.0),
            plate_offscreen: Vec2::new(560.0, -120.0),
            accent_rest: Vec2::new(104.0, 160.0),
            accent_offscreen: Vec2::new(104.0, -120.0),
            title_rest: Vec2::new(132.0, 125.0),
            title_initial: Vec2::new(132.0, 95.0),
            desc_rest: Vec2::new(132.0, 170.0),
            desc_initial: Vec2::new(132.0, 140.0),
        },
    }
}

fn make_plate_layer(uid: u64, geom: &SubtitleGeometry) -> EffectInstance {
    let mut e = new_shape_layer(0, geom.plate_rest, Vec2::new(PLATE_WIDTH, PLATE_HEIGHT), vec![]);
    set_param(&mut e, "name", ParamValue::Text("Plate".into()));
    set_param(&mut e, "corner_radius", ParamValue::Float(6.0));
    set_param(&mut e, "fill_color", ParamValue::Color([0.043137256, 0.07058824, 0.11764706, 1.0]));
    set_param(&mut e, "size_w", ParamValue::Float(PLATE_WIDTH));
    set_param(&mut e, "size_h", ParamValue::Float(PLATE_HEIGHT));

    let kf_pos = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Vec2(geom.plate_offscreen), interp: Interpolation::EaseOut, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(0.8), value: ParamValue::Vec2(geom.plate_rest), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(5.2), value: ParamValue::Vec2(geom.plate_rest), interp: Interpolation::EaseIn, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(6.0), value: ParamValue::Vec2(geom.plate_offscreen), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "position", ParamValue::Vec2(geom.plate_rest), kf_pos);

    let kf_op = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Float(85.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(6.0), value: ParamValue::Float(85.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "opacity", ParamValue::Float(85.0), kf_op);

    e.layer = Some(Box::new(LayerExtra {
        uid,
        pin: Some(Pin {
            to: PinTarget::Layers(vec![3, 4]),
            left: true,
            top: true,
            right: true,
            bottom: true,
            offsets: [-32.0, -14.0, 32.0, 24.0],
            size_only: true,
        }),
        ..Default::default()
    }));
    e
}

fn make_accent_layer(uid: u64, dir: Direction, geom: &SubtitleGeometry) -> EffectInstance {
    let mut e = new_shape_layer(0, geom.accent_rest, Vec2::new(ACCENT_WIDTH, ACCENT_HEIGHT), vec![]);
    set_param(&mut e, "name", ParamValue::Text("Accent Bar".into()));
    set_param(&mut e, "corner_radius", ParamValue::Float(0.0));
    set_param(&mut e, "fill_color", ParamValue::Color([0.5176471, 0.8, 0.08627451, 1.0]));

    let kf_pos = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Vec2(geom.accent_offscreen), interp: Interpolation::EaseOut, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(0.8), value: ParamValue::Vec2(geom.accent_rest), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(5.2), value: ParamValue::Vec2(geom.accent_rest), interp: Interpolation::EaseIn, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(6.0), value: ParamValue::Vec2(geom.accent_offscreen), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "position", ParamValue::Vec2(geom.accent_rest), kf_pos);

    let kf_op = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Float(100.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(6.0), value: ParamValue::Float(100.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "opacity", ParamValue::Float(100.0), kf_op);

    e.layer = Some(Box::new(LayerExtra {
        uid,
        pin: Some(Pin {
            to: PinTarget::Layer(1),
            left: dir != Direction::Right,
            top: true,
            right: dir == Direction::Right,
            bottom: true,
            offsets: [0.0, 0.0, 0.0, 0.0],
            size_only: false,
        }),
        ..Default::default()
    }));
    e
}

fn make_text_layer(uid: u64, name: &str, text: &str, font_style: &str, size: f64, rest: Vec2, initial: Vec2) -> EffectInstance {
    let mut e = new_text_layer(text, rest, size);
    set_param(&mut e, "name", ParamValue::Text(name.into()));
    set_param(&mut e, "font", ParamValue::Text("Inter".into()));
    set_param(&mut e, "font_style", ParamValue::Text(font_style.into()));
    set_param(&mut e, "fill_color", ParamValue::Color([0.5176471, 0.8, 0.08627451, 1.0]));
    set_param(&mut e, "align", ParamValue::Choice(0));
    e.layer = Some(Box::new(LayerExtra { uid, ..Default::default() }));

    let kf_pos = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Vec2(initial), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(0.8), value: ParamValue::Vec2(initial), interp: Interpolation::EaseOut, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(1.5), value: ParamValue::Vec2(rest), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(4.5), value: ParamValue::Vec2(rest), interp: Interpolation::EaseIn, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(5.2), value: ParamValue::Vec2(initial), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(6.0), value: ParamValue::Vec2(initial), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "position", ParamValue::Vec2(rest), kf_pos);

    let kf_op = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Float(0.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(0.8), value: ParamValue::Float(0.0), interp: Interpolation::EaseOut, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(1.5), value: ParamValue::Float(100.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(4.5), value: ParamValue::Float(100.0), interp: Interpolation::EaseIn, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(5.2), value: ParamValue::Float(0.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(6.0), value: ParamValue::Float(0.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "opacity", ParamValue::Float(100.0), kf_op);

    e
}

fn build_template(dir: Direction) -> GraphicsTemplate {
    let geom = compute_geometry(dir);
    let layers = vec![
        make_plate_layer(1, &geom),
        make_accent_layer(2, dir, &geom),
        make_text_layer(3, "Title", DEFAULT_TITLE, "Bold", 44.0, geom.title_rest, geom.title_initial),
        make_text_layer(4, "Description", DEFAULT_DESCRIPTION, "Regular", 24.0, geom.desc_rest, geom.desc_initial),
    ];

    let controls = vec![
        ctl("title", "Title", 3, "text"),
        ctl("body", "Description", 4, "text"),
        ctl("accent_color", "Accent Color", 2, "fill_color"),
        ctl("text_color", "Title Color", 3, "fill_color"),
        ctl("body_color", "Description Color", 4, "fill_color"),
        ctl("box_color", "Box Color", 1, "fill_color"),
        slider("box_opacity", "Box Opacity", 1, "opacity", 0.0, 100.0),
        slider("box_width", "Box Width", 1, "size_w", 200.0, 1800.0),
        slider("box_height", "Box Height", 1, "size_h", 80.0, 900.0),
        slider("title_size", "Title Size", 3, "size", 20.0, 100.0),
        slider("body_size", "Description Size", 4, "size", 14.0, 60.0),
        ctl("font", "Font", 3, "font"),
    ];

    GraphicsTemplate {
        format: TEMPLATE_FORMAT.into(),
        version: TEMPLATE_VERSION,
        id: format!("user:{}", dir.id()),
        name: dir.name().into(),
        category: "Subtitles".into(),
        description: dir.description().into(),
        author: "FilmCraft user".into(),
        license: "MIT OR Apache-2.0".into(),
        tags: vec!["subtitles".into(), "unigine".into(), "lower-third".into()],
        canvas: [1920, 1080],
        duration: Tick(DURATION_TICKS),
        layers,
        graphic: GraphicMeta {
            intro: ticks(INTRO_SECS),
            outro: ticks(OUTRO_SECS),
            design_in: Tick(0),
            design_duration: Tick(DURATION_TICKS),
            ..Default::default()
        },
        controls,
        ..Default::default()
    }
}

fn resolve_out_dir() -> PathBuf {
    if let Some(arg) = std::env::args().nth(1) {
        return PathBuf::from(arg);
    }
    if let Ok(dir) = std::env::var("FILMCRAFT_TEMPLATES_DIR") {
        return PathBuf::from(dir);
    }
    let env = |k: &str| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    let data_dir = if let Some(d) = env("FILMCRAFT_DATA_DIR") {
        Some(d)
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/FilmCraft"))
    } else if cfg!(windows) {
        env("APPDATA").map(|h| h.join("FilmCraft"))
    } else {
        env("XDG_DATA_HOME").map(|d| d.join("filmcraft")).or_else(|| env("HOME").map(|h| h.join(".local/share/filmcraft")))
    };

    data_dir.map(|d| d.join("Graphics Templates")).unwrap_or_else(|| PathBuf::from("Graphics Templates"))
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = resolve_out_dir();
    fs::create_dir_all(&out_dir)?;

    for dir in [Direction::Left, Direction::Right, Direction::Up, Direction::Down] {
        let template = build_template(dir);
        let json_text = template.to_json();
        let bytes = json_text.as_bytes();

        let parsed = GraphicsTemplate::from_bytes(bytes).map_err(|e| format!("template parse error: {e:?}"))?;
        parsed.validate().map_err(|e| format!("template validation error: {e:?}"))?;

        let file_path = out_dir.join(format!("{}.fcgt", dir.id()));
        fs::write(&file_path, &json_text)?;
        println!("Generated & validated: {}", file_path.display());
    }

    Ok(())
}

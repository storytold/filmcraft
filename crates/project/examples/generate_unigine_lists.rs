//! Generates animated UNIGINE title list graphics templates (.fcgt).
//!
//! Creates 5-row staggered list templates sliding in from four directions (Left, Right, Up, Down).

use std::fs;
use std::path::PathBuf;

use filmcraft_geom::Vec2;
use filmcraft_project::effect::EffectInstance;
use filmcraft_project::graphic::{new_shape_layer, new_text_layer};
use filmcraft_project::graphic_design::{GraphicMeta, LayerExtra};
use filmcraft_project::gtemplate::{ControlKind, GraphicsTemplate, TEMPLATE_FORMAT, TEMPLATE_VERSION, TemplateControl};
use filmcraft_project::keyframe::{Interpolation, Keyframe, Param, ParamValue};
use filmcraft_time::{TICKS_PER_SECOND, Tick};

const TOTAL_SECONDS: f64 = 8.0;
const DURATION_TICKS: i64 = (TOTAL_SECONDS * TICKS_PER_SECOND as f64) as i64;
const STAGGER_SECS: f64 = 0.35;
const IN_SECS: f64 = 0.50;
const HOLD_END_SECS: f64 = 7.30;
const ROW_COUNT: usize = 5;

const ROW_WIDTH: f64 = 780.0;
const ROW_HEIGHT: f64 = 72.0;
const ACCENT_WIDTH: f64 = 8.0;
const ACCENT_HEIGHT: f64 = 72.0;

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

/// Slide direction for the title list animations.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Direction {
    /// Items slide in from the left edge.
    Left,
    /// Items slide in from the right edge.
    Right,
    /// Items slide up from the bottom edge.
    Up,
    /// Items slide down from the top edge.
    Down,
}

impl Direction {
    /// Unique template identifier string for this direction.
    fn id(self) -> &'static str {
        match self {
            Direction::Left => "unigine-list-slide-left",
            Direction::Right => "unigine-list-slide-right",
            Direction::Up => "unigine-list-slide-up",
            Direction::Down => "unigine-list-slide-down",
        }
    }

    /// User-facing display name for this template direction.
    fn name(self) -> &'static str {
        match self {
            Direction::Left => "Unigine List – Slide Left",
            Direction::Right => "Unigine List – Slide Right",
            Direction::Up => "Unigine List – Slide Up",
            Direction::Down => "Unigine List – Slide Down",
        }
    }

    /// User-facing description of the animation behavior.
    fn description(self) -> &'static str {
        match self {
            Direction::Left => "Animated list: 5 rows slide in from left one by one with accent bars.",
            Direction::Right => "Animated list: 5 rows slide in from right one by one with accent bars.",
            Direction::Up => "Animated list: 5 rows slide up from bottom one by one with accent bars.",
            Direction::Down => "Animated list: 5 rows slide down from top one by one with accent bars.",
        }
    }
}

/// Keyframe positions for a single row's plate, accent bar, and text layers.
struct RowGeometry {
    plate_rest: Vec2,
    plate_offscreen: Vec2,
    accent_rest: Vec2,
    accent_offscreen: Vec2,
    text_rest: Vec2,
    text_initial: Vec2,
    text_offscreen: Vec2,
}

/// Calculates spatial keyframe coordinates for a row given slide direction and row index.
fn compute_geometry(dir: Direction, row_idx: usize) -> RowGeometry {
    let row_y = 350.0 + (row_idx as f64 * 92.0);

    match dir {
        Direction::Left => {
            let plate_rest = Vec2::new(490.0, row_y);
            let plate_offscreen = Vec2::new(-500.0, row_y);
            let accent_rest = Vec2::new(104.0, row_y);
            let accent_offscreen = Vec2::new(-886.0, row_y);
            let text_rest = Vec2::new(132.0, row_y);
            let text_initial = Vec2::new(100.0, row_y);
            let text_offscreen = Vec2::new(-858.0, row_y);
            RowGeometry { plate_rest, plate_offscreen, accent_rest, accent_offscreen, text_rest, text_initial, text_offscreen }
        }
        Direction::Right => {
            let plate_rest = Vec2::new(1430.0, row_y);
            let plate_offscreen = Vec2::new(2420.0, row_y);
            let accent_rest = Vec2::new(1816.0, row_y);
            let accent_offscreen = Vec2::new(2806.0, row_y);
            let text_rest = Vec2::new(1060.0, row_y);
            let text_initial = Vec2::new(1092.0, row_y);
            let text_offscreen = Vec2::new(2050.0, row_y);
            RowGeometry { plate_rest, plate_offscreen, accent_rest, accent_offscreen, text_rest, text_initial, text_offscreen }
        }
        Direction::Up => {
            let plate_rest = Vec2::new(490.0, row_y);
            let plate_offscreen = Vec2::new(490.0, row_y + 800.0);
            let accent_rest = Vec2::new(104.0, row_y);
            let accent_offscreen = Vec2::new(104.0, row_y + 800.0);
            let text_rest = Vec2::new(132.0, row_y);
            let text_initial = Vec2::new(132.0, row_y + 24.0);
            let text_offscreen = Vec2::new(132.0, row_y + 800.0);
            RowGeometry { plate_rest, plate_offscreen, accent_rest, accent_offscreen, text_rest, text_initial, text_offscreen }
        }
        Direction::Down => {
            let plate_rest = Vec2::new(490.0, row_y);
            let plate_offscreen = Vec2::new(490.0, row_y - 800.0);
            let accent_rest = Vec2::new(104.0, row_y);
            let accent_offscreen = Vec2::new(104.0, row_y - 800.0);
            let text_rest = Vec2::new(132.0, row_y);
            let text_initial = Vec2::new(132.0, row_y - 24.0);
            let text_offscreen = Vec2::new(132.0, row_y - 800.0);
            RowGeometry { plate_rest, plate_offscreen, accent_rest, accent_offscreen, text_rest, text_initial, text_offscreen }
        }
    }
}

/// Builds the background plate shape layer for a specific row index.
fn make_plate_layer(uid: u64, row_idx: usize, geom: &RowGeometry) -> EffectInstance {
    let mut e = new_shape_layer(0, geom.plate_rest, Vec2::new(ROW_WIDTH, ROW_HEIGHT), vec![]);
    set_param(&mut e, "name", ParamValue::Text(format!("Plate Row {}", row_idx + 1)));
    set_param(&mut e, "fill_color", ParamValue::Color([0.043137256, 0.07058824, 0.11764706, 1.0]));
    set_param(&mut e, "corner_radius", ParamValue::Float(6.0));
    e.layer = Some(Box::new(LayerExtra { uid, ..Default::default() }));

    let t_start = row_idx as f64 * STAGGER_SECS;
    let t_in = t_start + IN_SECS;
    let t_hold_end = HOLD_END_SECS;
    let t_end = TOTAL_SECONDS;

    let kf_pos = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Vec2(geom.plate_offscreen), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_start), value: ParamValue::Vec2(geom.plate_offscreen), interp: Interpolation::EaseOut, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(t_in), value: ParamValue::Vec2(geom.plate_rest), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_hold_end), value: ParamValue::Vec2(geom.plate_rest), interp: Interpolation::EaseIn, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(t_end), value: ParamValue::Vec2(geom.plate_offscreen), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "position", ParamValue::Vec2(geom.plate_rest), kf_pos);

    let kf_op = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Float(85.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_hold_end), value: ParamValue::Float(85.0), interp: Interpolation::EaseIn, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_end), value: ParamValue::Float(0.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "opacity", ParamValue::Float(85.0), kf_op);

    e
}

/// Builds the vertical accent bar shape layer for a specific row index.
fn make_accent_layer(uid: u64, row_idx: usize, geom: &RowGeometry) -> EffectInstance {
    let mut e = new_shape_layer(0, geom.accent_rest, Vec2::new(ACCENT_WIDTH, ACCENT_HEIGHT), vec![]);
    set_param(&mut e, "name", ParamValue::Text(format!("Accent Row {}", row_idx + 1)));
    set_param(&mut e, "fill_color", ParamValue::Color([1.0, 0.85, 0.2, 1.0]));
    e.layer = Some(Box::new(LayerExtra { uid, ..Default::default() }));

    let t_start = row_idx as f64 * STAGGER_SECS;
    let t_in = t_start + IN_SECS;
    let t_hold_end = HOLD_END_SECS;
    let t_end = TOTAL_SECONDS;

    let kf_pos = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Vec2(geom.accent_offscreen), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe {
            time: ticks(t_start),
            value: ParamValue::Vec2(geom.accent_offscreen),
            interp: Interpolation::EaseOut,
            out_influence: 0.4,
            in_influence: 0.4,
        },
        Keyframe { time: ticks(t_in), value: ParamValue::Vec2(geom.accent_rest), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_hold_end), value: ParamValue::Vec2(geom.accent_rest), interp: Interpolation::EaseIn, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(t_end), value: ParamValue::Vec2(geom.accent_offscreen), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "position", ParamValue::Vec2(geom.accent_rest), kf_pos);

    let kf_op = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Float(100.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_hold_end), value: ParamValue::Float(100.0), interp: Interpolation::EaseIn, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_end), value: ParamValue::Float(0.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "opacity", ParamValue::Float(100.0), kf_op);

    e
}

/// Builds the text layer for a specific row index with stagger-delayed reveal animation.
fn make_text_layer(uid: u64, row_idx: usize, geom: &RowGeometry) -> EffectInstance {
    let default_label = format!("Item {}", row_idx + 1);
    let mut e = new_text_layer(&default_label, geom.text_rest, 32.0);
    set_param(&mut e, "name", ParamValue::Text(format!("Text Row {}", row_idx + 1)));
    set_param(&mut e, "font", ParamValue::Text("Inter".into()));
    set_param(&mut e, "font_style", ParamValue::Text("SemiBold".into()));
    set_param(&mut e, "fill_color", ParamValue::Color([1.0, 1.0, 1.0, 1.0]));
    set_param(&mut e, "align", ParamValue::Choice(0)); // Left align
    e.layer = Some(Box::new(LayerExtra { uid, ..Default::default() }));

    let t_start = row_idx as f64 * STAGGER_SECS;
    let t_reveal_start = t_start + 0.15;
    let t_reveal_end = t_start + 0.45;
    let t_hold_end = HOLD_END_SECS;
    let t_end = TOTAL_SECONDS;

    let kf_pos = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Vec2(geom.text_initial), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe {
            time: ticks(t_reveal_start),
            value: ParamValue::Vec2(geom.text_initial),
            interp: Interpolation::EaseOut,
            out_influence: 0.4,
            in_influence: 0.4,
        },
        Keyframe { time: ticks(t_reveal_end), value: ParamValue::Vec2(geom.text_rest), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_hold_end), value: ParamValue::Vec2(geom.text_rest), interp: Interpolation::EaseIn, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(t_end), value: ParamValue::Vec2(geom.text_offscreen), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "position", ParamValue::Vec2(geom.text_rest), kf_pos);

    let kf_op = vec![
        Keyframe { time: ticks(0.0), value: ParamValue::Float(0.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_reveal_start), value: ParamValue::Float(0.0), interp: Interpolation::EaseOut, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(t_reveal_end), value: ParamValue::Float(100.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_hold_end), value: ParamValue::Float(100.0), interp: Interpolation::EaseIn, out_influence: 0.33, in_influence: 0.33 },
        Keyframe { time: ticks(t_end), value: ParamValue::Float(0.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "opacity", ParamValue::Float(0.0), kf_op);

    e
}

/// Assembles complete GraphicsTemplate structure with layers and control bindings for the chosen direction.
fn build_template(dir: Direction) -> GraphicsTemplate {
    let mut layers = Vec::with_capacity(ROW_COUNT * 3);
    for row in 0..ROW_COUNT {
        let geom = compute_geometry(dir, row);
        let uid_plate = (row * 3 + 1) as u64;
        let uid_accent = (row * 3 + 2) as u64;
        let uid_text = (row * 3 + 3) as u64;

        layers.push(make_plate_layer(uid_plate, row, &geom));
        layers.push(make_accent_layer(uid_accent, row, &geom));
        layers.push(make_text_layer(uid_text, row, &geom));
    }

    let controls = vec![
        ctl("row1", "Row 1", 3, "text"),
        ctl("row2", "Row 2", 6, "text"),
        ctl("row3", "Row 3", 9, "text"),
        ctl("row4", "Row 4", 12, "text"),
        ctl("row5", "Row 5", 15, "text"),
        ctl("show_row4", "Show Row 4 Plate", 10, "enabled"),
        ctl("show_row4_bar", "Show Row 4 Accent", 11, "enabled"),
        ctl("show_row4_text", "Show Row 4 Text", 12, "enabled"),
        ctl("show_row5", "Show Row 5 Plate", 13, "enabled"),
        ctl("show_row5_bar", "Show Row 5 Accent", 14, "enabled"),
        ctl("show_row5_text", "Show Row 5 Text", 15, "enabled"),
    ];

    let intro_ticks = (ROW_COUNT as f64 * STAGGER_SECS + IN_SECS) * TICKS_PER_SECOND as f64;
    let outro_ticks = (TOTAL_SECONDS - HOLD_END_SECS) * TICKS_PER_SECOND as f64;

    GraphicsTemplate {
        format: TEMPLATE_FORMAT.into(),
        version: TEMPLATE_VERSION,
        id: format!("user:{}", dir.id()),
        name: dir.name().into(),
        category: "Lists".into(),
        description: dir.description().into(),
        author: "FilmCraft user".into(),
        license: "MIT OR Apache-2.0".into(),
        tags: vec!["lists".into(), "unigine".into(), "titles".into(), "lower-third".into()],
        canvas: [1920, 1080],
        duration: Tick(DURATION_TICKS),
        layers,
        graphic: GraphicMeta {
            intro: Tick(intro_ticks.round() as i64),
            outro: Tick(outro_ticks.round() as i64),
            design_in: Tick(0),
            design_duration: Tick(DURATION_TICKS),
            ..Default::default()
        },
        controls,
        ..Default::default()
    }
}

/// Resolves the destination directory for generated graphics templates.
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

/// Generates all direction variants of the UNIGINE list template, validates them, and writes `.fcgt` files.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = resolve_out_dir();
    fs::create_dir_all(&out_dir)?;

    let directions = [Direction::Left, Direction::Right, Direction::Up, Direction::Down];

    for dir in directions {
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

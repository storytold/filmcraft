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

fn ticks(s: f64) -> Tick {
    Tick((s * TICKS_PER_SECOND as f64).round() as i64)
}

fn set_param(e: &mut EffectInstance, id: &str, v: ParamValue) {
    e.params.insert(id.to_string(), Param::new(v));
}

fn set_animated(e: &mut EffectInstance, id: &str, static_val: ParamValue, keyframes: Vec<Keyframe>) {
    e.params.insert(id.to_string(), Param { value: static_val, keyframes });
}

fn ctl(id: &str, name: &str, layer: u64, param: &str) -> TemplateControl {
    TemplateControl { id: id.into(), name: name.into(), kind: ControlKind::for_param(param), layer, param: param.into(), min: None, max: None }
}

fn slider(id: &str, name: &str, layer: u64, param: &str, min: f64, max: f64) -> TemplateControl {
    TemplateControl { min: Some(min), max: Some(max), ..ctl(id, name, layer, param) }
}

#[derive(Clone, Copy, Debug)]
#[allow(clippy::enum_variant_names)]
enum Direction {
    SlideLeft,
    SlideRight,
    SlideUp,
    SlideDown,
}

impl Direction {
    /// Identifier string for the template variant.
    fn id(self) -> &'static str {
        match self {
            Direction::SlideLeft => "unigine-list-slide-left",
            Direction::SlideRight => "unigine-list-slide-right",
            Direction::SlideUp => "unigine-list-slide-up",
            Direction::SlideDown => "unigine-list-slide-down",
        }
    }

    /// User-facing display name of the template variant.
    fn name(self) -> &'static str {
        match self {
            Direction::SlideLeft => "Unigine List – Slide Left",
            Direction::SlideRight => "Unigine List – Slide Right",
            Direction::SlideUp => "Unigine List – Slide Up",
            Direction::SlideDown => "Unigine List – Slide Down",
        }
    }

    /// Summary description of the template's layout and animation behavior.
    fn description(self) -> &'static str {
        match self {
            Direction::SlideLeft => "Animated list: 5 rows slide in from left one by one with accent bars.",
            Direction::SlideRight => "Animated list: 5 rows slide in from right one by one with accent bars.",
            Direction::SlideUp => "Animated list: 5 rows slide up from bottom one by one with accent bars.",
            Direction::SlideDown => "Animated list: 5 rows slide down from top one by one with accent bars.",
        }
    }
}

struct RowGeometry {
    plate_rest: Vec2,
    plate_offscreen: Vec2,
    accent_rest: Vec2,
    accent_offscreen: Vec2,
    text_rest: Vec2,
    text_initial: Vec2,
    text_offscreen: Vec2,
}

/// Computes on-screen and off-screen positions for a row according to slide direction.
fn compute_geometry(dir: Direction, row_idx: usize) -> RowGeometry {
    let row_y = 350.0 + (row_idx as f64 * 92.0);

    match dir {
        Direction::SlideLeft => {
            let plate_rest = Vec2::new(490.0, row_y);
            let plate_offscreen = Vec2::new(-500.0, row_y);
            let accent_rest = Vec2::new(104.0, row_y);
            let accent_offscreen = Vec2::new(-886.0, row_y);
            let text_rest = Vec2::new(132.0, row_y);
            let text_initial = Vec2::new(100.0, row_y);
            let text_offscreen = Vec2::new(-858.0, row_y);
            RowGeometry { plate_rest, plate_offscreen, accent_rest, accent_offscreen, text_rest, text_initial, text_offscreen }
        }
        Direction::SlideRight => {
            let plate_rest = Vec2::new(1430.0, row_y);
            let plate_offscreen = Vec2::new(2420.0, row_y);
            let accent_rest = Vec2::new(1816.0, row_y);
            let accent_offscreen = Vec2::new(2806.0, row_y);
            let text_rest = Vec2::new(1060.0, row_y);
            let text_initial = Vec2::new(1092.0, row_y);
            let text_offscreen = Vec2::new(2050.0, row_y);
            RowGeometry { plate_rest, plate_offscreen, accent_rest, accent_offscreen, text_rest, text_initial, text_offscreen }
        }
        Direction::SlideUp => {
            let plate_rest = Vec2::new(490.0, row_y);
            let plate_offscreen = Vec2::new(490.0, row_y + 800.0);
            let accent_rest = Vec2::new(104.0, row_y);
            let accent_offscreen = Vec2::new(104.0, row_y + 800.0);
            let text_rest = Vec2::new(132.0, row_y);
            let text_initial = Vec2::new(132.0, row_y + 24.0);
            let text_offscreen = Vec2::new(132.0, row_y + 800.0);
            RowGeometry { plate_rest, plate_offscreen, accent_rest, accent_offscreen, text_rest, text_initial, text_offscreen }
        }
        Direction::SlideDown => {
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

/// Creates a background plate shape layer with staggered slide and fade keyframes.
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

/// Creates an accent vertical bar shape layer with staggered slide keyframes.
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

/// Creates a text title layer with position slide and opacity reveal keyframes.
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
        Keyframe { time: ticks(t_hold_end), value: ParamValue::Float(100.0), interp: Interpolation::EaseIn, out_influence: 0.4, in_influence: 0.4 },
        Keyframe { time: ticks(t_end), value: ParamValue::Float(0.0), interp: Interpolation::Linear, out_influence: 0.33, in_influence: 0.33 },
    ];
    set_animated(&mut e, "opacity", ParamValue::Float(0.0), kf_op);

    e
}

/// Constructs a 5-row list graphics template for the given direction.
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
        ctl("show_row4", "Show Row 4", 10, "enabled"),
        ctl("show_row4_bar", "Show Row 4 Accent", 11, "enabled"),
        ctl("show_row4_text", "Show Row 4 Text", 12, "enabled"),
        ctl("show_row5", "Show Row 5", 13, "enabled"),
        ctl("show_row5_bar", "Show Row 5 Accent", 14, "enabled"),
        ctl("show_row5_text", "Show Row 5 Text", 15, "enabled"),
        ctl("accent_color", "Accent Color", 2, "fill_color"),
        ctl("text_color", "Text Color", 3, "fill_color"),
        ctl("box_color", "Box Color", 1, "fill_color"),
        slider("box_opacity", "Box Opacity", 1, "opacity", 0.0, 100.0),
        slider("text_size", "Text Size", 3, "size", 18.0, 60.0),
        ctl("font", "Font", 3, "font"),
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

fn main() {
    let out_dir = match std::env::var("HOME") {
        Ok(h) => PathBuf::from(h).join(".local/share/filmcraft/Graphics Templates"),
        Err(_) => PathBuf::from("Graphics Templates"),
    };
    fs::create_dir_all(&out_dir).expect("create output directory");

    let directions = [Direction::SlideLeft, Direction::SlideRight, Direction::SlideUp, Direction::SlideDown];

    for dir in directions {
        let template = build_template(dir);
        let json_text = template.to_json();
        let bytes = json_text.as_bytes();

        let parsed = GraphicsTemplate::from_bytes(bytes).expect("template must parse");
        parsed.validate().expect("template must validate");

        let file_path = out_dir.join(format!("{}.fcgt", dir.id()));
        fs::write(&file_path, &json_text).expect("write template file");
        println!("Generated & validated: {}", file_path.display());
    }
}

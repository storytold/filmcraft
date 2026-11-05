//! View menu and monitor view options, driven headless through the control channel: playback /
//! paused resolution, High Quality Playback, display modes (channels, comparison, multi-camera,
//! source waveform), magnification and panning, rulers and guides (drag out of the ruler, move,
//! lock, clear, Add Guide…, templates), Snap in Program Monitor, and the Graphics menu commands.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the test also renders the UI with wgpu and writes `view-*.png`.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<std::path::PathBuf>,
    /// Where the pointer was last put by `move_to` or `press`.
    pointer: egui::Pos2,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots, pointer: egui::Pos2::ZERO };
        d.frames(4);
        d
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn menu(&mut self, id: &str) -> Value {
        let r = self.ok("ui.menu.invoke", json!({"id": id}));
        self.frames(2);
        r
    }
    fn program(&mut self) -> Value {
        self.ok("ui.inspect", json!({}))["ui"]["program"].clone()
    }
    fn rect(&mut self, id: &str) -> [f64; 4] {
        self.try_rect(id).unwrap_or_else(|| panic!("no element {id}"))
    }
    fn try_rect(&mut self, id: &str) -> Option<[f64; 4]> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id)?.clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Some([r[0], r[1], r[2], r[3]])
    }
    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }
    /// Move the pointer to `(x, y)` a few points per frame, as a mouse does (a button held stays
    /// held), and let the UI react.
    fn move_to(&mut self, x: f64, y: f64) {
        let (from, to) = (self.pointer, egui::pos2(x as f32, y as f32));
        let steps = ((to - from).length() / 20.0).ceil().max(1.0) as usize;
        for i in 1..=steps {
            self.harness.input_mut().events.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
        self.pointer = to;
        self.frames(2);
    }
    /// Press the primary button at `(x, y)`.
    fn press(&mut self, x: f64, y: f64) {
        let pos = egui::pos2(x as f32, y as f32);
        let events = &mut self.harness.input_mut().events;
        events.push(egui::Event::PointerMoved(pos));
        events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.pointer = pos;
        self.frames(1);
    }
    fn release(&mut self, x: f64, y: f64) {
        let pos = egui::pos2(x as f32, y as f32);
        self.harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        self.frames(2);
    }
    /// The cursor the last frame asked for.
    fn cursor(&self) -> egui::CursorIcon {
        self.harness.output().platform_output.cursor_icon
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        // give the frame workers time to deliver full-resolution frames
        for _ in 0..8 {
            self.frames(3);
            std::thread::sleep(std::time::Duration::from_millis(150));
        }
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("view-{name}.png"))).unwrap();
    }
}

fn first_media_item(v: &Value) -> Option<u64> {
    match v {
        Value::Object(m) => {
            if let Some(id) = m.get("item").and_then(Value::as_u64)
                && m.get("type").and_then(Value::as_str).is_some_and(|t| t == "Movie")
            {
                return Some(id);
            }
            m.values().find_map(first_media_item)
        }
        Value::Array(a) => a.iter().find_map(first_media_item),
        _ => None,
    }
}

fn item_duration(v: &Value, item: u64) -> Option<f64> {
    match v {
        Value::Object(m) if m.get("item").and_then(Value::as_u64) == Some(item) && m.contains_key("duration") => m["duration"].as_f64(),
        Value::Object(m) => m.values().find_map(|v| item_duration(v, item)),
        Value::Array(a) => a.iter().find_map(|v| item_duration(v, item)),
        _ => None,
    }
}

#[test]
fn view_menu_resolutions_and_display_modes() {
    let mut d = Driver::demo();
    let menu = d.ok("ui.menu.list", json!({}));
    let item = |id: &str| menu.as_array().unwrap().iter().find(|m| m["id"] == id).cloned().unwrap_or_else(|| panic!("no menu item {id}"));
    assert_eq!(item("view.playbackRes.sixteenth")["path"], json!(["View", "Playback Resolution"]));
    assert_eq!(item("view.pausedRes.full")["checked"], true, "Paused Resolution defaults to Full");
    assert_eq!(item("view.display.composite")["checked"], true);
    assert_eq!(item("view.magnification.1600")["label"], "1600%");
    assert_eq!(item("view.clearGuides")["enabled"], false, "no guides yet");
    assert_eq!(item("graphics.alignFrame.left")["path"], json!(["Graphics and Titles", "Align to Video Frame"]));

    d.menu("view.playbackRes.sixteenth");
    d.menu("view.pausedRes.quarter");
    d.menu("view.highQualityPlayback");
    let p = d.program();
    assert_eq!((p["res"].as_str(), p["paused_res"].as_str(), p["high_quality"].as_bool()), (Some("Sixteenth"), Some("Quarter"), Some(true)));

    // channels
    d.menu("view.display.red");
    assert_eq!(d.program()["display"], "Red");
    d.shot("display-red");
    d.menu("view.display.alpha");
    assert_eq!(d.program()["display"], "Alpha");

    // comparison view: reference frame at the playhead, stepped by the reference buttons
    d.exec("playhead.set", json!({"seconds": 2}));
    d.menu("view.display.comparison");
    let p = d.program();
    assert_eq!(p["display"], "Comparison");
    let r0 = p["compare_ref"].as_i64().unwrap();
    d.frames(3);
    assert!(d.try_rect("program.compare.reference").is_some());
    d.click("program.compare.next");
    let r1 = d.program()["compare_ref"].as_i64().unwrap();
    assert!(r1 > r0, "reference stepped forward: {r0} → {r1}");
    d.exec("playhead.set", json!({"seconds": 4}));
    d.shot("comparison");

    // Multi-Camera links to the existing multi-camera view; Composite turns it off
    d.menu("view.display.multicam");
    assert_eq!(d.program()["multicam"], true);
    d.menu("view.display.composite");
    let p = d.program();
    assert_eq!((p["multicam"].as_bool(), p["display"].as_str()), (Some(false), Some("Composite")));

    // wrench menu items carry automation ids
    d.click("program.settings");
    assert!(d.try_rect("program.settings.highQualityPlayback").is_some(), "wrench menu lists High Quality Playback");
    d.click("program.settings.highQualityPlayback");
    assert_eq!(d.program()["high_quality"], false);

    // source monitor: Audio Waveform display mode
    let project = d.exec("project.inspect", json!({}));
    let item = first_media_item(&project).expect("media item");
    d.exec("source.open", json!({"item": item}));
    d.menu("view.display.audioWaveform");
    let s = d.ok("ui.inspect", json!({}))["ui"]["source"]["display"].clone();
    assert_eq!(s, "AudioWaveform");
    d.frames(3);
    assert!(d.try_rect("source.waveform").is_some(), "waveform drawn in the source monitor");
    d.shot("source-waveform");
    d.menu("view.display.videoAndWaveform");
    d.frames(2);
    assert!(d.try_rect("source.waveform").is_some() && d.try_rect("source.picture").is_some());
    // ui.set reaches the monitor state directly
    d.ok("ui.set", json!({"source": {"display": "Composite", "show_rulers": true}}));
    let s = d.ok("ui.inspect", json!({}))["ui"]["source"].clone();
    assert_eq!((s["display"].as_str(), s["show_rulers"].as_bool()), (Some("Composite"), Some(true)));
}

/// The filled rectangles and the mesh vertex colors painted this frame inside `area`.
fn fills_in(d: &Driver, area: egui::Rect) -> (Vec<(egui::Rect, egui::Color32)>, Vec<egui::Color32>) {
    fn walk(s: &egui::Shape, area: egui::Rect, rects: &mut Vec<(egui::Rect, egui::Color32)>, verts: &mut Vec<egui::Color32>) {
        match s {
            egui::Shape::Rect(r) if area.expand(0.5).contains_rect(r.rect) => rects.push((r.rect, r.fill)),
            egui::Shape::Mesh(m) => verts.extend(m.vertices.iter().filter(|v| area.expand(0.5).contains(v.pos)).map(|v| v.color)),
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, area, rects, verts)),
            _ => {}
        }
    }
    let (mut rects, mut verts) = (Vec::new(), Vec::new());
    d.harness.output().shapes.iter().for_each(|c| walk(&c.shape, area, &mut rects, &mut verts));
    (rects, verts)
}

/// The wrench menu's Transparency Grid paints a gray and white checkerboard behind
/// the picture instead of the monitor background; each monitor keeps its own setting.
#[test]
fn transparency_grid_paints_a_checkerboard_behind_the_picture() {
    use filmcraft_ui_egui::frames::Target;
    use filmcraft_ui_egui::panels::monitor_view::{GRID_DARK, GRID_LIGHT};
    let mut d = Driver::demo();
    // the check mark of the wrench menu item
    let checked = |d: &mut Driver| filmcraft_ui_egui::panels::monitor_view::checked(d.harness.state(), "view.transparencyGrid");
    let menu = d.ok("ui.menu.list", json!({}));
    assert!(menu.as_array().unwrap().iter().all(|m| m["id"] != "view.transparencyGrid"), "not in the View menu");
    assert_eq!(d.program()["transparency_grid"], false, "off by default");
    assert_eq!(checked(&mut d), Some(false));
    let r = d.rect("program.picture");
    let pic = egui::Rect::from_min_size(egui::pos2(r[0] as f32, r[1] as f32), egui::vec2(r[2] as f32, r[3] as f32));
    let covers = |rects: &[(egui::Rect, egui::Color32)]| {
        rects.iter().any(|(r, c)| *c == GRID_LIGHT && (r.min - pic.min).length() < 1.0 && (r.max - pic.max).length() < 1.0)
    };
    let (rects, verts) = fills_in(&d, pic);
    assert!(!covers(&rects) && !verts.contains(&GRID_DARK), "no grid while it is off");

    d.click("program.settings");
    assert!(d.try_rect("program.settings.transparencyGrid").is_some(), "the wrench menu lists Transparency Grid");
    d.click("program.settings.transparencyGrid");
    d.frames(2);
    assert_eq!(d.program()["transparency_grid"], true);
    assert_eq!(checked(&mut d), Some(true));
    let (rects, verts) = fills_in(&d, pic);
    assert!(covers(&rects), "the light squares fill the picture");
    let dark = verts.iter().filter(|c| **c == GRID_DARK).count();
    assert!(dark >= 4 * 100, "the dark squares are drawn: {dark} vertices");
    // the Program picture is rendered live (render previews are flattened over black)
    let live = |d: &Driver| matches!(d.harness.state().program_picture().map(|k| k.target), Some(Target::SequenceLive(_)));
    for _ in 0..600 {
        if live(&d) {
            break;
        }
        d.frames(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!(live(&d), "the Program monitor shows a live render: {:?}", d.harness.state().program_picture());
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["source"]["transparency_grid"], false, "the Source monitor keeps its own setting");

    // Comparison View: the reference frame has the grid behind it too
    d.menu("view.display.comparison");
    d.frames(3);
    let r = d.rect("program.compare.reference");
    let reference = egui::Rect::from_min_size(egui::pos2(r[0] as f32, r[1] as f32), egui::vec2(r[2] as f32, r[3] as f32));
    let (rects, _) = fills_in(&d, reference);
    assert!(
        rects.iter().any(|(r, c)| *c == GRID_LIGHT && (r.min - reference.min).length() < 1.0 && (r.max - reference.max).length() < 1.0),
        "the light squares fill the reference frame"
    );
    d.menu("view.display.composite");
    d.frames(2);

    d.ok("ui.menu.invoke", json!({"id": "view.transparencyGrid", "params": {"monitor": "source", "enabled": true}}));
    assert_eq!(d.ok("ui.inspect", json!({}))["ui"]["source"]["transparency_grid"], true);
    assert_eq!(d.program()["transparency_grid"], true);

    // a Transparent Video in the Source monitor: the frame keeps its alpha and goes up as the
    // grid's texture, so the grid shows through it
    let item = d.exec("file.newTransparentVideo", json!({}))["item"].as_u64().expect("new item");
    d.exec("source.open", json!({"item": item}));
    let mut key = None;
    for _ in 0..600 {
        d.frames(1);
        key = d.harness.state().texture_key("monitor-source-grid");
        if key.is_some() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    let key = key.expect("the Source monitor shows its frame through the grid");
    let frame = d.harness.state().frames.get(&key).expect("the shown frame is cached");
    assert!(!frame.px.is_empty() && frame.px.as_chunks::<4>().0.iter().all(|p| p[3] == 0), "the frame keeps its alpha");

    d.ok("ui.menu.invoke", json!({"id": "view.transparencyGrid", "params": {"monitor": "program"}}));
    d.frames(2);
    assert_eq!(d.program()["transparency_grid"], false);
    let (rects, verts) = fills_in(&d, pic);
    assert!(!covers(&rects) && !verts.contains(&GRID_DARK), "the grid goes when it is turned off");
}

/// Display Mode ▸ Alpha shows a live render of the sequence (render previews are opaque); Red
/// keeps the usual frames.
#[test]
fn alpha_display_mode_shows_a_live_render() {
    use filmcraft_ui_egui::frames::Target;
    let shown_live = |d: &mut Driver, live: bool| {
        let is = |d: &Driver| match d.harness.state().program_picture().map(|k| k.target) {
            Some(Target::SequenceLive(_)) => live,
            Some(Target::Sequence(_)) => !live,
            _ => false,
        };
        for _ in 0..600 {
            if is(d) {
                break;
            }
            d.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(is(d), "live render {live}: {:?}", d.harness.state().program_picture());
    };
    let mut d = Driver::demo();
    d.menu("view.display.alpha");
    shown_live(&mut d, true);
    d.menu("view.display.red");
    shown_live(&mut d, false);
}

fn num(v: &Value) -> f64 {
    v.as_f64().unwrap_or_else(|| panic!("not a number: {v}"))
}

/// #807: the wheel over a monitor zooms about the pointer, a middle-button drag pans, Fit resets,
/// and each monitor keeps its own view.
#[test]
fn wheel_zooms_about_the_pointer_and_the_middle_button_pans() {
    let mut d = Driver::demo();
    // at 100% the picture is larger than the monitor, so the element is the monitor's area
    d.menu("view.magnification.100");
    let area = d.rect("program.picture");
    let centre = (area[0] + area[2] / 2.0, area[1] + area[3] / 2.0);
    let at = (area[0] + area[2] * 0.25, area[1] + area[3] * 0.4);
    // one wheel notch (40 points) up: 10% larger; the point under the pointer stays under it
    d.ok("ui.scroll", json!({"x": at.0, "y": at.1, "dx": 0.0, "dy": 40.0}));
    d.frames(2);
    let p = d.program();
    assert!((num(&p["zoom"]) - 1.1).abs() < 1e-4, "{p}");
    let want = ((centre.0 - at.0) * 0.1, (centre.1 - at.1) * 0.1);
    assert!((num(&p["pan"][0]) - want.0).abs() < 1.0 && (num(&p["pan"][1]) - want.1).abs() < 1.0, "pan {} for {want:?}", p["pan"]);
    // a notch down, then up again, comes back to the same zoom
    d.ok("ui.scroll", json!({"x": at.0, "y": at.1, "dx": 0.0, "dy": -40.0}));
    d.frames(2);
    assert!((num(&d.program()["zoom"]) - 1.0).abs() < 1e-4, "{}", d.program());
    d.ok("ui.scroll", json!({"x": at.0, "y": at.1, "dx": 0.0, "dy": 40.0}));
    d.frames(2);
    let p = d.program();
    assert!((num(&p["zoom"]) - 1.1).abs() < 1e-4, "{p}");

    // the middle button drags the picture, whatever the tool
    let (zoom, pan) = (num(&p["zoom"]), [num(&p["pan"][0]), num(&p["pan"][1])]);
    let send = |d: &mut Driver, e: egui::Event| {
        d.harness.input_mut().events.push(e);
        d.frames(1);
    };
    let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Middle, pressed, modifiers: Default::default() };
    let from = egui::pos2(centre.0 as f32, centre.1 as f32);
    send(&mut d, egui::Event::PointerMoved(from));
    send(&mut d, button(from, true));
    for k in 1..=4 {
        send(&mut d, egui::Event::PointerMoved(from + egui::vec2(-10.0 * k as f32, 5.0 * k as f32)));
    }
    send(&mut d, button(from + egui::vec2(-40.0, 20.0), false));
    let p = d.program();
    assert_eq!(num(&p["zoom"]), zoom, "a pan does not zoom");
    assert!((num(&p["pan"][0]) - (pan[0] - 40.0)).abs() < 1.0 && (num(&p["pan"][1]) - (pan[1] + 20.0)).abs() < 1.0, "pan {} after {pan:?}", p["pan"]);

    // zooming out stops at the menu's smallest level; Fit resets zoom and pan
    for _ in 0..12 {
        d.ok("ui.scroll", json!({"x": centre.0, "y": centre.1, "dx": 0.0, "dy": -200.0}));
    }
    d.frames(2);
    assert!((num(&d.program()["zoom"]) - 0.1).abs() < 1e-6, "stops at 10%");
    d.menu("view.magnification.fit");
    let p = d.program();
    assert!(p["zoom"].is_null() && p["pan"] == json!([0.0, 0.0]), "{p}");

    // from Fit, the wheel zooms in too; the Source monitor keeps its own view
    d.ok("ui.scroll", json!({"x": centre.0, "y": centre.1, "dx": 0.0, "dy": 20.0}));
    d.frames(2);
    assert!(d.program()["zoom"].as_f64().is_some(), "zoomed in from Fit");
    let source = d.ok("ui.inspect", json!({}))["ui"]["source"].clone();
    assert!(source["zoom"].is_null() && source["pan"] == json!([0.0, 0.0]), "{source}");
}

/// The x positions of the tick marks drawn on the time ruler `bar`.
fn ruler_ticks(d: &Driver, bar: [f64; 4]) -> Vec<f64> {
    let bottom = (bar[1] + bar[3]) as f32;
    let (left, right) = (bar[0] as f32 - 1.0, (bar[0] + bar[2]) as f32 + 1.0);
    let mut xs: Vec<f64> = d
        .harness
        .output()
        .shapes
        .iter()
        .filter_map(|s| match &s.shape {
            egui::Shape::LineSegment { points: [a, b], .. }
                if a.x == b.x && (b.y - bottom).abs() < 0.01 && [3.5, 8.0].iter().any(|h| (b.y - a.y - h).abs() < 0.01) && (left..=right).contains(&a.x) =>
            {
                Some(f64::from(a.x))
            }
            _ => None,
        })
        .collect();
    xs.sort_by(f64::total_cmp);
    xs
}

/// The frames the ticks `xs` of the ruler `bar` stand for while it shows `zoom` (fractions) of a
/// sequence `frames` long; and that each is a whole frame.
fn tick_frames(xs: &[f64], bar: [f64; 4], zoom: (f64, f64), frames: f64) -> Vec<f64> {
    let f: Vec<f64> = xs.iter().map(|x| (zoom.0 + (x - bar[0]) / bar[2] * (zoom.1 - zoom.0)) * frames).collect();
    assert!(f.iter().all(|f| (f - f.round()).abs() < 0.05), "ticks off whole frames: {f:?}");
    f
}

/// The minor tick step (frames): the commonest gap between neighbouring ticks.
fn tick_step(frames: &[f64]) -> i64 {
    let mut counts = std::collections::BTreeMap::new();
    for w in frames.windows(2) {
        *counts.entry((w[1] - w[0]).round() as i64).or_insert(0) += 1;
    }
    counts.into_iter().max_by_key(|(_, n)| *n).map(|(gap, _)| gap).expect("ticks")
}

/// The zoom bar under a monitor's time ruler: a handle zooms the ruler in, the middle scrolls it,
/// the ruler maps clicks and the playhead to the part it shows, and each monitor keeps its own.
#[test]
fn time_zoom_bar_zooms_and_scrolls_the_monitor_ruler() {
    let mut d = Driver::demo();
    let seq = d.exec("sequence.inspect", json!({}));
    let duration = num(&seq["duration"]);
    let rate = &seq["settings"]["frame_rate"];
    let frames = duration * num(&rate["num"]) / num(&rate["den"]) / 254_016_000_000.0;
    let track = d.rect("program.zoomBar.track");
    assert!((d.rect("program.zoomBar")[2] - track[2]).abs() < 0.5, "the whole sequence at first");
    let bar = d.rect("program.scrubBar");
    let full = tick_frames(&ruler_ticks(&d, bar), bar, (0.0, 1.0), frames);
    // the right handle dragged to the middle: the ruler shows the first half
    let y = track[1] + track[3] / 2.0;
    d.ok("ui.drag", json!({"from": {"x": track[0] + track[2] - 4.0, "y": y}, "to": {"x": track[0] + track[2] / 2.0, "y": y}, "steps": 8}));
    d.frames(2);
    let z = d.program()["time_zoom"].clone();
    assert!(num(&z[0]) == 0.0 && (num(&z[1]) - 0.5).abs() < 0.02, "{z}");
    let thumb = d.rect("program.zoomBar");
    assert!((thumb[2] - track[2] * num(&z[1])).abs() < 1.0, "thumb {thumb:?} for {z}");
    // the ticks follow the zoom: still on whole frames, no further apart in time, and spread
    // further apart on screen than the same step was before
    let zoomed = tick_frames(&ruler_ticks(&d, bar), bar, (0.0, num(&z[1])), frames);
    let (full_step, zoomed_step) = (tick_step(&full), tick_step(&zoomed));
    assert!(zoomed_step <= full_step, "tick steps {full_step} → {zoomed_step} frames");
    let (before, after) = (full_step as f64 / frames * bar[2], zoomed_step as f64 / (frames * num(&z[1])) * bar[2]);
    assert!(after >= 12.0 - 0.01, "minor ticks at least 12 pt apart: {after}");
    if zoomed_step == full_step {
        assert!(after > before * 1.5, "same step, spread out: {before} → {after} pt");
    }

    // a click halfway along the ruler is halfway through the half shown, and the playhead is drawn there
    d.ok("ui.click", json!({"x": bar[0] + bar[2] / 2.0, "y": bar[1] + bar[3] / 2.0}));
    d.frames(2);
    let playhead = num(&d.exec("sequence.inspect", json!({}))["playhead"]);
    let want = duration * num(&z[1]) / 2.0;
    assert!((playhead - want).abs() < duration * 0.01, "playhead {playhead} for {want}");
    let head = d.rect("program.playhead");
    let at = bar[0] + bar[2] * (playhead / (duration * num(&z[1])));
    assert!((head[0] + head[2] / 2.0 - at).abs() < 1.0, "playhead drawn at {head:?}, want x {at}");

    // the thumb's middle dragged right scrolls the ruler without zooming; the playhead goes out of view
    let (x, shift) = (thumb[0] + thumb[2] / 2.0, track[2] * 0.3);
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + shift, "y": y}, "steps": 8}));
    d.frames(2);
    let z2 = d.program()["time_zoom"].clone();
    assert!((num(&z2[0]) - 0.3).abs() < 0.02 && (num(&z2[1]) - num(&z2[0]) - num(&z[1])).abs() < 1e-9, "{z2}");
    assert!(d.try_rect("program.playhead").is_none(), "a playhead off the zoomed ruler is not drawn");
    let source = d.ok("ui.inspect", json!({}))["ui"]["source"]["time_zoom"].clone();
    assert!(source.is_null(), "the Source monitor keeps its own range: {source}");

    // an unusable range from ui.set shows all of it
    d.ok("ui.set", json!({"program": {"time_zoom": [0.9, 0.1]}}));
    d.frames(2);
    assert!((d.rect("program.zoomBar")[2] - track[2]).abs() < 0.5);
}

/// The Program monitor's zoom bar with the middle half of the duration shown: the thumb sits
/// inside the track. Returns the thumb (x, y, width, height), the track, and the y of the bar's middle.
fn program_zoom_bar(d: &mut Driver) -> ([f64; 4], [f64; 4], f64) {
    d.ok("ui.set", json!({"program": {"time_zoom": [0.25, 0.75]}}));
    d.frames(3);
    let (thumb, track) = (d.rect("program.zoomBar"), d.rect("program.zoomBar.track"));
    (thumb, track, thumb[1] + thumb[3] / 2.0)
}

/// The monitor zoom bar's circular end handles zoom, so the pointer over one shows the horizontal
/// resize arrow; the middle of the thumb scrolls and the bar beside it jumps, which keep the normal
/// pointer.
#[test]
fn time_zoom_bar_handles_show_the_resize_cursor_on_hover() {
    let mut d = Driver::demo();
    let (thumb, track, y) = program_zoom_bar(&mut d);
    d.move_to(thumb[0] + 3.0, y);
    assert_eq!(d.cursor(), egui::CursorIcon::ResizeHorizontal, "left handle");
    d.move_to(thumb[0] + thumb[2] - 3.0, y);
    assert_eq!(d.cursor(), egui::CursorIcon::ResizeHorizontal, "right handle");
    d.move_to(thumb[0] + thumb[2] / 2.0, y);
    assert_eq!(d.cursor(), egui::CursorIcon::Default, "middle of the thumb");
    d.move_to(track[0] + 10.0, y);
    assert_eq!(d.cursor(), egui::CursorIcon::Default, "the bar beside the thumb");
}

/// A handle drag goes on whatever the pointer is over, so the arrow stays while the pointer is off
/// the bar, and goes when the button is released.
#[test]
fn time_zoom_bar_handle_drag_keeps_the_resize_cursor_off_the_bar() {
    let mut d = Driver::demo();
    let (thumb, _, y) = program_zoom_bar(&mut d);
    let handle = thumb[0] + thumb[2] - 3.0;
    d.press(handle, y);
    d.move_to(handle + 40.0, y);
    let z = d.program()["time_zoom"].clone();
    assert!(z[1].as_f64().is_some_and(|end| end > 0.75), "the right handle dragged right zooms out: {z}");
    assert_eq!(d.cursor(), egui::CursorIcon::ResizeHorizontal, "dragging along the bar");
    d.move_to(handle + 40.0, y - 300.0);
    assert_eq!(d.cursor(), egui::CursorIcon::ResizeHorizontal, "dragging above the bar");
    d.release(handle + 40.0, y - 300.0);
    assert_ne!(d.cursor(), egui::CursorIcon::ResizeHorizontal, "button released, pointer off the bar");
}

/// Dragging the thumb by its middle scrolls: with the thumb stopped at the end of the bar the
/// pointer can sit where a handle is, and must not show the zoom arrow.
#[test]
fn time_zoom_bar_middle_drag_never_shows_the_resize_cursor() {
    let mut d = Driver::demo();
    let (thumb, track, y) = program_zoom_bar(&mut d);
    d.press(thumb[0] + thumb[2] / 2.0, y);
    d.move_to(thumb[0] + thumb[2] / 2.0 + 60.0, y);
    d.move_to(track[0] + track[2] - 3.0, y);
    let end = d.rect("program.zoomBar");
    assert!((end[0] + end[2] - (track[0] + track[2])).abs() < 0.5, "the thumb is at the end of the bar: {end:?}");
    assert_eq!(d.cursor(), egui::CursorIcon::Default, "pointer over the right handle's place");
    d.release(track[0] + track[2] - 3.0, y);
}

/// While a monitor plays, its zoomed ruler pages to keep the playhead in view, keeping its width,
/// as the Timeline follows the playhead; stopped, or with auto-scrolling off, it stays put.
#[test]
fn zoomed_monitor_ruler_follows_the_playhead_while_playing() {
    let mut d = Driver::demo();
    let duration = num(&d.exec("sequence.inspect", json!({}))["duration"]);
    d.ok("ui.set", json!({"program": {"time_zoom": [0.0, 0.25]}}));
    // a playhead moved past the range while stopped leaves it alone
    d.exec("playhead.set", json!({"time": (duration * 0.6) as i64}));
    d.frames(2);
    assert_eq!(d.program()["time_zoom"], json!([0.0, 0.25]));
    // playing, the range pages forward: the playhead near its left edge
    d.menu("playback.toggle");
    d.menu("playback.toggle");
    let z = d.program()["time_zoom"].clone();
    let f = num(&d.exec("sequence.inspect", json!({}))["playhead"]) / duration;
    let (s, e) = (num(&z[0]), num(&z[1]));
    assert!(s <= f && f <= e && (e - s - 0.25).abs() < 1e-9, "playhead at {f} in {z}");
    assert!((f - s - 0.25 * 0.05).abs() < 0.01, "paged to near the left edge: {f} in {z}");
    // and back when the playhead is before it
    d.exec("playhead.set", json!({"time": (duration * 0.1) as i64}));
    d.menu("playback.toggle");
    d.menu("playback.toggle");
    let z = d.program()["time_zoom"].clone();
    assert!(num(&z[0]) < 0.1 && num(&z[1]) > 0.1, "{z}");

    // with auto-scrolling off the range stays put
    d.exec("prefs.set", json!({"key": "timeline.autoScroll", "value": "noScroll"}));
    d.ok("ui.set", json!({"program": {"time_zoom": [0.0, 0.25]}}));
    d.exec("playhead.set", json!({"time": (duration * 0.6) as i64}));
    d.menu("playback.toggle");
    d.menu("playback.toggle");
    assert_eq!(d.program()["time_zoom"], json!([0.0, 0.25]));
    d.exec("prefs.set", json!({"key": "timeline.autoScroll", "value": "pageScroll"}));

    // the Source monitor follows its own playback
    let project = d.exec("project.inspect", json!({}));
    let item = first_media_item(&project).expect("media item");
    let source_duration = item_duration(&project, item).expect("media duration");
    d.exec("source.open", json!({"item": item}));
    d.ok("ui.set", json!({"source": {"time_zoom": [0.0, 0.25]}}));
    d.exec("source.setPlayhead", json!({"time": (source_duration * 0.6) as i64}));
    d.ok("ui.menu.invoke", json!({"id": "playback.toggle", "params": {"monitor": "source"}}));
    d.frames(2);
    d.ok("ui.menu.invoke", json!({"id": "playback.toggle", "params": {"monitor": "source"}}));
    let z = d.ok("ui.inspect", json!({}))["ui"]["source"]["time_zoom"].clone();
    assert!(num(&z[0]) > 0.5 && (num(&z[1]) - num(&z[0]) - 0.25).abs() < 1e-9, "{z}");
}

/// The Source monitor's waveform shows the part of the clip its zoomed time ruler shows, so
/// zooming in spreads the waveform out (#1782): a click halfway across it is halfway through
/// that part, not halfway through the clip.
#[test]
fn source_waveform_follows_the_time_zoom() {
    let mut d = Driver::demo();
    let project = d.exec("project.inspect", json!({}));
    let item = first_media_item(&project).expect("media item");
    let duration = item_duration(&project, item).expect("media duration");
    d.exec("source.open", json!({"item": item}));
    d.menu("view.display.audioWaveform");
    d.ok("ui.set", json!({"source": {"time_zoom": [0.5, 0.75]}}));
    d.frames(3);
    let w = d.rect("source.waveform");
    d.ok("ui.click", json!({"x": w[0] + w[2] / 2.0, "y": w[1] + w[3] / 2.0}));
    d.frames(2);
    let playhead = d.harness.state().session.state.source_playhead.0 as f64;
    let want = duration * 0.625;
    assert!((playhead - want).abs() < duration * 0.01, "source playhead {playhead}, want {want} of {duration}");
}

#[test]
fn magnification_pans_and_guides_from_rulers() {
    let mut d = Driver::demo();
    // zoom dropdown → 200%
    d.click("program.zoom");
    d.click("program.zoom.200");
    assert_eq!(d.program()["zoom"], 2.0);
    let pic = d.rect("program.picture");
    // pan with the Hand tool
    d.ok("ui.set", json!({"tool": "Hand"}));
    let (cx, cy) = (pic[0] + pic[2] / 2.0, pic[1] + pic[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx + 80.0, "y": cy + 30.0}, "steps": 6}));
    d.frames(2);
    let pan = d.program()["pan"].clone();
    assert!(pan[0].as_f64().unwrap() > 20.0, "panned right: {pan}");
    d.shot("magnified");
    d.menu("view.magnification.fit");
    let p = d.program();
    assert!(p["zoom"].is_null() && p["pan"] == json!([0.0, 0.0]));
    d.ok("ui.set", json!({"tool": "Selection"}));

    // rulers, then drag a guide out of the left ruler (vertical guide)
    d.menu("view.showRulers");
    d.frames(2);
    let left = d.rect("program.ruler.left");
    let pic = d.rect("program.picture");
    let x = pic[0] + pic[2] * 0.25;
    d.ok("ui.drag", json!({"from": {"x": left[0] + left[2] / 2.0, "y": pic[1] + pic[3] / 2.0}, "to": {"x": x, "y": pic[1] + pic[3] / 2.0}, "steps": 6}));
    d.frames(2);
    let g = d.program()["guides"].clone();
    assert_eq!(g.as_array().unwrap().len(), 1, "{g}");
    assert_eq!(g[0]["vertical"], true);
    let pos = g[0]["position"].as_f64().unwrap();
    assert!((pos - 480.0).abs() < 8.0, "a quarter across a 1920 frame: {pos}");
    // and a horizontal one from the top ruler
    let top = d.rect("program.ruler.top");
    d.ok(
        "ui.drag",
        json!({"from": {"x": pic[0] + pic[2] / 2.0, "y": top[1] + top[3] / 2.0}, "to": {"x": pic[0] + pic[2] / 2.0, "y": pic[1] + pic[3] * 0.5}, "steps": 6}),
    );
    d.frames(2);
    assert_eq!(d.program()["guides"].as_array().unwrap().len(), 2);
    // move the vertical guide
    let gr = d.rect("program.guide.0");
    let (gx, gy) = (gr[0] + gr[2] / 2.0, gr[1] + gr[3] * 0.2);
    d.ok("ui.drag", json!({"from": {"x": gx, "y": gy}, "to": {"x": gx + 40.0, "y": gy}, "steps": 6}));
    d.frames(2);
    let moved = d.program()["guides"][0]["position"].as_f64().unwrap();
    assert!(moved > pos + 50.0, "guide moved: {pos} → {moved}");
    d.shot("rulers-guides");
    // lock: dragging no longer moves it
    d.menu("view.lockGuides");
    let gr = d.rect("program.guide.0");
    let (gx, gy) = (gr[0] + gr[2] / 2.0, gr[1] + gr[3] * 0.2);
    d.ok("ui.drag", json!({"from": {"x": gx, "y": gy}, "to": {"x": gx + 40.0, "y": gy}, "steps": 6}));
    d.frames(2);
    assert_eq!(d.program()["guides"][0]["position"].as_f64().unwrap(), moved);
    d.menu("view.lockGuides");

    // templates live in the preferences
    d.ok("ui.menu.invoke", json!({"id": "view.guideTemplates.save", "params": {"name": "Two lines"}}));
    let prefs = d.exec("prefs.get", json!({"key": "guides.templates"}));
    assert_eq!(prefs[0]["name"], "Two lines");
    assert_eq!(prefs[0]["guides"].as_array().unwrap().len(), 2);
    d.menu("view.clearGuides");
    assert!(d.program()["guides"].as_array().unwrap().is_empty());
    d.menu("view.guideTemplates.manage");
    d.click("guides.manage.row.0");
    d.click("guides.manage.apply");
    assert_eq!(d.program()["guides"].as_array().unwrap().len(), 2, "template applied");
    assert!(d.ok("ui.inspect", json!({}))["ui"]["guide_dialog"].is_null(), "dialog closed");

    // Add Guide… dialog
    d.menu("view.addGuide");
    d.click("guides.add.horizontal");
    d.click("guides.add.ok");
    let g = d.program()["guides"].clone();
    assert_eq!(g.as_array().unwrap().len(), 3);
    assert_eq!(g[2]["vertical"], false);
    // dragging a guide out of the picture removes it
    let gr = d.rect("program.guide.0");
    let left = d.rect("program.ruler.left");
    d.ok("ui.drag", json!({"from": {"x": gr[0] + gr[2] / 2.0, "y": gr[1] + gr[3] * 0.2}, "to": {"x": left[0] + 4.0, "y": gr[1] + gr[3] * 0.2}, "steps": 6}));
    d.frames(2);
    assert_eq!(d.program()["guides"].as_array().unwrap().len(), 2, "guide dropped on the ruler is removed");
}

#[test]
fn snap_in_program_monitor_and_graphics_menu() {
    let mut d = Driver::demo();
    d.exec("playhead.set", json!({"seconds": 2}));
    let r = d.exec("graphics.newRectangle", json!({"position": [880, 300], "size": [200, 100]}));
    let clip = r["clip"].as_u64().unwrap();
    d.frames(4);
    let drag_by = |d: &mut Driver, frame_dx: f64, extra: f64| {
        let pic = d.rect("program.picture");
        let k = pic[2] / 1920.0;
        let lr = d.rect(&format!("program.layer.{clip}.0"));
        // off the centre: there the anchor point is picked, and dragging it moves it alone
        let (x, y) = (lr[0] + lr[2] / 4.0, lr[1] + lr[3] / 2.0);
        d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + frame_dx * k + extra, "y": y}, "steps": 8}));
        d.frames(2);
    };
    let pos_x = |d: &mut Driver| d.exec("graphics.list", json!({"clip": clip}))["layers"][0]["position"][0].as_f64().unwrap();
    // centre 880 → near 960 (+3 screen px): snaps onto the frame centre
    drag_by(&mut d, 80.0, 3.0);
    assert!((pos_x(&mut d) - 960.0).abs() < 0.01, "snapped to the centre: {}", pos_x(&mut d));
    // a guide at x = 1300: the right edge (centre + 100) snaps to it
    d.ok("ui.menu.invoke", json!({"id": "view.addGuide", "params": {"orientation": "vertical", "position": 1300}}));
    drag_by(&mut d, 240.0, -4.0);
    assert!((pos_x(&mut d) - 1200.0).abs() < 0.01, "right edge on the guide: {}", pos_x(&mut d));
    d.shot("snap");
    // snapping off: the same drag lands off the guide
    d.menu("view.snapInProgramMonitor");
    assert_eq!(d.program()["snap"], false);
    drag_by(&mut d, -240.0, 4.0);
    assert!((pos_x(&mut d) - 960.0).abs() > 1.0, "free move: {}", pos_x(&mut d));

    // Graphics menu: New Layer ▸ Ellipse, Align to Video Frame, Arrange
    d.exec("graphics.newEllipse", json!({"clip": clip, "position": [400, 400], "size": [100, 100]}));
    d.menu("graphics.alignFrame.left");
    let l = d.exec("graphics.list", json!({"clip": clip}));
    assert!(l["layers"][1]["quad"][0][0].as_f64().unwrap().abs() < 0.01, "ellipse on the left edge: {l}");
    d.menu("graphics.sendToBack");
    let l = d.exec("graphics.list", json!({"clip": clip}));
    assert_eq!(l["layers"][0]["kind"], "Ellipse");
    let menu = d.ok("ui.menu.list", json!({}));
    let shortcut = menu.as_array().unwrap().iter().find(|m| m["id"] == "graphics.bringToFront").unwrap()["shortcut"].clone();
    assert_eq!(shortcut, "Cmd+Shift+]");
    d.shot("graphics-menu");
}

#[test]
fn snap_in_program_monitor_snaps_video_clip_motion_drags() {
    let mut d = Driver::demo();
    let seq = d.exec("sequence.inspect", json!({}));
    let c = &seq["video"][0]["items"][0];
    let (clip, start, dur) = (c["clip"].as_u64().unwrap(), c["start"].as_i64().unwrap(), c["duration"].as_i64().unwrap());
    d.exec("playhead.set", json!({"time": start + dur / 2}));
    d.exec("timeline.select", json!({"clips": [clip]}));
    // half size, centred 80 frame pixels left of the frame centre
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "scale", "value": 50.0}));
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "position", "value": [880.0, 540.0]}));
    d.frames(3);
    d.exec("effects.select", json!({"clip": clip, "effect": "motion"}));
    d.frames(3);
    let pos_x = |d: &mut Driver| {
        let app = d.harness.state();
        let it = app.session.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1;
        let mt = it.source_time_at(app.session.playhead());
        it.effect("motion").unwrap().param("position").unwrap().vec2_at(mt).x
    };
    let drag = |d: &mut Driver, frame_dx: f64, extra: f64| {
        let pic = d.rect("program.picture");
        let k = pic[2] / 1920.0;
        let b = d.rect("program.transform.box");
        // off the anchor point at the centre
        let (x, y) = (b[0] + b[2] * 0.3, b[1] + b[3] * 0.3);
        d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + frame_dx * k + extra, "y": y}, "steps": 8}));
        d.frames(2);
    };
    // the box centre goes 80 frame pixels + 3 screen pixels towards the frame centre: it snaps onto it
    drag(&mut d, 80.0, 3.0);
    assert!((pos_x(&mut d) - 960.0).abs() < 0.01, "snapped to the frame centre: {}", pos_x(&mut d));
    // snapping off: the same kind of drag lands off the centre
    d.menu("view.snapInProgramMonitor");
    assert_eq!(d.program()["snap"], false);
    // back to the start, then the same drag towards the centre: it moves as far as the pointer did
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "position", "value": [880.0, 540.0]}));
    d.frames(3);
    let k = d.rect("program.picture")[2] / 1920.0;
    drag(&mut d, 80.0, 3.0);
    let free = pos_x(&mut d);
    assert!((free - (960.0 + 3.0 / k)).abs() < 1.5 && (free - 960.0).abs() > 0.5, "free move near the centre: {free}");
}

#[test]
fn snap_in_program_monitor_follows_the_modifier_with_the_pointer_still() {
    let mut d = Driver::demo();
    let seq = d.exec("sequence.inspect", json!({}));
    let c = &seq["video"][0]["items"][0];
    let (clip, start, dur) = (c["clip"].as_u64().unwrap(), c["start"].as_i64().unwrap(), c["duration"].as_i64().unwrap());
    d.exec("playhead.set", json!({"time": start + dur / 2}));
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "scale", "value": 50.0}));
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "position", "value": [880.0, 540.0]}));
    d.frames(3);
    d.exec("effects.select", json!({"clip": clip, "effect": "motion"}));
    d.frames(3);
    let pos_x = |d: &mut Driver| {
        let app = d.harness.state();
        let it = app.session.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1;
        let mt = it.source_time_at(app.session.playhead());
        it.effect("motion").unwrap().param("position").unwrap().vec2_at(mt).x
    };
    let pic = d.rect("program.picture");
    let k = pic[2] / 1920.0;
    let b = d.rect("program.transform.box");
    let (x, y) = (b[0] + b[2] * 0.3, b[1] + b[3] * 0.3);
    let at = |dx: f32| egui::pos2(x as f32 + dx, y as f32);
    let push = |d: &mut Driver, e: egui::Event| d.harness.input_mut().events.push(e);
    let button =
        |pressed: bool, pos: egui::Pos2| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
    push(&mut d, egui::Event::PointerMoved(at(0.0)));
    d.frames(1);
    push(&mut d, button(true, at(0.0)));
    d.frames(1);
    let end = (80.0 * k + 3.0) as f32;
    for i in 1..=8 {
        push(&mut d, egui::Event::PointerMoved(at(end * i as f32 / 8.0)));
        d.frames(1);
    }
    assert!((pos_x(&mut d) - 960.0).abs() < 0.01, "snapped to the centre: {}", pos_x(&mut d));
    // ⌘/Ctrl goes down with the pointer still: the position is free again
    push(&mut d, egui::Event::ModifiersChanged(egui::Modifiers::COMMAND));
    d.frames(2);
    assert!((pos_x(&mut d) - 960.0).abs() > 0.5, "free with the modifier held: {}", pos_x(&mut d));
    // and released again: snapped
    push(&mut d, egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    d.frames(2);
    assert!((pos_x(&mut d) - 960.0).abs() < 0.01, "snapped again: {}", pos_x(&mut d));
    push(&mut d, button(false, at(end)));
    d.frames(3);
    assert!((pos_x(&mut d) - 960.0).abs() < 0.01, "released snapped: {}", pos_x(&mut d));
}

#[test]
fn elements_wait_for_the_timeline_zoom_to_settle() {
    let mut d = Driver::demo();
    let project = d.exec("project.inspect", json!({}));
    let item = first_media_item(&project).expect("a movie in the demo project");
    // opening a sequence fits the timeline with an animated zoom: rects read right away must
    // already be the final ones
    d.exec("file.newSequence", json!({"name": "Zoom", "fromItem": item}));
    let clip = d.exec("sequence.inspect", json!({}))["video"][0]["items"][0]["clip"].as_u64().expect("clip");
    let id = format!("timeline.clip.{clip}");
    let first = d.rect(&id);
    d.frames(120);
    let settled = d.rect(&id);
    for k in 0..4 {
        assert!((first[k] - settled[k]).abs() < 0.5, "rect read while zooming {first:?}, settled {settled:?}");
    }
}

/// Frames until `ui.elements` answers (it retries while the timeline is still zooming).
fn elements_latency(d: &mut Driver) -> usize {
    let (req, reply) = ControlRequest::new("ui.elements", json!({"prefix": "timeline."}));
    d.tx.send(req).unwrap();
    for n in 1..=600 {
        d.frames(1);
        if let Ok(v) = reply.try_recv() {
            assert_eq!(v["ok"], true, "{v}");
            return n;
        }
    }
    panic!("ui.elements never answered");
}

#[test]
fn elements_answer_promptly_once_the_timeline_is_at_rest() {
    let mut d = Driver::demo();
    d.frames(120);
    // a timeline at rest must not keep `ui.elements` waiting (the animation snaps to its target)
    assert!(elements_latency(&mut d) <= 3);
    for pps in [40.0, 400.0, 7.5] {
        d.ok("ui.set", json!({"timeline": {"pps": pps}}));
        d.ok("ui.set", json!({"timeline": {"scroll": 1.5}}));
        d.frames(120);
        let n = elements_latency(&mut d);
        assert!(n <= 3, "ui.elements took {n} frames at rest after zooming to {pps} px/s");
    }
}

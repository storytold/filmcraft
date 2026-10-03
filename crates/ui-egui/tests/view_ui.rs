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
        let mut d = Driver { harness, tx, shots };
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
        let (x, y) = (lr[0] + lr[2] / 2.0, lr[1] + lr[3] / 2.0);
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

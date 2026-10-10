//! Graphics UI, driven headless through the control channel (like `scripted.rs`): the Type tool
//! creates a text layer on the Program monitor, typing and keys edit it, the Properties panel's
//! controls style it, the Selection tool moves it.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the test also renders the UI with wgpu (works without a
//! window or an unlocked screen) and writes PNGs there for review.

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
    fn rect(&mut self, id: &str) -> [f64; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}: {v}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        [r[0], r[1], r[2], r[3]]
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(8);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
}

#[test]
fn type_tool_edit_style_and_move() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.exec("playhead.set", json!({"seconds": 2}));
    d.ok("ui.set", json!({"tool": "Type"}));
    d.frames(3);
    let pic = d.rect("program.picture");
    let (px, py) = (pic[0] + pic[2] * 0.2, pic[1] + pic[3] * 0.35);
    d.ok("ui.click", json!({"x": px, "y": py}));
    d.ok("ui.type", json!({"text": "Night Drive"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.ok("ui.type", json!({"text": "through the city"}));
    let list = d.exec("graphics.list", json!({}));
    assert_eq!(list["layers"][0]["text"], "Night Drive\nthrough the city", "{list}");
    // caret editing: Backspace ×4, select the word "Drive" with Shift+arrows… then retype
    for _ in 0..4 {
        d.ok("ui.key", json!({"key": "Backspace"}));
    }
    d.ok("ui.type", json!({"text": "town"}));
    d.ok("ui.key", json!({"key": "ArrowUp"}));
    d.ok("ui.key", json!({"key": "End"}));
    for _ in 0..5 {
        d.ok("ui.key", json!({"key": "Shift+ArrowLeft"}));
    }
    d.ok("ui.type", json!({"text": "Ride"}));
    let list = d.exec("graphics.list", json!({}));
    assert_eq!(list["layers"][0]["text"], "Night Ride\nthrough the town", "{list}");
    // one undo step for the whole typing session (after "New Graphic")
    let hist = d.exec("history.list", json!({}));
    let undo: Vec<&str> = hist["undo"].as_array().unwrap().iter().filter_map(Value::as_str).collect();
    assert_eq!(&undo[undo.len() - 2..], ["New Graphic", "Edit Text"], "{undo:?}");
    d.shot("type-tool-editing");

    // style through the Properties panel controls
    d.ok("ui.key", json!({"key": "Escape"}));
    d.ok("ui.set", json!({"tool": "Selection"}));
    d.ok("ui.click", json!({"id": "graphics.prop.faux_bold"}));
    d.ok("ui.click", json!({"id": "graphics.prop.align.1"}));
    d.ok("ui.click", json!({"id": "graphics.prop.stroke"}));
    d.ok("ui.click", json!({"id": "graphics.prop.shadow"}));
    let clip = list["clip"].as_u64().unwrap();
    d.exec("graphics.set", json!({"clip": clip, "props": {"font_style": "Bold", "fill_color": "#ffd23f", "size": 120}}));
    let seq = d.exec("sequence.inspect", json!({}));
    let s = seq.to_string();
    assert!(s.contains("graphic_text"), "graphic layer in the sequence");
    let list2 = d.exec("graphics.list", json!({"clip": clip}));
    let q0 = list2["layers"][0]["quad"][0][0].as_f64().unwrap();
    // selection-tool drag moves the layer
    let lr = d.rect(&format!("program.layer.{clip}.0"));
    let pic = d.rect("program.picture");
    // a point on the layer that is inside the picture
    let x0 = lr[0].max(pic[0]);
    let x1 = (lr[0] + lr[2]).min(pic[0] + pic[2]);
    let (cx, cy) = ((x0 + x1) / 2.0, lr[1] + lr[3] * 0.3);
    let from = json!({"x": cx, "y": cy});
    let to = json!({"x": cx + 60.0, "y": cy + 20.0});
    d.ok("ui.drag", json!({"from": from, "to": to, "steps": 6}));
    let list3 = d.exec("graphics.list", json!({"clip": clip}));
    let q1 = list3["layers"][0]["quad"][0][0].as_f64().unwrap();
    assert!(q1 > q0 + 20.0, "moved right: {q0} → {q1}");
    d.shot("properties-styled");

    // shapes: rectangle tool drag
    d.ok("ui.set", json!({"tool": "Rectangle"}));
    let pic = d.rect("program.picture");
    d.ok("ui.drag", json!({"from": {"x": pic[0] + pic[2] * 0.15, "y": pic[1] + pic[3] * 0.75}, "to": {"x": pic[0] + pic[2] * 0.85, "y": pic[1] + pic[3] * 0.9}, "steps": 5}));
    let list4 = d.exec("graphics.list", json!({"clip": clip}));
    assert_eq!(list4["layers"].as_array().unwrap().len(), 2, "{list4}");
    assert_eq!(list4["layers"][1]["kind"], "Rectangle");
    d.exec("graphics.arrangeLayer", json!({"clip": clip, "layer": 1, "to": "back"}));
    d.ok("ui.set", json!({"tool": "Selection"}));
    d.shot("shape-layer");
}

/// The value of `param` on the text layer of graphic clip `clip`, at its first frame.
fn text_param(d: &mut Driver, clip: u64, param: &str) -> Option<filmcraft_project::ParamValue> {
    let s = &d.harness.state().session;
    let (_, it) = s.active_sequence()?.find_item(filmcraft_project::ClipId(clip))?;
    let e = it.effects.iter().find(|e| e.effect == "graphic_text")?;
    Some(e.param(param)?.value_at(filmcraft_time::Tick::ZERO))
}

/// A drag of a property in the Properties panel is one undo step, so one Cmd+Z restores the
/// original value (a drag was committing one undo step per frame, so undo only stepped back one
/// frame's worth; the shadow color picker showed it as "sometimes undo works, sometimes not").
#[test]
fn a_property_drag_is_one_undo_step() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let r = d.exec("graphics.newText", json!({"text": "Title"}));
    let clip = r["clip"].as_u64().unwrap();
    d.exec("graphics.selectLayer", json!({"clip": clip, "layers": [0]}));
    d.frames(4);
    // the layer's Opacity number (visible in the Align and Transform section)
    d.exec("graphics.set", json!({"clip": clip, "layer": 0, "props": {"opacity": 50}}));
    d.frames(2);
    let opacity = |d: &mut Driver| -> f64 { text_param(d, clip, "opacity").and_then(|v| v.as_f64()).unwrap() };
    let before = opacity(&mut d);
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    // drag the Opacity number to the left: several move frames, one undo step
    let n = d.rect("graphics.prop.opacity");
    let (cx, cy) = (n[0] + n[2] / 2.0, n[1] + n[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx - 60.0, "y": cy}, "steps": 6}));
    d.frames(4);
    let after = opacity(&mut d);
    assert!(after < before - 5.0, "the drag changed the opacity: {before} -> {after}");
    let undo1 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    assert_eq!(undo1, undo0 + 1, "a property drag is one undo step");
    d.exec("edit.undo", json!({}));
    assert_eq!(opacity(&mut d), before, "one undo restores the original value");
}

/// The shadow color picker popup: a drag in the 2D picker is one undo step, so one Cmd+Z
/// restores the original color (a drag was committing one undo step per frame, so undo only
/// stepped back one frame's worth — the reported "sometimes undo works, sometimes not").
#[test]
fn shadow_color_drag_is_one_undo_step() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let r = d.exec("graphics.newText", json!({"text": "Title"}));
    let clip = r["clip"].as_u64().unwrap();
    d.exec("graphics.selectLayer", json!({"clip": clip, "layers": [0]}));
    d.frames(4);
    let color = |d: &mut Driver| text_param(d, clip, "shadow_color").and_then(|v| v.as_color());
    // scroll the properties panel so the shadow row is visible, then open the picker
    d.ok("ui.scroll", json!({"x": 1400.0, "y": 500.0, "dx": 0.0, "dy": -400.0}));
    d.frames(3);
    let sw = d.rect("graphics.prop.shadow_color");
    d.ok("ui.click", json!({"x": sw[0] + sw[2] / 2.0, "y": sw[1] + sw[3] / 2.0}));
    d.frames(3);
    // the popup flips above the swatch (it does not fit below); find a point in its 2D picker
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    let mut hit = None;
    for (dx, dy) in [(30.0, 30.0), (150.0, 30.0), (30.0, 150.0), (150.0, 150.0)] {
        let (px, py) = (sw[0] + dx, sw[1] - dy);
        let before = color(&mut d);
        d.ok("ui.drag", json!({"from": {"x": px, "y": py}, "to": {"x": px + 60.0, "y": py + 40.0}, "steps": 4}));
        d.frames(3);
        let after = color(&mut d);
        if after != before {
            hit = Some((before, after));
            break;
        }
    }
    let (before, after) = hit.expect("no drag point hit the color picker");
    assert_ne!(after, before, "the drag changed the shadow color");
    let undo1 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    assert_eq!(undo1, undo0 + 1, "a color drag is one undo step");
    d.exec("edit.undo", json!({}));
    assert_eq!(color(&mut d), before, "one undo restores the original color");
}

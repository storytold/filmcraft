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

/// Character ranges use setCharStyle, but must share the same one-gesture undo rule as a layer.
#[test]
fn selected_character_size_drag_is_one_undo_step() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace":"Captions and Graphics","tool":"Type"}));
    d.exec("playhead.set", json!({"seconds":2}));
    d.frames(3);
    let p = d.rect("program.picture");
    d.ok("ui.click", json!({"x":p[0]+p[2]*0.2,"y":p[1]+p[3]*0.35}));
    d.ok("ui.type", json!({"text":"Undo drag test"}));
    d.ok("ui.key", json!({"key":"Cmd+A"}));
    d.frames(3);
    let before = d.harness.state().session.project.clone();
    let n = d.harness.state().session.history.undo.len();
    let r = d.rect("graphics.prop.size");
    d.ok("ui.drag", json!({"from":{"x":r[0]+r[2]/2.0,"y":r[1]+r[3]/2.0},"to":{"x":r[0]+r[2]/2.0+80.0,"y":r[1]+r[3]/2.0},"steps":8}));
    d.frames(3);
    assert_eq!(d.harness.state().session.history.undo.len(), n + 1);
    let after = d.harness.state().session.project.clone();
    assert_ne!(*after, *before);
    d.exec("edit.undo", json!({}));
    assert_eq!(*d.harness.state().session.project, *before);
    d.exec("edit.redo", json!({}));
    assert_eq!(*d.harness.state().session.project, *after);
}

/// The Properties panel's stopwatches animate a layer property (#2358): Tracking (letter
/// spacing) gets keyframes from the Essential Graphics panel itself, a change at another time
/// adds a keyframe there instead of changing the value everywhere, and the stopwatch turns
/// keyframing off again.
#[test]
fn the_tracking_stopwatch_animates_letter_spacing() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.exec("playhead.set", json!({"seconds": 0.0}));
    let r = d.exec("graphics.newText", json!({"text": "Title", "seconds": 4}));
    let clip = r["clip"].as_u64().unwrap();
    d.exec("graphics.selectLayer", json!({"clip": clip, "layers": [0]}));
    d.frames(4);
    for _ in 0..20 {
        if d.rect("graphics.prop.tracking")[1] < 800.0 {
            break;
        }
        d.ok("ui.scroll", json!({"id": "graphics.prop.scale", "dy": -200.0}));
        d.frames(2);
    }
    let keys = |d: &mut Driver| -> Vec<(i64, f64)> {
        let s = &d.harness.state().session;
        let (_, it) = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap();
        let e = it.effects.iter().find(|e| e.effect == "graphic_text").unwrap();
        e.params.get("tracking").map(|p| p.keyframes.iter().map(|k| (k.time.0, k.value.as_f64().unwrap())).collect()).unwrap_or_default()
    };
    assert!(keys(&mut d).is_empty());
    d.ok("ui.click", json!({"id": "graphics.prop.tracking.stopwatch"}));
    d.frames(2);
    assert_eq!(keys(&mut d).len(), 1, "the stopwatch sets a keyframe at the playhead");
    // two seconds later, drag the Tracking number: a second keyframe, the first stays
    d.exec("playhead.set", json!({"seconds": 2.0}));
    d.frames(2);
    let n = d.rect("graphics.prop.tracking");
    let (cx, cy) = (n[0] + n[2] / 2.0, n[1] + n[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx + 60.0, "y": cy}, "steps": 6}));
    d.frames(4);
    let k = keys(&mut d);
    assert_eq!(k.len(), 2, "{k:?}");
    // the clip starts at 0, where the first keyframe was set, so the second one sits at the playhead
    let playhead = d.harness.state().session.playhead().0;
    assert_eq!((k[0].0, k[1].0), (0, playhead), "the keyframes are where the playhead was: {k:?}");
    assert_eq!(k[0].1, 0.0, "the first keyframe keeps its value: {k:?}");
    assert!(k[1].1 > 10.0, "the new keyframe has the dragged value: {k:?}");
    // letter spacing now animates: halfway between the keyframes the value is in between
    let mid = {
        let s = &d.harness.state().session;
        let (_, it) = s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap();
        let e = it.effects.iter().find(|e| e.effect == "graphic_text").unwrap();
        e.param("tracking").unwrap().value_at(filmcraft_time::Tick((k[0].0 + k[1].0) / 2)).as_f64().unwrap()
    };
    assert!(mid > 0.0 && mid < k[1].1, "{mid} between 0 and {}", k[1].1);
    d.ok("ui.click", json!({"id": "graphics.prop.tracking.stopwatch"}));
    d.frames(2);
    assert!(keys(&mut d).is_empty(), "the stopwatch turns keyframing off");
    d.shot("tracking-stopwatch");
}

/// Text panel ▸ Graphics (#1487): the text of every text layer in the sequence, in timeline
/// order, searchable; the timecode goes to the clip and selects the layer, and the text edits in
/// place as one undo step.
#[test]
fn the_text_panels_graphics_tab_lists_and_edits_graphic_text() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let mut clips = Vec::new();
    for (sec, text) in [(6.0, "Closing credits"), (1.0, "Opening title")] {
        d.exec("playhead.set", json!({"seconds": sec}));
        let r = d.exec("graphics.newText", json!({"text": text, "seconds": 2}));
        clips.push(r["clip"].as_u64().unwrap());
    }
    let (credits, title) = (clips[0], clips[1]);
    d.exec("playhead.set", json!({"seconds": 10.0}));
    d.ok("ui.click", json!({"id": "text.tab.Graphics"}));
    d.frames(3);
    let rows = |d: &mut Driver| -> Vec<String> {
        let v = d.ok("ui.elements", json!({"prefix": "text.graphics."}));
        v.as_array().unwrap().iter().filter(|e| e["id"].as_str().unwrap().ends_with(".row")).map(|e| e["label"].as_str().unwrap_or("").to_string()).collect()
    };
    assert_eq!(rows(&mut d), ["Opening title", "Closing credits"], "in timeline order");
    // search
    d.ok("ui.click", json!({"id": "text.graphics.search"}));
    d.ok("ui.type", json!({"text": "CRED"}));
    d.frames(3);
    assert_eq!(rows(&mut d), ["Closing credits"]);
    d.ok("ui.key", json!({"key": "A", "command": true}));
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.frames(3);
    assert_eq!(rows(&mut d).len(), 2);
    // the timecode goes to the clip and selects its layer
    d.ok("ui.click", json!({"id": format!("text.graphics.{credits}.0.goto")}));
    d.frames(3);
    let start = d.harness.state().session.active_sequence().unwrap().find_item(filmcraft_project::ClipId(credits)).unwrap().1.start;
    assert_eq!(d.harness.state().session.playhead(), start);
    assert_eq!(d.harness.state().session.state.selection, vec![filmcraft_project::ClipId(credits)]);
    assert_eq!(d.harness.state().session.state.graphic_layers, vec![0]);
    // edit the title's text in place
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    d.ok("ui.click", json!({"id": format!("text.graphics.{title}.0.text")}));
    d.ok("ui.key", json!({"key": "A", "command": true}));
    d.ok("ui.type", json!({"text": "Night Drive"}));
    d.ok("ui.click", json!({"id": "text.graphics.search"}));
    d.frames(3);
    let text = text_param(&mut d, title, "text");
    assert_eq!(text, Some(filmcraft_project::ParamValue::Text("Night Drive".into())));
    assert_eq!(d.exec("history.list", json!({}))["undo"].as_array().unwrap().len(), undo0 + 1, "one undo step");
    assert_eq!(rows(&mut d), ["Night Drive", "Closing credits"]);
    d.shot("text-panel-graphics");
}

/// Keyframed text: the Graphics tab shows a clip's text at the clip's start when the playhead is
/// past the clip, and an edit is written there (not at the clip's last frame), so the value shown
/// is the one that changes.
#[test]
fn a_graphics_tab_edit_writes_keyframed_text_where_it_is_shown() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.exec("playhead.set", json!({"seconds": 1.0}));
    let clip = d.exec("graphics.newText", json!({"text": "Start", "seconds": 2}))["clip"].as_u64().unwrap();
    let it = d.harness.state().session.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.clone();
    // animate the text: "Start" at the clip's start, "End" near its end
    d.exec("effects.toggleAnimation", json!({"clip": clip, "effect": "graphic_text", "param": "text"}));
    let late = it.start.0 + (it.duration.0 * 9) / 10;
    d.exec("graphics.set", json!({"clip": clip, "layer": 0, "props": {"text": "End"}, "time": late}));
    // the playhead past the clip: the row shows the start's text
    d.exec("playhead.set", json!({"seconds": 10.0}));
    d.ok("ui.click", json!({"id": "text.tab.Graphics"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": format!("text.graphics.{clip}.0.text")}));
    d.ok("ui.key", json!({"key": "A", "command": true}));
    d.ok("ui.type", json!({"text": "Edited"}));
    d.ok("ui.click", json!({"id": "text.graphics.search"}));
    d.frames(3);
    let at = |d: &mut Driver, t: i64| {
        let it = d.harness.state().session.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.clone();
        let mt = it.effect_time_at(filmcraft_time::Tick(t));
        it.effects.iter().find(|e| e.effect == "graphic_text").unwrap().param("text").unwrap().value_at(mt)
    };
    assert_eq!(at(&mut d, it.start.0), filmcraft_project::ParamValue::Text("Edited".into()), "the shown (start) value changed");
    assert_eq!(at(&mut d, late), filmcraft_project::ParamValue::Text("End".into()), "the end keeps its own");
}

/// 縦中横 in the Text section: the toggle sits beside Vertical Text, sets the layer's
/// `tate_chu_yoko` property through `graphics.set`, and is disabled for horizontal text.
#[test]
fn tate_chu_yoko_toggle_in_the_text_section() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let r = d.exec("graphics.newText", json!({"text": "12", "vertical": true}));
    let clip = r["clip"].as_u64().unwrap();
    d.exec("graphics.selectLayer", json!({"clip": clip, "layers": [0]}));
    d.frames(4);
    assert_eq!(text_param(&mut d, clip, "vertical").and_then(|v| v.as_bool()), Some(true));
    assert_eq!(text_param(&mut d, clip, "tate_chu_yoko").and_then(|v| v.as_bool()), Some(false), "off by default");
    d.ok("ui.click", json!({"id": "graphics.prop.tate_chu_yoko"}));
    d.frames(2);
    assert_eq!(text_param(&mut d, clip, "tate_chu_yoko").and_then(|v| v.as_bool()), Some(true), "the toggle sets 縦中横");
    d.ok("ui.click", json!({"id": "graphics.prop.tate_chu_yoko"}));
    d.frames(2);
    assert_eq!(text_param(&mut d, clip, "tate_chu_yoko").and_then(|v| v.as_bool()), Some(false), "and clears it");
    // Horizontal text: the toggle is disabled, so a click leaves the property alone.
    let r = d.exec("graphics.newText", json!({"text": "12"}));
    let hclip = r["clip"].as_u64().unwrap();
    d.exec("graphics.selectLayer", json!({"clip": hclip, "layers": [0]}));
    d.frames(4);
    assert_eq!(text_param(&mut d, hclip, "vertical").and_then(|v| v.as_bool()), Some(false));
    let before = text_param(&mut d, hclip, "tate_chu_yoko").and_then(|v| v.as_bool());
    d.ok("ui.click", json!({"id": "graphics.prop.tate_chu_yoko"}));
    d.frames(2);
    assert_eq!(text_param(&mut d, hclip, "tate_chu_yoko").and_then(|v| v.as_bool()), before, "disabled for horizontal text");
}

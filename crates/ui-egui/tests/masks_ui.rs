//! Effect masks in the UI, driven headless through the control channel: create masks from the
//! Effect Controls mask icons, edit them on the Program monitor (vertex, Bézier handle, feather,
//! whole-mask drags), draw a pen mask, toggle Inverted, track a mask; apply and save effect presets,
//! one effect or several selected together.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the test also renders the UI with wgpu and writes PNGs there.

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
    fn center(&mut self, id: &str) -> (f64, f64) {
        let r = self.rect(id);
        (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    }
    fn drag(&mut self, id: &str, dx: f64, dy: f64) {
        let (x, y) = self.center(id);
        self.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + dx, "y": y + dy}, "steps": 6}));
        self.frames(2);
    }
    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().is_some_and(|a| a.iter().any(|e| e["id"] == id))
    }
    fn masks(&mut self) -> Vec<Value> {
        self.exec("masks.list", json!({}))["masks"].as_array().unwrap().clone()
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        // let the background frame workers finish the (blurred) frame
        for _ in 0..40 {
            self.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
}

fn v0(m: &Value, i: usize) -> (f64, f64) {
    let p = &m["path"]["vertices"][i]["p"];
    (p[0].as_f64().unwrap(), p[1].as_f64().unwrap())
}

#[test]
fn create_and_edit_masks_on_the_monitor() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = &seq["video"][0]["items"][0];
    let (id, start, dur) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap(), clip["duration"].as_i64().unwrap());
    d.exec("playhead.set", json!({"time": start + dur / 2}));
    d.exec("timeline.select", json!({"clips": [id]}));
    d.exec("effects.apply", json!({"effect": "gaussian_blur"}));
    d.exec("effects.setParam", json!({"clip": id, "effect": "gaussian_blur", "param": "blurriness", "value": 30.0}));
    d.frames(4);
    // ellipse mask from the Effect Controls icon
    // fold Motion and Time Remapping so the Gaussian Blur rows are on screen
    for fx in ["motion", "time_remap"] {
        d.ok("ui.click", json!({"id": format!("effectControls.effect.{fx}.twirl")}));
        d.frames(2);
    }
    d.ok("ui.click", json!({"id": "effectControls.gaussian_blur.mask.ellipse"}));
    d.frames(3);
    let m = d.masks();
    assert_eq!(m.len(), 1, "{m:?}");
    assert_eq!(m[0]["selected"], true);
    assert_eq!(m[0]["name"], "Mask (1)");
    d.rect("effectControls.gaussian_blur.mask0");
    d.rect("effectControls.gaussian_blur.mask0.feather.stopwatch");
    d.shot("mask-ellipse-selected");

    // drag the top vertex up: one undo step for the gesture
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    let before = v0(&m[0], 0);
    d.drag("program.mask.vertex.0", 0.0, -30.0);
    let m = d.masks();
    let after = v0(&m[0], 0);
    assert!(after.1 < before.1 - 20.0, "vertex moved up: {before:?} → {after:?}");
    assert!((after.0 - before.0).abs() < 1e-6);
    let undo1 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    assert_eq!(undo1, undo0 + 1, "one undo step per drag");

    // Bézier handle of vertex 1 (the right vertex of the ellipse)
    let out_before = m[0]["path"]["vertices"][1]["out"].clone();
    d.drag("program.mask.out.1", 0.0, 25.0);
    let m = d.masks();
    assert_ne!(m[0]["path"]["vertices"][1]["out"], out_before);

    // feather handle outward (up from the top) grows the feather
    let f0 = m[0]["feather"].as_f64().unwrap();
    d.drag("program.mask.feather", 0.0, -20.0);
    let m = d.masks();
    let f1 = m[0]["feather"].as_f64().unwrap();
    assert!(f1 > f0 + 10.0, "feather {f0} → {f1}");
    // expansion handle (bottom) outward = down
    d.drag("program.mask.expansion", 0.0, 12.0);
    let m = d.masks();
    assert!(m[0]["expansion"].as_f64().unwrap() > 5.0, "{}", m[0]["expansion"]);

    // drag inside the mask moves it
    let c0 = v0(&m[0], 2);
    let body = d.rect("program.mask.body");
    let (bx, by) = (body[0] + body[2] / 2.0, body[1] + body[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": bx, "y": by}, "to": {"x": bx + 40.0, "y": by}, "steps": 5}));
    d.frames(2);
    let m = d.masks();
    let c1 = v0(&m[0], 2);
    assert!(c1.0 > c0.0 + 20.0, "{c0:?} → {c1:?}");
    d.shot("mask-edited");

    // Inverted checkbox (fold Opacity so the row is on screen)
    d.ok("ui.click", json!({"id": "effectControls.effect.opacity.twirl"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "effectControls.gaussian_blur.mask0.inverted"}));
    d.frames(2);
    assert_eq!(d.masks()[0]["inverted"], true);
    d.shot("mask-inverted");
    d.ok("ui.click", json!({"id": "effectControls.effect.opacity.twirl"}));
    d.frames(2);

    // pen mask on Opacity: four clicks, then click the first point to close
    d.ok("ui.click", json!({"id": "effectControls.opacity.mask.pen"}));
    d.frames(2);
    let pic = d.rect("program.picture");
    let pts = [(0.3, 0.3), (0.6, 0.25), (0.65, 0.7), (0.35, 0.75)];
    for (fx, fy) in pts {
        d.ok("ui.click", json!({"x": pic[0] + pic[2] * fx, "y": pic[1] + pic[3] * fy}));
        d.frames(2);
    }
    d.shot("mask-pen-drawing");
    d.ok("ui.click", json!({"x": pic[0] + pic[2] * 0.3, "y": pic[1] + pic[3] * 0.3}));
    d.frames(3);
    let m = d.masks();
    assert_eq!(m.len(), 2, "{m:?}");
    let pen = m.iter().find(|x| x["effectId"] == "opacity").expect("opacity mask");
    assert_eq!(pen["path"]["vertices"].as_array().unwrap().len(), 4);
    assert_eq!(pen["selected"], true);
    d.shot("mask-pen-opacity");

    // mask tracking buttons on the Mask Path row: forward one frame runs a job
    d.rect("effectControls.opacity.mask0.track.fwd");
    d.rect("effectControls.opacity.mask0.trackMethod");
    d.ok("ui.click", json!({"id": "effectControls.opacity.mask0.track.fwdFrame"}));
    let mut finished = false;
    for _ in 0..400 {
        d.frames(1);
        let jobs = d.exec("jobs.list", json!({}));
        if jobs.as_array().unwrap().iter().any(|j| j["label"].as_str().unwrap_or("").starts_with("Track") && j["finished"] == true) {
            finished = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    assert!(finished, "tracking job ran");
    d.frames(3);
    let m = d.masks();
    let pen = m.iter().find(|x| x["effectId"] == "opacity").unwrap();
    assert_eq!(pen["pathKeyframes"].as_array().unwrap().len(), 2, "{pen}");
    d.shot("mask-tracked");
}

/// The selected mask's outline and handles are drawn on the Program monitor only while its clip
/// is selected, as Motion's box is: selecting another clip, or none, hides them (#1341).
#[test]
fn mask_handles_follow_the_clip_selection() {
    let mut d = Driver::demo();
    let seq = d.exec("sequence.inspect", json!({}));
    let items = seq["video"][0]["items"].as_array().unwrap().clone();
    let (id, start, dur) = (items[0]["clip"].as_u64().unwrap(), items[0]["start"].as_i64().unwrap(), items[0]["duration"].as_i64().unwrap());
    let other = items[1]["clip"].as_u64().unwrap();
    d.exec("playhead.set", json!({"time": start + dur / 2}));
    d.exec("timeline.select", json!({"clips": [id]}));
    d.exec("masks.add", json!({"clip": id, "effect": "opacity", "shape": "ellipse"}));
    d.frames(3);
    assert!(d.has("program.mask.vertex.0"), "handles of the selected clip's mask");
    d.shot("mask-clip-selected");
    for clips in [json!([other]), json!([])] {
        d.exec("timeline.select", json!({"clips": clips}));
        d.frames(3);
        assert!(!d.has("program.mask.vertex.0"), "no handles with {clips} selected");
        assert!(!d.has("program.mask.body"), "no outline with {clips} selected");
        d.shot(&format!("mask-clip-deselected-{}", clips.as_array().map_or(0, Vec::len)));
    }
    d.exec("timeline.select", json!({"clips": [id]}));
    d.frames(3);
    assert!(d.has("program.mask.vertex.0"), "the handles come back with the clip");
}

/// Shift held while placing pen points snaps each new segment to 45° steps from the previous
/// point; a Shift-drag also constrains the Bézier handle it pulls out (#853).
#[test]
fn shift_constrains_pen_mask_segments_to_45_degrees() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = &seq["video"][0]["items"][0];
    let (id, start, dur) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap(), clip["duration"].as_i64().unwrap());
    d.exec("playhead.set", json!({"time": start + dur / 2}));
    d.exec("timeline.select", json!({"clips": [id]}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "effectControls.opacity.mask.pen"}));
    d.frames(2);
    let pic = d.rect("program.picture");
    let at = |fx: f64, fy: f64| json!({"x": pic[0] + pic[2] * fx, "y": pic[1] + pic[3] * fy});
    // Shift-clicks (control gestures carry their own modifiers since UI.6, #881)
    for (fx, fy) in [(0.3, 0.3), (0.6, 0.34), (0.63, 0.6)] {
        let mut p = at(fx, fy);
        p["modifiers"] = json!({"shift": true});
        d.ok("ui.click", p);
        d.frames(2);
    }
    // a Shift-drag places the fourth point and pulls out its handle
    d.ok("ui.drag", json!({"from": at(0.32, 0.62), "to": at(0.42, 0.64), "steps": 6, "modifiers": {"shift": true}}));
    d.frames(2);
    d.ok("ui.click", at(0.3, 0.3));
    d.frames(3);
    let m = d.masks();
    let pen = m.iter().find(|x| x["effectId"] == "opacity").unwrap_or_else(|| panic!("opacity mask: {m:?}"));
    assert_eq!(pen["path"]["vertices"].as_array().unwrap().len(), 4, "{pen}");
    let (p0, p1, p2, p3) = (v0(pen, 0), v0(pen, 1), v0(pen, 2), v0(pen, 3));
    assert!((p1.1 - p0.1).abs() < 1e-6, "segment 0→1 horizontal: {p0:?} {p1:?}");
    assert!(p1.0 > p0.0 + 50.0, "{p0:?} {p1:?}");
    assert!((p2.0 - p1.0).abs() < 1e-6, "segment 1→2 vertical: {p1:?} {p2:?}");
    assert!(p2.1 > p1.1 + 50.0, "{p1:?} {p2:?}");
    assert!((p3.1 - p2.1).abs() < 1e-6, "segment 2→3 horizontal: {p2:?} {p3:?}");
    let out = &pen["path"]["vertices"][3]["out"];
    let (ox, oy) = (out[0].as_f64().unwrap(), out[1].as_f64().unwrap());
    assert!(ox > 10.0 && oy.abs() < 1e-6, "handle constrained horizontal: {pen}");
}

/// Delete / Backspace and Cmd+Z take back the last pen point while a mask is drawn: the draft is
/// outside the undo history, so the keys must not reach Clear (on the clip being masked) or Undo
/// (of the previous project edit). The last point taken back closes the draft (#1345).
#[test]
fn delete_and_undo_take_back_the_last_pen_point() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = &seq["video"][0]["items"][0];
    let (id, start, dur) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap(), clip["duration"].as_i64().unwrap());
    d.exec("playhead.set", json!({"time": start + dur / 2}));
    d.exec("timeline.select", json!({"clips": [id]}));
    // a project edit for a stray Undo to revert
    d.exec("effects.apply", json!({"effect": "gaussian_blur"}));
    d.frames(3);
    let undo_len = |d: &mut Driver| d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    let clip_there = |d: &mut Driver| {
        let seq = d.exec("sequence.inspect", json!({}));
        seq["video"][0]["items"].as_array().unwrap().iter().any(|c| c["clip"].as_u64() == Some(id))
    };
    let undo0 = undo_len(&mut d);
    d.ok("ui.click", json!({"id": "effectControls.opacity.mask.pen"}));
    d.frames(2);
    let pic = d.rect("program.picture");
    let at = |fx: f64, fy: f64| json!({"x": pic[0] + pic[2] * fx, "y": pic[1] + pic[3] * fy});
    for (fx, fy) in [(0.3, 0.3), (0.6, 0.3), (0.6, 0.6)] {
        d.ok("ui.click", at(fx, fy));
        d.frames(2);
    }
    // with the Timeline focused, Delete and Backspace are Clear on the selected clip
    d.ok("ui.set", json!({"focused": "Timeline"}));
    // Redo is not a take-back
    d.ok("ui.key", json!({"key": "Shift+Cmd+Z"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Delete"}));
    d.frames(2);
    assert!(clip_there(&mut d), "Delete cleared the clip being masked");
    d.ok("ui.key", json!({"key": "Cmd+Z"}));
    d.frames(2);
    assert_eq!(undo_len(&mut d), undo0, "Cmd+Z undid a project edit while drawing");
    // one point left: two more, then a click on the first closes the mask with three vertices
    for (fx, fy) in [(0.35, 0.65), (0.25, 0.5)] {
        d.ok("ui.click", at(fx, fy));
        d.frames(2);
    }
    d.ok("ui.click", at(0.3, 0.3));
    d.frames(3);
    let m = d.masks();
    let pen = m.iter().find(|x| x["effectId"] == "opacity").unwrap_or_else(|| panic!("opacity mask: {m:?}"));
    assert_eq!(pen["path"]["vertices"].as_array().unwrap().len(), 3, "{pen}");
    let (p0, p1) = (v0(pen, 0), v0(pen, 1));
    assert!(p1.1 > p0.1 + 50.0, "second vertex is the one placed after the take-backs: {p0:?} {p1:?}");

    // Backspace on the only point closes the draft; Cmd+Z then undoes as usual
    d.ok("ui.click", json!({"id": "effectControls.opacity.mask.pen"}));
    d.frames(2);
    d.ok("ui.click", at(0.5, 0.5));
    d.frames(2);
    d.ok("ui.set", json!({"focused": "Timeline"}));
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.frames(2);
    assert!(clip_there(&mut d), "Backspace cleared the clip being masked");
    assert!(d.ok("ui.elements", json!({"prefix": "program.maskPen"})).as_array().unwrap().is_empty(), "draft still open");
    let undo1 = undo_len(&mut d);
    d.ok("ui.key", json!({"key": "Cmd+Z"}));
    d.frames(2);
    assert_eq!(undo_len(&mut d), undo1 - 1);
    assert!(d.masks().iter().all(|x| x["effectId"] != "opacity"), "Undo removed the pen mask");
}

#[test]
fn presets_bin_drag_and_save_dialog() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = &seq["video"][0]["items"][0];
    let (id, start, dur) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap(), clip["duration"].as_i64().unwrap());
    d.exec("playhead.set", json!({"time": start + dur / 2}));
    d.exec("timeline.select", json!({"clips": [id]}));
    // Presets bin: open the folder, double-click a built-in preset
    d.ok("ui.click", json!({"id": "effects.folder.Presets"}));
    d.frames(2);
    d.rect("effects.preset.Soft Vignette");
    // drag the preset onto the clip in the timeline
    let (fx0, fy0) = d.center("effects.preset.Soft Vignette");
    let (cx, cy) = d.center(&format!("timeline.clip.{id}"));
    d.ok("ui.drag", json!({"from": {"x": fx0, "y": fy0}, "to": {"x": cx, "y": cy}, "steps": 10}));
    d.frames(3);
    let seq = d.exec("sequence.inspect", json!({}));
    let fx = seq["video"][0]["items"][0]["effects"].as_array().unwrap().clone();
    let bc = fx.iter().find(|e| e["effect"] == "brightness_contrast").unwrap_or_else(|| panic!("{fx:?}"));
    assert_eq!(bc["masks"], 1);
    d.shot("presets-bin");
    // Save Preset dialog from the effect's context menu (opened through the command-equivalent UI state)
    for fx in ["motion", "opacity", "time_remap"] {
        d.ok("ui.click", json!({"id": format!("effectControls.effect.{fx}.twirl")}));
        d.frames(2);
    }
    d.ok("ui.click", json!({"id": "effectControls.effect.brightness_contrast", "button": "right"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "effectControls.effect.brightness_contrast.savePreset"}));
    d.frames(2);
    d.rect("savePreset.name");
    d.ok("ui.click", json!({"id": "savePreset.name"}));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "My Vignette"}));
    d.ok("ui.click", json!({"id": "savePreset.keyframes.anchorIn"}));
    d.shot("save-preset-dialog");
    d.ok("ui.click", json!({"id": "savePreset.ok"}));
    d.frames(3);
    let l = d.exec("presets.list", json!({}));
    let mine = l["presets"].as_array().unwrap().iter().find(|p| p["name"] == "My Vignette").cloned().expect("saved");
    assert_eq!(mine["keyframes"], "Anchor to In Point");
    assert_eq!(mine["masks"], 1);
    d.rect("effects.preset.My Vignette");
}

/// #1519: effects selected together in Effect Controls (Ctrl/Cmd-click, Shift-click) are saved as
/// one preset, which brings all of them, with their values, to another clip. Right-click ▸ Save
/// Preset… on an effect outside the selection saves that one alone.
#[test]
fn effects_selected_together_save_as_one_preset() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = |n: usize| seq["video"][0]["items"][n]["clip"].as_u64().unwrap();
    let (a, b) = (clip(0), clip(1));
    let item = &seq["video"][0]["items"][0];
    d.exec("playhead.set", json!({"time": item["start"].as_i64().unwrap() + item["duration"].as_i64().unwrap() / 2}));
    d.exec("timeline.select", json!({"clips": [a]}));
    for fx in ["gaussian_blur", "mosaic"] {
        d.exec("effects.apply", json!({"clips": [a], "effect": fx}));
    }
    d.exec("effects.setParam", json!({"clip": a, "effect": "gaussian_blur", "param": "blurriness", "value": 42}));
    d.exec("effects.setParam", json!({"clip": a, "effect": "mosaic", "param": "horizontal", "value": 33}));
    d.frames(3);
    for fx in ["motion", "opacity", "time_remap", "gaussian_blur", "mosaic"] {
        d.ok("ui.click", json!({"id": format!("effectControls.effect.{fx}.twirl")}));
        d.frames(2);
    }
    let save = |d: &mut Driver, fx: &str, name: &str| -> Value {
        d.ok("ui.click", json!({"id": format!("effectControls.effect.{fx}"), "button": "right"}));
        d.frames(2);
        d.ok("ui.click", json!({"id": format!("effectControls.effect.{fx}.savePreset")}));
        d.frames(2);
        let several = d.has("savePreset.effects");
        d.ok("ui.click", json!({"id": "savePreset.name"}));
        d.ok("ui.key", json!({"key": "Cmd+A"}));
        d.ok("ui.type", json!({"text": name}));
        d.ok("ui.click", json!({"id": "savePreset.ok"}));
        d.frames(3);
        let l = d.exec("presets.list", json!({}));
        let p = l["presets"].as_array().unwrap().iter().find(|p| p["name"] == name).cloned().unwrap_or_else(|| panic!("{name} not saved: {l}"));
        assert_eq!(several, p["effects"].as_array().unwrap().len() > 1, "{p}");
        p["effects"].clone()
    };
    // click Gaussian Blur, Ctrl/Cmd-click Mosaic: both go into one preset
    d.ok("ui.click", json!({"id": "effectControls.effect.gaussian_blur"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "effectControls.effect.mosaic", "modifiers": {"command": true}}));
    d.frames(2);
    assert_eq!(save(&mut d, "mosaic", "Blur and Mosaic"), json!(["Gaussian Blur", "Mosaic"]));
    // applied to another clip, it brings both effects with their values
    d.exec("presets.apply", json!({"preset": "Blur and Mosaic", "clips": [b]}));
    let seq = d.exec("sequence.inspect", json!({}));
    let fx = seq["video"][0]["items"].as_array().unwrap().iter().find(|c| c["clip"] == json!(b)).unwrap()["effects"].clone();
    let value = |id: &str, param: &str| fx.as_array().unwrap().iter().find(|e| e["effect"] == id).map(|e| e["params"][param]["value"].clone());
    assert_eq!(value("gaussian_blur", "blurriness"), Some(json!("Float(42.0)")), "{fx}");
    assert_eq!(value("mosaic", "horizontal"), Some(json!("Float(33.0)")), "{fx}");
    // Ctrl/Cmd-click again takes Mosaic out: Gaussian Blur alone
    d.ok("ui.click", json!({"id": "effectControls.effect.mosaic", "modifiers": {"command": true}}));
    d.frames(2);
    assert_eq!(save(&mut d, "gaussian_blur", "Blur Only"), json!(["Gaussian Blur"]));
    // Shift-click takes the effects in between, in the panel's order: Opacity, Time Remapping,
    // Gaussian Blur (saved in the clip's order)
    d.ok("ui.click", json!({"id": "effectControls.effect.opacity"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "effectControls.effect.gaussian_blur", "modifiers": {"shift": true}}));
    d.frames(2);
    let range = save(&mut d, "opacity", "Range");
    assert_eq!(range.as_array().unwrap().len(), 3, "{range}");
    for name in ["Opacity", "Time Remapping", "Gaussian Blur"] {
        assert!(range.as_array().unwrap().contains(&json!(name)), "{name} in {range}");
    }
    // Save Preset… on an effect outside the selection saves that one
    assert_eq!(save(&mut d, "mosaic", "Mosaic Only"), json!(["Mosaic"]));
    // a plain click selects one effect again
    d.ok("ui.click", json!({"id": "effectControls.effect.mosaic"}));
    d.frames(2);
    assert!(d.harness.state().ui.effect_multi_selection.is_none());
}

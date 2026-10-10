//! Effect masks in the UI, driven headless through the control channel: create masks from the
//! Effect Controls mask icons, edit them on the Program monitor (vertex, Bézier handle, feather,
//! whole-mask drags), draw a pen mask, toggle Inverted, track a mask; apply and save effect presets.
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

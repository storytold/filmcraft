//! The corner-roundness handle of rectangle / polygon masks on the Program monitor, driven
//! headless through the control channel: dragging it rounds the corners in one undo step; masks
//! without sharp corners show no handle.
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

fn vertex_count(m: &Value) -> usize {
    m["path"]["vertices"].as_array().map_or(0, Vec::len)
}

#[test]
fn roundness_handle_rounds_mask_corners_in_one_undo_step() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = &seq["video"][0]["items"][0];
    let (id, start, dur) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap(), clip["duration"].as_i64().unwrap());
    d.exec("playhead.set", json!({"time": start + dur / 2}));
    d.exec("timeline.select", json!({"clips": [id]}));

    // an ellipse has no sharp corner: no roundness handle
    d.exec("masks.add", json!({"clip": id, "effect": "opacity", "shape": "ellipse"}));
    d.exec("masks.select", json!({"clip": id, "effect": "opacity", "mask": 0}));
    d.frames(3);
    assert!(d.has("program.mask.vertex.0"), "the mask overlay is on the monitor");
    assert!(!d.has("program.mask.roundness"), "no roundness handle on an ellipse");

    // a rectangle has one, just inside its top-left corner
    d.exec("masks.add", json!({"clip": id, "effect": "opacity", "shape": "rectangle", "center": [960, 540], "size": [960, 540]}));
    d.exec("masks.select", json!({"clip": id, "effect": "opacity", "mask": 1}));
    d.frames(3);
    assert!(d.has("program.mask.roundness"));
    let corner = d.center("program.mask.vertex.0");
    let handle = d.center("program.mask.roundness");
    assert!(handle.0 > corner.0 && handle.1 > corner.1, "handle {handle:?} inside the top-left corner {corner:?}");
    assert_eq!(vertex_count(&d.masks()[1]), 4);
    d.shot("mask-roundness-handle");

    // a click without a drag changes nothing
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    d.ok("ui.click", json!({"id": "program.mask.roundness"}));
    d.frames(2);
    assert_eq!(vertex_count(&d.masks()[1]), 4);
    assert_eq!(d.exec("history.list", json!({}))["undo"].as_array().unwrap().len(), undo0);

    // dragging inward rounds all four corners: eight vertices, one undo step
    d.drag("program.mask.roundness", 30.0, 30.0);
    let m = d.masks();
    assert_eq!(vertex_count(&m[1]), 8, "{}", m[1]["path"]);
    let undo1 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    assert_eq!(undo1, undo0 + 1, "one undo step per drag");
    // the first fillet starts below the old top-left corner (960 − 480, 540 − 270)
    let p = &m[1]["path"]["vertices"][0]["p"];
    let (x, y) = (p[0].as_f64().unwrap(), p[1].as_f64().unwrap());
    assert!((x - 480.0).abs() < 1e-6 && y > 270.0 + 10.0, "fillet start ({x}, {y})");
    d.shot("mask-rounded-corners");
    // the rounded corners are no longer sharp: the handle is gone
    d.frames(2);
    assert!(!d.has("program.mask.roundness"));

    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(vertex_count(&d.masks()[1]), 4);
    assert!(d.has("program.mask.roundness"));
}

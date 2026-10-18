//! The Properties panel's Transform quick actions (#1686): **Fill** and **Fit** sit above Position,
//! as in Premiere's Properties panel, and run Fill frame / Fit to frame on every selected video
//! clip in one undo step.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the test also renders the UI with wgpu and writes PNGs there.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<std::path::PathBuf>,
}

impl Driver {
    fn new() -> Self {
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
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    /// The element's rect `[x, y, w, h]`, or `None` when it is not on screen.
    fn find(&mut self, id: &str) -> Option<[f64; 4]> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id)?;
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Some([r[0], r[1], r[2], r[3]])
    }
    fn click(&mut self, id: &str) {
        assert!(self.find(id).is_some(), "no element {id} in the Properties panel");
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
    /// A clip's Motion ▸ Scale and Position.
    fn motion(&self, clip: u64) -> (f64, [f64; 2]) {
        let it = self.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1;
        let m = it.effect("motion").unwrap();
        let scale = m.param("scale").unwrap().value.as_f64().unwrap();
        let p = m.param("position").unwrap().value.as_vec2().unwrap();
        (scale, [p.x, p.y])
    }
    fn undo_len(&mut self) -> usize {
        self.exec("history.list", json!({}))["undo"].as_array().unwrap().len()
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        for _ in 0..40 {
            self.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
    /// A portrait colour matte of `width` x 1080 on `track` at 0 s.
    fn matte(&mut self, width: u32, track: &str) -> u64 {
        let item = self.exec("file.newColorMatte", json!({"width": width, "height": 1080, "color": "#3080c0"}))["item"].as_u64().unwrap();
        self.exec("timeline.place", json!({"item": item, "track": track, "seconds": 0.0}))["clips"][0].as_u64().unwrap()
    }
}

#[test]
fn fill_and_fit_buttons_resize_every_selected_clip() {
    let mut d = Driver::new();
    // the demo sequence is 1920 x 1080: a 960-wide matte fills at 200 %, a 480-wide one at 400 %;
    // both fit at 100 % (their height)
    let (a, b) = (d.matte(960, "V3"), d.matte(480, "V2"));
    d.exec("timeline.select", json!({"clips": [a, b]}));
    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.frames(3);
    let (fill, fit) = (d.find("properties.fillFrame").expect("Fill button"), d.find("properties.fitToFrame").expect("Fit button"));
    // one row, Fill first, as in Premiere
    assert!((fill[1] - fit[1]).abs() < 0.5 && fill[0] + fill[2] <= fit[0], "Fill then Fit on one row: {fill:?} {fit:?}");
    d.shot("fit-fill-before");

    // move a clip off centre first: both actions centre it again
    d.exec("effects.setParam", json!({"clip": a, "effect": "motion", "param": "position", "value": [300.0, 200.0]}));
    let n0 = d.undo_len();
    d.click("properties.fillFrame");
    assert_eq!(d.undo_len(), n0 + 1, "Fill is one undo step for the whole selection");
    let ((sa, pa), (sb, _)) = (d.motion(a), d.motion(b));
    assert!((sa - 200.0).abs() < 1e-9 && (sb - 400.0).abs() < 1e-9, "Fill: {sa} {sb}");
    assert_eq!(pa, [960.0, 540.0], "Fill centres the clip");
    d.shot("fit-fill-after-fill");

    d.click("properties.fitToFrame");
    assert_eq!(d.undo_len(), n0 + 2);
    let ((sa, _), (sb, _)) = (d.motion(a), d.motion(b));
    assert!((sa - 100.0).abs() < 1e-9 && (sb - 100.0).abs() < 1e-9, "Fit: {sa} {sb}");
    d.shot("fit-fill-after-fit");

    // undo brings Fill's values back for both clips
    d.exec("edit.undo", json!({}));
    let ((sa, _), (sb, _)) = (d.motion(a), d.motion(b));
    assert!((sa - 200.0).abs() < 1e-9 && (sb - 400.0).abs() < 1e-9, "undo Fit: {sa} {sb}");
}

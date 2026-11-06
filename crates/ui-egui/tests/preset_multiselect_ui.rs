//! #1394: saving an effect preset from Effect Controls captures every selected effect.
//! Ctrl-click adds rows to the selection; the Save Preset… context menu on a selected row
//! captures the whole selection, and the dialog's OK saves them as one preset.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
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
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({ "command": command, "params": params }))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({ "id": id }));
        self.frames(3);
    }

    fn ctrl_click(&mut self, id: &str) {
        self.ok("ui.click", json!({ "id": id, "modifiers": { "ctrl": true } }));
        self.frames(3);
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({ "prefix": id }));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    /// Video clip on V1 with two standard video effects applied and selected; Effect
    /// Controls showing it, fixed-effect rows folded so the applied rows render.
    fn prepared(&mut self) -> u64 {
        self.ok("ui.set", json!({ "workspace": "Effects" }));
        self.exec("sequence.linkedSelection", json!({ "on": false }));
        let seq = self.exec("sequence.inspect", json!({}));
        let clip = &seq["video"][0]["items"][0];
        let (id, start) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap());
        self.exec("playhead.set", json!({ "time": start }));
        self.exec("timeline.select", json!({ "clips": [id] }));
        self.exec("effects.apply", json!({ "clips": [id], "effect": "invert" }));
        self.exec("effects.apply", json!({ "clips": [id], "effect": "gaussian_blur" }));
        self.frames(3);
        for fx in ["motion", "opacity"] {
            self.click(&format!("effectControls.effect.{fx}.twirl"));
        }
        id
    }
}

#[test]
fn ctrl_click_multi_select_saves_all_selected_effects_into_one_preset() {
    let mut d = Driver::new();
    d.prepared();

    // click the Invert row, ctrl-click the Gaussian Blur row: both selected
    d.click("effectControls.effect.invert");
    d.ctrl_click("effectControls.effect.gaussian_blur");

    // the context menu on a selected row offers Save Preset… for the whole selection
    d.ok("ui.click", json!({ "id": "effectControls.effect.gaussian_blur", "button": "right" }));
    d.frames(3);
    assert!(d.has("effectControls.effect.gaussian_blur.savePreset"), "context menu open");
    d.click("effectControls.effect.gaussian_blur.savePreset");
    // the draft carries both effects; give it a name and press OK
    {
        let app = d.harness.state_mut();
        let draft = app.ui.save_preset.as_mut().expect("save dialog open");
        assert_eq!(draft.effects.len(), 2, "draft carries both rows: {:?}", draft.effects);
        draft.name = "Pair Preset".into();
    }
    d.frames(2);
    d.click("savePreset.ok");
    let presets = d.exec("presets.list", json!({}))["presets"].clone();
    let arr = presets.as_array().unwrap();
    let saved = arr.iter().find(|p| p["name"] == json!("Pair Preset"));
    let Some(p) = saved else { panic!("preset not saved: {:?}", arr.iter().map(|p| &p["name"]).collect::<Vec<_>>()) };
    assert_eq!(p["effects"].as_array().unwrap().len(), 2, "the preset captured BOTH effects: {p}");
}

#[test]
fn a_plain_click_replaces_the_selection_with_one_effect() {
    let mut d = Driver::new();
    d.prepared();
    d.click("effectControls.effect.invert");
    d.ctrl_click("effectControls.effect.gaussian_blur");
    d.click("effectControls.effect.invert");
    // a plain click replaces the selection: the context menu now captures one effect
    d.ok("ui.click", json!({ "id": "effectControls.effect.invert", "button": "right" }));
    d.frames(3);
    d.click("effectControls.effect.invert.savePreset");
    let draft = d.harness.state_mut().ui.save_preset.clone().expect("save dialog open");
    assert_eq!(draft.effects.len(), 1, "plain click replaces the selection: {:?}", draft.effects);
}

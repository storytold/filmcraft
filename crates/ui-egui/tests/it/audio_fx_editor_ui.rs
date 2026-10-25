//! Headless UI tests of the Clip / Track Fx Editor windows (Parametric Equalizer, Graphic
//! Equalizer, Multiband Compressor, Dynamics): opened from Effect Controls ("Custom Setup ▸
//! Edit…") and from a mixer effect slot, driven by automation id and by pointer drags.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also write PNGs of the windows (`fx-editor-*.png`).

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
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
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn element(&mut self, id: &str) -> Option<[f32; 4]> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array()?.iter().find(|e| e["id"] == id).map(|e| {
            let r = &e["rect"];
            [r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32]
        })
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    /// Press on `id`, move by (dx, dy) over several frames, release.
    fn drag(&mut self, id: &str, dx: f32, dy: f32, steps: usize) {
        let r = self.element(id).unwrap_or_else(|| panic!("no element {id}"));
        let mut p = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(p));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.frames(1);
        for _ in 0..steps {
            p.x += dx / steps as f32;
            p.y += dy / steps as f32;
            push(self, egui::Event::PointerMoved(p));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(3);
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }

    fn undo_len(&mut self) -> usize {
        self.exec("history.list", json!({}))["undo"].as_array().unwrap().len()
    }

    /// Audio clip on A1 (id) with `effect` applied and selected; Effect Controls showing it.
    fn audio_clip_with(&mut self, effect: &str) -> u64 {
        self.ok("ui.set", json!({"workspace": "Effects"}));
        let seq = self.exec("sequence.inspect", json!({}));
        let clip = &seq["audio"][0]["items"][0];
        let (id, start) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap());
        self.exec("playhead.set", json!({"time": start}));
        // only the audio clip (Effect Controls prefers a linked video clip)
        self.exec("sequence.linkedSelection", json!({"on": false}));
        self.exec("timeline.select", json!({"clips": [id]}));
        self.exec("effects.apply", json!({"clips": [id], "effect": effect}));
        self.frames(3);
        // fold the fixed effects so the applied one's rows are on screen
        for fx in ["volume", "channel_volume", "panner"] {
            self.click(&format!("effectControls.effect.{fx}.twirl"));
        }
        id
    }

    fn clip_param(&mut self, clip: u64, effect: &str, param: &str) -> Value {
        let seq = self.exec("sequence.inspect", json!({}));
        let items = seq["audio"][0]["items"].as_array().unwrap().clone();
        let it = items.iter().find(|i| i["clip"] == clip).unwrap().clone();
        let fx = it["effects"].as_array().unwrap().iter().find(|e| e["effect"] == effect).unwrap().clone();
        fx["params"][param]["value"].clone()
    }
}

/// A parameter value from `sequence.inspect` (`"Float(3.5)"`, `"Bool(true)"`) or
/// `mixer.inspect` (plain JSON).
fn num(v: &Value) -> f64 {
    v.as_f64()
        .or_else(|| v.as_str().and_then(|s| s.trim_start_matches("Float(").trim_start_matches("Choice(").trim_end_matches(')').parse().ok()))
        .unwrap_or(f64::NAN)
}

fn truthy(v: &Value) -> bool {
    v.as_bool().unwrap_or(false) || v.as_str() == Some("Bool(true)")
}

#[test]
fn parametric_eq_editor_from_effect_controls() {
    let mut d = Driver::demo();
    let clip = d.audio_clip_with("parametric_eq");
    d.click("effectControls.effect.parametric_eq.edit");
    d.frames(3);
    let ids = d.ids("fxEditor.parametric_eq.");
    for id in ["plot", "node.hp", "node.low", "node.mid", "node.lp", "toggle.hp_on", "param.master_gain", "close"] {
        assert!(ids.iter().any(|i| *i == format!("fxEditor.parametric_eq.{id}")), "{id} missing from {ids:?}");
    }
    // band toggle by id
    d.click("fxEditor.parametric_eq.toggle.hp_on");
    assert!(truthy(&d.clip_param(clip, "parametric_eq", "hp_on")));
    // dragging band 3 up boosts it — one undo step for the whole drag
    let undo0 = d.undo_len();
    d.drag("fxEditor.parametric_eq.node.mid", 0.0, -60.0, 6);
    let g = num(&d.clip_param(clip, "parametric_eq", "mid_gain"));
    assert!(g > 3.0, "mid gain after drag: {g}");
    assert_eq!(d.undo_len(), undo0 + 2, "frequency + gain committed on release");
    d.snapshot("fx-editor-parametric-eq");
    d.click("fxEditor.parametric_eq.close");
    assert!(d.ids("fxEditor.parametric_eq.").is_empty(), "window closed");
}

#[test]
fn graphic_eq_and_dynamics_editors() {
    let mut d = Driver::demo();
    let clip = d.audio_clip_with("graphic_eq_30");
    d.click("effectControls.effect.graphic_eq_30.edit");
    d.frames(3);
    let ids = d.ids("fxEditor.graphic_eq_30.param.");
    assert_eq!(ids.iter().filter(|i| i.starts_with("fxEditor.graphic_eq_30.param.b")).count(), 30, "{ids:?}");
    d.drag("fxEditor.graphic_eq_30.param.b17", 0.0, -40.0, 5);
    let g = num(&d.clip_param(clip, "graphic_eq_30", "b17"));
    assert!(g > 3.0, "1 kHz band after drag: {g}");
    d.click("fxEditor.graphic_eq_30.reset");
    assert_eq!(num(&d.clip_param(clip, "graphic_eq_30", "b17")), 0.0);
    d.snapshot("fx-editor-graphic-eq-30");
    d.click("fxEditor.graphic_eq_30.close");
    // Dynamics: the transfer curve and section toggles
    d.exec("effects.apply", json!({"clips": [clip], "effect": "dynamics_rack"}));
    d.frames(3);
    d.click("effectControls.effect.graphic_eq_30.twirl");
    d.click("effectControls.effect.dynamics_rack.edit");
    d.frames(3);
    assert!(d.element("fxEditor.dynamics_rack.curve").is_some());
    d.click("fxEditor.dynamics_rack.toggle.lim_on");
    assert!(truthy(&d.clip_param(clip, "dynamics_rack", "lim_on")));
    d.snapshot("fx-editor-dynamics");
}

#[test]
fn track_fx_editor_from_the_mixer_slot() {
    let mut d = Driver::demo();
    d.exec("mixer.addInsert", json!({"strip": "A1", "effect": "multiband_compressor"}));
    d.ok("ui.set", json!({"workspace": "Audio"}));
    d.frames(3);
    d.click("mixer.showEffects");
    d.click("mixer.A1.fx.0");
    d.click("mixer.A1.fx.0.edit");
    d.frames(3);
    let ids = d.ids("fxEditor.multiband_compressor.");
    for id in ["xo1", "xo2", "xo3", "curve.1", "curve.4", "toggle.b2_solo", "param.b3_threshold", "toggle.lim_on"] {
        assert!(ids.iter().any(|i| *i == format!("fxEditor.multiband_compressor.{id}")), "{id} missing from {ids:?}");
    }
    d.click("fxEditor.multiband_compressor.toggle.b2_solo");
    let m = d.exec("mixer.inspect", json!({}));
    let a1 = m["strips"].as_array().unwrap().iter().find(|s| s["ref"] == "A1").unwrap().clone();
    assert!(truthy(&a1["inserts"][0]["params"]["b2_solo"]), "solo set on the insert: {a1}");
    // dragging the mid crossover to the right raises it
    d.drag("fxEditor.multiband_compressor.xo2", 60.0, 0.0, 5);
    let m = d.exec("mixer.inspect", json!({}));
    let a1 = m["strips"].as_array().unwrap().iter().find(|s| s["ref"] == "A1").unwrap().clone();
    let xo2 = num(&a1["inserts"][0]["params"]["xo2"]);
    assert!(xo2 > 2200.0, "crossover after drag: {xo2} ({a1})");
    d.snapshot("fx-editor-multiband");
}

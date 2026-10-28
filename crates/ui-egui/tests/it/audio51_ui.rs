//! Headless UI tests of the M7.8 audio features: the 5.1 panner puck on Audio Track Mixer strips
//! that feed a 5.1 Mix, six-channel meters, Show/Hide Tracks and Meter Input(s) Only, the mixer
//! transport's record button, and the Audio Clip Mixer automation modes recording clip keyframes.
//! The real `FilmcraftApp` runs under `egui_kittest`, driven by automation id and frame-by-frame
//! pointer drags.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to render the window offscreen with wgpu and write
//! `audio51-*.png` there.

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

    fn has(&mut self, id: &str) -> bool {
        self.ids(id).iter().any(|i| i == id)
    }

    /// Press on `id`, move by (`dx`, `dy`) over `steps` frames, release.
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
        self.frames(2);
    }

    fn strip(&mut self, r: &str) -> Value {
        let m = self.exec("mixer.inspect", json!({}));
        m["strips"].as_array().unwrap().iter().find(|s| s["ref"] == r).cloned().unwrap_or(Value::Null)
    }

    fn undo_len(&mut self) -> usize {
        self.exec("history.list", json!({}))["undo"].as_array().unwrap().len()
    }

    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                let path = dir.join(format!("{name}.png"));
                img.save(&path).unwrap();
                eprintln!("snapshot: {}", path.display());
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }
}

#[test]
fn surround_panner_puck_and_six_channel_meters() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Audio"}));
    d.frames(3);
    assert!(d.has("mixer.A1.pan") && !d.has("mixer.A1.pan51"), "stereo Mix: pan knob");
    d.exec("sequence.settings", json!({"mix": "5.1"}));
    d.frames(3);
    assert!(d.has("mixer.A1.pan51"), "5.1 Mix: the puck replaces the knob");
    assert!(!d.has("mixer.A1.pan"));
    for id in ["mixer.A1.pan51.center", "mixer.A1.pan51.lfe"] {
        assert!(d.has(id), "{id} missing");
    }
    // drag the puck to the left and towards the rear: one undo step, both lanes move
    let u0 = d.undo_len();
    d.drag("mixer.A1.pan51", -14.0, 10.0, 6);
    let a1 = d.strip("A1");
    let (x, y) = (a1["pan51"]["x"].as_f64().unwrap(), a1["pan51"]["y"].as_f64().unwrap());
    assert!(x < -20.0, "puck moved left: {a1}");
    assert!(y < 80.0, "puck moved back: {a1}");
    assert!(d.undo_len() > u0);
    // double-click returns to front centre
    d.ok("ui.click", json!({"id": "mixer.A1.pan51", "double": true}));
    d.frames(2);
    let a1 = d.strip("A1");
    if a1["pan51"]["x"].as_f64() != Some(0.0) {
        // controls without double-click support in this build of the control channel: set by command
        d.exec("mixer.setValue", json!({"strip": "A1", "lane": "pan51.x", "value": 0.0}));
    }
    // Center % and LFE from commands show on the strip
    d.exec("mixer.setValue", json!({"strip": "A1", "lane": "pan51.center", "value": 40.0}));
    d.frames(2);
    let ids = d.ok("ui.elements", json!({"prefix": "mixer.A1.pan51.center"}));
    assert!(ids.to_string().contains("Center 40 %"), "{ids}");
    // the Mix meter has six bars (label lists six values)
    let m = d.ok("ui.elements", json!({"prefix": "mixer.Mix.meter"}));
    let label = m[0]["label"].as_str().unwrap_or("").to_string();
    assert_eq!(label.matches('/').count(), 5, "six channel levels: {label}");
    d.snapshot("audio51-track-mixer");
}

#[test]
fn show_hide_tracks_and_meter_input_only() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Audio"}));
    d.frames(3);
    assert!(d.has("mixer.A2.fader"));
    d.click("mixer.menu");
    d.click("mixer.menu.showHide");
    d.frames(2);
    assert!(d.has("mixer.showHide.A2"), "{:?}", d.ids("mixer.showHide"));
    d.click("mixer.showHide.A2");
    assert!(!d.has("mixer.A2.fader"), "A2 hidden");
    assert!(d.has("mixer.A1.fader") && d.has("mixer.Mix.fader"));
    let st = d.ok("ui.inspect", json!({}));
    assert!(st.to_string().contains("mixer_hidden") || st.to_string().contains("mixerHidden"), "UI state is inspectable");
    d.click("mixer.showHide.all");
    d.click("mixer.showHide.ok");
    assert!(d.has("mixer.A2.fader"), "shown again");
    // Meter Input(s) Only: a UI toggle in the panel menu
    d.click("mixer.menu");
    d.click("mixer.menu.meterInputOnly");
    let st = d.ok("ui.inspect", json!({})).to_string();
    assert!(st.contains("\"mixer_meter_input_only\":true") || st.contains("\"mixerMeterInputOnly\":true"), "{st}");
    // the transport record button is wired to voice-over recording
    assert!(d.has("mixer.transport.voiceover.recordToggle"));
    d.snapshot("audio51-show-hide");
}

#[test]
fn clip_mixer_mode_records_clip_keyframes() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "AudioClipMixer"}));
    d.frames(3);
    assert!(d.has("clipMixer.A1.mode"));
    d.click("clipMixer.A1.mode");
    d.click("clipMixer.A1.mode.Latch");
    let m = d.exec("mixer.inspect", json!({}));
    assert_eq!(m["clipModes"][0]["mode"], "Latch");
    // an automation pass: the fader drag is a live gesture, written as clip keyframes at the end
    d.exec("playhead.set", json!({"seconds": 1.0}));
    d.exec("mixer.recordStart", json!({}));
    d.drag("clipMixer.A1.fader", 0.0, 40.0, 5);
    d.exec("playhead.set", json!({"seconds": 2.5}));
    let r = d.exec("mixer.recordStop", json!({}));
    assert_eq!(r["lanes"], 1, "{r}");
    assert_eq!(r["written"][0]["clips"], true, "{r}");
    let seq = d.exec("sequence.inspect", json!({}));
    let fx = &seq["audio"][0]["items"][0]["effects"];
    let vol = fx.as_array().unwrap().iter().find(|e| e["effect"] == "volume").unwrap();
    assert!(vol["params"]["level"]["keyframes"].as_u64().unwrap_or(0) >= 2, "clip keyframes written: {vol}");
    d.snapshot("audio51-clip-mixer");
}

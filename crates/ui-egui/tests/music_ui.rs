//! Headless UI tests of the Music panel: describing a cue, generating it with a fake provider,
//! and finding it on the timeline typed as Music with provenance, without any model or network.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use std::collections::HashMap;
use std::sync::Arc;

use filmcraft_engine::Session;
use filmcraft_engine::music::MusicSource;
use filmcraft_music as fm;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Fake;
struct FakeProvider;

impl MusicSource for Fake {
    fn provider(&self, _: &str, _: Option<&std::path::Path>, _: &HashMap<String, String>) -> Result<Box<dyn fm::Provider>, fm::MusicError> {
        Ok(Box::new(FakeProvider))
    }
}

impl fm::Provider for FakeProvider {
    fn info(&self) -> fm::ProviderInfo {
        fm::catalog::audio_api_info()
    }

    fn generate(&self, p: &fm::Params) -> Result<fm::Audio, fm::MusicError> {
        let n = (p.duration * 44_100.0) as usize;
        Ok(fm::Audio { samples: (0..n * 2).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.1).collect(), sample_rate: 44_100, channels: 2 })
    }
}

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo(scratch: &str) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.music_source = Arc::new(Fake);
        session.execute("file.projectSettings.scratchDisks", json!({"captured": scratch})).expect("scratch disk");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 1500.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
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
        self.frames(3);
    }

    fn type_text(&mut self, text: &str) {
        self.ok("ui.type", json!({"text": text}));
        self.frames(3);
    }

    fn ui(&mut self) -> Value {
        self.ok("ui.inspect", json!({}))
    }

    fn draft(&mut self) -> Value {
        self.ui()["ui"]["music"].clone()
    }

    /// Let a background generation job finish.
    fn settle(&mut self) {
        for _ in 0..400 {
            if self.draft()["pending"].is_null() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
            self.frames(1);
        }
        panic!("generation never finished");
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn music_clips(&mut self) -> Vec<u64> {
        let seq = self.exec("sequence.inspect", json!({}));
        let mut out = Vec::new();
        for t in seq["audio"].as_array().into_iter().flatten() {
            for it in t["items"].as_array().into_iter().flatten() {
                let id = it["clip"].as_u64().unwrap();
                if self.call("engine.execute", json!({"command": "music.inspect", "params": {"clip": id}}))["ok"] == json!(true) {
                    out.push(id);
                }
            }
        }
        out
    }
}

fn scratch(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("filmcraft-music-ui-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d.to_string_lossy().into_owned()
}

#[test]
fn describe_generate_and_find_the_music_on_the_timeline() {
    let dir = scratch("flow");
    let mut d = Driver::demo(&dir);
    d.exec("window.panel.Music", json!({}));
    d.frames(3);
    assert!(!d.ids("program.").is_empty(), "Program monitor still shown");
    let ids = d.ids("music.");
    for id in [
        "music.provider",
        "music.prompt",
        "music.fillInOut",
        "music.duration",
        "music.bpm",
        "music.key",
        "music.instrumental",
        "music.ducking",
        "music.advanced",
        "music.generate",
    ] {
        assert!(ids.iter().any(|i| i == id), "missing {id} in {ids:?}");
    }
    // the on-device model is not installed here, so Generate is off and nothing is added
    d.click("music.generate");
    assert!(d.music_clips().is_empty());
    // pick the API provider (the fake answers for it) and describe a cue
    d.ok("ui.set", json!({"music": {"provider": "audio-api", "duration": 5.0}}));
    d.exec("prefs.set", json!({"values": {"music.api.otherEndpoint": "https://example.org/gen", "music.api.otherApiKey": "sk-ui-test"}}));
    d.frames(80);
    d.click("music.prompt");
    d.type_text("Warm piano");
    d.exec("playhead.set", json!({"seconds": 1.0}));
    d.click("music.generate");
    d.settle();
    let clips = d.music_clips();
    let status = d.ui()["ui"]["status"].clone();
    assert_eq!(clips.len(), 1, "one music clip; status {status}");
    let info = d.exec("music.inspect", json!({"clip": clips[0]}));
    assert_eq!(info["provenance"]["prompt"], "Warm piano");
    assert_eq!(info["provenance"]["provider"], "audio-api");
    assert!(!info.to_string().contains("sk-ui-test"));
    assert_eq!(d.ui()["selection"], json!([clips[0]]), "the new clip is selected");
    let hist = d.exec("history.list", json!({}));
    assert_eq!(hist["undo"].as_array().unwrap().last().unwrap(), "New Music");
    d.exec("edit.undo", json!({}));
    d.frames(3);
    assert!(d.music_clips().is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn controls_fold_and_keys_are_masked() {
    let dir = scratch("controls");
    let mut d = Driver::demo(&dir);
    d.exec("window.panel.Music", json!({}));
    d.frames(3);
    assert_eq!(d.draft()["advancedOpen"], false);
    assert!(!d.ids("music.").iter().any(|i| i == "music.seed"));
    d.click("music.advanced");
    assert_eq!(d.draft()["advancedOpen"], true);
    let ids = d.ids("music.");
    for id in ["music.seed", "music.setting.stabilityEndpoint", "music.setting.stabilityApiKey", "music.setting.otherEndpoint", "music.setting.otherApiKey"] {
        assert!(ids.iter().any(|i| i == id), "missing {id} in {ids:?}");
    }
    // Fill In to Out replaces the length slider
    assert!(ids.iter().any(|i| i == "music.duration"));
    d.click("music.fillInOut");
    assert_eq!(d.draft()["fillInOut"], true);
    assert!(!d.ids("music.").iter().any(|i| i == "music.duration"));
    // a key typed in the panel is saved to the preferences and never echoed back
    d.exec("prefs.set", json!({"key": "music.api.stabilityApiKey", "value": "sk-masked-1"}));
    let echoed = d.exec("prefs.get", json!({})).to_string();
    assert!(!echoed.contains("sk-masked-1"));
    assert!(!d.ui().to_string().contains("sk-masked-1"), "ui.inspect leaked the key");
    let _ = std::fs::remove_dir_all(&dir);
}

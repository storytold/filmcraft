//! Headless UI tests of voice-over recording: the audio track header's microphone button, its
//! right-click Voice-Over Record Settings dialog, and a recording with the synthetic input landing
//! on the clicked track.

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
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
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

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn settings(&mut self) -> Value {
        self.exec("audio.voiceover.settings", json!({}))["settings"].clone()
    }
}

#[test]
fn a_failed_take_can_be_discarded_from_the_settings_dialog() {
    let mut d = Driver::demo();
    d.exec("audio.voiceover.start", json!({"track":"A1","time":0,"preroll":0}));
    let blocker = std::env::temp_dir().join(format!("filmcraft-voiceover-ui-blocker-{}", std::process::id()));
    std::fs::write(&blocker, b"not a directory").unwrap();
    let failure = d.call("engine.execute", json!({"command":"audio.voiceover.stop","params":{"seconds":1,"dir":blocker}}));
    assert_eq!(failure["ok"], false);
    assert!(d.harness.state().session.voiceover.recording());
    d.ok("ui.menu.invoke", json!({"id":"voiceover.settingsDialog"}));
    d.frames(4); // Allow the newly anchored window to settle before using its button rectangle.
    d.click("voiceover.discard");
    assert!(!d.harness.state().session.voiceover.recording(), "{}", d.harness.state().ui.status);
    d.exec("file.newProject", json!({}));
    std::fs::remove_file(blocker).unwrap();
}

#[test]
fn settings_dialog_from_the_track_header_persists_on_ok() {
    let mut d = Driver::demo();
    assert!(d.ids("timeline.track.A1.voiceover").contains(&"timeline.track.A1.voiceover".to_string()));
    // right-click the microphone: the context menu has the settings entry
    d.ok("ui.click", json!({"id": "timeline.track.A1.voiceover", "button": "right"}));
    d.frames(2);
    d.click("timeline.track.A1.voiceover.settings");
    let ids = d.ids("voiceover.");
    for id in [
        "voiceover.name",
        "voiceover.source",
        "voiceover.input",
        "voiceover.countdown",
        "voiceover.preroll",
        "voiceover.postroll",
        "voiceover.ok",
        "voiceover.cancel",
    ] {
        assert!(ids.iter().any(|x| x == id), "{id} missing in {ids:?}");
    }
    // edit: countdown off, name appended, input channel 2 from the dropdown
    d.click("voiceover.countdown");
    d.click("voiceover.name");
    d.ok("ui.type", json!({"text": " Take"}));
    d.frames(2);
    d.click("voiceover.input");
    d.click("voiceover.input.1");
    d.click("voiceover.ok");
    assert!(d.ids("voiceover.ok").is_empty(), "the dialog closed");
    let s = d.settings();
    assert_eq!(s["countdownSoundCues"], false);
    assert_eq!(s["name"], "Voice-over Take");
    assert_eq!(s["inputChannel"], 1);
    // Cancel discards edits (opened through the UI command this time)
    d.ok("ui.menu.invoke", json!({"id": "voiceover.settingsDialog"}));
    d.frames(2);
    d.click("voiceover.countdown");
    d.click("voiceover.cancel");
    assert!(d.ids("voiceover.ok").is_empty());
    assert_eq!(d.settings()["countdownSoundCues"], false);
}

#[test]
fn microphone_button_records_onto_its_track() {
    let mut d = Driver::demo();
    let dir = std::env::temp_dir().join(format!("filmcraft-vo-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    d.exec("file.projectSettings.scratchDisks", json!({"captured": dir.to_string_lossy()}));
    d.exec("audio.voiceover.settings", json!({"prerollSeconds": 0.5}));
    let (n, label) = (2usize, "A2".to_string());
    d.exec("playhead.set", json!({"seconds": 2.0}));
    d.click(&format!("timeline.track.{label}.voiceover"));
    let st = d.settings();
    let rec = d.exec("audio.voiceover.settings", json!({}))["recording"].clone();
    assert!(rec.is_object(), "recording started: {st} {rec}");
    let (start, capture) = (rec["recordStart"].as_i64().unwrap(), rec["captureStart"].as_i64().unwrap());
    // half a second of pre-roll; playback starts on the frame at or before the capture start (the
    // playhead snaps to frames) and the capture restarts there when the audio clock starts
    let frame = 10_594_584_000; // 1/23.976 s
    assert!((127_008_000_000..=127_008_000_000 + frame).contains(&(start - capture)), "{}", start - capture);
    assert_eq!(d.ok("ui.inspect", json!({}))["playback"]["playing"], json!(true));
    // playing: the countdown / recording overlay is up
    assert!(!d.ids("voiceover.countdown.overlay").is_empty());
    // stop at 3 s (headless frames don't advance the playback clock; the engine command takes the
    // time) and look at the result on the record track
    let v = d.exec("audio.voiceover.stop", json!({"time": start + 254_016_000_000}));
    assert_eq!(v["placed"], true);
    assert_eq!(v["track"], label.as_str());
    assert_eq!(v["samples"], 48_000);
    d.ok("ui.playback", json!({"action": "stop"}));
    let q = d.exec("sequence.inspect", json!({}));
    let clips = q["audio"][n - 1]["items"].as_array().cloned().unwrap_or_default();
    assert!(clips.iter().any(|c| c["start"] == json!(start) && c["duration"] == json!(254_016_000_000i64) && c["clip"] == v["clip"]), "{q}");
    assert!(d.exec("audio.voiceover.settings", json!({}))["recording"].is_null());
    // clicking the microphone while recording stops the take (and playback)
    d.click(&format!("timeline.track.{label}.voiceover"));
    assert!(d.exec("audio.voiceover.settings", json!({}))["recording"].is_object());
    d.click(&format!("timeline.track.{label}.voiceover"));
    assert!(d.exec("audio.voiceover.settings", json!({}))["recording"].is_null());
    assert_eq!(d.ok("ui.inspect", json!({}))["playback"]["playing"], json!(false));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recording_past_the_end_of_the_timeline_starts_there_not_at_the_beginning() {
    let mut d = Driver::demo();
    let dir = std::env::temp_dir().join(format!("filmcraft-vo-ui-end-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    d.exec("file.projectSettings.scratchDisks", json!({"captured": dir.to_string_lossy()}));
    d.exec("audio.voiceover.settings", json!({"prerollSeconds": 0.5}));
    d.harness.state_mut().session.prefs.timeline.return_to_beginning = true;
    // far beyond the last clip: empty timeline
    d.exec("playhead.set", json!({"seconds": 10_000.0}));
    d.click("timeline.track.A2.voiceover");
    let rec = d.exec("audio.voiceover.settings", json!({}))["recording"].clone();
    assert!(rec.is_object(), "recording started: {rec}");
    let ps = d.ok("ui.inspect", json!({}));
    assert_eq!(ps["playback"]["playing"], json!(true));
    let playhead = ps["playhead"].as_i64().unwrap();
    assert!(playhead > 9_000 * 254_016_000_000i64, "playhead jumped back to {playhead}");
    d.click("timeline.track.A2.voiceover");
    let _ = std::fs::remove_dir_all(&dir);
}

//! Headless UI tests of renaming a track in its Timeline header: double-click the name (or
//! Rename in the header's context menu), type, then Enter commits as one undo step and Escape
//! cancels. A double-click elsewhere on the header still toggles the track height.

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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 1400.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        // tall enough that the headers show the track names
        d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 64.0, "audioTrackHeight": 64.0}}));
        d.frames(3);
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
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn key(&mut self, key: &str) {
        self.ok("ui.key", json!({"key": key}));
        self.frames(2);
    }

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn rect(&mut self, id: &str) -> Vec<f32> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        e["rect"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect()
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn track_name(&self, label: &str) -> String {
        let seq = self.app().session.active_sequence().unwrap();
        let (tracks, i) = match label.split_at(1) {
            ("V", n) => (&seq.video_tracks, n.parse::<usize>().unwrap() - 1),
            (_, n) => (&seq.audio_tracks, n.parse::<usize>().unwrap() - 1),
        };
        tracks[i].name.clone()
    }

    fn rename_by_double_click(&mut self, label: &str) {
        // let earlier clicks leave egui's multi-click window, or this one counts as a triple click
        self.frames(30);
        self.ok("ui.click", json!({"id": format!("timeline.track.{label}.name"), "count": 2}));
        self.frames(3);
        assert!(self.app().ui.timeline.rename_track.is_some(), "double-clicking the name of {label} starts renaming it");
        assert!(self.has(&format!("timeline.track.{label}.rename")), "the rename field of {label} is shown");
    }
}

#[test]
fn double_click_the_name_renames_the_track_as_one_undo_step() {
    let mut d = Driver::demo();
    let before = d.track_name("V1");
    let heights = (d.app().ui.timeline.video_track_h, d.app().ui.timeline.audio_track_h);
    d.rename_by_double_click("V1");
    assert_eq!(
        (d.app().ui.timeline.video_track_h, d.app().ui.timeline.audio_track_h),
        heights,
        "a double-click on the name renames instead of toggling the track height"
    );
    d.key("Cmd+A");
    d.ok("ui.type", json!({"text": "Gameplay"}));
    d.frames(2);
    d.key("Enter");
    assert_eq!(d.track_name("V1"), "Gameplay");
    assert!(d.app().ui.timeline.rename_track.is_none(), "Enter ends the rename");
    assert!(!d.has("timeline.track.V1.rename"));
    d.exec("edit.undo", json!({}));
    assert_eq!(d.track_name("V1"), before, "the rename is one undo step");

    // audio tracks too, with surrounding spaces trimmed
    d.rename_by_double_click("A1");
    d.key("Cmd+A");
    d.ok("ui.type", json!({"text": "  Microfone  "}));
    d.frames(2);
    d.key("Enter");
    assert_eq!(d.track_name("A1"), "Microfone");
}

#[test]
fn escape_and_empty_names_leave_the_track_name_unchanged() {
    let mut d = Driver::demo();
    let before = d.track_name("V1");
    let undo = d.app().session.history.undo.len();
    d.rename_by_double_click("V1");
    d.key("Cmd+A");
    d.ok("ui.type", json!({"text": "Discarded"}));
    d.frames(2);
    d.key("Escape");
    assert_eq!(d.track_name("V1"), before, "Escape cancels");
    assert!(d.app().ui.timeline.rename_track.is_none());

    d.rename_by_double_click("V1");
    d.key("Cmd+A");
    d.key("Backspace");
    d.key("Enter");
    assert_eq!(d.track_name("V1"), before, "an empty name is ignored");
    assert_eq!(d.app().session.history.undo.len(), undo, "nothing was added to the history");
}

#[test]
fn double_click_beside_the_name_still_toggles_the_track_height() {
    let mut d = Driver::demo();
    let h = d.app().ui.timeline.video_track_h;
    let name = d.rect("timeline.track.V1.name");
    // on the name line, right of the name
    d.ok("ui.click", json!({"x": name[0] + name[2] + 30.0, "y": name[1] + name[3] / 2.0, "count": 2}));
    d.frames(3);
    assert!(d.app().ui.timeline.rename_track.is_none(), "beside the name nothing is renamed");
    assert_ne!(d.app().ui.timeline.video_track_h, h, "the header double-click still changes the height");
}

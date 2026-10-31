//! Headless UI tests of the Edit Marker dialog (#3023): double-clicking a marker on the Timeline
//! ruler opens it, OK applies the fields in one undo step, Cancel changes nothing.

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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
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
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn has_element(&mut self, id: &str) -> bool {
        self.ok("ui.elements", json!({"prefix": id})).as_array().is_some_and(|v| v.iter().any(|e| e["id"] == id))
    }

    fn marker(&self, id: u64) -> filmcraft_project::Marker {
        self.harness.state().session.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == id).unwrap().clone()
    }

    /// One new marker at 2 s (the demo has others), and the Timeline focused.
    fn setup(&mut self) -> u64 {
        self.ok("ui.set", json!({"focused": "Timeline"}));
        let id = self.exec("markers.add", json!({"time": filmcraft_time::Tick::from_seconds_f64(2.0).0, "name": "First"}))["marker"].as_u64().unwrap();
        self.frames(4);
        id
    }

    fn click(&mut self, id: &str, count: u64) {
        self.ok("ui.click", json!({"id": id, "count": count}));
        self.frames(2);
    }

    fn type_into(&mut self, id: &str, text: &str) {
        self.click(id, 3);
        self.ok("ui.type", json!({"text": text}));
        self.frames(2);
    }
}

#[test]
fn double_clicking_a_marker_opens_the_dialog_and_ok_applies_the_fields() {
    let mut d = Driver::demo();
    let id = d.setup();
    let undo_before = d.harness.state().session.history.undo.len();
    assert!(!d.has_element("timeline.markerDialog.ok"), "no dialog before the double-click");
    d.click(&format!("timeline.marker.{id}"), 1);
    assert!(!d.has_element("timeline.markerDialog.ok"), "one click only moves the playhead");
    d.click(&format!("timeline.marker.{id}"), 2);
    assert!(d.has_element("timeline.markerDialog.ok"), "a double-click opens Edit Marker");
    d.type_into("timeline.markerDialog.name", "Renamed");
    d.type_into("timeline.markerDialog.comment", "A note");
    d.click("timeline.markerDialog.ok", 1);
    assert!(!d.has_element("timeline.markerDialog.ok"), "OK closes the dialog");
    let m = d.marker(id);
    assert_eq!((m.name.as_str(), m.comment.as_str()), ("Renamed", "A note"));
    assert_eq!(d.harness.state().session.history.undo.len(), undo_before + 1, "one OK is one undo step");
}

#[test]
fn cancel_leaves_the_marker_alone() {
    let mut d = Driver::demo();
    let id = d.setup();
    let before = d.marker(id);
    d.click(&format!("timeline.marker.{id}"), 2);
    d.type_into("timeline.markerDialog.name", "Nope");
    d.click("timeline.markerDialog.cancel", 1);
    assert!(!d.has_element("timeline.markerDialog.cancel"));
    assert_eq!(d.marker(id), before);
}

/// Renaming a marker keeps a duration the dialog cannot show exactly: one that is not a whole number
/// of frames, and one beyond the duration control's range.
#[test]
fn ok_keeps_a_duration_that_was_not_edited() {
    for ticks in [|frame: i64| frame * 10 + 1, |frame: i64| frame * 20_000_000] {
        let mut d = Driver::demo();
        let id = d.setup();
        let frame = d.harness.state().session.sequence_rate().tick_of(1).0;
        let want = filmcraft_time::Tick(ticks(frame));
        d.harness
            .state_mut()
            .session
            .edit_sequence("Set duration", |q, _, _| {
                q.markers.iter_mut().filter(|m| m.id.0 == id).for_each(|m| m.duration = want);
                Ok(())
            })
            .expect("set the duration");
        d.click(&format!("timeline.marker.{id}"), 2);
        d.type_into("timeline.markerDialog.name", "Renamed");
        d.click("timeline.markerDialog.ok", 1);
        let m = d.marker(id);
        assert_eq!((m.name.as_str(), m.duration), ("Renamed", want), "only the name changed");
    }
}

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

fn sequence_markers(d: &Driver) -> usize {
    d.harness.state().session.active_sequence().unwrap().markers.len()
}

/// Add Marker on the frame of a marker opens that marker's Edit Marker dialog instead of putting a
/// second marker on top of it: from the key, the Timeline's button and the Program monitor's.
#[test]
fn add_marker_on_a_marker_opens_its_dialog() {
    let mut d = Driver::demo();
    let id = d.setup();
    let before = sequence_markers(&d);
    for how in ["key", "timeline.toggle.marker", "program.transport.markers.add"] {
        let at = d.marker(id).start;
        d.harness.state_mut().session.set_playhead(at);
        d.ok("ui.set", json!({"focused": "Timeline"}));
        if how == "key" {
            d.ok("ui.key", json!({"key": "M"}));
            d.frames(2);
        } else {
            d.click(how, 1);
        }
        assert_eq!(d.harness.state().ui.marker_properties.as_ref().map(|m| m.marker), Some(id), "{how}: the marker's dialog");
        assert_eq!(sequence_markers(&d), before, "{how}: no second marker");
        d.harness.state_mut().ui.marker_properties = None;
        d.frames(2);
    }
}

/// Two markers already stacked on one frame: Add Marker opens the one drawn on top, the later one.
#[test]
fn add_marker_on_stacked_markers_opens_the_top_one() {
    let mut d = Driver::demo();
    let first = d.setup();
    let at = d.marker(first).start;
    let top = d.exec("markers.add", json!({"time": at.0, "name": "Top"}))["marker"].as_u64().unwrap();
    d.frames(2);
    let before = sequence_markers(&d);
    d.harness.state_mut().session.set_playhead(at);
    d.ok("ui.key", json!({"key": "M"}));
    d.frames(2);
    assert_eq!(d.harness.state().ui.marker_properties.as_ref().map(|m| m.marker), Some(top), "the marker drawn on top");
    assert_eq!(sequence_markers(&d), before, "no third marker");
}

/// Off a marker Add Marker adds one, and a time given to it always adds, even on a marker.
#[test]
fn add_marker_off_a_marker_or_with_a_time_adds_one() {
    let mut d = Driver::demo();
    let id = d.setup();
    let before = sequence_markers(&d);
    let at = d.marker(id).start;
    let rate = d.harness.state().session.sequence_rate();
    d.harness.state_mut().session.set_playhead(at + rate.frame_duration());
    d.ok("ui.key", json!({"key": "M"}));
    d.frames(2);
    assert_eq!(sequence_markers(&d), before + 1, "a frame later is off the marker");
    assert!(d.harness.state().ui.marker_properties.is_none());
    d.ok("ui.menu.invoke", json!({"command": "markers.add", "params": {"time": at.0}}));
    d.frames(2);
    assert_eq!(sequence_markers(&d), before + 2, "a time given adds");
    assert!(d.harness.state().ui.marker_properties.is_none());
}

/// The Source monitor's Add Marker does the same with the clip's markers.
#[test]
fn add_marker_in_the_source_monitor_on_a_marker_opens_its_dialog() {
    use filmcraft_ui_egui::panels::marker_properties::Owner;
    let mut d = Driver::demo();
    d.exec("source.open", json!({"item": 5}));
    d.frames(4);
    let id = d.exec("markers.add", json!({"target": "source"}))["marker"].as_u64().unwrap();
    d.frames(2);
    let count = |d: &Driver| filmcraft_engine::clip_ops::source_view(&d.harness.state().session, filmcraft_project::ItemId(5)).unwrap().markers.len();
    let before = count(&d);
    d.click("source.transport.markers.add", 1);
    let draft = d.harness.state().ui.marker_properties.as_ref().map(|m| (m.marker, m.owner));
    assert_eq!(draft, Some((id, Owner::Source(5))), "the clip marker's dialog");
    assert_eq!(count(&d), before, "no second marker on the clip");
}

#[test]
fn double_clicking_a_marker_opens_the_dialog_and_ok_applies_the_fields() {
    let mut d = Driver::demo();
    let id = d.setup();
    let undo_before = d.harness.state().session.history.undo.len();
    assert!(!d.has_element("markerProperties.apply"), "no dialog before the double-click");
    d.click(&format!("timeline.marker.{id}"), 1);
    assert!(!d.has_element("markerProperties.apply"), "one click only moves the playhead");
    d.click(&format!("timeline.marker.{id}"), 2);
    assert!(d.has_element("markerProperties.apply"), "a double-click opens Edit Marker");
    d.type_into("markerProperties.name", "Renamed");
    d.type_into("markerProperties.comment", "A note");
    d.click("markerProperties.apply", 1);
    assert!(!d.has_element("markerProperties.apply"), "OK closes the dialog");
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
    d.type_into("markerProperties.name", "Nope");
    d.click("markerProperties.cancel", 1);
    assert!(!d.has_element("markerProperties.cancel"));
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
        d.type_into("markerProperties.name", "Renamed");
        d.click("markerProperties.apply", 1);
        let m = d.marker(id);
        assert_eq!((m.name.as_str(), m.duration), ("Renamed", want), "only the name changed");
    }
}

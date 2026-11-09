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

    /// The screen rect of a marker's flag on the Timeline ruler: x, y, width, height.
    fn flag(&mut self, id: u64) -> [f64; 4] {
        let flag = format!("timeline.marker.{id}");
        let e = self.ok("ui.elements", json!({"prefix": flag}));
        let r = e.as_array().unwrap().iter().find(|e| e["id"] == flag.as_str()).unwrap()["rect"].clone();
        [0, 1, 2, 3].map(|i| r[i].as_f64().unwrap())
    }

    /// Drags with the primary button from `from`, `dx` px sideways, and lets the release land.
    fn drag(&mut self, from: (f64, f64), dx: f64) {
        self.ok("ui.drag", json!({"from": {"x": from.0, "y": from.1}, "to": {"x": from.0 + dx, "y": from.1}, "steps": 8}));
        self.frames(3);
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

/// #4491: dragging a marker's flag on the ruler moves the marker, in one undo step, and leaves the
/// playhead on the press (it no longer scrubs along).
#[test]
fn dragging_a_marker_flag_moves_the_marker() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    let id = d.setup();
    let before = d.marker(id).start;
    let r = d.flag(id);
    let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let tick_at = |d: &mut Driver, x: f64| d.ok("ui.timeline.hit", json!({"x": x, "y": y}))["time"].as_i64().unwrap();
    let delta = tick_at(&mut d, x + 120.0) - tick_at(&mut d, x);
    let undo_before = d.harness.state().session.history.undo.len();
    d.drag((x, y), 120.0);
    let rate = d.harness.state().session.sequence_rate();
    let after = d.marker(id).start;
    assert!((after.0 - (before.0 + delta)).abs() <= rate.frame_duration().0, "the marker follows the pointer: {before:?} -> {after:?}, moved {delta}");
    assert_eq!(after, rate.snap_nearest(after), "it lands on a frame");
    let playhead = d.harness.state().session.playhead();
    // within a frame: the playhead shows a whole frame, and 2 s is not one at the demo's 23.976
    assert!((playhead.0 - before.0).abs() <= rate.frame_duration().0, "the playhead stays on the press, not dragged along: {playhead:?}");
    assert_eq!(d.harness.state().session.history.undo.last().map(|u| u.0.as_str()), Some("Edit Markers"));
    assert_eq!(d.harness.state().session.history.undo.len(), undo_before + 1, "one drag is one undo step");
}

/// #4491: the bottom of a flag reaches into the work area bar's row; a drag there moves the marker,
/// not the bar.
#[test]
fn a_marker_flag_over_the_work_area_bar_drags_the_marker() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    let id = d.setup();
    let second = filmcraft_time::Tick::from_seconds_f64(1.0).0;
    d.exec("sequence.setWorkArea", json!({"start": second, "end": 4 * second}));
    d.frames(2);
    let area = d.harness.state().session.active_sequence().unwrap().work_area;
    let before = d.marker(id).start;
    let r = d.flag(id);
    // 0.5 px above the flag's bottom: inside the bar's hit row too
    let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] - 0.5);
    assert_eq!(d.ok("ui.timeline.hit", json!({"x": x, "y": y}))["hit"], "ruler", "the flag wins over the bar");
    d.drag((x, y), 120.0);
    assert!(d.marker(id).start > before, "the marker moved");
    assert_eq!(d.harness.state().session.active_sequence().unwrap().work_area, area, "the bar stayed");
}

/// #4491: snapped onto another marker that starts between frames, the dragged marker lands on that
/// marker's frame, not between frames.
#[test]
fn a_marker_snapped_to_an_off_frame_marker_lands_on_a_frame() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": true}));
    let id = d.setup();
    let target = filmcraft_time::Tick::from_seconds_f64(6.0);
    let other = d.exec("markers.add", json!({"time": target.0}))["marker"].as_u64().unwrap();
    d.frames(2);
    let rate = d.harness.state().session.sequence_rate();
    assert_ne!(rate.snap_nearest(target), target, "6 s is between frames at the demo's rate");
    let (r, o) = (d.flag(id), d.flag(other));
    let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.drag((x, y), o[0] + o[2] / 2.0 + 2.0 - x);
    assert_eq!(d.marker(id).start, rate.snap_nearest(target), "on the frame of the marker it snapped to");
}

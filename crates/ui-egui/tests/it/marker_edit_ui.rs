//! Headless UI tests of the Edit Marker dialog (#3023): double-clicking a marker on the Timeline
//! ruler opens it, OK applies the fields in one undo step, Cancel changes nothing.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::panels::timeline::{Drag, SNAP_PX};
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

    /// The screen rect of a Timeline element (`timeline.marker.<id>`, `timeline.markerEnd.<id>`,
    /// `timeline.workArea`): x, y, width, height.
    fn rect(&mut self, element: &str) -> [f64; 4] {
        let e = self.ok("ui.elements", json!({"prefix": element}));
        let r = e.as_array().unwrap().iter().find(|e| e["id"] == element).unwrap_or_else(|| panic!("no element {element}"))["rect"].clone();
        [0, 1, 2, 3].map(|i| r[i].as_f64().unwrap())
    }

    /// The centre of a Timeline element.
    fn center(&mut self, element: &str) -> (f64, f64) {
        let r = self.rect(element);
        (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    }

    /// The centre of a marker's start flag on the ruler.
    fn flag(&mut self, id: u64) -> (f64, f64) {
        self.center(&format!("timeline.marker.{id}"))
    }

    /// The time under a Timeline point, unsnapped, as the drag reads it.
    fn tick_at(&mut self, (x, y): (f64, f64)) -> Tick {
        Tick(self.ok("ui.timeline.hit", json!({"x": x, "y": y}))["time"].as_i64().unwrap())
    }

    /// What `ui.timeline.hit` reports under a point (`ruler`, `workArea`, ...).
    fn hit(&mut self, (x, y): (f64, f64)) -> Value {
        self.ok("ui.timeline.hit", json!({"x": x, "y": y}))["hit"].clone()
    }

    /// Where a marker at `start` lands, snapping off, when its flag is pressed at `from` and the
    /// pointer is at `to`: moved by the pointer's travel, on a frame, not before zero.
    fn moved(&mut self, start: Tick, from: (f64, f64), to: (f64, f64)) -> Tick {
        let rate = self.rate();
        let travel = self.tick_at(to) - rate.snap_nearest(self.tick_at(from));
        rate.snap_nearest(rate.snap_nearest(start + travel).max(Tick::ZERO))
    }

    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }

    fn button(&mut self, (x, y): (f64, f64), pressed: bool) {
        let pos = egui::pos2(x as f32, y as f32);
        self.send(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }

    fn move_to(&mut self, (x, y): (f64, f64)) {
        self.send(egui::Event::PointerMoved(egui::pos2(x as f32, y as f32)));
    }

    /// Presses the primary button at `from` and moves to `to` in 8 steps, still holding it.
    fn drag_to(&mut self, from: (f64, f64), to: (f64, f64)) {
        self.move_to(from);
        self.button(from, true);
        for k in 1..=8 {
            let f = f64::from(k) / 8.0;
            self.move_to((from.0 + (to.0 - from.0) * f, from.1 + (to.1 - from.1) * f));
        }
    }

    /// Releases the primary button at `at` and lets the release land.
    fn release(&mut self, at: (f64, f64)) {
        self.button(at, false);
        self.frames(2);
    }

    fn escape(&mut self) {
        self.send(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: egui::Modifiers::NONE });
        self.send(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: false, repeat: false, modifiers: egui::Modifiers::NONE });
    }

    /// The marker the ruler is dragging and where it shows it now, if a marker drag is on.
    fn dragged(&self) -> Option<(u64, Tick)> {
        match self.harness.state().tl.drag {
            Some(Drag::Marker { id, to, .. }) => Some((id.0, to)),
            _ => None,
        }
    }

    fn undo_len(&self) -> usize {
        self.harness.state().session.history.undo.len()
    }

    fn rate(&self) -> filmcraft_time::FrameRate {
        self.harness.state().session.sequence_rate()
    }

    fn playhead(&self) -> Tick {
        self.harness.state().session.playhead()
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

/// Any travel well past egui's click distance: what a drag of a flag moves by.
const TRAVEL: f64 = 120.0;

/// #4491: dragging a marker's flag on the ruler shows it at the pointer while the button is held,
/// then moves the marker there in one undo step, and leaves the playhead on the press (it no
/// longer scrubs along).
#[test]
fn dragging_a_marker_flag_moves_the_marker() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    let id = d.setup();
    let before = d.marker(id).start;
    let from = d.flag(id);
    let to = (from.0 + TRAVEL, from.1);
    let want = d.moved(before, from, to);
    assert_ne!(want, before, "the drag is long enough to move the marker");
    let undo_before = d.undo_len();
    d.drag_to(from, to);
    assert_eq!(d.dragged(), Some((id, want)), "mid-drag, the ruler shows the marker at the pointer");
    assert_eq!(d.marker(id).start, before, "nothing is committed before the release");
    let shown = d.flag(id);
    let px = d.tick_at((shown.0 + 1.0, shown.1)) - d.tick_at(shown);
    assert!((d.tick_at(shown) - want).0.abs() <= px.0, "the flag is drawn where the marker would land");
    d.release(to);
    assert_eq!(d.dragged(), None);
    assert_eq!(d.marker(id).start, want, "the marker lands where it was shown, on a frame");
    assert_eq!(d.playhead(), d.rate().snap(before), "the playhead stays on the press, not dragged along");
    assert_eq!(d.harness.state().session.history.undo.last().map(|u| u.0.as_str()), Some("Edit Markers"));
    assert_eq!(d.undo_len(), undo_before + 1, "one drag is one undo step");
}

/// #4491: Escape during a flag drag cancels it: the marker stays where it was and no undo step is
/// made, though the release comes after.
#[test]
fn escape_cancels_a_marker_drag() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    let id = d.setup();
    let before = d.marker(id);
    let from = d.flag(id);
    let to = (from.0 + TRAVEL, from.1);
    let undo_before = d.undo_len();
    d.drag_to(from, to);
    assert!(d.dragged().is_some_and(|(_, t)| t != before.start), "the drag is on and has moved the flag");
    d.escape();
    assert_eq!(d.dragged(), None, "Escape ends the drag");
    d.release(to);
    assert_eq!(d.marker(id), before, "the marker did not move");
    assert_eq!(d.undo_len(), undo_before, "no undo step");
}

/// #4491: a press on a flag without a drag only selects the marker: the playhead goes onto it
/// (#695), the marker stays and there is no undo step.
#[test]
fn a_click_on_a_marker_flag_parks_the_playhead_and_moves_nothing() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    let id = d.setup();
    let before = d.marker(id);
    assert_ne!(d.playhead(), d.rate().snap(before.start), "the playhead starts elsewhere");
    let at = d.flag(id);
    let undo_before = d.undo_len();
    d.move_to(at);
    d.button(at, true);
    d.release(at);
    assert_eq!(d.dragged(), None);
    assert_eq!(d.marker(id), before, "the marker stays");
    assert_eq!(d.playhead(), d.rate().snap(before.start), "the playhead is on the marker");
    assert_eq!(d.undo_len(), undo_before, "no undo step");
}

/// #4491: a drag on the ruler away from any flag still scrubs the playhead, and moves no marker.
#[test]
fn a_ruler_drag_away_from_a_flag_still_scrubs() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    let id = d.setup();
    let before = d.marker(id);
    let r = d.rect(&format!("timeline.marker.{id}"));
    // one flag width right of the flag, in the markers row, then away from it
    let from = (r[0] + 2.0 * r[2], r[1] + r[3] / 2.0);
    let to = (from.0 + TRAVEL, from.1);
    assert_eq!(d.hit(from), "ruler");
    let want = d.rate().snap_nearest(d.tick_at(to));
    let undo_before = d.undo_len();
    d.drag_to(from, to);
    assert!(matches!(d.harness.state().tl.drag, Some(Drag::Scrub { .. })), "a scrub, not a marker drag");
    assert_eq!(d.playhead(), want, "the playhead follows the pointer");
    d.release(to);
    assert_eq!(d.playhead(), want);
    assert_eq!(d.marker(id), before, "the marker did not move");
    assert_eq!(d.undo_len(), undo_before, "a scrub is no edit");
}

/// #4491: a range marker dragged by its start flag keeps its duration.
#[test]
fn a_range_marker_dragged_by_its_start_flag_keeps_its_duration() {
    drag_range_marker("timeline.marker.");
}

/// #4491: a range marker dragged by its end flag moves whole too, keeping its duration, as by its
/// start flag.
#[test]
fn a_range_marker_dragged_by_its_end_flag_moves_whole() {
    drag_range_marker("timeline.markerEnd.");
}

/// Drags a new range marker by the flag `prefix` names and checks it moved by the pointer's travel
/// with its duration, in one undo step.
fn drag_range_marker(prefix: &str) {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let id = d.exec("markers.addRange", json!({"frame": 48, "durationFrames": 24}))["marker"].as_u64().unwrap();
    d.frames(4);
    let before = d.marker(id);
    assert!(before.duration > Tick::ZERO);
    let from = d.center(&format!("{prefix}{id}"));
    let to = (from.0 + TRAVEL, from.1);
    let want = d.moved(before.start, from, to);
    let undo_before = d.undo_len();
    d.drag_to(from, to);
    assert_eq!(d.dragged(), Some((id, want)), "mid-drag, the marker is shown moved");
    d.release(to);
    let after = d.marker(id);
    assert_eq!((after.start, after.duration), (want, before.duration), "moved by the travel, with its duration");
    assert_eq!(d.undo_len(), undo_before + 1, "one drag is one undo step");
}

/// #4491: dragged left past the start of the sequence, a marker stops at zero, a frame.
#[test]
fn a_marker_dragged_left_past_zero_lands_at_zero() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    let id = d.setup();
    let from = d.flag(id);
    // the demo's "Open" marker sits at zero: one flag width left of it is before the sequence
    let zero = d.rect("timeline.marker.800000");
    let to = (zero[0] - zero[2], from.1);
    let px = d.tick_at((from.0 + 1.0, from.1)) - d.tick_at(from);
    let start = d.marker(id).start;
    assert!(((from.0 - to.0) * px.0 as f64) as i64 > start.0, "the pointer travels further left than the marker's start");
    let undo_before = d.undo_len();
    d.drag_to(from, to);
    assert_eq!(d.dragged(), Some((id, Tick::ZERO)), "mid-drag, the flag stops at zero");
    d.release(to);
    assert_eq!(d.marker(id).start, Tick::ZERO);
    assert_eq!(d.undo_len(), undo_before + 1);
}

/// #4491: the bottom of a flag reaches into the work area bar's row; a drag there moves the marker
/// by the pointer's travel in one undo step, not the bar. For the start flag of a point marker.
#[test]
fn a_marker_start_flag_over_the_work_area_bar_drags_the_marker() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    let id = d.setup();
    drag_over_work_area(&mut d, id, &format!("timeline.marker.{id}"));
}

/// #4491: as for a start flag, a range marker's end flag over the work area bar drags the marker,
/// not the bar.
#[test]
fn a_marker_end_flag_over_the_work_area_bar_drags_the_marker() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    d.ok("ui.set", json!({"focused": "Timeline"}));
    let id = d.exec("markers.addRange", json!({"frame": 48, "durationFrames": 24}))["marker"].as_u64().unwrap();
    d.frames(4);
    drag_over_work_area(&mut d, id, &format!("timeline.markerEnd.{id}"));
}

/// Shows the work area bar over the whole of marker `id`, drags `flag` from where it overlaps the
/// bar, and checks the marker moved by the travel and the bar did not.
fn drag_over_work_area(d: &mut Driver, id: u64, flag: &str) {
    let m = d.marker(id);
    let second = Tick::from_seconds_f64(1.0);
    d.exec("sequence.setWorkArea", json!({"start": (m.start - second).max(Tick::ZERO).0, "end": (m.start + m.duration + second + second).0}));
    d.frames(2);
    let area = d.harness.state().session.active_sequence().unwrap().work_area;
    let r = d.rect(flag);
    // 0.5 px above the flag's bottom: inside the bar's hit row too
    let from = (r[0] + r[2] / 2.0, r[1] + r[3] - 0.5);
    let bar = d.rect("timeline.workArea");
    assert!(from.1 >= bar[1] - 1.0 && from.0 > bar[0] && from.0 < bar[0] + bar[2], "the point is over the bar");
    assert_eq!(d.hit(from), "ruler", "the flag wins over the bar");
    let to = (from.0 + TRAVEL, from.1);
    let want = d.moved(m.start, from, to);
    let undo_before = d.undo_len();
    d.drag_to(from, to);
    assert_eq!(d.dragged(), Some((id, want)), "mid-drag, a marker drag");
    d.release(to);
    let after = d.marker(id);
    assert_eq!((after.start, after.duration), (want, m.duration), "moved by the travel");
    assert_eq!(d.harness.state().session.active_sequence().unwrap().work_area, area, "the bar stayed");
    assert_eq!(d.undo_len(), undo_before + 1, "one drag is one undo step");
}

/// #4491: snapped onto another marker that starts between frames, the dragged marker lands on that
/// marker's frame, not between frames.
#[test]
fn a_marker_snapped_to_an_off_frame_marker_lands_on_a_frame() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": true}));
    let id = d.setup();
    let target = Tick::from_seconds_f64(6.0);
    let other = d.exec("markers.add", json!({"time": target.0}))["marker"].as_u64().unwrap();
    d.frames(2);
    let rate = d.rate();
    assert_ne!(rate.snap_nearest(target), target, "6 s is between frames at the demo's rate");
    let (from, o) = (d.flag(id), d.flag(other));
    // half the snap distance past the other marker: in its reach
    let to = (o.0 + f64::from(SNAP_PX) / 2.0, from.1);
    d.drag_to(from, to);
    assert_eq!(d.dragged(), Some((id, rate.snap_nearest(target))), "mid-drag, shown snapped");
    d.release(to);
    assert_eq!(d.marker(id).start, rate.snap_nearest(target), "on the frame of the marker it snapped to");
}

/// #4491: brought back near where it started, a dragged marker still snaps to another marker in
/// reach, though its own old start is nearer: the marker is not a snap target of its own drag.
#[test]
fn a_dragged_marker_snaps_past_its_own_start() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": true}));
    let id = d.setup();
    let from = d.flag(id);
    let snap = f64::from(SNAP_PX);
    // the other marker 0.85 of the snap distance right of the flag; the drop 0.25 of it right, so
    // nearer the flag's old start (0.25) than the other marker (0.6), both in reach
    let near = d.tick_at((from.0 + 0.85 * snap, from.1));
    let px = d.tick_at((from.0 + 1.0, from.1)) - d.tick_at(from);
    // landing on frames moves each by up to half a frame: the 0.35 gap and the 0.4 to spare hold
    let frame_px = d.rate().frame_duration().0 as f64 / px.0 as f64;
    assert!(frame_px < 0.2 * snap, "a frame ({frame_px} px) is narrower than the margins");
    let other = d.exec("markers.add", json!({"time": near.0}))["marker"].as_u64().unwrap();
    d.frames(2);
    let want = d.rate().snap_nearest(d.marker(other).start);
    let click = f64::from(d.harness.ctx.options(|o| o.input_options.max_click_dist));
    // out past the click distance and both snap reaches so the drag starts, then back
    let out = (from.0 + 2.0 * (click + snap), from.1);
    let back = (from.0 + 0.25 * snap, from.1);
    d.drag_to(from, out);
    d.move_to(back);
    assert_eq!(d.dragged(), Some((id, want)), "mid-drag, shown on the other marker");
    d.release(back);
    assert_eq!(d.marker(id).start, want, "on the other marker's frame");
}

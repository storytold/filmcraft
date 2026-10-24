//! Headless UI tests of the Edit Marker dialog (#2416).

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
        // a real frame step, so the two clicks of a double-click fall within its delay
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

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    /// A double-click, after long enough that egui does not count earlier clicks into a triple-click.
    fn double_click(&mut self, id: &str) {
        self.frames(60);
        self.ok("ui.click", json!({"id": id, "count": 2}));
        self.frames(2);
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

/// #2416: Edit Marker… (a marker's menu on the ruler or in the Markers panel, or a double-click on
/// either) sets name, comment, duration, colour and type in one undo step; Cancel and Escape change
/// nothing.
#[test]
fn edit_marker_dialog_edits_a_marker_in_one_undo_step() {
    use filmcraft_engine::project::{Label, MarkerKind};
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let id = d.exec("markers.add", json!({"frame": 30}))["marker"].as_u64().unwrap();
    let marker = |d: &mut Driver| d.app().session.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == id).unwrap().clone();
    let before = marker(&mut d);
    let undo = d.app().session.history.undo.len();
    d.ok("ui.click", json!({"id": format!("timeline.marker.{id}"), "button": "right"}));
    d.frames(3);
    d.click("timeline.markerMenu.edit");
    assert!(d.app().ui.marker_dialog.is_some(), "Edit Marker… opens the dialog");
    d.click("markerDialog.name");
    d.ok("ui.type", json!({"text": "Intro"}));
    d.click("markerDialog.comment");
    d.ok("ui.type", json!({"text": "tighten"}));
    d.click("markerDialog.duration");
    d.ok("ui.key", json!({"key": "End"}));
    for _ in 0..2 {
        d.ok("ui.key", json!({"key": "Backspace"}));
    }
    d.ok("ui.type", json!({"text": "12"}));
    d.click("markerDialog.color.Blue");
    d.click("markerDialog.kind.chapter");
    d.click("markerDialog.ok");
    assert!(d.app().ui.marker_dialog.is_none(), "OK closes it");
    let m = marker(&mut d);
    assert_eq!((m.name.as_str(), m.comment.as_str(), m.color, m.kind), ("Intro", "tighten", Label::Blue, MarkerKind::Chapter));
    assert_eq!(m.duration, d.app().session.sequence_rate().tick_of(12));
    assert_eq!(d.app().session.history.undo.len(), undo + 1, "one undo step");
    d.exec("edit.undo", json!({}));
    assert_eq!(marker(&mut d), before);

    // a double-click on the ruler marker opens it too; Cancel drops what was typed
    d.double_click(&format!("timeline.marker.{id}"));
    assert!(d.app().ui.marker_dialog.is_some(), "double-clicking a ruler marker opens the dialog");
    d.click("markerDialog.name");
    d.ok("ui.type", json!({"text": "dropped"}));
    d.click("markerDialog.cancel");
    assert!(d.app().ui.marker_dialog.is_none());
    assert_eq!(marker(&mut d), before, "Cancel changes nothing");

    // the Markers panel: a double-click on the row, and Edit Marker… in its menu; Escape cancels
    d.ok("ui.panel.show", json!({"panel": "Markers"}));
    d.frames(3);
    d.double_click(&format!("markers.row.{id}"));
    assert!(d.app().ui.marker_dialog.is_some(), "double-clicking a Markers panel row opens the dialog");
    d.click("markerDialog.cancel");
    d.ok("ui.click", json!({"id": format!("markers.row.{id}"), "button": "right"}));
    d.frames(2);
    d.click("markers.menu.edit");
    assert!(d.app().ui.marker_dialog.is_some(), "the row's Edit Marker… opens the dialog");
    d.click("markerDialog.kind.webLink");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    assert!(d.app().ui.marker_dialog.is_none(), "Escape closes it");
    assert_eq!(marker(&mut d), before, "Escape changes nothing");
}

/// #2470 review: OK used to send the duration as the dialog showed it, in whole frames, so renaming
/// a marker whose duration is part of a frame changed its length (or made a range marker a point).
#[test]
fn renaming_a_marker_leaves_a_duration_it_cannot_show_exactly() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    let frame = d.app().session.sequence_rate().tick_of(1).0;
    let id = d.exec("markers.addRange", json!({"frame": 30, "duration": frame / 2}))["marker"].as_u64().unwrap();
    let duration = |d: &mut Driver| d.app().session.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == id).unwrap().duration;
    let before = duration(&mut d);
    assert!(before.0 > 0 && before.0 < frame);
    d.ok("ui.panel.show", json!({"panel": "Markers"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": format!("markers.row.{id}"), "button": "right"}));
    d.frames(2);
    d.click("markers.menu.edit");
    d.click("markerDialog.name");
    d.ok("ui.type", json!({"text": "Renamed"}));
    d.click("markerDialog.ok");
    assert!(d.app().ui.marker_dialog.is_none());
    let m = d.app().session.active_sequence().unwrap().markers.iter().find(|m| m.id.0 == id).unwrap().clone();
    assert_eq!((m.name.as_str(), m.duration), ("Renamed", before), "the name changes, the duration stays");
}

/// #2470 review: a dialog left open while another project opens (or the same one again) closes
/// instead of editing a marker of that project that happens to have the same id.
#[test]
fn the_dialog_closes_when_another_project_opens() {
    let mut d = Driver::demo();
    let id = d.exec("markers.add", json!({"frame": 30}))["marker"].as_u64().unwrap();
    d.ok("ui.click", json!({"id": format!("timeline.marker.{id}"), "button": "right"}));
    d.frames(3);
    d.click("timeline.markerMenu.edit");
    assert!(d.app().ui.marker_dialog.is_some());
    // the demo again: the same sequence and marker ids, but another project
    d.exec("file.openDemoProject", json!({}));
    let id_again = d.exec("markers.add", json!({"frame": 30}))["marker"].as_u64().unwrap();
    assert_eq!(id_again, id, "the reopened project has a marker with the same id");
    d.frames(2);
    assert!(d.app().ui.marker_dialog.is_none(), "another project: the dialog closes");
}

//! The track header's context menu (#1117): right-clicking a track header offers Add Track, Add
//! Tracks…, Delete Track and Delete Tracks…, next to the Sequence menu's dialogs.

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
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                self.frames(2);
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
}

fn has(d: &mut Driver, id: &str) -> bool {
    let v = d.ok("ui.elements", json!({"prefix": id}));
    v.as_array().unwrap().iter().any(|e| e["id"] == id)
}

fn rect(d: &mut Driver, id: &str) -> [f64; 4] {
    let v = d.ok("ui.elements", json!({"prefix": id}));
    let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
    let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    [r[0], r[1], r[2], r[3]]
}

fn counts(d: &Driver) -> (usize, usize) {
    let q = d.harness.state().session.active_sequence().unwrap();
    (q.video_tracks.len(), q.audio_tracks.len())
}

/// A point on track `label`'s header, clear of its buttons: just left of the first one.
fn header_point(d: &mut Driver, label: &str) -> (f64, f64) {
    let patch = rect(d, &format!("timeline.track.{label}.sourcePatch"));
    (patch[0] - 4.0, patch[1] + patch[3] / 2.0)
}

#[test]
fn a_track_header_menu_adds_and_deletes_tracks() {
    let mut d = Driver::demo();
    let (v0, a0) = counts(&d);
    let (x, y) = header_point(&mut d, "V1");
    d.ok("ui.click", json!({"x": x, "y": y, "button": "right"}));
    for item in ["addTrack", "addTracks", "deleteTrack", "deleteTracks"] {
        assert!(has(&mut d, &format!("timeline.track.V1.menu.{item}")), "{item} is offered");
    }
    // Add Track puts one video track after V1
    let (v1, v2) = {
        let q = d.harness.state().session.active_sequence().unwrap();
        (q.video_tracks[0].id, q.video_tracks[1].id)
    };
    d.ok("ui.click", json!({"id": "timeline.track.V1.menu.addTrack"}));
    d.frames(2);
    assert_eq!(counts(&d), (v0 + 1, a0));
    let q = d.harness.state().session.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].id, v1, "V1 stays first");
    assert_eq!(q.video_tracks[2].id, v2, "the old V2 is pushed to the third place, the new track sits between");
    // Delete Track on an audio header removes that track
    let a2 = d.harness.state().session.active_sequence().unwrap().audio_tracks[1].id;
    let (x, y) = header_point(&mut d, "A2");
    d.ok("ui.click", json!({"x": x, "y": y, "button": "right"}));
    d.ok("ui.click", json!({"id": "timeline.track.A2.menu.deleteTrack"}));
    d.frames(2);
    assert_eq!(counts(&d), (v0 + 1, a0 - 1));
    assert!(d.harness.state().session.active_sequence().unwrap().track(a2).is_none(), "A2 itself is gone");
    // the strip at the right of a tall header opens it too
    let lock = rect(&mut d, "timeline.track.V1.locked");
    let tracks = rect(&mut d, "timeline.tracks");
    d.ok("ui.click", json!({"x": tracks[0] - 5.0, "y": lock[1] + lock[3] / 2.0, "button": "right"}));
    assert!(has(&mut d, "timeline.track.V1.menu.deleteTracks"));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    // the dialogs open from the same menu
    let (x, y) = header_point(&mut d, "V1");
    d.ok("ui.click", json!({"x": x, "y": y, "button": "right"}));
    d.ok("ui.click", json!({"id": "timeline.track.V1.menu.addTracks"}));
    d.frames(3);
    assert!(d.harness.state().dialog.is_some(), "Add Tracks… opens its dialog");
}

#[test]
fn the_empty_part_of_the_header_column_offers_the_dialogs() {
    let mut d = Driver::demo();
    // short tracks leave room above the top video track: the header column has no track there
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 30.0}}));
    d.frames(4);
    let n = d.harness.state().session.active_sequence().unwrap().video_tracks.len();
    let top = rect(&mut d, &format!("timeline.track.V{n}.sourcePatch"));
    let (x, y) = (top[0] + 30.0, top[1] - 25.0);
    d.ok("ui.click", json!({"x": x, "y": y, "button": "right"}));
    assert!(has(&mut d, "timeline.tracks.menu.addTracks"), "Add Tracks… is offered");
    assert!(has(&mut d, "timeline.tracks.menu.deleteTracks"));
    assert!(!has(&mut d, &format!("timeline.track.V{n}.menu.addTrack")), "not a track's menu");
    d.ok("ui.click", json!({"id": "timeline.tracks.menu.addTracks"}));
    d.frames(3);
    assert!(d.harness.state().dialog.is_some(), "the dialog opens");
}

/// A right-click on one of the header's buttons opens the same menu, and the button keeps its state.
#[test]
fn a_right_click_on_a_header_button_opens_the_menu() {
    let mut d = Driver::demo();
    let before = serde_json::to_value(d.harness.state().session.active_sequence().unwrap().video_tracks[0].clone()).unwrap();
    for (label, button) in [("V1", "sourcePatch"), ("V1", "locked"), ("V1", "target"), ("V1", "syncLock"), ("V1", "enabled"), ("A1", "muted"), ("A1", "solo")] {
        let r = rect(&mut d, &format!("timeline.track.{label}.{button}"));
        d.ok("ui.click", json!({"x": r[0] + r[2] / 2.0, "y": r[1] + r[3] / 2.0, "button": "right"}));
        assert!(has(&mut d, &format!("timeline.track.{label}.menu.addTrack")), "a right-click on {button} opens the menu");
        d.ok("ui.key", json!({"key": "Escape"}));
        d.frames(2);
        assert!(!has(&mut d, &format!("timeline.track.{label}.menu.addTrack")), "Escape closes it");
    }
    let q = d.harness.state().session.active_sequence().unwrap();
    assert_eq!(serde_json::to_value(q.video_tracks[0].clone()).unwrap(), before, "no button toggled");
    assert!(!q.audio_tracks[0].muted && !q.audio_tracks[0].solo);
}

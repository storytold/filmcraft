//! Headless UI tests of the Sequence / Markers menu additions: the Delete Tracks dialog, the
//! through-edit marks on the timeline, the Markers panel colour filter and the Shift+; gap key.
//! The real `FilmcraftApp` under `egui_kittest`, driven over the control channel by automation id.

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
}

#[test]
fn delete_tracks_dialog_deletes_empty_tracks() {
    let mut d = Driver::demo();
    let before = d.exec("sequence.inspect", json!({}));
    let nv = before["video"].as_array().unwrap().len();
    let na = before["audio"].as_array().unwrap().len();
    let r = d.ok("ui.menu.invoke", json!({"id": "sequence.deleteTracks"}));
    assert_eq!(r["dialog"], "deleteTracks", "{r}");
    d.frames(3);
    let ids = d.ids("deleteTracks.");
    for id in ["deleteTracks.video", "deleteTracks.audio", "deleteTracks.video.target", "deleteTracks.audio.target", "deleteTracks.ok", "deleteTracks.cancel"] {
        assert!(ids.iter().any(|i| i == id), "{id} missing: {ids:?}");
    }
    d.click("deleteTracks.video");
    d.click("deleteTracks.audio");
    d.click("deleteTracks.ok");
    assert!(d.ids("deleteTracks.").is_empty(), "dialog closed");
    let after = d.exec("sequence.inspect", json!({}));
    let (nv2, na2) = (after["video"].as_array().unwrap().len(), after["audio"].as_array().unwrap().len());
    assert!(nv2 < nv && na2 < na, "empty tracks deleted: {nv}->{nv2}, {na}->{na2}");
    // Cancel leaves the sequence alone
    d.ok("ui.menu.invoke", json!({"id": "sequence.deleteTracks"}));
    d.frames(3);
    d.click("deleteTracks.video");
    d.click("deleteTracks.cancel");
    assert_eq!(d.exec("sequence.inspect", json!({}))["video"].as_array().unwrap().len(), nv2);
}

#[test]
fn through_edits_are_marked_on_the_timeline() {
    let mut d = Driver::demo();
    d.exec("playhead.set", json!({"frame": 30}));
    d.exec("sequence.addEditAllTracks", json!({}));
    d.frames(3);
    assert!(d.ids("timeline.throughEdit.").is_empty(), "hidden until Show Through Edits");
    d.ok("ui.menu.invoke", json!({"id": "sequence.showThroughEdits"}));
    d.frames(3);
    let marks = d.ids("timeline.throughEdit.");
    assert!(marks.len() >= 2, "V1 + A1 marks: {marks:?}");
    d.exec("sequence.joinThroughEdits", json!({"all": true}));
    d.frames(3);
    assert!(d.ids("timeline.throughEdit.").is_empty(), "joined");
}

#[test]
fn markers_panel_colour_filter() {
    let mut d = Driver::demo();
    d.exec("markers.clearAll", json!({}));
    d.exec("markers.add", json!({"frame": 10, "name": "green"}));
    d.exec("markers.add", json!({"frame": 20, "name": "red", "color": "Rose"}));
    d.ok("ui.panel.show", json!({"panel": "Markers"}));
    d.frames(3);
    assert_eq!(d.ids("markers.row.").len(), 2);
    assert!(d.ids("markers.filter.").len() >= 7);
    d.click("markers.filter.Rose");
    d.frames(2);
    assert_eq!(d.ids("markers.row.").len(), 1, "red hidden");
    let m = d.ok("ui.menu.list", json!({}));
    let show_all = m.as_array().unwrap().iter().find(|i| i["id"] == "markers.showAllMarkerColors").unwrap().clone();
    assert_eq!(show_all["enabled"], json!(true));
    d.ok("ui.menu.invoke", json!({"id": "markers.showAllMarkerColors"}));
    d.frames(3);
    assert_eq!(d.ids("markers.row.").len(), 2);
}

#[test]
fn shift_semicolon_goes_to_the_next_gap() {
    let mut d = Driver::demo();
    d.exec("markers.markIn", json!({"frame": 48}));
    d.exec("markers.markOut", json!({"frame": 71}));
    d.exec("sequence.lift", json!({}));
    d.exec("playhead.set", json!({"frame": 0}));
    // the key a US layout sends for Shift+; is `:`
    d.harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Colon,
        physical_key: Some(egui::Key::Semicolon),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    d.frames(3);
    let ph = d.exec("sequence.inspect", json!({}))["playhead"].clone();
    let want = d.exec("playhead.set", json!({"frame": 48}))["time"].clone();
    assert_eq!(ph, want);
}

fn first_movie(v: &Value) -> Option<u64> {
    match v {
        Value::Object(m) => {
            if let Some(id) = m.get("item").and_then(Value::as_u64)
                && m.get("type").and_then(Value::as_str) == Some("Movie")
            {
                return Some(id);
            }
            m.values().find_map(first_movie)
        }
        Value::Array(a) => a.iter().find_map(first_movie),
        _ => None,
    }
}

/// On-screen x of a clip's in and out edges in the timeline (not clipped to the panel).
fn clip_span(d: &mut Driver, clip: u64) -> (f64, f64) {
    let x = |d: &mut Driver, edge: &str| d.ok("ui.timeline.locate", json!({"clip": clip, "edge": edge}))["x"].as_f64().expect("x");
    (x(d, "in"), x(d, "out"))
}

#[test]
fn new_empty_sequence_fits_its_first_clip() {
    let mut d = Driver::demo();
    let project = d.exec("project.inspect", json!({}));
    let item = first_movie(&project).expect("a movie in the demo project");
    // an empty sequence fits its 1 s minimum; the clip placed into it afterwards is fitted again
    d.exec("file.newSequence", json!({"name": "Empty"}));
    d.frames(30);
    let clip = d.exec("timeline.place", json!({"item": item, "track": "V1", "seconds": 0}))["clips"][0].as_u64().expect("clip");
    d.frames(60);
    let (x_in, x_out) = clip_span(&mut d, clip);
    assert!(x_out < 1600.0 && x_out - x_in > 500.0, "clip spans x {x_in}..{x_out}: not fitted to the 1600 px window");

    // a zoom chosen while the sequence is still empty is kept
    d.exec("file.newSequence", json!({"name": "Zoomed"}));
    d.frames(30);
    d.ok("ui.set", json!({"timeline": {"pps": 300.0}}));
    d.frames(30);
    let clip = d.exec("timeline.place", json!({"item": item, "track": "V1", "seconds": 0}))["clips"][0].as_u64().expect("clip");
    d.frames(60);
    let secs = d.exec("sequence.inspect", json!({}))["video"][0]["items"][0]["duration"].as_f64().expect("duration") / 254_016_000_000.0;
    let (x_in, x_out) = clip_span(&mut d, clip);
    assert!(((x_out - x_in) - (secs * 300.0 - 4.0)).abs() < 2.0, "zoom not kept: {} px for {secs} s at 300 px/s", x_out - x_in);
}

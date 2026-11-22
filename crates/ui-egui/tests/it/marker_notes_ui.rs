//! Headless UI tests of Marker Notes (#5175): while the Program playhead is parked on a sequence
//! marker, its name and notes are written over the picture; the wrench menu turns them off.

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

    /// A marker `frames` long (0 = a point marker) at sequence frame `at`.
    fn marker(&mut self, at: i64, frames: i64, name: &str, comment: &str) -> u64 {
        let rate = self.harness.state().session.sequence_rate();
        let p = json!({"time": rate.tick_of(at).0, "name": name, "comment": comment, "durationFrames": frames});
        self.exec("markers.add", p)["marker"].as_u64().expect("marker id")
    }

    /// Parks the playhead on sequence frame `frame` and returns the notes shown over the picture.
    fn notes_at(&mut self, frame: i64) -> Vec<(String, String, [f64; 4])> {
        self.exec("playhead.set", json!({"frame": frame}));
        self.frames(3);
        let picture = self.ok("ui.elements", json!({"prefix": "program.picture"}));
        let picture = rect(&picture.as_array().expect("elements")[0]);
        let notes = self.ok("ui.elements", json!({"prefix": "program.markerNotes."}));
        notes
            .as_array()
            .expect("elements")
            .iter()
            .map(|e| {
                let r = rect(e);
                assert!(
                    r[0] >= picture[0] && r[1] >= picture[1] && r[0] + r[2] <= picture[0] + picture[2] + 0.5 && r[1] + r[3] <= picture[1] + picture[3] + 0.5,
                    "the notes stay inside the picture: {r:?} in {picture:?}"
                );
                (e["id"].as_str().unwrap_or_default().to_owned(), e["label"].as_str().unwrap_or_default().to_owned(), r)
            })
            .collect()
    }
}

fn rect(e: &Value) -> [f64; 4] {
    let r = e["rect"].as_array().expect("rect");
    [0, 1, 2, 3].map(|i| r[i].as_f64().unwrap_or_default())
}

#[test]
fn the_notes_of_the_marker_under_the_parked_playhead_are_written_over_the_picture() {
    let mut d = Driver::demo();
    let point = d.marker(48, 0, "Fix caption", "Typo in the second line");
    let range = d.marker(96, 24, "", "Trim this shot");
    d.marker(150, 0, "", "");

    let shown = d.notes_at(48);
    assert_eq!(shown.len(), 1, "one marker under the playhead: {shown:?}");
    assert_eq!(shown[0].0, format!("program.markerNotes.{point}"));
    assert!(shown[0].1.contains("Fix caption") && shown[0].1.contains("Typo in the second line"), "name and notes: {}", shown[0].1);
    assert!(d.notes_at(49).is_empty(), "gone one frame after a point marker");

    for frame in [96, 108, 119] {
        let shown = d.notes_at(frame);
        assert!(shown.len() == 1 && shown[0].1.contains("Trim this shot"), "inside the range marker at {frame}: {shown:?}");
        assert_eq!(shown[0].0, format!("program.markerNotes.{range}"));
    }
    assert!(d.notes_at(120).is_empty(), "gone after the range marker's last frame");
    assert!(d.notes_at(150).is_empty(), "a marker with no name or notes has nothing to show");

    // coincident markers are stacked, not drawn over each other
    d.marker(48, 0, "Second note", "");
    let shown = d.notes_at(48);
    assert_eq!(shown.len(), 2, "both coincident markers: {shown:?}");
    let (a, b) = (shown[0].2, shown[1].2);
    assert!(a[1] + a[3] <= b[1] || b[1] + b[3] <= a[1], "the plates do not overlap: {a:?} {b:?}");
}

#[test]
fn the_wrench_menu_turns_marker_notes_off_and_on() {
    let mut d = Driver::demo();
    d.marker(48, 0, "Fix caption", "");
    assert_eq!(d.notes_at(48).len(), 1, "on by default");

    d.ok("ui.click", json!({"id": "program.settings"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "program.settings.markerNotes"}));
    d.frames(2);
    assert!(d.notes_at(48).is_empty(), "turned off from the wrench menu");

    d.ok("ui.menu.invoke", json!({"id": "view.markerNotes", "params": {"enabled": true}}));
    assert_eq!(d.notes_at(48).len(), 1, "turned back on through automation");
}

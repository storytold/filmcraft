//! A transition on a compact track fills the track's height: below the clip's name strip on a
//! tall track, from its top on a short one, so it can be read and grabbed.

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

fn rect(d: &mut Driver, id: &str) -> [f64; 4] {
    let v = d.ok("ui.elements", json!({"prefix": id}));
    let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
    let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    [r[0], r[1], r[2], r[3]]
}

#[test]
fn a_transition_fills_a_compact_track_and_sits_below_the_name_on_a_tall_one() {
    let mut d = Driver::demo();
    let id = d.harness.state().session.active_sequence().unwrap().video_tracks[0].transitions[0].id.0;
    let (name, row) = (format!("timeline.transition.{id}"), "timeline.track.V1.sourcePatch");
    // tall: a strip for the clip's name above it
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 96.0}}));
    d.frames(4);
    let (t, r) = (rect(&mut d, &name), rect(&mut d, row));
    assert!(t[1] - r[1] >= 15.0, "tall: the transition starts below the name strip ({t:?} in {r:?})");
    // compact: it fills the track
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 30.0}}));
    d.frames(4);
    let (t, r) = (rect(&mut d, &name), rect(&mut d, row));
    assert!(t[1] - r[1] <= 2.0, "compact: it starts at the top of the track ({t:?} in {r:?})");
    assert!(t[3] >= r[3] - 3.0, "and is as tall as the track: {} of {}", t[3], r[3]);
    // and a press on its upper part grabs the transition, not the clip under it
    let hit = d.ok("ui.timeline.hit", json!({"x": t[0] + t[2] / 2.0, "y": t[1] + 4.0}));
    assert_eq!(hit["hit"], "transition", "{hit}");
}

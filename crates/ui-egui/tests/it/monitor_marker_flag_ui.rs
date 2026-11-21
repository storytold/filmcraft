//! #4324: a flag beside the monitor's timecode while the playhead is on a marker, so an editor
//! cutting to music sees the hit while watching the picture, without looking at the Timeline ruler.

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

    fn has(&mut self, id: &str) -> bool {
        self.ok("ui.elements", json!({"prefix": id})).as_array().is_some_and(|v| v.iter().any(|e| e["id"] == id))
    }

    fn marker(&mut self, params: Value) -> u64 {
        self.exec("markers.add", params)["marker"].as_u64().expect("a marker id")
    }
}

/// Program monitor: the flag appears on a marker's frame, both ends of a range marker, coincident
/// markers all show, and nothing is left as soon as the playhead moves off.
#[test]
fn program_monitor_flags_the_marker_under_the_playhead() {
    let mut d = Driver::demo();
    let rate = d.harness.state().session.sequence_rate();
    let at = rate.tick_of(48);
    let beat = d.marker(json!({"time": at.0, "name": "Beat"}));
    let beat_flag = format!("program.markerFlag.{beat}");
    d.exec("playhead.set", json!({"time": at.0}));
    d.frames(3);
    assert!(d.has(&beat_flag), "the flag shows on the marker's frame");
    // one frame later it is gone
    d.exec("playhead.set", json!({"time": rate.tick_of(49).0}));
    d.frames(3);
    assert!(!d.has(&beat_flag), "the flag clears as soon as the playhead moves off");

    // A range marker flags its start and its end (start + duration).
    let range = d.marker(json!({"time": at.0, "durationFrames": 10, "name": "Range"}));
    let range_flag = format!("program.markerFlag.{range}");
    d.exec("playhead.set", json!({"time": at.0}));
    d.frames(3);
    assert!(d.has(&beat_flag) && d.has(&range_flag), "coincident markers all show");
    d.exec("playhead.set", json!({"time": rate.tick_of(58).0}));
    d.frames(3);
    assert!(d.has(&range_flag), "the end of a range marker flags too");
    assert!(!d.has(&beat_flag), "a marker that ended earlier does not");

    // A marker on another frame does not light up at this one.
    let other = d.marker(json!({"time": rate.tick_of(72).0, "name": "Later"}));
    assert!(!d.has(&format!("program.markerFlag.{other}")), "a marker elsewhere stays hidden");
}

/// The Source monitor shows the same flag for its clip's markers.
#[test]
fn source_monitor_flags_the_marker_under_the_source_playhead() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "Source"}));
    d.frames(3);
    let item = d.exec("file.newColorMatte", json!({"color": "#3366ff", "width": 64, "height": 48, "seconds": 4.0}))["item"].as_u64().expect("an item");
    d.exec("source.open", json!({"item": item}));
    let rate = d.harness.state().session.sequence_rate();
    let at = rate.tick_of(12);
    let id = d.marker(json!({"target": "source", "time": at.0, "name": "Clip beat"}));
    let flag = format!("source.markerFlag.{id}");
    d.exec("source.setPlayhead", json!({"time": at.0}));
    d.frames(3);
    assert!(d.has(&flag), "the Source monitor flags its clip's marker");
    d.exec("source.setPlayhead", json!({"time": rate.tick_of(13).0}));
    d.frames(3);
    assert!(!d.has(&flag), "and clears when the source playhead moves off");
}

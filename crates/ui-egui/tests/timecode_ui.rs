//! The Timeline's and monitors' current-time timecodes, driven headless through the control
//! channel: dragging scrubs the playhead (1 frame per point, never before the start), a click
//! turns the timecode into a field where Enter goes to the typed time and Escape cancels.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::{FrameRate, parse_timecode};
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
        session.execute("source.open", json!({"item": 5})).expect("source clip");
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
    /// Centre of an element, from `ui.elements`.
    fn centre(&mut self, id: &str) -> (f64, f64) {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    }
    fn label(&mut self, id: &str) -> String {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().find(|e| e["id"] == id).and_then(|e| e["label"].as_str()).unwrap_or_default().to_string()
    }
    fn drag_by(&mut self, id: &str, dx: f64) {
        let (x, y) = self.centre(id);
        self.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + dx, "y": y}}));
    }
    fn type_into(&mut self, id: &str, text: &str, key: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.ok("ui.type", json!({"text": text}));
        self.ok("ui.key", json!({"key": key}));
    }
    fn frame(&self) -> i64 {
        let s = &self.harness.state().session;
        s.sequence_rate().frame_at(s.playhead())
    }
    fn source_rate(&self) -> FrameRate {
        filmcraft_engine::clip_ops::source_view(&self.harness.state().session, filmcraft_project::ItemId(5)).unwrap().rate
    }
    fn source_frame(&self) -> i64 {
        self.source_rate().frame_at(self.harness.state().session.state.source_playhead)
    }
    fn set_frame(&mut self, f: i64) {
        self.harness.state_mut().session.execute("playhead.set", json!({"frame": f})).unwrap();
        self.frames(2);
    }
}

#[test]
fn dragging_the_timeline_timecode_scrubs_and_stops_at_the_start() {
    let mut d = Driver::demo();
    let before = d.harness.state().session.project.to_json();
    d.set_frame(30);
    d.drag_by("timeline.timecode", 20.0);
    assert_eq!(d.frame(), 50, "1 frame per point");
    d.drag_by("timeline.timecode", -12.0);
    assert_eq!(d.frame(), 38);
    d.drag_by("timeline.timecode", -300.0);
    assert_eq!(d.frame(), 0, "never before the start");
    assert_eq!(d.harness.state().session.project.to_json(), before, "scrubbing does not edit the project");
}

#[test]
fn typing_in_the_timeline_timecode_goes_to_that_time() {
    let mut d = Driver::demo();
    d.set_frame(30);
    d.type_into("timeline.timecode", "1000", "Enter");
    let (rate, df) = {
        let s = &d.harness.state().session;
        (s.sequence_rate(), s.active_sequence().unwrap().settings.drop_frame)
    };
    assert_eq!(d.frame(), parse_timecode("1000", rate, df, 0).unwrap(), "the whole value was selected and replaced");
    d.type_into("timeline.timecode", "+15", "Enter");
    assert_eq!(d.frame(), parse_timecode("1000", rate, df, 0).unwrap() + 15, "relative to the current time");
}

#[test]
fn arrow_keys_move_the_caret_instead_of_committing() {
    let mut d = Driver::demo();
    let (rate, df) = {
        let s = &d.harness.state().session;
        (s.sequence_rate(), s.active_sequence().unwrap().settings.drop_frame)
    };
    // Left on the selected value lands before its last digit: Delete removes that digit
    d.set_frame(30);
    let tc = d.label("timeline.timecode");
    d.ok("ui.click", json!({"id": "timeline.timecode"}));
    d.ok("ui.key", json!({"key": "Left"}));
    assert_eq!(d.frame(), 30, "Left neither commits nor steps the playhead");
    d.ok("ui.key", json!({"key": "Delete"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    let mut expect = tc.clone();
    expect.pop();
    assert_eq!(d.frame(), parse_timecode(&expect, rate, df, 0).unwrap(), "{tc} without its last digit");
    // Right goes to the end: Backspace removes the last digit, then a new one is typed
    d.set_frame(30);
    d.ok("ui.click", json!({"id": "timeline.timecode"}));
    d.ok("ui.key", json!({"key": "Right"}));
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.ok("ui.type", json!({"text": "9"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    let mut expect = tc.clone();
    expect.pop();
    expect.push('9');
    assert_eq!(d.frame(), parse_timecode(&expect, rate, df, 0).unwrap(), "{tc} with its last digit replaced");
}

#[test]
fn escape_cancels_and_a_bad_time_is_reported_without_moving() {
    let mut d = Driver::demo();
    d.set_frame(30);
    d.type_into("timeline.timecode", "1000", "Escape");
    assert_eq!(d.frame(), 30, "Escape cancels");
    d.type_into("timeline.timecode", "abc", "Enter");
    assert_eq!(d.frame(), 30);
    assert!(d.harness.state().ui.status.contains("timecode"), "status: {}", d.harness.state().ui.status);
    // after a cancel or an error the readout scrubs again
    d.drag_by("timeline.timecode", 10.0);
    assert_eq!(d.frame(), 40);
}

#[test]
fn the_program_and_source_timecodes_move_their_own_playheads() {
    let mut d = Driver::demo();
    d.set_frame(30);
    d.drag_by("program.timecode", 20.0);
    assert_eq!(d.frame(), 50);
    let program = d.harness.state().session.playhead();
    let source = d.source_frame();
    d.drag_by("source.timecode", 20.0);
    assert_eq!(d.source_frame(), source + 20);
    assert_eq!(d.harness.state().session.playhead(), program, "the Source timecode leaves the sequence alone");
    d.type_into("source.timecode", "100", "Enter");
    assert_eq!(d.source_frame(), parse_timecode("100", d.source_rate(), false, 0).unwrap(), "1 s at the clip's own rate");
    assert_eq!(d.harness.state().session.playhead(), program);
}

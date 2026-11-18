//! The work area bar on the Timeline ruler: dragging an end moves that end, dragging its middle
//! moves the bar, a double-click fits it to the visible part of the sequence, and none of these
//! moves the playhead. Each gesture is one `sequence.setWorkArea` undo step.

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
        // 60 frames a second: the two clicks of a double-click are one frame apart each, well
        // inside egui's 0.3 s
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
    fn bar(&mut self) -> Option<[f64; 4]> {
        let v = self.ok("ui.elements", json!({"prefix": "timeline.workArea"}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == "timeline.workArea")?.clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Some([r[0], r[1], r[2], r[3]])
    }
    fn drag(&mut self, from: (f64, f64), dx: f64) {
        self.ok("ui.drag", json!({"from": {"x": from.0, "y": from.1}, "to": {"x": from.0 + dx, "y": from.1}, "steps": 8}));
        self.frames(3);
    }
    fn area(&self) -> Option<(i64, i64)> {
        self.harness.state().session.active_sequence().unwrap().work_area.map(|w| (w.start.0, w.end().0))
    }
    fn last_undo(&self) -> Option<String> {
        self.harness.state().session.history.undo.last().map(|u| u.0.clone())
    }
}

#[test]
fn dragging_and_double_clicking_the_work_area_bar() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    let rate = d.harness.state().session.sequence_rate();
    let (f24, f72) = (rate.tick_of(24).0, rate.tick_of(72).0);
    let frame = rate.frame_duration().0;
    assert_eq!(d.bar(), None, "hidden, the bar is not drawn");
    d.exec("sequence.setWorkArea", json!({"start": f24, "end": f72}));
    d.frames(2);
    let r = d.bar().expect("the bar is drawn once it is set");
    let (y, per_frame) = (r[1] + r[3] / 2.0, r[2] / 48.0);
    let playhead = d.harness.state().session.playhead();

    // the Out end, 24 frames to the right
    d.drag((r[0] + r[2] - 1.0, y), 24.0 * per_frame);
    let (start, end) = d.area().unwrap();
    assert_eq!(start, f24, "the In end stays");
    assert!((end - rate.tick_of(96).0).abs() <= frame, "the Out end moved 24 frames: {end}");
    assert_eq!(d.last_undo().as_deref(), Some("Set Work Area"));

    // the middle, 12 frames to the left: both ends move, the length stays
    let r = d.bar().unwrap();
    d.drag((r[0] + r[2] / 2.0, y), -12.0 * per_frame);
    let (start2, end2) = d.area().unwrap();
    assert_eq!(end2 - start2, end - start);
    assert!((start2 - rate.tick_of(12).0).abs() <= frame, "the bar moved 12 frames: {start2}");
    assert_eq!(d.harness.state().session.playhead(), playhead, "dragging the bar leaves the playhead");

    // the In end cannot cross the Out end
    let r = d.bar().unwrap();
    d.drag((r[0] + 1.0, y), r[2] + 40.0 * per_frame);
    let (start3, end3) = d.area().unwrap();
    assert_eq!((start3, end3), (end2 - frame, end2), "the In end stops one frame before the Out end");

    // a double-click spans the visible part of the sequence (the whole sequence is in view)
    d.exec("sequence.setWorkArea", json!({"start": f24, "end": f72}));
    d.ok("ui.menu.invoke", json!({"id": "view.zoomToSequence"}));
    d.frames(60);
    let r = d.bar().unwrap();
    let hit = d.ok("ui.timeline.hit", json!({"x": r[0] + r[2] / 2.0, "y": y}));
    assert_eq!(hit["hit"], "workArea", "the control channel reports the bar: {hit}");
    assert_eq!(hit["edge"], Value::Null, "{hit}");
    d.ok("ui.click", json!({"x": r[0] + r[2] / 2.0, "y": y, "count": 2}));
    d.frames(3);
    let duration = d.harness.state().session.active_sequence().unwrap().duration().0;
    let (start4, end4) = d.area().unwrap();
    assert_eq!(start4, 0);
    assert!((end4 - duration).abs() <= frame, "{end4} of {duration}");
    assert_eq!(d.harness.state().session.playhead(), playhead, "nor does a double-click");

    // hidden again from the Timeline panel menu's command
    d.exec("sequence.workAreaBar", json!({"show": false}));
    d.frames(2);
    assert_eq!(d.bar(), None);
}

#[test]
fn the_in_end_of_a_work_area_shorter_than_a_frame_stays_at_zero() {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": false}));
    let rate = d.harness.state().session.sequence_rate();
    let frame = rate.frame_duration().0;
    // zoomed in about the start of the sequence, so half a frame is a few pixels wide
    d.exec("playhead.start", json!({}));
    for _ in 0..5 {
        d.ok("ui.menu.invoke", json!({"id": "view.zoomIn"}));
    }
    d.exec("sequence.setWorkArea", json!({"start": rate.tick_of(24).0, "end": rate.tick_of(72).0}));
    d.frames(2);
    let per_frame = d.bar().unwrap()[2] / 48.0;
    assert!(per_frame > 4.0, "a frame is wide enough to grab half of it: {per_frame}");
    d.exec("sequence.setWorkArea", json!({"start": 0, "end": frame / 2}));
    d.frames(2);
    let r = d.bar().unwrap();
    let y = r[1] + r[3] / 2.0;
    d.drag((r[0] + 0.5, y), 10.0 * per_frame);
    assert_eq!(d.area(), Some((0, frame / 2)), "the In end stays at 0");
    let status = d.harness.state().ui.status.clone();
    assert!(!status.contains("must not be negative"), "the release sends a valid start: {status}");
}

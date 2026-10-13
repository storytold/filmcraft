//! Headless UI tests of the fade handles on a clip's top corners (#1385): a click applies the
//! default transition at that edge, a drag sets its length. The real `FilmcraftApp` under
//! `egui_kittest` with the demo project.

use std::sync::mpsc::{Sender, channel};

use egui::{Pos2, pos2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// A clip of the demo project on V1 whose start has no transition.
const CITY: &str = "City_Night_Drive.mp4";

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project, snapping off so drags land exactly where the pointer goes. Frames are
    /// 1/60 s apart, as on a display: at the harness's default 1/4 s, a press held for four frames
    /// is already a drag by time (egui's 0.8 s click limit), wherever the pointer is.
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.execute("sequence.snap", json!({"on": false})).expect("snapping off");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        // `ui.elements` answers once the timeline has finished fitting the sequence
        d.ok("ui.elements", json!({"prefix": "timeline.clip."}));
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

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    /// Press at `from`, move to `to` over `steps` frames (as a mouse does), release.
    fn drag(&mut self, from: Pos2, to: Pos2, steps: usize) {
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(from));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.frames(1);
        for i in 1..=steps {
            push(self, egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(2);
    }

    /// Move the pointer to `at` with no button down, and let the Timeline react.
    fn hover(&mut self, at: Pos2) {
        self.harness.input_mut().events.push(egui::Event::PointerMoved(at));
        self.frames(2);
    }

    /// The clip named `name` on V1.
    fn v1_clip(&self, name: &str) -> u64 {
        let seq = self.app().session.active_sequence().expect("sequence");
        seq.video_tracks[0].items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("no {name} on V1")).id.0
    }
}

fn handle_at(d: &mut Driver, clip: u64, out: bool) -> Pos2 {
    let id = format!("timeline.clip.{clip}");
    let els = d.ok("ui.elements", json!({"prefix": id}));
    let el = els.as_array().unwrap().iter().find(|e| e["id"] == id.as_str()).unwrap_or_else(|| panic!("no {id}")).clone();
    let r: Vec<f32> = el["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
    // the handle sits 13 points in from the corner, 8 below the clip's top
    pos2(if out { r[0] + r[2] - 13.0 } else { r[0] + 13.0 }, r[1] + 8.0)
}

fn transitions_to(d: &Driver, clip: u64) -> Vec<(i64, i64)> {
    let q = d.app().session.active_sequence().unwrap();
    q.video_tracks.iter().flat_map(|t| t.transitions.iter()).filter(|t| t.to == Some(ClipId(clip))).map(|t| (t.start.0, t.duration.0)).collect()
}

#[test]
fn dragging_a_fade_handle_sets_the_length_of_the_fade_in() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    assert!(transitions_to(&d, city).is_empty(), "the clip starts without a transition");
    let undo = d.app().session.history.undo.len();
    let from = handle_at(&mut d, city, false);
    d.drag(from, from + egui::vec2(90.0, 0.0), 12);
    let made = transitions_to(&d, city);
    assert_eq!(made.len(), 1, "one transition ends at the clip's start edge");
    let q = d.app().session.active_sequence().unwrap();
    let start = q.find_item(ClipId(city)).unwrap().1.start.0;
    assert!(made[0].0 <= start && start <= made[0].0 + made[0].1, "it covers the clip's start edge: {made:?} for a start at {start}");
    let one = d.app().session.sequence_rate().frame_duration().0;
    let frames = made[0].1 / one;
    assert!((8..=60).contains(&frames), "its length follows the drag: {frames} frames");
    assert_eq!(d.app().session.history.undo.len(), undo + 1, "one undo step");
    d.exec("edit.undo", json!({}));
    assert!(transitions_to(&d, city).is_empty());
}

#[test]
fn a_click_on_a_fade_handle_applies_the_default_transition() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let at = handle_at(&mut d, city, false);
    d.hover(at);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    d.frames(1);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: at,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    d.frames(2);
    assert_eq!(transitions_to(&d, city).len(), 1, "the default transition is at the clip's start");
}

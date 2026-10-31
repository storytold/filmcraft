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

    /// Click at `at` with `modifiers` held, then let go of them.
    fn click_with(&mut self, at: Pos2, modifiers: egui::Modifiers) {
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::ModifiersChanged(modifiers));
        push(self, egui::Event::PointerMoved(at));
        self.frames(2);
        push(self, egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers });
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: false, modifiers });
        self.frames(2);
        push(self, egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        self.frames(1);
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

/// Shift, Alt and Ctrl/Cmd are the selection modifiers: held over a fade handle, a click still
/// selects the clip (add to the selection, one side of a link, toggle) and applies no transition.
#[test]
fn a_click_on_a_fade_handle_with_a_selection_modifier_selects_the_clip() {
    for modifiers in [egui::Modifiers::SHIFT, egui::Modifiers::ALT, egui::Modifiers::COMMAND] {
        let mut d = Driver::demo();
        let city = d.v1_clip(CITY);
        let at = handle_at(&mut d, city, false);
        d.click_with(at, modifiers);
        assert!(transitions_to(&d, city).is_empty(), "no transition with {modifiers:?} held");
        assert!(d.app().session.state.selection.contains(&ClipId(city)), "the clip is selected with {modifiers:?} held");
    }
}

/// At a cut the dissolve is centred on it, so a drag makes it at most twice as long as a shorter
/// neighbour, which it would otherwise run past.
#[test]
fn a_fade_at_a_cut_stays_inside_a_short_neighbour() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let one = d.app().session.sequence_rate().frame_duration().0;
    let start = d.app().session.active_sequence().unwrap().find_item(ClipId(city)).unwrap().1.start.0;
    // cut 4 frames in: the 4-frame piece is the neighbour of the rest's start
    d.exec("timeline.razor", json!({"track": "V1", "clip": city, "time": start + 4 * one}));
    let rest = {
        let q = d.app().session.active_sequence().unwrap();
        q.video_tracks[0].items.iter().find(|i| i.start.0 == start + 4 * one).expect("the rest of the clip").id.0
    };
    d.frames(2);
    let from = handle_at(&mut d, rest, false);
    d.drag(from, from + egui::vec2(200.0, 0.0), 16);
    let made = transitions_to(&d, rest);
    assert_eq!(made.len(), 1, "one transition at the cut: {made:?}");
    assert_eq!(made[0].1 / one, 8, "twice the 4-frame neighbour");
    assert!(made[0].0 >= start, "it starts inside the neighbour: {made:?} for a neighbour at {start}");
}

/// A click at a cut is held to the same length as a drag: the default (30 frames) would run far
/// past a 4-frame neighbour.
#[test]
fn a_click_on_a_fade_handle_at_a_cut_stays_inside_a_short_neighbour() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let one = d.app().session.sequence_rate().frame_duration().0;
    let start = d.app().session.active_sequence().unwrap().find_item(ClipId(city)).unwrap().1.start.0;
    d.exec("timeline.razor", json!({"track": "V1", "clip": city, "time": start + 4 * one}));
    let rest = {
        let q = d.app().session.active_sequence().unwrap();
        q.video_tracks[0].items.iter().find(|i| i.start.0 == start + 4 * one).expect("the rest of the clip").id.0
    };
    d.frames(2);
    let at = handle_at(&mut d, rest, false);
    d.click_with(at, egui::Modifiers::NONE);
    let made = transitions_to(&d, rest);
    assert_eq!(made.len(), 1, "one transition at the cut: {made:?}");
    assert_eq!(made[0].1 / one, 8, "twice the 4-frame neighbour, not the 30-frame default");
}

/// Each handle on a hovered clip is an automation element at the spot it can be pressed.
#[test]
fn the_fade_handles_of_a_hovered_clip_have_automation_ids() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    // cut in two, so that both ends of the first piece are free
    let (start, duration) = {
        let it = d.app().session.active_sequence().unwrap().find_item(ClipId(city)).unwrap().1.clone();
        (it.start.0, it.duration.0)
    };
    let one = d.app().session.sequence_rate().frame_duration().0;
    d.exec("timeline.razor", json!({"track": "V1", "clip": city, "time": start + duration * 3 / 4 / one * one}));
    d.frames(2);
    let (fade_in, fade_out) = (handle_at(&mut d, city, false), handle_at(&mut d, city, true));
    d.hover(fade_in);
    let els = d.ok("ui.elements", json!({"prefix": format!("timeline.clip.{city}.fade.")}));
    for (side, at) in [("in", fade_in), ("out", fade_out)] {
        let id = format!("timeline.clip.{city}.fade.{side}");
        let el = els.as_array().unwrap().iter().find(|e| e["id"] == id.as_str()).unwrap_or_else(|| panic!("no {id}: {els}")).clone();
        let r: Vec<f32> = el["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        let centre = pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        assert!(centre.distance(at) < 1.0, "{id} is at {centre:?}, the handle at {at:?}");
    }
}

/// A one-frame clip has no room for a fade: a click on its handle applies none.
#[test]
fn a_click_on_a_one_frame_clip_applies_no_fade() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let one = d.app().session.sequence_rate().frame_duration().0;
    let start = d.app().session.active_sequence().unwrap().find_item(ClipId(city)).unwrap().1.start.0;
    d.exec("timeline.razor", json!({"track": "V1", "clip": city, "time": start + one}));
    // zoomed in so the one frame is wide enough to show its handles
    let seconds = start as f64 / 254_016_000_000.0;
    d.ok("ui.set", json!({"timeline": {"pps": 2400.0, "scroll": seconds - 0.05}}));
    d.frames(4);
    let at = handle_at(&mut d, city, false);
    d.hover(at);
    for pressed in [true, false] {
        d.harness.input_mut().events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        d.frames(1);
    }
    d.frames(2);
    assert!(transitions_to(&d, city).is_empty(), "no fade on a one-frame clip");
}

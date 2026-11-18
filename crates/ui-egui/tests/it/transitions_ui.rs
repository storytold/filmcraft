//! Headless UI tests of applied transitions in the Timeline (#430, #224): click one to select it
//! and see it in Effect Controls, Delete it, drag its end or its middle, double-click it for Set
//! Transition Duration. The real `FilmcraftApp` under `egui_kittest`, driven over the control
//! channel by automation id.
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write
//! `transition-*.png` there; without it no GPU is needed.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    snapshots: Option<std::path::PathBuf>,
}

/// The demo's first video transition between two clips.
#[derive(Clone, Copy, Debug)]
struct Crossing {
    id: u64,
    start: i64,
    duration: i64,
    cut: i64,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        // 60 fps steps, so a double-click's two clicks fall within egui's double-click time
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000);
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
        d.frames(4);
        d.ok("ui.panel.show", json!({"panel": "EffectControls"}));
        d.frames(3);
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

    fn find(&mut self, id: &str) -> Option<(f64, f64, f64, f64, String)> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id)?.clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Some((r[0], r[1], r[2], r[3], e["label"].as_str().unwrap_or_default().to_string()))
    }

    /// With `FILMCRAFT_UI_SNAPSHOT_DIR` set, render the whole window offscreen to `<name>.png`.
    fn snapshot(&mut self, name: &str) {
        let Some(dir) = self.snapshots.clone() else { return };
        self.frames(2);
        match self.harness.render() {
            Ok(img) => {
                std::fs::create_dir_all(&dir).unwrap();
                img.save(dir.join(format!("{name}.png"))).unwrap();
            }
            Err(e) => eprintln!("snapshot {name} skipped: {e}"),
        }
    }

    fn label(&mut self, id: &str) -> String {
        self.find(id).unwrap_or_else(|| panic!("no element {id}")).4
    }

    fn click(&mut self, id: &str) {
        self.find(id).unwrap_or_else(|| panic!("no element {id}"));
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn crossing(&mut self) -> Crossing {
        let q = self.exec("sequence.inspect", json!({}));
        for tr in q["video"].as_array().unwrap() {
            for x in tr["transitions"].as_array().unwrap() {
                if let (Some(_), Some(to)) = (x["from"].as_u64(), x["to"].as_u64()) {
                    let cut = tr["items"].as_array().unwrap().iter().find(|i| i["clip"] == to).unwrap()["start"].as_i64().unwrap();
                    return Crossing { id: x["id"].as_u64().unwrap(), start: x["start"].as_i64().unwrap(), duration: x["duration"].as_i64().unwrap(), cut };
                }
            }
        }
        panic!("the demo has a transition between two clips");
    }

    fn transition(&mut self, id: u64) -> Option<Value> {
        let q = self.exec("sequence.inspect", json!({}));
        q["video"].as_array().unwrap().iter().flat_map(|t| t["transitions"].as_array().unwrap().clone()).find(|x| x["id"] == id)
    }

    fn rate(&mut self) -> (i64, i64) {
        let q = self.exec("sequence.inspect", json!({}));
        (q["settings"]["frame_rate"]["num"].as_i64().unwrap(), q["settings"]["frame_rate"]["den"].as_i64().unwrap())
    }
}

/// Ticks in one frame of `rate`.
fn frame_ticks((num, den): (i64, i64)) -> i64 {
    254_016_000_000 * den / num
}

/// Clicking a transition selects it (and only it) and opens it in Effect Controls; Delete removes
/// it, leaving its clips; undo brings it back.
#[test]
fn click_selects_a_transition_and_delete_removes_it() {
    let mut d = Driver::demo();
    let c = d.crossing();
    let tid = format!("timeline.transition.{}", c.id);
    d.click(&tid);
    let q = d.exec("sequence.inspect", json!({}));
    assert_eq!(q["transitionSelection"], json!([c.id]), "{q}");
    assert_eq!(q["selection"], json!([]), "a transition replaces the clip selection");
    // Effect Controls shows it: duration as timecode, its alignment, Reverse
    let (num, den) = d.rate();
    let rate = filmcraft_engine::time::FrameRate { num, den };
    let frames = c.duration / frame_ticks((num, den));
    assert_eq!(d.label("effectControls.transition.duration"), filmcraft_engine::time::format_timecode_frames(frames, rate, false));
    assert_eq!(d.label("effectControls.transition.alignment"), "Center at Cut");
    assert_eq!(d.label("effectControls.transition.reverse"), "false");
    d.snapshot("transition-selected");
    let clips_before = q["video"].as_array().unwrap().iter().map(|t| t["items"].as_array().unwrap().len()).sum::<usize>();
    // Delete
    d.ok("ui.key", json!({"key": "Delete"}));
    d.frames(3);
    assert!(d.transition(c.id).is_none(), "Delete removes the selected transition");
    let q = d.exec("sequence.inspect", json!({}));
    assert_eq!(q["video"].as_array().unwrap().iter().map(|t| t["items"].as_array().unwrap().len()).sum::<usize>(), clips_before, "its clips stay");
    assert!(d.find("effectControls.transition.duration").is_none(), "Effect Controls lets go of it");
    d.exec("edit.undo", json!({}));
    assert!(d.transition(c.id).is_some(), "undo brings it back");
    // clicking a clip selects the clip instead
    let clip = q["video"][0]["items"][0]["clip"].as_u64().unwrap();
    d.exec("timeline.select", json!({"transitions": [c.id]}));
    d.exec("timeline.select", json!({"clips": [clip]}));
    assert_eq!(d.exec("sequence.inspect", json!({}))["transitionSelection"], json!([]));
}

/// Dragging a transition's end trims that end only (#224); dragging its middle slides it over the
/// cut (Custom Start). Each drag is one undo step.
#[test]
fn dragging_a_transition_end_or_middle() {
    let mut d = Driver::demo();
    let c = d.crossing();
    let frame = frame_ticks(d.rate());
    let undo_before = d.exec("sequence.inspect", json!({}));
    let (x, y, w, h, _) = d.find(&format!("timeline.transition.{}.out", c.id)).expect("the transition's end");
    let (fx, fy) = (x + w / 2.0, y + h / 2.0);
    d.ok("ui.drag", json!({"from": {"x": fx, "y": fy}, "to": {"x": fx + 40.0, "y": fy}, "steps": 8}));
    d.frames(3);
    let x1 = d.transition(c.id).unwrap();
    let (s1, d1) = (x1["start"].as_i64().unwrap(), x1["duration"].as_i64().unwrap());
    assert!(d1 > c.duration, "dragging the end out lengthens it: {} -> {d1}", c.duration);
    assert_eq!(s1, c.start, "the start stays where it was");
    assert_eq!((d1 - c.duration) % frame, 0, "by whole frames");
    assert_eq!(x1["align"], json!("custom"), "no longer centred: Custom Start");
    // the middle: slide it right of centre
    let (x, y, w, h, _) = d.find(&format!("timeline.transition.{}", c.id)).unwrap();
    let (mx, my) = (x + w / 2.0, y + h / 2.0);
    // more than egui's 6 px click tolerance, or the press is a click
    d.ok("ui.drag", json!({"from": {"x": mx, "y": my}, "to": {"x": mx + 14.0, "y": my}, "steps": 6}));
    d.frames(3);
    let x2 = d.transition(c.id).unwrap();
    assert_eq!(x2["duration"].as_i64(), Some(d1), "sliding keeps the duration");
    assert!(x2["start"].as_i64().unwrap() > s1, "and moves it right");
    assert!(x2["start"].as_i64().unwrap() <= c.cut, "still over the cut");
    // off-centre (Custom Start), or Start at Cut if the slide reached the cut, where it stops
    assert!(matches!(x2["align"].as_str(), Some("custom" | "start")), "{}", x2["align"]);
    // two drags, two undo steps
    d.exec("edit.undo", json!({}));
    d.exec("edit.undo", json!({}));
    let back = d.transition(c.id).unwrap();
    assert_eq!((back["start"].as_i64(), back["duration"].as_i64()), (Some(c.start), Some(c.duration)), "{undo_before}");
}

/// Double-clicking a transition opens Set Transition Duration, whose OK sets the duration
/// (following the alignment); Effect Controls' Alignment menu re-aligns it on the cut.
#[test]
fn set_transition_duration_and_alignment_menu() {
    let mut d = Driver::demo();
    let c = d.crossing();
    let frame = frame_ticks(d.rate());
    // double-click it
    let (x0, y0, w, h, _) = d.find(&format!("timeline.transition.{}", c.id)).unwrap();
    d.ok("ui.click", json!({"x": x0 + w / 2.0, "y": y0 + h / 2.0, "count": 2}));
    d.frames(3);
    let frames = c.duration / frame;
    assert!(d.find("transitionDuration.value").is_some(), "Set Transition Duration opened");
    assert_eq!(d.harness.state().ui.transition_duration.frames, frames, "with its duration");
    assert_eq!(d.exec("sequence.inspect", json!({}))["transitionSelection"], json!([c.id]), "and selected it");
    d.snapshot("transition-duration-dialog");
    // Center at Cut: a new duration moves both ends
    d.harness.state_mut().ui.transition_duration.frames = 10;
    d.frames(2);
    d.click("transitionDuration.ok");
    assert!(d.find("transitionDuration.value").is_none(), "closed");
    let x = d.transition(c.id).unwrap();
    assert_eq!((x["start"].as_i64(), x["duration"].as_i64(), x["align"].as_str()), (Some(c.cut - 5 * frame), Some(10 * frame), Some("center")));
    // the Alignment menu: Start at Cut
    d.click("effectControls.transition.alignment");
    d.click("effectControls.transition.alignment.option.start");
    let x = d.transition(c.id).unwrap();
    assert_eq!((x["start"].as_i64(), x["duration"].as_i64(), x["align"].as_str()), (Some(c.cut), Some(10 * frame), Some("start")));
    assert_eq!(d.label("effectControls.transition.alignment"), "Start at Cut");
    // Reverse
    d.click("effectControls.transition.reverse");
    assert_eq!(d.transition(c.id).unwrap()["reverse"], json!(true));
}

/// A transition's own settings are in Effect Controls and change it (Push: Direction, Motion Blur).
#[test]
fn transition_settings_in_effect_controls() {
    let mut d = Driver::demo();
    let q = d.exec("sequence.inspect", json!({}));
    let push = q["video"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["transitions"].as_array().unwrap().clone())
        .find(|x| x["effect"] == "push")
        .expect("the demo has a Push");
    let id = push["id"].as_u64().unwrap();
    d.click(&format!("timeline.transition.{id}"));
    for p in ["direction", "motion_blur"] {
        assert!(d.find(&format!("effectControls.transition.param.{p}")).is_some(), "{p} shown");
    }
    d.snapshot("transition-push-settings");
    d.click("effectControls.transition.param.direction");
    d.click("effectControls.transition.param.direction.option.0");
    assert_eq!(d.transition(id).unwrap()["params"]["direction"], json!({"Choice": 0}));
    d.click("effectControls.transition.reset");
    let def = filmcraft_engine::project::vtransition::find_transition("push", filmcraft_engine::project::EffectKind::VideoTransition).unwrap();
    let want = serde_json::to_value(&def.instance().params.get("direction").unwrap().value).unwrap();
    assert_eq!(d.transition(id).unwrap()["params"]["direction"], want, "Reset puts Push's own defaults back");
}

/// Effect Controls' own timeline for a selected transition (#577): clip A, clip B and the
/// transition over their cut. Dragging its end changes its duration from that end, dragging its
/// middle slides it over the cut, each drag one undo step; the ruler moves the playhead.
#[test]
fn transition_timeline_in_effect_controls() {
    let mut d = Driver::demo();
    let c = d.crossing();
    let frame = frame_ticks(d.rate());
    d.click(&format!("timeline.transition.{}", c.id));
    // both clips are shown, by name
    let q = d.exec("sequence.inspect", json!({}));
    let names: Vec<String> = q["video"]
        .as_array()
        .unwrap()
        .iter()
        .flat_map(|t| t["items"].as_array().unwrap().clone())
        .filter(|i| i["start"].as_i64().unwrap() < c.cut + c.duration && i["start"].as_i64().unwrap() + i["duration"].as_i64().unwrap() > c.cut - c.duration)
        .map(|i| i["name"].as_str().unwrap_or_default().to_string())
        .collect();
    let (a, b) = (d.label("effectControls.transition.timeline.a"), d.label("effectControls.transition.timeline.b"));
    assert!(names.contains(&a) && names.contains(&b), "A {a:?} and B {b:?} are the clips at the cut: {names:?}");
    d.snapshot("transition-effect-controls-timeline");
    // its end: drag it right
    let (x, y, w, h, _) = d.find("effectControls.transition.timeline.span.out").expect("the transition's end in Effect Controls");
    let (fx, fy) = (x + w / 2.0, y + h / 2.0);
    d.ok("ui.drag", json!({"from": {"x": fx, "y": fy}, "to": {"x": fx + 30.0, "y": fy}, "steps": 8}));
    d.frames(3);
    let x1 = d.transition(c.id).unwrap();
    let (s1, d1) = (x1["start"].as_i64().unwrap(), x1["duration"].as_i64().unwrap());
    assert!(d1 > c.duration, "dragging the end out lengthens it: {} -> {d1}", c.duration);
    assert_eq!(s1, c.start, "the start stays where it was");
    assert_eq!((d1 - c.duration) % frame, 0, "by whole frames");
    let (num, den) = d.rate();
    let rate = filmcraft_engine::time::FrameRate { num, den };
    assert_eq!(
        d.label("effectControls.transition.duration"),
        filmcraft_engine::time::format_timecode_frames(d1 / frame, rate, false),
        "the Duration field follows"
    );
    // its middle: slide it left (more than egui's click tolerance)
    let (x, y, w, h, _) = d.find("effectControls.transition.timeline.span").unwrap();
    let (mx, my) = (x + w / 2.0, y + h / 2.0);
    d.ok("ui.drag", json!({"from": {"x": mx, "y": my}, "to": {"x": mx - 14.0, "y": my}, "steps": 6}));
    d.frames(3);
    let x2 = d.transition(c.id).unwrap();
    assert_eq!(x2["duration"].as_i64(), Some(d1), "sliding keeps the duration");
    let s2 = x2["start"].as_i64().unwrap();
    assert!(s2 < s1, "and moves it left: {s1} -> {s2}");
    assert!(s2 + d1 >= c.cut, "still over the cut");
    assert_eq!((s1 - s2) % frame, 0, "by whole frames");
    // two drags, two undo steps
    d.exec("edit.undo", json!({}));
    let back = d.transition(c.id).unwrap();
    assert_eq!((back["start"].as_i64(), back["duration"].as_i64()), (Some(s1), Some(d1)), "the slide is one step");
    d.exec("edit.undo", json!({}));
    let back = d.transition(c.id).unwrap();
    assert_eq!((back["start"].as_i64(), back["duration"].as_i64()), (Some(c.start), Some(c.duration)), "the resize is the other");
    // the ruler: a click at its left end puts the playhead before the cut, at its right end after
    let (x, y, w, h, _) = d.find("effectControls.transition.timeline.ruler").unwrap();
    d.ok("ui.click", json!({"x": x + 3.0, "y": y + h / 2.0}));
    d.frames(3);
    let ph = d.exec("sequence.inspect", json!({}))["playhead"].as_i64();
    assert!(ph.is_some_and(|p| p < c.cut), "{ph:?}");
    d.ok("ui.click", json!({"x": x + w - 3.0, "y": y + h / 2.0}));
    d.frames(3);
    let ph = d.exec("sequence.inspect", json!({}))["playhead"].as_i64();
    assert!(ph.is_some_and(|p| p > c.cut), "{ph:?}");
    assert!(d.find("effectControls.transition.timeline.playhead").is_some(), "the playhead shows in the transition's timeline");
}

/// Dragging the cut line over the transition in Effect Controls moves the cut and the transition
/// together: the clips' In and Out points change, the sequence keeps its length, and the drag is
/// one undo step.
#[test]
fn dragging_the_cut_in_effect_controls_moves_it_with_the_transition() {
    let mut d = Driver::demo();
    let c = d.crossing();
    let frame = frame_ticks(d.rate());
    d.click(&format!("timeline.transition.{}", c.id));
    let x = d.transition(c.id).unwrap();
    let (from, to) = (x["from"].as_u64().unwrap(), x["to"].as_u64().unwrap());
    // (start, duration, source in) of a clip
    let clip = |d: &mut Driver, id: u64| {
        let q = d.exec("sequence.inspect", json!({}));
        let it = q["video"].as_array().unwrap().iter().flat_map(|t| t["items"].as_array().unwrap().clone()).find(|i| i["clip"] == id).unwrap();
        (it["start"].as_i64().unwrap(), it["duration"].as_i64().unwrap(), it["sourceIn"].clone())
    };
    let (a0, b0) = (clip(&mut d, from), clip(&mut d, to));
    let (x0, y0, w, h, _) = d.find("effectControls.transition.timeline.cut").expect("the cut line over the transition");
    let (cx, cy) = (x0 + w / 2.0, y0 + h / 2.0);
    d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx + 20.0, "y": cy}, "steps": 8}));
    d.frames(3);
    let (a1, b1) = (clip(&mut d, from), clip(&mut d, to));
    let moved = b1.0 - b0.0;
    assert!(moved > 0, "the cut moved right: {b0:?} -> {b1:?}");
    assert_eq!(moved % frame, 0, "by whole frames");
    assert_eq!((a1.0, a1.1), (a0.0, a0.1 + moved), "the first clip ends at the new cut");
    assert_eq!(b1.0 + b1.1, b0.0 + b0.1, "the second clip still ends where it did");
    assert_ne!(b1.2, b0.2, "the second clip starts later in its media");
    let x1 = d.transition(c.id).unwrap();
    assert_eq!((x1["start"].as_i64(), x1["duration"].as_i64()), (Some(c.start + moved), Some(c.duration)), "the transition moved with the cut");
    assert_eq!(x1["align"], json!("center"), "still centered on its cut");
    // one undo step
    d.exec("edit.undo", json!({}));
    assert_eq!((clip(&mut d, from), clip(&mut d, to)), (a0, b0));
    let back = d.transition(c.id).unwrap();
    assert_eq!((back["start"].as_i64(), back["duration"].as_i64()), (Some(c.start), Some(c.duration)));
}

/// With Start at Cut the cut is at the transition's edge, where a press over the transition grabs
/// its In end, so the cut line is grabbed where it crosses clip A. A drag past what the clips
/// allow stops where `timeline.roll` stops.
#[test]
fn the_cut_is_grabbed_over_the_clips_and_stops_at_the_rolls_limit() {
    let mut d = Driver::demo();
    let c = d.crossing();
    d.click(&format!("timeline.transition.{}", c.id));
    let x = d.transition(c.id).unwrap();
    let (from, to) = (x["from"].as_u64().unwrap(), x["to"].as_u64().unwrap());
    d.exec("sequence.setTransition", json!({"transition": c.id, "align": "start"}));
    d.frames(3);
    let cut_of = |d: &mut Driver| {
        let q = d.exec("sequence.inspect", json!({}));
        q["video"].as_array().unwrap().iter().flat_map(|t| t["items"].as_array().unwrap().clone()).find(|i| i["clip"] == to).unwrap()["start"].as_i64().unwrap()
    };
    let start_of = |d: &mut Driver| d.transition(c.id).unwrap()["start"].as_i64().unwrap();
    let drag_cut = |d: &mut Driver, by: f64| {
        let (x0, y0, w, h, _) = d.find("effectControls.transition.timeline.cut.a").expect("the cut line over clip A");
        let (cx, cy) = (x0 + w / 2.0, y0 + h / 2.0);
        d.ok("ui.drag", json!({"from": {"x": cx, "y": cy}, "to": {"x": cx + by, "y": cy}, "steps": 8}));
        d.frames(3);
    };
    let (cut0, start0) = (cut_of(&mut d), start_of(&mut d));
    assert_eq!(start0, cut0, "Start at Cut");
    drag_cut(&mut d, 20.0);
    let (cut1, start1) = (cut_of(&mut d), start_of(&mut d));
    let moved = cut1 - cut0;
    assert!(moved > 0, "the cut moved right: {cut0} -> {cut1}");
    assert_eq!(start1, cut1, "the transition still starts at the cut");
    // far to the right: the cut stops at the roll's limit
    let far = filmcraft_time::Tick(3600 * filmcraft_time::TICKS_PER_SECOND);
    let limit =
        filmcraft_engine::commands::roll_delta(&d.harness.state().session, filmcraft_project::ClipId(from), filmcraft_project::ClipId(to), far).unwrap();
    assert!(0 < limit.0 && limit < far, "the roll has a limit: {limit:?}");
    drag_cut(&mut d, limit.0 as f64 / moved as f64 * 20.0 + 200.0);
    assert_eq!(cut_of(&mut d), cut1 + limit.0, "stopped at the limit");
}

/// #3573: right-clicking a video transition in the Effects panel offers Set as Default Transition,
/// and Cmd+D then applies that transition.
#[test]
fn set_as_default_transition_from_the_effects_panel_feeds_cmd_d() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    d.ok("ui.set", json!({"effectsSearch": "Iris Round"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "effects.item.iris_round", "button": "right"}));
    d.frames(3);
    d.click("effects.itemMenu.setDefaultTransition");
    assert_eq!(d.harness.state().session.state.default_video_transition, "iris_round");
    // a plain effect has no such item
    d.ok("ui.set", json!({"effectsSearch": "Gaussian Blur"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "effects.item.gaussian_blur", "button": "right"}));
    d.frames(3);
    assert!(d.find("effects.itemMenu.setDefaultTransition").is_none());
    d.ok("ui.key", json!({"key": "Escape"}));
    // Cmd+D on a selected clip uses it
    let q = d.exec("sequence.inspect", json!({}));
    let clip = q["video"][0]["items"][2]["clip"].clone();
    d.exec("timeline.select", json!({"clips": [clip]}));
    d.ok("ui.key", json!({"key": "Cmd+D"}));
    d.frames(2);
    let q = d.exec("sequence.inspect", json!({}));
    assert!(q["video"][0]["transitions"].as_array().unwrap().iter().any(|t| t["to"] == clip && t["effect"] == "iris_round"), "{q}");
}

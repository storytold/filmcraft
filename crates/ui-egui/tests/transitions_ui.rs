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

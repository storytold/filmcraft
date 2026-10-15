//! Time Remapping in Effect Controls, driven headless the way a person drags the Speed value: a
//! drag edits one keyframe (each step retimes the clip, which moved the frame under the playhead and
//! wrote a keyframe per step), the value only follows the pointer, and the playhead stays on the
//! keyframe, so the next drag or the ◆ acts on that keyframe again.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, Param};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project with its first video clip selected and both panels showing.
    fn demo() -> (Self, Clip) {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        let seq = d.exec("sequence.inspect", json!({}));
        let c = &seq["video"][0]["items"][0];
        let clip = Clip { id: c["clip"].as_u64().unwrap() };
        d.exec("timeline.select", json!({"clips": [clip.id]}));
        for panel in ["EffectControls", "Properties"] {
            d.ok("ui.panel.show", json!({"panel": panel}));
        }
        d.frames(3);
        (d, clip)
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
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    /// The element's rect `[x, y, w, h]`, or `None` when it is not on screen.
    fn find(&mut self, id: &str) -> Option<[f64; 4]> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id)?;
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Some([r[0], r[1], r[2], r[3]])
    }
    fn rect(&mut self, id: &str) -> [f64; 4] {
        self.find(id).unwrap_or_else(|| panic!("no element {id}"))
    }
    fn click(&mut self, id: &str) {
        self.rect(id);
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
    fn release(&mut self, to: egui::Pos2, modifiers: egui::Modifiers) {
        self.harness.input_mut().events.push(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers });
        self.frames(2);
        self.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        self.frames(2);
    }
    fn playhead(&mut self) -> i64 {
        self.exec("sequence.inspect", json!({}))["playhead"].as_i64().unwrap()
    }
    fn seek(&mut self, seconds: f64) -> i64 {
        self.exec("playhead.set", json!({"seconds": seconds}));
        self.frames(2);
        self.playhead()
    }
    fn parameter(&self, clip: &Clip, effect: &str, param: &str) -> Param {
        self.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effect(effect).unwrap().param(param).unwrap().clone()
    }
    fn undo_len(&mut self) -> usize {
        self.exec("history.list", json!({}))["undo"].as_array().unwrap().len()
    }
}

struct Clip {
    id: u64,
}

const VALUE: &str = "effectControls.time_remap.speed.value";

fn speed(d: &Driver, clip: &Clip) -> Param {
    d.parameter(clip, "time_remap", "speed")
}

/// Whether the playhead is on one of the Speed keyframes (on a remapped clip: within half a frame).
fn on_keyframe(d: &Driver, clip: &Clip) -> bool {
    let s = &d.harness.state().session;
    let q = s.active_sequence().unwrap();
    let it = q.find_item(ClipId(clip.id)).unwrap().1;
    let p = it.effect("time_remap").unwrap().param("speed").unwrap();
    it.keyframe_near(p, it.source_time_at(s.playhead()), q.settings.frame_rate.frame_duration()).is_some()
}

/// Press on the Speed value and drag it `px` to the left in `steps` moves, checking after every move
/// that the speed only went down; returns the keyframe values seen.
fn drag_speed_left(d: &mut Driver, clip: &Clip, px: f32, steps: usize) -> Vec<f64> {
    let r = d.rect(VALUE);
    let from = egui::pos2((r[0] + r[2] / 2.0) as f32, (r[1] + r[3] / 2.0) as f32);
    let none = egui::Modifiers::NONE;
    d.harness.input_mut().events.push(egui::Event::PointerMoved(from));
    d.frames(1);
    d.harness.input_mut().events.push(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: none });
    d.frames(1);
    let mut seen: Vec<f64> = Vec::new();
    for i in 1..=steps {
        d.harness.input_mut().events.push(egui::Event::PointerMoved(from - egui::vec2(px * i as f32 / steps as f32, 0.0)));
        d.frames(2);
        let p = speed(d, clip);
        let lowest = p.keyframes.iter().filter_map(|k| k.value.as_f64()).fold(f64::MAX, f64::min);
        if let Some(&last) = seen.last() {
            assert!(lowest <= last + 1e-9, "step {i}: the speed went back up from {last} to {lowest}");
        }
        seen.push(lowest);
    }
    let to = from - egui::vec2(px, 0.0);
    d.release(to, none);
    seen
}

#[test]
fn dragging_the_speed_edits_one_keyframe_and_keeps_the_playhead_on_it() {
    let (mut d, clip) = Driver::demo();
    // a keyframe at 0.5 s (100 %), then the playhead at 2 s, where the drag writes its own
    d.seek(0.5);
    d.click("effectControls.time_remap.speed.stopwatch");
    assert_eq!(speed(&d, &clip).keyframes.len(), 1);
    d.seek(2.0);
    let undo = d.undo_len();
    let seen = drag_speed_left(&mut d, &clip, 120.0, 12);
    let p = speed(&d, &clip);
    assert_eq!(p.keyframes.len(), 2, "one keyframe for the whole drag: {:?}", p.keyframes);
    assert!(*seen.last().unwrap() < 60.0, "the drag slowed it down: {seen:?}");
    assert_eq!(d.undo_len(), undo + 1, "one undo step");
    assert!(on_keyframe(&d, &clip), "the playhead stayed on the keyframe it changed");
    // a second drag carries on from that keyframe's value instead of starting a new keyframe
    let before = p.keyframes[1].value.as_f64().unwrap();
    let seen = drag_speed_left(&mut d, &clip, 40.0, 4);
    let p = speed(&d, &clip);
    assert_eq!(p.keyframes.len(), 2, "{:?}", p.keyframes);
    assert!(seen.iter().all(|v| *v <= before + 1e-9) && p.keyframes[1].value.as_f64().unwrap() < before);
    assert!(on_keyframe(&d, &clip));
    // and the ◆ there removes it rather than adding one beside it
    d.click("effectControls.time_remap.speed.addKeyframe");
    assert_eq!(speed(&d, &clip).keyframes.len(), 1);
}

#[test]
fn the_keyframe_arrows_land_on_a_remapped_clips_keyframes() {
    let (mut d, clip) = Driver::demo();
    // keyframes at 0.5 s and 2 s of the clip; the second one's speed puts it more than half a
    // frame past a frame (the playhead used to snap down to the frame before it, off the keyframe)
    d.seek(0.5);
    d.click("effectControls.time_remap.speed.stopwatch");
    d.seek(2.0);
    d.click("effectControls.time_remap.speed.addKeyframe");
    let second = speed(&d, &clip).keyframes[1].time;
    let past_half = |d: &Driver| {
        let s = &d.harness.state().session;
        let q = s.active_sequence().unwrap();
        let frame = q.settings.frame_rate.frame_duration().0;
        let t = q.find_item(ClipId(clip.id)).unwrap().1.timeline_time_of(second).0;
        let f = t.rem_euclid(frame) as f64 / frame as f64;
        f > 0.6 && f < 0.95
    };
    let mut found = false;
    for v in 20..95 {
        d.exec("effects.setKeyframe", json!({"clip": clip.id, "effect": "time_remap", "param": "speed", "mediaTime": second.0, "value": v}));
        if past_half(&d) {
            found = true;
            break;
        }
    }
    assert!(found, "a speed that puts the keyframe past half a frame");
    d.seek(0.0);
    for _ in 0..2 {
        d.click("effectControls.time_remap.speed.nextKeyframe");
        assert!(on_keyframe(&d, &clip), "▶ lands on the keyframe");
    }
    d.click("effectControls.time_remap.speed.prevKeyframe");
    assert!(on_keyframe(&d, &clip), "◀ lands on the keyframe");
    d.click("effectControls.time_remap.speed.nextKeyframe");
    assert!(on_keyframe(&d, &clip));
}

//! On-screen transform handles on the Program monitor (#639), driven headless through the control
//! channel: selecting Motion (or the Transform effect) in Effect Controls shows a box with handles
//! and the anchor point; dragging moves, scales, rotates and moves the anchor point, one undo
//! step per drag, keying the playhead when the stopwatch is on. Clicking an effect header selects
//! it and only its arrow twirls it; double-clicking a clip's picture selects its Motion.
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the tests also render the UI with wgpu and write PNGs there.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, ParamValue};
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<std::path::PathBuf>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from);
        // frames 1/60 s apart, so that two clicks make a double-click
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots };
        d.frames(4);
        d.ok("ui.set", json!({"workspace": "Effects"}));
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
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn find(&mut self, id: &str) -> Option<[f64; 4]> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id)?.clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Some([r[0], r[1], r[2], r[3]])
    }
    fn rect(&mut self, id: &str) -> [f64; 4] {
        self.find(id).unwrap_or_else(|| panic!("no element {id}"))
    }
    fn center(&mut self, id: &str) -> (f64, f64) {
        let r = self.rect(id);
        (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    }
    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }
    fn drag_from(&mut self, (x, y): (f64, f64), dx: f64, dy: f64) {
        self.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + dx, "y": y + dy}, "steps": 6}));
        self.frames(2);
    }
    fn drag(&mut self, id: &str, dx: f64, dy: f64) {
        let at = self.center(id);
        self.drag_from(at, dx, dy);
    }
    /// A drag of 48 × 24 points with `button` (the control channel's `ui.drag` uses the left one).
    fn drag_with(&mut self, (x, y): (f64, f64), button: egui::PointerButton) {
        let from = egui::pos2(x as f32, y as f32);
        let press = |pos, pressed| egui::Event::PointerButton { pos, button, pressed, modifiers: Default::default() };
        let mut events = vec![egui::Event::PointerMoved(from), press(from, true)];
        events.extend((1..=6).map(|k| egui::Event::PointerMoved(from + egui::vec2(8.0 * k as f32, 4.0 * k as f32))));
        events.push(press(from + egui::vec2(48.0, 24.0), false));
        for e in events {
            self.harness.input_mut().events.push(e);
            self.frames(1);
        }
        self.frames(3);
    }
    fn undo_len(&mut self) -> usize {
        self.exec("history.list", json!({}))["undo"].as_array().unwrap().len()
    }
    fn selected_effect(&mut self) -> Value {
        self.exec("state.inspect", json!({}))["selected_effect"].clone()
    }
    /// The first V1 clip, selected, with the playhead in its middle.
    fn first_clip(&mut self) -> (u64, i64) {
        let seq = self.exec("sequence.inspect", json!({}));
        let clip = &seq["video"][0]["items"][0];
        let (id, start, dur) = (clip["clip"].as_u64().unwrap(), clip["start"].as_i64().unwrap(), clip["duration"].as_i64().unwrap());
        self.exec("playhead.set", json!({"time": start + dur / 2}));
        self.exec("timeline.select", json!({"clips": [id]}));
        self.frames(3);
        (id, start + dur / 2)
    }
    /// A parameter of a clip's effect: its value at the playhead and its keyframe times.
    fn param(&self, clip: u64, effect: &str, param: &str) -> (ParamValue, Vec<Tick>) {
        let app = self.harness.state();
        let it = app.session.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1;
        let mt = it.source_time_at(app.session.playhead());
        let p = it.effect(effect).unwrap().param(param).unwrap();
        (p.value_at(mt), p.keyframes.iter().map(|k| k.time).collect())
    }
    fn num(&self, clip: u64, effect: &str, param: &str) -> f64 {
        self.param(clip, effect, param).0.as_f64().unwrap()
    }
    fn point(&self, clip: u64, effect: &str, param: &str) -> (f64, f64) {
        match self.param(clip, effect, param).0 {
            ParamValue::Vec2(v) => (v.x, v.y),
            other => panic!("{param} is not a point: {other:?}"),
        }
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        for _ in 0..40 {
            self.frames(1);
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
}

fn near(a: (f64, f64), b: (f64, f64)) -> bool {
    (a.0 - b.0).abs() < 1.5 && (a.1 - b.1).abs() < 1.5
}

const HANDLES: [&str; 11] = [
    "program.transform.box",
    "program.transform.anchor",
    "program.transform.handle.0",
    "program.transform.handle.1",
    "program.transform.handle.2",
    "program.transform.handle.3",
    "program.transform.handle.4",
    "program.transform.handle.5",
    "program.transform.handle.6",
    "program.transform.handle.7",
    "program.transform.rotate.1",
];

#[test]
fn motion_handles_move_scale_rotate_and_move_the_anchor() {
    let mut d = Driver::demo();
    let (clip, _) = d.first_clip();
    // half size, so the whole box and the corners' rotate zones are on the monitor
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "scale", "value": 50.0}));
    d.frames(2);
    assert!(d.find("program.transform.box").is_none(), "no handles before Motion is selected");
    d.click("effectControls.effect.motion");
    assert_eq!(d.selected_effect(), json!({"clip": clip, "effect": "motion", "instance": 0}));
    for id in HANDLES {
        d.rect(id);
    }
    d.shot("transform-motion-selected");

    // a drag inside the box (away from the anchor point) moves the picture: one undo step
    let undo = d.undo_len();
    let p0 = d.point(clip, "motion", "position");
    let b = d.rect("program.transform.box");
    let pic = d.rect("program.picture");
    d.drag_from((b[0] + b[2] * 0.3, b[1] + b[3] * 0.3), 40.0, 20.0);
    let p1 = d.point(clip, "motion", "position");
    let k = 1920.0 / pic[2];
    assert!((p1.0 - p0.0 - 40.0 * k).abs() < 2.0 * k && (p1.1 - p0.1 - 20.0 * k).abs() < 2.0 * k, "{p0:?} → {p1:?} (×{k})");
    assert_eq!(d.num(clip, "motion", "scale"), 50.0);
    assert_eq!(d.undo_len(), undo + 1, "one undo step per drag");
    assert!(d.param(clip, "motion", "position").1.is_empty(), "the stopwatch is off: no keyframe");

    // a corner scales both axes about the anchor point (Uniform Scale is on); the drawn square is
    // 5 points wide but a press a few points beside it, outside the box, still takes it
    let undo = d.undo_len();
    let (hx, hy) = d.center("program.transform.handle.2");
    d.drag_from((hx + 4.0, hy + 4.0), 30.0, 30.0);
    let s = d.num(clip, "motion", "scale");
    assert!(s > 55.0, "scale {s}");
    assert_eq!(d.point(clip, "motion", "position"), p1, "scaling keeps the position");
    assert_eq!(d.undo_len(), undo + 1);

    // just outside a corner rotates: down from beside the top-right corner turns clockwise
    d.drag("program.transform.rotate.1", 0.0, 60.0);
    let r = d.num(clip, "motion", "rotation");
    assert!(r > 5.0, "rotation {r}");
    d.shot("transform-motion-rotated");

    // the anchor point moves over the picture, which stays where it is
    let corners: Vec<(f64, f64)> = (0..4).map(|n| d.center(&format!("program.transform.handle.{n}"))).collect();
    let (a0, p2) = (d.point(clip, "motion", "anchor"), d.point(clip, "motion", "position"));
    let at = d.center("program.transform.anchor");
    let undo = d.undo_len();
    d.drag("program.transform.anchor", 30.0, 20.0);
    assert!(near(d.center("program.transform.anchor"), (at.0 + 30.0, at.1 + 20.0)), "the anchor point follows the pointer");
    for (n, c) in corners.iter().enumerate() {
        assert!(near(d.center(&format!("program.transform.handle.{n}")), *c), "corner {n} stays put");
    }
    assert_ne!(d.point(clip, "motion", "anchor"), a0);
    let p3 = d.point(clip, "motion", "position");
    assert!((p3.0 - p2.0 - 30.0 * k).abs() < 2.0 * k && (p3.1 - p2.1 - 20.0 * k).abs() < 2.0 * k, "position {p2:?} → {p3:?}");
    assert_eq!(d.undo_len(), undo + 1, "anchor and position change in one undo step");
    d.exec("edit.undo", json!({}));
    assert_eq!((d.point(clip, "motion", "anchor"), d.point(clip, "motion", "position")), (a0, p2));

    // Uniform Scale off: a side handle changes only the width, the top only the height
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "rotation", "value": 0.0}));
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "uniform_scale", "value": false}));
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "scale_width", "value": 50.0}));
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "scale", "value": 50.0}));
    d.frames(2);
    d.drag("program.transform.handle.5", 30.0, 10.0);
    assert!(d.num(clip, "motion", "scale_width") > 55.0);
    assert_eq!(d.num(clip, "motion", "scale"), 50.0);
    let w = d.num(clip, "motion", "scale_width");
    d.drag("program.transform.handle.4", 10.0, -30.0);
    assert!(d.num(clip, "motion", "scale") > 55.0);
    assert_eq!(d.num(clip, "motion", "scale_width"), w);
    // a corner changes both, each by its own side
    d.drag("program.transform.handle.2", 30.0, 0.0);
    assert!(d.num(clip, "motion", "scale_width") > w + 3.0);
    d.shot("transform-motion-non-uniform");

    // a click beside the box, beyond its rotate zone, lets go of it
    let b = d.rect("program.transform.box");
    d.ok("ui.click", json!({"x": b[0] - 40.0, "y": b[1] + b[3] / 2.0}));
    d.frames(2);
    assert_eq!(d.selected_effect(), Value::Null);
    assert!(d.find("program.transform.box").is_none());
}

#[test]
fn a_drag_keys_the_playhead_and_headers_select_while_arrows_twirl() {
    let mut d = Driver::demo();
    let (clip, mid) = d.first_clip();
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "scale", "value": 50.0}));
    // stopwatch on at the middle of the clip, then a second further on
    d.exec("effects.toggleAnimation", json!({"clip": clip, "effect": "motion", "param": "position"}));
    let key0 = d.param(clip, "motion", "position").1;
    assert_eq!(key0.len(), 1);
    d.exec("playhead.set", json!({"time": mid + filmcraft_time::TICKS_PER_SECOND}));
    d.frames(2);
    // clicking a property (its name) selects Motion too
    let (sx, sy) = d.center("effectControls.motion.scale.stopwatch");
    d.ok("ui.click", json!({"x": sx + 30.0, "y": sy}));
    d.frames(2);
    assert_eq!(d.selected_effect(), json!({"clip": clip, "effect": "motion", "instance": 0}));
    let p0 = d.point(clip, "motion", "position");
    let undo = d.undo_len();
    let b = d.rect("program.transform.box");
    d.drag_from((b[0] + b[2] * 0.3, b[1] + b[3] * 0.3), -30.0, 15.0);
    let (moved, keys) = d.param(clip, "motion", "position");
    assert_eq!(keys.len(), 2, "a keyframe at the playhead: {keys:?}");
    assert!(keys.contains(&key0[0]));
    assert_ne!(moved, ParamValue::Vec2(filmcraft_geom::Vec2::new(p0.0, p0.1)));
    assert_eq!(d.undo_len(), undo + 1);
    // the first keyframe keeps its value
    d.exec("playhead.set", json!({"time": mid}));
    d.frames(2);
    assert_eq!(d.point(clip, "motion", "position"), p0);

    // the header selects without twirling; the arrow twirls without selecting
    d.exec("effects.select", json!({"none": true}));
    d.click("effectControls.effect.motion");
    assert_eq!(d.selected_effect(), json!({"clip": clip, "effect": "motion", "instance": 0}));
    assert!(d.find("effectControls.motion.position.stopwatch").is_some(), "Motion is still open");
    d.click("effectControls.effect.motion.twirl");
    assert!(d.find("effectControls.motion.position.stopwatch").is_none(), "the arrow folds Motion");
    assert_eq!(d.selected_effect(), json!({"clip": clip, "effect": "motion", "instance": 0}));
    d.click("effectControls.effect.motion.twirl");
    assert!(d.find("effectControls.motion.position.stopwatch").is_some(), "and opens it again");
    // selecting another effect hides the handles: Opacity has none
    d.click("effectControls.effect.opacity");
    assert_eq!(d.selected_effect(), json!({"clip": clip, "effect": "opacity", "instance": 0}));
    assert!(d.find("program.transform.box").is_none());
}

#[test]
fn transform_effect_handles_and_double_click_selection() {
    let mut d = Driver::demo();
    let (clip, _) = d.first_clip();
    d.exec("effects.apply", json!({"clips": [clip], "effect": "transform"}));
    d.exec("effects.setParam", json!({"clip": clip, "effect": "transform", "param": "scale_height", "value": 50.0}));
    d.frames(2);
    // fold the fixed effects so the Transform header is on screen
    for fx in ["motion", "opacity", "time_remap"] {
        d.click(&format!("effectControls.effect.{fx}.twirl"));
    }
    d.click("effectControls.effect.transform");
    assert_eq!(d.selected_effect(), json!({"clip": clip, "effect": "transform", "instance": 0}));
    for id in HANDLES {
        d.rect(id);
    }
    d.shot("transform-effect-selected");
    let motion = d.point(clip, "motion", "position");
    let p0 = d.point(clip, "transform", "position");
    let b = d.rect("program.transform.box");
    d.drag_from((b[0] + b[2] * 0.3, b[1] + b[3] * 0.3), 40.0, 0.0);
    let p1 = d.point(clip, "transform", "position");
    assert!(p1.0 > p0.0 + 20.0 && (p1.1 - p0.1).abs() < 1e-6, "{p0:?} → {p1:?}");
    assert_eq!(d.point(clip, "motion", "position"), motion, "Motion is left alone");
    d.drag("program.transform.handle.2", 30.0, 30.0);
    assert!(d.num(clip, "transform", "scale_height") > 55.0);
    d.drag("program.transform.rotate.1", 0.0, 60.0);
    assert!(d.num(clip, "transform", "rotation") > 5.0);
    d.exec("effects.setParam", json!({"clip": clip, "effect": "transform", "param": "rotation", "value": 0.0}));
    d.exec("effects.setParam", json!({"clip": clip, "effect": "transform", "param": "uniform_scale", "value": false}));
    d.exec("effects.setParam", json!({"clip": clip, "effect": "transform", "param": "scale_width", "value": 50.0}));
    d.exec("effects.setParam", json!({"clip": clip, "effect": "transform", "param": "scale_height", "value": 50.0}));
    d.frames(2);
    d.drag("program.transform.handle.7", -30.0, 0.0);
    assert!(d.num(clip, "transform", "scale_width") > 55.0);
    assert_eq!(d.num(clip, "transform", "scale_height"), 50.0);
    let corners: Vec<(f64, f64)> = (0..4).map(|n| d.center(&format!("program.transform.handle.{n}"))).collect();
    d.drag("program.transform.anchor", -25.0, 15.0);
    for (n, c) in corners.iter().enumerate() {
        assert!(near(d.center(&format!("program.transform.handle.{n}")), *c), "corner {n} stays put");
    }

    // double-clicking a picture selects its clip's Motion: the overlay on V2, then V1 under it
    let seq = d.exec("sequence.inspect", json!({}));
    let overlay = seq["video"][1]["items"][0]["clip"].as_u64().unwrap();
    let start = seq["video"][1]["items"][0]["start"].as_i64().unwrap();
    d.exec("playhead.set", json!({"time": start + filmcraft_time::TICKS_PER_SECOND}));
    d.frames(3);
    let pic = d.rect("program.picture");
    d.ok("ui.click", json!({"x": pic[0] + pic[2] * 0.8, "y": pic[1] + pic[3] * 0.76, "count": 2}));
    d.frames(3);
    assert_eq!(d.selected_effect(), json!({"clip": overlay, "effect": "motion", "instance": 0}));
    let sel = d.exec("state.inspect", json!({}))["selection"].clone();
    assert!(sel.as_array().unwrap().contains(&json!(overlay)), "{sel}");
    d.rect("program.transform.anchor");
    d.shot("transform-double-click-overlay");
    // half a second later, or egui counts the next clicks as a triple click
    d.frames(30);
    d.ok("ui.click", json!({"x": pic[0] + pic[2] * 0.2, "y": pic[1] + pic[3] * 0.2, "count": 2}));
    d.frames(3);
    let under = d.exec("sequence.inspect", json!({}))["video"][0]["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|it| {
            let (s, l) = (it["start"].as_i64().unwrap(), it["duration"].as_i64().unwrap());
            (s..s + l).contains(&(start + filmcraft_time::TICKS_PER_SECOND))
        })
        .unwrap()["clip"]
        .as_u64()
        .unwrap();
    assert_eq!(d.selected_effect(), json!({"clip": under, "effect": "motion", "instance": 0}));
}

/// Only the left button takes hold of the box: a middle or right drag leaves the picture alone.
#[test]
fn only_the_left_button_drags_the_handles() {
    let mut d = Driver::demo();
    let (clip, _) = d.first_clip();
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "scale", "value": 50.0}));
    d.click("effectControls.effect.motion");
    let (p0, undo) = (d.point(clip, "motion", "position"), d.undo_len());
    let b = d.rect("program.transform.box");
    let from = (b[0] + b[2] * 0.3, b[1] + b[3] * 0.3);
    for button in [egui::PointerButton::Middle, egui::PointerButton::Secondary] {
        d.drag_with(from, button);
        assert_eq!(d.point(clip, "motion", "position"), p0, "{button:?} drag moved the picture");
        assert_eq!(d.undo_len(), undo, "{button:?} drag made an undo step");
    }
    d.drag_from(from, 40.0, 20.0);
    assert_ne!(d.point(clip, "motion", "position"), p0, "the left button still moves it");
}

/// The other things dragged on the Program monitor take the left button only too: a guide, a
/// mask's body and vertices, and a graphic layer stay put under a middle or right drag.
#[test]
fn guides_masks_and_graphics_take_the_left_button_only() {
    let mut d = Driver::demo();
    let (clip, _) = d.first_clip();
    let others = [egui::PointerButton::Middle, egui::PointerButton::Secondary];
    let project = |d: &Driver| d.harness.state().session.project.to_json();
    // a guide
    d.ok("ui.menu.invoke", json!({"id": "view.addGuide", "params": {"orientation": "vertical", "position": 700}}));
    d.frames(2);
    let g = d.rect("program.guide.0");
    for b in others {
        d.drag_with((g[0] + g[2] / 2.0, g[1] + g[3] * 0.2), b);
        assert_eq!(d.rect("program.guide.0"), g, "a {b:?} drag moved the guide");
    }
    // a mask
    d.exec("effects.apply", json!({"clips": [clip], "effect": "gaussian_blur"}));
    let fx = {
        let s = &d.harness.state().session;
        s.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.effects.iter().position(|e| e.effect == "gaussian_blur").unwrap()
    };
    d.exec("masks.add", json!({"clip": clip, "effect": fx, "shape": "ellipse"}));
    d.exec("masks.select", json!({"clip": clip, "effect": fx, "mask": 0}));
    d.frames(3);
    let before = project(&d);
    for id in ["program.mask.body", "program.mask.vertex.0"] {
        for b in others {
            let at = d.center(id);
            d.drag_with(at, b);
            assert_eq!(project(&d), before, "a {b:?} drag on {id} changed the mask");
        }
    }
    // a graphic layer
    d.exec("masks.select", json!({"none": true}));
    let gclip = d.exec("graphics.newText", json!({"text": "Title", "position": [600, 500], "size": 120}))["clip"].as_u64().unwrap();
    d.frames(3);
    let before = project(&d);
    let at = d.center(&format!("program.layer.{gclip}.0"));
    for b in others {
        d.drag_with(at, b);
        assert_eq!(project(&d), before, "a {b:?} drag moved the layer");
    }
    d.drag_from(at, 40.0, 20.0);
    assert_ne!(project(&d), before, "the left button still moves the layer");
}
/// Anywhere just outside the box rotates, beside the middle of an edge as beside a corner.
#[test]
fn rotating_from_beside_an_edge() {
    let mut d = Driver::demo();
    let (clip, _) = d.first_clip();
    d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "scale", "value": 50.0}));
    d.click("effectControls.effect.motion");
    let left = d.rect("program.transform.handle.7");
    let (x, y) = (left[0] + left[2] / 2.0 - 14.0, left[1] + left[3] / 2.0);
    d.drag_from((x, y), 0.0, -60.0);
    let r = d.num(clip, "motion", "rotation");
    assert!(r > 5.0, "up from beside the left edge turns clockwise: {r}");
}

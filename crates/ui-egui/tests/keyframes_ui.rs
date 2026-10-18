//! Keyframes in the Properties and Effect Controls panels, driven headless through the control
//! channel: the Properties panel's keyframe navigator (◀ ◆ ▶) adds and removes the keyframe at the
//! playhead and steps between keyframes, as Premiere's does, and the Effect Controls time ruler
//! moves the playhead (click, or drag the playhead's handle).
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the tests also render the UI with wgpu and write PNGs there.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, Interpolation, Param};
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
    /// The demo project with its first video clip selected and both panels showing.
    fn demo() -> (Self, Clip) {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots };
        d.frames(4);
        let seq = d.exec("sequence.inspect", json!({}));
        let c = &seq["video"][0]["items"][0];
        let clip = Clip { id: c["clip"].as_u64().unwrap(), start: c["start"].as_i64().unwrap(), duration: c["duration"].as_i64().unwrap() };
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
    fn gesture(&mut self, from: egui::Pos2, to: egui::Pos2, modifiers: egui::Modifiers, shot: Option<&str>) {
        self.press_drag(from, to, modifiers);
        if let Some(name) = shot {
            self.shot(name);
        }
        self.release(to, modifiers);
    }
    fn press_drag(&mut self, from: egui::Pos2, to: egui::Pos2, modifiers: egui::Modifiers) {
        self.harness.input_mut().events.push(egui::Event::ModifiersChanged(modifiers));
        self.harness.input_mut().events.push(egui::Event::PointerMoved(from));
        self.frames(1);
        self.harness.input_mut().events.push(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers });
        self.frames(1);
        for i in 1..=8 {
            self.harness.input_mut().events.push(egui::Event::ModifiersChanged(modifiers));
            self.harness.input_mut().events.push(egui::Event::PointerMoved(from.lerp(to, i as f32 / 8.0)));
            self.frames(1);
        }
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
    /// The clip's Motion ▸ Scale: its keyframe times (media time) and its value at the playhead.
    fn scale(&mut self, clip: &Clip) -> (Vec<i64>, f64) {
        let s = &self.harness.state().session;
        let it = s.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1;
        let p = it.effect("motion").unwrap().param("scale").unwrap();
        (p.keyframes.iter().map(|k| k.time.0).collect(), p.f64_at(it.source_time_at(s.playhead())))
    }
    fn parameter(&self, clip: &Clip, effect: &str, param: &str) -> Param {
        self.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effect(effect).unwrap().param(param).unwrap().clone()
    }
    fn undo_len(&mut self) -> usize {
        self.exec("history.list", json!({}))["undo"].as_array().unwrap().len()
    }
    fn right_click(&mut self, id: &str) {
        self.rect(id);
        self.ok("ui.click", json!({"id": id, "button": "right"}));
        self.frames(3);
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

struct Clip {
    id: u64,
    start: i64,
    duration: i64,
}

const SCALE: &str = "properties.motion.scale";

/// The reported case: two Scale keyframes, the second set to 50 %. The Properties diamond removes
/// only the keyframe at the playhead, so Scale is 100 % again there (both panels read the same
/// parameter); it used to switch animation off and keep 50 %.
#[test]
fn properties_diamond_adds_and_removes_the_keyframe_at_the_playhead() {
    let (mut d, clip) = Driver::demo();
    // not animated: the diamond alone
    d.rect(&format!("{SCALE}.addKeyframe"));
    assert!(d.find(&format!("{SCALE}.prevKeyframe")).is_none(), "no arrows before the parameter is animated");

    let t0 = d.seek(0.5);
    d.click(&format!("{SCALE}.addKeyframe"));
    assert_eq!(d.scale(&clip).0.len(), 1, "the diamond turns animation on with a keyframe at the playhead");
    // animated: the arrows appear, in Properties and in Effect Controls
    for panel in [SCALE, "effectControls.motion.scale"] {
        d.rect(&format!("{panel}.prevKeyframe"));
        d.rect(&format!("{panel}.nextKeyframe"));
    }

    let t1 = d.seek(2.5);
    d.click(&format!("{SCALE}.addKeyframe"));
    assert_eq!(d.scale(&clip).0.len(), 2, "a second keyframe at the playhead");
    d.exec("effects.setParam", json!({"clip": clip.id, "effect": "motion", "param": "scale", "value": 50.0}));
    assert_eq!(d.scale(&clip).1, 50.0);
    d.shot("keyframes-two");

    // on a keyframe: the diamond removes that keyframe only
    d.click(&format!("{SCALE}.addKeyframe"));
    let (keys, value) = d.scale(&clip);
    assert_eq!(keys.len(), 1, "only the keyframe at the playhead went: {keys:?}");
    assert_eq!(value, 100.0, "Scale falls back to the remaining keyframe's value");
    assert_eq!(d.playhead(), t1);
    d.shot("keyframes-removed");
    d.exec("edit.undo", json!({}));
    let (keys, value) = d.scale(&clip);
    assert_eq!((keys.len(), value), (2, 50.0), "one undo step brings the keyframe back");

    // the arrows step between the keyframes
    d.click(&format!("{SCALE}.prevKeyframe"));
    assert_eq!(d.playhead(), t0, "◀ goes to the previous keyframe");
    assert_eq!(d.scale(&clip).1, 100.0);
    d.click(&format!("{SCALE}.prevKeyframe"));
    assert_eq!(d.playhead(), t0, "no keyframe before the first: ◀ does nothing");
    d.click(&format!("{SCALE}.nextKeyframe"));
    assert_eq!(d.playhead(), t1, "▶ goes to the next keyframe");
    assert_eq!(d.scale(&clip).1, 50.0);
    d.click("effectControls.motion.scale.prevKeyframe");
    assert_eq!(d.playhead(), t0, "the Effect Controls arrows do the same");

    // removing the last keyframe ends the animation; the arrows go
    d.click(&format!("{SCALE}.addKeyframe"));
    d.click(&format!("{SCALE}.nextKeyframe"));
    d.click(&format!("{SCALE}.addKeyframe"));
    let (keys, value) = d.scale(&clip);
    assert!(keys.is_empty(), "{keys:?}");
    assert_eq!(value, 50.0, "the last keyframe's value stays");
    assert!(d.find(&format!("{SCALE}.prevKeyframe")).is_none());
}

/// #284: every Effect Controls property has a reset button; on an animated property it puts the
/// default in as a keyframe at the playhead and keeps the other keyframes.
#[test]
fn effect_controls_reset_button_keeps_keyframes() {
    let (mut d, clip) = Driver::demo();
    let t0 = d.seek(0.5);
    d.click(&format!("{SCALE}.addKeyframe"));
    d.exec("effects.setParam", json!({"clip": clip.id, "effect": "motion", "param": "scale", "value": 150.0, "time": t0}));
    d.seek(2.5);
    d.click("effectControls.motion.scale.reset");
    d.shot("effect-controls-reset");
    let (keys, value) = d.scale(&clip);
    assert_eq!(keys.len(), 2, "the keyframe stays and the default is keyed at the playhead: {keys:?}");
    assert_eq!(value, 100.0);
}

/// Crop is not on a clip until it is used: its diamond applies the effect, then adds the keyframe.
#[test]
fn properties_diamond_applies_crop_first() {
    let (mut d, clip) = Driver::demo();
    d.seek(1.0);
    d.click("properties.crop.left.addKeyframe");
    let s = &d.harness.state().session;
    let it = s.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1;
    assert_eq!(it.effect("crop").expect("crop applied").param("left").unwrap().keyframes.len(), 1);
    d.rect("properties.crop.left.nextKeyframe");
}

/// The Effect Controls time ruler: a click moves the playhead there, the playhead's handle drags,
/// and there is no playhead while it is off the clip.
#[test]
fn effect_controls_ruler_moves_the_playhead() {
    let (mut d, clip) = Driver::demo();
    let frame = Tick::from_seconds_f64(1.0 / 23.976).0;
    let [x, y, w, h] = d.rect("effectControls.ruler");
    let lane = d.rect("effectControls.lane");
    assert_eq!((x, w), (lane[0], lane[2]), "the ruler spans the keyframe lane");
    let at = |f: f64| clip.start + (f * clip.duration as f64) as i64;

    d.ok("ui.click", json!({"x": x + w * 0.25, "y": y + h * 0.3}));
    d.frames(2);
    assert!((d.playhead() - at(0.25)).abs() <= frame, "a click on the ruler: {} vs {}", d.playhead(), at(0.25));
    let head = d.rect("effectControls.playhead");
    assert!((head[0] + head[2] / 2.0 - (x + w * 0.25)).abs() <= 2.0, "the handle sits where the playhead is: {head:?}");
    d.shot("keyframes-ruler");

    // grab the handle and drag it right
    let (hx, hy) = (head[0] + head[2] / 2.0, head[1] + head[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": hx, "y": hy}, "to": {"x": hx + w * 0.5, "y": hy}, "steps": 6}));
    d.frames(2);
    assert!((d.playhead() - at(0.75)).abs() <= frame, "dragging the handle: {} vs {}", d.playhead(), at(0.75));

    // the playhead is drawn only on the clip
    d.exec("playhead.set", json!({"time": clip.start + clip.duration * 2}));
    assert!(d.find("effectControls.playhead").is_none(), "no handle while the playhead is past the clip");
    d.rect("effectControls.ruler");
}

/// #412: Scale and Position both have a keyframe at the same time. A click on Scale's keyframe in
/// the Effect Controls lane selects that keyframe only (every keyframe under the playhead, which
/// the click moves there, used to light up); a keyframe that is gone drops out of the selection.
#[test]
fn clicking_a_lane_keyframe_selects_only_that_keyframe() {
    let (mut d, clip) = Driver::demo();
    let t0 = d.seek(1.0);
    for param in ["scale", "position"] {
        d.exec("effects.addKeyframe", json!({"clip": clip.id, "effect": "motion", "param": param}));
    }
    let time = d.scale(&clip).0[0];
    let motion = {
        let s = &d.harness.state().session;
        let it = s.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1;
        it.effects.iter().position(|e| e.effect == "motion").unwrap()
    };
    let selection = |d: &mut Driver| d.ok("ui.inspect", json!({}))["ui"]["keyframe_selection"].clone();
    d.seek(2.0);
    assert_eq!(selection(&mut d), json!([]), "nothing selected yet");

    d.click(&format!("effectControls.motion.scale.keyframe.{time}"));
    assert_eq!(selection(&mut d), json!([{"clip": clip.id, "effect": motion, "param": "scale", "time": time}]), "only Scale's keyframe");
    assert_eq!(d.playhead(), t0, "the click still moves the playhead to the keyframe");
    d.shot("keyframes-select-one");

    d.click(&format!("effectControls.motion.position.keyframe.{time}"));
    assert_eq!(selection(&mut d), json!([{"clip": clip.id, "effect": motion, "param": "position", "time": time}]), "a click replaces the selection");

    // the diamond removes the keyframe at the playhead, the selected one: the selection empties
    d.click("effectControls.motion.position.addKeyframe");
    assert_eq!(selection(&mut d), json!([]), "a deleted keyframe is not selected");
}

#[test]
fn select_all_in_effect_controls_selects_its_keyframes_not_the_clips() {
    // #1241
    let (mut d, clip) = Driver::demo();
    d.seek(1.0);
    for param in ["scale", "position"] {
        d.exec("effects.addKeyframe", json!({"clip": clip.id, "effect": "motion", "param": param}));
    }
    d.seek(2.0);
    d.exec("effects.addKeyframe", json!({"clip": clip.id, "effect": "motion", "param": "scale"}));
    let clips = d.ok("ui.inspect", json!({}))["selection"].clone();
    d.ok("ui.set", json!({"focused": "EffectControls"}));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.frames(2);
    let inspect = d.ok("ui.inspect", json!({}));
    let keys = inspect["ui"]["keyframe_selection"].as_array().cloned().unwrap_or_default();
    assert_eq!(keys.len(), 3, "{keys:?}");
    assert!(keys.iter().all(|k| k["clip"] == clip.id));
    assert_eq!(inspect["selection"], clips, "the Timeline selection is unchanged");
    // with the Timeline focused, Select All still selects the clips
    d.ok("ui.set", json!({"focused": "Timeline"}));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.frames(2);
    assert!(d.ok("ui.inspect", json!({}))["selection"].as_array().map_or(0, Vec::len) > clips.as_array().map_or(0, Vec::len));
}

/// Effect Controls' divider (#643): dragging it widens the effect list or the keyframe area, each
/// keeping a minimum width, and the width stays. The mouse wheel scrolls the rows over the
/// keyframe area too, not only over the effect list.
#[test]
fn effect_controls_divider_and_scrolling_anywhere() {
    let (mut d, clip) = Driver::demo();
    let lane = d.rect("effectControls.lane");
    let div = d.rect("effectControls.divider");
    let (x, y) = (div[0] + div[2] / 2.0, div[1] + div[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 80.0, "y": y}, "steps": 6}));
    let narrower = d.rect("effectControls.lane");
    assert!((narrower[0] - (lane[0] + 80.0)).abs() <= 1.0, "the keyframe area starts 80 points further right: {narrower:?} vs {lane:?}");
    assert!((narrower[0] + narrower[2] - (lane[0] + lane[2])).abs() <= 1.0, "and still ends at the panel's edge");
    d.frames(3);
    assert_eq!(d.rect("effectControls.lane"), narrower, "the width stays");
    // dragged all the way right, the keyframe area keeps its minimum width
    let drag_divider = |d: &mut Driver, by: f64| {
        let div = d.rect("effectControls.divider");
        let (x, y) = (div[0] + div[2] / 2.0, div[1] + div[3] / 2.0);
        d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + by, "y": y}, "steps": 6}));
    };
    drag_divider(&mut d, 3000.0);
    assert!(d.rect("effectControls.lane")[2] >= 59.0, "the keyframe area can't vanish");
    // and all the way left, the effect list keeps the width its rows are laid out for
    drag_divider(&mut d, -3000.0);
    assert!(d.rect("effectControls.effect.motion")[2] >= 259.0, "the effect list stays wide enough for its values");

    // enough effects to scroll; the wheel over the keyframe area scrolls the rows
    for fx in ["gaussian_blur", "transform", "crop", "tint"] {
        d.exec("effects.apply", json!({"clips": [clip.id], "effect": fx}));
    }
    let before = d.rect("effectControls.motion.scale.stopwatch");
    let lane = d.rect("effectControls.lane");
    d.ok("ui.scroll", json!({"x": lane[0] + lane[2] / 2.0, "y": lane[1] + lane[3] * 0.7, "dx": 0.0, "dy": -80.0}));
    d.frames(4);
    let after = d.find("effectControls.motion.scale.stopwatch");
    assert!(after.is_none_or(|r| r[1] < before[1] - 20.0), "the rows scrolled up: {before:?} → {after:?}");
}

/// #967: empty lane space clears both the selected refs and the highlight, without an edit.
#[test]
fn empty_keyframe_lane_click_clears_selection() {
    let (mut d, clip) = Driver::demo();
    d.seek(1.0);
    d.exec("effects.addKeyframe", json!({"clip":clip.id,"effect":"motion","param":"scale"}));
    let time = d.scale(&clip).0[0];
    let id = format!("effectControls.motion.scale.keyframe.{time}");
    let r = d.rect(&id);
    let lane = d.rect("effectControls.lane");
    d.click(&id);
    assert_eq!(d.harness.state().ui.keyframe_selection.len(), 1);
    let undo = d.harness.state().session.history.undo.len();
    d.ok("ui.click", json!({"x":lane[0] + lane[2] * 0.9,"y":r[1] + r[3]/2.0}));
    d.frames(3);
    assert!(d.harness.state().ui.keyframe_selection.is_empty(), "empty lane clicks must clear the highlight");
    assert_eq!(d.harness.state().session.history.undo.len(), undo);
    d.shot("keyframes-selection-cleared");
}

/// #967: one rectangle selects times across property rows and leaves outside keys alone.
#[test]
fn marquee_selects_keyframes_across_property_rows() {
    let (mut d, clip) = Driver::demo();
    let mut keys = Vec::new();
    for (param, seconds) in [("scale", 1.0), ("scale", 2.0), ("rotation", 1.5), ("position", 3.0)] {
        let timeline = d.seek(seconds);
        d.exec("effects.addKeyframe", json!({"clip":clip.id,"effect":"motion","param":param}));
        let it = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1;
        keys.push((param, it.source_time_at(Tick(timeline)).0));
    }
    let rects: Vec<_> = keys.iter().take(3).map(|(param, time)| d.rect(&format!("effectControls.motion.{param}.keyframe.{time}"))).collect();
    let from = egui::pos2(
        rects.iter().map(|r| r[0]).fold(f64::INFINITY, f64::min) as f32 - 14.0,
        rects.iter().map(|r| r[1]).fold(f64::INFINITY, f64::min) as f32 - 12.0,
    );
    let to = egui::pos2(
        rects.iter().map(|r| r[0] + r[2]).fold(f64::NEG_INFINITY, f64::max) as f32 + 14.0,
        rects.iter().map(|r| r[1] + r[3]).fold(f64::NEG_INFINITY, f64::max) as f32 + 12.0,
    );
    let undo = d.harness.state().session.history.undo.len();
    d.gesture(from, to, egui::Modifiers::default(), Some("keyframes-marquee-drag"));
    let selected = &d.harness.state().ui.keyframe_selection;
    assert_eq!(selected.len(), 3, "marquee must select Scale and Rotation together");
    for (param, time) in keys.iter().take(3) {
        assert!(selected.iter().any(|k| k.clip == clip.id && k.param == *param && k.time.0 == *time));
    }
    assert_eq!(d.harness.state().session.history.undo.len(), undo, "selection is not a project edit");
    d.shot("keyframes-marquee-selected");
}

fn lane_keys(d: &mut Driver, clip: &Clip, specs: &[(&str, f64)]) -> Vec<filmcraft_ui_egui::state::KeyframeRef> {
    specs
        .iter()
        .map(|(param, seconds)| {
            let time = d.seek(*seconds);
            d.exec("effects.addKeyframe", json!({"clip":clip.id,"effect":"motion","param":param}));
            let session = &d.harness.state().session;
            let it = session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1;
            filmcraft_ui_egui::state::KeyframeRef {
                clip: clip.id,
                effect: it.effects.iter().position(|e| e.effect == "motion").unwrap(),
                param: (*param).into(),
                mask: None,
                time: it.source_time_at(Tick(time)),
            }
        })
        .collect()
}

fn key_id(d: &Driver, key: &filmcraft_ui_egui::state::KeyframeRef) -> String {
    let it = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(key.clip)).unwrap().1;
    let mask = key.mask.map_or_else(String::new, |m| format!("mask{m}."));
    format!("effectControls.{}.{mask}{}.keyframe.{}", it.effects[key.effect].effect, key.param, key.time.0)
}

fn center(rect: [f64; 4]) -> egui::Pos2 {
    egui::pos2((rect[0] + rect[2] / 2.0) as f32, (rect[1] + rect[3] / 2.0) as f32)
}

fn select_keys(d: &mut Driver, keys: &[filmcraft_ui_egui::state::KeyframeRef]) {
    for (i, key) in keys.iter().enumerate() {
        let id = key_id(d, key);
        d.rect(&id);
        d.ok("ui.click", json!({"id":id,"modifiers":{"shift":i>0}}));
        d.frames(3);
    }
    assert_eq!(d.harness.state().ui.keyframe_selection, keys);
}

#[test]
fn selected_keyframes_move_together_and_undo_as_one_edit() {
    let (mut d, clip) = Driver::demo();
    let keys = lane_keys(&mut d, &clip, &[("scale", 1.0), ("scale", 2.0), ("rotation", 1.5)]);
    select_keys(&mut d, &keys);
    let lane = d.rect("effectControls.lane");
    let from = center(d.rect(&key_id(&d, &keys[0])));
    let undo = d.harness.state().session.history.undo.len();
    let before = serde_json::to_value(&d.harness.state().session.project).unwrap();
    d.gesture(from, from + egui::vec2(lane[2] as f32 * 0.15, 0.0), egui::Modifiers::NONE, Some("keyframes-group-preview"));
    let selected = &d.harness.state().ui.keyframe_selection;
    assert_eq!(selected.len(), 3);
    let delta = selected[0].time - keys[0].time;
    assert!(delta > Tick::ZERO);
    for (old, new) in keys.iter().zip(selected) {
        assert_eq!(new.time, old.time + delta);
        assert_eq!(new.param, old.param);
    }
    assert_eq!(d.harness.state().session.history.undo.len(), undo + 1);
    let after = serde_json::to_value(&d.harness.state().session.project).unwrap();
    d.exec("edit.undo", json!({}));
    assert_eq!(serde_json::to_value(&d.harness.state().session.project).unwrap(), before);
    d.exec("edit.redo", json!({}));
    assert_eq!(serde_json::to_value(&d.harness.state().session.project).unwrap(), after);
}

#[test]
fn shift_snaps_the_group_to_other_rows_at_different_lane_widths() {
    for (width, theme) in [(280.0, "dark"), (480.0, "light")] {
        let (mut d, clip) = Driver::demo();
        d.harness.state_mut().ui.effect_controls_split = width;
        d.ok("ui.set", json!({"theme":theme}));
        let at = |f: f64| Tick(clip.start).seconds() + Tick(clip.duration).seconds() * f;
        let keys = lane_keys(&mut d, &clip, &[("scale", at(0.2)), ("scale", at(0.4)), ("rotation", at(0.65))]);
        select_keys(&mut d, &keys[..2]);
        d.seek(0.0);
        let from = center(d.rect(&key_id(&d, &keys[0])));
        let last = center(d.rect(&key_id(&d, &keys[1])));
        let target = center(d.rect(&key_id(&d, &keys[2])));
        let to = from + egui::vec2(target.x - last.x - 3.0, 0.0);
        let shift = egui::Modifiers { shift: true, ..egui::Modifiers::NONE };
        d.press_drag(from, to, shift);
        let guide = d.rect("effectControls.snapTarget");
        assert!(((guide[0] + guide[2] / 2.0) as f32 - target.x).abs() < 1.0);
        d.shot(&format!("keyframes-snap-row-{theme}"));
        d.release(to, shift);
        let selected = &d.harness.state().ui.keyframe_selection;
        assert_eq!(selected.len(), 2);
        assert_eq!(selected[1].time, keys[2].time, "the rightmost selected key snaps to Rotation's time");
        assert_eq!(selected[1].time - selected[0].time, keys[1].time - keys[0].time);
    }
}

#[test]
fn shift_snaps_to_playhead_with_a_screen_space_threshold() {
    for (width, theme) in [(280.0, "dark"), (480.0, "light")] {
        let (mut d, clip) = Driver::demo();
        d.harness.state_mut().ui.effect_controls_split = width;
        d.ok("ui.set", json!({"theme":theme}));
        let at = |f: f64| Tick(clip.start).seconds() + Tick(clip.duration).seconds() * f;
        let keys = lane_keys(&mut d, &clip, &[("scale", at(0.2)), ("scale", at(0.4))]);
        select_keys(&mut d, &keys);
        let target_time = Tick(d.seek(at(0.6)));
        let target_media = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.source_time_at(target_time);
        let from = center(d.rect(&key_id(&d, &keys[0])));
        let target = center(d.rect("effectControls.playhead"));
        let shift = egui::Modifiers { shift: true, ..egui::Modifiers::NONE };
        let to = egui::pos2(target.x + 3.0, from.y);
        d.press_drag(from, to, shift);
        assert!((center(d.rect("effectControls.snapTarget")).x - target.x).abs() < 1.0);
        d.shot(&format!("keyframes-snap-playhead-{theme}"));
        d.release(to, shift);
        assert_eq!(d.harness.state().ui.keyframe_selection[0].time, target_media);
        d.exec("edit.undo", json!({}));
        select_keys(&mut d, &keys);
        let outside = egui::pos2(target.x + 16.0, from.y);
        d.press_drag(from, outside, shift);
        assert!(d.find("effectControls.snapTarget").is_none(), "a target more than 8 screen points away must not snap");
        d.release(outside, shift);
        assert_ne!(d.harness.state().ui.keyframe_selection[0].time, target_media);
    }
}

#[test]
fn escape_cancels_a_group_preview_without_editing_the_project() {
    let (mut d, clip) = Driver::demo();
    let keys = lane_keys(&mut d, &clip, &[("scale", 1.0), ("scale", 1.5)]);
    select_keys(&mut d, &keys);
    let from = center(d.rect(&key_id(&d, &keys[0])));
    let lane = d.rect("effectControls.lane");
    let to = from + egui::vec2(lane[2] as f32 * 0.2, 0.0);
    let project = d.harness.state().session.project.clone();
    let undo = d.harness.state().session.history.undo.len();
    d.press_drag(from, to, egui::Modifiers::NONE);
    assert!(std::sync::Arc::ptr_eq(&project, &d.harness.state().session.project), "drag previews are not project edits");
    d.ok("ui.key", json!({"key":"Escape"}));
    d.release(to, egui::Modifiers::NONE);
    assert!(std::sync::Arc::ptr_eq(&project, &d.harness.state().session.project));
    assert_eq!(d.harness.state().session.history.undo.len(), undo);
    assert_eq!(d.harness.state().ui.keyframe_selection, keys);
    d.gesture(from, to, egui::Modifiers::NONE, None);
    assert_eq!(d.harness.state().session.history.undo.len(), undo + 1, "a new gesture works after cancellation");
}

#[test]
fn effect_and_mask_keyframes_move_together_without_crossing_parameters() {
    let (mut d, clip) = Driver::demo();
    d.click("effectControls.effect.motion");
    d.exec("masks.add", json!({"clip":clip.id,"effect":"opacity","shape":"ellipse"}));
    let mut keys = Vec::new();
    for (mask, seconds) in [(None, 1.0), (Some(0), 1.5)] {
        let time = Tick(d.seek(seconds));
        let mut p = json!({"clip":clip.id,"effect":"opacity","param":"opacity"});
        if let Some(m) = mask {
            p["mask"] = json!(m);
        }
        d.exec("effects.addKeyframe", p);
        let it = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1;
        keys.push(filmcraft_ui_egui::state::KeyframeRef {
            clip: clip.id,
            effect: it.effects.iter().position(|e| e.effect == "opacity").unwrap(),
            param: "opacity".into(),
            mask,
            time: it.source_time_at(time),
        });
    }
    select_keys(&mut d, &keys);
    let from = center(d.rect(&key_id(&d, &keys[0])));
    let lane = d.rect("effectControls.lane");
    d.gesture(from, from + egui::vec2(lane[2] as f32 * 0.1, 0.0), egui::Modifiers::NONE, Some("keyframes-mask-group"));
    let selected = &d.harness.state().ui.keyframe_selection;
    assert_eq!(selected.len(), 2);
    let delta = selected[0].time - keys[0].time;
    assert!(delta > Tick::ZERO);
    assert_eq!(selected[1].time - keys[1].time, delta);
    let it = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1;
    assert!(it.effects[keys[0].effect].masks[0].feather.keyframes.is_empty());
}

/// #962: graph points need the same interpolation menu as lane diamonds, including either
/// coordinate of Transform Position. Opening a menu never changes the value or creates an undo.
#[test]
fn graph_keyframes_offer_interpolation_and_clear_in_one_undo_step() {
    for (effect, param, values, component) in [
        ("motion", "scale", [json!(40.0), json!(80.0)], ""),
        ("motion", "position", [json!([400.0, 300.0]), json!([800.0, 500.0])], ".x"),
        ("transform", "position", [json!([400.0, 300.0]), json!([800.0, 500.0])], ".x"),
        ("transform", "position", [json!([400.0, 300.0]), json!([800.0, 500.0])], ".y"),
    ] {
        let (mut d, clip) = Driver::demo();
        if effect == "transform" {
            d.exec("effects.apply", json!({"clips": [clip.id], "effect": effect}));
            for intrinsic in ["motion", "opacity", "time_remap"] {
                d.click(&format!("effectControls.effect.{intrinsic}.twirl"));
            }
        }
        for (seconds, value) in [0.5, 2.5].into_iter().zip(values) {
            d.seek(seconds);
            d.exec("effects.addKeyframe", json!({"clip": clip.id, "effect": effect, "param": param}));
            d.exec("effects.setParam", json!({"clip": clip.id, "effect": effect, "param": param, "value": value}));
        }
        d.click(&format!("effectControls.{effect}.{param}.graphs"));
        let before = d.parameter(&clip, effect, param);
        let undo = d.undo_len();
        let target = format!("effectControls.{param}.graph.keyframe.{}{component}", before.keyframes[0].time.0);
        for (key, interpolation) in [
            ("bezier", Interpolation::Bezier),
            ("autoBezier", Interpolation::AutoBezier),
            ("continuousBezier", Interpolation::ContinuousBezier),
            ("hold", Interpolation::Hold),
            ("easeIn", Interpolation::EaseIn),
            ("easeOut", Interpolation::EaseOut),
            ("linear", Interpolation::Linear),
        ] {
            d.right_click(&target);
            assert_eq!(d.parameter(&clip, effect, param), before, "opening the graph menu cannot edit {effect}.{param}{component}");
            assert_eq!(d.undo_len(), undo);
            if key == "easeIn" && effect == "transform" {
                d.shot(&format!("graph-menu-{effect}-{param}{component}"));
            }
            d.click(&format!("effectControls.keyframeMenu.{key}"));
            let changed = d.parameter(&clip, effect, param);
            assert_eq!(changed.keyframes[0].interp, interpolation);
            assert_eq!(changed.keyframes[0].time, before.keyframes[0].time);
            assert_eq!(changed.keyframes[0].value, before.keyframes[0].value);
            assert_eq!(changed.keyframes[1], before.keyframes[1]);
            assert_eq!(d.undo_len(), undo + 1);
            assert!(d.find("effectControls.keyframeMenu.clear").is_none(), "choosing interpolation closes the menu");
            d.exec("edit.undo", json!({}));
            assert_eq!(d.parameter(&clip, effect, param), before);
            d.exec("edit.redo", json!({}));
            assert_eq!(d.parameter(&clip, effect, param), changed);
            d.exec("edit.undo", json!({}));
        }
        d.right_click(&target);
        d.click("effectControls.keyframeMenu.clear");
        let remaining = d.parameter(&clip, effect, param);
        assert_eq!(remaining.keyframes, before.keyframes[1..]);
        assert_eq!(d.undo_len(), undo + 1);
        d.exec("edit.undo", json!({}));
        assert_eq!(d.parameter(&clip, effect, param), before);
    }
}

#[test]
fn mask_keyframe_menus_preserve_the_effect_and_other_masks() {
    let (mut d, clip) = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Effects"}));
    for intrinsic in ["motion", "time_remap"] {
        d.click(&format!("effectControls.effect.{intrinsic}.twirl"));
    }
    for mask in 0..2 {
        d.exec("masks.add", json!({"clip": clip.id, "effect": "opacity", "shape": "ellipse"}));
        for (seconds, value) in [(0.5, 20.0), (2.5, 60.0)] {
            d.seek(seconds);
            d.exec("effects.addKeyframe", json!({"clip": clip.id, "effect": "opacity", "mask": mask, "param": "feather"}));
            d.exec("effects.setParam", json!({"clip": clip.id, "effect": "opacity", "mask": mask, "param": "feather", "value": value}));
        }
    }
    let effect = |d: &Driver| d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effect("opacity").unwrap().clone();
    let before = effect(&d);
    let time = before.masks[0].param("feather").unwrap().keyframes[0].time.0;
    let undo = d.undo_len();
    for (target, key, interpolation) in [
        (format!("effectControls.opacity.mask0.feather.keyframe.{time}"), "easeOut", Interpolation::EaseOut),
        (format!("effectControls.feather.graph.keyframe.{time}"), "easeIn", Interpolation::EaseIn),
    ] {
        if key == "easeIn" {
            d.click("effectControls.opacity.mask0.feather.graphs");
        }
        d.right_click(&target);
        assert_eq!(effect(&d), before, "opening the menu cannot change any mask");
        assert_eq!(d.undo_len(), undo);
        d.click(&format!("effectControls.keyframeMenu.{key}"));
        let mut expected = before.clone();
        expected.masks[0].param_mut("feather").unwrap().keyframes[0].interp = interpolation;
        assert_eq!(effect(&d), expected, "only the first mask's first feather keyframe changes");
        assert_eq!(d.undo_len(), undo + 1);
        d.exec("edit.undo", json!({}));
        assert_eq!(effect(&d), before);
    }
}

fn reorder_demo() -> (Driver, Clip) {
    let (mut d, clip) = Driver::demo();
    d.exec("timeline.select", json!({"clips":[clip.id],"linked":false}));
    for effect in ["rounded_crop", "drop_shadow"] {
        d.exec("effects.apply", json!({"clips":[clip.id],"effect":effect}));
    }
    let ids: Vec<String> =
        d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects.iter().map(|e| e.effect.clone()).collect();
    for id in ids {
        d.click(&format!("effectControls.effect.{id}.twirl"));
    }
    (d, clip)
}

#[test]
fn effect_header_drag_reorders_only_on_drop_and_is_one_undo_step() {
    let (mut d, clip) = reorder_demo();
    d.exec("effects.setParam", json!({"clip":clip.id,"effect":0,"param":"left","value":10,"time":0,"keyframe":true}));
    let key_time = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects[0].params["left"].keyframes[0].time.0;
    d.ok("ui.set", json!({"keyframeSelection":[{"clip":clip.id,"effect":0,"param":"left","time":key_time}]}));
    assert_eq!(d.harness.state().ui.keyframe_selection.len(), 1);
    let before = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects.clone();
    let mark = d.harness.state().session.history.undo.len();
    let a = d.rect("effectControls.effect.drop_shadow");
    let b = d.rect("effectControls.effect.rounded_crop");
    let from = egui::pos2(a[0] as f32 + 100.0, (a[1] + a[3] / 2.0) as f32);
    let to = egui::pos2(b[0] as f32 + 100.0, b[1] as f32 + 2.0);
    d.press_drag(from, to, egui::Modifiers::NONE);
    assert_eq!(d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects, before);
    assert_eq!(d.harness.state().session.history.undo.len(), mark);
    d.shot("effect-order-insertion");
    d.release(to, egui::Modifiers::NONE);
    let after = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects.clone();
    assert_eq!(after[0], before[1]);
    assert_eq!(after[1], before[0]);
    assert_eq!(after[2..], before[2..]);
    assert_eq!(d.harness.state().session.history.undo.len(), mark + 1);
    assert!(d.harness.state().ui.keyframe_selection.is_empty(), "stale indices must not select the wrong effect's keys");
    d.shot("effect-order-after");
    d.exec("edit.undo", json!({}));
    assert_eq!(d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects, before);
    d.exec("edit.redo", json!({}));
    assert_eq!(d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects, after);
}

#[test]
fn cancelled_same_position_and_intrinsic_header_drags_leave_the_stack_alone() {
    let (mut d, clip) = reorder_demo();
    let before = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects.clone();
    let mark = d.harness.state().session.history.undo.len();
    let a = d.rect("effectControls.effect.rounded_crop");
    let b = d.rect("effectControls.effect.drop_shadow");
    let from = egui::pos2(a[0] as f32 + 100.0, (a[1] + a[3] / 2.0) as f32);
    let to = egui::pos2(b[0] as f32 + 100.0, (b[1] + b[3] - 2.0) as f32);
    d.press_drag(from, to, egui::Modifiers::NONE);
    d.harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(2);
    d.release(to, egui::Modifiers::NONE);
    d.harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: None,
        pressed: false,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(2);
    let a = d.rect("effectControls.effect.rounded_crop");
    let from = egui::pos2(a[0] as f32 + 100.0, (a[1] + a[3] / 2.0) as f32);
    d.gesture(from, from + egui::vec2(60.0, 0.0), egui::Modifiers::NONE, None);
    d.gesture(from, egui::pos2(-20.0, -20.0), egui::Modifiers::NONE, None);
    let a = d.rect("effectControls.effect.motion");
    let from = egui::pos2(a[0] as f32 + 100.0, (a[1] + a[3] / 2.0) as f32);
    d.gesture(from, to, egui::Modifiers::NONE, None);
    assert_eq!(d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects, before);
    assert_eq!(d.harness.state().session.history.undo.len(), mark);
}

#[test]
fn duplicate_effect_headers_have_distinct_drag_targets() {
    let (mut d, clip) = reorder_demo();
    d.exec("effects.apply", json!({"clips":[clip.id],"effect":"rounded_crop"}));
    d.exec("effects.setParam", json!({"clip":clip.id,"effect":2,"param":"left","value":17}));
    let before = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects.clone();
    for idx in 0..before.len() {
        if !d.harness.state().ui.collapsed_fx.contains(&format!("{}:{idx}", clip.id)) {
            d.click(&format!("effectControls.clip.{}.effect.{idx}.twirl", clip.id));
        }
    }
    let a = d.rect(&format!("effectControls.clip.{}.effect.2", clip.id));
    let b = d.rect(&format!("effectControls.clip.{}.effect.0", clip.id));
    d.gesture(egui::pos2(a[0] as f32 + 100.0, (a[1] + a[3] / 2.0) as f32), egui::pos2(b[0] as f32 + 100.0, b[1] as f32 + 2.0), egui::Modifiers::NONE, None);
    let after = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects.clone();
    assert_eq!(after[..3], [before[2].clone(), before[0].clone(), before[1].clone()]);
    assert_eq!(d.harness.state().session.state.selected_effect.as_ref().unwrap().index(&after), Some(0));
    d.exec("edit.undo", json!({}));
    assert_eq!(d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects, before);
    assert_eq!(d.harness.state().session.state.selected_effect.as_ref().unwrap().index(&before), Some(2));
}

#[test]
fn an_effect_drag_with_a_stale_snapshot_does_not_commit() {
    let (mut d, clip) = reorder_demo();
    let a = d.rect("effectControls.effect.drop_shadow");
    let b = d.rect("effectControls.effect.rounded_crop");
    let from = egui::pos2(a[0] as f32 + 100.0, (a[1] + a[3] / 2.0) as f32);
    let to = egui::pos2(b[0] as f32 + 100.0, b[1] as f32 + 2.0);
    d.press_drag(from, to, egui::Modifiers::NONE);
    d.exec("effects.setParam", json!({"clip":clip.id,"effect":0,"param":"left","value":17}));
    let expected = d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects.clone();
    let mark = d.harness.state().session.history.undo.len();
    d.release(to, egui::Modifiers::NONE);
    assert_eq!(d.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip.id)).unwrap().1.effects, expected);
    assert_eq!(d.harness.state().session.history.undo.len(), mark);
}

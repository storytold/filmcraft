//! Keyframes in the Properties and Effect Controls panels, driven headless through the control
//! channel: the Properties panel's keyframe navigator (◀ ◆ ▶) adds and removes the keyframe at the
//! playhead and steps between keyframes, as Premiere's does, and the Effect Controls time ruler
//! moves the playhead (click, or drag the playhead's handle).
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the tests also render the UI with wgpu and write PNGs there.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
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

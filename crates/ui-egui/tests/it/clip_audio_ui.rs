//! Audio of video clips (#223): a selected video clip shows the Volume, Channel Volume and Panner
//! of the audio linked to it as well, as in Premiere, and the Volume line on audio clips in the
//! timeline can be dragged and keyframed, like the line Show Clip Keyframes puts on any clip (#2213).
//!
//! With `FILMCRAFT_UI_SHOTS=<dir>` the tests also render the UI with wgpu and write PNGs there.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{ClipId, TrackItem};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    shots: Option<std::path::PathBuf>,
}

/// The demo's first clip: its video on V1 and the audio linked to it on A1.
struct Pair {
    video: u64,
    audio: u64,
}

impl Driver {
    fn demo(panels: &[&str]) -> (Self, Pair) {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let shots = std::env::var_os("FILMCRAFT_UI_SHOTS").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 1100.0)).with_max_steps(10_000);
        if shots.is_some() {
            b = b.wgpu().with_pixels_per_point(1.0);
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, shots };
        d.frames(4);
        let seq = d.exec("sequence.inspect", json!({}));
        let v = &seq["video"][0]["items"][0];
        let a = seq["audio"][0]["items"].as_array().unwrap().iter().find(|a| a["link"] == v["link"]).expect("linked audio").clone();
        let pair = Pair { video: v["clip"].as_u64().unwrap(), audio: a["clip"].as_u64().unwrap() };
        for panel in panels {
            d.ok("ui.panel.show", json!({"panel": panel}));
        }
        d.frames(3);
        (d, pair)
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
    fn item(&self, clip: u64) -> TrackItem {
        self.harness.state().session.active_sequence().unwrap().find_item(ClipId(clip)).unwrap().1.clone()
    }
    /// The clip's Volume level: its value and its keyframes (media time, dB).
    fn level(&self, clip: u64) -> (f64, Vec<(i64, f64)>) {
        let it = self.item(clip);
        let p = it.effect("volume").unwrap().param("level").unwrap().clone();
        (p.value.as_f64().unwrap(), p.keyframes.iter().map(|k| (k.time.0, k.value.as_f64().unwrap())).collect())
    }
    fn drag(&mut self, from: (f64, f64), by: (f64, f64)) {
        self.ok("ui.drag", json!({"from": {"x": from.0, "y": from.1}, "to": {"x": from.0 + by.0, "y": from.1 + by.1}, "steps": 6}));
        self.frames(3);
    }
    fn shot(&mut self, name: &str) {
        let Some(dir) = self.shots.clone() else { return };
        self.frames(10);
        let img = self.harness.render().expect("wgpu render");
        std::fs::create_dir_all(&dir).unwrap();
        img.save(dir.join(format!("{name}.png"))).unwrap();
    }
}

fn centre(r: [f64; 4]) -> (f64, f64) {
    (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
}

#[test]
fn effect_controls_show_the_audio_of_a_video_clip() {
    let (mut d, pair) = Driver::demo(&["EffectControls"]);
    d.exec("timeline.select", json!({"clips": [pair.video]}));
    let motion = d.rect("effectControls.effect.motion");
    let volume = d.rect("effectControls.effect.volume");
    assert!(volume[1] > motion[1], "Audio comes after Video: {volume:?} {motion:?}");
    d.rect("effectControls.effect.channel_volume");
    d.rect("effectControls.effect.panner");

    // the audio rows edit the audio clip
    for fx in ["motion", "opacity", "time_remap"] {
        d.click(&format!("effectControls.effect.{fx}.twirl"));
    }
    d.click("effectControls.volume.level.stopwatch");
    assert!(d.item(pair.audio).effect("volume").unwrap().param("level").unwrap().is_animated());
    assert!(d.item(pair.video).effect("volume").is_none());

    // only the video selected: no audio
    d.exec("sequence.linkedSelection", json!({"on": false}));
    d.exec("timeline.select", json!({"clips": [pair.video]}));
    assert!(d.find("effectControls.effect.volume").is_none());
    d.rect("effectControls.effect.motion");
}

/// #696: Parametric EQ on a linked A/V pair shows in Effect Controls (and Custom Setup) while the
/// video clip is selected — Effect Controls used to keep only the picture clip.
#[test]
fn effect_controls_show_audio_fx_on_a_linked_pair() {
    let (mut d, pair) = Driver::demo(&["EffectControls"]);
    d.exec("timeline.select", json!({"clips": [pair.video]}));
    d.exec("effects.apply", json!({"clips": [pair.video], "effect": "parametric_eq"}));
    assert_eq!(d.item(pair.audio).effects.last().map(|e| e.effect.as_str()), Some("parametric_eq"));
    for fx in ["motion", "opacity", "time_remap", "volume", "channel_volume", "panner"] {
        d.click(&format!("effectControls.effect.{fx}"));
    }
    d.rect("effectControls.effect.parametric_eq");
    d.rect("effectControls.effect.parametric_eq.edit");
}

#[test]
fn properties_show_the_audio_of_a_video_clip() {
    let (mut d, pair) = Driver::demo(&["Properties"]);
    d.exec("timeline.select", json!({"clips": [pair.video]}));
    let scale = d.rect("properties.motion.scale.addKeyframe");
    let level = d.rect("properties.volume.level.addKeyframe");
    assert!(level[1] > scale[1]);
    d.click("properties.volume.level.addKeyframe");
    assert_eq!(d.item(pair.audio).effect("volume").unwrap().param("level").unwrap().keyframes.len(), 1);

    d.exec("sequence.linkedSelection", json!({"on": false}));
    d.exec("timeline.select", json!({"clips": [pair.audio]}));
    d.rect("properties.volume.level.addKeyframe");
    assert!(d.find("properties.motion.scale.addKeyframe").is_none());
}

#[test]
fn dragging_the_volume_line_changes_the_level() {
    let (mut d, pair) = Driver::demo(&[]);
    let start = d.item(pair.audio).start;
    let (before, _) = d.level(pair.audio);
    d.shot("volume-line");
    let line = d.rect(&format!("timeline.clip.{}.volume", pair.audio));
    d.drag(centre(line), (0.0, -12.0));
    let (after, keys) = d.level(pair.audio);
    assert!(after > before + 1.0, "{before} dB → {after} dB");
    assert!(keys.is_empty());
    assert_eq!(d.item(pair.audio).start, start, "the clip did not move");
    let moved = d.rect(&format!("timeline.clip.{}.volume", pair.audio));
    assert!(moved[1] < line[1] - 8.0, "the line went up with the pointer: {line:?} → {moved:?}");
    d.shot("volume-line-dragged");
    d.exec("edit.undo", json!({}));
    assert_eq!(d.level(pair.audio).0, before, "the drag was one undo step");
}

/// The line belongs to the clip: a click on it selects the clip and a right-click opens the clip
/// menu, only a drag (or the Pen tool) edits the level.
#[test]
fn clicks_on_the_volume_line_still_reach_the_clip() {
    let (mut d, pair) = Driver::demo(&[]);
    let (x, y) = centre(d.rect(&format!("timeline.clip.{}.volume", pair.audio)));
    assert_eq!(d.ok("ui.timeline.hit", json!({"x": x, "y": y}))["kind"], "volume");
    d.ok("ui.click", json!({"x": x, "y": y}));
    d.frames(3);
    assert!(d.harness.state().session.state.selection.contains(&ClipId(pair.audio)));
    d.ok("ui.click", json!({"x": x, "y": y, "button": "right"}));
    d.frames(3);
    d.rect("timeline.clipMenu.clip.link");
    assert!(d.level(pair.audio).1.is_empty());
}

#[test]
fn volume_keyframes_on_the_timeline() {
    let (mut d, pair) = Driver::demo(&[]);
    let id = format!("timeline.clip.{}.volume", pair.audio);
    let clip = d.rect(&format!("timeline.clip.{}", pair.audio));
    let (_, y) = centre(d.rect(&id));
    // the Pen tool adds keyframes on the line
    d.ok("ui.set", json!({"tool": "Pen"}));
    for f in [0.3, 0.7] {
        d.ok("ui.click", json!({"x": clip[0] + clip[2] * f, "y": y}));
        d.frames(3);
    }
    let (_, keys) = d.level(pair.audio);
    assert_eq!(keys.len(), 2, "{keys:?}");
    d.ok("ui.set", json!({"tool": "Selection"}));

    // the line between them takes both down
    d.drag((clip[0] + clip[2] * 0.5, y), (0.0, 10.0));
    let (_, low) = d.level(pair.audio);
    assert_eq!(low.iter().map(|k| k.0).collect::<Vec<_>>(), keys.iter().map(|k| k.0).collect::<Vec<_>>());
    assert!(low[0].1 < keys[0].1 - 1.0 && (low[0].1 - low[1].1).abs() < 1e-9, "{keys:?} → {low:?}");
    d.exec("edit.undo", json!({}));
    assert_eq!(d.level(pair.audio).1, keys);

    // a keyframe moves in time and level
    let k = d.rect(&format!("{id}.kf.1"));
    d.drag(centre(k), (40.0, -10.0));
    let (_, moved) = d.level(pair.audio);
    assert_eq!(moved[0], keys[0]);
    assert!(moved[1].0 > keys[1].0 && moved[1].1 > keys[1].1, "{keys:?} → {moved:?}");
    d.shot("volume-keyframes");
    d.exec("edit.undo", json!({}));
    assert_eq!(d.level(pair.audio).1, keys, "the keyframe drag was one undo step");

    // right-click ▸ Delete
    d.ok("ui.click", json!({"id": format!("{id}.kf.0"), "button": "right"}));
    d.frames(3);
    d.click(&format!("{id}.kf.0.delete"));
    assert_eq!(d.level(pair.audio).1, keys[1..]);
}

/// The clip's parameter `effect`/`param`: its value and its keyframes (media time, value).
fn param_of(it: &TrackItem, effect: &str, param: &str) -> (f64, Vec<(i64, f64)>) {
    let p = it.effect(effect).unwrap().param(param).unwrap().clone();
    (p.value.as_f64().unwrap(), p.keyframes.iter().map(|k| (k.time.0, k.value.as_f64().unwrap())).collect())
}

/// #2213: right-click ▸ Show Clip Keyframes picks the parameter a clip's line shows, as in
/// Premiere: a video clip then gets a line for Opacity (or Motion, Time Remapping, an added
/// effect) that drags and keyframes like the Volume line, and an audio clip can show Panner.
#[test]
fn show_clip_keyframes_picks_the_line_of_a_clip() {
    let (mut d, pair) = Driver::demo(&[]);
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 90}}));
    d.frames(3);
    let line = format!("timeline.clip.{}.keyframes", pair.video);
    assert!(d.find(&line).is_none(), "video clips show no line until a parameter is picked");

    // right-click the video clip ▸ Show Clip Keyframes ▸ Opacity ▸ Opacity
    d.exec("sequence.linkedSelection", json!({"on": false}));
    let clip = d.rect(&format!("timeline.clip.{}", pair.video));
    d.ok("ui.click", json!({"x": clip[0] + clip[2] * 0.5, "y": clip[1] + 8.0, "button": "right"}));
    d.frames(3);
    d.click("timeline.clipMenu.clip.showKeyframes");
    d.click("timeline.clipMenu.showKeyframes.opacity#0");
    d.click("timeline.clipMenu.showKeyframes.opacity#0/opacity");
    let at = d.rect(&line);
    let (_, y) = centre(at);
    assert!(y < clip[1] + clip[3] * 0.5, "100 % sits at the top of the clip: {at:?} in {clip:?}");

    // dragging it down lowers the opacity, in one undo step
    d.drag(centre(at), (0.0, 25.0));
    let (op, keys) = param_of(&d.item(pair.video), "opacity", "opacity");
    assert!(op < 90.0 && keys.is_empty(), "{op}");
    let moved = d.rect(&line);
    assert!(moved[1] > at[1] + 10.0, "the line went down with the pointer: {at:?} → {moved:?}");
    d.shot("clip-keyframes-opacity");
    d.exec("edit.undo", json!({}));
    assert_eq!(param_of(&d.item(pair.video), "opacity", "opacity").0, 100.0);

    // the Pen tool keyframes it, a keyframe drags in time and value, right-click deletes it
    let (_, y) = centre(d.rect(&line));
    d.ok("ui.set", json!({"tool": "Pen"}));
    for f in [0.3, 0.7] {
        d.ok("ui.click", json!({"x": clip[0] + clip[2] * f, "y": y}));
        d.frames(3);
    }
    d.ok("ui.set", json!({"tool": "Selection"}));
    let (_, keys) = param_of(&d.item(pair.video), "opacity", "opacity");
    assert_eq!(keys.len(), 2, "{keys:?}");
    let k = d.rect(&format!("{line}.kf.1"));
    d.drag(centre(k), (40.0, 20.0));
    let (_, moved) = param_of(&d.item(pair.video), "opacity", "opacity");
    assert!(moved[1].0 > keys[1].0 && moved[1].1 < keys[1].1, "{keys:?} → {moved:?}");
    d.ok("ui.click", json!({"id": format!("{line}.kf.0"), "button": "right"}));
    d.frames(3);
    d.click(&format!("{line}.kf.0.delete"));
    assert_eq!(param_of(&d.item(pair.video), "opacity", "opacity").1.len(), 1);
    assert!(param_of(&d.item(pair.audio), "volume", "level").1.is_empty(), "the audio clip's Volume is untouched");

    // ▸ None hides it again
    d.ok("ui.click", json!({"x": clip[0] + clip[2] * 0.5, "y": clip[1] + 8.0, "button": "right"}));
    d.frames(3);
    d.click("timeline.clipMenu.clip.showKeyframes");
    d.click("timeline.clipMenu.showKeyframes.none");
    assert!(d.find(&line).is_none());

    // an audio clip shows Volume until Panner ▸ Balance is picked
    let vol = format!("timeline.clip.{}.volume", pair.audio);
    let level = param_of(&d.item(pair.audio), "volume", "level").0;
    let aclip = d.rect(&format!("timeline.clip.{}", pair.audio));
    d.rect(&vol);
    d.ok("ui.click", json!({"x": aclip[0] + aclip[2] * 0.5, "y": aclip[1] + 8.0, "button": "right"}));
    d.frames(3);
    d.click("timeline.clipMenu.clip.showKeyframes");
    assert!(d.find("timeline.clipMenu.showKeyframes.none").is_none(), "an audio clip always shows a line");
    d.click("timeline.clipMenu.showKeyframes.panner#0");
    d.click("timeline.clipMenu.showKeyframes.panner#0/balance");
    assert!(d.find(&vol).is_none());
    let pan = format!("timeline.clip.{}.keyframes", pair.audio);
    let p = d.rect(&pan);
    d.drag(centre(p), (0.0, -10.0));
    assert!(param_of(&d.item(pair.audio), "panner", "balance").0 > 1.0, "dragging up pans right");
    assert_eq!(param_of(&d.item(pair.audio), "volume", "level").0, level);
}

/// Show Clip Keyframes ▸ None is offered for video clips only, so with Linked Selection on — its
/// default — it must leave the linked audio clip's Volume line alone. That clip's own menu never
/// offers None, so a line taken away there is one the user has to know to pick back by name.
#[test]
fn none_on_a_linked_pair_leaves_the_audio_clips_volume_line() {
    let (mut d, pair) = Driver::demo(&[]);
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 90}}));
    d.frames(3);
    let vol = format!("timeline.clip.{}.volume", pair.audio);
    let line = format!("timeline.clip.{}.keyframes", pair.video);
    d.rect(&vol);

    // right-click the video clip, which selects its linked audio clip too, and give it a line
    let clip = d.rect(&format!("timeline.clip.{}", pair.video));
    let menu = |d: &mut Driver| {
        d.ok("ui.click", json!({"x": clip[0] + clip[2] * 0.5, "y": clip[1] + 8.0, "button": "right"}));
        d.frames(3);
        d.click("timeline.clipMenu.clip.showKeyframes");
    };
    menu(&mut d);
    d.click("timeline.clipMenu.showKeyframes.opacity#0");
    d.click("timeline.clipMenu.showKeyframes.opacity#0/opacity");
    d.rect(&line);

    // ▸ None takes the video clip's line away and nothing else
    menu(&mut d);
    d.click("timeline.clipMenu.showKeyframes.none");
    assert!(d.find(&line).is_none(), "None takes the video clip's line away");
    d.rect(&vol);
}

/// With an audio clip first in a mixed selection, Show Clip Keyframes still offers None, because
/// the selection holds a video clip, and None still takes only the video clip's line away.
#[test]
fn none_is_offered_with_an_audio_clip_first_in_the_selection() {
    let (mut d, pair) = Driver::demo(&[]);
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 90}}));
    d.frames(3);
    let vol = format!("timeline.clip.{}.volume", pair.audio);
    let line = format!("timeline.clip.{}.keyframes", pair.video);
    // give the video clip a line from its own menu
    let vclip = d.rect(&format!("timeline.clip.{}", pair.video));
    d.ok("ui.click", json!({"x": vclip[0] + vclip[2] * 0.5, "y": vclip[1] + 8.0, "button": "right"}));
    d.frames(3);
    d.click("timeline.clipMenu.clip.showKeyframes");
    d.click("timeline.clipMenu.showKeyframes.opacity#0");
    d.click("timeline.clipMenu.showKeyframes.opacity#0/opacity");
    d.rect(&line);
    // now with the audio clip first in the selection (as a Shift-click from the audio side
    // leaves it): None is there and hides the video clip's line only
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [pair.audio, pair.video]}}));
    d.frames(2);
    let order: Vec<u64> = d.harness.state().session.state.selection.iter().map(|c| c.0).collect();
    assert_eq!(order.first(), Some(&pair.audio), "{order:?}");
    let aclip = d.rect(&format!("timeline.clip.{}", pair.audio));
    d.ok("ui.click", json!({"x": aclip[0] + aclip[2] * 0.5, "y": aclip[1] + 8.0, "button": "right"}));
    d.frames(3);
    d.click("timeline.clipMenu.clip.showKeyframes");
    assert!(d.find("timeline.clipMenu.showKeyframes.none").is_some(), "None is offered for the selected video clip");
    d.click("timeline.clipMenu.showKeyframes.none");
    assert!(d.find(&line).is_none(), "None takes the video clip's line away");
    d.rect(&vol);
}

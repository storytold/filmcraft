//! Audio of video clips (#223): a selected video clip shows the Volume, Channel Volume and Panner
//! of the audio linked to it as well, as in Premiere, and the Volume line on audio clips in the
//! timeline can be dragged and keyframed.
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
    /// Press at `from`, move by `by` over `steps` frames (calling `between` before each move and
    /// once before the release, with the step number), then release: a drag a test can look into.
    fn drag_frames(&mut self, from: (f64, f64), by: (f64, f64), steps: usize, mut between: impl FnMut(&mut Self, usize)) {
        let mut p = egui::pos2(from.0 as f32, from.1 as f32);
        let delta = egui::vec2(by.0 as f32, by.1 as f32) / steps as f32;
        self.harness.input_mut().events.push(egui::Event::PointerMoved(p));
        self.frames(1);
        self.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        self.frames(1);
        for i in 0..steps {
            between(self, i);
            p += delta;
            self.harness.input_mut().events.push(egui::Event::PointerMoved(p));
            self.frames(1);
        }
        between(self, steps);
        self.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: p,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        self.frames(2);
    }
    /// The media times of the Timeline's selected keyframes on `clip`.
    fn selected_times(&self, clip: u64) -> Vec<i64> {
        self.harness.state().ui.keyframe_selection.iter().filter(|k| k.clip == clip).map(|k| k.time.0).collect()
    }
    /// Two Volume keyframes added with the Pen tool on the line of `clip`, the second at -12 dB:
    /// the line's automation id and the keys (media time, dB).
    fn two_volume_keys(&mut self, clip: u64) -> (String, Vec<(i64, f64)>) {
        let id = format!("timeline.clip.{clip}.volume");
        let r = self.rect(&format!("timeline.clip.{clip}"));
        let (_, y) = centre(self.rect(&id));
        self.ok("ui.set", json!({"tool": "Pen"}));
        for f in [0.3, 0.6] {
            self.ok("ui.click", json!({"x": r[0] + r[2] * f, "y": y}));
            self.frames(3);
        }
        self.ok("ui.set", json!({"tool": "Selection"}));
        let m = self.level(clip).1[1].0;
        self.exec("effects.setKeyframe", json!({"clip": clip, "effect": "volume", "param": "level", "mediaTime": m, "value": -12.0}));
        let (_, keys) = self.level(clip);
        assert_eq!(keys.len(), 2, "{keys:?}");
        (id, keys)
    }
    /// A2 (added when the demo has none) showing its Volume lane with one key in the middle of
    /// `clip`'s span: the key's sequence time.
    fn a2_lane_key(&mut self, clip: u64) -> i64 {
        if self.exec("sequence.inspect", json!({}))["audio"].as_array().map_or(0, Vec::len) < 2 {
            self.exec("sequence.addTracks", json!({"video": 0, "audio": 1}));
        }
        let it = self.item(clip);
        let t = it.start.0 + it.duration.0 / 2;
        self.exec("mixer.setKeyframe", json!({"strip": "A2", "lane": "volume", "time": t, "value": -6.0}));
        self.click("timeline.track.A2.keyframes");
        self.click("timeline.track.A2.keyframes.volume");
        self.rect("timeline.track.A2.lane.kf.0");
        t
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

/// The Pen tool adds Volume keyframes on a clip's line in the Timeline, and they can be deleted.
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

/// Copy and paste of Volume keyframes on the Timeline (#3562): a click selects a keyframe (Shift
/// adds one), Cmd+C copies them, and Cmd+V puts them at the playhead on the selected clip with
/// their levels and spacing.
#[test]
fn volume_keyframes_copy_and_paste_on_the_timeline() {
    let (mut d, pair) = Driver::demo(&[]);
    let (id, keys) = d.two_volume_keys(pair.audio);
    d.click(&format!("{id}.kf.0"));
    d.ok("ui.click", json!({"id": format!("{id}.kf.1"), "modifiers": {"shift": true}}));
    d.frames(3);
    d.ok("ui.key", json!({"key": "Cmd+C"}));

    // the next sound clip on A1 takes them, the first at the playhead
    let seq = d.exec("sequence.inspect", json!({}));
    let other = seq["audio"][0]["items"][1]["clip"].as_u64().expect("a second clip on A1");
    d.click(&format!("timeline.clip.{other}"));
    d.exec("playhead.set", json!({"time": d.item(other).start.0 + 1_000_000}));
    let at = d.harness.state().session.playhead().0;
    d.ok("ui.key", json!({"key": "Cmd+V"}));
    let (_, pasted) = d.level(other);
    assert_eq!(pasted.len(), 2, "pasted on the selected clip: {pasted:?}");
    assert_eq!(d.level(pair.audio).1, keys, "the copied keys stay where they were");
    let (src, dst) = (d.item(pair.audio), d.item(other));
    let on = |it: &TrackItem, k: &(i64, f64)| it.timeline_time(filmcraft_time::Tick(k.0)).0;
    assert!((on(&dst, &pasted[0]) - at).abs() <= 1, "the first at the playhead: {pasted:?}");
    let (gap, copied_gap) = (on(&dst, &pasted[1]) - on(&dst, &pasted[0]), on(&src, &keys[1]) - on(&src, &keys[0]));
    assert!((gap - copied_gap).abs() <= 2, "spacing kept: {gap} vs {copied_gap}");
    assert_eq!((pasted[0].1, pasted[1].1), (keys[0].1, keys[1].1), "levels kept");
}

/// Shift-click on a selected Volume keyframe takes it out of the selection and keeps the others.
#[test]
fn shift_click_takes_a_volume_keyframe_out_of_the_selection() {
    let (mut d, pair) = Driver::demo(&[]);
    let (id, keys) = d.two_volume_keys(pair.audio);
    d.click(&format!("{id}.kf.0"));
    assert_eq!(d.selected_times(pair.audio), vec![keys[0].0]);
    d.ok("ui.click", json!({"id": format!("{id}.kf.1"), "modifiers": {"shift": true}}));
    d.frames(3);
    assert_eq!(d.selected_times(pair.audio), vec![keys[0].0, keys[1].0], "Shift adds");
    d.ok("ui.click", json!({"id": format!("{id}.kf.0"), "modifiers": {"shift": true}}));
    d.frames(3);
    assert_eq!(d.selected_times(pair.audio), vec![keys[1].0], "Shift on a selected key takes it out");
    d.click(&format!("{id}.kf.0"));
    assert_eq!(d.selected_times(pair.audio), vec![keys[0].0], "a plain click selects it alone");
}

/// Dragging one of two selected Volume keyframes keeps both selected, and the selection follows
/// the dragged key on every frame of the drag, so Copy still finds it.
#[test]
fn dragging_a_selected_volume_keyframe_keeps_the_selection_on_it() {
    let (mut d, pair) = Driver::demo(&[]);
    let (id, keys) = d.two_volume_keys(pair.audio);
    d.click(&format!("{id}.kf.0"));
    d.ok("ui.click", json!({"id": format!("{id}.kf.1"), "modifiers": {"shift": true}}));
    d.frames(3);
    let (k0, k1) = (centre(d.rect(&format!("{id}.kf.0"))), centre(d.rect(&format!("{id}.kf.1"))));
    // a third of the way back towards the first key
    d.drag_frames(k1, ((k0.0 - k1.0) / 3.0, 0.0), 6, |d, i| {
        let now = d.level(pair.audio).1;
        assert_eq!(d.selected_times(pair.audio), vec![now[0].0, now[1].0], "step {i}: both selected, on the keys");
    });
    let (_, moved) = d.level(pair.audio);
    assert_eq!(moved[0], keys[0]);
    assert!(moved[1].0 < keys[1].0, "the key moved: {keys:?} → {moved:?}");
    assert_eq!(d.selected_times(pair.audio), vec![keys[0].0, moved[1].0]);
}

/// Escape during a Volume keyframe drag puts the key back, and the selection goes back with it.
#[test]
fn escape_puts_a_dragged_volume_keyframe_and_its_selection_back() {
    let (mut d, pair) = Driver::demo(&[]);
    let (id, keys) = d.two_volume_keys(pair.audio);
    d.click(&format!("{id}.kf.1"));
    let (k0, k1) = (centre(d.rect(&format!("{id}.kf.0"))), centre(d.rect(&format!("{id}.kf.1"))));
    d.drag_frames(k1, ((k0.0 - k1.0) / 3.0, 0.0), 6, |d, i| {
        if i == 4 {
            assert_ne!(d.level(pair.audio).1[1].0, keys[1].0, "moving");
            d.harness.input_mut().events.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: Default::default(),
            });
        }
    });
    assert_eq!(d.level(pair.audio).1, keys, "back where it was");
    assert_eq!(d.selected_times(pair.audio), vec![keys[1].0], "and still selected");
}

/// Shift-clicking a track lane keyframe keeps the Volume keyframes selected on clips (and the
/// other way round), and Cmd+C copies both kinds.
#[test]
fn shift_selects_clip_and_track_keyframes_together() {
    let (mut d, pair) = Driver::demo(&[]);
    let (id, keys) = d.two_volume_keys(pair.audio);
    let t = d.a2_lane_key(pair.audio);
    let track = |d: &Driver| d.harness.state().ui.track_keyframe_selection.iter().map(|k| (k.lane.clone(), k.time.0)).collect::<Vec<_>>();
    d.click(&format!("{id}.kf.0"));
    d.ok("ui.click", json!({"id": "timeline.track.A2.lane.kf.0", "modifiers": {"shift": true}}));
    d.frames(3);
    assert_eq!((d.selected_times(pair.audio), track(&d)), (vec![keys[0].0], vec![("volume".to_string(), t)]), "Shift on a track key keeps the clip key");
    d.ok("ui.click", json!({"id": format!("{id}.kf.1"), "modifiers": {"shift": true}}));
    d.frames(3);
    assert_eq!(
        (d.selected_times(pair.audio), track(&d)),
        (vec![keys[0].0, keys[1].0], vec![("volume".to_string(), t)]),
        "Shift on a clip key keeps the track key"
    );
    d.ok("ui.key", json!({"key": "Cmd+C"}));
    let copied: usize = d.harness.state().session.state.keyframe_clipboard.iter().map(|c| c.keys.len()).sum();
    assert_eq!(copied, 3, "Copy takes the clip and the track keys");
    // a plain click on the track key selects it alone
    d.click("timeline.track.A2.lane.kf.0");
    assert_eq!((d.selected_times(pair.audio), track(&d)), (vec![], vec![("volume".to_string(), t)]));
}

/// A selected track keyframe that an Undo removed is no longer selected: one added again at the
/// same time is not drawn selected.
#[test]
fn undo_drops_a_removed_track_keyframe_from_the_selection() {
    let (mut d, pair) = Driver::demo(&[]);
    let t = d.a2_lane_key(pair.audio);
    d.click("timeline.track.A2.lane.kf.0");
    assert_eq!(d.harness.state().ui.track_keyframe_selection.len(), 1);
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert!(d.harness.state().ui.track_keyframe_selection.is_empty(), "the removed key is not selected");
    d.exec("mixer.setKeyframe", json!({"strip": "A2", "lane": "volume", "time": t, "value": -6.0}));
    d.frames(2);
    assert!(d.harness.state().ui.track_keyframe_selection.is_empty(), "nor one added back at its time");
}

/// A selected Volume keyframe that an Undo removed is no longer selected.
#[test]
fn undo_drops_a_removed_volume_keyframe_from_the_selection() {
    let (mut d, pair) = Driver::demo(&[]);
    let (id, keys) = d.two_volume_keys(pair.audio);
    d.click(&format!("{id}.kf.1"));
    assert_eq!(d.selected_times(pair.audio), vec![keys[1].0]);
    // the -12 dB edit, then the Pen click that added the key
    d.exec("edit.undo", json!({}));
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(d.level(pair.audio).1.len(), 1);
    assert!(d.selected_times(pair.audio).is_empty(), "the removed key is not selected");
}

/// A click on a caption drops the keyframe selection, so Cmd+C copies the caption (#3562).
#[test]
fn a_caption_click_drops_the_keyframe_selection() {
    let (mut d, pair) = Driver::demo(&[]);
    let (id, _) = d.two_volume_keys(pair.audio);
    d.exec("captions.add", json!({"text": "hi", "durationSeconds": 1.0}));
    let caption = d.harness.state().session.active_sequence().unwrap().caption_tracks[0].captions[0].id.0;
    d.click(&format!("{id}.kf.0"));
    assert_eq!(d.harness.state().ui.keyframe_selection.len(), 1);
    d.click(&format!("timeline.caption.{caption}"));
    assert!(d.harness.state().ui.keyframe_selection.is_empty(), "the caption click drops the keyframe");
    d.ok("ui.key", json!({"key": "Cmd+C"}));
    let st = &d.harness.state().session.state;
    assert_eq!((st.caption_clipboard.len(), st.keyframe_clipboard.len()), (1, 0), "Cmd+C copied the caption");
}

/// Dragging a caption that was already selected drops the keyframe selection too, so Cmd+C
/// copies the caption (#3562 review).
#[test]
fn dragging_a_selected_caption_drops_the_keyframe_selection() {
    let (mut d, pair) = Driver::demo(&[]);
    let (id, _) = d.two_volume_keys(pair.audio);
    d.exec("captions.add", json!({"text": "hi", "durationSeconds": 1.0}));
    let caption = d.harness.state().session.active_sequence().unwrap().caption_tracks[0].captions[0].id.0;
    d.click(&format!("{id}.kf.0"));
    d.exec("captions.select", json!({"captions": [caption]}));
    d.frames(2);
    let st = d.harness.state();
    assert_eq!((st.ui.keyframe_selection.len(), st.session.state.caption_selection.len()), (1, 1), "a keyframe and the caption are selected");
    let r = d.rect(&format!("timeline.caption.{caption}"));
    d.drag_frames(centre(r), (r[2] / 4.0, 0.0), 6, |_, _| {});
    assert!(d.harness.state().ui.keyframe_selection.is_empty(), "the caption drag drops the keyframe");
    d.ok("ui.key", json!({"key": "Cmd+C"}));
    let st = &d.harness.state().session.state;
    assert_eq!((st.caption_clipboard.len(), st.keyframe_clipboard.len()), (1, 0), "Cmd+C copied the caption");
}

/// A click on a clip drops the keyframe selection too, so Cmd+C copies the clip.
#[test]
fn a_clip_click_drops_the_keyframe_selection() {
    let (mut d, pair) = Driver::demo(&[]);
    let (id, _) = d.two_volume_keys(pair.audio);
    d.click(&format!("{id}.kf.0"));
    assert_eq!(d.harness.state().ui.keyframe_selection.len(), 1);
    d.click(&format!("timeline.clip.{}", pair.video));
    assert!(d.harness.state().ui.keyframe_selection.is_empty());
    d.ok("ui.key", json!({"key": "Cmd+C"}));
    let st = &d.harness.state().session.state;
    assert!(st.keyframe_clipboard.is_empty() && st.clipboard.iter().any(|(_, _, it)| it.id.0 == pair.video), "Cmd+C copied the clip");
}

/// Drag a selected track keyframe, Undo the move, then Cmd+C: the selection no longer points at
/// a key, so Copy copies the selected clip instead of failing on the keyframe.
#[test]
fn copy_after_undoing_a_track_keyframe_drag_copies_the_clip() {
    let (mut d, pair) = Driver::demo(&[]);
    let t = d.a2_lane_key(pair.audio);
    d.click("timeline.track.A2.lane.kf.0");
    let clip = d.rect(&format!("timeline.clip.{}", pair.audio));
    let key = centre(d.rect("timeline.track.A2.lane.kf.0"));
    d.drag_frames(key, (clip[2] / 10.0, 0.0), 6, |_, _| {});
    let lane =
        |d: &Driver| d.harness.state().session.active_sequence().unwrap().audio_tracks[1].lane_keyframes("volume").iter().map(|k| k.time.0).collect::<Vec<_>>();
    let moved = lane(&d);
    assert!(moved.len() == 1 && moved[0] > t, "the key moved: {moved:?}");
    let sel = |d: &Driver| d.harness.state().ui.track_keyframe_selection.iter().map(|k| k.time.0).collect::<Vec<_>>();
    assert_eq!(sel(&d), moved, "the selection followed it");
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(lane(&d), vec![t]);
    assert!(sel(&d).is_empty(), "the moved-away ref is dropped");
    d.exec("timeline.select", json!({"clips": [pair.video]}));
    d.ok("ui.set", json!({"focused": "Timeline"}));
    d.ok("ui.key", json!({"key": "Cmd+C"}));
    let st = &d.harness.state().session.state;
    assert!(st.keyframe_clipboard.is_empty() && st.clipboard.iter().any(|(_, _, it)| it.id.0 == pair.video), "Cmd+C copied the clip");
}

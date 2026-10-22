//! Headless UI tests of Clip ▸ Remix: the menu entries, the Remix Properties dialog and the Remix
//! tool (drag a music clip's Out edge), driven by automation id through the control channel.

use std::sync::Arc;
use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

const SR: u32 = 48_000;

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new(session: Session) -> Self {
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let mut builder = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").is_some() {
            builder = builder.wgpu();
        }
        let harness = builder.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
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

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    /// Press at `from`, move to `to` over several frames (as a mouse does), release. Use steps of a
    /// few pixels: the drag starts where the pointer has moved past egui's drag threshold, and edge
    /// hits are 7 px wide.
    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2, steps: usize) {
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

    fn duration(&mut self, clip: u64) -> f64 {
        let s = self.harness.state();
        s.session.active_sequence().unwrap().find_item(filmcraft_project::ClipId(clip)).unwrap().1.duration.seconds()
    }
}

/// A session with 32 s of generated 120 BPM music on A1 (selected).
fn session() -> (Session, u64) {
    let (m, _) = filmcraft_audio_dsp::remix::test_music(SR, 120.0, 16, 5);
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "remix", "audio": 2, "video": 1, "fps":24})).unwrap();
    let inter: Vec<f32> = (0..m[0].len()).flat_map(|i| [m[0][i], m[1][i]]).collect();
    let bytes: Arc<[u8]> = filmcraft_engine::previews::write_wav_f32(&inter, SR).into();
    let item = filmcraft_engine::commands::import_bytes(&mut s, "/music.wav", bytes, None).unwrap();
    let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    let c = r["clips"][0].as_u64().unwrap();
    s.execute("timeline.select", json!({"clips": [c]})).unwrap();
    (s, c)
}

#[test]
fn remix_menu_and_properties_dialog() {
    let (s, clip) = session();
    let mut d = Driver::new(s);
    // the menu lists the Remix submenu
    let items = d.ok("ui.menu.list", json!({}));
    let ids: Vec<&str> = items.as_array().unwrap().iter().filter_map(|i| i["id"].as_str()).collect();
    for id in ["clip.remix.enable", "clip.remix.properties", "clip.remix.revert"] {
        assert!(ids.contains(&id), "{id} missing from the menus");
    }
    let original = d.duration(clip);
    // properties before enabling: refused with a reason
    let v = d.call("ui.menu.invoke", json!({"id": "clip.remix.properties"}));
    assert_eq!(v["ok"], json!(false));
    d.ok("ui.menu.invoke", json!({"id": "clip.remix.enable"}));
    let r = d.ok("ui.menu.invoke", json!({"id": "clip.remix.properties"}));
    assert_eq!(r["dialog"], "remixProperties");
    d.frames(3);
    let ids = d.ids("remixProperties.");
    for id in ["duration", "segments", "variations", "info", "ok", "cancel"] {
        assert!(ids.contains(&format!("remixProperties.{id}")), "remixProperties.{id} missing: {ids:?}");
    }
    // Cancel changes nothing
    let before = d.duration(clip);
    d.ok("ui.click", json!({"id": "remixProperties.cancel"}));
    d.frames(3);
    assert!(d.ids("remixProperties.").is_empty());
    assert_eq!(d.duration(clip), before);
    // type a target duration and press OK: one undo step, within a beat of the target
    d.ok("ui.menu.invoke", json!({"id": "clip.remix.properties"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "remixProperties.duration"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "20"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    let undo_before = d.harness.state().session.history.undo.len();
    d.ok("ui.click", json!({"id": "remixProperties.ok"}));
    d.frames(3);
    let after = d.duration(clip);
    assert!((after - 20.0).abs() <= 0.5, "remixed to {after} s");
    assert_eq!(d.harness.state().session.history.undo.len(), undo_before + 1);
    let p = d.exec("clip.remix.properties", json!({"clip": clip}));
    assert_eq!(p["targetSeconds"].as_f64().unwrap(), 20.0);
    // Revert Remix from the menu
    d.ok("ui.menu.invoke", json!({"id": "clip.remix.revert"}));
    assert_eq!(d.duration(clip), original);
}

#[test]
fn remix_tool_drag_sets_the_duration() {
    let (s, clip) = session();
    let mut d = Driver::new(s);
    d.ok("ui.menu.invoke", json!({"id": "tool.remix"}));
    let ui = d.ok("ui.inspect", json!({}));
    assert_eq!(ui["ui"]["tool"], "Remix");
    // the tool sits in the Rate Stretch group of the Tools panel
    d.frames(2);
    let edge = d.ok("ui.timeline.locate", json!({"clip": clip, "edge": "out"}));
    let (x, y) = (edge["x"].as_f64().unwrap(), edge["y"].as_f64().unwrap());
    let hit = d.ok("ui.timeline.hit", json!({"x": x - 1.0, "y": y}));
    assert!(hit.to_string().contains("out") || hit.to_string().contains("Out"), "{hit}");
    // drag the Out edge to the left by a third of the clip
    let start = d.ok("ui.timeline.locate", json!({"clip": clip, "edge": "in"}));
    let w = x - start["x"].as_f64().unwrap();
    let undo_before = d.harness.state().session.history.undo.len();
    d.drag(egui::pos2(x as f32 - 0.5, y as f32), egui::pos2((x - 0.5 - w / 3.0) as f32, y as f32), 300);
    d.frames(4);
    let dur = d.duration(clip);
    assert!((dur - 32.0 * 2.0 / 3.0).abs() < 1.5, "remixed to {dur} s");
    assert_eq!(d.harness.state().session.history.undo.len(), undo_before + 1, "one undo step");
    let p = d.exec("clip.remix.properties", json!({"clip": clip}));
    assert_eq!(p["remixed"], true);
    assert!((p["seconds"].as_f64().unwrap() - p["targetSeconds"].as_f64().unwrap()).abs() <= 0.5);
    assert!(!p["cuts"].as_array().unwrap().is_empty());
}

#[test]
fn essential_sound_duration_applies_and_reverts_music() {
    let (s, clip) = session();
    let mut d = Driver::new(s);
    d.ok("ui.set", json!({"workspace": "Audio"}));
    d.exec("essentialSound.setType", json!({"clips": [clip], "type": "music"}));
    d.frames(4);
    d.ok("ui.click", json!({"id": "essentialSound.section.Duration"}));
    d.frames(3);
    for suffix in ["seconds", "apply"] {
        assert!(d.ids("essentialSound.duration.").contains(&format!("essentialSound.duration.{suffix}")));
    }
    assert!(d.ids("essentialSound.duration.segments").is_empty());
    assert!(d.ids("essentialSound.duration.variations").is_empty());
    assert!(d.ids("essentialSound.duration.matchMelody").is_empty());
    let original = d.duration(clip);
    let undo = d.harness.state().session.history.undo.len();
    d.ok("ui.click", json!({"id": "essentialSound.duration.seconds", "count": 2}));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "00:00:20:00"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);

    assert!((d.duration(clip) - 20.0).abs() <= 0.5);
    assert_eq!(d.harness.state().session.history.undo.len(), undo + 1);
    let state = d.exec("clip.remix.properties", json!({"clip": clip}));
    assert!(!state["cuts"].as_array().unwrap().is_empty());
    d.ok("ui.click", json!({"id": "essentialSound.duration.revert"}));
    d.frames(3);
    assert_eq!(d.duration(clip), original);
    // The button also commits the currently edited value without Enter.
    d.ok("ui.click", json!({"id":"essentialSound.duration.seconds","count":2}));
    d.ok("ui.key", json!({"key":"Cmd+A"}));
    d.ok("ui.type", json!({"text":"00:00:24:00"}));
    let undo = d.harness.state().session.history.undo.len();
    d.ok("ui.click", json!({"id":"essentialSound.duration.apply"}));
    d.frames(3);
    assert!((d.duration(clip) - 24.0).abs() <= 0.5);
    assert_eq!(d.harness.state().session.history.undo.len(), undo + 1);
    if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR") {
        std::fs::create_dir_all(&dir).unwrap();
        d.harness.render().unwrap().save(std::path::PathBuf::from(dir).join("duration-enter-apply.png")).unwrap();
    }
}

#[test]
fn essential_sound_revert_then_shorter_target_uses_original_music() {
    let (s, clip) = session();
    let mut d = Driver::new(s);
    d.ok("ui.set", json!({"workspace":"Audio"}));
    d.exec("essentialSound.setType", json!({"clips":[clip],"type":"music"}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"essentialSound.section.Duration"}));
    d.frames(3);
    for seconds in [24, 20] {
        d.ok("ui.click", json!({"id":"essentialSound.duration.seconds","count":2}));
        d.ok("ui.key", json!({"key":"Cmd+A"}));
        d.ok("ui.type", json!({"text":format!("00:00:{seconds:02}:00")}));
        d.ok("ui.key", json!({"key":"Enter"}));
        d.frames(3);

        let state = d.exec("clip.remix.properties", json!({"clip":clip}));
        assert_eq!(state["targetSeconds"].as_f64().unwrap(), f64::from(seconds));
        assert!((state["originalSeconds"].as_f64().unwrap() - 32.0).abs() < 0.01);
        assert!((d.duration(clip) - f64::from(seconds)).abs() <= 0.5);
        if seconds == 24 {
            d.ok("ui.click", json!({"id":"essentialSound.duration.revert"}));
            d.frames(3);
            assert!((d.duration(clip) - 32.0).abs() < 0.01);
        }
    }
}

#[test]
fn duration_timecode_uses_sequence_frames_without_moving_the_playhead() {
    for (fps, drop_frame) in [(23.976_f64, false), (24., false), (25., false), (29.97, false), (30., false), (29.97, true)] {
        let (mut s, clip) = session();
        s.execute("sequence.settings", json!({"fps":fps,"dropFrame":drop_frame})).unwrap();
        let rate = s.sequence_rate();
        let (typed, expected_frames) = if drop_frame { ("00:01:00;02", 1800) } else { ("00:00:14:13", 14 * fps.round() as i64 + 13) };
        let playhead = s.playhead();
        let mut d = Driver::new(s);
        d.ok("ui.set", json!({"workspace":"Audio"}));
        d.exec("essentialSound.setType", json!({"clips":[clip],"type":"music"}));
        d.frames(3);
        d.ok("ui.click", json!({"id":"essentialSound.section.Duration"}));
        d.frames(3);
        let n = d.harness.state().session.history.undo.len();
        d.ok("ui.click", json!({"id":"essentialSound.duration.timecode"}));
        d.frames(2);
        d.ok("ui.key", json!({"key":"Cmd+A"}));
        d.ok("ui.type", json!({"text":typed}));
        d.ok("ui.key", json!({"key":"Enter"}));
        d.frames(3);
        let r = d.exec("clip.remix.properties", json!({"clip":clip}));
        assert_eq!(r["targetSeconds"].as_f64().unwrap(), rate.tick_of(expected_frames).seconds());
        assert_eq!(d.harness.state().session.playhead(), playhead);
        assert_eq!(d.harness.state().session.history.undo.len(), n + 1);
    }
}

#[test]
fn invalid_duration_does_not_apply_and_advanced_controls_remain_available() {
    let (s, clip) = session();
    let mut d = Driver::new(s);
    d.ok("ui.set", json!({"workspace":"Audio"}));
    d.exec("essentialSound.setType", json!({"clips":[clip],"type":"music"}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"essentialSound.section.Duration"}));
    d.frames(3);
    let before = d.harness.state().session.project.to_json();
    for text in ["not a time", "00:00:00:00", "02:00:00:00"] {
        d.ok("ui.click", json!({"id":"essentialSound.duration.timecode"}));
        d.frames(2);
        d.ok("ui.key", json!({"key":"Cmd+A"}));
        d.ok("ui.type", json!({"text":text}));
        d.ok("ui.key", json!({"key":"Enter"}));
        d.frames(3);
        assert_eq!(d.harness.state().session.project.to_json(), before);
    }
    d.ok("ui.click", json!({"id":"essentialSound.duration.advanced"}));
    d.frames(3);
    assert!(d.ids("essentialSound.duration.matchMelody").is_empty());
    assert!(d.ids("essentialSound.duration.alternative.").is_empty());
    for id in ["segments", "variations"] {
        assert!(d.ids("essentialSound.duration.").contains(&format!("essentialSound.duration.{id}")));
    }
}

#[test]
fn duration_advanced_parameters_are_sent_to_the_remix_command() {
    let (s, clip) = session();
    let mut d = Driver::new(s);
    d.ok("ui.set", json!({"workspace":"Audio"}));
    d.exec("essentialSound.setType", json!({"clips":[clip],"type":"music"}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"essentialSound.section.Duration"}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"essentialSound.duration.advanced"}));
    d.frames(2);
    for (key, value) in [("segments", "20"), ("variations", "80")] {
        d.ok("ui.click", json!({"id":format!("essentialSound.duration.{key}"),"count":2}));
        d.ok("ui.key", json!({"key":"Cmd+A"}));
        d.ok("ui.type", json!({"text":value}));
        d.ok("ui.key", json!({"key":"Enter"}));
        d.frames(2);
    }
    d.ok("ui.click", json!({"id":"essentialSound.duration.timecode"}));
    d.frames(2);
    d.ok("ui.key", json!({"key":"Cmd+A"}));
    d.ok("ui.type", json!({"text":"00:00:20:00"}));
    d.ok("ui.key", json!({"key":"Enter"}));
    d.frames(3);
    let r = d.exec("clip.remix.properties", json!({"clip":clip}));
    assert_eq!(r["segments"], 20.0);
    assert_eq!(r["variations"], 80.0);
}

#[test]
fn duration_draft_tracks_remix_tool_and_undo_redo() {
    use filmcraft_time::Tick;
    let (s, clip) = session();
    let mut d = Driver::new(s);
    d.ok("ui.set", json!({"workspace":"Audio"}));
    d.exec("essentialSound.setType", json!({"clips":[clip],"type":"music"}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"essentialSound.section.Duration"}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"essentialSound.duration.timecode"}));
    d.frames(2);
    d.ok("ui.key", json!({"key":"Cmd+A"}));
    d.ok("ui.type", json!({"text":"00:00:20:00"}));
    d.ok("ui.key", json!({"key":"Enter"}));
    d.frames(3);
    let id = egui::Id::new(("es-duration-draft", clip));
    assert_eq!(d.harness.ctx.data(|data| data.get_temp::<Tick>(id)), Some(Tick::from_seconds_f64(20.)));
    d.ok("ui.menu.invoke", json!({"id":"tool.remix"}));
    d.frames(2);
    let edge = d.ok("ui.timeline.locate", json!({"clip":clip,"edge":"out"}));
    let start = d.ok("ui.timeline.locate", json!({"clip":clip,"edge":"in"}));
    let x = edge["x"].as_f64().unwrap();
    let y = edge["y"].as_f64().unwrap();
    let width = x - start["x"].as_f64().unwrap();
    d.drag(egui::pos2(x as f32 - 0.5, y as f32), egui::pos2((x - 0.5 - width / 4.) as f32, y as f32), 100);
    d.frames(4);
    let state = d.exec("clip.remix.properties", json!({"clip":clip}));
    let target = Tick(state["target"].as_i64().unwrap());
    assert!((target.seconds() - 15.).abs() < 1.0, "actual drag target: {state}");
    assert_eq!(d.harness.ctx.data(|data| data.get_temp::<Tick>(id)), Some(target));
    d.exec("edit.undo", json!({}));
    d.frames(3);
    assert_eq!(d.harness.ctx.data(|data| data.get_temp::<Tick>(id)), Some(Tick::from_seconds_f64(20.)));
    d.exec("edit.redo", json!({}));
    d.frames(3);
    assert_eq!(d.harness.ctx.data(|data| data.get_temp::<Tick>(id)), Some(target));
    d.ok("ui.click", json!({"id":"essentialSound.duration.apply"}));
    d.frames(3);
    let applied = d.exec("clip.remix.properties", json!({"clip":clip}));
    assert_eq!(applied["target"], state["target"], "Apply must not restore the stale 20-second draft");
}

#[test]
fn duration_keeps_uncommitted_input_until_the_clip_changes() {
    use filmcraft_time::Tick;
    let (s, clip) = session();
    let mut d = Driver::new(s);
    d.ok("ui.set", json!({"workspace":"Audio"}));
    d.exec("essentialSound.setType", json!({"clips":[clip],"type":"music"}));
    d.exec("clip.remix", json!({"clip":clip,"seconds":20.}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"essentialSound.section.Duration"}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"essentialSound.duration.timecode"}));
    d.frames(2);
    d.ok("ui.key", json!({"key":"Cmd+A"}));
    d.ok("ui.type", json!({"text":"00:00:22:00"}));
    d.ok("ui.click", json!({"id":"essentialSound.duration.advanced"}));
    d.frames(4);
    let id = egui::Id::new(("es-duration-draft", clip));
    assert_eq!(d.harness.ctx.data(|data| data.get_temp::<Tick>(id)), Some(Tick::from_seconds_f64(22.)));
    assert!((d.duration(clip) - 20.).abs() <= 0.5, "draft is not applied on blur");
    // A manual trim changes the duration without changing the stored remix target.
    d.exec("timeline.trim", json!({"clip":clip,"edge":"out","delta":-Tick::from_seconds_f64(1.).0}));
    d.frames(3);
    assert_eq!(d.harness.ctx.data(|data| data.get_temp::<Tick>(id)), Some(Tick::from_seconds_f64(20.)));
    // An external target change must also discard an active invalid text buffer.
    d.ok("ui.click", json!({"id":"essentialSound.duration.timecode"}));
    d.frames(2);
    d.ok("ui.key", json!({"key":"Cmd+A"}));
    d.ok("ui.type", json!({"text":"not a time"}));
    d.exec("clip.remix", json!({"clip":clip,"seconds":25.}));
    d.frames(3);
    assert_eq!(d.harness.ctx.data(|data| data.get_temp::<Tick>(id)), Some(Tick::from_seconds_f64(25.)));
    assert!(d.harness.ctx.data(|data| data.get_temp::<String>(id.with("timecode").with("editing"))).is_none());
    d.ok("ui.click", json!({"id":"essentialSound.duration.apply"}));
    d.frames(3);
    assert_eq!(d.exec("clip.remix.properties", json!({"clip":clip}))["targetSeconds"], 25.);
}

#[test]
fn duration_controls_refresh_when_only_remix_parameters_change() {
    let (s, clip) = session();
    let mut d = Driver::new(s);
    d.ok("ui.set", json!({"workspace":"Audio"}));
    d.exec("essentialSound.setType", json!({"clips":[clip],"type":"music"}));
    d.exec("clip.remix", json!({"clip":clip,"seconds":32.,"segments":50.,"variations":50.}));
    d.frames(3);
    d.ok("ui.click", json!({"id":"essentialSound.section.Duration"}));
    d.frames(3);
    let id = egui::Id::new(("es-duration-draft", clip)).with("controls");
    for (segments, variations) in [(20., 50.), (20., 80.)] {
        let duration = d.duration(clip);
        d.exec("clip.remix", json!({"clip":clip,"seconds":32.,"segments":segments,"variations":variations}));
        d.frames(3);
        assert_eq!(d.duration(clip), duration, "exercise unchanged target and clip duration");
        assert_eq!(d.harness.ctx.data(|data| data.get_temp::<(f64, f64)>(id)), Some((segments, variations)));
        d.ok("ui.click", json!({"id":"essentialSound.duration.apply"}));
        d.frames(3);
        let applied = d.exec("clip.remix.properties", json!({"clip":clip}));
        assert_eq!(applied["segments"], segments);
        assert_eq!(applied["variations"], variations);
    }
}

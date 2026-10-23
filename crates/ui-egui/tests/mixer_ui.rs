//! Headless UI tests of the Audio Track Mixer, Audio Clip Mixer and timeline track keyframes: the
//! real `FilmcraftApp` under `egui_kittest`, driven by automation id and by frame-by-frame pointer
//! input (a fader drag spans several frames, as with a mouse).
//!
//! Set `FILMCRAFT_UI_SNAPSHOT_DIR=<dir>` to also render the window offscreen with wgpu and write PNGs
//! there (`mixer-*.png`); without it no GPU is needed.

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

impl Driver {
    fn demo() -> Self {
        Self::demo_with_step_dt(None)
    }

    fn demo_with_step_dt(step_dt: Option<f32>) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let snapshots = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut b = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000);
        if let Some(dt) = step_dt {
            b = b.with_step_dt(dt);
        }
        if snapshots.is_some() {
            b = b.wgpu();
        }
        let harness = b.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, snapshots };
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

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
    }

    fn element(&mut self, id: &str) -> Option<[f32; 4]> {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array()?.iter().find(|e| e["id"] == id).map(|e| {
            let r = &e["rect"];
            [r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32]
        })
    }

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    /// Press on `id`, move `dy` pixels over several frames (calling `between` before each move),
    /// release.
    fn drag_frames(&mut self, id: &str, dy: f32, steps: usize, mut between: impl FnMut(&mut Self, usize)) {
        let r = self.element(id).unwrap_or_else(|| panic!("no element {id}"));
        let mut p = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(p));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.frames(1);
        for i in 0..steps {
            between(self, i);
            p.y += dy / steps as f32;
            push(self, egui::Event::PointerMoved(p));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(2);
    }

    fn strip(&mut self, r: &str) -> Value {
        let m = self.exec("mixer.inspect", json!({}));
        m["strips"].as_array().unwrap().iter().find(|s| s["ref"] == r).cloned().unwrap_or(Value::Null)
    }

    /// Offscreen render of the window (or the bounding box of the elements under `prefix`).
    fn snapshot(&mut self, name: &str, prefix: Option<&str>) {
        let Some(dir) = self.snapshots.clone() else { return };
        let crop = prefix.map(|p| {
            let v = self.ok("ui.elements", json!({"prefix": p}));
            let mut bb = [f32::MAX, f32::MAX, f32::MIN, f32::MIN];
            for e in v.as_array().unwrap() {
                let r = &e["rect"];
                let (x, y, w, h) = (r[0].as_f64().unwrap() as f32, r[1].as_f64().unwrap() as f32, r[2].as_f64().unwrap() as f32, r[3].as_f64().unwrap() as f32);
                bb = [bb[0].min(x), bb[1].min(y), bb[2].max(x + w), bb[3].max(y + h)];
            }
            bb
        });
        self.frames(2);
        let img = match self.harness.render() {
            Ok(i) => i,
            Err(e) => {
                eprintln!("snapshot {name} skipped: {e}");
                return;
            }
        };
        let ppp = img.width() as f32 / 1600.0;
        let img = match crop {
            Some(bb) => {
                let x0 = ((bb[0] - 24.0) * ppp).max(0.0) as u32;
                let y0 = ((bb[1] - 30.0) * ppp).max(0.0) as u32;
                let x1 = (((bb[2] + 16.0) * ppp) as u32).min(img.width());
                let y1 = (((bb[3] + 46.0) * ppp) as u32).min(img.height());
                image::imageops::crop_imm(&img, x0, y0, x1 - x0, y1 - y0).to_image()
            }
            None => img,
        };
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        img.save(&path).unwrap();
        eprintln!("snapshot: {}", path.display());
    }
}

#[test]
fn audio_chrome_switches_between_themes() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Audio"}));
    for theme in ["light", "dark", "medium"] {
        d.ok("ui.set", json!({"theme": theme}));
        d.frames(3);
        assert!(d.element("mixer.A1.fader").is_some());
        assert!(d.element("mixer.A1.pan").is_some());
        d.snapshot(&format!("mixer-theme-{theme}"), None);
    }
}

#[test]
fn track_mixer_controls_by_id_and_by_drag() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Audio"}));
    d.frames(3);
    let ids = d.ids("mixer.");
    for id in [
        "mixer.A1.fader",
        "mixer.A1.pan",
        "mixer.A1.mode",
        "mixer.A1.mute",
        "mixer.A1.solo",
        "mixer.A1.record",
        "mixer.A1.output",
        "mixer.Mix.fader",
        "mixer.showEffects",
    ] {
        assert!(ids.iter().any(|i| i == id), "{id} missing from {ids:?}");
    }
    // automation mode from the dropdown
    d.click("mixer.A1.mode");
    d.click("mixer.A1.mode.Touch");
    assert_eq!(d.strip("A1")["mode"], "Touch");
    // mute / solo buttons
    d.click("mixer.A2.mute");
    assert_eq!(d.strip("A2")["muted"], true);
    d.click("mixer.A2.mute");
    // a fader drag (stopped) is heard live while held and committed once on release
    let undo0 = d.exec("history.list", json!({}))["undo"].as_array().unwrap().len();
    d.drag_frames("mixer.A1.fader", 60.0, 6, |_, _| {});
    let a1 = d.strip("A1");
    assert!(a1["volumeDb"].as_f64().unwrap() < -3.0, "fader moved down: {a1}");
    let undo = d.exec("history.list", json!({}))["undo"].as_array().unwrap().clone();
    assert_eq!(undo.len(), undo0 + 1, "one undo step: {undo:?}");
    // effects and sends
    d.click("mixer.showEffects");
    d.click("mixer.A1.fx.0");
    d.click("mixer.A1.fx.0.folder.Reverb");
    d.click("mixer.A1.fx.0.studio_reverb");
    assert_eq!(d.strip("A1")["inserts"][0]["effect"], "studio_reverb");
    d.exec("mixer.addSubmix", json!({"name": "Reverb Bus"}));
    d.frames(2);
    d.click("mixer.A2.send.0");
    d.click("mixer.A2.send.0.S1");
    assert_eq!(d.strip("A2")["sends"][0]["target"], "S1");
    let s1 = d.ids("mixer.S1.");
    assert!(s1.iter().any(|i| i == "mixer.S1.fader"), "submix strip shown: {s1:?}");
    d.snapshot("mixer-track-mixer-effects", Some("mixer."));
}

#[test]
fn recording_a_touch_pass_from_the_fader() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Audio"}));
    d.frames(3);
    d.exec("mixer.setStrip", json!({"strip": "A1", "mode": "Touch"}));
    d.exec("playhead.set", json!({"seconds": 1.0}));
    d.exec("mixer.recordStart", json!({}));
    // the playhead advances while the fader is held (as during playback)
    d.drag_frames("mixer.A1.fader", 80.0, 8, |d, i| {
        d.exec("playhead.set", json!({"seconds": 1.0 + 0.1 * i as f64}));
    });
    let held_lanes = d.strip("A1")["lanes"].clone();
    assert!(held_lanes.as_object().is_none_or(|m| m.is_empty()), "nothing written until the pass ends");
    d.exec("playhead.set", json!({"seconds": 3.0}));
    let r = d.exec("mixer.recordStop", json!({}));
    assert_eq!(r["lanes"], 1, "{r}");
    let a1 = d.strip("A1");
    let kfs = a1["lanes"]["volume"].as_array().unwrap().clone();
    assert!(kfs.len() >= 3, "{kfs:?}");
    // the lane in the timeline: header button → Volume
    d.click("timeline.track.A1.keyframes");
    d.click("timeline.track.A1.keyframes.volume");
    let ids = d.ids("timeline.track.A1.lane");
    assert!(ids.iter().any(|i| i == "timeline.track.A1.lane"), "{ids:?}");
    assert!(ids.iter().filter(|i| i.contains(".kf.")).count() >= 1, "diamonds: {ids:?}");
    // the pen tool adds a keyframe on the lane
    d.ok("ui.set", json!({"tool": "Pen"}));
    d.frames(2);
    let lane = d.element("timeline.track.A1.lane").unwrap();
    d.ok("ui.click", json!({"x": lane[0] + lane[2] * 0.9, "y": lane[1] + lane[3] * 0.5}));
    d.frames(3);
    let n = d.strip("A1")["lanes"]["volume"].as_array().unwrap().len();
    assert_eq!(n, kfs.len() + 1, "pen click adds a track keyframe");
    d.ok("ui.set", json!({"tool": "Selection"}));
    d.snapshot("mixer-timeline-track-keyframes", Some("timeline.track.A"));
    d.snapshot("mixer-audio-workspace", None);
}

#[test]
fn clip_mixer_strips_follow_the_playhead() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "AudioClipMixer"}));
    d.frames(3);
    let ids = d.ids("clipMixer.");
    assert!(ids.iter().any(|i| i == "clipMixer.A1.fader"), "{ids:?}");
    d.exec("playhead.set", json!({"seconds": 1.0}));
    d.drag_frames("clipMixer.A1.fader", 40.0, 5, |_, _| {});
    let seq = d.exec("sequence.inspect", json!({}));
    let fx = &seq["audio"][0]["items"][0]["effects"];
    let vol = fx.as_array().unwrap().iter().find(|e| e["effect"] == "volume").unwrap();
    let level = vol["params"]["level"]["value"]["Float"].as_f64().unwrap();
    assert!(level < -8.0, "clip volume lowered from the demo's -8 dB: {level}");
    d.snapshot("mixer-clip-mixer", Some("clipMixer."));
}

/// Horizontal extent `[left, right]` of each Clip Mixer strip `A1 … An` (its controls), `None` for
/// a strip that is not drawn.
fn clip_strip_spans(d: &mut Driver, n: usize) -> Vec<Option<[f32; 2]>> {
    let v = d.ok("ui.elements", json!({"prefix": "clipMixer.A"}));
    let elements = v.as_array().unwrap();
    (1..=n)
        .map(|i| {
            let prefix = format!("clipMixer.A{i}.");
            elements
                .iter()
                .filter(|e| e["id"].as_str().is_some_and(|id| id.starts_with(&prefix)))
                .map(|e| {
                    let x = e["rect"][0].as_f64().unwrap() as f32;
                    [x, x + e["rect"][2].as_f64().unwrap() as f32]
                })
                .reduce(|a, b| [a[0].min(b[0]), a[1].max(b[1])])
        })
        .collect()
}

/// #1922: like Premiere, the Audio Clip Mixer narrows its strips so that every track fits the
/// panel, and below the narrowest usable strip it scrolls: no track is ever out of reach.
#[test]
fn clip_mixer_strips_narrow_to_fit_the_panel() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "AudioClipMixer"}));
    d.frames(3);
    let panel = d.element("panel.AudioClipMixer").expect("clip mixer panel");
    let (left, right) = (panel[0], panel[0] + panel[2]);
    let have = d.exec("sequence.inspect", json!({}))["audio"].as_array().unwrap().len();
    let n = 8;
    d.exec("sequence.addTracks", json!({"video": 0, "audio": n - have}));
    d.frames(3);
    assert!(n as f32 * 100.0 > panel[2], "{n} full-width strips would fit the {} px panel", panel[2]);
    let spans = clip_strip_spans(&mut d, n);
    for (i, s) in spans.iter().enumerate() {
        let s = s.unwrap_or_else(|| panic!("A{} is not shown in the {} px panel: {spans:?}", i + 1, panel[2]));
        assert!(s[0] >= left && s[1] <= right, "A{} {s:?} outside the panel {left}…{right}", i + 1);
    }
    for w in spans.windows(2) {
        let (a, b) = (w[0].unwrap(), w[1].unwrap());
        assert!(a[1] < b[0], "strips overlap: {a:?} {b:?}");
    }
    // the narrowed strip still lays its buttons out side by side
    for i in 1..=n {
        let [m, s, k] = ["mute", "solo", "keyframe"].map(|c| d.element(&format!("clipMixer.A{i}.{c}")).unwrap());
        assert!(m[0] + m[2] <= s[0] && s[0] + s[2] <= k[0], "A{i} buttons overlap: {m:?} {s:?} {k:?}");
    }
    assert!(d.element("clipMixer.scroll").is_none(), "no scrollbar while every strip fits");
    d.snapshot("mixer-clip-mixer-narrow", Some("clipMixer."));

    // too many tracks even for the narrowest strips: the row scrolls to the last one
    let n = 30;
    d.exec("sequence.addTracks", json!({"video": 0, "audio": 22}));
    d.frames(3);
    let spans = clip_strip_spans(&mut d, n);
    assert!(spans[n - 1].is_none(), "A{n} cannot fit before scrolling: {spans:?}");
    let shown: Vec<[f32; 2]> = spans.iter().flatten().copied().collect();
    for w in shown.windows(2) {
        assert!(w[0][1] < w[1][0], "strips overlap: {:?} {:?}", w[0], w[1]);
    }
    let knob = d.element("clipMixer.scroll").expect("a scrollbar when the strips cannot all fit");
    let mut p = egui::pos2(knob[0] + knob[2] / 2.0, knob[1] + knob[3] / 2.0);
    let push = |d: &mut Driver, e: egui::Event| d.harness.input_mut().events.push(e);
    push(&mut d, egui::Event::PointerMoved(p));
    d.frames(1);
    push(&mut d, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    d.frames(1);
    for _ in 0..6 {
        p.x += panel[2] / 4.0;
        push(&mut d, egui::Event::PointerMoved(p));
        d.frames(1);
    }
    push(&mut d, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(2);
    let last = clip_strip_spans(&mut d, n)[n - 1].unwrap_or_else(|| panic!("A{n} is still out of reach after scrolling"));
    assert!(last[0] >= left && last[1] <= right, "A{n} {last:?} outside the panel {left}…{right}");
    d.snapshot("mixer-clip-mixer-scrolled", Some("clipMixer."));
}

#[test]
fn clip_mixer_db_value_can_be_typed() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "AudioClipMixer"}));
    d.frames(3);
    d.exec("playhead.set", json!({"seconds": 1.0}));
    d.frames(2);
    d.click("clipMixer.A1.value");
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.frames(1);
    d.ok("ui.type", json!({"text": "-20"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    let seq = d.exec("sequence.inspect", json!({}));
    let fx = &seq["audio"][0]["items"][0]["effects"];
    let vol = fx.as_array().unwrap().iter().find(|e| e["effect"] == "volume").unwrap();
    let level = vol["params"]["level"]["value"]["Float"].as_f64().unwrap();
    assert!((level + 20.0).abs() < 1e-6, "typed -20 dB into the clip mixer readout: {level}");
}

/// #817: a typed calculation, also one that starts with `*` or `/` and applies to the value.
#[test]
fn clip_mixer_db_value_takes_a_calculation() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "AudioClipMixer"}));
    d.frames(3);
    d.exec("playhead.set", json!({"seconds": 1.0}));
    d.frames(2);
    let level = |d: &mut Driver| -> f64 {
        let seq = d.exec("sequence.inspect", json!({}));
        let fx = &seq["audio"][0]["items"][0]["effects"];
        let vol = fx.as_array().unwrap().iter().find(|e| e["effect"] == "volume").unwrap();
        vol["params"]["level"]["value"]["Float"].as_f64().unwrap()
    };
    let enter = |d: &mut Driver, keys: &[&str], text: &str| {
        d.click("clipMixer.A1.value");
        d.frames(2);
        for k in keys {
            d.ok("ui.key", json!({"key": k}));
        }
        d.ok("ui.type", json!({"text": text}));
        d.frames(2);
        d.ok("ui.key", json!({"key": "Enter"}));
        d.frames(3);
    };
    assert!((level(&mut d) + 8.0).abs() < 1e-6, "the demo's -8 dB");
    // the value opens selected: `*2` replaces it and doubles it
    enter(&mut d, &[], "*2");
    assert!((level(&mut d) + 16.0).abs() < 1e-6, "{}", level(&mut d));
    // added after the value: `-16.0/4`
    enter(&mut d, &["End"], "/4");
    assert!((level(&mut d) + 4.0).abs() < 1e-6, "{}", level(&mut d));
    enter(&mut d, &[], "(1+2)*-3");
    assert!((level(&mut d) + 9.0).abs() < 1e-6, "{}", level(&mut d));
    // not a calculation: the value stays
    enter(&mut d, &[], "2*");
    assert!((level(&mut d) + 9.0).abs() < 1e-6, "{}", level(&mut d));
}

/// #817: the number fields of dialogs take calculations too.
#[test]
fn audio_gain_dialog_field_takes_a_calculation() {
    let mut d = Driver::demo();
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = seq["audio"][0]["items"][0]["clip"].as_u64().unwrap();
    d.exec("timeline.select", json!({"clips": [clip]}));
    let peak = |d: &mut Driver| d.exec("clip.audioPeak", json!({"clips": [clip]}))["peakDb"].as_f64().unwrap();
    let before = peak(&mut d);
    d.ok("ui.menu.invoke", json!({"id": "clip.audioGain"}));
    d.frames(3);
    d.click("audioGain.adjust");
    d.click("audioGain.adjust.value");
    d.frames(2);
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "1.5*2"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(2);
    d.click("audioGain.ok");
    assert!((peak(&mut d) - (before + 3.0)).abs() < 0.05, "3 dB louder: {before} -> {}", peak(&mut d));
}

#[test]
fn audio_gain_dialog_normalizes_the_selection() {
    let mut d = Driver::demo();
    let seq = d.exec("sequence.inspect", json!({}));
    let clip = seq["audio"][0]["items"][0]["clip"].as_u64().unwrap();
    d.exec("timeline.select", json!({"clips": [clip]}));
    let r = d.ok("ui.menu.invoke", json!({"id": "clip.audioGain"}));
    assert_eq!(r["dialog"], "audioGain", "{r}");
    d.frames(3);
    let ids = d.ids("audioGain.");
    for id in ["audioGain.set", "audioGain.adjust", "audioGain.normalizeMax", "audioGain.normalizeAll", "audioGain.ok", "audioGain.peak"] {
        assert!(ids.iter().any(|i| i == id), "{id} missing: {ids:?}");
    }
    d.click("audioGain.normalizeMax");
    d.snapshot("mixer-audio-gain-dialog", Some("audioGain."));
    d.click("audioGain.ok");
    let pk = d.exec("clip.audioPeak", json!({"clips": [clip]}));
    assert!(pk["peakDb"].as_f64().unwrap().abs() < 1e-6, "normalized to 0 dB: {pk}");
    assert!(d.ids("audioGain.").is_empty(), "dialog closed");
}

/// #1062: after Stop, the meter must finish its release without pointer events, then let the
/// window sleep. Run only frames requested by egui; forcing frames would hide the original bug.
#[test]
fn paused_meters_repaint_without_input_and_settle_after_release() {
    for workspace in ["Editing", "Audio"] {
        let mut d = Driver::demo_with_step_dt(Some(0.1));
        d.ok("ui.set", json!({"workspace": workspace}));
        d.ok("ui.playback", json!({"action": "play"}));
        d.frames(10);
        let prefix = if workspace == "Audio" { "mixer.Mix.meter" } else { "audioMeters.channel." };
        let playing = d.ok("ui.elements", json!({"prefix": prefix}));
        let labels = |elements: &Value| elements.as_array().unwrap().iter().map(|e| e["label"].as_str().unwrap().to_string()).collect::<Vec<_>>();
        let active = labels(&playing);
        assert!(!active.is_empty(), "{workspace}: the meters must be visible");
        assert!(active.iter().any(|label| !label.contains("-90.0")), "{workspace}: the demo must feed the meters: {active:?}");
        d.ok("ui.playback", json!({"action": "stop"}));
        assert!(!d.harness.state().playback.playing);
        d.snapshot(&format!("meters-{workspace}-paused-start"), None);
        // Harness::run follows immediate repaint requests, adding no mouse or keyboard input.
        let steps = d.harness.run();
        // Inspect both values that drive the bars: a silent level alone must not strand its
        // slower peak hold. No new frames or pointer events are injected to read this state.
        let (_, meters): (u64, std::collections::HashMap<u64, Vec<[f32; 2]>>) =
            d.harness.ctx.data(|data| data.get_temp(egui::Id::new("mixer-meters"))).unwrap();
        assert!(!meters.is_empty());
        assert!(meters.values().flatten().all(|meter| *meter == [-90.0; 2]), "{workspace}: meters stopped after {steps} requested frames: {meters:?}");
        assert_ne!(
            d.harness.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay,
            std::time::Duration::ZERO,
            "{workspace}: silent meters must not keep repainting"
        );
        let quiet = d.ok("ui.elements", json!({"prefix": prefix}));
        assert!(labels(&quiet).iter().all(|label| label.contains("-90.0")), "{workspace}: {:?}", labels(&quiet));
        d.snapshot(&format!("meters-{workspace}-paused-silent"), None);
    }
}

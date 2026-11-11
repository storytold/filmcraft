//! Headless UI tests of the mouse wheel over the Program Monitor and Effect Controls, as Settings ▸
//! Navigation sets it: over a monitor's time ruler the wheel steps frames by default and zooms the
//! ruler when set to; over the picture it zooms the picture by default and can step frames
//! instead; over Effect Controls' keyframe lane it steps frames by default and zooms the lane when
//! set to.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
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

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn set(&mut self, key: &str, value: &str) {
        self.exec("prefs.set", json!({"key": key, "value": value}));
        self.frames(2);
    }

    /// Turn the wheel over the element `id` (positive `dy`: up).
    fn wheel(&mut self, id: &str, dy: f64) {
        self.ok("ui.scroll", json!({"id": id, "fx": 0.5, "fy": 0.5, "dx": 0.0, "dy": dy}));
        self.frames(3);
    }

    fn playhead(&mut self) -> Tick {
        self.app().session.playhead()
    }

    /// Put the playhead near `t` and return where it lands (the app snaps it to a frame).
    fn put_playhead(&mut self, t: Tick) -> Tick {
        self.app().session.set_playhead(t);
        self.frames(3);
        self.playhead()
    }
}

/// Over the Program Monitor's time ruler the wheel steps frames (down: later, up: back) and leaves
/// the ruler's zoom alone; set to Zoom, it zooms the ruler and leaves the playhead.
#[test]
fn the_wheel_over_the_program_ruler_steps_frames_and_zooms_when_set_to() {
    let mut d = Driver::demo();
    let t0 = d.put_playhead(Tick(TICKS_PER_SECOND * 2));
    d.wheel("program.scrubBar", -40.0);
    let t1 = d.playhead();
    assert!(t1 > t0, "a notch down steps a frame later: {t0:?} -> {t1:?}");
    d.wheel("program.scrubBar", 40.0);
    assert_eq!(d.playhead(), t0, "a notch up steps back");
    assert!(d.app().ui.program.time_zoom.is_none(), "stepping does not zoom the ruler");
    d.set("navigation.programWheel", "zoom");
    d.wheel("program.scrubBar", 40.0);
    assert!(d.app().ui.program.time_zoom.is_some(), "set to Zoom, the wheel zooms the ruler");
    assert_eq!(d.playhead(), t0, "and leaves the playhead where it was");
}

/// Over the picture the wheel zooms the picture by default; set to Step Frames, it steps frames and
/// the picture keeps its size.
#[test]
fn the_wheel_over_the_program_picture_zooms_it_or_steps_frames() {
    let mut d = Driver::demo();
    let t0 = d.put_playhead(Tick(TICKS_PER_SECOND * 2));
    d.wheel("program.picture", 40.0);
    assert!(d.app().ui.program.zoom.is_some(), "by default the wheel zooms the picture");
    assert_eq!(d.playhead(), t0);
    d.app().ui.program.zoom = None;
    d.set("navigation.programPreviewWheel", "stepFrames");
    d.wheel("program.picture", -40.0);
    assert!(d.playhead() > t0, "set to Step Frames, a notch down steps a frame later");
    assert!(d.app().ui.program.zoom.is_none(), "and the picture is not zoomed");
}

/// Over Effect Controls' keyframe lane the wheel steps frames by default; set to Zoom, it zooms the
/// lane's time range.
#[test]
fn the_wheel_over_the_effect_controls_lane_steps_frames_and_zooms_when_set_to() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "EffectControls"}));
    let seq = d.exec("sequence.inspect", json!({}));
    let it = seq["video"][0]["items"][0].clone();
    let (clip, start) = (it["clip"].as_u64().unwrap(), it["start"].as_i64().unwrap());
    d.exec("timeline.select", json!({"clips": [clip]}));
    let t0 = d.put_playhead(Tick(start + TICKS_PER_SECOND));
    d.frames(4);
    d.wheel("effectControls.lane", -40.0);
    assert!(d.playhead() > t0, "a notch down over the lane steps a frame later");
    assert!(d.app().ui.effect_controls_zoom.is_none(), "stepping does not zoom the lane");
    let t0 = d.put_playhead(t0);
    d.set("navigation.effectControlsWheel", "zoom");
    d.wheel("effectControls.lane", 40.0);
    assert!(d.app().ui.effect_controls_zoom.is_some(), "set to Zoom, the wheel zooms the lane");
    assert_eq!(d.playhead(), t0, "and leaves the playhead where it was");
}

//! `perf.stats` through the control channel (headless app under `egui_kittest`): the control
//! method and the command id (what MCP `command_run` sends) both return the engine's decode
//! counters plus playback, frame-worker and UI timings. Also how closely the Program monitor's
//! picture follows a value that is dragged and a scrub.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::frames::{FrameKey, Target};
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
                assert_eq!(v["ok"], json!(true), "{method} {params}: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method}");
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    /// The frame due in the Program monitor: `shown` at the project's revision and the playhead.
    fn due(&self, shown: FrameKey) -> FrameKey {
        let s = &self.harness.state().session;
        let frame = s.active_sequence().unwrap().settings.frame_rate.frame_at(s.playhead());
        FrameKey { frame, revision: s.revision, ..shown }
    }

    /// Wait (without a UI pass) until the frame workers have `key` ready.
    fn ready(&self, key: FrameKey) {
        for _ in 0..2000 {
            if self.harness.state().frames.is_ready(&key) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("{key:?} was never rendered");
    }

    /// The Program monitor at rest: the picture of the frame due is on screen.
    fn settled(&mut self) -> FrameKey {
        for _ in 0..2000 {
            self.frames(1);
            if let Some(k) = self.harness.state().program_picture()
                && k == self.due(k)
            {
                return k;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("the Program monitor never showed its frame");
    }
}

#[test]
fn perf_stats_reports_playback_frames_and_decode() {
    let mut d = Driver::demo();
    // let the Program monitor render a few frames on the workers
    for _ in 0..50 {
        d.frames(1);
        std::thread::sleep(std::time::Duration::from_millis(10));
    }
    let v = d.ok("perf.stats", json!({}));
    for (section, keys) in [
        ("decode", &["cacheHitRate", "samplesDecoded", "decodeMs"][..]),
        ("playback", &["shown", "dropped", "dropRate"][..]),
        ("frames", &["jobs", "cancelled", "requestHitRate", "workers", "queued"][..]),
        ("ui", &["fps", "frameMs"][..]),
    ] {
        for k in keys {
            assert!(!v[section][k].is_null(), "{section}.{k} missing in {v}");
        }
    }
    assert!(v["frames"]["decodeMs"]["p95"].is_number() && v["frames"]["renderMs"]["mean"].is_number(), "{v}");
    assert!(v["frames"]["jobs"].as_u64().unwrap() > 0, "the monitor rendered frames: {v}");
    assert_eq!(v["playback"]["playing"], json!(false));
    // the same through the command id (what MCP `command_run` sends in bridge mode)
    let c = d.ok("engine.execute", json!({"command": "perf.stats"}));
    assert!(c["frames"]["workers"].as_u64().unwrap() >= 1 && c["decode"].is_object(), "{c}");
}

/// A value dragged in Effect Controls changes on every UI pass, so the frame the monitor asks for
/// is never ready on the pass that asks. The monitor shows the frame of the pass before (ready by
/// then) and stays one pass behind the mouse; it used to keep the picture from before the drag
/// until the mouse rested.
#[test]
fn program_monitor_follows_a_value_that_changes_every_pass() {
    let mut d = Driver::demo();
    let clip = d.exec("sequence.inspect", json!({}))["video"][0]["items"][0]["clip"].as_u64().unwrap();
    d.exec("playhead.set", json!({"seconds": 1.0}));
    let at_rest = d.settled();
    let mut before = at_rest;
    for i in 1..=12 {
        // one change and one UI pass, as a mouse move while dragging the value
        d.exec("effects.setParam", json!({"clip": clip, "effect": "motion", "param": "scale", "value": 100.0 - i as f64}));
        let due = d.due(at_rest);
        assert_ne!(due.revision, before.revision, "each change is a new revision");
        let shown = d.harness.state().program_picture().unwrap();
        assert!(shown.revision >= before.revision, "change {i}: the picture is at most one change behind, shown {shown:?}, due {due:?}");
        d.ready(due);
        before = due;
    }
    assert_eq!(d.settled().revision, before.revision, "at rest the picture is the frame due");
}

/// The same for a scrub, backwards: every pass asks for an earlier frame, and no cached frame
/// lies before it.
#[test]
fn program_monitor_follows_a_backward_scrub() {
    let mut d = Driver::demo();
    d.exec("playhead.set", json!({"frame": 60}));
    let at_rest = d.settled();
    let mut before = at_rest;
    for i in 1..=12 {
        d.exec("playhead.set", json!({"frame": 60 - i * 3}));
        let due = d.due(at_rest);
        assert_eq!(due.frame, 60 - i * 3);
        let shown = d.harness.state().program_picture().unwrap();
        assert!(shown.frame <= before.frame, "step {i}: the picture is at most one step behind, shown {shown:?}, due {due:?}");
        d.ready(due);
        before = due;
    }
    assert_eq!(d.settled().frame, before.frame);
}

/// A frame being rendered is kept when the same frame is asked for at a newer revision: frames
/// that take longer than a refresh (CPU effects) were each cancelled by the next step of a drag,
/// so none was ever shown until the mouse rested.
#[test]
fn a_newer_revision_keeps_the_frame_being_rendered() {
    let d = Driver::demo();
    let app = d.harness.state();
    let idle = || {
        for _ in 0..200_000 {
            if app.frames.queue_len() == 0 {
                return;
            }
            std::thread::sleep(std::time::Duration::from_micros(50));
        }
        panic!("the frame queue never emptied");
    };
    // the clip with Lumetri Color at full resolution: tens of milliseconds on the CPU
    let seq = app.session.state.active_sequence.unwrap();
    let rate = app.session.active_sequence().unwrap().settings.frame_rate;
    let frame = rate.frame_at(filmcraft_time::Tick::from_seconds_f64(15.0));
    let key = |revision| FrameKey { target: Target::Sequence(seq), frame, size: 1000, revision, draft: false };
    let project = app.session.project.clone();
    idle();
    app.frames.request(key(9001), rate.tick_of(frame), 1.0, &project, 0);
    idle(); // a worker has taken it
    app.frames.request(key(9002), rate.tick_of(frame), 1.0, &project, 0);
    // both arrive, in either order: the one that was rendering was not cancelled for the newer
    d.ready(key(9002));
    d.ready(key(9001));
}

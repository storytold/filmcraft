//! `perf.stats` through the control channel (headless app under `egui_kittest`): the control
//! method and the command id (what MCP `command_run` sends) both return the engine's decode
//! counters plus playback, frame-worker and UI timings. Also how closely the Program monitor's
//! picture follows a value that is dragged and a scrub.

use std::sync::mpsc::{Sender, channel};
use std::time::{Duration, Instant};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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
        wait_for(&format!("{key:?} to be rendered"), Duration::from_millis(5), || self.harness.state().frames.is_ready(&key));
    }

    /// The Program monitor at rest: the picture of the frame due is on screen.
    fn settled(&mut self) -> FrameKey {
        let mut shown = None;
        wait_for("the Program monitor to show its frame", Duration::from_millis(5), || {
            self.frames(1);
            shown = self.harness.state().program_picture().filter(|&k| k == self.due(k));
            shown.is_some()
        });
        shown.unwrap()
    }
}

/// Call `done` every `pause` until it holds, for at most two minutes of wall-clock time.
fn wait_for(what: &str, pause: Duration, mut done: impl FnMut() -> bool) {
    let t0 = Instant::now();
    while !done() {
        assert!(t0.elapsed().as_secs() < 120, "timed out waiting for {what}");
        std::thread::sleep(pause);
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
    d.settled();
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

/// Opening a project gives the app a new media pool and a new frame server. The old server's
/// workers end, so the first project's media pool is released.
#[test]
fn opening_a_project_releases_the_previous_media_pool() {
    let dir = std::env::temp_dir().join(format!("filmcraft-perf-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("reopen.fcproj").to_string_lossy().to_string();

    let mut d = Driver::demo();
    d.settled();
    d.exec("file.saveAs", json!({"path": path}));
    let first = std::sync::Arc::downgrade(&d.harness.state().session.media);
    d.exec("file.open", json!({"path": path}));
    d.frames(3);
    let app = d.harness.state();
    assert!(std::sync::Arc::ptr_eq(&app.frames.pool, &app.session.media), "the frame server serves the opened project's pool");
    for _ in 0..2000 {
        if first.strong_count() == 0 {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert_eq!(first.strong_count(), 0, "something still holds the first project's media pool");
    let _ = std::fs::remove_dir_all(&dir);
}

/// A frame being rendered is kept when the same frame is asked for at a newer revision: frames
/// that take longer than a refresh (CPU effects) were each cancelled by the next step of a drag,
/// so none was ever shown until the mouse rested.
#[test]
fn a_newer_revision_keeps_the_frame_being_rendered() {
    let d = Driver::demo();
    let app = d.harness.state();
    let idle = || wait_for("the frame queue to empty", Duration::from_micros(50), || app.frames.queue_len() == 0);
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

/// #1142, #1606: a backlog of frames to render does not make the UI repaint at 60 Hz on native
/// builds (the workers wake it when a frame is done; only the web pumps frames in the UI pass).
#[test]
fn queued_frames_do_not_poll_the_ui() {
    let mut d = Driver::demo();
    // a frame server with no workers keeps its queue: what a slow backlog looks like to the UI
    let (pool, services, previews) = {
        let s = &d.harness.state().session;
        (s.media.clone(), s.services.clone(), s.previews.clone())
    };
    d.harness.state_mut().frames = std::sync::Arc::new(filmcraft_ui_egui::frames::FrameServer::new(pool, services, previews, 0));
    d.exec("playhead.set", json!({"frame": 7}));
    d.frames(3);
    assert!(d.harness.state().frames.queue_len() > 0, "frames are queued");
    // with nothing else going on the UI goes idle (no repaint asked for) instead of polling
    let idle = (0..5).any(|_| {
        d.frames(1);
        d.harness.output().viewport_output[&egui::ViewportId::ROOT].repaint_delay > std::time::Duration::from_secs(1)
    });
    assert!(idle, "the UI kept repainting while frames were queued");
}

/// #2438 review: when another project's media pool replaces the session's, the frame server is
/// rebuilt in the middle of a frame; it must be able to wake the UI at once, since nothing polls
/// for its queued frames any more.
#[test]
fn a_rebuilt_frame_server_wakes_the_ui_from_its_first_frame() {
    let mut d = Driver::demo();
    let before = d.harness.state().frames.clone();
    d.harness.state_mut().session.media = std::sync::Arc::new(filmcraft_engine::media_pool::MediaPool::default());
    d.harness.step();
    let after = d.harness.state().frames.clone();
    assert!(!std::sync::Arc::ptr_eq(&before, &after), "the frame server was rebuilt");
    assert!(after.wakes_ui(), "its finished frames wake the UI");
}

/// #1682: thumbnail textures (timeline tiles, hover scrubs) are capped instead of one being kept
/// for every frame ever shown.
#[test]
fn thumbnail_textures_are_capped() {
    let mut d = Driver::demo();
    let ctx = d.harness.ctx.clone();
    let item = {
        let p = &d.harness.state().session.project;
        p.items.values().find(|i| i.as_media().is_some_and(|m| m.info.video.is_some())).expect("a video item").id
    };
    let rate = d.harness.state().session.project.item(item).unwrap().frame_rate();
    // 800 distinct frames, 100 per UI pass: request, wait until rendered, then make the textures
    for chunk in 0..8 {
        let frames: Vec<i64> = (chunk * 100..chunk * 100 + 100).collect();
        for &f in &frames {
            d.harness.state_mut().thumbnail(&ctx, item, rate.tick_of(f), 32);
        }
        wait_for("100 thumbnails to be rendered", Duration::from_millis(5), || {
            frames.iter().all(|&f| d.harness.state_mut().thumbnail(&ctx, item, rate.tick_of(f), 32).is_some())
        });
        d.frames(1);
    }
    let n = d.harness.state().thumbnail_textures();
    assert!((100..=512).contains(&n), "{n} thumbnail textures");
}

/// #3361: Effect Controls of a graphic clip with Responsive Design – Time says its clip effects are
/// not remapped; a video clip shows no such hint.
#[test]
fn effect_controls_hint_that_clip_effects_are_not_remapped() {
    let mut d = Driver::demo();
    let hint = |d: &mut Driver| {
        d.frames(4);
        d.ok("ui.elements", json!({"prefix": "effectControls.graphicTimeHint"})).as_array().is_some_and(|a| !a.is_empty())
    };
    d.ok("ui.panel.show", json!({"panel": "Effect Controls"}));
    let first = d.harness.state().session.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    d.exec("timeline.select", json!({"clips": [first]}));
    assert!(!hint(&mut d), "no hint for a video clip");
    let g = d.exec("graphics.newText", json!({"text": "Title"}))["clip"].as_u64().unwrap();
    d.exec("timeline.select", json!({"clips": [g]}));
    assert!(!hint(&mut d), "no hint without a responsive intro / outro");
    d.exec("graphics.setResponsiveTime", json!({"clip": g, "introFrames": 12, "outroFrames": 12}));
    d.exec("timeline.select", json!({"clips": [g]}));
    assert!(hint(&mut d), "the hint shows for a graphic with responsive time");
    // it is a labelled button a keyboard can reach, naming both the layer and the clip effects
    let label = "Responsive Design – Time remaps the graphic's layer keyframes; clip effects like Opacity and Motion keep media time";
    let _ = d.harness.get_by_role_and_label(egui::accesskit::Role::Button, label);
}
/// #1673: a value dragged in Effect Controls while playing makes a revision per UI pass, so the
/// frame due is never ready at the project's revision. The Program monitor shows the frames the
/// workers finish for the last few revisions and keeps playing; it used to hold the picture from
/// before the drag until the mouse rested.
#[test]
fn program_monitor_plays_on_while_a_value_is_dragged() {
    let mut d = Driver::demo();
    let clip = d.exec("sequence.inspect", json!({}))["video"][0]["items"][0]["clip"].as_u64().unwrap();
    d.exec("effects.apply", json!({"clips": [clip], "effect": "crop"}));
    d.exec("playhead.set", json!({"seconds": 1.0}));
    let at_rest = d.settled();
    d.harness.state_mut().play(1.0);
    let mut frames = std::collections::BTreeSet::new();
    let mut newest = at_rest.revision;
    for i in 0..90 {
        // one change and one UI pass, as a mouse move while dragging the value
        d.exec("effects.setParam", json!({"clip": clip, "effect": "crop", "param": "left", "value": (i % 30) as f64}));
        let shown = d.harness.state().program_picture().unwrap();
        frames.insert(shown.frame);
        newest = newest.max(shown.revision);
        std::thread::sleep(std::time::Duration::from_millis(16));
    }
    assert!(frames.len() >= 5, "the picture kept playing during the drag: frames shown {frames:?}");
    assert!(newest > at_rest.revision, "the picture showed the dragged value, not only the one from before");
}

/// The same for playback scheduling: a frame being rendered ahead of the playhead is kept when
/// playback asks for frames at a newer revision (CPU effects take longer than a pass).
#[test]
fn playback_at_a_newer_revision_keeps_the_frame_being_rendered() {
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
    let key = |frame, revision| FrameKey { target: Target::Sequence(seq), frame, size: 1000, revision, draft: false };
    let project = app.session.project.clone();
    idle();
    // starting to play from the frame before it (preroll: every frame, none skipped as too
    // expensive): that one, then `frame` and the next ones as prefetch
    app.frames.schedule_playback(key(frame - 1, 9001), rate, 1.0, &project, 1.0, true);
    idle(); // the workers have taken them
    // one change later, still playing from there
    app.frames.schedule_playback(key(frame - 1, 9002), rate, 1.0, &project, 1.0, true);
    d.ready(key(frame, 9001));
    d.ready(key(frame, 9002));
}

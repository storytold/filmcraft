use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use crate::support::demo_session;
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::Tick;
use filmcraft_ui_egui::{AudioOut, FilmcraftApp, dock::PanelKind, menus};
use serde_json::json;

fn app(item: u64) -> FilmcraftApp {
    let mut session = demo_session();
    session.execute("source.open", json!({"item":item})).unwrap();
    FilmcraftApp::new(session)
}

#[test]
fn source_runs_pauses_seeks_and_does_not_edit_the_sequence() {
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app(5)
    });
    h.step();
    let before = h.state().session.project.to_json();
    let program = h.state().session.playhead();
    h.state_mut().play_source().unwrap();
    for _ in 0..8 {
        h.step();
    }
    assert!(h.state().session.state.source_playhead > Tick::from_seconds_f64(1.0));
    assert_eq!(h.state().session.playhead(), program);
    assert_eq!(h.state().session.project.to_json(), before);
    h.state_mut().session.execute("source.setPlayhead", json!({"seconds":8})).unwrap();
    h.step();
    h.step();
    assert!(h.state().session.state.source_playhead.seconds() >= 8.0);
    assert!(h.state().session.state.source_playhead.seconds() < 9.0);
    h.state_mut().stop_source();
    let stopped = h.state().session.state.source_playhead;
    h.step();
    h.step();
    assert_eq!(h.state().session.state.source_playhead, stopped);
}

#[test]
fn source_stops_at_end_restarts_and_stops_when_item_changes() {
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app(5)
    });
    h.step();
    h.state_mut().session.execute("source.setPlayhead", json!({"seconds":17.5})).unwrap();
    h.state_mut().play_source().unwrap();
    for _ in 0..8 {
        h.step();
    }
    assert!(!h.state().source_playback.clock.playing);
    assert!(h.state().session.state.source_playhead.seconds() < 18.0);
    h.state_mut().play_source().unwrap();
    assert_eq!(h.state().session.state.source_playhead, Tick::ZERO);
    h.state_mut().session.execute("source.open", json!({"item":6})).unwrap();
    h.step();
    assert!(!h.state().source_playback.clock.playing);
}

#[test]
fn space_targets_the_focused_monitor_and_output_is_exclusive() {
    let mut app = app(5);
    let ctx = egui::Context::default();
    app.ui.focused = PanelKind::Source;
    menus::invoke(&mut app, &ctx, "playback.toggle", json!({})).unwrap();
    assert!(app.source_playback.clock.playing);
    assert!(!app.playback.playing);
    menus::invoke(&mut app, &ctx, "playback.toggle", json!({"monitor":"program"})).unwrap();
    assert!(app.playback.playing);
    assert!(!app.source_playback.clock.playing);
    app.play_source().unwrap();
    assert!(!app.playback.playing);
    menus::invoke(&mut app, &ctx, "playback.stop", json!({})).unwrap();
    assert!(!app.source_playback.clock.playing);
}

#[test]
fn empty_source_returns_a_useful_error() {
    let mut app = FilmcraftApp::new(Session::default());
    assert!(app.play_source().unwrap_err().contains("Source monitor"));
    assert!(!app.source_playback.clock.playing);
}

#[test]
fn source_and_program_gpu_pictures_stay_independent() {
    use eframe::egui_wgpu::{RenderState, RendererOptions, WgpuConfiguration};
    use filmcraft_ui_egui::frames::Target;
    use std::time::{Duration, Instant};

    let setup = egui_kittest::wgpu::default_wgpu_setup();
    let instance = pollster::block_on(setup.new_instance());
    let config = WgpuConfiguration { wgpu_setup: setup, ..Default::default() };
    let Ok(rs) = pollster::block_on(RenderState::create(&config, &instance, None, RendererOptions::PREDICTABLE)) else {
        eprintln!("no GPU adapter for monitor test");
        return;
    };
    if let Some(why) = filmcraft_ui_egui::gpu_compositor_unsupported(&rs.adapter) {
        eprintln!("no supported GPU compositor: {why}");
        return;
    }
    let renderer = egui_kittest::wgpu::WgpuTestRenderer::from_render_state(rs.clone());
    let (tx, rx) = std::sync::mpsc::channel();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).renderer(renderer).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        let mut app = app(5).with_control(rx);
        app.set_wgpu(rs);
        app
    });
    let start = Instant::now();
    loop {
        crate::support::step_repaint(&mut h);
        if h.state().source_gpu.as_ref().and_then(|g| g.last_key).is_some() && h.state().gpu.as_ref().and_then(|g| g.last_key).is_some() {
            break;
        }
        assert!(start.elapsed() < Duration::from_secs(30), "both monitors should show a GPU picture");
        std::thread::sleep(Duration::from_millis(5));
    }
    let program_key = h.state().gpu.as_ref().unwrap().last_key;
    let source = h.state().source_gpu.as_ref().unwrap();
    let source_key = source.last_key.unwrap();
    assert!(matches!(source_key.target, Target::ItemPlan(..)), "Source should leave conversion to the GPU");
    assert_ne!(source.texture, h.state().gpu.as_ref().unwrap().texture, "each monitor owns its displayed texture");
    let before = h.state_mut().gpu.as_mut().unwrap().compositor.read_output().unwrap();
    h.state_mut().session.execute("source.setPlayhead", json!({"seconds":4})).unwrap();
    let start = Instant::now();
    while h.state().source_gpu.as_ref().unwrap().last_key == Some(source_key) {
        crate::support::step_repaint(&mut h);
        assert!(start.elapsed() < Duration::from_secs(30), "the source seek should present a new picture");
        std::thread::sleep(Duration::from_millis(5));
    }
    assert_eq!(h.state().gpu.as_ref().unwrap().last_key, program_key, "Source does not move Program");
    assert_eq!(h.state_mut().gpu.as_mut().unwrap().compositor.read_output().unwrap(), before, "Source does not overwrite Program pixels");
    assert!(h.state().frames.recent(source_key, 1, 0, 0).is_some(), "a Source GPU frame participates in playback lookback");

    // Exercise the real UI screenshot request and read the AccessKit tree for the same pass.
    if let Some(dir) = crate::support::shots_dir() {
        std::fs::create_dir_all(&dir).unwrap();
        let (request, result) = filmcraft_ui_egui::control::ControlRequest::new("ui.screenshot", json!({"path":dir.join("source-gpu.png")}));
        tx.send(request).unwrap();
        let start = Instant::now();
        let response = loop {
            crate::support::step_repaint(&mut h);
            if let Ok(response) = result.try_recv() {
                break response;
            }
            assert!(start.elapsed() < Duration::from_secs(15), "UI screenshot should complete");
            std::thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(response["ok"], true, "{response}");
        assert!(dir.join("source-gpu.png").is_file());
        eprintln!("monitor accessibility tree: {:?}", h.root());
    }
}

type Fill = Box<dyn FnMut(&mut [f32], usize) + Send>;
struct Out {
    frames: Arc<AtomicU64>,
    fill: Arc<Mutex<Option<Fill>>>,
    stalled: bool,
}
impl AudioOut for Out {
    fn start(&mut self, fill: Fill) -> Result<u32, String> {
        self.frames.store(0, Ordering::SeqCst);
        *self.fill.lock().unwrap() = Some(fill);
        Ok(48_000)
    }
    fn stop(&mut self) {
        *self.fill.lock().unwrap() = None;
    }
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn played_frames(&self) -> Option<u64> {
        Some(if self.stalled { 0 } else { self.frames.fetch_add(4_800, Ordering::SeqCst) + 4_800 })
    }
}

#[test]
fn audio_source_outputs_sound_and_uses_the_device_clock() {
    let frames = Arc::new(AtomicU64::new(0));
    let fill = Arc::new(Mutex::new(None));
    let mut app = app(11);
    app.audio = Some(Box::new(Out { frames, fill: fill.clone(), stalled: false }));
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app
    });
    h.step();
    h.state_mut().play_source().unwrap();
    for _ in 0..8 {
        h.step();
    }
    assert!(h.state().source_playback.clock.audio_clock);
    assert!(h.state().session.state.source_playhead.seconds() > 0.2);
    let mut heard = false;
    for _ in 0..100 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut buffer = vec![0.0; 960];
        if let Some(f) = fill.lock().unwrap().as_mut() {
            f(&mut buffer, 2);
        }
        heard |= buffer.iter().any(|x| x.abs() > 0.001);
        if heard {
            break;
        }
    }
    assert!(heard, "the audio callback must produce non-silent source samples");
    h.state_mut().stop_source();
}

#[test]
fn stalled_source_audio_falls_back_to_wall_clock() {
    let mut app = app(11);
    app.audio = Some(Box::new(Out { frames: Arc::new(AtomicU64::new(0)), fill: Arc::new(Mutex::new(None)), stalled: true }));
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app
    });
    h.step();
    h.state_mut().play_source().unwrap();
    for _ in 0..16 {
        h.step();
    }
    assert!(!h.state().source_playback.clock.audio_clock);
    assert!(h.state().session.state.source_playhead.seconds() > 1.5);
    h.state_mut().stop_source();
}

#[test]
fn source_range_playback_stops_at_out_and_loop_restarts_without_program_playback() {
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app(5)
    });
    h.step();
    let rate = filmcraft_engine::clip_ops::source_view(&h.state().session, filmcraft_engine::project::ItemId(5)).unwrap().rate;
    let (start, out) = (rate.tick_of(24), rate.tick_of(47));
    h.state_mut().session.execute("project.setMarks", json!({"item":5,"in":start.0,"out":out.0})).unwrap();
    h.state_mut().ui.focused = PanelKind::Source;
    let before = h.state().session.project.to_json();
    let program = h.state().session.playhead();
    let ctx = h.ctx.clone();
    menus::invoke(h.state_mut(), &ctx, "playback.inToOut", json!({})).unwrap();
    assert_eq!(h.state().session.state.source_playhead, start);
    for _ in 0..10 {
        h.step();
    }
    assert!(!h.state().source_playback.clock.playing);
    assert_eq!(h.state().session.state.source_playhead, out);
    assert!(!h.state().playback.playing);
    assert_eq!(h.state().session.playhead(), program);
    menus::invoke(h.state_mut(), &ctx, "playback.loop", json!({})).unwrap();
    menus::invoke(h.state_mut(), &ctx, "playback.inToOut", json!({})).unwrap();
    for _ in 0..20 {
        h.step();
        assert!(h.state().source_playback.clock.playing);
        let t = h.state().session.state.source_playhead;
        assert!(t >= start && t <= out);
    }
    assert_eq!(h.state().session.project.to_json(), before);
    assert!(!h.state().playback.playing);
    h.state_mut().stop_source();
}
#[test]
fn a_single_frame_source_range_starts_at_the_marked_last_frame() {
    let mut h = Harness::builder().with_step_dt(0.1).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app(5)
    });
    // Keep the stepped clock ahead of slow first-frame host/font initialization.
    const START: f64 = 10_000.0;
    h.input_mut().time = Some(START);
    h.step();
    let v = filmcraft_engine::clip_ops::source_view(&h.state().session, filmcraft_engine::project::ItemId(5)).unwrap();
    let last = v.rate.snap(v.end - v.rate.frame_duration());
    h.state_mut().session.execute("project.setMarks", json!({"item":5,"in":last.0,"out":last.0})).unwrap();
    h.state_mut().play_source_range(false, false).unwrap();
    assert_eq!(h.state().session.state.source_playhead, last);
    // RawInput::time overrides the harness clock. Advance it explicitly: sleeping with
    // START held fixed freezes now() when the host clock's offset was captured earlier.
    for step in 1..=200 {
        if !h.state().source_playback.clock.playing {
            break;
        }
        h.input_mut().time = Some(START + f64::from(step) * 0.1);
        h.step();
    }
    assert!(!h.state().source_playback.clock.playing);
    assert_eq!(h.state().session.state.source_playhead, last);
}

#[test]
fn j_and_l_shuttle_the_source_both_ways_and_double_the_speed() {
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app(5)
    });
    h.step();
    h.state_mut().ui.focused = PanelKind::Source;
    let before = h.state().session.project.to_json();
    let program = h.state().session.playhead();
    let ctx = h.ctx.clone();
    for speed in [1.0, 2.0, 4.0, 8.0, 8.0] {
        let r = menus::invoke(h.state_mut(), &ctx, "playback.forward", json!({})).unwrap();
        assert_eq!(r["speed"], speed);
    }
    // J while running forward turns around at normal speed, then runs backward to the first frame
    h.state_mut().session.execute("source.setPlayhead", json!({"seconds":2})).unwrap();
    assert_eq!(menus::invoke(h.state_mut(), &ctx, "playback.reverse", json!({})).unwrap()["speed"], -1.0);
    let at = h.state().session.state.source_playhead;
    h.step();
    h.step();
    assert!(h.state().session.state.source_playhead < at, "J plays the Source backward");
    assert!(!h.state().source_playback.clock.audio_clock, "shuttle speeds play without sound");
    assert_eq!(menus::invoke(h.state_mut(), &ctx, "playback.reverse", json!({})).unwrap()["speed"], -2.0);
    for _ in 0..8 {
        h.step();
    }
    assert!(!h.state().source_playback.clock.playing);
    assert_eq!(h.state().session.state.source_playhead, Tick::ZERO);
    assert_eq!(menus::invoke(h.state_mut(), &ctx, "playback.slowForward", json!({})).unwrap()["speed"], 0.25);
    menus::invoke(h.state_mut(), &ctx, "playback.stop", json!({})).unwrap();
    assert!(!h.state().source_playback.clock.playing);
    assert_eq!(h.state().session.playhead(), program);
    assert!(!h.state().playback.playing);
    assert_eq!(h.state().session.project.to_json(), before);
}

/// A Source monitor session of the demo project's active sequence, with `Out` as the audio device.
fn sequence_in_source(prepare: impl FnOnce(&mut Session)) -> (FilmcraftApp, Arc<Mutex<Option<Fill>>>) {
    let mut session = demo_session();
    prepare(&mut session);
    let seq = session.state.active_sequence.expect("a sequence").0;
    session.execute("source.open", json!({"item": seq})).unwrap();
    let fill = Arc::new(Mutex::new(None));
    let mut app = FilmcraftApp::new(session);
    app.audio = Some(Box::new(Out { frames: Arc::new(AtomicU64::new(0)), fill: fill.clone(), stalled: false }));
    (app, fill)
}

#[test]
fn a_sequence_with_sound_plays_in_the_source_monitor_through_the_device() {
    let (app, fill) = sequence_in_source(|_| {});
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app
    });
    h.step();
    h.state_mut().play_source().unwrap();
    for _ in 0..4 {
        h.step();
    }
    assert!(h.state().source_playback.clock.playing);
    assert!(h.state().source_playback.clock.audio_clock, "the sequence's sound drives the clock");
    assert!(fill.lock().unwrap().is_some(), "the sequence's sound reaches the output");
    h.state_mut().stop_source();
}

#[test]
fn a_sequence_without_sound_plays_in_the_source_monitor_without_starting_the_output() {
    let (app, fill) = sequence_in_source(|s| {
        s.execute("file.newSequence", json!({"name": "Silent", "width": 1280, "height": 720, "fps": 24})).unwrap();
        let picture = s.project.items.values().find(|i| i.as_media().is_some_and(|m| m.info.video.is_some())).expect("a picture item").id.0;
        s.execute("timeline.place", json!({"item": picture, "track": "V1", "time": 0, "insert": false, "audio": false})).unwrap();
    });
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app
    });
    h.step();
    h.state_mut().play_source().unwrap();
    for _ in 0..4 {
        h.step();
    }
    assert!(h.state().source_playback.clock.playing);
    assert!(!h.state().source_playback.clock.audio_clock);
    assert!(fill.lock().unwrap().is_none(), "no sound, so no output stream");
    h.state_mut().stop_source();
}

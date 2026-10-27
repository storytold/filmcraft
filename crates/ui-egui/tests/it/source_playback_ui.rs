use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::Tick;
use filmcraft_ui_egui::{AudioOut, FilmcraftApp, dock::PanelKind, menus};
use serde_json::json;

fn app(item: u64) -> FilmcraftApp {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    session.execute("source.open", json!({"item":item})).unwrap();
    FilmcraftApp::new(session)
}

#[test]
fn source_runs_pauses_seeks_and_does_not_edit_the_sequence() {
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| app(5));
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
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| app(5));
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
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| app);
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

/// #1789: L / L-L in the Source monitor plays sound at 2× and 4×, as the Program monitor does,
/// with and without Maintain Pitch While Shuttling; reverse stays silent.
#[test]
fn source_shuttle_forward_plays_sound() {
    for (speed, maintain) in [(2.0, true), (4.0, false)] {
        let fill = Arc::new(Mutex::new(None));
        let mut app = app(11);
        app.session.prefs.audio.maintain_pitch_shuttling = maintain;
        app.audio = Some(Box::new(Out { frames: Arc::new(AtomicU64::new(0)), fill: fill.clone(), stalled: false }));
        let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| app);
        h.step();
        h.state_mut().shuttle_source(speed).unwrap();
        for _ in 0..4 {
            h.step();
        }
        assert!(h.state().source_playback.clock.audio_clock, "{speed}× plays through the audio device");
        let mut heard = false;
        for _ in 0..200 {
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
        assert!(heard, "{speed}× (maintain pitch {maintain}) must produce non-silent source samples");
        h.state_mut().stop_source();
    }
}

/// The left channel the Source monitor sends the device at `speed`, after the play-ahead has started.
fn shuttle_output(speed: f64, maintain: bool) -> Vec<f32> {
    let fill = Arc::new(Mutex::new(None));
    // a pure 440 Hz tone in the Source monitor
    let mut session = Session::default();
    let item = filmcraft_engine::demo::add_generator(
        std::sync::Arc::make_mut(&mut session.project),
        &session.media,
        filmcraft_media::generators::GeneratorSource::new(
            filmcraft_media::Generator::Tone { hz: 440.0, db: -6.0 },
            0,
            0,
            filmcraft_time::FrameRate::FPS_24,
            Tick::from_seconds_f64(30.0),
        ),
        "Tone",
        filmcraft_project::Label::Iris,
        None,
    );
    session.execute("source.open", json!({"item": item.0})).unwrap();
    let mut app = FilmcraftApp::new(session);
    app.session.prefs.audio.maintain_pitch_shuttling = maintain;
    app.audio = Some(Box::new(Out { frames: Arc::new(AtomicU64::new(0)), fill: fill.clone(), stalled: false }));
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| app);
    h.step();
    h.state_mut().shuttle_source(speed).unwrap();
    h.step();
    let mut out = Vec::new();
    for _ in 0..400 {
        std::thread::sleep(std::time::Duration::from_millis(5));
        let mut buffer = vec![0.0; 960];
        if let Some(f) = fill.lock().unwrap().as_mut() {
            f(&mut buffer, 2);
        }
        if out.is_empty() && buffer.iter().all(|x| x.abs() <= 0.001) {
            continue;
        }
        out.extend(buffer.chunks(2).map(|c| c[0]));
        if out.len() >= 24_000 {
            break;
        }
    }
    h.state_mut().stop_source();
    out
}

/// Maintain Pitch While Shuttling keeps the pitch at 2×: without it the sound is an octave up,
/// with twice the zero crossings.
#[test]
fn source_shuttle_keeps_the_pitch_when_asked() {
    let crossings = |x: &[f32]| x.windows(2).filter(|w| (w[0] < 0.0) != (w[1] < 0.0)).count() as f64 / x.len().max(1) as f64;
    let (kept, raised) = (shuttle_output(2.0, true), shuttle_output(2.0, false));
    assert!(kept.len() >= 24_000 && raised.len() >= 24_000, "{} / {} samples", kept.len(), raised.len());
    let ratio = crossings(&raised) / crossings(&kept);
    assert!(ratio > 1.5, "zero-crossing ratio {ratio}");
}

#[test]
fn stalled_source_audio_falls_back_to_wall_clock() {
    let mut app = app(11);
    app.audio = Some(Box::new(Out { frames: Arc::new(AtomicU64::new(0)), fill: Arc::new(Mutex::new(None)), stalled: true }));
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| app);
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
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| app(5));
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
    let mut h = Harness::builder().with_step_dt(0.1).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| app(5));
    h.step();
    let v = filmcraft_engine::clip_ops::source_view(&h.state().session, filmcraft_engine::project::ItemId(5)).unwrap();
    let last = v.rate.snap(v.end - v.rate.frame_duration());
    h.state_mut().session.execute("project.setMarks", json!({"item":5,"in":last.0,"out":last.0})).unwrap();
    h.state_mut().play_source_range(false, false).unwrap();
    assert_eq!(h.state().session.state.source_playhead, last);
    for _ in 0..4 {
        h.step();
    }
    assert!(!h.state().source_playback.clock.playing);
    assert_eq!(h.state().session.state.source_playhead, last);
}

#[test]
fn j_and_l_shuttle_the_source_both_ways_and_double_the_speed() {
    let mut h = Harness::builder().with_step_dt(0.25).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_cc| app(5));
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

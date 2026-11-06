use super::*;
use crate::AudioOut;
use filmcraft_engine::Session;
use std::sync::atomic::{AtomicBool, Ordering};

#[derive(Clone)]
struct Output {
    opens: Arc<AtomicBool>,
    first_samples: Arc<Mutex<Vec<f32>>>,
}

impl AudioOut for Output {
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn start(&mut self, mut fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        if !self.opens.load(Ordering::Relaxed) {
            return Err("device unavailable".into());
        }
        let mut frame = [0.0; 2];
        fill(&mut frame, 2);
        self.first_samples.lock().unwrap().push(frame[0]);
        Ok(48_000)
    }
    fn stop(&mut self) {}
    fn played_frames(&self) -> Option<u64> {
        Some(0) // opens, but its clock never advances
    }
}

fn app() -> (FilmcraftApp, Output) {
    let mut session = Session::default();
    // Each second has a different level, so the first reopened sample proves the mixer cursor
    // was advanced before opening the output, not just the displayed Source playhead afterward.
    let samples: Vec<f32> = (0..48_000 * 9).flat_map(|i| [0.1 + i as f32 / 48_000.0 * 0.05; 2]).collect();
    let bytes: Arc<[u8]> = filmcraft_engine::previews::write_wav_f32(&samples, 48_000).into();
    let item = filmcraft_engine::commands::import_bytes(&mut session, "/source-retry.wav", bytes, None).unwrap();
    session.execute("source.open", json!({"item": item.0})).unwrap();
    let output = Output { opens: Arc::new(AtomicBool::new(true)), first_samples: Arc::new(Mutex::new(Vec::new())) };
    let mut app = FilmcraftApp::new(session);
    app.audio = Some(Box::new(output.clone()));
    app.play_source().unwrap();
    (app, output)
}

fn advance(app: &mut FilmcraftApp, now: f64) {
    app.set_now_for_test(now);
    app.advance_source_playback(&egui::Context::default());
}

fn stall(app: &mut FilmcraftApp) {
    advance(app, 10.0);
    advance(app, 10.0 + AUDIO_STALL_S);
    advance(app, 11.5);
    assert!(!app.source_playback.clock.audio_clock);
    assert_eq!(app.source_playback.clock.audio_retry_at, 11.0 + AUDIO_RETRY_S);
}

fn assert_sample(output: &Output, seconds: f64) {
    let sample = *output.first_samples.lock().unwrap().last().unwrap();
    assert!((sample - (0.1 + seconds as f32 * 0.05)).abs() < 0.00001, "reopened at the wrong source position: {sample}");
}

fn snapped(app: &FilmcraftApp, seconds: f64) -> Tick {
    source_view(&app.session, app.session.state.source_item.unwrap()).unwrap().rate.snap(Tick::from_seconds_f64(seconds))
}

#[test]
fn delayed_source_retry_advances_wall_time_before_opening_audio() {
    for speed in [1.0, 2.0] {
        let (mut app, output) = app();
        stall(&mut app);
        app.source_playback.clock.speed = speed;
        advance(&mut app, 14.25); // no UI frames since 11.5; retry was due at 13
        let expected = Tick::from_seconds_f64(3.25 * speed);
        assert_eq!(app.source_playback.clock.anchor_tick, expected);
        assert_eq!(app.source_playback.clock.anchor_time, 14.25);
        let view = source_view(&app.session, app.session.state.source_item.unwrap()).unwrap();
        assert_eq!(app.session.state.source_playhead, view.rate.snap(expected));
        if speed == 1.0 {
            assert!(app.source_playback.clock.audio_clock);
            assert_sample(&output, app.session.state.source_playhead.seconds());
        }
        app.stop_source();
    }
}

#[test]
fn delayed_source_retry_failure_keeps_wall_time_for_the_next_attempt() {
    let (mut app, output) = app();
    stall(&mut app);
    output.opens.store(false, Ordering::Relaxed);
    advance(&mut app, 14.25);
    assert!(!app.source_playback.clock.audio_clock);
    assert_eq!(app.source_playback.clock.anchor_tick, Tick::from_seconds_f64(3.25));
    advance(&mut app, 15.25);
    assert_eq!(app.session.state.source_playhead, snapped(&app, 4.25));
    output.opens.store(true, Ordering::Relaxed);
    advance(&mut app, 17.25);
    assert_eq!(app.session.state.source_playhead, snapped(&app, 6.25));
    assert_sample(&output, app.session.state.source_playhead.seconds());
    app.stop_source();
}

#[test]
fn a_source_seek_reanchors_before_a_due_retry() {
    let (mut app, output) = app();
    stall(&mut app);
    app.session.execute("source.setPlayhead", json!({"seconds": 5.0})).unwrap();
    let sought = app.session.state.source_playhead;
    advance(&mut app, 14.25);
    assert_eq!(app.session.state.source_playhead, sought);
    assert_eq!(app.source_playback.clock.anchor_tick, sought);
    assert_sample(&output, sought.seconds());
    app.stop_source();
}

#[test]
fn a_delayed_source_retry_still_stops_or_loops_at_the_end() {
    for looping in [false, true] {
        let (mut app, output) = app();
        stall(&mut app);
        app.source_playback.clock.looping = looping;
        let view = source_view(&app.session, app.session.state.source_item.unwrap()).unwrap();
        advance(&mut app, 21.0); // wall time has crossed the nine-second source's end
        assert_eq!(app.source_playback.clock.playing, looping);
        if looping {
            assert_eq!(app.session.state.source_playhead, Tick::ZERO);
            assert_sample(&output, 0.0);
        } else {
            assert_eq!(app.session.state.source_playhead, view.rate.snap(view.end - view.rate.frame_duration()));
        }
        app.stop_source();
    }
}

#[test]
fn a_source_retry_before_the_first_tick_has_no_elapsed_playback() {
    let (mut app, output) = app();
    app.stop_source();
    output.opens.store(false, Ordering::Relaxed);
    app.play_source().unwrap();
    assert!(app.source_playback.clock.anchor_time < 0.0);
    advance(&mut app, 10.0);
    assert!(app.source_playback.clock.playing);
    assert_eq!(app.session.state.source_playhead, Tick::ZERO);
    assert_eq!(app.source_playback.clock.anchor_tick, Tick::ZERO);
    assert_eq!(app.source_playback.clock.anchor_time, 10.0);
    app.stop_source();
}

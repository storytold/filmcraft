//! Playback clock: the audio device drives the playhead while it plays, and a device that opens
//! but never consumes samples (seen on Linux with ALSA, #136) must not freeze playback.

use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::Tick;
use filmcraft_ui_egui::{AudioOut, FilmcraftApp};
use serde_json::json;

/// An output whose stream opens, then plays `per_poll` frames each time the clock is read
/// (0 = a stalled device that never calls back).
struct FakeOut {
    played: Arc<AtomicU64>,
    per_poll: u64,
    started: bool,
    note: Option<String>,
}

impl AudioOut for FakeOut {
    fn start(&mut self, _fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        self.played.store(0, Ordering::SeqCst);
        self.started = true;
        Ok(48_000)
    }
    fn stop(&mut self) {
        self.started = false;
    }
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn played_frames(&self) -> Option<u64> {
        self.started.then(|| self.played.fetch_add(self.per_poll, Ordering::SeqCst) + self.per_poll)
    }
    fn note(&self) -> Option<String> {
        self.note.clone()
    }
}

/// Play for `steps` × 0.25 s; return position, current clock and frames using wall time.
fn play_with(per_poll: u64, steps: usize) -> (f64, bool, usize) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.set_playhead(Tick::ZERO);
    let mut app = FilmcraftApp::new(s);
    app.audio = Some(Box::new(FakeOut { played: Arc::new(AtomicU64::new(0)), per_poll, started: false, note: None }));
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).with_step_dt(0.25).build_eframe(move |_cc| app);
    h.step();
    h.state_mut().play(1.0);
    let mut wall_clock_steps = 0;
    for _ in 0..steps {
        h.step();
        if h.state().playback.preroll.is_none() && !h.state().playback.audio_clock {
            wall_clock_steps += 1;
        }
    }
    let app = h.state_mut();
    (app.session.playhead().seconds(), app.playback.audio_clock, wall_clock_steps)
}

/// #136: the stream opened, the device never asked for samples, the audio clock stayed at 0 and
/// the playhead never moved. Playback still advances if every reopened stream stalls (#3785).
#[test]
fn a_stalled_audio_device_does_not_freeze_playback() {
    let (t, _, wall_clock_steps) = play_with(0, 40);
    assert!(t > 4.0, "the playhead keeps moving across repeated stalls, it is at {t:.2} s");
    // At the final frame the device may be in a fresh retry's grace period; it must have
    // relinquished the clock between attempts rather than freezing playback indefinitely.
    assert!(wall_clock_steps >= 8, "playback must use wall time while waiting to retry");
}

/// Control: a device that plays keeps driving the clock (0.1 s of audio per poll).
#[test]
fn a_playing_audio_device_drives_the_clock() {
    let (t, audio_clock, wall_clock_steps) = play_with(4_800, 8);
    assert!(t > 0.2, "{t:.2} s");
    assert!(audio_clock);
    assert_eq!(wall_clock_steps, 0, "a working device never falls back to wall time");
}

/// A fallback note from the audio device (broken ALSA `default`, #23/#106) reaches the status
/// bar on the first frame, and survives while playback runs.
#[test]
fn a_fallback_note_from_the_audio_device_reaches_the_status_bar() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.set_playhead(Tick::ZERO);
    let note = "Audio hardware: the default output device cannot be opened; using 'sofhdadsp'".to_string();
    let mut app = FilmcraftApp::new(s);
    app.audio = Some(Box::new(FakeOut { played: Arc::new(AtomicU64::new(0)), per_poll: 4_800, started: false, note: Some(note.clone()) }));
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).with_step_dt(0.25).build_eframe(move |_cc| app);
    h.step();
    assert_eq!(h.state_mut().ui.status, note, "the note surfaces on the first frame (apply_prefs)");
    h.state_mut().play(1.0);
    for _ in 0..16 {
        h.step();
    }
    assert_eq!(h.state_mut().ui.status, note, "the note survives while playback runs");
}

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
    /// Clock reads during which the output reports `waiting` and plays nothing (a stream
    /// starting, or reconnecting after its server restarted).
    wait_polls: u64,
    polls: Arc<AtomicU64>,
    /// The clock counts frames heard, not frames handed over.
    heard: bool,
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
        if !self.started {
            return None;
        }
        if self.polls.fetch_add(1, Ordering::SeqCst) < self.wait_polls {
            return Some(self.played.load(Ordering::SeqCst));
        }
        Some(self.played.fetch_add(self.per_poll, Ordering::SeqCst) + self.per_poll)
    }
    fn waiting(&self) -> bool {
        self.started && self.polls.load(Ordering::SeqCst) < self.wait_polls
    }
    fn clock_is_heard(&self) -> bool {
        self.heard
    }
    fn note(&self) -> Option<String> {
        self.note.clone()
    }
}

/// Play for `steps` × 0.25 s of simulated time with the given output; the playhead in seconds.
fn play_with(per_poll: u64, steps: usize) -> (f64, bool) {
    play_waiting(per_poll, 0, steps)
}

/// [`play_with`] on an output that first waits for `wait_polls` clock reads.
fn play_waiting(per_poll: u64, wait_polls: u64, steps: usize) -> (f64, bool) {
    play_out(per_poll, wait_polls, false, steps)
}

/// [`play_waiting`] with an output whose clock is the heard sound (`heard`) or not.
fn play_out(per_poll: u64, wait_polls: u64, heard: bool, steps: usize) -> (f64, bool) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.set_playhead(Tick::ZERO);
    let mut app = FilmcraftApp::new(s);
    app.audio = Some(Box::new(FakeOut {
        played: Arc::new(AtomicU64::new(0)),
        per_poll,
        started: false,
        note: None,
        wait_polls,
        polls: Arc::new(AtomicU64::new(0)),
        heard,
    }));
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).with_step_dt(0.25).build_eframe(move |_cc| app);
    h.step();
    h.state_mut().play(1.0);
    for _ in 0..steps {
        h.step();
    }
    let app = h.state_mut();
    (app.session.playhead().seconds(), app.playback.audio_clock)
}

/// #136: the stream opened, the device never asked for samples, the audio clock stayed at 0 and
/// the playhead never moved. After a short grace period playback falls back to the wall clock.
#[test]
fn a_stalled_audio_device_does_not_freeze_playback() {
    let (t, audio_clock) = play_with(0, 16);
    assert!(t > 1.5, "the playhead should move with the wall clock, it is at {t:.2} s");
    assert!(!audio_clock, "the stalled device no longer drives the clock");
}

/// Control: a device that plays keeps driving the clock (0.1 s of audio per poll).
#[test]
fn a_playing_audio_device_drives_the_clock() {
    let (t, audio_clock) = play_with(4_800, 8);
    assert!(t > 0.2, "{t:.2} s");
    assert!(audio_clock);
}

/// An output that is waiting (a stream starting, or reconnecting while its sound server restarts)
/// keeps the clock: the picture holds with the silent clock instead of going on without sound
/// after the stall limit. Before, a server restart longer than 1 s lost the sound for good.
#[test]
fn a_waiting_audio_output_keeps_the_clock() {
    let (t, audio_clock) = play_waiting(4_800, 8, 16);
    assert!(audio_clock, "the output still drives the clock after waiting longer than the stall limit");
    assert!(t < 1.5, "the picture held while nothing was heard: {t:.2} s, not the wall clock's 4 s");
    // control: the same output without `waiting` is given up as stalled
    let (_, audio_clock) = play_with(0, 16);
    assert!(!audio_clock);
}

/// With an output whose clock is the heard sound, the playhead is drawn for the moment its frame
/// reaches the screen, a frame ahead of the sound (here capped: the test steps 0.25 s frames). A
/// clock of frames handed to the device gets no lead, as its buffer already puts it ahead.
#[test]
fn a_heard_clock_draws_the_playhead_a_frame_ahead() {
    let (handed, _) = play_out(4_800, 0, false, 8);
    let (heard, audio_clock) = play_out(4_800, 0, true, 8);
    assert!(audio_clock);
    let lead = heard - handed;
    // the playhead is on the frame grid (23.976 fps here), so the 50 ms lead shows as one frame
    assert!(lead > 0.04 && lead <= filmcraft_ui_egui::MAX_DISPLAY_LEAD_S, "lead {lead:.4} s");
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
    app.audio = Some(Box::new(FakeOut {
        played: Arc::new(AtomicU64::new(0)),
        per_poll: 4_800,
        started: false,
        note: Some(note.clone()),
        wait_polls: 0,
        polls: Arc::new(AtomicU64::new(0)),
        heard: false,
    }));
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).with_step_dt(0.25).build_eframe(move |_cc| app);
    h.step();
    assert_eq!(h.state_mut().ui.status, note, "the note surfaces on the first frame (apply_prefs)");
    h.state_mut().play(1.0);
    for _ in 0..16 {
        h.step();
    }
    assert_eq!(h.state_mut().ui.status, note, "the note survives while playback runs");
}

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

/// Play for `steps` × 0.25 s of simulated time with the given output; the playhead in seconds.
fn play_with(per_poll: u64, steps: usize) -> (f64, bool) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.set_playhead(Tick::ZERO);
    let mut app = FilmcraftApp::new(s);
    app.audio = Some(Box::new(FakeOut { played: Arc::new(AtomicU64::new(0)), per_poll, started: false, note: None }));
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

/// A 1080p25 long-GOP H.264 clip with AAC sound (one keyframe in 250 frames), made with ffmpeg.
/// `None` only when ffmpeg is not installed; any other fixture failure fails the test.
fn long_gop_clip() -> Option<std::path::PathBuf> {
    let dir = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures");
    std::fs::create_dir_all(&dir).unwrap_or_else(|e| panic!("fixture directory {}: {e}", dir.display()));
    let path = dir.join("play-start-1080p.mp4");
    if !path.exists() {
        let tmp = dir.join(format!("play-start-{}.tmp.mp4", std::process::id()));
        let status = std::process::Command::new("ffmpeg")
            .args(["-v", "error", "-y", "-f", "lavfi", "-i", "testsrc2=size=1920x1080:rate=25:duration=12"])
            .args(["-f", "lavfi", "-i", "sine=frequency=440:duration=12"])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-g", "250", "-bf", "2", "-pix_fmt", "yuv420p", "-c:a", "aac", "-shortest"])
            .arg(&tmp)
            .status();
        match status {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return None,
            Err(e) => panic!("running ffmpeg: {e}"),
            Ok(s) if !s.success() => panic!("ffmpeg could not make the fixture: {s}"),
            Ok(_) => {}
        }
        std::fs::rename(&tmp, &path).unwrap_or_else(|e| panic!("moving the fixture into place: {e}"));
    }
    Some(path)
}

/// Play on the long-GOP clip from the middle of its group of pictures, after `idle_frames` UI
/// frames of the paused picture, with `pause` of real time per UI frame (the UI clock advances
/// 1/60 s per frame either way). Returns the UI frames and the real seconds until the clock
/// started, and whether the first frames were ready then (not a timeout); `None` without ffmpeg.
fn play_start(idle_frames: usize, pause: std::time::Duration) -> Option<(usize, f64, bool)> {
    let clip = long_gop_clip()?;
    let mut s = Session::default();
    let r = s.execute("file.import", json!({"paths": [clip.to_string_lossy()]})).unwrap();
    let item = r["items"][0].as_u64().unwrap();
    s.execute("file.newSequenceFromClip", json!({"items": [item]})).unwrap();
    s.set_playhead(Tick::from_seconds_f64(6.0));
    let mut app = FilmcraftApp::new(s);
    app.audio = Some(Box::new(FakeOut { played: Arc::new(AtomicU64::new(0)), per_poll: 800, started: false, note: None }));
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).with_step_dt(1.0 / 60.0).build_eframe(move |_cc| app);
    for _ in 0..idle_frames {
        std::thread::sleep(pause);
        h.step();
    }
    h.state_mut().play(1.0);
    let t0 = std::time::Instant::now();
    let mut frames = 0;
    while h.state_mut().playback.preroll.is_some() && frames < 120 {
        std::thread::sleep(pause);
        h.step();
        frames += 1;
    }
    let ready = h.state_mut().playback.preroll_ready;
    Some((frames, t0.elapsed().as_secs_f64(), ready))
}

/// #3600, wall clock: real seconds from Play to the clock starting on the long-GOP clip. Depends
/// on decode speed and machine load, so it is a timing run (docs/testing.md §5): `cargo test
/// --release -p filmcraft-ui-egui --test it play_start_wait -- --ignored --nocapture` on an idle
/// machine. About 0.15 s on an Apple-silicon Mac; it was 0.53 s right after a seek.
#[test]
#[ignore = "wall-clock timing: run on an idle machine (docs/testing.md §5)"]
fn play_start_wait_in_real_time() {
    let frame = std::time::Duration::from_millis(16);
    let (Some((warm_frames, warm, warm_ready)), Some((cold_frames, cold, cold_ready))) = (play_start(90, frame), play_start(3, frame)) else {
        eprintln!("SKIPPED (play_start_wait_in_real_time): ffmpeg not installed");
        return;
    };
    eprintln!("Play waited {warm:.3} s after the picture settled, {cold:.3} s right after the seek");
    assert!(warm < 0.2, "Play waited {warm:.2} s with the picture settled");
    assert!(cold < 0.25, "Play waited {cold:.2} s right after the seek");
    // and it did wait for frames, or they were ready: a cap of 0 would start the clock on the first UI frame
    // (the frames being ready sooner ends the wait sooner, which is fine)
    assert!(
        (warm_ready || warm_frames >= 4) && (cold_ready || cold_frames >= 4),
        "Play started the clock early without the frames ready: {warm_frames} / {cold_frames} UI frames"
    );
}

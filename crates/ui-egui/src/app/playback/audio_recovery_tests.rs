use super::*;
use crate::{AudioOut, shuttle_mix_rate};
use filmcraft_engine::Session;
use serde_json::json;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Device {
    ready: bool,
    starts: Arc<AtomicUsize>,
    reading: Option<u64>,
}

impl AudioOut for Device {
    fn sample_rate(&self) -> u32 {
        48000
    }
    fn channels(&self) -> usize {
        2
    }
    fn configure(&mut self, hardware: &filmcraft_engine::settings::AudioHardwarePrefs, _: Option<u32>) {
        self.ready = hardware.default_output == "available";
    }
    fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        self.starts.fetch_add(1, Ordering::Relaxed);
        if self.ready { Ok(48000) } else { Err("device unavailable".into()) }
    }
    fn stop(&mut self) {}
    fn played_frames(&self) -> Option<u64> {
        self.reading
    }
}

#[test]
fn forward_shuttle_playback_keeps_audio_enabled() {
    assert_eq!(shuttle_mix_rate(48_000, 1.0), Some(48_000));
    assert_eq!(shuttle_mix_rate(48_000, 2.0), Some(24_000));
    assert_eq!(shuttle_mix_rate(48_000, 4.0), Some(12_000));
    assert_eq!(shuttle_mix_rate(48_000, -1.0), None);
    assert_eq!(shuttle_mix_rate(0, 2.0), None);
    assert_eq!(shuttle_mix_rate(48_000, f64::NAN), None);

    let mut session = Session::default();
    session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
    let mut app = FilmcraftApp::new(session);
    let starts = Arc::new(AtomicUsize::new(0));
    app.audio = Some(Box::new(Device { ready: true, starts: starts.clone(), reading: Some(0) }));
    app.play(2.0);
    app.end_preroll(0.0);
    assert_eq!(starts.load(Ordering::Relaxed), 1, "2x playback must start audio output");
    assert!(app.playback.audio_clock, "the device is still the master clock");
    app.stop();
}

/// Capture the block primed by the real shuttle playback path, without relying on a
/// physical device or consuming the producer faster than a device would.
#[test]
fn maintain_pitch_shuttling_controls_the_samples_sent_to_the_device() {
    use std::sync::Mutex;
    struct Capture(Arc<Mutex<Vec<f32>>>);
    impl AudioOut for Capture {
        fn start(&mut self, mut fill: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
            let mut buf = vec![0.0; 2048];
            fill(&mut buf, 2);
            *self.0.lock().unwrap() = buf;
            Ok(48_000)
        }
        fn stop(&mut self) {}
        fn sample_rate(&self) -> u32 {
            48_000
        }
        fn played_frames(&self) -> Option<u64> {
            Some(4_800)
        }
    }
    fn assert_shuttle_tones(buf: &[f32], maintain: bool, speed: f64) {
        let amplitude = |samples: &[f32], ch: usize, hz: f64| {
            let (mut re, mut im) = (0.0, 0.0);
            for (i, frame) in samples.as_chunks::<2>().0.iter().enumerate() {
                let phase = std::f64::consts::TAU * hz * i as f64 / 48_000.0;
                re += f64::from(frame[ch]) * phase.cos();
                im += f64::from(frame[ch]) * phase.sin();
            }
            2.0 * re.hypot(im) / (samples.len() / 2) as f64
        };
        for (ch, tone) in [440.0, 660.0].into_iter().enumerate() {
            let hz = if maintain { tone } else { tone * speed };
            let level = amplitude(buf, ch, hz);
            assert!(level > 0.08, "maintain={maintain}, speed={speed}, channel={ch}: {hz} Hz level={level}");
            if speed != 1.0 {
                let wrong = if maintain { tone * speed } else { tone };
                assert!(amplitude(buf, ch, wrong) < level * 0.3, "wrong pitch {wrong} Hz at {speed}x, maintain={maintain}");
            }
        }
    }
    let mut session = Session::default();
    session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
    let inter: Vec<f32> =
        (0..48_000 * 8).flat_map(|i| [440.0, 660.0].map(|hz| (0.2 * (std::f64::consts::TAU * hz * i as f64 / 48_000.0).sin()) as f32)).collect();
    let bytes: Arc<[u8]> = filmcraft_engine::previews::write_wav_f32(&inter, 48_000).into();
    let item = filmcraft_engine::commands::import_bytes(&mut session, "/shuttle-tone.wav", bytes, None).unwrap();
    session.execute("timeline.place", json!({"item":item.0,"audioTrack":"A1","seconds":0.0})).unwrap();
    let mut app = FilmcraftApp::new(session);
    let samples = Arc::new(Mutex::new(Vec::new()));
    app.audio = Some(Box::new(Capture(samples.clone())));
    let mut normal = None;
    for maintain in [true, false] {
        app.session.prefs.audio.maintain_pitch_shuttling = maintain;
        for speed in [0.5, 1.0, 1.5, 2.0, 4.0] {
            app.session.set_playhead(Tick::from_seconds_f64(1.0));
            let expected = app.session.sequence_rate().snap(app.session.playhead() + Tick::from_seconds_f64(0.1 * speed));
            app.play(speed);
            app.end_preroll(0.0);
            assert!(app.playback.audio_clock);
            let buf = samples.lock().unwrap();
            assert_shuttle_tones(&buf, maintain, speed);
            if speed == 1.0 {
                if let Some(previous) = &normal {
                    assert_eq!(&*buf, previous, "1x must bypass pitch processing");
                } else {
                    normal = Some(buf.clone());
                }
            }
            drop(buf);
            app.advance_playback(&egui::Context::default());
            assert_eq!(app.session.playhead(), expected, "the device clock must advance the timeline at {speed}x, snapped to a video frame");
            app.stop();
        }
    }
}

#[test]
fn changing_pitch_preference_restarts_audio_at_the_current_playhead() {
    let mut session = Session::default();
    session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
    let ctx = egui::Context::default();
    let mut app = FilmcraftApp::new(session);
    let starts = Arc::new(AtomicUsize::new(0));
    app.audio = Some(Box::new(Device { ready: true, starts: starts.clone(), reading: Some(0) }));
    app.session.prefs.audio_hardware.default_output = "available".into();
    app.apply_prefs(&ctx);
    app.play(2.0);
    app.end_preroll(0.0);
    assert_eq!(starts.load(Ordering::Relaxed), 1);
    app.session.set_playhead(Tick::from_seconds_f64(2.0));
    let position = app.session.playhead();
    for (maintain, expected_starts) in [(false, 2), (true, 3)] {
        app.session.execute("prefs.set", json!({"key":"audio.maintainPitchShuttling","value":maintain})).unwrap();
        app.apply_prefs(&ctx);
        assert_eq!(starts.load(Ordering::Relaxed), expected_starts);
        assert!(app.playback.audio_clock);
        assert_eq!(app.playback.speed, 2.0);
        assert_eq!(app.playback.anchor_tick, app.session.playhead());
        assert_eq!(app.session.playhead(), position);
        app.apply_prefs(&ctx);
        assert_eq!(starts.load(Ordering::Relaxed), expected_starts, "unchanged settings do not restart");
    }
    for speed in [-1.0, 8.0] {
        app.play(speed);
        app.end_preroll(0.0);
        assert!(!app.playback.audio_clock, "unsupported speed {speed} remains silent");
    }
    assert_eq!(starts.load(Ordering::Relaxed), 3);
    app.stop();
}

#[test]
fn changing_hardware_recovers_wall_clock_playback_without_rewinding() {
    let mut session = Session::default();
    session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
    let ctx = egui::Context::default();
    let mut app = FilmcraftApp::new(session);
    let starts = Arc::new(AtomicUsize::new(0));
    app.audio = Some(Box::new(Device { ready: false, starts: starts.clone(), reading: Some(0) }));
    app.apply_prefs(&ctx);
    app.play(1.0);
    app.end_preroll(0.0);
    assert!(!app.playback.audio_clock);
    app.session.set_playhead(Tick::from_seconds_f64(2.0));
    app.session.prefs.audio_hardware.default_output = "available".into();
    app.apply_prefs(&ctx);
    assert_eq!(starts.load(Ordering::Relaxed), 2);
    assert!(app.playback.audio_clock);
    assert_eq!(app.playback.anchor_tick, app.session.playhead());
    app.stop();
}

/// A device that loses its playback clock resumes from the displayed frame and reports a status message.
#[test]
fn a_failed_device_clock_resumes_from_the_displayed_frame() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let ctx = egui::Context::default();
    let mut app = FilmcraftApp::new(session);
    app.audio = Some(Box::new(Device { ready: true, starts: Arc::new(AtomicUsize::new(0)), reading: None }));
    let displayed = app.session.sequence_rate().tick_of(48);
    app.session.set_playhead(displayed);
    app.playback.playing = true;
    app.playback.audio_clock = true;
    app.playback.anchor_tick = Tick::ZERO;
    app.playback.anchor_time = 0.0;
    app.set_now_for_test(10.0);
    let mut output = ctx.run_ui(egui::RawInput { time: Some(10.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
    output.textures_delta.clear();
    assert!(!app.playback.audio_clock);
    assert_eq!(app.playback.anchor_tick, displayed);
    assert_eq!(app.session.playhead(), displayed);
    assert!(app.ui.status.contains("Audio output failed"));
    app.stop();
}

/// A stream the device dropped, which `recover` restarts on the same clock.
struct Restartable {
    reading: Option<u64>,
}

impl AudioOut for Restartable {
    fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        Ok(48000)
    }
    fn stop(&mut self) {}
    fn sample_rate(&self) -> u32 {
        48000
    }
    fn played_frames(&self) -> Option<u64> {
        self.reading
    }
    fn recover(&mut self) {
        self.reading = self.reading.or(Some(48_000));
    }
}

/// A recovered stream keeps the device clock and the sound keeps playing.
#[test]
fn a_restarted_stream_keeps_the_device_clock_and_the_sound() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let ctx = egui::Context::default();
    let mut app = FilmcraftApp::new(session);
    app.audio = Some(Box::new(Restartable { reading: None }));
    app.playback.playing = true;
    app.playback.audio_clock = true;
    app.playback.anchor_tick = Tick::ZERO;
    app.playback.anchor_time = 0.0;
    app.playback.audio_seen = (0, 10.0);
    app.set_now_for_test(10.0);
    let mut output = ctx.run_ui(egui::RawInput { time: Some(10.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
    output.textures_delta.clear();
    assert!(app.playback.audio_clock, "the restarted stream is still the master clock");
    assert!(!app.ui.status.contains("Audio output failed"));
    // 48 000 frames played at 48 kHz: one second in, on a frame boundary
    let (shown, second) = (app.session.playhead(), Tick::from_seconds_f64(1.0));
    assert!(shown <= second && shown + app.session.sequence_rate().frame_duration() > second, "the playhead follows the frames the device played: {shown:?}");
    app.stop();
}

/// A device whose stream dies mid-play (`ticking`) or whose open fails (`opens`).
struct Reviving {
    starts: Arc<AtomicUsize>,
    opens: Arc<AtomicBool>,
    ticking: Arc<AtomicBool>,
}

impl AudioOut for Reviving {
    fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        self.starts.fetch_add(1, Ordering::Relaxed);
        if self.opens.load(Ordering::Relaxed) { Ok(48_000) } else { Err("device unavailable".into()) }
    }
    fn stop(&mut self) {}
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn played_frames(&self) -> Option<u64> {
        self.ticking.load(Ordering::Relaxed).then_some(0)
    }
}

/// A lost output is reopened on retry and the sound returns without pause/play.
#[test]
fn a_lost_output_is_reopened_and_the_sound_returns() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let ctx = egui::Context::default();
    let mut app = FilmcraftApp::new(session);
    let starts = Arc::new(AtomicUsize::new(0));
    let ticking = Arc::new(AtomicBool::new(true));
    app.audio = Some(Box::new(Reviving { starts: starts.clone(), opens: Arc::new(AtomicBool::new(true)), ticking: ticking.clone() }));
    app.play(1.0);
    app.end_preroll(0.0);
    assert!(app.playback.audio_clock && starts.load(Ordering::Relaxed) == 1);
    // the stream dies: the device stops reporting a playback clock
    ticking.store(false, Ordering::Relaxed);
    app.set_now_for_test(10.0);
    let mut output = ctx.run_ui(egui::RawInput { time: Some(10.0), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
    output.textures_delta.clear();
    assert!(!app.playback.audio_clock);
    // the device recovers before the next retry: playback reopens it itself
    ticking.store(true, Ordering::Relaxed);
    app.set_now_for_test(10.0 + AUDIO_RETRY_S + 0.5);
    let mut output = ctx.run_ui(egui::RawInput { time: Some(10.0 + AUDIO_RETRY_S + 0.5), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
    output.textures_delta.clear();
    assert_eq!(starts.load(Ordering::Relaxed), 2, "the output is reopened without pause/play");
    assert!(app.playback.audio_clock, "the fresh stream is the master clock again");
    app.stop();
}

/// A device that becomes available mid-playback starts sounding on the next retry.
#[test]
fn a_device_that_appears_mid_playback_starts_sounding() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let ctx = egui::Context::default();
    let mut app = FilmcraftApp::new(session);
    let starts = Arc::new(AtomicUsize::new(0));
    let opens = Arc::new(AtomicBool::new(false));
    app.audio = Some(Box::new(Reviving { starts: starts.clone(), opens: opens.clone(), ticking: Arc::new(AtomicBool::new(true)) }));
    app.play(1.0);
    app.end_preroll(0.0);
    assert!(!app.playback.audio_clock, "no device yet: playing on the wall clock");
    // headphones plug in while playing: the next retry opens them
    opens.store(true, Ordering::Relaxed);
    app.set_now_for_test(AUDIO_RETRY_S + 0.5);
    let mut output = ctx.run_ui(egui::RawInput { time: Some(AUDIO_RETRY_S + 0.5), ..Default::default() }, |ui| app.advance_playback(ui.ctx()));
    output.textures_delta.clear();
    assert_eq!(starts.load(Ordering::Relaxed), 2, "the output is retried without pause/play");
    assert!(app.playback.audio_clock);
    app.stop();
}

/// #2633: a press that only moves the playhead has nothing to stop, so it must not close the
/// audio stream (closing it makes the next scrub grain open the device again).
#[test]
fn only_a_running_transport_has_something_to_stop() {
    let mut session = Session::default();
    session.execute("file.newSequence", json!({"width": 16, "height": 16})).unwrap();
    let mut app = FilmcraftApp::new(session);
    assert!(!app.transport_running(), "nothing runs in a new project");
    app.playback.playing = true;
    assert!(app.transport_running(), "Program playback runs a transport");
    app.playback.playing = false;
    app.source_playback.clock.playing = true;
    assert!(app.transport_running(), "Source playback runs one");
    app.source_playback.clock.playing = false;
    app.playback.preroll = Some(0.0);
    assert!(app.transport_running(), "a preroll runs one");
    app.playback.preroll = None;
    assert!(!app.transport_running());
    app.playback.audio_clock = true;
    assert!(app.transport_running(), "an output a panicked frame left playing is stopped too");
    app.playback.audio_clock = false;
    assert!(!app.transport_running());
    // A recording pass runs a transport too, and `stop()` ends it.
    let seq = app.session.state.active_sequence.unwrap();
    app.session.mixrec.seq = Some(seq);
    assert!(app.transport_running(), "an automation pass runs one");
    app.session.mixrec.seq = None;
    app.session.mcrec.seq = Some(seq);
    assert!(app.transport_running(), "a multicam pass runs one");
    app.session.mcrec.seq = None;
    app.session.voiceover.rec = Some(filmcraft_engine::voiceover::Recording {
        seq,
        track: filmcraft_project::TrackId(0),
        record_start: filmcraft_time::Tick::ZERO,
        capture_start: filmcraft_time::Tick::ZERO,
        punch_out: None,
        format: filmcraft_engine::voiceover::InputFormat { sample_rate: 48_000, channels: 1 },
        channel: 0,
        epoch: app.session.document_epoch,
    });
    assert!(app.transport_running(), "a voice-over take runs one");
    app.session.voiceover.rec = None;
    assert!(!app.transport_running());
}

/// A Text to Speech preview plays without `playing`; a seek stops it, while a seek with nothing
/// sounding leaves the output (and the scrub stream on it) alone.
#[test]
fn a_scrub_stops_a_text_to_speech_preview_but_not_an_idle_output() {
    struct Counting(Arc<AtomicUsize>);
    impl AudioOut for Counting {
        fn sample_rate(&self) -> u32 {
            48000
        }
        fn channels(&self) -> usize {
            2
        }
        fn configure(&mut self, _: &filmcraft_engine::settings::AudioHardwarePrefs, _: Option<u32>) {}
        fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
            Ok(48000)
        }
        fn stop(&mut self) {
            self.0.fetch_add(1, Ordering::Relaxed);
        }
        fn played_frames(&self) -> Option<u64> {
            None
        }
    }
    let mut session = Session::default();
    session.execute("file.newSequence", json!({"width":16,"height":16})).unwrap();
    session.tts_preview = Some(Arc::new(filmcraft_tts::Audio { samples: vec![0.0; 4800], sample_rate: 48_000 }));
    let mut app = FilmcraftApp::new(session);
    let stops = Arc::new(AtomicUsize::new(0));
    app.audio = Some(Box::new(Counting(stops.clone())));
    app.stop_for_scrub();
    assert_eq!(stops.load(Ordering::Relaxed), 0, "nothing sounds: the scrub stream is left open");
    app.play_tts_preview().unwrap();
    let before = stops.load(Ordering::Relaxed);
    app.stop_for_scrub();
    assert_eq!(stops.load(Ordering::Relaxed), before + 1, "the preview ends when the playhead moves");
    app.stop_for_scrub();
    assert_eq!(stops.load(Ordering::Relaxed), before + 1, "and a second seek has nothing left to stop");
}

/// What a recording scrub output was asked to do, next to the playback output's starts.
#[derive(Clone, Default)]
struct ScrubLog {
    rates: Arc<std::sync::Mutex<Vec<Option<u32>>>>,
    events: Arc<std::sync::Mutex<Vec<&'static str>>>,
}

struct RecordingScrubOut(ScrubLog);

impl crate::scrub::ScrubOut for RecordingScrubOut {
    fn sample_rate(&self) -> u32 {
        48_000
    }
    fn play(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) {
        self.0.events.lock().unwrap().push("scrub play");
    }
    fn stop(&mut self) {
        self.0.events.lock().unwrap().push("scrub stop");
    }
    fn release(&mut self) {
        self.0.events.lock().unwrap().push("scrub release");
    }
    fn configure(&mut self, _: &filmcraft_engine::settings::AudioHardwarePrefs, document_rate: Option<u32>) {
        self.0.rates.lock().unwrap().push(document_rate);
    }
}

/// A playback output that logs its starts into the scrub output's log.
struct LoggedDevice(Arc<std::sync::Mutex<Vec<&'static str>>>);

impl AudioOut for LoggedDevice {
    fn sample_rate(&self) -> u32 {
        48000
    }
    fn channels(&self) -> usize {
        2
    }
    fn start(&mut self, _: Box<dyn FnMut(&mut [f32], usize) + Send>) -> Result<u32, String> {
        self.0.lock().unwrap().push("playback start");
        Ok(48000)
    }
    fn stop(&mut self) {}
    fn played_frames(&self) -> Option<u64> {
        Some(0)
    }
}

/// #2633: the scrub output follows the sequence's sample rate, not only the audio settings.
/// Settings ▸ Sequence (and opening another project or sequence) changes it without touching
/// them, and `apply_prefs` returns early then, so the output kept the rate it was built with.
#[test]
fn the_scrub_output_follows_the_sequence_sample_rate() {
    let mut session = Session::default();
    session.execute("file.newSequence", json!({"width": 16, "height": 16})).unwrap();
    let ctx = egui::Context::default();
    let mut app = FilmcraftApp::new(session);
    let log = ScrubLog::default();
    app.scrub_out = Some(Box::new(RecordingScrubOut(log.clone())));
    app.apply_prefs(&ctx);
    let first = app.session.active_sequence().unwrap().settings.sample_rate;
    assert_eq!(*log.rates.lock().unwrap(), vec![Some(first)], "the first frame configures it");
    app.apply_prefs(&ctx);
    assert_eq!(log.rates.lock().unwrap().len(), 1, "the same rate is not configured again");
    app.session.execute("sequence.settings", json!({"sampleRate": 44100})).unwrap();
    app.apply_prefs(&ctx);
    assert_eq!(*log.rates.lock().unwrap(), vec![Some(first), Some(44_100)], "the sequence's new rate reaches it");
}

/// #2633: the scrub stream lingers for a few seconds after the last grain, so playback gives its
/// device back before opening its own stream: a device that takes one stream at a time (a raw ALSA
/// `hw:` device, an exclusive-mode output) would refuse playback's otherwise.
#[test]
fn playback_gives_the_scrub_device_back_before_it_opens_its_stream() {
    let mut session = Session::default();
    session.execute("file.newSequence", json!({"width": 16, "height": 16})).unwrap();
    let mut app = FilmcraftApp::new(session);
    app.session.prefs.audio.scrub_audio = true;
    let log = ScrubLog::default();
    app.scrub_out = Some(Box::new(RecordingScrubOut(log.clone())));
    app.audio = Some(Box::new(LoggedDevice(log.events.clone())));
    // A scrub: the playhead moves under a held button, and a grain plays on the scrub output.
    app.scrub_tick(0.0, true);
    app.session.set_playhead(filmcraft_time::Tick::from_seconds_f64(2.0));
    assert!(app.scrub_tick(0.02, true), "the scrub stream is open");
    app.play(1.0);
    app.end_preroll(0.0);
    let events = log.events.lock().unwrap().clone();
    let pos = |e: &str| events.iter().position(|x| *x == e);
    let (Some(release), Some(start)) = (pos("scrub release"), pos("playback start")) else {
        panic!("expected a release and a playback start: {events:?}");
    };
    assert!(pos("scrub play").is_some_and(|p| p < release), "{events:?}");
    assert!(release < start, "the scrub device is given back before playback opens: {events:?}");
    assert!(!app.scrub.is_open(), "the scrub stream counts as closed");
    app.stop();
}

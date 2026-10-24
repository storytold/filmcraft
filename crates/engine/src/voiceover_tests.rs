//! Tests of [`crate::voiceover`]: recording with the synthetic input lands sample-accurately.

use super::*;
use crate::voiceover::{Synthetic, SyntheticInput, cue_times};
use filmcraft_time::TICKS_PER_SECOND;
use serde_json::json;

fn sec(x: f64) -> i64 {
    (x * TICKS_PER_SECOND as f64) as i64
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("filmcraft-vo-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d.to_string_lossy().into_owned()
}

/// The demo project plus an empty audio track (the record track, returned as `"A<n>"` with its
/// index); every other audio track is muted so only the recording is heard.
fn demo() -> (Session, String, usize) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("sequence.addTracks", json!({"video": 0, "audio": 1})).unwrap();
    let n = s.active_sequence().unwrap().audio_tracks.len();
    for i in 1..n {
        s.execute("mixer.setStrip", json!({"strip": format!("A{i}"), "muted": true})).unwrap();
    }
    (s, format!("A{n}"), n - 1)
}

fn mix(s: &Session, start: i64, n: usize) -> Vec<Vec<f32>> {
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    filmcraft_render::audio::mix_sequence(&s.project, s.active_sequence().unwrap(), start, n, &provider).channels
}

fn track_clips(s: &Session, i: usize) -> Vec<filmcraft_project::TrackItem> {
    s.active_sequence().unwrap().audio_tracks[i].items.clone()
}

#[test]
fn a_failed_recording_save_keeps_the_take_for_retry() {
    let (mut s, track, index) = demo();
    s.execute("playhead.set", json!({"seconds":0})).unwrap();
    s.execute("audio.voiceover.start", json!({"track":track, "preroll":0})).unwrap();
    let dir = tmp("save-retry");
    std::fs::write(&dir, b"not a directory").unwrap();
    assert!(s.execute("audio.voiceover.stop", json!({"time":sec(1.0),"dir":dir})).is_err());
    assert!(s.voiceover.recording(), "failed save must retain the recording for retry");
    assert!(s.execute("audio.voiceover.sync", json!({"time":sec(1.5)})).is_err(), "playback must not discard an unsaved take");
    std::fs::remove_file(&dir).unwrap();
    let result = s.execute("audio.voiceover.stop", json!({"time":sec(2.0),"dir":dir})).unwrap();
    assert_eq!(result["samples"], 48000, "retry preserves the stopped take's end point");
    assert_eq!(result["placed"], true);
    assert!(!s.voiceover.recording());
    assert_eq!(track_clips(&s, index).len(), 1);
    let audio = mix(&s, 0, 48000);
    assert!(audio[0][0] > 0.49, "retry retained the first captured sample");
    std::fs::remove_dir_all(&dir).unwrap();
}

#[test]
fn hostile_record_points_and_preroll_are_bounded() {
    let (mut s, track, _) = demo();
    assert!(s.execute("audio.voiceover.start", json!({"track":track,"time":-1,"preroll":1e300})).is_err());
    assert!(!s.voiceover.recording());
    let result = s.execute("audio.voiceover.start", json!({"track":track,"time":sec(120.0),"preroll":1e300})).unwrap();
    assert_eq!(result["captureStart"], sec(60.0));
    assert!(result["cues"].as_array().unwrap().len() <= 61);
    s.execute("audio.voiceover.stop", json!({"discard":true})).unwrap();
}

#[test]
fn commands_are_registered_and_disabled_without_a_recording() {
    for id in ["audio.voiceover.settings", "audio.voiceover.start", "audio.voiceover.sync", "audio.voiceover.stop"] {
        let c = commands::find(id).unwrap_or_else(|| panic!("{id} missing"));
        assert!(!c.label.is_empty() && !c.params.is_empty());
    }
    let s = Session::default();
    assert!(!s.is_enabled("audio.voiceover.start"), "needs a sequence");
    assert!(!s.is_enabled("audio.voiceover.stop"), "nothing recording");
}

#[test]
fn settings_persist_in_preferences() {
    let mut s = Session::default();
    let v = s.execute("audio.voiceover.settings", json!({})).unwrap();
    assert_eq!(v["settings"]["name"], "Voice-over");
    assert_eq!(v["devices"][0], SyntheticInput::DEVICE);
    assert_eq!(v["channels"], 2);
    s.execute(
        "audio.voiceover.settings",
        json!({"name": "Narration", "inputChannel": 1, "countdownSoundCues": false, "prerollSeconds": 3.0, "postrollSeconds": 99.0}),
    )
    .unwrap();
    let vo = &s.prefs.voice_over;
    assert_eq!((vo.name.as_str(), vo.input_channel, vo.countdown_sound_cues, vo.preroll_seconds, vo.postroll_seconds), ("Narration", 1, false, 3.0, 60.0));
    assert!(s.execute("audio.voiceover.settings", json!({"name": "  "})).is_err());
    // round trip through the preferences JSON
    let j = serde_json::to_value(&s.prefs).unwrap();
    assert_eq!(j["voiceOver"]["name"], "Narration");
    let back: crate::autosave::Preferences = serde_json::from_value(j).unwrap();
    assert_eq!(back.voice_over, s.prefs.voice_over);
}

#[test]
fn recording_lands_sample_accurately_after_the_preroll() {
    let (mut s, rt, ti) = demo();
    let dir = tmp("accurate");
    s.execute("audio.voiceover.settings", json!({"prerollSeconds": 1.0})).unwrap();
    s.voiceover.input = Some(Box::new(SyntheticInput::clicks()));
    let r = s.execute("audio.voiceover.start", json!({"track": rt, "time": sec(2.0)})).unwrap();
    assert_eq!(r["recordStart"], sec(2.0));
    assert_eq!(r["captureStart"], sec(1.0));
    assert_eq!(r["track"], rt.as_str());
    assert_eq!(r["cues"], json!([sec(1.0), sec(2.0)]));
    assert!(!s.is_enabled("audio.voiceover.start"), "already recording");
    let n_undo = s.history.undo.len();
    let v = s.execute("audio.voiceover.stop", json!({"time": sec(3.5), "dir": dir})).unwrap();
    assert_eq!(v["samples"], 72_000, "1.5 s at 48 kHz: the pre-roll is not kept");
    assert_eq!(s.history.undo.len(), n_undo + 1, "import + placement = one undo step");
    assert_eq!(s.history.undo.last().unwrap().0, "Record Voice-over");
    let path = v["path"].as_str().unwrap();
    assert!(path.ends_with("Voice-over 1.wav"), "{path}");
    assert!(std::path::Path::new(path).exists());
    let clips = track_clips(&s, ti);
    let c = clips.iter().find(|c| c.id.0 == v["clip"].as_u64().unwrap()).unwrap();
    assert_eq!((c.start, c.duration, c.source_in), (Tick(sec(2.0)), Tick(sec(1.5)), Tick::ZERO));
    // clicks every 12 000 capture samples from the capture start (1 s = sample 48 000): in the mix
    // at 96 000 + k·12 000, nothing in between
    let m = mix(&s, 90_000, 84_000);
    for (i, &x) in m[0].iter().enumerate() {
        let t = 90_000 + i as i64;
        let want = if (96_000..168_000).contains(&t) && (t - 48_000) % 12_000 == 0 { 0.5 } else { 0.0 };
        assert!((x - want).abs() < 1e-6, "sample {t}: {x} vs {want}");
    }
    // undo / redo
    s.execute("edit.undo", json!({})).unwrap();
    assert!(track_clips(&s, ti).is_empty());
    assert!(s.project.items.values().all(|i| i.name != "Voice-over 1.wav"));
    s.execute("edit.redo", json!({})).unwrap();
    assert!(track_clips(&s, ti).iter().any(|c| c.start == Tick(sec(2.0)) && c.duration == Tick(sec(1.5))));
    // the next take gets the next number
    s.execute("audio.voiceover.start", json!({"track": rt, "time": sec(5.0)})).unwrap();
    let v2 = s.execute("audio.voiceover.stop", json!({"time": sec(6.0), "dir": dir})).unwrap();
    assert!(v2["path"].as_str().unwrap().ends_with("Voice-over 2.wav"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn punch_in_records_between_in_and_out_only() {
    let (mut s, rt, ti) = demo();
    let dir = tmp("punch");
    s.execute("markers.markIn", json!({"time": sec(1.0)})).unwrap();
    s.execute("markers.markOut", json!({"time": sec(2.0)})).unwrap();
    // a ramp so every sample is identifiable: value = capture sample / 1e6
    let ramp: Vec<f32> = (0..400_000).map(|i| i as f32 * 1e-6).collect();
    s.voiceover.input = Some(Box::new(SyntheticInput::new(Synthetic::Buffer(vec![ramp]))));
    let r = s.execute("audio.voiceover.start", json!({"track": rt, "time": sec(7.0), "preroll": 0.5})).unwrap();
    assert_eq!(r["recordStart"], sec(1.0), "punch-in at the In point");
    assert_eq!(r["punchOut"], sec(2.0));
    // playback ran on past the Out point (post-roll): the clip still ends at Out
    let v = s.execute("audio.voiceover.stop", json!({"time": sec(4.0), "dir": dir})).unwrap();
    assert_eq!(v["samples"], 48_000);
    let c = track_clips(&s, ti).into_iter().find(|c| c.id.0 == v["clip"].as_u64().unwrap()).unwrap();
    assert_eq!((c.start, c.end()), (Tick(sec(1.0)), Tick(sec(2.0))));
    // clip sample 0 = capture sample 24 000 (0.5 s pre-roll)
    let m = mix(&s, 48_000, 48_000);
    for (i, &x) in m[0].iter().enumerate().step_by(997) {
        assert!((x - (24_000 + i) as f32 * 1e-6).abs() < 1e-6, "sample {i}: {x}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn record_track_choice_sync_and_discard() {
    let (mut s, rt, ti) = demo();
    let dir = tmp("track");
    // the record-armed track wins over targeting
    s.execute("mixer.setStrip", json!({"strip": rt, "recordArm": true})).unwrap();
    let r = s.execute("audio.voiceover.start", json!({"time": sec(1.0), "preroll": 0.0})).unwrap();
    assert_eq!(r["track"], rt.as_str());
    assert_eq!(r["cues"], json!([sec(1.0)]));
    // the host's playback started late: the capture restarts there
    let y = s.execute("audio.voiceover.sync", json!({"time": sec(1.0)})).unwrap();
    assert_eq!(y["captureStart"], sec(1.0));
    let v = s.execute("audio.voiceover.stop", json!({"time": sec(2.0), "dir": dir, "discard": true})).unwrap();
    assert_eq!(v["placed"], false);
    assert!(!s.voiceover.recording());
    // stopping at (or before) the record point places nothing
    s.execute("audio.voiceover.start", json!({"time": sec(3.0)})).unwrap();
    let v = s.execute("audio.voiceover.stop", json!({"time": sec(3.0), "dir": dir})).unwrap();
    assert_eq!(v["placed"], false);
    // unknown and locked tracks
    assert!(s.execute("audio.voiceover.start", json!({"track": "A99"})).is_err());
    assert!(s.execute("audio.voiceover.start", json!({"track": "V1"})).is_err());
    let a2 = s.active_sequence().unwrap().audio_tracks[ti].id.0;
    s.execute("timeline.setTrack", json!({"track": a2, "locked": true})).unwrap();
    assert!(s.execute("audio.voiceover.start", json!({"track": rt})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cue_times_count_down_whole_seconds() {
    let t = |x: f64| Tick(sec(x));
    assert_eq!(cue_times(t(0.5), t(3.5)), vec![t(0.5), t(1.5), t(2.5), t(3.5)]);
    assert_eq!(cue_times(t(1.2), t(3.0)), vec![t(2.0), t(3.0)]);
    assert_eq!(cue_times(t(3.0), t(3.0)), vec![t(3.0)]);
    let tone = crate::voiceover::cue_tone(48_000);
    assert_eq!(tone.len(), 4_800);
    assert!(tone.iter().all(|x| x.abs() <= 0.25) && tone[0] == 0.0);
}

/// The synthetic input, reporting fixed peaks to the input meter.
struct PeakInput(SyntheticInput);

impl crate::voiceover::AudioInput for PeakInput {
    fn devices(&self) -> Vec<String> {
        self.0.devices()
    }
    fn channels(&self, device: &str) -> u16 {
        self.0.channels(device)
    }
    fn start(&mut self, device: &str, sample_rate: u32) -> std::result::Result<crate::voiceover::InputFormat, String> {
        self.0.start(device, sample_rate)
    }
    fn read(&mut self, frames: usize) -> Vec<Vec<f32>> {
        self.0.read(frames)
    }
    fn take_peaks(&mut self) -> Vec<f32> {
        vec![0.5, 0.25]
    }
    fn discard(&mut self) {
        self.0.discard()
    }
    fn stop(&mut self) {
        self.0.stop()
    }
}

#[test]
fn the_input_meter_follows_the_input_while_recording() {
    let (mut s, track, _) = demo();
    s.voiceover.input = Some(Box::new(PeakInput(SyntheticInput::clicks())));
    crate::voiceover::post_input_meter(&mut s);
    assert!(s.previews.live.take_input_meter().is_empty(), "not recording: the meter stays idle");
    s.execute("audio.voiceover.start", json!({"track": track, "preroll": 0})).unwrap();
    crate::voiceover::post_input_meter(&mut s);
    assert_eq!(s.previews.live.take_input_meter(), [0.5, 0.25]);
    let dir = tmp("input-meter");
    s.execute("audio.voiceover.stop", json!({"time": sec(1.0), "dir": dir, "discard": true})).unwrap();
    crate::voiceover::post_input_meter(&mut s);
    assert!(s.previews.live.take_input_meter().is_empty(), "stopped: the meter stays idle");
    let _ = std::fs::remove_dir_all(&dir);
}

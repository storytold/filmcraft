use filmcraft_export::{ExportSettings, Format, Progress, SampleFormat};
use filmcraft_project::{ClipId, ItemId};
use filmcraft_time::Tick;
use serde_json::{Value, json};
use std::sync::Arc;

use crate::Session;

const SR: u32 = 48_000;
const QUERY: &str = "audio.measureLoudness";

fn session(mix: &str, track: &str) -> Session {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name":"Loudness", "mix": mix, "trackType": track, "fps":24, "sampleRate":SR, "video":1, "audio":2})).unwrap();
    s
}

fn tone(channels: u16) -> Vec<f32> {
    (0..4 * SR)
        .flat_map(|i| (0..channels).map(move |c| (0.15 * (std::f64::consts::TAU * (300.0 + 40.0 * c as f64) * i as f64 / SR as f64).sin()) as f32))
        .collect()
}

fn place(s: &mut Session, samples: &[f32], channels: u16, track: &str) -> (ItemId, ClipId) {
    let bytes = filmcraft_export::write_wav(samples, channels, SR, SampleFormat::Float32).unwrap();
    let item = crate::commands::import_bytes(s, &format!("/{track}.wav"), bytes.into(), None).unwrap();
    let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": track, "seconds": 0})).unwrap();
    (item, ClipId(r["clips"][0].as_u64().unwrap()))
}

fn close(actual: f64, expected: f64) {
    assert!((actual - expected).abs() <= 0.1, "{actual} is not within 0.1 of {expected}");
}

#[test]
fn loudness_untyped_mono_clip_matches_auto_match_without_an_edit() {
    let mut s = session("Stereo", "Standard");
    let samples = tone(1);
    let (_, clip) = place(&mut s, &samples, 1, "A1");
    s.execute("clip.audioGain", json!({"clips":[clip.0], "mode":"set", "db":6.0})).unwrap();
    s.execute("playhead.set", json!({"seconds":2.0})).unwrap();
    s.execute("mixer.setStrip", json!({"strip":"A1", "volumeDb":-12.0})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    let project = Arc::clone(&s.project);
    let dirty = s.is_dirty();
    let undo = s.undo_mark();
    let redo = s.history.redo.len();
    assert!(redo > 0);

    assert!(!crate::commands::find(QUERY).unwrap().journal);
    let query = s.execute(QUERY, json!({"of": "clips", "clips":[clip.0]})).unwrap();
    let read = &query["clips"][0];
    let integrated = read["integratedLufs"].as_f64().unwrap();
    let direct = filmcraft_audio_dsp::loudness::measure(&[&samples], SR as f64);
    close(integrated, direct.integrated_lufs + 6.0);
    assert!(read["truePeakDbtp"].is_number());
    assert_eq!(read["clip"], clip.0);
    assert_eq!(query["offlineMedia"], json!([]));
    assert!(Arc::ptr_eq(&s.project, &project));
    assert_eq!(s.is_dirty(), dirty);
    assert_eq!(s.undo_mark(), undo);
    assert_eq!(s.history.redo.len(), redo);
    assert_eq!(s.playhead(), Tick::from_seconds_f64(2.0));

    s.execute("essentialSound.setType", json!({"clips":[clip.0], "type":"dialogue"})).unwrap();
    let matched = s.execute("essentialSound.autoMatch", json!({"clips":[clip.0]})).unwrap();
    close(integrated, matched["clips"][0]["measuredLufs"].as_f64().unwrap());
}

#[test]
fn loudness_mix_matches_the_export_pass_and_track_has_its_own_strip() {
    for (layout, track, channels) in [("Stereo", "Standard", 2), ("5.1", "5.1", 6), ("Mono", "Mono", 1)] {
        let mut s = session(layout, track);
        place(&mut s, &tone(channels), channels, "A1");
        let seq = s.state.active_sequence.unwrap();
        let dry = s.execute(QUERY, json!({"of":"track", "track":"A1"})).unwrap();
        let out = dry["integratedLufs"].as_f64().unwrap();

        s.execute("mixer.setStrip", json!({"strip":"A1", "volumeDb":-6.0})).unwrap();
        s.execute("mixer.setStrip", json!({"strip":"Mix", "volumeDb":-3.0})).unwrap();
        let track_id = s.active_sequence().unwrap().audio_tracks[0].id.0;
        let after = s.execute(QUERY, json!({"of":"track", "track": track_id})).unwrap();
        close(after["integratedLufs"].as_f64().unwrap(), out - 6.0);

        let measured = s.execute(QUERY, json!({})).unwrap();
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("normalized.wav").to_string_lossy().into_owned();
        let mut settings = ExportSettings { format: Format::Wav, path, ..Default::default() };
        settings.audio.channels = channels.into();
        settings.effects.loudness.enabled = true;
        let progress = Progress::default();
        let provider = s.media.full_res_provider(s.project.clone(), s.services.clone()).own_sources();
        filmcraft_export::export(&s.project, seq, &settings, &provider, &progress).unwrap();
        assert_eq!(measured["of"], "mix");
        assert_eq!(measured["seconds"], 4.0);
        close(measured["integratedLufs"].as_f64().unwrap(), progress.loudness.lock().unwrap().unwrap().measured_lufs);
    }
}

#[test]
fn loudness_track_stops_before_the_destination_bus_and_clip_ranges_are_intersected() {
    let mut s = session("Stereo", "Standard");
    let (_, clip) = place(&mut s, &tone(2), 2, "A1");
    let baseline = s.execute(QUERY, json!({"of":"track","track":"A1"})).unwrap()["integratedLufs"].as_f64().unwrap();
    s.execute("mixer.addSubmix", json!({"name":"Bus"})).unwrap();
    s.execute("mixer.setStrip", json!({"strip":"A1", "output":"S1", "pan":-100.0})).unwrap();
    s.execute("mixer.setStrip", json!({"strip":"S1", "volumeDb":-18.0})).unwrap();
    s.execute("mixer.addInsert", json!({"strip":"A1", "effect":"amplify", "postFader":true})).unwrap();
    s.execute("mixer.setInsert", json!({"strip":"A1", "slot":0, "params":{"gain":6.0}})).unwrap();
    let track = s.execute(QUERY, json!({"of":"track","track":"A1"})).unwrap()["integratedLufs"].as_f64().unwrap();
    let mix = s.execute(QUERY, json!({})).unwrap()["integratedLufs"].as_f64().unwrap();
    assert!(track > baseline, "the 6 dB insert outweighs muting one side");
    close(mix, track - 18.0);

    let v = s.execute(QUERY, json!({"of":"clips","clips":[clip.0],"startSeconds":3, "endSeconds":5})).unwrap();
    let c = &v["clips"][0];
    assert_eq!(c["start"], Tick::from_seconds_f64(3.0).0);
    assert_eq!(c["end"], Tick::from_seconds_f64(4.0).0);
    assert_eq!(c["seconds"], 1.0);
    assert!(c["integratedLufs"].is_number());

    let v = s.execute(QUERY, json!({"of":"clips","clips":[clip.0],"startSeconds":5, "endSeconds":6})).unwrap();
    assert_eq!(v["clips"][0]["seconds"], 0.0);
    assert_eq!(v["clips"][0]["integratedLufs"], Value::Null);
}

#[test]
fn loudness_mix_does_not_reuse_another_projects_effect_tail() {
    assert_measurement_has_own_effect_state("mix");
}

#[test]
fn loudness_track_does_not_reuse_another_projects_effect_tail() {
    assert_measurement_has_own_effect_state("track");
}

#[test]
fn loudness_clips_do_not_reuse_another_projects_effect_tail() {
    assert_measurement_has_own_effect_state("clips");
}

fn assert_measurement_has_own_effect_state(target: &str) {
    // Two projects assign the same track and clip IDs to different source media.
    let setup = |samples: &[f32]| {
        let mut s = session("Stereo", "Standard");
        let (_, clip) = place(&mut s, samples, 1, "A1");
        if target == "clips" {
            s.execute("effects.apply", json!({"effect":"studio_reverb", "clips":[clip.0]})).unwrap();
        } else {
            s.execute("mixer.addInsert", json!({"strip":"A1", "effect":"studio_reverb"})).unwrap();
        }
        (s, clip)
    };
    let samples = tone(1);
    let (mut audible, first_clip) = setup(&samples);
    let (mut silent, second_clip) = setup(&vec![0.0; samples.len()]);
    assert_eq!(first_clip, second_clip);
    let measure = |session: &mut Session, clip: ClipId, start: u32, end: u32| {
        let report = session.execute(QUERY, json!({"of":target, "track":"A1", "clips":[clip.0], "startSeconds":start, "endSeconds":end})).unwrap();
        report["clips"].get(0).unwrap_or(&report)["samplePeakDbfs"].clone()
    };
    assert!(measure(&mut audible, first_clip, 0, 1).is_number());
    assert_eq!(measure(&mut silent, second_clip, 1, 2), Value::Null);
}

#[test]
fn loudness_ranges_and_offline_media_use_the_requested_sequence() {
    let mut s = session("Stereo", "Standard");
    let (item, clip) = place(&mut s, &tone(1), 1, "A1");
    let seq = s.state.active_sequence.unwrap().0;
    let from = Tick::from_seconds_f64(1.0).0;
    let to = Tick::from_seconds_f64(2.0).0;
    s.execute("markers.markIn", json!({"target":"program", "frame":24})).unwrap();
    s.execute("markers.markOut", json!({"target":"program", "frame":47})).unwrap();
    s.execute("sequence.setWorkArea", json!({"start":from, "end":to})).unwrap();
    s.execute("file.newSequence", json!({"name":"Other", "fps":30})).unwrap();

    for range in ["inOut", "workArea", "custom"] {
        let v = s.execute(QUERY, json!({"sequence":seq, "of":"track", "track":"A1", "range":range, "startSeconds":1, "endSeconds":2})).unwrap();
        assert_eq!(v["start"], from);
        assert_eq!(v["end"], to);
        assert_eq!(v["seconds"], 1.0);
        assert!(v["integratedLufs"].is_number());
    }
    assert_eq!(s.execute(QUERY, json!({"sequence":seq})).unwrap()["seconds"], 4.0);

    s.execute("media.makeOffline", json!({"items":[item.0]})).unwrap();
    assert_eq!(s.execute(QUERY, json!({"sequence":seq, "of":"track", "track":"A2"})).unwrap()["offlineMedia"], json!([]));
    for p in [json!({"sequence":seq}), json!({"sequence":seq, "of":"track", "track":"A1"}), json!({"sequence":seq, "of":"clips", "clips":[clip.0]})] {
        let v = s.execute(QUERY, p).unwrap();
        let fields = v["clips"].get(0).unwrap_or(&v);
        for name in ["integratedLufs", "loudnessRangeLu", "maxMomentaryLufs", "maxShortTermLufs", "samplePeakDbfs", "truePeakDbtp"] {
            assert_eq!(fields.get(name), Some(&Value::Null), "{name}");
        }
        assert_eq!(v["offlineMedia"].as_array().unwrap().len(), 1);
        assert_eq!(v["offlineMedia"][0]["item"], item.0);
        assert_eq!(v["offlineMedia"][0]["status"], "offline");
    }
}

#[test]
fn loudness_reports_failed_audio_reads_as_unreadable() {
    use filmcraft_frame::{AudioBuffer, VideoFrame};
    use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource};

    struct FailingAudio(MediaInfo);

    impl MediaSource for FailingAudio {
        fn info(&self) -> &MediaInfo {
            &self.0
        }

        fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
            Err(MediaError::NoStream("video"))
        }

        fn audio(&self, _: i64, _: usize, _: u32) -> filmcraft_media::Result<AudioBuffer> {
            Err(MediaError::Decode("synthetic read failure".into()))
        }
    }

    let mut s = session("Stereo", "Standard");
    let samples = tone(1);
    place(&mut s, &samples, 1, "A1");
    let (item, clip) = place(&mut s, &samples, 1, "A2");
    let original = s.media.source_for(&s.project, item, &*s.services).unwrap();
    s.media.insert(item, Arc::new(FailingAudio(original.info().clone())));

    for target in ["mix", "track", "clips"] {
        let v = s.execute(QUERY, json!({"of":target,"track":"A2","clips":[clip.0]})).unwrap();
        let report = v["clips"].get(0).unwrap_or(&v);
        assert_eq!(report["samplePeakDbfs"].is_number(), target == "mix");
        let offline = v["offlineMedia"].as_array().unwrap();
        assert_eq!(offline.len(), 1, "{target}: {v}");
        assert_eq!(offline[0]["item"], item.0);
        assert_eq!(offline[0]["status"], "unreadable");
        assert_eq!(offline[0]["path"], "/A2.wav");
        assert_eq!(offline[0]["error"], "decode error: synthetic read failure");
    }

    assert!(s.media.offline_status(item).is_none());
    s.media.insert(item, original);
    let fixed = s.execute(QUERY, json!({"of":"track","track":"A2"})).unwrap();
    assert!(fixed["integratedLufs"].is_number());
    assert_eq!(fixed["offlineMedia"], json!([]));
}

#[test]
fn loudness_rejects_invalid_inputs() {
    let mut s = session("Stereo", "Standard");
    for params in [
        json!([1, 2]),
        json!({"sequence":"invalid"}),
        json!({"of":12}),
        json!({"of":"unknown"}),
        json!({"of":"track"}),
        json!({"of":"track", "track":"V1"}),
        json!({"of":"clips", "clips":[]}),
        json!({"of":"clips", "clips":[3.5]}),
        json!({"of":"clips", "clips":[999]}),
        json!({"range":"workArea"}),
        json!({"range":"custom", "startSeconds":1.0}),
        json!({"startSeconds":1.0}),
        json!({"endSeconds":1.0}),
        json!({"range":"custom", "startSeconds":-1.0, "endSeconds":1.0}),
        json!({"range":"custom", "startSeconds":0, "endSeconds":1e60}),
    ] {
        assert!(s.execute(QUERY, params.clone()).is_err(), "accepted {params}");
    }
}

#[test]
fn loudness_clip_and_auto_match_measure_the_same_enabled_proxy() {
    let mut s = session("Stereo", "Standard");
    let temp = tempfile::tempdir().unwrap();
    let original_path = temp.path().join("Original.wav");
    let proxy_path = temp.path().join("Proxy.wav");
    let original = tone(1);
    let proxy: Vec<_> = original.iter().map(|v| v * 0.25).collect();
    for (path, samples) in [(&original_path, original), (&proxy_path, proxy)] {
        std::fs::write(path, filmcraft_export::write_wav(&samples, 1, SR, SampleFormat::Float32).unwrap()).unwrap();
    }

    let media = s.execute("file.import", json!({"paths":[original_path]})).unwrap()["items"][0].as_u64().unwrap();
    let clip = s.execute("timeline.place", json!({"item":media,"audioTrack":"A1","seconds":0})).unwrap()["clips"][0].as_u64().unwrap();
    let original_mix = s.execute(QUERY, json!({})).unwrap()["integratedLufs"].as_f64().unwrap();
    s.execute("media.attachProxies", json!({"item":media,"path":proxy_path})).unwrap();
    s.execute("media.toggleProxies", json!({"enabled":true})).unwrap();
    s.execute("essentialSound.setType", json!({"clips":[clip],"type":"dialogue"})).unwrap();

    let query = s.execute(QUERY, json!({"of":"clips","clips":[clip]})).unwrap();
    let matched = s.execute("essentialSound.autoMatch", json!({"clips":[clip]})).unwrap();
    close(query["clips"][0]["integratedLufs"].as_f64().unwrap(), matched["clips"][0]["measuredLufs"].as_f64().unwrap());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.media.use_proxies());
    close(s.execute(QUERY, json!({})).unwrap()["integratedLufs"].as_f64().unwrap(), original_mix);
}

#[test]
fn loudness_pending_clip_keeps_a_later_decode_failure_visible() {
    use filmcraft_frame::{AudioBuffer, VideoFrame};
    use filmcraft_media::{FrameRequest, MediaError, MediaInfo, MediaSource, pending};

    struct FailingSource {
        info: MediaInfo,
        deferred: bool,
    }

    impl MediaSource for FailingSource {
        fn info(&self) -> &MediaInfo {
            &self.info
        }

        fn video_frame(&self, _: FrameRequest) -> filmcraft_media::Result<Arc<VideoFrame>> {
            Err(MediaError::NoStream("video"))
        }

        fn audio(&self, _: i64, _: usize, _: u32) -> filmcraft_media::Result<AudioBuffer> {
            if self.deferred {
                return Err(MediaError::Io(pending::would_block().to_string()));
            }
            Err(MediaError::Decode("synthetic independent audio failure".into()))
        }
    }

    let mut s = session("Stereo", "Standard");
    let audio = tone(1);
    let (waiting_item, waiting_clip) = place(&mut s, &audio, 1, "A1");
    let (broken_item, broken_clip) = place(&mut s, &audio, 1, "A2");
    let second_broken = s.execute("timeline.place", json!({"item":broken_item.0,"audioTrack":"A1","seconds":2})).unwrap()["clips"][0].as_u64().unwrap();
    for (id, deferred) in [(waiting_item, true), (broken_item, false)] {
        let info = s.media.source_for(&s.project, id, &*s.services).unwrap().info().clone();
        s.media.insert(id, Arc::new(FailingSource { info, deferred }));
    }
    for target in ["mix", "track", "clips"] {
        let _ = pending::take();
        let report = s.execute(QUERY, json!({"of":target,"track":"A1","clips":[waiting_clip.0,broken_clip.0,second_broken]})).unwrap();
        assert!(pending::take(), "{target}: the first source reported a deferred read");
        let errors = report["offlineMedia"].as_array().unwrap();
        assert_eq!(errors.len(), 1, "{target}: {report}");
        assert_eq!(errors[0]["item"], broken_item.0);
        assert_eq!(errors[0]["status"], "unreadable");
        assert_eq!(errors[0]["error"], "decode error: synthetic independent audio failure");
    }
    assert!(s.media.offline_status(waiting_item).is_none());
    assert!(s.media.offline_status(broken_item).is_none());
}

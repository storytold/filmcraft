//! Tests of [`crate::remix`]: Clip ▸ Remix on generated rhythmic music (120 BPM, 16 bars, 32 s).

use std::sync::Arc;

use filmcraft_audio_dsp::remix as dsp;
use filmcraft_project::{ClipId, TrackItem};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use super::*;

const SR: u32 = 48_000;

/// Generated music (stereo planar) and its analysis.
fn music() -> (Vec<Vec<f32>>, dsp::Analysis) {
    let (m, _) = dsp::test_music(SR, 120.0, 16, 3);
    let refs: Vec<&[f32]> = m.iter().map(Vec::as_slice).collect();
    let a = dsp::analyze(&refs, SR).unwrap();
    (m, a)
}

/// A session with `music` on A1 at 0 s (and optionally a second clip at `next` s), the music
/// clip selected.
fn session(m: &[Vec<f32>], next: Option<f64>) -> (Session, ClipId) {
    let mut s = Session::default();
    s.execute("file.newSequence", json!({"name": "remix", "audio": 2, "video": 1})).unwrap();
    let inter: Vec<f32> = (0..m[0].len()).flat_map(|i| [m[0][i], m[1][i]]).collect();
    let bytes: Arc<[u8]> = crate::previews::write_wav_f32(&inter, SR).into();
    let item = crate::commands::import_bytes(&mut s, "/music.wav", bytes, None).unwrap();
    let r = s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": 0.0})).unwrap();
    let c = ClipId(r["clips"][0].as_u64().unwrap());
    if let Some(at) = next {
        s.execute("timeline.place", json!({"item": item.0, "audioTrack": "A1", "seconds": at})).unwrap();
    }
    s.execute("timeline.select", json!({"clips": [c.0]})).unwrap();
    (s, c)
}

fn item(s: &Session, c: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(c).unwrap().1.clone()
}

fn beat() -> Tick {
    Tick::from_seconds_f64(0.5)
}

/// Every cut of the report lies on a detected beat of the music.
fn assert_cuts_on_beats(r: &Value, a: &dsp::Analysis) {
    for cut in r["cuts"].as_array().unwrap() {
        for k in ["out", "in"] {
            let t = Tick(cut[k].as_i64().unwrap());
            let smp = t.to_units_floor(SR as i64);
            assert!(a.beats.contains(&smp), "cut {k} at sample {smp} is not a detected beat");
        }
    }
}

#[test]
fn menu_commands_and_disabled_reasons() {
    let (m, _) = music();
    let (mut s, c) = session(&m, None);
    let pos = |id: &str| crate::commands::command_specs().iter().position(|c| c.id == id).unwrap();
    assert!(pos("clip.remix.enable") < pos("clip.remix.properties") && pos("clip.remix.properties") < pos("clip.remix.revert"));
    let spec = crate::commands::find("clip.remix.enable").unwrap();
    assert_eq!(spec.menu, &["Clip", "Remix"]);
    // not remixed yet: properties / revert are disabled
    assert!(s.execute("clip.remix.revert", json!({})).is_err());
    assert!(s.execute("clip.remix.properties", json!({})).is_err());
    // nothing selected: enable is disabled with a reason
    s.execute("edit.deselectAll", json!({})).unwrap();
    let why = (crate::commands::find("clip.remix.enable").unwrap().enabled)(&s).unwrap_err();
    assert!(why.contains("audio clip"), "{why}");
    s.execute("timeline.select", json!({"clips": [c.0]})).unwrap();
    assert!((spec.enabled)(&s).is_ok());
    // agents must say what duration they want
    assert!(s.execute("clip.remix", json!({"clip": c.0})).is_err());
}

#[test]
fn remix_shorter_and_longer_hits_target_at_beats_with_undo() {
    let (m, a) = music();
    let (mut s, c) = session(&m, None);
    let before = item(&s, c);
    let r = s.execute("clip.remix.enable", json!({})).unwrap();
    assert_eq!(r["remixed"], true);
    let enabled = item(&s, c);
    assert!((before.duration - enabled.duration).abs() < Tick::from_units(1, SR as i64), "enable keeps the duration");
    let n0 = s.history.undo.len();
    for secs in [12.0, 20.0, 26.5, 45.0, 70.0] {
        let r = s.execute("clip.remix", json!({"seconds": secs})).unwrap();
        let d = item(&s, c).duration;
        let err = (d - Tick::from_seconds_f64(secs)).abs();
        assert!(err <= beat(), "{secs} s: off by {:.3} s", err.seconds());
        assert_eq!(Tick(r["duration"].as_i64().unwrap()), d);
        assert_cuts_on_beats(&r, &a);
        assert!(!r["cuts"].as_array().unwrap().is_empty());
        eprintln!("remix to {secs} s: {:.4} s, {} cuts, {:.1} BPM", d.seconds(), r["cuts"].as_array().unwrap().len(), r["bpm"].as_f64().unwrap());
    }
    assert_eq!(s.history.undo.len(), n0 + 5, "one undo step per remix");
    // properties: read and change the sliders (keeps the target)
    let p = s.execute("clip.remix.properties", json!({})).unwrap();
    assert_eq!(p["targetSeconds"].as_f64().unwrap(), 70.0);
    let few = s.execute("clip.remix.properties", json!({"segments": 0.0})).unwrap();
    let many = s.execute("clip.remix.properties", json!({"segments": 100.0})).unwrap();
    assert!(few["pieces"].as_array().unwrap().len() < many["pieces"].as_array().unwrap().len());
    // determinism
    let again = s.execute("clip.remix.properties", json!({"segments": 100.0})).unwrap();
    assert_eq!(again["pieces"], many["pieces"]);
    // undo / redo
    let cur = item(&s, c);
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(item(&s, c), cur);
    // revert restores the clip exactly
    s.execute("clip.remix.revert", json!({})).unwrap();
    assert_eq!(item(&s, c), before);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(item(&s, c), cur);
}

#[test]
fn mixed_audio_is_the_source_pieces() {
    let (m, a) = music();
    let (mut s, c) = session(&m, None);
    let r = s.execute("clip.remix", json!({"seconds": 18.0})).unwrap();
    assert_cuts_on_beats(&r, &a);
    let it = item(&s, c);
    let rm = filmcraft_render::remix::Remix::of(&it).unwrap();
    let plan = rm.plan_at(SR);
    let total = it.duration.to_units_floor(SR as i64) as usize;
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let q = s.active_sequence().unwrap();
    let mix = filmcraft_render::audio::mix_sequence(&s.project, q, 0, total, &provider);
    let mut d = 0usize;
    let mut checked = 0;
    let mut worst = 0f32;
    for p in &plan.pieces {
        for k in (plan.xfade as usize..p.len as usize - plan.xfade as usize).step_by(31) {
            let want = m[0][p.src as usize + k];
            worst = worst.max((mix.channels[0][d + k] - want).abs());
            checked += 1;
        }
        d += p.len as usize;
    }
    assert!(worst < 1e-5, "mix differs from the pieces by {worst}");
    assert!(checked > 10_000);
}

#[test]
fn overlaps_are_refused_and_noise_cannot_be_remixed() {
    let (m, _) = music();
    let (mut s, c) = session(&m, Some(40.0));
    let err = s.execute("clip.remix", json!({"seconds": 50.0})).unwrap_err().to_string();
    assert!(err.contains("room"), "{err}");
    assert!(s.execute("clip.remix", json!({"seconds": 39.0})).is_ok());
    let _ = c;
    // a steady tone has no beat
    let tone: Vec<f32> = (0..SR as usize * 20).map(|i| (i as f32 * 0.05).sin() * 0.3).collect();
    let (mut s, _) = session(&[tone.clone(), tone], None);
    let err = s.execute("clip.remix.enable", json!({})).unwrap_err().to_string();
    assert!(err.contains("beat") || err.contains("short"), "{err}");
}

#[test]
fn remix_analysis_never_reuses_another_sessions_item_id() {
    let (music, _) = music();
    let (mut first, c1) = session(&music, None);
    first.execute("clip.remix.enable", json!({"clip": c1.0})).unwrap();
    // Same item id, path, duration and sample rate; entirely different source bytes.
    let silence = vec![vec![0.0; music[0].len()]; 2];
    let (mut second, c2) = session(&silence, None);
    assert_eq!(c1, c2);
    let before = second.project.to_json();
    let error = second.execute("clip.remix.enable", json!({"clip": c2.0})).unwrap_err();
    assert!(error.to_string().contains("no beat"), "{error}");
    assert_eq!(second.project.to_json(), before);
    // The first session still has its own valid source and can re-plan.
    first.execute("clip.remix", json!({"clip": c1.0, "seconds": 20})).unwrap();
}

#[test]
fn detected_beats_are_editable_sequence_markers_with_one_undo_step() {
    let (music, _) = music();
    for reverse in [false, true] {
        let (mut s, c) = session(&music, None);
        s.edit_sequence("Prepare trimmed music", |q, _, _| {
            let it = q.find_item_mut(c).unwrap().1;
            it.start = Tick::from_units(5, 1);
            it.source_in = Tick::from_units(1, 1);
            it.duration = Tick::from_units(10, 1);
            it.speed = 2.0;
            it.reverse = reverse;
            Ok(())
        })
        .unwrap();
        let before = s.project.to_json();
        let clip_before = item(&s, c);
        let undo = s.history.undo.len();
        let r = s.execute("markers.detectBeats", json!({})).unwrap();
        let markers = s.active_sequence().unwrap().markers.clone();
        assert!(markers.len() >= 30, "a beat grid across the selected music: {r}");
        assert_eq!(markers.len(), r["count"].as_u64().unwrap() as usize);
        assert_eq!(item(&s, c), clip_before, "source range, effects and media remain untouched");
        assert_eq!(s.history.undo.len(), undo + 1);
        for marker in &markers {
            assert!(marker.start >= clip_before.start && marker.start < clip_before.end());
            assert_eq!(marker.duration, Tick::ZERO);
            // 120 BPM music at 200% speed: an attack every quarter second.
            let residual = (marker.start - clip_before.start).0.rem_euclid(Tick::from_units(1, 4).0);
            assert!(residual.min(Tick::from_units(1, 4).0 - residual) < Tick::from_units(40, 1000).0, "beat time {:?}", marker.start);
        }
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project.to_json(), before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.active_sequence().unwrap().markers, markers);
        let r = s.execute("markers.detectBeats", json!({"clip": c.0})).unwrap();
        assert_eq!(r["count"], 0, "repeat detection does not duplicate markers");
        assert_eq!(s.history.undo.len(), undo + 1);
        s.execute("markers.edit", json!({"marker": markers[0].id.0, "name": "Chorus", "comment": "Cut here"})).unwrap();
        let edited = s.active_sequence().unwrap().markers.iter().find(|m| m.id == markers[0].id).unwrap();
        assert_eq!((edited.name.as_str(), edited.comment.as_str()), ("Chorus", "Cut here"));
    }
}

#[test]
fn beat_detection_uses_the_clips_selected_audio_stream() {
    struct Streams {
        info: filmcraft_media::MediaInfo,
        music: Arc<dyn filmcraft_media::MediaSource>,
    }
    impl filmcraft_media::MediaSource for Streams {
        fn info(&self) -> &filmcraft_media::MediaInfo {
            &self.info
        }
        fn video_frame(&self, _: filmcraft_media::FrameRequest) -> filmcraft_media::Result<Arc<filmcraft_frame::VideoFrame>> {
            Err(filmcraft_media::MediaError::NoStream("video"))
        }
        fn audio(&self, start: i64, frames: usize, rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
            self.audio_stream(0, start, frames, rate)
        }
        fn audio_stream(&self, stream: usize, start: i64, frames: usize, rate: u32) -> filmcraft_media::Result<filmcraft_frame::AudioBuffer> {
            match stream {
                0 => Ok(filmcraft_frame::AudioBuffer { sample_rate: rate, channels: vec![vec![0.0; frames]; 2] }),
                1 => self.music.audio(start, frames, rate),
                _ => Err(filmcraft_media::MediaError::NoStream("audio")),
            }
        }
    }
    let (music, _) = music();
    let (mut s, c) = session(&music, None);
    let media = item(&s, c).item;
    let source = s.media.full_res_source(&s.project, media, &*s.services).unwrap();
    let mut info = source.info().clone();
    info.audio_streams.push(info.audio_streams[0].clone());
    Arc::make_mut(&mut s.project).item_mut(media).unwrap().as_media_mut().unwrap().info = info.clone();
    s.media.insert(media, Arc::new(Streams { info, music: source }));
    s.edit_sequence("Select music stream", |q, _, _| {
        q.find_item_mut(c).unwrap().1.audio_stream = 1;
        Ok(())
    })
    .unwrap();
    assert!(s.execute("markers.detectBeats", json!({})).unwrap()["count"].as_u64().unwrap() > 30);
    for stream in [0, usize::MAX] {
        s.edit_sequence("Select silent or invalid stream", |q, _, _| {
            q.find_item_mut(c).unwrap().1.audio_stream = stream;
            Ok(())
        })
        .unwrap();
        let before = s.project.to_json();
        assert!(s.execute("markers.detectBeats", json!({})).is_err());
        assert_eq!(s.project.to_json(), before);
    }
}

#[test]
fn beat_detection_rejects_bad_input_silence_and_unbounded_ranges_without_edits() {
    let silence = vec![vec![0.0; SR as usize * 10]; 2];
    let (mut s, c) = session(&silence, None);
    let before = s.project.to_json();
    for params in [
        json!(null),
        json!([]),
        json!({"clip": "wrong"}),
        json!({"clip": -1}),
        json!({"clip": 0}),
        json!({"clip": u64::MAX}),
        json!({"unknown": true}),
        json!({}),
    ] {
        assert!(s.execute("markers.detectBeats", params).is_err());
        assert_eq!(s.project.to_json(), before);
    }
    let media = item(&s, c).item;
    for channels in [0, u32::MAX] {
        Arc::make_mut(&mut s.project).item_mut(media).unwrap().as_media_mut().unwrap().info.audio_streams.first_mut().unwrap().channels = channels;
        let before = s.project.to_json();
        let undo = s.history.undo.len();
        let error = s.execute("markers.detectBeats", json!({})).unwrap_err();
        assert!(error.to_string().contains("memory budget"), "{error}");
        assert_eq!(s.project.to_json(), before);
        assert_eq!(s.history.undo.len(), undo);
    }
    Arc::make_mut(&mut s.project).item_mut(media).unwrap().as_media_mut().unwrap().info.audio_streams.first_mut().unwrap().channels = 2;
    // Bypass normal edit validation to exercise an already-corrupt project range.
    let seq = s.state.active_sequence.unwrap();
    Arc::make_mut(&mut s.project).sequence_mut(seq).unwrap().find_item_mut(c).unwrap().1.duration = Tick(i64::MAX);
    let before = s.project.to_json();
    let error = s.execute("markers.detectBeats", json!({"clip": c.0})).unwrap_err();
    assert!(error.to_string().contains("ten source minutes"), "{error}");
    assert_eq!(s.project.to_json(), before);
}

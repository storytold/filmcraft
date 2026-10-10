//! Tests of [`crate::narration`]: Text to Speech narrations are placed without overwriting, keep
//! their clip length when edited, and are one exact undo step.

use super::*;
use filmcraft_project::{ClipId, ItemId};
use serde_json::json;

const RATE: i64 = filmcraft_tts::SAMPLE_RATE as i64;

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("filmcraft-tts-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d.to_string_lossy().into_owned()
}

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// The 32-bit float samples of a WAV written by `write_wav_f32`.
fn wav_samples(path: &str) -> Vec<f32> {
    let b = std::fs::read(path).unwrap();
    b[44..].as_chunks::<4>().0.iter().map(|c| f32::from_le_bytes(*c)).collect()
}

fn clip(s: &Session, id: u64) -> filmcraft_project::TrackItem {
    s.active_sequence().unwrap().find_item(ClipId(id)).unwrap().1.clone()
}

fn all_clips(s: &Session) -> Vec<filmcraft_project::TrackItem> {
    s.active_sequence().unwrap().all_tracks().flat_map(|t| t.items.clone()).collect()
}

#[test]
fn voices_lists_the_built_in_voices() {
    let mut s = Session::default();
    let r = s.execute("tts.voices", json!({})).unwrap();
    let ids: Vec<&str> = r["voices"].as_array().unwrap().iter().map(|v| v["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"basic-female") && ids.contains(&"basic-male"), "{ids:?}");
    assert_eq!(r["languages"][0]["id"], "en-US");
    assert_eq!(r["pitches"].as_array().unwrap().len(), 5);
    assert_eq!(r["pauseMarker"], "[pause 1s]");
}

#[test]
fn create_places_the_narration_at_the_playhead_as_one_undo_step() {
    let mut s = demo();
    let dir = tmp("create");
    s.execute("playhead.set", json!({"seconds": 2.0})).unwrap();
    let t0 = s.playhead();
    assert!(t0 > Tick::ZERO);
    let before = all_clips(&s);
    let undo0 = s.history.undo.len();
    let r = s.execute("tts.create", json!({"text": "Welcome to FilmCraft.", "voice": "basic-male", "pitch": "low", "pace": 1.2, "dir": dir})).unwrap();
    let c = clip(&s, r["clip"].as_u64().unwrap());
    assert_eq!(c.start, t0);
    // the clip is exactly as long as the speech (sample-exact) and plays it from the start
    let samples = wav_samples(r["path"].as_str().unwrap());
    assert_eq!(c.duration, Tick::from_units(samples.len() as i64, RATE));
    assert_eq!(c.source_in, Tick::ZERO);
    assert_eq!(r["speechDuration"], c.duration.0);
    let n = s.project.narrations.get(&ItemId(r["item"].as_u64().unwrap())).unwrap();
    assert_eq!((n.text.as_str(), n.voice.as_str(), n.pitch, n.pace), ("Welcome to FilmCraft.", "basic-male", VocalPitch::Low, 1.2));
    assert_eq!(n.speech_duration, c.duration);
    assert_eq!(s.state.selection, vec![c.id]);
    // nothing that was there before moved or changed
    let after = all_clips(&s);
    for b in &before {
        assert!(after.contains(b), "clip {:?} changed", b.id);
    }
    assert_eq!(after.len(), before.len() + 1);
    // one undo step that removes the clip and the narration; redo brings both back
    assert_eq!(s.history.undo.len(), undo0 + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "New Narration");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(all_clips(&s), before);
    assert!(s.project.narrations.is_empty());
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(clip(&s, c.id.0), c);
    assert_eq!(s.project.narrations.len(), 1);
    let _ = std::fs::remove_dir_all(&dir);
}

use filmcraft_project::VocalPitch;
use filmcraft_time::Tick;

#[test]
fn create_never_overwrites_and_adds_a_track_when_every_track_is_busy() {
    let mut s = demo();
    let dir = tmp("busy");
    // fill every audio track over 0–60 s with a narration-length clip so none is free at 0
    let tracks = s.active_sequence().unwrap().audio_tracks.len();
    let first = s.execute("tts.create", json!({"text": "One two three.", "time": 0, "dir": dir})).unwrap();
    let busy_len = Tick(first["duration"].as_i64().unwrap());
    for _ in 1..tracks + 2 {
        s.execute("tts.create", json!({"text": "One two three.", "time": 0, "dir": dir})).unwrap();
    }
    // every create landed on its own track at 0: no two narrations overlap on one track
    let seq = s.active_sequence().unwrap();
    for t in &seq.audio_tracks {
        let mut items: Vec<_> = t.items.iter().map(|i| (i.start, i.end())).collect();
        items.sort();
        for w in items.windows(2) {
            assert!(w[0].1 <= w[1].0, "overlap on {:?}", t.name);
        }
    }
    assert!(seq.audio_tracks.len() > tracks, "a new audio track was added");
    assert!(busy_len > Tick::ZERO);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn create_on_an_occupied_or_unknown_track_is_refused_and_changes_nothing() {
    let mut s = demo();
    let dir = tmp("occupied");
    let r = s.execute("tts.create", json!({"text": "Hello.", "time": 0, "track": "A1", "dir": dir}));
    if s.active_sequence().unwrap().audio_tracks[0].items.iter().any(|i| i.start <= Tick::ZERO && Tick::ZERO < i.end()) {
        assert!(r.is_err(), "A1 is busy at 0");
    }
    let undo0 = s.history.undo.len();
    let items0 = s.project.items.len();
    assert!(s.execute("tts.create", json!({"text": "Hello.", "track": "A99", "dir": dir})).is_err());
    s.execute("timeline.setTrack", json!({"track": "A2", "locked": true})).unwrap();
    let undo1 = s.history.undo.len();
    assert!(s.execute("tts.create", json!({"text": "Hello.", "track": "A2", "time": 0, "dir": dir})).is_err());
    assert_eq!(s.history.undo.len(), undo1);
    assert!(undo1 >= undo0);
    assert_eq!(s.project.items.len(), items0, "no file was imported");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn editing_to_longer_speech_keeps_the_clip_length_and_the_whole_speech() {
    let mut s = demo();
    let dir = tmp("longer");
    let r = s.execute("tts.create", json!({"text": "Hi.", "time": 0, "dir": dir})).unwrap();
    let c0 = clip(&s, r["clip"].as_u64().unwrap());
    let undo0 = s.history.undo.len();
    let e = s.execute("tts.edit", json!({"clip": c0.id.0, "text": "Hi. This narration is now a great deal longer than it was before."})).unwrap();
    let c1 = clip(&s, c0.id.0);
    assert_eq!((c1.id, c1.start, c1.duration, c1.speed), (c0.id, c0.start, c0.duration, c0.speed), "position and length kept");
    assert_ne!(c1.item, c0.item, "a new file");
    assert_eq!(c1.source_in, Tick::ZERO);
    let n = s.project.narrations.get(&c1.item).unwrap();
    assert!(n.speech_duration > c1.duration, "the speech is longer than the clip");
    // the file holds the whole speech, nothing padded
    let samples = wav_samples(e["path"].as_str().unwrap());
    assert_eq!(Tick::from_units(samples.len() as i64, RATE), n.speech_duration);
    assert_eq!(e["fileDuration"], n.speech_duration.0);
    // the old item and its narration are untouched (other clips of it, undo)
    assert_eq!(s.project.narrations.get(&c0.item).unwrap().text, "Hi.");
    assert_eq!(s.history.undo.len(), undo0 + 1);
    assert_eq!(s.history.undo.last().unwrap().0, "Edit Narration");
    // the edited file sits next to the first one
    assert!(e["path"].as_str().unwrap().starts_with(&dir), "{}", e["path"]);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn editing_to_shorter_speech_pads_the_file_with_exact_silence() {
    let mut s = demo();
    let dir = tmp("shorter");
    let r = s.execute("tts.create", json!({"text": "This first version of the narration is fairly long, with several words.", "time": 0, "dir": dir})).unwrap();
    let id = r["clip"].as_u64().unwrap();
    let c0 = clip(&s, id);
    let e = s.execute("tts.edit", json!({"clip": id, "text": "Short."})).unwrap();
    let c1 = clip(&s, id);
    assert_eq!((c1.start, c1.duration), (c0.start, c0.duration));
    let n = s.project.narrations.get(&c1.item).unwrap().clone();
    assert!(n.speech_duration < c1.duration);
    let samples = wav_samples(e["path"].as_str().unwrap());
    let speech = n.speech_duration.to_units_floor(RATE) as usize;
    // the file covers the clip, and everything after the speech is exactly zero
    assert!(Tick::from_units(samples.len() as i64, RATE) >= c1.duration);
    assert!(samples.len() > speech);
    assert!(samples[speech..].iter().all(|&x| x == 0.0));
    assert!(samples[..speech].iter().any(|&x| x != 0.0));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn undo_and_redo_of_an_edit_are_exact() {
    let mut s = demo();
    let dir = tmp("undo");
    let r = s.execute("tts.create", json!({"text": "Before.", "time": 0, "dir": dir})).unwrap();
    let id = r["clip"].as_u64().unwrap();
    let before = (*s.project).clone();
    s.execute("tts.edit", json!({"clip": id, "text": "After.", "voice": "basic-male", "pitch": "extraHigh", "pace": 0.7})).unwrap();
    let after = (*s.project).clone();
    assert_ne!(before, after);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, before);
    let n = s.project.narrations.get(&clip(&s, id).item).unwrap();
    assert_eq!((n.text.as_str(), n.voice.as_str()), ("Before.", "basic-female"));
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, after);
    let n = s.project.narrations.get(&clip(&s, id).item).unwrap();
    assert_eq!((n.text.as_str(), n.pitch, n.pace), ("After.", VocalPitch::ExtraHigh, 0.7));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edit_without_changes_keeps_settings_and_settings_inherit() {
    let mut s = demo();
    let dir = tmp("inherit");
    let r = s.execute("tts.create", json!({"text": "Keep me.", "voice": "basic-male", "pitch": "high", "pace": 1.5, "time": 0, "dir": dir})).unwrap();
    let id = r["clip"].as_u64().unwrap();
    s.execute("tts.edit", json!({"clip": id, "pace": 0.8})).unwrap();
    let n = s.project.narrations.get(&clip(&s, id).item).unwrap();
    assert_eq!((n.text.as_str(), n.voice.as_str(), n.pitch, n.pace), ("Keep me.", "basic-male", VocalPitch::High, 0.8));
    let i = s.execute("tts.inspect", json!({"clip": id})).unwrap();
    assert_eq!(i["narration"]["pace"], 0.8);
    assert_eq!(i["narration"]["pitch"], "high");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edit_is_disabled_without_a_narration_clip() {
    let mut s = demo();
    let dir = tmp("enabled");
    s.state.selection.clear();
    assert!(!s.is_enabled("tts.edit"));
    assert!(s.execute("tts.edit", json!({"text": "x"})).is_err());
    // a non-narration clip is refused
    let other = s.active_sequence().unwrap().all_tracks().find_map(|t| t.items.first().map(|i| i.id)).unwrap();
    assert!(s.execute("tts.edit", json!({"clip": other.0, "text": "x"})).is_err());
    let r = s.execute("tts.create", json!({"text": "Enable me.", "time": 0, "dir": dir})).unwrap();
    assert!(s.is_enabled("tts.edit"), "the new narration is selected");
    s.state.selection.clear();
    assert!(s.execute("tts.edit", json!({"clip": r["clip"], "text": "By id."})).is_ok());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn edit_on_a_locked_track_is_refused() {
    let mut s = demo();
    let dir = tmp("locked");
    let r = s.execute("tts.create", json!({"text": "Locked.", "time": 0, "dir": dir})).unwrap();
    let track = r["track"].as_str().unwrap().to_string();
    s.execute("timeline.setTrack", json!({"track": track, "locked": true})).unwrap();
    let snap = (*s.project).clone();
    assert!(s.execute("tts.edit", json!({"clip": r["clip"], "text": "Changed."})).is_err());
    assert_eq!(*s.project, snap);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hostile_parameters_are_refused_without_changes() {
    let mut s = demo();
    let dir = tmp("hostile");
    let snap = (*s.project).clone();
    let undo0 = s.history.undo.len();
    let big = "word ".repeat(300_000);
    let cases = [
        json!({}),
        json!({"text": ""}),
        json!({"text": "   [pause 2s]  "}),
        json!({"text": "👍"}),
        json!({"text": 42}),
        json!({"text": big}),
        json!({"text": "hi", "voice": "nope"}),
        json!({"text": "hi", "voice": 7}),
        json!({"text": "hi", "pitch": "loud"}),
        json!({"text": "hi", "pace": "fast"}),
        json!({"text": "hi", "pace": 5.0}),
        json!({"text": "hi", "pace": -1}),
        json!({"text": "hi", "pace": 1e308}),
        json!({"text": "hi", "time": -5}),
        json!({"text": "hi", "track": {"x": 1}}),
        json!({"text": "hi", "track": "V1"}),
    ];
    for p in cases {
        let mut q = p.clone();
        q["dir"] = json!(dir);
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute("tts.create", q.clone())));
        assert!(r.is_ok(), "panicked on {p}");
        assert!(r.unwrap().is_err(), "accepted {p}");
        assert_eq!(*s.project, snap, "changed by {p}");
        assert_eq!(s.history.undo.len(), undo0);
    }
    for p in [json!({"clip": 999_999_999}), json!({"clip": "x"}), json!({"clip": -1})] {
        assert!(s.execute("tts.edit", p.clone()).is_err(), "{p}");
        assert!(s.execute("tts.inspect", p.clone()).is_err(), "{p}");
    }
    assert!(s.execute("tts.preview", json!({"text": "hi", "pace": 9})).is_err());
    // no sequence open
    let mut empty = Session::default();
    assert!(empty.execute("tts.create", json!({"text": "hi", "dir": dir})).is_err());
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn preview_changes_nothing_and_hands_the_audio_to_the_host() {
    let mut s = demo();
    let dir = tmp("preview");
    std::fs::create_dir_all(&dir).unwrap();
    let snap = (*s.project).clone();
    let undo0 = s.history.undo.len();
    let path = format!("{dir}/p.wav");
    let r = s.execute("tts.preview", json!({"text": "Preview me.", "path": path})).unwrap();
    assert_eq!(*s.project, snap);
    assert_eq!(s.history.undo.len(), undo0);
    let a = s.tts_preview.clone().unwrap();
    assert_eq!(r["samples"], a.samples.len());
    assert_eq!(wav_samples(&path), a.samples);
    // the sample sentence for "Hear this voice"
    let r = s.execute("tts.preview", json!({"sample": true, "voice": "basic-male"})).unwrap();
    assert_eq!(r["voice"], "basic-male");
    assert!(r["seconds"].as_f64().unwrap() > 1.0);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn narrations_survive_save_and_reload() {
    let mut s = demo();
    let dir = tmp("save");
    let r = s.execute("tts.create", json!({"text": "Saved [pause 500ms] and loaded.", "pitch": "extraLow", "time": 0, "dir": dir})).unwrap();
    let bytes = filmcraft_format::encode(&s.project, false);
    let loaded = filmcraft_format::decode(&bytes).unwrap();
    assert_eq!(loaded.project.narrations, s.project.narrations);
    let n = loaded.project.narrations.get(&ItemId(r["item"].as_u64().unwrap())).unwrap();
    assert_eq!((n.text.as_str(), n.pitch), ("Saved [pause 500ms] and loaded.", VocalPitch::ExtraLow));
    // a project without narrations writes no `narrations` key (older files are unchanged)
    let plain = String::from_utf8(filmcraft_format::encode(&demo().project, false)).unwrap();
    assert!(!plain.contains("narrations"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_narration_is_heard_in_the_mix() {
    let mut s = demo();
    let dir = tmp("mix");
    let r = s.execute("tts.create", json!({"text": "Can you hear me now?", "time": 0, "dir": dir})).unwrap();
    let track = r["track"].as_str().unwrap().to_string();
    let n = s.active_sequence().unwrap().audio_tracks.len();
    for i in 1..=n {
        if format!("A{i}") != track {
            s.execute("mixer.setStrip", json!({"strip": format!("A{i}"), "muted": true})).unwrap();
        }
    }
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let sr = s.active_sequence().unwrap().settings.sample_rate as usize;
    let mix = filmcraft_render::audio::mix_sequence(&s.project, s.active_sequence().unwrap(), 0, sr, &provider).channels;
    let rms = (mix[0].iter().map(|x| f64::from(x * x)).sum::<f64>() / sr as f64).sqrt();
    assert!(rms > 0.01, "narration audible, rms {rms}");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn render_fills_the_cache_that_create_and_preview_use() {
    let mut s = demo();
    let dir = tmp("render");
    let p = json!({"text": "Rendered in the background.", "voice": "basic-male", "pace": 1.1});
    let r = s.execute("tts.render", json!({"text": "Rendered in the background.", "voice": "basic-male", "pace": 1.1, "wait": true})).unwrap();
    let job = r["job"].as_u64().expect("a job");
    let j = s.jobs.iter().find(|j| j.id == job).unwrap();
    assert!(j.progress.finished.load(std::sync::atomic::Ordering::Relaxed));
    assert!(matches!(&*j.result.lock().unwrap(), Some(Ok(_))));
    // the same settings are now cached: render again is a no-op, preview hands back the same audio
    assert_eq!(s.execute("tts.render", p.clone()).unwrap()["cached"], true);
    s.execute("tts.preview", p.clone()).unwrap();
    let cached = s.tts_cache.lock().unwrap().last().unwrap().1.clone();
    assert!(std::sync::Arc::ptr_eq(s.tts_preview.as_ref().unwrap(), &cached));
    let mut q = p.clone();
    q["dir"] = json!(dir);
    q["time"] = json!(0);
    let c = s.execute("tts.create", q).unwrap();
    assert_eq!(c["speechDuration"], Tick::from_units(cached.samples.len() as i64, RATE).0);
    // a failing render reports the error in the job
    let r = s.execute("tts.render", json!({"text": "x", "voice": "kokoro-heart", "wait": true}));
    if let Ok(r) = r {
        let job = r["job"].as_u64().unwrap();
        let j = s.jobs.iter().find(|j| j.id == job).unwrap();
        assert!(matches!(&*j.result.lock().unwrap(), Some(Err(_))) || cfg!(feature = "neural-voices"));
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn natural_voices_are_listed_and_explain_what_is_missing() {
    let mut s = demo();
    let v = s.execute("tts.voices", json!({})).unwrap();
    let ids: Vec<&str> = v["voices"].as_array().unwrap().iter().map(|x| x["id"].as_str().unwrap()).collect();
    assert!(ids.contains(&"kokoro-heart") && ids.contains(&"kokoro-michael"), "{ids:?}");
    assert_eq!(v["package"]["id"], "kokoro-82m");
    assert!(v["package"]["size"].as_u64().unwrap() > 300_000_000);
    assert_eq!(v["package"]["available"], cfg!(feature = "neural-voices"));
    if !v["package"]["installed"].as_bool().unwrap() {
        let e = s.execute("tts.create", json!({"text": "Hi.", "voice": "kokoro-heart"})).unwrap_err().to_string();
        assert!(e.contains("download") || e.contains("not available"), "{e}");
    }
    if !cfg!(feature = "neural-voices") && !v["package"]["installed"].as_bool().unwrap() {
        let e = s.execute("tts.downloadVoices", json!({})).unwrap_err().to_string();
        assert!(e.contains("not available in this build"), "{e}");
    }
}

#[test]
fn a_project_subset_keeps_only_the_narrations_of_kept_items() {
    let mut s = demo();
    let dir = tmp("subset");
    let r = s.execute("tts.create", json!({"text": "Hello.", "voice": "basic-male", "dir": dir})).unwrap();
    let item = ItemId(r["item"].as_u64().unwrap());
    let others: std::collections::BTreeSet<ItemId> = s.project.items.keys().copied().filter(|i| *i != item).collect();
    assert!(crate::project_tools::project_subset(&s.project, &others).narrations.is_empty());
    let with: std::collections::BTreeSet<ItemId> = [item].into();
    assert!(crate::project_tools::project_subset(&s.project, &with).narrations.contains_key(&item));
}

#[test]
fn unique_wav_path_gives_up_instead_of_looping_forever() {
    let mut s = demo();
    let first = crate::voiceover::unique_wav_path(&s, "/nonexistent-dir", "Narration").unwrap();
    assert!(first.ends_with("Narration 1.wav"), "{first}");
    // every candidate name taken by a project item: an error, not an endless loop
    let template = s.project.items.values().next().cloned().unwrap();
    for k in 1..=99_999u32 {
        let mut it = template.clone();
        it.name = format!("Narration {k}.wav");
        std::sync::Arc::make_mut(&mut s.project).items.insert(ItemId(1_000_000 + u64::from(k)), it);
    }
    assert!(crate::voiceover::unique_wav_path(&s, "/nonexistent-dir", "Narration").is_err());
}

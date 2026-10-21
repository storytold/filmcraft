//! Tests of [`crate::music`]: a fake provider stands in for the models, so the command parsing,
//! the render job, import and placement, the Essential Sound type, the provenance sidecar and the
//! handling of API keys are tested without any model or network.

use super::*;
use filmcraft_music as fm;
use serde_json::json;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};

/// A provider that returns a quiet stereo tone of the requested length and counts its calls.
struct Fake {
    calls: Arc<AtomicUsize>,
    fail: Option<&'static str>,
}

struct FakeProvider {
    calls: Arc<AtomicUsize>,
    fail: Option<&'static str>,
}

impl music::MusicSource for Fake {
    fn provider(&self, id: &str, _: Option<&std::path::Path>, cfg: &HashMap<String, String>) -> std::result::Result<Box<dyn fm::Provider>, fm::MusicError> {
        if id == fm::catalog::STABLE_AUDIO_HOSTED && cfg.get("apiKey").is_none_or(String::is_empty) {
            return Err(fm::MusicError::Unavailable("no key".into()));
        }
        Ok(Box::new(FakeProvider { calls: self.calls.clone(), fail: self.fail }))
    }
}

impl fm::Provider for FakeProvider {
    fn info(&self) -> fm::ProviderInfo {
        fm::catalog::stable_audio_local_info(None)
    }

    fn generate(&self, p: &fm::Params) -> std::result::Result<fm::Audio, fm::MusicError> {
        self.generate_with_progress(p, &mut |_| true)
    }

    fn generate_with_progress(&self, p: &fm::Params, progress: &mut dyn FnMut(f32) -> bool) -> std::result::Result<fm::Audio, fm::MusicError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if let Some(e) = self.fail {
            return Err(fm::MusicError::Inference(e.into()));
        }
        for i in 0..4 {
            if !progress(i as f32 / 4.0) {
                return Err(fm::MusicError::Inference("cancelled".into()));
            }
        }
        let n = (p.duration * 44_100.0) as usize;
        let samples = (0..n * 2).map(|i| ((i / 2) as f32 * 0.05).sin() * 0.1).collect();
        Ok(fm::Audio { samples, sample_rate: 44_100, channels: 2 })
    }
}

fn tmp(name: &str) -> String {
    let d = std::env::temp_dir().join(format!("filmcraft-music-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    d.to_string_lossy().into_owned()
}

fn demo() -> (Session, Arc<AtomicUsize>) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let calls = Arc::new(AtomicUsize::new(0));
    s.music_source = Arc::new(Fake { calls: calls.clone(), fail: None });
    (s, calls)
}

fn clip(s: &Session, id: u64) -> filmcraft_project::TrackItem {
    s.active_sequence().unwrap().find_item(ClipId(id)).unwrap().1.clone()
}

fn all_clips(s: &Session) -> Vec<filmcraft_project::TrackItem> {
    s.active_sequence().unwrap().all_tracks().flat_map(|t| t.items.clone()).collect()
}

#[test]
fn providers_report_which_are_ready() {
    let (mut s, _) = demo();
    let r = s.execute("music.providers", json!({})).unwrap();
    let list = r["providers"].as_array().unwrap();
    assert_eq!(list.len(), 3);
    assert!(list.iter().any(|p| p["id"] == "stable-audio-local" && p["kind"] == "local"));
    let hosted = list.iter().find(|p| p["id"] == "stability-api").unwrap();
    assert_eq!(hosted["ready"], false, "no key yet");
    s.execute("prefs.set", json!({"key": "music.api.stabilityApiKey", "value": "sk-test-123"})).unwrap();
    let r = s.execute("music.providers", json!({})).unwrap();
    assert_eq!(r["providers"].as_array().unwrap().iter().find(|p| p["id"] == "stability-api").unwrap()["ready"], true);
    assert_eq!(r["poweredBy"], "Powered by Stability AI");
}

#[test]
fn create_places_a_music_clip_at_the_playhead_as_one_undo_step() {
    let (mut s, calls) = demo();
    let dir = tmp("create");
    s.execute("playhead.set", json!({"seconds": 2.0})).unwrap();
    let t0 = s.playhead();
    let undo0 = s.history.undo.len();
    let before = all_clips(&s);
    let r = s.execute("music.create", json!({"prompt": "warm piano", "duration": 3.0, "bpm": 70, "seed": 5, "dir": dir})).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let c = clip(&s, r["clip"].as_u64().unwrap());
    assert_eq!(c.start, t0);
    assert_eq!(c.duration, Tick::from_units(3 * 44_100, 44_100));
    assert_eq!(c.essential.as_ref().map(|e| e.kind), Some(filmcraft_project::AudioType::Music), "typed as Music");
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step");
    assert!(std::path::Path::new(r["path"].as_str().unwrap()).exists());
    // the provenance sidecar says where the music came from, and never holds a key
    let prov: Value = serde_json::from_slice(&std::fs::read(r["provenance"].as_str().unwrap()).unwrap()).unwrap();
    assert_eq!(prov["provider"], "stable-audio-local");
    assert_eq!(prov["prompt"], "warm piano");
    assert_eq!(prov["seed"], 5);
    assert_eq!(prov["bpm"], 70.0);
    let i = s.execute("music.inspect", json!({"clip": r["clip"]})).unwrap();
    assert_eq!(i["provenance"]["prompt"], "warm piano");
    s.undo().unwrap();
    assert_eq!(all_clips(&s).len(), before.len(), "undo removes the clip");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_cue_never_overwrites_existing_clips() {
    let (mut s, _) = demo();
    let dir = tmp("place");
    let a = s.execute("music.create", json!({"prompt": "a", "duration": 2.0, "time": 0, "seed": 1, "dir": dir})).unwrap();
    let b = s.execute("music.create", json!({"prompt": "b", "duration": 2.0, "time": 0, "seed": 2, "dir": dir})).unwrap();
    assert_ne!(a["track"], b["track"], "the second cue lands on a different track");
    let (ca, cb) = (clip(&s, a["clip"].as_u64().unwrap()), clip(&s, b["clip"].as_u64().unwrap()));
    assert_eq!(ca.start, cb.start);
    assert!(
        s.execute("music.create", json!({"prompt": "c", "duration": 2.0, "time": 0, "track": a["track"], "seed": 3, "dir": dir})).is_err(),
        "an explicit busy track is refused"
    );
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn render_job_fills_the_cache_and_create_reuses_it() {
    let (mut s, calls) = demo();
    let dir = tmp("render");
    let params = json!({"prompt": "slow strings", "duration": 2.0, "seed": 9});
    let mut render = params.clone();
    render["wait"] = json!(true);
    let r = s.execute("music.render", render).unwrap();
    assert_eq!(r["cached"], false);
    assert_eq!(r["seed"], 9);
    let job = s.jobs.iter().find(|j| Some(j.id) == r["job"].as_u64()).unwrap();
    assert!(job.progress.finished.load(Ordering::Relaxed));
    assert!(job.result.lock().unwrap().as_ref().unwrap().is_ok());
    assert_eq!(job.progress.done.load(Ordering::Relaxed), 1000, "progress ran to the end");
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    let mut create = params;
    create["dir"] = json!(dir);
    s.execute("music.create", create).unwrap();
    assert_eq!(calls.load(Ordering::SeqCst), 1, "create used the cached render");
    assert_eq!(s.execute("music.render", json!({"prompt": "slow strings", "duration": 2.0, "seed": 9})).unwrap()["cached"], true);
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn a_cancelled_or_failed_job_reports_the_error_and_changes_nothing() {
    let (mut s, calls) = demo();
    let clips = all_clips(&s).len();
    s.music_source = Arc::new(Fake { calls: calls.clone(), fail: Some("the model crashed") });
    let r = s.execute("music.render", json!({"prompt": "x", "duration": 2.0, "wait": true})).unwrap();
    let job = s.jobs.iter().find(|j| Some(j.id) == r["job"].as_u64()).unwrap();
    assert!(job.result.lock().unwrap().as_ref().unwrap().is_err());
    assert!(job.progress.error.lock().unwrap().as_deref().unwrap_or("").contains("the model crashed"));
    let e = s.execute("music.create", json!({"prompt": "x", "duration": 2.0})).unwrap_err().to_string();
    assert!(e.contains("the model crashed"), "{e}");
    assert_eq!(all_clips(&s).len(), clips, "a failed create leaves the sequence untouched");
}

#[test]
fn bad_parameters_are_errors_not_panics() {
    let (mut s, calls) = demo();
    for p in [
        json!({}),
        json!({"prompt": ""}),
        json!({"prompt": "x"}),
        json!({"prompt": "x", "duration": 0}),
        json!({"prompt": "x", "duration": -3}),
        json!({"prompt": "x", "duration": 1e9}),
        json!({"prompt": "x", "duration": "long"}),
        json!({"prompt": 5, "duration": 10}),
        json!({"prompt": "x", "duration": 10, "bpm": "fast"}),
        json!({"prompt": "x", "duration": 10, "bpm": -1}),
        json!({"prompt": "x", "duration": 10, "seed": -4}),
        json!({"prompt": "x", "duration": 10, "provider": "suno"}),
    ] {
        for cmd in ["music.render", "music.create"] {
            assert!(s.execute(cmd, p.clone()).is_err(), "{cmd} {p}");
        }
    }
    assert!(s.execute("music.create", json!({"prompt": "x", "duration": 10, "time": -5})).is_err(), "negative start time");
    assert_eq!(calls.load(Ordering::SeqCst), 0, "nothing reached a provider");
}

#[test]
fn fill_inout_uses_the_sequence_marks() {
    let (mut s, _) = demo();
    let dir = tmp("inout");
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    s.execute("markers.markIn", json!({})).unwrap();
    s.execute("playhead.set", json!({"seconds": 5.0})).unwrap();
    s.execute("markers.markOut", json!({})).unwrap();
    s.execute("playhead.set", json!({"seconds": 0.0})).unwrap();
    let r = s.execute("music.create", json!({"prompt": "bed", "fill": "inout", "seed": 4, "dir": dir})).unwrap();
    let c = clip(&s, r["clip"].as_u64().unwrap());
    let q = s.active_sequence().unwrap();
    let (mark_in, mark_out) = (q.mark_in.unwrap(), q.mark_out.unwrap());
    assert_eq!(c.start, mark_in, "starts at the In point");
    let want = (mark_out + q.settings.frame_rate.frame_duration() - mark_in).seconds();
    assert!((c.duration.seconds() - want).abs() < 1e-4, "{} vs {want}", c.duration.seconds());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn ducking_is_optional_and_does_not_break_the_edit() {
    let (mut s, _) = demo();
    let dir = tmp("duck");
    let r = s.execute("music.create", json!({"prompt": "bed", "duration": 4.0, "seed": 4, "ducking": true, "dir": dir})).unwrap();
    assert_eq!(r["ducking"], true);
    assert!(clip(&s, r["clip"].as_u64().unwrap()).essential.is_some());
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn api_keys_stay_in_preferences_and_out_of_everything_else() {
    let (mut s, _) = demo();
    let dir = tmp("secret");
    let key = "sk-SECRET-VALUE-42";
    s.execute("prefs.set", json!({"key": "music.api.stabilityApiKey", "value": key})).unwrap();
    s.execute("prefs.set", json!({"key": "music.api.otherApiKey", "value": key})).unwrap();
    // the stored preference has it...
    assert!(s.prefs.to_value().to_string().contains(key));
    // ...but reading preferences back never shows it
    for q in [json!({}), json!({"key": "music"}), json!({"key": "music.api"}), json!({"key": "music.api.stabilityApiKey"})] {
        let out = s.execute("prefs.get", q.clone()).unwrap().to_string();
        assert!(!out.contains(key), "prefs.get {q} leaked the key");
    }
    assert_eq!(s.execute("prefs.get", json!({"key": "music.api.stabilityApiKey"})).unwrap(), "(set)");
    // generating with it puts it nowhere in the project, the history labels or the provenance
    let r = s.execute("music.create", json!({"prompt": "bed", "duration": 2.0, "provider": "stability-api", "seed": 1, "dir": dir})).unwrap();
    let project = serde_json::to_string(&s.project).unwrap();
    assert!(!project.contains(key), "the project file holds the key");
    assert!(!std::fs::read_to_string(r["provenance"].as_str().unwrap()).unwrap().contains(key));
    assert!(!r.to_string().contains(key));
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn unknown_music_preferences_are_rejected() {
    let (mut s, _) = demo();
    assert!(s.execute("prefs.set", json!({"key": "music.nope", "value": 1})).is_err());
    assert!(s.execute("prefs.set", json!({"key": "music.provider", "value": 5})).is_err());
    assert_eq!(s.execute("prefs.get", json!({"key": "music.provider"})).unwrap(), "stable-audio-local");
}

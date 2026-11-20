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
fn mixed_audio_is_the_source_pieces_outside_local_level_matching() {
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
    let margin = rm.levels.as_ref().map_or(plan.xfade as usize, |m| Tick(m.span).to_units_floor(SR as i64) as usize);
    for p in &plan.pieces {
        for k in (margin..(p.len as usize).saturating_sub(margin)).step_by(31) {
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
fn remixed_out_trim_keeps_requested_duration_and_audio() {
    let (m, _) = music();
    let (mut s, c) = session(&m, None);
    let original = item(&s, c);
    s.execute("clip.remix", json!({"clip":c.0,"seconds":65.0})).unwrap();
    let extended = item(&s, c);
    assert!(extended.duration > Tick::from_seconds_f64(60.0));
    let sample_audio = |s: &Session, at: i64| {
        let provider = s.media.provider(s.project.clone(), s.services.clone());
        filmcraft_render::audio::mix_sequence(&s.project, s.active_sequence().unwrap(), at, 48000, &provider).channels
    };
    let first = sample_audio(&s, 0);
    let last = sample_audio(&s, 59 * 48000);
    let n = s.history.undo.len();
    let target = Tick::from_seconds_f64(60.0);
    s.execute("timeline.trim", json!({"clip":c.0,"edge":"out","delta":(target-extended.duration).0})).unwrap();
    let trimmed = item(&s, c);
    assert_eq!(trimmed.duration, target, "a normal Out trim must not clamp to the original source length");
    assert_eq!(trimmed.effects, extended.effects, "trim must preserve the arrangement and seam levels");
    let restored: TrackItem = serde_json::from_str(&serde_json::to_string(&trimmed).unwrap()).unwrap();
    assert_eq!(restored.remix_output_duration(), Some(extended.duration));
    assert_eq!(s.history.undo.len(), n + 1);
    assert_eq!(sample_audio(&s, 0), first);
    assert_eq!(sample_audio(&s, 59 * 48000), last);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(item(&s, c), extended);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(item(&s, c), trimmed);
    s.execute("timeline.trim", json!({"clip":c.0,"edge":"out","delta":Tick::from_seconds_f64(100.0).0})).unwrap();
    assert_eq!(item(&s, c), extended, "the trimmed tail can be restored, but not extended past the plan");
    s.execute("clip.remix.revert", json!({"clip":c.0})).unwrap();
    assert_eq!(item(&s, c), original);
}

#[test]
fn long_extension_survives_the_optional_level_matching_limit() {
    let (m, _) = dsp::test_music(SR, 120.0, 15, 3);
    let (mut s, c) = session(&m, None);
    let original = item(&s, c);
    let undo = s.history.undo.len();
    let target = Tick::from_seconds_f64(15.0 * 60.0);
    let report = s.execute("clip.remix", json!({"clip": c.0, "duration": target.0})).unwrap();
    let extended = item(&s, c);
    let rm = filmcraft_render::remix::Remix::of(&extended).unwrap();
    assert!(rm.pieces.len() > 33, "must exercise the matching cap: {} pieces", rm.pieces.len());
    assert!(rm.levels.is_none(), "over-budget matching falls back to the original crossfades");
    assert!((extended.duration - target).abs() <= beat());
    assert_eq!(report["remixed"], true);
    assert_eq!(s.history.undo.len(), undo + 1);
    // The persisted arrangement remains playable without joinLevels, including its tail.
    let restored: TrackItem = serde_json::from_str(&serde_json::to_string(&extended).unwrap()).unwrap();
    assert_eq!(filmcraft_render::remix::Remix::of(&restored), Some(rm));
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let q = s.active_sequence().unwrap();
    let tail = extended.duration.to_units_floor(SR as i64) - SR as i64;
    let audio = filmcraft_render::audio::mix_sequence(&s.project, q, tail, SR as usize, &provider);
    assert!(audio.channels.iter().flatten().all(|sample| sample.is_finite()));
    assert!(audio.channels.iter().flatten().any(|sample| sample.abs() > 0.01));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(item(&s, c), original);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(item(&s, c), extended);
}

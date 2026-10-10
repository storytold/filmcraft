//! Render previews end to end: render → green, edit → stale, undo → green, save/open keeps them,
//! delete, audio previews, playback frames from previews.

use std::sync::Arc;

use filmcraft_media::Generator;
use filmcraft_media::generators::GeneratorSource;
use filmcraft_project::{ClipId, Label, ParamValue, SequenceSettings, TrackKind, find_effect};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde_json::{Value, json};

use crate::Session;
use crate::previews::BarState;

fn tmp(name: &str) -> std::path::PathBuf {
    let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    std::env::temp_dir().join(format!("filmcraft-previews-test-{name}-{}-{nanos}", std::process::id()))
}

/// A 64×36 24 fps sequence: V1 = matte with Tint (0..24) + plain matte (24..48), A1 = tone (0..48).
fn session(name: &str) -> (Session, ClipId) {
    let mut s = Session::default();
    s.previews.set_dir(Some(tmp(name)));
    let r = FrameRate::FPS_24;
    let mut p = (*s.project).clone();
    let matte = crate::demo::add_generator(
        &mut p,
        &s.media,
        GeneratorSource::new(Generator::ColorMatte { color: [0.2, 0.5, 0.8, 1.0] }, 64, 36, r, Tick(10 * TICKS_PER_SECOND)),
        "Matte",
        Label::Iris,
        None,
    );
    let tone = crate::demo::add_generator(
        &mut p,
        &s.media,
        GeneratorSource::new(Generator::Tone { hz: 440.0, db: -12.0 }, 0, 0, r, Tick(10 * TICKS_PER_SECOND)),
        "Tone",
        Label::Iris,
        None,
    );
    let seq = p.new_sequence("Seq", SequenceSettings { width: 64, height: 36, frame_rate: r, ..Default::default() }, 2, 1, None);
    let mut a = p.make_track_item(matte, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(24)), r).unwrap();
    let mut b = p.make_track_item(matte, TrackKind::Video, r.tick_of(24), TimeRange::new(r.tick_of(24), r.tick_of(24)), r).unwrap();
    let au = p.make_track_item(tone, TrackKind::Audio, Tick::ZERO, TimeRange::new(Tick::ZERO, r.tick_of(48)), r).unwrap();
    for e in a.effects.iter_mut().chain(b.effects.iter_mut()) {
        filmcraft_project::resolve_auto_points(e, (64, 36), (64, 36));
    }
    a.effects.push(find_effect("tint").unwrap().instance());
    let a_id = a.id;
    let q = p.sequence_mut(seq).unwrap();
    q.video_tracks[0].items = vec![a, b];
    q.audio_tracks[0].items = vec![au];
    s.project = Arc::new(p);
    s.state.active_sequence = Some(seq);
    s.state.open_sequences = vec![seq];
    (s, a_id)
}

fn states(s: &Session) -> Vec<String> {
    let v = crate::previews::bar_json(s).unwrap();
    v["segments"].as_array().unwrap().iter().map(|g| g["state"].as_str().unwrap().to_string()).collect()
}

fn job_ok(s: &Session, v: &Value) {
    let id = v["job"].as_u64().expect("a job was started");
    let j = s.jobs.iter().find(|j| j.id == id).unwrap().to_json();
    assert!(j["finished"].as_bool().unwrap(), "{j}");
    assert!(j["result"].get("error").is_none(), "{j}");
}

#[test]
fn render_turns_green_edit_invalidates_and_undo_restores() {
    let (mut s, a) = session("undo");
    assert_eq!(states(&s), vec!["yellow", "none"]);
    let v = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap();
    assert_eq!(v["segments"], 1, "only the segment with an effect");
    job_ok(&s, &v);
    assert_eq!(states(&s), vec!["green", "none"]);
    assert_eq!(s.previews.count(), 1);
    // playback reads preview frames for the rendered segment only
    let seq = s.state.active_sequence.unwrap();
    let f = s.previews.frame(&s.media, &s.project, seq, 5, 1.0).expect("preview frame");
    assert_eq!((f.width, f.height), (64, 36));
    assert!(s.previews.frame(&s.media, &s.project, seq, 30, 1.0).is_none());
    // the preview frame matches the live render closely (ProRes 10-bit)
    let live = filmcraft_render::render_sequence(
        &s.project,
        seq,
        FrameRate::FPS_24.tick_of(5),
        Default::default(),
        &s.media.provider(s.project.clone(), s.services.clone()),
    )
    .unwrap();
    let live = live.over_black_rgba8();
    let prev = f.to_rgba8().unwrap();
    let diff = live.iter().zip(&prev).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap();
    assert!(diff <= 4, "preview differs from live render by {diff}");
    // edit the effect → stale
    s.edit_sequence("Tint", |q, _, _| {
        let (_, it) = q.find_item_mut(a).unwrap();
        it.effect_mut("tint").unwrap().params.values_mut().find(|p| matches!(p.value, ParamValue::Float(_))).unwrap().value = ParamValue::Float(30.0);
        Ok(())
    })
    .unwrap();
    assert_eq!(states(&s), vec!["yellow", "none"]);
    // undo → the old content hash → green again
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(states(&s), vec!["green", "none"]);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(states(&s), vec!["yellow", "none"]);
    s.execute("edit.undo", json!({})).unwrap();
    // nothing left to render
    let v = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap();
    assert!(v["job"].is_null());
    // Render In to Out also renders the no-bar segment
    let v = s.execute("sequence.renderInToOut", json!({"wait": true})).unwrap();
    assert_eq!(v["segments"], 1);
    assert_eq!(states(&s), vec!["green", "green"]);
    let bar = s.previews.bar(&s.project, s.state.active_sequence.unwrap());
    assert_eq!(bar.len(), 1, "adjacent green spans merge in the drawn bar");
    assert_eq!(bar[0].state, BarState::Green);
    // Delete Render Files In to Out (In/Out around the second clip only)
    s.edit_sequence("marks", |q, _, _| {
        q.mark_in = Some(FrameRate::FPS_24.tick_of(30));
        q.mark_out = Some(FrameRate::FPS_24.tick_of(40));
        Ok(())
    })
    .unwrap();
    s.execute("sequence.deleteRenderFilesInToOut", json!({})).unwrap();
    assert_eq!(states(&s), vec!["green", "none"]);
    s.execute("sequence.deleteRenderFiles", json!({})).unwrap();
    assert_eq!(states(&s), vec!["yellow", "none"]);
    assert_eq!(s.previews.count(), 0);
    assert!(!s.is_enabled("sequence.deleteRenderFiles"), "disabled with no files");
    let _ = std::fs::remove_dir_all(s.previews.dir().unwrap());
}

#[test]
fn previews_survive_save_and_open() {
    let (mut s, _) = session("save");
    let v = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap();
    job_ok(&s, &v);
    let folder = tmp("save-project");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("Film.fcproj").to_string_lossy().to_string();
    s.execute("file.save", json!({"path": path})).unwrap();
    // the unsaved project's previews moved next to the project
    assert_eq!(s.previews.dir().unwrap(), folder.join("FilmCraft Previews").join("Film"));
    assert_eq!(states(&s), vec!["green", "none"]);
    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(states(&t), vec!["green", "none"], "hashes are stable across save/load");
    let _ = std::fs::remove_dir_all(&folder);
}

#[test]
fn render_selection_and_audio_previews() {
    let (mut s, a) = session("sel");
    s.state.selection = vec![a];
    let v = s.execute("sequence.renderSelection", json!({"wait": true})).unwrap();
    assert_eq!(v["segments"], 1);
    job_ok(&s, &v);
    // audio: the preview mix equals the live mix
    let seq = s.state.active_sequence.unwrap();
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let q = s.project.sequence(seq).unwrap();
    let live = filmcraft_render::audio::mix_sequence(&s.project, q, 1000, 30_000, &provider);
    let v = s.execute("sequence.renderAudio", json!({"wait": true})).unwrap();
    job_ok(&s, &v);
    assert!(s.previews.audio_segments(&s.project, seq).iter().all(|g| s.previews.has_audio(&g.hash)));
    let mixed = s.previews.mix(&s.project, seq, 1000, 30_000, &provider);
    for c in 0..2 {
        for (x, y) in live.channels[c].iter().zip(&mixed.channels[c]) {
            assert!((x - y).abs() < 1e-6);
        }
    }
    assert!(mixed.channels[0].iter().any(|x| x.abs() > 0.05), "tone is audible");
    // muting the track changes the audio hash: the preview no longer applies
    s.edit_sequence("mute", |q, _, _| {
        q.audio_tracks[0].muted = true;
        Ok(())
    })
    .unwrap();
    assert!(!s.previews.audio_segments(&s.project, seq).iter().any(|g| s.previews.has_audio(&g.hash)));
    let _ = std::fs::remove_dir_all(s.previews.dir().unwrap());
}

#[test]
fn render_commands_are_registered_like_premiere() {
    let f = |id| crate::commands::find(id).unwrap();
    assert_eq!(f("sequence.renderEffectsInToOut").shortcut, Some("Enter"));
    for id in [
        "sequence.renderEffectsInToOut",
        "sequence.renderInToOut",
        "sequence.renderSelection",
        "sequence.renderAudio",
        "sequence.deleteRenderFiles",
        "sequence.deleteRenderFilesInToOut",
    ] {
        assert_eq!(f(id).menu, &["Sequence"]);
    }
}

#[test]
fn float_wav_round_trip() {
    let x: Vec<f32> = (0..200).map(|i| (i as f32 * 0.1).sin()).collect();
    assert_eq!(crate::previews::read_wav_f32(&crate::previews::write_wav_f32(&x, 48_000)).unwrap(), x);
}

#[test]
fn bar_state_serializes_lowercase() {
    assert_eq!(serde_json::to_value(BarState::Green).unwrap(), json!("green"));
}

//! `clip.export` (the Timeline's clip menu ▸ Export Clip…): the selected clips are exported alone,
//! over their own span, at their source's frame size, frame rate and sample rate, without changing
//! the project, the selection or the undo history.

use std::path::{Path, PathBuf};

use filmcraft_media::DemoScene;
use filmcraft_project::{ClipId, ItemId, ParamValue};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde_json::{Value, json};

use crate::Session;
use crate::media_test_util::{make_movie, session_with, tmp_dir};

/// Two 64×36 24 fps movies, `a.mp4` (12 frames) then `b.mp4` (6 frames), in turn on V1/A1 of a
/// sequence then made 128×72 at 30 fps with Motion scaled, so each clip shows at 200 %. Returns the
/// session, the folder of the movies, and the video and audio clips in timeline order.
fn two_clips(name: &str) -> (Session, PathBuf, Vec<ClipId>, Vec<ClipId>) {
    let dir = tmp_dir(name);
    let (a, b) = (dir.join("a.mp4"), dir.join("b.mp4"));
    make_movie(&a, DemoScene::Dunes, 64, 36, 12);
    make_movie(&b, DemoScene::Forest, 64, 36, 6);
    let (mut s, _, _) = session_with(&[&a, &b]);
    s.execute("sequence.settings", json!({"width": 128, "height": 72, "fps": 30, "scaleMotion": true})).unwrap();
    let q = s.active_sequence().unwrap();
    let v = q.video_tracks[0].items.iter().map(|i| i.id).collect();
    let au = q.audio_tracks[0].items.iter().map(|i| i.id).collect();
    (s, dir, v, au)
}

fn motion(q: &filmcraft_project::Sequence, clip: ClipId) -> ((f64, f64), f64) {
    let m = q.find_item(clip).unwrap().1.effect("motion").unwrap();
    let ParamValue::Vec2(p) = m.param("position").unwrap().value else { panic!("position is a point") };
    let ParamValue::Float(sc) = m.param("scale").unwrap().value else { panic!("scale is a number") };
    ((p.x, p.y), sc)
}

fn folder(dir: &Path) -> String {
    format!("{}/", dir.display())
}

#[test]
fn the_snapshot_holds_only_the_clips_at_their_sources_size_and_rate() {
    let (s, _dir, v, a) = two_clips("clip-export-snapshot");
    let seq = s.state.active_sequence.unwrap();
    let snap = crate::export_tools::clip_snapshot(&s.project, seq, &[v[1], a[1]]).unwrap();
    assert!(s.project.item(snap.sequence).is_none(), "the snapshot's sequence is not in the user's project");
    let q = snap.project.sequence(snap.sequence).unwrap();
    assert_eq!((q.settings.width, q.settings.height, q.settings.frame_rate), (64, 36, FrameRate::FPS_24));
    let kept: Vec<ClipId> = q.all_tracks().flat_map(|t| &t.items).map(|i| i.id).collect();
    assert_eq!(kept, vec![v[1], a[1]]);
    let orig = s.active_sequence().unwrap().find_item(v[1]).unwrap().1.clone();
    assert_eq!(snap.range, TimeRange::new(orig.start, orig.duration), "the clip's own span");
    // shown at 200 % in the 128×72 sequence, the clip fills the 64×36 frame at 100 %
    let ((x, y), scale) = motion(q, v[1]);
    assert!((x - 32.0).abs() < 1e-6 && (y - 18.0).abs() < 1e-6 && (scale - 100.0).abs() < 1e-6, "{x} {y} {scale}");
    assert_eq!(snap.name, "b", "named after the clip, without its extension");
    assert!(snap.video);
    assert_eq!(snap.sources.len(), 1);
}

#[test]
fn export_clip_writes_the_clip_alone_in_source_quality_and_changes_nothing() {
    let (mut s, dir, v, a) = two_clips("clip-export-run");
    s.state.selection = vec![v[1], a[1]];
    let (before, undo, redo) = ((*s.project).clone(), s.history.undo.len(), s.history.redo.len());
    let out = dir.join("out");
    let r = s.execute("clip.export", json!({"path": folder(&out), "wait": true})).unwrap();
    assert_eq!(r["preset"], json!(filmcraft_export::presets::DEFAULT_PRESET), "{r}");
    assert_eq!(r["source"]["width"], json!(64), "{r}");
    let path = r["path"].as_str().unwrap().to_string();
    assert!(path.ends_with("b.mp4"), "{path}");
    assert_eq!(r["result"]["frames"], json!(6), "the 6 frames of b.mp4 at its own 24 fps: {r}");
    // what was written is the source's size and rate
    let mut check = Session::default();
    let imported = check.execute("file.import", json!({"paths": [path]})).unwrap();
    let id = ItemId(imported["items"][0].as_u64().unwrap());
    let info = check.project.item(id).unwrap().as_media().unwrap().info.clone();
    let video = info.video.as_ref().unwrap();
    assert_eq!((video.width, video.height, video.frame_rate), (64, 36, FrameRate::FPS_24));
    assert!(info.audio().is_some(), "the linked sound came along");
    // the project, its history and the selection are as they were
    assert_eq!(*s.project, before);
    assert_eq!((s.history.undo.len(), s.history.redo.len()), (undo, redo));
    assert_eq!(s.state.selection, vec![v[1], a[1]]);
}

#[test]
fn a_default_name_never_replaces_a_file_and_no_path_replaces_the_source() {
    let (mut s, dir, v, _) = two_clips("clip-export-names");
    let source = dir.join("a.mp4");
    let original = std::fs::read(&source).unwrap();
    // into the movies' own folder: `a.mp4` is the source, so the clip becomes `a_2.mp4`
    let r = s.execute("clip.export", json!({"clip": v[0].0, "path": folder(&dir), "wait": true})).unwrap();
    assert!(r["path"].as_str().unwrap().ends_with("a_2.mp4"), "{r}");
    assert_eq!(std::fs::read(&source).unwrap(), original, "the source is untouched");
    // naming the source itself is refused before any job starts
    let jobs = s.jobs.len();
    let e = s.execute("clip.export", json!({"clip": v[0].0, "path": source.to_string_lossy(), "wait": true})).unwrap_err();
    assert!(e.to_string().contains("own media"), "{e}");
    assert_eq!(s.jobs.len(), jobs);
    assert_eq!(std::fs::read(&source).unwrap(), original);
}

#[test]
fn sound_alone_is_exported_as_wav() {
    let (mut s, dir, _, a) = two_clips("clip-export-sound");
    s.state.selection = vec![a[0]];
    let r = s.execute("clip.export", json!({"path": folder(&dir.join("out")), "wait": true})).unwrap();
    let path = r["path"].as_str().unwrap();
    assert!(path.ends_with("a.wav"), "{r}");
    assert!(Path::new(path).metadata().is_ok_and(|m| m.len() > 44), "{path}");
    // a chosen preset or format is used instead
    let r = s.execute("clip.export", json!({"clip": a[0].0, "format": "flac", "path": folder(&dir.join("out")), "wait": true})).unwrap();
    assert!(r["path"].as_str().unwrap().ends_with(".flac"), "{r}");
}

#[test]
fn hostile_clip_export_parameters_are_errors_not_panics() {
    let (mut s, dir, v, _) = two_clips("clip-export-hostile");
    let out = folder(&dir.join("out"));
    s.state.selection.clear();
    let jobs = s.jobs.len();
    for params in [
        json!({}),
        Value::Null,
        json!({"clip": -1}),
        json!({"clip": "x"}),
        json!({"clips": []}),
        json!({"clips": [1.5]}),
        json!({"clip": u64::MAX}),
        json!({"clip": v[0].0, "clips": [v[0].0]}),
        json!({"clip": v[0].0, "range": "entire"}),
        json!({"clip": v[0].0, "sequence": 1}),
        json!({"clip": v[0].0, "fps": 0, "path": out}),
        json!({"clip": v[0].0, "width": 0, "height": 0, "path": out}),
        json!({"clip": v[0].0, "preset": "no such preset", "path": out}),
        json!({"clip": v[0].0, "format": "nope", "path": out}),
    ] {
        let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| s.execute("clip.export", params.clone())));
        assert!(r.is_ok_and(|r| r.is_err()), "{params} must be refused");
    }
    assert_eq!(s.jobs.len(), jobs, "nothing was started");
    // without an open sequence the command is not available
    assert!(Session::default().execute("clip.export", json!({"clip": 1})).is_err());
}

#[test]
fn a_clip_ending_past_the_time_range_is_refused() {
    let (s, _dir, v, _) = two_clips("clip-export-overflow");
    let seq = s.state.active_sequence.unwrap();
    let mut p = (*s.project).clone();
    let item = p.sequence_mut(seq).unwrap().find_item_mut(v[0]).unwrap().1;
    (item.start, item.duration) = (Tick(i64::MAX - 1), Tick(10));
    let r = std::panic::catch_unwind(|| crate::export_tools::clip_snapshot(&p, seq, &[v[0]]).map(|_| ()));
    assert!(r.is_ok_and(|r| r.is_err()));
    // clips that are not in the sequence give an error too
    assert!(crate::export_tools::clip_snapshot(&s.project, seq, &[ClipId(u64::MAX)]).is_err());
}

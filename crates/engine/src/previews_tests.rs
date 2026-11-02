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
    session_with(name, Generator::ColorMatte { color: [0.2, 0.5, 0.8, 1.0] })
}

/// [`session`] with `video` in place of the matte on V1.
fn session_with(name: &str, video: Generator) -> (Session, ClipId) {
    let mut s = Session::default();
    s.previews.set_dir(Some(tmp(name)));
    let r = FrameRate::FPS_24;
    let mut p = (*s.project).clone();
    let matte = crate::demo::add_generator(&mut p, &s.media, GeneratorSource::new(video, 64, 36, r, Tick(10 * TICKS_PER_SECOND)), "Matte", Label::Iris, None);
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

/// Render the (yellow) Tint segment: Enter leaves yellow segments live, so render the selection.
fn render_tint(s: &mut Session, tint: ClipId) -> Value {
    s.state.selection = vec![tint];
    let v = s.execute("sequence.renderSelection", json!({"wait": true})).unwrap();
    job_ok(s, &v);
    v
}

#[test]
fn render_turns_green_edit_invalidates_and_undo_restores() {
    let (mut s, a) = session("undo");
    assert_eq!(states(&s), vec!["yellow", "none"]);
    let v = render_tint(&mut s, a);
    assert_eq!(v["segments"], 1, "only the segment with the selected clip");
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
    );
    let live = live.over_black_rgba8();
    let prev = f.to_rgba8();
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

fn max_diff(a: &[u8], b: &[u8]) -> i32 {
    a.iter().zip(b).map(|(a, b)| (*a as i32 - *b as i32).abs()).max().unwrap()
}

fn set_marks(s: &mut Session, marks: Option<(i64, i64)>) {
    let r = FrameRate::FPS_24;
    s.edit_sequence("marks", |q, _, _| {
        q.mark_in = marks.map(|m| r.tick_of(m.0));
        q.mark_out = marks.map(|m| r.tick_of(m.1));
        Ok(())
    })
    .unwrap();
}

/// Render-bar spans as (first frame, end frame, state).
fn spans(s: &Session) -> Vec<(i64, i64, BarState)> {
    let r = FrameRate::FPS_24;
    s.previews.bar(&s.project, s.state.active_sequence.unwrap()).iter().map(|b| (r.frame_at(b.start), r.frame_at(b.end), b.state)).collect()
}

/// #424: Render In to Out renders the frames between In and Out, not every segment that In/Out
/// touches; a later render fills only the gaps, and Delete Render Files In to Out removes only the
/// files holding marked frames.
#[test]
fn render_in_to_out_renders_only_the_marked_frames() {
    use BarState::{Green, None as NoBar, Yellow};
    let (mut s, _) = session_with("range", Generator::CountingLeader);
    let seq = s.state.active_sequence.unwrap();
    set_marks(&mut s, Some((6, 11)));
    let v = s.execute("sequence.renderInToOut", json!({"wait": true})).unwrap();
    job_ok(&s, &v);
    assert_eq!(v["frames"], 6, "frames 6..=11, not the whole 24-frame segment: {v}");
    assert_eq!(spans(&s), vec![(0, 6, Yellow), (6, 12, Green), (12, 24, Yellow), (24, 48, NoBar)]);
    assert_eq!(states(&s), vec!["yellow", "none"], "a partly rendered segment is not green");
    assert_eq!(crate::previews::bar_json(&s).unwrap()["segments"][0]["renderedFrames"], 6);
    for f in [5, 12, 30] {
        assert!(s.previews.frame(&s.media, &s.project, seq, f, 1.0).is_none(), "frame {f} has no preview");
    }
    // the preview's frames line up with the sequence (the leader changes from frame to frame)
    let live = |s: &Session, f: i64| {
        filmcraft_render::render_sequence(
            &s.project,
            seq,
            FrameRate::FPS_24.tick_of(f),
            Default::default(),
            &s.media.provider(s.project.clone(), s.services.clone()),
        )
        .over_black_rgba8()
    };
    let prev = s.previews.frame(&s.media, &s.project, seq, 9, 1.0).expect("frame 9 has a preview").to_rgba8();
    assert!(max_diff(&live(&s, 9), &live(&s, 3)) > 16, "the test needs frames that differ");
    let d = max_diff(&live(&s, 9), &prev);
    assert!(d <= 4, "preview of frame 9 differs from the live frame 9 by {d}");
    // the files survive re-indexing the folder (as on reopening the project)
    s.previews.set_dir(s.previews.dir());
    assert_eq!(spans(&s)[1], (6, 12, Green));
    // In/Out over the whole sequence: only the two gaps around the part, and the second segment
    set_marks(&mut s, None);
    let v = s.execute("sequence.renderInToOut", json!({"wait": true})).unwrap();
    job_ok(&s, &v);
    assert_eq!((v["segments"].as_i64(), v["frames"].as_i64()), (Some(3), Some(18 + 24)), "{v}");
    assert_eq!(states(&s), vec!["green", "green"]);
    assert_eq!(s.previews.count(), 4);
    let prev = s.previews.frame(&s.media, &s.project, seq, 20, 1.0).expect("frame 20 has a preview").to_rgba8();
    assert!(max_diff(&live(&s, 20), &prev) <= 4);
    // Delete Render Files In to Out keeps the parts outside In/Out
    set_marks(&mut s, Some((7, 8)));
    let v = s.execute("sequence.deleteRenderFilesInToOut", json!({})).unwrap();
    assert_eq!(v["deleted"], 1);
    assert_eq!(spans(&s), vec![(0, 6, Green), (6, 12, Yellow), (12, 48, Green)]);
    let _ = std::fs::remove_dir_all(s.previews.dir().unwrap());
}

#[test]
fn previews_survive_save_and_open() {
    let (mut s, a) = session("save");
    render_tint(&mut s, a);
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
fn saving_without_previews_creates_no_previews_folder() {
    // #2343: a project with nothing rendered leaves only the file it was asked to write
    let mut s = Session::default();
    let folder = tmp("save-empty");
    std::fs::create_dir_all(&folder).unwrap();
    let path = folder.join("Film.fcproj").to_string_lossy().to_string();
    s.execute("file.save", json!({"path": path})).unwrap();
    assert_eq!(s.previews.dir().unwrap(), folder.join("FilmCraft Previews").join("Film"));
    assert!(!folder.join("FilmCraft Previews").exists());
    // the first render creates it
    let (mut r, a) = session("save-empty-render");
    r.execute("file.save", json!({"path": path})).unwrap();
    assert!(!folder.join("FilmCraft Previews").exists());
    render_tint(&mut r, a);
    assert!(folder.join("FilmCraft Previews").join("Film").is_dir());
    assert_eq!(states(&r), vec!["green", "none"]);
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

/// #1220: Enter (Render Effects In to Out) renders red segments only, as in Premiere. Yellow
/// segments play in real time; rendering them made Enter on a 49-minute 4K sequence of plain phone
/// clips encode 126 000 frames of ProRes.
#[test]
fn enter_renders_only_red_segments() {
    use crate::previews::RenderMode;
    use filmcraft_render::preview::Need;
    assert!(RenderMode::EffectsInToOut.renders(Need::Render));
    assert!(!RenderMode::EffectsInToOut.renders(Need::Realtime));
    assert!(!RenderMode::EffectsInToOut.renders(Need::None));
    for need in [Need::None, Need::Realtime, Need::Render] {
        assert!(RenderMode::InToOut.renders(need));
        assert!(RenderMode::Selection.renders(need));
    }
    // a sequence with yellow and no-bar segments only: nothing to render, no job, no folder
    let (mut s, _) = session("enter-yellow");
    assert_eq!(states(&s), vec!["yellow", "none"]);
    let v = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap();
    assert!(v["job"].is_null(), "{v}");
    assert_eq!(states(&s), vec!["yellow", "none"]);
    assert_eq!(s.previews.count(), 0);
    assert!(!s.previews.dir().unwrap().exists(), "no previews folder for nothing rendered");
}

/// #1220 end to end: on a red and a yellow segment, Enter starts a job for the red one only.
#[test]
fn enter_renders_the_red_segment_and_leaves_the_yellow_one() {
    let mut s = Session::default();
    s.previews.set_dir(Some(tmp("enter-red")));
    let r = FrameRate::FPS_24;
    let mut p = (*s.project).clone();
    let matte = GeneratorSource::new(Generator::ColorMatte { color: [0.2, 0.5, 0.8, 1.0] }, 1920, 1080, r, Tick(TICKS_PER_SECOND));
    let matte = crate::demo::add_generator(&mut p, &s.media, matte, "Matte", Label::Iris, None);
    // 1080p, where four heavy effects cost more than a frame lasts (red) and a Tint does not
    let seq = p.new_sequence("Seq", SequenceSettings { width: 1920, height: 1080, frame_rate: r, ..Default::default() }, 1, 1, None);
    let clip = |p: &mut filmcraft_project::Project, at: i64, fx: &[&str]| {
        let mut it = p.make_track_item(matte, TrackKind::Video, r.tick_of(at), TimeRange::new(r.tick_of(at), r.tick_of(2)), r).unwrap();
        for e in &mut it.effects {
            filmcraft_project::resolve_auto_points(e, (1920, 1080), (1920, 1080));
        }
        it.effects.extend(fx.iter().map(|f| find_effect(f).unwrap().instance()));
        it
    };
    let red = clip(&mut p, 0, &["gaussian_blur", "sharpen", "lumetri", "noise"]);
    let yellow = clip(&mut p, 2, &["tint"]);
    p.sequence_mut(seq).unwrap().video_tracks[0].items = vec![red, yellow];
    s.project = Arc::new(p);
    s.state.active_sequence = Some(seq);
    s.state.open_sequences = vec![seq];
    assert_eq!(states(&s), vec!["red", "yellow"]);
    let v = s.execute("sequence.renderEffectsInToOut", json!({"wait": true})).unwrap();
    job_ok(&s, &v);
    assert_eq!((v["segments"].as_i64(), v["frames"].as_i64()), (Some(1), Some(2)), "{v}");
    assert_eq!(states(&s), vec!["green", "yellow"]);
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

/// #2185: preview files are read in place, never whole. Rendered previews still play from their
/// files, and a file only a whole-file reader could open is refused rather than loaded.
#[test]
fn preview_files_are_streamed_not_read_whole() {
    let (mut s, a) = session("stream");
    render_tint(&mut s, a);
    let seq = s.state.active_sequence.unwrap();
    assert!(s.previews.frame(&s.media, &s.project, seq, 5, 1.0).is_some(), "a rendered frame plays from its file");
    // a still image opens only from its whole bytes: as a preview file it is not opened
    let dir = s.previews.dir().unwrap();
    let png = dir.join("still.png");
    // a 1×1 PNG
    const PNG: &[u8] = &[
        137, 80, 78, 71, 13, 10, 26, 10, 0, 0, 0, 13, 73, 72, 68, 82, 0, 0, 0, 1, 0, 0, 0, 1, 8, 6, 0, 0, 0, 31, 21, 196, 137, 0, 0, 0, 13, 73, 68, 65, 84,
        120, 218, 99, 100, 96, 248, 95, 15, 0, 2, 135, 1, 128, 235, 71, 186, 146, 0, 0, 0, 0, 73, 69, 78, 68, 174, 66, 96, 130,
    ];
    std::fs::write(&png, PNG).unwrap();
    assert!(s.media.open_bytes("still.png", PNG.into()).is_ok(), "the image opens from its bytes");
    // (the web, without a file reader, still reads preview files whole)
    if cfg!(any(unix, windows)) {
        assert!(crate::previews::open_preview_file(&s.media, "still.png", &png).is_none());
    }
    let _ = std::fs::remove_dir_all(dir);
}

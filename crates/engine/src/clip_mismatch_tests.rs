//! Settings ▸ Timeline ▸ Show Clip Mismatch Warning dialog: [`crate::commands::clip_mismatch`] and
//! `matchSequence` on `timeline.place`, `source.insert` and `source.overwrite`.

use filmcraft_media::MediaKind;
use filmcraft_project::{ItemId, ItemKind};
use filmcraft_time::FrameRate;
use serde_json::json;

use crate::Session;
use crate::commands::clip_mismatch;

/// A movie of the demo project (23.976 fps, 1920 x 1080).
fn demo_movie(s: &Session, not: Option<ItemId>) -> ItemId {
    s.project
        .items
        .values()
        .find(|i| Some(i.id) != not && i.has_video() && i.has_audio() && i.as_media().is_some_and(|m| m.info.kind == MediaKind::Movie))
        .map(|i| i.id)
        .unwrap()
}

/// The demo project with a new, empty sequence open (1920 x 1080, 23.976 fps), and one of the
/// demo's movies made to play at 120 fps, like a phone or game capture.
fn empty_sequence_and_120fps_movie() -> (Session, ItemId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let movie = demo_movie(&s, None);
    s.edit("120 fps", |pr, _| {
        if let Some(ItemKind::Media(m)) = pr.item_mut(movie).map(|i| &mut i.kind)
            && let Some(v) = m.info.video.as_mut()
        {
            v.frame_rate = FrameRate::FPS_120;
        }
        Ok(())
    })
    .unwrap();
    s.execute("file.newSequence", json!({"name": "Empty"})).unwrap();
    (s, movie)
}

fn settings(s: &Session) -> (u32, u32, FrameRate) {
    let q = s.active_sequence().unwrap();
    (q.settings.width, q.settings.height, q.settings.frame_rate)
}

fn clips(s: &Session) -> usize {
    s.active_sequence().unwrap().all_tracks().map(|t| t.items.len()).sum()
}

fn last_undo(s: &Session) -> Option<&str> {
    s.history.undo.last().map(|u| u.0.as_str())
}

#[test]
fn a_first_clip_that_differs_from_an_empty_sequence_mismatches() {
    let (mut s, movie) = empty_sequence_and_120fps_movie();
    assert_eq!(settings(&s), (1920, 1080, FrameRate::FPS_23_976));
    let st = clip_mismatch(&s, movie).expect("a 120 fps movie does not match a 23.976 fps sequence");
    assert_eq!((st.width, st.height, st.frame_rate), (1920, 1080, FrameRate::FPS_120));
    assert_eq!(st.preset, "1920x1080 120");
    assert_eq!(st.sample_rate, s.active_sequence().unwrap().settings.sample_rate, "the sequence's other settings stay");
    // once the sequence holds a clip, the next one is not asked about
    s.execute("timeline.place", json!({"item": movie.0, "time": 0})).unwrap();
    assert!(clip_mismatch(&s, movie).is_none());
}

#[test]
fn a_matching_movie_sound_a_still_and_a_sequence_do_not_mismatch() {
    let (mut s, movie) = empty_sequence_and_120fps_movie();
    assert!(clip_mismatch(&s, demo_movie(&s, Some(movie))).is_none(), "the demo's other movies match the sequence");
    let sound = s.project.items.values().find(|i| !i.has_video() && i.has_audio()).map(|i| i.id).unwrap();
    assert!(clip_mismatch(&s, sound).is_none());
    let sequence = s.project.items.values().find(|i| i.as_sequence().is_some()).map(|i| i.id).unwrap();
    assert!(clip_mismatch(&s, sequence).is_none());
    // a still of another rate: stills have no frame rate of their own to match
    s.edit("still", |pr, _| {
        if let Some(ItemKind::Media(m)) = pr.item_mut(movie).map(|i| &mut i.kind) {
            m.info.kind = MediaKind::Still;
        }
        Ok(())
    })
    .unwrap();
    assert!(clip_mismatch(&s, movie).is_none());
}

#[test]
fn match_sequence_takes_the_clips_settings_in_the_same_undo_step() {
    let (mut s, movie) = empty_sequence_and_120fps_movie();
    let steps = s.history.undo.len();
    let r = s.execute("timeline.place", json!({"item": movie.0, "time": 0, "matchSequence": true})).unwrap();
    assert_eq!(r["clips"].as_array().map(Vec::len), Some(2), "picture and sound: {r}");
    assert_eq!(settings(&s), (1920, 1080, FrameRate::FPS_120));
    assert_eq!(s.active_sequence().unwrap().settings.preset, "1920x1080 120");
    assert_eq!(s.history.undo.len(), steps + 1, "one undo step");
    assert_eq!(last_undo(&s), Some("Place Clip"));
    s.undo();
    assert_eq!(clips(&s), 0);
    assert_eq!(settings(&s), (1920, 1080, FrameRate::FPS_23_976), "the one undo puts the settings back too");
}

#[test]
fn without_match_sequence_the_settings_stay() {
    for params in [json!({"time": 0}), json!({"time": 0, "matchSequence": false})] {
        let (mut s, movie) = empty_sequence_and_120fps_movie();
        let mut p = params.clone();
        p["item"] = json!(movie.0);
        s.execute("timeline.place", p).unwrap();
        assert_eq!(clips(&s), 2, "{params}");
        assert_eq!(settings(&s), (1920, 1080, FrameRate::FPS_23_976), "{params}");
    }
}

#[test]
fn match_sequence_leaves_a_sequence_that_holds_clips_alone() {
    let (mut s, movie) = empty_sequence_and_120fps_movie();
    s.execute("timeline.place", json!({"item": demo_movie(&s, Some(movie)).0, "time": 0})).unwrap();
    s.execute("timeline.place", json!({"item": movie.0, "seconds": 60.0, "matchSequence": true})).unwrap();
    assert_eq!(settings(&s), (1920, 1080, FrameRate::FPS_23_976));
    assert_eq!(clips(&s), 4);
}

#[test]
fn match_sequence_on_items_insert_and_overwrite() {
    // `items`: the first item decides
    let (mut s, movie) = empty_sequence_and_120fps_movie();
    s.execute("timeline.place", json!({"items": [movie.0], "time": 0, "matchSequence": true})).unwrap();
    assert_eq!(settings(&s).2, FrameRate::FPS_120);
    assert_eq!(last_undo(&s), Some("Place Clips"));
    // Insert and Overwrite from the Source monitor
    for (command, label) in [("source.insert", "Insert"), ("source.overwrite", "Overwrite")] {
        let (mut s, movie) = empty_sequence_and_120fps_movie();
        s.execute("source.open", json!({"item": movie.0})).unwrap();
        s.execute(command, json!({"matchSequence": true})).unwrap();
        assert_eq!(settings(&s).2, FrameRate::FPS_120, "{command}");
        assert_eq!(clips(&s), 2, "{command}");
        assert_eq!(last_undo(&s), Some(label), "{command}");
        s.undo();
        assert_eq!((settings(&s).2, clips(&s)), (FrameRate::FPS_23_976, 0), "{command}");
    }
}

#[test]
fn the_setting_is_wired() {
    // the UI's Clip Mismatch Warning reads it, so Settings no longer lists it as kept only for parity
    assert!(crate::settings::field("timeline.clipMismatchWarning").is_some_and(|f| f.wired));
}

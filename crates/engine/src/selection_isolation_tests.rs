//! #1703: commands that create or copy clips select the individual clip the caller acted on, not
//! every clip sharing the same source time. An undo restores the pre-edit selection state, so it
//! never leaves ids from the razor edit behind.
use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn range(s: &Session, c: ClipId) -> (Tick, Tick) {
    s.active_sequence().unwrap().find_item(c).map(|(_, i)| (i.start, i.end())).unwrap()
}

fn source_in(s: &Session, c: ClipId) -> Tick {
    s.active_sequence().unwrap().find_item(c).unwrap().1.source_in
}

/// One 48-frame A/V clip: picture on V1, linked sound on A1.
fn av_clip(s: &mut Session) -> (ClipId, ClipId) {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Selection Isolation", "video": 3, "audio": 3})).unwrap();
    let rate = s.sequence_rate();
    let item = s
        .project
        .items
        .values()
        .find(|i| i.has_video() && i.has_audio() && i.as_media().is_some_and(|m| m.info.audio_streams.len() == 1))
        .map(|i| i.id)
        .unwrap();
    s.execute("timeline.place", json!({"item": item.0, "frame": 0, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
    let q = s.active_sequence().unwrap();
    (q.video_tracks[0].items[0].id, q.audio_tracks[0].items[0].id)
}

/// One original picture on V1 with its linked sound on A1, and the original itself placed again on
/// V2 by `timeline.move`'s copy mode (the same action under test).
fn prepare_stacked_copy(s: &mut Session) -> (ClipId, ClipId) {
    let (v1, _a1) = av_clip(s);
    let start = range(s, v1).0;
    let rate = s.sequence_rate();
    let track = s.active_sequence().unwrap().video_tracks[1].id;
    let r = s.execute("timeline.move", json!({"moves": [{"clip": v1.0, "track": track.0, "time": (start + rate.tick_of(120)).0}], "copy": true})).unwrap();
    let copy = r["copied"].as_array().and_then(|a| a.first()).and_then(|x| x.as_u64()).map(ClipId).unwrap();
    s.state.selection.clear();
    (v1, copy)
}

/// Razor at frame 24. Return every surviving right-half id introduced at that cut.
fn right_segments(s: &Session) -> Vec<ClipId> {
    let t = s.active_sequence().unwrap().frame_rate().tick_of(24);
    s.active_sequence().unwrap().all_tracks().flat_map(|tr| tr.items.iter().filter(|i| i.start == t)).map(|i| i.id).collect()
}

/// #1703: `timeline.razor` selects the individual new right segment(s), not the pre-razor whole/linked pair.
#[test]
fn razor_selects_only_right_segment_and_linked_partner() {
    let mut s = Session::default();
    let (v, a) = av_clip(&mut s);
    assert!(s.state.linked_selection);
    let rate = s.sequence_rate();
    let r = s.execute("timeline.razor", json!({"clip": v.0, "time": rate.tick_of(24).0})).unwrap();
    assert_eq!(r["cuts"], json!(2), "{r}");
    let right = right_segments(&s);
    assert_eq!(right.len(), 2, "linked selection cuts and selects each stream's right half");
    assert_eq!(s.state.selection, right, "a razor must select only each stream's new right half");
    let original_whole = [v, a].into_iter().collect::<Vec<_>>();
    assert!(right.iter().all(|id| !original_whole.contains(id)), "the right halves have their own clip ids");
}

/// #1703: linked selection off, a razor applies only to the asked track and selects only that track's segment.
#[test]
fn razor_without_linked_selection_selects_one_segment() {
    let mut s = Session::default();
    let (_v, a) = av_clip(&mut s);
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let rate = s.sequence_rate();
    let r = s.execute("timeline.razor", json!({"track": "V1", "time": rate.tick_of(24).0})).unwrap();
    assert_eq!(r["cuts"], json!(1), "{r}");
    let q = s.active_sequence().unwrap();
    let right_video = q.video_tracks[0].items.iter().find(|i| i.start == rate.tick_of(24)).map(|i| i.id).unwrap();
    assert_eq!(s.state.selection, vec![right_video], "unlinked razor selects only the new right picture segment");
    assert_eq!(q.audio_tracks[0].items.len(), 1);
    assert_eq!(range(&s, a), (Tick::ZERO, rate.tick_of(48)));
}

/// #1703: an overwrite copy onto a stacked track selects the copy, not the clicked source under it.
#[test]
fn stacked_copy_selects_only_clicked_track() {
    let mut s = Session::default();
    let (v1, stacked) = prepare_stacked_copy(&mut s);
    let track = s.active_sequence().unwrap().video_tracks[1].id;
    let start = range(&s, v1).0;
    let rate = s.sequence_rate();
    let r = s.execute("timeline.move", json!({"moves": [{"clip": v1.0, "track": track.0, "time": (start + rate.tick_of(120)).0}], "copy": true})).unwrap();
    let copied: Vec<ClipId> = r["copied"].as_array().unwrap().iter().filter_map(|x| x.as_u64().map(ClipId)).collect();
    assert_ne!(copied, vec![], "{r}");
    let copy = copied[0];
    assert_ne!(copy, v1);
    assert_ne!(copy, stacked);
    assert_eq!(range(&s, copy).1 - range(&s, copy).0, range(&s, v1).1 - range(&s, v1).0);
    assert_eq!(source_in(&s, copy), source_in(&s, v1));
    assert!(s.state.selection.contains(&copy), "copy must include the newly copied picture, not the clicked original");
    assert!(s.state.selection.iter().all(|c| copied.contains(c)), "copy selects only the newly copied clip(s), not the clicked original");
    assert!(!s.active_sequence().unwrap().video_tracks[1].items.is_empty());
}
/// #1703: undo must not leave dangling right-half ids after restoring the whole A/V clip.
#[test]
fn undo_after_razor_selects_valid_replacement_segments() {
    let mut s = Session::default();
    let (v, _a) = av_clip(&mut s);
    let rate = s.sequence_rate();
    s.execute("timeline.razor", json!({"clip": v.0, "time": rate.tick_of(24).0})).unwrap();
    assert_eq!(s.state.selection.len(), 2);
    s.execute("edit.undo", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks[0].items.len(), 1);
    assert_eq!(q.audio_tracks[0].items.len(), 1);
    assert!(s.state.selection.is_empty(), "undo restores the pre-razor selection instead of dangling right-half ids");
}

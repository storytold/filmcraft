//! Select Forward / Select Backward (Edit menu): all clips and captions from or up to the
//! playhead.

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_time::{Tick, TimeRange};
use serde_json::json;

/// A 24 fps-style sequence with two adjacent 48-frame video clips, `[0, 48)` and `[48, 96)`, and
/// two captions, `[12, 48)` and `[60, 96)`.
fn setup() -> (Session, Vec<ClipId>, Vec<ClipId>, Tick) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let item = s.project.items.values().find(|i| i.has_video() && i.as_media().is_some()).unwrap().id;
    s.execute("file.newSequence", json!({"name": "Select range", "video": 1, "audio": 0})).unwrap();
    let rate = s.sequence_rate();
    let mid = rate.tick_of(48);
    for frame in [0, 48] {
        s.execute("timeline.place", json!({"item": item.0, "frame": frame, "duration": mid.0})).unwrap();
    }
    let d36 = rate.tick_of(36).seconds();
    for (frame, text) in [(12, "A"), (60, "B")] {
        s.execute("captions.add", json!({"text": text, "frame": frame, "durationSeconds": d36})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    let clips: Vec<ClipId> = q.video_tracks[0].items.iter().map(|i| i.id).collect();
    let caps: Vec<ClipId> = q.caption_tracks[0].captions.iter().map(|c| c.id).collect();
    assert_eq!((clips.len(), caps.len()), (2, 2));
    // the ranges the commands are judged against
    assert_eq!(q.find_item(clips[1]).unwrap().1.start, mid);
    assert_eq!(q.caption_tracks[0].captions[0].end(), mid);
    assert_eq!(q.caption_tracks[0].captions[1].start, rate.tick_of(60));
    (s, clips, caps, mid)
}

#[test]
fn select_forward_takes_what_starts_at_or_after_the_playhead() {
    let (mut s, clips, caps, mid) = setup();
    let rate = s.sequence_rate();
    // from the start: everything
    s.execute("edit.selectForward", json!({})).unwrap();
    assert_eq!(s.state.selection, clips);
    assert_eq!(s.state.caption_selection, caps);
    // from the middle: the second clip and caption only
    s.execute("playhead.set", json!({"time": mid.0})).unwrap();
    let r = s.execute("edit.selectForward", json!({})).unwrap();
    assert_eq!(s.state.selection, vec![clips[1]], "{r}");
    assert_eq!(s.state.caption_selection, vec![caps[1]]);
    // from the end: nothing
    s.execute("playhead.set", json!({"time": rate.tick_of(96).0})).unwrap();
    let r = s.execute("edit.selectForward", json!({})).unwrap();
    assert_eq!(r, json!({"clips": 0, "captions": 0}));
    assert!(s.state.selection.is_empty());
    assert!(s.state.caption_selection.is_empty());
}

#[test]
fn select_backward_takes_what_ends_at_or_before_the_playhead() {
    let (mut s, clips, caps, mid) = setup();
    let rate = s.sequence_rate();
    s.execute("playhead.set", json!({"time": mid.0})).unwrap();
    let r = s.execute("edit.selectBackward", json!({})).unwrap();
    assert_eq!(s.state.selection, vec![clips[0]], "{r}");
    assert_eq!(s.state.caption_selection, vec![caps[0]]);
    // from the end: everything; from the start: nothing
    s.execute("playhead.set", json!({"time": rate.tick_of(96).0})).unwrap();
    s.execute("edit.selectBackward", json!({})).unwrap();
    assert_eq!(s.state.selection, clips);
    assert_eq!(s.state.caption_selection, caps);
    s.execute("playhead.set", json!({"time": 0})).unwrap();
    s.execute("edit.selectBackward", json!({})).unwrap();
    assert!(s.state.selection.is_empty());
    assert!(s.state.caption_selection.is_empty());
}

#[test]
fn both_replace_both_selections_and_clear_the_gap_selection() {
    let (mut s, clips, caps, mid) = setup();
    let rate = s.sequence_rate();
    // a stale selection of everything, plus a gap selection from an earlier click
    s.state.selection = clips.clone();
    s.state.caption_selection = caps.clone();
    let gap = s.active_sequence().unwrap().video_tracks[0].id;
    s.state.gap_selection = Some(crate::GapSelection { track: gap, range: TimeRange::from_bounds(mid, rate.tick_of(60)) });
    s.execute("playhead.set", json!({"time": mid.0})).unwrap();
    s.execute("edit.selectForward", json!({})).unwrap();
    assert_eq!(s.state.selection, vec![clips[1]], "the stale selection is replaced");
    assert_eq!(s.state.caption_selection, vec![caps[1]]);
    assert!(s.state.gap_selection.is_none());
}

#[test]
fn an_explicit_time_overrides_the_playhead() {
    let (mut s, clips, caps, mid) = setup();
    s.execute("playhead.set", json!({"time": 0})).unwrap();
    // forward from the middle, without moving the playhead
    s.execute("edit.selectForward", json!({"time": mid.0})).unwrap();
    assert_eq!(s.state.selection, vec![clips[1]]);
    assert_eq!(s.state.caption_selection, vec![caps[1]]);
    // backward from the middle too
    s.execute("edit.selectBackward", json!({"time": mid.0})).unwrap();
    assert_eq!(s.state.selection, vec![clips[0]]);
    assert_eq!(s.state.caption_selection, vec![caps[0]]);
    // the playhead itself did not move
    assert_eq!(s.playhead(), filmcraft_time::Tick::ZERO);
}

#[test]
fn empty_sequence_selects_nothing_and_a_cancelled_gap_selection_clears() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Empty", "video": 1, "audio": 0})).unwrap();
    let r = s.execute("edit.selectForward", json!({})).unwrap();
    assert_eq!(r, json!({"clips": 0, "captions": 0}));
    let r = s.execute("edit.selectBackward", json!({})).unwrap();
    assert_eq!(r, json!({"clips": 0, "captions": 0}));
}

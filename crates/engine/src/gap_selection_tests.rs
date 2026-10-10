//! Gaps between clips: select one (a click on the empty space before a clip), close it with Clear
//! or Ripple Delete as Premiere does, and Go to Sequence End on the edge after the last frame.

use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn start_of(s: &Session, c: ClipId) -> Tick {
    s.active_sequence().unwrap().find_item(c).map(|(_, i)| i.start).unwrap()
}

/// Four 48-frame clips in a row (pictures on V1, linked sounds on A1), then the second removed:
/// a 48-frame gap on V1 and A1 from frame 48 to 96. Returns the clips after the gap.
fn gap_sequence(s: &mut Session) -> [(ClipId, ClipId); 2] {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Gaps", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    for n in 0..4 {
        s.execute("timeline.place", json!({"item": item.0, "frame": 48 * n, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    let second = q.video_tracks[0].items[1].id;
    let after = [2, 3].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id));
    s.execute("timeline.select", json!({"clips": [second.0]})).unwrap();
    s.execute("edit.clear", json!({})).unwrap();
    after
}

#[test]
fn a_selected_gap_closes_with_clear_and_ripple_delete() {
    let mut s = Session::default();
    let after = gap_sequence(&mut s);
    let rate = s.sequence_rate();
    let (v3, a3) = after[0];
    assert_eq!(start_of(&s, v3), rate.tick_of(96));
    // a click in the gap on V1
    let r = s.execute("timeline.selectGap", json!({"track": "V1", "frame": 70})).unwrap();
    assert_eq!((r["start"].as_i64(), r["end"].as_i64()), (Some(rate.tick_of(48).0), Some(rate.tick_of(96).0)), "{r}");
    let v1 = s.active_sequence().unwrap().video_tracks[0].id;
    assert_eq!(s.selected_gap(), Some((v1, rate.tick_of(48), rate.tick_of(96))));
    assert_eq!(s.execute("sequence.inspect", json!({})).unwrap()["gap"]["start"], json!(rate.tick_of(48).0));
    assert!(s.is_enabled("edit.clear"), "Clear works on a selected gap");
    // Clear (Delete / Backspace) closes it: everything after moves up on V1 and A1
    let r = s.execute("edit.clear", json!({})).unwrap();
    assert!(r["closedGap"].is_object(), "{r}");
    assert_eq!(start_of(&s, v3), rate.tick_of(48));
    assert_eq!(start_of(&s, a3), rate.tick_of(48), "the sync-locked sound track follows");
    assert_eq!(start_of(&s, after[1].0), rate.tick_of(96));
    assert!(s.selected_gap().is_none() && s.state.gap_selection.is_none(), "the gap is gone");
    // one undo step brings it back
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(start_of(&s, v3), rate.tick_of(96));
    // Ripple Delete closes a selected gap too
    s.execute("timeline.selectGap", json!({"track": "A1", "frame": 50})).unwrap();
    s.execute("edit.rippleDelete", json!({})).unwrap();
    assert_eq!(start_of(&s, v3), rate.tick_of(48));
}

#[test]
fn gap_selection_rules() {
    let mut s = Session::default();
    let after = gap_sequence(&mut s);
    let rate = s.sequence_rate();
    // not a gap: on a clip, or past the last clip (nothing follows to close up)
    assert!(s.execute("timeline.selectGap", json!({"track": "V1", "frame": 10})).is_err());
    assert!(s.execute("timeline.selectGap", json!({"track": "V1", "frame": 400})).is_err());
    assert!(s.execute("timeline.selectGap", json!({"track": "V9", "frame": 70})).is_err());
    assert!(s.execute("timeline.selectGap", json!({"frame": 70})).is_err(), "needs a track");
    // selecting a clip replaces the gap; selecting the gap replaces the clips
    s.execute("timeline.selectGap", json!({"track": "V1", "frame": 70})).unwrap();
    s.execute("timeline.select", json!({"clips": [after[0].0.0]})).unwrap();
    assert!(s.selected_gap().is_none());
    s.execute("timeline.selectGap", json!({"track": "V1", "frame": 70})).unwrap();
    assert!(s.state.selection.is_empty() && s.selected_gap().is_some());
    s.execute("edit.deselectAll", json!({})).unwrap();
    assert!(s.selected_gap().is_none());
    assert!(!s.is_enabled("edit.clear"), "nothing to clear");
    // an edit that fills the gap leaves nothing selected
    s.execute("timeline.selectGap", json!({"track": "V1", "frame": 70})).unwrap();
    s.execute("edit.undo", json!({})).unwrap(); // the removed clip comes back into the gap
    assert!(s.selected_gap().is_none());
    assert!(s.execute("edit.clear", json!({})).is_err(), "Clear has nothing to act on");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(start_of(&s, after[0].0), rate.tick_of(96));
    // a gap on a locked track can't be selected
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    assert!(s.execute("timeline.selectGap", json!({"track": "V1", "frame": 70})).is_err());
}

/// Premiere does not ripple a gap away when a sync-locked track has a clip across it (a music
/// bed): Clear says why and changes nothing. Without sync lock on that track it closes.
#[test]
fn a_clip_across_the_gap_on_a_sync_locked_track_blocks_it() {
    let mut s = Session::default();
    let after = gap_sequence(&mut s);
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_audio() && !i.has_video() && i.as_media().is_some()).map(|i| i.id).unwrap();
    s.execute("sequence.addTracks", json!({"video": 0, "audio": 1})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "track": "A2", "frame": 0, "duration": rate.tick_of(150).0})).unwrap();
    s.execute("timeline.selectGap", json!({"track": "V1", "frame": 70})).unwrap();
    let e = s.execute("edit.clear", json!({})).unwrap_err().to_string();
    assert!(e.contains("A2") && e.contains("sync"), "{e}");
    assert_eq!(start_of(&s, after[0].0), rate.tick_of(96), "nothing moved");
    assert!(s.selected_gap().is_some(), "the gap stays selected");
    s.execute("timeline.setTrack", json!({"track": "A2", "syncLock": false})).unwrap();
    s.execute("edit.clear", json!({})).unwrap();
    assert_eq!(start_of(&s, after[0].0), rate.tick_of(48));
}

#[test]
fn go_to_sequence_end_is_after_the_last_frame() {
    let mut s = Session::default();
    let after = gap_sequence(&mut s);
    let rate = s.sequence_rate();
    s.execute("playhead.end", json!({})).unwrap();
    assert_eq!(s.playhead(), rate.tick_of(192), "flush with the end of the last clip");
    // a clip ending mid-frame: the end rounds up to the next frame edge, never into the clip
    let last = after[1].1;
    s.edit_sequence("test", |q, _, _| {
        let (_, it) = q.find_item_mut(last).unwrap();
        it.duration += Tick(rate.frame_duration().0 / 3);
        Ok(())
    })
    .unwrap();
    let d = s.active_sequence().unwrap().duration();
    s.execute("playhead.end", json!({})).unwrap();
    assert!(s.playhead() >= d, "{:?} < {d:?}", s.playhead());
    assert_eq!(s.playhead(), rate.tick_of(193));
}

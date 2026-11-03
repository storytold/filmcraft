//! #1357: `edit.rippleDeleteHead` removes the portion of the selected clip from its in point
//! through the playhead and ripples the sequence closed.

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

fn f(s: &Session, n: i64) -> Tick {
    s.sequence_rate().tick_of(n)
}

fn range(s: &Session, c: ClipId) -> (Tick, Tick) {
    s.active_sequence().unwrap().find_item(c).map(|(_, i)| (i.start, i.end())).unwrap()
}

/// Four 48-frame clips in a row, pictures on V1 and their linked sounds on A1: [(picture, sound); 4].
fn four_clips(s: &mut Session) -> [(ClipId, ClipId); 4] {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Head", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    for n in 0..4 {
        s.execute("timeline.place", json!({"item": item.0, "frame": 48 * n, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    [0, 1, 2, 3].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id))
}

/// The head (in point .. playhead) goes and the rest of the sequence closes up: the clip keeps its
/// tail at the left of its old range, later clips shift left.
#[test]
fn ripple_delete_head_closes_the_head_and_shifts_later_clips() {
    let mut s = Session::default();
    let [(_, _), (pic2, snd2), (pic3, snd3), (pic4, snd4)] = four_clips(&mut s);
    s.set_playhead(f(&s, 60));
    s.state.selection = vec![pic2];

    s.execute("edit.rippleDeleteHead", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert_eq!(range(&s, pic2), (f(&s, 48), f(&s, 84)), "the head is gone and the tail slid left");
    assert_eq!(range(&s, snd2), (f(&s, 48), f(&s, 84)), "the linked sound follows");
    assert_eq!(range(&s, pic3), (f(&s, 84), f(&s, 132)), "the next picture closes up by the head length");
    assert_eq!((range(&s, snd3), range(&s, pic4), range(&s, snd4)), ((f(&s, 84), f(&s, 132)), (f(&s, 132), f(&s, 180)), (f(&s, 132), f(&s, 180))));
    assert_eq!(q.duration(), f(&s, 180), "the sequence is 12 frames shorter");
}

/// The trimmed clip's media in point advances by the head length (the head frames are dropped,
/// not hidden).
#[test]
fn ripple_delete_head_advances_the_source_in_point() {
    let mut s = Session::default();
    let [(pic1, _), ..] = four_clips(&mut s);
    let in_before = s.active_sequence().unwrap().find_item(pic1).unwrap().1.source_in;
    s.set_playhead(f(&s, 12));
    s.state.selection = vec![pic1];
    s.execute("edit.rippleDeleteHead", json!({})).unwrap();
    let item = s.active_sequence().unwrap().find_item(pic1).unwrap().1;
    assert_eq!(item.source_in, in_before + f(&s, 12), "the head frames are dropped from the source");
    assert_eq!((item.start, item.end()), (Tick::ZERO, f(&s, 36)), "the clip starts at zero, 36 frames long");
}

/// The playhead at or before the in point has no head to remove.
#[test]
fn ripple_delete_head_needs_the_playhead_past_the_in_point() {
    let mut s = Session::default();
    let [(pic1, _), ..] = four_clips(&mut s);
    s.set_playhead(Tick::ZERO);
    s.state.selection = vec![pic1];
    s.execute("edit.rippleDeleteHead", json!({})).unwrap_err();
    assert_eq!(range(&s, pic1), (Tick::ZERO, s.sequence_rate().tick_of(48)), "nothing moved");
}

/// The playhead past the clip's out point deletes the whole clip (the head covers all of it).
#[test]
fn ripple_delete_head_past_the_end_removes_the_whole_clip() {
    let mut s = Session::default();
    let [(_, _), (pic2, snd2), (pic3, snd3), ..] = four_clips(&mut s);
    s.set_playhead(f(&s, 120)); // past clip 2's out point at 96
    s.state.selection = vec![pic2];
    s.execute("edit.rippleDeleteHead", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert!(q.find_item(pic2).is_none() && q.find_item(snd2).is_none(), "the whole clip went");
    assert_eq!(range(&s, pic3), (f(&s, 48), f(&s, 96)), "later clips closed the whole 48-frame gap");
    assert_eq!(range(&s, snd3), (f(&s, 48), f(&s, 96)));
}

/// With Ripple Sequence Markers on, markers after the removed head move back with the clips, and
/// markers inside the head go with it.
#[test]
fn ripple_delete_head_ripples_sequence_markers() {
    let mut s = Session::default();
    let [(_, _), (pic2, _), (_, _), (_, _)] = four_clips(&mut s);
    s.state.ripple_sequence_markers = true;
    let at = |s: &Session, n: i64| s.sequence_rate().tick_of(n).0;
    for (frame, name) in [(30, "before"), (52, "inside"), (120, "after")] {
        s.execute("markers.add", json!({"time": at(&s, frame), "name": name})).unwrap();
    }
    s.set_playhead(f(&s, 60));
    s.state.selection = vec![pic2];

    s.execute("edit.rippleDeleteHead", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    let markers: Vec<(String, Tick)> = q.markers.iter().map(|m| (m.name.clone(), m.start)).collect();
    assert_eq!(markers, vec![("before".to_string(), f(&s, 30)), ("after".to_string(), f(&s, 108))], "the 12-frame head closes");
}

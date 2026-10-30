//! Add Marker (M) with timeline clips selected marks those clips, as in Premiere (#3383): the
//! marker goes on each selected clip under the playhead, at the media time shown there.

use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s
}

/// The second clip of V1 and a playhead a few frames into it.
fn second_clip_and_inside(s: &Session) -> (ClipId, Tick) {
    let c = s.active_sequence().unwrap().video_tracks[0].items[1].clone();
    (c.id, c.start + s.sequence_rate().tick_of(5))
}

fn clip_markers(s: &Session, id: ClipId) -> Vec<(Tick, String)> {
    let q = s.active_sequence().unwrap();
    let c = q.all_tracks().flat_map(|t| t.items.iter()).find(|c| c.id == id).unwrap();
    c.markers.iter().map(|m| (m.start, m.name.clone())).collect()
}

#[test]
fn add_marker_marks_the_selected_clip_at_the_media_time_under_the_playhead() {
    let mut s = demo();
    let (clip, at) = second_clip_and_inside(&s);
    let sequence_markers = s.active_sequence().unwrap().markers.len();
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    s.set_playhead(at);

    let r = s.execute("markers.add", json!({"name": "Beat"})).unwrap();

    let c = s.active_sequence().unwrap().video_tracks[0].items[1].clone();
    assert_eq!(clip_markers(&s, clip), vec![(c.moving_source_time_at(at), "Beat".to_string())]);
    assert_eq!(s.active_sequence().unwrap().markers.len(), sequence_markers, "no sequence marker");
    let id = c.markers[0].id.0;
    assert_eq!(r["clipMarkers"], json!([{"clip": clip.0, "marker": id}]), "{r}");
    // one undo step takes it off again
    s.execute("edit.undo", json!({})).unwrap();
    assert!(clip_markers(&s, clip).is_empty());
}

#[test]
fn add_marker_on_a_sped_up_clip_lands_on_the_media_frame_shown() {
    let mut s = demo();
    let (clip, _) = second_clip_and_inside(&s);
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    s.execute("clip.speedDuration", json!({"speed": 200})).unwrap();
    let c = s.active_sequence().unwrap().video_tracks[0].items[1].clone();
    let at = c.start + s.sequence_rate().tick_of(4);
    s.set_playhead(at);

    s.execute("markers.add", json!({})).unwrap();

    let start = clip_markers(&s, clip)[0].0;
    assert_eq!(start, c.source_in + s.sequence_rate().tick_of(8), "4 frames at 200 % show media frame 8");
}

#[test]
fn add_marker_marks_every_selected_clip_under_the_playhead_and_skips_the_others() {
    let mut s = demo();
    let (clip, at) = second_clip_and_inside(&s);
    let (first, under) = {
        let q = s.active_sequence().unwrap();
        let under = q.audio_tracks.iter().flat_map(|t| t.items.iter()).find(|c| c.range().contains(at)).map(|c| c.id).unwrap();
        (q.video_tracks[0].items[0].id, under)
    };
    s.execute("timeline.select", json!({"clips": [first.0, clip.0, under.0]})).unwrap();
    s.set_playhead(at);

    s.execute("markers.add", json!({})).unwrap();

    assert_eq!(clip_markers(&s, clip).len(), 1);
    assert_eq!(clip_markers(&s, under).len(), 1);
    assert!(clip_markers(&s, first).is_empty(), "the playhead is not over the first clip");
}

#[test]
fn add_marker_without_a_selected_clip_under_the_playhead_marks_the_sequence() {
    let mut s = demo();
    let (clip, at) = second_clip_and_inside(&s);
    let first = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    let before = s.active_sequence().unwrap().markers.len();
    // nothing selected
    s.execute("timeline.select", json!({"clips": []})).unwrap();
    s.set_playhead(at);
    s.execute("markers.add", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers.len(), before + 1);
    // a selected clip elsewhere
    s.execute("timeline.select", json!({"clips": [first.0]})).unwrap();
    s.execute("markers.add", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().markers.len(), before + 2);
    assert!(clip_markers(&s, clip).is_empty() && clip_markers(&s, first).is_empty());
}

#[test]
fn add_marker_at_an_explicit_time_marks_the_sequence_even_with_clips_selected() {
    // agents that pass a time, in any of its forms, keep getting sequence markers
    let mut s = demo();
    let (clip, at) = second_clip_and_inside(&s);
    let rate = s.sequence_rate();
    let before = s.active_sequence().unwrap().markers.len();
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    s.set_playhead(at);
    let given = [json!({"time": at.0}), json!({"frame": rate.frame_at(at)}), json!({"seconds": at.seconds()})];
    for (n, p) in given.iter().enumerate() {
        s.execute("markers.add", p.clone()).unwrap();
        assert_eq!(s.active_sequence().unwrap().markers.len(), before + n + 1, "{p}");
    }
    assert!(clip_markers(&s, clip).is_empty());
}

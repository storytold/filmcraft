//! #855: a rolling edit on a linked A/V cut must move the sound cut with the picture.

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

/// Two consecutive 48-frame A/V clips (picture on V1, linked sound on A1).
fn two_clips(s: &mut Session) -> [(ClipId, ClipId); 2] {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Roll", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    for n in 0..2 {
        s.execute("timeline.place", json!({"item": item.0, "frame": 48 * n, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    [0, 1].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id))
}

/// #855: rolling the V1 cut +12 frames used to leave A1, so export played the second clip's audio
/// under the first clip's picture.
#[test]
fn roll_moves_linked_audio_cut() {
    let mut s = Session::default();
    let [(v0, a0), (v1, a1)] = two_clips(&mut s);
    assert!(s.state.linked_selection);
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    let r = s.execute("timeline.roll", json!({"left": v0.0, "right": v1.0, "deltaFrames": 12})).unwrap();
    assert_eq!(r["linked"], json!(true), "{r}");
    assert_eq!(r["pairs"], json!(2), "{r}");
    assert_eq!(range(&s, v0), (f(0), f(60)));
    assert_eq!(range(&s, v1), (f(60), f(96)));
    assert_eq!(range(&s, a0), (f(0), f(60)), "linked sound must roll with the picture");
    assert_eq!(range(&s, a1), (f(60), f(96)));
}

/// Linked selection off (or unlinked clips) rolls only the given pair and says so.
#[test]
fn roll_without_linked_selection_leaves_audio() {
    let mut s = Session::default();
    let [(v0, a0), (v1, a1)] = two_clips(&mut s);
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    let r = s.execute("timeline.roll", json!({"left": v0.0, "right": v1.0, "deltaFrames": 12})).unwrap();
    assert_eq!(r["linked"], json!(false), "{r}: clips were not followed as a linked pair");
    assert_eq!(range(&s, v0), (f(0), f(60)));
    assert_eq!(range(&s, v1), (f(60), f(96)));
    assert_eq!(range(&s, a0), (f(0), f(48)), "unlinked A1 cut stays");
    assert_eq!(range(&s, a1), (f(48), f(96)));
}

/// #526/#558: reversed linked clips still keep the media on the unmoved timeline edges.
#[test]
fn roll_linked_reversed_clips_keeps_media_mapping() {
    let mut s = Session::default();
    let [(v0, a0), (v1, a1)] = two_clips(&mut s);
    s.execute("timeline.select", json!({"clips": [v0.0, a0.0, v1.0, a1.0]})).unwrap();
    s.execute("clip.speedDuration", json!({"speed": 100, "reverse": true})).unwrap();
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    let left_in = source_in(&s, v0);
    let right_in = source_in(&s, v1);
    s.execute("timeline.roll", json!({"left": v0.0, "right": v1.0, "deltaFrames": 12})).unwrap();
    assert_eq!(range(&s, v0), (f(0), f(60)));
    assert_eq!(range(&s, a0), (f(0), f(60)));
    assert_eq!(range(&s, v1), (f(60), f(96)));
    assert_eq!(range(&s, a1), (f(60), f(96)));
    // reversed left: source_in moves so the clip's In still shows the same media
    assert_eq!(source_in(&s, v0), left_in - f(12));
    assert_eq!(source_in(&s, a0), left_in - f(12));
    // reversed right: source_in stays so the clip's Out still shows the same media
    assert_eq!(source_in(&s, v1), right_in);
    assert_eq!(source_in(&s, a1), right_in);
}

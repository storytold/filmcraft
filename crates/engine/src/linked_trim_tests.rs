//! Trimming and rate-stretching one side of a linked clip (#3300): `"linked": false` (Alt held
//! in the Timeline) edits only the given clip, while the default still edits its partners too.

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

fn linked_pair() -> (Session, ClipId, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Linked trim", "video": 1, "audio": 1})).unwrap();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).unwrap().id;
    s.execute("timeline.place", json!({"item": item.0, "frame": 0, "duration": s.sequence_rate().tick_of(48).0})).unwrap();
    let q = s.active_sequence().unwrap();
    let (video, sound) = (q.video_tracks[0].items[0].id, q.audio_tracks[0].items[0].id);
    assert!(q.find_item(video).unwrap().1.link.is_some(), "the pair is linked");
    (s, video, sound)
}

fn state(s: &Session, id: ClipId) -> (Tick, Tick, Tick) {
    let (_, it) = s.active_sequence().unwrap().find_item(id).unwrap();
    (it.start, it.duration, it.source_in)
}

#[test]
fn unlinked_trim_leaves_the_partner_alone() {
    for mode in ["regular", "ripple"] {
        for edge in ["in", "out"] {
            let (mut s, video, sound) = linked_pair();
            let (before_v, before_a) = (state(&s, video), state(&s, sound));
            let delta = if edge == "in" { 10 } else { -10 };
            s.execute("timeline.trim", json!({"clip": video.0, "edge": edge, "mode": mode, "deltaFrames": delta, "linked": false})).unwrap();
            assert_eq!(state(&s, video).1, before_v.1 - s.sequence_rate().tick_of(10), "{mode} {edge}: the video is trimmed");
            assert_eq!(state(&s, sound), before_a, "{mode} {edge}: the linked audio keeps its range");
            assert!(s.state.linked_selection, "the global toggle stays on");

            // without the flag, the partner follows as before
            let (mut s, video, sound) = linked_pair();
            s.execute("timeline.trim", json!({"clip": video.0, "edge": edge, "mode": mode, "deltaFrames": delta})).unwrap();
            assert_eq!(state(&s, video).1, before_v.1 - s.sequence_rate().tick_of(10));
            assert_eq!(state(&s, sound).1, before_a.1 - s.sequence_rate().tick_of(10), "{mode} {edge}: the linked audio follows");
        }
    }
}

#[test]
fn unlinked_rate_stretch_leaves_the_partner_alone() {
    let (mut s, video, sound) = linked_pair();
    let (before_v, before_a) = (state(&s, video), state(&s, sound));
    s.execute("timeline.rateStretch", json!({"clip": video.0, "edge": "out", "deltaFrames": 12, "linked": false})).unwrap();
    assert_eq!(state(&s, video).1, before_v.1 + s.sequence_rate().tick_of(12));
    assert_eq!(state(&s, sound), before_a, "the linked audio keeps its range");

    let (mut s, video, sound) = linked_pair();
    s.execute("timeline.rateStretch", json!({"clip": video.0, "edge": "out", "deltaFrames": 12})).unwrap();
    assert_eq!(state(&s, video).1, before_v.1 + s.sequence_rate().tick_of(12));
    assert_eq!(state(&s, sound).1, before_a.1 + s.sequence_rate().tick_of(12), "the linked audio follows");
}

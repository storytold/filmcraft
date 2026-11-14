//! A transition plays the clips it joins past the cut, into their handles. Where a clip has no
//! media there the picture repeats a frame and the sound is silent, as Premiere's "insufficient
//! media" transitions do. The transition is applied as asked, and the result says in `short` which
//! clip edge has no media under it and for how many frames.

use filmcraft_project::ClipId;
use serde_json::{Value, json};

use crate::Session;

/// Two consecutive 48-frame A/V clips: the first ends on the last frame of its media and the second
/// starts on the first frame of its media (`handles` false), or both have 48 frames of media on
/// each side (`handles` true).
fn cut(s: &mut Session, handles: bool) -> [(ClipId, ClipId); 2] {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Handles", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    let media = crate::media_duration(&s.project, &s.media, item).unwrap();
    let source_in = |n: usize| match (handles, n) {
        (true, _) => rate.tick_of(48),
        (false, 0) => media - rate.tick_of(48),
        (false, _) => filmcraft_time::Tick::ZERO,
    };
    for n in 0..2 {
        s.execute("timeline.place", json!({"item": item.0, "frame": 48 * n, "sourceIn": source_in(n as usize).0, "duration": rate.tick_of(48).0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    [0, 1].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id))
}

fn short(clip: ClipId, edge: &str, frames: i64) -> Value {
    json!({"clip": clip.0, "edge": edge, "shortByFrames": frames})
}

#[test]
fn a_transition_past_the_media_says_which_clips_are_short() {
    let mut s = Session::default();
    let [(v0, a0), (v1, a1)] = cut(&mut s, false);
    let rate = s.sequence_rate();
    // centred on the cut, 12 frames on each side: none of them has media
    let r = s.execute("sequence.applyVideoTransition", json!({"clip": v1.0, "edge": "in", "frames": 24})).unwrap();
    assert_eq!(r["short"], json!([short(v0, "out", 12), short(v1, "in", 12)]), "{r}");
    let id = r["transition"].as_u64().unwrap();
    let tr = s.active_sequence().unwrap().video_tracks[0].transitions.iter().find(|t| t.id.0 == id).cloned().unwrap();
    assert_eq!((tr.start, tr.duration), (rate.tick_of(36), rate.tick_of(24)), "applied as asked");
    let r = s.execute("sequence.applyAudioTransition", json!({"clip": a1.0, "edge": "in", "frames": 24})).unwrap();
    assert_eq!(r["short"], json!([short(a0, "out", 12), short(a1, "in", 12)]), "{r}");
    // moved to start at the cut, the whole transition runs past the first clip's Out point
    let r = s.execute("sequence.setTransition", json!({"transition": id, "align": "start"})).unwrap();
    assert_eq!(r["short"], json!([short(v0, "out", 24)]), "{r}");
}

#[test]
fn a_transition_within_the_handles_is_not_short() {
    let mut s = Session::default();
    let [_, (v1, a1)] = cut(&mut s, true);
    let r = s.execute("sequence.applyVideoTransition", json!({"clip": v1.0, "edge": "in", "frames": 24})).unwrap();
    assert_eq!(r["short"], json!([]), "{r}");
    let r = s.execute("sequence.applyAudioTransition", json!({"clip": a1.0, "edge": "in", "frames": 48})).unwrap();
    assert_eq!(r["short"], json!([]), "{r}");
}

/// Applied to the selected clips, the transitions report together.
#[test]
fn transitions_applied_to_a_selection_say_which_clips_are_short() {
    let mut s = Session::default();
    let [(v0, _), (v1, _)] = cut(&mut s, false);
    s.execute("timeline.select", json!({"clips": [v0.0, v1.0], "linked": false})).unwrap();
    let r = s.execute("sequence.applyVideoTransition", json!({"frames": 24})).unwrap();
    assert_eq!(r["transitions"].as_array().map(Vec::len), Some(3), "{r}: a fade in, the cut and a fade out");
    assert_eq!(r["short"], json!([short(v0, "out", 12), short(v1, "in", 12)]), "{r}");
}

//! A trim, roll or extend that needs media a clip does not have stops at the media. The result
//! says how far each edge moved and, in `short`, which clip edge ran out of media and by how many
//! frames, so a script or agent can tell a blocked edit from one that happened.

use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::Session;

fn range(s: &Session, c: ClipId) -> (Tick, Tick) {
    s.active_sequence().unwrap().find_item(c).map(|(_, i)| (i.start, i.end())).unwrap()
}

/// Two consecutive 48-frame A/V clips that start at the first frame of their media, so neither
/// has media before its In point (plenty after its Out point).
fn no_head_handles(s: &mut Session) -> [(ClipId, ClipId); 2] {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Handles", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    for n in 0..2 {
        s.execute("timeline.place", json!({"item": item.0, "frame": 48 * n, "sourceIn": 0, "duration": rate.tick_of(48).0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    [0, 1].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id))
}

fn short(clip: ClipId, edge: &str, frames: i64) -> Value {
    json!({"clip": clip.0, "edge": edge, "shortByFrames": frames})
}

#[test]
fn a_roll_that_needs_a_missing_head_says_which_clip_is_short() {
    let mut s = Session::default();
    let [(v0, a0), (v1, a1)] = no_head_handles(&mut s);
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    // rolling the cut earlier needs 12 frames before the right clips' In points
    let r = s.execute("timeline.roll", json!({"left": v0.0, "right": v1.0, "deltaFrames": -12})).unwrap();
    assert_eq!(r["delta"], json!(0), "{r}");
    assert_eq!(r["short"], json!([short(v1, "in", 12), short(a1, "in", 12)]), "{r}");
    assert_eq!((range(&s, v0), range(&s, v1)), ((f(0), f(48)), (f(48), f(96))), "the cut stays");
    // later uses the left clips' tails, which the media has
    let r = s.execute("timeline.roll", json!({"left": v0.0, "right": v1.0, "deltaFrames": 12})).unwrap();
    assert_eq!(r["delta"], json!(f(12).0), "{r}");
    assert_eq!(r["short"], json!([]), "{r}");
    assert_eq!(range(&s, a0), (f(0), f(60)));
}

#[test]
fn a_trim_that_needs_a_missing_head_says_which_clip_is_short() {
    let mut s = Session::default();
    let [_, (v1, a1)] = no_head_handles(&mut s);
    let r = s.execute("timeline.trim", json!({"clip": v1.0, "edge": "in", "mode": "ripple", "deltaFrames": -12})).unwrap();
    assert_eq!(r["delta"], json!(0), "{r}");
    assert_eq!(r["short"], json!([short(v1, "in", 12), short(a1, "in", 12)]), "{r}");
    // shortening the head needs no media
    let r = s.execute("timeline.trim", json!({"clip": v1.0, "edge": "in", "mode": "ripple", "deltaFrames": 12})).unwrap();
    assert_eq!(r["short"], json!([]), "{r}");
}

/// Trim mode passes on what each roll or trim it ran was short of.
#[test]
fn a_blocked_nudge_says_which_clip_is_short() {
    let mut s = Session::default();
    let [(v0, _), (v1, a1)] = no_head_handles(&mut s);
    s.execute("trim.selectEditPoint", json!({"clip": v0.0, "edge": "out", "kind": "roll"})).unwrap();
    let r = s.execute("trim.nudge", json!({"frames": -5})).unwrap();
    assert_eq!(r["applied"], json!([0]), "{r}");
    assert_eq!(r["short"], json!([short(v1, "in", 5), short(a1, "in", 5)]), "{r}");
}

/// Extend Next Edit to Playhead with no media before the next clip moved nothing but reported
/// `edits: 2`.
#[test]
fn extend_edit_reports_the_edits_it_moved() {
    let mut s = Session::default();
    let [(v0, a0), (v1, a1)] = no_head_handles(&mut s);
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    // the cut at 48 rolled back to 30 needs 18 frames before the right clips' In points
    s.set_playhead(f(30));
    let r = s.execute("trim.extendNextEdit", json!({})).unwrap();
    assert_eq!(r["edits"], json!(0), "{r}");
    assert_eq!(r["applied"], json!([0, 0]), "{r}");
    assert_eq!(r["short"], json!([short(v1, "in", 18), short(a1, "in", 18)]), "{r}");
    assert_eq!((range(&s, v0), range(&s, a1)), ((f(0), f(48)), (f(48), f(96))), "nothing moved");
    // rolled forward to 60 it uses the left clips' tails
    s.set_playhead(f(60));
    let r = s.execute("trim.extendPreviousEdit", json!({})).unwrap();
    assert_eq!(r["edits"], json!(2), "{r}");
    assert_eq!(r["applied"], json!([f(12).0, f(12).0]), "{r}");
    assert_eq!(r["short"], json!([]), "{r}");
    assert_eq!((range(&s, v0), range(&s, a0)), ((f(0), f(60)), (f(0), f(60))));
}

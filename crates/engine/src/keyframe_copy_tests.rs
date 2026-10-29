//! Copy / cut / paste of selected keyframe points (#3562, #3319, #2582).

use filmcraft_project::Interpolation;
use serde_json::json;

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// First clip of the active sequence with its media time mapping.
fn first_clip(s: &Session) -> (filmcraft_project::ClipId, filmcraft_time::Tick, filmcraft_time::Tick) {
    let q = s.active_sequence().unwrap();
    let (id, it) = q.all_tracks().find_map(|t| t.items.first().map(|i| (i.id, i.clone()))).unwrap();
    (id, it.start, it.source_in)
}

fn scale_keys(s: &Session, clip: filmcraft_project::ClipId) -> Vec<(i64, f64, Interpolation)> {
    let q = s.active_sequence().unwrap();
    let (_, it) = q.find_item(clip).unwrap();
    it.effects
        .iter()
        .find(|e| e.effect == "motion")
        .and_then(|e| e.param("scale"))
        .map(|p| p.keyframes.iter().map(|k| (k.time.0, k.value.as_f64().unwrap(), k.interp)).collect())
        .unwrap()
}

/// Keyframe refs for `keys` of the motion scale param at media times.
fn refs(s: &Session, clip: filmcraft_project::ClipId, media: &[i64]) -> Vec<serde_json::Value> {
    let q = s.active_sequence().unwrap();
    let (_, it) = q.find_item(clip).unwrap();
    let idx = it.effects.iter().position(|e| e.effect == "motion").unwrap();
    media.iter().map(|t| json!({"clip": clip.0, "effect": idx, "param": "scale", "mediaTime": t})).collect()
}

/// Add a scale keyframe at a timeline time; returns the media time it landed on.
fn add_scale_key(s: &mut Session, clip: filmcraft_project::ClipId, timeline: i64) -> i64 {
    s.execute("effects.addKeyframe", json!({"clip": clip.0, "effect": "motion", "param": "scale", "time": timeline})).unwrap();
    scale_keys(s, clip).last().map(|k| k.0).unwrap()
}

#[test]
fn copy_paste_preserves_values_timing_and_interp() {
    let mut s = demo();
    let (clip, start, _) = first_clip(&s);
    let rate = s.sequence_rate();
    let fd = rate.frame_duration().0;
    // two scale keys thirty frames apart
    let t0 = add_scale_key(&mut s, clip, start.0);
    let t1 = add_scale_key(&mut s, clip, start.0 + fd * 30);
    for (t, v) in [(t0, 50.0), (t1, 150.0)] {
        s.execute("effects.setKeyframe", json!({"clip": clip.0, "effect": "motion", "param": "scale", "mediaTime": t, "value": v})).unwrap();
    }
    s.execute(
        "effects.setInterpolation",
        json!({"clip": clip.0, "effect": "motion", "param": "scale", "mediaTime": t1, "interpolation": "hold"}),
    )
    .unwrap();

    let r = s.execute("keyframes.copy", json!({"keys": refs(&s, clip, &[t0, t1])})).unwrap();
    assert_eq!(r["copied"], 2, "{r}");
    // copying keyframes clears the clip clipboard, so a plain Paste reaches them
    assert!(s.state.clipboard.is_empty());

    // paste sixty frames later in the clip
    let later = t0 + fd * 60;
    s.execute("keyframes.paste", json!({"time": later})).unwrap();
    let keys = scale_keys(&s, clip);
    assert_eq!(keys.len(), 4, "{keys:?}");
    assert_eq!((keys[2].0, keys[2].1), (later, 50.0));
    assert_eq!((keys[3].0, keys[3].1, keys[3].2), (later + (t1 - t0), 150.0, Interpolation::Hold));
    assert_eq!(keys[0].0, t0, "the originals are untouched");
}

#[test]
fn paste_defaults_to_the_playhead_and_is_one_undo_step() {
    let mut s = demo();
    let (clip, start, _) = first_clip(&s);
    let t0 = add_scale_key(&mut s, clip, start.0);
    let before = scale_keys(&s, clip);
    assert_eq!(before.len(), 1);
    s.execute("keyframes.copy", json!({"keys": refs(&s, clip, &[t0])})).unwrap();
    // playhead at the clip start: pasting there lands on the same key (replace, no duplicate)
    s.execute("playhead.set", json!({"time": start.0})).unwrap();
    let r = s.execute("keyframes.paste", json!({})).unwrap();
    assert_eq!(r["pasted"], 1, "{r}");
    assert_eq!(scale_keys(&s, clip).len(), 1, "pasting onto itself replaces");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(scale_keys(&s, clip), before, "one undo step removes the paste");
}

#[test]
fn cut_copies_and_removes() {
    let mut s = demo();
    let (clip, start, _) = first_clip(&s);
    let t0 = add_scale_key(&mut s, clip, start.0);
    let r = s.execute("keyframes.cut", json!({"keys": refs(&s, clip, &[t0])})).unwrap();
    assert_eq!(r["cut"], 1, "{r}");
    assert!(scale_keys(&s, clip).is_empty(), "the point is gone");
    s.execute("keyframes.paste", json!({"time": t0})).unwrap();
    assert_eq!(scale_keys(&s, clip).len(), 1, "the cut point pastes back");
}

#[test]
fn errors_and_clipboard_exclusion() {
    let mut s = demo();
    let (clip, start, _) = first_clip(&s);
    // nothing copied yet
    assert!(s.execute("keyframes.paste", json!({})).is_err(), "paste needs a copy first");
    assert!(s.execute("keyframes.copy", json!({"keys": []})).is_err(), "copy needs keys");
    assert!(
        s.execute("keyframes.copy", json!({"keys": refs(&s, clip, &[start.0])})).is_err(),
        "no keyframe at a timeline time passed as media time"
    );
    // copying clips clears the keyframe clipboard: Paste goes back to the clips
    let t0 = add_scale_key(&mut s, clip, start.0);
    s.execute("keyframes.copy", json!({"keys": refs(&s, clip, &[t0])})).unwrap();
    assert_eq!(s.state.keyframe_clipboard.len(), 1);
    s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    assert!(s.state.keyframe_clipboard.is_empty(), "clip copy supersedes keyframes");
    assert!(!s.state.clipboard.is_empty());
    // and captions supersede them too
    s.execute("keyframes.copy", json!({"keys": refs(&s, clip, &[t0])})).unwrap();
    crate::captions::copy(&mut s);
    assert!(s.state.keyframe_clipboard.is_empty(), "caption copy supersedes keyframes");
}

//! Copy and paste of keyframes (#3562): clip Volume keys and track automation keys keep their
//! values and spacing, the first lands at the playhead on the same parameter of the target.

use filmcraft_project::mixer::{LANE_PAN, LANE_VOLUME, MASTER_STRIP};
use filmcraft_project::{ClipId, ParamValue, TrackItem};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::json;

use crate::Session;

/// A session with the demo project open.
fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

/// `x` seconds in ticks.
fn sec(x: f64) -> i64 {
    (x * TICKS_PER_SECOND as f64) as i64
}

/// A copy of clip `id` in the active sequence.
fn clip(s: &Session, id: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(id).unwrap().1.clone()
}

/// The clip's Volume level keys: (timeline time, dB).
fn volume_keys(s: &Session, id: ClipId) -> Vec<(i64, f64)> {
    let it = clip(s, id);
    let p = it.effect("volume").and_then(|e| e.param("level")).unwrap();
    p.keyframes.iter().map(|k| (it.timeline_time(k.time).0, k.value.as_f64().unwrap())).collect()
}

/// Copied Volume keys paste on the selected clip, the first at the playhead, in one undo step.
#[test]
fn copied_volume_keyframes_paste_at_the_playhead_on_the_selected_clip() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let a1 = &q.audio_tracks[0].items;
    let (src, dst) = (a1[0].clone(), a1[1].clone());
    let volume = src.effects.iter().position(|e| e.effect == "volume").unwrap();
    // two keys a second apart on the first clip, at -6 and -20 dB
    let (t0, t1) = (src.start.0 + sec(0.5), src.start.0 + sec(1.5));
    for (t, db) in [(t0, -6.0), (t1, -20.0)] {
        s.execute("effects.addKeyframe", json!({"clip": src.id.0, "effect": "volume", "param": "level", "time": t})).unwrap();
        let m = clip(&s, src.id).moving_source_time_at(Tick(t));
        s.execute("effects.setKeyframe", json!({"clip": src.id.0, "effect": "volume", "param": "level", "mediaTime": m.0, "value": db})).unwrap();
    }
    let keys: Vec<_> = [t0, t1]
        .iter()
        .map(|&t| json!({"clip": src.id.0, "effect": volume, "param": "level", "time": clip(&s, src.id).moving_source_time_at(Tick(t)).0}))
        .collect();
    assert_eq!(s.execute("edit.copyKeyframes", json!({"keyframes": keys})).unwrap()["copied"], 2);
    assert!(s.is_enabled("edit.paste"), "copied keyframes make Paste available");

    // select the second clip, playhead into it, plain Edit > Paste
    s.execute("timeline.select", json!({"clips": [dst.id.0], "linked": false})).unwrap();
    s.set_playhead(Tick(dst.start.0 + sec(0.25)));
    let at = s.playhead().0;
    assert_eq!(s.execute("edit.paste", json!({})).unwrap()["pasted"], 2);
    let pasted = volume_keys(&s, dst.id);
    assert_eq!(pasted.len(), 2, "{pasted:?}");
    assert!((pasted[0].0 - at).abs() <= 1 && (pasted[1].0 - (at + sec(1.0))).abs() <= 1, "first at the playhead, a second apart: {pasted:?}");
    assert_eq!((pasted[0].1, pasted[1].1), (-6.0, -20.0));
    assert_eq!(volume_keys(&s, src.id).len(), 2, "the copied keys stay");

    s.execute("edit.undo", json!({})).unwrap();
    assert!(volume_keys(&s, dst.id).is_empty(), "one undo step");
}

/// Copied track lane keys paste on their track with their values, the first at the playhead.
#[test]
fn copied_track_keyframes_paste_at_the_playhead() {
    let mut s = demo();
    for (t, v) in [(1.0, -3.0), (3.0, -12.0)] {
        s.execute("mixer.setKeyframe", json!({"strip": "A1", "lane": LANE_VOLUME, "time": sec(t), "value": v})).unwrap();
    }
    let strip = s.active_sequence().unwrap().audio_tracks[0].id.0;
    let keys = json!([{"strip": strip, "lane": LANE_VOLUME, "time": sec(1.0)}, {"strip": strip, "lane": LANE_VOLUME, "time": sec(3.0)}]);
    s.execute("edit.copyKeyframes", json!({"keyframes": keys})).unwrap();
    s.set_playhead(Tick(sec(10.0)));
    let at = s.playhead().0;
    s.execute("edit.paste", json!({})).unwrap();
    let tr = &s.active_sequence().unwrap().audio_tracks[0];
    let got: Vec<(i64, ParamValue)> = tr.lane_keyframes(LANE_VOLUME).iter().map(|k| (k.time.0, k.value.clone())).collect();
    assert_eq!(
        got,
        vec![
            (sec(1.0), ParamValue::Float(-3.0)),
            (sec(3.0), ParamValue::Float(-12.0)),
            (at, ParamValue::Float(-3.0)),
            (at + sec(2.0), ParamValue::Float(-12.0)),
        ]
    );
}

/// Copying clips empties the keyframe clipboard, and references to no keyframe are refused.
#[test]
fn copying_clips_after_keyframes_pastes_clips_and_bad_refs_are_refused() {
    let mut s = demo();
    let strip = s.active_sequence().unwrap().audio_tracks[0].id.0;
    s.execute("mixer.setKeyframe", json!({"strip": "A1", "lane": LANE_VOLUME, "time": sec(1.0), "value": -3.0})).unwrap();
    s.execute("edit.copyKeyframes", json!({"keyframes": [{"strip": strip, "lane": LANE_VOLUME, "time": sec(1.0)}]})).unwrap();
    let first = s.active_sequence().unwrap().audio_tracks[0].items[0].id;
    s.execute("timeline.select", json!({"clips": [first.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    assert!(s.state.keyframe_clipboard.is_empty(), "the last copy wins");
    for bad in [
        json!({}),
        json!({"keyframes": []}),
        json!({"keyframes": [{"strip": strip, "lane": LANE_VOLUME, "time": 7}]}),
        json!({"keyframes": [{"clip": u64::MAX, "effect": 0, "param": "level", "time": 0}]}),
    ] {
        assert!(s.execute("edit.copyKeyframes", bad.clone()).is_err(), "{bad}");
    }
}

/// Pasting track keys on the Mix track keeps only the lanes Mix has (volume and effects), as the
/// mixer commands do: a copied pan lane is skipped, and pan alone is refused.
#[test]
fn pasting_on_mix_skips_the_lanes_mix_has_not() {
    let mut s = demo();
    s.execute("mixer.setKeyframe", json!({"strip": "A1", "lane": LANE_VOLUME, "time": sec(1.0), "value": -3.0})).unwrap();
    s.execute("mixer.setKeyframe", json!({"strip": "A1", "lane": LANE_PAN, "time": sec(1.0), "value": 0.5})).unwrap();
    let strip = s.active_sequence().unwrap().audio_tracks[0].id.0;
    let both = json!([{"strip": strip, "lane": LANE_VOLUME, "time": sec(1.0)}, {"strip": strip, "lane": LANE_PAN, "time": sec(1.0)}]);
    s.execute("edit.copyKeyframes", json!({"keyframes": both})).unwrap();
    assert_eq!(s.execute("edit.paste", json!({"strip": "Mix", "time": sec(5.0)})).unwrap()["pasted"], 1);
    let mix = |s: &Session, lane: &str| s.active_sequence().unwrap().strip(MASTER_STRIP).unwrap().lane_keyframes(lane).len();
    assert_eq!((mix(&s, LANE_VOLUME), mix(&s, LANE_PAN)), (1, 0));

    s.execute("edit.copyKeyframes", json!({"keyframes": [{"strip": strip, "lane": LANE_PAN, "time": sec(1.0)}]})).unwrap();
    assert!(s.execute("edit.paste", json!({"strip": "Mix", "time": sec(5.0)})).is_err(), "pan alone has nowhere to go on Mix");
    assert_eq!(mix(&s, LANE_PAN), 0);
}

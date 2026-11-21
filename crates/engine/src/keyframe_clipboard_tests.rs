//! Copy and paste of keyframes (#3562): clip Volume keys and track automation keys keep their
//! values and spacing, the first lands at the playhead on the same parameter of the target.

use filmcraft_project::mixer::{LANE_PAN, LANE_VOLUME, MASTER_STRIP};
use filmcraft_project::{ClipId, Interpolation, ParamValue, TrackItem};
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
    assert!(volume_keys(&s, dst.id).is_empty(), "the demo's second clip starts with no Volume keys");
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
    let got = a1_volume(&s);
    assert_eq!(
        got,
        vec![
            (sec(1.0), ParamValue::Float(-3.0)),
            (sec(3.0), ParamValue::Float(-12.0)),
            (at, ParamValue::Float(-3.0)),
            (at + sec(2.0), ParamValue::Float(-12.0)),
        ]
    );
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(a1_volume(&s), got[..2], "one undo step takes the pasted keys back");
}

/// A1's Volume lane keys: (sequence time, value).
fn a1_volume(s: &Session) -> Vec<(i64, ParamValue)> {
    s.active_sequence().unwrap().audio_tracks[0].lane_keyframes(LANE_VOLUME).iter().map(|k| (k.time.0, k.value.clone())).collect()
}

/// A1's Volume key at 1 s (-3 dB) on the keyframe clipboard; returns A1's strip id.
fn copy_a1_key(s: &mut Session) -> u64 {
    s.execute("mixer.setKeyframe", json!({"strip": "A1", "lane": LANE_VOLUME, "time": sec(1.0), "value": -3.0})).unwrap();
    let strip = s.active_sequence().unwrap().audio_tracks[0].id.0;
    s.execute("edit.copyKeyframes", json!({"keyframes": [{"strip": strip, "lane": LANE_VOLUME, "time": sec(1.0)}]})).unwrap();
    strip
}

/// `track` names the target strip as `strip` does, as in the mixer commands.
#[test]
fn track_names_the_strip_to_paste_on() {
    let mut s = demo();
    s.execute("sequence.addTracks", json!({"video": 0, "audio": 1})).unwrap();
    copy_a1_key(&mut s);
    let other = s.active_sequence().unwrap().audio_tracks.last().unwrap().id;
    assert_eq!(s.execute("edit.paste", json!({"track": other.0, "time": sec(5.0)})).unwrap()["pasted"], 1);
    let lane = |s: &Session, i: usize| s.active_sequence().unwrap().audio_tracks[i].lane_keyframes(LANE_VOLUME).iter().map(|k| k.time.0).collect::<Vec<_>>();
    let last = s.active_sequence().unwrap().audio_tracks.len() - 1;
    assert_eq!(lane(&s, last), vec![sec(5.0)], "on the named track");
    assert_eq!(lane(&s, 0), vec![sec(1.0)], "not on the track it came from");
}

/// A `time`, `clip`, `strip` or `track` given but malformed, or naming nothing, is refused with
/// the field named, instead of pasting at the playhead or on the source.
#[test]
fn malformed_paste_targets_are_refused() {
    let mut s = demo();
    copy_a1_key(&mut s);
    let before = a1_volume(&s);
    for (p, field) in [
        (json!({"time": "soon"}), "time"),
        (json!({"time": 1.5}), "time"),
        (json!({"time": -1}), "time"),
        (json!({"frame": "x"}), "time"),
        (json!({"clip": "x"}), "clip"),
        (json!({"clip": u64::MAX}), "clip"),
        (json!({"strip": "Z9"}), "strip"),
        (json!({"track": 999_999}), "strip"),
    ] {
        let e = s.execute("edit.paste", p.clone()).unwrap_err().to_string();
        assert!(e.contains(field), "{p}: {e}");
        assert_eq!(a1_volume(&s), before, "{p} pasted nothing");
    }
    assert_eq!(s.execute("edit.paste", json!({"time": sec(5.0)})).unwrap()["pasted"], 1, "a valid time still pastes");
}

/// A selected clip with the effect but not the copied parameter (here a mask it has not) is
/// passed over: the keys go back to the clip they came from rather than failing.
#[test]
fn a_selected_clip_without_the_parameter_is_passed_over() {
    let mut s = demo();
    let v1 = s.active_sequence().unwrap().video_tracks[0].items.clone();
    let (src, other) = (v1[0].clone(), v1[1].clone());
    s.execute("masks.add", json!({"clip": src.id.0, "shape": "ellipse"})).unwrap();
    let t = src.start.0 + src.duration.0 / 4;
    s.execute("effects.setParam", json!({"clip": src.id.0, "effect": "opacity", "param": "feather", "mask": 0, "value": 20.0, "time": t, "keyframe": true}))
        .unwrap();
    let it = clip(&s, src.id);
    let opacity = it.effects.iter().position(|e| e.effect == "opacity").unwrap();
    let feather = |it: &TrackItem| it.effects[opacity].masks[0].feather.keyframes.clone();
    let m = feather(&it)[0].time;
    s.execute("edit.copyKeyframes", json!({"keyframes": [{"clip": src.id.0, "effect": opacity, "param": "feather", "mask": 0, "time": m.0}]})).unwrap();
    assert!(clip(&s, other.id).effects.iter().find(|e| e.effect == "opacity").is_some_and(|e| e.masks.is_empty()), "the other clip has opacity, no mask");

    s.execute("timeline.select", json!({"clips": [other.id.0], "linked": false})).unwrap();
    let at = src.start.0 + src.duration.0 / 2;
    assert_eq!(s.execute("edit.paste", json!({"time": at})).unwrap()["pasted"], 1);
    let keys = feather(&clip(&s, src.id));
    assert_eq!(keys.len(), 2, "pasted on the source's mask: {keys:?}");
    assert!((clip(&s, src.id).timeline_time(keys[1].time).0 - at).abs() <= 1, "{keys:?}");
}

/// Keys that would land past the target clip's ends are skipped; none landing is refused.
#[test]
fn keys_past_the_target_clip_are_skipped() {
    let mut s = demo();
    let a1 = s.active_sequence().unwrap().audio_tracks[0].items.clone();
    let (src, dst) = (a1[0].clone(), a1[1].clone());
    let volume = src.effects.iter().position(|e| e.effect == "volume").unwrap();
    let mut keys = Vec::new();
    for t in [src.start.0 + sec(0.5), src.start.0 + sec(1.5)] {
        s.execute("effects.addKeyframe", json!({"clip": src.id.0, "effect": "volume", "param": "level", "time": t})).unwrap();
        keys.push(json!({"clip": src.id.0, "effect": volume, "param": "level", "time": clip(&s, src.id).moving_source_time_at(Tick(t)).0}));
    }
    s.execute("edit.copyKeyframes", json!({"keyframes": keys})).unwrap();
    // half a second before the end: the second key (a second later) falls past it
    let at = dst.end().0 - sec(0.5);
    assert_eq!(s.execute("edit.paste", json!({"clip": dst.id.0, "time": at})).unwrap()["pasted"], 1);
    let got = volume_keys(&s, dst.id);
    assert_eq!(got.len(), 1, "{got:?}");
    assert!((got[0].0 - at).abs() <= 1, "{got:?}");
    // at the very end nothing lands
    let e = s.execute("edit.paste", json!({"clip": dst.id.0, "time": dst.end().0})).unwrap_err().to_string();
    assert!(e.contains("outside"), "{e}");
    assert_eq!(volume_keys(&s, dst.id).len(), 1);
}

/// Pasting on an explicit `clip` and `time` (over the selection) replaces a key already at a
/// pasted time and keeps each key's interpolation; Undo restores the replaced key.
#[test]
fn paste_on_an_explicit_clip_replaces_keys_and_keeps_interpolation() {
    let mut s = demo();
    let a1 = s.active_sequence().unwrap().audio_tracks[0].items.clone();
    let (src, dst) = (a1[0].clone(), a1[1].clone());
    let volume = src.effects.iter().position(|e| e.effect == "volume").unwrap();
    let mut keys = Vec::new();
    for (t, db, interp) in [(src.start.0 + sec(0.5), -6.0, "hold"), (src.start.0 + sec(1.5), -20.0, "easeIn")] {
        s.execute("effects.addKeyframe", json!({"clip": src.id.0, "effect": "volume", "param": "level", "time": t})).unwrap();
        let m = clip(&s, src.id).moving_source_time_at(Tick(t)).0;
        s.execute("effects.setKeyframe", json!({"clip": src.id.0, "effect": "volume", "param": "level", "mediaTime": m, "value": db})).unwrap();
        s.execute("effects.setInterpolation", json!({"clip": src.id.0, "effect": "volume", "param": "level", "mediaTime": m, "interpolation": interp}))
            .unwrap();
        keys.push(json!({"clip": src.id.0, "effect": volume, "param": "level", "time": m}));
    }
    s.execute("edit.copyKeyframes", json!({"keyframes": keys})).unwrap();
    // a key of the destination's own where the first one lands
    let at = dst.start.0 + sec(0.25);
    s.execute("effects.addKeyframe", json!({"clip": dst.id.0, "effect": "volume", "param": "level", "time": at})).unwrap();
    let own = clip(&s, dst.id).effect("volume").and_then(|e| e.param("level")).unwrap().keyframes.clone();
    assert_eq!(own.len(), 1);

    // the source is selected: `clip` wins over it
    s.execute("timeline.select", json!({"clips": [src.id.0], "linked": false})).unwrap();
    assert_eq!(s.execute("edit.paste", json!({"clip": dst.id.0, "time": at})).unwrap()["pasted"], 2);
    let it = clip(&s, dst.id);
    let got = &it.effect("volume").and_then(|e| e.param("level")).unwrap().keyframes;
    let summary: Vec<_> = got.iter().map(|k| (it.timeline_time(k.time).0, k.value.as_f64().unwrap(), k.interp)).collect();
    assert_eq!(summary.len(), 2, "the key at `time` was replaced, not doubled: {summary:?}");
    assert!((summary[0].0 - at).abs() <= 1 && (summary[1].0 - (at + sec(1.0))).abs() <= 1, "{summary:?}");
    assert_eq!((summary[0].1, summary[0].2), (-6.0, Interpolation::Hold));
    assert_eq!((summary[1].1, summary[1].2), (-20.0, Interpolation::EaseIn));
    assert_eq!(volume_keys(&s, src.id).len(), 2, "nothing pasted on the selected source");

    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(clip(&s, dst.id).effect("volume").and_then(|e| e.param("level")).unwrap().keyframes, own, "Undo brings the replaced key back");
}

/// Paste Insert has nothing to ripple for keyframes: it is disabled while they are on the
/// clipboard. Clip and caption pastes refuse the keyframe-only parameters.
#[test]
fn paste_insert_is_off_for_keyframes_and_clip_paste_refuses_keyframe_params() {
    let mut s = demo();
    copy_a1_key(&mut s);
    assert!(s.is_enabled("edit.paste"));
    assert!(!s.is_enabled("edit.pasteInsert"), "keyframes paste with Paste only");
    let e = s.execute("edit.pasteInsert", json!({})).unwrap_err().to_string();
    assert!(e.contains("Paste"), "{e}");

    let first = s.active_sequence().unwrap().video_tracks[0].items[0].id;
    s.execute("timeline.select", json!({"clips": [first.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    assert!(s.is_enabled("edit.pasteInsert"), "clips paste-insert again");
    for id in ["edit.paste", "edit.pasteInsert"] {
        for k in ["time", "clip", "strip", "track"] {
            let e = s.execute(id, json!({k: 1})).unwrap_err().to_string();
            assert!(e.contains(k), "{id} {k}: {e}");
        }
    }
    let n = s.active_sequence().unwrap().video_tracks[0].items.len();
    s.execute("edit.paste", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.len(), n + 1, "a plain Paste still pastes the clip");
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
    // copying keyframes empties the clip clipboard in turn
    s.execute("edit.copyKeyframes", json!({"keyframes": [{"strip": strip, "lane": LANE_VOLUME, "time": sec(1.0)}]})).unwrap();
    assert!(s.state.clipboard.is_empty() && !s.state.keyframe_clipboard.is_empty());
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

/// Copying captions empties the keyframe clipboard, so Paste places the captions.
#[test]
fn copying_captions_after_keyframes_pastes_captions() {
    let mut s = demo();
    copy_a1_key(&mut s);
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    s.execute("captions.add", json!({"text": "hi", "durationSeconds": 1.0})).unwrap();
    assert_eq!(s.execute("edit.copy", json!({})).unwrap()["copied"], 1);
    assert!(s.state.keyframe_clipboard.is_empty() && !s.state.caption_clipboard.is_empty());
    let before = a1_volume(&s);
    s.execute("playhead.set", json!({"seconds": 10.0})).unwrap();
    s.execute("edit.paste", json!({})).unwrap();
    assert_eq!(s.active_sequence().unwrap().caption_tracks[0].captions.len(), 2, "the caption was pasted");
    assert_eq!(a1_volume(&s), before, "no keyframe was");
}

//! Time Remapping (#510, #1491, #1709): the clip's Speed in Effect Controls changes how the picture
//! plays, static or keyframed, through the same commands the panels, the control channel and MCP
//! use. Its linked sound follows unless Remap Audio is off.

use filmcraft_project::{ClipId, TrackItem};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde_json::json;

use crate::Session;

const S: i64 = TICKS_PER_SECOND;

/// One 96-frame clip with linked sound at the start of a new sequence, then a second clip on V1
/// far enough away not to get in the way: (picture, sound).
fn one_clip(s: &mut Session) -> (ClipId, ClipId) {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Remap", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "frame": 0, "sourceIn": rate.tick_of(24).0, "duration": rate.tick_of(96).0})).unwrap();
    let q = s.active_sequence().unwrap();
    (q.video_tracks[0].items[0].id, q.audio_tracks[0].items[0].id)
}

fn clip(s: &Session, c: ClipId) -> TrackItem {
    s.active_sequence().unwrap().find_item(c).unwrap().1.clone()
}

fn set_speed(s: &mut Session, c: ClipId, value: f64, at: Option<i64>) -> serde_json::Value {
    let mut p = json!({"clip": c.0, "effect": "time_remap", "param": "speed", "value": value});
    if let Some(t) = at {
        p["time"] = json!(t);
        p["keyframe"] = json!(true);
    }
    s.execute("effects.setParam", p).unwrap()
}

fn static_speed(it: &TrackItem) -> f64 {
    it.effect("time_remap").unwrap().param("speed").unwrap().value.as_f64().unwrap()
}

#[test]
fn a_static_speed_plays_the_picture_faster() {
    let mut s = Session::default();
    let (v, a) = one_clip(&mut s);
    let (before, sound) = (clip(&s, v), clip(&s, a));
    set_speed(&mut s, v, 400.0, None);
    let it = clip(&s, v);
    // the same media in a quarter of the time
    assert_eq!(it.speed, 4.0);
    assert_eq!(it.source_out(), before.source_out());
    assert_eq!(it.duration, Tick(before.duration.0 / 4));
    assert_eq!(it.source_time_at(it.start + Tick(S / 2)), before.source_in + Tick(2 * S));
    assert_eq!(static_speed(&it), 400.0);
    // the sound plays at the same speed, in step
    let snd = clip(&s, a);
    assert_eq!((snd.speed, snd.duration, snd.source_out()), (4.0, it.duration, sound.source_out()));
    s.undo();
    assert_eq!(clip(&s, v), before);
    assert_eq!(clip(&s, a), sound);
}

#[test]
fn speed_duration_shows_up_as_the_static_speed() {
    let mut s = Session::default();
    let (v, _) = one_clip(&mut s);
    s.execute("timeline.select", json!({"clips": [v.0]})).unwrap();
    s.execute("clip.speedDuration", json!({"speed": 50})).unwrap();
    assert_eq!(static_speed(&clip(&s, v)), 50.0);
    // and a static value set afterwards starts from there: 200 % plays it four times as fast
    let slow = clip(&s, v);
    set_speed(&mut s, v, 200.0, None);
    assert_eq!(clip(&s, v).duration, Tick(slow.duration.0 / 4));
}

#[test]
fn keyframes_ramp_the_speed() {
    let mut s = Session::default();
    let (v, a) = one_clip(&mut s);
    let (before, sound) = (clip(&s, v), clip(&s, a));
    // 100 % up to one second into the clip, rising to 400 % at two seconds
    set_speed(&mut s, v, 100.0, Some(S));
    set_speed(&mut s, v, 400.0, Some(2 * S));
    let it = clip(&s, v);
    assert!(it.is_remapped());
    // the clip keeps its media and gets shorter
    assert!((it.source_out() - before.source_out()).0.abs() <= 1);
    assert!(it.duration < before.duration && it.duration > Tick(before.duration.0 / 4), "{:?}", it.duration);
    // normal speed in its first second, faster after the ramp
    assert_eq!(it.source_time_at(Tick(S / 2)), before.source_in + Tick(S / 2));
    let step = |t: i64| (it.source_time_at(Tick(t + S / 10)) - it.source_time_at(Tick(t))).0 as f64 / (S / 10) as f64;
    assert!((step(S / 4) - 1.0).abs() < 1e-3);
    assert!(step(it.duration.0 - S / 5) > 3.9);
    // the sound follows the ramp sample for sample
    let snd = clip(&s, a);
    assert!(snd.is_remapped());
    assert_eq!((snd.start, snd.duration), (it.start, it.duration));
    for i in 0..40 {
        let t = Tick(i * it.duration.0 / 40);
        assert_eq!(snd.source_time_at(t), it.source_time_at(t));
    }
    // Speed/Duration can't set a constant speed over the keyframes
    s.execute("timeline.select", json!({"clips": [v.0]})).unwrap();
    assert!(s.execute("clip.speedDuration", json!({"speed": 50})).is_err());
    // but OK on the speed it shows (the average, to two decimals) sets the Time Interpolation alone
    let shown = (it.speed * 10_000.0).round() / 100.0;
    s.execute("clip.speedDuration", json!({"speed": shown, "interpolation": "frameBlending"})).unwrap();
    let blended = clip(&s, v);
    assert_eq!(blended.time_interpolation, filmcraft_project::TimeInterpolation::FrameBlending);
    assert_eq!((blended.duration, blended.effect("time_remap")), (it.duration, it.effect("time_remap")));
    s.undo();
    // one undo step per keyframe
    s.undo();
    s.undo();
    assert_eq!(clip(&s, v), before);
    assert_eq!(clip(&s, a), sound);
}

#[test]
fn remap_audio_off_keeps_the_sound_at_normal_speed() {
    let mut s = Session::default();
    let (v, a) = one_clip(&mut s);
    let sound = clip(&s, a);
    s.execute("effects.setParam", json!({"clip": v.0, "effect": "time_remap", "param": "audio", "value": false})).unwrap();
    set_speed(&mut s, v, 100.0, Some(0));
    set_speed(&mut s, v, 400.0, Some(S));
    assert!(clip(&s, v).is_remapped());
    assert_eq!(clip(&s, a), sound);
    // on again: the sound picks up the ramp
    s.execute("effects.setParam", json!({"clip": v.0, "effect": "time_remap", "param": "audio", "value": true})).unwrap();
    let (it, snd) = (clip(&s, v), clip(&s, a));
    assert!(snd.is_remapped());
    assert_eq!(snd.duration, it.duration);
    // a razor cut through both keeps them in step
    s.execute("sequence.addEdit", json!({"time": it.duration.0 / 2})).unwrap();
    let q = s.active_sequence().unwrap();
    for (pv, pa) in q.video_tracks[0].items.iter().zip(&q.audio_tracks[0].items) {
        assert_eq!((pv.start, pv.duration, pv.source_in), (pa.start, pa.duration, pa.source_in));
    }
}

#[test]
fn the_stopwatch_turns_a_ramp_on_and_off() {
    let mut s = Session::default();
    let (v, _) = one_clip(&mut s);
    s.execute("timeline.select", json!({"clips": [v.0]})).unwrap();
    s.execute("clip.speedDuration", json!({"speed": 200})).unwrap();
    let constant = clip(&s, v);
    s.execute("playhead.set", json!({"seconds": 0.5})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": v.0, "effect": "time_remap", "param": "speed"})).unwrap();
    // one keyframe at the clip's speed: the same timing
    let on = clip(&s, v);
    assert!(on.is_remapped());
    assert_eq!((on.start, on.duration, on.source_in), (constant.start, constant.duration, constant.source_in));
    set_speed(&mut s, v, 100.0, Some(S));
    assert!(clip(&s, v).duration > constant.duration);
    // off: the clip plays at the speed it had at the playhead (200 %) and shows its media again
    s.execute("effects.toggleAnimation", json!({"clip": v.0, "effect": "time_remap", "param": "speed"})).unwrap();
    let off = clip(&s, v);
    assert!(!off.is_remapped());
    assert_eq!(off.speed, 2.0);
    assert!((off.source_out() - constant.source_out()).0.abs() <= 1);
}

#[test]
fn remove_attributes_resets_the_speed() {
    let mut s = Session::default();
    let (v, _) = one_clip(&mut s);
    let before = clip(&s, v);
    set_speed(&mut s, v, 100.0, Some(0));
    set_speed(&mut s, v, 300.0, Some(S));
    s.execute("edit.removeAttributes", json!({"clips": [v.0], "motion": false, "opacity": false, "effects": false})).unwrap();
    let it = clip(&s, v);
    assert!(!it.is_remapped());
    assert_eq!((it.speed, it.duration, it.source_in), (1.0, before.duration, before.source_in));
}

#[test]
fn a_razor_cut_keeps_every_frame_of_a_ramp() {
    let mut s = Session::default();
    let (v, _) = one_clip(&mut s);
    set_speed(&mut s, v, 50.0, Some(0));
    set_speed(&mut s, v, 300.0, Some(2 * S));
    let whole = clip(&s, v);
    let shows = |s: &Session, t: Tick| {
        let q = s.active_sequence().unwrap();
        q.video_tracks[0].items.iter().find(|i| i.start <= t && t < i.end()).map(|i| i.source_time_at(t)).unwrap()
    };
    let probes: Vec<Tick> = (0..48).map(|i| Tick(i * whole.duration.0 / 48)).collect();
    let want: Vec<Tick> = probes.iter().map(|t| shows(&s, *t)).collect();
    s.execute("sequence.addEdit", json!({"time": whole.duration.0 / 3})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.len(), 2);
    for (t, w) in probes.iter().zip(&want) {
        assert!((shows(&s, *t) - *w).0.abs() <= 2, "{t:?}");
    }
}

#[test]
fn hostile_speeds_stay_in_range() {
    let mut s = Session::default();
    let (v, _) = one_clip(&mut s);
    for value in [0.0, -250.0, 1e300, -1e300, 5e-324] {
        set_speed(&mut s, v, value, None);
        let it = clip(&s, v);
        assert!(it.speed >= 0.01 && it.speed <= 100.0, "{value}: {}", it.speed);
        assert!(it.duration > Tick::ZERO);
        set_speed(&mut s, v, value, Some(S));
        assert!(clip(&s, v).duration > Tick::ZERO);
        s.undo();
        s.undo();
    }
}

/// Dragging the Speed value with the stopwatch on wrote a new keyframe on every step of the drag:
/// each step retimes the clip, so the media under the playhead moved. A drag edits one keyframe.
#[test]
fn dragging_the_speed_edits_one_keyframe() {
    let mut s = Session::default();
    let (v, _) = one_clip(&mut s);
    set_speed(&mut s, v, 100.0, Some(0));
    s.execute("playhead.set", json!({"seconds": 1.0})).unwrap();
    // as Effect Controls sends it: the effect by index, every step merged, the first one beginning
    let idx = clip(&s, v).effects.iter().position(|e| e.effect == "time_remap").unwrap();
    let undo = s.history.undo.len();
    for (i, value) in [90.0, 70.0, 50.0, 30.0, 12.0, 6.0].into_iter().enumerate() {
        s.execute("effects.setParam", json!({"clip": v.0, "effect": idx, "param": "speed", "value": value, "merge": true, "begin": i == 0})).unwrap();
    }
    let it = clip(&s, v);
    let keys = &it.effect("time_remap").unwrap().param("speed").unwrap().keyframes;
    assert_eq!(keys.len(), 2, "{keys:?}");
    assert_eq!(keys[1].value, filmcraft_project::ParamValue::Float(6.0));
    assert_eq!(s.history.undo.len(), undo + 1, "one undo step");
    // the playhead followed the keyframe: it is on it (within half a frame)
    let frame = s.sequence_rate().frame_duration();
    let p = it.effect("time_remap").unwrap().param("speed").unwrap();
    assert_eq!(it.keyframe_near(p, it.source_time_at(s.playhead()), frame), Some(keys[1].time), "playhead {:?}", s.playhead());
    // a new drag at a new place adds its own keyframe
    s.execute("playhead.set", json!({"seconds": 0.25})).unwrap();
    for (i, value) in [95.0, 80.0].into_iter().enumerate() {
        s.execute("effects.setParam", json!({"clip": v.0, "effect": idx, "param": "speed", "value": value, "merge": true, "begin": i == 0})).unwrap();
    }
    assert_eq!(clip(&s, v).effect("time_remap").unwrap().param("speed").unwrap().keyframes.len(), 3);
}

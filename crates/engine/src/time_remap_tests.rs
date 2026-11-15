//! Time Remapping (#510, #1491, #1709): the clip's Speed in Effect Controls changes how the picture
//! plays, static or keyframed, through the same commands the panels, the control channel and MCP
//! use. Its linked sound is retimed with a static speed and left at 100 % under a keyframed ramp,
//! as Premiere's Time Remapping does.

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

/// The sequence's mix from its start, `frames` samples long: what the timeline sounds like.
fn mix(s: &Session, frames: usize) -> filmcraft_frame::AudioBuffer {
    mix_at(s, 0, frames)
}

/// The sequence's mix of `frames` samples starting at timeline sample `start`.
fn mix_at(s: &Session, start: i64, frames: usize) -> filmcraft_frame::AudioBuffer {
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    filmcraft_render::audio::mix_sequence(&s.project, s.active_sequence().unwrap(), start, frames, &provider)
}

/// The loudest sample in a mix.
fn peak(b: &filmcraft_frame::AudioBuffer) -> f32 {
    b.channels.iter().flatten().fold(0.0f32, |m, v| m.max(v.abs()))
}

/// The media a clip plays over its length: 1.0 plays its source at its own rate.
fn rate(it: &TrackItem) -> f64 {
    (it.source_out() - it.source_in).0 as f64 / it.duration.0 as f64
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
    // the sound stays at 100 % and only follows the picture's length (see
    // `a_keyframed_ramp_leaves_the_linked_sound_at_normal_speed`)
    let snd = clip(&s, a);
    assert!(!snd.is_remapped());
    assert_eq!((snd.speed, snd.start, snd.duration), (1.0, it.start, it.duration));
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

/// A keyframed ramp leaves the linked sound at 100 %: still linked and audible, playing its own
/// media at its own rate while the picture drifts ahead of it.
#[test]
fn a_keyframed_ramp_leaves_the_linked_sound_at_normal_speed() {
    let mut s = Session::default();
    let (v, a) = one_clip(&mut s);
    let (picture, sound) = (clip(&s, v), clip(&s, a));
    let untouched = mix(&s, 48_000);
    // 100 % up to one second into the clip, rising to 400 % at two seconds
    set_speed(&mut s, v, 100.0, Some(S));
    set_speed(&mut s, v, 400.0, Some(2 * S));
    let (it, snd) = (clip(&s, v), clip(&s, a));
    assert!(it.is_remapped() && it.duration < picture.duration);
    // no curve on the sound: it plays its source at its own rate
    assert!(!snd.is_remapped() && snd.effect("time_remap").is_none());
    assert_eq!(snd.speed, 1.0);
    assert!((rate(&snd) - 1.0).abs() < 1e-9, "the sound plays at {}", rate(&snd));
    // still linked to its picture, enabled, unmuted and at its own level
    assert!(snd.link.is_some() && snd.link == it.link);
    assert!(snd.enabled && !snd.essential.as_ref().is_some_and(|e| e.mute) && snd.gain_db == 0.0);
    // its length follows the picture's and its media still starts where it did, so the ramp cuts
    // its tail rather than stretching it
    assert_eq!((snd.start, snd.duration, snd.source_in), (it.start, it.duration, sound.source_in));
    assert_eq!(snd.source_out(), snd.source_in + snd.duration);
    assert!(snd.source_out() < it.source_out(), "the picture plays media past the sound's out point");
    // the samples are the source at 100 %: the same sound the ramp never touched, and not silence
    let after = mix(&s, 48_000);
    for (x, y) in untouched.channels.iter().zip(&after.channels) {
        let d = x.iter().zip(y).map(|(p, q)| (p - q).abs()).fold(0.0f32, f32::max);
        assert!(d < 1e-6, "the ramp retimed the sound: off by {d}");
        assert!(y.iter().fold(0.0f32, |m, v| m.max(v.abs())) > 1e-3, "the linked sound went silent");
    }
    // a razor through the pair keeps the sound on the time the picture is on
    s.execute("sequence.addEdit", json!({"time": snd.duration.0 / 2})).unwrap();
    let q = s.active_sequence().unwrap();
    for (pv, pa) in q.video_tracks[0].items.iter().zip(&q.audio_tracks[0].items) {
        assert_eq!((pv.start, pv.duration), (pa.start, pa.duration));
    }
}

/// A constant speed change retimes the linked sound with the picture, playing the same media; its
/// pitch follows only with Maintain Audio Pitch, which is off until the user sets it.
#[test]
fn a_constant_speed_change_retimes_the_linked_sound() {
    let mut s = Session::default();
    let (v, a) = one_clip(&mut s);
    let (picture, sound) = (clip(&s, v), clip(&s, a));
    // a static 200 % in Effect Controls is a constant speed: the pair shrinks together
    set_speed(&mut s, v, 200.0, None);
    let (it, snd) = (clip(&s, v), clip(&s, a));
    assert_eq!(it.speed, 2.0);
    assert!((it.duration.0 as f64 - picture.duration.0 as f64 / 2.0).abs() <= 1.0, "{:?}", it.duration);
    assert_eq!((snd.speed, snd.start, snd.duration), (2.0, it.start, it.duration));
    assert_eq!((snd.source_in, snd.source_out()), (sound.source_in, sound.source_out()), "the same media");
    assert!((rate(&snd) - 2.0).abs() < 1e-9, "the sound plays at {}", rate(&snd));
    assert!(snd.link == it.link && snd.link.is_some());
    assert!(!snd.maintain_pitch, "pitch preservation is off until it is set");
    // Speed/Duration… ▸ Maintain Audio Pitch sets it for the pair
    s.execute("clip.speedDuration", json!({"clips": [v.0], "speed": 200, "maintainPitch": true})).unwrap();
    assert!(clip(&s, v).maintain_pitch && clip(&s, a).maintain_pitch, "the pair keeps its pitch");
    // a later speed change retimes the sound and keeps the setting
    set_speed(&mut s, v, 50.0, None);
    let snd = clip(&s, a);
    assert!((snd.speed - 0.5).abs() < 1e-9 && snd.maintain_pitch, "{} {snd:?}", snd.speed);
    assert_eq!((snd.duration, snd.source_out()), (clip(&s, v).duration, sound.source_out()));
    // ... and off again
    s.execute("clip.speedDuration", json!({"clips": [v.0], "speed": 50, "maintainPitch": false})).unwrap();
    assert!(!clip(&s, v).maintain_pitch && !clip(&s, a).maintain_pitch);
    s.active_sequence().unwrap().check().unwrap();
}

/// A slow ramp makes the clip longer than its sound's media: the sound plays on at 100 % and then
/// the rest of the clip is silence. Nothing loops, repeats or holds the last sample.
#[test]
fn a_slow_ramp_leaves_silence_after_the_sounds_media() {
    let mut s = Session::default();
    let (v, a) = one_clip(&mut s);
    let (picture, sound) = (clip(&s, v), clip(&s, a));
    let sr = i64::from(s.active_sequence().unwrap().settings.sample_rate);
    let media = crate::media_duration(&s.project, &s.media, sound.item).expect("the demo media has a length");
    assert!(media > sound.source_out(), "the clip is inside its media: {media:?}");
    let untouched = mix(&s, 2 * sr as usize);
    assert!(peak(&untouched) > 1e-3, "the demo has audio on A1");
    // 100 % down to 10 %: the picture, and its sound at 100 %, outlast the media
    set_speed(&mut s, v, 100.0, Some(0));
    set_speed(&mut s, v, 10.0, Some(2 * S));
    let (it, snd) = (clip(&s, v), clip(&s, a));
    assert!(it.duration.0 > picture.duration.0 * 2, "a slow ramp lengthens the clip: {:?}", it.duration);
    assert_eq!((snd.speed, snd.start, snd.duration, snd.source_in), (1.0, it.start, it.duration, sound.source_in));
    // the media the sound has left from its in point, and where that runs out
    let left = media - snd.source_in;
    assert!(snd.duration > left, "the clip ({:?}) outlasts the sound's media ({left:?})", snd.duration);
    let at = |t: Tick| (t.0 as f64 * sr as f64 / TICKS_PER_SECOND as f64) as i64;
    let media_end = at(snd.start + left);
    assert!(media_end + sr / 2 < at(snd.end()), "a margin of silence inside the clip");
    // up to the media's end it is the source at 100 %: the samples the ramp never touched
    let head = mix(&s, 2 * sr as usize);
    for (x, y) in untouched.channels.iter().zip(&head.channels) {
        let d = x.iter().zip(y).map(|(p, q)| (p - q).abs()).fold(0.0f32, f32::max);
        assert!(d < 1e-6, "the slow ramp retimed the sound: off by {d}");
    }
    // past it, the rest of the clip is silence: no loop, no repeat, no held sample
    let tail = mix_at(&s, media_end + sr / 4, sr as usize / 2);
    assert!(peak(&tail) < 1e-6, "the rest of the clip repeats or holds audio: peak {}", peak(&tail));
}

/// A ramp that ends at no speed (clamped to the slowest there is) still leaves a whole, playing
/// sound: no zero-length clip, no overflow, nothing muted.
#[test]
fn a_ramp_through_no_speed_keeps_the_sound_whole() {
    let mut s = Session::default();
    let (v, a) = one_clip(&mut s);
    for keys in [(0.0, 400.0), (400.0, 0.0), (0.0, 0.0)] {
        set_speed(&mut s, v, keys.0, Some(S));
        set_speed(&mut s, v, keys.1, Some(2 * S));
        let (it, snd) = (clip(&s, v), clip(&s, a));
        assert!(it.duration > Tick::ZERO, "{keys:?}");
        assert!(snd.duration > Tick::ZERO, "{keys:?}: {:?}", snd.duration);
        assert!(snd.start.0.checked_add(snd.duration.0).is_some(), "{keys:?}");
        assert_eq!((snd.speed, rate(&snd)), (1.0, 1.0), "{keys:?}");
        assert!(snd.enabled && !snd.essential.as_ref().is_some_and(|e| e.mute));
        s.active_sequence().unwrap().check().unwrap();
        s.undo();
        s.undo();
    }
}

#[test]
fn a_ramp_through_no_speed_is_undone_in_one_step_per_keyframe() {
    let mut s = Session::default();
    let (v, a) = one_clip(&mut s);
    let (picture, sound) = (clip(&s, v), clip(&s, a));
    set_speed(&mut s, v, 0.0, Some(S));
    set_speed(&mut s, v, 400.0, Some(2 * S));
    let ramped = (clip(&s, v), clip(&s, a));
    s.undo();
    s.undo();
    assert_eq!((clip(&s, v), clip(&s, a)), (picture, sound), "the sound comes back with its picture");
    s.execute("edit.redo", json!({})).unwrap();
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!((clip(&s, v), clip(&s, a)), ramped);
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
    for value in [0.0, -250.0, 10_000.0, -10_000.0, 5e-324] {
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

/// Current command validation rejects values outside the published parameter range before mutation.
#[test]
fn out_of_range_speed_edits_leave_the_project_unchanged() {
    let mut s = Session::default();
    let (v, _) = one_clip(&mut s);
    for value in [1e300, -1e300, 10_001.0, -10_001.0] {
        for keyframe in [false, true] {
            let before = s.project.clone();
            assert!(
                s.execute("effects.setParam", json!({"clip": v.0, "effect": "time_remap", "param": "speed", "value": value, "keyframe": keyframe, "time": S}))
                    .is_err()
            );
            assert_eq!(s.project, before);
        }
    }
}

#[test]
fn interpolation_only_edits_keep_picture_only_ramps_and_linked_sound_timing() {
    let mut s = Session::default();
    let (v, a) = one_clip(&mut s);
    set_speed(&mut s, v, 100.0, Some(0));
    set_speed(&mut s, v, 400.0, Some(S));
    s.execute("timeline.select", json!({"clips": [v.0]})).unwrap();
    let (picture, sound) = (clip(&s, v), clip(&s, a));
    assert!(!sound.is_remapped());
    for params in [json!({"interpolation": "frameBlending"}), json!({"speed": (picture.speed * 10_000.0).round() / 100.0, "interpolation": "opticalFlow"})] {
        s.execute("clip.speedDuration", params).unwrap();
        let (after_v, after_a) = (clip(&s, v), clip(&s, a));
        assert_eq!(
            (after_v.start, after_v.duration, after_v.source_in, after_v.speed, after_v.effect("time_remap")),
            (picture.start, picture.duration, picture.source_in, picture.speed, picture.effect("time_remap"))
        );
        assert_eq!((after_a.start, after_a.duration, after_a.source_in, after_a.speed), (sound.start, sound.duration, sound.source_in, sound.speed));
        s.undo();
    }
    assert!(s.execute("clip.speedDuration", json!({"speed": 50})).is_err());
}

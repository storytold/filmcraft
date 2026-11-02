//! Time Remapping drives the picture: the Speed parameter of the intrinsic Time Remapping effect
//! retimes the media a clip shows — as a keyframed curve keyed on the clip's timeline time — and
//! a rate change retakes the clip's length so its media fits (no still frames past the end). A
//! static speed does not drive playback until the parameter is animated.

use serde_json::json;

use crate::Session;

use filmcraft_project::TrackItem;
use filmcraft_time::Tick;

/// The demo project's last video clip, selected, with linked selection off.
fn selected_clip() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let clip = s.active_sequence().unwrap().video_tracks[0].items.last().unwrap().id.0;
    s.execute("timeline.select", json!({"clips": [clip]})).unwrap();
    s
}

fn clip_of(s: &Session) -> (u64, TrackItem) {
    let q = s.active_sequence().unwrap();
    let it = q.video_tracks[0].items.last().unwrap();
    (it.id.0, it.clone())
}

fn set_speed_key(s: &mut Session, clip: u64, at: i64, value: f64) {
    s.execute("effects.setParam", json!({"clip": clip, "effect": "time_remap", "param": "speed", "value": value, "keyframe": true, "time": at})).unwrap();
}

#[test]
fn a_keyframed_remap_speed_change_retakes_the_clips_length() {
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (start, dur) = (it.start.0, it.duration.0);
    assert_eq!(it.speed, 1.0);
    // a 100 % -> 50 % ramp across the clip: the same media takes longer, so the clip grows to
    // where its media ends instead of leaving media unplayed
    set_speed_key(&mut s, clip, start, 100.0);
    let (_, it) = clip_of(&s);
    assert_eq!(it.duration.0, dur, "a flat 100 % curve changes nothing");
    set_speed_key(&mut s, clip, start + dur, 50.0);
    let (_, it) = clip_of(&s);
    assert!(it.duration.0 > dur, "the slower curve grows the clip: {} -> {}", dur, it.duration.0);
    // the grown clip still shows the media it showed before (the fit's whole point)
    let span = it.source_out().0 - it.source_in.0;
    assert!((span as f64 - dur as f64).abs() <= 2.0, "the same {dur} ticks of media, got {span}");
    // the keyframes sit at clip-relative timeline times
    let p = it.effect("time_remap").unwrap().param("speed").unwrap();
    assert_eq!(p.keyframes.len(), 2);
    assert_eq!(p.keyframes[0].time.0, 0);
    assert_eq!(p.keyframes[1].time.0, dur);
}

#[test]
fn a_faster_remap_ramp_ends_the_clip_where_its_media_ends() {
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (start, dur) = (it.start.0, it.duration.0);
    // a 100 % -> 300 % ramp consumes the clip's media before its timeline end: the fit shortens
    // the clip to the crossing instead of holding a still frame past the media's end
    set_speed_key(&mut s, clip, start, 100.0);
    set_speed_key(&mut s, clip, start + dur, 300.0);
    let (_, it) = clip_of(&s);
    assert!(it.duration.0 < dur, "the faster curve shortens the clip: {} -> {}", dur, it.duration.0);
    let span = it.source_out().0 - it.source_in.0;
    assert!((span as f64 - dur as f64).abs() <= 2.0, "the same {dur} ticks of media, got {span}");
    // the picture still advances across the whole clip
    let (a, b) = (it.source_time_at(it.start), it.source_time_at(it.end() - Tick(1)));
    assert!(b > a, "the picture advances");
}

#[test]
fn a_razor_keeps_a_remapped_clip_continuous() {
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (start, dur) = (it.start, it.duration);
    set_speed_key(&mut s, clip, start.0, 100.0);
    set_speed_key(&mut s, clip, (start + dur).0, 300.0);
    let (_, it) = clip_of(&s);
    let end = it.end();
    let whole: Vec<_> = (0..8)
        .map(|i| {
            let t = start + Tick(i as i64 * it.duration.0 / 8);
            it.source_time_at(t)
        })
        .collect();
    let cut = start + Tick(it.duration.0 / 2);
    s.execute("timeline.razor", json!({"time": cut.0})).unwrap();
    let q = s.active_sequence().unwrap();
    let pieces: Vec<_> = q.video_tracks[0].items.iter().filter(|i| i.start >= start && i.end() <= end).map(|i| i.clone()).collect();
    assert_eq!(pieces.len(), 2, "the clip split in two");
    for (i, m) in whole.iter().enumerate() {
        let t = start + Tick(i as i64 * (end - start).0 / 8);
        let piece = pieces.iter().find(|p| p.start <= t && t < p.end()).expect("a piece covers the time");
        assert!((piece.source_time_at(t).0 - m.0).abs() <= 1, "timeline +{} ticks shows the same frame whole and split", t.0 - start.0);
    }
}

#[test]
fn a_static_remap_speed_stays_inert_until_keyed() {
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (dur, sin) = (it.duration.0, it.source_in.0);
    // without a keyframe the value is stored but does not drive playback (a speed change retakes
    // the length only once the parameter is animated)
    s.execute("effects.setParam", json!({"clip": clip, "effect": "time_remap", "param": "speed", "value": 50.0})).unwrap();
    let (_, it) = clip_of(&s);
    assert_eq!(it.duration.0, dur, "the timeline stays put");
    assert_eq!(it.source_out().0 - it.source_in.0, dur, "the media span is the plain speed's");
    assert_eq!(it.source_in.0, sin);
    // keying the parameter turns the curve on: the same media now takes twice the timeline
    set_speed_key(&mut s, clip, it.start.0, 50.0);
    let (_, it) = clip_of(&s);
    let p = it.effect("time_remap").unwrap().param("speed").unwrap();
    assert!(p.is_animated(), "the curve drives playback now");
    assert_eq!(it.duration.0, dur * 2, "the same media at half rate takes twice the time");
    assert_eq!(it.source_out().0 - it.source_in.0, dur, "the same media as before");
}

#[test]
fn editing_a_remap_keyframe_retakes_the_clips_length() {
    // the graph's value drag goes through effects.setKeyframe: revaluing the speed key to 300 %
    // shortens the clip to where its media ends, like a speed value change does
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (start, dur) = (it.start.0, it.duration.0);
    set_speed_key(&mut s, clip, start, 100.0);
    s.execute("effects.setKeyframe", json!({"clip": clip, "effect": "time_remap", "param": "speed", "mediaTime": 0, "value": 300.0})).unwrap();
    let (_, it) = clip_of(&s);
    assert_eq!(it.duration.0, (dur as f64 / 3.0).round() as i64, "300 % takes a third of the timeline");
    assert_eq!(it.source_out().0 - it.source_in.0, dur, "the same media as before");
}

#[test]
fn moving_a_remap_keyframe_retakes_the_clips_length() {
    // moving the ramp's end key to the middle steepens the curve over the same length: the fit
    // shortens the clip so its media still ends at the clip's end
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (start, dur) = (it.start.0, it.duration.0);
    set_speed_key(&mut s, clip, start, 100.0);
    set_speed_key(&mut s, clip, start + dur, 200.0);
    s.execute("effects.moveKeyframe", json!({"clip": clip, "effect": "time_remap", "param": "speed", "mediaTime": dur, "to": dur / 2})).unwrap();
    let (_, it) = clip_of(&s);
    // the 100->200 ramp now takes half the clip: X consumes dur of media where 2X - dur/4 = dur
    assert!((it.duration.0 as f64 - dur as f64 * 0.625).abs() <= 1.0, "the fit found the ramp's crossing: {}", it.duration.0);
    assert!((it.source_out().0 - it.source_in.0 - dur).abs() <= 2, "the same media as before");
}

#[test]
fn deleting_a_remap_keyframe_retakes_the_clips_length() {
    // deleting the 200 % key leaves the flat 100 % curve: the media takes the original length again
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (start, dur) = (it.start.0, it.duration.0);
    set_speed_key(&mut s, clip, start, 100.0);
    set_speed_key(&mut s, clip, start + dur, 200.0);
    let (_, it) = clip_of(&s);
    assert!(it.duration.0 < dur, "the ramp had shortened the clip: {}", it.duration.0);
    s.execute("effects.deleteKeyframe", json!({"clip": clip, "effect": "time_remap", "param": "speed", "mediaTime": dur})).unwrap();
    let (_, it) = clip_of(&s);
    assert_eq!(it.source_out().0 - it.source_in.0, it.duration.0, "the flat curve ends where its media ends");
    assert!((it.duration.0 - dur).abs() <= 2, "back to the original length: {}", it.duration.0);
}

#[test]
fn holding_a_remap_keyframe_retakes_the_clips_length() {
    // holding the 100 % key stretches its rate across the ramp's segment: the clip grows back to
    // its media's length at 100 %
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (start, dur) = (it.start.0, it.duration.0);
    set_speed_key(&mut s, clip, start, 100.0);
    set_speed_key(&mut s, clip, start + dur, 200.0);
    let (_, it) = clip_of(&s);
    assert!(it.duration.0 < dur, "the ramp had shortened the clip: {}", it.duration.0);
    s.execute("effects.setInterpolation", json!({"clip": clip, "effect": "time_remap", "param": "speed", "mediaTime": 0, "interpolation": "hold"})).unwrap();
    let (_, it) = clip_of(&s);
    assert!((it.duration.0 - dur).abs() <= 1, "the held 100 % consumes its media 1:1 again: {}", it.duration.0);
    assert_eq!(it.source_out().0 - it.source_in.0, it.duration.0, "the same media as before, at the held rate");
}

#[test]
fn toggling_the_remap_stopwatch_on_retakes_the_clips_length() {
    // a static 50 % is stored but inert; switching animation on makes it the curve, and the fit
    // grows the clip to where its media ends at the halved rate instead of leaving the old length
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let dur = it.duration.0;
    s.execute("effects.setParam", json!({"clip": clip, "effect": "time_remap", "param": "speed", "value": 50.0})).unwrap();
    s.execute("effects.toggleAnimation", json!({"clip": clip, "effect": "time_remap", "param": "speed"})).unwrap();
    let (_, it) = clip_of(&s);
    let p = it.effect("time_remap").unwrap().param("speed").unwrap();
    assert!(p.is_animated(), "the stopwatch keyed the curve");
    assert_eq!(it.duration.0, dur * 2, "the same media at half rate takes twice the time");
    assert_eq!(it.source_out().0 - it.source_in.0, dur, "the same media as before");
}

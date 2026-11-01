//! Time Remapping drives the picture: the Speed parameter of the intrinsic Time Remapping effect
//! changes the media a clip shows — statically and as a keyframed curve — while Speed/Duration
//! keeps scaling the base factor, so the two never count each other twice.

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

#[test]
fn time_remap_speed_changes_what_the_clip_shows() {
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (sin, dur) = (it.source_in.0, it.duration.0);
    assert_eq!(it.speed, 1.0);
    // 50 % Time Remapping: half the media over the same timeline span
    s.execute("effects.setParam", json!({"clip": clip, "effect": "time_remap", "param": "speed", "value": 50.0})).unwrap();
    let (_, it) = clip_of(&s);
    assert_eq!((it.source_in.0, it.duration.0, it.speed), (sin, dur, 1.0), "the timeline stays put");
    assert_eq!(it.source_out().0 - it.source_in.0, dur / 2, "half the media");
    // the clip no longer plays natively (its fixed effects are touched)
    let js = s.execute("sequence.inspect", json!({})).unwrap();
    let j = &js["video"][0]["items"].as_array().unwrap().iter().find(|j| j["clip"] == clip).unwrap()["effects"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["effect"] == "time_remap")
        .unwrap()["params"]["speed"];
    assert_eq!(j["value"], json!("Float(50.0)"), "the parameter carries the speed");
    // Speed/Duration composes without counting the remap twice: the dialog's 200 % halves the
    // timeline span, and 200 % x 50 % plays the halved span at 100 %
    s.execute("clip.speedDuration", json!({"speed": 200})).unwrap();
    let (_, it) = clip_of(&s);
    assert_eq!(it.speed, 2.0);
    assert_eq!(it.duration.0, dur / 2, "Speed/Duration halves the timeline span");
    assert_eq!(it.source_out().0 - it.source_in.0, dur / 2, "200 % x 50 % = 100 % of the halved span");
    assert!((it.effective_speed() - 1.0).abs() < 1e-9, "the rates compose to 100 %");
    // back to 100 % Time Remapping: the whole base media again (the halved timeline span at 200 %)
    s.execute("effects.setParam", json!({"clip": clip, "effect": "time_remap", "param": "speed", "value": 100.0})).unwrap();
    let (_, it) = clip_of(&s);
    assert_eq!(it.source_out().0 - it.source_in.0, dur);
}

#[test]
fn time_remap_keyframes_bend_the_speed() {
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (start, dur, sin) = (it.start.0, it.duration.0, it.source_in.0);
    // a keyframe at the clip's In (timeline start, media source_in at 100 %) and one at its out
    // point (media source_in + duration): a 100 % -> 300 % ramp across the clip's media
    s.execute("effects.setParam", json!({"clip": clip, "effect": "time_remap", "param": "speed", "value": 100.0, "keyframe": true, "time": start})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "time_remap", "param": "speed", "value": 300.0, "keyframe": true, "time": start + dur}))
        .unwrap();
    let (_, it) = clip_of(&s);
    assert_eq!((it.start.0, it.duration.0), (start, dur), "the timeline stays put");
    // the ramp integrates: crossing the clip's media at 100 % -> 300 % takes ln(3)/2 of the
    // timeline span, the rest runs at 300 %, so the covered media lands at 1 + 3·(1 − ln3/2)
    let span = (it.source_out().0 - sin) as f64 / dur as f64;
    let want = 1.0 + 3.0 * (1.0 - (3.0f64).ln() / 2.0);
    assert!((span - want).abs() < 0.01, "span {span} vs the integrated {want}");
    // the keyframes sit at the media times the clip showed at the two timeline times
    let p = it.effect("time_remap").unwrap().param("speed").unwrap();
    assert_eq!(p.keyframes.len(), 2);
    assert_eq!(p.keyframes[0].time.0, sin);
    assert_eq!(p.keyframes[1].time.0, sin + dur);
    // the picture at a quarter of the clip still moves forward and covers the ramp
    let quarter = Tick(dur / 4);
    let (a, b) = (it.source_time_at(it.start), it.source_time_at(it.start + quarter));
    assert!(b > a, "the picture advances");
    assert!(b.0 - a.0 < quarter.0 * 3, "the early ramp runs no faster than its end rate");
}

#[test]
fn time_remap_ramp_shows_the_same_media_whole_and_split() {
    let mut s = selected_clip();
    let (clip, it) = clip_of(&s);
    let (start, dur) = (it.start, it.duration);
    // a ramp, then a razor in the middle: both pieces keep showing the same frames
    s.execute("effects.setParam", json!({"clip": clip, "effect": "time_remap", "param": "speed", "value": 100.0, "keyframe": true, "time": start.0})).unwrap();
    s.execute("effects.setParam", json!({"clip": clip, "effect": "time_remap", "param": "speed", "value": 300.0, "keyframe": true, "time": (start + dur).0}))
        .unwrap();
    let whole: Vec<_> = {
        let q = s.active_sequence().unwrap();
        let it = &q.video_tracks[0].items.last().unwrap();
        (0..8).map(|i| it.source_time_at(start + Tick(i as i64 * dur.0 / 8))).collect()
    };
    s.execute("timeline.razor", json!({"time": (start + Tick(dur.0 / 2)).0})).unwrap();
    let q = s.active_sequence().unwrap();
    let pieces: Vec<_> = q.video_tracks[0].items.iter().filter(|i| i.start >= start && i.end() <= start + dur).map(|i| i.clone()).collect();
    assert_eq!(pieces.len(), 2, "the clip split in two");
    for (i, m) in whole.iter().enumerate() {
        let t = start + Tick(i as i64 * dur.0 / 8);
        let piece = pieces.iter().find(|p| p.start <= t && t < p.end()).expect("a piece covers the time");
        assert!((piece.source_time_at(t).0 - m.0).abs() <= 1, "timeline +{} ticks shows the same frame whole and split", t.0 - start.0);
    }
}

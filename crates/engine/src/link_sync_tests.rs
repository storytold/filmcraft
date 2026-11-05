//! `sequence.inspect` reports each linked clip's sync offset from its partners (#4256).

use crate::Session;
use filmcraft_project::ClipId;
use serde_json::{Value, json};

/// A linked picture/sound pair at frame 10 that starts 20 frames into its source.
fn linked_pair() -> (Session, ClipId, ClipId) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sync", "video": 1, "audio": 1})).unwrap();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).unwrap().id;
    let rate = s.sequence_rate();
    s.execute("timeline.place", json!({"item": item.0, "frame": 10, "sourceIn": rate.tick_of(20).0, "duration": rate.tick_of(48).0})).unwrap();
    let q = s.active_sequence().unwrap();
    let (video, sound) = (q.video_tracks[0].items[0].id, q.audio_tracks[0].items[0].id);
    (s, video, sound)
}

fn inspect(s: &mut Session) -> Value {
    s.execute("sequence.inspect", json!({})).unwrap()
}

/// The inspected entry of `clip`.
fn clip(v: &Value, clip: ClipId) -> Value {
    ["video", "audio"]
        .iter()
        .flat_map(|k| v[*k].as_array().unwrap())
        .flat_map(|t| t["items"].as_array().unwrap())
        .find(|i| i["clip"] == clip.0)
        .unwrap()
        .clone()
}

/// `clip`'s offset in frames from `partner` (start and end).
fn offset(s: &mut Session, of: ClipId, from: ClipId) -> (Value, Value) {
    let v = inspect(s);
    let sync = clip(&v, of)["sync"].as_array().unwrap().clone();
    let e = sync.iter().find(|e| e["clip"] == from.0).unwrap();
    (e["offsetFrames"].clone(), e["endOffsetFrames"].clone())
}

fn move_to(s: &mut Session, c: ClipId, frame: i64) {
    let time = s.sequence_rate().tick_of(frame).0;
    s.execute("timeline.move", json!({"moves": [{"clip": c.0, "track": "V1", "time": time}], "insert": false, "linked": false})).unwrap();
}

#[test]
fn a_one_sided_move_shows_on_both_clips_and_moving_back_clears_it() {
    let (mut s, video, sound) = linked_pair();
    assert_eq!(offset(&mut s, video, sound).0, json!(0.0), "in sync");
    let before = s.project.clone();
    inspect(&mut s);
    assert_eq!(s.project, before, "inspecting changes nothing");

    move_to(&mut s, video, 13);
    assert_eq!(offset(&mut s, video, sound), (json!(3.0), Value::Null), "the picture runs 3 frames late");
    assert_eq!(offset(&mut s, sound, video).0, json!(-3.0));
    let v = inspect(&mut s);
    let tick = s.sequence_rate().frame_duration().0;
    assert_eq!(clip(&v, video)["sync"][0]["offset"], json!(3 * tick));

    move_to(&mut s, video, 10);
    assert_eq!(offset(&mut s, video, sound).0, json!(0.0));
    assert_eq!(offset(&mut s, sound, video).0, json!(0.0));
}

#[test]
fn a_regular_trim_keeps_sync_and_a_slip_or_ripple_in_trim_breaks_it() {
    for edge in ["in", "out"] {
        let (mut s, video, sound) = linked_pair();
        s.execute(
            "timeline.trim",
            json!({"clip": video.0, "edge": edge, "mode": "regular", "deltaFrames": if edge == "in" { 4 } else { -4 }, "linked": false}),
        )
        .unwrap();
        assert_eq!(offset(&mut s, video, sound).0, json!(0.0), "regular {edge} trim");
    }
    let (mut s, video, sound) = linked_pair();
    // slip takes no `linked`: with Linked Selection on it slips the partner too
    s.state.linked_selection = false;
    s.execute("timeline.slip", json!({"clip": video.0, "deltaFrames": 5})).unwrap();
    assert_eq!(offset(&mut s, video, sound).0.as_f64().unwrap().abs(), 5.0, "slip");

    let (mut s, video, sound) = linked_pair();
    s.execute("timeline.trim", json!({"clip": video.0, "edge": "in", "mode": "ripple", "deltaFrames": 2, "linked": false})).unwrap();
    assert_eq!(offset(&mut s, video, sound).0.as_f64().unwrap().abs(), 2.0, "ripple In trim");
}

#[test]
fn a_one_sided_rate_stretch_drifts_along_the_clip() {
    let (mut s, video, sound) = linked_pair();
    let delta = s.sequence_rate().tick_of(12).0;
    s.execute("timeline.rateStretch", json!({"clip": video.0, "edge": "out", "delta": delta, "linked": false})).unwrap();
    let (start, end) = offset(&mut s, video, sound);
    assert_eq!(start, json!(0.0), "both start on the same source frame");
    assert!(end.as_f64().unwrap() > 0.5, "the slowed picture falls behind by its end: {end}");
    // the sound at one speed against the picture at another drifts too, seen from its side
    assert!(offset(&mut s, sound, video).1.as_f64().is_some());
}

#[test]
fn every_member_lists_every_other_and_other_media_has_no_offset() {
    let (mut s, video, sound) = linked_pair();
    // a second sound clip from another item joined into the link
    let other = s
        .project
        .items
        .values()
        .find(|i| i.has_audio() && i.as_media().is_some() && i.id != s.active_sequence().unwrap().find_item(video).unwrap().1.item)
        .map(|i| i.id)
        .unwrap();
    s.execute(
        "timeline.place",
        json!({"item": other.0, "track": "V1", "audioTrack": "new", "video": false, "frame": 100, "duration": s.sequence_rate().tick_of(10).0}),
    )
    .unwrap();
    let q = s.active_sequence().unwrap();
    let third = q.audio_tracks.last().unwrap().items[0].id;
    // Link toggles: the first call unlinks the pair, the second links all three
    s.execute("clip.link", json!({"clips": [video.0, sound.0]})).unwrap();
    s.execute("clip.link", json!({"clips": [video.0, sound.0, third.0]})).unwrap();

    let v = inspect(&mut s);
    for c in [video, sound, third] {
        let sync = clip(&v, c)["sync"].as_array().unwrap().clone();
        assert_eq!(sync.len(), 2, "{c:?} lists both others: {sync:?}");
    }
    let e = clip(&v, video)["sync"].as_array().unwrap().iter().find(|e| e["clip"] == third.0).unwrap().clone();
    assert_eq!((e["offset"].clone(), e["offsetFrames"].clone()), (Value::Null, Value::Null), "no shared source");

    s.execute("clip.link", json!({"clips": [video.0, sound.0, third.0]})).unwrap();
    let v = inspect(&mut s);
    assert!(clip(&v, video).get("sync").is_none(), "an unlinked clip has no sync field");
}

#[test]
fn the_timeline_indicators_round_to_whole_frames() {
    let (mut s, video, sound) = linked_pair();
    let rate = s.sequence_rate();
    let marks = |s: &Session| crate::link_sync::out_of_sync_frames(s.active_sequence().unwrap(), rate);
    assert!(marks(&s).is_empty(), "in sync");
    move_to(&mut s, video, 14);
    assert_eq!(marks(&s), [(video, 4), (sound, -4)].into_iter().collect());

    // off the frame grid: under half a frame shows nothing, more rounds to the nearest frame
    let frame = rate.frame_duration().0;
    for (ticks, shown) in [(frame / 3, None), (frame * 2 / 3, Some(1)), (frame * 5 / 2 + 1, Some(3))] {
        s.edit_sequence("nudge", |q, _, _| {
            q.find_item_mut(video).unwrap().1.start = rate.tick_of(10) + filmcraft_time::Tick(ticks);
            Ok(())
        })
        .unwrap();
        assert_eq!(marks(&s).get(&video).copied(), shown, "{ticks} ticks late");
        assert_eq!(marks(&s).get(&sound).copied(), shown.map(|f| -f));
    }
}

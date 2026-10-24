//! #911: a locked track refuses timing and source-range edits the way a locked destination
//! refuses `timeline.move`.

use super::*;
use filmcraft_project::ClipId;
use serde_json::json;

fn fixture() -> (Session, u64) {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Lock check"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "video": 1, "audio": 0, "width": 16, "height": 16})).unwrap();
    s.execute("file.newOfflineFile", json!({"name": "Media", "seconds": 4, "fps": 25, "video": true, "audio": false, "width": 16, "height": 16})).unwrap();
    let item = s.project.items.values().find(|i| i.as_media().is_some()).map(|i| i.id.0).unwrap();
    s.execute("timeline.place", json!({"item": item, "track": "V1", "time": 0, "sourceIn": 0, "duration": 1_016_064_000_000i64})).unwrap();
    let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    (s, clip)
}

fn v1_clip(s: &Session, id: u64) -> filmcraft_project::TrackItem {
    s.active_sequence().unwrap().find_item(ClipId(id)).unwrap().1.clone()
}

/// Place a 100-frame clip, lock V1, trim Out by −20 frames: the command errors and duration stays 100.
#[test]
fn locked_track_refuses_trim_out_and_keeps_duration() {
    let (mut s, clip) = fixture();
    let before = v1_clip(&s, clip);
    assert_eq!(s.sequence_rate().frame_at(before.duration), 100);
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    let err = s.execute("timeline.trim", json!({"clip": clip, "edge": "out", "mode": "regular", "deltaFrames": -20})).unwrap_err();
    assert!(err.to_string().contains("locked"), "{err}");
    let after = v1_clip(&s, clip);
    assert_eq!(after.duration, before.duration);
    assert_eq!(after.start, before.start);
    assert_eq!(after.source_in, before.source_in);
    assert_eq!(after.speed, before.speed);
}

#[test]
fn unlocked_track_still_trims_out() {
    let (mut s, clip) = fixture();
    s.execute("timeline.trim", json!({"clip": clip, "edge": "out", "mode": "regular", "deltaFrames": -20})).unwrap();
    assert_eq!(s.sequence_rate().frame_at(v1_clip(&s, clip).duration), 80);
}

#[test]
fn locked_track_refuses_move_on_the_same_track() {
    let (mut s, clip) = fixture();
    let before = v1_clip(&s, clip);
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    let err = s.execute("timeline.move", json!({"moves": [{"clip": clip, "track": "V1", "time": 1_016_064_000_000i64}], "linked": false})).unwrap_err();
    assert!(err.to_string().contains("locked"), "{err}");
    assert_eq!(v1_clip(&s, clip).duration, before.duration);
}

#[test]
fn locked_track_refuses_roll_slip_slide_speed() {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Lock check"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "video": 1, "audio": 0, "width": 16, "height": 16})).unwrap();
    s.execute("file.newOfflineFile", json!({"name": "Media", "seconds": 4, "fps": 25, "video": true, "audio": false, "width": 16, "height": 16})).unwrap();
    let item = s.project.items.values().find(|i| i.as_media().is_some()).map(|i| i.id.0).unwrap();
    let frame = 10_160_640_000i64;
    s.execute("timeline.place", json!({"item": item, "track": "V1", "time": 0, "sourceIn": frame * 10, "duration": frame * 60})).unwrap();
    s.execute("timeline.place", json!({"item": item, "track": "V1", "time": frame * 60, "sourceIn": 0, "duration": frame * 100})).unwrap();
    let (left, right) = {
        let q = s.active_sequence().unwrap();
        (q.video_tracks[0].items[0].id.0, q.video_tracks[0].items[1].id.0)
    };
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    let snap = s.project.clone();
    for cmd in [
        json!({"id": "timeline.roll", "p": {"left": left, "right": right, "deltaFrames": 10}}),
        json!({"id": "timeline.slip", "p": {"clip": left, "deltaFrames": -10}}),
        json!({"id": "timeline.slide", "p": {"clip": left, "deltaFrames": 10}}),
        json!({"id": "timeline.rateStretch", "p": {"clip": left, "edge": "out", "deltaFrames": 20}}),
        json!({"id": "clip.speedDuration", "p": {"clips": [left], "speed": 200}}),
        json!({"id": "timeline.trim", "p": {"clip": left, "edge": "in", "mode": "regular", "deltaFrames": 20}}),
    ] {
        let err = s.execute(cmd["id"].as_str().unwrap(), cmd["p"].clone()).unwrap_err();
        assert!(err.to_string().contains("locked"), "{}: {err}", cmd["id"]);
        assert_eq!(*s.project, *snap, "{} must leave the sequence unchanged", cmd["id"]);
    }
}

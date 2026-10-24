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

fn linked_slide_fixture(reversed_neighbours: bool) -> (Session, u64, u64) {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Linked slide lock"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "video": 1, "audio": 1, "width": 16, "height": 16})).unwrap();
    let item =
        s.execute("file.newOfflineFile", json!({"name": "Media", "seconds": 4, "fps": 25, "video": true, "audio": true})).unwrap()["item"].as_u64().unwrap();
    let frame = s.sequence_rate().tick_of(1).0;
    for start in [0, 12, 24] {
        s.execute(
            "timeline.place",
            json!({"item": item, "track": "V1", "audioTrack": "A1", "time": start * frame, "sourceIn": (start + 6) * frame, "duration": 12 * frame}),
        )
        .unwrap();
    }
    if reversed_neighbours {
        let clips: Vec<u64> = s.active_sequence().unwrap().video_tracks[0].items.iter().step_by(2).map(|i| i.id.0).collect();
        s.execute("clip.speedDuration", json!({"clips": clips, "speed": 100, "reverse": true})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    let (v, a) = (q.video_tracks[0].items[1].id.0, q.audio_tracks[0].items[1].id.0);
    assert!(q.video_tracks[0].items[1].link.is_some());
    assert_eq!(q.video_tracks[0].items[1].link, q.audio_tracks[0].items[1].link);
    (s, v, a)
}

#[test]
fn linked_slide_rejects_any_locked_partner_without_changing_project_or_history() {
    for reversed in [false, true] {
        for locks in [vec!["V1"], vec!["A1"], vec!["V1", "A1"]] {
            for audio_target in [false, true] {
                for delta in [-3, 3] {
                    let (mut s, v, a) = linked_slide_fixture(reversed);
                    for track in &locks {
                        s.execute("timeline.setTrack", json!({"track": track, "locked": true})).unwrap();
                    }
                    let before = (*s.project).clone();
                    let history = s.execute("history.list", json!({})).unwrap();
                    let err = s.execute("timeline.slide", json!({"clip": if audio_target { a } else { v }, "deltaFrames": delta})).unwrap_err();
                    assert!(err.to_string().contains("locked"), "{err}");
                    assert_eq!(*s.project, before, "locked linked slide must be all-or-nothing");
                    assert_eq!(s.execute("history.list", json!({})).unwrap(), history);
                }
            }
        }
    }
}

#[test]
fn linked_slide_matches_keyboard_lock_rejection() {
    for track in ["V1", "A1"] {
        for command in ["timeline.slideLeft", "timeline.slideRight", "timeline.slideLeft5", "timeline.slideRight5"] {
            let (mut s, v, _) = linked_slide_fixture(false);
            s.execute("timeline.select", json!({"clips": [v]})).unwrap();
            s.execute("timeline.setTrack", json!({"track": track, "locked": true})).unwrap();
            let before = (*s.project).clone();
            let history = s.execute("history.list", json!({})).unwrap();
            assert!(s.execute(command, json!({})).unwrap_err().to_string().contains("locked"));
            assert_eq!(*s.project, before);
            assert_eq!(s.execute("history.list", json!({})).unwrap(), history);
        }
    }
}

#[test]
fn linked_slide_after_unlock_moves_both_partners_and_preserves_undo_redo() {
    for reversed in [false, true] {
        for track in ["V1", "A1"] {
            for delta in [-3, 3, -300, 300] {
                let (mut s, v, a) = linked_slide_fixture(reversed);
                s.execute("timeline.setTrack", json!({"track": track, "locked": true})).unwrap();
                assert!(s.execute("timeline.slide", json!({"clip": v, "deltaFrames": delta})).is_err());
                s.execute("timeline.setTrack", json!({"track": track, "locked": false})).unwrap();
                let before = (*s.project).clone();
                s.execute("timeline.slide", json!({"clip": v, "deltaFrames": delta})).unwrap();
                let after = (*s.project).clone();
                let q = s.active_sequence().unwrap();
                q.check().unwrap();
                let picture = q.find_item(ClipId(v)).unwrap().1;
                let sound = q.find_item(ClipId(a)).unwrap().1;
                assert_ne!(picture.start, s.sequence_rate().tick_of(12));
                assert_eq!((picture.start, picture.duration, picture.source_in, picture.link), (sound.start, sound.duration, sound.source_in, sound.link));
                s.execute("edit.undo", json!({})).unwrap();
                assert_eq!(*s.project, before);
                s.execute("edit.redo", json!({})).unwrap();
                assert_eq!(*s.project, after);
            }
        }
    }
}

#[test]
fn linked_slide_with_linked_selection_off_still_edits_only_the_requested_clip() {
    for audio_target in [false, true] {
        let (mut s, v, a) = linked_slide_fixture(false);
        s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
        let (target, locked, frozen) = if audio_target { (a, "V1", v) } else { (v, "A1", a) };
        s.execute("timeline.setTrack", json!({"track": locked, "locked": true})).unwrap();
        let fixed = v1_clip(&s, frozen);
        s.execute("timeline.slide", json!({"clip": target, "deltaFrames": 3})).unwrap();
        assert_eq!(v1_clip(&s, target).start, s.sequence_rate().tick_of(15));
        assert_eq!(v1_clip(&s, frozen), fixed);
        let before = (*s.project).clone();
        assert!(s.execute("timeline.slide", json!({"clip": frozen, "deltaFrames": 3})).unwrap_err().to_string().contains("locked"));
        assert_eq!(*s.project, before);
    }
}

#[test]
fn linked_slide_explicitly_unlinked_clip_does_not_require_other_track_unlocked() {
    let (mut s, v, a) = linked_slide_fixture(false);
    s.execute("clip.link", json!({"clips": [v, a]})).unwrap();
    assert!(v1_clip(&s, v).link.is_none());
    assert!(v1_clip(&s, a).link.is_none());
    s.execute("timeline.setTrack", json!({"track": "A1", "locked": true})).unwrap();
    let frozen = s.active_sequence().unwrap().audio_tracks[0].clone();
    s.execute("timeline.slide", json!({"clip": v, "deltaFrames": 3})).unwrap();
    assert_eq!(v1_clip(&s, v).start, s.sequence_rate().tick_of(15));
    assert_eq!(s.active_sequence().unwrap().audio_tracks[0], frozen);
}

#[test]
fn linked_slide_missing_clip_is_atomic() {
    let (mut s, _, _) = linked_slide_fixture(false);
    let before = (*s.project).clone();
    let history = s.execute("history.list", json!({})).unwrap();
    assert!(s.execute("timeline.slide", json!({"clip": u64::MAX, "deltaFrames": 3})).is_err());
    assert_eq!(*s.project, before);
    assert_eq!(s.execute("history.list", json!({})).unwrap(), history);
}

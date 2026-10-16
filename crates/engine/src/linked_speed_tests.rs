//! `clip.speedDuration` with `ripple` on a clip that has linked sound: the clip and its sound
//! change as one edit, and the later clips move once. Maintain Audio Pitch goes with them.

use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn range(s: &Session, c: ClipId) -> (Tick, Tick) {
    s.active_sequence().unwrap().find_item(c).map(|(_, i)| (i.start, i.end())).unwrap()
}

/// Three 48-frame clips in a row, pictures on V1 and their linked sounds on A1: [(picture, sound); 3].
fn three_clips(s: &mut Session) -> [(ClipId, ClipId); 3] {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Speed", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    for n in 0..3 {
        s.execute("timeline.place", json!({"item": item.0, "frame": 48 * n, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    [0, 1, 2].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id))
}

/// Slower with ripple moved the later clips twice (once for the picture, once for its sound) and
/// left a double gap.
#[test]
fn slower_with_ripple_moves_later_clips_once() {
    let mut s = Session::default();
    let [_, (picture2, sound2), (picture3, sound3)] = three_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f48, f144, f192) = (f(48), f(144), f(192));
    let before = s.project.clone();
    s.execute("clip.speedDuration", json!({"clips": [picture2.0], "speed": 50, "ripple": true})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    assert_eq!((range(&s, picture2), range(&s, sound2)), ((f48, f144), (f48, f144)), "the clip and its sound are twice as long");
    assert_eq!((range(&s, picture3), range(&s, sound3)), ((f144, f192), (f144, f192)), "the next clip follows, once");
    assert_eq!(q.duration(), f192);
    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before, "one undo step restores it");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(*s.project, *after);
}

/// Faster with ripple was refused when more than one clip followed: the second ripple pulled the
/// farther clips onto the nearer ones.
#[test]
fn faster_with_ripple_moves_later_clips_once() {
    let mut s = Session::default();
    let [(picture1, sound1), (picture2, sound2), (picture3, sound3)] = three_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f0, f24, f72, f120) = (f(0), f(24), f(72), f(120));
    s.execute("clip.speedDuration", json!({"clips": [picture1.0], "speed": 200, "ripple": true})).unwrap();
    s.active_sequence().unwrap().check().unwrap();
    for (picture, sound, at) in [(picture1, sound1, (f0, f24)), (picture2, sound2, (f24, f72)), (picture3, sound3, (f72, f120))] {
        assert_eq!((range(&s, picture), range(&s, sound)), (at, at));
    }
}

/// A clip that is not in the sequence is an error and nothing changes, also next to one that is.
#[test]
fn speed_on_a_missing_clip_is_an_error() {
    let mut s = Session::default();
    let [_, (picture2, _), _] = three_clips(&mut s);
    let before = s.project.clone();
    let undo = s.history.undo.len();
    assert!(s.execute("clip.speedDuration", json!({"clips": [999_999], "speed": 50, "ripple": true})).is_err());
    assert!(s.execute("clip.speedDuration", json!({"clips": [picture2.0, 999_999], "speed": 50, "ripple": true})).is_err());
    assert_eq!(*s.project, *before);
    assert_eq!(s.history.undo.len(), undo);
}

/// Several clips in one call each ripple in turn, as before.
#[test]
fn speed_with_ripple_on_two_clips_ripples_each() {
    let mut s = Session::default();
    let [(picture1, sound1), (picture2, sound2), (picture3, sound3)] = three_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f0, f24, f48, f96) = (f(0), f(24), f(48), f(96));
    s.execute("clip.speedDuration", json!({"clips": [picture1.0, picture2.0], "speed": 200, "ripple": true})).unwrap();
    s.active_sequence().unwrap().check().unwrap();
    for (picture, sound, at) in [(picture1, sound1, (f0, f24)), (picture2, sound2, (f24, f48)), (picture3, sound3, (f48, f96))] {
        assert_eq!((range(&s, picture), range(&s, sound)), (at, at));
    }
}

/// Without `ripple` nothing after the clip moves.
#[test]
fn speed_without_ripple_leaves_later_clips() {
    let mut s = Session::default();
    let [_, (picture2, sound2), (picture3, sound3)] = three_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f48, f72, f96, f144) = (f(48), f(72), f(96), f(144));
    s.execute("clip.speedDuration", json!({"clips": [picture2.0], "speed": 200})).unwrap();
    assert_eq!((range(&s, picture2), range(&s, sound2)), ((f48, f72), (f48, f72)));
    assert_eq!((range(&s, picture3), range(&s, sound3)), ((f96, f144), (f96, f144)));
}

/// Linked Selection off and both the picture and its sound named: still one edit, one ripple.
#[test]
fn speed_with_ripple_on_a_named_picture_and_sound() {
    let mut s = Session::default();
    let [_, (picture2, sound2), (picture3, sound3)] = three_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f48, f144, f192) = (f(48), f(144), f(192));
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("clip.speedDuration", json!({"clips": [picture2.0, sound2.0], "speed": 50, "ripple": true})).unwrap();
    assert_eq!((range(&s, picture2), range(&s, sound2)), ((f48, f144), (f48, f144)));
    assert_eq!((range(&s, picture3), range(&s, sound3)), ((f144, f192), (f144, f192)));
}

/// Two clips of one track that were linked to each other (Clip ▸ Link) ripple one after the
/// other, as unlinked clips do.
#[test]
fn speed_with_ripple_on_linked_clips_of_one_track() {
    let mut s = Session::default();
    let [(picture1, _), (picture2, _), (picture3, _)] = three_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f0, f96, f192, f240) = (f(0), f(96), f(192), f(240));
    s.execute("timeline.setTrack", json!({"track": "A1", "syncLock": false})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.select", json!({"clips": [picture1.0, picture2.0]})).unwrap();
    // both are linked to their sounds, so the first call unlinks them and the second links the two
    s.execute("clip.link", json!({})).unwrap();
    assert_eq!(s.execute("clip.link", json!({})).unwrap()["linked"], true);
    s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
    s.execute("clip.speedDuration", json!({"clips": [picture1.0], "speed": 50, "ripple": true})).unwrap();
    s.active_sequence().unwrap().check().unwrap();
    assert_eq!((range(&s, picture1), range(&s, picture2), range(&s, picture3)), ((f0, f96), (f96, f192), (f192, f240)));
}

/// A split edit: the sound runs 12 frames longer than its picture. Slower, the later clips move
/// by the sound's change, so nothing lands on the sound and the next clip stays in sync.
#[test]
fn speed_with_ripple_on_a_split_edit() {
    let mut s = Session::default();
    let [_, (picture2, sound2), (picture3, sound3)] = three_clips(&mut s);
    let f = |n: i64| s.sequence_rate().tick_of(n);
    let (f48, f144, f156, f168) = (f(48), f(144), f(156), f(168));
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.trim", json!({"clip": sound3.0, "edge": "in", "mode": "regular", "deltaFrames": 12})).unwrap();
    s.execute("timeline.trim", json!({"clip": sound2.0, "edge": "out", "mode": "regular", "deltaFrames": 12})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
    s.execute("clip.speedDuration", json!({"clips": [picture2.0], "speed": 50, "ripple": true})).unwrap();
    s.active_sequence().unwrap().check().unwrap();
    assert_eq!((range(&s, picture2), range(&s, sound2)), ((f48, f144), (f48, f168)), "picture 48 to 96 frames, sound 60 to 120");
    assert_eq!((range(&s, picture3).0, range(&s, sound3).0), (f156, f168), "the next sound still starts 12 after its picture");
}

/// Speed/Duration ▸ Maintain Audio Pitch is set on the clip and its linked sound, kept when a later
/// change leaves it out, shown by `sequence.inspect`, undone in one step and saved with the project.
#[test]
fn maintain_audio_pitch_goes_with_the_clip_and_its_sound() {
    let mut s = Session::default();
    let [(picture, sound), (other, _), _] = three_clips(&mut s);
    let flag = |s: &Session, c: ClipId| s.active_sequence().and_then(|q| q.find_item(c)).is_some_and(|(_, i)| i.maintain_pitch);
    let before = s.project.clone();
    s.execute("clip.speedDuration", json!({"clips": [picture.0], "speed": 150, "maintainPitch": true})).unwrap();
    assert!(flag(&s, picture) && flag(&s, sound));
    assert!(!flag(&s, other));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before, "one undo step");
    s.execute("edit.redo", json!({})).unwrap();
    // left out: unchanged; `false`: off
    s.execute("clip.speedDuration", json!({"clips": [picture.0], "speed": 120})).unwrap();
    assert!(flag(&s, sound));
    let inspect = s.execute("sequence.inspect", json!({})).unwrap();
    let shown = inspect["audio"][0]["items"].as_array().unwrap().iter().find(|c| c["clip"] == json!(sound.0)).unwrap().clone();
    assert_eq!(shown["maintainPitch"], json!(true), "{shown}");
    // saved with the project and read back
    let path = std::env::temp_dir().join(format!("fc-maintain-pitch-{}.fcproj", std::process::id()));
    s.execute("file.saveAs", json!({"path": path.to_string_lossy()})).unwrap();
    let mut opened = Session::default();
    opened.execute("file.open", json!({"path": path.to_string_lossy()})).unwrap();
    let _ = std::fs::remove_file(&path);
    let saved = |c: ClipId| opened.project.sequences().filter_map(|i| i.as_sequence()).find_map(|q| q.find_item(c)).map(|(_, i)| i.maintain_pitch);
    assert_eq!((saved(picture), saved(sound), saved(other)), (Some(true), Some(true), Some(false)));
    s.execute("clip.speedDuration", json!({"clips": [picture.0], "speed": 120, "maintainPitch": false})).unwrap();
    assert!(!flag(&s, picture) && !flag(&s, sound));
}

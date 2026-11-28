//! A ripple trim ahead of a split edit: the later clip's sound starts before its picture (a J cut)
//! and has to move with it.

use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

use crate::Session;

fn start(s: &Session, c: ClipId) -> Tick {
    s.active_sequence().unwrap().find_item(c).map(|(_, i)| i.start).unwrap()
}

/// Two clips in a row on V1. The first has its sound on A1, the second on A2, and the second's
/// sound starts 12 frames before its picture. Returns (first picture, second picture, second sound).
fn j_cut(s: &mut Session) -> (ClipId, ClipId, ClipId) {
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "J cut", "video": 1, "audio": 2})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    let (source_in, len) = (rate.tick_of(48).0, rate.tick_of(48).0);
    s.execute("timeline.place", json!({"item": item.0, "track": "V1", "audioTrack": "A1", "frame": 0, "sourceIn": source_in, "duration": len})).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "track": "V1", "audioTrack": "A2", "frame": 48, "sourceIn": source_in, "duration": len})).unwrap();
    let q = s.active_sequence().unwrap();
    let (first, second, sound) = (q.video_tracks[0].items[0].id, q.video_tracks[0].items[1].id, q.audio_tracks[1].items[0].id);
    // the split: only the sound's In edge moves, so Linked Selection is off for this one trim
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.trim", json!({"clip": sound.0, "edge": "in", "mode": "regular", "deltaFrames": -12})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
    assert_eq!(start(s, second) - start(s, sound), rate.tick_of(12), "the sound leads its picture by 12 frames");
    (first, second, sound)
}

/// Lengthening the clip before a J cut moved the later picture and left its sound behind: a lead
/// of 12 frames became 20, with no error.
#[test]
fn lengthening_before_a_j_cut_keeps_the_sound_with_its_picture() {
    ripple_and_check(8);
}

/// Shortening it was refused as a sync-lock conflict, because the early sound sat in the stretch
/// that closes.
#[test]
fn shortening_before_a_j_cut_keeps_the_sound_with_its_picture() {
    ripple_and_check(-8);
}

fn ripple_and_check(frames: i64) {
    let mut s = Session::default();
    let (first, second, sound) = j_cut(&mut s);
    let rate = s.sequence_rate();
    let (picture_at, sound_at) = (start(&s, second), start(&s, sound));
    let before = s.project.clone();
    let r = s.execute("timeline.trim", json!({"clip": first.0, "edge": "out", "mode": "ripple", "deltaFrames": frames})).unwrap();
    let by = rate.tick_of(frames);
    assert_eq!(r["delta"], json!(by.0));
    assert_eq!(start(&s, second), picture_at + by, "the later picture ripples");
    assert_eq!(start(&s, sound), sound_at + by, "its sound still leads by 12 frames");
    s.active_sequence().unwrap().check().unwrap();
    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(**s.project, *before, "one undo step puts both back");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(**s.project, *after);
}

/// The early sound has no room to follow: the sequence start, or another clip right before it on
/// its track. The trim is refused and nothing moves.
#[test]
fn ripple_trim_is_refused_when_the_early_sound_has_no_room() {
    let mut s = Session::default();
    let (first, second, sound) = j_cut(&mut s);
    let rate = s.sequence_rate();
    let before = s.project.clone();
    // the sound starts at frame 36: 40 frames earlier is before the sequence starts
    let e = s.execute("timeline.trim", json!({"clip": first.0, "edge": "out", "mode": "ripple", "deltaFrames": -40})).unwrap_err();
    assert!(e.to_string().contains("sync"), "{e}");
    assert_eq!(**s.project, *before, "a refused trim changes nothing");
    let tone = s.project.items.values().find(|i| !i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    s.execute("timeline.place", json!({"item": tone.0, "audioTrack": "A2", "frame": 12, "duration": rate.tick_of(24).0})).unwrap();
    let before = s.project.clone();
    assert_eq!(s.active_sequence().unwrap().audio_tracks[1].items[0].end(), start(&s, sound));
    let e = s.execute("timeline.trim", json!({"clip": first.0, "edge": "out", "mode": "ripple", "deltaFrames": -8})).unwrap_err();
    assert!(e.to_string().contains("sync"), "{e}");
    assert_eq!(**s.project, *before, "a refused trim changes nothing");
    assert_eq!(start(&s, second) - start(&s, sound), rate.tick_of(12));
}

/// The mirror, an L cut: the first clip's sound runs 6 frames on into the second picture. Shortening
/// the second clip's head was refused as a sync-lock conflict, because the first clip's sound crossed
/// the cut. The first clip never moves, so its sound stays and the L cut still ends where it did.
#[test]
fn shortening_the_head_after_an_l_cut_keeps_the_earlier_sound_in_place() {
    let mut s = Session::default();
    let (first, second, sound) = j_cut(&mut s);
    let rate = s.sequence_rate();
    // undo the J cut, then run the first clip's sound (A1) 6 frames past its picture
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.trim", json!({"clip": sound.0, "edge": "in", "mode": "regular", "deltaFrames": 12})).unwrap();
    let first_sound = s.active_sequence().unwrap().audio_tracks[0].items[0].id;
    s.execute("timeline.trim", json!({"clip": first_sound.0, "edge": "out", "mode": "regular", "deltaFrames": 6})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
    let end = |s: &Session, c: ClipId| s.active_sequence().unwrap().find_item(c).map(|(_, i)| i.end()).unwrap();
    assert_eq!(end(&s, first_sound), start(&s, second) + rate.tick_of(6), "the L cut runs 6 frames into the second picture");
    let (l_cut_ends, picture_at, picture_ends) = (end(&s, first_sound), start(&s, second), end(&s, second));
    let before = s.project.clone();
    s.execute("timeline.trim", json!({"clip": second.0, "edge": "in", "mode": "ripple", "deltaFrames": 3})).unwrap();
    assert_eq!(end(&s, first_sound), l_cut_ends, "the L cut still ends where it did");
    assert_eq!((start(&s, first), start(&s, first_sound)), (Tick::ZERO, Tick::ZERO));
    assert_eq!((start(&s, second), end(&s, second)), (picture_at, picture_ends - rate.tick_of(3)), "the second picture is 3 frames shorter");
    assert_eq!((start(&s, sound), end(&s, sound)), (start(&s, second), end(&s, second)), "its own sound with it");
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(**s.project, *before, "one undo step puts it back");
}

/// A shorter head on the J-cut take itself: the picture and its early sound lose the same stretch
/// and the sound still leads by 12. It was refused as a sync-lock conflict because the check looked
/// at the stretch before the cut, where the first clip's sound sits on A1, instead of the stretch
/// the shorter head takes away.
#[test]
fn shorter_head_on_a_j_cut_take_shortens_picture_and_sound_together() {
    shorter_head_on_a_j_cut_take(false);
}

/// The same trim grabbed by the early sound instead of the picture gives the same result.
#[test]
fn shorter_head_grabbed_by_the_early_sound_gives_the_same_result() {
    shorter_head_on_a_j_cut_take(true);
}

fn shorter_head_on_a_j_cut_take(grab_sound: bool) {
    let mut s = Session::default();
    let (first, second, sound) = j_cut(&mut s);
    let rate = s.sequence_rate();
    let len = |s: &Session, c: ClipId| s.active_sequence().unwrap().find_item(c).map(|(_, i)| (i.duration, i.source_in)).unwrap();
    let (picture_at, sound_at) = (start(&s, second), start(&s, sound));
    let ((pic_len, pic_in), (snd_len, snd_in)) = (len(&s, second), len(&s, sound));
    let first_before = len(&s, first);
    let before = s.project.clone();
    let r = s.execute("timeline.trim", json!({"clip": if grab_sound { sound.0 } else { second.0 }, "edge": "in", "mode": "ripple", "deltaFrames": 8})).unwrap();
    let by = rate.tick_of(8);
    assert_eq!(r["delta"], json!(by.0));
    assert_eq!((start(&s, second), start(&s, sound)), (picture_at, sound_at), "both stay where they start");
    assert_eq!(len(&s, second), (pic_len - by, pic_in + by), "the picture loses 8 from its head");
    assert_eq!(len(&s, sound), (snd_len - by, snd_in + by), "its sound loses the same 8");
    assert_eq!(start(&s, second) - start(&s, sound), rate.tick_of(12), "the sound still leads by 12");
    assert_eq!(len(&s, first), first_before, "the clip before is untouched");
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(**s.project, *before, "one undo step puts both back");
}

/// Material on a sync-locked track inside the stretch a shorter head takes away still refuses it.
#[test]
fn shorter_head_is_refused_when_a_sync_locked_track_has_material_in_the_stretch() {
    let mut s = Session::default();
    let (_, second, _) = j_cut(&mut s);
    let rate = s.sequence_rate();
    let tone = s.project.items.values().find(|i| !i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    s.execute("timeline.place", json!({"item": tone.0, "audioTrack": "A1", "frame": 50, "duration": rate.tick_of(24).0})).unwrap();
    let before = s.project.clone();
    let e = s.execute("timeline.trim", json!({"clip": second.0, "edge": "in", "mode": "ripple", "deltaFrames": 8})).unwrap_err();
    assert!(e.to_string().contains("sync"), "{e}");
    assert_eq!(**s.project, *before, "a refused trim changes nothing");
}

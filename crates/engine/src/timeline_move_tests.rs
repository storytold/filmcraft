//! `timeline.move`: linked partners follow, locked partners stay, the group stops together at the
//! sequence start, and hostile times are refused.

use filmcraft_project::{ClipId, TrackId, TrackItem};
use filmcraft_time::Tick;
use serde_json::json;

use crate::{EngineError, Session};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn state(s: &Session, c: ClipId) -> (TrackId, Tick) {
    s.active_sequence().unwrap().find_item(c).map(|(t, i)| (t, i.start)).unwrap()
}

/// The last picture clip of V1 and its linked sound: (V1, picture, sound's track, sound).
fn last_pair(s: &Session) -> (TrackId, TrackItem, TrackId, TrackItem) {
    let q = s.active_sequence().unwrap();
    let v = q.video_tracks[0].items.last().unwrap().clone();
    let (a_track, a) = q.audio_tracks.iter().find_map(|t| t.items.iter().find(|i| i.link == v.link && v.link.is_some()).map(|i| (t.id, i.clone()))).unwrap();
    (q.video_tracks[0].id, v, a_track, a)
}

/// An empty sequence with one linked picture + sound pair placed at `seconds`: (V1, picture, A1, sound).
fn pair_in_an_empty_sequence(s: &mut Session, seconds: f64) -> (TrackId, TrackItem, TrackId, TrackItem) {
    s.execute("file.newSequence", json!({"name": "Moves", "video": 2, "audio": 2})).unwrap();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).unwrap();
    s.execute("timeline.place", json!({"item": item.0, "seconds": seconds})).unwrap();
    let q = s.active_sequence().unwrap();
    let (v, a) = (q.video_tracks[0].items[0].clone(), q.audio_tracks[0].items[0].clone());
    assert!(v.link.is_some() && v.link == a.link, "a placed clip's picture and sound are linked");
    (q.video_tracks[0].id, v, q.audio_tracks[0].id, a)
}

/// `timeline.move` of one clip left its linked sound behind although Linked Selection was on.
#[test]
fn move_takes_linked_partners_along() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let (v_track, v, a_track, a) = last_pair(&s);
    let end = s.active_sequence().unwrap().duration();
    assert!(s.state.linked_selection, "linked selection is on by default");
    let before = s.project.clone();
    // only the picture is listed: the sound follows by the same offset, on its own track
    let to = end + rate.tick_of(48);
    let r = s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "V1", "time": to.0}]})).unwrap();
    assert_eq!(state(&s, v.id), (v_track, to));
    assert_eq!(state(&s, a.id), (a_track, a.start + (to - v.start)), "linked audio stays in sync");
    assert_eq!(r["moved"], json!([v.id.0, a.id.0]));
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before, "one undo step restores both clips");
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!((state(&s, v.id).1, state(&s, a.id).1), (to, a.start + (to - v.start)));
    s.execute("edit.undo", json!({})).unwrap();
    // listing both moves each exactly once
    let (tv, ta) = (to, a.start + (to - v.start));
    let r = s
        .execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "V1", "time": tv.0}, {"clip": a.id.0, "track": a_track.0, "time": ta.0}]}))
        .unwrap();
    assert_eq!((state(&s, v.id), state(&s, a.id)), ((v_track, tv), (a_track, ta)));
    assert_eq!(r["moved"], json!([v.id.0, a.id.0]));
    s.execute("edit.undo", json!({})).unwrap();
    // `linked: false` moves just the listed clip, whatever the toggle says
    let r = s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "V1", "time": to.0}], "linked": false})).unwrap();
    assert_eq!((state(&s, v.id), state(&s, a.id)), ((v_track, to), (a_track, a.start)));
    assert_eq!(r["moved"], json!([v.id.0]));
    s.execute("edit.undo", json!({})).unwrap();
    // linked selection off: unchanged behaviour, only the listed clip moves
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "V1", "time": to.0}]})).unwrap();
    assert_eq!((state(&s, v.id), state(&s, a.id)), ((v_track, to), (a_track, a.start)));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
    // ...unless the command asks for it
    s.execute("timeline.move", json!({"moves": [{"clip": a.id.0, "track": a_track.0, "time": ta.0}], "linked": true})).unwrap();
    assert_eq!((state(&s, v.id), state(&s, a.id)), ((v_track, tv), (a_track, ta)), "moving the sound takes the picture along");
}

/// A partner on a locked track cannot move; that must not refuse the move of the listed clip.
#[test]
fn a_linked_partner_on_a_locked_track_stays_and_the_listed_clip_still_moves() {
    let mut s = demo();
    let (v_track, v, a_track, a) = last_pair(&s);
    let to = s.active_sequence().unwrap().duration() + s.sequence_rate().tick_of(48);
    s.execute("timeline.setTrack", json!({"track": a_track.0, "locked": true})).unwrap();
    let r = s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "V1", "time": to.0}]})).unwrap();
    assert_eq!(r["moved"], json!([v.id.0]), "the locked sound is not among the moved clips");
    assert_eq!((state(&s, v.id), state(&s, a.id)), ((v_track, to), (a_track, a.start)));
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(state(&s, v.id), (v_track, v.start));
    // a listed clip whose own destination is locked is still refused, and nothing moves
    let before = s.project.clone();
    assert!(s.execute("timeline.move", json!({"moves": [{"clip": a.id.0, "track": a_track.0, "time": to.0}]})).is_err());
    assert_eq!(*s.project, *before);
}

/// Moving a clip off a locked track used to leave it there and place another item with the same
/// clip id on the destination (#917). The source is locked, so the move is refused.
#[test]
fn a_clip_on_a_locked_track_cannot_move_to_another_track() {
    let mut s = demo();
    let (v_track, v, _, _) = pair_in_an_empty_sequence(&mut s, 0.0);
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    let before = s.project.clone();
    let e = s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "V2", "time": v.start.0}], "linked": false})).unwrap_err();
    assert!(matches!(&e, EngineError::Edit(filmcraft_edit::EditError::Locked)), "{e:?}");
    assert_eq!(*s.project, *before);
    let q = s.active_sequence().unwrap();
    assert_eq!(q.find_item(v.id).map(|(t, _)| t), Some(v_track));
    assert_eq!(q.video_tracks[1].items.len(), 0);
    q.check().unwrap();
}

/// A partner that would land where it already is does not move: re-placing it would drop the
/// transitions at its edges and list it in `moved` for nothing. Moving only the picture to
/// another track at the same time leaves the sound as it was.
#[test]
fn a_partner_whose_position_does_not_change_is_left_alone() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let a = q.audio_tracks[0].items[1].clone();
    let (v_track, v) = q.video_tracks.iter().find_map(|t| t.items.iter().find(|i| i.link == a.link && a.link.is_some()).map(|i| (t.id, i.clone()))).unwrap();
    let (a_track, v2) = (q.audio_tracks[0].id, q.video_tracks[1].id);
    assert_ne!(v_track, v2);
    s.execute("sequence.applyAudioTransition", json!({"clip": a.id.0})).unwrap();
    let sound = |s: &Session| s.active_sequence().unwrap().track(a_track).unwrap().clone();
    let before = sound(&s);
    assert_eq!(before.transitions.len(), 1, "the sound clip has a crossfade");
    // picture to V2 at the same time, Linked Selection on
    let r = s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": v2.0, "time": v.start.0}]})).unwrap();
    assert_eq!(state(&s, v.id), (v2, v.start));
    assert_eq!(sound(&s), before, "the sound track, its clip and its crossfade are untouched");
    assert_eq!(r["moved"], json!([v.id.0]), "the sound did not move, so it is not listed");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!((state(&s, v.id), sound(&s)), ((v_track, v.start), before.clone()));
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!((state(&s, v.id), sound(&s)), ((v2, v.start), before.clone()));
    s.execute("edit.undo", json!({})).unwrap();
    // a real offset still takes the sound along
    let later = v.start + s.sequence_rate().tick_of(2);
    let r = s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": v2.0, "time": later.0}]})).unwrap();
    assert_eq!(r["moved"], json!([v.id.0, a.id.0]));
    assert_eq!(state(&s, a.id).1 - a.start, later - v.start);
}

/// When a clip would start before the sequence start, every clip of the call lands later by the
/// same amount: a split edit keeps its offset and several listed clips keep their spacing.
#[test]
fn the_group_stops_together_at_the_sequence_start() {
    let mut s = demo();
    let (v_track, v, a_track, a) = pair_in_an_empty_sequence(&mut s, 60.0);
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    let (lead, gap) = (f(10), f(100));
    // make a split edit: the sound starts 10 frames before the picture
    s.execute("timeline.move", json!({"moves": [{"clip": a.id.0, "track": a_track.0, "time": (a.start - lead).0}], "linked": false})).unwrap();
    assert_eq!(state(&s, a.id).1, v.start - lead);
    // ask for the picture at 4 frames: the sound would start 6 frames before zero
    let r = s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": v_track.0, "time": f(4).0}]})).unwrap();
    assert_eq!(r["moved"], json!([v.id.0, a.id.0]));
    assert_eq!((state(&s, v.id).1, state(&s, a.id).1), (lead, Tick::ZERO), "the sound stops at zero and the picture keeps its 10 frames");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!((state(&s, v.id).1, state(&s, a.id).1), (v.start, v.start - lead));
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!((state(&s, v.id).1, state(&s, a.id).1), (lead, Tick::ZERO));
    s.execute("edit.undo", json!({})).unwrap();
    // a second, unlinked clip 100 frames after the picture, on V2; both listed, both asked 20 frames too far left
    let item = s.active_sequence().unwrap().video_tracks[0].items[0].item;
    let v2 = s.active_sequence().unwrap().video_tracks[1].id;
    s.execute("timeline.place", json!({"item": item.0, "time": (v.start + gap).0, "track": v2.0, "audioTrack": "A2"})).unwrap();
    let w = s.active_sequence().unwrap().video_tracks[1].items[0].clone();
    let back = v.start + f(20);
    let moves = json!([{"clip": v.id.0, "track": v_track.0, "time": (v.start - back).0}, {"clip": w.id.0, "track": v2.0, "time": (w.start - back).0}]);
    s.execute("timeline.move", json!({"moves": moves, "linked": false})).unwrap();
    assert_eq!((state(&s, v.id).1, state(&s, w.id).1), (Tick::ZERO, gap), "the two clips are still 100 frames apart");
    s.execute("edit.undo", json!({})).unwrap();
    // with the links followed, the earliest clip of all (the split sound) is the one that stops at zero
    s.execute("timeline.move", json!({"moves": moves})).unwrap();
    assert_eq!((state(&s, a.id).1, state(&s, v.id).1, state(&s, w.id).1), (Tick::ZERO, lead, lead + gap));
    s.active_sequence().unwrap().check().unwrap();
}

/// AGENTS.md §0.8: `time` is any i64. Out-of-range values are parameter errors, never a panic
/// (which `execute` would report as an internal error) and never a wrapped position.
#[test]
fn hostile_times_are_refused_or_clamped_never_a_panic() {
    let mut s = demo();
    let (v_track, v, a_track, a) = pair_in_an_empty_sequence(&mut s, 60.0);
    let before = s.project.clone();
    for linked in [true, false] {
        for time in [i64::MAX, i64::MAX - 1, Tick::MAX.0 + 1] {
            let e = s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": v_track.0, "time": time}], "linked": linked})).unwrap_err();
            assert!(matches!(&e, EngineError::BadParams { cmd, .. } if cmd == "timeline.move"), "{time} linked={linked}: {e:?}");
            assert_eq!(*s.project, *before, "{time}: nothing moved");
        }
        // far before the start: the clip lands at zero, as a small negative time does
        for time in [i64::MIN, i64::MIN + 1, -1] {
            s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": v_track.0, "time": time}], "linked": linked})).unwrap();
            assert_eq!(state(&s, v.id).1, Tick::ZERO, "{time}");
            assert_eq!(state(&s, a.id).1, if linked { Tick::ZERO } else { a.start }, "{time}");
            s.execute("edit.undo", json!({})).unwrap();
        }
    }
    // the sound asked far to the left and the picture far to the right in one call: out of range together
    let moves = json!([{"clip": v.id.0, "track": v_track.0, "time": Tick::MAX.0}, {"clip": a.id.0, "track": a_track.0, "time": i64::MIN}]);
    assert!(matches!(s.execute("timeline.move", json!({"moves": moves})), Err(EngineError::BadParams { .. })));
    // junk entries are skipped as before; a call with nothing usable is a parameter error
    for moves in
        [json!([]), json!([{"clip": v.id.0, "track": v_track.0, "time": "soon"}]), json!([{"clip": v.id.0, "track": v_track.0, "time": 1e300}]), json!("all")]
    {
        assert!(matches!(s.execute("timeline.move", json!({"moves": moves})), Err(EngineError::BadParams { .. })), "{moves}");
    }
    assert!(s.execute("timeline.move", json!({"moves": [{"clip": 987_654_321u64, "track": v_track.0, "time": 0}]})).is_err(), "a clip that does not exist");
    assert_eq!(*s.project, *before);
}

/// Moving one narration line onto a spot another line still held shortened that line and said
/// nothing; the result now names every clip the move changed without being asked to.
#[test]
fn a_move_that_lands_on_another_clip_reports_what_it_cut() {
    let mut s = demo();
    let (_, _, a_track, a1) = pair_in_an_empty_sequence(&mut s, 0.0);
    let later = a1.end().0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64 + 2.0;
    let item = a1.item.0;
    s.execute("timeline.place", json!({"item": item, "seconds": later})).unwrap();
    let a2 = s.active_sequence().unwrap().track(a_track).unwrap().items[1].clone();
    let line = |c: ClipId, t: Tick| json!({"moves": [{"clip": c.0, "track": a_track.0, "time": t.0}], "linked": false});
    // onto empty space: nothing else changes, nothing is reported
    let r = s.execute("timeline.move", line(a1.id, a2.end() + Tick(1000))).unwrap();
    assert_eq!(r["overwritten"], json!([]));
    s.execute("edit.undo", json!({})).unwrap();
    // over the head of the next line: that line is shortened (living on under a new id), and the result says so
    let half = Tick(a1.duration.0 / 2);
    let r = s.execute("timeline.move", line(a1.id, a2.start - half)).unwrap();
    let rest = s.active_sequence().unwrap().track(a_track).unwrap().items.last().unwrap().clone();
    assert_eq!((rest.end(), rest.duration), (a2.end(), a2.duration - half), "the overwrite itself is unchanged");
    assert_eq!(r["overwritten"], json!([{"clip": a2.id.0, "was": a2.duration.0, "now": rest.duration.0, "pieces": [rest.id.0]}]));
    s.execute("edit.undo", json!({})).unwrap();
    // over its tail: shortened in place, same id
    let r = s.execute("timeline.move", line(a2.id, a1.end() - half)).unwrap();
    assert_eq!(r["overwritten"], json!([{"clip": a1.id.0, "was": a1.duration.0, "now": (a1.duration - half).0, "pieces": [a1.id.0]}]));
    s.execute("edit.undo", json!({})).unwrap();
    // squarely onto it: the line under it is gone, and the result names it
    let r = s.execute("timeline.move", line(a1.id, a2.start)).unwrap();
    assert!(s.active_sequence().unwrap().find_item(a2.id).is_none());
    assert_eq!(r["overwritten"], json!([{"clip": a2.id.0, "was": a2.duration.0, "now": 0, "pieces": []}]));
    s.execute("edit.undo", json!({})).unwrap();
    // inside a longer line: it is split in two around the moved one
    s.execute("timeline.trim", json!({"clip": a1.id.0, "edge": "out", "delta": -(a1.duration.0 / 2)})).unwrap();
    let short = s.active_sequence().unwrap().find_item(a1.id).unwrap().1.duration;
    let r = s.execute("timeline.move", line(a1.id, a2.start + Tick(a2.duration.0 / 4))).unwrap();
    let o = &r["overwritten"][0];
    assert_eq!(r["overwritten"].as_array().unwrap().len(), 1, "{r}");
    assert_eq!((o["clip"].as_u64(), o["now"].as_i64()), (Some(a2.id.0), Some((a2.duration - short).0)), "{r}");
    assert_eq!(o["pieces"].as_array().unwrap().len(), 2, "{r}");
    // an insert-mode move shifts clips along and covers nothing
    s.execute("edit.undo", json!({})).unwrap();
    let r = s.execute("timeline.move", json!({"moves": [{"clip": a1.id.0, "track": a_track.0, "time": a2.start.0}], "insert": true, "linked": false})).unwrap();
    assert_eq!(r["overwritten"], json!([]));
}

/// Option-drag (`copy: true`) moved the clips instead of copying them.
#[test]
fn copy_leaves_the_originals_and_links_the_copies() {
    let mut s = demo();
    let (v_track, v, a_track, a) = pair_in_an_empty_sequence(&mut s, 0.0);
    let to = v.end() + s.sequence_rate().tick_of(24);
    let before = s.project.clone();
    // the picture is listed; its linked sound is copied with it
    let r = s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "V2", "time": to.0}], "copy": true})).unwrap();
    let copies: Vec<ClipId> = r["copied"].as_array().unwrap().iter().map(|c| ClipId(c.as_u64().unwrap())).collect();
    assert_eq!(copies.len(), 2, "picture and sound are copied: {r}");
    assert_eq!((state(&s, v.id), state(&s, a.id)), ((v_track, v.start), (a_track, a.start)), "the originals stay");
    let q = s.active_sequence().unwrap();
    let (vt, vc) = q.find_item(copies[0]).unwrap();
    let (at, ac) = q.find_item(copies[1]).unwrap();
    assert_eq!((vt, vc.start, vc.item, vc.source_in, vc.duration), (q.video_tracks[1].id, to, v.item, v.source_in, v.duration));
    assert_eq!((at, ac.start), (a_track, to), "the sound copy keeps its offset on its own track");
    assert!(vc.link.is_some() && vc.link == ac.link && vc.link != v.link, "the copies are linked to each other only");
    assert_eq!(s.state.selection, copies, "the copies are selected");
    q.check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before, "one undo step");
    // copying just one side (linked: false) leaves the copy unlinked
    let r = s.execute("timeline.move", json!({"moves": [{"clip": a.id.0, "track": a_track.0, "time": to.0}], "copy": true, "linked": false})).unwrap();
    let c = ClipId(r["copied"][0].as_u64().unwrap());
    assert_eq!(r["copied"].as_array().unwrap().len(), 1);
    assert!(s.active_sequence().unwrap().find_item(c).unwrap().1.link.is_none());
    // a copy onto a locked track is refused and changes nothing
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("timeline.setTrack", json!({"track": "V2", "locked": true})).unwrap();
    let r = s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "V2", "time": to.0}], "copy": true, "linked": false}));
    assert!(r.is_err());
    assert_eq!(s.active_sequence().unwrap().all_tracks().map(|t| t.items.len()).sum::<usize>(), 2);
}

#[test]
fn new_track_moves_and_copies_are_one_atomic_edit() {
    for copy in [false, true] {
        let mut s = demo();
        let (vt, v, at, a) = last_pair(&s);
        let before = s.project.clone();
        let count = s.history.undo.len();
        let q = s.active_sequence().unwrap();
        let counts = (q.video_tracks.len(), q.audio_tracks.len());
        let r = s
            .execute(
                "timeline.move",
                json!({"moves":[{"clip":v.id.0,"track":"new","time":v.start.0},{"clip":a.id.0,"track":"new","time":a.start.0}],"linked":false,"copy":copy}),
            )
            .unwrap();
        let q = s.active_sequence().unwrap();
        assert_eq!((q.video_tracks.len(), q.audio_tracks.len()), (counts.0 + 1, counts.1 + 1));
        let new_v = q.video_tracks.last().unwrap();
        let new_a = q.audio_tracks.last().unwrap();
        assert_eq!(new_a.channels, before.sequence(s.state.active_sequence.unwrap()).unwrap().track(at).unwrap().channels);
        assert_eq!((new_v.items[0].item, new_a.items[0].item), (v.item, a.item));
        assert_eq!((new_v.items[0].start, new_a.items[0].start), (v.start, a.start));
        if copy {
            assert_eq!(state(&s, v.id), (vt, v.start));
            assert_eq!(state(&s, a.id), (at, a.start));
            assert_ne!(new_v.items[0].id, v.id);
            assert!(new_v.items[0].link.is_some() && new_v.items[0].link == new_a.items[0].link);
            assert_ne!(new_v.items[0].link, v.link);
            assert_eq!(s.state.selection.len(), 2);
            assert_eq!(r["copied"].as_array().unwrap().len(), 2);
        } else {
            assert_eq!(state(&s, v.id), (new_v.id, v.start));
            assert_eq!(state(&s, a.id), (new_a.id, a.start));
        }
        q.check().unwrap();
        assert_eq!(s.history.undo.len(), count + 1);
        let after = s.project.clone();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(*s.project, *before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(*s.project, *after);
    }
}

#[test]
fn new_track_group_keeps_clips_from_the_same_source_together() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let clips = q.video_tracks[0].items[..2].to_vec();
    let tracks = q.video_tracks.len();
    let before = s.project.clone();
    let moves: Vec<_> = clips.iter().map(|c| json!({"clip":c.id.0,"track":"new","time":c.start.0})).collect();
    s.execute("timeline.move", json!({"moves":moves,"linked":false})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.video_tracks.len(), tracks + 1);
    let t = q.video_tracks.last().unwrap();
    assert_eq!(t.items, clips);
    q.check().unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
}

#[test]
fn refused_new_track_moves_preserve_document_and_both_history_stacks() {
    let mut s = demo();
    let (_, v, _, _) = last_pair(&s);
    s.execute("timeline.setTrack", json!({"track":"V1","locked":true})).unwrap();
    for moves in [
        json!([{"clip":v.id.0,"track":"new","time":v.start.0}]),
        json!([{"clip":u64::MAX,"track":"new","time":0}]),
        json!([{"clip":v.id.0,"track":"new","time":i64::MAX}]),
        json!([{"clip":v.id.0,"track":"new","time":0},{"clip":u64::MAX,"track":"V2","time":0}]),
    ] {
        let before = s.project.clone();
        let history = s.execute("history.list", json!({})).unwrap();
        assert!(s.execute("timeline.move", json!({"moves":moves,"linked":false})).is_err());
        assert_eq!(*s.project, *before);
        assert_eq!(s.execute("history.list", json!({})).unwrap(), history);
    }
    s.execute("edit.undo", json!({})).unwrap();
    let before = s.project.clone();
    let history = s.execute("history.list", json!({})).unwrap();
    assert!(!history["redo"].as_array().unwrap().is_empty());
    for moves in [
        json!([{"clip":v.id.0,"track":"new","time":0},{"clip":u64::MAX,"track":"V2","time":0}]),
        json!([{"clip":v.id.0,"track":"new","time":i64::MAX}]),
        json!([{"clip":v.id.0,"track":"new"}]),
        json!([{"clip":v.id.0,"track":"new","time":"invalid"}]),
    ] {
        assert!(s.execute("timeline.move", json!({"moves":moves,"linked":false})).is_err());
        assert_eq!(*s.project, *before);
        assert_eq!(s.execute("history.list", json!({})).unwrap(), history);
    }
}

#[test]
fn new_track_moves_respect_the_track_limit() {
    let mut s = demo();
    let (_, v, _, _) = last_pair(&s);
    s.edit_sequence("Limit fixture", |q, ctx, _| {
        while q.video_tracks.len() < crate::sequence_tools::MAX_TRACKS {
            q.video_tracks.push(filmcraft_project::Track::new(TrackId(ctx.alloc()), filmcraft_project::TrackKind::Video, "Limit".into()));
        }
        Ok(())
    })
    .unwrap();
    let before = s.project.clone();
    let history = s.execute("history.list", json!({})).unwrap();
    assert!(s.execute("timeline.move", json!({"moves":[{"clip":v.id.0,"track":"new","time":v.start.0}],"linked":false})).is_err());
    assert_eq!(*s.project, *before);
    assert_eq!(s.execute("history.list", json!({})).unwrap(), history);
}

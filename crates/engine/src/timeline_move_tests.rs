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

fn assert_selection_edit_matches_control(s: &mut Session, selection: Vec<ClipId>, command: &str, params: serde_json::Value) {
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let mut control = Session { project: s.project.clone(), state: s.state.clone(), history: s.history.clone(), media: s.media.clone(), ..Session::default() };
    control.state.selection = selection;
    let expected = control.execute(command, params.clone()).unwrap();
    let actual = s.execute(command, params).unwrap();
    assert_eq!(actual, expected, "{command} returns the unique-selection result");
    assert_eq!(s.project, control.project, "{command} produces the complete unique-selection project");
    assert_eq!(s.history.undo.len(), undo + 1, "the edit is one undo step");
    assert_eq!(s.history.undo, control.history.undo);
    assert_eq!(s.history.redo, control.history.redo);
    assert_eq!(s.history.merge_key, control.history.merge_key);
    let q = s.active_sequence().unwrap();
    let ids: Vec<ClipId> = q.all_tracks().flat_map(|t| t.items.iter().map(|i| i.id)).collect();
    assert_eq!(ids.iter().copied().collect::<std::collections::HashSet<_>>().len(), ids.len(), "no identity was duplicated");
    q.check().unwrap();
    let after = s.project.clone();
    s.execute("edit.undo", json!({})).unwrap();
    control.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project, before, "undo restores the entire pre-edit project");
    assert_eq!(s.project, control.project);
    assert_eq!(s.history.undo, control.history.undo);
    assert_eq!(s.history.redo, control.history.redo);
    s.active_sequence().unwrap().check().unwrap();
    s.execute("edit.redo", json!({})).unwrap();
    control.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.project, after, "redo restores the entire edited project");
    assert_eq!(s.project, control.project);
    assert_eq!(s.history.undo, control.history.undo);
    assert_eq!(s.history.redo, control.history.redo);
    s.active_sequence().unwrap().check().unwrap();
}

#[test]
fn duplicate_selection_inputs_keep_first_occurrence_and_toggle_once() {
    for linked in [false, true] {
        let mut s = demo();
        let (_, v, _, a) = pair_in_an_empty_sequence(&mut s, 0.0);
        s.execute("sequence.linkedSelection", json!({"on": linked})).unwrap();
        let before = s.project.clone();
        let history = s.history.clone();
        let ids = json!([a.id.0, a.id.0, v.id.0, a.id.0, v.id.0]);
        let result = s.execute("timeline.select", json!({"clips": ids})).unwrap();
        assert_eq!(s.state.selection, vec![a.id, v.id], "explicit order precedes linked expansion");
        assert_eq!(result["selection"], json!([a.id.0, v.id.0]));
        s.execute("timeline.select", json!({"clips": [v.id.0, v.id.0], "add": true})).unwrap();
        assert_eq!(s.state.selection, vec![a.id, v.id], "adding an identity twice does not reorder the selection");
        s.execute("timeline.select", json!({"clips": ids, "toggle": true})).unwrap();
        assert!(s.state.selection.is_empty(), "each selected identity toggles off once");
        s.execute("timeline.select", json!({"clips": ids, "toggle": true})).unwrap();
        assert_eq!(s.state.selection, vec![a.id, v.id], "each unselected identity toggles on once");
        s.execute("timeline.select", json!({"clips": []})).unwrap();
        s.execute("timeline.select", json!({"clips": [v.id.0, v.id.0], "add": true})).unwrap();
        assert_eq!(s.state.selection, if linked { vec![v.id, a.id] } else { vec![v.id] });
        assert_eq!(s.project, before, "selection admission does not edit the project");
        assert_eq!(s.history.undo, history.undo);
        assert_eq!(s.history.redo, history.redo);
        s.active_sequence().unwrap().check().unwrap();
    }
}

#[test]
fn duplicate_selection_expansion_is_unique_without_an_active_sequence() {
    let clips = [ClipId(9), ClipId(3), ClipId(9), ClipId(3), ClipId(5)];
    for linked in [false, true] {
        let mut s = Session::default();
        s.state.linked_selection = linked;
        for active in [None, Some(filmcraft_project::ItemId(u64::MAX))] {
            s.state.active_sequence = active;
            let before = serde_json::to_value(&s.state).unwrap();
            assert_eq!(crate::commands::with_links(&s, &clips), vec![ClipId(9), ClipId(3), ClipId(5)]);
            assert_eq!(serde_json::to_value(&s.state).unwrap(), before);
        }
    }
}

#[test]
fn duplicate_selection_public_admission_keeps_nudges_equivalent_to_unique_selection() {
    for (command, linked) in [("timeline.nudgeRight", false), ("timeline.nudgeRight", true), ("timeline.nudgeUp", false)] {
        let mut s = demo();
        let (_, v, _, a) = pair_in_an_empty_sequence(&mut s, 60.0);
        s.execute("sequence.linkedSelection", json!({"on": linked})).unwrap();
        s.execute("timeline.select", json!({"clips": [v.id.0, v.id.0]})).unwrap();
        let selection = if linked { vec![v.id, a.id] } else { vec![v.id] };
        assert_eq!(s.state.selection, selection);
        assert_selection_edit_matches_control(&mut s, selection.clone(), command, json!({}));
        assert_eq!(s.state.selection, selection);
    }
}

#[test]
fn duplicate_selection_already_in_session_keeps_nudges_equivalent_to_unique_selection() {
    for linked in [false, true] {
        for command in ["timeline.nudgeRight", "timeline.nudgeUp"] {
            let mut s = demo();
            let (_, v, a_track, a) = pair_in_an_empty_sequence(&mut s, 60.0);
            s.execute("sequence.linkedSelection", json!({"on": linked})).unwrap();
            s.state.selection = vec![v.id, v.id, v.id];
            assert_selection_edit_matches_control(&mut s, vec![v.id], command, json!({}));
            let q = s.active_sequence().unwrap();
            if command == "timeline.nudgeUp" {
                assert_eq!(state(&s, v.id), (q.video_tracks[1].id, v.start));
                assert_eq!(state(&s, a.id), (a_track, a.start), "vertical nudges remain selected-only even with linked selection on");
            } else {
                assert_eq!(state(&s, v.id).1, v.start + s.sequence_rate().tick_of(1));
                assert_eq!(state(&s, a.id).1, a.start + if linked { s.sequence_rate().tick_of(1) } else { Tick::ZERO });
            }
        }
    }
}

#[test]
fn duplicate_selection_delete_matches_unique_selection_with_and_without_links() {
    for linked in [false, true] {
        for command in ["edit.clear", "edit.rippleDelete"] {
            let mut s = demo();
            let (_, v, a_track, a) = pair_in_an_empty_sequence(&mut s, 60.0);
            s.execute("sequence.linkedSelection", json!({"on": linked})).unwrap();
            if !linked {
                s.execute("timeline.setTrack", json!({"track": a_track.0, "syncLock": false})).unwrap();
            }
            // Clear and Ripple Delete act on the selection as it is: selecting the clip takes its
            // partner along only with Linked Selection on
            s.execute("timeline.select", json!({"clips": [v.id.0]})).unwrap();
            let unique = s.state.selection.clone();
            s.state.selection = unique.iter().chain(&unique).copied().collect();
            assert_selection_edit_matches_control(&mut s, unique, command, json!({}));
            assert!(s.active_sequence().unwrap().find_item(v.id).is_none());
            assert_eq!(s.active_sequence().unwrap().find_item(a.id).is_none(), linked);
            assert!(s.state.selection.is_empty());
        }
    }
}

#[test]
fn duplicate_move_targets_are_rejected_and_retry_is_undoable() {
    for insert in [false, true] {
        let mut s = demo();
        let (_, v, _, _) = pair_in_an_empty_sequence(&mut s, 0.0);
        let before = s.project.clone();
        let undo = s.history.undo.len();
        let at = s.sequence_rate().tick_of(48).0;
        let result = s.execute(
            "timeline.move",
            json!({"moves": [{"clip": v.id.0, "track": "V1", "time": at}, {"clip": v.id.0, "track": "V2", "time": at}], "insert": insert, "linked": false}),
        );
        assert!(result.is_err(), "duplicate move entries were accepted: {result:?}");
        assert_eq!(s.project, before);
        assert_eq!(s.history.undo.len(), undo);
        s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "V2", "time": at}], "insert": insert, "linked": false})).unwrap();
        let after = s.project.clone();
        assert_eq!(state(&s, v.id).1.0, at);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(s.project, before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(s.project, after);
    }
}

fn reject_duplicate_new_track_move(s: &mut Session, clip: ClipId, insert: bool, linked: bool, new_first: bool) {
    s.state.selection = vec![clip];
    let before = s.project.clone();
    let selection = s.state.selection.clone();
    let editor_state = serde_json::to_value(&s.state).unwrap();
    let history = s.history.clone();
    let journal = s.journal.clone();
    let at = s.sequence_rate().tick_of(48).0;
    let tracks = if new_first { ["new", "V2"] } else { ["V2", "new"] };
    let result = s.execute(
        "timeline.move",
        json!({"moves": [{"clip": clip.0, "track": tracks[0], "time": at}, {"clip": clip.0, "track": tracks[1], "time": at}], "insert": insert, "linked": linked}),
    );
    assert!(
        matches!(result, Err(EngineError::Edit(filmcraft_edit::EditError::Other(_)) | EngineError::BadParams { .. })),
        "duplicate move entries must be rejected: {result:?}"
    );
    assert_eq!(s.project, before, "the project and its id allocator stay unchanged");
    assert_eq!(s.state.selection, selection);
    assert_eq!(serde_json::to_value(&s.state).unwrap(), editor_state);
    assert_eq!(s.history.undo.len(), history.undo.len(), "no oldest undo entry is evicted");
    assert_eq!(s.history.redo.len(), history.redo.len(), "no existing redo entry is lost");
    assert_eq!(s.history.undo, history.undo);
    assert_eq!(s.history.redo, history.redo);
    assert_eq!(s.history.merge_key, history.merge_key);
    assert_eq!(s.history.limit, history.limit);
    assert_eq!(s.journal, journal, "the failed command is not journaled");
}

#[test]
fn duplicate_new_track_move_preserves_redo_and_retry_is_undoable() {
    for insert in [false, true] {
        for linked in [false, true] {
            for new_first in [false, true] {
                let mut s = demo();
                let (_, v, _, _) = pair_in_an_empty_sequence(&mut s, 0.0);
                s.execute("timeline.setTrack", json!({"track": "V2", "locked": true})).unwrap();
                let redo_project = s.project.clone();
                s.execute("edit.undo", json!({})).unwrap();
                assert!(!s.history.redo.is_empty());
                reject_duplicate_new_track_move(&mut s, v.id, insert, linked, new_first);
                let before = s.project.clone();
                s.execute("edit.redo", json!({})).unwrap();
                assert_eq!(s.project, redo_project, "the previously available redo still works");
                s.execute("edit.undo", json!({})).unwrap();
                assert_eq!(s.project, before);
                let at = s.sequence_rate().tick_of(48).0;
                s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "new", "time": at}], "insert": insert, "linked": linked})).unwrap();
                let after = s.project.clone();
                assert_eq!(state(&s, v.id).1.0, at);
                assert_eq!(s.active_sequence().unwrap().video_tracks.len(), 3);
                s.active_sequence().unwrap().check().unwrap();
                s.execute("edit.undo", json!({})).unwrap();
                assert_eq!(s.project, before, "one undo removes the new track and restores the move");
                s.execute("edit.redo", json!({})).unwrap();
                assert_eq!(s.project, after);
            }
        }
    }
}

#[test]
fn duplicate_new_track_move_preserves_full_undo_history() {
    for insert in [false, true] {
        for linked in [false, true] {
            for new_first in [false, true] {
                let mut s = demo();
                let (_, v, _, _) = pair_in_an_empty_sequence(&mut s, 0.0);
                assert_eq!(s.history.limit, 200);
                while s.history.undo.len() < s.history.limit {
                    let step = s.history.undo.len();
                    s.edit_merged("History step", &format!("history-{step}"), |project, _| {
                        project.root.name = format!("History {step}");
                        Ok(())
                    })
                    .unwrap();
                }
                assert_eq!(s.history.undo.len(), s.history.limit);
                assert!(s.history.merge_key.is_some());
                reject_duplicate_new_track_move(&mut s, v.id, insert, linked, new_first);
                let oldest = s.history.undo.first().unwrap().1.clone();
                for _ in 0..s.history.limit {
                    s.execute("edit.undo", json!({})).unwrap();
                }
                assert_eq!(s.project, oldest, "every retained undo step remains usable");
            }
        }
    }
}

#[test]
fn unique_new_track_moves_keep_linked_partner_behavior() {
    for insert in [false, true] {
        for linked in [false, true] {
            let mut s = demo();
            let (_, v, a_track, a) = pair_in_an_empty_sequence(&mut s, 0.0);
            let before = s.project.clone();
            let undo = s.history.undo.len();
            let to = s.sequence_rate().tick_of(48);
            let result =
                s.execute("timeline.move", json!({"moves": [{"clip": v.id.0, "track": "new", "time": to.0}], "insert": insert, "linked": linked})).unwrap();
            let sequence = s.active_sequence().unwrap();
            assert_eq!(sequence.video_tracks.len(), 3);
            assert_eq!(state(&s, v.id), (sequence.video_tracks[2].id, to));
            assert_eq!(state(&s, a.id), (a_track, if linked { to } else { a.start }));
            assert_eq!(result["moved"], if linked { json!([v.id.0, a.id.0]) } else { json!([v.id.0]) });
            assert_eq!(s.history.undo.len(), undo + 1);
            sequence.check().unwrap();
            let after = s.project.clone();
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(s.project, before);
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(s.project, after);
        }
    }
}

#[test]
fn repeated_new_track_copy_destinations_get_distinct_ids() {
    for insert in [false, true] {
        for linked in [false, true] {
            for new_first in [false, true] {
                let mut s = demo();
                let (v_track, v, a_track, a) = pair_in_an_empty_sequence(&mut s, 0.0);
                let before = s.project.clone();
                let undo = s.history.undo.len();
                let to = s.active_sequence().unwrap().duration() + s.sequence_rate().tick_of(48);
                let tracks = if new_first { ["new", "V2"] } else { ["V2", "new"] };
                let result = s
                .execute(
                    "timeline.move",
                    json!({"moves": [{"clip": v.id.0, "track": tracks[0], "time": to.0}, {"clip": v.id.0, "track": tracks[1], "time": to.0}], "insert": insert, "linked": linked, "copy": true}),
                )
                .unwrap();
                let copies: Vec<ClipId> = result["copied"].as_array().unwrap().iter().map(|c| ClipId(c.as_u64().unwrap())).collect();
                assert_eq!(copies.len(), if linked { 3 } else { 2 });
                assert_eq!(copies.iter().copied().collect::<std::collections::HashSet<_>>().len(), copies.len());
                assert!(!copies.contains(&v.id) && !copies.contains(&a.id));
                assert_eq!((state(&s, v.id), state(&s, a.id)), ((v_track, v.start), (a_track, a.start)));
                assert_eq!(s.state.selection, copies);
                let sequence = s.active_sequence().unwrap();
                assert_eq!(sequence.video_tracks.len(), 3);
                assert!(copies.iter().all(|c| sequence.find_item(*c).is_some()), "every reported copy exists");
                assert!(copies.iter().any(|c| state(&s, *c) == (sequence.video_tracks[2].id, to)));
                assert!(copies.iter().any(|c| state(&s, *c) == (sequence.video_tracks[1].id, to)));
                assert_eq!(s.history.undo.len(), undo + 1);
                sequence.check().unwrap();
                let after = s.project.clone();
                s.execute("edit.undo", json!({})).unwrap();
                assert_eq!(s.project, before);
                s.execute("edit.redo", json!({})).unwrap();
                assert_eq!(s.project, after);
            }
        }
    }
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

/// #3450: Copy + Paste keeps the transitions that sit only on the copied clips (a fade at a
/// copied clip's edge), moved by the paste offset onto the pasted copies with new ids; the
/// originals keep theirs.
#[test]
fn copy_paste_keeps_clip_transitions() {
    let mut s = demo();
    let (v1, v, a1, a) = pair_in_an_empty_sequence(&mut s, 0.0);
    s.execute("effects.apply", json!({"effect": "cross_dissolve", "clip": v.id.0, "edge": "out", "frames": 10})).unwrap();
    s.execute("effects.apply", json!({"effect": "constant_power", "clip": a.id.0, "edge": "out", "frames": 10})).unwrap();
    let before = s.active_sequence().unwrap().clone();
    let originals: Vec<_> = [v1, a1].iter().map(|t| before.track(*t).unwrap().transitions.clone()).collect();
    assert!(originals.iter().all(|t| t.len() == 1));
    s.execute("timeline.select", json!({"clips": [v.id.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    let at = v.end() + before.settings.frame_rate.tick_of(48);
    s.set_playhead(at);
    s.execute("edit.paste", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    for (track, orig) in [v1, a1].into_iter().zip(&originals) {
        let t = q.track(track).unwrap();
        let pasted = t.items.iter().find(|i| i.start == at).unwrap();
        let orig = &orig[0];
        assert!(t.transitions.contains(orig), "the original keeps its transition");
        let copies: Vec<_> = t.transitions.iter().filter(|x| x.from == Some(pasted.id) || x.to == Some(pasted.id)).collect();
        assert_eq!(copies.len(), 1, "pasted clip keeps its crossfade: {:?}", t.transitions);
        let c = copies[0];
        assert_ne!(c.id, orig.id);
        assert_eq!((c.from, c.to), (Some(pasted.id), None));
        assert_eq!((&c.effect, c.duration, c.align, c.reverse), (&orig.effect, orig.duration, orig.align, orig.reverse));
        assert_eq!(c.start, orig.start + at);
    }
}

/// A transition between a copied clip and a neighbour left behind is not pasted.
#[test]
fn copy_paste_drops_transition_to_uncopied_neighbour() {
    let mut s = demo();
    let q = s.active_sequence().unwrap();
    let t = &q.video_tracks[0];
    let first = t.items.iter().enumerate().find(|(i, c)| t.items.get(i + 1).is_some_and(|n| n.start == c.end())).map(|(_, c)| c.clone()).unwrap();
    let end = q.duration();
    s.execute("effects.apply", json!({"effect": "cross_dissolve", "clip": first.id.0, "edge": "out", "frames": 10})).unwrap();
    let q = s.active_sequence().unwrap();
    assert!(q.video_tracks[0].transitions.iter().any(|x| x.from == Some(first.id) && x.to.is_some()));
    let count = q.video_tracks[0].transitions.len();
    s.execute("timeline.select", json!({"clips": [first.id.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    s.set_playhead(end);
    s.execute("edit.paste", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    q.check().unwrap();
    let t = &q.video_tracks[0];
    let pasted = t.items.iter().find(|i| i.start == end).unwrap();
    assert!(!t.transitions.iter().any(|x| x.from == Some(pasted.id)), "the cut to the neighbour left behind is not pasted: {:?}", t.transitions);
    assert!(t.transitions.len() >= count, "the original transitions stay");
}

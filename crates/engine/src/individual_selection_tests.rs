//! Individual linked-clip selection and deletion (#680, #1163).

use crate::Session;
use filmcraft_project::ClipId;
use filmcraft_time::Tick;
use serde_json::json;

fn paired_sequence() -> (Session, [(ClipId, ClipId); 2]) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Individual selection", "video": 1, "audio": 1})).unwrap();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).unwrap().id;
    for frame in [0, 48] {
        s.execute("timeline.place", json!({"item": item.0, "frame": frame, "duration": s.sequence_rate().tick_of(48).0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    let pairs = [0, 1].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id));
    (s, pairs)
}

#[test]
fn linked_false_is_local_to_selection_and_respects_add_toggle() {
    for audio in [false, true] {
        let (mut s, [(video, sound), _]) = paired_sequence();
        let (selected, partner) = if audio { (sound, video) } else { (video, sound) };
        s.execute("timeline.select", json!({"clips": [selected.0]})).unwrap();
        assert_eq!(s.state.selection.len(), 2, "ordinary selection includes the partner");
        s.execute("timeline.select", json!({"clips": [selected.0], "linked": false})).unwrap();
        assert_eq!(s.state.selection, vec![selected]);
        assert!(s.state.linked_selection, "Alt selection does not change the global toggle");
        s.execute("timeline.select", json!({"clips": [partner.0], "add": true, "linked": false})).unwrap();
        assert_eq!(s.state.selection, vec![selected, partner]);
        s.execute("timeline.select", json!({"clips": [selected.0], "toggle": true, "linked": false})).unwrap();
        assert_eq!(s.state.selection, vec![partner]);
        s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
        for params in [json!({"clips": [selected.0]}), json!({"clips": [selected.0], "linked": true})] {
            s.execute("timeline.select", params).unwrap();
            assert_eq!(s.state.selection, vec![selected], "the global off toggle still applies");
        }
        s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
        s.execute("timeline.select", json!({"clips": [selected.0]})).unwrap();
        assert_eq!(s.state.selection.len(), 2);
    }
}

#[test]
fn implicit_clear_keeps_an_individually_selected_partner_and_undo() {
    for audio in [false, true] {
        let (mut s, [(video, sound), _]) = paired_sequence();
        let (selected, partner) = if audio { (sound, video) } else { (video, sound) };
        // Alt-click and the marquee preview can already hold a precise transient selection.
        // Test deletion independently of the timeline.select parameter handling.
        s.state.selection = vec![selected];
        let before = s.project.clone();
        s.execute("edit.clear", json!({})).unwrap();
        let q = s.active_sequence().unwrap();
        assert!(q.find_item(selected).is_none());
        assert!(q.find_item(partner).is_some(), "the unselected linked partner stays");
        assert!(s.state.linked_selection);
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(*s.project, *before, "one undo restores the pair");
    }
}

#[test]
fn implicit_ripple_delete_keeps_the_partner_on_an_unsynced_track() {
    for audio in [false, true] {
        let (mut s, [(video, sound), (next_video, next_sound)]) = paired_sequence();
        let (selected, partner, next, partner_next, partner_track) =
            if audio { (sound, video, next_sound, next_video, "V1") } else { (video, sound, next_video, next_sound, "A1") };
        s.execute("timeline.setTrack", json!({"track": partner_track, "syncLock": false})).unwrap();
        s.state.selection = vec![selected];
        let before = s.project.clone();
        s.execute("edit.rippleDelete", json!({})).unwrap();
        let q = s.active_sequence().unwrap();
        q.check().unwrap();
        assert!(q.find_item(selected).is_none());
        assert_eq!(q.find_item(partner).unwrap().1.start, Tick::ZERO);
        assert_eq!(q.find_item(next).unwrap().1.start, Tick::ZERO, "the selected track closes its gap");
        assert_eq!(q.find_item(partner_next).unwrap().1.start, s.sequence_rate().tick_of(48), "the unsynced partner track stays put");
        assert!(s.state.linked_selection);
        let after = s.project.clone();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(*s.project, *before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(*s.project, *after);
    }
}

#[test]
fn individual_ripple_sync_lock_conflict_is_atomic() {
    for audio in [false, true] {
        let (mut s, [(video, sound), _]) = paired_sequence();
        let selected = if audio { sound } else { video };
        s.state.selection = vec![selected];
        let before = s.project.clone();
        let history = (s.history.undo.len(), s.history.redo.len());
        let error = s.execute("edit.rippleDelete", json!({})).unwrap_err();
        assert!(error.to_string().to_lowercase().contains("sync"), "{error}");
        assert_eq!(*s.project, *before, "a conflict must not delete either clip");
        assert_eq!(s.state.selection, vec![selected]);
        assert_eq!((s.history.undo.len(), s.history.redo.len()), history);
        assert!(s.state.linked_selection);
    }
}

#[test]
fn explicit_clear_and_ripple_targets_keep_the_linked_selection_setting() {
    for command in ["edit.clear", "edit.rippleDelete"] {
        for key in ["clip", "clips"] {
            for linked in [false, true] {
                let (mut s, [(video, sound), (next, _)]) = paired_sequence();
                for track in ["V1", "A1"] {
                    s.execute("timeline.setTrack", json!({"track": track, "syncLock": false})).unwrap();
                }
                s.execute("sequence.linkedSelection", json!({"on": linked})).unwrap();
                s.state.selection = vec![next];
                let params = if key == "clip" { json!({"clip": video.0}) } else { json!({"clips": [video.0]}) };
                s.execute(command, params).unwrap();
                let q = s.active_sequence().unwrap();
                assert!(q.find_item(video).is_none(), "{command} {key}");
                assert_eq!(q.find_item(sound).is_none(), linked, "{command} {key}: explicit targets retain link expansion");
                assert!(q.find_item(next).is_some(), "explicit targets do not delete the current selection");
            }
        }
    }
}

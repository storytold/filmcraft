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

/// Grouped clips select (and so drag) together; Alt-click still picks one member.
#[test]
fn grouped_clips_select_together() {
    let (mut s, [(v0, a0), (v1, a1)]) = paired_sequence();
    s.execute("timeline.select", json!({"clips": [v0.0, v1.0], "linked": false})).unwrap();
    s.execute("clip.group", json!({})).unwrap();
    s.execute("timeline.select", json!({"clips": [v0.0]})).unwrap();
    for c in [v0, a0, v1, a1] {
        assert!(s.state.selection.contains(&c), "group member {c:?} is selected");
    }
    // the linked partner of a grouped clip brings the group along
    s.execute("timeline.select", json!({"clips": [a0.0]})).unwrap();
    for c in [v0, a0, v1, a1] {
        assert!(s.state.selection.contains(&c), "{c:?} is selected from the partner");
    }
    s.execute("timeline.select", json!({"clips": [v0.0], "linked": false})).unwrap();
    assert_eq!(s.state.selection, vec![v0], "Alt-click selects one member");
    s.execute("timeline.select", json!({"clips": [v0.0, v1.0], "linked": false})).unwrap();
    s.execute("clip.ungroup", json!({})).unwrap();
    s.execute("timeline.select", json!({"clips": [v0.0]})).unwrap();
    assert!(!s.state.selection.contains(&v1), "ungrouped clips select alone");
}

/// Copies of a group (Option-drag, copy and paste) form a group of their own: selecting a copy
/// leaves the originals alone, so the next drag moves or copies only what was clicked.
#[test]
fn copies_of_a_group_do_not_join_the_original_group() {
    for how in ["drag", "paste"] {
        let (mut s, [(v0, a0), (v1, a1)]) = paired_sequence();
        let originals = [v0, a0, v1, a1];
        s.execute("timeline.select", json!({"clips": [v0.0, a0.0, v1.0, a1.0], "linked": false})).unwrap();
        s.execute("clip.group", json!({})).unwrap();
        s.execute("timeline.select", json!({"clips": [v0.0]})).unwrap();
        let end = s.active_sequence().unwrap().duration();
        if how == "drag" {
            let q = s.active_sequence().unwrap();
            let moves: Vec<_> = originals
                .iter()
                .map(|c| {
                    let (t, i) = q.find_item(*c).unwrap();
                    json!({"clip": c.0, "track": t.0, "time": (i.start + end).0})
                })
                .collect();
            s.execute("timeline.move", json!({"moves": moves, "copy": true, "linked": false})).unwrap();
        } else {
            s.execute("edit.copy", json!({})).unwrap();
            s.set_playhead(end);
            s.execute("edit.paste", json!({})).unwrap();
        }
        let q = s.active_sequence().unwrap();
        let copies: Vec<_> = q.all_tracks().flat_map(|t| t.items.iter()).filter(|i| !originals.contains(&i.id)).map(|i| i.id).collect();
        assert_eq!(copies.len(), 4, "{how}: the whole group was copied");
        let g = |c| q.find_item(c).unwrap().1.group;
        assert!(g(copies[0]).is_some() && copies.iter().all(|c| g(*c) == g(copies[0])), "{how}: the copies are one group");
        assert_ne!(g(copies[0]), g(v0), "{how}: not the originals' group");
        s.execute("timeline.select", json!({"clips": [copies[0].0]})).unwrap();
        assert_eq!(s.state.selection.len(), 4, "{how}: a copy selects its own group");
        assert!(originals.iter().all(|c| !s.state.selection.contains(c)), "{how}: the originals stay unselected");
        s.execute("timeline.select", json!({"clips": [v0.0]})).unwrap();
        assert!(copies.iter().all(|c| !s.state.selection.contains(c)), "{how}: an original does not select the copies");
    }
}

/// Toggling a partly selected group flips the whole group one way: the requested clip decides
/// add or remove once, instead of each member flipping its own state.
#[test]
fn toggling_a_partly_selected_group_flips_it_as_a_whole() {
    let (mut s, [(v0, a0), (v1, a1)]) = paired_sequence();
    let group = [v0, a0, v1, a1];
    s.execute("timeline.select", json!({"clips": [v0.0, v1.0], "linked": false})).unwrap();
    s.execute("clip.group", json!({})).unwrap();
    // only v1 selected: toggling the unselected v0 adds the whole group
    s.execute("timeline.select", json!({"clips": [v1.0], "linked": false})).unwrap();
    s.execute("timeline.select", json!({"clips": [v0.0], "toggle": true})).unwrap();
    assert!(group.iter().all(|c| s.state.selection.contains(c)), "the whole group is selected: {:?}", s.state.selection);
    assert_eq!(s.state.selection.len(), 4, "no member is listed twice");
    // only v0 selected: toggling the selected v0 removes the whole group
    s.execute("timeline.select", json!({"clips": [v0.0], "linked": false})).unwrap();
    s.execute("timeline.select", json!({"clips": [v0.0], "toggle": true})).unwrap();
    assert!(s.state.selection.is_empty(), "the whole group is deselected: {:?}", s.state.selection);
}

/// Cut, Copy and Nest take an Alt selection as it is, as Clear does (#2244): one half of a linked
/// pair goes, and its partner stays where it was.
#[test]
fn cut_copy_and_nest_keep_an_alt_selection_exact() {
    for command in ["edit.cut", "edit.copy", "clip.nest"] {
        for audio in [false, true] {
            let (mut s, [(video, sound), _]) = paired_sequence();
            let (selected, partner) = if audio { (sound, video) } else { (video, sound) };
            let partner_at = s.active_sequence().unwrap().find_item(partner).unwrap().1.range();
            s.execute("timeline.select", json!({"clips": [selected.0], "linked": false})).unwrap();
            let before = s.project.clone();
            let r = s.execute(command, json!({})).unwrap();
            let q = s.active_sequence().unwrap();
            q.check().unwrap();
            assert_eq!(q.find_item(partner).unwrap().1.range(), partner_at, "{command} {audio}: the partner stays");
            assert_eq!(q.find_item(selected).is_none(), command != "edit.copy", "{command} {audio}");
            if command == "clip.nest" {
                let nested = s.project.sequence(filmcraft_project::ItemId(r["sequence"].as_u64().unwrap())).unwrap();
                let inside: Vec<_> = nested.all_tracks().flat_map(|t| t.items.iter()).collect();
                assert_eq!(inside.len(), 1, "{audio}: only the selected half is nested");
                // neither half has a partner to follow any more
                assert_eq!(inside[0].link, None, "{audio}: the nested half is not linked");
                assert_eq!(q.find_item(partner).unwrap().1.link, None, "{audio}: the half left behind is not linked");
            } else {
                assert_eq!(s.state.clipboard.len(), 1, "{command} {audio}: only the selected half is copied");
                assert_eq!(s.state.clipboard[0].2.id, selected);
            }
            assert!(s.state.linked_selection);
            if command != "edit.copy" {
                s.execute("edit.undo", json!({})).unwrap();
                assert_eq!(*s.project, *before, "{command} {audio}: one undo restores the pair");
            }
        }
    }
}

/// An ordinary click selects both halves, so Cut, Copy and Nest still take the whole pair.
#[test]
fn cut_copy_and_nest_take_both_halves_of_a_linked_selection() {
    for command in ["edit.cut", "edit.copy", "clip.nest"] {
        let (mut s, [(video, sound), _]) = paired_sequence();
        s.execute("timeline.select", json!({"clips": [sound.0]})).unwrap();
        let r = s.execute(command, json!({})).unwrap();
        let q = s.active_sequence().unwrap();
        if command == "edit.copy" {
            assert_eq!(s.state.clipboard.len(), 2, "both halves are copied");
            continue;
        }
        assert!(q.find_item(video).is_none() && q.find_item(sound).is_none(), "{command}: both halves go");
        if command == "clip.nest" {
            let nested = s.project.sequence(filmcraft_project::ItemId(r["sequence"].as_u64().unwrap())).unwrap();
            let inside: Vec<_> = nested.all_tracks().flat_map(|t| t.items.iter()).collect();
            assert_eq!(inside.len(), 2, "both halves are nested");
            assert!(inside[0].link.is_some() && inside[0].link == inside[1].link, "the nested pair stays linked");
        } else {
            assert_eq!(s.state.clipboard.len(), 2, "both halves are cut");
        }
    }
}

/// Removing one half of a linked pair leaves the other half unlinked: it has nothing to follow,
/// and a later Link on it must link, not unlink. A partner on a locked track stays put and is
/// unlinked too.
#[test]
fn the_half_left_behind_is_no_longer_linked() {
    for command in ["edit.cut", "edit.clear", "edit.rippleDelete", "clip.nest"] {
        for locked in [false, true] {
            let (mut s, [(video, sound), (next_video, next_sound)]) = paired_sequence();
            for track in ["V1", "A1"] {
                s.execute("timeline.setTrack", json!({"track": track, "syncLock": false})).unwrap();
            }
            s.execute("timeline.select", json!({"clips": [video.0], "linked": false})).unwrap();
            if locked {
                s.execute("timeline.setTrack", json!({"track": "A1", "locked": true})).unwrap();
            }
            s.execute(command, json!({})).unwrap();
            let q = s.active_sequence().unwrap();
            q.check().unwrap();
            assert!(q.find_item(video).is_none(), "{command} {locked}");
            assert_eq!(q.find_item(sound).unwrap().1.link, None, "{command} {locked}: the sound left behind is not linked");
            let link = |c| q.find_item(c).unwrap().1.link;
            assert!(link(next_video).is_some() && link(next_video) == link(next_sound), "{command} {locked}: other pairs keep their link");
        }
    }
}

/// A link with more than two members keeps the members that stay: removing one Alt-selected sound
/// clip of a picture linked to two sound clips leaves the picture and the other sound linked.
#[test]
fn the_rest_of_a_larger_link_stays_linked() {
    for command in ["edit.cut", "edit.clear", "edit.rippleDelete"] {
        let (mut s, [(video, sound), (next_video, next_sound)]) = paired_sequence();
        for track in ["V1", "A1"] {
            s.execute("timeline.setTrack", json!({"track": track, "syncLock": false})).unwrap();
        }
        for pair in [[next_video, next_sound], [video, sound]] {
            s.execute("clip.link", json!({"clips": [pair[0].0, pair[1].0]})).unwrap();
        }
        s.execute("clip.link", json!({"clips": [video.0, sound.0, next_sound.0]})).unwrap();
        s.execute("timeline.select", json!({"clips": [sound.0], "linked": false})).unwrap();
        s.execute(command, json!({})).unwrap();
        let q = s.active_sequence().unwrap();
        q.check().unwrap();
        assert!(q.find_item(sound).is_none(), "{command}");
        let link = |c| q.find_item(c).unwrap().1.link;
        assert!(link(video).is_some() && link(video) == link(next_sound), "{command}: the picture and the other sound stay linked");
        assert_eq!(link(next_video), None, "{command}");
    }
}

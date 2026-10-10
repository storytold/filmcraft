//! A caller that names its targets in the parameters (`clips`, `clip`, `item`, `items`) does not
//! need a selection: enablement is checked against the named targets. Without such parameters
//! (the menus, shortcuts and `engine.commands`) enablement is the selection's, as before.

use filmcraft_project::{ClipId, ItemId, TrackItem};
use serde_json::json;

use crate::{EngineError, Session};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.state.selection.clear();
    s.state.project_selection.clear();
    s
}

fn item_named(s: &Session, name: &str) -> ItemId {
    s.project.items.values().find(|i| i.name == name).map(|i| i.id).unwrap_or_else(|| panic!("no item {name}"))
}

fn v1(s: &Session) -> Vec<TrackItem> {
    s.active_sequence().unwrap().video_tracks[0].items.clone()
}

fn a1(s: &Session) -> Vec<TrackItem> {
    s.active_sequence().unwrap().audio_tracks[0].items.clone()
}

fn clip(s: &Session, id: ClipId) -> Option<TrackItem> {
    s.active_sequence().unwrap().find_item(id).map(|(_, i)| i.clone())
}

fn disabled(r: crate::Result<serde_json::Value>) -> String {
    match r {
        Err(EngineError::Disabled(_, why)) => why,
        other => panic!("expected a disabled command, got {other:?}"),
    }
}

#[test]
fn replace_from_bin_takes_an_explicit_item_and_clips_without_any_selection() {
    let mut s = demo();
    let c = v1(&s)[0].clone();
    let dunes = item_named(&s, "Desert_Dunes.mp4");
    assert_ne!(c.item, dunes);
    // nothing named, nothing selected: disabled, for the menus and for a caller alike
    assert!(!s.is_enabled("clip.replaceFromBin"));
    assert_eq!(disabled(s.execute("clip.replaceFromBin", json!({}))), "no clips selected");
    // the clip is named but the replacement is not: still the Project panel's call
    assert_eq!(disabled(s.execute("clip.replaceFromBin", json!({"clips": [c.id.0]}))), "select a clip in the Project panel");
    // both named: runs with no timeline and no Project-panel selection
    s.execute("clip.replaceFromBin", json!({"clips": [c.id.0], "item": dunes.0})).unwrap();
    assert_eq!(clip(&s, c.id).unwrap().item, dunes);
    assert!(s.state.selection.is_empty() && s.state.project_selection.is_empty(), "the selections are left as they were");
    assert!(!s.is_enabled("clip.replaceFromBin"), "menu enablement still follows the selection");
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(clip(&s, c.id).unwrap().item, c.item);
    // a timeline selection plus an explicit item (no Project-panel selection)
    s.execute("timeline.select", json!({"clips": [c.id.0]})).unwrap();
    s.execute("clip.replaceFromBin", json!({"item": dunes.0})).unwrap();
    assert_eq!(clip(&s, c.id).unwrap().item, dunes);
}

#[test]
fn clip_commands_take_explicit_clips_without_a_selection() {
    let mut s = demo();
    let (a, b) = (v1(&s)[0].clone(), v1(&s)[1].clone());
    for id in ["edit.clear", "edit.rippleDelete", "clip.audioGain", "edit.pasteAttributes", "clip.enable"] {
        assert!(!s.is_enabled(id), "{id} is disabled without a selection");
        assert!(matches!(s.execute(id, json!({})), Err(EngineError::Disabled(..))), "{id} with no parameters stays disabled");
    }
    // Clear
    s.execute("edit.clear", json!({"clips": [a.id.0]})).unwrap();
    assert!(clip(&s, a.id).is_none() && clip(&s, b.id).is_some());
    s.execute("edit.undo", json!({})).unwrap();
    assert!(clip(&s, a.id).is_some());
    // Ripple Delete
    let a_and_its_audio: Vec<u64> = s
        .active_sequence()
        .unwrap()
        .all_tracks()
        .flat_map(|t| &t.items)
        .filter(|i| i.id == a.id || (i.link.is_some() && i.link == a.link))
        .map(|i| i.id.0)
        .collect();
    s.execute("timeline.setTrack", json!({"track": "A2", "syncLock": false})).unwrap(); // the music on A2 would block the ripple
    s.execute("edit.rippleDelete", json!({"clips": a_and_its_audio})).unwrap();
    assert!(clip(&s, a.id).is_none());
    assert_eq!(clip(&s, b.id).unwrap().start, a.start, "the next clip closes the gap");
    s.execute("edit.undo", json!({})).unwrap();
    // Audio Gain
    let au = a1(&s)[0].clone();
    s.execute("clip.audioGain", json!({"clips": [au.id.0], "mode": "set", "db": -6.0})).unwrap();
    assert_eq!(clip(&s, au.id).unwrap().gain_db, -6.0);
    // Enable
    s.execute("clip.enable", json!({"clips": [a.id.0]})).unwrap();
    assert!(!clip(&s, a.id).unwrap().enabled);
    s.execute("edit.undo", json!({})).unwrap();
    assert!(clip(&s, a.id).unwrap().enabled);
    s.execute("edit.redo", json!({})).unwrap();
    assert!(!clip(&s, a.id).unwrap().enabled);
    // `clip.enable` documents `clips` only: a key it does not document is not a named target
    assert_eq!(disabled(s.execute("clip.enable", json!({"clip": a.id.0}))), "no clips selected");
    // Paste Attributes: naming the clips lifts only the selection condition, not the clipboard one
    assert_eq!(disabled(s.execute("edit.pasteAttributes", json!({"clips": [b.id.0]}))), "copy a clip first");
    s.execute("timeline.select", json!({"clips": [a.id.0]})).unwrap();
    s.execute("edit.copy", json!({})).unwrap();
    s.state.selection.clear();
    s.execute("edit.pasteAttributes", json!({"clips": [b.id.0]})).unwrap();
    assert!(s.state.selection.is_empty(), "the selection is left as it was");
    for id in ["edit.clear", "edit.rippleDelete", "clip.audioGain", "edit.pasteAttributes"] {
        assert!(!s.is_enabled(id), "menu enablement of {id} still follows the selection");
    }
}

#[test]
fn naming_nothing_usable_does_not_enable_a_command() {
    let mut s = demo();
    let before = s.project.clone();
    // no ids, ids of no clip, the wrong type, and a key the command does not take
    for p in [json!({"clips": []}), json!({"clips": [987_654_321u64]}), json!({"clips": "all"}), json!({"clips": [-1, 1.5, null]}), json!({"items": [1]})] {
        assert_eq!(disabled(s.execute("edit.rippleDelete", p.clone())), "nothing selected", "{p}");
    }
    assert_eq!(disabled(s.execute("clip.replaceFromBin", json!({"clips": [v1(&s)[0].id.0], "item": 987_654_321u64}))), "select a clip in the Project panel");
    // `edit.cut` takes no `clips`: it works on the selection only
    assert_eq!(disabled(s.execute("edit.cut", json!({"clips": [v1(&s)[0].id.0]}))), "no clips selected");
    assert!(std::sync::Arc::ptr_eq(&before, &s.project), "nothing was edited");
}

#[test]
fn link_and_speed_take_explicit_clips_without_a_selection() {
    let mut s = demo();
    let (v, au) = (v1(&s)[0].clone(), a1(&s)[0].clone());
    for id in ["clip.link", "clip.speedDuration"] {
        assert!(!s.is_enabled(id));
        assert_eq!(disabled(s.execute(id, json!({}))), "no clips selected");
    }
    // Link toggles the named pair
    let was = v.link.is_some() && v.link == au.link;
    let r = s.execute("clip.link", json!({"clips": [v.id.0, au.id.0]})).unwrap();
    assert_eq!(r["linked"], !was);
    assert_eq!(clip(&s, v.id).unwrap().link.is_some(), !was);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(clip(&s, v.id).unwrap().link, v.link);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(clip(&s, v.id).unwrap().link.is_some(), !was);
    s.execute("edit.undo", json!({})).unwrap();
    // Speed/Duration on the last clip of V1 (nothing after it to run into)
    let last = v1(&s).last().unwrap().clone();
    s.execute("clip.speedDuration", json!({"clips": [last.id.0], "speed": 200})).unwrap();
    let fast = clip(&s, last.id).unwrap();
    assert!(fast.duration < last.duration, "{:?} -> {:?}", last.duration, fast.duration);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(clip(&s, last.id).unwrap().duration, last.duration);
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(clip(&s, last.id).unwrap().duration, fast.duration);
    assert!(s.state.selection.is_empty() && !s.is_enabled("clip.speedDuration"), "menu enablement still follows the selection");
}

#[test]
fn naming_targets_lifts_only_the_selection_condition() {
    // no sequence: named clips cannot stand in for one
    let mut empty = Session::default();
    assert_eq!(disabled(empty.execute("edit.clear", json!({"clips": [1]}))), "no sequence is open");
    assert_eq!(disabled(empty.execute("clip.speedDuration", json!({"clips": [1], "speed": 50}))), "no sequence is open");

    let mut s = demo();
    let a = v1(&s)[0].clone();
    // the wrong kind of target: a video clip is not a graphic
    assert_eq!(disabled(s.execute("graphics.setText", json!({"clip": a.id.0, "text": "x"}))), "select a graphic clip");
    // a locked track is still the edit's own business: named or selected, the clip stays
    s.execute("timeline.setTrack", json!({"track": "V1", "locked": true})).unwrap();
    let _ = s.execute("edit.clear", json!({"clips": [a.id.0]}));
    assert!(clip(&s, a.id).is_some(), "a clip on a locked track is not removed");
}

/// `project.delete` with explicit ids needs no Project-panel selection, for bins as much as for
/// items: the selection can hold both, so a named bin stands in for it (#244).
#[test]
fn project_delete_takes_explicit_bins_and_items_without_a_selection() {
    let mut s = demo();
    let before = s.project.items.len();
    let empty = s.execute("file.newBin", json!({"name": "Empty"})).unwrap()["bin"].as_u64().unwrap();
    let outer = s.execute("file.newBin", json!({"name": "Old footage"})).unwrap()["bin"].as_u64().unwrap();
    let ocean = item_named(&s, "Ocean_Sunset.mp4");
    s.execute("project.moveToBin", json!({"items": [ocean.0], "bin": outer})).unwrap();
    s.state.project_selection.clear();
    assert!(!s.is_enabled("project.delete"));
    assert_eq!(disabled(s.execute("project.delete", json!({}))), "select an item in the Project panel");
    // an empty bin, by its id alone
    let r = s.execute("project.delete", json!({"items": [empty]})).unwrap();
    assert_eq!(r, json!({"items": 0, "bins": 1}));
    assert!(s.project.root.find_bin(filmcraft_project::BinId(empty)).is_none());
    // a bin with a clip in it, and an item, each by id alone
    let r = s.execute("project.delete", json!({"items": [outer]})).unwrap();
    assert_eq!(r, json!({"items": 1, "bins": 1}));
    assert!(s.project.item(ocean).is_none());
    let dunes = item_named(&s, "Desert_Dunes.mp4");
    let r = s.execute("project.delete", json!({"items": [dunes.0]})).unwrap();
    assert_eq!(r, json!({"items": 1, "bins": 0}));
    assert_eq!(s.project.items.len(), before - 2);
    assert!(s.state.project_selection.is_empty(), "the selection is left as it was");
    assert!(!s.is_enabled("project.delete"), "menu enablement still follows the selection");
    // ids of nothing, or of the project's own top bin, do not stand in for a selection
    let root = s.project.root.id.0;
    assert_eq!(disabled(s.execute("project.delete", json!({"items": [root]}))), "select an item in the Project panel");
    assert_eq!(disabled(s.execute("project.delete", json!({"items": [999_999]}))), "select an item in the Project panel");
    s.execute("edit.undo", json!({})).unwrap();
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(s.project.items.len(), before);
}

#[test]
fn link_multiple_video_and_audio_pairs_creates_one_link_per_pair() {
    let mut s = demo();
    let v_clips = v1(&s);
    let a_clips = a1(&s);
    assert!(v_clips.len() >= 3 && a_clips.len() >= 3);
    let (v0, v1, v2) = (v_clips[0].id, v_clips[1].id, v_clips[2].id);
    let (a0, a1, a2) = (a_clips[0].id, a_clips[1].id, a_clips[2].id);

    // First, unlink all three pairs
    let r = s.execute("clip.link", json!({"clips": [v0.0, a0.0, v1.0, a1.0, v2.0, a2.0]})).unwrap();
    assert_eq!(r["linked"], false);
    for cid in [v0, a0, v1, a1, v2, a2] {
        assert_eq!(clip(&s, cid).unwrap().link, None);
    }

    // Now, run clip.link on all six clips. In Premiere parity (#507),
    // it should create 3 separate links (one per pair), not one big link for all six.
    let r = s.execute("clip.link", json!({"clips": [v0.0, a0.0, v1.0, a1.0, v2.0, a2.0]})).unwrap();
    assert_eq!(r["linked"], true);

    let l0 = clip(&s, v0).unwrap().link;
    let la0 = clip(&s, a0).unwrap().link;
    let l1 = clip(&s, v1).unwrap().link;
    let la1 = clip(&s, a1).unwrap().link;
    let l2 = clip(&s, v2).unwrap().link;
    let la2 = clip(&s, a2).unwrap().link;

    assert!(l0.is_some() && l1.is_some() && l2.is_some());
    assert_eq!(l0, la0, "v0 linked to a0");
    assert_eq!(l1, la1, "v1 linked to a1");
    assert_eq!(l2, la2, "v2 linked to a2");

    assert_ne!(l0, l1, "different pairs must not share the same link id");
    assert_ne!(l1, l2, "different pairs must not share the same link id");
    assert_ne!(l0, l2, "different pairs must not share the same link id");
}

/// A video clip with no overlapping selected audio, and audio with no overlapping video, stay
/// unlinked: a one-member link would make the clip read as "linked" with no partner.
#[test]
fn link_leaves_clips_without_a_partner_unlinked() {
    let mut s = demo();
    let (v, a) = (v1(&s), a1(&s));
    let (v0, v1, a0, a2) = (v[0].id, v[1].id, a[0].id, a[2].id);
    s.execute("clip.link", json!({"clips": [v0.0, v1.0, a0.0, a2.0]})).unwrap();
    s.execute("clip.link", json!({"clips": [v0.0, v1.0, a0.0, a2.0]})).unwrap();
    assert!(clip(&s, v0).unwrap().link.is_some());
    assert_eq!(clip(&s, v0).unwrap().link, clip(&s, a0).unwrap().link);
    assert_eq!(clip(&s, v1).unwrap().link, None, "v1 has no overlapping audio in the selection");
    assert_eq!(clip(&s, a2).unwrap().link, None, "a2 has no overlapping video in the selection");
}

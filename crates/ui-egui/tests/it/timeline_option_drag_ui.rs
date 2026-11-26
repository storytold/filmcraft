//! Option-drag of a timeline clip copies it (Premiere): the original stays, a copy lands where
//! the pointer lets go and is selected; a plain drag still moves.

use crate::support::{Drive, Launch, driver};
use serde_json::json;

driver!(Driver);

impl Driver {
    fn rect(&mut self, id: &str) -> [f64; 4] {
        self.expect_rect(id)
    }
}

fn clip_count(d: &mut Driver) -> usize {
    d.harness.state().session.active_sequence().map(|q| q.all_tracks().map(|t| t.items.len()).sum()).unwrap_or(0)
}

#[test]
fn option_drag_copies_a_clip_and_a_plain_drag_moves_it() {
    let mut d = Driver::demo();
    // the clip on V2 (no linked sound), and empty space to its right
    let q = d.harness.state().session.active_sequence().unwrap().clone();
    let v2 = q.video_tracks[1].items[0].clone();
    let n = clip_count(&mut d);
    let r = d.rect(&format!("timeline.clip.{}", v2.id.0));
    let (x, y) = (r[0] + 10.0, r[1] + r[3] / 2.0);
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 240.0, "y": y}, "steps": 12, "modifiers": {"alt": true}}));
    d.frames(3);
    let q2 = d.harness.state().session.active_sequence().unwrap().clone();
    assert_eq!(clip_count(&mut d), n + 1, "one copy was added");
    assert_eq!(q2.find_item(v2.id).map(|(_, i)| i.start), Some(v2.start), "the original stays");
    let sel = d.harness.state().session.state.selection.clone();
    assert_eq!(sel.len(), 1);
    let (track, copy) = q2.find_item(sel[0]).unwrap();
    assert_ne!(copy.id, v2.id);
    assert_eq!((track, copy.item, copy.duration), (q.video_tracks[1].id, v2.item, v2.duration));
    assert!(copy.start > v2.start, "the copy is where the pointer let go");
    // one undo removes the copy
    d.exec("edit.undo", json!({}));
    assert_eq!(clip_count(&mut d), n);
    // without Option the same drag moves the clip
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 240.0, "y": y}, "steps": 12}));
    d.frames(3);
    assert_eq!(clip_count(&mut d), n);
    assert!(d.harness.state().session.active_sequence().unwrap().find_item(v2.id).unwrap().1.start > v2.start, "moved");
}

#[test]
fn option_drag_preserves_an_already_selected_linked_group() {
    for audio in [false, true] {
        let mut d = Driver::demo();
        d.exec("file.newSequence", json!({"name": "Group copy", "video": 1, "audio": 1}));
        let session = &d.harness.state().session;
        let item = session.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).unwrap().id;
        let duration = session.sequence_rate().tick_of(48).0;
        for frame in [0, 72] {
            d.exec("timeline.place", json!({"item": item.0, "frame": frame, "duration": duration}));
        }
        d.ok("ui.set", json!({"timeline": {"pps": 40.0, "scroll": 0.0}}));
        d.frames(3);
        let before = d.harness.state().session.project.clone();
        let q = d.harness.state().session.active_sequence().unwrap().clone();
        let videos: Vec<u64> = q.video_tracks[0].items.iter().map(|i| i.id.0).collect();
        d.exec("timeline.select", json!({"clips": videos}));
        let original_selection = d.harness.state().session.state.selection.clone();
        assert_eq!(original_selection.len(), 4);
        let grabbed = if audio { &q.audio_tracks[0].items[0] } else { &q.video_tracks[0].items[0] };
        let r = d.rect(&format!("timeline.clip.{}", grabbed.id.0));
        let (x, y) = (r[0] + r[2] * 0.5, r[1] + r[3] * 0.3);
        d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 300.0, "y": y}, "steps": 12, "modifiers": {"alt": true}}));
        d.frames(3);
        let session = &d.harness.state().session;
        let copied = &session.state.selection;
        assert_eq!(copied.len(), 4, "Alt-drag copies the entire selected group");
        assert!(copied.iter().all(|id| !original_selection.contains(id)));
        assert!(session.state.linked_selection);
        let after = session.active_sequence().unwrap();
        after.check().unwrap();
        assert_eq!(after.all_tracks().map(|t| t.items.len()).sum::<usize>(), 8);
        let delta = copied.iter().map(|id| after.find_item(*id).unwrap().1.start).min().unwrap();
        assert!(delta > q.duration(), "the copies land in empty time");
        for (track, original) in q.all_tracks().flat_map(|t| t.items.iter().map(move |i| (t.id, i))) {
            assert_eq!(after.find_item(original.id).unwrap().1, original, "the originals stay put");
            let copy = after.track(track).unwrap().items.iter().find(|i| copied.contains(&i.id) && i.start == original.start + delta).unwrap();
            assert_eq!((copy.item, copy.source_in, copy.duration), (original.item, original.source_in, original.duration));
            assert_ne!(copy.link, original.link, "the copies have a new link group");
        }
        d.exec("edit.undo", json!({}));
        assert_eq!(*d.harness.state().session.project, *before, "one undo removes the group copy");
    }
}

#[test]
fn dragging_after_frame_hold_with_repeated_clip_arguments_edits_once() {
    for copy in [false, true] {
        let mut d = Driver::demo();
        let original = d.harness.state().session.active_sequence().unwrap().video_tracks[1].items[0].clone();
        d.exec("clip.frameHold", json!({"clips": [original.id.0, original.id.0], "time": original.start.0}));
        let before = d.harness.state().session.project.clone();
        let undo = d.harness.state().session.history.undo.len();
        let n = clip_count(&mut d);
        let r = d.rect(&format!("timeline.clip.{}", original.id.0));
        let (x, y) = (r[0] + 10.0, r[1] + r[3] / 2.0);
        d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 240.0, "y": y}, "steps": 12, "modifiers": {"alt": copy}}));
        d.frames(3);
        let s = &d.harness.state().session;
        let q = s.active_sequence().unwrap();
        if copy {
            assert_eq!(s.state.selection.len(), 1, "one selected clip produces one copy");
            let copied = s.state.selection[0];
            assert_ne!(copied, original.id);
            assert!(q.find_item(copied).unwrap().1.start > original.start);
            assert_eq!(q.find_item(original.id).unwrap().1.start, original.start);
        } else {
            assert!(q.find_item(original.id).unwrap().1.start > original.start, "a repeated selection must still drag");
        }
        let ids: Vec<_> = q.all_tracks().flat_map(|t| t.items.iter().map(|i| i.id)).collect();
        assert_eq!(ids.len(), n + usize::from(copy));
        assert_eq!(ids.iter().copied().collect::<std::collections::HashSet<_>>().len(), ids.len());
        q.check().unwrap();
        assert_eq!(s.history.undo.len(), undo + 1);
        let after = s.project.clone();
        d.exec("edit.undo", json!({}));
        assert_eq!(d.harness.state().session.project, before);
        d.exec("edit.redo", json!({}));
        assert_eq!(d.harness.state().session.project, after);
    }
}

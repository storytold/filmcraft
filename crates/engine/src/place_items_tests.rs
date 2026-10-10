//! `timeline.place` with `items` and `file.newSequence` with `fromItems`: several items back to
//! back, as one undo step.

use filmcraft_project::ItemId;
use serde_json::json;

use crate::Session;

/// A session on an empty sequence, and the ids of three media items with picture and sound.
fn three() -> (Session, Vec<ItemId>) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let items: Vec<ItemId> = s.project.items.values().filter(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id).take(3).collect();
    assert!(items.len() >= 2, "the demo project has media to place");
    s.execute("file.newSequence", json!({"name": "Back to back", "video": 2, "audio": 2})).unwrap();
    (s, items)
}

fn ends(s: &Session) -> Vec<(i64, i64)> {
    let q = s.active_sequence().unwrap();
    q.video_tracks[0].items.iter().map(|i| (i.start.0, i.end().0)).collect()
}

/// Dragging several selected items placed them one by one, each one an undo step.
#[test]
fn items_land_back_to_back_in_one_undo_step() {
    let (mut s, items) = three();
    let steps = s.history.undo.len();
    let r = s.execute("timeline.place", json!({"items": items.iter().map(|i| i.0).collect::<Vec<_>>(), "time": 0})).unwrap();
    assert_eq!(r["placed"].as_array().unwrap().len(), items.len());
    let spans = ends(&s);
    assert_eq!(spans.len(), items.len());
    assert_eq!(spans[0].0, 0);
    for w in spans.windows(2) {
        assert_eq!(w[0].1, w[1].0, "each item starts where the one before ends");
    }
    assert_eq!(s.history.undo.len(), steps + 1, "one undo step for all of them");
    assert_eq!(s.history.undo.last().map(|u| u.0.as_str()), Some("Place Clips"));
    s.undo();
    assert!(ends(&s).is_empty(), "one undo takes them all off");
    s.redo();
    assert_eq!(ends(&s), spans);
}

/// One bad item in the list leaves the sequence and the history as they were.
#[test]
fn a_failing_item_places_nothing() {
    let (mut s, items) = three();
    let steps = s.history.undo.len();
    let list = vec![items[0].0, u64::MAX - 1];
    assert!(s.execute("timeline.place", json!({"items": list, "time": 0})).is_err());
    assert!(ends(&s).is_empty());
    assert_eq!(s.history.undo.len(), steps);
}

/// `items` is checked: not a list, empty, mixed with `item`, or with one item's range.
#[test]
fn items_are_checked() {
    let (mut s, items) = three();
    let one = items[0].0;
    for p in [
        json!({"items": one}),
        json!({"items": []}),
        json!({"items": ["x"]}),
        json!({"items": [one], "item": one}),
        json!({"items": [one], "duration": 5}),
        json!({"items": [one], "sourceIn": 5}),
    ] {
        assert!(s.execute("timeline.place", p.clone()).is_err(), "{p}");
    }
    assert!(ends(&s).is_empty());
}

/// A new sequence from several items is one undo step: the sequence and its clips go together.
#[test]
fn new_sequence_from_items_is_one_undo_step() {
    let (mut s, items) = three();
    let steps = s.history.undo.len();
    let sequences = s.project.sequences().count();
    s.execute("file.newSequence", json!({"fromItems": items.iter().map(|i| i.0).collect::<Vec<_>>()})).unwrap();
    assert_eq!(ends(&s).len(), items.len());
    assert_eq!(s.history.undo.len(), steps + 1);
    s.undo();
    assert_eq!(s.project.sequences().count(), sequences);
    assert!(s.execute("file.newSequence", json!({"fromItems": 3})).is_err());
}

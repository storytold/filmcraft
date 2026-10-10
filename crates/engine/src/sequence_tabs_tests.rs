//! The Timeline's sequence tabs: closing the others, reordering, and what a project file keeps of
//! them (the open tabs in order, the active one, each sequence's zoom, scroll and track heights,
//! and each sequence's playhead). Premiere's tab behaviour was observed in Premiere Pro 26.5.2.

use super::*;
use filmcraft_project::SequenceView;
use filmcraft_time::TICKS_PER_SECOND;
use serde_json::json;

/// The demo project with two more sequences open: tabs [main, b, c], `c` active.
fn three_tabs() -> (Session, [ItemId; 3]) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let main = s.state.active_sequence.unwrap();
    let mut new = |name: &str| ItemId(s.execute("file.newSequence", json!({"name": name})).unwrap()["sequence"].as_u64().unwrap());
    let (b, c) = (new("B"), new("C"));
    assert_eq!(s.state.open_sequences, [main, b, c]);
    assert_eq!(s.state.active_sequence, Some(c));
    (s, [main, b, c])
}

fn view(pps: f64, scroll: f64) -> SequenceView {
    SequenceView { pps, scroll, v_scroll: 12.0, a_scroll: 7.0, video_track_h: 90.0, audio_track_h: 40.0 }
}

fn temp_dir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("filmcraft-tabs-{name}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn close_others_keeps_one_tab_and_shows_it() {
    let (mut s, [main, b, c]) = three_tabs();
    // from the active tab
    assert_eq!(s.execute("sequence.closeOthers", json!({})).unwrap()["closed"], json!(2));
    assert_eq!((s.state.open_sequences.clone(), s.state.active_sequence), (vec![c], Some(c)));
    // from another tab: that one becomes the active one
    let (mut s, _) = three_tabs();
    s.drain_events();
    s.execute("sequence.closeOthers", json!({"item": b.0})).unwrap();
    assert_eq!((s.state.open_sequences.clone(), s.state.active_sequence), (vec![b], Some(b)));
    assert!(s.drain_events().contains(&Event::OpenSequence(b)));
    // a sequence that is not open cannot be the one to keep
    s.execute("sequence.close", json!({})).unwrap();
    assert!(s.execute("sequence.closeOthers", json!({"item": main.0})).is_err());
}

#[test]
fn a_tab_moves_to_another_place_among_the_tabs() {
    let (mut s, [main, b, c]) = three_tabs();
    let (revision, steps) = (s.revision, s.history.undo.len());
    s.execute("sequence.moveTab", json!({"item": c.0, "index": 0})).unwrap();
    assert_eq!(s.state.open_sequences, [c, main, b]);
    assert_eq!(s.state.active_sequence, Some(c), "moving a tab does not change which is shown");
    s.execute("sequence.moveTab", json!({"item": c.0, "index": 1})).unwrap();
    assert_eq!(s.state.open_sequences, [main, c, b]);
    // past the end is the end; the active tab is the default
    assert_eq!(s.execute("sequence.moveTab", json!({"index": u64::MAX})).unwrap()["index"], json!(2));
    assert_eq!(s.state.open_sequences, [main, b, c]);
    // not an undo step, not an edit
    assert_eq!((s.revision, s.history.undo.len()), (revision, steps));
    s.execute("sequence.close", json!({"item": b.0})).unwrap();
    assert!(s.execute("sequence.moveTab", json!({"item": b.0, "index": 0})).is_err());
    assert!(s.execute("sequence.moveTab", json!({"item": c.0})).is_err());
}

#[test]
fn a_saved_project_reopens_with_its_tabs_and_their_views() {
    let (mut s, [main, b, c]) = three_tabs();
    s.execute("sequence.moveTab", json!({"item": c.0, "index": 0})).unwrap();
    s.execute("sequence.open", json!({"item": b.0})).unwrap();
    s.state.timeline_views.insert(main, view(12.5, 3.0));
    s.state.timeline_views.insert(b, view(400.0, 0.25));
    let dir = temp_dir("reopen");
    let path = dir.join("tabs.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": path})).unwrap();

    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(t.state.open_sequences, [c, main, b]);
    assert_eq!(t.state.active_sequence, Some(b));
    assert_eq!(t.state.timeline_views.get(&main), Some(&view(12.5, 3.0)));
    assert_eq!(t.state.timeline_views.get(&b), Some(&view(400.0, 0.25)));
    assert_eq!(t.state.timeline_views.get(&c), None, "a sequence that was never shown has no view");
    assert!(t.drain_events().contains(&Event::OpenSequence(b)));
    assert!(!t.is_dirty());

    // with the preference off, a project opens on its first sequence as before
    let mut u = Session::default();
    u.prefs.timeline.restore_open_sequences = false;
    u.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(u.state.open_sequences, [main]);
    assert_eq!(u.state.active_sequence, Some(main));
    assert!(u.state.timeline_views.is_empty());

    // saved with every tab closed (or by a session that never showed a sequence): the project
    // opens on its first sequence, not on an empty Timeline, and still knows the views
    s.execute("sequence.closeOthers", json!({})).unwrap();
    s.execute("sequence.close", json!({})).unwrap();
    s.execute("file.save", json!({})).unwrap();
    let mut w = Session::default();
    w.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!((w.state.open_sequences.clone(), w.state.active_sequence), (vec![main], Some(main)));
    assert_eq!(w.state.timeline_views.get(&b), Some(&view(400.0, 0.25)));
    let _ = std::fs::remove_dir_all(&dir);
}

/// Each sequence reopens with its playhead where it was left: the project file keeps one playhead
/// per sequence beside the tabs and views.
#[test]
fn each_sequence_reopens_with_its_playhead_where_it_was_left() {
    let (mut s, [main, b, c]) = three_tabs();
    let at = |s: &Session, seq: ItemId, secs: i64| s.project.sequence(seq).unwrap().settings.frame_rate.snap(Tick(secs * TICKS_PER_SECOND));
    s.execute("sequence.open", json!({"item": main.0})).unwrap();
    s.execute("playhead.set", json!({"seconds": 7})).unwrap();
    s.execute("sequence.open", json!({"item": b.0})).unwrap();
    s.execute("playhead.set", json!({"seconds": 2})).unwrap();
    let (main_at, b_at) = (at(&s, main, 7), at(&s, b, 2));
    assert_eq!(s.playhead(), b_at);
    assert_eq!(s.project_view().playheads.get(&c), None, "a playhead that never moved is not written");
    let dir = temp_dir("playheads");
    let path = dir.join("tabs.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": path})).unwrap();

    let mut t = Session::default();
    t.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(t.state.active_sequence, Some(b));
    assert_eq!(t.playhead(), b_at, "the sequence that was shown is back where it was");
    t.execute("sequence.open", json!({"item": main.0})).unwrap();
    assert_eq!(t.playhead(), main_at, "and so is every other sequence");
    t.execute("sequence.open", json!({"item": c.0})).unwrap();
    assert_eq!(t.playhead(), Tick::ZERO);
    assert!(!t.is_dirty(), "a restored playhead is not an edit");

    // with the preference off, nothing of the view comes back, playheads included
    let mut u = Session::default();
    u.prefs.timeline.restore_open_sequences = false;
    u.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(u.playhead(), Tick::ZERO);

    // a file written before playheads were kept opens as it did then: every playhead at zero
    let mut doc: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(doc["view"]["playheads"].is_object());
    doc["view"].as_object_mut().unwrap().remove("playheads");
    std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
    let mut w = Session::default();
    w.execute("file.open", json!({"path": path})).unwrap();
    assert_eq!(w.state.active_sequence, Some(b));
    assert_eq!(w.playhead(), Tick::ZERO);
    assert!(w.state.playheads.is_empty());
    let _ = std::fs::remove_dir_all(&dir);
}

/// The view in a project file is not trusted: ids of things that are not sequences, repeats and
/// numbers out of range are dropped or brought into range, and a view that cannot be read at all
/// does not stop the project from opening.
#[test]
fn a_damaged_view_in_a_project_file_is_cleaned_up_or_ignored() {
    let (mut s, [main, b, c]) = three_tabs();
    let footage = s.project.sequence(main).unwrap().video_tracks[0].items[0].item;
    let dir = temp_dir("damaged");
    let path = dir.join("tabs.fcproj").to_string_lossy().to_string();
    s.execute("file.saveAs", json!({"path": path})).unwrap();
    let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let with_view = |view: serde_json::Value| {
        let mut doc = saved.clone();
        doc["view"] = view;
        std::fs::write(&path, serde_json::to_vec(&doc).unwrap()).unwrap();
        let mut t = Session::default();
        t.execute("file.open", json!({"path": path})).unwrap();
        t
    };

    let t = with_view(json!({
        "open_sequences": [c.0, 999_999, footage.0, c.0, b.0],
        "active_sequence": 999_999,
        "sequences": {
            b.0.to_string(): {"pps": 1e300, "scroll": -5.0, "v_scroll": 1e30, "a_scroll": -1.0, "video_track_h": 0.0, "audio_track_h": 1e9},
            "999999": {"pps": 40.0, "scroll": 0.0, "video_track_h": 60.0, "audio_track_h": 56.0},
            footage.0.to_string(): {"pps": 40.0, "scroll": 0.0, "video_track_h": 60.0, "audio_track_h": 56.0},
        },
        "playheads": {
            c.0.to_string(): -5,
            b.0.to_string(): i64::MAX,
            main.0.to_string(): TICKS_PER_SECOND + 1,
            "999999": TICKS_PER_SECOND,
            footage.0.to_string(): TICKS_PER_SECOND,
        },
    }));
    assert_eq!(t.state.open_sequences, [c, b]);
    assert_eq!(t.state.active_sequence, Some(c), "an active tab that is not open: the first tab");
    assert_eq!(t.state.timeline_views.len(), 1);
    let v = t.state.timeline_views[&b];
    assert!(v.pps <= 1e5 && v.scroll == 0.0 && v.v_scroll <= 1e6 && v.a_scroll == 0.0);
    assert!(v.video_track_h >= 8.0 && v.audio_track_h <= 600.0);
    // playheads: only sequences keep one, never before zero or past the bound, always on a frame
    let rate = |seq: ItemId| t.project.sequence(seq).unwrap().settings.frame_rate;
    assert_eq!(t.state.playheads.keys().copied().collect::<Vec<_>>(), {
        let mut ids = vec![main, b, c];
        ids.sort();
        ids
    });
    assert_eq!(t.state.playheads[&c], Tick::ZERO);
    assert_eq!(t.state.playheads[&b], rate(b).snap(filmcraft_project::ProjectView::MAX_PLAYHEAD));
    assert_eq!(t.state.playheads[&main], rate(main).snap(Tick(TICKS_PER_SECOND + 1)));

    // not a view at all: the project opens as it does without one
    for junk in [json!("tabs"), json!([1, 2, 3]), json!({"open_sequences": "all"}), json!({"sequences": {"x": 1}}), json!(null)] {
        let t = with_view(junk.clone());
        assert_eq!((t.state.open_sequences.clone(), t.state.active_sequence), (vec![main], Some(main)), "{junk}");
    }
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn the_view_of_a_deleted_sequence_is_forgotten() {
    let (mut s, [main, b, c]) = three_tabs();
    for id in [main, b, c] {
        s.state.timeline_views.insert(id, view(40.0, 0.0));
    }
    s.execute("project.delete", json!({"items": [b.0]})).unwrap();
    assert_eq!(s.state.timeline_views.keys().copied().collect::<Vec<_>>(), [main, c]);
    assert_eq!(s.state.open_sequences, [main, c]);
    // the saved view never names it either, nor keeps its playhead
    assert_eq!(s.project_view().open_sequences, [main, c]);
    assert!(!s.project_view().playheads.contains_key(&b));
}

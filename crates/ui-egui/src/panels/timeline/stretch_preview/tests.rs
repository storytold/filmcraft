use super::*;
use crate::panels::timeline::{Drag, interact::preview_drag};
use filmcraft_engine::Session;
use serde_json::json;

/// Build aligned offline media followed by another linked pair, with a locked music bed.
fn fixture(duration: i64) -> (FilmcraftApp, Vec<ClipId>) {
    let mut session = Session::default();
    session.execute("file.newProject", json!({"name":"Preview cache"})).unwrap();
    session.execute("file.newSequence", json!({"fps":25,"video":1,"audio":2})).unwrap();
    let item =
        session.execute("file.newOfflineFile", json!({"name":"Synthetic","seconds":10,"fps":25,"video":true,"audio":true})).unwrap()["item"].as_u64().unwrap();
    let frame = session.sequence_rate().frame_duration().0;
    let mut clips = Vec::new();
    for n in 0..2 {
        let placed = session
            .execute(
                "timeline.place",
                json!({"item":item,"track":"V1","audioTrack":"A1","time":n*duration*frame,"sourceIn":20*frame,"duration":duration*frame}),
            )
            .unwrap();
        clips.extend(placed["clips"].as_array().unwrap().iter().map(|c| ClipId(c.as_u64().unwrap())));
    }
    session.execute("timeline.place", json!({"item":item,"track":"A2","time":0,"duration":200*frame})).unwrap();
    session.execute("timeline.setTrack", json!({"track":"A2","locked":true})).unwrap();
    (FilmcraftApp::new(session), clips)
}

/// Pointer updates and drawing reuse one edit snapshot while preserving project and history.
#[test]
fn drawing_reuses_the_pointer_preview_and_releases_it_after_the_drag() {
    let (mut app, clips) = fixture(60);
    let delta = app.session.sequence_rate().tick_of(10);
    let before = app.session.project.clone();
    let history = app.session.history.undo.len();
    let pointer = cached(&app, clips[0], Edge::Out, delta, true, true).unwrap();
    app.tl.drag = Some(Drag::Stretch { clip: clips[0], edge: Edge::Out, delta, from: Tick::ZERO, ripple: true, linked: true });
    let seq = app.session.active_sequence().unwrap().clone();
    let (mut positions, mut edited) = (std::collections::HashMap::new(), std::collections::HashMap::new());
    let drawn = preview_drag(&app, &seq, &mut positions, &mut edited).unwrap();
    assert!(Arc::ptr_eq(&pointer, &drawn));
    assert_eq!(positions[&clips[2]].0, seq.find_item(clips[2]).unwrap().1.start + delta);
    assert_eq!(drawn.audio_tracks[1], seq.audio_tracks[1], "locked music remains fixed");
    assert!(Arc::ptr_eq(&app.session.project, &before));
    assert_eq!(app.session.history.undo.len(), history);
    app.tl.drag = None;
    assert!(preview_drag(&app, &seq, &mut positions, &mut edited).is_none());
    assert!(!Arc::ptr_eq(&pointer, &cached(&app, clips[0], Edge::Out, delta, true, true).unwrap()));
}

/// A pointer request clamped by its neighbour shares the accepted-delta draw's snapshot.
#[test]
fn clamped_requests_reuse_their_accepted_delta() {
    let (app, clips) = fixture(60);
    let delta = app.session.sequence_rate().tick_of(20);
    let requested = cached(&app, clips[0], Edge::Out, delta, false, true).unwrap();
    assert_eq!(requested.find_item(clips[0]).unwrap().1.duration, app.session.sequence_rate().tick_of(60));
    let accepted = cached(&app, clips[0], Edge::Out, Tick::ZERO, false, true).unwrap();
    assert!(Arc::ptr_eq(&requested, &accepted));
}

/// Changing a stretch parameter or link-selection state computes the corresponding edit.
#[test]
fn changed_inputs_do_not_reuse_an_incompatible_preview() {
    let (mut app, clips) = fixture(60);
    let frame = app.session.sequence_rate().frame_duration();
    let first = cached(&app, clips[0], Edge::Out, Tick(frame.0 * 10), true, true).unwrap();
    let longer = cached(&app, clips[0], Edge::Out, Tick(frame.0 * 20), true, true).unwrap();
    assert!(!Arc::ptr_eq(&first, &longer));
    assert_eq!(longer.find_item(clips[0]).unwrap().1.duration, Tick(frame.0 * 80));
    let regular = cached(&app, clips[0], Edge::Out, Tick(frame.0 * 10), false, true).unwrap();
    assert_eq!(regular.find_item(clips[0]).unwrap().1.duration, Tick(frame.0 * 60));
    let unlinked = cached(&app, clips[0], Edge::Out, Tick(frame.0 * 10), true, false).unwrap();
    assert_eq!(unlinked.find_item(clips[1]).unwrap().1.duration, Tick(frame.0 * 60));
    app.session.state.linked_selection = false;
    let selection_off = cached(&app, clips[0], Edge::Out, Tick(frame.0 * 10), true, true).unwrap();
    assert_eq!(selection_off.find_item(clips[1]).unwrap().1.duration, Tick(frame.0 * 60));
    let other = cached(&app, clips[2], Edge::Out, Tick(frame.0 * 10), true, true).unwrap();
    assert_eq!(other.find_item(clips[2]).unwrap().1.duration, Tick(frame.0 * 70));
    let inside = cached(&app, clips[0], Edge::In, Tick(frame.0 * 10), false, true).unwrap();
    assert_eq!(inside.find_item(clips[0]).unwrap().1.start, Tick(frame.0 * 10));
    app.session.state.active_sequence = None;
    assert!(cached(&app, clips[0], Edge::Out, Tick(frame.0 * 10), true, true).is_err());
}

/// Cache blocked requests alongside unchanged geometry; project edits and undo invalidate both.
#[test]
fn failures_are_reused_and_revision_changes_recompute() {
    let (mut app, clips) = fixture(60);
    let delta = app.session.sequence_rate().tick_of(10);
    // Adding the first caption otherwise creates its track as a separate undoable operation.
    app.session.execute("captions.newTrack", json!({})).unwrap();
    let before = cached(&app, clips[0], Edge::Out, delta, true, true).unwrap();
    app.session.execute("captions.add", json!({"text":"Synthetic caption","time":app.session.sequence_rate().tick_of(80).0,"durationSeconds":1})).unwrap();
    let error = cached(&app, clips[0], Edge::Out, delta, true, true).unwrap_err();
    let unchanged = cached(&app, clips[0], Edge::Out, Tick::ZERO, true, true).unwrap();
    let repeated = cached(&app, clips[0], Edge::Out, delta, true, true).unwrap_err();
    assert!(Arc::ptr_eq(&error, &repeated));
    assert!(Arc::ptr_eq(&unchanged, &cached(&app, clips[0], Edge::Out, Tick::ZERO, true, true).unwrap()));
    // As a blocked pointer keeps moving, each draw still reuses the unchanged geometry.
    for frames in [20, 30] {
        assert!(cached(&app, clips[0], Edge::Out, app.session.sequence_rate().tick_of(frames), true, true).is_err());
        assert!(Arc::ptr_eq(&unchanged, &cached(&app, clips[0], Edge::Out, Tick::ZERO, true, true).unwrap()));
    }
    app.session.execute("edit.undo", json!({})).unwrap();
    let after = cached(&app, clips[0], Edge::Out, delta, true, true).unwrap();
    assert!(!Arc::ptr_eq(&before, &after));
    assert_eq!(*before, *after);
}

/// Project replacement can repeat ids/revision, so cache identity follows the actual project too.
#[test]
fn project_replacement_with_repeated_ids_and_revision_is_not_a_cache_hit() {
    let (mut app, clips) = fixture(60);
    let delta = app.session.sequence_rate().tick_of(10);
    let before = cached(&app, clips[0], Edge::Out, delta, true, true).unwrap();
    let (other, other_clips) = fixture(40);
    assert_eq!(clips, other_clips);
    assert_eq!(app.session.revision, other.session.revision);
    app.session = other.session;
    let after = cached(&app, clips[0], Edge::Out, delta, true, true).unwrap();
    assert!(!Arc::ptr_eq(&before, &after));
    assert_eq!(after.find_item(clips[0]).unwrap().1.duration, app.session.sequence_rate().tick_of(50));
}

/// Marker-ripple state changes independently of the project revision and belongs in the preview key.
#[test]
fn ripple_rate_stretch_marker_preference_invalidates_cached_geometry() {
    let (mut app, clips) = fixture(60);
    let rate = app.session.sequence_rate();
    app.session.execute("markers.add", json!({"target":"program","time":rate.tick_of(60).0,"name":"cut"})).unwrap();
    app.session.state.ripple_sequence_markers = false;
    let revision = app.session.revision;
    let before = cached(&app, clips[0], Edge::Out, rate.tick_of(10), true, true).unwrap();
    assert_eq!(before.markers[0].start, rate.tick_of(60));
    app.session.state.ripple_sequence_markers = true;
    let after = cached(&app, clips[0], Edge::Out, rate.tick_of(10), true, true).unwrap();
    assert_eq!(app.session.revision, revision);
    assert!(!Arc::ptr_eq(&before, &after), "the preference changes the requested preview");
    assert_eq!(after.markers[0].start, rate.tick_of(70));
    app.session.state.ripple_sequence_markers = false;
    assert_eq!(cached(&app, clips[0], Edge::Out, rate.tick_of(10), true, true).unwrap().markers[0].start, rate.tick_of(60));
}

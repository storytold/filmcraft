//! Timeline edits that change nothing (a trim already at the media end, a razor where no clip is,
//! a clip moved to where it already is) add no undo step, keep Redo and leave the saved state
//! alone, as an unchanged clip name and a dynamic trim with no offset already do.

use serde_json::{Value, json};

use crate::Session;

/// One frame at 30 fps, in ticks.
const FRAME: i64 = 8_467_200_000;

/// A 30 fps sequence: a two-second picture and sound clip at frame 0 on V1 and A1, and two
/// two-second picture clips back to back on V2. Returns the session and the V1 clip, then the
/// left and right V2 clips.
fn fixture() -> (Session, u64, u64, u64) {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "No-op"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 30, "video": 2, "audio": 1, "width": 16, "height": 16})).unwrap();
    let id = |v: Value, key: &str| v[key].as_u64().unwrap();
    let av = id(
        s.execute("file.newOfflineFile", json!({"name": "AV", "seconds": 2, "fps": 30, "video": true, "audio": true, "width": 16, "height": 16})).unwrap(),
        "item",
    );
    let picture = id(
        s.execute("file.newOfflineFile", json!({"name": "Picture", "seconds": 2, "fps": 30, "video": true, "audio": false, "width": 16, "height": 16}))
            .unwrap(),
        "item",
    );
    let first = |v: Value| v["clips"][0].as_u64().unwrap();
    let clip = first(s.execute("timeline.place", json!({"item": av, "track": "V1", "audioTrack": "A1", "frame": 0})).unwrap());
    let left = first(s.execute("timeline.place", json!({"item": picture, "track": "V2", "frame": 0})).unwrap());
    let right = first(s.execute("timeline.place", json!({"item": picture, "track": "V2", "frame": 60})).unwrap());
    (s, clip, left, right)
}

#[test]
fn timeline_edits_that_change_nothing_add_no_undo_step_and_keep_redo() {
    let (mut s, clip, left, right) = fixture();
    // a real trim, taken back, so Redo holds a step a no-op must not discard
    let trim = s.execute("timeline.trim", json!({"clip": clip, "edge": "out", "mode": "regular", "deltaFrames": -10})).unwrap();
    assert_eq!(trim["delta"], json!(-10 * FRAME));
    s.execute("edit.undo", json!({})).unwrap();
    let project = s.project.clone();
    let (undo, redo, revision) = (s.history.undo.len(), s.history.redo.len(), s.revision);
    assert_eq!(redo, 1);

    for (id, params) in [
        // the clip already shows its media from the first frame to the last
        ("timeline.trim", json!({"clip": clip, "edge": "out", "mode": "regular", "deltaFrames": 5})),
        ("timeline.trim", json!({"clip": clip, "edge": "in", "mode": "regular", "deltaFrames": -5})),
        ("timeline.slip", json!({"clip": clip, "deltaFrames": 3})),
        // and starts at the beginning of the sequence
        ("timeline.slide", json!({"clip": clip, "deltaFrames": -3})),
        ("timeline.razor", json!({"time": 100 * FRAME, "track": "V1"})),
        ("timeline.razor", json!({"time": 0, "track": "V1"})),
        ("timeline.roll", json!({"left": left, "right": right, "deltaFrames": 0})),
        ("timeline.move", json!({"moves": [{"clip": clip, "track": "V1", "time": 0}], "insert": false})),
        ("clip.speedDuration", json!({"clips": [clip], "speed": 100, "reverse": false, "ripple": false})),
        ("timeline.rateStretch", json!({"clip": clip, "edge": "out", "delta": 0})),
    ] {
        s.execute(id, params.clone()).unwrap_or_else(|e| panic!("{id} {params}: {e}"));
        assert_eq!(*s.project, *project, "{id} {params} changes nothing");
        assert_eq!(s.history.undo.len(), undo, "{id} {params} adds no undo step");
        assert_eq!(s.history.redo.len(), redo, "{id} {params} keeps Redo");
        assert_eq!(s.revision, revision, "{id} {params} leaves the project's revision (its saved state) alone");
    }

    // Redo still brings the trim back
    s.execute("edit.redo", json!({})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 1);
    let v1 = s.active_sequence().unwrap().all_tracks().flat_map(|t| &t.items).find(|i| i.id.0 == clip).map(|i| i.duration.0);
    assert_eq!(v1, Some(50 * FRAME));
    // and an edit that changes something still records its step
    s.execute("timeline.trim", json!({"clip": clip, "edge": "out", "mode": "regular", "deltaFrames": 5})).unwrap();
    assert_eq!(s.history.undo.len(), undo + 2);
    assert_eq!(s.history.undo.last().map(|u| u.0.as_str()), Some("Trim"));
}

#[test]
fn clearing_a_clip_on_a_locked_track_adds_no_undo_step() {
    let (mut s, _, left, _) = fixture();
    s.execute("timeline.setTrack", json!({"track": "V2", "locked": true})).unwrap();
    let project = s.project.clone();
    let undo = s.history.undo.len();
    // the locked clip stays, so there is nothing to take back
    s.execute("timeline.delete", json!({"clips": [left]})).unwrap();
    assert_eq!(*s.project, *project);
    assert_eq!(s.history.undo.len(), undo);
}

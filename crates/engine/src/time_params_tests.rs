//! Time parameters (`time` ticks, `frame`, `seconds`, `timecode`, and their prefixed forms such as
//! `startSeconds`): one given in the wrong form is a parameter error of the command, never
//! skipped in favour of the playhead (#4008).

use serde_json::{Value, json};

use crate::{EngineError, Session};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name": "Times", "fps": 24})).unwrap();
    s
}

fn refused(s: &mut Session, cmd: &str, p: Value) -> String {
    let e = s.execute(cmd, p.clone()).unwrap_err();
    assert!(matches!(&e, EngineError::BadParams { cmd: c, .. } if c == cmd), "{cmd} {p}: {e:?}");
    e.to_string()
}

/// `timeline.place {time: {"seconds": 10}}` put the clip on the playhead, over the clip there.
#[test]
fn a_time_object_is_refused_and_the_clip_under_the_playhead_stays() {
    let mut s = demo();
    let items: Vec<u64> = s.project.items.values().filter(|i| i.has_video() && i.as_media().is_some()).map(|i| i.id.0).take(2).collect();
    s.execute("timeline.place", json!({"item": items[0], "track": "V1", "seconds": 0})).unwrap();
    let before = s.project.to_json();
    let e = refused(&mut s, "timeline.place", json!({"item": items[1], "track": "V1", "time": {"seconds": 10}}));
    assert!(e.contains("`time` must be integer ticks") && e.contains("`seconds`"), "{e}");
    assert_eq!(s.project.to_json(), before, "the clip at frame 0 is still there");
    s.execute("timeline.place", json!({"item": items[1], "track": "V1", "seconds": 10})).unwrap();
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.len(), 2);
}

/// Every command that reads its time through the shared parser refuses a wrong form, and a wrong
/// key next to a right one; `null` still means not given.
#[test]
fn every_time_key_in_the_wrong_form_is_refused() {
    let mut s = demo();
    for p in [
        json!({"time": "5"}),
        json!({"time": 1.5}),
        json!({"time": {"seconds": 1}}),
        json!({"frame": 24.0}),
        json!({"frame": "24"}),
        json!({"seconds": "1"}),
        json!({"timecode": 5}),
        json!({"timecode": "00:00:xx:00"}),
        json!({"time": 0, "frame": "x"}),
    ] {
        refused(&mut s, "playhead.set", p);
        assert_eq!(s.playhead().0, 0);
    }
    s.execute("playhead.set", json!({"time": null, "frame": 24})).unwrap();
    assert_eq!(s.playhead(), s.sequence_rate().tick_of(24));

    let markers = s.active_sequence().unwrap().markers.len();
    refused(&mut s, "markers.add", json!({"time": {"seconds": 1}}));
    assert_eq!(s.active_sequence().unwrap().markers.len(), markers);

    let e = refused(&mut s, "export.resolve", json!({"range": "custom", "startSeconds": "x", "endSeconds": 1}));
    assert!(e.contains("`startSeconds` must be a number") && e.contains("`startTimecode`"), "{e}");
}

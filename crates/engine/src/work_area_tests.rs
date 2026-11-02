//! The work area bar: the Timeline panel menu's Work Area Bar shows and hides it, Set Work Area Bar
//! In / Out Point (Alt+[ / Alt+] in the Timeline) move its ends to the playhead, `sequence.setWorkArea`
//! sets both ends (a drag of the bar), and Export ▸ Range ▸ Work Area exports it.

use filmcraft_time::{Tick, TimeRange};
use serde_json::json;

use crate::Session;
use crate::shortcuts::{Chord, Platform};

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s
}

fn area(s: &Session) -> Option<(Tick, Tick)> {
    s.active_sequence().unwrap().work_area.map(|w| (w.start, w.end()))
}

#[test]
fn the_work_area_bar_spans_the_sequence_when_shown_and_undoes() {
    let mut s = demo();
    let end = s.active_sequence().unwrap().duration();
    assert!(end > Tick::ZERO);
    assert_eq!(area(&s), None, "the bar starts hidden");
    assert!(s.execute("export.resolve", json!({"range": "workArea"})).is_err(), "no work area to export");

    assert_eq!(s.execute("sequence.workAreaBar", json!({})).unwrap(), json!({"show": true}));
    assert_eq!(area(&s), Some((Tick::ZERO, end)));
    assert_eq!(s.history.undo.last().map(|u| u.0.as_str()), Some("Work Area Bar"));
    let frames = s.execute("export.resolve", json!({"preset": "PNG Sequence", "range": "workArea"})).unwrap()["frames"].as_i64().unwrap();
    let whole = s.execute("export.resolve", json!({"preset": "PNG Sequence", "range": "entire"})).unwrap()["frames"].as_i64().unwrap();
    assert_eq!(frames, whole, "the work area covers the whole sequence");

    // asking for the state it is already in changes nothing
    let steps = s.history.undo.len();
    assert_eq!(s.execute("sequence.workAreaBar", json!({"show": true})).unwrap(), json!({"show": true}));
    assert_eq!(s.history.undo.len(), steps);

    assert_eq!(s.execute("sequence.workAreaBar", json!({})).unwrap(), json!({"show": false}));
    assert_eq!(area(&s), None);
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(area(&s), Some((Tick::ZERO, end)));
}

#[test]
fn set_work_area_in_and_out_points_follow_the_playhead() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let f = |n: i64| rate.tick_of(n);
    let end = s.active_sequence().unwrap().duration();

    // with no bar yet, the In point runs to the end of the sequence
    s.set_playhead(f(24));
    assert_eq!(s.execute("sequence.setWorkAreaIn", json!({})).unwrap(), json!({"start": f(24).0, "end": end.0}));
    assert_eq!(area(&s), Some((f(24), end)));
    assert_eq!(s.history.undo.last().map(|u| u.0.as_str()), Some("Set Work Area Bar In Point"));

    s.set_playhead(f(72));
    s.execute("sequence.setWorkAreaOut", json!({})).unwrap();
    assert_eq!(area(&s), Some((f(24), f(72))));
    assert_eq!(s.history.undo.last().map(|u| u.0.as_str()), Some("Set Work Area Bar Out Point"));
    let frames = s.execute("export.resolve", json!({"preset": "PNG Sequence", "range": "workArea"})).unwrap()["frames"].as_i64().unwrap();
    assert!((47..=49).contains(&frames), "{frames}");

    // an In point past the Out point moves the Out point to the end of the sequence
    s.execute("sequence.setWorkAreaIn", json!({"frame": 96})).unwrap();
    assert_eq!(area(&s), Some((f(96), end)));
    // an Out point before the In point moves the In point to the start
    s.execute("sequence.setWorkAreaOut", json!({"frame": 48})).unwrap();
    assert_eq!(area(&s), Some((Tick::ZERO, f(48))));
    // an In point at or past the end of the sequence keeps one frame
    s.execute("sequence.setWorkAreaIn", json!({"time": end.0})).unwrap();
    assert_eq!(area(&s), Some((end, end + rate.frame_duration())));

    let before = area(&s);
    assert!(s.execute("sequence.setWorkAreaOut", json!({"time": 0})).is_err(), "an Out point at 0 leaves no work area");
    assert!(s.execute("sequence.setWorkAreaIn", json!({"time": -1})).is_err(), "negative times are refused");
    assert_eq!(area(&s), before);
}

#[test]
fn set_work_area_takes_both_ends_and_refuses_bad_ones() {
    let mut s = demo();
    let rate = s.sequence_rate();
    let (a, b) = (rate.tick_of(10), rate.tick_of(20));
    assert_eq!(s.execute("sequence.setWorkArea", json!({"start": a.0, "end": b.0})).unwrap(), json!({"start": a.0, "end": b.0}));
    assert_eq!(s.active_sequence().unwrap().work_area, Some(TimeRange::from_bounds(a, b)));
    for p in [
        json!({"start": b.0, "end": a.0}),
        json!({"start": a.0, "end": a.0}),
        json!({"start": -1, "end": b.0}),
        json!({"start": a.0}),
        json!({"end": b.0}),
        json!({"start": 0, "end": i64::MAX}),
    ] {
        assert!(s.execute("sequence.setWorkArea", p.clone()).is_err(), "{p}");
        assert_eq!(s.active_sequence().unwrap().work_area, Some(TimeRange::from_bounds(a, b)), "{p}");
    }
    let mut empty = Session::default();
    for id in ["sequence.workAreaBar", "sequence.setWorkAreaIn", "sequence.setWorkAreaOut", "sequence.setWorkArea"] {
        assert!(empty.execute(id, json!({"start": 0, "end": 1})).is_err(), "{id} needs a sequence");
    }
}

#[test]
fn alt_brackets_set_the_work_area_in_the_timeline() {
    let s = Session::default();
    for p in [Platform::Mac, Platform::Windows] {
        for (keys, cmd) in [("Alt+[", "sequence.setWorkAreaIn"), ("Alt+]", "sequence.setWorkAreaOut")] {
            let chord = Chord::parse(keys).unwrap();
            assert_eq!(s.shortcuts.resolve(&chord, Some("Timeline"), p).map(|b| b.command.as_str()), Some(cmd), "{keys} {p:?}");
        }
    }
}

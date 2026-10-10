//! Trim Monitor + dynamic (J/K/L) trimming, driven by an exact simulated clock.

use super::*;
use serde_json::json;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    for t in ["V2", "V3", "A2", "A3"] {
        let _ = s.execute("timeline.setTrack", json!({"track": t, "syncLock": false}));
    }
    s
}

/// V1 clips as (id, start, end).
fn v1(s: &Session) -> Vec<(u64, Tick, Tick)> {
    s.active_sequence().unwrap().video_tracks[0].items.iter().map(|i| (i.id.0, i.start, i.end())).collect()
}

/// Whole frames covered by `secs` of real time at the sequence rate.
fn frames(s: &Session, secs: f64) -> i64 {
    s.sequence_rate().frame_at(Tick::from_seconds_f64(secs))
}

/// Select the cut between V1 clips 0 and 1 as `kind` (roll / ripple out of clip 0).
fn select_first_cut(s: &mut Session, kind: &str) -> (u64, Tick) {
    let c = v1(s);
    s.execute("trim.selectEditPoint", json!({"clip": c[0].0, "edge": "out", "kind": kind})).unwrap();
    (c[0].0, c[0].2)
}

#[test]
fn dynamic_roll_forward_is_one_undo_step() {
    let mut s = demo();
    let (left, cut) = select_first_cut(&mut s, "roll");
    let rate = s.sequence_rate();
    let undo0 = s.history.undo.len();
    let journal0 = s.journal.len();
    let dur0 = s.active_sequence().unwrap().duration();
    s.execute("trim.shuttle", json!({"direction": "forward", "clock": 10.0})).unwrap();
    // live: the cut moves with the clock, no history yet
    s.execute("trim.tick", json!({"clock": 10.5})).unwrap();
    let mid = v1(&s).into_iter().find(|c| c.0 == left).unwrap();
    assert_eq!(mid.2, cut + rate.tick_of(frames(&s, 0.5)), "half a second of roll");
    assert_eq!(s.playhead(), mid.2, "the Program monitor follows the edit");
    assert_eq!(s.history.undo.len(), undo0, "no undo step while trimming");
    let info = s.execute("trim.monitor", json!({})).unwrap();
    assert_eq!(info["dynamic"]["offsetFrames"], json!(frames(&s, 0.5)), "{info}");
    assert_eq!(info["outShift"], json!(frames(&s, 0.5)));
    assert_eq!(info["inShift"], json!(frames(&s, 0.5)), "roll moves both sides");
    // K commits
    let r = s.execute("trim.shuttleStop", json!({"clock": 11.0})).unwrap();
    assert_eq!(r["committed"], json!(true), "{r}");
    assert_eq!(r["offsetFrames"], json!(frames(&s, 1.0)));
    let after = v1(&s).into_iter().find(|c| c.0 == left).unwrap();
    assert_eq!(after.2, cut + rate.tick_of(frames(&s, 1.0)));
    assert_eq!(s.active_sequence().unwrap().duration(), dur0, "roll keeps the duration");
    assert_eq!(s.history.undo.len(), undo0 + 1, "exactly one undo step");
    assert_eq!(s.history.undo.last().unwrap().0, "Dynamic Rolling Edit");
    assert!(
        s.journal[journal0..].iter().all(|(id, _)| id.starts_with("trim.")),
        "live updates are not journaled as separate edits: {:?}",
        &s.journal[journal0..]
    );
    assert!(s.trim_play.dynamic.is_none());
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == left).unwrap().2, cut, "undo restores the cut in one step");
}

#[test]
fn pressing_l_again_doubles_speed_and_j_reverses() {
    let mut s = demo();
    let (left, cut) = select_first_cut(&mut s, "roll");
    let rate = s.sequence_rate();
    let r = s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.0})).unwrap();
    assert_eq!(r["speed"], json!(1.0));
    let r = s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.5})).unwrap();
    assert_eq!(r["speed"], json!(2.0));
    // 0.5 s at 1× + 0.5 s at 2×
    let a = frames(&s, 0.5);
    s.execute("trim.tick", json!({"clock": 1.0})).unwrap();
    let expect = a + frames(&s, 1.0);
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == left).unwrap().2, cut + rate.tick_of(expect));
    // J: reverse at 1×
    let r = s.execute("trim.shuttle", json!({"direction": "reverse", "clock": 1.0})).unwrap();
    assert_eq!(r["speed"], json!(-1.0));
    s.execute("trim.tick", json!({"clock": 1.25})).unwrap();
    let back = expect - frames(&s, 0.25);
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == left).unwrap().2, cut + rate.tick_of(back));
    // slow (Shift+J / Shift+L) = quarter speed
    let r = s.execute("trim.shuttle", json!({"direction": "forward", "slow": true, "clock": 1.25})).unwrap();
    assert_eq!(r["speed"], json!(0.25));
    let r = s.execute("trim.shuttleStop", json!({"clock": 2.25})).unwrap();
    assert_eq!(r["offsetFrames"], json!(back + frames(&s, 0.25)));
    assert_eq!(s.history.undo.last().unwrap().0, "Dynamic Rolling Edit");
}

#[test]
fn dynamic_ripple_backward_shortens_and_stops_at_a_limit() {
    let mut s = demo();
    let (clip, end0) = select_first_cut(&mut s, "ripple");
    let rate = s.sequence_rate();
    let start0 = v1(&s)[0].1;
    let v1_end = |s: &Session| v1(s).last().unwrap().2;
    let dur0 = v1_end(&s);
    s.execute("trim.shuttle", json!({"direction": "reverse", "clock": 0.0})).unwrap();
    s.execute("trim.tick", json!({"clock": 0.25})).unwrap();
    let n = frames(&s, 0.25);
    assert_eq!(v1(&s)[0].2, end0 - rate.tick_of(n));
    assert_eq!(v1_end(&s), dur0 - rate.tick_of(n), "ripple pulls later material in");
    // keep going for a minute: the clip can't get shorter than one frame
    s.execute("trim.tick", json!({"clock": 600.0})).unwrap();
    let info = s.execute("trim.monitor", json!({})).unwrap();
    assert_eq!(info["dynamic"]["atLimit"], json!(true), "{info}");
    assert_eq!(info["dynamic"]["speed"], json!(0.0));
    let c = v1(&s).into_iter().find(|c| c.0 == clip).unwrap();
    assert_eq!(c.2 - c.1, rate.frame_duration(), "held at the one-frame limit");
    assert_eq!(c.1, start0);
    let r = s.execute("trim.shuttleStop", json!({})).unwrap();
    assert_eq!(r["label"], json!("Dynamic Ripple Trim"));
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(v1_end(&s), dur0);
}

#[test]
fn another_command_commits_first_and_cancel_restores() {
    let mut s = demo();
    let (left, cut) = select_first_cut(&mut s, "roll");
    let undo0 = s.history.undo.len();
    s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.0})).unwrap();
    s.execute("trim.tick", json!({"clock": 0.5})).unwrap();
    // Undo mid-trim: the dynamic trim is committed, then undone as a single step
    s.execute("edit.undo", json!({})).unwrap();
    assert!(s.trim_play.dynamic.is_none());
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == left).unwrap().2, cut);
    assert_eq!(s.history.undo.len(), undo0);
    assert_eq!(s.history.redo.last().unwrap().0, "Dynamic Rolling Edit");
    // Esc cancels without an undo step (and restores the counters)
    let shift0 = s.execute("trim.monitor", json!({})).unwrap()["outShift"].clone();
    s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.0})).unwrap();
    s.execute("trim.tick", json!({"clock": 0.5})).unwrap();
    assert_ne!(v1(&s).into_iter().find(|c| c.0 == left).unwrap().2, cut);
    s.execute("trim.cancelDynamic", json!({})).unwrap();
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == left).unwrap().2, cut);
    assert_eq!(s.history.undo.len(), undo0);
    assert_eq!(s.execute("trim.monitor", json!({})).unwrap()["outShift"], shift0);
    // K with nothing moved commits nothing
    s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.0})).unwrap();
    let r = s.execute("trim.shuttleStop", json!({"clock": 0.0})).unwrap();
    assert_eq!(r["committed"], json!(false));
    assert_eq!(s.history.undo.len(), undo0);
}

#[test]
fn play_around_loops_preroll_to_postroll() {
    let mut s = demo();
    let (_, cut) = select_first_cut(&mut s, "roll");
    let rate = s.sequence_rate();
    s.execute("prefs.set", json!({"values": {"playback.prerollSeconds": 1.0, "playback.postrollSeconds": 0.5}})).unwrap();
    let r = s.execute("trim.playAround", json!({"clock": 100.0})).unwrap();
    let start = rate.snap(cut - Tick::from_seconds_f64(1.0));
    let end = rate.snap(cut + Tick::from_seconds_f64(0.5));
    assert_eq!(r["start"], json!(start.0));
    assert_eq!(r["end"], json!(end.0));
    assert_eq!(s.playhead(), start);
    s.execute("trim.tick", json!({"clock": 101.0})).unwrap();
    assert_eq!(s.playhead(), rate.snap(start + Tick::from_seconds_f64(1.0)));
    // loops: 1.5 s later we're back at the start + (elapsed mod length)
    let len = (end - start).seconds();
    let el = 1.75 % len;
    s.execute("trim.tick", json!({"clock": 101.75})).unwrap();
    assert_eq!(s.playhead(), rate.snap(start + Tick::from_seconds_f64(el)));
    // trim buttons keep the loop running; other commands stop it
    s.execute("trim.forward", json!({})).unwrap();
    assert!(s.trim_play.around.is_some());
    s.execute("markers.add", json!({})).unwrap();
    assert!(s.trim_play.around.is_none());
    // playhead mode
    s.execute("prefs.set", json!({"key": "trim.playheadDeterminesLoop", "value": true})).unwrap();
    s.execute("playhead.set", json!({"time": (cut + rate.tick_of(48)).0})).unwrap();
    let r = s.execute("trim.playAround", json!({"clock": 0.0, "loop": false})).unwrap();
    assert_eq!(r["start"], json!(rate.snap(cut + rate.tick_of(48) - Tick::from_seconds_f64(1.0)).0));
    s.execute("trim.tick", json!({"clock": 5.0})).unwrap();
    assert!(s.trim_play.around.is_none(), "one-shot play-around stops at the postroll");
}

#[test]
fn trim_monitor_sides_and_shift_counters() {
    let mut s = demo();
    let c = v1(&s);
    s.execute("trim.selectEditPoint", json!({"clip": c[1].0, "edge": "in", "kind": "ripple"})).unwrap();
    let info = s.execute("trim.monitor", json!({})).unwrap();
    assert_eq!(info["outgoing"]["clip"], json!(c[0].0), "{info}");
    assert_eq!(info["incoming"]["clip"], json!(c[1].0));
    assert_eq!(info["outgoing"]["time"], json!((c[0].2 - s.sequence_rate().frame_duration()).0), "outgoing shows its last frame");
    assert_eq!(info["incoming"]["time"], json!(c[1].1));
    assert_eq!(info["edge"], json!("in"));
    s.execute("prefs.set", json!({"key": "trim.largeTrimOffset", "value": 3})).unwrap();
    s.execute("trim.forwardMany", json!({})).unwrap();
    s.execute("trim.forward", json!({})).unwrap();
    let info = s.execute("trim.monitor", json!({})).unwrap();
    assert_eq!(info["inShift"], json!(4), "large offset 3 + 1: {info}");
    assert_eq!(info["outShift"], json!(0), "a ripple In trim leaves the outgoing side alone");
    // a new selection starts new counters
    s.execute("trim.selectEditPoint", json!({"clip": c[1].0, "edge": "in", "kind": "roll"})).unwrap();
    assert_eq!(s.execute("trim.monitor", json!({})).unwrap()["inShift"], json!(0));
    // Apply Default Transitions to Selection at the edit point
    let r = s.execute("trim.applyDefaultTransition", json!({})).unwrap();
    assert_eq!(r["transitions"].as_array().unwrap().len(), 1, "{r}");
    let seq = s.execute("sequence.inspect", json!({})).unwrap();
    assert!(!seq["video"][0]["transitions"].as_array().unwrap().is_empty());
}

#[test]
fn extend_to_playhead_moves_each_unequal_out_point_to_the_playhead() {
    let mut s = demo();
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let rate = s.sequence_rate();
    let q = s.active_sequence().unwrap();
    // a clip on V1 and the overlay on V2 that overlap in time but end at different frames
    let mut found = None;
    for a in &q.video_tracks[0].items {
        for b in &q.video_tracks[1].items {
            let (lo, hi) = (a.start.max(b.start), a.end().min(b.end()));
            if found.is_none() && a.end() != b.end() && hi - lo > rate.tick_of(2) {
                found = Some((a.id, b.id, lo + rate.tick_of(1)));
            }
        }
    }
    let (a, b, ph) = found.expect("demo has overlapping clips on V1 and V2 with different ends");
    s.execute("playhead.set", json!({"time": ph.0})).unwrap();
    s.execute("trim.selectEditPoint", json!({"clip": a.0, "edge": "out", "kind": "trim"})).unwrap();
    s.execute("trim.selectEditPoint", json!({"clip": b.0, "edge": "out", "kind": "trim", "add": true})).unwrap();
    s.execute("trim.extendToPlayhead", json!({})).unwrap();
    let q = s.active_sequence().unwrap();
    assert_eq!(q.find_item(a).unwrap().1.end(), ph);
    assert_eq!(q.find_item(b).unwrap().1.end(), ph);
}

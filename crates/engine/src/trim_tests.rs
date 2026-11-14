//! Trim Monitor + dynamic (J/K/L) trimming, driven by an exact simulated clock.

use super::*;
use serde_json::json;

/// Two adjacent linked A/V clips with handles on both sides of their cut.
fn linked_cut() -> (Session, [(ClipId, ClipId); 2]) {
    let mut s = demo();
    s.execute("file.newSequence", json!({"name": "Linked trim", "video": 1, "audio": 1})).unwrap();
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).unwrap().id;
    for n in 0..2 {
        s.execute("timeline.place", json!({"item": item.0, "frame": 48 * n, "sourceIn": rate.tick_of(48).0, "duration": rate.tick_of(48).0})).unwrap();
    }
    let q = s.active_sequence().unwrap();
    let clips = [0, 1].map(|n| (q.video_tracks[0].items[n].id, q.audio_tracks[0].items[n].id));
    (s, clips)
}

/// #3085: selecting either side of a picture cut also selects its linked sound edge.
#[test]
fn linked_edit_point_selection_and_shift_toggle() {
    for kind in ["trim", "ripple", "roll"] {
        for edge in ["in", "out"] {
            let (mut s, clips) = linked_cut();
            let (video, audio) = clips[usize::from(edge == "in")];
            let p = json!({"clip": video.0, "edge": edge, "kind": kind});
            s.execute("trim.selectEditPoint", p.clone()).unwrap();
            assert_eq!(s.state.edit_points.iter().map(|e| e.clip).collect::<Vec<_>>(), vec![video, audio], "{kind} {edge}");
            s.execute("trim.selectEditPoint", json!({"clip": audio.0, "edge": edge, "kind": kind, "add": true})).unwrap();
            assert!(s.state.edit_points.is_empty(), "Shift-click removes the linked group");
            s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
            s.execute("trim.selectEditPoint", p).unwrap();
            assert_eq!(s.state.edit_points.len(), 1);
        }
    }
}

/// #3085: Trim Monitor rolls picture and sound once, with one undo/redo step.
#[test]
fn trim_mode_roll_moves_linked_audio_once() {
    for edge in ["in", "out"] {
        let (mut s, clips) = linked_cut();
        let selected = clips[usize::from(edge == "in")].0;
        s.execute("trim.selectEditPoint", json!({"clip": selected.0, "edge": edge, "kind": "roll"})).unwrap();
        let before = s.project.clone();
        let undo = s.history.undo.len();
        s.execute("trim.nudge", json!({"frames": 3})).unwrap();
        let q = s.active_sequence().unwrap();
        for clip in [clips[0].0, clips[0].1] {
            assert_eq!(q.find_item(clip).unwrap().1.end(), s.sequence_rate().tick_of(51));
        }
        for clip in [clips[1].0, clips[1].1] {
            assert_eq!(q.find_item(clip).unwrap().1.start, s.sequence_rate().tick_of(51));
        }
        assert_eq!(s.history.undo.len(), undo + 1);
        let after = s.project.clone();
        s.execute("edit.undo", json!({})).unwrap();
        assert_eq!(*s.project, *before);
        s.execute("edit.redo", json!({})).unwrap();
        assert_eq!(*s.project, *after);
    }
}

/// Explicitly selected partners must not run the same linked trim twice, including ripple shifts.
#[test]
fn selected_linked_regular_and_ripple_edges_move_once() {
    for kind in ["trim", "ripple"] {
        for out in [false, true] {
            let (mut s, clips) = linked_cut();
            let pair = clips[usize::from(!out)];
            s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
            for (n, clip) in [pair.0, pair.1].into_iter().enumerate() {
                s.execute("trim.selectEditPoint", json!({"clip": clip.0, "edge": if out { "out" } else { "in" }, "kind": kind, "add": n != 0})).unwrap();
            }
            s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
            let before = s.project.clone();
            s.execute("trim.nudge", json!({"frames": if out { -3 } else { 3 }})).unwrap();
            let q = s.active_sequence().unwrap();
            let f = |n| s.sequence_rate().tick_of(n);
            for clip in [pair.0, pair.1] {
                let it = q.find_item(clip).unwrap().1;
                let expected = if out {
                    (0, 45)
                } else if kind == "ripple" {
                    (48, 93)
                } else {
                    (51, 96)
                };
                assert_eq!((it.start, it.end()), (f(expected.0), f(expected.1)), "{kind} out={out}");
            }
            if out {
                for clip in [clips[1].0, clips[1].1] {
                    assert_eq!(q.find_item(clip).unwrap().1.start, f(if kind == "ripple" { 45 } else { 48 }), "{kind}");
                }
            }
            let after = s.project.clone();
            s.execute("edit.undo", json!({})).unwrap();
            assert_eq!(*s.project, *before);
            s.execute("edit.redo", json!({})).unwrap();
            assert_eq!(*s.project, *after);
        }
    }
}

/// Nearest-edge selection follows links even when only the video track is targeted, without
/// duplicating the sound when it is targeted too. Dynamic trimming uses the same grouped edit.
#[test]
fn nearest_linked_roll_and_dynamic_trim() {
    let (mut s, clips) = linked_cut();
    let rate = s.sequence_rate();
    s.execute("timeline.setTargeting", json!({"track": "A1", "targeted": false})).unwrap();
    s.execute("playhead.set", json!({"frame": 48})).unwrap();
    s.execute("trim.selectNearest", json!({"kind": "roll"})).unwrap();
    assert_eq!(s.state.edit_points.len(), 2);
    assert!(s.state.edit_points.iter().any(|e| e.clip == clips[0].1));
    s.execute("timeline.setTargeting", json!({"track": "A1", "targeted": true})).unwrap();
    s.execute("trim.selectNearest", json!({"kind": "roll"})).unwrap();
    assert_eq!(s.state.edit_points.len(), 2);
    let before = s.project.clone();
    s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.0})).unwrap();
    s.execute("trim.shuttleStop", json!({"clock": 0.25})).unwrap();
    let expected = rate.tick_of(48 + frames(&s, 0.25));
    for clip in [clips[0].0, clips[0].1] {
        assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.end(), expected);
    }
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(*s.project, *before);
}

/// The shorter audio handle limits the picture too; separately applying the two cuts desyncs them.
#[test]
fn linked_trim_roll_uses_the_shortest_handle() {
    let (mut s, clips) = linked_cut();
    let rate = s.sequence_rate();
    s.edit_sequence("Short audio handle", |q, _, _| {
        q.find_item_mut(clips[1].1).unwrap().1.source_in = rate.tick_of(2);
        Ok(())
    })
    .unwrap();
    s.execute("trim.selectEditPoint", json!({"clip": clips[0].0.0, "edge": "out", "kind": "roll"})).unwrap();
    s.execute("trim.nudge", json!({"frames": -10})).unwrap();
    for clip in [clips[0].0, clips[0].1] {
        assert_eq!(s.active_sequence().unwrap().find_item(clip).unwrap().1.end(), rate.tick_of(46));
    }
}

/// Locked partners are neither selected nor changed; directly trimming a locked primary fails.
#[test]
fn linked_trim_respects_locked_tracks() {
    for kind in ["trim", "ripple", "roll"] {
        let (mut s, clips) = linked_cut();
        s.execute("timeline.setTrack", json!({"track": "A1", "locked": true})).unwrap();
        s.execute("trim.selectEditPoint", json!({"clip": clips[0].0.0, "edge": "out", "kind": kind})).unwrap();
        assert_eq!(s.state.edit_points.len(), 1);
        let audio = s.active_sequence().unwrap().audio_tracks[0].clone();
        s.execute("trim.nudge", json!({"frames": -3})).unwrap();
        assert_eq!(s.active_sequence().unwrap().audio_tracks[0], audio);
        s.execute("trim.selectEditPoint", json!({"clip": clips[0].1.0, "edge": "out", "kind": kind})).unwrap();
        let before = s.project.clone();
        assert!(s.execute("trim.nudge", json!({"frames": -3})).is_err());
        assert_eq!(*s.project, *before);
    }
}

/// A split audio cut stays separate from a picture roll; linked selection off also edits one cut.
#[test]
fn split_or_disabled_linked_roll_leaves_audio_alone() {
    for split in [false, true] {
        let (mut s, clips) = linked_cut();
        s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
        if split {
            s.execute("timeline.roll", json!({"left": clips[0].1.0, "right": clips[1].1.0, "deltaFrames": -4})).unwrap();
            s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
        }
        let audio = s.active_sequence().unwrap().audio_tracks[0].clone();
        s.execute("trim.selectEditPoint", json!({"clip": clips[0].0.0, "edge": "out", "kind": "roll"})).unwrap();
        assert_eq!(s.state.edit_points.len(), 1);
        s.execute("trim.nudge", json!({"frames": 3})).unwrap();
        assert_eq!(s.active_sequence().unwrap().audio_tracks[0], audio);
        assert_eq!(s.active_sequence().unwrap().find_item(clips[0].0).unwrap().1.end(), s.sequence_rate().tick_of(51));
    }
}

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).expect("ok");
    for t in ["V2", "V3", "A2", "A3"] {
        let _ = s.execute("timeline.setTrack", json!({"track": t, "syncLock": false}));
    }
    s
}

/// V1 clips as (id, start, end).
fn v1(s: &Session) -> Vec<(u64, Tick, Tick)> {
    s.active_sequence().expect("ok").video_tracks[0].items.iter().map(|i| (i.id.0, i.start, i.end())).collect()
}

/// Whole frames covered by `secs` of real time at the sequence rate.
fn frames(s: &Session, secs: f64) -> i64 {
    s.sequence_rate().frame_at(Tick::from_seconds_f64(secs))
}

/// Select the cut between V1 clips 0 and 1 as `kind` (roll / ripple out of clip 0).
fn select_first_cut(s: &mut Session, kind: &str) -> (u64, Tick) {
    let c = v1(s);
    s.execute("trim.selectEditPoint", json!({"clip": c[0].0, "edge": "out", "kind": kind})).expect("ok");
    (c[0].0, c[0].2)
}

#[test]
fn dynamic_roll_forward_is_one_undo_step() {
    let mut s = demo();
    let (left, cut) = select_first_cut(&mut s, "roll");
    let rate = s.sequence_rate();
    let undo0 = s.history.undo.len();
    let journal0 = s.journal.len();
    let dur0 = s.active_sequence().expect("ok").duration();
    s.execute("trim.shuttle", json!({"direction": "forward", "clock": 10.0})).expect("ok");
    // live: the cut moves with the clock, no history yet
    s.execute("trim.tick", json!({"clock": 10.5})).expect("ok");
    let mid = v1(&s).into_iter().find(|c| c.0 == left).expect("ok");
    assert_eq!(mid.2, cut + rate.tick_of(frames(&s, 0.5)), "half a second of roll");
    assert_eq!(s.playhead(), mid.2, "the Program monitor follows the edit");
    assert_eq!(s.history.undo.len(), undo0, "no undo step while trimming");
    let info = s.execute("trim.monitor", json!({})).expect("ok");
    assert_eq!(info["dynamic"]["offsetFrames"], json!(frames(&s, 0.5)), "{info}");
    assert_eq!(info["outShift"], json!(frames(&s, 0.5)));
    assert_eq!(info["inShift"], json!(frames(&s, 0.5)), "roll moves both sides");
    // K commits
    let r = s.execute("trim.shuttleStop", json!({"clock": 11.0})).expect("ok");
    assert_eq!(r["committed"], json!(true), "{r}");
    assert_eq!(r["offsetFrames"], json!(frames(&s, 1.0)));
    let after = v1(&s).into_iter().find(|c| c.0 == left).expect("ok");
    assert_eq!(after.2, cut + rate.tick_of(frames(&s, 1.0)));
    assert_eq!(s.active_sequence().expect("ok").duration(), dur0, "roll keeps the duration");
    assert_eq!(s.history.undo.len(), undo0 + 1, "exactly one undo step");
    assert_eq!(s.history.undo.last().expect("ok").0, "Dynamic Rolling Edit");
    assert!(
        s.journal[journal0..].iter().all(|(id, _)| id.starts_with("trim.")),
        "live updates are not journaled as separate edits: {:?}",
        &s.journal[journal0..]
    );
    assert!(s.trim_play.dynamic.is_none());
    s.execute("edit.undo", json!({})).expect("ok");
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == left).expect("ok").2, cut, "undo restores the cut in one step");
}

#[test]
fn pressing_l_again_doubles_speed_and_j_reverses() {
    let mut s = demo();
    let (left, cut) = select_first_cut(&mut s, "roll");
    let rate = s.sequence_rate();
    let r = s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.0})).expect("ok");
    assert_eq!(r["speed"], json!(1.0));
    let r = s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.5})).expect("ok");
    assert_eq!(r["speed"], json!(2.0));
    // 0.5 s at 1× + 0.5 s at 2×
    let a = frames(&s, 0.5);
    s.execute("trim.tick", json!({"clock": 1.0})).expect("ok");
    let expect = a + frames(&s, 1.0);
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == left).expect("ok").2, cut + rate.tick_of(expect));
    // J: reverse at 1×
    let r = s.execute("trim.shuttle", json!({"direction": "reverse", "clock": 1.0})).expect("ok");
    assert_eq!(r["speed"], json!(-1.0));
    s.execute("trim.tick", json!({"clock": 1.25})).expect("ok");
    let back = expect - frames(&s, 0.25);
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == left).expect("ok").2, cut + rate.tick_of(back));
    // slow (Shift+J / Shift+L) = quarter speed
    let r = s.execute("trim.shuttle", json!({"direction": "forward", "slow": true, "clock": 1.25})).expect("ok");
    assert_eq!(r["speed"], json!(0.25));
    let r = s.execute("trim.shuttleStop", json!({"clock": 2.25})).expect("ok");
    assert_eq!(r["offsetFrames"], json!(back + frames(&s, 0.25)));
    assert_eq!(s.history.undo.last().expect("ok").0, "Dynamic Rolling Edit");
}

#[test]
fn dynamic_ripple_backward_shortens_and_stops_at_a_limit() {
    let mut s = demo();
    let (clip, end0) = select_first_cut(&mut s, "ripple");
    let rate = s.sequence_rate();
    let start0 = v1(&s)[0].1;
    let v1_end = |s: &Session| v1(s).last().expect("ok").2;
    let dur0 = v1_end(&s);
    s.execute("trim.shuttle", json!({"direction": "reverse", "clock": 0.0})).expect("ok");
    s.execute("trim.tick", json!({"clock": 0.25})).expect("ok");
    let n = frames(&s, 0.25);
    assert_eq!(v1(&s)[0].2, end0 - rate.tick_of(n));
    assert_eq!(v1_end(&s), dur0 - rate.tick_of(n), "ripple pulls later material in");
    // keep going for a minute: the clip can't get shorter than one frame
    s.execute("trim.tick", json!({"clock": 600.0})).expect("ok");
    let info = s.execute("trim.monitor", json!({})).expect("ok");
    assert_eq!(info["dynamic"]["atLimit"], json!(true), "{info}");
    assert_eq!(info["dynamic"]["speed"], json!(0.0));
    let c = v1(&s).into_iter().find(|c| c.0 == clip).expect("ok");
    assert_eq!(c.2 - c.1, rate.frame_duration(), "held at the one-frame limit");
    assert_eq!(c.1, start0);
    let r = s.execute("trim.shuttleStop", json!({})).expect("ok");
    assert_eq!(r["label"], json!("Dynamic Ripple Trim"));
    s.execute("edit.undo", json!({})).expect("ok");
    assert_eq!(v1_end(&s), dur0);
}

/// Each trim's undo step records its trim kind's id, which gives its History icon: from a Trim
/// Monitor nudge, a dynamic trim and a direct `timeline.trim`.
#[test]
fn trim_steps_record_their_trim_kind() {
    let last = |s: &Session| s.history.undo.last().and_then(|h| h.2.clone());
    for (kind, origin) in [("ripple", crate::trim::RIPPLE_TRIM_ORIGIN), ("roll", "timeline.roll"), ("trim", "timeline.trim")] {
        let mut s = demo();
        select_first_cut(&mut s, kind);
        s.execute("trim.nudge", json!({"frames": -2})).unwrap();
        assert_eq!(last(&s).as_deref(), Some(origin), "{kind} nudge");
        s.execute("trim.shuttle", json!({"direction": "reverse", "clock": 0.0})).unwrap();
        s.execute("trim.tick", json!({"clock": 0.25})).unwrap();
        assert_eq!(s.execute("trim.shuttleStop", json!({})).unwrap()["committed"], json!(true));
        assert_eq!(last(&s).as_deref(), Some(origin), "{kind} dynamic trim");
    }
    let mut s = demo();
    let clip = v1(&s)[0].0;
    s.execute("timeline.trim", json!({"clip": clip, "edge": "out", "mode": "ripple", "deltaFrames": -2})).unwrap();
    assert_eq!(last(&s).as_deref(), Some(crate::trim::RIPPLE_TRIM_ORIGIN), "a ripple drag");
    assert_eq!(s.command_origin, None, "the ripple id is not left behind");
    s.execute("timeline.trim", json!({"clip": clip, "edge": "out", "mode": "regular", "deltaFrames": -2})).unwrap();
    assert_eq!(last(&s).as_deref(), Some("timeline.trim"), "a regular drag");
}

#[test]
fn another_command_commits_first_and_cancel_restores() {
    let mut s = demo();
    let (left, cut) = select_first_cut(&mut s, "roll");
    let undo0 = s.history.undo.len();
    s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.0})).expect("ok");
    s.execute("trim.tick", json!({"clock": 0.5})).expect("ok");
    // Undo mid-trim: the dynamic trim is committed, then undone as a single step
    s.execute("edit.undo", json!({})).expect("ok");
    assert!(s.trim_play.dynamic.is_none());
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == left).expect("ok").2, cut);
    assert_eq!(s.history.undo.len(), undo0);
    assert_eq!(s.history.redo.last().expect("ok").0, "Dynamic Rolling Edit");
    // Esc cancels without an undo step (and restores the counters)
    let shift0 = s.execute("trim.monitor", json!({})).expect("ok")["outShift"].clone();
    s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.0})).expect("ok");
    s.execute("trim.tick", json!({"clock": 0.5})).expect("ok");
    assert_ne!(v1(&s).into_iter().find(|c| c.0 == left).expect("ok").2, cut);
    s.execute("trim.cancelDynamic", json!({})).expect("ok");
    assert_eq!(v1(&s).into_iter().find(|c| c.0 == left).expect("ok").2, cut);
    assert_eq!(s.history.undo.len(), undo0);
    assert_eq!(s.execute("trim.monitor", json!({})).expect("ok")["outShift"], shift0);
    // K with nothing moved commits nothing
    s.execute("trim.shuttle", json!({"direction": "forward", "clock": 0.0})).expect("ok");
    let r = s.execute("trim.shuttleStop", json!({"clock": 0.0})).expect("ok");
    assert_eq!(r["committed"], json!(false));
    assert_eq!(s.history.undo.len(), undo0);
}

#[test]
fn play_around_loops_preroll_to_postroll() {
    let mut s = demo();
    let (_, cut) = select_first_cut(&mut s, "roll");
    let rate = s.sequence_rate();
    s.execute("prefs.set", json!({"values": {"playback.prerollSeconds": 1.0, "playback.postrollSeconds": 0.5}})).expect("ok");
    let r = s.execute("trim.playAround", json!({"clock": 100.0})).expect("ok");
    let start = rate.snap(cut - Tick::from_seconds_f64(1.0));
    let end = rate.snap(cut + Tick::from_seconds_f64(0.5));
    assert_eq!(r["start"], json!(start.0));
    assert_eq!(r["end"], json!(end.0));
    assert_eq!(s.playhead(), start);
    s.execute("trim.tick", json!({"clock": 101.0})).expect("ok");
    assert_eq!(s.playhead(), rate.snap(start + Tick::from_seconds_f64(1.0)));
    // loops: 1.5 s later we're back at the start + (elapsed mod length)
    let len = (end - start).seconds();
    let el = 1.75 % len;
    s.execute("trim.tick", json!({"clock": 101.75})).expect("ok");
    assert_eq!(s.playhead(), rate.snap(start + Tick::from_seconds_f64(el)));
    // trim buttons keep the loop running; other commands stop it
    s.execute("trim.forward", json!({})).expect("ok");
    assert!(s.trim_play.around.is_some());
    s.execute("markers.add", json!({})).expect("ok");
    assert!(s.trim_play.around.is_none());
    // playhead mode
    s.execute("prefs.set", json!({"key": "trim.playheadDeterminesLoop", "value": true})).expect("ok");
    s.execute("playhead.set", json!({"time": (cut + rate.tick_of(48)).0})).expect("ok");
    let r = s.execute("trim.playAround", json!({"clock": 0.0, "loop": false})).expect("ok");
    assert_eq!(r["start"], json!(rate.snap(cut + rate.tick_of(48) - Tick::from_seconds_f64(1.0)).0));
    s.execute("trim.tick", json!({"clock": 5.0})).expect("ok");
    assert!(s.trim_play.around.is_none(), "one-shot play-around stops at the postroll");
}

#[test]
fn trim_monitor_sides_and_shift_counters() {
    let mut s = demo();
    let c = v1(&s);
    s.execute("trim.selectEditPoint", json!({"clip": c[1].0, "edge": "in", "kind": "ripple"})).expect("ok");
    let info = s.execute("trim.monitor", json!({})).expect("ok");
    assert_eq!(info["outgoing"]["clip"], json!(c[0].0), "{info}");
    assert_eq!(info["incoming"]["clip"], json!(c[1].0));
    assert_eq!(info["outgoing"]["time"], json!((c[0].2 - s.sequence_rate().frame_duration()).0), "outgoing shows its last frame");
    assert_eq!(info["incoming"]["time"], json!(c[1].1));
    assert_eq!(info["edge"], json!("in"));
    s.execute("prefs.set", json!({"key": "trim.largeTrimOffset", "value": 3})).expect("ok");
    s.execute("trim.forwardMany", json!({})).expect("ok");
    s.execute("trim.forward", json!({})).expect("ok");
    let info = s.execute("trim.monitor", json!({})).expect("ok");
    assert_eq!(info["inShift"], json!(4), "large offset 3 + 1: {info}");
    assert_eq!(info["outShift"], json!(0), "a ripple In trim leaves the outgoing side alone");
    // a new selection starts new counters
    s.execute("trim.selectEditPoint", json!({"clip": c[1].0, "edge": "in", "kind": "roll"})).expect("ok");
    assert_eq!(s.execute("trim.monitor", json!({})).expect("ok")["inShift"], json!(0));
    // Apply Default Transitions to Selection at the edit point
    let r = s.execute("trim.applyDefaultTransition", json!({})).unwrap();
    assert_eq!(r["transitions"].as_array().unwrap().len(), 2, "picture and linked sound: {r}");
    let seq = s.execute("sequence.inspect", json!({})).unwrap();
    assert!(!seq["video"][0]["transitions"].as_array().unwrap().is_empty());
    assert!(!seq["audio"][0]["transitions"].as_array().unwrap().is_empty());
}

#[test]
fn extend_to_playhead_moves_each_unequal_out_point_to_the_playhead() {
    let mut s = demo();
    s.execute("sequence.linkedSelection", json!({"on": false})).expect("ok");
    let rate = s.sequence_rate();
    let q = s.active_sequence().expect("ok");
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
    s.execute("playhead.set", json!({"time": ph.0})).expect("ok");
    s.execute("trim.selectEditPoint", json!({"clip": a.0, "edge": "out", "kind": "trim"})).expect("ok");
    s.execute("trim.selectEditPoint", json!({"clip": b.0, "edge": "out", "kind": "trim", "add": true})).expect("ok");
    s.execute("trim.extendToPlayhead", json!({})).expect("ok");
    let q = s.active_sequence().expect("ok");
    assert_eq!(q.find_item(a).expect("ok").1.end(), ph);
    assert_eq!(q.find_item(b).expect("ok").1.end(), ph);
}

/// #914: Trim by Frames on two selected edit points is one undo (and redo) step.
#[test]
fn multipoint_trim_by_frames_is_one_undo_step() {
    let mut s = demo();
    s.execute("sequence.linkedSelection", json!({"on": false})).expect("ok");
    let c = v1(&s);
    let (a, b) = (c[0], c[1]);
    s.execute("trim.selectEditPoint", json!({"clip": a.0, "edge": "out", "kind": "trim"})).expect("ok");
    s.execute("trim.selectEditPoint", json!({"clip": b.0, "edge": "out", "kind": "trim", "add": true})).expect("ok");
    let undo0 = s.history.undo.len();
    let d = s.sequence_rate().tick_of(-1);
    s.execute("trim.nudge", json!({"frames": -1})).expect("ok");
    let q = s.active_sequence().expect("ok");
    let a1 = q.find_item(ClipId(a.0)).expect("ok").1.end();
    let b1 = q.find_item(ClipId(b.0)).expect("ok").1.end();
    assert_eq!(a1, a.2 + d);
    assert_eq!(b1, b.2 + d);
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step for both clips");
    s.execute("edit.undo", json!({})).expect("ok");
    let q = s.active_sequence().expect("ok");
    assert_eq!(q.find_item(ClipId(a.0)).expect("ok").1.end(), a.2);
    assert_eq!(q.find_item(ClipId(b.0)).expect("ok").1.end(), b.2);
    s.execute("edit.redo", json!({})).expect("ok");
    let q = s.active_sequence().expect("ok");
    assert_eq!(q.find_item(ClipId(a.0)).expect("ok").1.end(), a1);
    assert_eq!(q.find_item(ClipId(b.0)).expect("ok").1.end(), b1);
}

/// #653: the delta the Timeline previews while an edge is dragged is the one `timeline.trim`
/// applies on release: dragging the last clip's Out edge far past the end of its media stops at
/// the media's end instead of following the pointer.
#[test]
fn trim_delta_stops_at_the_end_of_the_media() {
    let mut s = demo();
    let last = *v1(&s).last().expect("ok");
    let clip = ClipId(last.0);
    let far = Tick(3600 * filmcraft_time::TICKS_PER_SECOND);
    let shown = commands::trim_delta(&s, clip, filmcraft_edit::Edge::Out, filmcraft_edit::TrimMode::Regular, far).expect("ok");
    assert!(Tick::ZERO < shown && shown < far, "clamped to the media left after the clip: {shown:?}");
    let r = s.execute("timeline.trim", json!({"clip": last.0, "edge": "out", "mode": "regular", "delta": far.0})).expect("ok");
    assert_eq!(r["delta"], json!(shown.0), "the preview and the trim agree");
    let small = s.sequence_rate().frame_duration();
    let back = commands::trim_delta(&s, clip, filmcraft_edit::Edge::Out, filmcraft_edit::TrimMode::Regular, -small).expect("ok");
    assert_eq!(back, -small, "a trim within the media is not clamped");
}

/// Two clips on their own video track, 48 frames each with a 48-frame gap between them: the
/// fixture for trims that act on a whole selection.
fn spaced_clips() -> (Session, ClipId, ClipId) {
    let mut s = demo();
    s.execute("file.newSequence", json!({"name": "Group trim", "video": 1, "audio": 0})).expect("ok");
    let rate = s.sequence_rate();
    let item = s.project.items.values().find(|i| i.has_video() && i.as_media().is_some()).expect("a video item").id;
    for n in 0..2 {
        s.execute("timeline.place", json!({"item": item.0, "frame": 96 * n, "duration": rate.tick_of(48).0})).expect("ok");
    }
    let ids: Vec<ClipId> = v1(&s).iter().map(|c| ClipId(c.0)).collect();
    (s, ids[0], ids[1])
}

/// The Out-edge handle each clip has left, as `timeline.trim` would apply it on its own.
fn out_handle(s: &Session, clip: ClipId, far: Tick) -> Tick {
    commands::trim_delta(s, clip, filmcraft_edit::Edge::Out, filmcraft_edit::TrimMode::Regular, far).expect("ok")
}

/// A marquee selection trims as one group: dragging one edge moves every selected clip's Out edge
/// by the same delta, in one edit step, so a single undo puts the whole group back.
#[test]
fn a_selection_trims_together_as_one_undo_step() {
    let (mut s, a, b) = spaced_clips();
    let far = s.sequence_rate().tick_of(240);
    let want = out_handle(&s, a, far).min(out_handle(&s, b, far));
    assert!(want > Tick::ZERO, "the fixture has an Out handle on both clips");
    let clip = |s: &Session, c: ClipId| s.active_sequence().expect("ok").find_item(c).expect("ok").1.clone();
    let (before_a, before_b) = (clip(&s, a), clip(&s, b));
    let undo0 = s.history.undo.len();
    let shown = commands::trim_delta_group(&s, &[a, b], filmcraft_edit::Edge::Out, filmcraft_edit::TrimMode::Regular, want, false).expect("ok");
    let r = s.execute("timeline.trim", json!({"clips": [a.0, b.0], "edge": "out", "mode": "regular", "delta": want.0})).expect("ok");
    assert_eq!(Tick(r["delta"].as_i64().expect("ok")), shown, "the preview and the trim agree");
    assert_eq!(shown, want, "the group moves by the delta both clips can take");
    for (id, before) in [(a, &before_a), (b, &before_b)] {
        let now = clip(&s, id);
        assert_eq!(now.duration, before.duration + want, "every selected clip grows by the same delta");
        assert_eq!(now.start, before.start, "an Out trim leaves the start where it was");
    }
    assert_eq!(s.history.undo.len(), undo0 + 1, "one undo step for the whole group");
    s.execute("edit.undo", json!({})).expect("ok");
    assert_eq!(clip(&s, a).duration, before_a.duration);
    assert_eq!(clip(&s, b).duration, before_b.duration);
    s.execute("edit.redo", json!({})).expect("ok");
    assert_eq!(clip(&s, a).duration, before_a.duration + want);
    assert_eq!(clip(&s, b).duration, before_b.duration + want);
}

/// The group stops where the first clip has to: a delta past one clip's handle is clamped for the
/// whole group, so no clip overlaps its neighbour and none runs past its media.
#[test]
fn a_group_trim_stops_where_the_first_clip_has_to() {
    let (mut s, a, b) = spaced_clips();
    let far = s.sequence_rate().tick_of(240);
    let want = out_handle(&s, a, far).min(out_handle(&s, b, far));
    let r = s.execute("timeline.trim", json!({"clips": [a.0, b.0], "edge": "out", "mode": "regular", "delta": far.0})).expect("ok");
    assert_eq!(Tick(r["delta"].as_i64().expect("ok")), want, "clamped for the whole group, not per clip");
    let q = s.active_sequence().expect("ok");
    let (_, ca) = q.find_item(a).expect("ok");
    let (_, cb) = q.find_item(b).expect("ok");
    assert!(ca.end() <= cb.start, "the trimmed neighbour does not overlap the next clip");
}

/// A ripple moves the rest of the track, so it still trims one clip (and its links) at a time.
#[test]
fn a_ripple_trim_still_takes_one_clip() {
    let (mut s, a, b) = spaced_clips();
    let e = s.execute("timeline.trim", json!({"clips": [a.0, b.0], "edge": "out", "mode": "ripple", "deltaFrames": 2})).expect_err("refused");
    assert!(e.to_string().contains("ripple trim"), "{e}");
    s.execute("timeline.trim", json!({"clip": a.0, "edge": "out", "mode": "ripple", "deltaFrames": 2})).expect("one clip still ripples");
}

/// A malformed group never silently trims something else: an id of no clip in the active sequence
/// is a bad parameter (#65's rule for explicit targets).
#[test]
fn a_group_trim_refuses_clip_ids_it_does_not_know() {
    let (mut s, a, _) = spaced_clips();
    let e = s.execute("timeline.trim", json!({"clips": [a.0, 999_999], "edge": "out", "mode": "regular", "deltaFrames": 2})).expect_err("refused");
    assert!(e.to_string().contains("999999"), "{e}");
}

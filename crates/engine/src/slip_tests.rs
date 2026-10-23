//! The Slip Monitor's preview (`timeline.slipPreview`, [`crate::slip::preview`]): it shows what
//! `timeline.slip` with the same delta commits, frame for frame, never edits the project, and keeps
//! linked picture and sound in sync at unequal media limits (#529).

use filmcraft_project::TrackItem;
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::Session;

fn clip(s: &Session, id: u64) -> TrackItem {
    s.active_sequence().unwrap().find_item(filmcraft_project::ClipId(id)).unwrap().1.clone()
}

/// A 25 fps sequence with `fps` media of `seconds`; returns the session and the media item.
fn session(fps: u32, seconds: u32) -> (Session, u64) {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Slip"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "video": 1, "audio": 1, "width": 16, "height": 16})).unwrap();
    let r = s.execute("file.newOfflineFile", json!({"name": "Media", "seconds": seconds, "fps": fps, "video": true, "audio": true})).unwrap();
    (s, r["item"].as_u64().unwrap())
}

fn frame(s: &Session) -> i64 {
    s.sequence_rate().tick_of(1).0
}

/// The #529 split edit: a linked pair at source In 20 frames, 60 long, the picture lengthened by
/// 10 frames on its own, so it has 10 frames of tail handle and the sound 20 (4 s media).
fn split_edit() -> (Session, u64, u64) {
    let (mut s, item) = session(25, 4);
    let fr = frame(&s);
    let r = s.execute("timeline.place", json!({"item": item, "track": "V1", "audioTrack": "A1", "time": 0, "sourceIn": fr * 20, "duration": fr * 60})).unwrap();
    let (v, a) = (r["clips"][0].as_u64().unwrap(), r["clips"][1].as_u64().unwrap());
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    s.execute("timeline.trim", json!({"clip": v, "edge": "out", "deltaFrames": 10})).unwrap();
    s.execute("sequence.linkedSelection", json!({"on": true})).unwrap();
    (s, v, a)
}

fn preview(s: &mut Session, clip: u64, frames: i64) -> Value {
    s.execute("timeline.slipPreview", json!({"clip": clip, "deltaFrames": frames})).unwrap()
}

fn source_ins(v: &Value) -> Vec<(u64, i64)> {
    let mut out: Vec<(u64, i64)> = v["clips"].as_array().unwrap().iter().map(|c| (c["clip"].as_u64().unwrap(), c["sourceIn"].as_i64().unwrap())).collect();
    out.sort();
    out
}

#[test]
fn the_preview_is_what_the_slip_commits_for_every_delta() {
    let (mut s, v, a) = split_edit();
    for frames in (-40..=40).step_by(3) {
        for dragged in [v, a] {
            let p = preview(&mut s, dragged, frames);
            s.execute("timeline.slip", json!({"clip": dragged, "deltaFrames": frames})).unwrap();
            let committed = {
                let mut c = vec![(v, clip(&s, v).source_in.0), (a, clip(&s, a).source_in.0)];
                c.sort();
                c
            };
            assert_eq!(source_ins(&p), committed, "slip of {frames} frames dragging {dragged}");
            // the frames shown are the committed clip's first and last
            let pic = clip(&s, v);
            let fd = Tick(frame(&s));
            assert_eq!(p["in"]["mediaTime"].as_i64(), Some(pic.source_time_at(pic.start).0));
            assert_eq!(p["out"]["mediaTime"].as_i64(), Some(pic.source_time_at(pic.end() - fd).0));
            if frames != 0 {
                s.execute("edit.undo", json!({})).unwrap();
            }
        }
    }
}

#[test]
fn linked_picture_and_sound_stop_together_at_the_shorter_handle() {
    let (mut s, v, a) = split_edit();
    let fr = frame(&s);
    // the picture has 10 frames of tail handle: a 30 frame slip later stops both at 10
    let p = preview(&mut s, a, 30);
    assert_eq!(source_ins(&p), vec![(v, fr * 30), (a, fr * 30)]);
    assert_eq!(p["atLimit"], json!(true));
    assert_eq!(p["offsetFrames"], json!(10));
    assert_eq!(p["requested"], json!(fr * 30));
    // the head has 20 frames for both
    let p = preview(&mut s, v, -25);
    assert_eq!(source_ins(&p), vec![(v, 0), (a, 0)]);
    assert_eq!(p["offsetFrames"], json!(-20));
    let p = preview(&mut s, v, 5);
    assert_eq!(p["atLimit"], json!(false));
    assert_eq!(p["offsetFrames"], json!(5));
}

#[test]
fn the_preview_changes_nothing_and_makes_no_undo_step() {
    let (mut s, v, a) = split_edit();
    let before = s.project.clone();
    let undo = s.history.undo.len();
    let revision = s.revision;
    for frames in [-100, -3, 0, 7, 100] {
        preview(&mut s, v, frames);
        preview(&mut s, a, frames);
    }
    assert_eq!(*s.project, *before);
    assert_eq!(s.history.undo.len(), undo);
    assert_eq!(s.revision, revision);
}

#[test]
fn the_neighbours_are_the_touching_clips_and_do_not_move() {
    let (mut s, item) = session(25, 20);
    let fr = frame(&s);
    s.execute("sequence.linkedSelection", json!({"on": false})).unwrap();
    let place = |s: &mut Session, time: i64, source_in: i64| {
        let r = s.execute("timeline.place", json!({"item": item, "track": "V1", "time": fr * time, "sourceIn": fr * source_in, "duration": fr * 25})).unwrap();
        r["clips"][0].as_u64().unwrap()
    };
    let a = place(&mut s, 0, 100);
    let b = place(&mut s, 25, 200);
    let c = place(&mut s, 50, 300);
    let lone = place(&mut s, 100, 50); // after a gap
    let p0 = preview(&mut s, b, 0);
    for frames in [-30, -1, 1, 30] {
        let p = preview(&mut s, b, frames);
        assert_eq!(p["previous"]["clip"].as_u64(), Some(a));
        assert_eq!(p["next"]["clip"].as_u64(), Some(c));
        // the previous clip's last frame and the next one's first, whatever the slip
        assert_eq!(p["previous"], p0["previous"]);
        assert_eq!(p["next"], p0["next"]);
        assert_eq!(p["previous"]["mediaTime"].as_i64(), Some(fr * (100 + 24)));
        assert_eq!(p["next"]["mediaTime"].as_i64(), Some(fr * 300));
        assert_eq!(p["in"]["mediaTime"].as_i64(), Some(fr * (200 + frames)));
        assert_eq!(p["out"]["mediaTime"].as_i64(), Some(fr * (200 + 24 + frames)));
        // the slipped clip keeps its place and length
        assert_eq!(p["in"]["time"].as_i64(), Some(fr * 25));
        assert_eq!(p["out"]["time"].as_i64(), Some(fr * 49));
    }
    // the first clip has nothing before it, the one after a gap nothing on either side
    assert!(preview(&mut s, a, 5)["previous"].is_null());
    let p = preview(&mut s, lone, 5);
    assert!(p["previous"].is_null() && p["next"].is_null());
    assert!(p["in"].is_object() && p["out"].is_object());
}

#[test]
fn slipping_the_sound_shows_its_linked_picture() {
    let (mut s, v, a) = split_edit();
    let p = preview(&mut s, a, 4);
    assert_eq!(p["clip"].as_u64(), Some(a));
    assert_eq!(p["shown"].as_u64(), Some(v));
    assert_eq!(p["video"], json!(true));
    assert_eq!(p["in"]["video"], json!(true));
}

#[test]
fn sound_only_clips_preview_without_pictures() {
    let mut s = Session::default();
    s.execute("file.newProject", json!({"name": "Slip"})).unwrap();
    s.execute("file.newSequence", json!({"name": "Sequence", "fps": 25, "video": 1, "audio": 1, "width": 16, "height": 16})).unwrap();
    let r = s.execute("file.newOfflineFile", json!({"name": "Sound", "seconds": 4, "video": false, "audio": true})).unwrap();
    let item = r["item"].as_u64().unwrap();
    let fr = frame(&s);
    let r = s.execute("timeline.place", json!({"item": item, "track": "A1", "time": 0, "sourceIn": fr * 10, "duration": fr * 25})).unwrap();
    let a = r["clips"][0].as_u64().unwrap();
    let p = preview(&mut s, a, 3);
    assert_eq!(p["video"], json!(false));
    assert_eq!(p["in"]["video"], json!(false));
    assert_eq!(p["offsetFrames"], json!(3));
    s.execute("timeline.slip", json!({"clip": a, "deltaFrames": 3})).unwrap();
    assert_eq!(source_ins(&p), vec![(a, clip(&s, a).source_in.0)]);
}

#[test]
fn speed_reverse_and_other_media_rates_show_the_committed_frames() {
    // 30 fps media in a 25 fps sequence, at 200 %, then reversed
    for reverse in [false, true] {
        let (mut s, item) = session(30, 20);
        let fr = frame(&s);
        let r = s
            .execute("timeline.place", json!({"item": item, "track": "V1", "audioTrack": "A1", "time": 0, "sourceIn": fr * 100, "duration": fr * 50}))
            .unwrap();
        let v = r["clips"][0].as_u64().unwrap();
        s.execute("timeline.select", json!({"clips": [v]})).unwrap();
        s.execute("clip.speedDuration", json!({"speed": 200, "reverse": reverse, "ripple": false})).unwrap();
        for frames in [-7, 0, 13] {
            let p = preview(&mut s, v, frames);
            s.execute("timeline.slip", json!({"clip": v, "deltaFrames": frames})).unwrap();
            let it = clip(&s, v);
            let fd = Tick(fr);
            assert_eq!(p["in"]["mediaTime"].as_i64(), Some(it.source_time_at(it.start).0), "reverse {reverse}, {frames} frames");
            assert_eq!(p["out"]["mediaTime"].as_i64(), Some(it.source_time_at(it.end() - fd).0), "reverse {reverse}, {frames} frames");
            assert_eq!(p["offsetFrames"], json!(frames));
            // the media frame on the media's own 30 fps grid
            let rate30 = filmcraft_time::FrameRate::new(30, 1);
            assert_eq!(p["in"]["mediaFrame"].as_i64(), Some(rate30.frame_at(it.source_time_at(it.start))));
            if frames != 0 {
                s.execute("edit.undo", json!({})).unwrap();
            }
        }
    }
}

#[test]
fn the_source_timecodes_follow_the_slip() {
    let (mut s, v, _) = split_edit();
    // source In frame 20 + 5 = 00:00:01:00; the picture is 70 frames long, so its Out frame is
    // 20 + 69 + 5 = 94 = 00:00:03:19
    let p = preview(&mut s, v, 5);
    assert_eq!(p["in"]["sourceTimecode"], json!("00:00:01:00"));
    assert_eq!(p["out"]["sourceTimecode"], json!("00:00:03:19"));
}

#[test]
fn hostile_slip_preview_params_are_errors_or_clamped_never_panics() {
    let (mut s, v, a) = split_edit();
    assert!(s.execute("timeline.slipPreview", json!({})).is_err());
    assert!(s.execute("timeline.slipPreview", json!({"clip": 999_999, "delta": 5})).is_err());
    assert!(s.execute("timeline.slipPreview", json!({"clip": "V1", "delta": 5})).is_err());
    let fr = frame(&s);
    for d in [json!({"delta": i64::MAX}), json!({"delta": i64::MIN}), json!({"deltaFrames": i64::MAX}), json!({"deltaFrames": i64::MIN}), json!({"delta": "x"})]
    {
        let mut p = d.clone();
        p["clip"] = json!(v);
        let r = s.execute("timeline.slipPreview", p).unwrap();
        let ins = source_ins(&r);
        // clamped at one end of the media or not moved, in sync
        assert_eq!(ins[0].1, ins[1].1, "{d}");
        assert!([0, fr * 20, fr * 30].contains(&ins[0].1), "{d}: {ins:?}");
    }
    let _ = a;
}

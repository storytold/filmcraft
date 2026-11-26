//! `sequence.pacing` has to count the cuts a viewer sees: a razor that leaves the picture
//! continuous is not one, a removed stretch (a jump cut) is.

use serde_json::{Value, json};

use crate::Session;

fn pacing(s: &mut Session) -> Value {
    s.execute("sequence.pacing", json!({})).unwrap()
}

#[test]
fn shots_and_gaps_tile_the_whole_sequence_in_order() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let q = s.active_sequence().unwrap().clone();
    let segs = super::segments(&s.project, &q);
    assert!(!segs.is_empty());
    assert_eq!(segs.first().map(|g| g.start), Some(filmcraft_time::Tick::ZERO));
    assert_eq!(segs.last().map(|g| g.end), Some(q.duration()));
    for w in segs.windows(2) {
        assert_eq!(w[0].end, w[1].start, "segments are contiguous");
        assert!(w[0].start < w[0].end, "no empty segment");
        assert!(!(w[0].clip.is_none() && w[1].clip.is_none()), "black is merged into one gap");
    }
    let r = pacing(&mut s);
    let total: f64 = r["shots"].as_array().unwrap().iter().chain(r["gaps"].as_array().unwrap()).map(|g| g["seconds"].as_f64().unwrap()).sum();
    assert!((total - r["durationSeconds"].as_f64().unwrap()).abs() < 1e-6, "{r}");
    let stat = |k: &str| r["shotSeconds"][k].as_f64().unwrap_or_else(|| panic!("shotSeconds.{k} missing: {r}"));
    assert!(stat("min") <= stat("median") && stat("median") <= stat("max"), "{r}");
}

#[test]
fn a_through_edit_is_not_a_cut_but_a_jump_cut_is() {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let rate = s.sequence_rate();
    let before = pacing(&mut s);
    // the longest shot: its clip is the picture for the whole shot
    let shot = before["longest"].clone();
    let clip = shot["clip"].as_u64().unwrap();
    let (a, b) = (shot["start"].as_i64().unwrap(), shot["end"].as_i64().unwrap());
    let f1 = rate.frame_at(filmcraft_time::Tick(a + (b - a) / 3));
    let f2 = rate.frame_at(filmcraft_time::Tick(a + 2 * (b - a) / 3));
    assert!(f2 > f1 + 1, "the shot is long enough to cut twice");
    let (t1, t2) = (rate.tick_of(f1), rate.tick_of(f2));

    s.execute("timeline.razor", json!({"time": t2.0, "clip": clip})).unwrap();
    s.execute("timeline.razor", json!({"time": t1.0, "clip": clip})).unwrap();
    let razored = pacing(&mut s);
    assert_eq!(razored["shotCount"], before["shotCount"], "razor cuts that keep the picture continuous are not shots");
    assert_eq!(razored["cuts"], before["cuts"]);

    // take the middle piece out: what was one shot now jumps. Sync lock off, so the demo's
    // music bed (which has no room to ripple there) doesn't block the delete.
    let (middle, tracks) = {
        let q = s.active_sequence().unwrap();
        let middle = q.video_tracks.iter().flat_map(|t| t.items.iter()).find(|c| c.start == t1).map(|c| c.id.0).unwrap();
        (middle, q.all_tracks().map(|t| t.id.0).collect::<Vec<_>>())
    };
    for t in tracks {
        s.execute("timeline.setTrack", json!({"track": t, "syncLock": false})).unwrap();
    }
    s.execute("edit.rippleDelete", json!({"clips": [middle]})).unwrap();
    let after = pacing(&mut s);
    assert!(after["shotCount"].as_u64() > before["shotCount"].as_u64(), "before {before}\nafter {after}");
    // the picture lost the middle third (the unlocked music bed keeps the sequence's length)
    let shown = |r: &Value| r["shots"].as_array().unwrap().iter().map(|g| g["seconds"].as_f64().unwrap()).sum::<f64>();
    assert!(shown(&after) < shown(&before), "before {before}\nafter {after}");
}

/// A through edit is decided by what plays: reverse playback that carries on backwards is one
/// shot, a jump in reverse is a cut, so is a change of held frame or of multi-camera angle.
#[test]
fn through_edits_follow_direction_holds_and_angles() {
    use filmcraft_time::Tick;
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let base = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    let d = Tick::from_seconds_f64(1.0);
    let x = base.source_in;
    // two pieces of the same media, `a` then `b`, each `d` long
    let piece = |start: Tick, src: Tick, reverse: bool| {
        let mut c = base.clone();
        (c.start, c.duration, c.source_in, c.speed, c.reverse) = (start, d, src, 1.0, reverse);
        c
    };
    let through = |a: &filmcraft_project::TrackItem, b: &filmcraft_project::TrackItem| super::is_through_edit(&s.project, a, b);

    assert!(through(&piece(Tick::ZERO, x, false), &piece(d, x + d, false)), "forward, continuous");
    assert!(!through(&piece(Tick::ZERO, x, false), &piece(d, x + d + d, false)), "forward jump");
    // reversed: `a` plays x+d+d → x+d, `b` carries on x+d → x
    assert!(through(&piece(Tick::ZERO, x + d, true), &piece(d, x, true)), "reverse, continuous");
    assert!(!through(&piece(Tick::ZERO, x + d + d, true), &piece(d, x, true)), "reverse jump");
    assert!(!through(&piece(Tick::ZERO, x, false), &piece(d, x + d, true)), "direction change");
    // a reversed split rounds each piece's media range: a tick apart is still continuous
    assert!(through(&piece(Tick::ZERO, x + d + Tick(1), true), &piece(d, x, true)), "reverse, one tick of rounding");
    let mut quiet = piece(d, x + d, false);
    quiet.gain_db = -96.0;
    assert!(!through(&piece(Tick::ZERO, x, false), &quiet), "a gain change is heard");

    let held = |c: &mut filmcraft_project::TrackItem, at: Tick| c.frame_hold = Some(at);
    let (mut a, mut b) = (piece(Tick::ZERO, x, false), piece(d, x + d, false));
    held(&mut a, x);
    held(&mut b, x);
    assert!(through(&a, &b), "the same held frame carries on");
    held(&mut b, x + d);
    assert!(!through(&a, &b), "another held frame is a cut");

    let (mut a, mut b) = (piece(Tick::ZERO, x, false), piece(d, x + d, false));
    a.multicam = Some(filmcraft_project::MulticamSel { enabled: true, angle: 0 });
    b.multicam = Some(filmcraft_project::MulticamSel { enabled: true, angle: 1 });
    assert!(!through(&a, &b), "an angle change is a cut");
    // with multi-camera off the clip plays like a nest: a stored angle doesn't show
    a.multicam = Some(filmcraft_project::MulticamSel { enabled: false, angle: 0 });
    b.multicam = Some(filmcraft_project::MulticamSel { enabled: false, angle: 1 });
    assert!(through(&a, &b), "a disabled selection's angle is not a cut");
}

/// Overlapping clips on one track (a loaded project can have them) show what playback shows:
/// the latest start at or before a time, and nothing once that clip has ended (#5167).
#[test]
fn overlapping_clips_on_a_track_follow_playback() {
    use filmcraft_project::{ClipId, ItemId, TrackKind};
    use filmcraft_time::{Tick, TimeRange};
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let sec = Tick::from_seconds_f64;
    let item = |name: &str| s.project.items.iter().find(|(_, i)| i.name == name).map(|(id, _)| *id).unwrap();
    let (city, desert): (ItemId, ItemId) = (item("City_Night_Drive.mp4"), item("Desert_Dunes.mp4"));
    let seq = s.state.active_sequence.unwrap();
    let rate = s.sequence_rate();
    let p = std::sync::Arc::make_mut(&mut s.project);
    let mut clip = |item: ItemId, at: f64, len: f64| {
        let mut c = p.make_track_item(item, TrackKind::Video, sec(at), TimeRange::new(Tick::ZERO, sec(len)), rate).unwrap();
        c.id = ClipId(p.alloc_id());
        c
    };
    let (a, b) = (clip(city, 0.0, 10.0), clip(desert, 3.0, 2.0));
    let (a_id, b_id, b_end) = (a.id, b.id, b.end());
    let q = p.sequence_mut(seq).unwrap();
    for t in &mut q.video_tracks {
        t.items.clear();
    }
    q.video_tracks[0].items = vec![a, b];
    let q = s.project.sequence(seq).unwrap();
    let segs = super::segments(&s.project, q);
    let at = |x: f64| segs.iter().find(|g| g.start <= sec(x) && sec(x) < g.end).unwrap();
    assert_eq!(at(1.0).clip, Some(a_id));
    assert_eq!(at(4.0).clip, Some(b_id));
    assert_eq!(at(6.0).clip, None, "B has ended and playback shows nothing on V1: {segs:?}");
    assert_eq!(at(6.0).start, b_end, "the black starts where B ends");
}

/// A nest that outlasts its shortened contents shows nothing past their end: pacing reports
/// black there, as playback shows it (#5165).
#[test]
fn a_nest_past_its_contents_is_black() {
    use filmcraft_project::{ClipId, ItemId, TrackKind};
    use filmcraft_time::{Tick, TimeRange};
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let sec = Tick::from_seconds_f64;
    let city: ItemId = s.project.items.iter().find(|(_, i)| i.name == "City_Night_Drive.mp4").map(|(id, _)| *id).unwrap();
    let seq = s.state.active_sequence.unwrap();
    let rate = s.sequence_rate();
    let p = std::sync::Arc::make_mut(&mut s.project);
    let settings = p.sequence(seq).unwrap().settings.clone();
    // a child sequence with 6 s of contents
    let child = p.new_sequence("child", settings, 1, 1, None);
    let mut inner = p.make_track_item(city, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, sec(6.0)), rate).unwrap();
    inner.id = ClipId(p.alloc_id());
    p.sequence_mut(child).unwrap().video_tracks[0].items.push(inner);
    // in the parent, a 10 s nest of it: its last 4 s are past the contents
    let mut nest = p.make_track_item(child, TrackKind::Video, Tick::ZERO, TimeRange::new(Tick::ZERO, sec(6.0)), rate).unwrap();
    nest.id = ClipId(p.alloc_id());
    nest.duration = sec(10.0);
    let mut reversed = nest.clone();
    reversed.reverse = true;
    let reversed_overhang = p.nest_overhang(&reversed).expect("reversed, the overhang comes first");
    let nest_id = nest.id;
    let overhang = p.nest_overhang(&nest).expect("the nest outlasts its contents");
    let q = p.sequence_mut(seq).unwrap();
    for t in &mut q.video_tracks {
        t.items.clear();
    }
    q.video_tracks[0].items = vec![nest];
    let q = s.project.sequence(seq).unwrap();
    let segs = super::segments(&s.project, q);
    let at = |t: Tick| segs.iter().find(|g| g.start <= t && t < g.end).unwrap();
    assert_eq!(at(sec(1.0)).clip, Some(nest_id));
    let tail = at(overhang.start);
    assert_eq!((tail.clip, tail.start), (None, overhang.start), "black from where the contents end: {segs:?}");
    assert!(tail.end >= overhang.end());

    // reversed, the nest plays its contents backwards from where they end: black first, then picture
    let q = std::sync::Arc::make_mut(&mut s.project).sequence_mut(seq).unwrap();
    q.video_tracks[0].items[0].reverse = true;
    let q = s.project.sequence(seq).unwrap();
    let segs = super::segments(&s.project, q);
    let at = |t: Tick| segs.iter().find(|g| g.start <= t && t < g.end).unwrap();
    assert_eq!(at(reversed_overhang.start).clip, None, "{segs:?}");
    assert_eq!(at(reversed_overhang.end()).clip, Some(nest_id), "picture from where the overhang ends: {segs:?}");
}

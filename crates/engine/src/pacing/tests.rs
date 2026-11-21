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
    assert!(r["shotSeconds"]["min"].as_f64() <= r["shotSeconds"]["median"].as_f64());
    assert!(r["shotSeconds"]["median"].as_f64() <= r["shotSeconds"]["max"].as_f64());
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

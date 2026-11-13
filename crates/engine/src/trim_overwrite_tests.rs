//! Overwrite trimming: an edge dragged past a neighbour eats it (the neighbour is shortened, split
//! or removed) instead of stopping at the cut, linked partners trim together, and one drag is one
//! undo step.

use filmcraft_project::ClipId;
use serde_json::json;

use crate::Session;

fn demo() -> Session {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).expect("ok");
    s
}

fn longest(s: &Session, video: bool) -> u64 {
    s.project
        .items
        .values()
        .filter(|i| (if video { i.has_video() } else { i.has_audio() }) && i.as_media().is_some())
        .max_by_key(|i| crate::media_duration(&s.project, &s.media, i.id).map(|d| d.0).unwrap_or(0))
        .map(|i| i.id.0)
        .expect("a media item")
}

/// A fresh sequence with the two adjacent 48-frame clips every test trims, on V1.
fn two_clips(s: &mut Session) -> (ClipId, ClipId) {
    s.execute("file.newSequence", json!({"name": "Overwrite", "video": 1, "audio": 0})).expect("ok");
    let item = longest(s, true);
    let d48 = s.sequence_rate().tick_of(48);
    s.execute("timeline.place", json!({"item": item, "frame": 0, "duration": d48, "track": "V1"})).expect("ok");
    // the second clip starts 24 frames into its media, so its In edge has somewhere to grow
    s.execute("timeline.place", json!({"item": item, "frame": 48, "sourceIn": s.sequence_rate().tick_of(24), "duration": d48, "track": "V1"})).expect("ok");
    let q = s.active_sequence().expect("ok");
    let items = &q.video_tracks[0].items;
    assert_eq!(items.len(), 2, "two adjacent clips on V1");
    (items[0].id, items[1].id)
}

fn span(s: &Session, c: ClipId) -> Option<(i64, i64)> {
    s.active_sequence().and_then(|q| q.find_item(c)).map(|(_, i)| (i.start.0, i.end().0))
}

#[test]
fn an_overwrite_out_edge_shortens_the_clip_it_lands_on() {
    let mut s = demo();
    let (a, b) = two_clips(&mut s);
    let rate = s.sequence_rate();
    let d24 = rate.tick_of(24);
    let before = (*s.project).clone();
    let r = s.execute("timeline.trim", json!({"clip": a.0, "edge": "out", "mode": "overwrite", "delta": d24.0})).expect("ok");
    assert_eq!(r["delta"], json!(d24.0));
    let (a_start, a_end) = span(&s, a).expect("the trimmed clip");
    assert_eq!((a_start, a_end), (0, rate.tick_of(72).0), "the Out edge moved 24 frames right");
    let q = s.active_sequence().expect("ok");
    assert!(q.find_item(b).is_none(), "the neighbour was split: its old id is gone");
    assert_eq!(q.video_tracks[0].items.len(), 2, "the rest of the neighbour lives on");
    let rest = &q.video_tracks[0].items[1];
    assert_eq!((rest.start.0, rest.end().0), (rate.tick_of(72).0, rate.tick_of(96).0), "shortened from its head");
    assert!(rest.source_in > filmcraft_time::Tick::ZERO, "it keeps showing the same media, later");
    q.check().expect("ok");
    s.execute("edit.undo", json!({})).expect("ok");
    assert_eq!(*s.project, before, "one undo step puts the neighbour back");
}

#[test]
fn an_overwrite_out_edge_wipes_a_clip_it_fully_covers() {
    let mut s = demo();
    let (a, b) = two_clips(&mut s);
    let rate = s.sequence_rate();
    let r = s.execute("timeline.trim", json!({"clip": a.0, "edge": "out", "mode": "overwrite", "delta": rate.tick_of(48).0})).expect("ok");
    assert_eq!(r["overwritten"], json!([b.0]), "the caller is told what was eaten");
    assert_eq!(span(&s, a).map(|(_, e)| e), Some(rate.tick_of(96).0));
    assert_eq!(s.active_sequence().expect("ok").video_tracks[0].items.len(), 1, "the neighbour is gone");
    s.active_sequence().expect("ok").check().expect("ok");
}

#[test]
fn a_clamped_trim_still_stops_at_the_neighbour() {
    let mut s = demo();
    let (a, _) = two_clips(&mut s);
    let rate = s.sequence_rate();
    let r = s.execute("timeline.trim", json!({"clip": a.0, "edge": "out", "mode": "regular", "delta": rate.tick_of(48).0})).expect("ok");
    assert_eq!(r["delta"], json!(0), "Alt (regular) keeps the old clamp: the neighbour is not touched");
    assert_eq!(s.active_sequence().expect("ok").video_tracks[0].items.len(), 2);
}

#[test]
fn an_overwrite_trim_shortens_the_linked_sound_and_its_neighbour_together() {
    let mut s = demo();
    s.execute("file.newSequence", json!({"name": "Linked", "video": 1, "audio": 1})).expect("ok");
    let item = s.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).map(|i| i.id.0).expect("ok");
    let rate = s.sequence_rate();
    let d48 = rate.tick_of(48);
    for frame in [0, 48] {
        s.execute("timeline.place", json!({"item": item, "frame": frame, "duration": d48, "track": "V1"})).expect("ok");
    }
    let (a, b_sound) = {
        let q = s.active_sequence().expect("ok");
        assert_eq!(q.video_tracks[0].items.len(), 2, "two pictures on V1");
        assert_eq!(q.audio_tracks[0].items.len(), 2, "and their two sounds on A1");
        let a = q.video_tracks[0].items[0].clone();
        let b_sound = q.audio_tracks[0].items[1].clone();
        assert_eq!(a.link, q.audio_tracks[0].items[0].link, "the first pair is linked");
        (a, b_sound)
    };
    let before = (*s.project).clone();
    let d24 = rate.tick_of(24);
    s.execute("timeline.trim", json!({"clip": a.id.0, "edge": "out", "mode": "overwrite", "delta": d24.0})).expect("ok");
    let q = s.active_sequence().expect("ok");
    assert_eq!(q.video_tracks[0].items[0].end().0, rate.tick_of(72).0, "the picture grew");
    assert_eq!(q.audio_tracks[0].items[0].end().0, rate.tick_of(72).0, "its sound grew with it, still in sync");
    assert!(q.find_item(b_sound.id).is_none(), "the sound the edge landed on was split");
    let right_v = q.video_tracks[0].items[1].clone();
    let right_a = q.audio_tracks[0].items[1].clone();
    assert_eq!((right_v.start.0, right_v.end().0), (rate.tick_of(72).0, rate.tick_of(96).0));
    assert_eq!((right_a.start.0, right_a.end().0), (rate.tick_of(72).0, rate.tick_of(96).0));
    assert!(right_v.link.is_some(), "the shortened picture and its sound stay linked");
    assert_eq!(right_v.link, right_a.link, "split together, linked together");
    q.check().expect("ok");
    s.execute("edit.undo", json!({})).expect("ok");
    assert_eq!(*s.project, before, "one undo step puts both tracks back");
}

#[test]
fn an_overwrite_in_edge_eats_the_tail_of_the_clip_before_it() {
    let mut s = demo();
    let (a, b) = two_clips(&mut s);
    let rate = s.sequence_rate();
    let before = (*s.project).clone();
    s.execute("timeline.trim", json!({"clip": b.0, "edge": "in", "mode": "overwrite", "delta": -rate.tick_of(24).0})).expect("ok");
    let (a_start, a_end) = span(&s, a).expect("the clip before");
    assert_eq!((a_start, a_end), (0, rate.tick_of(24).0), "its tail was eaten");
    let (b_start, b_end) = span(&s, b).expect("the wider clip keeps its id");
    assert_eq!((b_start, b_end), (rate.tick_of(24).0, rate.tick_of(96).0), "the In edge moved left into the cut");
    s.active_sequence().expect("ok").check().expect("ok");
    s.execute("edit.undo", json!({})).expect("ok");
    assert_eq!(*s.project, before);
}

/// Two captions meet at frame 12: trimming one edge over the other eats or shortens it.
fn two_captions(s: &mut Session) -> (u64, u64) {
    s.execute("file.newSequence", json!({"name": "Captions", "video": 1, "audio": 0})).expect("ok");
    let rate = s.sequence_rate();
    let d12 = rate.tick_of(12).0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64;
    let c1 = s.execute("captions.add", json!({"frame": 0, "durationSeconds": d12, "text": "one"})).expect("ok")["caption"].as_u64().expect("ok");
    let c2 = s.execute("captions.add", json!({"frame": 12, "durationSeconds": d12, "text": "two"})).expect("ok")["caption"].as_u64().expect("ok");
    (c1, c2)
}

fn caption_span(s: &Session, c: ClipId) -> Option<(i64, i64)> {
    s.active_sequence().and_then(|q| q.find_caption(c)).map(|(_, c)| (c.start.0, c.end().0))
}

#[test]
fn an_overwrite_caption_edge_eats_its_neighbour() {
    let mut s = demo();
    let (c1, c2) = two_captions(&mut s);
    let rate = s.sequence_rate();
    let before = (*s.project).clone();
    // trim caption one's Out edge 12 frames right: it covers all of caption two
    let r = s.execute("captions.trim", json!({"caption": c1, "edge": "out", "mode": "overwrite", "delta": rate.tick_of(12).0})).expect("ok");
    assert_eq!(r["overwritten"], json!([c2]), "the covered caption is reported");
    assert_eq!(caption_span(&s, ClipId(c1)), Some((0, rate.tick_of(24).0)));
    assert_eq!(caption_span(&s, ClipId(c2)), None, "and gone");
    s.active_sequence().expect("ok").check().expect("ok");
    s.execute("edit.undo", json!({})).expect("ok");
    assert_eq!(*s.project, before);
}

#[test]
fn an_overwrite_caption_in_edge_shortens_the_caption_before_it() {
    let mut s = demo();
    let (c1, c2) = two_captions(&mut s);
    let rate = s.sequence_rate();
    // caption two's In edge moves 6 frames left: it eats the tail of caption one
    s.execute("captions.trim", json!({"caption": c2, "edge": "in", "mode": "overwrite", "delta": -rate.tick_of(6).0})).expect("ok");
    assert_eq!(caption_span(&s, ClipId(c1)), Some((0, rate.tick_of(6).0)), "caption one keeps its id and text slot, shortened");
    assert_eq!(caption_span(&s, ClipId(c2)), Some((rate.tick_of(6).0, rate.tick_of(24).0)));
}

#[test]
fn a_regular_caption_trim_still_stops_at_the_neighbour() {
    let mut s = demo();
    let (c1, c2) = two_captions(&mut s);
    let rate = s.sequence_rate();
    // leave a 12-frame gap, then a regular Out trim stops at the next caption's start
    s.execute("captions.move", json!({"caption": c2, "delta": rate.tick_of(12).0})).expect("ok");
    let r = s.execute("captions.trim", json!({"caption": c1, "edge": "out", "mode": "regular", "delta": rate.tick_of(24).0})).expect("ok");
    assert_eq!(r["applied"], json!(rate.tick_of(12).0));
    assert_eq!(caption_span(&s, ClipId(c1)), Some((0, rate.tick_of(24).0)), "moved 12 frames, no further");
    assert_eq!(caption_span(&s, ClipId(c2)), Some((rate.tick_of(24).0, rate.tick_of(36).0)), "the neighbour is untouched");
}

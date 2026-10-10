//! Gaps: D selects the gap under the playhead, Delete closes it; and the playhead lines up with
//! cuts stored a tick before a frame boundary (a 58.824 fps screen recording cut in 0.4.0).

use super::*;
use filmcraft_project::{SequenceSettings, TrackItem};
use serde_json::json;

/// 7353/125 fps: a frame is 4 318 237 453.42 ticks, so 0.4.0's rounded-down `tick_of` stored
/// every cut one tick before its frame boundary.
const RATE: FrameRate = FrameRate { num: 7353, den: 125 };

fn cut(f: i64) -> Tick {
    RATE.tick_of(f) - Tick(1)
}

/// A sequence like the one in the report: V1/A1 linked clips at frames 0–100, 100–177,
/// 177–253, a gap, then 323–400, every cut a tick early. Returns the session and the clip ids
/// (picture, sound) of the four clips.
fn screen_recording() -> (Session, Vec<(ClipId, ClipId)>) {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let template: TrackItem = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
    let spans = [(Tick::ZERO, cut(100)), (cut(100), cut(177)), (cut(177), cut(253)), (cut(323), cut(400))];
    let p = std::sync::Arc::make_mut(&mut s.project);
    let settings = SequenceSettings { frame_rate: RATE, ..Default::default() };
    let sid = p.new_sequence("Screen Recording", settings, 2, 2, None);
    let mut next = p.next_id.max(100_000);
    let mut ids = Vec::new();
    let seq = p.sequence_mut(sid).unwrap();
    for (a, b) in spans {
        let (v, au, link) = (ClipId(next), ClipId(next + 1), next + 2);
        next += 3;
        for (track, id) in [(&mut seq.video_tracks[0], v), (&mut seq.audio_tracks[0], au)] {
            let mut it = template.clone();
            (it.id, it.start, it.duration, it.source_in, it.link) = (id, a, b - a, a, Some(link));
            it.effects.clear();
            track.items.push(it);
        }
        ids.push((v, au));
    }
    p.next_id = next;
    s.execute("sequence.open", json!({"item": sid.0})).unwrap();
    (s, ids)
}

fn starts(s: &Session) -> Vec<Tick> {
    s.active_sequence().unwrap().video_tracks[0].items.iter().map(|i| i.start).collect()
}

#[test]
fn edit_points_a_tick_before_a_frame_put_the_playhead_on_that_frame() {
    let (mut s, _) = screen_recording();
    s.set_playhead(RATE.tick_of(240));
    // Down: the end of the third clip, frame 253 (0.4.0 parked on 252, the clip's last frame)
    s.execute("playhead.nextEdit", json!({})).unwrap();
    assert_eq!(s.playhead(), RATE.tick_of(253));
    s.execute("playhead.nextEdit", json!({})).unwrap();
    assert_eq!(s.playhead(), RATE.tick_of(323));
    // Up walks back without sticking on the cut a tick before the playhead
    s.execute("playhead.prevEdit", json!({})).unwrap();
    assert_eq!(s.playhead(), RATE.tick_of(253));
    s.execute("playhead.prevEdit", json!({})).unwrap();
    assert_eq!(s.playhead(), RATE.tick_of(177));
    // End: lined up with the end of the last clip
    s.execute("playhead.end", json!({})).unwrap();
    assert_eq!(s.playhead(), RATE.tick_of(400));
    // a cut set straight as the playhead lands on its frame too
    s.set_playhead(cut(253));
    assert_eq!(s.playhead(), RATE.tick_of(253));
    // Shift+End: the selected clip's last frame
    s.execute("timeline.selectClipAtPlayhead", json!({})).unwrap();
    s.set_playhead(RATE.tick_of(120));
    s.execute("timeline.selectClipAtPlayhead", json!({})).unwrap();
    s.execute("playhead.selectedClipEnd", json!({})).unwrap();
    assert_eq!(s.playhead(), RATE.tick_of(176));
    s.execute("playhead.selectedClipStart", json!({})).unwrap();
    assert_eq!(s.playhead(), RATE.tick_of(100));
}

#[test]
fn d_in_a_gap_selects_the_gap_and_delete_closes_it() {
    let (mut s, ids) = screen_recording();
    // the user's steps: Down to the end of the clip before the gap, D, Delete
    s.set_playhead(RATE.tick_of(240));
    s.execute("playhead.nextEdit", json!({})).unwrap();
    let r = s.execute("timeline.selectClipAtPlayhead", json!({})).unwrap();
    assert!(s.state.selection.is_empty(), "the clip before the gap is not selected: {r}");
    let gaps = gaps::selected_gaps(&s);
    assert_eq!(gaps.len(), 2, "the gap on V1 and on A1: {r}");
    for g in &gaps {
        assert_eq!((g.start, g.end), (cut(253), cut(323)));
    }
    assert_eq!(r["gaps"].as_array().map(Vec::len), Some(2));
    // Delete (Clear) closes it, picture and sound together; nothing is deleted
    s.execute("edit.clear", json!({})).unwrap();
    assert_eq!(starts(&s), vec![Tick::ZERO, cut(100), cut(177), cut(253)]);
    let a1: Vec<Tick> = s.active_sequence().unwrap().audio_tracks[0].items.iter().map(|i| i.start).collect();
    assert_eq!(a1, starts(&s));
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.len(), 4);
    assert!(gaps::selected_gaps(&s).is_empty());
    assert!(s.state.gap_selection.is_empty());
    // one undo step brings the gap back
    s.execute("edit.undo", json!({})).unwrap();
    assert_eq!(starts(&s)[3], cut(323));
    // on a clip, D still selects the clip (and its linked sound), not a gap
    s.set_playhead(RATE.tick_of(200));
    s.execute("timeline.selectClipAtPlayhead", json!({})).unwrap();
    assert_eq!(s.state.selection.len(), 2);
    assert!(s.state.selection.contains(&ids[2].0) && s.state.selection.contains(&ids[2].1));
    assert!(gaps::selected_gaps(&s).is_empty());
    // on the first frame after the gap: the clip there
    s.set_playhead(RATE.tick_of(323));
    s.execute("timeline.selectClipAtPlayhead", json!({})).unwrap();
    assert!(s.state.selection.contains(&ids[3].0));
}

#[test]
fn ripple_delete_and_select_gap_by_track() {
    let (mut s, ids) = screen_recording();
    let v1 = s.active_sequence().unwrap().video_tracks[0].id;
    // a click in the gap on V1 selects that gap only
    let r = s.execute("timeline.selectGap", json!({"track": "V1", "time": RATE.tick_of(300).0})).unwrap();
    assert_eq!(r["gaps"][0]["trackName"], "V1", "{r}");
    let g = gaps::selected_gaps(&s);
    assert_eq!(g.len(), 1);
    assert_eq!(g[0].track, v1);
    // Shift+Delete closes it too; A1 is sync-locked and empty there, so it follows
    s.execute("edit.rippleDelete", json!({})).unwrap();
    assert_eq!(starts(&s)[3], cut(253));
    assert_eq!(s.active_sequence().unwrap().audio_tracks[0].items[3].start, cut(253));
    // a click on a clip or after the last clip selects no gap and clears the selection
    s.execute("timeline.select", json!({"clips": [ids[0].0.0]})).unwrap();
    let r = s.execute("timeline.selectGap", json!({"track": "V1", "time": RATE.tick_of(5000).0})).unwrap();
    assert_eq!(r["gaps"].as_array().map(Vec::len), Some(0));
    assert!(s.state.selection.is_empty());
    // nothing selected: Delete is disabled with a reason, not an edit
    assert!(s.execute("edit.clear", json!({})).is_err());
}

#[test]
fn a_gap_selection_gives_way_to_clips_and_to_edits() {
    let (mut s, ids) = screen_recording();
    s.execute("timeline.selectGap", json!({"track": "V1", "time": RATE.tick_of(300).0})).unwrap();
    assert_eq!(gaps::selected_gaps(&s).len(), 1);
    // selecting a clip drops the gap; deselecting doesn't bring it back
    s.execute("timeline.select", json!({"clips": [ids[1].0.0]})).unwrap();
    assert!(s.state.gap_selection.is_empty());
    s.execute("edit.deselectAll", json!({})).unwrap();
    assert!(gaps::selected_gaps(&s).is_empty());
    // an edit that fills the gap makes the old selection stale: Delete must not close anything
    s.execute("timeline.selectGap", json!({"track": "V1", "time": RATE.tick_of(300).0})).unwrap();
    let before = starts(&s);
    let item = s.active_sequence().unwrap().video_tracks[0].items[0].item;
    let gap_item = ClipId(999_999);
    {
        let p = std::sync::Arc::make_mut(&mut s.project);
        let sid = s.state.active_sequence.unwrap();
        let tr = &mut p.sequence_mut(sid).unwrap().video_tracks[0];
        let mut it = tr.items[0].clone();
        (it.id, it.item, it.start, it.duration, it.link) = (gap_item, item, RATE.tick_of(280), RATE.tick_of(10), None);
        tr.items.push(it);
        tr.sort();
    }
    assert!(gaps::selected_gaps(&s).is_empty(), "the gap changed: the selection no longer stands");
    assert!(s.execute("edit.clear", json!({})).is_err());
    assert_eq!(starts(&s).len(), before.len() + 1);
}

/// Old cuts sit a tick before the frame boundary the playhead parks on: Cmd+K there must not
/// leave a one-tick clip.
#[test]
fn add_edit_on_a_cut_a_tick_away_makes_no_sliver() {
    let (mut s, _) = screen_recording();
    s.set_playhead(RATE.tick_of(323));
    let r = s.execute("sequence.addEdit", json!({})).unwrap();
    assert_eq!(r["cuts"], 0, "{r}");
    assert_eq!(s.active_sequence().unwrap().video_tracks[0].items.len(), 4);
    s.set_playhead(RATE.tick_of(350));
    let r = s.execute("sequence.addEdit", json!({})).unwrap();
    assert_eq!(r["cuts"], 2, "{r}");
}

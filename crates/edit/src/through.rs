//! Through edits, gaps and range copies.
//!
//! - A **through edit** is a cut between two adjacent pieces of the same source that continue each
//!   other in media time (what Add Edit leaves behind). The timeline can mark them (Sequence ▸ Show
//!   Through Edits) and Join Through Edits heals them; the left piece's attributes win.
//! - A **gap** is empty time followed by material: on one track, or on every track at once (Sequence
//!   ▸ Go to Gap).
//! - [`clip_item_to`] trims a copy of an item to a time range (Make Subsequence).

use std::collections::HashSet;

use filmcraft_project::{ClipId, Sequence, Track, TrackId, TrackItem, TrackKind};
use filmcraft_time::{Tick, TimeRange};

use crate::remove_orphan_transitions;

/// Media-time slack when comparing the pieces of a cut (speed changes round each piece's source
/// range to whole ticks).
const MEDIA_SLACK: i64 = 2;

/// Whether the cut between adjacent items `a` (left) and `b` (right) is a through edit: same
/// source, same speed and direction, and `b` continues `a` in media time.
pub fn is_through_edit(a: &TrackItem, b: &TrackItem) -> bool {
    if a.end() != b.start || a.item != b.item || a.speed != b.speed || a.reverse != b.reverse || a.multicam != b.multicam {
        return false;
    }
    match (a.frame_hold, b.frame_hold) {
        (None, None) => {}
        (Some(x), Some(y)) => return x == y,
        _ => return false,
    }
    let diff = if a.reverse { b.source_out() - a.source_in } else { b.source_in - a.source_out() };
    diff.0.abs() <= MEDIA_SLACK
}

/// Compare audio stream and source-channel choices for audio continuity; linked video joins are guarded separately.
fn is_through_edit_on_track(is_audio: bool, a: &TrackItem, b: &TrackItem) -> bool {
    is_through_edit(a, b) && (!is_audio || (a.audio_stream == b.audio_stream && a.source_channels == b.source_channels))
}

/// Audio links that would remain after the requested, unlocked audio cuts heal. This also sees
/// pieces moved to another track or separated by a gap: they cannot be removed by an audio join.
fn surviving_audio_links(seq: &Sequence, wanted: &impl Fn(ClipId, ClipId) -> bool) -> HashSet<u64> {
    let mut links = HashSet::new();
    for track in &seq.audio_tracks {
        let mut items = track.items.iter();
        let Some(first) = items.next() else { continue };
        let mut left = first.clone();
        for right in items {
            if !track.locked && wanted(left.id, right.id) && is_through_edit_on_track(true, &left, right) {
                left.duration += right.duration;
                if left.reverse {
                    left.source_in = right.source_in;
                }
            } else {
                if let Some(link) = left.link {
                    links.insert(link);
                }
                left = right.clone();
            }
        }
        if let Some(link) = left.link {
            links.insert(link);
        }
    }
    links
}

/// Keep the right-hand clip ID of a video cut when its distinct link would still be owned by
/// audio after the requested audio joins. Joining that video cut would orphan its sound.
fn blocked_video_right_ids(seq: &Sequence, audio_links: &HashSet<u64>) -> HashSet<ClipId> {
    seq.video_tracks
        .iter()
        .flat_map(|track| track.items.windows(2))
        .filter(|pair| pair[0].link != pair[1].link && pair[1].link.is_some_and(|right| audio_links.contains(&right)))
        .map(|pair| pair[1].id)
        .collect()
}

/// One through edit on a track.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ThroughEdit {
    pub track: TrackId,
    pub left: ClipId,
    pub right: ClipId,
    pub time: Tick,
}

fn track_through_edits_with_kind(track: &Track, is_audio: bool) -> Vec<ThroughEdit> {
    track
        .items
        .windows(2)
        .filter(|w| is_through_edit_on_track(is_audio, &w[0], &w[1]))
        .map(|w| ThroughEdit { track: track.id, left: w[0].id, right: w[1].id, time: w[1].start })
        .collect()
}

/// Through edits on one track, left to right.
pub fn track_through_edits(track: &Track) -> Vec<ThroughEdit> {
    track_through_edits_with_kind(track, track.kind == TrackKind::Audio)
}

/// Through edits on every video and audio track.
pub fn through_edits(seq: &Sequence) -> Vec<ThroughEdit> {
    seq.video_tracks
        .iter()
        .flat_map(|track| track_through_edits_with_kind(track, false))
        .chain(seq.audio_tracks.iter().flat_map(|track| track_through_edits_with_kind(track, true)))
        .collect()
}

/// Through edits on one sequence track, using its actual video/audio storage rather than a
/// serialized track kind that a malformed project could misstate.
pub fn through_edits_on_track(seq: &Sequence, track_id: TrackId) -> Vec<ThroughEdit> {
    if let Some(track) = seq.video_tracks.iter().find(|track| track.id == track_id) {
        track_through_edits_with_kind(track, false)
    } else if let Some(track) = seq.audio_tracks.iter().find(|track| track.id == track_id) {
        track_through_edits_with_kind(track, true)
    } else {
        Vec::new()
    }
}

/// Cuts that can be joined without discarding a surviving linked audio piece.
pub fn joinable_through_edits(seq: &Sequence) -> Vec<ThroughEdit> {
    joinable_through_edits_where(seq, |_, _| true)
}

/// Joinability for the same selection that the command will apply. Simulate only audio joins to
/// determine which links survive, then inspect the original cuts without changing the sequence.
pub fn joinable_through_edits_where(seq: &Sequence, wanted: impl Fn(ClipId, ClipId) -> bool) -> Vec<ThroughEdit> {
    let audio_links = surviving_audio_links(seq, &wanted);
    let blocked = blocked_video_right_ids(seq, &audio_links);
    let mut edits = Vec::new();
    for track in seq.video_tracks.iter().filter(|track| !track.locked) {
        edits.extend(
            track
                .items
                .windows(2)
                .filter(|pair| wanted(pair[0].id, pair[1].id) && is_through_edit_on_track(false, &pair[0], &pair[1]) && !blocked.contains(&pair[1].id))
                .map(|pair| ThroughEdit { track: track.id, left: pair[0].id, right: pair[1].id, time: pair[1].start }),
        );
    }
    for track in seq.audio_tracks.iter().filter(|track| !track.locked) {
        edits.extend(track_through_edits_with_kind(track, true).into_iter().filter(|e| wanted(e.left, e.right)));
    }
    edits
}

/// Join through edits on unlocked tracks. With `only` non-empty, just the edits whose left or right
/// piece is listed. The left piece absorbs the right one (keeping its own effects and attributes);
/// a transition sitting on the cut is removed. Returns the number of edits joined.
pub fn join_through_edits(seq: &mut Sequence, only: &[ClipId]) -> usize {
    join_through_edits_where(seq, |a, b| only.is_empty() || only.contains(&a) || only.contains(&b))
}

/// Join the through edits on unlocked tracks whose (left, right) pieces `wanted` accepts.
pub fn join_through_edits_where(seq: &mut Sequence, wanted: impl Fn(ClipId, ClipId) -> bool) -> usize {
    let none = HashSet::new();
    let mut n = 0;
    // Audio first: the remaining links are the ones a video join must preserve.
    for tr in seq.audio_tracks.iter_mut().filter(|track| !track.locked) {
        n += join_track_through_edits(tr, true, &wanted, &none);
    }
    let audio_links = seq.audio_tracks.iter().flat_map(|track| track.items.iter().filter_map(|item| item.link)).collect();
    let blocked = blocked_video_right_ids(seq, &audio_links);
    for tr in seq.video_tracks.iter_mut().filter(|track| !track.locked) {
        n += join_track_through_edits(tr, false, &wanted, &blocked);
    }
    n
}

fn join_track_through_edits(tr: &mut Track, is_audio: bool, wanted: &impl Fn(ClipId, ClipId) -> bool, blocked: &HashSet<ClipId>) -> usize {
    let mut n = 0;
    let mut i = 0;
    while i + 1 < tr.items.len() {
        let (a, b) = (&tr.items[i], &tr.items[i + 1]);
        if !(wanted(a.id, b.id) && is_through_edit_on_track(is_audio, a, b) && !blocked.contains(&b.id)) {
            i += 1;
            continue;
        }
        let (aid, bid) = (a.id, b.id);
        let (dur, b_source_in) = (b.duration, b.source_in);
        let left = &mut tr.items[i];
        left.duration += dur;
        if left.reverse {
            // reversed: the right piece shows the earlier media
            left.source_in = b_source_in;
        }
        tr.items.remove(i + 1);
        tr.transitions.retain(|t| !(t.from == Some(aid) && t.to == Some(bid)));
        for t in &mut tr.transitions {
            if t.from == Some(bid) {
                t.from = Some(aid);
            }
            if t.to == Some(bid) {
                t.to = Some(aid);
            }
        }
        n += 1;
    }
    remove_orphan_transitions(tr);
    n
}

/// Start times of the gaps on one track: empty time from 0 or from the end of an item up to the
/// next item (trailing empty time after the last item is not a gap).
pub fn track_gaps(track: &Track) -> Vec<TimeRange> {
    gaps_of(track.items.iter().map(TrackItem::range))
}

/// Gaps of the whole sequence: time where no video or audio track has material, followed by
/// material.
pub fn sequence_gaps(seq: &Sequence) -> Vec<TimeRange> {
    let mut spans: Vec<TimeRange> = seq.all_tracks().flat_map(|t| t.items.iter().map(TrackItem::range)).collect();
    spans.sort_by_key(|r| r.start);
    gaps_of(spans.into_iter())
}

/// Gaps between `spans` (sorted by start), from time 0.
fn gaps_of(spans: impl Iterator<Item = TimeRange>) -> Vec<TimeRange> {
    let mut out = Vec::new();
    let mut covered = Tick::ZERO;
    for r in spans {
        if r.start > covered {
            out.push(TimeRange::from_bounds(covered, r.start));
        }
        covered = covered.max(r.end());
    }
    out
}

/// A copy of `it` trimmed to the timeline `range` (None when they don't overlap). Media time
/// follows the trim, so the copy shows the same frames at the same timeline times.
pub fn clip_item_to(it: &TrackItem, range: TimeRange) -> Option<TrackItem> {
    let a = it.start.max(range.start);
    let b = it.end().min(range.end());
    if b <= a {
        return None;
    }
    let mut c = it.clone();
    let head = a - it.start;
    let tail = it.end() - b;
    let speed = it.speed.abs();
    let src = |d: Tick| Tick((d.0 as f64 * speed).round() as i64);
    if it.frame_hold.is_none() {
        if it.reverse {
            c.source_in += src(tail);
        } else {
            c.source_in += src(head);
        }
    }
    c.start = a;
    c.duration = b - a;
    Some(c)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{EditCtx, razor};
    use filmcraft_project::{ItemId, Label, SequenceSettings, TrackKind};

    fn item(id: u64, start: i64, dur: i64, src: i64) -> TrackItem {
        TrackItem {
            id: ClipId(id),
            item: ItemId(1),
            name: "c".into(),
            label: Label::Iris,
            start: Tick(start),
            duration: Tick(dur),
            source_in: Tick(src),
            speed: 1.0,
            reverse: false,
            enabled: true,
            link: None,
            group: None,
            effects: vec![],
            markers: vec![],
            gain_db: 0.0,
            frame_hold: None,
            scale_to_frame: false,
            essential: None,
            multicam: None,
            time_interpolation: Default::default(),
            hold_filters: false,
            field_options: None,
            source_channels: Vec::new(),
            audio_stream: 0,
            graphic: None,
        }
    }

    fn seq_with(items: Vec<TrackItem>) -> Sequence {
        let mut p = filmcraft_project::Project::new("t");
        let id = p.new_sequence("s", SequenceSettings::default(), 1, 1, None);
        let mut q = p.sequence(id).unwrap().clone();
        q.video_tracks[0].items = items;
        q
    }

    fn ctx(next: &mut u64) -> EditCtx<'_> {
        EditCtx { next_id: next, media_duration: &|_| None, media_start: &|_| Tick::ZERO, min_duration: Tick(1) }
    }

    #[test]
    fn razor_makes_a_through_edit_and_join_heals_it() {
        let mut q = seq_with(vec![item(10, 0, 100, 500)]);
        let orig = q.clone();
        let mut n = 1000;
        razor(&mut q, &[], Tick(40), &mut ctx(&mut n));
        let te = through_edits(&q);
        assert_eq!(te.len(), 1);
        assert_eq!(te[0].time, Tick(40));
        assert_eq!(join_through_edits(&mut q, &[]), 1);
        assert_eq!(q.video_tracks[0].items, orig.video_tracks[0].items);
    }

    #[test]
    fn reversed_and_speed_changed_pieces_join() {
        for (speed, reverse) in [(1.0, true), (1.5, false), (0.7, true)] {
            let mut it = item(10, 0, 99, 500);
            it.speed = speed;
            it.reverse = reverse;
            let mut q = seq_with(vec![it.clone()]);
            let mut n = 1000;
            razor(&mut q, &[], Tick(37), &mut ctx(&mut n));
            assert_eq!(through_edits(&q).len(), 1, "speed {speed} reverse {reverse}");
            join_through_edits(&mut q, &[]);
            let j = &q.video_tracks[0].items[0];
            assert_eq!((j.start, j.duration), (it.start, it.duration));
            assert!((j.source_in - it.source_in).0.abs() <= MEDIA_SLACK, "speed {speed} reverse {reverse}: {:?} vs {:?}", j.source_in, it.source_in);
        }
    }

    #[test]
    fn discontinuous_neighbours_are_not_through_edits() {
        let q = seq_with(vec![item(10, 0, 40, 500), item(11, 40, 60, 600)]);
        assert!(through_edits(&q).is_empty());
        let mut other = item(11, 40, 60, 540);
        other.item = ItemId(2);
        let q = seq_with(vec![item(10, 0, 40, 500), other]);
        assert!(through_edits(&q).is_empty());
    }

    #[test]
    fn different_audio_sources_are_not_through_edits() {
        let left = item(10, 0, 40, 500);
        for (stream, channels) in [(1, vec![]), (0, vec![1])] {
            let mut right = item(11, 40, 60, 540);
            right.audio_stream = stream;
            right.source_channels = channels;
            let mut q = seq_with(Vec::new());
            q.audio_tracks[0].items = vec![left.clone(), right];
            let before = q.clone();
            assert!(through_edits(&q).is_empty(), "different stream/channel mapping must preserve the cut");
            assert_eq!(join_through_edits(&mut q, &[]), 0);
            assert_eq!(q, before, "joining cannot replace a different audio source");
            let mut malformed = before.clone();
            malformed.audio_tracks[0].kind = TrackKind::Video;
            assert!(through_edits(&malformed).is_empty(), "audio-track storage, not a serialized kind, determines source matching");
            assert!(through_edits_on_track(&malformed, malformed.audio_tracks[0].id).is_empty());
            assert_eq!(join_through_edits(&mut malformed, &[]), 0);
        }
    }

    #[test]
    fn join_all_keeps_a_later_linked_audio_choice_boundary() {
        let mut q = seq_with(vec![item(10, 0, 30, 0), item(11, 30, 10, 30), item(12, 40, 20, 40)]);
        q.audio_tracks[0].items = vec![item(20, 0, 30, 0), item(21, 30, 10, 30), item(22, 40, 20, 40)];
        for (index, link) in [100, 200, 300].into_iter().enumerate() {
            q.video_tracks[0].items[index].link = Some(link);
            q.audio_tracks[0].items[index].link = Some(link);
        }
        q.audio_tracks[0].items[2].source_channels = vec![1];
        assert_eq!(join_through_edits(&mut q, &[]), 2, "only the first video/audio cuts may heal");
        assert_eq!(q.video_tracks[0].items.len(), 2);
        assert_eq!(q.audio_tracks[0].items.len(), 2);
        assert_eq!(q.video_tracks[0].items[1].link, q.audio_tracks[0].items[1].link);
    }

    #[test]
    fn audio_only_heal_then_join_all_keeps_later_video_choice_boundary() {
        let mut q = seq_with(vec![item(10, 0, 30, 0), item(11, 30, 10, 30), item(12, 40, 20, 40)]);
        q.audio_tracks[0].items = vec![item(20, 0, 30, 0), item(21, 30, 10, 30), item(22, 40, 20, 40)];
        for (index, link) in [100, 200, 300].into_iter().enumerate() {
            q.video_tracks[0].items[index].link = Some(link);
            q.audio_tracks[0].items[index].link = Some(link);
        }
        q.audio_tracks[0].items[2].source_channels = vec![1];
        assert_eq!(join_through_edits(&mut q, &[ClipId(20)]), 1, "audio-only heal must leave the video pieces split");
        assert_eq!(q.video_tracks[0].items.len(), 3);
        assert_eq!(q.audio_tracks[0].items.len(), 2);
        assert_eq!(join_through_edits(&mut q, &[]), 1, "later choice boundary must remain protected");
        assert_eq!(q.video_tracks[0].items.len(), 2);
        assert_eq!(q.video_tracks[0].items[1].id, ClipId(12));
        assert_eq!(q.audio_tracks[0].items[1].id, ClipId(22));
        assert_eq!(q.audio_tracks[0].items[1].source_channels, vec![1]);
    }

    #[test]
    fn unrelated_audio_boundary_does_not_block_link_preserving_video_join() {
        let mut q = seq_with(vec![item(10, 0, 10, 0), item(11, 10, 10, 10)]);
        for video in &mut q.video_tracks[0].items {
            video.link = Some(7);
        }
        let mut unrelated = item(20, 0, 4, 0);
        unrelated.item = ItemId(2);
        unrelated.source_channels = vec![0];
        let mut sound = item(21, 5, 15, 5);
        sound.source_channels = vec![1];
        sound.link = Some(7);
        q.audio_tracks[0].items = vec![unrelated, sound];
        assert!(q.check().is_ok());
        assert_eq!(join_through_edits(&mut q, &[]), 1);
        assert_eq!(q.video_tracks[0].items.len(), 1);
        assert_eq!(q.video_tracks[0].items[0].link, Some(7));
        assert_eq!(q.audio_tracks[0].items.len(), 2);
        assert_eq!(q.audio_tracks[0].items[1].source_channels, vec![1]);
    }

    #[test]
    fn gapped_audio_choice_still_protects_its_linked_video_cut() {
        let mut q = seq_with(vec![item(10, 0, 30, 0), item(11, 30, 30, 30)]);
        q.audio_tracks[0].items = vec![item(20, 0, 25, 0), item(21, 30, 30, 30)];
        for (index, link) in [100, 200].into_iter().enumerate() {
            q.video_tracks[0].items[index].link = Some(link);
            q.audio_tracks[0].items[index].link = Some(link);
        }
        q.audio_tracks[0].items[1].source_channels = vec![1];
        let before = q.clone();
        assert_eq!(join_through_edits(&mut q, &[]), 0);
        assert_eq!(q, before);
    }

    #[test]
    fn moved_audio_right_piece_keeps_its_linked_video_boundary() {
        let mut q = seq_with(vec![item(10, 0, 30, 0), item(11, 30, 30, 30)]);
        q.audio_tracks[0].items = vec![item(20, 0, 30, 0), item(21, 30, 30, 30)];
        for (index, link) in [100, 200].into_iter().enumerate() {
            q.video_tracks[0].items[index].link = Some(link);
            q.audio_tracks[0].items[index].link = Some(link);
        }
        q.audio_tracks[0].items[1].source_channels = vec![1];
        let mut moved = q.audio_tracks[0].clone();
        moved.id = TrackId(999);
        moved.name = "A2".into();
        moved.items = vec![q.audio_tracks[0].items.remove(1)];
        q.audio_tracks.push(moved);
        assert!(q.check().is_ok());
        let before = q.clone();
        assert!(joinable_through_edits(&q).is_empty());
        assert_eq!(join_through_edits(&mut q, &[]), 0);
        assert_eq!(q, before);
    }

    #[test]
    fn locked_audio_keeps_its_linked_video_boundary() {
        let mut q = seq_with(vec![item(10, 0, 30, 0), item(11, 30, 30, 30)]);
        q.audio_tracks[0].items = vec![item(20, 0, 30, 0), item(21, 30, 30, 30)];
        for (index, link) in [100, 200].into_iter().enumerate() {
            q.video_tracks[0].items[index].link = Some(link);
            q.audio_tracks[0].items[index].link = Some(link);
        }
        q.audio_tracks[0].locked = true;
        let before = q.clone();
        assert!(joinable_through_edits(&q).is_empty());
        assert_eq!(join_through_edits(&mut q, &[]), 0);
        assert_eq!(q, before);
    }

    #[test]
    fn video_only_join_keeps_an_unselected_linked_audio_piece() {
        let mut q = seq_with(vec![item(10, 0, 30, 0), item(11, 30, 30, 30)]);
        q.audio_tracks[0].items = vec![item(20, 0, 30, 0), item(21, 30, 30, 30)];
        for (index, link) in [100, 200].into_iter().enumerate() {
            q.video_tracks[0].items[index].link = Some(link);
            q.audio_tracks[0].items[index].link = Some(link);
        }
        let before = q.clone();
        assert_eq!(join_through_edits(&mut q, &[ClipId(10)]), 0);
        assert_eq!(q, before);
    }

    #[test]
    fn audio_only_join_does_not_link_distinct_surviving_video_pieces() {
        let mut q = seq_with(vec![item(10, 0, 30, 0), item(11, 30, 30, 30)]);
        q.video_tracks[0].items[0].link = Some(100);
        q.video_tracks[0].items[1].link = Some(200);
        q.audio_tracks[0].items = vec![item(20, 0, 30, 0), item(21, 30, 30, 30)];
        q.audio_tracks[0].items[0].link = Some(100);
        q.audio_tracks[0].items[1].link = Some(200);
        assert_eq!(join_through_edits(&mut q, &[ClipId(20)]), 1);
        assert_eq!(q.video_tracks[0].items[0].link, Some(100));
        assert_eq!(q.video_tracks[0].items[1].link, Some(200));
    }

    #[test]
    fn audio_metadata_on_video_does_not_block_a_continuous_video_join() {
        let left = item(10, 0, 30, 0);
        let mut right = item(11, 30, 30, 30);
        right.source_channels = vec![1];
        let mut q = seq_with(vec![left, right]);
        // A malformed project can duplicate track IDs; storage still defines the track kind.
        q.audio_tracks[0].id = q.video_tracks[0].id;
        assert_eq!(track_through_edits(&q.video_tracks[0]).len(), 1);
        assert_eq!(join_through_edits(&mut q, &[]), 1);
    }

    #[test]
    fn join_only_listed_edits() {
        let mut q = seq_with(vec![item(10, 0, 30, 0), item(11, 30, 30, 30), item(12, 60, 30, 60)]);
        assert_eq!(join_through_edits(&mut q, &[ClipId(12)]), 1);
        assert_eq!(q.video_tracks[0].items.len(), 2);
        assert_eq!(q.video_tracks[0].items[1].duration, Tick(60));
    }

    #[test]
    fn gaps_on_tracks_and_sequence() {
        let mut q = seq_with(vec![item(10, 10, 20, 0), item(11, 50, 10, 0)]);
        q.audio_tracks[0].items = vec![item(20, 0, 15, 0), item(21, 40, 15, 0)];
        assert_eq!(track_gaps(&q.video_tracks[0]), vec![TimeRange::from_bounds(Tick(0), Tick(10)), TimeRange::from_bounds(Tick(30), Tick(50))]);
        assert_eq!(sequence_gaps(&q), vec![TimeRange::from_bounds(Tick(30), Tick(40))]);
        assert_eq!(q.audio_tracks[0].kind, TrackKind::Audio);
    }

    #[test]
    fn clip_to_range_keeps_frames_in_place() {
        let it = item(10, 100, 100, 1000);
        let c = clip_item_to(&it, TimeRange::from_bounds(Tick(120), Tick(150))).unwrap();
        assert_eq!((c.start, c.duration, c.source_in), (Tick(120), Tick(30), Tick(1020)));
        assert_eq!(c.source_time_at(Tick(130)), it.source_time_at(Tick(130)));
        let mut r = it.clone();
        r.reverse = true;
        let c = clip_item_to(&r, TimeRange::from_bounds(Tick(120), Tick(150))).unwrap();
        assert_eq!(c.source_time_at(Tick(130)), r.source_time_at(Tick(130)));
        assert!(clip_item_to(&it, TimeRange::from_bounds(Tick(0), Tick(100))).is_none());
    }
}

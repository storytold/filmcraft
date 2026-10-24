//! Caption edits.
//!
//! Captions live on caption tracks ([`CaptionTrack`]); like track items they never overlap. Every
//! function validates first and returns an error leaving the sequence untouched, or applies the
//! whole edit. Locked caption tracks refuse edits. Sync-locked caption tracks follow insert and
//! extract edits on the media tracks (see [`crate::insert`] / [`crate::extract`]).

use filmcraft_project::{Caption, CaptionTrack, ClipId, Sequence, TrackId};
use filmcraft_time::{Tick, TimeRange};

use crate::{Edge, EditCtx, EditError, Result};

/// Default duration of a new caption (Premiere uses about three seconds).
pub const DEFAULT_SECONDS: i64 = 3;

fn track_mut(seq: &mut Sequence, id: TrackId) -> Result<&mut CaptionTrack> {
    let t = seq.caption_track_mut(id).ok_or(EditError::NoTrack(id))?;
    if t.locked {
        return Err(EditError::Locked);
    }
    Ok(t)
}

fn owner(seq: &Sequence, id: ClipId) -> Result<TrackId> {
    seq.find_caption(id).map(|(t, _)| t).ok_or(EditError::NoItem(id))
}

/// Previous end / next start around caption `id` on its track.
fn neighbours(t: &CaptionTrack, id: ClipId) -> (Tick, Tick) {
    let i = t.captions.iter().position(|c| c.id == id).unwrap_or(0);
    let prev = if i > 0 { t.captions[i - 1].end() } else { Tick::ZERO };
    let next = t.captions.get(i + 1).map(|c| c.start).unwrap_or(Tick::MAX);
    (prev, next)
}

/// Add a caption at `start`. The duration is cut short at the next caption; fails if a caption
/// already covers `start` or less than one frame would remain.
pub fn add_caption(seq: &mut Sequence, track: TrackId, start: Tick, duration: Tick, text: &str, ctx: &mut EditCtx) -> Result<ClipId> {
    let start = start.max(Tick::ZERO);
    let t = track_mut(seq, track)?;
    if t.caption_at(start).is_some() {
        return Err(EditError::Other("a caption already covers this time".into()));
    }
    let next = t.captions.iter().map(|c| c.start).filter(|s| *s > start).min().unwrap_or(Tick::MAX);
    let dur = duration.min(next - start);
    if dur < ctx.min_duration {
        return Err(EditError::TooShort);
    }
    let id = ClipId(ctx.alloc());
    t.captions.push(Caption { id, start, duration: dur, text: text.to_string(), speaker: None, cue_id: None, settings: String::new() });
    t.sort();
    Ok(id)
}

/// Split caption `id` at `t` (strictly inside it); both halves keep the text. Returns the new
/// (right) caption.
pub fn split_caption(seq: &mut Sequence, id: ClipId, t: Tick, ctx: &mut EditCtx) -> Result<ClipId> {
    let tid = owner(seq, id)?;
    let min = ctx.min_duration;
    let new_id = ClipId(ctx.alloc());
    let tr = track_mut(seq, tid)?;
    let c = tr.caption_mut(id).ok_or(EditError::NoItem(id))?;
    if t - c.start < min || c.end() - t < min {
        return Err(EditError::TooShort);
    }
    let mut right = c.clone();
    right.id = new_id;
    right.start = t;
    right.duration = c.end() - t;
    right.cue_id = None;
    c.duration = t - c.start;
    tr.captions.push(right);
    tr.sort();
    Ok(new_id)
}

/// Split every caption strictly containing `t` on unlocked caption tracks (all when `tracks` is
/// empty). Returns the new captions.
pub fn split_captions_at(seq: &mut Sequence, tracks: &[TrackId], t: Tick, ctx: &mut EditCtx) -> Vec<ClipId> {
    let ids: Vec<ClipId> = seq
        .caption_tracks
        .iter()
        .filter(|tr| !tr.locked && (tracks.is_empty() || tracks.contains(&tr.id)))
        .filter_map(|tr| tr.caption_at(t).filter(|c| c.start < t).map(|c| c.id))
        .collect();
    ids.into_iter().filter_map(|id| split_caption(seq, id, t, ctx).ok()).collect()
}

/// Merge captions on one track into the first: it spans from the first start to the last end and
/// joins the texts with line breaks. The captions must be consecutive (none in between).
pub fn merge_captions(seq: &mut Sequence, ids: &[ClipId]) -> Result<ClipId> {
    if ids.len() < 2 {
        return Err(EditError::Nothing);
    }
    let tid = owner(seq, ids[0])?;
    for id in ids {
        if owner(seq, *id)? != tid {
            return Err(EditError::Other("captions to merge must be on one track".into()));
        }
    }
    let tr = track_mut(seq, tid)?;
    let mut idx: Vec<usize> = ids.iter().filter_map(|id| tr.captions.iter().position(|c| c.id == *id)).collect();
    idx.sort_unstable();
    idx.dedup();
    if idx.windows(2).any(|w| w[1] != w[0] + 1) {
        return Err(EditError::Other("captions to merge must be next to each other".into()));
    }
    let (Some(&first), Some(&last)) = (idx.first(), idx.last()) else {
        return Err(EditError::Nothing);
    };
    let end = tr.captions[last].end();
    let text: Vec<String> = idx.iter().map(|&i| tr.captions[i].text.trim().to_string()).filter(|t| !t.is_empty()).collect();
    let keep = tr.captions[first].id;
    let c = &mut tr.captions[first];
    c.duration = end - c.start;
    c.text = text.join("\n");
    tr.captions.drain(first + 1..=last);
    Ok(keep)
}

/// Change a caption's text and/or speaker.
pub fn set_caption(seq: &mut Sequence, id: ClipId, text: Option<&str>, speaker: Option<Option<&str>>) -> Result<()> {
    let tid = owner(seq, id)?;
    let c = track_mut(seq, tid)?.caption_mut(id).ok_or(EditError::NoItem(id))?;
    if let Some(t) = text {
        c.text = t.replace("\r\n", "\n");
    }
    if let Some(s) = speaker {
        c.speaker = s.map(str::trim).filter(|s| !s.is_empty()).map(str::to_string);
    }
    Ok(())
}

/// Set a caption's in and out points directly (Captions panel timecode fields).
pub fn set_caption_times(seq: &mut Sequence, id: ClipId, start: Tick, end: Tick, ctx: &EditCtx) -> Result<()> {
    let tid = owner(seq, id)?;
    let tr = track_mut(seq, tid)?;
    if end - start < ctx.min_duration || start < Tick::ZERO {
        return Err(EditError::TooShort);
    }
    if tr.captions.iter().any(|c| c.id != id && c.start < end && start < c.end()) {
        return Err(EditError::Other("captions would overlap".into()));
    }
    let c = tr.caption_mut(id).ok_or(EditError::NoItem(id))?;
    c.start = start;
    c.duration = end - start;
    tr.sort();
    Ok(())
}

/// Trim a caption edge by `delta`, limited by its neighbours and a one-frame minimum. Returns the
/// delta applied.
pub fn trim_caption(seq: &mut Sequence, id: ClipId, edge: Edge, delta: Tick, ctx: &EditCtx) -> Result<Tick> {
    let tid = owner(seq, id)?;
    let tr = track_mut(seq, tid)?;
    let (prev, next) = neighbours(tr, id);
    let c = tr.caption_mut(id).ok_or(EditError::NoItem(id))?;
    let d = match edge {
        Edge::In => delta.clamp(prev - c.start, c.duration - ctx.min_duration),
        Edge::Out => delta.clamp(ctx.min_duration - c.duration, next - c.end()),
    };
    if d == Tick::ZERO {
        return Err(EditError::Nothing);
    }
    match edge {
        Edge::In => {
            c.start += d;
            c.duration -= d;
        }
        Edge::Out => c.duration += d,
    }
    Ok(d)
}

/// Move captions by `delta`, clamped so they stay at or after zero and do not overlap captions
/// that are not moving. Returns the delta applied.
pub fn move_captions(seq: &mut Sequence, ids: &[ClipId], delta: Tick) -> Result<Tick> {
    let mut lo = Tick::MIN;
    let mut hi = Tick::MAX;
    let mut tracks = Vec::new();
    for id in ids {
        let tid = owner(seq, *id)?;
        let tr = seq.caption_track(tid).ok_or(EditError::NoTrack(tid))?;
        if tr.locked {
            return Err(EditError::Locked);
        }
        let c = tr.caption(*id).ok_or(EditError::NoItem(*id))?;
        lo = lo.max(-c.start);
        // nearest non-moving neighbours on each side
        for o in tr.captions.iter().filter(|o| !ids.contains(&o.id)) {
            if o.end() <= c.start {
                lo = lo.max(o.end() - c.start);
            } else if o.start >= c.end() {
                hi = hi.min(o.start - c.end());
            } else {
                return Err(EditError::Other("captions overlap".into()));
            }
        }
        if !tracks.contains(&tid) {
            tracks.push(tid);
        }
    }
    // a gap between moving and non-moving captions is jumped over only when it fits entirely
    let d = delta.clamp(lo.min(Tick::ZERO), hi.max(Tick::ZERO));
    if d == Tick::ZERO {
        return Err(EditError::Nothing);
    }
    for tid in tracks {
        let Some(tr) = seq.caption_track_mut(tid) else { continue };
        for c in tr.captions.iter_mut().filter(|c| ids.contains(&c.id)) {
            c.start += d;
        }
        tr.sort();
    }
    Ok(d)
}

/// Place copies of `captions` on `track`, overwriting what lies under them (captions there are cut
/// back, split or removed), as Paste and Option-drag do. The copies keep their text, speaker and
/// settings at their own `start`/`duration`, and get new ids (returned in time order).
pub fn overwrite_captions(seq: &mut Sequence, track: TrackId, captions: &[Caption], ctx: &mut EditCtx) -> Result<Vec<ClipId>> {
    let min = ctx.min_duration;
    let mut placed: Vec<Caption> = captions.iter().filter(|c| c.duration >= min).cloned().collect();
    if placed.is_empty() {
        return Err(EditError::Nothing);
    }
    placed.sort_by_key(|c| c.start);
    if placed.first().is_some_and(|c| c.start < Tick::ZERO) {
        return Err(EditError::Other("captions cannot start before the sequence".into()));
    }
    if placed.windows(2).any(|w| w[0].end() > w[1].start) {
        return Err(EditError::Other("captions overlap".into()));
    }
    let tr = track_mut(seq, track)?;
    for c in &mut placed {
        c.id = ClipId(ctx.alloc());
        c.cue_id = None;
    }
    for c in &placed {
        clear_range(tr, c.start, c.end(), min, ctx);
    }
    let ids = placed.iter().map(|c| c.id).collect();
    tr.captions.extend(placed);
    tr.sort();
    Ok(ids)
}

/// Clear `a..b` on a caption track without closing the gap: captions inside go, captions across an
/// edge are cut back, a caption across the whole range is split. Pieces shorter than `min` go.
fn clear_range(tr: &mut CaptionTrack, a: Tick, b: Tick, min: Tick, ctx: &mut EditCtx) {
    let mut out = Vec::with_capacity(tr.captions.len() + 1);
    for c in tr.captions.drain(..) {
        let (s, e) = (c.start, c.end());
        if e <= a || s >= b {
            out.push(c);
            continue;
        }
        let mut kept_left = false;
        if s < a && a - s >= min {
            let mut l = c.clone();
            l.duration = a - s;
            out.push(l);
            kept_left = true;
        }
        if e > b && e - b >= min {
            let mut r = c;
            if kept_left {
                r.id = ClipId(ctx.alloc());
                r.cue_id = None;
            }
            r.start = b;
            r.duration = e - b;
            out.push(r);
        }
    }
    tr.captions = out;
}

/// Delete captions. With `ripple`, later captions on the same track move left to close each gap.
pub fn delete_captions(seq: &mut Sequence, ids: &[ClipId], ripple: bool) -> Result<usize> {
    let mut n = 0;
    for id in ids {
        let tid = owner(seq, *id)?;
        if seq.caption_track(tid).is_some_and(|t| t.locked) {
            return Err(EditError::Locked);
        }
    }
    for tr in seq.caption_tracks.iter_mut().filter(|t| !t.locked) {
        let mut removed: Vec<TimeRange> = tr.captions.iter().filter(|c| ids.contains(&c.id)).map(Caption::range).collect();
        if removed.is_empty() {
            continue;
        }
        n += removed.len();
        tr.captions.retain(|c| !ids.contains(&c.id));
        if ripple {
            removed.sort_by_key(|r| std::cmp::Reverse(r.start));
            for r in removed {
                shift_from(tr, r.end(), -r.duration);
            }
        }
    }
    Ok(n)
}

/// Shift every caption starting at or after `at` by `delta`.
pub fn shift_from(tr: &mut CaptionTrack, at: Tick, delta: Tick) {
    for c in tr.captions.iter_mut().filter(|c| c.start >= at) {
        c.start += delta;
    }
}

/// Open a gap of `dur` at `at` (insert edit): a caption straddling `at` is split.
pub fn insert_gap(tr: &mut CaptionTrack, at: Tick, dur: Tick, ctx: &mut EditCtx) {
    if let Some(c) = tr.caption_at(at).filter(|c| c.start < at).cloned() {
        let mut right = c.clone();
        right.id = ClipId(ctx.alloc());
        right.start = at;
        right.duration = c.end() - at;
        right.cue_id = None;
        if let Some(l) = tr.caption_mut(c.id) {
            l.duration = at - c.start;
        }
        tr.captions.push(right);
        tr.sort();
    }
    shift_from(tr, at, dur);
}

/// Remove `range` from a caption track (captions inside go, captions across it are cut) and close
/// the gap (extract).
pub fn extract_range(tr: &mut CaptionTrack, range: TimeRange) {
    let (a, b) = (range.start, range.end());
    let mut out = Vec::with_capacity(tr.captions.len());
    for mut c in tr.captions.drain(..) {
        let (s, e) = (c.start, c.end());
        if e <= a {
            out.push(c);
        } else if s >= b {
            c.start = s - range.duration;
            out.push(c);
        } else {
            // overlaps the range: keep what lies outside it, joined across the closed gap
            let left = (a - s).max(Tick::ZERO);
            let right = (e - b).max(Tick::ZERO);
            if left + right > Tick::ZERO {
                c.start = s.min(a);
                c.duration = left + right;
                out.push(c);
            }
        }
    }
    tr.captions = out;
    tr.sort();
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_project::CaptionFormat;

    fn seq_with(captions: &[(i64, i64, &str)]) -> (Sequence, TrackId) {
        let mut p = filmcraft_project::Project::new("t");
        let sid = p.new_sequence("s", Default::default(), 1, 1, None);
        let mut seq = p.sequence(sid).unwrap().clone();
        let tid = TrackId(900);
        let mut tr = CaptionTrack::new(tid, "Subtitle".into(), CaptionFormat::Subtitle);
        for (i, (s, d, t)) in captions.iter().enumerate() {
            tr.captions.push(Caption {
                id: ClipId(1000 + i as u64),
                start: Tick(*s),
                duration: Tick(*d),
                text: t.to_string(),
                speaker: None,
                cue_id: None,
                settings: String::new(),
            });
        }
        seq.caption_tracks.push(tr);
        (seq, tid)
    }

    fn ctx(next: &mut u64) -> EditCtx<'_> {
        static NONE: fn(filmcraft_project::ItemId) -> Option<Tick> = |_| None;
        EditCtx { next_id: next, media_duration: &NONE, media_start: &|_| Tick::ZERO, min_duration: Tick(10) }
    }

    fn spans(seq: &Sequence, tid: TrackId) -> Vec<(i64, i64, String)> {
        seq.caption_track(tid).unwrap().captions.iter().map(|c| (c.start.0, c.end().0, c.text.clone())).collect()
    }

    #[test]
    fn add_clamps_to_next() {
        let (mut seq, tid) = seq_with(&[(100, 50, "b")]);
        let mut n = 1;
        let id = add_caption(&mut seq, tid, Tick(40), Tick(1000), "a", &mut ctx(&mut n)).unwrap();
        assert_eq!(seq.find_caption(id).unwrap().1.end(), Tick(100));
        assert!(add_caption(&mut seq, tid, Tick(120), Tick(10), "x", &mut ctx(&mut n)).is_err());
        assert!(add_caption(&mut seq, tid, Tick(95), Tick(10), "x", &mut ctx(&mut n)).is_err(), "would be shorter than min");
        assert!(seq.check().is_ok());
    }

    #[test]
    fn split_and_merge() {
        let (mut seq, tid) = seq_with(&[(0, 100, "hello"), (100, 50, "world")]);
        let mut n = 1;
        let r = split_caption(&mut seq, ClipId(1000), Tick(40), &mut ctx(&mut n)).unwrap();
        assert_eq!(spans(&seq, tid), vec![(0, 40, "hello".into()), (40, 100, "hello".into()), (100, 150, "world".into())]);
        assert!(split_caption(&mut seq, r, Tick(45), &mut ctx(&mut n)).is_err());
        let m = merge_captions(&mut seq, &[r, ClipId(1001)]).unwrap();
        assert_eq!(m, r);
        assert_eq!(spans(&seq, tid), vec![(0, 40, "hello".into()), (40, 150, "hello\nworld".into())]);
        // not adjacent
        let (mut seq, _) = seq_with(&[(0, 10, "a"), (20, 10, "b"), (40, 10, "c")]);
        assert!(merge_captions(&mut seq, &[ClipId(1000), ClipId(1002)]).is_err());
    }

    #[test]
    fn trim_and_move_clamp() {
        let (mut seq, tid) = seq_with(&[(0, 100, "a"), (150, 50, "b"), (300, 50, "c")]);
        let n = &mut 1;
        assert_eq!(trim_caption(&mut seq, ClipId(1001), Edge::In, Tick(-500), &ctx(n)).unwrap(), Tick(-50));
        assert_eq!(trim_caption(&mut seq, ClipId(1001), Edge::Out, Tick(500), &ctx(n)).unwrap(), Tick(100));
        assert_eq!(spans(&seq, tid)[1], (100, 300, "b".into()));
        assert_eq!(trim_caption(&mut seq, ClipId(1001), Edge::Out, Tick(-1000), &ctx(n)).unwrap(), Tick(-190));
        // move c left: stops at b's end (110)
        assert_eq!(move_captions(&mut seq, &[ClipId(1002)], Tick(-1000)).unwrap(), Tick(-190));
        assert_eq!(spans(&seq, tid)[2], (110, 160, "c".into()));
        // a cannot move right (b is flush against it); b and c move together freely
        assert_eq!(move_captions(&mut seq, &[ClipId(1000)], Tick(5)), Err(EditError::Nothing));
        assert_eq!(move_captions(&mut seq, &[ClipId(1001), ClipId(1002)], Tick(5)).unwrap(), Tick(5));
        assert_eq!(spans(&seq, tid)[1..], [(105, 115, "b".into()), (115, 165, "c".into())]);
        assert!(seq.check().is_ok());
    }

    #[test]
    fn delete_ripple_and_sync() {
        let (mut seq, tid) = seq_with(&[(0, 100, "a"), (100, 50, "b"), (200, 50, "c")]);
        delete_captions(&mut seq, &[ClipId(1001)], true).unwrap();
        assert_eq!(spans(&seq, tid), vec![(0, 100, "a".into()), (150, 200, "c".into())]);
        let mut n = 1;
        let tr = seq.caption_track_mut(tid).unwrap();
        insert_gap(tr, Tick(50), Tick(30), &mut ctx(&mut n));
        assert_eq!(spans(&seq, tid), vec![(0, 50, "a".into()), (80, 130, "a".into()), (180, 230, "c".into())]);
        let tr = seq.caption_track_mut(tid).unwrap();
        extract_range(tr, TimeRange::new(Tick(40), Tick(150)));
        assert_eq!(spans(&seq, tid), vec![(0, 40, "a".into()), (40, 80, "c".into())]);
    }

    #[test]
    fn overwrite_cuts_splits_and_removes_what_is_under() {
        let (mut seq, tid) = seq_with(&[(0, 100, "a"), (100, 50, "b"), (200, 100, "c")]);
        let mut n = 1;
        let copy = |s: i64, d: i64, t: &str| Caption {
            id: ClipId(1),
            start: Tick(s),
            duration: Tick(d),
            text: t.into(),
            speaker: None,
            cue_id: Some("x".into()),
            settings: String::new(),
        };
        // across a's tail and all of b; then inside c (c is split around it)
        let ids = overwrite_captions(&mut seq, tid, &[copy(60, 95, "x"), copy(230, 40, "y")], &mut ctx(&mut n)).unwrap();
        assert_eq!(ids.len(), 2);
        assert_eq!(spans(&seq, tid), vec![(0, 60, "a".into()), (60, 155, "x".into()), (200, 230, "c".into()), (230, 270, "y".into()), (270, 300, "c".into())]);
        let tr = seq.caption_track(tid).unwrap();
        assert!(tr.captions.iter().all(|c| c.cue_id.is_none()));
        let seen: std::collections::HashSet<u64> = tr.captions.iter().map(|c| c.id.0).collect();
        assert_eq!(seen.len(), 5, "every piece has its own id");
        assert!(seq.check().is_ok());
        // a sliver under one frame is dropped, not kept
        let (mut seq, tid) = seq_with(&[(0, 100, "a")]);
        overwrite_captions(&mut seq, tid, &[copy(5, 90, "x")], &mut ctx(&mut n)).unwrap();
        assert_eq!(spans(&seq, tid), vec![(5, 95, "x".into())]);
        // hostile input: before zero, overlapping copies, nothing
        assert!(overwrite_captions(&mut seq, tid, &[copy(-5, 50, "x")], &mut ctx(&mut n)).is_err());
        assert!(overwrite_captions(&mut seq, tid, &[copy(0, 50, "x"), copy(40, 50, "y")], &mut ctx(&mut n)).is_err());
        assert_eq!(overwrite_captions(&mut seq, tid, &[], &mut ctx(&mut n)), Err(EditError::Nothing));
        seq.caption_track_mut(tid).unwrap().locked = true;
        assert_eq!(overwrite_captions(&mut seq, tid, &[copy(0, 50, "x")], &mut ctx(&mut n)), Err(EditError::Locked));
    }

    #[test]
    fn locked_refuses() {
        let (mut seq, tid) = seq_with(&[(0, 100, "a")]);
        seq.caption_track_mut(tid).unwrap().locked = true;
        assert_eq!(set_caption(&mut seq, ClipId(1000), Some("x"), None), Err(EditError::Locked));
        assert_eq!(delete_captions(&mut seq, &[ClipId(1000)], false), Err(EditError::Locked));
    }
}

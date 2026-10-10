//! Timeline edit algebra.
//!
//! Every function takes a `&mut Sequence` (the engine passes a copy-on-write clone) and either
//! applies the whole edit or returns an error leaving the sequence untouched (functions validate
//! first, or work on a clone). Track items never overlap afterwards (checked in tests).
//!
//! Semantics follow Premiere Pro:
//! - **Overwrite** replaces whatever is under the new item on its track.
//! - **Insert** pushes material right on the target tracks *and every sync-locked, unlocked track*.
//! - **Lift** leaves a gap; **Extract** closes it (ripple) on targeted + sync-locked tracks.
//! - **Ripple delete** removes items and closes the gap; fails if a sync-locked track has material in
//!   the gap (it would lose sync).
//! - Trims: regular, ripple, roll, slip, slide and rate stretch, all limited by media handles and
//!   neighbouring items.
//!
//! Keyframes are stored in media time, so trims and splits never need to move them.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod captions;
pub mod frame_size;
pub mod multicam;
pub mod through;
pub mod transcript;
pub mod transitions;

use std::collections::HashMap;

use filmcraft_project::{ClipId, ItemId, Sequence, Track, TrackId, TrackItem, Transition, TransitionId};
use filmcraft_time::{Tick, TimeRange};

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum EditError {
    #[error("no such track item {0:?}")]
    NoItem(ClipId),
    #[error("no such track {0:?}")]
    NoTrack(TrackId),
    #[error("track is locked")]
    Locked,
    /// Names the sync-locked track (`A2`) whose clip is in the way.
    #[error("this edit would break sync: {0} is sync-locked and has a clip in the way (turn off its sync lock, or lock the track, to make this edit)")]
    SyncLockConflict(String),
    #[error("not enough media (handles) for this trim")]
    NoHandles,
    #[error("edit would make a clip shorter than one frame")]
    TooShort,
    #[error("nothing to do")]
    Nothing,
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, EditError>;

/// Supplies fresh ids and media durations to edits.
pub struct EditCtx<'a> {
    pub next_id: &'a mut u64,
    /// Media duration of a project item (None = unlimited, e.g. stills / adjustment layers): the
    /// latest media time its clips may show.
    pub media_duration: &'a dyn Fn(ItemId) -> Option<Tick>,
    /// The earliest media time clips of a project item may show (a subclip that restricts trims
    /// starts at its In point; everything else at 0).
    pub media_start: &'a dyn Fn(ItemId) -> Tick,
    /// Minimum item duration (one sequence frame).
    pub min_duration: Tick,
}

impl EditCtx<'_> {
    pub fn alloc(&mut self) -> u64 {
        let v = *self.next_id;
        *self.next_id += 1;
        v
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Edge {
    In,
    Out,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TrimMode {
    Regular,
    Ripple,
}

// ---------------------------------------------------------------------------------------------
// Track-level primitives
// ---------------------------------------------------------------------------------------------

/// Split the item strictly containing `t` into two. Returns the new (right) item's id.
/// `links` maps old link groups to the new group for right-hand pieces (so linked partners split
/// in the same operation stay linked to each other).
pub fn split_track_at(track: &mut Track, t: Tick, ctx: &mut EditCtx, links: &mut HashMap<u64, u64>) -> Option<ClipId> {
    let idx = track.items.iter().position(|i| i.start < t && t < i.end())?;
    let left = &mut track.items[idx];
    let mut right = left.clone();
    let cut = t - left.start;
    left.duration = cut;
    let new_id = ClipId(ctx.alloc());
    right.id = new_id;
    right.start = t;
    right.duration -= cut;
    if !right.reverse && right.frame_hold.is_none() {
        right.source_in += Tick((cut.0 as f64 * right.speed.abs()).round() as i64);
    } else if right.reverse {
        // reversed: the left piece now shows the later media; keep content continuous
        let left_ref = &mut track.items[idx];
        let consumed = Tick((cut.0 as f64 * left_ref.speed.abs()).round() as i64);
        let total_src = Tick(((cut + right.duration).0 as f64 * left_ref.speed.abs()).round() as i64);
        right.source_in = left_ref.source_in;
        left_ref.source_in += total_src - consumed;
    }
    if let Some(l) = right.link {
        let nl = *links.entry(l).or_insert_with(|| ctx.alloc());
        right.link = Some(nl);
    }
    // transitions: those after the cut that referenced the left piece now reference the right one;
    // a transition spanning the cut on this clip's interior is removed.
    let old_id = track.items[idx].id;
    track
        .transitions
        .retain(|tr| !(tr.start < t && t < tr.end() && (tr.from == Some(old_id) || tr.to == Some(old_id)) && !(tr.from.is_some() && tr.to.is_some())));
    for tr in &mut track.transitions {
        if tr.start >= t && tr.from == Some(old_id) {
            tr.from = Some(new_id);
        }
    }
    track.items.insert(idx + 1, right);
    Some(new_id)
}

/// Remove all material in `range` on a track (splitting items at the boundaries).
pub fn clear_track_range(track: &mut Track, range: TimeRange, ctx: &mut EditCtx, links: &mut HashMap<u64, u64>) -> Vec<ClipId> {
    if range.is_empty() {
        return Vec::new();
    }
    split_track_at(track, range.start, ctx, links);
    split_track_at(track, range.end(), ctx, links);
    let mut removed = Vec::new();
    track.items.retain(|i| {
        let inside = i.start >= range.start && i.end() <= range.end();
        if inside {
            removed.push(i.id);
        }
        !inside
    });
    remove_orphan_transitions(track);
    removed
}

/// Shift every item starting at or after `at` by `delta`.
pub fn shift_track_from(track: &mut Track, at: Tick, delta: Tick) {
    for i in &mut track.items {
        if i.start >= at {
            i.start += delta;
        }
    }
    for tr in &mut track.transitions {
        if tr.start >= at || (tr.end() > at && tr.from.is_none()) {
            tr.start += delta;
        }
    }
    track.sort();
}

/// Open a gap of `dur` at `at` (splitting an item that spans `at`).
pub fn insert_track_gap(track: &mut Track, at: Tick, dur: Tick, ctx: &mut EditCtx, links: &mut HashMap<u64, u64>) {
    split_track_at(track, at, ctx, links);
    shift_track_from(track, at, dur);
}

/// Whether `range` holds no items on a track.
pub fn track_range_empty(track: &Track, range: TimeRange) -> bool {
    !track.items.iter().any(|i| i.range().overlaps(&range))
}

/// Drop transitions whose clips no longer exist or are no longer adjacent to them.
pub fn remove_orphan_transitions(track: &mut Track) {
    let ids: HashMap<ClipId, (Tick, Tick)> = track.items.iter().map(|i| (i.id, (i.start, i.end()))).collect();
    track.transitions.retain(|tr| {
        let from_ok = tr.from.is_none_or(|f| ids.contains_key(&f));
        let to_ok = tr.to.is_none_or(|t| ids.contains_key(&t));
        from_ok && to_ok && (tr.from.is_some() || tr.to.is_some())
    });
}

/// Where a transition sits: the incoming clip's start, or the outgoing clip's end for a fade to nothing.
fn transition_anchor(track: &Track, tr: &Transition) -> Option<Tick> {
    match (tr.from, tr.to) {
        (_, Some(to)) => track.item(to).map(|i| i.start),
        (Some(from), None) => track.item(from).map(|i| i.end()),
        (None, None) => None,
    }
}

/// Keep transitions on their edit points after an edit moved cuts (ripple trim, roll, slide, ripple
/// speed change): each transition shifts by as much as its anchor clip edge moved.
fn transitions_follow_cuts(before: &Sequence, after: &mut Sequence) {
    for track in after.all_tracks_mut() {
        let Some(old) = before.track(track.id) else { continue };
        let shifts: Vec<Tick> = track
            .transitions
            .iter()
            .map(|tr| match (transition_anchor(old, tr), transition_anchor(track, tr)) {
                (Some(a), Some(b)) => b - a,
                _ => Tick::ZERO,
            })
            .collect();
        for (tr, d) in track.transitions.iter_mut().zip(shifts) {
            tr.start += d;
        }
    }
}

/// Keep a clip's one-sided transitions (a fade in from nothing, a fade out to nothing) on its edges
/// after a regular trim moved them, no longer than the clip (#374). Transitions between two clips
/// are left alone: a regular trim cannot move their cut.
fn fades_follow_edges(track: &mut Track, clip: ClipId) {
    let Some((start, end, len)) = track.item(clip).map(|i| (i.start, i.end(), i.duration)) else { return };
    for tr in &mut track.transitions {
        match (tr.from, tr.to) {
            (None, Some(to)) if to == clip => {
                tr.duration = tr.duration.min(len);
                tr.start = start;
            }
            (Some(from), None) if from == clip => {
                tr.duration = tr.duration.min(len);
                tr.start = end - tr.duration;
            }
            _ => {}
        }
    }
}

fn place(track: &mut Track, item: TrackItem) {
    let idx = track.items.partition_point(|i| i.start <= item.start);
    track.items.insert(idx, item);
}

// ---------------------------------------------------------------------------------------------
// Sequence edits
// ---------------------------------------------------------------------------------------------

fn track_mut(seq: &mut Sequence, id: TrackId) -> Result<&mut Track> {
    seq.track_mut(id).ok_or(EditError::NoTrack(id))
}

/// Overwrite edits: each item replaces material on its track.
pub fn overwrite(seq: &mut Sequence, placements: Vec<(TrackId, TrackItem)>, ctx: &mut EditCtx) -> Result<Vec<ClipId>> {
    let mut links = HashMap::new();
    let mut ids = Vec::new();
    for (tid, _) in &placements {
        if track_mut(seq, *tid)?.locked {
            return Err(EditError::Locked);
        }
    }
    for (tid, item) in placements {
        let t = track_mut(seq, tid)?;
        clear_track_range(t, item.range(), ctx, &mut links);
        ids.push(item.id);
        place(t, item);
    }
    Ok(ids)
}

/// Insert edits: open a gap on target tracks and all sync-locked unlocked tracks, then place.
pub fn insert(seq: &mut Sequence, placements: Vec<(TrackId, TrackItem)>, ctx: &mut EditCtx) -> Result<Vec<ClipId>> {
    if placements.is_empty() {
        return Err(EditError::Nothing);
    }
    for (tid, _) in &placements {
        if track_mut(seq, *tid)?.locked {
            return Err(EditError::Locked);
        }
    }
    let at = placements.iter().map(|p| p.1.start).min().unwrap_or_default();
    let dur = placements.iter().map(|p| p.1.end()).max().unwrap_or_default() - at;
    let targets: Vec<TrackId> = placements.iter().map(|p| p.0).collect();
    let mut links = HashMap::new();
    for t in seq.all_tracks_mut() {
        if !t.locked && (targets.contains(&t.id) || t.sync_lock) {
            insert_track_gap(t, at, dur, ctx, &mut links);
        }
    }
    for ct in seq.caption_tracks.iter_mut().filter(|c| !c.locked && c.sync_lock) {
        captions::insert_gap(ct, at, dur, ctx);
    }
    let mut ids = Vec::new();
    for (tid, item) in placements {
        let t = track_mut(seq, tid)?;
        ids.push(item.id);
        place(t, item);
    }
    Ok(ids)
}

/// Add Edit (razor) at `t` on the given tracks (all unlocked tracks when empty). Returns new ids.
pub fn razor(seq: &mut Sequence, tracks: &[TrackId], t: Tick, ctx: &mut EditCtx) -> Vec<ClipId> {
    let mut links = HashMap::new();
    let mut out = Vec::new();
    for tr in seq.all_tracks_mut() {
        if tr.locked || (!tracks.is_empty() && !tracks.contains(&tr.id)) {
            continue;
        }
        if let Some(id) = split_track_at(tr, t, ctx, &mut links) {
            out.push(id);
        }
    }
    out
}

/// Razor only the given items (and their linked partners if listed) at `t`.
pub fn razor_items(seq: &mut Sequence, items: &[ClipId], t: Tick, ctx: &mut EditCtx) -> Vec<ClipId> {
    let tracks: Vec<TrackId> = items.iter().filter_map(|c| seq.find_item(*c).map(|(tid, _)| tid)).collect();
    let mut links = HashMap::new();
    let mut out = Vec::new();
    for tr in seq.all_tracks_mut() {
        if tr.locked || !tracks.contains(&tr.id) {
            continue;
        }
        // only split if the item at t is one of ours
        if tr.item_at(t).is_some_and(|i| items.contains(&i.id))
            && let Some(id) = split_track_at(tr, t, ctx, &mut links)
        {
            out.push(id);
        }
    }
    out
}

/// Lift: remove `range` on tracks, leaving a gap.
pub fn lift(seq: &mut Sequence, tracks: &[TrackId], range: TimeRange, ctx: &mut EditCtx) -> Vec<ClipId> {
    let mut links = HashMap::new();
    let mut removed = Vec::new();
    for tr in seq.all_tracks_mut() {
        if !tr.locked && tracks.contains(&tr.id) {
            removed.extend(clear_track_range(tr, range, ctx, &mut links));
        }
    }
    removed
}

/// Extract: remove `range` on targeted and sync-locked tracks and close the gap.
pub fn extract(seq: &mut Sequence, tracks: &[TrackId], range: TimeRange, ctx: &mut EditCtx) -> Vec<ClipId> {
    let mut links = HashMap::new();
    let mut removed = Vec::new();
    for tr in seq.all_tracks_mut() {
        if tr.locked || !(tracks.contains(&tr.id) || tr.sync_lock) {
            continue;
        }
        removed.extend(clear_track_range(tr, range, ctx, &mut links));
        shift_track_from(tr, range.end(), -range.duration);
    }
    for ct in seq.caption_tracks.iter_mut().filter(|c| !c.locked && c.sync_lock) {
        captions::extract_range(ct, range);
    }
    removed
}

/// Delete track items (leaving gaps).
pub fn delete_items(seq: &mut Sequence, items: &[ClipId]) -> usize {
    let mut n = 0;
    for tr in seq.all_tracks_mut() {
        if tr.locked {
            continue;
        }
        let before = tr.items.len();
        tr.items.retain(|i| !items.contains(&i.id));
        n += before - tr.items.len();
        remove_orphan_transitions(tr);
    }
    n
}

/// The stretches a ripple delete of `items` closes, in time order: the time the deleted items
/// covered, less whatever stays on the tracks that lose an item. Every such track then moves up
/// by the same amount, into time that is free on all of them, so later clips stay in sync and
/// nothing is overwritten. A clip and its linked sound with a split edit have different edges;
/// closing each one's own range would move the tracks twice. Locked tracks lose nothing and do
/// not count.
fn ripple_delete_spans(seq: &Sequence, items: &[ClipId]) -> Vec<TimeRange> {
    let (mut deleted, mut kept) = (Vec::new(), Vec::new());
    for tr in seq.all_tracks().filter(|t| !t.locked && t.items.iter().any(|i| items.contains(&i.id))) {
        for i in &tr.items {
            if items.contains(&i.id) { deleted.push(i.range()) } else { kept.push(i.range()) }
        }
    }
    let kept = merged(kept);
    let mut spans = Vec::new();
    for r in merged(deleted) {
        let mut at = r.start;
        for k in kept.iter().filter(|k| k.overlaps(&r)) {
            if k.start > at {
                spans.push(TimeRange::from_bounds(at, k.start));
            }
            at = at.max(k.end());
        }
        if at < r.end() {
            spans.push(TimeRange::from_bounds(at, r.end()));
        }
    }
    spans
}

/// How the UI names a track: `V1`, `A2` (by position), else its name.
pub fn track_label(seq: &Sequence, id: TrackId) -> String {
    if let Some(i) = seq.video_tracks.iter().position(|t| t.id == id) {
        return format!("V{}", i + 1);
    }
    if let Some(i) = seq.audio_tracks.iter().position(|t| t.id == id) {
        return format!("A{}", i + 1);
    }
    seq.track(id).map(|t| t.name.clone()).unwrap_or_else(|| format!("track {}", id.0))
}

/// `ranges` in time order, with the ones that touch or overlap joined.
fn merged(mut ranges: Vec<TimeRange>) -> Vec<TimeRange> {
    ranges.sort_by_key(|r| r.start);
    let mut out: Vec<TimeRange> = Vec::new();
    for r in ranges {
        match out.last_mut() {
            Some(last) if r.start <= last.end() => *last = TimeRange::from_bounds(last.start, last.end().max(r.end())),
            _ => out.push(r),
        }
    }
    out
}

/// Ripple delete track items: remove them and close the resulting gaps. Returns the stretches
/// that were closed, in time order. Refused when no gap can close (what stays on the items'
/// tracks covers all of their time): that would be a plain delete.
pub fn ripple_delete_items(seq: &mut Sequence, items: &[ClipId]) -> Result<Vec<TimeRange>> {
    // Collect the tracks that lose an item; the gaps close right-to-left.
    let mut affected: Vec<TrackId> = Vec::new();
    for c in items {
        let (tid, _) = seq.find_item(*c).ok_or(EditError::NoItem(*c))?;
        affected.push(tid);
    }
    let spans = ripple_delete_spans(seq, items);
    if spans.is_empty() && !items.is_empty() {
        let locked = affected.iter().all(|t| seq.track(*t).is_some_and(|tr| tr.locked));
        return Err(if locked { EditError::Locked } else { EditError::Other("no gap can close: other clips on these tracks cover the deleted time".into()) });
    }
    let mut work = seq.clone();
    delete_items(&mut work, items);
    for span in spans.iter().rev() {
        // the gap actually closable: from span.start to the next material on affected tracks
        for tr in work.all_tracks_mut() {
            if tr.locked {
                continue;
            }
            let on_affected = affected.contains(&tr.id);
            if !on_affected && !tr.sync_lock {
                continue;
            }
            if !on_affected && !track_range_empty(tr, *span) {
                return Err(EditError::SyncLockConflict(track_label(seq, tr.id)));
            }
            shift_track_from(tr, span.end(), -span.duration);
        }
    }
    *seq = work;
    Ok(spans)
}

/// Close the gap containing `t` on a track (Ripple Delete on a gap).
pub fn close_gap(seq: &mut Sequence, track: TrackId, t: Tick) -> Result<()> {
    let tr = seq.track(track).ok_or(EditError::NoTrack(track))?;
    if tr.locked {
        // the other tracks would move while this one stayed put
        return Err(EditError::Locked);
    }
    if tr.item_at(t).is_some() {
        return Err(EditError::Nothing);
    }
    let prev_end = tr.items.iter().filter(|i| i.end() <= t).map(|i| i.end()).max().unwrap_or(Tick::ZERO);
    let next_start = tr.items.iter().filter(|i| i.start > t).map(|i| i.start).min().ok_or(EditError::Nothing)?;
    let gap = TimeRange::from_bounds(prev_end, next_start);
    let mut work = seq.clone();
    for tr in work.all_tracks_mut() {
        if tr.locked {
            continue;
        }
        if tr.id != track {
            if !tr.sync_lock {
                continue;
            }
            if !track_range_empty(tr, gap) {
                return Err(EditError::SyncLockConflict(track_label(seq, tr.id)));
            }
        }
        shift_track_from(tr, gap.end(), -gap.duration);
    }
    *seq = work;
    Ok(())
}

/// Move items by (track, time) offsets with overwrite (default drag) or insert (Cmd-drag) semantics.
/// `moves`: (item, destination track, new start).
pub fn move_items(seq: &mut Sequence, moves: &[(ClipId, TrackId, Tick)], insert_mode: bool, ctx: &mut EditCtx) -> Result<()> {
    let mut work = seq.clone();
    let mut placed = Vec::new();
    for (c, dest, start) in moves {
        let (_, it) = work.find_item(*c).ok_or(EditError::NoItem(*c))?;
        let mut it = it.clone();
        if work.track(*dest).ok_or(EditError::NoTrack(*dest))?.locked {
            return Err(EditError::Locked);
        }
        it.start = (*start).max(Tick::ZERO);
        placed.push((*dest, it));
    }
    let carried = carried_transitions(&work, &placed);
    // a transition whose partner stays behind is dropped, as in Premiere
    delete_items(&mut work, &moves.iter().map(|m| m.0).collect::<Vec<_>>());
    if insert_mode {
        insert(&mut work, placed, ctx)?;
    } else {
        overwrite(&mut work, placed, ctx)?;
    }
    for (dest, tr) in carried {
        let t = track_mut(&mut work, dest)?;
        if [tr.from, tr.to].into_iter().flatten().all(|c| t.item(c).is_some()) {
            t.transitions.push(tr);
            t.sort();
        }
    }
    *seq = work;
    Ok(())
}

/// The transitions that travel with a move (#227, #374): those whose clips all move, onto one
/// track and by one offset (a fade at a moved clip's edge, or the cut between two clips that move
/// together), shifted to where those clips land.
fn carried_transitions(seq: &Sequence, placed: &[(TrackId, TrackItem)]) -> Vec<(TrackId, Transition)> {
    let landing = |c: ClipId| {
        let (dest, it) = placed.iter().find(|(_, it)| it.id == c)?;
        let (_, old) = seq.find_item(c)?;
        Some((*dest, it.start - old.start))
    };
    let mut carried = Vec::new();
    for tr in seq.all_tracks().filter(|t| !t.locked).flat_map(|t| t.transitions.iter()) {
        let lands: Option<Vec<(TrackId, Tick)>> = [tr.from, tr.to].into_iter().flatten().map(landing).collect();
        let Some((&(dest, delta), rest)) = lands.as_deref().and_then(<[_]>::split_first) else { continue };
        if rest.iter().all(|l| *l == (dest, delta)) {
            carried.push((dest, Transition { start: tr.start + delta, ..tr.clone() }));
        }
    }
    carried
}

fn media_len(ctx: &EditCtx, item: &TrackItem) -> Option<Tick> {
    if item.frame_hold.is_some() {
        return None;
    }
    (ctx.media_duration)(item.item)
}

/// Media available before a clip's source In (its head handle, media ticks).
fn head(ctx: &EditCtx, item: &TrackItem) -> Tick {
    if item.frame_hold.is_some() {
        return item.source_in;
    }
    (item.source_in - (ctx.media_start)(item.item)).max(Tick::ZERO)
}

fn src_of(dur: Tick, speed: f64) -> Tick {
    Tick((dur.0 as f64 * speed.abs()).round() as i64)
}

/// Neighbours on the same track: (previous end, next start).
fn neighbours(track: &Track, id: ClipId) -> (Tick, Tick) {
    let idx = track.items.iter().position(|i| i.id == id).unwrap_or(0);
    let prev_end = if idx > 0 { track.items[idx - 1].end() } else { Tick::ZERO };
    let next_start = track.items.get(idx + 1).map(|i| i.start).unwrap_or(Tick::MAX);
    (prev_end, next_start)
}

/// Compute the clamped delta a trim may apply (for UI feedback while dragging).
pub fn clamp_trim(seq: &Sequence, clip: ClipId, edge: Edge, mode: TrimMode, delta: Tick, ctx: &EditCtx) -> Result<Tick> {
    let (tid, it) = seq.find_item(clip).ok_or(EditError::NoItem(clip))?;
    let tr = seq.track(tid).ok_or(EditError::NoTrack(tid))?;
    let (prev_end, next_start) = neighbours(tr, clip);
    let media = media_len(ctx, it);
    let speed = it.speed.abs().max(1e-9);
    let mut d = delta;
    match edge {
        Edge::In => {
            // extending left (d<0) needs media before source_in; shortening needs min duration
            // a reversed clip plays source_out down to source_in, so its In edge consumes media after source_out
            let handle = match (it.reverse, media) {
                (true, Some(m)) => (m - it.source_out()).max(Tick::ZERO),
                (true, None) => Tick::MAX,
                (false, _) => head(ctx, it),
            };
            let max_ext = if handle == Tick::MAX { Tick::MAX } else { Tick((handle.0 as f64 / speed).floor() as i64) };
            let lo = if mode == TrimMode::Regular { (-(it.start - prev_end)).max(-max_ext) } else { -max_ext };
            let hi = it.duration - ctx.min_duration;
            // a frame hold needs no source head, but a Regular trim still stops at the previous clip
            let lo = match (it.frame_hold.is_some(), mode) {
                (true, TrimMode::Regular) => -(it.start - prev_end),
                (true, _) => Tick::MIN,
                (false, _) => lo,
            };
            d = d.clamp(lo, hi);
        }
        Edge::Out => {
            let lo = -(it.duration - ctx.min_duration);
            let mut hi = if mode == TrimMode::Regular { next_start - it.end() } else { Tick::MAX };
            if it.reverse && it.frame_hold.is_none() {
                // a reversed clip's Out edge consumes media before source_in
                hi = hi.min(Tick((head(ctx, it).0 as f64 / speed).floor() as i64));
            } else if let Some(m) = media {
                let remain = Tick(((m - it.source_out()).0 as f64 / speed).floor() as i64);
                hi = hi.min(remain);
            }
            d = d.clamp(lo, hi.max(lo));
        }
    }
    Ok(d)
}

/// What moving one edge by `d` (already clamped) does to the item itself: its length, the media
/// it starts at (a reversed clip consumes media from its other end; a frame hold has none to
/// move), and, for the In edge with `move_start`, where it starts. The trims and the timeline's
/// live trim preview share it, so the preview shows the media the edit will keep (#374).
pub fn move_edge(it: &mut TrackItem, edge: Edge, d: Tick, move_start: bool) {
    let speed = it.speed.abs();
    match edge {
        Edge::In => {
            if !it.reverse && it.frame_hold.is_none() {
                it.source_in += src_of(d, speed);
            }
            it.duration -= d;
            if move_start {
                it.start += d;
            }
        }
        Edge::Out => {
            if it.reverse {
                it.source_in -= src_of(d, speed);
            }
            it.duration += d;
        }
    }
}

/// Trim one edge of an item (Selection-tool edge drag = Regular; Ripple tool = Ripple).
/// Linked partners should be trimmed by the caller with the same delta.
pub fn trim(seq: &mut Sequence, clip: ClipId, edge: Edge, mode: TrimMode, delta: Tick, ctx: &mut EditCtx) -> Result<Tick> {
    let d = clamp_trim(seq, clip, edge, mode, delta, ctx)?;
    if d == Tick::ZERO {
        return Ok(d);
    }
    let (tid, it) = seq.find_item(clip).ok_or(EditError::NoItem(clip))?;
    let old_end = it.end();
    let old_start = it.start;
    let own_link = it.link;
    let mut work = seq.clone();
    {
        let (_, it) = work.find_item_mut(clip).ok_or(EditError::NoItem(clip))?;
        move_edge(it, edge, d, mode == TrimMode::Regular);
    }
    if mode == TrimMode::Ripple {
        let (at, shift) = match edge {
            Edge::In => (old_start + Tick(1), -d),
            Edge::Out => (old_end, d),
        };
        for tr in work.all_tracks_mut() {
            if tr.locked {
                continue;
            }
            if tr.id == tid || tr.sync_lock {
                // the stretch that closes: a shorter head takes it from just after the cut, a
                // shorter tail from just before it
                let closing = if edge == Edge::In { TimeRange::new(old_start, -shift) } else { TimeRange::new(at + shift, -shift) };
                // (the clip's own linked partners are the caller's to trim or leave)
                let in_the_way = tr.items.iter().any(|i| i.range().overlaps(&closing) && (own_link.is_none() || i.link != own_link));
                if tr.id != tid && shift < Tick::ZERO && in_the_way {
                    return Err(EditError::SyncLockConflict(track_label(seq, tr.id)));
                }
                let from = if edge == Edge::In { old_start + Tick(1) } else { old_end };
                for i in &mut tr.items {
                    if i.id != clip && i.start >= from {
                        i.start += shift;
                    }
                }
                tr.sort();
            }
        }
        transitions_follow_cuts(seq, &mut work);
    } else if let Ok(track) = track_mut(&mut work, tid) {
        fades_follow_edges(track, clip);
    }
    work.check().map_err(EditError::Other)?;
    *seq = work;
    Ok(d)
}

/// Ripple-trim one edge of a group of linked items (e.g. a clip and its audio partners) as a single
/// edit: every member's edge moves by the same delta, later material on the members' tracks and on
/// sync-locked tracks shifts by the change, and so does a clip that starts before the cut, reaches
/// past it and is linked to a clip that shifts (a split edit). The edit is refused when it would
/// overwrite material on a sync-locked track, or when such a linked clip has no room to follow.
pub fn ripple_trim_group(seq: &mut Sequence, clips: &[ClipId], edge: Edge, delta: Tick, ctx: &mut EditCtx) -> Result<Tick> {
    let Some(&first) = clips.first() else { return Ok(Tick::ZERO) };
    let mut d = delta;
    for c in clips {
        let x = clamp_trim(seq, *c, edge, TrimMode::Ripple, d, ctx)?;
        if x.abs() < d.abs() {
            d = x;
        }
    }
    if d == Tick::ZERO {
        return Ok(d);
    }
    let mut work = seq.clone();
    // track → shift origin for the members on it
    let mut origins: Vec<(TrackId, Tick)> = Vec::new();
    for c in clips {
        let (tid, it) = work.find_item_mut(*c).ok_or(EditError::NoItem(*c))?;
        let from = match edge {
            Edge::In => it.start + Tick(1),
            Edge::Out => it.end(),
        };
        move_edge(it, edge, d, false);
        if !origins.iter().any(|(t, _)| *t == tid) {
            origins.push((tid, from));
        }
    }
    // a shorter or longer head: the cut is the latest member's start (a sound that leads its picture
    // starts earlier), whichever member was grabbed
    let main_from = if edge == Edge::In {
        origins.iter().map(|o| o.1).max().unwrap_or_default()
    } else {
        let (tid, _) = seq.find_item(first).ok_or(EditError::NoItem(first))?;
        origins.iter().find(|(t, _)| *t == tid).map(|o| o.1).unwrap_or_default()
    };
    let shift = if edge == Edge::In { -d } else { d };
    // where the shift starts on each track that ripples: the members' tracks and the sync-locked ones
    let rippling: Vec<(TrackId, Tick, bool)> = work
        .all_tracks()
        .filter(|tr| !tr.locked)
        .filter_map(|tr| match origins.iter().find(|(t, _)| *t == tr.id) {
            Some(o) => Some((tr.id, o.1, true)),
            None => tr.sync_lock.then_some((tr.id, main_from, false)),
        })
        .collect();
    // A split edit: the linked partner of a later clip can start before the shift does and reach
    // past it (its sound leads the cut). It follows the clip it is linked to, or the two would
    // drift apart.
    // (the trimmed clips' own partners are trimmed, or left alone, by the caller: they never follow)
    let own_links: Vec<u64> = clips.iter().filter_map(|c| work.find_item(*c).and_then(|(_, i)| i.link)).collect();
    let mut shifting_links: Vec<u64> = Vec::new();
    for (tid, from, _) in &rippling {
        let Some(tr) = work.track(*tid) else { continue };
        let later = tr.items.iter().filter(|i| !clips.contains(&i.id) && i.start >= *from);
        shifting_links.extend(later.filter_map(|i| i.link).filter(|l| !own_links.contains(l)));
    }
    // every linked clip: its link, and where it is and ends
    let linked: Vec<(u64, ClipId, Tick)> = work.all_tracks().flat_map(|tr| tr.items.iter().filter_map(|i| i.link.map(|l| (l, i.id, i.end())))).collect();
    for (tid, from, member) in rippling {
        let Some(tr) = work.track_mut(tid) else { continue };
        let follows = |i: &TrackItem| i.start < from && i.end() > from && i.link.is_some_and(|l| shifting_links.contains(&l));
        // An L cut: the sound of a clip that ends at the cut runs on into the shortened head. That clip
        // stays put, so its sound stays with it and the L cut still ends where it did. (Unlinked material
        // across the cut, a music bed, still refuses, and so does a linked clip whose partner reaches past
        // the cut.)
        let stays = |i: &TrackItem| {
            let cut = from - Tick(1);
            let mut partners = linked.iter().filter(|p| Some(p.0) == i.link && p.1 != i.id).peekable();
            edge == Edge::In && i.start < cut && i.link.is_some_and(|l| !own_links.contains(&l)) && partners.peek().is_some() && partners.all(|p| p.2 <= cut)
        };
        if !member && shift < Tick::ZERO {
            // a shorter head closes the stretch just after the cut, a shorter tail the one before it
            let closing = if edge == Edge::In { TimeRange::new(from - Tick(1), -shift) } else { TimeRange::new(from + shift, -shift) };
            let own = |i: &TrackItem| i.link.is_some_and(|l| own_links.contains(&l));
            if tr.items.iter().any(|i| i.range().overlaps(&closing) && !follows(i) && !own(i) && !stays(i)) {
                return Err(EditError::SyncLockConflict(track_label(seq, tid)));
            }
        }
        let followers: Vec<ClipId> = tr.items.iter().filter(|i| !clips.contains(&i.id) && follows(i)).map(|i| i.id).collect();
        for i in &mut tr.items {
            if !clips.contains(&i.id) && (i.start >= from || followers.contains(&i.id)) {
                i.start += shift;
            }
        }
        tr.sort();
        // a transition into a follower from a clip that stayed behind no longer sits on a cut
        let ends: Vec<(ClipId, Tick)> = tr.items.iter().map(|i| (i.id, i.end())).collect();
        let starts: Vec<(ClipId, Tick)> = tr.items.iter().map(|i| (i.id, i.start)).collect();
        tr.transitions.retain(|t| match (t.from, t.to) {
            (Some(a), Some(b)) if followers.contains(&b) => ends.iter().find(|e| e.0 == a).map(|e| e.1) == starts.iter().find(|x| x.0 == b).map(|x| x.1),
            _ => true,
        });
        // a follower with no room (a clip in its way, or the sequence start) blocks the edit
        if tr.items.first().is_some_and(|i| i.start < Tick::ZERO) || tr.items.windows(2).any(|w| w[0].end() > w[1].start) {
            return Err(EditError::SyncLockConflict(track_label(seq, tr.id)));
        }
    }
    transitions_follow_cuts(seq, &mut work);
    work.check().map_err(EditError::Other)?;
    *seq = work;
    Ok(d)
}

/// Rolling edit between two adjacent items on one track: moves the cut by `delta`.
pub fn roll(seq: &mut Sequence, left: ClipId, right: ClipId, delta: Tick, ctx: &mut EditCtx) -> Result<Tick> {
    let (_, l) = seq.find_item(left).ok_or(EditError::NoItem(left))?;
    let (_, r) = seq.find_item(right).ok_or(EditError::NoItem(right))?;
    if l.end() != r.start {
        return Err(EditError::Other("items are not adjacent".into()));
    }
    let mut d = delta;
    // left out-point limits
    d = d.max(-(l.duration - ctx.min_duration)).min(r.duration - ctx.min_duration);
    // a reversed clip consumes media from the opposite end, so its media limit is the other one
    if l.reverse {
        d = d.min(Tick((head(ctx, l).0 as f64 / l.speed.abs()).floor() as i64));
    } else if let Some(m) = media_len(ctx, l) {
        d = d.min(Tick(((m - l.source_out()).0 as f64 / l.speed.abs()).floor() as i64));
    }
    if r.reverse {
        if let Some(m) = media_len(ctx, r) {
            d = d.max(-Tick(((m - r.source_out()).0 as f64 / r.speed.abs()).floor() as i64));
        }
    } else {
        d = d.max(-Tick((head(ctx, r).0 as f64 / r.speed.abs()).floor() as i64));
    }
    if d == Tick::ZERO {
        return Ok(d);
    }
    let (ls, rs) = (l.speed.abs(), r.speed.abs());
    let before = seq.clone();
    {
        let (_, l) = seq.find_item_mut(left).ok_or(EditError::NoItem(left))?;
        if l.reverse {
            l.source_in -= src_of(d, ls);
        }
        l.duration += d;
    }
    {
        let (_, r) = seq.find_item_mut(right).ok_or(EditError::NoItem(right))?;
        r.start += d;
        r.duration -= d;
        if !r.reverse {
            r.source_in += src_of(d, rs);
        }
    }
    transitions_follow_cuts(&before, seq);
    Ok(d)
}

/// Slip: change which part of the media an item shows without moving it.
pub fn slip(seq: &mut Sequence, clip: ClipId, delta_media: Tick, ctx: &mut EditCtx) -> Result<Tick> {
    let (_, it) = seq.find_item(clip).ok_or(EditError::NoItem(clip))?;
    let used = src_of(it.duration, it.speed);
    let max_in = media_len(ctx, it).map(|m| m - used).unwrap_or(Tick::MAX);
    let lo = if it.frame_hold.is_some() { Tick::ZERO } else { (ctx.media_start)(it.item) };
    let new_in = (it.source_in + delta_media).clamp(lo, max_in.max(lo));
    let d = new_in - it.source_in;
    let (_, it) = seq.find_item_mut(clip).ok_or(EditError::NoItem(clip))?;
    it.source_in = new_in;
    Ok(d)
}

/// Slide: move an item between its neighbours, trimming them to keep the gapless span.
pub fn slide(seq: &mut Sequence, clip: ClipId, delta: Tick, ctx: &mut EditCtx) -> Result<Tick> {
    let (tid, it) = seq.find_item(clip).ok_or(EditError::NoItem(clip))?;
    let it = it.clone();
    let tr = seq.track(tid).ok_or(EditError::NoTrack(tid))?;
    let idx = tr.items.iter().position(|i| i.id == clip).ok_or(EditError::NoItem(clip))?;
    let prev = idx.checked_sub(1).map(|i| tr.items[i].clone()).filter(|p| p.end() == it.start);
    let next = tr.items.get(idx + 1).cloned().filter(|n| n.start == it.end());
    let mut d = delta;
    if let Some(p) = &prev {
        d = d.max(-(p.duration - ctx.min_duration));
        if p.reverse {
            // a reversed neighbour grows by pulling earlier media in front of its source In
            d = d.min(Tick((head(ctx, p).0 as f64 / p.speed.abs()).floor() as i64));
        } else if let Some(m) = media_len(ctx, p) {
            d = d.min(Tick(((m - p.source_out()).0 as f64 / p.speed.abs()).floor() as i64));
        }
    } else {
        d = d.max(-(it.start - neighbours(tr, clip).0));
    }
    if let Some(n) = &next {
        d = d.min(n.duration - ctx.min_duration);
        if n.reverse {
            // a reversed neighbour grows leftwards by pulling later media behind its source Out
            if let Some(m) = media_len(ctx, n) {
                d = d.max(-Tick(((m - n.source_out()).0 as f64 / n.speed.abs()).floor() as i64));
            }
        } else {
            d = d.max(-Tick((head(ctx, n).0 as f64 / n.speed.abs()).floor() as i64));
        }
    } else {
        d = d.min(neighbours(tr, clip).1 - it.end());
    }
    if d == Tick::ZERO {
        return Ok(d);
    }
    let before = seq.clone();
    let t = seq.track_mut(tid).ok_or(EditError::NoTrack(tid))?;
    if let Some(p) = prev {
        let pi = t.item_mut(p.id).ok_or(EditError::NoItem(p.id))?;
        pi.duration += d;
        if p.reverse {
            pi.source_in -= src_of(d, p.speed);
        }
    }
    if let Some(n) = next {
        let ni = t.item_mut(n.id).ok_or(EditError::NoItem(n.id))?;
        ni.start += d;
        ni.duration -= d;
        if !n.reverse {
            ni.source_in += src_of(d, n.speed);
        }
    }
    let me = t.item_mut(clip).ok_or(EditError::NoItem(clip))?;
    me.start += d;
    transitions_follow_cuts(&before, seq);
    Ok(d)
}

/// Slide several items (a linked picture/sound pair, a selection) by one common delta: the most
/// restrictive item's neighbour limit clamps them all, so their relative timing is kept.
/// Returns the delta applied.
pub fn slide_items(seq: &mut Sequence, clips: &[ClipId], delta: Tick, ctx: &mut EditCtx) -> Result<Tick> {
    let mut d = delta;
    for c in clips {
        let x = slide(&mut seq.clone(), *c, d, ctx)?;
        if x.0.signum() != d.0.signum() {
            d = Tick::ZERO;
        } else if x.abs() < d.abs() {
            d = x;
        }
    }
    if d != Tick::ZERO {
        for c in clips {
            slide(seq, *c, d, ctx)?;
        }
    }
    Ok(d)
}

/// Rate stretch: change an item's duration by dragging an edge, adjusting speed to keep the same media.
pub fn rate_stretch(seq: &mut Sequence, clip: ClipId, edge: Edge, delta: Tick, ctx: &mut EditCtx) -> Result<f64> {
    let (tid, it) = seq.find_item(clip).ok_or(EditError::NoItem(clip))?;
    let tr = seq.track(tid).ok_or(EditError::NoTrack(tid))?;
    let (prev_end, next_start) = neighbours(tr, clip);
    let src_len = it.source_out() - it.source_in;
    let d = match edge {
        Edge::Out => delta.clamp(-(it.duration - ctx.min_duration), next_start - it.end()),
        Edge::In => delta.clamp(-(it.start - prev_end), it.duration - ctx.min_duration),
    };
    let new_dur = match edge {
        Edge::Out => it.duration + d,
        Edge::In => it.duration - d,
    };
    let speed = src_len.0 as f64 / new_dur.0 as f64;
    let (_, it) = seq.find_item_mut(clip).ok_or(EditError::NoItem(clip))?;
    if edge == Edge::In {
        it.start += d;
    }
    it.duration = new_dur;
    it.speed = speed;
    Ok(speed)
}

/// Set speed/duration (Clip ▸ Speed/Duration…). `ripple` shifts following material.
pub fn set_speed(seq: &mut Sequence, clip: ClipId, speed: f64, reverse: bool, ripple: bool, ctx: &mut EditCtx) -> Result<()> {
    set_speed_group(seq, &[clip], speed, reverse, ripple, ctx)
}

/// Speed/Duration on a clip and its linked partners on other tracks (its sound) as a single edit.
/// `clips` holds at most one clip per track. Every member takes the speed, and with `ripple` the
/// later material moves once, on the members' tracks and on sync-locked tracks. Rippling each
/// member on its own would move the later clips once per member. When the members' lengths change
/// by different amounts (a split edit), the later material moves by the largest change, so no
/// member is overlapped and the tracks stay in sync. Nothing changes when the edit fails.
pub fn set_speed_group(seq: &mut Sequence, clips: &[ClipId], speed: f64, reverse: bool, ripple: bool, ctx: &mut EditCtx) -> Result<()> {
    if !(speed.is_finite() && speed > 0.0) {
        return Err(EditError::Other("speed must be positive".into()));
    }
    let mut work = seq.clone();
    // per member track: where its later material starts, and by how much the member's length changed
    let mut origins: Vec<(TrackId, Tick, Tick)> = Vec::new();
    for clip in clips {
        let (tid, it) = work.find_item(*clip).ok_or(EditError::NoItem(*clip))?;
        if origins.iter().any(|o| o.0 == tid) {
            return Err(EditError::Other("clips on one track change speed one at a time".into()));
        }
        let src_len = it.source_out() - it.source_in;
        // a tiny speed saturates the cast: keep the clip's end representable
        let room = Tick(Tick::MAX.0.saturating_sub(it.start.0.max(0)));
        let mut new_dur = Tick((src_len.0 as f64 / speed).round() as i64).min(room).max(ctx.min_duration);
        let old_end = it.end();
        let tr = work.track(tid).ok_or(EditError::NoTrack(tid))?;
        let (_, next_start) = neighbours(tr, *clip);
        if !ripple {
            new_dur = new_dur.min(next_start - it.start);
        }
        let delta = new_dur - it.duration;
        let (_, it) = work.find_item_mut(*clip).ok_or(EditError::NoItem(*clip))?;
        it.speed = speed;
        it.reverse = reverse;
        it.duration = new_dur;
        origins.push((tid, old_end, delta));
    }
    let main_from = origins.first().map(|o| o.1).unwrap_or_default();
    let shift = origins.iter().map(|o| o.2).max().unwrap_or_default();
    if ripple && shift != Tick::ZERO {
        for tr in work.all_tracks_mut() {
            if tr.locked {
                continue;
            }
            let member = origins.iter().find(|o| o.0 == tr.id).map(|o| o.1);
            if member.is_none() && !tr.sync_lock {
                continue;
            }
            let from = member.unwrap_or(main_from);
            for i in &mut tr.items {
                if !clips.contains(&i.id) && i.start >= from {
                    // later clips must still end on the representable timeline
                    let start = i.start.0.checked_add(shift.0).filter(|s| s.checked_add(i.duration.0).is_some());
                    i.start = Tick(start.ok_or_else(|| EditError::Other("the speed change would move later clips past the end of the timeline".into()))?);
                }
            }
            tr.sort();
        }
    }
    if ripple {
        transitions_follow_cuts(seq, &mut work);
    }
    work.check().map_err(EditError::Other)?;
    *seq = work;
    Ok(())
}

/// Add a transition at the cut between `from` and `to` (or at a single clip edge).
pub fn add_transition(seq: &mut Sequence, track: TrackId, mut tr: Transition, ctx: &mut EditCtx) -> Result<TransitionId> {
    let t = seq.track_mut(track).ok_or(EditError::NoTrack(track))?;
    if t.locked {
        return Err(EditError::Locked);
    }
    tr.id = TransitionId(ctx.alloc());
    // Replace an existing transition at the same place.
    t.transitions.retain(|x| !(x.range().overlaps(&tr.range()) && (x.from == tr.from || x.to == tr.to)));
    let id = tr.id;
    t.transitions.push(tr);
    t.sort();
    Ok(id)
}

#[cfg(test)]
mod tests;

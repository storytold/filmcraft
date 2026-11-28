//! Conform: fit a sequence's sections to new lengths at their ends, rippling everything after each
//! change, while one clip (a voice-over take, the reference the picture is fitted to) stays exactly
//! where it is (#5265). The engine works out the lengths from the take's prompter log; this module
//! is only the timeline edit.
//!
//! A [`Fit`] changes the sequence at a section's end `end` by `delta`:
//!
//! - **Longer** (`delta > 0`): time is opened at `end` on every track that ripples, the way an
//!   insert opens it ([`crate::insert_track_gap`], [`crate::captions::insert_gap`]): a clip that
//!   spans `end` is split and its later part moves. On each video track the opened time holds the
//!   track's last frame before `end`, as a frame hold segment (the representation
//!   `clip.insertFrameHoldSegment` uses: a copy of the clip with `frame_hold` set and its
//!   `source_in` at the held frame), with Hold Filters on so its effects freeze on that frame too.
//!   Every video track holds its own last frame, so the stacked picture holds as it was. A clip that
//!   already is a frame hold just continues (same held frame, same Hold Filters). Where a video track
//!   has no clip just before `end` (a gap) the opened time stays empty, as does every audio track
//!   (silence) and caption track.
//! - **Shorter** (`delta < 0`): `[end + delta, end)` is removed and closed on every track that
//!   ripples, exactly as [`crate::extract`] removes a range.
//!
//! Which tracks ripple is decided as for an insert or extract with no targeted track: an unlocked,
//! sync-locked track moves; a track with sync lock off stays where it is. A locked, sync-locked track
//! with material after the first change would stop the ripple and lose sync: the conform is refused
//! and names that track, so nothing is left half-moved. A transition in progress where a fit cuts
//! (at a section end it extends; across or at the edges of a range it removes; on a video track, over
//! the last frame a hold would show) is refused too ([`obstacle`]): a held frame of one clip cannot
//! show a mix of two, and cutting through a transition would drop it.
//!
//! The kept clip is taken off its track before the edit and put back unchanged afterwards, so it
//! is never split or moved; the other clips on its track ripple like any other. If one of them would
//! then overlap the kept clip, or the kept clip has a transition with another clip, the conform is
//! refused.
//!
//! Fits are applied from the last to the first, so each one's `end` is in the sequence's original
//! time: a fit only changes time at and after its own end, which is after every earlier fit's.
//! Markers are not touched here; the engine moves them with `ripple_markers`.

use std::collections::HashMap;

use filmcraft_project::{ClipId, Sequence, Track, TrackItem, Transition};
use filmcraft_time::{Tick, TimeRange};

use crate::{EditCtx, EditError, Result, captions, insert_track_gap, place, track_label};

/// Change the sequence at `end` (a section's end, in the original sequence time) by `delta`:
/// positive extends by holding the last frame, negative removes `[end + delta, end)`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Fit {
    pub end: Tick,
    pub delta: Tick,
}

impl Fit {
    /// Where the fit starts changing the sequence: the end for an extension, the start of the
    /// removed range for a trim.
    pub fn from(&self) -> Tick {
        if self.delta < Tick::ZERO { self.end + self.delta } else { self.end }
    }
}

/// Whether a track follows the conform's ripple (as an untargeted track follows an insert).
fn ripples(t: &Track) -> bool {
    !t.locked && t.sync_lock
}

/// Why `fit` cannot be made on `seq`, or None: a transition is in progress where it cuts. The
/// transitions of `keep` (which does not move) are not counted.
pub fn obstacle(seq: &Sequence, fit: Fit, keep: Option<ClipId>) -> Option<String> {
    let frame = seq.settings.frame_rate.frame_duration();
    let video: Vec<_> = seq.video_tracks.iter().map(|t| t.id).collect();
    for t in seq.all_tracks().filter(|t| ripples(t)) {
        let is_video = video.contains(&t.id);
        let lo = if fit.delta < Tick::ZERO {
            fit.end + fit.delta
        } else if is_video {
            fit.end - frame
        } else {
            fit.end
        };
        let hit = t.transitions.iter().find(|tr| !touches(tr, keep) && tr.start < fit.end && tr.end() > lo);
        if hit.is_some() {
            return Some(format!("a transition on {} is in progress at the section's end", track_label(seq, t.id)));
        }
    }
    None
}

fn touches(tr: &Transition, keep: Option<ClipId>) -> bool {
    keep.is_some() && (tr.from == keep || tr.to == keep)
}

/// The first locked track that would stop a ripple starting at `from`: it is sync-locked (so it
/// should move) but locked (so it cannot) and has material that ends after `from`. `keep` is not
/// counted: it does not move.
pub fn locked_in_the_way(seq: &Sequence, from: Tick, keep: Option<ClipId>) -> Option<String> {
    if let Some(t) = seq.all_tracks().find(|t| t.locked && t.sync_lock && t.items.iter().any(|i| Some(i.id) != keep && i.end() > from)) {
        return Some(track_label(seq, t.id));
    }
    seq.caption_tracks.iter().find(|t| t.locked && t.sync_lock && t.captions.iter().any(|c| c.end() > from)).map(|t| t.name.clone())
}

/// Apply `fits` (sorted by `end`, each one's changed range after the previous fit's end) as one
/// edit, keeping `keep` exactly as it is. Either the whole conform is made or `seq` is unchanged.
pub fn conform(seq: &mut Sequence, fits: &[Fit], keep: Option<ClipId>, ctx: &mut EditCtx) -> Result<()> {
    validate(seq, fits)?;
    if fits.is_empty() {
        return Ok(());
    }
    let first = fits.iter().map(Fit::from).min().unwrap_or(Tick::ZERO);
    if let Some(track) = locked_in_the_way(seq, first, keep) {
        return Err(EditError::Other(format!("{track} is locked and would stop the ripple; unlock it, or turn off its sync lock, to conform")));
    }
    if let Some(why) = fits.iter().find_map(|f| obstacle(seq, *f, keep)) {
        return Err(EditError::Other(why));
    }
    let mut work = seq.clone();
    let kept = match keep {
        Some(id) => Some(take_out(&mut work, id)?),
        None => None,
    };
    for fit in fits.iter().rev() {
        if fit.delta > Tick::ZERO {
            extend(&mut work, *fit, ctx);
        } else {
            crate::extract(&mut work, &[], TimeRange::from_bounds(fit.end + fit.delta, fit.end), ctx);
        }
    }
    if let Some(k) = kept {
        put_back(&mut work, k)?;
    }
    work.check().map_err(EditError::Other)?;
    *seq = work;
    Ok(())
}

/// Fits in time order, each changing time only after the previous one's end, within the bounds
/// timeline arithmetic supports (the sequence's own content is bounded by `Sequence::check_bounds`).
fn validate(seq: &Sequence, fits: &[Fit]) -> Result<()> {
    let room = Tick::MAX - seq.duration().max(Tick::ZERO);
    let total: i128 = fits.iter().map(|f| i128::from(f.delta.0.max(0))).sum();
    let bounded = |t: Tick| (Tick::MIN..=Tick::MAX).contains(&t);
    if total > i128::from(room.0) || fits.iter().any(|f| !bounded(f.end) || !bounded(f.delta) || !bounded(f.end + f.delta)) {
        return Err(EditError::Other("a fit is outside supported time bounds".into()));
    }
    let mut prev_end: Option<Tick> = None;
    for f in fits {
        if f.delta == Tick::ZERO {
            return Err(EditError::Other("a fit must change the length".into()));
        }
        if let Some(p) = prev_end
            && f.from() <= p
        {
            return Err(EditError::Other("fits must be in time order, each changing time after the previous one's end".into()));
        }
        prev_end = Some(f.end);
    }
    Ok(())
}

/// The kept clip, taken off its track, with its transitions.
struct Kept {
    track: filmcraft_project::TrackId,
    item: TrackItem,
    transitions: Vec<Transition>,
}

fn take_out(seq: &mut Sequence, id: ClipId) -> Result<Kept> {
    let (track, _) = seq.find_item(id).ok_or(EditError::NoItem(id))?;
    let label = track_label(seq, track);
    let t = seq.track_mut(track).ok_or(EditError::NoTrack(track))?;
    if t.transitions.iter().any(|tr| touches(tr, Some(id)) && tr.from.is_some() && tr.to.is_some()) {
        return Err(EditError::Other(format!("the kept clip on {label} has a transition with another clip; remove it to conform")));
    }
    let pos = t.items.iter().position(|i| i.id == id).ok_or(EditError::NoItem(id))?;
    let item = t.items.remove(pos);
    let (transitions, rest): (Vec<_>, Vec<_>) = std::mem::take(&mut t.transitions).into_iter().partition(|tr| touches(tr, Some(id)));
    t.transitions = rest;
    Ok(Kept { track, item, transitions })
}

fn put_back(seq: &mut Sequence, k: Kept) -> Result<()> {
    let label = track_label(seq, k.track);
    let t = seq.track_mut(k.track).ok_or(EditError::NoTrack(k.track))?;
    if let Some(other) = t.items.iter().find(|i| i.range().overlaps(&k.item.range())) {
        return Err(EditError::Other(format!(
            "on {label}, clip {} would overlap the kept clip after the conform; move it to another track to conform",
            other.id.0
        )));
    }
    place(t, k.item);
    t.transitions.extend(k.transitions);
    t.sort();
    Ok(())
}

/// The frame hold segment that continues `clip`'s last frame before `end` for `dur`.
fn hold_segment(clip: &TrackItem, end: Tick, dur: Tick, frame: Tick, ctx: &mut EditCtx) -> TrackItem {
    let mut seg = clip.clone();
    seg.id = ClipId(ctx.alloc());
    seg.start = end;
    seg.duration = dur;
    seg.link = None;
    seg.markers.clear();
    if clip.frame_hold.is_some() {
        // already a hold: it continues as the clip would have (same frame, same Hold Filters)
        seg.source_in = clip.moving_source_time_at(end);
    } else {
        let last = (end - frame).max(clip.start);
        let held = clip.source_time_at(last);
        seg.source_in = held;
        seg.frame_hold = Some(held);
        seg.hold_filters = true;
    }
    seg
}

fn extend(seq: &mut Sequence, fit: Fit, ctx: &mut EditCtx) {
    let (end, dur) = (fit.end, fit.delta);
    let frame = seq.settings.frame_rate.frame_duration();
    // each video track's clip showing the last frame before the end, before anything is split
    let holds: Vec<(filmcraft_project::TrackId, TrackItem)> = seq
        .video_tracks
        .iter()
        .filter(|t| ripples(t))
        .filter_map(|t| t.items.iter().find(|i| i.start < end && i.end() >= end).map(|i| (t.id, i.clone())))
        .collect();
    let mut links: HashMap<u64, u64> = HashMap::new();
    for t in seq.all_tracks_mut().filter(|t| ripples(t)) {
        insert_track_gap(t, end, dur, ctx, &mut links);
    }
    for ct in seq.caption_tracks.iter_mut().filter(|c| !c.locked && c.sync_lock) {
        captions::insert_gap(ct, end, dur, ctx);
    }
    for (track, clip) in holds {
        let seg = hold_segment(&clip, end, dur, frame, ctx);
        if let Some(t) = seq.track_mut(track) {
            place(t, seg);
        }
    }
}

#[cfg(test)]
mod tests;

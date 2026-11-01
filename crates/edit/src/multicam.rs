//! Multi-camera editing: switching angles, recording cuts live, healing through edits and
//! flattening multi-camera clips into the clips they show.
//!
//! A multi-camera clip is a track item whose [`TrackItem::multicam`] is set; it refers to a
//! multi-camera source sequence. Its angle is a camera index of that source.

use std::collections::HashMap;

use filmcraft_project::{ClipId, ItemId, MulticamSel, Sequence, Track, TrackId, TrackItem};
use filmcraft_time::{Tick, TimeRange};

use crate::{EditCtx, EditError, Result, remove_orphan_transitions, split_track_at};

/// Whether `it` is a multi-camera clip (enabled or not).
pub fn is_multicam(it: &TrackItem) -> bool {
    it.multicam.is_some()
}

/// Set the angle of multi-camera clips. Returns how many changed.
pub fn set_angle(seq: &mut Sequence, clips: &[ClipId], angle: u32) -> usize {
    let mut n = 0;
    for t in seq.all_tracks_mut() {
        if t.locked {
            continue;
        }
        for it in t.items.iter_mut().filter(|i| clips.contains(&i.id)) {
            if let Some(m) = &mut it.multicam
                && m.angle != angle
            {
                m.angle = angle;
                n += 1;
            }
        }
    }
    n
}

/// Enable or disable multi-camera behaviour of clips (Multi-Camera ▸ Enable). Clips that are not
/// multi-camera clips are left alone. Returns how many changed.
pub fn set_enabled(seq: &mut Sequence, clips: &[ClipId], enabled: bool) -> usize {
    let mut n = 0;
    for t in seq.all_tracks_mut() {
        for it in t.items.iter_mut().filter(|i| clips.contains(&i.id)) {
            if let Some(m) = &mut it.multicam
                && m.enabled != enabled
            {
                m.enabled = enabled;
                n += 1;
            }
        }
    }
    n
}

/// Two adjacent pieces of one multi-camera clip that a cut between them would not change.
fn continuous(a: &TrackItem, b: &TrackItem) -> bool {
    a.end() == b.start
        && a.item == b.item
        && a.multicam.is_some()
        && a.multicam == b.multicam
        && a.speed == b.speed
        && !a.reverse
        && !b.reverse
        && a.frame_hold.is_none()
        && b.frame_hold.is_none()
        && a.source_out() == b.source_in
        && a.enabled == b.enabled
        && a.gain_db == b.gain_db
        && a.effects == b.effects
}

/// Remove through edits between pieces of the same multi-camera clip showing the same angle,
/// for edit points in `range` (inclusive of its ends). Returns the ids of the removed pieces.
pub fn heal_track(track: &mut Track, range: TimeRange) -> Vec<ClipId> {
    let mut removed = Vec::new();
    let mut i = 0;
    while i + 1 < track.items.len() {
        let (a, b) = (&track.items[i], &track.items[i + 1]);
        let at = b.start;
        if at >= range.start && at <= range.end() && continuous(a, b) {
            let (aid, bid) = (a.id, b.id);
            let dur = b.duration;
            track.items[i].duration += dur;
            track.items.remove(i + 1);
            track.transitions.retain(|tr| !(tr.from == Some(aid) && tr.to == Some(bid)));
            for tr in &mut track.transitions {
                if tr.from == Some(bid) {
                    tr.from = Some(aid);
                }
                if tr.to == Some(bid) {
                    tr.to = Some(aid);
                }
            }
            removed.push(bid);
        } else {
            i += 1;
        }
    }
    remove_orphan_transitions(track);
    removed
}

/// One live switch: from `time` on, show `angle`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Cut {
    pub time: Tick,
    pub angle: u32,
}

/// Apply a recording pass: on `tracks`, the multi-camera clips of `source` get angle `cuts[i].angle` from
/// `cuts[i].time` until the next cut, the last one until `end`. Clips are split at each cut and at
/// `end`; afterwards through edits inside the recorded range are healed, so pressing the angle that
/// is already showing adds no edit. Locked tracks are skipped. Returns the number of edit points
/// in the recorded range after the pass.
pub fn record(seq: &mut Sequence, tracks: &[TrackId], source: ItemId, cuts: &[Cut], end: Tick, ctx: &mut EditCtx) -> usize {
    let mut cuts: Vec<Cut> = cuts.iter().copied().filter(|c| c.time < end).collect();
    cuts.sort_by_key(|c| c.time);
    // two presses at one time: the later wins
    let mut dedup: Vec<Cut> = Vec::new();
    for c in cuts {
        match dedup.last_mut() {
            Some(l) if l.time == c.time => *l = c,
            _ => dedup.push(c),
        }
    }
    let Some(first) = dedup.first().map(|c| c.time) else { return 0 };
    let mut links = HashMap::new();
    let mut points = 0;
    for tr in seq.all_tracks_mut() {
        if tr.locked || !tracks.contains(&tr.id) {
            continue;
        }
        let mc_at = |tr: &Track, t: Tick| tr.items.iter().any(|i| i.start < t && t < i.end() && is_multicam(i) && i.item == source);
        for t in dedup.iter().map(|c| c.time).chain([end]) {
            if mc_at(tr, t) {
                split_track_at(tr, t, ctx, &mut links);
            }
        }
        for (k, c) in dedup.iter().enumerate() {
            let r_end = dedup.get(k + 1).map_or(end, |n| n.time);
            for it in tr.items.iter_mut().filter(|i| i.start >= c.time && i.end() <= r_end && i.item == source) {
                if let Some(m) = &mut it.multicam {
                    m.angle = c.angle;
                }
            }
        }
        heal_track(tr, TimeRange::from_bounds(first, end));
        points += tr.items.iter().filter(|i| i.start > first && i.start < end).count();
    }
    points
}

/// The clips a multi-camera (or any nested) clip shows, as track items of the outer sequence:
/// the parts of `source_track`'s items inside the clip's source range, moved to timeline time.
/// Inner links are remapped through `links` (so a flattened video/audio pair stays linked).
/// Fails for clips with a speed change, reverse or frame hold (no exact flattening).
pub fn flatten_items(outer: &TrackItem, source_track: &Track, ctx: &mut EditCtx, links: &mut HashMap<u64, u64>) -> Result<Vec<TrackItem>> {
    if (outer.speed - 1.0).abs() > 1e-9 || outer.reverse || outer.frame_hold.is_some() {
        return Err(EditError::Other("can't flatten a clip with a speed change, reverse or frame hold".into()));
    }
    let (s0, s1) = (outer.source_in, outer.source_out());
    let mut out = Vec::new();
    for ni in source_track.items.iter().filter(|i| i.start < s1 && i.end() > s0) {
        let mut it = ni.clone();
        if it.start < s0 {
            let cut = s0 - it.start;
            it.source_in += Tick((cut.0 as f64 * it.speed.abs()).round() as i64);
            it.duration -= cut;
            it.start = s0;
        }
        if it.end() > s1 {
            it.duration = s1 - it.start;
        }
        if it.duration <= Tick::ZERO {
            continue;
        }
        it.start = outer.start + (it.start - s0);
        it.id = ClipId(ctx.alloc());
        it.link = it.link.map(|l| *links.entry(l).or_insert_with(|| ctx.alloc()));
        it.group = None;
        // the outer clip's look carries over: its standard effects follow the inner ones, and its
        // Motion/Opacity/Volume replace the inner ones when only the outer clip changed them
        let take_intrinsics = outer.has_modified_intrinsics() && !it.has_modified_intrinsics();
        for e in &outer.effects {
            let intrinsic = e.def().is_some_and(|d| d.intrinsic);
            if !intrinsic {
                it.effects.push(e.clone());
            } else if take_intrinsics && let Some(slot) = it.effects.iter_mut().find(|x| x.effect == e.effect) {
                *slot = e.clone();
            }
        }
        out.push(it);
    }
    Ok(out)
}

/// Replace a multi-camera clip on `track` by `pieces` (from [`flatten_items`]); pieces whose
/// track is not `track` go to the tracks given with them. Returns the new ids.
pub fn replace_with(seq: &mut Sequence, clip: ClipId, pieces: Vec<(TrackId, TrackItem)>, ctx: &mut EditCtx) -> Result<Vec<ClipId>> {
    let (tid, _) = seq.find_item(clip).ok_or(EditError::NoItem(clip))?;
    let t = seq.track_mut(tid).ok_or(EditError::NoTrack(tid))?;
    if t.locked {
        return Err(EditError::Locked);
    }
    t.items.retain(|i| i.id != clip);
    remove_orphan_transitions(t);
    crate::overwrite(seq, pieces, ctx)
}

/// The multi-camera selection a freshly edited multi-camera clip gets.
pub fn default_sel(angle: u32) -> MulticamSel {
    MulticamSel { enabled: true, angle }
}

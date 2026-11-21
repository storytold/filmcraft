//! Sync offsets between the clips of a link (#4256): how much later a clip shows a moment of its
//! source than each partner shows the same moment, for `sequence.inspect`.

use std::collections::HashMap;

use filmcraft_project::{ClipId, Sequence, TrackItem};
use filmcraft_time::FrameRate;
use serde_json::{Value, json};

/// Media time (ticks, unrounded) the clip shows at sequence time `t`, ignoring a frame hold.
fn source_at(c: &TrackItem, t: f64) -> f64 {
    let v = c.speed.abs();
    let (start, end, src_in) = (c.start.0 as f64, c.end().0 as f64, c.source_in.0 as f64);
    if c.reverse { src_in + (end - t) * v } else { src_in + (t - start) * v }
}

/// Sequence time at which the clip shows media time `s` (extended past its edges).
fn time_of(c: &TrackItem, s: f64) -> f64 {
    let v = c.speed.abs();
    let (start, end, src_in) = (c.start.0 as f64, c.end().0 as f64, c.source_in.0 as f64);
    if c.reverse { end - (s - src_in) / v } else { start + (s - src_in) / v }
}

/// Whether the clip maps sequence time to media time one to one at some speed.
fn moving(c: &TrackItem) -> bool {
    c.frame_hold.is_none() && c.speed.is_finite() && c.speed != 0.0
}

/// `clip`'s offset from `partner` at sequence time `t`, in ticks; `None` when they share no
/// source moments (different items, a frame hold, a zero speed).
fn offset_at(clip: &TrackItem, partner: &TrackItem, t: f64) -> Option<i64> {
    if clip.item != partner.item || !moving(clip) || !moving(partner) {
        return None;
    }
    let o = t - time_of(partner, source_at(clip, t));
    o.is_finite().then(|| o.round() as i64)
}

/// The `sync` list of a linked clip: one entry per other member of its link. `None` for a clip
/// without a link.
pub fn sync_entries(q: &Sequence, clip: &TrackItem, rate: FrameRate) -> Option<Value> {
    let link = clip.link?;
    let frame = rate.frame_duration().0.max(1) as f64;
    let frames = |o: Option<i64>| o.map(|o| o as f64 / frame);
    let partners = q.video_tracks.iter().chain(&q.audio_tracks).flat_map(|t| &t.items).filter(|i| i.link == Some(link) && i.id != clip.id);
    let entries = partners
        .map(|p| {
            let offset = offset_at(clip, p, clip.start.0 as f64);
            let mut e = json!({"clip": p.id.0, "offset": offset, "offsetFrames": frames(offset)});
            // at different speeds or directions the offset drifts along the clip
            if offset.is_some() && (clip.speed.abs() != p.speed.abs() || clip.reverse != p.reverse) {
                let end = offset_at(clip, p, clip.end().0 as f64);
                e["endOffset"] = json!(end);
                e["endOffsetFrames"] = json!(frames(end));
            }
            e
        })
        .collect::<Vec<_>>();
    Some(Value::Array(entries))
}

/// The Timeline's out-of-sync indicators (#4275): each linked clip that shows its source at a
/// different time than a partner does, with its offset at its start in whole frames (rounded; a
/// pair less than half a frame apart is in sync). Partners from other media are skipped.
pub fn out_of_sync_frames(q: &Sequence, rate: FrameRate) -> HashMap<ClipId, i64> {
    let mut links: HashMap<u64, Vec<&TrackItem>> = HashMap::new();
    for i in q.video_tracks.iter().chain(&q.audio_tracks).flat_map(|t| &t.items) {
        if let Some(l) = i.link {
            links.entry(l).or_default().push(i);
        }
    }
    let frame = rate.frame_duration().0.max(1) as f64;
    let mut out = HashMap::new();
    for members in links.values() {
        for c in members {
            let off = members
                .iter()
                .filter(|p| p.id != c.id)
                .filter_map(|p| offset_at(c, p, c.start.0 as f64))
                .map(|o| (o as f64 / frame).round() as i64)
                .find(|f| *f != 0);
            if let Some(f) = off {
                out.insert(c.id, f);
            }
        }
    }
    out
}

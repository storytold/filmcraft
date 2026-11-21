//! `sequence.pacing`: the rhythm of a sequence's picture, for agents and reviews.
//!
//! An agent that edits by transcript sees words, not shots, and tends to cut too rarely and hold
//! shots too long. This read-only report gives it the numbers an editor judges by eye: the shots
//! the viewer sees, their lengths, the cut rate, the longest holds and the black gaps.
//!
//! The picture at a time is the clip on the highest enabled video track that covers it. Graphic
//! clips (titles, shapes) and adjustment layers sit over the picture without replacing it, so they
//! don't start a shot. A through edit (the next clip continues the same media from the frame the
//! previous one stopped at, at the same speed and direction) is not a visible cut, so the shot
//! carries on across it. Opacity, scale and transitions are not considered: a dissolve counts as a
//! cut at the edit point, and a picture-in-picture clip on a higher track counts as the shot.

use filmcraft_project::{ClipId, ItemId, ItemKind, Project, Sequence, TrackItem};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, always, bad, f64_p, u64_p};
use crate::{EngineError, Result, Session};

/// Shots listed in full at most; the statistics always cover every shot.
const MAX_LISTED: usize = 500;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "sequence.pacing",
        label: "Pacing Report",
        menu: &[],
        shortcut: None,
        params: r#"{"sequence":id?,"longSeconds":f=5}"#,
        enabled: always,
        run: pacing,
        journal: false,
        named: NamedKeys::NONE,
    }]
}

/// One stretch of the program picture: a shot (`clip` set) or black (`clip` none).
#[derive(Clone, Debug, PartialEq)]
pub struct Segment {
    pub start: Tick,
    pub end: Tick,
    /// The clip shown at the start of the shot.
    pub clip: Option<ClipId>,
}

impl Segment {
    pub(crate) fn len(&self) -> Tick {
        self.end - self.start
    }
}

/// Whether the project item behind a clip replaces the picture (media, nests, subclips) rather
/// than sitting over it (graphics, adjustment layers).
pub(crate) fn is_picture(p: &Project, item: ItemId) -> bool {
    !matches!(p.item(item).map(|i| &i.kind), Some(ItemKind::Graphic { .. } | ItemKind::AdjustmentLayer { .. }))
}

/// `b` continues `a` with no visible change: the editing rule for through edits
/// ([`filmcraft_edit::through::is_through_edit`]: same speed, direction, holds, gain and effects,
/// continuous media within its rounding slack), with two differences for what the viewer sees:
/// subclips of one file are the same media (their clips use the parent's media time), and a
/// multi-camera selection that is switched off plays like a nest, so its stored angle doesn't count.
pub(crate) fn is_through_edit(p: &Project, a: &TrackItem, b: &TrackItem) -> bool {
    let root = |c: &TrackItem| p.resolve_media(c.item).map_or(c.item, |(r, _, _)| r);
    if root(a) != root(b) {
        return false;
    }
    let angle = |c: &TrackItem| c.multicam.filter(|m| m.enabled).map(|m| m.angle);
    let mut b = b.clone();
    b.item = a.item;
    if angle(a) == angle(&b) {
        b.multicam = a.multicam;
    }
    filmcraft_edit::through::is_through_edit(a, &b)
}

/// The clip that is the picture at `t`: from the top enabled video track down, the clip playback
/// takes on that track (`Track::item_at`: the latest start at or before `t`, nothing once it has
/// ended), if it is enabled and replaces the picture (graphics and adjustment layers sit over it).
/// A nest shows nothing past the end of its contents (`Project::nest_overhang`), so there the
/// tracks below show through, as through an empty spot.
pub(crate) fn picture_at<'a>(p: &Project, q: &'a Sequence, t: Tick) -> Option<&'a TrackItem> {
    let shows = |c: &TrackItem| c.enabled && c.duration > Tick::ZERO && is_picture(p, c.item) && p.nest_overhang(c).is_none_or(|r| !r.contains(t));
    q.video_tracks.iter().rev().filter(|tr| tr.enabled).find_map(|tr| tr.item_at(t).filter(|c| shows(c)))
}

/// The program picture of `q` from 0 to its end, as shots and black gaps.
pub fn segments(p: &Project, q: &Sequence) -> Vec<Segment> {
    let end = q.duration();
    let mut bounds: Vec<Tick> = vec![Tick::ZERO, end];
    // the picture can only change where a clip on an enabled video track starts or ends
    for c in q.video_tracks.iter().filter(|tr| tr.enabled).flat_map(|tr| tr.items.iter()) {
        bounds.push(c.start);
        bounds.push(c.end());
        // a shortened nest stops showing where its contents end
        if let Some(r) = p.nest_overhang(c) {
            bounds.push(r.start);
        }
    }
    bounds.retain(|t| *t >= Tick::ZERO && *t <= end);
    bounds.sort();
    bounds.dedup();

    let mut out: Vec<Segment> = Vec::new();
    // the clip shown in the previous piece, to recognise through edits
    let mut prev: Option<&TrackItem> = None;
    for w in bounds.windows(2) {
        let (a, b) = (w[0], w[1]);
        let top = picture_at(p, q, a);
        let same_shot = match (out.last(), prev, top) {
            (Some(last), _, None) => last.clip.is_none(),
            (Some(last), Some(pc), Some(tc)) => last.clip.is_some() && (pc.id == tc.id || is_through_edit(p, pc, tc)),
            _ => false,
        };
        match out.last_mut() {
            Some(last) if same_shot => last.end = b,
            _ => out.push(Segment { start: a, end: b, clip: top.map(|c| c.id) }),
        }
        prev = top;
    }
    out
}

fn median(sorted: &[f64]) -> f64 {
    match sorted.len() {
        0 => 0.0,
        n if n % 2 == 1 => sorted[n / 2],
        n => (sorted[n / 2 - 1] + sorted[n / 2]) / 2.0,
    }
}

fn pacing(s: &mut Session, p: &Value) -> Result<Value> {
    let seq_id = match u64_p(p, "sequence") {
        Some(id) => ItemId(id),
        None => s.state.active_sequence.ok_or(EngineError::NoSequence)?,
    };
    let q = s.project.sequence(seq_id).ok_or_else(|| bad("sequence.pacing", format!("{} is not a sequence", seq_id.0)))?;
    let long = f64_p(p, "longSeconds").unwrap_or(5.0);
    if !long.is_finite() || long <= 0.0 {
        return Err(bad("sequence.pacing", "longSeconds must be a positive number"));
    }
    let segs = segments(&s.project, q);
    let name = |c: Option<ClipId>| c.and_then(|c| q.find_item(c)).map(|(_, it)| it.name.clone());
    let seg_json = |g: &Segment| {
        json!({
            "start": g.start.0, "end": g.end.0,
            "startSeconds": g.start.seconds(), "seconds": g.len().seconds(),
            "clip": g.clip.map(|c| c.0), "name": name(g.clip),
        })
    };
    let shots: Vec<&Segment> = segs.iter().filter(|g| g.clip.is_some()).collect();
    let gaps: Vec<&Segment> = segs.iter().filter(|g| g.clip.is_none()).collect();
    let mut lens: Vec<f64> = shots.iter().map(|g| g.len().seconds()).collect();
    lens.sort_by(f64::total_cmp);
    let duration = q.duration().seconds();
    // every change of picture: shot to shot, and into or out of black
    let cuts = segs.len().saturating_sub(1);
    let longest = shots.iter().max_by_key(|g| g.len()).map(|g| seg_json(g));
    let long_shots: Vec<Value> = shots.iter().filter(|g| g.len().seconds() > long).map(|g| seg_json(g)).collect();
    let mean = if lens.is_empty() { 0.0 } else { lens.iter().sum::<f64>() / lens.len() as f64 };
    Ok(json!({
        "sequence": seq_id.0,
        "durationSeconds": duration,
        "shotCount": shots.len(),
        "cuts": cuts,
        "cutsPerMinute": if duration > 0.0 { cuts as f64 * 60.0 / duration } else { 0.0 },
        "shotSeconds": {
            "mean": mean,
            "median": median(&lens),
            "min": lens.first().copied().unwrap_or(0.0),
            "max": lens.last().copied().unwrap_or(0.0),
        },
        "longest": longest,
        "longSeconds": long,
        "longShots": long_shots,
        "gaps": gaps.iter().map(|g| seg_json(g)).collect::<Vec<_>>(),
        "gapSeconds": gaps.iter().map(|g| g.len().seconds()).sum::<f64>(),
        "shots": shots.iter().take(MAX_LISTED).map(|g| seg_json(g)).collect::<Vec<_>>(),
        "shotsTruncated": shots.len() > MAX_LISTED,
        "notes": "Shots are what the viewer sees: the clip on the highest enabled video track; titles and adjustment layers don't start a shot, and a through edit isn't a cut. Opacity, scale and transitions are not considered.",
    }))
}

#[cfg(test)]
mod tests;

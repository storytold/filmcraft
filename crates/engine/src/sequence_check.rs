//! `sequence.check`: a read-only list of the mistakes an editor sees on the first playback and
//! a command result never mentions (#4257): flash frames, holes in the picture or the sound,
//! stretches where nothing is seen or heard, and jump cuts inside one shot.
//!
//! What is seen and heard follows the renderer, so an agent does not have to copy its rules:
//!
//! - **Picture** comes from an enabled clip on a video track whose output (eye) is on that is not
//!   an adjustment layer (the compositor skips disabled clips, `plan.rs` skips a track whose output
//!   is off, and an adjustment layer only changes what is under it).
//! - **Sound** comes from an enabled clip on an audio track that is heard: not muted and, while any
//!   audio track is soloed, soloed itself (the preview mix's rule in `crates/render/src/preview.rs`).
//! - A **transition** shows or plays each of its clips that does so over its whole length, into
//!   the clip's handles, so its frames count even past the end of that clip.
//!
//! Frames are in the sequence rate, end exclusive. A frame belongs to a clip when the clip covers
//! the frame's start, as export samples it, so a clip trimmed off the frame grid still lines up
//! with `durationFrames` (the frames export renders, a partial last frame included); on the grid
//! these are the frames `sequence.inspect` reports. Findings come in time order.
//!
//! A nested sequence counts as picture (on a video track) or sound (on an audio track) for all of
//! its length: what plays inside it is not looked into.
//!
//! | Check | Severity | Finding |
//! |---|---|---|
//! | `shortClip` | warning | a clip on any track shorter than `minFrames` (default 3) |
//! | `pictureHole` | warning | frames with sound and no picture; `audioClips` play during them |
//! | `soundHole` | warning | frames with picture and no sound; `videoClips` show during them |
//! | `emptyStretch` | warning | frames with neither: `before` ends at its start, `after` starts at its end, `captions` are over it; at the end of the sequence (`atEnd`) `clips` (rendering nothing) and the captions keep it running |
//! | `jumpCut` | info | adjacent clips of one item at one speed and direction, both seen or heard, whose source skips at the cut (not across a frame hold or a speed ramp) |
//!
//! Checking a transition against its clips' media handles (`transitionHandles` in #4257) needs the
//! media's length and is not done yet: asking for it is an "unsupported" error, not an empty list.

use std::collections::BTreeMap;

use filmcraft_project::{ClipId, ItemId, ItemKind, Project, Sequence, Track, TrackItem, TrackKind};
use filmcraft_time::{FrameRate, Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::bad;
use crate::{EngineError, Result, Session};

const CMD: &str = "sequence.check";

/// The `params` shown for the command.
pub(crate) const PARAMS: &str = r#"{"item":id?,"minFrames":int?,"checks":["shortClip"|"pictureHole"|"soundHole"|"emptyStretch"|"jumpCut"]?}"#;

/// The checks run when `checks` is left out, in the order findings of one frame are listed.
const CHECKS: [&str; 5] = ["shortClip", "pictureHole", "soundHole", "emptyStretch", "jumpCut"];

/// Checks named in #4257 that are not implemented yet.
const UNSUPPORTED: [&str; 1] = ["transitionHandles"];

/// Clips shorter than this many frames are short clips unless `minFrames` says otherwise.
const DEFAULT_MIN_FRAMES: i64 = 3;

/// The largest `minFrames` taken: a day at 120 fps, far beyond any clip anyone means as "short".
const MAX_MIN_FRAMES: i64 = 120 * 60 * 60 * 24;

/// Media-time slack at a cut, as `filmcraft_edit::through` allows it: a speed change rounds each
/// piece's source range to whole ticks, so a cut that skips less than this is not a jump.
const SOURCE_SLACK: i64 = 2;

/// `sequence.check`: run the checks on the sequence `item` (the active sequence by default).
/// Changes nothing: not the project, its dirty flag, the history, the selection or the playhead.
pub(crate) fn run(s: &mut Session, p: &Value) -> Result<Value> {
    let checks = checks_p(p)?;
    let min_frames = min_frames_p(p)?;
    let id = match p.get("item") {
        None | Some(Value::Null) => s.state.active_sequence.ok_or(EngineError::NoSequence)?,
        // an exact id only: `7.5` is not sequence 7
        Some(v) => v.as_u64().map(ItemId).ok_or_else(|| bad(CMD, "`item` must be a sequence id"))?,
    };
    let q = s.project.sequence(id).ok_or_else(|| bad(CMD, format!("item {} is not a sequence", id.0)))?;
    let rate = q.settings.frame_rate;
    let duration = filmcraft_export::frame_span(rate, TimeRange::new(Tick::ZERO, q.duration())).1;
    let mut found: Vec<Finding> = Vec::new();
    let on = |c: &str| checks.contains(&c);
    if on("shortClip") {
        short_clips(q, rate, min_frames, &mut found);
    }
    if on("pictureHole") || on("soundHole") || on("emptyStretch") {
        coverage(&s.project, q, rate, duration, &checks, &mut found);
    }
    if on("jumpCut") {
        jump_cuts(&s.project, q, rate, &mut found);
    }
    found.sort_by_key(|f| (f.frame, CHECKS.iter().position(|c| *c == f.check), f.order));
    let warnings = found.iter().filter(|f| f.severity == "warning").count();
    Ok(json!({
        "sequence": id.0,
        "durationFrames": duration,
        "counts": {"warning": warnings, "info": found.len() - warnings},
        "findings": found.into_iter().map(|f| f.json).collect::<Vec<_>>(),
    }))
}

/// One finding: its JSON and where it sorts.
struct Finding {
    check: &'static str,
    severity: &'static str,
    /// The frame it starts at.
    frame: i64,
    /// Track order (video tracks first), to keep findings of one frame stable.
    order: usize,
    json: Value,
}

fn checks_p(p: &Value) -> Result<Vec<&'static str>> {
    let Some(v) = p.get("checks").filter(|v| !v.is_null()) else {
        return Ok(CHECKS.to_vec());
    };
    let Some(list) = v.as_array() else {
        return Err(bad(CMD, "`checks` must be a list of check names"));
    };
    let mut out = Vec::new();
    for c in list {
        let Some(name) = c.as_str() else {
            return Err(bad(CMD, "`checks` must be a list of check names"));
        };
        if UNSUPPORTED.contains(&name) {
            return Err(EngineError::Other(format!("{CMD}: the `{name}` check is unsupported for now")));
        }
        let Some(known) = CHECKS.iter().find(|k| **k == name) else {
            return Err(bad(CMD, format!("unknown check `{name}` (checks: {})", CHECKS.join(", "))));
        };
        if !out.contains(known) {
            out.push(*known);
        }
    }
    Ok(out)
}

fn min_frames_p(p: &Value) -> Result<i64> {
    match p.get("minFrames") {
        None | Some(Value::Null) => Ok(DEFAULT_MIN_FRAMES),
        Some(v) => match v.as_i64() {
            Some(n) if (1..=MAX_MIN_FRAMES).contains(&n) => Ok(n),
            _ => Err(bad(CMD, format!("`minFrames` must be a whole number of frames from 1 to {MAX_MIN_FRAMES}"))),
        },
    }
}

/// Every video and audio track with the name a track parameter takes (`V1`, `A2`…) and its
/// place in the list (video tracks first).
fn named_tracks(q: &Sequence) -> impl Iterator<Item = (String, usize, &Track)> {
    let video = q.video_tracks.iter().enumerate().map(|(i, t)| (format!("V{}", i + 1), i, t));
    let n = q.video_tracks.len();
    video.chain(q.audio_tracks.iter().enumerate().map(move |(i, t)| (format!("A{}", i + 1), n.saturating_add(i), t)))
}

/// The first frame whose start is at or after `t`: the first frame export samples from `t` on.
fn frame_from(rate: FrameRate, t: Tick) -> i64 {
    let f = rate.frame_at(t);
    if rate.tick_of(f) < t { f.saturating_add(1) } else { f }
}

/// The frames from `start` to `end` (end exclusive) that export samples inside them.
fn frames_of(rate: FrameRate, start: Tick, end: Tick) -> (i64, i64) {
    let a = frame_from(rate, start);
    (a, frame_from(rate, end).max(a))
}

/// A clip's frames on the timeline (start, end exclusive).
fn frames(rate: FrameRate, it: &TrackItem) -> (i64, i64) {
    frames_of(rate, it.start, it.end())
}

/// Whether a clip shows (video) or plays (audio) on its track, as the renderer decides it.
fn seen_or_heard(project: &Project, q: &Sequence, tr: &Track, it: &TrackItem) -> bool {
    if !it.enabled {
        return false;
    }
    match tr.kind {
        TrackKind::Video => tr.enabled && !is_adjustment(project, it),
        TrackKind::Audio => !tr.muted && (tr.solo || !q.audio_tracks.iter().any(|t| t.solo)),
    }
}

fn short_clips(q: &Sequence, rate: FrameRate, min_frames: i64, out: &mut Vec<Finding>) {
    for (name, order, tr) in named_tracks(q) {
        for it in &tr.items {
            let (a, b) = frames(rate, it);
            if b.saturating_sub(a) < min_frames {
                out.push(Finding {
                    check: "shortClip",
                    severity: "warning",
                    frame: a,
                    order,
                    json: json!({"check": "shortClip", "severity": "warning", "track": name, "trackId": tr.id.0, "clips": [it.id.0], "startFrame": a, "endFrame": b}),
                });
            }
        }
    }
}

/// A clip that shows or plays, or one that the sequence runs on without either.
struct Span {
    clip: ClipId,
    start: i64,
    end: i64,
}

impl Span {
    fn overlaps(&self, a: i64, b: i64) -> bool {
        self.start < b && self.end > a
    }
}

fn is_adjustment(project: &Project, it: &TrackItem) -> bool {
    project.item(it.item).is_some_and(|i| matches!(i.kind, ItemKind::AdjustmentLayer { .. }))
}

/// Picture, sound and empty-stretch findings: one sweep over the frames where what is seen or
/// heard changes.
fn coverage(project: &Project, q: &Sequence, rate: FrameRate, duration: i64, checks: &[&str], out: &mut Vec<Finding>) {
    let span = |it: &TrackItem| {
        let (start, end) = frames(rate, it);
        Span { clip: it.id, start, end }
    };
    let mut picture = Vec::new();
    let mut sound = Vec::new();
    // clips the sequence runs on although they render nothing
    let mut silent = Vec::new();
    for tr in q.all_tracks() {
        for it in &tr.items {
            match (seen_or_heard(project, q, tr, it), tr.kind) {
                (true, TrackKind::Video) => picture.push(span(it)),
                (true, TrackKind::Audio) => sound.push(span(it)),
                (false, _) => silent.push(span(it)),
            }
        }
        // A transition plays both of its clips over its whole length, into their handles (the
        // compositor and `render::audio` draw and mix each side that is enabled), so its frames
        // are seen or heard while one side renders, even past that clip's own end.
        for x in &tr.transitions {
            let (start, end) = frames_of(rate, x.start, x.end());
            for side in [x.from, x.to].into_iter().flatten() {
                let Some(it) = tr.item(side) else { continue };
                if seen_or_heard(project, q, tr, it) {
                    let s = Span { clip: it.id, start, end };
                    if tr.kind == TrackKind::Video { picture.push(s) } else { sound.push(s) }
                }
            }
        }
    }
    // captions over a stretch: shown when the caption track's output is on, but no picture of
    // their own, so they are named and do not fill it
    let captions_in = |a: i64, b: i64| {
        q.caption_tracks
            .iter()
            .flat_map(|t| &t.captions)
            .filter(|c| {
                let (s, e) = frames_of(rate, c.start, c.end());
                s < b && e > a
            })
            .map(|c| c.id.0)
            .collect::<Vec<_>>()
    };
    // +1 / -1 at each span's edges, per kind: (picture, sound)
    let mut edges: BTreeMap<i64, (i64, i64)> = BTreeMap::new();
    edges.entry(0).or_default();
    edges.entry(duration).or_default();
    for (spans, pic) in [(&picture, true), (&sound, false)] {
        for s in spans.iter().filter(|s| s.end > s.start) {
            for (at, d) in [(s.start, 1), (s.end, -1)] {
                let e = edges.entry(at).or_default();
                if pic {
                    e.0 = e.0.saturating_add(d);
                } else {
                    e.1 = e.1.saturating_add(d);
                }
            }
        }
    }
    // runs of frames with the same (picture, sound) state inside the sequence
    let mut runs: Vec<(i64, i64, bool, bool)> = Vec::new();
    let (mut pic, mut snd) = (0i64, 0i64);
    let mut it = edges.iter().peekable();
    while let Some((&at, &(dp, ds))) = it.next() {
        pic = pic.saturating_add(dp);
        snd = snd.saturating_add(ds);
        let Some(&(&next, _)) = it.peek() else { break };
        let (a, b) = (at.max(0), next.min(duration));
        if a >= b {
            continue;
        }
        let state = (pic > 0, snd > 0);
        match runs.last_mut() {
            Some(last) if last.1 == a && (last.2, last.3) == state => last.1 = b,
            _ => runs.push((a, b, state.0, state.1)),
        }
    }
    // a clip can have a span of its own and one per transition it is in: each is named once
    let unique = |ids: &mut dyn Iterator<Item = u64>| {
        let mut v: Vec<u64> = Vec::new();
        for id in ids {
            if !v.contains(&id) {
                v.push(id);
            }
        }
        v
    };
    let ids = |spans: &[Span], keep: &dyn Fn(&Span) -> bool| unique(&mut spans.iter().filter(|s| keep(s)).map(|s| s.clip.0));
    for (a, b, pic, snd) in runs {
        let (check, json) = match (pic, snd) {
            (true, true) => continue,
            (false, true) => (
                "pictureHole",
                json!({"check": "pictureHole", "severity": "warning", "startFrame": a, "endFrame": b, "audioClips": ids(&sound, &|s| s.overlaps(a, b))}),
            ),
            (true, false) => (
                "soundHole",
                json!({"check": "soundHole", "severity": "warning", "startFrame": a, "endFrame": b, "videoClips": ids(&picture, &|s| s.overlaps(a, b))}),
            ),
            (false, false) => {
                let shown: Vec<&Span> = picture.iter().chain(&sound).collect();
                let before = unique(&mut shown.iter().filter(|s| s.end == a && s.start < a).map(|s| s.clip.0));
                let after = unique(&mut shown.iter().filter(|s| s.start == b && s.end > b).map(|s| s.clip.0));
                let mut j = json!({
                    "check": "emptyStretch", "severity": "warning", "startFrame": a, "endFrame": b, "before": before, "after": after,
                    "captions": captions_in(a, b),
                });
                if b >= duration {
                    // what keeps the sequence running past the last thing seen or heard
                    j["atEnd"] = json!(true);
                    j["clips"] = json!(ids(&silent, &|s| s.overlaps(a, b)));
                }
                ("emptyStretch", j)
            }
        };
        if checks.contains(&check) {
            out.push(Finding { check, severity: "warning", frame: a, order: 0, json });
        }
    }
}

/// How far the source skips at the cut from `a` to the adjacent `b` of the same item, at the same
/// speed and direction, in media time (negative: back). None when they are not one shot.
fn source_skip(a: &TrackItem, b: &TrackItem) -> Option<Tick> {
    if a.end() != b.start || a.item != b.item || a.speed != b.speed || a.reverse != b.reverse || a.multicam != b.multicam {
        return None;
    }
    // a frame hold or a speed ramp (animated Time Remapping) does not play its source linearly
    let ramped = |it: &TrackItem| it.effect("time_remap").and_then(|e| e.param("speed")).is_some_and(|p| p.is_animated());
    if a.frame_hold.is_some() || b.frame_hold.is_some() || ramped(a) || ramped(b) {
        return None;
    }
    // reversed, the right piece shows the earlier media: forward in playback is back in the source
    let skip = if a.reverse { a.source_in - b.source_out() } else { b.source_in - a.source_out() };
    (skip.0.saturating_abs() > SOURCE_SLACK).then_some(skip)
}

/// Jump cuts between clips that are seen or heard (a cut nobody sees is not one). Linked picture
/// and sound cut together give a finding on each track.
fn jump_cuts(project: &Project, q: &Sequence, rate: FrameRate, out: &mut Vec<Finding>) {
    for (name, order, tr) in named_tracks(q) {
        for w in tr.items.windows(2) {
            let [a, b] = w else { continue };
            if !(seen_or_heard(project, q, tr, a) && seen_or_heard(project, q, tr, b)) {
                continue;
            }
            let Some(skip) = source_skip(a, b) else { continue };
            // the first frame that shows the incoming clip, as export samples it
            let frame = frame_from(rate, b.start);
            out.push(Finding {
                check: "jumpCut",
                severity: "info",
                frame,
                order,
                json: json!({
                    "check": "jumpCut", "severity": "info", "track": name, "trackId": tr.id.0, "clips": [a.id.0, b.id.0],
                    "frame": frame, "sourceSkip": skip.0, "sourceSkipSeconds": (skip.seconds() * 1000.0).round() / 1000.0,
                }),
            });
        }
    }
}

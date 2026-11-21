//! `sequence.review`: check an edit for the mistakes a viewer notices, before calling it done.
//!
//! Agents that edit video tend to report success whether or not the result holds up. This
//! read-only check lists concrete problems with their time, a severity and a suggested fix: black
//! gaps, flash frames, missing media, cuts inside a spoken word, hard audio cuts that are likely
//! to click, jump cuts and long holds. With `againstUndo: n` it also compares the sequence with how
//! it was `n` undo steps ago, so an agent can see what its edit actually changed.
//!
//! The checks are heuristics on the timeline, the transcripts and the rendered mix. They find
//! likely problems; they don't judge taste, and a clean report doesn't mean a good edit.

use filmcraft_project::{ItemId, ItemKind, Project, Sequence, TrackItem};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, NamedKeys, always, bad, f64_p, u64_p};
use crate::pacing::{Segment, is_picture, is_through_edit, picture_at, segments};
use crate::{EngineError, Result, Session};

const ID: &str = "sequence.review";
/// Issues listed at most; the counts cover all of them.
const MAX_ISSUES: usize = 300;
/// Mix measured on each side of an audio edit for the click check.
const POP_WINDOW_SECONDS: f64 = 0.003;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: ID,
        label: "Review Edit",
        menu: &[],
        shortcut: None,
        params: r#"{"sequence":id?,"againstUndo":n?,"flashFrames":n=3,"longSeconds":f=5,"wordMarginSeconds":f=0.04,"popDb":f=-30}"#,
        enabled: always,
        run: review,
        journal: false,
        named: NamedKeys::NONE,
    }]
}

struct Issue {
    kind: &'static str,
    severity: &'static str,
    at: Tick,
    clip: Option<u64>,
    detail: String,
    fix: String,
}

fn issue(kind: &'static str, severity: &'static str, at: Tick, clip: Option<u64>, detail: String, fix: impl Into<String>) -> Issue {
    Issue { kind, severity, at, clip, detail, fix: fix.into() }
}

fn secs(t: Tick) -> String {
    format!("{:.2} s", t.seconds())
}

/// Whether a clip's media is footage that has to be readable (not a graphic, nest or adjustment layer).
fn needs_media(p: &Project, item: ItemId) -> bool {
    matches!(p.item(item).map(|i| &i.kind), Some(ItemKind::Media(_) | ItemKind::Subclip { .. }))
}

fn picture_issues(p: &Project, q: &Sequence, segs: &[Segment], flash: i64, long: f64, out: &mut Vec<Issue>) {
    let fd = q.settings.frame_rate.frame_duration();
    let name = |c: Option<filmcraft_project::ClipId>| c.and_then(|c| q.find_item(c)).map(|(_, it)| it.name.clone()).unwrap_or_default();
    // black before the end of the last picture clip is a gap, also where an overlap or a nest's
    // overhang hides a clip that still runs on; black after every clip is just the sequence's tail
    let picture_end = q
        .video_tracks
        .iter()
        .filter(|tr| tr.enabled)
        .flat_map(|tr| tr.items.iter())
        .filter(|c| c.enabled && c.duration > Tick::ZERO && is_picture(p, c.item))
        .map(|c| c.end())
        .max()
        .unwrap_or(Tick::ZERO);
    for (k, g) in segs.iter().enumerate() {
        match g.clip {
            // black that starts before the last picture clip ends, up to that end (what follows
            // every clip is the sequence's tail, not a gap)
            None if g.start < picture_end => {
                let end = g.end.min(picture_end);
                out.push(issue(
                    "gap",
                    "error",
                    g.start,
                    None,
                    format!("{} of black between {} and {}", secs(end - g.start), secs(g.start), secs(end)),
                    "close the gap (ripple delete it) or fill it",
                ));
            }
            None => {}
            Some(c) => {
                if g.len() < Tick(fd.0.saturating_mul(flash)) {
                    let frames = g.len().0 / fd.0.max(1);
                    out.push(issue(
                        "flashFrame",
                        "error",
                        g.start,
                        Some(c.0),
                        format!("{} shows for only {frames} frame(s)", name(Some(c))),
                        "delete the sliver or extend the shot",
                    ));
                }
                if g.len().seconds() > long {
                    out.push(issue(
                        "longHold",
                        "info",
                        g.start,
                        Some(c.0),
                        format!("{} holds for {}", name(Some(c)), secs(g.len())),
                        "cut to another shot, add B-roll or trim it if the moment doesn't need the time",
                    ));
                }
                // a jump: the next shot continues the same media further on
                if let Some(next) = segs.get(k + 1).filter(|n| n.clip.is_some()) {
                    let (Some(a), Some(b)) = (picture_at(p, q, g.end - Tick(1)), picture_at(p, q, next.start)) else { continue };
                    // the media times on either side of the instant the picture switches (`a` may
                    // run on under `b`, or `b` start before it shows)
                    let (a_at, b_at) = (a.source_time_at(next.start), b.source_time_at(next.start));
                    let root = |c: &TrackItem| p.resolve_media(c.item).map_or(c.item, |(r, _, _)| r);
                    let moving = a.frame_hold.is_none() && b.frame_hold.is_none() && a.multicam == b.multicam;
                    if root(a) == root(b) && moving && !a.reverse && !b.reverse && a.speed == b.speed && b_at > a_at {
                        let skipped = (b_at - a_at).seconds() / a.speed.max(f64::EPSILON);
                        out.push(issue(
                            "jumpCut",
                            "info",
                            next.start,
                            Some(b.id.0),
                            format!("{} jumps {skipped:.2} s forward in the same shot", b.name),
                            "cover the cut with B-roll, a punch-in (scale up the second clip) or a short transition",
                        ));
                    }
                }
            }
        }
    }
}

/// Whether the media behind a clip is offline: marked so in the project, or found offline when the
/// media pool tried to open it (`Session::source` records that).
fn is_offline(s: &Session, item: ItemId) -> bool {
    let Some((media, clip, _)) = s.project.resolve_media(item) else { return false };
    let _ = s.source(item);
    clip.offline || s.media.offline_status(media).is_some()
}

fn media_issues(s: &Session, q: &Sequence, out: &mut Vec<Issue>) {
    let mut w = MediaWalk { s, seen: Default::default(), walked: Default::default(), budget: MAX_WALKED_CLIPS, out };
    w.walk(q, (Tick::ZERO, Tick(i64::MAX)), &|t| t, None, 0);
    if w.budget == 0 {
        w.out.push(issue(
            "offlineMedia",
            "info",
            Tick::ZERO,
            None,
            format!("the check of nested sequences stopped after {MAX_WALKED_CLIPS} clips: offline footage deeper in nests may not be listed"),
            "review the nested sequences on their own",
        ));
    }
}

/// Clips the offline check visits at most, nests included: nests of nests can multiply.
const MAX_WALKED_CLIPS: usize = 100_000;

/// The offline-media check's walk through a sequence and its nests.
struct MediaWalk<'a> {
    s: &'a Session,
    /// Media items already reported: once each, however often they show.
    seen: std::collections::BTreeSet<ItemId>,
    /// (sequence, range) parts already walked: another nest showing the same part holds the same
    /// media, so it isn't walked again.
    walked: std::collections::HashSet<(ItemId, Tick, Tick)>,
    /// Clips left to visit.
    budget: usize,
    out: &'a mut Vec<Issue>,
}

/// The clips of `q` that show within `window` (in `q`'s time), down into nests: a nest shows the
/// part of its sequence its source range covers (not past the contents), and its clips are
/// checked there. `to_top` maps `q`'s time to the reviewed sequence's; `nest` is the top-level
/// nest clip (id, name) the walk went through.
impl MediaWalk<'_> {
    fn walk(&mut self, q: &Sequence, window: (Tick, Tick), to_top: &dyn Fn(Tick) -> Tick, nest: Option<(u64, &str)>, depth: u32) {
        let s = self.s;
        for it in q.video_tracks.iter().chain(&q.audio_tracks).filter(|t| t.enabled).flat_map(|t| t.items.iter()).filter(|it| it.enabled) {
            if self.budget == 0 {
                return;
            }
            self.budget -= 1;
            // the part of the clip inside the window, in `q`'s time
            let (a, b) = (it.start.max(window.0), it.end().min(window.1));
            if a >= b {
                continue;
            }
            if let Some(child) = s.project.sequence(it.item) {
                if depth + 1 >= filmcraft_render::MAX_NEST_DEPTH || it.speed <= 0.0 {
                    continue;
                }
                // the child times that part plays: forward from the In point, reversed back from the Out point
                let scale = |d: Tick| Tick((d.0 as f64 * it.speed) as i64);
                let (lo, hi) = if it.reverse {
                    (it.source_out() - scale(b - it.start), it.source_out() - scale(a - it.start))
                } else {
                    (it.source_in + scale(a - it.start), it.source_in + scale(b - it.start))
                };
                let (lo, hi) = (lo.max(Tick::ZERO), hi.min(child.duration()));
                if lo >= hi || !self.walked.insert((it.item, lo, hi)) {
                    continue;
                }
                let (start, src_in, src_out, speed, reverse) = (it.start, it.source_in, it.source_out(), it.speed, it.reverse);
                let to_q = move |ct: Tick| {
                    let d = if reverse { src_out - ct } else { ct - src_in };
                    start + Tick((d.0 as f64 / speed) as i64)
                };
                let top = nest.unwrap_or((it.id.0, it.name.as_str()));
                self.walk(child, (lo, hi), &|ct| to_top(to_q(ct)), Some(top), depth + 1);
                continue;
            }
            if needs_media(&s.project, it.item) && !self.seen.contains(&it.item) && is_offline(s, it.item) && self.seen.insert(it.item) {
                let (clip, detail) = match nest {
                    None => (it.id.0, format!("{}: the media can't be opened (missing, moved or unreadable); it renders as a placeholder", it.name)),
                    Some((id, name)) => (
                        id,
                        format!(
                            "{} inside the nested sequence {name}: the media can't be opened (missing, moved or unreadable); it renders as a placeholder",
                            it.name
                        ),
                    ),
                };
                self.out.push(issue("offlineMedia", "error", to_top(a), Some(clip), detail, "relink the media (media.findMissing) before exporting"));
            }
        }
    }
}

/// Audio clip edges whose source time lands inside a spoken word, where the sound doesn't carry
/// on: a razor that leaves the audio continuous (a through edit) cuts no word.
fn word_issues(p: &Project, q: &Sequence, margin: Tick, out: &mut Vec<Issue>) {
    for t in q.audio_tracks.iter().filter(|t| t.enabled && !t.muted) {
        let items: Vec<&TrackItem> = t.items.iter().filter(|it| audible(it)).collect();
        for (k, it) in items.iter().enumerate() {
            if it.reverse || it.speed <= 0.0 {
                continue;
            }
            // a subclip's clips play its parent's media, which holds the transcript
            let root = p.resolve_media(it.item).map_or(it.item, |(r, _, _)| r);
            let Some(tr) = p.transcripts.get(&it.item).or_else(|| p.transcripts.get(&root)) else { continue };
            let continued_from = k.checked_sub(1).and_then(|j| items.get(j)).is_some_and(|prev| is_through_edit(p, prev, it));
            let continues = items.get(k + 1).is_some_and(|next| is_through_edit(p, it, next));
            for (edge, at, src) in [("starts", it.start, it.source_in), ("ends", it.end(), it.source_out())] {
                if (edge == "starts" && continued_from) || (edge == "ends" && continues) {
                    continue;
                }
                // a loaded transcript isn't guaranteed to be sorted by end: look at every word
                let Some(w) = tr.words.iter().find(|w| w.start + margin < src && src < w.end - margin) else { continue };
                // the edge moved to either word boundary (sequence time scales with speed)
                let to = |b: Tick| (at + Tick(((b - src).0 as f64 / it.speed) as i64)).seconds();
                let (keep, drop) = if edge == "starts" { (to(w.start), to(w.end)) } else { (to(w.end), to(w.start)) };
                let point = if edge == "starts" { "In point" } else { "Out point" };
                out.push(issue(
                    "midWord",
                    "warning",
                    at,
                    Some(it.id.0),
                    format!("{} {edge} inside the word \"{}\"", it.name, w.text),
                    format!("trim the {point} to {keep:.3} s to keep the word, or to {drop:.3} s to drop it"),
                ));
            }
        }
    }
}

/// Whether the clip is heard: Essential Sound ▸ Mute takes it out of the mix.
fn audible(c: &TrackItem) -> bool {
    c.enabled && !c.essential.as_ref().is_some_and(|e| e.mute)
}

/// The clip's Volume effect level (dB, keyframes included) at media time `t` (where effect
/// keyframes are keyed); 0 without one.
fn volume_db_at(c: &TrackItem, t: Tick) -> f64 {
    c.effects
        .iter()
        .filter(|e| e.enabled && e.effect == "volume")
        .filter(|e| !matches!(e.params.get("bypass").map(|p| p.value_at(t)), Some(filmcraft_project::ParamValue::Bool(true))))
        .filter_map(|e| match e.params.get("level").map(|p| p.value_at(t)) {
            Some(filmcraft_project::ParamValue::Float(db)) => Some(db),
            _ => None,
        })
        .sum()
}

/// Peak (dBFS) of what `c` plays in the few milliseconds next to its start or end (`at_end`), in
/// playing direction: its own audio stream and routed channels, with its gain and Volume level
/// there. None when it has no sound to read.
fn edge_peak_db(s: &Session, c: &TrackItem, at_end: bool) -> Option<f64> {
    let src = s.source(c.item)?;
    let sr = src.info().audio_streams.get(c.audio_stream)?.sample_rate.max(1);
    let n = ((POP_WINDOW_SECONDS * f64::from(sr)).ceil() as usize).max(1);
    // the media time at the edge: a reversed clip starts at its Out point and ends at its In point
    let edge = if at_end != c.reverse { c.source_out() } else { c.source_in };
    let pos = edge.to_units_floor(i64::from(sr));
    // the media that plays next to the edge lies before `pos` at a forward end or a reversed start
    let from = if at_end != c.reverse { pos - n as i64 } else { pos };
    let b = src.audio_stream(c.audio_stream, from.max(0), n, sr).ok()?;
    let routed = |k: usize| c.source_channels.is_empty() || c.source_channels.iter().any(|&ch| usize::from(ch) == k);
    let pk = b.channels.iter().enumerate().filter(|(k, _)| routed(*k)).flat_map(|(_, ch)| ch.iter()).fold(0f32, |m, x| m.max(x.abs()));
    // effect keyframes are keyed in media time: evaluate at the media time of the edge frame
    let at = if at_end { c.end() - Tick(1) } else { c.start };
    Some(if pk > 0.0 { 20.0 * f64::from(pk).log10() + c.gain_db + volume_db_at(c, c.effect_time_at(at)) } else { -120.0 })
}

/// Hard audio edits where an edited clip is loud at the cut: likely clicks. Each side is measured
/// from the clip's own sound, so loud audio on another track doesn't count against a quiet edit.
fn pop_issues(s: &Session, q: &Sequence, pop_db: f64, out: &mut Vec<Issue>) {
    let end = q.duration();
    let mut found: Vec<(Tick, u64, f64, f64)> = Vec::new();
    for t in q.audio_tracks.iter().filter(|t| t.enabled && !t.muted) {
        let covered = |at: Tick| t.transitions.iter().any(|tr| tr.start <= at && at <= tr.start + tr.duration);
        let items: Vec<&TrackItem> = t.items.iter().filter(|it| audible(it)).collect();
        let mut times: Vec<Tick> = items.iter().flat_map(|it| [it.start, it.end()]).filter(|at| *at > Tick::ZERO && *at < end && !covered(*at)).collect();
        times.sort();
        times.dedup();
        for at in times {
            let outgoing = items.iter().find(|it| it.end() == at).copied();
            let incoming = items.iter().find(|it| it.start == at).copied();
            // a through edit plays on without a change
            if let (Some(a), Some(b)) = (outgoing, incoming)
                && is_through_edit(&s.project, a, b)
            {
                continue;
            }
            // as the track plays it: its fader applies to every clip on it
            let before = outgoing.and_then(|c| edge_peak_db(s, c, true)).map_or(-120.0, |db| db + t.volume_db);
            let after = incoming.and_then(|c| edge_peak_db(s, c, false)).map_or(-120.0, |db| db + t.volume_db);
            let clip = incoming.or(outgoing).map_or(0, |c| c.id.0);
            if before.max(after) > pop_db && !found.iter().any(|f| f.0 == at) {
                found.push((at, clip, before, after));
            }
        }
    }
    found.sort_by_key(|f| f.0);
    for (at, clip, before, after) in found {
        out.push(issue(
            "audioPop",
            "warning",
            at,
            Some(clip),
            format!("hard audio cut at {} with the edited sound at {before:.0} dB before and {after:.0} dB after: likely to click", secs(at)),
            "add a short audio crossfade (2–10 frames) at the edit",
        ));
    }
}

/// The sequence's picture in numbers, for the before/after comparison.
fn shape(p: &Project, q: &Sequence) -> Value {
    let segs = segments(p, q);
    let shots: Vec<&Segment> = segs.iter().filter(|g| g.clip.is_some()).collect();
    let duration = q.duration().seconds();
    let cuts = segs.len().saturating_sub(1);
    json!({
        "durationSeconds": duration,
        "pictureSeconds": shots.iter().map(|g| g.len().seconds()).sum::<f64>(),
        "shotCount": shots.len(),
        "cutsPerMinute": if duration > 0.0 { cuts as f64 * 60.0 / duration } else { 0.0 },
        "longestShotSeconds": shots.iter().map(|g| g.len().seconds()).fold(0.0, f64::max),
    })
}

fn review(s: &mut Session, p: &Value) -> Result<Value> {
    let seq_id = match u64_p(p, "sequence") {
        Some(id) => ItemId(id),
        None => s.state.active_sequence.ok_or(EngineError::NoSequence)?,
    };
    let q = s.project.sequence(seq_id).ok_or_else(|| bad(ID, format!("{} is not a sequence", seq_id.0)))?;
    let flash = u64_p(p, "flashFrames").unwrap_or(3).clamp(1, 1000) as i64;
    let long = f64_p(p, "longSeconds").unwrap_or(5.0);
    let margin = f64_p(p, "wordMarginSeconds").unwrap_or(0.04);
    let pop_db = f64_p(p, "popDb").unwrap_or(-30.0);
    if !(long.is_finite() && long > 0.0 && margin.is_finite() && margin >= 0.0 && pop_db.is_finite()) {
        return Err(bad(ID, "longSeconds must be positive, wordMarginSeconds ≥ 0 and popDb a number"));
    }
    let against = match u64_p(p, "againstUndo") {
        None => None,
        Some(n) if n >= 1 && usize::try_from(n).is_ok_and(|n| n <= s.history.undo.len()) => Some(s.history.undo[s.history.undo.len() - n as usize].1.clone()),
        Some(n) => return Err(bad(ID, format!("againstUndo {n} is out of range: there are {} undo steps", s.history.undo.len()))),
    };

    let mut issues = Vec::new();
    let segs = segments(&s.project, q);
    picture_issues(&s.project, q, &segs, flash, long, &mut issues);
    media_issues(s, q, &mut issues);
    word_issues(&s.project, q, Tick::from_seconds_f64(margin), &mut issues);
    pop_issues(s, q, pop_db, &mut issues);
    let rank = |sev: &str| match sev {
        "error" => 0,
        "warning" => 1,
        _ => 2,
    };
    issues.sort_by(|a, b| rank(a.severity).cmp(&rank(b.severity)).then(a.at.cmp(&b.at)));
    let count = |sev: &str| issues.iter().filter(|i| i.severity == sev).count();
    let mut out = json!({
        "sequence": seq_id.0,
        "errors": count("error"),
        "warnings": count("warning"),
        "infos": count("info"),
        "issues": issues.iter().take(MAX_ISSUES).map(|i| json!({
            "kind": i.kind, "severity": i.severity, "time": i.at.0, "seconds": i.at.seconds(),
            "clip": i.clip, "detail": i.detail, "fix": i.fix,
        })).collect::<Vec<_>>(),
        "issuesTruncated": issues.len() > MAX_ISSUES,
        "notes": "Heuristic checks of the timeline, transcripts and mix. They find likely mistakes; they don't judge taste, and no issues doesn't mean a good edit. Look at the cuts too (cut_inspect, render_frame).",
    });
    if let Some(old) = against {
        let after = shape(&s.project, q);
        out["before"] = match old.sequence(seq_id) {
            Some(oq) => shape(&old, oq),
            None => Value::Null,
        };
        out["after"] = after;
    }
    Ok(out)
}

#[cfg(test)]
mod tests;

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
use crate::pacing::{Segment, is_through_edit, picture_at, picture_clips, segments};
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
    let picture_end = segs.iter().rev().find(|g| g.clip.is_some()).map_or(Tick::ZERO, |g| g.end);
    let clips = picture_clips(p, q);
    for (k, g) in segs.iter().enumerate() {
        match g.clip {
            None if g.end <= picture_end => out.push(issue(
                "gap",
                "error",
                g.start,
                None,
                format!("{} of black between {} and {}", secs(g.len()), secs(g.start), secs(g.end)),
                "close the gap (ripple delete it) or fill it",
            )),
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
                    let (Some(a), Some(b)) = (picture_at(&clips, g.end - Tick(1)), picture_at(&clips, next.start)) else { continue };
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
    let mut seen = std::collections::BTreeSet::new();
    for it in q.video_tracks.iter().chain(&q.audio_tracks).filter(|t| t.enabled).flat_map(|t| t.items.iter()) {
        if it.enabled && needs_media(&s.project, it.item) && !seen.contains(&it.item) && is_offline(s, it.item) && seen.insert(it.item) {
            out.push(issue(
                "offlineMedia",
                "error",
                it.start,
                Some(it.id.0),
                format!("{}: the media can't be opened (missing, moved or unreadable); it renders as a placeholder", it.name),
                "relink the media (media.findMissing) before exporting",
            ));
        }
    }
}

/// Audio clip edges whose source time lands inside a spoken word, where the sound doesn't carry
/// on: a razor that leaves the audio continuous (a through edit) cuts no word.
fn word_issues(p: &Project, q: &Sequence, margin: Tick, out: &mut Vec<Issue>) {
    for t in q.audio_tracks.iter().filter(|t| t.enabled && !t.muted) {
        let items: Vec<&TrackItem> = t.items.iter().filter(|it| it.enabled).collect();
        for (k, it) in items.iter().enumerate() {
            if it.reverse || it.speed <= 0.0 {
                continue;
            }
            let Some(tr) = p.transcripts.get(&it.item) else { continue };
            let continued_from = k.checked_sub(1).and_then(|j| items.get(j)).is_some_and(|prev| is_through_edit(p, prev, it));
            let continues = items.get(k + 1).is_some_and(|next| is_through_edit(p, it, next));
            for (edge, at, src) in [("starts", it.start, it.source_in), ("ends", it.end(), it.source_out())] {
                if (edge == "starts" && continued_from) || (edge == "ends" && continues) {
                    continue;
                }
                let i = tr.words.partition_point(|w| w.end <= src);
                let Some(w) = tr.words.get(i).filter(|w| w.start + margin < src && src < w.end - margin) else { continue };
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

/// Hard audio edits where the mix is loud: likely clicks.
fn pop_issues(s: &Session, q: &Sequence, pop_db: f64, out: &mut Vec<Issue>) {
    let sr = q.settings.sample_rate.max(1);
    let win = (POP_WINDOW_SECONDS * f64::from(sr)).ceil() as usize;
    let end = q.duration();
    let mut edits: Vec<(Tick, u64)> = Vec::new();
    for t in q.audio_tracks.iter().filter(|t| t.enabled && !t.muted) {
        let covered = |at: Tick| t.transitions.iter().any(|tr| tr.start <= at && at <= tr.start + tr.duration);
        let items: Vec<&TrackItem> = t.items.iter().filter(|it| it.enabled).collect();
        for (k, it) in items.iter().enumerate() {
            for at in [it.start, it.end()] {
                if at <= Tick::ZERO || at >= end || covered(at) {
                    continue;
                }
                // a through edit plays on without a change
                let prev = items.get(k.wrapping_sub(1)).filter(|p| p.end() == at && at == it.start);
                let next = items.get(k + 1).filter(|n| n.start == at && at == it.end());
                if prev.is_some_and(|pv| is_through_edit(&s.project, pv, it)) || next.is_some_and(|n| is_through_edit(&s.project, it, n)) {
                    continue;
                }
                if !edits.iter().any(|(e, _)| *e == at) {
                    edits.push((at, it.id.0));
                }
            }
        }
    }
    if edits.is_empty() {
        return;
    }
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    edits.sort();
    for (at, clip) in edits {
        let pos = at.to_units_floor(i64::from(sr));
        // `win` samples on each side of the edit; near the start there are fewer before it
        let from = (pos - win as i64).max(0);
        let split = (pos - from) as usize;
        let b = filmcraft_render::audio::mix_sequence(&s.project, q, from, split + win, &provider);
        let peak_db = |range: std::ops::Range<usize>| {
            let pk = b.channels.iter().flat_map(|c| c.get(range.clone()).unwrap_or(&[]).iter()).fold(0f32, |m, x| m.max(x.abs()));
            if pk > 0.0 { 20.0 * f64::from(pk).log10() } else { -120.0 }
        };
        let (before, after) = (peak_db(0..split), peak_db(split..split + win));
        if before.max(after) > pop_db {
            out.push(issue(
                "audioPop",
                "warning",
                at,
                Some(clip),
                format!("hard audio cut at {} with the mix at {before:.0} dB before and {after:.0} dB after: likely to click", secs(at)),
                "add a short audio crossfade (2–10 frames) at the edit",
            ));
        }
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

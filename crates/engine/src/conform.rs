//! Conform the picture's sections to a voice-over take (#5265): `sequence.conformSections` fits
//! each section (range marker) to how long the narrator spent on it, from the take's prompter log
//! ([`crate::prompter::TakeLog`]); `sequence.conformSectionsReport` says what it would do and
//! changes nothing. The timeline edit itself is [`filmcraft_edit::conform`].
//!
//! **Prompt time over the take.** The log gives the prompt time `p(x)` at every take time `x` (the
//! playhead minus the record point): between two entries it runs from the earlier one's
//! `promptAfter` at its `rateAfter`, `p(x) = promptAfter + (x - takeTime) * rateAfter / 100`
//! rounded down, and it jumps at each Next, Previous or Return to Playhead. Only `0 <= x <= T`
//! counts, `T` the take clip's length: the pre-roll before the record point and moves logged after
//! the recording ended (a punch-out's post-roll) are not narration. If the narrator never moved the
//! prompter, `p(x)` is the playhead itself and nothing changes.
//!
//! **When a section was delivered.** For a picture time `t`, the *crossing* `L(t)` is the take time
//! at which the prompt time reached `t` for the last time: the end of the last stretch of the take
//! in which `p(x) < t`. A section `[a, b)` was entered at `L(a)` and left at `L(b)`; its delivered
//! length is `L(b) - L(a)`. Because `L` only grows with `t`, the delivered stretches of the
//! sections follow one another in picture order and never overlap, whatever the narrator did:
//!
//! - **Entered once, left once** (the normal case): from when the prompt reached its start (running
//!   into it, or Next to its start) to when it left (running past its end, or Next out of it early).
//! - **Re-entered** (Previous back into it): it ends when the narrator finally left it. The time
//!   spent on a section they had moved on to too soon, before coming back, belongs to this one: the
//!   picture cannot go back and forth, and the narration went on about this section.
//! - **Skipped** (Next pressed twice, or a move past it): it was entered and left at the same take
//!   time, or less than a frame apart. Not conformed (its picture stays as it is) and reported as
//!   skipped: removing a section's whole picture is not something to do on a guess.
//! - **Partly recorded:** a section the take started inside (`p(0)` in it), ended inside, went back
//!   before and ended there, or only came into after its start (a move into its middle) was not
//!   delivered from start to end, so its delivered length says nothing about how long it should be.
//!   Not conformed, with the reason. Only sections the take covers whole are candidates: entered at
//!   their start (the crossing lands less than a frame into it) and left in the take.
//! - **Outside the take** (the prompt was past it all along, or never reached it): untouched, and
//!   reported as `outside`.
//! - **Nested or overlapping range markers** among the sections the take touches: refused. The
//!   prompter shows one of them at a time, but there is no single edit point at which an outer
//!   section "ends" while a nested one is shown, so there is no sound way to fit both.
//!
//! **Rounding.** The edits land on frame boundaries: a section's new start and end are the frame
//! boundaries nearest to where the narrator entered and left it (record point plus crossing), and
//! its new length is their difference. Rounding positions, not lengths, keeps every boundary
//! within half a frame of the narration however many sections there are: the errors do not add
//! up. A conformed section whose rounded length would be under a frame counts as skipped. Sections
//! whose marker does not start and end on frame boundaries are not conformed.
//!
//! **Offset.** A section's picture starts where the narrator started it when everything between the
//! record point and that section was conformed; time between sections (no marker) and sections that
//! were not conformed keep their length, so a difference there shifts every later section by the
//! same amount. Each row reports it as `offset` (new start minus where the narrator started it).
//!
//! **The edit** is [`filmcraft_edit::conform::conform`], one undo step: an extension holds the last
//! frame of each video track, a trim removes the end as Extract would, everything after ripples on
//! every sync-locked track, and the take clip stays as it is. Each conformed section's marker gets
//! its new length; every later marker moves with the ripple ([`crate::sequence_tools::ripple_markers`],
//! also when Ripple Sequence Markers is off: the markers are what is being conformed), and a point
//! marker inside a removed range goes, as in a ripple delete. Captions on sync-locked caption tracks
//! move with the ripple like any other track content (they were timed to the old picture).
//!
//! **Which take.** The last finished take of this session ([`crate::prompter::PrompterState`]): the
//! log is not saved with the clip yet (that needs a project schema change). The command checks that
//! the take's clip is still where it was placed and that the sections are where the log saw them.
//! [`plan`] takes the log and the clip explicitly, so a log read from a saved clip can go the same
//! way once it is stored.

use filmcraft_edit::EditCtx;
use filmcraft_edit::conform::{Fit, locked_in_the_way, obstacle};
use filmcraft_project::{ClipId, ItemId, Marker, MarkerId, Sequence, TrackId, TrackItem};
use filmcraft_time::Tick;
use serde::Serialize;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, has_seq};
use crate::prompter::{MAX_RATE_PERCENT, MIN_RATE_PERCENT, MoveKind, TakeLog, section_marks};
use crate::{EngineError, Result, Session};

/// Where a take's clip was placed: what the conform checks is still so.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeClip {
    /// `Session::document_epoch` of the project the take was placed in.
    pub epoch: u64,
    pub sequence: ItemId,
    pub track: TrackId,
    pub clip: ClipId,
    pub start: Tick,
    pub duration: Tick,
    pub source_in: Tick,
}

impl TakeClip {
    pub fn of(epoch: u64, sequence: ItemId, track: TrackId, it: &TrackItem) -> Self {
        Self { epoch, sequence, track, clip: it.id, start: it.start, duration: it.duration, source_in: it.source_in }
    }
    /// Whether `it` on `track` is the clip as it was placed (not moved, trimmed or slipped).
    fn unchanged(&self, track: TrackId, it: &TrackItem) -> bool {
        track == self.track && it.start == self.start && it.duration == self.duration && it.source_in == self.source_in && it.speed == 1.0 && !it.reverse
    }
}

// ------------------------------------------------------------------------------- prompt time

/// One stretch of the take in which the prompt time runs without a jump: `p(x) = p0 + (x - x0) *
/// rate / 100` (rounded down) for `lo <= x < hi` (`x <= hi` for the last stretch).
#[derive(Clone, Copy, Debug)]
struct Stretch {
    lo: i128,
    hi: i128,
    x0: i128,
    p0: i128,
    rate: i128,
}

impl Stretch {
    fn at(&self, x: i128) -> i128 {
        self.p0 + (x - self.x0) * self.rate / 100
    }
}

/// The prompt time over take times `0..=end`, rebuilt from a log.
#[derive(Clone, Debug)]
pub struct Curve {
    stretches: Vec<Stretch>,
    end: i128,
}

fn bounded(t: i128) -> bool {
    (i128::from(Tick::MIN.0)..=i128::from(Tick::MAX.0)).contains(&t)
}

fn tick(v: i128) -> Tick {
    Tick(i64::try_from(v.clamp(i128::from(Tick::MIN.0), i128::from(Tick::MAX.0))).unwrap_or(0))
}

impl Curve {
    /// Check `log` and rebuild the prompt time over a take of length `take_len`. A log that does not
    /// replay (entries out of order, rates out of range, a prompt time that does not follow from the
    /// entry before, moves dropped) is refused with the entry it fails at.
    pub fn from_log(log: &TakeLog, take_len: Tick) -> std::result::Result<Self, String> {
        let bad = |k: usize, why: &str| format!("the take's prompter log does not replay at entry {k}: {why}");
        if log.dropped > 0 {
            return Err(format!("the take's prompter log is incomplete: {} moves were not kept", log.dropped));
        }
        let first = log.entries.first().ok_or("the take's prompter log is empty")?;
        if first.kind != MoveKind::Start || log.entries.iter().skip(1).any(|e| e.kind == MoveKind::Start) {
            return Err(bad(0, "it must start with the take's start, once"));
        }
        if take_len <= Tick::ZERO || !bounded(i128::from(take_len.0)) || !bounded(i128::from(log.record_start.0)) {
            return Err("the take has no length".into());
        }
        let rate_ok = |r: u32| (MIN_RATE_PERCENT..=MAX_RATE_PERCENT).contains(&r);
        let mut prev: Option<Stretch> = None;
        for (k, e) in log.entries.iter().enumerate() {
            let (x, before, after) = (i128::from(e.take_time.0), i128::from(e.prompt_before.0), i128::from(e.prompt_after.0));
            if ![x, before, after, i128::from(e.playhead.0)].into_iter().all(bounded) {
                return Err(bad(k, "a time is out of range"));
            }
            if i128::from(e.playhead.0) - i128::from(log.record_start.0) != x {
                return Err(bad(k, "its take time is not its playhead minus the record point"));
            }
            if !rate_ok(e.rate_before) || !rate_ok(e.rate_after) {
                return Err(bad(k, "a rate is out of range"));
            }
            match prev {
                Some(p) if x < p.x0 => return Err(bad(k, "it is earlier than the entry before it")),
                Some(p) if before != p.at(x) || i128::from(e.rate_before) != p.rate => {
                    return Err(bad(k, "the prompt time or rate before it does not follow from the entry before"));
                }
                None if x > 0 => return Err(bad(k, "the log starts after the record point")),
                _ => {}
            }
            prev = Some(Stretch { lo: x, hi: x, x0: x, p0: after, rate: i128::from(e.rate_after) });
        }
        // the narration is take times 0..=end: moves at or after its end are not part of it
        let end = i128::from(take_len.0);
        let used: Vec<_> = log.entries.iter().take_while(|e| i128::from(e.take_time.0) < end).collect();
        let mut stretches = Vec::with_capacity(used.len());
        for (k, e) in used.iter().enumerate() {
            let x0 = i128::from(e.take_time.0);
            let hi = used.get(k + 1).map_or(end, |n| i128::from(n.take_time.0)).min(end);
            let lo = x0.max(0);
            if lo < hi || k + 1 == used.len() {
                stretches.push(Stretch { lo, hi, x0, p0: i128::from(e.prompt_after.0), rate: i128::from(e.rate_after) });
            }
        }
        if stretches.is_empty() {
            return Err("the take's prompter log does not cover the take".into());
        }
        if stretches.iter().any(|s| !bounded(s.at(s.hi))) {
            return Err("the take's prompt times run out of range".into());
        }
        Ok(Self { stretches, end })
    }

    /// The prompt time at take time `x` (clamped to the take).
    pub fn at(&self, x: Tick) -> Tick {
        let x = i128::from(x.0).clamp(0, self.end);
        let s = self.stretches.iter().rev().find(|s| s.lo <= x).or(self.stretches.first());
        tick(s.map_or(0, |s| s.at(x)))
    }

    /// When the prompt time reached `t` for the last time during the take.
    pub fn crossing(&self, t: Tick) -> Crossing {
        let t = i128::from(t.0);
        let mut found = Crossing::Never;
        let n = self.stretches.len();
        for (i, s) in self.stretches.iter().enumerate() {
            if s.at(s.lo) >= t {
                continue;
            }
            // the first take time in this stretch at which the prompt time is at or past t
            let need = t - s.p0;
            let y = s.x0 + (100 * need + s.rate - 1) / s.rate;
            let last = i + 1 == n;
            found = if y < s.hi || (last && y <= s.hi) {
                Crossing::At { at: tick(y), landed: tick(s.at(y)) }
            } else if last {
                Crossing::NotReached
            } else {
                // a move at the end of this stretch took it there
                let next = self.stretches.get(i + 1).map_or(t, |n| n.at(n.lo));
                Crossing::At { at: tick(s.hi), landed: tick(next) }
            };
        }
        found
    }

    /// Whether the prompt time was in `[a, b)` at any take time.
    fn visits(&self, a: Tick, b: Tick) -> bool {
        let (a, b) = (i128::from(a.0), i128::from(b.0));
        let n = self.stretches.len();
        self.stretches.iter().enumerate().any(|(i, s)| {
            let top = if i + 1 == n { s.at(s.hi) } else { s.at((s.hi - 1).max(s.lo)) };
            s.at(s.lo) < b && top >= a
        })
    }
}

/// The last time the prompt time reached a picture time during the take.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Crossing {
    /// The prompt time was at or past it the whole take.
    Never,
    /// At take time `at` it reached it, landing at `landed` (the time itself when it ran into it,
    /// where a move put it otherwise), and stayed at or past it to the end.
    At { at: Tick, landed: Tick },
    /// It was before it when the take ended.
    NotReached,
}

// ------------------------------------------------------------------------------- the plan

/// What the conform does to one section.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum Change {
    Extend,
    Trim,
    None,
    NotConformed,
    Outside,
}

/// One section of the plan (and of the report).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Row {
    pub marker: MarkerId,
    pub name: String,
    pub start: Tick,
    pub planned_length: Tick,
    /// Take time at which the narrator entered and left it, when the take covers it.
    pub entered_at: Option<Tick>,
    pub left_at: Option<Tick>,
    /// `leftAt - enteredAt`, exact; None for a section the take does not cover whole.
    pub delivered_length: Option<Tick>,
    pub change: Change,
    /// How much it is extended or trimmed by (whole frames; zero otherwise).
    pub by: Tick,
    pub by_frames: i64,
    pub new_length: Tick,
    /// Where its picture starts after the conform minus where the narrator started it (sequence
    /// time); None for sections not conformed.
    pub offset: Option<Tick>,
    pub reason: Option<String>,
}

/// The conform worked out for a take: one row per section, and the fits to make.
#[derive(Clone, Debug, PartialEq)]
pub struct Plan {
    pub rows: Vec<Row>,
    pub fits: Vec<(MarkerId, Fit)>,
}

/// A section the conform leaves as it is, and why.
fn not(row: &mut Row, change: Change, why: &str) {
    row.change = change;
    row.reason = Some(why.to_string());
}

fn end_of(m: &Marker) -> Tick {
    m.start.saturating_add(m.duration)
}

/// Work out the conform of `seq`'s sections to a take whose clip `take` starts at the log's
/// record point and lasts `take.duration`. Refused when the log does not replay or sections the
/// take touches overlap.
pub fn plan(seq: &Sequence, log: &TakeLog, take: &TakeClip) -> std::result::Result<Plan, String> {
    let curve = Curve::from_log(log, take.duration)?;
    let rate = seq.settings.frame_rate;
    let frame = rate.frame_duration();
    let rs = log.record_start;
    let mut sections: Vec<&Marker> = seq.markers.iter().filter(|m| m.duration > Tick::ZERO).collect();
    sections.sort_by_key(|m| (m.start, m.id.0));
    let target = |x: Tick| rate.snap_nearest(rs.saturating_add(x));
    let p0 = curve.at(Tick::ZERO);

    let mut rows = Vec::with_capacity(sections.len());
    let mut fits = Vec::new();
    let mut shift = Tick::ZERO;
    for m in &sections {
        let (a, b) = (m.start, end_of(m));
        let mut row = Row {
            marker: m.id,
            name: m.name.clone(),
            start: a,
            planned_length: m.duration,
            entered_at: None,
            left_at: None,
            delivered_length: None,
            change: Change::NotConformed,
            by: Tick::ZERO,
            by_frames: 0,
            new_length: m.duration,
            offset: None,
            reason: None,
        };
        match (curve.crossing(a), curve.crossing(b)) {
            (Crossing::Never, Crossing::Never) => not(&mut row, Change::Outside, "before the take: the prompter was past it when the take began"),
            (Crossing::Never, _) if p0 >= a && p0 < b => not(&mut row, Change::NotConformed, "the take started inside it, so its start was not delivered"),
            (Crossing::Never, _) => not(&mut row, Change::NotConformed, "the prompter came into it after the take began without passing its start"),
            (Crossing::NotReached, _) if curve.visits(a, b) => not(&mut row, Change::NotConformed, "the take went back before it and ended there"),
            (Crossing::NotReached, _) => not(&mut row, Change::Outside, "after the take: the prompter did not reach it"),
            (Crossing::At { .. }, Crossing::NotReached | Crossing::Never) => not(&mut row, Change::NotConformed, "the take ended inside it"),
            (Crossing::At { at: xa, landed }, Crossing::At { at: xb, .. }) => {
                row.entered_at = Some(xa);
                row.left_at = Some(xb);
                if landed >= b {
                    not(&mut row, Change::NotConformed, "skipped: the prompter passed it without stopping");
                } else if landed.saturating_sub(a) >= frame {
                    not(&mut row, Change::NotConformed, "the prompter jumped into its middle, so its start was not delivered");
                } else {
                    row.delivered_length = Some(xb.saturating_sub(xa));
                    let (na, nb) = (target(xa), target(xb));
                    let new_length = nb.saturating_sub(na);
                    let delta = new_length.saturating_sub(m.duration);
                    if new_length < frame {
                        not(&mut row, Change::NotConformed, "skipped: delivered in less than a frame");
                    } else if rate.snap(a) != a || rate.snap(b) != b {
                        not(&mut row, Change::NotConformed, "its marker does not start and end on frame boundaries");
                    } else if delta == Tick::ZERO {
                        row.change = Change::None;
                        row.offset = Some(a.saturating_add(shift).saturating_sub(na));
                    } else if let Some(why) = obstacle(seq, Fit { end: b, delta }, Some(take.clip)) {
                        not(&mut row, Change::NotConformed, &why);
                    } else {
                        row.change = if delta > Tick::ZERO { Change::Extend } else { Change::Trim };
                        row.by = Tick(delta.0.saturating_abs());
                        row.by_frames = rate.frames_between(Tick::ZERO, row.by);
                        row.new_length = new_length;
                        row.offset = Some(a.saturating_add(shift).saturating_sub(na));
                        shift = shift.saturating_add(delta);
                        fits.push((m.id, Fit { end: b, delta }));
                    }
                }
            }
        }
        rows.push(row);
    }
    // sections the take touches must not overlap any other
    for r in rows.iter().filter(|r| r.change != Change::Outside) {
        let Some(m) = sections.iter().find(|m| m.id == r.marker) else { continue };
        if let Some(o) = sections.iter().find(|o| o.id != m.id && o.start < end_of(m) && m.start < end_of(o)) {
            return Err(format!(
                "the sections \"{}\" and \"{}\" overlap; conform needs range markers that follow one another without overlapping",
                m.name, o.name
            ));
        }
    }
    Ok(Plan { rows, fits })
}

/// Make `plan` on `seq`: the timeline edit, then the markers. Returns the point markers a trim
/// removed. Either all of it is made or `seq` is unchanged.
pub fn apply(seq: &mut Sequence, plan: &Plan, keep: ClipId, ctx: &mut EditCtx) -> Result<Vec<MarkerId>> {
    let fits: Vec<Fit> = plan.fits.iter().map(|(_, f)| *f).collect();
    let mut work = seq.clone();
    filmcraft_edit::conform::conform(&mut work, &fits, Some(keep), ctx).map_err(|e| EngineError::Other(e.to_string()))?;
    let before: Vec<MarkerId> = work.markers.iter().map(|m| m.id).collect();
    for (marker, fit) in plan.fits.iter().rev() {
        crate::sequence_tools::ripple_markers(&mut work.markers, fit.end, fit.delta);
        let m = work.markers.iter_mut().find(|m| m.id == *marker).ok_or_else(|| EngineError::Other("a section's marker went missing".into()))?;
        m.duration = m.duration.saturating_add(fit.delta);
    }
    let removed = before.into_iter().filter(|id| !work.markers.iter().any(|m| m.id == *id)).collect();
    *seq = work;
    Ok(removed)
}

// ------------------------------------------------------------------------------- the session

/// The last finished take's log and clip, checked against the active sequence: everything that
/// makes conforming to it meaningless is refused with the reason.
fn last_take(s: &Session) -> std::result::Result<(&TakeLog, TakeClip, &Sequence), String> {
    has_seq(s)?;
    if s.voiceover.recording() {
        return Err("a voice-over is recording; stop it first".into());
    }
    let log = s.prompter.last_take.as_ref().ok_or("no voice-over take has been recorded in this session; record one with the prompter first")?;
    let take = s.prompter.last_take_clip.ok_or("the last voice-over take was not placed in the sequence")?;
    if take.epoch != s.document_epoch {
        return Err("the project was replaced since the last take".into());
    }
    let seq = s.active_sequence().ok_or("no sequence is open")?;
    if Some(take.sequence) != s.state.active_sequence || log.sequence != take.sequence {
        let name = s.project.item(take.sequence).map(|i| i.name.clone()).unwrap_or_default();
        return Err(format!("the last take was recorded in another sequence (\"{name}\"); open it to conform"));
    }
    if take.start != log.record_start {
        return Err("the take's log and clip do not match".into());
    }
    match seq.find_item(take.clip) {
        None => return Err("the take's clip was deleted".into()),
        Some((track, it)) if !take.unchanged(track, it) => return Err("the take's clip was moved, trimmed or changed in speed since it was recorded".into()),
        Some(_) => {}
    }
    let now = section_marks(seq);
    if now != log.sections {
        let changed = log
            .sections
            .iter()
            .find(|m| !now.contains(m))
            .or_else(|| now.iter().find(|m| !log.sections.contains(m)))
            .and_then(|m| seq.markers.iter().find(|x| x.id == m.marker))
            .map(|m| format!(" (\"{}\")", m.name))
            .unwrap_or_default();
        return Err(format!("the sections changed since the take{changed}; undo the change or record a new take"));
    }
    Ok((log, take, seq))
}

/// The plan for the last take, and why the edit would be refused if it would.
fn prepare(s: &Session) -> std::result::Result<(Plan, TakeClip), String> {
    let (log, take, seq) = last_take(s)?;
    Ok((plan(seq, log, &take)?, take))
}

fn nothing_to_change(plan: &Plan) -> String {
    if plan.rows.iter().any(|r| r.change == Change::NotConformed) {
        "nothing to change: the sections the take covers whole were delivered at their planned length, and the others cannot be conformed (see Conform Sections Report)".into()
    } else {
        "nothing to change: every section the take covers was delivered at its planned length".into()
    }
}

fn can_conform(s: &Session) -> std::result::Result<(), String> {
    let (plan, take) = prepare(s)?;
    if plan.fits.is_empty() {
        return Err(nothing_to_change(&plan));
    }
    let seq = s.active_sequence().ok_or("no sequence is open")?;
    let from = plan.fits.iter().map(|(_, f)| f.from()).min().unwrap_or(Tick::ZERO);
    if let Some(track) = locked_in_the_way(seq, from, Some(take.clip)) {
        return Err(format!("{track} is locked and would stop the ripple; unlock it, or turn off its sync lock, to conform"));
    }
    Ok(())
}

fn totals(plan: &Plan) -> Value {
    let count = |c: Change| plan.rows.iter().filter(|r| r.change == c).count();
    let sum = |c: Change| plan.rows.iter().filter(|r| r.change == c).fold(Tick::ZERO, |a, r| a.saturating_add(r.by));
    let net = plan.fits.iter().fold(Tick::ZERO, |a, (_, f)| a.saturating_add(f.delta));
    json!({
        "sections": plan.rows.len(),
        "extended": count(Change::Extend),
        "trimmed": count(Change::Trim),
        "unchanged": count(Change::None),
        "notConformed": count(Change::NotConformed),
        "outside": count(Change::Outside),
        "extendedBy": sum(Change::Extend),
        "trimmedBy": sum(Change::Trim),
        "change": net,
    })
}

/// What a run did, or a report found it would do.
struct Outcome {
    applied: bool,
    blocked: Option<String>,
    before: Tick,
    after: Tick,
    removed: Vec<MarkerId>,
}

fn result(s: &Session, plan: &Plan, take: &TakeClip, o: Outcome) -> Value {
    let Outcome { applied, blocked, before, after, removed } = o;
    let track = s.active_sequence().map(|q| filmcraft_edit::track_label(q, take.track)).unwrap_or_default();
    json!({
        "applied": applied,
        "blocked": blocked,
        "take": {"clip": take.clip, "track": track, "start": take.start, "duration": take.duration},
        "sections": plan.rows,
        "totals": totals(plan),
        "durationBefore": before,
        "durationAfter": after,
        "removedMarkers": removed,
    })
}

/// `sequence.conformSectionsReport`: what `sequence.conformSections` would do, changing nothing.
fn report(s: &mut Session, _: &Value) -> Result<Value> {
    let (plan, take) = prepare(s).map_err(EngineError::Other)?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let before = seq.duration();
    let mut work = seq.clone();
    let mut next = s.project.next_id;
    let mut ctx =
        EditCtx { next_id: &mut next, media_duration: &|_| None, media_start: &|_| Tick::ZERO, min_duration: seq.settings.frame_rate.frame_duration() };
    let (blocked, after, removed) = if plan.fits.is_empty() {
        (None, before, Vec::new())
    } else {
        match apply(&mut work, &plan, take.clip, &mut ctx) {
            Ok(removed) => (None, work.duration(), removed),
            Err(e) => (Some(e.to_string()), before, Vec::new()),
        }
    };
    Ok(result(s, &plan, &take, Outcome { applied: false, blocked, before, after, removed }))
}

/// `sequence.conformSections`: conform the sections to the last take, as one undo step.
fn run(s: &mut Session, _: &Value) -> Result<Value> {
    let (plan, take) = prepare(s).map_err(EngineError::Other)?;
    let before = s.active_sequence().ok_or(EngineError::NoSequence)?.duration();
    let removed = s.edit_sequence("Conform Sections", |q, ctx, _| apply(q, &plan, take.clip, ctx))?;
    let after = s.active_sequence().ok_or(EngineError::NoSequence)?.duration();
    Ok(result(s, &plan, &take, Outcome { applied: true, blocked: None, before, after, removed }))
}

pub fn commands() -> Vec<CommandSpec> {
    let named = crate::commands::NamedKeys::NONE;
    vec![
        CommandSpec {
            id: "sequence.conformSections",
            label: "Conform Sections to Last Take",
            menu: &["Sequence"],
            shortcut: None,
            params: "{}",
            enabled: can_conform,
            run,
            journal: true,
            named,
        },
        CommandSpec {
            id: "sequence.conformSectionsReport",
            label: "Conform Sections Report",
            menu: &[],
            shortcut: None,
            params: "{}",
            enabled: crate::commands::always,
            run: report,
            journal: false,
            named,
        },
    ]
}

#[cfg(test)]
mod tests;

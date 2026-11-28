//! The prompter: what a narrator should be saying at a given time, read from the sequence's range
//! markers. The Program monitor's Prompter overlay draws [`prompt_at`]; the `prompter.inspect`
//! query returns the same result to the automation interface.
//!
//! **Sections.** A section is a range marker (any marker kind with a duration): its name is the
//! section name, its range is the section. Point markers are not sections. When range markers
//! overlap, the one that started last is shown while it lasts (a marker nested in another
//! interrupts it, and the outer one is shown again after it); markers that start together show the
//! shorter one first; a remaining tie goes to the marker listed first.
//!
//! **Notes.** The marker's comment holds the section's text ([`parse_notes`]). A line starting
//! with `-`, `*` or `•` and a space is a talking point; it may end in a duration in brackets,
//! `[m:ss]` or `[h:mm:ss]`. Points without a duration share the time the others leave
//! ([`allocate`]). When the written durations leave no time for the rest, or add up to more than
//! the section, the result carries an [`Overrun`]. Every other line is script, which runs from the
//! section's start to its end in proportion to its characters.
//!
//! **Time is a parameter.** [`prompt_at`] answers for any time; the overlay passes the prompt time.
//! The function only reads the sequence: it changes nothing and is never part of a render.
//!
//! **Prompt time.** The prompter has its own position, normally the playhead. `prompter.next` and
//! `prompter.previous` move it to the start of the next or previous step; `prompter.toPlayhead`,
//! starting playback (the UI) and starting a voice-over take put it back. Off the playhead it is
//! held as an [`Anchor`] (prompt time P at playhead H) and keeps moving with the playhead, so the
//! gap stays the same through playback, seeks and stops until something returns it. It is session
//! state ([`PrompterState`] on `Session::prompter`): never saved and never an undo step; another
//! sequence or a replaced project starts in step. `prompter.inspect` reports `promptTime`,
//! `playhead`, `gap` (prompt time minus playhead) and `direction` (`ahead`, `behind` or null).
//!
//! **Rate.** The prompt time can also run faster or slower than the playhead (#5263): `prompter.faster`
//! and `prompter.slower` change the rate by [`RATE_STEP_PERCENT`] (5%) between [`MIN_RATE_PERCENT`]
//! (50%) and [`MAX_RATE_PERCENT`] (200%), `prompter.resetRate` returns it to 100%. The rate is held in
//! the [`Anchor`] as a whole percent, and the prompt time is `P + (playhead - H) * rate / 100`. Every
//! change re-anchors at the current prompt time and playhead, so the position is continuous and only
//! the speed from there on differs. Only the prompter's own clock changes: playback, its speed and
//! the voice-over recording are not touched. The anchor is kept while the rate is not 100%, even when
//! the prompter is at the playhead, so a rate of 100% is the same as no anchor at gap zero. Next,
//! Previous and Return to Playhead keep the rate (they move the position; Return to Playhead puts the
//! position back in step, and the reset command is the one that resets the rate). Starting playback
//! (the UI) and starting a take reset the rate along with the gap, so it never survives into the next
//! take; stopping playback (the UI) resets the rate and keeps the gap, as it always has. A seek while
//! a rate is in force moves the prompt time by the rate times the jump; with the rate reset on stop,
//! that only happens when seeking during playback. `prompter.inspect` reports `rate` (percent).
//!
//! **Steps.** A section's steps are its start and the starts of its talking points, or, without
//! points, of its script paragraphs (text after a blank line); a section without either is one
//! step. Points that start after the section's end (an overrun) are not steps. Only what the
//! overlay shows counts: points hidden under a nested section are skipped, and a parent section is
//! a step again where it resumes. Next goes to the first step after the prompt time, across a gap
//! to the next section's start. Previous goes to the start of the current step once it is more than
//! [`PREVIOUS_GRACE_SECONDS`] in, else to the step before (from the first point of a section, the
//! previous section's last point; from a gap, the last point before it). Both are disabled, with a
//! reason, without range markers, after the last step (Next) and at or before the first (Previous).
//!
//! **Take log.** During a voice-over take each move and each rate change is logged ([`TakeLog`],
//! [`PromptMove`]): the first entry is where the prompt was when the capture began, then one per
//! move or rate change (kind `rate`, with `rateBefore` and `rateAfter` in percent, and the same
//! prompt time before and after). Every entry carries both rates; between entries the prompt time
//! runs at `rateAfter` of the last one, so the prompt time at any take time can be rebuilt from
//! the log. It is kept for
//! the take in progress and the last finished take (`prompter.takeLog`), and `audio.voiceover.stop`
//! returns it as `promptLog`. It is not stored on the recorded clip yet (that needs a project
//! schema change). The log also records the sections (range markers) as they were when the take
//! began, and the session keeps where the take's clip was placed ([`PrompterState::last_take_clip`]):
//! `sequence.conformSections` ([`crate::conform`]) fits the sections to the take from them.

use std::cmp::Reverse;

use filmcraft_project::{ClipId, ItemId, Marker, MarkerId, Sequence, TrackId};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, has_seq, sequence_time_p};
use crate::{EngineError, Result, Session};

/// Only the start of a longer comment is read, so a damaged or pasted-in comment cannot stall
/// drawing; the result says it was cut ([`Section::truncated`]).
pub const MAX_COMMENT_BYTES: usize = 64 * 1024;
/// Talking points read from one comment.
pub const MAX_POINTS: usize = 100;
/// Characters kept of one talking point.
pub const MAX_POINT_CHARS: usize = 500;
/// The longest written duration accepted (99 hours); a longer one is left in the point's text.
const MAX_DURATION_SECONDS: u64 = 99 * 3600;
/// The next section is announced this long before the current one ends (at most half the section).
pub const NEXT_LEAD_SECONDS: i64 = 10;

/// One talking point as written: its text and its duration, if one was given.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Note {
    pub text: String,
    pub duration: Option<Tick>,
}

/// A marker comment split into talking points and script.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Notes {
    pub points: Vec<Note>,
    /// The lines that are not talking points, in order; blank runs become one blank line.
    pub script: String,
    /// The comment was longer than the prompter reads, or had more points than it shows.
    pub truncated: bool,
}

/// Split a marker comment into talking points and script. Any text is accepted; nothing fails.
pub fn parse_notes(comment: &str) -> Notes {
    let mut truncated = false;
    let text = if comment.len() > MAX_COMMENT_BYTES {
        truncated = true;
        let end = (0..=MAX_COMMENT_BYTES).rev().find(|&i| comment.is_char_boundary(i)).unwrap_or(0);
        comment.get(..end).unwrap_or("")
    } else {
        comment
    };
    let mut points = Vec::new();
    let mut script: Vec<&str> = Vec::new();
    for line in text.lines() {
        if let Some(item) = list_item(line) {
            if points.len() < MAX_POINTS {
                points.push(note(item, &mut truncated));
            } else {
                truncated = true;
            }
        } else if !line.trim().is_empty() || script.last().is_some_and(|l| !l.is_empty()) {
            script.push(line.trim_end());
        }
    }
    while script.last().is_some_and(|l| l.is_empty()) {
        script.pop();
    }
    Notes { points, script: script.join("\n"), truncated }
}

/// The text of a list line (`- text`, `* text`, `• text`), or None for any other line.
fn list_item(line: &str) -> Option<&str> {
    let t = line.trim_start();
    let rest = t.strip_prefix('-').or_else(|| t.strip_prefix('*')).or_else(|| t.strip_prefix('•'))?;
    rest.starts_with(char::is_whitespace).then(|| rest.trim())
}

/// A talking point with its trailing `[m:ss]` duration taken off.
fn note(item: &str, truncated: &mut bool) -> Note {
    let (mut text, duration) = match item.strip_suffix(']').and_then(|body| body.rfind('[').map(|open| (body, open))) {
        Some((body, open)) => match body.get(open + 1..).and_then(parse_duration) {
            Some(d) => (body.get(..open).unwrap_or("").trim_end().to_string(), Some(d)),
            None => (item.to_string(), None),
        },
        None => (item.to_string(), None),
    };
    if text.chars().count() > MAX_POINT_CHARS {
        *truncated = true;
        text = text.chars().take(MAX_POINT_CHARS).collect();
        text.push('…');
    }
    Note { text, duration }
}

/// `m:ss` or `h:mm:ss` (seconds and, with hours, minutes below 60); None for anything else.
pub fn parse_duration(s: &str) -> Option<Tick> {
    let parts: Vec<&str> = s.trim().split(':').collect();
    if !(2..=3).contains(&parts.len()) || parts.iter().any(|p| p.is_empty() || p.len() > 4 || !p.bytes().all(|b| b.is_ascii_digit())) {
        return None;
    }
    let n: Vec<u64> = parts.iter().filter_map(|p| p.parse().ok()).collect();
    let secs = match n.as_slice() {
        [m, s] if *s < 60 => m.checked_mul(60)?.checked_add(*s)?,
        [h, m, s] if *m < 60 && *s < 60 => h.checked_mul(3600)?.checked_add(m.checked_mul(60)?)?.checked_add(*s)?,
        _ => return None,
    };
    if secs > MAX_DURATION_SECONDS {
        return None;
    }
    Some(Tick::from_units(i64::try_from(secs).ok()?, 1))
}

/// The talking points' durations add up to more than the section, or leave no time for the
/// points that have none.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Overrun {
    /// The written durations added up.
    pub points: Tick,
    /// The section's length.
    pub section: Tick,
    /// How much longer the points are than the section (zero when they fill it exactly).
    pub over: Tick,
    /// Points without a duration that get no time.
    pub untimed: usize,
}

/// Each point's `[start, end)` in sequence time, laid end to end from `start`: a written duration
/// is kept, and the points without one share what is left of `length` equally (the last of them
/// takes the rounding remainder, so they end with the section). Points always keep their order;
/// when the written durations run past the section the later points start after it ends.
pub fn allocate(points: &[Note], start: Tick, length: Tick) -> (Vec<(Tick, Tick)>, Option<Overrun>) {
    let length = length.max(Tick::ZERO);
    let written = points.iter().filter_map(|p| p.duration).fold(Tick::ZERO, |a, d| a.saturating_add(d.max(Tick::ZERO)));
    let untimed = points.iter().filter(|p| p.duration.is_none()).count();
    let rest = length.saturating_sub(written).max(Tick::ZERO);
    let count = i64::try_from(untimed).unwrap_or(i64::MAX).max(1);
    let share = Tick(rest.0 / count);
    let remainder = Tick(rest.0 - share.0.saturating_mul(count));
    let last_untimed = points.iter().rposition(|p| p.duration.is_none());
    let mut cursor = start;
    let spans = points
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let d = match p.duration {
                Some(d) => d.max(Tick::ZERO),
                None if Some(i) == last_untimed => share.saturating_add(remainder),
                None => share,
            };
            let span = (cursor, cursor.saturating_add(d));
            cursor = span.1;
            span
        })
        .collect();
    let overrun = (written > length || (untimed > 0 && rest == Tick::ZERO)).then(|| Overrun {
        points: written,
        section: length,
        over: written.saturating_sub(length).max(Tick::ZERO),
        untimed: if rest == Tick::ZERO { untimed } else { 0 },
    });
    (spans, overrun)
}

fn end_of(m: &Marker) -> Tick {
    m.start.saturating_add(m.duration)
}

fn is_section(m: &Marker) -> bool {
    m.duration > Tick::ZERO
}

/// The range marker shown at `t`: of those covering it, the one that started last, then the
/// shorter, then the first listed. None between and outside sections.
pub fn section_at(seq: &Sequence, t: Tick) -> Option<&Marker> {
    seq.markers.iter().filter(|m| is_section(m) && m.start <= t && t < end_of(m)).fold(None, |best: Option<&Marker>, m| match best {
        Some(b) if (b.start, Reverse(b.duration)) >= (m.start, Reverse(m.duration)) => Some(b),
        _ => Some(m),
    })
}

/// When the shown section changes after `t` (the current one ends or is interrupted), when the
/// next one is shown (later than the change when a gap follows) and which one it is. None after
/// the sequence's last section.
fn next_after<'a>(seq: &'a Sequence, t: Tick, current: Option<&Marker>) -> Option<(Tick, Tick, &'a Marker)> {
    let next_start = |after: Tick, before: Option<Tick>| {
        seq.markers.iter().filter(|m| is_section(m) && m.start > after && before.is_none_or(|b| m.start < b)).map(|m| m.start).min()
    };
    let change = match current {
        // a section that starts inside the current one takes over at its start
        Some(c) => next_start(t, Some(end_of(c))).unwrap_or(end_of(c)),
        None => next_start(t, None)?,
    };
    if let Some(m) = section_at(seq, change) {
        return Some((change, change, m));
    }
    // a gap after the current section: the next one to start
    let at = next_start(change, None)?;
    section_at(seq, at).map(|m| (change, at, m))
}

/// Progress through a talking point.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PointState {
    Done,
    Current,
    Upcoming,
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Point {
    pub text: String,
    /// The duration written after it, if any.
    pub written: Option<Tick>,
    /// Its share of the section, in sequence time.
    pub start: Tick,
    pub end: Tick,
    pub state: PointState,
}

/// The script and how far into it the narrator should be.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Script {
    pub text: String,
    /// Characters in `text`.
    pub chars: usize,
    /// The character the narrator should be reading now (0 at the section's start, `chars` at its end).
    pub offset: usize,
    /// The paragraph `offset` is in (0-based; paragraphs are separated by blank lines).
    pub paragraph: usize,
}

/// Character offsets where the script's paragraphs start: 0, and the first character after each
/// blank line ([`parse_notes`] collapses blank runs to one blank line).
pub fn paragraph_starts(script: &str) -> Vec<usize> {
    let mut starts = vec![0];
    let mut prev = None;
    for (i, c) in script.chars().enumerate() {
        if prev == Some('\n') && c == '\n' {
            starts.push(i + 1);
        }
        prev = Some(c);
    }
    starts.retain(|&i| i < script.chars().count());
    starts.dedup();
    starts
}

/// The script character read `elapsed` into a section of `duration`: linear in characters.
fn script_offset(chars: usize, elapsed: Tick, duration: Tick) -> usize {
    let progress = if duration > Tick::ZERO { (elapsed.0 as f64 / duration.0 as f64).clamp(0.0, 1.0) } else { 0.0 };
    ((chars as f64) * progress).floor().clamp(0.0, chars as f64) as usize
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub marker: MarkerId,
    pub name: String,
    pub start: Tick,
    pub duration: Tick,
    pub end: Tick,
    pub elapsed: Tick,
    pub remaining: Tick,
    /// `elapsed / duration`, 0 to 1.
    pub progress: f64,
    pub points: Vec<Point>,
    /// Index of the current point in `points`.
    pub current: Option<usize>,
    pub script: Option<Script>,
    pub overrun: Option<Overrun>,
    /// Part of the comment was not read ([`parse_notes`]).
    pub truncated: bool,
}

/// The section shown after the current one (or after the gap the time is in).
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Next {
    pub marker: MarkerId,
    pub name: String,
    /// When it is shown: its start, or the end of a section nested in it.
    pub at: Tick,
    pub starts_in: Tick,
    /// Whether the prompter announces it now: in the last [`NEXT_LEAD_SECONDS`] before the current
    /// section ends or is interrupted (the second half of a section shorter than twice that), and
    /// always between sections.
    pub soon: bool,
}

/// The caption at the time, from the top caption track that is turned on and has one there.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptionLine {
    pub track: TrackId,
    pub caption: ClipId,
    pub start: Tick,
    pub end: Tick,
    pub lines: Vec<String>,
}

/// What the prompter shows at one time.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Prompt {
    pub time: Tick,
    /// Range markers in the sequence (0: there are no sections to show).
    pub sections: usize,
    pub section: Option<Section>,
    pub next: Option<Next>,
    pub caption: Option<CaptionLine>,
}

/// The caption shown at `t`: the top enabled caption track with a caption covering `t`.
pub fn caption_at(seq: &Sequence, t: Tick) -> Option<CaptionLine> {
    seq.caption_tracks.iter().filter(|tr| tr.enabled).find_map(|tr| {
        tr.caption_at(t).map(|c| CaptionLine {
            track: tr.id,
            caption: c.id,
            start: c.start,
            end: c.start.saturating_add(c.duration),
            lines: c.plain_lines().into_iter().map(|l| l.trim().to_string()).filter(|l| !l.is_empty()).collect(),
        })
    })
}

fn section(m: &Marker, t: Tick) -> Section {
    let notes = parse_notes(&m.comment);
    let end = end_of(m);
    let elapsed = t.saturating_sub(m.start).clamp(Tick::ZERO, m.duration);
    let progress = if m.duration > Tick::ZERO { (elapsed.0 as f64 / m.duration.0 as f64).clamp(0.0, 1.0) } else { 0.0 };
    let (spans, overrun) = allocate(&notes.points, m.start, m.duration);
    let points: Vec<Point> = notes
        .points
        .into_iter()
        .zip(spans)
        .map(|(n, (start, end))| {
            let state = if start <= t && t < end {
                PointState::Current
            } else if t >= end {
                PointState::Done
            } else {
                PointState::Upcoming
            };
            Point { text: n.text, written: n.duration, start, end, state }
        })
        .collect();
    let current = points.iter().position(|p| p.state == PointState::Current);
    let script = (!notes.script.is_empty()).then(|| {
        let chars = notes.script.chars().count();
        let offset = script_offset(chars, elapsed, m.duration);
        let paragraph = paragraph_starts(&notes.script).iter().filter(|&&p| p <= offset).count().saturating_sub(1);
        Script { text: notes.script, chars, offset, paragraph }
    });
    Section {
        marker: m.id,
        name: m.name.clone(),
        start: m.start,
        duration: m.duration,
        end,
        elapsed,
        remaining: m.duration.saturating_sub(elapsed),
        progress,
        points,
        current,
        script,
        overrun,
        truncated: notes.truncated,
    }
}

/// What the prompter shows at time `t` (sequence ticks) of `seq`. Any time is accepted, also one
/// before the sequence starts or after it ends.
pub fn prompt_at(seq: &Sequence, t: Tick) -> Prompt {
    let current = section_at(seq, t);
    let next = next_after(seq, t, current).map(|(change, at, m)| {
        let starts_in = at.saturating_sub(t);
        let lead = |c: &Marker| Tick(TICKS_PER_SECOND.saturating_mul(NEXT_LEAD_SECONDS)).min(Tick(c.duration.0 / 2));
        let soon = current.is_none_or(|c| change.saturating_sub(t) <= lead(c));
        Next { marker: m.id, name: m.name.clone(), at, starts_in, soon }
    });
    Prompt {
        time: t,
        sections: seq.markers.iter().filter(|m| is_section(m)).count(),
        section: current.map(|m| section(m, t)),
        next,
        caption: caption_at(seq, t),
    }
}

// ------------------------------------------------------------------------------- prompt time

/// Previous within this long of a step's start goes to the step before it; later, to the
/// step's own start (the usual rule for "previous" controls).
pub const PREVIOUS_GRACE_SECONDS: i64 = 2;
/// A tempo step: one Faster or Slower changes the rate by this many percentage points.
pub const RATE_STEP_PERCENT: u32 = 5;
/// The slowest rate the prompter runs at, in percent of the playhead's speed.
pub const MIN_RATE_PERCENT: u32 = 50;
/// The fastest rate the prompter runs at, in percent of the playhead's speed.
pub const MAX_RATE_PERCENT: u32 = 200;
/// The prompter keeps pace with the playhead.
pub const NORMAL_RATE_PERCENT: u32 = 100;
/// Moves kept in one take's log; later moves are counted in [`TakeLog::dropped`].
pub const MAX_TAKE_LOG_ENTRIES: usize = 10_000;

/// Where the prompter is when it is not in step with the playhead: prompt time `prompt` at
/// playhead `playhead`, in `sequence` of the document `epoch`. From there the prompt time moves
/// with the playhead at `rate` percent, so at 100% the gap stays `prompt - playhead`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Anchor {
    pub sequence: ItemId,
    pub epoch: u64,
    pub prompt: Tick,
    pub playhead: Tick,
    /// How fast the prompt time runs relative to the playhead, in percent
    /// ([`MIN_RATE_PERCENT`] to [`MAX_RATE_PERCENT`]).
    pub rate: u32,
}

/// `delta` at `percent` of its length, rounded toward zero and saturating.
pub fn scale(delta: Tick, percent: u32) -> Tick {
    let v = i128::from(delta.0) * i128::from(percent) / 100;
    Tick(i64::try_from(v).unwrap_or(if v < 0 { i64::MIN } else { i64::MAX }))
}

impl Anchor {
    /// The prompt time when the playhead is at `playhead`.
    pub fn at(&self, playhead: Tick) -> Tick {
        self.prompt.saturating_add(scale(playhead.saturating_sub(self.playhead), self.rate))
    }
}

/// The prompter's session state: never saved with the project and never an undo step.
#[derive(Clone, Debug, Default)]
pub struct PrompterState {
    /// None: the prompter follows the playhead.
    pub anchor: Option<Anchor>,
    /// The log of the voice-over take in progress.
    pub take: Option<TakeLog>,
    /// The log of the last finished take.
    pub last_take: Option<TakeLog>,
    /// Where the last finished take was placed, when it was (`sequence.conformSections` checks
    /// that the clip is still there). None for a discarded or failed take.
    pub last_take_clip: Option<crate::conform::TakeClip>,
}

/// The anchor, when it belongs to the active sequence of the current document (another sequence
/// or a replaced project starts in step).
fn anchor(s: &Session) -> Option<Anchor> {
    s.prompter.anchor.filter(|a| Some(a.sequence) == s.state.active_sequence && a.epoch == s.document_epoch)
}

/// The prompt time when the playhead is at `playhead` (the overlay passes the playhead it draws).
pub fn prompt_time_at(s: &Session, playhead: Tick) -> Tick {
    anchor(s).map_or(playhead, |a| a.at(playhead))
}

/// The prompt time now.
pub fn prompt_time(s: &Session) -> Tick {
    prompt_time_at(s, s.playhead())
}

/// The rate the prompt time runs at relative to the playhead, in percent (100 when in step).
pub fn rate(s: &Session) -> u32 {
    anchor(s).map_or(NORMAL_RATE_PERCENT, |a| a.rate)
}

/// Put the prompter at `prompt` for `playhead` at `rate` percent. At 100% and at the playhead it is
/// in step, which is no anchor.
fn set_position(s: &mut Session, prompt: Tick, playhead: Tick, rate: u32) -> Result<()> {
    let sequence = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    s.prompter.anchor = (prompt != playhead || rate != NORMAL_RATE_PERCENT).then_some(Anchor { sequence, epoch: s.document_epoch, prompt, playhead, rate });
    Ok(())
}

/// Prompt time minus playhead: positive when the prompter is ahead of the picture.
pub fn gap(s: &Session) -> Tick {
    prompt_time(s).saturating_sub(s.playhead())
}

/// Which side of the picture the prompter is on.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Direction {
    /// The prompt time is later than the playhead: the narrator moved on first.
    Ahead,
    /// The prompt time is earlier: the picture moved on without the narrator.
    Behind,
}

/// The direction of a gap; None when there is none.
pub fn direction(gap: Tick) -> Option<Direction> {
    match gap.0.signum() {
        1 => Some(Direction::Ahead),
        -1 => Some(Direction::Behind),
        _ => None,
    }
}

// ------------------------------------------------------------------------------- steps

/// The start times of a section's steps after its start: its talking points, else its script
/// paragraphs. Points that start at or after the section's end (an overrun) are not steps.
fn inner_steps(m: &Marker) -> Vec<Tick> {
    let notes = parse_notes(&m.comment);
    let end = end_of(m);
    if !notes.points.is_empty() {
        let (spans, _) = allocate(&notes.points, m.start, m.duration);
        return spans.into_iter().map(|(a, _)| a).filter(|&a| a > m.start && a < end).collect();
    }
    let chars = notes.script.chars().count();
    if chars == 0 {
        return Vec::new();
    }
    paragraph_starts(&notes.script)
        .into_iter()
        .skip(1)
        .filter_map(|c| {
            // the first tick at which the script position has reached the paragraph
            let ideal = (c as i128 * m.duration.0 as i128 + chars as i128 - 1) / chars as i128;
            let mut e = Tick(i64::try_from(ideal).ok()?);
            for _ in 0..8 {
                if script_offset(chars, e, m.duration) >= c {
                    break;
                }
                e = e.saturating_add(Tick(1));
            }
            let t = m.start.saturating_add(e);
            (t > m.start && t < end).then_some(t)
        })
        .collect()
}

/// Times the prompter can be moved to, with the section they belong to (None: a time where the
/// shown section may change, such as a parent resuming after a nested marker), of the sections
/// `keep` selects.
fn candidates(seq: &Sequence, keep: impl Fn(&Marker) -> bool) -> Vec<(Tick, Option<MarkerId>)> {
    let mut out = Vec::new();
    for m in seq.markers.iter().filter(|m| is_section(m) && keep(m)) {
        out.push((m.start, Some(m.id)));
        out.extend(inner_steps(m).into_iter().map(|t| (t, Some(m.id))));
        out.push((end_of(m), None));
    }
    out
}

/// Whether `t` is a step: inside the section shown there, and either one of that section's own
/// steps or the moment that section comes into view.
fn is_step(seq: &Sequence, t: Tick, of: Option<MarkerId>) -> bool {
    let Some(shown) = section_at(seq, t) else { return false };
    of == Some(shown.id) || t.0.checked_sub(1).is_none_or(|b| section_at(seq, Tick(b)).map(|m| m.id) != Some(shown.id))
}

/// The first step after `p`.
pub fn next_step(seq: &Sequence, p: Tick) -> Option<Tick> {
    let mut c: Vec<_> = candidates(seq, |m| end_of(m) > p).into_iter().filter(|(t, _)| *t > p).collect();
    c.sort_by_key(|(t, of)| (*t, of.is_none()));
    c.into_iter().find(|(t, of)| is_step(seq, *t, *of)).map(|(t, _)| t)
}

/// Where Previous goes from `p`: the start of the step `p` is in when that started more than
/// [`PREVIOUS_GRACE_SECONDS`] ago (or no step comes before it), else the step before it. None
/// before the first step, or at it.
pub fn previous_step(seq: &Sequence, p: Tick) -> Option<Tick> {
    let mut c: Vec<_> = candidates(seq, |m| m.start <= p).into_iter().filter(|(t, _)| *t <= p).collect();
    c.sort_by_key(|(t, of)| (Reverse(*t), of.is_none()));
    let mut valid = c.into_iter().filter(|(t, of)| is_step(seq, *t, *of)).map(|(t, _)| t);
    let current = valid.next()?;
    let mut earlier = valid.filter(|&t| t < current);
    let grace = Tick(TICKS_PER_SECOND.saturating_mul(PREVIOUS_GRACE_SECONDS));
    let target = if p.saturating_sub(current) > grace { Some(current) } else { earlier.next().or(Some(current)) };
    target.filter(|&t| t != p)
}

// ------------------------------------------------------------------------------- take log

/// What moved the prompter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum MoveKind {
    /// Where the prompt was when the take began (the first entry of every log).
    Start,
    Next,
    Previous,
    ToPlayhead,
    /// Faster, Slower or Reset Rate (#5263): the position does not change, the rate does.
    Rate,
}

/// One entry of a take's prompter log. Times are ticks, rates are percent. Between two entries the
/// prompt time runs with the playhead at `rateAfter` of the earlier one, so the prompt time at take
/// time `x` is `promptAfter + (x - takeTime) * rateAfter / 100` (rounded toward zero) of the last
/// entry before `x`, and the section and point on screen follow from it. A `rate` entry has equal
/// `promptBefore` and `promptAfter`; moves have equal `rateBefore` and `rateAfter`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PromptMove {
    pub kind: MoveKind,
    /// The playhead minus the take's record point (negative during the pre-roll).
    pub take_time: Tick,
    /// The playhead (sequence time).
    pub playhead: Tick,
    pub prompt_before: Tick,
    pub prompt_after: Tick,
    /// The rate in force just before this entry, in percent.
    pub rate_before: u32,
    /// The rate in force from this entry on, in percent.
    pub rate_after: u32,
    /// The section (range marker) shown at `promptAfter`, if any.
    pub marker: Option<MarkerId>,
    /// Its talking point current at `promptAfter` (index in the marker's points).
    pub point: Option<usize>,
    /// Its script paragraph at `promptAfter`, for a section without talking points.
    pub paragraph: Option<usize>,
}

/// The prompter log of one voice-over take: kept in the session for the take in progress and the
/// last finished one, and returned by `audio.voiceover.stop` and `prompter.takeLog`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TakeLog {
    pub sequence: ItemId,
    /// The take's record point (sequence time): where its clip starts.
    pub record_start: Tick,
    pub entries: Vec<PromptMove>,
    /// Moves not kept after [`MAX_TAKE_LOG_ENTRIES`].
    pub dropped: u64,
    /// The sequence's sections (range markers) when the take began, in marker order: the prompt
    /// times in `entries` refer to them (`sequence.conformSections` refuses once they changed).
    #[serde(default)]
    pub sections: Vec<SectionMark>,
}

/// Where a section (range marker) was when a take began.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionMark {
    pub marker: MarkerId,
    pub start: Tick,
    pub duration: Tick,
}

/// The sequence's sections as [`TakeLog::sections`] records them.
pub fn section_marks(seq: &Sequence) -> Vec<SectionMark> {
    seq.markers.iter().filter(|m| is_section(m)).map(|m| SectionMark { marker: m.id, start: m.start, duration: m.duration }).collect()
}

/// What an entry records: the playhead, the prompt time before and after, and the rate before and after.
struct Change {
    kind: MoveKind,
    playhead: Tick,
    before: Tick,
    after: Tick,
    rates: (u32, u32),
}

fn entry(seq: Option<&Sequence>, record_start: Tick, c: Change) -> PromptMove {
    let Change { kind, playhead, before, after, rates } = c;
    let section = seq.and_then(|q| prompt_at(q, after).section);
    PromptMove {
        kind,
        take_time: playhead.saturating_sub(record_start),
        playhead,
        prompt_before: before,
        prompt_after: after,
        rate_before: rates.0,
        rate_after: rates.1,
        marker: section.as_ref().map(|x| x.marker),
        point: section.as_ref().and_then(|x| x.current),
        paragraph: section.as_ref().filter(|x| x.points.is_empty()).and_then(|x| x.script.as_ref()).map(|x| x.paragraph),
    }
}

/// A voice-over take starts with its capture at `capture_start`: the prompter returns to the
/// playhead at the normal rate and a new log starts with where the prompt is then.
pub(crate) fn begin_take(s: &mut Session, record_start: Tick, capture_start: Tick) {
    s.prompter.anchor = None;
    let Some(sequence) = s.state.active_sequence else { return };
    let at = capture_start;
    let first = entry(
        s.active_sequence(),
        record_start,
        Change { kind: MoveKind::Start, playhead: at, before: at, after: at, rates: (NORMAL_RATE_PERCENT, NORMAL_RATE_PERCENT) },
    );
    let sections = s.active_sequence().map(section_marks).unwrap_or_default();
    s.prompter.take = Some(TakeLog { sequence, record_start, entries: vec![first], dropped: 0, sections });
}

/// The take ended (placed, discarded or failed): its log becomes the last take's, returned as JSON.
pub(crate) fn finish_take(s: &mut Session) -> Value {
    s.prompter.last_take_clip = None;
    match s.prompter.take.take() {
        Some(log) => {
            let v = serde_json::to_value(&log).unwrap_or(Value::Null);
            s.prompter.last_take = Some(log);
            v
        }
        None => Value::Null,
    }
}

// ------------------------------------------------------------------------------- commands

/// Log a change on the take in progress, if a voice-over is being recorded.
fn log(s: &mut Session, c: Change) {
    if !s.voiceover.recording() {
        return;
    }
    let seq = s.active_sequence();
    let e = s.prompter.take.as_ref().map(|log| entry(seq, log.record_start, c));
    if let (Some(log), Some(e)) = (s.prompter.take.as_mut(), e) {
        if log.entries.len() < MAX_TAKE_LOG_ENTRIES {
            log.entries.push(e);
        } else {
            log.dropped = log.dropped.saturating_add(1);
        }
    }
}

/// Move the prompt time to `to` (in step when that is the playhead and the rate is 100%), keeping
/// the rate, and log the move on the take in progress.
fn move_to(s: &mut Session, kind: MoveKind, to: Tick) -> Result<Value> {
    let (before, playhead, rate) = (prompt_time(s), s.playhead(), rate(s));
    set_position(s, to, playhead, rate)?;
    log(s, Change { kind, playhead, before, after: to, rates: (rate, rate) });
    Ok(status(s))
}

/// Run at `to` percent from here on: re-anchor at the current prompt time and playhead, so the
/// position does not jump, and log the change on the take in progress.
fn set_rate(s: &mut Session, to: u32) -> Result<Value> {
    let (prompt, playhead, from) = (prompt_time(s), s.playhead(), rate(s));
    set_position(s, prompt, playhead, to)?;
    log(s, Change { kind: MoveKind::Rate, playhead, before: prompt, after: prompt, rates: (from, to) });
    Ok(status(s))
}

/// The prompter's position: prompt time, playhead, the gap and its direction, and what the
/// prompt time lands on.
fn status(s: &Session) -> Value {
    let (prompt, playhead) = (prompt_time(s), s.playhead());
    let gap = prompt.saturating_sub(playhead);
    let section = s.active_sequence().and_then(|q| prompt_at(q, prompt).section);
    json!({
        "promptTime": prompt,
        "playhead": playhead,
        "gap": gap,
        "direction": direction(gap),
        "rate": rate(s),
        "marker": section.as_ref().map(|x| x.marker),
        "point": section.as_ref().and_then(|x| x.current),
        "paragraph": section.as_ref().and_then(|x| x.script.as_ref()).map(|x| x.paragraph),
    })
}

fn sections_enabled(s: &Session) -> std::result::Result<&Sequence, String> {
    has_seq(s)?;
    let seq = s.active_sequence().ok_or("no sequence is open")?;
    if !seq.markers.iter().any(is_section) {
        return Err("the sequence has no sections: range markers become prompter sections".into());
    }
    Ok(seq)
}

fn can_next(s: &Session) -> std::result::Result<(), String> {
    let seq = sections_enabled(s)?;
    next_step(seq, prompt_time(s)).map(|_| ()).ok_or_else(|| "the prompter is at the last point of the last section".into())
}

fn can_previous(s: &Session) -> std::result::Result<(), String> {
    let seq = sections_enabled(s)?;
    previous_step(seq, prompt_time(s)).map(|_| ()).ok_or_else(|| "the prompter is at the start of the first section".into())
}

fn can_return(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if gap(s) == Tick::ZERO { Err("the prompter is already at the playhead".into()) } else { Ok(()) }
}

fn can_faster(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if rate(s) >= MAX_RATE_PERCENT { Err(format!("the prompter is already at its fastest rate, {MAX_RATE_PERCENT}%")) } else { Ok(()) }
}

fn can_slower(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if rate(s) <= MIN_RATE_PERCENT { Err(format!("the prompter is already at its slowest rate, {MIN_RATE_PERCENT}%")) } else { Ok(()) }
}

fn can_reset_rate(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if rate(s) == NORMAL_RATE_PERCENT { Err(format!("the prompter is already at {NORMAL_RATE_PERCENT}%")) } else { Ok(()) }
}

fn faster(s: &mut Session, _: &Value) -> Result<Value> {
    let to = rate(s).saturating_add(RATE_STEP_PERCENT).min(MAX_RATE_PERCENT);
    set_rate(s, to)
}

fn slower(s: &mut Session, _: &Value) -> Result<Value> {
    let to = rate(s).saturating_sub(RATE_STEP_PERCENT).max(MIN_RATE_PERCENT);
    set_rate(s, to)
}

fn reset_rate(s: &mut Session, _: &Value) -> Result<Value> {
    set_rate(s, NORMAL_RATE_PERCENT)
}

fn next(s: &mut Session, _: &Value) -> Result<Value> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let to = next_step(seq, prompt_time(s)).ok_or_else(|| bad("prompter.next", "the prompter is at the last point of the last section"))?;
    move_to(s, MoveKind::Next, to)
}

fn previous(s: &mut Session, _: &Value) -> Result<Value> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let to = previous_step(seq, prompt_time(s)).ok_or_else(|| bad("prompter.previous", "the prompter is at the start of the first section"))?;
    move_to(s, MoveKind::Previous, to)
}

fn to_playhead(s: &mut Session, _: &Value) -> Result<Value> {
    let playhead = s.playhead();
    move_to(s, MoveKind::ToPlayhead, playhead)
}

/// `prompter.takeLog`: the log of the take in progress and of the last finished take.
fn take_log(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"recording": s.voiceover.recording(), "inProgress": s.prompter.take, "last": s.prompter.last_take, "lastClip": s.prompter.last_take_clip}))
}

/// The time parameters `prompter.inspect` reads.
const TIME_KEYS: [&str; 4] = ["time", "frame", "seconds", "timecode"];

/// `prompter.inspect`: the prompt for the active sequence at `time` (any time format), else at the
/// prompt time, with the prompter's position (`promptTime`, `playhead`, `gap`, `direction`, `rate`).
/// A time that is given but cannot be read is refused, not answered for the prompt time.
fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let given = TIME_KEYS.iter().any(|k| p.get(k).is_some_and(|v| !v.is_null()));
    let t = match sequence_time_p(s, p, "", "prompter.inspect")? {
        Some(t) => t,
        None if given => {
            return Err(crate::commands::bad("prompter.inspect", "the time must be ticks, a frame, seconds or a timecode such as 00:00:01:00"));
        }
        None => prompt_time(s),
    };
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut v = serde_json::to_value(prompt_at(seq, t)).map_err(|e| EngineError::Other(e.to_string()))?;
    if let (Some(o), Value::Object(pos)) = (v.as_object_mut(), status(s)) {
        for k in ["promptTime", "playhead", "gap", "direction", "rate"] {
            if let Some(x) = pos.get(k) {
                o.insert(k.into(), x.clone());
            }
        }
    }
    Ok(v)
}

pub fn commands() -> Vec<CommandSpec> {
    let spec = |id, label, menu, shortcut, params, enabled, run, journal| CommandSpec {
        id,
        label,
        menu,
        shortcut,
        params,
        enabled,
        run,
        journal,
        named: crate::commands::NamedKeys::NONE,
    };
    vec![
        spec("prompter.inspect", "Inspect Prompter", &[], None, r#"{"time":ticks?,"frame":i64?,"seconds":f64?,"timecode":str?}"#, has_seq, inspect, false),
        spec("prompter.next", "Next Point", &["Markers", "Prompter"], Some("Alt+PageDown"), "{}", can_next, next, true),
        spec("prompter.previous", "Previous Point", &["Markers", "Prompter"], Some("Alt+PageUp"), "{}", can_previous, previous, true),
        spec("prompter.toPlayhead", "Return to Playhead", &["Markers", "Prompter"], Some("Alt+Home"), "{}", can_return, to_playhead, true),
        spec("prompter.faster", "Prompt Faster", &["Markers", "Prompter"], Some("Alt+Shift+PageDown"), "{}", can_faster, faster, true),
        spec("prompter.slower", "Prompt Slower", &["Markers", "Prompter"], Some("Alt+Shift+PageUp"), "{}", can_slower, slower, true),
        spec("prompter.resetRate", "Reset Prompt Rate", &["Markers", "Prompter"], Some("Alt+Shift+Home"), "{}", can_reset_rate, reset_rate, true),
        spec("prompter.takeLog", "Prompter Take Log", &[], None, "{}", crate::commands::always, take_log, false),
    ]
}

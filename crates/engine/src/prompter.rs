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
//! **Time is a parameter.** [`prompt_at`] answers for any time; the overlay passes the playhead.
//! The function only reads the sequence: it changes nothing and is never part of a render.

use std::cmp::Reverse;

use filmcraft_project::{ClipId, Marker, MarkerId, Sequence, TrackId};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::Serialize;
use serde_json::Value;

use crate::commands::{CommandSpec, has_seq, sequence_time_p};
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
        let offset = ((chars as f64) * progress).floor().clamp(0.0, chars as f64) as usize;
        Script { text: notes.script, chars, offset }
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

/// The time parameters `prompter.inspect` reads.
const TIME_KEYS: [&str; 4] = ["time", "frame", "seconds", "timecode"];

/// `prompter.inspect`: the prompt for the active sequence at `time` (any time format), else at the
/// playhead. A time that is given but cannot be read is refused, not answered for the playhead.
fn inspect(s: &mut Session, p: &Value) -> Result<Value> {
    let given = TIME_KEYS.iter().any(|k| p.get(k).is_some_and(|v| !v.is_null()));
    let t = match sequence_time_p(s, p, "", "prompter.inspect")? {
        Some(t) => t,
        None if given => {
            return Err(crate::commands::bad("prompter.inspect", "the time must be ticks, a frame, seconds or a timecode such as 00:00:01:00"));
        }
        None => s.playhead(),
    };
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    serde_json::to_value(prompt_at(seq, t)).map_err(|e| EngineError::Other(e.to_string()))
}

pub fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: "prompter.inspect",
        label: "Inspect Prompter",
        menu: &[],
        shortcut: None,
        params: r#"{"time":ticks?,"frame":i64?,"seconds":f64?,"timecode":str?}"#,
        enabled: has_seq,
        run: inspect,
        journal: false,
        named: crate::commands::NamedKeys::NONE,
    }]
}

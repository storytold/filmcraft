//! Trim mode: selected edit points and keyboard trimming (Premiere's trim workflow).
//!
//! An edit point is one edge of a clip plus a trim kind. Trim and Ripple act on that edge; Roll acts
//! on the edge shared with the adjacent clip on the same track. Trimming delegates to the
//! `timeline.trim` / `timeline.roll` commands so linked partners and sync locks behave the same as
//! with the mouse.
//!
//! **Trim Monitor** ([`monitor_info`]): while edit points are selected the Program monitor shows the
//! outgoing clip's last frame and the incoming clip's first frame, with the Out/In shift counters
//! ([`TrimShift`]) that count how far each side moved since the edit point was selected.
//!
//! **Dynamic trimming** ([`TrimPlayback`]): in trim mode J/L "play" the edit point — it moves at the
//! shuttle speed (L forward, J backward, pressing again doubles the speed up to 8×, Shift = slow)
//! and the sequence updates live. K stops and commits: the whole dynamic trim is **one** undo step.
//! Space loops playback around the edit (Preferences ▸ Playback preroll/postroll). Everything is
//! driven by a caller-supplied clock (`clock`, seconds): the UI passes its frame time, tests pass
//! exact values, so dynamic trims are deterministic.

use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use filmcraft_project::{ClipId, Project, Sequence, TrackId, TrackKind};
use filmcraft_time::{Tick, TimeRange};

use crate::{EngineError, Result, Session};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum TrimKind {
    /// Regular trim: the edge moves, leaving a gap or covering a neighbour's gap.
    Trim,
    /// Ripple: the edge moves and later material shifts to close/open the difference.
    Ripple,
    /// Roll: the shared edge between two adjacent clips moves; duration is unchanged.
    Roll,
}

impl TrimKind {
    fn next(self) -> Self {
        match self {
            TrimKind::Ripple => TrimKind::Roll,
            TrimKind::Roll => TrimKind::Trim,
            TrimKind::Trim => TrimKind::Ripple,
        }
    }
}

/// Trim Monitor counters: how far the outgoing clip's Out point and the incoming clip's In point
/// moved since the edit point was selected.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TrimShift {
    pub out_shift: Tick,
    pub in_shift: Tick,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EditPoint {
    pub clip: ClipId,
    /// The clip's Out edge (else its In edge).
    pub out: bool,
    pub kind: TrimKind,
}

/// Time of an edit point in the sequence.
pub fn edit_time(seq: &Sequence, ep: &EditPoint) -> Option<Tick> {
    let (_, it) = seq.find_item(ep.clip)?;
    Some(if ep.out { it.end() } else { it.start })
}

/// The (left, right) clips of a roll at this edit point.
pub fn roll_pair(seq: &Sequence, ep: &EditPoint) -> Option<(ClipId, ClipId)> {
    let (tid, it) = seq.find_item(ep.clip)?;
    let track = seq.track(tid)?;
    if ep.out {
        let right = track.items.iter().find(|o| o.start == it.end())?;
        Some((it.id, right.id))
    } else {
        let left = track.items.iter().find(|o| o.end() == it.start)?;
        Some((left.id, it.id))
    }
}

fn kind_p(p: &Value) -> Option<TrimKind> {
    serde_json::from_value(p.get("kind")?.clone()).ok()
}

/// `trim.selectEditPoint {clip, edge: "in"|"out", kind, add?}`
pub fn select(s: &mut Session, p: &Value) -> Result<Value> {
    let clip = p.get("clip").and_then(Value::as_u64).map(ClipId).ok_or_else(|| EngineError::Other("need `clip`".into()))?;
    let out = p.get("edge").and_then(Value::as_str) != Some("in");
    let kind = kind_p(p).unwrap_or(TrimKind::Trim);
    s.active_sequence().and_then(|q| q.find_item(clip)).ok_or_else(|| EngineError::Other(format!("no clip {}", clip.0)))?;
    let ep = EditPoint { clip, out, kind };
    if p.get("add").and_then(Value::as_bool).unwrap_or(false) {
        if let Some(i) = s.state.edit_points.iter().position(|e| e.clip == clip && e.out == out) {
            s.state.edit_points.remove(i);
        } else {
            s.state.edit_points.push(ep);
        }
    } else {
        s.state.edit_points = vec![ep];
    }
    s.state.selection.clear();
    reset_shift(s);
    Ok(json!({"editPoints": s.state.edit_points}))
}

/// `trim.selectNearest {kind: "rippleIn"|"rippleOut"|"roll"|"trimIn"|"trimOut"}`: the edit point
/// nearest the playhead on each targeted track (all tracks when none are targeted).
pub fn select_nearest(s: &mut Session, p: &Value) -> Result<Value> {
    let which = p.get("kind").and_then(Value::as_str).unwrap_or("roll");
    let (kind, want_out) = match which {
        "rippleIn" => (TrimKind::Ripple, Some(false)),
        "rippleOut" => (TrimKind::Ripple, Some(true)),
        "trimIn" => (TrimKind::Trim, Some(false)),
        "trimOut" => (TrimKind::Trim, Some(true)),
        _ => (TrimKind::Roll, None),
    };
    let ph = s.playhead();
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let targeted = s.state.active_sequence.and_then(|id| s.state.targeting.get(&id)).map(|t| t.targeted.clone()).unwrap_or_default();
    let mut points = Vec::new();
    for tr in seq.video_tracks.iter().chain(seq.audio_tracks.iter()) {
        if !targeted.is_empty() && !targeted.contains(&tr.id) {
            continue;
        }
        // nearest edge on this track
        let mut best: Option<(i64, EditPoint)> = None;
        for it in &tr.items {
            for out in [false, true] {
                if want_out.is_some_and(|w| w != out) {
                    continue;
                }
                let t = if out { it.end() } else { it.start };
                let ep = EditPoint { clip: it.id, out, kind };
                if kind == TrimKind::Roll && roll_pair(seq, &ep).is_none() {
                    continue;
                }
                let d = (t - ph).0.abs();
                if best.is_none_or(|(bd, _)| d < bd) {
                    best = Some((d, ep));
                }
            }
        }
        if let Some((_, ep)) = best {
            // one roll point per edge: skip the partner's mirror of an edge already chosen
            if kind == TrimKind::Roll && points.iter().any(|e: &EditPoint| roll_pair(seq, e) == roll_pair(seq, &ep)) {
                continue;
            }
            points.push(ep);
        }
    }
    s.state.edit_points = points;
    s.state.selection.clear();
    reset_shift(s);
    Ok(json!({"editPoints": s.state.edit_points}))
}

/// `trim.toggleType`: cycle Ripple → Roll → Trim for every selected edit point.
pub fn toggle_type(s: &mut Session) -> Result<Value> {
    for e in s.state.edit_points.iter_mut() {
        e.kind = e.kind.next();
    }
    Ok(json!({"editPoints": s.state.edit_points}))
}

/// Trim every selected edit point by `delta` (edit point movement; positive = later).
fn trim_all(s: &mut Session, delta: Tick) -> Result<Value> {
    let applied = trim_all_raw(s, &|_, _| Some(delta))?;
    let first = applied.first().and_then(Value::as_i64).map(Tick).unwrap_or_default();
    add_shift(s, first);
    Ok(json!({"applied": applied}))
}

/// Add an applied movement of the primary edit point to the Trim Monitor's shift counters.
fn add_shift(s: &mut Session, d: Tick) {
    let Some(ep) = s.state.edit_points.first() else { return };
    match (ep.kind, ep.out) {
        (TrimKind::Roll, _) => {
            s.state.trim_shift.out_shift += d;
            s.state.trim_shift.in_shift += d;
        }
        (_, true) => s.state.trim_shift.out_shift += d,
        (_, false) => s.state.trim_shift.in_shift += d,
    }
}

/// Reset the Trim Monitor counters (a new edit point selection starts a new trim session).
fn reset_shift(s: &mut Session) {
    s.state.trim_shift = TrimShift::default();
}

/// Trim every selected edit point; `delta_for` gives each point's own movement, read from the sequence as it is when that point is reached.
fn trim_all_raw(s: &mut Session, delta_for: &dyn Fn(&Sequence, &EditPoint) -> Option<Tick>) -> Result<Vec<Value>> {
    if s.state.edit_points.is_empty() {
        return Err(EngineError::Other("no edit points selected".into()));
    }
    let pts = s.state.edit_points.clone();
    let mut applied = Vec::new();
    for ep in &pts {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let Some(delta) = delta_for(seq, ep) else { continue };
        let r = match ep.kind {
            TrimKind::Roll => {
                let Some((l, r)) = roll_pair(seq, ep) else { continue };
                s.execute("timeline.roll", json!({"left": l.0, "right": r.0, "delta": delta.0}))?
            }
            k => s.execute(
                "timeline.trim",
                json!({"clip": ep.clip.0, "edge": if ep.out { "out" } else { "in" }, "mode": if k == TrimKind::Ripple { "ripple" } else { "regular" }, "delta": delta.0}),
            )?,
        };
        applied.push(r.get("delta").cloned().unwrap_or(Value::Null));
    }
    Ok(applied)
}

/// `trim.nudge {frames}`: Trim Forward/Backward (±1), Many (±5 by default).
pub fn nudge(s: &mut Session, p: &Value) -> Result<Value> {
    let frames = p.get("frames").and_then(Value::as_i64).unwrap_or(1);
    let d = s.sequence_rate().tick_of(frames);
    trim_all(s, d)
}

/// `trim.extendToPlayhead`: move each selected edit point to the playhead.
pub fn extend_to_playhead(s: &mut Session) -> Result<Value> {
    let ph = s.playhead();
    let applied = trim_all_raw(s, &|seq, e| edit_time(seq, e).map(|t| ph - t))?;
    let first = applied.first().and_then(Value::as_i64).map(Tick).unwrap_or_default();
    add_shift(s, first);
    Ok(json!({"applied": applied}))
}

/// `trim.toPlayhead {side: "previous"|"next", ripple}` (Q/W, ⌥Q/⌥W): remove the material between
/// the playhead and the previous (Q) or next (W) edit point on the targeted tracks. Ripple variants
/// extract the range (later material moves left, sync locks respected); the others lift it.
pub fn to_playhead(s: &mut Session, p: &Value) -> Result<Value> {
    let next = p.get("side").and_then(Value::as_str) == Some("next");
    let ripple = p.get("ripple").and_then(Value::as_bool).unwrap_or(true);
    let ph = s.playhead();
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let under = |t: &filmcraft_project::Track, keep: &dyn Fn(ClipId) -> bool| t.items.iter().any(|i| i.start < ph && ph < i.end() && keep(i.id));
    // Like Add Edit: selected clips under the playhead (and their linked partners) are trimmed,
    // and only their tracks (#164); with none, the targeted tracks are.
    let sel = crate::commands::with_links(s, &s.state.selection);
    let selected: Vec<TrackId> =
        seq.video_tracks.iter().chain(seq.audio_tracks.iter()).filter(|t| !t.locked && under(t, &|c| sel.contains(&c))).map(|t| t.id).collect();
    let tracks = if selected.is_empty() {
        let targeted = s.targeting().targeted;
        seq.video_tracks.iter().chain(seq.audio_tracks.iter()).filter(|t| targeted.contains(&t.id) && !t.locked && under(t, &|_| true)).map(|t| t.id).collect()
    } else {
        selected
    };
    if tracks.is_empty() {
        return Err(EngineError::Other("no clip under the playhead on the targeted tracks".into()));
    }
    // nearest edit point on those tracks
    let edits =
        seq.video_tracks.iter().chain(seq.audio_tracks.iter()).filter(|t| tracks.contains(&t.id)).flat_map(|t| t.items.iter().flat_map(|i| [i.start, i.end()]));
    let edge = if next { edits.filter(|e| *e > ph).min() } else { edits.filter(|e| *e < ph).max() };
    let edge = edge.ok_or_else(|| EngineError::Other("no edit point".into()))?;
    let range = if next { TimeRange::from_bounds(ph, edge) } else { TimeRange::from_bounds(edge, ph) };
    let label = match (next, ripple) {
        (true, true) => "Ripple Trim Next Edit to Playhead",
        (false, true) => "Ripple Trim Previous Edit to Playhead",
        (true, false) => "Trim Next Edit to Playhead",
        (false, false) => "Trim Previous Edit to Playhead",
    };
    s.edit_sequence(label, |q, ctx, _| {
        if ripple {
            filmcraft_edit::extract(q, &tracks, range, ctx);
        } else {
            filmcraft_edit::lift(q, &tracks, range, ctx);
        }
        Ok(())
    })?;
    if !next && ripple {
        // the head before the playhead was removed; park on the new edit (as Premiere does)
        s.set_playhead(edge);
    }
    Ok(json!({"range": [range.start.0, range.end().0]}))
}

// ------------------------------------------------------------------ Trim Monitor

/// One side of the Trim Monitor: the clip and the frame shown for it.
fn side_json(s: &Session, seq: &Sequence, clip: ClipId, outgoing: bool) -> Option<Value> {
    let (tid, it) = seq.find_item(clip)?;
    let rate = seq.settings.frame_rate;
    let fd = rate.frame_duration();
    let t = if outgoing { it.end() - fd } else { it.start };
    let media_time = it.source_time_at(t.clamp(it.start, it.end() - Tick(1)));
    let track = seq.track(tid)?;
    let tracks = seq.tracks(track.kind);
    let idx = tracks.iter().position(|x| x.id == tid).unwrap_or(0) + 1;
    let track_name = format!("{}{idx}", if track.kind == TrackKind::Video { "V" } else { "A" });
    let item_rate = s.project.item(it.item).map(|i| i.frame_rate()).unwrap_or(rate);
    Some(json!({
        "clip": it.id.0, "item": it.item.0, "name": it.name, "track": track_name, "video": track.kind == TrackKind::Video,
        "time": t.0, "mediaTime": media_time.0, "mediaFrame": item_rate.frame_at(media_time),
        "sourceIn": it.source_in.0, "sourceOut": it.source_out().0,
    }))
}

/// The (outgoing, incoming) clips at an edit point.
pub fn sides(seq: &Sequence, ep: &EditPoint) -> (Option<ClipId>, Option<ClipId>) {
    match roll_pair(seq, ep) {
        Some((l, r)) => (Some(l), Some(r)),
        None if ep.out => (Some(ep.clip), None),
        None => (None, Some(ep.clip)),
    }
}

/// `trim.monitor`: everything the Trim Monitor shows, for the primary (first) edit point.
pub fn monitor_info(s: &Session) -> Value {
    let Some(seq) = s.active_sequence() else { return Value::Null };
    let Some(ep) = s.state.edit_points.first() else { return json!({"active": false}) };
    let rate = seq.settings.frame_rate;
    let (out_c, in_c) = sides(seq, ep);
    let tp = &s.trim_play;
    json!({
        "active": true,
        "kind": ep.kind,
        "edge": if ep.out { "out" } else { "in" },
        "editTime": edit_time(seq, ep).map(|t| t.0),
        "editPoints": s.state.edit_points.len(),
        "outgoing": out_c.and_then(|c| side_json(s, seq, c, true)),
        "incoming": in_c.and_then(|c| side_json(s, seq, c, false)),
        "outShift": rate.frame_at(s.state.trim_shift.out_shift),
        "inShift": rate.frame_at(s.state.trim_shift.in_shift),
        "largeTrimOffset": s.prefs.trim.large_trim_offset,
        "dynamic": tp.dynamic.as_ref().map(|d| json!({"offsetFrames": rate.frame_at(d.offset), "offset": d.offset.0, "speed": d.speed, "atLimit": d.at_limit})),
        "playAround": tp.around.as_ref().map(|a| json!({"start": a.start.0, "end": a.end.0, "loop": a.looping})),
    })
}

/// `trim.applyDefaultTransition` (Apply Default Transitions to Selection, Trim Monitor button): the
/// default video/audio transition at every selected edit point.
pub fn apply_default_transitions(s: &mut Session) -> Result<Value> {
    let pts = s.state.edit_points.clone();
    let mut ids = Vec::new();
    for ep in &pts {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let Some((tid, _)) = seq.find_item(ep.clip) else { continue };
        let video = seq.track(tid).is_some_and(|t| t.kind == TrackKind::Video);
        let cmd = if video { "sequence.applyVideoTransition" } else { "sequence.applyAudioTransition" };
        if let Ok(r) = s.execute(cmd, json!({"clip": ep.clip.0, "edge": if ep.out { "out" } else { "in" }})) {
            ids.push(r["transition"].clone());
        }
    }
    if ids.is_empty() {
        return Err(EngineError::Other("no transition could be applied at the selected edit points".into()));
    }
    Ok(json!({"transitions": ids}))
}

// ------------------------------------------------------------------ dynamic trimming

/// Trim-mode playback state (not project data; never saved).
#[derive(Clone, Debug, Default)]
pub struct TrimPlayback {
    pub dynamic: Option<DynamicTrim>,
    pub around: Option<PlayAround>,
}

impl TrimPlayback {
    pub fn active(&self) -> bool {
        self.dynamic.is_some() || self.around.is_some()
    }
}

/// A J/K/L trim in progress.
#[derive(Clone, Debug)]
pub struct DynamicTrim {
    /// The project before the trim: the one undo step pushed on commit, and the base every live
    /// update re-applies the offset to (so updates never accumulate rounding or clamping).
    before: Arc<Project>,
    shift_before: TrimShift,
    /// Edit point movement applied so far (whole frames).
    pub offset: Tick,
    /// Shuttle speed (× real time); negative = backward, 0 = held (stopped at a limit).
    pub speed: f64,
    anchor_clock: f64,
    anchor_offset: Tick,
    /// The trim ran into a limit (media handle, neighbour, sync) and stopped there.
    pub at_limit: bool,
}

/// Loop (or one-shot) playback around the edit: preroll before it to postroll after it.
#[derive(Clone, Debug)]
pub struct PlayAround {
    pub start: Tick,
    pub end: Tick,
    pub looping: bool,
    clock0: f64,
}

fn clock_p(p: &Value) -> f64 {
    p.get("clock").and_then(Value::as_f64).unwrap_or(0.0)
}

fn direction_p(p: &Value) -> f64 {
    match p.get("direction") {
        Some(Value::String(d)) if d.starts_with('r') || d.starts_with('b') || d.eq_ignore_ascii_case("j") => -1.0,
        Some(v) if v.as_f64().is_some_and(|x| x < 0.0) => -1.0,
        _ => 1.0,
    }
}

/// Whole frames of movement for `secs` of edit point travel (truncated toward zero).
fn frames_for(s: &Session, secs: f64) -> Tick {
    let rate = s.sequence_rate();
    let mag = rate.frame_at(Tick::from_seconds_f64(secs.abs()));
    rate.tick_of(if secs < 0.0 { -mag } else { mag })
}

/// `trim.shuttle {direction: "forward"|"reverse", slow?, clock}` (L / J in trim mode): start a
/// dynamic trim, speed it up (pressing the same direction again doubles the speed, up to 8×) or
/// reverse it.
pub fn shuttle(s: &mut Session, p: &Value) -> Result<Value> {
    let clock = clock_p(p);
    let dir = direction_p(p);
    let slow = p.get("slow").and_then(Value::as_bool).unwrap_or(false);
    s.trim_play.around = None;
    if s.trim_play.dynamic.is_none() {
        s.trim_play.dynamic = Some(DynamicTrim {
            before: s.project.clone(),
            shift_before: s.state.trim_shift,
            offset: Tick::ZERO,
            speed: 0.0,
            anchor_clock: clock,
            anchor_offset: Tick::ZERO,
            at_limit: false,
        });
    } else {
        // bring the offset up to date before changing speed
        tick(s, clock)?;
    }
    let rate = s.sequence_rate();
    let d = s.trim_play.dynamic.as_mut().ok_or_else(|| EngineError::Other("dynamic trimming is not active".into()))?;
    let speed = if slow {
        0.25 * dir
    } else if d.speed * dir > 0.0 && !d.at_limit {
        (d.speed * 2.0).clamp(-8.0, 8.0)
    } else {
        dir
    };
    d.speed = speed;
    d.anchor_clock = clock;
    d.anchor_offset = d.offset;
    d.at_limit = false;
    Ok(json!({"speed": speed, "offsetFrames": rate.frame_at(d.offset)}))
}

/// `trim.tick {clock}`: advance dynamic trimming / loop playback to `clock` (call once per frame).
pub fn tick(s: &mut Session, clock: f64) -> Result<Value> {
    if let Some(d) = s.trim_play.dynamic.clone() {
        let want = d.anchor_offset + frames_for(s, (clock - d.anchor_clock).max(0.0) * d.speed);
        if want != d.offset {
            let got = apply_dynamic(s, want)?;
            if let Some(dd) = s.trim_play.dynamic.as_mut() {
                dd.offset = got;
                if got != want {
                    // ran into a limit: hold there (K still commits)
                    dd.at_limit = true;
                    dd.speed = 0.0;
                    dd.anchor_offset = got;
                    dd.anchor_clock = clock;
                }
            }
        }
    } else if let Some(a) = s.trim_play.around.clone() {
        let len = (a.end - a.start).seconds().max(1e-9);
        let mut el = (clock - a.clock0).max(0.0);
        if a.looping {
            el %= len;
        } else if el >= len {
            s.trim_play.around = None;
            s.set_playhead(a.end - s.sequence_rate().frame_duration());
            return Ok(status(s));
        }
        s.set_playhead(a.start + Tick::from_seconds_f64(el));
    }
    Ok(status(s))
}

fn status(s: &Session) -> Value {
    let rate = s.sequence_rate();
    let tp = &s.trim_play;
    json!({
        "playing": tp.active(),
        "dynamic": tp.dynamic.as_ref().map(|d| json!({"offsetFrames": rate.frame_at(d.offset), "speed": d.speed, "atLimit": d.at_limit})),
        "playhead": s.playhead().0,
    })
}

/// Re-apply `offset` to the project as it was before the dynamic trim, without touching the undo
/// history or the journal. Returns the offset that could actually be applied.
fn apply_dynamic(s: &mut Session, offset: Tick) -> Result<Tick> {
    let Some(d) = s.trim_play.dynamic.clone() else { return Ok(Tick::ZERO) };
    let (undo_len, journal_len, limit) = (s.history.undo.len(), s.journal.len(), s.history.limit);
    let redo = std::mem::take(&mut s.history.redo);
    s.history.limit = usize::MAX;
    s.project = d.before.clone();
    let r = if offset == Tick::ZERO { Ok(vec![json!(0)]) } else { trim_all_raw(s, &|_, _| Some(offset)) };
    let applied = match r {
        Ok(a) => a.first().and_then(Value::as_i64).map(Tick).unwrap_or_default(),
        Err(_) => {
            // refused outright (e.g. it would break sync): stay where we were
            s.project = d.before.clone();
            if d.offset != Tick::ZERO {
                let _ = trim_all_raw(s, &|_, _| Some(d.offset));
            }
            d.offset
        }
    };
    s.history.undo.truncate(undo_len);
    s.history.limit = limit;
    s.history.redo = redo;
    s.journal.truncate(journal_len);
    s.state.trim_shift = d.shift_before;
    add_shift(s, applied);
    // the Program monitor follows the moving edit
    if let Some(t) = s.active_sequence().and_then(|q| s.state.edit_points.first().and_then(|e| edit_time(q, e))) {
        s.set_playhead(t);
    }
    Ok(applied)
}

/// `trim.shuttleStop {clock?}` (K in trim mode): stop dynamic trimming and commit it as one undo
/// step, and stop loop playback.
pub fn stop(s: &mut Session, p: &Value) -> Result<Value> {
    if let Some(c) = p.get("clock").and_then(Value::as_f64)
        && s.trim_play.dynamic.is_some()
    {
        tick(s, c)?;
    }
    s.trim_play.around = None;
    Ok(commit(s))
}

/// Commit a dynamic trim in progress (one undo step labelled after the trim kind).
pub fn commit(s: &mut Session) -> Value {
    let Some(d) = s.trim_play.dynamic.take() else { return json!({"committed": false}) };
    let rate = s.sequence_rate();
    if d.offset == Tick::ZERO || Arc::ptr_eq(&d.before, &s.project) {
        s.project = d.before;
        s.state.trim_shift = d.shift_before;
        return json!({"committed": false, "offsetFrames": 0});
    }
    let label = match s.state.edit_points.first().map(|e| e.kind) {
        Some(TrimKind::Roll) => "Dynamic Rolling Edit",
        Some(TrimKind::Ripple) => "Dynamic Ripple Trim",
        _ => "Dynamic Trim",
    };
    s.history.undo.push((label.to_string(), d.before));
    if s.history.undo.len() > s.history.limit {
        s.history.undo.remove(0);
    }
    s.history.redo.clear();
    json!({"committed": true, "label": label, "offsetFrames": rate.frame_at(d.offset), "offset": d.offset.0})
}

/// `trim.cancelDynamic` (Esc): drop a dynamic trim in progress, restoring the edit.
pub fn cancel(s: &mut Session) -> Result<Value> {
    s.trim_play.around = None;
    if let Some(d) = s.trim_play.dynamic.take() {
        s.project = d.before;
        s.state.trim_shift = d.shift_before;
        s.fix_state();
        if let Some(t) = s.active_sequence().and_then(|q| s.state.edit_points.first().and_then(|e| edit_time(q, e))) {
            s.set_playhead(t);
        }
        s.revision += 1;
        s.events.push(crate::Event::ProjectChanged { revision: s.revision });
    }
    Ok(Value::Null)
}

/// `trim.playAround {clock, loop?: true, toggle?}` (Space / Shift+K in trim mode): play from
/// preroll before the edit (or the playhead, per Preferences ▸ Trim) to postroll after it.
pub fn play_around(s: &mut Session, p: &Value) -> Result<Value> {
    if s.trim_play.dynamic.is_some() {
        commit(s);
    }
    if s.trim_play.around.is_some() && p.get("toggle").and_then(Value::as_bool).unwrap_or(false) {
        s.trim_play.around = None;
        return Ok(status(s));
    }
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let edit = s.state.edit_points.first().and_then(|e| edit_time(seq, e));
    let center = match edit {
        Some(e) if !s.prefs.trim.playhead_determines_loop => e,
        _ => s.playhead(),
    };
    let rate = seq.settings.frame_rate;
    let dur = seq.duration();
    let start = rate.snap((center - Tick::from_seconds_f64(s.prefs.playback.preroll_seconds)).max(Tick::ZERO));
    let end = rate.snap((center + Tick::from_seconds_f64(s.prefs.playback.postroll_seconds)).min(dur.max(center + rate.frame_duration())));
    let end = end.max(start + rate.frame_duration());
    let looping = p.get("loop").and_then(Value::as_bool).unwrap_or(true);
    s.trim_play.around = Some(PlayAround { start, end, looping, clock0: clock_p(p) });
    s.set_playhead(start);
    Ok(json!({"start": start.0, "end": end.0, "loop": looping}))
}

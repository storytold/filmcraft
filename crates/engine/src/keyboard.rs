//! Keyboard-only editing commands from Premiere's default keyboard (M3.12): commands that have a
//! default shortcut in Premiere but no menu item, so they exist only to be pressed.
//!
//! | Id | Premiere command | Default key (scope) |
//! |---|---|---|
//! | `playhead.nextEditAnyTrack` / `playhead.prevEditAnyTrack` | Go to Next / Previous Edit Point on Any Track | Shift+Down / Shift+Up |
//! | `playhead.selectedClipStart` / `playhead.selectedClipEnd` | Go to Selected Clip Start / End | Shift+Home / Shift+End |
//! | `sequence.revealNested` | Reveal Nested Sequence | Cmd+Alt+F |
//! | `timeline.selectClipAtPlayhead` | Select Clip at Playhead | D |
//! | `timeline.selectNextClip` / `timeline.selectPrevClip` | Select Next / Previous Clip | Cmd+Down / Cmd+Up |
//! | `trim.extendPreviousEdit` / `trim.extendNextEdit` | Extend Previous / Next Edit To Playhead | Shift+Q / Shift+W |
//! | `timeline.nudgeLeft` … `timeline.nudgeRight5` | Nudge Clip Selection Left / Right One / Five Frames | Cmd+Left, Cmd+Right, Cmd+Shift+Left, Cmd+Shift+Right (Timeline) |
//! | `timeline.nudgeUp` / `timeline.nudgeDown` | Nudge Clip Selection Up / Down | Alt+Up / Alt+Down (Timeline) |
//! | `timeline.slipLeft` … `timeline.slipRight5` | Slip Clip Selection Left / Right One / Five Frames | Cmd+Alt+Left … Cmd+Alt+Shift+Right (Timeline) |
//! | `timeline.slideLeft` … `timeline.slideRight5` | Slide Clip Selection Left / Right One / Five Frames | Alt+, … Alt+Shift+. (Timeline) |
//! | `timeline.toggleAllVideoTargets` / `timeline.toggleAllAudioTargets` | Toggle All Video / Audio Targets | Cmd+0 / Cmd+9 |
//! | `timeline.toggleAllSourceVideo` / `timeline.toggleAllSourceAudio` | Toggle All Source Video / Audio | Cmd+Alt+0 / Cmd+Alt+9 |
//! | `timeline.toggleTargetV1` … `timeline.toggleTargetA8` | Toggle Target Video 1–8 / Audio 1–8 | |
//! | `timeline.moveVideoTargetsUp` … `timeline.moveAudioTargetsDown` | Move All Video / Audio Targets Up / Down | |
//! | `timeline.toggleMuteTargetedAudio` / `timeline.toggleSoloTargetedAudio` | Toggle Mute / Solo for All Targeted Audio Tracks | |
//! | `timeline.toggleOutputTargetedVideo` | Toggle Track Output for All Targeted Video Tracks | |
//! | `clip.volumeUp` / `clip.volumeDown` | Increase / Decrease Clip Volume (1 dB) | ] / [ |
//! | `clip.volumeUpMany` / `clip.volumeDownMany` | Increase / Decrease Clip Volume Many (Settings ▸ Audio ▸ Large Volume Adjustment) | Shift+] / Shift+[ |
//! | `clip.nudgeVolumeUp1` … `clip.nudgeVolumeDown3` | Nudge Volume ±1 dB / ±3 dB | |
//! | `audio.toggleScrubbing` | Toggle Audio During Scrubbing | Shift+S |
//! | `graphics.fontSizeUp` … `graphics.fontSizeDown5` | Increase / Decrease Font Size by One / Five Units | Cmd+Alt+Right … Cmd+Alt+Shift+Left |
//! | `graphics.leadingUp` … `graphics.leadingDown5` | Increase / Decrease Leading by One / Five Units | Alt+Up … Alt+Shift+Down |
//! | `graphics.alignTextLeft` / `…Center` / `…Right` | Left / Center / Right align text | Cmd+Shift+L / C / R |
//! | `graphics.nudgeLeft` … `graphics.nudgeDown5` | Nudge Selected Object by one / five | Cmd+arrows, Cmd+Shift+arrows (Program, Properties) |
//! | `file.exportFrame` | Export Frame | Shift+E |
//! | `clip.setPosterFrame` / `clip.clearPosterFrame` | Set / Clear Poster Frame | Cmd+P / Alt+P |
//!
//! Navigation and targeting are editor state (not undoable, as in Premiere); everything that
//! changes the project is one undo step.

use filmcraft_edit as edit;
pub use filmcraft_export::still::StillFormat;
use filmcraft_geom::Vec2;
use filmcraft_project::{ClipId, ItemKind, ParamValue, Sequence, TrackId, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, has_selection, has_seq, item_p, time_p, with_links};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, shortcut: Option<&'static str>, params: &'static str, enabled: Enabled, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut, params, enabled, run, journal: true }
}

/// Metadata key holding an item's poster frame (media ticks).
pub const POSTER_FRAME_KEY: &str = "Poster Frame";

/// An item's poster frame (Set Poster Frame), if one is set.
pub fn poster_frame(item: &filmcraft_project::ProjectItem) -> Option<Tick> {
    item.metadata.get(POSTER_FRAME_KEY).and_then(|v| v.parse::<i64>().ok()).map(Tick)
}

// ------------------------------------------------------------------ navigation

/// Edit points on the given tracks (all tracks when `tracks` is None), sorted and unique.
fn edits_on(seq: &Sequence, tracks: Option<&[TrackId]>) -> Vec<Tick> {
    let mut v: Vec<Tick> =
        seq.all_tracks().filter(|t| tracks.is_none_or(|ts| ts.contains(&t.id))).flat_map(|t| t.items.iter().flat_map(|i| [i.start, i.end()])).collect();
    v.sort_unstable();
    v.dedup();
    v
}

/// Go to the next / previous edit point: on the targeted tracks (`any` = false; all tracks when
/// none is targeted) or on any track.
pub fn go_to_edit(s: &mut Session, next: bool, any: bool) -> Result<Value> {
    let t = s.playhead();
    let tg = s.targeting().targeted;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let only = (!any && !tg.is_empty()).then_some(tg.as_slice());
    let edits = edits_on(seq, only);
    let e = if next { edits.into_iter().find(|e| *e > t) } else { edits.into_iter().rev().find(|e| *e < t) };
    if let Some(e) = e {
        s.set_playhead(e);
    }
    Ok(json!({"time": s.playhead().0}))
}

fn selected_span(s: &Session) -> Option<(Tick, Tick)> {
    let seq = s.active_sequence()?;
    let items: Vec<_> = s.state.selection.iter().filter_map(|c| seq.find_item(*c).map(|(_, i)| i)).collect();
    let a = items.iter().map(|i| i.start).min()?;
    let b = items.iter().map(|i| i.end()).max()?;
    Some((a, b))
}

fn go_to_selected(s: &mut Session, end: bool) -> Result<Value> {
    let (a, b) = selected_span(s).ok_or_else(|| EngineError::Other("no clips selected".into()))?;
    // Premiere parks on the clip's last frame for End (the frame before the Out edge)
    let t = if end { b - s.sequence_rate().frame_duration() } else { a };
    s.set_playhead(t.max(a));
    Ok(json!({"time": s.playhead().0}))
}

/// Reveal Nested Sequence: open the nested sequence of the selected clip (or the topmost nested
/// clip under the playhead on a targeted track) with its playhead on the matching frame.
fn reveal_nested(s: &mut Session) -> Result<Value> {
    let ph = s.playhead();
    let tg = s.targeting().targeted;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let is_nest = |it: &filmcraft_project::TrackItem| s.project.sequence(it.item).is_some();
    let pick = s
        .state
        .selection
        .iter()
        .filter_map(|c| seq.find_item(*c).map(|(_, i)| i))
        .find(|i| is_nest(i))
        .or_else(|| {
            seq.video_tracks.iter().rev().chain(seq.audio_tracks.iter()).filter(|t| tg.contains(&t.id)).filter_map(|t| t.item_at(ph)).find(|i| is_nest(i))
        })
        .ok_or_else(|| EngineError::Other("select a nested sequence clip".into()))?;
    let (nested, t) = (pick.item, pick.source_time_at(ph.clamp(pick.start, pick.end() - Tick(1))));
    s.execute("sequence.open", json!({"item": nested.0}))?;
    s.set_playhead(t);
    Ok(json!({"sequence": nested.0, "time": s.playhead().0}))
}

// ------------------------------------------------------------------ selection

fn select_clip_at_playhead(s: &mut Session) -> Result<Value> {
    let t = s.playhead();
    let tg = s.targeting().targeted;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let ids: Vec<ClipId> = seq.all_tracks().filter(|tr| tg.contains(&tr.id)).filter_map(|tr| tr.item_at(t).map(|i| i.id)).collect();
    s.state.selection = with_links(s, &ids);
    s.state.edit_points.clear();
    s.state.caption_selection.clear();
    Ok(json!({"selection": s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>()}))
}

/// Select Next / Previous Clip: the clip after (before) the selection on each selected clip's
/// track; with nothing selected, the clips at the playhead on the targeted tracks.
fn select_step(s: &mut Session, next: bool) -> Result<Value> {
    if s.state.selection.is_empty() {
        return select_clip_at_playhead(s);
    }
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    // one anchor per track: the last (first) selected item
    let mut picks: Vec<ClipId> = Vec::new();
    for tr in seq.all_tracks() {
        let sel: Vec<usize> = tr.items.iter().enumerate().filter(|(_, i)| s.state.selection.contains(&i.id)).map(|(k, _)| k).collect();
        let Some(&anchor) = (if next { sel.last() } else { sel.first() }) else { continue };
        let k = if next { Some(anchor + 1) } else { anchor.checked_sub(1) };
        if let Some(it) = k.and_then(|k| tr.items.get(k)) {
            picks.push(it.id);
        }
    }
    // linked partners already follow their own track's step; keep the old selection when at the end
    if picks.is_empty() {
        return Ok(json!({"selection": s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>()}));
    }
    s.state.selection = with_links(s, &picks);
    Ok(json!({"selection": s.state.selection.iter().map(|c| c.0).collect::<Vec<_>>()}))
}

// ------------------------------------------------------------------ trimming

/// Extend Previous / Next Edit To Playhead: on each targeted, unlocked track, move the nearest edit
/// point before (after) the playhead to the playhead. A cut between two clips rolls; an edge
/// next to a gap is trimmed (no ripple).
fn extend_edit(s: &mut Session, next: bool) -> Result<Value> {
    let ph = s.playhead();
    let tg = s.targeting().targeted;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut ops: Vec<(Option<ClipId>, Option<ClipId>, Tick)> = Vec::new();
    for tr in seq.all_tracks().filter(|t| tg.contains(&t.id) && !t.locked) {
        let edges = tr.items.iter().flat_map(|i| [i.start, i.end()]);
        let e = if next { edges.filter(|e| *e > ph).min() } else { edges.filter(|e| *e < ph).max() };
        let Some(e) = e else { continue };
        let left = tr.items.iter().find(|i| i.end() == e).map(|i| i.id);
        let right = tr.items.iter().find(|i| i.start == e).map(|i| i.id);
        ops.push((left, right, ph - e));
    }
    if ops.is_empty() {
        return Err(EngineError::Other("no edit point on the targeted tracks".into()));
    }
    let label = if next { "Extend Next Edit To Playhead" } else { "Extend Previous Edit To Playhead" };
    s.edit_sequence(label, |q, ctx, _| {
        for (l, r, d) in &ops {
            match (l, r) {
                (Some(l), Some(r)) => {
                    edit::roll(q, *l, *r, *d, ctx)?;
                }
                (Some(l), None) => {
                    edit::trim(q, *l, edit::Edge::Out, edit::TrimMode::Regular, *d, ctx)?;
                }
                (None, Some(r)) => {
                    edit::trim(q, *r, edit::Edge::In, edit::TrimMode::Regular, *d, ctx)?;
                }
                (None, None) => {}
            }
        }
        Ok(())
    })?;
    Ok(json!({"edits": ops.len()}))
}

/// Nudge the selection (with linked partners) by `frames` along the timeline (overwrite).
fn nudge_time(s: &mut Session, frames: i64) -> Result<Value> {
    let d = s.sequence_rate().tick_of(frames);
    let clips = with_links(s, &s.state.selection.clone());
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let items: Vec<(ClipId, TrackId, Tick)> = clips.iter().filter_map(|c| seq.find_item(*c).map(|(t, i)| (*c, t, i.start))).collect();
    let min_start = items.iter().map(|m| m.2).min().unwrap_or_default();
    // never past the sequence start
    let d = d.max(-min_start);
    if d == Tick::ZERO {
        return Ok(json!({"delta": 0}));
    }
    let moves: Vec<(ClipId, TrackId, Tick)> = items.into_iter().map(|(c, t, st)| (c, t, st + d)).collect();
    s.edit_sequence("Nudge", |q, ctx, _| Ok(edit::move_items(q, &moves, false, ctx)?))?;
    Ok(json!({"delta": d.0}))
}

/// Nudge the selected clips one track up (`up`) or down, as the timeline shows them: video up is
/// V(n+1), audio up is A(n-1).
fn nudge_track(s: &mut Session, up: bool) -> Result<Value> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut moves = Vec::new();
    for c in &s.state.selection {
        let Some((tid, it)) = seq.find_item(*c) else { continue };
        let Some(tr) = seq.track(tid) else { continue };
        let list = seq.tracks(tr.kind);
        let idx = list.iter().position(|t| t.id == tid).unwrap_or(0) as i64;
        let step = match (tr.kind, up) {
            (TrackKind::Video, true) | (TrackKind::Audio, false) => 1,
            _ => -1,
        };
        let dest = usize::try_from(idx + step).ok().and_then(|i| list.get(i)).ok_or_else(|| EngineError::Other("no track to move to".into()))?;
        moves.push((*c, dest.id, it.start));
    }
    if moves.is_empty() {
        return Err(EngineError::Other("no clips selected".into()));
    }
    s.edit_sequence("Nudge", |q, ctx, _| Ok(edit::move_items(q, &moves, false, ctx)?))?;
    Ok(json!({"moved": moves.len()}))
}

fn slip_selection(s: &mut Session, frames: i64) -> Result<Value> {
    let rate = s.sequence_rate();
    let clips = with_links(s, &s.state.selection.clone());
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    // media delta per clip (source frames run at the clip's speed)
    let deltas: Vec<(ClipId, Tick)> = clips
        .iter()
        .filter_map(|c| seq.find_item(*c).map(|(_, i)| (*c, Tick((rate.tick_of(frames).0 as f64 * i.speed.abs().max(1e-9)).round() as i64))))
        .collect();
    s.edit_sequence("Slip", |q, ctx, _| {
        for (c, d) in &deltas {
            edit::slip(q, *c, *d, ctx)?;
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

fn slide_selection(s: &mut Session, frames: i64) -> Result<Value> {
    let d = s.sequence_rate().tick_of(frames);
    let clips = with_links(s, &s.state.selection.clone());
    s.edit_sequence("Slide", |q, ctx, _| {
        for c in &clips {
            edit::slide(q, *c, d, ctx)?;
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

// ------------------------------------------------------------------ targeting

fn set_targeting(s: &mut Session, tg: crate::Targeting) -> Result<Value> {
    let seq = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let out = json!({
        "targeted": tg.targeted.iter().map(|t| t.0).collect::<Vec<_>>(),
        "videoDest": tg.video_dest.map(|t| t.0),
        "audioDest": tg.audio_dest.map(|t| t.0),
    });
    s.state.targeting.insert(seq, tg);
    Ok(out)
}

fn track_ids(s: &Session, kind: TrackKind) -> Vec<TrackId> {
    s.active_sequence().map(|q| q.tracks(kind).iter().map(|t| t.id).collect()).unwrap_or_default()
}

fn toggle_all_targets(s: &mut Session, kind: TrackKind) -> Result<Value> {
    let ids = track_ids(s, kind);
    let mut tg = s.targeting();
    if ids.iter().all(|t| tg.targeted.contains(t)) {
        tg.targeted.retain(|t| !ids.contains(t));
    } else {
        for t in ids {
            if !tg.targeted.contains(&t) {
                tg.targeted.push(t);
            }
        }
    }
    set_targeting(s, tg)
}

fn toggle_all_source(s: &mut Session, kind: TrackKind) -> Result<Value> {
    let first = track_ids(s, kind).first().copied();
    let mut tg = s.targeting();
    let slot = if kind == TrackKind::Video { &mut tg.video_dest } else { &mut tg.audio_dest };
    *slot = if slot.is_some() { None } else { first };
    set_targeting(s, tg)
}

fn toggle_target(s: &mut Session, kind: TrackKind, n: usize) -> Result<Value> {
    let t = *track_ids(s, kind)
        .get(n - 1)
        .ok_or_else(|| EngineError::Other(format!("there is no track {}{n}", if kind == TrackKind::Video { "V" } else { "A" })))?;
    let mut tg = s.targeting();
    if let Some(i) = tg.targeted.iter().position(|x| *x == t) {
        tg.targeted.remove(i);
    } else {
        tg.targeted.push(t);
    }
    set_targeting(s, tg)
}

/// Move All Video / Audio Targets Up / Down: shift every targeted track of `kind` by one.
fn move_targets(s: &mut Session, kind: TrackKind, up: bool) -> Result<Value> {
    let ids = track_ids(s, kind);
    let mut tg = s.targeting();
    let idx: Vec<usize> = ids.iter().enumerate().filter(|(_, t)| tg.targeted.contains(t)).map(|(i, _)| i).collect();
    // visually up: video n+1, audio n-1
    let step: i64 = if (kind == TrackKind::Video) == up { 1 } else { -1 };
    if idx.iter().any(|i| *i as i64 + step < 0 || *i as i64 + step >= ids.len() as i64) {
        return Err(EngineError::Other("the targets are already at the edge".into()));
    }
    tg.targeted.retain(|t| !ids.contains(t));
    tg.targeted.extend(idx.iter().map(|i| ids[(*i as i64 + step) as usize]));
    set_targeting(s, tg)
}

fn toggle_targeted_tracks(s: &mut Session, kind: TrackKind, what: &'static str) -> Result<Value> {
    let tg = s.targeting().targeted;
    let ids: Vec<TrackId> = track_ids(s, kind).into_iter().filter(|t| tg.contains(t)).collect();
    if ids.is_empty() {
        return Err(EngineError::Other("no targeted tracks".into()));
    }
    let label = match what {
        "mute" => "Toggle Mute",
        "solo" => "Toggle Solo",
        _ => "Toggle Track Output",
    };
    s.edit_sequence(label, |q, _, _| {
        // all on → all off, else all on
        let get = |t: &filmcraft_project::Track| match what {
            "mute" => t.muted,
            "solo" => t.solo,
            _ => !t.enabled,
        };
        let all = ids.iter().filter_map(|i| q.track(*i)).all(get);
        for i in &ids {
            if let Some(t) = q.track_mut(*i) {
                match what {
                    "mute" => t.muted = !all,
                    "solo" => t.solo = !all,
                    _ => t.enabled = all,
                }
            }
        }
        Ok(())
    })?;
    Ok(Value::Null)
}

// ------------------------------------------------------------------ audio

const VOLUME_MIN_DB: f64 = -287.5;
const VOLUME_MAX_DB: f64 = 15.0;

/// Change the Volume level of the selected audio clips by `db` (keyframed levels shift as a whole).
pub fn adjust_clip_volume(s: &mut Session, db: f64) -> Result<Value> {
    let clips = with_links(s, &s.state.selection.clone());
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let audio: Vec<ClipId> =
        clips.into_iter().filter(|c| seq.find_item(*c).and_then(|(t, _)| seq.track(t)).is_some_and(|t| t.kind == TrackKind::Audio)).collect();
    if audio.is_empty() {
        return Err(EngineError::Other("select audio clips".into()));
    }
    let shift = |v: &ParamValue| match v {
        ParamValue::Float(x) => ParamValue::Float((x + db).clamp(VOLUME_MIN_DB, VOLUME_MAX_DB)),
        o => o.clone(),
    };
    let label = if db > 0.0 { "Increase Clip Volume" } else { "Decrease Clip Volume" };
    s.edit_sequence(label, |q, _, _| {
        for c in &audio {
            let Some((_, it)) = q.find_item_mut(*c) else { continue };
            let Some(p) = it.effect_mut("volume").and_then(|e| e.param_mut("level")) else { continue };
            p.value = shift(&p.value);
            for k in &mut p.keyframes {
                k.value = shift(&k.value);
            }
        }
        Ok(())
    })?;
    let levels: Vec<Value> = s
        .active_sequence()
        .map(|q| {
            audio
                .iter()
                .filter_map(|c| q.find_item(*c))
                .filter_map(|(_, it)| it.effect("volume").and_then(|e| e.params.get("level")).and_then(|p| p.value.as_f64()))
                .map(|v| json!(v))
                .collect()
        })
        .unwrap_or_default();
    Ok(json!({"levels": levels}))
}

// ------------------------------------------------------------------ graphics

fn text_layer_targets(s: &Session) -> Result<(ClipId, Vec<usize>)> {
    let clip = crate::graphics::target_clip(s, &Value::Null).ok_or_else(|| EngineError::Other("select a graphic clip".into()))?;
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| EngineError::Other("no such clip".into()))?;
    let idx = filmcraft_project::graphic::layer_indices(&it.effects);
    if idx.is_empty() {
        return Err(EngineError::Other("the graphic has no layers".into()));
    }
    let chosen: Vec<usize> = if s.state.graphic_layers.is_empty() {
        vec![*idx.last().unwrap_or(&0)]
    } else {
        s.state.graphic_layers.iter().filter_map(|l| idx.get(*l).copied()).collect()
    };
    Ok((clip, chosen))
}

/// Apply `f` to parameter `param` of the targeted graphic layers at the playhead (keyframe-aware).
fn edit_layers(s: &mut Session, label: &str, param: &'static str, text_only: bool, f: impl Fn(&ParamValue) -> Option<ParamValue>) -> Result<Value> {
    let (clip, layers) = text_layer_targets(s)?;
    let tl = s.playhead();
    let n = s.edit_sequence(label, |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let mt = it.source_time_at(tl.clamp(it.start, it.end() - Tick(1)));
        let mut n = 0;
        for ei in &layers {
            let Some(e) = it.effects.get_mut(*ei) else { continue };
            if text_only && !e.params.contains_key("text") {
                continue;
            }
            let Some(p) = e.params.get_mut(param) else { continue };
            if let Some(v) = f(&p.value_at(mt)) {
                p.set_at(mt, v);
                n += 1;
            }
        }
        if n == 0 {
            return Err(EngineError::Other(if text_only { "select a text layer".into() } else { "nothing to change".into() }));
        }
        Ok(n)
    })?;
    Ok(json!({"layers": n}))
}

fn font_size(s: &mut Session, d: f64) -> Result<Value> {
    edit_layers(s, "Font Size", "size", true, |v| match v {
        ParamValue::Float(x) => Some(ParamValue::Float((x + d).clamp(1.0, 2000.0))),
        _ => None,
    })
}

fn leading(s: &mut Session, d: f64) -> Result<Value> {
    edit_layers(s, "Leading", "leading", true, |v| match v {
        ParamValue::Float(x) => Some(ParamValue::Float((x + d).clamp(-5000.0, 5000.0))),
        _ => None,
    })
}

fn align_text(s: &mut Session, choice: u32) -> Result<Value> {
    edit_layers(s, "Text Alignment", "align", true, move |_| Some(ParamValue::Choice(choice)))
}

/// Nudge Selected Object: graphic layers when a graphic is targeted, else the Motion position of
/// the selected video clips.
fn nudge_object(s: &mut Session, dx: f64, dy: f64) -> Result<Value> {
    let shift = move |v: &ParamValue| match v {
        ParamValue::Vec2(p) => Some(ParamValue::Vec2(Vec2::new(p.x + dx, p.y + dy))),
        _ => None,
    };
    if crate::graphics::target_clip(s, &Value::Null).is_some()
        && s.state.selection.iter().all(|c| s.active_sequence().is_some_and(|q| crate::graphics::is_graphic(s, q, *c)))
    {
        return edit_layers(s, "Nudge", "position", false, shift);
    }
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let clips: Vec<ClipId> =
        s.state.selection.iter().copied().filter(|c| seq.find_item(*c).and_then(|(t, _)| seq.track(t)).is_some_and(|t| t.kind == TrackKind::Video)).collect();
    if clips.is_empty() {
        return Err(EngineError::Other("select a video clip or a graphic".into()));
    }
    let tl = s.playhead();
    s.edit_sequence("Nudge", |q, _, _| {
        for c in &clips {
            let Some((_, it)) = q.find_item_mut(*c) else { continue };
            let mt = it.source_time_at(tl.clamp(it.start, it.end() - Tick(1)));
            if let Some(p) = it.effect_mut("motion").and_then(|e| e.param_mut("position"))
                && let Some(v) = shift(&p.value_at(mt))
            {
                p.set_at(mt, v);
            }
        }
        Ok(())
    })?;
    Ok(json!({"clips": clips.len()}))
}

fn has_graphic_or_selection(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if crate::graphics::target_clip(s, &Value::Null).is_some() || !s.state.selection.is_empty() { Ok(()) } else { Err("select a clip or a graphic".into()) }
}
fn has_graphic(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    crate::graphics::target_clip(s, &Value::Null).map(|_| ()).ok_or_else(|| "select a graphic clip".into())
}

// ------------------------------------------------------------------ export frame, poster frame

fn sanitize(name: &str) -> String {
    name.chars().map(|c| if matches!(c, '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|') { '_' } else { c }).collect()
}

/// A monitor frame captured when Export Frame opens. Export uses original media, never proxies.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameExportTarget {
    pub source: bool,
    pub item: filmcraft_project::ItemId,
    pub time: Tick,
    pub name: String,
    pub width: u32,
    pub height: u32,
}

/// Resolve the requested monitor, item and exact frame without changing either playhead.
pub fn frame_export_target(s: &Session, p: &Value) -> Result<FrameExportTarget> {
    const CMD: &str = "file.exportFrame";
    let source = match p.get("target") {
        None => false,
        Some(Value::String(t)) if t == "program" => false,
        Some(Value::String(t)) if t == "source" => true,
        _ => return Err(bad(CMD, "target must be source or program")),
    };
    let key = if source { "item" } else { "sequence" };
    let item = match p.get(key) {
        Some(v) => filmcraft_project::ItemId(v.as_u64().ok_or_else(|| bad(CMD, format!("{key} must be an item id")))?),
        None => if source { s.state.source_item } else { s.state.active_sequence }
            .ok_or_else(|| bad(CMD, "open a video clip or sequence in the requested monitor"))?,
    };
    let pi = s.project.item(item).ok_or_else(|| bad(CMD, "the requested item is unavailable"))?;
    let (width, height) = s.project.source_size(item).ok_or_else(|| bad(CMD, "the requested Source clip has no video frame"))?;
    filmcraft_project::validate_frame_size(width, height).map_err(|e| bad(CMD, e.to_string()))?;
    let (rate, current, bounds) = if source {
        let v = crate::clip_ops::source_view(s, item).ok_or_else(|| bad(CMD, "Source clip is unavailable"))?;
        if v.start.0 < 0 || v.end <= v.start {
            return Err(bad(CMD, "Source clip has invalid bounds"));
        }
        (v.rate, s.state.source_playhead, Some((v.start, v.end)))
    } else {
        let q = s.project.sequence(item).ok_or(EngineError::NoSequence)?;
        (q.settings.frame_rate, s.state.playheads.get(&item).copied().unwrap_or_default(), None)
    };
    let requested = match p.get("time") {
        None => current,
        Some(v) => Tick(v.as_i64().ok_or_else(|| bad(CMD, "time must be integer ticks"))?),
    };
    if rate.num <= 0 || rate.den <= 0 || requested.0 < 0 {
        return Err(bad(CMD, "frame rate must be positive and time non-negative"));
    }
    let invalid = || bad(CMD, "frame time exceeds the supported range");
    let unit = i128::from(filmcraft_time::TICKS_PER_SECOND).checked_mul(i128::from(rate.den)).ok_or_else(invalid)?;
    let fd = unit / i128::from(rate.num);
    if fd <= 0 || fd > i128::from(i64::MAX) {
        return Err(invalid());
    }
    let t = bounds.map(|(a, b)| requested.clamp(a, Tick(b.0.saturating_sub(1)).max(a))).unwrap_or(requested);
    let frame = i128::from(t.0).checked_mul(i128::from(rate.num)).ok_or_else(invalid)? / unit;
    let snapped = frame.checked_mul(unit).ok_or_else(invalid)? / i128::from(rate.num);
    let mut time = Tick(i64::try_from(snapped).map_err(|_| invalid())?);
    if let Some((a, _)) = bounds {
        time = time.max(a);
    }
    Ok(FrameExportTarget { source, item, time, name: pi.name.clone(), width, height })
}

/// Export the captured Source or Program frame as a full-resolution SDR still.
fn export_frame(s: &mut Session, p: &Value) -> Result<Value> {
    let target = frame_export_target(s, p)?;
    let fmt = match p.get("format") {
        None => "png".to_string(),
        Some(v) => v.as_str().ok_or_else(|| bad("file.exportFrame", "format must be a string"))?.to_ascii_lowercase(),
    };
    let format = filmcraft_export::still::StillFormat::parse(&fmt).map_err(|e| bad("file.exportFrame", e.to_string()))?;
    let depth = match p.get("depth") {
        None => 8,
        Some(v) => u8::try_from(v.as_u64().ok_or_else(|| bad("file.exportFrame", "depth must be 8 or 16"))?)
            .map_err(|_| bad("file.exportFrame", "depth must be 8 or 16"))?,
    };
    if !matches!(depth, 8 | 16) || (depth == 16 && !format.supports_16()) {
        return Err(bad("file.exportFrame", "16-bit output requires PNG or TIFF; JPEG and BMP require 8-bit output"));
    }
    let import = match p.get("import") {
        None => false,
        Some(v) => v.as_bool().ok_or_else(|| bad("file.exportFrame", "import must be true or false"))?,
    };
    let ext = format.extension();
    let path = match p.get("path") {
        Some(v) => {
            let path = v.as_str().filter(|v| !v.trim().is_empty()).ok_or_else(|| bad("file.exportFrame", "path must be a non-empty string"))?;
            crate::export_tools::expand_home(path)
        }
        None => {
            let dir = s.path.as_deref().and_then(|p| std::path::Path::new(p).parent()).unwrap_or_else(|| std::path::Path::new(""));
            let base = sanitize(&target.name);
            let candidate =
                (1..=9999).map(|n| dir.join(format!("{base}.Still{n:03}.{ext}")).to_string_lossy().into_owned()).find(|p| s.services.file_size(p).is_err());
            candidate.ok_or_else(|| bad("file.exportFrame", "choose a path: all default still names already exist"))?
        }
    };
    let provider = s.media.full_res_provider(s.project.clone(), s.services.clone());
    let img = if target.source {
        filmcraft_render::render_item(&s.project, target.item, target.time, 1.0, &provider)
            .map_err(EngineError::Other)?
            .ok_or_else(|| bad("file.exportFrame", "cannot decode the Source video frame"))?
    } else {
        let opts = filmcraft_render::RenderOptions { scale: 1.0, captions: true, ..Default::default() };
        filmcraft_render::render_sequence(&s.project, target.item, target.time, opts, &provider).map_err(EngineError::Other)?
    };
    let (w, h) = (
        u32::try_from(img.w).map_err(|_| bad("file.exportFrame", "image width exceeds limits"))?,
        u32::try_from(img.h).map_err(|_| bad("file.exportFrame", "image height exceeds limits"))?,
    );
    let bytes = filmcraft_export::still::encode(&img, format, depth).map_err(|e| EngineError::Other(e.to_string()))?;
    s.services.write_file(&path, &bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let mut out =
        json!({"path": path, "width": w, "height": h, "depth": depth, "target": if target.source { "source" } else { "program" }, "time": target.time.0});
    if import {
        out["import"] = s.execute("file.import", json!({"paths": [path]}))?;
    }
    s.toast(format!("Exported frame to {path}"));
    Ok(out)
}

/// The item Set / Clear Poster Frame acts on, and the frame: `item` + time, else the Source
/// monitor clip at its playhead, else the first Project panel selection (its current poster).
fn poster_target(s: &Session, p: &Value) -> Option<(filmcraft_project::ItemId, Tick)> {
    if let Some(i) = item_p(p, "item") {
        return Some((i, time_p(s, p, "").unwrap_or_default()));
    }
    let sel = s.state.project_selection.first().copied();
    match (s.state.source_item, sel) {
        (Some(src), None) => Some((src, s.state.source_playhead)),
        (Some(src), Some(sel)) if src == sel => Some((src, s.state.source_playhead)),
        (_, Some(sel)) => Some((sel, time_p(s, p, "").unwrap_or_default())),
        (None, None) => None,
    }
}

fn set_poster(s: &mut Session, p: &Value, clear: bool) -> Result<Value> {
    let (item, t) = poster_target(s, p).ok_or_else(|| EngineError::Other("open a clip in the Source monitor or select one in the Project panel".into()))?;
    let it = s.project.item(item).ok_or_else(|| bad("clip.setPosterFrame", "no such item"))?;
    if matches!(it.kind, ItemKind::AdjustmentLayer { .. }) {
        return Err(EngineError::Other("adjustment layers have no poster frame".into()));
    }
    let snapped = it.frame_rate().snap(t);
    s.edit(if clear { "Clear Poster Frame" } else { "Set Poster Frame" }, |pr, _| {
        let it = pr.item_mut(item).ok_or_else(|| bad("clip.setPosterFrame", "no such item"))?;
        if clear {
            it.metadata.remove(POSTER_FRAME_KEY);
        } else {
            it.metadata.insert(POSTER_FRAME_KEY.into(), snapped.0.to_string());
        }
        Ok(())
    })?;
    Ok(json!({"item": item.0, "posterFrame": (!clear).then_some(snapped.0)}))
}

fn has_poster_target(s: &Session) -> std::result::Result<(), String> {
    if s.state.source_item.is_some() || !s.state.project_selection.is_empty() {
        Ok(())
    } else {
        Err("open a clip in the Source monitor or select one in the Project panel".into())
    }
}

// ------------------------------------------------------------------ registry

pub fn commands() -> Vec<CommandSpec> {
    use TrackKind::{Audio, Video};
    let mut v = vec![
        // ---- navigation
        spec("playhead.nextEditAnyTrack", "Go to Next Edit Point on Any Track", Some("Shift+Down"), "{}", has_seq, |s, _| go_to_edit(s, true, true)),
        spec("playhead.prevEditAnyTrack", "Go to Previous Edit Point on Any Track", Some("Shift+Up"), "{}", has_seq, |s, _| go_to_edit(s, false, true)),
        spec("playhead.selectedClipStart", "Go to Selected Clip Start", Some("Shift+Home"), "{}", has_selection, |s, _| go_to_selected(s, false)),
        spec("playhead.selectedClipEnd", "Go to Selected Clip End", Some("Shift+End"), "{}", has_selection, |s, _| go_to_selected(s, true)),
        spec("sequence.revealNested", "Reveal Nested Sequence", Some("Cmd+Alt+F"), "{}", has_seq, |s, _| reveal_nested(s)),
        // ---- selection
        spec("timeline.selectClipAtPlayhead", "Select Clip at Playhead", Some("D"), "{}", has_seq, |s, _| select_clip_at_playhead(s)),
        spec("timeline.selectNextClip", "Select Next Clip", Some("Cmd+Down"), "{}", has_seq, |s, _| select_step(s, true)),
        spec("timeline.selectPrevClip", "Select Previous Clip", Some("Cmd+Up"), "{}", has_seq, |s, _| select_step(s, false)),
        // ---- trimming
        spec("trim.extendPreviousEdit", "Extend Previous Edit To Playhead", Some("Shift+Q"), "{}", has_seq, |s, _| extend_edit(s, false)),
        spec("trim.extendNextEdit", "Extend Next Edit To Playhead", Some("Shift+W"), "{}", has_seq, |s, _| extend_edit(s, true)),
        spec("timeline.nudgeLeft", "Nudge Clip Selection Left One Frame", None, "{}", has_selection, |s, _| nudge_time(s, -1)),
        spec("timeline.nudgeRight", "Nudge Clip Selection Right One Frame", None, "{}", has_selection, |s, _| nudge_time(s, 1)),
        spec("timeline.nudgeLeft5", "Nudge Clip Selection Left Five Frames", None, "{}", has_selection, |s, _| nudge_time(s, -5)),
        spec("timeline.nudgeRight5", "Nudge Clip Selection Right Five Frames", None, "{}", has_selection, |s, _| nudge_time(s, 5)),
        spec("timeline.nudgeUp", "Nudge Clip Selection Up", None, "{}", has_selection, |s, _| nudge_track(s, true)),
        spec("timeline.nudgeDown", "Nudge Clip Selection Down", None, "{}", has_selection, |s, _| nudge_track(s, false)),
        spec("timeline.slipLeft", "Slip Clip Selection Left One Frame", None, "{}", has_selection, |s, _| slip_selection(s, 1)),
        spec("timeline.slipRight", "Slip Clip Selection Right One Frame", None, "{}", has_selection, |s, _| slip_selection(s, -1)),
        spec("timeline.slipLeft5", "Slip Clip Selection Left Five Frames", None, "{}", has_selection, |s, _| slip_selection(s, 5)),
        spec("timeline.slipRight5", "Slip Clip Selection Right Five Frames", None, "{}", has_selection, |s, _| slip_selection(s, -5)),
        spec("timeline.slideLeft", "Slide Clip Selection Left One Frame", None, "{}", has_selection, |s, _| slide_selection(s, -1)),
        spec("timeline.slideRight", "Slide Clip Selection Right One Frame", None, "{}", has_selection, |s, _| slide_selection(s, 1)),
        spec("timeline.slideLeft5", "Slide Clip Selection Left Five Frames", None, "{}", has_selection, |s, _| slide_selection(s, -5)),
        spec("timeline.slideRight5", "Slide Clip Selection Right Five Frames", None, "{}", has_selection, |s, _| slide_selection(s, 5)),
        // ---- targeting
        spec("timeline.toggleAllVideoTargets", "Toggle All Video Targets", Some("Cmd+0"), "{}", has_seq, |s, _| toggle_all_targets(s, Video)),
        spec("timeline.toggleAllAudioTargets", "Toggle All Audio Targets", Some("Cmd+9"), "{}", has_seq, |s, _| toggle_all_targets(s, Audio)),
        spec("timeline.toggleAllSourceVideo", "Toggle All Source Video", Some("Cmd+Alt+0"), "{}", has_seq, |s, _| toggle_all_source(s, Video)),
        spec("timeline.toggleAllSourceAudio", "Toggle All Source Audio", Some("Cmd+Alt+9"), "{}", has_seq, |s, _| toggle_all_source(s, Audio)),
        spec("timeline.moveVideoTargetsUp", "Move All Video Targets Up", None, "{}", has_seq, |s, _| move_targets(s, Video, true)),
        spec("timeline.moveVideoTargetsDown", "Move All Video Targets Down", None, "{}", has_seq, |s, _| move_targets(s, Video, false)),
        spec("timeline.moveAudioTargetsUp", "Move All Audio Targets Up", None, "{}", has_seq, |s, _| move_targets(s, Audio, true)),
        spec("timeline.moveAudioTargetsDown", "Move All Audio Targets Down", None, "{}", has_seq, |s, _| move_targets(s, Audio, false)),
        spec("timeline.toggleMuteTargetedAudio", "Toggle Mute for All Targeted Audio Tracks", None, "{}", has_seq, |s, _| {
            toggle_targeted_tracks(s, Audio, "mute")
        }),
        spec("timeline.toggleSoloTargetedAudio", "Toggle Solo for All Targeted Audio Tracks", None, "{}", has_seq, |s, _| {
            toggle_targeted_tracks(s, Audio, "solo")
        }),
        spec("timeline.toggleOutputTargetedVideo", "Toggle Track Output for All Targeted Video Tracks", None, "{}", has_seq, |s, _| {
            toggle_targeted_tracks(s, Video, "output")
        }),
        // ---- audio
        spec("clip.volumeUp", "Increase Clip Volume", Some("]"), "{}", has_selection, |s, _| adjust_clip_volume(s, 1.0)),
        spec("clip.volumeDown", "Decrease Clip Volume", Some("["), "{}", has_selection, |s, _| adjust_clip_volume(s, -1.0)),
        spec("clip.volumeUpMany", "Increase Clip Volume Many", Some("Shift+]"), "{}", has_selection, |s, _| {
            let d = s.prefs.audio.large_volume_adjustment;
            adjust_clip_volume(s, d)
        }),
        spec("clip.volumeDownMany", "Decrease Clip Volume Many", Some("Shift+["), "{}", has_selection, |s, _| {
            let d = s.prefs.audio.large_volume_adjustment;
            adjust_clip_volume(s, -d)
        }),
        spec("clip.nudgeVolumeUp1", "Nudge Volume +1dB", None, "{}", has_selection, |s, _| adjust_clip_volume(s, 1.0)),
        spec("clip.nudgeVolumeUp3", "Nudge Volume +3dB", None, "{}", has_selection, |s, _| adjust_clip_volume(s, 3.0)),
        spec("clip.nudgeVolumeDown1", "Nudge Volume -1dB", None, "{}", has_selection, |s, _| adjust_clip_volume(s, -1.0)),
        spec("clip.nudgeVolumeDown3", "Nudge Volume -3dB", None, "{}", has_selection, |s, _| adjust_clip_volume(s, -3.0)),
        spec("audio.toggleScrubbing", "Toggle Audio During Scrubbing", Some("Shift+S"), "{}", always, |s, _| {
            let on = !s.prefs.audio.scrub_audio;
            s.execute("prefs.set", json!({"key": "audio.scrubAudio", "value": on}))?;
            s.toast(if on { "Audio during scrubbing on" } else { "Audio during scrubbing off" });
            Ok(json!({"scrubAudio": on}))
        }),
        // ---- graphics and titles
        spec("graphics.fontSizeUp", "Increase Font Size by One Unit", Some("Cmd+Alt+Right"), "{}", has_graphic, |s, _| font_size(s, 1.0)),
        spec("graphics.fontSizeDown", "Decrease Font Size by One Unit", Some("Cmd+Alt+Left"), "{}", has_graphic, |s, _| font_size(s, -1.0)),
        spec("graphics.fontSizeUp5", "Increase Font Size by Five Units", Some("Cmd+Alt+Shift+Right"), "{}", has_graphic, |s, _| font_size(s, 5.0)),
        spec("graphics.fontSizeDown5", "Decrease Font Size by Five Units", Some("Cmd+Alt+Shift+Left"), "{}", has_graphic, |s, _| font_size(s, -5.0)),
        spec("graphics.leadingUp", "Increase Leading by One Unit", Some("Alt+Up"), "{}", has_graphic, |s, _| leading(s, 1.0)),
        spec("graphics.leadingDown", "Decrease Leading by One Unit", Some("Alt+Down"), "{}", has_graphic, |s, _| leading(s, -1.0)),
        spec("graphics.leadingUp5", "Increase Leading by Five Units", Some("Alt+Shift+Up"), "{}", has_graphic, |s, _| leading(s, 5.0)),
        spec("graphics.leadingDown5", "Decrease Leading by Five Units", Some("Alt+Shift+Down"), "{}", has_graphic, |s, _| leading(s, -5.0)),
        spec("graphics.alignTextLeft", "Left align text", Some("Cmd+Shift+L"), "{}", has_graphic, |s, _| align_text(s, 0)),
        spec("graphics.alignTextCenter", "Center align text", Some("Cmd+Shift+C"), "{}", has_graphic, |s, _| align_text(s, 1)),
        spec("graphics.alignTextRight", "Right align text", Some("Cmd+Shift+R"), "{}", has_graphic, |s, _| align_text(s, 2)),
        spec("graphics.nudgeLeft", "Nudge Selected Object to left by one", None, "{}", has_graphic_or_selection, |s, _| nudge_object(s, -1.0, 0.0)),
        spec("graphics.nudgeRight", "Nudge Selected Object to right by one", None, "{}", has_graphic_or_selection, |s, _| nudge_object(s, 1.0, 0.0)),
        spec("graphics.nudgeUp", "Nudge Selected Object up by one", None, "{}", has_graphic_or_selection, |s, _| nudge_object(s, 0.0, -1.0)),
        spec("graphics.nudgeDown", "Nudge Selected Object down by one", None, "{}", has_graphic_or_selection, |s, _| nudge_object(s, 0.0, 1.0)),
        spec("graphics.nudgeLeft5", "Nudge Selected Object to left by five", None, "{}", has_graphic_or_selection, |s, _| nudge_object(s, -5.0, 0.0)),
        spec("graphics.nudgeRight5", "Nudge Selected Object to right by five", None, "{}", has_graphic_or_selection, |s, _| nudge_object(s, 5.0, 0.0)),
        spec("graphics.nudgeUp5", "Nudge Selected Object up by five", None, "{}", has_graphic_or_selection, |s, _| nudge_object(s, 0.0, -5.0)),
        spec("graphics.nudgeDown5", "Nudge Selected Object down by five", None, "{}", has_graphic_or_selection, |s, _| nudge_object(s, 0.0, 5.0)),
        // ---- export frame, poster frame
        // Shift+E is Clip > Enable in FilmCraft Default; the Premiere preset moves it here
        spec(
            "file.exportFrame",
            "Export Frame",
            None,
            r#"{"path":str?,"format":"png|jpeg|tiff|bmp"?,"depth":8|16?,"import":bool?,"target":"source|program"?,"item":id?,"sequence":id?,"time":ticks?}"#,
            always,
            export_frame,
        ),
        spec("clip.setPosterFrame", "Set Poster Frame", Some("Cmd+P"), r#"{"item":id?,"time":ticks?}"#, has_poster_target, |s, p| set_poster(s, p, false)),
        spec("clip.clearPosterFrame", "Clear Poster Frame", Some("Alt+P"), r#"{"item":id?}"#, has_poster_target, |s, p| set_poster(s, p, true)),
    ];
    v.extend(target_toggles());
    v
}

/// Toggle Target Video 1–8 / Audio 1–8.
fn target_toggles() -> Vec<CommandSpec> {
    macro_rules! tt {
        ($id:literal, $label:literal, $kind:expr, $n:literal) => {
            spec($id, $label, None, "{}", has_seq, |s, _| toggle_target(s, $kind, $n))
        };
    }
    use TrackKind::{Audio, Video};
    vec![
        tt!("timeline.toggleTargetV1", "Toggle Target Video 1", Video, 1),
        tt!("timeline.toggleTargetV2", "Toggle Target Video 2", Video, 2),
        tt!("timeline.toggleTargetV3", "Toggle Target Video 3", Video, 3),
        tt!("timeline.toggleTargetV4", "Toggle Target Video 4", Video, 4),
        tt!("timeline.toggleTargetV5", "Toggle Target Video 5", Video, 5),
        tt!("timeline.toggleTargetV6", "Toggle Target Video 6", Video, 6),
        tt!("timeline.toggleTargetV7", "Toggle Target Video 7", Video, 7),
        tt!("timeline.toggleTargetV8", "Toggle Target Video 8", Video, 8),
        tt!("timeline.toggleTargetA1", "Toggle Target Audio 1", Audio, 1),
        tt!("timeline.toggleTargetA2", "Toggle Target Audio 2", Audio, 2),
        tt!("timeline.toggleTargetA3", "Toggle Target Audio 3", Audio, 3),
        tt!("timeline.toggleTargetA4", "Toggle Target Audio 4", Audio, 4),
        tt!("timeline.toggleTargetA5", "Toggle Target Audio 5", Audio, 5),
        tt!("timeline.toggleTargetA6", "Toggle Target Audio 6", Audio, 6),
        tt!("timeline.toggleTargetA7", "Toggle Target Audio 7", Audio, 7),
        tt!("timeline.toggleTargetA8", "Toggle Target Audio 8", Audio, 8),
    ]
}

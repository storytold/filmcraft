//! Effect and transition clipboard payloads share the normal Copy/Paste workflow.
use filmcraft_project::{TrackItem, TrackKind, Transition};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::{EngineError, Result, Session};

#[derive(Clone, Debug)]
pub enum Payload {
    Effect(TrackKind, Box<TrackItem>),
    Transition(TrackKind, Transition, Tick),
}

pub fn can_copy(s: &Session) -> std::result::Result<(), String> {
    crate::commands::has_seq(s)?;
    if s.state.selection.is_empty() && s.state.transition_selection.is_empty() { Err("select a clip or transition".into()) } else { Ok(()) }
}

pub fn copy_transition(s: &mut Session) -> Result<Value> {
    if s.state.transition_selection.len() != 1 {
        return Err(EngineError::Other("select one transition to copy".into()));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, tr) = q
        .all_tracks()
        .find_map(|t| t.transitions.iter().find(|tr| s.state.transition_selection.contains(&tr.id)).map(|tr| (t, tr)))
        .ok_or_else(|| EngineError::Other("the selected transition no longer exists".into()))?;
    let (_, cut, _) = filmcraft_edit::transitions::bounds(track, tr).ok_or_else(|| EngineError::Other("transition has no cut".into()))?;
    let payload = Payload::Transition(track.kind, tr.clone(), cut);
    s.state.clipboard.clear();
    s.state.clipboard_markers.clear();
    s.state.effect_clipboard = Some(payload);
    Ok(json!({"copied": 1, "type": "transition"}))
}

pub fn copy_effect(s: &mut Session, p: &Value) -> Result<Value> {
    let selected = s.state.selected_effect.as_ref();
    let clip = match p.get("clip") {
        Some(value) => value.as_u64().map(filmcraft_project::ClipId),
        None => selected.map(|e| e.clip),
    }
    .ok_or_else(|| EngineError::Other("select an effect or provide a valid clip id".into()))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (tid, it) = q.find_item(clip).ok_or_else(|| EngineError::Other("no such clip".into()))?;
    let index = match p.get("index") {
        Some(value) => value.as_u64().and_then(|v| usize::try_from(v).ok()),
        None => selected.filter(|e| e.clip == clip).and_then(|e| e.index(&it.effects)),
    }
    .ok_or_else(|| EngineError::Other("invalid or missing effect index".into()))?;
    let effect = it.effects.get(index).ok_or_else(|| EngineError::Other("no such effect".into()))?.clone();
    if filmcraft_project::graphic::is_layer(&effect) {
        return Err(EngineError::Other("graphic layers use the graphics copy workflow".into()));
    }
    let kind = q.track(tid).ok_or_else(|| EngineError::Other("no such track".into()))?.kind;
    let mut source = it.clone();
    source.effects = vec![effect];
    s.state.clipboard.clear();
    s.state.clipboard_markers.clear();
    s.state.effect_clipboard = Some(Payload::Effect(kind, Box::new(source)));
    Ok(json!({"copied": 1, "type": "effect"}))
}

pub fn paste(s: &mut Session) -> Result<Value> {
    let payload = s.state.effect_clipboard.clone().ok_or_else(|| EngineError::Other("copy an effect or transition first".into()))?;
    match payload {
        Payload::Effect(kind, source) => {
            let targets = crate::commands::with_links(s, &s.state.selection);
            let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
            let targets: Vec<_> = targets.into_iter().filter(|c| q.find_item(*c).and_then(|(t, _)| q.track(t)).is_some_and(|t| t.kind == kind)).collect();
            if targets.is_empty() {
                return Err(EngineError::Other("select compatible destination clips".into()));
            }
            if targets.iter().any(|c| q.find_item(*c).and_then(|(t, _)| q.track(t)).is_some_and(|t| t.locked)) {
                return Err(filmcraft_edit::EditError::Locked.into());
            }
            let n = s.edit_sequence("Paste Effect", |q, _, _| {
                for c in &targets {
                    let Some((_, dst)) = q.find_item_mut(*c) else { continue };
                    for effect in &source.effects {
                        let mut effect = effect.clone();
                        crate::clip_ops::retime_effect(&mut effect, &source, dst, false);
                        if effect.def().is_some_and(|d| d.intrinsic) {
                            if let Some(slot) = dst.effects.iter_mut().find(|e| e.effect == effect.effect) {
                                *slot = effect;
                            } else {
                                dst.effects.push(effect);
                            }
                        } else {
                            dst.effects.push(effect);
                        }
                    }
                }
                Ok(targets.len())
            })?;
            Ok(json!({"clips": n}))
        }
        Payload::Transition(kind, source, source_cut) => {
            let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
            let mut inputs = Vec::new();
            for ep in &s.state.edit_points {
                if q.find_item(ep.clip).and_then(|(t, _)| q.track(t)).is_some_and(|t| t.kind == kind) {
                    inputs.push(json!({"clip": ep.clip.0, "edge": if ep.out { "out" } else { "in" }}));
                }
            }
            if inputs.is_empty() {
                for c in &s.state.selection {
                    if q.find_item(*c).and_then(|(t, _)| q.track(t)).is_some_and(|t| t.kind == kind) {
                        let mut input = json!({"clip": c.0});
                        if source.from.is_none() {
                            input["edge"] = json!("in");
                        } else if source.to.is_none() {
                            input["edge"] = json!("out");
                        }
                        inputs.push(input);
                    }
                }
            }
            if inputs.is_empty() {
                if !s.state.selection.is_empty() || !s.state.edit_points.is_empty() {
                    return Err(EngineError::Other("select a cut on a compatible track".into()));
                }
                inputs.push(json!({}));
            }
            let mut plans = Vec::new();
            for mut input in inputs {
                input["effect"] = json!(source.effect.effect);
                input["frames"] = json!(s.sequence_rate().frame_at(source.duration).max(1));
                let (tid, mut tr) = crate::commands::preview_transition(s, &input, kind)?;
                tr.effect = source.effect.clone();
                tr.reverse = source.reverse;
                tr.align = source.align;
                if tr.from.is_some()
                    && tr.to.is_some()
                    && let Some(track) = q.track(tid)
                    && let Some((_, cut, _)) = filmcraft_edit::transitions::bounds(track, &tr)
                {
                    tr.start = Tick(cut.0.saturating_add(source.start.0.saturating_sub(source_cut.0)));
                }
                if !plans.iter().any(|(t, old): &(filmcraft_project::TrackId, Transition)| *t == tid && old.from == tr.from && old.to == tr.to) {
                    plans.push((tid, tr));
                }
            }
            let n = s.edit_sequence("Paste Transition", |q, ctx, _| {
                for (tid, tr) in &plans {
                    filmcraft_edit::add_transition(q, *tid, tr.clone(), ctx)?;
                }
                Ok(plans.len())
            })?;
            Ok(json!({"transitions": n}))
        }
    }
}

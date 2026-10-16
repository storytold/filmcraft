//! Copy, cut and paste effects, as in Premiere's Effect Controls: with an effect selected there,
//! Copy / Cut put that effect on the clipboard and Paste puts it on the selected clips (another
//! clip, an adjustment layer). Delete removes the selected effect, or the selected keyframes,
//! instead of the clip.
//!
//! The clipboard holds clips or effects, never both, as Premiere's does: copying effects empties
//! the clip clipboard and copying clips empties this one. `edit.paste` pastes whichever it holds.
//! Which of Copy / Cut / Delete acts on effects depends on keyboard focus, so the frontend sends
//! them here when Effect Controls has it.

use filmcraft_project::{ClipId, EffectInstance, Sequence, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, clip_p, clips_p, has_seq, u64_p, with_links};
use crate::{EngineError, Result, Session};

/// Effects on the clipboard and the clip they came from (its media range retimes keyframes).
#[derive(Clone, Debug)]
pub struct CopiedEffects {
    pub kind: TrackKind,
    pub from: TrackItem,
    pub effects: Vec<EffectInstance>,
}

const MAX_KEYS: usize = 4096;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: "effects.copy",
            label: "Copy Effect",
            menu: &[],
            shortcut: None,
            params: r#"{"clip":id?,"effect":index? (default: the effect selected in Effect Controls)}"#,
            enabled: has_seq,
            run: |s, p| copy(s, p, false),
            journal: true,
        },
        CommandSpec {
            id: "effects.cut",
            label: "Cut Effect",
            menu: &[],
            shortcut: None,
            params: r#"{"clip":id?,"effect":index? (default: the effect selected in Effect Controls)}"#,
            enabled: has_seq,
            run: |s, p| copy(s, p, true),
            journal: true,
        },
        CommandSpec {
            id: "effects.paste",
            label: "Paste Effects",
            menu: &[],
            shortcut: None,
            params: r#"{"clips":[id]? (default: the selected clips)}"#,
            enabled: has_effects,
            run: paste,
            journal: true,
        },
        CommandSpec {
            id: "effects.deleteKeyframes",
            label: "Delete Keyframes",
            menu: &[],
            shortcut: None,
            params: r#"{"keyframes":[{"clip":id,"effect":index,"param":str,"mask":n?,"time":mediaTicks}]}"#,
            enabled: has_seq,
            run: delete_keyframes,
            journal: true,
        },
    ]
}

pub(crate) fn has_effects(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.state.effect_clipboard.is_some() { Ok(()) } else { Err("copy an effect first".into()) }
}

/// The clip and effect index a call names, or the effect selected in Effect Controls.
fn target(s: &Session, p: &Value, cmd: &str) -> Result<(ClipId, usize)> {
    let named = p.get("effect").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok());
    match (clip_p(p, "clip"), named) {
        (Some(c), Some(i)) => Ok((c, i)),
        (None, None) => {
            let sel = s.state.selected_effect.as_ref().ok_or_else(|| bad(cmd, "select an effect in Effect Controls"))?;
            let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
            let (_, it) = q.find_item(sel.clip).ok_or_else(|| bad(cmd, "the selected effect's clip is gone"))?;
            let i = sel.index(&it.effects).ok_or_else(|| bad(cmd, "the selected effect is gone"))?;
            Ok((sel.clip, i))
        }
        _ => Err(bad(cmd, "name both `clip` and `effect`, or neither")),
    }
}

fn copy(s: &mut Session, p: &Value, cut: bool) -> Result<Value> {
    let cmd = if cut { "effects.cut" } else { "effects.copy" };
    let (clip, index) = target(s, p, cmd)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, it) = q.find_item(clip).ok_or_else(|| bad(cmd, "no such clip"))?;
    let kind = q.track(track).map(|t| t.kind).unwrap_or_default();
    let effect = it.effects.get(index).ok_or_else(|| bad(cmd, "no such effect on the clip"))?;
    if filmcraft_project::graphic::is_layer(effect) {
        return Err(bad(cmd, "graphic layers are copied in the Essential Graphics panel"));
    }
    if effect.essential {
        return Err(bad(cmd, "this effect belongs to the Essential Sound panel"));
    }
    let intrinsic = effect.def().is_some_and(|d| d.intrinsic);
    let copied = CopiedEffects { kind, from: it.clone(), effects: vec![effect.clone()] };
    // the effect selected in Effect Controls, and where it is once the cut effect is gone
    let effects = it.effects.clone();
    let sel = s.state.selected_effect.clone().filter(|e| e.clip == clip).and_then(|e| e.index(&effects));
    // Motion, Opacity, Volume… belong to every clip: Cut copies them and leaves them in place
    let removed = cut && !intrinsic;
    if removed {
        s.edit_sequence("Cut Effect", |q, _, _| {
            let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
            if index < it.effects.len() {
                it.effects.remove(index);
            }
            Ok(())
        })?;
        if let Some(i) = sel {
            let mut rest = effects;
            if index < rest.len() {
                rest.remove(index);
            }
            s.state.selected_effect = if i == index { None } else { crate::EffectSel::of(clip, &rest, if i > index { i - 1 } else { i }) };
        }
    }
    s.state.effect_clipboard = Some(copied);
    s.state.clipboard.clear();
    s.state.clipboard_markers.clear();
    Ok(json!({"copied": 1, "removed": removed}))
}

/// Where a pasted standard effect goes: before the intrinsic ones (render order), as Apply Effect
/// puts it; after the others on audio clips.
fn insert_at(it: &TrackItem, kind: TrackKind) -> usize {
    if kind == TrackKind::Audio {
        return it.effects.len();
    }
    it.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(it.effects.len())
}

/// Keyframes keep their offset from the clip's In point on the clip they are pasted on.
fn retime(e: &mut EffectInstance, from: &TrackItem, to: &TrackItem) {
    let shift = |k: Tick| to.source_in + (k - from.source_in);
    for p in e.params.values_mut() {
        for k in &mut p.keyframes {
            k.time = shift(k.time);
        }
    }
    for m in &mut e.masks {
        for p in [&mut m.path, &mut m.feather, &mut m.opacity, &mut m.expansion] {
            for k in &mut p.keyframes {
                k.time = shift(k.time);
            }
        }
    }
}

pub(crate) fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.paste";
    let copied = s.state.effect_clipboard.clone().ok_or_else(|| bad(CMD, "copy an effect first"))?;
    let named = clips_p(s, p);
    if named.is_empty() {
        return Err(bad(CMD, "select the clips to paste on"));
    }
    // a video clip's linked audio takes the audio effects (and the other way round)
    let targets = with_links(s, &named);
    let n = s.edit_sequence("Paste Effects", |q, _, _| {
        let mut n = 0;
        for c in &targets {
            if paste_on(q, *c, &copied) {
                n += 1;
            }
        }
        Ok(n)
    })?;
    if n == 0 {
        let what = if copied.kind == TrackKind::Audio { "an audio clip" } else { "a video clip or an adjustment layer" };
        return Err(bad(CMD, format!("select {what} to paste this effect on")));
    }
    Ok(json!({"clips": n}))
}

/// Paste the copied effects on clip `c` when it is on a track of their kind; true when it changed.
fn paste_on(q: &mut Sequence, c: ClipId, copied: &CopiedEffects) -> bool {
    let Some((tid, _)) = q.find_item(c) else { return false };
    if q.track(tid).map(|t| t.kind) != Some(copied.kind) {
        return false;
    }
    let Some((_, dst)) = q.find_item_mut(c) else { return false };
    let before = dst.clone();
    for e in &copied.effects {
        let mut e = e.clone();
        retime(&mut e, &copied.from, &before);
        if e.def().is_some_and(|d| d.intrinsic) {
            // Motion, Opacity… replace the clip's own
            match dst.effects.iter_mut().find(|x| x.effect == e.effect) {
                Some(slot) => *slot = e,
                None => dst.effects.push(e),
            }
        } else {
            let at = insert_at(dst, copied.kind);
            dst.effects.insert(at, e);
        }
    }
    *dst != before
}

/// Delete the keyframes selected in Effect Controls, as one undo step.
fn delete_keyframes(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.deleteKeyframes";
    let raw = p.get("keyframes").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need `keyframes`"))?;
    if raw.is_empty() || raw.len() > MAX_KEYS {
        return Err(bad(CMD, "keyframe count must be between 1 and 4096"));
    }
    let mut keys = Vec::with_capacity(raw.len());
    for k in raw {
        let clip = clip_p(k, "clip").ok_or_else(|| bad(CMD, "a keyframe needs `clip`"))?;
        let effect = u64_p(k, "effect").and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(CMD, "a keyframe needs `effect` (index)"))?;
        let param = k.get("param").and_then(Value::as_str).filter(|s| s.len() <= 256).ok_or_else(|| bad(CMD, "a keyframe needs `param`"))?.to_string();
        let mask = u64_p(k, "mask").and_then(|i| usize::try_from(i).ok());
        let time = k.get("time").and_then(Value::as_i64).map(Tick).ok_or_else(|| bad(CMD, "a keyframe needs `time` (media ticks)"))?;
        keys.push((clip, effect, param, mask, time));
    }
    let n = s.edit_sequence("Delete Keyframes", |q, _, _| {
        let mut n = 0;
        for (clip, effect, param, mask, time) in &keys {
            let Some((_, it)) = q.find_item_mut(*clip) else { continue };
            let Some(e) = it.effects.get_mut(*effect) else { continue };
            let prm = match mask {
                Some(m) => e.masks.get_mut(*m).and_then(|m| m.param_mut(param)),
                None => e.params.get_mut(param),
            };
            if prm.is_some_and(|prm| prm.remove_keyframe_at(*time)) {
                n += 1;
            }
        }
        if n == 0 {
            return Err(bad(CMD, "none of those keyframes exist"));
        }
        Ok(n)
    })?;
    Ok(json!({"deleted": n}))
}

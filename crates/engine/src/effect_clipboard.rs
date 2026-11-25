//! Copy, cut and paste effects, as in Premiere's Effect Controls: with an effect selected there,
//! Copy / Cut put that effect on the clipboard and Paste puts it on the selected clips (another
//! clip, an adjustment layer).
//!
//! The clipboard holds clips, captions or effects, only one at a time: copying effects empties the
//! clip and caption clipboards and copying clips or captions empties this one. `edit.paste` pastes
//! whichever it holds. Whether Copy / Cut acts on effects depends on keyboard focus, so the frontend
//! sends them here when Effect Controls has it.

use filmcraft_project::{ClipId, EffectInstance, Sequence, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, clip_p, clips_p, has_seq, with_links};
use crate::{EngineError, Result, Session};

/// Effects on the clipboard and the clip they came from (its media range retimes keyframes).
#[derive(Clone, Debug)]
pub struct CopiedEffects {
    pub kind: TrackKind,
    pub from: TrackItem,
    pub effects: Vec<EffectInstance>,
}

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
            named: crate::commands::NamedKeys::NONE,
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
            named: crate::commands::NamedKeys::NONE,
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
            named: crate::commands::NamedKeys::NONE,
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
    s.state.clipboard_transitions.clear();
    s.state.caption_clipboard.clear();
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
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let takes = |c: &ClipId| seq.find_item(*c).and_then(|(t, _)| seq.track(t)).is_some_and(|t| t.kind == copied.kind);
    if !targets.iter().any(takes) {
        let what = if copied.kind == TrackKind::Audio { "an audio clip" } else { "a video clip or an adjustment layer" };
        return Err(bad(CMD, format!("select {what} to paste this effect on")));
    }
    // a clip that already has the copied settings (Motion pasted on itself) changes nothing: no undo
    // step. An error leaves the project and the history as they were.
    let mut n = 0;
    let r = s.edit_sequence("Paste Effects", |q, _, _| {
        n = targets.iter().filter(|c| paste_on(q, **c, &copied)).count();
        if n == 0 { Err(bad(CMD, "nothing to change")) } else { Ok(()) }
    });
    if n > 0 {
        r?;
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

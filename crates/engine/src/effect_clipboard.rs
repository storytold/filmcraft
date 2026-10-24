//! Copy and paste of effects in Effect Controls (#1680), as in Premiere Pro:
//!
//! | command | what it does |
//! |---|---|
//! | `effects.copy` | copies effects of one clip (the selected effect by default) with their settings and keyframes |
//! | `effects.paste` | pastes them onto clips of the same kind: a standard effect is added, a fixed effect (Motion, Opacity, Volume…) takes the copied settings |
//! | `effects.pasteSettings` | replaces one effect's settings with those of the copied effect of the same kind |
//!
//! Keyframes keep their offset from the clip's start. Each paste is one undo step.

use filmcraft_project::{ClipId, EffectInstance, TrackItem, TrackKind};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, clip_p, clips_p, has_seq, with_links};
use crate::{EngineError, Result, Session};

/// Effects copied from one clip.
#[derive(Clone, Debug)]
pub struct CopiedEffects {
    /// The kind of track the clip was on: effects paste only onto clips of the same kind.
    pub kind: TrackKind,
    /// The clip as it was when copied (its source range places the keyframes).
    pub source: TrackItem,
    pub effects: Vec<EffectInstance>,
}

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: fn(&mut Session, &Value) -> Result<Value>,
) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal: true }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("effects.copy", "Copy Effect", r#"{"clip":id?,"indices":[usize]?}"#, has_seq, copy),
        spec("effects.paste", "Paste Effects", r#"{"clips":[id]?}"#, can_paste, paste),
        spec("effects.pasteSettings", "Paste Effect Settings", r#"{"clip":id,"index":usize}"#, can_paste, paste_settings),
    ]
}

fn can_paste(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.state.effect_clipboard.is_none() { Err("copy an effect first".into()) } else { Ok(()) }
}

/// The effects pasted as they would be on `to`: keyframes keep their offset from the clip start.
fn placed(copied: &CopiedEffects, to: &TrackItem) -> Vec<EffectInstance> {
    copied
        .effects
        .iter()
        .map(|e| {
            let mut e = e.clone();
            crate::clip_ops::retime_effect(&mut e, &copied.source, to, false);
            e
        })
        .collect()
}

fn is_fixed(e: &EffectInstance) -> bool {
    e.def().is_some_and(|d| d.intrinsic)
}

/// `effects.copy`: `indices` of `clip`'s effects, or the effect selected in Effect Controls.
fn copy(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.copy";
    let sel = s.state.selected_effect.clone();
    let clip = clip_p(p, "clip").or(sel.as_ref().map(|e| e.clip)).ok_or_else(|| bad(CMD, "select an effect, or pass `clip` and `indices`"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (tid, it) = q.find_item(clip).ok_or_else(|| bad(CMD, "no such clip"))?;
    let kind = q.track(tid).map(|t| t.kind).unwrap_or_default();
    let indices: Vec<usize> = match p.get("indices").filter(|v| !v.is_null()) {
        Some(v) => {
            let a = v.as_array().ok_or_else(|| bad(CMD, "`indices` is a list of effect indices"))?;
            let mut out = Vec::with_capacity(a.len().min(it.effects.len()));
            for i in a {
                let i = i.as_u64().and_then(|i| usize::try_from(i).ok()).filter(|i| *i < it.effects.len()).ok_or_else(|| bad(CMD, "no such effect on clip"))?;
                if !out.contains(&i) {
                    out.push(i);
                }
            }
            out
        }
        None => vec![sel.filter(|e| e.clip == clip).and_then(|e| e.index(&it.effects)).ok_or_else(|| bad(CMD, "select an effect, or pass `indices`"))?],
    };
    if indices.is_empty() {
        return Err(bad(CMD, "no effects to copy"));
    }
    let effects: Vec<EffectInstance> = indices.iter().filter_map(|i| it.effects.get(*i).cloned()).collect();
    let names: Vec<String> = effects.iter().map(|e| e.effect.clone()).collect();
    s.state.effect_clipboard = Some(CopiedEffects { kind, source: it.clone(), effects });
    Ok(json!({"effects": names}))
}

/// `effects.paste`: onto `clips` (the selection by default, with its linked partners).
fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.paste";
    let copied = s.state.effect_clipboard.clone().ok_or_else(|| bad(CMD, "copy an effect first"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    // clips named in the call are taken as they are; the selection brings its linked partners
    // every named clip must be a clip id: a malformed one is not silently left out
    if let Some(v) = p.get("clips").filter(|v| !v.is_null())
        && !v.as_array().is_some_and(|a| a.iter().all(|c| c.as_u64().is_some()))
    {
        return Err(bad(CMD, "`clips` must be a list of clip ids"));
    }
    if p.get("clip").is_some_and(|v| !v.is_null() && v.as_u64().is_none()) {
        return Err(bad(CMD, "`clip` must be a clip id"));
    }
    let named = p.get("clips").is_some_and(|v| !v.is_null()) || p.get("clip").is_some_and(|v| !v.is_null());
    let clips = if named { clips_p(s, p) } else { with_links(s, &s.state.selection) };
    if let Some(missing) = clips.iter().find(|c| q.find_item(**c).is_none()) {
        return Err(bad(CMD, format!("no clip {}", missing.0)));
    }
    let targets: Vec<ClipId> = clips.into_iter().filter(|c| q.find_item(*c).and_then(|(tid, _)| q.track(tid)).is_some_and(|t| t.kind == copied.kind)).collect();
    if targets.is_empty() {
        return Err(bad(CMD, "select clips of the kind the effects were copied from (video or audio)"));
    }
    let n = s.edit_sequence("Paste Effects", |q, _, _| {
        let mut n = 0;
        for c in &targets {
            let Some((_, dst)) = q.find_item_mut(*c) else { continue };
            for e in placed(&copied, dst) {
                match dst.effects.iter_mut().find(|x| is_fixed(&e) && x.effect == e.effect) {
                    Some(slot) => *slot = e,
                    None => dst.effects.push(e),
                }
            }
            n += 1;
        }
        Ok(n)
    })?;
    Ok(json!({"clips": n}))
}

/// `effects.pasteSettings`: `clip`'s effect `index` takes the settings (and keyframes) of the copied
/// effect of the same kind.
fn paste_settings(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.pasteSettings";
    let copied = s.state.effect_clipboard.clone().ok_or_else(|| bad(CMD, "copy an effect first"))?;
    let clip = clip_p(p, "clip").ok_or_else(|| bad(CMD, "need `clip`"))?;
    let index = p.get("index").and_then(Value::as_u64).and_then(|i| usize::try_from(i).ok()).ok_or_else(|| bad(CMD, "need `index`"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = q.find_item(clip).ok_or_else(|| bad(CMD, "no such clip"))?;
    let target = it.effects.get(index).ok_or_else(|| bad(CMD, "no such effect on clip"))?;
    let pos = copied.effects.iter().position(|e| e.effect == target.effect).ok_or_else(|| bad(CMD, format!("the copied effects have no {}", target.effect)))?;
    s.edit_sequence("Paste Effect Settings", |q, _, _| {
        let (_, dst) = q.find_item_mut(clip).ok_or_else(|| bad(CMD, "no such clip"))?;
        let Some(mut e) = placed(&copied, dst).into_iter().nth(pos) else { return Err(bad(CMD, "no such effect")) };
        let slot = dst.effects.get_mut(index).ok_or_else(|| bad(CMD, "no such effect on clip"))?;
        // the effect stays where it is in the stack, on or off as it was
        e.enabled = slot.enabled;
        *slot = e;
        Ok(())
    })?;
    Ok(Value::Null)
}

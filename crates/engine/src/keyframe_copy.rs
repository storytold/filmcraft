//! Copy and paste of selected keyframe points (#3562, #3319, #2582): `keyframes.copy`,
//! `keyframes.cut` and `keyframes.paste`. A copied point keeps its value, interpolation and
//! ease, with its time relative to the earliest copied point; pasting writes the points back
//! onto the same clip / effect / parameter at the playhead (or a given media time), shifted so
//! the earliest lands there. One paste is one undo step.
//!
//! The keyframe clipboard is mutually exclusive with the clip and caption clipboards (as those
//! two are with each other), so Edit ▸ Paste places whichever was copied last.

use filmcraft_project::{ClipId, Interpolation, ParamValue};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::bad;
use crate::{EngineError, Result, Session};

/// One copied keyframe point: where it goes back and what it carries.
#[derive(Clone, Debug)]
pub struct KeyframeCopy {
    pub clip: ClipId,
    /// Effect index within the clip (as in the UI's `KeyframeRef`).
    pub effect: usize,
    pub param: String,
    pub mask: Option<usize>,
    /// Time relative to the earliest copied point.
    pub dt: Tick,
    pub value: ParamValue,
    pub interp: Interpolation,
    pub in_influence: f64,
    pub out_influence: f64,
}

/// One requested point: clip, effect (index or name), parameter, mask and media time.
struct KeyRef {
    clip: ClipId,
    effect: Value,
    param: String,
    mask: Option<usize>,
    time: Tick,
}

fn key_ref(v: &Value, cmd: &str) -> Result<KeyRef> {
    let clip = v
        .get("clip")
        .and_then(Value::as_u64)
        .map(ClipId)
        .ok_or_else(|| bad(cmd, "need `clip`"))?;
    let effect = v.get("effect").cloned().ok_or_else(|| bad(cmd, "need `effect`"))?;
    let param = v.get("param").and_then(Value::as_str).ok_or_else(|| bad(cmd, "need `param`"))?.to_string();
    let mask = v.get("mask").and_then(Value::as_u64).map(|m| m as usize);
    let time = v
        .get("mediaTime")
        .or_else(|| v.get("time"))
        .and_then(Value::as_i64)
        .map(Tick)
        .ok_or_else(|| bad(cmd, "need `mediaTime`"))?;
    Ok(KeyRef { clip, effect, param, mask, time })
}

fn mask_params(mask: Option<usize>) -> Value {
    match mask {
        Some(m) => json!({"mask": m}),
        None => json!({}),
    }
}

/// Read the keyframe each ref names; times are made relative to the earliest one.
fn resolve(s: &Session, keys: &[KeyRef], cmd: &str) -> Result<Vec<KeyframeCopy>> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let min = keys.iter().map(|k| k.time).min().unwrap_or(Tick::ZERO);
    let mut out = Vec::new();
    for k in keys {
        let (_, it) = q.find_item(k.clip).ok_or_else(|| bad(cmd, format!("no clip {}", k.clip.0)))?;
        let e = match &k.effect {
            Value::Number(n) => {
                let idx = n.as_u64().ok_or_else(|| bad(cmd, "`effect`: an index or a name"))?;
                it.effects.get(idx as usize).ok_or_else(|| bad(cmd, "no such effect on clip"))?
            }
            Value::String(sid) => {
                it.effects.iter().find(|e| &e.effect == sid).ok_or_else(|| bad(cmd, "no such effect on clip"))?
            }
            _ => return Err(bad(cmd, "`effect`: an index or a name")),
        };
        let prm = match k.mask {
            Some(mi) => e.masks.get(mi).and_then(|m| m.param(&k.param)),
            None => e.param(&k.param),
        }
        .ok_or_else(|| bad(cmd, format!("no param `{}`", k.param)))?;
        let kf = prm
            .keyframes
            .iter()
            .find(|kf| kf.time == k.time)
            .ok_or_else(|| bad(cmd, format!("no keyframe of `{}` at {}", k.param, k.time.0)))?;
        let effect = match &k.effect {
            Value::Number(n) => n.as_u64().ok_or_else(|| bad(cmd, "`effect`: an index or a name"))? as usize,
            Value::String(sid) => {
                it.effects.iter().position(|e| &e.effect == sid).ok_or_else(|| bad(cmd, "no such effect on clip"))?
            }
            _ => return Err(bad(cmd, "`effect`: an index or a name")),
        };
        out.push(KeyframeCopy {
            clip: k.clip,
            effect,
            param: k.param.clone(),
            mask: k.mask,
            dt: Tick(k.time.0.saturating_sub(min.0)),
            value: kf.value.clone(),
            interp: kf.interp,
            in_influence: kf.in_influence,
            out_influence: kf.out_influence,
        });
    }
    Ok(out)
}

/// `keyframes.copy`: remember the named points (relative to the earliest). Clears the clip and
/// caption clipboards, so Edit ▸ Paste places the keyframes.
pub fn copy(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "keyframes.copy";
    let keys: Vec<KeyRef> = p
        .get("keys")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| key_ref(v, cmd)).collect::<Result<Vec<_>>>())
        .transpose()?
        .ok_or_else(|| bad(cmd, "need `keys`"))?;
    if keys.is_empty() {
        return Err(bad(cmd, "nothing to copy"));
    }
    let entries = resolve(s, &keys, cmd)?;
    s.state.clipboard.clear();
    s.state.clipboard_transitions.clear();
    s.state.clipboard_markers.clear();
    s.state.caption_clipboard.clear();
    let n = entries.len();
    s.state.keyframe_clipboard = entries;
    Ok(json!({"copied": n}))
}

/// `keyframes.cut`: copy, then delete the named points in the same undo step.
pub fn cut(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "keyframes.cut";
    let keys: Vec<KeyRef> = p
        .get("keys")
        .and_then(Value::as_array)
        .map(|a| a.iter().map(|v| key_ref(v, cmd)).collect::<Result<Vec<_>>>())
        .transpose()?
        .ok_or_else(|| bad(cmd, "need `keys`"))?;
    if keys.is_empty() {
        return Err(bad(cmd, "nothing to cut"));
    }
    let entries = resolve(s, &keys, cmd)?;
    // deleting addresses the media times, not the relative ones
    let targets: Vec<(ClipId, usize, String, Option<usize>, Tick)> = keys
        .iter()
        .zip(entries.iter())
        .map(|(k, e)| (e.clip, e.effect, e.param.clone(), e.mask, k.time))
        .collect();
    s.edit_sequence_as("Cut Keyframes", None, |q, _, _| {
        for (clip, effect, param, mask, at) in &targets {
            let (_, it) = q.find_item_mut(*clip).ok_or(filmcraft_edit::EditError::NoItem(*clip))?;
            let e = it
                .effects
                .get_mut(*effect)
                .ok_or_else(|| bad(cmd, "no such effect on clip"))
                .map_err(|e| filmcraft_edit::EditError::Other(e.to_string()))?;
            let prm = crate::masks::target_param(e, &mask_params(*mask), param)
                .ok_or_else(|| filmcraft_edit::EditError::Other(bad(cmd, format!("no param `{param}`")).to_string()))?;
            prm.remove_keyframe_at(*at);
        }
        Ok(())
    })?;
    s.state.clipboard.clear();
    s.state.clipboard_transitions.clear();
    s.state.clipboard_markers.clear();
    s.state.caption_clipboard.clear();
    let n = entries.len();
    s.state.keyframe_clipboard = entries;
    Ok(json!({"cut": n}))
}

/// `keyframes.paste`: write the clipboard back onto the same clip / effect / parameter, shifted
/// so the earliest point lands at `time` (a media time) or at the playhead. One undo step.
pub fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "keyframes.paste";
    if s.state.keyframe_clipboard.is_empty() {
        return Err(bad(cmd, "copy keyframes first"));
    }
    let at = p.get("time").or_else(|| p.get("mediaTime")).and_then(Value::as_i64).map(Tick);
    let entries = s.state.keyframe_clipboard.clone();
    let ph = s.playhead();
    s.edit_sequence_as("Paste Keyframes", None, |q, _, _| {
        for e in &entries {
            let (_, it) = q.find_item_mut(e.clip).ok_or(filmcraft_edit::EditError::NoItem(e.clip))?;
            let base = match at {
                Some(t) => t,
                None => it.source_time_at(ph.clamp(it.start, (it.end() - Tick(1)).max(it.start))),
            };
            let (mlo, mhi) = (it.source_in, (it.source_out() - Tick(1)).max(it.source_in));
            let target = (base + e.dt).clamp(mlo, mhi);
            let eff = it.effects.get_mut(e.effect).ok_or_else(|| {
                filmcraft_edit::EditError::Other(bad(cmd, "no such effect on clip").to_string())
            })?;
            let prm = crate::masks::target_param(eff, &mask_params(e.mask), &e.param).ok_or_else(|| {
                filmcraft_edit::EditError::Other(bad(cmd, format!("no param `{}`", e.param)).to_string())
            })?;
            prm.put_keyframe(target, e.value.clone());
            if let Some(k) = prm.keyframes.iter_mut().find(|k| k.time == target) {
                k.interp = e.interp;
                k.in_influence = e.in_influence;
                k.out_influence = e.out_influence;
            }
        }
        Ok(())
    })?;
    Ok(json!({"pasted": entries.len()}))
}



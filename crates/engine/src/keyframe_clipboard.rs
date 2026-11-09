//! Copy and paste of keyframes (#3562): the selected points of clip parameters (the Volume line,
//! Effect Controls lanes) and of track automation lanes go on a keyframe clipboard with their
//! values, interpolation and spacing. Paste puts the first one at the playhead (or `time`), each on
//! the same parameter of the target clip or track, as in Premiere.
use std::collections::BTreeMap;

use filmcraft_project::{ClipId, Keyframe, Param, TrackId};
use filmcraft_time::Tick;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::{Result, Session, commands::bad};

const COPY: &str = "edit.copyKeyframes";
const PASTE: &str = "edit.paste";
const MAX_KEYS: usize = 4096;

/// The parameter a copied keyframe belongs to, by what it is rather than where it was, so it can
/// land on the same parameter of another clip or track.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum KeyframeLane {
    /// A clip effect parameter: the effect id, which of the clip's effects with that id, the mask
    /// (for a mask parameter) and the parameter id.
    Clip { effect: String, nth: usize, mask: Option<usize>, param: String },
    /// A track automation lane (`volume`, `pan`, `send.0.level`, `fx.0.<param>`…).
    Track { lane: String },
}

/// Keyframes of one lane on the clipboard: where they came from (a clip or a strip id) and the
/// keys, each `time` the timeline distance from the earliest copied key.
#[derive(Clone, Debug, PartialEq)]
pub struct CopiedKeyframes {
    pub lane: KeyframeLane,
    pub source: u64,
    pub keys: Vec<Keyframe>,
}

/// A keyframe named by `edit.copyKeyframes`: on a clip parameter (media time, as Effect Controls
/// names it) or on a track lane (sequence time).
#[derive(Deserialize)]
#[serde(untagged)]
enum KeyRef {
    Clip {
        clip: u64,
        effect: usize,
        param: String,
        #[serde(default)]
        mask: Option<usize>,
        time: Tick,
    },
    Track {
        strip: u64,
        lane: String,
        time: Tick,
    },
}

/// The parameter behind a clip lane on `it`, if the clip has it.
fn clip_param_mut<'a>(it: &'a mut filmcraft_project::TrackItem, effect: &str, nth: usize, mask: Option<usize>, param: &str) -> Option<&'a mut Param> {
    let e = it.effects.iter_mut().filter(|e| e.effect == effect).nth(nth)?;
    match mask {
        Some(m) => e.masks.get_mut(m)?.param_mut(param),
        None => e.params.get_mut(param),
    }
}

/// `edit.copyKeyframes`: put the named keyframes on the keyframe clipboard (emptying the clip and
/// caption clipboards, so Paste places what was copied last). Returns how many were copied.
/// References to keyframes that no longer exist are skipped; none existing is an error.
pub(crate) fn copy(s: &mut Session, p: &Value) -> Result<Value> {
    let raw = p.get("keyframes").and_then(Value::as_array).ok_or_else(|| bad(COPY, "need keyframes"))?;
    if raw.is_empty() || raw.len() > MAX_KEYS {
        return Err(bad(COPY, "keyframe count must be between 1 and 4096"));
    }
    let refs: Vec<KeyRef> = serde_json::from_value(Value::Array(raw.clone())).map_err(|_| bad(COPY, "invalid keyframe reference"))?;
    let q = s.active_sequence().ok_or(crate::EngineError::NoSequence)?;
    // (lane, source) -> keys at their timeline time
    let mut lanes: BTreeMap<(KeyframeLane, u64), Vec<Keyframe>> = BTreeMap::new();
    // a reference to a keyframe that is gone (deleted, undone) is skipped, as a stale selection
    let found = |r: KeyRef| -> Option<(KeyframeLane, u64, Keyframe)> {
        Some(match r {
            KeyRef::Clip { clip, effect, param, mask, time } => {
                let (_, it) = q.find_item(ClipId(clip))?;
                let e = it.effects.get(effect)?;
                let par = match mask {
                    Some(m) => e.masks.get(m)?.param(&param)?,
                    None => e.params.get(&param)?,
                };
                let mut k = par.keyframes.iter().find(|k| k.time == time)?.clone();
                k.time = it.timeline_time(k.time);
                let nth = it.effects.iter().take(effect).filter(|o| o.effect == e.effect).count();
                (KeyframeLane::Clip { effect: e.effect.clone(), nth, mask, param }, clip, k)
            }
            KeyRef::Track { strip, lane, time } => {
                let k = q.strip(TrackId(strip))?.lane_keyframes(&lane).iter().find(|k| k.time == time)?.clone();
                (KeyframeLane::Track { lane }, strip, k)
            }
        })
    };
    for (lane, source, key) in refs.into_iter().filter_map(found) {
        let keys = lanes.entry((lane, source)).or_default();
        if keys.iter().any(|k| k.time == key.time) {
            continue;
        }
        keys.push(key);
    }
    let first = lanes.values().flatten().map(|k| k.time).min().ok_or_else(|| bad(COPY, "none of those keyframes exists"))?;
    let mut copied = Vec::with_capacity(lanes.len());
    let mut n = 0;
    for ((lane, source), mut keys) in lanes {
        for k in &mut keys {
            k.time = Tick(k.time.0.saturating_sub(first.0));
        }
        keys.sort_by_key(|k| k.time);
        n += keys.len();
        copied.push(CopiedKeyframes { lane, source, keys });
    }
    let st = &mut s.state;
    st.clipboard.clear();
    st.clipboard_markers.clear();
    st.clipboard_transitions.clear();
    st.caption_clipboard.clear();
    st.keyframe_clipboard = copied;
    Ok(json!({"copied": n}))
}

/// Paste the keyframe clipboard: the first key at `time` (default: the playhead), the others at
/// their copied distance from it, each on the same parameter. Clip parameters go to `clip`, else
/// the first selected clip that has the parameter, else the clip they came from; track lanes go to
/// `strip`, else the track they came from. One undo step; returns how many keys were pasted.
pub(crate) fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    let at = crate::commands::time_p(s, p, "").unwrap_or(s.playhead());
    let clip = crate::commands::u64_p(p, "clip");
    let strip = if p.get("strip").is_some() { Some(crate::mixer::strip_p(s, p, PASTE)?) } else { None };
    let selection = s.state.selection.clone();
    let lanes = s.state.keyframe_clipboard.clone();
    let n = s.edit_sequence("Paste Keyframes", |q, _, _| {
        let mut n = 0;
        for c in &lanes {
            let place = |k: &Keyframe| Tick(at.0.saturating_add(k.time.0));
            match &c.lane {
                KeyframeLane::Clip { effect, nth, mask, param } => {
                    let has = |q: &filmcraft_project::Sequence, id: ClipId| {
                        q.find_item(id).is_some_and(|(_, it)| it.effects.iter().filter(|e| &e.effect == effect).nth(*nth).is_some())
                    };
                    let target = match clip {
                        Some(id) => ClipId(id),
                        None => selection.iter().copied().find(|id| has(&*q, *id)).unwrap_or(ClipId(c.source)),
                    };
                    let Some((_, it)) = q.find_item_mut(target) else { continue };
                    let times: Vec<Tick> = c.keys.iter().map(|k| it.moving_source_time_at(place(k))).collect();
                    let Some(par) = clip_param_mut(it, effect, *nth, *mask, param) else { continue };
                    n += put(par, &c.keys, &times);
                }
                KeyframeLane::Track { lane } => {
                    let times: Vec<Tick> = c.keys.iter().map(place).collect();
                    n += q
                        .with_strip_mut(strip.unwrap_or(TrackId(c.source)), |tr| {
                            // the same lanes the mixer commands accept, so pasted keys stay editable
                            if crate::mixer::lane_refusal(tr, lane).is_some() {
                                return 0;
                            }
                            tr.lane_mut(lane).map_or(0, |par| put(par, &c.keys, &times))
                        })
                        .unwrap_or(0);
                }
            }
        }
        if n == 0 {
            return Err(bad(PASTE, "the target has none of the copied keyframes' parameters"));
        }
        Ok(n)
    })?;
    Ok(json!({"pasted": n}))
}

/// Add `keys` to `par` at `times` (replacing keys already there); returns how many were added.
fn put(par: &mut Param, keys: &[Keyframe], times: &[Tick]) -> usize {
    for (k, &t) in keys.iter().zip(times) {
        let k = Keyframe { time: t, ..k.clone() };
        match par.keyframes.binary_search_by_key(&t, |x| x.time) {
            Ok(i) => {
                if let Some(slot) = par.keyframes.get_mut(i) {
                    *slot = k;
                }
            }
            Err(i) => par.keyframes.insert(i, k),
        }
    }
    keys.len().min(times.len())
}

//! Atomic Effect Controls operations that work on a set of effects or keyframes.

use std::collections::{BTreeMap, HashSet};

use filmcraft_project::{ClipId, EffectInstance, EffectKind, Interpolation, Keyframe, Param, ParamKind, ParamValue, Sequence, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{CommandSpec, EngineError, Result, Session, commands::bad};

const MAX_EFFECTS: usize = 64;
const MAX_TARGETS: usize = 256;
const MAX_KEYS: usize = 4096;
const MAX_MASKS: usize = 64;
const MAX_COPIED_KEYS: usize = 100_000;
const MAX_PATH_VERTICES: usize = 16_384;
const MAX_PASTE_INSTANCES: usize = 4096;
const MAX_PASTE_KEYS: usize = 200_000;
const MAX_PASTE_VERTICES: usize = 65_536;

type Run = fn(&mut Session, &Value) -> Result<Value>;

/// One Effect Controls command spec: no menu entry or shortcut, enabled while a sequence is open,
/// journaled, dispatched to `run`.
fn spec(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: crate::commands::has_seq, run, journal: true }
}

/// The `effects.*` commands this module adds. Each one edits a whole set of effects or keyframes,
/// so one call is one undo step.
pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("effects.removeMany", "Remove Effects", r#"{"clip":id,"indices":[index]}"#, remove_effects),
        spec(
            "effects.deleteKeyframes",
            "Delete Keyframes",
            r#"{"keyframes":[{"clip":id,"effect":index,"param":str,"mask":n?,"time":ticks}]}"#,
            delete_keyframes,
        ),
        spec(
            "effects.setInterpolations",
            "Set Keyframe Interpolation",
            r#"{"keyframes":[{"clip":id,"effect":index,"param":str,"mask":n?,"time":ticks}],"interpolation":"linear|bezier|autoBezier|continuousBezier|hold|easeIn|easeOut"}"#,
            set_interpolations,
        ),
        spec("effects.move", "Reorder Effects", r#"{"clip":id,"from":index,"to":index}"#, move_effect),
        spec("effects.paste", "Paste Effects", r#"{"sourceIn":ticks,"effects":[EffectInstance],"clips":[id]}"#, paste_effects),
        spec(
            "effects.duplicateKeyframes",
            "Duplicate Keyframes",
            r#"{"keyframes":[{"clip":id,"effect":index,"param":str,"mask":n?,"time":ticks}],"delta":timelineTicks}"#,
            duplicate_keyframes,
        ),
        spec("effects.pasteKeyframes", "Paste Keyframes", r#"{"clip":id,"anchor":timelineTicks,"keyframes":[...],"effects":[...]}"#, paste_keyframes),
    ]
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
struct KeyRef {
    clip: u64,
    effect: usize,
    param: String,
    #[serde(default)]
    mask: Option<usize>,
    time: Tick,
}

/// The parameter a keyframe reference names: `mask` picks a mask parameter, `None` an effect
/// parameter.
fn key_param_mut<'a>(sequence: &'a mut Sequence, k: &KeyRef) -> Result<&'a mut Param> {
    let (_, item) = sequence.find_item_mut(ClipId(k.clip)).ok_or_else(|| bad("effects.keyframes", "clip does not exist"))?;
    let effect = item.effects.get_mut(k.effect).ok_or_else(|| bad("effects.keyframes", "effect does not exist"))?;
    match k.mask {
        Some(mask) => effect.masks.get_mut(mask).and_then(|m| m.param_mut(&k.param)),
        None => effect.params.get_mut(&k.param),
    }
    .ok_or_else(|| bad("effects.keyframes", "parameter does not exist"))
}

/// The interpolation a command parameter names (case-insensitive), or `None` for an unknown one.
fn interpolation(s: &str) -> Option<Interpolation> {
    match s.to_ascii_lowercase().as_str() {
        "linear" => Some(Interpolation::Linear),
        "bezier" => Some(Interpolation::Bezier),
        "autobezier" => Some(Interpolation::AutoBezier),
        "continuousbezier" => Some(Interpolation::ContinuousBezier),
        "hold" => Some(Interpolation::Hold),
        "easein" => Some(Interpolation::EaseIn),
        "easeout" => Some(Interpolation::EaseOut),
        _ => None,
    }
}

/// `effects.removeMany`: remove the given effect indices from one clip — duplicates, intrinsic
/// effects and out-of-range indices are refused, and the panel's selection follows.
fn remove_effects(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.removeMany";
    let clip = p.get("clip").and_then(Value::as_u64).map(ClipId).ok_or_else(|| bad(CMD, "need `clip`"))?;
    let raw = p.get("indices").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need `indices`"))?;
    if raw.is_empty() || raw.len() > MAX_EFFECTS {
        return Err(bad(CMD, format!("effect count must be between 1 and {MAX_EFFECTS}")));
    }
    let mut indices = Vec::with_capacity(raw.len());
    let mut unique = HashSet::with_capacity(raw.len());
    for value in raw {
        let index = value.as_u64().and_then(|value| usize::try_from(value).ok()).ok_or_else(|| bad(CMD, "effect indices must be non-negative integers"))?;
        if !unique.insert(index) {
            return Err(bad(CMD, "duplicate effect index"));
        }
        indices.push(index);
    }
    indices.sort_unstable();
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, item) = q.find_item(clip).ok_or_else(|| bad(CMD, "clip does not exist"))?;
    if indices.iter().any(|index| item.effects.get(*index).is_none_or(|effect| effect.def().is_none_or(|definition| definition.intrinsic))) {
        return Err(bad(CMD, "an effect does not exist or is intrinsic"));
    }
    let selected_effect = s.state.selected_effect.clone().filter(|selected| selected.clip == clip);
    let selected_index = selected_effect.as_ref().and_then(|selected| selected.index(&item.effects));
    s.edit_sequence("Remove Effects", |sequence, _, _| {
        let (_, item) = sequence.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        for index in indices.iter().rev() {
            let effect = item.effects.get(*index).ok_or_else(|| bad(CMD, "effect order changed before removal"))?;
            if effect.def().is_none_or(|definition| definition.intrinsic) {
                return Err(bad(CMD, "intrinsic effects cannot be removed"));
            }
            item.effects.remove(*index);
        }
        Ok(())
    })?;
    if let Some(selected_index) = selected_index
        && indices.binary_search(&selected_index).is_ok()
    {
        s.state.selected_effect = None;
    }
    Ok(json!({"removed": indices.len()}))
}

/// `effects.deleteKeyframes`: remove the named keyframes, validating every reference before the
/// edit so a stale one changes nothing.
fn delete_keyframes(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.deleteKeyframes";
    let raw = p.get("keyframes").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need `keyframes`"))?;
    if raw.is_empty() || raw.len() > MAX_KEYS {
        return Err(bad(CMD, format!("keyframe count must be between 1 and {MAX_KEYS}")));
    }
    let keys: Vec<KeyRef> = serde_json::from_value(Value::Array(raw.clone())).map_err(|_| bad(CMD, "invalid keyframe reference"))?;
    let mut unique = HashSet::with_capacity(keys.len());
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    for key in &keys {
        if key.param.is_empty() || key.param.len() > 256 || key.mask.is_some_and(|mask| mask >= MAX_MASKS) || !unique.insert(key.clone()) {
            return Err(bad(CMD, "duplicate or invalid keyframe reference"));
        }
        let (_, item) = q.find_item(ClipId(key.clip)).ok_or_else(|| bad(CMD, "clip does not exist"))?;
        let effect = item.effects.get(key.effect).ok_or_else(|| bad(CMD, "effect does not exist"))?;
        let param = match key.mask {
            Some(mask) => effect.masks.get(mask).and_then(|mask| mask.param(&key.param)),
            None => effect.params.get(&key.param),
        }
        .ok_or_else(|| bad(CMD, "parameter does not exist"))?;
        if !param.keyframes.iter().any(|keyframe| keyframe.time == key.time) {
            return Err(bad(CMD, "keyframe does not exist"));
        }
    }
    s.edit_sequence("Delete Keyframes", |sequence, _, _| {
        for key in &keys {
            let param = key_param_mut(sequence, key)?;
            let Some(index) = param.keyframes.iter().position(|keyframe| keyframe.time == key.time) else {
                return Err(bad(CMD, "keyframe disappeared before removal"));
            };
            param.keyframes.remove(index);
        }
        Ok(())
    })?;
    Ok(json!({"deleted": keys.len()}))
}

/// `effects.setInterpolations`: set the interpolation of every named keyframe as one edit.
fn set_interpolations(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.setInterpolations";
    let raw = p.get("keyframes").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need `keyframes`"))?;
    if raw.is_empty() || raw.len() > MAX_KEYS {
        return Err(bad(CMD, format!("keyframe count must be between 1 and {MAX_KEYS}")));
    }
    let kind = p.get("interpolation").and_then(Value::as_str).and_then(interpolation).ok_or_else(|| bad(CMD, "unknown interpolation"))?;
    let keys: Vec<KeyRef> = serde_json::from_value(Value::Array(raw.clone())).map_err(|_| bad(CMD, "invalid keyframe reference"))?;
    let mut unique = HashSet::with_capacity(keys.len());
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    for key in &keys {
        if key.param.is_empty() || key.param.len() > 256 || !unique.insert(key.clone()) {
            return Err(bad(CMD, "duplicate or invalid keyframe reference"));
        }
        let (_, it) = q.find_item(ClipId(key.clip)).ok_or_else(|| bad(CMD, "clip does not exist"))?;
        let effect = it.effects.get(key.effect).ok_or_else(|| bad(CMD, "effect does not exist"))?;
        let param = match key.mask {
            Some(m) => effect.masks.get(m).and_then(|m| m.param(&key.param)),
            None => effect.params.get(&key.param),
        }
        .ok_or_else(|| bad(CMD, "parameter does not exist"))?;
        if !param.keyframes.iter().any(|kf| kf.time == key.time) {
            return Err(bad(CMD, "keyframe does not exist"));
        }
    }
    s.edit_sequence("Keyframe Interpolation", |sequence, _, _| {
        for key in &keys {
            let param = key_param_mut(sequence, key)?;
            let kf = param.keyframes.iter_mut().find(|kf| kf.time == key.time).ok_or_else(|| bad(CMD, "keyframe disappeared"))?;
            kf.interp = kind;
        }
        Ok(())
    })?;
    Ok(json!({"changed": keys.len()}))
}

/// Whether a chain keeps its required order: intrinsic effects last on a video track, first on an
/// audio track.
fn ordered_effects(items: &[EffectInstance], kind: TrackKind) -> bool {
    let mut intrinsic_seen = false;
    let mut standard_seen = false;
    for effect in items {
        let intrinsic = effect.def().is_some_and(|def| def.intrinsic);
        if kind == TrackKind::Video {
            if !intrinsic && intrinsic_seen {
                return false;
            }
            intrinsic_seen |= intrinsic;
        } else {
            // Audio chain order is intrinsic controls, followed by insert effects.
            if intrinsic && standard_seen {
                return false;
            }
            standard_seen |= !intrinsic;
        }
    }
    true
}

/// `effects.move`: move an effect within one clip's chain, never across the intrinsic boundary.
fn move_effect(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.move";
    let clip = p.get("clip").and_then(Value::as_u64).map(ClipId).ok_or_else(|| bad(CMD, "need `clip`"))?;
    let from = p.get("from").and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()).ok_or_else(|| bad(CMD, "need `from` index"))?;
    let to = p.get("to").and_then(Value::as_u64).and_then(|x| usize::try_from(x).ok()).ok_or_else(|| bad(CMD, "need `to` index"))?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track_id, item) = q.find_item(clip).ok_or_else(|| bad(CMD, "clip does not exist"))?;
    let track_kind = q.track(track_id).map(|t| t.kind).ok_or_else(|| bad(CMD, "track does not exist"))?;
    if from >= item.effects.len() || to >= item.effects.len() {
        return Err(bad(CMD, "effect index is out of range"));
    }
    if item.effects.len() > 512 {
        return Err(bad(CMD, "clip has too many effects to reorder safely"));
    }
    if from == to {
        return Ok(json!({"from": from, "to": to}));
    }
    if item.effects[from].def().is_none_or(|d| d.intrinsic) {
        return Err(bad(CMD, "intrinsic effects cannot be moved"));
    }
    let mut reordered = item.effects.clone();
    let picked = reordered.remove(from);
    reordered.insert(to, picked);
    if !ordered_effects(&reordered, track_kind) {
        return Err(bad(CMD, "effect cannot cross the intrinsic/standard effect boundary"));
    }
    s.edit_sequence("Reorder Effects", |sequence, _, _| {
        let (_, item) = sequence.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        if from >= item.effects.len() || to >= item.effects.len() || item.effects[from].def().is_none_or(|d| d.intrinsic) {
            return Err(bad(CMD, "effect order changed before the move"));
        }
        let picked = item.effects.remove(from);
        item.effects.insert(to, picked);
        Ok(())
    })?;
    Ok(json!({"from": from, "to": to}))
}

/// Whether a value is finite and within its size caps, so a clipboard cannot hand the renderer a
/// NaN or an unbounded path.
fn finite_value(v: &ParamValue) -> bool {
    match v {
        ParamValue::Float(x) => x.is_finite(),
        ParamValue::Vec2(p) => p.x.is_finite() && p.y.is_finite(),
        ParamValue::Color(c) => c.iter().all(|x| x.is_finite()),
        ParamValue::Path(path) => {
            path.vertices.len() <= 8192
                && path.vertices.iter().all(|vertex| {
                    vertex.p.x.is_finite()
                        && vertex.p.y.is_finite()
                        && vertex.t_in.x.is_finite()
                        && vertex.t_in.y.is_finite()
                        && vertex.t_out.x.is_finite()
                        && vertex.t_out.y.is_finite()
                })
        }
        ParamValue::Text(text) => text.len() <= 1_048_576,
        _ => true,
    }
}

/// The path vertices a value holds; every other kind holds none.
fn path_vertices(v: &ParamValue) -> usize {
    match v {
        ParamValue::Path(path) => path.vertices.len(),
        _ => 0,
    }
}

/// The keyframe and path-vertex counts of an effect, saturating — the paste commands use them to
/// bound what a clipboard would add to the project.
fn snapshot_counts(effect: &EffectInstance) -> (usize, usize) {
    let mut keys = 0usize;
    let mut vertices = 0usize;
    for param in
        effect.params.values().chain(effect.masks.iter().flat_map(|mask| filmcraft_project::mask::MASK_PARAMS.into_iter().filter_map(|id| mask.param(id))))
    {
        keys = keys.saturating_add(param.keyframes.len());
        vertices = vertices.saturating_add(path_vertices(&param.value));
        for keyframe in &param.keyframes {
            vertices = vertices.saturating_add(path_vertices(&keyframe.value));
        }
    }
    (keys, vertices)
}

/// Reject an effect that arrived in a clipboard and this app cannot apply: unknown definitions,
/// transitions, layers, essential or intrinsic instances (unless `allow_intrinsic`), oversized
/// parameter sets, non-finite values and invalid influences.
fn validate_effect(effect: &EffectInstance, command: &str, allow_intrinsic: bool) -> Result<()> {
    let def = effect.def().ok_or_else(|| bad(command, "unknown effect in clipboard"))?;
    if (def.intrinsic && !allow_intrinsic)
        || matches!(def.kind, EffectKind::AudioTransition | EffectKind::VideoTransition)
        || effect.layer.is_some()
        || effect.essential
        || effect.masks.len() > MAX_MASKS
        || effect.params.len() > 256
    {
        return Err(bad(command, "clipboard contains an unsupported effect"));
    }
    if effect.params.iter().any(|(id, param)| {
        (!allow_intrinsic && !def.params.iter().any(|p| p.id == id))
            || (!allow_intrinsic && !finite_value(&param.value))
            || param.keyframes.len() > 100_000
            || param.keyframes.iter().any(|kf| (!allow_intrinsic && !finite_value(&kf.value)) || !kf.influence_is_valid())
    }) {
        return Err(bad(command, "effect parameters are invalid"));
    }
    for mask in &effect.masks {
        for id in filmcraft_project::mask::MASK_PARAMS {
            if mask.param(id).is_none_or(|param| {
                !finite_value(&param.value)
                    || param.keyframes.len() > 100_000
                    || param.keyframes.iter().any(|kf| !finite_value(&kf.value) || !kf.influence_is_valid())
            }) {
                return Err(bad(command, "effect mask is invalid"));
            }
        }
    }
    Ok(())
}

trait ValidInfluence {
    /// Whether both influence handles are finite.
    fn influence_is_valid(&self) -> bool;
}
impl ValidInfluence for Keyframe {
    /// Whether both influence handles are finite.
    fn influence_is_valid(&self) -> bool {
        self.in_influence.is_finite() && self.out_influence.is_finite()
    }
}

/// Retime every keyframe of an effect by `delta`, refusing overflow and collisions.
fn shift_effect(effect: &mut EffectInstance, delta: Tick) -> Result<()> {
    for param in effect.params.values_mut().chain(effect.masks.iter_mut().flat_map(|m| m.params_mut())) {
        for key in &mut param.keyframes {
            key.time = Tick(key.time.0.checked_add(delta.0).ok_or_else(|| bad("effects.paste", "retimed keyframe overflows"))?);
        }
        param.keyframes.sort_by_key(|key| key.time);
        if param.keyframes.windows(2).any(|w| w[0].time == w[1].time) {
            return Err(bad("effects.paste", "retiming causes colliding keyframes"));
        }
    }
    Ok(())
}

/// `effects.paste`: apply a clipboard of effects to each target clip, retiming their keyframes to
/// the target's source time.
fn paste_effects(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.paste";
    let source_in = p.get("sourceIn").and_then(Value::as_i64).map(Tick).ok_or_else(|| bad(CMD, "need sourceIn"))?;
    let raw_clips = p.get("clips").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need clips"))?;
    let raw_effects = p.get("effects").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need effects"))?;
    if raw_clips.is_empty() || raw_clips.len() > MAX_TARGETS || raw_effects.is_empty() || raw_effects.len() > MAX_EFFECTS {
        return Err(bad(CMD, "clipboard or target count is empty or too large"));
    }
    let clips: Vec<ClipId> = raw_clips.iter().map(|v| v.as_u64().map(ClipId).ok_or_else(|| bad(CMD, "invalid clip id"))).collect::<Result<_>>()?;
    let mut unique = HashSet::with_capacity(clips.len());
    if clips.iter().any(|clip| !unique.insert(*clip)) {
        return Err(bad(CMD, "duplicate target clip"));
    }
    let effects: Vec<EffectInstance> = serde_json::from_value(Value::Array(raw_effects.clone())).map_err(|_| bad(CMD, "invalid effect clipboard"))?;
    for effect in &effects {
        validate_effect(effect, CMD, false)?;
    }
    let (mut total_keys, mut total_vertices) = (0usize, 0usize);
    for effect in &effects {
        let (keys, vertices) = snapshot_counts(effect);
        total_keys = total_keys.checked_add(keys).ok_or_else(|| bad(CMD, "clipboard is too large"))?;
        total_vertices = total_vertices.checked_add(vertices).ok_or_else(|| bad(CMD, "clipboard is too large"))?;
    }
    if total_keys > MAX_COPIED_KEYS || total_vertices > MAX_PATH_VERTICES {
        return Err(bad(CMD, "clipboard contains too much keyframe or path data"));
    }
    if effects.len().checked_mul(clips.len()).is_none_or(|count| count > MAX_PASTE_INSTANCES)
        || total_keys.checked_mul(clips.len()).is_none_or(|count| count > MAX_PASTE_KEYS)
        || total_vertices.checked_mul(clips.len()).is_none_or(|count| count > MAX_PASTE_VERTICES)
    {
        return Err(bad(CMD, "paste request would create too much project data"));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let source_clip = p.get("sourceClip").and_then(Value::as_u64).map(ClipId).ok_or_else(|| bad(CMD, "need sourceClip"))?;
    let (_, source_item) = q.find_item(source_clip).ok_or_else(|| bad(CMD, "source clip no longer exists"))?;
    if source_item.source_in != source_in {
        return Err(bad(CMD, "source In point changed after copying"));
    }
    for clip in &clips {
        let (track, item) = q.find_item(*clip).ok_or_else(|| bad(CMD, "target clip does not exist"))?;
        let kind = q.track(track).map(|t| t.kind).ok_or_else(|| bad(CMD, "target track does not exist"))?;
        if item.effects.len().checked_add(effects.len()).is_none_or(|count| count > 512) {
            return Err(bad(CMD, "target clip would have too many effects"));
        }
        if effects.iter().any(|effect| effect.def().is_some_and(|d| (d.kind == EffectKind::Audio) != (kind == TrackKind::Audio))) {
            return Err(bad(CMD, "effect type does not match every target clip"));
        }
    }
    let mut inserted = 0usize;
    s.edit_sequence("Paste Effects", |sequence, _, _| {
        for clip in &clips {
            let (_, item) = sequence.find_item_mut(*clip).ok_or(filmcraft_edit::EditError::NoItem(*clip))?;
            let delta = if *clip == source_clip {
                Tick::ZERO
            } else {
                Tick(item.source_in.0.checked_sub(source_in.0).ok_or_else(|| bad(CMD, "source-time offset overflows"))?)
            };
            let mut copies = effects.clone();
            if delta != Tick::ZERO {
                for effect in &mut copies {
                    shift_effect(effect, delta)?;
                }
            }
            for effect in copies {
                let audio = effect.def().is_some_and(|d| d.kind == EffectKind::Audio);
                let index = if audio {
                    item.effects.len()
                } else {
                    item.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(item.effects.len())
                };
                item.effects.insert(index, effect);
                inserted += 1;
            }
        }
        Ok(())
    })?;
    Ok(json!({"inserted": inserted}))
}

/// The saved state of every keyframe reference — the keyframe plus the clip speed that positions
/// it — after checking that each one can move on its clip.
fn validated_key_snapshot(q: &Sequence, keys: &[KeyRef], command: &str) -> Result<Vec<(KeyRef, Keyframe, f64)>> {
    if keys.is_empty() || keys.len() > MAX_KEYS {
        return Err(bad(command, format!("keyframe count must be between 1 and {MAX_KEYS}")));
    }
    let mut unique = HashSet::with_capacity(keys.len());
    let mut saved = Vec::with_capacity(keys.len());
    for key in keys {
        if key.param.is_empty() || key.param.len() > 256 || !unique.insert(key.clone()) {
            return Err(bad(command, "duplicate or invalid keyframe reference"));
        }
        let (_, item) = q.find_item(ClipId(key.clip)).ok_or_else(|| bad(command, "clip does not exist"))?;
        let effect = item.effects.get(key.effect).ok_or_else(|| bad(command, "effect does not exist"))?;
        let param = match key.mask {
            Some(mask) => effect.masks.get(mask).and_then(|m| m.param(&key.param)),
            None => effect.params.get(&key.param),
        }
        .ok_or_else(|| bad(command, "parameter does not exist"))?;
        let frame = q.settings.frame_rate.frame_duration();
        let kf = param.keyframes.iter().find(|kf| kf.time == key.time).cloned().ok_or_else(|| bad(command, "keyframe does not exist"))?;
        let speed = item.speed.abs() * if item.reverse { -1.0 } else { 1.0 };
        if !speed.is_finite() || speed == 0.0 || item.duration < frame {
            return Err(bad(command, "clip has no movable keyframe range"));
        }
        let first = item.moving_source_time_at(item.start);
        let last = item.moving_source_time_at(item.end() - frame);
        if key.time < first.min(last) || key.time > first.max(last) {
            return Err(bad(command, "keyframe is outside the visible clip range"));
        }
        saved.push((key.clone(), kf, speed));
    }
    Ok(saved)
}

/// A movement in ticks that fits an `i64`, or an error for a delta that cannot.
fn bounded_delta(value: f64, command: &str) -> Result<i64> {
    if !value.is_finite() || value < i64::MIN as f64 || value >= i64::MAX as f64 {
        return Err(bad(command, "keyframe movement range overflows"));
    }
    Ok(value as i64)
}

/// `effects.duplicateKeyframes`: copy the named keyframes by a timeline delta, clamped to the
/// range every one of them can reach.
fn duplicate_keyframes(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.duplicateKeyframes";
    let raw = p.get("keyframes").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need keyframes"))?;
    if raw.is_empty() || raw.len() > MAX_KEYS {
        return Err(bad(CMD, format!("keyframe count must be between 1 and {MAX_KEYS}")));
    }
    let keys: Vec<KeyRef> = serde_json::from_value(Value::Array(raw.clone())).map_err(|_| bad(CMD, "invalid keyframe reference"))?;
    let requested = p.get("delta").and_then(Value::as_i64).ok_or_else(|| bad(CMD, "need an integer timeline delta"))?;
    if requested == 0 {
        return Err(bad(CMD, "duplicate destination must differ from the source"));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let saved = validated_key_snapshot(q, &keys, CMD)?;
    let frame = q.settings.frame_rate.frame_duration();
    let (mut low, mut high) = (i64::MIN, i64::MAX);
    for (key, _, speed) in &saved {
        let (_, item) = q.find_item(ClipId(key.clip)).ok_or_else(|| bad(CMD, "clip does not exist"))?;
        let a = item.moving_source_time_at(item.start);
        let b = item.moving_source_time_at(item.end() - frame);
        let da = (i128::from(a.0) - i128::from(key.time.0)) as f64 / speed;
        let db = (i128::from(b.0) - i128::from(key.time.0)) as f64 / speed;
        low = low.max(bounded_delta(da.min(db).ceil(), CMD)?);
        high = high.min(bounded_delta(da.max(db).floor(), CMD)?);
    }
    if low > high {
        return Err(bad(CMD, "keyframes have no common duplication range"));
    }
    let delta = requested.clamp(low, high);
    if delta == 0 {
        return Err(bad(CMD, "duplicate destination must differ from the source"));
    }
    let mut copies = Vec::with_capacity(saved.len());
    let mut destinations = HashSet::with_capacity(saved.len());
    for (mut key, mut kf, speed) in saved {
        let shift = (delta as f64 * speed).round();
        if !shift.is_finite() || shift < i64::MIN as f64 || shift >= i64::MAX as f64 {
            return Err(bad(CMD, "keyframe destination overflows"));
        }
        kf.time = Tick(kf.time.0.checked_add(shift as i64).ok_or_else(|| bad(CMD, "keyframe destination overflows"))?);
        key.time = kf.time;
        if !destinations.insert(key.clone()) {
            return Err(bad(CMD, "duplicate keyframes share a destination"));
        }
        let (_, item) = q.find_item(ClipId(key.clip)).ok_or_else(|| bad(CMD, "clip does not exist"))?;
        let effect = item.effects.get(key.effect).ok_or_else(|| bad(CMD, "effect does not exist"))?;
        let param = match key.mask {
            Some(mask) => effect.masks.get(mask).and_then(|m| m.param(&key.param)),
            None => effect.params.get(&key.param),
        }
        .ok_or_else(|| bad(CMD, "parameter does not exist"))?;
        if param.keyframes.iter().any(|existing| existing.time == key.time) {
            return Err(bad(CMD, "a keyframe already exists at the duplicate destination"));
        }
        copies.push((key, kf));
    }
    s.edit_sequence("Duplicate Keyframes", |sequence, _, _| {
        for (key, kf) in &copies {
            let param = key_param_mut(sequence, key)?;
            let at = param.keyframes.partition_point(|existing| existing.time < kf.time);
            param.keyframes.insert(at, kf.clone());
        }
        Ok(())
    })?;
    Ok(json!({"keyframes": copies.into_iter().map(|(key, _)| key).collect::<Vec<_>>(), "delta": delta}))
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct PasteKey {
    effect: String,
    #[serde(default)]
    instance: usize,
    param: String,
    #[serde(default)]
    mask: Option<usize>,
    offset: Tick,
    keyframe: Keyframe,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct EffectTemplate {
    effect: String,
    #[serde(default)]
    instance: usize,
    template: EffectInstance,
}

/// Whether a value holds the variant a parameter kind expects.
fn value_matches_kind(kind: &ParamKind, value: &ParamValue) -> bool {
    matches!(
        (kind, value),
        (ParamKind::Float { .. } | ParamKind::Angle, ParamValue::Float(_))
            | (ParamKind::Point, ParamValue::Vec2(_))
            | (ParamKind::Bool, ParamValue::Bool(_))
            | (ParamKind::Choice(_), ParamValue::Choice(_))
            | (ParamKind::Color, ParamValue::Color(_))
            | (ParamKind::Path, ParamValue::Path(_))
            | (ParamKind::Text, ParamValue::Text(_))
    )
}

/// Whether a value is finite, of the kind's variant, and inside the parameter's own range.
fn value_allowed(kind: &ParamKind, value: &ParamValue) -> bool {
    if !finite_value(value) || !value_matches_kind(kind, value) {
        return false;
    }
    match (kind, value) {
        (ParamKind::Float { min, max, .. }, ParamValue::Float(x)) => x >= min && x <= max,
        (ParamKind::Choice(options), ParamValue::Choice(choice)) => usize::try_from(*choice).is_ok_and(|i| i < options.len()),
        _ => true,
    }
}

/// Whether two values hold the same variant.
fn same_value_type(left: &ParamValue, right: &ParamValue) -> bool {
    matches!(
        (left, right),
        (ParamValue::Float(_), ParamValue::Float(_))
            | (ParamValue::Vec2(_), ParamValue::Vec2(_))
            | (ParamValue::Bool(_), ParamValue::Bool(_))
            | (ParamValue::Choice(_), ParamValue::Choice(_))
            | (ParamValue::Color(_), ParamValue::Color(_))
            | (ParamValue::Path(_), ParamValue::Path(_))
            | (ParamValue::Text(_), ParamValue::Text(_))
    )
}

/// The index of the `occurrence`-th instance of effect `id` on a clip.
fn effect_index(item: &TrackItem, id: &str, occurrence: usize) -> Option<usize> {
    item.effects.iter().enumerate().filter(|(_, effect)| effect.effect == id).nth(occurrence).map(|(index, _)| index)
}

/// `effects.pasteKeyframes`: paste copied keyframes into a clip, placed relative to the playhead,
/// creating an effect or mask the clipboard needs first.
fn paste_keyframes(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.pasteKeyframes";
    let clip = p.get("clip").and_then(Value::as_u64).map(ClipId).ok_or_else(|| bad(CMD, "need target clip"))?;
    let anchor = p.get("anchor").and_then(Value::as_i64).map(Tick).ok_or_else(|| bad(CMD, "need anchor time"))?;
    let raw_keys = p.get("keyframes").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need keyframes"))?;
    let raw_templates = p.get("effects").and_then(Value::as_array).ok_or_else(|| bad(CMD, "need effect templates"))?;
    if raw_keys.is_empty() || raw_keys.len() > MAX_KEYS || raw_templates.is_empty() || raw_templates.len() > MAX_EFFECTS {
        return Err(bad(CMD, "clipboard is empty or too large"));
    }
    let keys: Vec<PasteKey> = serde_json::from_value(Value::Array(raw_keys.clone())).map_err(|_| bad(CMD, "invalid keyframe clipboard"))?;
    let templates: Vec<EffectTemplate> = serde_json::from_value(Value::Array(raw_templates.clone())).map_err(|_| bad(CMD, "invalid effect templates"))?;
    let mut template_map = BTreeMap::new();
    for template in &templates {
        if template.instance >= MAX_EFFECTS
            || template.template.effect != template.effect
            || template_map.insert((template.effect.clone(), template.instance), &template.template).is_some()
        {
            return Err(bad(CMD, "duplicate or mismatched effect template"));
        }
        let def = template.template.def().ok_or_else(|| bad(CMD, "unknown effect template"))?;
        if matches!(def.kind, EffectKind::AudioTransition | EffectKind::VideoTransition) {
            return Err(bad(CMD, "intrinsic effects and transitions cannot be pasted as keyframes"));
        }
        validate_effect(&template.template, CMD, true)?;
    }
    let (mut template_keys, mut template_vertices) = (0usize, 0usize);
    for template in &templates {
        let (keys, vertices) = snapshot_counts(&template.template);
        template_keys = template_keys.checked_add(keys).ok_or_else(|| bad(CMD, "effect templates are too large"))?;
        template_vertices = template_vertices.checked_add(vertices).ok_or_else(|| bad(CMD, "effect templates are too large"))?;
    }
    let key_vertices = keys
        .iter()
        .map(|entry| path_vertices(&entry.keyframe.value))
        .try_fold(0usize, |sum, count| sum.checked_add(count))
        .ok_or_else(|| bad(CMD, "keyframe clipboard is too large"))?;
    if template_keys > MAX_COPIED_KEYS || template_vertices.saturating_add(key_vertices) > MAX_PATH_VERTICES {
        return Err(bad(CMD, "clipboard contains too much keyframe or path data"));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (track, item) = q.find_item(clip).ok_or_else(|| bad(CMD, "target clip does not exist"))?;
    let track_kind = q.track(track).map(|t| t.kind).ok_or_else(|| bad(CMD, "target track does not exist"))?;
    let frame = q.settings.frame_rate.frame_duration();
    let speed = item.speed.abs() * if item.reverse { -1.0 } else { 1.0 };
    if !speed.is_finite() || speed == 0.0 || item.duration < frame {
        return Err(bad(CMD, "target clip has no keyframe range"));
    }
    let mut destinations = HashSet::with_capacity(keys.len());
    let mut prepared = Vec::with_capacity(keys.len());
    for key in &keys {
        if key.effect.is_empty()
            || key.effect.len() > 128
            || key.param.is_empty()
            || key.param.len() > 256
            || key.instance >= MAX_EFFECTS
            || key.mask.is_some_and(|m| m >= MAX_MASKS)
        {
            return Err(bad(CMD, "invalid keyframe entry"));
        }
        let def = filmcraft_project::find_effect(&key.effect).ok_or_else(|| bad(CMD, "unknown effect in clipboard"))?;
        if matches!(def.kind, EffectKind::AudioTransition | EffectKind::VideoTransition) || (def.kind == EffectKind::Audio) != (track_kind == TrackKind::Audio)
        {
            return Err(bad(CMD, "effect type does not match target clip"));
        }
        let template = template_map.get(&(key.effect.clone(), key.instance)).ok_or_else(|| bad(CMD, "missing effect template"))?;
        if def.intrinsic && item.effects.iter().filter(|effect| effect.effect == key.effect).count() <= key.instance {
            return Err(bad(CMD, "a missing intrinsic effect cannot be created"));
        }
        let parameter_def = def.params.iter().find(|pd| pd.id == key.param);
        if key.mask.is_none() && parameter_def.is_none() {
            return Err(bad(CMD, "parameter is not part of the effect"));
        }
        if let Some(pd) = parameter_def
            && !value_allowed(&pd.kind, &key.keyframe.value)
        {
            return Err(bad(CMD, "keyframe value has the wrong parameter type"));
        }
        if let Some(mask_index) = key.mask {
            let mask_param = template.masks.get(mask_index).and_then(|mask| mask.param(&key.param)).ok_or_else(|| bad(CMD, "mask parameter is invalid"))?;
            let mask_def = filmcraft_project::mask::mask_param_defs().iter().find(|definition| definition.id == key.param);
            if !same_value_type(&mask_param.value, &key.keyframe.value)
                || mask_def.is_none_or(|definition| !value_allowed(&definition.kind, &key.keyframe.value))
            {
                return Err(bad(CMD, "keyframe value has the wrong mask parameter type"));
            }
        }
        let timeline = Tick(anchor.0.checked_add(key.offset.0).ok_or_else(|| bad(CMD, "keyframe timeline overflows"))?);
        let last = item.end() - frame;
        if timeline < item.start || timeline > last {
            return Err(bad(CMD, "pasted keyframe falls outside the target clip"));
        }
        let media_time = item.moving_source_time_at(timeline);
        if !destinations.insert((key.effect.clone(), key.instance, key.mask, key.param.clone(), media_time)) {
            return Err(bad(CMD, "clipboard contains colliding keyframes"));
        }
        let mut keyframe = key.keyframe.clone();
        keyframe.time = media_time;
        if !finite_value(&keyframe.value) || !keyframe.influence_is_valid() {
            return Err(bad(CMD, "keyframe value is not finite"));
        }
        prepared.push((key.clone(), keyframe));
    }
    let mut out = Vec::with_capacity(prepared.len());
    s.edit_sequence("Paste Keyframes", |sequence, _, _| {
        let (_, item) = sequence.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        for (key, keyframe) in &prepared {
            if effect_index(item, &key.effect, key.instance).is_none() {
                let template = template_map.get(&(key.effect.clone(), key.instance)).ok_or_else(|| bad(CMD, "missing effect template"))?;
                let mut instance = template.def().ok_or_else(|| bad(CMD, "unknown effect template"))?.instance();
                instance.enabled = template.enabled;
                let audio = instance.def().is_some_and(|def| def.kind == EffectKind::Audio);
                let index = if audio {
                    item.effects.len()
                } else {
                    item.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(item.effects.len())
                };
                item.effects.insert(index, instance);
            }
            let index = effect_index(item, &key.effect, key.instance).ok_or_else(|| bad(CMD, "could not create target effect"))?;
            let effect = item.effects.get_mut(index).ok_or_else(|| bad(CMD, "target effect disappeared"))?;
            if let Some(mask) = key.mask {
                while effect.masks.len() <= mask {
                    let template_effect = template_map.get(&(key.effect.clone(), key.instance)).ok_or_else(|| bad(CMD, "missing effect template"))?;
                    let source_mask = template_effect.masks.get(effect.masks.len()).ok_or_else(|| bad(CMD, "missing mask template"))?;
                    let mut copied = source_mask.clone();
                    copied.feather = Param::new(ParamValue::Float(10.0));
                    copied.opacity = Param::new(ParamValue::Float(100.0));
                    copied.expansion = Param::new(ParamValue::Float(0.0));
                    copied.path.keyframes.clear();
                    effect.masks.push(copied);
                }
            }
            let param = match key.mask {
                Some(mask) => effect.masks.get_mut(mask).and_then(|m| m.param_mut(&key.param)),
                None => effect.params.get_mut(&key.param),
            }
            .ok_or_else(|| bad(CMD, "target parameter does not exist"))?;
            if !same_value_type(&param.value, &keyframe.value) {
                return Err(bad(CMD, "keyframe value has the wrong parameter type"));
            }
            if let Some(index) = param.keyframes.iter().position(|existing| existing.time == keyframe.time) {
                param.keyframes[index] = keyframe.clone();
            } else {
                if param.keyframes.len() >= MAX_COPIED_KEYS {
                    return Err(bad(CMD, "target parameter already has too many keyframes"));
                }
                let index = param.keyframes.partition_point(|existing| existing.time < keyframe.time);
                param.keyframes.insert(index, keyframe.clone());
            }
            out.push(KeyRef { clip: clip.0, effect: index, param: key.param.clone(), mask: key.mask, time: keyframe.time });
        }
        Ok(())
    })?;
    Ok(json!({"keyframes": out}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    /// A session on the demo project, with the id of its first video clip.
    fn fixture() -> (Session, ClipId) {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let sequence = session.active_sequence().expect("demo sequence");
        let clip = sequence.video_tracks.first().and_then(|track| track.items.first()).map(|item| item.id).expect("demo video clip");
        (session, clip)
    }

    /// Add a Motion keyframe to the demo clip at `seconds` and return its reference.
    fn add_key(session: &mut Session, clip: ClipId, param: &str, seconds: f64) -> KeyRef {
        session.execute("playhead.set", json!({"seconds": seconds})).expect("seek");
        session.execute("effects.addKeyframe", json!({"clip": clip.0, "effect": "motion", "param": param})).expect("add keyframe");
        let item = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| item).expect("clip remains");
        let effect = item.effects.iter().position(|effect| effect.effect == "motion").expect("motion effect");
        let time = item.source_time_at(session.playhead());
        KeyRef { clip: clip.0, effect, param: param.to_string(), mask: None, time }
    }

    #[test]
    /// Setting the interpolation of several selected keyframes is one undo step, and a stale
    /// reference in the set rejects the whole call.
    fn selected_interpolation_is_one_atomic_edit_and_rejects_stale_refs() {
        let (mut session, clip) = fixture();
        let first = add_key(&mut session, clip, "scale", 0.8);
        let second = add_key(&mut session, clip, "rotation", 1.2);
        session.execute("masks.add", json!({"clip": clip.0, "effect": "opacity", "shape": "ellipse"})).expect("add mask");
        session.execute("playhead.set", json!({"seconds": 1.5})).expect("seek mask key");
        session.execute("effects.addKeyframe", json!({"clip": clip.0, "effect": "opacity", "mask": 0, "param": "feather"})).expect("add mask keyframe");
        let item = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| item).expect("clip");
        let opacity = item.effects.iter().position(|effect| effect.effect == "opacity").expect("opacity");
        let masked =
            KeyRef { clip: clip.0, effect: opacity, param: "feather".into(), mask: Some(0), time: item.effects[opacity].masks[0].feather.keyframes[0].time };
        let undo = session.history.undo.len();
        session
            .execute("effects.setInterpolations", json!({"keyframes": [first.clone(), second.clone(), masked.clone()], "interpolation": "easeOut"}))
            .expect("batch interpolation");
        assert_eq!(session.history.undo.len(), undo + 1);
        let item = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| item).expect("clip");
        for key in [&first, &second] {
            let param = item.effects[key.effect].params.get(&key.param).expect("parameter");
            assert_eq!(param.keyframes.iter().find(|kf| kf.time == key.time).map(|kf| kf.interp), Some(Interpolation::EaseOut));
        }
        assert_eq!(item.effects[masked.effect].masks[0].feather.keyframes[0].interp, Interpolation::EaseOut);
        let before = session.project.clone();
        let undo = session.history.undo.len();
        let mut stale = second;
        stale.time += Tick(1);
        assert!(session.execute("effects.setInterpolations", json!({"keyframes": [first, stale, masked], "interpolation": "hold"})).is_err());
        assert!(Arc::ptr_eq(&before, &session.project));
        assert_eq!(session.history.undo.len(), undo);
    }

    #[test]
    /// The selected keyframes delete as one edit, and a stale reference in the set deletes nothing.
    fn selected_keyframes_delete_together_and_reject_stale_refs_atomically() {
        let (mut session, clip) = fixture();
        let scale = add_key(&mut session, clip, "scale", 0.8);
        let rotation = add_key(&mut session, clip, "rotation", 1.2);
        let undo = session.history.undo.len();
        let before = session.project.clone();
        let result = session.execute("effects.deleteKeyframes", json!({"keyframes": [scale.clone(), rotation.clone()]})).expect("delete keys");
        assert_eq!(result["deleted"], 2);
        assert_eq!(session.history.undo.len(), undo + 1);
        let item = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| item).expect("clip");
        assert!(item.effects[scale.effect].params["scale"].keyframes.is_empty());
        assert!(item.effects[rotation.effect].params["rotation"].keyframes.is_empty());
        let deleted = session.project.clone();
        session.undo();
        assert_eq!(serde_json::to_value(&*session.project).expect("restored project"), serde_json::to_value(&*before).expect("before project"));
        session.redo();
        assert_eq!(serde_json::to_value(&*session.project).expect("deleted project"), serde_json::to_value(&*deleted).expect("deleted snapshot"));

        let undo = session.history.undo.len();
        let before_bad = session.project.clone();
        let stale = KeyRef { time: Tick::MAX, ..rotation };
        assert!(session.execute("effects.deleteKeyframes", json!({"keyframes": [scale, stale]})).is_err());
        assert!(Arc::ptr_eq(&before_bad, &session.project));
        assert_eq!(session.history.undo.len(), undo);
    }

    #[test]
    /// Several selected effects delete as one edit; intrinsic effects and duplicate indices are
    /// refused.
    fn selected_effects_delete_together_and_refuse_intrinsic_or_duplicate_indices() {
        let (mut session, clip) = fixture();
        session.execute("effects.apply", json!({"clips": [clip.0], "effect": "gaussian_blur"})).expect("apply blur");
        session.execute("effects.apply", json!({"clips": [clip.0], "effect": "tint"})).expect("apply tint");
        let before = session.project.clone();
        let effects = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| item.effects.clone()).expect("effects");
        let indices: Vec<usize> =
            effects.iter().enumerate().filter_map(|(index, effect)| matches!(effect.effect.as_str(), "gaussian_blur" | "tint").then_some(index)).collect();
        let undo = session.history.undo.len();
        let result = session.execute("effects.removeMany", json!({"clip": clip.0, "indices": indices})).expect("remove effects");
        assert_eq!(result["removed"], 2);
        assert_eq!(session.history.undo.len(), undo + 1);
        let remaining = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| &item.effects).expect("effects");
        assert!(remaining.iter().all(|effect| !matches!(effect.effect.as_str(), "gaussian_blur" | "tint")));
        let deleted = session.project.clone();
        session.undo();
        assert_eq!(serde_json::to_value(&*session.project).expect("restored project"), serde_json::to_value(&*before).expect("before project"));
        session.redo();
        assert_eq!(serde_json::to_value(&*session.project).expect("deleted project"), serde_json::to_value(&*deleted).expect("deleted snapshot"));

        let undo = session.history.undo.len();
        let before_bad = session.project.clone();
        let intrinsic = effects.iter().position(|effect| effect.def().is_some_and(|definition| definition.intrinsic)).expect("intrinsic effect");
        for request in [json!({"clip": clip.0, "indices": [intrinsic]}), json!({"clip": clip.0, "indices": [0, 0]})] {
            assert!(session.execute("effects.removeMany", request).is_err());
            assert!(Arc::ptr_eq(&before_bad, &session.project));
            assert_eq!(session.history.undo.len(), undo);
        }
    }

    #[test]
    /// A duplicate keeps the original keyframe and adds one full copy at the destination.
    fn alt_drag_duplicate_preserves_original_and_copies_full_keyframe_once() {
        let (mut session, clip) = fixture();
        let reference = add_key(&mut session, clip, "scale", 0.8);
        let before = session
            .active_sequence()
            .and_then(|q| q.find_item(clip))
            .map(|(_, item)| item.effects[reference.effect].params["scale"].keyframes.clone())
            .expect("keyframes");
        let result = session
            .execute("effects.duplicateKeyframes", json!({"keyframes": [reference.clone()], "delta": Tick::from_seconds_f64(0.5).0}))
            .expect("duplicate");
        let copied: Vec<KeyRef> = serde_json::from_value(result["keyframes"].clone()).expect("returned key refs");
        let after = session
            .active_sequence()
            .and_then(|q| q.find_item(clip))
            .map(|(_, item)| item.effects[reference.effect].params["scale"].keyframes.clone())
            .expect("keyframes");
        assert_eq!(after.len(), before.len() + 1);
        assert!(after.iter().any(|kf| kf.time == reference.time));
        let copied_key = after.iter().find(|kf| kf.time == copied[0].time).expect("copy");
        let original = before.iter().find(|kf| kf.time == reference.time).expect("original");
        assert_eq!(copied_key.value, original.value);
        assert_eq!(copied_key.interp, original.interp);
        assert_eq!(copied_key.in_influence, original.in_influence);
        assert_eq!(copied_key.out_influence, original.out_influence);
    }

    #[test]
    /// Reordering and pasting keep the chain's intrinsic/standard order, and each undoes in one step.
    fn effects_reorder_and_paste_keep_chain_order_and_support_undo() {
        let (mut session, clip) = fixture();
        session.execute("effects.apply", json!({"clips": [clip.0], "effect": "gaussian_blur"})).expect("apply blur");
        session.execute("effects.apply", json!({"clips": [clip.0], "effect": "tint"})).expect("apply tint");
        let before = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| item.effects.clone()).expect("effects");
        let before_move = session.project.clone();
        let undo_before_move = session.history.undo.len();
        let blur = before.iter().position(|effect| effect.effect == "gaussian_blur").expect("blur");
        let tint = before.iter().position(|effect| effect.effect == "tint").expect("tint");
        session.execute("effects.move", json!({"clip": clip.0, "from": blur, "to": tint})).expect("move effect");
        assert_eq!(session.history.undo.len(), undo_before_move + 1);
        let moved = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| item.effects.clone()).expect("effects");
        assert!(moved.iter().position(|effect| effect.effect == "tint") < moved.iter().position(|effect| effect.effect == "gaussian_blur"));
        let after_move = session.project.clone();
        session.undo();
        assert_eq!(serde_json::to_value(&*session.project).expect("undo project"), serde_json::to_value(&*before_move).expect("before project"));
        session.redo();
        assert_eq!(serde_json::to_value(&*session.project).expect("redo project"), serde_json::to_value(&*after_move).expect("after move project"));
        let source_start = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| item.start).expect("source clip start");
        session.execute("playhead.set", json!({"time": source_start.0 + Tick::from_seconds_f64(0.5).0})).expect("seek to source effect");
        session.execute("effects.addKeyframe", json!({"clip": clip.0, "effect": "gaussian_blur", "param": "blurriness"})).expect("animate copied effect");
        let source_item = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| item).expect("source clip");
        let source_in = source_item.source_in;
        let source_key = source_item
            .effects
            .iter()
            .find(|effect| effect.effect == "gaussian_blur")
            .map(|effect| effect.params["blurriness"].keyframes[0].time)
            .expect("source keyframe");
        let snapshot = source_item.effects.iter().find(|effect| effect.effect == "gaussian_blur").cloned().expect("blur instance");
        let undo = session.history.undo.len();
        session
            .execute("effects.paste", json!({"sourceClip": clip.0, "sourceIn": source_in.0, "effects": [snapshot], "clips": [clip.0]}))
            .expect("paste effect");
        assert_eq!(session.history.undo.len(), undo + 1);
        let after = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| &item.effects).expect("effects");
        assert_eq!(after.iter().filter(|effect| effect.effect == "gaussian_blur").count(), 2);

        let second_clip =
            session.active_sequence().and_then(|q| q.video_tracks.first().and_then(|track| track.items.get(1))).map(|item| item.id).expect("second video clip");
        let target_in = session.active_sequence().and_then(|q| q.find_item(second_clip)).map(|(_, item)| item.source_in).expect("target In");
        session
            .execute("effects.paste", json!({"sourceClip": clip.0, "sourceIn": source_in.0, "effects": [snapshot.clone()], "clips": [second_clip.0]}))
            .expect("paste to another clip");
        let pasted_key = session
            .active_sequence()
            .and_then(|q| q.find_item(second_clip))
            .and_then(|(_, item)| item.effects.iter().find(|effect| effect.effect == "gaussian_blur"))
            .and_then(|effect| effect.params.get("blurriness"))
            .and_then(|param| param.keyframes.first())
            .map(|key| key.time)
            .expect("retimed keyframe");
        assert_eq!(pasted_key, Tick(source_key.0 + target_in.0 - source_in.0));

        let audio_clip =
            session.active_sequence().and_then(|q| q.audio_tracks.first().and_then(|track| track.items.first())).map(|item| item.id).expect("audio clip");
        let before_bad_target = session.project.clone();
        let history = session.history.undo.len();
        assert!(
            session.execute("effects.paste", json!({"sourceClip": clip.0, "sourceIn": source_in.0, "effects": [snapshot], "clips": [audio_clip.0]}),).is_err()
        );
        assert!(Arc::ptr_eq(&before_bad_target, &session.project), "an incompatible target leaves the project untouched");
        assert_eq!(session.history.undo.len(), history);
    }

    #[test]
    /// Pasting a keyframe whose effect is missing applies the effect once and touches only the
    /// requested parameter lane.
    fn keyframe_paste_creates_missing_effect_and_only_adds_the_requested_lane() {
        let (mut session, clip) = fixture();
        let template = filmcraft_project::find_effect("gaussian_blur").expect("blur def").instance();
        let keyframe = Keyframe::new(Tick::ZERO, ParamValue::Float(33.0));
        let entry = json!({"effect":"gaussian_blur","instance":0,"param":"blurriness","offset":0,"keyframe":keyframe});
        session
            .execute(
                "effects.pasteKeyframes",
                json!({"clip":clip.0,"anchor":Tick::from_seconds_f64(1.0).0,"keyframes":[entry],"effects":[{"effect":"gaussian_blur","instance":0,"template":template}]}),
            )
            .expect("paste into missing effect");
        let item = session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, item)| item).expect("clip");
        let blur = item.effects.iter().find(|effect| effect.effect == "gaussian_blur").expect("created blur");
        assert_eq!(blur.params["blurriness"].keyframes.len(), 1);
        assert_eq!(blur.params["blurriness"].keyframes[0].value, ParamValue::Float(33.0));
        assert!(blur.params["dimensions"].keyframes.is_empty(), "unselected lanes are not copied");
        assert_eq!(blur.params["blurriness"].value, ParamValue::Float(0.0), "unrelated static values use defaults");
    }
}

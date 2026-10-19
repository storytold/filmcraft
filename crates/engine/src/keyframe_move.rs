//! Atomic movement of keys across effect/mask parameters; the common timeline delta keeps
//! relative timing intact, including at the clip boundaries. Selection itself is a UI concern.
use std::collections::{BTreeMap, HashSet};

use filmcraft_project::{ClipId, Keyframe, Param, Sequence, TrackItem};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::{Result, Session, commands::bad};

const COMMAND: &str = "effects.moveKeyframes";
const MAX_KEYS: usize = 4096;

#[derive(Clone, Deserialize, Serialize, PartialEq, Eq, Hash)]
#[serde(deny_unknown_fields)]
struct Key {
    clip: u64,
    effect: usize,
    param: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    mask: Option<usize>,
    time: Tick,
}

type Lane = (u64, usize, Option<usize>, String);

fn param_mut<'a>(sequence: &'a mut Sequence, lane: &Lane) -> Result<&'a mut Param> {
    let (_, it) = sequence.find_item_mut(ClipId(lane.0)).ok_or_else(|| bad(COMMAND, "clip does not exist"))?;
    let effect = it.effects.get_mut(lane.1).ok_or_else(|| bad(COMMAND, "effect does not exist"))?;
    match lane.2 {
        Some(mask) => effect.masks.get_mut(mask).and_then(|m| m.param_mut(&lane.3)),
        None => effect.params.get_mut(&lane.3),
    }
    .ok_or_else(|| bad(COMMAND, "parameter does not exist"))
}

fn direction(it: &TrackItem) -> f64 {
    it.speed.abs() * if it.reverse { -1.0 } else { 1.0 }
}

pub(crate) fn move_many(s: &mut Session, p: &Value) -> Result<Value> {
    let raw = p.get("keyframes").and_then(Value::as_array).ok_or_else(|| bad(COMMAND, "need keyframes"))?;
    if raw.is_empty() || raw.len() > MAX_KEYS {
        return Err(bad(COMMAND, "keyframe count must be between 1 and 4096"));
    }
    let requested = p.get("delta").and_then(Value::as_i64).ok_or_else(|| bad(COMMAND, "need an integer timeline delta"))?;
    let mut keys: Vec<Key> = serde_json::from_value(Value::Array(raw.clone())).map_err(|_| bad(COMMAND, "invalid keyframe reference"))?;
    let mut unique = HashSet::new();
    let mut saved = Vec::with_capacity(keys.len());
    let q = s.active_sequence().ok_or(crate::EngineError::NoSequence)?;
    let frame = q.settings.frame_rate.frame_duration();
    let (mut low, mut high) = (i64::MIN, i64::MAX);
    for key in &keys {
        if key.param.len() > 256 || !unique.insert(key.clone()) {
            return Err(bad(COMMAND, "duplicate or invalid keyframe reference"));
        }
        let (_, it) = q.find_item(ClipId(key.clip)).ok_or_else(|| bad(COMMAND, "clip does not exist"))?;
        let effect = it.effects.get(key.effect).ok_or_else(|| bad(COMMAND, "effect does not exist"))?;
        crate::commands::ensure_supported_time_remap_edit(&effect.effect, &key.param)?;
        let param = match key.mask {
            Some(mask) => effect.masks.get(mask).and_then(|m| m.param(&key.param)),
            None => effect.params.get(&key.param),
        }
        .ok_or_else(|| bad(COMMAND, "parameter does not exist"))?;
        let k = param.keyframes.iter().find(|k| k.time == key.time).ok_or_else(|| bad(COMMAND, "keyframe does not exist"))?;
        let speed = direction(it);
        if !speed.is_finite() || speed == 0.0 || it.duration < frame {
            return Err(bad(COMMAND, "clip has no movable keyframe range"));
        }
        let a = it.moving_source_time_at(it.start);
        let b = it.moving_source_time_at(it.end() - frame);
        if key.time < a.min(b) || key.time > a.max(b) {
            return Err(bad(COMMAND, "keyframe is outside the visible clip range"));
        }
        let da = (a - key.time).0 as f64 / speed;
        let db = (b - key.time).0 as f64 / speed;
        low = low.max(da.min(db).ceil() as i64);
        high = high.min(da.max(db).floor() as i64);
        saved.push((k.clone(), speed));
    }
    if low > high {
        return Err(bad(COMMAND, "keyframes have no common movement range"));
    }
    let delta = requested.clamp(low, high);
    if delta == 0 {
        return Ok(json!({"delta":0,"keyframes":keys}));
    }
    let mut lanes: BTreeMap<Lane, Vec<(Tick, Keyframe)>> = BTreeMap::new();
    for (key, (mut k, speed)) in keys.iter_mut().zip(saved) {
        let from = key.time;
        k.time += Tick((delta as f64 * speed).round() as i64);
        key.time = k.time;
        lanes.entry((key.clip, key.effect, key.mask, key.param.clone())).or_default().push((from, k));
    }
    s.edit_sequence("Move Keyframes", |sequence, _, _| {
        for (lane, moved) in lanes {
            let param = param_mut(sequence, &lane)?;
            let from: HashSet<_> = moved.iter().map(|(t, _)| *t).collect();
            let to: HashSet<_> = moved.iter().map(|(_, k)| k.time).collect();
            // Remove all originals before inserting any destination: selected keys can move into
            // each other's old positions without overwriting a key still waiting to be moved.
            param.keyframes.retain(|k| !from.contains(&k.time) && !to.contains(&k.time));
            param.keyframes.extend(moved.into_iter().map(|(_, k)| k));
            param.keyframes.sort_by_key(|k| k.time);
        }
        Ok(())
    })?;
    Ok(json!({"delta":delta,"keyframes":keys}))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn fixture() -> (Session, Vec<Value>) {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
        let mut keys = Vec::new();
        for (param, seconds) in [("scale", 0.5), ("scale", 1.0), ("rotation", 1.5)] {
            s.execute("playhead.set", json!({"seconds":seconds})).unwrap();
            s.execute("effects.addKeyframe", json!({"clip":clip.0,"effect":"motion","param":param})).unwrap();
            let it = s.active_sequence().unwrap().find_item(clip).unwrap().1;
            let effect = it.effects.iter().position(|e| e.effect == "motion").unwrap();
            keys.push(json!({"clip":clip.0,"effect":effect,"param":param,"time":it.source_time_at(s.playhead()).0}));
        }
        (s, keys)
    }

    fn values(s: &Session, key: &Value) -> Vec<Keyframe> {
        let it = s.active_sequence().unwrap().find_item(ClipId(key["clip"].as_u64().unwrap())).unwrap().1;
        it.effects[key["effect"].as_u64().unwrap() as usize].params[key["param"].as_str().unwrap()].keyframes.clone()
    }

    #[test]
    fn movement_across_lanes_is_atomic_and_one_undo_step() {
        let (mut s, keys) = fixture();
        let project = s.project.clone();
        let before = values(&s, &keys[0]);
        let undo = s.history.undo.len();
        // Scale's first key moves into the second one's old position. Both must survive.
        let delta = before[1].time - before[0].time;
        let result = s.execute(COMMAND, json!({"keyframes":keys,"delta":delta.0})).unwrap();
        let after = values(&s, &keys[0]);
        assert_eq!(after.len(), 2);
        for (old, new) in before.iter().zip(&after) {
            assert_eq!(new.time, old.time + delta);
            assert_eq!(serde_json::to_value(&new.value).unwrap(), serde_json::to_value(&old.value).unwrap());
            assert_eq!(new.interp, old.interp);
            assert_eq!(new.in_influence, old.in_influence);
            assert_eq!(new.out_influence, old.out_influence);
        }
        assert_eq!(result["keyframes"][2]["time"].as_i64().unwrap(), keys[2]["time"].as_i64().unwrap() + delta.0);
        assert_eq!(s.history.undo.len(), undo + 1);
        let moved = serde_json::to_value(&s.project).unwrap();
        s.undo();
        assert_eq!(serde_json::to_value(&s.project).unwrap(), serde_json::to_value(project).unwrap());
        s.redo();
        assert_eq!(serde_json::to_value(&s.project).unwrap(), moved);
    }

    #[test]
    fn bounds_clamp_the_group_without_changing_its_spacing() {
        let (mut s, keys) = fixture();
        let undo = s.history.undo.len();
        let result = s.execute(COMMAND, json!({"keyframes":keys,"delta":i64::MAX})).unwrap();
        let delta = result["delta"].as_i64().unwrap();
        assert!(delta > 0 && delta < i64::MAX);
        for (old, new) in keys.iter().zip(result["keyframes"].as_array().unwrap()) {
            assert_eq!(new["time"].as_i64().unwrap() - old["time"].as_i64().unwrap(), delta);
        }
        assert_eq!(s.history.undo.len(), undo + 1);
        s.execute(COMMAND, json!({"keyframes":result["keyframes"],"delta":i64::MAX})).unwrap();
        assert_eq!(s.history.undo.len(), undo + 1, "a boundary no-op creates no edit");
    }

    #[test]
    fn invalid_references_and_hostile_requests_cannot_make_partial_edits() {
        let (mut s, keys) = fixture();
        let before = s.project.clone();
        let undo = s.history.undo.len();
        let mut invalid = keys[1].clone();
        invalid["param"] = json!("missing");
        for request in [
            json!({"keyframes":[keys[0],invalid],"delta":100}),
            json!({"keyframes":[keys[0],keys[0]],"delta":100}),
            json!({"keyframes":[],"delta":100}),
            json!({"keyframes":vec![keys[0].clone();MAX_KEYS+1],"delta":100}),
            json!({"keyframes":[{"clip":keys[0]["clip"],"effect":u64::MAX,"param":"scale","time":0}],"delta":i64::MIN}),
            json!({"keyframes":keys,"delta":"fast"}),
        ] {
            assert!(s.execute(COMMAND, request).is_err());
            assert!(Arc::ptr_eq(&before, &s.project));
            assert_eq!(s.history.undo.len(), undo);
        }
    }

    #[test]
    fn media_times_follow_clip_speed_and_reverse() {
        for reverse in [false, true] {
            let (mut s, keys) = fixture();
            let clip = ClipId(keys[0]["clip"].as_u64().unwrap());
            s.edit_sequence("test speed", |q, _, _| {
                let it = q.find_item_mut(clip).unwrap().1;
                it.speed = 2.0;
                it.reverse = reverse;
                Ok(())
            })
            .unwrap();
            let delta = Tick::from_seconds_f64(0.1);
            let result = s.execute(COMMAND, json!({"keyframes":keys,"delta":delta.0})).unwrap();
            for (old, new) in keys.iter().zip(result["keyframes"].as_array().unwrap()) {
                let diff = new["time"].as_i64().unwrap() - old["time"].as_i64().unwrap();
                assert_eq!(diff, delta.0 * 2 * if reverse { -1 } else { 1 });
            }
        }
    }
}

//! Editing standard effect instances. Intrinsic effects and graphic layers keep their fixed roles.
use filmcraft_project::{ClipId, EffectInstance, EffectKind, Sequence, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, has_seq};
use crate::{EffectSel, EngineError, Result, Session};

#[derive(Clone, Debug)]
pub struct EffectClipboard {
    pub effect: EffectInstance,
    pub source_in: Tick,
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    type Run = fn(&mut Session, &Value) -> Result<Value>;
    [
        ("effects.copy", "Copy Effect", copy as Run, false),
        ("effects.cut", "Cut Effect", cut as Run, true),
        ("effects.remove", "Remove Effect", remove as Run, true),
        ("effects.duplicate", "Duplicate Effect", duplicate as Run, true),
        ("effects.move", "Move Effect", move_effect as Run, true),
        ("effects.paste", "Paste Effect", paste as Run, true),
    ]
    .into_iter()
    .map(|(id, label, run, journal)| CommandSpec {
        id,
        label,
        menu: &[],
        shortcut: None,
        params: if id == "effects.paste" {
            r#"{"clips":[id]? (defaults to selected clips); inserts after selected effect on its clip, otherwise at end of standard stack}"#
        } else if id == "effects.move" {
            r#"{"clip":id?,"index":n?,"to":n (final absolute effect index); omitted source uses selected effect}"#
        } else {
            r#"{"clip":id?,"index":n?} (omit both to use selected effect)"#
        },
        enabled: if id == "effects.paste" { can_paste } else { has_seq },
        run,
        journal,
    })
    .collect()
}

fn standard(e: &EffectInstance) -> bool {
    e.def().is_some_and(|d| !d.intrinsic && matches!(d.kind, EffectKind::Video | EffectKind::Audio)) && !filmcraft_project::graphic::is_layer_id(&e.effect)
}

fn can_paste(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if s.state.effect_clipboard.is_none() {
        return Err("copy an effect first".into());
    }
    if s.state.selection.is_empty() {
        return Err("select destination clips".into());
    }
    Ok(())
}

fn item(q: &Sequence, clip: ClipId, writable: bool) -> Result<&TrackItem> {
    let (track, it) = q.find_item(clip).ok_or_else(|| bad("effects", "no such clip"))?;
    if writable && q.track(track).is_some_and(|t| t.locked) {
        return Err(bad("effects", "track is locked"));
    }
    Ok(it)
}

fn source(s: &Session, p: &Value, writable: bool) -> Result<(ClipId, usize)> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (clip, index) = if p.get("clip").is_some() || p.get("index").is_some() {
        let clip = ClipId(p.get("clip").and_then(Value::as_u64).ok_or_else(|| bad("effects", "need valid clip"))?);
        let index = p.get("index").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).ok_or_else(|| bad("effects", "need valid index"))?;
        (clip, index)
    } else {
        let sel = s.state.selected_effect.as_ref().ok_or_else(|| bad("effects", "select an effect first"))?;
        let it = item(q, sel.clip, writable)?;
        (sel.clip, sel.index(&it.effects).ok_or_else(|| bad("effects", "selected effect no longer exists"))?)
    };
    let it = item(q, clip, writable)?;
    if !it.effects.get(index).is_some_and(standard) {
        return Err(bad("effects", "select a standard effect; intrinsic effects and graphic layers are fixed"));
    }
    Ok((clip, index))
}

fn copied(s: &Session, clip: ClipId, index: usize) -> Result<EffectClipboard> {
    let it = item(s.active_sequence().ok_or(EngineError::NoSequence)?, clip, false)?;
    let effect = it.effects.get(index).ok_or_else(|| bad("effects", "no such effect"))?.clone();
    Ok(EffectClipboard { effect, source_in: it.source_in })
}

fn copy(s: &mut Session, p: &Value) -> Result<Value> {
    let (clip, index) = source(s, p, false)?;
    s.state.effect_clipboard = Some(copied(s, clip, index)?);
    Ok(json!({"copied": 1}))
}

fn cut(s: &mut Session, p: &Value) -> Result<Value> {
    let (clip, index) = source(s, p, true)?;
    let clipboard = copied(s, clip, index)?;
    remove(s, p)?;
    s.state.effect_clipboard = Some(clipboard);
    Ok(json!({"cut": 1}))
}

// Keep occurrence-based effect selections and index-based mask selections attached to their instance.
fn remap(st: &mut crate::EditorState, clip: ClipId, before: &[EffectInstance], after: &[EffectInstance], map: impl Fn(usize) -> Option<usize>) {
    if let Some(sel) = st.selected_effect.as_ref().filter(|s| s.clip == clip) {
        st.selected_effect = sel.index(before).and_then(&map).and_then(|i| EffectSel::of(clip, after, i));
    }
    if let Some(sel) = st.selected_mask.filter(|s| s.clip == clip) {
        st.selected_mask = map(sel.effect).map(|effect| crate::masks::MaskSel { effect, ..sel });
    }
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let (clip, index) = source(s, p, true)?;
    s.edit_sequence("Remove Effect", |q, _, st| {
        let (_, it) = q.find_item_mut(clip).ok_or_else(|| bad("effects.remove", "no such clip"))?;
        let before = it.effects.clone();
        it.effects.remove(index); // source validated the index against this unchanged snapshot
        remap(st, clip, &before, &it.effects, |i| if i == index { None } else { Some(if i > index { i - 1 } else { i }) });
        Ok(())
    })?;
    Ok(Value::Null)
}

fn move_effect(s: &mut Session, p: &Value) -> Result<Value> {
    let (clip, from) = source(s, p, true)?;
    let to = p.get("to").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).ok_or_else(|| bad("effects.move", "need valid to index"))?;
    let it = item(s.active_sequence().ok_or(EngineError::NoSequence)?, clip, true)?;
    if !it.effects.get(to).is_some_and(standard) || !it.effects.get(from.min(to)..=from.max(to)).is_some_and(|e| e.iter().all(standard)) {
        return Err(bad("effects.move", "effects can only move within their standard stack"));
    }
    if from == to {
        return Ok(Value::Null);
    }
    s.edit_sequence("Move Effect", |q, _, st| {
        let (_, it) = q.find_item_mut(clip).ok_or_else(|| bad("effects.move", "no such clip"))?;
        let before = it.effects.clone();
        let e = it.effects.remove(from);
        it.effects.insert(to, e);
        remap(st, clip, &before, &it.effects, |i| {
            Some(if i == from {
                to
            } else if from < i && i <= to {
                i - 1
            } else if to <= i && i < from {
                i + 1
            } else {
                i
            })
        });
        Ok(())
    })?;
    Ok(Value::Null)
}

fn duplicate(s: &mut Session, p: &Value) -> Result<Value> {
    let (clip, index) = source(s, p, true)?;
    let e = copied(s, clip, index)?;
    insert(s, &e, vec![(clip, Some(index + 1))], "Duplicate Effect")
}

fn paste(s: &mut Session, p: &Value) -> Result<Value> {
    let e = s.state.effect_clipboard.clone().ok_or_else(|| bad("effects.paste", "copy an effect first"))?;
    let clips: Vec<ClipId> = match p.get("clips") {
        Some(Value::Array(ids)) if ids.len() <= 10_000 => {
            ids.iter().map(|v| v.as_u64().map(ClipId).ok_or_else(|| bad("effects.paste", "invalid clip id"))).collect::<Result<_>>()?
        }
        Some(_) => return Err(bad("effects.paste", "clips must be a list of at most 10000 ids")),
        None => s.state.selection.clone(),
    };
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let audio = e.effect.def().is_some_and(|d| d.kind == EffectKind::Audio);
    let mut targets = Vec::new();
    for clip in clips {
        let it = item(q, clip, false)?;
        let (track, _) = q.find_item(clip).ok_or_else(|| bad("effects.paste", "no such clip"))?;
        if q.track(track).is_none_or(|t| (t.kind == TrackKind::Audio) != audio) || targets.iter().any(|(c, _)| *c == clip) {
            continue;
        }
        let index = s
            .state
            .selected_effect
            .as_ref()
            .filter(|s| s.clip == clip)
            .and_then(|s| s.index(&it.effects))
            .filter(|i| it.effects.get(*i).is_some_and(standard))
            .map(|i| i + 1);
        targets.push((clip, index));
    }
    insert(s, &e, targets, "Paste Effect")
}

fn insert(s: &mut Session, e: &EffectClipboard, targets: Vec<(ClipId, Option<usize>)>, label: &str) -> Result<Value> {
    if targets.is_empty() {
        return Err(bad("effects.paste", "select a compatible clip first"));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut prepared = Vec::new();
    for (clip, at) in targets {
        let it = item(q, clip, true)?;
        if it.effects.len() >= 4096 {
            return Err(bad("effects.paste", "effect stack limit reached"));
        }
        let audio = e.effect.def().is_some_and(|d| d.kind == EffectKind::Audio);
        let at = at.unwrap_or_else(|| {
            if audio { it.effects.len() } else { it.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(it.effects.len()) }
        });
        if at > it.effects.len() {
            return Err(bad("effects.paste", "invalid insertion index"));
        }
        let mut effect = e.effect.clone();
        let delta = i128::from(it.source_in.0) - i128::from(e.source_in.0);
        for param in effect.params.values_mut().chain(effect.masks.iter_mut().flat_map(|m| [&mut m.path, &mut m.feather, &mut m.opacity, &mut m.expansion])) {
            for k in &mut param.keyframes {
                k.time = Tick(i64::try_from(i128::from(k.time.0) + delta).map_err(|_| bad("effects.paste", "keyframe time overflow"))?);
            }
        }
        prepared.push((clip, at, effect));
    }
    let count = prepared.len();
    s.edit_sequence(label, |q, _, st| {
        for (clip, at, effect) in prepared {
            let (_, it) = q.find_item_mut(clip).ok_or_else(|| bad("effects.paste", "no such clip"))?;
            it.effects.insert(at, effect);
            st.selected_effect = EffectSel::of(clip, &it.effects, at);
        }
        st.selected_mask = None;
        Ok(())
    })?;
    Ok(json!({"inserted": count}))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> (Session, ClipId, ClipId) {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let ids: Vec<_> = s.active_sequence().unwrap().video_tracks.iter().flat_map(|t| &t.items).map(|i| i.id).take(2).collect();
        let (a, b) = (ids[0], ids[1]);
        for effect in ["drop_shadow", "rounded_crop"] {
            s.execute("effects.apply", json!({"clips":[a.0],"effect":effect})).unwrap();
        }
        s.execute("effects.select", json!({"clip":a.0,"effect":"drop_shadow"})).unwrap();
        (s, a, b)
    }
    fn effects(s: &Session, clip: ClipId) -> Vec<EffectInstance> {
        s.active_sequence().unwrap().find_item(clip).unwrap().1.effects.clone()
    }
    fn index(s: &Session, clip: ClipId, name: &str) -> usize {
        effects(s, clip).iter().position(|e| e.effect == name).unwrap()
    }

    #[test]
    fn move_duplicate_remove_and_undo_keep_instances_and_clips() {
        let (mut s, a, _) = fixture();
        let before = s.project.to_json();
        let shadow = index(&s, a, "drop_shadow");
        let crop = index(&s, a, "rounded_crop");
        s.execute("effects.move", json!({"to":crop})).unwrap();
        assert!(index(&s, a, "rounded_crop") < index(&s, a, "drop_shadow"));
        assert_eq!(s.state.selected_effect.as_ref().unwrap().index(&effects(&s, a)), Some(crop));
        let moved = s.project.to_json();
        s.undo();
        assert_eq!(s.project.to_json(), before);
        s.redo();
        assert_eq!(s.project.to_json(), moved);
        s.execute("effects.duplicate", json!({})).unwrap();
        let e = effects(&s, a);
        assert_eq!(e[crop], e[crop + 1]);
        assert_eq!(s.state.selected_effect.as_ref().unwrap().instance, 1);
        s.execute("effects.move", json!({"to":shadow})).unwrap();
        assert_eq!(s.state.selected_effect.as_ref().unwrap().instance, 0);
        s.execute("effects.remove", json!({})).unwrap();
        assert!(s.state.selected_effect.is_none());
        assert_eq!(effects(&s, a).iter().filter(|e| e.effect == "drop_shadow").count(), 1);
        s.undo();
        assert_eq!(effects(&s, a).iter().filter(|e| e.effect == "drop_shadow").count(), 2);
    }

    #[test]
    fn copy_paste_preserves_masks_keys_and_shifts_to_target_source_in() {
        let (mut s, a, b) = fixture();
        s.execute("effects.setParam", json!({"clip":a.0,"effect":"drop_shadow","param":"distance","value":37,"keyframe":true})).unwrap();
        s.execute("masks.add", json!({"clip":a.0,"effect":"drop_shadow","shape":"ellipse"})).unwrap();
        let i = index(&s, a, "drop_shadow");
        s.execute("effects.copy", json!({"clip":a.0,"index":i})).unwrap();
        let copy = s.state.effect_clipboard.clone().unwrap();
        let history = s.history.undo.len();
        s.execute("effects.copy", json!({"clip":a.0,"index":i})).unwrap();
        assert_eq!(s.history.undo.len(), history);
        s.edit_sequence("Trim fixture", |q, _, _| {
            q.find_item_mut(b).unwrap().1.source_in = Tick(1234);
            Ok(())
        })
        .unwrap();
        let before = s.project.to_json();
        s.execute("effects.paste", json!({"clips":[b.0]})).unwrap();
        let pasted = effects(&s, b).into_iter().find(|e| e.effect == "drop_shadow").unwrap();
        assert_eq!(pasted.masks, copy.effect.masks);
        assert_eq!(pasted.enabled, copy.effect.enabled);
        let key = &pasted.params["distance"].keyframes[0];
        assert_eq!(key.time.0, copy.effect.params["distance"].keyframes[0].time.0 + 1234 - copy.source_in.0);
        assert_eq!(key.value, copy.effect.params["distance"].keyframes[0].value);
        s.undo();
        assert_eq!(s.project.to_json(), before);
        s.redo();
        assert!(effects(&s, b).iter().any(|e| e.effect == "drop_shadow"));
    }

    #[test]
    fn hostile_indices_intrinsics_and_locked_tracks_are_atomic() {
        let (mut s, a, b) = fixture();
        for p in [json!({"to":-1}), json!({"to":u64::MAX}), json!({"to":"0"}), json!({"to":index(&s,a,"motion")})] {
            let before = s.project.to_json();
            let history = s.history.undo.len();
            assert!(s.execute("effects.move", p).is_err());
            assert_eq!(s.project.to_json(), before);
            assert_eq!(s.history.undo.len(), history);
        }
        for cmd in ["effects.copy", "effects.remove", "effects.duplicate"] {
            assert!(s.execute(cmd, json!({"clip":a.0,"index":index(&s,a,"motion")})).is_err());
            assert!(s.execute(cmd, json!({"clip":a.0,"index":u64::MAX})).is_err());
        }
        s.execute("effects.copy", json!({})).unwrap();
        s.edit_sequence("Lock fixture", |q, _, _| {
            let (t, _) = q.find_item(b).unwrap();
            q.track_mut(t).unwrap().locked = true;
            Ok(())
        })
        .unwrap();
        let before = s.project.to_json();
        assert!(s.execute("effects.paste", json!({"clips":[a.0,b.0]})).is_err());
        assert_eq!(s.project.to_json(), before);
        assert!(Session::default().execute("effects.move", json!({"to":0})).is_err());
    }

    #[test]
    fn cut_keeps_clipboard_after_source_removal_and_clip_copy_replaces_it() {
        let (mut s, a, b) = fixture();
        s.execute("effects.cut", json!({})).unwrap();
        assert!(!effects(&s, a).iter().any(|e| e.effect == "drop_shadow"));
        s.execute("effects.paste", json!({"clips":[b.0]})).unwrap();
        assert!(effects(&s, b).iter().any(|e| e.effect == "drop_shadow"));
        s.execute("timeline.select", json!({"clips":[a.0]})).unwrap();
        s.execute("edit.copy", json!({})).unwrap();
        assert!(s.state.effect_clipboard.is_none());
    }

    #[test]
    fn paste_filters_media_types_and_rejects_keyframe_overflow_without_changes() {
        let (mut s, a, b) = fixture();
        let audio = s.active_sequence().unwrap().audio_tracks[0].items[0].id;
        let audio_before = effects(&s, audio);
        s.execute("effects.copy", json!({})).unwrap();
        let r = s.execute("effects.paste", json!({"clips":[audio.0,b.0,b.0]})).unwrap();
        assert_eq!(r["inserted"], 1);
        assert_eq!(effects(&s, audio), audio_before);
        assert!(s.execute("effects.paste", json!({"clips":[audio.0]})).is_err());
        s.execute("effects.setParam", json!({"clip":a.0,"effect":"drop_shadow","param":"distance","value":20,"keyframe":true})).unwrap();
        s.execute("effects.copy", json!({"clip":a.0,"index":index(&s,a,"drop_shadow")})).unwrap();
        let clipboard = s.state.effect_clipboard.as_mut().unwrap();
        clipboard.source_in = Tick(i64::MIN);
        clipboard.effect.params.get_mut("distance").unwrap().keyframes[0].time = Tick(i64::MAX);
        let before = s.project.to_json();
        assert!(s.execute("effects.paste", json!({"clips":[b.0]})).is_err());
        assert_eq!(s.project.to_json(), before);
    }
}

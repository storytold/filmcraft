//! Effect order: `effects.reorder`, `effects.moveUp` and `effects.moveDown` (#1731).
//!
//! A clip's standard effects run in stack order, so their order changes the picture or the sound:
//! Rounded Crop before Drop Shadow shadows the rounded shape, the other way round crops the shadow
//! away. Only those effects move. The fixed effects (Motion, Opacity, Time Remapping, Volume,
//! Channel Volume, Panner) keep their slots, because the renderer applies them after the standard
//! effects wherever they sit; graphic layers keep theirs, because they are arranged among
//! themselves with `graphics.arrangeLayer`; an effect this build doesn't know stays where the file
//! put it. A move therefore permutes only the slots that hold movable effects, and the indices of
//! everything else (`graphics.list` `effectIndex`, a fixed effect's index) stay valid.
//!
//! Some state names an effect by its index: the selected mask here, and the frontends' keyframe
//! selection, expanded rows and open editors. A move, and Undo or Redo of one, reports how the
//! indices changed in [`crate::Event::EffectsReordered`], and the selected effect and mask follow
//! the effect they were on.

use filmcraft_project::{ClipId, EffectInstance, Project};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, checked_u32_p, clip_p};
use crate::{EditorState, EffectSel, Event, Result, Session};

type Enabled = fn(&Session) -> std::result::Result<(), String>;
type Run = fn(&mut Session, &Value) -> Result<Value>;

/// Whether `effects.reorder` may move `e`: a standard effect this build knows, not a fixed one and
/// not a graphic layer.
pub fn movable(e: &EffectInstance) -> bool {
    e.def().is_some_and(|d| !d.intrinsic) && !filmcraft_project::graphic::is_layer(e)
}

/// Where each effect goes when the effect at `from` takes the place of the effect at `to` among the
/// movable effects: entry `i` is the new index of the effect now at index `i`. The movable effects
/// in between shift by one slot towards `from`; every other effect keeps its index. `None` when
/// either index is not a movable effect.
pub fn move_order(effects: &[EffectInstance], from: usize, to: usize) -> Option<Vec<usize>> {
    let slots: Vec<usize> = effects.iter().enumerate().filter(|(_, e)| movable(e)).map(|(i, _)| i).collect();
    let a = slots.iter().position(|s| *s == from)?;
    let b = slots.iter().position(|s| *s == to)?;
    // the movable effects in their new order, then each into the next movable slot
    let mut ranked = slots.clone();
    let moved = ranked.remove(a);
    ranked.insert(b, moved);
    let mut order: Vec<usize> = (0..effects.len()).collect();
    for (slot, effect) in slots.iter().zip(&ranked) {
        if let Some(o) = order.get_mut(*effect) {
            *o = *slot;
        }
    }
    Some(order)
}

/// Rearranges `effects` so the effect at `i` ends up at `order[i]`. `order` must be a permutation
/// of the indices (from [`move_order`] or [`order_between`]); anything else leaves `effects` alone.
pub fn apply_order(effects: &mut Vec<EffectInstance>, order: &[usize]) {
    if !is_permutation(order, effects.len()) {
        return;
    }
    let mut placed: Vec<Option<EffectInstance>> = std::iter::repeat_with(|| None).take(effects.len()).collect();
    for (effect, to) in effects.drain(..).zip(order) {
        if let Some(slot) = placed.get_mut(*to) {
            *slot = Some(effect);
        }
    }
    *effects = placed.into_iter().flatten().collect();
}

fn is_permutation(order: &[usize], len: usize) -> bool {
    let mut seen = vec![false; len];
    order.len() == len && order.iter().all(|i| seen.get_mut(*i).is_some_and(|s| !std::mem::replace(s, true)))
}

/// How `after` rearranges `before`, when it is only a rearrangement (the same effects, each
/// unchanged): entry `i` is the index in `after` of `before[i]`. `None` when the stacks are equal or
/// differ in anything but order. Identical copies of an effect can't be told apart, so they keep
/// their relative order; see [`has_copy`].
pub fn order_between(before: &[EffectInstance], after: &[EffectInstance]) -> Option<Vec<usize>> {
    if before.len() != after.len() || before == after {
        return None;
    }
    let mut used = vec![false; after.len()];
    before
        .iter()
        .map(|e| {
            let i = after.iter().zip(&used).position(|(a, u)| !u && a == e)?;
            *used.get_mut(i)? = true;
            Some(i)
        })
        .collect()
}

/// Whether another effect of `effects` is identical to the one at `index` (same effect, same
/// parameters, keyframes and masks), so a rearrangement found by [`order_between`] can't say which
/// of the two went where.
pub fn has_copy(effects: &[EffectInstance], index: usize) -> bool {
    effects.get(index).is_some_and(|e| effects.iter().enumerate().any(|(i, other)| i != index && other == e))
}

/// Points the selected effect and the selected mask on `clip` at the effect they were on, after
/// the clip's effects (`before`, now `after`) moved as `order` says. A selection on an effect with an
/// identical copy is dropped when `exact` is false: the rearrangement came from comparing the two
/// stacks, and the copy may be the one that moved.
fn follow_selection(state: &mut EditorState, clip: ClipId, before: &[EffectInstance], after: &[EffectInstance], order: &[usize], exact: bool) {
    let new_index = |i: usize| order.get(i).copied().filter(|_| exact || !has_copy(before, i));
    if let Some(sel) = state.selected_effect.clone().filter(|e| e.clip == clip)
        && let Some(i) = sel.index(before)
    {
        state.selected_effect = new_index(i).and_then(|to| EffectSel::of(clip, after, to));
    }
    if let Some(mask) = state.selected_mask.filter(|m| m.clip == clip) {
        state.selected_mask = new_index(mask.effect).map(|effect| crate::masks::MaskSel { effect, ..mask });
    }
}

/// `effects.reorder`: move the effect at `from` into the place of the effect at `to`.
fn reorder(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "effects.reorder";
    let clip = p.get("clip").and_then(Value::as_u64).map(ClipId).ok_or_else(|| bad(CMD, "need an integer `clip`"))?;
    let index = |key: &str| -> Result<usize> {
        let v = checked_u32_p(p, key, CMD)?.ok_or_else(|| bad(CMD, format!("need `{key}`: an effect index")))?;
        usize::try_from(v).map_err(|_| bad(CMD, format!("`{key}` is out of range")))
    };
    let (from, to) = (index("from")?, index("to")?);
    move_effect(s, CMD, clip, from, to)
}

/// `effects.moveUp` / `effects.moveDown`: move an effect (default: the selected one) one place
/// earlier or later among the clip's movable effects.
fn step(s: &mut Session, p: &Value, cmd: &str, later: bool) -> Result<Value> {
    let (clip, from) = match (clip_p(p, "clip"), p.get("index").filter(|v| !v.is_null())) {
        (Some(clip), Some(_)) => {
            let i = checked_u32_p(p, "index", cmd)?.and_then(|v| usize::try_from(v).ok()).ok_or_else(|| bad(cmd, "`index` must be an effect index"))?;
            (clip, i)
        }
        (None, None) => {
            let sel = s.state.selected_effect.clone().ok_or_else(|| bad(cmd, "select an effect first"))?;
            let effects = &s.active_sequence().and_then(|q| q.find_item(sel.clip)).ok_or(filmcraft_edit::EditError::NoItem(sel.clip))?.1.effects;
            (sel.clip, sel.index(effects).ok_or_else(|| bad(cmd, "the selected effect is no longer on its clip"))?)
        }
        _ => return Err(bad(cmd, "give both `clip` and `index`, or neither for the selected effect")),
    };
    let effects = &s.active_sequence().and_then(|q| q.find_item(clip)).ok_or(filmcraft_edit::EditError::NoItem(clip))?.1.effects;
    if !effects.get(from).is_some_and(movable) {
        return Err(bad(cmd, not_movable(effects, from)));
    }
    let slots: Vec<usize> = effects.iter().enumerate().filter(|(_, e)| movable(e)).map(|(i, _)| i).collect();
    let at = slots.iter().position(|i| *i == from).unwrap_or(0);
    let neighbour = if later { slots.get(at + 1) } else { at.checked_sub(1).and_then(|b| slots.get(b)) };
    let to = *neighbour.ok_or_else(|| bad(cmd, if later { "the effect is already the last one" } else { "the effect is already the first one" }))?;
    move_effect(s, cmd, clip, from, to)
}

/// Why the effect at `index` can't be moved, for the error.
fn not_movable(effects: &[EffectInstance], index: usize) -> String {
    match effects.get(index) {
        None => format!("the clip has no effect {index}"),
        Some(e) if filmcraft_project::graphic::is_layer(e) => "graphic layers are arranged with graphics.arrangeLayer".into(),
        Some(e) if e.def().is_some_and(|d| d.intrinsic) => format!("{} is a fixed effect and keeps its place", e.def().map_or("", |d| d.name)),
        Some(e) => format!("`{}` is not an effect this version knows, so it keeps its place", e.effect),
    }
}

/// Move the effect at `from` into the place of the effect at `to` as one undo step, keep the
/// selection on the effects it was on and tell the frontends. Answers `{clip, index, order}`: the
/// moved effect's new index and where every effect went (entry `i`: the new index of the effect
/// that was at `i`), so a caller can update indices it holds.
fn move_effect(s: &mut Session, cmd: &str, clip: ClipId, from: usize, to: usize) -> Result<Value> {
    let effects = &s.active_sequence().and_then(|q| q.find_item(clip)).ok_or(filmcraft_edit::EditError::NoItem(clip))?.1.effects;
    for i in [from, to] {
        if !effects.get(i).is_some_and(movable) {
            return Err(bad(cmd, not_movable(effects, i)));
        }
    }
    let order = move_order(effects, from, to).ok_or_else(|| bad(cmd, "those effects can't be reordered"))?;
    let answer = json!({"clip": clip.0, "index": to, "order": order});
    if from == to {
        return Ok(answer);
    }
    s.edit_sequence("Reorder Effect", |q, _, state| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let before = it.effects.clone();
        apply_order(&mut it.effects, &order);
        follow_selection(state, clip, &before, &it.effects, &order, true);
        Ok(())
    })?;
    s.events.push(Event::EffectsReordered { clip, order, exact: true });
    Ok(answer)
}

/// After Undo or Redo replaced `before` with the session's project: for each clip whose effects
/// were only rearranged, move the selection with them and tell the frontends. Only the clips that
/// state can point at are compared: the selected clips and those of the selected effect and mask.
pub(crate) fn follow_history(s: &mut Session, before: &Project) {
    let Some(seq_id) = s.state.active_sequence else { return };
    let mut clips = s.state.selection.clone();
    clips.extend(s.state.selected_effect.as_ref().map(|e| e.clip));
    clips.extend(s.state.selected_mask.map(|m| m.clip));
    clips.sort_unstable();
    clips.dedup();
    let project = s.project.clone();
    for clip in clips {
        let stack = |p: &Project| p.sequence(seq_id).and_then(|q| q.find_item(clip)).map(|(_, it)| it.effects.clone());
        let (Some(old), Some(new)) = (stack(before), stack(&project)) else { continue };
        let Some(order) = order_between(&old, &new) else { continue };
        // identical copies keep their relative order in `order`, which may not be how they moved
        let exact = !(0..old.len()).any(|i| order.get(i) != Some(&i) && has_copy(&old, i));
        follow_selection(&mut s.state, clip, &old, &new, &order, exact);
        s.events.push(Event::EffectsReordered { clip, order, exact });
    }
}

fn spec(id: &'static str, label: &'static str, params: &'static str, enabled: Enabled, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal: true, named: crate::commands::NamedKeys::NONE }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "effects.reorder",
            "Reorder Effect",
            r#"{"clip":id,"from":index,"to":index (the effect at `from` takes the place of the effect at `to`; both must be standard effects, which are the only ones that move)}"#,
            crate::commands::has_seq,
            reorder,
        ),
        spec("effects.moveUp", "Move Effect Up", r#"{"clip":id?,"index":n? (default: the selected effect)}"#, crate::commands::has_seq, |s, p| {
            step(s, p, "effects.moveUp", false)
        }),
        spec("effects.moveDown", "Move Effect Down", r#"{"clip":id?,"index":n? (default: the selected effect)}"#, crate::commands::has_seq, |s, p| {
            step(s, p, "effects.moveDown", true)
        }),
    ]
}

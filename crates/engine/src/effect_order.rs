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
//! Some state names an effect by its index: the selected mask and mask-tracking jobs here, and the
//! frontends' keyframe selection, expanded rows and open editors. A move, and Undo or Redo of one,
//! reports how the indices changed in [`crate::Event::EffectsReordered`], and the engine's own
//! state follows the effect it was on. Each move is recorded with the project snapshots on either
//! side of it ([`Moves`]), so Undo and Redo know exactly which effect went where, whatever is
//! selected then and even between identical copies of an effect.

use std::sync::{Arc, Weak};

use filmcraft_project::{ClipId, EffectInstance, ItemId, Project};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, checked_u32_p};
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
/// of the indices (from [`move_order`]); anything else leaves `effects` alone.
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

/// The order that undoes `order`: entry `j` is the index the effect now at `j` came from.
fn inverse(order: &[usize]) -> Vec<usize> {
    let mut back: Vec<usize> = (0..order.len()).collect();
    for (from, to) in order.iter().enumerate() {
        if let Some(b) = back.get_mut(*to) {
            *b = from;
        }
    }
    back
}

/// One effect move: the project before and after it (as the undo history holds them) and how the
/// clip's effects moved. Weak, so a step the history drops doesn't keep its project alive.
struct Move {
    before: Weak<Project>,
    after: Weak<Project>,
    seq: ItemId,
    clip: ClipId,
    order: Vec<usize>,
}

/// The effect moves Undo and Redo can still reach ([`Session`]'s `effect_moves`).
#[derive(Default)]
pub struct Moves(Vec<Move>);

impl Moves {
    fn record(&mut self, before: &Arc<Project>, after: &Arc<Project>, seq: ItemId, clip: ClipId, order: Vec<usize>) {
        // steps the history let go of (trimmed, cleared, or a replaced project) can't come back
        self.0.retain(|m| m.before.strong_count() > 0 && m.after.strong_count() > 0);
        self.0.push(Move { before: Arc::downgrade(before), after: Arc::downgrade(after), seq, clip, order });
    }

    /// The move from `from` to `to`, if one was recorded between these exact snapshots, in that
    /// direction (a recorded move) or the other (its undo, with the order reversed).
    fn between(&self, from: &Arc<Project>, to: &Arc<Project>) -> Option<(ItemId, ClipId, Vec<usize>)> {
        let is = |w: &Weak<Project>, a: &Arc<Project>| std::ptr::eq(w.as_ptr(), Arc::as_ptr(a));
        self.0.iter().rev().find_map(|m| {
            if is(&m.before, from) && is(&m.after, to) {
                Some((m.seq, m.clip, m.order.clone()))
            } else if is(&m.after, from) && is(&m.before, to) {
                Some((m.seq, m.clip, inverse(&m.order)))
            } else {
                None
            }
        })
    }
}

/// Points the engine's state that names one of `clip`'s effects by index (the selected effect and
/// mask, mask-tracking jobs) at the effect it was on, after the effects (`before`, now `after`)
/// moved as `order` says, and tells the frontends.
fn follow_move(s: &mut Session, clip: ClipId, before: &[EffectInstance], after: &[EffectInstance], order: Vec<usize>) {
    let moved = |i: usize| order.get(i).copied().unwrap_or(i);
    let state: &mut EditorState = &mut s.state;
    if let Some(sel) = state.selected_effect.clone().filter(|e| e.clip == clip)
        && let Some(i) = sel.index(before)
    {
        state.selected_effect = EffectSel::of(clip, after, moved(i));
    }
    if let Some(mask) = state.selected_mask.as_mut().filter(|m| m.clip == clip) {
        mask.effect = moved(mask.effect);
    }
    // a running track keeps writing Mask Path keyframes into the effect it started on
    for job in s.mask_jobs.iter_mut().filter(|j| j.target.clip == clip) {
        job.target.effect = moved(job.target.effect);
    }
    s.events.push(Event::EffectsReordered { clip, order });
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
    // a `clip` that isn't a clip id is an error, not "the selected effect"
    let clip = match p.get("clip").filter(|v| !v.is_null()) {
        Some(v) => Some(v.as_u64().map(ClipId).ok_or_else(|| bad(cmd, "`clip` must be an integer clip id"))?),
        None => None,
    };
    let (clip, from) = match (clip, p.get("index").filter(|v| !v.is_null())) {
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
    let mut stacks = None;
    s.edit_sequence("Reorder Effect", |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let before = it.effects.clone();
        apply_order(&mut it.effects, &order);
        stacks = Some((before, it.effects.clone()));
        Ok(())
    })?;
    if let (Some((_, before_project, _)), Some(seq)) = (s.history.undo.last(), s.state.active_sequence) {
        let (before_project, after_project) = (before_project.clone(), s.project.clone());
        s.effect_moves.record(&before_project, &after_project, seq, clip, order.clone());
    }
    if let Some((before, after)) = stacks {
        follow_move(s, clip, &before, &after, order);
    }
    Ok(answer)
}

/// After Undo or Redo replaced the project `from` with the session's project: when that step was
/// an effect move, the state that names effects by index follows it back (or forward again).
pub(crate) fn follow_history(s: &mut Session, from: &Arc<Project>) {
    let to = s.project.clone();
    let Some((seq, clip, order)) = s.effect_moves.between(from, &to) else { return };
    let stack = |p: &Project| p.sequence(seq).and_then(|q| q.find_item(clip)).map(|(_, it)| it.effects.clone());
    if let (Some(before), Some(after)) = (stack(from), stack(&to)) {
        follow_move(s, clip, &before, &after, order);
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

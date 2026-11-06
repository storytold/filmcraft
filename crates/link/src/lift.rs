//! Effects EffectCraft cannot import stay on the outer clip.
//!
//! EffectCraft imports `motion`, `opacity` and `time_remap`. Every other effect of a nested clip
//! is dropped by its importer, and the result movie then shows the clip without it. When the
//! nest holds one video clip and the nest sits on the timeline as one clip, [`lift_unimported`]
//! moves those effects from the inner clip to the outer clip, so they keep working on the result
//! movie and the payload never names them. Any other shape is ambiguous: nothing moves and
//! EffectCraft reports the drop.
//!
//! Order changes. An effect that ran on the inner clip, before EffectCraft's motion and opacity,
//! now runs on the outer clip, after the whole EffectCraft result.

use filmcraft_engine::project::{ClipId, ItemId, Project, TrackItem};
use filmcraft_engine::{EngineError, Session};

use crate::prompt::Unsupported;

/// Effect ids EffectCraft imports. Mirrors the importer in EffectCraft
/// (`crates/interchange/src/import.rs`, the `motion`, `opacity` and `time_remap` cases).
pub const IMPORTED_BY_EFFECTCRAFT: [&str; 3] = ["motion", "opacity", "time_remap"];

fn imported(effect: &str) -> bool {
    IMPORTED_BY_EFFECTCRAFT.contains(&effect)
}

/// The effects that moved, by display name.
pub type Lifted = Vec<String>;

/// Where the effects go, when the shape is unambiguous.
///
/// - One inner video clip at normal speed that spans the outer clip: its effects move, keyframes and all.
/// - Several inner video clips that all carry the same effects, none animated and none masked, and
///   no two on screen at once: the same grade on every clip is one grade on the outer clip. A clip
///   without it, a different grade, or a time-dependent one is ambiguous. Clips that overlap are too:
///   an effect that runs on each clip before the composite (a key, a blend) gives another picture
///   than the same effect on the composite.
fn plan(project: &Project, active: ItemId, nest: ItemId, outer: &[ClipId]) -> Option<ClipId> {
    let [outer_id] = outer else { return None };
    let inside = project.sequence(nest)?;
    let inner: Vec<&TrackItem> = inside.video_tracks.iter().flat_map(|track| track.items.iter()).collect();
    let first = inner.first()?;
    let grade = |clip: &TrackItem| clip.effects.iter().filter(|e| !imported(&e.effect)).cloned().collect::<Vec<_>>();
    let wanted = grade(first);
    if wanted.is_empty() {
        return None;
    }
    let (_, clip) = project.sequence(active)?.find_item(*outer_id)?;
    if let [only] = inner.as_slice() {
        // The effects keep their clip-relative keyframe times only when both clips cover the same span.
        return (movable(only) && clip.duration == only.duration).then_some(*outer_id);
    }
    let static_grade = wanted.iter().all(|e| !e.is_animated() && e.masks.is_empty());
    (static_grade && inner.iter().all(|c| c.enabled && grade(c) == wanted) && !overlap(&inner)).then_some(*outer_id)
}

/// True when two of `clips` cover a common stretch of the timeline.
fn overlap(clips: &[&TrackItem]) -> bool {
    let mut spans: Vec<_> = clips.iter().map(|c| (c.start, c.end())).collect();
    spans.sort();
    spans.windows(2).any(|pair| matches!(pair, [(_, end), (start, _)] if start < end))
}

/// A clip whose effect time is its timeline time. A speed change, a hold or a time remap moves it.
fn movable(clip: &TrackItem) -> bool {
    clip.speed == 1.0 && !clip.reverse && clip.frame_hold.is_none() && !clip.hold_filters && !clip.effects.iter().any(|e| e.effect == "time_remap")
}

/// Move the effects EffectCraft cannot import from the inner video clips of `nest` to the one outer
/// clip in `active`. Returns their names, or an empty list when nothing moved. Masks go with their
/// effect. One undo step, which `link.editIn` folds into its own.
pub fn lift_unimported(session: &mut Session, active: ItemId, nest: ItemId, outer: &[ClipId]) -> Result<Lifted, EngineError> {
    let Some(target) = plan(&session.project, active, nest, outer) else { return Ok(Vec::new()) };
    session.edit("link.liftEffects", move |project, _state| {
        let inside = project.sequence_mut(nest).ok_or(EngineError::NoSequence)?;
        let mut moved = Vec::new();
        for clip in inside.video_tracks.iter_mut().flat_map(|track| track.items.iter_mut()) {
            let (keep, away): (Vec<_>, Vec<_>) = std::mem::take(&mut clip.effects).into_iter().partition(|e| imported(&e.effect));
            clip.effects = keep;
            // Identical on every clip (plan checked), so the first clip's copy is the outer clip's.
            if moved.is_empty() {
                moved = away;
            }
        }
        let names = moved.iter().map(|e| e.def().map_or_else(|| e.effect.clone(), |def| def.name.to_string())).collect();
        let (_, clip) = project
            .sequence_mut(active)
            .ok_or(EngineError::NoSequence)?
            .find_item_mut(target)
            .ok_or_else(|| EngineError::Other("the linked clip is gone".into()))?;
        clip.effects.extend(moved);
        Ok(names)
    })
}

/// True when EffectCraft reported a dropped effect and nothing else. Older EffectCraft builds send
/// the importer's warning text as `what: "import"`.
pub fn is_effect_drop(part: &Unsupported) -> bool {
    part.what == "effect" || (part.what == "import" && part.name.starts_with("effect \"") && part.name.ends_with("is not imported"))
}

/// The effect name inside an [`is_effect_drop`] part.
pub fn dropped_name(part: &Unsupported) -> String {
    if part.what == "effect" {
        return part.name.clone();
    }
    part.name.strip_prefix("effect \"").and_then(|rest| rest.strip_suffix("\" is not imported")).unwrap_or(&part.name).to_string()
}

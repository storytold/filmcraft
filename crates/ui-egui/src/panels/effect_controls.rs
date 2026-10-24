//! Effect Controls: the selected clip's effects (fixed Motion/Opacity/Time Remapping first, as in
//! Premiere, then standard effects), generated from parameter schemas, with stopwatches and a
//! keyframe lane on the right. Also hosts the Lumetri Color panel body (same editor, grouped).

use egui::{Align2, Color32, Pos2, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::{ClipId, EffectInstance, ParamKind, ParamValue, TrackItem, TrackKind};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::{EffectClipboard, EffectKeyframeClipboard, EffectKeyframeClipboardEntry, EffectKeyframeTemplate, KeyframeRef};
use crate::theme::Tokens;
use crate::widgets::HotNumberOptions;

#[path = "effect_controls_selection.rs"]
mod selection;
pub(crate) use selection::Interaction;

const ROW_H: f32 = 22.0;
/// The narrowest the effect list and the keyframe area can be dragged (#643): the list's rows are
/// laid out for at least 260 points (values from 150 points in, the reset button at the end).
const MIN_LIST_W: f32 = 260.0;
const MIN_LANE_W: f32 = 60.0;
const MIN_ZOOM_THUMB_W: f32 = 34.0;
const MIN_ZOOM_FRAMES: i64 = 5;
/// Keyframe lane header: the time ruler (beside the Source / Sequence pills) and the clip's bar.
const RULER_H: f32 = 24.0;
const CLIP_BAR_H: f32 = 18.0;
const ZOOM_BAR_H: f32 = 10.0;
/// Distance between the keyframe navigator's buttons (◀ ◆ ▶).
const NAV_STEP: f32 = 16.0;
/// The Y curve of a point parameter's value graph (X uses the accent colour).
const Y_CURVE: Color32 = Color32::from_rgb(0x60, 0xc0, 0x80);
/// Properties panel: the keyframe diamonds' column, from a row's right edge (the arrows sit one
/// [`NAV_STEP`] to either side of it, and a section's reset button above it).
const PROPS_NAV_X: f32 = 26.0;
/// Properties panel: where a row's value fields end, left of the navigator.
const PROPS_VALUE_R: f32 = PROPS_NAV_X + NAV_STEP + 12.0;

#[derive(Clone)]
struct EffectDrag {
    clip: u64,
    from: usize,
    target: Option<usize>,
}

/// Timeline time at which the clip shows media time `m` (where a keyframe sits in the sequence).
fn timeline_time_of(it: &TrackItem, m: Tick) -> Tick {
    let offset = if it.reverse { it.source_out() - m - Tick(1) } else { m - it.source_in };
    it.start + Tick((offset.0 as f64 / it.speed.abs().max(1e-6)).round() as i64)
}

/// A possibly UI-supplied range, clamped to the clip. Invalid / stale ranges show the full clip.
fn time_zoom_range(zoom: Option<[f64; 2]>) -> (f64, f64) {
    let Some([start, end]) = zoom.filter(|[start, end]| start.is_finite() && end.is_finite()) else { return (0.0, 1.0) };
    let (start, end) = (start.clamp(0.0, 1.0), end.clamp(0.0, 1.0));
    if start < end { (start, end) } else { (0.0, 1.0) }
}

/// Keep the minimum view width at five frames, including ranges supplied through UI automation or
/// restored from an older UI-state snapshot.
fn constrained_time_zoom(zoom: Option<[f64; 2]>, duration: Tick, rate: filmcraft_time::FrameRate) -> Option<[f64; 2]> {
    let (start, end) = time_zoom_range(zoom);
    let duration = duration.0.max(1);
    let min_span = (rate.frame_duration().0.max(1).saturating_mul(MIN_ZOOM_FRAMES) as f64 / duration as f64).clamp(0.0, 1.0);
    let span = (end - start).max(min_span).min(1.0);
    let start = start.min(1.0 - span);
    let end = start + span;
    (start > 0.0 || end < 1.0).then_some([start, end])
}

fn shown_range(start: Tick, duration: Tick, zoom: Option<[f64; 2]>) -> (Tick, Tick) {
    let (from, to) = time_zoom_range(zoom);
    let duration = duration.0.max(1);
    let at = |fraction: f64| Tick(start.0.saturating_add((fraction * duration as f64).round().clamp(0.0, duration as f64) as i64));
    let shown_start = at(from);
    let shown_end = at(to).max(Tick(shown_start.0.saturating_add(1)));
    (shown_start, shown_end)
}

/// The overview bar under the clip. Its thumb is the part of the clip shown in the lane: drag the
/// middle to scroll and either handle to change the zoom. A full-width thumb shows the whole clip.
fn time_zoom_bar(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, duration: Tick, rate: filmcraft_time::FrameRate) {
    let t = app.tokens;
    let clip_duration = duration;
    let duration = duration.0.max(1);
    let frame = rate.frame_duration().0.max(1);
    let min_span = (frame.saturating_mul(MIN_ZOOM_FRAMES) as f64 / duration as f64).clamp(0.0, 1.0);
    let (start, end) = time_zoom_range(constrained_time_zoom(app.ui.effect_controls_time_zoom, clip_duration, rate));
    let span = end - start;
    let w = (span as f32 * bar.width()).clamp(MIN_ZOOM_THUMB_W.min(bar.width()), bar.width());
    let travel = (bar.width() - w).max(0.0);
    let range = (1.0 - span).max(0.0);
    let left = bar.min.x + if range > 0.0 { (start / range).clamp(0.0, 1.0) as f32 * travel } else { 0.0 };
    let per_point = if travel > 0.5 { range / travel as f64 } else { 0.0 };
    let thumb = Rect::from_min_max(pos2(left, bar.min.y), pos2(left + w, bar.max.y));
    let p = ui.painter();
    p.rect_filled(bar, bar.height() / 2.0, t.separator);
    p.rect_filled(thumb, thumb.height() / 2.0, if ui.rect_contains_pointer(thumb) { t.scroll_thumb_hover } else { t.scroll_thumb });
    for x in [thumb.min.x + thumb.height() / 2.0, thumb.max.x - thumb.height() / 2.0] {
        p.circle_filled(pos2(x, thumb.center().y), 3.5, t.panel_bg);
        p.circle_stroke(pos2(x, thumb.center().y), 3.5, Stroke::new(1.25, t.icon_active));
    }
    app.auto.add("effectControls.zoomBar", thumb, "keyframe zoom range");
    app.auto.add("effectControls.zoomBar.track", bar, "keyframe zoom track");
    let response = ui.interact(bar, egui::Id::unique("effect-controls-zoom-bar"), Sense::click_and_drag());
    let jump = |x: f32| ((x - bar.min.x - w / 2.0) as f64 * per_point).clamp(0.0, range);
    let mut next = None;
    if response.clicked()
        && per_point > 0.0
        && let Some(pos) = response.interact_pointer_pos().filter(|pos| !thumb.contains(*pos))
    {
        let start = jump(pos.x);
        next = Some((start, start + span));
    }
    let drag_id = egui::Id::unique("effect-controls-zoom-drag");
    if response.drag_started()
        && let Some(pos) = ui.input(|i| i.pointer.press_origin()).or(response.interact_pointer_pos())
    {
        let mut drag_start = start;
        let mode = if !thumb.expand2(vec2(0.0, 4.0)).contains(pos) {
            if per_point > 0.0 {
                drag_start = jump(pos.x);
                next = Some((drag_start, drag_start + span));
            }
            0u8
        } else if (pos.x - thumb.min.x).abs() < 9.0 {
            1
        } else if (pos.x - thumb.max.x).abs() < 9.0 {
            2
        } else {
            0
        };
        ui.data_mut(|d| d.insert_temp(drag_id, (pos.x, drag_start, mode)));
    }
    if response.dragged()
        && let Some((grab, drag_start, mode)) = ui.data(|d| d.get_temp::<(f32, f64, u8)>(drag_id))
    {
        match mode {
            0 => {
                if let Some(pos) = response.interact_pointer_pos() {
                    let next_start = (drag_start + (pos.x - grab) as f64 * per_point).clamp(0.0, range);
                    next = Some((next_start, next_start + span));
                }
            }
            1 => {
                let dx = response.drag_delta().x as f64 / bar.width().max(1.0) as f64;
                next = Some(((start + dx).clamp(0.0, (end - min_span).max(0.0)), end));
            }
            _ => {
                let dx = response.drag_delta().x as f64 / bar.width().max(1.0) as f64;
                next = Some((start, (end + dx).clamp((start + min_span).min(1.0), 1.0)));
            }
        }
    }
    if response.drag_stopped() {
        ui.data_mut(|d| d.remove::<(f32, f64, u8)>(drag_id));
    }
    if let Some((start, end)) = next {
        app.ui.effect_controls_time_zoom = (start > 0.0 || end < 1.0).then_some([start, end.min(1.0)]);
    }
}

/// Alt+wheel scales the lane around the timeline time directly under the pointer. The modified
/// wheel is consumed here so it cannot also scroll the effect rows or the surrounding view.
fn zoom_at_pointer(app: &mut FilmcraftApp, ui: &mut egui::Ui, lane: Rect, duration: Tick, rate: filmcraft_time::FrameRate) {
    let Some(pointer) = ui.input(|i| i.pointer.hover_pos()).filter(|p| lane.contains(*p)) else { return };
    let (delta, has_alt) = ui.input(|input| {
        let mut delta = 0.0f32;
        let mut has_alt = false;
        for event in &input.events {
            if let egui::Event::MouseWheel { unit, delta: d, modifiers, .. } = event
                && d.y.is_finite()
                && d.x.is_finite()
            {
                has_alt |= modifiers.alt;
                if modifiers.alt {
                    let scale = match unit {
                        egui::MouseWheelUnit::Point => 1.0,
                        egui::MouseWheelUnit::Line => 40.0,
                        egui::MouseWheelUnit::Page => 400.0,
                    };
                    delta += d.y * scale;
                }
            }
        }
        (delta, has_alt)
    });
    if !has_alt {
        return;
    }
    // ScrollArea reads the aggregated deltas, while other panel gestures can inspect raw events.
    // Remove Alt-wheel events and rebuild both deltas from any ordinary wheel events left in this
    // frame, so zooming cannot also move the Timeline or a surrounding scroll view.
    ui.input_mut(|input| {
        let mut remaining = egui::Vec2::ZERO;
        input.raw.events.retain(|event| match event {
            egui::Event::MouseWheel { modifiers, .. } if modifiers.alt => false,
            egui::Event::MouseWheel { unit, delta, .. } if delta.x.is_finite() && delta.y.is_finite() => {
                let scale = match unit {
                    egui::MouseWheelUnit::Point => 1.0,
                    egui::MouseWheelUnit::Line => 40.0,
                    egui::MouseWheelUnit::Page => 400.0,
                };
                remaining += *delta * scale;
                true
            }
            _ => true,
        });
        input.smooth_scroll_delta = remaining;
    });
    if delta.abs() < 0.01 {
        return;
    }
    let (start, end) = time_zoom_range(app.ui.effect_controls_time_zoom);
    let span = end - start;
    let under_pointer = ((pointer.x - lane.min.x) / lane.width().max(1.0)).clamp(0.0, 1.0) as f64;
    let anchor = start + under_pointer * span;
    // A positive wheel delta is content moving down: this is a wheel-down gesture, so it widens
    // the view; wheel-up has a negative delta and narrows it (zooms in).
    let factor = f64::from(-delta / 300.0).exp().clamp(0.1, 10.0);
    let clip_ticks = duration.0.max(1);
    let min_span = (rate.frame_duration().0.max(1).saturating_mul(MIN_ZOOM_FRAMES) as f64 / clip_ticks as f64).clamp(0.0, 1.0);
    let new_span = (span * factor).clamp(min_span, 1.0);
    let new_start = (anchor - under_pointer * new_span).clamp(0.0, 1.0 - new_span);
    app.ui.effect_controls_time_zoom = (new_start > 0.0 || new_start + new_span < 1.0).then_some([new_start, new_start + new_span]);
    ui.input_mut(|input| input.smooth_scroll_delta = egui::Vec2::ZERO);
}

// The press frame can include movement before the button went down. Start from the press
// origin so that only gesture motion enters the preview, including same-frame movement.
fn graph_drag_offset(ui: &egui::Ui, response: &egui::Response, previous: egui::Vec2) -> egui::Vec2 {
    if response.drag_started_by(egui::PointerButton::Primary) {
        response
            .interact_pointer_pos()
            .zip(ui.input(|i| i.pointer.press_origin()))
            .map_or_else(|| response.drag_delta(), |(position, origin)| position - origin)
    } else {
        previous + response.drag_delta()
    }
}

/// Whether `k` names a keyframe of the clip `it` (its effect, parameter and media time).
fn keyframe_exists(it: &TrackItem, k: &KeyframeRef) -> bool {
    let Some(e) = it.effects.get(k.effect) else { return false };
    let param = match k.mask {
        Some(m) => e.masks.get(m).and_then(|m| m.param(&k.param)),
        None => e.params.get(&k.param),
    };
    param.is_some_and(|p| p.keyframes.iter().any(|kf| kf.time == k.time))
}

/// Edit ▸ Select All (⌘A) with Effect Controls focused selects every keyframe the panel shows
/// instead of the Timeline's clips (#1241). False when the panel is not focused or shows none.
pub(crate) fn select_all_keyframes(app: &mut FilmcraftApp) -> bool {
    if app.ui.focused != crate::dock::PanelKind::EffectControls {
        return false;
    }
    let mut keys = Vec::new();
    for (clip, it, _) in selected_clips(app) {
        for (effect, e) in it.effects.iter().enumerate() {
            let key = |param: &str, mask, time| KeyframeRef { clip: clip.0, effect, param: param.to_string(), mask, time };
            for (param, p) in &e.params {
                keys.extend(p.keyframes.iter().map(|kf| key(param, None, kf.time)));
            }
            for (m, mask) in e.masks.iter().enumerate() {
                for param in filmcraft_project::mask::MASK_PARAMS {
                    keys.extend(mask.param(param).into_iter().flat_map(|p| &p.keyframes).map(|kf| key(param, Some(m), kf.time)));
                }
            }
        }
    }
    if keys.is_empty() {
        return false;
    }
    app.ui.keyframe_selection = keys;
    true
}

fn selected_clip(app: &FilmcraftApp) -> Option<(ClipId, TrackItem, TrackKind)> {
    selected_clips(app).into_iter().next()
}

/// The selected clip (video first) and the audio linked to it when that audio is selected or
/// linked selection is on — so clip audio FX (Parametric EQ, Custom Setup) stay visible on A/V pairs.
fn selected_clips(app: &FilmcraftApp) -> Vec<(ClipId, TrackItem, TrackKind)> {
    let Some(seq) = app.session.active_sequence() else { return Vec::new() };
    let picked: Vec<(ClipId, &TrackItem, TrackKind)> = app
        .session
        .state
        .selection
        .iter()
        .filter_map(|c| {
            let (tid, it) = seq.find_item(*c)?;
            Some((*c, it, seq.track(tid)?.kind))
        })
        .collect();
    let Some(&(clip, it, kind)) = picked.iter().find(|p| p.2 == TrackKind::Video).or(picked.first()) else { return Vec::new() };
    let mut out = vec![(clip, it.clone(), kind)];
    if kind == TrackKind::Video {
        let linked = picked.iter().find(|p| p.2 == TrackKind::Audio && p.1.link.is_some() && p.1.link == it.link).copied().or_else(|| {
            if !app.session.state.linked_selection {
                return None;
            }
            let link = it.link?;
            seq.audio_tracks.iter().find_map(|t| t.items.iter().find(|i| i.link == Some(link)).map(|i| (i.id, i, TrackKind::Audio)))
        });
        if let Some((ac, ai, ak)) = linked {
            out.push((ac, ai.clone(), ak));
        }
    }
    out
}

/// An effect (or preset) dragged from the Effects panel and released over this panel is applied to
/// the clip shown here, so it need not be aimed at the clip in the Timeline (#2326). Transitions
/// need an edge, so they stay Timeline drops.
fn drop_effect(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, clips: &[(ClipId, TrackItem, TrackKind)]) {
    let Some(effect) = crate::panels::dragged_effect(ui) else { return };
    let is_transition = filmcraft_project::find_effect(&effect)
        .is_some_and(|d| matches!(d.kind, filmcraft_project::EffectKind::VideoTransition | filmcraft_project::EffectKind::AudioTransition));
    let ctx = ui.ctx().clone();
    let over = ctx.pointer_hover_pos().is_some_and(|p| rect.contains(p) && ctx.layer_id_at(p) == Some(ui.layer_id()));
    if is_transition || !over {
        return;
    }
    app.auto.add("effectControls.dropTarget", rect, "drop an effect here");
    ui.painter().rect_stroke(rect.shrink(1.0), 3.0, Stroke::new(2.0, app.tokens.accent), egui::StrokeKind::Inside);
    if !ctx.input(|i| i.pointer.any_released()) {
        return;
    }
    // the video clip first; its linked audio clip takes audio effects the video clip refuses
    let mut result = Err(filmcraft_engine::EngineError::NoSequence);
    for (clip, _, _) in clips {
        let params_clips = json!([clip.0]);
        result = if let Some(name) = effect.strip_prefix("preset:") {
            app.session.execute("presets.apply", json!({"preset": name, "clips": params_clips}))
        } else if let Some(name) = effect.strip_prefix(crate::panels::LUMETRI_PRESET_PREFIX) {
            app.session.execute("lumetri.applyPreset", json!({"name": name, "clips": params_clips}))
        } else {
            app.session.execute("effects.apply", json!({"effect": effect, "clips": params_clips}))
        };
        if result.is_ok() {
            break;
        }
    }
    if let Err(e) = result {
        app.ui.status = e.to_string();
    }
    crate::panels::clear_drag(ui);
}

fn selected_effect_indices(app: &FilmcraftApp, clip: ClipId, effects: &[EffectInstance]) -> Vec<usize> {
    let mut indices: Vec<usize> =
        app.ui.selected_effects.iter().filter(|selection| selection.clip == clip).filter_map(|selection| selection.index(effects)).collect();
    indices.sort_unstable();
    indices.dedup();
    indices
}

fn removable_effect_indices(app: &FilmcraftApp, clip: ClipId, effects: &[EffectInstance], clicked: Option<usize>) -> Vec<usize> {
    let mut indices = selected_effect_indices(app, clip, effects);
    if let Some(clicked) = clicked
        && !indices.contains(&clicked)
    {
        indices = vec![clicked];
    }
    indices.retain(|index| effects.get(*index).is_some_and(|effect| effect.def().is_some_and(|definition| !definition.intrinsic)));
    indices
}

fn effect_clipboard_size(effect: &EffectInstance) -> (usize, usize) {
    let mut keys = 0usize;
    let mut vertices = 0usize;
    for param in
        effect.params.values().chain(effect.masks.iter().flat_map(|mask| filmcraft_project::mask::MASK_PARAMS.into_iter().filter_map(|id| mask.param(id))))
    {
        keys = keys.saturating_add(param.keyframes.len());
        if let ParamValue::Path(path) = &param.value {
            vertices = vertices.saturating_add(path.vertices.len());
        }
        for keyframe in &param.keyframes {
            if let ParamValue::Path(path) = &keyframe.value {
                vertices = vertices.saturating_add(path.vertices.len());
            }
        }
    }
    (keys, vertices)
}

fn copy_effects(app: &mut FilmcraftApp, clip: ClipId, item: &TrackItem, clicked: usize) {
    let mut indices = selected_effect_indices(app, clip, &item.effects);
    if !indices.contains(&clicked) {
        indices = vec![clicked];
    }
    if indices.len() > 64 {
        app.ui.status = tl!("Select no more than 64 effects to copy").to_string();
        return;
    }
    let mut copied_keys = 0usize;
    let mut copied_vertices = 0usize;
    for index in &indices {
        if let Some(effect) = item.effects.get(*index) {
            let (keys, vertices) = effect_clipboard_size(effect);
            copied_keys = copied_keys.saturating_add(keys);
            copied_vertices = copied_vertices.saturating_add(vertices);
        }
    }
    if copied_keys > 100_000 || copied_vertices > 16_384 {
        app.ui.status = tl!("The selected effects contain too much animation or path data to copy").to_string();
        return;
    }
    let effects: Vec<EffectInstance> = indices.iter().filter_map(|index| item.effects.get(*index)).cloned().collect();
    if effects.is_empty()
        || effects.iter().any(|effect| effect.def().is_none_or(|def| def.intrinsic || filmcraft_project::graphic::is_layer(effect) || effect.essential))
    {
        app.ui.status = tl!("Only standard effects can be copied").to_string();
        return;
    }
    app.ui.effect_clipboard = EffectClipboard { source_clip: clip.0, source_in: item.source_in, effects };
    app.ui.effect_controls_clipboard_kind = Some(crate::state::EffectControlsClipboardKind::Effects);
    app.ui.status = tlf!("Copied {count} effect(s)", count = indices.len());
}

fn paste_effects(app: &mut FilmcraftApp, current_clip: ClipId) {
    let clipboard = app.ui.effect_clipboard.clone();
    if clipboard.effects.is_empty() {
        app.ui.status = tl!("No effects have been copied").to_string();
        return;
    }
    let Some(first) = clipboard.effects.first().and_then(EffectInstance::def) else {
        app.ui.status = tl!("The copied effects are unavailable").to_string();
        return;
    };
    let audio = first.kind == filmcraft_project::EffectKind::Audio;
    if clipboard.effects.iter().any(|effect| effect.def().is_none_or(|def| (def.kind == filmcraft_project::EffectKind::Audio) != audio)) {
        app.ui.status = tl!("Audio and video effects cannot be pasted as one chain").to_string();
        return;
    }
    let candidates: Vec<ClipId> = if app.session.state.selection.is_empty() { vec![current_clip] } else { app.session.state.selection.clone() };
    let clips: Vec<u64> = app
        .session
        .active_sequence()
        .into_iter()
        .flat_map(|sequence| {
            candidates.iter().filter_map(move |clip| {
                let (track, _) = sequence.find_item(*clip)?;
                let kind = sequence.track(track)?.kind;
                ((kind == TrackKind::Audio) == audio).then_some(clip.0)
            })
        })
        .collect();
    if clips.is_empty() {
        app.ui.status = tl!("No selected clips match the copied effects").to_string();
        return;
    }
    let result = app.session.execute(
        "effects.paste",
        json!({"sourceClip": clipboard.source_clip, "sourceIn": clipboard.source_in.0, "effects": clipboard.effects, "clips": clips}),
    );
    match result {
        Ok(value) => app.ui.status = tlf!("Pasted {count} effect(s)", count = value["inserted"].as_u64().unwrap_or(0)),
        Err(error) => app.ui.status = error.to_string(),
    }
}

fn copy_keyframes(app: &mut FilmcraftApp) -> Result<(), String> {
    let selected = app.ui.keyframe_selection.clone();
    if selected.is_empty() {
        return Err(tl!("Select keyframes to copy").to_string());
    }
    if selected.len() > 4096 {
        return Err(tl!("Too many keyframes to copy").to_string());
    }
    let sequence = app.session.active_sequence().ok_or_else(|| tl!("No active sequence").to_string())?;
    let playhead = app.session.playhead();
    let mut clipboard = EffectKeyframeClipboard::default();
    let mut path_vertices = 0usize;
    for key in selected {
        let (_, item) = sequence.find_item(ClipId(key.clip)).ok_or_else(|| tl!("A selected clip no longer exists").to_string())?;
        let effect = item.effects.get(key.effect).ok_or_else(|| tl!("A selected effect no longer exists").to_string())?;
        let param = match key.mask {
            Some(mask) => effect.masks.get(mask).and_then(|mask| mask.param(&key.param)),
            None => effect.params.get(&key.param),
        }
        .ok_or_else(|| tl!("A selected parameter no longer exists").to_string())?;
        let keyframe = param.keyframes.iter().find(|kf| kf.time == key.time).cloned().ok_or_else(|| tl!("A selected keyframe no longer exists").to_string())?;
        if let ParamValue::Path(path) = &keyframe.value {
            path_vertices = path_vertices.saturating_add(path.vertices.len());
            if path_vertices > 16_384 {
                return Err(tl!("The selected keys contain too much path data to copy").to_string());
            }
        }
        let timeline = timeline_time_of(item, key.time);
        let offset = Tick(timeline.0.checked_sub(playhead.0).ok_or_else(|| tl!("Keyframe offset overflows").to_string())?);
        let instance = item.effects.iter().take(key.effect).filter(|previous| previous.effect == effect.effect).count();
        if !clipboard.effects.iter().any(|template| template.effect == effect.effect && template.instance == instance) {
            let mut template = effect.def().ok_or_else(|| tl!("The selected effect is unavailable").to_string())?.instance();
            template.enabled = effect.enabled;
            template.masks = effect
                .masks
                .iter()
                .map(|source| {
                    let mut mask = source.clone();
                    mask.path.keyframes.clear();
                    mask.feather = filmcraft_project::Param::new(ParamValue::Float(10.0));
                    mask.opacity = filmcraft_project::Param::new(ParamValue::Float(100.0));
                    mask.expansion = filmcraft_project::Param::new(ParamValue::Float(0.0));
                    mask
                })
                .collect();
            path_vertices = template.masks.iter().fold(path_vertices, |total, mask| match &mask.path.value {
                ParamValue::Path(path) => total.saturating_add(path.vertices.len()),
                _ => total,
            });
            if path_vertices > 16_384 {
                return Err(tl!("The selected keys contain too much path data to copy").to_string());
            }
            clipboard.effects.push(EffectKeyframeTemplate { effect: effect.effect.clone(), instance, template });
        }
        clipboard.entries.push(EffectKeyframeClipboardEntry { effect: effect.effect.clone(), instance, param: key.param, mask: key.mask, offset, keyframe });
    }
    app.ui.effect_keyframe_clipboard = clipboard;
    app.ui.effect_controls_clipboard_kind = Some(crate::state::EffectControlsClipboardKind::Keyframes);
    app.ui.status = tlf!("Copied {count} keyframe(s)", count = app.ui.effect_keyframe_clipboard.entries.len());
    Ok(())
}

fn paste_keyframes(app: &mut FilmcraftApp, current_clip: ClipId) {
    let clipboard = app.ui.effect_keyframe_clipboard.clone();
    if clipboard.entries.is_empty() {
        app.ui.status = tl!("No keyframes have been copied").to_string();
        return;
    }
    let target = current_clip;
    let result = app.session.execute(
        "effects.pasteKeyframes",
        json!({"clip": target.0, "anchor": app.session.playhead().0, "keyframes": clipboard.entries, "effects": clipboard.effects}),
    );
    match result {
        Ok(result) => match serde_json::from_value(result["keyframes"].clone()) {
            Ok(keys) => app.ui.keyframe_selection = keys,
            Err(error) => app.ui.status = format!("Could not select pasted keyframes: {error}"),
        },
        Err(error) => app.ui.status = error.to_string(),
    }
}

pub(crate) fn copy_for_shortcut(app: &mut FilmcraftApp) {
    if !app.ui.keyframe_selection.is_empty() {
        if let Err(error) = copy_keyframes(app) {
            app.ui.status = error;
        }
        return;
    }
    copy_selected_effects(app);
}

pub(crate) fn copy_selected_effects(app: &mut FilmcraftApp) {
    let Some((clip, item, _)) = selected_clip(app) else { return };
    let selected = selected_effect_indices(app, clip, &item.effects);
    let fallback = app.session.state.selected_effect.as_ref().filter(|selection| selection.clip == clip).and_then(|selection| selection.index(&item.effects));
    if let Some(index) = selected.first().copied().or(fallback) {
        copy_effects(app, clip, &item, index);
    }
}

pub(crate) fn can_paste_for_shortcut(app: &FilmcraftApp) -> bool {
    !app.session.state.selection.is_empty()
        && match app.ui.effect_controls_clipboard_kind {
            Some(crate::state::EffectControlsClipboardKind::Keyframes) => !app.ui.effect_keyframe_clipboard.entries.is_empty(),
            Some(crate::state::EffectControlsClipboardKind::Effects) => !app.ui.effect_clipboard.effects.is_empty(),
            None => false,
        }
}

pub(crate) fn delete_for_shortcut(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if !app.ui.keyframe_selection.is_empty() {
        run(app, ctx, vec![("effects.deleteKeyframes".into(), json!({"keyframes": app.ui.keyframe_selection}))]);
        return;
    }
    let Some((clip, item, _)) = selected_clip(app) else { return };
    let mut indices = removable_effect_indices(app, clip, &item.effects, None);
    if indices.is_empty()
        && let Some(index) =
            app.session.state.selected_effect.as_ref().filter(|selection| selection.clip == clip).and_then(|selection| selection.index(&item.effects))
    {
        indices = removable_effect_indices(app, clip, &item.effects, Some(index));
    }
    if !indices.is_empty() {
        run(app, ctx, vec![("effects.removeMany".into(), json!({"clip": clip.0, "indices": indices}))]);
    }
}

pub(crate) fn paste_for_shortcut(app: &mut FilmcraftApp) {
    let Some((clip, _, _)) = selected_clip(app) else { return };
    match app.ui.effect_controls_clipboard_kind {
        Some(crate::state::EffectControlsClipboardKind::Keyframes) if !app.ui.effect_keyframe_clipboard.entries.is_empty() => paste_keyframes(app, clip),
        Some(crate::state::EffectControlsClipboardKind::Effects) if !app.ui.effect_clipboard.effects.is_empty() => paste_effects(app, clip),
        _ if !app.ui.effect_keyframe_clipboard.entries.is_empty() => paste_keyframes(app, clip),
        _ => paste_effects(app, clip),
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let clips = selected_clips(app);
    app.ui.selected_effects.retain(|selected| clips.iter().any(|(clip, item, _)| selected.clip == *clip && selected.index(&item.effects).is_some()));
    // keep only selected keyframes that still exist on a clip shown here
    app.ui.keyframe_selection.retain(|k| clips.iter().any(|(c, it, _)| c.0 == k.clip && keyframe_exists(it, k)));
    let Some((clip, it, _)) = clips.first().cloned() else {
        // a transition clicked in the Timeline (#430)
        if let Some(id) = crate::panels::transition_controls::selected(app) {
            crate::panels::transition_controls::effect_controls(app, ui, rect, id);
            return;
        }
        crate::dock::placeholder(ui, rect, &t, tl!("(no clip selected)"));
        return;
    };
    drop_effect(app, ui, rect, &clips);
    // Keep the immutable project snapshot alive while UI commands mutate the session.
    // Cloning a sequence here copies every clip and effect on every redraw.
    let project = app.session.project.clone();
    let Some(seq) = app.session.state.active_sequence.and_then(|id| project.sequence(id)) else {
        crate::dock::placeholder(ui, rect, &t, tl!("(no sequences)"));
        return;
    };
    // the divider between the effect list and the keyframe area; dragging it sets the list's width
    // (#643), each side keeping a minimum width
    // (the keyframe area has 4 points of margin left of it and 6 right)
    let panel_width = rect.width().max(0.0);
    let max_list = (panel_width - MIN_LANE_W - 10.0).max(MIN_LIST_W);
    let list_w = if panel_width >= MIN_LIST_W + MIN_LANE_W + 10.0 {
        let preferred = if app.ui.effect_controls_split > 0.0 { app.ui.effect_controls_split } else { (panel_width * 0.58).max(MIN_LIST_W) };
        preferred.clamp(MIN_LIST_W, max_list)
    } else {
        panel_width * 0.58
    };
    let split = rect.min.x + list_w;
    let head = Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2((split - rect.min.x - 12.0).max(0.0), 24.0));
    // Premiere: two pill tabs — "Source · clip" and "Sequence · clip" (active)
    let pill = |ui: &mut egui::Ui, r: Rect, text: &str, active: bool| {
        ui.painter().rect_filled(r, 4.0, if active { t.pill_active_bg } else { t.panel_bg });
        if !active {
            ui.painter().rect_stroke(r, 4.0, Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
        }
        let cp = ui.painter().with_clip_rect(r.shrink(2.0));
        cp.text(pos2(r.min.x + 8.0, r.center().y), Align2::LEFT_CENTER, text, Tokens::ui(11.5), if active { t.text } else { t.text_dim });
    };
    let half = (head.width() - 6.0).max(0.0) / 2.0;
    pill(ui, Rect::from_min_size(head.min, vec2(half, 24.0)), &tlf!("Source · {name}", name = it.name), false);
    pill(ui, Rect::from_min_size(head.min + vec2(half + 6.0, 0.0), vec2(half, 24.0)), &format!("{} · {}", seq_name(app), it.name), true);
    // keyframe lane header, as Premiere's: a time ruler over the clip's stretch of the sequence,
    // which the playhead's handle rides on, and the clip's bar under it
    let lane_min = pos2((split + 4.0).min(rect.max.x), (rect.min.y + 4.0).min(rect.max.y));
    let lane_max = pos2((rect.max.x - 6.0).max(lane_min.x), (rect.max.y - 26.0).max(lane_min.y));
    let lane = Rect::from_min_max(lane_min, lane_max);
    ui.painter().rect_filled(lane, 0.0, t.tl_bg);
    let ph = app.session.playhead();
    let rate = seq.settings.frame_rate;
    app.ui.effect_controls_time_zoom = constrained_time_zoom(app.ui.effect_controls_time_zoom, it.duration, rate);
    zoom_at_pointer(app, ui, lane, it.duration, rate);
    let (shown_start, shown_end) = shown_range(it.start, it.duration, app.ui.effect_controls_time_zoom);
    let shown_duration = shown_end.0.saturating_sub(shown_start.0).max(1) as f64;
    let lx = |tk: Tick| -> f32 { lane.min.x + (((tk.0.saturating_sub(shown_start.0) as f64 / shown_duration) as f32).clamp(0.0, 1.0)) * lane.width() };
    let ruler = Rect::from_min_max(lane.min, pos2(lane.max.x, (lane.min.y + RULER_H).min(lane.max.y)));
    let bar_top = (ruler.max.y + 4.0).min(lane.max.y);
    let bar = Rect::from_min_max(pos2(lane.min.x, bar_top), pos2(lane.max.x, (bar_top + CLIP_BAR_H).min(lane.max.y)));
    // Keep navigation controls below the keyframe rows; only the ruler and clip overview remain
    // fixed at the top of the lane.
    let zoom_bar_bottom = (lane.max.y - 1.0).max(lane.min.y);
    let zoom_bar_top = (zoom_bar_bottom - ZOOM_BAR_H).max(lane.min.y);
    let zoom_bar = Rect::from_min_max(pos2(lane.min.x, zoom_bar_top), pos2(lane.max.x, zoom_bar_bottom));
    let scrub = Rect::from_min_max(ruler.min, bar.max);
    app.auto.add("effectControls.lane", lane, "keyframe lane");
    app.auto.add("effectControls.ruler", scrub, "time ruler");
    let body_min = pos2(rect.min.x, (head.max.y + 4.0).min(rect.max.y));
    let body_max = pos2(split.max(rect.min.x).min(rect.max.x), (rect.max.y - 26.0).max(body_min.y).min(rect.max.y));
    let body = Rect::from_min_max(body_min, body_max);
    let mut actions: Vec<(String, Value)> = Vec::new();
    let drag_id = egui::Id::unique("effect-controls-effect-drag");
    let mut effect_drag = ui.data(|d| d.get_temp::<EffectDrag>(drag_id));
    if let Some(drag) = effect_drag.as_mut() {
        drag.target = None;
    }
    let mut reorder = None;
    let selection_top = (bar.max.y + 2.0).min(zoom_bar.min.y);
    let selection_area =
        Rect::from_min_max(pos2(lane.min.x, selection_top), pos2(lane.max.x, (zoom_bar.min.y - 2.0).max(selection_top))).intersect(ui.clip_rect());
    let mut keys = Interaction::begin(app, ui, selection_area, clip.0, (shown_start, shown_end));
    // the rows span the list and the keyframe area: the wheel scrolls anywhere over them and the
    // scrollbar sits at the panel's right edge (#643)
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_max(body.min, pos2(lane.max.x, body.max.y))).id_salt("ec-body"));
    bui.set_clip_rect(Rect::from_min_max(body.min, pos2(rect.max.x, body.max.y)));
    let scroll_out = egui::ScrollArea::vertical().id_salt("ec-scroll").auto_shrink([false, false]).show(&mut bui, |bui| {
        for (clip, it, kind) in &clips {
            let clip = *clip;
            let (hr, _) = bui.allocate_exact_size(vec2(body.width(), 18.0), Sense::hover());
            let heading = if *kind == TrackKind::Video { tl!("Video") } else { tl!("Audio") };
            bui.painter().text(pos2(hr.min.x + 8.0, hr.center().y), Align2::LEFT_CENTER, heading, Tokens::semibold(11.5), t.text_dim);
            // a graphic with Responsive Design – Time: its clip effects below keep media time (#3361)
            if *kind == TrackKind::Video && it.graphic.as_deref().is_some_and(|g| g.has_responsive_time()) {
                let hint = tl!("Responsive Design – Time remaps the graphic's layer keyframes; clip effects like Opacity and Motion keep media time");
                let ir = Rect::from_center_size(pos2(hr.max.x - 14.0, hr.center().y), vec2(18.0, 18.0));
                // a small button: keyboard-focusable, with an accessible name, tooltip on hover or focus
                let resp = bui.put(ir, egui::Button::new("").frame(false));
                icons::paint(bui.painter(), ir.shrink(2.0), Icon::Info, t.text_dim);
                if resp.ctx.has_accesskit_node(resp.id) {
                    resp.ctx.accesskit_node_builder(resp.id, |node| node.set_label(hint));
                }
                if resp.has_focus() {
                    resp.show_tooltip_text(hint);
                } else {
                    resp.on_hover_text(hint);
                }
                app.auto.add("effectControls.graphicTimeHint", ir, hint);
            }
            let mt_now = it.effect_time_at(ph.clamp(it.start, (it.end() - Tick(1)).max(it.start)));
            // Premiere lists the fixed effects (Motion, Opacity, Time Remapping / Volume…) first.
            let mut order: Vec<usize> = (0..it.effects.len()).collect();
            order.sort_by_key(|i| !it.effects[*i].def().is_some_and(|d| d.intrinsic));
            let mut after_fixed = false;
            for idx in order {
                let e = &it.effects[idx];
                let Some(def) = e.def() else { continue };
                // the first effect added from the library after the fixed ones gets a line on top
                // (#2328); drawn on its edge, so no row moves
                let first_added = if def.intrinsic {
                    after_fixed = true;
                    false
                } else {
                    std::mem::take(&mut after_fixed)
                };
                let key = format!("{}:{}", clip.0, idx);
                let open = !app.ui.collapsed_fx.contains(&key);
                let identity = filmcraft_engine::EffectSel::of(clip, &it.effects, idx);
                let selected = app.ui.selected_effects.iter().any(|s| Some(s) == identity.as_ref())
                    || (app.ui.selected_effects.is_empty()
                        && app.session.state.selected_effect.as_ref().is_some_and(|s| s.clip == clip && s.index(&it.effects) == Some(idx)));
                let (r, resp) = bui.allocate_exact_size(vec2(body.width(), ROW_H), Sense::click_and_drag());
                let resp = if def.intrinsic { resp } else { resp.on_hover_cursor(egui::CursorIcon::Grab) };
                if selected {
                    bui.painter().rect_filled(crate::panels::effect_controls::row_fill(r), 0.0, t.row_selected);
                } else if resp.hovered() {
                    bui.painter().rect_filled(crate::panels::effect_controls::row_fill(r), 0.0, t.hover);
                }
                row_line(bui, r, &lane, &t);
                if first_added {
                    let line = Rect::from_min_max(pos2(r.min.x, r.min.y - 1.0), pos2(r.max.x, r.min.y + 1.0));
                    bui.painter().rect_filled(line, 0.0, t.accent.gamma_multiply(0.8));
                    let which = if *kind == TrackKind::Video { "video" } else { "audio" };
                    app.auto.add(&format!("effectControls.addedEffects.{which}"), line, "Added effects");
                }
                // only the arrow twirls the effect open or shut; the rest of the header selects it
                let tw = Rect::from_center_size(pos2(r.min.x + 10.0, r.center().y), vec2(10.0, 10.0));
                icons::paint(bui.painter(), tw, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_dim);
                let twresp = bui.interact(tw.expand(3.0), egui::Id::unique(("fx-twirl", clip.0, idx)), Sense::click());
                app.auto.add(&format!("effectControls.effect.{}.twirl", e.effect), tw, "twirl");
                // fx enable toggle
                let fxr = Rect::from_center_size(pos2(r.min.x + 28.0, r.center().y), vec2(18.0, 14.0));
                let fxresp = bui.interact(fxr, egui::Id::unique(("fxen", clip.0, idx)), Sense::click());
                bui.painter().text(fxr.center(), Align2::CENTER_CENTER, "fx", Tokens::semibold(10.5), if e.enabled { t.text } else { t.text_faint });
                if !e.enabled {
                    bui.painter().line_segment([fxr.left_bottom(), fxr.right_top()], Stroke::new(1.0, t.text_faint));
                }
                if fxresp.clicked() {
                    actions.push(("effects.toggleEnabled".into(), json!({"clip": clip.0, "index": idx})));
                }
                // (the reset button sits at the row's right end)
                row_label(bui, pos2(r.min.x + 42.0, r.center().y), crate::i18n::t(def.name), r.max.x - 26.0 - (r.min.x + 42.0), Tokens::ui(12.0), t.text);
                // reset button
                let rr = Rect::from_center_size(pos2(r.max.x - 14.0, r.center().y), vec2(16.0, 16.0));
                let rresp = bui.interact(rr, egui::Id::unique(("fxreset", clip.0, idx)), Sense::click()).on_hover_text(tl!("Reset Effect"));
                icons::paint(bui.painter(), rr.shrink(2.0), Icon::Reset, if rresp.hovered() { t.text } else { t.text_dim });
                if rresp.clicked() {
                    actions.push(("effects.reset".into(), json!({"clip": clip.0, "index": idx})));
                }
                app.auto.add(&format!("effectControls.effect.{}", e.effect), r, def.name);
                if twresp.clicked() {
                    if open {
                        app.ui.collapsed_fx.push(key.clone());
                    } else {
                        app.ui.collapsed_fx.retain(|k| *k != key);
                    }
                } else if resp.clicked() && !fxresp.clicked() && !rresp.clicked() {
                    let reference = identity.clone();
                    if bui.input(|i| i.modifiers.shift) {
                        if let Some(reference) = reference {
                            if let Some(at) = app.ui.selected_effects.iter().position(|s| *s == reference) {
                                app.ui.selected_effects.remove(at);
                            } else {
                                app.ui.selected_effects.push(reference);
                            }
                        }
                    } else {
                        app.ui.selected_effects = reference.into_iter().collect();
                    }
                    actions.push(("effects.select".into(), json!({"clip": clip.0, "effect": idx})));
                }
                if resp.drag_started() && !def.intrinsic {
                    effect_drag = Some(EffectDrag { clip: clip.0, from: idx, target: Some(idx) });
                    bui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
                }
                if let Some(drag) = effect_drag.as_mut() {
                    if drag.clip == clip.0 && resp.hovered() && def.intrinsic {
                        drag.target = None;
                    } else if drag.clip == clip.0 && !def.intrinsic && resp.hovered() {
                        let below = bui.input(|i| i.pointer.hover_pos().is_some_and(|p| p.y > r.center().y));
                        let insertion = if below { idx.saturating_add(1) } else { idx };
                        let to = if drag.from < insertion { insertion.saturating_sub(1) } else { insertion };
                        drag.target = Some(to.min(it.effects.len().saturating_sub(1)));
                        let y = if below { r.max.y } else { r.min.y };
                        bui.painter().line_segment([pos2(r.min.x, y), pos2(r.max.x, y)], Stroke::new(2.0, t.accent));
                    }
                }
                let mut save_preset = false;
                let mut copy_effect = false;
                let mut paste_effect = false;
                let fx_id = e.effect.clone();
                resp.context_menu(|ui| {
                    if !def.intrinsic && !filmcraft_project::graphic::is_layer(e) {
                        let sp = ui.button(tl!("Save Preset…"));
                        app.auto.add(&format!("effectControls.effect.{fx_id}.savePreset"), sp.rect, "Save Preset…");
                        if sp.clicked() {
                            save_preset = true;
                            ui.close();
                        }
                        let cp = ui.button(tl!("Copy Effects"));
                        app.auto.add(&format!("effectControls.effect.{fx_id}.copy"), cp.rect, "Copy Effects");
                        if cp.clicked() {
                            copy_effect = true;
                            ui.close();
                        }
                    }
                    let ps = ui.button(tl!("Paste Effects"));
                    app.auto.add(&format!("effectControls.effect.{fx_id}.paste"), ps.rect, "Paste Effects");
                    if ps.clicked() {
                        paste_effect = true;
                        ui.close();
                    }
                    if !def.intrinsic {
                        let clear = ui.button(tl!("Clear"));
                        app.auto.add(&format!("effectControls.effect.{fx_id}.clear"), clear.rect, "Clear");
                        if clear.clicked() {
                            let indices = removable_effect_indices(app, clip, &it.effects, Some(idx));
                            if !indices.is_empty() {
                                actions.push(("effects.removeMany".into(), json!({"clip": clip.0, "indices": indices})));
                            }
                            ui.close();
                        }
                    }
                });
                if save_preset {
                    let selected = selected_effect_indices(app, clip, &it.effects);
                    let effects = if selected.contains(&idx) { selected } else { vec![idx] };
                    let effects: Vec<usize> = effects
                        .into_iter()
                        .filter(|index| {
                            it.effects.get(*index).is_some_and(|effect| {
                                effect.def().is_some_and(|definition| !definition.intrinsic)
                                    && !filmcraft_project::graphic::is_layer(effect)
                                    && !effect.essential
                            })
                        })
                        .collect();
                    if effects.is_empty() {
                        app.ui.status = tl!("No supported effects are selected").to_string();
                    } else {
                        crate::panels::presets::open_save(app, clip.0, effects, def.name);
                    }
                }
                if copy_effect {
                    copy_effects(app, clip, it, idx);
                }
                if paste_effect {
                    paste_effects(app, clip);
                }
                if !open {
                    continue;
                }
                if crate::panels::audio_fx_editor::has_editor(&e.effect) {
                    custom_setup_row(app, bui, body, &lane, clip, idx, &e.effect);
                }
                for pd in &def.params {
                    param_row(app, bui, body, clip, idx, e, None, pd, mt_now, &mut actions, &lane, &lx, (shown_start, shown_end), it, &mut keys);
                    if app.ui.expanded_fx.contains(&graph_key(clip, idx, pd.id))
                        && let Some(param) = e.params.get(pd.id)
                        && param.is_animated()
                        && graphable(&param.value)
                    {
                        graph_rows(app, bui, body, clip, idx, None, pd, param, &lane, (shown_start, shown_end), it, &mut actions);
                    }
                }
                if crate::panels::masks::maskable(e) {
                    crate::panels::masks::effect_rows(app, bui, body, clip, idx, e, mt_now, &mut actions, &lane, &lx, (shown_start, shown_end), it, &mut keys);
                }
            }
        }
    });
    let _ = scroll_out;
    if ui.input(|i| i.pointer.button_released(egui::PointerButton::Primary))
        && let Some(drag) = effect_drag.take()
        && let Some(to) = drag.target.filter(|to| *to != drag.from)
    {
        reorder = Some((drag.clip, drag.from, to));
    }
    if effect_drag.is_some() {
        ui.ctx().set_cursor_icon(egui::CursorIcon::Grabbing);
    }
    ui.data_mut(|d| {
        if let Some(drag) = effect_drag.clone() {
            d.insert_temp(drag_id, drag);
        } else {
            d.remove::<EffectDrag>(drag_id);
        }
    });
    if let Some((clip_id, from, to)) = reorder {
        let before = app.session.active_sequence().and_then(|q| q.find_item(ClipId(clip_id))).map(|(_, item)| item.effects.clone());
        match app.session.execute("effects.move", json!({"clip": clip_id, "from": from, "to": to})) {
            Ok(_) => {
                if let (Some(before), Some((_, after))) = (before, app.session.active_sequence().and_then(|q| q.find_item(ClipId(clip_id)))) {
                    let mut remapped: Vec<_> = app.ui.selected_effects.iter().filter(|selection| selection.clip != ClipId(clip_id)).cloned().collect();
                    remapped.extend(
                        app.ui
                            .selected_effects
                            .iter()
                            .filter(|selection| selection.clip == ClipId(clip_id))
                            .filter_map(|selection| selection.index(&before))
                            .filter_map(|index| {
                                let index = if index == from {
                                    to
                                } else if from < to && (from..=to).contains(&index) {
                                    index.saturating_sub(1)
                                } else if to < from && (to..=from).contains(&index) {
                                    index.saturating_add(1)
                                } else {
                                    index
                                };
                                filmcraft_engine::EffectSel::of(ClipId(clip_id), &after.effects, index)
                            }),
                    );
                    app.ui.selected_effects = remapped;
                    app.ui.keyframe_selection.retain_mut(|key| {
                        if key.clip != clip_id {
                            return true;
                        }
                        key.effect = if key.effect == from {
                            to
                        } else if from < to && (from..=to).contains(&key.effect) {
                            key.effect.saturating_sub(1)
                        } else if to < from && (to..=from).contains(&key.effect) {
                            key.effect.saturating_add(1)
                        } else {
                            key.effect
                        };
                        after.effects.get(key.effect).is_some_and(|effect| {
                            match key.mask {
                                Some(mask) => effect.masks.get(mask).and_then(|mask| mask.param(&key.param)),
                                None => effect.params.get(&key.param),
                            }
                            .is_some_and(|param| param.keyframes.iter().any(|kf| kf.time == key.time))
                        })
                    });
                }
            }
            Err(error) => app.ui.status = error.to_string(),
        }
    }
    // the ruler and the clip's bar, painted after the rows so rows scrolled up under them (their
    // lines and keyframes) stay hidden
    ui.painter().rect_filled(Rect::from_min_max(lane.min, pos2(lane.max.x, bar.max.y)), 0.0, t.tl_bg);
    paint_ruler(ui.painter(), ruler, shown_start, shown_end, rate, seq.settings.drop_frame, &t);
    ui.painter().rect_filled(bar.shrink2(vec2(0.0, 2.0)), 2.0, t.clip_bar_bg);
    ui.painter().with_clip_rect(bar).text(pos2(bar.min.x + 4.0, bar.center().y), Align2::LEFT_CENTER, &it.name, Tokens::ui(10.0), t.text);
    time_zoom_bar(app, ui, zoom_bar, it.duration, rate);
    // click or drag anywhere on the ruler (or the bar) to move the playhead (after the rows, which
    // reach under it, so it gets the pointer first)
    let sresp = ui.interact(scrub, egui::Id::unique(("ec-scrub", clip.0)), Sense::click_and_drag());
    if (sresp.dragged() || sresp.clicked())
        && let Some(pos) = sresp.interact_pointer_pos()
    {
        let f = ((pos.x - lane.min.x) / lane.width()).clamp(0.0, 1.0) as f64;
        let tk = rate.snap_nearest(shown_start + Tick((f * shown_duration) as i64));
        app.stop_for_scrub();
        app.session.set_playhead(tk);
    }
    // the divider: a line the height of the panel, a resize cursor and a wider grab area
    let divider = Rect::from_min_max(pos2(split - 2.0, rect.min.y), pos2(split + 4.0, rect.max.y - 26.0));
    let dresp = ui.interact(divider, egui::Id::unique("ec-divider"), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    if dresp.dragged() {
        app.ui.effect_controls_split = if panel_width >= MIN_LIST_W + MIN_LANE_W + 10.0 {
            (list_w + dresp.drag_delta().x).clamp(MIN_LIST_W, max_list)
        } else {
            (list_w + dresp.drag_delta().x).clamp(0.0, panel_width)
        };
    }
    let dcol = if dresp.hovered() || dresp.dragged() { t.accent } else { t.separator };
    ui.painter().line_segment([pos2(split + 1.5, rect.min.y), pos2(split + 1.5, rect.max.y - 26.0)], Stroke::new(1.0, dcol));
    app.auto.add("effectControls.divider", divider, "divider");
    // playhead: a handle on the ruler and a line down the lane, while it is on the clip
    if ph >= shown_start && ph <= shown_end {
        let px = lx(ph);
        let (top, tip) = (ruler.max.y - 13.0, ruler.max.y);
        let head = vec![pos2(px - 5.0, top), pos2(px + 5.0, top), pos2(px + 5.0, tip - 5.0), pos2(px, tip), pos2(px - 5.0, tip - 5.0)];
        ui.painter().add(egui::Shape::convex_polygon(head, t.playhead, Stroke::NONE));
        ui.painter().line_segment([pos2(px, tip), pos2(px, lane.max.y)], Stroke::new(1.0, t.playhead));
        app.auto.add("effectControls.playhead", Rect::from_min_max(pos2(px - 5.0, top), pos2(px + 5.0, tip)), "playhead");
    }
    // footer timecode
    let tc = filmcraft_time::format_time(ph, seq.settings.frame_rate, seq.settings.drop_frame, filmcraft_time::TimeDisplay::Timecode, 48000);
    ui.painter().text(pos2(rect.min.x + 10.0, rect.max.y - 13.0), Align2::LEFT_CENTER, tc, Tokens::mono(13.0), t.timecode);
    keys.finish(app, ui);
    run(app, ui.ctx(), actions);
}

/// The thin line under a row, across the names, the values and the keyframe lane, so a row can be
/// followed across the panel, as in Premiere (#640). It runs through the middle of the spacing
/// between rows, so each row sits centred between its two lines.
pub(crate) fn row_line(ui: &egui::Ui, r: Rect, lane: &Rect, t: &Tokens) {
    let y = r.max.y + ui.spacing().item_spacing.y / 2.0;
    ui.painter().line_segment([pos2(r.min.x, y), pos2(lane.max.x, y)], Stroke::new(1.0, t.separator));
}

/// A row's label, cut off with "…" where it would run into what follows it, as Premiere does when
/// the effect list is narrow (#643).
pub(crate) fn row_label(ui: &egui::Ui, left_center: Pos2, text: &str, max_w: f32, font: egui::FontId, color: Color32) {
    let mut job = egui::text::LayoutJob::single_section(text.to_owned(), egui::TextFormat::simple(font, color));
    job.wrap.max_width = max_w.max(0.0);
    job.wrap.max_rows = 1;
    job.wrap.break_anywhere = true;
    job.wrap.overflow_character = Some('…');
    let g = ui.fonts_mut(|f| f.layout_job(job));
    ui.painter().galley(pos2(left_center.x, left_center.y - g.size().y / 2.0), g, color);
}

/// Make a dropdown or a checkbox in a row fit inside the row, centred between its row lines.
pub(crate) fn fit_to_row(ui: &mut egui::Ui) {
    ui.spacing_mut().interact_size.y = ROW_H - 4.0;
    ui.spacing_mut().button_padding.y = 1.0;
}

/// Premiere's "Custom Setup ▸ Edit…" row: opens the effect's Clip Fx Editor window.
fn custom_setup_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, body: Rect, lane: &Rect, clip: ClipId, idx: usize, effect: &str) {
    let t = app.tokens;
    let (r, _) = ui.allocate_exact_size(vec2(body.width(), ROW_H), Sense::hover());
    row_line(ui, r, lane, &t);
    ui.painter().text(pos2(r.min.x + 42.0, r.center().y), Align2::LEFT_CENTER, tl!("Custom Setup"), Tokens::ui(12.0), t.text_dim);
    let br = Rect::from_min_size(pos2(r.min.x + 160.0, r.min.y + 2.0), vec2(60.0, ROW_H - 4.0));
    let resp = ui.interact(br, egui::Id::unique(("fx-custom-setup", clip.0, idx)), Sense::click());
    ui.painter().rect_filled(br, 3.0, if resp.hovered() { t.hover } else { t.field_bg });
    ui.painter().rect_stroke(br, 3.0, Stroke::new(1.0, t.field_border), egui::StrokeKind::Inside);
    ui.painter().text(br.center(), Align2::CENTER_CENTER, tl!("Edit…"), Tokens::ui(11.5), t.text);
    app.auto.add(&format!("effectControls.effect.{effect}.edit"), br, "Edit…");
    if resp.clicked() {
        crate::panels::audio_fx_editor::open(app, crate::panels::audio_fx_editor::FxTarget::Clip { clip: clip.0, index: idx });
    }
}

fn seq_name(app: &FilmcraftApp) -> String {
    app.session.state.active_sequence.and_then(|s| app.session.project.item(s)).map(|i| i.name.clone()).unwrap_or_default()
}

/// The keyframe lane's time ruler: ticks and sequence timecode across the visible part of the clip.
fn paint_ruler(p: &egui::Painter, ruler: Rect, start: Tick, end: Tick, rate: filmcraft_time::FrameRate, drop_frame: bool, t: &Tokens) {
    let p = p.with_clip_rect(ruler);
    let dur = end.0.saturating_sub(start.0).max(1) as f64;
    let frame_px = ruler.width() as f64 * rate.frame_duration().0.max(1) as f64 / dur;
    let base = rate.timecode_base();
    let steps = [
        1,
        2,
        5,
        10,
        base / 2,
        base,
        base * 2,
        base * 5,
        base * 10,
        base * 15,
        base * 30,
        base * 60,
        base * 120,
        base * 300,
        base * 600,
        base * 1800,
        base * 3600,
    ];
    // labels far enough apart to read; the small ticks divide the labelled ones evenly
    let label_step = steps.iter().copied().find(|s| *s > 0 && *s as f64 * frame_px >= 80.0);
    let minor = steps.iter().copied().find(|s| *s > 0 && *s as f64 * frame_px >= 8.0 && label_step.is_none_or(|l| l % s == 0));
    let (f0, f1) = (rate.frame_at(start), rate.frame_at(end));
    let base_y = ruler.max.y - 1.0;
    for (step, h, labelled) in [(minor, 3.0, false), (label_step, 7.0, true)] {
        let Some(step) = step else { continue };
        let mut f = f0.div_euclid(step).saturating_mul(step);
        if f < f0 {
            f = f.saturating_add(step);
        }
        // a step is at least 8 px wide, so this covers any ruler; the cap is for damaged numbers
        for _ in 0..4096 {
            if f > f1 {
                break;
            }
            let x = ruler.min.x + (rate.tick_of(f).0.saturating_sub(start.0) as f64 / dur) as f32 * ruler.width();
            p.line_segment([pos2(x, base_y - h), pos2(x, base_y)], Stroke::new(1.0, t.tl_ruler_tick));
            if labelled {
                let label = filmcraft_time::format_time(rate.tick_of(f), rate, drop_frame, filmcraft_time::TimeDisplay::Timecode, 48000);
                let half = p.layout_no_wrap(label.clone(), Tokens::ui(10.0), t.tl_ruler_text).size().x / 2.0;
                let label_x = x.clamp(ruler.min.x + half, (ruler.max.x - half).max(ruler.min.x + half));
                p.text(pos2(label_x, ruler.min.y + 7.0), Align2::CENTER_CENTER, label, Tokens::ui(10.0), t.tl_ruler_text);
            }
            f = f.saturating_add(step);
        }
    }
}

/// Premiere's keyframe navigator, centred on `at`: a diamond that adds a keyframe at the playhead
/// or removes the one there (filled while the playhead is on a keyframe) and, once the parameter
/// is animated, arrows to the previous and the next keyframe. `param` is `None` while the effect
/// is not on the clip yet. Automation ids: `<auto>.addKeyframe`, `.prevKeyframe`, `.nextKeyframe`.
/// Returns whether the diamond was clicked; the arrows queue their own `playhead.set`.
#[allow(clippy::too_many_arguments)]
fn keyframe_nav(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    at: Pos2,
    param: Option<&filmcraft_project::Param>,
    mt: Tick,
    it: &TrackItem,
    id: egui::Id,
    auto: &str,
    tip: &str,
    actions: &mut Vec<(String, Value)>,
) -> bool {
    let t = app.tokens;
    if let Some(param) = param.filter(|p| p.is_animated()) {
        for (d, target, name, tip) in
            [(-1.0, param.prev_keyframe(mt), "prevKeyframe", "Go to previous keyframe"), (1.0, param.next_keyframe(mt), "nextKeyframe", "Go to next keyframe")]
        {
            // the arrows almost the row's height to click, as Premiere's
            let r = Rect::from_center_size(at + vec2(d * NAV_STEP, 0.0), vec2(14.0, 20.0));
            let resp = ui.interact(r, id.with(name), Sense::click()).on_hover_text(crate::i18n::t(tip));
            let c = r.center();
            let pts = vec![c + vec2(-3.5 * d, -5.5), c + vec2(-3.5 * d, 5.5), c + vec2(3.5 * d, 0.0)];
            ui.painter().add(egui::Shape::convex_polygon(pts, if target.is_some() { t.text } else { t.text_faint }, Stroke::NONE));
            app.auto.add(&format!("{auto}.{name}"), r, tip);
            if resp.clicked()
                && let Some(k) = target
            {
                actions.push(("playhead.set".into(), json!({"time": timeline_time_of(it, k).0})));
            }
        }
    }
    let r = Rect::from_center_size(at, vec2(15.0, 15.0));
    let resp = ui.interact(r.expand(1.5), id.with("key"), Sense::click()).on_hover_text(crate::i18n::t(tip));
    if param.is_some_and(|p| p.keyframes.iter().any(|k| k.time == mt)) {
        icons::paint(ui.painter(), r, Icon::Keyframe, t.hot_text);
    } else {
        let s = 4.7;
        let outline = vec![at + vec2(0.0, -s), at + vec2(s, 0.0), at + vec2(0.0, s), at + vec2(-s, 0.0)];
        ui.painter().add(egui::Shape::closed_line(outline, Stroke::new(1.2, if resp.hovered() { t.text } else { t.text_dim })));
    }
    app.auto.add(&format!("{auto}.addKeyframe"), r, tip);
    resp.clicked()
}

/// A Properties row's keyframe navigator for `effect`.`param`, with Premiere's tooltips. The
/// diamond is `effects.addKeyframe`: it adds a keyframe at the playhead (turning animation on) or
/// removes the one there, so the value falls back to what the remaining keyframes give. An effect
/// the clip does not have yet (Crop) is applied first.
#[allow(clippy::too_many_arguments)]
fn properties_nav(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    row: Rect,
    clip: ClipId,
    it: &TrackItem,
    mt: Tick,
    effect: &str,
    param: &str,
    actions: &mut Vec<(String, Value)>,
) {
    let prm = it.effect(effect).and_then(|e| e.param(param));
    let tip = match prm {
        Some(p) if p.keyframes.iter().any(|k| k.time == mt) => "Remove keyframe",
        Some(p) if p.is_animated() => "Add keyframe",
        _ => "Turn on animation and add keyframe",
    };
    let at = pos2(row.max.x - PROPS_NAV_X, row.center().y);
    let id = egui::Id::unique(("props-nav", effect, param, clip.0));
    if keyframe_nav(app, ui, at, prm, mt, it, id, &format!("properties.{effect}.{param}"), tip, actions) {
        if it.effect(effect).is_none() {
            actions.push(("effects.apply".into(), json!({"clips": [clip.0], "effect": effect})));
        }
        actions.push(("effects.addKeyframe".into(), json!({"clip": clip.0, "effect": effect, "param": param})));
    }
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn param_row(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    body: Rect,
    clip: ClipId,
    idx: usize,
    e: &EffectInstance,
    mask: Option<usize>,
    pd: &filmcraft_project::ParamDef,
    mt: Tick,
    actions: &mut Vec<(String, Value)>,
    lane: &Rect,
    lx: &dyn Fn(Tick) -> f32,
    timeline_range: (Tick, Tick),
    it: &TrackItem,
    keys: &mut Interaction,
) {
    let t = app.tokens;
    let param = match mask {
        Some(k) => e.masks.get(k).and_then(|m| m.param(pd.id)),
        None => e.params.get(pd.id),
    };
    let Some(param) = param else { return };
    // mask parameters: automation ids / widget ids get a `m<k>.` prefix, commands a `mask` field
    let pkey: String = match mask {
        Some(k) => format!("mask{k}.{}", pd.id),
        None => pd.id.to_string(),
    };
    let pkey = pkey.as_str();
    let with_mask = |mut v: Value| -> Value {
        if let Some(k) = mask {
            v["mask"] = json!(k);
        }
        v
    };
    let (r, row) = ui.allocate_exact_size(vec2(body.width(), ROW_H), if mask.is_none() { Sense::click() } else { Sense::hover() });
    row_line(ui, r, lane, &t);
    // clicking one of an effect's properties selects the effect, as clicking its header does
    if row.clicked() {
        app.ui.selected_effects = filmcraft_engine::EffectSel::of(clip, &it.effects, idx).into_iter().collect();
        actions.push(("effects.select".into(), json!({"clip": clip.0, "effect": idx})));
    }
    let mut x = r.min.x + 26.0;
    // twirl-down for the value/velocity graphs (animated scalar and point params)
    if param.is_animated() && graphable(&param.value) {
        let key = graph_key(clip, idx, pkey);
        let open = app.ui.expanded_fx.contains(&key);
        let tw = Rect::from_center_size(pos2(r.min.x + 12.0, r.center().y), vec2(12.0, 12.0));
        let tresp = ui.interact(tw.expand(2.0), egui::Id::unique(("twirl", clip.0, idx, pkey)), Sense::click()).on_hover_text(tl!("Show graphs"));
        icons::paint(ui.painter(), tw, if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_dim);
        app.auto.add(&format!("effectControls.{}.{}.graphs", e.effect, pkey), tw, "Show graphs");
        if tresp.clicked() {
            if open {
                app.ui.expanded_fx.retain(|k| *k != key);
            } else {
                app.ui.expanded_fx.push(key);
            }
        }
    }
    if pd.animatable {
        let sw = Rect::from_center_size(pos2(x, r.center().y), vec2(14.0, 14.0));
        let resp = ui.interact(sw, egui::Id::unique(("sw", clip.0, idx, pkey)), Sense::click()).on_hover_text(tl!("Toggle animation"));
        icons::paint(ui.painter(), sw, Icon::Stopwatch, if param.is_animated() { t.accent } else { t.text_dim });
        if resp.clicked() {
            actions.push(("effects.toggleAnimation".into(), with_mask(json!({"clip": clip.0, "effect": idx, "param": pd.id}))));
        }
        app.auto.add(&format!("effectControls.{}.{}.stopwatch", e.effect, pkey), sw, "Toggle animation");
    }
    x += 14.0;
    let vx = r.min.x + (r.width() * 0.5).max(150.0);
    row_label(ui, pos2(x, r.center().y), crate::i18n::t(pd.label), vx - 6.0 - x, Tokens::ui(12.0), t.text);
    // double-clicking a property's name resets it, as the reset button at the row's end does (a
    // click on the value itself starts typing, so the name is the double-click target)
    if mask.is_none() {
        let name = Rect::from_min_max(pos2(x, r.min.y), pos2(vx - 6.0, r.max.y));
        // the instance index tells two copies of one effect apart; only the visible part of a row
        // scrolled partly out of view is registered
        let shown = name.intersect(ui.clip_rect());
        if shown.is_positive() {
            app.auto.add(&format!("effectControls.{}.{idx}.{}.name", e.effect, pkey), shown, crate::i18n::t(pd.label));
        }
        if row.double_clicked() && row.interact_pointer_pos().is_some_and(|p| name.contains(p)) {
            actions.push(("effects.resetParam".into(), json!({"clip": clip.0, "effect": idx, "param": pd.id})));
        }
    }
    let value = param.value_at(mt);
    let mut vui = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_max(pos2(vx, r.min.y + 1.0), pos2(r.max.x - 26.0, r.max.y - 1.0)))
            .layout(egui::Layout::left_to_right(egui::Align::Center)),
    );
    let id = egui::Id::unique(("pv", clip.0, idx, pkey));
    let set = param_value_widget(app, &mut vui, e, pd, &value, clip, idx, mask, pkey, id, t, actions);
    if let Some(v) = set {
        actions.push(("effects.setParam".into(), with_mask(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": v}))));
    }
    // keyframe navigator ◀ ◆ ▶ (when animated)
    let eff_json = json!(idx);
    if pd.animatable && param.is_animated() {
        let at = pos2(r.max.x - 42.0, r.center().y);
        let auto = format!("effectControls.{}.{}", e.effect, pkey);
        if keyframe_nav(app, ui, at, Some(param), mt, it, egui::Id::unique(("knav", clip.0, idx, pkey)), &auto, "Add/Remove Keyframe", actions) {
            actions.push(("effects.addKeyframe".into(), with_mask(json!({"clip": clip.0, "effect": eff_json, "param": pd.id}))));
        }
    }
    // reset to the default, under the effect's own reset button (an animated parameter keeps its
    // keyframes and gets one at the playhead)
    if mask.is_none() {
        let rr = Rect::from_center_size(pos2(r.max.x - 14.0, r.center().y), vec2(12.0, 12.0));
        let rresp = ui.interact(rr, egui::Id::unique(("param-reset", clip.0, idx, pkey)), Sense::click()).on_hover_text(tl!("Reset Parameter"));
        icons::paint(ui.painter(), rr, Icon::Reset, if rresp.hovered() { t.text } else { t.text_dim });
        app.auto.add(&format!("effectControls.{}.{}.reset", e.effect, pkey), rr, "Reset Parameter");
        if rresp.clicked() {
            actions.push(("effects.resetParam".into(), json!({"clip": clip.0, "effect": eff_json, "param": pd.id})));
        }
    }
    // keyframes in the lane: draggable diamonds; right-click for interpolation
    if param.is_animated() {
        let y = r.center().y;
        for k in &param.keyframes {
            let tl = timeline_time_of(it, k.time);
            if tl < timeline_range.0 || tl > timeline_range.1 {
                continue;
            }
            let id = egui::Id::unique(("kf", clip.0, idx, pkey, k.time.0));
            let original = Rect::from_center_size(pos2(lx(tl), y), vec2(15.0, 15.0));
            let resp = ui.interact(original.expand(2.0), id, Sense::click_and_drag());
            let reference = KeyframeRef { clip: clip.0, effect: idx, param: pd.id.to_string(), mask, time: k.time };
            let kr = keys.key(app, ui, reference.clone(), it, *lane, original, &resp, k.interp);
            app.auto.add(&format!("effectControls.{}.{}.keyframe.{}", e.effect, pkey, k.time.0), kr, "keyframe");
            let selected = app.ui.keyframe_selection.contains(&reference);
            let col = if selected { t.hot_text } else { Color32::from_rgb(0xb0, 0xb0, 0xb0) };
            selection::paint(ui.painter(), kr, k.interp, col);
            if resp.clicked_by(egui::PointerButton::Primary) {
                actions.push(("playhead.set".into(), json!({"time": tl.0})));
            }
            resp.context_menu(|ui| {
                keyframe_menu(app, ui, KeyframeRef { clip: clip.0, effect: idx, param: pd.id.to_string(), mask, time: k.time }, k.interp, actions);
            });
        }
    }
}

/// The value editor of one parameter row: number, point, checkbox, choice, colour (with the
/// eyedropper), path or text field. Returns the edited value, if any.
#[allow(clippy::too_many_arguments)]
fn param_value_widget(
    app: &mut FilmcraftApp,
    vui: &mut egui::Ui,
    e: &EffectInstance,
    pd: &filmcraft_project::ParamDef,
    value: &ParamValue,
    clip: ClipId,
    idx: usize,
    mask: Option<usize>,
    pkey: &str,
    id: egui::Id,
    t: Tokens,
    actions: &mut Vec<(String, Value)>,
) -> Option<Value> {
    let mut set: Option<Value> = None;
    match (&pd.kind, value) {
        (ParamKind::Float { min, max, soft_min, soft_max, unit, decimals }, ParamValue::Float(v)) => {
            let speed = ((soft_max - soft_min) / 400.0).max(0.01);
            let (_, nv) =
                crate::widgets::hot_number(vui, crate::i18n::t(pd.label), id, *v, HotNumberOptions::new(speed, (*min, *max), *decimals as usize, unit, &t));
            if let Some(nv) = nv {
                set = Some(json!(nv));
            }
        }
        (ParamKind::Angle, ParamValue::Float(v)) => {
            let (_, nv) = crate::widgets::hot_number(vui, crate::i18n::t(pd.label), id, *v, HotNumberOptions::new(0.5, (-36000.0, 36000.0), 1, "°", &t));
            if let Some(nv) = nv {
                set = Some(json!(nv));
            }
        }
        (ParamKind::Point, ParamValue::Vec2(p)) => {
            let (_, nx) = crate::widgets::hot_number(
                vui,
                &format!("{} X", crate::i18n::t(pd.label)),
                id.with("x"),
                p.x,
                HotNumberOptions::new(1.0, (-100_000.0, 100_000.0), 1, "", &t),
            );
            let (_, ny) = crate::widgets::hot_number(
                vui,
                &format!("{} Y", crate::i18n::t(pd.label)),
                id.with("y"),
                p.y,
                HotNumberOptions::new(1.0, (-100_000.0, 100_000.0), 1, "", &t),
            );
            if nx.is_some() || ny.is_some() {
                set = Some(json!([nx.unwrap_or(p.x), ny.unwrap_or(p.y)]));
            }
        }
        (ParamKind::Bool, ParamValue::Bool(b)) => {
            let mut v = *b;
            fit_to_row(vui);
            if crate::widgets::accessible_name(vui.checkbox(&mut v, ""), crate::i18n::t(pd.label)).changed() {
                set = Some(json!(v));
            }
        }
        (ParamKind::Choice(opts), ParamValue::Choice(c)) => {
            let mut sel = *c as usize;
            fit_to_row(vui);
            // (narrower when the effect list is narrow, so it stays left of the divider)
            let w = vui.available_width().min(130.0);
            let combo = craft_ui::ComboBox::from_id_salt(id).selected_text(opts.get(sel).map_or("", |o| crate::i18n::t(o))).width(w).show_ui(vui, |ui| {
                for (i, o) in opts.iter().enumerate() {
                    if ui.selectable_value(&mut sel, i, crate::i18n::t(o)).changed() {
                        set = Some(json!(i));
                    }
                }
            });
            crate::widgets::accessible_name(combo.response, crate::i18n::t(pd.label));
        }
        (ParamKind::Color, ParamValue::Color(c)) => {
            let mut rgba = egui::Rgba::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
            if egui::color_picker::color_edit_button_rgba(vui, &mut rgba, egui::color_picker::Alpha::Opaque).changed() {
                set = Some(json!([rgba.r(), rgba.g(), rgba.b(), rgba.a()]));
            }
            // Eyedropper: arm it, then click a pixel in the Program monitor (Esc cancels)
            let target = crate::state::Eyedropper { clip: clip.0, effect: idx, param: pd.id.to_string(), mask };
            let armed = app.ui.eyedropper.as_ref() == Some(&target);
            let drop = icons::button(vui, Icon::Eyedropper, 18.0, armed, &t, tl!("Eyedropper (pick a color from the Program monitor)"));
            app.auto.add(&format!("effectControls.{}.{}.eyedropper", e.effect, pkey), drop.rect, "Eyedropper");
            if drop.clicked() {
                app.ui.eyedropper = if armed { None } else { Some(target) };
            }
        }
        (ParamKind::Path, _) => {
            crate::panels::masks::path_value(app, vui, clip, idx, mask, actions);
        }
        // a text layer's Font and Font Style pick from the installed families and styles, as the
        // Text panel's drop-downs do (#3539)
        (ParamKind::Text, ParamValue::Text(current)) if e.effect == filmcraft_project::graphic::TEXT_LAYER && matches!(pd.id, "font" | "font_style") => {
            if !filmcraft_text::fonts::system_scanned() {
                filmcraft_text::fonts::scan_system();
            }
            let fams = filmcraft_text::families();
            let salt = ("ec-font", clip.0, idx, pd.id);
            // the value column can be narrower than the usual drop-down; keep the button inside it
            let w = 150.0_f32.min(vui.available_width()).max(40.0);
            let prefix = format!("effectControls.text.{}.{}.option", idx, pd.id);
            let combo = if pd.id == "font" {
                craft_ui::ComboBox::from_id_salt(salt).selected_text(current).width(w).height(400.0).show_ui(vui, |ui| {
                    for (f, _) in &fams {
                        let r = ui.selectable_label(f == current, f);
                        app.auto.add(&format!("{prefix}.{f}"), r.rect, f);
                        if r.clicked() {
                            set = Some(json!(f));
                        }
                    }
                })
            } else {
                let family = match e.param("font").map(|p| &p.value) {
                    Some(ParamValue::Text(f)) => f.as_str(),
                    _ => "",
                };
                let mut styles: Vec<&str> =
                    fams.iter().find(|(f, _)| f.eq_ignore_ascii_case(family)).map(|(_, s)| s.iter().map(String::as_str).collect()).unwrap_or_default();
                for extra in ["Bold", "Italic", "Bold Italic"] {
                    if !styles.contains(&extra) {
                        styles.push(extra);
                    }
                }
                craft_ui::ComboBox::from_id_salt(salt).selected_text(current).width(w).show_ui(vui, |ui| {
                    for st in styles {
                        let r = ui.selectable_label(st == current, st);
                        app.auto.add(&format!("{prefix}.{st}"), r.rect, st);
                        if r.clicked() {
                            set = Some(json!(st));
                        }
                    }
                })
            };
            let r = crate::widgets::accessible_name(combo.response, crate::i18n::t(pd.label));
            app.auto.add(&format!("effectControls.text.{}.{}", idx, pd.id), r.rect, pd.label);
        }
        (ParamKind::Text, ParamValue::Text(s)) if !pd.id.ends_with("_lut") => {
            // edited in a buffer; committed (one undo step) when the field loses focus
            let mut buf = vui.data_mut(|d| d.get_temp::<String>(id)).unwrap_or_else(|| s.clone());
            let r = crate::widgets::accessible_name(vui.add(egui::TextEdit::singleline(&mut buf).desired_width(160.0)), crate::i18n::t(pd.label));
            app.auto.add(&format!("effectControls.text.{}.{}", idx, pd.id), r.rect, pd.label);
            crate::widgets::select_all_on_focus(vui, &r, &buf);
            if r.lost_focus() {
                if buf != *s {
                    set = Some(json!(buf));
                }
                vui.data_mut(|d| d.remove::<String>(id));
            } else if r.has_focus() {
                vui.data_mut(|d| d.insert_temp(id, buf));
            } else {
                vui.data_mut(|d| d.remove::<String>(id));
            }
        }
        // Lumetri's Input LUT / Look LUT: the Lumetri Color panel's menu (it showed only the name, #1123)
        (ParamKind::Text, ParamValue::Text(cur)) => {
            let input = pd.id == "input_lut";
            let auto_id = format!("effectControls.{}.{}", e.effect, pkey);
            let (rect, pick) = crate::panels::lumetri::lut_menu(vui, app, ("fx-lut", clip.0, idx, pkey), cur, input, 160.0, &auto_id);
            app.auto.add(&auto_id, rect, pd.label);
            match pick {
                Some(crate::panels::lumetri::LutPick::Lut(r)) => set = Some(json!(r)),
                Some(crate::panels::lumetri::LutPick::Browse) => {
                    let (param, clip, mask, label) = (pd.id.to_string(), clip.0, mask, pd.label);
                    app.pick_ui(crate::native_dialogs::FileDialog::open_file(tl!("LUT"), &["cube", "3dl"]), move |app, paths| {
                        let Some(path) = paths.into_iter().next() else { return };
                        // importing into the LUT library and setting the parameter undo as one step
                        let r = app.session.edit_group(label, |s| {
                            let r = s.execute("lut.import", json!({"path": path}))?["ref"].clone();
                            let mut p = json!({"clip": clip, "effect": idx, "param": param, "value": r});
                            if let Some(k) = mask {
                                p["mask"] = json!(k);
                            }
                            s.execute("effects.setParam", p)
                        });
                        if let Err(err) = r {
                            app.ui.status = err.to_string();
                        }
                    });
                }
                None => {}
            }
        }
        _ => {
            vui.label(param_text(&app.session.project, pd.id, value));
        }
    }
    set
}

/// Shared by the lane diamonds and graph points: commands use media time and preserve the mask.
/// The rows and their order are Premiere's (Cut, Copy and Paste have no keyframe command yet, and
/// Select All waits for Clear to act on the whole selection); `current` is the keyframe's
/// interpolation, the checked row.
fn keyframe_menu(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    target: KeyframeRef,
    current: filmcraft_project::Interpolation,
    actions: &mut Vec<(String, Value)>,
) {
    use filmcraft_project::Interpolation;
    let mut params = json!({"clip": target.clip, "effect": target.effect, "param": target.param, "mediaTime": target.time.0});
    if let Some(mask) = target.mask {
        params["mask"] = json!(mask);
    }
    // A selected keyframe stands for the whole selection, as when dragging it (#1916).
    let interpolated = if app.ui.keyframe_selection.contains(&target) { json!({"keyframes": app.ui.keyframe_selection}) } else { params.clone() };
    let response = crate::menus::checked_entry(ui, tl!("Clear"), true, None);
    app.auto.add("effectControls.keyframeMenu.clear", response.rect, "Clear");
    if response.clicked() {
        actions.push(("effects.deleteKeyframe".into(), params.clone()));
        ui.close();
    }
    for group in [
        &[
            (tl!("Linear"), "linear", Interpolation::Linear),
            (tl!("Bezier"), "bezier", Interpolation::Bezier),
            (tl!("Auto Bezier"), "autoBezier", Interpolation::AutoBezier),
            (tl!("Continuous Bezier"), "continuousBezier", Interpolation::ContinuousBezier),
            (tl!("Hold"), "hold", Interpolation::Hold),
        ][..],
        &[(tl!("Ease In"), "easeIn", Interpolation::EaseIn), (tl!("Ease Out"), "easeOut", Interpolation::EaseOut)],
    ] {
        ui.separator();
        for &(label, key, interpolation) in group {
            let response = crate::menus::checked_entry(ui, label, true, Some(current == interpolation));
            app.auto.add(&format!("effectControls.keyframeMenu.{key}"), response.rect, label);
            if response.clicked() {
                let mut params = interpolated.clone();
                params["interpolation"] = json!(key);
                actions.push(("effects.setInterpolation".into(), params));
                ui.close();
            }
        }
    }
}

/// Lumetri Color panel: edits (or adds) the Lumetri effect on the selected clip, grouped by section.
pub fn lumetri_panel(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some((clip, it, _)) = selected_clip(app) else {
        crate::dock::placeholder(ui, rect, &t, tl!("Select a clip to grade"));
        return;
    };
    let idx = it.effects.iter().position(|e| e.effect == "lumetri");
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(6.0)).id_salt("lumetri"));
    bui.label(egui::RichText::new(tlf!("Master · {name}", name = it.name)).color(t.text_dim));
    let Some(idx) = idx else {
        if bui.button(tl!("Add Lumetri Color to clip")).clicked() {
            let _ = app.session.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"}));
        }
        return;
    };
    let e = it.effects[idx].clone();
    let Some(def) = e.def() else { return };
    let ph = app.session.playhead();
    let mt = it.effect_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let mut actions = Vec::new();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut bui, |ui| {
        let mut groups: Vec<&str> = Vec::new();
        for p in &def.params {
            if let Some(g) = p.group
                && !groups.contains(&g)
            {
                groups.push(g);
            }
        }
        for g in groups {
            let key = format!("lumetri:{g}");
            let open = !app.ui.collapsed_fx.contains(&key);
            let (resp, now_open) = crate::widgets::section_header(ui, egui::Id::unique(&key), crate::i18n::t(g), open, &t, true);
            app.auto.add(&format!("lumetri.section.{g}"), resp.rect, g);
            if now_open != open {
                if now_open {
                    app.ui.collapsed_fx.retain(|k| *k != key);
                } else {
                    app.ui.collapsed_fx.push(key.clone());
                }
            }
            if !now_open {
                continue;
            }
            for pd in def.params.iter().filter(|p| p.group == Some(g)) {
                let v = e.params.get(pd.id).map(|p| p.value_at(mt)).unwrap_or(pd.default.clone());
                ui.horizontal(|ui| {
                    ui.add_space(18.0);
                    ui.add_sized(vec2(110.0, 18.0), egui::Label::new(egui::RichText::new(crate::i18n::t(pd.label)).size(12.0)));
                    if let (ParamKind::Float { min, max, soft_min, soft_max, .. }, ParamValue::Float(x)) = (&pd.kind, &v) {
                        let mut val = *x;
                        let s = ui.add(egui::Slider::new(&mut val, *soft_min..=*soft_max).show_value(false));
                        let (_, nv) = crate::widgets::hot_number(
                            ui,
                            crate::i18n::t(pd.label),
                            egui::Id::unique(("lum", pd.id)),
                            val,
                            HotNumberOptions::new((soft_max - soft_min) / 300.0, (*min, *max), 1, "", &t),
                        );
                        if s.changed() {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": val}));
                        } else if let Some(nv) = nv {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": nv}));
                        }
                        if s.double_clicked() {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": pd.default.as_f64().unwrap_or(0.0)}));
                        }
                    } else if let ParamValue::Color(c) = v {
                        let mut rgba = egui::Rgba::from_rgba_unmultiplied(c[0], c[1], c[2], c[3]);
                        let (color_response, changed, hex_rect) =
                            crate::widgets::color_edit_with_hex(ui, egui::Id::unique(("lumetri-color", pd.id)), &mut rgba, egui::color_picker::Alpha::Opaque);
                        app.auto.add(&format!("lumetri.{}.color", pd.id), color_response.rect, "Color picker");
                        if changed {
                            actions.push(json!({"clip": clip.0, "effect": idx, "param": pd.id, "value": [rgba.r(), rgba.g(), rgba.b(), 1.0]}));
                        }
                        if let Some(rect) = hex_rect {
                            app.auto.add(&format!("lumetri.{}.hex", pd.id), rect, "HEX color");
                        }
                    }
                });
            }
        }
    });
    run(app, ui.ctx(), actions.into_iter().map(|a| ("effects.setParam".to_string(), a)).collect());
}

/// Properties panel (Premiere 26): a compact inspector for the selected clip.
pub fn properties_panel(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let clips = selected_clips(app);
    let Some((clip, it, kind)) = clips.first().cloned() else {
        crate::dock::placeholder(ui, rect, &t, tl!("Select a clip to see its properties"));
        return;
    };
    let ph = app.session.playhead();
    let mt = it.effect_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(14.0, 8.0))).id_salt("props"));
    // header: clip name + menu
    let (hr, _) = bui.allocate_exact_size(vec2(bui.available_width(), 30.0), Sense::hover());
    let sw = Rect::from_center_size(pos2(hr.min.x + 8.0, hr.center().y), vec2(12.0, 12.0));
    let lc = app.session.prefs.labels.rgb(it.label);
    bui.painter().rect_filled(sw, 2.0, Color32::from_rgb(lc[0], lc[1], lc[2]));
    bui.painter().text(pos2(hr.min.x + 22.0, hr.center().y), Align2::LEFT_CENTER, &it.name, Tokens::semibold(12.5), t.text);
    let row = |ui: &mut egui::Ui, label: &str| -> Rect {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 32.0), Sense::hover());
        ui.painter().text(pos2(r.min.x + 18.0, r.center().y), Align2::LEFT_CENTER, label, Tokens::ui(12.0), t.text_dim);
        r
    };
    let section = |ui: &mut egui::Ui, app: &mut FilmcraftApp, name: &str, reset: Option<(usize, u64)>, actions: &mut Vec<(String, Value)>| -> bool {
        ui.add_space(4.0);
        let key = format!("props:{name}");
        let open = !app.ui.collapsed_fx.contains(&key);
        let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 30.0), Sense::click());
        ui.painter().line_segment([r.left_top(), r.right_top()], Stroke::new(1.0, t.separator));
        icons::paint(
            ui.painter(),
            Rect::from_center_size(pos2(r.min.x + 6.0, r.center().y), vec2(10.0, 10.0)),
            if open { Icon::ChevronDown } else { Icon::ChevronRight },
            t.text_dim,
        );
        ui.painter().text(pos2(r.min.x + 18.0, r.center().y), Align2::LEFT_CENTER, crate::i18n::t(name), Tokens::semibold(13.0), t.text);
        if let Some((idx, c)) = reset {
            let rr = Rect::from_center_size(pos2(r.max.x - PROPS_NAV_X, r.center().y), vec2(14.0, 14.0));
            icons::paint(ui.painter(), rr, Icon::Reset, t.text_dim);
            if ui.interact(rr, egui::Id::unique(("props-reset", name, c)), Sense::click()).clicked() {
                actions.push(("effects.reset".into(), json!({"clip": c, "index": idx})));
            }
        }
        if resp.clicked() {
            if open {
                app.ui.collapsed_fx.push(key);
            } else {
                app.ui.collapsed_fx.retain(|k| *k != key);
            }
        }
        open
    };
    let mut open_speed = false;
    // the sections scroll: a short panel would otherwise cut Audio and the speed button off (#1322)
    egui::ScrollArea::vertical().id_salt("props-scroll").auto_shrink([false, false]).show(&mut bui, |bui| {
        // keep the rows (and their keyframe arrows) clear of the scrollbar at the panel edge
        bui.set_max_width((bui.available_width() - 14.0).max(1.0));
        let eff_idx = |id: &str| it.effects.iter().position(|e| e.effect == id);
        let val = |eid: &str, p: &str| it.effect(eid).and_then(|e| e.param(p)).map(|p| p.value_at(mt));
        if kind == TrackKind::Video {
            if section(bui, app, "Transform", eff_idx("motion").map(|i| (i, clip.0)), &mut actions) {
                for (label, p, unit, speed, range) in [
                    (tl!("Position"), "position", "", 1.0, (-100_000.0, 100_000.0)),
                    (tl!("Anchor point"), "anchor", "", 1.0, (-100_000.0, 100_000.0)),
                    (tl!("Scale"), "scale", " %", 0.5, (0.0, 10000.0)),
                    (tl!("Rotation"), "rotation", " °", 0.5, (-36000.0, 36000.0)),
                ] {
                    let r = row(bui, label);
                    let mut vui = bui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(Rect::from_min_max(pos2(r.min.x + 150.0, r.min.y + 4.0), pos2(r.max.x - PROPS_VALUE_R, r.max.y - 4.0)))
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    match val("motion", p) {
                        Some(ParamValue::Vec2(v)) => {
                            let (_, nx) = crate::widgets::hot_number(
                                &mut vui,
                                &format!("{label} X"),
                                egui::Id::unique(("pp", p, "x", clip.0)),
                                v.x,
                                HotNumberOptions::new(speed, range, 0, " X", &t),
                            );
                            vui.add_space(10.0);
                            let (_, ny) = crate::widgets::hot_number(
                                &mut vui,
                                &format!("{label} Y"),
                                egui::Id::unique(("pp", p, "y", clip.0)),
                                v.y,
                                HotNumberOptions::new(speed, range, 0, " Y", &t),
                            );
                            if nx.is_some() || ny.is_some() {
                                actions.push((
                                    "effects.setParam".into(),
                                    json!({"clip": clip.0, "effect": "motion", "param": p, "value": [nx.unwrap_or(v.x), ny.unwrap_or(v.y)]}),
                                ));
                            }
                        }
                        Some(ParamValue::Float(v)) => {
                            let (_, nv) = crate::widgets::hot_number(
                                &mut vui,
                                label,
                                egui::Id::unique(("pp", p, clip.0)),
                                v,
                                HotNumberOptions::new(speed, range, if p == "scale" { 0 } else { 1 }, unit, &t),
                            );
                            if let Some(nv) = nv {
                                actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": "motion", "param": p, "value": nv})));
                            }
                        }
                        _ => {}
                    }
                    properties_nav(app, bui, r, clip, &it, mt, "motion", p, &mut actions);
                }
                let r = row(bui, tl!("Opacity"));
                let mut vui = bui.new_child(
                    egui::UiBuilder::new()
                        .max_rect(Rect::from_min_max(pos2(r.min.x + 150.0, r.min.y + 4.0), pos2(r.max.x - PROPS_VALUE_R, r.max.y - 4.0)))
                        .layout(egui::Layout::left_to_right(egui::Align::Center)),
                );
                if let Some(ParamValue::Float(v)) = val("opacity", "opacity") {
                    let (_, nv) = crate::widgets::hot_number(
                        &mut vui,
                        tl!("Opacity"),
                        egui::Id::unique(("pp-op", clip.0)),
                        v,
                        HotNumberOptions::new(0.5, (0.0, 100.0), 0, " %", &t),
                    );
                    if let Some(nv) = nv {
                        actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": "opacity", "param": "opacity", "value": nv})));
                    }
                }
                properties_nav(app, bui, r, clip, &it, mt, "opacity", "opacity", &mut actions);
            }
            let crop = eff_idx("crop");
            if section(bui, app, "Crop", crop.map(|i| (i, clip.0)), &mut actions) {
                for (label, p) in [(tl!("Left"), "left"), (tl!("Top"), "top"), (tl!("Right"), "right"), (tl!("Bottom"), "bottom")] {
                    let r = row(bui, label);
                    let mut vui = bui.new_child(
                        egui::UiBuilder::new()
                            .max_rect(Rect::from_min_max(pos2(r.min.x + 150.0, r.min.y + 4.0), pos2(r.max.x - PROPS_VALUE_R, r.max.y - 4.0)))
                            .layout(egui::Layout::left_to_right(egui::Align::Center)),
                    );
                    let v = val("crop", p).and_then(|v| v.as_f64()).unwrap_or(0.0);
                    let (_, nv) = crate::widgets::hot_number(
                        &mut vui,
                        label,
                        egui::Id::unique(("pp-crop", p, clip.0)),
                        v,
                        HotNumberOptions::new(0.2, (0.0, 100.0), 1, " %", &t),
                    );
                    if let Some(nv) = nv {
                        if crop.is_none() {
                            actions.push(("effects.apply".into(), json!({"clips": [clip.0], "effect": "crop"})));
                        }
                        actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": "crop", "param": p, "value": nv})));
                    }
                    properties_nav(app, bui, r, clip, &it, mt, "crop", p, &mut actions);
                }
            }
        }
        if let Some((clip, it, _)) = clips.iter().find(|c| c.2 == TrackKind::Audio)
            && section(bui, app, "Audio", it.effects.iter().position(|e| e.effect == "volume").map(|i| (i, clip.0)), &mut actions)
        {
            let clip = *clip;
            let mt = it.effect_time_at(ph.clamp(it.start, (it.end() - Tick(1)).max(it.start)));
            let val = |eid: &str, p: &str| it.effect(eid).and_then(|e| e.param(p)).map(|p| p.value_at(mt));
            let r = row(bui, tl!("Level"));
            let mut vui = bui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_max(pos2(r.min.x + 150.0, r.min.y + 4.0), pos2(r.max.x - PROPS_VALUE_R, r.max.y - 4.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            let v = val("volume", "level").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let (_, nv) = crate::widgets::hot_number(
                &mut vui,
                tl!("Level"),
                egui::Id::unique(("pp-vol", clip.0)),
                v,
                HotNumberOptions::new(0.1, (-96.0, 15.0), 1, " dB", &t),
            );
            if let Some(nv) = nv {
                actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": "volume", "param": "level", "value": nv})));
            }
            properties_nav(app, bui, r, clip, it, mt, "volume", "level", &mut actions);
            let r = row(bui, tl!("Pan"));
            let mut vui = bui.new_child(
                egui::UiBuilder::new()
                    .max_rect(Rect::from_min_max(pos2(r.min.x + 150.0, r.min.y + 4.0), pos2(r.max.x - PROPS_VALUE_R, r.max.y - 4.0)))
                    .layout(egui::Layout::left_to_right(egui::Align::Center)),
            );
            let v = val("panner", "balance").and_then(|v| v.as_f64()).unwrap_or(0.0);
            let (_, nv) = crate::widgets::hot_number(
                &mut vui,
                tl!("Pan"),
                egui::Id::unique(("pp-pan", clip.0)),
                v,
                HotNumberOptions::new(0.5, (-100.0, 100.0), 0, "", &t),
            );
            if let Some(nv) = nv {
                actions.push(("effects.setParam".into(), json!({"clip": clip.0, "effect": "panner", "param": "balance", "value": nv})));
            }
        }
        // speed footer
        bui.add_space(12.0);
        let (br, bresp) = bui.allocate_exact_size(vec2(118.0, 26.0), Sense::click());
        bui.painter().rect_filled(br, 4.0, if bresp.hovered() { t.hover } else { t.panel_bg });
        bui.painter().rect_stroke(br, 4.0, Stroke::new(1.0, t.separator), egui::StrokeKind::Inside);
        bui.painter().text(br.center(), Align2::CENTER_CENTER, tlf!("Speed {n}%", n = format!("{:.0}", it.speed * 100.0)), Tokens::ui(12.0), t.text);
        app.auto.add("properties.speed", br, "Speed");
        open_speed = bresp.clicked();
    });
    run(app, ui.ctx(), actions);
    // the footer button reads out the clip's speed and opens Clip ▸ Speed/Duration…, as in Premiere
    if open_speed && let Err(e) = crate::menus::invoke(app, ui.ctx(), "clip.speedDuration", json!({})) {
        app.ui.status = e;
    }
}

/// Where a row's highlight (hover, selection) is painted: the row less its first point, which is
/// where the focused panel's outline runs, so the highlight doesn't cut through the outline.
pub(crate) fn row_fill(r: Rect) -> Rect {
    Rect::from_min_max(pos2(r.min.x + 1.0, r.min.y), r.max)
}

/// Run the panel's actions (also the Lumetri Color panel's). Parameter changes made
/// while the mouse button is down (a drag) share one undo step, which the first change of each
/// press begins (#201); typed values and clicks stay separate steps. A drag value only changes once
/// the mouse moves, after the press frame, so the press that already began a step is remembered by
/// its start time. That memory is refreshed on every frame the button stays down — including frames
/// with no value change — so a Scale scrub that pauses between percents is still one undo (#887). A
/// change that brings its own `merge` key (a Program monitor handle changing several parameters)
/// keeps it.
pub(crate) fn run(app: &mut FilmcraftApp, ctx: &egui::Context, actions: Vec<(String, Value)>) {
    let (down, press) = ctx.input(|i| (i.pointer.any_down(), i.pointer.press_start_time()));
    let key = egui::Id::unique("effect-controls-drag-step");
    let begun = ctx.data(|d| d.get_temp::<Option<f64>>(key)).flatten();
    let mut wrote = false;
    for (cmd, mut p) in actions {
        if cmd == "ui.effectControls.copyEffects" {
            copy_selected_effects(app);
            continue;
        }
        if cmd == "ui.effectControls.pasteEffects" {
            if let Some((clip, _, _)) = selected_clip(app) {
                paste_effects(app, clip);
            }
            continue;
        }
        let removed: Option<(ClipId, Vec<usize>)> = match cmd.as_str() {
            "effects.removeMany" => p
                .get("clip")
                .and_then(Value::as_u64)
                .map(ClipId)
                .zip(
                    p.get("indices")
                        .and_then(Value::as_array)
                        .map(|indices| indices.iter().filter_map(Value::as_u64).filter_map(|index| usize::try_from(index).ok()).collect()),
                )
                .filter(|(_, indices): &(ClipId, Vec<usize>)| !indices.is_empty()),
            "effects.remove" => p
                .get("clip")
                .and_then(Value::as_u64)
                .map(ClipId)
                .zip(p.get("index").and_then(Value::as_u64).and_then(|index| usize::try_from(index).ok()).map(|index| vec![index])),
            _ => None,
        };
        let before =
            removed.as_ref().and_then(|(clip, _)| app.session.active_sequence().and_then(|q| q.find_item(*clip)).map(|(_, item)| item.effects.clone()));
        let deleted_keys: Option<Vec<KeyframeRef>> =
            (cmd == "effects.deleteKeyframes").then(|| serde_json::from_value(p.get("keyframes").cloned().unwrap_or(Value::Null)).ok()).flatten();
        if cmd == "effects.setParam" && down {
            if p.get("merge").is_none() {
                p["merge"] = json!(true);
            }
            // only the first write of a frame begins the step: a handle that changes several
            // parameters at once (position and anchor point) sends them together
            p["begin"] = json!(begun != press && !wrote);
            wrote = true;
        }
        match app.session.execute(&cmd, p) {
            Ok(_) if let (Some((clip, indices)), Some(before)) = (&removed, before) => {
                if let Some((_, after)) = app.session.active_sequence().and_then(|q| q.find_item(*clip)) {
                    let mut remapped: Vec<_> = app.ui.selected_effects.iter().filter(|selection| selection.clip != *clip).cloned().collect();
                    remapped.extend(app.ui.selected_effects.iter().filter(|selection| selection.clip == *clip).filter_map(|selection| {
                        let old = selection.index(&before)?;
                        if indices.contains(&old) {
                            return None;
                        }
                        let shift = indices.iter().filter(|removed| **removed < old).count();
                        filmcraft_engine::EffectSel::of(*clip, &after.effects, old.saturating_sub(shift))
                    }));
                    app.ui.selected_effects = remapped;
                    app.ui.keyframe_selection.retain_mut(|key| {
                        if key.clip != clip.0 {
                            return true;
                        }
                        if indices.contains(&key.effect) {
                            return false;
                        }
                        let shift = indices.iter().filter(|removed| **removed < key.effect).count();
                        if shift > 0 {
                            key.effect = key.effect.saturating_sub(shift);
                        }
                        true
                    });
                }
            }
            Ok(_) if let Some(deleted) = deleted_keys => app.ui.keyframe_selection.retain(|key| !deleted.contains(key)),
            Err(error) => app.ui.status = error.to_string(),
            _ => {}
        }
    }
    if down && (wrote || begun.is_some()) {
        ctx.data_mut(|d| d.insert_temp(key, press));
    } else if !down {
        ctx.data_mut(|d| d.remove::<Option<f64>>(key));
    }
}

pub(crate) fn graph_key(clip: ClipId, idx: usize, pid: &str) -> String {
    format!("graph:{}:{}:{}", clip.0, idx, pid)
}

/// Parameters with value/velocity graphs: scalars, and points such as Position (#239).
pub(crate) fn graphable(v: &ParamValue) -> bool {
    matches!(v, ParamValue::Float(_) | ParamValue::Vec2(_))
}

/// The curves a graphed value draws: a scalar's one, or a point's X and Y.
fn graph_components(v: &ParamValue) -> Vec<f64> {
    match v {
        ParamValue::Float(x) => vec![*x],
        ParamValue::Vec2(p) => vec![p.x, p.y],
        _ => Vec::new(),
    }
}

/// Value and velocity graphs of an animated scalar or point parameter, drawn across the keyframe
/// lane. A point draws an X and a Y curve, and its velocity graph is the speed along its path.
/// Keyframes drag vertically (value); Bezier influence handles drag horizontally.
#[allow(clippy::too_many_arguments)]
pub(crate) fn graph_rows(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    body: Rect,
    clip: ClipId,
    idx: usize,
    mask: Option<usize>,
    pd: &filmcraft_project::ParamDef,
    param: &filmcraft_project::Param,
    lane: &Rect,
    timeline_range: (Tick, Tick),
    it: &TrackItem,
    actions: &mut Vec<(String, Value)>,
) {
    let t = app.tokens;
    let (vr, _) = ui.allocate_exact_size(vec2(body.width(), 110.0), Sense::hover());
    let (velr, _) = ui.allocate_exact_size(vec2(body.width(), 64.0), Sense::hover());
    row_line(ui, velr, lane, &t);
    let speed = if it.speed.is_finite() { it.speed.abs().max(1e-6) } else { 1.0 };
    let shown_duration = timeline_range.1.0.saturating_sub(timeline_range.0.0).max(1);
    let timeline_at = |f: f64| Tick(timeline_range.0.0.saturating_add((f * shown_duration as f64).round() as i64));
    let to_media = |f: f64| it.source_time_at(timeline_at(f));
    let to_f = |m: Tick| {
        let time = timeline_time_of(it, m);
        (time.0.saturating_sub(timeline_range.0.0) as f64 / shown_duration as f64) as f32
    };
    let x_of = |f: f32| lane.min.x + f * lane.width();
    // samples: one row of curve values per sample (a point's NaN "auto" coordinate is skipped)
    let n = ((lane.width() / 2.0).max(8.0) as usize).min(4096);
    let vals: Vec<Vec<f64>> = (0..=n).map(|i| graph_components(&param.value_at(to_media(i as f64 / n as f64)))).collect();
    let curves = graph_components(&param.value).len();
    let at_keys: Vec<Vec<f64>> = param.keyframes.iter().map(|k| graph_components(&k.value)).collect();
    let (mut lo, mut hi) = vals.iter().chain(&at_keys).flatten().filter(|v| v.is_finite()).fold((f64::MAX, f64::MIN), |(a, b), v| (a.min(*v), b.max(*v)));
    if lo > hi {
        (lo, hi) = (0.0, 0.0);
    }
    if hi - lo < 1e-6 {
        lo -= 1.0;
        hi += 1.0;
    }
    let pad = (hi - lo) * 0.12;
    let (mut lo, mut hi) = (lo - pad, hi + pad);
    if let ParamKind::Float { min, max, .. } = pd.kind {
        lo = lo.max(min);
        hi = hi.min(max);
    }
    // While the graph stays open its range only widens: refitted to the keyframes, a keyframe
    // dragged off the top or bottom edge was drawn back on that edge, the curve unchanged (#1918)
    let range_id = egui::Id::unique(("kfg-range", clip.0, idx, mask, pd.id));
    let frame = ui.ctx().cumulative_frame_nr();
    if let Some((a, b, seen)) = ui.data(|d| d.get_temp::<(f64, f64, u64)>(range_id))
        && frame.saturating_sub(seen) <= 1
    {
        (lo, hi) = (lo.min(a), hi.max(b));
    }
    ui.data_mut(|d| d.insert_temp(range_id, (lo, hi, frame)));
    let area = Rect::from_min_max(pos2(lane.min.x, vr.min.y + 4.0), pos2(lane.max.x, vr.max.y - 4.0));
    let y_of = |v: f64| area.max.y - ((v - lo) / (hi - lo)) as f32 * area.height();
    let p = ui.painter();
    p.rect_filled(area, 0.0, t.keyframe_plot_bg);
    for g in 1..4 {
        let y = area.min.y + area.height() * g as f32 / 4.0;
        p.line_segment([pos2(area.min.x, y), pos2(area.max.x, y)], Stroke::new(1.0, t.keyframe_plot_grid));
    }
    // range labels in the property column
    let dec = if let ParamKind::Float { decimals, .. } = pd.kind { decimals as usize } else { 1 };
    p.text(pos2(vr.max.x - 30.0, area.min.y + 6.0), Align2::RIGHT_CENTER, format!("{hi:.dec$}"), Tokens::ui(10.0), t.text_dim);
    p.text(pos2(vr.max.x - 30.0, area.max.y - 6.0), Align2::RIGHT_CENTER, format!("{lo:.dec$}"), Tokens::ui(10.0), t.text_dim);
    p.text(pos2(vr.min.x + 44.0, area.center().y), Align2::LEFT_CENTER, tl!("Value"), Tokens::ui(11.0), t.text_dim);
    let curve_color = |c: usize| if c == 0 { t.accent } else { Y_CURVE };
    if curves > 1 {
        p.text(pos2(vr.min.x + 44.0, area.center().y + 14.0), Align2::LEFT_CENTER, "X", Tokens::ui(10.5), curve_color(0));
        p.text(pos2(vr.min.x + 58.0, area.center().y + 14.0), Align2::LEFT_CENTER, "Y", Tokens::ui(10.5), curve_color(1));
    }
    for c in 0..curves {
        let line: Vec<Pos2> =
            vals.iter().enumerate().filter_map(|(i, v)| v.get(c).filter(|v| v.is_finite()).map(|v| pos2(x_of(i as f32 / n as f32), y_of(*v)))).collect();
        p.add(egui::Shape::line(line, Stroke::new(1.5, curve_color(c))));
    }
    // velocity (units per second, derivative of the sampled value; a point's speed along its path)
    let secs = (shown_duration as f64 * speed / filmcraft_time::TICKS_PER_SECOND as f64 / n as f64).max(1e-9);
    let vel: Vec<f64> = vals
        .windows(2)
        .map(|w| {
            let [a, b] = w else { return 0.0 };
            let d: Vec<f64> = a.iter().zip(b).map(|(a, b)| (b - a) / secs).collect();
            match d.as_slice() {
                [v] => *v,
                d => d.iter().map(|v| v * v).sum::<f64>().sqrt(),
            }
        })
        .collect();
    let vmax = vel.iter().filter(|v| v.is_finite()).fold(1e-6f64, |a, v| a.max(v.abs())) * 1.15;
    let varea = Rect::from_min_max(pos2(lane.min.x, velr.min.y + 2.0), pos2(lane.max.x, velr.max.y - 4.0));
    p.rect_filled(varea, 0.0, t.keyframe_plot_bg);
    let vy = |v: f64| varea.center().y - (v / vmax) as f32 * varea.height() / 2.0;
    p.line_segment([pos2(varea.min.x, varea.center().y), pos2(varea.max.x, varea.center().y)], Stroke::new(1.0, t.keyframe_plot_axis));
    let vline: Vec<Pos2> = vel.iter().enumerate().filter(|(_, v)| v.is_finite()).map(|(i, v)| pos2(x_of((i as f32 + 0.5) / n as f32), vy(*v))).collect();
    p.add(egui::Shape::line(vline, Stroke::new(1.2, Color32::from_rgb(0xd0, 0xa0, 0x40))));
    p.text(pos2(velr.min.x + 44.0, varea.center().y), Align2::LEFT_CENTER, tl!("Velocity"), Tokens::ui(11.0), t.text_dim);
    p.text(pos2(velr.max.x - 30.0, varea.min.y + 6.0), Align2::RIGHT_CENTER, format!("{vmax:.1}/s"), Tokens::ui(10.0), t.text_dim);
    let p = p.clone();
    // keyframes + handles
    draw_graph_keyframes(app, ui, &p, clip, idx, mask, pd, param, it, lane, lo, hi, area, actions);
}

/// One parameter's keyframe diamonds on its graph, with the Bezier influence handles.
#[allow(clippy::too_many_arguments)]
fn draw_graph_keyframes(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    p: &egui::Painter,
    clip: ClipId,
    idx: usize,
    mask: Option<usize>,
    pd: &filmcraft_project::ParamDef,
    param: &filmcraft_project::Param,
    it: &TrackItem,
    lane: &Rect,
    lo: f64,
    hi: f64,
    area: Rect,
    actions: &mut Vec<(String, Value)>,
) {
    let t = app.tokens;
    let speed = it.speed.abs().max(1e-6);
    let dur = it.duration.0.max(1) as f64;
    let to_f = |m: Tick| ((m - it.source_in).0 as f64 / speed / dur) as f32;
    let x_of = |f: f32| lane.min.x + f * lane.width();
    let y_of = |v: f64| area.max.y - ((v - lo) / (hi - lo)) as f32 * area.height();
    let v_of = |y: f32| lo + ((area.max.y - y) / area.height()) as f64 * (hi - lo);
    let dec = if let ParamKind::Float { decimals, .. } = pd.kind { decimals as usize } else { 1 };
    let with_mask = |mut v: Value| -> Value {
        if let Some(k) = mask {
            v["mask"] = json!(k);
        }
        v
    };
    // keyframes + handles
    let ks = &param.keyframes;
    for (i, k) in ks.iter().enumerate() {
        let f = to_f(k.time);
        if !(-0.01..=1.01).contains(&f) {
            continue;
        }
        let eases_out =
            !matches!(k.interp, filmcraft_project::Interpolation::Linear | filmcraft_project::Interpolation::Hold | filmcraft_project::Interpolation::EaseIn);
        let eases_in =
            !matches!(k.interp, filmcraft_project::Interpolation::Linear | filmcraft_project::Interpolation::Hold | filmcraft_project::Interpolation::EaseOut);
        let comps = graph_components(&k.value);
        for (ci, &v) in comps.iter().enumerate() {
            if !v.is_finite() {
                continue;
            }
            let id = egui::Id::unique(("kfg", clip.0, idx, mask, pd.id, k.time.0));
            let id = if ci == 0 { id } else { id.with(ci) };
            let dy: f32 = ui.data(|d| d.get_temp(id)).unwrap_or(0.0);
            let c = pos2(x_of(f), y_of(v) + dy);
            let r = Rect::from_center_size(c, vec2(10.0, 10.0));
            let resp = ui.interact(r.expand(2.0), id, Sense::click_and_drag());
            resp.context_menu(|ui| {
                keyframe_menu(app, ui, KeyframeRef { clip: clip.0, effect: idx, param: pd.id.to_string(), mask, time: k.time }, k.interp, actions);
            });
            let auto_id = match (comps.len(), ci) {
                (1, _) => format!("effectControls.{}.graph.keyframe.{}", pd.id, k.time.0),
                (_, 0) => format!("effectControls.{}.graph.keyframe.{}.x", pd.id, k.time.0),
                _ => format!("effectControls.{}.graph.keyframe.{}.y", pd.id, k.time.0),
            };
            app.auto.add(&auto_id, r, "keyframe value");
            // influence handles: flat (ease) tangents with length ∝ influence × neighbouring segment;
            // a point's sit on its X curve only, as both coordinates share the influence
            let (eases_out, eases_in) = (ci == 0 && eases_out, ci == 0 && eases_in);
            for (side, on, nb) in [(1.0f32, eases_out, ks.get(i + 1)), (-1.0f32, eases_in && i > 0, if i > 0 { ks.get(i - 1) } else { None })] {
                let (true, Some(nb)) = (on, nb) else { continue };
                let seg = (x_of(to_f(nb.time)) - c.x).abs();
                let infl = if side > 0.0 { k.out_influence } else { k.in_influence } as f32;
                let hid = id.with(if side > 0.0 { "out" } else { "in" });
                let hdx: f32 = ui.data(|d| d.get_temp(hid)).unwrap_or(0.0);
                let hx = c.x + side * (infl * seg + hdx * side).clamp(seg * 0.01, seg);
                let hp = pos2(hx, c.y);
                p.line_segment([c, hp], Stroke::new(1.0, t.plot_handle_dim));
                p.circle_filled(hp, 3.5, t.keyframe_handle);
                let hr = ui.interact(Rect::from_center_size(hp, vec2(10.0, 10.0)), hid.with("h"), Sense::drag());
                if hr.dragged_by(egui::PointerButton::Primary) {
                    let nx = graph_drag_offset(ui, &hr, vec2(hdx, 0.0)).x;
                    ui.data_mut(|d| d.insert_temp(hid, nx));
                }
                if hr.drag_stopped() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                    ui.data_mut(|d| d.remove::<f32>(hid));
                } else if hr.drag_stopped_by(egui::PointerButton::Primary) {
                    ui.data_mut(|d| d.remove::<f32>(hid));
                    let ni = ((infl * seg + hdx * side) / seg.max(1.0)).clamp(0.01, 1.0);
                    let key = if side > 0.0 { "outInfluence" } else { "inInfluence" };
                    actions.push((
                        "effects.setKeyframe".into(),
                        with_mask(json!({"clip": clip.0, "effect": idx, "param": pd.id, "mediaTime": k.time.0, key: ni})),
                    ));
                }
            }
            p.circle_filled(c, 4.5, if resp.dragged_by(egui::PointerButton::Primary) { t.hot_text } else { t.plot_handle });
            if resp.dragged_by(egui::PointerButton::Primary) {
                let ny = graph_drag_offset(ui, &resp, vec2(0.0, dy)).y;
                ui.data_mut(|d| d.insert_temp(id, ny));
                p.text(c + vec2(8.0, -10.0), Align2::LEFT_BOTTOM, format!("{:.dec$}", v_of(c.y)), Tokens::ui(10.5), t.hot_text);
            }
            // egui ends a drag on Escape: that stop abandons it (#580)
            if resp.drag_stopped() && ui.input(|i| i.key_pressed(egui::Key::Escape)) {
                ui.data_mut(|d| d.remove::<f32>(id));
            } else if resp.drag_stopped_by(egui::PointerButton::Primary) {
                ui.data_mut(|d| d.remove::<f32>(id));
                let nv = v_of(c.y);
                let nv = if let ParamKind::Float { min, max, .. } = pd.kind { nv.clamp(min, max) } else { nv };
                let value = if comps.len() == 1 {
                    json!(nv)
                } else {
                    let mut moved = comps.clone();
                    if let Some(slot) = moved.get_mut(ci) {
                        *slot = nv;
                    }
                    json!(moved)
                };
                actions.push((
                    "effects.setKeyframe".into(),
                    with_mask(json!({"clip": clip.0, "effect": idx, "param": pd.id, "mediaTime": k.time.0, "value": value})),
                ));
            }
        }
    }
}

/// Read-only text for parameters without an inline editor (LUT references, free text, curves).
fn param_text(project: &filmcraft_project::Project, id: &str, value: &ParamValue) -> String {
    match value {
        ParamValue::Text(s) if id.ends_with("_lut") => filmcraft_render::luts::label(Some(project), s),
        ParamValue::Text(s) if s.is_empty() => tl!("None").into(),
        ParamValue::Text(s) => s.clone(),
        ParamValue::Curve(pts) if pts.is_empty() || *pts == [[0.0, 0.0], [1.0, 1.0]] => tl!("Default").into(),
        ParamValue::Curve(pts) => tlf!("Custom ({n} points)", n = pts.len()),
        other => format!("{other:?}"),
    }
}

#[cfg(test)]
mod param_text_tests {
    use super::*;

    #[test]
    fn readable_values_for_text_lut_and_curve_params() {
        let p = filmcraft_project::Project::default();
        assert_eq!(param_text(&p, "input_lut", &ParamValue::Text(String::new())), "None");
        assert_eq!(param_text(&p, "look_lut", &ParamValue::Text("builtin:look-teal-orange".into())), "Teal & Orange");
        assert_eq!(param_text(&p, "label", &ParamValue::Text(String::new())), "None");
        assert_eq!(param_text(&p, "label", &ParamValue::Text("Reel 3".into())), "Reel 3");
        assert_eq!(param_text(&p, "curve_luma", &ParamValue::Curve(vec![[0.0, 0.0], [1.0, 1.0]])), "Default");
        assert_eq!(param_text(&p, "hue_vs_sat", &ParamValue::Curve(vec![])), "Default");
        assert_eq!(param_text(&p, "curve_luma", &ParamValue::Curve(vec![[0.0, 0.0], [0.5, 0.6], [1.0, 1.0]])), "Custom (3 points)");
    }
}

#[cfg(test)]
mod drag_undo_tests {
    use serde_json::json;

    /// #201: a drag value changes only once the mouse moves, after the press frame; two drags of
    /// the same parameter are still two undo steps.
    #[test]
    fn each_press_begins_its_own_undo_step() {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
        let mut app = crate::FilmcraftApp::new(s);
        let ctx = egui::Context::default();
        let opacity = |app: &crate::FilmcraftApp| {
            let q = app.session.active_sequence().unwrap();
            let it = q.find_item(filmcraft_project::ClipId(clip)).unwrap().1;
            it.effects.iter().find(|e| e.effect == "opacity").unwrap().params["opacity"].value.as_f64().unwrap()
        };
        let start = opacity(&app);
        let pos = egui::pos2(10.0, 10.0);
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        let mut time = 0.0;
        let mut frame = |app: &mut crate::FilmcraftApp, events: Vec<egui::Event>, value: Option<f64>| {
            time += 0.1;
            let raw = egui::RawInput { time: Some(time), events, ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                let actions = value.map(|v| ("effects.setParam".to_string(), json!({"clip": clip, "effect": "opacity", "param": "opacity", "value": v})));
                super::run(app, ui.ctx(), actions.into_iter().collect());
            });
            out.textures_delta.clear();
        };
        for values in [[90.0, 70.0], [50.0, 60.0]] {
            frame(&mut app, vec![egui::Event::PointerMoved(pos), button(true)], None); // the press: nothing changes yet
            for v in values {
                frame(&mut app, vec![egui::Event::PointerMoved(pos)], Some(v));
            }
            frame(&mut app, vec![button(false)], None);
        }
        assert_eq!(opacity(&app), 60.0);
        app.session.undo();
        assert_eq!(opacity(&app), 70.0, "undo takes back only the second drag");
        app.session.undo();
        assert_eq!(opacity(&app), start, "and then the first");
    }

    /// #887: a Scale hot-text drag that pauses between percents (no `setParam` that frame) is
    /// still one undo, not one step per 1%.
    #[test]
    fn a_scale_drag_with_idle_frames_is_one_undo_step() {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id.0;
        let mut app = crate::FilmcraftApp::new(s);
        let ctx = egui::Context::default();
        let scale = |app: &crate::FilmcraftApp| {
            let q = app.session.active_sequence().unwrap();
            let it = q.find_item(filmcraft_project::ClipId(clip)).unwrap().1;
            it.effects.iter().find(|e| e.effect == "motion").unwrap().params["scale"].value.as_f64().unwrap()
        };
        let start = scale(&app);
        let pos = egui::pos2(10.0, 10.0);
        let button = |pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        let mut time = 0.0;
        let mut frame = |app: &mut crate::FilmcraftApp, events: Vec<egui::Event>, value: Option<f64>| {
            time += 0.1;
            let raw = egui::RawInput { time: Some(time), events, ..Default::default() };
            let mut out = ctx.run_ui(raw, |ui| {
                let actions = value.map(|v| ("effects.setParam".to_string(), json!({"clip": clip, "effect": "motion", "param": "scale", "value": v})));
                super::run(app, ui.ctx(), actions.into_iter().collect());
            });
            out.textures_delta.clear();
        };
        frame(&mut app, vec![egui::Event::PointerMoved(pos), button(true)], None);
        for v in [101.0, 150.0, 200.0, 249.0] {
            frame(&mut app, vec![egui::Event::PointerMoved(pos)], Some(v));
            frame(&mut app, vec![egui::Event::PointerMoved(pos)], None); // idle: mouse down, no 1% tick
        }
        frame(&mut app, vec![button(false)], None);
        assert_eq!(scale(&app), 249.0);
        app.session.undo();
        assert_eq!(scale(&app), start, "one undo restores the scale from before the drag");
        assert_eq!(app.session.undo(), None, "the drag must not have left extra 1% steps");
    }
}

#[cfg(test)]
mod lane_tests {
    use super::*;

    /// A context with the app's fonts (the panels use the semibold family).
    fn context() -> egui::Context {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &Tokens::for_kind(Default::default()));
        ctx
    }

    /// A project file is only checked for overflow, so a damaged one can carry a clip with no
    /// (or a negative) duration. Selecting it must still draw both panels, ruler, playhead and
    /// keyframe navigators included.
    #[test]
    fn a_clip_without_a_duration_still_draws() {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let clip = s.active_sequence().unwrap().video_tracks[0].items[0].id;
        s.execute("timeline.select", json!({"clips": [clip.0]})).unwrap();
        s.execute("effects.toggleAnimation", json!({"clip": clip.0, "effect": "motion", "param": "scale"})).unwrap();
        let seq = s.state.active_sequence.unwrap();
        let mut app = crate::FilmcraftApp::new(s);
        let ctx = context();
        for duration in [0, -5 * filmcraft_time::TICKS_PER_SECOND] {
            let project = std::sync::Arc::make_mut(&mut app.session.project);
            project.sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1.duration = Tick(duration);
            let rect = Rect::from_min_size(pos2(0.0, 0.0), vec2(700.0, 500.0));
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                show(&mut app, ui, rect);
                properties_panel(&mut app, ui, rect);
            });
            out.textures_delta.clear();
        }
    }

    /// The ruler takes its numbers from the project (clip times, the sequence's frame rate) and
    /// the panel's size. Anything a project that opens can carry (times within `Tick::MIN..=MAX`,
    /// a rate within `SequenceSettings::validate`, or an unset one) must not make it panic or
    /// tick forever.
    #[test]
    fn ruler_survives_damaged_numbers() {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let mut it = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
        let ctx = context();
        let t = Tokens::for_kind(Default::default());
        let big = i64::from(u32::MAX);
        let rates = [(24000, 1001), (30, 1), (1000, 1), (1, 1), (1, big), (big, big), (0, 0), (-25, 1)];
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            for width in [300.0, 0.0, -40.0, 0.001, 1.0e9, f32::NAN, f32::INFINITY] {
                for duration in [1, 0, -7, filmcraft_time::TICKS_PER_SECOND, Tick::MAX.0 / 2] {
                    for start in [0, Tick::MIN.0, Tick::MAX.0 / 2] {
                        for (num, den) in rates {
                            it.start = Tick(start);
                            it.duration = Tick(duration);
                            let ruler = Rect::from_min_size(pos2(10.0, 10.0), vec2(width, RULER_H));
                            paint_ruler(ui.painter(), ruler, it.start, it.end(), filmcraft_time::FrameRate { num, den }, false, &t);
                        }
                    }
                }
            }
        });
        out.textures_delta.clear();
    }

    /// A five-second lane 300 px wide is labelled every two seconds, in sequence timecode.
    #[test]
    fn ruler_labels_sequence_timecode() {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let mut it = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
        let rate = filmcraft_time::FrameRate { num: 24, den: 1 };
        it.start = rate.tick_of(24 * 60);
        it.duration = rate.tick_of(24 * 5);
        let ctx = context();
        let t = Tokens::for_kind(Default::default());
        let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
            paint_ruler(ui.painter(), Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, RULER_H)), it.start, it.end(), rate, false, &t);
        });
        out.textures_delta.clear();
        fn texts(s: egui::Shape, out: &mut Vec<String>) {
            match s {
                egui::Shape::Vec(v) => v.into_iter().for_each(|s| texts(s, out)),
                egui::Shape::Text(t) => out.push(t.galley.text().to_string()),
                _ => {}
            }
        }
        let mut labels = Vec::new();
        out.shapes.into_iter().for_each(|c| texts(c.shape, &mut labels));
        assert_eq!(labels, ["00:01:00:00", "00:01:02:00", "00:01:04:00"]);

        // A zoomed view starts between ruler ticks: its first label must be a tick inside the
        // visible range, not the previous timecode clamped onto the left edge.
        let zoom_start = rate.tick_of(24 * 60 + 7);
        let zoom_end = rate.tick_of(24 * 65);
        let mut zoomed = ctx.run_ui(egui::RawInput::default(), |ui| {
            paint_ruler(ui.painter(), Rect::from_min_size(pos2(0.0, 0.0), vec2(300.0, RULER_H)), zoom_start, zoom_end, rate, false, &t);
        });
        zoomed.textures_delta.clear();
        let mut labels = Vec::new();
        zoomed.shapes.into_iter().for_each(|c| texts(c.shape, &mut labels));
        assert_eq!(labels.first().map(String::as_str), Some("00:01:02:00"));
    }
}

#[cfg(test)]
mod graph_drag_tests {
    use super::*;
    use filmcraft_project::{Interpolation, Keyframe, Param};

    struct Driver {
        app: FilmcraftApp,
        ctx: egui::Context,
        clip: ClipId,
        time: f64,
        param: &'static str,
    }

    impl Driver {
        fn new() -> Self {
            Self::with("scale", ParamValue::Float(40.0), ParamValue::Float(80.0))
        }

        fn with(param: &'static str, from: ParamValue, to: ParamValue) -> Self {
            let mut session = filmcraft_engine::Session::default();
            session.execute("file.openDemoProject", json!({})).unwrap();
            let seq = session.state.active_sequence.unwrap();
            let clip = session.active_sequence().unwrap().video_tracks[0].items[0].id;
            let it = std::sync::Arc::make_mut(&mut session.project).sequence_mut(seq).unwrap().find_item_mut(clip).unwrap().1;
            let mut a = Keyframe::new(it.source_in + Tick(it.duration.0 / 4), from.clone());
            let mut b = Keyframe::new(it.source_in + Tick(it.duration.0 * 3 / 4), to);
            a.interp = Interpolation::Bezier;
            b.interp = Interpolation::Bezier;
            it.effects.iter_mut().find(|e| e.effect == "motion").unwrap().params.insert(param.into(), Param { value: from, keyframes: vec![a, b] });
            let app = FilmcraftApp::new(session);
            let ctx = egui::Context::default();
            crate::theme::install(&ctx, &app.tokens);
            Self { app, ctx, clip, time: 0.0, param }
        }

        fn param(&self) -> Param {
            self.app.session.active_sequence().unwrap().find_item(self.clip).unwrap().1.effect("motion").unwrap().params[self.param].clone()
        }

        fn undo_len(&mut self) -> usize {
            self.app.session.execute("history.list", json!({})).unwrap()["undo"].as_array().unwrap().len()
        }

        // Draw the actual scalar graph and execute its emitted command, as the panel does.
        // Current paint shapes give a real held-frame position rather than previous telemetry.
        fn frame(&mut self, events: Vec<egui::Event>) -> [Pos2; 2] {
            self.time += 0.1;
            self.app.auto.begin_frame();
            let mut out = self.ctx.run_ui(egui::RawInput { time: Some(self.time), events, ..Default::default() }, |ui| {
                let it = self.app.session.active_sequence().unwrap().find_item(self.clip).unwrap().1.clone();
                let index = it.effects.iter().position(|e| e.effect == "motion").unwrap();
                let effect = &it.effects[index];
                let pd = effect.def().unwrap().param(self.param).unwrap();
                let body = Rect::from_min_size(pos2(8.0, 8.0), vec2(320.0, 200.0));
                let lane = Rect::from_min_max(pos2(360.0, 8.0), pos2(760.0, 220.0));
                let mut actions = Vec::new();
                graph_rows(&mut self.app, ui, body, self.clip, index, None, pd, &effect.params[self.param], &lane, (it.start, it.end()), &it, &mut actions);
                run(&mut self.app, ui.ctx(), actions);
            });
            out.textures_delta.clear();
            fn first_circle(shape: &egui::Shape, radius: f32) -> Option<Pos2> {
                match shape {
                    egui::Shape::Circle(circle) if circle.radius == radius => Some(circle.center),
                    egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| first_circle(shape, radius)),
                    _ => None,
                }
            }
            [4.5, 3.5].map(|radius| {
                out.shapes.iter().find_map(|shape| first_circle(&shape.shape, radius)).expect("scalar keyframe and outgoing influence are painted")
            })
        }
    }

    fn button(pos: Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }

    /// #1918, #1921: a keyframe dragged to a new value stays where it was dropped. The graph
    /// used to fit itself to the new values, so with two keyframes the one at the bottom (or
    /// the top) went straight back there and the curve looked unchanged.
    #[test]
    fn a_dropped_graph_keyframe_stays_where_it_was_dropped() {
        for (key, offset) in [(0, vec2(0.0, -20.0)), (1, vec2(0.0, 20.0))] {
            let mut d = Driver::new();
            let before = d.param();
            d.frame(vec![]);
            let id = format!("effectControls.scale.graph.keyframe.{}", before.keyframes[key].time.0);
            let [x, y, w, h] = d.app.auto.find(&id).expect("the keyframe is drawn").rect;
            let start = pos2(x + w / 2.0, y + h / 2.0);
            d.frame(vec![egui::Event::PointerMoved(start), button(start, true)]);
            d.frame(vec![egui::Event::PointerMoved(start + offset)]);
            d.frame(vec![button(start + offset, false)]);
            d.frame(vec![]);
            d.frame(vec![]);
            let changed = d.param();
            assert_ne!(changed.keyframes[key].value, before.keyframes[key].value, "keyframe {key} takes the dropped value");
            let [x, y, w, h] = d.app.auto.find(&id).expect("the keyframe is drawn").rect;
            let now = pos2(x + w / 2.0, y + h / 2.0);
            assert!(now.distance(start + offset) < 0.5, "keyframe {key} stays where it was dropped: dropped at {:?}, drawn at {now:?}", start + offset);
        }
    }

    #[test]
    fn scalar_graph_press_starts_at_the_pointer_origin_and_release_is_one_undo_step() {
        for influence in [false, true] {
            for batched in [true, false] {
                let mut d = Driver::new();
                let before = d.param();
                let undo = d.undo_len();
                let target = usize::from(influence);
                let start = d.frame(vec![])[target];
                // A different prior cursor makes the pre-press movement observable.
                d.frame(vec![egui::Event::PointerMoved(start - vec2(80.0, 40.0))]);
                if batched {
                    d.frame(vec![egui::Event::PointerMoved(start), button(start, true)]);
                } else {
                    d.frame(vec![egui::Event::PointerMoved(start)]);
                    d.frame(vec![button(start, true)]);
                }
                let pressed = d.frame(vec![])[target];
                assert!(pressed.distance(start) < 0.05, "press cannot jump: influence={influence}, batched={batched}, start={start:?}, pressed={pressed:?}");
                assert_eq!(d.param(), before);
                assert_eq!(d.undo_len(), undo);
                let offset = if influence { vec2(20.0, 0.0) } else { vec2(0.0, -20.0) };
                d.frame(vec![egui::Event::PointerMoved(start + offset)]);
                let preview = d.frame(vec![])[target];
                assert!(
                    preview.distance(start + offset) < 0.05,
                    "preview follows only gesture motion: influence={influence}, batched={batched}, start={start:?}, preview={preview:?}"
                );
                assert_eq!(d.param(), before, "held preview cannot commit");
                assert_eq!(d.undo_len(), undo);
                d.frame(vec![button(start + offset, false)]);
                d.frame(vec![]);
                let changed = d.param();
                assert_eq!(d.undo_len(), undo + 1);
                assert_eq!(changed.keyframes[0].time, before.keyframes[0].time);
                assert_eq!(changed.keyframes[1], before.keyframes[1]);
                if influence {
                    assert_eq!(changed.keyframes[0].value, before.keyframes[0].value);
                    assert!(changed.keyframes[0].out_influence > before.keyframes[0].out_influence);
                } else {
                    assert!(changed.keyframes[0].value.as_f64().unwrap() > before.keyframes[0].value.as_f64().unwrap());
                    assert_eq!(changed.keyframes[0].out_influence, before.keyframes[0].out_influence);
                }
                d.app.session.undo().expect("one graph gesture to undo");
                assert_eq!(d.param(), before);
                assert_eq!(d.undo_len(), undo);
                d.app.session.redo().expect("the graph gesture to redo");
                assert_eq!(d.param(), changed);
                assert_eq!(d.undo_len(), undo + 1);
            }
        }
    }

    /// #239: Position (a point) has graphs too; dragging its X keyframe moves only X, in one undo step.
    #[test]
    fn position_graph_drags_one_coordinate_of_a_point_keyframe() {
        use filmcraft_geom::Vec2;
        let mut d = Driver::with("position", ParamValue::Vec2(Vec2::new(400.0, 300.0)), ParamValue::Vec2(Vec2::new(800.0, 500.0)));
        assert!(graphable(&d.param().value));
        let before = d.param();
        let undo = d.undo_len();
        let start = d.frame(vec![])[0];
        assert!(d.app.auto.find(&format!("effectControls.position.graph.keyframe.{}.y", before.keyframes[0].time.0)).is_some());
        d.frame(vec![egui::Event::PointerMoved(start), button(start, true)]);
        d.frame(vec![egui::Event::PointerMoved(start - vec2(0.0, 20.0))]);
        d.frame(vec![button(start - vec2(0.0, 20.0), false)]);
        d.frame(vec![]);
        let changed = d.param();
        assert_eq!(d.undo_len(), undo + 1);
        let (old, new) = (before.keyframes[0].value.as_vec2().unwrap(), changed.keyframes[0].value.as_vec2().unwrap());
        assert!(new.x > old.x, "dragging the X keyframe up raises X: {old:?} -> {new:?}");
        assert_eq!(new.y, old.y);
        assert_eq!(changed.keyframes[1], before.keyframes[1]);
    }
}

#[cfg(test)]
mod frame_hold_tests {
    //! #1675: on a Frame Hold clip with Hold Filters off the effects keep animating through the
    //! hold, so the panels that show effect values read them where the render does, as Effect
    //! Controls does, and not on the held frame.
    use super::*;
    use crate::panels::audio_fx_editor::FxTarget;
    use filmcraft_geom::Vec2;
    use filmcraft_project::{Keyframe, Param};

    #[derive(Clone, Copy, Debug)]
    enum Panel {
        Lumetri,
        Masks,
        TransformHandles,
        AudioFxEditor,
    }

    fn animate(it: &mut TrackItem, effect: &str, param: &str, from: ParamValue, to: ParamValue) {
        let (a, b) = (it.moving_source_time_at(it.start), it.moving_source_time_at(it.end() - Tick(1)));
        let e = it.effects.iter_mut().find(|e| e.effect == effect).unwrap();
        e.params.insert(param.into(), Param { value: from.clone(), keyframes: vec![Keyframe::new(a, from), Keyframe::new(b, to)] });
    }

    /// A V1 clip and an A1 clip under the playhead, their effects animated from
    /// their In to their Out, each held on its In (`Some(hold_filters)`) or not held.
    fn app(hold: Option<bool>) -> FilmcraftApp {
        let mut s = filmcraft_engine::Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let v = s.active_sequence().unwrap().video_tracks[0].items[0].clone();
        // away from the In, where the held and the moving times meet
        let ph = v.start + Tick(v.duration.0 / 2);
        s.execute("playhead.set", json!({"time": ph.0})).unwrap();
        let a = s.active_sequence().unwrap().audio_tracks.iter().find_map(|t| t.item_at(ph)).unwrap().id;
        s.execute("effects.apply", json!({"clips": [v.id.0], "effect": "lumetri"})).unwrap();
        s.execute("effects.apply", json!({"clips": [a.0], "effect": "parametric_eq"})).unwrap();
        s.execute("timeline.select", json!({"clips": [v.id.0]})).unwrap();
        s.execute("masks.add", json!({"clip": v.id.0, "effect": "opacity", "shape": "ellipse"})).unwrap();
        s.execute("effects.select", json!({"clip": v.id.0, "effect": "motion"})).unwrap();
        if let Some(hold_filters) = hold {
            s.execute("clip.frameHoldOptions", json!({"clips": [v.id.0], "holdOn": "in", "holdFilters": hold_filters})).unwrap();
        }
        let seq = s.state.active_sequence.unwrap();
        let q = std::sync::Arc::make_mut(&mut s.project).sequence_mut(seq).unwrap();
        let it = q.find_item_mut(v.id).unwrap().1;
        animate(it, "motion", "position", ParamValue::Vec2(Vec2::new(400.0, 300.0)), ParamValue::Vec2(Vec2::new(800.0, 500.0)));
        animate(it, "lumetri", "saturation", ParamValue::Float(50.0), ParamValue::Float(150.0));
        let it = q.find_item_mut(a).unwrap().1;
        animate(it, "parametric_eq", "b1_gain", ParamValue::Float(-12.0), ParamValue::Float(12.0));
        // Frame Hold Options takes video clips; an imported edit list can hold an audio clip
        if let Some(hold_filters) = hold {
            (it.frame_hold, it.hold_filters) = (Some(it.source_in), hold_filters);
        }
        let index = it.effects.iter().position(|e| e.effect == "parametric_eq").unwrap();
        let mut app = FilmcraftApp::new(s);
        app.ui.audio_fx_editors.push(FxTarget::Clip { clip: a.0, index });
        app
    }

    /// What `panel` draws, after a few frames (the editor window sizes itself on the first one).
    fn observe(mut app: FilmcraftApp, panel: Panel) -> String {
        let q = app.session.active_sequence().unwrap();
        let frame = (q.settings.width, q.settings.height);
        let pic = Rect::from_min_size(pos2(0.0, 0.0), vec2(640.0, 360.0));
        // a selected mask and a selected effect exclude each other
        if let Panel::Masks = panel {
            let clip = app.session.state.selection[0];
            app.session.execute("masks.select", json!({"clip": clip.0, "effect": "opacity", "mask": 0})).unwrap();
        }
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &app.tokens);
        let mut shapes = String::new();
        for i in 0..3 {
            app.auto.begin_frame();
            let mut out = ctx.run_ui(egui::RawInput { time: Some(f64::from(i) * 0.1), ..Default::default() }, |ui| match panel {
                Panel::Lumetri => crate::panels::lumetri::show(&mut app, ui, Rect::from_min_size(pos2(0.0, 0.0), vec2(360.0, 900.0))),
                Panel::Masks => crate::panels::masks::monitor_overlay(&mut app, ui, pic, frame),
                Panel::TransformHandles => crate::panels::transform_handles::monitor_overlay(&mut app, ui, pic, frame),
                Panel::AudioFxEditor => crate::panels::audio_fx_editor::show(&mut app, ui.ctx()),
            });
            out.textures_delta.clear();
            shapes = format!("{:?}", out.shapes);
        }
        shapes
    }

    #[test]
    fn panels_show_effect_values_where_the_render_takes_them_on_a_frame_hold() {
        for panel in [Panel::Lumetri, Panel::Masks, Panel::TransformHandles, Panel::AudioFxEditor] {
            let moving = observe(app(None), panel);
            let held = observe(app(Some(true)), panel);
            assert!(moving != held, "{panel:?}: with Hold Filters on it shows the held frame's values");
            assert!(observe(app(Some(false)), panel) == moving, "{panel:?}: with Hold Filters off it shows the values it shows without the hold");
        }
    }
}

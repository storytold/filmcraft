//! Graphics commands (`graphics.*`, `fonts.list`): graphic clips with text and shape layers.
//!
//! A graphic clip's layers are the `graphic_text` / `graphic_shape` effect instances on the clip
//! (see `filmcraft_project::graphic`). Commands address a clip by `clip` (default: the first
//! selected graphic clip, else the topmost graphic clip under the playhead) and a layer by
//! `layer` = index among the clip's graphic layers, 0 = back (default: the selected layer, else
//! the frontmost). Property values are keyframe-aware: on an animated property they set the value
//! at the playhead.

use filmcraft_geom::Vec2;
use filmcraft_project::graphic::{self, LayerContent, SHAPE_OPTS, eval_layer, layer_display_name, layer_indices, new_shape_layer, new_text_layer};
use filmcraft_project::{ClipId, EffectInstance, ItemId, ItemKind, Label, Param, ParamValue, Sequence, TrackKind};
use filmcraft_render::graphic_clip::{item_layer_specs, layer_local_bounds, layer_quad, text_layout};
use filmcraft_time::{Tick, TimeRange};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, f64_p, has_seq, str_p, time_p, u64_p};
use crate::{EngineError, Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    shortcut: Option<&'static str>,
    params: &'static str,
    enabled: Enabled,
    run: Run,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut, params, enabled, run, journal: true }
}
fn query(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

/// Whether a track item is a graphic clip.
pub fn is_graphic(s: &Session, seq: &Sequence, c: ClipId) -> bool {
    seq.find_item(c).and_then(|(_, it)| s.project.item(it.item)).is_some_and(|p| matches!(p.kind, ItemKind::Graphic { .. }))
}

/// The graphic clip a command acts on: `clip`, else the first selected graphic clip, else the
/// topmost graphic clip under the playhead.
pub fn target_clip(s: &Session, p: &Value) -> Option<ClipId> {
    let seq = s.active_sequence()?;
    if let Some(c) = u64_p(p, "clip").map(ClipId) {
        return is_graphic(s, seq, c).then_some(c);
    }
    if let Some(c) = s.state.selection.iter().copied().find(|c| is_graphic(s, seq, *c)) {
        return Some(c);
    }
    let t = s.playhead();
    seq.video_tracks.iter().rev().filter_map(|tr| tr.item_at(t)).map(|it| it.id).find(|c| is_graphic(s, seq, *c))
}

fn has_graphic(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    target_clip(s, &Value::Null).map(|_| ()).ok_or_else(|| "select a graphic clip".into())
}

/// Effect index of graphic layer `layer` (or the selected / frontmost layer) of `clip`.
pub(crate) fn layer_effect_index(s: &Session, clip: ClipId, p: &Value) -> Result<(usize, usize)> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad("graphics", "no such clip"))?;
    let idx = layer_indices(&it.effects);
    if idx.is_empty() {
        return Err(bad("graphics", "the graphic has no layers"));
    }
    let l = match p.get("layer") {
        Some(Value::Number(n)) => n.as_u64().unwrap_or(0) as usize,
        Some(Value::String(name)) => (0..idx.len())
            .find(|&i| layer_display_name(&it.effects[idx[i]], i).eq_ignore_ascii_case(name))
            .ok_or_else(|| bad("graphics", format!("no layer named `{name}`")))?,
        _ => s.state.graphic_layers.first().copied().filter(|l| *l < idx.len()).unwrap_or(idx.len() - 1),
    };
    let e = *idx.get(l).ok_or_else(|| bad("graphics", format!("no layer {l}")))?;
    Ok((l, e))
}

pub(crate) fn vec2_p(p: &Value, k: &str) -> Option<Vec2> {
    let a = p.get(k)?.as_array()?;
    Some(Vec2::new(a.first()?.as_f64()?, a.get(1)?.as_f64()?))
}

/// Find or create the graphic source item for the active sequence's frame size (source graphics
/// have their own items and are never reused here).
pub(crate) fn graphic_source(p: &mut filmcraft_project::Project, w: u32, h: u32, rate: filmcraft_time::FrameRate) -> ItemId {
    if let Some((id, _)) = p.items.iter().find(|(id, i)| {
        !p.source_graphics.contains_key(id) && matches!(i.kind, ItemKind::Graphic { width, height, rate: r } if width == w && height == h && r == rate)
    }) {
        return *id;
    }
    p.add_item("Graphic", Label::Rose, ItemKind::Graphic { width: w, height: h, rate }, None)
}

/// Place a new graphic clip holding `layer` at the playhead: on the first video track above the
/// topmost clip at the playhead that is free for the duration (a track is added if needed).
fn new_graphic_clip(s: &mut Session, layer: filmcraft_project::EffectInstance, name: &str, p: &Value) -> Result<ClipId> {
    let seconds = f64_p(p, "seconds").unwrap_or(5.0).max(0.01);
    // Here `seconds` is duration, not the generic time parser's placement alias.
    let mut placement = p.clone();
    if let Some(params) = placement.as_object_mut() {
        params.remove("seconds");
    }
    place_video_clip(s, name, &placement, "New Graphic", vec![layer], move |pr, (w, h, rate)| {
        let src = graphic_source(pr, w, h, rate);
        (src, rate.snap_nearest(Tick::from_seconds_f64(seconds)).max(rate.frame_duration()))
    })
}

/// Place a video clip of the item `source` returns (with its duration) at the playhead (or
/// `time`), above the clips there, as one undo step; selects it.
pub(crate) fn place_video_clip(
    s: &mut Session,
    name: &str,
    p: &Value,
    label: &str,
    extra: Vec<filmcraft_project::EffectInstance>,
    source: impl FnOnce(&mut filmcraft_project::Project, (u32, u32, filmcraft_time::FrameRate)) -> (ItemId, Tick),
) -> Result<ClipId> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let t = time_p(s, p, "").unwrap_or_else(|| s.playhead());
    let want_track = u64_p(p, "track").map(|v| v as usize);
    let name = name.to_string();
    s.edit(label, |pr, st| {
        let (w, h, rate) = {
            let q = pr.sequence(seq_id).ok_or(EngineError::NoSequence)?;
            (q.settings.width, q.settings.height, q.settings.frame_rate)
        };
        let (src, dur) = source(pr, (w, h, rate));
        // an imported picture (`graphics.newFromFile`) keeps its own size: centre its anchor in it
        let src_size = pr.source_size(src).unwrap_or((w, h));
        let t = rate.snap(t);
        let dur = dur.max(rate.frame_duration());
        let mut ti = pr.make_track_item(src, TrackKind::Video, t, TimeRange::new(Tick::ZERO, dur), rate).ok_or_else(|| bad("graphics.newText", "bad item"))?;
        ti.name = name;
        ti.effects.extend(extra);
        for e in &mut ti.effects {
            filmcraft_project::resolve_auto_points(e, (w, h), src_size);
        }
        let id = ti.id;
        let track_id = filmcraft_project::TrackId(pr.alloc_id());
        let q = pr.sequence_mut(seq_id).ok_or(EngineError::NoSequence)?;
        let range = TimeRange::new(t, dur);
        let free = |tr: &filmcraft_project::Track| !tr.items.iter().any(|i| i.range().overlaps(&range));
        let idx = match want_track {
            Some(i) if i < q.video_tracks.len() && free(&q.video_tracks[i]) => i,
            Some(i) if i < q.video_tracks.len() => return Err(bad("graphics.newText", format!("V{} is not free here", i + 1))),
            _ => {
                let top = q.video_tracks.iter().rposition(|tr| tr.item_at(t).is_some()).map_or(0, |k| k + 1);
                match (top..q.video_tracks.len()).find(|&i| free(&q.video_tracks[i]) && !q.video_tracks[i].locked) {
                    Some(i) => i,
                    None => {
                        let n = q.video_tracks.len() + 1;
                        q.video_tracks.push(filmcraft_project::Track::new(track_id, TrackKind::Video, format!("Video {n}")));
                        q.video_tracks.len() - 1
                    }
                }
            }
        };
        q.video_tracks[idx].items.push(ti);
        q.video_tracks[idx].sort();
        q.check().map_err(EngineError::Other)?;
        st.selection = vec![id];
        st.graphic_layers = vec![0];
        Ok(id)
    })
}

/// Add a layer to an existing graphic clip; returns the new layer index.
fn add_layer(s: &mut Session, clip: ClipId, mut layer: filmcraft_project::EffectInstance) -> Result<usize> {
    let frame = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
    filmcraft_project::resolve_auto_points(&mut layer, frame, frame);
    s.edit_sequence("Add Graphic Layer", |q, _, st| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        // new layers go in front: after the last graphic layer
        let pos = layer_indices(&it.effects).last().map_or(it.effects.len(), |l| l + 1);
        it.effects.insert(pos, layer);
        let n = layer_indices(&it.effects).len() - 1;
        st.selection = vec![clip];
        st.graphic_layers = vec![n];
        Ok(n)
    })
}

/// Map friendly property names to parameter ids.
fn param_id(k: &str) -> &str {
    match k {
        "fontSize" => "size",
        "fontStyle" | "style" => "font_style",
        "fillColor" | "color" => "fill_color",
        "strokeColor" => "stroke_color",
        "strokeWidth" => "stroke_width",
        "backgroundColor" => "background_color",
        "shadowColor" => "shadow_color",
        "baselineShift" => "baseline_shift",
        "fauxBold" => "faux_bold",
        "fauxItalic" => "faux_italic",
        "boxWidth" => "box_width",
        "boxHeight" => "box_height",
        "anchorPoint" => "anchor",
        "cornerRadius" => "corner_radius",
        other => other,
    }
}

/// JSON value for a property, accepting names for choices ("center", "small caps", "ellipse"…).
pub(crate) fn to_param(template: &ParamValue, id: &str, v: &Value) -> Option<ParamValue> {
    if let (ParamValue::Choice(_), Value::String(name)) = (template, v) {
        let opts: &[&str] = match id {
            "align" => graphic::ALIGN_OPTS,
            "caps" => graphic::CAPS_OPTS,
            "stroke_type" | "stroke2_type" => graphic::STROKE_OPTS,
            "shape" => SHAPE_OPTS,
            _ => &[],
        };
        let n = name.to_ascii_lowercase().replace('_', " ");
        let n = if n == "centre" { "center".to_string() } else { n };
        return opts.iter().position(|o| o.to_ascii_lowercase() == n).map(|i| ParamValue::Choice(i as u32));
    }
    crate::commands::json_to_param(template, v)
}

/// Parameter `id` of a layer, created from the layer definition when the layer was saved before
/// the parameter existed.
fn param_entry<'a>(e: &'a mut EffectInstance, id: &str) -> Option<&'a mut Param> {
    if !e.params.contains_key(id) {
        let d = filmcraft_project::find_effect(&e.effect)?.param(id)?.default.clone();
        e.params.insert(id.to_string(), Param::new(d));
    }
    e.params.get_mut(id)
}

/// Set properties `props` on layer `e` (keyframe-aware at media time `mt`). Changing the text
/// keeps per-character styles on their characters.
fn apply_props(e: &mut EffectInstance, props: &serde_json::Map<String, Value>, mt: Tick) -> Result<()> {
    for (k, v) in props {
        if k == "enabled" {
            e.enabled = v.as_bool().unwrap_or(true);
            continue;
        }
        let id = param_id(k);
        let prm = param_entry(e, id).ok_or_else(|| bad("graphics.set", format!("no property `{k}`")))?;
        let pv = to_param(&prm.value, id, v).ok_or_else(|| bad("graphics.set", format!("`{k}`: value has the wrong type")))?;
        let old = prm.value_at(mt);
        prm.set_at(mt, pv.clone());
        if let (ParamValue::Text(a), ParamValue::Text(b), "text") = (&old, &pv, id)
            && let Some(x) = e.layer.as_mut()
        {
            x.runs = filmcraft_project::graphic_design::adjust_runs(a, b, &x.runs);
        }
    }
    Ok(())
}

/// Set properties `props` on a layer (keyframe-aware at time `tl`). Changing the text keeps
/// per-character styles on their characters.
pub(crate) fn set_props(s: &mut Session, clip: ClipId, eidx: usize, props: &serde_json::Map<String, Value>, tl: Tick, label: &str) -> Result<()> {
    let props = props.clone();
    s.edit_sequence(label, |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let mt = it.source_time_at(tl.clamp(it.start, it.end() - Tick(1)));
        let e = it.effects.get_mut(eidx).ok_or_else(|| bad("graphics.set", "no such layer"))?;
        apply_props(e, &props, mt)?;
        Ok(())
    })
}

/// Turn a text layer into point text (no box; the lines the box wrapped become real lines) or
/// paragraph text (a box fitted to the text). The text stays where it is: the layer's origin
/// moves between the first baseline and the box's top-left corner, and the anchor point is
/// renumbered to stay on the same spot.
fn set_text_type(s: &mut Session, p: &Value) -> Result<Value> {
    const ID: &str = "graphics.setTextType";
    let clip = target_clip(s, p).ok_or_else(|| bad(ID, "no graphic clip"))?;
    let (layer, ei) = layer_effect_index(s, clip, p)?;
    let to_paragraph = match str_p(p, "type").map(str::to_ascii_lowercase).as_deref() {
        Some("paragraph" | "paragraph text") => true,
        Some("point" | "point text") => false,
        _ => return Err(bad(ID, "need `type`: point or paragraph")),
    };
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad(ID, "no such clip"))?;
    let size = match s.project.item(it.item).map(|p| &p.kind) {
        Some(ItemKind::Graphic { width, height, .. }) => (*width, *height),
        _ => (seq.settings.width, seq.settings.height),
    };
    let ph = s.playhead();
    let mt = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let spec = it.effects.get(ei).and_then(|e| eval_layer(e, mt, size)).ok_or_else(|| bad(ID, "no such layer"))?;
    let LayerContent::Text(tp) = &spec.content else { return Err(bad(ID, "not a text layer")) };
    if tp.vertical {
        return Err(bad(ID, "vertical text is always point text"));
    }
    let name = |paragraph: bool| if paragraph { "paragraph" } else { "point" };
    if (tp.box_width > 0.0) == to_paragraph {
        return Ok(json!({"clip": clip.0, "layer": layer, "type": name(to_paragraph), "changed": false}));
    }
    // where point text's origin (its alignment point on the first baseline) sits in a box `w` wide
    let origin_x = |w: f32| match tp.align {
        1 => w / 2.0,
        2 => w,
        _ => 0.0,
    };
    let mut props = serde_json::Map::new();
    // what to add to the anchor so that it stays on the same spot of the picture
    let shift = if to_paragraph {
        let widest = text_layout(tp).lines.iter().map(|l| l.width).fold(0.0, f32::max);
        // a little slack, so that no line wraps
        let w = (widest.ceil() + 1.0).clamp(1.0, 100_000.0);
        let boxed = text_layout(&graphic::TextProps { box_width: w, box_height: 0.0, ..tp.clone() });
        let h = (boxed.bounds[3].ceil() + 1.0).clamp(1.0, 100_000.0);
        props.insert("box_width".into(), json!(w));
        props.insert("box_height".into(), json!(h));
        (origin_x(w), boxed.lines.first().map_or(0.0, |l| l.baseline))
    } else {
        // every line, also those the box hides
        let all = text_layout(&graphic::TextProps { box_height: 0.0, ..tp.clone() });
        let mut text = String::with_capacity(tp.text.len());
        for (i, l) in all.lines.iter().enumerate() {
            let line = tp.text.get(l.range.clone()).unwrap_or_default();
            // a wrapped line ends where the next begins: its trailing space becomes the line break
            let wrapped = all.lines.get(i + 1).is_some_and(|n| n.range.start == l.range.end);
            text.push_str(if wrapped { line.trim_end() } else { line });
            if i + 1 < all.lines.len() {
                text.push('\n');
            }
        }
        props.insert("text".into(), json!(text));
        props.insert("box_width".into(), json!(0.0));
        props.insert("box_height".into(), json!(0.0));
        (-origin_x(tp.box_width), -all.lines.first().map_or(0.0, |l| l.baseline))
    };
    s.edit_sequence("Text Layer Type", |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let e = it.effects.get_mut(ei).ok_or_else(|| bad(ID, "no such layer"))?;
        apply_props(e, &props, mt)?;
        if let Some(a) = param_entry(e, "anchor") {
            a.map_values(|v| match v.as_vec2() {
                Some(a) => ParamValue::Vec2(Vec2::new(a.x + shift.0 as f64, a.y + shift.1 as f64)),
                None => v,
            });
        }
        Ok(())
    })?;
    Ok(json!({"clip": clip.0, "layer": layer, "type": name(to_paragraph), "changed": true}))
}

/// Layers of a graphic clip with their evaluated bounds (sequence/canvas pixels).
fn list_layers(s: &Session, clip: ClipId) -> Result<Value> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad("graphics.list", "no such clip"))?;
    let size = match s.project.item(it.item).map(|p| &p.kind) {
        Some(ItemKind::Graphic { width, height, .. }) => (*width, *height),
        _ => (seq.settings.width, seq.settings.height),
    };
    let ph = s.playhead();
    let mt = it.source_time_at(ph.clamp(it.start, it.end() - Tick(1)));
    let shown = item_layer_specs(it, mt, size);
    let layers: Vec<Value> = layer_indices(&it.effects)
        .iter()
        .enumerate()
        .filter_map(|(i, &ei)| {
            let e = &it.effects[ei];
            let sp = shown.iter().find(|(x, _)| *x == ei).map(|(_, s)| s.clone()).or_else(|| eval_layer(e, mt, size))?;
            let q = layer_quad(&sp);
            let extra = e.layer.as_deref();
            let (kind, text) = match &sp.content {
                LayerContent::Text(t) => ("text", Some(t.text.clone())),
                LayerContent::Shape(sh) => (SHAPE_OPTS.get(sh.shape as usize).copied().unwrap_or("Shape"), None),
            };
            // point text has no box; paragraph text wraps in one and hides what does not fit
            let (text_type, text_box, overflow) = match &sp.content {
                LayerContent::Text(t) if t.box_width > 0.0 && !t.vertical => {
                    (Some("paragraph"), Some([t.box_width, t.box_height]), Some(text_layout(t).overflow))
                }
                LayerContent::Text(_) => (Some("point"), None, None),
                LayerContent::Shape(_) => (None, None, None),
            };
            Some(json!({
                "layer": i,
                "effectIndex": ei,
                "name": layer_display_name(e, i),
                "kind": kind,
                "text": text,
                "enabled": e.enabled,
                "textType": text_type,
                "box": text_box,
                "overflow": overflow,
                "position": [sp.transform.position.x, sp.transform.position.y],
                "anchor": [sp.transform.anchor.x, sp.transform.anchor.y],
                "scale": sp.transform.scale.y * 100.0,
                "scaleWidth": sp.transform.scale.x * 100.0,
                "uid": extra.map_or(0, |x| x.uid),
                "pin": extra.and_then(|x| x.pin.as_ref()).map(|p| serde_json::to_value(p).unwrap_or_default()),
                "styles": extra.map(|x| serde_json::to_value(&x.runs).unwrap_or_default()).unwrap_or(json!([])),
                "localBounds": layer_local_bounds(&sp),
                "quad": q.iter().map(|p| [p.x, p.y]).collect::<Vec<_>>(),
            }))
        })
        .collect();
    let meta = it.graphic.as_deref().map(|m| serde_json::to_value(m).unwrap_or_default());
    let source = s.project.source_graphics.contains_key(&it.item).then_some(it.item.0);
    Ok(
        json!({"clip": clip.0, "name": it.name, "start": it.start.0, "duration": it.duration.0, "canvas": [size.0, size.1], "layers": layers, "graphic": meta, "sourceGraphic": source}),
    )
}

fn quad_bounds(q: &[Vec2; 4]) -> [f64; 4] {
    let mut b = [f64::MAX, f64::MAX, f64::MIN, f64::MIN];
    for p in q {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    b
}

/// Layer bounds of `layers` (layer indices) of `clip` at the playhead: (effect index, position, bounds).
fn layer_boxes(s: &Session, clip: ClipId, layers: &[usize]) -> Result<(Vec<(usize, Vec2, [f64; 4])>, (u32, u32))> {
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad("graphics.align", "no such clip"))?;
    let size = match s.project.item(it.item).map(|p| &p.kind) {
        Some(ItemKind::Graphic { width, height, .. }) => (*width, *height),
        _ => (seq.settings.width, seq.settings.height),
    };
    let mt = it.source_time_at(s.playhead().clamp(it.start, it.end() - Tick(1)));
    let idx = layer_indices(&it.effects);
    let shown = item_layer_specs(it, mt, size);
    let mut out = Vec::new();
    for &l in layers {
        let ei = *idx.get(l).ok_or_else(|| bad("graphics.align", format!("no layer {l}")))?;
        let sp = shown.iter().find(|(x, _)| *x == ei).map(|(_, s)| s.clone()).ok_or_else(|| bad("graphics.align", "bad layer"))?;
        // the position stored in the layer (what moving it changes)
        let own = eval_layer(&it.effects[ei], mt, size).map_or(sp.transform.position, |o| o.transform.position);
        // bounds where it is shown (after pins / roll); moving `own` by d moves those by d
        out.push((ei, own, quad_bounds(&layer_quad(&sp))));
    }
    Ok((out, size))
}

fn layers_p(s: &Session, clip: ClipId, p: &Value) -> Result<Vec<usize>> {
    if let Some(a) = p.get("layers").and_then(Value::as_array) {
        return Ok(a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect());
    }
    if !s.state.graphic_layers.is_empty() {
        return Ok(s.state.graphic_layers.clone());
    }
    let (l, _) = layer_effect_index(s, clip, p)?;
    Ok(vec![l])
}

pub(crate) fn move_layers(s: &mut Session, clip: ClipId, moves: Vec<(usize, Vec2)>, label: &str) -> Result<()> {
    let tl = s.playhead();
    s.edit_sequence(label, |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let mt = it.source_time_at(tl.clamp(it.start, it.end() - Tick(1)));
        for (ei, pos) in &moves {
            let Some(e) = it.effects.get_mut(*ei) else { continue };
            let Some(prm) = e.params.get_mut("position") else { continue };
            let old = prm.value_at(mt).as_vec2().unwrap_or(*pos);
            prm.set_at(mt, ParamValue::Vec2(*pos));
            // a pinned layer keeps following its target from its new place
            if let Some(pin) = e.layer.as_mut().and_then(|x| x.pin.as_mut()) {
                let (dx, dy) = (pos.x - old.x, pos.y - old.y);
                pin.offsets[0] += dx;
                pin.offsets[2] += dx;
                pin.offsets[1] += dy;
                pin.offsets[3] += dy;
            }
        }
        Ok(())
    })
}

#[derive(Clone, Copy, PartialEq)]
enum AlignTo {
    /// Each layer to the video frame.
    Frame,
    /// The layers' union to the video frame (they keep their relative positions).
    FrameGroup,
    /// Each layer to the union of the selection.
    Selection,
}

fn align_delta(how: &str, r: [f64; 4], b: [f64; 4]) -> Result<(f64, f64)> {
    Ok(match how {
        "left" => (r[0] - b[0], 0.0),
        "right" => (r[2] - b[2], 0.0),
        "hcenter" | "center" => ((r[0] + r[2]) / 2.0 - (b[0] + b[2]) / 2.0, 0.0),
        "top" => (0.0, r[1] - b[1]),
        "bottom" => (0.0, r[3] - b[3]),
        "vcenter" | "middle" => (0.0, (r[1] + r[3]) / 2.0 - (b[1] + b[3]) / 2.0),
        o => return Err(bad("graphics.align", format!("unknown alignment `{o}`"))),
    })
}

fn union_box(boxes: &[(usize, Vec2, [f64; 4])]) -> [f64; 4] {
    boxes.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |a, b| [a[0].min(b.2[0]), a[1].min(b.2[1]), a[2].max(b.2[2]), a[3].max(b.2[3])])
}

/// Graphics ▸ Align to Video Frame / Align to Video Frame as Group / Align to Selection.
fn align_layers(s: &mut Session, p: &Value, how: &str, mode: AlignTo) -> Result<Value> {
    let clip = target_clip(s, p).ok_or_else(|| bad("graphics.align", "no graphic clip"))?;
    let layers = layers_p(s, clip, p)?;
    let (boxes, size) = layer_boxes(s, clip, &layers)?;
    let frame = [0.0, 0.0, size.0 as f64, size.1 as f64];
    let mode = if mode == AlignTo::Selection && boxes.len() < 2 { AlignTo::Frame } else { mode };
    let union = union_box(&boxes);
    let group = align_delta(how, frame, union)?;
    let mut moves = Vec::new();
    for (ei, pos, b) in &boxes {
        let (dx, dy) = match mode {
            AlignTo::Frame => align_delta(how, frame, *b)?,
            AlignTo::FrameGroup => group,
            AlignTo::Selection => align_delta(how, union, *b)?,
        };
        moves.push((*ei, Vec2::new(pos.x + dx, pos.y + dy)));
    }
    move_layers(s, clip, moves, "Align Layers")?;
    Ok(json!({"clip": clip.0, "layers": layers}))
}

/// Graphics ▸ Distribute: equal centre spacing, or (`space`) equal gaps between the layers.
fn distribute_layers(s: &mut Session, p: &Value, vertical: bool, space: bool) -> Result<Value> {
    let clip = target_clip(s, p).ok_or_else(|| bad("graphics.distribute", "no graphic clip"))?;
    let layers = layers_p(s, clip, p)?;
    let (mut boxes, _) = layer_boxes(s, clip, &layers)?;
    if boxes.len() < 3 {
        return Err(bad("graphics.distribute", "select three or more layers"));
    }
    let (lo, hi) = if vertical { (1, 3) } else { (0, 2) };
    let c = |b: &[f64; 4]| (b[lo] + b[hi]) / 2.0;
    boxes.sort_by(|a, b| c(&a.2).total_cmp(&c(&b.2)));
    let n = boxes.len() - 1;
    let deltas: Vec<f64> = if space {
        let span = boxes[n].2[hi] - boxes[0].2[lo];
        let total: f64 = boxes.iter().map(|b| b.2[hi] - b.2[lo]).sum();
        let gap = (span - total) / n as f64;
        let mut at = boxes[0].2[lo];
        boxes
            .iter()
            .map(|b| {
                let d = at - b.2[lo];
                at += b.2[hi] - b.2[lo] + gap;
                d
            })
            .collect()
    } else {
        let (first, last) = (c(&boxes[0].2), c(&boxes[n].2));
        boxes.iter().enumerate().map(|(i, b)| first + (last - first) * i as f64 / n as f64 - c(&b.2)).collect()
    };
    let moves =
        boxes.iter().zip(deltas).map(|((ei, pos, _), d)| (*ei, if vertical { Vec2::new(pos.x, pos.y + d) } else { Vec2::new(pos.x + d, pos.y) })).collect();
    move_layers(s, clip, moves, "Distribute Layers")?;
    Ok(json!({"clip": clip.0, "layers": layers}))
}

/// Move a graphic layer in the paint order (`to`: front / back / forward / backward / index).
fn arrange_layer(s: &mut Session, p: &Value, to: Value) -> Result<Value> {
    let clip = target_clip(s, p).ok_or_else(|| bad("graphics.arrangeLayer", "no graphic clip"))?;
    let (l, _) = layer_effect_index(s, clip, p)?;
    let dest = s.edit_sequence("Arrange Graphic Layer", |q, _, st| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let idx = layer_indices(&it.effects);
        let last = idx.len() - 1;
        let dest = match &to {
            Value::Number(v) => (v.as_u64().unwrap_or(0) as usize).min(last),
            Value::String(w) => match w.as_str() {
                "front" => last,
                "back" => 0,
                "forward" => (l + 1).min(last),
                "backward" => l.saturating_sub(1),
                o => return Err(bad("graphics.arrangeLayer", format!("unknown `to` `{o}`"))),
            },
            _ => last,
        };
        let e = it.effects.remove(idx[l]);
        let mut idx2 = layer_indices(&it.effects);
        let insert_at = if dest >= idx2.len() { idx2.pop().map_or(idx[0], |x| x + 1) } else { idx2[dest] };
        it.effects.insert(insert_at, e);
        st.selection = vec![clip];
        st.graphic_layers = vec![dest];
        Ok(dest)
    })?;
    Ok(json!({"clip": clip.0, "layer": dest}))
}

/// Graphic clips of the active sequence in timeline order (start, then track bottom → top).
fn graphic_clips(s: &Session) -> Vec<(Tick, usize, ClipId)> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let mut v: Vec<(Tick, usize, ClipId)> = q
        .video_tracks
        .iter()
        .enumerate()
        .flat_map(|(ti, tr)| tr.items.iter().map(move |it| (it.start, ti, it.id)))
        .filter(|(_, _, c)| is_graphic(s, q, *c))
        .collect();
    v.sort();
    v
}

/// Graphics ▸ Select ▸ Select Next / Previous Graphic.
fn select_graphic(s: &mut Session, forward: bool) -> Result<Value> {
    let all = graphic_clips(s);
    let cur = s.state.selection.iter().find_map(|c| all.iter().position(|g| g.2 == *c));
    let ph = s.playhead();
    let pick = match (cur, forward) {
        (Some(i), true) => all.get(i + 1),
        (Some(i), false) => i.checked_sub(1).and_then(|j| all.get(j)),
        (None, true) => all.iter().find(|g| g.0 >= ph).or(all.first()),
        (None, false) => all.iter().rev().find(|g| g.0 < ph).or(all.last()),
    };
    let (start, _, clip) = *pick.ok_or_else(|| bad("graphics.select", if forward { "no next graphic" } else { "no previous graphic" }))?;
    let dur = s.active_sequence().and_then(|q| q.find_item(clip)).map_or(Tick::ZERO, |(_, it)| it.duration);
    s.state.selection = vec![clip];
    s.state.graphic_layers.clear();
    if !(ph >= start && ph < start + dur) {
        s.set_playhead(start);
    }
    Ok(json!({"clip": clip.0}))
}

/// Graphics ▸ Select ▸ Select Next / Previous Layer (cycles through the clip's layers).
fn select_layer_step(s: &mut Session, forward: bool) -> Result<Value> {
    let clip = target_clip(s, &Value::Null).ok_or_else(|| bad("graphics.selectLayer", "no graphic clip"))?;
    let n = s.active_sequence().and_then(|q| q.find_item(clip)).map_or(0, |(_, it)| layer_indices(&it.effects).len());
    if n == 0 {
        return Err(bad("graphics.selectLayer", "the graphic has no layers"));
    }
    let l = match (s.state.graphic_layers.first().copied().filter(|l| *l < n), forward) {
        (Some(l), true) => (l + 1) % n,
        (Some(l), false) => (l + n - 1) % n,
        (None, true) => 0,
        (None, false) => n - 1,
    };
    s.state.selection = vec![clip];
    s.state.graphic_layers = vec![l];
    Ok(json!({"clip": clip.0, "layer": l}))
}

fn has_two_layers(s: &Session) -> std::result::Result<(), String> {
    has_graphic(s)?;
    if s.state.graphic_layers.len() < 2 { Err("select two or more graphic layers".into()) } else { Ok(()) }
}

fn has_three_layers(s: &Session) -> std::result::Result<(), String> {
    has_graphic(s)?;
    if s.state.graphic_layers.len() < 3 { Err("select three or more graphic layers".into()) } else { Ok(()) }
}

fn has_graphics_in_seq(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    if graphic_clips(s).is_empty() { Err("the sequence has no graphics".into()) } else { Ok(()) }
}

const ALIGN_PARAMS: &str = r#"{"clip":id?,"layers":[n]? (default: the selected layers)}"#;

macro_rules! align_cmd {
    ($id:literal, $label:literal, $menu:expr, $how:literal, $mode:expr, $en:expr) => {
        spec($id, $label, &["Graphics and Titles", $menu], None, ALIGN_PARAMS, $en, |s, p| align_layers(s, p, $how, $mode))
    };
}

macro_rules! arrange_cmd {
    ($id:literal, $label:literal, $sc:literal, $to:literal) => {
        spec($id, $label, &["Graphics and Titles", "Arrange"], Some($sc), r#"{"clip":id?,"layer":n?}"#, has_graphic, |s, p| arrange_layer(s, p, json!($to)))
    };
}

fn new_shape_cmd(s: &mut Session, p: &Value, shape: &str) -> Result<Value> {
    let mut q = p.clone();
    q["shape"] = json!(shape);
    (find_spec("graphics.newShape")?.run)(s, &q)
}

fn find_spec(id: &str) -> Result<CommandSpec> {
    commands().into_iter().find(|c| c.id == id).ok_or_else(|| EngineError::UnknownCommand(id.to_string()))
}

/// Graphics menu commands (New Layer shapes, Align/Distribute/Arrange/Select submenus, resets).
fn menu_commands() -> Vec<CommandSpec> {
    const F: &str = "Align to Video Frame";
    const G: &str = "Align to Video Frame as Group";
    const S: &str = "Align to Selection";
    const D: &[&str] = &["Graphics and Titles", "Distribute"];
    const SEL: &[&str] = &["Graphics and Titles", "Select"];
    vec![
        spec(
            "graphics.newVerticalText",
            "Vertical Text",
            &["Graphics and Titles", "New Layer"],
            None,
            r#"{"text":str="New Text","position":[x,y]?,"clip":id?,"size":px=100,"seconds":f64=5,"time":ticks?}"#,
            has_seq,
            |s, p| {
                let mut p = if p.is_object() { p.clone() } else { json!({}) };
                p["vertical"] = json!(true);
                (find_spec("graphics.newText")?.run)(s, &p)
            },
        ),
        spec(
            "graphics.newRectangle",
            "Rectangle",
            &["Graphics and Titles", "New Layer"],
            Some("Cmd+Alt+R"),
            r#"{"position":[x,y]?,"size":[w,h]=[400,200],"clip":id?}"#,
            has_seq,
            |s, p| new_shape_cmd(s, p, "rectangle"),
        ),
        spec(
            "graphics.newEllipse",
            "Ellipse",
            &["Graphics and Titles", "New Layer"],
            Some("Cmd+Alt+E"),
            r#"{"position":[x,y]?,"size":[w,h]=[400,200],"clip":id?}"#,
            has_seq,
            |s, p| new_shape_cmd(s, p, "ellipse"),
        ),
        spec(
            "graphics.newPolygon",
            "Polygon",
            &["Graphics and Titles", "New Layer"],
            None,
            r#"{"position":[x,y]?,"size":[w,h]=[300,300],"sides":n=6,"clip":id?}"#,
            has_seq,
            |s, p| {
                let mut q = if p.is_object() { p.clone() } else { json!({}) };
                if q.get("size").is_none() {
                    q["size"] = json!([300, 300]);
                }
                let r = new_shape_cmd(s, &q, "polygon")?;
                if let Some(n) = f64_p(p, "sides") {
                    // fold the side count into the same undo step
                    let clip = ClipId(r["clip"].as_u64().unwrap_or(0));
                    let (_, ei) = layer_effect_index(s, clip, &json!({"layer": r["layer"]}))?;
                    let mut props = serde_json::Map::new();
                    props.insert("sides".into(), json!(n));
                    let ph = s.playhead();
                    let before = s.history.undo.len();
                    set_props(s, clip, ei, &props, ph, "Change Graphic Property")?;
                    if s.history.undo.len() > before {
                        s.history.undo.pop();
                    }
                }
                Ok(r)
            },
        ),
        spec(
            "graphics.newFromFile",
            "From file…",
            &["Graphics and Titles", "New Layer"],
            None,
            r#"{"path":str,"time":ticks?,"track":index?} (imports the image or video and places it above the clips at the playhead)"#,
            has_seq,
            |s, p| {
                let path = str_p(p, "path").ok_or_else(|| bad("graphics.newFromFile", "need `path`"))?.to_string();
                let r = s.execute("file.import", json!({"paths": [path]}))?;
                // (a file the project already has is placed from its existing item)
                let item = crate::commands::imported_item(&r).ok_or_else(|| {
                    bad(
                        "graphics.newFromFile",
                        r["errors"].as_array().and_then(|e| e.first()).and_then(Value::as_str).unwrap_or("nothing imported").to_string(),
                    )
                })?;
                let pi = s.project.item(item).ok_or_else(|| bad("graphics.newFromFile", "no such item"))?;
                if matches!(&pi.kind, ItemKind::Media(m) if m.info.video.is_none()) {
                    return Err(bad("graphics.newFromFile", "the file has no picture"));
                }
                let (name, dur) = (pi.name.clone(), pi.duration());
                let clip = place_video_clip(s, &name, p, "New Layer from File", Vec::new(), move |_, _| (item, dur))?;
                s.state.graphic_layers.clear();
                Ok(json!({"clip": clip.0, "item": item.0}))
            },
        ),
        align_cmd!("graphics.alignFrame.left", "Left", F, "left", AlignTo::Frame, has_graphic),
        align_cmd!("graphics.alignFrame.hcenter", "Center Horizontally", F, "hcenter", AlignTo::Frame, has_graphic),
        align_cmd!("graphics.alignFrame.right", "Right", F, "right", AlignTo::Frame, has_graphic),
        align_cmd!("graphics.alignFrame.top", "Top", F, "top", AlignTo::Frame, has_graphic),
        align_cmd!("graphics.alignFrame.vcenter", "Center Vertically", F, "vcenter", AlignTo::Frame, has_graphic),
        align_cmd!("graphics.alignFrame.bottom", "Bottom", F, "bottom", AlignTo::Frame, has_graphic),
        align_cmd!("graphics.alignGroup.left", "Left", G, "left", AlignTo::FrameGroup, has_two_layers),
        align_cmd!("graphics.alignGroup.hcenter", "Center Horizontally", G, "hcenter", AlignTo::FrameGroup, has_two_layers),
        align_cmd!("graphics.alignGroup.right", "Right", G, "right", AlignTo::FrameGroup, has_two_layers),
        align_cmd!("graphics.alignGroup.top", "Top", G, "top", AlignTo::FrameGroup, has_two_layers),
        align_cmd!("graphics.alignGroup.vcenter", "Center Vertically", G, "vcenter", AlignTo::FrameGroup, has_two_layers),
        align_cmd!("graphics.alignGroup.bottom", "Bottom", G, "bottom", AlignTo::FrameGroup, has_two_layers),
        align_cmd!("graphics.alignSelection.left", "Left", S, "left", AlignTo::Selection, has_two_layers),
        align_cmd!("graphics.alignSelection.hcenter", "Center Horizontally", S, "hcenter", AlignTo::Selection, has_two_layers),
        align_cmd!("graphics.alignSelection.right", "Right", S, "right", AlignTo::Selection, has_two_layers),
        align_cmd!("graphics.alignSelection.top", "Top", S, "top", AlignTo::Selection, has_two_layers),
        align_cmd!("graphics.alignSelection.vcenter", "Center Vertically", S, "vcenter", AlignTo::Selection, has_two_layers),
        align_cmd!("graphics.alignSelection.bottom", "Bottom", S, "bottom", AlignTo::Selection, has_two_layers),
        spec("graphics.distributeVertically", "Distribute Vertically", D, None, ALIGN_PARAMS, has_three_layers, |s, p| distribute_layers(s, p, true, false)),
        spec("graphics.distributeSpaceVertically", "Distribute Space Vertically", D, None, ALIGN_PARAMS, has_three_layers, |s, p| {
            distribute_layers(s, p, true, true)
        }),
        spec("graphics.distributeHorizontally", "Distribute Horizontally", D, None, ALIGN_PARAMS, has_three_layers, |s, p| {
            distribute_layers(s, p, false, false)
        }),
        spec("graphics.distributeSpaceHorizontally", "Distribute Space Horizontally", D, None, ALIGN_PARAMS, has_three_layers, |s, p| {
            distribute_layers(s, p, false, true)
        }),
        arrange_cmd!("graphics.bringToFront", "Bring to Front", "Cmd+Shift+]", "front"),
        arrange_cmd!("graphics.bringForward", "Bring Forward", "Cmd+]", "forward"),
        arrange_cmd!("graphics.sendBackward", "Send Backward", "Cmd+[", "backward"),
        arrange_cmd!("graphics.sendToBack", "Send to Back", "Cmd+Shift+[", "back"),
        spec("graphics.selectNextGraphic", "Select Next Graphic", SEL, None, "{}", has_graphics_in_seq, |s, _| select_graphic(s, true)),
        spec("graphics.selectPreviousGraphic", "Select Previous Graphic", SEL, None, "{}", has_graphics_in_seq, |s, _| select_graphic(s, false)),
        spec("graphics.selectNextLayer", "Select Next Layer", SEL, Some("Cmd+Alt+]"), "{}", has_graphic, |s, _| select_layer_step(s, true)),
        spec("graphics.selectPreviousLayer", "Select Previous Layer", SEL, Some("Cmd+Alt+["), "{}", has_graphic, |s, _| select_layer_step(s, false)),
        spec(
            "graphics.resetAllParameters",
            "Reset All Parameters",
            &["Graphics and Titles"],
            None,
            r#"{"clip":id?,"layers":[n]? (default: the selected layers, else all)}"#,
            has_graphic,
            |s, p| {
                let clip = target_clip(s, p).ok_or_else(|| bad("graphics.resetAllParameters", "no graphic clip"))?;
                let frame = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
                let want: Vec<usize> = match p.get("layers").and_then(Value::as_array) {
                    Some(a) => a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect(),
                    None => s.state.graphic_layers.clone(),
                };
                let n = s.edit_sequence("Reset All Parameters", |q, _, _| {
                    let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
                    let mut n = 0;
                    for (l, ei) in layer_indices(&it.effects).into_iter().enumerate() {
                        if want.is_empty() || want.contains(&l) {
                            graphic::reset_layer_params(&mut it.effects[ei]);
                            filmcraft_project::resolve_auto_points(&mut it.effects[ei], frame, frame);
                            n += 1;
                        }
                    }
                    Ok(n)
                })?;
                Ok(json!({"clip": clip.0, "reset": n}))
            },
        ),
        spec(
            "graphics.resetDuration",
            "Reset Duration",
            &["Graphics and Titles"],
            None,
            r#"{"clip":id?,"seconds":f64=5 (the default graphic duration; limited by the next clip on the track)}"#,
            has_graphic,
            |s, p| {
                let clip = target_clip(s, p).ok_or_else(|| bad("graphics.resetDuration", "no graphic clip"))?;
                let seconds = f64_p(p, "seconds").unwrap_or(5.0).max(0.01);
                let d = s.edit_sequence("Reset Duration", |q, _, _| {
                    let rate = q.settings.frame_rate;
                    let want = rate.snap_nearest(Tick::from_seconds_f64(seconds)).max(rate.frame_duration());
                    let tr = q.video_tracks.iter_mut().find(|tr| tr.items.iter().any(|i| i.id == clip)).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
                    let i = tr.items.iter().position(|i| i.id == clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
                    let start = tr.items[i].start;
                    let limit = tr.items.get(i + 1).map_or(want, |n| n.start - start);
                    let d = want.min(limit).max(rate.frame_duration());
                    tr.items[i].duration = d;
                    Ok(d)
                })?;
                Ok(json!({"clip": clip.0, "duration": d.0}))
            },
        ),
    ]
}

pub fn commands() -> Vec<CommandSpec> {
    let mut v = vec![
        spec(
            "graphics.newText",
            "Text",
            &["Graphics and Titles", "New Layer"],
            Some("Cmd+T"),
            r#"{"text":str="New Text","position":[x,y]? (point text: its alignment point on the first baseline; with `box`: the box's top-left corner),"box":[w,h]? (paragraph text wrapped in a box this size),"clip":id?,"newClip":bool?,"vertical":bool=false,"size":px=100,"font":str?,"fontStyle":str?,"seconds":f64=5,"track":index?,"time":ticks?}"#,
            has_seq,
            |s, p| {
                let text = str_p(p, "text").unwrap_or("New Text").to_string();
                // Settings ▸ Graphics ▸ Text: smart quotes, ligatures, default font
                let gp = s.prefs.graphics.clone();
                let text = if gp.smart_quotes { crate::settings::smart_quotes(&text) } else { text };
                let (w, h) = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
                let pos = vec2_p(p, "position").unwrap_or(Vec2::new(w as f64 / 2.0, h as f64 / 2.0));
                let size = f64_p(p, "size").unwrap_or(100.0);
                let vertical = p.get("vertical").and_then(Value::as_bool) == Some(true);
                let mut layer = if vertical { graphic::new_vertical_text_layer(&text, pos, size) } else { new_text_layer(&text, pos, size) };
                if let Some(b) = vec2_p(p, "box").filter(|_| !vertical) {
                    for (id, v) in [("box_width", b.x), ("box_height", b.y)] {
                        layer.params.insert(id.into(), Param::new(ParamValue::Float(v.clamp(1.0, 100_000.0))));
                    }
                }
                layer.params.insert("ligatures".into(), Param::new(ParamValue::Bool(gp.ligatures)));
                if !gp.default_font.trim().is_empty() {
                    layer.params.insert("font".into(), Param::new(ParamValue::Text(gp.default_font.trim().into())));
                }
                for (k, id) in [("font", "font"), ("fontStyle", "font_style")] {
                    if let Some(v) = str_p(p, k) {
                        layer.params.insert(id.into(), Param::new(ParamValue::Text(v.into())));
                    }
                }
                let into = if p.get("newClip").and_then(Value::as_bool) == Some(true) { None } else { u64_p(p, "clip").map(ClipId) };
                let into = into.filter(|c| s.active_sequence().is_some_and(|q| is_graphic(s, q, *c)));
                let (clip, layer_i) = match into {
                    Some(c) => (c, add_layer(s, c, layer)?),
                    None => {
                        let name = text.lines().next().filter(|l| !l.trim().is_empty()).unwrap_or("Graphic").to_string();
                        (new_graphic_clip(s, layer, &name, p)?, 0)
                    }
                };
                Ok(json!({"clip": clip.0, "layer": layer_i}))
            },
        ),
        spec(
            "graphics.newShape",
            "Shape",
            &[],
            None,
            r#"{"shape":"rectangle|ellipse|polygon|path","position":[x,y]?,"size":[w,h]=[400,200],"points":[[x,y],…]?,"clip":id?,"seconds":f64=5}"#,
            has_seq,
            |s, p| {
                let shape = str_p(p, "shape").unwrap_or("rectangle").to_ascii_lowercase();
                let k = SHAPE_OPTS
                    .iter()
                    .position(|o| o.to_ascii_lowercase() == shape)
                    .ok_or_else(|| bad("graphics.newShape", format!("unknown shape `{shape}`")))? as u32;
                let (w, h) = s.active_sequence().map(|q| (q.settings.width, q.settings.height)).unwrap_or((1920, 1080));
                let pos = vec2_p(p, "position").unwrap_or(Vec2::new(w as f64 / 2.0, h as f64 / 2.0));
                let size = vec2_p(p, "size").unwrap_or(Vec2::new(400.0, 200.0));
                let points: Vec<[f32; 2]> = p
                    .get("points")
                    .and_then(Value::as_array)
                    .map(|a| a.iter().filter_map(|q| Some([q.get(0)?.as_f64()? as f32, q.get(1)?.as_f64()? as f32])).collect())
                    .unwrap_or_default();
                if k == 3 && points.len() < 3 {
                    return Err(bad("graphics.newShape", "a path needs at least 3 points"));
                }
                let layer = new_shape_layer(k, pos, size, points);
                let into = u64_p(p, "clip").map(ClipId).filter(|c| s.active_sequence().is_some_and(|q| is_graphic(s, q, *c)));
                let (clip, layer_i) = match into {
                    Some(c) => (c, add_layer(s, c, layer)?),
                    None => (new_graphic_clip(s, layer, "Shape", p)?, 0),
                };
                Ok(json!({"clip": clip.0, "layer": layer_i}))
            },
        ),
        spec(
            "graphics.setText",
            "Edit Text",
            &[],
            None,
            r#"{"clip":id?,"layer":n|name?,"text":str,"merge":bool? (coalesce with the previous Edit Text undo step)}"#,
            has_graphic,
            |s, p| {
                let clip = target_clip(s, p).ok_or_else(|| bad("graphics.setText", "no graphic clip"))?;
                let (_, ei) = layer_effect_index(s, clip, p)?;
                let text = str_p(p, "text").ok_or_else(|| bad("graphics.setText", "need `text`"))?.to_string();
                let merge = p.get("merge").and_then(Value::as_bool).unwrap_or(false) && s.history.undo.last().is_some_and(|h| h.0 == "Edit Text");
                let mut props = serde_json::Map::new();
                props.insert("text".into(), Value::String(text));
                let ph = s.playhead();
                set_props(s, clip, ei, &props, ph, "Edit Text")?;
                if merge && s.history.undo.len() >= 2 {
                    // keep the snapshot from before the typing session
                    s.history.undo.pop();
                }
                Ok(Value::Null)
            },
        ),
        spec(
            "graphics.set",
            "Set Graphic Properties",
            &[],
            None,
            r##"{"clip":id?,"layer":n|name?,"props":{"font":"Inter","font_style":"Bold","size":120,"align":"center","tracking":50,"leading":0,"fill_color":"#ffcc00","stroke":true,"stroke_width":6,"background":true,"shadow":true,"position":[x,y],"scale":100,"rotation":0,"opacity":100,…},"time":ticks?}"##,
            has_graphic,
            |s, p| {
                let clip = target_clip(s, p).ok_or_else(|| bad("graphics.set", "no graphic clip"))?;
                let (_, ei) = layer_effect_index(s, clip, p)?;
                let props = p.get("props").and_then(Value::as_object).ok_or_else(|| bad("graphics.set", "need `props`"))?.clone();
                let tl = time_p(s, p, "").unwrap_or_else(|| s.playhead());
                set_props(s, clip, ei, &props, tl, "Change Graphic Property")?;
                Ok(Value::Null)
            },
        ),
        spec(
            "graphics.setTextType",
            "Text Layer Type",
            &[],
            None,
            r#"{"clip":id?,"layer":n|name?,"type":"point|paragraph" (point: no box, handles scale the text; paragraph: the text wraps in a box that handles resize)}"#,
            has_graphic,
            set_text_type,
        ),
        spec("graphics.selectLayer", "Select Graphic Layer", &[], None, r#"{"clip":id?,"layers":[n]}"#, has_graphic, |s, p| {
            let clip = target_clip(s, p).ok_or_else(|| bad("graphics.selectLayer", "no graphic clip"))?;
            let layers: Vec<usize> =
                p.get("layers").and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(|v| v as usize).collect()).unwrap_or_default();
            s.state.selection = vec![clip];
            s.state.graphic_layers = layers;
            Ok(Value::Null)
        }),
        spec("graphics.deleteLayer", "Delete Graphic Layer", &[], None, r#"{"clip":id?,"layer":n?}"#, has_graphic, |s, p| {
            let clip = target_clip(s, p).ok_or_else(|| bad("graphics.deleteLayer", "no graphic clip"))?;
            let (_, ei) = layer_effect_index(s, clip, p)?;
            s.edit_sequence("Delete Graphic Layer", |q, _, st| {
                let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
                it.effects.remove(ei);
                st.graphic_layers.clear();
                Ok(())
            })?;
            Ok(Value::Null)
        }),
        spec(
            "graphics.arrangeLayer",
            "Arrange Graphic Layer",
            &[],
            None,
            r#"{"clip":id?,"layer":n?,"to":"front|back|forward|backward"|index}"#,
            has_graphic,
            |s, p| {
                let to = p.get("to").cloned().unwrap_or(json!("front"));
                arrange_layer(s, p, to)
            },
        ),
        spec(
            "graphics.align",
            "Align Layers",
            &[],
            None,
            r#"{"clip":id?,"layers":[n]?,"align":"left|hcenter|right|top|vcenter|bottom","to":"frame|group|selection"="frame" (frame: each layer; group: the layers' union; one layer always aligns to the frame)}"#,
            has_graphic,
            |s, p| {
                let how = str_p(p, "align").ok_or_else(|| bad("graphics.align", "need `align`"))?.to_string();
                let mode = match str_p(p, "to") {
                    Some("selection") => AlignTo::Selection,
                    Some("group") => AlignTo::FrameGroup,
                    _ => AlignTo::Frame,
                };
                align_layers(s, p, &how, mode)
            },
        ),
        spec(
            "graphics.distribute",
            "Distribute Layers",
            &[],
            None,
            r#"{"clip":id?,"layers":[n] (3 or more)?,"axis":"horizontal|vertical","space":bool=false (equal gaps instead of equal centre spacing)}"#,
            has_graphic,
            |s, p| {
                let vertical = str_p(p, "axis") == Some("vertical");
                let space = p.get("space").and_then(Value::as_bool).unwrap_or(false);
                distribute_layers(s, p, vertical, space)
            },
        ),
        query("graphics.list", "List Graphic Layers", r#"{"clip":id?}"#, |s, p| {
            let clip = target_clip(s, p).ok_or_else(|| bad("graphics.list", "no graphic clip"))?;
            list_layers(s, clip)
        }),
        query("fonts.list", "List Fonts", r#"{"system":bool=true (scan the system font folders)}"#, |_, p| {
            if p.get("system").and_then(Value::as_bool).unwrap_or(true) {
                filmcraft_text::fonts::scan_system();
            }
            Ok(json!(filmcraft_text::families().into_iter().map(|(f, st)| json!({"family": f, "styles": st})).collect::<Vec<_>>()))
        }),
    ];
    v.extend(menu_commands());
    v
}

#[cfg(test)]
#[path = "graphics_tests.rs"]
mod tests;

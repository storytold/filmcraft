use super::common::{item_rate, media_root};
use super::{
    BTreeMap, ClipId, EngineError, FieldOptions, FieldProcessing, FrameRate, ParamValue, Result, Session, TICKS_PER_SECOND, Tick, TimeInterpolation, TrackItem,
    Value, bad, bool_p, clips_p, edit, f64_p, json, parse_timecode, str_p, time_p,
};

// ---------------------------------------------------------------------------------------------
// Clip: Video Options
// ---------------------------------------------------------------------------------------------

fn video_clips(s: &Session, p: &Value) -> Vec<ClipId> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    clips_p(s, p).into_iter().filter(|c| q.video_tracks.iter().any(|t| t.item(*c).is_some())).collect()
}

pub(super) fn frame_hold_options(s: &mut Session, p: &Value) -> Result<Value> {
    let clips = video_clips(s, p);
    if clips.is_empty() {
        return Err(bad("clip.frameHoldOptions", "select a video clip"));
    }
    let enabled = bool_p(p, "enabled").unwrap_or(true);
    let hold_filters = bool_p(p, "holdFilters").unwrap_or(false);
    let mode = str_p(p, "holdOn").unwrap_or("in").to_string();
    let ph = s.playhead();
    let seq_t = time_p(s, p, "");
    let tc = str_p(p, "timecode").map(str::to_string);
    let rates: BTreeMap<ClipId, (FrameRate, i64)> = {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        clips
            .iter()
            .filter_map(|c| {
                let (_, it) = q.find_item(*c)?;
                let start_tc = media_root(&s.project, it.item).and_then(|(_, m, _)| m.info.start_timecode).unwrap_or(0);
                Some((*c, (item_rate(&s.project, it.item), start_tc)))
            })
            .collect()
    };
    let raw_time = p.get("time").and_then(Value::as_i64).map(Tick);
    let held = s.edit_sequence("Frame Hold Options", |q, _, _| {
        let mut out = Vec::new();
        for c in &clips {
            let Some((_, it)) = q.find_item_mut(*c) else { continue };
            if !enabled {
                it.frame_hold = None;
                it.hold_filters = false;
                continue;
            }
            let last = it.end() - Tick(1);
            let unheld = |it: &TrackItem, t: Tick| it.moving_source_time_at(t.clamp(it.start, last));
            let h = match mode.as_str() {
                "in" => unheld(it, it.start),
                "out" => unheld(it, last),
                "playhead" => unheld(it, ph),
                "sequenceTime" => unheld(it, seq_t.ok_or_else(|| bad("clip.frameHoldOptions", "sequenceTime needs `time`/`timecode`"))?),
                "sourceTimecode" => {
                    let (rate, start_tc) = rates[c];
                    match (&tc, raw_time) {
                        (Some(tc), _) => {
                            let f = parse_timecode(tc, rate, false, 0).map_err(|e| bad("clip.frameHoldOptions", format!("timecode: {e:?}")))?;
                            rate.tick_of(f - start_tc)
                        }
                        (None, Some(t)) => t,
                        (None, None) => return Err(bad("clip.frameHoldOptions", "sourceTimecode needs `timecode` or `time` (media ticks)")),
                    }
                }
                m => return Err(bad("clip.frameHoldOptions", format!("unknown holdOn `{m}`"))),
            };
            let rate = rates.get(c).map(|r| r.0).unwrap_or_default();
            let h = rate.snap(h.max(Tick::ZERO));
            it.frame_hold = Some(h);
            it.hold_filters = hold_filters;
            out.push(json!({"clip": c.0, "hold": h.0}));
        }
        Ok(out)
    })?;
    Ok(json!({"clips": held}))
}

/// Add Frame Hold: the selected clips (or the targeted video clip under the playhead) hold the
/// frame at the playhead from there to their end; the clip is split at the playhead first.
pub(crate) fn add_frame_hold(s: &mut Session, p: &Value) -> Result<Value> {
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let mut clips: Vec<ClipId> = video_clips(s, p).into_iter().filter(|c| q.find_item(*c).is_some_and(|(_, i)| i.start <= t && t < i.end())).collect();
    if clips.is_empty() && p.get("clips").is_none() && p.get("clip").is_none() {
        let tg = s.targeting().targeted;
        clips = q.video_tracks.iter().filter(|tr| tg.contains(&tr.id) && !tr.locked).filter_map(|tr| tr.item_at(t).map(|i| i.id)).take(1).collect();
    }
    if clips.is_empty() {
        return Err(EngineError::Other("place the playhead over a selected video clip".into()));
    }
    let held = s.edit_sequence("Add Frame Hold", |q, ctx, st| {
        let mut out = Vec::new();
        for c in &clips {
            let Some((_, it)) = q.find_item(*c) else { continue };
            let hold = it.moving_source_time_at(t);
            let target = if it.start < t { edit::razor_items(q, &[*c], t, ctx).first().copied() } else { Some(*c) };
            if let Some(id) = target
                && let Some((_, r)) = q.find_item_mut(id)
            {
                r.frame_hold = Some(hold);
                out.push(id);
            }
        }
        st.selection = out.clone();
        Ok(out)
    })?;
    Ok(json!({"clips": held.iter().map(|c| c.0).collect::<Vec<_>>()}))
}

pub(super) fn insert_frame_hold_segment(s: &mut Session, p: &Value) -> Result<Value> {
    let t = time_p(s, p, "").unwrap_or(s.playhead());
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let rate = q.settings.frame_rate;
    let pick = |c: &ClipId| q.video_tracks.iter().find_map(|tr| tr.item(*c).filter(|i| i.start <= t && t < i.end()).map(|i| (tr.id, i.clone())));
    let mut hit = crate::commands::clip_p(p, "clip").and_then(|c| pick(&c)).or_else(|| s.state.selection.iter().find_map(pick));
    if hit.is_none() && crate::commands::clip_p(p, "clip").is_none() {
        let tg = s.targeting().targeted;
        hit = q.video_tracks.iter().filter(|tr| tg.contains(&tr.id) && !tr.locked).find_map(|tr| tr.item_at(t).map(|i| (tr.id, i.clone())));
    }
    let (track, it) = hit.ok_or_else(|| EngineError::Other("place the playhead over a video clip".into()))?;
    let secs = f64_p(p, "seconds").unwrap_or(2.0);
    let dur = rate.snap_nearest(Tick((secs * TICKS_PER_SECOND as f64).round() as i64)).max(rate.frame_duration());
    let hold = it.moving_source_time_at(t);
    let id = s.edit_sequence("Insert Frame Hold Segment", |q, ctx, st| {
        let mut seg = it.clone();
        seg.id = ClipId(ctx.alloc());
        seg.start = t;
        seg.duration = dur;
        seg.source_in = hold;
        seg.frame_hold = Some(hold);
        seg.link = None;
        seg.markers.clear();
        let ids = edit::insert(q, vec![(track, seg)], ctx)?;
        st.selection = ids.clone();
        Ok(ids[0])
    })?;
    Ok(json!({"clip": id.0, "duration": dur.0}))
}

pub(super) fn field_options(s: &mut Session, p: &Value) -> Result<Value> {
    let clips = video_clips(s, p);
    if clips.is_empty() {
        return Err(bad("clip.fieldOptions", "select a video clip"));
    }
    let processing = match str_p(p, "processing").unwrap_or("none") {
        "none" => FieldProcessing::None,
        "alwaysDeinterlace" => FieldProcessing::AlwaysDeinterlace,
        "flickerRemoval" => FieldProcessing::FlickerRemoval,
        x => return Err(bad("clip.fieldOptions", format!("unknown processing `{x}`"))),
    };
    let fo = FieldOptions { reverse_field_dominance: bool_p(p, "reverseFieldDominance").unwrap_or(false), processing };
    s.edit_sequence("Field Options", |q, _, _| {
        for c in &clips {
            if let Some((_, it)) = q.find_item_mut(*c) {
                it.field_options = (fo != FieldOptions::default()).then_some(fo);
            }
        }
        Ok(())
    })?;
    Ok(json!({"clips": clips.len()}))
}

pub(super) fn set_interpolation(s: &mut Session, p: &Value, m: TimeInterpolation) -> Result<Value> {
    let clips = video_clips(s, p);
    if clips.is_empty() {
        return Err(bad("clip.setTimeInterpolation", "select a video clip"));
    }
    s.edit_sequence(&format!("Time Interpolation: {}", m.label()), |q, _, _| {
        for c in &clips {
            if let Some((_, it)) = q.find_item_mut(*c) {
                it.time_interpolation = m;
            }
        }
        Ok(())
    })?;
    Ok(json!({"mode": m.name(), "clips": clips.len()}))
}

/// Fit to frame (the whole clip visible) / Fill frame (the frame covered): sets Motion ▸ Scale
/// and centres the clip.
pub(super) fn fit_fill(s: &mut Session, p: &Value, fill: bool) -> Result<Value> {
    let clips = video_clips(s, p);
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (fw, fh) = (q.settings.width as f64, q.settings.height as f64);
    let frame_par = q.settings.par;
    // (clip, storage size for the anchor, size at the display aspect in sequence pixels for the scale)
    let sizes: Vec<(ClipId, (u32, u32), (f64, f64))> = clips
        .iter()
        .filter_map(|c| {
            let (_, i) = q.find_item(*c)?;
            Some((*c, filmcraft_render::source_size(&s.project, i.item)?, s.project.conformed_source_size(i.item, frame_par)?))
        })
        .collect();
    if sizes.is_empty() {
        return Err(bad(if fill { "clip.fillFrame" } else { "clip.fitToFrame" }, "select a video clip"));
    }
    let label = if fill { "Fill Frame" } else { "Fit to Frame" };
    let out = s.edit_sequence(label, |q, _, _| {
        let mut out = Vec::new();
        for (c, (w, h), (dw, dh)) in &sizes {
            let Some((_, it)) = q.find_item_mut(*c) else { continue };
            let (sx, sy) = (fw / dw.max(1.0), fh / dh.max(1.0));
            let pct = (if fill { sx.max(sy) } else { sx.min(sy) }) * 100.0;
            it.scale_to_frame = false;
            let Some(m) = it.effect_mut("motion") else { continue };
            for (k, v) in [
                ("scale", ParamValue::Float(pct)),
                ("uniform_scale", ParamValue::Bool(true)),
                ("position", ParamValue::Vec2(filmcraft_geom::Vec2::new(fw / 2.0, fh / 2.0))),
                ("anchor", ParamValue::Vec2(filmcraft_geom::Vec2::new(*w as f64 / 2.0, *h as f64 / 2.0))),
            ] {
                match m.param_mut(k) {
                    Some(prm) => {
                        prm.keyframes.clear();
                        prm.value = v;
                    }
                    None => {
                        m.params.insert(k.to_string(), filmcraft_project::Param::new(v));
                    }
                }
            }
            out.push(json!({"clip": c.0, "scale": pct}));
        }
        Ok(out)
    })?;
    Ok(json!({"clips": out}))
}

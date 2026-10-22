//! OpenTimelineIO JSON (`.otio`).
//!
//! Schemas read and written: `Timeline.1`, `Stack.1`, `Track.1`, `Clip.1`/`Clip.2`, `Gap.1`,
//! `Transition.1`, `Marker.1`/`Marker.2`, `ExternalReference.1`, `MissingReference.1`,
//! `GeneratorReference.1`, `LinearTimeWarp.1`, `FreezeFrame.1`, `Effect.1`, `RationalTime.1`,
//! `TimeRange.1` and `SerializableCollection.1` (several timelines).
//!
//! Times are written as frames at the sequence rate (`rate` is the exact rate as a float, e.g.
//! 23.976023976023978) and read back exactly by snapping the rate to its rational form. Fields
//! OTIO has no schema for (effects and their keyframes, labels, links, groups, clip gain, track
//! state, sequence settings, media info, generators) round-trip through `metadata.filmcraft`.
//! Nested sequences are `Stack` items inside tracks.

use std::collections::HashMap;

use filmcraft_media::{Generator, MediaInfo};
use filmcraft_project::{
    ClipId, EffectInstance, ItemId, ItemKind, Label, Marker, MarkerId, MarkerKind, MediaRef, ParamValue, Project, Sequence, SequenceSettings, Track, TrackItem,
    TrackKind, Transition, TransitionAlign, TransitionId,
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick};
use serde_json::{Value, json};

use crate::common::{
    Builder, MediaSpec, base_item, empty_sequence, file_name, generator_of, item_media, path_to_file_url, rate_from_f64, relative_path, resolve_path,
    settings_for, transition_effect, transition_name,
};
use crate::{Error, ExportOptions, Format, ImportOptions, Imported, Report, Result};

// ---------------------------------------------------------------------------------------------
// Time
// ---------------------------------------------------------------------------------------------

fn rt(t: Tick, rate: FrameRate) -> Value {
    let f = rate.frame_at(t);
    let value = if rate.tick_of(f) == t { json!(f as f64) } else { json!(t.0 as f64 * rate.num as f64 / (rate.den as f64 * TICKS_PER_SECOND as f64)) };
    json!({"OTIO_SCHEMA": "RationalTime.1", "rate": rate.as_f64(), "value": value})
}

fn tr(start: Tick, dur: Tick, rate: FrameRate) -> Value {
    json!({"OTIO_SCHEMA": "TimeRange.1", "start_time": rt(start, rate), "duration": rt(dur, rate)})
}

/// RationalTime → ticks (exact for integral values at broadcast rates).
fn read_rt(v: &Value) -> Option<Tick> {
    let rate = v.get("rate")?.as_f64()?;
    let value = v.get("value")?.as_f64()?;
    let r = rate_from_f64(rate);
    if value == value.round() && value.abs() < 9e15 {
        return Some(Tick::from_rational(value as i64, r.den, r.num));
    }
    Some(Tick((value * TICKS_PER_SECOND as f64 * r.den as f64 / r.num as f64).round() as i64))
}

fn read_rate(v: &Value) -> Option<FrameRate> {
    v.get("rate").and_then(Value::as_f64).map(rate_from_f64)
}

fn read_tr(v: &Value) -> Option<(Tick, Tick)> {
    if v.is_null() {
        return None;
    }
    Some((read_rt(v.get("start_time")?)?, read_rt(v.get("duration")?)?))
}

fn schema(v: &Value) -> &str {
    v.get("OTIO_SCHEMA").and_then(Value::as_str).unwrap_or("")
}

fn schema_name(v: &Value) -> &str {
    schema(v).split('.').next().unwrap_or("")
}

fn fc(v: &Value) -> Option<&Value> {
    v.get("metadata").and_then(|m| m.get("filmcraft"))
}

fn name_of(v: &Value) -> String {
    v.get("name").and_then(Value::as_str).unwrap_or("").to_string()
}

fn color_name(l: Label) -> &'static str {
    match l {
        Label::Rose => "RED",
        Label::Mango | Label::Tan => "ORANGE",
        Label::Yellow => "YELLOW",
        Label::Green | Label::Forest => "GREEN",
        Label::Teal | Label::Caribbean => "CYAN",
        Label::Blue | Label::Iris | Label::Cerulean => "BLUE",
        Label::Violet | Label::Purple | Label::Lavender => "PURPLE",
        Label::Magenta => "MAGENTA",
        Label::Brown => "BLACK",
    }
}

fn color_label(s: &str) -> Label {
    match s.to_ascii_uppercase().as_str() {
        "RED" => Label::Rose,
        "PINK" | "MAGENTA" => Label::Magenta,
        "ORANGE" => Label::Mango,
        "YELLOW" => Label::Yellow,
        "GREEN" => Label::Green,
        "CYAN" => Label::Teal,
        "BLUE" => Label::Blue,
        "PURPLE" => Label::Purple,
        "BLACK" => Label::Brown,
        "WHITE" => Label::Lavender,
        _ => Label::Green,
    }
}

// ---------------------------------------------------------------------------------------------
// Export
// ---------------------------------------------------------------------------------------------

struct Exp<'a, 'r> {
    p: &'a Project,
    opts: &'a ExportOptions,
    report: &'r mut Report,
    stack: Vec<ItemId>,
}

pub(crate) fn export(p: &Project, seq_id: ItemId, opts: &ExportOptions, report: &mut Report) -> Result<String> {
    let seq = p.sequence(seq_id).ok_or(Error::NoSequence(seq_id))?;
    let rate = seq.settings.frame_rate;
    let mut x = Exp { p, opts, report, stack: vec![seq_id] };
    let name = opts.name.clone().unwrap_or_else(|| p.item(seq_id).map(|i| i.name.clone()).unwrap_or_default());
    let stack = x.stack_json(seq_id, &name, None);
    let tl = json!({
        "OTIO_SCHEMA": "Timeline.1",
        "name": name,
        "global_start_time": rt(rate.tick_of(seq.start_timecode), rate),
        "metadata": {"filmcraft": x.seq_meta(seq_id)},
        "tracks": stack,
    });
    serde_json::to_string_pretty(&tl).map_err(|e| Error::Other(e.to_string()))
}

impl Exp<'_, '_> {
    fn seq_meta(&self, id: ItemId) -> Value {
        let Some((it, s)) = self.p.item(id).and_then(|it| Some((it, it.as_sequence()?))) else { return json!({}) };
        json!({
            "item": id.0, "custom_colors": self.p.custom_colors, "custom_color_names": self.p.custom_color_names,
            "label": it.label, "custom_label_color": it.custom_label_color,
            "settings": s.settings,
            "start_timecode": s.start_timecode,
            "master_volume_db": s.master_volume_db,
            "master_effects": s.master_effects,
        })
    }

    /// The `Stack` for a sequence; `range` = (source in, duration) when nested in a track.
    fn stack_json(&mut self, id: ItemId, name: &str, range: Option<(Tick, Tick, FrameRate)>) -> Value {
        let Some(seq) = self.p.sequence(id) else {
            self.report.warn("missing nested sequence skipped");
            return json!({"OTIO_SCHEMA": "Stack.1", "name": name, "children": [], "effects": [], "markers": [], "enabled": true, "metadata": {}});
        };
        let rate = seq.settings.frame_rate;
        let mut tracks = Vec::new();
        for kind in [TrackKind::Video, TrackKind::Audio] {
            for t in seq.tracks(kind) {
                tracks.push(self.track_json(t, kind, seq));
            }
        }
        let markers: Vec<Value> = seq.markers.iter().map(|m| marker_json(m, rate)).collect();
        json!({
            "OTIO_SCHEMA": "Stack.1",
            "name": name,
            "children": tracks,
            "source_range": range.map(|(s, d, r)| tr(s, d, r)).unwrap_or(Value::Null),
            "effects": [],
            "markers": markers,
            "enabled": true,
            "metadata": if range.is_some() { json!({"filmcraft": self.seq_meta(id)}) } else { json!({}) },
        })
    }

    fn track_json(&mut self, t: &Track, kind: TrackKind, seq: &Sequence) -> Value {
        let rate = seq.settings.frame_rate;
        let mut children = Vec::new();
        let mut pos = Tick::ZERO;
        for it in &t.items {
            if it.start > pos {
                children.push(json!({"OTIO_SCHEMA": "Gap.1", "name": "", "source_range": tr(Tick::ZERO, it.start - pos, rate), "effects": [], "markers": [], "enabled": true, "metadata": {}}));
            }
            for x in t.transitions.iter().filter(|x| x.to == Some(it.id)) {
                children.push(self.transition_json(x, it.start, rate));
            }
            children.push(self.item_json(it, kind, rate));
            for x in t.transitions.iter().filter(|x| x.from == Some(it.id) && x.to.is_none()) {
                children.push(self.transition_json(x, it.end(), rate));
            }
            pos = it.end();
        }
        json!({
            "OTIO_SCHEMA": "Track.1",
            "name": t.name,
            "kind": if kind == TrackKind::Video { "Video" } else { "Audio" },
            "children": children,
            "source_range": null,
            "effects": [],
            "markers": [],
            "enabled": t.enabled,
            "metadata": {"filmcraft": {
                "locked": t.locked, "sync_lock": t.sync_lock, "muted": t.muted, "solo": t.solo,
                "channels": t.channels, "volume_db": t.volume_db, "pan": t.pan, "effects": t.effects,
            }},
        })
    }

    fn transition_json(&mut self, x: &Transition, cut: Tick, rate: FrameRate) -> Value {
        let dissolve = matches!(x.effect.effect.as_str(), "cross_dissolve" | "constant_power" | "constant_gain" | "film_dissolve");
        json!({
            "OTIO_SCHEMA": "Transition.1",
            "name": transition_name(&x.effect),
            "transition_type": if dissolve { "SMPTE_Dissolve" } else { "Custom_Transition" },
            "in_offset": rt(cut - x.start, rate),
            "out_offset": rt(x.end() - cut, rate),
            "metadata": {"filmcraft": {"effect": x.effect, "align": x.align, "reverse": x.reverse}},
        })
    }

    fn media_ref(&mut self, item: ItemId, rate: FrameRate) -> Value {
        let name = self.p.item(item).map(|i| i.name.clone()).unwrap_or_default();
        let Some(m) = item_media(self.p, item) else {
            return json!({"OTIO_SCHEMA": "MissingReference.1", "name": name, "available_range": null, "metadata": {}});
        };
        let avail = tr(m.info.start_timecode.map(|f| m.frame_rate().tick_of(f)).unwrap_or(Tick::ZERO), m.info.duration, rate);
        let meta = json!({"filmcraft": {"item": item.0, "label": self.p.item(item).map(|i| i.label), "custom_label_color": self.p.item(item).and_then(|i| i.custom_label_color), "info": m.info, "offline": m.offline}});
        match &m.media {
            MediaRef::Generator(g) => json!({
                "OTIO_SCHEMA": "GeneratorReference.1",
                "name": name,
                "generator_kind": match g { Generator::ColorMatte { .. } => "SolidColor", Generator::BlackVideo => "black", Generator::BarsAndTone => "SMPTEBars", _ => "FilmCraftGenerator" },
                "parameters": match g { Generator::ColorMatte { color } => json!({"color": color}), _ => json!({}) },
                "available_range": avail,
                "metadata": {"filmcraft": {"item": item.0, "generator": g, "info": m.info, "custom_label_color": self.p.item(item).and_then(|i| i.custom_label_color)}},
            }),
            MediaRef::File { path } => {
                let url = match &self.opts.relative_to {
                    Some(base) => relative_path(path, base).unwrap_or_else(|| abs_url(path)),
                    None => abs_url(path),
                };
                json!({"OTIO_SCHEMA": "ExternalReference.1", "name": name, "target_url": url, "available_range": avail, "available_image_bounds": null, "metadata": meta})
            }
        }
    }

    fn item_json(&mut self, it: &TrackItem, kind: TrackKind, rate: FrameRate) -> Value {
        let base = base_item(self.p, it.item);
        let mut effects = Vec::new();
        if let Some(h) = it.frame_hold {
            effects.push(
                json!({"OTIO_SCHEMA": "FreezeFrame.1", "name": "", "effect_name": "FreezeFrame", "time_scalar": 0.0, "metadata": {"filmcraft": {"hold": h}}}),
            );
        } else if it.speed != 1.0 || it.reverse {
            let s = if it.reverse { -it.speed } else { it.speed };
            effects.push(json!({"OTIO_SCHEMA": "LinearTimeWarp.1", "name": "", "effect_name": "LinearTimeWarp", "time_scalar": s, "metadata": {}}));
        }
        let meta = json!({"filmcraft": {
            "label": it.label, "custom_label_color": it.custom_label_color, "link": it.link, "group": it.group, "effects": it.effects,
            "gain_db": it.gain_db, "scale_to_frame": it.scale_to_frame, "source_in_ticks": it.source_in.0,
            "duration_ticks": it.duration.0,
        }});
        let markers: Vec<Value> = it.markers.iter().map(|m| marker_json(m, rate)).collect();
        let _ = kind;
        if matches!(self.p.item(base).map(|i| &i.kind), Some(ItemKind::Sequence(_))) {
            if self.stack.contains(&base) {
                self.report.warn("recursive nested sequence skipped");
                return json!({"OTIO_SCHEMA": "Gap.1", "name": "", "source_range": tr(Tick::ZERO, it.duration, rate), "effects": [], "markers": [], "enabled": true, "metadata": {}});
            }
            self.stack.push(base);
            let mut s = self.stack_json(base, &it.name, Some((it.source_in, it.duration, rate)));
            self.stack.pop();
            s["effects"] = json!(effects);
            s["enabled"] = json!(it.enabled);
            s["metadata"]["filmcraft"]["clip"] = meta["filmcraft"].clone();
            // Nested sequence markers stay on the Stack; clip instance markers live on the clip metadata.
            s["metadata"]["filmcraft"]["clip"]["markers"] = json!(markers);
            return s;
        }
        let _ = generator_of(self.p, base);
        if let Some(clip) = crate::common::uncarried_clip(self.p, it, rate) {
            self.report
                .warn(format!("{clip} has no OTIO equivalent: it is written as a clip with a missing media reference and is read back as offline media"));
        }
        let mref = self.media_ref(base, rate);
        // OTIO source times are in the reference's time (which starts at its available_range start).
        let origin = item_media(self.p, base).and_then(|m| m.info.start_timecode.map(|f| m.frame_rate().tick_of(f))).unwrap_or(Tick::ZERO);
        let markers: Vec<Value> = it.markers.iter().map(|m| marker_json(&Marker { start: m.start + origin, ..m.clone() }, rate)).collect();
        json!({
            "OTIO_SCHEMA": "Clip.2",
            "name": it.name,
            "source_range": tr(origin + it.source_in, it.duration, rate),
            "effects": effects,
            "markers": markers,
            "enabled": it.enabled,
            "media_references": {"DEFAULT_MEDIA": mref},
            "active_media_reference_key": "DEFAULT_MEDIA",
            "metadata": meta,
        })
    }
}

fn abs_url(path: &str) -> String {
    if crate::common::is_absolute(path) { path_to_file_url(path, false) } else { path.to_string() }
}

fn marker_json(m: &Marker, rate: FrameRate) -> Value {
    json!({
        "OTIO_SCHEMA": "Marker.2",
        "name": m.name,
        "comment": m.comment,
        "color": color_name(m.color),
        "marked_range": tr(m.start, m.duration, rate),
        "metadata": {"filmcraft": {"kind": m.kind, "label": m.color, "custom_color": m.custom_color, "author": m.author, "start_ticks": m.start.0, "duration_ticks": m.duration.0}},
    })
}

/// JSON turns NaN (our "auto" point defaults) into `null`; restore it when reading effects back.
const NAN_SENTINEL: f64 = 1.0e308;

fn null_to_sentinel(v: &mut Value) {
    match v {
        Value::Object(o) => {
            for (k, x) in o.iter_mut() {
                if x.is_null() && (k == "x" || k == "y") {
                    *x = json!(NAN_SENTINEL);
                } else {
                    null_to_sentinel(x);
                }
            }
        }
        Value::Array(a) => a.iter_mut().for_each(null_to_sentinel),
        _ => {}
    }
}

fn effects_from(v: Option<&Value>) -> Option<Vec<EffectInstance>> {
    let mut v = v?.clone();
    null_to_sentinel(&mut v);
    let mut effects: Vec<EffectInstance> = serde_json::from_value(v).ok()?;
    let fix = |p: &mut ParamValue| {
        if let ParamValue::Vec2(v) = p {
            if v.x == NAN_SENTINEL {
                v.x = f64::NAN;
            }
            if v.y == NAN_SENTINEL {
                v.y = f64::NAN;
            }
        }
    };
    for e in &mut effects {
        for p in e.params.values_mut() {
            fix(&mut p.value);
            p.keyframes.iter_mut().for_each(|k| fix(&mut k.value));
        }
    }
    Some(effects)
}

// ---------------------------------------------------------------------------------------------
// Import
// ---------------------------------------------------------------------------------------------

struct Imp<'a, 'r> {
    b: Builder,
    opts: &'a ImportOptions,
    report: &'r mut Report,
    links: HashMap<u64, u64>,
    groups: HashMap<u64, u64>,
}

pub(crate) fn import(text: &str, opts: &ImportOptions, report: &mut Report) -> Result<Imported> {
    let v: Value = serde_json::from_str(text).map_err(|e| Error::parse(Format::Otio, e.to_string()))?;
    let mut timelines = Vec::new();
    collect_timelines(&v, &mut timelines);
    if timelines.is_empty() {
        return Err(Error::parse(Format::Otio, format!("no Timeline in a {} document", schema(&v))));
    }
    let mut imp = Imp { b: Builder::new(&name_of(&v)), opts, report, links: HashMap::new(), groups: HashMap::new() };
    if imp.b.p.name.is_empty() {
        imp.b.p.name = "Imported OTIO".into();
    }
    for tl in timelines {
        let id = imp.timeline(tl);
        imp.b.top.push(id);
    }
    Ok(imp.b.finish())
}

fn collect_timelines<'v>(v: &'v Value, out: &mut Vec<&'v Value>) {
    match schema_name(v) {
        "Timeline" | "Stack" => out.push(v),
        "SerializableCollection" => {
            for c in v.get("children").and_then(Value::as_array).into_iter().flatten() {
                collect_timelines(c, out);
            }
        }
        "Track" => out.push(v),
        _ => {}
    }
}

fn first_rate(v: &Value) -> Option<FrameRate> {
    if let Some(sr) = v.get("source_range").filter(|s| !s.is_null()) {
        return sr.get("duration").and_then(read_rate);
    }
    v.get("children").and_then(Value::as_array).into_iter().flatten().find_map(first_rate)
}

impl Imp<'_, '_> {
    fn timeline(&mut self, tl: &Value) -> ItemId {
        let name = name_of(tl);
        let name = if name.is_empty() { self.opts.name.clone().unwrap_or_else(|| "Timeline".into()) } else { name };
        let stack_owned;
        let stack = match schema_name(tl) {
            "Timeline" => tl.get("tracks").unwrap_or(&Value::Null),
            "Track" => {
                stack_owned = json!({"OTIO_SCHEMA": "Stack.1", "children": [tl.clone()]});
                &stack_owned
            }
            _ => tl,
        };
        let meta = fc(tl).or_else(|| fc(stack));
        if let Some(colors) = meta.and_then(|m| m.get("custom_colors")).and_then(Value::as_array) {
            self.b.p.custom_colors = colors.iter().filter_map(Value::as_str).filter_map(filmcraft_project::HexColor::parse).take(64).collect();
        }
        if let Some(names) = meta.and_then(|m| m.get("custom_color_names")).and_then(Value::as_object) {
            for (key, value) in names.iter().take(64) {
                if let (Some(color), Some(name)) = (filmcraft_project::HexColor::parse(key), value.as_str())
                    && self.b.p.custom_colors.contains(&color)
                    && name.len() <= 128
                    && !name.chars().any(char::is_control)
                {
                    self.b.p.custom_color_names.insert(color, name.to_owned());
                }
            }
        }

        let rate = tl.get("global_start_time").and_then(read_rate).or_else(|| first_rate(stack)).unwrap_or_default();
        let id = self.sequence(stack, &name, meta, rate);
        if let Some(st) = tl.get("global_start_time").and_then(read_rt)
            && let Some(s) = self.b.p.sequence_mut(id)
        {
            s.start_timecode = s.settings.frame_rate.frame_at(st);
        }
        id
    }

    fn sequence(&mut self, stack: &Value, name: &str, meta: Option<&Value>, rate: FrameRate) -> ItemId {
        let settings: SequenceSettings =
            meta.and_then(|m| m.get("settings")).and_then(|s| serde_json::from_value(s.clone()).ok()).unwrap_or_else(|| settings_for(rate, 1920, 1080, false));
        let id = self.b.reserve_sequence(name, settings.clone(), None);
        if let Some(l) = meta.and_then(|m| m.get("label")).and_then(|l| serde_json::from_value::<Label>(l.clone()).ok())
            && let Some(it) = self.b.p.item_mut(id)
        {
            it.label = l;
        }
        if let Some(it) = self.b.p.item_mut(id) {
            it.custom_label_color = meta.and_then(|m| m.get("custom_label_color")).and_then(Value::as_str).and_then(filmcraft_project::HexColor::parse);
        }
        let mut seq = empty_sequence(settings);
        if let Some(m) = meta {
            seq.start_timecode = m.get("start_timecode").and_then(Value::as_i64).unwrap_or(0);
            seq.master_volume_db = m.get("master_volume_db").and_then(Value::as_f64).unwrap_or(0.0);
            seq.master_effects = effects_from(m.get("master_effects")).unwrap_or_default();
        }
        seq.markers = self.markers(stack);
        for t in stack.get("children").and_then(Value::as_array).into_iter().flatten() {
            match schema_name(t) {
                "Track" => {
                    let kind = if t.get("kind").and_then(Value::as_str) == Some("Audio") { TrackKind::Audio } else { TrackKind::Video };
                    self.track(&mut seq, t, kind);
                }
                other => self.report.warn(format!("OTIO {other} directly in a stack is not supported")),
            }
        }
        self.b.ensure_tracks(&mut seq, TrackKind::Video, 1);
        self.b.ensure_tracks(&mut seq, TrackKind::Audio, 1);
        self.b.put_sequence(id, seq);
        id
    }

    fn track(&mut self, seq: &mut Sequence, t: &Value, kind: TrackKind) {
        let idx = seq.tracks(kind).len();
        let mut track = self.b.track(kind, idx);
        let n = name_of(t);
        if !n.is_empty() {
            track.name = n;
        }
        track.enabled = t.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        if let Some(m) = fc(t) {
            let g = |k: &str| m.get(k).and_then(Value::as_bool);
            track.locked = g("locked").unwrap_or(false);
            track.sync_lock = g("sync_lock").unwrap_or(true);
            track.muted = g("muted").unwrap_or(false);
            track.solo = g("solo").unwrap_or(false);
            track.volume_db = m.get("volume_db").and_then(Value::as_f64).unwrap_or(0.0);
            track.pan = m.get("pan").and_then(Value::as_f64).unwrap_or(0.0);
            if let Some(c) = m.get("channels").and_then(|c| serde_json::from_value(c.clone()).ok()) {
                track.channels = c;
            }
            track.effects = effects_from(m.get("effects")).unwrap_or_default();
        }
        if t.get("markers").and_then(Value::as_array).is_some_and(|m| !m.is_empty()) {
            let ms = self.markers(t);
            seq.markers.extend(ms);
            seq.markers.sort_by_key(|m| m.start);
        }
        // (position in children list, clip at that slot or None for gaps)
        let mut slots: Vec<(Option<ClipId>, Tick, Tick)> = Vec::new();
        let mut pending: Vec<(usize, &Value)> = Vec::new();
        let mut pos = Tick::ZERO;
        let kids = t.get("children").and_then(Value::as_array).cloned().unwrap_or_default();
        for c in &kids {
            match schema_name(c) {
                "Transition" => {
                    pending.push((slots.len(), c));
                    continue;
                }
                "Gap" => {
                    let d = c.get("source_range").and_then(read_tr).map(|r| r.1).unwrap_or(Tick::ZERO);
                    slots.push((None, pos, pos + d));
                    pos += d;
                }
                "Clip" | "Stack" => {
                    let item = if schema_name(c) == "Clip" { self.clip(c, kind, pos) } else { self.nested(c, kind, pos) };
                    match item {
                        Some(ti) => {
                            let (s, e, id) = (ti.start, ti.end(), ti.id);
                            track.items.push(ti);
                            slots.push((Some(id), s, e));
                            pos = e;
                        }
                        None => {
                            let d = c.get("source_range").and_then(read_tr).map(|r| r.1).unwrap_or(Tick::ZERO);
                            slots.push((None, pos, pos + d));
                            pos += d;
                        }
                    }
                }
                other => {
                    self.report.warn(format!("OTIO {other} items are not supported and were skipped"));
                }
            }
        }
        for (slot, c) in pending {
            let cut = if slot < slots.len() { slots[slot].1 } else { pos };
            let from = slot.checked_sub(1).and_then(|i| slots[i].0);
            let to = slots.get(slot).and_then(|s| s.0);
            if from.is_none() && to.is_none() {
                continue;
            }
            let inn = c.get("in_offset").and_then(read_rt).unwrap_or(Tick::ZERO);
            let out = c.get("out_offset").and_then(read_rt).unwrap_or(Tick::ZERO);
            let meta = fc(c);
            let effect: EffectInstance = match meta.and_then(|m| m.get("effect")).and_then(|e| effects_from(Some(&json!([e])))).and_then(|mut v| v.pop()) {
                Some(e) => e,
                None => {
                    let n = name_of(c);
                    let ty = c.get("transition_type").and_then(Value::as_str).unwrap_or("SMPTE_Dissolve");
                    transition_effect(if n.is_empty() { ty } else { &n }, kind == TrackKind::Audio, self.report)
                }
            };
            let align = meta.and_then(|m| m.get("align")).and_then(|a| serde_json::from_value(a.clone()).ok()).unwrap_or(if inn == Tick::ZERO {
                TransitionAlign::StartAtCut
            } else if out == Tick::ZERO {
                TransitionAlign::EndAtCut
            } else {
                TransitionAlign::CenterAtCut
            });
            let reverse = meta.and_then(|m| m.get("reverse")).and_then(Value::as_bool).unwrap_or(false);
            track.transitions.push(Transition { id: TransitionId(self.b.alloc()), effect, start: cut - inn, duration: inn + out, from, to, align, reverse });
        }
        track.sort();
        seq.tracks_mut(kind).push(track);
    }

    fn apply_clip_meta(&mut self, ti: &mut TrackItem, c: &Value, meta: Option<&Value>) {
        ti.enabled = c.get("enabled").and_then(Value::as_bool).unwrap_or(true);
        for e in c.get("effects").and_then(Value::as_array).into_iter().flatten() {
            match schema_name(e) {
                "LinearTimeWarp" => {
                    let s = e.get("time_scalar").and_then(Value::as_f64).unwrap_or(1.0);
                    if s == 0.0 {
                        ti.frame_hold = Some(ti.source_in);
                    } else {
                        ti.speed = s.abs();
                        ti.reverse = s < 0.0;
                    }
                }
                "FreezeFrame" => {
                    ti.frame_hold = Some(fc(e).and_then(|m| m.get("hold")).and_then(|h| h.as_i64()).map(Tick).unwrap_or(ti.source_in));
                }
                _ => {
                    if meta.and_then(|m| m.get("effects")).is_none() {
                        let n = e.get("effect_name").and_then(Value::as_str).unwrap_or("effect");
                        self.report.warn(format!("OTIO effect \"{n}\" is not supported"));
                    }
                }
            }
        }
        let Some(m) = meta else { return };
        ti.custom_label_color = m.get("custom_label_color").and_then(Value::as_str).and_then(filmcraft_project::HexColor::parse);
        if let Some(l) = m.get("label").and_then(|l| serde_json::from_value::<Label>(l.clone()).ok()) {
            ti.label = l;
        }
        if let Some(effects) = effects_from(m.get("effects")) {
            ti.effects = effects;
        }
        if let Some(l) = m.get("link").and_then(Value::as_u64) {
            let next = self.b.link_id();
            ti.link = Some(*self.links.entry(l).or_insert(next));
        }
        if let Some(g) = m.get("group").and_then(Value::as_u64) {
            let next = self.b.alloc();
            ti.group = Some(*self.groups.entry(g).or_insert(next));
        }
        ti.gain_db = m.get("gain_db").and_then(Value::as_f64).unwrap_or(0.0);
        ti.scale_to_frame = m.get("scale_to_frame").and_then(Value::as_bool).unwrap_or(false);
        // Exact ticks for sub-frame values (audio) when the rational values were rounded.
        if let Some(s) = m.get("source_in_ticks").and_then(Value::as_i64)
            && (Tick(s) - ti.source_in).abs() < Tick(TICKS_PER_SECOND / 1000)
        {
            ti.source_in = Tick(s);
            if let Some(h) = &mut ti.frame_hold
                && c.get("effects").and_then(Value::as_array).is_some_and(|e| e.iter().any(|x| schema_name(x) == "LinearTimeWarp"))
            {
                *h = Tick(s);
            }
        }
        if let Some(d) = m.get("duration_ticks").and_then(Value::as_i64)
            && (Tick(d) - ti.duration).abs() < Tick(TICKS_PER_SECOND / 1000)
        {
            ti.duration = Tick(d);
        }
    }

    fn clip(&mut self, c: &Value, kind: TrackKind, pos: Tick) -> Option<TrackItem> {
        let mref = match c.get("media_references") {
            Some(refs) => {
                let key = c.get("active_media_reference_key").and_then(Value::as_str).unwrap_or("DEFAULT_MEDIA");
                refs.get(key).or_else(|| refs.as_object().and_then(|o| o.values().next()))
            }
            None => c.get("media_reference"),
        }
        .cloned()
        .unwrap_or(Value::Null);
        let name = name_of(c);
        let avail = mref.get("available_range").and_then(read_tr);
        let (src_in, dur) = c.get("source_range").and_then(read_tr).or(avail)?;
        if dur <= Tick::ZERO {
            return None;
        }
        let item = self.media(&mref, &name, kind, avail)?;
        // Sources are media time: subtract the media's start time (available_range start).
        let origin = avail.map(|a| a.0).unwrap_or(Tick::ZERO);
        let mut ti = self.b.clip(item, kind, if name.is_empty() { "Clip" } else { &name }, pos, dur, src_in - origin);
        ti.markers = self.markers(c);
        for m in &mut ti.markers {
            m.start -= origin;
        }
        let meta = fc(c).cloned();
        self.apply_clip_meta(&mut ti, c, meta.as_ref());
        Some(ti)
    }

    fn nested(&mut self, c: &Value, kind: TrackKind, pos: Tick) -> Option<TrackItem> {
        let (src_in, dur) = c.get("source_range").and_then(read_tr)?;
        let meta = fc(c);
        let key = format!("stack:{}", meta.and_then(|m| m.get("item")).and_then(Value::as_u64).map(|i| i.to_string()).unwrap_or_else(|| name_of(c)));
        let item = match self.b.find_media(&key) {
            Some(i) => i,
            None => {
                let rate = first_rate(c).unwrap_or_default();
                let mut inner = c.clone();
                inner["source_range"] = Value::Null;
                let name = name_of(c);
                let id = self.sequence(&inner, if name.is_empty() { "Nested Sequence" } else { &name }, meta, rate);
                self.b.register_media(&key, id);
                id
            }
        };
        let name = name_of(c);
        let mut ti = self.b.clip(item, kind, if name.is_empty() { "Nested Sequence" } else { &name }, pos, dur, src_in);
        let clip_meta = meta.and_then(|m| m.get("clip")).cloned();
        // Stack markers belong to the nested sequence; instance markers are on filmcraft.clip.
        if let Some(ref cm) = clip_meta {
            ti.markers = self.markers(cm);
        }
        self.apply_clip_meta(&mut ti, c, clip_meta.as_ref());
        Some(ti)
    }

    fn media(&mut self, r: &Value, clip_name: &str, kind: TrackKind, avail: Option<(Tick, Tick)>) -> Option<ItemId> {
        let meta = fc(r);
        let info: Option<MediaInfo> = meta.and_then(|m| m.get("info")).and_then(|i| serde_json::from_value(i.clone()).ok());
        let name = {
            let n = name_of(r);
            if n.is_empty() { clip_name.to_string() } else { n }
        };
        let spec = MediaSpec {
            duration: avail.map(|a| a.1),
            video: (kind == TrackKind::Video).then(|| (1920, 1080, FrameRate::default())),
            audio: (kind == TrackKind::Audio).then_some((48_000, 2)),
            start_tc: None,
            kind: None,
        };
        let id = match schema_name(r) {
            "ExternalReference" | "ImageSequenceReference" => {
                let url = r.get("target_url").and_then(Value::as_str).or_else(|| r.get("target_url_base").and_then(Value::as_str)).unwrap_or("");
                let path = resolve_path(url, self.opts.base_dir.as_deref());
                let display = if name.is_empty() { file_name(&path).to_string() } else { name.clone() };
                let key = format!("file:{path}");
                let existed = self.b.find_media(&key);
                let id = self.b.file_media(&key, &display, &path, &spec, None);
                if existed.is_none() {
                    self.fill_media(id, info, meta, kind);
                }
                id
            }
            "GeneratorReference" => {
                let g: Generator = meta.and_then(|m| m.get("generator")).and_then(|g| serde_json::from_value(g.clone()).ok()).unwrap_or_else(|| {
                    match r.get("generator_kind").and_then(Value::as_str).unwrap_or("").to_ascii_lowercase().as_str() {
                        "solidcolor" | "solid_color" => {
                            let c = r
                                .get("parameters")
                                .and_then(|p| p.get("color"))
                                .and_then(|c| serde_json::from_value::<[f32; 4]>(c.clone()).ok())
                                .unwrap_or([0.0, 0.0, 0.0, 1.0]);
                            Generator::ColorMatte { color: c }
                        }
                        "smptebars" | "bars" => Generator::BarsAndTone,
                        "black" => Generator::BlackVideo,
                        other => {
                            self.report.warn(format!("OTIO generator \"{other}\" is not supported; imported as Black Video"));
                            Generator::BlackVideo
                        }
                    }
                });
                let key = format!("gen:{}", serde_json::to_string(&g).unwrap_or_default());
                let existed = self.b.find_media(&key);
                let id = self.b.generator_media(&key, if name.is_empty() { "Generator" } else { &name }, g, &spec, None);
                if existed.is_none() {
                    self.fill_media(id, info, meta, kind);
                }
                id
            }
            _ => {
                if schema_name(r) != "MissingReference" && !r.is_null() {
                    self.report.warn(format!("OTIO media reference {} is not supported; imported offline", schema(r)));
                }
                let key = format!("missing:{name}");
                let existed = self.b.find_media(&key);
                let path = resolve_path(&name, self.opts.base_dir.as_deref());
                let id = self.b.file_media(&key, &name, &path, &spec, None);
                if existed.is_none() {
                    self.fill_media(id, info, meta, kind);
                    if let Some(m) = self.b.p.item_mut(id).and_then(|i| i.as_media_mut()) {
                        m.offline = true;
                    }
                }
                id
            }
        };
        Some(id)
    }

    fn fill_media(&mut self, id: ItemId, info: Option<MediaInfo>, meta: Option<&Value>, kind: TrackKind) {
        let label = meta.and_then(|m| m.get("label")).and_then(|l| serde_json::from_value::<Label>(l.clone()).ok());
        let offline = meta.and_then(|m| m.get("offline")).and_then(Value::as_bool);
        let Some(it) = self.b.p.item_mut(id) else { return };
        it.custom_label_color = meta.and_then(|m| m.get("custom_label_color")).and_then(Value::as_str).and_then(filmcraft_project::HexColor::parse);
        if let Some(l) = label {
            it.label = l;
        }
        if let Some(m) = it.as_media_mut() {
            match info {
                Some(i) => m.info = i,
                None => {
                    // A clip on the other kind of track proves the media has that stream too.
                    let _ = kind;
                }
            }
            if let Some(o) = offline {
                m.offline = o;
            }
        }
    }

    fn markers(&mut self, v: &Value) -> Vec<Marker> {
        let mut out = Vec::new();
        for m in v.get("markers").and_then(Value::as_array).into_iter().flatten() {
            let (mut start, mut duration) = m.get("marked_range").and_then(read_tr).unwrap_or((Tick::ZERO, Tick::ZERO));
            let meta = fc(m);
            if let Some(s) = meta.and_then(|x| x.get("start_ticks")).and_then(Value::as_i64)
                && (Tick(s) - start).abs() < Tick(TICKS_PER_SECOND / 1000)
            {
                start = Tick(s);
            }
            if let Some(d) = meta.and_then(|x| x.get("duration_ticks")).and_then(Value::as_i64)
                && (Tick(d) - duration).abs() < Tick(TICKS_PER_SECOND / 1000)
            {
                duration = Tick(d);
            }
            let color = meta
                .and_then(|x| x.get("label"))
                .and_then(|l| serde_json::from_value::<Label>(l.clone()).ok())
                .unwrap_or_else(|| color_label(m.get("color").and_then(Value::as_str).unwrap_or("")));
            out.push(Marker {
                author: meta.and_then(|x| x.get("author")).and_then(Value::as_str).unwrap_or("").to_string(),
                custom_color: meta.and_then(|x| x.get("custom_color")).and_then(|c| c.as_str()).and_then(filmcraft_project::HexColor::parse),
                id: MarkerId(self.b.alloc()),
                start,
                duration,
                name: name_of(m),
                comment: m.get("comment").and_then(Value::as_str).unwrap_or("").to_string(),
                kind: meta.and_then(|x| x.get("kind")).and_then(|k| serde_json::from_value::<MarkerKind>(k.clone()).ok()).unwrap_or_default(),
                color,
            });
        }
        out.sort_by_key(|m| m.start);
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rational_time() {
        for r in FrameRate::COMMON {
            for f in [0i64, 1, 1799, 86_400, 10_000_001] {
                let t = r.tick_of(f);
                assert_eq!(read_rt(&rt(t, r)), Some(t), "{r} {f}");
            }
        }
        // 23.976 written by other tools as a rounded float
        let v = json!({"OTIO_SCHEMA": "RationalTime.1", "rate": 23.976, "value": 48.0});
        assert_eq!(read_rt(&v), Some(FrameRate::FPS_23_976.tick_of(48)));
        // sub-frame audio time survives
        let t = Tick(TICKS_PER_SECOND / 48_000 * 12_345);
        let back = read_rt(&rt(t, FrameRate::FPS_24)).unwrap();
        assert!((back - t).abs() <= Tick(1));
    }
}

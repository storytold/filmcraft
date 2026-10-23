//! Development bridge for Avid cuts, filler, channel combiners, rate adapters and bounded effects.
//! Reads only the catalog contract and native project model; no binary decoding or I/O.
use std::collections::{BTreeMap, BTreeSet};

use filmcraft_geom::Vec2;
use filmcraft_project::{
    AudioChannels, BinId, ItemId, Mask, MaskPath, MaskVertex, MediaRef, Project, SequenceSettings, TrackKind,
    effect::EffectInstance,
    keyframe::{Keyframe, Param, ParamValue},
};
use filmcraft_time::{FrameRate, TICKS_PER_SECOND, Tick, TimeRange};
use serde::Serialize;
use serde_json::{Value, json};

use crate::{Error, Result, avid_catalog};

pub const SEQUENCE_KEY: &str = "filmcraft:avid-sequence";
const MAX_NODES: usize = 100_000;
const MAX_SEQUENCE_TRACKS: usize = 128;

#[derive(Debug, Serialize)]
pub struct Plan {
    sequences: Vec<SequencePlan>,
    rejected: Vec<String>,
    pub warnings: Vec<String>,
    bin: BinId,
}
#[derive(Debug, Serialize)]
struct SequencePlan {
    name: String,
    settings: SequenceSettings,
    start_timecode: i64,
    duration: Tick,
    tracks: Vec<TrackPlan>,
    evidence: Value,
    warnings: Vec<String>,
}
#[derive(Debug, Serialize)]
struct TrackPlan {
    kind: TrackKind,
    number: usize,
    stereo: bool,
    muted: bool,
    solo: bool,
    clips: Vec<ClipPlan>,
}
#[derive(Clone, Debug, PartialEq, Serialize)]
struct ClipPlan {
    item: ItemId,
    placeholder: Option<String>,
    start: Tick,
    source_in: Tick,
    duration: Tick,
    channels: Vec<u16>,
    scale_to_frame: bool,
    effects: Vec<EffectInstance>,
    /// Source offsets at every output frame, including the boundary sample. Empty for
    /// ordinary clips. The visible frames come from the saved Avid position table.
    sampled_source_offsets: Vec<Tick>,
}
fn bad(s: impl Into<String>) -> Error {
    Error::Other(format!("Avid sequence: {}", s.into()))
}
fn text<'a>(v: &'a Value, key: &str) -> Result<&'a str> {
    v[key].as_str().filter(|s| s.len() <= 4096 && !s.contains('\0')).ok_or_else(|| bad(format!("invalid {key}")))
}
fn list<'a>(v: &'a Value, key: &str, max: usize) -> Result<&'a [Value]> {
    v[key].as_array().filter(|a| a.len() <= max).map(Vec::as_slice).ok_or_else(|| bad(format!("invalid or excessive {key}")))
}
fn number(v: &Value, key: &str) -> Result<i64> {
    v[key].as_i64().filter(|n| *n >= 0).ok_or_else(|| bad(format!("invalid {key}")))
}
fn rate(v: &Value) -> Result<FrameRate> {
    let n = number(&v["rate"], "numerator")?;
    let d = number(&v["rate"], "denominator")?;
    if n == 0 || d == 0 || n > i64::from(u32::MAX) || d > i64::from(u32::MAX) || n > d * 1000 {
        return Err(bad("invalid edit rate"));
    }
    Ok(FrameRate::new(n, d))
}
fn ticks(v: &Value, key: &str) -> Result<Tick> {
    ticks_at(v, key, rate(v)?)
}
fn ticks_at(v: &Value, key: &str, r: FrameRate) -> Result<Tick> {
    let n = i128::from(number(v, key)?)
        .checked_mul(i128::from(r.den))
        .and_then(|n| n.checked_mul(i128::from(TICKS_PER_SECOND)))
        .ok_or_else(|| bad("timing overflow"))?;
    if n % i128::from(r.num) != 0 || n / i128::from(r.num) > i128::from(Tick::MAX.0) {
        return Err(bad("timing cannot be represented exactly"));
    }
    Ok(Tick((n / i128::from(r.num)) as i64))
}
fn add(a: Tick, b: Tick) -> Result<Tick> {
    a.0.checked_add(b.0).filter(|n| *n <= Tick::MAX.0).map(Tick).ok_or_else(|| bad("timing overflow"))
}
fn flag(v: &Value, key: &str) -> Result<bool> {
    match v.get(key) {
        None => Ok(false),
        Some(Value::Number(n)) if n.as_u64() == Some(0) => Ok(false),
        Some(Value::Number(n)) if n.as_u64() == Some(1) => Ok(true),
        _ => Err(bad(format!("unsupported {key}"))),
    }
}

/// The default 1920x1080 Key In AniMatte path is represented by FilmCraft's
/// intrinsic Opacity masks. Other AniMatte settings are not flattened here.
fn animatte_masks(c: &Value, source_in: Tick, rate: FrameRate, length: Tick) -> Result<Vec<Mask>> {
    let raw = &c["properties"]["animatte"];
    let states = list(raw, "states", 128)?;
    if states.len() < 2 {
        return Err(bad("AniMatte shape states are incomplete"));
    }
    for state in states {
        for shape in list(state, "shapes", 64)? {
            if !(3..=4096).contains(&list(shape, "points", 4096)?.len()) {
                return Err(bad("AniMatte shape point count is unsupported"));
            }
        }
    }
    if let Some(tracker) = raw.get("tracker_xy").filter(|v| !v.is_null())
        && tracker.as_array().is_none_or(|v| v.len() > 8192)
    {
        return Err(bad("AniMatte tracker sample count is excessive"));
    }
    let data: crate::avid_bin::AniMatteData = serde_json::from_value(raw.clone()).map_err(|_| bad("invalid AniMatte geometry"))?;
    let first = data.states.first().ok_or_else(|| bad("AniMatte has no first shape state"))?;
    let count = first.shapes.len();
    if count == 0 || data.states.iter().any(|s| s.shapes.len() != count) {
        return Err(bad("AniMatte shape count changes during effect"));
    }
    let frames = rate.frame_at(length);
    if frames < 2 || data.states.first().is_none_or(|s| s.frame != 0) || data.states.last().is_none_or(|s| i64::from(s.frame) != frames - 1) {
        return Err(bad("AniMatte state timing disagrees with clip"));
    }
    let shape_path = |shape: &crate::avid_bin::AniMatteShape| -> Result<MaskPath> {
        let point = |xy: [f64; 2], position: bool| -> Result<Vec2> {
            let x = xy[0] * (1920.0 / 720.0) + if position { 960.0 } else { 0.0 };
            let y = xy[1] * (1080.0 / 243.0) + if position { 540.0 } else { 0.0 };
            if !x.is_finite() || !y.is_finite() || x.abs() > 100_000.0 || y.abs() > 100_000.0 {
                return Err(bad("AniMatte point outside supported canvas"));
            }
            Ok(Vec2::new(x, y))
        };
        let mut vertices = Vec::with_capacity(shape.points.len());
        for p in &shape.points {
            vertices.push(MaskVertex {
                p: point(p.xy, true)?,
                t_in: if p.corner { Vec2::ZERO } else { point(p.incoming, false)? },
                t_out: if p.corner { Vec2::ZERO } else { point(p.outgoing, false)? },
            });
        }
        Ok(MaskPath { vertices, closed: true })
    };
    let mut masks = Vec::with_capacity(count);
    for index in 0..count {
        let shape = first.shapes.get(index).ok_or_else(|| bad("AniMatte shape missing"))?;
        let original = shape_path(shape)?;
        let mut mask = Mask::new(format!("Avid AniMatte {}", index + 1), original.clone());
        mask.feather.value = ParamValue::Float(0.0);
        if let Some(samples) = &data.tracker_xy {
            if count != 1 || data.states.len() != 2 || data.states[0].shapes != data.states[1].shapes || samples.len() != frames as usize {
                return Err(bad("AniMatte tracker combination is unsupported"));
            }
            let first_xy = *samples.first().ok_or_else(|| bad("AniMatte tracker is empty"))?;
            for (frame, xy) in samples.iter().enumerate() {
                let delta = Vec2::new((xy[0] - first_xy[0]) * 1920.0, (xy[1] - first_xy[1]) * 1080.0);
                if !delta.x.is_finite() || !delta.y.is_finite() || delta.x.abs() > 100_000.0 || delta.y.abs() > 100_000.0 {
                    return Err(bad("AniMatte tracker position is invalid"));
                }
                let shifted = MaskPath { vertices: original.vertices.iter().map(|v| MaskVertex { p: v.p + delta, ..*v }).collect(), closed: true };
                let time = add(source_in, rate.tick_of(frame as i64))?;
                mask.path.keyframes.push(Keyframe::new(time, ParamValue::Path(shifted)));
            }
        } else if data.states.iter().skip(1).any(|s| s.shapes[index] != *shape) {
            for state in &data.states {
                let state_shape = &state.shapes[index];
                if state_shape.id != shape.id || state_shape.points.len() != shape.points.len() {
                    return Err(bad("AniMatte animated shape identity changed"));
                }
                let time = add(source_in, rate.tick_of(i64::from(state.frame)))?;
                mask.path.keyframes.push(Keyframe::new(time, ParamValue::Path(shape_path(state_shape)?)));
            }
        }
        masks.push(mask);
    }
    Ok(masks)
}

/// Avid's native Image > Resize has a different parameter family from promoted
/// 3D Warp. This bounded path converts only static geometry; nonzero crop and
/// background controls retain their explicit unsupported result.
fn resize_effects(c: &Value, width: u32, height: u32) -> Result<Vec<EffectInstance>> {
    let params = list(&c["properties"], "numeric_parameters", 128)?;
    if params.is_empty() {
        return Err(bad("Resize has no verified static controls"));
    }
    const FAMILY: [u8; 14] = [0x56, 0x8d, 0x7e, 0x84, 0xd5, 0x11, 0x93, 0x5a, 0x50, 0xf8, 0x57, 0xc1, 0, 0];
    const KNOWN: &[u16] = &[0x8125, 0x8126, 0x8129, 0x812a, 0x812b, 0x812e, 0x812f, 0x8130, 0x8131, 0x8147, 0x8148, 0x8149, 0x814d, 0x8153];
    let mut values = BTreeMap::<u16, f64>::new();
    for p in params {
        let id = list(p, "id", 16)?;
        if id.len() != 16 || id[2..].iter().zip(FAMILY).any(|(x, y)| x.as_u64() != Some(u64::from(y))) {
            return Err(bad("unsupported Resize parameter family"));
        }
        let key = u16::from_le_bytes([
            u8::try_from(id[0].as_u64().ok_or_else(|| bad("invalid Resize parameter id"))?).map_err(|_| bad("invalid Resize parameter id"))?,
            u8::try_from(id[1].as_u64().ok_or_else(|| bad("invalid Resize parameter id"))?).map_err(|_| bad("invalid Resize parameter id"))?,
        ]);
        if !KNOWN.contains(&key) {
            return Err(bad(format!("Resize control {key:04x} is not translated")));
        }
        let value = p["value"].as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad("invalid Resize value"))?;
        if values.insert(key, value).is_some() {
            return Err(bad("duplicate Resize control"));
        }
    }
    let v = |id, default| values.get(&id).copied().unwrap_or(default);
    if ![0x8125, 0x8126, 0x8129, 0x812a].iter().all(|id| values.contains_key(id)) {
        return Err(bad("Resize geometry controls are incomplete"));
    }
    for id in [0x812e, 0x812f, 0x8130, 0x8131, 0x8147, 0x8148, 0x8149, 0x814d, 0x8153] {
        if v(id, 0.0) != 0.0 {
            return Err(bad(format!("Resize control {id:04x} is not translated")));
        }
    }
    if !matches!(v(0x812b, 0.0), 0.0 | 1.0) {
        return Err(bad("unsupported Resize Fixed Aspect state"));
    }
    let x = v(0x8125, 0.0);
    let y = v(0x8126, 0.0);
    let sx = v(0x8129, 100.0);
    let sy = v(0x812a, 100.0);
    if !(-1000.0..=1000.0).contains(&x)
        || !(-1000.0..=1000.0).contains(&y)
        || !(0.0..=10000.0).contains(&sx)
        || !(0.0..=10000.0).contains(&sy)
        || (v(0x812b, 0.0) == 1.0 && (sx - sy).abs() > 1e-6)
    {
        return Err(bad("Resize geometry outside supported range"));
    }
    let mut effect = filmcraft_project::find_effect("transform").ok_or_else(|| bad("FilmCraft Transform is unavailable"))?.instance();
    // First-party 1920x1080 renders: Resize X+100 moves +192px; its Y sign is
    // opposite promoted Warp. Both scale controls use UI percent directly.
    effect.params.get_mut("position").ok_or_else(|| bad("Transform position missing"))?.value =
        ParamValue::Vec2(Vec2::new(f64::from(width) * (0.5 + x / 1000.0), f64::from(height) * (0.5 + y / 1000.0)));
    effect.params.get_mut("uniform_scale").ok_or_else(|| bad("Transform scale mode missing"))?.value = ParamValue::Bool(false);
    effect.params.get_mut("scale_width").ok_or_else(|| bad("Transform width missing"))?.value = ParamValue::Float(sx);
    effect.params.get_mut("scale_height").ok_or_else(|| bad("Transform height missing"))?.value = ParamValue::Float(sy);
    Ok(vec![effect])
}

/// The numeric controls verified against user-saved 2025.12.1 bins and Avid-rendered frames.
/// Other controls (including 3D rotation, axis, keyframes and foreground compositing) need
/// their own translation path; never flatten them to 2D.
fn warp_effects(c: &Value, width: u32, height: u32) -> Result<Vec<EffectInstance>> {
    if !c["properties"]["numeric_parameters"].is_array() {
        return Err(bad("3D Warp has animated or unsupported parameter storage"));
    }
    let params = list(&c["properties"], "numeric_parameters", 128)?;
    if params.is_empty() {
        return Err(bad("3D Warp has no verified numeric controls"));
    }
    let mut values = BTreeMap::<u16, f64>::new();
    const FAMILY: [u8; 14] = [0x56, 0x8d, 0x7e, 0x84, 0xd5, 0x11, 0x93, 0x5a, 0x50, 0xf8, 0x57, 0xc1, 0, 0];
    const KNOWN: &[u16] = &[
        0x812b, 0x81a1, 0x81a2, 0x81a4, 0x81a9, 0x81aa, 0x81ab, 0x81ac, 0x81b2, 0x81b3, 0x81b4, 0x81b7, 0x81b8, 0x81b9, 0x81ba, 0x81bc, 0x81bd, 0x81be, 0x81bf,
        0x81cb, 0x81cc, 0x81cd, 0x81ce, 0x81cf, 0x81d0, 0x81ed, 0x81fc, 0x821c, 0x828e, 0x814d, 0x8153, 0x8206, 0x8207, 0x8208, 0x8209,
    ];
    for p in params {
        let id = list(p, "id", 16)?;
        if id.len() != 16 || id[2..].iter().zip(FAMILY).any(|(x, y)| x.as_u64() != Some(u64::from(y))) {
            return Err(bad("unsupported 3D Warp parameter family"));
        }
        let key = u16::from_le_bytes([
            u8::try_from(id[0].as_u64().ok_or_else(|| bad("invalid Warp parameter id"))?).map_err(|_| bad("invalid Warp parameter id"))?,
            u8::try_from(id[1].as_u64().ok_or_else(|| bad("invalid Warp parameter id"))?).map_err(|_| bad("invalid Warp parameter id"))?,
        ]);
        if !KNOWN.contains(&key) {
            return Err(bad(format!("3D Warp control {key:04x} is not translated")));
        }
        let value = p["value"].as_f64().filter(|v| v.is_finite()).ok_or_else(|| bad("invalid Warp value"))?;
        if values.insert(key, value).is_some() {
            return Err(bad("duplicate 3D Warp control"));
        }
    }
    let v = |id, default| values.get(&id).copied().unwrap_or(default);
    let check = |id, default| -> Result<()> {
        if (v(id, default) - default).abs() > 1e-6 {
            return Err(bad(format!("3D Warp control {id:04x} is not translated")));
        }
        Ok(())
    };
    for (id, default) in [
        (0x81a1, 0.59),
        (0x81a2, 0.0),
        (0x81a4, 0.0),
        (0x81d0, 0.0),
        (0x81ed, 1.0),
        (0x81fc, 255.0),
        (0x821c, 0.0),
        (0x828e, 0.0),
        (0x814d, 0.0),
        (0x8153, 0.0),
        (0x8206, 0.0),
        (0x8207, 0.0),
        (0x8208, 0.0),
        (0x8209, 0.0),
    ] {
        check(id, default)?;
    }
    for id in [0x812b, 0x81a9, 0x81b2, 0x81b7, 0x81bc, 0x81cb] {
        if !matches!(v(id, 0.0), 0.0 | 1.0) {
            return Err(bad("unsupported 3D Warp enable state"));
        }
    }
    if v(0x81a9, 0.0) == 1.0 {
        check(0x81ac, 0.0)?; // Z position changes perspective/size, not 2D translation.
    }
    if v(0x81b7, 0.0) == 1.0 {
        check(0x81b8, 0.0)?;
        check(0x81b9, 0.0)?;
    }
    if v(0x81bc, 0.0) == 1.0 {
        for id in [0x81bd, 0x81be, 0x81bf] {
            check(id, 0.0)?;
        }
    }
    let x = if v(0x81a9, 0.0) == 1.0 { v(0x81aa, 0.0) } else { 0.0 };
    let y = if v(0x81a9, 0.0) == 1.0 { v(0x81ab, 0.0) } else { 0.0 };
    let sx = if v(0x81b2, 0.0) == 1.0 { v(0x81b3, 100.0) } else { 100.0 };
    let sy = if v(0x81b2, 0.0) == 1.0 { v(0x81b4, 100.0) } else { 100.0 };
    let rotation = if v(0x81b7, 0.0) == 1.0 { v(0x81ba, 0.0) } else { 0.0 };
    if !(-1000.0..=1000.0).contains(&x)
        || !(-1000.0..=1000.0).contains(&y)
        || !(0.0..=10000.0).contains(&sx)
        || !(0.0..=10000.0).contains(&sy)
        || !(-3600.0..=3600.0).contains(&rotation)
    {
        return Err(bad("3D Warp value outside supported 2D range"));
    }
    let mut effects = Vec::with_capacity(2);
    if v(0x81cb, 0.0) == 1.0 {
        // Avid stores the four crop edges in its -1000..1000 image coordinates.
        // The first-party 1920x1080 export with (-799,699,839,-759) has visible
        // boundaries at x≈193/1631 and y≈87/950. Crop precedes rotation/scale:
        // the later combined frames rotate or scale the cropped rectangle.
        let left = v(0x81cc, -1000.0);
        let right = v(0x81cd, 1000.0);
        let top = v(0x81ce, 1000.0);
        let bottom = v(0x81cf, -1000.0);
        if [left, right, top, bottom].iter().any(|n| !(-1000.0..=1000.0).contains(n)) || left >= right || bottom >= top {
            return Err(bad("3D Warp Crop edges outside supported range"));
        }
        if (left, right, top, bottom) != (-1000.0, 1000.0, 1000.0, -1000.0) {
            let mut crop = filmcraft_project::find_effect("crop").ok_or_else(|| bad("FilmCraft Crop is unavailable"))?.instance();
            for (name, percent) in
                [("left", (left + 1000.0) / 20.0), ("right", (1000.0 - right) / 20.0), ("top", (1000.0 - top) / 20.0), ("bottom", (bottom + 1000.0) / 20.0)]
            {
                crop.params.get_mut(name).ok_or_else(|| bad("FilmCraft Crop edge missing"))?.value = ParamValue::Float(percent);
            }
            effects.push(crop);
        }
    }
    let mut effect = filmcraft_project::find_effect("transform").ok_or_else(|| bad("FilmCraft Transform is unavailable"))?.instance();
    effect.params.get_mut("position").ok_or_else(|| bad("Transform position missing"))?.value =
        // In the 1920x1080 Avid render, +100 X moves the picture +192 px and
        // -100 Y moves it +108 px: the UI unit is one tenth of one percent.
        ParamValue::Vec2(Vec2::new(f64::from(width) * (0.5 + x / 1000.0), f64::from(height) * (0.5 - y / 1000.0)));
    effect.params.get_mut("uniform_scale").ok_or_else(|| bad("Transform scale mode missing"))?.value = ParamValue::Bool(false);
    effect.params.get_mut("scale_width").ok_or_else(|| bad("Transform width missing"))?.value = ParamValue::Float(sx);
    effect.params.get_mut("scale_height").ok_or_else(|| bad("Transform height missing"))?.value = ParamValue::Float(sy);
    effect.params.get_mut("rotation").ok_or_else(|| bad("Transform rotation missing"))?.value = ParamValue::Float(rotation);
    effects.push(effect);
    Ok(effects)
}

/// Resolve only masters imported for the explicitly selected bin: its own members and the
/// referenced sources imported alongside them. Repeated imports in other bins cannot accidentally
/// authorize a source; aliases within this bin are ambiguous.
fn sources(p: &Project, bin: BinId) -> Result<(String, BTreeMap<[u8; 32], ItemId>)> {
    avid_catalog::validate_project(p, Some(bin))?;
    let mut uid = None;
    let mut ids = BTreeMap::new();
    // Subclips are sources too: a sequence can cut from one, and its identity is the subclip's.
    let items = avid_catalog::imported_items(p, bin)?;
    for id in items.placed.into_iter().chain(items.referenced).chain(items.subclips) {
        let it = p.item(id).ok_or_else(|| bad("missing bin item"))?;
        if it.as_media().is_none() && !matches!(it.kind, filmcraft_project::ItemKind::Subclip { .. }) {
            continue;
        }
        let Some(raw) = it.metadata.get(avid_catalog::SOURCE_KEY) else { continue };
        if raw.len() > 16 * 1024 * 1024 {
            return Err(bad("excessive source evidence"));
        }
        let e: Value = serde_json::from_str(raw).map_err(|e| bad(e.to_string()))?;
        if e["version"] != 1 {
            return Err(bad("unsupported source evidence version"));
        }
        let u = text(&e, "bin_uid")?.to_owned();
        if uid.as_ref().is_some_and(|old| old != &u) {
            return Err(bad("mixed source bin identities"));
        }
        uid = Some(u);
        if ids.insert(avid_catalog::full_identity(&e["clip"]["id"])?, id).is_some() {
            return Err(bad("ambiguous master identity in selected bin"));
        }
    }
    Ok((uid.ok_or_else(|| bad("selected bin has no imported masters"))?, ids))
}

mod formats;
mod rates;
mod recovery;
use rates::automatic_source_rate_at;
pub(crate) use rates::offline_rate_evidence;

struct Walk<'a> {
    project: &'a Project,
    sources: &'a BTreeMap<[u8; 32], ItemId>,
    rate: FrameRate,
    clock: formats::Clock,
    kind: TrackKind,
    nodes: &'a mut usize,
    warnings: &'a mut Vec<String>,
    best_effort: bool,
    audio_channels: usize,
    frame_width: u32,
    frame_height: u32,
}
impl Walk<'_> {
    fn phase_warning(&mut self, c: &Value, start: Tick) {
        if c["properties"]["phase_offset"] != 0 {
            self.warnings.push(format!(
                "Frame {}: Avid adapter phase offset {} is not translated. Using the recorded source-frame start at 100% playback; check trimmed picture/audio alignment against Avid. Original adapter is retained.",
                self.rate.frame_at(start), c["properties"]["phase_offset"]
            ));
        }
    }
    fn audio_adapter(&mut self, c: &Value, start: Tick, length: Tick, depth: usize) -> Result<Vec<ClipPlan>> {
        let (leaf, source_rate) = automatic_source_rate_at(c, depth, self.nodes, self.clock.rate(c)?, self.best_effort)?;
        let id = avid_catalog::full_identity(&leaf["source_id"])?;
        let item = *self.sources.get(&id).ok_or_else(|| bad("audio adapter source master missing"))?;
        let m = self.project.item(item).and_then(|i| i.as_media()).ok_or_else(|| bad("audio adapter source is not media"))?;
        let MediaRef::Components { channels, .. } = &m.media else {
            return Err(bad("audio adapter source must be bound to managed components"));
        };
        let track = u32::try_from(number(leaf, "source_track")?).map_err(|_| bad("source track overflow"))?;
        let mut matches = channels.iter().enumerate().filter(|(_, ch)| ch.track == track);
        let (index, channel) = matches.next().ok_or_else(|| bad("audio adapter source track missing"))?;
        if matches.next().is_some() {
            return Err(bad("ambiguous audio adapter source track"));
        }
        let source_in = ticks_at(leaf, "start", source_rate)?;
        let source_length = ticks_at(leaf, "length", source_rate)?;
        if add(source_in, source_length)? > channel.length || add(source_in, length)? > m.info.duration {
            return Err(bad("audio adapter range exceeds source media"));
        }
        self.phase_warning(c, start);
        // Avid's automatic ratio changes edit units, not PCM playback speed. Samples
        // retain wall-clock timing; only the timeline endpoint is rounded to its frames.
        self.warnings.push(format!(
            "Frame {}: automatic audio rate adapter mapped to 100% playback with original source timing; Avid audio processing may differ.",
            self.rate.frame_at(start)
        ));
        Ok(vec![ClipPlan {
            item,
            start,
            source_in,
            duration: length,
            channels: vec![u16::try_from(index).map_err(|_| bad("source channel overflow"))?],
            scale_to_frame: false,
            effects: Vec::new(),
            sampled_source_offsets: Vec::new(),
            placeholder: None,
        }])
    }
    /// Accept only constant automatic rate adaptation, not creative speed effects. Avid's
    /// output/source rate ratio represents ordinary real-time playback in FilmCraft's tick model.
    fn motion_adapter(&mut self, c: &Value, start: Tick, length: Tick, depth: usize) -> Result<Vec<ClipPlan>> {
        let (leaf, derived_rate) = automatic_source_rate_at(c, depth, self.nodes, self.clock.rate(c)?, self.best_effort)?;
        let id = avid_catalog::full_identity(&leaf["source_id"])?;
        let item = *self.sources.get(&id).ok_or_else(|| bad("motion source master missing"))?;
        let m = self.project.item(item).and_then(|i| i.as_media()).ok_or_else(|| bad("motion source is not media"))?;
        let MediaRef::Components { picture: Some(picture), .. } = &m.media else {
            return Err(bad("motion source must be bound to verified picture"));
        };
        let video = m.info.video.as_ref().ok_or_else(|| bad("motion source video metadata missing"))?;
        let src_rate = video.frame_rate;
        if src_rate != derived_rate {
            return Err(bad("adapter and picture rates disagree"));
        }
        // Validate bounds/rational arithmetic before using native project metadata.
        let exact_rate = json!({"numerator":src_rate.num,"denominator":src_rate.den});
        rate(&json!({"rate":exact_rate}))?;
        if !avid_catalog::picture_rate_matches(leaf, src_rate) || number(leaf, "source_track")? != i64::from(picture.track) {
            return Err(bad("motion source track or recorded rate disagrees with verified picture"));
        }
        let exact = |field: &str| ticks(&json!({(field):leaf[field],"rate":exact_rate}), field);
        let source_in = exact("start")?;
        let source_length = exact("length")?;
        // The child must cover exactly the output interval rounded up to whole source frames.
        if source_length < length
            || source_length.0 - length.0 >= src_rate.frame_duration().0
            || add(source_in, source_length)? > picture.length
            || add(source_in, length)? > m.info.duration
        {
            return Err(bad("motion input does not cover the declared output range"));
        }
        self.phase_warning(c, start);
        self.warnings.push(format!("Frame {}: {}/{} fps source adapted to {}/{} fps with FilmCraft Frame Sampling at 100% playback speed; Avid interpolation/frame selection may differ.",self.rate.frame_at(start),src_rate.num,src_rate.den,self.rate.num,self.rate.den));
        Ok(vec![ClipPlan {
            item,
            start,
            source_in,
            duration: length,
            channels: Vec::new(),
            scale_to_frame: false,
            effects: Vec::new(),
            sampled_source_offsets: Vec::new(),
            placeholder: None,
        }])
    }

    /// Avid saves the output-to-source position function at half-frame intervals. For
    /// Frame Sampling, the integer-time samples directly determine displayed frames;
    /// evaluating the speed-curve handles ourselves would disagree at some frames.
    fn sampled_timewarp(&mut self, c: &Value, start: Tick, length: Tick, depth: usize) -> Result<Vec<ClipPlan>> {
        let p = &c["properties"];
        if p["mc_mode"] != 0 || p["phase_offset"] != 0 || p["offset_adjust"].as_f64() != Some(0.0) || p["new_source_calculation"] != true {
            return Err(bad("unsupported Timewarp routing or phase"));
        }
        let ratio = list(p, "speed_ratio", 2)?;
        if ratio.len() != 2 || ratio.iter().any(|v| v.as_i64().is_none_or(|n| n <= 0 || n > i64::from(u32::MAX))) {
            return Err(bad("invalid Timewarp ratio"));
        }
        let frames = usize::try_from(number(c, "length")?).map_err(|_| bad("Timewarp duration overflow"))?;
        let samples = list(p, "timewarp_positions", 8193)?;
        if frames == 0 || frames > 4096 || samples.len() != frames * 2 + 1 || length != self.rate.tick_of(frames as i64) {
            return Err(bad("unsupported Timewarp sample count or duration"));
        }
        let inputs = list(c, "input_tracks", 1)?;
        let input = inputs.first().filter(|i| i["index"] == 1).ok_or_else(|| bad("unsupported Timewarp input"))?;
        let leaf = rates::source_leaf(&input["component"], 1, depth + 1, self.nodes)?;
        if rate(leaf)? != self.rate {
            return Err(bad("mixed-rate Timewarp source is unsupported"));
        }
        let id = avid_catalog::full_identity(&leaf["source_id"])?;
        let item = *self.sources.get(&id).ok_or_else(|| bad("Timewarp source master missing"))?;
        let m = self.project.item(item).and_then(|i| i.as_media()).ok_or_else(|| bad("Timewarp source is not media"))?;
        let MediaRef::Components { picture: Some(picture), .. } = &m.media else {
            return Err(bad("Timewarp source must be bound to verified picture"));
        };
        if m.info.video.as_ref().is_none_or(|v| v.frame_rate != self.rate) || number(leaf, "source_track")? != i64::from(picture.track) {
            return Err(bad("Timewarp source rate or track disagrees with picture"));
        }
        let source_in = ticks(leaf, "start")?;
        let mut previous = 0.0;
        let mut offsets = Vec::with_capacity(frames + 1);
        for (i, sample) in samples.iter().enumerate() {
            let value = sample.as_f64().filter(|v| v.is_finite() && *v >= 0.0 && *v <= 1_000_000.0).ok_or_else(|| bad("invalid Timewarp source position"))?;
            if (i == 0 && value.abs() > 1e-9) || value + 1e-9 < previous {
                return Err(bad("non-monotone Timewarp source positions"));
            }
            previous = value;
            if i % 2 == 0 {
                let source_frame = (value + 1e-9).floor() as i64;
                offsets.push(self.rate.tick_of(source_frame));
            }
        }
        let furthest = offsets.last().copied().ok_or_else(|| bad("Timewarp position table is empty"))?;
        if add(source_in, furthest)? > picture.length || add(source_in, furthest)? > m.info.duration {
            return Err(bad("Timewarp position exceeds source media"));
        }
        self.warnings.push(format!(
            "Frame {}: Avid Timewarp imported from saved source-position samples with FilmCraft Frame Sampling; speed handles and optical processing are not translated.",
            self.rate.frame_at(start)
        ));
        Ok(vec![ClipPlan {
            item,
            start,
            source_in,
            duration: length,
            channels: Vec::new(),
            scale_to_frame: false,
            effects: Vec::new(),
            sampled_source_offsets: offsets,
            placeholder: None,
        }])
    }

    fn checked_node(&mut self, c: &Value, start: Tick, depth: usize) -> Result<Vec<ClipPlan>> {
        *self.nodes = self.nodes.checked_add(1).ok_or_else(|| bad("node count overflow"))?;
        if *self.nodes > MAX_NODES || depth >= 16 {
            return Err(bad("component graph exceeds bounds"));
        }
        if c.get("truncated").is_some_and(|v| v != &Value::Bool(false)) {
            return Err(bad("truncated component"));
        }
        if self.clock.rate(c)? != self.rate {
            return Err(bad("mixed-rate component is unsupported"));
        }
        let expected = if self.kind == TrackKind::Video { 1 } else { 2 };
        if c["media_kind"] != expected {
            return Err(bad("component media kind disagrees with track"));
        }
        let length = self.clock.ticks(c, "length")?;
        add(start, length)?;
        let class = text(c, "class")?;
        let combiner = class == "TKFX" && c.pointer("/properties/effect_id").and_then(Value::as_str) == Some("EFF2_AUDIO_CHANNEL_COMBINER");
        let spatial = class == "TKFX" && c.pointer("/properties/effect_id").and_then(Value::as_str) == Some("INT_EFF_SPATIAL_ADAPTER");
        let flip = class == "TKFX"
            && self.kind == TrackKind::Video
            && matches!(c.pointer("/properties/effect_id").and_then(Value::as_str), Some("EFF2_BLEND_FLIP_VERT" | "EFF2_BLEND_FLIP_BOTH"));
        let warp = class == "TKFX"
            && self.kind == TrackKind::Video
            && c.pointer("/properties/effect_id").and_then(Value::as_str) == Some("EFF2_SBLEND")
            && c.pointer("/properties/attributes/_EFFECT_PLUGIN_NAME").and_then(Value::as_str) == Some("3DWarp");
        let resize =
            class == "TKFX" && self.kind == TrackKind::Video && c.pointer("/properties/effect_id").and_then(Value::as_str) == Some("EFF2_BLEND_RESIZE");
        let animatte = class == "TKFX" && self.kind == TrackKind::Video && c.pointer("/properties/effect_id").and_then(Value::as_str) == Some("EFF_ANIMATTE");
        let motion = class == "SPED" && c.pointer("/properties/effect_id").and_then(Value::as_str) == Some("EFF_TIMEWARP.MOTION_CTL");
        let timewarp =
            class == "SPED" && self.kind == TrackKind::Video && c.pointer("/properties/effect_id").and_then(Value::as_str) == Some("EFF_ADV_MOTION_CTL");
        let audio_adapter = class == "SPED" && c.pointer("/properties/effect_id").and_then(Value::as_str) == Some("EFF_TIMEWARP.AUDIO_TIME_WARP");
        if c.get("unsupported").is_some()
            && !((combiner || spatial || flip || warp || resize || animatte || motion || timewarp || audio_adapter)
                && c["unsupported"] == "component_semantics")
        {
            return Err(bad(format!(
                "unsupported {class} at frame {} ({})",
                self.rate.frame_at(start),
                c.pointer("/properties/effect_id").and_then(Value::as_str).unwrap_or("unknown semantics")
            )));
        }
        match class {
            "FILL" => Ok(Vec::new()),
            "SEQU" => {
                let mut pos = start;
                let mut clips = Vec::new();
                for child in list(c, "components", MAX_NODES)? {
                    clips.extend(self.node(child, pos, depth + 1)?);
                    pos = add(pos, self.clock.ticks(child, "length")?)?;
                }
                if pos != add(start, length)? {
                    return Err(bad("sequence child lengths disagree with parent"));
                }
                Ok(clips)
            }
            "SCLP" => {
                if length == Tick::ZERO {
                    return Err(bad("zero-length source clip"));
                }
                let id = avid_catalog::full_identity(&c["source_id"])?;
                let item = *self.sources.get(&id).ok_or_else(|| bad("source master is missing from selected bin"))?;
                // A source may be a subclip. Resolve to the media it restricts, keeping its range:
                // clips of a subclip are expressed in the parent's media time.
                let (master_item, m, span) =
                    self.project.resolve_media(item).ok_or_else(|| bad("source is not media, or its subclip chain does not reach any"))?;
                let MediaRef::Components { channels, picture } = &m.media else {
                    return Err(bad(if span.is_some() {
                        "the master this subclip restricts must be bound to managed components first"
                    } else {
                        "source master must be bound to managed components first"
                    }));
                };
                // Saved project ranges are untrusted too. Resolve_media finds the parent, but
                // does not validate range bounds; never let an invalid span authorize an edit.
                let (source_in, limit) = match span {
                    Some(range) => {
                        if range.start < Tick::ZERO || range.duration <= Tick::ZERO {
                            return Err(bad("invalid saved subclip range"));
                        }
                        let end = add(range.start, range.duration)?;
                        if end > m.info.duration {
                            return Err(bad("saved subclip range exceeds master duration"));
                        }
                        (add(range.start, self.clock.ticks(c, "start")?)?, end)
                    }
                    None => (self.clock.ticks(c, "start")?, m.info.duration),
                };
                // A cut from a subclip stays inside that subclip, not merely inside its master.
                if add(source_in, length)? > limit {
                    return Err(bad(if span.is_some() { "source range exceeds the subclip" } else { "source range exceeds master duration" }));
                }
                let mut track = u32::try_from(number(c, "source_track")?).map_err(|_| bad("source track overflow"))?;
                if span.is_some() {
                    let raw = self
                        .project
                        .item(item)
                        .and_then(|i| i.metadata.get(avid_catalog::SOURCE_KEY))
                        .filter(|s| s.len() <= 16 * 1024 * 1024)
                        .ok_or_else(|| bad("subclip source evidence missing or excessive"))?;
                    let evidence: Value = serde_json::from_str(raw).map_err(|e| bad(e.to_string()))?;
                    let (parent, parent_track) = avid_catalog::subclip_track(&evidence["clip"], expected, track)?;
                    if self.sources.get(&parent) != Some(&master_item) {
                        return Err(bad("subclip routing disagrees with its parent media"));
                    }
                    track = parent_track;
                }
                let mapping = if self.kind == TrackKind::Audio {
                    let indexes: Vec<_> = channels.iter().enumerate().filter(|(_, ch)| ch.track == track).map(|(i, _)| i).collect();
                    if indexes.len() != 1 {
                        return Err(bad("missing or ambiguous source audio track"));
                    }
                    vec![u16::try_from(indexes[0]).map_err(|_| bad("source channel overflow"))?]
                } else {
                    if picture.as_ref().is_none_or(|p| p.track != track) || m.info.video.as_ref().is_none_or(|v| v.frame_rate != self.rate) {
                        return Err(bad("picture source track or rate mismatch"));
                    }
                    Vec::new()
                };
                Ok(vec![ClipPlan {
                    item,
                    start,
                    source_in,
                    duration: length,
                    channels: mapping,
                    scale_to_frame: false,
                    effects: Vec::new(),
                    sampled_source_offsets: Vec::new(),
                    placeholder: None,
                }])
            }
            "TKFX" if flip => {
                let inputs = list(c, "input_tracks", 1)?;
                let input = inputs.first().ok_or_else(|| bad("flip needs one input"))?;
                if input["index"] != 1 || self.clock.ticks(&input["component"], "length")? != length || c["properties"]["mc_mode"] != 0 {
                    return Err(bad("flip input duration/index/mode mismatch"));
                }
                let mut clips = self.node(&input["component"], start, depth + 1)?;
                if clips.len() != 1 || clips[0].placeholder.is_some() || clips[0].start != start || clips[0].duration != length {
                    return Err(bad("flip input must be one supported clip"));
                }
                let clip = &mut clips[0];
                clip.effects.push(filmcraft_project::find_effect("vertical_flip").ok_or_else(|| bad("vertical flip unavailable"))?.instance());
                if c["properties"]["effect_id"] == "EFF2_BLEND_FLIP_BOTH" {
                    clip.effects.push(filmcraft_project::find_effect("horizontal_flip").ok_or_else(|| bad("horizontal flip unavailable"))?.instance());
                }
                Ok(clips)
            }
            "TKFX" if resize => {
                let inputs = list(c, "input_tracks", 1)?;
                let input = inputs.first().ok_or_else(|| bad("Resize needs one input"))?;
                if inputs.len() != 1 || input["index"] != 1 || self.clock.ticks(&input["component"], "length")? != length || c["properties"]["mc_mode"] != 0 {
                    return Err(bad("unsupported Resize input routing"));
                }
                let effects = resize_effects(c, self.frame_width, self.frame_height)?;
                let mut clips = self.node(&input["component"], start, depth + 1)?;
                if clips.len() != 1 || clips[0].placeholder.is_some() || clips[0].start != start || clips[0].duration != length {
                    return Err(bad("Resize input must be one supported clip"));
                }
                clips[0].effects.extend(effects);
                self.warnings.push(format!(
                    "Frame {}: static Avid Resize Position and Scaling mapped to FilmCraft Transform; exposed areas use FilmCraft transparency, and other Resize controls remain unsupported.",
                    self.rate.frame_at(start)
                ));
                Ok(clips)
            }
            "TKFX" if warp => {
                let inputs = list(c, "input_tracks", 2)?;
                if inputs.len() != 2 || inputs[0]["index"] != 1 || inputs[1]["index"] != 2 || c["properties"]["mc_mode"] != 0 {
                    return Err(bad("unsupported 3D Warp input routing"));
                }
                if self.clock.ticks(&inputs[0]["component"], "length")? != length
                    || self.clock.ticks(&inputs[1]["component"], "length")? != length
                    || !self.node(&inputs[0]["component"], start, depth + 1)?.is_empty()
                {
                    return Err(bad("3D Warp background must be filler"));
                }
                let effects = warp_effects(c, self.frame_width, self.frame_height)?;
                let mut clips = self.node(&inputs[1]["component"], start, depth + 1)?;
                if clips.len() != 1 || clips[0].placeholder.is_some() || clips[0].start != start || clips[0].duration != length {
                    return Err(bad("3D Warp foreground must be one supported clip"));
                }
                clips[0].effects.extend(effects);
                self.warnings.push(format!(
                    "Frame {}: static Avid 3D Warp Crop, Position, Scaling and Z Rotation mapped to FilmCraft effects; Avid sampling/edge treatment may differ.",
                    self.rate.frame_at(start)
                ));
                Ok(clips)
            }
            "TKFX" if animatte => {
                if (self.frame_width, self.frame_height) != (1920, 1080) {
                    return Err(bad("AniMatte canvas calibration is limited to 1920x1080"));
                }
                let inputs = list(c, "input_tracks", 2)?;
                if inputs.len() != 2 || inputs[0]["index"] != 1 || inputs[1]["index"] != 2 || c["properties"]["mc_mode"] != 0 {
                    return Err(bad("unsupported AniMatte input routing"));
                }
                if self.clock.ticks(&inputs[0]["component"], "length")? != length
                    || self.clock.ticks(&inputs[1]["component"], "length")? != length
                    || !self.node(&inputs[0]["component"], start, depth + 1)?.is_empty()
                {
                    return Err(bad("AniMatte background must be filler"));
                }
                let mut clips = self.node(&inputs[1]["component"], start, depth + 1)?;
                if clips.len() != 1 || clips[0].placeholder.is_some() || clips[0].start != start || clips[0].duration != length {
                    return Err(bad("AniMatte foreground must be one supported clip"));
                }
                if self.project.source_size(clips[0].item) != Some((1920, 1080)) {
                    return Err(bad("AniMatte source must match the calibrated canvas"));
                }
                if clips[0].effects.iter().any(|e| e.effect == "opacity" && !e.masks.is_empty()) {
                    return Err(bad("nested AniMatte composition is unsupported"));
                }
                let mut opacity = filmcraft_project::find_effect("opacity").ok_or_else(|| bad("FilmCraft Opacity is unavailable"))?.instance();
                opacity.masks = animatte_masks(c, clips[0].source_in, self.rate, length)?;
                clips[0].effects.push(opacity);
                self.warnings.push(format!(
                    "Frame {}: default Key In Avid AniMatte geometry mapped to FilmCraft Opacity masks; one-point tracking and linear shape motion are supported, while other AniMatte settings require separate validation.",
                    self.rate.frame_at(start)
                ));
                Ok(clips)
            }
            "TKFX" if spatial && self.kind == TrackKind::Video => {
                let inputs = list(c, "input_tracks", 1)?;
                let input = inputs.first().ok_or_else(|| bad("spatial adapter needs one input"))?;
                if input["index"] != 1 || self.clock.ticks(&input["component"], "length")? != length {
                    return Err(bad("spatial adapter input duration/index mismatch"));
                }
                let mut clips = self.node(&input["component"], start, depth + 1)?;
                for clip in &mut clips {
                    clip.scale_to_frame = true;
                }
                self.warnings.push(format!(
                    "Frame {}: Avid spatial adapter mapped to FilmCraft Scale to Frame Size; Avid crop/reformat treatment may differ.",
                    self.rate.frame_at(start)
                ));
                Ok(clips)
            }
            "SPED" if motion && self.kind == TrackKind::Video => self.motion_adapter(c, start, length, depth),
            "SPED" if timewarp => self.sampled_timewarp(c, start, length, depth),
            "SPED" if audio_adapter && self.kind == TrackKind::Audio => self.audio_adapter(c, start, length, depth),
            "TKFX" if combiner && self.kind == TrackKind::Audio => {
                let inputs = list(c, "input_tracks", 2)?;
                if inputs.len() != 2 {
                    return Err(bad("stereo combiner needs two inputs"));
                }
                let mut pair = Vec::new();
                for input in inputs {
                    let mut clips = self.node(&input["component"], start, depth + 1)?;
                    if clips.len() != 1 {
                        return Err(bad("combiner input must be one source"));
                    }
                    pair.push(clips.remove(0));
                }
                let mut left = pair.remove(0);
                let right = pair.remove(0);
                if left.placeholder.is_some()
                    || right.placeholder.is_some()
                    || left.item != right.item
                    || left.source_in != right.source_in
                    || left.start != start
                    || right.start != start
                    || left.duration != length
                    || right.duration != length
                    || left.channels.len() != 1
                    || right.channels.len() != 1
                    || left.channels == right.channels
                {
                    return Err(bad("combiner inputs disagree on source, range or channels"));
                }
                left.channels.extend(right.channels);
                Ok(vec![left])
            }
            _ => Err(bad(format!("unsupported {class} at frame {}", self.rate.frame_at(start)))),
        }
    }
}

fn sequence(p: &Project, ids: &BTreeMap<[u8; 32], ItemId>, m: &Value, nodes: &mut usize, best_effort: bool) -> Result<SequencePlan> {
    if m.get("unsupported").is_some() || m.get("truncated").is_some_and(|v| v != &Value::Bool(false)) {
        return Err(bad("incomplete composition"));
    }
    if m["live"] != true {
        return Err(bad("inactive composition"));
    }
    avid_catalog::full_identity(&m["id"])?;
    let name = text(m, "name")?.to_owned();
    let format = formats::resolve(m, best_effort)?;
    let r = format.clock.actual;
    let clock = format.clock;
    let mut warnings = format.warnings;
    let mut settings = format.settings;
    let duration = clock.ticks(m, "length")?;
    let mut tracks = Vec::new();
    let mut seen = BTreeSet::new();
    let mut tc = None;
    for t in list(m, "tracks", MAX_SEQUENCE_TRACKS)? {
        let c = &t["component"];
        if t.get("unsupported").is_some()
            || [t, c].iter().any(|v| v.get("truncated").is_some_and(|x| x != &Value::Bool(false)))
            || (c["media_kind"] == 3 && t["index"] == 1 && c.get("unsupported").is_some())
        {
            return Err(bad("incomplete sequence track or timecode"));
        }
        let index = number(t, "index")?;
        let kind = match c["media_kind"].as_u64() {
            Some(1) => TrackKind::Video,
            Some(2) => TrackKind::Audio,
            Some(3) if index == 1 => {
                // Observed TCCP flags: 0 is non-drop; 1 is drop-frame. The stored
                // start is already a frame count (107892 = 01;00;00;00 at 29.97).
                // Other bits remain unsupported, as does DF on an incompatible rate.
                let drop_frame = c["flags"] == 1;
                if tc.is_some()
                    || c["class"] != "TCCP"
                    || clock.rate(c)? != r
                    || c["fps"] != r.timecode_base()
                    || clock.ticks(c, "length")? != duration
                    || !(c["flags"] == 0 || (drop_frame && r.supports_drop_frame()))
                {
                    if best_effort {
                        warnings.push("Primary timecode could not be converted; using zero.".into());
                        settings.drop_frame = false;
                        tc = Some(0);
                        continue;
                    }
                    return Err(bad("unsupported primary timecode"));
                }
                let start = number(c, "start")?;
                clock.ticks(c, "start")?;
                tc = Some(start);
                settings.drop_frame = drop_frame;
                continue;
            }
            Some(3 | 4) => continue, // auxiliary timecode and edgecode are retained as evidence
            Some(7) => {
                // Corpus auxiliary track: accept only the observed complete all-filler form. Any
                // other shape, including an absent, empty or multi-component list, is decided
                // here; it must never escape to the caller and placeholder the whole composition.
                let child = c["components"].as_array().filter(|a| a.len() == 1).and_then(|a| a.first());
                let supported = child.is_some_and(|child| {
                    c["class"] == "SEQU"
                        && child["class"] == "FILL"
                        && child["media_kind"] == 7
                        && clock.rate(c).ok() == Some(r)
                        && clock.rate(child).ok() == Some(r)
                        && clock.ticks(c, "length").ok() == Some(duration)
                        && clock.ticks(child, "length").ok() == Some(duration)
                        && ![c, child].iter().any(|v| v.get("unsupported").is_some() || v.get("truncated").is_some_and(|x| x != &Value::Bool(false)))
                });
                if !supported {
                    if best_effort {
                        warnings.push(format!("Auxiliary track {index} was not converted; source evidence retained."));
                        continue;
                    }
                    return Err(bad("nonempty or unsupported auxiliary kind-7 track"));
                }
                continue;
            }
            _ if best_effort => {
                warnings.push(format!("Track {index}: unknown track kind was not converted; source evidence retained."));
                continue;
            }
            _ => return Err(bad("unknown sequence track kind")),
        };
        if !(1..=64).contains(&index) || !seen.insert((kind == TrackKind::Audio, index)) {
            return Err(bad("invalid or duplicate sequence track number"));
        }
        if clock.ticks(c, "length")? != duration {
            return Err(bad("track duration disagrees with composition"));
        }
        let attrs = &t["properties"]["attributes"];
        let stereo = match attrs.get("_TRACK_FORMAT") {
            None => false,
            Some(v) if *v == 2 && kind == TrackKind::Audio => true,
            _ => return Err(bad("unsupported audio track format")),
        };
        let clips = Walk {
            project: p,
            sources: ids,
            rate: r,
            clock,
            kind,
            nodes,
            warnings: &mut warnings,
            best_effort,
            audio_channels: if stereo { 2 } else { 1 },
            frame_width: settings.width,
            frame_height: settings.height,
        }
        .node(c, Tick::ZERO, 0)?;
        if kind == TrackKind::Audio && clips.iter().any(|c| c.channels.len() != if stereo { 2 } else { 1 }) {
            return Err(bad("source channel mapping disagrees with timeline format"));
        }
        tracks.push(TrackPlan {
            kind,
            number: index as usize,
            stereo,
            muted: flag(attrs, "AudioMixerCompMute")?,
            solo: flag(attrs, "AudioMixerCompSolo")?,
            clips,
        });
    }
    let end = tracks.iter().flat_map(|t| &t.clips).map(|c| add(c.start, c.duration)).collect::<Result<Vec<_>>>()?.into_iter().max().unwrap_or(Tick::ZERO);
    if end != duration {
        if !best_effort || end > duration {
            return Err(bad("empty sequence or trailing filler requires explicit native duration support"));
        }
        let reason = "Trailing Avid gap represented by an offline placeholder to preserve sequence duration".to_string();
        warnings.push(reason.clone());
        let placeholder = ClipPlan {
            item: ItemId(0),
            placeholder: Some(reason),
            start: end,
            source_in: Tick::ZERO,
            duration: Tick(duration.0 - end.0),
            channels: Vec::new(),
            scale_to_frame: false,
            effects: Vec::new(),
            sampled_source_offsets: Vec::new(),
        };
        if let Some(track) = tracks.iter_mut().find(|t| t.kind == TrackKind::Video) {
            track.clips.push(placeholder);
        } else {
            tracks.push(TrackPlan { kind: TrackKind::Video, number: 1, stereo: false, muted: false, solo: false, clips: vec![placeholder] });
        }
    }
    if best_effort && tc.is_none() {
        warnings.push("Missing primary timecode: sequence starts at zero.".into());
        tc = Some(0);
    }
    tracks.sort_by_key(|t| (t.kind == TrackKind::Audio, t.number));
    Ok(SequencePlan { name, settings, start_timecode: tc.ok_or_else(|| bad("missing primary timecode"))?, duration, tracks, evidence: m.clone(), warnings })
}

/// Preview is read-only. Unsupported compositions are reported individually; import refuses a
/// partially supported batch. All raw attributes (including color) survive in native metadata.
pub fn plan(p: &Project, bytes: &[u8], bin: BinId) -> Result<Plan> {
    plan_inner(p, bytes, bin, false)
}
/// Preserve interpretable edits and timing, with explicit bypass/placeholder diagnostics.
pub fn plan_best_effort(p: &Project, bytes: &[u8], bin: BinId) -> Result<Plan> {
    plan_inner(p, bytes, bin, true)
}
fn plan_inner(p: &Project, bytes: &[u8], bin: BinId, best_effort: bool) -> Result<Plan> {
    plan_document(p, &avid_catalog::read_document(bytes)?, bin, best_effort)
}
/// Convert a shared parsed snapshot. The catalog is still validated; a caller cannot bypass
/// identity/membership checks by using this entry point instead of JSON.
pub fn plan_document(p: &Project, doc: &avid_catalog::Document, bin: BinId, best_effort: bool) -> Result<Plan> {
    let _ = avid_catalog::plan_document(doc, avid_catalog::TransferMode::Sequences)?;
    let (uid, ids) = match sources(p, bin) {
        Ok(v) => v,
        Err(_) if best_effort && doc.bins.len() == 1 && p.root.find_bin(bin).is_some_and(|b| b.children.is_empty()) => {
            (text(&doc.bins[0]["summary"], "bin_uid")?.to_owned(), BTreeMap::new())
        }
        Err(e) => return Err(e),
    };
    let bins = &doc.bins;
    let matching: Vec<_> = bins.iter().filter(|b| b["summary"]["bin_uid"] == uid).collect();
    if matching.len() != 1 {
        return Err(bad("report must identify exactly one matching source bin"));
    }
    let mut result = Plan { bin, sequences:Vec::new(), rejected:Vec::new(), warnings:vec!["Sequence audio output uses FilmCraft defaults: 48000 Hz stereo. Raw Avid attributes and auxiliary tracks are retained; color, mixer automation, markers and most effects are not translated.".into()] };
    // Not every placed composition is a timeline. A bin also holds subclips, saved effect
    // templates, group clips and motion effects, each a composition mob with its own usage code.
    // Converting one produces a sequence item that is not a sequence, so name what was skipped
    // instead and leave it to the parts of the importer that model those kinds.
    let mut skipped = BTreeMap::<&str, usize>::new();
    let mut compositions = Vec::new();
    for m in list(matching[0], "mobs", 200_000)? {
        if m["kind"] != "CompositionMob" || m["user_placed"] != true {
            continue;
        }
        match m["usage_code"].as_i64() {
            // Imported as bin contents by the catalog, each restricting one master.
            Some(2) => *skipped.entry("subclips").or_default() += 1,
            // An Avid effect template does not describe FilmCraft's own effects.
            Some(3) => *skipped.entry("saved effect templates").or_default() += 1,
            // Grouped media: selector decoding exists, but multicam conversion is not implemented.
            Some(4) => *skipped.entry("group clips").or_default() += 1,
            // A retime, with its own rendered media when the effect has been rendered.
            Some(6) => *skipped.entry("motion effect clips").or_default() += 1,
            _ => compositions.push(m),
        }
    }
    for (kind, count) in &skipped {
        result.warnings.push(format!("{count} {kind} in this bin are not timelines and were not converted into sequences."));
    }
    if compositions.len() > 1024 {
        return Err(bad("too many compositions"));
    }
    let mut identities = BTreeMap::<_, usize>::new();
    for m in &compositions {
        if m["live"] == true
            && let Ok(id) = avid_catalog::full_identity(&m["id"])
        {
            *identities.entry(id).or_default() += 1;
        }
    }
    let mut nodes = 0;
    for m in compositions {
        // Eligibility/identity failures must never be turned into live placeholder sequences.
        // Reject every ambiguous live copy, while allowing unrelated sequences to proceed.
        let eligible = || -> Result<()> {
            if m["live"] != true {
                return Err(bad("inactive composition"));
            }
            let id = avid_catalog::full_identity(&m["id"])?;
            if identities.get(&id) != Some(&1) {
                return Err(bad("duplicate composition identity"));
            }
            text(m, "name")?;
            Ok(())
        };
        if let Err(e) = eligible() {
            if !best_effort {
                return Err(e);
            }
            result.rejected.push(format!("{}: {e}", text(m, "name").unwrap_or("(unnamed composition)")));
            continue;
        }
        let converted = sequence(p, &ids, m, &mut nodes, best_effort);
        let converted = match converted {
            Err(ref e) if best_effort && nodes <= MAX_NODES => recovery::sequence_placeholder(m, e),
            other => other,
        };
        match converted {
            Ok(q) => {
                result.warnings.extend(q.warnings.iter().map(|w| format!("{}: {w}", q.name)));
                result.sequences.push(q);
            }
            Err(e) => result.rejected.push(format!("{}: {e}", text(m, "name")?)),
        }
    }
    if result.sequences.is_empty() && result.rejected.is_empty() {
        return Err(bad("no placed compositions in report"));
    }
    Ok(result)
}

/// Apply independently planned sequences and return names/reasons for unconvertible compositions.
pub fn apply_supported(p: &mut Project, mut plan: Plan) -> Result<(Vec<ItemId>, Vec<String>)> {
    let rejected = std::mem::take(&mut plan.rejected);
    Ok((apply(p, plan)?, rejected))
}
/// Call inside a project transaction. Strict plans reject the entire batch on an unsupported sequence.
pub fn apply(p: &mut Project, plan: Plan) -> Result<Vec<ItemId>> {
    if !plan.rejected.is_empty() {
        return Err(bad(plan.rejected.join("; ")));
    }
    avid_catalog::validate_project(p, Some(plan.bin))?;
    let mut required = 0u64;
    for q in &plan.sequences {
        required = required
            .checked_add(129)
            .and_then(|n| n.checked_add(q.tracks.iter().map(|t| t.clips.len() as u64 * 2).sum::<u64>()))
            .ok_or_else(|| bad("ID overflow"))?;
    }
    p.next_id.checked_add(required).ok_or_else(|| bad("project ID space exhausted"))?;
    let mut out = Vec::new();
    for q in plan.sequences {
        let ntracks = |kind| q.tracks.iter().filter(|t| t.kind == kind).map(|t| t.number).max().unwrap_or(0);
        let id = p.new_sequence(&q.name, q.settings.clone(), ntracks(TrackKind::Video), ntracks(TrackKind::Audio), Some(plan.bin));
        for t in q.tracks {
            let mut placed = Vec::new();
            for c in t.clips {
                let item = if let Some(reason) = &c.placeholder { recovery::placeholder(p, plan.bin, t.kind, t.stereo, c.duration, reason)? } else { c.item };
                let mut clip = p
                    .make_track_item(item, t.kind, c.start, TimeRange::new(c.source_in, c.duration), q.settings.frame_rate)
                    .ok_or_else(|| bad("source disappeared"))?;
                if clip.duration != c.duration {
                    return Err(bad("native placement would change duration"));
                }
                if c.placeholder.is_some() {
                    // The placeholder canvas uses the sequence raster. This does not assert
                    // geometry for the unknown Avid source, and keeps saved auto-points finite.
                    for effect in &mut clip.effects {
                        filmcraft_project::resolve_auto_points(effect, (q.settings.width, q.settings.height), (q.settings.width, q.settings.height));
                    }
                }
                clip.source_channels = c.channels;
                clip.scale_to_frame = c.scale_to_frame;
                for effect in c.effects {
                    if effect.effect == "opacity" && !effect.masks.is_empty() {
                        clip.effect_mut("opacity").ok_or_else(|| bad("intrinsic Opacity is unavailable"))?.masks.extend(effect.masks);
                    } else {
                        clip.effects.push(effect);
                    }
                }
                if !c.sampled_source_offsets.is_empty() {
                    let lane = clip.effect_mut("time_remap").ok_or_else(|| bad("intrinsic Time Remapping is unavailable"))?;
                    let mut samples = Param::new(ParamValue::Float(0.0));
                    for (index, offset) in c.sampled_source_offsets.iter().enumerate() {
                        if offset.0 < 0 || offset.0 as u64 > 1u64 << 53 {
                            return Err(bad("Timewarp sampled position is outside exact project range"));
                        }
                        let mut key = Keyframe::new(q.settings.frame_rate.tick_of(index as i64), ParamValue::Float(offset.0 as f64));
                        key.interp = filmcraft_project::keyframe::Interpolation::Hold;
                        samples.keyframes.push(key);
                    }
                    lane.params.insert("sampled_source_offset_ticks".to_owned(), samples);
                }
                clip.time_interpolation = filmcraft_project::TimeInterpolation::FrameSampling;
                placed.push(clip);
            }
            let track = p.sequence_mut(id).and_then(|s| s.tracks_mut(t.kind).get_mut(t.number - 1)).ok_or_else(|| bad("new track missing"))?;
            track.channels = if t.stereo { AudioChannels::Stereo } else { AudioChannels::Mono };
            track.muted = t.muted;
            track.solo = t.solo;
            track.items = placed;
        }
        let seq = p.sequence_mut(id).ok_or_else(|| bad("new sequence missing"))?;
        seq.start_timecode = q.start_timecode;
        if seq.duration() != q.duration {
            return Err(bad("native duration changed"));
        }
        p.item_mut(id)
            .ok_or_else(|| bad("new item missing"))?
            .metadata
            .insert(SEQUENCE_KEY.into(), json!({"version":1,"composition":q.evidence,"conversion_warnings":q.warnings}).to_string());
        out.push(id);
    }
    // Binding has established source geometry, so native Motion anchors/positions can be
    // resolved now instead of retaining NaN "auto" coordinates in imported video clips.
    p.resolve_placed_auto_points(|id| out.contains(&id), |_| true);
    Ok(out)
}

#[cfg(test)]
mod warp_tests {
    use super::*;

    fn resize_evidence(changes: &[(u16, f64)]) -> Value {
        let mut controls = BTreeMap::from([(0x8125, 100.0), (0x8126, -100.0), (0x8129, 50.0), (0x812a, 50.0), (0x812b, 1.0)]);
        controls.extend(changes.iter().copied());
        let family = [0x56, 0x8d, 0x7e, 0x84, 0xd5, 0x11, 0x93, 0x5a, 0x50, 0xf8, 0x57, 0xc1, 0, 0];
        json!({"properties":{"numeric_parameters":controls.into_iter().map(|(id,value)| {
            let mut guid = Vec::from(id.to_le_bytes());
            guid.extend(family);
            json!({"id":guid,"value":value})
        }).collect::<Vec<_>>()}})
    }

    #[test]
    fn static_resize_geometry_converts_but_other_controls_stay_blocked() {
        let effects = resize_effects(&resize_evidence(&[]), 1920, 1080).unwrap();
        assert_eq!(effects.len(), 1);
        let fx = &effects[0];
        assert_eq!(fx.effect, "transform");
        assert_eq!(fx.param("position").unwrap().value, ParamValue::Vec2(Vec2::new(1152.0, 432.0)));
        assert_eq!(fx.param("scale_width").unwrap().value, ParamValue::Float(50.0));
        assert_eq!(fx.param("scale_height").unwrap().value, ParamValue::Float(50.0));
        assert!(resize_effects(&resize_evidence(&[(0x812e, 47.0)]), 1920, 1080).is_err());
        assert!(resize_effects(&resize_evidence(&[(0x8147, 50.0)]), 1920, 1080).is_err());
        assert!(resize_effects(&resize_evidence(&[(0x812a, 75.0)]), 1920, 1080).is_err());
        assert!(resize_effects(&resize_evidence(&[(0x8125, 1e20)]), 1920, 1080).is_err());
    }

    fn evidence(changes: &[(u16, f64)]) -> Value {
        let mut controls = BTreeMap::from([
            (0x81a1, 0.59),
            (0x81a9, 1.0),
            (0x81aa, 100.0),
            (0x81ab, -100.0),
            (0x81b2, 1.0),
            (0x81b3, 50.0),
            (0x81b4, 75.0),
            (0x81b7, 1.0),
            (0x81ba, 30.0),
            (0x81bc, 1.0),
            (0x81ed, 1.0),
            (0x81fc, 255.0),
        ]);
        controls.extend(changes.iter().copied());
        let family = [0x56, 0x8d, 0x7e, 0x84, 0xd5, 0x11, 0x93, 0x5a, 0x50, 0xf8, 0x57, 0xc1, 0, 0];
        json!({"properties":{"numeric_parameters":controls.into_iter().map(|(id,value)| {
            let mut guid = Vec::from(id.to_le_bytes());
            guid.extend(family);
            json!({"id":guid,"value":value})
        }).collect::<Vec<_>>()}})
    }

    #[test]
    fn calibrated_static_2d_controls_convert_and_3d_controls_stop() {
        let effects = warp_effects(&evidence(&[]), 1920, 1080).unwrap();
        assert_eq!(effects.len(), 1);
        let fx = &effects[0];
        assert_eq!(fx.effect, "transform");
        assert_eq!(fx.param("position").unwrap().value, ParamValue::Vec2(Vec2::new(1152.0, 648.0)));
        assert_eq!(fx.param("scale_width").unwrap().value, ParamValue::Float(50.0));
        assert_eq!(fx.param("scale_height").unwrap().value, ParamValue::Float(75.0));
        assert_eq!(fx.param("rotation").unwrap().value, ParamValue::Float(30.0));
        assert!(warp_effects(&evidence(&[(0x81b8, 30.0)]), 1920, 1080).is_err());
        assert!(warp_effects(&evidence(&[(0x81bd, 100.0)]), 1920, 1080).is_err());
        assert!(warp_effects(&evidence(&[(0x81c9, 50.0)]), 1920, 1080).is_err());
        assert!(warp_effects(&evidence(&[(0x8206, 1.0)]), 1920, 1080).is_err());
        assert_eq!(warp_effects(&evidence(&[(0x814d, 0.0), (0x8153, 0.0), (0x8206, 0.0)]), 1920, 1080).unwrap().len(), 1);
        let mut missing = evidence(&[]);
        missing["properties"]["numeric_parameters"] = Value::Null;
        assert!(warp_effects(&missing, 1920, 1080).is_err());
    }

    #[test]
    fn crop_boundaries_from_avid_export_precede_transform_and_disabled_crop_is_ignored() {
        let crop_values = [(0x81cb, 1.0), (0x81cc, -799.0), (0x81cd, 699.0), (0x81ce, 839.0), (0x81cf, -759.0)];
        let effects = warp_effects(&evidence(&crop_values), 1920, 1080).unwrap();
        assert_eq!(effects.iter().map(|e| e.effect.as_str()).collect::<Vec<_>>(), ["crop", "transform"]);
        let crop = &effects[0];
        for (name, value) in [("left", 10.05), ("right", 15.05), ("top", 8.05), ("bottom", 12.05)] {
            assert_eq!(crop.param(name).unwrap().value, ParamValue::Float(value));
        }
        assert_eq!(warp_effects(&evidence(&[(0x81cb, 0.0), (0x81cc, -600.0)]), 1920, 1080).unwrap().len(), 1);
        assert!(warp_effects(&evidence(&[(0x81cb, 1.0), (0x81cc, 1001.0)]), 1920, 1080).is_err());
        assert!(warp_effects(&evidence(&[(0x81cb, 1.0), (0x81cc, 700.0), (0x81cd, 500.0)]), 1920, 1080).is_err());
    }
}

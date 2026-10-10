//! Effect presets (Premiere's Presets bin): one or more configured effects — parameters,
//! keyframes and masks — saved under a name and applied to clips.
//!
//! - **Keyframes.** Times are stored relative to the in point of the clip the preset was saved
//!   from, with that clip's length (`source_duration`, media ticks). Applying re-times them per
//!   [`KeyframeMode`]: *Scale* stretches them over the target clip, *Anchor to In Point* keeps their
//!   distance from the target's in point, *Anchor to Out Point* their distance from its out point.
//!   Saving with `keyframes: "none"` stores every parameter as its value at the playhead.
//! - **Geometry.** Point parameters and mask paths are in clip pixels; they are scaled by the
//!   target / source frame-size ratio so a preset made on 4K media lands in the same place on HD.
//! - **Intrinsic effects** (Motion, Opacity, Time Remapping) replace the target clip's own instance;
//!   standard effects are added before the intrinsic ones (render order), like `effects.apply`.
//! - **Library.** Built-in presets are authored in code ([`builtin_presets`]); user presets persist
//!   in `<data dir>/effect-presets.json`. Preset files (export/import) are JSON:
//!   `{"format": "filmcraft.effect-presets", "version": 1, "presets": [EffectPreset…]}`.
//!
//! Commands: `presets.list`, `presets.save`, `presets.apply` (undoable), `presets.delete`,
//! `presets.rename`, `presets.export`, `presets.import`.

use std::path::{Path, PathBuf};

use filmcraft_geom::{Affine, Vec2};
use filmcraft_project::{ClipId, EffectInstance, Keyframe, Mask, MaskPath, Param, ParamValue, TrackItem, TrackKind};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, clip_p, has_seq, str_p};
use crate::{EngineError, Result, Session};

pub const FILE_FORMAT: &str = "filmcraft.effect-presets";
const LIBRARY_FILE: &str = "effect-presets.json";

/// How a preset's keyframes are placed on the target clip.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum KeyframeMode {
    #[default]
    Scale,
    AnchorToIn,
    AnchorToOut,
}

impl KeyframeMode {
    pub fn from_name(s: &str) -> Option<Self> {
        match s.to_ascii_lowercase().replace([' ', '_', '-'], "").as_str() {
            "scale" => Some(KeyframeMode::Scale),
            "anchorin" | "anchortoin" | "anchortoinpoint" | "in" => Some(KeyframeMode::AnchorToIn),
            "anchorout" | "anchortoout" | "anchortooutpoint" | "out" => Some(KeyframeMode::AnchorToOut),
            _ => None,
        }
    }
    pub fn label(self) -> &'static str {
        match self {
            KeyframeMode::Scale => "Scale",
            KeyframeMode::AnchorToIn => "Anchor to In Point",
            KeyframeMode::AnchorToOut => "Anchor to Out Point",
        }
    }
}

/// A saved effect preset.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct EffectPreset {
    pub name: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub description: String,
    #[serde(default)]
    pub keyframes: KeyframeMode,
    /// Length (media ticks) of the clip the preset was saved from; keyframe times are relative to its in point.
    pub source_duration: Tick,
    /// Frame size (clip pixels) of the clip the preset was saved from.
    #[serde(default = "hd")]
    pub source_size: (u32, u32),
    pub effects: Vec<EffectInstance>,
    /// Built-in (not saved, cannot be deleted).
    #[serde(skip)]
    pub builtin: bool,
}

fn hd() -> (u32, u32) {
    (1920, 1080)
}

#[derive(Serialize, Deserialize)]
struct PresetFile {
    format: String,
    version: u32,
    presets: Vec<EffectPreset>,
}

/// User presets (+ where they persist).
#[derive(Default)]
pub struct PresetLibrary {
    pub user: Vec<EffectPreset>,
    dir: Option<PathBuf>,
}

impl PresetLibrary {
    /// Persist user presets in `data_dir` (and load what is there).
    pub fn set_dir(&mut self, data_dir: &Path) {
        self.dir = Some(data_dir.to_path_buf());
        let path = data_dir.join(LIBRARY_FILE);
        if let Ok(bytes) = std::fs::read(&path) {
            match parse_file(&bytes) {
                Ok(p) => self.user = p,
                Err(e) => log::warn!("{}: {e}", path.display()),
            }
        }
    }
    fn persist(&self) -> std::result::Result<(), String> {
        let Some(d) = &self.dir else { return Ok(()) };
        let _ = std::fs::create_dir_all(d);
        let bytes = file_bytes(&self.user);
        filmcraft_format::atomic_write(&d.join(LIBRARY_FILE), &bytes).map_err(|e| e.to_string())
    }
    /// Built-in and user presets (user presets shadow built-ins of the same name).
    pub fn all(&self) -> Vec<EffectPreset> {
        let mut v: Vec<EffectPreset> = builtin_presets().into_iter().filter(|b| !self.user.iter().any(|u| u.name == b.name)).collect();
        v.extend(self.user.iter().cloned());
        v
    }
    pub fn find(&self, name: &str) -> Option<EffectPreset> {
        self.user.iter().find(|p| p.name == name).cloned().or_else(|| builtin_presets().into_iter().find(|p| p.name == name))
    }
}

fn file_bytes(presets: &[EffectPreset]) -> Vec<u8> {
    let f = PresetFile { format: FILE_FORMAT.into(), version: 1, presets: presets.to_vec() };
    serde_json::to_vec_pretty(&f).unwrap_or_default()
}

fn parse_file(bytes: &[u8]) -> std::result::Result<Vec<EffectPreset>, String> {
    let f: PresetFile = serde_json::from_slice(bytes).map_err(|e| format!("not an effect preset file: {e}"))?;
    if f.format != FILE_FORMAT {
        return Err(format!("not an effect preset file (format `{}`)", f.format));
    }
    if f.version > 1 {
        return Err(format!("preset file version {} is newer than this build reads (1)", f.version));
    }
    for p in &f.presets {
        if p.effects.iter().any(|e| e.def().is_none()) {
            return Err(format!("preset `{}` uses an effect this build doesn't have", p.name));
        }
    }
    Ok(f.presets)
}

// ---------------------------------------------------------------- built-ins

fn kf(t_secs: f64, v: ParamValue) -> Keyframe {
    Keyframe::new(Tick((t_secs * TICKS_PER_SECOND as f64).round() as i64), v)
}

/// A fresh instance of a built-in effect (a bare instance named `id` if the registry lacks it,
/// which the preset tests rule out).
pub(crate) fn effect_instance(id: &str) -> EffectInstance {
    filmcraft_project::find_effect(id).map(|d| d.instance()).unwrap_or_else(|| EffectInstance {
        effect: id.to_string(),
        enabled: true,
        params: Default::default(),
        masks: Vec::new(),
        post_fader: false,
        essential: false,
        layer: None,
    })
}

fn effect(id: &str, params: &[(&str, ParamValue)]) -> EffectInstance {
    let mut e = effect_instance(id);
    for (k, v) in params {
        if let Some(p) = e.params.get_mut(*k) {
            p.value = v.clone();
        }
    }
    e
}

fn animate(e: &mut EffectInstance, param: &str, keys: Vec<Keyframe>) {
    if let Some(p) = e.params.get_mut(param) {
        p.value = keys[0].value.clone();
        p.keyframes = keys;
    }
}

fn preset(name: &str, description: &str, keyframes: KeyframeMode, secs: f64, effects: Vec<EffectInstance>) -> EffectPreset {
    EffectPreset {
        name: name.into(),
        description: description.into(),
        keyframes,
        source_duration: Tick((secs * TICKS_PER_SECOND as f64) as i64),
        source_size: hd(),
        effects,
        builtin: true,
    }
}

/// Presets that ship with FilmCraft (original work: parameter values chosen by us).
pub fn builtin_presets() -> Vec<EffectPreset> {
    let f = ParamValue::Float;
    let mut blur_in = effect("gaussian_blur", &[]);
    animate(&mut blur_in, "blurriness", vec![kf(0.0, f(60.0)), kf(1.0, f(0.0))]);
    let mut blur_out = effect("gaussian_blur", &[]);
    animate(&mut blur_out, "blurriness", vec![kf(9.0, f(0.0)), kf(10.0, f(60.0))]);
    let mut fade_in = effect("opacity", &[]);
    animate(&mut fade_in, "opacity", vec![kf(0.0, f(0.0)), kf(1.0, f(100.0))]);
    let mut fade_out = effect("opacity", &[]);
    animate(&mut fade_out, "opacity", vec![kf(9.0, f(100.0)), kf(10.0, f(0.0))]);
    let mut mosaic_in = effect("mosaic", &[]);
    animate(&mut mosaic_in, "horizontal", vec![kf(0.0, f(8.0)), kf(1.0, f(400.0))]);
    animate(&mut mosaic_in, "vertical", vec![kf(0.0, f(5.0)), kf(1.0, f(225.0))]);
    // soft oval vignette: darken outside an inverted, heavily feathered ellipse
    let mut vignette = effect("brightness_contrast", &[("brightness", f(-60.0))]);
    let mut m = Mask::new("Mask (1)", MaskPath::ellipse(Vec2::new(960.0, 540.0), Vec2::new(900.0, 520.0)));
    m.inverted = true;
    m.feather.value = f(320.0);
    vignette.masks.push(m);
    let mut spotlight = effect("brightness_contrast", &[("brightness", f(18.0)), ("contrast", f(10.0))]);
    let mut m = Mask::new("Mask (1)", MaskPath::ellipse(Vec2::new(960.0, 540.0), Vec2::new(420.0, 420.0)));
    m.feather.value = f(160.0);
    spotlight.masks.push(m);
    let warm =
        effect("tint", &[("black", ParamValue::Color([0.08, 0.04, 0.02, 1.0])), ("white", ParamValue::Color([1.0, 0.93, 0.8, 1.0])), ("amount", f(35.0))]);
    let mut pip = effect("motion", &[("scale", f(35.0)), ("position", ParamValue::Vec2(Vec2::new(1530.0, 270.0)))]);
    pip.enabled = true;
    vec![
        preset("Blur In", "Gaussian Blur from 60 to 0 over the first second", KeyframeMode::AnchorToIn, 10.0, vec![blur_in]),
        preset("Blur Out", "Gaussian Blur from 0 to 60 over the last second", KeyframeMode::AnchorToOut, 10.0, vec![blur_out]),
        preset("Fade In", "Opacity 0 → 100 % over the first second", KeyframeMode::AnchorToIn, 10.0, vec![fade_in]),
        preset("Fade Out", "Opacity 100 → 0 % over the last second", KeyframeMode::AnchorToOut, 10.0, vec![fade_out]),
        preset("Mosaic In", "Large blocks resolving to the picture over the first second", KeyframeMode::AnchorToIn, 10.0, vec![mosaic_in]),
        preset("Soft Vignette", "Darkens outside a feathered oval mask", KeyframeMode::Scale, 10.0, vec![vignette]),
        preset("Spotlight", "Brightens inside a feathered circular mask", KeyframeMode::Scale, 10.0, vec![spotlight]),
        preset("Warm Tint", "Warm highlights, brown shadows", KeyframeMode::Scale, 10.0, vec![warm]),
        preset("PiP 35% Upper Right", "Picture-in-picture: 35 % scale in the upper right", KeyframeMode::Scale, 10.0, vec![pip]),
    ]
}

// ---------------------------------------------------------------- save / apply

/// Every parameter of an effect instance (its own and its masks').
fn params_mut(e: &mut EffectInstance) -> Vec<&mut Param> {
    let mut v: Vec<&mut Param> = e.params.values_mut().collect();
    for m in &mut e.masks {
        v.extend(m.params_mut());
    }
    v
}

/// Media length of a track item.
fn media_len(it: &TrackItem) -> Tick {
    Tick((it.duration.0 as f64 * it.speed.abs()).round() as i64)
}

/// Build a preset from effects of a clip. `mt` = the clip's media time at the playhead.
pub fn capture(it: &TrackItem, idx: &[usize], name: &str, description: &str, mode: Option<KeyframeMode>, mt: Tick, size: (u32, u32)) -> EffectPreset {
    let mut effects: Vec<EffectInstance> = idx.iter().filter_map(|i| it.effects.get(*i).cloned()).collect();
    for e in &mut effects {
        e.essential = false;
        for p in params_mut(e) {
            match mode {
                None => {
                    // without keyframes: the value at the playhead
                    p.value = p.value_at(mt);
                    p.keyframes.clear();
                }
                Some(_) => {
                    for k in &mut p.keyframes {
                        k.time -= it.source_in;
                    }
                }
            }
        }
    }
    EffectPreset {
        name: name.to_string(),
        description: description.to_string(),
        keyframes: mode.unwrap_or_default(),
        source_duration: media_len(it),
        source_size: size,
        effects,
        builtin: false,
    }
}

/// Map a preset keyframe time (relative to the source in point) onto `target`.
pub fn retime(rel: Tick, mode: KeyframeMode, source_len: Tick, target: &TrackItem) -> Tick {
    let tin = target.source_in;
    let tlen = media_len(target);
    match mode {
        KeyframeMode::AnchorToIn => tin + rel,
        KeyframeMode::AnchorToOut => tin + tlen - (source_len - rel),
        KeyframeMode::Scale => {
            if source_len.0 <= 0 {
                tin + rel
            } else {
                tin + Tick((rel.0 as i128 * tlen.0 as i128 / source_len.0 as i128) as i64)
            }
        }
    }
}

fn scale_value(v: &mut ParamValue, sx: f64, sy: f64) {
    match v {
        ParamValue::Vec2(p) if !p.x.is_nan() => *p = Vec2::new(p.x * sx, p.y * sy),
        ParamValue::Path(path) => *path = path.transformed(&Affine::scale(sx, sy)),
        _ => {}
    }
}

/// The preset's effects prepared for `target` (keyframes re-timed, geometry scaled).
pub fn instantiate(p: &EffectPreset, target: &TrackItem, size: (u32, u32)) -> Vec<EffectInstance> {
    let sx = size.0 as f64 / p.source_size.0.max(1) as f64;
    let sy = size.1 as f64 / p.source_size.1.max(1) as f64;
    let geometric = (sx - 1.0).abs() > 1e-9 || (sy - 1.0).abs() > 1e-9;
    let mut out = p.effects.clone();
    for e in &mut out {
        // Point params scale only when they are positions (the def kind says so)
        let point_ids: Vec<String> = e
            .def()
            .map(|d| d.params.iter().filter(|q| matches!(q.kind, filmcraft_project::ParamKind::Point)).map(|q| q.id.to_string()).collect())
            .unwrap_or_default();
        for (id, prm) in e.params.iter_mut() {
            for k in &mut prm.keyframes {
                k.time = retime(k.time, p.keyframes, p.source_duration, target);
            }
            prm.keyframes.sort_by_key(|k| k.time);
            prm.keyframes.dedup_by_key(|k| k.time);
            if geometric && point_ids.contains(id) {
                scale_value(&mut prm.value, sx, sy);
                for k in &mut prm.keyframes {
                    scale_value(&mut k.value, sx, sy);
                }
            }
        }
        for m in &mut e.masks {
            for prm in m.params_mut() {
                for k in &mut prm.keyframes {
                    k.time = retime(k.time, p.keyframes, p.source_duration, target);
                }
                prm.keyframes.sort_by_key(|k| k.time);
                prm.keyframes.dedup_by_key(|k| k.time);
                if geometric {
                    scale_value(&mut prm.value, sx, sy);
                    for k in &mut prm.keyframes {
                        scale_value(&mut k.value, sx, sy);
                    }
                }
            }
            if geometric {
                // feather / expansion are lengths: scale by the mean factor
                let s = (sx * sy).sqrt();
                for prm in [&mut m.feather, &mut m.expansion] {
                    if let ParamValue::Float(v) = &mut prm.value {
                        *v *= s;
                    }
                    for k in &mut prm.keyframes {
                        if let ParamValue::Float(v) = &mut k.value {
                            *v *= s;
                        }
                    }
                }
            }
        }
    }
    out
}

/// Keep `old`'s keyframes that lie outside the span `new` animates, so presets keyframing the same
/// parameter at different times (Fade In, then Fade Out) add up instead of replacing each other.
fn merge_keyframes(new: &mut EffectInstance, old: &EffectInstance) {
    for (id, p) in new.params.iter_mut() {
        let (Some(first), Some(last)) = (p.keyframes.first().map(|k| k.time), p.keyframes.last().map(|k| k.time)) else { continue };
        let Some(o) = old.params.get(id) else { continue };
        p.keyframes.extend(o.keyframes.iter().filter(|k| k.time < first || k.time > last).cloned());
        p.keyframes.sort_by_key(|k| k.time);
    }
}

/// Put preset effects on a clip: intrinsic ones replace the clip's instance (keeping its keyframes
/// outside the span the preset animates), others are inserted before the intrinsic effects.
pub fn put_effects(it: &mut TrackItem, effects: Vec<EffectInstance>) {
    for mut e in effects {
        let intrinsic = e.def().is_some_and(|d| d.intrinsic);
        if intrinsic && let Some(slot) = it.effects.iter_mut().find(|x| x.effect == e.effect) {
            merge_keyframes(&mut e, slot);
            *slot = e;
            continue;
        }
        let audio = e.def().is_some_and(|d| d.kind == filmcraft_project::EffectKind::Audio);
        let pos = if audio { it.effects.len() } else { it.effects.iter().position(|x| x.def().is_some_and(|d| d.intrinsic)).unwrap_or(it.effects.len()) };
        it.effects.insert(pos, e);
    }
}

// ---------------------------------------------------------------- commands

type Run = fn(&mut Session, &Value) -> Result<Value>;

fn spec(
    id: &'static str,
    label: &'static str,
    params: &'static str,
    enabled: fn(&Session) -> std::result::Result<(), String>,
    run: Run,
    journal: bool,
) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled, run, journal }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec("presets.list", "List Effect Presets", "{}", always, list, false),
        spec(
            "presets.save",
            "Save Preset",
            r#"{"clip":id?,"effects":[index]?,"name":str,"description":str?,"keyframes":"scale"|"anchorIn"|"anchorOut"|"none"?}"#,
            has_seq,
            save,
            true,
        ),
        spec("presets.apply", "Apply Preset", r#"{"preset":str,"clips":[id]?}"#, has_seq, apply, true),
        spec("presets.delete", "Delete Preset", r#"{"name":str}"#, always, delete, true),
        spec("presets.rename", "Rename Preset", r#"{"name":str,"to":str}"#, always, rename, true),
        spec("presets.export", "Export Presets", r#"{"path":str,"names":[str]?}"#, always, export, false),
        spec("presets.import", "Import Presets", r#"{"path":str}"#, always, import, true),
    ]
}

fn preset_json(p: &EffectPreset) -> Value {
    json!({
        "name": p.name,
        "description": p.description,
        "builtin": p.builtin,
        "keyframes": p.keyframes.label(),
        "effects": p.effects.iter().map(|e| e.def().map(|d| d.name).unwrap_or(e.effect.as_str()).to_string()).collect::<Vec<_>>(),
        "animated": p.effects.iter().any(|e| e.is_animated()),
        "masks": p.effects.iter().map(|e| e.masks.len()).sum::<usize>(),
    })
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    Ok(json!({"presets": s.presets.all().iter().map(preset_json).collect::<Vec<_>>()}))
}

fn clip_size(s: &Session, it: &TrackItem) -> (u32, u32) {
    filmcraft_render::source_size(&s.project, it.item).or_else(|| s.active_sequence().map(|q| (q.settings.width, q.settings.height))).unwrap_or(hd())
}

fn save(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").map(str::trim).filter(|n| !n.is_empty()).ok_or_else(|| bad("presets.save", "need a `name`"))?.to_string();
    let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let clip = clip_p(p, "clip")
        .or_else(|| s.state.selection.iter().copied().find(|c| seq.find_item(*c).and_then(|(t, _)| seq.track(t)).is_some_and(|t| t.kind == TrackKind::Video)))
        .or_else(|| s.state.selection.first().copied())
        .ok_or_else(|| bad("presets.save", "select a clip"))?;
    let (_, it) = seq.find_item(clip).ok_or_else(|| bad("presets.save", "no such clip"))?;
    let idx: Vec<usize> = match p.get("effects").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(|n| n as usize)).filter(|i| *i < it.effects.len()).collect(),
        // default: the standard (non-intrinsic, non-layer) effects
        None => (0..it.effects.len())
            .filter(|i| {
                let e = &it.effects[*i];
                e.def().is_some_and(|d| !d.intrinsic) && !filmcraft_project::graphic::is_layer(e) && !e.essential
            })
            .collect(),
    };
    if idx.is_empty() {
        return Err(bad("presets.save", "no effects to save (pass `effects`)"));
    }
    let mode = match str_p(p, "keyframes") {
        None => Some(KeyframeMode::Scale),
        Some(k) if k.eq_ignore_ascii_case("none") => None,
        Some(k) => Some(KeyframeMode::from_name(k).ok_or_else(|| bad("presets.save", format!("unknown keyframe mode `{k}`")))?),
    };
    let mt = it.source_time_at(s.playhead().clamp(it.start, it.end() - Tick(1)));
    let size = clip_size(s, it);
    let preset = capture(it, &idx, &name, str_p(p, "description").unwrap_or(""), mode, mt, size);
    let out = preset_json(&preset);
    s.presets.user.retain(|x| x.name != name);
    s.presets.user.push(preset);
    s.presets.persist().map_err(EngineError::Other)?;
    Ok(out)
}

fn apply(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "preset").or_else(|| str_p(p, "name")).ok_or_else(|| bad("presets.apply", "need `preset`"))?;
    let preset = s.presets.find(name).ok_or_else(|| bad("presets.apply", format!("no preset `{name}`")))?;
    let clips: Vec<ClipId> = match p.get("clips").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ClipId)).collect(),
        None => s.state.selection.clone(),
    };
    if clips.is_empty() {
        return Err(bad("presets.apply", "select clips first"));
    }
    let audio = preset.effects.iter().all(|e| e.def().is_some_and(|d| d.kind == filmcraft_project::EffectKind::Audio));
    let sizes: Vec<(ClipId, (u32, u32))> = {
        let seq = s.active_sequence().ok_or(EngineError::NoSequence)?;
        clips.iter().filter_map(|c| seq.find_item(*c).map(|(_, it)| (*c, clip_size(s, it)))).collect()
    };
    let label = format!("Apply Preset {}", preset.name);
    let n = s.edit_sequence(&label, |q, _, _| {
        let mut n = 0;
        for t in q.all_tracks_mut() {
            if (t.kind == TrackKind::Audio) != audio {
                continue;
            }
            for it in t.items.iter_mut().filter(|i| clips.contains(&i.id)) {
                let size = sizes.iter().find(|(c, _)| *c == it.id).map(|x| x.1).unwrap_or(hd());
                let fx = instantiate(&preset, it, size);
                put_effects(it, fx);
                n += 1;
            }
        }
        if n == 0 {
            return Err(bad("presets.apply", "the preset doesn't fit the selected clips (video/audio)"));
        }
        Ok(n)
    })?;
    Ok(json!({"applied": n}))
}

fn delete(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").ok_or_else(|| bad("presets.delete", "need `name`"))?;
    let before = s.presets.user.len();
    s.presets.user.retain(|x| x.name != name);
    if s.presets.user.len() == before {
        return Err(bad(
            "presets.delete",
            if builtin_presets().iter().any(|b| b.name == name) { "built-in presets cannot be deleted" } else { "no such preset" },
        ));
    }
    s.presets.persist().map_err(EngineError::Other)?;
    Ok(Value::Null)
}

fn rename(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").ok_or_else(|| bad("presets.rename", "need `name`"))?;
    let to = str_p(p, "to").map(str::trim).filter(|t| !t.is_empty()).ok_or_else(|| bad("presets.rename", "need `to`"))?.to_string();
    if s.presets.user.iter().any(|x| x.name == to) {
        return Err(bad("presets.rename", format!("a preset named `{to}` exists")));
    }
    let pr = s.presets.user.iter_mut().find(|x| x.name == name).ok_or_else(|| bad("presets.rename", "no such user preset"))?;
    pr.name = to;
    s.presets.persist().map_err(EngineError::Other)?;
    Ok(Value::Null)
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("presets.export", "need `path`"))?;
    let all = s.presets.all();
    let chosen: Vec<EffectPreset> = match p.get("names").and_then(Value::as_array) {
        Some(n) => {
            let names: Vec<&str> = n.iter().filter_map(Value::as_str).collect();
            let v: Vec<EffectPreset> = all.into_iter().filter(|x| names.contains(&x.name.as_str())).collect();
            if v.len() != names.len() {
                return Err(bad("presets.export", "unknown preset name"));
            }
            v
        }
        None => s.presets.user.clone(),
    };
    if chosen.is_empty() {
        return Err(bad("presets.export", "no presets to export"));
    }
    // through the host: atomic on the desktop, a download on the web (no filesystem there)
    s.services.write_file(path, &file_bytes(&chosen)).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "count": chosen.len()}))
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("presets.import", "need `path`"))?;
    let bytes = s.services.read_file(path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let presets = parse_file(&bytes).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let names: Vec<String> = presets.iter().map(|x| x.name.clone()).collect();
    for pr in presets {
        s.presets.user.retain(|x| x.name != pr.name);
        s.presets.user.push(pr);
    }
    s.presets.persist().map_err(EngineError::Other)?;
    Ok(json!({"imported": names}))
}

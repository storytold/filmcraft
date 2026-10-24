//! `lumetri.auto`: automatic white balance and exposure (Color ▸ Auto Correct; the Auto Correct
//! button of Lumetri ▸ Basic Correction). The clip's source picture — the frame at the playhead,
//! or frames sampled across the clip — is rendered with its Lumetri bypassed, measured by
//! [`filmcraft_render::auto_correct`] and the solved Temperature, Tint and Exposure are written
//! into the clip's Lumetri Color Basic Correction (added when missing, keyframes untouched) or,
//! with `into: "raw"`, into the RAW development settings of a camera RAW clip through
//! `clip.sourceSettings` (`kelvin`, `tint`, `exposure`), falling back to Lumetri with a note when
//! the clip has no such controls. One undo step per clip; the result reports what was measured,
//! what was set and how confident the measurement is, so a script can batch-correct a bin and
//! skip the doubtful clips.

use filmcraft_project::{ClipId, ItemId, ParamValue, TrackKind};
use filmcraft_render::auto_correct::{self, Analysis, Options};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::commands::{CommandSpec, bad, bool_p, has_seq, str_p, u64_p};
use crate::{EngineError, Result, Session};

const CMD: &str = "lumetri.auto";
const LABEL: &str = "Auto Correct";

/// Frames spread over a clip at most (`samples`).
pub const MAX_SAMPLES: u64 = 16;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![CommandSpec {
        id: CMD,
        label: LABEL,
        menu: &["Color"],
        shortcut: None,
        params: r#"{"clips":[id]?|"clip":id?|"items":[id]?,"frames":"playhead"|"sampled"="playhead","samples":n=5,"whiteBalance":bool=true,"exposure":bool=true,"into":"lumetri"|"raw"="lumetri","apply":bool=true,"targetGrey":f32=0.18,"highlightCeiling":f32=0.9}"#,
        enabled: has_targets,
        run: auto,
        journal: true,
    }]
}

/// A sequence whose timeline or Project panel selection (or the clips / items named, see
/// `Session::execute`) reaches a video clip, as [`targets`] finds them: an audio-only selection
/// leaves the command disabled.
fn has_targets(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    targets(s, &Value::Null).map(|_| ()).map_err(|_| {
        if s.state.selection.is_empty() && s.state.project_selection.is_empty() {
            "select video clips (timeline or Project panel)".into()
        } else {
            "not a video clip: select video clips, or Project items they use".into()
        }
    })
}

#[derive(Clone, Copy, PartialEq)]
enum Into {
    Lumetri,
    Raw,
}

#[derive(Clone, Copy)]
struct Config {
    /// `None`: the frame at the playhead; `Some(n)`: `n` frames spread over the clip.
    samples: Option<u64>,
    options: Options,
    into: Into,
    apply: bool,
}

fn config(p: &Value) -> Result<Config> {
    let samples = match str_p(p, "frames").unwrap_or("playhead") {
        "playhead" => None,
        "sampled" => Some(u64_p(p, "samples").unwrap_or(5).clamp(1, MAX_SAMPLES)),
        o => return Err(bad(CMD, format!("unknown frames `{o}` (playhead, sampled)"))),
    };
    let into = match str_p(p, "into").unwrap_or("lumetri") {
        "lumetri" => Into::Lumetri,
        "raw" => Into::Raw,
        o => return Err(bad(CMD, format!("unknown into `{o}` (lumetri, raw)"))),
    };
    let f = |k: &str, d: f32| p.get(k).and_then(Value::as_f64).map(|v| v as f32).unwrap_or(d);
    let options = Options {
        white_balance: bool_p(p, "whiteBalance").unwrap_or(true),
        exposure: bool_p(p, "exposure").unwrap_or(true),
        target_grey: f("targetGrey", 0.18),
        highlight_ceiling: f("highlightCeiling", 0.9),
    }
    .sanitized();
    if !options.white_balance && !options.exposure {
        return Err(bad(CMD, "nothing to do: whiteBalance and exposure are both off"));
    }
    Ok(Config { samples, options, into, apply: bool_p(p, "apply").unwrap_or(true) })
}

/// The timeline clips to correct: `clips` / `clip`, else every video clip of the active sequence
/// whose media is one of `items` (subclips and the Project panel selection count), else the
/// timeline selection's video clips.
fn targets(s: &Session, p: &Value) -> Result<Vec<ClipId>> {
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    if let Some(a) = p.get("clips").and_then(Value::as_array) {
        let v: Vec<ClipId> = a.iter().filter_map(Value::as_u64).map(ClipId).collect();
        if v.is_empty() {
            return Err(bad(CMD, "`clips` names no clip"));
        }
        return Ok(v);
    }
    if let Some(c) = u64_p(p, "clip") {
        return Ok(vec![ClipId(c)]);
    }
    let items: Option<Vec<ItemId>> = match p.get("items").and_then(Value::as_array) {
        Some(a) => Some(a.iter().filter_map(Value::as_u64).map(ItemId).collect()),
        None if s.state.selection.is_empty() && !s.state.project_selection.is_empty() => Some(s.state.project_selection.clone()),
        None => None,
    };
    if let Some(items) = items {
        let root = |i: ItemId| s.project.resolve_media(i).map(|(r, _, _)| r);
        let roots: Vec<ItemId> = items.iter().filter_map(|i| root(*i)).collect();
        let v: Vec<ClipId> = q
            .video_tracks
            .iter()
            .flat_map(|t| t.items.iter())
            .filter(|it| items.contains(&it.item) || root(it.item).is_some_and(|r| roots.contains(&r)))
            .map(|it| it.id)
            .collect();
        if v.is_empty() {
            return Err(bad(CMD, "none of the items is used by a video clip of the active sequence"));
        }
        return Ok(v);
    }
    // the selection's video clips: a linked clip's audio partner is selected with it
    let video: Vec<ClipId> =
        s.state.selection.iter().copied().filter(|c| q.find_item(*c).is_some_and(|(t, _)| q.track(t).is_some_and(|t| t.kind == TrackKind::Video))).collect();
    if video.is_empty() {
        return Err(bad(CMD, "select video clips, or pass `clips` or `items`"));
    }
    Ok(video)
}

fn auto(s: &mut Session, p: &Value) -> Result<Value> {
    let cfg = config(p)?;
    let clips = targets(s, p)?;
    let mut done = Vec::new();
    let mut errors = Vec::new();
    // media whose RAW development this batch already set: clips sharing it go to Lumetri
    let mut developed: Vec<ItemId> = Vec::new();
    for clip in clips {
        match correct_clip(s, clip, cfg, &mut developed) {
            Ok(v) => {
                errors.extend(sample_error(&v));
                done.push(v);
            }
            Err(e) => errors.push(json!({"clip": clip.0, "error": e.to_string()})),
        }
    }
    if done.is_empty()
        && let Some(first) = errors.first()
    {
        return Err(EngineError::Other(first["error"].as_str().unwrap_or("auto correct failed").to_string()));
    }
    Ok(json!({"corrected": done.len(), "items": done, "errors": errors}))
}

/// Render each of `times`: the frames that rendered, with their times, and the times that did not.
fn render_samples<T>(times: &[Tick], mut render: impl FnMut(Tick) -> Option<T>) -> (Vec<(Tick, T)>, Vec<Tick>) {
    let mut frames = Vec::new();
    let mut failed = Vec::new();
    for t in times {
        match render(*t) {
            Some(img) => frames.push((*t, img)),
            None => failed.push(*t),
        }
    }
    (frames, failed)
}

/// The `errors` entry for a clip measured from fewer frames than asked: the times that did not
/// render.
fn sample_error(v: &Value) -> Option<Value> {
    let failed = v.get("failedTimes")?.as_array().filter(|a| !a.is_empty())?;
    let measured = v.get("frames").and_then(Value::as_u64).unwrap_or(0);
    Some(json!({
        "clip": v.get("clip"),
        "error": format!("{} sampled frame(s) did not render; measured from the other {measured}", failed.len()),
        "times": failed,
    }))
}

/// Where the clip's frames are measured: the playhead clamped into the clip, or `n` frames
/// spread evenly over it (never its very last frame, which may be a transition's tail).
fn sample_times(start: Tick, end: Tick, playhead: Tick, samples: Option<u64>) -> Vec<Tick> {
    let last = Tick(end.0.saturating_sub(1)).max(start);
    let Some(n) = samples else { return vec![playhead.clamp(start, last)] };
    let n = n.clamp(1, MAX_SAMPLES);
    let dur = i128::from(end.0.saturating_sub(start.0));
    (0..n)
        .map(|k| {
            let off = dur * (2 * i128::from(k) + 1) / (2 * i128::from(n));
            Tick(start.0.saturating_add(i64::try_from(off).unwrap_or(i64::MAX))).clamp(start, last)
        })
        .collect()
}

fn correct_clip(s: &mut Session, clip: ClipId, cfg: Config, developed: &mut Vec<ItemId>) -> Result<Value> {
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let (tid, it) = q.find_item(clip).ok_or_else(|| bad(CMD, format!("no clip {} in the active sequence", clip.0)))?;
    if q.track(tid).map(|t| t.kind) != Some(TrackKind::Video) {
        return Err(bad(CMD, format!("clip {} is not a video clip (it has no picture to measure)", clip.0)));
    }
    let (start, end, item) = (it.start, it.end(), it.item);
    let times = sample_times(start, end, s.playhead(), cfg.samples);
    // the source as Lumetri sees it: the clip with its Lumetri switched off, so running the
    // command again measures the same picture
    let mut probe = (*s.project).clone();
    if let Some((_, pi)) = probe.sequence_mut(seq_id).and_then(|q| q.find_item_mut(clip)) {
        for e in pi.effects.iter_mut().filter(|e| e.effect == "lumetri") {
            e.enabled = false;
        }
    }
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: 0.25, working_output: true, ..Default::default() };
    let (frames, failed) = render_samples(&times, |t| {
        filmcraft_render::render_clip(&probe, seq_id, clip, t, opts, &provider).map(|img| filmcraft_render::color_match::shrink(&img, 192))
    });
    if frames.is_empty() {
        return Err(bad(CMD, format!("clip {} has no picture", clip.0)));
    }
    let refs: Vec<&filmcraft_render::Image> = frames.iter().map(|(_, img)| img).collect();
    let a = auto_correct::analyse(&refs, cfg.options);
    let mut out = json!({
        "clip": clip.0,
        "item": item.0,
        "frames": frames.len(),
        "times": frames.iter().map(|(t, _)| t.0).collect::<Vec<_>>(),
        "measured": a,
        "confidence": a.confidence,
        "applied": false,
    });
    if !failed.is_empty() {
        out["failedTimes"] = json!(failed.iter().map(|t| t.0).collect::<Vec<_>>());
    }
    if !cfg.apply {
        return Ok(out);
    }
    if a.pixels == 0 {
        out["note"] = json!("no opaque pixels to measure; nothing was set");
        return Ok(out);
    }
    let n0 = s.history.undo.len();
    let mut into = cfg.into;
    let root = s.project.resolve_media(item).map(|(r, _, _)| r);
    if into == Into::Raw && root.is_some_and(|r| developed.contains(&r)) {
        // the development is the media's, not the clip's: a second write would undo the first
        // clip's correction while it still reports it applied
        out["note"] = json!("an earlier clip of this batch set this media's RAW development; Lumetri Basic Correction was set instead");
        into = Into::Lumetri;
    }
    if into == Into::Raw {
        match raw_develop(s, item, &a, cfg.options) {
            Ok(Some(set)) => {
                out["set"] = set;
                developed.extend(root);
            }
            Ok(None) => {
                out["note"] = json!("this clip has no RAW development controls in this build; Lumetri Basic Correction was set instead");
                into = Into::Lumetri;
            }
            Err(e) => {
                out["note"] = json!(format!("RAW development not available ({e}); Lumetri Basic Correction was set instead"));
                into = Into::Lumetri;
            }
        }
    }
    if into == Into::Lumetri {
        match write_lumetri(s, clip, &a, cfg.options) {
            Ok(set) => out["set"] = set,
            Err(e) => {
                while s.history.undo.len() > n0 {
                    s.undo();
                }
                s.history.redo.clear();
                return Err(e);
            }
        }
    }
    crate::clip_ops::collapse_history(s, n0, LABEL);
    out["applied"] = json!(true);
    Ok(out)
}

/// Temperature, Tint and Exposure into the clip's Lumetri (added when missing); its Basic
/// Correction section and the effect are switched on. Keyframes stay: a keyframed parameter keeps
/// them and gets the new static value, and the result names it.
fn write_lumetri(s: &mut Session, clip: ClipId, a: &Analysis, o: Options) -> Result<Value> {
    let (clip, idx) = crate::color::lumetri_clip(s, &json!({"clip": clip.0}), CMD)?;
    let mut values: Vec<(&str, ParamValue)> = vec![("basic_on", ParamValue::Bool(true))];
    if o.white_balance {
        values.push(("temperature", ParamValue::Float(f64::from(a.temperature))));
        values.push(("tint", ParamValue::Float(f64::from(a.tint))));
    }
    if o.exposure {
        values.push(("exposure", ParamValue::Float(f64::from(a.exposure))));
    }
    let mut keyframed = Vec::new();
    s.edit_sequence(LABEL, |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let e = it.effects.get_mut(idx).ok_or_else(|| bad(CMD, "no Lumetri"))?;
        for (k, v) in &values {
            let prm = e.params.entry((*k).to_string()).or_insert_with(|| filmcraft_project::Param::new(v.clone()));
            if !prm.keyframes.is_empty() {
                keyframed.push((*k).to_string());
            }
            prm.value = v.clone();
        }
        e.enabled = true;
        Ok(())
    })?;
    let mut set = json!({"into": "lumetri", "clip": clip.0, "effect": idx});
    if o.white_balance {
        set["temperature"] = json!(a.temperature);
        set["tint"] = json!(a.tint);
    }
    if o.exposure {
        set["exposure"] = json!(a.exposure);
    }
    if !keyframed.is_empty() {
        set["keyframed"] = json!(keyframed);
    }
    Ok(set)
}

/// The RAW development route: `clip.sourceSettings {items: [media], kelvin, tint, exposure}` on
/// the clip's media item. `Ok(None)` when the media has no such controls (the command answered
/// without a `kelvin` setting), so the caller falls back to Lumetri.
fn raw_develop(s: &mut Session, item: ItemId, a: &Analysis, o: Options) -> Result<Option<Value>> {
    let root = s.project.resolve_media(item).map(|(r, _, _)| r).ok_or_else(|| bad(CMD, "the clip has no media"))?;
    let current = s.execute("clip.sourceSettings", json!({"items": [root.0]})).ok();
    let offers = |v: &Value| v.pointer("/develop/kelvin").is_some() || v["settings"].as_array().is_some_and(|a| a.iter().any(|x| x["id"] == "kelvin"));
    if !current.as_ref().is_some_and(offers) {
        return Ok(None);
    }
    let k0 = current.as_ref().and_then(|v| v.pointer("/develop/kelvin")).and_then(Value::as_f64).unwrap_or(5600.0) as f32;
    let kelvin = auto_correct::kelvin_for_cast(a.cast, k0);
    let mut p = json!({"items": [root.0]});
    let mut set = json!({"into": "raw", "item": root.0});
    if o.white_balance {
        p["kelvin"] = json!(kelvin);
        p["tint"] = json!(a.tint);
        set["kelvin"] = json!(kelvin);
        set["tint"] = json!(a.tint);
    }
    if o.exposure {
        p["exposure"] = json!(a.exposure);
        set["exposure"] = json!(a.exposure);
    }
    let r = s.execute("clip.sourceSettings", p)?;
    if !offers(&r) {
        return Ok(None);
    }
    Ok(Some(set))
}

#[cfg(test)]
#[path = "color_auto_tests.rs"]
mod tests;

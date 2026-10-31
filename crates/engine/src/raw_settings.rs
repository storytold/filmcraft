//! Camera raw Source Settings (Clip ▸ Source Settings…): how DNG footage (stills, CinemaDNG
//! folders, CinemaDNG in MXF) is developed, per media item. Behaviour follows Premiere's Source
//! Settings for camera raw formats as Adobe's public help describes it: the settings belong to
//! the source, so every clip of the item changes, and they are one undoable change.
//!
//! - `clip.sourceSettings {items?|item?, whiteBalance?, temperature?, tint?, exposure?,
//!   highlights?, demosaic?, output?, reset?}`: change the settings of the camera raw items
//!   addressed (default: the Project panel selection, else the media of the selected timeline
//!   clips; subclips address their media). Values left out keep each item's current one.
//!   `temperature` / `tint` set a custom white balance; `whiteBalance: "asShot"` goes back to the
//!   camera's. `reset: true` develops as the file says. Without any setting it changes nothing
//!   and describes the first item (as `media.rawInfo`; for other footage, why it has no source
//!   settings), which is what the dialog shows.
//! - `media.rawInfo {item}`: whether the item is camera raw, its settings, and the file's as-shot
//!   white balance as temperature and tint (the starting point of a custom one).
//!
//! The settings are stored in the item's `Interpretation::raw` (None = as the file says); the
//! media pool opens the item again with them ([`crate::media_pool::media_key`]).

use filmcraft_media::raw::{self, DemosaicQuality, Highlights, OutputSpace, RawSettings, WhiteBalance};
use filmcraft_project::{ItemId, ItemKind};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, f64_p, item_p, str_p};
use crate::{Result, Session};

const CMD: &str = "clip.sourceSettings";

/// The parameters that change settings (any of them makes the command an edit).
const SETTING_KEYS: [&str; 8] = ["whiteBalance", "temperature", "tint", "exposure", "highlights", "demosaic", "output", "reset"];

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        CommandSpec {
            id: CMD,
            label: "Source Settings…",
            menu: &["Clip"],
            shortcut: None,
            params: r#"{"items":[id]?,"item":id?,"whiteBalance"":"asShot"|"custom"?,"temperature":kelvin?,"tint":n?,"exposure":stops?,"highlights":"clip"?,"demosaic":"fast"|"high"?,"output":"sceneLinearRec709"?,"reset":bool?}"#,
            enabled: has_footage,
            run: source_settings,
            journal: true,
        },
        CommandSpec {
            id: "media.rawInfo",
            label: "Camera Raw Info",
            menu: &[],
            shortcut: None,
            params: r#"{"item":id}"#,
            enabled: always,
            run: raw_info,
            journal: false,
        },
    ]
}

/// Whether a media item is camera raw footage FilmCraft develops.
fn is_raw(s: &Session, id: ItemId) -> bool {
    s.project.item(id).and_then(|i| i.as_media()).is_some_and(|m| filmcraft_media::still::dng::is_camera_raw(&m.info))
}

/// The media items addressed by `p` (`items`, `item`, else the Project panel selection, else the
/// selected timeline clips), subclips resolved to their media, without duplicates.
fn footage(s: &Session, p: &Value) -> Vec<ItemId> {
    let mut v = Vec::new();
    for i in crate::project_tools::targets(s, p) {
        if let Some((root, _, _)) = s.project.resolve_media(i)
            && !v.contains(&root)
        {
            v.push(root);
        }
    }
    v
}

/// Enabled with footage selected; for footage that is not camera raw the dialog says why it has
/// no source settings.
fn has_footage(s: &Session) -> std::result::Result<(), String> {
    if footage(s, &Value::Null).is_empty() { Err("select a clip".into()) } else { Ok(()) }
}

/// The settings as JSON (the command's parameter names).
pub fn settings_json(r: &RawSettings) -> Value {
    let (wb, temperature, tint) = match r.white_balance {
        WhiteBalance::AsShot => ("asShot", None, None),
        WhiteBalance::Custom { temperature, tint } => ("custom", Some(temperature), Some(tint)),
    };
    json!({
        "whiteBalance": wb,
        "temperature": temperature,
        "tint": tint,
        "exposure": r.exposure,
        "highlights": match r.highlights { Highlights::Clip => "clip", Highlights::Recover => "recover" },
        "demosaic": match r.demosaic { DemosaicQuality::Fast => "fast", DemosaicQuality::High => "high" },
        "output": match r.output { OutputSpace::SceneLinearRec709 => "sceneLinearRec709" },
    })
}

/// A finite number parameter (None when absent; an error when present but not a number).
fn num(p: &Value, k: &str) -> Result<Option<f64>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(_) => f64_p(p, k).filter(|v| v.is_finite()).map(Some).ok_or_else(|| bad(CMD, format!("`{k}` must be a number"))),
    }
}

/// A string parameter (None when absent; an error when present but not a string).
fn word<'a>(p: &'a Value, k: &str) -> Result<Option<&'a str>> {
    match p.get(k) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s)),
        Some(_) => Err(bad(CMD, format!("`{k}` must be a string"))),
    }
}

/// `cur` with the parameters of `p` applied.
fn apply(cur: RawSettings, p: &Value) -> Result<RawSettings> {
    let mut r = cur;
    for k in ["whiteBalance", "highlights", "demosaic", "output"] {
        word(p, k)?;
    }
    if p.get("reset").is_some_and(|v| !v.is_boolean()) {
        return Err(bad(CMD, "`reset` must be true or false"));
    }
    let (temperature, tint) = (num(p, "temperature")?, num(p, "tint")?);
    match str_p(p, "whiteBalance") {
        None => {}
        Some("asShot") => {
            if temperature.is_some() || tint.is_some() {
                return Err(bad(CMD, "`temperature` / `tint` need `whiteBalance: \"custom\"` (or no `whiteBalance`)"));
            }
            r.white_balance = WhiteBalance::AsShot;
        }
        Some("custom") => {
            if temperature.is_none() && !matches!(r.white_balance, WhiteBalance::Custom { .. }) {
                return Err(bad(CMD, "a custom white balance needs `temperature` (kelvin)"));
            }
        }
        Some(o) => return Err(bad(CMD, format!("whiteBalance must be \"asShot\" or \"custom\", got `{o}`"))),
    }
    if temperature.is_some() || tint.is_some() {
        let (t0, n0) = match r.white_balance {
            WhiteBalance::Custom { temperature, tint } => (Some(temperature), tint),
            WhiteBalance::AsShot => (None, 0.0),
        };
        let t = temperature.or(t0).ok_or_else(|| bad(CMD, "a custom white balance needs `temperature` (kelvin)"))?;
        r.white_balance = WhiteBalance::Custom { temperature: t, tint: tint.unwrap_or(n0) };
    }
    if let Some(e) = num(p, "exposure")? {
        r.exposure = e;
    }
    match str_p(p, "highlights") {
        None => {}
        Some("clip") => r.highlights = Highlights::Clip,
        // the decoder accepts Recover but does not reconstruct highlights yet: refuse it rather
        // than store a setting that changes nothing
        Some("recover") => return Err(bad(CMD, "highlight recovery is not available yet (highlights are clipped)")),
        Some(o) => return Err(bad(CMD, format!("highlights must be \"clip\", got `{o}`"))),
    }
    match str_p(p, "demosaic") {
        None => {}
        Some("fast") => r.demosaic = DemosaicQuality::Fast,
        Some("high") => r.demosaic = DemosaicQuality::High,
        Some(o) => return Err(bad(CMD, format!("demosaic must be \"fast\" or \"high\", got `{o}`"))),
    }
    match str_p(p, "output") {
        None | Some("sceneLinearRec709") => {}
        Some(o) => return Err(bad(CMD, format!("output colour space must be \"sceneLinearRec709\" (scene-linear Rec. 709), got `{o}`"))),
    }
    r.validate().map_err(|e| bad(CMD, e))?;
    Ok(r)
}

fn source_settings(s: &mut Session, p: &Value) -> Result<Value> {
    if !p.is_object() && !p.is_null() {
        return Err(bad(CMD, "parameters must be an object"));
    }
    let all = footage(s, p);
    if !SETTING_KEYS.iter().any(|k| p.get(*k).is_some()) {
        // describe the first item
        let first = all.first().copied().ok_or_else(|| bad(CMD, "select a clip"))?;
        return describe(s, first);
    }
    if all.is_empty() {
        return Err(bad(CMD, "select a clip"));
    }
    let items: Vec<ItemId> = all.into_iter().filter(|i| is_raw(s, *i)).collect();
    if items.is_empty() {
        return Err(bad(CMD, "no camera raw footage selected: source settings apply to DNG stills, CinemaDNG folders and CinemaDNG MXF"));
    }
    let reset = bool_p(p, "reset").unwrap_or(false);
    // check every item's result before changing any
    let mut new = Vec::with_capacity(items.len());
    for i in &items {
        let cur = s.project.item(*i).and_then(|it| it.as_media()).and_then(|m| m.interpret.raw).unwrap_or_default();
        let r = if reset { RawSettings::default() } else { apply(cur, p)? };
        new.push((*i, (r != RawSettings::default()).then_some(r)));
    }
    s.edit("Source Settings", |pr, _| {
        for (i, r) in &new {
            if let Some(ItemKind::Media(m)) = pr.item_mut(*i).map(|it| &mut it.kind) {
                m.interpret.raw = *r;
            }
        }
        Ok(())
    })?;
    let first = new.first().and_then(|(_, r)| *r).unwrap_or_default();
    Ok(json!({"items": items.iter().map(|i| i.0).collect::<Vec<_>>(), "settings": settings_json(&first)}))
}

fn raw_info(s: &mut Session, p: &Value) -> Result<Value> {
    let id = item_p(p, "item").ok_or_else(|| bad("media.rawInfo", "need `item`"))?;
    let Some((root, _, _)) = s.project.resolve_media(id) else { return Err(bad("media.rawInfo", "not a media item")) };
    describe(s, root)
}

/// The Source Settings of media item `id`: its codec and container, whether it is camera raw, and
/// for camera raw its settings, the file's as-shot white balance and the value ranges.
fn describe(s: &Session, id: ItemId) -> Result<Value> {
    let Some(m) = s.project.item(id).and_then(|i| i.as_media()) else { return Err(bad(CMD, "not a media item")) };
    let codec = m.info.video.as_ref().map(|v| v.codec.clone()).or_else(|| m.info.audio().map(|a| a.codec.clone())).unwrap_or_default();
    if !filmcraft_media::still::dng::is_camera_raw(&m.info) {
        let what = if codec.is_empty() { "This clip".to_string() } else { codec.to_uppercase() };
        return Ok(json!({
            "item": id.0,
            "raw": false,
            "codec": codec,
            "container": m.info.container,
            "settings": null,
            "message": format!("{what} has no source settings in FilmCraft. Source settings apply to camera raw formats; FilmCraft develops DNG and CinemaDNG (folders of DNG frames and MXF). R3D, ARRIRAW, BRAW and ProRes RAW are not decoded."),
        }));
    }
    let pixel_format = m.info.video.as_ref().map(|v| v.pixel_format.clone()).unwrap_or_default();
    let settings = m.interpret.raw.unwrap_or_default();
    // the as-shot white comes from the file (None when it is offline or has no colour calibration)
    let as_shot = s.media.full_res_source(&s.project, id, &*s.services).and_then(|src| src.raw_info()).and_then(|r| r.as_shot);
    Ok(json!({
        "item": id.0,
        "raw": true,
        "codec": codec,
        "container": m.info.container,
        "pixelFormat": pixel_format,
        "settings": settings_json(&settings),
        "asShot": as_shot.map(|(t, n)| json!({"temperature": t, "tint": n})),
        "ranges": {
            "temperature": [raw::TEMPERATURE_RANGE.0, raw::TEMPERATURE_RANGE.1],
            "tint": [raw::TINT_RANGE.0, raw::TINT_RANGE.1],
            "exposure": [-raw::MAX_EXPOSURE, raw::MAX_EXPOSURE],
        },
    }))
}

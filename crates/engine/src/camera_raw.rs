//! Camera RAW development: Clip ▸ Source Settings… (`clip.sourceSettings`) and File ▸ Project
//! Settings ▸ Camera RAW (`file.projectSettings.cameraRaw`), modelled on a RED Camera Raw panel.
//!
//! A RAW clip decodes with the project's Camera RAW settings (Decode Using ▸ Project, what a clip
//! without settings of its own does) or with its own (Decode Using ▸ Clip). Both are
//! [`RawDevelop`]s: decode quality, colour science (IPP2), colour space and gamma curve, output
//! tone map, highlight rolloff and HDR peak, chroma noise reduction, flashing pixel adjustment,
//! the clip's metadata curves, creative LUT and CDL, and ISO, colour temperature, tint and
//! exposure adjustment. The decoder hands over linear light in the developed primaries, and the
//! compositor interprets it through [`filmcraft_project::Project::color_space_override`], so
//! nothing about the colour pipeline is stored per clip.

use filmcraft_media::{
    DecodeUsing, Hundredths, RAW_EXPOSURE_RANGE, RAW_HDR_PEAK_RANGE, RAW_ISO_VALUES, RAW_KELVIN_RANGE, RAW_TINT_RANGE, RawClipInfo, RawColorSpace, RawDevelop,
    RawFlashing, RawGamma, RawOutput, RawQuality, RawRollOff, RawToneMap,
};
use filmcraft_project::{ItemId, ItemKind, MediaClip};
use serde_json::{Value, json};

use crate::commands::bad;
use crate::project_tools::{media_of, targets};
use crate::{EngineError, Result, Session};

/// A named choice parameter: absent is None, anything but one of the names is an error.
pub(crate) fn choice_p<T: Copy>(p: &Value, key: &str, all: &[T], id: fn(T) -> &'static str, cmd: &str) -> Result<Option<T>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(v) => {
            let names: Vec<&str> = all.iter().map(|t| id(*t)).collect();
            let name = v.as_str().ok_or_else(|| bad(cmd, format!("{key} must be one of {}", names.join(", "))))?;
            all.iter()
                .copied()
                .find(|t| id(*t).eq_ignore_ascii_case(name))
                .map(Some)
                .ok_or_else(|| bad(cmd, format!("{key} must be one of {}, not `{name}`", names.join(", "))))
        }
    }
}

/// A setting with a recorded value: absent leaves it alone, `null` sets it back to the clip's
/// recorded value (as shot), anything else sets it.
#[derive(Clone, Copy)]
enum Adjust<T> {
    Keep,
    AsShot,
    Set(T),
}

impl<T: Copy> Adjust<T> {
    fn apply(&self, field: &mut Option<T>) {
        match self {
            Adjust::Keep => {}
            Adjust::AsShot => *field = None,
            Adjust::Set(v) => *field = Some(*v),
        }
    }
    fn given(&self) -> bool {
        !matches!(self, Adjust::Keep)
    }
}

fn adjust_p<T>(p: &Value, key: &str, cmd: &str, what: &str, parse: impl Fn(&Value) -> Option<T>) -> Result<Adjust<T>> {
    match p.get(key) {
        None => Ok(Adjust::Keep),
        Some(Value::Null) => Ok(Adjust::AsShot),
        Some(v) => parse(v).map(Adjust::Set).ok_or_else(|| bad(cmd, format!("{key} must be {what}, or null for as recorded"))),
    }
}

fn flag_p(p: &Value, key: &str, cmd: &str) -> Result<Option<bool>> {
    match p.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(Value::Bool(b)) => Ok(Some(*b)),
        Some(_) => Err(bad(cmd, format!("{key} must be true or false"))),
    }
}

/// The development changes one call asks for (every field optional).
struct Change {
    decode_using: Option<DecodeUsing>,
    /// Some(None) = the project's decode quality.
    quality: Option<Option<RawQuality>>,
    color_space: Option<RawColorSpace>,
    gamma: Option<RawGamma>,
    tone_map: Option<RawToneMap>,
    rolloff: Option<RawRollOff>,
    hdr_peak: Adjust<u32>,
    chroma: Adjust<bool>,
    flashing: Adjust<RawFlashing>,
    metadata_curves: Option<bool>,
    creative_lut: Option<bool>,
    cdl: Option<bool>,
    as_shot: bool,
    iso: Adjust<u32>,
    kelvin: Adjust<u32>,
    tint: Adjust<Hundredths>,
    exposure: Adjust<Hundredths>,
}

impl Change {
    fn parse(p: &Value, cmd: &str) -> Result<Change> {
        let quality = match p.get("quality") {
            None | Some(Value::Null) => None,
            Some(Value::String(s)) if s.eq_ignore_ascii_case("project") => Some(None),
            Some(_) => choice_p(p, "quality", &RawQuality::ALL, RawQuality::id, cmd)?.map(Some),
        };
        // the first version's single output list maps onto colour space and gamma
        let output = choice_p(p, "output", &RawOutput::ALL, RawOutput::id, cmd)?.map(RawOutput::space_gamma);
        let whole = |v: &Value| v.as_u64().filter(|n| *n <= u64::from(u32::MAX)).map(|n| n as u32);
        let hundredths = |v: &Value| v.as_f64().and_then(Hundredths::from_f64);
        Ok(Change {
            decode_using: choice_p(p, "decodeUsing", &DecodeUsing::ALL, DecodeUsing::id, cmd)?,
            quality,
            color_space: choice_p(p, "colorSpace", &RawColorSpace::ALL, RawColorSpace::id, cmd)?.or(output.map(|o| o.0)),
            gamma: choice_p(p, "gamma", &RawGamma::ALL, RawGamma::id, cmd)?.or(output.map(|o| o.1)),
            tone_map: choice_p(p, "toneMap", &RawToneMap::ALL, RawToneMap::id, cmd)?,
            rolloff: choice_p(p, "rolloff", &RawRollOff::ALL, RawRollOff::id, cmd)?,
            hdr_peak: adjust_p(p, "hdrPeakNits", cmd, "a peak in cd/m²", whole)?,
            chroma: adjust_p(p, "chromaNoiseReduction", cmd, "true or false", Value::as_bool)?,
            flashing: match p.get("flashingPixel") {
                None => Adjust::Keep,
                Some(Value::Null) => Adjust::AsShot,
                Some(_) => choice_p(p, "flashingPixel", &RawFlashing::ALL, RawFlashing::id, cmd)?.map_or(Adjust::AsShot, Adjust::Set),
            },
            metadata_curves: flag_p(p, "metadataCurves", cmd)?,
            creative_lut: flag_p(p, "creativeLut", cmd)?,
            cdl: flag_p(p, "cdl", cmd)?,
            as_shot: flag_p(p, "asShot", cmd)?.unwrap_or(false),
            iso: adjust_p(p, "iso", cmd, "one of the decoder's ISO values", whole)?,
            kelvin: adjust_p(p, "kelvin", cmd, "a colour temperature in Kelvin", whole)?,
            tint: adjust_p(p, "tint", cmd, "a tint from -100 to 100", hundredths)?,
            exposure: adjust_p(p, "exposure", cmd, "an exposure adjustment in stops", hundredths)?,
        })
    }

    /// A development setting is given (anything but Decode Using and the decode quality).
    fn develops(&self) -> bool {
        self.color_space.is_some()
            || self.gamma.is_some()
            || self.tone_map.is_some()
            || self.rolloff.is_some()
            || self.hdr_peak.given()
            || self.chroma.given()
            || self.flashing.given()
            || self.metadata_curves.is_some()
            || self.creative_lut.is_some()
            || self.cdl.is_some()
            || self.as_shot
            || self.iso.given()
            || self.kelvin.given()
            || self.tint.given()
            || self.exposure.given()
    }

    fn any(&self) -> bool {
        self.develops() || self.decode_using.is_some() || self.quality.is_some()
    }

    /// Apply the development settings (not Decode Using or the quality) to `d`. A colour space
    /// or gamma given alone that does not go with the other one brings the other one along
    /// (REDWideGamutRGB → Log3G10, ST 2084 → Rec. 2020…); both given must fit together.
    fn develop(&self, d: &mut RawDevelop) {
        if let Some(c) = self.color_space {
            d.color_space = c;
        }
        if let Some(g) = self.gamma {
            d.gamma = g;
        }
        if !RawDevelop::supports(d.color_space, d.gamma) {
            match (self.color_space.is_some(), self.gamma.is_some()) {
                (true, false) => d.gamma = RawDevelop::gamma_for(d.color_space),
                (false, true) => d.color_space = RawDevelop::space_for(d.gamma),
                _ => {} // refused by validate
            }
        }
        if let Some(t) = self.tone_map {
            d.tone_map = t;
        }
        if let Some(r) = self.rolloff {
            d.rolloff = r;
        }
        self.hdr_peak.apply(&mut d.hdr_peak_nits);
        self.chroma.apply(&mut d.chroma_noise_reduction);
        self.flashing.apply(&mut d.flashing_pixel);
        if let Some(v) = self.metadata_curves {
            d.metadata_curves = v;
        }
        if let Some(v) = self.creative_lut {
            d.creative_lut = v;
        }
        if let Some(v) = self.cdl {
            d.cdl = v;
        }
        if self.as_shot {
            *d = d.with_as_shot();
        }
        self.iso.apply(&mut d.iso);
        self.kelvin.apply(&mut d.kelvin);
        self.tint.apply(&mut d.tint);
        self.exposure.apply(&mut d.exposure);
    }
}

/// A clip's own settings after `change`, under the project's `camera_raw`: Decode Using is the
/// one given, else Clip when a development setting is given, else what it was. A clip that
/// turns to Clip starts from the project's settings; one that decodes with the project's keeps
/// only its decode quality (None when it has none).
fn changed(camera_raw: &RawDevelop, own: Option<RawDevelop>, change: &Change) -> Option<RawDevelop> {
    let was = own.map_or(DecodeUsing::Project, |d| d.decode_using);
    let using = change.decode_using.unwrap_or(if change.develops() { DecodeUsing::Clip } else { was });
    let quality = change.quality.unwrap_or(own.and_then(|d| d.quality));
    match using {
        DecodeUsing::Project => quality.map(|q| RawDevelop { decode_using: DecodeUsing::Project, quality: Some(q), ..Default::default() }),
        DecodeUsing::Clip => {
            let mut d = match own {
                Some(d) if d.decode_using == DecodeUsing::Clip => d,
                _ => RawDevelop { decode_using: DecodeUsing::Clip, ..*camera_raw },
            };
            d.quality = quality;
            change.develop(&mut d);
            Some(d)
        }
    }
}

/// The settings that reproduce what the clip records (or its RMD sidecar says), as its decoder
/// reported it (`info`): Decode Using ▸ Clip, everything as recorded, the LUT and CDL as the clip
/// asks for them, and its recorded colour space and gamma when they are ones offered here (else
/// the ones in effect). The decode quality stays.
fn recorded(current: &RawDevelop, info: Option<&RawClipInfo>) -> RawDevelop {
    let (space, gamma) = match info.and_then(|i| i.color_space.zip(i.gamma)) {
        Some((s, g)) if RawDevelop::supports(s, g) => (s, g),
        _ => (current.color_space, current.gamma),
    };
    RawDevelop {
        decode_using: DecodeUsing::Clip,
        quality: current.quality,
        color_space: space,
        gamma,
        creative_lut: info.is_some_and(|i| i.lut && i.lut_enabled),
        cdl: info.is_some_and(|i| i.cdl_enabled),
        ..Default::default()
    }
}

/// The merge key every live preview of these clips shares, until a commit or a revert ends it.
fn merge_key(ids: &[ItemId]) -> String {
    format!("clip.sourceSettings:{}", ids.iter().map(|i| i.0.to_string()).collect::<Vec<_>>().join(","))
}

/// `clip.sourceSettings`: the Camera Raw panel of camera RAW clips (Clip ▸ Source Settings…).
/// Without parameters it reports the controls (`settings`, each with its id, kind, label, value,
/// options, whether it is enabled and why not, and the panel column it sits in) and their
/// values; with them it sets them on every selected RAW clip (one undo step) and reports the
/// result. Values outside the decoder's ranges, and a colour space and gamma that do not go
/// together, are refused before anything changes.
///
/// - `decodeUsing: clip | project`: the clip's own settings, or the project's Camera RAW
///   settings. A development setting given for a clip that uses the project's gives it settings
///   of its own, starting from the project's; `decodeUsing: project` drops them.
/// - `quality: project | fullPremium | halfPremium | halfGood | quarterGood | eighthGood |
///   sixteenthGood`: the decode quality (the largest decode a preview uses).
/// - `colorSpace`, `gamma` (and the first version's `output`), `toneMap`, `rolloff`,
///   `hdrPeakNits`, `chromaNoiseReduction`, `flashingPixel`, `metadataCurves`, `creativeLut`,
///   `cdl`, `iso`, `kelvin`, `tint`, `exposure` (`null`: as recorded; `asShot: true` puts the
///   four RAW adjustments back).
/// - `useChanges: true` copies the first clip's settings (after this call's changes) to every
///   other clip in `items`; `useSettings: true` puts every clip back to what it records (or its
///   RMD sidecar says); both in the same undo step as the changes.
/// - `updateRmd: true` writes every clip's settings into its RMD sidecar beside the clip
///   (creating or updating it; not an undo step: it changes a file), after the changes; the
///   decoder then reads them as the clip's recorded settings.
///
/// Live preview, as the dialog does it: `preview: true` applies the call at once (the open decoder
/// develops the current frame with it) as a merged undo step, so every preview of the same clips
/// until the dialog closes is one step; `commit: true` (with or without settings, which it applies
/// first) ends that step; `revert: true` takes the whole step back, leaving the project as it was
/// before the first preview. The result's `preview` says whether such a step is open; a revert
/// reports `reverted`. An undo or any other edit in between ends the step as well. Other media
/// report that they have no source settings.
pub(crate) fn source_settings(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "clip.sourceSettings";
    let mut ids: Vec<ItemId> = Vec::new();
    for i in targets(s, p) {
        if let Some((root, _)) = media_of(&s.project, i)
            && !ids.contains(&root)
        {
            ids.push(root);
        }
    }
    // the first RAW clip speaks for the selection; a mixed selection develops its RAW clips only
    let id = *ids
        .iter()
        .find(|i| media_of(&s.project, **i).is_some_and(|(_, m)| crate::media_pool::is_camera_raw(m)))
        .or_else(|| ids.first())
        .ok_or_else(|| EngineError::Other("select a clip".into()))?;
    let (_, m) = media_of(&s.project, id).ok_or_else(|| EngineError::Other("select a clip".into()))?;
    let codec = m.info.video.as_ref().map(|v| v.codec.clone()).or_else(|| m.info.audio().map(|a| a.codec.clone())).unwrap_or_default();
    let container = m.info.container.clone();
    let developable = crate::media_pool::is_camera_raw(m);
    let change = Change::parse(p, CMD)?;
    let flag = |key: &str| flag_p(p, key, CMD).map(|v| v.unwrap_or(false));
    let (preview, commit, revert) = (flag("preview")?, flag("commit")?, flag("revert")?);
    let (use_changes, use_settings, update_rmd) = (flag("useChanges")?, flag("useSettings")?, flag("updateRmd")?);
    let key = merge_key(&ids);
    let changing = change.any() || use_changes || use_settings;
    if revert && (changing || preview || commit || update_rmd) {
        return Err(bad(CMD, "revert takes no settings, preview, commit or RMD update"));
    }
    let reverted = revert && s.revert_merged(&key);
    if !developable {
        let name = if codec.is_empty() { "This clip".to_string() } else { codec.to_uppercase() };
        if changing || update_rmd {
            return Err(bad(CMD, format!("{name} has no source settings")));
        }
        return Ok(json!({
            "item": id.0,
            "items": ids.iter().map(|i| i.0).collect::<Vec<_>>(),
            "codec": codec,
            "container": container,
            "settings": [],
            "message": format!("{name} has no source settings controls in FilmCraft. Supported camera RAW clips use their recorded settings; use timeline color effects for grading."),
        }));
    }
    if changing {
        let camera_raw = s.project.settings.camera_raw;
        let raw_ids: Vec<ItemId> = ids.iter().copied().filter(|i| media_of(&s.project, *i).is_some_and(|(_, m)| crate::media_pool::is_camera_raw(m))).collect();
        // what each clip records, for Use Settings (a clip whose decoder is not open has nothing
        // to report: its LUT and CDL stay off, its colour space and gamma as they are)
        let infos: Vec<Option<RawClipInfo>> =
            raw_ids.iter().map(|i| if use_settings { s.source(*i).and_then(|src| src.raw_clip_info()) } else { None }).collect();
        // every clip's resulting settings, worked out and checked before anything changes
        let mut next: Vec<(ItemId, Option<RawDevelop>)> = Vec::new();
        for (i, info) in raw_ids.iter().zip(&infos) {
            let Some((_, m)) = media_of(&s.project, *i) else { continue };
            let mut own = changed(&camera_raw, m.interpret.develop, &change);
            if use_settings {
                let current = RawDevelop::effective(&camera_raw, own.as_ref());
                own = Some(recorded(&current, info.as_ref()));
            }
            next.push((*i, own));
        }
        if use_changes && let Some(first) = next.first().map(|n| n.1) {
            for n in next.iter_mut().skip(1) {
                n.1 = first;
            }
        }
        for (_, own) in &next {
            RawDevelop::effective(&camera_raw, own.as_ref()).validate().map_err(|e| bad(CMD, e))?;
        }
        let body = |pr: &mut filmcraft_project::Project, _: &mut crate::EditorState| {
            for (i, own) in &next {
                let Some(ItemKind::Media(m)) = pr.item_mut(*i).map(|it| &mut it.kind) else { continue };
                set_own(m, &camera_raw, *own);
            }
            Ok(())
        };
        if preview || commit {
            // a live preview, or the dialog's OK after one: the same undo step as the previews
            s.edit_merged("Source Settings", &key, body)?;
        } else {
            s.edit("Source Settings", body)?;
        }
    }
    if commit {
        s.end_merged(&key);
    }
    let mut written = Vec::new();
    let mut rmd_errors: Vec<String> = Vec::new();
    if update_rmd {
        let mut errors = Vec::new();
        for i in &ids {
            let Some((_, m)) = media_of(&s.project, *i) else { continue };
            let Some(d) = crate::media_pool::develop_of(&s.project.settings.camera_raw, m) else { continue };
            let name = s.project.item(*i).map(|it| it.name.clone()).unwrap_or_default();
            match s.source(*i).ok_or_else(|| "its media is not open".to_string()).and_then(|src| src.write_raw_sidecar(&d).map_err(|e| e.to_string())) {
                Ok(path) => written.push(json!({"item": i.0, "path": path})),
                Err(e) => errors.push(format!("{name}: {e}")),
            }
        }
        if written.is_empty() && !errors.is_empty() {
            return Err(EngineError::Other(format!("RMD not written: {}", errors.join("; "))));
        }
        rmd_errors = errors;
    }
    let mut r = report(s, id, &ids)?;
    r["preview"] = json!(s.history.merge_key.as_deref() == Some(key.as_str()));
    r["reverted"] = json!(reverted);
    if update_rmd {
        r["rmdWritten"] = json!(written);
        r["rmdErrors"] = json!(rmd_errors);
    }
    Ok(r)
}

/// Store a clip's own settings and bring what depends on them in line: a colour space
/// interpretation the first version of Source Settings stored for its log output (or one that
/// the development now decides) gives way to the derived one, and the stream reports the linear
/// light the decoder hands over.
fn set_own(m: &mut MediaClip, camera_raw: &RawDevelop, own: Option<RawDevelop>) {
    let before = RawDevelop::effective(camera_raw, m.interpret.develop.as_ref());
    m.interpret.develop = own;
    let after = RawDevelop::effective(camera_raw, m.interpret.develop.as_ref());
    sync_derived(m, &before, &after);
}

/// Bring what depends on a clip's development in line after it changed from `before` to `after`
/// (its own settings, or the project's for a clip that follows them).
fn sync_derived(m: &mut MediaClip, before: &RawDevelop, after: &RawDevelop) {
    let output_changed = (before.color_space, before.gamma) != (after.color_space, after.gamma);
    if let Some(cs) = m.interpret.color_space
        && RawDevelop::INTERPRETATIONS.contains(&cs)
        && (output_changed || cs == filmcraft_color::ColorSpace::Log3G10Rwg)
    {
        m.interpret.color_space = None;
    }
    if let Some(v) = m.info.video.as_mut() {
        v.color.transfer = filmcraft_color::Transfer::Linear;
        v.hdr = (after.gamma == RawGamma::St2084)
            .then_some(after.hdr_peak_nits)
            .flatten()
            .map(|nits| filmcraft_color::HdrMetadata { mastering_max_nits: Some(nits as f32), ..Default::default() });
    }
}

fn options<T: Copy>(all: &[T], id: fn(T) -> &'static str, label: fn(T) -> &'static str) -> Vec<Value> {
    all.iter().map(|t| json!({"id": id(*t), "label": label(*t)})).collect()
}

/// The controls of the panel for develop `d` (the settings in effect), the clip's own `own`
/// (None for the project's panel), what the clip records (`info`, `as_shot`) and whether its
/// development controls follow the project (`follows`). Columns 1–3 as the panel lays them out.
fn controls(
    d: &RawDevelop,
    own: Option<Option<RawDevelop>>,
    info: Option<&RawClipInfo>,
    as_shot: Option<filmcraft_media::RawAsShot>,
    follows: bool,
) -> Vec<Value> {
    let clip_panel = own.is_some();
    let on = !follows;
    let display = d.gamma.is_display();
    let shot = |f: fn(&filmcraft_media::RawAsShot) -> f64| as_shot.as_ref().map(f);
    let not_display = "Applies to a display gamma curve (BT.1886, sRGB, ST 2084, Hybrid Log-Gamma).";
    let linear_note = "The linear output carries no look: choose a display or log gamma curve to apply it.";
    let mut v = Vec::new();
    if clip_panel {
        let quality = own.flatten().and_then(|o| o.quality).map_or("project", RawQuality::id);
        let mut q = vec![json!({"id": "project", "label": "Use project setting"})];
        q.extend(options(&RawQuality::ALL, RawQuality::id, RawQuality::label));
        v.push(json!({"id": "quality", "kind": "choice", "label": "Decode Quality", "column": 1, "value": quality, "enabled": true, "options": q,
            "note": "The largest decode a preview uses; an export and a monitor at 100 % always decode at full resolution, premium."}));
        v.push(json!({"id": "decodeUsing", "kind": "choice", "label": "Decode Using", "column": 1, "value": d.decode_using.id(), "enabled": true,
            "options": options(&DecodeUsing::ALL, DecodeUsing::id, DecodeUsing::label)}));
    } else {
        v.push(json!({"id": "quality", "kind": "choice", "label": "Decode Quality", "column": 1, "value": d.quality().id(), "enabled": true,
            "options": options(&RawQuality::ALL, RawQuality::id, RawQuality::label),
            "note": "The largest decode a preview uses; an export and a monitor at 100 % always decode at full resolution, premium."}));
    }
    v.push(json!({"id": "colorScience", "kind": "fixed", "label": "Color Science", "column": 1, "value": "ipp2", "enabled": false,
        "options": [{"id": "ipp2", "label": "IPP2"}], "note": "Nikon R3D NE is developed with RED IPP2 only."}));
    let spaces: Vec<Value> = RawColorSpace::ALL.iter().map(|c| json!({"id": c.id(), "label": c.label()})).collect();
    let gammas: Vec<Value> = RawGamma::ALL.iter().map(|g| json!({"id": g.id(), "label": g.label(), "fits": RawDevelop::supports(d.color_space, *g)})).collect();
    v.push(json!({"id": "colorSpace", "kind": "choice", "label": "Color Space", "column": 1, "value": d.color_space.id(), "enabled": on, "options": spaces}));
    v.push(json!({"id": "gamma", "kind": "choice", "label": "Gamma Curve", "column": 1, "value": d.gamma.id(), "enabled": on, "options": gammas,
        "note": "Display curves go with Rec. 709, Rec. 2020 and P3 D65, Log3G10 with REDWideGamutRGB, ST 2084 and Hybrid Log-Gamma with Rec. 2020; linear with any."}));
    if clip_panel {
        let hdrx = info.is_some_and(|i| i.hdrx);
        let why = if hdrx {
            "HDRx blending is not available in FilmCraft yet."
        } else {
            "This clip has no HDRx track (Nikon R3D NE records none): there is nothing to blend."
        };
        v.push(json!({"id": "blendType", "kind": "fixed", "label": "Blend Type", "column": 1, "value": "none", "enabled": false,
            "options": [{"id": "none", "label": "None"}], "note": why}));
        v.push(json!({"id": "blendBias", "kind": "fixed", "label": "Blend Bias", "column": 1, "value": 0.0, "enabled": false, "note": why}));
    }
    let grades = d.grades_apply();
    v.push(json!({"id": "metadataCurves", "kind": "bool", "label": "Apply Metadata Curves", "column": 1, "value": d.metadata_curves, "enabled": on && grades,
        "recorded": info.map(|i| i.curves), "note": if grades { "The luma and RGB curves the clip records." } else { linear_note }}));
    let has_lut = info.is_none_or(|i| i.lut);
    v.push(json!({"id": "creativeLut", "kind": "bool", "label": "Apply Creative LUT", "column": 1, "value": d.creative_lut, "enabled": on && grades && has_lut, "available": has_lut,
        "recorded": info.map(|i| i.lut_enabled),
        "note": if !has_lut { "This clip carries no creative LUT." } else if grades { "The 3D LUT the clip (or its RMD) carries." } else { linear_note }}));
    let cdl = info.map(|i| i.cdl);
    v.push(
        json!({"id": "cdl", "kind": "bool", "label": "Apply CDL", "column": 1, "value": d.cdl, "enabled": on && grades, "recorded": info.map(|i| i.cdl_enabled),
        "cdl": cdl.map(|c| json!({"saturation": c[0], "slope": [c[1], c[4], c[7]], "offset": [c[2], c[5], c[8]], "power": [c[3], c[6], c[9]]})),
        "note": if grades { "The ASC CDL the clip (or its RMD) records." } else { linear_note }}),
    );
    v.push(json!({"id": "iso", "kind": "iso", "label": "ISO", "column": 2, "value": d.iso, "asShot": shot(|a| f64::from(a.iso)), "enabled": on,
        "options": RAW_ISO_VALUES, "default": 800}));
    v.push(json!({"id": "toneMap", "kind": "choice", "label": "Output Tone Map", "column": 2, "value": d.tone_map.id(), "enabled": on && display,
        "recorded": info.and_then(|i| i.tone_map).map(RawToneMap::id),
        "options": options(&RawToneMap::ALL, RawToneMap::id, RawToneMap::label), "note": if display { "" } else { not_display }}));
    v.push(json!({"id": "rolloff", "kind": "choice", "label": "Highlight Roll Off", "column": 2, "value": d.rolloff.id(), "enabled": on && display,
        "recorded": info.and_then(|i| i.rolloff).map(RawRollOff::id),
        "options": options(&RawRollOff::ALL, RawRollOff::id, RawRollOff::label), "note": if display { "" } else { not_display }}));
    let pq = d.gamma == RawGamma::St2084;
    let (lo, hi) = info.map(|i| i.hdr_peak_range).filter(|r| r.0 <= r.1 && r.1 > 0).unwrap_or((*RAW_HDR_PEAK_RANGE.start(), *RAW_HDR_PEAK_RANGE.end()));
    v.push(json!({"id": "hdrPeakNits", "kind": "number", "label": "HDR Peak Nits", "column": 2, "value": d.hdr_peak_nits,
        "asShot": info.map(|i| f64::from(i.hdr_peak_nits.unwrap_or(i.hdr_peak_default))), "enabled": on && pq,
        "min": lo.max(*RAW_HDR_PEAK_RANGE.start()), "max": hi.min(*RAW_HDR_PEAK_RANGE.end()), "step": 10, "decimals": 0, "suffix": " cd/m²", "default": 1000,
        "note": if pq { "" } else { "Applies to the ST 2084 (PQ) gamma curve only." }}));
    v.push(json!({"id": "chromaNoiseReduction", "kind": "bool", "label": "Chroma Noise Reduction", "column": 2, "value": d.chroma_noise_reduction,
        "recorded": info.map(|i| i.chroma_noise_reduction), "enabled": on, "note": "Full and half resolution premium decodes."}));
    let mut fpa = vec![json!({"id": "asRecorded", "label": "As recorded"})];
    fpa.extend(options(&RawFlashing::ALL, RawFlashing::id, RawFlashing::label));
    v.push(json!({"id": "flashingPixel", "kind": "choice", "label": "Flashing Pixel Adjust", "column": 2, "value": d.flashing_pixel.map_or("asRecorded", RawFlashing::id),
        "recorded": info.map(|i| i.flashing_pixel.id()), "enabled": on, "options": fpa, "note": "Full and half resolution premium decodes."}));
    v.push(
        json!({"id": "kelvin", "kind": "number", "label": "Color Temp", "column": 3, "value": d.kelvin, "asShot": shot(|a| f64::from(a.kelvin)), "enabled": on,
        "min": RAW_KELVIN_RANGE.start(), "max": RAW_KELVIN_RANGE.end(), "step": 50, "decimals": 0, "suffix": " K", "default": 5600}),
    );
    v.push(json!({"id": "tint", "kind": "number", "label": "Tint", "column": 3, "value": d.tint, "asShot": shot(|a| f64::from(a.tint)), "enabled": on,
        "min": RAW_TINT_RANGE.start(), "max": RAW_TINT_RANGE.end(), "step": 1, "decimals": 2, "suffix": "", "default": 0}));
    v.push(json!({"id": "exposure", "kind": "number", "label": "Exposure Adjust", "column": 3, "value": d.exposure, "asShot": shot(|a| f64::from(a.exposure)),
        "enabled": on, "min": RAW_EXPOSURE_RANGE.start(), "max": RAW_EXPOSURE_RANGE.end(), "step": 0.1, "decimals": 2, "suffix": " stops", "default": 0}));
    v
}

/// The report of `clip.sourceSettings` for clip `id` of the selection `ids`.
fn report(s: &Session, id: ItemId, ids: &[ItemId]) -> Result<Value> {
    let (_, m) = media_of(&s.project, id).ok_or_else(|| EngineError::Other("select a clip".into()))?;
    let camera_raw = s.project.settings.camera_raw;
    let d = RawDevelop::effective(&camera_raw, m.interpret.develop.as_ref());
    let follows = d.decode_using == DecodeUsing::Project;
    // the clip's recorded values, from the open decoder (an unopened or offline clip has none yet)
    let src = s.source(id);
    let as_shot = src.as_ref().and_then(|src| src.raw_as_shot());
    let info = src.as_ref().and_then(|src| src.raw_clip_info());
    let codec = m.info.video.as_ref().map(|v| v.codec.clone()).unwrap_or_default();
    let rmd_note = match info.as_ref().and_then(|i| i.rmd.clone()) {
        Some(path) => format!("The clip's RMD sidecar is in effect: its settings are the clip's recorded settings ({path})."),
        None => "No RMD sidecar: the clip's recorded settings are the camera's.".to_string(),
    };
    Ok(json!({
        "item": id.0,
        "items": ids.iter().map(|i| i.0).collect::<Vec<_>>(),
        "codec": codec,
        "container": m.info.container,
        "develop": d,
        "own": m.interpret.develop,
        "project": camera_raw,
        "asShot": as_shot,
        "clip": info,
        "rmd": info.as_ref().and_then(|i| i.rmd.clone()),
        "rmdNote": rmd_note,
        "colorSpace": s.project.color_space_override(m).map(|c| c.id()),
        "settings": controls(&d, Some(m.interpret.develop), info.as_ref(), as_shot, follows),
        "message": if follows {
            "This clip decodes with the project's Camera RAW settings (File > Project Settings > Camera RAW). Choose Decode Using > Clip to give it settings of its own."
        } else {
            "RED IPP2 development of the RAW picture. ISO, colour temperature, tint and exposure are applied by the SDK before development (as shot unless set); the output tone map and highlight roll-off shape a display gamma curve. Proxies keep the look they were made with."
        },
    }))
}

/// `file.projectSettings.cameraRaw`: the project's Camera RAW settings, which every RAW clip
/// that decodes with the project's settings (Source Settings ▸ Decode Using ▸ Project) develops
/// with. The same parameters as `clip.sourceSettings` without Decode Using (`quality` is the
/// project's decode quality); one undo step. Without parameters it reports them. `standard: true`
/// puts back the settings a new project starts with (Rec. 709, BT.1886, the SDK's tone map and
/// rolloff, as shot).
pub(crate) fn project_settings(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "file.projectSettings.cameraRaw";
    let change = Change::parse(p, CMD)?;
    if change.decode_using.is_some() {
        return Err(bad(CMD, "decodeUsing is a clip setting (clip.sourceSettings)"));
    }
    if change.quality == Some(None) {
        return Err(bad(CMD, "the project's quality is one of the decode qualities, not `project`"));
    }
    let standard = flag_p(p, "standard", CMD)?.unwrap_or(false);
    let mut d = if standard { RawDevelop::STANDARD } else { s.project.settings.camera_raw };
    if let Some(q) = change.quality {
        d.quality = q;
    }
    change.develop(&mut d);
    d.decode_using = DecodeUsing::Clip;
    d.validate().map_err(|e| bad(CMD, e))?;
    if d != s.project.settings.camera_raw {
        s.edit("Project Settings", |pr, _| {
            let old = std::mem::replace(&mut pr.settings.camera_raw, d);
            // Clips that follow the project (Decode Using ▸ Project) take its new output with it.
            for it in pr.items.values_mut() {
                if let ItemKind::Media(m) = &mut it.kind
                    && m.is_camera_raw()
                {
                    let (before, after) = (RawDevelop::effective(&old, m.interpret.develop.as_ref()), RawDevelop::effective(&d, m.interpret.develop.as_ref()));
                    if before != after {
                        sync_derived(m, &before, &after);
                    }
                }
            }
            Ok(())
        })?;
    }
    let d = s.project.settings.camera_raw;
    let follow = s
        .project
        .items
        .values()
        .filter(|it| matches!(&it.kind, ItemKind::Media(m) if m.is_camera_raw() && RawDevelop::effective(&d, m.interpret.develop.as_ref()).decode_using == DecodeUsing::Project))
        .count();
    Ok(json!({
        "develop": d,
        "standard": d == RawDevelop::STANDARD,
        "clips": follow,
        "settings": controls(&d, None, None, None, false),
        "message": "Camera RAW clips develop with these settings unless they have settings of their own (Clip > Source Settings > Decode Using > Clip).",
    }))
}

/// Every text the Camera Raw panels show from these commands (labels, option labels, notes and
/// messages, in each state that changes them), for the interface's translation catalogs.
pub fn panel_texts() -> Vec<String> {
    let mut out: Vec<String> = Vec::new();
    let mut add = |s: &str| {
        if !s.is_empty() && !out.iter().any(|o| o == s) {
            out.push(s.to_string());
        }
    };
    let pq = RawDevelop { color_space: RawColorSpace::Rec2020, gamma: RawGamma::St2084, ..Default::default() };
    let no_lut = RawClipInfo::default();
    let lut = RawClipInfo { lut: true, hdrx: true, ..Default::default() };
    for d in [RawDevelop::default(), RawDevelop::STANDARD, pq] {
        for (own, info) in [(None, None), (Some(None), None), (Some(None), Some(&no_lut)), (Some(None), Some(&lut))] {
            for st in controls(&d, own, info, None, false) {
                for k in ["label", "note"] {
                    add(st[k].as_str().unwrap_or_default());
                }
                for o in st["options"].as_array().into_iter().flatten() {
                    add(o["label"].as_str().unwrap_or_default());
                }
            }
        }
    }
    for m in [
        "This clip decodes with the project's Camera RAW settings (File > Project Settings > Camera RAW). Choose Decode Using > Clip to give it settings of its own.",
        "RED IPP2 development of the RAW picture. ISO, colour temperature, tint and exposure are applied by the SDK before development (as shot unless set); the output tone map and highlight roll-off shape a display gamma curve. Proxies keep the look they were made with.",
        "Camera RAW clips develop with these settings unless they have settings of their own (Clip > Source Settings > Decode Using > Clip).",
        "No RMD sidecar: the clip's recorded settings are the camera's.",
    ] {
        add(m);
    }
    out
}

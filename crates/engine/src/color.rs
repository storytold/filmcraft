//! Colour commands: colour management (`sequence.colorSettings`, `clip.interpretFootage`,
//! `color.spaces`, `media.colorInfo`), the project LUT library (`lut.*`), Lumetri LUT slots and
//! section switches (`lumetri.*`).
//!
//! Lumetri commands address a clip by `clip` (default: the first selected video clip with
//! Lumetri Color, else the first selected video clip — Lumetri is added if missing).
//!
//! Lumetri Presets (Effects panel): `lumetri.presets` lists them, `lumetri.applyPreset` adds a
//! Lumetri Color configured as the preset to clips (one undo step), `lumetri.presetThumbnails`
//! renders a folder's thumbnail grid (our Lumetri on the procedural preview picture) to a PNG.

use filmcraft_color::{ColorSpace, Lut, LutFormat, WorkingSpace};
use filmcraft_project::{ClipId, ItemId, ItemKind, ParamValue, ProjectLut, TrackKind};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, clip_p, has_seq, item_p, str_p, time_p};
use crate::{Result, Session};

type Run = fn(&mut Session, &Value) -> Result<Value>;
type Enabled = fn(&Session) -> std::result::Result<(), String>;

fn spec(id: &'static str, label: &'static str, menu: &'static [&'static str], params: &'static str, enabled: Enabled, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut: None, params, enabled, run, journal: true }
}
fn query(id: &'static str, label: &'static str, params: &'static str, run: Run) -> CommandSpec {
    CommandSpec { id, label, menu: &[], shortcut: None, params, enabled: always, run, journal: false }
}

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        spec(
            "sequence.colorSettings",
            "Color Management…",
            &["Sequence"],
            r#"{"workingSpace":"rec709"|"rec2100-pq"|"rec2100-hlg"?,"wideGamut":bool?,"autoToneMap":bool?}"#,
            has_seq,
            color_settings,
        ),
        spec(
            "clip.interpretFootage",
            "Interpret Footage…",
            &["Clip", "Modify"],
            r#"{"items":[id]?,"colorSpace":"auto"|"<color space id>"?,"pixelAspect":[num,den]|"file"?}"#,
            has_footage,
            interpret,
        ),
        query("color.spaces", "List Colour Spaces", "{}", spaces),
        query("lumetri.presets", "List Lumetri Presets", r#"{"folder":str?}"#, list_presets),
        spec("lumetri.applyPreset", "Apply Lumetri Preset", &[], r#"{"name":str,"clips":[id]?}"#, has_seq, apply_preset),
        query(
            "lumetri.presetThumbnails",
            "Lumetri Preset Thumbnails",
            r#"{"folder":str?,"names":[str]?,"width":n=160,"columns":n=4,"path":str?}"#,
            preset_thumbnails,
        ),
        query("media.colorInfo", "Media Colour Info", r#"{"item":id}"#, color_info),
        spec("lut.import", "Import LUT…", &[], r#"{"path":str,"name":str?}"#, always, import),
        query("lut.list", "List LUTs", "{}", list),
        spec("lut.remove", "Remove LUT", &[], r#"{"id":str}"#, always, remove),
        query("lut.export", "Export LUT", r#"{"lut":"lib:<id>"|"builtin:<id>","path":str,"format":"cube"|"3dl"?}"#, export),
        spec("lumetri.setInputLut", "Set Input LUT", &[], r#"{"clip":id?,"lut":"lib:<id>"|"builtin:<id>"|""?,"path":str?}"#, has_seq, |s, p| {
            set_lut(s, p, "input_lut")
        }),
        spec("lumetri.setLook", "Set Creative Look", &[], r#"{"clip":id?,"lut":"lib:<id>"|"builtin:<id>"|""?,"path":str?}"#, has_seq, |s, p| {
            set_lut(s, p, "look_lut")
        }),
        spec(
            "lumetri.applyMatch",
            "Apply Match",
            &[],
            r#"{"clip":id?,"referenceTime":ticks?|"referenceFrame":n?|"referenceTimecode":str?,"faceDetection":bool=true}"#,
            has_seq,
            apply_match,
        ),
        spec(
            "lumetri.setSection",
            "Toggle Lumetri Section",
            &[],
            r#"{"clip":id?,"section":"basic"|"creative"|"curves"|"wheels"|"hsl"|"vignette","on":bool?}"#,
            has_seq,
            set_section,
        ),
    ]
}

fn import_lut(s: &mut Session, path: &str, name: Option<&str>) -> Result<(String, Value)> {
    let text = std::fs::read_to_string(path).map_err(|e| bad("lut.import", format!("{path}: {e}")))?;
    let fmt = LutFormat::from_path(path).ok_or_else(|| bad("lut.import", "expected a .cube or .3dl file"))?;
    let lut = Lut::parse(&text, Some(fmt)).map_err(|e| bad("lut.import", format!("{path}: {e}")))?;
    let stem = std::path::Path::new(path).file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or_else(|| "LUT".into());
    let name = name.map(str::to_string).unwrap_or(stem);
    // re-importing identical content reuses the entry
    if let Some(l) = s.project.luts.iter().find(|l| *l.text == *text) {
        return Ok((l.id.clone(), json!({"id": l.id, "ref": format!("lib:{}", l.id), "name": l.name, "reused": true})));
    }
    let info = json!({
        "size3d": lut.cube.as_ref().map(|c| c.size),
        "size1d": lut.shaper.as_ref().map(|c| c.size()),
        "title": lut.title,
    });
    let mut id = String::new();
    s.edit("Import LUT", |pr, _| {
        id = format!("lut{}", pr.alloc_id());
        pr.luts.push(ProjectLut {
            id: id.clone(),
            name: name.clone(),
            source_path: Some(path.to_string()),
            format: fmt.extension().into(),
            text: text.as_str().into(),
        });
        Ok(())
    })?;
    Ok((id.clone(), json!({"id": id, "ref": format!("lib:{id}"), "name": name, "lut": info})))
}

fn import(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("lut.import", "need `path`"))?.to_string();
    Ok(import_lut(s, &path, str_p(p, "name"))?.1)
}

fn list(s: &mut Session, _: &Value) -> Result<Value> {
    let lib: Vec<Value> = s
        .project
        .luts
        .iter()
        .map(|l| json!({"id": l.id, "ref": format!("lib:{}", l.id), "name": l.name, "format": l.format, "source": l.source_path}))
        .collect();
    let builtin: Vec<Value> = filmcraft_render::luts::builtins()
        .iter()
        .map(|b| json!({"ref": format!("builtin:{}", b.id), "name": b.label, "input": filmcraft_render::luts::input_builtins().any(|i| i.id == b.id)}))
        .collect();
    Ok(json!({"library": lib, "builtin": builtin}))
}

fn remove(s: &mut Session, p: &Value) -> Result<Value> {
    let id = str_p(p, "id").ok_or_else(|| bad("lut.remove", "need `id`"))?.trim_start_matches("lib:").to_string();
    if !s.project.luts.iter().any(|l| l.id == id) {
        return Err(bad("lut.remove", format!("no LUT `{id}`")));
    }
    s.edit("Remove LUT", |pr, _| {
        pr.luts.retain(|l| l.id != id);
        Ok(())
    })?;
    Ok(Value::Null)
}

fn export(s: &mut Session, p: &Value) -> Result<Value> {
    let r = str_p(p, "lut").ok_or_else(|| bad("lut.export", "need `lut`"))?;
    let path = str_p(p, "path").ok_or_else(|| bad("lut.export", "need `path`"))?;
    let lut = filmcraft_render::luts::resolve(Some(&s.project), r).ok_or_else(|| bad("lut.export", format!("unknown LUT `{r}`")))?;
    let fmt = match str_p(p, "format") {
        Some("3dl") => LutFormat::ThreeDl,
        Some("cube") => LutFormat::Cube,
        Some(o) => return Err(bad("lut.export", format!("unknown format `{o}`"))),
        None => LutFormat::from_path(path).unwrap_or(LutFormat::Cube),
    };
    let text = match fmt {
        LutFormat::Cube => lut.to_cube(),
        LutFormat::ThreeDl => lut.to_3dl().map_err(|e| bad("lut.export", e))?,
    };
    std::fs::write(path, &text).map_err(|e| bad("lut.export", format!("{path}: {e}")))?;
    Ok(json!({"path": path, "bytes": text.len()}))
}

/// The target clip and the index of its Lumetri effect (applying Lumetri when missing).
pub(crate) fn lumetri_clip(s: &mut Session, p: &Value, cmd: &str) -> Result<(ClipId, usize)> {
    let seq = s.active_sequence().ok_or_else(|| bad(cmd, "no active sequence"))?;
    let video = |c: &ClipId| seq.find_item(*c).is_some_and(|(t, _)| seq.track(t).is_some_and(|t| t.kind == TrackKind::Video));
    let has = |c: &ClipId| seq.find_item(*c).is_some_and(|(_, it)| it.effects.iter().any(|e| e.effect == "lumetri"));
    let clip = match clip_p(p, "clip") {
        Some(c) if video(&c) => c,
        Some(_) => return Err(bad(cmd, "not a video clip")),
        None => {
            let sel: Vec<ClipId> = s.state.selection.iter().copied().filter(video).collect();
            sel.iter().copied().find(has).or(sel.first().copied()).ok_or_else(|| bad(cmd, "select a video clip"))?
        }
    };
    if !has(&clip) {
        s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"}))?;
    }
    let seq = s.active_sequence().ok_or_else(|| bad(cmd, "no active sequence"))?;
    let idx = seq.find_item(clip).and_then(|(_, it)| it.effects.iter().position(|e| e.effect == "lumetri")).ok_or_else(|| bad(cmd, "no Lumetri"))?;
    Ok((clip, idx))
}

/// Set a Lumetri parameter (inserting it when an older project's instance lacks it).
pub(crate) fn set_lumetri_param(s: &mut Session, clip: ClipId, idx: usize, param: &str, v: ParamValue, label: &str) -> Result<()> {
    let param = param.to_string();
    s.edit_sequence(label, |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let e = it.effects.get_mut(idx).ok_or_else(|| bad("lumetri", "no Lumetri"))?;
        e.params.entry(param.clone()).or_insert_with(|| filmcraft_project::Param::new(v.clone())).value = v.clone();
        Ok(())
    })?;
    Ok(())
}

fn set_lut(s: &mut Session, p: &Value, param: &str) -> Result<Value> {
    let cmd = if param == "input_lut" { "lumetri.setInputLut" } else { "lumetri.setLook" };
    let r = match (str_p(p, "path"), str_p(p, "lut")) {
        (Some(path), _) => {
            let path = path.to_string();
            format!("lib:{}", import_lut(s, &path, None)?.0)
        }
        (None, Some(r)) => r.to_string(),
        (None, None) => String::new(),
    };
    if !r.is_empty() && filmcraft_render::luts::resolve(Some(&s.project), &r).is_none() {
        return Err(bad(cmd, format!("unknown LUT `{r}`")));
    }
    let (clip, idx) = lumetri_clip(s, p, cmd)?;
    set_lumetri_param(s, clip, idx, param, ParamValue::Text(r.clone()), if param == "input_lut" { "Input LUT" } else { "Creative Look" })?;
    Ok(json!({"clip": clip.0, "lut": r, "name": filmcraft_render::luts::label(Some(&s.project), &r)}))
}

fn set_section(s: &mut Session, p: &Value) -> Result<Value> {
    let sec = str_p(p, "section").ok_or_else(|| bad("lumetri.setSection", "need `section`"))?;
    let param = match sec.to_ascii_lowercase().as_str() {
        "basic" | "basic correction" => "basic_on",
        "creative" => "creative_on",
        "curves" => "curves_on",
        "wheels" | "color wheels" | "color wheels & match" => "wheels_on",
        "hsl" | "hsl secondary" => "hsl_on",
        "vignette" => "vignette_on",
        o => return Err(bad("lumetri.setSection", format!("unknown section `{o}`"))),
    };
    let (clip, idx) = lumetri_clip(s, p, "lumetri.setSection")?;
    let cur = s
        .active_sequence()
        .and_then(|q| q.find_item(clip))
        .and_then(|(_, it)| it.effects.get(idx).and_then(|e| e.param(param)).and_then(|v| v.value.as_bool()))
        .unwrap_or(param != "hsl_on");
    let v = bool_p(p, "on").unwrap_or(!cur);
    set_lumetri_param(s, clip, idx, param, ParamValue::Bool(v), "Lumetri Section")?;
    Ok(json!({"clip": clip.0, "param": param, "on": v}))
}

fn color_settings(s: &mut Session, p: &Value) -> Result<Value> {
    let id = s.state.active_sequence.ok_or_else(|| bad("sequence.colorSettings", "no active sequence"))?;
    let ws = match str_p(p, "workingSpace") {
        Some(w) => Some(WorkingSpace::parse(w).ok_or_else(|| bad("sequence.colorSettings", format!("unknown working space `{w}`")))?),
        None => None,
    };
    let (wide, tone) = (bool_p(p, "wideGamut"), bool_p(p, "autoToneMap"));
    let cur = s.project.sequence(id).map(|q| q.settings.color).unwrap_or_default();
    let changed = ws.is_some_and(|w| w != cur.working) || wide.is_some_and(|v| v != cur.wide_gamut) || tone.is_some_and(|v| v != cur.auto_tone_map);
    if changed {
        s.edit("Sequence Color Settings", |pr, _| {
            let q = pr.sequence_mut(id).ok_or_else(|| bad("sequence.colorSettings", "no sequence"))?;
            let c = &mut q.settings.color;
            if let Some(w) = ws {
                c.working = w;
            }
            if let Some(v) = wide {
                c.wide_gamut = v;
            }
            if let Some(v) = tone {
                c.auto_tone_map = v;
            }
            q.settings.working_space = c.working.label().into();
            Ok(())
        })?;
    }
    let c = s.project.sequence(id).map(|q| q.settings.color).unwrap_or_default();
    Ok(json!({"workingSpace": c.working.id(), "label": c.working.label(), "wideGamut": c.wide_gamut, "autoToneMap": c.auto_tone_map}))
}

fn has_footage(s: &Session) -> std::result::Result<(), String> {
    if footage_targets(s, &Value::Null).is_empty() { Err("select footage in the Project panel or clips in the timeline".into()) } else { Ok(()) }
}

/// Media items addressed by `items`, else the Project panel selection, else the media of the
/// selected timeline clips.
pub(crate) fn footage_targets(s: &Session, p: &Value) -> Vec<ItemId> {
    let media = |i: &ItemId| s.project.item(*i).is_some_and(|it| matches!(it.kind, ItemKind::Media(_)));
    if let Some(a) = p.get("items").and_then(Value::as_array) {
        return a.iter().filter_map(Value::as_u64).map(ItemId).filter(media).collect();
    }
    let mut v: Vec<ItemId> = s.state.project_selection.iter().copied().filter(media).collect();
    if v.is_empty()
        && let Some(q) = s.active_sequence()
    {
        for c in &s.state.selection {
            if let Some((_, it)) = q.find_item(*c)
                && media(&it.item)
                && !v.contains(&it.item)
            {
                v.push(it.item);
            }
        }
    }
    v
}

/// `clip.interpretFootage`: the colour space (`colorSpace`) and / or the pixel aspect ratio
/// (`pixelAspect`) the footage is interpreted with. `"auto"` / `"file"` go back to the file's
/// metadata; a parameter left out keeps its current interpretation.
fn interpret(s: &mut Session, p: &Value) -> Result<Value> {
    const CMD: &str = "clip.interpretFootage";
    let items = footage_targets(s, p);
    if items.is_empty() {
        return Err(bad(CMD, "no footage selected"));
    }
    let cs = match str_p(p, "colorSpace") {
        None => None,
        Some("auto") | Some("") => Some(None),
        Some(c) => Some(Some(ColorSpace::parse(c).ok_or_else(|| bad(CMD, format!("unknown colour space `{c}`")))?)),
    };
    let par =
        match p.get("pixelAspect") {
            None | Some(Value::Null) => None,
            Some(Value::String(f)) if f == "file" || f == "auto" => Some(None),
            Some(v) => Some(Some(pixel_aspect_p(v).ok_or_else(|| {
                bad(CMD, format!("pixelAspect must be [num, den] between 1:{0} and {0}:1, or \"file\"", filmcraft_project::MAX_PIXEL_ASPECT))
            })?)),
        };
    if cs.is_none() && par.is_none() {
        return Err(bad(CMD, "need `colorSpace` (auto or a colour space id) or `pixelAspect`"));
    }
    s.edit("Interpret Footage", |pr, _| {
        for i in &items {
            if let Some(ItemKind::Media(m)) = pr.item_mut(*i).map(|it| &mut it.kind) {
                if let Some(cs) = cs {
                    m.interpret.color_space = cs;
                }
                if let Some(par) = par {
                    m.interpret.par = par;
                }
            }
        }
        Ok(())
    })?;
    let first = items.first().and_then(|i| s.project.item(*i)).and_then(|i| i.as_media());
    Ok(json!({
        "items": items.iter().map(|i| i.0).collect::<Vec<_>>(),
        "colorSpace": first.and_then(|m| m.interpret.color_space).map(|c| c.id()).unwrap_or("auto"),
        "pixelAspect": first.map(|m| m.pixel_aspect()),
    }))
}

/// `[num, den]` as a pixel aspect ratio FilmCraft can use ([`filmcraft_project::checked_par`]).
fn pixel_aspect_p(v: &Value) -> Option<(u32, u32)> {
    let [n, d] = v.as_array()?.as_slice() else { return None };
    let (n, d) = (u32::try_from(n.as_u64()?).ok()?, u32::try_from(d.as_u64()?).ok()?);
    filmcraft_project::checked_par((n, d))
}

fn spaces(_: &mut Session, _: &Value) -> Result<Value> {
    let cs: Vec<Value> = ColorSpace::ALL
        .iter()
        .map(|c| json!({"id": c.id(), "label": c.label(), "hdr": c.is_hdr(), "log": c.is_log(), "gamut": format!("{:?}", c.gamut())}))
        .collect();
    let ws: Vec<Value> = WorkingSpace::ALL.iter().map(|w| json!({"id": w.id(), "label": w.label(), "hdr": w.is_hdr()})).collect();
    Ok(json!({"colorSpaces": cs, "workingSpaces": ws}))
}

fn color_info(s: &mut Session, p: &Value) -> Result<Value> {
    let id = item_p(p, "item").ok_or_else(|| bad("media.colorInfo", "need `item`"))?;
    let Some(ItemKind::Media(m)) = s.project.item(id).map(|i| &i.kind) else { return Err(bad("media.colorInfo", "not a media item")) };
    let detected = m.info.video.as_ref().map(|v| ColorSpace::from_info(&v.color));
    let effective = m.interpret.color_space.or(detected);
    Ok(json!({
        "detected": detected.map(|c| c.id()),
        "detectedLabel": detected.map(|c| c.label()),
        "metadata": m.info.video.as_ref().map(|v| json!({"transfer": format!("{:?}", v.color.transfer), "primaries": format!("{:?}", v.color.primaries), "matrix": format!("{:?}", v.color.matrix), "range": format!("{:?}", v.color.range)})),
        "override": m.interpret.color_space.map(|c| c.id()),
        "effective": effective.map(|c| c.id()),
        "effectiveLabel": effective.map(|c| c.label()),
        "hdr": effective.is_some_and(|c| c.is_hdr()),
        // mastering display / content light level and the peak tone mapping uses
        "hdrMetadata": m.info.video.as_ref().and_then(|v| v.hdr),
        "toneMapPeakNits": m.info.video.as_ref().and_then(|v| v.hdr.and_then(|h| h.peak_nits())),
    }))
}

fn presets_of(p: &Value) -> Vec<filmcraft_render::lumetri_presets::LumetriPreset> {
    let all = filmcraft_render::lumetri_presets::presets();
    let names: Option<Vec<String>> = p.get("names").and_then(Value::as_array).map(|a| a.iter().filter_map(|v| v.as_str().map(str::to_lowercase)).collect());
    let folder = str_p(p, "folder");
    all.into_iter()
        .filter(|x| folder.is_none_or(|f| x.folder.eq_ignore_ascii_case(f)))
        .filter(|x| names.as_ref().is_none_or(|n| n.contains(&x.name.to_lowercase())))
        .collect()
}

fn list_presets(_: &mut Session, p: &Value) -> Result<Value> {
    let v: Vec<Value> = presets_of(p).iter().map(|x| json!({"folder": x.folder, "name": x.name, "description": x.description})).collect();
    Ok(json!({"folders": filmcraft_render::lumetri_presets::FOLDERS, "presets": v}))
}

/// Lumetri Presets ▸ apply: a new Lumetri Color configured as the preset on each video clip
/// (standard effects go before the intrinsic ones, like `effects.apply`).
fn apply_preset(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "lumetri.applyPreset";
    let name = str_p(p, "name").ok_or_else(|| bad(cmd, "need `name`"))?;
    let preset = filmcraft_render::lumetri_presets::find(name).ok_or_else(|| bad(cmd, format!("no Lumetri preset `{name}`")))?;
    let clips = crate::commands::clips_p(s, p);
    if clips.is_empty() {
        return Err(bad(cmd, "select clips first"));
    }
    let inst = preset.instance();
    let n = s.edit_sequence(&format!("Apply {}", preset.name), |q, _, _| {
        let mut n = 0;
        for t in q.video_tracks.iter_mut() {
            for it in t.items.iter_mut().filter(|i| clips.contains(&i.id)) {
                let pos = it.effects.iter().position(|e| e.def().is_some_and(|d| d.intrinsic)).unwrap_or(it.effects.len());
                it.effects.insert(pos, inst.clone());
                n += 1;
            }
        }
        Ok(n)
    })?;
    if n == 0 {
        return Err(bad(cmd, "no video clips among the clips"));
    }
    Ok(json!({"preset": preset.name, "clips": n}))
}

fn preset_thumbnails(_: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "lumetri.presetThumbnails";
    let presets = presets_of(p);
    if presets.is_empty() {
        return Err(bad(cmd, "no presets match"));
    }
    let w = p.get("width").and_then(Value::as_u64).unwrap_or(160).clamp(16, 1920) as usize;
    let h = (w * 9 / 16).max(9);
    let cols = p.get("columns").and_then(Value::as_u64).unwrap_or(4).clamp(1, 32) as usize;
    let img = filmcraft_render::lumetri_presets::grid(&presets, cols, w, h);
    let mut out = json!({"presets": presets.iter().map(|x| x.name).collect::<Vec<_>>(), "width": img.w, "height": img.h, "cell": [w, h], "columns": cols});
    if let Some(path) = str_p(p, "path") {
        let png = filmcraft_export::encode_png(img.over_black_rgba8(), img.w as u32, img.h as u32).map_err(|e| bad(cmd, e.to_string()))?;
        std::fs::write(path, png).map_err(|e| bad(cmd, format!("{path}: {e}")))?;
        out["path"] = json!(path);
    }
    Ok(out)
}

/// Lumetri ▸ Color Wheels & Match ▸ Apply Match (see `filmcraft_render::color_match`): match the
/// clip at the playhead to the sequence frame at the reference time. Sets the three wheels, their
/// lightness and Basic saturation in one undo step.
fn apply_match(s: &mut Session, p: &Value) -> Result<Value> {
    let (clip, idx) = lumetri_clip(s, p, "lumetri.applyMatch")?;
    let seq_id = s.state.active_sequence.ok_or_else(|| bad("lumetri.applyMatch", "no active sequence"))?;
    let reference_t = time_p(s, p, "reference").ok_or_else(|| bad("lumetri.applyMatch", "need `referenceTime`, `referenceFrame` or `referenceTimecode`"))?;
    let skin = bool_p(p, "faceDetection").unwrap_or(true);
    let q = s.active_sequence().ok_or_else(|| bad("lumetri.applyMatch", "no active sequence"))?;
    let (_, it) = q.find_item(clip).ok_or_else(|| bad("lumetri.applyMatch", "no such clip"))?;
    let t = s.playhead().clamp(it.start, it.end() - filmcraft_time::Tick(1));
    if it.range().contains(reference_t) {
        return Err(bad("lumetri.applyMatch", "the reference frame is inside the clip being matched; pick a frame from another shot"));
    }
    let base = it.effects[idx].clone();
    // the current shot as Lumetri sees it: the clip with its Lumetri switched off
    let mut probe = (*s.project).clone();
    if let Some((_, pi)) = probe.sequence_mut(seq_id).and_then(|q| q.find_item_mut(clip)) {
        pi.effects[idx].enabled = false;
    }
    let provider = s.media.provider(s.project.clone(), s.services.clone());
    let opts = filmcraft_render::RenderOptions { scale: 0.25, working_output: true, ..Default::default() };
    let current =
        filmcraft_render::render_clip(&probe, seq_id, clip, t, opts, &provider).ok_or_else(|| bad("lumetri.applyMatch", "the clip has no picture"))?;
    let reference = filmcraft_render::render_sequence(&s.project, seq_id, reference_t, opts, &provider);
    let m = filmcraft_render::color_match::solve(&current, &reference, &base, skin);
    let v2 = |a: [f32; 2]| ParamValue::Vec2(filmcraft_geom::Vec2::new(a[0] as f64, a[1] as f64));
    let values = [
        ("wheel_shadows", v2(m.shadows)),
        ("wheel_midtones", v2(m.midtones)),
        ("wheel_highlights", v2(m.highlights)),
        ("wheel_shadows_l", ParamValue::Float(m.lightness[0] as f64)),
        ("wheel_midtones_l", ParamValue::Float(m.lightness[1] as f64)),
        ("wheel_highlights_l", ParamValue::Float(m.lightness[2] as f64)),
        ("saturation", ParamValue::Float(m.saturation as f64)),
        ("wheels_on", ParamValue::Bool(true)),
    ];
    s.edit_sequence("Apply Match", |q, _, _| {
        let (_, it) = q.find_item_mut(clip).ok_or(filmcraft_edit::EditError::NoItem(clip))?;
        let e = it.effects.get_mut(idx).ok_or_else(|| bad("lumetri.applyMatch", "no Lumetri"))?;
        for (k, v) in &values {
            let prm = e.params.entry(k.to_string()).or_insert_with(|| filmcraft_project::Param::new(v.clone()));
            prm.keyframes.clear();
            prm.value = v.clone();
        }
        e.enabled = true;
        Ok(())
    })?;
    Ok(json!({
        "clip": clip.0,
        "shadows": m.shadows, "midtones": m.midtones, "highlights": m.highlights,
        "lightness": m.lightness, "saturation": m.saturation,
        "distanceBefore": m.before, "distanceAfter": m.after,
    }))
}

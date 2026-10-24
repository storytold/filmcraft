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
//!
//! LUT folders: `.cube` / `.3dl` files under `FILMCRAFT_LUT_DIR` (a path list), `<exe dir>/LUTs`
//! and `<data dir>/LUTs` are listed by `lut.folders` and offered in the Lumetri LUT menus grouped
//! by subfolder; choosing one imports it into the project library (`lumetri.setInputLut {path}`),
//! so the project stays self-contained.

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
        query("lut.folders", "List LUT Folders", "{}", folders),
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
    // re-importing identical content reuses the entry, now from this path (so the LUT menus mark
    // the folder file just picked)
    if let Some(i) = s.project.luts.iter().position(|l| *l.text == *text) {
        if s.project.luts.get(i).is_some_and(|l| l.source_path.as_deref() != Some(path)) {
            s.edit("Import LUT", |pr, _| {
                if let Some(l) = pr.luts.get_mut(i) {
                    l.source_path = Some(path.to_string());
                }
                Ok(())
            })?;
        }
        let l = s.project.luts.get(i).ok_or_else(|| bad("lut.import", "the LUT library changed"))?;
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

/// A LUT file found in one of the [`lut_folders`]: `group` is its folder relative to the LUT
/// folder (the folder's own name at the top level), `name` the file stem.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct FolderLut {
    pub group: String,
    pub name: String,
    pub path: String,
}

/// At most this many files are listed, this deep: a LUT folder pointed at a whole drive must not
/// stall the menu.
const MAX_FOLDER_LUTS: usize = 2000;
const MAX_FOLDER_DEPTH: usize = 4;
/// At most this many directory entries are read under each LUT folder (LUTs or not, at every
/// depth), so a folder of many other files is not read and sorted in full either.
const MAX_FOLDER_ENTRIES: usize = 20_000;

/// The LUT folders, in menu order: every entry of `FILMCRAFT_LUT_DIR` (a `PATH`-style list), then
/// `LUTs` beside the executable, then `LUTs` in the data directory. Missing folders are fine.
pub fn lut_folders() -> Vec<std::path::PathBuf> {
    let mut v: Vec<std::path::PathBuf> = Vec::new();
    if let Some(list) = std::env::var_os("FILMCRAFT_LUT_DIR") {
        v.extend(std::env::split_paths(&list).filter(|p| !p.as_os_str().is_empty()));
    }
    if let Some(dir) = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.join("LUTs"))) {
        v.push(dir);
    }
    if let Some(d) = crate::autosave::default_data_dir() {
        v.push(d.join("LUTs"));
    }
    // a folder named twice is scanned once, at its first place
    let mut seen = std::collections::HashSet::new();
    v.retain(|p| seen.insert(p.clone()));
    v
}

/// The LUT files under `folders`, sorted by group then name (case-insensitively), each file once
/// (a folder can also sit inside another). Hidden entries (a leading `.`) are skipped; unreadable
/// folders are empty.
pub fn scan_lut_folders(folders: &[std::path::PathBuf]) -> Vec<FolderLut> {
    scan_lut_folders_within(folders, MAX_FOLDER_ENTRIES, MAX_FOLDER_LUTS)
}

/// [`scan_lut_folders`] reading at most `entries` directory entries under each folder and listing
/// at most `luts` files.
pub(crate) fn scan_lut_folders_within(folders: &[std::path::PathBuf], entries: usize, luts: usize) -> Vec<FolderLut> {
    let mut w = LutWalk { budget: 0, max_luts: luts, out: Vec::new(), seen: std::collections::HashSet::new() };
    for root in folders {
        w.budget = entries;
        walk_lut_folder(root, root, 0, &mut w);
    }
    let mut out = w.out;
    out.sort_by_cached_key(|l| (l.group.to_lowercase(), l.name.to_lowercase(), l.path.clone()));
    out
}

/// A LUT folder scan: `budget` is the directory entries still to read under the current folder,
/// `seen` the files listed so far (a file reached from two folders keeps its first listing and
/// counts once toward `max_luts`).
struct LutWalk {
    budget: usize,
    max_luts: usize,
    out: Vec<FolderLut>,
    seen: std::collections::HashSet<std::path::PathBuf>,
}

fn walk_lut_folder(root: &std::path::Path, dir: &std::path::Path, depth: usize, w: &mut LutWalk) {
    if depth > MAX_FOLDER_DEPTH || w.out.len() >= w.max_luts || w.budget == 0 {
        return;
    }
    let Ok(rd) = std::fs::read_dir(dir) else { return };
    // unreadable entries count too, so the read stops after `budget` entries whatever they are
    let mut read = 0usize;
    let mut entries: Vec<_> = rd.take(w.budget).inspect(|_| read = read.saturating_add(1)).flatten().collect();
    w.budget = w.budget.saturating_sub(read);
    entries.sort_by_key(|e| e.file_name());
    for e in entries {
        if w.out.len() >= w.max_luts {
            return;
        }
        let name = e.file_name().to_string_lossy().to_string();
        if name.starts_with('.') {
            continue;
        }
        let path = e.path();
        if path.is_dir() {
            walk_lut_folder(root, &path, depth.saturating_add(1), w);
        } else if LutFormat::from_path(&name).is_some() && w.seen.insert(path.clone()) {
            let group = match path.parent().and_then(|p| p.strip_prefix(root).ok()).filter(|r| !r.as_os_str().is_empty()) {
                Some(rel) => rel.components().map(|c| c.as_os_str().to_string_lossy().to_string()).collect::<Vec<_>>().join(" / "),
                None => root.file_name().map(|n| n.to_string_lossy().to_string()).unwrap_or_else(|| "LUTs".into()),
            };
            let stem = path.file_stem().map(|s| s.to_string_lossy().to_string()).unwrap_or(name);
            w.out.push(FolderLut { group, name: stem, path: path.to_string_lossy().to_string() });
        }
    }
}

/// The LUT folders' contents for the menus, rescanned at most every few seconds (the menu asks
/// every frame while open; a folder of a few thousand files is a noticeable read).
pub fn folder_luts() -> std::sync::Arc<Vec<FolderLut>> {
    use std::sync::{Arc, Mutex, OnceLock, PoisonError};
    use std::time::Duration;
    use web_time::Instant;
    static CACHE: OnceLock<Mutex<Option<(Instant, Arc<Vec<FolderLut>>)>>> = OnceLock::new();
    let cache = CACHE.get_or_init(|| Mutex::new(None));
    let mut slot = cache.lock().unwrap_or_else(PoisonError::into_inner);
    if let Some((at, luts)) = slot.as_ref()
        && at.elapsed() < Duration::from_secs(3)
    {
        return luts.clone();
    }
    let luts = Arc::new(scan_lut_folders(&lut_folders()));
    *slot = Some((Instant::now(), luts.clone()));
    luts
}

fn folders(_: &mut Session, _: &Value) -> Result<Value> {
    let folders = lut_folders();
    let luts = scan_lut_folders(&folders);
    Ok(json!({
        "folders": folders.iter().map(|f| json!({"path": f.to_string_lossy(), "exists": f.is_dir()})).collect::<Vec<_>>(),
        "luts": luts,
    }))
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
    let clip = lumetri_target(s, p, cmd)?;
    let seq = s.active_sequence().ok_or_else(|| bad(cmd, "no active sequence"))?;
    let has = |c: &ClipId| seq.find_item(*c).is_some_and(|(_, it)| it.effects.iter().any(|e| e.effect == "lumetri"));
    if !has(&clip) {
        s.execute("effects.apply", json!({"clips": [clip.0], "effect": "lumetri"}))?;
    }
    let seq = s.active_sequence().ok_or_else(|| bad(cmd, "no active sequence"))?;
    let idx = seq.find_item(clip).and_then(|(_, it)| it.effects.iter().position(|e| e.effect == "lumetri")).ok_or_else(|| bad(cmd, "no Lumetri"))?;
    Ok((clip, idx))
}

/// The video clip [`lumetri_clip`] works on (`clip`, else the selected video clip with a Lumetri,
/// else the first selected one), without changing anything.
fn lumetri_target(s: &Session, p: &Value, cmd: &str) -> Result<ClipId> {
    let seq = s.active_sequence().ok_or_else(|| bad(cmd, "no active sequence"))?;
    let video = |c: &ClipId| seq.find_item(*c).is_some_and(|(t, _)| seq.track(t).is_some_and(|t| t.kind == TrackKind::Video));
    let has = |c: &ClipId| seq.find_item(*c).is_some_and(|(_, it)| it.effects.iter().any(|e| e.effect == "lumetri"));
    match clip_p(p, "clip") {
        Some(c) if video(&c) => Ok(c),
        Some(_) => Err(bad(cmd, "not a video clip")),
        None => {
            let sel: Vec<ClipId> = s.state.selection.iter().copied().filter(video).collect();
            sel.iter().copied().find(has).or(sel.first().copied()).ok_or_else(|| bad(cmd, "select a video clip"))
        }
    }
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
    // the target first: a bad `clip` or no selection must not import the path
    let target = lumetri_target(s, p, cmd)?;
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
    let (clip, idx) = lumetri_clip(s, &json!({"clip": target.0}), cmd)?;
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

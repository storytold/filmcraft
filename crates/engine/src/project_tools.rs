//! File / Edit / Clip menu long tail (M3.11), and [`apply_layout`], which puts these commands (and
//! those of `sequence_extras` and `scene_detect`) in Premiere's menu order.
//!
//! | Id | Menu |
//! |---|---|
//! | `file.newSearchBin` | File ▸ New ▸ Search Bin (`project.searchBinItems`, `project.editSearchBin`, `project.deleteSearchBin`) |
//! | `file.close` | File ▸ Close (⌘W): closes the active sequence |
//! | `file.closeAllProjects` / `file.closeAllOtherProjects` | File ▸ Close All Projects / Close All Other Projects |
//! | `file.saveAsTemplate` | File ▸ Save as Template… (`file.templates`, `file.newProjectFromTemplate`) |
//! | `file.importFromMediaBrowser` | File ▸ Import from Media Browser (⌥⌘I) |
//! | `file.exportSelectionProject` | File ▸ Export ▸ Selection as FilmCraft Project… |
//! | `file.exportAle` | File ▸ Export ▸ Avid Log Exchange… |
//! | `file.mediaPropertiesFile` / `file.mediaProperties` | File ▸ Get Media File Properties for ▸ File… / Selection… (⇧⌘H) |
//! | `file.projectSettings.general` / `.scratchDisks` | File ▸ Project Settings ▸ General… / Scratch Disks… |
//! | `edit.find` / `edit.findNext` | Edit ▸ Find… (⌘F) / Find Next |
//! | `edit.editOriginal` | Edit ▸ Edit Original (⌘E): the frontend opens the files |
//! | `clip.editOffline` | Clip ▸ Edit Offline… |
//! | `clip.sourceSettings` | Clip ▸ Source Settings… |
//! | `clip.restoreCaptionsFromSource` | Clip ▸ Restore Captions from Source Clip (always disabled: no embedded caption reader) |
//! | `clip.updateMetadata` | Clip ▸ Update Metadata… (XMP sidecars) |
//! | `clip.generateAudioWaveform` | Clip ▸ Generate Audio Waveform (the frontend rebuilds its peaks) |
//! | `clip.automateToSequence` | Clip ▸ Automate to Sequence… |
//! | `help.systemReport` | query behind Help ▸ System Compatibility Report… |

use std::collections::BTreeSet;

use filmcraft_project::{BinEntry, FindOp, FindQuery, FindRow, ItemId, ItemKind, MarkerKind, MediaRef, Project, SearchBin, TrackKind};
use filmcraft_time::{Tick, TimeDisplay, TimeRange};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::commands::{CommandSpec, always, bad, bool_p, has_project_selection, has_seq, str_p, u64_p};
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

fn query(
    id: &'static str,
    label: &'static str,
    menu: &'static [&'static str],
    shortcut: Option<&'static str>,
    params: &'static str,
    enabled: Enabled,
    run: Run,
) -> CommandSpec {
    CommandSpec { id, label, menu, shortcut, params, enabled, run, journal: false }
}

const FIND_PARAMS: &str = r#"{"scope":"project|timeline"?,"column":str?,"operator":"contains|matches|beginsWith|endsWith|doesNotContain"?,"text":str?,"rows":[{"column":str,"operator":str,"text":str}]?,"matchAll":bool=true,"caseSensitive":bool=false,"in":"all|clips|markers"?}"#;

pub(crate) fn commands() -> Vec<CommandSpec> {
    vec![
        // ---------------- File ----------------
        spec(
            "file.newSearchBin",
            "Search Bin",
            &["File", "New"],
            None,
            r#"{"name":str?,"column":str?,"operator":str?,"text":str?,"rows":[..]?,"matchAll":bool?,"caseSensitive":bool?}"#,
            always,
            new_search_bin,
        ),
        query("project.searchBinItems", "Search Bin Contents", &[], None, r#"{"bin":id}"#, always, search_bin_items),
        spec(
            "project.editSearchBin",
            "Edit Search Bin",
            &[],
            None,
            r#"{"bin":id,"name":str?,"column":str?,"operator":str?,"text":str?,"rows":[..]?,"matchAll":bool?,"caseSensitive":bool?}"#,
            always,
            edit_search_bin,
        ),
        spec("project.deleteSearchBin", "Delete Search Bin", &[], None, r#"{"bin":id}"#, always, delete_search_bin),
        spec("file.close", "Close", &["File"], Some("Cmd+W"), r#"{"item":id?}"#, has_seq, |s, p| s.execute("sequence.close", p.clone())),
        spec("file.closeAllProjects", "Close All Projects", &["File"], None, r#"{"force":bool?}"#, always, |s, p| s.execute("file.closeProject", p.clone())),
        spec("file.closeAllOtherProjects", "Close All Other Projects", &["File"], None, "{}", only_one_project, |_, _| {
            Err(EngineError::Other("only one project is open".into()))
        }),
        spec("file.saveAsTemplate", "Save as Template…", &["File"], None, r#"{"name":str?,"path":str?}"#, always, save_as_template),
        query("file.templates", "List Project Templates", &[], None, "{}", always, list_templates),
        spec("file.newProjectFromTemplate", "New Project from Template", &[], None, r#"{"template":name|path,"name":str?}"#, always, new_from_template),
        spec(
            "file.importFromMediaBrowser",
            "Import from Media Browser",
            &["File"],
            Some("Cmd+Alt+I"),
            r#"{"paths":[str]?,"imageSequence":bool?}"#,
            always,
            |s, p| {
                // the Media Browser's selection unless `paths` are given
                let paths = match p.get("paths").and_then(Value::as_array) {
                    Some(a) => a.clone(),
                    None => s.browser.selection.iter().map(|x| json!(x)).collect(),
                };
                if paths.is_empty() {
                    return Err(bad("file.importFromMediaBrowser", "select files in the Media Browser"));
                }
                let seq = p.get("imageSequence").and_then(Value::as_bool).unwrap_or(false);
                s.execute("file.import", json!({"paths": paths, "imageSequence": seq}))
            },
        ),
        // File ▸ Import with "Image Sequence" checked (the UI asks for the first frame).
        spec("file.importImageSequence", "Import Image Sequence…", &["File"], None, r#"{"path":str,"bin":binId?}"#, always, |s, p| {
            let path = p.get("path").and_then(Value::as_str).ok_or_else(|| bad("file.importImageSequence", "need `path` (the first numbered still)"))?;
            let mut q = json!({"paths": [path], "imageSequence": true});
            if let Some(b) = p.get("bin") {
                q["bin"] = b.clone();
            }
            s.execute("file.import", q)
        }),
        spec(
            "file.exportSelectionProject",
            "Selection as FilmCraft Project…",
            &["File", "Export"],
            None,
            r#"{"path":str,"items":[id]?}"#,
            has_project_selection,
            export_selection,
        ),
        spec("file.exportAle", "Avid Log Exchange…", &["File", "Export"], None, r#"{"path":str,"items":[id]?}"#, has_loggable, export_ale),
        query("file.mediaPropertiesFile", "File…", &["File", "Get Media File Properties for"], None, r#"{"path":str}"#, always, media_properties_file),
        query(
            "file.mediaProperties",
            "Selection…",
            &["File", "Get Media File Properties for"],
            Some("Cmd+Shift+H"),
            r#"{"items":[id]?}"#,
            has_media_selection,
            media_properties,
        ),
        spec(
            "file.projectSettings.general",
            "General…",
            &["File", "Project Settings"],
            None,
            r#"{"renderer":"gpu|software"?,"videoDisplay":"timecode|feet35|feet16|frames"?,"audioDisplay":"samples|milliseconds"?,"captureFormat":"DV|HDV"?,"titleSafe":[h,v]?,"actionSafe":[h,v]?}"#,
            always,
            project_settings_general,
        ),
        spec(
            "file.projectSettings.scratchDisks",
            "Scratch Disks…",
            &["File", "Project Settings"],
            None,
            r#"{"captured":path|null?,"videoPreviews":path|null?,"audioPreviews":path|null?,"autoSave":path|null?}"#,
            always,
            project_settings_scratch,
        ),
        // ---------------- Edit ----------------
        spec("edit.find", "Find…", &["Edit"], Some("Cmd+F"), FIND_PARAMS, always, find),
        spec("edit.findNext", "Find Next", &["Edit"], None, "{}", has_find, find_next),
        spec("edit.editOriginal", "Edit Original", &["Edit"], Some("Cmd+E"), r#"{"items":[id]?}"#, has_original, edit_original),
        // ---------------- Clip ----------------
        spec(
            "clip.editOffline",
            "Edit Offline…",
            &["Clip"],
            None,
            r#"{"item":id?,"mediaName":str?,"tapeName":str?,"description":str?,"scene":str?,"shot":str?,"logNote":str?}"#,
            has_offline_selection,
            edit_offline,
        ),
        query("clip.sourceSettings", "Source Settings…", &["Clip"], None, r#"{"item":id?}"#, has_media_selection, source_settings),
        spec("clip.restoreCaptionsFromSource", "Restore Captions from Source Clip", &["Clip"], None, "{}", no_embedded_captions, |_, _| {
            Err(EngineError::Other(NO_CAPTIONS.into()))
        }),
        spec("clip.updateMetadata", "Update Metadata…", &["Clip"], None, r#"{"items":[id]?}"#, has_file_selection, update_metadata),
        query("clip.generateAudioWaveform", "Generate Audio Waveform", &["Clip"], None, r#"{"items":[id]?}"#, has_audio_selection, generate_waveform),
        spec(
            "clip.automateToSequence",
            "Automate to Sequence…",
            &["Clip"],
            None,
            r#"{"items":[id]?,"ordering":"sort|selection","placement":"sequentially|unnumberedMarkers","method":"insert|overwrite","overlapFrames":n=30,"stillFrames":n?,"videoTransition":bool=true,"audioTransition":bool=true,"ignoreAudio":bool=false,"ignoreVideo":bool=false}"#,
            can_automate,
            automate_to_sequence,
        ),
        // ---------------- Help ----------------
        query("help.systemReport", "System Compatibility Report", &[], None, "{}", always, |_, _| Ok(system_report())),
    ]
}

// ---------------------------------------------------------------------------------------------
// menu layout
// ---------------------------------------------------------------------------------------------

enum At {
    After(&'static str),
    Before(&'static str),
}

/// Add this module's, `sequence_extras`' and `scene_detect`'s commands and arrange them (and a few
/// existing ones) in Premiere's menu order.
pub(crate) fn apply_layout(v: &mut Vec<CommandSpec>) {
    let mut pool = commands();
    pool.extend(crate::sequence_extras::commands());
    pool.extend(crate::scene_detect::commands());
    // Apply Default Transitions to Selection works on selected clips too (⇧D), not only on
    // selected edit points; the Trim Monitor button keeps using the same id.
    if let Some(c) = v.iter_mut().find(|c| c.id == "trim.applyDefaultTransition")
        && let Some(i) = pool.iter().position(|c| c.id == "sequence.applyDefaultTransitionsToSelection")
    {
        let new = pool.remove(i);
        c.enabled = new.enabled;
        c.run = new.run;
        c.params = new.params;
    }
    // Project Settings ▸ Ingest Settings…
    if let Some(c) = v.iter_mut().find(|c| c.id == "project.ingestSettings") {
        c.menu = &["File", "Project Settings"];
    }
    let layout: Vec<(At, Vec<&'static str>)> = vec![
        (At::After("file.newBinFromSelection"), vec!["file.newSearchBin"]),
        (At::Before("file.closeProject"), vec!["file.close"]),
        (At::After("file.closeProject"), vec!["file.closeAllProjects", "file.closeAllOtherProjects"]),
        (At::After("file.saveCopy"), vec!["file.saveAsTemplate"]),
        (At::After("file.exportEdl"), vec!["file.exportSelectionProject", "file.exportAle", "file.exportOtio", "file.exportFcp7Xml", "file.exportFcpxml"]),
        (
            At::Before("file.projectManager"),
            vec![
                "file.mediaPropertiesFile",
                "file.mediaProperties",
                "file.projectSettings.general",
                "file.projectSettings.scratchDisks",
                "project.ingestSettings",
            ],
        ),
        (At::After("edit.deselectAll"), vec!["edit.find", "edit.findNext"]),
        (At::After("edit.consolidateDuplicates"), vec!["edit.editOriginal"]),
        (At::After("clip.editSubclip"), vec!["clip.editOffline", "clip.sourceSettings"]),
        (At::After("clip.speedDuration"), vec!["clip.sceneEditDetection"]),
        (
            At::After("clip.replaceFromBin"),
            vec!["clip.restoreCaptionsFromSource", "clip.updateMetadata", "clip.generateAudioWaveform", "clip.automateToSequence"],
        ),
        (At::After("sequence.applyAudioTransition"), vec!["trim.applyDefaultTransition"]),
        (At::After("sequence.showThroughEdits"), vec!["sequence.normalizeMixTrack"]),
        (At::After("sequence.makeSubsequence"), vec!["sequence.transcribe", "sequence.simplify"]),
        (At::After("captions.showAll"), vec!["captions.showActiveOnly"]),
        (At::After("markers.addChapter"), vec!["markers.addFlashCue"]),
        (At::Before("file.close"), vec!["file.open"]),
        (At::After("media.makeOffline"), vec!["file.importFromMediaBrowser", "file.import", "file.importImageSequence"]),
    ];
    for (at, ids) in layout {
        let mut run = Vec::new();
        for id in ids {
            if let Some(i) = pool.iter().position(|c| c.id == id) {
                run.push(pool.remove(i));
            } else if let Some(i) = v.iter().position(|c| c.id == id) {
                run.push(v.remove(i));
            }
        }
        let idx = match at {
            At::After(a) => v.iter().position(|c| c.id == a).map(|i| i + 1),
            At::Before(b) => v.iter().position(|c| c.id == b),
        }
        .unwrap_or(v.len());
        v.splice(idx..idx, run);
    }
    v.extend(pool);
    reorder(v, &SEQUENCE_MENU);
}

/// The Sequence menu in Premiere's order (submenu items where their submenu goes).
const SEQUENCE_MENU: [&str; 34] = [
    "sequence.settings",
    "sequence.renderEffectsInToOut",
    "sequence.renderInToOut",
    "sequence.renderSelection",
    "sequence.renderAudio",
    "sequence.deleteRenderFiles",
    "sequence.deleteRenderFilesInToOut",
    "sequence.matchFrame",
    "sequence.reverseMatchFrame",
    "sequence.addEdit",
    "sequence.addEditAllTracks",
    "trim.edit",
    "sequence.applyVideoTransition",
    "sequence.applyAudioTransition",
    "trim.applyDefaultTransition",
    "sequence.lift",
    "sequence.extract",
    "sequence.closeGap",
    "sequence.goToNextGap",
    "sequence.goToPrevGap",
    "sequence.goToNextGapInTrack",
    "sequence.goToPrevGapInTrack",
    "sequence.snap",
    "sequence.linkedSelection",
    "sequence.selectionFollowsPlayhead",
    "sequence.showThroughEdits",
    "sequence.normalizeMixTrack",
    "sequence.makeSubsequence",
    "sequence.transcribe",
    "sequence.simplify",
    "sequence.addTracks",
    "sequence.deleteTracks",
    "sequence.colorSettings",
    "mixer.addSubmix",
];

/// Put the commands `ids` (those present) in this order, at the position of the first of them.
fn reorder(v: &mut Vec<CommandSpec>, ids: &[&str]) {
    let Some(at) = v.iter().position(|c| ids.contains(&c.id)) else { return };
    let mut run = Vec::new();
    for id in ids {
        if let Some(i) = v.iter().position(|c| c.id == *id) {
            run.push(v.remove(i));
        }
    }
    let at = at.min(v.len());
    v.splice(at..at, run);
}

// ---------------------------------------------------------------------------------------------
// enablement
// ---------------------------------------------------------------------------------------------

fn only_one_project(_: &Session) -> std::result::Result<(), String> {
    Err("only one project is open".into())
}

const NO_CAPTIONS: &str = "the clip's media has no embedded captions FilmCraft can read (embedded CEA-608/708 caption streams are not supported yet)";

fn no_embedded_captions(_: &Session) -> std::result::Result<(), String> {
    Err(NO_CAPTIONS.into())
}

fn has_find(s: &Session) -> std::result::Result<(), String> {
    if s.state.find.is_some() { Ok(()) } else { Err("use Find… first".into()) }
}

/// Project items the command works on: `items`, else the Project panel selection, else the media
/// of the selected timeline clips.
fn targets(s: &Session, p: &Value) -> Vec<ItemId> {
    if let Some(a) = p.get("items").and_then(Value::as_array) {
        return a.iter().filter_map(|v| v.as_u64().map(ItemId)).filter(|i| s.project.items.contains_key(i)).collect();
    }
    if let Some(i) = u64_p(p, "item") {
        return vec![ItemId(i)];
    }
    if !s.state.project_selection.is_empty() {
        return s.state.project_selection.clone();
    }
    let mut out = Vec::new();
    if let Some(q) = s.active_sequence() {
        for c in &s.state.selection {
            if let Some((_, it)) = q.find_item(*c)
                && !out.contains(&it.item)
            {
                out.push(it.item);
            }
        }
    }
    out
}

/// The media clip behind an item (subclips resolve to their parent).
fn media_of(p: &Project, id: ItemId) -> Option<(ItemId, &filmcraft_project::MediaClip)> {
    p.resolve_media(id).map(|(root, media, _)| (root, media))
}

fn file_path(m: &filmcraft_project::MediaClip) -> Option<&str> {
    match &m.media {
        MediaRef::File { path } => Some(path),
        MediaRef::Generator(_) => None,
    }
}

fn has_media_selection(s: &Session) -> std::result::Result<(), String> {
    if targets(s, &Value::Null).iter().any(|i| media_of(&s.project, *i).is_some()) { Ok(()) } else { Err("select a clip".into()) }
}
fn has_file_selection(s: &Session) -> std::result::Result<(), String> {
    if targets(s, &Value::Null).iter().any(|i| media_of(&s.project, *i).is_some_and(|(_, m)| file_path(m).is_some())) {
        Ok(())
    } else {
        Err("select a clip with a media file".into())
    }
}
fn has_original(s: &Session) -> std::result::Result<(), String> {
    has_file_selection(s)
}
fn has_audio_selection(s: &Session) -> std::result::Result<(), String> {
    if targets(s, &Value::Null).iter().any(|i| media_of(&s.project, *i).is_some_and(|(_, m)| m.info.has_audio())) {
        Ok(())
    } else {
        Err("select a clip with audio".into())
    }
}
fn is_offline(s: &Session, id: ItemId) -> bool {
    media_of(&s.project, id).is_some_and(|(mid, m)| m.offline || s.media.offline_status(mid).is_some())
}
fn has_offline_selection(s: &Session) -> std::result::Result<(), String> {
    if targets(s, &Value::Null).iter().any(|i| is_offline(s, *i)) { Ok(()) } else { Err("select an offline clip".into()) }
}
fn has_loggable(s: &Session) -> std::result::Result<(), String> {
    if s.project.items.keys().any(|i| media_of(&s.project, *i).is_some()) { Ok(()) } else { Err("the project has no clips to log".into()) }
}
fn can_automate(s: &Session) -> std::result::Result<(), String> {
    has_seq(s)?;
    has_project_selection(s)?;
    if s.state.project_selection.iter().any(|i| s.project.item(*i).is_some_and(|it| !matches!(it.kind, ItemKind::Graphic { .. }))) {
        Ok(())
    } else {
        Err("select clips in the Project panel".into())
    }
}

// ---------------------------------------------------------------------------------------------
// queries (Find, search bins)
// ---------------------------------------------------------------------------------------------

/// Read a query from params: `rows`, or one `column` / `operator` / `text` row.
pub fn query_p(p: &Value, cmd: &str) -> Result<FindQuery> {
    let row = |v: &Value| -> Result<FindRow> {
        let op = match str_p(v, "operator").or(str_p(v, "op")) {
            Some(o) => FindOp::from_name(o).ok_or_else(|| bad(cmd, format!("unknown operator `{o}`")))?,
            None => FindOp::Contains,
        };
        Ok(FindRow { column: str_p(v, "column").unwrap_or("All").to_string(), op, text: str_p(v, "text").unwrap_or_default().to_string() })
    };
    let rows = match p.get("rows").and_then(Value::as_array) {
        Some(a) => a.iter().map(row).collect::<Result<Vec<_>>>()?,
        None => vec![row(p)?],
    };
    Ok(FindQuery { rows, match_all: bool_p(p, "matchAll").unwrap_or(true), case_sensitive: bool_p(p, "caseSensitive").unwrap_or(false) })
}

fn search_bin_json(p: &Project, b: &SearchBin) -> Value {
    json!({"bin": b.id.0, "name": b.name, "query": b.query, "items": b.query.find(p).iter().map(|i| i.0).collect::<Vec<_>>()})
}

fn new_search_bin(s: &mut Session, p: &Value) -> Result<Value> {
    let q = query_p(p, "file.newSearchBin")?;
    if q.is_empty() {
        return Err(bad("file.newSearchBin", "enter text to search for"));
    }
    let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| q.rows.iter().find(|r| !r.text.is_empty()).map(|r| r.text.clone()).unwrap_or_default());
    let b = s.edit("New Search Bin", |pr, _| {
        let id = filmcraft_project::BinId(pr.alloc_id());
        let b = SearchBin { id, name, query: q };
        pr.search_bins.push(b.clone());
        Ok(b)
    })?;
    Ok(search_bin_json(&s.project, &b))
}

fn bin_p(s: &Session, p: &Value, cmd: &str) -> Result<SearchBin> {
    let id = u64_p(p, "bin").ok_or_else(|| bad(cmd, "need `bin`"))?;
    s.project.search_bins.iter().find(|b| b.id.0 == id).cloned().ok_or_else(|| bad(cmd, format!("no search bin {id}")))
}

fn search_bin_items(s: &mut Session, p: &Value) -> Result<Value> {
    let b = bin_p(s, p, "project.searchBinItems")?;
    Ok(search_bin_json(&s.project, &b))
}

fn edit_search_bin(s: &mut Session, p: &Value) -> Result<Value> {
    let b = bin_p(s, p, "project.editSearchBin")?;
    let q = if p.get("rows").is_some() || p.get("text").is_some() { Some(query_p(p, "project.editSearchBin")?) } else { None };
    let name = str_p(p, "name").map(str::to_string);
    let out = s.edit("Edit Search Bin", |pr, _| {
        let x = pr.search_bins.iter_mut().find(|x| x.id == b.id).ok_or_else(|| bad("project.editSearchBin", "gone"))?;
        if let Some(q) = q {
            x.query = q;
        }
        if let Some(n) = name {
            x.name = n;
        }
        Ok(x.clone())
    })?;
    Ok(search_bin_json(&s.project, &out))
}

fn delete_search_bin(s: &mut Session, p: &Value) -> Result<Value> {
    let b = bin_p(s, p, "project.deleteSearchBin")?;
    s.edit("Delete Search Bin", |pr, _| {
        pr.search_bins.retain(|x| x.id != b.id);
        Ok(())
    })?;
    Ok(Value::Null)
}

/// The last Find: Find Next walks its results.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FindState {
    /// `project` or `timeline`.
    pub scope: String,
    pub query: FindQuery,
    /// Timeline: search clip names, marker names / comments, or both (`all`).
    pub within: String,
    /// Index of the current result.
    pub index: usize,
}

/// One timeline Find hit.
#[derive(Clone, Debug, PartialEq)]
enum Hit {
    Clip(filmcraft_project::ClipId, Tick),
    Marker(filmcraft_project::MarkerId, Tick),
}

fn timeline_hits(s: &Session, f: &FindState) -> Vec<Hit> {
    let Some(q) = s.active_sequence() else { return Vec::new() };
    let test = |text: &str| {
        f.query.rows.iter().filter(|r| !r.text.is_empty()).fold(f.query.match_all, |acc, r| {
            let (h, n) = if f.query.case_sensitive { (text.to_string(), r.text.clone()) } else { (text.to_lowercase(), r.text.to_lowercase()) };
            let ok = match r.op {
                FindOp::Contains => h.contains(&n),
                FindOp::Matches => h == n,
                FindOp::BeginsWith => h.starts_with(&n),
                FindOp::EndsWith => h.ends_with(&n),
                FindOp::DoesNotContain => !h.contains(&n),
            };
            if f.query.match_all { acc && ok } else { acc || ok }
        })
    };
    if f.query.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    if f.within != "markers" {
        for t in q.all_tracks() {
            for it in &t.items {
                let name = if it.name.is_empty() { s.project.item(it.item).map(|i| i.name.clone()).unwrap_or_default() } else { it.name.clone() };
                if test(&name) {
                    hits.push(Hit::Clip(it.id, it.start));
                }
            }
        }
    }
    if f.within != "clips" {
        for m in &q.markers {
            if test(&m.name) || (!m.comment.is_empty() && test(&m.comment)) {
                hits.push(Hit::Marker(m.id, m.start));
            }
        }
    }
    hits.sort_by_key(|h| match h {
        Hit::Clip(_, t) | Hit::Marker(_, t) => *t,
    });
    hits
}

/// Select / reveal result `f.index`.
fn show_result(s: &mut Session, f: &FindState) -> Result<Value> {
    if f.scope == "timeline" {
        let hits = timeline_hits(s, f);
        let Some(h) = hits.get(f.index % hits.len().max(1)).cloned() else { return Err(EngineError::Other("no matches".into())) };
        let out = match h {
            Hit::Clip(c, t) => {
                s.state.selection = vec![c];
                s.set_playhead(t);
                json!({"clip": c.0, "time": t.0})
            }
            Hit::Marker(m, t) => {
                s.set_playhead(t);
                json!({"marker": m.0, "time": t.0})
            }
        };
        let mut out = out;
        out["matches"] = json!(hits.len());
        out["index"] = json!(f.index % hits.len().max(1));
        return Ok(out);
    }
    let items = f.query.find(&s.project);
    let Some(id) = items.get(f.index % items.len().max(1)).copied() else { return Err(EngineError::Other("no matches".into())) };
    s.state.project_selection = vec![id];
    Ok(json!({"item": id.0, "matches": items.len(), "index": f.index % items.len().max(1), "items": items.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

fn find(s: &mut Session, p: &Value) -> Result<Value> {
    let scope = str_p(p, "scope").unwrap_or("project").to_ascii_lowercase();
    if !matches!(scope.as_str(), "project" | "timeline") {
        return Err(bad("edit.find", "`scope` is project or timeline"));
    }
    let within = str_p(p, "in").unwrap_or("all").to_ascii_lowercase();
    if scope == "timeline" {
        has_seq(s).map_err(EngineError::Other)?;
    }
    let mut query = query_p(p, "edit.find")?;
    // timeline Find searches names: an `All` / empty column is fine
    if scope == "timeline" {
        query.rows.iter_mut().for_each(|r| r.column = "Name".into());
    }
    if query.is_empty() {
        return Err(bad("edit.find", "enter text to find"));
    }
    let f = FindState { scope, query, within, index: 0 };
    s.state.find = Some(f.clone());
    show_result(s, &f)
}

fn find_next(s: &mut Session, _: &Value) -> Result<Value> {
    let mut f = s.state.find.clone().ok_or_else(|| EngineError::Other("use Find… first".into()))?;
    f.index += 1;
    let r = show_result(s, &f);
    if let Ok(v) = &r {
        f.index = v["index"].as_u64().unwrap_or(0) as usize;
        s.state.find = Some(f);
    }
    r
}

// ---------------------------------------------------------------------------------------------
// templates, closing
// ---------------------------------------------------------------------------------------------

/// Where project templates live: `<data dir>/Templates` (None without a data directory).
pub fn templates_dir(s: &Session) -> Option<std::path::PathBuf> {
    s.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("Templates"))
}

fn save_as_template(s: &mut Session, p: &Value) -> Result<Value> {
    let name = str_p(p, "name").map(str::to_string).unwrap_or_else(|| s.project.name.clone());
    let path = match str_p(p, "path") {
        Some(x) => x.to_string(),
        None => {
            let dir = templates_dir(s).ok_or_else(|| bad("file.saveAsTemplate", "need `path` (no data directory for templates)"))?;
            let _ = std::fs::create_dir_all(&dir);
            dir.join(format!("{}.fcproj", sanitize(&name))).to_string_lossy().to_string()
        }
    };
    let mut proj = (*s.project).clone();
    proj.name = name.clone();
    proj.root.name = name.clone();
    s.services.write_file(&path, &filmcraft_format::encode(&proj, false)).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "name": name}))
}

fn sanitize(n: &str) -> String {
    n.chars().map(|c| if c.is_alphanumeric() || " -_().".contains(c) { c } else { '_' }).collect::<String>().trim().to_string()
}

fn list_templates(s: &mut Session, _: &Value) -> Result<Value> {
    let Some(dir) = templates_dir(s) else { return Ok(json!([])) };
    let mut v: Vec<Value> = std::fs::read_dir(&dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.path().extension().is_some_and(|x| x == "fcproj"))
                .map(|e| json!({"name": e.path().file_stem().map(|x| x.to_string_lossy().to_string()), "path": e.path().to_string_lossy()}))
                .collect()
        })
        .unwrap_or_default();
    v.sort_by(|a, b| a["name"].as_str().cmp(&b["name"].as_str()));
    Ok(Value::Array(v))
}

fn new_from_template(s: &mut Session, p: &Value) -> Result<Value> {
    let t = str_p(p, "template").ok_or_else(|| bad("file.newProjectFromTemplate", "need `template`"))?;
    let path = if t.ends_with(".fcproj") || t.contains('/') || t.contains('\\') {
        t.to_string()
    } else {
        templates_dir(s).map(|d| d.join(format!("{}.fcproj", sanitize(t))).to_string_lossy().to_string()).unwrap_or_else(|| t.to_string())
    };
    let bytes = s.services.read_file(&path).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    let mut proj = filmcraft_format::decode(&bytes).map_err(|e| EngineError::Other(e.to_string()))?.project;
    if s.is_dirty() && !bool_p(p, "force").unwrap_or(false) {
        return Err(EngineError::Other("the project has unsaved changes: save it first, or pass {\"force\": true} to discard them".into()));
    }
    let name = str_p(p, "name").unwrap_or("Untitled").to_string();
    proj.name = name.clone();
    proj.root.name = name;
    s.project = std::sync::Arc::new(proj);
    s.history = Default::default();
    s.history.limit = 200;
    s.state = Session::default().state;
    s.state.active_sequence = s.project.sequences().next().map(|i| i.id);
    s.state.open_sequences = s.state.active_sequence.into_iter().collect();
    if s.path.take().is_some() {
        s.previews.reset_temp();
    }
    s.media.clear();
    s.revision += 1;
    s.saved_revision = s.revision;
    s.events.push(crate::Event::ProjectChanged { revision: s.revision });
    if let Some(q) = s.state.active_sequence {
        s.events.push(crate::Event::OpenSequence(q));
    }
    Ok(json!({"items": s.project.items.len()}))
}

// ---------------------------------------------------------------------------------------------
// exports
// ---------------------------------------------------------------------------------------------

/// The selected items plus everything they need: sequence contents (recursively), subclip
/// parents, multi-camera / merged-clip sources and graphic canvases.
pub fn closure(p: &Project, roots: &[ItemId]) -> BTreeSet<ItemId> {
    let mut keep: BTreeSet<ItemId> = BTreeSet::new();
    let mut todo: Vec<ItemId> = roots.to_vec();
    while let Some(id) = todo.pop() {
        if !keep.insert(id) {
            continue;
        }
        let Some(it) = p.item(id) else { continue };
        match &it.kind {
            ItemKind::Subclip { parent, .. } => todo.push(*parent),
            ItemKind::Sequence(q) => {
                for t in q.all_tracks() {
                    todo.extend(t.items.iter().map(|i| i.item));
                }
                let mut ids = BTreeSet::new();
                for v in [serde_json::to_value(&q.multicam).unwrap_or_default(), serde_json::to_value(&q.merged).unwrap_or_default()] {
                    crate::clip_ops::collect_ids(&v, &mut ids, p);
                }
                todo.extend(ids);
            }
            _ => {}
        }
    }
    keep
}

/// A copy of `p` with only `keep` (bins that end up empty are dropped).
pub fn project_subset(p: &Project, keep: &BTreeSet<ItemId>) -> Project {
    fn prune(b: &mut filmcraft_project::Bin, keep: &BTreeSet<ItemId>) {
        b.children.retain_mut(|c| match c {
            BinEntry::Item(i) => keep.contains(i),
            BinEntry::Bin(x) => {
                prune(x, keep);
                !x.children.is_empty()
            }
        });
    }
    let mut out = p.clone();
    out.items.retain(|id, _| keep.contains(id));
    prune(&mut out.root, keep);
    out.transcripts.retain(|id, _| keep.contains(id));
    out.generated.retain(|id, _| keep.contains(id));
    out.search_bins.clear();
    out
}

fn export_selection(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("file.exportSelectionProject", "need `path`"))?.to_string();
    let roots = match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect(),
        None => s.state.project_selection.clone(),
    };
    if roots.is_empty() {
        return Err(bad("file.exportSelectionProject", "select items in the Project panel"));
    }
    let keep = closure(&s.project, &roots);
    let mut sub = project_subset(&s.project, &keep);
    let name = std::path::Path::new(&path).file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| sub.name.clone());
    sub.name = name.clone();
    sub.root.name = name;
    s.services.write_file(&path, &filmcraft_format::encode(&sub, false)).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "items": keep.iter().map(|i| i.0).collect::<Vec<_>>()}))
}

fn export_ale(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("file.exportAle", "need `path`"))?.to_string();
    let mut items: Vec<ItemId> = match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect(),
        None => s.state.project_selection.clone(),
    };
    if items.is_empty() {
        // the whole project, in Project panel order
        s.project.root.all_items(&mut items);
    }
    let doc = filmcraft_interchange::ale::export_items(&s.project, &items);
    if doc.rows.is_empty() {
        return Err(EngineError::Other("none of the items is a clip that can be logged".into()));
    }
    s.services.write_file(&path, filmcraft_interchange::ale::write(&doc).as_bytes()).map_err(|e| EngineError::Other(format!("{path}: {e}")))?;
    Ok(json!({"path": path, "clips": doc.rows.len()}))
}

// ---------------------------------------------------------------------------------------------
// media properties, settings
// ---------------------------------------------------------------------------------------------

/// File properties of a probed media file (Get Media File Properties).
pub fn info_json(name: &str, path: Option<&str>, info: &filmcraft_media::MediaInfo) -> Value {
    let rate = info.frame_rate();
    let secs = info.duration.seconds();
    let mut v = json!({
        "name": name,
        "path": path,
        "type": format!("{:?}", info.kind),
        "container": info.container,
        "fileSize": info.file_size,
        "duration": {"ticks": info.duration.0, "seconds": secs, "timecode": filmcraft_time::format_timecode_frames(rate.frame_at(info.duration), rate, false)},
        "startTimecode": info.start_timecode.map(|f| filmcraft_time::format_timecode_frames(f, rate, false)),
        "dataRateKbps": info.file_size.filter(|_| secs > 0.0).map(|b| (b as f64 * 8.0 / secs / 1000.0).round()),
    });
    if let Some(vs) = &info.video {
        v["video"] = json!({
            "codec": vs.codec, "width": vs.width, "height": vs.height, "frameRate": vs.frame_rate.as_f64(), "frameRateLabel": vs.frame_rate.label(),
            "pixelAspectRatio": format!("{}:{}", vs.par.0, vs.par.1), "pixelFormat": vs.pixel_format, "alpha": vs.has_alpha,
            "bitrate": vs.bitrate, "color": format!("{:?} / {:?} / {:?}", vs.color.primaries, vs.color.transfer, vs.color.matrix),
        });
    }
    if let Some(a) = &info.audio {
        v["audio"] = json!({"codec": a.codec, "sampleRate": a.sample_rate, "channels": a.channels, "bitsPerSample": a.bits_per_sample});
    }
    v
}

fn media_properties(s: &mut Session, p: &Value) -> Result<Value> {
    let mut out = Vec::new();
    for id in targets(s, p) {
        let Some((mid, m)) = media_of(&s.project, id) else { continue };
        let name = s.project.item(id).map(|i| i.name.clone()).unwrap_or_default();
        let mut v = info_json(&name, file_path(m), &m.info);
        v["item"] = json!(id.0);
        v["offline"] = json!(m.offline || s.media.offline_status(mid).is_some());
        out.push(v);
    }
    if out.is_empty() {
        return Err(EngineError::Other("select a clip".into()));
    }
    Ok(Value::Array(out))
}

fn media_properties_file(s: &mut Session, p: &Value) -> Result<Value> {
    let path = str_p(p, "path").ok_or_else(|| bad("file.mediaPropertiesFile", "need `path`"))?;
    let src = s.media.open_file(path, &*s.services)?;
    let name = std::path::Path::new(path).file_name().map(|x| x.to_string_lossy().to_string()).unwrap_or_default();
    Ok(info_json(&name, Some(path), src.info()))
}

fn pair_p(p: &Value, k: &str) -> Option<(f64, f64)> {
    let a = p.get(k)?.as_array()?;
    Some((a.first()?.as_f64()?.clamp(0.0, 50.0), a.get(1)?.as_f64()?.clamp(0.0, 50.0)))
}

pub const RENDERER_GPU: &str = "FilmCraft GPU Acceleration (wgpu)";
pub const RENDERER_SOFTWARE: &str = "FilmCraft Software Only (CPU)";

fn general_json(st: &filmcraft_project::ProjectSettings) -> Value {
    json!({
        "renderer": st.renderer,
        "renderers": [RENDERER_GPU, RENDERER_SOFTWARE],
        "videoDisplay": st.video_display.label(),
        "audioDisplay": if st.audio_display_samples { "Audio Samples" } else { "Milliseconds" },
        "captureFormat": st.capture_format,
        "titleSafe": [st.title_safe.0, st.title_safe.1],
        "actionSafe": [st.action_safe.0, st.action_safe.1],
    })
}

fn project_settings_general(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "file.projectSettings.general";
    let mut st = s.project.settings.clone();
    if let Some(r) = str_p(p, "renderer") {
        st.renderer = match r.to_ascii_lowercase().as_str() {
            "gpu" => RENDERER_GPU.into(),
            "software" | "cpu" => RENDERER_SOFTWARE.into(),
            _ if r == RENDERER_GPU || r == RENDERER_SOFTWARE => r.into(),
            _ => return Err(bad(cmd, "`renderer` is gpu or software")),
        };
    }
    if let Some(d) = str_p(p, "videoDisplay") {
        let k = d.to_ascii_lowercase().replace([' ', '-'], "");
        st.video_display = TimeDisplay::ALL
            .into_iter()
            .find(|x| x.label().to_ascii_lowercase().replace([' ', '-'], "") == k || format!("{x:?}").to_ascii_lowercase() == k)
            .ok_or_else(|| bad(cmd, format!("unknown video display format `{d}`")))?;
    }
    if let Some(a) = str_p(p, "audioDisplay") {
        st.audio_display_samples = match a.to_ascii_lowercase().as_str() {
            "samples" | "audio samples" => true,
            "milliseconds" | "ms" => false,
            _ => return Err(bad(cmd, "`audioDisplay` is samples or milliseconds")),
        };
    }
    if let Some(c) = str_p(p, "captureFormat") {
        if !matches!(c, "DV" | "HDV") {
            return Err(bad(cmd, "`captureFormat` is DV or HDV"));
        }
        st.capture_format = c.into();
    }
    if let Some(t) = pair_p(p, "titleSafe") {
        st.title_safe = t;
    }
    if let Some(a) = pair_p(p, "actionSafe") {
        st.action_safe = a;
    }
    if st != s.project.settings {
        s.edit("Project Settings", |pr, _| {
            pr.settings = st;
            Ok(())
        })?;
    }
    Ok(general_json(&s.project.settings))
}

/// Where each scratch disk resolves to now (`Same as Project` = next to the project file, or the
/// data directory for an unsaved project).
pub fn scratch_paths(s: &Session) -> Value {
    let sc = &s.project.settings.scratch;
    let project_dir = s.path.as_deref().and_then(|p| std::path::Path::new(p).parent()).map(|d| d.to_string_lossy().to_string());
    let same = |o: &Option<String>| o.clone().or(project_dir.clone()).unwrap_or_else(|| "(data directory until the project is saved)".into());
    json!({
        "captured": {"setting": sc.captured, "path": same(&sc.captured)},
        "videoPreviews": {"setting": sc.video_previews, "path": previews_dir(s).map(|d| d.to_string_lossy().to_string()).unwrap_or_else(|| same(&sc.video_previews))},
        "audioPreviews": {"setting": sc.audio_previews, "path": same(&sc.audio_previews)},
        "autoSave": {"setting": sc.auto_save, "path": sc.auto_save.clone().or_else(|| s.path.as_deref().map(|p| filmcraft_format::autosave::auto_save_dir(std::path::Path::new(p)).to_string_lossy().to_string())).unwrap_or_else(|| same(&None))},
    })
}

/// Render previews folder of the open project: Scratch Disks ▸ Video Previews / `<project
/// name>`, else `FilmCraft Previews/<project name>` next to the project (None while unsaved).
pub fn previews_dir(s: &Session) -> Option<std::path::PathBuf> {
    s.path.as_deref().map(|p| previews_dir_for(&s.project, p))
}

/// [`previews_dir`] for `project` saved at `path`.
pub fn previews_dir_for(project: &Project, path: &str) -> std::path::PathBuf {
    match &project.settings.scratch.video_previews {
        Some(d) if !d.is_empty() => {
            let stem = std::path::Path::new(path).file_stem().map(|x| x.to_string_lossy().to_string()).unwrap_or_else(|| "Untitled".into());
            std::path::Path::new(d).join(stem)
        }
        _ => crate::previews::dir_for_project(path),
    }
}

fn project_settings_scratch(s: &mut Session, p: &Value) -> Result<Value> {
    let mut sc = s.project.settings.scratch.clone();
    for (k, slot) in
        [("captured", &mut sc.captured), ("videoPreviews", &mut sc.video_previews), ("audioPreviews", &mut sc.audio_previews), ("autoSave", &mut sc.auto_save)]
    {
        match p.get(k) {
            None => {}
            Some(Value::Null) => *slot = None,
            Some(Value::String(x)) if x.is_empty() || x.eq_ignore_ascii_case("same as project") => *slot = None,
            Some(Value::String(x)) => *slot = Some(x.clone()),
            Some(_) => return Err(bad("file.projectSettings.scratchDisks", format!("`{k}` is a folder path or null"))),
        }
    }
    if sc != s.project.settings.scratch {
        s.edit("Scratch Disks", |pr, _| {
            pr.settings.scratch = sc;
            Ok(())
        })?;
        s.previews_follow_path();
    }
    Ok(scratch_paths(s))
}

// ---------------------------------------------------------------------------------------------
// Edit Original, clip items
// ---------------------------------------------------------------------------------------------

fn edit_original(s: &mut Session, p: &Value) -> Result<Value> {
    let mut paths = Vec::new();
    for id in targets(s, p) {
        if let Some(path) = media_of(&s.project, id).and_then(|(_, m)| file_path(m))
            && !paths.iter().any(|x: &String| x == path)
        {
            paths.push(path.to_string());
        }
    }
    if paths.is_empty() {
        return Err(EngineError::Other("select a clip with a media file".into()));
    }
    Ok(json!({"paths": paths}))
}

/// Metadata fields of the Edit Offline dialog: (param, metadata key).
pub const OFFLINE_FIELDS: [(&str, &str); 5] =
    [("tapeName", "Tape Name"), ("description", "Description"), ("scene", "Scene"), ("shot", "Shot"), ("logNote", "Log Note")];

fn edit_offline(s: &mut Session, p: &Value) -> Result<Value> {
    let id = targets(s, p).into_iter().find(|i| is_offline(s, *i)).ok_or_else(|| EngineError::Other("select an offline clip".into()))?;
    let name = str_p(p, "mediaName").map(str::to_string);
    let fields: Vec<(&'static str, String)> = OFFLINE_FIELDS.iter().filter_map(|(k, key)| str_p(p, k).map(|v| (*key, v.to_string()))).collect();
    s.edit("Edit Offline", |pr, _| {
        let it = pr.item_mut(id).ok_or_else(|| bad("clip.editOffline", "no such item"))?;
        if let Some(n) = name.filter(|n| !n.is_empty()) {
            it.name = n;
        }
        for (k, v) in fields {
            if v.is_empty() {
                it.metadata.remove(k);
            } else {
                it.metadata.insert(k.to_string(), v);
            }
        }
        Ok(())
    })?;
    let it = s.project.item(id).ok_or_else(|| bad("clip.editOffline", "no such item"))?;
    Ok(json!({"item": id.0, "name": it.name, "metadata": it.metadata}))
}

fn source_settings(s: &mut Session, p: &Value) -> Result<Value> {
    let id = targets(s, p).into_iter().find(|i| media_of(&s.project, *i).is_some()).ok_or_else(|| EngineError::Other("select a clip".into()))?;
    let (_, m) = media_of(&s.project, id).ok_or_else(|| EngineError::Other("select a clip".into()))?;
    let codec = m.info.video.as_ref().map(|v| v.codec.clone()).or_else(|| m.info.audio.as_ref().map(|a| a.codec.clone())).unwrap_or_default();
    Ok(json!({
        "item": id.0,
        "codec": codec,
        "container": m.info.container,
        "settings": [],
        "message": format!("{} has no source settings in FilmCraft. Source settings apply to camera raw formats (R3D, ARRIRAW, CinemaDNG, ProRes RAW…), which FilmCraft does not decode.", if codec.is_empty() { "This clip".to_string() } else { codec.to_uppercase() }),
    }))
}

fn xml_escape(s: &str) -> String {
    s.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// An XMP packet (ISO 16684-1) with the item's name and log metadata (Dublin Core and the XMP
/// Dynamic Media schema).
pub fn xmp_packet(it: &filmcraft_project::ProjectItem) -> String {
    let meta = |k: &str| it.metadata.iter().find(|(x, _)| x.eq_ignore_ascii_case(k)).map(|(_, v)| xml_escape(v));
    let mut body = format!("   <dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">{}</rdf:li></rdf:Alt></dc:title>\n", xml_escape(&it.name));
    if let Some(d) = meta("Description") {
        body.push_str(&format!("   <dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">{d}</rdf:li></rdf:Alt></dc:description>\n"));
    }
    for (k, tag) in [("Scene", "scene"), ("Shot", "shotName"), ("Log Note", "logComment"), ("Tape Name", "tapeName"), ("Comment", "comment")] {
        if let Some(v) = meta(k) {
            body.push_str(&format!("   <xmpDM:{tag}>{v}</xmpDM:{tag}>\n"));
        }
    }
    format!(
        "<?xpacket begin=\"\u{feff}\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n  <rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\" xmlns:xmpDM=\"http://ns.adobe.com/xmp/1.0/DynamicMedia/\" xmp:CreatorTool=\"FilmCraft\">\n{body}  </rdf:Description>\n </rdf:RDF>\n</x:xmpmeta>\n<?xpacket end=\"w\"?>\n"
    )
}

/// The XMP sidecar of a media file: `<dir>/<stem>.xmp`.
pub fn sidecar_path(media: &str) -> String {
    std::path::Path::new(media).with_extension("xmp").to_string_lossy().to_string()
}

/// Clip ▸ Update Metadata…: write the items' metadata to XMP sidecars next to their media. An
/// existing sidecar written by another application is left alone.
fn update_metadata(s: &mut Session, p: &Value) -> Result<Value> {
    let mut written = Vec::new();
    let mut skipped = Vec::new();
    for id in targets(s, p) {
        let Some(it) = s.project.item(id) else { continue };
        let Some(path) = media_of(&s.project, id).and_then(|(_, m)| file_path(m)) else { continue };
        let side = sidecar_path(path);
        if let Ok(old) = s.services.read_file(&side)
            && !String::from_utf8_lossy(&old).contains("xmp:CreatorTool=\"FilmCraft\"")
        {
            skipped.push(json!({"item": id.0, "path": side, "reason": "an XMP file written by another application exists"}));
            continue;
        }
        match s.services.write_file(&side, xmp_packet(it).as_bytes()) {
            Ok(()) => written.push(json!({"item": id.0, "path": side})),
            Err(e) => skipped.push(json!({"item": id.0, "path": side, "reason": e.to_string()})),
        }
    }
    if written.is_empty() && skipped.is_empty() {
        return Err(EngineError::Other("select a clip with a media file".into()));
    }
    Ok(json!({"written": written, "skipped": skipped}))
}

fn generate_waveform(s: &mut Session, p: &Value) -> Result<Value> {
    let items: Vec<u64> = targets(s, p).into_iter().filter_map(|i| media_of(&s.project, i).filter(|(_, m)| m.info.has_audio()).map(|(mid, _)| mid.0)).collect();
    if items.is_empty() {
        return Err(EngineError::Other("select a clip with audio".into()));
    }
    Ok(json!({"items": items}))
}

// ---------------------------------------------------------------------------------------------
// Automate to Sequence
// ---------------------------------------------------------------------------------------------

/// Clip ▸ Automate to Sequence…: place the selected Project panel items in the active sequence
/// at the playhead (sequentially, or at the sequence's unnumbered — unnamed — markers), by insert
/// or overwrite, on the source-patched tracks. Consecutive clips overlap by `overlapFrames`
/// (taken from the outgoing clip's tail) and get the default transitions over the overlap. One
/// undo step.
fn automate_to_sequence(s: &mut Session, p: &Value) -> Result<Value> {
    let cmd = "clip.automateToSequence";
    let ordering = str_p(p, "ordering").unwrap_or("sort");
    let placement = str_p(p, "placement").unwrap_or("sequentially");
    let method = str_p(p, "method").unwrap_or("insert");
    if !matches!(ordering, "sort" | "selection") || !matches!(placement, "sequentially" | "unnumberedMarkers") || !matches!(method, "insert" | "overwrite") {
        return Err(bad(cmd, "ordering: sort|selection, placement: sequentially|unnumberedMarkers, method: insert|overwrite"));
    }
    let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
    let rate = q.settings.frame_rate;
    let fd = rate.frame_duration();
    let mut items: Vec<ItemId> = match p.get("items").and_then(Value::as_array) {
        Some(a) => a.iter().filter_map(|v| v.as_u64().map(ItemId)).collect(),
        None => s.state.project_selection.clone(),
    };
    let seq_id = s.state.active_sequence.ok_or(EngineError::NoSequence)?;
    items.retain(|i| *i != seq_id && s.project.item(*i).is_some_and(|it| !matches!(it.kind, ItemKind::Graphic { .. })));
    if ordering == "sort" {
        let mut order = Vec::new();
        s.project.root.all_items(&mut order);
        items.sort_by_key(|i| order.iter().position(|x| x == i).unwrap_or(usize::MAX));
    }
    if items.is_empty() {
        return Err(EngineError::Other("select clips in the Project panel".into()));
    }
    let tg = s.targeting();
    let vdest = if bool_p(p, "ignoreVideo").unwrap_or(false) { None } else { tg.video_dest };
    let adest = if bool_p(p, "ignoreAudio").unwrap_or(false) { None } else { tg.audio_dest };
    if vdest.is_none() && adest.is_none() {
        return Err(EngineError::Other("no destination track (check source patching, or don't ignore both audio and video)".into()));
    }
    let overlap = if placement == "sequentially" { rate.tick_of(u64_p(p, "overlapFrames").unwrap_or(30) as i64) } else { Tick::ZERO };
    let still = u64_p(p, "stillFrames").map(|f| rate.tick_of(f as i64));
    // each item's range (In/Out marks; stills: `stillFrames` or the default still duration)
    let mut plan: Vec<(ItemId, TimeRange)> = Vec::new();
    for id in &items {
        let Some(mut r) = crate::clip_ops::item_range(&s.project, *id, s.prefs.timeline.still_duration(s.sequence_rate())) else { continue };
        let is_still = s.project.item(*id).and_then(|i| i.as_media()).is_some_and(|m| matches!(m.info.kind, filmcraft_media::MediaKind::Still));
        if is_still && let Some(d) = still {
            r = TimeRange::new(r.start, d);
        }
        // snap to whole sequence frames
        let d = rate.tick_of(rate.frame_at(r.duration + Tick(fd.0 / 2)).max(1));
        plan.push((*id, TimeRange::new(r.start, d)));
    }
    // destinations
    let starts: Vec<Tick> = if placement == "unnumberedMarkers" {
        let mut m: Vec<Tick> = q.markers.iter().filter(|m| m.name.is_empty() && m.kind == MarkerKind::Comment).map(|m| rate.snap(m.start)).collect();
        m.sort();
        if m.is_empty() {
            return Err(EngineError::Other("the sequence has no unnumbered markers".into()));
        }
        plan.truncate(m.len());
        m.truncate(plan.len());
        m
    } else {
        // the overlap can't exceed half of either clip
        let mut at = s.playhead();
        let mut v = Vec::new();
        for (k, (_, r)) in plan.iter().enumerate() {
            if k > 0 {
                let prev = plan[k - 1].1.duration;
                let ov = overlap.min(Tick(prev.0 / 2)).min(Tick(r.duration.0 / 2));
                at -= rate.snap(ov);
            }
            v.push(at);
            at += r.duration;
        }
        v
    };
    let n0 = s.history.undo.len();
    // insert: open room first (sync-locked / destination tracks ripple), then overwrite into it
    if method == "insert" && placement == "sequentially" {
        let first = starts[0];
        let end = starts.iter().zip(&plan).map(|(a, (_, r))| *a + r.duration).max().unwrap_or(first);
        let len = end - first;
        let dests: Vec<_> = [vdest, adest].into_iter().flatten().collect();
        s.edit_sequence("Automate to Sequence", |q, ctx, _| {
            let mut links = std::collections::HashMap::new();
            for t in q.video_tracks.iter_mut().chain(q.audio_tracks.iter_mut()) {
                if !t.locked && (t.sync_lock || dests.contains(&t.id)) {
                    filmcraft_edit::insert_track_gap(t, first, len, ctx, &mut links);
                }
            }
            Ok(())
        })?;
    }
    let mut placed = Vec::new();
    let mut result: Result<()> = Ok(());
    for (k, ((id, r), at)) in plan.iter().zip(&starts).enumerate() {
        let insert = method == "insert" && placement == "unnumberedMarkers";
        match crate::commands::place_item(s, *id, *r, *at, vdest, adest, insert, "Automate to Sequence", None) {
            Ok(ids) => placed.push((k, ids)),
            Err(e) => {
                result = Err(e);
                break;
            }
        }
    }
    if let Err(e) = result {
        // undo everything this command did
        while s.history.undo.len() > n0 {
            s.undo();
        }
        s.history.redo.clear();
        return Err(e);
    }
    // default transitions over the overlaps (and at the cuts when there is no overlap)
    let vt = bool_p(p, "videoTransition").unwrap_or(true);
    let at = bool_p(p, "audioTransition").unwrap_or(true);
    let mut transitions = 0;
    if placement == "sequentially" && (vt || at) {
        let q = s.active_sequence().ok_or(EngineError::NoSequence)?;
        let mut edges = Vec::new();
        for (_, ids) in placed.iter().skip(1) {
            for c in ids {
                let Some((tid, _)) = q.find_item(*c) else { continue };
                let kind = q.track(tid).map(|t| t.kind);
                if (kind == Some(TrackKind::Video) && vt) || (kind == Some(TrackKind::Audio) && at) {
                    edges.push((*c, kind == Some(TrackKind::Video)));
                }
            }
        }
        let frames = if overlap > Tick::ZERO { rate.frame_at(overlap) } else { s.project.settings.default_transition_duration_frames };
        for (c, video) in edges {
            let id = if video { "sequence.applyVideoTransition" } else { "sequence.applyAudioTransition" };
            if s.execute(id, json!({"clip": c.0, "edge": "in", "frames": frames})).is_ok() {
                transitions += 1;
            }
        }
    }
    crate::clip_ops::collapse_history(s, n0, "Automate to Sequence");
    let clips: Vec<u64> = placed.iter().flat_map(|(_, ids)| ids.iter().map(|c| c.0)).collect();
    s.state.selection = clips.iter().map(|c| filmcraft_project::ClipId(*c)).collect();
    Ok(json!({"placed": placed.len(), "clips": clips, "transitions": transitions, "starts": starts.iter().map(|t| t.0).collect::<Vec<_>>()}))
}

// ---------------------------------------------------------------------------------------------
// System Compatibility Report
// ---------------------------------------------------------------------------------------------

/// What this build supports: OS, CPU, decoders / containers and export formats. The frontend adds
/// the GPU adapter.
pub fn system_report() -> Value {
    let cpus = std::thread::available_parallelism().map(|n| n.get()).unwrap_or(1);
    let formats: Vec<Value> =
        filmcraft_export::Format::ALL.iter().map(|f| json!({"format": f.label(), "available": filmcraft_export::available(*f)})).collect();
    json!({
        "app": format!("FilmCraft {}", env!("CARGO_PKG_VERSION")),
        "os": std::env::consts::OS,
        "family": std::env::consts::FAMILY,
        "arch": std::env::consts::ARCH,
        "cpuThreads": cpus,
        "decoders": ["H.264 (AVC)", "H.265 (HEVC) Main / Main 10", "VP9", "AV1 Main", "Apple ProRes", "Avid DNxHD / DNxHR", "Motion JPEG", "AAC-LC", "Opus", "PCM"],
        "containers": ["MP4 / MOV (ISO BMFF)", "Matroska / WebM", "WAV", "PNG / JPEG stills"],
        "exportFormats": formats,
        "speechToText": crate::transcript::speech_available(),
        "checks": [
            {"name": "Operating system", "ok": true, "detail": format!("{} ({})", std::env::consts::OS, std::env::consts::ARCH)},
            {"name": "CPU", "ok": cpus >= 4, "detail": format!("{cpus} hardware threads{}", if cpus < 4 { " — 4 or more recommended for 1080p playback" } else { "" })},
        ],
    })
}

#[cfg(test)]
#[path = "project_tools_tests.rs"]
mod tests;

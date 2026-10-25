//! Menu long tail (M3.11): the dialogs of Find, Create Search Bin, Project Settings (General /
//! Scratch Disks / Ingest Settings), Get Media File Properties, Edit Offline, Source Settings,
//! Update Metadata, Automate to Sequence, Scene Edit Detection, Normalize Mix Track, Simplify
//! Sequence, Transcribe Sequence, Save as Template, Add Flash Cue Marker and the System
//! Compatibility Report; plus the frontend side of Edit Original / Reveal Log Files (the OS
//! opener), Reveal in Finder, Generate Audio Waveform (the timeline's peak cache), Import from
//! Media Browser (the Media Browser selection) and View ▸ Dynamic Audio Waveforms.
//!
//! The open dialog lives in `UiState::extras.dialog` (serde, like `clip_dialogs`): the engine
//! command it runs and the parameters being edited, so agents open it from the menu, change
//! `params` with `ui.set {"menuDialog": {...}}` and click OK. Menu items invoked without params
//! open the dialog; with params the engine command runs directly.
//!
//! Automation ids (`<p>` = the dialog prefix, `<key>` = a parameter): `<p>.ok`, `<p>.cancel` and
//! `<p>.<key>` for each control; radio buttons `<p>.<key>.<value>`; Find rows
//! `find.row.<n>.column|operator|text`; Project Settings tabs `projectSettings.tab.<general|scratchDisks|ingest>`,
//! scratch rows `projectSettings.scratch.<key>` / `.browse` / `.same`.

use crate::widgets::CenteredDialog as _;
use egui::{Align2, RichText};
use filmcraft_project::{FindOp, find::COLUMNS};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::native_dialogs::FileDialog;
use crate::state::ClipDialogDraft;

/// Frontend state of the M3.11 menu items (`UiState::extras`).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Extras {
    /// The open dialog (None = closed).
    pub dialog: Option<ClipDialogDraft>,
    /// View ▸ Dynamic Audio Waveforms: logarithmic (dB) waveform display; off = linear amplitude.
    pub dynamic_waveforms: bool,
    /// Paths handed to the operating system's opener (Edit Original, Reveal Log Files), newest last.
    pub opened: Vec<String>,
    /// Clip ▸ Source Settings… live preview: the last settings sent to the clip while the dialog is
    /// open (None = none yet). See [`source_preview`].
    pub source_preview: Option<SourcePreview>,
}

impl Default for Extras {
    fn default() -> Self {
        Extras { dialog: None, dynamic_waveforms: true, opened: Vec::new(), source_preview: None }
    }
}

/// The live preview of the open Source Settings dialog: what was last sent to `clip.sourceSettings
/// {preview: true}` (applied, or refused with the error the dialog shows), for which clips, at
/// which project revision and when (egui time, seconds), so the next change waits for the
/// Program monitor to show that revision before it is sent (at most one apply outstanding).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct SourcePreview {
    pub sent: Value,
    pub items: Value,
    pub revision: u64,
    pub sent_at: f64,
}

/// Longest a live preview waits for the monitors before the next change is sent anyway (a frame
/// that never comes: no sequence open, the clip not on the Program monitor).
const PREVIEW_WAIT_MAX: f64 = 1.0;

/// The Camera Raw panel's values as `clip.sourceSettings` / `file.projectSettings.cameraRaw`
/// take them: every control but the fixed ones (colour science, HDRx blend), with "as recorded"
/// as `null`.
fn panel_params(settings: &Value, p: &Value) -> Value {
    let mut q = json!({});
    for st in settings.as_array().into_iter().flatten() {
        let Some(id) = st["id"].as_str() else { continue };
        if st["kind"] == "fixed" {
            continue;
        }
        q[id] = match &p[id] {
            Value::String(v) if v == "asRecorded" => Value::Null,
            v => v.clone(),
        };
    }
    q
}

/// The parameters of the Source Settings dialog, as `clip.sourceSettings` takes them. Decode
/// Using goes only when it changes. A clip that decodes with the project's settings sends no
/// development settings of its own, unless one was changed (over the control channel: the panel
/// disables them), which gives the clip settings of its own, as the command does.
fn source_settings_params(d: &ClipDialogDraft) -> Value {
    let mut q = panel_params(&d.info["settings"], &d.params);
    let mode = d.info["develop"]["decodeUsing"].as_str().unwrap_or("clip");
    let wanted = d.params["decodeUsing"].as_str().unwrap_or(mode);
    let Some(m) = q.as_object_mut() else { return q };
    m.remove("decodeUsing");
    if wanted != mode {
        m.insert("decodeUsing".into(), json!(wanted));
        if wanted == "project" {
            m.retain(|k, _| k == "decodeUsing" || k == "quality");
        }
    } else if mode == "project" {
        // only what differs from the project's settings the panel shows
        let shown = panel_params(&d.info["settings"], &setting_values(&d.info["settings"]));
        m.retain(|k, v| k == "quality" || shown.get(k) != Some(v));
    }
    q
}

/// The value of every control the command reported, as the dialog's parameters.
fn setting_values(settings: &Value) -> Value {
    let mut p = json!({});
    for st in settings.as_array().into_iter().flatten() {
        if let Some(id) = st["id"].as_str() {
            p[id] = st["value"].clone();
        }
    }
    p
}

/// The Camera Raw panel's values of develop `d` (the project's panel: no Decode Using).
fn develop_values(d: &filmcraft_media::RawDevelop) -> Value {
    json!({
        "quality": d.quality().id(),
        "colorScience": "ipp2",
        "colorSpace": d.color_space.id(),
        "gamma": d.gamma.id(),
        "toneMap": d.tone_map.id(),
        "rolloff": d.rolloff.id(),
        "hdrPeakNits": d.hdr_peak_nits,
        "chromaNoiseReduction": d.chroma_noise_reduction,
        "flashingPixel": d.flashing_pixel.map_or("asRecorded", |f| f.id()),
        "metadataCurves": d.metadata_curves,
        "creativeLut": d.creative_lut,
        "cdl": d.cdl,
        "iso": d.iso,
        "kelvin": d.kelvin,
        "tint": d.tint,
        "exposure": d.exposure,
    })
}

/// Take a `clip.sourceSettings` reply into the open dialog: the controls (values, what is
/// enabled), the clip's mode and what it records, so the panel shows what the clip now develops
/// with (Decode Using switched, a colour space that brought its gamma along, Use Settings).
fn take_reply(d: &mut ClipDialogDraft, r: &Value) {
    for k in ["settings", "develop", "own", "asShot", "clip", "rmd", "rmdNote", "message"] {
        if let Some(v) = r.get(k) {
            d.info[k] = v.clone();
        }
    }
    d.params = setting_values(&d.info["settings"]);
}

/// Keep the clip in step with the Source Settings dialog: a changed control is
/// applied through `clip.sourceSettings {preview: true}` so the monitors develop the current
/// frame with it before OK. Every preview of one dialog is one undo step (the command merges
/// them); OK commits it ([`run`]) and Cancel takes it back ([`revert_source_preview`]). While a
/// control is dragged, at most one apply is outstanding: the next waits until the Program monitor
/// shows the revision of the last one (or [`PREVIEW_WAIT_MAX`] passes), and a drag that ends
/// meanwhile still sends its final value. A refused value (hostile, over the control channel)
/// shows its error and is not sent again until it changes.
fn source_preview(app: &mut FilmcraftApp, ctx: &egui::Context, d: &mut ClipDialogDraft) {
    let wanted = source_settings_params(d);
    if d.info["opened"].is_null() {
        // a dialog opened without them (restored state): what it shows now counts as its opening values
        d.info["opened"] = wanted;
        return;
    }
    let last = app.ui.extras.source_preview.as_ref().map(|s| s.sent.clone()).unwrap_or_else(|| d.info["opened"].clone());
    if wanted == last {
        return;
    }
    let now = ctx.input(|i| i.time);
    if let Some(sp) = &app.ui.extras.source_preview {
        let dragging = ctx.input(|i| i.pointer.any_down());
        let shown = app.program_picture().map(|k| k.revision);
        let behind = shown.is_some_and(|r| r < sp.revision);
        if dragging && behind && now - sp.sent_at < PREVIEW_WAIT_MAX {
            // the last apply is still being developed: send this one when its frame is on screen
            ctx.request_repaint_after(std::time::Duration::from_millis(16));
            return;
        }
    }
    let items = d.info["items"].clone();
    let mut q = wanted.clone();
    q["items"] = items.clone();
    q["preview"] = json!(true);
    let sent = match app.session.execute("clip.sourceSettings", q) {
        Ok(r) => {
            d.error.clear();
            take_reply(d, &r);
            source_settings_params(d)
        }
        Err(e) => {
            d.error = e.to_string();
            wanted
        }
    };
    app.ui.extras.source_preview = Some(SourcePreview { sent, items, revision: app.session.revision, sent_at: now });
}

/// The Camera Raw panel's buttons (Update RMD, Use Changes, Use Settings): the dialog's settings
/// with the action, applied as part of the dialog's live preview (one undo step; Cancel takes it
/// back, except the RMD file, which is written at once).
fn camera_raw_action(app: &mut FilmcraftApp, d: &mut ClipDialogDraft, flag: &str, now: f64) {
    let items = d.info["items"].clone();
    let mut q = source_settings_params(d);
    q["items"] = items.clone();
    q["preview"] = json!(true);
    q[flag] = json!(true);
    match app.session.execute("clip.sourceSettings", q) {
        Ok(r) => {
            d.error.clear();
            take_reply(d, &r);
            if flag == "updateRmd" {
                let paths: Vec<&str> = r["rmdWritten"].as_array().into_iter().flatten().filter_map(|w| w["path"].as_str()).collect();
                app.ui.status = tlf!("RMD written: {paths}", paths = paths.join(", "));
            }
            let sent = source_settings_params(d);
            app.ui.extras.source_preview = Some(SourcePreview { sent, items, revision: app.session.revision, sent_at: now });
        }
        Err(e) => d.error = e.to_string(),
    }
}

/// Take back the live preview of a Source Settings dialog that closes without OK (Cancel, Escape,
/// or the dialog replaced): the clip returns to the values the dialog opened with, and the
/// history is as before it opened.
fn revert_source_preview(app: &mut FilmcraftApp) {
    let Some(sp) = app.ui.extras.source_preview.take() else { return };
    if let Err(e) = app.session.execute("clip.sourceSettings", json!({"items": sp.items, "revert": true})) {
        log::warn!("source settings preview not reverted: {e}");
    }
}

type Elems = Vec<(String, egui::Rect, String)>;

fn push(elems: &mut Elems, id: impl Into<String>, r: &egui::Response, label: impl Into<String>) {
    let label = label.into();
    crate::widgets::accessible_name(r.clone(), label.clone());
    elems.push((id.into(), r.rect, label));
}

/// Dialog title, automation prefix and OK button text of a command's dialog.
fn meta(command: &str) -> Option<(&'static str, &'static str, &'static str)> {
    Some(match command {
        "edit.find" => (tl!("Find"), "find", tl!("Find")),
        "file.newSearchBin" => (tl!("Create Search Bin"), "searchBin", tl!("OK")),
        "project.settings" => (tl!("Project Settings"), "projectSettings", tl!("OK")),
        "file.mediaProperties" => (tl!("Properties"), "properties", tl!("Close")),
        "clip.editOffline" => (tl!("Edit Offline File"), "editOffline", tl!("OK")),
        "clip.sourceSettings" => (tl!("Source Settings"), "sourceSettings", tl!("OK")),
        "clip.updateMetadata" => (tl!("Update Metadata"), "updateMetadata", tl!("OK")),
        "clip.automateToSequence" => (tl!("Automate To Sequence"), "automate", tl!("OK")),
        "clip.sceneEditDetection" => (tl!("Scene Edit Detection"), "sceneDetect", tl!("Analyze")),
        "sequence.normalizeMixTrack" => (tl!("Normalize Mix Track"), "normalizeMix", tl!("OK")),
        "sequence.simplify" => (tl!("Simplify Sequence"), "simplify", tl!("Simplify")),
        "sequence.transcribe" => (tl!("Transcribe Sequence"), "transcribe", tl!("Transcribe")),
        "file.saveAsTemplate" => (tl!("Save as Template"), "saveTemplate", tl!("Save")),
        "markers.addFlashCue" => (tl!("Flash Cue Marker"), "flashCue", tl!("OK")),
        "help.systemCompatibilityReport" => (tl!("System Compatibility Report"), "systemReport", tl!("Close")),
        "file.newColorMatte" => (tl!("New Color Matte"), "colorMatte", tl!("OK")),
        "project.matteColor" => (tl!("Color Matte Color"), "matteColor", tl!("OK")),
        _ => return None,
    })
}

/// Commands whose menu item opens a dialog (the Project Settings items share one).
fn dialog_command(id: &str) -> Option<&'static str> {
    Some(match id {
        "file.projectSettings.general" | "file.projectSettings.scratchDisks" | "project.ingestSettings" | "file.projectSettings.cameraRaw" => {
            "project.settings"
        }
        "edit.find" => "edit.find",
        "file.newSearchBin" => "file.newSearchBin",
        "file.mediaProperties" => "file.mediaProperties",
        "clip.editOffline" => "clip.editOffline",
        "clip.sourceSettings" => "clip.sourceSettings",
        "clip.updateMetadata" => "clip.updateMetadata",
        "clip.automateToSequence" => "clip.automateToSequence",
        "clip.sceneEditDetection" => "clip.sceneEditDetection",
        "sequence.normalizeMixTrack" => "sequence.normalizeMixTrack",
        "sequence.simplify" => "sequence.simplify",
        "sequence.transcribe" => "sequence.transcribe",
        "file.saveAsTemplate" => "file.saveAsTemplate",
        "markers.addFlashCue" => "markers.addFlashCue",
        "file.newColorMatte" => "file.newColorMatte",
        "project.matteColor" => "project.matteColor",
        _ => return None,
    })
}

fn enabled(app: &FilmcraftApp, id: &str) -> Result<(), String> {
    filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session))
}

/// Checkmark state of this module's toggles in the menus.
pub fn checked(app: &FilmcraftApp, id: &str) -> Option<bool> {
    (id == "view.dynamicAudioWaveforms").then_some(app.ui.extras.dynamic_waveforms)
}

/// Project Settings ▸ General ▸ Renderer is "Software Only": the Program monitor composites on
/// the CPU.
pub fn software_renderer(app: &FilmcraftApp) -> bool {
    app.session.project.settings.renderer == filmcraft_engine::project_tools::RENDERER_SOFTWARE
}

/// Hand a path to the operating system (open it in its default application, or reveal it in the
/// file manager). The host hook does it on the desktop; otherwise the path is opened as a
/// `file://` URL.
pub fn open_path(app: &mut FilmcraftApp, ctx: &egui::Context, path: &str, reveal: bool) -> Result<(), String> {
    app.ui.extras.opened.push(path.to_string());
    if app.ui.extras.opened.len() > 50 {
        app.ui.extras.opened.remove(0);
    }
    match app.hooks.open_path.as_mut() {
        Some(f) => f(path, reveal),
        None => {
            let p = path.replace('\\', "/");
            ctx.open_url(egui::OpenUrl::new_tab(format!("file://{}{p}", if p.starts_with('/') { "" } else { "/" })));
            Ok(())
        }
    }
}

/// Where Reveal Log Files points: `<data dir>/Logs` (the temp folder without a data directory).
pub fn logs_dir(app: &FilmcraftApp) -> std::path::PathBuf {
    app.session.prefs_path.as_ref().and_then(|p| p.parent()).map(|d| d.join("Logs")).unwrap_or_else(|| filmcraft_engine::temp_dir().join("FilmCraft Logs"))
}

/// Write this session's log (system report and the command journal) into [`logs_dir`].
fn write_session_log(app: &FilmcraftApp) -> Result<std::path::PathBuf, String> {
    let dir = logs_dir(app);
    std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let path = dir.join(format!("session-{}.log", std::process::id()));
    let mut text = format!("{}\n", system_report(app));
    for (id, p) in app.session.journal.iter().rev().take(2000).collect::<Vec<_>>().into_iter().rev() {
        text.push_str(&format!("{}\n", json!({"command": id, "params": p})));
    }
    std::fs::write(&path, text).map_err(|e| format!("{}: {e}", path.display()))?;
    Ok(path)
}

/// The engine's report plus the GPU adapter (wgpu) when the GPU compositor is on.
pub fn system_report(app: &FilmcraftApp) -> Value {
    let mut r = filmcraft_engine::project_tools::system_report();
    let gpu = app.gpu.as_ref().map(|g| {
        let i = g.render_state.adapter.get_info();
        json!({"name": i.name, "backend": format!("{:?}", i.backend), "type": format!("{:?}", i.device_type), "driver": i.driver, "driverInfo": i.driver_info})
    });
    let ok = gpu.is_some();
    r["gpu"] = gpu.unwrap_or(Value::Null);
    let detail =
        if ok { r["gpu"]["name"].as_str().unwrap_or("wgpu adapter").to_string() } else { tl!("no GPU adapter: the CPU compositor is used").to_string() };
    if let Some(c) = r["checks"].as_array_mut() {
        c.push(json!({"name": "GPU acceleration", "ok": ok, "detail": detail}));
    }
    r
}

/// Menu / shortcut entry points. Returns None for ids this module doesn't handle.
pub fn route(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let empty = params.as_object().is_none_or(|m| m.is_empty());
    // UI commands
    match id {
        "view.dynamicAudioWaveforms" => {
            app.ui.extras.dynamic_waveforms = params.get("on").and_then(Value::as_bool).unwrap_or(!app.ui.extras.dynamic_waveforms);
            return Some(Ok(json!({"dynamicAudioWaveforms": app.ui.extras.dynamic_waveforms})));
        }
        "help.revealLogFiles" => {
            let r = write_session_log(app).and_then(|p| {
                let s = p.to_string_lossy().to_string();
                open_path(app, ctx, &s, true).map(|_| json!({"path": s}))
            });
            return Some(r);
        }
        "help.systemCompatibilityReport" => {
            let report = system_report(app);
            app.ui.extras.dialog = Some(ClipDialogDraft { command: id.into(), params: json!({}), info: report.clone(), error: String::new() });
            return Some(Ok(json!({"dialog": "systemReport", "report": report})));
        }
        "edit.editOriginal" | "file.reveal" => {
            let reveal = id == "file.reveal";
            let r = app.session.execute(id, params.clone()).map_err(|e| e.to_string());
            return Some(r.and_then(|v| {
                for p in v["paths"].as_array().cloned().unwrap_or_default() {
                    open_path(app, ctx, p.as_str().unwrap_or_default(), reveal)?;
                }
                Ok(v)
            }));
        }
        "clip.generateAudioWaveform" => {
            let r = app.session.execute(id, params.clone()).map_err(|e| e.to_string());
            if let Ok(v) = &r {
                let mut peaks = app.tl.peaks.lock().unwrap_or_else(|e| e.into_inner());
                for i in v["items"].as_array().cloned().unwrap_or_default() {
                    let item = filmcraft_engine::project::ItemId(i.as_u64().unwrap_or_default());
                    peaks.retain(|(it, _), _| *it != item);
                }
            }
            return Some(r);
        }
        "file.exportSelectionProject" | "file.exportAle" if params.get("path").is_none() => {
            if let Err(e) = enabled(app, id) {
                return Some(Err(e));
            }
            let (filter, ext, name) = if id == "file.exportAle" {
                (tl!("Avid Log Exchange"), "ale", format!("{}.ale", app.session.project.name))
            } else {
                (tl!("FilmCraft Project"), "fcproj", tlf!("{name} Selection.fcproj", name = app.session.project.name))
            };
            let mut p = params.clone();
            if !p.is_object() {
                p = json!({});
            }
            let id = id.to_string();
            return Some(app.pick(FileDialog::save_as(filter, &[ext], &name), move |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                p["path"] = json!(path);
                app.session.execute(&id, p).map_err(|e| e.to_string())
            }));
        }
        "file.mediaPropertiesFile" if params.get("path").is_none() => {
            let exts: Vec<&str> =
                filmcraft_media::VIDEO_EXTENSIONS.iter().chain(filmcraft_media::AUDIO_EXTENSIONS).chain(filmcraft_media::STILL_EXTENSIONS).copied().collect();
            let id = id.to_string();
            return Some(app.pick(FileDialog::open_file(tl!("Media"), &exts), move |app, paths| {
                let Some(path) = paths.into_iter().next() else { return Ok(Value::Null) };
                let r = app.session.execute(&id, json!({"path": path})).map_err(|e| e.to_string());
                if let Ok(v) = &r {
                    app.ui.extras.dialog =
                        Some(ClipDialogDraft { command: "file.mediaProperties".into(), params: json!({}), info: json!([v]), error: String::new() });
                }
                r
            }));
        }
        _ => {}
    }
    if !empty {
        return None;
    }
    let cmd = dialog_command(id)?;
    if let Err(e) = enabled(app, id) {
        app.ui.status = e.clone();
        return Some(Err(e));
    }
    let (params, info) = match defaults(app, cmd, id) {
        Ok(x) => x,
        Err(e) => return Some(Err(e)),
    };
    app.ui.extras.dialog = Some(ClipDialogDraft { command: cmd.into(), params, info, error: String::new() });
    Some(Ok(json!({"dialog": meta(cmd).map(|m| m.1)})))
}

/// Initial parameters (and display info) of a dialog.
fn defaults(app: &mut FilmcraftApp, cmd: &str, id: &str) -> Result<(Value, Value), String> {
    let s = &mut app.session;
    Ok(match cmd {
        "edit.find" => {
            let timeline = app.ui.focused == PanelKind::Timeline && s.active_sequence().is_some();
            let last = s.state.find.clone();
            let rows = match &last {
                Some(f) if f.scope == "project" => {
                    f.query.rows.iter().map(|r| json!({"column": r.column, "operator": r.op.name(), "text": r.text})).collect::<Vec<_>>()
                }
                _ => vec![json!({"column": "Name", "operator": "contains", "text": ""})],
            };
            let mut rows = rows;
            while rows.len() < 2 {
                rows.push(json!({"column": "Name", "operator": "contains", "text": ""}));
            }
            (json!({"scope": if timeline { "timeline" } else { "project" }, "rows": rows, "matchAll": true, "caseSensitive": false, "in": "all"}), json!({}))
        }
        "file.newSearchBin" => {
            let text = app.ui.project_search.clone();
            (json!({"name": "", "column": "All", "operator": "contains", "text": text}), Value::Null)
        }
        "project.settings" => {
            let tab = match id {
                "file.projectSettings.scratchDisks" => "scratchDisks",
                "project.ingestSettings" => "ingest",
                "file.projectSettings.cameraRaw" => "cameraRaw",
                _ => "general",
            };
            let st = s.project.settings.clone();
            let general = s.execute("file.projectSettings.general", json!({})).map_err(|e| e.to_string())?;
            let scratch = filmcraft_engine::project_tools::scratch_paths(s);
            let camera_raw = s.execute("file.projectSettings.cameraRaw", json!({})).map_err(|e| e.to_string())?;
            let camera_values = setting_values(&camera_raw["settings"]);
            let ingest = serde_json::to_value(&st.ingest).unwrap_or_default();
            let sc = &st.scratch;
            (
                json!({
                    "tab": tab,
                    "renderer": general["renderer"], "videoDisplay": general["videoDisplay"], "audioDisplay": general["audioDisplay"],
                    "captureFormat": general["captureFormat"], "titleSafe": general["titleSafe"], "actionSafe": general["actionSafe"],
                    "captured": sc.captured, "videoPreviews": sc.video_previews, "audioPreviews": sc.audio_previews, "autoSave": sc.auto_save,
                    "ingest": ingest.clone(),
                    "cameraRaw": camera_values.clone(),
                }),
                json!({"scratch": scratch, "ingest": ingest, "project": s.project.name, "cameraRaw": camera_raw, "cameraRawOpened": camera_values}),
            )
        }
        "file.mediaProperties" => {
            let v = s.execute("file.mediaProperties", json!({})).map_err(|e| e.to_string())?;
            (json!({}), v)
        }
        "clip.editOffline" => {
            let item = s.state.project_selection.first().copied().and_then(|i| s.project.item(i)).cloned();
            let mut p = json!({});
            if let Some(it) = item {
                p["item"] = json!(it.id.0);
                p["mediaName"] = json!(it.name);
                for (k, key) in filmcraft_engine::project_tools::OFFLINE_FIELDS {
                    p[k] = json!(it.metadata.get(key).cloned().unwrap_or_default());
                }
            }
            (p, Value::Null)
        }
        "clip.sourceSettings" => {
            // the controls and their values come from the command; the dialog edits a copy
            let info = s.execute("clip.sourceSettings", json!({})).map_err(|e| e.to_string())?;
            let mut draft = ClipDialogDraft { command: cmd.into(), params: setting_values(&info["settings"]), info, error: String::new() };
            // the values the dialog opened with: the live preview applies every change from them
            draft.info["opened"] = source_settings_params(&draft);
            (draft.params, draft.info)
        }
        "clip.updateMetadata" => {
            let n = s.state.project_selection.len().max(s.state.selection.len());
            (json!({}), json!({"count": n}))
        }
        "clip.automateToSequence" => (
            json!({"ordering": "sort", "placement": "sequentially", "method": "insert", "overlapFrames": 30, "stillFrames": 150, "videoTransition": true, "audioTransition": true, "ignoreAudio": false, "ignoreVideo": false}),
            json!({"sequence": s.active_sequence().map(|_| s.state.active_sequence.and_then(|q| s.project.item(q)).map(|i| i.name.clone()).unwrap_or_default())}),
        ),
        "clip.sceneEditDetection" => (json!({"applyCuts": true, "createSubclips": false, "generateMarkers": false, "sensitivity": 50.0}), Value::Null),
        "sequence.normalizeMixTrack" => (json!({"db": 0.0}), Value::Null),
        "sequence.simplify" => {
            let name = s.state.active_sequence.and_then(|q| s.project.item(q)).map(|i| tlf!("{name} (Simplified)", name = i.name)).unwrap_or_default();
            (
                json!({"name": name, "removeDisabled": true, "removeEmptyTracks": true, "closeGaps": false, "moveClipsDown": false, "removeVideoEffects": false, "removeAudioEffects": false, "removeText": false, "keep": "both"}),
                Value::Null,
            )
        }
        "sequence.transcribe" => {
            let tracks: Vec<String> = s.active_sequence().map(|q| (1..=q.audio_tracks.len()).map(|i| format!("A{i}")).collect()).unwrap_or_default();
            (json!({"language": "auto", "track": "mix", "diarize": true}), json!({"tracks": tracks}))
        }
        "file.saveAsTemplate" => (json!({"name": s.project.name}), Value::Null),
        "markers.addFlashCue" => (json!({"name": "", "comment": ""}), Value::Null),
        "file.newColorMatte" => {
            let st = s.active_sequence().map(|q| q.settings.clone()).unwrap_or_default();
            (json!({"color": "#000000", "name": "Color Matte", "width": st.width, "height": st.height, "seconds": 5.0}), Value::Null)
        }
        "project.matteColor" => {
            let matte = match s.state.project_selection.as_slice() {
                [id] => s.project.item(*id).and_then(|it| match &it.kind {
                    filmcraft_engine::project::ItemKind::Media(m) => match &m.media {
                        filmcraft_engine::project::MediaRef::Generator(filmcraft_media::Generator::ColorMatte { color }) => Some((it.id, *color)),
                        _ => None,
                    },
                    _ => None,
                }),
                _ => None,
            };
            let (id, color) = matte.ok_or("select one Color Matte")?;
            (json!({"item": id.0, "color": filmcraft_color::to_hex(color)}), Value::Null)
        }
        _ => (json!({}), Value::Null),
    })
}

fn check(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, key: &str, label: &str) {
    let mut v = p.get(key).and_then(Value::as_bool).unwrap_or(false);
    let r = ui.checkbox(&mut v, label);
    push(elems, format!("{pre}.{key}"), &r, label);
    if r.changed() {
        p[key] = json!(v);
    }
}

fn text(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, key: &str, label: &str, width: f32) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut v = p.get(key).and_then(Value::as_str).unwrap_or_default().to_string();
        let r = ui.add(egui::TextEdit::singleline(&mut v).desired_width(width));
        push(elems, format!("{pre}.{key}"), &r, label);
        if r.changed() {
            p[key] = json!(v);
        }
    });
}

fn number(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, key: &str, label: &str, range: std::ops::RangeInclusive<f64>, suffix: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        let mut v = p.get(key).and_then(Value::as_f64).unwrap_or(0.0);
        let r = ui.add(crate::widgets::number_drag(&mut v).range(range).suffix(suffix));
        push(elems, format!("{pre}.{key}"), &r, label);
        if r.changed() {
            p[key] = json!(v);
        }
    });
}

fn radios(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, key: &str, label: &str, options: &[(&str, &str)]) {
    ui.horizontal_wrapped(|ui| {
        ui.label(label);
        for (k, l) in options {
            let r = ui.radio(p[key].as_str() == Some(*k), *l);
            push(elems, format!("{pre}.{key}.{k}"), &r, *l);
            if r.clicked() {
                p[key] = json!(k);
            }
        }
    });
}

fn combo(ui: &mut egui::Ui, elems: &mut Elems, id: &str, label: &str, value: &mut Value, options: &[(String, String)]) {
    ui.horizontal(|ui| {
        if !label.is_empty() {
            ui.label(label);
        }
        let cur = value.as_str().unwrap_or_default().to_string();
        let shown = options.iter().find(|o| o.0 == cur).map(|o| o.1.clone()).unwrap_or(cur.clone());
        let r = egui::ComboBox::from_id_salt(id).selected_text(crate::i18n::t(&shown)).show_ui(ui, |ui| {
            for (k, l) in options {
                if ui.selectable_label(cur == *k, crate::i18n::t(l)).clicked() {
                    *value = json!(k);
                }
            }
        });
        push(elems, id, &r.response, label);
    });
}

/// A color swatch that opens the color picker; the parameter is `#rrggbb`.
fn color(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, key: &str, label: &str) {
    ui.horizontal(|ui| {
        ui.label(label);
        let c = p.get(key).and_then(Value::as_str).and_then(filmcraft_color::parse_hex).unwrap_or([0.0, 0.0, 0.0, 1.0]);
        let mut rgb = [c[0], c[1], c[2]].map(|v| (v.clamp(0.0, 1.0) * 255.0).round() as u8);
        let r = egui::color_picker::color_edit_button_srgb(ui, &mut rgb);
        push(elems, format!("{pre}.{key}"), &r, label);
        if r.changed() {
            p[key] = json!(format!("#{:02x}{:02x}{:02x}", rgb[0], rgb[1], rgb[2]));
        }
        ui.label(p.get(key).and_then(Value::as_str).unwrap_or_default());
    });
}

/// The value of one setting with a recorded value (`<pre>.<id>`): ISO as a list of the decoder's
/// values, the others as numbers within the decoder's range, showing the clip's recorded value
/// until set, with an As Shot button (`<pre>.<id>.asShot`) that unsets it. The parameter is
/// `null` for as shot, else the number.
fn raw_value(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, st: &Value) {
    let id = st["id"].as_str().unwrap_or_default();
    let label = crate::i18n::t(st["label"].as_str().unwrap_or(id)).to_string();
    let as_shot = st["asShot"].as_f64().filter(|v| v.is_finite());
    let fallback = as_shot.or_else(|| st["default"].as_f64()).unwrap_or(0.0);
    let set = !p[id].is_null();
    ui.horizontal(|ui| {
        if st["kind"] == "iso" {
            let as_shot_label = match as_shot {
                Some(v) => tlf!("As Shot (ISO {v})", v = v.round().max(0.0) as u64),
                None => tl!("As Shot").to_string(),
            };
            let mut options: Vec<(String, String)> = vec![("asShot".into(), as_shot_label)];
            options.extend(st["options"].as_array().into_iter().flatten().filter_map(Value::as_u64).map(|v| (v.to_string(), v.to_string())));
            let mut choice = json!(p[id].as_u64().map_or_else(|| "asShot".to_string(), |v| v.to_string()));
            combo(ui, elems, &format!("{pre}.{id}"), "", &mut choice, &options);
            p[id] = match choice.as_str().and_then(|c| c.parse::<u64>().ok()) {
                Some(v) => json!(v),
                None => Value::Null,
            };
        } else {
            let mut v = p[id].as_f64().unwrap_or(fallback);
            let (min, max) = (st["min"].as_f64().unwrap_or(f64::MIN), st["max"].as_f64().unwrap_or(f64::MAX));
            let decimals = st["decimals"].as_u64().unwrap_or(0).min(6) as usize;
            let step = st["step"].as_f64().filter(|s| *s > 0.0).unwrap_or(1.0);
            let suffix = st["suffix"].as_str().unwrap_or_default();
            let mut drag = crate::widgets::number_drag(&mut v).speed(step).fixed_decimals(decimals).suffix(suffix);
            if min <= max {
                drag = drag.range(min..=max);
            }
            let r = ui.add(drag);
            push(elems, format!("{pre}.{id}"), &r, label.as_str());
            if r.changed() {
                // whole-number settings (Kelvin, the HDR peak) travel as integers
                p[id] = if decimals == 0 { json!(v.round().max(0.0) as u64) } else { json!(v) };
            }
            if !set {
                let what = if id == "hdrPeakNits" {
                    tl!("as recorded")
                } else if as_shot.is_some() {
                    tl!("as shot")
                } else {
                    tl!("as shot (decoder not open)")
                };
                ui.label(RichText::new(what).weak());
            }
        }
        if set {
            let r = ui.small_button(tl!("As Shot"));
            push(elems, format!("{pre}.{id}.asShot"), &r, "As Shot");
            if r.clicked() {
                p[id] = Value::Null;
            }
        }
    });
}

/// The Camera Raw panel (Clip ▸ Source Settings…, and Project Settings ▸ Camera RAW with
/// `clip` false): the controls the command reports, in its three columns as a RED Camera Raw
/// panel lays them out (decode and colour pipeline; ISO and the output transform; white balance
/// and exposure), each `<pre>.<id>`. What a control does not apply to is greyed out with the
/// reason on hover: every development control while the clip decodes with the project's settings,
/// the tone map and roll-off without a display gamma, the HDR peak without ST 2084, the curves,
/// LUT and CDL with linear light, the LUT of a clip that has none, colour science and the HDRx
/// blend always (with the reason under them). A colour space or gamma chosen that does not go
/// with the other one brings a fitting other one along, as the command does.
fn camera_raw_panel(ui: &mut egui::Ui, elems: &mut Elems, pre: &str, p: &mut Value, settings: &Value, clip: bool) {
    use filmcraft_media::{RawColorSpace, RawDevelop, RawGamma};
    // the colour space and gamma shown last frame, to tell which of them changed (from the panel
    // or over the control channel)
    let shown = p.get("shownPair").cloned().filter(Value::is_array);
    let follows = clip && p["decodeUsing"] == "project";
    let gamma = p["gamma"].as_str().unwrap_or("linear").to_string();
    let display = RawGamma::parse(&gamma).is_some_and(RawGamma::is_display);
    let enabled = |st: &Value| -> bool {
        match st["id"].as_str().unwrap_or_default() {
            _ if st["kind"] == "fixed" => false,
            "quality" | "decodeUsing" => true,
            _ if follows => false,
            "toneMap" | "rolloff" => display,
            "hdrPeakNits" => gamma == "st2084",
            "metadataCurves" | "cdl" => gamma != "linear",
            "creativeLut" => gamma != "linear" && st["available"] != false,
            _ => true,
        }
    };
    let all: Vec<Value> = settings.as_array().cloned().unwrap_or_default();
    ui.horizontal_top(|ui| {
        for column in 1..=3u64 {
            ui.vertical(|ui| {
                egui::Grid::new((pre, "camera-raw", column)).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                    for st in all.iter().filter(|st| st["column"] == column) {
                        let id = st["id"].as_str().unwrap_or_default().to_string();
                        let label = crate::i18n::t(st["label"].as_str().unwrap_or(&id)).to_string();
                        let on = enabled(st);
                        let note = crate::i18n::t(st["note"].as_str().unwrap_or_default()).to_string();
                        if st["kind"] == "bool" {
                            ui.label("");
                        } else {
                            ui.add_enabled(on, egui::Label::new(label.as_str()));
                        }
                        let r = ui.add_enabled_ui(on, |ui| match st["kind"].as_str().unwrap_or_default() {
                            "bool" => {
                                let recorded = st["recorded"].as_bool();
                                let mut v = p[id.as_str()].as_bool().or(recorded).unwrap_or(false);
                                ui.horizontal(|ui| {
                                    let r = ui.checkbox(&mut v, label.as_str());
                                    push(elems, format!("{pre}.{id}"), &r, label.as_str());
                                    if r.changed() {
                                        p[id.as_str()] = json!(v);
                                    }
                                    if p[id.as_str()].is_null() {
                                        ui.label(RichText::new(tl!("as recorded")).weak());
                                    }
                                });
                            }
                            "fixed" => {
                                let shown = st["options"][0]["label"]
                                    .as_str()
                                    .map(str::to_string)
                                    .unwrap_or_else(|| format!("{:.2}", st["value"].as_f64().unwrap_or(0.0)));
                                let r = ui.add(egui::Button::new(crate::i18n::t(&shown)).min_size(egui::vec2(140.0, 0.0)));
                                push(elems, format!("{pre}.{id}"), &r, label.as_str());
                            }
                            "iso" | "number" => raw_value(ui, elems, pre, p, st),
                            _ => {
                                let options: Vec<(String, String)> = st["options"]
                                    .as_array()
                                    .into_iter()
                                    .flatten()
                                    .filter_map(|o| Some((o["id"].as_str()?.to_string(), o["label"].as_str().unwrap_or_default().to_string())))
                                    .collect();
                                combo(ui, elems, &format!("{pre}.{id}"), "", &mut p[id.as_str()], &options);
                            }
                        });
                        if !note.is_empty() {
                            r.response.on_hover_text(note);
                        }
                        ui.end_row();
                    }
                });
                // why the blend is greyed out, where it shows (the panel's first column)
                if let Some(blend) = all.iter().find(|st| st["id"] == "blendType" && st["column"] == column) {
                    ui.scope(|ui| {
                        ui.set_max_width(320.0);
                        ui.add(egui::Label::new(RichText::new(crate::i18n::t(blend["note"].as_str().unwrap_or_default())).weak().small()).wrap());
                    });
                }
            });
            if column < 3 {
                // space, not a separator: a vertical one in a row takes the whole height left
                ui.add_space(18.0);
            }
        }
    });
    // a colour space or gamma that does not go with the other brings a fitting one along
    let space = p["colorSpace"].as_str().and_then(RawColorSpace::parse);
    let curve = p["gamma"].as_str().and_then(RawGamma::parse);
    if let (Some(s), Some(g)) = (space, curve)
        && !RawDevelop::supports(s, g)
    {
        // without a last frame to compare with, a gamma that fits one colour space only wins
        let gamma_changed = match &shown {
            Some(pair) => p["gamma"] != pair[1],
            None => matches!(g, RawGamma::Log3G10 | RawGamma::St2084 | RawGamma::Hlg),
        };
        if gamma_changed {
            p["colorSpace"] = json!(RawDevelop::space_for(g).id());
        } else {
            p["gamma"] = json!(RawDevelop::gamma_for(s).id());
        }
    }
    p["shownPair"] = json!([p["colorSpace"].clone(), p["gamma"].clone()]);
}

fn pairs(v: &[&str]) -> Vec<(String, String)> {
    v.iter().map(|x| (x.to_string(), x.to_string())).collect()
}

/// Draw the open dialog, if any.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    // a Source Settings dialog that went away without OK (closed from outside, or replaced by
    // another dialog) leaves its live preview behind: take it back, as Cancel would
    if app.ui.extras.source_preview.is_some() && app.ui.extras.dialog.as_ref().is_none_or(|d| d.command != "clip.sourceSettings") {
        revert_source_preview(app);
    }
    let Some(mut d) = app.ui.extras.dialog.clone() else { return };
    crate::widgets::revert_drag_on_escape(ctx, egui::Id::unique("menu-dialog-before-drag"), &mut d);
    let Some((title, pre, ok_text)) = meta(&d.command) else {
        app.ui.extras.dialog = None;
        return;
    };
    let mut elems: Elems = Vec::new();
    let mut action: Option<&'static str> = None;
    // a Camera Raw panel button (Update RMD, Use Changes, Use Settings), run after the widgets
    let mut camera_action: Option<&'static str> = None;
    let mut browse: Option<String> = None;
    // Source Settings is read-only for media without development controls
    let info_only = matches!(d.command.as_str(), "file.mediaProperties" | "help.systemCompatibilityReport")
        || (d.command == "clip.sourceSettings" && d.info["settings"].as_array().is_none_or(Vec::is_empty));
    // the title is translated; the window keeps one id whatever the interface language
    let id = egui::Id::unique(("menu-dialog", pre));
    egui::Window::new(title).id(id).collapsible(false).resizable(false).default_width(420.0).centered(ctx).show(ctx, |ui| {
        let p = &mut d.params;
        match d.command.as_str() {
            "edit.find" => {
                radios(ui, &mut elems, pre, p, "scope", tl!("Find in:"), &[("project", tl!("Project")), ("timeline", tl!("Timeline"))]);
                ui.separator();
                if p["scope"] == "timeline" {
                    let mut t = p["rows"][0]["text"].as_str().unwrap_or_default().to_string();
                    ui.horizontal(|ui| {
                        ui.label(tl!("Find:"));
                        let r = ui.add(egui::TextEdit::singleline(&mut t).desired_width(240.0));
                        push(&mut elems, "find.row.0.text", &r, "Find");
                    });
                    p["rows"][0]["text"] = json!(t);
                    radios(
                        ui,
                        &mut elems,
                        pre,
                        p,
                        "in",
                        tl!("Search:"),
                        &[("all", tl!("Clip Names and Markers")), ("clips", tl!("Clip Names")), ("markers", tl!("Markers"))],
                    );
                } else {
                    let cols = pairs(&COLUMNS);
                    let ops: Vec<(String, String)> = FindOp::ALL.iter().map(|o| (o.name().to_string(), o.label().to_string())).collect();
                    for n in 0..2 {
                        let mut row = p["rows"][n].clone();
                        ui.horizontal(|ui| {
                            combo(ui, &mut elems, &format!("find.row.{n}.column"), "", &mut row["column"], &cols);
                            combo(ui, &mut elems, &format!("find.row.{n}.operator"), "", &mut row["operator"], &ops);
                            let mut t = row["text"].as_str().unwrap_or_default().to_string();
                            let r = ui.add(egui::TextEdit::singleline(&mut t).desired_width(160.0));
                            push(&mut elems, format!("find.row.{n}.text"), &r, "Find What");
                            row["text"] = json!(t);
                        });
                        p["rows"][n] = row;
                    }
                    ui.horizontal(|ui| {
                        ui.label(tl!("Match:"));
                        for (v, key, l) in [(true, "all", tl!("All")), (false, "any", tl!("Any"))] {
                            let r = ui.radio(p["matchAll"].as_bool() == Some(v), l);
                            push(&mut elems, format!("find.match.{key}"), &r, l);
                            if r.clicked() {
                                p["matchAll"] = json!(v);
                            }
                        }
                        check(ui, &mut elems, pre, p, "caseSensitive", tl!("Case Sensitive"));
                    });
                }
                if let Some(m) = d.info.get("result").and_then(Value::as_str) {
                    ui.label(RichText::new(m).weak());
                }
            }
            "file.newSearchBin" => {
                text(ui, &mut elems, pre, p, "name", tl!("Name:"), 220.0);
                combo(ui, &mut elems, "searchBin.column", tl!("Search:"), &mut p["column"], &pairs(&COLUMNS));
                let ops: Vec<(String, String)> = FindOp::ALL.iter().map(|o| (o.name().to_string(), o.label().to_string())).collect();
                combo(ui, &mut elems, "searchBin.operator", "", &mut p["operator"], &ops);
                text(ui, &mut elems, pre, p, "text", tl!("Find:"), 220.0);
            }
            "project.settings" => {
                ui.label(tlf!("Project: {name}", name = d.info["project"].as_str().unwrap_or_default()));
                ui.horizontal(|ui| {
                    for (k, l) in [
                        ("general", tl!("General")),
                        ("scratchDisks", tl!("Scratch Disks")),
                        ("ingest", tl!("Ingest Settings")),
                        ("cameraRaw", tl!("Camera RAW")),
                    ] {
                        let r = ui.selectable_label(p["tab"].as_str() == Some(k), l);
                        push(&mut elems, format!("projectSettings.tab.{k}"), &r, l);
                        if r.clicked() {
                            p["tab"] = json!(k);
                        }
                    }
                });
                ui.separator();
                match p["tab"].as_str().unwrap_or("general") {
                    "cameraRaw" => {
                        // the project's Camera RAW settings: what clips that decode using the project develop with
                        let settings = d.info["cameraRaw"]["settings"].clone();
                        camera_raw_panel(ui, &mut elems, "projectSettings.cameraRaw", &mut p["cameraRaw"], &settings, false);
                        ui.horizontal(|ui| {
                            let r = ui.button(tl!("Standard (Rec. 709, BT.1886)"));
                            push(&mut elems, "projectSettings.cameraRaw.standard", &r, "Standard");
                            if r.clicked() {
                                p["cameraRaw"] = develop_values(&filmcraft_media::RawDevelop::STANDARD);
                            }
                            let n = d.info["cameraRaw"]["clips"].as_u64().unwrap_or(0);
                            ui.label(RichText::new(tlf!("{n} camera RAW clips decode using these settings.", n)).weak());
                        });
                        ui.label(RichText::new(crate::i18n::t(d.info["cameraRaw"]["message"].as_str().unwrap_or_default())).weak());
                    }
                    "scratchDisks" => {
                        for (k, l) in [
                            ("captured", tl!("Captured and Generated:")),
                            ("videoPreviews", tl!("Video Previews:")),
                            ("audioPreviews", tl!("Audio Previews:")),
                            ("autoSave", tl!("Project Auto Save:")),
                        ] {
                            ui.label(RichText::new(l).strong());
                            ui.horizontal(|ui| {
                                let mut v = p[k].as_str().unwrap_or_default().to_string();
                                let r = ui.add(egui::TextEdit::singleline(&mut v).hint_text(tl!("Same as Project")).desired_width(240.0));
                                push(&mut elems, format!("projectSettings.scratch.{k}"), &r, l);
                                if r.changed() {
                                    p[k] = if v.is_empty() { Value::Null } else { json!(v) };
                                }
                                let r = ui.button(tl!("Browse…"));
                                push(&mut elems, format!("projectSettings.scratch.{k}.browse"), &r, "Browse");
                                if r.clicked() {
                                    browse = Some(k.to_string());
                                }
                                let r = ui.button(tl!("Same as Project"));
                                push(&mut elems, format!("projectSettings.scratch.{k}.same"), &r, "Same as Project");
                                if r.clicked() {
                                    p[k] = Value::Null;
                                }
                            });
                            let path = d.info["scratch"][k]["path"].as_str().unwrap_or_default();
                            ui.label(RichText::new(tlf!("Path: {path}", path)).weak().small());
                        }
                    }
                    "ingest" => {
                        let mut ing = p["ingest"].clone();
                        let mut on = ing["enabled"].as_bool().unwrap_or(false);
                        let r = ui.checkbox(&mut on, tl!("Ingest"));
                        push(&mut elems, "projectSettings.ingest.enabled", &r, "Ingest");
                        ing["enabled"] = json!(on);
                        let actions = vec![
                            ("copy".to_string(), tl!("Copy").to_string()),
                            ("transcode".to_string(), tl!("Transcode").to_string()),
                            ("createProxies".to_string(), tl!("Create Proxies").to_string()),
                            ("copyAndCreateProxies".to_string(), tl!("Copy and Create Proxies").to_string()),
                        ];
                        combo(ui, &mut elems, "projectSettings.ingest.action", tl!("Action:"), &mut ing["action"], &actions);
                        ui.horizontal(|ui| {
                            ui.label(tl!("Destination:"));
                            let mut v = ing["destination"].as_str().unwrap_or_default().to_string();
                            let r = ui.add(egui::TextEdit::singleline(&mut v).hint_text(tl!("Next to the media")).desired_width(220.0));
                            push(&mut elems, "projectSettings.ingest.destination", &r, "Destination");
                            if r.changed() {
                                ing["destination"] = if v.is_empty() { Value::Null } else { json!(v) };
                            }
                        });
                        p["ingest"] = ing;
                    }
                    _ => {
                        ui.label(RichText::new(tl!("Video Rendering and Playback")).strong());
                        let rs = vec![
                            (filmcraft_engine::project_tools::RENDERER_GPU.to_string(), filmcraft_engine::project_tools::RENDERER_GPU.to_string()),
                            (filmcraft_engine::project_tools::RENDERER_SOFTWARE.to_string(), filmcraft_engine::project_tools::RENDERER_SOFTWARE.to_string()),
                        ];
                        combo(ui, &mut elems, "projectSettings.renderer", tl!("Renderer:"), &mut p["renderer"], &rs);
                        ui.label(RichText::new(tl!("Video")).strong());
                        let vd: Vec<(String, String)> =
                            filmcraft_time::TimeDisplay::ALL.iter().map(|t| (t.label().to_string(), t.label().to_string())).collect();
                        combo(ui, &mut elems, "projectSettings.videoDisplay", tl!("Display Format:"), &mut p["videoDisplay"], &vd);
                        ui.label(RichText::new(tl!("Audio")).strong());
                        combo(
                            ui,
                            &mut elems,
                            "projectSettings.audioDisplay",
                            tl!("Display Format:"),
                            &mut p["audioDisplay"],
                            &[("Audio Samples".to_string(), tl!("Audio Samples").to_string()), ("Milliseconds".to_string(), tl!("Milliseconds").to_string())],
                        );
                        ui.label(RichText::new(tl!("Capture")).strong());
                        combo(ui, &mut elems, "projectSettings.captureFormat", tl!("Capture Format:"), &mut p["captureFormat"], &pairs(&["DV", "HDV"]));
                        ui.label(RichText::new(tl!("Action and Title Safe Areas")).strong());
                        for (k, l) in [("titleSafe", tl!("Title Safe Area")), ("actionSafe", tl!("Action Safe Area"))] {
                            ui.horizontal(|ui| {
                                ui.label(l);
                                for (i, axis) in ["horizontal", "vertical"].iter().enumerate() {
                                    let mut v = p[k][i].as_f64().unwrap_or(0.0);
                                    let r = ui.add(crate::widgets::number_drag(&mut v).range(0.0..=50.0).suffix(" %"));
                                    push(&mut elems, format!("projectSettings.{k}.{axis}"), &r, format!("{l} {axis}"));
                                    p[k][i] = json!(v);
                                    ui.label(*axis);
                                }
                            });
                        }
                    }
                }
            }
            "file.mediaProperties" => {
                let list = d.info.as_array().cloned().unwrap_or_default();
                egui::ScrollArea::vertical().max_height(420.0).show(ui, |ui| {
                    for v in list {
                        ui.label(RichText::new(v["name"].as_str().unwrap_or_default()).strong());
                        let line = |ui: &mut egui::Ui, k: &str, val: String| {
                            if !val.is_empty() && val != "null" {
                                ui.label(format!("{}: {val}", crate::i18n::t(k)));
                            }
                        };
                        let s = |x: &Value| x.as_str().map(str::to_string).unwrap_or_else(|| if x.is_null() { String::new() } else { x.to_string() });
                        line(ui, tl!("File Path"), s(&v["path"]));
                        line(ui, tl!("Type"), format!("{} ({})", s(&v["type"]), s(&v["container"])));
                        line(ui, tl!("File Size"), v["fileSize"].as_u64().map(|b| format!("{:.2} MB", b as f64 / 1e6)).unwrap_or_default());
                        line(ui, tl!("Total Duration"), s(&v["duration"]["timecode"]));
                        line(ui, tl!("Average Data Rate"), v["dataRateKbps"].as_f64().map(|k| format!("{k} kbit/s")).unwrap_or_default());
                        if v["video"].is_object() {
                            let x = &v["video"];
                            line(
                                ui,
                                tl!("Video"),
                                format!(
                                    "{} · {} x {} ({}) · {} fps · {}",
                                    s(&x["codec"]),
                                    x["width"],
                                    x["height"],
                                    s(&x["pixelAspectRatio"]),
                                    s(&x["frameRateLabel"]),
                                    s(&x["pixelFormat"])
                                ),
                            );
                            line(ui, tl!("Colour"), s(&x["color"]));
                        }
                        if v["audio"].is_object() {
                            let x = &v["audio"];
                            line(
                                ui,
                                tl!("Audio"),
                                tlf!("{codec} · {rate} Hz · {n} channel(s)", codec = s(&x["codec"]), rate = x["sampleRate"], n = x["channels"]),
                            );
                        }
                        ui.separator();
                    }
                });
            }
            "clip.editOffline" => {
                text(ui, &mut elems, pre, p, "mediaName", tl!("Media Name:"), 220.0);
                text(ui, &mut elems, pre, p, "tapeName", tl!("Tape Name:"), 220.0);
                text(ui, &mut elems, pre, p, "description", tl!("Description:"), 220.0);
                text(ui, &mut elems, pre, p, "scene", tl!("Scene:"), 220.0);
                text(ui, &mut elems, pre, p, "shot", tl!("Shot/Take:"), 220.0);
                text(ui, &mut elems, pre, p, "logNote", tl!("Log Note:"), 220.0);
            }
            "clip.sourceSettings" => {
                let settings = d.info["settings"].clone();
                let developable = settings.as_array().is_some_and(|s| !s.is_empty());
                ui.horizontal(|ui| {
                    ui.label(RichText::new(tl!("Camera Raw")).strong());
                    if developable {
                        ui.label(RichText::new("RED · IPP2").strong());
                    }
                    ui.label(tlf!("Codec: {codec}", codec = d.info["codec"].as_str().unwrap_or_default()));
                });
                if developable {
                    ui.separator();
                    camera_raw_panel(ui, &mut elems, pre, p, &settings, true);
                    ui.separator();
                    // the panel's buttons, in a plain row (a right-to-left layout makes an
                    // auto-sized window grow to the screen)
                    ui.horizontal(|ui| {
                        let many = d.info["items"].as_array().is_some_and(|i| i.len() > 1);
                        for (flag, label, on, tip) in [
                            ("updateRmd", tl!("Update RMD"), true, tl!("Write this clip's settings into its RMD sidecar beside the clip")),
                            ("useChanges", tl!("Use Changes"), many, tl!("Apply this clip's settings to every selected clip")),
                            ("useSettings", tl!("Use Settings"), true, tl!("Put this clip back to the settings it records (or its RMD sidecar says)")),
                        ] {
                            let r = ui.add_enabled(on, egui::Button::new(label)).on_hover_text(tip);
                            push(&mut elems, format!("{pre}.{flag}"), &r, label);
                            if r.clicked() {
                                camera_action = Some(flag);
                            }
                        }
                    });
                    // what the RAW adjustments start from: as shot, or the clip's RMD sidecar
                    let rmd = match d.info["rmd"].as_str() {
                        Some(path) => tlf!("RMD sidecar in effect: {path}", path),
                        None => tl!("No RMD sidecar: the clip's recorded settings are the camera's.").to_string(),
                    };
                    ui.label(RichText::new(rmd).weak().small());
                }
                ui.label(RichText::new(crate::i18n::t(d.info["message"].as_str().unwrap_or_default())).weak());
            }
            "clip.updateMetadata" => {
                ui.label(tlf!("Write the metadata of the selected clip(s) ({n}) to XMP files next to their media?", n = d.info["count"]));
                ui.label(RichText::new(tl!("Existing XMP files written by other applications are left unchanged.")).weak());
            }
            "clip.automateToSequence" => {
                ui.label(tlf!("To {name}", name = d.info["sequence"].as_str().unwrap_or_default()));
                radios(ui, &mut elems, pre, p, "ordering", tl!("Ordering:"), &[("sort", tl!("Sort Order")), ("selection", tl!("Selection Order"))]);
                radios(
                    ui,
                    &mut elems,
                    pre,
                    p,
                    "placement",
                    tl!("Placement:"),
                    &[("sequentially", tl!("Sequentially")), ("unnumberedMarkers", tl!("At Unnumbered Markers"))],
                );
                radios(ui, &mut elems, pre, p, "method", tl!("Method:"), &[("insert", tl!("Insert Edit")), ("overwrite", tl!("Overwrite Edit"))]);
                number(ui, &mut elems, pre, p, "overlapFrames", tl!("Clip Overlap:"), 0.0..=600.0, " frames");
                number(ui, &mut elems, pre, p, "stillFrames", tl!("Frames per Still:"), 1.0..=100_000.0, " frames");
                ui.label(RichText::new(tl!("Transitions")).strong());
                check(ui, &mut elems, pre, p, "audioTransition", tl!("Apply Default Audio Transition"));
                check(ui, &mut elems, pre, p, "videoTransition", tl!("Apply Default Video Transition"));
                ui.label(RichText::new(tl!("Ignore Options")).strong());
                check(ui, &mut elems, pre, p, "ignoreAudio", tl!("Ignore Audio"));
                check(ui, &mut elems, pre, p, "ignoreVideo", tl!("Ignore Video"));
            }
            "clip.sceneEditDetection" => {
                check(ui, &mut elems, pre, p, "applyCuts", tl!("Apply a cut at each detected cut point"));
                check(ui, &mut elems, pre, p, "createSubclips", tl!("Create a bin of subclips from each detected cut point"));
                check(ui, &mut elems, pre, p, "generateMarkers", tl!("Create a clip marker at each detected cut point"));
                ui.horizontal(|ui| {
                    ui.label(tl!("Sensitivity:"));
                    let mut v = p["sensitivity"].as_f64().unwrap_or(50.0);
                    let r = ui.add(egui::Slider::new(&mut v, 0.0..=100.0));
                    push(&mut elems, "sceneDetect.sensitivity", &r, "Sensitivity");
                    p["sensitivity"] = json!(v);
                });
            }
            "sequence.normalizeMixTrack" => number(ui, &mut elems, pre, p, "db", tl!("Normalize Mix Track to:"), -96.0..=24.0, " dB"),
            "sequence.simplify" => {
                text(ui, &mut elems, pre, p, "name", tl!("Sequence Name:"), 240.0);
                ui.label(RichText::new(tl!("Remove")).strong());
                check(ui, &mut elems, pre, p, "removeDisabled", tl!("Disabled clips"));
                check(ui, &mut elems, pre, p, "removeEmptyTracks", tl!("Empty tracks"));
                check(ui, &mut elems, pre, p, "closeGaps", tl!("Gaps"));
                check(ui, &mut elems, pre, p, "removeVideoEffects", tl!("Video effects"));
                check(ui, &mut elems, pre, p, "removeAudioEffects", tl!("Audio effects"));
                check(ui, &mut elems, pre, p, "removeText", tl!("Text (graphics and captions)"));
                ui.label(RichText::new(tl!("Clips")).strong());
                check(ui, &mut elems, pre, p, "moveClipsDown", tl!("Move clips to the lowest track possible"));
                radios(
                    ui,
                    &mut elems,
                    pre,
                    p,
                    "keep",
                    tl!("Keep:"),
                    &[("both", tl!("Video and Audio")), ("video", tl!("Video Only")), ("audio", tl!("Audio Only"))],
                );
            }
            "sequence.transcribe" => {
                let langs = pairs(&["auto", "en", "es", "fr", "de", "it", "pt", "ja", "ko", "zh", "nl", "ru"]);
                combo(ui, &mut elems, "transcribe.language", tl!("Language:"), &mut p["language"], &langs);
                let mut tracks: Vec<(String, String)> = vec![("mix".into(), tl!("Mix").into())];
                tracks.extend(
                    d.info["tracks"]
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .filter_map(|t| t.as_str())
                        .map(|t| (t.to_string(), tlf!("Audio on track {t}", t))),
                );
                combo(ui, &mut elems, "transcribe.track", tl!("Audio analysis:"), &mut p["track"], &tracks);
                check(ui, &mut elems, pre, p, "diarize", tl!("Recognize when different speakers are talking"));
            }
            "file.saveAsTemplate" => text(ui, &mut elems, pre, p, "name", tl!("Template Name:"), 240.0),
            "file.newColorMatte" => {
                color(ui, &mut elems, pre, p, "color", tl!("Color:"));
                text(ui, &mut elems, pre, p, "name", tl!("Name:"), 220.0);
                number(ui, &mut elems, pre, p, "width", tl!("Width:"), 1.0..=16384.0, " px");
                number(ui, &mut elems, pre, p, "height", tl!("Height:"), 1.0..=16384.0, " px");
                number(ui, &mut elems, pre, p, "seconds", tl!("Duration:"), 0.04..=36000.0, " s");
            }
            "project.matteColor" => color(ui, &mut elems, pre, p, "color", tl!("Color:")),
            "markers.addFlashCue" => {
                text(ui, &mut elems, pre, p, "name", tl!("Name:"), 220.0);
                text(ui, &mut elems, pre, p, "comment", tl!("Comments:"), 220.0);
            }
            "help.systemCompatibilityReport" => {
                let r = &d.info;
                ui.label(RichText::new(r["app"].as_str().unwrap_or_default()).strong());
                for c in r["checks"].as_array().cloned().unwrap_or_default() {
                    let ok = c["ok"].as_bool().unwrap_or(false);
                    ui.label(format!(
                        "{} {}: {}",
                        if ok { "✔" } else { "⚠" },
                        c["name"].as_str().unwrap_or_default(),
                        c["detail"].as_str().unwrap_or_default()
                    ));
                }
                ui.separator();
                let list = |ui: &mut egui::Ui, k: &str, v: &Value| {
                    let items: Vec<String> = v
                        .as_array()
                        .cloned()
                        .unwrap_or_default()
                        .iter()
                        .map(|x| {
                            x.as_str().map(str::to_string).unwrap_or_else(|| {
                                format!("{}{}", x["format"].as_str().unwrap_or_default(), if x["available"] == false { tl!(" (unavailable)") } else { "" })
                            })
                        })
                        .collect();
                    ui.label(format!("{k}: {}", items.join(", ")));
                };
                list(ui, tl!("Decoders"), &r["decoders"]);
                list(ui, tl!("Containers"), &r["containers"]);
                list(ui, tl!("Export formats"), &r["exportFormats"]);
            }
            _ => {}
        }
        if !d.error.is_empty() {
            ui.colored_label(egui::Color32::from_rgb(0xe0, 0x60, 0x60), &d.error);
        }
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            if !info_only {
                let r = ui.button(if d.command == "edit.find" { tl!("Done") } else { tl!("Cancel") });
                push(&mut elems, format!("{pre}.cancel"), &r, "Cancel");
                if r.clicked() {
                    action = Some("cancel");
                }
            }
            let r = ui.button(ok_text);
            push(&mut elems, format!("{pre}.ok"), &r, ok_text);
            if r.clicked() {
                action = Some(if info_only { "cancel" } else { "ok" });
            }
        });
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if let Some(k) = browse {
        // into the dialog's parameters, if the same dialog is still open when the folder comes back
        let command = d.command.clone();
        app.pick_ui(FileDialog::Folder { at: None }, move |app, paths| {
            if let (Some(dir), Some(d)) = (paths.into_iter().next(), app.ui.extras.dialog.as_mut())
                && d.command == command
            {
                d.params[k.as_str()] = json!(dir);
            }
        });
    }
    if crate::widgets::escape_closes(ctx) {
        action = Some("cancel");
    }
    // Source Settings previews live: a changed control reaches the clip (and the monitors) now
    if d.command == "clip.sourceSettings" && !info_only && action.is_none() {
        if let Some(flag) = camera_action {
            camera_raw_action(app, &mut d, flag, ctx.input(|i| i.time));
        }
        source_preview(app, ctx, &mut d);
    }
    match action {
        Some("cancel") => {
            revert_source_preview(app);
            app.ui.extras.dialog = None;
            return;
        }
        Some("ok") => match run(app, &d) {
            Ok(Some(info)) => d.info["result"] = json!(info),
            Ok(None) => {
                app.ui.extras.source_preview = None;
                app.ui.extras.dialog = None;
                return;
            }
            Err(e) => d.error = e,
        },
        _ => {}
    }
    app.ui.extras.dialog = Some(d);
}

/// Run the dialog's command(s). `Ok(Some(text))` keeps the dialog open with a message (Find).
fn run(app: &mut FilmcraftApp, d: &ClipDialogDraft) -> Result<Option<String>, String> {
    let s = &mut app.session;
    let p = &d.params;
    match d.command.as_str() {
        "edit.find" => {
            let mut q = p.clone();
            if q["scope"] == "timeline" {
                q["rows"] = json!([p["rows"][0]]);
            }
            let r = s.execute("edit.find", q).map_err(|e| e.to_string())?;
            Ok(Some(tlf!("Match {n} of {total}", n = r["index"].as_u64().unwrap_or(0) + 1, total = r["matches"].as_u64().unwrap_or(0))))
        }
        "project.settings" => {
            s.execute(
                "file.projectSettings.general",
                json!({"renderer": p["renderer"], "videoDisplay": p["videoDisplay"], "audioDisplay": if p["audioDisplay"] == "Milliseconds" { "milliseconds" } else { "samples" },
                    "captureFormat": p["captureFormat"], "titleSafe": p["titleSafe"], "actionSafe": p["actionSafe"]}),
            )
            .map_err(|e| e.to_string())?;
            s.execute(
                "file.projectSettings.scratchDisks",
                json!({"captured": p["captured"], "videoPreviews": p["videoPreviews"], "audioPreviews": p["audioPreviews"], "autoSave": p["autoSave"]}),
            )
            .map_err(|e| e.to_string())?;
            if p["ingest"] != d.info["ingest"] {
                s.execute("project.ingestSettings", p["ingest"].clone()).map_err(|e| e.to_string())?;
            }
            // the panel's state also holds what it shows (`shownPair`): compare the parameters
            let settings = &d.info["cameraRaw"]["settings"];
            let wanted = panel_params(settings, &p["cameraRaw"]);
            if wanted != panel_params(settings, &d.info["cameraRawOpened"]) {
                s.execute("file.projectSettings.cameraRaw", wanted).map_err(|e| e.to_string())?;
            }
            Ok(None)
        }
        "file.saveAsTemplate" => {
            let r = s.execute("file.saveAsTemplate", p.clone()).map_err(|e| e.to_string())?;
            app.ui.status = tlf!("Saved template {path}", path = r["path"].as_str().unwrap_or_default());
            Ok(None)
        }
        "clip.sceneEditDetection" => {
            s.execute(&d.command, p.clone()).map_err(|e| e.to_string())?;
            app.ui.status = tl!("Scene Edit Detection: analysing in the background (see Progress)").into();
            Ok(None)
        }
        "clip.sourceSettings" => {
            // OK keeps what the live preview applied, as one undo step with it
            let mut q = source_settings_params(d);
            q["items"] = d.info["items"].clone();
            q["commit"] = json!(true);
            s.execute("clip.sourceSettings", q).map_err(|e| e.to_string())?;
            Ok(None)
        }
        cmd => {
            let mut q = p.clone();
            if cmd == "markers.addFlashCue" && q["name"].as_str().is_some_and(str::is_empty) {
                q.as_object_mut().map(|m| m.remove("name"));
            }
            if cmd == "sequence.transcribe" && q["language"] == "auto" {
                q.as_object_mut().map(|m| m.remove("language"));
            }
            s.execute(cmd, q).map_err(|e| e.to_string())?;
            Ok(None)
        }
    }
}

/// Search bins in the Project panel's list view: one row per search bin (`project.searchBin.<id>`),
/// expanded to its live matches. Right-click: Delete.
pub fn search_bin_rows(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    row: &mut usize,
    actions: &mut Vec<(String, Value)>,
    draw_items: &mut dyn FnMut(&mut FilmcraftApp, &mut egui::Ui, &filmcraft_project::Bin, &mut usize, &mut Vec<(String, Value)>),
) {
    let t = app.tokens;
    for sb in app.session.project.search_bins.clone() {
        let open = app.ui.expanded_bins.contains(&sb.id.0);
        let (r, resp) = ui.allocate_exact_size(egui::vec2(ui.available_width(), 22.0), egui::Sense::click());
        if *row % 2 == 1 {
            ui.painter().rect_filled(r, 0.0, t.row_alt);
        }
        *row += 1;
        let x = r.min.x + 6.0;
        crate::icons::paint(
            ui.painter(),
            egui::Rect::from_center_size(egui::pos2(x + 5.0, r.center().y), egui::vec2(10.0, 10.0)),
            if open { crate::icons::Icon::ChevronDown } else { crate::icons::Icon::ChevronRight },
            t.text_dim,
        );
        crate::icons::paint(
            ui.painter(),
            egui::Rect::from_center_size(egui::pos2(x + 20.0, r.center().y), egui::vec2(13.0, 13.0)),
            crate::icons::Icon::Search,
            t.icon,
        );
        let matches = sb.query.find(&app.session.project);
        ui.painter().text(
            egui::pos2(x + 32.0, r.center().y),
            Align2::LEFT_CENTER,
            format!("{} ({})", sb.name, matches.len()),
            crate::theme::Tokens::ui(12.0),
            t.text,
        );
        app.auto.add(&format!("project.searchBin.{}", sb.id.0), r.intersect(ui.clip_rect()), &sb.name);
        if resp.clicked() {
            if open {
                app.ui.expanded_bins.retain(|b| *b != sb.id.0);
            } else {
                app.ui.expanded_bins.push(sb.id.0);
            }
        }
        resp.context_menu(|ui| {
            if ui.button(tl!("Delete Search Bin")).clicked() {
                actions.push(("project.deleteSearchBin".into(), json!({"bin": sb.id.0})));
                ui.close();
            }
        });
        if open {
            let bin =
                filmcraft_project::Bin { id: sb.id, name: sb.name.clone(), children: matches.into_iter().map(filmcraft_project::BinEntry::Item).collect() };
            draw_items(app, ui, &bin, row, actions);
        }
    }
}

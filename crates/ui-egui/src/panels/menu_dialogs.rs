//! Menu long tail (M3.11): the dialogs of Find, Create Search Bin, Project Settings (General /
//! Scratch Disks / Ingest Settings), Get Media File Properties, Edit Offline, Source Settings,
//! Update Metadata, Automate to Sequence, Scene Edit Detection, Normalize Mix Track, Simplify
//! Sequence, Transcribe Sequence, Save as Template, Add Flash Cue Marker and the System
//! Compatibility Report; plus the frontend side of Edit Original / Reveal Log Files (the OS
//! opener), Generate Audio Waveform (the timeline's peak cache), Import from Media Browser (the
//! Media Browser selection) and View ▸ Dynamic Audio Waveforms.
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

use egui::{Align2, RichText};
use filmcraft_project::{FindOp, find::COLUMNS};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::dock::PanelKind;
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
}

impl Default for Extras {
    fn default() -> Self {
        Extras { dialog: None, dynamic_waveforms: true, opened: Vec::new() }
    }
}

type Elems = Vec<(String, egui::Rect, String)>;

fn push(elems: &mut Elems, id: impl Into<String>, r: &egui::Response, label: impl Into<String>) {
    elems.push((id.into(), r.rect, label.into()));
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
        "file.projectSettings.general" | "file.projectSettings.scratchDisks" | "project.ingestSettings" => "project.settings",
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
        "edit.editOriginal" => {
            let r = app.session.execute(id, params.clone()).map_err(|e| e.to_string());
            return Some(r.and_then(|v| {
                for p in v["paths"].as_array().cloned().unwrap_or_default() {
                    open_path(app, ctx, p.as_str().unwrap_or_default(), false)?;
                }
                Ok(v)
            }));
        }
        "clip.generateAudioWaveform" => {
            let r = app.session.execute(id, params.clone()).map_err(|e| e.to_string());
            if let Ok(v) = &r {
                let mut peaks = app.tl.peaks.lock().unwrap_or_else(|e| e.into_inner());
                for i in v["items"].as_array().cloned().unwrap_or_default() {
                    peaks.remove(&filmcraft_engine::project::ItemId(i.as_u64().unwrap_or_default()));
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
            let Some(path) = app.hooks.pick_save_as.as_mut().and_then(|f| f(filter, &[ext], &name)) else {
                return Some(Ok(Value::Null));
            };
            let mut p = params.clone();
            if !p.is_object() {
                p = json!({});
            }
            p["path"] = json!(path);
            return Some(app.session.execute(id, p).map_err(|e| e.to_string()));
        }
        "file.mediaPropertiesFile" if params.get("path").is_none() => {
            let exts: Vec<&str> =
                filmcraft_media::VIDEO_EXTENSIONS.iter().chain(filmcraft_media::AUDIO_EXTENSIONS).chain(filmcraft_media::STILL_EXTENSIONS).copied().collect();
            let Some(path) = app.hooks.pick_open_file.as_mut().and_then(|f| f(tl!("Media"), &exts)) else { return Some(Ok(Value::Null)) };
            let r = app.session.execute(id, json!({"path": path})).map_err(|e| e.to_string());
            if let Ok(v) = &r {
                app.ui.extras.dialog =
                    Some(ClipDialogDraft { command: "file.mediaProperties".into(), params: json!({}), info: json!([v]), error: String::new() });
            }
            return Some(r);
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
                _ => "general",
            };
            let st = s.project.settings.clone();
            let general = s.execute("file.projectSettings.general", json!({})).map_err(|e| e.to_string())?;
            let scratch = filmcraft_engine::project_tools::scratch_paths(s);
            let ingest = serde_json::to_value(&st.ingest).unwrap_or_default();
            let sc = &st.scratch;
            (
                json!({
                    "tab": tab,
                    "renderer": general["renderer"], "videoDisplay": general["videoDisplay"], "audioDisplay": general["audioDisplay"],
                    "captureFormat": general["captureFormat"], "titleSafe": general["titleSafe"], "actionSafe": general["actionSafe"],
                    "captured": sc.captured, "videoPreviews": sc.video_previews, "audioPreviews": sc.audio_previews, "autoSave": sc.auto_save,
                    "ingest": ingest.clone(),
                }),
                json!({"scratch": scratch, "ingest": ingest, "project": s.project.name}),
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
        "clip.sourceSettings" => (json!({}), s.execute("clip.sourceSettings", json!({})).map_err(|e| e.to_string())?),
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
        let r = ui.add(egui::DragValue::new(&mut v).range(range).suffix(suffix));
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

fn pairs(v: &[&str]) -> Vec<(String, String)> {
    v.iter().map(|x| (x.to_string(), x.to_string())).collect()
}

/// Draw the open dialog, if any.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.extras.dialog.clone() else { return };
    crate::widgets::revert_drag_on_escape(ctx, egui::Id::new("menu-dialog-before-drag"), &mut d);
    let Some((title, pre, ok_text)) = meta(&d.command) else {
        app.ui.extras.dialog = None;
        return;
    };
    let mut elems: Elems = Vec::new();
    let mut action: Option<&'static str> = None;
    let mut browse: Option<String> = None;
    let info_only = matches!(d.command.as_str(), "file.mediaProperties" | "clip.sourceSettings" | "help.systemCompatibilityReport");
    // the title is translated; the window keeps one id whatever the interface language
    let id = egui::Id::new(("menu-dialog", pre));
    egui::Window::new(title).id(id).collapsible(false).resizable(false).default_width(420.0).anchor(Align2::CENTER_CENTER, [0.0, 0.0]).show(ctx, |ui| {
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
                    for (k, l) in [("general", tl!("General")), ("scratchDisks", tl!("Scratch Disks")), ("ingest", tl!("Ingest Settings"))] {
                        let r = ui.selectable_label(p["tab"].as_str() == Some(k), l);
                        push(&mut elems, format!("projectSettings.tab.{k}"), &r, l);
                        if r.clicked() {
                            p["tab"] = json!(k);
                        }
                    }
                });
                ui.separator();
                match p["tab"].as_str().unwrap_or("general") {
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
                                    let r = ui.add(egui::DragValue::new(&mut v).range(0.0..=50.0).suffix(" %"));
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
                ui.label(tlf!("Codec: {codec}", codec = d.info["codec"].as_str().unwrap_or_default()));
                ui.label(RichText::new(d.info["message"].as_str().unwrap_or_default()).weak());
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
                check(ui, &mut elems, pre, p, "createSubclips", tl!("Create a subclip for each detected cut point"));
                check(ui, &mut elems, pre, p, "generateMarkers", tl!("Generate clip markers at each detected cut point"));
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
    if let Some(k) = browse
        && let Some(dir) = app.hooks.pick_folder.as_mut().and_then(|f| f())
    {
        d.params[k.as_str()] = json!(dir);
    }
    if crate::widgets::escape_closes(ctx) {
        action = Some("cancel");
    }
    match action {
        Some("cancel") => {
            app.ui.extras.dialog = None;
            return;
        }
        Some("ok") => match run(app, &d) {
            Ok(Some(info)) => d.info["result"] = json!(info),
            Ok(None) => {
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

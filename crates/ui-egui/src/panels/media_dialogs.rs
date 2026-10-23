//! Media management dialogs: Link Media (offline media / relink), Make Offline, Create Proxies and
//! Project Manager. Their state lives in `UiState` (serde), so agents can open and fill them with
//! `ui.set` as well as by clicking; the actions run engine commands (`media.*`,
//! `file.projectManager`).
//!
//! Automation ids:
//! - Link Media: `linkMedia.row.<n>`, `linkMedia.match.<fileName|extension|clipId|duration|mediaStart|metadata>`,
//!   `linkMedia.alignTimecode`, `linkMedia.relinkOthers`, `linkMedia.folder`, `linkMedia.browse`,
//!   `linkMedia.exactName`, `linkMedia.search`, `linkMedia.stopSearch`, `linkMedia.candidate.<n>`, `linkMedia.preview`,
//!   `linkMedia.status.<n>`, `linkMedia.nextStep`, `linkMedia.link`, `linkMedia.locate`,
//!   `linkMedia.offline`, `linkMedia.offlineAll`, `linkMedia.cancel`.
//! - Make Offline: `makeOffline.keep`, `makeOffline.delete`, `makeOffline.ok`, `makeOffline.cancel`.
//! - Create Proxies: `proxies.preset.<id>`, `proxies.destination`, `proxies.browse`, `proxies.ok`, `proxies.cancel`.
//! - Project Manager: `pm.seq.<id>`, `pm.mode.<collect|consolidate>`, `pm.preset.<id>`, `pm.excludeUnused`,
//!   `pm.handles`, `pm.includeProxies`, `pm.includePreviews`, `pm.destination`, `pm.browse`,
//!   `pm.calculate`, `pm.sizes`, `pm.ok`, `pm.cancel`.

use egui::{Color32, RichText};
use serde_json::{Value, json};

use crate::native_dialogs::FileDialog;
use crate::state::{LinkMediaDraft, ProjectManagerDraft, ProxyDraft};
use crate::{FilmcraftApp, RelinkHint};

type Elems = Vec<(String, egui::Rect, String)>;

fn push(elems: &mut Elems, id: impl Into<String>, r: &egui::Response, label: impl Into<String>) {
    elems.push((id.into(), r.rect, label.into()));
}

fn mb(b: u64) -> String {
    if b >= 1 << 30 { format!("{:.2} GB", b as f64 / (1u64 << 30) as f64) } else { format!("{:.1} MB", b as f64 / (1u64 << 20) as f64) }
}

/// Draw whichever media dialogs are open; open Link Media when a project needs it.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    super::avid_import::show(app, ctx);
    if app.session.offline.prompt && app.ui.link_media.is_none() {
        app.ui.link_media = Some(LinkMediaDraft::default());
    }
    if app.ui.link_media.is_some() {
        link_media(app, ctx);
    }
    if app.ui.make_offline.is_some() {
        make_offline(app, ctx);
    }
    if app.ui.create_proxies.is_some() {
        create_proxies(app, ctx);
    }
    if app.ui.project_manager.is_some() {
        project_manager(app, ctx);
    }
}

/// Menu / shortcut entry points without params open a dialog. Returns None for other ids.
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if let Some(result) = super::avid_import::route(app, id, params) {
        return Some(result);
    }
    let empty = params.as_object().is_none_or(|m| m.is_empty());
    if !empty {
        return None;
    }
    let enabled = |app: &FilmcraftApp, id: &str| filmcraft_engine::find_command(id).map_or(Ok(()), |c| (c.enabled)(&app.session));
    match id {
        "media.linkMedia" => {
            let r = app.session.execute("media.linkMedia", json!({})).map_err(|e| e.to_string());
            if let Ok(v) = &r
                && v["missing"].as_array().is_some_and(|a| a.is_empty())
            {
                app.ui.status = tl!("All media is online.").into();
                return Some(r);
            }
            app.ui.link_media = Some(LinkMediaDraft::default());
            Some(r)
        }
        "media.makeOffline" => Some(enabled(app, id).map(|_| {
            app.ui.make_offline = Some(false);
            json!({"dialog": "makeOffline"})
        })),
        "media.createProxies" => Some(enabled(app, id).map(|_| {
            let items = app.session.state.project_selection.iter().map(|i| i.0).collect();
            app.ui.create_proxies = Some(ProxyDraft { items, preset: filmcraft_engine::proxies::DEFAULT_PROXY_PRESET.into(), destination: String::new() });
            json!({"dialog": "createProxies"})
        })),
        "media.attachProxies" | "media.reconnectFullRes" => {
            let item = app.session.state.project_selection.first().copied();
            let Some(item) = item else { return Some(Err("select a clip in the Project panel".into())) };
            let exts: Vec<&str> = filmcraft_media::VIDEO_EXTENSIONS.to_vec();
            let hint = RelinkHint { command: id.to_string(), params: json!({"item": item.0}) };
            let id = id.to_string();
            Some(app.pick(FileDialog::Relink { exts: exts.iter().map(|e| e.to_string()).collect(), hint }, move |app, paths| {
                let Some(picked) = paths.into_iter().next() else { return Ok(Value::Null) };
                app.session.execute(&id, json!({"item": item.0, "path": picked})).map_err(|e| e.to_string())
            }))
        }
        "file.projectManager" => Some(enabled(app, id).map(|_| {
            let seqs = app.session.state.active_sequence.map(|s| vec![s.0]).unwrap_or_default();
            let dest = app
                .session
                .path
                .as_deref()
                .and_then(|p| std::path::Path::new(p).parent())
                .map(|d| d.join(tlf!("{name} (copy)", name = app.session.project.name)).to_string_lossy().into_owned())
                .unwrap_or_default();
            app.ui.project_manager = Some(ProjectManagerDraft { sequences: seqs, destination: dest, ..Default::default() });
            json!({"dialog": "projectManager"})
        })),
        _ => None,
    }
}

#[derive(Clone)]
struct MissingRow {
    item: u64,
    name: String,
    file_name: String,
    path: String,
    status: &'static str,
    component: Option<filmcraft_engine::component_relink::ComponentKey>,
}
impl MissingRow {
    fn key(&self) -> String {
        format!("{}:{:?}", self.item, self.component)
    }
    fn target(&self) -> Value {
        json!({"item":self.item,"component":self.component})
    }
}
fn missing_list(app: &FilmcraftApp) -> Vec<MissingRow> {
    if !app.session.offline.rows.is_empty() {
        return app
            .session
            .offline
            .rows
            .iter()
            .filter(|m| m.status != "proxyMissing")
            .map(|m| MissingRow {
                item: m.item,
                name: m.name.clone(),
                file_name: m.file_name.clone(),
                path: m.path.clone(),
                status: m.status,
                component: m.component,
            })
            .collect();
    }
    app.session
        .offline
        .missing
        .iter()
        .filter_map(|id| {
            let it = app.session.project.item(*id)?;
            let m = it.as_media()?;
            let filmcraft_project::MediaRef::File { path } = &m.media else { return None };
            Some(MissingRow {
                item: id.0,
                name: it.name.clone(),
                file_name: path.rsplit(['/', '\\']).next().unwrap_or(path).into(),
                path: path.clone(),
                status: if m.offline { "offline" } else { "missing" },
                component: None,
            })
        })
        .collect()
}
fn component_result<'a>(d: &'a LinkMediaDraft, row: &MissingRow) -> Option<&'a Value> {
    d.component_report.as_ref()?["report"]["results"].as_array()?.iter().find(|r| r["item"] == row.item && r["component"] == json!(row.component))
}

/// Search evidence describes a possible replacement; it never marks media online.
fn row_status(d: &LinkMediaDraft, row: &MissingRow) -> (&'static str, Color32) {
    let status = component_result(d, row).and_then(|r| r["status"].as_str());
    if d.component_job.is_none() && d.selected_key == row.key() && d.candidate.and_then(|i| d.candidates.get(i)).is_some_and(|c| c.1) {
        return (if status == Some("ambiguous") { tl!("Copy selected") } else { tl!("Match found") }, Color32::LIGHT_GREEN);
    }
    match status {
        Some("unique") => (tl!("Match found"), Color32::LIGHT_GREEN),
        Some("ambiguous") => (tl!("Choose a copy"), Color32::LIGHT_YELLOW),
        Some("unreadable") => (tl!("Cannot link"), Color32::LIGHT_RED),
        Some("absent") => (tl!("Not found"), Color32::LIGHT_RED),
        Some("unresolved") => (tl!("Search incomplete"), Color32::LIGHT_YELLOW),
        _ => (row.status, Color32::LIGHT_RED),
    }
}

/// Mirror applyComponentSearch's explicit choice plus complete, unique matches.
/// A selected candidate is required, including when the search was incomplete.
fn link_count(d: &LinkMediaDraft, row: &MissingRow) -> usize {
    if d.component_job.is_some() || !d.candidate.and_then(|i| d.candidates.get(i)).is_some_and(|c| c.1) {
        return 0;
    }
    let others = if row.component.is_some() && d.relink_others {
        d.component_report.as_ref().filter(|r| r["report"]["complete"] == true).and_then(|r| r["report"]["results"].as_array()).map_or(0, |results| {
            results.iter().filter(|r| r["status"] == "unique" && !(r["item"] == row.item && r["component"] == json!(row.component))).count()
        })
    } else {
        0
    };
    1 + others
}

fn load_component_candidates(d: &mut LinkMediaDraft, row: &MissingRow) {
    d.candidates.clear();
    d.candidate = None;
    if let Some(report) = &d.component_report {
        if let Some(found) =
            report["report"]["results"].as_array().and_then(|a| a.iter().find(|r| r["item"] == row.item && r["component"] == json!(row.component)))
        {
            d.candidates = candidates(&found["candidates"]);
            if found["status"] == "unique" && report["report"]["complete"] == true {
                d.candidate = Some(0);
            }
            d.message = match found["status"].as_str() {
                Some("ambiguous") => tl!("Multiple files carry this media identity. Choose the copy to use.").into(),
                Some("absent") => tl!("No matching media identity found in this search.").into(),
                Some("unreadable") => tl!("Identity found, but the file cannot satisfy this component. Hover over the candidate for details.").into(),
                _ => String::new(),
            };
        }
        if report["report"]["complete"] != true {
            d.message.push_str(tl!(" Search incomplete: automatic linking is disabled. Choose a verified file explicitly or search a smaller folder."));
        }
    }
}
fn candidates(v: &Value) -> Vec<(String, bool, Option<bool>, String)> {
    v.as_array()
        .map(|a| {
            a.iter()
                .map(|c| {
                    let problems = c["problems"].as_array().map(|p| p.iter().filter_map(Value::as_str).collect::<Vec<_>>().join("; ")).unwrap_or_default();
                    (c["path"].as_str().unwrap_or_default().into(), c["ok"] == true, c["identityMatch"].as_bool(), problems)
                })
                .collect()
        })
        .unwrap_or_default()
}
fn cancel_component_search(app: &mut FilmcraftApp, d: &mut LinkMediaDraft) {
    if let Some(job) = d.component_job.take()
        && let Err(e) = app.session.execute("jobs.cancel", json!({"job":job}))
    {
        d.message = e.to_string();
    }
}

fn match_params(d: &LinkMediaDraft) -> Value {
    json!({
        "match": {"fileName": d.file_name, "extension": d.extension, "clipId": d.clip_id, "duration": d.duration, "mediaStart": d.media_start, "metadata": d.metadata},
        "alignTimecode": d.align_timecode,
        "relinkOthers": d.relink_others,
    })
}

fn preview_texture(app: &FilmcraftApp, ctx: &egui::Context, path: &str) -> Option<egui::TextureHandle> {
    let key = egui::Id::new(("link-preview", path));
    if let Some(t) = ctx.data(|d| d.get_temp::<Option<egui::TextureHandle>>(key)) {
        return t;
    }
    let tex = app.session.media.open_file(path, &*app.session.services).ok().and_then(|src| {
        let t = filmcraft_time::Tick(src.info().duration.0 / 3);
        let f = src.video_frame(filmcraft_media::FrameRequest { time: t, scale: 0.25 }).ok()?;
        let img = egui::ColorImage::from_rgba_unmultiplied([f.width as usize, f.height as usize], &f.to_rgba8());
        Some(ctx.load_texture(format!("link-preview-{path}"), img, egui::TextureOptions::LINEAR))
    });
    ctx.data_mut(|d| d.insert_temp(key, tex.clone()));
    tex
}

fn link_media(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.link_media.clone() else { return };
    if d.observed_revision.is_some_and(|r| r != app.session.revision) {
        filmcraft_engine::relink::refresh(&mut app.session);
        cancel_component_search(app, &mut d);
        d.component_report = None;
        d.selected_key.clear();
    }
    d.observed_revision = Some(app.session.revision);
    let rows: Vec<_> = missing_list(app).into_iter().filter(|r| !d.skipped.contains(&r.item) && !d.skipped_components.contains(&r.key())).collect();
    if rows.is_empty() {
        cancel_component_search(app, &mut d);
        app.ui.link_media = None;
        app.session.offline.prompt = false;
        return;
    }
    d.row = d.row.min(rows.len() - 1);
    if let Some(job) = d.component_job {
        match app.session.execute("media.componentSearchResult", json!({"job":job})) {
            Ok(v) if v["finished"] == true => {
                d.component_job = None;
                d.component_report = Some(v);
                d.selected_key.clear();
            }
            Ok(v) => {
                d.message = tlf!("Searching MXF identities… {n} entries checked", n = v["entries"]);
                ctx.request_repaint_after(std::time::Duration::from_millis(100));
            }
            Err(e) => {
                d.component_job = None;
                d.component_report = None;
                d.message = e.to_string();
            }
        }
    }
    if let Some(row) = rows.get(d.row)
        && d.selected_key != row.key()
    {
        d.selected_key = row.key();
        d.candidates.clear();
        d.candidate = None;
        if row.component.is_some() {
            load_component_candidates(&mut d, row);
        }
    }
    let component_selected = rows.get(d.row).is_some_and(|r| r.component.is_some());
    let mut elems: Elems = Vec::new();
    let mut keep = true;
    let mut action: Option<&str> = None;
    let accent = app.tokens.accent;
    let sel_path = d.candidate.and_then(|c| d.candidates.get(c)).map(|c| c.0.clone());
    let preview = sel_path.as_deref().and_then(|p| preview_texture(app, ctx, p));
    let rows_max_h = (ctx.content_rect().height() * 0.3).clamp(90.0, 320.0);
    let width = (ctx.content_rect().width() - 40.0).clamp(1.0, 760.0);
    egui::Window::new(tl!("Link Media"))
        .id(egui::Id::new("Link Media"))
        .collapsible(false)
        .resizable(false)
        .default_width(width)
        .max_width(width)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            egui::ScrollArea::vertical().id_salt("link-media-body-scroll").max_height((ctx.content_rect().height() - 140.0).max(1.0)).show(ui, |ui| {
                ui.label(tlf!("{n} media file(s) need reconnecting.", n = rows.len()));
                ui.label(tl!("Search finds matching files. Click Link to reconnect them, or leave them offline."));
                ui.add_space(6.0);
                egui::ScrollArea::both().id_salt("link-media-rows-scroll").max_height(rows_max_h).auto_shrink([false, true]).show(ui, |ui| {
                    egui::Grid::new("link-media-rows").num_columns(4).striped(true).spacing([14.0, 4.0]).show(ui, |ui| {
                        for h in [tl!("Clip Name"), tl!("File Name"), tl!("File Path"), tl!("Status")] {
                            ui.label(RichText::new(h).strong());
                        }
                        ui.end_row();
                        for (k, row) in rows.iter().enumerate() {
                            let (name, fname, path) = (&row.name, &row.file_name, &row.path);
                            let display_name = if row.component.is_some() {
                                // Keep the component label visible when a long clip name is truncated.
                                name.rsplit_once(' ').map(|(clip, component)| format!("{component} · {clip}")).unwrap_or_else(|| name.clone())
                            } else {
                                name.clone()
                            };
                            let r = ui.add_sized([250.0, 24.0], egui::Button::selectable(d.row == k, &display_name).truncate()).on_hover_text(name);
                            push(&mut elems, format!("linkMedia.row.{k}"), &r, name.clone());
                            if r.clicked() && d.row != k {
                                d.row = k;
                                d.candidates.clear();
                                d.candidate = None;
                            }
                            ui.add_sized([160.0, 24.0], egui::Label::new(fname).truncate()).on_hover_text(fname);
                            ui.add_sized([170.0, 24.0], egui::Label::new(RichText::new(path).small()).truncate()).on_hover_text(path);
                            let (status, color) = row_status(&d, row);
                            let r = ui.label(RichText::new(status).color(color));
                            push(&mut elems, format!("linkMedia.status.{k}"), &r, status);
                            ui.end_row();
                        }
                    });
                });
                ui.separator();
                if component_selected {
                    ui.label(tl!("Match by Avid media identity. Picture and audio channels keep their original slots."));
                    let r = ui.checkbox(&mut d.relink_others, tl!("Also link other unique component matches"));
                    push(&mut elems, "linkMedia.relinkOthers", &r, tl!("Also link other unique component matches"));
                } else {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tl!("Match file properties:"));
                        for (id, label, v) in [
                            ("fileName", tl!("File Name"), &mut d.file_name),
                            ("extension", tl!("File Extension"), &mut d.extension),
                            ("clipId", tl!("Clip ID (fingerprint)"), &mut d.clip_id),
                            ("duration", tl!("Duration"), &mut d.duration),
                            ("mediaStart", tl!("Media Start"), &mut d.media_start),
                            ("metadata", tl!("Frame Size / Rate"), &mut d.metadata),
                        ] {
                            let r = ui.checkbox(v, label);
                            push(&mut elems, format!("linkMedia.match.{id}"), &r, label);
                        }
                    });
                    ui.horizontal_wrapped(|ui| {
                        let r = ui.checkbox(&mut d.align_timecode, tl!("Align Timecode"));
                        push(&mut elems, "linkMedia.alignTimecode", &r, "Align Timecode");
                        let r = ui.checkbox(&mut d.relink_others, tl!("Relink others automatically"));
                        push(&mut elems, "linkMedia.relinkOthers", &r, "Relink others automatically");
                    });
                }
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label(tl!("Search in:"));
                    let r = ui.add(egui::TextEdit::singleline(&mut d.folder).desired_width((ui.available_width() * 0.5).min(360.0)).hint_text(tl!("Folder")));
                    push(&mut elems, "linkMedia.folder", &r, "folder");
                    let r = ui.button(tl!("Browse…"));
                    push(&mut elems, "linkMedia.browse", &r, "Browse…");
                    if r.clicked() {
                        action = Some("browse");
                    }
                    if !component_selected {
                        let r = ui.checkbox(&mut d.exact_name, tl!("Exact name matches only"));
                        push(&mut elems, "linkMedia.exactName", &r, "Exact name matches only");
                    }
                    let r = ui.add_enabled(d.component_job.is_none(), egui::Button::new(tl!("Search")));
                    push(&mut elems, "linkMedia.search", &r, "Search");
                    if r.clicked() {
                        action = Some("search");
                    }
                    if d.component_job.is_some() {
                        let r = ui.button(tl!("Stop search"));
                        push(&mut elems, "linkMedia.stopSearch", &r, tl!("Stop search"));
                        if r.clicked() {
                            action = Some("stopSearch");
                        }
                    }
                });
                ui.horizontal(|ui| {
                    let candidates_width = (ui.available_width() - 192.0 - ui.spacing().item_spacing.x).max(1.0);
                    ui.vertical(|ui| {
                        ui.set_width(candidates_width);
                        if d.candidates.is_empty() {
                            ui.label(RichText::new(tl!("No candidates yet — Search a folder or Locate the file.")).weak());
                        }
                        egui::ScrollArea::vertical().id_salt("link-media-candidates-scroll").max_height(108.0).auto_shrink([false, true]).show(ui, |ui| {
                            for (k, (path, ok, idm, problems)) in d.candidates.iter().enumerate() {
                                let mark = match (ok, idm) {
                                    (true, Some(true)) if component_selected => tl!("✔ verified identity"),
                                    (true, Some(true)) => tl!("✔ same file"),
                                    (true, _) => tl!("✔ matches"),
                                    (false, Some(false)) => tl!("✖ different file"),
                                    _ => "✖",
                                };
                                let text = format!("{mark}  {path}");
                                let r = ui.selectable_label(
                                    d.candidate == Some(k),
                                    RichText::new(&text).color(if *ok { Color32::LIGHT_GREEN } else { Color32::from_rgb(0xe0, 0x8a, 0x6a) }),
                                );
                                let r = if problems.is_empty() { r } else { r.on_hover_text(problems) };
                                push(&mut elems, format!("linkMedia.candidate.{k}"), &r, text);
                                if r.clicked() {
                                    d.candidate = Some(k);
                                }
                            }
                        });
                    });
                    let (rect, _) = ui.allocate_exact_size(egui::vec2(192.0, 108.0), egui::Sense::hover());
                    ui.painter().rect_filled(rect, 3.0, Color32::from_rgb(12, 12, 12));
                    match &preview {
                        Some(t) => {
                            let fitted = crate::panels::monitor::fit(rect, t.size()[0] as f32, t.size()[1] as f32);
                            ui.painter().image(t.id(), fitted, egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0)), Color32::WHITE);
                        }
                        None => {
                            ui.painter().text(rect.center(), egui::Align2::CENTER_CENTER, tl!("Preview"), crate::theme::Tokens::ui(11.0), Color32::GRAY);
                        }
                    }
                    elems.push(("linkMedia.preview".into(), rect, "preview".into()));
                });
                if let Some(report) = &d.component_report
                    && let Some(diagnostics) = report["report"]["diagnostics"].as_array()
                    && !diagnostics.is_empty()
                {
                    let response = ui.collapsing(tl!("Search diagnostics"), |ui| {
                        for message in diagnostics.iter().filter_map(Value::as_str) {
                            ui.label(message);
                        }
                        if let Some(n) = report["report"]["omittedDiagnostics"].as_u64().filter(|n| *n > 0) {
                            ui.label(tlf!("{n} additional diagnostics omitted", n));
                        }
                    });
                    push(&mut elems, "linkMedia.diagnostics", &response.header_response, tl!("Search diagnostics"));
                }
                if !d.message.is_empty() {
                    ui.colored_label(Color32::from_rgb(0xe0, 0x8a, 0x6a), &d.message);
                }
            });
            ui.add_space(6.0);
            let ready = rows.get(d.row).map_or(0, |row| link_count(&d, row));
            let link_label = match (component_selected, ready) {
                (true, 1) => tl!("Link 1 file").into(),
                (true, n) if n > 1 => tlf!("Link {n} files", n),
                _ => tl!("Link").into(),
            };
            if ready > 0 {
                let message = if component_selected {
                    tlf!("Click '{button}' to apply the matches. Media is still offline until then.", button = link_label)
                } else {
                    tl!("Match selected. Click Link to reconnect the media.").into()
                };
                let r = ui.label(RichText::new(&message).strong().color(Color32::from_rgb(0xe0, 0x8a, 0x6a)));
                push(&mut elems, "linkMedia.nextStep", &r, message);
            }
            ui.horizontal_wrapped(|ui| {
                for (id, label) in [("offlineAll", tl!("Offline All")), ("offline", tl!("Offline")), ("cancel", tl!("Cancel")), ("locate", tl!("Locate…"))] {
                    let r = ui.button(label);
                    push(&mut elems, format!("linkMedia.{id}"), &r, label);
                    if r.clicked() {
                        action = Some(id);
                    }
                }
                let r = ui.add_enabled(ready > 0, egui::Button::new(RichText::new(&link_label).color(Color32::WHITE)).fill(accent));
                push(&mut elems, "linkMedia.link", &r, link_label);
                if r.clicked() {
                    action = Some("link");
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if crate::widgets::escape_closes(ctx) {
        action = Some("cancel");
    }
    let Some(row) = rows.get(d.row).cloned() else { return };
    let item = row.item;
    match action {
        Some("browse") => app.pick_ui(FileDialog::Folder { at: None }, |app, paths| {
            if let (Some(f), Some(d)) = (paths.into_iter().next(), app.ui.link_media.as_mut()) {
                d.folder = f;
            }
        }),
        Some("search") if row.component.is_some() => {
            d.candidates.clear();
            d.candidate = None;
            d.component_report = None;
            let targets: Vec<_> =
                if d.relink_others { rows.iter().filter(|r| r.component.is_some()).map(MissingRow::target).collect() } else { vec![row.target()] };
            match app.session.execute("media.searchComponents", json!({"folder":d.folder,"targets":targets})) {
                Ok(v) => {
                    d.component_job = v["job"].as_u64();
                    d.message = tl!("Searching MXF identities…").into();
                }
                Err(e) => d.message = e.to_string(),
            }
        }
        Some("stopSearch") => cancel_component_search(app, &mut d),
        Some("search") => {
            let mut p = match_params(&d);
            p["folder"] = json!(d.folder);
            p["item"] = json!(item);
            p["exactName"] = json!(d.exact_name);
            match app.session.execute("media.search", p) {
                Ok(v) => {
                    d.candidates = candidates(&v["results"][0]["candidates"]);
                    d.candidate = d.candidates.iter().position(|c| c.1);
                    d.message = if d.candidates.is_empty() { tl!("Nothing found with that name.").into() } else { String::new() };
                }
                Err(e) => d.message = e.to_string(),
            }
        }
        Some("locate") => {
            let exts: Vec<&str> =
                filmcraft_media::VIDEO_EXTENSIONS.iter().chain(filmcraft_media::AUDIO_EXTENSIONS).chain(filmcraft_media::STILL_EXTENSIONS).copied().collect();
            let mut params = match_params(&d);
            params["item"] = json!(item);
            cancel_component_search(app, &mut d);
            d.component_report = None;
            let command = match row.component {
                Some(filmcraft_engine::component_relink::ComponentKey::Audio { channel }) => {
                    params = json!({"item":item,"channel":channel});
                    "media.relinkAudioComponent"
                }
                Some(filmcraft_engine::component_relink::ComponentKey::Picture) => {
                    params = json!({"item":item});
                    "media.relinkPictureComponent"
                }
                None => "media.relink",
            };
            let hint = RelinkHint { command: command.into(), params };
            app.pick_ui(FileDialog::Relink { exts: exts.iter().map(|e| e.to_string()).collect(), hint }, move |app, paths| {
                let (Some(path), Some(mut d)) = (paths.into_iter().next(), app.ui.link_media.clone()) else { return };
                // A delayed answer belongs to the row that opened the picker.
                if d.selected_key != row.key() {
                    return;
                }
                relink_to(app, &mut d, &row, path);
                app.ui.link_media = Some(d);
            });
        }
        Some("link") => {
            if let Some(path) = d.candidate.and_then(|c| d.candidates.get(c)).map(|c| c.0.clone()) {
                relink_to(app, &mut d, &row, path);
            }
        }
        Some("offline") => {
            if row.component.is_some() {
                d.skipped_components.push(row.key());
            } else {
                d.skipped.push(item);
            }
            cancel_component_search(app, &mut d);
            d.component_report = None;
            d.candidates.clear();
            d.candidate = None;
        }
        Some("offlineAll") | Some("cancel") => {
            cancel_component_search(app, &mut d);
            let _ = app.session.execute("media.offlineAll", json!({}));
            keep = false;
        }
        _ => {}
    }
    app.ui.link_media = keep.then_some(d);
}

fn make_offline(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut delete) = app.ui.make_offline else { return };
    let mut elems: Elems = Vec::new();
    let mut keep = true;
    let mut ok = false;
    let n = app.session.state.project_selection.len();
    egui::Window::new(tl!("Make Offline"))
        .id(egui::Id::new("Make Offline"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            ui.label(tlf!("Make {n} selected clip(s) offline. Their clips show the offline slate until you link them again.", n));
            let r = ui.radio(!delete, tl!("Media files remain on disk"));
            push(&mut elems, "makeOffline.keep", &r, "Media files remain on disk");
            if r.clicked() {
                delete = false;
            }
            let r = ui.radio(delete, tl!("Media files are deleted"));
            push(&mut elems, "makeOffline.delete", &r, "Media files are deleted");
            if r.clicked() {
                delete = true;
            }
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Cancel"));
                push(&mut elems, "makeOffline.cancel", &r, "Cancel");
                keep &= !r.clicked();
                let r = ui.button(tl!("OK"));
                push(&mut elems, "makeOffline.ok", &r, "OK");
                ok = r.clicked();
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if ok {
        if let Err(e) = app.session.execute("media.makeOffline", json!({"deleteFiles": delete})) {
            app.ui.status = e.to_string();
        }
        keep = false;
    }
    app.ui.make_offline = (keep && !crate::widgets::escape_closes(ctx)).then_some(delete);
}

fn create_proxies(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.create_proxies.clone() else { return };
    let mut elems: Elems = Vec::new();
    let mut keep = true;
    let mut ok = false;
    let mut browse = false;
    egui::Window::new(tl!("Create Proxies"))
        .id(egui::Id::new("Create Proxies"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            ui.label(tlf!("{n} clip(s) selected.", n = d.items.len()));
            ui.add_space(4.0);
            ui.label(RichText::new(tl!("Format and size")).strong());
            for p in filmcraft_engine::proxies::PRESETS.iter().filter(|p| p.proxy) {
                let r = ui.radio(d.preset == p.id, p.label);
                push(&mut elems, format!("proxies.preset.{}", p.id), &r, p.label);
                if r.clicked() {
                    d.preset = p.id.into();
                }
            }
            ui.add_space(4.0);
            ui.label(RichText::new(tl!("Destination")).strong());
            ui.horizontal(|ui| {
                let r = ui
                    .add(egui::TextEdit::singleline(&mut d.destination).desired_width(320.0).hint_text(tl!("Next to the original media, in a Proxies folder")));
                push(&mut elems, "proxies.destination", &r, "destination");
                let r = ui.button(tl!("Browse…"));
                push(&mut elems, "proxies.browse", &r, "Browse…");
                browse = r.clicked();
            });
            ui.label(RichText::new(tl!("Proxies are made in the background and attached when done. Export always uses full-resolution media.")).weak());
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Cancel"));
                push(&mut elems, "proxies.cancel", &r, "Cancel");
                keep &= !r.clicked();
                let r = ui.add(egui::Button::new(RichText::new(tl!("OK")).color(Color32::WHITE)).fill(app.tokens.accent));
                push(&mut elems, "proxies.ok", &r, "OK");
                ok = r.clicked();
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if browse {
        app.pick_ui(FileDialog::Folder { at: None }, |app, paths| {
            if let (Some(f), Some(d)) = (paths.into_iter().next(), app.ui.create_proxies.as_mut()) {
                d.destination = f;
            }
        });
    }
    if ok {
        let p = json!({"items": d.items, "preset": d.preset, "destination": if d.destination.is_empty() { Value::Null } else { json!(d.destination) }});
        match app.session.execute("media.createProxies", p) {
            Ok(v) => app.ui.status = tlf!("Creating {n} proxy file(s)…", n = v["outputs"].as_array().map_or(0, Vec::len)),
            Err(e) => app.ui.status = e.to_string(),
        }
        keep = false;
    }
    app.ui.create_proxies = (keep && !crate::widgets::escape_closes(ctx)).then_some(d);
}

fn pm_params(d: &ProjectManagerDraft, dry: bool) -> Value {
    json!({
        "destination": d.destination,
        "mode": d.mode,
        "sequences": d.sequences,
        "excludeUnused": d.exclude_unused,
        "handles": d.handles,
        "preset": d.preset,
        "includeProxies": d.include_proxies,
        "includePreviews": d.include_previews,
        "dryRun": dry,
    })
}

fn project_manager(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.project_manager.clone() else { return };
    crate::widgets::revert_drag_on_escape(ctx, egui::Id::new("project-manager-before-drag"), &mut d);
    let before = d.clone();
    let mut elems: Elems = Vec::new();
    let mut keep = true;
    let (mut ok, mut calc, mut browse) = (false, false, false);
    let seqs: Vec<(u64, String)> = app.session.project.sequences().map(|i| (i.id.0, i.name.clone())).collect();
    egui::Window::new(tl!("Project Manager"))
        .id(egui::Id::new("Project Manager"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            ui.label(RichText::new(tl!("Sequences")).strong());
            for (id, name) in &seqs {
                let mut on = d.sequences.contains(id);
                let r = ui.checkbox(&mut on, name);
                push(&mut elems, format!("pm.seq.{id}"), &r, name.clone());
                if r.changed() {
                    if on {
                        d.sequences.push(*id);
                    } else {
                        d.sequences.retain(|s| s != id);
                    }
                }
            }
            ui.separator();
            ui.label(RichText::new(tl!("Resulting Project")).strong());
            for (id, label) in [("collect", tl!("Collect Files and Copy to New Location")), ("consolidate", tl!("Consolidate and Transcode"))] {
                let r = ui.radio(d.mode == id, label);
                push(&mut elems, format!("pm.mode.{id}"), &r, label);
                if r.clicked() {
                    d.mode = id.into();
                }
            }
            if d.mode == "consolidate" {
                ui.indent("pm-presets", |ui| {
                    for p in filmcraft_engine::proxies::PRESETS.iter().filter(|p| !p.proxy) {
                        let r = ui.radio(d.preset == p.id, p.label);
                        push(&mut elems, format!("pm.preset.{}", p.id), &r, p.label);
                        if r.clicked() {
                            d.preset = p.id.into();
                        }
                    }
                });
            }
            ui.separator();
            ui.label(RichText::new(tl!("Options")).strong());
            let r = ui.checkbox(&mut d.exclude_unused, tl!("Exclude Unused Clips"));
            push(&mut elems, "pm.excludeUnused", &r, "Exclude Unused Clips");
            ui.horizontal(|ui| {
                ui.add_enabled_ui(d.mode == "consolidate", |ui| {
                    ui.label(tl!("Include Handles:"));
                    let r = ui.add(crate::widgets::number_drag(&mut d.handles).range(0..=600).suffix(tl!(" frames")));
                    push(&mut elems, "pm.handles", &r, format!("{} frames", d.handles));
                });
            });
            ui.add_enabled_ui(d.mode == "collect", |ui| {
                let r = ui.checkbox(&mut d.include_proxies, tl!("Include Proxies"));
                push(&mut elems, "pm.includeProxies", &r, "Include Proxies");
            });
            let r = ui.checkbox(&mut d.include_previews, tl!("Include Preview Files"));
            push(&mut elems, "pm.includePreviews", &r, "Include Preview Files");
            ui.separator();
            ui.label(RichText::new(tl!("Destination Path")).strong());
            ui.horizontal(|ui| {
                let r = ui.add(egui::TextEdit::singleline(&mut d.destination).desired_width(360.0));
                push(&mut elems, "pm.destination", &r, "destination");
                let r = ui.button(tl!("Browse…"));
                push(&mut elems, "pm.browse", &r, "Browse…");
                browse = r.clicked();
            });
            ui.horizontal(|ui| {
                let text = match d.estimate {
                    Some((a, b, n)) => tlf!("Disk space: original {original} · resulting {result} ({n} files)", original = mb(a), result = mb(b), n),
                    None => tl!("Disk space: —").into(),
                };
                let r = ui.label(&text);
                push(&mut elems, "pm.sizes", &r, text);
                let r = ui.button(tl!("Calculate"));
                push(&mut elems, "pm.calculate", &r, "Calculate");
                calc = r.clicked();
            });
            if !d.message.is_empty() {
                ui.colored_label(Color32::from_rgb(0xe0, 0x8a, 0x6a), &d.message);
            }
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Cancel"));
                push(&mut elems, "pm.cancel", &r, "Cancel");
                keep &= !r.clicked();
                let r = ui.add_enabled(
                    !d.destination.is_empty() && !d.sequences.is_empty(),
                    egui::Button::new(RichText::new(tl!("OK")).color(Color32::WHITE)).fill(app.tokens.accent),
                );
                push(&mut elems, "pm.ok", &r, "OK");
                ok = r.clicked();
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if browse {
        app.pick_ui(FileDialog::Folder { at: None }, |app, paths| {
            if let (Some(f), Some(d)) = (paths.into_iter().next(), app.ui.project_manager.as_mut()) {
                d.destination = f;
            }
        });
    }
    // settings changed: the estimate is stale
    if (d.mode != before.mode
        || d.sequences != before.sequences
        || d.exclude_unused != before.exclude_unused
        || d.handles != before.handles
        || d.preset != before.preset)
        && !calc
    {
        d.estimate = None;
    }
    if calc {
        match app.session.execute("file.projectManager", pm_params(&d, true)) {
            Ok(v) => {
                d.estimate =
                    Some((v["originalBytes"].as_u64().unwrap_or(0), v["resultBytes"].as_u64().unwrap_or(0), v["files"].as_array().map_or(0, Vec::len)));
                d.message.clear();
            }
            Err(e) => d.message = e.to_string(),
        }
    }
    if ok {
        match app.session.execute("file.projectManager", pm_params(&d, false)) {
            Ok(v) => {
                app.ui.status = tlf!("Project Manager: writing {path}", path = v["project"].as_str().unwrap_or_default());
                keep = false;
            }
            Err(e) => d.message = e.to_string(),
        }
    }
    app.ui.project_manager = (keep && !crate::widgets::escape_closes(ctx)).then_some(d);
}

/// Relink the captured row, retaining component identity checks and ordinary file matching.
fn relink_to(app: &mut FilmcraftApp, d: &mut LinkMediaDraft, row: &MissingRow, path: String) {
    let item = row.item;
    let mut p = match_params(d);
    p["item"] = json!(item);
    p["path"] = json!(path);
    let (command, params) = if let Some(component) = row.component {
        if let Some(report) = &d.component_report {
            (
                "media.applyComponentSearch",
                json!({"job":report["job"],"choice":{"item":item,"component":component,"path":path},"relinkOthers":d.relink_others && report["report"]["complete"] == true}),
            )
        } else {
            ("media.relinkComponents", json!({"bindings":[{"item":item,"component":component,"path":path}]}))
        }
    } else {
        ("media.relink", p)
    };
    match app.session.execute(command, params) {
        Ok(v) => {
            d.message.clear();
            d.component_report = None;
            d.selected_key.clear();
            d.candidates.clear();
            d.candidate = None;
            let n = v["relinked"].as_array().map_or(0, Vec::len);
            app.ui.status = if row.component.is_some() { tlf!("Linked {n} media component(s)", n = n) } else { tlf!("Linked {n} clip(s)", n = n) };
        }
        Err(e) => d.message = e.to_string(),
    }
}

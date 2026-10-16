//! Native Avid bin preview. Parsing runs in an engine job; applying is one edit.
use crate::{FilmcraftApp, native_dialogs::FileDialog, state::AvidImportDraft};
use egui::{Color32, RichText};
use serde_json::{Value, json};

pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id != "file.importAvidBin" || params.get("job").is_some() {
        return None;
    }
    let parent = app.import_bin().0;
    app.ui.avid_import = Some(AvidImportDraft { parent: Some(parent), ..Default::default() });
    Some(Ok(json!({"dialog":"avidImport"})))
}
fn cancel(app: &mut FilmcraftApp, job: Option<u64>) {
    if let Some(job) = job
        && let Err(e) = app.session.execute("jobs.cancel", json!({"job":job}))
    {
        app.ui.status = e.to_string();
    }
}
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut draft) = app.ui.avid_import.take() else { return };
    let mut open = true;
    let mut browse = false;
    let mut browse_media = None;
    if draft.media_folders.is_empty() {
        draft.media_folders.push(String::new());
    }
    let mut read = false;
    let mut apply = false;
    if let Some(job) = draft.job {
        match app.session.execute("file.avidBinPreparation", json!({"job":job})) {
            Ok(v) => {
                if v["finished"] == true {
                    draft.report = Some(v["report"].clone());
                } else {
                    ctx.request_repaint_after(std::time::Duration::from_millis(75));
                }
            }
            Err(e) => {
                draft.error = e.to_string();
                cancel(app, draft.job.take());
            }
        }
    }
    let running = draft.job.is_some() && draft.report.is_none();
    let response = egui::Modal::new(egui::Id::new("avid-import")).show(ctx, |ui| {
        ui.set_width(620.0);
        ui.heading(tl!("Import Avid Bin"));
        ui.label(tl!("Transfer the bin contents into a new FilmCraft bin."));
        let mut changed = false;
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let r = ui.add_enabled(
                !running && !draft.completed,
                egui::TextEdit::singleline(&mut draft.path).desired_width(500.0).hint_text(tl!("Choose an .avb file")),
            );
            app.auto.add("avidImport.path", r.rect, &draft.path);
            if r.changed() {
                cancel(app, draft.job.take());
                draft.report = None;
                draft.error.clear();
            }
            let r = ui.add_enabled(!running && !draft.completed, egui::Button::new(tl!("Browse…")));
            app.auto.add("avidImport.browse", r.rect, tl!("Browse"));
            browse = r.clicked();
        });
        ui.label(tl!("Media folders (optional) · Includes subfolders"));
        let mut remove_folder = None;
        egui::ScrollArea::vertical().id_salt("avid-import-folders").max_height(110.0).show(ui, |ui| {
            for (index, folder) in draft.media_folders.iter_mut().enumerate() {
                ui.horizontal(|ui| {
                    let r = ui.add_enabled(
                        !running && !draft.completed,
                        egui::TextEdit::singleline(folder).desired_width(450.0).hint_text(tl!("Choose a folder, or leave empty for offline import")),
                    );
                    app.auto.add(&format!("avidImport.mediaFolder.{index}"), r.rect, folder);
                    if index == 0 {
                        app.auto.add("avidImport.mediaFolder", r.rect, folder);
                    }
                    changed |= r.changed();
                    let r = ui.add_enabled(!running && !draft.completed, egui::Button::new(tl!("Browse…")));
                    app.auto.add(&format!("avidImport.browseMedia.{index}"), r.rect, tl!("Browse"));
                    if index == 0 {
                        app.auto.add("avidImport.browseMedia", r.rect, tl!("Browse"));
                    }
                    if r.clicked() {
                        browse_media = Some(index);
                    }
                    if index > 0 {
                        let r = ui.add_enabled(!running && !draft.completed, egui::Button::new(tl!("Remove")));
                        app.auto.add(&format!("avidImport.removeFolder.{index}"), r.rect, tl!("Remove"));
                        if r.clicked() {
                            remove_folder = Some(index);
                        }
                    }
                });
            }
        });
        if let Some(index) = remove_folder {
            draft.media_folders.remove(index);
            changed = true;
        }
        let r = ui.add_enabled(!running && !draft.completed && draft.media_folders.len() < 32, egui::Button::new(tl!("+ Add folder")));
        app.auto.add("avidImport.addFolder", r.rect, tl!("+ Add folder"));
        if r.clicked() {
            draft.media_folders.push(String::new());
            changed = true;
        }
        if changed {
            cancel(app, draft.job.take());
            draft.report = None;
            draft.error.clear();
        }
        if running {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tl!("Reading bin… You can cancel without changing the project."));
            });
        }
        // Keep actions outside the report viewport, including when every detail section is open.
        let report_height = (ctx.content_rect().height() - ui.min_size().y - 100.0).clamp(80.0, 420.0);
        egui::ScrollArea::vertical().id_salt("avid-import-report").max_height(report_height).show(ui, |ui| {
            if let Some(report) = &draft.report {
                ui.separator();
                for (i, bin) in report["bins"].as_array().into_iter().flatten().enumerate() {
                    let label = tlf!("{name} — {n} clips", name = bin["name"].as_str().unwrap_or(""), n = bin["clips"].as_u64().unwrap_or(0));
                    let r = ui.label(&label);
                    app.auto.add(&format!("avidImport.bin.{i}"), r.rect, &label);
                }
                if let Some(sequences) = report["sequences"].as_array() {
                    ui.label(if sequences.len() == 1 { tl!("1 sequence").to_owned() } else { tlf!("{n} sequences", n = sequences.len()) });
                }
                if report["mediaSearch"].is_object() {
                    let m = &report["mediaSearch"];
                    ui.label(tlf!(
                        "{ready} media components ready to link; {offline} will remain offline",
                        ready = m["linkedComponents"],
                        offline = m["offlineComponents"]
                    ));
                    if m["search"]["complete"] != true {
                        ui.colored_label(app.tokens.danger, tl!("Search was incomplete. No automatic matches were applied."));
                    }
                    let matches = ui.collapsing(tl!("Media matches"), |ui| {
                        for row in m["search"]["results"].as_array().into_iter().flatten().take(64) {
                            ui.label(tlf!(
                                "Item {item} · {kind} · {status}",
                                item = row["item"],
                                kind = row["component"]["kind"].as_str().unwrap_or(""),
                                status = row["status"].as_str().unwrap_or("")
                            ));
                        }
                        if let Some(total) = m["search"]["results"].as_array().map(Vec::len).filter(|n| *n > 64) {
                            ui.label(tlf!("Showing {shown} of {total} matches.", shown = 64, total));
                        }
                    });
                    app.auto.add("avidImport.matches", matches.header_response.rect, tl!("Media matches"));
                    if let Some(diagnostics) = m["search"]["diagnostics"].as_array().filter(|d| !d.is_empty()) {
                        let details = ui.collapsing(tlf!("Technical diagnostics ({n})", n = diagnostics.len()), |ui| {
                            ui.label(tl!("Reader details for troubleshooting; these messages do not by themselves mean an import failed."));
                            for diagnostic in diagnostics.iter().filter_map(Value::as_str) {
                                ui.label(diagnostic);
                            }
                            if let Some(n) = m["search"]["omittedDiagnostics"].as_u64().filter(|n| *n > 0) {
                                ui.label(tlf!("{n} additional diagnostics omitted", n));
                            }
                        });
                        app.auto.add("avidImport.diagnostics", details.header_response.rect, tl!("Technical diagnostics"));
                    }
                } else if report["mode"] != "metadata" {
                    ui.label(tl!("Media remains offline until you link its components."));
                }
                if let Some(errors) = report["errors"].as_array().filter(|e| !e.is_empty()) {
                    ui.colored_label(app.tokens.danger, tlf!("{n} conversion problems — partial results will still be imported", n = errors.len()));
                    for error in errors.iter().filter_map(Value::as_str) {
                        ui.colored_label(app.tokens.danger, error);
                    }
                }
                if let Some(warnings) = report["warnings"].as_array().filter(|w| !w.is_empty()) {
                    let notes = ui.collapsing(tlf!("Conversion notes ({n})", n = warnings.len()), |ui| {
                        for warning in warnings.iter().filter_map(Value::as_str) {
                            ui.label(warning);
                        }
                    });
                    app.auto.add("avidImport.notes", notes.header_response.rect, tl!("Conversion notes"));
                }
                ui.label(
                    RichText::new(if draft.completed {
                        tl!("Imported. Review the conversion problems, then close this report.")
                    } else {
                        tl!("Click 'Import' to transfer this snapshot. The original Avid bin will not be changed.")
                    })
                    .color(Color32::from_rgb(0xe0, 0x8a, 0x6a)),
                );
            }
            if !draft.error.is_empty() {
                let r = ui.colored_label(app.tokens.danger, &draft.error);
                app.auto.add("avidImport.error", r.rect, &draft.error);
            }
        });
        ui.separator();
        ui.horizontal(|ui| {
            let r = ui.button(if draft.completed { tl!("Close") } else { tl!("Cancel") });
            app.auto.add("avidImport.cancel", r.rect, tl!("Cancel"));
            if r.clicked() {
                open = false;
            }
            let r = ui.add_enabled(!running && !draft.completed && !draft.path.trim().is_empty(), egui::Button::new(tl!("Read bin")));
            app.auto.add("avidImport.read", r.rect, tl!("Read bin"));
            read = r.clicked();
            let r = ui.add_enabled(draft.report.is_some() && draft.job.is_some(), egui::Button::new(tl!("Import")).fill(app.tokens.accent));
            app.auto.add("avidImport.import", r.rect, tl!("Import"));
            apply = r.clicked();
        });
    });
    if response.should_close() {
        open = false;
    }
    if browse {
        let previous_path = draft.path.clone();
        app.pick_ui(FileDialog::open_files(&["avb"]), move |app, paths| {
            let (Some(path), Some(current)) = (paths.into_iter().next(), app.ui.avid_import.as_mut()) else { return };
            if current.path != previous_path || current.completed {
                return;
            }
            let job = current.job.take();
            current.path = path;
            current.report = None;
            current.error.clear();
            cancel(app, job);
        });
    }
    if let Some(index) = browse_media {
        let previous_path = draft.path.clone();
        let previous_folders = draft.media_folders.clone();
        app.pick_ui(FileDialog::Folder { at: None }, move |app, paths| {
            let (Some(folder), Some(current)) = (paths.into_iter().next(), app.ui.avid_import.as_mut()) else { return };
            // A delayed folder choice must not move to another row after removal/reordering.
            if current.path != previous_path || current.media_folders != previous_folders || current.completed {
                return;
            }
            let Some(value) = current.media_folders.get_mut(index) else { return };
            *value = folder;
            let job = current.job.take();
            current.report = None;
            current.error.clear();
            cancel(app, job);
        });
    }
    if read {
        draft.report = None;
        draft.error.clear();
        match app.session.execute(
            "file.prepareAvidBin",
            json!({"path":draft.path,"parent":draft.parent,"mediaFolders":draft.media_folders.iter().filter(|v| !v.trim().is_empty()).collect::<Vec<_>>()}),
        ) {
            Ok(v) => draft.job = v["job"].as_u64(),
            Err(e) => draft.error = e.to_string(),
        }
    }
    if apply {
        match app.session.execute("file.importAvidBin", json!({"job":draft.job})) {
            Ok(v) => {
                let count = v["bins"].as_array().into_iter().flatten().filter_map(|b| b["clips"].as_u64()).sum::<u64>();
                app.ui.status =
                    tlf!("Imported {clips} Avid clips and {sequences} sequences.", clips = count, sequences = v["sequences"].as_array().map_or(0, Vec::len));
                draft.job = None;
                draft.completed = true;
                let errors = v["errors"].as_array().filter(|e| !e.is_empty());
                if let Some(errors) = errors {
                    draft.error = tlf!("Import completed with {n} conversion problems. Review the messages above.", n = errors.len());
                    draft.report = Some(v);
                } else {
                    open = false;
                }
            }
            Err(e) => {
                draft.error = e.to_string();
                draft.report = None;
            }
        }
    }
    if open {
        app.ui.avid_import = Some(draft);
    } else {
        cancel(app, draft.job);
    }
}

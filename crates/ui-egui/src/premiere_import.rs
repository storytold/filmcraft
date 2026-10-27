//! Show the conversion result from the engine, regardless of whether import began in a menu,
//! File > Import, drag/drop, the CLI bridge or the control channel.

use serde_json::{Value, json};

use crate::FilmcraftApp;

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(report) = app.ui.import_report.take() else { return };
    if report.get("documentEpoch").and_then(Value::as_u64) != Some(app.session.document_epoch) {
        return;
    }
    let mut open = true;
    let mut close = false;
    let mut save = false;
    let shown = egui::Window::new(tl!("Premiere project imported"))
        .id(egui::Id::unique("premiere-import-report"))
        .open(&mut open)
        .collapsible(false)
        .default_width(600.0)
        .default_pos(crate::widgets::dialog_center(ctx))
        .pivot(egui::Align2::CENTER_CENTER)
        .show(ctx, |ui| {
            ui.label(tl!("The sequences are ready to edit. Save as a FilmCraft project to keep the conversion."));
            ui.label(tl!("Your Premiere project and source media have not been changed."));
            ui.separator();
            if let Some(sequences) = report.get("sequenceSummaries").and_then(Value::as_array) {
                for sequence in sequences.iter().take(64) {
                    let name = sequence.get("name").and_then(Value::as_str).unwrap_or("");
                    let video = sequence.get("videoClips").and_then(Value::as_u64).unwrap_or(0);
                    let audio = sequence.get("audioClips").and_then(Value::as_u64).unwrap_or(0);
                    let line = tlf!("{name}: {video} video clips, {audio} audio clips", name, video, audio);
                    if sequence.get("id") == report.get("activeSequence") {
                        ui.strong(tlf!("Open timeline: {sequence}", sequence = line));
                    } else {
                        ui.label(line);
                    }
                }
                if sequences.len() > 64 {
                    ui.label(tl!("Additional sequences are available in the Project panel."));
                }
            }
            ui.label(tl!("Double-click a sequence in the Project panel to open another timeline."));
            ui.separator();
            ui.strong(tl!("Review the conversion"));
            let scrolled = egui::ScrollArea::vertical().id_salt("premiere-import-warnings").max_height(240.0).show(ui, |ui| {
                if let Some(missing) = report.get("offlineMedia").and_then(Value::as_array).filter(|v| !v.is_empty()) {
                    ui.label(tlf!("{count} media files could not be linked. Use Link Media to locate them.", count = missing.len()));
                    for path in missing.iter().filter_map(Value::as_str).take(40) {
                        ui.label(path);
                    }
                }
                if let Some(warnings) = report.get("report").and_then(Value::as_array) {
                    for warning in warnings.iter().filter_map(Value::as_str) {
                        ui.label(warning);
                        ui.add_space(4.0);
                    }
                }
            });
            app.auto.add("importReport.warnings", scrolled.inner_rect, "Conversion warnings");
            ui.separator();
            ui.horizontal(|ui| {
                let button = ui.button(tl!("Save As FilmCraft Project…"));
                app.auto.add("importReport.saveAs", button.rect, "Save As FilmCraft Project");
                save = button.clicked();
                let button = ui.button(tl!("Continue Editing"));
                app.auto.add("importReport.close", button.rect, "Continue Editing");
                close = button.clicked();
            });
        });
    if let Some(shown) = shown {
        app.auto.add("importReport.window", shown.response.rect, "Premiere project imported");
    }
    if crate::widgets::escape_closes(ctx) {
        close = true;
    }
    if open && !close && !save {
        app.ui.import_report = Some(report);
    }
    if save {
        let _ = crate::menus::invoke(app, ctx, "file.saveAs", json!({}));
    }
}

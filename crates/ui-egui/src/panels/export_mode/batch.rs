//! Queue several sequences with the Export panel's current settings, without switching timelines.

use filmcraft_engine::export_tools::MAX_QUEUE_ITEMS;
use filmcraft_engine::project::ItemId;
use serde::{Deserialize, Serialize};

use super::{Align2, CUSTOM, FilmcraftApp, Reg, export_params, json, vec2};

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BatchExport {
    pub sequences: Vec<u64>,
    pub error: Option<String>,
}

pub(super) fn open(app: &mut FilmcraftApp) {
    let mut sequences: Vec<u64> =
        app.session.state.project_selection.iter().filter(|id| app.session.project.sequence(**id).is_some()).take(MAX_QUEUE_ITEMS).map(|id| id.0).collect();
    if sequences.is_empty()
        && let Some(id) = app.session.state.active_sequence
    {
        sequences.push(id.0);
    }
    app.ui.export.batch = Some(BatchExport { sequences, error: None });
}

pub(super) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut batch) = app.ui.export.batch.take() else { return };
    // ui.set can supply stale or repeated ids; only retain sequences still in this project.
    batch.sequences.truncate(MAX_QUEUE_ITEMS);
    let mut seen = std::collections::BTreeSet::new();
    batch.sequences.retain(|id| app.session.project.sequence(ItemId(*id)).is_some() && seen.insert(*id));
    let mut sequences: Vec<_> = app.session.project.sequences().map(|item| (item.id.0, item.name.clone())).collect();
    sequences.sort_by(|a, b| a.1.cmp(&b.1).then(a.0.cmp(&b.0)));
    let mut reg = Reg::default();
    let mut open = true;
    let mut cancel = false;
    let mut submit = false;
    egui::Window::new(tl!("Queue Sequences…"))
        .id(egui::Id::unique("export-batch"))
        .open(&mut open)
        .collapsible(false)
        .resizable(true)
        .default_size(vec2(480.0, 420.0))
        .pivot(Align2::CENTER_CENTER)
        .default_pos(ctx.content_rect().center())
        .show(ctx, |ui| {
            ui.label(tl!("Use the current export settings and range for each selected sequence."));
            ui.label(tl!("Files are named after each sequence in the export location."));
            ui.label(tlf!("Location: {path}", path = app.ui.export.location));
            ui.horizontal(|ui| {
                let r = ui.add_enabled(sequences.len() <= MAX_QUEUE_ITEMS, egui::Button::new(tl!("Select All")));
                reg.add("export.batch.all", r.rect, "Select All");
                if r.clicked() {
                    batch.sequences = sequences.iter().map(|(id, _)| *id).collect();
                }
                let r = ui.button(tl!("Deselect All"));
                reg.add("export.batch.none", r.rect, "Deselect All");
                if r.clicked() {
                    batch.sequences.clear();
                }
            });
            egui::ScrollArea::vertical().id_salt("export-batch-sequences").max_height(260.0).auto_shrink([false, false]).show(ui, |ui| {
                for (id, name) in &sequences {
                    ui.push_id(id, |ui| {
                        let mut selected = batch.sequences.contains(id);
                        let r = ui.add_enabled(selected || batch.sequences.len() < MAX_QUEUE_ITEMS, egui::Checkbox::new(&mut selected, name));
                        reg.add(format!("export.batch.sequence.{id}"), r.rect, name.clone());
                        if r.changed() {
                            batch.sequences.retain(|other| other != id);
                            if selected {
                                batch.sequences.push(*id);
                            }
                        }
                    });
                }
            });
            if let Some(error) = &batch.error {
                ui.colored_label(app.tokens.danger, error);
            }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label(tlf!("{n} sequences selected", n = batch.sequences.len()));
                let r = ui.add_enabled(!batch.sequences.is_empty(), egui::Button::new(tl!("Send to Queue")));
                reg.add("export.batch.add", r.rect, "Send to Queue");
                submit = r.clicked();
                let r = ui.button(tl!("Cancel"));
                reg.add("export.batch.cancel", r.rect, "Cancel");
                cancel = r.clicked();
            });
        });
    reg.flush(app);
    cancel |= ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape));
    if !open || cancel {
        return;
    }
    if submit {
        let mut params = export_params(app);
        // A trailing separator makes even a not-yet-created directory unambiguous to the engine.
        let location = app.ui.export.location.trim();
        params["path"] = json!(if location.is_empty() { "./".to_string() } else { format!("{location}/") });
        params["sequences"] = json!(batch.sequences);
        if app.ui.export.preset != CUSTOM {
            params["preset"] = json!(app.ui.export.preset);
        }
        match app.session.execute("export.queue.add", params) {
            Ok(_) => {
                app.ui.status = tlf!("Added {n} sequences to the export queue", n = batch.sequences.len());
                return;
            }
            Err(error) => {
                app.ui.status = error.to_string();
                batch.error = Some(error.to_string());
            }
        }
    }
    app.ui.export.batch = Some(batch);
}

//! Project Notes panel (#620): free plain text kept in the project file, for to-dos,
//! instructions for whoever opens the project next, frame numbers and the like. Every change is
//! a `project.setNotes`; one typing session (from focusing the editor until it loses focus) is
//! one undo step.
//!
//! Automation ids: `notes.text` (the editor; its label is the start of the notes). Agents read
//! and write the notes with `project.notes` / `project.setNotes`.

use egui::{Rect, vec2};
use serde_json::json;

use crate::FilmcraftApp;

/// Longest automation label (characters): the full notes are `project.notes`.
const LABEL_CHARS: usize = 200;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 6.0))));
    let ui = &mut child;
    let mut text = app.session.project.notes.clone();
    let height = ui.available_height();
    let r = egui::ScrollArea::vertical()
        .id_salt("project-notes-scroll")
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.add(
                egui::TextEdit::multiline(&mut text)
                    .id_salt("project-notes")
                    .hint_text(tl!("Notes for this project: to-dos, instructions, frame numbers… Saved with the project."))
                    .text_color(t.text)
                    .desired_width(f32::INFINITY)
                    .min_size(vec2(0.0, height)),
            )
        })
        .inner;
    // each focus of the editor starts a new undo step; the edits while it keeps focus join it
    let typing = egui::Id::new("project-notes-typing");
    if r.gained_focus() {
        ui.data_mut(|d| {
            let n = d.get_temp::<u64>(typing).unwrap_or(0).wrapping_add(1);
            d.insert_temp(typing, n);
        });
    }
    if r.changed() {
        let key = ui.data(|d| d.get_temp::<u64>(typing)).unwrap_or(0);
        if let Err(e) = app.session.execute("project.setNotes", json!({"text": text, "merge": key.to_string()})) {
            app.ui.status = e.to_string();
        }
    }
    let label: String = app.session.project.notes.chars().take(LABEL_CHARS).collect();
    app.auto.add("notes.text", r.rect, &label);
}

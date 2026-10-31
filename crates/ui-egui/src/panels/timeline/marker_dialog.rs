//! The Edit Marker dialog: double-click a marker on the Timeline ruler (or Edit Marker… in its menu)
//! to rename it, add a comment, pick its colour and give it a duration (#3023). It runs
//! `markers.edit`, so one OK is one undo step.

use super::{FilmcraftApp, MarkerId, json};
use egui::Align2;
use filmcraft_project::{Label, Marker};

/// The dialog's fields while it is open, kept in egui's temporary data until OK or Cancel.
#[derive(Clone, Debug, PartialEq)]
struct Draft {
    marker: u64,
    name: String,
    comment: String,
    color: Label,
    duration_frames: i64,
    /// The duration when the dialog opened, in whole frames: `durationFrames` is sent only once it has been edited, so a
    /// duration that is not a whole number of frames, or beyond the control's range, survives renaming the marker.
    opened_frames: i64,
}

fn key() -> egui::Id {
    egui::Id::unique("timeline.markerDialog.draft")
}

/// Opens the dialog for `marker`, with the marker's current fields.
pub(crate) fn open(app: &FilmcraftApp, ctx: &egui::Context, marker: MarkerId) {
    let Some(seq) = app.session.active_sequence() else { return };
    let Some(m) = seq.markers.iter().find(|m| m.id == marker) else { return };
    ctx.data_mut(|d| d.insert_temp(key(), draft_of(m, seq.settings.frame_rate.sane())));
}

fn draft_of(m: &Marker, rate: filmcraft_time::FrameRate) -> Draft {
    let frames = rate.frame_at(m.duration).max(0);
    Draft { marker: m.id.0, name: m.name.clone(), comment: m.comment.clone(), color: m.color, duration_frames: frames, opened_frames: frames }
}

/// The parameters of the `markers.edit` that OK runs.
fn edit_params(draft: &Draft) -> serde_json::Value {
    let mut params = json!({
        "target": "program",
        "marker": draft.marker,
        "name": draft.name,
        "comment": draft.comment,
        "color": draft.color.name(),
    });
    if draft.duration_frames != draft.opened_frames {
        params["durationFrames"] = json!(draft.duration_frames);
    }
    params
}

/// Draws the dialog when it is open. It closes by itself when its marker is gone.
pub(crate) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut draft) = ctx.data(|d| d.get_temp::<Draft>(key())) else { return };
    let Some(seq) = app.session.active_sequence() else {
        ctx.data_mut(|d| d.remove::<Draft>(key()));
        return;
    };
    if !seq.markers.iter().any(|m| m.id.0 == draft.marker) {
        ctx.data_mut(|d| d.remove::<Draft>(key()));
        return;
    }
    let accent = app.tokens.accent;
    let (mut ok, mut cancel) = (false, false);
    egui::Window::new(tl!("Edit Marker"))
        .id(key().with("window"))
        .collapsible(false)
        .resizable(false)
        .default_width(320.0)
        .pivot(Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            egui::Grid::new(key().with("grid")).num_columns(2).spacing([8.0, 6.0]).show(ui, |ui| {
                ui.label(tl!("Name:"));
                let r = ui.add(egui::TextEdit::singleline(&mut draft.name).desired_width(220.0));
                app.auto.add_response("timeline.markerDialog.name", &r, "Name");
                ui.end_row();

                ui.label(tl!("Comments:"));
                let r = ui.add(egui::TextEdit::multiline(&mut draft.comment).desired_width(220.0).desired_rows(3));
                app.auto.add_response("timeline.markerDialog.comment", &r, "Comments");
                ui.end_row();

                ui.label(tl!("Color:"));
                let combo = craft_ui::ComboBox::from_id_salt(key().with("color")).selected_text(crate::i18n::t(draft.color.name())).show_ui(ui, |ui| {
                    for color in Label::ALL {
                        ui.selectable_value(&mut draft.color, color, crate::i18n::t(color.name()));
                    }
                });
                app.auto.add_response("timeline.markerDialog.color", &combo.response, "Color");
                ui.end_row();

                ui.label(tl!("Duration (frames):"));
                let r = ui.add(egui::DragValue::new(&mut draft.duration_frames).range(0..=draft.opened_frames.max(10_000_000)).speed(1.0));
                app.auto.add_response("timeline.markerDialog.duration", &r, "Duration");
                ui.end_row();
            });
            ui.add_space(8.0);
            let row = egui::vec2(ui.available_width().max(220.0), ui.spacing().interact_size.y);
            ui.allocate_ui_with_layout(row, egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let r = ui.add(egui::Button::new(egui::RichText::new(tl!("OK")).color(egui::Color32::WHITE)).fill(accent));
                app.auto.add_response("timeline.markerDialog.ok", &r, "OK");
                ok = r.clicked();
                let r = ui.button(tl!("Cancel"));
                app.auto.add_response("timeline.markerDialog.cancel", &r, "Cancel");
                cancel = r.clicked();
            });
        });
    if ok && let Err(e) = app.session.execute("markers.edit", edit_params(&draft)) {
        app.ui.status = e.to_string();
    }
    if ok || cancel || crate::widgets::escape_closes(ctx) {
        ctx.data_mut(|d| d.remove::<Draft>(key()));
    } else {
        ctx.data_mut(|d| d.insert_temp(key(), draft));
    }
}

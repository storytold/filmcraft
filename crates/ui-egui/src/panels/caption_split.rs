//! Split Caption at Line Breaks: edit a caption's text in a window; every line break starts the
//! next caption, right after this one on the timeline (`captions.splitText`).
use crate::FilmcraftApp;
use filmcraft_project::ClipId;
use serde_json::json;
use std::sync::Arc;

#[derive(Clone, Debug)]
pub struct Draft {
    project: Arc<()>,
    caption: u64,
    text: String,
    error: String,
    focus: bool,
}

/// Open the window on caption `caption` of the active sequence.
pub fn open(app: &mut FilmcraftApp, caption: u64) {
    let Some(text) = app.session.active_sequence().and_then(|q| q.find_caption(ClipId(caption))).map(|(_, c)| c.text.clone()) else { return };
    app.ui.caption_split = Some(Draft { project: app.session.project_instance.clone(), caption, text, error: String::new(), focus: true });
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut draft) = app.ui.caption_split.take() else { return };
    // the project was replaced while the window was open: the caption id means nothing now
    if !Arc::ptr_eq(&draft.project, &app.session.project_instance) {
        return;
    }
    let (mut apply, mut cancel) = (false, false);
    let modal = egui::Modal::new(egui::Id::unique("caption-split")).show(ctx, |ui| {
        ui.set_width((ctx.content_rect().width() - 40.0).clamp(240.0, 480.0));
        ui.heading(tl!("Split Caption at Line Breaks"));
        ui.label(tl!("Each line break starts the next caption, right after this one on the timeline."));
        ui.add_space(6.0);
        let response = ui.add(egui::TextEdit::multiline(&mut draft.text).desired_rows(5).desired_width(f32::INFINITY));
        app.auto.add_response("captionSplit.text", &response, tl!("Caption text"));
        if std::mem::take(&mut draft.focus) {
            response.request_focus();
        }
        let n = draft.text.lines().filter(|l| !l.trim().is_empty()).count();
        ui.label(tlf!("Captions: {count}", count = n));
        if !draft.error.is_empty() {
            ui.colored_label(ui.visuals().error_fg_color, &draft.error);
        }
        ui.horizontal(|ui| {
            let ok = ui.add_enabled(n > 0, egui::Button::new(tl!("Apply")));
            app.auto.add_response("captionSplit.apply", &ok, tl!("Apply"));
            apply = ok.clicked();
            let no = ui.button(tl!("Cancel"));
            app.auto.add_response("captionSplit.cancel", &no, tl!("Cancel"));
            cancel = no.clicked();
        });
    });
    if cancel || modal.should_close() {
        return;
    }
    if apply {
        match app.session.execute("captions.splitText", json!({"caption": draft.caption, "text": draft.text})) {
            Ok(_) => return,
            Err(e) => draft.error = e.to_string(),
        }
    }
    app.ui.caption_split = Some(draft);
}

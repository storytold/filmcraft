//! The speech model download (G19): Transcribe without the speech model downloaded asks first,
//! showing the model, its size, where it comes from and its licence, then downloads it in the
//! background (progress, Cancel) and transcribes. Nothing is bundled with FilmCraft; the audio
//! never leaves the computer.
//!
//! Automation ids: `speechModel.download`, `speechModel.cancel`, `speechModel.license`,
//! `speechModel.progress`.

use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::state::SpeechDownload;

/// Run `command` (a transcription) once the speech model it needs is there: when it is missing,
/// open the download dialog, which runs `command` after the download, and return true. False:
/// nothing to download (run the command now).
pub(crate) fn request(app: &mut FilmcraftApp, command: &str, params: Value) -> bool {
    if !filmcraft_engine::transcript::speech_available() {
        return false;
    }
    let Some((m, installed)) = filmcraft_engine::transcript::selected_model(&app.session) else { return false };
    if installed {
        return false;
    }
    app.ui.speech_download =
        Some(SpeechDownload { model: m.id.to_string(), job: None, then_command: command.to_string(), then_params: params, error: String::new() });
    true
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", bytes as f64 / 1_000_000.0)
}

/// The download dialog, while one is asked for or running.
pub fn dialog(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.speech_download.clone() else { return };
    let Some(m) = filmcraft_engine::transcript::find_model(&d.model) else {
        app.ui.speech_download = None;
        return;
    };
    // the running download: how far, and whether it finished
    let job = d.job.and_then(|id| app.session.jobs.iter().find(|j| j.id == id));
    let progress = job.map(|j| {
        use std::sync::atomic::Ordering;
        let p = &j.progress;
        let error = p.error.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone();
        (p.done.load(Ordering::Relaxed), p.total.load(Ordering::Relaxed), p.finished.load(Ordering::Relaxed), error, p.cancel.load(Ordering::Relaxed))
    });
    if let Some((_, _, true, error, cancelled)) = &progress {
        d.job = None;
        match error {
            Some(_) if *cancelled => {
                app.ui.speech_download = None;
                return;
            }
            Some(e) => d.error = e.clone(),
            None => {
                // downloaded: transcribe as asked
                app.ui.speech_download = None;
                let (cmd, params) = (d.then_command.clone(), d.then_params.clone());
                if let Err(e) = crate::menus::invoke(app, ctx, &cmd, params) {
                    app.ui.status = e;
                }
                return;
            }
        }
    }
    if d.job.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
    let mut keep = true;
    let mut start = false;
    let mut cancel = false;
    let mut elems: Vec<(String, egui::Rect, String)> = Vec::new();
    egui::Window::new(tl!("Download Speech Model"))
        .id(egui::Id::new("speech-model-download"))
        .collapsible(false)
        .resizable(false)
        .pivot(egui::Align2::CENTER_CENTER)
        .default_pos(crate::widgets::dialog_center(ctx))
        .show(ctx, |ui| {
            ui.set_max_width(420.0);
            ui.label(tlf!(
                "Transcription runs on this computer with {model}. It needs a one-time download of {size}.",
                model = m.name,
                size = megabytes(m.size())
            ));
            ui.add_space(4.0);
            ui.label(tl!("The audio is not sent anywhere."));
            ui.add_space(8.0);
            egui::Grid::new("speech-model-facts").num_columns(2).spacing([10.0, 4.0]).show(ui, |ui| {
                ui.label(tl!("Source:"));
                ui.label(format!("{} ({})", m.author, m.source));
                ui.end_row();
                ui.label(tl!("Licence:"));
                let l = ui.hyperlink_to(m.license, m.license_url);
                elems.push(("speechModel.license".into(), l.rect, m.license.to_string()));
                ui.end_row();
            });
            ui.add_space(8.0);
            if let Some((done, total, ..)) = &progress {
                let f = if *total > 0 { (*done as f32 / *total as f32).clamp(0.0, 1.0) } else { 0.0 };
                let bar = ui.add(egui::ProgressBar::new(f).text(format!("{} / {}", megabytes(*done), megabytes(*total))));
                elems.push(("speechModel.progress".into(), bar.rect, format!("{f:.2}")));
            }
            if !d.error.is_empty() {
                ui.colored_label(app.tokens.danger, &d.error);
            }
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                let c = ui.button(tl!("Cancel"));
                elems.push(("speechModel.cancel".into(), c.rect, "Cancel".into()));
                if c.clicked() {
                    cancel = true;
                }
                if d.job.is_none() {
                    let label = if d.error.is_empty() { tl!("Download and Transcribe") } else { tl!("Try Again") };
                    let o = ui.add(egui::Button::new(egui::RichText::new(label).color(egui::Color32::WHITE)).fill(app.tokens.accent));
                    elems.push(("speechModel.download".into(), o.rect, label.to_string()));
                    if o.clicked() {
                        start = true;
                    }
                }
            });
        });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if cancel || crate::widgets::escape_closes(ctx) {
        if let Some(j) = d.job.and_then(|id| app.session.jobs.iter().find(|j| j.id == id)) {
            j.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
        }
        keep = false;
    }
    if start {
        d.error.clear();
        match app.session.execute("transcript.downloadModel", json!({"model": m.id, "wait": false})) {
            Ok(r) => d.job = r["job"].as_u64(),
            Err(e) => d.error = e.to_string(),
        }
    }
    app.ui.speech_download = keep.then_some(d);
}

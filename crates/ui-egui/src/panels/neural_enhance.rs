//! Effect Controls ▸ Enhance Speech (Neural): the model row under the effect's header.
//!
//! The effect needs a downloaded model. While the chosen one is missing the row offers the
//! download, asks before it starts (size, source, licence), then shows its progress; with the
//! model in place the row says so. State and downloads go through the engine (`enhance.models`,
//! `enhance.downloadModel`), so an agent sees and does the same.

use egui::{Rect, Sense, vec2};
use filmcraft_project::{ClipId, EffectInstance};
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::FilmcraftApp;

/// The project effect this row belongs to.
pub const EFFECT: &str = "neural_enhance";

const ROW_H: f32 = 22.0;

/// UI state of the model row (serde so agents can read it).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct NeuralEnhanceUi {
    /// The model whose download confirmation is showing.
    pub confirm: Option<String>,
    /// The running download: job id and model id.
    pub download: Option<(u64, String)>,
}

/// What the row shows for the instance's model.
#[derive(Clone, Debug, PartialEq)]
enum State {
    /// Built without the `neural-enhance` feature.
    Unavailable,
    Installed,
    Downloading(f32, String),
    Confirm,
    Missing,
}

struct Model {
    id: String,
    name: String,
    size: u64,
    license: String,
    source: String,
    installed: bool,
}

/// The catalogue model the instance's `model` choice selects, as the engine lists it.
fn model_of(app: &mut FilmcraftApp, e: &EffectInstance) -> Option<(Model, bool, bool)> {
    let v = app.session.execute("enhance.models", json!({})).ok()?;
    let index = e.param("model").and_then(|p| p.value.as_f64()).filter(|i| i.is_finite() && *i >= 0.0).map_or(0, |i| i as usize);
    let list = v["models"].as_array()?;
    let m = list.get(index).or_else(|| list.first())?;
    let model = Model {
        id: m["id"].as_str()?.to_string(),
        name: m["name"].as_str()?.to_string(),
        size: m["size"].as_u64().unwrap_or(0),
        license: m["license"].as_str().unwrap_or_default().to_string(),
        source: m["source"].as_str().unwrap_or_default().to_string(),
        installed: m["installed"].as_bool().unwrap_or(false),
    };
    Some((model, v["available"].as_bool().unwrap_or(false), v["canDownload"].as_bool().unwrap_or(false)))
}

fn state(app: &FilmcraftApp, m: &Model, available: bool) -> State {
    if !available {
        return State::Unavailable;
    }
    if m.installed {
        return State::Installed;
    }
    if let Some((job, id)) = &app.ui.neural_enhance.download
        && *id == m.id
        && let Some(j) = app.session.jobs.iter().find(|j| j.id == *job)
    {
        return State::Downloading(j.progress.fraction(), j.progress.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone());
    }
    if app.ui.neural_enhance.confirm.as_deref() == Some(m.id.as_str()) { State::Confirm } else { State::Missing }
}

/// Report a finished download in the status bar. Called once per frame.
pub fn poll(app: &mut FilmcraftApp) {
    use std::sync::atomic::Ordering;
    let Some((job, id)) = app.ui.neural_enhance.download.clone() else { return };
    let outcome = match app.session.jobs.iter().find(|j| j.id == job) {
        None => Some(Err("the job disappeared".to_string())),
        Some(j) if !j.progress.finished.load(Ordering::Relaxed) => None,
        Some(j) => Some(match &*j.result.lock().unwrap_or_else(std::sync::PoisonError::into_inner) {
            Some(Ok(_)) => Ok(()),
            Some(Err(e)) => Err(e.clone()),
            None => Err("the job stopped".to_string()),
        }),
    };
    if let Some(r) = outcome {
        app.ui.neural_enhance.download = None;
        app.ui.status = match r {
            Ok(()) => format!("Enhance Speech (Neural): {id} installed"),
            Err(e) => format!("Download of the speech-enhancement model failed: {e}"),
        };
    }
}

fn megabytes(bytes: u64) -> String {
    format!("{:.0} MB", (bytes as f64 / 1e6).max(1.0))
}

/// The model row of the effect instance `e` (effect `idx` of `clip`).
pub(crate) fn model_row(app: &mut FilmcraftApp, ui: &mut egui::Ui, body: Rect, clip: ClipId, idx: usize, e: &EffectInstance) {
    let t = app.tokens;
    let Some((m, available, can_download)) = model_of(app, e) else { return };
    let st = state(app, &m, available);
    let rows = match st {
        State::Confirm => 4.0,
        State::Downloading(..) => 2.0,
        _ => 1.0,
    };
    let (r, _) = ui.allocate_exact_size(vec2(body.width(), ROW_H * rows), Sense::hover());
    let inner = Rect::from_min_max(egui::pos2(r.min.x + 42.0, r.min.y + 1.0), egui::pos2(r.max.x - 8.0, r.max.y - 1.0));
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(inner).id_salt(("neural-enhance", clip.0, idx)));
    ui.spacing_mut().item_spacing.y = 2.0;
    let dim = |s: String| egui::RichText::new(s).color(t.text_dim).size(11.5);
    let base = format!("effectControls.effect.{EFFECT}");
    match st {
        State::Unavailable => {
            ui.label(dim("Not available in this build: audio passes through unchanged.".into()));
        }
        State::Installed => {
            ui.label(dim(format!("{} is installed.", m.name)));
        }
        State::Downloading(frac, status) => {
            ui.label(dim(status));
            ui.horizontal(|ui| {
                ui.add(egui::ProgressBar::new(frac).desired_width((inner.width() - 70.0).max(40.0)).show_percentage());
                let b = ui.small_button("Cancel");
                app.auto.add(&format!("{base}.download.cancel"), b.rect, "Cancel download");
                if b.clicked()
                    && let Some((job, _)) = &app.ui.neural_enhance.download
                    && let Some(j) = app.session.jobs.iter().find(|j| j.id == *job)
                {
                    j.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            });
        }
        State::Confirm => {
            ui.label(egui::RichText::new(format!("Download {}? {}, once.", m.name, megabytes(m.size))).color(t.text).size(12.0).strong());
            ui.add(egui::Label::new(dim(format!("From {}. Licence: {}. The file is checked against a pinned SHA-256.", m.source, m.license))).wrap());
            ui.horizontal(|ui| {
                let ok = ui.small_button("Download");
                app.auto.add(&format!("{base}.download.confirm"), ok.rect, "Download");
                let no = ui.small_button("Cancel");
                app.auto.add(&format!("{base}.download.dismiss"), no.rect, "Cancel");
                if ok.clicked() {
                    app.ui.neural_enhance.confirm = None;
                    match app.session.execute("enhance.downloadModel", json!({"model": m.id})) {
                        Ok(v) => app.ui.neural_enhance.download = v["job"].as_u64().map(|j| (j, m.id.clone())),
                        Err(e) => app.ui.status = e.to_string(),
                    }
                }
                if no.clicked() {
                    app.ui.neural_enhance.confirm = None;
                }
            });
        }
        State::Missing => {
            ui.horizontal(|ui| {
                if can_download {
                    let b = ui.small_button(format!("Download {} ({})…", m.name, megabytes(m.size)));
                    app.auto.add(&format!("{base}.download"), b.rect, "Download model");
                    if b.clicked() {
                        app.ui.neural_enhance.confirm = Some(m.id.clone());
                    }
                } else {
                    ui.label(dim(format!("{} is not installed, and this build cannot download it.", m.name)));
                }
            });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model(installed: bool) -> Model {
        Model {
            id: "deepfilternet3".into(),
            name: "DeepFilterNet 3".into(),
            size: 7_983_136,
            license: "MIT OR Apache-2.0".into(),
            source: "https://example.invalid".into(),
            installed,
        }
    }

    #[test]
    fn the_row_follows_the_model_and_the_download() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        assert_eq!(state(&app, &model(false), false), State::Unavailable);
        assert_eq!(state(&app, &model(true), true), State::Installed);
        assert_eq!(state(&app, &model(false), true), State::Missing);
        app.ui.neural_enhance.confirm = Some("deepfilternet3".into());
        assert_eq!(state(&app, &model(false), true), State::Confirm);
        // a confirmation for another model does not show on this one
        app.ui.neural_enhance.confirm = Some("deepfilternet3-ll".into());
        assert_eq!(state(&app, &model(false), true), State::Missing);
        // a download whose job is gone is reported once and cleared
        app.ui.neural_enhance.download = Some((u64::MAX, "deepfilternet3".into()));
        assert_eq!(state(&app, &model(false), true), State::Missing);
        poll(&mut app);
        assert_eq!(app.ui.neural_enhance.download, None);
        assert!(app.ui.status.contains("failed"), "{}", app.ui.status);
    }

    #[test]
    fn the_state_survives_a_round_trip_and_sizes_read_well() {
        let ui = NeuralEnhanceUi { confirm: Some("deepfilternet3".into()), download: Some((3, "deepfilternet3-ll".into())) };
        let back: NeuralEnhanceUi = serde_json::from_value(serde_json::to_value(&ui).unwrap()).unwrap();
        assert_eq!(back, ui);
        assert_eq!(serde_json::from_value::<NeuralEnhanceUi>(json!({})).unwrap(), NeuralEnhanceUi::default());
        assert_eq!(megabytes(7_983_136), "8 MB");
        assert_eq!(megabytes(0), "1 MB");
    }
}

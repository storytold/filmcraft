//! Music panel: describe a cue (mood, length, tempo, key), pick a provider, and add the generated
//! music to the timeline as a clip typed Music. Every action is a `music.*` command, so the panel,
//! MCP and the control channel share one path.
//!
//! Generation runs as a background job (`music.render`, with progress and Cancel); when it
//! finishes the panel runs `music.create`, which uses the cached audio, so the window never
//! stalls. The on-device model needs a one-time download that the user confirms in the panel (size,
//! sources, licences); API providers need their endpoint and key, which are saved to the
//! preferences (`music.api.*`), never to the project.
//!
//! Automation ids: `music.provider`, `music.prompt`, `music.duration`, `music.fillInOut`,
//! `music.bpm`, `music.key`, `music.instrumental`, `music.ducking`, `music.seed`, `music.advanced`
//! (twirl), `music.generate`, `music.cancel`, `music.licenses.accept`, `music.download`,
//! `music.download.cancel`, `music.setting.<name>`.

use egui::{Rect, pos2, vec2};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::tts::{button, job_outcome, label, visible};
use crate::FilmcraftApp;
use crate::icons::Icon;

const PAD: f32 = 14.0;
const FOOTER_H: f32 = 48.0;

/// What the panel is writing (UI state, serde so agents can read it).
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MusicDraft {
    pub prompt: String,
    /// Empty: the preferred provider (`music.provider`).
    pub provider: String,
    pub duration: f64,
    /// Fill the sequence In to Out range instead of `duration`.
    pub fill_in_out: bool,
    pub bpm: String,
    pub key: String,
    pub instrumental: bool,
    pub ducking: bool,
    pub seed: String,
    pub advanced_open: bool,
    /// The user ticked "I accept the licences".
    pub accept_licenses: bool,
    /// A `music.create` waiting for its render job.
    pub pending: Option<Pending>,
    /// The running model download job.
    pub download_job: Option<u64>,
    /// Last `music.providers` answer, refreshed about once a second.
    #[serde(skip)]
    pub providers: Value,
    #[serde(skip)]
    pub providers_at: f64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Pending {
    pub params: Value,
    pub job: u64,
}

impl Default for MusicDraft {
    fn default() -> Self {
        MusicDraft {
            prompt: String::new(),
            provider: String::new(),
            duration: 30.0,
            fill_in_out: false,
            bpm: String::new(),
            key: String::new(),
            instrumental: true,
            ducking: false,
            seed: String::new(),
            advanced_open: false,
            accept_licenses: false,
            pending: None,
            download_job: None,
            providers: Value::Null,
            providers_at: f64::NEG_INFINITY,
        }
    }
}

impl MusicDraft {
    fn params(&self) -> Value {
        let mut p = json!({"prompt": self.prompt.trim(), "instrumental": self.instrumental, "ducking": self.ducking});
        if self.fill_in_out {
            p["fill"] = json!("inout");
        } else {
            p["duration"] = json!(self.duration);
        }
        if !self.provider.is_empty() {
            p["provider"] = json!(self.provider);
        }
        if let Ok(b) = self.bpm.trim().parse::<f64>() {
            p["bpm"] = json!(b);
        }
        if !self.key.trim().is_empty() {
            p["key"] = json!(self.key.trim());
        }
        if let Ok(s) = self.seed.trim().parse::<u64>() {
            p["seed"] = json!(s);
        }
        p
    }
}

fn refresh(app: &mut FilmcraftApp, now: f64, force: bool) {
    if force || now - app.ui.music.providers_at > 1.0 {
        app.ui.music.providers_at = now;
        if let Ok(v) = app.session.execute("music.providers", json!({})) {
            app.ui.music.providers = v;
        }
    }
}

/// Start a generation: render in the background (or reuse the cache), then `music.create`.
fn start(app: &mut FilmcraftApp, params: Value) {
    let mut render = params.clone();
    if let Some(o) = render.as_object_mut() {
        o.remove("ducking");
    }
    match app.session.execute("music.render", render) {
        Ok(r) => {
            let mut params = params;
            params["seed"] = r["seed"].clone();
            match r["job"].as_u64() {
                Some(job) => {
                    app.ui.music.pending = Some(Pending { params, job });
                    app.ui.status = "Generating music…".into();
                }
                None => create(app, params),
            }
        }
        Err(e) => app.ui.status = e.to_string(),
    }
}

fn create(app: &mut FilmcraftApp, params: Value) {
    app.ui.status = match app.session.execute("music.create", params) {
        Ok(r) => format!("Music added on {}", r["track"].as_str().unwrap_or("an audio track")),
        Err(e) => e.to_string(),
    };
}

/// Finish a pending cue or download whose job is done. Runs every frame, also when hidden.
pub fn poll(app: &mut FilmcraftApp) {
    if let Some(p) = app.ui.music.pending.clone()
        && let Some(r) = job_outcome(app, p.job)
    {
        app.ui.music.pending = None;
        match r {
            Ok(()) => create(app, p.params),
            Err(e) => app.ui.status = format!("Music: {e}"),
        }
    }
    if let Some(j) = app.ui.music.download_job
        && let Some(r) = job_outcome(app, j)
    {
        app.ui.music.download_job = None;
        app.ui.music.providers_at = f64::NEG_INFINITY;
        app.ui.status = match r {
            Ok(()) => "Music model installed".into(),
            Err(e) => format!("Download of the music model failed: {e}"),
        };
    }
}

enum Action {
    Generate(Value),
    Cancel,
    AcceptAndDownload,
    CancelDownload,
    SetPref(&'static str, String),
}

fn mb(bytes: u64) -> String {
    if bytes >= 1_000_000_000 { format!("{:.1} GB", bytes as f64 / 1e9) } else { format!("{:.0} MB", bytes as f64 / 1e6) }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    let now = ui.input(|i| i.time);
    refresh(app, now, false);
    let info = app.ui.music.providers.clone();
    let providers: Vec<Value> = info["providers"].as_array().cloned().unwrap_or_default();
    let default_id = info["default"].as_str().unwrap_or("").to_string();
    let chosen_id = if app.ui.music.provider.is_empty() { default_id.clone() } else { app.ui.music.provider.clone() };
    let chosen = providers.iter().find(|p| p["id"] == chosen_id.as_str()).cloned().unwrap_or(Value::Null);
    let ready = chosen["ready"].as_bool().unwrap_or(false);
    let busy = app.ui.music.pending.is_some();
    let progress = app
        .ui
        .music
        .pending
        .as_ref()
        .map(|p| p.job)
        .or(app.ui.music.download_job)
        .and_then(|id| app.session.jobs.iter().find(|j| j.id == id))
        .map(|j| (j.progress.fraction(), j.progress.status.lock().unwrap_or_else(std::sync::PoisonError::into_inner).clone()));
    let prefs = app.session.prefs.music.clone();
    let has_seq = app.session.active_sequence().is_some();
    let mut actions: Vec<Action> = Vec::new();
    let auto = &mut app.auto;
    let d = &mut app.ui.music;

    let body = Rect::from_min_max(pos2(rect.min.x + PAD, rect.min.y + 8.0), pos2(rect.max.x - PAD, rect.max.y - FOOTER_H));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("music-body"));
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(&mut child, |ui| {
        ui.set_max_width(ui.available_width() - 6.0);
        let w = ui.available_width();

        label(ui, &t, "Provider");
        let name = chosen["name"].as_str().unwrap_or("Choose a provider").to_string();
        let cb = egui::ComboBox::from_id_salt("music-provider").selected_text(name).width(w).show_ui(ui, |ui| {
            for p in &providers {
                let id = p["id"].as_str().unwrap_or("");
                let suffix = if p["ready"].as_bool().unwrap_or(false) { "" } else { " (set up)" };
                let text = format!("{}{suffix}", p["name"].as_str().unwrap_or(id));
                if ui.selectable_label(chosen_id == id, text).on_hover_text(p["description"].as_str().unwrap_or("")).clicked() {
                    d.provider = id.to_string();
                }
            }
        });
        auto.add("music.provider", visible(ui, cb.response.rect), "Provider");
        if let Some(desc) = chosen["description"].as_str() {
            ui.label(egui::RichText::new(desc).color(t.text_dim).size(11.0));
        }
        ui.add_space(8.0);

        if chosen["id"] == "stable-audio-local" && !ready {
            setup_card(ui, &t, w, &info["model"], d, auto, progress.clone(), &mut actions);
        } else if chosen["kind"] == "hosted" && !ready {
            ui.label(egui::RichText::new("Add your endpoint and API key under Provider settings below.").color(t.text).size(11.5));
            ui.add_space(6.0);
        }

        label(ui, &t, "Describe the music");
        let te = egui::TextEdit::multiline(&mut d.prompt).hint_text("Warm, slow piano and strings for a reflective scene").desired_width(w).desired_rows(4);
        let r = ui.add(te);
        auto.add("music.prompt", visible(ui, r.rect), "Prompt");
        ui.add_space(10.0);

        label(ui, &t, "Length");
        ui.horizontal(|ui| {
            let c = ui.checkbox(&mut d.fill_in_out, "Fill In to Out");
            auto.add("music.fillInOut", visible(ui, c.rect), "Fill In to Out");
        });
        if !d.fill_in_out {
            ui.spacing_mut().slider_width = w - 70.0;
            let s = ui.add(egui::Slider::new(&mut d.duration, 5.0..=filmcraft_music::MAX_SECONDS).step_by(1.0).suffix(" s"));
            auto.add("music.duration", visible(ui, s.rect), "Duration");
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.vertical(|ui| {
                label(ui, &t, "Tempo (BPM)");
                let r = ui.add(egui::TextEdit::singleline(&mut d.bpm).hint_text("optional").desired_width(w / 2.0 - 8.0));
                auto.add("music.bpm", visible(ui, r.rect), "BPM");
            });
            ui.vertical(|ui| {
                label(ui, &t, "Key");
                let r = ui.add(egui::TextEdit::singleline(&mut d.key).hint_text("e.g. D minor").desired_width(w / 2.0 - 8.0));
                auto.add("music.key", visible(ui, r.rect), "Key");
            });
        });
        ui.add_space(8.0);
        let c = ui.checkbox(&mut d.instrumental, "Instrumental only");
        auto.add("music.instrumental", visible(ui, c.rect), "Instrumental");
        let c = ui.checkbox(&mut d.ducking, "Duck under dialogue");
        auto.add("music.ducking", visible(ui, c.rect), "Duck under dialogue");

        ui.add_space(8.0);
        let (hr, open) = crate::widgets::section_header(ui, egui::Id::new("music-advanced"), "Advanced", d.advanced_open, &t, true);
        auto.add("music.advanced", visible(ui, hr.rect), "Advanced");
        d.advanced_open = open;
        if open {
            ui.add_space(4.0);
            label(ui, &t, "Seed (same seed, same music)");
            let r = ui.add(egui::TextEdit::singleline(&mut d.seed).hint_text("random").desired_width(w));
            auto.add("music.seed", visible(ui, r.rect), "Seed");
            ui.add_space(10.0);
            ui.label(egui::RichText::new("Provider settings").color(t.text).size(12.5).strong());
            ui.label(egui::RichText::new("Saved in your preferences on this computer, never in the project.").color(t.text_dim).size(11.0));
            ui.add_space(4.0);
            for (name, caption, key, value, secret) in [
                ("stabilityEndpoint", "Stability AI endpoint", "music.api.stabilityEndpoint", prefs.api.stability_endpoint.clone(), false),
                ("stabilityApiKey", "Stability AI API key", "music.api.stabilityApiKey", prefs.api.stability_api_key.clone(), true),
                ("otherEndpoint", "Other API endpoint", "music.api.otherEndpoint", prefs.api.other_endpoint.clone(), false),
                ("otherApiKey", "Other API key", "music.api.otherApiKey", prefs.api.other_api_key.clone(), true),
            ] {
                ui.label(egui::RichText::new(caption).color(t.text_dim).size(11.0));
                let mut text = value.clone();
                let r = ui.add(egui::TextEdit::singleline(&mut text).password(secret).desired_width(w));
                auto.add(&format!("music.setting.{name}"), visible(ui, r.rect), caption);
                if text != value {
                    actions.push(Action::SetPref(key, text));
                }
            }
        }
    });

    // footer: progress and Cancel while working, else Generate
    let fy = rect.max.y - FOOTER_H + 10.0;
    let full = Rect::from_min_size(pos2(rect.min.x + PAD, fy), vec2(rect.width() - PAD * 2.0, 28.0));
    if busy || d.download_job.is_some() {
        let (frac, status) = progress.clone().unwrap_or((0.0, String::new()));
        let half = full.width() - 90.0;
        let bar = Rect::from_min_size(full.min, vec2(half, 28.0));
        ui.put(bar, egui::ProgressBar::new(frac).text(status).show_percentage());
        let cr = Rect::from_min_size(pos2(bar.max.x + 8.0, fy), vec2(82.0, 28.0));
        if button(ui, &t, cr, Icon::Close, "Cancel", false, true, "music-cancel").clicked() {
            actions.push(if busy { Action::Cancel } else { Action::CancelDownload });
        }
        auto.add("music.cancel", cr, "Cancel");
    } else {
        let can = ready && !d.prompt.trim().is_empty() && has_seq;
        let label_text = if has_seq { "Generate and add to timeline" } else { "Open a sequence first" };
        if button(ui, &t, full, Icon::Plus, label_text, true, can, "music-generate").clicked() && can {
            actions.push(Action::Generate(d.params()));
        }
        auto.add("music.generate", full, "Generate");
    }

    for a in actions {
        match a {
            Action::Generate(p) => start(app, p),
            Action::Cancel => {
                if let Some(p) = app.ui.music.pending.take()
                    && let Some(j) = app.session.jobs.iter().find(|j| j.id == p.job)
                {
                    j.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                    app.ui.status = "Music generation cancelled".into();
                }
            }
            Action::CancelDownload => {
                if let Some(id) = app.ui.music.download_job
                    && let Some(j) = app.session.jobs.iter().find(|j| j.id == id)
                {
                    j.progress.cancel.store(true, std::sync::atomic::Ordering::Relaxed);
                }
            }
            Action::AcceptAndDownload => {
                match app.session.execute("music.acceptLicenses", json!({})).and_then(|_| app.session.execute("music.download", json!({}))) {
                    Ok(r) => {
                        app.ui.music.download_job = r["job"].as_u64();
                        app.ui.music.providers_at = f64::NEG_INFINITY;
                    }
                    Err(e) => app.ui.status = e.to_string(),
                }
            }
            Action::SetPref(key, value) => {
                if let Err(e) = app.session.execute("prefs.set", json!({"key": key, "value": value})) {
                    app.ui.status = e.to_string();
                }
            }
        }
    }
}

/// The one-time setup of the on-device model: what it is, the licences, the size, and the button.
#[allow(clippy::too_many_arguments)]
fn setup_card(
    ui: &mut egui::Ui,
    t: &crate::theme::Tokens,
    w: f32,
    model: &Value,
    d: &mut MusicDraft,
    auto: &mut crate::automation::Registry,
    progress: Option<(f32, String)>,
    actions: &mut Vec<Action>,
) {
    egui::Frame::new().fill(t.field_bg).stroke(egui::Stroke::new(1.0, t.field_border)).corner_radius(4.0).inner_margin(8.0).show(ui, |ui| {
        ui.set_width(w - 18.0);
        if model.is_null() {
            ui.label(egui::RichText::new("The on-device model is not set up on this computer.").color(t.text).size(11.5));
            ui.label(egui::RichText::new("Use an API provider, or add the model to the models folder (see docs/music.md).").color(t.text_dim).size(11.0));
            return;
        }
        if let (Some(job), Some((frac, status))) = (d.download_job, progress) {
            let _ = job;
            ui.label(egui::RichText::new(status).color(t.text).size(11.5));
            ui.add(egui::ProgressBar::new(frac).show_percentage());
            let b = ui.small_button("Cancel");
            auto.add("music.download.cancel", visible(ui, b.rect), "Cancel download");
            if b.clicked() {
                actions.push(Action::CancelDownload);
            }
            return;
        }
        let size = model["size"].as_u64().unwrap_or(0);
        ui.label(
            egui::RichText::new(format!("Download {}? {}, once.", model["name"].as_str().unwrap_or("the model"), mb(size))).color(t.text).size(12.0).strong(),
        );
        ui.label(
            egui::RichText::new("It runs on your computer, offline. The files are checked against pinned SHA-256 hashes. It is licensed under:")
                .color(t.text_dim)
                .size(11.0),
        );
        for l in model["licenses"].as_array().cloned().unwrap_or_default() {
            ui.hyperlink_to(l["name"].as_str().unwrap_or("licence"), l["url"].as_str().unwrap_or(""));
        }
        ui.label(egui::RichText::new("Powered by Stability AI.").color(t.text_dim).size(11.0));
        let c = ui.checkbox(&mut d.accept_licenses, "I accept these licences");
        auto.add("music.licenses.accept", visible(ui, c.rect), "Accept licences");
        let b = ui.add_enabled(d.accept_licenses, egui::Button::new(format!("Accept and download ({})", mb(size))));
        auto.add("music.download", visible(ui, b.rect), "Download");
        if b.clicked() {
            actions.push(Action::AcceptAndDownload);
        }
    });
    ui.add_space(8.0);
}

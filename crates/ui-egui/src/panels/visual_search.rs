//! Visual search controls. Indexing, model download and query inference are engine jobs.
use crate::FilmcraftApp;
use egui::{Rect, TextureHandle};
use filmcraft_engine::visual_search::SearchResult;
use serde_json::json;
use std::collections::HashMap;
use std::sync::{Arc, Weak};

pub(crate) struct VisualUi {
    job: Option<u64>,
    confirm_download: bool,
    interval: u64,
    result: Weak<SearchResult>,
    textures: HashMap<usize, TextureHandle>,
}
impl Default for VisualUi {
    fn default() -> Self {
        Self { job: None, confirm_download: false, interval: 2, result: Weak::new(), textures: HashMap::new() }
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let info = match app.session.execute("visualSearch.inspect", json!({})) {
        Ok(info) => info,
        Err(e) => {
            app.ui.status = e.to_string();
            return;
        }
    };
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(10.0)).id_salt("visual-search"));
    let mut command = None;
    if info["available"] != true {
        child.label(tl!("Local visual search is not included in this build."));
        return;
    }
    if info["installed"] != true {
        child.label(tl!("Find shots by describing what is visible. Searches stay on this computer."));
        let r = child.add_enabled(info["busy"] != true, egui::Button::new(tl!("Download visual search model…")));
        app.auto.add("text.visual.download", r.rect, "Download visual search model");
        if r.clicked() {
            app.visual_search_ui.confirm_download = true;
        }
    } else {
        child.horizontal_wrapped(|ui| {
            let r = ui.add_enabled(info["busy"] != true, egui::Button::new(tl!("Index project visuals")));
            app.auto.add("text.visual.index", r.rect, "Index project visuals");
            if r.clicked() {
                command = Some(("visualSearch.index", json!({"intervalSeconds":app.visual_search_ui.interval,"wait":false})));
            }
            let r = ui.add(egui::Slider::new(&mut app.visual_search_ui.interval, 2..=10).text(tl!("Sample interval (seconds)")));
            app.auto.add("text.visual.interval", r.rect, "Sample interval (seconds)");
        });
        child.label(tlf!("{sources} indexed sources · {samples} samples", sources = info["indexedSources"], samples = info["samples"]));
        let input = child.add(
            egui::TextEdit::singleline(&mut app.ui.visual_search_query)
                .hint_text(tl!("Describe a shot in English…"))
                .char_limit(1024)
                .desired_width(f32::INFINITY),
        );
        app.auto.add("text.visual.query", input.rect, "Describe a shot in English");
        let enter = input.lost_focus() && child.input(|i| i.key_pressed(egui::Key::Enter));
        let r = child.add_enabled(info["busy"] != true && !app.ui.visual_search_query.trim().is_empty(), egui::Button::new(tl!("Search visuals")));
        app.auto.add("text.visual.search", r.rect, "Search visuals");
        if r.clicked() || enter {
            command = Some(("visualSearch.search", json!({"query":app.ui.visual_search_query,"wait":false})));
        }
        child.small(tl!("Sampled visual similarity, not exact detection. Short events may be missed."));
    }
    if let Some(id) = app.visual_search_ui.job
        && let Some(job) = app.session.jobs.iter().find(|j| j.id == id)
    {
        let data = job.to_json();
        if data["finished"] != true {
            child.add(egui::ProgressBar::new(job.progress.fraction()).text(data["status"].as_str().unwrap_or_default()));
            let r = child.button(tl!("Cancel"));
            app.auto.add("text.visual.cancel", r.rect, "Cancel");
            if r.clicked() {
                command = Some(("jobs.cancel", json!({"job":id})));
            }
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(50));
        } else if let Some(error) = data["result"]["error"].as_str() {
            child.colored_label(app.tokens.danger, error);
        }
    }
    if let Some(warnings) = info["warnings"].as_array() {
        for warning in warnings.iter().take(3) {
            if let Some(w) = warning.as_str() {
                child.small(w);
            }
        }
    }
    let result = app.session.visual_search.result_for(&app.session.project);
    if let Some(result) = result {
        if !app.visual_search_ui.result.ptr_eq(&Arc::downgrade(&result)) {
            app.visual_search_ui.textures.clear();
            app.visual_search_ui.result = Arc::downgrade(&result);
        }
        if result.incomplete || result.stale_sources > 0 {
            child.label(tl!("Some sources are incomplete or changed. Reindex to refresh results."));
        }
        let height = child.available_height().max(0.0);
        egui::ScrollArea::vertical().max_height(height).show_rows(&mut child, 82.0, result.matches.len(), |ui, rows| {
            for index in rows {
                let Some(hit) = result.matches.get(index) else { continue };
                let row = ui.horizontal(|ui| {
                    if let Some(frame) = &hit.thumbnail {
                        let texture = app.visual_search_ui.textures.entry(index).or_insert_with(|| {
                            ui.ctx().load_texture(
                                format!("visual-result-{index}"),
                                egui::ColorImage::from_rgba_unmultiplied([frame.width as usize, frame.height as usize], &frame.to_rgba8()),
                                egui::TextureOptions::LINEAR,
                            )
                        });
                        ui.image((texture.id(), egui::vec2(96.0, 54.0)));
                    }
                    let rate = filmcraft_engine::clip_ops::source_view(&app.session, hit.item).map(|v| v.rate).unwrap_or_default();
                    let tc = |t| filmcraft_time::format_time(t, rate, false, filmcraft_time::TimeDisplay::Timecode, 48_000);
                    let r = ui.add_sized(
                        [ui.available_width(), 70.0],
                        egui::Button::new(format!(
                            "{}\n{} – {}\n{}",
                            hit.name,
                            tc(hit.start),
                            tc(hit.end),
                            tlf!("Similarity {score}", score = format!("{:.3}", hit.score))
                        ))
                        .wrap(),
                    );
                    app.auto.add(&format!("text.visual.result.{index}"), r.rect, &hit.name);
                    if r.clicked() {
                        command = Some(("visualSearch.openResult", json!({"result":index})));
                    }
                });
                let _ = row;
            }
        });
    }
    if app.visual_search_ui.confirm_download {
        egui::Window::new(tl!("Download visual search model")).collapsible(false).resizable(false).show(ui.ctx(), |ui| {
            ui.label(tl!("OpenCLIP ViT-B/32 (LAION-2B) · approximately 608 MB · MIT license"));
            ui.hyperlink_to(
                "LAION/OpenCLIP — model and license",
                "https://huggingface.co/laion/CLIP-ViT-B-32-laion2B-s34B-b79K/blob/1a25a446712ba5ee05982a381eed697ef9b435cf/README.md",
            );
            ui.label(tl!("The model downloads from Hugging Face. Your footage and searches are not uploaded."));
            let r = ui.button(tl!("Download"));
            app.auto.add("text.visual.download.confirm", r.rect, "Download");
            if r.clicked() {
                command = Some(("visualSearch.downloadModel", json!({"wait":false})));
                app.visual_search_ui.confirm_download = false;
            }
            if ui.button(tl!("Cancel")).clicked() {
                app.visual_search_ui.confirm_download = false;
            }
        });
    }
    if let Some((cmd, params)) = command {
        match app.session.execute(cmd, params) {
            Ok(result) => {
                if let Some(id) = result["job"].as_u64() {
                    app.visual_search_ui.job = Some(id);
                }
                if cmd == "visualSearch.openResult" {
                    app.show_panel(crate::dock::PanelKind::Source);
                }
            }
            Err(e) => app.ui.status = e.to_string(),
        }
    }
}

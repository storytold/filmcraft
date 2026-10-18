//! Project search runs on a snapshot in a cancellable worker, never on the redraw path.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak, mpsc};

use egui::Rect;
use filmcraft_engine::transcript_search::{self, Report};
use filmcraft_project::Project;
use serde_json::json;

use crate::FilmcraftApp;

type Answer = std::result::Result<Report, String>;
struct Worker {
    cancel: Arc<AtomicBool>,
    rx: mpsc::Receiver<Answer>,
}

#[derive(Default)]
pub(crate) struct Search {
    project: Weak<Project>,
    query: String,
    changed: Option<web_time::Instant>,
    worker: Option<Worker>,
    pub answer: Option<Arc<Answer>>,
}

impl Drop for Search {
    fn drop(&mut self) {
        if let Some(w) = &self.worker {
            w.cancel.store(true, Ordering::Relaxed);
        }
    }
}

impl Search {
    fn update(&mut self, project: &Arc<Project>, query: &str) -> bool {
        let project_key = Arc::downgrade(project);
        if !self.project.ptr_eq(&project_key) || self.query != query {
            self.project = project_key;
            self.query = query.to_owned();
            self.changed = Some(web_time::Instant::now());
            self.answer = None;
            if let Some(w) = &self.worker {
                w.cancel.store(true, Ordering::Relaxed);
            }
        }
        if let Some(w) = &self.worker {
            match w.rx.try_recv() {
                Ok(answer) => {
                    if !w.cancel.load(Ordering::Relaxed) {
                        self.answer = Some(Arc::new(answer));
                    }
                    self.worker = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    if !w.cancel.load(Ordering::Relaxed) {
                        self.answer = Some(Arc::new(Err("Search worker stopped".into())));
                    }
                    self.worker = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if self.worker.is_none() && self.changed.is_some_and(|at| at.elapsed().as_millis() >= 200) {
            self.changed = None;
            if !self.query.trim().is_empty() {
                let project = project.clone();
                let query = self.query.clone();
                let cancel = Arc::new(AtomicBool::new(false));
                let token = cancel.clone();
                let (tx, rx) = mpsc::channel();
                let task = move || {
                    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        transcript_search::search(&project, &query, 100, || !token.load(Ordering::Relaxed))
                    }))
                    .unwrap_or_else(|_| Err("Search failed; the project is unchanged".into()));
                    let _ = tx.send(result);
                };
                #[cfg(not(target_arch = "wasm32"))]
                match std::thread::Builder::new().name("transcript-search".into()).spawn(task) {
                    Ok(_) => self.worker = Some(Worker { cancel, rx }),
                    Err(e) => self.answer = Some(Arc::new(Err(e.to_string()))),
                }
                #[cfg(target_arch = "wasm32")]
                {
                    task();
                    self.worker = Some(Worker { cancel, rx });
                }
            }
        }
        self.worker.is_some() || self.changed.is_some()
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let mut header =
        ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + egui::vec2(10.0, 4.0), egui::vec2((rect.width() - 20.0).max(0.0), 28.0))));
    header.horizontal(|ui| {
        let r = ui.selectable_label(!app.ui.visual_search_mode, tl!("Transcripts"));
        app.auto.add("text.search.transcripts", r.rect, "Transcripts");
        if r.clicked() {
            app.ui.visual_search_mode = false;
        }
        let r = ui.selectable_label(app.ui.visual_search_mode, tl!("Visuals"));
        app.auto.add("text.search.visuals", r.rect, "Visuals");
        if r.clicked() {
            app.ui.visual_search_mode = true;
        }
    });
    let body = Rect::from_min_max(rect.min + egui::vec2(0.0, 32.0), rect.max);
    if app.ui.visual_search_mode { crate::panels::visual_search::show(app, ui, body) } else { transcripts(app, ui, body) }
}

fn transcripts(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(10.0)).id_salt("project-transcript-search"));
    let response = child.add(
        egui::TextEdit::singleline(&mut app.ui.project_transcript_query)
            .hint_text(tl!("Search all source transcripts"))
            .char_limit(1024)
            .desired_width(f32::INFINITY),
    );
    app.auto.add("text.search.query", response.rect, "Search all source transcripts");
    let busy = app.media_search.update(&app.session.project, &app.ui.project_transcript_query);
    if busy {
        child.label(tl!("Searching…"));
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(30));
    }
    let answer = app.media_search.answer.clone();
    let mut open = None;
    match answer.as_deref() {
        None if !busy => {
            child.label(tl!("Find spoken words in any transcribed source, including unused clips."));
        }
        Some(Err(error)) => {
            child.colored_label(app.tokens.danger, error);
        }
        Some(Ok(report)) => {
            child.label(tlf!(
                "{count} results · {sources} transcribed · {missing} not transcribed",
                count = report.matches.len(),
                sources = report.transcribed_sources,
                missing = report.untranscribed_sources
            ));
            if report.incomplete {
                child.label(tl!("Search limit reached. Refine your query."));
            }
            let height = child.available_height().max(0.0);
            egui::ScrollArea::vertical().max_height(height).show_rows(&mut child, 66.0, report.matches.len(), |ui, rows| {
                for index in rows {
                    let Some(hit) = report.matches.get(index) else { continue };
                    let rate = filmcraft_engine::clip_ops::source_view(&app.session, hit.item).map(|v| v.rate).unwrap_or_default();
                    let tc = |t| filmcraft_time::format_time(t, rate, false, filmcraft_time::TimeDisplay::Timecode, 48_000);
                    let r = ui.add_sized(
                        [ui.available_width(), 62.0],
                        egui::Button::new(format!("{} · {} – {}\n{}", hit.name, tc(hit.start), tc(hit.end), hit.context)).wrap(),
                    );
                    app.auto.add(&format!("text.search.result.{index}"), r.rect, &hit.text);
                    if r.clicked() {
                        open = Some(json!({"item":hit.item.0,"from":hit.from,"to":hit.to}));
                    }
                }
            });
        }
        _ => {}
    }
    if let Some(params) = open {
        match app.session.execute("transcript.openSearchResult", params) {
            Ok(_) => app.show_panel(crate::dock::PanelKind::Source),
            Err(e) => app.ui.status = e.to_string(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_engine::Session;

    fn finish(search: &mut Search, project: &Arc<Project>, query: &str) {
        search.update(project, query);
        search.changed = Some(web_time::Instant::now() - std::time::Duration::from_millis(250));
        for _ in 0..100 {
            if !search.update(project, query) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        panic!("search did not finish");
    }

    #[test]
    fn cached_results_survive_playhead_and_refresh_after_undo_and_replacement() {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let item = s.active_sequence().unwrap().audio_tracks[0].items[0].item;
        s.execute("transcript.set", json!({"item":item.0,"transcript":{"words":[{"text":"hello","start":0,"end":100}]}})).unwrap();
        let mut cache = Search::default();
        finish(&mut cache, &s.project, "hello");
        let answer = cache.answer.clone().unwrap();
        assert_eq!(answer.as_ref().as_ref().unwrap().matches.len(), 1);
        s.set_playhead(filmcraft_time::Tick(123));
        assert!(!cache.update(&s.project, "hello"));
        assert!(Arc::ptr_eq(&answer, cache.answer.as_ref().unwrap()));
        s.execute("transcript.delete", json!({"items":[item.0]})).unwrap();
        finish(&mut cache, &s.project, "hello");
        assert!(cache.answer.as_ref().unwrap().as_ref().as_ref().unwrap().matches.is_empty());
        s.undo();
        finish(&mut cache, &s.project, "hello");
        assert_eq!(cache.answer.as_ref().unwrap().as_ref().as_ref().unwrap().matches.len(), 1);
        s.execute("file.openDemoProject", json!({})).unwrap();
        finish(&mut cache, &s.project, "hello");
        assert!(cache.answer.as_ref().unwrap().as_ref().as_ref().unwrap().matches.is_empty());
    }
}

//! Source-local speaker management. Drafts are transient; Apply is one guarded engine edit.
use std::collections::BTreeMap;
use std::sync::Arc;

use filmcraft_project::{ItemId, Transcript};
use filmcraft_time::Tick;
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::widgets::CenteredDialog as _;

#[derive(Clone, Debug, PartialEq)]
struct Source {
    item: ItemId,
    title: String,
    original: Arc<Transcript>,
    names: Vec<String>,
    passages: BTreeMap<usize, usize>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Draft {
    epoch: u64,
    source: bool,
    target: ItemId,
    sources: Vec<Source>,
    detection: bool,
    error: String,
    preview: Option<(ItemId, bool)>,
}

impl Draft {
    fn changed(&self) -> bool {
        self.sources.iter().any(|s| s.names.iter().zip(&s.original.speakers).any(|(name, old)| name.trim() != old.name))
    }
    fn current(&self, app: &FilmcraftApp) -> bool {
        self.epoch == app.session.document_epoch
            && self.source == app.ui.transcript_source
            && Some(self.target) == if self.source { app.session.state.source_item } else { app.session.state.active_sequence }
    }
    fn stale(&self, app: &FilmcraftApp) -> bool {
        self.sources.iter().any(|s| app.session.project.transcripts.get(&s.item).is_none_or(|t| !Arc::ptr_eq(t, &s.original) && **t != *s.original))
    }
    fn params(&self) -> Value {
        json!({"sources": self.sources.iter().filter_map(|s| {
            let names: Vec<_> = s.names.iter().zip(&s.original.speakers).enumerate()
                .filter(|(_, (name, old))| name.trim() != old.name)
                .map(|(index, (name, _))| json!({"speaker":index,"name":name})).collect();
            (!names.is_empty()).then(||json!({"item":s.item.0,"expected":s.original,"names":names}))
        }).collect::<Vec<_>>()})
    }
    fn stop_preview(&mut self, app: &mut FilmcraftApp) {
        if let Some((item, looping)) = self.preview.take() {
            if app.session.state.source_item == Some(item) {
                app.stop_source();
            }
            app.source_playback.clock.looping = looping;
        }
    }
}

fn open(app: &mut FilmcraftApp) -> Result<(), String> {
    if let Some(mut old) = app.ui.speaker_dialog.take() {
        old.stop_preview(app);
    }
    let source = app.ui.transcript_source;
    let target = if source { app.session.state.source_item } else { app.session.state.active_sequence }
        .ok_or_else(|| tl!("Open a sequence or Source clip to see its transcript").to_string())?;
    let content = app.transcript_cache.content(&app.session, source);
    let mut items: BTreeMap<ItemId, BTreeMap<usize, usize>> = BTreeMap::new();
    if source {
        let root = app.session.project.resolve_media(target).map(|(root, _, _)| root).ok_or("Source is unavailable")?;
        items.entry(root).or_default();
    }
    for word in &content.words {
        let first = items.entry(word.item).or_default();
        if let Some(index) = app.session.project.transcripts.get(&word.item).and_then(|tr| tr.words.get(word.index)).and_then(|w| w.speaker) {
            first.entry(index as usize).or_insert(word.index);
        }
    }
    if items.len() > 256 {
        return Err(tl!("Too many speakers to show. Open one Source clip.").into());
    }
    let mut sources = Vec::new();
    let mut rows = 0usize;
    for (item, passages) in items {
        let Some(original) = app.session.project.transcripts.get(&item).filter(|t| !t.words.is_empty()).cloned() else { continue };
        rows = rows.saturating_add(original.speakers.len());
        if rows > 4096 {
            return Err(tl!("Too many speakers to show. Open one Source clip.").into());
        }
        let title = app.session.project.item(item).map(|it| it.name.chars().take(256).collect()).unwrap_or_default();
        let names = original.speakers.iter().map(|sp| sp.name.clone()).collect();
        sources.push(Source { item, title, original, names, passages });
    }
    app.ui.speaker_dialog = Some(Draft { epoch: app.session.document_epoch, source, target, sources, detection: false, error: String::new(), preview: None });
    Ok(())
}

/// Frontend commands for opening the manager and auditioning a source-local identity.
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    match id {
        "textPanel.manageSpeakers" => Some(open(app).map(|_| json!({"dialog":"speakers"}))),
        "textPanel.previewSpeaker" => Some(preview(app, params)),
        _ => None,
    }
}

fn preview(app: &mut FilmcraftApp, params: &Value) -> Result<Value, String> {
    let item = params.get("item").and_then(Value::as_u64).map(ItemId).ok_or("Source is required")?;
    let speaker = params.get("speaker").and_then(Value::as_u64).and_then(|v| u32::try_from(v).ok()).ok_or("Speaker is required")?;
    let index = params.get("index").and_then(Value::as_u64).and_then(|v| usize::try_from(v).ok()).ok_or("Passage is required")?;
    let view = filmcraft_engine::clip_ops::source_view(&app.session, item).ok_or("Source is unavailable")?;
    let tr = app.session.project.transcripts.get(&view.media).ok_or("Source has no transcript")?;
    let first = tr
        .words
        .get(index)
        .filter(|w| w.speaker == Some(speaker) && w.end > view.start && w.start < view.end)
        .ok_or("Speaker passage changed; reopen Speakers")?;
    let start = first.start.max(view.start);
    let cap = Tick(start.0.saturating_add(8 * filmcraft_time::TICKS_PER_SECOND)).min(view.end);
    let mut end = first.end.min(cap);
    for word in tr.words.iter().skip(index + 1) {
        if word.speaker != Some(speaker) || word.start.0.saturating_sub(end.0) > filmcraft_time::TICKS_PER_SECOND || word.start >= cap {
            break;
        }
        end = word.end.min(cap);
    }
    if end <= start {
        return Err("Speaker passage has no playable duration".into());
    }
    app.session.execute("source.open", json!({"item":item.0})).map_err(|e| e.to_string())?;
    app.preview_source_passage(start, end)?;
    app.show_panel(crate::dock::PanelKind::Source);
    Ok(json!({"item":item.0,"start":start.0,"end":end.0}))
}

fn error_text(error: filmcraft_engine::EngineError) -> String {
    match error {
        filmcraft_engine::EngineError::BadParams { msg, .. } => msg,
        other => other.to_string(),
    }
}

pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut d) = app.ui.speaker_dialog.take() else { return };
    if !d.current(app) {
        d.stop_preview(app);
        return;
    }
    if d.preview.is_some() && !app.source_playback.clock.playing {
        d.stop_preview(app);
    }
    let stale = d.stale(app);
    let changed = d.changed();
    let mut cancel = false;
    let mut apply = false;
    let mut identify = false;
    let mut listen = None;
    let title = if d.detection { tl!("Identify Speakers") } else { tl!("Speakers") };
    egui::Window::new(title).id(egui::Id::unique("speaker-manager")).centered(ctx).collapsible(false).resizable(false).default_width(500.0).show(ctx, |ui| {
        ui.set_width(500.0);
        if d.detection {
            ui.horizontal(|ui| {
                ui.label(tl!("Maximum speakers"));
                let response = ui.add(egui::DragValue::new(&mut app.ui.transcript_speaker_limit).range(1..=32));
                app.auto.add("speakers.limit", response.rect, tl!("Maximum speakers"));
            });
            ui.add_space(8.0);
            ui.label(tl!("Replaces speaker names and assignments. Undo is available."));
        } else {
            ui.label(egui::RichText::new(tl!("Names are specific to each source clip.")).small().color(app.tokens.text_dim));
            if d.sources.iter().all(|s| s.names.is_empty()) {
                ui.add_space(12.0);
                ui.label(tl!("No speakers identified yet."));
            }
            let height = (ctx.content_rect().height() - 270.0).clamp(80.0, 340.0);
            egui::ScrollArea::vertical().max_height(height).id_salt("speaker-manager-list").show(ui, |ui| {
                for source in &mut d.sources {
                    ui.add_space(8.0);
                    ui.add(egui::Label::new(egui::RichText::new(&source.title).strong()).truncate());
                    for (index, name) in source.names.iter_mut().enumerate() {
                        let key = format!("speakers.{}.{}", source.item.0, index);
                        ui.horizontal(|ui| {
                            ui.add_sized([86.0, 26.0], egui::Label::new(format!("{} {}", tl!("Speaker"), index + 1)));
                            let field = crate::widgets::accessible_name(
                                ui.add_sized([300.0, 26.0], egui::TextEdit::singleline(name).char_limit(256)),
                                tl!("Speaker name"),
                            );
                            app.auto.add(&format!("{key}.name"), field.rect, tl!("Speaker name"));
                            let button = ui.add_enabled(!stale && source.passages.contains_key(&index), egui::Button::new(tl!("Preview")));
                            app.auto.add(&format!("{key}.preview"), button.rect, tl!("Preview"));
                            if button.clicked() {
                                listen = source.passages.get(&index).copied().map(|word| (source.item, index, word));
                            }
                        });
                    }
                }
            });
        }
        if stale {
            ui.colored_label(app.tokens.danger, tl!("Transcript changed. Close and reopen Speakers."));
        }
        if !d.error.is_empty() {
            ui.colored_label(app.tokens.danger, &d.error);
        }
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        if d.detection {
            crate::widgets::centered_buttons(ui, &[tl!("Back"), tl!("Cancel"), tl!("Identify Speakers")], |ui| {
                let back = ui.button(tl!("Back"));
                app.auto.add("speakers.back", back.rect, tl!("Back"));
                if back.clicked() {
                    d.detection = false;
                }
                let close = ui.button(tl!("Cancel"));
                app.auto.add("speakers.cancel", close.rect, tl!("Cancel"));
                cancel = close.clicked();
                let enabled = !stale && !d.sources.is_empty() && app.session.transcribe_jobs.is_empty();
                let button = ui
                    .add_enabled(enabled, egui::Button::new(egui::RichText::new(tl!("Identify Speakers")).color(egui::Color32::WHITE)).fill(app.tokens.accent));
                app.auto.add("speakers.identify", button.rect, tl!("Identify Speakers"));
                identify = button.clicked();
            });
        } else {
            crate::widgets::centered_buttons(ui, &[tl!("Identify Speakers…"), tl!("Cancel"), tl!("Apply")], |ui| {
                let detect = ui.add_enabled(
                    !stale && !changed && !d.sources.is_empty() && app.session.transcribe_jobs.is_empty(),
                    egui::Button::new(tl!("Identify Speakers…")),
                );
                app.auto.add("speakers.detect", detect.rect, tl!("Identify Speakers…"));
                if changed {
                    detect.clone().on_hover_text(tl!("Apply name changes before identifying speakers."));
                }
                if detect.clicked() {
                    d.detection = true;
                    d.error.clear();
                }
                let close = ui.button(tl!("Cancel"));
                app.auto.add("speakers.cancel", close.rect, tl!("Cancel"));
                cancel = close.clicked();
                let valid = d
                    .sources
                    .iter()
                    .flat_map(|s| s.names.iter().zip(&s.original.speakers))
                    .filter(|(n, old)| n.trim() != old.name)
                    .all(|(n, _)| !n.trim().is_empty() && n.trim().len() <= 256 && !n.chars().any(char::is_control));
                let button = ui.add_enabled(
                    changed && valid && !stale,
                    egui::Button::new(egui::RichText::new(tl!("Apply")).color(egui::Color32::WHITE)).fill(app.tokens.accent),
                );
                app.auto.add("speakers.apply", button.rect, tl!("Apply"));
                apply = button.clicked();
            });
        }
        app.auto.add("speakers.bounds", ui.min_rect(), title);
    });
    if let Some((root, speaker, index)) = listen {
        d.stop_preview(app);
        let item = if d.source { d.target } else { root };
        let looping = app.source_playback.clock.looping;
        match crate::menus::invoke(app, ctx, "textPanel.previewSpeaker", json!({"item":item.0,"speaker":speaker,"index":index})) {
            Ok(_) => {
                d.error.clear();
                d.preview = Some((item, looping));
            }
            Err(error) => d.error = error,
        }
    }
    if apply {
        match app.session.execute("transcript.renameSpeakers", d.params()) {
            Ok(_) => cancel = true,
            Err(error) => d.error = error_text(error),
        }
    }
    if identify {
        d.stop_preview(app);
        match app.session.execute(
            "transcript.identifySpeakers",
            json!({"items":d.sources.iter().map(|s|s.item.0).collect::<Vec<_>>(),"maxSpeakers":app.ui.transcript_speaker_limit,"wait":false}),
        ) {
            Ok(_) => cancel = true,
            Err(error) => d.error = error_text(error),
        }
    }
    if cancel || crate::widgets::escape_closes(ctx) {
        d.stop_preview(app);
    } else {
        app.ui.speaker_dialog = Some(d);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn switching_source_during_speaker_preview_restores_loop_without_stopping_the_new_source() {
        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let first = session.active_sequence().unwrap().audio_tracks[0].items[0].item;
        let second = session.active_sequence().unwrap().audio_tracks[0].items.iter().find(|clip| clip.item != first).unwrap().item;
        session.execute("source.open", json!({"item":first.0})).unwrap();
        let mut app = FilmcraftApp::new(session);
        app.ui.transcript_source = true;
        open(&mut app).unwrap();
        app.ui.speaker_dialog.as_mut().unwrap().preview = Some((first, true));
        app.source_playback.clock.looping = false;
        app.session.execute("source.open", json!({"item":second.0})).unwrap();
        app.source_playback.clock.playing = true;
        let before = app.session.project.clone();
        let undo = app.session.history.undo.len();
        show(&mut app, &egui::Context::default());
        assert!(app.ui.speaker_dialog.is_none());
        assert!(app.source_playback.clock.looping, "the preview's temporary loop setting must be restored");
        assert!(app.source_playback.clock.playing, "closing the old preview must not stop another source");
        assert_eq!(app.session.state.source_item, Some(second));
        assert_eq!(app.session.project, before);
        assert_eq!(app.session.history.undo.len(), undo);
    }

    #[test]
    fn speaker_preview_validates_hostile_input_and_respects_a_subclip() {
        let mut session = filmcraft_engine::Session::default();
        session.execute("file.openDemoProject", json!({})).unwrap();
        let root = session.active_sequence().unwrap().audio_tracks[0].items[0].item;
        let t = filmcraft_time::TICKS_PER_SECOND;
        session
            .execute(
                "transcript.set",
                json!({"item":root.0,"transcript":{"words":[
            {"text":"Outside","start":0,"end":t,"speaker":0},
            {"text":"Inside","start":2*t,"end":9*t,"speaker":0}]}}),
            )
            .unwrap();
        let sub = session
            .edit("Subclip", |project, _| {
                let id = ItemId(project.alloc_id());
                let mut item = project.item(root).unwrap().clone();
                item.id = id;
                item.kind = filmcraft_project::ItemKind::Subclip {
                    parent: root,
                    range: filmcraft_time::TimeRange::from_bounds(Tick(2 * t), Tick(4 * t)),
                    restrict_trims: true,
                };
                project.items.insert(id, item);
                Ok(id)
            })
            .unwrap();
        session.execute("source.open", json!({"item":sub.0})).unwrap();
        let mut app = FilmcraftApp::new(session);
        app.ui.transcript_source = true;
        open(&mut app).unwrap();
        let draft = app.ui.speaker_dialog.as_ref().unwrap();
        assert_eq!(draft.target, sub);
        assert_eq!(draft.sources[0].passages[&0], 1, "subclip preview skips a passage outside its range");
        let before = app.session.project.clone();
        let undo = app.session.history.undo.len();
        for params in [
            json!({}),
            json!({"item":-1,"speaker":0,"index":1}),
            json!({"item":sub.0,"speaker":-1,"index":1}),
            json!({"item":sub.0,"speaker":0,"index":u64::MAX}),
            json!({"item":sub.0,"speaker":0,"index":0}),
            json!({"item":sub.0,"speaker":0,"index":0.5}),
        ] {
            assert!(preview(&mut app, &params).is_err());
            assert_eq!(app.session.project, before);
            assert_eq!(app.session.state.source_item, Some(sub));
        }
        let result = preview(&mut app, &json!({"item":sub.0,"speaker":0,"index":1})).unwrap();
        assert_eq!(result["start"], 2 * t);
        assert_eq!(result["end"], 4 * t);
        assert_eq!(app.source_playback.clock.stop_at, Some(Tick(4 * t)));
        assert_eq!(app.session.state.source_item, Some(sub));
        assert_eq!(app.session.project, before);
        assert_eq!(app.session.history.undo.len(), undo);
        app.stop_source();
    }
}

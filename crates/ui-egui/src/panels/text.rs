//! The Text panel: Transcript / Captions / Graphics tabs. The Captions tab lists the caption
//! segments of a caption track with editable in/out timecodes and text, a toolbar (add, split,
//! merge, delete), track choice and the track style. Every edit dispatches a `captions.*` engine
//! command; every widget registers an automation id (`text.*`). The Transcript tab shows the
//! sequence transcript as speaker paragraphs of clickable words (click, Shift+click to extend);
//! the selection marks In/Out and can be extracted or lifted (`transcript.*` commands).

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::{CaptionAlign, CaptionAnchor, CaptionFormat};
use filmcraft_time::{TimeDisplay, format_time};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

const TABS: [&str; 3] = ["Transcript", "Captions", "Graphics"];

/// The width a tab label needs: 7 px per character, which the strip was laid out with for Latin
/// text, or the label as drawn if that is wider (Japanese glyphs are about twice as wide, and a
/// per-character estimate alone let a label run into the next tab). Measured in the semibold the
/// active tab uses, so selecting a tab does not move its neighbours.
fn tab_text_width(painter: &egui::Painter, label: &str) -> f32 {
    let drawn = painter.layout_no_wrap(label.to_string(), Tokens::semibold(12.5), Color32::WHITE).size().x;
    (label.chars().count() as f32 * 7.0).max(drawn)
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    // tabs
    let mut x = rect.min.x + 12.0;
    for tab in TABS {
        let shown = crate::i18n::t(tab);
        let text_w = tab_text_width(ui.painter(), shown);
        let w = text_w + 16.0;
        let r = Rect::from_min_size(pos2(x, rect.min.y + 4.0), vec2(w, 24.0));
        let resp = ui.interact(r, egui::Id::new(("text-tab", tab)), Sense::click());
        let active = app.ui.text_tab == tab;
        ui.painter().text(
            pos2(r.min.x, r.center().y),
            Align2::LEFT_CENTER,
            shown,
            if active { Tokens::semibold(12.5) } else { Tokens::ui(12.5) },
            if active { t.text } else { t.text_dim },
        );
        if active {
            ui.painter().line_segment([pos2(r.min.x, r.max.y), pos2(r.min.x + text_w, r.max.y)], Stroke::new(2.0, t.text));
        }
        app.auto.add(&format!("text.tab.{tab}"), r, tab);
        if resp.clicked() {
            app.ui.text_tab = tab.to_string();
        }
        x += w + 6.0;
    }
    let body = Rect::from_min_max(pos2(rect.min.x, rect.min.y + 34.0), rect.max);
    match app.ui.text_tab.as_str() {
        "Captions" => captions(app, ui, body),
        "Transcript" => transcript(app, ui, body),
        _ => crate::dock::placeholder(ui, body, &t, tl!("Graphics text search arrives with M10.1–M10.2")),
    }
}

fn tool_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, icon: Icon, id: &str, label: &str, enabled: bool) -> bool {
    let t = app.tokens;
    let resp = ui.interact(r, egui::Id::new(("text-tool", id)), if enabled { Sense::click() } else { Sense::hover() });
    if enabled && resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    }
    icons::paint(ui.painter(), r.shrink(5.0), icon, if enabled { t.icon } else { t.text_faint });
    app.auto.add(id, r, label);
    resp.on_hover_text(label).clicked() && enabled
}

fn captions(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let Some(seq) = app.session.active_sequence().cloned() else {
        crate::dock::placeholder(ui, rect, &t, tl!("Open a sequence to work with captions"));
        return;
    };
    let mut actions: Vec<(String, Value)> = Vec::new();
    if seq.caption_tracks.is_empty() {
        let c = rect.center();
        icons::paint(ui.painter(), Rect::from_center_size(c - vec2(0.0, 70.0), vec2(40.0, 40.0)), Icon::Captions, t.text_dim);
        ui.painter().text(c - vec2(0.0, 30.0), Align2::CENTER_CENTER, tl!("Add captions"), Tokens::semibold(16.0), t.text);
        ui.painter().text(c - vec2(0.0, 8.0), Align2::CENTER_CENTER, tl!("Create a caption track or import a caption file."), Tokens::ui(12.0), t.text_dim);
        for (i, (id, label, cmd)) in [
            ("text.captions.newTrack", tl!("Create new caption track"), "captions.newTrack"),
            ("text.captions.import", tl!("Import captions file…"), "captions.import"),
        ]
        .into_iter()
        .enumerate()
        {
            let r = Rect::from_center_size(c + vec2(0.0, 26.0 + i as f32 * 34.0), vec2(200.0, 26.0));
            let resp = ui.interact(r, egui::Id::new(id), Sense::click());
            ui.painter().rect_filled(r, 13.0, if i == 0 { if resp.hovered() { t.accent_hover } else { t.accent } } else { t.field_bg });
            ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::semibold(12.0), Color32::WHITE);
            app.auto.add(id, r, label);
            if resp.clicked() {
                actions.push((cmd.into(), json!({})));
            }
        }
        run(app, ui, actions);
        return;
    }
    let rate = seq.settings.frame_rate;
    let df = seq.settings.drop_frame;
    let tc = |x: filmcraft_time::Tick| format_time(x, rate, df, TimeDisplay::Timecode, 48_000);
    // the track shown: the one holding the first selected caption, else C1
    let sel = app.session.state.caption_selection.clone();
    let track_idx = sel.first().and_then(|c| seq.caption_tracks.iter().position(|tr| tr.caption(*c).is_some())).unwrap_or(0);
    let track_idx = ui.ctx().data(|d| d.get_temp::<usize>(egui::Id::new("text-cap-track"))).filter(|i| *i < seq.caption_tracks.len()).unwrap_or(track_idx);
    let track = &seq.caption_tracks[track_idx];

    // ---- toolbar: search, track picker, add / split / merge / delete
    let bar = Rect::from_min_size(rect.min + vec2(10.0, 2.0), vec2(rect.width() - 20.0, 26.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(bar.min, vec2(170.0f32.min(bar.width() * 0.4), 24.0))));
    let sresp = crate::widgets::search_field(&mut child, &mut app.ui.caption_search, tl!("Search"), 170.0f32.min(bar.width() * 0.4), &t);
    app.auto.add("text.captions.search", sresp.rect, "Search captions");
    let mut x = bar.min.x + 180.0f32.min(bar.width() * 0.4 + 10.0);
    let picker = Rect::from_min_size(pos2(x, bar.min.y + 1.0), vec2(130.0, 22.0));
    let label = format!("C{} · {}", track_idx + 1, track.name);
    let presp = crate::widgets::dropdown_text(ui, picker, &label, &t, egui::Id::new("text-cap-track-picker"));
    app.auto.add("text.captions.track", picker, "Caption track");
    egui::Popup::menu(&presp).show(|ui| {
        for (i, tr) in seq.caption_tracks.iter().enumerate() {
            if ui.selectable_label(i == track_idx, format!("C{} · {} ({})", i + 1, tr.name, crate::i18n::t(tr.format.label()))).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("text-cap-track"), i));
            }
        }
        ui.separator();
        for f in CaptionFormat::ALL {
            if ui.button(tlf!("New {format} track", format = crate::i18n::t(f.label()))).clicked() {
                actions.push(("captions.newTrack".into(), json!({"format": f.label()})));
            }
        }
    });
    x = picker.max.x + 8.0;
    let ph = app.session.playhead();
    let any_sel = !sel.is_empty();
    let under = track.caption_at(ph).filter(|c| c.start < ph).map(|c| c.id);
    let tools: [(Icon, &str, &str, bool, &str, Value); 5] = [
        (Icon::Plus, "text.captions.add", tl!("Add caption at playhead"), track.caption_at(ph).is_none(), "captions.add", json!({"track": track.id.0})),
        (
            Icon::Razor,
            "text.captions.split",
            tl!("Split caption at playhead"),
            under.is_some(),
            "captions.split",
            json!({"caption": under.map(|c| c.0), "time": ph.0}),
        ),
        (Icon::Link, "text.captions.merge", tl!("Merge selected captions"), sel.len() > 1, "captions.merge", json!({})),
        (Icon::Trash, "text.captions.delete", tl!("Delete selected captions"), any_sel, "captions.delete", json!({})),
        (Icon::Export, "text.captions.export", "Export captions…", !track.captions.is_empty(), "captions.export", json!({"track": track.id.0})),
    ];
    for (icon, id, label, enabled, cmd, params) in tools {
        let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(24.0, 24.0));
        if r.max.x > rect.max.x {
            break;
        }
        if tool_button(app, ui, r, icon, id, label, enabled) {
            actions.push((cmd.into(), params));
        }
        x += 28.0;
    }

    // ---- style strip
    let style_h = 30.0;
    let style_rect = Rect::from_min_max(pos2(rect.min.x + 10.0, rect.max.y - style_h), pos2(rect.max.x - 10.0, rect.max.y - 2.0));
    style_strip(app, ui, style_rect, track_idx, &mut actions);

    // ---- segment list
    let list = Rect::from_min_max(pos2(rect.min.x + 6.0, bar.max.y + 8.0), pos2(rect.max.x - 6.0, style_rect.min.y - 6.0));
    ui.painter().rect_filled(list, 3.0, t.app_bg);
    let q = app.ui.caption_search.to_lowercase();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list.shrink(4.0)).id_salt("caption-list"));
    let current = track.caption_at(ph).map(|c| c.id);
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("caption-scroll").show(&mut child, |ui| {
        ui.set_width(list.width() - 12.0);
        let mut shown = 0;
        for (n, c) in track.captions.iter().enumerate() {
            if !q.is_empty() && !c.text.to_lowercase().contains(&q) && !c.speaker.as_deref().unwrap_or("").to_lowercase().contains(&q) {
                continue;
            }
            shown += 1;
            let selected = sel.contains(&c.id);
            let fill = if selected {
                t.row_selected
            } else if n % 2 == 1 {
                t.row_alt
            } else {
                Color32::TRANSPARENT
            };
            let frame = egui::Frame::NONE.fill(fill).inner_margin(egui::Margin::symmetric(6, 4)).corner_radius(3.0);
            let fr = frame.show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(92.0);
                        let num = ui.add(egui::Button::new(egui::RichText::new(format!("{}", n + 1)).size(11.0).color(t.text_dim)).frame(false));
                        app.auto.add(&format!("text.captions.{}.goto", c.id.0), num.rect, "Go to caption");
                        if num.clicked() {
                            if ui.input(|i| i.modifiers.command || i.modifiers.shift) {
                                actions.push(("captions.select".into(), json!({"captions": [c.id.0], "add": true})));
                            } else {
                                actions.push(("captions.goTo".into(), json!({"caption": c.id.0})));
                            }
                        }
                        for (edge, val) in [("in", c.start), ("out", c.end())] {
                            let key = egui::Id::new(("cap-tc", c.id.0, edge));
                            let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| tc(val));
                            let resp = ui.add(
                                egui::TextEdit::singleline(&mut buf)
                                    .id(key.with("te"))
                                    .desired_width(88.0)
                                    .font(Tokens::mono(11.0))
                                    .text_color(if edge == "in" { t.hot_text } else { t.text_dim }),
                            );
                            app.auto.add(&format!("text.captions.{}.{edge}", c.id.0), resp.rect, if edge == "in" { "Caption in" } else { "Caption out" });
                            if resp.has_focus() {
                                ui.data_mut(|d| d.insert_temp(key, buf.clone()));
                            } else {
                                ui.data_mut(|d| d.remove::<String>(key));
                            }
                            if resp.lost_focus() && buf.trim() != tc(val) {
                                let k = if edge == "in" { "startTimecode" } else { "endTimecode" };
                                actions.push(("captions.setTimes".into(), json!({"caption": c.id.0, k: buf.trim()})));
                            }
                        }
                    });
                    ui.vertical(|ui| {
                        if let Some(sp) = &c.speaker {
                            ui.label(egui::RichText::new(sp).size(11.0).color(t.text_dim).strong());
                        }
                        let key = egui::Id::new(("cap-text", c.id.0));
                        let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| c.text.clone());
                        let resp = ui.add(
                            egui::TextEdit::multiline(&mut buf)
                                .id(key.with("te"))
                                .desired_rows(1)
                                .desired_width(ui.available_width())
                                .font(Tokens::ui(12.5))
                                .frame(egui::Frame::NONE),
                        );
                        app.auto.add(&format!("text.captions.{}.text", c.id.0), resp.rect, "Caption text");
                        if resp.has_focus() {
                            ui.data_mut(|d| d.insert_temp(key, buf.clone()));
                            if !selected {
                                actions.push(("captions.select".into(), json!({"captions": [c.id.0]})));
                            }
                        } else {
                            ui.data_mut(|d| d.remove::<String>(key));
                        }
                        if resp.lost_focus() && buf != c.text {
                            actions.push(("captions.setText".into(), json!({"caption": c.id.0, "text": buf})));
                        }
                    });
                });
            });
            let row = fr.response.rect;
            if current == Some(c.id) {
                ui.painter().rect_stroke(row, 3.0, Stroke::new(1.0, t.accent), StrokeKind::Inside);
            }
            app.auto.add(&format!("text.captions.{}.row", c.id.0), row, &c.text);
            ui.add_space(2.0);
        }
        if shown == 0 {
            ui.label(
                egui::RichText::new(if q.is_empty() {
                    tl!("No captions on this track. Press + to add one at the playhead.")
                } else {
                    tl!("No matching captions.")
                })
                .color(t.text_faint),
            );
        }
    });
    run(app, ui, actions);
}

fn transcript(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut header = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + vec2(10.0, 0.0), vec2(rect.width() - 20.0, 28.0))));
    header.horizontal(|ui| {
        for (source, label, id) in [(false, tl!("Sequence"), "sequence"), (true, tl!("Source"), "source")] {
            let resp = ui.selectable_label(app.ui.transcript_source == source, label);
            app.auto.add(&format!("text.transcript.view.{id}"), resp.rect, label);
            if resp.clicked() {
                app.ui.transcript_source = source;
                app.ui.transcript_sel = None;
                app.ui.transcript_edit = None;
            }
        }
        let menu = ui.menu_button(tl!("Speech models"), |ui| {
            if let Ok(models) = app.session.execute("transcript.models", json!({}))
                && let Some(models) = models["models"].as_array()
            {
                for model in models {
                    let Some(id) = model["id"].as_str() else { continue };
                    ui.label(format!(
                        "{} — {:.0} MB — {}",
                        model["name"].as_str().unwrap_or(id),
                        model["size"].as_u64().unwrap_or(0) as f64 / 1_000_000.0,
                        model["license"].as_str().unwrap_or("")
                    ));
                    if let Some(url) = model["source"].as_str() {
                        ui.hyperlink_to(tl!("Source"), url);
                    }
                    let installed = model["installed"].as_bool().unwrap_or(false);
                    let button = ui.add_enabled(
                        !installed && app.session.is_enabled("transcript.downloadModel"),
                        egui::Button::new(if installed { tl!("Installed") } else { tl!("Download model") }),
                    );
                    app.auto.add(&format!("text.transcript.download.{id}"), button.rect, "Download model");
                    if button.clicked() {
                        actions.push(("transcript.downloadModel".into(), json!({"model": id, "wait": false})));
                        ui.close();
                    }
                    let selected = app.session.prefs.media_analysis.whisper_model == id;
                    let select = ui.add_enabled(installed, egui::Button::new(tl!("Use model")).selected(selected));
                    app.auto.add(&format!("text.transcript.model.{id}"), select.rect, "Use model");
                    if select.clicked() {
                        actions.push(("prefs.set".into(), json!({"key": "mediaAnalysis.whisperModel", "value": id})));
                        ui.close();
                    }
                    ui.separator();
                }
            }
        });
        app.auto.add("text.transcript.models", menu.response.rect, "Speech models");
    });
    let rect = Rect::from_min_max(rect.min + vec2(0.0, 30.0), rect.max);
    let source = app.ui.transcript_source;
    let words = if source { filmcraft_engine::transcript::source_words(&app.session) } else { filmcraft_engine::transcript::sequence_words(&app.session) };
    // Keep progress visible even while the transcript is empty or the user switches monitors.
    if let Some(job) = app
        .session
        .jobs
        .iter()
        .rev()
        .find(|j| matches!(j.label.as_str(), "Transcribe" | "Download Speech Model") && !j.to_json()["finished"].as_bool().unwrap_or(false))
        .cloned()
    {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect));
        child.vertical(|ui| {
            ui.label(job.label.clone());
            let status = job.progress.status.lock().unwrap_or_else(|e| e.into_inner()).clone();
            let progress = ui.add(egui::ProgressBar::new(job.progress.fraction()).text(status));
            app.auto.add("text.transcript.progress", progress.rect, "Transcription progress");
            let cancel = ui.button(tl!("Cancel"));
            app.auto.add("text.transcript.cancel", cancel.rect, "Cancel transcription");
            if cancel.clicked() {
                actions.push(("jobs.cancel".into(), json!({"job": job.id})));
            }
        });
        run(app, ui, actions);
        ui.ctx().request_repaint();
        return;
    }
    if (!source && app.session.active_sequence().is_none()) || (source && app.session.state.source_item.is_none()) {
        crate::dock::placeholder(ui, rect, &t, tl!("Open a sequence or Source clip to see its transcript"));
        run(app, ui, actions);
        return;
    }
    if words.is_empty() {
        let c = rect.center();
        icons::paint(ui.painter(), Rect::from_center_size(c - vec2(0.0, 70.0), vec2(40.0, 40.0)), Icon::Captions, t.text_dim);
        ui.painter().text(c - vec2(0.0, 30.0), Align2::CENTER_CENTER, tl!("Transcribe"), Tokens::semibold(16.0), t.text);
        let note = if filmcraft_speech_available(app) {
            tl!("Speech-to-text turns the dialogue into editable text.")
        } else {
            tl!("This build has no speech-to-text; import a transcript with transcript.set.")
        };
        ui.painter().text(c - vec2(0.0, 8.0), Align2::CENTER_CENTER, note, Tokens::ui(12.0), t.text_dim);
        let r = Rect::from_center_size(c + vec2(0.0, 26.0), vec2(200.0, 26.0));
        let resp = ui.interact(r, egui::Id::new("text.transcript.generate"), Sense::click());
        ui.painter().rect_filled(r, 13.0, if resp.hovered() { t.accent_hover } else { t.accent });
        ui.painter().text(r.center(), Align2::CENTER_CENTER, tl!("Transcribe"), Tokens::semibold(12.0), Color32::WHITE);
        app.auto.add("text.transcript.generate", r, "Transcribe");
        if resp.clicked() {
            let params = if source { json!({"item": app.session.state.source_item.map(|i| i.0), "wait": false}) } else { json!({"wait": false}) };
            actions.push(("transcript.generate".into(), params));
        }
        run(app, ui, actions);
        return;
    }
    let sel = app.ui.transcript_sel.filter(|(a, b)| *a < words.len() && *b < words.len());
    let (sa, sb) = sel.map(|(a, b)| (a.min(b), a.max(b))).unzip();

    // ---- toolbar: search, extract / lift selection, remove fillers / pauses, captions
    let bar = Rect::from_min_size(rect.min + vec2(10.0, 2.0), vec2(rect.width() - 20.0, 26.0));
    let sw = 170.0f32.min(bar.width() * 0.4);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(bar.min, vec2(sw, 24.0))));
    let sresp = crate::widgets::search_field(&mut child, &mut app.ui.transcript_search, tl!("Search"), sw, &t);
    app.auto.add("text.transcript.search", sresp.rect, "Search transcript");
    let hits: Vec<std::ops::Range<usize>> = filmcraft_edit::transcript::search(&words, &app.ui.transcript_search);
    let mut x = bar.min.x + sw + 10.0;
    let range = sel.map(|(a, b)| json!({"from": a.min(b), "to": a.max(b)}));
    let tools: [(Icon, &str, &str, bool, &str, Value); 4] = [
        (
            Icon::Razor,
            "text.transcript.extract",
            tl!("Extract selected text"),
            sel.is_some() && !source,
            "transcript.extract",
            range.clone().unwrap_or_default(),
        ),
        (Icon::Trash, "text.transcript.lift", tl!("Lift selected text"), sel.is_some() && !source, "transcript.lift", range.unwrap_or_default()),
        (Icon::Link, "text.transcript.removeFillers", tl!("Remove filler words"), !source, "transcript.removeFillers", json!({})),
        (Icon::Captions, "text.transcript.createCaptions", tl!("Create captions"), !source, "transcript.createCaptions", json!({})),
    ];
    for (icon, id, label, enabled, cmd, params) in tools {
        let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(24.0, 24.0));
        if r.max.x > rect.max.x {
            break;
        }
        if tool_button(app, ui, r, icon, id, label, enabled) {
            if cmd.ends_with("extract") || cmd.ends_with("lift") {
                app.ui.transcript_sel = None;
            }
            actions.push((cmd.into(), params));
        }
        x += 28.0;
    }

    // ---- paragraphs of words
    let list = Rect::from_min_max(pos2(rect.min.x + 6.0, bar.max.y + 8.0), pos2(rect.max.x - 6.0, rect.max.y - 4.0));
    ui.painter().rect_filled(list, 3.0, t.app_bg);
    let ph = if source { app.session.state.source_playhead } else { app.session.playhead() };
    let current = filmcraft_edit::transcript::word_at(&words, ph);
    let paras = filmcraft_edit::transcript::paragraphs(&words, filmcraft_time::Tick::from_seconds_f64(1.5));
    let rate = if source {
        app.session.state.source_item.and_then(|id| filmcraft_engine::clip_ops::source_view(&app.session, id)).map(|v| v.rate).unwrap_or_default()
    } else {
        app.session.sequence_rate()
    };
    let df = !source && app.session.active_sequence().is_some_and(|q| q.settings.drop_frame);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list.shrink(6.0)).id_salt("transcript-list"));
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("transcript-scroll").show(&mut child, |ui| {
        ui.set_width(list.width() - 16.0);
        for (pi, pr) in paras.iter().enumerate() {
            let w0 = &words[pr.start];
            let head = format!("{}  {}", w0.speaker.as_deref().unwrap_or(tl!("Speaker")), format_time(w0.start, rate, df, TimeDisplay::Timecode, 48_000));
            let hr = ui.label(egui::RichText::new(head).size(11.0).color(t.text_dim).strong());
            app.auto.add(&format!("text.transcript.paragraph.{pi}"), hr.rect, "Paragraph");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 3.0);
                for i in pr.clone() {
                    let w = &words[i];
                    let in_sel = sa.is_some_and(|a| i >= a) && sb.is_some_and(|b| i <= b);
                    let hit = hits.iter().any(|h| h.contains(&i));
                    let mut text = egui::RichText::new(&w.text).size(13.0).color(if Some(i) == current { t.hot_text } else { t.text });
                    if in_sel {
                        text = text.background_color(t.row_selected);
                    } else if hit {
                        text = text.background_color(t.hover);
                    }
                    let resp = ui.add(egui::Label::new(text).sense(Sense::click()));
                    app.auto.add(&format!("text.transcript.word.{i}"), resp.rect, &w.text);
                    if resp.double_clicked() {
                        app.ui.transcript_edit = Some((w.item.0, w.index, w.text.clone(), w.text.clone()));
                    }
                    if resp.clicked() {
                        let shift = ui.input(|inp| inp.modifiers.shift);
                        app.ui.transcript_sel = Some(match (shift, sel) {
                            (true, Some((a, _))) => (a, i),
                            _ => (i, i),
                        });
                        let (a, b) = app.ui.transcript_sel.unwrap_or((i, i));
                        if source {
                            actions.push(("source.setPlayhead".into(), json!({"time": w.start.0})));
                        } else {
                            actions.push(("transcript.select".into(), json!({"from": a.min(b), "to": a.max(b)})));
                        }
                    }
                }
            });
            ui.add_space(8.0);
        }
    });
    if let Some((item, index, mut text, original)) = app.ui.transcript_edit.clone() {
        let mut close = false;
        egui::Window::new(tl!("Correct transcript word"))
            .id(egui::Id::new("transcript-correction"))
            .default_pos(rect.center() - vec2(160.0, 50.0))
            .default_width(300.0)
            .resizable(false)
            .collapsible(false)
            .show(ui.ctx(), |ui| {
                let edit = ui.text_edit_singleline(&mut text);
                app.auto.add("text.transcript.correction", edit.rect, "Correct transcript word");
                ui.horizontal(|ui| {
                    let apply = ui.button(tl!("Apply"));
                    app.auto.add("text.transcript.correction.apply", apply.rect, "Apply correction");
                    if apply.clicked() || (edit.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter))) {
                        actions.push(("transcript.correctWord".into(), json!({"item": item, "index": index, "text": text, "expected": original})));
                        close = true;
                    }
                    let cancel = ui.button(tl!("Cancel"));
                    app.auto.add("text.transcript.correction.cancel", cancel.rect, "Cancel correction");
                    close |= cancel.clicked() || ui.input(|i| i.key_pressed(egui::Key::Escape));
                });
            });
        app.ui.transcript_edit = if close { None } else { Some((item, index, text, original)) };
    }

    run(app, ui, actions);
}

fn filmcraft_speech_available(app: &FilmcraftApp) -> bool {
    app.session.transcriber.is_some() || filmcraft_engine::transcript::speech_available()
}

fn style_strip(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, track_idx: usize, actions: &mut Vec<(String, Value)>) {
    let Some(tr) = app.session.active_sequence().and_then(|q| q.caption_tracks.get(track_idx)).cloned() else { return };
    let st = tr.style.clone();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).id_salt("caption-style"));
    child.horizontal_centered(|ui| {
        ui.spacing_mut().item_spacing.x = 6.0;
        let mut size = st.size;
        let resp = ui.add(egui::DragValue::new(&mut size).range(8.0..=200.0).speed(0.5).suffix(" px"));
        app.auto.add("text.captions.style.size", resp.rect, "Caption size");
        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
            actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "size": size})));
        }
        let mut col = Color32::from_rgba_unmultiplied(st.color[0], st.color[1], st.color[2], st.color[3]);
        let resp = egui::color_picker::color_edit_button_srgba(ui, &mut col, egui::color_picker::Alpha::Opaque);
        app.auto.add("text.captions.style.color", resp.rect, "Text colour");
        if resp.changed() {
            let c = col.to_srgba_unmultiplied();
            actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "color": c})));
        }
        let mut bg = st.background;
        let resp = ui.checkbox(&mut bg, tl!("Box"));
        app.auto.add("text.captions.style.background", resp.rect, "Background box");
        if resp.changed() {
            actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "background": bg})));
        }
        ui.label(egui::RichText::new(tl!("Align")).size(11.0).color(app.tokens.text_dim));
        for (a, icon_txt, name, tip) in [
            (CaptionAlign::Left, "L", "left", tl!("Align left")),
            (CaptionAlign::Center, "C", "center", tl!("Align center")),
            (CaptionAlign::Right, "R", "right", tl!("Align right")),
        ] {
            let resp = ui.selectable_label(st.align == a, icon_txt).on_hover_text(tip);
            app.auto.add(&format!("text.captions.style.align.{name}"), resp.rect, name);
            if resp.clicked() {
                actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "align": name})));
            }
        }
        ui.label(egui::RichText::new(tl!("Position")).size(11.0).color(app.tokens.text_dim));
        for (a, name, tip) in [
            (CaptionAnchor::Top, "top", tl!("Position: top")),
            (CaptionAnchor::Middle, "middle", tl!("Position: middle")),
            (CaptionAnchor::Bottom, "bottom", tl!("Position: bottom")),
        ] {
            let resp = ui.selectable_label(st.anchor == a, name[..1].to_uppercase()).on_hover_text(tip);
            app.auto.add(&format!("text.captions.style.anchor.{name}"), resp.rect, name);
            if resp.clicked() {
                actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "anchor": name})));
            }
        }
    });
}

fn run(app: &mut FilmcraftApp, ui: &egui::Ui, actions: Vec<(String, Value)>) {
    let ctx = ui.ctx().clone();
    for (cmd, p) in actions {
        if let Err(e) = crate::menus::invoke(app, &ctx, &cmd, p) {
            app.ui.status = e;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Latin labels keep the 7 px per character they were laid out with; Japanese labels are as
    /// wide as they are drawn, so they no longer run into the next tab.
    #[test]
    fn tab_widths_follow_the_drawn_label() {
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &Tokens::for_kind(crate::theme::ThemeKind::default()));
        // Japanese needs a craft-fonts build or an installed font; without one there is nothing wide to measure
        if !crate::i18n::install_japanese_font(&ctx) {
            return;
        }
        let labels = ["Transcript", "Captions", "Graphics", "文字起こし", "キャプション", "グラフィックス"];
        let mut widths = Vec::new();
        for _ in 0..2 {
            // the added font is in place from the second pass
            widths.clear();
            let mut out = ctx.run_ui(egui::RawInput::default(), |ui| {
                for label in labels {
                    widths.push(tab_text_width(ui.painter(), label));
                }
            });
            out.textures_delta.clear();
        }
        assert_eq!(widths[..3], [70.0, 56.0, 56.0], "Latin tabs keep 7 px per character");
        // 5, 6 and 7 Japanese characters of about 12.5 px each, far over the 7 px per character estimate
        for ((width, chars), label) in widths[3..].iter().zip([5.0_f32, 6.0, 7.0]).zip(&labels[3..]) {
            assert!(*width > chars * 7.0 * 1.5, "{label}: {width} px");
        }
    }
}

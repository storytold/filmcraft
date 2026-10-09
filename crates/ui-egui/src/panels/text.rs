//! The Text panel: Transcript / Captions / Graphics tabs. The Captions tab lists the caption
//! segments of a caption track with editable in/out timecodes and text, a toolbar (add, split,
//! merge, delete), track choice and the track style. Every edit dispatches a `captions.*` engine
//! command; every widget registers an automation id (`text.*`). The Transcript tab shows the
//! sequence transcript as speaker paragraphs of clickable words (click, Shift+click to extend);
//! the selection marks In/Out and can be extracted or lifted (`transcript.*` commands).
//!
//! Captions toolbar ids: `text.captions.search`, `text.captions.track` (the track picker; while
//! open `text.captions.track.<n>` shows track C<n> and `text.captions.newTrack.<format>`), and
//! `text.captions.<add|split|merge|delete|export>`; in a panel too narrow for them all, the last
//! ones move to the `text.captions.overflow` menu (») under the same ids. Transcribe options:
//! `text.transcribe.language` (+ `.option.<auto|code>` while open).

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_project::{CaptionAlign, CaptionAnchor, CaptionFormat};
use filmcraft_time::{TimeDisplay, format_time};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

const TABS: [&str; 3] = ["Transcript", "Captions", "Graphics"];

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    // tabs
    let mut x = rect.min.x + 12.0;
    for tab in TABS {
        let w = tab.len() as f32 * 7.0 + 16.0;
        let r = Rect::from_min_size(pos2(x, rect.min.y + 4.0), vec2(w, 24.0));
        let resp = ui.interact(r, egui::Id::new(("text-tab", tab)), Sense::click());
        let active = app.ui.text_tab == tab;
        ui.painter().text(
            pos2(r.min.x, r.center().y),
            Align2::LEFT_CENTER,
            tab,
            if active { Tokens::semibold(12.5) } else { Tokens::ui(12.5) },
            if active { t.text } else { t.text_dim },
        );
        if active {
            ui.painter().line_segment([pos2(r.min.x, r.max.y), pos2(r.min.x + w - 16.0, r.max.y)], Stroke::new(2.0, t.text));
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
        _ => crate::dock::placeholder(ui, body, &t, "Graphics text search arrives with M10.1–M10.2"),
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
        crate::dock::placeholder(ui, rect, &t, "Open a sequence to work with captions");
        return;
    };
    let mut actions: Vec<(String, Value)> = Vec::new();
    if seq.caption_tracks.is_empty() {
        let c = rect.center();
        icons::paint(ui.painter(), Rect::from_center_size(c - vec2(0.0, 70.0), vec2(40.0, 40.0)), Icon::Captions, t.text_dim);
        ui.painter().text(c - vec2(0.0, 30.0), Align2::CENTER_CENTER, "Add captions", Tokens::semibold(16.0), t.text);
        ui.painter().text(c - vec2(0.0, 8.0), Align2::CENTER_CENTER, "Create a caption track or import a caption file.", Tokens::ui(12.0), t.text_dim);
        for (i, (id, label, cmd)) in
            [("text.captions.newTrack", "Create new caption track", "captions.newTrack"), ("text.captions.import", "Import captions file…", "captions.import")]
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
    let sresp = crate::widgets::search_field(&mut child, &mut app.ui.caption_search, "Search", 170.0f32.min(bar.width() * 0.4), &t);
    app.auto.add("text.captions.search", sresp.rect, "Search captions");
    let mut x = bar.min.x + 180.0f32.min(bar.width() * 0.4 + 10.0);
    // in a narrow panel the picker gives way so the overflow menu (») still fits beside it
    let picker = Rect::from_min_size(pos2(x, bar.min.y + 1.0), vec2(130.0f32.min(rect.max.x - x - 36.0).max(40.0), 22.0));
    let label = format!("C{} · {}", track_idx + 1, track.name);
    let presp = crate::widgets::dropdown_text(ui, picker, &label, &t, egui::Id::new("text-cap-track-picker"));
    app.auto.add("text.captions.track", picker, "Caption track");
    egui::Popup::menu(&presp).show(|ui| {
        for (i, tr) in seq.caption_tracks.iter().enumerate() {
            let label = format!("C{} · {} ({})", i + 1, tr.name, tr.format.label());
            let o = ui.selectable_label(i == track_idx, &label);
            app.auto.add(&format!("text.captions.track.{}", i + 1), o.rect, &label);
            if o.clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("text-cap-track"), i));
            }
        }
        ui.separator();
        for f in CaptionFormat::ALL {
            let label = format!("New {} track", f.label());
            let b = ui.button(&label);
            app.auto.add(&format!("text.captions.newTrack.{}", f.label()), b.rect, &label);
            if b.clicked() {
                actions.push(("captions.newTrack".into(), json!({"format": f.label()})));
            }
        }
    });
    x = picker.max.x + 8.0;
    let ph = app.session.playhead();
    let any_sel = !sel.is_empty();
    let under = track.caption_at(ph).filter(|c| c.start < ph).map(|c| c.id);
    let tools: [(Icon, &str, &str, bool, &str, Value); 5] = [
        (Icon::Plus, "text.captions.add", "Add caption at playhead", track.caption_at(ph).is_none(), "captions.add", json!({"track": track.id.0})),
        (
            Icon::Razor,
            "text.captions.split",
            "Split caption at playhead",
            under.is_some(),
            "captions.split",
            json!({"caption": under.map(|c| c.0), "time": ph.0}),
        ),
        (Icon::Link, "text.captions.merge", "Merge selected captions", sel.len() > 1, "captions.merge", json!({})),
        (Icon::Trash, "text.captions.delete", "Delete selected captions", any_sel, "captions.delete", json!({})),
        (Icon::Export, "text.captions.export", "Export captions…", !track.captions.is_empty(), "captions.export", json!({"track": track.id.0})),
    ];
    // the buttons that fit; the rest go in an overflow menu (»), whose entries keep their ids
    let slots = ((rect.max.x - x + 4.0) / 28.0).floor().max(0.0) as usize;
    let fit = if slots >= tools.len() { tools.len() } else { slots.saturating_sub(1) };
    let mut overflow = Vec::new();
    for (n, (icon, id, label, enabled, cmd, params)) in tools.into_iter().enumerate() {
        if n >= fit {
            overflow.push((id, label, enabled, cmd, params));
            continue;
        }
        let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(24.0, 24.0));
        if tool_button(app, ui, r, icon, id, label, enabled) {
            actions.push((cmd.into(), params));
        }
        x += 28.0;
    }
    if !overflow.is_empty() {
        let r = Rect::from_min_size(pos2(x.min(rect.max.x - 26.0), bar.min.y), vec2(24.0, 24.0));
        let resp = ui.interact(r, egui::Id::new("text-tool-overflow"), Sense::click()).on_hover_text("More caption tools");
        if resp.hovered() {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        ui.painter().text(r.center(), Align2::CENTER_CENTER, "»", Tokens::semibold(14.0), t.icon);
        app.auto.add("text.captions.overflow", r, "More caption tools");
        egui::Popup::menu(&resp).show(|ui| {
            for (id, label, enabled, cmd, params) in overflow {
                let b = ui.add_enabled(enabled, egui::Button::new(label));
                app.auto.add(id, b.rect, label);
                if b.clicked() {
                    actions.push((cmd.into(), params));
                    ui.close();
                }
            }
        });
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
                egui::RichText::new(if q.is_empty() { "No captions on this track. Press + to add one at the playhead." } else { "No matching captions." })
                    .color(t.text_faint),
            );
        }
    });
    run(app, ui, actions);
}

/// The transcript view, cached while the project, the sequence and the pause length stay the same.
fn transcript_view(app: &mut FilmcraftApp) -> std::sync::Arc<filmcraft_engine::transcript::TranscriptView> {
    let key: crate::TranscriptKey = (app.session.revision, app.session.state.active_sequence, app.session.prefs.media_analysis.pause_min_ms);
    if let Some((k, v)) = &app.transcript_view
        && *k == key
    {
        return v.clone();
    }
    let v = std::sync::Arc::new(filmcraft_engine::transcript::view(&app.session, None));
    app.transcript_view = Some((key, v.clone()));
    v
}

fn filter_of(app: &FilmcraftApp) -> filmcraft_engine::transcript::Filter {
    filmcraft_engine::transcript::Filter::from_name(&app.ui.transcript_filter).unwrap_or(filmcraft_engine::transcript::Filter::Text)
}

const FILTERS: [(&str, &str); 3] = [("text", "Transcript text"), ("fillers", "Filler words"), ("pauses", "Pauses")];

/// A small pill button; returns whether it was clicked.
fn pill(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, id: &str, label: &str, enabled: bool, primary: bool) -> bool {
    let t = app.tokens;
    let resp = ui.interact(r, egui::Id::new(("text-pill", id)), if enabled { Sense::click() } else { Sense::hover() });
    let fill = match (enabled, primary, resp.hovered()) {
        (false, _, _) => t.field_bg,
        (true, true, true) => t.accent_hover,
        (true, true, false) => t.accent,
        (true, false, true) => t.hover,
        (true, false, false) => t.field_bg,
    };
    ui.painter().rect_filled(r, r.height() / 2.0, fill);
    let col = if !enabled {
        t.text_faint
    } else if primary {
        Color32::WHITE
    } else {
        t.text
    };
    ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::semibold(11.5), col);
    app.auto.add(id, r, label);
    resp.clicked() && enabled
}

fn secs(t: filmcraft_time::Tick) -> f64 {
    t.0 as f64 / filmcraft_time::TICKS_PER_SECOND as f64
}

/// The empty Transcript tab: Transcribe (with its options), or the progress of a transcription.
fn transcribe_prompt(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    let c = rect.center();
    if let Some((_, f, status)) = filmcraft_engine::transcript::running(&app.session) {
        progress_card(app, ui, Rect::from_center_size(c, vec2((rect.width() - 40.0).clamp(120.0, 360.0), 70.0)), f, &status, actions);
        return;
    }
    icons::paint(ui.painter(), Rect::from_center_size(c - vec2(0.0, 92.0), vec2(40.0, 40.0)), Icon::Captions, t.text_dim);
    ui.painter().text(c - vec2(0.0, 52.0), Align2::CENTER_CENTER, "Transcribe sequence", Tokens::semibold(16.0), t.text);
    let model = filmcraft_engine::transcript::model_status(&app.session);
    let note = if model.is_some() {
        "Turns the dialogue into text you can edit, with every pause marked."
    } else {
        "This build has no speech-to-text; import a transcript with transcript.set."
    };
    ui.painter().text(c - vec2(0.0, 30.0), Align2::CENTER_CENTER, note, Tokens::ui(12.0), t.text_dim);
    let dialog = app.ui.transcribe_dialog.clone();
    let (Some(model), Some(draft)) = (model.clone(), dialog) else {
        let r = Rect::from_center_size(c + vec2(0.0, 2.0), vec2(200.0, 26.0));
        let resp = ui.interact(r, egui::Id::new("text.transcript.generate"), Sense::click());
        ui.painter().rect_filled(r, 13.0, if resp.hovered() { t.accent_hover } else { t.accent });
        ui.painter().text(r.center(), Align2::CENTER_CENTER, "Transcribe", Tokens::semibold(12.0), Color32::WHITE);
        app.auto.add("text.transcript.generate", r, "Transcribe");
        if resp.clicked() {
            if model.is_some() {
                app.ui.transcribe_dialog = Some(Default::default());
            } else {
                // no speech-to-text in this build: the command says why
                actions.push(("transcript.generate".into(), json!({})));
            }
        }
        return;
    };
    // the options: Premiere's "Create transcription" dialog
    let card = Rect::from_center_size(c + vec2(0.0, 58.0), vec2((rect.width() - 40.0).clamp(220.0, 340.0), 150.0));
    ui.painter().rect_filled(card, 6.0, t.app_bg);
    ui.painter().rect_stroke(card, 6.0, Stroke::new(1.0, t.field_bg), StrokeKind::Inside);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(card.shrink(10.0)).id_salt("transcribe-dialog"));
    let mut d = draft;
    let mut go = false;
    let mut close = false;
    child.vertical(|ui| {
        ui.spacing_mut().item_spacing.y = 6.0;
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Language").size(12.0).color(t.text_dim));
            let langs = filmcraft_engine::transcript::languages();
            let shown = if d.language == "auto" {
                "Detect automatically".to_string()
            } else {
                langs.iter().find(|(c, _)| *c == d.language).map(|(_, n)| n.to_string()).unwrap_or(d.language.clone())
            };
            let resp = egui::ComboBox::from_id_salt("transcribe-language").selected_text(shown).width(160.0).show_ui(ui, |ui| {
                // `text.transcribe.language.option.<auto|code>`: the entries scrolled into view
                for (code, name) in std::iter::once(&("auto", "Detect automatically")).chain(langs.iter()) {
                    let o = ui.selectable_value(&mut d.language, code.to_string(), *name);
                    if let Some(vr) = crate::widgets::visible(ui, o.rect) {
                        app.auto.add(&format!("text.transcribe.language.option.{code}"), vr, name);
                    }
                }
            });
            app.auto.add("text.transcribe.language", resp.response.rect, "Language");
        });
        let resp = ui.checkbox(&mut d.speakers, "Separate speakers");
        app.auto.add("text.transcribe.speakers", resp.rect, "Separate speakers");
        let model_line = if model.installed {
            format!("Model: {}", model.name)
        } else {
            format!("Model: {} · {:.1} GB one-time download ({})", model.name, model.download_bytes as f64 / 1e9, model.source.trim_start_matches("https://"))
        };
        ui.label(egui::RichText::new(model_line).size(11.0).color(t.text_dim));
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            let cancel = ui.button("Cancel");
            app.auto.add("text.transcribe.cancel", cancel.rect, "Cancel");
            close = cancel.clicked();
            let label = if model.installed { "Transcribe" } else { "Download and transcribe" };
            let ok = ui.add(egui::Button::new(egui::RichText::new(label).color(Color32::WHITE)).fill(t.accent));
            app.auto.add("text.transcribe.ok", ok.rect, label);
            go = ok.clicked();
        });
    });
    if go {
        actions.push(("transcript.generate".into(), json!({"wait": false, "language": d.language, "diarize": d.speakers, "download": !model.installed})));
        close = true;
    }
    app.ui.transcribe_dialog = if close { None } else { Some(d) };
}

/// A transcription in progress: status, bar, Stop.
fn progress_card(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, f: f32, status: &str, actions: &mut Vec<(String, Value)>) {
    let t = app.tokens;
    ui.painter().rect_filled(r, 6.0, t.app_bg);
    ui.painter().text(
        pos2(r.min.x + 10.0, r.min.y + 16.0),
        Align2::LEFT_CENTER,
        if status.is_empty() { "Transcribing…" } else { status },
        Tokens::ui(12.0),
        t.text,
    );
    let bar = Rect::from_min_size(pos2(r.min.x + 10.0, r.min.y + 32.0), vec2(r.width() - 90.0, 6.0));
    ui.painter().rect_filled(bar, 3.0, t.field_bg);
    ui.painter().rect_filled(Rect::from_min_size(bar.min, vec2(bar.width() * f.clamp(0.0, 1.0), bar.height())), 3.0, t.accent);
    app.auto.add("text.transcript.progress", bar, &format!("{:.0}%", f * 100.0));
    let stop = Rect::from_min_size(pos2(bar.max.x + 12.0, r.min.y + 24.0), vec2(56.0, 22.0));
    if pill(app, ui, stop, "text.transcript.stop", "Stop", true, false) {
        actions.push(("transcript.cancel".into(), json!({})));
    }
    ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
}

fn transcript(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    use filmcraft_edit::transcript::Token;
    use filmcraft_engine::transcript::Filter;
    let t = app.tokens;
    if app.session.active_sequence().is_none() {
        crate::dock::placeholder(ui, rect, &t, "Open a sequence to see its transcript");
        return;
    }
    let mut actions: Vec<(String, Value)> = Vec::new();
    let v = transcript_view(app);
    if v.words.is_empty() {
        transcribe_prompt(app, ui, rect, &mut actions);
        run(app, ui, actions);
        return;
    }
    let words = &v.words;
    let sel = app.ui.transcript_sel.filter(|(a, b)| *a < words.len() && *b < words.len());
    let (sa, sb) = sel.map(|(a, b)| (a.min(b), a.max(b))).unzip();
    let filter = filter_of(app);
    let hits = filmcraft_engine::transcript::hits(&app.session, &v, filter, &app.ui.transcript_search, &json!({}));
    let current = (!hits.is_empty()).then(|| app.ui.transcript_hit.min(hits.len() - 1));
    let rate = app.session.sequence_rate();
    let ma = app.session.prefs.media_analysis.clone();
    let mut scroll_to_hit = false;

    // ---- row 1: filter, search, result count, ▲ ▼, "…"
    let row1 = Rect::from_min_size(rect.min + vec2(10.0, 2.0), vec2(rect.width() - 20.0, 24.0));
    let fr = Rect::from_min_size(row1.min, vec2(112.0, 22.0));
    let flabel = FILTERS.iter().find(|(k, _)| *k == app.ui.transcript_filter).map(|(_, n)| *n).unwrap_or("Transcript text");
    let fresp = crate::widgets::dropdown_text(ui, fr, flabel, &t, egui::Id::new("text-transcript-filter"));
    app.auto.add("text.transcript.filter", fr, "Search filter");
    egui::Popup::menu(&fresp).show(|ui| {
        for (k, name) in FILTERS {
            let r = ui.selectable_label(app.ui.transcript_filter == k, name);
            app.auto.add(&format!("text.transcript.filter.{k}"), r.rect, name);
            if r.clicked() {
                app.ui.transcript_filter = k.to_string();
                app.ui.transcript_hit = 0;
            }
        }
    });
    let sw = (row1.width() - 112.0 - 8.0 - 150.0).clamp(80.0, 220.0);
    let sr = Rect::from_min_size(pos2(fr.max.x + 6.0, row1.min.y), vec2(sw, 22.0));
    if filter == Filter::Text {
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(sr));
        let before = app.ui.transcript_search.clone();
        let sresp = crate::widgets::search_field(&mut child, &mut app.ui.transcript_search, "Search", sw, &t);
        if app.ui.transcript_search != before {
            app.ui.transcript_hit = 0;
        }
        app.auto.add("text.transcript.search", sresp.rect, "Search transcript");
    } else {
        // Premiere greys the search field out and names what the filter found
        ui.painter().rect_filled(sr, 11.0, t.field_bg);
        let what = if filter == Filter::Pauses { format!("Pauses ≥ {:.2} s", f64::from(ma.pause_min_ms) / 1000.0) } else { "Filler words".to_string() };
        ui.painter().text(pos2(sr.min.x + 10.0, sr.center().y), Align2::LEFT_CENTER, what, Tokens::ui(12.0), t.text_dim);
        app.auto.add("text.transcript.search", sr, "Search transcript");
    }
    let mut x = sr.max.x + 8.0;
    let count = match current {
        Some(i) => format!("{} of {}", i + 1, hits.len()),
        None if filter == Filter::Text && app.ui.transcript_search.trim().is_empty() => String::new(),
        None => "No results".into(),
    };
    let cr = Rect::from_min_size(pos2(x, row1.min.y), vec2(66.0, 22.0));
    ui.painter().text(cr.left_center(), Align2::LEFT_CENTER, &count, Tokens::ui(11.5), t.text_dim);
    app.auto.add("text.transcript.hits", cr, &count);
    x = cr.max.x + 2.0;
    for (id, label, d) in [("text.transcript.prevHit", "Previous result", -1i64), ("text.transcript.nextHit", "Next result", 1)] {
        let r = Rect::from_min_size(pos2(x, row1.min.y), vec2(22.0, 22.0));
        let resp = ui.interact(r, egui::Id::new(id), if hits.is_empty() { Sense::hover() } else { Sense::click() });
        if resp.hovered() && !hits.is_empty() {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        let col = if hits.is_empty() { t.text_faint } else { t.icon };
        let (cx, cy) = (r.center().x, r.center().y);
        let tri = if d < 0 {
            vec![pos2(cx - 4.5, cy + 2.5), pos2(cx + 4.5, cy + 2.5), pos2(cx, cy - 3.0)]
        } else {
            vec![pos2(cx - 4.5, cy - 2.5), pos2(cx + 4.5, cy - 2.5), pos2(cx, cy + 3.0)]
        };
        ui.painter().add(egui::Shape::convex_polygon(tri, col, Stroke::NONE));
        app.auto.add(id, r, label);
        if resp.on_hover_text(label).clicked() && !hits.is_empty() {
            let n = hits.len() as i64;
            let i = (current.unwrap_or(0) as i64 + d).rem_euclid(n) as usize;
            app.ui.transcript_hit = i;
            if let Some(h) = hits.get(i) {
                app.session.set_playhead(h.range.start);
                app.ui.transcript_pause = h.pause;
            }
            scroll_to_hit = true;
        }
        x += 24.0;
    }
    // "…": Transcript View Options (pauses), transcription
    let mr = Rect::from_min_size(pos2(row1.max.x - 22.0, row1.min.y), vec2(22.0, 22.0));
    let mresp = ui.interact(mr, egui::Id::new("text.transcript.menu"), Sense::click());
    if mresp.hovered() {
        ui.painter().rect_filled(mr, 3.0, t.hover);
    }
    for dx in [-5.0, 0.0, 5.0] {
        ui.painter().circle_filled(mr.center() + vec2(dx, 0.0), 1.6, t.icon);
    }
    app.auto.add("text.transcript.menu", mr, "Transcript options");
    egui::Popup::menu(&mresp).close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside).show(|ui| {
        ui.set_min_width(250.0);
        let mut show = ma.show_pauses;
        let r = ui.checkbox(&mut show, "Show pauses as [...]");
        app.auto.add("text.transcript.menu.showPauses", r.rect, "Show pauses");
        if r.changed() {
            actions.push(("prefs.set".into(), json!({"key": "mediaAnalysis.showPauses", "value": show})));
        }
        for (key, label, val, lo, hi) in [
            ("pauseMinMs", "Pause length", ma.pause_min_ms, 80u32, 3000u32),
            ("pauseKeepAfterMs", "Keep after a word", ma.pause_keep_after_ms, 0, 300),
            ("pauseKeepBeforeMs", "Keep before the next word", ma.pause_keep_before_ms, 0, 300),
        ] {
            ui.horizontal(|ui| {
                ui.label(label);
                let mut v = val;
                let r = ui.add(egui::DragValue::new(&mut v).range(lo..=hi).speed(5.0).suffix(" ms"));
                app.auto.add(&format!("text.transcript.menu.{key}"), r.rect, label);
                if v != val && (r.drag_stopped() || (r.changed() && !r.dragged())) {
                    actions.push(("prefs.set".into(), json!({"key": format!("mediaAnalysis.{key}"), "value": v})));
                }
            });
        }
        ui.separator();
        let r = ui.button("Re-transcribe sequence…");
        app.auto.add("text.transcript.menu.retranscribe", r.rect, "Re-transcribe sequence");
        if r.clicked() {
            // this sequence's clips only; other sequences keep their transcripts
            let items: Vec<u64> =
                app.session.active_sequence().map(|q| q.audio_tracks.iter().flat_map(|t| t.items.iter()).map(|i| i.item.0).collect()).unwrap_or_default();
            actions.push(("transcript.delete".into(), json!({"items": items})));
            app.ui.transcribe_dialog = Some(Default::default());
            ui.close();
        }
        let r = ui.button("Create captions from transcript");
        app.auto.add("text.transcript.menu.captions", r.rect, "Create captions");
        if r.clicked() {
            actions.push(("transcript.createCaptions".into(), json!({})));
            ui.close();
        }
    });

    // ---- row 2: Delete / Delete all, Extract | Lift, selected-text edits
    let row2 = Rect::from_min_size(pos2(rect.min.x + 10.0, row1.max.y + 6.0), vec2(rect.width() - 20.0, 22.0));
    let no_query = filter == Filter::Text && app.ui.transcript_search.trim().is_empty();
    let can_delete = !(hits.is_empty() || no_query);
    let mode = if app.ui.transcript_lift { "lift" } else { "extract" };
    let base = json!({"filter": app.ui.transcript_filter, "query": app.ui.transcript_search, "mode": mode});
    let mut x = row2.min.x;
    let dr = Rect::from_min_size(pos2(x, row2.min.y), vec2(62.0, 22.0));
    if pill(app, ui, dr, "text.transcript.delete", "Delete", can_delete && current.is_some(), false) {
        let mut p = base.clone();
        p["hit"] = json!(current.unwrap_or(0));
        actions.push(("transcript.deleteHits".into(), p));
    }
    x = dr.max.x + 6.0;
    let da = Rect::from_min_size(pos2(x, row2.min.y), vec2(80.0, 22.0));
    if pill(app, ui, da, "text.transcript.deleteAll", "Delete all", can_delete, true) {
        actions.push(("transcript.deleteHits".into(), base.clone()));
        app.ui.transcript_hit = 0;
    }
    x = da.max.x + 10.0;
    for (lift, id, label) in [(false, "text.transcript.mode.extract", "Extract"), (true, "text.transcript.mode.lift", "Lift")] {
        let r = Rect::from_min_size(pos2(x, row2.min.y), vec2(58.0, 22.0));
        let on = app.ui.transcript_lift == lift;
        let resp = ui.interact(r, egui::Id::new(id), Sense::click());
        ui.painter().rect_filled(
            r,
            3.0,
            if on {
                t.row_selected
            } else if resp.hovered() {
                t.hover
            } else {
                Color32::TRANSPARENT
            },
        );
        ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::ui(11.5), if on { t.text } else { t.text_dim });
        app.auto.add(id, r, label);
        if resp.on_hover_text(if lift { "Delete leaves a gap where the result was" } else { "Delete closes the gap (ripple)" }).clicked() {
            app.ui.transcript_lift = lift;
        }
        x += 60.0;
    }
    x += 8.0;
    let range = sel.map(|(a, b)| json!({"from": a.min(b), "to": a.max(b)}));
    let tools: [(Icon, &str, &str, bool, &str, Value); 2] = [
        (Icon::Razor, "text.transcript.extract", "Extract selected text", sel.is_some(), "transcript.extract", range.clone().unwrap_or_default()),
        (Icon::Trash, "text.transcript.lift", "Lift selected text", sel.is_some(), "transcript.lift", range.unwrap_or_default()),
    ];
    for (icon, id, label, enabled, cmd, params) in tools {
        let r = Rect::from_min_size(pos2(x, row2.min.y - 1.0), vec2(24.0, 24.0));
        if r.max.x > rect.max.x {
            break;
        }
        if tool_button(app, ui, r, icon, id, label, enabled) {
            app.ui.transcript_sel = None;
            actions.push((cmd.into(), params));
        }
        x += 28.0;
    }

    // ---- notices: transcription running, clips not transcribed, pauses from word gaps
    let mut top = row2.max.y + 6.0;
    if let Some((_, f, status)) = filmcraft_engine::transcript::running(&app.session) {
        let r = Rect::from_min_size(pos2(rect.min.x + 6.0, top), vec2(rect.width() - 12.0, 50.0));
        progress_card(app, ui, r, f, &status, &mut actions);
        top = r.max.y + 4.0;
    } else if v.untranscribed > 0 || v.without_voice > 0 {
        let r = Rect::from_min_size(pos2(rect.min.x + 10.0, top), vec2(rect.width() - 20.0, 22.0));
        let (msg, id, label, cmd, p) = if v.untranscribed > 0 {
            (
                format!(
                    "{} clip{} in this sequence {} no transcript.",
                    v.untranscribed,
                    if v.untranscribed == 1 { "" } else { "s" },
                    if v.untranscribed == 1 { "has" } else { "have" }
                ),
                "text.transcript.transcribeRest",
                "Transcribe",
                "",
                json!({}),
            )
        } else {
            (
                format!("Pauses in {} clip{} come from word gaps.", v.without_voice, if v.without_voice == 1 { "" } else { "s" }),
                "text.transcript.findPauses",
                "Measure pauses",
                "transcript.findPauses",
                json!({"wait": false}),
            )
        };
        ui.painter().text(r.left_center(), Align2::LEFT_CENTER, msg, Tokens::ui(11.5), t.text_dim);
        let br = Rect::from_min_size(pos2((r.max.x - 120.0).max(r.min.x), r.min.y), vec2(120.0, 22.0));
        if pill(app, ui, br, id, label, true, false) {
            if cmd.is_empty() {
                app.ui.transcribe_dialog = Some(Default::default());
                let items: Vec<u64> = app
                    .session
                    .active_sequence()
                    .map(|q| {
                        q.audio_tracks
                            .iter()
                            .flat_map(|t| t.items.iter())
                            .filter(|i| i.enabled && !app.session.project.transcripts.contains_key(&i.item))
                            .map(|i| i.item.0)
                            .collect()
                    })
                    .unwrap_or_default();
                actions.push(("transcript.generate".into(), json!({"items": items, "wait": false, "language": "en", "diarize": false})));
                app.ui.transcribe_dialog = None;
            } else {
                actions.push((cmd.into(), p));
            }
        }
        top = r.max.y + 4.0;
    }

    // ---- the transcript: paragraphs of words, [...] pauses and [speech]
    let list = Rect::from_min_max(pos2(rect.min.x + 6.0, top + 2.0), pos2(rect.max.x - 6.0, rect.max.y - 4.0));
    ui.painter().rect_filled(list, 3.0, t.app_bg);
    let ph = app.session.playhead();
    let playing = filmcraft_edit::transcript::word_at(words, ph);
    let show_pauses = ma.show_pauses || filter == Filter::Pauses;
    let (keep_after, keep_before) = (
        filmcraft_time::Tick::from_seconds_f64(f64::from(ma.pause_keep_after_ms) / 1000.0),
        filmcraft_time::Tick::from_seconds_f64(f64::from(ma.pause_keep_before_ms) / 1000.0),
    );
    // which token is a result, and the current one
    let hit_of_word = |i: usize| hits.iter().position(|h| h.words.as_ref().is_some_and(|r| r.contains(&i)));
    let hit_of_pause = |k: usize| hits.iter().position(|h| h.pause == Some(k));
    // paragraphs: a new one at a speaker change or at a pause of 1.5 s or more
    let mut paras: Vec<Vec<Token>> = vec![Vec::new()];
    let mut last_speaker: Option<&Option<String>> = None;
    for tok in &v.tokens {
        match tok {
            Token::Pause(k) if v.pauses.get(*k).is_some_and(|p| p.range.duration >= filmcraft_time::Tick::from_seconds_f64(1.5)) => {
                if let Some(p) = paras.last_mut() {
                    p.push(*tok);
                }
                paras.push(Vec::new());
                continue;
            }
            Token::Word(i) => {
                let sp = &words[*i].speaker;
                if last_speaker.is_some_and(|l| l != sp) && paras.last().is_some_and(|p| !p.is_empty()) {
                    paras.push(Vec::new());
                }
                last_speaker = Some(sp);
            }
            _ => {}
        }
        if let Some(p) = paras.last_mut() {
            p.push(*tok);
        }
    }
    paras.retain(|p| p.iter().any(|t| matches!(t, Token::Word(_))));
    let df = app.session.active_sequence().is_some_and(|q| q.settings.drop_frame);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list.shrink(6.0)).id_salt("transcript-list"));
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("transcript-scroll").show(&mut child, |ui| {
        ui.set_width(list.width() - 16.0);
        for (pi, para) in paras.iter().enumerate() {
            let Some(w0) = para.iter().find_map(|t| if let Token::Word(i) = t { words.get(*i) } else { None }) else { continue };
            let who = if v.words.iter().any(|w| w.speaker.is_some() && w.speaker != words[0].speaker) {
                w0.speaker.clone().unwrap_or_default()
            } else {
                String::new()
            };
            let head = format!("{}{}{}", who, if who.is_empty() { "" } else { "  " }, format_time(w0.start, rate, df, TimeDisplay::Timecode, 48_000));
            let hr = ui.label(egui::RichText::new(head).size(11.0).color(t.text_dim).strong());
            app.auto.add(&format!("text.transcript.paragraph.{pi}"), hr.rect, "Paragraph");
            ui.horizontal_wrapped(|ui| {
                ui.spacing_mut().item_spacing = vec2(4.0, 3.0);
                for tok in para {
                    match *tok {
                        Token::Word(i) => {
                            let w = &words[i];
                            let in_sel = sa.is_some_and(|a| i >= a) && sb.is_some_and(|b| i <= b);
                            let h = hit_of_word(i);
                            let mut text = egui::RichText::new(&w.text).size(13.0).color(if Some(i) == playing { t.hot_text } else { t.text });
                            if in_sel {
                                text = text.background_color(t.row_selected);
                            } else if h.is_some() && h == current {
                                text = text.background_color(t.accent).color(Color32::WHITE);
                            } else if h.is_some() {
                                text = text.background_color(t.hover);
                            }
                            let resp = ui.add(egui::Label::new(text).sense(Sense::click()));
                            app.auto.add(&format!("text.transcript.word.{i}"), resp.rect, &w.text);
                            if scroll_to_hit && h.is_some() && h == current {
                                resp.scroll_to_me(Some(egui::Align::Center));
                            }
                            if resp.clicked() {
                                let shift = ui.input(|inp| inp.modifiers.shift);
                                app.ui.transcript_sel = Some(match (shift, sel) {
                                    (true, Some((a, _))) => (a, i),
                                    _ => (i, i),
                                });
                                app.ui.transcript_pause = None;
                                if let Some(h) = h {
                                    app.ui.transcript_hit = h;
                                }
                                let (a, b) = app.ui.transcript_sel.unwrap_or((i, i));
                                actions.push(("transcript.select".into(), json!({"from": a.min(b), "to": a.max(b)})));
                            }
                        }
                        Token::Pause(k) => {
                            if !show_pauses {
                                continue;
                            }
                            let Some(p) = v.pauses.get(k) else { continue };
                            let h = hit_of_pause(k);
                            let selected = app.ui.transcript_pause == Some(k);
                            let mut text = egui::RichText::new("[...]").size(12.0).color(t.text_dim);
                            if selected || (h.is_some() && h == current && filter == Filter::Pauses) {
                                text = text.background_color(t.accent).color(Color32::WHITE);
                            } else if h.is_some() {
                                text = text.background_color(t.hover);
                            }
                            let cut = filmcraft_edit::transcript::pause_cut(p, keep_after, keep_before, rate);
                            let tip = match cut {
                                Some(c) => format!("Pause {:.2} s · Delete removes {:.2} s", secs(p.range.duration), secs(c.duration)),
                                None => format!("Pause {:.2} s · too short to cut after the kept margins", secs(p.range.duration)),
                            };
                            let resp = ui.add(egui::Label::new(text).sense(Sense::click())).on_hover_text(tip);
                            app.auto.add(&format!("text.transcript.pause.{k}"), resp.rect, &format!("Pause {:.2} s", secs(p.range.duration)));
                            if scroll_to_hit && h.is_some() && h == current {
                                resp.scroll_to_me(Some(egui::Align::Center));
                            }
                            if resp.clicked() {
                                app.ui.transcript_pause = Some(k);
                                app.ui.transcript_sel = None;
                                if let Some(h) = h {
                                    app.ui.transcript_hit = h;
                                }
                                app.session.set_playhead(p.range.start);
                            }
                        }
                        Token::Speech(k) => {
                            let Some(r) = v.speech.get(k) else { continue };
                            let text = egui::RichText::new("[speech]").size(11.5).italics().color(t.render_yellow);
                            let resp = ui
                                .add(egui::Label::new(text).sense(Sense::click()))
                                .on_hover_text(format!("{:.2} s of voice the transcript has no words for (a stutter, a restart, a laugh)", secs(r.duration)));
                            app.auto.add(&format!("text.transcript.speech.{k}"), resp.rect, "Speech without words");
                            if resp.clicked() {
                                app.session.set_playhead(r.start);
                            }
                        }
                    }
                }
            });
            ui.add_space(8.0);
        }
    });
    run(app, ui, actions);
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
        let resp = ui.checkbox(&mut bg, "Box");
        app.auto.add("text.captions.style.background", resp.rect, "Background box");
        if resp.changed() {
            actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "background": bg})));
        }
        ui.label(egui::RichText::new("Align").size(11.0).color(app.tokens.text_dim));
        for (a, icon_txt, name) in [(CaptionAlign::Left, "L", "left"), (CaptionAlign::Center, "C", "center"), (CaptionAlign::Right, "R", "right")] {
            let resp = ui.selectable_label(st.align == a, icon_txt).on_hover_text(format!("Align {name}"));
            app.auto.add(&format!("text.captions.style.align.{name}"), resp.rect, name);
            if resp.clicked() {
                actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "align": name})));
            }
        }
        ui.label(egui::RichText::new("Position").size(11.0).color(app.tokens.text_dim));
        for (a, name) in [(CaptionAnchor::Top, "top"), (CaptionAnchor::Middle, "middle"), (CaptionAnchor::Bottom, "bottom")] {
            let resp = ui.selectable_label(st.anchor == a, name[..1].to_uppercase()).on_hover_text(format!("Position: {name}"));
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

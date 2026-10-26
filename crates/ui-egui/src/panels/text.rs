//! The Text panel: Transcript / Captions / Graphics tabs. The Graphics tab lists the text of every
//! text layer in the sequence's graphics (#1487), with a search; clicking an entry goes to its clip
//! and selects the layer, and the text edits in place (`graphics.set`). The Captions tab lists the caption
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
        let resp = ui.interact(r, egui::Id::unique(("text-tab", tab)), Sense::click());
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
        _ => graphics(app, ui, body),
    }
}

fn tool_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, icon: Icon, id: &str, label: &str, enabled: bool) -> bool {
    let t = app.tokens;
    let resp = ui.interact(r, egui::Id::unique(("text-tool", id)), if enabled { Sense::click() } else { Sense::hover() });
    if enabled && resp.hovered() {
        ui.painter().rect_filled(r, 3.0, t.hover);
    }
    icons::paint(ui.painter(), r.shrink(5.0), icon, if enabled { t.icon } else { t.text_faint });
    app.auto.add(id, r, label);
    resp.on_hover_text(label).clicked() && enabled
}

fn captions(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let project = app.session.project.clone();
    let Some(seq) = app.session.state.active_sequence.and_then(|id| project.sequence(id)) else {
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
            let resp = ui.interact(r, egui::Id::unique(id), Sense::click());
            ui.painter().rect_filled(r, 13.0, if i == 0 { if resp.hovered() { t.accent_hover } else { t.accent } } else { t.field_bg });
            ui.painter().text(r.center(), Align2::CENTER_CENTER, label, Tokens::semibold(12.0), if i == 0 { Color32::WHITE } else { t.text });
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
    let track_idx = ui.ctx().data(|d| d.get_temp::<usize>(egui::Id::unique("text-cap-track"))).filter(|i| *i < seq.caption_tracks.len()).unwrap_or(track_idx);
    let track = &seq.caption_tracks[track_idx];

    // ---- toolbar: search, track picker, add / split / merge / delete
    let bar = Rect::from_min_size(rect.min + vec2(10.0, 2.0), vec2(rect.width() - 20.0, 26.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(bar.min, vec2(170.0f32.min(bar.width() * 0.4), 24.0))));
    let sresp = crate::widgets::search_field(&mut child, &mut app.ui.caption_search, tl!("Search"), 170.0f32.min(bar.width() * 0.4), &t);
    app.auto.add("text.captions.search", sresp.rect, "Search captions");
    let mut x = bar.min.x + 180.0f32.min(bar.width() * 0.4 + 10.0);
    let picker = Rect::from_min_size(pos2(x, bar.min.y + 1.0), vec2(130.0, 22.0));
    let label = format!("C{} · {}", track_idx + 1, track.name);
    let presp = crate::widgets::dropdown_text(ui, picker, &label, &t, egui::Id::unique("text-cap-track-picker"));
    app.auto.add("text.captions.track", picker, "Caption track");
    egui::Popup::menu(&presp).show(|ui| {
        for (i, tr) in seq.caption_tracks.iter().enumerate() {
            if ui.selectable_label(i == track_idx, format!("C{} · {} ({})", i + 1, tr.name, crate::i18n::t(tr.format.label()))).clicked() {
                ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("text-cap-track"), i));
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
    if current.is_none() {
        ui.data_mut(|d| d.remove::<u64>(egui::Id::unique("caption-follow")));
    }
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
                            let key = egui::Id::unique(("cap-tc", c.id.0, edge));
                            let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| tc(val));
                            let resp = crate::widgets::accessible_name(
                                ui.add(
                                    egui::TextEdit::singleline(&mut buf)
                                        .id(key.with("te"))
                                        .desired_width(88.0)
                                        .font(Tokens::mono(11.0))
                                        .text_color(if edge == "in" { t.hot_text } else { t.text_dim }),
                                ),
                                if edge == "in" { "Caption in" } else { "Caption out" },
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
                        let key = egui::Id::unique(("cap-text", c.id.0));
                        let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| c.text.clone());
                        let resp = if crate::rtl::has_rtl(&buf) && !ui.memory(|m| m.has_focus(key.with("te"))) {
                            reading_order(ui, &buf, key.with("te"))
                        } else {
                            crate::widgets::accessible_name(
                                ui.add(
                                    egui::TextEdit::multiline(&mut buf)
                                        .id(key.with("te"))
                                        .desired_rows(1)
                                        .desired_width(ui.available_width())
                                        .font(Tokens::ui(12.5))
                                        .frame(egui::Frame::NONE),
                                ),
                                "Caption text",
                            )
                        };
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
                // the list follows the playhead: each caption it reaches is brought to the middle
                // once, but never while a field is being edited
                let follow = egui::Id::unique("caption-follow");
                if ui.data(|d| d.get_temp::<u64>(follow)) != Some(c.id.0) && ui.memory(|m| m.focused().is_none()) {
                    ui.scroll_to_rect(row, Some(egui::Align::Center));
                    ui.data_mut(|d| d.insert_temp(follow, c.id.0));
                }
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

/// A text layer of a graphic clip in the active sequence, for the Graphics tab.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphicText {
    pub clip: filmcraft_project::ClipId,
    /// The layer's number among the clip's layers (what `graphics.set` / `graphics.selectLayer` take).
    pub layer: usize,
    pub start: filmcraft_time::Tick,
    /// The track's label (`V2`).
    pub track: String,
    /// The layer's own name, when it has one.
    pub name: Option<String>,
    pub text: String,
    /// The timeline time the text was read at (the playhead, or the clip's start when the
    /// playhead is outside the clip): an edit is written there, so keyframed text changes the
    /// value that is shown.
    pub at: filmcraft_time::Tick,
}

/// Every text layer of every graphic clip in `seq`, in timeline order (then lower tracks first,
/// then paint order), with its text at `at` when that falls in the clip (Source Text keyframes
/// hold), else at the clip's first frame.
pub fn graphic_texts(seq: &filmcraft_project::Sequence, at: filmcraft_time::Tick) -> Vec<GraphicText> {
    use filmcraft_project::graphic::{TEXT_LAYER, layer_indices};
    let mut out = Vec::new();
    for (ti, track) in seq.video_tracks.iter().enumerate() {
        for it in &track.items {
            let t = if it.start <= at && at < it.end() { at } else { it.start };
            let mt = it.effect_time_at(t);
            for (layer, ei) in layer_indices(&it.effects).into_iter().enumerate() {
                let Some(e) = it.effects.get(ei).filter(|e| e.effect == TEXT_LAYER) else { continue };
                let text = match e.param("text").map(|p| p.value_at(mt)) {
                    Some(filmcraft_project::ParamValue::Text(s)) => s,
                    _ => String::new(),
                };
                let name = match e.param("name").map(|p| p.value.clone()) {
                    Some(filmcraft_project::ParamValue::Text(n)) if !n.trim().is_empty() => Some(n),
                    _ => None,
                };
                out.push((ti, GraphicText { clip: it.id, layer, start: it.start, track: format!("V{}", ti + 1), name, text, at: t }));
            }
        }
    }
    out.sort_by(|(ta, a), (tb, b)| a.start.cmp(&b.start).then(ta.cmp(tb)).then(a.layer.cmp(&b.layer)));
    out.into_iter().map(|(_, g)| g).collect()
}

/// The Graphics tab (#1487): the text of every text layer in the sequence, searchable. The
/// timecode goes to the clip and selects the layer; the text edits in place.
fn graphics(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let project = app.session.project.clone();
    let Some(seq) = app.session.state.active_sequence.and_then(|id| project.sequence(id)) else {
        crate::dock::placeholder(ui, rect, &t, tl!("Open a sequence to search its graphics"));
        return;
    };
    let (rate, df) = (seq.settings.frame_rate, seq.settings.drop_frame);
    let tc = |x: filmcraft_time::Tick| format_time(x, rate, df, TimeDisplay::Timecode, 48_000);
    let ph = app.session.playhead();
    let entries = graphic_texts(seq, ph);
    let mut actions: Vec<(String, Value)> = Vec::new();
    // search
    let bar = Rect::from_min_size(rect.min + vec2(10.0, 2.0), vec2(rect.width() - 20.0, 26.0));
    let skey = egui::Id::unique("text-gfx-search");
    let mut search = ui.data(|d| d.get_temp::<String>(skey)).unwrap_or_default();
    let w = 220.0f32.min(bar.width());
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(Rect::from_min_size(bar.min, vec2(w, 24.0))));
    let sresp = crate::widgets::search_field(&mut child, &mut search, tl!("Search"), w, &t);
    app.auto.add("text.graphics.search", sresp.rect, "Search graphics text");
    ui.data_mut(|d| d.insert_temp(skey, search.clone()));
    let q = search.to_lowercase();
    // list
    let list = Rect::from_min_max(pos2(rect.min.x + 6.0, bar.max.y + 8.0), pos2(rect.max.x - 6.0, rect.max.y - 4.0));
    ui.painter().rect_filled(list, 3.0, t.app_bg);
    let sel_clips = app.session.state.selection.clone();
    let sel_layers = app.session.state.graphic_layers.clone();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(list.shrink(4.0)).id_salt("graphics-text-list"));
    egui::ScrollArea::vertical().auto_shrink([false, false]).id_salt("graphics-text-scroll").show(&mut child, |ui| {
        ui.set_width(list.width() - 12.0);
        let mut shown = 0;
        for (n, g) in entries.iter().enumerate() {
            if !q.is_empty() && !g.text.to_lowercase().contains(&q) && !g.name.as_deref().unwrap_or("").to_lowercase().contains(&q) {
                continue;
            }
            shown += 1;
            let key = format!("{}.{}", g.clip.0, g.layer);
            let selected = sel_clips.contains(&g.clip) && sel_layers.contains(&g.layer);
            let fill = if selected {
                t.row_selected
            } else if n % 2 == 1 {
                t.row_alt
            } else {
                Color32::TRANSPARENT
            };
            let select = |actions: &mut Vec<(String, Value)>| {
                actions.push(("timeline.select".into(), json!({"clips": [g.clip.0]})));
                actions.push(("graphics.selectLayer".into(), json!({"clip": g.clip.0, "layers": [g.layer]})));
            };
            let frame = egui::Frame::NONE.fill(fill).inner_margin(egui::Margin::symmetric(6, 4)).corner_radius(3.0);
            let fr = frame.show(ui, |ui| {
                ui.horizontal_top(|ui| {
                    ui.vertical(|ui| {
                        ui.set_width(92.0);
                        let goto = ui.add(egui::Button::new(egui::RichText::new(tc(g.start)).font(Tokens::mono(11.0)).color(t.hot_text)).frame(false));
                        app.auto.add(&format!("text.graphics.{key}.goto"), goto.rect, "Go to graphic");
                        if goto.clicked() {
                            actions.push(("playhead.set".into(), json!({"time": g.start.0})));
                            select(&mut actions);
                        }
                        let label = match &g.name {
                            Some(name) => format!("{} · {name}", g.track),
                            None => g.track.clone(),
                        };
                        ui.label(egui::RichText::new(label).size(11.0).color(t.text_dim));
                    });
                    ui.vertical(|ui| {
                        let tkey = egui::Id::unique(("gfx-text", g.clip.0, g.layer));
                        let mut buf = ui.data(|d| d.get_temp::<String>(tkey)).unwrap_or_else(|| g.text.clone());
                        let resp = crate::widgets::accessible_name(
                            ui.add(
                                egui::TextEdit::multiline(&mut buf)
                                    .id(tkey.with("te"))
                                    .desired_rows(1)
                                    .desired_width(ui.available_width())
                                    .font(Tokens::ui(12.5))
                                    .frame(egui::Frame::NONE),
                            ),
                            "Graphic text",
                        );
                        app.auto.add(&format!("text.graphics.{key}.text"), resp.rect, "Graphic text");
                        if resp.has_focus() {
                            ui.data_mut(|d| d.insert_temp(tkey, buf.clone()));
                            if !selected {
                                select(&mut actions);
                            }
                        } else {
                            ui.data_mut(|d| d.remove::<String>(tkey));
                        }
                        if resp.lost_focus() && buf != g.text {
                            actions.push(("graphics.set".into(), json!({"clip": g.clip.0, "layer": g.layer, "props": {"text": buf}, "time": g.at.0})));
                        }
                    });
                });
            });
            app.auto.add(&format!("text.graphics.{key}.row"), fr.response.rect, &g.text);
            ui.add_space(2.0);
        }
        if shown == 0 {
            ui.label(
                egui::RichText::new(if entries.is_empty() {
                    tl!("No text graphics in this sequence. Add text with the Type tool.")
                } else {
                    tl!("No matching text.")
                })
                .color(t.text_faint),
            );
        }
    });
    run(app, ui, actions);
}

/// Whether a caption is selected, so the Properties panel shows [`caption_properties`].
pub fn caption_selected(app: &FilmcraftApp) -> bool {
    app.session.active_sequence().is_some_and(|q| app.session.state.caption_selection.iter().any(|c| q.find_caption(*c).is_some()))
}

/// The Properties panel for the first selected caption (#1846): its text and in/out timecodes,
/// edited with the same `captions.*` commands as the Captions tab. The style belongs to the caption
/// track and stays in the Captions tab.
pub fn caption_properties(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let project = app.session.project.clone();
    let Some(seq) = app.session.state.active_sequence.and_then(|id| project.sequence(id)) else { return };
    let Some((track_id, c)) = app.session.state.caption_selection.iter().find_map(|id| seq.find_caption(*id)) else { return };
    let track_idx = seq.caption_tracks.iter().position(|tr| tr.id == track_id).unwrap_or(0);
    let track_name = seq.caption_tracks.get(track_idx).map(|tr| tr.name.as_str()).unwrap_or_default();
    let (rate, df) = (seq.settings.frame_rate, seq.settings.drop_frame);
    let tc = |x: filmcraft_time::Tick| format_time(x, rate, df, TimeDisplay::Timecode, 48_000);
    let mut actions: Vec<(String, Value)> = Vec::new();
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(14.0, 8.0))).id_salt("props-caption"));
    let (hr, _) = bui.allocate_exact_size(vec2(bui.available_width(), 30.0), Sense::hover());
    bui.painter().text(pos2(hr.min.x, hr.center().y), Align2::LEFT_CENTER, format!("C{} · {}", track_idx + 1, track_name), Tokens::semibold(12.5), t.text);
    bui.label(egui::RichText::new(tl!("Text")).size(12.0).color(t.text_dim));
    let key = egui::Id::unique(("props-cap-text", c.id.0));
    let mut buf = bui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| c.text.clone());
    let resp = crate::widgets::accessible_name(
        bui.add(egui::TextEdit::multiline(&mut buf).id(key.with("te")).desired_rows(3).desired_width(bui.available_width()).font(Tokens::ui(12.5))),
        "Caption text",
    );
    app.auto.add("properties.caption.text", resp.rect, &c.text);
    if resp.has_focus() {
        bui.data_mut(|d| d.insert_temp(key, buf.clone()));
    } else {
        bui.data_mut(|d| d.remove::<String>(key));
    }
    if resp.lost_focus() && buf != c.text {
        actions.push(("captions.setText".into(), json!({"caption": c.id.0, "text": buf})));
    }
    bui.add_space(8.0);
    for (edge, label, val) in [("in", tl!("In"), c.start), ("out", tl!("Out"), c.end())] {
        bui.horizontal(|ui| {
            ui.add_sized(vec2(70.0, 22.0), egui::Label::new(egui::RichText::new(label).size(12.0).color(t.text_dim)));
            let key = egui::Id::unique(("props-cap-tc", c.id.0, edge));
            let mut buf = ui.data(|d| d.get_temp::<String>(key)).unwrap_or_else(|| tc(val));
            let resp = crate::widgets::accessible_name(
                ui.add(egui::TextEdit::singleline(&mut buf).id(key.with("te")).desired_width(100.0).font(Tokens::mono(11.5))),
                if edge == "in" { "In" } else { "Out" },
            );
            app.auto.add(&format!("properties.caption.{edge}"), resp.rect, &tc(val));
            if resp.has_focus() {
                ui.data_mut(|d| d.insert_temp(key, buf.clone()));
            } else {
                ui.data_mut(|d| d.remove::<String>(key));
            }
            if resp.lost_focus() && buf.trim() != tc(val) {
                let k = if edge == "in" { "startTimecode" } else { "endTimecode" };
                actions.push(("captions.setTimes".into(), json!({"caption": c.id.0, k: buf.trim()})));
            }
        });
    }
    bui.horizontal(|ui| {
        ui.add_sized(vec2(70.0, 22.0), egui::Label::new(egui::RichText::new(tl!("Duration")).size(12.0).color(t.text_dim)));
        ui.label(egui::RichText::new(tc(c.duration)).font(Tokens::mono(11.5)).color(t.text));
    });
    run(app, ui, actions);
}

/// A caption's text in reading order ([`crate::rtl::Lines`]) in place of its `TextEdit`, which
/// shows right-to-left words in logical order. A click hands focus to the `TextEdit` (`id`), so the
/// text is edited in logical order and shown in reading order again when the field loses focus.
fn reading_order(ui: &mut egui::Ui, text: &str, id: egui::Id) -> egui::Response {
    let margin = vec2(4.0, 2.0); // a TextEdit's own margin, so the text does not move on focus
    let color = ui.visuals().widgets.inactive.text_color();
    let width = ui.available_width();
    let lines = crate::rtl::Lines::new(ui.painter(), text, Tokens::ui(12.5), color, (width - 2.0 * margin.x).max(0.0));
    let (rect, resp) = ui.allocate_exact_size(vec2(width, lines.size.y + 2.0 * margin.y), Sense::click());
    lines.paint(ui.painter(), rect.shrink2(margin), color);
    if resp.clicked() {
        ui.memory_mut(|m| m.request_focus(id));
    }
    resp.on_hover_cursor(egui::CursorIcon::Text)
}

fn transcript(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    if app.session.active_sequence().is_none() {
        crate::dock::placeholder(ui, rect, &t, tl!("Open a sequence to see its transcript"));
        return;
    }
    let mut actions: Vec<(String, Value)> = Vec::new();
    let content = app.transcript_cache.content(&app.session);
    let words = &content.words;
    if words.is_empty() {
        let c = rect.center();
        icons::paint(ui.painter(), Rect::from_center_size(c - vec2(0.0, 70.0), vec2(40.0, 40.0)), Icon::Captions, t.text_dim);
        ui.painter().text(c - vec2(0.0, 30.0), Align2::CENTER_CENTER, tl!("Transcribe sequence"), Tokens::semibold(16.0), t.text);
        let note = if filmcraft_speech_available(app) {
            tl!("Speech-to-text turns the dialogue into editable text.")
        } else {
            tl!("This build has no speech-to-text; import a transcript with transcript.set.")
        };
        ui.painter().text(c - vec2(0.0, 8.0), Align2::CENTER_CENTER, note, Tokens::ui(12.0), t.text_dim);
        let r = Rect::from_center_size(c + vec2(0.0, 26.0), vec2(200.0, 26.0));
        let resp = ui.interact(r, egui::Id::unique("text.transcript.generate"), Sense::click());
        ui.painter().rect_filled(r, 13.0, if resp.hovered() { t.accent_hover } else { t.accent });
        ui.painter().text(r.center(), Align2::CENTER_CENTER, tl!("Transcribe"), Tokens::semibold(12.0), Color32::WHITE);
        app.auto.add("text.transcript.generate", r, "Transcribe");
        if resp.clicked() {
            actions.push(("transcript.generate".into(), json!({})));
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
    let highlights = app.transcript_cache.highlights(&app.ui.transcript_search);
    let mut x = bar.min.x + sw + 10.0;
    let range = sel.map(|(a, b)| json!({"from": a.min(b), "to": a.max(b)}));
    let tools: [(Icon, &str, &str, bool, &str, Value); 4] = [
        (Icon::Razor, "text.transcript.extract", tl!("Extract selected text"), sel.is_some(), "transcript.extract", range.clone().unwrap_or_default()),
        (Icon::Trash, "text.transcript.lift", tl!("Lift selected text"), sel.is_some(), "transcript.lift", range.unwrap_or_default()),
        (Icon::Link, "text.transcript.removeFillers", tl!("Remove filler words"), true, "transcript.removeFillers", json!({})),
        (Icon::Captions, "text.transcript.createCaptions", tl!("Create captions"), true, "transcript.createCaptions", json!({})),
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
    let ph = app.session.playhead();
    let current = filmcraft_edit::transcript::word_at(words, ph);
    let paras = &content.paragraphs;
    let rate = app.session.sequence_rate();
    let df = app.session.active_sequence().is_some_and(|q| q.settings.drop_frame);
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
                    let hit = highlights.get(i).copied().unwrap_or(false);
                    let mut text = egui::RichText::new(&w.text).size(13.0).color(if Some(i) == current { t.hot_text } else { t.text });
                    if in_sel {
                        text = text.background_color(t.row_selected);
                    } else if hit {
                        text = text.background_color(t.hover);
                    }
                    let resp = ui.add(egui::Label::new(text).sense(Sense::click()));
                    app.auto.add(&format!("text.transcript.word.{i}"), resp.rect, &w.text);
                    if resp.clicked() {
                        let shift = ui.input(|inp| inp.modifiers.shift);
                        app.ui.transcript_sel = Some(match (shift, sel) {
                            (true, Some((a, _))) => (a, i),
                            _ => (i, i),
                        });
                        let (a, b) = app.ui.transcript_sel.unwrap_or((i, i));
                        actions.push(("transcript.select".into(), json!({"from": a.min(b), "to": a.max(b)})));
                    }
                }
            });
            ui.add_space(8.0);
        }
    });
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
        let r = egui::ComboBox::from_id_salt(("caption-font", tr.id.0)).selected_text(&st.font).width(130.0).height(400.0).show_ui(ui, |ui| {
            if !filmcraft_text::fonts::system_scanned() {
                filmcraft_text::fonts::scan_system();
            }
            // names starting with '.' are macOS-private system faces (UI/fallback fonts, some without a space glyph)
            for (f, _) in filmcraft_text::families().into_iter().filter(|(f, _)| !f.starts_with('.')) {
                let resp = ui.selectable_label(f.eq_ignore_ascii_case(&st.font), &f);
                // An edge row may be only partly visible; automation must click its visible portion.
                let visible = resp.rect.intersect(ui.clip_rect());
                if visible.is_positive() {
                    app.auto.add(&format!("text.captions.style.font.option.{f}"), visible, &f);
                }
                if resp.clicked() {
                    actions.push(("captions.setStyle".into(), json!({"track": tr.id.0, "font": f})));
                }
            }
        });
        app.auto.add("text.captions.style.font", r.response.rect, "Caption font");
        let mut size = st.size;
        let resp = ui.add(crate::widgets::number_drag(&mut size).range(8.0..=200.0).speed(0.5).suffix(" px"));
        app.auto.add("text.captions.style.size", resp.rect, "Caption size");
        // a drag ended by Escape keeps the size it had (#580)
        let escaped = resp.drag_stopped() && ui.input(|i| i.key_pressed(egui::Key::Escape));
        if !escaped && (resp.drag_stopped() || (resp.changed() && !resp.dragged())) {
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

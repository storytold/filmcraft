//! Search and selection are view state; mutations dispatch atomic owner-scoped commands.
use super::marker_properties::Owner;
use crate::FilmcraftApp;
use egui::{Color32, Rect, Sense, vec2};
use filmcraft_project::Label;
use serde::{Deserialize, Serialize};
use serde_json::json;

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct State {
    pub source: bool,
    pub search: String,
    #[serde(skip)]
    pub owner: Option<Owner>,
    #[serde(skip)]
    pub selected: std::collections::BTreeSet<u64>,
    #[serde(skip)]
    pub anchor: Option<u64>,
    #[serde(skip)]
    pub project: Option<std::sync::Arc<()>>,
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let mut state = std::mem::take(&mut app.ui.markers_panel);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(6.0)));
    let ui = &mut child;
    ui.horizontal_wrapped(|ui| {
        for (source, label) in [(false, "Sequence"), (true, "Source")] {
            let response = ui.selectable_label(state.source == source, label);
            app.auto.add_response(&format!("markers.scope.{label}"), &response, label);
            if response.clicked() {
                state.source = source;
            }
        }
        let response = ui.button("Manage Colors…");
        app.auto.add_response("markers.manageColors", &response, "Manage Colors");
        if response.clicked() {
            super::palette_manager::open(app);
        }
    });
    let response = ui.add(egui::TextEdit::singleline(&mut state.search).hint_text("Search name, notes, author").desired_width(f32::INFINITY));
    app.auto.add_response("markers.search", &response, "Search markers");
    let owner = Owner::current(app, state.source);
    if owner != state.owner || state.project.as_ref().is_none_or(|p| !std::sync::Arc::ptr_eq(p, &app.session.project_instance)) {
        state.selected.clear();
        state.anchor = None;
        state.owner = owner;
        state.project = Some(app.session.project_instance.clone());
    }
    let Some((owner, (title, rate, drop, markers))) = owner.and_then(|owner| owner.details(app).map(|d| (owner, d))) else {
        ui.label(if state.source { "No source clip open" } else { "No sequence open" });
        app.ui.markers_panel = state;
        return;
    };
    ui.label(title);
    let hidden = app.session.state.hidden_marker_colors.clone();
    let hidden_custom = app.session.state.hidden_custom_marker_colors.clone();
    let mut customs = super::custom_colors::available(app);
    for c in markers.iter().filter_map(|m| m.custom_color) {
        if !customs.contains(&c) && customs.len() < 64 {
            customs.push(c);
        }
    }
    ui.horizontal_wrapped(|ui| {
        for color in Label::ALL {
            let on = !hidden.contains(&color);
            let response = filter(ui, app.session.prefs.labels.rgb(color), on).on_hover_text(color.name());
            app.auto.add_response(&format!("markers.filter.{}", color.name()), &response, color.name());
            if response.clicked()
                && let Err(e) = app.session.execute("markers.filterColors", json!({"color":color.name(),"visible":!on}))
            {
                app.ui.status = e.to_string();
            }
        }
    });
    ui.horizontal_wrapped(|ui| {
        for color in customs {
            let on = !hidden_custom.contains(&color);
            let response = filter(ui, color.0, on).on_hover_text(super::custom_colors::name(app, color));
            app.auto.add_response(&format!("markers.filter.{}", color.hex()), &response, &color.hex());
            if response.clicked()
                && let Err(e) = app.session.execute("markers.filterColors", json!({"color":color,"visible":!on}))
            {
                app.ui.status = e.to_string();
            }
        }
    });
    let query = state.search.to_lowercase();
    let shown: Vec<_> = markers
        .iter()
        .filter(|m| {
            m.custom_color.map_or(!hidden.contains(&m.color), |c| !hidden_custom.contains(&c))
                && (query.is_empty() || [&m.name, &m.comment, &m.author].iter().any(|s| s.to_lowercase().contains(&query)))
        })
        .collect();
    // Hidden rows are never accidental bulk-operation targets.
    let visible_ids: std::collections::BTreeSet<_> = shown.iter().map(|m| m.id.0).collect();
    state.selected.retain(|id| visible_ids.contains(id));
    ui.horizontal_wrapped(|ui| {
        let r = ui.button("Select all");
        app.auto.add_response("markers.selectAll", &r, "Select all visible markers");
        if r.clicked() {
            state.selected = shown.iter().map(|m| m.id.0).collect();
        }
        let r = ui.add_enabled(!state.selected.is_empty(), egui::Button::new("Delete"));
        app.auto.add_response("markers.deleteSelected", &r, "Delete selected markers");
        if r.clicked() {
            bulk(app, owner, &state.selected, "markers.delete", None);
        }
        ui.add_enabled_ui(!state.selected.is_empty(), |ui| {
            let menu = ui.menu_button("Color", |ui| {
                for preset in Label::ALL {
                    let r = ui.button(preset.name());
                    app.auto.add_response(&format!("markers.bulkColor.{}", preset.name()), &r, preset.name());
                    if r.clicked() {
                        bulk(app, owner, &state.selected, "markers.edit", Some(preset.name()));
                        ui.close();
                    }
                }
                for color in super::custom_colors::available(app) {
                    let name = super::custom_colors::name(app, color);
                    let r = ui.button(name);
                    app.auto.add_response(&format!("markers.bulkColor.{}", color.hex()), &r, &color.hex());
                    if r.clicked() {
                        bulk(app, owner, &state.selected, "markers.edit", Some(&color.hex()));
                        ui.close();
                    }
                }
            });
            app.auto.add_response("markers.bulkColor", &menu.response, "Color selected markers");
        });
        ui.label(format!("{} selected", state.selected.len()));
    });
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        if shown.is_empty() {
            ui.label(if markers.is_empty() { "No markers" } else { "No matching markers" });
        }
        for (index, m) in shown.iter().enumerate() {
            let selected = state.selected.contains(&m.id.0);
            let (rect, response) = ui.allocate_exact_size(vec2(ui.available_width(), 42.0), Sense::click());
            let t = app.tokens;
            if selected || response.hovered() {
                ui.painter().rect_filled(rect, 3.0, if selected { t.row_selected } else { t.hover });
            }
            let [r, g, b] = m.custom_color.map_or_else(|| app.session.prefs.labels.rgb(m.color), |c| c.0);
            ui.painter().rect_filled(Rect::from_min_size(rect.min + vec2(4.0, 6.0), vec2(4.0, 30.0)), 2.0, Color32::from_rgb(r, g, b));
            let name = if m.name.is_empty() { "Marker" } else { &m.name };
            let time = |tick| filmcraft_time::format_time(tick, rate, drop, filmcraft_time::TimeDisplay::Timecode, 48000);
            let mut line = time(m.start);
            if m.duration > filmcraft_time::Tick::ZERO {
                line.push_str(&format!("  ({})", time(m.duration)));
            }
            if m.kind == filmcraft_project::MarkerKind::Chapter {
                line.push_str("  Chapter");
            }
            if !m.author.is_empty() {
                line.push_str(&format!("  · {}", m.author));
            }
            ui.painter().text(rect.min + vec2(16.0, 12.0), egui::Align2::LEFT_CENTER, name, crate::theme::Tokens::ui(12.0), t.text);
            ui.painter().text(rect.min + vec2(16.0, 29.0), egui::Align2::LEFT_CENTER, line, crate::theme::Tokens::mono(11.0), t.hot_text);
            app.auto.add_response(&format!("markers.row.{}", m.id.0), &response, name);
            if response.clicked() {
                let mods = ui.input(|i| i.modifiers);
                if mods.shift
                    && let Some(anchor) = state.anchor.and_then(|id| shown.iter().position(|m| m.id.0 == id))
                {
                    if !mods.command {
                        state.selected.clear();
                    }
                    for m in shown.iter().take(index.max(anchor) + 1).skip(index.min(anchor)) {
                        if !state.selected.contains(&m.id.0) {
                            state.selected.insert(m.id.0);
                        }
                    }
                } else if mods.command {
                    if selected {
                        state.selected.retain(|id| *id != m.id.0);
                    } else {
                        state.selected.insert(m.id.0);
                    }
                    state.anchor = Some(m.id.0);
                } else {
                    state.selected = std::collections::BTreeSet::from([m.id.0]);
                    state.anchor = Some(m.id.0);
                }
                if state.source {
                    if let Err(e) = app.session.execute("source.setPlayhead", json!({"time":m.start.0})) {
                        app.ui.status = e.to_string();
                    }
                } else {
                    app.session.set_playhead(m.start);
                }
            }
            if response.double_clicked() && ui.input(|i| !i.modifiers.command && !i.modifiers.shift) {
                super::marker_properties::open_owned(app, m, owner);
            }
            response.clone().on_hover_text(format!("{}\n{}", m.author, m.comment));
            response.context_menu(|ui| super::misc::marker_owned_menu(app, ui, m, "markers.menu", owner));
        }
    });
    app.ui.markers_panel = state;
}
fn bulk(app: &mut FilmcraftApp, owner: Owner, ids: &std::collections::BTreeSet<u64>, command: &str, color: Option<&str>) {
    let mut p = owner.params();
    p["markers"] = json!(ids);
    if let Some(color) = color {
        p["color"] = json!(color);
    }
    if let Err(e) = app.session.execute(command, p) {
        app.ui.status = e.to_string();
    }
}
fn filter(ui: &mut egui::Ui, rgb: [u8; 3], on: bool) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(vec2(16.0, 16.0), Sense::click());
    let [r, g, b] = rgb;
    let color = Color32::from_rgb(r, g, b);
    if on {
        ui.painter().rect_filled(rect.shrink(2.0), 3.0, color);
    } else {
        ui.painter().rect_stroke(rect.shrink(2.5), 3.0, egui::Stroke::new(1.0, color), egui::StrokeKind::Inside);
    }
    response
}

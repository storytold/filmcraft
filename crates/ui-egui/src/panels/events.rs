//! Events panel (warnings and errors of commands, background jobs and the auto-save worker, from
//! the engine's `Session::log`) and Progress panel (background jobs with progress and Cancel).
//!
//! Automation ids: Events — `events.filter`, `events.filter.<info|warning|error>`,
//! `events.clearAll`, `events.row.<id>`, `events.details`; Progress — `progress.job.<id>`,
//! `progress.cancel.<id>`, `progress.showFinished`. State: `ui.panels.events` / `ui.panels.progress`.

use egui::{Align2, Color32, Rect, RichText, Sense, Stroke, pos2, vec2};
use filmcraft_engine::panels::{Level, LogEntry};
use serde_json::json;

use crate::FilmcraftApp;
use crate::theme::Tokens;

fn level_color(l: Level) -> Color32 {
    match l {
        Level::Info => Color32::from_rgb(0x57, 0x94, 0xec),
        Level::Warning => Color32::from_rgb(0xe8, 0xb3, 0x3c),
        Level::Error => Color32::from_rgb(0xe0, 0x4f, 0x4f),
    }
}

/// A small level glyph: a dot (info), a triangle (warning), a square (error), drawn in code.
fn glyph(p: &egui::Painter, c: egui::Pos2, l: Level) {
    let col = level_color(l);
    match l {
        Level::Info => {
            p.circle_filled(c, 5.0, col);
        }
        Level::Warning => {
            p.add(egui::Shape::convex_polygon(vec![c + vec2(0.0, -6.0), c + vec2(6.0, 5.0), c + vec2(-6.0, 5.0)], col, Stroke::NONE));
        }
        Level::Error => {
            p.rect_filled(Rect::from_center_size(c, vec2(10.0, 10.0)), 2.0, col);
        }
    }
    let mark = if l == Level::Info { "i" } else { "!" };
    p.text(c + vec2(0.0, if l == Level::Warning { 1.0 } else { 0.0 }), Align2::CENTER_CENTER, mark, Tokens::semibold(8.0), Color32::BLACK);
}

fn ago(ms: u64) -> String {
    let now = web_time::SystemTime::now().duration_since(web_time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(ms);
    let s = now.saturating_sub(ms) / 1000;
    match s {
        0..=4 => tl!("just now").into(),
        5..=59 => tlf!("{n} s ago", n = s),
        60..=3599 => tlf!("{n} min ago", n = s / 60),
        _ => tlf!("{n} h ago", n = s / 3600),
    }
}

pub fn events(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let st = app.ui.panels.events.clone();
    let min = Level::from_name(&st.level).unwrap_or(Level::Info);
    let entries: Vec<LogEntry> = app.session.log.entries.iter().rev().filter(|e| e.level >= min).cloned().collect();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 6.0))));
    let ui = &mut child;
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut clear = false;
    let mut level = None;
    let mut select = None;
    // toolbar: [All ▾]  counts …  [Clear All]
    ui.horizontal(|ui| {
        let label = |l: Level| match l {
            Level::Info => tl!("All Events"),
            Level::Warning => tl!("Warnings and Errors"),
            Level::Error => tl!("Errors Only"),
        };
        let r = egui::ComboBox::from_id_salt("events-filter").selected_text(label(min)).width(150.0).show_ui(ui, |ui| {
            for l in [Level::Info, Level::Warning, Level::Error] {
                let r = ui.selectable_label(min == l, label(l));
                elems.push((format!("events.filter.{}", l.label().to_ascii_lowercase()), r.rect, label(l).into()));
                if r.clicked() {
                    level = Some(l);
                }
            }
        });
        elems.push(("events.filter".into(), r.response.rect, label(min).into()));
        let log = &app.session.log;
        for l in [Level::Error, Level::Warning, Level::Info] {
            let (gr, _) = ui.allocate_exact_size(vec2(14.0, 14.0), Sense::hover());
            glyph(ui.painter(), gr.center(), l);
            ui.label(RichText::new(log.count(l).to_string()).color(t.text_dim));
        }
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            let r = ui.button(tl!("Clear All"));
            elems.push(("events.clearAll".into(), r.rect, "Clear All".into()));
            clear = r.clicked();
        });
    });
    ui.add_space(4.0);
    let details_h = if st.selected.is_some() { 70.0 } else { 0.0 };
    let list_h = (ui.available_height() - details_h).max(20.0);
    egui::ScrollArea::vertical().max_height(list_h).auto_shrink([false, false]).show(ui, |ui| {
        if entries.is_empty() {
            ui.label(RichText::new(tl!("No events.")).color(t.text_faint));
        }
        for (n, e) in entries.iter().enumerate() {
            let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 22.0), Sense::click());
            let selected = st.selected == Some(e.id);
            if selected {
                ui.painter().rect_filled(r, 0.0, t.row_selected);
            } else if resp.hovered() {
                ui.painter().rect_filled(r, 0.0, t.hover);
            } else if n % 2 == 1 {
                ui.painter().rect_filled(r, 0.0, t.row_alt);
            }
            glyph(ui.painter(), pos2(r.min.x + 10.0, r.center().y), e.level);
            let when = ago(e.time_ms);
            let right = ui.painter().text(pos2(r.max.x - 6.0, r.center().y), Align2::RIGHT_CENTER, &when, Tokens::ui(11.0), t.text_faint);
            let msg = if e.count > 1 { format!("{}  (×{})", e.message, e.count) } else { e.message.clone() };
            let clip = Rect::from_min_max(pos2(r.min.x + 22.0, r.min.y), pos2(right.min.x - 8.0, r.max.y));
            ui.painter().with_clip_rect(clip).text(pos2(clip.min.x, r.center().y), Align2::LEFT_CENTER, &msg, Tokens::ui(12.0), t.text);
            elems.push((format!("events.row.{}", e.id), r, msg));
            if resp.clicked() {
                select = Some(if selected { None } else { Some(e.id) });
            }
        }
    });
    if let Some(sel) = st.selected.and_then(|id| entries.iter().find(|e| e.id == id)) {
        let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), details_h - 4.0), Sense::hover());
        ui.painter().rect_filled(r, 3.0, t.field_bg);
        let text = format!("{} · {} · {}\n{}", crate::i18n::t(sel.level.label()), sel.source, ago(sel.time_ms), sel.message);
        let g = ui.painter().layout(text, Tokens::ui(11.5), t.text, r.width() - 12.0);
        ui.painter().with_clip_rect(r).galley(r.min + vec2(6.0, 4.0), g, t.text);
        elems.push(("events.details".into(), r, sel.message.clone()));
    }
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    if let Some(l) = level {
        app.ui.panels.events.level = l.label().to_ascii_lowercase();
    }
    if let Some(s) = select {
        app.ui.panels.events.selected = s;
    }
    app.ui.panels.events.seen = app.session.log.entries.back().map(|e| e.id).unwrap_or(0);
    if clear {
        app.ui.panels.events.selected = None;
        if let Err(e) = app.session.execute("events.clear", json!({})) {
            app.ui.status = e.to_string();
        }
    }
}

pub fn progress(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let show_finished = app.ui.panels.progress.show_finished;
    let jobs: Vec<serde_json::Value> = app.session.jobs.iter().rev().map(|j| j.to_json()).collect();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 6.0))));
    let ui = &mut child;
    let mut elems: Vec<(String, Rect, String)> = Vec::new();
    let mut cancel = None;
    let mut finished = show_finished;
    let r = ui.checkbox(&mut finished, tl!("Show finished jobs"));
    elems.push(("progress.showFinished".into(), r.rect, "Show finished jobs".into()));
    ui.add_space(4.0);
    let active = jobs.iter().filter(|j| j["finished"] != json!(true)).count();
    egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
        let mut shown = 0;
        for j in &jobs {
            let done = j["finished"] == json!(true);
            if done && !show_finished {
                continue;
            }
            shown += 1;
            let id = j["id"].as_u64().unwrap_or(0);
            let label = crate::i18n::t(j["label"].as_str().unwrap_or("Job"));
            let frac = j["progress"].as_f64().unwrap_or(0.0).clamp(0.0, 1.0) as f32;
            let (r, _) = ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
            ui.painter().rect_filled(r.shrink(1.0), 3.0, t.row_alt);
            ui.painter().text(r.min + vec2(8.0, 6.0), Align2::LEFT_TOP, label, Tokens::semibold(12.0), t.text);
            let err = j["result"]["error"].as_str();
            let status = match (done, err) {
                (true, Some(e)) if e.to_ascii_lowercase().contains("cancel") => tl!("Cancelled").to_string(),
                (true, Some(e)) => tlf!("Failed: {e}", e),
                (true, None) => tl!("Done").to_string(),
                (false, _) => {
                    let s = j["status"].as_str().unwrap_or("");
                    let n = format!("{} / {}", j["done"].as_u64().unwrap_or(0), j["total"].as_u64().unwrap_or(0));
                    let left = super::eta_suffix(j);
                    if s.is_empty() { format!("{:.0} %  ({n}){left}", frac * 100.0) } else { format!("{:.0} %  {s}{left}", frac * 100.0) }
                }
            };
            let bar = Rect::from_min_max(pos2(r.min.x + 8.0, r.min.y + 24.0), pos2(r.max.x - 36.0, r.min.y + 30.0));
            ui.painter().rect_filled(bar, 3.0, t.field_bg);
            let fill = if err.is_some() && done { t.danger } else { t.accent };
            let f = if done && err.is_none() { 1.0 } else { frac };
            ui.painter().rect_filled(Rect::from_min_max(bar.min, pos2(bar.min.x + bar.width() * f, bar.max.y)), 3.0, fill);
            ui.painter().text(
                pos2(r.min.x + 8.0, r.max.y - 4.0),
                Align2::LEFT_BOTTOM,
                &status,
                Tokens::ui(10.5),
                if err.is_some() { t.danger } else { t.text_dim },
            );
            elems.push((format!("progress.job.{id}"), r, format!("{label}: {status}")));
            if !done {
                let cr = Rect::from_center_size(pos2(r.max.x - 18.0, bar.center().y), vec2(20.0, 20.0));
                let resp = ui.interact(cr, egui::Id::new(("progress-cancel", id)), Sense::click());
                let col = if resp.hovered() { t.danger } else { t.icon };
                let s = Stroke::new(1.5, col);
                ui.painter().line_segment([cr.center() + vec2(-5.0, -5.0), cr.center() + vec2(5.0, 5.0)], s);
                ui.painter().line_segment([cr.center() + vec2(5.0, -5.0), cr.center() + vec2(-5.0, 5.0)], s);
                elems.push((format!("progress.cancel.{id}"), cr, format!("Cancel {label}")));
                if resp.on_hover_text(tl!("Cancel")).clicked() {
                    cancel = Some(id);
                }
            }
        }
        if shown == 0 {
            ui.label(RichText::new(if active == 0 { tl!("No background jobs.") } else { "" }).color(t.text_faint));
        }
    });
    for (id, r, l) in elems {
        app.auto.add(&id, r, &l);
    }
    app.ui.panels.progress.show_finished = finished;
    if let Some(id) = cancel
        && let Err(e) = app.session.execute("jobs.cancel", json!({"job": id}))
    {
        app.ui.status = e.to_string();
    }
    if active > 0 {
        ui.ctx().request_repaint_after(std::time::Duration::from_millis(250));
    }
}

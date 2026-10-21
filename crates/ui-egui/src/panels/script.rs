//! Lined Script panel. The document is project data; this file keeps only view state.
//! Rows, take coverage and script marks share one vertical scroll coordinate.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::{ItemId, ItemKind, Label, ScriptDocument, ScriptLine, ScriptTake};
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::io::Read;

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::native_dialogs::FileDialog;
use crate::theme::Tokens;

const MIN_ROW_H: f32 = 30.0;
const MAX_VISUAL_LINES: f32 = 12.0;
const TAKE_W: f32 = 112.0;
const HEADER_H: f32 = 84.0;
const REBIND_WARNING: &str = "replacement is not identity-confirmed as the same footage;";
const REBIND_SPAN_WARNING: &str = "replacement source span excludes existing marks;";

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptUi {
    pub item: Option<u64>,
    /// Two stable line IDs, inclusive; order records the anchor and extension direction.
    pub selected_lines: Option<(u64, u64)>,
    pub selected_slate: Option<u64>,
    pub selected_take: Option<u64>,
    pub edit_line: Option<u64>,
    pub edit_text: String,
    pub label_line: Option<u64>,
    pub scene_text: String,
    pub page_text: String,
    pub search: String,
    /// Only the take columns move horizontally; script text stays in view.
    pub take_scroll: f32,
    pub pending_rebind: Option<(u64, u64)>,
    /// Source selected by Project ▸ Find in Script; a chooser is shown for multiple matches.
    pub find_source: Option<u64>,
    /// Request to bring this logical line into the viewport on the next frame.
    #[serde(skip)]
    pub scroll_to_line: Option<u64>,
    #[serde(skip)]
    layout_item: Option<u64>,
    #[serde(skip)]
    layout_revision: u64,
    #[serde(skip)]
    layout_width: u32,
    #[serde(skip)]
    row_tops: Vec<f32>,
}

#[derive(Clone, Debug)]
enum Action {
    Command(&'static str, Value),
    Create,
    Import,
    FindInProject(ItemId),
    Rebind { take: u64, source: u64 },
}

fn command(app: &mut FilmcraftApp, id: &str, params: Value) -> Option<Value> {
    match app.session.execute(id, params) {
        Ok(value) => Some(value),
        Err(error) => {
            app.ui.status = error.to_string();
            None
        }
    }
}

fn run_actions(app: &mut FilmcraftApp, actions: Vec<Action>) {
    for action in actions {
        match action {
            Action::Command(id, params) => {
                let _ = command(app, id, params);
            }
            Action::Create => {
                if let Some(value) = command(app, "script.create", json!({"name": "Untitled Script"})) {
                    app.ui.script.item = value["item"].as_u64();
                    app.ui.script.selected_slate = None;
                    app.ui.script.selected_take = None;
                    app.ui.script.selected_lines = None;
                }
            }
            Action::Import => import_text(app),
            Action::FindInProject(source) => {
                if command(app, "project.select", json!({"items": [source.0]})).is_some() {
                    app.reveal_in_project(source);
                }
            }
            Action::Rebind { take, source } => {
                let params = json!({"item": app.ui.script.item, "take": take, "source": source});
                match app.session.execute("script.rebindTake", params) {
                    Ok(_) => app.ui.script.pending_rebind = None,
                    Err(error) if error.to_string().contains(REBIND_WARNING) || error.to_string().contains(REBIND_SPAN_WARNING) => {
                        app.ui.script.pending_rebind = Some((take, source));
                    }
                    Err(error) => app.ui.status = error.to_string(),
                }
            }
        }
    }
}

fn import_text(app: &mut FilmcraftApp) {
    app.pick_ui(FileDialog::open_file("Plain-text script", &["txt"]), |app, paths| {
        let Some(path) = paths.into_iter().next() else { return };
        let file = std::path::Path::new(&path);
        let input = match std::fs::File::open(file) {
            Ok(file) => file,
            Err(error) => {
                app.ui.status = format!("Cannot read script: {error}");
                return;
            }
        };
        let mut bytes = Vec::new();
        if let Err(error) = input.take(ScriptDocument::MAX_TEXT_BYTES as u64 + 1).read_to_end(&mut bytes) {
            app.ui.status = format!("Cannot read script: {error}");
            return;
        }
        if bytes.len() > ScriptDocument::MAX_TEXT_BYTES {
            app.ui.status = "Script text exceeds the 4 MiB import limit".into();
            return;
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                app.ui.status = format!("Cannot read script as UTF-8 text: {error}");
                return;
            }
        };
        let name = file.file_stem().and_then(std::ffi::OsStr::to_str).filter(|name| !name.is_empty()).unwrap_or("Untitled Script");
        if let Some(value) = command(app, "script.importText", json!({"name": name, "text": text})) {
            app.ui.script.item = value["item"].as_u64();
            app.ui.script.selected_lines = None;
            app.ui.script.selected_slate = None;
            app.ui.script.selected_take = None;
        }
    });
}

fn selected_range(doc: &ScriptDocument, pair: Option<(u64, u64)>) -> Option<(usize, usize)> {
    let (a, b) = pair?;
    let a = doc.lines.iter().position(|line| line.id.0 == a)?;
    let b = doc.lines.iter().position(|line| line.id.0 == b)?;
    Some((a.min(b), a.max(b)))
}

fn take_by_id(doc: &ScriptDocument, id: Option<u64>) -> Option<&ScriptTake> {
    let id = id?;
    doc.slates.iter().flat_map(|slate| &slate.takes).find(|take| take.id.0 == id)
}

fn current_line<'a>(doc: &'a ScriptDocument, state: &ScriptUi) -> Option<&'a ScriptLine> {
    let id = state.selected_lines?.1;
    doc.lines.iter().find(|line| line.id.0 == id)
}

fn select_line(state: &mut ScriptUi, line: &ScriptLine, extend: bool) {
    state.selected_lines = Some(if extend { (state.selected_lines.map_or(line.id.0, |pair| pair.0), line.id.0) } else { (line.id.0, line.id.0) });
    if state.label_line != Some(line.id.0) {
        state.label_line = Some(line.id.0);
        state.scene_text = line.scene.clone().unwrap_or_default();
        state.page_text = line.page.clone().unwrap_or_default();
    }
}

fn line_text_x(line: &ScriptLine, left_x: f32) -> f32 {
    left_x + 42.0 + if line.page.is_some() { 53.0 } else { 0.0 } + if line.scene.is_some() { 72.0 } else { 0.0 }
}

/// Measured once per document revision and width; a 4 MiB script may have 50,000 rows, so only
/// visible rows are painted while their cached prefix heights keep the candles in exact alignment.
fn row_layout(app: &mut FilmcraftApp, ui: &egui::Ui, doc: &ScriptDocument, left_w: f32) {
    let width = left_w.round().max(1.0) as u32;
    if app.ui.script.layout_item == app.ui.script.item
        && app.ui.script.layout_revision == app.session.revision
        && app.ui.script.layout_width == width
        && app.ui.script.row_tops.len() == doc.lines.len().saturating_add(1)
    {
        return;
    }
    let mut tops = Vec::with_capacity(doc.lines.len().saturating_add(1));
    tops.push(0.0);
    let mut total = 0.0_f32;
    for line in &doc.lines {
        let text_w = (left_w - (line_text_x(line, 0.0) + 7.0)).max(48.0);
        let galley = ui.painter().layout(line.text.clone(), Tokens::ui(12.5), app.tokens.text, text_w);
        let height = (galley.size().y + 8.0).clamp(MIN_ROW_H, MIN_ROW_H * MAX_VISUAL_LINES);
        total += height;
        tops.push(total);
    }
    app.ui.script.row_tops = tops;
    app.ui.script.layout_item = app.ui.script.item;
    app.ui.script.layout_revision = app.session.revision;
    app.ui.script.layout_width = width;
}

fn first_visible(tops: &[f32], y: f32) -> usize {
    tops.partition_point(|top| *top <= y).saturating_sub(1)
}

fn header(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, scripts: &[(ItemId, String)], doc: Option<&ScriptDocument>, actions: &mut Vec<Action>) {
    let t = app.tokens;
    let narrow = rect.height() > 80.0;
    let mut top = ui.new_child(
        egui::UiBuilder::new().max_rect(Rect::from_min_size(rect.min + vec2(8.0, 4.0), vec2((rect.width() - 16.0).max(1.0), if narrow { 61.0 } else { 30.0 }))),
    );
    top.horizontal_wrapped(|ui| {
        let name = app.ui.script.item.and_then(|id| scripts.iter().find(|entry| entry.0.0 == id).map(|entry| entry.1.as_str())).unwrap_or("Choose a script");
        egui::ComboBox::from_id_salt("script.document").selected_text(name).width(180.0).show_ui(ui, |ui| {
            for (id, name) in scripts {
                let response = ui.selectable_label(app.ui.script.item == Some(id.0), name);
                app.auto.add(&format!("script.document.{}", id.0), response.rect, name);
                if response.clicked() {
                    app.ui.script.item = Some(id.0);
                    app.ui.script.selected_lines = None;
                    app.ui.script.selected_slate = None;
                    app.ui.script.selected_take = None;
                }
            }
        });
        for (key, text, action) in [("new", "New", Action::Create), ("import", "Import Text…", Action::Import)] {
            let response = ui.button(text);
            app.auto.add(&format!("script.{key}"), response.rect, text);
            if response.clicked() {
                actions.push(action);
            }
        }
        if doc.is_some() {
            let response = ui.button("Add Line");
            app.auto.add("script.line.add", response.rect, "Add Line");
            if response.clicked() {
                let after = app.ui.script.selected_lines.map(|pair| pair.1).or_else(|| doc.and_then(|d| d.lines.last()).map(|line| line.id.0));
                actions.push(Action::Command("script.insertLine", json!({"item": app.ui.script.item, "after": after, "text": ""})));
            }
        }
    });
    let mut search = ui.new_child(
        egui::UiBuilder::new()
            .max_rect(Rect::from_min_size(rect.min + vec2(8.0, if narrow { 69.0 } else { 38.0 }), vec2((rect.width() - 16.0).max(1.0), 32.0))),
    );
    search.horizontal(|ui| {
        ui.label("Find in Script");
        let response = ui.add(egui::TextEdit::singleline(&mut app.ui.script.search).hint_text("Search text").desired_width(180.0));
        app.auto.add_response("script.search", &response, "Find in Script");
        if response.changed()
            && let Some(doc) = doc
            && let Some(line) = doc.lines.iter().find(|line| line.text.to_lowercase().contains(&app.ui.script.search.to_lowercase()))
            && !app.ui.script.search.is_empty()
        {
            select_line(&mut app.ui.script, line, false);
            app.ui.script.scroll_to_line = Some(line.id.0);
        }
        if let Some(source) = app.ui.script.find_source {
            let matches = source_matches(app, ItemId(source));
            let chooser = egui::ComboBox::from_id_salt("script.findSource").selected_text(format!("Choose linked take ({})", matches.len())).width(190.0);
            chooser.show_ui(ui, |ui| {
                for entry in matches {
                    let response = ui.selectable_label(false, &entry.label);
                    app.auto.add(&format!("script.findSource.{}.{}", entry.item.0, entry.take), response.rect, &entry.label);
                    if response.clicked() {
                        select_source_match(&mut app.ui.script, &entry);
                        app.ui.script.find_source = None;
                    }
                }
            });
        }
        if let Some(line) = doc.and_then(|doc| current_line(doc, &app.ui.script)) {
            ui.label(format!(
                "Line {} of {}",
                doc.map_or(0, |d| d.lines.iter().position(|candidate| candidate.id == line.id).map_or(0, |index| index + 1)),
                doc.map_or(0, |d| d.lines.len())
            ));
        }
    });
    ui.painter().line_segment([pos2(rect.min.x, rect.max.y - 1.0), pos2(rect.max.x, rect.max.y - 1.0)], Stroke::new(1.0, t.separator));
}

/// A selected line's optional scene/page labels and the selected take's commands.
fn tools(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, doc: &ScriptDocument, actions: &mut Vec<Action>) {
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink2(vec2(8.0, 3.0))));
    let Some(line) = current_line(doc, &app.ui.script) else {
        child.label("Select a line, then drag a clip from Project onto it to create a take.");
        return;
    };
    let line_id = line.id.0;
    if app.ui.script.label_line != Some(line_id) {
        app.ui.script.label_line = Some(line_id);
        app.ui.script.scene_text = line.scene.clone().unwrap_or_default();
        app.ui.script.page_text = line.page.clone().unwrap_or_default();
    }
    child.horizontal_wrapped(|ui| {
        ui.label("Scene");
        let scene = ui.add(egui::TextEdit::singleline(&mut app.ui.script.scene_text).desired_width(76.0));
        app.auto.add_response("script.scene", &scene, "Scene");
        ui.label("Page");
        let page = ui.add(egui::TextEdit::singleline(&mut app.ui.script.page_text).desired_width(58.0));
        app.auto.add_response("script.page", &page, "Page");
        let apply = ui.button("Set");
        app.auto.add("script.labels.set", apply.rect, "Set Scene and Page");
        if apply.clicked() || (scene.lost_focus() || page.lost_focus()) && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            actions.push(Action::Command(
                "script.setLabel",
                json!({"item": app.ui.script.item, "line": line_id,
                "scene": if app.ui.script.scene_text.trim().is_empty() { None } else { Some(app.ui.script.scene_text.trim()) },
                "page": if app.ui.script.page_text.trim().is_empty() { None } else { Some(app.ui.script.page_text.trim()) }}),
            ));
        }
        if let Some(take) = take_by_id(doc, app.ui.script.selected_take) {
            ui.separator();
            endpoint_buttons(app, ui, doc, take, actions);
            let is_open = app.session.state.source_item == take.source;
            let in_candle = doc
                .line_range(take.start_line, take.end_line)
                .is_some_and(|(start, end)| doc.line_index(line.id).is_some_and(|current| (start..=end).contains(&current)));
            let mark = ui.add_enabled(is_open && in_candle, egui::Button::new("Mark")).on_hover_text("Mark this line at the Source playhead");
            app.auto.add("script.mark.add", mark.rect, "Mark this line at the Source playhead");
            if mark.clicked() {
                actions.push(Action::Command(
                    "script.addMark",
                    json!({"item": app.ui.script.item, "take": take.id.0,
                    "line": line_id, "sourceTime": app.session.state.source_playhead.0}),
                ));
            }
            if !is_open && take.source.is_some() {
                let open = ui.button("Open Take");
                app.auto.add("script.take.open", open.rect, "Open take in Source monitor");
                if open.clicked() {
                    actions.push(Action::Command("source.open", json!({"item": take.source.map(|id| id.0)})));
                }
            }
        }
    });
    if app.ui.focused == PanelKind::Script
        && !ui.ctx().text_edit_focused()
        && let Some(take) = take_by_id(doc, app.ui.script.selected_take)
    {
        let move_key = ui.input(|input| {
            input.events.iter().find_map(|event| {
                let egui::Event::Key { key, pressed: true, modifiers, .. } = event else { return None };
                if modifiers.alt && !modifiers.command && !modifiers.ctrl {
                    match key {
                        egui::Key::ArrowUp => Some((!modifiers.shift, -1_i32)),
                        egui::Key::ArrowDown => Some((!modifiers.shift, 1_i32)),
                        _ => None,
                    }
                } else {
                    None
                }
            })
        });
        if let Some((top, delta)) = move_key {
            let start = doc.lines.iter().position(|line| line.id == take.start_line);
            let end = doc.lines.iter().position(|line| line.id == take.end_line);
            if let (Some(start), Some(end)) = (start, end) {
                let idx = if top { start } else { end };
                let next = if delta < 0 { idx.checked_sub(1) } else { idx.checked_add(1) };
                if let Some(line) = next.filter(|n| *n < doc.lines.len() && if top { *n <= end } else { *n >= start }).and_then(|n| doc.lines.get(n)) {
                    actions.push(Action::Command(
                        "script.setCoverage",
                        json!({"item": app.ui.script.item, "take": take.id.0,
                        "startLine": if top { line.id.0 } else { take.start_line.0 },
                        "endLine": if top { take.end_line.0 } else { line.id.0 }}),
                    ));
                }
            }
        }
    }
}

fn endpoint_buttons(app: &mut FilmcraftApp, ui: &mut egui::Ui, doc: &ScriptDocument, take: &ScriptTake, actions: &mut Vec<Action>) {
    let start = doc.lines.iter().position(|line| line.id == take.start_line);
    let end = doc.lines.iter().position(|line| line.id == take.end_line);
    let (Some(start), Some(end)) = (start, end) else { return };
    for (key, label, top, delta) in
        [("topUp", "Top ↑", true, -1), ("topDown", "Top ↓", true, 1), ("bottomUp", "Bottom ↑", false, -1), ("bottomDown", "Bottom ↓", false, 1)]
    {
        let idx = if top { start } else { end };
        let next = if delta < 0 { idx.checked_sub(1) } else { idx.checked_add(1) };
        let enabled = next.is_some_and(|n| n < doc.lines.len() && if top { n <= end } else { n >= start });
        let response = ui.add_enabled(enabled, egui::Button::new(label)).on_hover_text(if top {
            "Alt+↑/↓ adjusts the top"
        } else {
            "Alt+Shift+↑/↓ adjusts the bottom"
        });
        app.auto.add(&format!("script.take.{key}"), response.rect, label);
        if response.clicked()
            && let Some(line) = next.and_then(|n| doc.lines.get(n))
        {
            actions.push(Action::Command(
                "script.setCoverage",
                json!({"item": app.ui.script.item, "take": take.id.0,
                "startLine": if top { line.id.0 } else { take.start_line.0 },
                "endLine": if top { take.end_line.0 } else { line.id.0 }}),
            ));
        }
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    ui.painter().rect_filled(rect, 0.0, t.panel_bg);
    let scripts: Vec<(ItemId, String)> =
        app.session.project.items.values().filter(|item| item.as_script().is_some()).map(|item| (item.id, item.name.clone())).collect();
    if app.ui.script.item.is_none_or(|id| !scripts.iter().any(|entry| entry.0.0 == id)) {
        app.ui.script.item = scripts.first().map(|entry| entry.0.0);
    }
    let doc = app.ui.script.item.and_then(|id| app.session.project.item(ItemId(id))).and_then(|item| match &item.kind {
        ItemKind::Script(doc) => Some(std::sync::Arc::clone(doc)),
        _ => None,
    });
    let mut actions = Vec::new();
    let header_rect = Rect::from_min_size(rect.min, vec2(rect.width(), if rect.width() < 520.0 { 105.0 } else { 74.0 }));
    header(app, ui, header_rect, &scripts, doc.as_deref(), &mut actions);
    if let Some(doc) = doc.as_deref() {
        let tools_rect = Rect::from_min_size(pos2(rect.min.x, header_rect.max.y), vec2(rect.width(), if rect.width() < 790.0 { 75.0 } else { 37.0 }));
        tools(app, ui, tools_rect, doc, &mut actions);
        let body = Rect::from_min_max(pos2(rect.min.x, tools_rect.max.y), rect.max);
        content(app, ui, body, doc, &mut actions);
    } else {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, "Create a script or import a plain-text script.", Tokens::ui(14.0), t.text_dim);
    }
    run_actions(app, actions);
    rebind_confirmation(app, ui.ctx());
}

fn rebind_confirmation(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some((take, source)) = app.ui.script.pending_rebind else { return };
    egui::Window::new("Replace take?").collapsible(false).resizable(false).show(ctx, |ui| {
        ui.label("This replacement could be different footage. Old script marks would point to the wrong frames.");
        ui.label("Replace the take and clear its marks?");
        ui.horizontal(|ui| {
            let cancel = ui.button("Cancel");
            app.auto.add("script.rebind.cancel", cancel.rect, "Cancel take replacement");
            if cancel.clicked() {
                app.ui.script.pending_rebind = None;
            }
            let replace = ui.button("Replace and Clear Marks");
            app.auto.add("script.rebind.confirm", replace.rect, "Replace take and clear marks");
            if replace.clicked() {
                let item = app.ui.script.item;
                if command(app, "script.rebindTake", json!({"item": item, "take": take, "source": source, "discardMarks": true})).is_some() {
                    app.ui.script.pending_rebind = None;
                }
            }
        });
    });
}

/// Reveal the first script take using a selected Project item; exposed as a UI-only command.
pub fn find_source(app: &mut FilmcraftApp, source: ItemId) -> Result<Value, String> {
    let matches = source_matches(app, source);
    if matches.is_empty() {
        return Err("This clip is not linked to a script".into());
    }
    if matches.len() == 1 {
        select_source_match(&mut app.ui.script, &matches[0]);
    } else {
        app.ui.script.find_source = Some(source.0);
    }
    app.show_panel(PanelKind::Script);
    Ok(json!({"matches": matches.len(), "chooser": matches.len() > 1}))
}

struct SourceMatch {
    item: ItemId,
    take: u64,
    start: u64,
    end: u64,
    label: String,
}

fn source_matches(app: &FilmcraftApp, source: ItemId) -> Vec<SourceMatch> {
    let mut matches = Vec::new();
    for item in app.session.project.items.values() {
        let Some(doc) = item.as_script() else { continue };
        for slate in &doc.slates {
            for take in &slate.takes {
                if take.source == Some(source) {
                    matches.push(SourceMatch {
                        item: item.id,
                        take: take.id.0,
                        start: take.start_line.0,
                        end: take.end_line.0,
                        label: format!("{} / {} / {}", item.name, slate.name, take.source_name),
                    });
                }
            }
        }
    }
    matches.sort_by_key(|entry| (entry.item.0, entry.take));
    matches
}

fn select_source_match(state: &mut ScriptUi, entry: &SourceMatch) {
    state.item = Some(entry.item.0);
    state.selected_take = Some(entry.take);
    state.selected_lines = Some((entry.start, entry.end));
    state.scroll_to_line = Some(entry.start);
}

fn content(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, doc: &ScriptDocument, actions: &mut Vec<Action>) {
    let t = app.tokens;
    let left_w = (rect.width() * 0.49).clamp(215.0, 490.0).min((rect.width() - 85.0).max(100.0));
    let left_w = left_w.max(100.0);
    row_layout(app, ui, doc, left_w);
    let line_indices: HashMap<u64, usize> = doc.lines.iter().enumerate().map(|(index, line)| (line.id.0, index)).collect();
    let left = Rect::from_min_size(rect.min, vec2(left_w, rect.height()));
    let right = Rect::from_min_max(pos2(left.max.x, rect.min.y), rect.max);
    let columns: Vec<_> = doc
        .slates
        .iter()
        .flat_map(|slate| {
            if slate.takes.is_empty() {
                vec![(slate.id.0, None, slate.name.as_str())]
            } else {
                slate.takes.iter().map(|take| (slate.id.0, Some(take), slate.name.as_str())).collect()
            }
        })
        .collect();
    let take_total = columns.len() as f32 * TAKE_W;
    let max_scroll = (take_total - right.width()).max(0.0);
    app.ui.script.take_scroll = app.ui.script.take_scroll.clamp(0.0, max_scroll);
    let head = Rect::from_min_size(rect.min, vec2(rect.width(), HEADER_H));
    ui.painter().rect_filled(head, 0.0, t.field_bg);
    ui.painter().text(pos2(left.min.x + 8.0, head.min.y + 16.0), Align2::LEFT_CENTER, "SCRIPT LINES", Tokens::semibold(11.0), t.text_dim);
    ui.painter().text(pos2(right.min.x + 8.0, head.min.y + 16.0), Align2::LEFT_CENTER, "SETUPS / TAKES", Tokens::semibold(11.0), t.text_dim);
    let right_clip = Rect::from_min_max(pos2(right.min.x, head.min.y + 20.0), pos2(right.max.x, head.max.y - 22.0));
    for (index, (slate, take, name)) in columns.iter().enumerate() {
        let x = right.min.x + index as f32 * TAKE_W - app.ui.script.take_scroll;
        let cell = Rect::from_min_size(pos2(x + 3.0, head.min.y + 22.0), vec2(TAKE_W - 6.0, 37.0));
        if !cell.intersects(right_clip) {
            continue;
        }
        let selected = take.is_some_and(|take| app.ui.script.selected_take == Some(take.id.0));
        ui.painter().with_clip_rect(right_clip).rect_filled(cell, 3.0, if selected { t.hover } else { t.panel_bg });
        if let Some(take) = take
            && let Some(source) = take.source.and_then(|id| app.session.project.item(id))
        {
            let thumb_item = app.session.project.resolve_media(source.id).map_or(source.id, |value| value.0);
            if let Some((texture, size)) = app.thumbnail(ui.ctx(), thumb_item, Tick::ZERO, 96) {
                let thumbnail = Rect::from_min_size(cell.min + vec2(3.0, 3.0), vec2(51.0, 30.0));
                ui.painter().with_clip_rect(right_clip).image(
                    texture,
                    super::monitor::fit(thumbnail, size.x, size.y),
                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                    Color32::WHITE,
                );
            }
        }
        let title = take.map_or(*name, |take| {
            take.source
                .and_then(|id| app.session.project.item(id))
                .and_then(|item| item.metadata.get("Take"))
                .filter(|value| !value.trim().is_empty())
                .map_or(take.source_name.as_str(), String::as_str)
        });
        ui.painter().with_clip_rect(right_clip).text(
            pos2(cell.min.x + 57.0, cell.center().y),
            Align2::LEFT_CENTER,
            truncate(title, 9),
            Tokens::ui(10.0),
            t.text,
        );
        let response = ui.interact(cell.intersect(right_clip), egui::Id::new(("script.header", *slate, take.map(|take| take.id.0))), Sense::click());
        app.auto.add(&format!("script.slate.{slate}.take.{}", take.map_or(0, |take| take.id.0)), response.rect, title);
        if response.clicked() {
            app.ui.script.selected_slate = Some(*slate);
            app.ui.script.selected_take = take.map(|take| take.id.0);
        }
        if let Some(take) = take {
            response.context_menu(|ui| take_menu(app, ui, take, actions));
        }
        if response.hovered()
            && ui.input(|input| input.pointer.any_released())
            && let Some(source) = super::dragged_project_item(ui)
        {
            if let Some(take) = take {
                actions.push(Action::Rebind { take: take.id.0, source: source.0 });
            } else if let Some((start, end)) = selected_range(doc, app.ui.script.selected_lines)
                && let (Some(a), Some(b)) = (doc.lines.get(start), doc.lines.get(end))
            {
                actions.push(Action::Command(
                    "script.addTake",
                    json!({"item": app.ui.script.item, "slate": slate, "source": source.0, "startLine": a.id.0, "endLine": b.id.0}),
                ));
            }
            super::clear_drag(ui);
        }
    }
    let slider_r = Rect::from_min_max(pos2(right.min.x + 8.0, head.max.y - 20.0), pos2(right.max.x - 8.0, head.max.y - 2.0));
    if slider_r.width() > 48.0 && max_scroll > 0.0 {
        let response = ui.put(slider_r, egui::Slider::new(&mut app.ui.script.take_scroll, 0.0..=max_scroll).show_value(false));
        app.auto.add("script.takeScroll", response.rect, "Scroll take columns");
    }
    ui.painter().line_segment([pos2(left.max.x, rect.min.y), pos2(left.max.x, rect.max.y)], Stroke::new(1.0, t.separator));
    let body = Rect::from_min_max(pos2(rect.min.x, head.max.y), rect.max);
    if body.height() < MIN_ROW_H {
        return;
    }
    let mut scroll_ui = ui.new_child(egui::UiBuilder::new().max_rect(body));
    let mut drop_line = None;
    let mut clicked_mark = None;
    let range = selected_range(doc, app.ui.script.selected_lines);
    let tops = app.ui.script.row_tops.clone();
    egui::ScrollArea::vertical().id_salt("script.rows").auto_shrink([false, false]).show_viewport(&mut scroll_ui, |ui, viewport| {
        let total = tops.last().copied().unwrap_or_default();
        ui.set_height(total);
        let base_y = ui.max_rect().top();
        let start = first_visible(&tops, viewport.min.y).min(doc.lines.len());
        let end = first_visible(&tops, viewport.max.y).saturating_add(2).min(doc.lines.len());
        for index in start..end {
            let (Some(line), Some(&y0), Some(&y1)) = (doc.lines.get(index), tops.get(index), tops.get(index + 1)) else { continue };
            let row = Rect::from_min_size(pos2(ui.max_rect().left(), base_y + y0), vec2(body.width().max(1.0), y1 - y0));
            if row.intersects(body) {
                row_view(app, ui, row, left_w, index, line, &columns, &line_indices, range, &mut drop_line, &mut clicked_mark, actions);
            }
        }
        if let Some(target) = app.ui.script.scroll_to_line
            && let Some(index) = doc.lines.iter().position(|line| line.id.0 == target)
            && let (Some(&y0), Some(&y1)) = (tops.get(index), tops.get(index + 1))
        {
            let target_rect = Rect::from_min_max(pos2(ui.max_rect().left(), base_y + y0), pos2(ui.max_rect().right(), base_y + y1));
            ui.scroll_to_rect(target_rect, Some(egui::Align::Center));
            app.ui.script.scroll_to_line = None;
        }
    });
    if let Some((take, mark)) = clicked_mark {
        actions.push(Action::Command("script.cueMark", json!({"item": app.ui.script.item, "take": take, "mark": mark})));
    }
    if let Some(line_id) = drop_line
        && let Some(source) = super::dragged_project_item(ui)
    {
        let (start, end) = selected_range(doc, app.ui.script.selected_lines)
            .and_then(|(a, b)| {
                let i = doc.lines.iter().position(|line| line.id.0 == line_id)?;
                (a..=b).contains(&i).then_some((a, b))
            })
            .unwrap_or_else(|| {
                let i = doc.lines.iter().position(|line| line.id.0 == line_id).unwrap_or(0);
                (i, i)
            });
        if let (Some(a), Some(b)) = (doc.lines.get(start), doc.lines.get(end)) {
            let mut params = json!({"item": app.ui.script.item, "source": source.0, "startLine": a.id.0, "endLine": b.id.0});
            if let Some(slate) = app.ui.script.selected_slate {
                params["slate"] = json!(slate);
            }
            actions.push(Action::Command("script.addTake", params));
        }
        super::clear_drag(ui);
    }
}

#[allow(clippy::too_many_arguments)]
fn row_view(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    row: Rect,
    left_w: f32,
    index: usize,
    line: &ScriptLine,
    columns: &[(u64, Option<&ScriptTake>, &str)],
    line_indices: &HashMap<u64, usize>,
    range: Option<(usize, usize)>,
    drop_line: &mut Option<u64>,
    clicked_mark: &mut Option<(u64, u64)>,
    actions: &mut Vec<Action>,
) {
    let t = app.tokens;
    let left = Rect::from_min_size(row.min, vec2(left_w, row.height()));
    let right = Rect::from_min_max(pos2(left.max.x, row.min.y), row.max);
    let selected = range.is_some_and(|(a, b)| (a..=b).contains(&index));
    if selected {
        ui.painter().rect_filled(left, 0.0, t.hover);
    }
    if index % 2 == 1 {
        ui.painter().rect_filled(right, 0.0, Color32::from_black_alpha(12));
    }
    let interaction = ui.interact(left, egui::Id::new(("script.line", line.id.0)), Sense::click_and_drag());
    app.auto.add(&format!("script.line.{}", line.id.0), interaction.rect, &line.text);
    if interaction.clicked() {
        select_line(&mut app.ui.script, line, ui.input(|input| input.modifiers.shift));
    }
    if interaction.double_clicked() {
        app.ui.script.edit_line = Some(line.id.0);
        app.ui.script.edit_text = line.text.clone();
    }
    if interaction.hovered() && ui.input(|input| input.pointer.any_released()) && super::dragged_project_item(ui).is_some() {
        *drop_line = Some(line.id.0);
    }
    interaction.clone().context_menu(|ui| {
        if ui.button("Edit Line").clicked() {
            app.ui.script.edit_line = Some(line.id.0);
            app.ui.script.edit_text = line.text.clone();
            ui.close();
        }
        if ui.button("Insert Line Below").clicked() {
            actions.push(Action::Command("script.insertLine", json!({"item": app.ui.script.item, "after": line.id.0, "text": ""})));
            ui.close();
        }
        if ui.button("Delete Line").clicked() {
            actions.push(Action::Command("script.deleteLine", json!({"item": app.ui.script.item, "line": line.id.0})));
            ui.close();
        }
    });
    ui.painter().text(pos2(left.min.x + 6.0, left.center().y), Align2::LEFT_CENTER, format!("{}", index + 1), Tokens::ui(10.0), t.text_dim);
    let mut text_x = left.min.x + 42.0;
    if let Some(page) = &line.page {
        ui.painter().text(pos2(text_x, left.center().y), Align2::LEFT_CENTER, truncate(page, 8), Tokens::semibold(10.0), t.accent);
        text_x += 53.0;
    }
    if let Some(scene) = &line.scene {
        ui.painter().text(pos2(text_x, left.center().y), Align2::LEFT_CENTER, truncate(scene, 10), Tokens::semibold(10.0), t.text_dim);
        text_x += 72.0;
    }
    if app.ui.script.edit_line == Some(line.id.0) {
        let edit_rect = Rect::from_min_max(pos2(left.min.x + 38.0, left.min.y + 2.0), pos2(left.max.x - 6.0, left.max.y - 2.0));
        let response = ui.put(edit_rect, egui::TextEdit::singleline(&mut app.ui.script.edit_text).desired_width(edit_rect.width()));
        app.auto.add_response(&format!("script.line.{}.edit", line.id.0), &response, "Edit script line");
        if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
            actions.push(Action::Command("script.editLine", json!({"item": app.ui.script.item, "line": line.id.0, "text": app.ui.script.edit_text})));
            app.ui.script.edit_line = None;
        }
    } else {
        let text_rect = Rect::from_min_max(pos2(text_x, left.min.y + 3.0), pos2(left.max.x - 7.0, left.max.y - 2.0));
        let galley = ui.painter().layout(line.text.clone(), Tokens::ui(12.5), t.text, text_rect.width().max(48.0));
        let clipped = galley.size().y > text_rect.height();
        ui.painter().with_clip_rect(text_rect).galley(text_rect.min, galley, t.text);
        if clipped {
            interaction.on_hover_text(&line.text);
        }
    }
    let clip = Rect::from_min_max(right.min, row.max);
    for (column, (_slate, maybe_take, _name)) in columns.iter().enumerate() {
        let Some(take) = maybe_take else { continue };
        let x = right.min.x + column as f32 * TAKE_W - app.ui.script.take_scroll;
        if x + TAKE_W < right.min.x || x > right.max.x {
            continue;
        }
        let (Some(&start), Some(&end)) = (line_indices.get(&take.start_line.0), line_indices.get(&take.end_line.0)) else { continue };
        if !(start..=end).contains(&index) {
            continue;
        }
        let rgb = app.session.prefs.labels.rgb(take.color);
        let color = Color32::from_rgb(rgb[0], rgb[1], rgb[2]);
        let cx = x + TAKE_W * 0.5;
        let stroke = Stroke::new(if app.ui.script.selected_take == Some(take.id.0) { 6.0 } else { 4.0 }, color);
        // Configurable label colors include a deep blue that disappears against the dark theme.
        // A text-colored border makes every candle and offscreen zigzag readable in either theme.
        let halo = Stroke::new(stroke.width + 4.0, t.text_dim);
        let paint = ui.painter().with_clip_rect(clip);
        if take.offscreen_lines.contains(&line.id) {
            let points =
                [pos2(cx, row.min.y), pos2(cx - 5.0, row.min.y + row.height() * 0.33), pos2(cx + 5.0, row.min.y + row.height() * 0.66), pos2(cx, row.max.y)];
            paint.add(egui::Shape::line(points.to_vec(), halo));
            paint.add(egui::Shape::line(points.to_vec(), stroke));
        } else {
            paint.line_segment([pos2(cx, row.min.y), pos2(cx, row.max.y)], halo);
            paint.line_segment([pos2(cx, row.min.y), pos2(cx, row.max.y)], stroke);
        }
        if index == start || index == end {
            let ends = [
                pos2(cx - 12.0, if index == start { row.min.y + 2.0 } else { row.max.y - 2.0 }),
                pos2(cx + 12.0, if index == start { row.min.y + 2.0 } else { row.max.y - 2.0 }),
            ];
            paint.line_segment(ends, Stroke::new(6.0, t.text_dim));
            paint.line_segment(ends, Stroke::new(2.0, color));
        }
        let candle = Rect::from_min_size(pos2(x + 5.0, row.min.y), vec2(TAKE_W - 10.0, row.height()));
        let response = ui.interact(candle.intersect(clip), egui::Id::new(("script.candle", take.id.0, line.id.0)), Sense::click());
        app.auto.add(&format!("script.take.{}.line.{}", take.id.0, line.id.0), response.rect, "Take coverage");
        if response.clicked() {
            app.ui.script.selected_take = Some(take.id.0);
        }
        if response.hovered()
            && ui.input(|input| input.pointer.any_released())
            && let Some(source) = super::dragged_project_item(ui)
        {
            actions.push(Action::Rebind { take: take.id.0, source: source.0 });
            super::clear_drag(ui);
        }
        if let Some(mark) = take.marks.iter().find(|mark| mark.line == line.id) {
            let mark_rect = Rect::from_center_size(pos2(cx + 18.0, row.center().y), vec2(14.0, 14.0));
            ui.painter().with_clip_rect(clip).circle_filled(mark_rect.center(), 5.0, t.accent);
            let response = ui.interact(mark_rect.intersect(clip), egui::Id::new(("script.mark", mark.id.0)), Sense::click());
            app.auto.add(&format!("script.mark.{}", mark.id.0), response.rect, "Cue script mark");
            if response.clicked() {
                *clicked_mark = Some((take.id.0, mark.id.0));
            }
            response.context_menu(|ui| {
                if ui.button("Delete Mark").clicked() {
                    actions.push(Action::Command("script.deleteMark", json!({"item": app.ui.script.item, "take": take.id.0, "mark": mark.id.0})));
                    ui.close();
                }
            });
        }
    }
}

fn take_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, take: &ScriptTake, actions: &mut Vec<Action>) {
    if let Some(source) = take.source
        && ui.button("Find in Project").clicked()
    {
        actions.push(Action::FindInProject(source));
        ui.close();
    }
    ui.menu_button("Candle Color", |ui| {
        for label in Label::ALL {
            if ui.selectable_label(take.color == label, label.name()).clicked() {
                actions.push(Action::Command("script.setTakeStyle", json!({"item": app.ui.script.item, "take": take.id.0, "color": label.name()})));
                ui.close();
            }
        }
    });
    let selected_ids = app.ui.script.item.and_then(|id| app.session.project.item(ItemId(id))).and_then(|item| item.as_script()).and_then(|doc| {
        let (selected_start, selected_end) = selected_range(doc, app.ui.script.selected_lines)?;
        let (take_start, take_end) = doc.line_range(take.start_line, take.end_line)?;
        let start = selected_start.max(take_start);
        let end = selected_end.min(take_end);
        (start <= end).then(|| doc_ids(doc, (start, end)).collect::<Vec<_>>())
    });
    if let Some(ids) = selected_ids {
        let mut updated = take.offscreen_lines.clone();
        let all_offscreen = ids.iter().all(|id| updated.contains(id));
        if ui.button(if all_offscreen { "Clear Offscreen" } else { "Mark Offscreen" }).clicked() {
            if all_offscreen {
                updated.retain(|id| !ids.contains(id));
            } else {
                for id in ids {
                    if !updated.contains(&id) {
                        updated.push(id);
                    }
                }
            }
            actions.push(Action::Command(
                "script.setTakeStyle",
                json!({"item": app.ui.script.item, "take": take.id.0,
                "offscreenLines": updated.iter().map(|id| id.0).collect::<Vec<_>>()}),
            ));
            ui.close();
        }
    }
}

fn doc_ids(doc: &ScriptDocument, range: (usize, usize)) -> impl Iterator<Item = filmcraft_project::ScriptLineId> + '_ {
    doc.lines.get(range.0..=range.1).unwrap_or_default().iter().map(|line| line.id)
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

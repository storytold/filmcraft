//! Lined Script panel. The document is project data; this file keeps only view state.
//! Rows, take coverage and script marks share one vertical scroll coordinate.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_project::{ItemId, ItemKind, ScriptColor, ScriptDocument, ScriptLine, ScriptTake};
use filmcraft_time::{TICKS_PER_SECOND, Tick};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::collections::HashMap;
use std::sync::Arc;

use crate::FilmcraftApp;
use crate::dock::PanelKind;
use crate::native_dialogs::FileDialog;
use crate::theme::Tokens;

const MIN_ROW_H: f32 = 20.0;
const MAX_VISUAL_LINES: f32 = 12.0;
const SCRIPT_FONT_SIZE: f32 = 12.5;
const TAKE_W: f32 = 27.0;
const DEFAULT_THUMB_W: f32 = 96.0;
const MIN_THUMB_W: f32 = 64.0;
const MAX_THUMB_W: f32 = 192.0;
const THUMB_CHIN_H: f32 = 16.0;
const TAKE_NUMBER_H: f32 = 15.0;
const TAKE_HEADER_H: f32 = TAKE_NUMBER_H + 2.0;
const CANDLE_EDGE_INSET: f32 = 2.0;
const CANDLE_CAP_H: f32 = 4.0;
const SLATE_GAP: f32 = 14.0;
const IMPORT_LEADING_LINES: &str = "\n\n\n\n\n";
const DEFAULT_TEXT_INSET: f32 = 64.0;
const REBIND_WARNING: &str = "replacement is not identity-confirmed as the same footage;";
const REBIND_SPAN_WARNING: &str = "replacement source span excludes existing marks;";

mod canvas;
mod toolbar;

#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScriptUi {
    pub item: Option<u64>,
    /// Two stable line IDs, inclusive; order records the anchor and extension direction.
    pub selected_lines: Option<(u64, u64)>,
    pub selected_slate: Option<u64>,
    pub selected_take: Option<u64>,
    pub selected_mark: Option<u64>,
    /// Thumbnail selection is distinct from its display take for delete routing.
    pub selected_thumbnail: Option<u64>,
    #[serde(skip)]
    thumbnail_shuttle_at: f64,
    #[serde(skip)]
    thumbnail_shuttle_count: u32,
    #[serde(skip)]
    thumbnail_shuttle_direction: i8,
    #[serde(skip)]
    pub selected_section_slates: Vec<u64>,
    #[serde(skip)]
    pub section_clipboard: Option<Value>,
    #[serde(skip)]
    lasso_start: Option<egui::Pos2>,
    #[serde(skip)]
    lasso_end: Option<egui::Pos2>,
    pub paint_color: ScriptColor,
    pub edit_line: Option<u64>,
    pub edit_text: String,
    #[serde(skip)]
    pub edit_caret: Option<usize>,
    pub label_line: Option<u64>,
    pub scene_text: String,
    pub page_text: String,
    #[serde(skip)]
    pub label_focus: Option<u8>,
    #[serde(skip)]
    pub edit_scene: Option<u64>,
    #[serde(skip)]
    pub edit_page: Option<u64>,
    #[serde(skip)]
    pub drag_endpoint: Option<(u64, bool)>,
    #[serde(skip)]
    pub drag_mark: Option<(u64, u64)>,
    #[serde(skip)]
    pub drag_mark_target: Option<usize>,
    #[serde(skip)]
    pub drag_target: Option<usize>,
    #[serde(skip)]
    pub drag_slate: Option<u64>,
    #[serde(skip)]
    pub drag_slate_target: Option<usize>,
    #[serde(skip)]
    pub drag_slate_group: bool,
    #[serde(skip)]
    drag_slate_delta: egui::Vec2,
    #[serde(skip)]
    drag_slate_grab_y: f32,
    #[serde(skip)]
    drag_slate_origin_x: f32,
    /// Script view margins in panel points. They affect wrapping, not stored line text.
    pub text_column_width: Option<f32>,
    pub text_inset: Option<f32>,
    pub font_size: Option<f32>,
    pub thumbnail_width: Option<f32>,
    pub show_row_colors: Option<bool>,
    pub show_line_numbers: Option<bool>,
    pub search: String,
    #[serde(skip)]
    search_results: Vec<u64>,
    #[serde(skip)]
    search_result_index: usize,
    #[serde(skip)]
    search_cache_item: Option<u64>,
    #[serde(skip)]
    search_cache_doc: Option<Arc<ScriptDocument>>,
    #[serde(skip)]
    search_cache_query: String,
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
    layout_doc: Option<Arc<ScriptDocument>>,
    #[serde(skip)]
    layout_width: u32,
    #[serde(skip)]
    layout_inset: u32,
    #[serde(skip)]
    layout_font_size: u32,
    #[serde(skip)]
    layout_thumbnail_width: u32,
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
    CueMark { take: u64, mark: u64 },
    CueLine { take: u64, line: u64 },
    SplitLine { line: u64, text: String, at: usize },
    MergeWithPrevious { line: u64, text: String },
    AddMarkAtSelection,
    ToggleOffscreenSelection,
    FinishSlateDrag,
}

pub(crate) fn add_mark_at_selection(app: &mut FilmcraftApp) -> Result<Value, String> {
    let item = app.ui.script.item.ok_or("open a script first")?;
    let doc = app.session.project.item(ItemId(item)).and_then(|item| item.as_script()).ok_or("open a script first")?;
    let take = take_by_id(doc, app.ui.script.selected_take).ok_or("select a candle first")?;
    let line = current_line(doc, &app.ui.script).ok_or("select a script line first")?;
    let in_candle = doc
        .line_range(take.start_line, take.end_line)
        .is_some_and(|(start, end)| doc.line_index(line.id).is_some_and(|current| (start..=end).contains(&current)));
    if !in_candle {
        return Err("select a line within the candle".into());
    }
    let source = take.source.ok_or("reattach this candle's source first")?;
    let selected_mark = app.ui.script.selected_mark.filter(|id| take.marks.iter().any(|mark| mark.id.0 == *id && mark.line == line.id));
    let take_id = take.id.0;
    let line_id = line.id.0;
    if app.session.state.source_item != Some(source) {
        app.session.execute("source.open", json!({"item":source.0})).map_err(|error| error.to_string())?;
    }
    let time = app.session.state.source_playhead.0;
    let (command, params) = if let Some(mark) = selected_mark {
        ("script.setMarkTime", json!({"item":item,"take":take_id,"mark":mark,"sourceTime":time}))
    } else {
        ("script.addMark", json!({"item":item,"take":take_id,"line":line_id,"sourceTime":time}))
    };
    app.session.execute(command, params).map_err(|error| error.to_string())
}

pub(crate) fn delete_selection(app: &mut FilmcraftApp) -> Result<Value, String> {
    let Some(item) = app.ui.script.item else { return Ok(Value::Null) };
    let (command, params) = if let (Some(take), Some(mark)) = (app.ui.script.selected_take, app.ui.script.selected_mark) {
        ("script.deleteMark", json!({"item":item,"take":take,"mark":mark}))
    } else if let Some(slate) = app.ui.script.selected_thumbnail.filter(|slate| Some(*slate) == app.ui.script.selected_slate) {
        ("script.deleteSlate", json!({"item":item,"slate":slate}))
    } else if let Some(take) = app.ui.script.selected_take {
        ("script.deleteTake", json!({"item":item,"take":take}))
    } else {
        return Ok(Value::Null);
    };
    let result = app.session.execute(command, params).map_err(|error| error.to_string())?;
    app.ui.script.selected_mark = None;
    app.ui.script.selected_take = None;
    app.ui.script.selected_slate = None;
    app.ui.script.selected_thumbnail = None;
    app.ui.script.selected_section_slates.clear();
    Ok(result)
}

pub(crate) fn toggle_offscreen_selection(app: &mut FilmcraftApp) -> Result<Value, String> {
    let item = app.ui.script.item.ok_or("open a script first")?;
    let doc = app.session.project.item(ItemId(item)).and_then(|item| item.as_script()).ok_or("open a script first")?;
    let take = take_by_id(doc, app.ui.script.selected_take).ok_or("select a candle first")?;
    let ids = selected_take_lines(doc, &app.ui.script, take);
    if ids.is_empty() {
        return Err("select candle rows first".into());
    }
    let mut updated = take.offscreen_lines.clone();
    if ids.iter().all(|id| updated.contains(id)) {
        updated.retain(|id| !ids.contains(id));
    } else {
        for id in ids {
            if !updated.contains(&id) {
                updated.push(id);
            }
        }
    }
    app.session
        .execute("script.setTakeStyle", json!({"item":item,"take":take.id.0,"offscreenLines":updated.iter().map(|id|id.0).collect::<Vec<_>>()}))
        .map_err(|error| error.to_string())
}

pub(crate) fn shuttle_thumbnail(app: &mut FilmcraftApp, ctx: &egui::Context, direction: i8) -> Result<Value, String> {
    let item = app.ui.script.item.ok_or("open a script first")?;
    let slate_id = app.ui.script.selected_slate.ok_or("select a thumbnail first")?;
    let doc = app.session.project.item(ItemId(item)).and_then(|item| item.as_script()).ok_or("open a script first")?;
    let slate = doc.slates.iter().find(|slate| slate.id.0 == slate_id).ok_or("select a thumbnail first")?;
    let take =
        slate.takes.iter().find(|take| app.ui.script.selected_take == Some(take.id.0)).or_else(|| slate.takes.first()).ok_or("thumbnail has no candles")?;
    let source = take.source.ok_or("reattach this candle's source first")?;
    let view = filmcraft_engine::clip_ops::source_view(&app.session, source).ok_or("linked source is unavailable")?;
    let first = take.thumbnail_time.or_else(|| doc.first_visible_mark_time(take)).unwrap_or(view.start);
    let take_id = take.id.0;
    let now = ctx.input(|input| input.time);
    if app.ui.script.thumbnail_shuttle_direction == direction && now - app.ui.script.thumbnail_shuttle_at < 0.25 {
        app.ui.script.thumbnail_shuttle_count = app.ui.script.thumbnail_shuttle_count.saturating_add(1);
    } else {
        app.ui.script.thumbnail_shuttle_count = 0;
    }
    app.ui.script.thumbnail_shuttle_at = now;
    app.ui.script.thumbnail_shuttle_direction = direction;
    let seconds = match app.ui.script.thumbnail_shuttle_count {
        0..=5 => 1_i64,
        6..=17 => 2,
        _ => 5,
    };
    let last = (view.end - view.rate.frame_duration()).max(view.start);
    let ticks = first.0.saturating_add(i64::from(direction).saturating_mul(seconds.saturating_mul(TICKS_PER_SECOND)));
    let target = Tick(ticks).clamp(view.start, last);
    let frame = view.rate.frame_at(Tick(target.0.saturating_add(view.rate.frame_duration().0 / 2)));
    let target = view.rate.tick_of(frame).clamp(view.start, last);
    app.session.execute("script.setThumbnailTime", json!({"item":item,"take":take_id,"time":target.0})).map_err(|error| error.to_string())
}

/// Route the ordinary Edit menu shortcuts to Script when this panel has focus.
/// Text fields keep their native clipboard handling before shortcuts reach here.
pub(crate) fn copy_paste_section(app: &mut FilmcraftApp, copy: bool) -> Result<Value, String> {
    let item = app.ui.script.item.ok_or("open a script first")?;
    if copy {
        let doc = app.session.project.item(ItemId(item)).and_then(|item| item.as_script()).ok_or("open a script first")?;
        let (a, b) = selected_range(doc, app.ui.script.selected_lines).ok_or("select script rows to copy")?;
        let params = json!({"item":item,"startLine":doc.lines[a].id.0,
            "endLine":doc.lines[b].id.0,"slates":app.ui.script.selected_section_slates});
        let section = app.session.execute("script.copySection", params).map_err(|error| error.to_string())?;
        app.ui.script.section_clipboard = Some(section);
        app.ui.status = tl!("Script section copied").into();
        Ok(Value::Null)
    } else {
        let section = app.ui.script.section_clipboard.clone().ok_or("copy a script section first")?;
        let after = app
            .session
            .project
            .item(ItemId(item))
            .and_then(|item| item.as_script())
            .and_then(|doc| selected_range(doc, app.ui.script.selected_lines).map(|(_, b)| doc.lines[b].id.0));
        let result = app.session.execute("script.pasteSection", json!({"item":item,"afterLine":after,"section":section})).map_err(|error| error.to_string())?;
        if let (Some(first), Some(last)) = (result["startLine"].as_u64(), result["endLine"].as_u64()) {
            app.ui.script.selected_lines = Some((first, last));
            app.ui.script.selected_section_slates.clear();
            app.ui.script.scroll_to_line = Some(first);
        }
        Ok(result)
    }
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
                if let Some(value) = command(app, "script.create", json!({"name": tl!("Untitled Script")})) {
                    app.ui.script.item = value["item"].as_u64();
                    app.ui.script.selected_slate = None;
                    app.ui.script.selected_thumbnail = None;
                    app.ui.script.selected_take = None;
                    app.ui.script.selected_lines = None;
                    if let Some(line) = app
                        .ui
                        .script
                        .item
                        .and_then(|id| app.session.project.item(ItemId(id)))
                        .and_then(|item| item.as_script())
                        .and_then(|doc| doc.lines.first())
                    {
                        app.ui.script.selected_lines = Some((line.id.0, line.id.0));
                        app.ui.script.edit_line = Some(line.id.0);
                        app.ui.script.edit_text.clear();
                        app.ui.script.edit_caret = Some(0);
                    }
                }
            }
            Action::Import => import_text(app),
            Action::AddMarkAtSelection => {
                if let Err(error) = add_mark_at_selection(app) {
                    app.ui.status = error;
                }
            }
            Action::ToggleOffscreenSelection => {
                if let Err(error) = toggle_offscreen_selection(app) {
                    app.ui.status = error;
                }
            }
            Action::FinishSlateDrag => {
                app.ui.script.drag_slate = None;
                app.ui.script.drag_slate_target = None;
                app.ui.script.drag_slate_group = false;
                app.ui.script.drag_slate_delta = egui::Vec2::ZERO;
            }
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
            Action::CueMark { take, mark } => {
                if command(app, "script.cueMark", json!({"item": app.ui.script.item, "take": take, "mark": mark})).is_some() {
                    app.show_panel(PanelKind::Source);
                    app.ui.focused = PanelKind::Script;
                }
            }
            Action::CueLine { take, line } => {
                if command(app, "script.cueLine", json!({"item": app.ui.script.item, "take": take, "line": line})).is_some() {
                    app.show_panel(PanelKind::Source);
                    app.ui.focused = PanelKind::Script;
                }
            }
            Action::SplitLine { line, text, at } => {
                if let Some(value) = command(app, "script.splitLine", json!({"item": app.ui.script.item, "line": line, "text": text, "at": at}))
                    && let Some(created) = value["line"].as_u64()
                {
                    app.ui.script.selected_lines = Some((created, created));
                    app.ui.script.scroll_to_line = Some(created);
                    if let Some(new_text) = app
                        .ui
                        .script
                        .item
                        .and_then(|id| app.session.project.item(ItemId(id)))
                        .and_then(|item| item.as_script())
                        .and_then(|doc| doc.lines.iter().find(|line| line.id.0 == created))
                        .map(|line| line.text.clone())
                    {
                        app.ui.script.edit_line = Some(created);
                        app.ui.script.edit_text = new_text;
                        app.ui.script.edit_caret = Some(0);
                    }
                }
            }
            Action::MergeWithPrevious { line, text } => {
                if let Some(value) = command(app, "script.mergeWithPrevious", json!({"item": app.ui.script.item, "line": line, "text": text}))
                    && let (Some(previous), Some(merged), Some(caret)) = (value["line"].as_u64(), value["text"].as_str(), value["at"].as_u64())
                {
                    app.ui.script.edit_line = Some(previous);
                    app.ui.script.edit_text = merged.to_owned();
                    app.ui.script.edit_caret = usize::try_from(caret).ok();
                    app.ui.script.selected_lines = Some((previous, previous));
                    app.ui.script.scroll_to_line = Some(previous);
                }
            }
        }
    }
}

fn import_text(app: &mut FilmcraftApp) {
    app.pick_ui(FileDialog::open_file(tl!("Plain-text script"), &["txt"]), |app, paths| {
        let Some(path) = paths.into_iter().next() else { return };
        let file = std::path::Path::new(&path);
        let size = match app.session.services.file_size(&path) {
            Ok(size) => size,
            Err(error) => {
                app.ui.status = tlf!("Cannot read script: {error}", error);
                return;
            }
        };
        if size > ScriptDocument::MAX_TEXT_BYTES as u64 {
            app.ui.status = tl!("Script text exceeds the 4 MiB import limit").into();
            return;
        }
        let bytes = match app.session.services.read_file(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                app.ui.status = tlf!("Cannot read script: {error}", error);
                return;
            }
        };
        if bytes.len() > ScriptDocument::MAX_TEXT_BYTES {
            app.ui.status = tl!("Script text exceeds the 4 MiB import limit").into();
            return;
        }
        let text = match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(error) => {
                app.ui.status = tlf!("Cannot read script as UTF-8 text: {error}", error);
                return;
            }
        };
        let name = file.file_stem().and_then(std::ffi::OsStr::to_str).filter(|name| !name.is_empty()).unwrap_or(tl!("Untitled Script"));
        let body = text.strip_prefix('\u{feff}').unwrap_or(&text);
        if body.len() + IMPORT_LEADING_LINES.len() > ScriptDocument::MAX_TEXT_BYTES {
            app.ui.status = tl!("Script text needs room for five leading setup lines").into();
            return;
        }
        let padded_text = format!("{IMPORT_LEADING_LINES}{body}");
        if let Some(value) = command(app, "script.importText", json!({"name": name, "text": padded_text})) {
            app.ui.script.item = value["item"].as_u64();
            app.ui.script.selected_lines = None;
            app.ui.script.selected_slate = None;
            app.ui.script.selected_thumbnail = None;
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
    state.selected_mark = None;
    if state.selected_lines.is_none_or(|pair| pair.1 != line.id.0) {
        state.edit_scene = None;
        state.edit_page = None;
    }
    state.selected_lines = Some(if extend { (state.selected_lines.map_or(line.id.0, |pair| pair.0), line.id.0) } else { (line.id.0, line.id.0) });
    if state.label_line != Some(line.id.0) {
        state.label_line = Some(line.id.0);
        state.scene_text = line.scene.clone().unwrap_or_default();
        state.page_text = line.page.clone().unwrap_or_default();
    }
}

fn shift_click(ui: &egui::Ui) -> bool {
    ui.input(|input| {
        input.modifiers.shift
            || input.events.iter().any(|event| matches!(event, egui::Event::PointerButton { pressed: true, modifiers, .. } if modifiers.shift))
    })
}

fn thumbnail_width(state: &ScriptUi) -> f32 {
    state.thumbnail_width.unwrap_or(DEFAULT_THUMB_W).clamp(MIN_THUMB_W, MAX_THUMB_W)
}

fn thumbnail_height(width: f32) -> f32 {
    width * 9.0 / 16.0 + THUMB_CHIN_H
}

/// Measured once per document revision and width; a 4 MiB script may have 50,000 rows, so only
/// visible rows are painted while their cached prefix heights keep the candles in exact alignment.
fn row_layout(app: &mut FilmcraftApp, ui: &egui::Ui, doc: &Arc<ScriptDocument>, left_w: f32, text_inset: f32) {
    let width = left_w.round().max(1.0) as u32;
    let inset = text_inset.round().max(1.0) as u32;
    let script_font_size = app.ui.script.font_size.unwrap_or(SCRIPT_FONT_SIZE).clamp(10.0, 24.0);
    let font_size = script_font_size.to_bits();
    let thumb_w = thumbnail_width(&app.ui.script);
    let thumb_width = thumb_w.round() as u32;
    if app.ui.script.layout_item == app.ui.script.item
        && app.ui.script.layout_doc.as_ref().is_some_and(|cached| Arc::ptr_eq(cached, doc))
        && app.ui.script.layout_width == width
        && app.ui.script.layout_inset == inset
        && app.ui.script.layout_font_size == font_size
        && app.ui.script.layout_thumbnail_width == thumb_width
        && app.ui.script.row_tops.len() == doc.lines.len().saturating_add(1)
    {
        return;
    }
    let mut tops = Vec::with_capacity(doc.lines.len().saturating_add(1));
    let leading_blank = doc.lines.iter().take(5).take_while(|line| line.text.is_empty()).count();
    let clearance = thumbnail_height(thumb_w) + TAKE_HEADER_H + 10.0;
    let top_space = if leading_blank >= 3 { (clearance - leading_blank as f32 * MIN_ROW_H).max(8.0) } else { clearance };
    tops.push(top_space);
    let mut total = top_space;
    let font = Tokens::ui(script_font_size);
    let visual_line_height = ui.painter().layout_no_wrap("Mg".to_owned(), font.clone(), app.tokens.text).size().y;
    let max_height = (visual_line_height * MAX_VISUAL_LINES + 4.0).max(MIN_ROW_H);
    for line in &doc.lines {
        let text_w = (left_w - text_inset).max(48.0);
        // Measure the rendered font instead of assigning a fixed row height.
        // The cap also scales with the font, preserving up to 12 visible lines.
        let galley = ui.painter().layout(line.text.clone(), font.clone(), app.tokens.text, text_w);
        let height = (galley.size().y + 4.0).clamp(MIN_ROW_H, max_height);
        total += height;
        tops.push(total);
    }
    app.ui.script.row_tops = tops;
    app.ui.script.layout_item = app.ui.script.item;
    app.ui.script.layout_doc = Some(Arc::clone(doc));
    app.ui.script.layout_width = width;
    app.ui.script.layout_inset = inset;
    app.ui.script.layout_font_size = font_size;
    app.ui.script.layout_thumbnail_width = thumb_width;
}

fn first_visible(tops: &[f32], y: f32) -> usize {
    tops.partition_point(|top| *top <= y).saturating_sub(1)
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
    let header_rect = Rect::from_min_size(rect.min, vec2(rect.width(), 72.0));
    toolbar::header(app, ui, header_rect, &scripts, doc.as_ref(), &mut actions);
    if let Some(doc) = doc.as_ref() {
        toolbar::coverage_hotkeys(app, ui, doc, &mut actions);
        let body = Rect::from_min_max(pos2(rect.min.x, header_rect.max.y), rect.max);
        canvas::content(app, ui, body, doc, &mut actions);
    } else {
        ui.painter().text(rect.center(), Align2::CENTER_CENTER, tl!("Create a script or import a plain-text script."), Tokens::ui(14.0), t.text_dim);
    }
    run_actions(app, actions);
    rebind_confirmation(app, ui.ctx());
}

fn rebind_confirmation(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some((take, source)) = app.ui.script.pending_rebind else { return };
    egui::Window::new(tl!("Replace take?")).collapsible(false).resizable(false).show(ctx, |ui| {
        ui.label(tl!("This replacement could be different footage. Old script marks would point to the wrong frames."));
        ui.label(tl!("Replace the take and clear its marks?"));
        ui.horizontal(|ui| {
            let cancel = ui.button(tl!("Cancel"));
            app.auto.add("script.rebind.cancel", cancel.rect, "Cancel take replacement");
            if cancel.clicked() {
                app.ui.script.pending_rebind = None;
            }
            let replace = ui.button(tl!("Replace and Clear Marks"));
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
    state.selected_thumbnail = None;
    state.selected_mark = None;
    state.selected_lines = Some((entry.start, entry.end));
    state.scroll_to_line = Some(entry.start);
}

fn selected_take_lines(doc: &ScriptDocument, state: &ScriptUi, take: &ScriptTake) -> Vec<filmcraft_project::ScriptLineId> {
    if state.selected_take != Some(take.id.0) {
        return Vec::new();
    }
    let Some((selected_start, selected_end)) = selected_range(doc, state.selected_lines) else { return Vec::new() };
    let Some((take_start, take_end)) = doc.line_range(take.start_line, take.end_line) else { return Vec::new() };
    let start = selected_start.max(take_start);
    let end = selected_end.min(take_end);
    if start > end { Vec::new() } else { doc_ids(doc, (start, end)).collect() }
}

fn script_color(app: &FilmcraftApp, color: ScriptColor) -> Color32 {
    match color {
        ScriptColor::Default => app.tokens.panel_bg,
        ScriptColor::White => Color32::from_rgb(238, 238, 238),
        ScriptColor::Gray => Color32::from_rgb(126, 129, 135),
        other => {
            let Some(label) = other.label() else { return Color32::WHITE };
            let rgb = app.session.prefs.labels.rgb(label);
            Color32::from_rgb(rgb[0], rgb[1], rgb[2])
        }
    }
}

fn toggle_offscreen(app: &FilmcraftApp, take: &ScriptTake, ids: Vec<filmcraft_project::ScriptLineId>, actions: &mut Vec<Action>) {
    let mut updated = take.offscreen_lines.clone();
    if ids.iter().all(|id| updated.contains(id)) {
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
}

fn doc_ids(doc: &ScriptDocument, range: (usize, usize)) -> impl Iterator<Item = filmcraft_project::ScriptLineId> + '_ {
    doc.lines.get(range.0..=range.1).unwrap_or_default().iter().map(|line| line.id)
}

fn truncate(text: &str, max_chars: usize) -> String {
    let mut chars = text.chars();
    let head: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() { format!("{head}…") } else { head }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn document_caches_survive_unrelated_edits_and_refresh_for_same_length_text_changes() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let item = app.session.execute("script.importText", json!({"name":"Cached","text":"Needle\nOther"})).unwrap()["item"].as_u64().unwrap();
        app.ui.script.item = Some(item);
        app.ui.script.search = "needle".into();
        let ctx = egui::Context::default();
        let draw = |app: &mut FilmcraftApp| {
            let mut output =
                ctx.run_ui(egui::RawInput { screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1600.0, 980.0))), ..Default::default() }, |ui| {
                    show(app, ui, ui.max_rect());
                });
            output.textures_delta.clear();
        };
        draw(&mut app);
        let tops = app.ui.script.row_tops.as_ptr();
        let results = app.ui.script.search_results.as_ptr();
        let first_height = app.ui.script.row_tops[1];
        assert_eq!(app.ui.script.search_results.len(), 1);
        app.session.execute("script.importText", json!({"name":"Unrelated","text":"Another document"})).unwrap();
        draw(&mut app);
        assert_eq!(app.ui.script.row_tops.as_ptr(), tops, "unrelated edits must not remeasure the script");
        assert_eq!(app.ui.script.search_results.as_ptr(), results, "unrelated edits must not rescan the script");
        let cached = Arc::clone(app.ui.script.layout_doc.as_ref().unwrap());
        assert!(Arc::ptr_eq(&cached, app.ui.script.search_cache_doc.as_ref().unwrap()));
        let revision = app.session.revision;
        Arc::make_mut(&mut app.session.project).item_mut(ItemId(item)).unwrap().as_script_mut().unwrap().lines[0].text = "Long wrapping dialogue ".repeat(50);
        assert_eq!(app.session.revision, revision, "document identity must work independently of the global revision");
        draw(&mut app);
        assert!(!Arc::ptr_eq(&cached, app.ui.script.layout_doc.as_ref().unwrap()));
        assert_eq!(cached.lines[0].text, "Needle", "the retained cache document stays immutable");
        assert!(app.ui.script.row_tops[1] > first_height);
        assert!(app.ui.script.search_results.is_empty());
    }
}

//! Media Browser panel (M12.7): a directory tree (Favorites, Local Drives, Network, Recent
//! Directories) beside the media list (List or Thumbnails view with hover scrub), back / forward /
//! up navigation, the path field, the file-type filter, Ingest and Import as Image Sequence, Edit
//! Columns…, Import / Open In Source Monitor and dragging files to the Project panel or Timeline.
//!
//! Listing, navigation, Favorites, settings and import are engine commands
//! (`filmcraft_engine::media_browser`, `mediaBrowser.*`); this module draws them and keeps only view
//! state ([`MediaBrowserUi`], `ui.set {"mediaBrowser": …}`).

use std::collections::BTreeMap;
use std::sync::Arc;

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_engine::media_browser::{self as mb, Entry};
use filmcraft_project::{ItemId, ItemKind, Label, MediaClip, MediaRef, Project, ProjectItem};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::frames::{FrameKey, Target};
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// Frontend state of the Media Browser (`UiState::media_browser`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct MediaBrowserUi {
    /// Expanded directories in the tree.
    pub expanded: Vec<String>,
    /// The path field being edited (None = shows the current directory).
    pub path_edit: Option<String>,
    /// Edit Columns… dialog: the columns being chosen (open when Some).
    pub edit_columns: Option<Vec<String>>,
    /// Width of the directory tree (points).
    pub tree_width: f32,
    /// The thumbnail under the pointer and the hovered fraction of its duration.
    pub hover: Option<(String, f32)>,
}

/// Cached directory listings (refreshed every few seconds or on Refresh).
#[derive(Clone, Default)]
struct Cache {
    lists: BTreeMap<(String, String), (f64, Arc<Vec<Entry>>)>,
    /// Preview items for thumbnails of files not in the project: path → id.
    preview_ids: BTreeMap<String, u64>,
    preview: Arc<Project>,
    /// Item imported for a drag in progress (undone when it is dropped nowhere): items, history
    /// length after the import, last pointer position.
    drag: Option<(Vec<u64>, usize, egui::Pos2)>,
}

const PREVIEW_BASE: u64 = 1 << 44;
const CACHE_SECS: f64 = 3.0;
const ROW_H: f32 = 22.0;

fn cache_id() -> egui::Id {
    egui::Id::new("media-browser-cache")
}

fn with_cache<R>(ctx: &egui::Context, f: impl FnOnce(&mut Cache) -> R) -> R {
    ctx.data_mut(|d| {
        let c = d.get_temp_mut_or_default::<Cache>(cache_id());
        f(c)
    })
}

/// Drop cached listings (Refresh, after navigation).
pub fn refresh(ctx: &egui::Context) {
    with_cache(ctx, |c| c.lists.clear());
}

fn listing(app: &FilmcraftApp, ctx: &egui::Context, dir: &str, filter: &str) -> Result<Arc<Vec<Entry>>, String> {
    let now = ctx.input(|i| i.time);
    let key = (dir.to_string(), filter.to_string());
    if let Some(v) = with_cache(ctx, |c| c.lists.get(&key).filter(|(t, _)| now - *t < CACHE_SECS).map(|(_, v)| v.clone())) {
        return Ok(v);
    }
    let v = Arc::new(mb::list(&*app.session.services, dir, filter).map_err(|e| e.to_string())?);
    with_cache(ctx, |c| c.lists.insert(key, (now, v.clone())));
    Ok(v)
}

fn exec(app: &mut FilmcraftApp, ctx: &egui::Context, cmd: &str, p: Value) -> Option<Value> {
    match crate::menus::invoke(app, ctx, cmd, p) {
        Ok(v) => Some(v),
        Err(e) => {
            app.ui.status = e;
            None
        }
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    finish_drag(app, ui);
    let dir = mb::current_dir(&app.session);
    let prefs = app.session.prefs.media_browser.clone();
    // ---- toolbar: back, forward, up, path, file types, view, Ingest, Import as Image Sequence
    let bar = Rect::from_min_size(rect.min + vec2(6.0, 4.0), vec2(rect.width() - 12.0, 24.0));
    let mut x = bar.min.x;
    let nav = [
        (Icon::ChevronLeft, "back", tl!("Go Back"), !app.session.browser.back.is_empty()),
        (Icon::ChevronRight, "forward", tl!("Go Forward"), !app.session.browser.forward.is_empty()),
        (Icon::ArrowUp, "up", tl!("Up One Level"), mb::parent(&dir).is_some()),
    ];
    for (icon, id, tip, enabled) in nav {
        let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(24.0, 24.0));
        let resp = ui.interact(r, egui::Id::new(("mb-nav", id)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("mediaBrowser.{id}"), r, tip);
        if resp.hovered() && enabled {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(5.0), icon, if enabled { t.icon } else { t.text_faint });
        if resp.clicked() && enabled {
            exec(app, &ctx, "mediaBrowser.navigate", json!({id: true}));
            refresh(&ctx);
        }
        x += 26.0;
    }
    // path field: type a directory and press Enter
    let pw = (bar.max.x - x - 290.0).max(80.0);
    let pr = Rect::from_min_size(pos2(x + 4.0, bar.min.y + 1.0), vec2(pw, 22.0));
    let mut path = app.ui.media_browser.path_edit.clone().unwrap_or_else(|| dir.clone());
    let resp = ui.put(pr, egui::TextEdit::singleline(&mut path).id(egui::Id::new("mb-path")).font(egui::FontId::proportional(11.5)));
    app.auto.add("mediaBrowser.path", pr, "Path");
    if resp.changed() {
        app.ui.media_browser.path_edit = Some(path.clone());
    }
    if resp.lost_focus() {
        if ui.input(|i| i.key_pressed(egui::Key::Enter)) && path != dir {
            exec(app, &ctx, "mediaBrowser.navigate", json!({"path": path}));
            refresh(&ctx);
        }
        app.ui.media_browser.path_edit = None;
    }
    x = pr.max.x + 6.0;
    // file types
    let fr = Rect::from_min_size(pos2(x, bar.min.y + 1.0), vec2(150.0, 22.0));
    let label = mb::FILE_TYPES.iter().find(|f| f.0 == prefs.file_types).map(|f| f.1.to_string()).unwrap_or_else(|| format!(".{}", prefs.file_types));
    let fresp = crate::widgets::dropdown_text(ui, fr, &label, &t, egui::Id::new("mb-types")).on_hover_text(tl!("File Types Displayed"));
    app.auto.add("mediaBrowser.fileTypes", fr, "File Types Displayed");
    egui::Popup::menu(&fresp).show(|ui| {
        for (k, l) in mb::FILE_TYPES {
            let r = ui.selectable_label(prefs.file_types == k, l);
            app.auto.add(&format!("mediaBrowser.fileTypes.{k}"), r.rect, l);
            if r.clicked() {
                exec(app, &ctx, "mediaBrowser.settings", json!({"fileTypes": k}));
                ui.close();
            }
        }
        ui.separator();
        ui.menu_button(tl!("File Extension"), |ui| {
            for e in mb::extensions() {
                if ui.selectable_label(prefs.file_types == e, format!(".{e}")).clicked() {
                    exec(app, &ctx, "mediaBrowser.settings", json!({"fileTypes": e}));
                    ui.close();
                }
            }
        });
    });
    x = fr.max.x + 6.0;
    for (icon, v, tip) in [(Icon::ListView, "list", tl!("List View")), (Icon::IconView, "thumbnails", tl!("Thumbnail View"))] {
        let r = Rect::from_min_size(pos2(x, bar.min.y), vec2(24.0, 24.0));
        let resp = ui.interact(r, egui::Id::new(("mb-view", v)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("mediaBrowser.view.{v}"), r, tip);
        if prefs.view == v {
            ui.painter().rect_filled(r, 3.0, t.pressed);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(5.0), icon, if prefs.view == v { t.icon_active } else { t.icon });
        if resp.clicked() {
            exec(app, &ctx, "mediaBrowser.settings", json!({"view": v}));
        }
        x += 26.0;
    }
    // second row: Ingest (Project ▸ Ingest Settings) and Import as Image Sequence
    let row2 = Rect::from_min_size(pos2(bar.min.x, bar.max.y + 4.0), vec2(bar.width(), 20.0));
    let mut ingest = app.session.project.settings.ingest.enabled;
    let mut c = ui.new_child(egui::UiBuilder::new().max_rect(row2).id_salt("mb-row2").layout(egui::Layout::left_to_right(egui::Align::Center)));
    let r = c.checkbox(&mut ingest, tl!("Ingest"));
    app.auto.add("mediaBrowser.ingest", r.rect, "Ingest");
    if r.changed() {
        exec(app, &ctx, "project.ingestSettings", json!({"enabled": ingest}));
    }
    let w = c.add(egui::Button::new(egui::RichText::new("⚙").size(12.0)).frame(false)).on_hover_text(tl!("Open Ingest Settings"));
    app.auto.add("mediaBrowser.ingestSettings", w.rect, "Open Ingest Settings");
    if w.clicked() {
        exec(app, &ctx, "project.ingestSettings", json!({}));
    }
    c.add_space(12.0);
    let mut seq = prefs.import_as_image_sequence;
    let r = c.checkbox(&mut seq, tl!("Import as Image Sequence"));
    app.auto.add("mediaBrowser.importAsImageSequence", r.rect, "Import as Image Sequence");
    if r.changed() {
        exec(app, &ctx, "mediaBrowser.settings", json!({"importAsImageSequence": seq}));
    }
    // ---- tree | list
    let body = Rect::from_min_max(pos2(rect.min.x, row2.max.y + 4.0), pos2(rect.max.x, rect.max.y - 24.0));
    let tw = if app.ui.media_browser.tree_width <= 0.0 { (rect.width() * 0.32).clamp(140.0, 260.0) } else { app.ui.media_browser.tree_width };
    let tree_r = Rect::from_min_max(body.min, pos2(body.min.x + tw, body.max.y));
    let list_r = Rect::from_min_max(pos2(tree_r.max.x + 4.0, body.min.y), body.max);
    // splitter
    let split = Rect::from_min_max(pos2(tree_r.max.x, body.min.y), pos2(tree_r.max.x + 4.0, body.max.y));
    let sresp = ui.interact(split, egui::Id::new("mb-split"), Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeHorizontal);
    if sresp.dragged() {
        app.ui.media_browser.tree_width = (tw + sresp.drag_delta().x).clamp(100.0, (rect.width() * 0.7).max(100.0));
    }
    ui.painter().line_segment([split.center_top(), split.center_bottom()], Stroke::new(1.0, t.separator));
    tree(app, ui, tree_r, &dir);
    let entries = listing(app, &ctx, &dir, &prefs.file_types);
    match entries {
        Ok(entries) => {
            if prefs.view == "thumbnails" {
                thumbnails(app, ui, list_r, &entries);
            } else {
                list(app, ui, list_r, &entries, &prefs.columns);
            }
            let n = entries.iter().filter(|e| !e.is_dir).count();
            let sel = app.session.browser.selection.len();
            let status = if sel > 0 { tlf!("{sel} of {total} items selected", sel, total = n) } else { tlf!("{total} items", total = n) };
            ui.painter().text(pos2(list_r.min.x + 6.0, rect.max.y - 12.0), Align2::LEFT_CENTER, &status, Tokens::ui(11.0), t.text_dim);
            app.auto.add("mediaBrowser.count", Rect::from_min_size(pos2(list_r.min.x, rect.max.y - 22.0), vec2(160.0, 20.0)), &status);
        }
        Err(e) => {
            ui.painter().text(list_r.center(), Align2::CENTER_CENTER, tlf!("Cannot open {dir}: {e}", dir, e), Tokens::ui(11.5), t.text_dim);
        }
    }
    if prefs.view == "thumbnails" {
        let sr = Rect::from_min_size(pos2(rect.max.x - 130.0, rect.max.y - 20.0), vec2(120.0, 16.0));
        let mut sz = prefs.thumbnail_size;
        let resp = ui.put(sr, egui::Slider::new(&mut sz, 60.0..=320.0).show_value(false));
        app.auto.add("mediaBrowser.thumbnailSize", sr, "Thumbnail size");
        if sz != prefs.thumbnail_size {
            app.session.prefs.media_browser.thumbnail_size = sz;
        }
        if resp.drag_stopped() || (resp.changed() && !resp.dragged()) {
            exec(app, &ctx, "mediaBrowser.settings", json!({"thumbnailSize": sz}));
        }
    }
}

// ------------------------------------------------------------------------------------ tree

fn tree(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, dir: &str) {
    let ctx = ui.ctx().clone();
    let roots = app.session.execute("mediaBrowser.roots", json!({})).unwrap_or_default();
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r.shrink2(vec2(4.0, 0.0))).id_salt("mb-tree"));
    child.set_clip_rect(r.intersect(ui.clip_rect()));
    let mut go: Option<String> = None;
    egui::ScrollArea::vertical().id_salt("mb-tree-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        let sections: [(&str, &str, Icon); 4] = [
            ("favorites", tl!("Favorites"), Icon::Star),
            ("localDrives", tl!("Local Drives"), Icon::Drive),
            ("network", tl!("Network"), Icon::Network),
            ("recent", tl!("Recent Directories"), Icon::Clock),
        ];
        for (key, title, icon) in sections {
            let list: Vec<(String, String)> = roots[key]
                .as_array()
                .map(|a| a.iter().filter_map(|v| Some((v["name"].as_str()?.to_string(), v["path"].as_str()?.to_string()))).collect())
                .unwrap_or_default();
            let hr = ui.label(egui::RichText::new(title).size(11.0).color(app.tokens.text_dim).strong());
            app.auto.add(&format!("mediaBrowser.tree.{key}"), hr.rect, title);
            if list.is_empty() {
                ui.label(egui::RichText::new(tl!("  (none)")).size(10.5).color(app.tokens.text_faint));
            }
            for (name, path) in list {
                let expandable = matches!(key, "localDrives" | "network" | "favorites");
                node(app, ui, &ctx, &name, &path, icon, 0, expandable, dir, key, &mut go);
            }
            ui.add_space(6.0);
        }
    });
    if let Some(p) = go {
        exec(app, &ctx, "mediaBrowser.navigate", json!({"path": p}));
        refresh(&ctx);
    }
}

#[allow(clippy::too_many_arguments)]
fn node(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    name: &str,
    path: &str,
    icon: Icon,
    depth: usize,
    expandable: bool,
    cur: &str,
    section: &str,
    go: &mut Option<String>,
) {
    let t = app.tokens;
    let open = app.ui.media_browser.expanded.iter().any(|p| p == path);
    let (r, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 20.0), Sense::click());
    if path == cur {
        ui.painter().rect_filled(r, 2.0, t.row_selected);
    } else if resp.hovered() {
        ui.painter().rect_filled(r, 2.0, t.hover);
    }
    let x = r.min.x + 2.0 + depth as f32 * 12.0;
    let tri = Rect::from_center_size(pos2(x + 5.0, r.center().y), vec2(12.0, 12.0));
    if expandable {
        icons::paint(ui.painter(), tri.shrink(2.0), if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_dim);
    }
    icons::paint(
        ui.painter(),
        Rect::from_center_size(pos2(x + 19.0, r.center().y), vec2(13.0, 13.0)),
        icon,
        if icon == Icon::Folder { Color32::from_rgb(237, 150, 58) } else { t.icon },
    );
    ui.painter().with_clip_rect(r).text(pos2(x + 30.0, r.center().y), Align2::LEFT_CENTER, name, Tokens::ui(11.5), t.text);
    let id = if depth == 0 { format!("mediaBrowser.tree.{section}.{name}") } else { format!("mediaBrowser.tree.dir.{path}") };
    app.auto.add(&id, r, name);
    if resp.clicked() {
        let on_tri = expandable && resp.interact_pointer_pos().is_some_and(|p| p.x < x + 11.0);
        if on_tri {
            toggle(app, path, open);
        } else {
            *go = Some(path.to_string());
        }
    }
    if resp.double_clicked() && expandable {
        toggle(app, path, open);
    }
    resp.context_menu(|ui| {
        let fav = app.session.prefs.media_browser.favorites.iter().any(|f| f == path);
        let label = if fav { tl!("Remove from Favorites") } else { tl!("Add to Favorites") };
        let b = ui.button(label);
        app.auto.add("mediaBrowser.treeMenu.favorite", b.rect, label);
        if b.clicked() {
            exec(app, ctx, "mediaBrowser.favorite", json!({"path": path, "remove": fav}));
            ui.close();
        }
        if ui.button(tl!("Import")).clicked() {
            exec(app, ctx, "mediaBrowser.import", json!({"paths": [path]}));
            ui.close();
        }
        if section == "recent" && ui.button(tl!("Clear Recent Directories")).clicked() {
            exec(app, ctx, "mediaBrowser.clearRecent", json!({}));
            ui.close();
        }
    });
    if expandable && open {
        let subs: Vec<Entry> = listing(app, ctx, path, "all").map(|v| v.iter().filter(|e| e.is_dir).cloned().collect()).unwrap_or_default();
        for s in subs {
            node(app, ui, ctx, &s.name, &s.path, Icon::Folder, depth + 1, true, cur, section, go);
        }
    }
}

fn toggle(app: &mut FilmcraftApp, path: &str, open: bool) {
    let e = &mut app.ui.media_browser.expanded;
    if open {
        e.retain(|p| p != path);
    } else {
        e.push(path.to_string());
    }
}

// ------------------------------------------------------------------------------------ entries

/// Click / double-click / drag / context menu of an entry.
fn entry_interactions(app: &mut FilmcraftApp, ui: &egui::Ui, resp: &egui::Response, e: &Entry, entries: &[Entry]) {
    let ctx = ui.ctx().clone();
    if resp.clicked() {
        let mods = crate::panels::project_views::click_modifiers(ui);
        let mut sel = app.session.browser.selection.clone();
        if mods.command {
            if let Some(p) = sel.iter().position(|x| *x == e.path) {
                sel.remove(p);
            } else {
                sel.push(e.path.clone());
            }
        } else if mods.shift && !sel.is_empty() {
            let a = entries.iter().position(|x| Some(&x.path) == sel.first()).unwrap_or(0);
            let b = entries.iter().position(|x| x.path == e.path).unwrap_or(0);
            sel = entries[a.min(b)..=a.max(b)].iter().map(|x| x.path.clone()).collect();
        } else {
            sel = vec![e.path.clone()];
        }
        exec(app, &ctx, "mediaBrowser.select", json!({"paths": sel}));
    }
    if resp.double_clicked() {
        if e.is_dir {
            exec(app, &ctx, "mediaBrowser.navigate", json!({"path": e.path}));
            refresh(&ctx);
        } else if matches!(e.kind, "project" | "caption") {
            exec(app, &ctx, "mediaBrowser.import", json!({"paths": [e.path]}));
        } else {
            exec(app, &ctx, "mediaBrowser.openInSource", json!({"path": e.path}));
        }
    }
    if resp.drag_started() && !e.is_dir {
        start_file_drag(app, ui, e);
    }
    resp.context_menu(|ui| {
        let paths: Vec<String> = if app.session.browser.selection.contains(&e.path) { app.session.browser.selection.clone() } else { vec![e.path.clone()] };
        let b = ui.button(tl!("Import"));
        app.auto.add("mediaBrowser.entryMenu.import", b.rect, "Import");
        if b.clicked() {
            exec(app, &ctx, "mediaBrowser.import", json!({"paths": paths, "imageSequence": false}));
            ui.close();
        }
        if !e.is_dir {
            let b = ui.button(tl!("Open In Source Monitor"));
            app.auto.add("mediaBrowser.entryMenu.openInSource", b.rect, "Open In Source Monitor");
            if b.clicked() {
                exec(app, &ctx, "mediaBrowser.openInSource", json!({"path": e.path}));
                ui.close();
            }
        }
        if e.numbered {
            let b = ui.button(tl!("Import as Image Sequence"));
            app.auto.add("mediaBrowser.entryMenu.importSequence", b.rect, "Import as Image Sequence");
            if b.clicked() {
                exec(app, &ctx, "mediaBrowser.import", json!({"paths": [e.path], "imageSequence": true}));
                ui.close();
            }
        }
        if e.is_dir {
            let fav = app.session.prefs.media_browser.favorites.contains(&e.path);
            if ui.button(if fav { tl!("Remove from Favorites") } else { tl!("Add to Favorites") }).clicked() {
                exec(app, &ctx, "mediaBrowser.favorite", json!({"path": e.path, "remove": fav}));
                ui.close();
            }
        }
        if ui.button(tl!("Reveal in Finder")).clicked() {
            let _ = crate::panels::menu_dialogs::open_path(app, &ctx, &e.path, true);
            ui.close();
        }
    });
}

/// Dragging a file imports it (the selection when the file is in it) and carries the new item, so
/// the Project panel, Timeline and monitors accept it; a drop elsewhere undoes the import.
fn start_file_drag(app: &mut FilmcraftApp, ui: &egui::Ui, e: &Entry) {
    let paths: Vec<String> = if app.session.browser.selection.contains(&e.path) { app.session.browser.selection.clone() } else { vec![e.path.clone()] };
    // files already in the project are dragged as they are
    if let Some(i) = mb::item_for_path(&app.session, &e.path) {
        crate::panels::start_drag_item(ui, i);
        return;
    }
    let before = app.session.history.undo.len();
    let Ok(r) = app.session.execute("mediaBrowser.import", json!({"paths": paths})) else { return };
    let items: Vec<u64> = r["items"].as_array().map(|a| a.iter().filter_map(Value::as_u64).collect()).unwrap_or_default();
    if let Some(first) = items.first() {
        crate::panels::start_drag_item(ui, ItemId(*first));
        let pos = ui.ctx().pointer_latest_pos().unwrap_or_default();
        let after = app.session.history.undo.len();
        with_cache(ui.ctx(), |c| c.drag = Some((items, after.max(before), pos)));
    }
}

/// After a file drag ends: keep the import when it landed on a panel that takes items.
fn finish_drag(app: &mut FilmcraftApp, ui: &egui::Ui) {
    let Some((items, hist, _)) = with_cache(ui.ctx(), |c| c.drag.clone()) else { return };
    if crate::panels::dragged_project_item(ui).is_some() {
        if let Some(p) = ui.ctx().pointer_latest_pos() {
            with_cache(ui.ctx(), |c| {
                if let Some(d) = c.drag.as_mut() {
                    d.2 = p;
                }
            });
        }
        return;
    }
    let pos = with_cache(ui.ctx(), |c| c.drag.take().map(|d| d.2)).unwrap_or_default();
    let targets = ["panel.Project", "panel.Timeline", "panel.Source", "panel.Program"];
    // a drag cancelled with Escape (#580) landed nowhere, wherever the pointer is
    let landed = !crate::panels::drag_cancelled(ui)
        && targets.iter().any(|t| app.auto.find(t).is_some_and(|e| Rect::from_min_size(pos2(e.rect[0], e.rect[1]), vec2(e.rect[2], e.rect[3])).contains(pos)));
    let used_elsewhere = app.session.history.undo.len() > hist;
    if !landed && !used_elsewhere && items.iter().all(|i| app.session.project.item(ItemId(*i)).is_some()) {
        // dropped nowhere: take the import back
        let _ = app.session.execute("edit.undo", json!({}));
    }
}

fn list(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, entries: &[Entry], columns: &[String]) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let widths: Vec<f32> = columns.iter().map(|c| if c == "Name" { 220.0 } else { 110.0 }).collect();
    let total: f32 = widths.iter().sum::<f32>() + 8.0;
    let header = Rect::from_min_size(r.min, vec2(r.width(), 20.0));
    let body = Rect::from_min_max(pos2(r.min.x, header.max.y), r.max);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("mb-list"));
    child.set_clip_rect(body.intersect(ui.clip_rect()));
    let needs_probe =
        columns.iter().any(|c| matches!(c.as_str(), "Frame Rate" | "Media Duration" | "Video Info" | "Audio Info" | "Video Codec" | "Audio Codec"));
    let mut probes_left = 2;
    let out = egui::ScrollArea::both().id_salt("mb-list-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        ui.set_min_width(total.max(body.width()));
        for (i, e) in entries.iter().enumerate() {
            let (rr, resp) = ui.allocate_exact_size(vec2(total.max(body.width()), ROW_H), Sense::click_and_drag());
            let sel = app.session.browser.selection.contains(&e.path);
            if sel {
                ui.painter().rect_filled(rr, 0.0, t.row_selected);
            } else if i % 2 == 1 {
                ui.painter().rect_filled(rr, 0.0, t.row_alt);
            }
            let info = if needs_probe && !e.is_dir {
                let cached = app.session.browser.probes.contains_key(&e.path);
                if cached || probes_left > 0 {
                    if !cached {
                        probes_left -= 1;
                        ctx.request_repaint();
                    }
                    mb::probe(&mut app.session, &e.path)
                } else {
                    None
                }
            } else {
                None
            };
            let mut x = rr.min.x + 4.0;
            for (c, w) in columns.iter().zip(&widths) {
                let cr = Rect::from_min_size(pos2(x, rr.min.y), vec2(*w, ROW_H));
                let p = ui.painter().with_clip_rect(cr.shrink2(vec2(2.0, 0.0)).intersect(ui.clip_rect()));
                if c == "Name" {
                    let icon = match e.kind {
                        "folder" => Icon::Folder,
                        "audio" => Icon::Audio,
                        "image" => Icon::Image,
                        "video" => Icon::Film,
                        "caption" => Icon::Captions,
                        _ => Icon::Sequence,
                    };
                    let col = if e.is_dir { Color32::from_rgb(237, 150, 58) } else { t.icon };
                    icons::paint(&p, Rect::from_center_size(pos2(cr.min.x + 9.0, cr.center().y), vec2(14.0, 14.0)), icon, col);
                    p.text(pos2(cr.min.x + 22.0, cr.center().y), Align2::LEFT_CENTER, &e.name, Tokens::ui(12.0), t.text);
                } else {
                    p.text(pos2(cr.min.x + 4.0, cr.center().y), Align2::LEFT_CENTER, mb::column_text(e, c, info.as_ref()), Tokens::ui(11.5), t.text_dim);
                }
                x += w;
            }
            app.auto.add(&format!("mediaBrowser.entry.{}", e.name), rr.intersect(ui.clip_rect()), &e.name);
            entry_interactions(app, ui, &resp, e, entries);
        }
        let rest = ui.available_rect_before_wrap();
        let resp = ui.allocate_rect(Rect::from_min_size(rest.min, vec2(rest.width(), rest.height().max(30.0))), Sense::click());
        if resp.clicked() {
            exec(app, &ctx, "mediaBrowser.select", json!({"paths": []}));
        }
    });
    let hp = ui.painter().with_clip_rect(header.intersect(ui.clip_rect()));
    hp.rect_filled(header, 0.0, t.panel_bg);
    hp.line_segment([header.left_bottom(), header.right_bottom()], Stroke::new(1.0, t.separator));
    let mut x = header.min.x + 4.0 - out.state.offset.x;
    for (c, w) in columns.iter().zip(&widths) {
        let cr = Rect::from_min_size(pos2(x, header.min.y), vec2(*w, 20.0));
        hp.text(pos2(cr.min.x + 4.0, cr.center().y), Align2::LEFT_CENTER, crate::i18n::t(c), Tokens::ui(11.0), t.text_dim);
        if cr.intersects(header) {
            let resp = ui.interact(cr.intersect(header), egui::Id::new(("mb-col", c)), Sense::click());
            app.auto.add(&format!("mediaBrowser.header.{c}"), cr.intersect(header), c);
            resp.context_menu(|ui| {
                if ui.button(tl!("Edit Columns…")).clicked() {
                    app.ui.media_browser.edit_columns = Some(columns.to_vec());
                    ui.close();
                }
            });
        }
        x += w;
    }
}

/// A preview item for a file's thumbnail (files not in the project), or the project item.
fn preview_item(
    app: &mut FilmcraftApp,
    ctx: &egui::Context,
    e: &Entry,
) -> Option<(Arc<Project>, ItemId, filmcraft_time::Tick, filmcraft_time::FrameRate, u32)> {
    if let Some(i) = mb::item_for_path(&app.session, &e.path) {
        let it = app.session.project.item(i)?;
        let w = it.as_media()?.info.video.as_ref()?.width;
        return Some((app.session.project.clone(), i, it.duration(), it.frame_rate(), w));
    }
    let info = app.session.browser.probes.get(&e.path).cloned().flatten()?;
    let v = info.video.as_ref()?;
    let (rate, w, dur) = (v.frame_rate, v.width, info.duration);
    let (proj, id) = with_cache(ctx, |c| {
        let n = c.preview_ids.len() as u64;
        let id = *c.preview_ids.entry(e.path.clone()).or_insert(PREVIEW_BASE + n);
        if c.preview.item(ItemId(id)).is_none() {
            let mut p = (*c.preview).clone();
            p.items.insert(
                ItemId(id),
                ProjectItem {
                    id: ItemId(id),
                    name: e.name.clone(),
                    label: Label::Iris,
                    kind: ItemKind::Media(MediaClip {
                        media: MediaRef::File { path: e.path.clone() },
                        info: info.clone(),
                        interpret: Default::default(),
                        mark_in: None,
                        mark_out: None,
                        markers: vec![],
                        offline: false,
                        proxy: None,
                        identity: None,
                    }),
                    metadata: Default::default(),
                    created: 0,
                    split: Default::default(),
                },
            );
            c.preview = Arc::new(p);
        }
        (c.preview.clone(), ItemId(id))
    });
    Some((proj, id, dur, rate, w))
}

fn thumbnails(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, entries: &[Entry]) {
    let t = app.tokens;
    let ctx = ui.ctx().clone();
    let size = app.session.prefs.media_browser.thumbnail_size;
    let hover_scrub = app.session.prefs.media_browser.hover_scrub;
    let th_h = size * 9.0 / 16.0;
    let cell = vec2(size + 16.0, th_h + 30.0);
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).id_salt("mb-thumbs"));
    child.set_clip_rect(r.intersect(ui.clip_rect()));
    let per_row = ((r.width() - 8.0) / cell.x).floor().max(1.0) as usize;
    let mut probes_left = 2;
    app.ui.media_browser.hover = None;
    egui::ScrollArea::vertical().id_salt("mb-thumb-scroll").auto_shrink([false, false]).show(&mut child, |ui| {
        for chunk in entries.chunks(per_row) {
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), cell.y), Sense::hover());
            for (k, e) in chunk.iter().enumerate() {
                let tr = Rect::from_min_size(pos2(row.min.x + 8.0 + k as f32 * cell.x, row.min.y + 6.0), vec2(size, th_h));
                let resp = ui.interact(tr.expand(2.0), egui::Id::new(("mb-thumb", &e.path)), Sense::click_and_drag());
                let sel = app.session.browser.selection.contains(&e.path);
                ui.painter().rect_filled(tr, 3.0, Color32::from_gray(14));
                if sel {
                    ui.painter().rect_stroke(tr, 3.0, Stroke::new(2.0, t.accent), StrokeKind::Outside);
                }
                if e.is_dir {
                    icons::paint(
                        ui.painter(),
                        Rect::from_center_size(tr.center(), vec2(th_h * 0.6, th_h * 0.6)),
                        Icon::Folder,
                        Color32::from_rgb(237, 150, 58),
                    );
                } else {
                    let visible = ui.is_rect_visible(tr);
                    if visible && matches!(e.kind, "video" | "image") && !app.session.browser.probes.contains_key(&e.path) && probes_left > 0 {
                        probes_left -= 1;
                        mb::probe(&mut app.session, &e.path);
                        ctx.request_repaint();
                    }
                    let frac = if hover_scrub && resp.hovered() {
                        ctx.pointer_hover_pos().map(|p| ((p.x - tr.min.x) / tr.width()).clamp(0.0, 1.0)).unwrap_or(0.3)
                    } else {
                        0.3
                    };
                    if resp.hovered() {
                        app.ui.media_browser.hover = Some((e.path.clone(), frac));
                    }
                    let drawn = visible && draw_preview(app, ui, &ctx, e, tr, frac);
                    if !drawn {
                        let icon = match e.kind {
                            "audio" => Icon::Audio,
                            "image" => Icon::Image,
                            "caption" => Icon::Captions,
                            "project" => Icon::Sequence,
                            _ => Icon::Film,
                        };
                        icons::paint(ui.painter(), Rect::from_center_size(tr.center(), vec2(26.0, 26.0)), icon, t.text_faint);
                    }
                    if hover_scrub && resp.hovered() {
                        let x = tr.min.x + tr.width() * frac;
                        ui.painter().line_segment([pos2(x, tr.min.y), pos2(x, tr.max.y)], Stroke::new(1.0, Color32::from_rgb(0x5a, 0x9b, 0xf0)));
                    }
                }
                ui.painter().with_clip_rect(Rect::from_min_max(pos2(tr.min.x, tr.max.y), pos2(tr.max.x, tr.max.y + 22.0))).text(
                    pos2(tr.min.x, tr.max.y + 11.0),
                    Align2::LEFT_CENTER,
                    &e.name,
                    Tokens::ui(11.5),
                    t.text,
                );
                app.auto.add(&format!("mediaBrowser.entry.{}", e.name), tr, &e.name);
                entry_interactions(app, ui, &resp, e, entries);
            }
        }
    });
}

fn draw_preview(app: &mut FilmcraftApp, ui: &egui::Ui, ctx: &egui::Context, e: &Entry, r: Rect, frac: f32) -> bool {
    let Some((proj, id, dur, rate, src_w)) = preview_item(app, ctx, e) else { return false };
    let t = filmcraft_time::Tick((dur.0 as f64 * frac as f64) as i64);
    let frame = rate.frame_at(crate::panels::project::quantize(t));
    let width = (r.width() as u32).clamp(96, 320);
    let rev = if proj.item(id).is_some() && id.0 < PREVIEW_BASE { app.item_revision(id) } else { 0 };
    let key = FrameKey { target: Target::Item(id), frame, size: width, revision: rev, draft: false };
    let name = format!("mbthumb-{}-{}-{}", id.0, frame, width);
    let tex = if let Some(img) = app.frames.get(&key) {
        let tid = app.texture_for(ctx, &name, key, &img);
        Some((tid, vec2(img.w as f32, img.h as f32)))
    } else {
        app.frames.request(key, rate.tick_of(frame), width as f32 / src_w.max(1) as f32, &proj, 40);
        app.texture_existing(&name)
    };
    match tex {
        Some((tid, sz)) => {
            ui.painter().image(tid, crate::panels::monitor::fit(r, sz.x, sz.y), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
            true
        }
        None => false,
    }
}

// ------------------------------------------------------------------------------------ menu, dialog

/// The Media Browser's ≡ menu. Returns true to close it.
pub fn panel_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui) -> bool {
    let ctx = ui.ctx().clone();
    let prefs = app.session.prefs.media_browser.clone();
    let dir = mb::current_dir(&app.session);
    let has_sel = !app.session.browser.selection.is_empty();
    let mut close = false;
    let item = |app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, label: &str, enabled: bool, checked: Option<bool>, shortcut: &str| -> bool {
        let b = match checked {
            Some(c) => egui::Button::selectable(c, label),
            None => egui::Button::new(label),
        };
        let r = ui.add_enabled(enabled, b.shortcut_text(shortcut));
        app.auto.add(&format!("mediaBrowser.menu.{id}"), r.rect, label);
        r.clicked()
    };
    if item(app, ui, "editColumns", tl!("Edit Columns…"), true, None, "") {
        app.ui.media_browser.edit_columns = Some(prefs.columns.clone());
        close = true;
    }
    if item(app, ui, "importAsImageSequence", tl!("Import as Image Sequence"), true, Some(prefs.import_as_image_sequence), "") {
        exec(app, &ctx, "mediaBrowser.settings", json!({"importAsImageSequence": !prefs.import_as_image_sequence}));
        close = true;
    }
    if item(app, ui, "hoverScrub", tl!("Hover Scrub"), true, Some(prefs.hover_scrub), "") {
        exec(app, &ctx, "mediaBrowser.settings", json!({"hoverScrub": !prefs.hover_scrub}));
        close = true;
    }
    item(app, ui, "newPanel", tl!("New Media Browser Panel"), false, None, "");
    let mprefs = app.session.prefs.media.clone();
    if item(app, ui, "createFolderForImportedProjects", tl!("Create Folder For Imported Projects"), true, Some(mprefs.create_folder_for_imported_projects), "")
    {
        let mut p = app.session.prefs.clone();
        p.media.create_folder_for_imported_projects = !mprefs.create_folder_for_imported_projects;
        let _ = app.session.set_prefs(p);
        close = true;
    }
    if item(app, ui, "allowDuplicateMedia", tl!("Allow Duplicate Media During Project Import"), true, Some(mprefs.allow_duplicate_media), "") {
        let mut p = app.session.prefs.clone();
        p.media.allow_duplicate_media = !mprefs.allow_duplicate_media;
        let _ = app.session.set_prefs(p);
        close = true;
    }
    if item(app, ui, "refresh", tl!("Refresh"), true, None, "") {
        refresh(&ctx);
        app.session.browser.probes.clear();
        close = true;
    }
    ui.separator();
    let fav = prefs.favorites.contains(&dir);
    if item(app, ui, "favorite", if fav { tl!("Remove from Favorites") } else { tl!("Add to Favorites") }, true, None, "") {
        exec(app, &ctx, "mediaBrowser.favorite", json!({"remove": fav}));
        close = true;
    }
    if item(app, ui, "clearRecent", tl!("Clear Recent Directories"), !prefs.recent.is_empty(), None, "") {
        exec(app, &ctx, "mediaBrowser.clearRecent", json!({}));
        close = true;
    }
    if item(app, ui, "import", tl!("Import"), has_sel, None, "") {
        exec(app, &ctx, "mediaBrowser.import", json!({}));
        close = true;
    }
    if item(app, ui, "openInSource", tl!("Open In Source Monitor"), has_sel, None, "Shift+O") {
        exec(app, &ctx, "mediaBrowser.openInSource", json!({}));
        close = true;
    }
    close
}

/// Edit Columns… dialog.
pub fn dialogs(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut cols) = app.ui.media_browser.edit_columns.clone() else { return };
    let mut close = false;
    let mut ok = false;
    egui::Window::new(tl!("Edit Columns"))
        .id(egui::Id::new("mb-edit-columns"))
        .collapsible(false)
        .resizable(false)
        .anchor(Align2::CENTER_CENTER, [0.0, 0.0])
        .show(ctx, |ui| {
            let mut mv: Option<(usize, isize)> = None;
            for c in mb::ALL_COLUMNS {
                ui.horizontal(|ui| {
                    let mut on = cols.iter().any(|x| x == c);
                    let r = ui.add_enabled(c != "Name", egui::Checkbox::new(&mut on, crate::i18n::t(c)));
                    app.auto.add(&format!("mediaBrowser.columns.{c}"), r.rect, c);
                    if r.changed() {
                        if on {
                            cols.push(c.to_string());
                        } else {
                            cols.retain(|x| x != c);
                        }
                    }
                    if let Some(i) = cols.iter().position(|x| x == c).filter(|i| *i > 0) {
                        if ui.add_enabled(i > 1, egui::Button::new("▲").small()).clicked() {
                            mv = Some((i, -1));
                        }
                        if ui.add_enabled(i + 1 < cols.len(), egui::Button::new("▼").small()).clicked() {
                            mv = Some((i, 1));
                        }
                    }
                });
            }
            if let Some((i, d)) = mv {
                cols.swap(i, (i as isize + d) as usize);
            }
            ui.separator();
            ui.horizontal(|ui| {
                let r = ui.button(tl!("Cancel"));
                app.auto.add("mediaBrowser.columns.cancel", r.rect, "Cancel");
                close |= r.clicked();
                let r = ui.button(tl!("OK"));
                app.auto.add("mediaBrowser.columns.ok", r.rect, "OK");
                ok = r.clicked();
            });
        });
    if ok {
        let r = app.session.execute("mediaBrowser.settings", json!({"columns": cols}));
        if let Err(e) = r {
            app.ui.status = e.to_string();
        }
        close = true;
    }
    app.ui.media_browser.edit_columns = if close || ctx.input(|i| i.key_pressed(egui::Key::Escape)) { None } else { Some(cols) };
}

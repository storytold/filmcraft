//! The Project panel's three views: List (columns from Metadata Display, sortable and resizable
//! headers, inline rename, label column, optional thumbnails), Icon (thumbnail cards with hover
//! scrub, In/Out bar, poster frames, Sort Icons) and Freeform (clip cards placed freely, stacks,
//! grid).

use egui::{Align2, Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use filmcraft_engine::project_panel::{self as pp, Card, SortSpec, ViewMode};
use filmcraft_project::{Bin, BinEntry, ItemId, ItemKind};
use filmcraft_time::Tick;
use serde_json::json;

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::panels::project::{Actions, Hover, Inst, View, commit_rename, font, open_bin, quantize};
use crate::theme::Tokens;

const OFFLINE: Color32 = Color32::from_rgb(0xe8, 0x5c, 0x5c);
const BIN_COLOR: Color32 = Color32::from_rgb(237, 150, 58);
/// Width of the label chip lead-in before the Name column.
const LEAD: f32 = 26.0;

pub fn item_icon(k: &ItemKind) -> Icon {
    match k {
        ItemKind::Media(m) => match m.info.kind {
            filmcraft_media::MediaKind::AudioOnly => Icon::Audio,
            filmcraft_media::MediaKind::Still | filmcraft_media::MediaKind::ImageSequence => Icon::Image,
            _ => Icon::Film,
        },
        ItemKind::Sequence(_) => Icon::Sequence,
        ItemKind::Subclip { .. } => Icon::Film,
        ItemKind::AdjustmentLayer { .. } | ItemKind::Graphic { .. } => Icon::Adjust,
    }
}

/// Offline / proxy state of a project item for the badges.
pub struct Badge {
    pub offline: bool,
    pub offline_text: &'static str,
    pub proxy: bool,
    /// Proxies are enabled (the badge is lit).
    pub proxy_on: bool,
}

pub fn media_badge(app: &FilmcraftApp, it: &filmcraft_project::ProjectItem) -> Badge {
    let mut b = Badge { offline: false, offline_text: "", proxy: false, proxy_on: false };
    let target = match &it.kind {
        ItemKind::Subclip { parent, .. } => app.session.project.item(*parent),
        _ => Some(it),
    };
    let Some(m) = target.and_then(|t| t.as_media()) else { return b };
    let id = target.map(|t| t.id).unwrap_or(it.id);
    if m.offline {
        (b.offline, b.offline_text) = (true, tl!("Offline"));
    } else if app.session.offline.missing.contains(&id) {
        (b.offline, b.offline_text) = (true, tl!("Media missing"));
    } else if let Some(st) = app.session.media.offline_status(id) {
        b.offline = true;
        b.offline_text = if st.reason == filmcraft_render::offline::OfflineReason::Unreadable { tl!("Unreadable") } else { tl!("Media missing") };
    }
    b.proxy = m.proxy.is_some();
    b.proxy_on = app.session.media.use_proxies();
    b
}

fn paint_badges(ui: &egui::Ui, b: &Badge, at: egui::Pos2, t: &Tokens) {
    let mut x = at.x;
    if b.offline {
        icons::paint(ui.painter(), Rect::from_center_size(pos2(x + 7.0, at.y), vec2(13.0, 13.0)), Icon::Offline, OFFLINE);
        x += 18.0;
    }
    if b.proxy {
        let pr = Rect::from_min_size(pos2(x, at.y - 7.0), vec2(16.0, 14.0));
        ui.painter().rect_filled(pr, 2.0, if b.proxy_on { t.accent } else { Color32::from_gray(70) });
        ui.painter().text(pr.center(), Align2::CENTER_CENTER, "P", Tokens::ui(10.0), Color32::WHITE);
    }
}

fn matches_filter(app: &FilmcraftApp, id: ItemId, filter: &str) -> bool {
    app.session.project.item(id).is_some_and(|i| pp::listed(&app.session.project, i) && (filter.is_empty() || i.name.to_ascii_lowercase().contains(filter)))
}

fn label_color(app: &FilmcraftApp, l: filmcraft_project::Label) -> Color32 {
    let c = app.session.prefs.labels.rgb(l);
    Color32::from_rgb(c[0], c[1], c[2])
}

/// Items in display order for a view (keyboard navigation): the list's expanded rows, or the icon
/// / freeform cards.
pub fn items_in_view(app: &FilmcraftApp, v: &View, filter: &str) -> Vec<ItemId> {
    let p = &app.session.project;
    let Some(bin) = p.root.find_bin(v.bin) else { return Vec::new() };
    match v.mode {
        ViewMode::List => {
            let sort = &app.session.prefs.project_panel.view.sort;
            let mut out = Vec::new();
            fn walk(app: &FilmcraftApp, b: &Bin, sort: &SortSpec, filter: &str, out: &mut Vec<ItemId>) {
                let (bins, items) = pp::bin_children(&app.session.project, b, sort);
                for bid in bins {
                    if (app.ui.expanded_bins.contains(&bid.0) || !filter.is_empty())
                        && let Some(sub) = app.session.project.root.find_bin(bid)
                    {
                        walk(app, sub, sort, filter, out);
                    }
                }
                out.extend(items.into_iter().filter(|i| matches_filter(app, *i, filter)));
            }
            walk(app, bin, sort, filter, &mut out);
            out
        }
        ViewMode::Icon => icon_order(app, bin, filter),
        ViewMode::Freeform => {
            let mut ids: Vec<ItemId> = Vec::new();
            for e in &bin.children {
                if let BinEntry::Item(i) = e
                    && matches_filter(app, *i, filter)
                {
                    ids.push(*i);
                }
            }
            ids
        }
    }
}

/// Icon view order: Sort Icons column, else the bin's order (User Order). A search shows matches
/// from sub-bins too.
fn icon_order(app: &FilmcraftApp, bin: &Bin, filter: &str) -> Vec<ItemId> {
    let mut ids = Vec::new();
    if filter.is_empty() {
        for e in &bin.children {
            if let BinEntry::Item(i) = e {
                ids.push(*i);
            }
        }
    } else {
        bin.all_items(&mut ids);
    }
    ids.retain(|i| matches_filter(app, *i, filter));
    pp::sort_items(&app.session.project, &mut ids, &app.session.prefs.project_panel.view.icon_sort);
    ids
}

// ------------------------------------------------------------------------------------ shared

/// Modifiers of this frame's click (the pointer event's own, so synthetic Cmd-clicks count too).
pub fn click_modifiers(ui: &egui::Ui) -> egui::Modifiers {
    ui.input(|i| {
        i.events
            .iter()
            .rev()
            .find_map(|e| match e {
                egui::Event::PointerButton { pressed: false, modifiers, .. } => Some(*modifiers),
                _ => None,
            })
            .unwrap_or(i.modifiers)
    })
}

/// Click / double-click / drag / context menu of an item (row, card).
pub fn item_interactions(
    app: &mut FilmcraftApp,
    ui: &egui::Ui,
    resp: &egui::Response,
    id: ItemId,
    kind: &ItemKind,
    actions: &mut Actions,
    drag: bool,
    menu: bool,
) {
    if resp.clicked() {
        let mods = click_modifiers(ui);
        let mut sel = app.session.state.project_selection.clone();
        if mods.command || mods.shift {
            if let Some(p) = sel.iter().position(|x| *x == id) {
                sel.remove(p);
            } else {
                sel.push(id);
            }
        } else {
            sel = vec![id];
        }
        app.ui.project_panel.selected_bin = None;
        actions.push(("project.select".into(), json!({"items": sel.iter().map(|i| i.0).collect::<Vec<_>>()})));
    }
    if resp.double_clicked() {
        let matte = matches!(kind, ItemKind::Media(m) if matches!(m.media, filmcraft_engine::project::MediaRef::Generator(filmcraft_media::Generator::ColorMatte { .. })));
        if matte {
            // like Premiere: double-clicking a Color Matte opens its color picker
            actions.push(("project.select".into(), json!({"items": [id.0]})));
            actions.push(("project.matteColor".into(), json!({})));
        } else {
            let cmd = if matches!(kind, ItemKind::Sequence(_)) { "sequence.open" } else { "source.open" };
            actions.push((cmd.into(), json!({"item": id.0})));
        }
    }
    if drag && resp.drag_started() {
        crate::panels::start_drag_item(ui, id);
    }
    if menu {
        resp.context_menu(|ui| item_menu(app, ui, id, kind, actions));
    }
}

fn item_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: ItemId, kind: &ItemKind, actions: &mut Actions) {
    let sel = || json!({"items": [id.0]});
    if ui.button(tl!("Rename")).clicked() {
        actions.push(("projectPanel.rename".into(), json!({"item": id.0})));
        ui.close();
    }
    if ui.button(tl!("Open in Source Monitor")).clicked() {
        actions.push(("source.open".into(), json!({"item": id.0})));
        ui.close();
    }
    if matches!(kind, ItemKind::Sequence(_)) && ui.button(tl!("Open in Timeline")).clicked() {
        actions.push(("sequence.open".into(), json!({"item": id.0})));
        ui.close();
    }
    if ui.button(tl!("New Sequence From Clip")).clicked() {
        actions.push(("file.newSequence".into(), json!({"fromItem": id.0})));
        ui.close();
    }
    if ui.button(tl!("Duplicate")).clicked() {
        actions.push(("project.select".into(), sel()));
        actions.push(("edit.duplicate".into(), json!({})));
        ui.close();
    }
    if ui.button(tl!("Set Poster Frame")).clicked() {
        // the hover-scrubbed time when there is one
        let h = app.ui.project_panel.hover.filter(|h| h.item == id.0);
        actions.push(("project.select".into(), sel()));
        actions.push(("clip.setPosterFrame".into(), h.map(|h| json!({"item": id.0, "time": h.time})).unwrap_or(json!({}))));
        ui.close();
    }
    if ui.button(tl!("Interpret Footage…")).clicked() {
        actions.push(("clip.interpretFootage".into(), json!({"items": [id.0]})));
        ui.close();
    }
    // subclips: Make Subclip (media), Edit Subclip and Convert to Master Clip (subclips)
    let subclip_items: Vec<(&str, &str, &str)> = match kind {
        ItemKind::Media(_) => vec![("makeSubclip", tl!("Make Subclip…"), "clip.makeSubclip")],
        ItemKind::Subclip { .. } => {
            vec![("editSubclip", tl!("Edit Subclip…"), "clip.editSubclip"), ("convertToMaster", tl!("Convert to Master Clip"), "clip.editSubclip")]
        }
        _ => Vec::new(),
    };
    for (key, label, cmd) in &subclip_items {
        let b = ui.button(*label);
        app.auto.add(&format!("project.itemMenu.{key}"), b.rect, label);
        if b.clicked() {
            actions.push(("project.select".into(), sel()));
            let p = if *key == "convertToMaster" { json!({"item": id.0, "convertToMaster": true}) } else { json!({}) };
            actions.push((cmd.to_string(), p));
            ui.close();
        }
    }
    ui.menu_button(tl!("Label"), |ui| {
        for l in filmcraft_project::Label::ALL {
            if ui.button(crate::i18n::t(l.name())).clicked() {
                actions.push(("project.select".into(), json!({"items": [id.0]})));
                actions.push(("edit.label".into(), json!({"label": l.name()})));
                ui.close();
            }
        }
    });
    if matches!(kind, ItemKind::Media(m) if matches!(m.media, filmcraft_project::MediaRef::File { .. })) {
        ui.separator();
        for (label, cmd) in [(tl!("Link Media…"), "media.linkMedia"), (tl!("Make Offline…"), "media.makeOffline")] {
            if ui.button(label).clicked() {
                actions.push(("project.select".into(), sel()));
                actions.push((cmd.into(), json!({})));
                ui.close();
            }
        }
        ui.menu_button(tl!("Proxy"), |ui| {
            for (label, cmd) in [
                (tl!("Create Proxies…"), "media.createProxies"),
                (tl!("Attach Proxies…"), "media.attachProxies"),
                (tl!("Reconnect Full Resolution Media…"), "media.reconnectFullRes"),
                (tl!("Detach Proxies"), "media.detachProxies"),
            ] {
                if ui.button(label).clicked() {
                    actions.push(("project.select".into(), json!({"items": [id.0]})));
                    actions.push((cmd.into(), json!({})));
                    ui.close();
                }
            }
        });
    }
    ui.separator();
    if ui.button(tl!("Clear")).clicked() {
        actions.push(("project.delete".into(), json!({"items": [id.0]})));
        ui.close();
    }
}

fn bin_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &View, bin: u64, actions: &mut Actions) {
    for (label, how) in [(tl!("Open in Place"), "inPlace"), (tl!("Open in New Tab"), "newTab"), (tl!("Open in New Window"), "newWindow")] {
        let b = ui.button(label);
        app.auto.add(&format!("{}.binMenu.{how}", v.prefix), b.rect, label);
        if b.clicked() {
            let r = open_bin(app, v.inst, bin, Some(&format!("open{}{}", how[..1].to_uppercase(), &how[1..])), egui::Modifiers::NONE);
            if let Err(e) = r {
                app.ui.status = e;
            }
            ui.close();
        }
    }
    ui.separator();
    if ui.button(tl!("Rename")).clicked() {
        actions.push(("projectPanel.rename".into(), json!({"bin": bin})));
        ui.close();
    }
    if ui.button(tl!("New Bin")).clicked() {
        actions.push(("file.newBin".into(), json!({"name": "New Bin", "parent": bin})));
        ui.close();
    }
}

/// Drop project items dragged onto a bin: move them (the selection when the dragged item is in it).
fn accept_bin_drop(app: &FilmcraftApp, ui: &egui::Ui, r: Rect, bin: u64, actions: &mut Actions) -> bool {
    let Some(item) = crate::panels::dragged_project_item(ui) else { return false };
    if !ui.rect_contains_pointer(r) {
        return false;
    }
    ui.painter().rect_stroke(r, 3.0, Stroke::new(1.5, app.tokens.accent), StrokeKind::Inside);
    if ui.input(|i| i.pointer.any_released()) {
        let sel = &app.session.state.project_selection;
        let items: Vec<u64> = if sel.contains(&item) { sel.iter().map(|i| i.0).collect() } else { vec![item.0] };
        actions.push(("project.moveToBin".into(), json!({"items": items, "bin": bin})));
        crate::panels::clear_drag(ui);
    }
    true
}

/// The inline rename field at `r`; true while it is shown.
fn rename_field(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, item: Option<u64>, bin: Option<u64>, pre: &str) -> bool {
    let Some(rn) = app.ui.project_panel.rename.clone() else { return false };
    if rn.item != item || rn.bin != bin || item.is_none() && bin.is_none() || (!rn.panel.is_empty() && rn.panel != pre) {
        return false;
    }
    let mut text = rn.text;
    let id = egui::Id::new(("rename-field", pre, item, bin));
    let resp = ui.put(r, egui::TextEdit::singleline(&mut text).id(id).font(egui::FontId::proportional(font(app))).margin(vec2(3.0, 1.0)));
    app.auto.add(&format!("{pre}.rename"), r, "Rename");
    // take keyboard focus the first frame the field is shown
    let shown = egui::Id::new("project-rename-shown");
    if ui.ctx().data(|d| d.get_temp::<egui::Id>(shown)) != Some(id) {
        ui.ctx().data_mut(|d| d.insert_temp(shown, id));
        resp.request_focus();
    }
    if let Some(rn) = app.ui.project_panel.rename.as_mut() {
        rn.text = text;
    }
    let done = if ui.input(|i| i.key_pressed(egui::Key::Escape)) {
        app.ui.project_panel.rename = None;
        true
    } else if resp.lost_focus() {
        commit_rename(app);
        true
    } else {
        false
    };
    if done {
        ui.ctx().data_mut(|d| d.remove::<egui::Id>(shown));
    }
    true
}

/// A second click on the name of the only selected item starts renaming it (after the
/// double-click interval, so a double-click still opens it).
fn slow_click_rename(app: &mut FilmcraftApp, ui: &egui::Ui, resp: &egui::Response, name_rect: Rect, id: ItemId, was_selected: bool) {
    let key = egui::Id::new("project-rename-pending");
    let now = ui.input(|i| i.time);
    if resp.double_clicked() {
        ui.ctx().data_mut(|d| d.remove::<(u64, f64)>(key));
        return;
    }
    if resp.clicked()
        && was_selected
        && app.session.state.project_selection.len() == 1
        && ui.input(|i| i.modifiers.is_none())
        && resp.interact_pointer_pos().is_some_and(|p| name_rect.contains(p))
    {
        ui.ctx().data_mut(|d| d.insert_temp(key, (id.0, now + 0.5)));
    }
    if let Some((pid, at)) = ui.ctx().data(|d| d.get_temp::<(u64, f64)>(key))
        && pid == id.0
    {
        if now >= at {
            ui.ctx().data_mut(|d| d.remove::<(u64, f64)>(key));
            if app.session.state.project_selection == [id] {
                let name = app.session.project.item(id).map(|i| i.name.clone()).unwrap_or_default();
                app.ui.project_panel.rename = Some(crate::panels::project::Rename { item: Some(id.0), bin: None, text: name, panel: String::new() });
            }
        } else {
            ui.ctx().request_repaint();
        }
    }
}

// ------------------------------------------------------------------------------------ list view

struct ListCtx {
    cols: Vec<pp::Column>,
    row_h: f32,
    font: f32,
    thumbs: bool,
    width: f32,
}

pub fn list_view(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, v: &View, filter: &str, actions: &mut Actions) {
    let t = app.tokens;
    let f = font(app);
    let thumbs = app.session.prefs.project_panel.thumbnails;
    let cols = app.session.prefs.project_panel.view.columns.clone();
    let width = LEAD + cols.iter().map(|c| c.width).sum::<f32>() + 8.0;
    let lc = ListCtx { row_h: if thumbs { (f * 3.6).max(44.0) } else { (f * 2.0).max(22.0) }, font: f, thumbs, width: width.max(rect.width()), cols };
    let header_h = (f * 1.8).max(20.0);
    let body = Rect::from_min_max(pos2(rect.min.x, rect.min.y + header_h), rect.max);
    let mut bui = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt((&v.prefix, "list-body")));
    bui.set_clip_rect(body.intersect(ui.clip_rect()));
    let root = app.session.project.root.find_bin(v.bin).cloned().unwrap_or_default();
    let out = egui::ScrollArea::both().id_salt((&v.prefix, "list-scroll")).auto_shrink([false, false]).show(&mut bui, |ui| {
        ui.set_min_width(lc.width);
        let mut row = 0usize;
        list_bin(app, ui, &root, 0, filter, &mut row, actions, v, &lc);
        if v.inst == Inst::Main && v.bin == app.session.project.root.id {
            let mut draw = |app: &mut FilmcraftApp, ui: &mut egui::Ui, bin: &Bin, row: &mut usize, actions: &mut Actions| {
                list_bin(app, ui, bin, 1, "", row, actions, v, &lc);
            };
            crate::panels::menu_dialogs::search_bin_rows(app, ui, &mut row, actions, &mut draw);
        }
        empty_space(app, ui, v, actions);
    });
    // pinned header, scrolled horizontally with the rows
    let hr = Rect::from_min_size(rect.min, vec2(rect.width(), header_h));
    let hp = ui.painter().with_clip_rect(hr.intersect(ui.clip_rect()));
    hp.rect_filled(hr, 0.0, t.panel_bg);
    hp.line_segment([hr.left_bottom(), hr.right_bottom()], Stroke::new(1.0, t.separator));
    let mut x = rect.min.x + LEAD - out.state.offset.x;
    let sort = app.session.prefs.project_panel.view.sort.clone();
    for (i, c) in lc.cols.iter().enumerate() {
        let cr = Rect::from_min_size(pos2(x, hr.min.y), vec2(c.width, header_h));
        let vis = cr.intersect(hr);
        if vis.width() > 1.0 {
            let id = egui::Id::new((&v.prefix, "col", &c.name));
            let resp = ui.interact(vis, id, Sense::click()).on_hover_text(crate::i18n::t(&c.name));
            app.auto.add(&format!("{}.list.header.{}", v.prefix, c.name), vis, &c.name);
            if resp.hovered() {
                hp.rect_filled(cr, 0.0, t.hover);
            }
            let g = hp.layout_no_wrap(crate::i18n::t(&c.name).to_string(), Tokens::ui(f - 0.5), t.text_dim);
            let gw = g.size().x;
            hp.with_clip_rect(cr.shrink2(vec2(4.0, 0.0)).intersect(hr)).galley(pos2(cr.min.x + 6.0, cr.center().y - g.size().y / 2.0), g, t.text_dim);
            if sort.column == c.name {
                let ax = (cr.min.x + 10.0 + gw).min(cr.max.x - 8.0);
                let (a, b) = if sort.descending { (cr.center().y + 4.0, cr.center().y - 4.0) } else { (cr.center().y - 4.0, cr.center().y + 4.0) };
                hp.line_segment([pos2(ax, b), pos2(ax, a)], Stroke::new(1.3, t.text_dim));
                hp.line_segment([pos2(ax - 3.0, a + (b - a).signum() * 3.0), pos2(ax, a)], Stroke::new(1.3, t.text_dim));
                hp.line_segment([pos2(ax + 3.0, a + (b - a).signum() * 3.0), pos2(ax, a)], Stroke::new(1.3, t.text_dim));
            }
            if resp.clicked() {
                actions.push(("project.sort".into(), json!({"column": c.name})));
            }
            resp.context_menu(|ui| {
                if ui.button(tl!("Metadata Display…")).clicked() {
                    actions.push(("projectPanel.metadataDisplay".into(), json!({})));
                    ui.close();
                }
            });
        }
        // resize handle on the column's right edge
        let hr2 = Rect::from_center_size(pos2(cr.max.x, hr.center().y), vec2(8.0, header_h));
        if hr2.intersects(hr) {
            let id = egui::Id::new((&v.prefix, "colsize", &c.name));
            let resp = ui.interact(hr2.intersect(hr), id, Sense::drag()).on_hover_cursor(egui::CursorIcon::ResizeColumn);
            app.auto.add(&format!("{}.list.resize.{}", v.prefix, c.name), hr2.intersect(hr), "Resize column");
            hp.line_segment([pos2(cr.max.x, hr.min.y + 4.0), pos2(cr.max.x, hr.max.y - 4.0)], Stroke::new(1.0, t.separator));
            if resp.dragged() {
                let w = (c.width + resp.drag_delta().x).clamp(30.0, 800.0);
                if let Some(col) = app.session.prefs.project_panel.view.columns.get_mut(i) {
                    col.width = w;
                }
            }
            if resp.drag_stopped() {
                let w = app.session.prefs.project_panel.view.columns.get(i).map(|c| c.width).unwrap_or(c.width);
                actions.push(("project.columns.resize".into(), json!({"column": c.name, "width": w})));
            }
        }
        x += c.width;
    }
}

/// Empty space under the rows / cards: click deselects, double-click imports, right-click menu.
fn empty_space(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &View, actions: &mut Actions) {
    let rest = ui.available_rect_before_wrap();
    let r = Rect::from_min_size(rest.min, vec2(rest.width(), rest.height().max(40.0)));
    let resp = ui.allocate_rect(r, Sense::click());
    app.auto.add(&format!("{}.empty", v.prefix), r.intersect(ui.clip_rect()), "Empty area");
    if resp.clicked() {
        app.ui.project_panel.selected_bin = None;
        actions.push(("project.select".into(), json!({"items": []})));
    }
    if resp.double_clicked() {
        actions.push(("file.import".into(), json!({})));
    }
    let bin = (v.bin != app.session.project.root.id).then_some(v.bin.0);
    resp.context_menu(|ui| background_menu(app, ui, v, bin, actions));
}

fn background_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &View, bin: Option<u64>, actions: &mut Actions) {
    for (label, cmd) in [
        (tl!("New Bin"), "file.newBin"),
        (tl!("New Search Bin"), "file.newSearchBin"),
        (tl!("Find…"), "edit.find"),
        (tl!("Automate to Sequence…"), "clip.automateToSequence"),
        (tl!("Import…"), "file.import"),
    ] {
        if ui.button(label).clicked() {
            let p = if cmd == "file.newBin" { json!({"name": tl!("New Bin"), "parent": bin}) } else { json!({}) };
            actions.push((cmd.into(), p));
            ui.close();
        }
    }
    ui.menu_button(tl!("New Item"), |ui| crate::panels::project::new_item_menu(app, ui, &v.prefix, actions));
}

#[allow(clippy::too_many_arguments)]
fn list_bin(app: &mut FilmcraftApp, ui: &mut egui::Ui, bin: &Bin, depth: usize, filter: &str, row: &mut usize, actions: &mut Actions, v: &View, lc: &ListCtx) {
    let t = app.tokens;
    let (bins, items) = pp::bin_children(&app.session.project, bin, &app.session.prefs.project_panel.view.sort);
    for bid in bins {
        let Some(b) = app.session.project.root.find_bin(bid).cloned() else { continue };
        let open = app.ui.expanded_bins.contains(&b.id.0) || !filter.is_empty();
        let (r, resp) = ui.allocate_exact_size(vec2(lc.width, lc.row_h), Sense::click());
        let selected = app.ui.project_panel.selected_bin == Some(b.id.0);
        if selected {
            ui.painter().rect_filled(r, 0.0, t.row_selected);
        } else if *row % 2 == 1 {
            ui.painter().rect_filled(r, 0.0, t.row_alt);
        }
        *row += 1;
        let x = r.min.x + LEAD + depth as f32 * 14.0;
        let tri = Rect::from_center_size(pos2(x + 5.0, r.center().y), vec2(14.0, 14.0));
        icons::paint(ui.painter(), tri.shrink(2.0), if open { Icon::ChevronDown } else { Icon::ChevronRight }, t.text_dim);
        icons::paint(ui.painter(), Rect::from_center_size(pos2(x + 20.0, r.center().y), vec2(14.0, 14.0)), Icon::Folder, BIN_COLOR);
        let name_r = Rect::from_min_max(pos2(x + 30.0, r.min.y + 2.0), pos2((r.min.x + LEAD + lc.cols[0].width - 4.0).max(x + 80.0), r.max.y - 2.0));
        if !rename_field(app, ui, name_r, None, Some(b.id.0), &v.prefix) {
            ui.painter().with_clip_rect(name_r).text(pos2(name_r.min.x + 2.0, r.center().y), Align2::LEFT_CENTER, &b.name, Tokens::ui(lc.font), t.text);
        }
        app.auto.add(&format!("{}.bin.{}", v.prefix, b.id.0), r.intersect(ui.clip_rect()), &b.name);
        app.auto.add(&format!("{}.bin.{}.toggle", v.prefix, b.id.0), tri, "Expand");
        accept_bin_drop(app, ui, r, b.id.0, actions);
        if resp.clicked() {
            let on_tri = resp.interact_pointer_pos().is_some_and(|p| p.x < x + 12.0);
            if on_tri || app.ui.project_panel.selected_bin == Some(b.id.0) && !resp.double_clicked() {
                if open {
                    app.ui.expanded_bins.retain(|x| *x != b.id.0);
                } else {
                    app.ui.expanded_bins.push(b.id.0);
                }
            }
            app.ui.project_panel.selected_bin = Some(b.id.0);
            actions.push(("project.select".into(), json!({"items": []})));
        }
        if resp.double_clicked() {
            let mods = click_modifiers(ui);
            if let Err(e) = open_bin(app, v.inst, b.id.0, None, mods) {
                app.ui.status = e;
            }
        }
        resp.context_menu(|ui| bin_menu(app, ui, v, b.id.0, actions));
        if open {
            list_bin(app, ui, &b, depth + 1, filter, row, actions, v, lc);
        }
    }
    for id in items {
        if !matches_filter(app, id, filter) {
            continue;
        }
        let Some(it) = app.session.project.item(id).cloned() else { continue };
        let (r, resp) = ui.allocate_exact_size(vec2(lc.width, lc.row_h), Sense::click_and_drag());
        let selected = app.session.state.project_selection.contains(&id);
        if selected {
            ui.painter().rect_filled(r, 0.0, t.row_selected);
        } else if *row % 2 == 1 {
            ui.painter().rect_filled(r, 0.0, t.row_alt);
        }
        *row += 1;
        // label chip
        let chip = Rect::from_center_size(pos2(r.min.x + 13.0, r.center().y), vec2(12.0, 12.0));
        ui.painter().rect_filled(chip, 2.5, label_color(app, it.label));
        app.auto.add(&format!("{}.item.{}.label", v.prefix, id.0), chip, it.label.name());
        let mut x = r.min.x + LEAD;
        let name_w = lc.cols.first().map(|c| c.width).unwrap_or(200.0);
        let nx = x + depth as f32 * 14.0 + 14.0;
        let mut icon_x = nx;
        if lc.thumbs {
            let th = Rect::from_min_size(pos2(nx, r.min.y + 3.0), vec2((lc.row_h - 6.0) * 16.0 / 9.0, lc.row_h - 6.0));
            thumb(app, ui, th, &it, None);
            icon_x = th.max.x + 4.0;
        }
        icons::paint(ui.painter(), Rect::from_center_size(pos2(icon_x + 7.0, r.center().y), vec2(14.0, 14.0)), item_icon(&it.kind), t.icon);
        let name_r = Rect::from_min_max(pos2(icon_x + 18.0, r.min.y + 2.0), pos2(x + name_w - 4.0, r.max.y - 2.0));
        let badge = media_badge(app, &it);
        if !rename_field(app, ui, name_r, Some(id.0), None, &v.prefix) {
            let nr = ui.painter().with_clip_rect(name_r).text(
                pos2(name_r.min.x + 2.0, r.center().y),
                Align2::LEFT_CENTER,
                &it.name,
                Tokens::ui(lc.font),
                if badge.offline { OFFLINE } else { t.text },
            );
            if nr.max.x + 40.0 < name_r.max.x {
                paint_badges(ui, &badge, pos2(nr.max.x + 6.0, r.center().y), &t);
            }
        }
        x += name_w;
        for c in lc.cols.iter().skip(1) {
            let cr = Rect::from_min_size(pos2(x, r.min.y), vec2(c.width, r.height()));
            let p = ui.painter().with_clip_rect(cr.shrink2(vec2(2.0, 0.0)).intersect(ui.clip_rect()));
            if c.name == "Label" {
                p.rect_filled(Rect::from_center_size(pos2(cr.min.x + 12.0, cr.center().y), vec2(10.0, 10.0)), 2.0, label_color(app, it.label));
                p.text(pos2(cr.min.x + 22.0, cr.center().y), Align2::LEFT_CENTER, crate::i18n::t(it.label.name()), Tokens::ui(lc.font - 0.5), t.text_dim);
            } else {
                let (text, _) = pp::cell(&app.session.project, &it, &c.name);
                let text = if c.name == "Media Type" { crate::i18n::t(&text).to_string() } else { text };
                p.text(pos2(cr.min.x + 6.0, cr.center().y), Align2::LEFT_CENTER, text, Tokens::ui(lc.font - 0.5), t.text_dim);
            }
            x += c.width;
        }
        app.auto.add(&format!("{}.item.{}", v.prefix, id.0), r.intersect(ui.clip_rect()), &it.name);
        slow_click_rename(app, ui, &resp, name_r, id, selected);
        item_interactions(app, ui, &resp, id, &it.kind, actions, true, true);
    }
}

/// An item's thumbnail at `t` (None: the poster frame or a third in) fitted into `r`.
fn thumb(app: &mut FilmcraftApp, ui: &egui::Ui, r: Rect, it: &filmcraft_project::ProjectItem, at: Option<Tick>) {
    ui.painter().rect_filled(r, 2.0, Color32::from_rgb(12, 12, 12));
    let tt = at.or_else(|| filmcraft_engine::keyboard::poster_frame(it)).unwrap_or(Tick(it.duration().0 * 3 / 10));
    let ctx = ui.ctx().clone();
    let w = (r.width() as u32).clamp(96, 320);
    if let Some((tex, sz)) = app.thumbnail(&ctx, it.id, quantize(tt), w) {
        ui.painter().image(tex, crate::panels::monitor::fit(r, sz.x, sz.y), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    } else {
        icons::paint(
            ui.painter(),
            Rect::from_center_size(r.center(), vec2(r.height().min(28.0) * 0.8, r.height().min(28.0) * 0.8)),
            item_icon(&it.kind),
            app.tokens.text_faint,
        );
    }
}

// ------------------------------------------------------------------------------------ icon view

/// Hover scrub over a thumbnail: the media time under the pointer (recorded for I / O).
fn hover_time(app: &mut FilmcraftApp, ui: &egui::Ui, resp: &egui::Response, r: Rect, it: &filmcraft_project::ProjectItem) -> Option<Tick> {
    if !(resp.hovered() && app.session.prefs.project_panel.hover_scrub) || it.duration().0 <= 0 {
        return None;
    }
    let p = ui.ctx().pointer_hover_pos()?;
    let f = ((p.x - r.min.x) / r.width()).clamp(0.0, 1.0) as f64;
    let rate = it.frame_rate();
    let t = rate.snap(Tick((it.duration().0 as f64 * f) as i64)).min(Tick((it.duration().0 - rate.frame_duration().0).max(0)));
    app.ui.project_panel.hover = Some(Hover { item: it.id.0, time: t.0, frame: ui.ctx().cumulative_frame_nr() });
    Some(t)
}

/// A clip card: thumbnail (hover scrub, poster frame), In/Out bar, badges, name and duration.
#[allow(clippy::too_many_arguments)]
fn card(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, id: ItemId, pre: &str, show_name: bool, show_dur: bool, sense: Sense) -> Option<egui::Response> {
    let t = app.tokens;
    let it = app.session.project.item(id).cloned()?;
    let resp = ui.interact(r.expand(2.0), egui::Id::new((pre, "card", id.0)), sense);
    let th = Rect::from_min_size(r.min, vec2(r.width(), r.width() * 9.0 / 16.0));
    let selected = app.session.state.project_selection.contains(&id);
    let card_bg = Rect::from_min_max(th.min - vec2(6.0, 6.0), pos2(th.max.x + 6.0, r.max.y + 2.0));
    ui.painter().rect_filled(card_bg, 4.0, if selected { t.tl_header_bg } else { t.panel_bg });
    if selected {
        ui.painter().rect_stroke(card_bg, 4.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
    }
    let at = hover_time(app, ui, &resp, th, &it);
    thumb(app, ui, th, &it, at);
    let dur = it.duration();
    // In/Out range bar along the bottom of the thumbnail
    let marks = match &it.kind {
        ItemKind::Media(m) => (m.mark_in, m.mark_out),
        ItemKind::Sequence(q) => (q.mark_in, q.mark_out),
        _ => (None, None),
    };
    if (marks.0.is_some() || marks.1.is_some()) && dur.0 > 0 {
        let fx = |t: Tick| th.min.x + th.width() * (t.0 as f32 / dur.0 as f32).clamp(0.0, 1.0);
        let a = fx(marks.0.unwrap_or(Tick::ZERO));
        let b = fx(marks.1.map(|o| o + it.frame_rate().frame_duration()).unwrap_or(dur));
        let bar = Rect::from_min_max(pos2(a, th.max.y - 4.0), pos2(b.max(a + 2.0), th.max.y));
        ui.painter().rect_filled(bar, 0.0, Color32::from_rgb(0x5a, 0x9b, 0xf0));
        app.auto.add(&format!("{pre}.item.{}.inOut", id.0), bar, "In/Out");
    }
    // hover scrub playhead
    if let Some(ht) = at
        && dur.0 > 0
    {
        let x = th.min.x + th.width() * (ht.0 as f32 / dur.0 as f32);
        ui.painter().line_segment([pos2(x, th.min.y), pos2(x, th.max.y)], Stroke::new(1.0, Color32::from_rgb(0x5a, 0x9b, 0xf0)));
    }
    // video / audio badges
    let mut bx = th.max.x - 4.0;
    for (has, icon) in [
        (it.has_audio(), Icon::Audio),
        (it.has_video() && !matches!(it.kind, ItemKind::Sequence(_)), Icon::Film),
        (matches!(it.kind, ItemKind::Sequence(_)), Icon::Sequence),
    ] {
        if has {
            let br = Rect::from_min_max(pos2(bx - 15.0, th.max.y - 17.0), pos2(bx, th.max.y - 5.0));
            ui.painter().rect_filled(br, 2.0, Color32::from_black_alpha(160));
            icons::paint(ui.painter(), br.shrink(1.5), icon, Color32::from_rgb(0x8f, 0xb8, 0xf5));
            bx -= 17.0;
        }
    }
    let badge = media_badge(app, &it);
    if badge.offline {
        let band = Rect::from_min_max(pos2(th.min.x, th.max.y - 20.0), th.max);
        ui.painter().rect_filled(band, 0.0, Color32::from_rgba_unmultiplied(0x5c, 0x10, 0x16, 230));
        icons::paint(ui.painter(), Rect::from_center_size(pos2(band.min.x + 12.0, band.center().y), vec2(13.0, 13.0)), Icon::Offline, Color32::WHITE);
        ui.painter().text(pos2(band.min.x + 24.0, band.center().y), Align2::LEFT_CENTER, badge.offline_text, Tokens::ui(10.5), Color32::WHITE);
        app.auto.add(&format!("{pre}.item.{}.offline", id.0), band, badge.offline_text);
    }
    if badge.proxy {
        let pr = Rect::from_min_size(pos2(th.max.x - 22.0, th.min.y + 4.0), vec2(18.0, 14.0));
        ui.painter().rect_filled(pr, 2.0, if badge.proxy_on { t.accent } else { Color32::from_gray(70) });
        ui.painter().text(pr.center(), Align2::CENTER_CENTER, "P", Tokens::ui(10.0), Color32::WHITE);
        app.auto.add(&format!("{pre}.item.{}.proxy", id.0), pr, "Proxy attached");
    }
    let f = font(app);
    let ty = th.max.y + 11.0;
    let mut dw = 0.0;
    if show_dur {
        let secs = dur.seconds().max(0.0) as u64;
        let dtext = if secs >= 3600 { format!("{}:{:02}:{:02}", secs / 3600, (secs / 60) % 60, secs % 60) } else { format!("{}:{:02}", secs / 60, secs % 60) };
        let dg = ui.painter().layout_no_wrap(dtext, Tokens::ui(f - 1.0), t.text_dim);
        dw = dg.size().x;
        ui.painter().galley(pos2(th.max.x - dw, ty - dg.size().y / 2.0), dg, t.text_dim);
    }
    let nr = Rect::from_min_max(pos2(th.min.x, th.max.y + 2.0), pos2(th.max.x - dw - 6.0, th.max.y + 20.0));
    if show_name && !rename_field(app, ui, nr, Some(id.0), None, pre) {
        ui.painter().rect_filled(Rect::from_min_size(pos2(th.min.x, ty - 5.0), vec2(8.0, 10.0)), 1.5, label_color(app, it.label));
        ui.painter().with_clip_rect(nr).text(
            pos2(th.min.x + 12.0, ty),
            Align2::LEFT_CENTER,
            &it.name,
            Tokens::ui(f - 0.5),
            if badge.offline { OFFLINE } else { t.text },
        );
    }
    app.auto.add(&format!("{pre}.item.{}", id.0), th, &it.name);
    Some(resp)
}

fn bin_card(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, b: &Bin, v: &View, actions: &mut Actions) {
    let t = app.tokens;
    let th = Rect::from_min_size(r.min, vec2(r.width(), r.width() * 9.0 / 16.0));
    let resp = ui.interact(r.expand(2.0), egui::Id::new((&v.prefix, "bincard", b.id.0)), Sense::click());
    let selected = app.ui.project_panel.selected_bin == Some(b.id.0);
    let bg = Rect::from_min_max(th.min - vec2(6.0, 6.0), pos2(th.max.x + 6.0, r.max.y + 2.0));
    ui.painter().rect_filled(bg, 4.0, if selected { t.tl_header_bg } else { t.panel_bg });
    if selected {
        ui.painter().rect_stroke(bg, 4.0, Stroke::new(1.5, t.accent), StrokeKind::Inside);
    }
    ui.painter().rect_filled(th, 2.0, Color32::from_gray(26));
    icons::paint(ui.painter(), Rect::from_center_size(th.center(), vec2(th.height() * 0.6, th.height() * 0.6)), Icon::Folder, BIN_COLOR);
    let mut n = Vec::new();
    b.all_items(&mut n);
    let f = font(app);
    let nr = Rect::from_min_max(pos2(th.min.x, th.max.y + 2.0), pos2(th.max.x, th.max.y + 20.0));
    if !rename_field(app, ui, nr, None, Some(b.id.0), &v.prefix) {
        ui.painter().with_clip_rect(nr).text(pos2(th.min.x, th.max.y + 11.0), Align2::LEFT_CENTER, &b.name, Tokens::ui(f - 0.5), t.text);
        ui.painter().text(pos2(th.max.x, th.max.y + 11.0), Align2::RIGHT_CENTER, tlf!("{n} items", n = n.len()), Tokens::ui(f - 1.5), t.text_dim);
    }
    app.auto.add(&format!("{}.bin.{}", v.prefix, b.id.0), th, &b.name);
    accept_bin_drop(app, ui, th, b.id.0, actions);
    if resp.clicked() {
        app.ui.project_panel.selected_bin = Some(b.id.0);
        actions.push(("project.select".into(), json!({"items": []})));
    }
    if resp.double_clicked() {
        let mods = click_modifiers(ui);
        if let Err(e) = open_bin(app, v.inst, b.id.0, None, mods) {
            app.ui.status = e;
        }
    }
    resp.context_menu(|ui| bin_menu(app, ui, v, b.id.0, actions));
}

pub fn icon_view(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, v: &View, filter: &str, actions: &mut Actions) {
    let Some(bin) = app.session.project.root.find_bin(v.bin).cloned() else { return };
    let size = v.icon_size;
    let th_h = size * 9.0 / 16.0;
    let cell = vec2(size + 20.0, th_h + 34.0);
    let ids = icon_order(app, &bin, filter);
    let bins: Vec<Bin> = if filter.is_empty() {
        bin.children.iter().filter_map(|e| if let BinEntry::Bin(b) = e { Some(b.clone()) } else { None }).collect()
    } else {
        Vec::new()
    };
    let n = bins.len() + ids.len();
    egui::ScrollArea::vertical().id_salt((&v.prefix, "icon-scroll")).auto_shrink([false, false]).show(ui, |ui| {
        let per_row = ((rect.width() - 12.0) / cell.x).floor().max(1.0) as usize;
        if v.inst == Inst::Main {
            app.ui.keys.icon_columns = per_row;
        }
        let rows = n.div_ceil(per_row);
        for rr in 0..rows {
            let (row, _) = ui.allocate_exact_size(vec2(ui.available_width(), cell.y), Sense::hover());
            for k in 0..per_row {
                let i = rr * per_row + k;
                if i >= n {
                    break;
                }
                let r = Rect::from_min_size(pos2(row.min.x + 12.0 + k as f32 * cell.x, row.min.y + 8.0), vec2(size, th_h + 20.0));
                if i < bins.len() {
                    bin_card(app, ui, r, &bins[i], v, actions);
                    continue;
                }
                let id = ids[i - bins.len()];
                let Some(kind) = app.session.project.item(id).map(|x| x.kind.clone()) else { continue };
                let was_selected = app.session.state.project_selection.contains(&id);
                if let Some(resp) = card(app, ui, r, id, &v.prefix, true, true, Sense::click_and_drag()) {
                    let name_r = Rect::from_min_max(pos2(r.min.x, r.min.y + th_h), r.max);
                    slow_click_rename(app, ui, &resp, name_r, id, was_selected);
                    item_interactions(app, ui, &resp, id, &kind, actions, true, true);
                }
            }
        }
        empty_space(app, ui, v, actions);
    });
}

// ------------------------------------------------------------------------------------ freeform

pub fn freeform_view(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, v: &View, filter: &str, actions: &mut Actions) {
    let t = app.tokens;
    let opts = app.session.prefs.project_panel.freeform.clone();
    let mut cards: Vec<Card> = pp::freeform_layout(&app.session.project, Some(v.bin), &opts, rect.width());
    cards.retain(|c| matches_filter(app, ItemId(c.item), filter));
    let drag_key = egui::Id::new((&v.prefix, "ff-drag"));
    // (dragged item, offset so far)
    let dragging: Option<(u64, egui::Vec2)> = ui.ctx().data(|d| d.get_temp(drag_key));
    let sel: Vec<u64> = app.session.state.project_selection.iter().map(|i| i.0).collect();
    let moving = |c: &Card| -> bool {
        dragging.is_some_and(|(d, _)| {
            let stack_of_d = cards_stack(&cards, d);
            c.item == d || (sel.contains(&d) && sel.contains(&c.item)) || (stack_of_d.is_some() && c.stack == stack_of_d)
        })
    };
    let extent = cards.iter().fold(vec2(rect.width(), rect.height()), |e, c| vec2(e.x.max(c.x + c.size + 40.0), e.y.max(c.y + c.size * 9.0 / 16.0 + 60.0)));
    egui::ScrollArea::both().id_salt((&v.prefix, "ff-scroll")).auto_shrink([false, false]).show(ui, |ui| {
        let (canvas, bg) = ui.allocate_exact_size(extent, Sense::click());
        app.auto.add(&format!("{}.freeform", v.prefix), canvas, "Freeform canvas");
        if opts.snap {
            let g = opts.grid.max(4.0);
            let mut x = canvas.min.x;
            while x < canvas.max.x {
                ui.painter().line_segment([pos2(x, canvas.min.y), pos2(x, canvas.max.y)], Stroke::new(0.5, t.separator.gamma_multiply(0.5)));
                x += g;
            }
            let mut y = canvas.min.y;
            while y < canvas.max.y {
                ui.painter().line_segment([pos2(canvas.min.x, y), pos2(canvas.max.x, y)], Stroke::new(0.5, t.separator.gamma_multiply(0.5)));
                y += g;
            }
        }
        // stack sizes for the "×N" badge
        let mut stack_n: std::collections::BTreeMap<u64, usize> = Default::default();
        for c in &cards {
            if let Some(s) = c.stack {
                *stack_n.entry(s).or_default() += 1;
            }
        }
        // stacked cards behind their stack's first card: draw deeper cards first
        let mut order: Vec<usize> = (0..cards.len()).collect();
        order.sort_by_key(|i| cards[*i].stack == Some(cards[*i].item));
        let mut released: Option<(u64, egui::Vec2)> = None;
        for i in order {
            let c = cards[i].clone();
            let off = if moving(&c) { dragging.map(|d| d.1).unwrap_or_default() } else { egui::Vec2::ZERO };
            let r = Rect::from_min_size(canvas.min + vec2(c.x, c.y) + off, vec2(c.size, c.size * 9.0 / 16.0 + 20.0));
            let Some(kind) = app.session.project.item(ItemId(c.item)).map(|x| x.kind.clone()) else { continue };
            let Some(resp) = card(app, ui, r, ItemId(c.item), &v.prefix, opts.show_names, opts.show_durations, Sense::click_and_drag()) else { continue };
            if let Some(s) = c.stack
                && s == c.item
                && let Some(n) = stack_n.get(&s)
            {
                let br = Rect::from_min_size(r.min + vec2(4.0, 4.0), vec2(26.0, 16.0));
                ui.painter().rect_filled(br, 3.0, t.accent);
                ui.painter().text(br.center(), Align2::CENTER_CENTER, format!("×{n}"), Tokens::ui(10.0), Color32::WHITE);
                app.auto.add(&format!("{}.stack.{s}", v.prefix), br, &format!("Stack of {n}"));
            }
            item_interactions(app, ui, &resp, ItemId(c.item), &kind, actions, false, false);
            if resp.dragged() {
                let total = dragging.filter(|d| d.0 == c.item).map(|d| d.1).unwrap_or_default() + resp.drag_delta();
                ui.ctx().data_mut(|d| d.insert_temp(drag_key, (c.item, total)));
                // leaving the panel turns the move into an item drag (to the Timeline, a monitor)
                if let Some(p) = ui.ctx().pointer_latest_pos()
                    && !rect.contains(p)
                    && crate::panels::dragged_project_item(ui).is_none()
                {
                    crate::panels::start_drag_item(ui, ItemId(c.item));
                }
            }
            if resp.drag_stopped() {
                released = dragging.filter(|d| d.0 == c.item).map(|d| (d.0, d.1 + resp.drag_delta()));
            }
            resp.context_menu(|ui| card_menu(app, ui, v, c.item, actions));
        }
        if let Some((item, off)) = released {
            ui.ctx().data_mut(|d| d.remove::<(u64, egui::Vec2)>(drag_key));
            let inside = ui.ctx().pointer_latest_pos().is_some_and(|p| rect.contains(p));
            if inside && off.length() > 1.0 {
                let c = cards.iter().find(|c| c.item == item).cloned();
                if let Some(c) = c {
                    // the dragged card and the selection (or its stack) move together
                    let mut items = vec![item];
                    if sel.contains(&item) {
                        items.extend(sel.iter().filter(|i| **i != item));
                    }
                    if let Some(s) = cards_stack(&cards, item) {
                        let more: Vec<u64> = cards.iter().filter(|x| x.stack == Some(s) && !items.contains(&x.item)).map(|x| x.item).collect();
                        items.extend(more);
                        // the stack's first card leads so the others keep their offsets
                        items.retain(|i| *i != s);
                        items.insert(0, s);
                    }
                    let lead = cards.iter().find(|x| x.item == items[0]).cloned().unwrap_or(c);
                    actions.push((
                        "project.freeform.move".into(),
                        json!({"items": items, "x": (lead.x + off.x).max(0.0), "y": (lead.y + off.y).max(0.0), "width": rect.width()}),
                    ));
                }
            }
        }
        if bg.clicked() {
            app.ui.project_panel.selected_bin = None;
            actions.push(("project.select".into(), json!({"items": []})));
        }
        if bg.double_clicked() {
            actions.push(("file.import".into(), json!({})));
        }
        let bin = Some(v.bin.0);
        bg.context_menu(|ui| canvas_menu(app, ui, v, bin, actions));
    });
}

fn cards_stack(cards: &[Card], item: u64) -> Option<u64> {
    cards.iter().find(|c| c.item == item).and_then(|c| c.stack)
}

fn card_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &View, item: u64, actions: &mut Actions) {
    let sel: Vec<u64> = app.session.state.project_selection.iter().map(|i| i.0).collect();
    let items: Vec<u64> = if sel.contains(&item) { sel.clone() } else { vec![item] };
    let b = ui.add_enabled(items.len() > 1, egui::Button::new(tl!("Stack")));
    app.auto.add(&format!("{}.cardMenu.stack", v.prefix), b.rect, "Stack");
    if b.clicked() {
        actions.push(("project.freeform.stack".into(), json!({"items": items})));
        ui.close();
    }
    let stacked = app.session.project.item(ItemId(item)).is_some_and(|i| i.metadata.contains_key(pp::FREEFORM_STACK));
    let b = ui.add_enabled(stacked, egui::Button::new(tl!("Unstack")));
    app.auto.add(&format!("{}.cardMenu.unstack", v.prefix), b.rect, "Unstack");
    if b.clicked() {
        actions.push(("project.freeform.unstack".into(), json!({"items": items})));
        ui.close();
    }
    ui.menu_button(tl!("Clip Size"), |ui| {
        for (label, step) in [(tl!("Larger"), 1), (tl!("Smaller"), -1)] {
            if ui.button(label).clicked() {
                actions.push(("project.freeform.resize".into(), json!({"items": items, "step": step})));
                ui.close();
            }
        }
        if ui.button(tl!("Default")).clicked() {
            actions.push(("project.freeform.resize".into(), json!({"items": items, "size": app.session.prefs.project_panel.freeform.card_size})));
            ui.close();
        }
    });
    ui.separator();
    let kind = app.session.project.item(ItemId(item)).map(|x| x.kind.clone());
    if let Some(kind) = kind {
        item_menu(app, ui, ItemId(item), &kind, actions);
    }
}

fn canvas_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &View, bin: Option<u64>, actions: &mut Actions) {
    fn entry(app: &mut FilmcraftApp, ui: &mut egui::Ui, v: &View, actions: &mut Actions, id: &str, label: &str, cmd: &str, p: serde_json::Value) {
        let b = ui.button(label);
        app.auto.add(&format!("{}.canvasMenu.{id}", v.prefix), b.rect, label);
        if b.clicked() {
            actions.push((cmd.into(), p));
            ui.close();
        }
    }
    entry(app, ui, v, actions, "alignToGrid", tl!("Align to Grid"), "project.freeform.alignToGrid", json!({"bin": bin}));
    entry(app, ui, v, actions, "resetToGrid", tl!("Reset to Grid"), "project.freeform.reset", json!({"bin": bin}));
    ui.separator();
    entry(app, ui, v, actions, "saveArrangement", tl!("Save Arrangement…"), "projectPanel.saveArrangement", json!({}));
    let names: Vec<String> = app
        .session
        .execute("project.freeform.arrangements", json!({"bin": bin}))
        .ok()
        .and_then(|v| serde_json::from_value(v["arrangements"].clone()).ok())
        .unwrap_or_default();
    ui.add_enabled_ui(!names.is_empty(), |ui| {
        ui.menu_button(tl!("Restore Arrangement"), |ui| {
            for n in &names {
                if ui.button(n).clicked() {
                    actions.push(("project.freeform.restoreArrangement".into(), json!({"name": n, "bin": bin})));
                    ui.close();
                }
            }
        });
        ui.menu_button(tl!("Delete Arrangement"), |ui| {
            for n in &names {
                if ui.button(n).clicked() {
                    actions.push(("project.freeform.deleteArrangement".into(), json!({"name": n, "bin": bin})));
                    ui.close();
                }
            }
        });
    });
    ui.separator();
    entry(app, ui, v, actions, "options", "Freeform View Options…", "projectPanel.freeformOptions", json!({}));
    ui.separator();
    background_menu(app, ui, v, bin.filter(|b| *b != app.session.project.root.id.0), actions);
}

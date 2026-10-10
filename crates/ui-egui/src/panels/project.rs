//! Project panel (M12.7): bins and items in List, Icon or Freeform view, with the breadcrumb, search
//! and selection count, the Preview Area, the footer (views, thumbnail size, Sort Icons, Automate to
//! Sequence, Find, New Bin, New Item, Clear) and the panel menu.
//!
//! View settings, columns, sorting and view presets are engine preferences
//! (`filmcraft_engine::project_panel`, `project.view.*`, `project.columns.*`, `project.viewPreset.*`);
//! Freeform positions are item metadata (`project.freeform.*`). This module keeps only what is on
//! screen: the bin shown in place, bins opened as tabs or floating panels, the inline rename, the
//! open dialog and the hover-scrubbed card ([`ProjectPanelUi`], `ui.set {"projectPanel": …}`).
//!
//! The views are drawn by `project_views`, the dialogs (Metadata Display…, view presets, Freeform
//! View Options…, Save Arrangement…) by `project_dialogs`.

use egui::{Align2, Color32, Rect, Sense, Stroke, pos2, vec2};
use filmcraft_engine::project_panel::{FontSize, ViewMode};
use filmcraft_project::{BinId, ItemId, ItemKind};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::theme::Tokens;

pub use crate::panels::project_views::{item_icon, media_badge};

/// A bin opened in its own tab of the Project panel, or in a floating panel.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct BinTab {
    pub bin: u64,
    /// A floating panel (Open in New Window) rather than a tab.
    pub floating: bool,
    pub view: ViewMode,
    pub icon_size: f32,
    /// A sub-bin opened in place inside this tab.
    pub nav: Option<u64>,
}

impl Default for BinTab {
    fn default() -> Self {
        BinTab { bin: 0, floating: false, view: ViewMode::List, icon_size: 110.0, nav: None }
    }
}

/// Inline rename in progress: an item or a bin and the text typed so far.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Rename {
    pub item: Option<u64>,
    pub bin: Option<u64>,
    pub text: String,
    /// The panel instance editing (automation prefix: `project`, `projectBin.<k>`).
    pub panel: String,
}

/// The card under the pointer while Hover Scrub is on (I / O set its In / Out).
#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Hover {
    pub item: u64,
    /// Media time under the pointer (ticks).
    pub time: i64,
    pub frame: u64,
}

/// Open Project panel dialog.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ProjectDialog {
    /// Metadata Display…: the columns shown, in order, and the field filter.
    MetadataDisplay { columns: Vec<String>, filter: String },
    /// Save As New View Preset.
    SavePresetAs { name: String },
    /// Manage Saved View Presets: the selected slot (0-based) and its name being edited.
    ManagePresets { selected: usize, name: String },
    /// Freeform View Options….
    FreeformOptions { options: filmcraft_engine::project_panel::FreeformOptions },
    /// Freeform ▸ Save Arrangement….
    SaveArrangement { name: String, bin: Option<u64> },
}

/// Frontend state of the Project panel (`UiState::project_panel`).
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct ProjectPanelUi {
    /// The bin the Project panel shows in place (None = the project's root).
    pub bin: Option<u64>,
    /// Bins opened in new tabs (inside the Project panel) or new windows (floating).
    pub tabs: Vec<BinTab>,
    /// The tab shown in the Project panel: None = the project, else an index into `tabs`.
    pub active_tab: Option<usize>,
    /// The bin selected in the panel (rename, open).
    pub selected_bin: Option<u64>,
    pub rename: Option<Rename>,
    pub dialog: Option<ProjectDialog>,
    pub hover: Option<Hover>,
}

/// Which panel instance a view belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Inst {
    /// The Project panel itself.
    Main,
    /// A bin tab or floating bin panel (index into `ProjectPanelUi::tabs`).
    Tab(usize),
}

/// What one panel instance shows.
#[derive(Clone, Debug)]
pub struct View {
    pub inst: Inst,
    /// Automation id prefix (`project`, `projectBin.<k>`).
    pub prefix: String,
    pub bin: BinId,
    pub mode: ViewMode,
    pub icon_size: f32,
}

pub type Actions = Vec<(String, Value)>;

const HEADER_H: f32 = 26.0;
const FOOTER_H: f32 = 32.0;
const PREVIEW_H: f32 = 92.0;

fn valid_bin(app: &FilmcraftApp, b: Option<u64>) -> Option<BinId> {
    b.map(BinId).filter(|b| app.session.project.root.find_bin(*b).is_some())
}

/// The view of a panel instance.
pub fn view_of(app: &FilmcraftApp, inst: Inst) -> View {
    let root = app.session.project.root.id;
    match inst {
        Inst::Tab(k) if k < app.ui.project_panel.tabs.len() => {
            let tab = &app.ui.project_panel.tabs[k];
            View {
                inst,
                prefix: format!("projectBin.{k}"),
                bin: valid_bin(app, tab.nav).or(valid_bin(app, Some(tab.bin))).unwrap_or(root),
                mode: tab.view,
                icon_size: tab.icon_size,
            }
        }
        _ => {
            let v = &app.session.prefs.project_panel.view;
            View {
                inst: Inst::Main,
                prefix: "project".into(),
                bin: valid_bin(app, app.ui.project_panel.bin).unwrap_or(root),
                mode: v.mode,
                icon_size: v.icon_size,
            }
        }
    }
}

/// The instance the Project panel shows now (its active tab).
pub fn shown_inst(app: &FilmcraftApp) -> Inst {
    match app.ui.project_panel.active_tab {
        Some(k) if app.ui.project_panel.tabs.get(k).is_some_and(|t| !t.floating) => Inst::Tab(k),
        _ => Inst::Main,
    }
}

/// Text size of the panel (Font Size).
pub fn font(app: &FilmcraftApp) -> f32 {
    app.session.prefs.project_panel.view.font_size.points()
}

pub fn set_mode(app: &mut FilmcraftApp, inst: Inst, mode: ViewMode, actions: &mut Actions) {
    match inst {
        Inst::Main => actions.push(("project.view.set".into(), json!({"view": mode}))),
        Inst::Tab(k) => {
            if let Some(t) = app.ui.project_panel.tabs.get_mut(k) {
                t.view = mode;
            }
        }
    }
}

/// Show a bin in place in a panel instance (None = its top: the root or the tab's bin).
pub fn navigate(app: &mut FilmcraftApp, inst: Inst, bin: Option<u64>) {
    let root = app.session.project.root.id.0;
    match inst {
        Inst::Main => app.ui.project_panel.bin = bin.filter(|b| *b != root),
        Inst::Tab(k) => {
            if let Some(t) = app.ui.project_panel.tabs.get_mut(k) {
                t.nav = bin.filter(|b| *b != t.bin);
            }
        }
    }
    app.ui.project_panel.selected_bin = None;
}

/// Open a bin as Settings ▸ General ▸ Bins says (`how`: `openInPlace` | `openNewTab` |
/// `openNewWindow`, or None for the double-click setting with these modifiers).
pub fn open_bin(app: &mut FilmcraftApp, inst: Inst, bin: u64, how: Option<&str>, mods: egui::Modifiers) -> Result<Value, String> {
    if app.session.project.root.find_bin(BinId(bin)).is_none() {
        return Err(format!("no bin {bin}"));
    }
    let g = &app.session.prefs.general;
    let how = how.map(str::to_string).unwrap_or_else(|| {
        if mods.command {
            g.bins_cmd_double_click.clone()
        } else if mods.alt {
            g.bins_opt_double_click.clone()
        } else {
            g.bins_double_click.clone()
        }
    });
    let pp = &mut app.ui.project_panel;
    let (view, icon_size) = (app.session.prefs.project_panel.view.mode, app.session.prefs.project_panel.view.icon_size);
    match how.as_str() {
        "openInPlace" => {
            navigate(app, inst, Some(bin));
            Ok(json!({"opened": "inPlace", "bin": bin}))
        }
        "openNewTab" | "openNewWindow" => {
            let floating = how == "openNewWindow";
            // an already open tab is brought forward
            if let Some(k) = pp.tabs.iter().position(|t| t.bin == bin && t.floating == floating) {
                if !floating {
                    pp.active_tab = Some(k);
                }
                return Ok(json!({"opened": if floating { "window" } else { "tab" }, "bin": bin, "tab": k}));
            }
            pp.tabs.push(BinTab { bin, floating, view, icon_size, nav: None });
            let k = pp.tabs.len() - 1;
            if !floating {
                pp.active_tab = Some(k);
            }
            Ok(json!({"opened": if floating { "window" } else { "tab" }, "bin": bin, "tab": k}))
        }
        other => Err(format!("unknown bin open mode `{other}`")),
    }
}

/// Close a bin tab or floating bin panel.
pub fn close_tab(app: &mut FilmcraftApp, k: usize) {
    let pp = &mut app.ui.project_panel;
    if k >= pp.tabs.len() {
        return;
    }
    pp.tabs.remove(k);
    pp.active_tab = match pp.active_tab {
        Some(a) if a == k => None,
        Some(a) if a > k => Some(a - 1),
        a => a,
    };
}

/// Items of a panel instance in display order (keyboard navigation, Select All).
pub fn visible_items(app: &FilmcraftApp, inst: Inst) -> Vec<ItemId> {
    let v = view_of(app, inst);
    crate::panels::project_views::items_in_view(app, &v, &app.ui.project_search.to_ascii_lowercase())
}

// ------------------------------------------------------------------------------------ panel

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let mut actions: Actions = Vec::new();
    let mut rect = rect;
    // bin tabs docked in the Project panel
    let docked: Vec<usize> = app.ui.project_panel.tabs.iter().enumerate().filter(|(_, t)| !t.floating).map(|(k, _)| k).collect();
    if !docked.is_empty() {
        let strip = Rect::from_min_size(rect.min, vec2(rect.width(), 24.0));
        tab_strip(app, ui, strip, &docked);
        rect.min.y = strip.max.y;
    }
    let inst = shown_inst(app);
    let v = view_of(app, inst);
    body(app, ui, rect, &v, &mut actions);
    run(app, ui.ctx(), actions);
}

fn tab_strip(app: &mut FilmcraftApp, ui: &mut egui::Ui, strip: Rect, docked: &[usize]) {
    let t = app.tokens;
    ui.painter().line_segment([strip.left_bottom(), strip.right_bottom()], Stroke::new(1.0, t.separator));
    let mut x = strip.min.x + 6.0;
    let active = shown_inst(app);
    let mut entries: Vec<(Option<usize>, String)> = vec![(None, tlf!("Project: {name}", name = app.session.project.name))];
    for k in docked {
        let name = app.session.project.root.find_bin(BinId(app.ui.project_panel.tabs[*k].bin)).map(|b| b.name.clone()).unwrap_or_default();
        entries.push((Some(*k), tlf!("Bin: {name}", name)));
    }
    let mut close = None;
    for (k, label) in entries {
        let g = ui.painter().layout_no_wrap(label.clone(), Tokens::ui(11.5), t.text);
        let w = g.size().x + if k.is_some() { 30.0 } else { 16.0 };
        let r = Rect::from_min_size(pos2(x, strip.min.y + 2.0), vec2(w, strip.height() - 4.0));
        let is_active = match (k, active) {
            (None, Inst::Main) => true,
            (Some(a), Inst::Tab(b)) => a == b,
            _ => false,
        };
        let id = k.map_or("project.tab.project".to_string(), |k| format!("project.tab.{k}"));
        let resp = ui.interact(r, egui::Id::new(&id), Sense::click());
        app.auto.add(&id, r, &label);
        if is_active || resp.hovered() {
            ui.painter().rect_filled(r, 3.0, if is_active { t.pressed } else { t.hover });
        }
        ui.painter().galley(pos2(r.min.x + 8.0, r.center().y - g.size().y / 2.0), g, if is_active { t.tab_text_active } else { t.tab_text });
        if resp.clicked() {
            app.ui.project_panel.active_tab = k;
        }
        if let Some(k) = k {
            let cr = Rect::from_center_size(pos2(r.max.x - 11.0, r.center().y), vec2(12.0, 12.0));
            let cresp = ui.interact(cr, egui::Id::new(("ptab-close", k)), Sense::click()).on_hover_text(tl!("Close"));
            app.auto.add(&format!("project.tab.{k}.close"), cr, "Close");
            icons::paint(ui.painter(), cr.shrink(2.0), Icon::Close, if cresp.hovered() { t.text } else { t.text_dim });
            if cresp.clicked() || resp.middle_clicked() {
                close = Some(k);
            }
        }
        x = r.max.x + 4.0;
    }
    if let Some(k) = close {
        close_tab(app, k);
    }
}

/// Floating bin panels (Open in New Window); drawn every frame by the panel layer.
pub fn floating(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let n = app.ui.project_panel.tabs.len();
    let mut close = None;
    let mut actions: Actions = Vec::new();
    for k in 0..n {
        if !app.ui.project_panel.tabs[k].floating {
            continue;
        }
        let name = app.session.project.root.find_bin(BinId(app.ui.project_panel.tabs[k].bin)).map(|b| b.name.clone());
        let Some(name) = name else {
            close = Some(k);
            continue;
        };
        let mut open = true;
        let root = app.ui.project_panel.tabs[k].bin == app.session.project.root.id.0;
        egui::Window::new(if root { tlf!("Project: {name}", name = app.session.project.name) } else { tlf!("Bin: {name}", name) })
            .id(egui::Id::new(("bin-window", k)))
            .open(&mut open)
            .default_size(vec2(520.0, 360.0))
            .min_size(vec2(320.0, 220.0))
            .collapsible(false)
            .show(ctx, |ui| {
                let r = ui.available_rect_before_wrap();
                let r = Rect::from_min_size(r.min, vec2(r.width().max(320.0), r.height().max(220.0)));
                ui.allocate_rect(r, Sense::hover());
                let v = view_of(app, Inst::Tab(k));
                app.auto.add(&format!("projectBin.{k}.window"), r, &name);
                let mut child = ui.new_child(egui::UiBuilder::new().max_rect(r).id_salt(("bin-window-body", k)));
                child.set_clip_rect(r);
                body(app, &mut child, r, &v, &mut actions);
            });
        if !open {
            close = Some(k);
        }
    }
    if let Some(k) = close {
        close_tab(app, k);
    }
    run(app, ctx, actions);
}

fn run(app: &mut FilmcraftApp, ctx: &egui::Context, actions: Actions) {
    for (cmd, p) in actions {
        if let Err(e) = crate::menus::invoke(app, ctx, &cmd, p) {
            app.ui.status = e;
        }
    }
}

/// One panel instance: breadcrumb, search, Preview Area, the view, footer.
fn body(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect, v: &View, actions: &mut Actions) {
    let t = app.tokens;
    let pre = v.prefix.clone();
    // breadcrumb: up one level, project file and bin path
    let head = Rect::from_min_size(rect.min + vec2(8.0, 2.0), vec2(rect.width() - 16.0, HEADER_H - 2.0));
    let root_id = app.session.project.root.id;
    let top = match v.inst {
        Inst::Main => root_id,
        Inst::Tab(k) => BinId(app.ui.project_panel.tabs[k].bin),
    };
    let up = Rect::from_min_size(pos2(head.min.x, head.center().y - 9.0), vec2(22.0, 18.0));
    let up_resp = ui.interact(up, egui::Id::new((&pre, "up")), Sense::click()).on_hover_text(tl!("Up one level"));
    app.auto.add(&format!("{pre}.up"), up, "Up one level");
    let can_up = v.bin != top;
    if up_resp.hovered() && can_up {
        ui.painter().rect_filled(up, 3.0, t.hover);
    }
    icons::paint(ui.painter(), up.shrink(3.0), Icon::Folder, if can_up { t.icon } else { t.text_faint });
    icons::paint(ui.painter(), Rect::from_center_size(up.center() + vec2(0.0, 1.0), vec2(8.0, 8.0)), Icon::ArrowUp, if can_up { t.icon } else { t.text_faint });
    if up_resp.clicked() && can_up {
        let parent = parent_bin(&app.session.project.root, v.bin);
        navigate(app, v.inst, parent.map(|b| b.0));
    }
    let mut crumb = format!("{}.fcproj", app.session.project.name);
    for b in bin_path(&app.session.project.root, v.bin) {
        crumb.push_str(" / ");
        crumb.push_str(&b);
    }
    let fs = font(app);
    let cr = ui.painter().with_clip_rect(head);
    cr.text(pos2(up.max.x + 8.0, head.center().y), Align2::LEFT_CENTER, &crumb, Tokens::ui(fs - 0.5), t.text_dim);
    app.auto.add(&format!("{pre}.breadcrumb"), head, &crumb);

    // search row: field, Find, item count
    let row = Rect::from_min_size(pos2(rect.min.x + 8.0, head.max.y + 2.0), vec2(rect.width() - 16.0, 24.0));
    let sw = (row.width() * 0.45).clamp(80.0, 240.0);
    let search_rect = Rect::from_min_size(row.min, vec2(sw, 22.0));
    let mut child = ui.new_child(egui::UiBuilder::new().max_rect(search_rect).id_salt((&pre, "search")));
    let mut q = app.ui.project_search.clone();
    crate::widgets::search_field(&mut child, &mut q, tl!("Search"), search_rect.width(), &t);
    app.ui.project_search = q;
    app.auto.add(&format!("{pre}.search"), search_rect, "Search");
    let find = Rect::from_min_size(pos2(search_rect.max.x + 8.0, row.min.y), vec2(24.0, 22.0));
    let fr = ui.interact(find, egui::Id::new((&pre, "find")), Sense::click()).on_hover_text(tl!("Find…"));
    app.auto.add(&format!("{pre}.find"), find, "Find…");
    if fr.hovered() {
        ui.painter().rect_filled(find, 3.0, t.hover);
    }
    icons::paint(ui.painter(), find.shrink(4.0), Icon::Search, t.icon);
    if fr.clicked() {
        actions.push(("edit.find".into(), json!({})));
    }
    let filter = app.ui.project_search.to_ascii_lowercase();
    let total = count_items(app, v.bin);
    let sel = app.session.state.project_selection.len();
    let count = if sel > 0 { tlf!("{sel} of {total} items selected", sel, total) } else { tlf!("{total} items", total) };
    ui.painter().text(pos2(row.max.x, row.center().y - 1.0), Align2::RIGHT_CENTER, &count, Tokens::ui(fs - 1.0), t.text_dim);
    app.auto.add(&format!("{pre}.count"), Rect::from_min_max(pos2(row.max.x - 160.0, row.min.y), row.max), &count);

    let mut y = row.max.y + 6.0;
    if app.session.prefs.project_panel.preview_area {
        let pr = Rect::from_min_size(pos2(rect.min.x + 8.0, y), vec2(rect.width() - 16.0, PREVIEW_H));
        preview_area(app, ui, pr, &pre);
        y = pr.max.y + 6.0;
    }
    let content = Rect::from_min_max(pos2(rect.min.x + 4.0, y), pos2(rect.max.x - 4.0, rect.max.y - FOOTER_H));
    ui.painter().rect_filled(content, 4.0, t.panel_bg.gamma_multiply(0.92));
    let mut cui = ui.new_child(egui::UiBuilder::new().max_rect(content).id_salt((&pre, "content")));
    cui.set_clip_rect(content.intersect(ui.clip_rect()));
    match v.mode {
        ViewMode::List => crate::panels::project_views::list_view(app, &mut cui, content, v, &filter, actions),
        ViewMode::Icon => crate::panels::project_views::icon_view(app, &mut cui, content, v, &filter, actions),
        ViewMode::Freeform => crate::panels::project_views::freeform_view(app, &mut cui, content, v, &filter, actions),
    }
    footer(app, ui, Rect::from_min_max(pos2(rect.min.x, rect.max.y - FOOTER_H), rect.max), v, actions);
}

fn bin_path(root: &filmcraft_project::Bin, target: BinId) -> Vec<String> {
    fn walk(b: &filmcraft_project::Bin, target: BinId, path: &mut Vec<String>) -> bool {
        for c in &b.children {
            if let filmcraft_project::BinEntry::Bin(sub) = c {
                path.push(sub.name.clone());
                if sub.id == target || walk(sub, target, path) {
                    return true;
                }
                path.pop();
            }
        }
        false
    }
    let mut p = Vec::new();
    if root.id != target {
        walk(root, target, &mut p);
    }
    p
}

/// The bin containing bin `id` (None at the root).
pub fn parent_bin(root: &filmcraft_project::Bin, id: BinId) -> Option<BinId> {
    for c in &root.children {
        if let filmcraft_project::BinEntry::Bin(b) = c {
            if b.id == id {
                return Some(root.id);
            }
            if let Some(p) = parent_bin(b, id) {
                return Some(p);
            }
        }
    }
    None
}

fn count_items(app: &FilmcraftApp, bin: BinId) -> usize {
    let mut ids = Vec::new();
    if let Some(b) = app.session.project.root.find_bin(bin) {
        b.all_items(&mut ids);
    }
    ids.iter().filter(|i| app.session.project.item(**i).is_some_and(|it| filmcraft_engine::project_panel::listed(&app.session.project, it))).count()
}

/// Preview Area: the selected item's poster frame and its properties.
fn preview_area(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, pre: &str) {
    let t = app.tokens;
    ui.painter().rect_filled(r, 4.0, t.tl_header_bg);
    app.auto.add(&format!("{pre}.previewArea"), r, "Preview Area");
    let Some(id) = app.session.state.project_selection.first().copied() else {
        ui.painter().text(r.center(), Align2::CENTER_CENTER, tl!("Select an item to preview it"), Tokens::ui(11.0), t.text_faint);
        return;
    };
    let Some(it) = app.session.project.item(id).cloned() else { return };
    let th = Rect::from_min_size(r.min + vec2(6.0, 6.0), vec2((r.height() - 12.0) * 16.0 / 9.0, r.height() - 12.0));
    ui.painter().rect_filled(th, 2.0, Color32::from_gray(10));
    let ctx = ui.ctx().clone();
    let poster = filmcraft_engine::keyboard::poster_frame(&it).unwrap_or(filmcraft_time::Tick(it.duration().0 / 3));
    if let Some((tex, sz)) = app.thumbnail(&ctx, id, quantize(poster), 160) {
        ui.painter().image(tex, crate::panels::monitor::fit(th, sz.x, sz.y), Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)), Color32::WHITE);
    } else {
        icons::paint(ui.painter(), Rect::from_center_size(th.center(), vec2(24.0, 24.0)), item_icon(&it.kind), t.text_faint);
    }
    let p = &app.session.project;
    let col = |c: &str| filmcraft_engine::project_panel::cell(p, &it, c).0;
    let (vu, au) = filmcraft_engine::project_panel::usage(p, id);
    let lines = [
        it.name.clone(),
        format!("{}{}", crate::i18n::t(it.type_label()), if it.has_video() { format!(", {}", col("Video Info")) } else { String::new() }),
        [col("Media Duration"), col("Frame Rate")].into_iter().filter(|s| !s.is_empty()).collect::<Vec<_>>().join(", "),
        col("Audio Info"),
        tlf!("Video Usage: {vu}  Audio Usage: {au}", vu, au),
    ];
    let mut y = r.min.y + 12.0;
    for (i, l) in lines.iter().enumerate() {
        if l.is_empty() {
            continue;
        }
        let f = if i == 0 { Tokens::semibold(12.0) } else { Tokens::ui(11.0) };
        ui.painter().with_clip_rect(r).text(pos2(th.max.x + 10.0, y), Align2::LEFT_CENTER, l, f, if i == 0 { t.text } else { t.text_dim });
        y += 15.0;
    }
}

/// Thumbnail times are cached on a quarter-second grid.
pub fn quantize(t: filmcraft_time::Tick) -> filmcraft_time::Tick {
    let q = filmcraft_time::TICKS_PER_SECOND / 4;
    filmcraft_time::Tick((t.0 / q) * q)
}

fn footer(app: &mut FilmcraftApp, ui: &mut egui::Ui, bar: Rect, v: &View, actions: &mut Actions) {
    let t = app.tokens;
    let pre = &v.prefix;
    let mut x = bar.min.x + 6.0;
    for (icon, mode, tip) in [
        (Icon::ListView, ViewMode::List, tl!("List View")),
        (Icon::IconView, ViewMode::Icon, tl!("Icon View")),
        (Icon::Freeform, ViewMode::Freeform, tl!("Freeform View")),
    ] {
        let r = Rect::from_min_size(pos2(x, bar.min.y + 5.0), vec2(26.0, 22.0));
        let resp = ui.interact(r, egui::Id::new((pre, "pv", tip)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("{pre}.view.{mode:?}"), r, tip);
        if v.mode == mode {
            ui.painter().rect_filled(r, 4.0, t.pressed);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(5.0), icon, if v.mode == mode { t.icon_active } else { t.icon });
        if resp.clicked() {
            set_mode(app, v.inst, mode, actions);
        }
        x += 28.0;
    }
    // thumbnail size: in-memory while dragging, saved on release
    let sw = (bar.width() - 300.0).clamp(40.0, 120.0);
    let sr = Rect::from_min_size(pos2(x + 8.0, bar.min.y + 8.0), vec2(sw, 16.0));
    let mut sz = v.icon_size;
    let resp = ui.put(sr, egui::Slider::new(&mut sz, 48.0..=400.0).show_value(false));
    app.auto.add(&format!("{pre}.iconSize"), sr, "Zoom");
    if sz != v.icon_size {
        match v.inst {
            Inst::Main => app.session.prefs.project_panel.view.icon_size = sz,
            Inst::Tab(k) => app.ui.project_panel.tabs[k].icon_size = sz,
        }
    }
    if v.inst == Inst::Main && (resp.drag_stopped() || (resp.changed() && !resp.dragged())) {
        actions.push(("project.view.set".into(), json!({"iconSize": sz})));
    }
    x = sr.max.x + 6.0;
    // Sort Icons (Icon view)
    if v.mode == ViewMode::Icon {
        let r = Rect::from_min_size(pos2(x, bar.min.y + 5.0), vec2(24.0, 22.0));
        let resp = ui.interact(r, egui::Id::new((pre, "sortIcons")), Sense::click()).on_hover_text(tl!("Sort Icons"));
        app.auto.add(&format!("{pre}.sortIcons"), r, "Sort Icons");
        if resp.hovered() {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(5.0), Icon::SortIcons, t.icon);
        egui::Popup::menu(&resp).show(|ui| sort_icons_menu(app, ui, pre, actions));
    }
    let mut rx = bar.max.x - 6.0;
    for (icon, id, tip) in [
        (Icon::Trash, "project.delete", tl!("Clear (Delete)")),
        (Icon::NewItem, "new-item", tl!("New Item")),
        (Icon::Folder, "file.newBin", tl!("New Bin")),
        (Icon::Search, "find", tl!("Find")),
        (Icon::Automate, "clip.automateToSequence", tl!("Automate to Sequence")),
    ] {
        let r = Rect::from_min_size(pos2(rx - 24.0, bar.min.y + 5.0), vec2(24.0, 22.0));
        let resp = ui.interact(r, egui::Id::new((pre, "pb", id)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("{pre}.button.{id}"), r, tip);
        if resp.hovered() {
            ui.painter().rect_filled(r, 3.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(5.0), icon, t.icon);
        if id == "new-item" {
            egui::Popup::menu(&resp).show(|ui| new_item_menu(app, ui, pre, actions));
        } else if resp.clicked() {
            match id {
                "find" => actions.push(("edit.find".into(), json!({}))),
                "file.newBin" => {
                    let parent = (v.bin != app.session.project.root.id).then_some(v.bin.0);
                    actions.push(("file.newBin".into(), json!({"name": "New Bin", "parent": parent})));
                }
                "project.delete" if app.session.state.project_selection.is_empty() => app.ui.status = tl!("Select items to clear").into(),
                _ => actions.push((id.into(), json!({}))),
            }
        }
        rx -= 28.0;
    }
}

pub const NEW_ITEMS: [(&str, &str); 8] = [
    ("Sequence…", "file.newSequence"),
    ("Adjustment Layer…", "file.newAdjustmentLayer"),
    ("Bars and Tone…", "file.newBarsAndTone"),
    ("Black Video…", "file.newBlackVideo"),
    ("Color Matte…", "file.newColorMatte"),
    ("Universal Counting Leader…", "file.newCountingLeader"),
    ("Transparent Video…", "file.newTransparentVideo"),
    ("Demo Footage", "file.importDemoFootage"),
];

pub fn new_item_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, pre: &str, actions: &mut Actions) {
    for (label, c) in NEW_ITEMS {
        let b = ui.button(crate::i18n::t(label));
        app.auto.add(&format!("{pre}.newItem.{c}"), b.rect, label);
        if b.clicked() {
            actions.push((c.into(), json!({})));
            ui.close();
        }
    }
}

fn sort_icons_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui, pre: &str, actions: &mut Actions) {
    let cur = app.session.prefs.project_panel.view.icon_sort.clone();
    let mut entries: Vec<(String, String)> = vec![("".into(), tl!("User Order").into())];
    for c in ["Name", "Label", "Media Type", "Frame Rate", "Media Start", "Media End", "Media Duration", "Video Info", "Audio Info", "Date Created"] {
        entries.push((c.into(), crate::i18n::t(c).into()));
    }
    for (col, label) in entries {
        let on = cur.column == col;
        let r = ui.selectable_label(on, &label);
        app.auto.add(&format!("{pre}.sortIcons.{}", if col.is_empty() { "userOrder" } else { col.as_str() }), r.rect, &label);
        if r.clicked() {
            actions.push(("project.view.set".into(), json!({"iconSort": {"column": col, "descending": false}})));
            ui.close();
        }
    }
    ui.separator();
    let r = ui.selectable_label(cur.descending, tl!("Descending"));
    app.auto.add(&format!("{pre}.sortIcons.descending"), r.rect, "Descending");
    if r.clicked() {
        actions.push(("project.view.set".into(), json!({"iconSort": {"descending": !cur.descending}})));
        ui.close();
    }
}

// ------------------------------------------------------------------------------------ panel menu

/// The Project panel's ≡ menu (after the generic panel items). Returns true to close it.
pub fn panel_menu(app: &mut FilmcraftApp, ui: &mut egui::Ui) -> bool {
    let mut actions: Actions = Vec::new();
    let mut close = false;
    let pp = app.session.prefs.project_panel.clone();
    let inst = shown_inst(app);
    let v = view_of(app, inst);
    let has_sel = !app.session.state.project_selection.is_empty() || app.ui.project_panel.selected_bin.is_some();
    let has_project_path = app.session.path.is_some();
    let sc = |id: &str| app.session.shortcuts.primary(id);
    let (s_close, s_save, s_bin, s_find) = (sc("file.closeProject"), sc("file.save"), sc("file.newBin"), sc("edit.find"));
    {
        let mut item = |ui: &mut egui::Ui, id: &str, label: &str, enabled: bool, shortcut: Option<&str>| -> bool {
            let mut b = egui::Button::new(label);
            if let Some(s) = shortcut {
                b = b.shortcut_text(crate::menus::shortcut_text(s));
            }
            let r = ui.add_enabled(enabled, b);
            app.auto.add(&format!("project.menu.{id}"), r.rect, label);
            r.clicked()
        };
        if item(ui, "closeProject", tl!("Close Project"), true, s_close.as_deref()) {
            actions.push(("file.closeProject".into(), json!({})));
        }
        item(ui, "closeAllOtherProjects", tl!("Close All Other Projects"), false, None);
        if item(ui, "saveProject", tl!("Save Project"), true, s_save.as_deref()) {
            actions.push(("file.save".into(), json!({})));
        }
        item(ui, "refreshProject", tl!("Refresh Project"), false, None);
        if item(ui, "revealProject", tl!("Reveal Project in Finder…"), has_project_path, None) {
            actions.push(("projectPanel.revealProject".into(), json!({})));
        }
        ui.separator();
        if item(ui, "newBin", tl!("New Bin"), true, s_bin.as_deref()) {
            let parent = (v.bin != app.session.project.root.id).then_some(v.bin.0);
            actions.push(("file.newBin".into(), json!({"name": "New Bin", "parent": parent})));
        }
        if item(ui, "newBinFromSelection", tl!("New Bin From Selection"), !app.session.state.project_selection.is_empty(), Some("Shift+B")) {
            actions.push(("file.newBinFromSelection".into(), json!({})));
        }
        if item(ui, "newSearchBin", tl!("New Search Bin"), true, None) {
            actions.push(("file.newSearchBin".into(), json!({})));
        }
        item(ui, "newProjectShortcut", tl!("New Project Shortcut"), false, None);
        if item(ui, "rename", tl!("Rename"), has_sel, None) {
            actions.push(("projectPanel.rename".into(), json!({})));
        }
        if item(ui, "delete", tl!("Delete"), !app.session.state.project_selection.is_empty(), Some("Backspace")) {
            actions.push(("project.delete".into(), json!({})));
        }
        ui.separator();
        if item(ui, "automateToSequence", tl!("Automate to Sequence…"), !app.session.state.project_selection.is_empty(), None) {
            actions.push(("clip.automateToSequence".into(), json!({})));
        }
        if item(ui, "find", tl!("Find…"), true, s_find.as_deref()) {
            actions.push(("edit.find".into(), json!({})));
        }
        ui.separator();
    }
    for (mode, label, key) in
        [(ViewMode::List, tl!("List"), "Cmd+PageUp"), (ViewMode::Icon, tl!("Icon"), "Cmd+PageDown"), (ViewMode::Freeform, tl!("Freeform"), "")]
    {
        let r = ui.add(egui::Button::selectable(v.mode == mode, label).shortcut_text(crate::menus::shortcut_text(key)));
        app.auto.add(&format!("project.menu.view.{mode:?}"), r.rect, label);
        if r.clicked() {
            set_mode(app, inst, mode, &mut actions);
            close = true;
        }
    }
    ui.separator();
    for (key, label, on, shortcut) in [
        ("previewArea", tl!("Preview Area"), pp.preview_area, ""),
        ("thumbnails", tl!("Thumbnails"), pp.thumbnails, ""),
        ("thumbnailsShowEffects", tl!("Thumbnails show effects applied"), pp.thumbnails_show_effects, ""),
        ("hoverScrub", tl!("Hover Scrub"), pp.hover_scrub, "Shift+H"),
        ("thumbnailControlsAllDevices", tl!("Thumbnail controls for all pointing devices"), pp.thumbnail_controls_all_devices, ""),
    ] {
        let r = ui.add(egui::Button::selectable(on, label).shortcut_text(crate::menus::shortcut_text(shortcut)));
        app.auto.add(&format!("project.menu.{key}"), r.rect, label);
        if r.clicked() {
            actions.push(("project.view.set".into(), json!({key: !on})));
            close = true;
        }
    }
    ui.separator();
    let fr = ui.menu_button(tl!("Font Size"), |ui| {
        for f in FontSize::ALL {
            let r = ui.selectable_label(pp.view.font_size == f, crate::i18n::t(f.label()));
            app.auto.add(&format!("project.menu.fontSize.{f:?}"), r.rect, f.label());
            if r.clicked() {
                actions.push(("project.view.set".into(), json!({"fontSize": f})));
                ui.close();
            }
        }
    });
    app.auto.add("project.menu.fontSize", fr.response.rect, "Font Size");
    ui.separator();
    let r = ui.button(tl!("Refresh Sort Order"));
    app.auto.add("project.menu.refreshSortOrder", r.rect, "Refresh Sort Order");
    if r.clicked() {
        actions.push(("project.sort".into(), json!({"column": pp.view.sort.column, "descending": pp.view.sort.descending})));
        close = true;
    }
    ui.separator();
    let r = ui.button(tl!("Metadata Display…"));
    app.auto.add("project.menu.metadataDisplay", r.rect, "Metadata Display…");
    if r.clicked() {
        actions.push(("projectPanel.metadataDisplay".into(), json!({})));
        close = true;
    }
    let current = pp.current_preset.filter(|i| pp.presets.get(*i).is_some_and(Option::is_some));
    let r = ui.add_enabled(current.is_some(), egui::Button::new(tl!("Save Current View Preset")));
    app.auto.add("project.menu.saveViewPreset", r.rect, "Save Current View Preset");
    if r.clicked() {
        actions.push(("project.viewPreset.save".into(), json!({})));
        close = true;
    }
    let r = ui.button(tl!("Save As New View Preset"));
    app.auto.add("project.menu.saveViewPresetAs", r.rect, "Save As New View Preset");
    if r.clicked() {
        actions.push(("projectPanel.saveViewPresetAs".into(), json!({})));
        close = true;
    }
    let any = pp.presets.iter().any(Option::is_some);
    let rr = ui.add_enabled_ui(any, |ui| {
        ui.menu_button(tl!("Restore View Preset"), |ui| {
            for (i, p) in pp.presets.iter().enumerate() {
                let label = p.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| tlf!("Project View Preset {n}", n = i + 1));
                let r = ui.add_enabled(p.is_some(), egui::Button::selectable(pp.current_preset == Some(i), label.clone()));
                app.auto.add(&format!("project.menu.restoreViewPreset.{}", i + 1), r.rect, &label);
                if r.clicked() {
                    actions.push(("project.viewPreset.restore".into(), json!({"slot": i + 1})));
                    ui.close();
                }
            }
        })
    });
    app.auto.add("project.menu.restoreViewPreset", rr.response.rect, "Restore View Preset");
    let r = ui.add_enabled(any, egui::Button::new(tl!("Manage Saved View Presets")));
    app.auto.add("project.menu.manageViewPresets", r.rect, "Manage Saved View Presets");
    if r.clicked() {
        actions.push(("projectPanel.manageViewPresets".into(), json!({})));
        close = true;
    }
    ui.separator();
    let r = ui.add_enabled(v.mode == ViewMode::Freeform, egui::Button::new(tl!("Freeform View Options…")));
    app.auto.add("project.menu.freeformOptions", r.rect, "Freeform View Options…");
    if r.clicked() {
        actions.push(("projectPanel.freeformOptions".into(), json!({})));
        close = true;
    }
    if !actions.is_empty() {
        close = true;
    }
    run(app, ui.ctx(), actions);
    close
}

// ------------------------------------------------------------------------------------ commands

/// Project panel UI commands routed from `projectPanel.*` (see `panels::keyboard`).
pub fn route(app: &mut FilmcraftApp, ctx: &egui::Context, op: &str, params: &Value) -> Option<Result<Value, String>> {
    let inst = shown_inst(app);
    Some(match op {
        "markIn" | "markOut" => hover_mark(app, ctx, op == "markIn"),
        "rename" => start_rename(app, params),
        "openBin" => {
            let Some(bin) = params.get("bin").and_then(Value::as_u64).or(app.ui.project_panel.selected_bin) else {
                return Some(Err("need `bin` (or select a bin)".into()));
            };
            let how = params.get("how").and_then(Value::as_str).map(|h| match h {
                "inPlace" => "openInPlace",
                "newTab" | "tab" => "openNewTab",
                "newWindow" | "window" => "openNewWindow",
                other => other,
            });
            // no `how`: as a double-click with these modifiers (Settings ▸ General ▸ Bins)
            let flag = |k: &str| params.get(k).and_then(Value::as_bool).unwrap_or(false);
            let mods =
                egui::Modifiers { command: flag("command"), mac_cmd: flag("command") && cfg!(target_os = "macos"), alt: flag("alt"), ..Default::default() };
            open_bin(app, inst, bin, how, mods)
        }
        "up" => {
            let v = view_of(app, inst);
            let parent = parent_bin(&app.session.project.root, v.bin);
            navigate(app, inst, parent.map(|b| b.0));
            Ok(json!({"bin": view_of(app, inst).bin.0}))
        }
        "closeBinTab" => {
            let k = params.get("tab").and_then(Value::as_u64).map(|k| k as usize).or(app.ui.project_panel.active_tab);
            match k {
                Some(k) if k < app.ui.project_panel.tabs.len() => {
                    close_tab(app, k);
                    Ok(Value::Null)
                }
                _ => Err("no such bin tab".into()),
            }
        }
        "metadataDisplay" => {
            let columns = app.session.prefs.project_panel.view.columns.iter().map(|c| c.name.clone()).collect();
            app.ui.project_panel.dialog = Some(ProjectDialog::MetadataDisplay { columns, filter: String::new() });
            Ok(Value::Null)
        }
        "saveViewPresetAs" => {
            let n = app.session.prefs.project_panel.presets.iter().position(Option::is_none).map_or(1, |i| i + 1);
            app.ui.project_panel.dialog = Some(ProjectDialog::SavePresetAs { name: tlf!("Project View Preset {n}", n) });
            Ok(Value::Null)
        }
        "manageViewPresets" => {
            let pp = &app.session.prefs.project_panel;
            let selected = pp.presets.iter().position(Option::is_some).unwrap_or(0);
            let name = pp.presets.get(selected).cloned().flatten().map(|p| p.name).unwrap_or_default();
            app.ui.project_panel.dialog = Some(ProjectDialog::ManagePresets { selected, name });
            Ok(Value::Null)
        }
        "freeformOptions" => {
            app.ui.project_panel.dialog = Some(ProjectDialog::FreeformOptions { options: app.session.prefs.project_panel.freeform.clone() });
            Ok(Value::Null)
        }
        "saveArrangement" => {
            let bin = Some(view_of(app, inst).bin.0);
            app.ui.project_panel.dialog = Some(ProjectDialog::SaveArrangement { name: String::new(), bin });
            Ok(Value::Null)
        }
        "revealProject" => {
            let Some(p) = app.session.path.clone() else { return Some(Err("the project has not been saved".into())) };
            crate::panels::menu_dialogs::open_path(app, ctx, &p, true).map(|_| json!({"path": p}))
        }
        "newPanel" => {
            // a second Project panel: the project's root in a floating panel
            let root = app.session.project.root.id.0;
            let pp = &mut app.ui.project_panel;
            pp.tabs.push(BinTab { bin: root, floating: true, view: app.session.prefs.project_panel.view.mode, icon_size: 110.0, nav: None });
            Ok(json!({"tab": pp.tabs.len() - 1}))
        }
        _ => return None,
    })
}

/// I / O over a hover-scrubbed card set that item's In / Out at the hovered time; otherwise the
/// keys keep their usual meaning (Mark In / Out).
fn hover_mark(app: &mut FilmcraftApp, ctx: &egui::Context, is_in: bool) -> Result<Value, String> {
    let fresh = app.ui.project_panel.hover.filter(|h| ctx.cumulative_frame_nr().saturating_sub(h.frame) <= 2);
    let Some(h) = fresh else {
        let id = if is_in { "markers.markIn" } else { "markers.markOut" };
        return app.session.execute(id, json!({})).map_err(|e| e.to_string());
    };
    let key = if is_in { "in" } else { "out" };
    app.session.execute("project.setMarks", json!({"item": h.item, key: h.time})).map_err(|e| e.to_string())?;
    Ok(json!({"item": h.item, key: h.time}))
}

fn start_rename(app: &mut FilmcraftApp, params: &Value) -> Result<Value, String> {
    let item = params.get("item").and_then(Value::as_u64);
    let bin = params.get("bin").and_then(Value::as_u64);
    let (item, bin) = match (item, bin) {
        (None, None) => match (app.session.state.project_selection.first(), app.ui.project_panel.selected_bin) {
            // an item selection wins over a bin clicked earlier
            (Some(i), _) => (Some(i.0), None),
            (None, Some(b)) => (None, Some(b)),
            _ => return Err("select an item or bin to rename".into()),
        },
        x => x,
    };
    let text = match (item, bin) {
        (Some(i), _) => app.session.project.item(ItemId(i)).map(|x| x.name.clone()).ok_or("no such item")?,
        (_, Some(b)) => app.session.project.root.find_bin(BinId(b)).map(|x| x.name.clone()).ok_or("no such bin")?,
        _ => return Err("rename needs an item or a bin".to_string()),
    };
    let panel = view_of(app, shown_inst(app)).prefix;
    app.ui.project_panel.rename = Some(Rename { item, bin, text, panel });
    Ok(json!({"item": item, "bin": bin}))
}

/// Commit the inline rename (Enter / focus lost).
pub fn commit_rename(app: &mut FilmcraftApp) {
    let Some(r) = app.ui.project_panel.rename.take() else { return };
    let name = r.text.trim().to_string();
    if name.is_empty() {
        return;
    }
    let res = match (r.item, r.bin) {
        (Some(i), _) if app.session.project.item(ItemId(i)).is_some_and(|x| x.name != name) => {
            app.session.execute("clip.rename", json!({"item": i, "name": name}))
        }
        (_, Some(b)) if app.session.project.root.find_bin(BinId(b)).is_some_and(|x| x.name != name) => {
            app.session.execute("project.renameBin", json!({"bin": b, "name": name}))
        }
        _ => return,
    };
    if let Err(e) = res {
        app.ui.status = e.to_string();
    }
}

/// Is `kind` a sequence (double-click opens it in the Timeline)?
pub fn is_sequence(kind: &ItemKind) -> bool {
    matches!(kind, ItemKind::Sequence(_))
}

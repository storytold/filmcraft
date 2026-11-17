//! FilmCraft's panels in craft-ui's shared docking. craft-ui draws the frames, tab strips,
//! floating windows and drag previews and owns the gestures; FilmCraft draws the panel
//! bodies, the ≡ panel menu, the panel items of each tab's context menu and the Timeline's
//! sequence tabs, and maps its control commands onto
//! craft-ui's `DockCommand`.

use std::collections::HashMap;

use craft_ui::docking::{
    Action, Divider, DockArea, DockCommand, DockContent, DockEvent, DockStyle, FloatSpace, Hit, Layout, Node, ResizeEdge, WindowKind, Workspace,
};
use egui::{Rect, Sense, Stroke, StrokeKind, vec2};
use serde_json::{Value, json};

use crate::dock::{self, LayoutExt as _, PanelKind, SeqAction, SeqTabs};
use crate::icons::{self, Icon};
use crate::theme::Tokens;
use crate::{FilmcraftApp, panels};

/// The dock area's egui id; `DockArea::hit_rects` and `float_space` read by it.
pub fn area_id() -> egui::Id {
    egui::Id::unique("filmcraft-docking")
}

/// Where each panel body was drawn last frame, and on which layer.
fn bodies_id() -> egui::Id {
    area_id().with("bodies")
}

/// Room the ≡ panel menu button takes at the end of the shown tab's label.
const TAB_MENU_W: f32 = 18.0;

/// Width of the » button at the right end of a tab strip whose tabs do not all fit.
const CHEVRON_W: f32 = 24.0;

/// craft-ui's padding around a tab's label (`docking::surface::TAB_PADDING`, private there): a
/// tab's natural width is its label's plus this.
const TAB_PADDING: f32 = 24.0;

/// craft-ui's width for its own overflow button (`docking::render::group::OVERFLOW_WIDTH`).
const CRAFT_OVERFLOW_W: f32 = 22.0;

pub(crate) fn route(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id == "window.dockAction" {
        return Some((|| {
            let value = params.get("action").cloned().ok_or("missing docking action")?;
            let action = serde_json::from_value(value).map_err(|error| format!("invalid docking action: {error}"))?;
            app.apply_dock(action)?;
            Ok(Value::Null)
        })());
    }
    if matches!(id, "window.dockPanel" | "window.floatPanel" | "window.closePanel") {
        return Some(alias(app, ctx, id, params));
    }
    if id.starts_with("window.panel.") && DockCommand::<PanelKind>::IDS.contains(&id) {
        return Some((|| {
            let mut params = if params.is_object() { params.clone() } else { json!({}) };
            params["command"] = json!(id);
            let command: DockCommand<PanelKind> = serde_json::from_value(params).map_err(|error| format!("invalid docking command: {error}"))?;
            app.run_dock_command(ctx, command)?;
            Ok(Value::Null)
        })());
    }
    None
}

/// FilmCraft's own docking commands, as craft-ui `window.panel.*` commands. `panel` defaults to
/// the focused panel.
fn alias(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Result<Value, String> {
    let panel = match params.get("panel") {
        None => app.ui.focused,
        Some(value) => value.as_str().and_then(PanelKind::from_name).ok_or("unknown panel")?,
    };
    let command = match id {
        "window.closePanel" => json!({"command": "window.panel.close", "panel": panel}),
        "window.floatPanel" => match params.get("rect") {
            None => json!({"command": "window.panel.float", "panel": panel}),
            Some(value) => {
                let rect = serde_json::from_value::<[f32; 4]>(value.clone()).map_err(|_| "rect must contain four finite numbers")?;
                json!({"command": "window.panel.layout", "action": Action::Float { panel, rect }})
            }
        },
        _ => {
            let zone = match params.get("zone") {
                None => "center",
                Some(value) => value.as_str().ok_or("zone must be a string")?,
            };
            match params.get("anchor") {
                Some(anchor) => {
                    let anchor = anchor.as_str().and_then(PanelKind::from_name).ok_or("a known anchor panel is required")?;
                    let mut command = json!({"command": "window.panel.dock", "panel": panel, "anchor": anchor, "zone": zone.to_ascii_lowercase()});
                    if let Some(before) = params.get("before") {
                        command["before"] = json!(before.as_str().and_then(PanelKind::from_name).ok_or("unknown before panel")?);
                    }
                    command
                }
                None if params.get("before").is_some() => return Err("before needs an anchor panel".into()),
                // Without an anchor a side zone docks along that edge of the whole workspace.
                None => match zone.to_ascii_lowercase().as_str() {
                    "center" | "tab" => json!({"command": "window.panel.dock", "panel": panel}),
                    edge @ ("left" | "right" | "top" | "bottom") => json!({"command": "window.panel.dock", "panel": panel, "zone": edge}),
                    _ => return Err("unknown docking zone".into()),
                },
            }
        }
    };
    let command: DockCommand<PanelKind> = serde_json::from_value(command).map_err(|error| format!("invalid docking command: {error}"))?;
    app.run_dock_command(ctx, command)?;
    Ok(json!({"panel": panel.id()}))
}

/// Keep one docked destination available for dragging floating panels back into the workspace:
/// no move or float may leave the frame tree empty. Closing the last docked panel stays allowed.
fn check_last_docked(before: &Layout<PanelKind>, action: &Action<PanelKind>, after: &Layout<PanelKind>) -> Result<(), String> {
    let closing = matches!(action, Action::Close { .. } | Action::CloseGroup { .. } | Action::CloseWindow { .. } | Action::CloseOthers { .. });
    if !closing && before.root.is_some() && after.root.is_none() {
        return Err("the last docked panel must remain in the workspace".into());
    }
    Ok(())
}

impl FilmcraftApp {
    /// A layout action from the dock area, through the command path the control channel uses.
    fn dock_command(&mut self, ctx: &egui::Context, action: Action<PanelKind>) -> Result<(), String> {
        let value = serde_json::to_value(action).map_err(|error| error.to_string())?;
        crate::menus::invoke(self, ctx, "window.dockAction", json!({"action": value})).map(|_| ())
    }

    /// Run a craft-ui docking command on the current layout, all or nothing. Workspaces are
    /// FilmCraft's own (Window ▸ Workspaces).
    pub(crate) fn run_dock_command(&mut self, ctx: &egui::Context, command: DockCommand<PanelKind>) -> Result<(), String> {
        if command.is_app_owned() {
            return Err("use the window.workspace.* commands for workspaces".into());
        }
        // A closed panel reopens through FilmCraft's own path: where it was, floating again if it
        // was closed floating (#1985).
        if let DockCommand::Activate { panel } = &command
            && !self.ui.dock.contains(panel)
        {
            self.show_panel(*panel);
            return Ok(());
        }
        let space = craft_ui::docking::float_space(ctx, area_id()).unwrap_or_else(|| FloatSpace::new(self.float_bounds(ctx)));
        let actions = command.to_actions(&Workspace::new(self.ui.dock.clone()), false, &space).map_err(|error| error.to_string())?;
        self.apply_dock_all(actions)
    }

    /// Where floating windows may go. With `integrated_titlebar` the header sits in the macOS
    /// title-bar strip, where a press on a floating window would move the whole app window.
    pub(crate) fn float_bounds(&self, ctx: &egui::Context) -> Rect {
        let mut bounds = ctx.content_rect();
        if self.integrated_titlebar {
            bounds.min.y += crate::header::HEIGHT;
        }
        bounds
    }

    pub fn apply_dock(&mut self, action: Action<PanelKind>) -> Result<(), String> {
        self.apply_dock_all(vec![action])
    }

    /// Clicking the Timeline brings the Program monitor to the front of its tab group, as in
    /// Premiere, so the frame being edited is on show (#3653). The focus stays on the Timeline.
    /// Only a tab behind another in a docked group: not when Program is floating, closed, already
    /// in front, in a group the user collapsed (minimized, icon-only column or accordion), or in the
    /// Timeline's own group (fronting it would hide the Timeline).
    fn raise_program_for_timeline(&mut self) {
        let dock = &self.ui.dock;
        if dock.is_visible(PanelKind::Program) || tab_group(dock, PanelKind::Program).is_none_or(|group| group.contains(&PanelKind::Timeline)) {
            return;
        }
        if let Err(error) = self.apply_dock(Action::Activate { panel: PanelKind::Program }) {
            log::warn!("could not bring the Program monitor to the front: {error}");
        }
    }

    fn apply_dock_all(&mut self, actions: Vec<Action<PanelKind>>) -> Result<(), String> {
        let mut layout = self.ui.dock.clone();
        let mut relocating = false;
        let mut reveal = None;
        let mut closed_floating = self.ui.closed_floating.clone();
        for action in actions {
            relocating |= matches!(&action, Action::Move { .. } | Action::Float { .. } | Action::MoveGroup { .. } | Action::MoveWindow { .. });
            if let Action::Activate { panel } = &action {
                reveal = Some(*panel);
            }
            // a panel closed while floating remembers where; closed while docked, it forgets (#1985)
            for panel in layout.closed_by(&action).map_err(|error| error.to_string())? {
                closed_floating.retain(|(p, _)| *p != panel);
                if let Some(window) = layout.floating.iter().find(|window| window.contains(&panel)) {
                    closed_floating.push((panel, window.rect));
                }
            }
            let before = layout.clone();
            layout.apply(action.clone()).map_err(|error| error.to_string())?;
            check_last_docked(&before, &action, &layout)?;
        }
        self.ui.dock = layout;
        self.ui.closed_floating = closed_floating;
        if let Some(panel) = reveal {
            self.pending_panel_raise = Some(panel);
        }
        if relocating || self.ui.keys.maximized.is_some_and(|panel| !self.ui.dock.is_docked(panel)) {
            self.ui.keys.maximized = None;
        }
        Ok(())
    }

    /// OS drag positions come from the host cursor hook, so egui pointer capture alone cannot
    /// decide whether a Timeline is covered: the Timeline body drawn last frame must hold `pos`,
    /// with no floating window above it there.
    pub(crate) fn timeline_drop_target(&self, ctx: &egui::Context, pos: egui::Pos2) -> bool {
        if !ctx.content_rect().contains(pos) {
            return false;
        }
        let Some(bodies) = ctx.data(|data| data.get_temp::<Vec<(PanelKind, Rect, egui::LayerId)>>(bodies_id())) else { return true };
        let Some((_, _, timeline)) = bodies.iter().find(|(panel, rect, _)| *panel == PanelKind::Timeline && rect.contains(pos)) else { return false };
        let layers = ctx.memory(|memory| memory.layer_ids().collect::<Vec<_>>());
        let rank = |layer: &egui::LayerId| layers.iter().position(|other| other == layer);
        !self.ui.dock.floating.iter().any(|window| {
            let Some(layer) = DockArea::floating_layer(area_id(), window) else { return false };
            // Floating windows are hidden while a frame is maximized.
            ctx.memory(|memory| memory.areas().is_visible(&layer))
                && layer != *timeline
                && rank(&layer) > rank(timeline)
                && ctx.memory(|memory| memory.area_rect(layer.id)).is_some_and(|rect| rect.contains(pos))
        })
    }

    /// Whether a modal dialog is open: Settings, Recovery or Revert (egui `Modal`s), any other
    /// egui `Modal` shown last frame, the Save Project prompt of Quit, Close Project or Open
    /// Project, or a native file dialog. egui's `Modal` blocks input only in the main window, so
    /// the dock area blocks its native windows meanwhile.
    fn modal_open(&self, ctx: &egui::Context) -> bool {
        matches!(self.dialog, Some(crate::Dialog::Preferences | crate::Dialog::Recovery | crate::Dialog::RevertConfirm))
            || self.ui.clip_dialog.as_ref().is_some_and(|dialog| matches!(dialog.command.as_str(), "app.quit" | "file.closeProject" | "file.open"))
            || self.native_dialog_open()
            || ctx.memory(|memory| memory.top_modal_layer().is_some())
    }

    pub(crate) fn dock_area(&mut self, ui: &mut egui::Ui, body: Rect) {
        let t = self.tokens;
        let ctx = ui.ctx().clone();
        let seqs = SeqTabs {
            open: self
                .session
                .state
                .open_sequences
                .iter()
                .filter_map(|id| {
                    self.session
                        .project
                        .item(*id)
                        .filter(|item| matches!(item.kind, filmcraft_project::ItemKind::Sequence(_)))
                        .map(|item| (id.0, item.name.clone()))
                })
                .collect(),
            active: self.session.state.active_sequence.map(|id| id.0),
        };
        let style = dock_style(ui, &t);
        // No `monitors`: the monitor list comes with the egui fork's `ViewportInfo::monitors`.
        let control_input_only = self.control_input_only;
        let armed = std::sync::Arc::clone(&self.native_ctrl_click_armed);
        let mut area = DockArea::new(area_id())
            .float_bounds(self.float_bounds(&ctx))
            .maximized(self.ui.keys.maximized.filter(|panel| self.ui.dock.is_docked(*panel)))
            .input_blocked(self.modal_open(&ctx))
            .native_input_hook(move |_ctx, raw_input| crate::rewrite_native_window_input(control_input_only, &armed, raw_input));
        if let Some(panel) = self.pending_panel_raise.take() {
            area = area.raise_panel(panel);
        }
        let layout = self.ui.dock.clone();
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("filmcraft-dock-area"));
        child.set_clip_rect(body.intersect(ui.clip_rect()));
        let seq_width = dock::sequence_tabs_width(ui, &seqs);
        let (min_tab_width, drag_bar_width, tab_font) = (style.min_tab_width, style.drag_bar_width, style.font.clone());
        // Known before anything draws, so header elements of a native window (the ≡ menu, the
        // Timeline's sequence tabs), drawn before its bodies, carry that window too.
        self.auto.windows =
            self.ui.dock.floating.iter().enumerate().map(|(index, window)| (DockArea::<PanelKind>::window_viewport(area_id(), window.id), index)).collect();
        let mut content = FilmContent {
            app: self,
            seqs,
            seq_width,
            tokens: t,
            seq_actions: Vec::new(),
            menu: None,
            ctx: ctx.clone(),
            min_tab_width,
            drag_bar_width,
            tab_font,
            chevrons: std::cell::RefCell::default(),
            picked_tab: None,
            seq_menu_viewport: egui::ViewportId::ROOT,
            bodies: Vec::new(),
            restore_workspace: false,
        };
        let output = area.show_content(&mut child, &layout, &style, &mut content);
        let FilmContent { seq_actions, mut menu, seq_menu_viewport, bodies, restore_workspace, picked_tab, .. } = content;
        ctx.data_mut(|data| data.insert_temp(bodies_id(), bodies));
        if let Some(panel) = output.focused {
            self.ui.focused = panel;
            if panel == PanelKind::Timeline {
                self.raise_program_for_timeline();
            }
        }
        self.handle_forwarded_keys(&ctx, output.forwarded_events);
        self.window_files = output.window_files;
        if let Some(error) = output.error {
            self.ui.status = error.to_string();
        }
        for event in output.events {
            if let DockEvent::ToggleMaximize { panel } = event {
                panels::keyboard::toggle_maximize(self, panel);
            }
        }
        for action in output.actions {
            for action in self.close_keeping_timeline(action) {
                if let Err(error) = self.dock_command(&ctx, action) {
                    self.ui.status = error;
                }
            }
        }
        if let Some(panel) = picked_tab {
            match self.dock_command(&ctx, Action::Activate { panel }) {
                Ok(()) => self.ui.focused = panel,
                Err(error) => self.ui.status = error,
            }
        }
        if restore_workspace {
            let workspace = self.ui.workspace.clone();
            self.set_workspace(&workspace);
        }
        for action in seq_actions {
            let result = match action {
                SeqAction::Open(id) => {
                    let _ = self.apply_dock(Action::Activate { panel: PanelKind::Timeline });
                    self.ui.focused = PanelKind::Timeline;
                    if self.session.state.active_sequence.map(|item| item.0) == Some(id) {
                        Ok(Value::Null)
                    } else {
                        self.session.execute("sequence.open", json!({"item": id}))
                    }
                }
                SeqAction::Close(id) => self.session.execute("sequence.close", json!({"item": id})),
                SeqAction::Move(id, index) => self.session.execute("sequence.moveTab", json!({"item": id, "index": index})),
                SeqAction::Menu(id, pos) => {
                    menu = Some((PanelKind::Timeline, pos, seq_menu_viewport));
                    self.ui.focused = PanelKind::Timeline;
                    if self.session.state.active_sequence.map(|item| item.0) == Some(id) {
                        Ok(Value::Null)
                    } else {
                        self.session.execute("sequence.open", json!({"item": id}))
                    }
                }
            };
            if let Err(error) = result {
                self.ui.status = error.to_string();
            }
        }
        if let Some((panel, pos, viewport)) = menu {
            let frame = ctx.cumulative_frame_nr();
            ctx.data_mut(|data| {
                data.insert_temp(egui::Id::unique("panel-menu"), (panel, pos, viewport));
                data.insert_temp(egui::Id::unique("panel-menu-opened"), frame);
            });
        }
        // a menu left in a window that has since closed has nowhere to draw
        ctx.data_mut(|data| {
            let id = egui::Id::unique("panel-menu");
            let gone = data
                .get_temp::<(PanelKind, egui::Pos2, egui::ViewportId)>(id)
                .is_some_and(|(_, _, owner)| owner != egui::ViewportId::ROOT && !self.auto.windows.iter().any(|(viewport, _)| *viewport == owner));
            if gone {
                data.remove::<(PanelKind, egui::Pos2, egui::ViewportId)>(id);
            }
        });
        let hits = DockArea::<PanelKind>::hit_rects(&ctx, area_id());
        self.register_dock_hits(&hits);
        panels::panel_menu_popup(self, ui);
    }

    /// A layout action chosen in a panel ≡ menu, through `window.dockAction` as the dock area's
    /// own actions go: a close that would remove the Timeline while a sequence is open closes
    /// the active sequence instead. A failure goes to the status bar.
    pub(crate) fn run_layout_action(&mut self, ctx: &egui::Context, action: Action<PanelKind>) {
        for action in self.close_keeping_timeline(action) {
            if let Err(error) = self.dock_command(ctx, action) {
                self.ui.status = error;
            }
        }
    }

    /// The Timeline's tabs stand for its open sequences: a close from the dock area (a tab's
    /// middle click or Close Panel, a group, window or Close Other Panels item) that would
    /// remove the Timeline while a sequence is open closes the active sequence instead, and the
    /// Timeline stays. The other panels the action covers still close.
    fn close_keeping_timeline(&mut self, action: Action<PanelKind>) -> Vec<Action<PanelKind>> {
        if self.session.state.active_sequence.is_none() {
            return vec![action];
        }
        let closed = match self.ui.dock.closed_by(&action) {
            Ok(closed) if closed.contains(&PanelKind::Timeline) => closed,
            _ => return vec![action],
        };
        if let Err(error) = self.session.execute("sequence.close", json!({})) {
            self.ui.status = error.to_string();
        }
        closed.into_iter().filter(|panel| *panel != PanelKind::Timeline).map(|panel| Action::Close { panel }).collect()
    }

    /// Automation ids for craft-ui's gesture sources: `panel.tab.<Panel>`, `panel.drag.<Panel>`
    /// (the group's drag bar), `panel.tabs.more.<Panel>`, `panel.float.<n>.move` and `.resize`,
    /// `dock.gutter.<path>` (`a`/`b` per split level), `dock.corner.<outer path>.<inner path>`
    /// where two dividers meet, the same inside floating window `n` as `panel.float.<n>.gutter.…`
    /// and `panel.float.<n>.corner.…`. Controls of a floating window in its own OS window are
    /// registered in that window's viewport.
    fn register_dock_hits(&mut self, hits: &[(Hit<PanelKind>, Rect)]) {
        let window = |panel: &PanelKind| self.ui.dock.floating.iter().position(|window| window.contains(panel));
        let native: HashMap<usize, egui::ViewportId> = self.auto.windows.iter().map(|(viewport, index)| (*index, *viewport)).collect();
        let mut found = Vec::new();
        for (hit, rect) in hits {
            let viewport = match hit {
                Hit::Divider(Divider::Split(_) | Divider::Intersection(..)) => None,
                Hit::Divider(Divider::WindowSplit(panel, _) | Divider::WindowIntersection(panel, ..))
                | Hit::Tab(panel)
                | Hit::DragBar(panel)
                | Hit::Overflow(panel)
                | Hit::Title(panel)
                | Hit::Resize(panel, _) => window(panel).and_then(|index| native.get(&index).copied()),
                _ => None,
            }
            .unwrap_or(egui::ViewportId::ROOT);
            let entry = match hit {
                Hit::Tab(panel) => (format!("panel.tab.{}", panel.id()), panel.title()),
                Hit::DragBar(panel) => (format!("panel.drag.{}", panel.id()), "Move panel"),
                Hit::Overflow(panel) => (format!("panel.tabs.more.{}", panel.id()), "more tabs"),
                Hit::Title(panel) => match window(panel) {
                    Some(index) => (format!("panel.float.{index}.move"), "Move floating panel"),
                    None => continue,
                },
                Hit::Resize(panel, edge) => match window(panel) {
                    Some(index) if *edge == ResizeEdge::BottomRight => (format!("panel.float.{index}.resize"), "Resize floating panel"),
                    Some(index) => (format!("panel.float.{index}.resize.{edge:?}"), "Resize floating panel"),
                    None => continue,
                },
                Hit::Divider(Divider::Split(path)) => (format!("dock.gutter.{}", path_id(path)), "gutter"),
                Hit::Divider(Divider::Intersection(outer, inner)) => (format!("dock.corner.{}.{}", path_id(outer), path_id(inner)), "gutter corner"),
                Hit::Divider(Divider::WindowSplit(panel, path)) => match window(panel) {
                    Some(index) => (format!("panel.float.{index}.gutter.{}", path_id(path)), "gutter"),
                    None => continue,
                },
                Hit::Divider(Divider::WindowIntersection(panel, outer, inner)) => match window(panel) {
                    Some(index) => (format!("panel.float.{index}.corner.{}.{}", path_id(outer), path_id(inner)), "gutter corner"),
                    None => continue,
                },
                _ => continue,
            };
            found.push((entry, *rect, DockArea::hit_id(area_id(), hit), viewport));
        }
        // The widget id lets keyboard focus on a divider or grip map to its element (shortcuts
        // stay live while one is focused).
        for ((id, label), rect, widget, viewport) in found {
            self.auto.add_in(viewport, &id, rect, label);
            if let Some(element) = self.auto.elements.last_mut() {
                element.widget_id = Some(format!("{:016x}", widget.value()));
                element.response_id = Some(widget);
            }
        }
    }
}

/// A split path as `a`/`b` per level (first/second child).
fn path_id(path: &[bool]) -> String {
    path.iter().map(|second| if *second { 'b' } else { 'a' }).collect()
}

/// FilmCraft's window is all groups: drops on all four window edges, Ctrl/Cmd-drag undocks, a
/// release over no target cancels, and a floating window's title bar only moves it.
fn dock_style(ui: &egui::Ui, t: &Tokens) -> DockStyle {
    let mut style = DockStyle::from_ui(ui);
    style.side_edge_zones = true;
    style.new_dock_zones = false;
    style.maximize_item = true;
    style.close_others_item = true;
    style.command_press_floats = true;
    style.float_on_empty_drop = false;
    style.title_bar_docks = false;
    style.float_hint = true;
    style.wheel_selects_tabs = true;
    style.middle_click_closes_tabs = true;
    style.float_label = "Undock Panel".into();
    style.float_group_label = "Undock Panel Group".into();
    style.close_group_label = "Close Panel Group".into();
    style.tab_height = t.tab_h;
    style.gap = t.gap;
    style.background = t.panel_bg;
    style.tab_background = t.tab_bg;
    style.active_background = t.panel_bg;
    style.floating_background = t.panel_bg;
    style.text = t.tab_text_active;
    style.inactive_text = t.tab_text;
    style.accent = t.focus;
    style.font = Tokens::ui(12.0);
    style
}

/// FilmCraft's side of the dock area for one frame.
struct FilmContent<'a> {
    app: &'a mut FilmcraftApp,
    seqs: SeqTabs,
    /// The width the sequence tabs take in the Timeline's tab strip.
    seq_width: f32,
    tokens: Tokens,
    seq_actions: Vec<SeqAction>,
    /// The ≡ button clicked: its panel and where its menu opens.
    menu: Option<(PanelKind, egui::Pos2, egui::ViewportId)>,
    /// For measuring tab labels in [`DockContent::group_header_width`], which has no `Ui`.
    ctx: egui::Context,
    /// The dock style's tab minimum, drag bar width and tab font, as craft-ui fits tabs with them.
    min_tab_width: f32,
    drag_bar_width: f32,
    tab_font: egui::FontId,
    /// The groups (by shown panel) whose tabs overflow this pass, so their header draws a ».
    chevrons: std::cell::RefCell<Vec<PanelKind>>,
    /// A tab chosen in a » list: it is activated after the dock area is drawn.
    picked_tab: Option<PanelKind>,
    /// The viewport of the Timeline tab strip whose sequence tab menu was opened this pass.
    seq_menu_viewport: egui::ViewportId,
    bodies: Vec<(PanelKind, Rect, egui::LayerId)>,
    /// Restore Workspace chosen in a tab's menu; it replaces the layout after the dock area is drawn.
    restore_workspace: bool,
}

impl DockContent<PanelKind> for FilmContent<'_> {
    fn body(&mut self, ui: &mut egui::Ui, panel: &PanelKind) {
        let t = self.tokens;
        let frame = ui.max_rect();
        let rect = frame;
        self.bodies.push((*panel, rect, ui.layer_id()));
        // A panel in its own OS window draws in that window's coordinates: its automation
        // elements carry the window so they are not matched against main-window points.
        let viewport = ui.ctx().viewport_id();
        self.app.auto.viewport = viewport;
        self.app.auto.add(&format!("panel.{}", panel.id()), rect, panel.title());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("panel", panel.id())));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        panels::show(self.app, &mut child, *panel, rect);
        self.tab_menu_button(ui, *panel, rect);
        // FilmCraft's drag payload lives in context data, not egui's drag-and-drop, so craft-ui
        // routes it to the native window under the pointer only when asked, each frame.
        if panels::dragging(ui.ctx()) {
            craft_ui::docking::route_app_drag(ui.ctx(), area_id());
        }
        self.app.auto.viewport = egui::ViewportId::ROOT;
        // The body fills its frame; allocating it lets a floated panel take its full size.
        ui.allocate_rect(ui.max_rect(), Sense::hover());
        // Painted after the body so child backgrounds cannot hide the focus outline (#1075).
        if self.app.ui.focused == *panel {
            ui.painter().rect_stroke(frame, 0.0, Stroke::new(1.0, t.focus), StrokeKind::Inside);
        }
    }

    /// A native floating window drew its last body: its end-of-pass IME check, for the Program
    /// monitor's text editor when it is there.
    fn window_drawn(&mut self, ui: &mut egui::Ui) {
        panels::panel_menu_area(self.app, ui);
        self.app.finish_ime_window(ui.ctx());
    }

    fn logical_label(&self, panel: &PanelKind) -> String {
        crate::i18n::t(panel.title()).to_string()
    }

    /// Every floating panel gets its own OS window on desktop.
    fn allow_native(&self, _panel: &PanelKind) -> bool {
        true
    }

    /// The video viewers float as normal windows, say on a second display; the other panels
    /// are palettes.
    fn window_kind(&self, panel: &PanelKind) -> WindowKind {
        match panel {
            PanelKind::Program | PanelKind::Source | PanelKind::ReferenceMonitor | PanelKind::LumetriScopes => WindowKind::Normal,
            _ => WindowKind::Palette,
        }
    }

    /// With no sequence open, the Timeline's tab says so. The shown tab of a group keeps room
    /// after its name for its ≡ button.
    fn label(&mut self, _ui: &egui::Ui, panel: &PanelKind, logical: &str, font: &egui::FontId, color: egui::Color32) -> egui::WidgetText {
        self.tab_label(panel, logical, font, color).into()
    }

    /// FilmCraft's own panel items head a tab's context menu, as in its ≡ menu; craft-ui adds
    /// Undock, Move, Close and the group items after them.
    fn panel_menu(&mut self, ui: &mut egui::Ui, panel: &PanelKind) {
        let app = &mut *self.app;
        let mut close = false;
        if *panel == PanelKind::Timeline && app.session.state.active_sequence.is_some() {
            let response = ui.add_enabled(app.session.state.open_sequences.len() > 1, egui::Button::new(tl!("Close Other Timeline Panels")));
            app.auto.add_response("panel.tabMenu.Timeline.closeOthers", &response, "Close Other Timeline Panels");
            if response.clicked() {
                if let Err(error) = app.session.execute("sequence.closeOthers", json!({})) {
                    app.ui.status = error.to_string();
                }
                close = true;
            }
        }
        let response = ui.button(tl!("Restore Workspace"));
        app.auto.add_response(&format!("panel.tabMenu.{}.restoreWorkspace", panel.id()), &response, "Restore Workspace");
        if response.clicked() {
            self.restore_workspace = true;
            close = true;
        }
        close |= panels::panel_items(app, ui, *panel);
        ui.separator();
        if close {
            ui.close();
        }
    }

    /// The Timeline's sequence tabs while the Timeline is shown, and the » list when the group's
    /// tabs do not all fit. The sequence tabs take at most two thirds of the strip, so the group's
    /// own tabs keep room; past that they scroll into their own » list.
    fn group_header_width(&self, panels: &[PanelKind], active: &PanelKind, _floating: bool, strip_width: f32) -> f32 {
        let base = if *active == PanelKind::Timeline && !self.seqs.open.is_empty() { self.seq_width.min(strip_width * 2.0 / 3.0) } else { 0.0 };
        let bar = self.drag_bar_width.clamp(0.0, (strip_width - base).max(0.0));
        let widths: Vec<f32> = panels.iter().map(|panel| self.tab_width(panel)).collect();
        let overflow = tabs_overflow(&widths, strip_width - base - bar, self.min_tab_width);
        // `group_header` draws the » for the groups that asked for its room this pass
        let mut chevrons = self.chevrons.borrow_mut();
        chevrons.retain(|panel| panel != active);
        if overflow {
            chevrons.push(*active);
            (base + CHEVRON_W).min(strip_width.max(0.0))
        } else {
            base
        }
    }

    /// The Timeline's sequence tabs, then the » list of every tab in the group when they overflow.
    fn group_header(&mut self, ui: &mut egui::Ui, panels: &[PanelKind], active: &PanelKind, _floating: bool) {
        let t = self.tokens;
        let header = ui.max_rect();
        let chevron = self.chevrons.borrow().contains(active);
        let seq_strip = if chevron { Rect::from_min_max(header.min, egui::pos2((header.max.x - CHEVRON_W).max(header.min.x), header.max.y)) } else { header };
        if *active == PanelKind::Timeline && !self.seqs.open.is_empty() {
            let actions = dock::sequence_tabs(ui, seq_strip, &self.seqs, &t, &mut self.app.auto);
            if actions.iter().any(|a| matches!(a, SeqAction::Menu(..))) {
                self.seq_menu_viewport = ui.ctx().viewport_id();
            }
            self.seq_actions.extend(actions);
        }
        if chevron {
            let rect = Rect::from_min_max(egui::pos2(seq_strip.max.x, header.min.y), header.max);
            self.tab_list(ui, rect, panels, *active);
        }
    }
}

impl FilmContent<'_> {
    /// A tab's label: the panel's name (the Timeline's with no sequence open says so), with room
    /// after it for the ≡ button when the panel is the one its group shows.
    fn tab_label(&self, panel: &PanelKind, logical: &str, font: &egui::FontId, color: egui::Color32) -> egui::text::LayoutJob {
        let name = if *panel == PanelKind::Timeline && self.seqs.open.is_empty() { self.seqs.label() } else { logical.to_owned() };
        let format = egui::TextFormat { font_id: font.clone(), color, ..Default::default() };
        let mut job = egui::text::LayoutJob::default();
        job.append(&name, 0.0, format.clone());
        if self.app.ui.dock.is_visible(*panel) {
            job.append(" ", TAB_MENU_W, format);
        }
        job
    }

    /// A tab's natural width, as craft-ui measures it to fit the group's tabs.
    fn tab_width(&self, panel: &PanelKind) -> f32 {
        let logical = crate::i18n::t(panel.title()).to_string();
        let job = self.tab_label(panel, &logical, &self.tab_font, egui::Color32::WHITE);
        self.ctx.fonts_mut(|fonts| fonts.layout_job(job)).size().x + TAB_PADDING
    }

    /// The ≡ button at the end of the shown tab of `panel`'s group, whose body is `body`: it opens
    /// the panel menu below it. The tab is the one craft-ui drew this pass right above the body;
    /// an accordion entry or a tab from an earlier pass has none.
    fn tab_menu_button(&mut self, ui: &mut egui::Ui, panel: PanelKind, body: Rect) {
        let t = self.tokens;
        let Some(tab) = ui.ctx().read_response(DockArea::<PanelKind>::hit_id(area_id(), &Hit::Tab(panel))).map(|response| response.rect) else { return };
        if !tab_menu_fits(tab, body) {
            return;
        }
        let rect = tab_menu_rect(tab);
        // Drawn on the tab's layer after the tab, so the button takes the click; a drag from
        // it still moves the tab, which senses drags.
        let mut over = ui.new_child(egui::UiBuilder::new().max_rect(tab).id_salt(("tab-menu", panel.id())));
        over.set_clip_rect(tab);
        let response = over.interact(rect, egui::Id::unique(("panel-menu-button", panel.id())), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, response.enabled(), "panel menu"));
        self.app.auto.add_response(&format!("panel.menu.{}", panel.id()), &response, "panel menu");
        let painter = over.painter();
        // a squeezed tab's cut label runs under the button
        painter.rect_filled(rect, 0.0, t.panel_bg);
        let color = if response.hovered() { t.tab_text_active } else { t.tab_text };
        icons::paint(painter, Rect::from_center_size(rect.center(), vec2(14.0, 14.0)), Icon::Hamburger, color);
        dock::paint_widget_focus(painter, &response, &t);
        if response.clicked() {
            self.app.ui.focused = panel;
            self.menu = Some((panel, rect.left_bottom(), ui.ctx().viewport_id()));
        }
    }

    /// The » button in `rect` and its list of every tab in the group, the shown one ticked.
    /// Choosing one shows it.
    fn tab_list(&mut self, ui: &mut egui::Ui, rect: Rect, panels: &[PanelKind], active: PanelKind) {
        let t = self.tokens;
        let list_id = egui::Id::unique(("panel-tab-list", active.id()));
        let button = Rect::from_center_size(rect.center(), vec2(18.0, 20.0)).intersect(rect);
        let response = ui.interact(button, egui::Id::unique(("panel-tab-list-button", active.id())), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, response.enabled(), "all tabs"));
        self.app.auto.add_response(&format!("panel.tabs.all.{}", active.id()), &response, "all tabs");
        let color = if response.hovered() { t.tab_text_active } else { t.tab_text };
        let painter = ui.painter_at(rect);
        icons::paint(&painter, button.shrink(4.0).translate(vec2(-2.0, 0.0)), Icon::ChevronRight, color);
        icons::paint(&painter, button.shrink(4.0).translate(vec2(2.0, 0.0)), Icon::ChevronRight, color);
        dock::paint_widget_focus(&painter, &response, &t);
        let mut opened = false;
        if response.clicked() {
            let open = ui.ctx().data(|d| d.get_temp::<bool>(list_id)).unwrap_or(false);
            ui.ctx().data_mut(|d| d.insert_temp(list_id, !open));
            opened = !open;
        }
        if !ui.ctx().data(|d| d.get_temp::<bool>(list_id)).unwrap_or(false) {
            return;
        }
        let mut picked = None;
        let auto = &mut self.app.auto;
        let area = egui::Area::new(list_id.with("area")).order(egui::Order::Foreground).pivot(egui::Align2::RIGHT_TOP).fixed_pos(button.right_bottom()).show(
            ui.ctx(),
            |ui| {
                egui::Frame::popup(ui.style()).show(ui, |ui| {
                    egui::containers::menu::menu_style(ui.style_mut());
                    // the names line up whether ticked or not
                    let font = egui::TextStyle::Button.resolve(ui.style());
                    let measure =
                        |ui: &egui::Ui, text: &str| ui.fonts_mut(|fonts| fonts.layout_no_wrap(text.into(), font.clone(), egui::Color32::WHITE)).size().x;
                    let tick = measure(ui, "✓ ");
                    // as wide as the longest name, not the window (#4446)
                    let widest = panels.iter().map(|panel| measure(ui, crate::i18n::t(panel.title()))).fold(0.0_f32, f32::max);
                    let width = (tick + widest + 2.0 * ui.spacing().button_padding.x).clamp(160.0, 480.0);
                    ui.set_width(width);
                    egui::ScrollArea::vertical().max_height(360.0).show(ui, |ui| {
                        for (panel, shown) in tab_list_entries(panels, active) {
                            let name = crate::i18n::t(panel.title());
                            let format = egui::TextFormat { font_id: font.clone(), color: ui.visuals().text_color(), ..Default::default() };
                            let mut job = egui::text::LayoutJob::default();
                            if shown {
                                job.append("✓ ", 0.0, format.clone());
                                job.append(name, 0.0, format);
                            } else {
                                job.append(name, tick, format);
                            }
                            let response = ui.add(egui::Button::new(job).min_size(vec2(width, 0.0)));
                            // the label says which is shown, as the list does
                            let label = if shown { format!("✓ {}", panel.title()) } else { panel.title().to_owned() };
                            auto.add_response(&format!("panel.tabs.list.{}", panel.id()), &response, &label);
                            if response.clicked() {
                                picked = Some(panel);
                            }
                        }
                    });
                });
            },
        );
        if picked.is_some() {
            self.picked_tab = picked;
        }
        if picked.is_some() || (!opened && area.response.clicked_elsewhere()) || crate::widgets::escape_closes(ui.ctx()) {
            ui.ctx().data_mut(|d| d.remove::<bool>(list_id));
        }
    }
}

/// The panels of the tab group holding `panel`, docked or floating, in tab order; just `panel`
/// when it is not in a tab group (an accordion entry), nothing when it is closed.
pub(crate) fn group_of(layout: &Layout<PanelKind>, panel: PanelKind) -> Vec<PanelKind> {
    fn find(node: &Node<PanelKind>, panel: PanelKind) -> Option<Vec<PanelKind>> {
        match node {
            Node::Tabs { panels, .. } => panels.contains(&panel).then(|| panels.clone()),
            Node::Dock { columns, .. } => {
                columns.iter().flat_map(|column| column.groups.iter()).find(|group| group.panels.contains(&panel)).map(|group| group.panels.clone())
            }
            Node::Split { first, second, .. } => find(first, panel).or_else(|| find(second, panel)),
            _ => None,
        }
    }
    let found = layout.root.iter().chain(layout.floating.iter().map(|window| &window.root)).find_map(|root| find(root, panel));
    match found {
        Some(panels) => panels,
        None if layout.contains(&panel) => vec![panel],
        None => Vec::new(),
    }
}

/// Whether `tab` is the tab right above `body`, so its ≡ belongs on it: a rect from an earlier
/// pass, after the layout changed, sits elsewhere.
fn tab_menu_fits(tab: Rect, body: Rect) -> bool {
    tab.is_positive() && (tab.bottom() - body.top()).abs() <= 1.0 && tab.left() >= body.left() - 1.0 && tab.right() <= body.right() + 1.0
}

/// Where the ≡ button sits on a shown `tab`: in the room its label keeps for it, before the tab's
/// right padding. A tab squeezed narrower than that keeps the button inside it, over the cut label.
fn tab_menu_rect(tab: Rect) -> Rect {
    let half = TAB_MENU_W * 0.5;
    let x = tab.right() - TAB_PADDING * 0.5 - half + 4.0;
    let x = if tab.width() <= TAB_MENU_W { tab.center().x } else { x.clamp(tab.left() + half, tab.right() - half) };
    Rect::from_center_size(egui::pos2(x, tab.center().y), vec2(TAB_MENU_W, TAB_MENU_W)).intersect(tab)
}

/// Whether a group's tabs, of natural `widths`, overflow a strip with `available` room for them
/// (craft-ui's fit: tabs shrink to `minimum`, then the rest go to a list). One tab never does.
fn tabs_overflow(widths: &[f32], available: f32, minimum: f32) -> bool {
    !craft_ui::tabs::fit(widths, 0, available, minimum, CRAFT_OVERFLOW_W).overflow.is_empty()
}

/// The » list of a group: every tab in order, with whether it is the shown one.
fn tab_list_entries(panels: &[PanelKind], active: PanelKind) -> Vec<(PanelKind, bool)> {
    panels.iter().map(|panel| (*panel, *panel == active)).collect()
}

/// The panels of the docked tab group holding `panel`, if that group shows one tab at a time and
/// is expanded: an accordion, a minimized group or an icon-only dock column gives `None`.
fn tab_group(layout: &Layout<PanelKind>, panel: PanelKind) -> Option<Vec<PanelKind>> {
    fn find(node: &Node<PanelKind>, panel: PanelKind) -> Option<Vec<PanelKind>> {
        match node {
            Node::Tabs { panels, .. } => panels.contains(&panel).then(|| panels.clone()),
            Node::Dock { columns, .. } => columns
                .iter()
                .filter(|column| !column.iconic)
                .flat_map(|column| column.groups.iter())
                .find(|group| !group.minimized && group.panels.contains(&panel))
                .map(|group| group.panels.clone()),
            Node::Split { first, second, .. } => find(first, panel).or_else(|| find(second, panel)),
            _ => None,
        }
    }
    layout.root.as_ref().and_then(|root| find(root, panel))
}

#[cfg(test)]
mod tests {
    use craft_ui::docking::{Column, Group, Node, Side, StackEntry};
    use craft_ui::layout::{SplitAxis, SplitSize};

    use super::{group_of, tab_group, tab_list_entries, tab_menu_fits, tab_menu_rect, tabs_overflow};
    use crate::dock::{Layout, PanelKind};

    /// The Timeline beside a dock column holding `group`.
    fn beside_dock(group: Group<PanelKind>, iconic: bool) -> Layout {
        let column = Column { groups: vec![group], width: 300.0, iconic, iconic_width: None };
        let dock = Node::Dock { side: Side::End, columns: vec![column] };
        let timeline = Node::Tabs { panels: vec![PanelKind::Timeline], active: 0 };
        let root = Node::Split { axis: SplitAxis::Horizontal, size: SplitSize::Ratio(0.5), first: Box::new(timeline), second: Box::new(dock) };
        Layout { root: Some(root), floating: vec![] }
    }

    /// #3653: Program is fronted only from an expanded group that shows one tab at a time, so a
    /// Timeline click never expands what the user collapsed.
    #[test]
    fn program_is_fronted_only_from_an_expanded_tab_group() {
        let group = || Group::new(vec![PanelKind::Properties, PanelKind::Program], 0);
        assert_eq!(tab_group(&beside_dock(group(), false), PanelKind::Program), Some(vec![PanelKind::Properties, PanelKind::Program]));
        assert_eq!(tab_group(&beside_dock(Group { minimized: true, ..group() }, false), PanelKind::Program), None, "a minimized group");
        assert_eq!(tab_group(&beside_dock(group(), true), PanelKind::Program), None, "an icon-only column");
        let entries = [PanelKind::Properties, PanelKind::Program].map(|panel| StackEntry { panel, open: false, height: None }).to_vec();
        let accordion = Layout { root: Some(Node::Stack { entries }), floating: vec![] };
        assert_eq!(tab_group(&accordion, PanelKind::Program), None, "an accordion");
    }

    /// Tabs overflow only when even at the minimum width they do not fit, as craft-ui fits them.
    #[test]
    fn tabs_overflow_only_past_their_minimum_width() {
        assert!(!tabs_overflow(&[120.0, 120.0], 240.0, 54.0), "both fit");
        assert!(!tabs_overflow(&[120.0, 120.0], 110.0, 54.0), "both shrink to fit");
        assert!(tabs_overflow(&[120.0, 120.0, 120.0], 110.0, 54.0), "three at their minimum do not fit");
        assert!(!tabs_overflow(&[400.0], 10.0, 54.0), "one tab never overflows");
        assert!(!tabs_overflow(&[], 0.0, 54.0));
        assert!(tabs_overflow(&[60.0, 60.0], -5.0, 54.0), "no room at all");
        assert!(!tabs_overflow(&[f32::NAN, f32::INFINITY], f32::NAN, f32::NAN), "hostile widths do not panic");
    }

    /// The » list holds every tab of the group in order, only the shown one ticked.
    #[test]
    fn the_tab_list_ticks_only_the_shown_tab() {
        use PanelKind::{Effects, MediaBrowser, Project};
        assert_eq!(tab_list_entries(&[Project, MediaBrowser, Effects], MediaBrowser), vec![(Project, false), (MediaBrowser, true), (Effects, false)]);
        assert_eq!(tab_list_entries(&[Project], Effects), vec![(Project, false)], "a shown panel outside the group ticks nothing");
        assert!(tab_list_entries(&[], Project).is_empty());
    }

    /// Close Other Panels in Group acts on the group holding the panel: docked, in a dock column
    /// or floating.
    #[test]
    fn group_of_finds_the_panels_tab_group_wherever_it_is() {
        use PanelKind::{Effects, History, Program, Project, Timeline};
        use craft_ui::docking::Floating;
        let mut layout = beside_dock(Group::new(vec![Program, Effects], 0), false);
        layout.floating.push(Floating::single(vec![Project, History], 1, [0.0, 0.0, 300.0, 200.0]));
        assert_eq!(group_of(&layout, Timeline), vec![Timeline]);
        assert_eq!(group_of(&layout, Effects), vec![Program, Effects], "a dock column's group");
        assert_eq!(group_of(&layout, History), vec![Project, History], "a floating window's group");
        assert!(group_of(&layout, PanelKind::Markers).is_empty(), "a closed panel has none");
    }

    /// The ≡ sits inside its tab at every width, and only on the tab right above the body.
    #[test]
    fn the_tab_menu_stays_inside_its_tab() {
        use egui::{Rect, pos2};
        let body = Rect::from_min_max(pos2(0.0, 30.0), pos2(400.0, 300.0));
        for width in [0.5, 10.0, 18.0, 30.0, 120.0] {
            let tab = Rect::from_min_max(pos2(4.0, 6.0), pos2(4.0 + width, 30.0));
            let menu = tab_menu_rect(tab);
            assert!(tab.contains_rect(menu), "{width}: {menu:?} in {tab:?}");
        }
        let tab = Rect::from_min_max(pos2(4.0, 6.0), pos2(124.0, 30.0));
        assert!(tab_menu_rect(tab).center().x > tab.center().x, "after the name");
        assert!(tab_menu_fits(tab, body));
        assert!(!tab_menu_fits(tab.translate(egui::vec2(0.0, 40.0)), body), "a tab elsewhere is from an earlier layout");
        assert!(!tab_menu_fits(tab.translate(egui::vec2(500.0, 0.0)), body), "outside the body's width");
        assert!(!tab_menu_fits(Rect::NOTHING, body));
    }
}

//! FilmCraft's panels in craft-ui's `DockArea`. craft-ui draws the frames, tab strips, floating
//! windows and drag previews and owns the gestures; FilmCraft draws the panel bodies, the ≡ panel
//! menu and the Timeline's sequence tabs in each group's header, and maps its control commands
//! onto craft-ui's dock commands.
//!
//! `ui.dock` stays the saved source of truth: the area shows a live `Workspace` that follows it
//! and writes nothing itself, and every change it reports runs through `window.dockAction`.

use std::sync::Arc;

use craft_behavior::DockSide;
use craft_behavior::dock::{Action, Layout, Node, Permissions, Workspace};
use craft_host::target::WindowKind;
use craft_ui::contract::{Bind, BodyCx, Tracked};
use craft_ui::docking::{
    DividerKind, DockCommands, DockConfig, DockEvent, DockPanels, DockScene, DockSizes, DockSource, DockTo, DragUnit, FloatSpace, PanelSpec, PanelSpecs,
    SourceKind, actions, move_actions,
};
use craft_ui::patterns::dock::DockArea;
use craft_ui::runtime::windows::ResizeEdge;
use egui::{Rect, Sense, Stroke, StrokeKind, vec2};
use serde_json::{Value, json};

use crate::dock::{self, LayoutExt as _, PanelKind, SeqAction, SeqTabs};
use crate::icons::{self, Icon};
use crate::theme::Tokens;
use crate::{FilmcraftApp, panels};

/// Where each panel body was drawn last frame, and on which layer.
fn bodies_id() -> egui::Id {
    egui::Id::unique("filmcraft-dock-bodies")
}

/// The dock area's gesture sources of the last pass, as automation elements.
fn sources_id() -> egui::Id {
    egui::Id::unique("filmcraft-dock-sources")
}

/// The app's focused panel at the end of the last dock pass.
fn app_focus_id() -> egui::Id {
    egui::Id::unique("filmcraft-dock-app-focus")
}

/// The panel of the group that held key focus in the last pass, as the dock area reported it.
fn key_focus_id() -> egui::Id {
    egui::Id::unique("filmcraft-dock-key-focus")
}

/// One gesture source of the dock area: its automation id, where it was drawn (in its own OS
/// window's coordinates for a native floating window), the id of its node, and whether it is a
/// divider or a resize grip, which take the arrow keys while focused.
#[derive(Clone, Debug)]
struct DockHit {
    id: String,
    rect: Rect,
    widget: egui::Id,
    viewport: egui::ViewportId,
    grip: bool,
}

/// `a`/`b` per split level from the root, as in `dock.gutter.<path>`.
fn path_id(path: &[bool]) -> String {
    path.iter().map(|second| if *second { 'b' } else { 'a' }).collect()
}

/// The dock area's gesture sources of the last pass, for `ui.docking`'s `hitRects`: each with its
/// automation id and rect.
pub(crate) fn hit_rects(ctx: &egui::Context) -> Vec<Value> {
    let hits = ctx.data(|data| data.get_temp::<Vec<DockHit>>(sources_id())).unwrap_or_default();
    hits.iter().map(|hit| json!({"hit": hit.id, "rect": [hit.rect.left(), hit.rect.top(), hit.rect.width(), hit.rect.height()]})).collect()
}

/// Whether key focus is on a dock divider or a floating window's resize grip, as drawn last pass:
/// the arrow keys then nudge it and fire no shortcut (#1482).
pub(crate) fn divider_focused(ctx: &egui::Context) -> bool {
    let focused = [craft_ui::runtime::access::focused_node(ctx), ctx.memory(|memory| memory.focused())];
    let hits = ctx.data(|data| data.get_temp::<Vec<DockHit>>(sources_id())).unwrap_or_default();
    hits.iter().any(|hit| hit.grip && focused.contains(&Some(hit.widget)))
}

/// Width of the ≡ panel menu button in each group's header.
const MENU_W: f32 = 26.0;

/// FilmCraft's `window.panel.*` control commands: the docking vocabulary of its control channel.
const PANEL_COMMANDS: [&str; 9] = [
    "window.panel.move",
    "window.panel.float",
    "window.panel.dock",
    "window.panel.activate",
    "window.panel.close",
    "window.panel.closeGroup",
    "window.panel.closeOthers",
    "window.panel.minimize",
    "window.panel.layout",
];

/// The panels' specs the dock area reads: each panel's tab title and the kind of OS window it
/// floats in. While no sequence is open the Timeline's tab says so.
#[derive(Clone, Debug, Default, PartialEq, craft_ui::contract::PlainData)]
pub(crate) struct FilmSpecs {
    timeline_title: Option<String>,
}

impl PanelSpecs<PanelKind> for FilmSpecs {
    fn spec(&self, panel: &PanelKind) -> PanelSpec {
        let title = match (panel, &self.timeline_title) {
            (PanelKind::Timeline, Some(title)) => title.clone(),
            _ => crate::i18n::t(panel.title()).to_string(),
        };
        // The video viewers float as document windows, say on a second display; the other panels
        // are tool windows.
        let kind = match panel {
            PanelKind::Program | PanelKind::Source | PanelKind::ReferenceMonitor | PanelKind::LumetriScopes => WindowKind::Document,
            _ => WindowKind::Tool,
        };
        PanelSpec::new(title).window_kind(kind).allow_native(true)
    }
}

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
    if PANEL_COMMANDS.contains(&id) {
        return Some(app.run_panel_command(ctx, id, params).map(|()| Value::Null));
    }
    None
}

/// The panel named by `params[key]`, if given.
fn panel_param(params: &Value, key: &str) -> Result<Option<PanelKind>, String> {
    match params.get(key) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value.as_str().and_then(PanelKind::from_name).map(Some).ok_or_else(|| format!("unknown {key} panel")),
    }
}

/// The side a `zone` names: `center` (or `tab`) joins the anchor's tabs. `left` and `right` are
/// the start and end sides in reading order.
fn side_of(zone: &str) -> Result<DockSide, String> {
    match zone.to_ascii_lowercase().as_str() {
        "center" | "tab" => Ok(DockSide::Center),
        "left" | "start" => Ok(DockSide::Start),
        "right" | "end" => Ok(DockSide::End),
        "top" => Ok(DockSide::Top),
        "bottom" => Ok(DockSide::Bottom),
        _ => Err("unknown docking zone".into()),
    }
}

/// Where `params` dock a panel: into the anchor's tabs (before `before`), beside the anchor on its
/// `zone`, along that edge of the whole workspace without an anchor, else back to the docked
/// layout.
fn destination(params: &Value) -> Result<DockTo<PanelKind>, String> {
    let side = match params.get("zone") {
        None | Some(Value::Null) => None,
        Some(value) => Some(side_of(value.as_str().ok_or("zone must be a string")?)?),
    };
    let before = panel_param(params, "before")?;
    match panel_param(params, "anchor")? {
        Some(anchor) => match side {
            None | Some(DockSide::Center) => Ok(DockTo::tabs(anchor, before)),
            Some(_) if before.is_some() => Err("before applies to the anchor's tabs".into()),
            Some(side) => Ok(DockTo::beside(anchor, side)),
        },
        None if before.is_some() => Err("before needs an anchor panel".into()),
        None => match side {
            None | Some(DockSide::Center) => Ok(DockTo::Docked),
            Some(side) => Ok(DockTo::Edge(side)),
        },
    }
}

/// FilmCraft's own docking commands, as its `window.panel.*` commands. `panel` defaults to the
/// focused panel.
fn alias(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str, params: &Value) -> Result<Value, String> {
    let panel = panel_param(params, "panel")?.unwrap_or(app.ui.focused);
    let mut command = if params.is_object() { params.clone() } else { json!({}) };
    command["panel"] = json!(panel.id());
    match id {
        "window.closePanel" => app.run_panel_command(ctx, "window.panel.close", &command)?,
        "window.floatPanel" => match params.get("rect") {
            None => app.run_panel_command(ctx, "window.panel.float", &command)?,
            Some(value) => {
                let rect = serde_json::from_value::<[f32; 4]>(value.clone()).map_err(|_| "rect must contain four finite numbers")?;
                if !rect.iter().all(|v| v.is_finite()) {
                    return Err("rect must contain four finite numbers".into());
                }
                app.apply_dock(Action::float(panel, dock::rect_of(rect)))?;
            }
        },
        _ => app.run_panel_command(ctx, "window.panel.dock", &command)?,
    }
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

    /// Run one of FilmCraft's `window.panel.*` commands on the current layout, all or nothing.
    /// Workspaces are FilmCraft's own (Window ▸ Workspaces).
    pub(crate) fn run_panel_command(&mut self, ctx: &egui::Context, id: &str, params: &Value) -> Result<(), String> {
        if id == "window.panel.layout" {
            let value = params.get("action").cloned().ok_or("missing docking action")?;
            let action = serde_json::from_value(value).map_err(|error| format!("invalid docking action: {error}"))?;
            return self.apply_dock(action);
        }
        let panel = panel_param(params, "panel")?.ok_or("missing panel")?;
        // A closed panel reopens through FilmCraft's own path: where it was, floating again if it
        // was closed floating (#1985).
        if id == "window.panel.activate" {
            if !self.ui.dock.contains(&panel) {
                self.show_panel(panel);
                return Ok(());
            }
            return self.apply_dock(Action::activate(panel));
        }
        let command = match id {
            "window.panel.float" if params.get("group").and_then(Value::as_bool) == Some(true) => Some(DockCommands::FloatGroup),
            "window.panel.float" => Some(DockCommands::FloatPanel),
            "window.panel.close" => Some(DockCommands::ClosePanel),
            "window.panel.closeGroup" => Some(DockCommands::CloseGroup),
            "window.panel.closeOthers" => Some(DockCommands::CloseOthers),
            "window.panel.minimize" => match params.get("minimized").and_then(Value::as_bool) {
                Some(minimized) => return self.apply_dock(Action::set_minimized(panel, minimized)),
                None => Some(DockCommands::ToggleMinimized),
            },
            _ => None,
        };
        let fallback;
        let space = match &self.dock_float_space {
            Some(space) => space,
            None => {
                fallback = FloatSpace::new(self.float_bounds(ctx), &DockSizes::of(ctx));
                &fallback
            }
        };
        let permissions = |_: &PanelKind| Permissions::ALL;
        let scene = DockScene::new(&self.ui.dock, space, false, &permissions).floated(self.dock_live.floated());
        let list = match command {
            Some(command) => actions(&scene, command, &panel),
            None => move_actions(&scene, &DragUnit::Panel(panel), destination(params)?),
        }
        .map_err(|error| error.to_string())?;
        self.apply_dock_all(list)
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
        // Program's tab comes to the front without taking the focus from the Timeline.
        let focus = self.dock_focus;
        if let Err(error) = self.apply_dock(Action::activate(PanelKind::Program)) {
            log::warn!("could not bring the Program monitor to the front: {error}");
        }
        self.dock_focus = focus;
    }

    fn apply_dock_all(&mut self, actions: Vec<Action<PanelKind>>) -> Result<(), String> {
        let mut layout = self.ui.dock.clone();
        let mut relocating = false;
        let mut closed_floating = self.ui.closed_floating.clone();
        for action in &actions {
            relocating |= matches!(action, Action::Move { .. } | Action::Float { .. } | Action::MoveGroup { .. } | Action::MoveWindow { .. });
            // a panel closed while floating remembers where; closed while docked, it forgets (#1985)
            for panel in layout.closed_by(action).map_err(|error| error.to_string())? {
                closed_floating.retain(|(p, _)| *p != panel);
                if let Some(window) = layout.floating.iter().find(|window| window.contains(&panel)) {
                    closed_floating.push((panel, dock::array_of(window.rect)));
                }
            }
            let before = layout.clone();
            layout.apply(action.clone()).map_err(|error| error.to_string())?;
            check_last_docked(&before, action, &layout)?;
        }
        self.ui.dock = layout;
        self.ui.closed_floating = closed_floating;
        // As in production, an applied Activate brings its panel forward: for a panel in a
        // floating window the next dock pass focuses it once, which raises that window. A docked
        // panel's tab is already in front, and key focus stays where it was.
        if let Some(panel) = actions.iter().rev().find_map(|action| match action {
            Action::Activate { panel, .. } => Some(*panel),
            _ => None,
        }) && self.ui.dock.floating.iter().any(|window| window.contains(&panel))
        {
            self.dock_focus = Some(panel);
        }
        // The live workspace takes the same actions, so its floating windows keep their ids.
        let _ = Arc::make_mut(&mut self.dock_live).apply_all(actions, false, |_| Permissions::ALL);
        self.sync_live();
        if relocating || self.ui.keys.maximized.is_some_and(|panel| !self.ui.dock.is_docked(panel)) {
            self.ui.keys.maximized = None;
        }
        Ok(())
    }

    /// The live workspace takes `ui.dock` where they differ: a workspace switch, a reset, or a
    /// layout set by a control request.
    fn sync_live(&mut self) {
        if self.dock_live.layout() != &self.ui.dock {
            Arc::make_mut(&mut self.dock_live).replace(Workspace::new(self.ui.dock.clone()));
        }
    }

    /// OS drag positions come from the host cursor hook, so egui pointer capture alone cannot
    /// decide whether a Timeline is covered: the Timeline body drawn last frame must hold `pos`,
    /// with no floating window above it there: neither another panel's body nor the window's
    /// title bar and tab strip above that body.
    pub(crate) fn timeline_drop_target(&self, ctx: &egui::Context, pos: egui::Pos2) -> bool {
        if !ctx.content_rect().contains(pos) {
            return false;
        }
        let Some(bodies) = ctx.data(|data| data.get_temp::<Vec<(PanelKind, Rect, egui::LayerId)>>(bodies_id())) else { return true };
        let Some((_, _, timeline)) = bodies.iter().find(|(panel, rect, _)| *panel == PanelKind::Timeline && rect.contains(pos)) else { return false };
        let layers = ctx.memory(|memory| memory.layer_ids().collect::<Vec<_>>());
        let rank = |layer: &egui::LayerId| layers.iter().position(|other| other == layer);
        let hits = ctx.data(|data| data.get_temp::<Vec<DockHit>>(sources_id())).unwrap_or_default();
        // A body drawn last frame on a layer above the Timeline's, of a panel that still floats: its
        // window covers `pos` from its grip down to the bottom of its body. A window closed or
        // docked since covers nothing.
        !bodies.iter().any(|(panel, rect, layer)| {
            if *panel == PanelKind::Timeline || layer == timeline || rank(layer) <= rank(timeline) {
                return false;
            }
            let Some(index) = self.ui.dock.floating.iter().position(|window| window.contains(panel)) else { return false };
            let grip = format!("panel.float.{index}.move");
            let window = hits.iter().filter(|hit| hit.id == grip && hit.viewport == egui::ViewportId::ROOT).fold(*rect, |area, hit| area.union(hit.rect));
            window.contains(pos)
        })
    }

    /// Whether a modal dialog is open: Settings, Recovery or Revert (egui `Modal`s), any other
    /// egui `Modal` shown last frame, the Save Project prompt of Quit, Close Project or Open
    /// Project, or a native file dialog. egui's `Modal` blocks input only in the main window, so
    /// the panels in native windows are disabled meanwhile.
    fn modal_open(&self, ctx: &egui::Context) -> bool {
        matches!(self.dialog, Some(crate::Dialog::Preferences | crate::Dialog::Recovery | crate::Dialog::RevertConfirm))
            || self.ui.clip_dialog.as_ref().is_some_and(|dialog| matches!(dialog.command.as_str(), "app.quit" | "file.closeProject" | "file.open"))
            || self.native_dialog_open()
            || ctx.memory(|memory| memory.top_modal_layer().is_some())
    }

    pub(crate) fn dock_area(&mut self, ui: &mut egui::Ui, body: Rect) {
        let t = self.tokens;
        let ctx = ui.ctx().clone();
        let blocked = self.modal_open(&ctx);
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
        let timeline_title = seqs.open.is_empty().then(|| seqs.label());
        if self.dock_specs.timeline_title != timeline_title {
            self.dock_specs.edit(|specs| specs.timeline_title = timeline_title);
        }
        self.sync_live();
        // The app moved its panel focus itself since the last pass (Source after `source.open`, a
        // panel a shortcut or menu focused) while key focus is in another panel's group: that key
        // focus ends, as in production, where a click on a panel left no widget focused. A focused
        // tab holds Space and the other keys it acts on, so they would not reach the panel now focused.
        let app_focus = ctx.data(|data| data.get_temp::<PanelKind>(app_focus_id()));
        let key_group = ctx.data(|data| data.get_temp::<Option<PanelKind>>(key_focus_id())).flatten();
        if app_focus.is_some_and(|last| last != self.ui.focused)
            && key_group.is_some_and(|group| group != self.ui.focused)
            && let Some(id) = ctx.memory(|memory| memory.focused())
        {
            ctx.memory_mut(|memory| memory.surrender_focus(id));
        }
        // Known before anything draws, so header elements of a native window (the ≡ menu, the
        // Timeline's sequence tabs), drawn before its bodies, carry that window too.
        self.auto.windows = self
            .dock_windows
            .iter()
            .filter_map(|(panel, viewport)| self.ui.dock.floating.iter().position(|window| window.contains(panel)).map(|index| (*viewport, index)))
            .collect();
        let maximized = self.ui.keys.maximized.filter(|panel| self.ui.dock.is_docked(*panel));
        let seq_width = dock::sequence_tabs_width(ui, &seqs);
        let workspace = Arc::clone(&self.dock_live);
        let specs = std::mem::replace(&mut self.dock_specs, Tracked::new(FilmSpecs::default()));
        // FilmCraft's window is all groups: drops on all four window edges, Ctrl/Cmd-drag undocks,
        // a release over no target cancels, and a floating window's title bar only moves it.
        let config = DockConfig::default()
            .side_edge_zones(true)
            .new_dock_zones(false)
            .maximize_item(true)
            .close_others_item(true)
            .float_on_empty_drop(false)
            .title_bar_docks(false)
            .wheel_selects_tabs(true);
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt("filmcraft-dock-area"));
        child.set_clip_rect(body.intersect(ui.clip_rect()));
        // One request per bring-forward: a later pass does not take the stacking back.
        let focus = self.dock_focus.take();
        let mut panels = FilmPanels {
            app: self,
            seqs,
            seq_width,
            tokens: t,
            seq_actions: Vec::new(),
            menu: None,
            seq_menu_viewport: egui::ViewportId::ROOT,
            bodies: Vec::new(),
            menu_windows: Vec::new(),
            pressed: None,
            blocked,
        };
        let mut area = DockArea::new("Panels", Bind::given(workspace), &specs, &mut panels, config);
        if let Some(panel) = maximized {
            area = area.maximized(panel);
        }
        if let Some(panel) = focus {
            area = area.focus(panel);
        }
        let output = area.show(&mut child);
        let FilmPanels { seq_actions, mut menu, seq_menu_viewport, bodies, pressed, .. } = panels;
        self.dock_specs = specs;
        ctx.data_mut(|data| data.insert_temp(bodies_id(), bodies));
        self.dock_windows = output.windows;
        self.register_dock_sources(&ctx, &output.sources);
        if output.float_space.is_some() {
            self.dock_float_space = output.float_space;
        }
        // next reports the group holding key focus every pass; only a move of that focus to another
        // group focuses its panel, so a panel the app focused itself (Source after `source.open`,
        // a reopened panel) keeps the focus while key focus stays where it was.
        let last_focused = ctx.data(|data| data.get_temp::<Option<PanelKind>>(key_focus_id())).flatten();
        ctx.data_mut(|data| data.insert_temp(key_focus_id(), output.focused));
        let focus_moved = output.focused.filter(|panel| last_focused != Some(*panel));
        if let Some(panel) = pressed.or(output.picked).or(focus_moved) {
            self.ui.focused = panel;
            if panel == PanelKind::Timeline {
                self.raise_program_for_timeline();
            }
        }
        // The fallback delivers a drop once. Keep it until root input drains it, even if
        // another layout pass publishes an empty output before the next root frame.
        self.window_files.extend(output.window_files);
        if !self.window_files.is_empty() {
            ctx.request_repaint_of(egui::ViewportId::ROOT);
        }
        if let Some(error) = output.error {
            self.ui.status = error.error.to_string();
        }
        for event in output.events {
            match event {
                DockEvent::ToggleMaximize { panel, .. } => {
                    panels::keyboard::toggle_maximize(self, panel);
                }
                // FilmCraft asks for no destination: a panel or group moves by its tab or bar.
                DockEvent::PanelCommand { .. } => self.ui.status = tl!("Drag the tab to move the panel").into(),
                _ => {}
            }
        }
        // The area's actions include the picks its focus request and the user's tab clicks made,
        // already in front: they ask for no new focus, or the request would repeat every pass.
        // A body's requests are the app's own and do.
        let focus = self.dock_focus;
        let taken = output.actions.len();
        for (index, action) in output.actions.into_iter().chain(output.requested).enumerate() {
            for action in self.close_keeping_timeline(action) {
                if let Err(error) = self.dock_command(&ctx, action) {
                    self.ui.status = error;
                }
            }
            if index + 1 == taken {
                self.dock_focus = focus;
            }
        }
        for action in seq_actions {
            let result = match action {
                SeqAction::Open(id) => {
                    let _ = self.apply_dock(Action::activate(PanelKind::Timeline));
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
                .is_some_and(|(_, _, owner)| owner != egui::ViewportId::ROOT && !self.dock_windows.iter().any(|(_, viewport)| *viewport == owner));
            if gone {
                data.remove::<(PanelKind, egui::Pos2, egui::ViewportId)>(id);
            }
            data.insert_temp(app_focus_id(), self.ui.focused);
        });
        panels::panel_menu_popup(self, ui);
    }

    /// Automation ids for the dock area's gesture sources: `panel.tab.<Panel>`, `panel.drag.<Panel>`
    /// (the group's drag bar), `panel.float.<n>.move` and `.resize` (`.resize.<Edge>` for the other
    /// grips), `dock.gutter.<path>` (`a`/`b` per split level), and next's column dividers as
    /// `dock.groups.<Panel>` and `dock.columnEdge.<Panel>`. A source in a floating window's own OS
    /// window is registered in that window. The source's node id is the element's widget id, so
    /// key focus on a divider or grip maps to its element; [`divider_focused`] reads it.
    fn register_dock_sources(&mut self, ctx: &egui::Context, sources: &[DockSource<PanelKind>]) {
        // The floating windows the area just showed in their own OS windows.
        self.auto.windows = self
            .dock_windows
            .iter()
            .filter_map(|(panel, viewport)| self.ui.dock.floating.iter().position(|window| window.contains(panel)).map(|index| (*viewport, index)))
            .collect();
        let window = |panel: &PanelKind| self.ui.dock.floating.iter().position(|window| window.contains(panel));
        let mut found = Vec::new();
        for source in sources {
            let (id, label, grip) = match &source.kind {
                SourceKind::Tab(panel) => (format!("panel.tab.{}", panel.id()), panel.title(), false),
                SourceKind::GroupBar(panel) => (format!("panel.drag.{}", panel.id()), "Move panel", false),
                SourceKind::WindowGrip(panel) => match window(panel) {
                    Some(index) => (format!("panel.float.{index}.move"), "Move floating panel", false),
                    None => continue,
                },
                SourceKind::WindowResize(panel, edge) => match window(panel) {
                    Some(index) if *edge == ResizeEdge::BottomRight => (format!("panel.float.{index}.resize"), "Resize floating panel", true),
                    Some(index) => (format!("panel.float.{index}.resize.{edge:?}"), "Resize floating panel", true),
                    None => continue,
                },
                SourceKind::Divider(DividerKind::Split(path)) => (format!("dock.gutter.{}", path_id(path)), "gutter", true),
                SourceKind::Divider(DividerKind::Groups(panel)) => (format!("dock.groups.{}", panel.id()), "gutter", true),
                SourceKind::Divider(DividerKind::ColumnEdge(panel)) => (format!("dock.columnEdge.{}", panel.id()), "gutter", true),
                _ => continue,
            };
            found.push((DockHit { id, rect: source.rect, widget: source.id, viewport: source.viewport, grip }, label));
        }
        for (hit, label) in &found {
            self.auto.add_in(hit.viewport, &hit.id, hit.rect, label);
            if let Some(element) = self.auto.elements.last_mut() {
                element.widget_id = Some(format!("{:016x}", hit.widget.value()));
                element.response_id = Some(hit.widget);
            }
        }
        let hits: Vec<DockHit> = found.into_iter().map(|(hit, _)| hit).collect();
        ctx.data_mut(|data| data.insert_temp(sources_id(), hits));
    }

    /// The Timeline's tabs stand for its open sequences: a close from the dock area (Close Panel,
    /// a group, window or Close Other Panels item) that would remove the Timeline while a sequence
    /// is open closes the active sequence instead, and the Timeline stays. The other panels the
    /// action covers still close.
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
        closed.into_iter().filter(|panel| *panel != PanelKind::Timeline).map(Action::close).collect()
    }
}

/// FilmCraft's side of the dock area for one pass.
struct FilmPanels<'a> {
    app: &'a mut FilmcraftApp,
    seqs: SeqTabs,
    /// The width the sequence tabs take in the Timeline's group header.
    seq_width: f32,
    tokens: Tokens,
    seq_actions: Vec<SeqAction>,
    /// The ≡ button clicked: its panel and where its menu opens.
    menu: Option<(PanelKind, egui::Pos2, egui::ViewportId)>,
    /// The viewport of the Timeline header whose sequence tab menu was opened this pass.
    seq_menu_viewport: egui::ViewportId,
    bodies: Vec<(PanelKind, Rect, egui::LayerId)>,
    /// The floating OS windows whose ≡ menu was drawn this pass.
    menu_windows: Vec<egui::ViewportId>,
    /// The panel whose body a pointer press landed in this pass.
    pressed: Option<PanelKind>,
    /// A modal dialog is open: the panels in native windows take no input.
    blocked: bool,
}

impl FilmPanels<'_> {
    /// While a modal is open, `ui` in a native window is disabled: egui's `Modal` blocks only the
    /// main window.
    fn block_native(&self, ui: &mut egui::Ui) {
        if self.blocked && ui.ctx().viewport_id() != egui::ViewportId::ROOT {
            ui.disable();
        }
    }
}

impl DockPanels<PanelKind> for FilmPanels<'_> {
    fn body(&mut self, panel: &PanelKind, ui: &mut egui::Ui, cx: &mut BodyCx<'_>) {
        crate::framework::app_style(ui);
        let t = self.tokens;
        let rect = ui.max_rect();
        // The dock area also measures a group in an invisible sizing pass of the same frame. The
        // body fills its frame either way; only the shown pass draws the panel, registers its
        // automation elements and acts on input.
        if ui.is_sizing_pass() {
            ui.allocate_rect(rect, Sense::hover());
            return;
        }
        // Every panel sits on FilmCraft's own panel background in each theme (`Tokens::panel_bg`,
        // the dock background production gave its bodies), under the panel's content; the dock's
        // frame, tabs and header around it stay the dock's.
        ui.painter().rect_filled(rect.union(cx.rect), 0.0, t.panel_bg);
        self.block_native(ui);
        self.bodies.push((*panel, rect, ui.layer_id()));
        // A panel in its own OS window draws in that window's coordinates: its automation
        // elements carry the window so they are not matched against main-window points.
        let viewport = ui.ctx().viewport_id();
        if viewport != egui::ViewportId::ROOT && !self.menu_windows.contains(&viewport) {
            self.menu_windows.push(viewport);
            panels::panel_menu_area(self.app, ui);
        }
        self.app.auto.viewport = viewport;
        self.app.auto.add(&format!("panel.{}", panel.id()), rect, panel.title());
        if ui.input(|i| i.pointer.any_pressed()) && ui.rect_contains_pointer(rect) {
            self.pressed = Some(*panel);
        }
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(rect).id_salt(("panel", panel.id())));
        child.set_clip_rect(rect.intersect(ui.clip_rect()));
        // The Timeline is FilmCraft's document view: Tab and the edit chords stay in egui for it.
        if *panel == PanelKind::Timeline {
            crate::framework::host_canvas(ui.ctx(), child.scope_id());
            if viewport != egui::ViewportId::ROOT {
                self.app.native_timeline_file_drop(&child, rect);
            }
        }
        panels::show(self.app, &mut child, *panel, rect);
        // The end-of-pass IME check of the Program monitor's text editor in its own OS window.
        if *panel == PanelKind::Program && viewport != egui::ViewportId::ROOT {
            self.app.finish_ime_window(ui.ctx());
        }
        self.app.auto.viewport = egui::ViewportId::ROOT;
        // The body fills its frame; allocating it lets a floated panel take its full size.
        ui.allocate_rect(rect, Sense::hover());
        // Painted after the body so child backgrounds cannot hide the focus outline (#1075).
        if self.app.ui.focused == *panel {
            ui.painter().rect_stroke(rect, 0.0, Stroke::new(1.0, t.focus), StrokeKind::Inside);
        }
    }

    /// The Timeline's sequence tabs while the Timeline is shown, then the ≡ panel menu of the
    /// group's shown panel. The sequence tabs take at most two thirds of the room, so the group's
    /// own tabs keep some; past that they scroll into their » list.
    fn group_header(&mut self, panels: &[PanelKind], active: usize, ui: &mut egui::Ui, _cx: &mut BodyCx<'_>) {
        let Some(active) = panels.get(active).copied() else { return };
        crate::framework::app_style(ui);
        let t = self.tokens;
        let sequences = active == PanelKind::Timeline && !self.seqs.open.is_empty();
        let width = if sequences { (self.seq_width + MENU_W).min(ui.available_width() * 2.0 / 3.0).max(MENU_W) } else { MENU_W };
        let (header, _) = ui.allocate_exact_size(vec2(width, t.tab_h), Sense::hover());
        // A sizing pass only measures the header: its widgets and their elements are the shown pass's.
        if ui.is_sizing_pass() {
            return;
        }
        self.block_native(ui);
        // A press on the header's own widgets (a sequence tab, the ≡ button) is no press on the
        // group's surface in next, so the group does not take the focus: the app focuses the
        // group's shown panel itself, as a press anywhere in the group did in production.
        if ui.input(|i| i.pointer.any_pressed()) && ui.rect_contains_pointer(header) {
            self.pressed = Some(active);
        }
        let menu = Rect::from_min_max(egui::pos2((header.max.x - MENU_W).max(header.min.x), header.min.y), header.max);
        if sequences {
            let strip = Rect::from_min_max(header.min, egui::pos2(menu.min.x, header.max.y));
            let actions = dock::sequence_tabs(ui, strip, &self.seqs, &t, &mut self.app.auto);
            if actions.iter().any(|a| matches!(a, SeqAction::Menu(..))) {
                self.seq_menu_viewport = ui.ctx().viewport_id();
            }
            self.seq_actions.extend(actions);
        }
        let rect = Rect::from_center_size(menu.center(), vec2(18.0, 18.0));
        let response = ui.interact(rect, egui::Id::unique(("panel-menu-button", active.id())), Sense::click());
        response.widget_info(|| egui::WidgetInfo::labeled(egui::Role::Button, response.enabled(), "panel menu"));
        self.app.auto.add_response(&format!("panel.menu.{}", active.id()), &response, "panel menu");
        let color = if response.hovered() { t.tab_text_active } else { t.tab_text };
        icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(16.0, 16.0)), Icon::Hamburger, color);
        dock::paint_widget_focus(ui.painter(), &response, &t);
        if response.clicked() {
            self.app.ui.focused = active;
            self.menu = Some((active, rect.left_bottom(), ui.ctx().viewport_id()));
        }
    }
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
    use craft_behavior::dock::{Action, DockEdge, Node, SplitSize, Target};
    use craft_behavior::flow::Axis;

    use super::tab_group;
    use crate::dock::{Layout, PanelKind};

    /// The Timeline beside a dock column holding Properties and Program, that group minimized or
    /// the column collapsed to icons as asked.
    fn beside_dock(minimized: bool, iconic: bool) -> Layout {
        let group = Node::tabs(vec![PanelKind::Properties, PanelKind::Program], 0);
        let root = Node::split(Axis::X, SplitSize::Ratio(0.5), Node::tabs(vec![PanelKind::Timeline], 0), group);
        let mut layout = Layout::new(Some(root), Vec::new());
        layout.apply(Action::move_group(PanelKind::Program, Target::new_dock(DockEdge::End))).unwrap();
        if minimized {
            layout.apply(Action::set_minimized(PanelKind::Program, true)).unwrap();
        }
        if iconic {
            layout.apply(Action::set_iconic(PanelKind::Program, true)).unwrap();
        }
        layout
    }

    /// #3653: Program is fronted only from an expanded group that shows one tab at a time, so a
    /// Timeline click never expands what the user collapsed.
    #[test]
    fn program_is_fronted_only_from_an_expanded_tab_group() {
        assert_eq!(tab_group(&beside_dock(false, false), PanelKind::Program), Some(vec![PanelKind::Properties, PanelKind::Program]));
        assert_eq!(tab_group(&beside_dock(true, false), PanelKind::Program), None, "a minimized group");
        assert_eq!(tab_group(&beside_dock(false, true), PanelKind::Program), None, "an icon-only column");
        let accordion: Layout = serde_json::from_value(serde_json::json!({"root": {"Stack": {"entries": [
            {"panel": "Properties", "open": false, "height": null}, {"panel": "Program", "open": false, "height": null}]}}, "floating": []}))
        .unwrap();
        assert_eq!(tab_group(&accordion, PanelKind::Program), None, "an accordion");
    }
}

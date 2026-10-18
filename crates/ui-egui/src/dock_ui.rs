//! FilmCraft's panel chrome and document tabs over the shared docking transactions.

use craft_ui::docking::{Action, DropTarget, Placement, Zone, resolve_drop};
use egui::{Color32, Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde_json::{Value, json};

use crate::dock::{self, DockAction, DockNode, Group, PanelKind};
use crate::{FilmcraftApp, panels};

enum NativeFloatingAction {
    None,
    CloseGroup(Vec<PanelKind>),
    DockGroup(Vec<PanelKind>),
    ClosePanel(PanelKind),
    ToggleMaximize(PanelKind),
}

/// Single source for rendering and hit testing: saved offscreen positions remain
/// reachable, and every panel has enough room for its chrome.
fn floating_rect(group: &dock::Floating, screen: Rect) -> Rect {
    let rect = Rect::from_min_size(pos2(group.rect[0], group.rect[1]), vec2(group.rect[2].max(160.0), group.rect[3].max(100.0)));
    rect.translate(vec2(
        (screen.min.x - rect.min.x).max(0.0) + (screen.max.x - 40.0 - rect.min.x).min(0.0),
        (screen.min.y - rect.min.y).max(0.0) + (screen.max.y - 30.0 - rect.min.y).min(0.0),
    ))
}

fn floating_viewport_id(owner: PanelKind) -> egui::ViewportId {
    egui::ViewportId::from_hash_of(("filmcraft-floating-panel", owner.id()))
}

fn floating_viewport_open_id(owner: PanelKind) -> egui::Id {
    egui::Id::new(("filmcraft-floating-panel-open", owner.id()))
}

fn bounded_inner_size(value: f32, minimum: f32) -> f32 {
    if value.is_finite() { value.clamp(minimum, 16_384.0) } else { minimum }
}

fn native_viewport_position(group: &dock::Floating, parent_outer: Option<Rect>) -> egui::Pos2 {
    let origin = parent_outer.map_or(egui::Pos2::ZERO, |rect| rect.min);
    origin + vec2(group.rect[0], group.rect[1])
}

fn saved_native_rect(group: &dock::Floating, parent_outer: Option<Rect>, child_outer: Option<Rect>, child_inner: Option<Rect>) -> [f32; 4] {
    let (Some(outer), Some(inner)) = (child_outer, child_inner) else { return group.rect };
    if !outer.min.x.is_finite() || !outer.min.y.is_finite() || !inner.width().is_finite() || !inner.height().is_finite() {
        return group.rect;
    }
    let origin = parent_outer.map_or(egui::Pos2::ZERO, |rect| rect.min);
    [
        (outer.min.x - origin.x).clamp(-100_000.0, 100_000.0),
        (outer.min.y - origin.y).clamp(-100_000.0, 100_000.0),
        bounded_inner_size(inner.width(), 240.0),
        bounded_inner_size(inner.height(), 160.0),
    ]
}

fn drag_id() -> egui::Id {
    egui::Id::new("dock-panel-drag")
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
    if !matches!(id, "window.dockPanel" | "window.floatPanel" | "window.closePanel") {
        return None;
    }
    Some((|| {
        let panel = match params.get("panel") {
            None => app.ui.focused,
            Some(value) => value.as_str().and_then(PanelKind::from_name).ok_or("unknown panel")?,
        };
        let action = match id {
            "window.closePanel" => Action::Close { panel },
            "window.floatPanel" => {
                let rect = match params.get("rect") {
                    None => default_float_rect(ctx.content_rect()),
                    Some(value) => serde_json::from_value::<[f32; 4]>(value.clone()).map_err(|_| "rect must contain four finite numbers")?,
                };
                Action::Float { panel, rect }
            }
            _ => {
                let zone_name = match params.get("zone") {
                    None => "center",
                    Some(value) => value.as_str().ok_or("zone must be a string")?,
                };
                let zone = match zone_name.to_ascii_lowercase().as_str() {
                    "left" => Zone::Left,
                    "right" => Zone::Right,
                    "top" => Zone::Top,
                    "bottom" => Zone::Bottom,
                    "center" | "tab" => Zone::Center,
                    _ => return Err("unknown docking zone".into()),
                };
                let Some(anchor) = params.get("anchor") else {
                    if params.get("before").is_some() {
                        return Err("before needs an anchor panel".into());
                    }
                    app.dock_to_edge(panel, zone)?;
                    return Ok(json!({"panel": panel.id()}));
                };
                let anchor = anchor.as_str().and_then(PanelKind::from_name).ok_or("a known anchor panel is required")?;
                let placement = match params.get("before") {
                    Some(value) => Placement::Tab { before: Some(value.as_str().and_then(PanelKind::from_name).ok_or("unknown before panel")?) },
                    None => Placement::Split(zone),
                };
                Action::Move { panel, anchor, placement }
            }
        };
        app.apply_dock(action)?;
        Ok(json!({"panel": panel.id()}))
    })())
}

pub(crate) fn default_float_rect(screen: Rect) -> [f32; 4] {
    let center = screen.center();
    [center.x - 210.0, center.y - 160.0, 420.0, 320.0]
}

impl FilmcraftApp {
    fn dock_command(&mut self, ctx: &egui::Context, action: Action<PanelKind>) -> Result<(), String> {
        let value = serde_json::to_value(action).map_err(|error| error.to_string())?;
        crate::menus::invoke(self, ctx, "window.dockAction", json!({"action": value})).map(|_| ())
    }

    pub fn apply_dock(&mut self, action: Action<PanelKind>) -> Result<(), String> {
        let relocating = matches!(&action, Action::Move { .. } | Action::Float { .. });
        let reveal = match &action {
            Action::Activate { panel } => Some(*panel),
            _ => None,
        };
        self.commit_dock(|shared| {
            crate::dock_shared::check_last_docked(shared, &action)?;
            shared.apply(action).map_err(|error| error.to_string())
        })?;
        if let Some(panel) = reveal {
            self.pending_panel_raise = Some(panel);
        }
        if relocating
            || self.ui.keys.maximized.is_some_and(|panel| !self.ui.dock.contains(panel) && !self.ui.floating.iter().any(|group| group.panels.contains(&panel)))
        {
            self.ui.keys.maximized = None;
        }
        Ok(())
    }

    /// Dock a panel along an edge of the whole workspace rather than beside one group.
    pub fn dock_to_edge(&mut self, panel: PanelKind, zone: Zone) -> Result<(), String> {
        self.commit_dock(|shared| crate::dock_shared::dock_at_edge(shared, panel, zone))?;
        self.ui.keys.maximized = None;
        Ok(())
    }

    fn commit_dock(&mut self, change: impl FnOnce(&mut craft_ui::docking::Layout<PanelKind>) -> Result<(), String>) -> Result<(), String> {
        let mut shared = crate::dock_shared::encode(&self.ui.dock, &self.ui.floating)?;
        shared.validate().map_err(|error| error.to_string())?;
        change(&mut shared)?;
        let layout = crate::dock_shared::decode(shared)?;
        crate::dock_shared::encode(&layout.root, &layout.floating)?;
        self.ui.dock = layout.root;
        self.ui.floating = layout.floating;
        Ok(())
    }

    fn panel_body(&mut self, ui: &mut egui::Ui, panel: PanelKind, content: Rect) {
        self.auto.add(&format!("panel.{}", panel.id()), content, panel.title());
        let mut child = ui.new_child(egui::UiBuilder::new().max_rect(content).id_salt(("panel", panel.id())));
        child.set_clip_rect(content.intersect(ui.clip_rect()));
        panels::show(self, &mut child, panel, content);
    }

    /// OS drag positions come from the host cursor hook, so egui pointer capture alone
    /// cannot decide whether a Timeline is covered. Use the same geometry and layer
    /// ordering as our renderer before any panel can dispatch an edit.
    pub(crate) fn timeline_drop_target(&self, ctx: &egui::Context, pos: egui::Pos2) -> bool {
        if !ctx.content_rect().contains(pos) {
            return false;
        }
        if let Some(panel) =
            self.ui.keys.maximized.filter(|panel| self.ui.dock.contains(*panel) || self.ui.floating.iter().any(|group| group.panels.contains(panel)))
        {
            return panel == PanelKind::Timeline;
        }
        for index in self.floating_order(ctx).into_iter().rev() {
            if let Some(group) = self.ui.floating.get(index)
                && floating_rect(group, ctx.content_rect()).contains(pos)
            {
                return group.panels.get(group.active) == Some(&PanelKind::Timeline);
            }
        }
        true
    }

    fn floating_order(&self, ctx: &egui::Context) -> Vec<usize> {
        let layers = ctx.memory(|memory| memory.layer_ids().collect::<Vec<_>>());
        let mut order: Vec<_> = (0..self.ui.floating.len()).collect();
        order.sort_by_key(|index| {
            self.ui
                .floating
                .get(*index)
                .and_then(|group| group.panels.first())
                .and_then(|panel| {
                    let id = egui::Id::new(("floating-panel", panel.id()));
                    layers.iter().position(|layer| layer.id == id)
                })
                .unwrap_or(usize::MAX)
        });
        order
    }

    pub(crate) fn dock_area(&mut self, ui: &mut egui::Ui, body: Rect) {
        let t = self.tokens;
        let ctx = ui.ctx().clone();
        if ctx.embed_viewports()
            && let Some(panel) = self.pending_panel_raise.take()
            && let Some(owner) = self.ui.floating.iter().find(|group| group.panels.contains(&panel)).and_then(|group| group.panels.first())
        {
            ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, egui::Id::new(("floating-panel", owner.id()))));
        }
        let maximized =
            self.ui.keys.maximized.filter(|panel| self.ui.dock.contains(*panel) || self.ui.floating.iter().any(|group| group.panels.contains(panel)));
        let mut dock = match maximized {
            Some(panel) => DockNode::Tabs { panels: vec![panel], active: 0 },
            None => std::mem::replace(&mut self.ui.dock, DockNode::Tabs { panels: vec![], active: 0 }),
        };
        let mut groups = vec![];
        let mut layout_actions = Vec::new();
        dock::layout_with_actions(ui, &mut dock, body, &t, "", &mut groups, &mut self.auto, &mut layout_actions);
        let seqs = dock::SeqTabs {
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
        let mut actions = vec![];
        let mut targets = vec![];
        for group in &groups {
            let chrome = dock::draw_group_chrome_details(ui, group, &seqs, &t, &mut self.auto);
            if let Some(target) = target(group, chrome.tabs, true) {
                targets.push(target);
            }
            actions.extend(chrome.actions);
        }
        if maximized.is_none() {
            self.ui.dock = dock;
        }
        for group in &groups {
            if let Some(panel) = group.panels.get(group.active).copied() {
                self.panel_body(ui, panel, group.content);
            }
        }
        for group in &groups {
            dock::paint_group_focus(ui, group, self.ui.focused, &t);
        }
        if ctx.embed_viewports() {
            let floating_order = self.floating_order(&ctx);
            for index in floating_order.into_iter().filter(|_| maximized.is_none()) {
                if let Some(target) = self.floating_group(&ctx, index, &seqs, &mut actions) {
                    targets.push(target);
                }
            }
        }
        for action in layout_actions {
            let _ = self.dock_command(&ctx, action);
        }
        for action in actions {
            match action {
                DockAction::Activate(panel) => {
                    let _ = self.dock_command(&ctx, Action::Activate { panel });
                }
                DockAction::Close(panel) => {
                    let _ = self.dock_command(&ctx, Action::Close { panel });
                }
                DockAction::Focus(panel) => self.ui.focused = panel,
                DockAction::BeginDrag(panel) => {
                    ctx.data_mut(|data| data.insert_temp(drag_id(), panel));
                }
                DockAction::OpenSequence(id) => {
                    let _ = self.dock_command(&ctx, Action::Activate { panel: PanelKind::Timeline });
                    if self.session.state.active_sequence.map(|item| item.0) != Some(id)
                        && let Err(error) = self.session.execute("sequence.open", json!({"item": id}))
                    {
                        self.ui.status = error.to_string();
                    }
                }
                DockAction::CloseSequence(id) => {
                    if let Err(error) = self.session.execute("sequence.close", json!({"item": id})) {
                        self.ui.status = error.to_string();
                    }
                }
                DockAction::MoveSequence(id, index) => {
                    if let Err(error) = self.session.execute("sequence.moveTab", json!({"item": id, "index": index})) {
                        self.ui.status = error.to_string();
                    }
                }
                DockAction::PanelMenu(panel, pos) => {
                    let frame = ctx.cumulative_frame_nr();
                    ctx.data_mut(|data| {
                        data.insert_temp(egui::Id::new("panel-menu"), (panel, pos));
                        data.insert_temp(egui::Id::new("panel-menu-opened"), frame);
                    });
                }
            }
        }
        self.panel_drag(&ctx, body, &targets);
        panels::panel_menu_popup(self, ui);
        self.cancel_value_drag(ui.ctx());
    }

    /// Native floating panels use independent OS windows on desktop. Backends that embed egui
    /// viewports continue to use the in-window areas below.
    pub(crate) fn native_floating_viewports(&mut self, ctx: &egui::Context) {
        if ctx.embed_viewports() {
            return;
        }

        if let Some(panel) = self.pending_panel_raise.take()
            && let Some(owner) = self.ui.floating.iter().find(|group| group.panels.contains(&panel)).and_then(|group| group.panels.first())
        {
            ctx.send_viewport_cmd_to(floating_viewport_id(*owner), egui::ViewportCommand::Focus);
        }

        let owners: Vec<_> = self.ui.floating.iter().filter_map(|group| group.panels.first().copied()).collect();
        ctx.data_mut(|data| {
            for panel in PanelKind::ALL {
                if !owners.contains(&panel) {
                    data.remove::<bool>(floating_viewport_open_id(panel));
                }
            }
        });

        let maximized = self.ui.keys.maximized;
        let mut actions = Vec::new();
        for index in 0..self.ui.floating.len() {
            let hidden = maximized.is_some_and(|panel| self.ui.floating.get(index).is_some_and(|group| group.panels.contains(&panel)));
            if let Some(action) = self.native_floating_group(ctx, index, hidden) {
                actions.push(action);
            }
        }

        for action in actions {
            match action {
                NativeFloatingAction::None => {}
                NativeFloatingAction::CloseGroup(panels) => {
                    for panel in panels {
                        if let Err(error) = self.dock_command(ctx, Action::Close { panel }) {
                            self.ui.status = error;
                        }
                    }
                }
                NativeFloatingAction::DockGroup(panels) => {
                    let mut docked = Vec::new();
                    self.ui.dock.panels(&mut docked);
                    let Some(anchor) = docked.first().copied() else {
                        self.ui.status = "There is no docked panel to receive this group".into();
                        continue;
                    };
                    for panel in panels.into_iter().rev() {
                        if let Err(error) =
                            self.dock_command(ctx, Action::Move { panel, anchor, placement: craft_ui::docking::Placement::Tab { before: None } })
                        {
                            self.ui.status = error;
                            break;
                        }
                    }
                    self.ui.focused = anchor;
                }
                NativeFloatingAction::ClosePanel(panel) => {
                    if let Err(error) = self.dock_command(ctx, Action::Close { panel }) {
                        self.ui.status = error;
                    }
                }
                NativeFloatingAction::ToggleMaximize(panel) => {
                    crate::panels::keyboard::toggle_maximize(self, panel);
                }
            }
        }
    }

    fn native_floating_group(&mut self, ctx: &egui::Context, index: usize, hidden: bool) -> Option<NativeFloatingAction> {
        let floating = self.ui.floating.get(index)?.clone();
        let owner = floating.panels.first().copied()?;
        let active_index = floating.active.min(floating.panels.len().saturating_sub(1));
        let active_panel = floating.panels.get(active_index).copied()?;
        let parent_outer = ctx.input(|input| input.viewport().outer_rect);
        let viewport_id = floating_viewport_id(owner);
        let open_id = floating_viewport_open_id(owner);
        let already_open = ctx.data(|data| data.get_temp::<bool>(open_id)).unwrap_or(false);
        let mut builder = egui::ViewportBuilder::default()
            .with_title(format!("FilmCraft — {}", crate::i18n::t(active_panel.title())))
            .with_decorations(true)
            .with_resizable(true)
            .with_min_inner_size([420.0, 220.0])
            .with_drag_and_drop(true)
            .with_visible(!hidden);
        if !already_open {
            builder = builder
                .with_position(native_viewport_position(&floating, parent_outer))
                .with_inner_size([bounded_inner_size(floating.rect[2], 420.0), bounded_inner_size(floating.rect[3], 220.0)])
                .with_active(!hidden);
            ctx.data_mut(|data| data.insert_temp(open_id, true));
        }

        let mut child_outer = None;
        let mut child_inner = None;
        let mut close_requested = false;
        let mut requested_action = None;
        ctx.show_viewport_immediate(viewport_id, builder, |ui, _class| {
            close_requested = ui.input(|input| input.viewport().close_requested());
            child_outer = ui.input(|input| input.viewport().outer_rect);
            child_inner = ui.input(|input| input.viewport().inner_rect);
            if close_requested || hidden {
                return;
            }

            let viewport_rect = ui.max_rect();
            let toolbar_height = self.tokens.tab_h.max(32.0);
            let toolbar_rect = Rect::from_min_size(viewport_rect.min, vec2(viewport_rect.width(), toolbar_height));
            ui.painter().rect_filled(viewport_rect, self.tokens.radius, self.tokens.panel_bg);
            let mut toolbar = ui.new_child(egui::UiBuilder::new().max_rect(toolbar_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
            for (tab, panel) in floating.panels.iter().copied().enumerate() {
                let response = toolbar.selectable_label(tab == active_index, crate::i18n::t(panel.title()));
                self.auto.add_response(&format!("panel.tab.{}", panel.id()), &response, panel.title());
                if response.clicked() {
                    if let Some(group) = self.ui.floating.get_mut(index) {
                        group.active = tab;
                    }
                    self.ui.focused = panel;
                }
            }
            let dock = toolbar.button(tl!("Dock Panel"));
            self.auto.add_response(&format!("panel.float.{}.dock", owner.id()), &dock, "Dock Panel");
            if dock.clicked() {
                requested_action = Some(NativeFloatingAction::DockGroup(floating.panels.clone()));
            }
            let close = toolbar.button(tl!("Close Panel"));
            self.auto.add_response(&format!("panel.float.{}.close", owner.id()), &close, "Close Panel");
            if close.clicked() {
                requested_action = Some(NativeFloatingAction::ClosePanel(active_panel));
            }
            let maximized = self.ui.keys.maximized == Some(active_panel);
            let maximize = toolbar.button(if maximized { tl!("Restore Frame") } else { tl!("Maximize Frame") });
            self.auto.add_response(&format!("panel.float.{}.maximize", owner.id()), &maximize, if maximized { "Restore Frame" } else { "Maximize Frame" });
            if maximize.clicked() {
                requested_action = Some(NativeFloatingAction::ToggleMaximize(active_panel));
            }

            let content = Rect::from_min_max(pos2(viewport_rect.min.x, toolbar_rect.max.y), viewport_rect.max);
            self.panel_body(ui, active_panel, content);
        });

        if close_requested {
            ctx.data_mut(|data| data.remove::<bool>(open_id));
            return Some(NativeFloatingAction::CloseGroup(floating.panels));
        }
        if let (Some(outer), Some(inner)) = (child_outer, child_inner)
            && let Some(group) = self.ui.floating.get_mut(index)
        {
            group.rect = saved_native_rect(&floating, parent_outer, Some(outer), Some(inner));
        }
        requested_action.or(Some(NativeFloatingAction::None))
    }

    fn floating_group(&mut self, ctx: &egui::Context, index: usize, seqs: &dock::SeqTabs, actions: &mut Vec<DockAction>) -> Option<DropTarget<PanelKind>> {
        let floating = self.ui.floating.get(index)?.clone();
        let panel = *floating.panels.first()?;
        let rect = floating_rect(&floating, ctx.content_rect());
        let t = self.tokens;
        let id = egui::Id::new(("floating-panel", panel.id()));
        let mut output = None;
        egui::Area::new(id).order(egui::Order::Middle).fixed_pos(rect.min).constrain(false).interactable(true).show(ctx, |ui| {
            let (rect, _) = ui.allocate_exact_size(rect.size(), Sense::hover());
            let compact = floating.panels.len() == 1 && panel.compact();
            let header = if compact { 8.0 } else { t.tab_h };
            let strip = Rect::from_min_size(rect.min, vec2(rect.width(), header));
            let response = ui.interact(strip, id.with("move"), Sense::drag());
            self.auto.add_response(&format!("panel.float.{index}.move"), &response, "Move floating panel");
            let mut next = if response.dragged() { rect.translate(response.drag_delta()) } else { rect };
            let group = Group {
                path: format!("f{index}"),
                rect,
                content: Rect::from_min_max(pos2(rect.min.x, rect.min.y + header), rect.max),
                panels: floating.panels.clone(),
                active: floating.active,
            };
            let chrome = dock::draw_group_chrome_details(ui, &group, seqs, &t, &mut self.auto);
            actions.extend(chrome.actions);
            if let Some(active) = group.panels.get(group.active).copied() {
                self.panel_body(ui, active, group.content);
            }
            ui.painter().rect_stroke(rect, t.radius, Stroke::new(1.0, t.focus), StrokeKind::Outside);
            let grip = Rect::from_min_max(rect.max - vec2(14.0, 14.0), rect.max);
            let response = ui.interact(grip, id.with("resize"), Sense::click_and_drag());
            response.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, response.enabled(), tl!("Resize floating panel")));
            self.auto.add_response(&format!("panel.float.{index}.resize"), &response, "Resize floating panel");
            for step in 0..3 {
                let offset = 4.0 + step as f32 * 4.0;
                ui.painter()
                    .line_segment([pos2(rect.max.x - offset, rect.max.y - 2.0), pos2(rect.max.x - 2.0, rect.max.y - offset)], Stroke::new(1.0, t.text_faint));
            }
            if response.hovered() || response.dragged() {
                ctx.set_cursor_icon(egui::CursorIcon::ResizeNwSe);
            }
            let delta = floating_resize_delta(ui, &response);
            if response.has_focus() {
                ui.painter().rect_stroke(grip, 2.0, Stroke::new(1.5, t.focus), StrokeKind::Inside);
            }
            if delta != egui::Vec2::ZERO {
                next = Rect::from_min_size(next.min, (next.size() + delta).max(vec2(160.0, 100.0)));
            }
            if next != rect {
                let _ = self.dock_command(ctx, Action::MoveFloating { panel, rect: [next.min.x, next.min.y, next.width(), next.height()] });
            }
            dock::paint_group_focus(ui, &group, self.ui.focused, &t);
            output = target(&group, chrome.tabs, false);
        });
        output
    }

    fn panel_drag(&mut self, ctx: &egui::Context, body: Rect, targets: &[DropTarget<PanelKind>]) {
        let Some(panel) = ctx.data(|data| data.get_temp::<PanelKind>(drag_id())) else { return };
        let canceled =
            ctx.input(|input| input.key_pressed(egui::Key::Escape) || !input.focused || (!input.pointer.primary_down() && !input.pointer.primary_released()));
        if canceled || (!self.ui.dock.contains(panel) && !self.ui.floating.iter().any(|group| group.panels.contains(&panel))) {
            ctx.data_mut(|data| data.remove::<PanelKind>(drag_id()));
            return;
        }
        let Some(pos) = ctx.pointer_latest_pos() else { return };
        let force_float = ctx.input(|input| input.modifiers.command);
        let edge = (!force_float).then(|| workspace_edge(body, pos, targets)).flatten();
        let proposal = (!force_float && edge.is_none()).then(|| resolve_drop(&panel, pos, targets)).flatten();
        let painter = ctx.layer_painter(egui::LayerId::new(egui::Order::Foreground, egui::Id::new("dock-drop")));
        let t = self.tokens;
        if let Some((_, _, preview)) = edge {
            painter.rect_filled(preview.shrink(2.0), t.radius, t.focus.gamma_multiply(0.2));
            painter.rect_stroke(preview.shrink(2.0), t.radius, Stroke::new(2.0, t.focus), StrokeKind::Inside);
        }
        if let Some(proposal) = &proposal {
            painter.rect_filled(proposal.preview.shrink(2.0), t.radius, t.focus.gamma_multiply(0.2));
            painter.rect_stroke(proposal.preview.shrink(2.0), t.radius, Stroke::new(2.0, t.focus), StrokeKind::Inside);
            if let Some(mark) = proposal.insertion {
                painter.rect_filled(mark, 0.0, t.focus);
            }
        }
        let title = painter.layout_no_wrap(panel.title().to_owned(), crate::theme::Tokens::ui(12.0), Color32::WHITE);
        let ghost = Rect::from_min_size(pos + vec2(12.0, 10.0), title.size() + vec2(16.0, 8.0));
        painter.rect_filled(ghost, 4.0, t.focus.gamma_multiply(0.85));
        painter.galley(ghost.min + vec2(8.0, 4.0), title, Color32::WHITE);
        ctx.set_cursor_icon(egui::CursorIcon::Grabbing);
        ctx.request_repaint();
        if ctx.input(|input| input.pointer.primary_released()) {
            ctx.data_mut(|data| data.remove::<PanelKind>(drag_id()));
            if let Some((_, zone, _)) = edge {
                if crate::menus::invoke(self, ctx, "window.dockPanel", json!({"panel": panel.id(), "zone": zone})).is_ok() {
                    self.ui.focused = panel;
                }
                return;
            }
            let action = match proposal {
                Some(proposal) => Some(Action::Move { panel, anchor: proposal.anchor, placement: proposal.placement }),
                None if force_float || !targets.iter().any(|target| target.rect.contains(pos)) => {
                    Some(Action::Float { panel, rect: [pos.x - 40.0, pos.y - 12.0, 420.0, 320.0] })
                }
                None => None,
            };
            if let Some(action) = action
                && self.dock_command(ctx, action).is_ok()
            {
                self.ui.focused = panel;
            }
        }
    }
}

/// Width of the band along the workspace border that docks beside the whole layout.
const EDGE_BAND: f32 = 8.0;

/// The outermost drop zone: near the workspace border a drop splits the root, not the group
/// under the pointer. A floating group over the border (it rejects splits) keeps the drop.
fn workspace_edge(body: Rect, pos: egui::Pos2, targets: &[DropTarget<PanelKind>]) -> Option<(Zone, &'static str, Rect)> {
    if !body.is_finite() || !body.contains(pos) || targets.iter().any(|target| !target.allow_splits && target.rect.contains(pos)) {
        return None;
    }
    let band = EDGE_BAND.min(body.width() / 4.0).min(body.height() / 4.0);
    let edges = [
        (pos.x - body.left(), Zone::Left, "left"),
        (body.right() - pos.x, Zone::Right, "right"),
        (pos.y - body.top(), Zone::Top, "top"),
        (body.bottom() - pos.y, Zone::Bottom, "bottom"),
    ];
    let (distance, zone, name) = edges.into_iter().min_by(|a, b| a.0.total_cmp(&b.0))?;
    if distance > band {
        return None;
    }
    let size = body.size() * 0.25;
    let preview = match zone {
        Zone::Left => Rect::from_min_size(body.min, vec2(size.x, body.height())),
        Zone::Right => Rect::from_min_max(pos2(body.right() - size.x, body.top()), body.max),
        Zone::Top => Rect::from_min_size(body.min, vec2(body.width(), size.y)),
        Zone::Bottom | Zone::Center => Rect::from_min_max(pos2(body.left(), body.bottom() - size.y), body.max),
    };
    Some((zone, name, preview))
}

fn target(group: &Group, mut tabs: Vec<(PanelKind, Rect)>, allow_splits: bool) -> Option<DropTarget<PanelKind>> {
    let strip = Rect::from_min_max(group.rect.min, pos2(group.rect.max.x, group.content.min.y));
    // A compact grip still identifies the source group to the shared resolver.
    for panel in &group.panels {
        if !tabs.iter().any(|(candidate, _)| candidate == panel) {
            tabs.push((*panel, Rect::NOTHING));
        }
    }
    Some(DropTarget { anchor: *group.panels.get(group.active)?, rect: group.rect, tab_strip: Some(strip), tabs, allow_splits, accepts_tabs: true })
}

/// A corner is a two-axis resize control; focused arrows belong to it and Tab moves on.
fn floating_resize_delta(ui: &mut egui::Ui, response: &egui::Response) -> egui::Vec2 {
    if response.drag_started() {
        response.request_focus();
    }
    let mut delta = if response.dragged() { response.drag_delta() } else { egui::Vec2::ZERO };
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(response.id, egui::EventFilter { horizontal_arrows: true, vertical_arrows: true, ..Default::default() })
        });
        ui.input_mut(|input| {
            for (key, amount) in [
                (egui::Key::ArrowLeft, vec2(-8.0, 0.0)),
                (egui::Key::ArrowRight, vec2(8.0, 0.0)),
                (egui::Key::ArrowUp, vec2(0.0, -8.0)),
                (egui::Key::ArrowDown, vec2(0.0, 8.0)),
            ] {
                if input.consume_key(egui::Modifiers::NONE, key) {
                    delta += amount;
                }
            }
        });
    }
    if response.enabled() { delta } else { egui::Vec2::ZERO }
}

#[cfg(test)]
mod native_viewport_tests {
    use super::*;

    #[test]
    fn floating_viewport_can_start_on_a_monitor_left_of_the_main_window() {
        let group = dock::Floating { panels: vec![PanelKind::Program], active: 0, rect: [-1920.0, 40.0, 640.0, 360.0] };
        let parent = Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 900.0));
        assert_eq!(native_viewport_position(&group, Some(parent)), pos2(-1920.0, 40.0));
    }

    #[test]
    fn native_viewport_geometry_tracks_the_window_outside_the_parent() {
        let group = dock::Floating { panels: vec![PanelKind::Program], active: 0, rect: [0.0, 0.0, 420.0, 220.0] };
        let parent = Rect::from_min_size(pos2(0.0, 0.0), vec2(1600.0, 900.0));
        let outer = Rect::from_min_size(pos2(1920.0, 60.0), vec2(656.0, 404.0));
        let inner = Rect::from_min_size(pos2(1928.0, 90.0), vec2(640.0, 360.0));
        assert_eq!(saved_native_rect(&group, Some(parent), Some(outer), Some(inner)), [1920.0, 60.0, 640.0, 360.0]);
    }

    #[test]
    fn floating_viewport_size_is_bounded() {
        assert_eq!(bounded_inner_size(f32::INFINITY, 240.0), 240.0);
        assert_eq!(bounded_inner_size(1.0e30, 240.0), 16_384.0);
        assert_eq!(bounded_inner_size(480.0, 240.0), 480.0);
    }
}

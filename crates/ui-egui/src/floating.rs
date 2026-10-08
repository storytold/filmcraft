//! Floating panels: a panel taken out of the dock (panel menu ▸ Undock Panel) lives in its own
//! window above the workspace. The window's header has three handles: the panel's tab, which is
//! dragged onto the dock to dock the panel again (the same docking guide as moving a docked tab);
//! the empty part of the header, which moves the window; and the window's right/bottom edges and
//! corner, which resize it. A panel is either in the dock or floating, never both.

use egui::{Rect, Sense, Stroke, StrokeKind, pos2, vec2};
use serde::{Deserialize, Serialize};

use crate::FilmcraftApp;
use crate::dock::{self, PanelKind};
use crate::icons::{self, Icon};
use crate::theme::Tokens;

/// One floating panel window.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FloatingPanel {
    pub panel: PanelKind,
    /// Top-left in points; None until the window is first shown (then centred in the dock area).
    #[serde(default)]
    pub pos: Option<[f32; 2]>,
    pub size: [f32; 2],
}

/// The size a panel gets when it is first undocked.
pub fn default_size(p: PanelKind) -> [f32; 2] {
    match p {
        PanelKind::Tools => [76.0, 340.0],
        PanelKind::AudioMeters => [132.0, 340.0],
        _ => [480.0, 360.0],
    }
}

fn min_size(p: PanelKind) -> [f32; 2] {
    if p.compact() { [56.0, 80.0] } else { [220.0, 140.0] }
}

/// `v` limited to `lo..=hi`; unlike `f32::clamp` never panics (a window can be smaller than the
/// limits asked of it).
fn clamp(v: f32, lo: f32, hi: f32) -> f32 {
    v.max(lo).min(hi.max(lo))
}

impl FilmcraftApp {
    /// Panel menu ▸ Undock Panel: take `p` out of the dock into a floating window.
    pub fn undock_panel(&mut self, p: PanelKind) -> Result<(), String> {
        if self.ui.floating.iter().any(|f| f.panel == p) {
            return Ok(());
        }
        let mut all = Vec::new();
        self.ui.dock.panels(&mut all);
        if !all.contains(&p) {
            return Err(format!("{} is not open", p.title()));
        }
        if all.len() <= 1 {
            return Err("The last panel in the window can't be undocked".into());
        }
        self.ui.dock.close(p);
        self.ui.floating.push(FloatingPanel { panel: p, pos: None, size: default_size(p) });
        self.ui.focused = p;
        Ok(())
    }

    /// Panel menu ▸ Dock Panel: put a floating panel back in the dock, where its workspace keeps it.
    pub fn dock_panel(&mut self, p: PanelKind) {
        self.ui.floating.retain(|f| f.panel != p);
        self.show_panel(p);
    }

    /// Show the Timeline where the workspaces keep it, unless it is floating.
    pub fn restore_timeline(&mut self) {
        if !self.ui.floating.iter().any(|f| f.panel == PanelKind::Timeline) {
            self.ui.dock.restore_timeline();
        }
    }
}

fn open_menu(ctx: &egui::Context, p: PanelKind, at: egui::Pos2) {
    let frame = ctx.cumulative_frame_nr();
    ctx.data_mut(|d| {
        d.insert_temp(egui::Id::new("panel-menu"), (p, at));
        d.insert_temp(egui::Id::new("panel-menu-opened"), frame);
    });
}

/// Draw every floating panel inside `area` (the dock area) and handle its header and edges.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context, area: Rect) {
    let t = app.tokens;
    let mut remove = None;
    for i in 0..app.ui.floating.len() {
        let Some(f) = app.ui.floating.get(i).cloned() else { continue };
        let p = f.panel;
        let [min_w, min_h] = min_size(p);
        let size = vec2(clamp(f.size[0], min_w, area.width()), clamp(f.size[1], min_h, area.height()));
        let off = 28.0 * i as f32;
        let at = match f.pos {
            Some([x, y]) => pos2(x, y),
            None => area.center() - size * 0.5 + vec2(off, off),
        };
        // keep a part of the header on screen so the window can always be grabbed
        let head_h = if p.compact() { 16.0 } else { t.tab_h };
        let at = pos2(clamp(at.x, area.min.x - size.x + 80.0, area.max.x - 80.0), clamp(at.y, area.min.y, area.max.y - head_h));
        let id = egui::Id::new(("floating-panel", p.id()));
        let focused = app.ui.focused == p;
        let (mut new_pos, mut new_size) = (at, size);
        let (mut menu_at, mut close, mut focus) = (None, false, false);

        egui::Area::new(id).order(egui::Order::Middle).fixed_pos(at).show(ctx, |ui| {
            let (rect, _) = ui.allocate_exact_size(size, Sense::hover());
            let painter = ui.painter().clone();
            painter.rect_filled(rect, t.radius, t.panel_bg);
            let head = Rect::from_min_size(rect.min, vec2(rect.width(), head_h));
            let body = Rect::from_min_max(pos2(rect.min.x, head.max.y), rect.max);
            if ui.rect_contains_pointer(rect) && ui.input(|i| i.pointer.any_pressed()) {
                focus = true;
                ctx.move_to_top(egui::LayerId::new(egui::Order::Middle, id));
            }

            // the panel itself
            app.auto.add(&format!("floating.{}", p.id()), rect, p.title());
            app.auto.add(&format!("panel.{}", p.id()), body, p.title());
            let mut child = ui.new_child(egui::UiBuilder::new().max_rect(body).id_salt(("panel", p.id())));
            child.set_clip_rect(body);
            crate::panels::show(app, &mut child, p, body);

            // header: the whole bar moves the window (drawn first, so the handles on it win)
            let mv = ui.interact(head, id.with("move"), Sense::click_and_drag());
            app.auto.add(&format!("floating.{}.move", p.id()), head, "move window");
            if mv.dragged() {
                new_pos += mv.drag_delta();
            }
            if mv.secondary_clicked() {
                menu_at = mv.interact_pointer_pos().or(Some(head.left_bottom()));
            }
            if p.compact() {
                // slim panels: grip dots at the left of the bar are the docking handle
                let grip = Rect::from_min_size(head.min, vec2(24.0_f32.min(head.width()), head_h));
                let gr = ui.interact(grip, id.with("tab"), Sense::click_and_drag());
                app.auto.add(&format!("panel.tab.{}", p.id()), grip, p.title());
                if gr.drag_started() {
                    dock::set_dragging(ctx, p);
                    focus = true;
                }
                let on = gr.hovered() || gr.dragged();
                for dx in [-4.0, 0.0, 4.0] {
                    painter.circle_filled(grip.center() + vec2(dx, 0.0), if on { 1.5 } else { 1.0 }, if on { t.tab_text_active } else { t.text_faint });
                }
            } else {
                let galley = painter.layout_no_wrap(p.title().to_string(), Tokens::ui(12.0), t.tab_text_active);
                let (label_w, label_h) = (galley.size().x, galley.size().y);
                let label_x = head.min.x + 16.0;
                let tab = Rect::from_min_size(head.min + vec2(8.0, 0.0), vec2((label_w + 16.0 + 24.0).min((head.width() - 36.0).max(24.0)), head_h));
                let tr = ui.interact(tab, id.with("tab"), Sense::click_and_drag());
                app.auto.add(&format!("panel.tab.{}", p.id()), tab, p.title());
                if tr.drag_started() {
                    dock::set_dragging(ctx, p);
                }
                if tr.clicked() || tr.drag_started() {
                    focus = true;
                }
                if tr.secondary_clicked() {
                    menu_at = tr.interact_pointer_pos().or(Some(tab.left_bottom()));
                }
                painter.galley_with_override_text_color(pos2(label_x, head.min.y + 16.0 - label_h / 2.0), galley, t.tab_text_active);
                // ≡ panel menu
                let mr = Rect::from_center_size(pos2(label_x + label_w + 12.0, head.min.y + 16.0), vec2(12.0, 10.0));
                let mresp = ui.interact(mr.expand(3.0), id.with("menu"), Sense::click());
                app.auto.add(&format!("panel.menu.{}", p.id()), mr, "panel menu");
                let mc = if mresp.hovered() { t.tab_text_active } else { t.tab_text };
                icons::paint(&painter, Rect::from_center_size(mr.center(), vec2(16.0, 16.0)), Icon::Hamburger, mc);
                painter.line_segment([pos2(label_x, head.min.y + 23.0), pos2(mr.max.x, head.min.y + 23.0)], Stroke::new(1.0, t.tab_text_active));
                if mresp.clicked() {
                    menu_at = Some(mr.left_bottom());
                }
                // × closes the panel
                let cr = Rect::from_center_size(pos2(head.max.x - 14.0, head.min.y + 16.0), vec2(8.0, 8.0));
                let cresp = ui.interact(cr.expand(5.0), id.with("close"), Sense::click());
                app.auto.add(&format!("floating.{}.close", p.id()), cr.expand(5.0), "close panel");
                let cc = if cresp.hovered() { t.tab_text_active } else { t.tab_text };
                painter.line_segment([cr.left_top(), cr.right_bottom()], Stroke::new(1.2, cc));
                painter.line_segment([cr.right_top(), cr.left_bottom()], Stroke::new(1.2, cc));
                close |= cresp.clicked();
            }

            // edges and corner resize (after the body, so they win over its widgets)
            let right = Rect::from_min_max(pos2(rect.max.x - 5.0, head.max.y), pos2(rect.max.x + 3.0, rect.max.y - 12.0));
            let bottom = Rect::from_min_max(pos2(rect.min.x, rect.max.y - 5.0), pos2(rect.max.x - 12.0, rect.max.y + 3.0));
            let corner = Rect::from_min_max(rect.max - vec2(12.0, 12.0), rect.max + vec2(3.0, 3.0));
            for (name, r, icon, dx, dy) in [
                ("right", right, egui::CursorIcon::ResizeHorizontal, 1.0, 0.0),
                ("bottom", bottom, egui::CursorIcon::ResizeVertical, 0.0, 1.0),
                ("corner", corner, egui::CursorIcon::ResizeNwSe, 1.0, 1.0),
            ] {
                let resp = ui.interact(r, id.with(name), Sense::drag());
                app.auto.add(&format!("floating.{}.resize.{name}", p.id()), r, "resize");
                if resp.hovered() || resp.dragged() {
                    ctx.set_cursor_icon(icon);
                }
                if resp.dragged() {
                    let d = resp.drag_delta();
                    new_size += vec2(d.x * dx, d.y * dy);
                }
            }
            // outline: the focus colour while the panel is focused
            painter.rect_stroke(rect, t.radius, Stroke::new(1.0, if focused { t.focus } else { egui::Color32::from_black_alpha(170) }), StrokeKind::Inside);
        });

        if let Some(f) = app.ui.floating.get_mut(i) {
            f.pos = Some([new_pos.x, new_pos.y]);
            f.size = [clamp(new_size.x, min_w, area.width()), clamp(new_size.y, min_h, area.height())];
        }
        if focus {
            app.ui.focused = p;
        }
        if let Some(at) = menu_at {
            app.ui.focused = p;
            open_menu(ctx, p, at);
        }
        if close {
            remove = Some(p);
        }
    }
    if let Some(p) = remove {
        app.ui.floating.retain(|f| f.panel != p);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_never_panics_when_the_limits_cross() {
        assert_eq!(clamp(5.0, 10.0, 2.0), 10.0);
        assert_eq!(clamp(50.0, 10.0, 20.0), 20.0);
        assert_eq!(clamp(f32::NAN, 10.0, 20.0), 10.0);
    }

    #[test]
    fn floating_panels_round_trip_through_json() {
        let f = FloatingPanel { panel: PanelKind::Effects, pos: None, size: default_size(PanelKind::Effects) };
        let back: FloatingPanel = serde_json::from_str(&serde_json::to_string(&f).unwrap()).unwrap();
        assert_eq!(back, f);
        assert!(default_size(PanelKind::Tools)[0] < default_size(PanelKind::Program)[0]);
    }
}

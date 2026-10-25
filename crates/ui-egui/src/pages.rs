//! The page bar of the Evolve workspace set ([`crate::dock::WorkspaceSet::Evolve`]):
//! one button per page (Media, Cut, Edit, Color, Audio, Deliver) centred along the bottom of
//! the window, the current page in the highlight colour with an underline. A click switches the
//! workspace (`window.page.<page>`); the buttons take keyboard focus and register the automation
//! ids `pages.<page>` (the bar itself is `pages.bar`). Original artwork: text labels only.

use egui::{Rect, Sense, Stroke, StrokeKind, pos2, vec2};

use crate::FilmcraftApp;
use crate::dock::{EVOLVE_PAGES, slug};
use crate::theme::Tokens;

/// Height of the bar in points.
pub const HEIGHT: f32 = 30.0;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let painter = ui.painter().clone();
    painter.rect_filled(rect, 0.0, t.header_bg);
    painter.line_segment([rect.left_top(), rect.right_top()], Stroke::new(1.0, t.separator));
    app.auto.add("pages.bar", rect, "Pages");
    let font = Tokens::ui(12.0);
    let labels: Vec<(&str, String)> = EVOLVE_PAGES.iter().map(|name| (*name, crate::i18n::t(name).to_string())).collect();
    let galleys: Vec<_> = labels.iter().map(|(_, label)| painter.layout_no_wrap(label.clone(), font.clone(), t.text)).collect();
    const PAD: f32 = 14.0;
    const SPACING: f32 = 6.0;
    let widths: Vec<f32> = galleys.iter().map(|g| g.size().x + 2.0 * PAD).collect();
    let total = widths.iter().sum::<f32>() + SPACING * widths.len().saturating_sub(1) as f32;
    // centred, unless the window is too narrow: then from the left edge
    let mut x = if total + 16.0 <= rect.width() { rect.center().x - total / 2.0 } else { rect.min.x + 8.0 };
    let mut clicked: Option<&str> = None;
    for (((name, label), galley), w) in labels.iter().zip(galleys).zip(widths) {
        let r = Rect::from_min_size(pos2(x, rect.min.y + 3.0), vec2(w, (rect.height() - 6.0).max(1.0)));
        let active = app.ui.workspace == *name;
        let resp = ui.interact(r, egui::Id::new(("page", *name)), Sense::click());
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, resp.enabled(), active, label.as_str()));
        ui.ctx().accesskit_node_builder(resp.id, |node| {
            node.set_role(egui::accesskit::Role::Tab);
            node.set_selected(active);
        });
        app.auto.add_response(&format!("pages.{}", slug(name)), &resp, label);
        if resp.hovered() && !active {
            painter.rect_filled(r, t.radius_sm, t.hover);
        }
        let color = if active {
            t.accent_text()
        } else if resp.hovered() {
            t.text
        } else {
            t.text_dim
        };
        let size = galley.size();
        painter.galley_with_override_text_color(pos2(r.center().x - size.x / 2.0, r.center().y - size.y / 2.0), galley, color);
        if active {
            painter.line_segment([pos2(r.min.x + 6.0, r.max.y - 1.0), pos2(r.max.x - 6.0, r.max.y - 1.0)], Stroke::new(2.0, t.accent));
        }
        if resp.has_focus() {
            painter.rect_stroke(r.shrink(1.0), t.radius_sm, Stroke::new(1.5, t.focus), StrokeKind::Inside);
        }
        if resp.clicked() {
            clicked = Some(name);
        }
        x += w + SPACING;
    }
    if let Some(name) = clicked {
        app.set_workspace(name);
    }
}

//! Closure-based dropdown for the remaining egui panels. The UI port's shared
//! picker binds typed options instead, so these bodies cannot be passed to it.
//! Keep the old shared combo's grow-back behavior until panels adopt that picker.

use egui::{
    AsIdSalt, Atom, AtomExt as _, Atoms, Button, IdSalt, InnerResponse, IntoAtoms, Popup, PopupCloseBehavior, Role, ScrollArea, TextWrapMode, Ui, WidgetInfo,
};

pub(crate) struct Dropdown<'a> {
    salt: IdSalt,
    selected: Atoms<'a>,
    width: Option<f32>,
    height: Option<f32>,
    truncate: bool,
}

impl<'a> Dropdown<'a> {
    pub(crate) fn from_id_salt(salt: impl AsIdSalt) -> Self {
        Self { salt: IdSalt::new(salt), selected: Atoms::default(), width: None, height: None, truncate: false }
    }

    pub(crate) fn selected_text(mut self, text: impl IntoAtoms<'a>) -> Self {
        self.selected = text.into_atoms();
        self
    }

    pub(crate) fn width(mut self, width: f32) -> Self {
        self.width = Some(width);
        self
    }

    pub(crate) fn height(mut self, height: f32) -> Self {
        self.height = Some(height);
        self
    }

    pub(crate) fn truncate(mut self) -> Self {
        self.truncate = true;
        self
    }

    pub(crate) fn show_ui<R>(mut self, ui: &mut Ui, body: impl FnOnce(&mut Ui) -> R) -> InnerResponse<Option<R>> {
        let name = self.selected.text().map(String::from).unwrap_or_default();
        let rtl = ui.style().right_to_left;
        self.selected.push_right(
            Atom::paint(egui::Vec2::splat(ui.spacing().icon_width), |ui, args| {
                let rect = egui::Rect::from_center_size(args.rect.center(), args.rect.size() * egui::vec2(0.7, 0.45));
                let color = ui.visuals().widgets.inactive.fg_stroke.color;
                ui.painter().add(egui::Shape::rotated_triangle(rect, 0.0, color));
            })
            .atom_grow(true)
            .atom_align(if rtl { egui::Align2::LEFT_CENTER } else { egui::Align2::RIGHT_CENTER }),
        );
        let width = self.width.unwrap_or(ui.spacing().combo_width);
        let mut button = Button::new(self.selected).min_size(egui::vec2(width, 0.0));
        if self.truncate {
            button = button.truncate();
        }
        let response = ui.push_id(self.salt, |ui| ui.add(button)).inner;
        response.widget_info(|| {
            let mut info = WidgetInfo::new(Role::ComboBox);
            info.enabled = ui.is_enabled();
            info.current_text_value = Some(name.clone());
            info
        });
        // A Button initially names itself from its displayed text. A dropdown's
        // selection is its value, not its field name; let the panel provide the name.
        ui.ctx().accesskit_node_builder(response.id, |node| node.clear_label());

        let bounds = ui.ctx().content_rect();
        let room = (bounds.bottom() - response.rect.bottom()).max(response.rect.top() - bounds.top());
        let max_height = self.height.unwrap_or(ui.spacing().combo_height).min(room).max(0.0);
        let inner = Popup::menu(&response)
            .id(response.id.with("popup"))
            .width(response.rect.width())
            .close_behavior(PopupCloseBehavior::CloseOnClick)
            .show(|ui| {
                ui.set_min_width(ui.available_width());
                // Unlike the stock combo, offer the full available height even if
                // the list was small last frame (e.g. when a filter is cleared).
                ui.set_max_height(max_height);
                ui.style_mut().wrap_mode = Some(TextWrapMode::Extend);
                ScrollArea::vertical().max_height(max_height).show(ui, body).inner
            })
            .map(|output| output.inner);
        InnerResponse { inner, response }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropdown_keeps_the_panel_name_separate_from_its_current_value() {
        let ctx = egui::Context::default();
        ctx.enable_accesskit();
        let mut frame = ctx.run_ui(egui::RawInput::default(), |ui| {
            let r = Dropdown::from_id_salt("shape").selected_text("Rectangle").show_ui(ui, |_| {});
            // This is the same name-if-missing convention used by accessible_name.
            ctx.accesskit_node_builder(r.response.id, |node| {
                if node.label().is_none_or(|label| label.trim().is_empty()) {
                    node.set_label("Shape");
                }
            });
        });
        frame.textures_delta.clear();
        let tree = frame.platform_output.accesskit_update.unwrap();
        assert!(tree.nodes.iter().any(|(_, node)| { node.role() == Role::ComboBox && node.label() == Some("Shape") && node.value() == Some("Rectangle") }));
    }

    #[test]
    fn dropdown_grows_again_without_reopening() {
        let ctx = egui::Context::default();
        let draw = |rows| {
            let mut size = None;
            let mut frame = ctx.run_ui(
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(800.0, 600.0))), ..Default::default() },
                |ui| {
                    let dropdown = Dropdown::from_id_salt("filter").selected_text("All formats").height(300.0).show_ui(ui, |ui| {
                        for row in 0..rows {
                            ui.label(format!("Format {row}"));
                        }
                    });
                    let popup = dropdown.response.id.with("popup");
                    size = ctx.read_response(popup).map(|response| response.rect.height());
                    if rows == 0 {
                        Popup::open_id(&ctx, popup);
                    }
                },
            );
            frame.textures_delta.clear();
            size
        };
        draw(0);
        let _ = draw(12);
        let large = draw(12).expect("the menu is open");
        let _ = draw(1);
        let small = draw(1).expect("filtering keeps the menu open");
        assert!(small < large, "filtering shrinks the menu: {small} vs {large}");
        let _ = draw(12);
        let regrown = draw(12).expect("clearing the filter keeps the same menu open");
        assert!(regrown > small * 2.0, "clearing the filter grows the menu: {regrown} vs {small}");
    }
}

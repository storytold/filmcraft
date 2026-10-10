//! The vertical Tools panel. Grouped tools show a small flyout triangle; right-click (or long
//! press) opens the group to pick another tool.
//!
//! Automation ids: `tools.<Tool>` is the button of the tool a group shows (a click selects
//! exactly that tool; the group's other tools have no button of their own), `tools.group.<first
//! tool of the group>` the same button whatever it shows (right-click it to open the flyout), and
//! while the flyout is open `tools.select.<Tool>` for each of the group's tools.

use egui::{Rect, Sense, pos2, vec2};

use crate::FilmcraftApp;
use crate::icons;
use crate::state::Tool;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let mut y = rect.min.y + 4.0;
    let size = 32.0;
    let held_id = egui::Id::new("tools-held");
    if ui.input(|i| i.pointer.any_pressed()) {
        ui.data_mut(|d| d.remove::<egui::Id>(held_id));
    }
    for group in Tool::groups() {
        let current = if group.contains(&app.ui.tool) { app.ui.tool } else { group[0] };
        let r = Rect::from_min_size(pos2(rect.center().x - size / 2.0, y), vec2(size, size));
        let tip = if current.shortcut().is_empty() {
            app.ui.language.tr(current.label()).to_owned()
        } else {
            format!("{} ({})", app.ui.language.tr(current.label()), current.shortcut())
        };
        let resp = ui.interact(r, egui::Id::new(("tool", format!("{:?}", group[0]))), Sense::click()).on_hover_text(tip);
        // only the tool the button shows: a click there selects it, not another tool of the group
        app.auto.add(&format!("tools.{current:?}"), r, current.label());
        if group.len() > 1 {
            app.auto.add(&format!("tools.group.{:?}", group[0]), r, current.label());
        }
        let active = group.contains(&app.ui.tool);
        if active {
            ui.painter().rect_filled(Rect::from_center_size(r.center(), vec2(26.0, 26.0)), 4.0, t.accent);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, t.hover);
        }
        icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(17.0, 17.0)), current.icon(), if active { egui::Color32::WHITE } else { t.icon });
        if group.len() > 1 {
            let c = r.right_bottom() - vec2(3.0, 3.0);
            ui.painter().add(egui::Shape::convex_polygon(vec![c, c - vec2(5.0, 0.0), c - vec2(0.0, 5.0)], t.text_dim, egui::Stroke::NONE));
        }
        let held = ui.data(|d| d.get_temp::<egui::Id>(held_id)) == Some(resp.id);
        if resp.clicked() && !held {
            app.ui.tool = current;
        }
        if group.len() > 1 {
            let held_for = resp.is_pointer_button_down_on().then(|| ui.input(|i| i.pointer.press_start_time().map(|s| i.time - s))).flatten();
            if let Some(seconds) = held_for
                && seconds < 0.35
            {
                ui.ctx().request_repaint_after(std::time::Duration::from_secs_f64(0.35 - seconds));
            }
            let long_press = held_for.is_some_and(|seconds| seconds >= 0.35) && !held;
            let command = if resp.secondary_clicked() || long_press {
                if long_press {
                    ui.data_mut(|d| d.insert_temp(held_id, resp.id));
                }
                Some(egui::SetOpenCommand::Bool(true))
            } else if resp.clicked() && !held {
                Some(egui::SetOpenCommand::Bool(false))
            } else {
                None
            };
            let close_behavior = if held && resp.clicked() { egui::PopupCloseBehavior::IgnoreClicks } else { egui::PopupCloseBehavior::CloseOnClickOutside };
            egui::Popup::menu(&resp).open_memory(command).close_behavior(close_behavior).show(|ui| {
                for tl in &group {
                    let row = ui.selectable_label(app.ui.tool == *tl, format!("{}   {}", app.ui.language.tr(tl.label()), tl.shortcut()));
                    app.auto.add(&format!("tools.select.{tl:?}"), row.rect, tl.label());
                    if row.clicked() {
                        app.ui.tool = *tl;
                        ui.close();
                    }
                }
            });
        }
        y += size + 4.0;
        if y > rect.max.y - size {
            break;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(app: &mut FilmcraftApp, ctx: &egui::Context, time: f64, events: Vec<egui::Event>) {
        app.auto.begin_frame();
        let mut out = ctx.run_ui(
            egui::RawInput { time: Some(time), events, screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 800.0))), ..Default::default() },
            |ui| show(app, ui, Rect::from_min_size(egui::Pos2::ZERO, vec2(42.0, 700.0))),
        );
        out.textures_delta.clear();
    }

    #[test]
    fn long_press_type_selects_vertical_without_selecting_on_release() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &app.tokens);
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(&mut app, &ctx, 0.1, vec![]);
        let center = |element: &crate::automation::Element| {
            let [x, y, w, h] = element.rect;
            egui::pos2(x + w / 2.0, y + h / 2.0)
        };
        let at = center(app.auto.elements.iter().find(|w| w.id == "tools.Type").unwrap());
        let pointer = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        frame(&mut app, &ctx, 1.0, vec![egui::Event::PointerMoved(at), pointer(at, true)]);
        frame(&mut app, &ctx, 1.36, vec![]);
        frame(&mut app, &ctx, 1.4, vec![pointer(at, false)]);
        frame(&mut app, &ctx, 1.45, vec![]);
        assert_eq!(app.ui.tool, Tool::Selection);
        let row = center(app.auto.elements.iter().find(|w| w.id == "tools.select.VerticalType").unwrap());
        frame(&mut app, &ctx, 2.0, vec![egui::Event::PointerMoved(row), pointer(row, true)]);
        frame(&mut app, &ctx, 2.05, vec![pointer(row, false)]);
        assert_eq!(app.ui.tool, Tool::VerticalType);
        frame(&mut app, &ctx, 2.1, vec![]);
        assert!(!app.auto.elements.iter().any(|w| w.id == "tools.select.VerticalType"));
        app.session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        let pic = Rect::from_min_size(pos2(100.0, 100.0), vec2(480.0, 270.0));
        for (time, pressed) in [(2.5, None), (2.6, None), (3.0, Some(true)), (3.05, Some(false))] {
            let mut out = ctx.run_ui(
                egui::RawInput {
                    time: Some(time),
                    events: pressed.map(|pressed| vec![egui::Event::PointerMoved(pic.center()), pointer(pic.center(), pressed)]).unwrap_or_default(),
                    screen_rect: Some(Rect::from_min_size(egui::Pos2::ZERO, vec2(1000.0, 800.0))),
                    ..Default::default()
                },
                |ui| {
                    crate::panels::graphics::monitor_overlay(&mut app, ui, pic, (1920, 1080));
                },
            );
            out.textures_delta.clear();
        }
        let edit = app.ui.gfx_edit.as_ref().unwrap();
        let sequence = app.session.active_sequence().unwrap();
        let (_, item) = sequence.find_item(filmcraft_project::ClipId(edit.clip)).unwrap();
        let effect_index = filmcraft_project::graphic::layer_indices(&item.effects)[edit.layer];
        let layer = filmcraft_project::graphic::eval_layer(&item.effects[effect_index], filmcraft_time::Tick::ZERO, (1920, 1080)).unwrap();
        let filmcraft_project::graphic::LayerContent::Text(text) = layer.content else { panic!("text layer expected") };
        assert!(text.vertical, "the actual monitor click dispatches the existing vertical text command");
    }
}

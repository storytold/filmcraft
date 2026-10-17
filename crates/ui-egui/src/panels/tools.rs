//! The Tools panel. The buttons run along the panel's longer side and wrap onto further lines when
//! it is too short to hold them. Grouped tools show a small flyout triangle; right-click, a long
//! press or pressing and dragging opens the group to pick another tool.

use egui::{Rect, Sense, pos2, vec2};

use crate::FilmcraftApp;
use crate::icons;
use crate::state::Tool;

const SIZE: f32 = 32.0;
const GAP: f32 = 4.0;

/// Where the `i`th of `n` buttons goes: in a line along the longer side of `rect`, wrapping onto
/// further lines as the panel's length needs, the lines centred across it. `None` when the panel
/// has no room for it.
fn button_pos(rect: Rect, i: usize, n: usize) -> Option<egui::Pos2> {
    let step = SIZE + GAP;
    let horizontal = rect.width() > rect.height();
    let (along, across) = if horizontal { (rect.width(), rect.height()) } else { (rect.height(), rect.width()) };
    let per_line = ((along / step).floor() as usize).max(1);
    let room = ((across / step).floor() as usize).max(1);
    let line = i / per_line;
    if line >= room {
        return None;
    }
    let lines = n.div_ceil(per_line).min(room);
    // in a row the lines are centred on the whole strip, the grip above the panel included
    let grip = if horizontal { crate::dock::COMPACT_GRIP_H } else { 0.0 };
    let start = (across - grip - (lines as f32 * step - GAP)) / 2.0;
    let (a, c) = (GAP + (i % per_line) as f32 * step, start + line as f32 * step);
    Some(if horizontal { pos2(rect.min.x + a, rect.min.y + c) } else { pos2(rect.min.x + c, rect.min.y + a) })
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let held_id = egui::Id::new("tools-held");
    if ui.input(|i| i.pointer.any_pressed()) {
        ui.data_mut(|d| d.remove::<egui::Id>(held_id));
    }
    let groups = Tool::groups();
    let count = groups.len();
    for (i, group) in groups.into_iter().enumerate() {
        let Some(at) = button_pos(rect, i, count) else { break };
        let current = if group.contains(&app.ui.tool) { app.ui.tool } else { group[0] };
        let r = Rect::from_min_size(at, vec2(SIZE, SIZE));
        let tip = if current.shortcut().is_empty() {
            app.ui.language.tr(current.label()).to_owned()
        } else {
            format!("{} ({})", app.ui.language.tr(current.label()), current.shortcut())
        };
        let resp = ui.interact(r, egui::Id::new(("tool", format!("{:?}", group[0]))), Sense::click_and_drag()).on_hover_text(tip);
        resp.widget_info(|| egui::WidgetInfo::selected(egui::WidgetType::SelectableLabel, resp.enabled(), app.ui.tool == current, current.label()));
        for tl in &group {
            if *tl == current {
                app.auto.add_response(&format!("tools.{tl:?}"), &resp, tl.label());
            } else {
                // Hidden group alternatives remain click aliases, not separate focus targets.
                app.auto.add(&format!("tools.{tl:?}"), r, tl.label());
            }
        }
        let active = group.contains(&app.ui.tool);
        if active {
            ui.painter().rect_filled(Rect::from_center_size(r.center(), vec2(26.0, 26.0)), 4.0, t.accent);
        } else if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, t.hover);
        }
        icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(17.0, 17.0)), current.icon(), if active { egui::Color32::WHITE } else { t.icon });
        if resp.has_focus() {
            ui.painter().rect_stroke(r.shrink(2.0), 4.0, egui::Stroke::new(1.5, t.focus), egui::StrokeKind::Inside);
        }
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
            // dragging off the press (egui's few-point drag threshold) opens it at once, without
            // waiting for the long press
            let long_press = (held_for.is_some_and(|seconds| seconds >= 0.35) || resp.drag_started()) && !held;
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
                    app.auto.add_response(&format!("tools.select.{tl:?}"), &row, tl.label());
                    // letting go over a row of a flyout this press opened picks that tool
                    let released_on = held && ui.input(|i| i.pointer.primary_released() && i.pointer.interact_pos().is_some_and(|p| row.rect.contains(p)));
                    if row.clicked() || released_on {
                        app.ui.tool = *tl;
                        ui.close();
                    }
                }
            });
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

    fn center_of(app: &FilmcraftApp, id: &str) -> Option<egui::Pos2> {
        let [x, y, w, h] = app.auto.elements.iter().find(|w| w.id == id)?.rect;
        Some(egui::pos2(x + w / 2.0, y + h / 2.0))
    }

    fn primary(pos: egui::Pos2, pressed: bool) -> egui::Event {
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }

    /// The panel shown twice, then the button pressed on the Type tool group and dragged by `by`
    /// within 0.1 s, well before a long press.
    fn press_and_drag_type(by: egui::Vec2) -> (FilmcraftApp, egui::Context) {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &app.tokens);
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(&mut app, &ctx, 0.1, vec![]);
        let at = center_of(&app, "tools.Type").unwrap();
        frame(&mut app, &ctx, 1.0, vec![egui::Event::PointerMoved(at), primary(at, true)]);
        frame(&mut app, &ctx, 1.05, vec![egui::Event::PointerMoved(at + by)]);
        frame(&mut app, &ctx, 1.1, vec![]);
        (app, ctx)
    }

    fn positions(width: f32, height: f32) -> Vec<Option<egui::Pos2>> {
        let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(width, height));
        (0..8).map(|i| button_pos(rect, i, 8)).collect()
    }

    #[test]
    fn a_tall_panel_is_one_column_and_a_wide_one_is_one_row() {
        let column = positions(42.0, 700.0);
        assert!(column.iter().all(|p| p.is_some_and(|p| p.x == 5.0)), "centred across a 42 wide panel: {column:?}");
        assert_eq!(column[1].map(|p| p.y), Some(40.0));
        let row = positions(700.0, 42.0);
        // the 42 point strip sits under an 8 point grip: centred on the 50 points of both
        assert!(row.iter().all(|p| p.is_some_and(|p| p.y == 1.0)), "{row:?}");
        assert_eq!(row[1].map(|p| p.x), Some(40.0));
    }

    #[test]
    fn a_short_panel_wraps_onto_further_lines_and_every_button_is_reachable() {
        // 150 wide by 80 tall: four to a line, and room for two lines
        let rect = Rect::from_min_size(egui::Pos2::ZERO, vec2(150.0, 80.0));
        let all: Vec<_> = (0..8).filter_map(|i| button_pos(rect, i, 8)).collect();
        assert_eq!(all.len(), 8);
        for p in &all {
            assert!(rect.contains_rect(Rect::from_min_size(*p, vec2(SIZE, SIZE))), "{p:?} is outside {rect:?}");
        }
        let distinct: std::collections::HashSet<_> = all.iter().map(|p| (p.x as i32, p.y as i32)).collect();
        assert_eq!(distinct.len(), 8, "no two buttons overlap");
    }

    #[test]
    fn a_panel_with_no_room_for_a_button_gives_none_and_never_panics() {
        assert!(button_pos(Rect::from_min_size(egui::Pos2::ZERO, vec2(0.0, 0.0)), 0, 8).is_some());
        assert!(button_pos(Rect::from_min_size(egui::Pos2::ZERO, vec2(30.0, 30.0)), 5, 8).is_none());
        let _ = button_pos(Rect::from_min_size(egui::Pos2::ZERO, vec2(f32::NAN, 50.0)), 1, 8);
    }

    #[test]
    fn dragging_off_a_grouped_tool_opens_its_flyout_and_letting_go_on_a_row_picks_it() {
        let (mut app, ctx) = press_and_drag_type(vec2(-6.0, 3.0));
        let row = center_of(&app, "tools.select.VerticalType").expect("the drag opened the flyout before a long press");
        frame(&mut app, &ctx, 1.15, vec![egui::Event::PointerMoved(row)]);
        frame(&mut app, &ctx, 1.2, vec![primary(row, false)]);
        frame(&mut app, &ctx, 1.25, vec![]);
        assert_eq!(app.ui.tool, Tool::VerticalType);
        assert!(center_of(&app, "tools.select.VerticalType").is_none(), "picking a tool closes the flyout");
    }

    #[test]
    fn letting_go_of_a_dragged_open_flyout_elsewhere_leaves_it_open() {
        let (mut app, ctx) = press_and_drag_type(vec2(5.0, 5.0));
        let away = pos2(600.0, 700.0);
        frame(&mut app, &ctx, 1.15, vec![egui::Event::PointerMoved(away)]);
        frame(&mut app, &ctx, 1.2, vec![primary(away, false)]);
        frame(&mut app, &ctx, 1.25, vec![]);
        assert_eq!(app.ui.tool, Tool::Selection);
        let row = center_of(&app, "tools.select.VerticalType").expect("the flyout stays open, as after a long press");
        // and a click on one of its rows still picks it
        frame(&mut app, &ctx, 2.0, vec![egui::Event::PointerMoved(row), primary(row, true)]);
        frame(&mut app, &ctx, 2.05, vec![primary(row, false)]);
        assert_eq!(app.ui.tool, Tool::VerticalType);
    }

    #[test]
    fn a_right_click_on_a_grouped_tool_still_opens_its_flyout() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        crate::theme::install(&ctx, &app.tokens);
        frame(&mut app, &ctx, 0.0, vec![]);
        frame(&mut app, &ctx, 0.1, vec![]);
        let at = center_of(&app, "tools.Type").unwrap();
        let secondary = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Secondary, pressed, modifiers: Default::default() };
        frame(&mut app, &ctx, 1.0, vec![egui::Event::PointerMoved(at), secondary(true)]);
        frame(&mut app, &ctx, 1.05, vec![secondary(false)]);
        frame(&mut app, &ctx, 1.1, vec![]);
        assert!(center_of(&app, "tools.select.VerticalType").is_some());
        assert_eq!(app.ui.tool, Tool::Selection);
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

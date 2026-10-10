//! The header bar (Premiere 26 layout, 38 pt): Home and Import / Edit / Export on the left, the
//! document title centred, the workspace name and quick actions on the right. On macOS the menus
//! live in the native menu bar (set up by the app); elsewhere a compact in-window menu bar follows
//! the mode tabs.

use egui::{Color32, Rect, Sense, Stroke, pos2, vec2};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::Mode;
use crate::theme::Tokens;

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let p = ui.painter().clone();
    p.rect_filled(rect, 0.0, t.header_bg);
    p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, Color32::BLACK));
    let drag = ui.interact(rect, egui::Id::new("header-drag"), Sense::click_and_drag());
    if drag.drag_started() {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }
    let mut x = rect.min.x + if app.integrated_titlebar { 104.0 } else { 14.0 };
    // Home
    let home = Rect::from_center_size(pos2(x + 10.0, rect.center().y), vec2(26.0, 26.0));
    let hresp = ui.interact(home, egui::Id::new("hdr-home"), Sense::click()).on_hover_text(tl!("Home"));
    app.auto.add("header.home", home, "Home");
    if hresp.hovered() {
        p.rect_filled(home, 4.0, t.hover);
    }
    icons::paint(&p, home.shrink(5.0), Icon::Home, if hresp.hovered() { t.text } else { t.text_dim });
    if hresp.clicked() {
        app.ui.mode = Mode::Import;
    }
    x = home.max.x + 16.0;
    // Mode tabs (14 pt; active = primary text with a 2 pt underline under the label)
    for (m, label, shown) in [(Mode::Import, "Import", tl!("Import")), (Mode::Edit, "Edit", tl!("Edit")), (Mode::Export, "Export", tl!("Export"))] {
        let active = app.ui.mode == m;
        let galley = p.layout_no_wrap(shown.to_string(), Tokens::ui(14.0), if active { t.tab_text_active } else { t.tab_text });
        let r = Rect::from_min_size(pos2(x - 6.0, rect.min.y + 6.0), vec2(galley.size().x + 12.0, rect.height() - 12.0));
        let resp = ui.interact(r, egui::Id::new(("mode", label)), Sense::click());
        app.auto.add(&format!("header.mode.{}", label.to_ascii_lowercase()), r, label);
        let col = if active || resp.hovered() { t.tab_text_active } else { t.tab_text };
        let gw = galley.size().x;
        p.galley_with_override_text_color(pos2(x, rect.center().y - galley.size().y / 2.0), galley, col);
        if active {
            p.line_segment([pos2(x, rect.min.y + 29.0), pos2(x + gw, rect.min.y + 29.0)], Stroke::new(2.0, t.tab_text_active));
        }
        if resp.clicked() {
            app.ui.mode = m;
        }
        x += gw + 24.0;
    }
    let mut left_end = x;
    // In-window menus when there is no native menu bar.
    if app.ui.show_menu_bar {
        let menu_rect = Rect::from_min_max(pos2(x + 6.0, rect.min.y + 7.0), pos2(x + 520.0, rect.max.y - 7.0));
        let mut mu = ui.new_child(egui::UiBuilder::new().max_rect(menu_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        mu.style_mut().visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        mu.style_mut().visuals.widgets.inactive.bg_stroke = Stroke::NONE;
        mu.style_mut().visuals.override_text_color = Some(t.text_dim);
        crate::menus::menu_bar(app, &mut mu);
        left_end = mu.min_rect().max.x;
    }
    // Document title, centred
    let title = if app.session.is_dirty() { tlf!("{name} - Edited", name = app.session.project.name) } else { app.session.project.name.clone() };
    let ws = crate::i18n::t(&app.ui.workspace).to_uppercase();
    let available = (rect.right() - 14.0 - left_end - 10.0).max(0.0);
    if available < 28.0 {
        return;
    }
    let mut job = egui::text::LayoutJob::simple_singleline(ws.clone(), Tokens::ui(11.0), t.text_dim);
    job.wrap = egui::text::TextWrapping::truncate_at_width((available - 46.0).clamp(0.0, 152.0));
    let wg = p.layout_job(job);
    let name_width = if available >= 54.0 { wg.size().x + 8.0 } else { 0.0 };
    let workspace_width = 28.0 + if name_width > 0.0 { 10.0 + name_width } else { 0.0 };
    let actions = ((available - workspace_width) / 38.0).floor().clamp(0.0, 6.0) as u8;
    // Right cluster: icons at ~38 pt pitch, then workspace name in caps.
    let mut rx = rect.max.x - 14.0;
    let mut btn = |ui: &mut egui::Ui, icon: Icon, id: &str, tip: &str, app: &mut FilmcraftApp| -> Option<egui::Response> {
        let priority = match id {
            "appearance" => 1,
            "search" => 2,
            "quickExport" => 3,
            "notifications" => 4,
            "volume" => 5,
            "fullscreen" => 6,
            _ => 0,
        };
        if actions < priority {
            return None;
        }
        let r = Rect::from_center_size(pos2(rx - 14.0, rect.center().y), vec2(28.0, 28.0));
        rx -= 38.0;
        let resp = ui.interact(r, egui::Id::new(("hdr", id)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("header.{id}"), r, tip);
        if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(6.0), icon, if resp.hovered() { t.text } else { t.text_dim });
        Some(resp)
    };
    if btn(ui, Icon::Fullscreen, "fullscreen", tl!("Full screen"), app).is_some_and(|r| r.clicked()) {
        let fs = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
    }
    if btn(ui, Icon::Speaker, "volume", tl!("Volume"), app).is_some_and(|r| r.clicked()) {
        app.ui.status = tl!("Master volume: use the Audio Track Mixer").into();
    }
    if btn(ui, Icon::Search, "search", tl!("Search"), app).is_some_and(|r| r.clicked()) {
        app.show_panel(crate::dock::PanelKind::Effects);
    }
    if btn(ui, Icon::Bell, "notifications", tl!("Progress"), app).is_some_and(|r| r.clicked()) {
        app.ui.mode = Mode::Export;
    }
    // Quick Export: a popup with File Name & Location, a preset list and Export (Premiere 26)
    let quick = btn(ui, Icon::Export, "quickExport", tl!("Quick Export"), app);
    if quick.as_ref().is_some_and(|r| r.clicked()) {
        app.ui.export.quick_open = !app.ui.export.quick_open;
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("quick-export-toggled"), true));
    }
    if let Some(quick) = quick {
        crate::panels::export_mode::quick_export(app, ui.ctx(), pos2((quick.rect.right() - 340.0).max(rect.left()), rect.max.y + 4.0));
    }
    // Appearance Mode: click for the next one (Auto, Light, Dark); the icon shows the current one.
    let (icon, mode) = match app.session.prefs.appearance.appearance_mode.as_str() {
        "auto" => (Icon::Monitor, tl!("Sync with system")),
        "light" => (Icon::Sun, tl!("Light")),
        _ => (Icon::Moon, tl!("Dark")),
    };
    let tip = format!("{}: {mode}", tl!("Appearance Mode"));
    if btn(ui, icon, "appearance", &tip, app).is_some_and(|r| r.clicked()) {
        crate::panels::settings::cycle_appearance(app, ui.ctx());
    }
    let Some(ws_resp) = btn(ui, Icon::Workspaces, "workspaces", tl!("Workspaces"), app) else { return };
    // workspace name (caps)
    let wr = Rect::from_min_size(pos2(ws_resp.rect.left() - 10.0 - name_width, rect.center().y - 10.0), vec2(name_width, 20.0));
    let wresp = ui.interact(wr, egui::Id::new("hdr-ws-name"), Sense::click());
    if name_width > 0.0 {
        app.auto.add("header.workspaceName", wr, &ws);
        p.galley_with_override_text_color(pos2(wr.min.x + 4.0, rect.center().y - wg.size().y / 2.0), wg, if wresp.hovered() { t.text } else { t.text_dim });
    }
    let mut controls_left = if name_width > 0.0 { wr.left() } else { ws_resp.rect.left() };
    // Community: match PhotoCraft's plain outline button; also available in Help on narrow windows.
    {
        let label = "Discord";
        let g = p.layout_no_wrap(label.to_string(), Tokens::ui(12.0), t.text_dim);
        let w = g.size().x + 36.0;
        let r = Rect::from_min_size(pos2(controls_left - w - 10.0, rect.center().y - 14.0), vec2(w, 28.0));
        if r.left() >= left_end + 10.0 {
            let resp = ui.interact(r, egui::Id::new("hdr-discord"), Sense::click()).on_hover_text(tl!("Join the ArtCraft Discord (discord.gg/artcraft)"));
            app.auto.add("header.discord", r, "Join the ArtCraft Discord");
            if resp.hovered() {
                ui.painter().rect_filled(r, 4.0, t.hover);
            }
            let col = if resp.hovered() { t.text } else { t.text_dim };
            icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 15.0, r.center().y), vec2(14.0, 14.0)), Icon::Chat, col);
            ui.painter().galley_with_override_text_color(pos2(r.min.x + 28.0, r.center().y - g.size().y / 2.0), g, col);
            controls_left = r.left();
            if resp.clicked() {
                crate::links::open(app, ui.ctx(), crate::links::DISCORD);
            }
        }
        // Localized menus and user titles can be wider than English. Use the actual gap.
        let left = left_end + 12.0;
        let right = controls_left - 12.0;
        if right > left + 40.0 {
            let mut job = egui::text::LayoutJob::simple_singleline(title, Tokens::ui(14.0), t.tab_text_active);
            job.wrap.max_width = right - left;
            job.wrap.max_rows = 1;
            let galley = p.layout_job(job);
            let half = galley.size().x / 2.0;
            let center = if rect.center().x - half >= left && rect.center().x + half <= right { rect.center().x } else { (left + right) / 2.0 };
            p.with_clip_rect(Rect::from_min_max(pos2(left, rect.min.y), pos2(right, rect.max.y))).galley(
                pos2(center - half, rect.center().y - galley.size().y / 2.0),
                galley,
                t.tab_text_active,
            );
        }
    }
    let popup_id = egui::Id::new("workspaces-popup");
    if ws_resp.clicked() || wresp.clicked() {
        ui.ctx().data_mut(|d| d.insert_temp(popup_id, true));
    }
    let open = ui.ctx().data(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));
    if open {
        let anchor = pos2(wr.min.x, rect.max.y + 4.0);
        let area = egui::Area::new(popup_id.with("area")).order(egui::Order::Foreground).fixed_pos(anchor).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(220.0);
                for w in crate::dock::names(&app.workspaces) {
                    let sel = app.ui.workspace == w;
                    if ui.selectable_label(sel, crate::i18n::t(&w)).clicked() {
                        app.set_workspace(&w);
                        ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
                    }
                }
                ui.separator();
                if ui.button(tl!("Reset to Saved Layout")).clicked() {
                    let n = app.ui.workspace.clone();
                    app.set_workspace(&n);
                    ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
                }
            });
        });
        if area.response.clicked_elsewhere() && !(ws_resp.clicked() || wresp.clicked()) {
            ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
        }
    }
}

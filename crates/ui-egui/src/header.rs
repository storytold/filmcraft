//! The header bar (Premiere 26 layout, 38 pt): Home and Import / Edit / Export on the left, the
//! document title centred, the workspace name and quick actions on the right. On macOS the menus
//! live in the native menu bar (set up by the app); elsewhere a compact in-window menu bar follows
//! the mode tabs.

use egui::{Color32, Rect, Sense, Stroke, pos2, vec2};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::Mode;
use crate::theme::Tokens;

/// Height of the header bar. With `integrated_titlebar` it sits in the macOS title-bar strip.
pub const HEIGHT: f32 = 38.0;

/// How much of the header the menu bar may use and what the right cluster gives up for
/// it (#4891). The cluster's left edge sits 270 pt plus the workspace name and the
/// Discord button left of the header's right edge (seven quick actions at a 38 pt
/// pitch, then the name, then Discord, each with its gap). The bar's usual allocation
/// is 520 pt from the end of the mode tabs; when that would come within 12 pt of the
/// cluster, the Discord button drops its label, and when the bar would still be under
/// 240 pt the workspace name drops its space. Without an in-window menu bar nothing
/// gives anything up.
struct MenuSpace {
    /// The menu bar's right edge.
    right: f32,
    /// The Discord button's width this frame.
    discord_w: f32,
    discord_icon_only: bool,
    hide_ws_name: bool,
}

fn menu_space(show_menu_bar: bool, tabs_end: f32, header_right: f32, name_w: f32, discord_w: f32) -> MenuSpace {
    let mut space = MenuSpace { right: tabs_end + 520.0, discord_w, discord_icon_only: false, hide_ws_name: false };
    if !show_menu_bar {
        return space;
    }
    let mut name_w = name_w;
    if tabs_end + 532.0 > header_right - 284.0 - name_w - space.discord_w {
        space.discord_icon_only = true;
        space.discord_w = 28.0;
    }
    if tabs_end + 246.0 > header_right - 296.0 - name_w - space.discord_w {
        space.hide_ws_name = true;
        name_w = 0.0;
    }
    space.right = (header_right - 296.0 - name_w - space.discord_w).min(tabs_end + 520.0);
    space
}

/// The Discord button in `r`: the chat icon and, unless the header is tight (#4891),
/// the label beside it.
fn discord_button(app: &mut FilmcraftApp, ui: &mut egui::Ui, r: Rect, icon_only: bool, dg: std::sync::Arc<egui::Galley>, t: Tokens) -> egui::Response {
    let resp = ui.interact(r, egui::Id::unique("hdr-discord"), Sense::click()).on_hover_text(tl!("Join the ArtCraft Discord (discord.gg/artcraft)"));
    app.auto.add("header.discord", r, "Join the ArtCraft Discord");
    ui.painter().rect_filled(r, 12.0, if resp.hovered() { t.accent_hover } else { t.accent });
    if icon_only {
        icons::paint(ui.painter(), Rect::from_center_size(r.center(), vec2(14.0, 14.0)), Icon::Chat, Color32::WHITE);
    } else {
        icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 14.0, r.center().y), vec2(14.0, 14.0)), Icon::Chat, Color32::WHITE);
        ui.painter().galley(pos2(r.min.x + 25.0, r.center().y - dg.size().y / 2.0), dg, Color32::WHITE);
    }
    resp
}

/// Draws the header bar in `rect` and handles its clicks: Home, the mode tabs, the workspace name
/// and its menu (the workspaces, then Reset to Saved Layout, Save Changes to this Workspace, Save as
/// New Workspace… and Edit Workspaces…), Discord and the quick actions.
pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let p = ui.painter().clone();
    p.rect_filled(rect, 0.0, t.header_bg);
    p.line_segment([rect.left_bottom(), rect.right_bottom()], Stroke::new(1.0, Color32::BLACK));
    let drag = ui.interact(rect, egui::Id::unique("header-drag"), Sense::click_and_drag());
    if drag.drag_started_by(egui::PointerButton::Primary) {
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
    }
    if drag.double_clicked() {
        let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max));
    }
    let mut x = rect.min.x + if app.integrated_titlebar { 104.0 } else { 14.0 };
    // Home
    let home = Rect::from_center_size(pos2(x + 10.0, rect.center().y), vec2(26.0, 26.0));
    let hresp = ui.interact(home, egui::Id::unique("hdr-home"), Sense::click()).on_hover_text(tl!("Home"));
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
        let resp = ui.interact(r, egui::Id::unique(("mode", label)), Sense::click());
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
    // The right cluster's extent is known before anything there is drawn: seven quick
    // actions at a 38 pt pitch, the workspace name, then the Discord button. The menu bar
    // gets what is left of the header, never the cluster's space (#4891): on a narrow
    // window the fixed 520 pt allocation used to run underneath the cluster, and the
    // menus and the buttons painted over each other. When the full allocation does not
    // fit, the Discord button gives up its label first, then the workspace name gives up
    // its space; the bar itself overflows in menus::menu_bar.
    let ws = crate::i18n::to_uppercase(crate::i18n::t(&app.ui.workspace));
    let wg = p.layout_no_wrap(ws.clone(), Tokens::ui(11.0), t.text_dim);
    let dg = p.layout_no_wrap("Discord".to_string(), Tokens::ui(12.0), Color32::WHITE);
    let space = menu_space(app.ui.show_menu_bar, x, rect.max.x, wg.size().x, dg.size().x + 34.0);
    let discord_w = space.discord_w;
    let discord_icon_only = space.discord_icon_only;
    let hide_ws_name = space.hide_ws_name;
    let name_w = if hide_ws_name { 0.0 } else { wg.size().x };
    let menu_right = space.right;
    // In-window menus when there is no native menu bar.
    if app.ui.show_menu_bar {
        let menu_rect = Rect::from_min_max(pos2(x + 6.0, rect.min.y + 7.0), pos2(menu_right, rect.max.y - 7.0));
        let mut mu = ui.new_child(egui::UiBuilder::new().max_rect(menu_rect).layout(egui::Layout::left_to_right(egui::Align::Center)));
        mu.style_mut().visuals.widgets.inactive.weak_bg_fill = Color32::TRANSPARENT;
        mu.style_mut().visuals.widgets.inactive.bg_stroke = Stroke::NONE;
        mu.style_mut().visuals.override_text_color = Some(t.text_dim);
        crate::menus::menu_bar(app, &mut mu);
        left_end = mu.min_rect().max.x;
    }
    // Document title, centred
    let title = if app.session.is_dirty() { tlf!("{name} - Edited", name = app.session.project.name) } else { app.session.project.name.clone() };
    // Right cluster: icons at ~38 pt pitch, then workspace name in caps.
    let mut rx = rect.max.x - 14.0;
    let mut btn = |ui: &mut egui::Ui, icon: Icon, id: &str, tip: &str, app: &mut FilmcraftApp| -> egui::Response {
        let r = Rect::from_center_size(pos2(rx - 14.0, rect.center().y), vec2(28.0, 28.0));
        rx -= 38.0;
        let resp = ui.interact(r, egui::Id::unique(("hdr", id)), Sense::click()).on_hover_text(tip);
        app.auto.add(&format!("header.{id}"), r, tip);
        if resp.hovered() {
            ui.painter().rect_filled(r, 4.0, t.hover);
        }
        icons::paint(ui.painter(), r.shrink(6.0), icon, if resp.hovered() { t.text } else { t.text_dim });
        resp
    };
    if btn(ui, Icon::Fullscreen, "fullscreen", tl!("Full screen"), app).clicked() {
        let fs = ui.ctx().input(|i| i.viewport().fullscreen.unwrap_or(false));
        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fs));
    }
    if btn(ui, Icon::Speaker, "volume", tl!("Volume"), app).clicked() {
        app.ui.status = tl!("Master volume: use the Audio Track Mixer").into();
    }
    if btn(ui, Icon::Search, "search", tl!("Search"), app).clicked() {
        app.show_panel(crate::dock::PanelKind::Effects);
    }
    if btn(ui, Icon::Bell, "notifications", tl!("Progress"), app).clicked() {
        app.ui.mode = Mode::Export;
    }
    // Quick Export: a popup with File Name & Location, a preset list and Export (Premiere 26)
    let qx = rect.max.x - 14.0 - 4.0 * 38.0; // the fifth button from the right
    if btn(ui, Icon::Export, "quickExport", tl!("Quick Export"), app).clicked() {
        app.ui.export.quick_open = !app.ui.export.quick_open;
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::unique("quick-export-toggled"), true));
    }
    crate::panels::export_mode::quick_export(app, ui.ctx(), pos2(qx - 340.0, rect.max.y + 4.0));
    // Appearance Mode: click for the next one (Auto, Light, Dark); the icon shows the current one.
    let (icon, mode) = match app.session.prefs.appearance.appearance_mode.as_str() {
        "auto" => (Icon::Monitor, tl!("Sync with system")),
        "light" => (Icon::Sun, tl!("Light")),
        _ => (Icon::Moon, tl!("Dark")),
    };
    let tip = format!("{}: {mode}", tl!("Appearance Mode"));
    let appearance = btn(ui, icon, "appearance", &tip, app);
    appearance.context_menu(|ui| crate::panels::settings::appearance_context_menu(app, ui));
    if appearance.clicked() {
        crate::panels::settings::cycle_appearance(app, ui.ctx());
    }
    let ws_resp = btn(ui, Icon::Workspaces, "workspaces", tl!("Workspaces"), app);
    // workspace name (caps); on a narrow window it gives up its space to the menu bar
    // (#4891) and only the Workspaces button remains
    let wr = Rect::from_min_size(pos2(rx - name_w + 10.0, rect.center().y - 10.0), vec2(name_w + 8.0, 20.0));
    let wresp = if hide_ws_name {
        None
    } else {
        let resp = ui.interact(wr, egui::Id::unique("hdr-ws-name"), Sense::click());
        app.auto.add("header.workspaceName", wr, &ws);
        p.galley_with_override_text_color(
            pos2(wr.min.x + 4.0, rect.center().y - wg.size().y / 2.0),
            wg.clone(),
            if resp.hovered() { t.text } else { t.text_dim },
        );
        Some(resp)
    };
    let wresp_clicked = wresp.as_ref().is_some_and(|r| r.clicked());
    // Community: a labelled Discord button, always one click away; icon only when the
    // header is tight (#4891)
    {
        let r = Rect::from_min_size(pos2(wr.min.x - discord_w - 14.0, rect.center().y - 12.0), vec2(discord_w, 24.0));
        let resp = discord_button(app, ui, r, discord_icon_only, dg.clone(), t);
        // Localized menus and user titles can be wider than English. Use the actual gap.
        let left = left_end + 12.0;
        let right = r.min.x - 12.0;
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

        if resp.clicked() {
            crate::links::open(app, ui.ctx(), crate::links::DISCORD);
        }
    }
    let popup_id = egui::Id::unique("workspaces-popup");
    if ws_resp.clicked() || wresp_clicked {
        ui.ctx().data_mut(|d| d.insert_temp(popup_id, true));
    }
    let open = ui.ctx().data(|d| d.get_temp::<bool>(popup_id).unwrap_or(false));
    if open {
        let anchor = pos2(wr.min.x, rect.max.y + 4.0);
        let area = egui::Area::new(popup_id.with("area")).order(egui::Order::Foreground).fixed_pos(anchor).show(ui.ctx(), |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.with_layout(egui::Layout::top_down_justified(egui::Align::Min), |ui| {
                    ui.set_min_width(220.0);
                    for w in crate::dock::names(&app.workspaces) {
                        let sel = app.ui.workspace == w;
                        if ui.selectable_label(sel, crate::i18n::t(&w)).clicked() {
                            app.set_workspace(&w);
                            ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
                        }
                    }
                    ui.separator();
                    // the actions of Window ▸ Workspaces, as in Premiere's workspace menu
                    let actions = [
                        ("reset", tl!("Reset to Saved Layout")),
                        ("saveChanges", tl!("Save Changes to this Workspace")),
                        ("saveAs", tl!("Save as New Workspace…")),
                        ("edit", tl!("Edit Workspaces…")),
                    ];
                    for (action, label) in actions {
                        let resp = ui.button(label);
                        app.auto.add(&format!("header.workspaces.{action}"), resp.rect, label);
                        if resp.clicked() {
                            ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
                            let ctx = ui.ctx().clone();
                            if let Err(e) = crate::menus::invoke(app, &ctx, &format!("window.workspace.{action}"), serde_json::json!({})) {
                                app.ui.status = e;
                            }
                        }
                    }
                });
            });
        });
        if area.response.clicked_elsewhere() && !(ws_resp.clicked() || wresp_clicked) {
            ui.ctx().data_mut(|d| d.insert_temp(popup_id, false));
        }
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn workspace_name_uppercasing_is_turkish_aware() {
        crate::i18n::set_current(crate::i18n::Language::Tr);
        assert_eq!(crate::i18n::to_uppercase("Birleştirme"), "BİRLEŞTİRME");
        assert_eq!(crate::i18n::to_uppercase("Altyazılar ve Grafikler"), "ALTYAZILAR VE GRAFİKLER");
        crate::i18n::set_current(crate::i18n::Language::En);
        assert_eq!(crate::i18n::to_uppercase("Birleştirme"), "BIRLEŞTIRME");
    }
}

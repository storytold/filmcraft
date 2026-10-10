//! The header bar (Premiere 26 layout, 38 pt): Home and Import / Edit / Export on the left, the
//! document title centred, the workspace name and quick actions on the right. On macOS the menus
//! live in the native menu bar (set up by the app); elsewhere a compact in-window menu bar follows
//! the mode tabs. On Linux the header is also the window's title bar: the window is undecorated,
//! the app icon leads the bar ([`crate::brand::paint_mark`]) and this row carries the
//! minimize / maximize / close caption buttons itself ([`window_controls`]).

use egui::{Color32, Rect, Sense, Stroke, pos2, vec2};

use crate::FilmcraftApp;
use crate::icons::{self, Icon};
use crate::state::Mode;
use crate::theme::Tokens;

/// One caption button's width, as wide as the native Windows ones (PhotoCraft's title bar).
const CAPTION_W: f32 = 46.0;

#[derive(Clone, Copy, PartialEq, Debug)]
pub enum Caption {
    Minimize,
    Maximize,
    Close,
}

/// Left to right. Minimize is X11-only: on Wayland `xdg_toplevel.set_minimize` is a request the
/// compositor is free to ignore (GNOME and the tiling WMs do), so the button would silently do
/// nothing there and is not offered.
const CAPTIONS: [Caption; 3] = [Caption::Minimize, Caption::Maximize, Caption::Close];
const CAPTIONS_WAYLAND: [Caption; 2] = [Caption::Maximize, Caption::Close];

/// The captions shown: `wayland` drops Minimize.
fn shown(wayland: bool) -> &'static [Caption] {
    if wayland { &CAPTIONS_WAYLAND } else { &CAPTIONS }
}

/// The caption row's width: the rightmost points of the bar it takes (`wayland` drops Minimize).
pub fn captions_w(wayland: bool) -> f32 {
    shown(wayland).len() as f32 * CAPTION_W
}

/// Caption button ids (stable, so tests and agents can find them).
pub fn caption_id(c: Caption) -> egui::Id {
    egui::Id::new(("hdr-win", c as u8))
}

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
    // The app icon leads the bar where the OS bar is gone (Linux): where PhotoCraft's title bar
    // shows its mark, here the FilmCraft tile stands in for the icon the OS title bar carried.
    if app.window_controls {
        let mark = Rect::from_center_size(pos2(x + 10.0, rect.center().y), vec2(20.0, 20.0));
        crate::brand::paint_mark(ui, mark);
        app.auto.add("header.brandMark", mark, "FilmCraft");
        x += 26.0;
    }
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
    // Right cluster: icons at ~38 pt pitch, then workspace name in caps. The caption buttons of
    // an undecorated window (Linux) take the rightmost [`captions_w`] points.
    let wc = if app.window_controls { captions_w(app.wayland) } else { 0.0 };
    let mut rx = rect.max.x - 14.0 - wc;
    let mut btn = |ui: &mut egui::Ui, icon: Icon, id: &str, tip: &str, app: &mut FilmcraftApp| -> egui::Response {
        let r = Rect::from_center_size(pos2(rx - 14.0, rect.center().y), vec2(28.0, 28.0));
        rx -= 38.0;
        let resp = ui.interact(r, egui::Id::new(("hdr", id)), Sense::click()).on_hover_text(tip);
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
    let qx = rect.max.x - 14.0 - 4.0 * 38.0 - wc; // the fifth button from the right
    if btn(ui, Icon::Export, "quickExport", tl!("Quick Export"), app).clicked() {
        app.ui.export.quick_open = !app.ui.export.quick_open;
        ui.ctx().data_mut(|d| d.insert_temp(egui::Id::new("quick-export-toggled"), true));
    }
    crate::panels::export_mode::quick_export(app, ui.ctx(), pos2(qx - 340.0, rect.max.y + 4.0));
    let ws_resp = btn(ui, Icon::Workspaces, "workspaces", tl!("Workspaces"), app);
    // workspace name (caps)
    let ws = crate::i18n::t(&app.ui.workspace).to_uppercase();
    let wg = p.layout_no_wrap(ws.clone(), Tokens::ui(11.0), t.text_dim);
    let wr = Rect::from_min_size(pos2(rx - wg.size().x + 10.0, rect.center().y - 10.0), vec2(wg.size().x + 8.0, 20.0));
    let wresp = ui.interact(wr, egui::Id::new("hdr-ws-name"), Sense::click());
    app.auto.add("header.workspaceName", wr, &ws);
    p.galley_with_override_text_color(pos2(wr.min.x + 4.0, rect.center().y - wg.size().y / 2.0), wg, if wresp.hovered() { t.text } else { t.text_dim });
    // Community: a labelled Discord button, always one click away.
    {
        let label = "Discord";
        let g = p.layout_no_wrap(label.to_string(), Tokens::ui(12.0), Color32::WHITE);
        let w = g.size().x + 34.0;
        let r = Rect::from_min_size(pos2(wr.min.x - w - 14.0, rect.center().y - 12.0), vec2(w, 24.0));
        let resp = ui.interact(r, egui::Id::new("hdr-discord"), Sense::click()).on_hover_text(tl!("Join the ArtCraft Discord (discord.gg/artcraft)"));
        app.auto.add("header.discord", r, "Join the ArtCraft Discord");
        ui.painter().rect_filled(r, 12.0, if resp.hovered() { t.accent_hover } else { t.accent });
        icons::paint(ui.painter(), Rect::from_center_size(pos2(r.min.x + 14.0, r.center().y), vec2(14.0, 14.0)), Icon::Chat, Color32::WHITE);
        ui.painter().galley(pos2(r.min.x + 25.0, r.center().y - g.size().y / 2.0), g, Color32::WHITE);
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
            crate::links::open(ui.ctx(), crate::links::DISCORD);
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
    if app.window_controls {
        window_controls(app, ui, rect);
    }
}

/// The window buttons of an undecorated window (`window_controls`, Linux): minimize (X11 only —
/// Wayland compositors may ignore it, see [`CAPTIONS`]), maximize / restore, close at the right
/// end of the header, full height, flush with the window's edge — the classic caption row, as in
/// PhotoCraft's title bar (`titlebar.rs` there; the glyphs and the 46 pt targets are the same).
/// Close hovers red, the Windows / Premiere convention; the rest of the bar keeps dragging and
/// double-click maximizes (`show`).
fn window_controls(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let t = app.tokens;
    let max = ui.ctx().input(|i| i.viewport().maximized.unwrap_or(false));
    let right = rect.right().min(ui.ctx().content_rect().right());
    let cw = captions_w(app.wayland);
    let mut clicked = None;
    for (i, c) in shown(app.wayland).iter().copied().enumerate() {
        let r = Rect::from_min_size(pos2(right - cw + i as f32 * CAPTION_W, rect.top()), vec2(CAPTION_W, rect.height()));
        let resp = ui.interact(r, caption_id(c), Sense::click());
        let close = c == Caption::Close;
        let fill = match (resp.is_pointer_button_down_on(), resp.hovered()) {
            (true, _) if close => t.caption_close.gamma_multiply(0.8),
            (_, true) if close => t.caption_close,
            (true, _) => t.pressed,
            (_, true) => t.hover,
            _ => Color32::TRANSPARENT,
        };
        ui.painter().rect_filled(r, 0.0, fill);
        let ink = if close && resp.hovered() { t.caption_close_text } else { t.icon };
        paint_caption_glyph(ui, c, max, r.center(), Stroke::new(1.0, ink));
        let tip = match c {
            Caption::Minimize => tl!("Minimize"),
            Caption::Maximize if max => tl!("Restore"),
            Caption::Maximize => tl!("Maximize"),
            Caption::Close => tl!("Close"),
        };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip));
        let id = match c {
            Caption::Minimize => "minimize",
            Caption::Maximize => "maximize",
            Caption::Close => "close",
        };
        app.auto.add(&format!("header.window.{id}"), r, tip);
        if resp.on_hover_text(tip).clicked() {
            clicked = Some(c);
        }
    }
    match clicked {
        Some(Caption::Minimize) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Minimized(true)),
        Some(Caption::Maximize) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Maximized(!max)),
        // FilmCraft's close is the window manager's close: auto-save and crash recovery keep the
        // work (there is no unsaved-changes dialog to route through, as PhotoCraft's Exit has).
        Some(Caption::Close) => ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close),
        None => {}
    }
}

/// The 10 pt line glyphs of the caption buttons, PhotoCraft's (`titlebar.rs::paint_glyph` there).
fn paint_caption_glyph(ui: &egui::Ui, c: Caption, maximized: bool, at: egui::Pos2, s: Stroke) {
    let p = ui.painter();
    let g = Rect::from_center_size(at, vec2(10.0, 10.0));
    match c {
        Caption::Minimize => {
            p.line_segment([g.left_center(), g.right_center()], s);
        }
        // Restore: a front square with the corner of the one behind it.
        Caption::Maximize if maximized => {
            let front = Rect::from_min_max(g.min + vec2(0.0, 2.0), g.max - vec2(2.0, 0.0));
            p.rect_stroke(front, 0.0, s, egui::StrokeKind::Middle);
            p.line(vec![g.min + vec2(2.0, 2.0), g.min + vec2(2.0, 0.0), g.right_top(), g.max - vec2(0.0, 2.0), g.max - vec2(2.0, 2.0)], s);
        }
        Caption::Maximize => {
            p.rect_stroke(g, 0.0, s, egui::StrokeKind::Middle);
        }
        Caption::Close => {
            p.line_segment([g.left_top(), g.right_bottom()], s);
            p.line_segment([g.right_top(), g.left_bottom()], s);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, PointerButton, Pos2, RawInput, ViewportCommand, ViewportId};
    use filmcraft_engine::Session;

    /// A headless window `width` pt wide showing the header and the resize zones of the
    /// undecorated window (`window_controls` on), as PhotoCraft's title bar tests do; eframe's
    /// behaviour is upstream, so these tests lock in what our layer emits.
    struct Win {
        ctx: egui::Context,
        app: FilmcraftApp,
        width: f32,
        time: f64,
        maximized: bool,
    }

    impl Win {
        fn new(width: f32) -> Self {
            let ctx = egui::Context::default();
            let mut app = FilmcraftApp::new(Session::default());
            app.window_controls = true;
            Self { ctx, app, width, time: 0.0, maximized: false }
        }

        fn frame(&mut self, events: Vec<Event>) -> egui::FullOutput {
            self.time += 0.05;
            let mut input =
                RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(self.width, 600.0))), time: Some(self.time), events, ..Default::default() };
            if let Some(v) = input.viewports.get_mut(&ViewportId::ROOT) {
                v.maximized = Some(self.maximized);
            }
            let app = &mut self.app;
            let mut out = self.ctx.run_ui(input, |ui| {
                let full = ui.max_rect();
                show(app, ui, Rect::from_min_size(full.min, vec2(full.width(), 38.0)));
                app.edge_resize(ui, 38.0);
            });
            out.textures_delta.clear();
            out
        }

        fn commands(&mut self, events: Vec<Event>) -> Vec<ViewportCommand> {
            self.frame(events).viewport_output.remove(&ViewportId::ROOT).map(|v| v.commands).unwrap_or_default()
        }

        fn click(&mut self, p: Pos2) -> Vec<ViewportCommand> {
            let btn = |pressed| Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Default::default() };
            let mut cmds = self.commands(vec![Event::PointerMoved(p)]);
            cmds.extend(self.commands(vec![btn(true), btn(false)]));
            cmds
        }

        fn caption(&self, c: Caption) -> Rect {
            self.ctx.read_response(caption_id(c)).map(|r| r.rect).unwrap_or(Rect::NOTHING)
        }

        fn rect_of(&self, id: &str) -> Option<Rect> {
            self.app.auto.find(id).map(|e| Rect::from_min_size(pos2(e.rect[0], e.rect[1]), vec2(e.rect[2], e.rect[3])))
        }

        /// The bar's registered widgets other than the caption buttons (for overlap checks).
        fn bar_widgets(&self) -> Vec<(&str, Rect)> {
            self.app
                .auto
                .elements
                .iter()
                .filter(|e| e.id.starts_with("header.") && !e.id.starts_with("header.window."))
                .filter_map(|e| self.rect_of(&e.id).map(|r| (e.id.as_str(), r)))
                .collect()
        }
    }

    /// The caption buttons take the right end of the bar (flush, full height, 46 pt each),
    /// nothing runs into them, and the app icon leads the bar where the OS bar is gone.
    #[test]
    fn caption_buttons_take_the_right_end_and_the_icon_leads_the_bar() {
        for width in [900.0, 1440.0, 1920.0] {
            let mut w = Win::new(width);
            w.frame(vec![]);
            w.frame(vec![]);
            let [min, max, close] = CAPTIONS.map(|c| w.caption(c));
            assert_eq!((close.right(), close.top()), (width, 0.0), "Close sits in the window's corner at {width}");
            assert_eq!(close.height(), 38.0);
            assert!([min, max, close].iter().all(|r| r.width() == CAPTION_W));
            assert_eq!((min.right(), max.right()), (max.left(), close.left()));
            for (id, r) in w.bar_widgets() {
                assert!(r.right() <= min.left(), "{id} {r:?} runs into the caption buttons at {width}");
            }
            let mark = w.rect_of("header.brandMark").expect("the brand mark is painted");
            assert!(mark.left() < 20.0 && mark.width() >= 16.0, "{mark:?}");
            let home = w.rect_of("header.home").expect("the Home button");
            assert!(mark.right() < home.left(), "the mark sits left of Home: {mark:?} {home:?}");
        }
    }

    #[test]
    fn minimize_maximize_and_close_follow_the_window_state() {
        let mut w = Win::new(1100.0);
        w.frame(vec![]);
        w.frame(vec![]);
        let (min, max) = (w.caption(Caption::Minimize).center(), w.caption(Caption::Maximize).center());
        assert!(w.click(min).contains(&ViewportCommand::Minimized(true)));
        assert!(w.click(max).contains(&ViewportCommand::Maximized(true)));
        w.maximized = true;
        let cmds = w.click(max);
        assert!(cmds.contains(&ViewportCommand::Maximized(false)), "Restore: {cmds:?}");
        assert!(w.click(w.caption(Caption::Close).center()).contains(&ViewportCommand::Close));
    }

    /// The rest of the bar keeps its window duties: a drag in the free gap moves the window, a
    /// double-click there maximizes it, and neither touches the caption buttons' commands.
    #[test]
    fn empty_bar_space_drags_and_double_click_maximizes() {
        let mut w = Win::new(1440.0);
        w.frame(vec![]);
        w.frame(vec![]);
        // Between the menu bar's right end and the Discord button: the free gap.
        let empty = pos2(850.0, 19.0);
        let covering: Vec<&str> = w
            .app
            .auto
            .elements
            .iter()
            .filter(|e| e.id != "header.brandMark")
            .filter(|e| w.rect_of(&e.id).is_some_and(|r| r.contains(empty)))
            .map(|e| e.id.as_str())
            .collect();
        assert!(covering.is_empty(), "the probe point must be empty bar space, covered by {covering:?}");
        let btn = |pressed, p| Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        let mut cmds = w.commands(vec![Event::PointerMoved(empty)]);
        cmds.extend(w.commands(vec![btn(true, empty)]));
        cmds.extend(w.commands(vec![Event::PointerMoved(empty + vec2(12.0, 4.0))]));
        cmds.extend(w.commands(vec![btn(false, empty + vec2(12.0, 4.0))]));
        assert!(cmds.contains(&ViewportCommand::StartDrag), "{cmds:?}");
        assert!(!cmds.iter().any(|c| matches!(c, ViewportCommand::Close | ViewportCommand::Minimized(_))), "{cmds:?}");
        let mut cmds = w.click(empty);
        cmds.extend(w.click(empty));
        assert!(cmds.contains(&ViewportCommand::Maximized(true)), "{cmds:?}");
        assert!(!cmds.iter().any(|c| matches!(c, ViewportCommand::Minimized(_) | ViewportCommand::Close)), "{cmds:?}");
    }

    /// The caption buttons own their strip: the resize zones start below the header, so every
    /// point of the Close button closes — a corner click must not begin a resize — while the
    /// side zones below the header still resize.
    #[test]
    fn caption_clicks_win_over_the_resize_zones() {
        let mut w = Win::new(1440.0);
        w.frame(vec![]);
        w.frame(vec![]);
        let close = w.caption(Caption::Close);
        for p in [close.right_top() + vec2(-1.0, 1.0), close.left_top() + vec2(1.0, 1.0), close.center()] {
            let cmds = w.click(p);
            assert!(cmds.contains(&ViewportCommand::Close), "no close from {p:?}: {cmds:?}");
            assert!(!cmds.iter().any(|c| matches!(c, ViewportCommand::BeginResize(_))), "a resize began at {p:?}: {cmds:?}");
        }
        let p = pos2(w.width - 2.0, 60.0);
        let btn = |pressed| Event::PointerButton { pos: p, button: PointerButton::Primary, pressed, modifiers: Default::default() };
        let mut cmds = w.commands(vec![Event::PointerMoved(p), btn(true)]);
        cmds.extend(w.commands(vec![Event::PointerMoved(p + vec2(-12.0, 4.0))]));
        cmds.extend(w.commands(vec![btn(false)]));
        assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::BeginResize(_))), "the side zone below the header still resizes: {cmds:?}");
    }

    /// Wayland drops Minimize (`xdg_toplevel.set_minimize` is a request the compositor may
    /// ignore — GNOME and the tiling WMs do): Maximize and Close keep the right end.
    #[test]
    fn wayland_drops_minimize() {
        let mut w = Win::new(1100.0);
        w.app.wayland = true;
        w.frame(vec![]);
        w.frame(vec![]);
        assert_eq!(w.caption(Caption::Minimize), Rect::NOTHING, "no Minimize button under Wayland");
        let (max, close) = (w.caption(Caption::Maximize), w.caption(Caption::Close));
        assert_eq!((close.right(), close.top()), (1100.0, 0.0));
        assert_eq!(max.right(), close.left());
        assert!(w.click(max.center()).contains(&ViewportCommand::Maximized(true)));
        assert!(w.click(close.center()).contains(&ViewportCommand::Close));
    }
}

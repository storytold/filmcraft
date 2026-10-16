//! The window's own title bar on Windows and Linux, where the window has no OS decorations
//! ([`FilmcraftApp::custom_titlebar`]): the app's top bar (`header::title_bar`) is the title bar.
//! The free gap between its menus and controls drags the window and a double-click there
//! maximizes it; caption buttons at the far right minimize, maximize/restore and close, and thin
//! invisible zones along the window's edges resize it.

use egui::{CursorIcon, Id, LayerId, Order, PointerButton, Rect, ResizeDirection, Sense, Stroke, Ui, ViewportCommand, pos2, vec2};
use serde_json::json;

use crate::FilmcraftApp;

/// One caption button: as wide as the native Windows ones.
pub const BUTTON_WIDTH: f32 = 46.0;
/// The three caption buttons together.
pub const WIDTH: f32 = 3.0 * BUTTON_WIDTH;
/// Resize zone thickness along an edge, and the side of a corner zone.
const EDGE: f32 = 8.0;
const CORNER: f32 = 12.0;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Caption {
    Minimize,
    Maximize,
    Close,
}

/// Left to right.
const CAPTIONS: [Caption; 3] = [Caption::Minimize, Caption::Maximize, Caption::Close];

/// Caption button ids (stable, so tests and agents can find them).
fn caption_id(c: Caption) -> Id {
    Id::new(("titlebar-caption", c as u8))
}

fn maximized(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().maximized.unwrap_or(false))
}

fn toggle_maximize(ctx: &egui::Context) {
    ctx.send_viewport_cmd(ViewportCommand::Maximized(!maximized(ctx)));
}

/// Minimize, Maximize/Restore and Close at the right end of `bar` (the title bar's outer rect),
/// full bar height, flush with the window's edge (even if the bar's contents run wider) so a
/// maximized window's corner hits Close. Close runs `app.quit`, which asks about unsaved
/// changes first (`intercept_quit`).
pub fn caption_buttons(app: &mut FilmcraftApp, ui: &mut Ui, bar: Rect) {
    let t = app.tokens;
    let max = maximized(ui.ctx());
    let right = bar.right().min(ui.ctx().content_rect().right());
    let mut clicked = None;
    for (i, c) in CAPTIONS.into_iter().enumerate() {
        let r = Rect::from_min_size(pos2(right - WIDTH + i as f32 * BUTTON_WIDTH, bar.top()), vec2(BUTTON_WIDTH, bar.height() - 1.0));
        let resp = ui.interact(r, caption_id(c), Sense::click());
        let close = c == Caption::Close;
        let fill = match (resp.is_pointer_button_down_on(), resp.hovered()) {
            (true, _) if close => t.danger.gamma_multiply(0.8),
            (_, true) if close => t.danger,
            (true, _) => t.pressed,
            (_, true) => t.hover,
            _ => egui::Color32::TRANSPARENT,
        };
        ui.painter().rect_filled(r, 0.0, fill);
        let ink = if close && resp.hovered() { egui::Color32::WHITE } else { t.icon };
        paint_glyph(ui, c, max, r.center(), Stroke::new(1.0, ink));
        let tip = match c {
            Caption::Minimize => tl!("Minimize"),
            Caption::Maximize if max => tl!("Restore"),
            Caption::Maximize => tl!("Maximize"),
            Caption::Close => tl!("Close"),
        };
        resp.widget_info(|| egui::WidgetInfo::labeled(egui::WidgetType::Button, true, tip));
        if resp.on_hover_text(tip).clicked() {
            clicked = Some(c);
        }
    }
    match clicked {
        Some(Caption::Minimize) => ui.ctx().send_viewport_cmd(ViewportCommand::Minimized(true)),
        Some(Caption::Maximize) => toggle_maximize(ui.ctx()),
        Some(Caption::Close) => {
            let ctx = ui.ctx().clone();
            let _ = crate::menus::invoke(app, &ctx, "app.quit", json!({}));
        }
        None => {}
    }
}

/// The 10 pt line glyphs of the caption buttons.
fn paint_glyph(ui: &Ui, c: Caption, maximized: bool, at: egui::Pos2, s: Stroke) {
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

/// Whether the window shows the system's title bar instead of the app drawing its own: Windows
/// and Linux with Settings ▸ Appearance ▸ System Title Bar. macOS always has system decorations,
/// but keeps the in-app brand mark and centred title, so it never counts as system mode here; nor
/// does the web build (the browser tab has no document title).
pub fn system_title_bar(app: &FilmcraftApp) -> bool {
    !app.custom_titlebar && app.session.prefs.appearance.system_title_bar && !cfg!(any(target_os = "macos", target_arch = "wasm32"))
}

/// The OS window title: the project name suffixed with the app name (a `*` prefix marks unsaved
/// changes, like the in-app title), or just the app name with no project open.
pub fn window_title(app: &FilmcraftApp) -> String {
    let name = app.session.project.name.clone();
    if name.is_empty() {
        return "FilmCraft".into();
    }
    format!("{}{} — FilmCraft", if app.session.is_dirty() { "*" } else { "" }, name)
}

/// Keep the OS window title (and the taskbar / Alt-Tab entry) on the project. Sends
/// `ViewportCommand::Title` only when the title changed since the last frame.
pub fn sync_window_title(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let want = window_title(app);
    if app.last_window_title != want {
        app.last_window_title = want.clone();
        ctx.send_viewport_cmd(egui::ViewportCommand::Title(want));
    }
}

/// The edge and corner zones of a window with content rect `w`: (zone, direction, cursor). Corners
/// come last so they win where they overlap the edges.
fn zones(w: Rect) -> [(Rect, ResizeDirection, CursorIcon); 8] {
    use ResizeDirection::*;
    let (e, c) = (EDGE, CORNER);
    [
        (Rect::from_min_max(w.left_top(), pos2(w.right(), w.top() + e)), North, CursorIcon::ResizeNorth),
        (Rect::from_min_max(pos2(w.left(), w.bottom() - e), w.right_bottom()), South, CursorIcon::ResizeSouth),
        (Rect::from_min_max(w.left_top(), pos2(w.left() + e, w.bottom())), West, CursorIcon::ResizeWest),
        (Rect::from_min_max(pos2(w.right() - e, w.top()), w.right_bottom()), East, CursorIcon::ResizeEast),
        (Rect::from_min_size(w.left_top(), vec2(c, c)), NorthWest, CursorIcon::ResizeNwSe),
        (Rect::from_min_size(w.right_top() - vec2(c, 0.0), vec2(c, c)), NorthEast, CursorIcon::ResizeNeSw),
        (Rect::from_min_size(w.left_bottom() - vec2(0.0, c), vec2(c, c)), SouthWest, CursorIcon::ResizeNeSw),
        (Rect::from_min_size(w.right_bottom() - vec2(c, c), vec2(c, c)), SouthEast, CursorIcon::ResizeNwSe),
    ]
}

fn resize_handed_off_id() -> Id {
    Id::new("titlebar-resize-handed-off")
}

/// An OS resize started at `pass`; `released` is the pass whose input ended the press
/// ([`release_after_os_resize`]).
#[derive(Clone, Copy, Debug)]
struct HandOff {
    pass: u64,
    released: Option<u64>,
}

/// The compositor takes the button release of an OS resize (KDE Wayland), so egui kept the edge
/// pressed: the zones stopped hovering until the next click. End the press ourselves on the input
/// after the hand-off, but only while egui still has it down and this input brings no release of
/// its own. Call from `raw_input_hook`.
pub fn release_after_os_resize(ctx: &egui::Context, raw: &mut egui::RawInput) {
    let Some(h) = ctx.data(|d| d.get_temp::<HandOff>(resize_handed_off_id())) else { return };
    if h.released.is_some() || h.pass >= ctx.cumulative_pass_nr() {
        return;
    }
    let os_released = raw.events.iter().any(|e| matches!(e, egui::Event::PointerButton { button: PointerButton::Primary, pressed: false, .. }));
    if let Some(pos) = ctx.input(|i| i.pointer.latest_pos()).filter(|_| !os_released && ctx.input(|i| i.pointer.primary_down())) {
        raw.events.push(egui::Event::PointerButton { pos, button: PointerButton::Primary, pressed: false, modifiers: ctx.input(|i| i.modifiers) });
    }
    let pass = ctx.cumulative_pass_nr();
    ctx.data_mut(|d| d.insert_temp(resize_handed_off_id(), HandOff { released: Some(pass), ..h }));
}

/// The compositor owns the pointer during an OS resize: it sets its own cursor, and the button
/// release goes to it, not to us. On the first pointer motion after the hand-off, show the
/// default cursor for one frame so the resize cursor is sent again on the next.
fn reshow_cursor_after_os_resize(ctx: &egui::Context) {
    // Only motion after the press was ended means the compositor gave the pointer back.
    let Some(HandOff { released: Some(pass), .. }) = ctx.data(|d| d.get_temp::<HandOff>(resize_handed_off_id())) else { return };
    if pass < ctx.cumulative_pass_nr() && ctx.input(|i| i.events.iter().any(|e| matches!(e, egui::Event::PointerMoved(_)))) {
        ctx.data_mut(|d| d.remove::<HandOff>(resize_handed_off_id()));
        ctx.set_cursor_icon(CursorIcon::Default);
        ctx.request_repaint();
    }
}

/// Invisible resize zones along the window's edges, above everything else; a press in one starts
/// an OS resize in that direction. None while the window is maximized or full screen.
pub fn resize_zones(ui: &mut Ui) {
    let ctx = ui.ctx().clone();
    if maximized(&ctx) || ctx.input(|i| i.viewport().fullscreen.unwrap_or(false)) {
        reshow_cursor_after_os_resize(&ctx);
        return;
    }
    let w = ctx.content_rect();
    let layer = LayerId::new(Order::Foreground, Id::new("titlebar-resize"));
    let zui = ui.new_child(egui::UiBuilder::new().layer_id(layer).max_rect(w));
    for (i, (r, dir, cursor)) in zones(w).into_iter().enumerate() {
        let resp = zui.interact(r, Id::new(("titlebar-resize", i)), Sense::drag()).on_hover_cursor(cursor);
        if resp.drag_started_by(PointerButton::Primary) {
            ctx.send_viewport_cmd(ViewportCommand::BeginResize(dir));
            let pass = ctx.cumulative_pass_nr();
            ctx.data_mut(|d| d.insert_temp(resize_handed_off_id(), HandOff { pass, released: None }));
        }
    }
    // After the zones, so its one-frame cursor wins over theirs.
    reshow_cursor_after_os_resize(&ctx);
}

#[cfg(test)]
mod tests {
    use super::*;
    use egui::{Event, Pos2, RawInput, ViewportId};
    use filmcraft_engine::Session;

    /// Title bar height in `crate::header`.
    const BAR_H: f32 = crate::header::TITLE_H;

    /// A headless window `width` pt wide showing the title bar and the resize zones.
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
            let mut session = Session::default();
            let _ = session.execute("file.openDemoProject", serde_json::json!({}));
            // An unsaved change, so Close must ask first.
            let clip = session.active_sequence().and_then(|s| s.video_tracks.first()).and_then(|t| t.items.first()).map(|i| i.id.0);
            if let Some(id) = clip {
                let _ = session.execute("effects.setParam", serde_json::json!({"clip": id, "effect": "motion", "param": "rotation", "value": 45.0}));
            }
            let mut app = FilmcraftApp::new(session);
            app.custom_titlebar = true;
            Self { ctx, app, width, time: 0.0, maximized: false }
        }

        fn frame(&mut self, events: Vec<Event>) -> egui::FullOutput {
            self.time += 0.05;
            let mut input =
                RawInput { screen_rect: Some(Rect::from_min_size(Pos2::ZERO, vec2(self.width, 600.0))), time: Some(self.time), events, ..Default::default() };
            if let Some(v) = input.viewports.get_mut(&ViewportId::ROOT) {
                v.maximized = Some(self.maximized);
            }
            // What `FilmcraftApp::raw_input_hook` does.
            release_after_os_resize(&self.ctx, &mut input);
            let app = &mut self.app;
            // The integrated title bar (macOS) keeps the classic single bar instead.
            let classic = app.integrated_titlebar;
            let width = self.width;
            let mut out = self.ctx.run_ui(input, |ui| {
                if classic {
                    crate::header::show(app, ui, Rect::from_min_size(Pos2::ZERO, vec2(width, crate::header::MODE_H)));
                } else {
                    crate::header::title_bar(app, ui, Rect::from_min_size(Pos2::ZERO, vec2(width, BAR_H)));
                    resize_zones(ui);
                }
            });
            out.textures_delta.clear();
            out
        }

        /// Frames for a pointer gesture: move to `path[0]`, press, move along `path`, release. The
        /// viewport commands the frames sent.
        fn gesture(&mut self, path: &[Pos2]) -> Vec<ViewportCommand> {
            let mut events = vec![vec![Event::PointerMoved(path[0])], vec![button(path[0], true)]];
            events.extend(path[1..].iter().map(|p| vec![Event::PointerMoved(*p)]));
            events.push(vec![button(*path.last().unwrap_or(&path[0]), false)]);
            events.into_iter().flat_map(|e| self.frame(e).viewport_output.remove(&ViewportId::ROOT).map(|v| v.commands).unwrap_or_default()).collect()
        }

        fn click(&mut self, p: Pos2) -> Vec<ViewportCommand> {
            self.gesture(&[p])
        }

        fn caption(&self, c: Caption) -> Rect {
            self.ctx.read_response(caption_id(c)).map(|r| r.rect).unwrap_or(Rect::NOTHING)
        }

        /// The bar's clickable widgets other than its drag gap and the caption buttons.
        fn bar_widgets(&self) -> Vec<Rect> {
            let skip = CAPTIONS.map(caption_id);
            self.ctx.viewport(|v| {
                v.prev_pass
                    .widgets
                    .layers()
                    .filter(|(l, _)| l.order != Order::Foreground)
                    .flat_map(|(_, ws)| ws.iter())
                    .filter(|w| w.sense.senses_click() && !w.sense.senses_drag() && w.rect.top() < BAR_H - 2.0 && !skip.contains(&w.id))
                    .map(|w| w.rect)
                    .collect()
            })
        }
    }

    fn texts(out: &egui::FullOutput) -> Vec<(String, Rect)> {
        fn walk(s: &egui::Shape, v: &mut Vec<(String, Rect)>) {
            match s {
                egui::Shape::Text(t) => v.push((t.galley.text().to_string(), Rect::from_min_size(t.pos, t.galley.size()))),
                egui::Shape::Vec(s) => s.iter().for_each(|s| walk(s, v)),
                _ => {}
            }
        }
        let mut v = vec![];
        out.shapes.iter().for_each(|c| walk(&c.shape, &mut v));
        v
    }

    fn button(p: Pos2, pressed: bool) -> Event {
        Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }
    }

    #[test]
    fn caption_buttons_take_the_right_end_and_nothing_overlaps_them() {
        for width in [1100.0, 1440.0, 1600.0] {
            let mut w = Win::new(width);
            w.frame(vec![]);
            w.frame(vec![]);
            let [min, max, close] = CAPTIONS.map(|c| w.caption(c));
            assert_eq!((close.right(), close.top()), (width, 0.0), "Close sits in the window's corner at {width}");
            assert!(close.height() >= BAR_H - 1.0 && [min, max, close].iter().all(|r| r.width() == BUTTON_WIDTH));
            assert_eq!((min.right(), max.right()), (max.left(), close.left()));
            let widgets = w.bar_widgets();
            assert!(!widgets.is_empty(), "menus and quick actions are clickable");
            for a in &widgets {
                assert!(a.right() <= min.left(), "{a:?} runs into the caption buttons at {width}");
            }
        }
    }

    #[test]
    fn the_brand_mark_leads_the_bar() {
        let mut w = Win::new(1440.0);
        w.frame(vec![]);
        w.frame(vec![]);
        let mark = crate::brand::mark_rect(&w.ctx).expect("the brand mark is painted");
        let out = w.frame(vec![]);
        let file = texts(&out).into_iter().find(|(t, _)| t == "File").map(|(_, r)| r).expect("the File menu title");
        assert!(mark.right() < file.left(), "the mark sits left of the menus: {mark:?} {file:?}");
        assert!(mark.left() < 20.0 && mark.width() >= 16.0, "{mark:?}");
    }

    #[test]
    fn the_system_title_bar_hides_the_mark_and_caption_buttons() {
        let mut w = Win::new(1440.0);
        w.frame(vec![]);
        assert!(crate::brand::mark_rect(&w.ctx).is_some());
        // With the system title bar the OS draws its own icon and caption buttons.
        w.app.custom_titlebar = false;
        let _ = w.app.session.execute("prefs.set", serde_json::json!({"key": "appearance.systemTitleBar", "value": true}));
        w.frame(vec![]);
        w.frame(vec![]);
        assert!(crate::titlebar::system_title_bar(&w.app));
        assert!(w.caption(Caption::Close) == Rect::NOTHING, "no in-app caption buttons");
    }

    #[test]
    fn close_asks_about_unsaved_work_before_quitting() {
        let mut w = Win::new(1440.0);
        w.frame(vec![]);
        assert!(w.app.session.is_dirty(), "the demo project starts dirty");
        let close = w.caption(Caption::Close).center();
        let cmds = w.click(close);
        // `app.quit` asks the window to close; the unsaved-changes guard (`intercept_quit`,
        // wired to the OS close request in `logic`) then opens the Save / Don't Save / Cancel
        // prompt instead of quitting.
        assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::Close)), "{cmds:?}");
        assert!(crate::panels::clip_dialogs::intercept_quit(&mut w.app), "dirty: the close is intercepted");
        assert!(w.app.ui.clip_dialog.as_ref().is_some_and(|d| d.command == "app.quit"), "the quit prompt opens");
    }

    #[test]
    fn minimize_and_maximize_follow_the_window_state() {
        let mut w = Win::new(1440.0);
        w.frame(vec![]);
        let (min, max) = (w.caption(Caption::Minimize).center(), w.caption(Caption::Maximize).center());
        assert!(w.click(min).contains(&ViewportCommand::Minimized(true)));
        assert!(w.click(max).contains(&ViewportCommand::Maximized(true)));
        w.maximized = true;
        assert!(w.click(max).contains(&ViewportCommand::Maximized(false)), "Restore");
    }

    #[test]
    fn empty_bar_space_drags_and_double_click_maximizes() {
        let mut w = Win::new(1440.0);
        w.frame(vec![]);
        w.frame(vec![]);
        // The free gap between the menus and the right cluster; must hold on all widths above.
        let empty = pos2(800.0, 16.0);
        assert!(w.bar_widgets().iter().all(|r| !r.contains(empty)), "the probe point must be empty bar space");
        let cmds = w.gesture(&[empty, empty + vec2(12.0, 4.0), empty + vec2(30.0, 8.0)]);
        assert_eq!(cmds.iter().filter(|c| matches!(c, ViewportCommand::StartDrag)).count(), 1, "{cmds:?}");
        let mut cmds = w.click(empty);
        cmds.extend(w.click(empty));
        assert!(cmds.contains(&ViewportCommand::Maximized(true)), "{cmds:?}");
        // Widgets keep their clicks: dragging from a menu title doesn't move the window.
        let file = texts(&w.frame(vec![])).into_iter().find(|(t, _)| t == "File").map(|(_, r)| r.center());
        let file = file.expect("the File menu title");
        let cmds = w.gesture(&[file, file + vec2(12.0, 4.0), file + vec2(30.0, 8.0)]);
        assert!(!cmds.iter().any(|c| matches!(c, ViewportCommand::StartDrag)), "{cmds:?}");
    }

    #[test]
    fn edge_zones_resize_unless_maximized() {
        use ResizeDirection::*;
        let mut w = Win::new(1440.0);
        w.frame(vec![]);
        for (p, dir) in [
            (pos2(1.0, 300.0), West),
            (pos2(1438.0, 300.0), East),
            (pos2(500.0, 1.0), North),
            (pos2(500.0, 598.0), South),
            (pos2(2.0, 2.0), NorthWest),
            (pos2(1437.0, 3.0), NorthEast),
            (pos2(3.0, 597.0), SouthWest),
            (pos2(1438.0, 598.0), SouthEast),
        ] {
            let cmds = w.gesture(&[p, p + vec2(6.0, 6.0)]);
            assert_eq!(cmds, vec![ViewportCommand::BeginResize(dir)], "at {p:?}");
        }
        // Maximized: no zones, and the window's top-right corner is Close.
        w.maximized = true;
        let cmds = w.gesture(&[pos2(1.0, 300.0), pos2(8.0, 300.0)]);
        assert!(!cmds.iter().any(|c| matches!(c, ViewportCommand::BeginResize(_))), "{cmds:?}");
        assert!(w.click(pos2(1439.5, 0.5)).iter().any(|c| matches!(c, ViewportCommand::Close)));
    }

    #[test]
    fn the_os_window_title_follows_the_project() {
        let mut w = Win::new(1440.0);
        let ctx = w.ctx.clone();
        crate::titlebar::sync_window_title(&mut w.app, &ctx);
        let mut out = w.frame(vec![]);
        let cmds: Vec<_> = out.viewport_output.remove(&ViewportId::ROOT).map(|v| v.commands).unwrap_or_default();
        assert!(cmds.iter().any(|c| matches!(c, ViewportCommand::Title(t) if t.ends_with("— FilmCraft"))), "{cmds:?}");
        // Sent once: no change, no command.
        let mut out = w.frame(vec![]);
        let cmds: Vec<_> = out.viewport_output.remove(&ViewportId::ROOT).map(|v| v.commands).unwrap_or_default();
        assert!(!cmds.iter().any(|c| matches!(c, ViewportCommand::Title(_))), "{cmds:?}");
    }

    /// With an integrated title bar (macOS, global menu bar) the chrome stays the classic
    /// single bar: Home and the mode tabs, the centred document title, the full quick-action
    /// cluster — no brand mark, no caption buttons, no second bar.
    #[test]
    fn the_integrated_title_bar_keeps_the_classic_single_bar() {
        let mut w = Win::new(1600.0);
        // As macOS main sets it: traffic lights over the strip, native menu bar.
        w.app.integrated_titlebar = true;
        w.app.custom_titlebar = false;
        w.app.ui.show_menu_bar = false;
        w.frame(vec![]);
        let out = w.frame(vec![]);
        // No in-app caption buttons: the OS draws them.
        assert_eq!(w.caption(Caption::Close), Rect::NOTHING);
        assert_eq!(w.caption(Caption::Minimize), Rect::NOTHING);
        // ... and no brand mark (none was ever painted on this context).
        assert!(crate::brand::mark_rect(&w.ctx).is_none());
        // Home and the mode tabs are on the bar ...
        assert!(w.ctx.read_response(egui::Id::new("hdr-home")).is_some(), "Home");
        assert!(w.ctx.read_response(egui::Id::new(("mode", "Edit"))).is_some(), "Edit tab");
        // ... with the document title centred ...
        let names: Vec<_> = texts(&out).into_iter().map(|(t, _)| t).collect();
        assert!(names.iter().any(|t| t.contains("Edited")), "dirty document title: {names:?}");
        // ... and the full classic quick-action cluster (volume and Quick Export live on
        // the mode bar elsewhere; here they share the one bar).
        for id in [
            egui::Id::new(("hdr", "volume")),
            egui::Id::new(("hdr", "quickExport")),
            egui::Id::new(("hdr", "fullscreen")),
            egui::Id::new("hdr-discord"),
            egui::Id::new("hdr-ws-name"),
        ] {
            assert!(w.ctx.read_response(id).is_some(), "{id:?}");
        }
        // No title-bar-only widgets: a single bar, not two.
        assert!(w.ctx.read_response(egui::Id::new(("ttl", "search"))).is_none());
        // The whole bar drags the window, as before.
        let cmds = w.gesture(&[pos2(800.0, 19.0), pos2(812.0, 23.0), pos2(830.0, 27.0)]);
        assert_eq!(cmds.iter().filter(|c| matches!(c, ViewportCommand::StartDrag)).count(), 1, "{cmds:?}");
    }
}

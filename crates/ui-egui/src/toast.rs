//! The transient message at the bottom of the window: an export finished, a frame was written,
//! media went missing. The engine sets [`FilmcraftApp::toast`] from its events, but nothing used to
//! draw it, so those messages never reached anyone.

use crate::FilmcraftApp;

/// How long a toast stays up, in seconds.
const TOAST_SECONDS: f64 = 3.5;

/// Draw the current toast (bottom centre) until it expires.
///
/// A `Label` rather than painted text, so a path or a command in the message can be selected and
/// copied — that is usually the whole point of a message like `Exported frame to /path/out.png`.
pub(crate) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some((text, shown_at)) = app.toast.take() else { return };
    if ctx.input(|i| i.time) - shown_at > TOAST_SECONDS {
        return;
    }
    egui::Window::new("toast")
        .id(egui::Id::new("toast"))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .movable(false)
        .anchor(egui::Align2::CENTER_BOTTOM, [0.0, -32.0])
        .show(ctx, |ui| {
            ui.set_max_width(560.0);
            ui.add(egui::Label::new(&text).wrap());
        });
    // Nothing else may be redrawing while a toast is up: expire it on time, not on the next event.
    ctx.request_repaint_after(std::time::Duration::from_millis(250));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One frame with `now` pinned, so the expiry check does not depend on the wall clock.
    /// Returns where egui laid the toast out, or `None` if it drew nothing.
    fn draw(app: &mut FilmcraftApp, shown_at: f64, now: f64) -> Option<egui::Rect> {
        if let Some((_, at)) = app.toast.as_mut() {
            *at = shown_at;
        }
        let ctx = egui::Context::default();
        let input = egui::RawInput { time: Some(now), ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| show(app, ui.ctx()));
        out.textures_delta.clear();
        ctx.memory(|m| m.area_rect(egui::Id::new("toast")))
    }

    fn app_with_toast() -> FilmcraftApp {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.toast = Some(("Exported frame to /tmp/scene-0001.png".to_owned(), 0.0));
        app
    }

    /// The message the engine left is drawn at all — it used to be collected and never shown — and
    /// it is a window with a label of its own, so it can be selected. Painted text never can be.
    #[test]
    fn a_fresh_toast_is_shown_as_a_widget() {
        let mut app = app_with_toast();
        let area = draw(&mut app, 2.0, 2.0).expect("the toast is laid out as a widget, not paint");
        assert!(area.is_positive(), "the toast has a rect of its own: {area:?}");
        assert!(app.toast.is_none(), "the message was taken to be shown");
    }

    /// An expired toast is dropped instead of staying on screen for the rest of the session.
    #[test]
    fn an_expired_toast_is_not_drawn() {
        let mut app = app_with_toast();
        assert!(draw(&mut app, -100.0, 2.0).is_none(), "an expired toast must not be laid out");
    }
}

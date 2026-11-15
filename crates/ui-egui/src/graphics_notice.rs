//! The notice shown while the window runs in a fallback graphics mode (#820): the desktop app
//! moved on to DirectX 12 only or a software adapter after earlier starts died in the graphics
//! driver. It says so and offers a one-click way back to the default mode for the next start.
//!
//! Automation ids: `graphics.notice` (the notice), `graphics.notice.reset` (Use Default Graphics),
//! `graphics.notice.close`.

use crate::{FilmcraftApp, i18n};

/// Puts the default graphics mode back for the next start; the error says what could not be saved.
pub type ResetFn = Box<dyn FnMut() -> Result<(), String>>;

/// A fallback graphics mode, as the host reports it.
pub struct GraphicsNotice {
    /// The mode's English name ("DirectX 12 only", "software rendering"), translated when shown.
    pub mode: String,
    reset: ResetFn,
    /// The default mode is saved for the next start.
    pub reset_done: bool,
    /// Closed for this session.
    pub closed: bool,
}

impl GraphicsNotice {
    pub fn new(mode: &str, reset: ResetFn) -> Self {
        Self { mode: mode.to_string(), reset, reset_done: false, closed: false }
    }

    /// Use Default Graphics: save the default mode for the next start.
    pub fn reset(&mut self) -> Result<(), String> {
        (self.reset)()?;
        self.reset_done = true;
        Ok(())
    }
}

/// Draw the notice (bottom right, above the status bar) until it is closed.
pub(crate) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut notice) = app.graphics_notice.take() else { return };
    if notice.closed {
        app.graphics_notice = Some(notice);
        return;
    }
    let title = tlf!("Safe graphics mode: {mode}", mode = i18n::t(&notice.mode));
    let (mut reset, mut close) = (false, false);
    let shown = egui::Window::new("graphics-notice")
        .id(egui::Id::unique("graphics-notice"))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -32.0])
        .show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.label(egui::RichText::new(&title).strong());
            ui.add_space(4.0);
            if notice.reset_done {
                ui.label(tl!("The default graphics are used from the next start."));
            } else {
                ui.label(tl!("An earlier start failed with the default graphics. Updating the graphics driver usually fixes this."));
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                if !notice.reset_done {
                    let label = tl!("Use Default Graphics");
                    let r = ui.button(label);
                    app.auto.add_response("graphics.notice.reset", &r, label);
                    reset = r.clicked();
                }
                let label = tl!("Close");
                let c = ui.button(label);
                app.auto.add_response("graphics.notice.close", &c, label);
                close = c.clicked();
            });
        });
    if let Some(shown) = shown {
        app.auto.add("graphics.notice", shown.response.rect, &title);
    }
    if reset && let Err(e) = notice.reset() {
        app.ui.status = e;
    }
    notice.closed |= close;
    app.graphics_notice = Some(notice);
}

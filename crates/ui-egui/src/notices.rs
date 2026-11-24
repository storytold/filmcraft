//! A plain in-window notice above the status bar: a title, a few lines and Close.
//!
//! The Nightly channel uses it for session health ("the previous session did not finish
//! normally"), which must not be a modal dialog: a launch from the Launcher or a test runner has
//! nobody to dismiss one. Automation id: `notice`, and `notice.close`.

use crate::{FilmcraftApp, i18n};

/// What to show. The lines are messages the host composed (Nightly-only, so not translated).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Notice {
    pub title: String,
    pub lines: Vec<String>,
    pub closed: bool,
}

impl Notice {
    pub fn new(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self { title: title.into(), lines, closed: false }
    }
}

pub(crate) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut notice) = app.notice.take() else { return };
    if notice.closed {
        app.notice = Some(notice);
        return;
    }
    let mut close = false;
    let shown = egui::Window::new("notice")
        .id(egui::Id::unique("notice"))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -32.0])
        .show(ctx, |ui| {
            ui.set_max_width(380.0);
            ui.label(egui::RichText::new(&notice.title).strong());
            ui.add_space(4.0);
            for line in &notice.lines {
                ui.label(line);
            }
            ui.add_space(6.0);
            let label = i18n::t("Close");
            let c = ui.button(label);
            app.auto.add_response("notice.close", &c, label);
            close = c.clicked();
        });
    if let Some(shown) = shown {
        app.auto.add("notice", shown.response.rect, &notice.title);
    }
    notice.closed |= close;
    app.notice = Some(notice);
}

//! A plain in-window notice above the status bar: a title, a few lines and Close.
//!
//! The Nightly channel uses it for session health ("the previous session did not finish
//! normally"), which must not be a modal dialog: a launch from the Launcher or a test runner has
//! nobody to dismiss one. Automation id: `notice`, and `notice.close`.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::{FilmcraftApp, i18n};

/// What to show. The lines are messages the host composed (Nightly-only, so not translated).
#[derive(Clone, Debug)]
pub struct Notice {
    pub title: String,
    pub lines: Vec<String>,
    pub closed: bool,
    /// Set by `show` once the notice reached a pass the host keeps, so a host-owned record is
    /// cleared only after the window really displayed it (a close before then keeps it).
    pub shown: Option<Arc<AtomicBool>>,
}

impl Notice {
    pub fn new(title: impl Into<String>, lines: Vec<String>) -> Self {
        Self { title: title.into(), lines, closed: false, shown: None }
    }
}

pub(crate) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut notice) = app.notice.take() else { return };
    if notice.closed {
        app.notice = Some(notice);
        return;
    }
    let mut close = false;
    let drawn = egui::Window::new("notice")
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
    if let Some(drawn) = drawn {
        app.auto.add("notice", drawn.response.rect, &notice.title);
        // Only a pass the host keeps counts: an early return before this pass (fonts not ready) or
        // a discarded pass must not consume the record the notice reports.
        if let Some(shown) = &notice.shown
            && !ctx.will_discard()
        {
            shown.store(true, Ordering::Relaxed);
        }
    }
    notice.closed |= close;
    app.notice = Some(notice);
}

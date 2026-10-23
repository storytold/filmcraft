//! The notice shown after the interface language changes: the new language takes effect the next
//! time the app starts (as in Premiere), so a pending change offers to restart now or later.
//! Restart closes the window through the usual quit path (unsaved changes ask to save first) and
//! the host relaunches the app on exit (`HostHooks::restart`).
//!
//! Automation ids: `language.restart` (the notice), `language.restart.restart` (Restart),
//! `language.restart.later`.

use crate::FilmcraftApp;

/// A language change waiting for a restart (Settings ▸ General ▸ Interface Language, Edit ▸
/// Language, `general.interfaceLanguage`). `language` is the one the next start will use.
pub struct RestartNotice {
    pub language: crate::i18n::Language,
    /// Closed for this session ("Later"); the change still applies at the next start.
    pub closed: bool,
}

impl RestartNotice {
    pub fn new(language: crate::i18n::Language) -> Self {
        Self { language, closed: false }
    }
}

/// Draw the notice (bottom right, above the status bar) until it is closed.
pub(crate) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut notice) = app.restart_notice.take() else { return };
    if notice.closed {
        app.restart_notice = Some(notice);
        return;
    }
    let title = tlf!("Restart FilmCraft to use {language}", language = notice.language.name());
    let (mut restart, mut later) = (false, false);
    let shown = egui::Window::new("language-restart")
        .id(egui::Id::new("language-restart"))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -32.0])
        .show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.label(egui::RichText::new(&title).strong());
            ui.add_space(4.0);
            ui.label(tl!("The interface language changes after a restart."));
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let label = tl!("Restart");
                let r = ui.button(label);
                app.auto.add_response("language.restart.restart", &r, label);
                restart = r.clicked();
                let label = tl!("Later");
                let c = ui.button(label);
                app.auto.add_response("language.restart.later", &c, label);
                later = c.clicked();
            });
        });
    if let Some(shown) = shown {
        app.auto.add("language.restart", shown.response.rect, &title);
    }
    if restart {
        app.begin_restart(ctx);
    }
    notice.closed |= later | restart;
    app.restart_notice = Some(notice);
}

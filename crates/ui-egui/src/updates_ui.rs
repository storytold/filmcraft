//! Software updates, background check at startup, and startup prompt modal.

use serde_json::Value;

use crate::FilmcraftApp;

#[cfg(not(target_arch = "wasm32"))]
use craft_update::updater::{Action, Status, Updater};

pub(crate) struct Runtime {
    #[cfg(not(target_arch = "wasm32"))]
    updater: Updater,
    /// The desktop app turns the check at start on; tests and the web build leave it off.
    #[cfg(not(target_arch = "wasm32"))]
    startup_check: bool,
    #[cfg(not(target_arch = "wasm32"))]
    check_requested: bool,
    /// The context of the last frame, to wake the UI when a check is queued from a menu callback.
    #[cfg(not(target_arch = "wasm32"))]
    repaint: Option<egui::Context>,
    pub(crate) manual_window_open: bool,
    pub(crate) status_message: Option<String>,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            updater: Updater::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION")),
            #[cfg(not(target_arch = "wasm32"))]
            startup_check: false,
            #[cfg(not(target_arch = "wasm32"))]
            check_requested: false,
            #[cfg(not(target_arch = "wasm32"))]
            repaint: None,
            manual_window_open: false,
            status_message: None,
        }
    }
}

impl FilmcraftApp {
    /// Whether the app asks for a newer release when it starts (off until the desktop app turns it on).
    #[cfg(not(target_arch = "wasm32"))]
    pub fn set_startup_update_check(&mut self, on: bool) {
        self.updates_rt.startup_check = on;
    }
}

pub(crate) fn check(app: &mut FilmcraftApp) -> Result<Value, String> {
    #[cfg(not(target_arch = "wasm32"))]
    {
        app.updates_rt.check_requested = true;
        if let Some(ctx) = &app.updates_rt.repaint {
            ctx.request_repaint();
        }
        Ok(Value::Null)
    }
    #[cfg(target_arch = "wasm32")]
    {
        app.updates_rt.manual_window_open = true;
        app.updates_rt.status_message = Some("Update check is not supported in the web browser.".into());
        Ok(Value::Null)
    }
}

pub(crate) fn tick(app: &mut FilmcraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let rt = &mut app.updates_rt;
        rt.repaint = Some(ctx.clone());
        if std::mem::take(&mut rt.check_requested) {
            rt.updater.check_now(ctx);
        } else if rt.startup_check {
            rt.updater.check_at_start(ctx);
        }
        rt.updater.tick(ctx);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (app, ctx);
}

pub(crate) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        // The restart helper is scheduled before the app closes, so no update starts over unsaved work.
        let clean = !app.session.is_dirty();
        match app.updates_rt.updater.show_if(ctx, clean) {
            Action::OpenUrl(url) => crate::links::open(app, ctx, &url),
            Action::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            Action::None => {}
        }
        // The window answers Help ▸ Check for Updates, so it follows the updater's status.
        let rt = &mut app.updates_rt;
        (rt.manual_window_open, rt.status_message) = match rt.updater.status() {
            Status::Idle => (false, None),
            Status::Checking => (true, Some(tl!("Checking for updates…").to_string())),
            Status::UpToDate => {
                (true, Some(tl!("FilmCraft {version} is currently the newest version available.").replace("{version}", env!("CARGO_PKG_VERSION"))))
            }
            Status::Failed(e) => (true, Some(tl!("Could not check for updates:\n{error}").replace("{error}", e))),
        };
    }

    if app.updates_rt.manual_window_open {
        let mut open = true;
        egui::Window::new(crate::i18n::t("FilmCraft Updates"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(ctx, |ui| {
                if let Some(msg) = &app.updates_rt.status_message {
                    ui.label(msg);
                }
                ui.add_space(8.0);
                if ui.button(crate::i18n::t("OK")).clicked() {
                    app.updates_rt.manual_window_open = false;
                }
            });
        if !open {
            app.updates_rt.manual_window_open = false;
        }
    }

    // Closing the window dismisses the outcome it showed.
    #[cfg(not(target_arch = "wasm32"))]
    if !app.updates_rt.manual_window_open {
        app.updates_rt.updater.dismiss_status();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_runtime_is_idle() {
        let rt = Runtime::default();
        assert!(!rt.manual_window_open);
        #[cfg(not(target_arch = "wasm32"))]
        {
            assert!(!rt.startup_check);
            assert!(!rt.updater.prompt_open());
            assert_eq!(rt.updater.status(), &Status::Idle);
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn startup_prompt_shows_and_closes() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let ctx = egui::Context::default();
        let release = craft_update::Release {
            tag: "v99.0.0".into(),
            version: craft_update::parse_version("99.0.0"),
            name: "FilmCraft 99.0.0".into(),
            html_url: "https://github.com/storytold/filmcraft/releases/tag/v99.0.0".into(),
            ..Default::default()
        };
        app.updates_rt.updater.offer(release);

        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(&mut app, ui.ctx());
        });
        output.textures_delta.clear();
        assert!(app.updates_rt.updater.prompt_open());
    }
}

//! Software updates, background check at startup, and startup prompt modal.

use serde_json::Value;

use crate::FilmcraftApp;

#[cfg(not(target_arch = "wasm32"))]
use craft_ui::dialog::DialogHost;
#[cfg(not(target_arch = "wasm32"))]
use craft_update::stack_dialogs::{CheckChoice, UpdateCheckDialog};
#[cfg(not(target_arch = "wasm32"))]
use craft_update::updater::{Action, Status, Updater};

pub(crate) struct Runtime {
    #[cfg(not(target_arch = "wasm32"))]
    updater: Updater,
    /// The update prompt and the check result dialog.
    #[cfg(not(target_arch = "wasm32"))]
    dialogs: DialogHost,
    /// The desktop app turns the check at start on; tests and the web build leave it off.
    #[cfg(not(target_arch = "wasm32"))]
    startup_check: bool,
    #[cfg(not(target_arch = "wasm32"))]
    check_requested: bool,
    /// The context of the last frame, to wake the UI when a check is queued from a menu callback.
    #[cfg(not(target_arch = "wasm32"))]
    repaint: Option<egui::Context>,
    pub(crate) manual_window_open: bool,
}

impl Default for Runtime {
    fn default() -> Self {
        Self {
            #[cfg(not(target_arch = "wasm32"))]
            updater: Updater::new(&crate::UPDATE_APP, env!("CARGO_PKG_VERSION")),
            #[cfg(not(target_arch = "wasm32"))]
            dialogs: DialogHost::new(),
            #[cfg(not(target_arch = "wasm32"))]
            startup_check: false,
            #[cfg(not(target_arch = "wasm32"))]
            check_requested: false,
            #[cfg(not(target_arch = "wasm32"))]
            repaint: None,
            manual_window_open: false,
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
        app.updates_rt.manual_window_open = true;
        if let Some(ctx) = &app.updates_rt.repaint {
            ctx.request_repaint();
        }
        Ok(Value::Null)
    }
    #[cfg(target_arch = "wasm32")]
    {
        app.updates_rt.manual_window_open = true;
        Ok(Value::Null)
    }
}

pub(crate) fn tick(app: &mut FilmcraftApp, ctx: &egui::Context) {
    #[cfg(not(target_arch = "wasm32"))]
    {
        let rt = &mut app.updates_rt;
        rt.repaint = Some(ctx.clone());
        if std::mem::take(&mut rt.check_requested) {
            rt.updater.check_now();
        } else if rt.startup_check {
            rt.updater.check_at_start();
        }
        rt.updater.tick(ctx);
    }
    #[cfg(target_arch = "wasm32")]
    let _ = (app, ctx);
}

pub(crate) fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    #[cfg(not(target_arch = "wasm32"))]
    {
        let ctx = &ctx;
        // The restart helper is scheduled before the app closes, so no update starts over unsaved work.
        let clean = !app.session.is_dirty();
        let prior_commands = ctx.output(|output| output.commands.len());
        let rt = &mut app.updates_rt;
        let action = rt.updater.run(&mut rt.dialogs, clean);
        rt.manual_window_open = result_window_open(rt.manual_window_open, rt.updater.status(), rt.check_requested);
        sync_check_dialog(rt);
        rt.dialogs.run(ui);
        if let Some(result) = rt.dialogs.take::<UpdateCheckDialog>() {
            match result.ok() {
                Some(CheckChoice::CheckAgain | CheckChoice::CheckNow) => rt.check_requested = true,
                // The next pass's `run` asks the window to close once nothing is unsaved.
                Some(CheckChoice::RestartToUpdate) => {
                    rt.updater.restart_to_update();
                    ctx.request_repaint();
                }
                // OK, Esc and the close box.
                _ => {
                    rt.manual_window_open = false;
                    rt.updater.dismiss_status();
                }
            }
        }
        if app.hooks.open_url.is_some() {
            // The update dialogs emit egui URL commands for their release-notes links. The desktop
            // host handles links itself, so forward only commands emitted by these dialogs.
            let urls = ctx.output_mut(|output| {
                let tail = output.commands.split_off(prior_commands.min(output.commands.len()));
                let mut urls = Vec::new();
                for command in tail {
                    match command {
                        egui::OutputCommand::OpenUrl(url) => urls.push(url.url),
                        other => output.commands.push(other),
                    }
                }
                urls
            });
            for url in urls {
                crate::links::open(app, ctx, &url);
            }
        }
        match action {
            Action::OpenUrl(url) => crate::links::open(app, ctx, &url),
            Action::Close => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            _ => {}
        }
    }

    #[cfg(target_arch = "wasm32")]
    if app.updates_rt.manual_window_open {
        let mut open = true;
        egui::Window::new(crate::i18n::t("FilmCraft Updates"))
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .pivot(egui::Align2::CENTER_CENTER)
            .default_pos(ctx.content_rect().center())
            .show(&ctx, |ui| {
                ui.label(crate::i18n::t("Update check is not supported in the web browser."));
                ui.add_space(8.0);
                if ui.button(crate::i18n::t("OK")).clicked() {
                    app.updates_rt.manual_window_open = false;
                }
            });
        if !open {
            app.updates_rt.manual_window_open = false;
        }
    }
}

/// Whether the check result dialog shows this frame. It answers Help ▸ Check for Updates until the
/// requested check ends, and it shows whenever the status is not idle: at the first start after an
/// update, the check at start leaves `Updated` or `InstallFailed`. A check that finds a release
/// leaves the status idle and the startup prompt takes over, so the dialog closes then.
#[cfg(not(target_arch = "wasm32"))]
fn result_window_open(open: bool, status: &Status, check_requested: bool) -> bool {
    !matches!(status, Status::Idle) || (open && check_requested)
}

/// Opens the check result dialog on the host while it should show and feeds it the updater's
/// status, or closes it once it should not.
#[cfg(not(target_arch = "wasm32"))]
fn sync_check_dialog(rt: &mut Runtime) {
    use craft_ui::dialog::StackDialog;
    if !rt.manual_window_open {
        if let Some(id) = rt.dialogs.get::<UpdateCheckDialog>().map(StackDialog::id) {
            rt.dialogs.close_by_id(id);
        }
        return;
    }
    let status = rt.updater.status().clone();
    if let Some(dialog) = rt.dialogs.get_mut::<UpdateCheckDialog>() {
        dialog.set_status(status);
    } else {
        let mut dialog = UpdateCheckDialog::new(env!("CARGO_PKG_VERSION"));
        dialog.set_status(status);
        rt.dialogs.open(dialog);
    }
}

/// Starts an update that waits for the app to exit. Called from `on_exit`; dropping the updater
/// does the same, and later calls do nothing.
pub(crate) fn exiting(app: &mut FilmcraftApp) {
    #[cfg(not(target_arch = "wasm32"))]
    app.updates_rt.updater.start_pending_update();
    #[cfg(target_arch = "wasm32")]
    let _ = app;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Offers `release` and returns once its prompt is open. The updater reads the installed
    /// files for an offer on a thread of its own and does not wake the UI when it answers: each
    /// tick polls for the answer, so the test ticks until the prompt opens, and fails rather than
    /// waits forever if the answer never comes. A copy the launcher manages ignores the offer and
    /// fails the test.
    #[cfg(not(target_arch = "wasm32"))]
    fn offer_and_wait(updater: &mut Updater, release: craft_update::Release) {
        let ctx = egui::Context::default();
        updater.offer(release);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
        while !updater.prompt_open() {
            // `offer`, and the tick that takes the thread's answer, read the launcher's choice: a
            // managed copy opens no prompt, and no further answer would come.
            assert!(updater.managed().is_none(), "the launcher manages this copy, so it ignored the offer");
            assert!(std::time::Instant::now() < deadline, "the offer's inspection never answered");
            std::thread::sleep(std::time::Duration::from_millis(5));
            updater.tick(&ctx);
        }
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn update_notes_link_reaches_the_desktop_host() {
        use std::{cell::RefCell, rc::Rc};
        let opened = Rc::new(RefCell::new(Vec::<String>::new()));
        let record = opened.clone();
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.hooks.open_url = Some(Box::new(move |url| {
            record.borrow_mut().push(url.to_owned());
            Ok(())
        }));
        let url = "https://github.com/storytold/filmcraft/releases/tag/v99.0.0";
        offer_and_wait(
            &mut app.updates_rt.updater,
            craft_update::Release { tag: "v99.0.0".into(), version: craft_update::parse_version("99.0.0"), html_url: url.into(), ..Default::default() },
        );
        let ctx = egui::Context::default();
        crate::framework::install_headless(&ctx);
        ctx.global_style_mut(|style| style.animation_time = 0.0);
        let frame = |app: &mut FilmcraftApp, events| {
            crate::framework::before_input(&ctx, app);
            let mut output = ctx.run_ui(
                egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1000.0, 700.0))), events, ..Default::default() },
                |ui| show(app, ui),
            );
            output.textures_delta.clear();
            output
        };
        frame(&mut app, vec![]);
        let output = frame(&mut app, vec![]);
        let link = craft_update::stack_dialogs::UpdateWords::default().whats_new.as_str().to_owned();
        fn position(shape: &egui::Shape, link: &str) -> Option<egui::Pos2> {
            match shape {
                egui::Shape::Vec(shapes) => shapes.iter().find_map(|shape| position(shape, link)),
                egui::Shape::Text(t) if t.galley.text() == link => Some(t.pos + t.galley.rect.center().to_vec2()),
                _ => None,
            }
        }
        let at = output.shapes.iter().find_map(|shape| position(&shape.shape, &link)).expect("update notes link");
        frame(&mut app, vec![egui::Event::PointerMoved(at)]);
        for pressed in [true, false] {
            frame(&mut app, vec![egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE }]);
        }
        assert_eq!(&*opened.borrow(), &[url.to_string()]);
    }

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
        offer_and_wait(&mut app.updates_rt.updater, release);

        crate::framework::install_headless(&ctx);
        crate::framework::before_input(&ctx, &app);
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            show(&mut app, ui);
        });
        output.textures_delta.clear();
        assert!(app.updates_rt.updater.prompt_open());
    }

    #[test]
    #[cfg(not(target_arch = "wasm32"))]
    fn result_window_shows_every_status_but_idle() {
        let shown = [
            Status::Checking,
            Status::UpToDate,
            Status::Failed("offline".into()),
            Status::Installing,
            Status::Ready,
            Status::Managed,
            Status::Updated("1.2.3".into()),
            Status::InstallFailed("access denied".into()),
        ];
        for status in &shown {
            assert!(result_window_open(false, status, false), "{status:?} opens the dialog without a requested check");
        }
        assert!(!result_window_open(false, &Status::Idle, false));
        assert!(!result_window_open(true, &Status::Idle, false), "a check that found a release closes it");
        assert!(result_window_open(true, &Status::Idle, true), "a queued check keeps it open");
        assert!(!result_window_open(false, &Status::Idle, true), "a queued check does not open it");
    }
}

//! FilmCraft Android mobile runner.
//!
//! Compiles to a `cdylib` and runs inside an Android Activity (GameActivity/NativeActivity)
//! using `android-activity`, `eframe` and `wgpu`.
//!
//! On non-Android target architectures only the platform-independent [`browser`] (the in-app file
//! browser that stands in for a native file dialog) is built, so it is tested with the workspace;
//! the app itself is conditionally empty to keep workspace-wide tests and checks fast.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod audio;
pub mod browser;

#[cfg(target_os = "android")]
mod android_app {
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, channel};

    use android_activity::AndroidApp;
    use filmcraft_engine::Session;
    use filmcraft_engine::autosave::AutosaveConfig;
    use filmcraft_ui_egui::FilmcraftApp;
    use log::LevelFilter;

    use crate::audio::AndroidAudio;
    use crate::browser::{Browser, Request, Root};

    /// Where the user's media usually is: shared storage and its standard folders, then the app's
    /// own folders. Reading shared storage needs the permission the user grants in system settings.
    fn roots(app: &AndroidApp) -> Vec<Root> {
        let shared = PathBuf::from("/storage/emulated/0");
        let mut roots: Vec<Root> = [("Internal storage", ""), ("Movies", "Movies"), ("DCIM", "DCIM"), ("Music", "Music"), ("Download", "Download")]
            .into_iter()
            .map(|(label, sub)| Root { label: label.to_string(), path: if sub.is_empty() { shared.clone() } else { shared.join(sub) } })
            .collect();
        if let Some(path) = app.external_data_path() {
            roots.push(Root { label: "App files (external)".into(), path });
        }
        if let Some(path) = app.internal_data_path() {
            roots.push(Root { label: "App files".into(), path });
        }
        roots
    }

    /// The editor plus the in-app file browser it asks for whenever it needs a file dialog.
    struct Shell {
        app: FilmcraftApp,
        requests: Receiver<Request>,
        browser: Option<Browser>,
        roots: Vec<Root>,
    }

    impl Shell {
        /// Show the browser for the next dialog request, and drop it once it has answered. A request
        /// dropped without an answer reads as cancelled on the app's side.
        fn overlay(&mut self, ctx: &egui::Context) {
            if self.browser.is_none()
                && let Ok(request) = self.requests.try_recv()
            {
                self.browser = Some(Browser::new(request, self.roots.clone()));
            }
            if self.browser.as_mut().is_some_and(|b| b.show(ctx)) {
                self.browser = None;
            }
        }
    }

    impl eframe::App for Shell {
        fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
            eframe::App::raw_input_hook(&mut self.app, ctx, raw_input);
        }

        fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            eframe::App::logic(&mut self.app, ctx, frame);
        }

        fn on_exit(&mut self) {
            eframe::App::on_exit(&mut self.app);
        }

        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            eframe::App::ui(&mut self.app, ui, frame);
            let ctx = ui.ctx().clone();
            self.overlay(&ctx);
        }
    }

    // SAFETY: The `android_main` function is the mandatory C-ABI entrypoint invoked by
    // the `android-activity` runtime when the native activity starts up.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    fn android_main(app: AndroidApp) {
        android_logger::init_once(android_logger::Config::default().with_max_level(LevelFilter::Info).with_tag("FilmCraft"));

        log::info!("Starting FilmCraft on Android");

        // Preferences, shortcuts, presets, auto-save and crash recovery live in the app-private
        // data folder, the only place an app can always write.
        let data_dir = app.internal_data_path();
        let roots = roots(&app);

        let mut native_options = eframe::NativeOptions::default();
        native_options.android_app = Some(app);

        let res = eframe::run_native(
            "FilmCraft",
            native_options,
            Box::new(move |cc| {
                let mut session = Session::default();
                match data_dir {
                    Some(dir) => {
                        if let Err(e) = session.start_autosave(AutosaveConfig::new(dir)) {
                            log::warn!("settings, auto-save and crash recovery unavailable: {e}");
                        }
                    }
                    None => log::warn!("no app data folder: settings and auto-save are not kept"),
                }
                let mut filmcraft = FilmcraftApp::new(session);
                // playback sound and the audio master clock
                filmcraft.audio = Some(Box::new(AndroidAudio::new()));
                // Every file dialog goes to the in-app browser (drawn by `Shell` on the next frame).
                let (requests_tx, requests) = channel::<Request>();
                let ctx = cc.egui_ctx.clone();
                filmcraft.hooks.run_dialog = Some(Box::new(move |dialog| {
                    let (reply, answer) = channel();
                    // a failed send drops `reply`, which the app takes as cancelled
                    let _ = requests_tx.send(Request { dialog, reply });
                    ctx.request_repaint();
                    answer
                }));
                Ok(Box::new(Shell { app: filmcraft, requests, browser: None, roots }))
            }),
        );

        if let Err(err) = res {
            log::error!("Failed to launch FilmCraft Android runner: {err:?}");
        }
    }
}

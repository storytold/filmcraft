//! FilmCraft Android mobile runner.
//!
//! Compiles to a `cdylib` and runs inside an Android Activity (GameActivity/NativeActivity)
//! using `android-activity`, `eframe` and `wgpu`.
//!
//! On non-Android target architectures, this crate is conditionally empty to keep
//! workspace-wide tests and checks fast.

#![cfg_attr(
    target_os = "android",
    cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))
)]

#[cfg(target_os = "android")]
mod android_app {
    use android_activity::AndroidApp;
    use filmcraft_engine::Session;
    use filmcraft_ui_egui::FilmcraftApp;
    use log::LevelFilter;

    // SAFETY: The `android_main` function is the mandatory C-ABI entrypoint invoked by
    // the `android-activity` runtime when the native activity starts up.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    fn android_main(app: AndroidApp) {
        android_logger::init_once(android_logger::Config::default().with_max_level(LevelFilter::Info).with_tag("FilmCraft"));

        log::info!("Starting FilmCraft on Android");

        let mut native_options = eframe::NativeOptions::default();
        native_options.android_app = Some(app);

        let res = eframe::run_native(
            "FilmCraft",
            native_options,
            Box::new(|_cc| {
                let session = Session::default();
                Ok(Box::new(FilmcraftApp::new(session)))
            }),
        );

        if let Err(err) = res {
            log::error!("Failed to launch FilmCraft Android runner: {err:?}");
        }
    }
}

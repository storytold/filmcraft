//! The launch-time ask for Android's "All files access" grant (`MANAGE_EXTERNAL_STORAGE`).
//!
//! Android 11+ hands an app the shared storage tree only after the user allows "All files
//! access" on a system Settings page. The Media Browser has its own "Grant access" affordance
//! (see `panels::media_browser`), which is easy to miss on a first launch, so a host that can
//! both check and ask for the grant ([`HostHooks::shared_storage_granted`] and
//! [`HostHooks::shared_storage_grant`](crate::HostHooks::shared_storage_grant), set by the
//! Android entry crate) gets one short prompt after the first frames have rendered:
//!
//! - **Grant access** opens Android's "All files access" page — the same ask the browser makes —
//!   so the user can flip the switch and come back to a working browser;
//! - **Not Now** is remembered (`general.androidSkipAllFilesAccessPrompt`) and the prompt stops
//!   appearing at launch; the browser's own Grant access button still asks on demand.
//!
//! Nothing is asked while the grant already holds, and hosts that cannot check the grant (the
//! desktop and the web) never ask: without `shared_storage_granted`, [`maybe_ask`] does nothing,
//! so the whole module is inert off Android. It compiles everywhere, so the prompt and the
//! preference round-trip run in this crate's own tests.
//!
//! Automation ids: `androidAccess.grant`, `androidAccess.notNow`.

use egui::{Frame, Margin, RichText};
use serde_json::json;

use super::file_dialogs::{button, modal_frame};
use crate::FilmcraftApp;

/// The stored "Not Now": [`filmcraft_engine::settings::GeneralPrefs`]'s field of the same name.
pub(crate) const SKIP_KEY: &str = "general.androidSkipAllFilesAccessPrompt";

/// How many frames to let the app settle before the ask may leave the activity: the activity must
/// be resumed and focused before it may start another, and some Samsung builds (One UI) silently
/// drop an intent sent while the splash is still up. Eight frames is a few hundred milliseconds,
/// invisible to the user and reliable in practice.
const ASK_AFTER_FRAMES: u64 = 8;

/// The launch prompt's state. Idle on every host that never asks.
#[derive(Default)]
pub struct State {
    /// The launch ask has had its one chance this run (granted or not), so it never springs back
    /// after the user answers it.
    pub(crate) offered: bool,
    /// The prompt is up (or waiting for its first frame).
    pub(crate) requested: bool,
    /// The egui frame the modal was last drawn in, so a pass that polls twice draws it once
    /// (see [`show`]).
    drawn_frame: Option<u64>,
}

impl State {
    /// Whether the prompt is up right now.
    pub fn is_open(&self) -> bool {
        self.requested
    }
}

/// Offer the launch ask once the first frames have rendered. Called every frame from
/// [`crate::panels::dialogs::show`]: nothing happens until frame [`ASK_AFTER_FRAMES`], and only
/// once, so an answered prompt never springs back.
pub fn launch_tick(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if app.android_access.offered {
        return;
    }
    // A host that cannot check the grant never asks: mark it done and stop, so the desktop and
    // the web never keep the frame loop awake for a prompt they never show.
    if app.hooks.shared_storage_granted.is_none() {
        app.android_access.offered = true;
        return;
    }
    // Wait for the first frames, and for any startup dialog (a recovered project) to be answered,
    // so the two prompts never stack. Keep the frame loop alive until then: an app that idles
    // before the trigger frame would otherwise never reach it.
    if ctx.cumulative_frame_nr() < ASK_AFTER_FRAMES || app.dialog.is_some() {
        ctx.request_repaint();
        return;
    }
    app.android_access.offered = true;
    maybe_ask(app);
}

/// Ask at launch, unless the grant already holds, the user chose "Not Now" before, or no host can
/// check the grant (the desktop and the web, or a build without the JNI). Infallible: every host
/// that cannot check the grant is left alone.
pub fn maybe_ask(app: &mut FilmcraftApp) {
    match app.hooks.shared_storage_granted.as_ref() {
        Some(granted) if !granted() => {}
        // No way to check the grant, or a grant that already holds: never ask.
        _ => return,
    }
    if app.session.prefs.general.android_skip_all_files_access_prompt {
        return;
    }
    app.android_access.requested = true;
}

/// Draw the launch prompt while it is up. Called every frame from
/// [`crate::panels::dialogs::show`], like the in-app browser's modals.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if !app.android_access.requested {
        return;
    }
    // Drawn once per frame, from whichever pass polls first (eframe may run the UI more than
    // once), which egui would otherwise report as an id clash.
    let frame = ctx.cumulative_frame_nr();
    if app.android_access.drawn_frame == Some(frame) {
        return;
    }
    app.android_access.drawn_frame = Some(frame);
    let t = app.tokens;
    let mut answer = None;
    egui::Modal::new(egui::Id::new("android-all-files-access")).frame(modal_frame(&t)).show(ctx, |ui| {
        ui.set_width(420.0);
        Frame::new().inner_margin(Margin { left: 28, right: 28, top: 24, bottom: 20 }).show(ui, |ui| {
            ui.label(RichText::new(tl!("Allow access to all files?")).size(18.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            ui.label(RichText::new(tl!("FilmCraft needs \"All files access\" to open and save files in shared storage.")).size(13.5).color(t.text));
            ui.add_space(18.0);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if button(app, ui, "androidAccess.grant", tl!("Grant access"), true) {
                    answer = Some(true);
                }
                if button(app, ui, "androidAccess.notNow", tl!("Not Now"), false) {
                    answer = Some(false);
                }
            });
        });
    });
    if let Some(allow) = answer {
        answer_once(app, allow);
    }
}

/// Act on the user's answer: `true` opens Android's "All files access" page (the Media Browser
/// re-checks the grant on its next visit), `false` remembers "Not Now" so the launch ask stops.
/// Separate from [`show`] so a test can answer without an egui frame.
pub(crate) fn answer_once(app: &mut FilmcraftApp, allow: bool) {
    app.android_access.requested = false;
    if allow {
        match app.hooks.shared_storage_grant.as_ref() {
            Some(ask) => {
                ask();
                app.status(tl!("Allow \"All files access\" for FilmCraft on the settings page, then tap Shared storage."));
            }
            // The Media Browser's own button says the same thing for a build without the JNI.
            None => app.status(tl!("This build can't request shared-storage access.")),
        }
    } else if let Err(e) = app.session.execute("prefs.set", json!({"key": SKIP_KEY, "value": true})) {
        // Only loses the "remembered" part: the prompt returns at the next launch.
        log::warn!("android access: couldn't remember \"Not Now\": {e}");
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use egui_kittest::Harness;
    use egui_kittest::kittest::Queryable;

    use super::*;

    /// A host that can check and ask for the grant: `granted` is what a Settings round-trip would
    /// flip, and every ask lands in the returned counter.
    fn android_app(granted: bool) -> (FilmcraftApp, Rc<Cell<bool>>, Rc<Cell<u32>>) {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let flag = Rc::new(Cell::new(granted));
        let asks = Rc::new(Cell::new(0));
        let (g, a) = (flag.clone(), asks.clone());
        app.hooks.shared_storage_root = Some(std::path::PathBuf::from("/storage/emulated/0"));
        app.hooks.shared_storage_granted = Some(Box::new(move || g.get()));
        app.hooks.shared_storage_grant = Some(Box::new(move || a.set(a.get() + 1)));
        (app, flag, asks)
    }

    #[test]
    fn a_missing_grant_asks_at_launch() {
        let (mut app, _, _) = android_app(false);
        maybe_ask(&mut app);
        assert!(app.android_access.is_open());
    }

    #[test]
    fn a_held_grant_never_asks() {
        let (mut app, _, _) = android_app(true);
        maybe_ask(&mut app);
        assert!(!app.android_access.is_open());
    }

    #[test]
    fn a_host_that_cannot_check_the_grant_never_asks() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        maybe_ask(&mut app);
        assert!(!app.android_access.is_open(), "the desktop must not show the Android prompt");
    }

    #[test]
    fn not_now_is_remembered_and_stops_the_launch_ask() {
        let (mut app, _, _) = android_app(false);
        maybe_ask(&mut app);
        assert!(app.android_access.is_open());
        answer_once(&mut app, false);
        assert!(!app.android_access.is_open());
        assert!(app.session.prefs.general.android_skip_all_files_access_prompt);
        maybe_ask(&mut app);
        assert!(!app.android_access.is_open(), "the remembered \"Not Now\" stops the launch ask");
    }

    #[test]
    fn grant_access_opens_the_system_page_through_the_hooks() {
        let (mut app, granted, asks) = android_app(false);
        maybe_ask(&mut app);
        answer_once(&mut app, true);
        assert_eq!(asks.get(), 1, "the ask reaches the host's JNI");
        assert!(!granted.get(), "the grant only holds after the user flips it in Settings");
        assert!(!app.android_access.is_open());
        assert!(!app.session.prefs.general.android_skip_all_files_access_prompt);
    }

    /// The prompt drawn in a real frame: its two buttons answer it.
    fn harness() -> (Harness<'static, FilmcraftApp>, Rc<Cell<bool>>, Rc<Cell<u32>>) {
        let (mut app, flag, asks) = android_app(false);
        app.android_access.requested = true;
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 420.0)).with_step_dt(1.0 / 60.0).build_ui_state(
            |ui, app: &mut FilmcraftApp| {
                let ctx = ui.ctx().clone();
                crate::panels::android_access::show(app, &ctx);
            },
            app,
        );
        h.run_steps(3);
        (h, flag, asks)
    }

    #[test]
    fn the_prompt_draws_and_not_now_answers_it() {
        let (mut h, _, _) = harness();
        assert!(h.state().android_access.is_open(), "the launch prompt is up");
        h.get_by_label("Not Now").click();
        h.run_steps(3);
        assert!(!h.state().android_access.is_open(), "the prompt closed");
        assert!(h.state().session.prefs.general.android_skip_all_files_access_prompt);
    }

    #[test]
    fn the_prompt_draws_and_grant_access_reaches_the_host() {
        let (mut h, _, asks) = harness();
        h.get_by_label("Grant access").click();
        h.run_steps(3);
        assert_eq!(asks.get(), 1);
        assert!(!h.state().android_access.is_open());
    }

    #[test]
    fn the_launch_tick_fires_once_after_the_first_frames() {
        let (app, _, _) = android_app(false);
        let mut h = Harness::builder().with_size(egui::vec2(900.0, 420.0)).build_ui_state(
            |ui, app: &mut FilmcraftApp| {
                let ctx = ui.ctx().clone();
                crate::panels::android_access::launch_tick(app, &ctx);
            },
            app,
        );
        h.run_steps(2);
        assert!(!h.state().android_access.is_open(), "not before the trigger frame");
        h.run_steps(10);
        assert!(h.state().android_access.is_open(), "asked once the first frames have rendered");
        // One-shot: answering it keeps it from springing back at the next tick, even while the
        // grant still does not hold.
        answer_once(h.state_mut(), true);
        assert!(!h.state().android_access.is_open());
        h.run_steps(3);
        assert!(!h.state().android_access.is_open(), "the launch ask is offered once per run");
    }
}

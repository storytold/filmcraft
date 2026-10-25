//! The app as eframe owns it, and how it outlives its window.
//!
//! eframe takes the `FilmcraftApp` for the lifetime of a window and drops it when the event loop
//! returns: when the window closed, and also when a panic unwinds through the event loop. The
//! paint path can panic on a lost or starved graphics device (egui-wgpu's `update_buffers`:
//! "Failed to create staging buffer for index data" when the device refuses the allocation; in a
//! field report another GPU-heavy process filled video memory and closed the app three times in
//! three minutes). [`Hosted`] wraps the app so that whenever eframe drops it, for either reason, the
//! `FilmcraftApp` is parked in an [`AppSlot`] instead of destroyed. `main.rs` catches the panic
//! around `eframe::run_native`, takes the app back out of the slot, and runs a new window with
//! it: the session, project, undo history and control channel carry over; only the window's own
//! state (textures, fonts, the device) is rebuilt. The same slot serves a close the app asked for
//! itself after the device-lost callback fired ([`FilmcraftApp::take_graphics_restart`]).

use std::sync::{Arc, Mutex, PoisonError};

use filmcraft_ui_egui::FilmcraftApp;

/// Where eframe leaves the app when it drops it; shared by the host loop and the app creator.
#[derive(Clone, Default)]
pub struct AppSlot(Arc<Mutex<Option<FilmcraftApp>>>);

impl AppSlot {
    pub fn park(&self, app: FilmcraftApp) {
        *self.0.lock().unwrap_or_else(PoisonError::into_inner) = Some(app);
    }

    pub fn take(&self) -> Option<FilmcraftApp> {
        self.0.lock().unwrap_or_else(PoisonError::into_inner).take()
    }
}

/// Why a window is being recreated, handed to the app creator of the next window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Recovery {
    /// Why the graphics device was lost, as the callback or the panic reported it.
    pub reason: String,
    /// The loss surfaced as a panic in the paint path (caught around the event loop) rather than
    /// as a callback the app acted on.
    pub panicked: bool,
}

/// The eframe app: the `FilmcraftApp` plus the slot it is parked in when eframe lets go of it.
pub struct Hosted {
    app: Option<FilmcraftApp>,
    slot: AppSlot,
}

impl Hosted {
    pub fn new(app: FilmcraftApp, slot: AppSlot) -> Self {
        Self { app: Some(app), slot }
    }
}

impl eframe::App for Hosted {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if let Some(app) = self.app.as_mut() {
            app.raw_input_hook(ctx, raw_input);
        }
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(app) = self.app.as_mut() {
            app.logic(ctx, frame);
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        if let Some(app) = self.app.as_mut() {
            app.ui(ui, frame);
        }
    }

    fn on_exit(&mut self) {
        if let Some(app) = self.app.as_mut() {
            app.on_exit();
        }
    }
}

impl Drop for Hosted {
    fn drop(&mut self) {
        // Runs on a normal close and while a panic unwinds through eframe: either way the app
        // waits in the slot for the host to decide (close for good, or a new window).
        if let Some(app) = self.app.take() {
            self.slot.park(app);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use filmcraft_engine::Session;

    /// The paint-path panic drops the eframe app during unwinding: the `FilmcraftApp` must be
    /// in the slot afterwards, or the session is gone with the window.
    #[test]
    fn a_panic_while_eframe_owns_the_app_parks_it_for_the_next_window() {
        let slot = AppSlot::default();
        let hosted = Hosted::new(FilmcraftApp::new(Session::default()), slot.clone());
        let unwound = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _owned_by_eframe = hosted;
            panic!("Failed to create staging buffer for index data. Index count: 44682");
        }));
        assert!(unwound.is_err());
        let mut app = slot.take().expect("the app survives the panic in the slot");
        assert!(slot.take().is_none(), "taken once");
        // and it can be detached from the dead window and handed to a new one
        app.detach_window();
        assert!(app.graphics_restart_pending().is_none());
    }

    #[test]
    fn a_normal_close_parks_the_app_too() {
        let slot = AppSlot::default();
        drop(Hosted::new(FilmcraftApp::new(Session::default()), slot.clone()));
        assert!(slot.take().is_some());
    }
}

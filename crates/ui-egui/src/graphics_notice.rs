//! The notice shown while the window runs in a fallback graphics mode (#820): the desktop app
//! moved on to DirectX 12 only or a software adapter after earlier starts died in the graphics
//! driver. It says so and offers a one-click way back to the default mode for the next start.
//!
//! The same notice says when the graphics device was lost while the app ran (out of GPU memory, a
//! driver reset; see [`crate::gpu_health`]): the host recreated the window, in software where
//! the platform has a software adapter, and the user should save. Help ▸ Graphics Recovery…
//! explains it ([`GraphicsRecovery`]).
//!
//! Automation ids: `graphics.notice` (the notice), `graphics.notice.reset` (Use Default Graphics),
//! `graphics.notice.save` (Save Project, after a device loss), `graphics.notice.close`.

use crate::{FilmcraftApp, i18n};

/// Puts the default graphics mode back for the next start; the error says what could not be saved.
pub type ResetFn = Box<dyn FnMut() -> Result<(), String>>;

/// What the notice reports.
pub enum Kind {
    /// The app started in a fallback mode after earlier starts failed.
    FallbackStart,
    /// The graphics device was lost while the app ran and the window was recreated; `software`
    /// when it now renders through a software adapter.
    DeviceLost { reason: String, software: bool },
}

/// A fallback graphics mode, as the host reports it.
pub struct GraphicsNotice {
    /// The mode's English name ("DirectX 12 only", "software rendering"), translated when shown.
    pub mode: String,
    pub kind: Kind,
    reset: Option<ResetFn>,
    /// The default mode is saved for the next start.
    pub reset_done: bool,
    /// Closed for this session.
    pub closed: bool,
}

impl GraphicsNotice {
    /// A fallback start, with the button that saves the default mode for the next start.
    pub fn new(mode: &str, reset: ResetFn) -> Self {
        Self { mode: mode.to_string(), kind: Kind::FallbackStart, reset: Some(reset), reset_done: false, closed: false }
    }

    /// The window was recreated in `mode` after the graphics device was lost for `reason`.
    pub fn device_lost(mode: &str, reason: &str, software: bool) -> Self {
        Self { mode: mode.to_string(), kind: Kind::DeviceLost { reason: reason.to_string(), software }, reset: None, reset_done: false, closed: false }
    }

    /// Use Default Graphics: save the default mode for the next start.
    pub fn reset(&mut self) -> Result<(), String> {
        let Some(reset) = self.reset.as_mut() else { return Ok(()) };
        reset()?;
        self.reset_done = true;
        Ok(())
    }

    /// The headline, as the user reads it: the required wording after a device loss.
    pub fn title(&self) -> String {
        match &self.kind {
            Kind::FallbackStart => tlf!("Safe graphics mode: {mode}", mode = i18n::t(&self.mode)),
            Kind::DeviceLost { software: true, .. } => tl!("Graphics device lost; rendering in software. Save your work.").to_string(),
            Kind::DeviceLost { software: false, .. } => tl!("Graphics device lost; the window was recreated. Save your work.").to_string(),
        }
    }
}

/// What happened to the graphics device in this session, for Help ▸ Graphics Recovery….
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct GraphicsRecovery {
    /// Why the device was lost, as wgpu or the panic reported it.
    pub reason: String,
    /// The English name of the mode the window was recreated in.
    pub mode: String,
    /// The window now renders through a software adapter.
    pub software: bool,
    /// When, as local time text.
    pub at: String,
    /// The window was recreated after a panic escaped the paint path (the crash log has the
    /// backtrace), rather than after a device-lost callback alone.
    pub panicked: bool,
}

/// Draw the notice (bottom right, above the status bar) until it is closed.
pub(crate) fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut notice) = app.graphics_notice.take() else { return };
    if notice.closed {
        app.graphics_notice = Some(notice);
        return;
    }
    let title = notice.title();
    let (mut reset, mut save, mut close) = (false, false, false);
    let shown = egui::Window::new("graphics-notice")
        .id(egui::Id::new("graphics-notice"))
        .title_bar(false)
        .collapsible(false)
        .resizable(false)
        .anchor(egui::Align2::RIGHT_BOTTOM, [-12.0, -32.0])
        .show(ctx, |ui| {
            ui.set_max_width(360.0);
            ui.label(egui::RichText::new(&title).strong());
            ui.add_space(4.0);
            match &notice.kind {
                Kind::FallbackStart if notice.reset_done => {
                    ui.label(tl!("The default graphics are used from the next start."));
                }
                Kind::FallbackStart => {
                    ui.label(tl!("An earlier start failed with the default graphics. Updating the graphics driver usually fixes this."));
                }
                Kind::DeviceLost { reason, .. } => {
                    ui.label(tlf!("The graphics device stopped responding ({reason}). Other programs using the GPU may have taken its memory: close them and restart FilmCraft to return to GPU rendering. Help ▸ Graphics Recovery… has the details.", reason));
                }
            }
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                match notice.kind {
                    Kind::FallbackStart if !notice.reset_done => {
                        let label = tl!("Use Default Graphics");
                        let r = ui.button(label);
                        app.auto.add_response("graphics.notice.reset", &r, label);
                        reset = r.clicked();
                    }
                    Kind::DeviceLost { .. } => {
                        let label = tl!("Save Project");
                        let r = ui.button(label);
                        app.auto.add_response("graphics.notice.save", &r, label);
                        save = r.clicked();
                    }
                    Kind::FallbackStart => {}
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
    if save {
        let _ = crate::menus::invoke(app, ctx, "file.save", serde_json::json!({}));
    }
}

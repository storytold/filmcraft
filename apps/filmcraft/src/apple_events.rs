//! macOS open-documents and quit Apple events.
//!
//! Finder double-clicks, Open With, drops on the Dock icon and `open -a FilmCraft p.fcproj` don't
//! pass paths on the command line: LaunchServices sends the running (or just-launched) app a
//! `kAEOpenDocuments` ('odoc') Apple event. winit 0.30 doesn't handle it and owns the
//! `NSApplicationDelegate`, so the project never opened. Handling it ourselves needs Objective-C
//! class declarations, i.e. `unsafe`, which this workspace forbids outside `crates/platform`. The
//! audited `fmv-macos-events` crate (also used by PdfCraft and PhotoCraft) wraps exactly that, an
//! `NSAppleEventManager` handler registered before Finder's launch event that leaves winit's
//! delegate alone, behind a safe main-thread API.

use filmcraft_ui_egui::{FilmcraftApp, menus};
use fmv_macos_events::{Event, Inbox, Registration};
use serde_json::json;

/// Keeps the Apple-event handlers registered; hold it until the event loop returns.
pub struct AppleEvents {
    _registration: Registration,
    inbox: Inbox,
}

impl AppleEvents {
    /// Register the handlers. Call on the main thread before the event loop starts, so the event
    /// that launched the app (a Finder double-click) is caught too.
    pub fn install() -> Self {
        let (registration, inbox) = Registration::install();
        Self { _registration: registration, inbox }
    }

    /// The queue the app drains every frame ([`Desktop`]); events arriving later wake `ctx`.
    pub fn connect(&self, ctx: &egui::Context) -> Inbox {
        let ctx = ctx.clone();
        self.inbox.set_wake(move || ctx.request_repaint());
        self.inbox.clone()
    }
}

/// The app plus the Apple-event queue it drains every frame.
pub struct Desktop {
    app: FilmcraftApp,
    inbox: Inbox,
}

impl Desktop {
    pub fn new(app: FilmcraftApp, inbox: Inbox) -> Self {
        Self { app, inbox }
    }

    /// Open what arrived since the last frame as the command line does: a project opens (File ▸
    /// Open), other files are imported. A quit event closes the window.
    fn poll(&mut self, ctx: &egui::Context) {
        for e in self.inbox.drain() {
            match e {
                Event::Open(paths) => {
                    let paths: Vec<String> = paths.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                    let (projects, media): (Vec<String>, Vec<String>) = paths.into_iter().partition(|p| p.to_ascii_lowercase().ends_with(".fcproj"));
                    if let Some(p) = projects.first()
                        && let Err(e) = menus::invoke(&mut self.app, ctx, "file.open", json!({"path": p}))
                    {
                        log::error!("{p}: {e}");
                    }
                    if !media.is_empty()
                        && let Err(e) = menus::invoke(&mut self.app, ctx, "file.import", json!({"paths": media}))
                    {
                        log::error!("import: {e}");
                    }
                }
                Event::Quit => ctx.send_viewport_cmd(egui::ViewportCommand::Close),
            }
        }
    }
}

impl eframe::App for Desktop {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.poll(ctx);
        self.app.logic(ctx, frame);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
    }

    fn on_exit(&mut self) {
        self.app.on_exit();
    }
}

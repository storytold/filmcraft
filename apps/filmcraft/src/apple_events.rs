//! macOS open-documents and quit Apple events.
//!
//! Finder double-clicks, Open With, drops on the Dock icon and `open -a FilmCraft clip.mp4` don't
//! pass paths on the command line: Launch Services sends the running (or just-launched) app a
//! `kAEOpenDocuments` ('odoc') Apple event. winit 0.30 doesn't handle it and owns the
//! `NSApplicationDelegate`, and handling it ourselves needs Objective-C class declarations, i.e.
//! `unsafe`, which this workspace allows only in `crates/platform`. The audited
//! `fmv-macos-events` crate, which PhotoCraft uses for the same purpose, wraps exactly that (an
//! `NSAppleEventManager` handler registered before Finder's launch event, leaving winit's
//! delegate alone) behind a safe main-thread API; this module adapts it to the
//! [`OsEvent`](filmcraft_ui_egui::OsEvent)s the UI polls every frame. Without a handler the
//! event reaches only AppKit, which answers "FilmCraft cannot open files in the “MPEG-4 movie”
//! format", and the file never arrives.
//!
//! On the other platforms files arrive as arguments (see `args`) and [`Source`] is inert.

use filmcraft_ui_egui::OsEventsFn;

/// Keeps the Apple-event handlers registered; hold it on the main thread until the event loop
/// returns.
pub struct Guard {
    #[cfg(target_os = "macos")]
    _registration: fmv_macos_events::Registration,
}

/// Where the UI reads the OS requests from. Cloneable, so every start attempt's app creator can
/// connect it.
#[derive(Clone, Default)]
pub struct Source {
    #[cfg(target_os = "macos")]
    inbox: Option<fmv_macos_events::Inbox>,
}

/// Register the handlers. Call on the main thread before the event loop starts, so the event that
/// launched the app (a Finder double-click) is caught too.
pub fn install() -> (Guard, Source) {
    #[cfg(target_os = "macos")]
    {
        let (registration, inbox) = fmv_macos_events::Registration::install();
        (Guard { _registration: registration }, Source { inbox: Some(inbox) })
    }
    #[cfg(not(target_os = "macos"))]
    {
        (Guard {}, Source::default())
    }
}

impl Source {
    /// The queue the UI drains ([`HostHooks::os_events`](filmcraft_ui_egui::HostHooks::os_events));
    /// events arriving later wake `ctx`. `None` where the OS sends none.
    pub fn connect(&self, ctx: &egui::Context) -> Option<OsEventsFn> {
        #[cfg(target_os = "macos")]
        {
            use filmcraft_ui_egui::OsEvent;
            use fmv_macos_events::Event;
            let inbox = self.inbox.clone()?;
            let ctx = ctx.clone();
            inbox.set_wake(move || ctx.request_repaint());
            Some(Box::new(move || {
                inbox
                    .drain()
                    .into_iter()
                    .map(|e| match e {
                        Event::Open(paths) => OsEvent::Open(paths.into_iter().map(|p| p.to_string_lossy().into_owned()).collect()),
                        Event::Quit => OsEvent::Quit,
                    })
                    .collect()
            }))
        }
        #[cfg(not(target_os = "macos"))]
        {
            let _ = ctx;
            None
        }
    }
}

//! macOS open-documents and quit Apple events.
//!
//! Finder double-clicks, Open With, drops on the Dock icon and `open -a FilmCraft cut.fcproj` don't
//! pass paths on the command line: LaunchServices sends the running (or just-launched) app a
//! `kAEOpenDocuments` ('odoc') Apple event. winit 0.30 doesn't handle it and owns the
//! `NSApplicationDelegate`, so AppKit answered "FilmCraft cannot open files in the “FilmCraft
//! Project” format". Handling it ourselves needs Objective-C class declarations, i.e. `unsafe`,
//! which this workspace forbids outside the OS media FFI in `crates/platform` (ADR 0001). The
//! audited `fmv-macos-events` crate (also used by PhotoCraft and PDFCraft) wraps exactly that, an
//! `NSAppleEventManager` handler registered before Finder's launch event that leaves winit's
//! delegate alone, behind a safe main-thread API. This module adapts it to the [`OsEvent`]s that
//! the UI polls every frame.

use filmcraft_ui_egui::{OsEvent, OsEventsFn};
use fmv_macos_events::{Event, Inbox, Registration};
use std::path::PathBuf;

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

    /// For the app creator: connects the queue to the app once it exists (see [`connect`]).
    pub fn connector(&self) -> crate::OsEventsConnect {
        let inbox = self.inbox.clone();
        Box::new(move |ctx| connect(inbox, ctx))
    }
}

/// Wake `ctx` for events arriving from now on. Returns the files the OS asked to open before the
/// app existed (the double-click that launched FilmCraft), to open like command-line files, and
/// the poll the UI drains every frame (`HostHooks::os_events`).
fn connect(inbox: Inbox, ctx: &egui::Context) -> (Vec<String>, OsEventsFn) {
    let ctx = ctx.clone();
    inbox.set_wake(move || ctx.request_repaint());
    let mut launch_files = Vec::new();
    let mut early = Vec::new();
    for event in inbox.drain() {
        match event {
            Event::Open(paths) => launch_files.extend(paths.into_iter().map(path_string)),
            Event::Quit => early.push(OsEvent::Quit),
        }
    }
    let poll = Box::new(move || {
        let mut events = std::mem::take(&mut early);
        events.extend(inbox.drain().into_iter().map(|event| match event {
            Event::Open(paths) => {
                let files: Vec<String> = paths.into_iter().map(path_string).collect();
                let (project, media) = crate::args::project_and_media(&files);
                OsEvent::Open { project, media }
            }
            Event::Quit => OsEvent::Quit,
        }));
        events
    });
    (launch_files, poll)
}

fn path_string(path: PathBuf) -> String {
    path.to_string_lossy().into_owned()
}

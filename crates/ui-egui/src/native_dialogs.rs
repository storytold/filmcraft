//! Native file dialogs that don't stop the frame loop (#260).
//!
//! A dialog is asked for with [`FilmcraftApp::pick`]: what to show ([`FileDialog`]) and what to do
//! with the chosen paths. A host that can show dialogs without blocking ([`HostHooks::run_dialog`]:
//! the desktop app on Linux, where the XDG portal call would otherwise hold the UI thread until the
//! user chooses, and GNOME offers to force-quit the window after about five seconds) gets the
//! dialog and answers on a channel; the frame loop keeps running, and the continuation runs on the
//! frame the answer arrives ([`FilmcraftApp::poll_native_dialog`]). Without it the host's
//! synchronous pickers (`pick_files`, `pick_save_as`, …) are called and the continuation runs at
//! once, as before: headless tests, the control channel's stand-ins and other hosts are unchanged.
//!
//! Two entry points:
//! - [`FilmcraftApp::pick`] for menu and control-channel commands: with the synchronous pickers
//!   the continuation runs at once and its result is the command's (agents and tests see what
//!   they saw before); with the runner the command answers `{"dialog": "open"}`.
//! - [`FilmcraftApp::pick_ui`] for a panel's Browse… / Import… / Export… button: the continuation
//!   always runs at the start of a later frame, before the panels draw, since a panel works on a
//!   copy of its state that it writes back at the end of the frame. It writes into the dialog's
//!   stored state (`app.ui.…`, an egui temporary); when the user has closed that dialog meanwhile
//!   there is nothing to write into and the path is dropped.
//!
//! One dialog is open at a time; asking for another one meanwhile is refused.

use std::sync::mpsc::{Receiver, TryRecvError};
use std::time::Duration;

use serde_json::{Value, json};

use crate::{FilmcraftApp, HostHooks, RelinkHint};

/// A native file dialog. Filters are a name and extensions without dots.
#[derive(Clone, Debug)]
pub enum FileDialog {
    /// Choose one or more files (File ▸ Import; one file where only the first is used).
    OpenFiles { exts: Vec<String> },
    /// Choose one file.
    OpenFile { filter: String, exts: Vec<String> },
    /// Choose a FilmCraft project to open.
    OpenProject,
    /// Save a FilmCraft project, suggesting `name`.
    SaveProject { name: String },
    /// Save a file, suggesting `name`.
    SaveAs { filter: String, exts: Vec<String>, name: String },
    /// Choose a folder, starting at `at` when there is one.
    Folder { at: Option<String> },
    /// Choose one file to relink to (Link Media ▸ Locate…, Attach Proxies, Reconnect Full
    /// Resolution): the host's relink picker, which may take over the command (the web).
    Relink { exts: Vec<String>, hint: RelinkHint },
}

/// What to do with the chosen paths (none: cancelled). Its result is the request's (control
/// channel, menu) when the dialog answers at once; otherwise an error goes to the status line.
pub(crate) type Continuation = Box<dyn FnOnce(&mut FilmcraftApp, Vec<String>) -> Result<Value, String>>;

/// The dialog a host is showing, and what to do with its answer.
pub(crate) struct PendingDialog {
    answer: Receiver<Vec<String>>,
    then: Continuation,
}

fn strings(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_string()).collect()
}

impl FileDialog {
    pub fn open_files(exts: &[&str]) -> Self {
        Self::OpenFiles { exts: strings(exts) }
    }
    pub fn open_file(filter: &str, exts: &[&str]) -> Self {
        Self::OpenFile { filter: filter.to_string(), exts: strings(exts) }
    }
    pub fn save_as(filter: &str, exts: &[&str], name: &str) -> Self {
        Self::SaveAs { filter: filter.to_string(), exts: strings(exts), name: name.to_string() }
    }

    /// Whether `hooks` can show this dialog at all (a host without the picker leaves it out).
    pub fn available(&self, hooks: &HostHooks) -> bool {
        hooks.run_dialog.is_some()
            || match self {
                Self::OpenFiles { .. } => hooks.pick_files.is_some(),
                Self::OpenFile { .. } => hooks.pick_open_file.is_some(),
                Self::OpenProject => hooks.pick_open_project.is_some(),
                Self::SaveProject { .. } => hooks.pick_save.is_some(),
                Self::SaveAs { .. } => hooks.pick_save_as.is_some(),
                Self::Folder { at } => hooks.pick_folder.is_some() || (at.is_some() && hooks.pick_folder_at.is_some()),
                Self::Relink { .. } => hooks.pick_file_for_relink.is_some() || hooks.pick_files.is_some(),
            }
    }

    /// Show the dialog with the host's synchronous pickers, as before #260.
    fn run_now(self, hooks: &mut HostHooks) -> Vec<String> {
        let one = |p: Option<String>| p.into_iter().collect::<Vec<_>>();
        match self {
            Self::OpenFiles { exts } => hooks.pick_files.as_mut().map(|f| f(&refs(&exts))).unwrap_or_default(),
            Self::OpenFile { filter, exts } => one(hooks.pick_open_file.as_mut().and_then(|f| f(&filter, &refs(&exts)))),
            Self::OpenProject => one(hooks.pick_open_project.as_mut().and_then(|f| f())),
            Self::SaveProject { name } => one(hooks.pick_save.as_mut().and_then(|f| f(&name))),
            Self::SaveAs { filter, exts, name } => one(hooks.pick_save_as.as_mut().and_then(|f| f(&filter, &refs(&exts), &name))),
            Self::Folder { at } => one(match (at, hooks.pick_folder_at.as_mut()) {
                (Some(dir), Some(f)) => f(&dir),
                _ => hooks.pick_folder.as_mut().and_then(|f| f()),
            }),
            Self::Relink { exts, hint } => match hooks.pick_file_for_relink.as_mut() {
                Some(f) => one(f(&refs(&exts), Some(hint))),
                None => hooks.pick_files.as_mut().map(|f| f(&refs(&exts))).unwrap_or_default().into_iter().take(1).collect(),
            },
        }
    }
}

fn refs(v: &[String]) -> Vec<&str> {
    v.iter().map(String::as_str).collect()
}

impl FilmcraftApp {
    /// Show `dialog`, then call `then` with the chosen paths (empty when cancelled). With a host
    /// that shows dialogs without blocking, this returns `{"dialog": "open"}` and `then` runs on a
    /// later frame; otherwise `then` runs now and its result is returned. While a dialog is open
    /// another one is refused.
    pub fn pick(&mut self, dialog: FileDialog, then: impl FnOnce(&mut FilmcraftApp, Vec<String>) -> Result<Value, String> + 'static) -> Result<Value, String> {
        if self.native_dialog.is_some() {
            return Err(tl!("A file dialog is already open").into());
        }
        if let Some(run) = self.hooks.run_dialog.as_mut() {
            self.native_dialog = Some(PendingDialog { answer: run(dialog), then: Box::new(then) });
            return Ok(json!({"dialog": "open"}));
        }
        let paths = dialog.run_now(&mut self.hooks);
        then(self, paths)
    }

    /// [`Self::pick`] for a panel's button: `then` returns nothing, and a refusal (another dialog is
    /// open) goes to the status line. `then` always runs at the start of a later frame, also with
    /// the host's synchronous pickers: a panel works on a copy of its state and writes it back at
    /// the end of the frame, which would overwrite a path written in the middle of it.
    pub fn pick_ui(&mut self, dialog: FileDialog, then: impl FnOnce(&mut FilmcraftApp, Vec<String>) + 'static) {
        if self.native_dialog.is_some() {
            self.ui.status = tl!("A file dialog is already open").into();
            return;
        }
        let answer = match self.hooks.run_dialog.as_mut() {
            Some(run) => run(dialog),
            None => {
                let (tx, rx) = std::sync::mpsc::channel();
                // the receiver is alive (held here): sending can't fail
                let _ = tx.send(dialog.run_now(&mut self.hooks));
                rx
            }
        };
        let then: Continuation = Box::new(move |app, paths| {
            then(app, paths);
            Ok(Value::Null)
        });
        self.native_dialog = Some(PendingDialog { answer, then });
    }

    /// Whether a file dialog is open (one the host shows without blocking).
    pub fn native_dialog_open(&self) -> bool {
        self.native_dialog.is_some()
    }

    /// Run the open dialog's continuation once it has answered (called first thing every frame).
    /// While it is open the frame loop keeps polling at a low rate.
    pub(crate) fn poll_native_dialog(&mut self, ctx: &egui::Context) {
        let Some(pending) = &self.native_dialog else { return };
        let paths = match pending.answer.try_recv() {
            Ok(paths) => paths,
            Err(TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(100));
                return;
            }
            // the host's dialog ended without an answer (its thread failed): as if cancelled
            Err(TryRecvError::Disconnected) => Vec::new(),
        };
        let Some(pending) = self.native_dialog.take() else { return };
        if let Err(e) = (pending.then)(self, paths) {
            self.ui.status = e;
        }
        ctx.request_repaint();
    }
}

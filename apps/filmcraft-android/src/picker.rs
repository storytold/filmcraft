//! Route imports and relinking through Android's Storage Access Framework.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::Duration;

use android_activity::AndroidApp;
use filmcraft_platform::android_picker;
use filmcraft_ui_egui::native_dialogs::FileDialog;

use crate::browser::Request;
use crate::media_storage::copy_picker_batch;

pub(super) struct Picker {
    pub request: Request,
    document: u64,
    destination: PathBuf,
    phase: Phase,
}

enum Phase {
    Launching(Receiver<Result<(), String>>),
    Waiting,
    Copying { ready: Receiver<Result<ImportedFiles, String>>, progress: Arc<android_picker::CopyProgress> },
}

pub(super) enum Outcome {
    Selected(ImportedFiles),
    Cancelled,
    Fallback(String),
    Failed(String),
}

/// If the project changes or the app closes during a copy, discard just this import batch.
pub(super) struct ImportedFiles(Vec<PathBuf>);

impl ImportedFiles {
    pub fn deliver(mut self, reply: Sender<Vec<String>>) {
        if reply.send(self.0.iter().map(|path| path.to_string_lossy().into_owned()).collect()).is_ok() {
            self.0.clear();
        }
    }
}

impl Drop for ImportedFiles {
    fn drop(&mut self) {
        for path in &self.0 {
            let _ = std::fs::remove_file(path);
            if let Some(parent) = path.parent() {
                let _ = std::fs::remove_dir(parent);
            }
        }
    }
}

impl Picker {
    pub fn supports(dialog: &FileDialog) -> bool {
        matches!(dialog, FileDialog::OpenFiles { .. } | FileDialog::OpenFile { .. } | FileDialog::Relink { .. })
    }

    pub fn new(request: Request, document: u64, destination: &Path, android: &AndroidApp) -> Self {
        let multi = matches!(request.dialog, FileDialog::OpenFiles { .. });
        let launch = android_picker::launch_documents(android, multi, &[]);
        Self { request, document, destination: destination.to_owned(), phase: Phase::Launching(launch) }
    }

    pub fn show_progress(&self, ui: &mut egui::Ui) {
        let Phase::Copying { progress, .. } = &self.phase else { return };
        let bytes = progress.copied.load(Ordering::Relaxed);
        let total = progress.total.load(Ordering::Relaxed);
        let cancelling = progress.cancelled.load(Ordering::Relaxed);
        if total > 0 {
            let label = format!("{:.1} / {:.1} MiB", bytes as f64 / 1048576.0, total.max(bytes) as f64 / 1048576.0);
            ui.add(egui::ProgressBar::new(bytes as f32 / total.max(bytes) as f32).desired_width(125.0).text(label));
        } else {
            ui.spinner();
            ui.label(format!("{:.1} MiB copied", bytes as f64 / 1048576.0));
        }
        if ui.add_enabled(!cancelling, egui::Button::new(if cancelling { "Cancelling import…" } else { "Cancel import" })).clicked() {
            self.cancel_copy();
        }
    }

    pub fn cancel_copy(&self) {
        if let Phase::Copying { progress, .. } = &self.phase {
            progress.cancelled.store(true, Ordering::Relaxed);
        }
    }

    pub fn belongs_to(&self, document: u64) -> bool {
        self.document == document
    }

    pub fn poll(&mut self, ctx: &egui::Context, android: &AndroidApp, document: u64) -> Option<Outcome> {
        let current = self.belongs_to(document);
        if !current {
            self.cancel_copy();
        }
        ctx.request_repaint_after(Duration::from_millis(100));
        match &self.phase {
            Phase::Launching(launch) => match launch.try_recv() {
                Ok(Ok(())) => self.phase = Phase::Waiting,
                Ok(Err(error)) => return Some(Outcome::Fallback(error)),
                Err(TryRecvError::Disconnected) => return Some(Outcome::Fallback("The Android picker could not start.".into())),
                Err(TryRecvError::Empty) => {}
            },
            Phase::Waiting => match android_picker::poll_documents(android) {
                Ok(Some(files)) => {
                    if !current || files.is_empty() {
                        return Some(Outcome::Cancelled);
                    }
                    let extensions = match &self.request.dialog {
                        FileDialog::OpenFiles { exts } | FileDialog::OpenFile { exts, .. } | FileDialog::Relink { exts, .. } => exts.clone(),
                        _ => return Some(Outcome::Failed("This picker request has no file type.".into())),
                    };
                    let (result, ready) = mpsc::channel();
                    let progress = Arc::new(android_picker::CopyProgress::default());
                    let worker_progress = progress.clone();
                    let android = android.clone();
                    let destination = self.destination.clone();
                    let repaint = ctx.clone();
                    let worker = std::thread::Builder::new().name("filmcraft-import".into()).spawn(move || {
                        let imported = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                            copy_picker_batch(&destination, |staging| android_picker::copy_documents(&android, files, staging, &extensions, &worker_progress))
                                .map(ImportedFiles)
                        }))
                        .unwrap_or_else(|_| Err("FilmCraft could not copy the selected files.".into()));
                        let _ = result.send(imported);
                        repaint.request_repaint();
                    });
                    if let Err(error) = worker {
                        return Some(Outcome::Failed(format!("Could not start importing files. {error}")));
                    }
                    self.phase = Phase::Copying { ready, progress };
                }
                Ok(None) => {}
                Err(error) => return Some(Outcome::Fallback(error)),
            },
            Phase::Copying { ready, progress } => match ready.try_recv() {
                Ok(_) if !current || progress.cancelled.load(Ordering::Relaxed) => return Some(Outcome::Cancelled),
                Ok(Ok(files)) => return Some(Outcome::Selected(files)),
                Ok(Err(error)) => return Some(Outcome::Failed(error)),
                Err(TryRecvError::Disconnected) => return Some(Outcome::Failed("The import stopped before the files could be copied.".into())),
                Err(TryRecvError::Empty) => {}
            },
        }
        None
    }
}

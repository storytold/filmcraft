//! Preview disk usage and restore a portable project selected in the Android Files app.
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread;
use std::time::Duration;

use android_activity::AndroidApp;
use filmcraft_platform::android_picker;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::menus::invoke_command;
use serde_json::json;

mod archive;
use archive::Archive;

pub(super) struct Restore {
    root: Option<PathBuf>,
    open: bool,
    phase: Phase,
    status: String,
}

enum Phase {
    Idle,
    Launching(Receiver<Result<(), String>>),
    Waiting,
    Inspecting(Receiver<Result<Plan, String>>, Arc<android_picker::CopyProgress>),
    Ready(Plan),
    Extracting(Receiver<Result<PathBuf, String>>, Arc<ExtractProgress>),
    Complete(PathBuf),
}

struct Plan {
    archive: Archive,
    folder: RestoreFolder,
    available: u64,
}

struct RestoreFolder {
    path: PathBuf,
    keep: bool,
}

impl RestoreFolder {
    fn retain(mut self) {
        self.keep = true;
    }
}

impl Drop for RestoreFolder {
    fn drop(&mut self) {
        if !self.keep {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

#[derive(Default)]
struct ExtractProgress {
    total: u64,
    written: AtomicU64,
    cancelled: AtomicBool,
}

impl Restore {
    pub fn new(root: Option<PathBuf>) -> Self {
        Self { root, open: false, phase: Phase::Idle, status: String::new() }
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn picker_busy(&self) -> bool {
        matches!(self.phase, Phase::Launching(_) | Phase::Waiting)
    }

    pub fn on_exit(&self, android: &AndroidApp) {
        if self.picker_busy() {
            android_picker::cancel_documents(android);
        }
        self.cancel();
    }

    fn cancel(&self) {
        match &self.phase {
            Phase::Inspecting(_, progress) => progress.cancelled.store(true, Ordering::Relaxed),
            Phase::Extracting(_, progress) => progress.cancelled.store(true, Ordering::Relaxed),
            _ => {}
        }
    }

    pub fn show(&mut self, ctx: &egui::Context, app: &mut FilmcraftApp, android: &AndroidApp, picker_in_use: bool) {
        self.poll(ctx, android);
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Restore backup").open(&mut open).vscroll(true).default_width(420.0).show(ctx, |ui| {
            match &self.phase {
                Phase::Idle | Phase::Complete(_) => {
                    if let Phase::Complete(path) = &self.phase {
                        ui.label(format!("Restored {}", path.display()));
                        if ui.button("Open restored project").clicked()
                            && let Err(error) = invoke_command(app, ctx, "file.open", json!({"path": path}))
                        {
                            self.status = error;
                        }
                    }
                    if ui.add_enabled(self.root.is_some() && !picker_in_use, egui::Button::new("Choose backup ZIP")).clicked() {
                        self.status.clear();
                        self.phase = Phase::Launching(android_picker::launch_documents(android, false, &["application/zip", "application/x-zip-compressed"]));
                    }
                }
                Phase::Launching(_) | Phase::Waiting => {
                    ui.label("Choose a FilmCraft backup in Android Files.");
                }
                Phase::Inspecting(_, progress) => {
                    display_progress(ui, progress.copied.load(Ordering::Relaxed), progress.total.load(Ordering::Relaxed));
                    if ui.add_enabled(!progress.cancelled.load(Ordering::Relaxed), egui::Button::new("Cancel")).clicked() {
                        self.cancel();
                    }
                }
                Phase::Ready(plan) => {
                    ui.label(plan.archive.project.display().to_string());
                    ui.label(format!("{} required · {} available", size(plan.archive.bytes), size(plan.available)));
                    let sufficient = plan.archive.bytes <= plan.available;
                    if ui.add_enabled(sufficient, egui::Button::new("Restore project")).clicked() {
                        self.start_extract(ctx, android);
                    } else if ui.button("Discard selection").clicked() {
                        self.phase = Phase::Idle;
                    }
                }
                Phase::Extracting(_, progress) => {
                    display_progress(ui, progress.written.load(Ordering::Relaxed), progress.total);
                    if ui.add_enabled(!progress.cancelled.load(Ordering::Relaxed), egui::Button::new("Cancel restore")).clicked() {
                        self.cancel();
                    }
                }
            }
            if !self.status.is_empty() {
                ui.label(&self.status);
            }
        });
        self.open = open;
    }

    fn start_extract(&mut self, ctx: &egui::Context, android: &AndroidApp) {
        let Phase::Ready(plan) = std::mem::replace(&mut self.phase, Phase::Idle) else { return };
        let progress = Arc::new(ExtractProgress { total: plan.archive.bytes, ..Default::default() });
        let stats = progress.clone();
        let android = android.clone();
        let ready = spawn(ctx, "filmcraft-restore-extract", move || {
            if android_picker::available_bytes(&android, &plan.folder.path)? < stats.total {
                return Err("Free up storage before restoring this project.".into());
            }
            let result = plan.archive.extract(&plan.folder.path.join("Project"), &stats.cancelled, &stats.written)?;
            fs::remove_dir_all(plan.folder.path.join(".archive")).map_err(|error| error.to_string())?;
            if stats.cancelled.load(Ordering::Relaxed) {
                return Err("Restore cancelled.".into());
            }
            plan.folder.retain();
            Ok(result)
        });
        match ready {
            Ok(ready) => self.phase = Phase::Extracting(ready, progress),
            Err(error) => self.status = error,
        }
    }

    fn poll(&mut self, ctx: &egui::Context, android: &AndroidApp) {
        match &self.phase {
            Phase::Launching(result) => {
                if let Some(result) = outcome(result) {
                    match result {
                        Ok(()) => self.phase = Phase::Waiting,
                        Err(error) => self.fail(error),
                    }
                }
            }
            Phase::Waiting => match android_picker::poll_documents(android) {
                Ok(Some(uris)) if uris.len() == 1 => self.inspect(ctx, android, uris),
                Ok(Some(_)) => self.phase = Phase::Idle,
                Ok(None) => {}
                Err(error) => self.fail(error),
            },
            Phase::Inspecting(ready, _) => {
                if let Some(result) = outcome(ready) {
                    match result {
                        Ok(plan) => self.phase = Phase::Ready(plan),
                        Err(error) => self.fail(error),
                    }
                }
            }
            Phase::Extracting(ready, _) => {
                if let Some(result) = outcome(ready) {
                    match result {
                        Ok(path) => self.phase = Phase::Complete(path),
                        Err(error) => self.fail(error),
                    }
                }
            }
            Phase::Idle | Phase::Ready(_) | Phase::Complete(_) => return,
        }
        ctx.request_repaint_after(Duration::from_millis(100));
    }

    fn fail(&mut self, error: String) {
        self.phase = Phase::Idle;
        self.status = error;
    }

    fn inspect(&mut self, ctx: &egui::Context, android: &AndroidApp, uris: Vec<String>) {
        let Some(root) = self.root.clone() else {
            self.fail("No project folder is available.".into());
            return;
        };
        let progress = Arc::new(android_picker::CopyProgress::default());
        let stats = progress.clone();
        let android = android.clone();
        let ready = spawn(ctx, "filmcraft-restore-inspect", move || {
            let folder = reserve(&root)?;
            let files = android_picker::copy_documents(&android, uris, &folder.path.join(".archive"), &["zip".into()], &stats);
            if stats.cancelled.load(Ordering::Relaxed) {
                return Err("Restore cancelled.".into());
            }
            let file = files?.into_iter().next().ok_or("The selected ZIP is unavailable.")?;
            let archive = Archive::open(&file)?;
            let available = android_picker::available_bytes(&android, &folder.path)?;
            Ok(Plan { archive, folder, available })
        });
        match ready {
            Ok(ready) => self.phase = Phase::Inspecting(ready, progress),
            Err(error) => self.fail(error),
        }
    }
}

fn reserve(root: &Path) -> Result<RestoreFolder, String> {
    fs::create_dir_all(root).map_err(|error| error.to_string())?;
    for number in 1..=1024 {
        let path = root.join(format!("Restored backup {number}"));
        match fs::create_dir(&path) {
            Ok(()) => return Ok(RestoreFolder { path, keep: false }),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.to_string()),
        }
    }
    Err("Could not find an unused restore folder.".into())
}

fn spawn<T: Send + 'static>(
    ctx: &egui::Context,
    name: &str,
    work: impl FnOnce() -> Result<T, String> + Send + 'static,
) -> Result<Receiver<Result<T, String>>, String> {
    let ctx = ctx.clone();
    let (reply, result) = mpsc::channel();
    thread::Builder::new()
        .name(name.into())
        .spawn(move || {
            let _ = reply.send(work());
            ctx.request_repaint();
        })
        .map_err(|error| error.to_string())?;
    Ok(result)
}

fn outcome<T>(result: &Receiver<Result<T, String>>) -> Option<Result<T, String>> {
    match result.try_recv() {
        Ok(outcome) => Some(outcome),
        Err(TryRecvError::Disconnected) => Some(Err("The restore worker stopped.".into())),
        Err(TryRecvError::Empty) => None,
    }
}

fn display_progress(ui: &mut egui::Ui, copied: u64, total: u64) {
    if total > 0 {
        ui.add(egui::ProgressBar::new(copied.min(total) as f32 / total as f32).text(format!("{} / {}", size(copied), size(total))));
    } else {
        ui.spinner();
        ui.label(size(copied));
    }
}

fn size(bytes: u64) -> String {
    format!("{:.1} MiB", bytes as f64 / 1_048_576.0)
}

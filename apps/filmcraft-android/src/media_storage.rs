//! Android picker copy usage and cleanup of interrupted transfers.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::time::Duration;

use filmcraft_ui_egui::FilmcraftApp;

mod files;

use files::{FileUsage, Report};

type Outcome = Result<Report, String>;

pub(super) fn copy_picker_batch(media: &Path, copy_into: impl FnOnce(&Path) -> Result<Vec<PathBuf>, String>) -> Result<Vec<PathBuf>, String> {
    files::copy_and_publish(media, copy_into)
}

pub(super) struct MediaStorage {
    media: Option<PathBuf>,
    open: bool,
    worker: Option<Receiver<Outcome>>,
    report: Option<Outcome>,
}

impl MediaStorage {
    pub(super) fn new(media: Option<PathBuf>) -> Self {
        Self { media, open: false, worker: None, report: None }
    }

    pub(super) fn open(&mut self, ctx: &egui::Context) {
        self.open = true;
        self.start(ctx, false);
    }

    pub(super) fn show(&mut self, ctx: &egui::Context, app: &mut FilmcraftApp) {
        if !self.open {
            return;
        }
        self.poll(ctx);
        let mut open = self.open;
        egui::Window::new("Media Storage").open(&mut open).default_width(380.0).vscroll(true).show(ctx, |ui| {
            ui.label("Copies created by FilmCraft’s Android file picker.");
            if let Some(result) = &self.report {
                match result {
                    Ok(report) => {
                        ui.label(format!("Completed copies · {}", summary(&report.completed)));
                        ui.label(format!("Unfinished transfers · {}", summary(&report.unfinished)));
                        if let Some(removed) = &report.removed {
                            ui.label(format!("Cleared {}", summary(removed)));
                        }
                    }
                    Err(error) => {
                        ui.label(error);
                    }
                }
            }
            ui.label("Completed copies may be needed by saved projects or Undo. Cleanup only clears files from unfinished transfers.");
            let busy = self.worker.is_some();
            ui.horizontal_wrapped(|ui| {
                let refresh = ui.add_enabled(!busy && self.media.is_some(), egui::Button::new("Refresh"));
                app.auto.add_response("creator.mediaStorage.refresh", &refresh, "Refresh");
                if refresh.clicked() {
                    self.start(ui.ctx(), false);
                }
                let can_reclaim = !busy && self.report.as_ref().and_then(|result| result.as_ref().ok()).is_some_and(|report| report.unfinished.files > 0);
                let reclaim = ui.add_enabled(can_reclaim, egui::Button::new("Clear unfinished transfers"));
                app.auto.add_response("creator.mediaStorage.reclaim", &reclaim, "Clear unfinished transfers");
                if reclaim.clicked() {
                    self.start(ui.ctx(), true);
                }
                if busy {
                    ui.spinner();
                }
            });
        });
        self.open = open;
    }

    fn start(&mut self, ctx: &egui::Context, reclaim: bool) {
        if self.worker.is_some() {
            return;
        }
        let Some(media) = self.media.clone() else {
            self.report = Some(Err("FilmCraft’s media folder is unavailable.".into()));
            return;
        };
        let (send, receive) = mpsc::channel();
        let repaint = ctx.clone();
        let worker = std::thread::Builder::new().name("filmcraft-media-storage".into()).spawn(move || {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| files::report(&media, reclaim).map_err(|error| error.to_string())))
                .unwrap_or_else(|_| Err("Could not read FilmCraft’s media storage.".into()));
            let _ = send.send(result);
            repaint.request_repaint();
        });
        match worker {
            Ok(_) => self.worker = Some(receive),
            Err(error) => self.report = Some(Err(format!("Could not check media storage. {error}"))),
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        if let Some(worker) = &self.worker {
            match worker.try_recv() {
                Ok(report) => self.report = Some(report),
                Err(TryRecvError::Disconnected) => self.report = Some(Err("The media storage check stopped.".into())),
                Err(TryRecvError::Empty) => {
                    ctx.request_repaint_after(Duration::from_millis(100));
                    return;
                }
            }
            self.worker = None;
        }
    }
}

fn summary(usage: &FileUsage) -> String {
    let bytes = match usage.bytes {
        n if n >= 1 << 30 => format!("{:.1} GiB", n as f64 / (1_u64 << 30) as f64),
        n if n >= 1 << 20 => format!("{:.1} MiB", n as f64 / (1_u64 << 20) as f64),
        n if n >= 1 << 10 => format!("{:.1} KiB", n as f64 / (1_u64 << 10) as f64),
        n => format!("{n} B"),
    };
    let unit = if usage.files == 1 { "file" } else { "files" };
    format!("{} {unit}, {bytes}", usage.files)
}

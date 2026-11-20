//! Project Manager's collection workflow with a fresh, portable backup folder.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::Duration;

use android_activity::AndroidApp;
use filmcraft_engine::Job;
use filmcraft_platform::android_downloads::save_backup_zip;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

pub(super) struct Backup {
    root: Option<PathBuf>,
    open: bool,
    include_proxies: bool,
    job: Option<(Job, PathBuf)>,
    completed: Option<PathBuf>,
    publishing: Option<Receiver<Result<(), String>>>,
    outcome: Option<String>,
}

impl Backup {
    pub fn new(directory: Option<PathBuf>) -> Self {
        Self {
            root: directory.map(|path| path.join("Project backups")),
            open: false,
            include_proxies: false,
            job: None,
            completed: None,
            publishing: None,
            outcome: None,
        }
    }

    pub fn open(&mut self) {
        self.open = true;
    }

    pub fn show(&mut self, ctx: &egui::Context, app: &mut FilmcraftApp, android: &AndroidApp) {
        if let Some((job, folder)) = &self.job {
            let result = job.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
            if let Some(result) = result {
                self.outcome = Some(match result {
                    Ok(report) => {
                        self.completed = Path::new(&report.path).parent().map(Path::to_path_buf);
                        format!("Backup saved at {}.", report.path)
                    }
                    Err(error) => {
                        if let Err(cleanup) = fs::remove_dir_all(folder) {
                            log::warn!("Could not clear the unfinished backup folder: {cleanup}");
                        }
                        if error == "cancelled" { "Backup cancelled.".into() } else { format!("Backup did not finish. {error}") }
                    }
                });
                self.job = None;
            }
        }
        if let Some(receiver) = &self.publishing {
            match receiver.try_recv() {
                Ok(result) => {
                    self.outcome = Some(match result {
                        Ok(()) => "ZIP saved in Downloads/FilmCraft.".into(),
                        Err(error) => format!("Could not save the backup ZIP. {error}"),
                    });
                    self.publishing = None;
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.outcome = Some("Saving the backup ZIP stopped.".into());
                    self.publishing = None;
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if !self.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Project backup").open(&mut open).default_width(415.0).vscroll(true).show(ctx, |ui| {
            ui.label("Copy the current project and its source media into a portable folder.");
            if let Some(root) = &self.root {
                ui.label(root.display().to_string());
            }
            if let Some((job, _)) = &self.job {
                ui.add(egui::ProgressBar::new(job.progress.fraction()).show_percentage());
                if ui.add_enabled(!job.progress.cancel.load(Ordering::Relaxed), egui::Button::new("Cancel backup")).clicked() {
                    job.progress.cancel.store(true, Ordering::Relaxed);
                }
                ctx.request_repaint_after(Duration::from_millis(250));
            } else if self.publishing.is_some() {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label("Saving the backup ZIP to Downloads");
                });
            } else {
                ui.checkbox(&mut self.include_proxies, "Include proxies");
                let available = self.root.is_some() && app.session.project.sequences().next().is_some();
                if ui.add_enabled(available, egui::Button::new("Create backup")).clicked() {
                    self.outcome = None;
                    match self.start(app) {
                        Ok(job) => {
                            self.job = Some(job);
                            self.completed = None;
                        }
                        Err(error) => self.outcome = Some(error),
                    }
                }
            }
            if let Some(outcome) = &self.outcome {
                ui.label(outcome);
            }
            if self.publishing.is_none()
                && let Some(folder) = &self.completed
                && ui.button("Save ZIP to Downloads").clicked()
            {
                let (reply, result) = mpsc::channel();
                let (android, folder, ctx) = (android.clone(), folder.clone(), ctx.clone());
                match thread::Builder::new().name("filmcraft-backup-zip".into()).spawn(move || {
                    let published = save_backup_zip(&android, &folder).map(|_| ()).map_err(|error| error.to_string());
                    let _ = reply.send(published);
                    ctx.request_repaint();
                }) {
                    Ok(_) => self.publishing = Some(result),
                    Err(error) => self.outcome = Some(format!("Could not start saving the backup ZIP. {error}")),
                }
            }
        });
        self.open = open;
    }

    fn start(&self, app: &mut FilmcraftApp) -> Result<(Job, PathBuf), String> {
        let root = self.root.as_deref().ok_or("FilmCraft has no export folder.")?;
        let name = filmcraft_engine::export_tools::file_safe(&app.session.project.name.chars().take(72).collect::<String>());
        let folder = reserve_folder(root, &name).map_err(|error| format!("Could not create the backup folder. {error}"))?;
        let params = json!({
            "destination": folder, "projectName": name, "mode": "collect", "portable": true,
            "excludeUnused": false, "includeProxies": self.include_proxies, "wait": false,
        });
        let result = match app.session.execute("file.projectManager", params) {
            Ok(result) => result,
            Err(error) => {
                let _ = fs::remove_dir(&folder);
                return Err(error.to_string());
            }
        };
        result["job"]
            .as_u64()
            .and_then(|id| app.session.jobs.iter().find(|job| job.id == id))
            .cloned()
            .map(|job| (job, folder))
            .ok_or("The backup job did not start.".into())
    }
}

fn reserve_folder(root: &Path, name: &str) -> io::Result<PathBuf> {
    fs::create_dir_all(root)?;
    for number in 1..=1024 {
        let folder = root.join(format!("{name} {number}"));
        match fs::create_dir(&folder) {
            Ok(()) => return Ok(folder),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("Could not find an unused backup folder name."))
}

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::Ordering;
use std::sync::mpsc::{self, Receiver};
use std::time::Duration;

use android_activity::AndroidApp;
use filmcraft_engine::{Job, Session};
use filmcraft_platform::android_downloads;
use filmcraft_project::{ClipId, ItemId, Sequence};
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::menus::invoke_command;
use filmcraft_ui_egui::state::PlaybackRes;
use serde_json::{Value, json};

use crate::performance::ExportMeasurement;

pub struct Creator {
    android: AndroidApp,
    output: Option<PathBuf>,
    delivery: Delivery,
    preview: AutoPreview,
    next_export: u32,
    try_hardware_encoding: bool,
    preset: &'static str,
    captions: &'static str,
    range: ExportRange,
    last_export: Option<ExportMeasurement>,
}

enum Delivery {
    Idle,
    Rendering { job: Job, path: PathBuf, plan: ExportPlan },
    Publishing { bundle: VideoBundle, result: Receiver<(VideoBundle, Result<(), String>)> },
    Saved { uri: String, subtitles: bool },
    Cancelled,
    RenderFailed { error: String, retry: Option<ExportPlan> },
    PublishFailed { bundle: VideoBundle, error: String },
}

#[derive(Clone)]
struct VideoBundle {
    path: PathBuf,
    subtitle: Option<PathBuf>,
    video_uri: Option<String>,
}

const PRESETS: [(&str, &str); 4] = [
    ("Match Source", filmcraft_engine::export::presets::DEFAULT_PRESET),
    ("1080p", "High Quality 1080p HD"),
    ("4K", "High Quality 2160p 4K"),
    ("Vertical 9:16", "Social Vertical 1080×1920"),
];
const CAPTIONS: [(&str, &str); 4] = [("None", "none"), ("Burn into video", "burn"), ("Separate SRT file", "srt"), ("Separate WebVTT file", "vtt")];

#[derive(Clone)]
struct ExportPlan {
    document: u64,
    revision: u64,
    sequence: ItemId,
    params: Value,
    size: (u32, u32),
    fps: f64,
    warnings: Vec<&'static str>,
    estimated_size: String,
}

impl ExportPlan {
    fn current(&self, session: &Session) -> bool {
        self.document == session.document_epoch && self.revision == session.revision && Some(self.sequence) == session.state.active_sequence
    }
}

struct AutoPreview {
    enabled: bool,
    baseline: (u64, u64),
    good: u8,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ExportRange {
    Entire,
    InOut,
}

impl ExportRange {
    fn param(self) -> &'static str {
        match self {
            Self::Entire => "entire",
            Self::InOut => "inOut",
        }
    }

    // The engine includes the Out frame, so marks on the same frame are valid.
    fn validate(self, sequence: &Sequence) -> Result<(), &'static str> {
        if let Self::InOut = self {
            let (Some(mark_in), Some(mark_out)) = (sequence.mark_in, sequence.mark_out) else {
                return Err("Set both In and Out points in the timeline.");
            };
            if mark_in < Tick::ZERO || mark_out < mark_in || mark_out >= sequence.duration() {
                return Err("Set the In and Out points in order within the sequence.");
            }
        }
        Ok(())
    }
}

/// A concrete video clip for quick edits, with no fallback to the playhead.
pub(super) fn selected_video_clip(session: &Session) -> Option<ClipId> {
    let sequence = session.active_sequence()?;
    session.state.selection.iter().copied().find(|&clip| sequence.video_tracks.iter().any(|track| track.item(clip).is_some()))
}

impl AutoPreview {
    fn update(&mut self, app: &mut FilmcraftApp) {
        if !self.enabled {
            return;
        }
        let current = (app.playback.meter.shown, app.playback.meter.dropped);
        if !app.playback.playing || current.0 < self.baseline.0 || current.1 < self.baseline.1 {
            self.baseline = current;
            self.good = 0;
            return;
        }
        let shown = current.0 - self.baseline.0;
        let dropped = current.1 - self.baseline.1;
        let total = shown.saturating_add(dropped);
        if total < 45 {
            return;
        }
        self.baseline = current;
        if dropped.saturating_mul(10) >= total {
            app.ui.program.res = match app.ui.program.res {
                PlaybackRes::Full | PlaybackRes::Half => PlaybackRes::Quarter,
                _ => PlaybackRes::Eighth,
            };
            self.good = 0;
        } else if dropped.saturating_mul(50) < total {
            self.good = self.good.saturating_add(1);
            if self.good >= 3 {
                app.ui.program.res = if app.ui.program.res == PlaybackRes::Eighth { PlaybackRes::Quarter } else { PlaybackRes::Half };
                self.good = 0;
            }
        } else {
            self.good = 0;
        }
    }
}

impl Creator {
    pub fn new(android: AndroidApp, output: Option<PathBuf>) -> Self {
        Self {
            android,
            output,
            delivery: Delivery::Idle,
            preview: AutoPreview { enabled: true, baseline: (0, 0), good: 0 },
            next_export: 1,
            try_hardware_encoding: false,
            preset: PRESETS[0].1,
            captions: CAPTIONS[0].1,
            range: ExportRange::Entire,
            last_export: None,
        }
    }

    pub fn toolbar(&mut self, ui: &mut egui::Ui, app: &mut FilmcraftApp) {
        let invoke = |app: &mut FilmcraftApp, ctx: &egui::Context, command| {
            if let Err(error) = invoke_command(app, ctx, command, json!({})) {
                app.status(error);
            }
        };
        if ui.button("Import").clicked() {
            invoke(app, ui.ctx(), "file.import");
        }
        if ui.button("Captions").clicked() {
            invoke(app, ui.ctx(), "window.workspace.captionsandgraphics");
        }
        ui.menu_button("Preview quality", |ui| {
            if ui.selectable_label(self.preview.enabled, format!("Auto · {}", app.ui.program.res.label())).clicked() {
                self.preview.enabled = true;
                self.preview.baseline = (app.playback.meter.shown, app.playback.meter.dropped);
                self.preview.good = 0;
                app.ui.program.res = PlaybackRes::Half;
                app.ui.program.high_quality = false;
                ui.close();
            }
            for resolution in [PlaybackRes::Full, PlaybackRes::Half, PlaybackRes::Quarter, PlaybackRes::Eighth] {
                if ui.selectable_label(!self.preview.enabled && app.ui.program.res == resolution, resolution.label()).clicked() {
                    self.preview.enabled = false;
                    app.ui.program.res = resolution;
                    ui.close();
                }
            }
            ui.separator();
            let mut proxies = app.session.media.use_proxies();
            if ui.checkbox(&mut proxies, "Use proxies").changed() {
                run(app, "media.toggleProxies", json!({"enabled": proxies}));
            }
            if ui.add_enabled(!app.session.state.project_selection.is_empty(), egui::Button::new("Create proxies for selected media")).clicked() {
                invoke(app, ui.ctx(), "media.createProxies");
                ui.close();
            }
            let demanding = demanding_clips(&app.session);
            if ui.add_enabled(!demanding.is_empty() && self.output.is_some(), egui::Button::new("Create lightweight proxies for 4K clips")).clicked()
                && let Some(root) = &self.output
            {
                match app.session.execute(
                    "media.createProxies",
                    json!({
                        "items": demanding, "preset": "h264_quarter", "destination": root.join("Proxies")
                    }),
                ) {
                    Ok(_) => {
                        run(app, "media.toggleProxies", json!({"enabled": true}));
                        app.status("Creating quarter-size H.264 proxies. Follow the job in Export progress.".to_string());
                    }
                    Err(error) => app.status(error.to_string()),
                }
                ui.close();
            }
        });
        ui.menu_button("Workspace", |ui| {
            for (label, command) in [("Creator", "window.workspace.editing"), ("Pro", "window.workspace.allpanels"), ("Vertical", "window.workspace.vertical")]
            {
                if ui.button(label).clicked() {
                    invoke(app, ui.ctx(), command);
                    ui.close();
                }
            }
        });
        let busy = matches!(self.delivery, Delivery::Rendering { .. } | Delivery::Publishing { .. });
        let enabled = !busy && self.output.is_some() && app.session.active_sequence().is_some();
        ui.add_enabled_ui(enabled, |ui| {
            let menu = ui.menu_button("Export", |ui| {
                choice(ui, "Preset", &mut self.preset, &PRESETS);
                choice(ui, "Captions", &mut self.captions, &CAPTIONS);
                if self.preset == PRESETS[3].1 {
                    ui.small("Vertical fills the 9:16 frame and crops wider footage.");
                }
                if matches!(self.captions, "srt" | "vtt") && app.session.active_sequence().is_some_and(|sequence| sequence.caption_tracks.len() > 1) {
                    ui.small("The subtitle file uses the first caption track.");
                }
                ui.checkbox(&mut self.try_hardware_encoding, "Try hardware encoding");
                for (label, range) in [("Entire sequence", ExportRange::Entire), ("In/Out range", ExportRange::InOut)] {
                    ui.radio_value(&mut self.range, range, label);
                }
                match self.export_plan(&app.session) {
                    Ok(plan) => {
                        ui.label(format!("{} × {} · Estimated {}", plan.size.0, plan.size.1, plan.estimated_size));
                        let button = ui.button("Export MP4 to Downloads");
                        app.auto.add_response(&format!("creator.quickExport.{}", self.range.param()), &button, "Export MP4 to Downloads");
                        if button.clicked() {
                            self.delivery = self.start_export(app, plan).unwrap_or_else(render_failure);
                            ui.close();
                        }
                    }
                    Err(error) => {
                        ui.label(error);
                    }
                }
            });
            app.auto.add_response("creator.quickExport", &menu.response, "Export");
        });
        match &self.delivery {
            Delivery::Rendering { job, .. } => {
                ui.add(egui::ProgressBar::new(job.progress.fraction()).desired_width(92.0).show_percentage());
                if job.progress.cancel.load(Ordering::Relaxed) {
                    ui.label("Cancelling…");
                } else {
                    let button = ui.add_enabled(!job.progress.finished.load(Ordering::Relaxed), egui::Button::new("Cancel export"));
                    app.auto.add_response("creator.quickExport.cancel", &button, "Cancel export");
                    if button.clicked() {
                        run(app, "jobs.cancel", json!({"job": job.id}));
                    }
                }
            }
            Delivery::Publishing { .. } => {
                ui.label("Saving to Downloads…");
            }
            Delivery::Saved { uri, subtitles } => {
                ui.label(if *subtitles { "Video and subtitles saved to Downloads" } else { "Video saved to Downloads" });
                if ui.button("Share").clicked()
                    && let Err(error) = android_downloads::share_mp4(&self.android, uri)
                {
                    app.status(error.to_string());
                }
            }
            Delivery::Cancelled => {
                ui.label("Export cancelled");
            }
            Delivery::RenderFailed { error, retry } => {
                ui.label(error);
                if let Some(plan) = retry {
                    let current = plan.current(&app.session);
                    if ui
                        .add_enabled(current, egui::Button::new("Retry in software"))
                        .on_disabled_hover_text("The timeline changed. Start a new export.")
                        .clicked()
                    {
                        let mut plan = plan.clone();
                        plan.params["hardwareEncoding"] = json!("off");
                        self.delivery = self.start_export(app, plan).unwrap_or_else(render_failure);
                    }
                }
            }
            Delivery::PublishFailed { bundle, error } => {
                ui.label(format!("Export kept at {}. {error}", bundle.path.display()));
                if ui.add_enabled(bundle.path.is_file(), egui::Button::new("Retry saving to Downloads")).clicked() {
                    self.delivery = publish(&self.android, bundle.clone(), ui.ctx());
                }
            }
            Delivery::Idle => {}
        }
    }

    fn export_plan(&self, session: &Session) -> Result<ExportPlan, String> {
        let sequence = session.active_sequence().ok_or("Open a sequence before exporting.")?;
        self.range.validate(sequence)?;
        if self.captions != "none" && sequence.caption_tracks.is_empty() {
            return Err("Add captions or choose None to export without captions.".into());
        }
        let id = session.state.active_sequence.ok_or("Open a sequence before exporting.")?;
        let mode = json!(self.range.param());
        let (resolved, warnings) = filmcraft_engine::export_tools::range_param(session, &session.project, id, Some(&mode), &json!({}), "file.exportMedia")
            .map_err(|error| error.to_string())?;
        let range = resolved.ok_or("Could not resolve the export range.")?;
        let encoding = if self.try_hardware_encoding { "auto" } else { "off" };
        let mut params = json!({
            "preset": self.preset, "sequence": id.0,
            "range": {"mode": "custom", "startTime": range.start.0, "endTime": range.end().0},
            "hardwareEncoding": encoding, "wait": false, "burnCaptions": self.captions == "burn",
        });
        if matches!(self.captions, "srt" | "vtt") {
            params["captionSidecar"] = json!(self.captions);
        }
        let (_, settings) = filmcraft_engine::export_tools::settings_from_params(session, &params, "file.exportMedia").map_err(|error| error.to_string())?;
        let (width, height, rate, sr) = (sequence.settings.width, sequence.settings.height, sequence.settings.frame_rate, sequence.settings.sample_rate);
        let output = settings.resolve(width, height, rate, sr);
        Ok(ExportPlan {
            document: session.document_epoch,
            revision: session.revision,
            sequence: id,
            params,
            size: (output.width, output.height),
            fps: output.rate.as_f64(),
            warnings,
            estimated_size: settings.summary(width, height, rate, sr, range.duration).estimated_size,
        })
    }

    fn start_export(&mut self, app: &mut FilmcraftApp, plan: ExportPlan) -> Result<Delivery, String> {
        let root = self.output.as_ref().ok_or("FilmCraft has no export folder.")?;
        let name = app.session.project.item(plan.sequence).map(|i| i.name.as_str()).unwrap_or("FilmCraft");
        let base = filmcraft_engine::export_tools::file_safe(&name.chars().take(80).collect::<String>());
        let path = reserve_export(root, &mut self.next_export, &base).map_err(|err| format!("Could not prepare export storage. {err}"))?;
        let mut params = plan.params.clone();
        params["path"] = json!(path);
        let event = match app.session.execute("file.exportMedia", params) {
            Ok(result) => result,
            Err(error) => {
                clean_subtitle(&path, &plan);
                remove_empty_staging_dir(&path);
                return Err(error.to_string());
            }
        };
        let job = event["job"].as_u64().and_then(|id| app.session.jobs.iter().find(|job| job.id == id)).cloned();
        let job = job.ok_or_else(|| {
            clean_subtitle(&path, &plan);
            remove_empty_staging_dir(&path);
            "The export job did not start."
        })?;
        Ok(Delivery::Rendering { job, path, plan })
    }

    pub(super) fn last_export(&self) -> Option<&ExportMeasurement> {
        self.last_export.as_ref()
    }

    pub fn poll(&mut self, ctx: &egui::Context, app: &mut FilmcraftApp) {
        self.preview.update(app);
        match &self.delivery {
            Delivery::Rendering { job, path, plan } => {
                let result = job.result.lock().unwrap_or_else(|poisoned| poisoned.into_inner()).clone();
                match result {
                    None => ctx.request_repaint_after(Duration::from_millis(250)),
                    Some(Ok(report)) => {
                        self.last_export = Some(ExportMeasurement {
                            size: plan.size,
                            frames: report.frames,
                            seconds: report.seconds,
                            encoder: report.encoder.unwrap_or_else(|| "Encoder unavailable".into()),
                            realtime: report.render_fps / plan.fps.max(0.01),
                            warnings: plan.warnings.iter().map(|warning| (*warning).to_owned()).chain(report.warnings).collect(),
                        });
                        let subtitle = plan.params["captionSidecar"].as_str().map(|ext| path.with_extension(ext)).filter(|path| path.is_file());
                        self.delivery = publish(&self.android, VideoBundle { path: path.clone(), subtitle, video_uri: None }, ctx);
                    }
                    Some(Err(error)) if error == "cancelled" => {
                        clean_subtitle(path, plan);
                        clean_staging(path);
                        self.delivery = Delivery::Cancelled;
                    }
                    Some(Err(error)) => {
                        clean_subtitle(path, plan);
                        remove_empty_staging_dir(path);
                        let retry = (plan.params["hardwareEncoding"] == "auto" && error.contains("Android MediaCodec")).then(|| plan.clone());
                        self.delivery = Delivery::RenderFailed { error, retry };
                    }
                }
            }
            Delivery::Publishing { bundle, result } => {
                self.delivery = match result.try_recv() {
                    Ok((bundle, Ok(()))) => Delivery::Saved { uri: bundle.video_uri.unwrap_or_default(), subtitles: bundle.subtitle.is_some() },
                    Ok((bundle, Err(error))) => Delivery::PublishFailed { bundle, error },
                    Err(mpsc::TryRecvError::Disconnected) => {
                        Delivery::PublishFailed { bundle: bundle.clone(), error: "The Downloads transfer stopped.".into() }
                    }
                    Err(mpsc::TryRecvError::Empty) => return,
                };
            }
            _ => {}
        }
    }
}

fn render_failure(error: String) -> Delivery {
    Delivery::RenderFailed { error, retry: None }
}

fn run(app: &mut FilmcraftApp, command: &str, params: serde_json::Value) {
    if let Err(error) = app.session.execute(command, params) {
        app.status(error.to_string());
    }
}

fn choice(ui: &mut egui::Ui, label: &str, value: &mut &'static str, options: &[(&'static str, &'static str)]) {
    let selected = options.iter().find(|(_, key)| key == value).map(|(name, _)| *name).unwrap_or_default();
    egui::ComboBox::from_id_salt(label).selected_text(selected).show_ui(ui, |ui| {
        for &(name, key) in options {
            ui.selectable_value(value, key, name);
        }
    });
}

fn demanding_clips(session: &Session) -> Vec<u64> {
    let Some(sequence) = session.state.active_sequence else { return Vec::new() };
    let (_, used) = filmcraft_engine::project_manager::used_ranges(&session.project, &[sequence]);
    used.into_iter()
        .filter_map(|id| {
            let media = session.project.item(id)?.as_media()?;
            let video = media.info.video.as_ref()?;
            (matches!(media.media, filmcraft_project::MediaRef::File { .. })
                && media.proxy.is_none()
                && video.width.max(video.height) >= 3840
                && media.info.kind == filmcraft_media::MediaKind::Movie)
                .then_some(id.0)
        })
        .collect()
}

/// Reserve a separate directory for each quick export. Cleanup never touches the general
/// Exports directory or any user-supplied filename.
fn reserve_export(root: &Path, next: &mut u32, name: &str) -> io::Result<PathBuf> {
    let staging = root.join("Quick exports");
    std::fs::create_dir_all(&staging)?;
    for _ in 0..1024 {
        let number = *next;
        *next = number.saturating_add(1);
        let dir = staging.join(number.to_string());
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                let random = getrandom::u32().map_err(|error| io::Error::other(error.to_string()));
                return match random {
                    Ok(random) => Ok(dir.join(format!("{name}-{random:08x}.mp4"))),
                    Err(error) => {
                        let _ = std::fs::remove_dir(dir);
                        Err(error)
                    }
                };
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
    Err(io::Error::other("No unused quick export folder was found."))
}

fn remove_empty_staging_dir(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = std::fs::remove_dir(parent);
    }
}

fn clean_staging(path: &Path) {
    if let Err(err) = std::fs::remove_file(path)
        && err.kind() != io::ErrorKind::NotFound
    {
        log::warn!("Could not clean up the quick export staging file: {err}");
        return;
    }
    remove_empty_staging_dir(path);
}

fn clean_subtitle(path: &Path, plan: &ExportPlan) {
    if let Some(ext) = plan.params["captionSidecar"].as_str() {
        let _ = std::fs::remove_file(path.with_extension(ext));
    }
}

fn publish(app: &AndroidApp, bundle: VideoBundle, ctx: &egui::Context) -> Delivery {
    let (reply, result) = mpsc::sync_channel(1);
    let app = app.clone();
    let ctx = ctx.clone();
    let mut work = bundle.clone();
    match std::thread::Builder::new().name("filmcraft-download".into()).spawn(move || {
        let saved = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| -> Result<(), String> {
            if work.video_uri.is_none() {
                work.video_uri = Some(android_downloads::save_mp4(&app, &work.path).map_err(|error| error.to_string())?);
            }
            if let Some(subtitle) = &work.subtitle {
                android_downloads::save_subtitles(&app, subtitle).map_err(|error| format!("The video was saved. Subtitles could not be saved. {error}"))?;
            }
            Ok(())
        }))
        .unwrap_or_else(|_| Err("Could not save the export to Downloads.".into()));
        if saved.is_ok() {
            if let Some(subtitle) = &work.subtitle {
                let _ = std::fs::remove_file(subtitle);
            }
            clean_staging(&work.path);
        }
        let _ = reply.send((work, saved));
        ctx.request_repaint();
    }) {
        Ok(_) => Delivery::Publishing { bundle, result },
        Err(error) => Delivery::PublishFailed { bundle, error: error.to_string() },
    }
}

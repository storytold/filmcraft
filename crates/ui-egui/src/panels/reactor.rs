//! Dockable FastH3 Omni workspace. Credentials and HTTP remain with the desktop host.
mod reference_media;

use crate::{
    FilmcraftApp,
    dock::PanelKind,
    icons::{self, Icon},
    native_dialogs::FileDialog,
};
use egui::{Context, Rect, Sense, pos2, vec2};
use filmcraft_project::BinId;
use filmcraft_time::Tick;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::sync::mpsc::{Receiver, TryRecvError};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default)]
pub struct Draft {
    pub prompt: String,
    pub seconds: f64,
    pub aspect: String,
    pub reference_images: Vec<String>,
    pub reference_audios: Vec<String>,
    pub message: String,
    pub grid: bool,
}
impl Default for Draft {
    fn default() -> Self {
        Self {
            prompt: String::new(),
            seconds: 8.0,
            aspect: "16:9".into(),
            reference_images: vec![],
            reference_audios: vec![],
            message: String::new(),
            grid: true,
        }
    }
}
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Request {
    pub prompt: String,
    pub seconds: f64,
    pub aspect: String,
    #[serde(default)]
    pub reference_images: Vec<String>,
    #[serde(default)]
    pub reference_audios: Vec<String>,
    /// Empty or omitted uses the active project's Reactor media folder.
    #[serde(default)]
    pub output_path: String,
}
fn media_folder(project_path: Option<&str>) -> Result<std::path::PathBuf, String> {
    let project = std::path::Path::new(project_path.ok_or("Save the project before generating a Reactor clip.")?);
    if !project.is_absolute() {
        return Err("Save the project to an absolute path before generating a Reactor clip.".into());
    }
    let parent = project.parent().ok_or("Project folder is unavailable.")?;
    let stem = project.file_stem().filter(|name| !name.is_empty()).ok_or("Project file has no name.")?;
    let mut folder = stem.to_os_string();
    folder.push(" Media");
    Ok(parent.join(folder).join("Reactor"))
}

fn project_output(project_path: Option<&str>) -> Result<String, String> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let folder = media_folder(project_path)?;
    let time = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_err(|error| format!("Could not name Reactor clip: {error}"))?;
    let name = format!("FastH3 Omni-{}-{}.mp4", time.as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed));
    folder.join(name).to_str().map(str::to_owned).ok_or_else(|| "Project media folder is not a valid UTF-8 path.".into())
}

pub enum Event {
    Stage(String),
    Finished(Result<String, String>),
}
pub(crate) struct Running {
    pub receiver: Receiver<Event>,
    id: u64,
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Status {
    Queued,
    Running,
    Imported,
    Saved,
    Failed,
}
#[derive(Clone)]
struct Job {
    id: u64,
    request: Request,
    project_name: String,
    project_path: Option<String>,
    bin: BinId,
    status: Status,
    message: String,
}
impl Job {
    fn current(&self, app: &FilmcraftApp) -> bool {
        self.project_name == app.session.project.name && self.project_path == app.session.path
    }
}
#[derive(Default)]
pub(crate) struct Runtime {
    jobs: Vec<Job>,
    next: u64,
    media: reference_media::Media,
    capture: Option<Receiver<Result<String, String>>>,
    capture_project: Option<String>,
}

pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    let result = match id {
        "reactor.generate" => {
            if !params.is_object() {
                return Some(Err("Reactor parameters must be an object.".into()));
            }
            if params.as_object().is_some_and(|p| p.is_empty()) {
                app.show_panel(PanelKind::Reactor);
                Ok(json!({"panel":"Reactor"}))
            } else {
                serde_json::from_value::<Request>(params.clone())
                    .map_err(|e| format!("Invalid Reactor generation request: {e}"))
                    .and_then(|request| start(app, request).map(|()| json!({"started":true})))
            }
        }
        "reactor.view" => match params["view"].as_str() {
            Some("grid" | "list") => {
                app.ui.reactor.grid = params["view"] == "grid";
                Ok(json!({"view":params["view"]}))
            }
            _ => Err("Choose grid or list view.".into()),
        },
        "reactor.references" => references(app, params),
        "reactor.cancelQueued" => {
            let index =
                app.reactor_runtime.jobs.iter().position(|job| job.status == Status::Queued && job.current(app) && params["id"].as_u64() == Some(job.id));
            match index {
                Some(index) => {
                    let job = app.reactor_runtime.jobs.remove(index);
                    Ok(json!({"cancelled":job.id}))
                }
                None => Err("Only a queued generation in this project can be cancelled.".into()),
            }
        }
        "reactor.reuse" => reuse(app, params),
        "reactor.captureFrame" => capture(app).map(|()| json!({"capturing":true})),
        "reactor.audio" => audio_action(app, params),
        "reactor.jobs" => Ok(json!(
            app.reactor_runtime
                .jobs
                .iter()
                .filter(|job| job.current(app))
                .map(|job| json!({
                    "id":job.id,"prompt":job.request.prompt,"output_path":job.request.output_path,
                    "status":format!("{:?}",job.status).to_ascii_lowercase(),"message":job.message
                }))
                .collect::<Vec<_>>()
        )),
        _ => return None,
    };
    Some(result)
}

fn references(app: &mut FilmcraftApp, params: &Value) -> Result<Value, String> {
    let image = match params["kind"].as_str() {
        Some("image") => true,
        Some("audio") => false,
        _ => return Err("Choose image or audio references.".into()),
    };
    let paths = if image { &mut app.ui.reactor.reference_images } else { &mut app.ui.reactor.reference_audios };
    let mut removed = None;
    if let Some(index) = params.get("remove") {
        let index = index.as_u64().and_then(|n| usize::try_from(n).ok()).filter(|&n| n < paths.len()).ok_or("Invalid reference index")?;
        removed = Some(paths.remove(index));
    } else {
        let added: Vec<String> = serde_json::from_value(params["paths"].clone()).map_err(|e| format!("Invalid reference paths: {e}"))?;
        let limit = if image { 9 } else { 3 };
        if paths.len().saturating_add(added.len()) > limit {
            return Err(format!("Use at most {limit} references of this type."));
        }
        if added.iter().any(|p| p.len() > 4096 || !std::path::Path::new(p).is_absolute()) {
            return Err("Use absolute reference file paths.".into());
        }
        for path in added {
            if !paths.contains(&path) {
                paths.push(path);
            }
        }
    }
    let count = paths.len();
    if !image && removed.as_ref().is_some_and(|path| app.reactor_runtime.media.playback.as_ref().is_some_and(|p| &p.path == path)) {
        let mut media = std::mem::take(&mut app.reactor_runtime.media);
        media.stop(app);
        app.reactor_runtime.media = media;
    }
    Ok(json!({"count":count}))
}
fn reuse(app: &mut FilmcraftApp, params: &Value) -> Result<Value, String> {
    let id = params["id"].as_u64().ok_or("A generation ID is required")?;
    let request = app
        .reactor_runtime
        .jobs
        .iter()
        .find(|job| job.id == id && job.current(app))
        .map(|j| j.request.clone())
        .ok_or("Generation is unavailable in this project")?;
    let mut media = std::mem::take(&mut app.reactor_runtime.media);
    media.stop(app);
    app.reactor_runtime.media = media;
    app.ui.reactor.prompt = request.prompt;
    app.ui.reactor.seconds = request.seconds;
    app.ui.reactor.aspect = request.aspect;
    app.ui.reactor.reference_images = request.reference_images;
    app.ui.reactor.reference_audios = request.reference_audios;
    app.ui.reactor.message.clear();
    Ok(json!({"reused":id}))
}
fn validate(request: &Request) -> Result<(), String> {
    if request.prompt.trim().is_empty() || request.prompt.len() > 2048 {
        return Err("Enter a prompt of at most 2048 UTF-8 bytes.".into());
    }
    if ![5.875, 8.0, 10.125, 12.25, 14.375].contains(&request.seconds) {
        return Err("Choose a supported FastH3 Omni duration.".into());
    }
    if !["16:9", "9:16", "4:3", "1:1"].contains(&request.aspect.as_str()) {
        return Err("Choose a supported aspect ratio.".into());
    }
    if request.reference_images.len() > 9 || request.reference_audios.len() > 3 || (!request.reference_audios.is_empty() && request.reference_images.is_empty())
    {
        return Err("Use up to 9 images and 3 audio clips; audio requires an image.".into());
    }
    Ok(())
}
fn start(app: &mut FilmcraftApp, mut request: Request) -> Result<(), String> {
    validate(&request)?;
    if app.hooks.reactor_generate.is_none() {
        return Err("Reactor generation is available in the desktop app.".into());
    }
    if app.reactor_runtime.jobs.iter().filter(|j| matches!(j.status, Status::Queued | Status::Running)).count() >= 8 {
        return Err("The generation queue is full. Wait for a clip to finish.".into());
    }
    if request.output_path.is_empty() {
        request.output_path = project_output(app.session.path.as_deref())?;
    }
    if !std::path::Path::new(&request.output_path).is_absolute() || !request.output_path.to_ascii_lowercase().ends_with(".mp4") {
        return Err("Choose an absolute MP4 output path.".into());
    }
    if std::path::Path::new(&request.output_path).exists() || app.reactor_runtime.jobs.iter().any(|j| j.request.output_path == request.output_path) {
        return Err("Output file already exists or is reserved by another generation.".into());
    }
    if app.reactor_runtime.jobs.len() >= 64
        && let Some(index) = app.reactor_runtime.jobs.iter().position(|j| !matches!(j.status, Status::Running | Status::Queued))
    {
        app.reactor_runtime.jobs.remove(index);
    }
    app.reactor_runtime.next = app.reactor_runtime.next.checked_add(1).ok_or("Generation counter exhausted")?;
    app.reactor_runtime.jobs.push(Job {
        id: app.reactor_runtime.next,
        request,
        project_name: app.session.project.name.clone(),
        project_path: app.session.path.clone(),
        bin: app.import_bin(),
        status: Status::Queued,
        message: "Queued".into(),
    });
    app.ui.reactor.message.clear();
    app.show_panel(PanelKind::Reactor);
    start_next(app);
    Ok(())
}
fn start_next(app: &mut FilmcraftApp) {
    if app.reactor_running.is_some() {
        return;
    }
    let next = app.reactor_runtime.jobs.iter().position(|job| job.status == Status::Queued && job.current(app));
    if let Some(index) = next {
        let Some(job) = app.reactor_runtime.jobs.get_mut(index) else { return };
        let Some(generate) = app.hooks.reactor_generate.as_mut() else { return };
        job.status = Status::Running;
        job.message = "Starting generation…".into();
        app.reactor_running = Some(Running { receiver: generate(job.request.clone()), id: job.id });
    }
}
fn finish(app: &mut FilmcraftApp, id: u64, result: Result<String, String>) {
    let Some(index) = app.reactor_runtime.jobs.iter().position(|job| job.id == id) else { return };
    let Some(job) = app.reactor_runtime.jobs.get(index).cloned() else { return };
    let (status, message) = match result {
        Err(error) => (Status::Failed, error),
        Ok(path) if !job.current(app) => (Status::Saved, format!("Clip saved to {path}. Import it when you return to this project.")),
        Ok(path) => match app.session.execute("file.import", json!({"paths":[path],"bin":job.bin.0})) {
            Ok(value) if value["items"].as_array().is_some_and(|a| !a.is_empty()) => (Status::Imported, "In project".into()),
            Ok(value) => (Status::Saved, format!("Clip saved, but import failed: {}", value["errors"])),
            Err(error) => (Status::Saved, format!("Clip saved, but import failed: {error}")),
        },
    };
    if let Some(job) = app.reactor_runtime.jobs.get_mut(index) {
        job.status = status;
        job.message = message.clone();
    }
    app.ui.status = message;
}
pub fn poll(app: &mut FilmcraftApp, ctx: &Context) {
    for _ in 0..32 {
        let next = app.reactor_running.as_ref().map(|job| (job.id, job.receiver.try_recv()));
        match next {
            Some((id, Ok(Event::Stage(message)))) => {
                if let Some(job) = app.reactor_runtime.jobs.iter_mut().find(|j| j.id == id) {
                    job.message = message;
                }
            }
            Some((id, Ok(Event::Finished(result)))) => {
                app.reactor_running = None;
                finish(app, id, result);
            }
            Some((id, Err(TryRecvError::Disconnected))) => {
                app.reactor_running = None;
                finish(app, id, Err("Reactor worker stopped unexpectedly.".into()));
            }
            _ => break,
        }
    }
    start_next(app);
    let capture = app.reactor_runtime.capture.as_ref().map(Receiver::try_recv);
    match capture {
        Some(Ok(result)) => {
            app.reactor_runtime.capture = None;
            match result {
                Ok(path) if app.reactor_runtime.capture_project == app.session.path && app.ui.reactor.reference_images.len() < 9 => {
                    app.ui.reactor.reference_images.push(path)
                }
                Ok(path) => app.ui.status = format!("Reference frame saved to {path}"),
                Err(error) => app.ui.reactor.message = error,
            }
        }
        Some(Err(TryRecvError::Disconnected)) => {
            app.reactor_runtime.capture = None;
            app.ui.reactor.message = "Frame capture worker stopped unexpectedly.".into();
        }
        _ => {}
    }
    let mut media = std::mem::take(&mut app.reactor_runtime.media);
    media.poll(ctx);
    media.advance(app, ctx);
    app.reactor_runtime.media = media;
    if app.reactor_running.is_some() || app.reactor_runtime.capture.is_some() {
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
    }
}
fn capture(app: &mut FilmcraftApp) -> Result<(), String> {
    if app.reactor_runtime.capture.is_some() {
        return Err("A frame capture is already running.".into());
    }
    if app.ui.reactor.reference_images.len() >= 9 {
        return Err("Use at most 9 image references.".into());
    }
    let target = filmcraft_engine::keyboard::frame_export_target(&app.session, &json!({"target":"program"})).map_err(|e| e.to_string())?;
    let path = std::path::PathBuf::from(project_output(app.session.path.as_deref())?).with_extension("png");
    let project = app.session.project.clone();
    let provider = app.session.media.full_res_provider(project.clone(), app.session.services.clone());
    app.reactor_runtime.capture_project = app.session.path.clone();
    app.reactor_runtime.capture = Some(reference_media::worker(move || {
        use std::io::Write;
        let _ = filmcraft_media::pending::take();
        let image = filmcraft_render::render_sequence(
            &project,
            target.item,
            target.time,
            filmcraft_render::RenderOptions { captions: true, ..Default::default() },
            &provider,
        );
        if filmcraft_media::pending::take() {
            return Err("The frame is still loading. Try capture again in a moment.".into());
        }
        let bytes = filmcraft_export::still::encode(&image, filmcraft_export::still::StillFormat::Png, 8).map_err(|e| e.to_string())?;
        let parent = path.parent().ok_or("Invalid reference folder")?;
        std::fs::create_dir_all(parent).map_err(|e| e.to_string())?;
        let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).map_err(|e| e.to_string())?;
        if let Err(error) = file.write_all(&bytes) {
            drop(file);
            let _ = std::fs::remove_file(&path);
            return Err(error.to_string());
        }
        path.to_str().map(str::to_owned).ok_or_else(|| "Invalid reference path".into())
    }));
    Ok(())
}
fn audio_action(app: &mut FilmcraftApp, params: &Value) -> Result<Value, String> {
    let path =
        params["path"].as_str().filter(|p| app.ui.reactor.reference_audios.iter().any(|r| r == p)).ok_or("Choose an audio reference from this composer")?;
    let action = params["action"].as_str().ok_or("Choose play, pause or seek")?;
    let path = path.to_string();
    let mut media = std::mem::take(&mut app.reactor_runtime.media);
    let position = media.playback.as_ref().filter(|p| p.path == path).map_or(Tick::ZERO, |p| p.position);
    let result = match action {
        "pause" => {
            media.stop(app);
            Ok(())
        }
        "play" => media.play(app, &path, position),
        "seek" => {
            let seconds = params["seconds"].as_f64().filter(|v| v.is_finite() && *v >= 0.0 && *v <= 15.0);
            match seconds {
                Some(seconds) => {
                    let playing = media.playback.as_ref().is_some_and(|p| p.path == path && p.playing);
                    if playing {
                        media.play(app, &path, Tick::from_seconds_f64(seconds))
                    } else {
                        media.stop(app);
                        media.set_position(&path, Tick::from_seconds_f64(seconds))
                    }
                }
                None => Err("Seek time must be between 0 and 15 seconds.".into()),
            }
        }
        _ => Err("Choose play, pause or seek".into()),
    };
    app.reactor_runtime.media = media;
    result.map(|()| json!({"audio":action}))
}
fn invoke(app: &mut FilmcraftApp, ctx: &Context, id: &str, params: Value) {
    if let Err(error) = crate::menus::invoke(app, ctx, id, params) {
        app.ui.reactor.message = error;
    }
}
fn request_preview(app: &mut FilmcraftApp, path: &str, audio: bool) {
    let mut media = std::mem::take(&mut app.reactor_runtime.media);
    media.request(app, path, audio);
    app.reactor_runtime.media = media;
}
fn paint_image(app: &mut FilmcraftApp, ui: &mut egui::Ui, path: &str, rect: Rect, square: bool) {
    if !ui.is_rect_visible(rect) {
        return;
    }
    request_preview(app, path, false);
    ui.painter().rect_filled(rect, 4.0, app.tokens.field_bg);
    if let Some(texture) = app.reactor_runtime.media.texture(path) {
        let size = texture.size_vec2();
        let (uv, destination) = if square {
            let aspect = size.x / size.y.max(1.0);
            let uv = if aspect > 1.0 {
                Rect::from_center_size(pos2(0.5, 0.5), vec2(1.0 / aspect, 1.0))
            } else {
                Rect::from_center_size(pos2(0.5, 0.5), vec2(1.0, aspect))
            };
            (uv, rect)
        } else {
            (
                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                Rect::from_center_size(rect.center(), size * (rect.width() / size.x.max(1.0)).min(rect.height() / size.y.max(1.0))),
            )
        };
        ui.painter().image(texture.id(), destination, uv, egui::Color32::WHITE);
    } else {
        icons::paint(ui.painter(), Rect::from_center_size(rect.center(), vec2(20.0, 20.0)), Icon::Image, app.tokens.icon);
        if let Some(error) = app.reactor_runtime.media.error(path, false) {
            ui.interact(rect, ui.id().with(path), Sense::hover()).on_hover_text(error);
        } else {
            ui.ctx().request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}

pub fn show(app: &mut FilmcraftApp, ui: &mut egui::Ui, rect: Rect) {
    let mut ui = ui.new_child(egui::UiBuilder::new().max_rect(rect.shrink(10.0)));
    let ctx = ui.ctx().clone();
    ui.label(egui::RichText::new("FastH3 Omni").size(15.0).strong());
    ui.add_space(8.0);
    let jobs: Vec<Job> = app.reactor_runtime.jobs.iter().filter(|j| j.current(app)).rev().cloned().collect();
    ui.horizontal(|ui| {
        ui.label(tl!("Generations"));
        ui.label(egui::RichText::new(jobs.len().to_string()).color(app.tokens.text_dim));
        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
            for (icon, grid, tip) in [(Icon::ListView, false, tl!("List View")), (Icon::IconView, true, tl!("Grid View"))] {
                let response = icons::button(ui, icon, 26.0, app.ui.reactor.grid == grid, &app.tokens, tip);
                app.auto.add(if grid { "reactor.view.grid" } else { "reactor.view.list" }, response.rect, tip);
                if response.clicked() {
                    invoke(app, &ctx, "reactor.view", json!({"view":if grid {"grid"}else{"list"}}));
                }
            }
        });
    });
    let remaining = ui.available_rect_before_wrap();
    // Keep the composer anchored; a tiny floating panel scrolls instead of clipping its controls.
    if remaining.height() < 400.0 {
        egui::ScrollArea::vertical().id_salt("reactor.small").show(&mut ui, |ui| {
            gallery(app, ui, &jobs, 170.0);
            composer(app, ui);
        });
    } else {
        let measure_id = egui::Id::new("reactor.composer.measure");
        let measured = ctx.data(|data| data.get_temp::<egui::Vec2>(measure_id));
        let has_refs = !app.ui.reactor.reference_images.is_empty() || !app.ui.reactor.reference_audios.is_empty();
        let fallback = 220.0 + if has_refs { 88.0 } else { 0.0 };
        let desired = measured.filter(|size| (size.x - remaining.width()).abs() < 1.0).map_or(fallback, |size| size.y);
        let composer_height = desired.min(remaining.height() - 105.0).max(100.0);
        let gallery_height = (remaining.height() - composer_height - 8.0).max(90.0);
        gallery(app, &mut ui, &jobs, gallery_height);
        let composer_rect = Rect::from_min_max(pos2(remaining.left(), remaining.bottom() - composer_height), remaining.right_bottom());
        let mut composer_ui = ui.new_child(egui::UiBuilder::new().max_rect(composer_rect));
        let output = egui::ScrollArea::vertical().id_salt("reactor.composerScroll").auto_shrink([false, false]).show(&mut composer_ui, |ui| {
            let top = ui.cursor().top();
            composer(app, ui);
            ui.cursor().top() - top
        });
        ctx.data_mut(|data| data.insert_temp(measure_id, vec2(remaining.width(), output.inner)));
        if (output.inner - desired).abs() > 1.0 {
            ctx.request_repaint();
        }
    }
}
fn gallery(app: &mut FilmcraftApp, ui: &mut egui::Ui, jobs: &[Job], height: f32) {
    egui::ScrollArea::vertical().id_salt("reactor.history").auto_shrink([false, false]).max_height(height).min_scrolled_height(height).show(ui, |ui| {
        if jobs.is_empty() {
            ui.add_space(18.0);
            ui.vertical_centered(|ui| {
                ui.label(tl!("Your next shot starts here"));
                ui.label(egui::RichText::new(tl!("Add references and describe your shot.")).small().color(app.tokens.text_dim));
            });
            return;
        }
        let columns = if app.ui.reactor.grid && ui.available_width() >= 230.0 { 2 } else { 1 };
        let width = ((ui.available_width() - 8.0 * (columns - 1) as f32) / columns as f32).max(40.0);
        egui::Grid::new("reactor.cards").spacing(vec2(8.0, 8.0)).num_columns(columns).show(ui, |ui| {
            for (index, job) in jobs.iter().enumerate() {
                ui.vertical(|ui| {
                    ui.set_width(width);
                    card(app, ui, job, width);
                });
                if (index + 1) % columns == 0 {
                    ui.end_row();
                }
            }
        });
    });
}
fn card(app: &mut FilmcraftApp, ui: &mut egui::Ui, job: &Job, width: f32) {
    let ctx = ui.ctx().clone();
    egui::Frame::new().fill(app.tokens.field_bg).corner_radius(5.0).inner_margin(8).show(ui, |ui| {
        ui.set_width((width - 16.0).max(24.0));
        let (rect, _) = ui.allocate_exact_size(vec2((width - 16.0).max(24.0), (width - 16.0).max(24.0) * 9.0 / 16.0), Sense::hover());
        match job.status {
            Status::Imported | Status::Saved => paint_image(app, ui, &job.request.output_path, rect, false),
            _ => {
                ui.painter().rect_filled(rect, 3.0, app.tokens.control_well);
                let mut overlay = ui.new_child(egui::UiBuilder::new().max_rect(rect));
                overlay.centered_and_justified(|ui| {
                    if job.status == Status::Running {
                        ui.spinner();
                    } else {
                        ui.label(if job.status == Status::Queued { tl!("Queued") } else { tl!("Failed") });
                    }
                });
            }
        }
        let title: String = job.request.prompt.chars().take(65).collect();
        ui.add(egui::Label::new(egui::RichText::new(title).small()).truncate()).on_hover_text(&job.request.prompt);
        ui.label(egui::RichText::new(format!("{} s · {}", job.request.seconds, job.request.aspect)).small().color(app.tokens.text_dim));
        if matches!(job.status, Status::Queued | Status::Running) {
            ui.add(egui::Label::new(egui::RichText::new(&job.message).small()).wrap());
        } else {
            let message = if job.status == Status::Imported { tl!("In project") } else { &job.message };
            ui.add(egui::Label::new(egui::RichText::new(message).small().color(app.tokens.text_dim)).truncate()).on_hover_text(&job.message);
        }
        if job.status == Status::Queued {
            let cancel = ui.small_button(tl!("Cancel"));
            app.auto.add(&format!("reactor.cancel.{}", job.id), cancel.rect, "Cancel queued generation");
            if cancel.clicked() {
                invoke(app, &ctx, "reactor.cancelQueued", json!({"id":job.id}));
            }
        }
        let reuse = ui.small_button(tl!("Reuse"));
        app.auto.add(&format!("reactor.reuse.{}", job.id), reuse.rect, "Reuse generation prompt, settings and references");
        if reuse.clicked() {
            invoke(app, &ctx, "reactor.reuse", json!({"id":job.id}));
        }
    });
}
fn composer(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    let ctx = ui.ctx().clone();
    let frame = egui::Frame::new().fill(app.tokens.control_well).stroke(egui::Stroke::new(1.0, app.tokens.field_border)).corner_radius(7.0).inner_margin(10);
    let form = frame.show(ui, |ui| {
        ui.set_width(ui.available_width().max(20.0));
        let images = app.ui.reactor.reference_images.clone();
        let audios = app.ui.reactor.reference_audios.clone();
        if !images.is_empty() || !audios.is_empty() {
            egui::ScrollArea::horizontal().id_salt("reactor.referenceTray").show(ui, |ui| {
                ui.horizontal(|ui| {
                    for (index, path) in images.iter().enumerate() {
                        ui.vertical(|ui| {
                            let (rect, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
                            paint_image(app, ui, path, rect, true);
                            ui.label(egui::RichText::new(format!("Picture{}", index + 1)).small());
                            let remove_rect = Rect::from_min_size(rect.right_top() - vec2(19.0, 0.0), vec2(19.0, 19.0));
                            let mut remove_ui = ui.new_child(egui::UiBuilder::new().max_rect(remove_rect));
                            let remove = icons::button(&mut remove_ui, Icon::Close, 19.0, false, &app.tokens, tl!("Remove reference"));
                            app.auto.add(&format!("reactor.image.remove.{index}"), remove.rect, "Remove image reference");
                            if remove.clicked() {
                                invoke(app, &ctx, "reactor.references", json!({"kind":"image","remove":index}));
                            }
                        });
                    }
                    for (index, path) in audios.iter().enumerate() {
                        ui.vertical(|ui| {
                            audio_tile(app, ui, path, index);
                        });
                    }
                });
            });
            ui.add_space(5.0);
        }
        let response = ui.add(
            egui::TextEdit::multiline(&mut app.ui.reactor.prompt)
                .id_salt("reactor.prompt.edit")
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .frame(egui::Frame::NONE)
                .hint_text(tl!("Describe the scene, movement, and sound…")),
        );
        response.widget_info(|| {
            let mut info = egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, ui.is_enabled(), tl!("Describe your shot"));
            info.current_text_value = Some(app.ui.reactor.prompt.clone());
            info
        });
        app.auto.add_response("reactor.prompt", &response, "Reactor prompt");
        if app.ui.reactor.prompt.len() > 2048 {
            ui.colored_label(app.tokens.accent, tl!("Prompt exceeds 2048 bytes"));
        }
        ui.horizontal_wrapped(|ui| {
            for (image, label, id) in [(true, tl!("Image"), "reactor.images"), (false, tl!("Audio"), "reactor.audios")] {
                let enabled = if image { images.len() < 9 } else { app.ui.reactor.reference_audios.len() < 3 };
                let response = ui.add_enabled(enabled, egui::Button::new(format!("+ {label}")).small());
                app.auto.add(id, response.rect, label);
                if response.clicked() {
                    let extensions = if image { vec!["png", "jpg", "jpeg", "webp"] } else { vec!["wav", "mp3", "m4a", "aac", "ogg", "opus", "flac", "webm"] };
                    app.pick_ui(FileDialog::open_files(&extensions), move |app, paths| {
                        if paths.is_empty() {
                            return;
                        }
                        if let Some(Err(error)) = route(app, "reactor.references", &json!({"kind":if image{"image"}else{"audio"},"paths":paths})) {
                            app.ui.reactor.message = error;
                        }
                    });
                }
            }
            let capture = ui.add_enabled(images.len() < 9 && app.reactor_runtime.capture.is_none(), egui::Button::new(tl!("Current frame")).small());
            app.auto.add("reactor.capture", capture.rect, "Capture current Program frame as image reference");
            if capture.clicked() {
                invoke(app, &ctx, "reactor.captureFrame", json!({}));
            }
        });
        ui.separator();
        ui.horizontal_wrapped(|ui| {
            egui::ComboBox::from_id_salt("reactor.aspect").width(58.0).selected_text(&app.ui.reactor.aspect).show_ui(ui, |ui| {
                for aspect in ["16:9", "9:16", "4:3", "1:1"] {
                    let r = ui.selectable_value(&mut app.ui.reactor.aspect, aspect.into(), aspect);
                    app.auto.add(&format!("reactor.aspect.{aspect}"), r.rect, aspect);
                }
            });
            egui::ComboBox::from_id_salt("reactor.seconds").width(58.0).selected_text(format!("{} s", app.ui.reactor.seconds)).show_ui(ui, |ui| {
                for seconds in [5.875, 8.0, 10.125, 12.25, 14.375] {
                    let r = ui.selectable_value(&mut app.ui.reactor.seconds, seconds, format!("{seconds} s"));
                    app.auto.add(&format!("reactor.seconds.{seconds}"), r.rect, "Duration");
                }
            });
            let enabled = app.session.path.is_some() && app.hooks.reactor_generate.is_some();
            let generate = ui.add_enabled(enabled, egui::Button::new(tl!("Generate and import")).fill(app.tokens.pill_active_bg));
            app.auto.add("reactor.start", generate.rect, "Generate and import Reactor clip");
            if generate.clicked() {
                let draft = &app.ui.reactor;
                let request = Request {
                    prompt: draft.prompt.clone(),
                    seconds: draft.seconds,
                    aspect: draft.aspect.clone(),
                    reference_images: draft.reference_images.clone(),
                    reference_audios: draft.reference_audios.clone(),
                    output_path: String::new(),
                };
                invoke(app, &ctx, "reactor.generate", json!(request));
            }
        });
    });
    app.auto.add("reactor.composer", form.response.rect, "Generation composer");
    if media_folder(app.session.path.as_deref()).is_err() {
        ui.label(tl!("Save the project before generating. Clips are saved beside the project and imported automatically."));
        let save = ui.button(tl!("Save"));
        app.auto.add("reactor.saveProject", save.rect, "Save project for Reactor clips");
        if save.clicked() {
            invoke(app, &ctx, "file.saveAs", json!({}));
        }
    }
    if !app.ui.reactor.message.is_empty() {
        ui.label(&app.ui.reactor.message);
    }
    if app.hooks.reactor_generate.is_none() {
        ui.label(tl!("Generation is available in the desktop app after adding a Reactor API key in Settings."));
    }
}
fn audio_tile(app: &mut FilmcraftApp, ui: &mut egui::Ui, path: &str, index: usize) {
    request_preview(app, path, true);
    let ctx = ui.ctx().clone();
    let (tile, _) = ui.allocate_exact_size(vec2(64.0, 64.0), Sense::hover());
    ui.painter().rect_filled(tile, 4.0, app.tokens.control_surface);
    ui.interact(tile, ui.id().with(("audio-name", path)), Sense::hover()).on_hover_text(path);
    let remove_rect = Rect::from_min_size(tile.right_top() - vec2(19.0, 0.0), vec2(19.0, 19.0));
    let mut remove_ui = ui.new_child(egui::UiBuilder::new().max_rect(remove_rect));
    let remove = icons::button(&mut remove_ui, Icon::Close, 19.0, false, &app.tokens, tl!("Remove reference"));
    app.auto.add(&format!("reactor.audio.remove.{index}"), remove.rect, "Remove audio reference");
    if remove.clicked() {
        invoke(app, &ctx, "reactor.references", json!({"kind":"audio","remove":index}));
    }
    let loaded = app.reactor_runtime.media.audio(path).map(|(sound, peaks)| (sound, peaks.to_vec()));
    if let Some((sound, peaks)) = loaded {
        let duration = Tick::from_units(sound.frames() as i64, i64::from(sound.sample_rate));
        let state = app.reactor_runtime.media.playback.as_ref().filter(|p| p.path == path);
        let (playing, position) = state.map_or((false, Tick::ZERO), |p| (p.playing, p.position));
        let play_rect = Rect::from_min_size(tile.min + vec2(2.0, 1.0), vec2(22.0, 22.0));
        let mut play_ui = ui.new_child(egui::UiBuilder::new().max_rect(play_rect));
        let play = icons::button(
            &mut play_ui,
            if playing { Icon::Pause } else { Icon::Play },
            22.0,
            false,
            &app.tokens,
            if playing { tl!("Pause") } else { tl!("Play") },
        );
        app.auto.add(&format!("reactor.audio.play.{index}"), play.rect, "Play or pause audio reference");
        if play.clicked() {
            invoke(app, &ctx, "reactor.audio", json!({"path":path,"action":if playing{"pause"}else{"play"}}));
        }
        let waveform = Rect::from_min_size(tile.min + vec2(5.0, 25.0), vec2(54.0, 18.0));
        let mut seconds = position.seconds();
        let mut seek_ui = ui.new_child(egui::UiBuilder::new().max_rect(waveform));
        seek_ui.spacing_mut().slider_width = 54.0;
        let response = seek_ui.add(egui::Slider::new(&mut seconds, 0.0..=duration.seconds()).show_value(false));
        app.auto.add_response(&format!("reactor.audio.seek.{index}"), &response, "Seek audio reference");
        let ratio = if duration.0 > 0 { (position.0 as f64 / duration.0 as f64).clamp(0.0, 1.0) as f32 } else { 0.0 };
        let max = peaks.iter().copied().fold(0.001f32, f32::max);
        let bars: Vec<f32> = peaks.chunks(peaks.len().div_ceil(26).max(1)).map(|chunk| chunk.iter().copied().fold(0.0, f32::max)).collect();
        for (i, peak) in bars.iter().enumerate() {
            let x = waveform.left() + (i as f32 + 0.5) * waveform.width() / bars.len().max(1) as f32;
            let h = (peak / max * 7.0).max(0.7);
            let color = if x <= waveform.left() + ratio * waveform.width() { app.tokens.accent } else { app.tokens.text_dim };
            ui.painter().line_segment([pos2(x, waveform.center().y - h), pos2(x, waveform.center().y + h)], egui::Stroke::new(1.0, color));
        }
        let x = waveform.left() + ratio * waveform.width();
        ui.painter().line_segment([pos2(x, waveform.top()), pos2(x, waveform.bottom())], egui::Stroke::new(1.0, app.tokens.text));
        if response.changed() {
            invoke(app, &ctx, "reactor.audio", json!({"path":path,"action":"seek","seconds":seconds}));
        }
        ui.painter().text(
            tile.left_bottom() + vec2(5.0, -5.0),
            egui::Align2::LEFT_BOTTOM,
            format!("{:.1}/{:.1}s", position.seconds(), duration.seconds()),
            egui::FontId::proportional(9.0),
            app.tokens.text_dim,
        );
    } else if let Some(error) = app.reactor_runtime.media.error(path, true) {
        ui.painter().text(tile.center(), egui::Align2::CENTER_CENTER, "!", egui::FontId::proportional(16.0), app.tokens.text_dim);
        ui.interact(tile, ui.id().with(("audio-error", path)), Sense::hover()).on_hover_text(error);
    } else {
        let mut loading = ui.new_child(egui::UiBuilder::new().max_rect(tile.shrink(20.0)));
        loading.spinner();
    }
    ui.label(egui::RichText::new(format!("Audio{}", index + 1)).small());
}
#[cfg(test)]
mod tests {
    use super::*;
    use egui_kittest::{Harness, kittest::Queryable};
    use std::{cell::RefCell, rc::Rc};

    fn app() -> (FilmcraftApp, Rc<RefCell<Vec<Request>>>) {
        let calls = Rc::new(RefCell::new(Vec::new()));
        let captured = calls.clone();
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.ui.reactor.prompt = "A paper boat on a lake".into();
        app.hooks.reactor_generate = Some(Box::new(move |request| {
            captured.borrow_mut().push(request);
            let (_, rx) = std::sync::mpsc::channel();
            rx
        }));
        app.hooks.pick_save_as = Some(Box::new(|_, _, _| panic!("generation must not ask for a clip filename")));
        (app, calls)
    }

    #[test]
    fn generate_uses_the_saved_project_without_a_clip_save_dialog() {
        let (mut app, calls) = app();
        let project = std::env::temp_dir().join("My Film.fcproj");
        app.session.path = Some(project.to_str().unwrap().into());
        let expected = project.parent().unwrap().join("My Film Media").join("Reactor");
        let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(|ui, app| show(app, ui, ui.available_rect_before_wrap()), app);
        h.run_steps(3);
        h.get_by_label("Generate and import").click();
        h.run_steps(3);
        let calls = calls.borrow();
        assert_eq!(calls.len(), 1);
        let output = std::path::Path::new(&calls[0].output_path);
        assert_eq!(output.parent(), Some(expected.as_path()));
        assert_eq!(output.extension().unwrap(), "mp4");
        assert_ne!(project_output(Some(project.to_str().unwrap())).unwrap(), calls[0].output_path);
    }

    #[test]
    fn unsaved_project_cannot_generate_and_saving_enables_project_storage() {
        let (mut app, calls) = app();
        let dir = std::env::temp_dir().join(format!("fc-reactor-save-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let project = dir.join("Untitled.fcproj");
        let answer = Rc::new(RefCell::new(None::<String>));
        let chosen = answer.clone();
        app.hooks.pick_save = Some(Box::new(move |_| chosen.borrow().clone()));
        assert!(route(&mut app, "reactor.generate", &json!({"prompt":"lake", "seconds":8, "aspect":"16:9"})).unwrap().is_err());
        assert!(calls.borrow().is_empty());
        let mut h = Harness::builder().with_size(egui::vec2(1000.0, 800.0)).build_ui_state(|ui, app| show(app, ui, ui.available_rect_before_wrap()), app);
        h.run_steps(3);
        h.get_by_label("Save").click();
        h.run_steps(3);
        assert!(h.state().session.path.is_none(), "cancelling leaves the project unsaved");
        assert!(calls.borrow().is_empty());
        *answer.borrow_mut() = Some(project.to_str().unwrap().into());
        h.get_by_label("Save").click();
        h.run_steps(3);
        assert!(project.is_file());
        h.get_by_label("Generate and import").click();
        h.run_steps(3);
        assert_eq!(calls.borrow().len(), 1);
        assert!(std::path::Path::new(&calls.borrow()[0].output_path).starts_with(dir.join("Untitled Media").join("Reactor")));
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn invalid_project_paths_cannot_choose_a_default_destination() {
        assert!(media_folder(None).is_err());
        assert!(media_folder(Some("relative.fcproj")).is_err());
        assert!(media_folder(Some("/")).is_err());
    }
    fn request(prompt: &str) -> Request {
        Request {
            prompt: prompt.into(),
            seconds: 8.0,
            aspect: "16:9".into(),
            reference_images: vec!["/tmp/character.png".into()],
            reference_audios: vec!["/tmp/voice.wav".into()],
            output_path: String::new(),
        }
    }

    #[test]
    fn queue_keeps_request_snapshots_and_pauses_on_project_switch() {
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        let project = std::env::temp_dir().join("Reactor queue.fcproj").to_str().unwrap().to_string();
        app.session.path = Some(project.clone());
        let calls = Rc::new(RefCell::new(Vec::new()));
        let senders = Rc::new(RefCell::new(Vec::new()));
        let (recorded, channels) = (calls.clone(), senders.clone());
        app.hooks.reactor_generate = Some(Box::new(move |request| {
            recorded.borrow_mut().push(request);
            let (tx, rx) = std::sync::mpsc::channel();
            channels.borrow_mut().push(tx);
            rx
        }));
        start(&mut app, request("First shot")).unwrap();
        start(&mut app, request("Second shot")).unwrap();
        assert_eq!(calls.borrow().len(), 1, "queue submits one billable request at a time");
        let first = app.reactor_runtime.jobs[0].clone();
        let second = app.reactor_runtime.jobs[1].clone();
        assert_ne!(first.request.output_path, second.request.output_path);
        app.ui.reactor.reference_images.clear();
        app.session.path = Some(std::env::temp_dir().join("Other.fcproj").to_str().unwrap().into());
        senders.borrow()[0].send(Event::Finished(Err("test failure".into()))).unwrap();
        let ctx = Context::default();
        poll(&mut app, &ctx);
        assert_eq!(calls.borrow().len(), 1, "a switched project cannot launch another project's pending request");
        app.session.path = Some(project);
        poll(&mut app, &ctx);
        assert_eq!(calls.borrow().len(), 2);
        assert_eq!(calls.borrow()[1].prompt, "Second shot");
        assert_eq!(calls.borrow()[1].reference_images, ["/tmp/character.png"]);
        route(&mut app, "reactor.reuse", &json!({"id":first.id})).unwrap().unwrap();
        assert_eq!(app.ui.reactor.prompt, "First shot");
        assert_eq!(app.ui.reactor.reference_audios, ["/tmp/voice.wav"]);
        assert_eq!(app.ui.reactor.reference_images, ["/tmp/character.png"]);
        assert_eq!(app.ui.reactor.seconds, 8.0);
        assert_eq!(app.ui.reactor.aspect, "16:9");
    }

    #[test]
    fn completed_generation_imports_automatically_but_never_into_another_project() {
        let dir = std::env::temp_dir().join(format!("fc-reactor-import-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("generated.png");
        image::RgbaImage::from_pixel(8, 4, image::Rgba([40, 90, 130, 255])).save(&path).unwrap();
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.path = Some(dir.join("Film.fcproj").to_str().unwrap().into());
        app.hooks.reactor_generate = Some(Box::new(|_| std::sync::mpsc::channel().1));
        start(&mut app, request("First shot")).unwrap();
        let id = app.reactor_runtime.jobs[0].id;
        let before = app.session.project.items.len();
        finish(&mut app, id, Ok(path.to_str().unwrap().into()));
        assert_eq!(app.reactor_runtime.jobs[0].status, Status::Imported);
        assert_eq!(app.session.project.items.len(), before + 1);
        let project = app.session.project.clone();
        app.session.path = Some(dir.join("Other.fcproj").to_str().unwrap().into());
        finish(&mut app, id, Ok(path.to_str().unwrap().into()));
        assert_eq!(app.reactor_runtime.jobs[0].status, Status::Saved);
        assert_eq!(app.session.project, project);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn hostile_panel_commands_are_errors_without_project_edits() {
        let (mut app, _) = app();
        let before = app.session.project.clone();
        for (id, params) in [
            ("reactor.view", json!({"view":"huge"})),
            ("reactor.references", json!({"kind":"image","remove":999999999999u64})),
            ("reactor.references", json!({"kind":"audio","paths":["relative.wav"]})),
            ("reactor.references", json!({"kind":"image","paths":vec!["/tmp/a";10]})),
            ("reactor.reuse", json!({"id":999})),
            ("reactor.audio", json!({"path":"/tmp/voice.wav","action":"seek","seconds":-5})),
            ("reactor.generate", json!({"prompt":"x","seconds":1e90,"aspect":"16:9"})),
        ] {
            assert!(route(&mut app, id, &params).unwrap().is_err(), "{id}: {params}");
        }
        assert_eq!(app.session.project, before);
        let old: Draft = serde_json::from_value(json!({"prompt":"older draft"})).unwrap();
        assert!(old.grid);
    }
    #[test]
    fn composer_keeps_keyboard_focus_when_reference_layout_changes() {
        let (mut app, _) = app();
        app.ui.reactor.prompt.clear();
        let mut h = Harness::builder().with_size(vec2(440.0, 850.0)).build_ui_state(|ui, app| show(app, ui, ui.available_rect_before_wrap()), app);
        h.run_steps(3);
        h.get_by_label("Describe your shot").click();
        h.run_steps(2);
        h.get_by_label("Describe your shot").type_text("First");
        h.run_steps(2);
        assert_eq!(h.state().ui.reactor.prompt, "First");
        h.state_mut().ui.reactor.reference_images.push("/tmp/another-reference.png".into());
        h.run_steps(3);
        h.get_by_label("Describe your shot").type_text(" second");
        h.run_steps(2);
        assert_eq!(h.state().ui.reactor.prompt, "First second");
    }

    #[test]
    fn grid_and_list_use_local_thumbnails_without_changing_project() {
        let dir = std::env::temp_dir().join(format!("fc-reactor-gallery-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let picture = dir.join("Character.png");
        image::RgbaImage::from_pixel(80, 40, image::Rgba([75, 120, 160, 255])).save(&picture).unwrap();
        let audio = dir.join("Voice.wav");
        let samples: Vec<_> = (0..96000).map(|i| ((i as f32 / 24000.0) * 440.0 * std::f32::consts::TAU).sin() * 0.2).collect();
        std::fs::write(&audio, filmcraft_media::wav::write_wav16(&samples, 1, 24000)).unwrap();
        let (mut app, _) = app();
        app.session.path = Some(dir.join("Northbound.fcproj").to_str().unwrap().into());
        app.ui.reactor.reference_images = vec![picture.to_str().unwrap().into()];
        app.ui.reactor.reference_audios = vec![audio.to_str().unwrap().into()];
        for id in 1..=4 {
            let mut req = request(&format!("Ridge walk · take {id}"));
            req.output_path = picture.to_str().unwrap().into();
            app.reactor_runtime.jobs.push(Job {
                id,
                request: req,
                project_name: app.session.project.name.clone(),
                project_path: app.session.path.clone(),
                bin: app.import_bin(),
                status: Status::Imported,
                message: "In project".into(),
            });
        }
        let project = app.session.project.clone();
        let screenshots = std::env::var_os("FILMCRAFT_REACTOR_SNAPSHOT_DIR").map(std::path::PathBuf::from);
        let mut builder = Harness::builder().with_size(vec2(440.0, 850.0));
        if screenshots.is_some() {
            builder = builder.wgpu();
        }
        let mut h = builder.build_ui_state(
            |ui, app| {
                app.auto.begin_frame();
                poll(app, ui.ctx());
                show(app, ui, ui.available_rect_before_wrap());
            },
            app,
        );
        for _ in 0..100 {
            h.run_steps(1);
            if h.state().reactor_runtime.media.texture(picture.to_str().unwrap()).is_some()
                && h.state().reactor_runtime.media.audio(audio.to_str().unwrap()).is_some()
            {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(h.state().reactor_runtime.media.texture(picture.to_str().unwrap()).is_some());
        assert!(h.state().reactor_runtime.media.audio(audio.to_str().unwrap()).is_some());
        let form = h.state().auto.find("reactor.composer").unwrap().rect;
        assert!(850.0 - (form[1] + form[3]) <= 24.0, "composer must sit near the panel bottom: {form:?}");
        let waveform = h.state().auto.find("reactor.audio.seek.0").unwrap().rect;
        assert!(waveform[2] <= 64.0, "audio waveform must fit in a square reference tile");
        let first = h.state().auto.find("reactor.reuse.4").unwrap().rect;
        let second = h.state().auto.find("reactor.reuse.3").unwrap().rect;
        assert_eq!(first[1], second[1]);
        assert!(second[0] > first[0]);
        if let Some(directory) = &screenshots {
            std::fs::create_dir_all(directory).unwrap();
            h.render().unwrap().save(directory.join("reactor-grid.png")).unwrap();
        }
        route(h.state_mut(), "reactor.view", &json!({"view":"list"})).unwrap().unwrap();
        h.run_steps(3);
        let first = h.state().auto.find("reactor.reuse.4").unwrap().rect;
        let second = h.state().auto.find("reactor.reuse.3").unwrap().rect;
        assert_eq!(first[0], second[0]);
        assert!(second[1] > first[1]);
        if let Some(directory) = &screenshots {
            h.render().unwrap().save(directory.join("reactor-list.png")).unwrap();
        }
        assert_eq!(h.state().session.project, project);
        std::fs::remove_dir_all(dir).unwrap();
    }
}

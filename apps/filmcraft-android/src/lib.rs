//! FilmCraft on Android — GameActivity + eframe/wgpu, reusing the desktop engine and egui UI.
//!
//! Nothing here is Android-specific beyond the entry point, the data directory, the touch size
//! adjustments and the font fallback: the same `filmcraft_engine::Session` and
//! `filmcraft_ui_egui::FilmcraftApp` the desktop shell builds are wired up here. What the desktop
//! shell gets from the OS, this crate takes from the activity:
//!
//! | Desktop (`apps/filmcraft`) | Android (this crate) |
//! |---|---|
//! | `std::env::args()` launch flags | [`winit::platform::android::activity::AndroidApp`] |
//! | data dir from `default_data_dir()` | [`AndroidApp::internal_data_path`] |
//! | rfd file dialogs | FilmCraft's own `HostHooks` (File ▸ Import / Open Project) start the SAF picker; the chosen files are copied into the app dir and handed to the engine over the control channel |
//! | cpal audio output | not wired yet (cpal's Android backend exists; needs permissions) |
//! | VideoToolbox hardware decode | MediaCodec in `crates/platform` (`src/mediacodec.rs`) |
//! | desktop-sized UI | [`TouchShell`] enlarges touch targets |
//!
//! The engine, the UI and the theme are untouched — the menus, the Import command and the project
//! panel are the desktop ones, so the phone uses the same code paths. The shell only adjusts egui's
//! spacing *after* FilmCraft installs its own theme on the first frame.
#![cfg(target_os = "android")]

use std::path::PathBuf;
use std::sync::mpsc::Sender;

use filmcraft_engine::Session;
use filmcraft_engine::autosave::AutosaveConfig;
use filmcraft_ui_egui::dock::WorkspacePrefs;
use filmcraft_ui_egui::{ControlRequest, FilmcraftApp};
use winit::platform::android::activity::AndroidApp;

mod saf;
mod selftest;

/// How much to scale the desktop UI up on a phone before the layout reflows.
///
/// Measured on a 1080×2400 @ 420 dpi phone: egui's logical size is 411×914 pt at
/// `pixels_per_point = 2.625`, while the desktop layout is designed for ≥900×560 pt. Scaling up
/// (tried 1.35) only shrinks the logical viewport further and makes the panels overlap more, so
/// the shell does **not** scale: the layout needs a rework (bottom toolbars, one panel at a time),
/// not a zoom. Keep this knob at 1.0 until that rework exists.
const TOUCH_SCALE: f32 = 1.0;

/// Touch shell: forward everything to [`FilmcraftApp`] and apply the phone adjustments once, after
/// FilmCraft's own theme is in place.
struct TouchShell {
    inner: FilmcraftApp,
    tuned: bool,
    /// The CJK fallback font has been added (after the theme's fonts went live).
    font_done: bool,
    /// Commands for the app's control channel (the desktop control server's entry point).
    control: Sender<ControlRequest>,
    /// The touch command palette is open.
    palette: bool,
    /// Press bookkeeping for the long-press gesture.
    press: Option<(std::time::Instant, egui::Pos2)>,
    /// The app's export directory: where the palette's explicit exports land.
    exports: Option<PathBuf>,
}

impl TouchShell {
    fn tune(ctx: &egui::Context) {
        let before = ctx.pixels_per_point();
        if (TOUCH_SCALE - 1.0).abs() > f32::EPSILON {
            ctx.set_pixels_per_point((before * TOUCH_SCALE).min(4.0));
        }
        ctx.all_styles_mut(|style| {
            // Touch targets: comfortable minimum, roomier padding, a little more separation so
            // neighbouring rows are harder to hit by accident.
            style.spacing.interact_size = egui::vec2(40.0, 40.0);
            style.spacing.button_padding = egui::vec2(10.0, 8.0);
            style.spacing.item_spacing = egui::vec2(8.0, 8.0);
            style.spacing.slider_width = style.spacing.slider_width.max(160.0);
            // Tooltips should not pop up from a resting finger.
            style.interaction.show_tooltips_only_when_still = true;
        });
        let rect = ctx.viewport_rect();
        log::info!(
            "filmcraft-android: touch mode applied (ppp {before}, logical {}×{} pt, interact {} pt)",
            rect.width().round(),
            rect.height().round(),
            ctx.style_of(ctx.theme()).spacing.interact_size.x,
        );
    }

    /// Whether the theme's own fonts are live (it defines named families such as "semibold").
    fn theme_fonts_live(ctx: &egui::Context) -> bool {
        ctx.fonts(|f| f.definitions().families.keys().any(|k| matches!(k, egui::FontFamily::Name(_))))
    }
}

impl eframe::App for TouchShell {
    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        self.inner.logic(ctx, frame);
    }

    /// Diagnostic: whether winit/eframe delivers input at all.
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        if !raw_input.events.is_empty() {
            log::info!("input: {} event(s), first {:?}", raw_input.events.len(), raw_input.events.first());
        }
        eframe::App::raw_input_hook(&mut self.inner, ctx, raw_input);
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.inner.ui(ui, frame);
        if !self.tuned {
            // The first `ui` pass runs FilmCraft's theme install, so adjust after it.
            self.tuned = true;
            Self::tune(ui.ctx());
        }
        if !self.font_done && Self::theme_fonts_live(ui.ctx()) {
            // The theme's fonts only become visible once egui has rebuilt them, a frame after the
            // theme install: adding the CJK fallback any earlier would drop the theme's families
            // (egui panics on a family that is bound to no font).
            self.font_done = true;
            install_system_font(ui.ctx());
        }
        self.gestures(ui.ctx());
        if self.palette {
            self.command_palette(ui.ctx());
        }
    }
}

impl TouchShell {
    /// Long-press on the strip where the desktop menu bar sits opens the command palette: those
    /// 12 pt menu items cannot be hit with a finger, so the same commands get finger-sized targets.
    fn gestures(&mut self, ctx: &egui::Context) {
        let (down, pos, released) = ctx.input(|i| (i.pointer.primary_down(), i.pointer.latest_pos(), i.pointer.primary_released()));
        if !down || released {
            self.press = None;
            return;
        }
        let Some(p) = pos else { return };
        // While the finger is down, keep frames coming so the 500 ms threshold can pass.
        ctx.request_repaint_after(std::time::Duration::from_millis(100));
        let strip = ctx.viewport_rect().height() * 0.08;
        match self.press {
            None => self.press = Some((std::time::Instant::now(), p)),
            Some((started, origin)) => {
                if origin.distance(p) > 16.0 {
                    self.press = None; // a drag, not a press
                } else if started.elapsed() > std::time::Duration::from_millis(500) {
                    if p.y < strip && !self.palette {
                        self.palette = true;
                        log::info!("android: long-press on the menu strip → command palette");
                    }
                    self.press = None;
                }
            }
        }
    }

    /// The touch replacement for the top menu bar: the File commands and the export entries as
    /// full-width buttons.
    ///
    /// Export runs the engine's own `file.exportMedia` (the shape the CLI and the bench pass) with
    /// explicit parameters, because the desktop dialog behind File ▸ Export ▸ Media File… cannot be
    /// driven with a finger; the file lands in the app's export directory. The job runs in the
    /// background (`wait: false`) — `wait: true` would block the UI thread — and
    /// [`spawn_export_watch`] follows it into logcat.
    fn command_palette(&mut self, ctx: &egui::Context) {
        const COMMANDS: [(&str, &str); 6] = [
            ("打开工程…", "file.open"),
            ("导入媒体…", "file.import"),
            ("保存", "file.save"),
            ("另存为…", "file.saveAs"),
            ("打开示例工程", "file.openDemoProject"),
            ("关闭工程", "file.close"),
        ];
        let exports = self.exports.clone();
        let mut close = false;
        egui::Area::new(egui::Id::new("command-palette")).anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 72.0)).show(ctx, |ui| {
            egui::Frame::popup(ui.style()).show(ui, |ui| {
                ui.set_min_width(260.0);
                ui.label(egui::RichText::new("命令（长按顶部菜单条打开）").size(13.0));
                ui.separator();
                for (label, id) in COMMANDS {
                    if ui.add_sized([260.0, 52.0], egui::Button::new(egui::RichText::new(label).size(18.0))).clicked() {
                        self.send(ctx, label, serde_json::json!({"command": id, "params": {}}));
                        close = true;
                    }
                }
                if let Some(dir) = &exports {
                    for (label, name, range) in
                        [("导出媒体（H.264，前 2 秒）", "export-2s.mp4", Some((0.0, 2.0))), ("导出媒体（H.264，整个序列）", "export-full.mp4", None)]
                    {
                        if ui.add_sized([260.0, 52.0], egui::Button::new(egui::RichText::new(label).size(18.0))).clicked() {
                            let mut params = serde_json::json!({
                                "path": dir.join(name).to_string_lossy(),
                                "format": "h264",
                                "wait": false,
                            });
                            if let Some((start, end)) = range {
                                params["range"] = serde_json::json!("custom");
                                params["startSeconds"] = serde_json::json!(start);
                                params["endSeconds"] = serde_json::json!(end);
                            }
                            self.send(ctx, label, serde_json::json!({"command": "file.exportMedia", "params": params}));
                            close = true;
                        }
                    }
                }
                if ui.add_sized([260.0, 52.0], egui::Button::new(egui::RichText::new("导出模式").size(18.0))).clicked() {
                    self.send(ctx, "导出模式", serde_json::json!({"command": "mode.export", "params": {}}));
                    close = true;
                }
                if ui.add_sized([260.0, 44.0], egui::Button::new(egui::RichText::new("取消").size(16.0))).clicked() {
                    close = true;
                }
            });
        });
        if close {
            self.palette = false;
        }
    }

    /// Run one command over the app's control channel (`engine.execute`, the same entry point the
    /// desktop control server and MCP use) and log it: the request's payload is the logcat record
    /// of what the palette did. Export jobs are followed to the end by [`spawn_export_watch`].
    fn send(&mut self, ctx: &egui::Context, label: &str, payload: serde_json::Value) {
        let (request, reply) = ControlRequest::new("engine.execute", payload.clone());
        match self.control.send(request) {
            Ok(()) => log::info!("palette: {label} → {payload}"),
            Err(_) => {
                log::warn!("palette: control channel is closed");
                return;
            }
        }
        if payload.get("command").and_then(serde_json::Value::as_str) == Some("file.exportMedia") {
            // The app services the control channel while it runs a frame, so nudge it: an idle
            // editor does not repaint on its own and the watcher's poll would never be answered.
            ctx.request_repaint();
            spawn_export_watch(self.control.clone(), ctx.clone(), reply);
        }
    }
}

/// Follow an export started from the palette to the end: log the command's reply and then poll
/// `jobs.list` for the job's progress, so logcat carries the export's start, progress and result.
/// Runs on its own thread — an export must never block the UI thread.
fn spawn_export_watch(control: Sender<ControlRequest>, ctx: egui::Context, reply: std::sync::mpsc::Receiver<serde_json::Value>) {
    std::thread::spawn(move || {
        let job = match reply.recv_timeout(std::time::Duration::from_secs(60)) {
            Ok(value) => {
                log::info!("export: {value}");
                value.get("result").and_then(|r| r.get("job")).and_then(serde_json::Value::as_u64)
            }
            Err(e) => {
                log::warn!("export: no reply: {e}");
                return;
            }
        };
        let Some(job) = job else { return };
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(7200);
        let mut last = String::new();
        loop {
            ctx.request_repaint();
            let (request, reply) = ControlRequest::new("engine.execute", serde_json::json!({"command": "jobs.list", "params": {}}));
            if control.send(request).is_err() {
                log::warn!("export: control channel is closed");
                return;
            }
            let Ok(value) = reply.recv_timeout(std::time::Duration::from_secs(60)) else {
                log::warn!("export: no reply for jobs.list");
                return;
            };
            let jobs = value.get("result").cloned().unwrap_or(serde_json::Value::Null);
            let job_value = jobs.as_array().and_then(|a| a.iter().find(|j| j.get("id").and_then(serde_json::Value::as_u64) == Some(job))).cloned();
            let Some(job_value) = job_value else {
                log::warn!("export: job {job} left the list");
                return;
            };
            let line = format!(
                "job {job} {} progress {:.1}% {}/{} {}",
                job_value.get("label").and_then(serde_json::Value::as_str).unwrap_or(""),
                job_value.get("progress").and_then(serde_json::Value::as_f64).unwrap_or(0.0) * 100.0,
                job_value.get("done").and_then(serde_json::Value::as_u64).unwrap_or(0),
                job_value.get("total").and_then(serde_json::Value::as_u64).unwrap_or(0),
                job_value.get("status").and_then(serde_json::Value::as_str).unwrap_or(""),
            );
            if line != last {
                log::info!("export: {line}");
                last = line;
            }
            if job_value.get("finished").and_then(serde_json::Value::as_bool).unwrap_or(false) {
                log::info!("export: job {job} finished: {}", job_value.get("result").cloned().unwrap_or_default());
                return;
            }
            if std::time::Instant::now() > deadline {
                log::warn!("export: giving up on job {job}");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1000));
        }
    });
}

/// Add the device's CJK font to egui's *current* fallback chain: the bundled fonts have no CJK
/// glyphs, so Chinese/Japanese text would render as tofu. This must run **after** FilmCraft's theme
/// install (which replaces the fonts), and it edits the current definitions rather than starting
/// from the defaults, so the theme's own fonts stay.
fn install_system_font(ctx: &egui::Context) {
    const CANDIDATES: &[&str] = &["/system/fonts/NotoSansCJK-Regular.ttc", "/system/fonts/NotoSerifCJK-Regular.ttc", "/system/fonts/DroidSansFallback.ttf"];
    for path in CANDIDATES {
        match std::fs::read(path) {
            Ok(bytes) => {
                let mut fonts = ctx.fonts(|f| f.definitions().clone());
                fonts.font_data.insert("system-cjk".to_owned(), std::sync::Arc::new(egui::FontData::from_owned(bytes)));
                for family in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                    fonts.families.entry(family).or_default().push("system-cjk".to_owned());
                }
                ctx.set_fonts(fonts);
                log::info!("filmcraft-android: loaded system CJK font {path}");
                return;
            }
            Err(e) => log::warn!("filmcraft-android: font {path} unavailable: {e}"),
        }
    }
    log::warn!("filmcraft-android: no system CJK font found; CJK text will be tofu");
}

/// A phone-sized workspace: the program monitor on top and the timeline below, with the project
/// bin and tools as tabs of the same dock. The desktop presets are built for ≥900×560 pt while a
/// phone gives ~411×914 pt, so the shell installs this layout with the
/// app's own workspace API instead of rearranging any panel code.
fn phone_workspace() -> WorkspacePrefs {
    use filmcraft_ui_egui::dock::{DockNode, PanelKind, SavedWorkspace, SplitSize};

    let monitor = DockNode::Tabs { panels: vec![PanelKind::Program], active: 0 };
    let lower = DockNode::Tabs { panels: vec![PanelKind::Timeline, PanelKind::Project, PanelKind::Tools], active: 0 };
    let layout = DockNode::Split { vertical: true, size: SplitSize::Ratio(0.42), a: Box::new(monitor), b: Box::new(lower) };
    WorkspacePrefs {
        saved: vec![SavedWorkspace {
            name: "Phone".to_owned(),
            layout: serde_json::to_value(&layout).unwrap_or_default(),
            // No detached windows on a phone: everything lives in the one dock tree.
            floating: serde_json::Value::Null,
        }],
        current: "Phone".to_owned(),
    }
}

/// The path an export or "Save As" should write to: a sanitized file name inside the app's export
/// directory (the app writes with plain file APIs, so a real path is what it needs).
fn export_path(dir: &std::path::Path, name: &str) -> String {
    let leaf = name.rsplit(['/', '\\']).next().unwrap_or(name);
    let leaf = if leaf.is_empty() { "export" } else { leaf };
    let path = dir.join(leaf);
    log::info!("filmcraft-android: save dialog → {}", path.display());
    path.to_string_lossy().into_owned()
}

/// Ask where the export should land, wait for the app to finish writing the scratch file, then
/// publish it there. Nothing is lost when the user cancels: the file stays in the export directory.
fn spawn_publish(activity: usize, scratch: String) {
    std::thread::spawn(move || {
        let title = scratch.rsplit('/').next().unwrap_or("export").to_owned();
        if !saf::pick_save(activity, &title, std::time::Duration::from_secs(300)) {
            log::info!("SAF: no export destination chosen; the file stays at {scratch}");
            return;
        }
        // The app writes the file after the dialog: wait until it exists and its size has stopped
        // changing for a moment.
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(900);
        let (mut size, mut stable_since) = (0u64, std::time::Instant::now());
        loop {
            let now = std::fs::metadata(&scratch).map(|m| m.len()).unwrap_or(0);
            if now != size {
                size = now;
                stable_since = std::time::Instant::now();
            } else if size > 0 && stable_since.elapsed() > std::time::Duration::from_millis(1500) {
                break;
            }
            if std::time::Instant::now() > deadline {
                log::warn!("SAF: giving up on publishing {scratch} (nothing was written)");
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(400));
        }
        if saf::publish(activity, &scratch) {
            log::info!("SAF: published {scratch} ({size} bytes) to the chosen destination");
        } else {
            log::warn!("SAF: publishing {scratch} failed; the file stays in the export directory");
        }
    });
}

/// Start the SAF picker for one of FilmCraft's own file dialogs.
///
/// `HostHooks` are synchronous (they answer "which files?" inside the frame), and a picker cannot
/// be answered synchronously without freezing the UI thread, so the hook starts this background
/// task and answers "nothing" — the chosen files reach the engine a moment later over the control
/// channel, which runs the *normal* command (`file.import` / `file.open`) on the UI thread.
fn spawn_pick(control: Sender<ControlRequest>, activity: usize, data_dir: Option<PathBuf>, method: &'static str, key: &'static str) {
    let Some(data_dir) = data_dir else {
        log::warn!("SAF: no data dir; cannot open the picker");
        return;
    };
    std::thread::spawn(move || {
        let paths = saf::pick_media(activity, &data_dir, std::time::Duration::from_secs(300));
        if paths.is_empty() {
            log::info!("SAF: nothing picked");
            return;
        }
        // `engine.execute` runs the *engine* command of the same name: `menus::invoke` only opens
        // the file dialog when no paths are given, so this is exactly what File ▸ Import does once
        // the files are known — the desktop import path, with the paths coming from SAF.
        let params = if key == "path" {
            serde_json::json!({"command": method, "params": { key: paths[0] }})
        } else {
            serde_json::json!({"command": method, "params": { key: paths }})
        };
        let (request, reply) = ControlRequest::new("engine.execute", params);
        match control.send(request) {
            Ok(()) => log::info!("SAF: sent {method} for {} file(s)", paths.len()),
            Err(_) => {
                log::warn!("SAF: control channel is closed");
                return;
            }
        }
        match reply.recv_timeout(std::time::Duration::from_secs(60)) {
            Ok(value) => log::info!("SAF: {method} replied {value}"),
            Err(e) => log::warn!("SAF: no reply for {method}: {e}"),
        }
    });
}

#[allow(unsafe_code)]
#[unsafe(no_mangle)]
fn android_main(app: AndroidApp) {
    android_logger::init_once(android_logger::Config::default().with_max_level(log::LevelFilter::Info));
    log::info!("filmcraft-android: android_main entered");
    // Hardware video decoding (MediaCodec). Streams it does not take fall through to FilmCraft's
    // own decoders, so registering never makes a file undecodable.
    let hardware = filmcraft_platform::register();
    log::info!("filmcraft-android: hardware decoding: {hardware:?}");
    // Decode the embedded clips through the registered factories: logcat evidence that the Android
    // backend works end to end (decoder name, picture count, hardware counters).
    std::thread::spawn(|| log::info!("filmcraft-android: {}", selftest::run()));
    // Application-private storage; used for auto-save, crash logs and preferences.
    let data_dir = app.internal_data_path();
    // Exports and "Save As" write here (see the save hooks below).
    let external_dir = app.external_data_path().map(|dir| dir.join("exports"));
    // The command palette writes its explicit exports to the same directory.
    let shell_exports = external_dir.clone();
    // The Java activity object: `ndk_context` only hands out the Application, which cannot start
    // the document picker for a result.
    let activity = app.activity_as_ptr() as usize;
    let options = eframe::NativeOptions { android_app: Some(app), ..Default::default() };
    // The control channel: SAF results are turned into `file.import` / `file.open` requests from a
    // background thread and `FilmcraftApp` executes them on the UI thread, exactly like the desktop
    // control server.
    let (control, requests) = std::sync::mpsc::channel::<ControlRequest>();
    let started = eframe::run_native(
        "FilmCraft",
        options,
        Box::new(move |cc| {
            let mut session = Session::default();
            if let Some(dir) = data_dir.clone() {
                if let Err(e) = session.start_autosave(AutosaveConfig::new(dir)) {
                    log::warn!("filmcraft-android: auto-save unavailable: {e}");
                }
            }
            let mut app = FilmcraftApp::new(session).with_control(requests);
            if let Some(rs) = cc.wgpu_render_state.clone() {
                app.set_wgpu(rs);
            }
            // A layout that fits a phone; the desktop presets do not (see `phone_workspace`).
            if let Err(e) = app.set_workspaces(phone_workspace()) {
                log::warn!("filmcraft-android: could not install the phone workspace: {e}");
            }
            // The app's own file dialogs (File ▸ Import, Import ▸ Media File, Open Project, …) go
            // through these hooks: the picker opens asynchronously and the picked files come back
            // as regular commands, so the desktop import path is what actually runs.
            {
                let (control, data_dir, activity) = (control.clone(), data_dir.clone(), activity);
                app.hooks.pick_files = Some(Box::new(move |_extensions: &[&str]| {
                    spawn_pick(control.clone(), activity, data_dir.clone(), "file.import", "paths");
                    Vec::new()
                }));
            }
            {
                let (control, data_dir, activity) = (control.clone(), data_dir.clone(), activity);
                app.hooks.pick_open_project = Some(Box::new(move || {
                    spawn_pick(control.clone(), activity, data_dir.clone(), "file.open", "path");
                    None
                }));
            }
            {
                let (control, data_dir, activity) = (control.clone(), data_dir.clone(), activity);
                app.hooks.pick_open_file = Some(Box::new(move |_filter: &str, _extensions: &[&str]| {
                    spawn_pick(control.clone(), activity, data_dir.clone(), "file.import", "paths");
                    None
                }));
            }
            // Save/export dialogs: the app writes to a plain path, so the hook answers with a
            // scratch file in the app's export directory and a watcher publishes the finished file
            // to wherever the user points ACTION_CREATE_DOCUMENT.
            if let Some(exports) = external_dir {
                if let Err(e) = std::fs::create_dir_all(&exports) {
                    log::warn!("filmcraft-android: cannot create {}: {e}", exports.display());
                }
                log::info!("filmcraft-android: export scratch directory {}", exports.display());
                {
                    let dir = exports.clone();
                    app.hooks.pick_save = Some(Box::new(move |name: &str| {
                        let scratch = export_path(&dir, name);
                        spawn_publish(activity, scratch.clone());
                        Some(scratch)
                    }));
                }
                {
                    let dir = exports.clone();
                    app.hooks.pick_save_as = Some(Box::new(move |_filter: &str, _extensions: &[&str], name: &str| {
                        let scratch = export_path(&dir, name);
                        spawn_publish(activity, scratch.clone());
                        Some(scratch)
                    }));
                }
                {
                    // Folder pickers (proxy and Project Manager destinations) stay in the app dir:
                    // SAF's tree picker returns a document tree the engine cannot write to by path.
                    let dir = exports.clone();
                    app.hooks.pick_folder = Some(Box::new(move || Some(dir.to_string_lossy().into_owned())));
                }
            }
            Ok(Box::new(TouchShell { inner: app, tuned: false, font_done: false, control, palette: false, press: None, exports: shell_exports }))
        }),
    );
    if let Err(e) = started {
        log::error!("filmcraft-android: eframe exited: {e}");
    }
}

//! FilmCraft in the browser.
//!
//! The same engine and egui UI as the desktop app, compiled to `wasm32-unknown-unknown` and run
//! by eframe's web runner on WebGPU (WebGL2 fallback). What the desktop shell gets from the OS,
//! this crate gets from the browser:
//!
//! | Desktop | Web ([module]) |
//! |---|---|
//! | file system ([`filmcraft_engine::FsServices`]) | virtual file table of `File`/`Blob`s with chunked range reads, downloads ([`fs`]) |
//! | file dialogs (rfd) | `<input type=file>` / File System Access pickers, drag-and-drop ([`import`]) |
//! | auto-save + recovery journal (data dir) | OPFS snapshot + media copies ([`recovery`], [`opfs`]) |
//! | cpal audio output | WebAudio `AudioWorklet`, played-frame clock ([`audio`]) |
//! | VideoToolbox… | WebCodecs `VideoDecoder` ([`webcodecs`]) |
//! | frame worker threads | cooperative frame queue pumped between UI frames (`FrameServer::pump`) |
//! | TCP control channel / MCP | `window.filmcraft` JavaScript API ([`api`]) |
//!
//! Everything here is `wasm32`-only; on other targets the crate is empty.
#![cfg(target_arch = "wasm32")]
#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

pub mod api;
pub mod audio;
pub mod download_pacing;
pub mod fs;
pub mod import;
pub mod opfs;
pub mod recovery;
pub mod recovery_policy;
pub mod webcodecs;

use std::cell::RefCell;
use std::sync::Arc;

use filmcraft_engine::Session;
use filmcraft_ui_egui::{FilmcraftApp, i18n};
use serde_json::json;
use wasm_bindgen::prelude::*;

/// Work for the UI thread with access to the app (posted by async tasks and the JS API).
pub type Action = Box<dyn FnOnce(&mut WebApp, &egui::Context)>;

thread_local! {
    static ACTIONS: RefCell<Vec<Action>> = const { RefCell::new(Vec::new()) };
    static CTX: RefCell<Option<egui::Context>> = const { RefCell::new(None) };
    static INFO: RefCell<serde_json::Value> = const { RefCell::new(serde_json::Value::Null) };
}

/// Run `f` on the UI thread at the next frame.
pub fn post(f: impl FnOnce(&mut WebApp, &egui::Context) + 'static) {
    ACTIONS.with(|a| a.borrow_mut().push(Box::new(f)));
    repaint();
}

/// Ask for a UI frame.
pub fn repaint() {
    CTX.with(|c| {
        if let Some(c) = c.borrow().as_ref() {
            c.request_repaint();
        }
    });
}

/// Give a host that builds its own [`WebApp`] the egui context that [`post`] and [`repaint`] wake.
/// [`start`] calls it; an embedding app that runs `WebApp` in its own eframe runner calls it once
/// from its app creator, or posted actions and JS API requests wait for the next input event.
pub fn set_context(ctx: egui::Context) {
    CTX.with(|c| *c.borrow_mut() = Some(ctx));
}

/// Environment facts reported by `filmcraft.info()` (backend, isolation, codecs…).
pub fn set_info(key: &str, v: serde_json::Value) {
    INFO.with(|i| {
        let mut i = i.borrow_mut();
        if !i.is_object() {
            *i = json!({});
        }
        i[key] = v;
    });
}

pub fn info() -> serde_json::Value {
    INFO.with(|i| i.borrow().clone())
}

/// A saved browser snapshot that could not be opened. Keep it available for download before
/// starting new recovery writes, which would replace the only copy in OPFS.
enum RecoveryHoldReason {
    OpenFailed(String),
    SkippedByLaunchOption,
    StorageReadFailed(String),
}

enum RecoveryDownload {
    Bytes(Arc<[u8]>),
    Blob(web_sys::Blob),
}

struct RejectedRecovery {
    name: String,
    download: RecoveryDownload,
    reason: RecoveryHoldReason,
    download_requested: bool,
    download_error: Option<String>,
}

enum StartupRecovery {
    Open(String, Vec<u8>),
    Skipped(String, web_sys::Blob),
    Unreadable(String, web_sys::Blob),
    Unavailable(String),
}

/// The eframe app: the shared FilmCraft UI plus the web host's per-frame duties.
pub struct WebApp {
    pub app: FilmcraftApp,
    pub audio: Option<audio::Handle>,
    pub autosave: recovery::Autosave,
    rejected_recovery: Option<RejectedRecovery>,
    recovery_unavailable: Option<String>,
    active_recovery_warning: Option<String>,
}

impl eframe::App for WebApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, raw_input);
    }

    fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        i18n::set_current(self.app.ui.language);
        let actions = ACTIONS.with(|a| std::mem::take(&mut *a.borrow_mut()));
        for f in actions {
            f(self, ctx);
        }
        self.app.logic(ctx, frame);
        api::poll_replies();
        // exports advance between frames (no threads)
        if self.app.session.pump_jobs(std::time::Duration::from_millis(30)) {
            ctx.request_repaint();
        }
        if let Some(a) = &self.audio {
            a.pump();
        }
        if self.rejected_recovery.is_none() && self.recovery_unavailable.is_none() {
            if let Some(error) = self.autosave.tick(&self.app.session, ctx) {
                if recovery_policy::should_show_warning(&self.app.ui.status, self.active_recovery_warning.as_deref()) {
                    self.app.keep_status_visible(error.clone(), ctx.input(|i| i.time));
                }
                self.active_recovery_warning = Some(error);
            } else if let Some(previous_warning) = self.active_recovery_warning.take()
                && self.app.ui.status == previous_warning
            {
                self.app.ui.status.clear();
            }
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        self.app.ui(ui, frame);
        // These host dialogs use FilmCraft's named fonts after the app installs them.
        if !self.app.host_overlays_ready() {
            return;
        }
        if !self.app.ui.show_status_bar
            && let Some(warning) = &self.active_recovery_warning
        {
            egui::Window::new(i18n::t("Recovery paused"))
                .collapsible(false)
                .resizable(false)
                .default_width(420.0)
                .anchor(egui::Align2::CENTER_BOTTOM, egui::vec2(0.0, -16.0))
                .show(ui.ctx(), |ui| {
                    ui.label(warning);
                });
        }
        if let Some(error) = &self.recovery_unavailable {
            egui::Window::new(i18n::t("Recovery unavailable"))
                .collapsible(false)
                .resizable(false)
                .default_width(420.0)
                .show(ui.ctx(), |ui| {
                    ui.label(i18n::fmt(
                        i18n::t("Could not read browser recovery storage: {error}. New recovery snapshots are paused. Save current work manually, then reload this page to retry."),
                        &[("error", error)],
                    ));
                });
        }
        if let Some(rejected) = self.rejected_recovery.as_mut() {
            let mut continue_after_download = false;
            egui::Window::new(i18n::t("Recover Unsaved Changes")).collapsible(false).default_width(420.0).show(ui.ctx(), |ui| {
                match &rejected.reason {
                    RecoveryHoldReason::OpenFailed(error) => ui
                        .label(i18n::fmt(i18n::t("Could not open the browser recovery file “{name}”: {error}"), &[("name", &rejected.name), ("error", error)])),
                    RecoveryHoldReason::SkippedByLaunchOption => {
                        ui.label(i18n::fmt(i18n::t("The browser recovery file “{name}” was skipped by a launch option."), &[("name", &rejected.name)]))
                    }
                    RecoveryHoldReason::StorageReadFailed(error) => {
                        ui.label(i18n::fmt(i18n::t("Could not inspect the browser recovery file: {error}"), &[("error", error)]))
                    }
                };
                ui.label(i18n::t("The file is still in this browser. New recovery snapshots are paused until you preserve it."));
                let download_label = i18n::t("Download recovery file");
                let download = ui.button(download_label);
                self.app.auto.add_response("recovery.download", &download, download_label);
                if download.clicked() {
                    let result = match &rejected.download {
                        RecoveryDownload::Bytes(bytes) => fs::download_bytes_now(bytes, &rejected.name),
                        RecoveryDownload::Blob(blob) => fs::download_blob_now(blob, &rejected.name),
                    };
                    match result {
                        Ok(()) => {
                            rejected.download_requested = true;
                            rejected.download_error = None;
                        }
                        Err(error) => rejected.download_error = Some(format!("{error:?}")),
                    }
                }
                if rejected.download_requested {
                    ui.label(i18n::t("After the file has been saved, continue. Editing a new project may replace the browser recovery copy."));
                    let continue_label = i18n::t("I saved the recovery file");
                    let continue_button = ui.button(continue_label);
                    self.app.auto.add_response("recovery.continue", &continue_button, continue_label);
                    if continue_button.clicked() {
                        continue_after_download = true;
                    }
                }
                if let Some(error) = &rejected.download_error {
                    ui.label(i18n::fmt(i18n::t("Could not download recovery file: {error}"), &[("error", error)]));
                }
            });
            if continue_after_download {
                self.rejected_recovery = None;
            }
        }
    }
}

fn query_flag(name: &str) -> bool {
    web_sys::window().and_then(|w| w.location().search().ok()).and_then(|s| web_sys::UrlSearchParams::new_with_str(&s).ok()).is_some_and(|p| p.has(name))
}

/// Entry point (called by the page's bootstrap script once the wasm module is instantiated).
#[wasm_bindgen]
pub async fn start(canvas_id: String) -> Result<(), JsValue> {
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    let t0 = web_time::Instant::now();
    let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let canvas: web_sys::HtmlCanvasElement = doc.get_element_by_id(&canvas_id).ok_or("no canvas")?.dyn_into()?;

    let isolated = js_sys::Reflect::get(&js_sys::global(), &"crossOriginIsolated".into()).ok().and_then(|v| v.as_bool()).unwrap_or(false);
    // Frame rendering runs cooperatively on this thread. wasm threads need a cross-origin
    // isolated page *and* a build with atomics (nightly `build-std`); this build has none.
    set_info("crossOriginIsolated", json!(isolated));
    set_info("threads", json!(false));
    set_info("frameWorkers", json!("cooperative"));
    set_info("opfs", json!(opfs::available()));

    // Media kept in OPFS by earlier sessions come back under their paths (recovery).
    let skip_recovery = query_flag("fresh") || query_flag("norecover");
    let restored = if opfs::available() && !query_flag("fresh") { recovery::restore_media().await } else { 0 };
    // A skipped dirty snapshot stays in OPFS as a Blob; do not decode it or load its bytes.
    let recovered_result = match recovery_policy::startup_recovery_mode(skip_recovery) {
        recovery_policy::StartupRecoveryMode::Open => {
            recovery::load_snapshot().await.map(|snapshot| snapshot.map(|(name, bytes)| StartupRecovery::Open(name, bytes)))
        }
        recovery_policy::StartupRecoveryMode::HoldForDownload => {
            recovery::load_snapshot_blob().await.map(|snapshot| snapshot.map(|(name, blob)| StartupRecovery::Skipped(name, blob)))
        }
    };
    let recovered = match recovered_result {
        Ok(recovered) => recovered,
        Err(error) => match recovery::load_raw_snapshot_blob().await {
            Ok(Some(blob)) => Some(StartupRecovery::Unreadable(error, blob)),
            Ok(None) => Some(StartupRecovery::Unavailable(error)),
            Err(fallback_error) => Some(StartupRecovery::Unavailable(format!("{error}; {fallback_error}"))),
        },
    };
    webcodecs::probe().await;

    api::install()?;
    import::install_drop_handlers()?;

    let demo = !query_flag("empty");
    let mut web_options = eframe::WebOptions::default();
    // `?webgl`: WebGL2 only. The page reloads with it when starting on WebGPU fails (an adapter
    // whose device can't be created, a driver the browser rejects…); see `index.html`.
    if query_flag("webgl")
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut web_options.wgpu_options.wgpu_setup
    {
        c.instance_descriptor.backends = eframe::wgpu::Backends::GL;
    }
    let runner = eframe::WebRunner::new();
    runner
        .start(
            canvas,
            web_options,
            Box::new(move |cc| {
                set_context(cc.egui_ctx.clone());
                let ctx = cc.egui_ctx.clone();
                fs::set_on_data(move || ctx.request_repaint());
                fs::set_on_write(|path, data| {
                    if let Err(e) = fs::download(path, data) {
                        log::warn!("download {path}: {e:?}");
                    }
                });
                webcodecs::install();
                let mut session = Session::new(Arc::new(fs::WebServices));
                let mut toast = None;
                let mut rejected_recovery = None;
                let mut recovery_unavailable = None;
                match recovered {
                    Some(StartupRecovery::Skipped(name, blob)) => {
                        rejected_recovery = Some(RejectedRecovery {
                            name,
                            download: RecoveryDownload::Blob(blob),
                            reason: RecoveryHoldReason::SkippedByLaunchOption,
                            download_requested: false,
                            download_error: None,
                        });
                        if demo {
                            let _ = session.execute("file.openDemoProject", json!({}));
                        }
                    }
                    Some(StartupRecovery::Open(name, bytes)) => {
                        let path = format!("/recovered/{name}");
                        let bytes: Arc<[u8]> = bytes.into();
                        fs::put_shared(&path, bytes.clone());
                        match session.execute("file.open", json!({"path": path})) {
                            Ok(_) => {
                                // the recovered changes are unsaved
                                session.saved_revision = 0;
                                toast = Some(format!("Recovered unsaved changes to {name}"));
                            }
                            Err(e) => {
                                log::warn!("recovery failed: {e}");
                                rejected_recovery = Some(RejectedRecovery {
                                    name,
                                    download: RecoveryDownload::Bytes(bytes),
                                    reason: RecoveryHoldReason::OpenFailed(e.to_string()),
                                    download_requested: false,
                                    download_error: None,
                                });
                            }
                        }
                    }
                    Some(StartupRecovery::Unreadable(error, blob)) => {
                        rejected_recovery = Some(RejectedRecovery {
                            name: "Recovered.fcproj".into(),
                            download: RecoveryDownload::Blob(blob),
                            reason: RecoveryHoldReason::StorageReadFailed(error),
                            download_requested: false,
                            download_error: None,
                        });
                        if demo {
                            let _ = session.execute("file.openDemoProject", json!({}));
                        }
                    }
                    Some(StartupRecovery::Unavailable(error)) => {
                        log::warn!("browser recovery unavailable: {error}");
                        recovery_unavailable = Some(error);
                        if demo {
                            let _ = session.execute("file.openDemoProject", json!({}));
                        }
                    }
                    None if demo => {
                        let _ = session.execute("file.openDemoProject", json!({}));
                    }
                    None => {}
                }
                let mut app = FilmcraftApp::new(session);
                import::install_hooks(&mut app);
                // Settings ▸ General ▸ Interface Language ▸ System Language (#218): the browser's languages.
                app.hooks.system_languages =
                    Some(Box::new(|| web_sys::window().map(|w| w.navigator().languages().iter().filter_map(|v| v.as_string()).collect()).unwrap_or_default()));
                let (tx, rx) = std::sync::mpsc::channel();
                api::set_sender(tx);
                app = app.with_control(rx);
                if let Some(t) = toast {
                    app.status(t);
                }
                let backend = cc.wgpu_render_state.as_ref().map(|rs| rs.adapter.get_info().backend);
                set_info("backend", json!(backend.map(|b| format!("{b:?}"))));
                // The GPU compositor needs float render targets everywhere: WebGPU only (WebGL2
                // composites on the CPU).
                if let Some(rs) = cc.wgpu_render_state.clone()
                    && backend == Some(eframe::wgpu::Backend::BrowserWebGpu)
                    && !query_flag("cpu")
                {
                    app.set_wgpu(rs);
                    set_info("compositor", json!("gpu"));
                } else {
                    set_info("compositor", json!("cpu"));
                }
                let audio = audio::Handle::new();
                if let Some(a) = &audio {
                    app.audio = Some(Box::new(a.clone()));
                }
                set_info("audio", json!(audio.is_some()));
                set_info("restoredMedia", json!(restored));
                Ok(Box::new(WebApp {
                    app,
                    audio,
                    autosave: recovery::Autosave::default(),
                    rejected_recovery,
                    recovery_unavailable,
                    active_recovery_warning: None,
                }))
            }),
        )
        .await?;
    set_info("startupMs", json!(t0.elapsed().as_secs_f64() * 1000.0));
    // keep the runner alive for the page's lifetime
    std::mem::forget(runner);
    if let Some(el) = doc.get_element_by_id("loading") {
        el.remove();
    }
    Ok(())
}

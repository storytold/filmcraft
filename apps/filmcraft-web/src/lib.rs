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
pub mod host;
pub mod hostmode;
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

/// An existing browser snapshot held for preservation before new recovery writes may replace it.
enum RecoveryHoldReason {
    OpenFailed(String),
    SkippedByLaunchOption,
    StorageReadFailed(String),
    StorageUnavailable,
}

fn recovery_storage_message(error: &str, media_entry_limit: bool) -> String {
    if media_entry_limit {
        let limit = opfs::MAX_MEDIA_DIRECTORY_ENTRIES.to_string();
        i18n::fmt(
            i18n::t(
                "Browser recovery exceeds {limit} media entries. Automatic snapshots and copies of imported media remain paused. Save current work manually and reduce retained media before reloading.",
            ),
            &[("limit", limit.as_str())],
        )
    } else {
        i18n::fmt(
            i18n::t(
                "Could not read browser recovery storage: {error}. New recovery snapshots are paused. Save current work manually, then reload this page to retry.",
            ),
            &[("error", error)],
        )
    }
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
    browser_recovery_initialized: bool,
    media_entry_limit: bool,
    /// The host bridge in host mode (`?host=parent` / `data-host="page"`): written files go to
    /// the host instead of downloads, it hears about unsaved changes, and it opens the files it
    /// sends.
    pub host: Option<host::Bridge>,
}

impl WebApp {
    /// Wrap the shared UI for a custom eframe runner, with no audio, host bridge or automatic browser recovery.
    /// Call [`set_context`] from the runner's app creator so posted actions can wake it.
    pub fn new(app: FilmcraftApp) -> Self {
        Self {
            app,
            audio: None,
            autosave: recovery::Autosave::default(),
            rejected_recovery: None,
            recovery_unavailable: None,
            browser_recovery_initialized: false,
            media_entry_limit: false,
            host: None,
        }
    }
}

/// Recovery diagnostics are untrusted text; keep their actions reachable within the viewport.
fn recovery_window(
    title: &str,
    ctx: &egui::Context,
    configure: impl FnOnce(egui::Window<'static>) -> egui::Window<'static>,
    contents: impl FnOnce(&mut egui::Ui),
) -> Option<egui::Response> {
    let max_height = (ctx.content_rect().height() - 80.0).max(1.0);
    configure(
        egui::Window::new(title.to_owned())
            .collapsible(false)
            .default_width(420.0)
            .max_width((ctx.content_rect().width() - 32.0).max(1.0))
            .max_height(max_height),
    )
    .show(ctx, |ui| {
        egui::ScrollArea::vertical().max_height(max_height).auto_shrink([false, true]).show(ui, contents);
    })
    .map(|window| window.response)
}

impl eframe::App for WebApp {
    fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        self.app.raw_input_hook(ctx, raw_input);
    }

    /// Hidden logic pass: host actions, app logic, API replies, jobs, host saves and dirty reports, audio, autosave, then IME reset; it does not draw UI.
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        i18n::set_current(self.app.ui.language);
        let actions = ACTIONS.with(|a| std::mem::take(&mut *a.borrow_mut()));
        for f in actions {
            f(self, ctx);
        }
        self.app.logic_inner(ctx);
        // exports advance between frames (no threads)
        if self.app.session.pump_jobs(std::time::Duration::from_millis(30)) {
            ctx.request_repaint();
        }
        // In host mode: files written this frame (commands, exports) go to the host, and it
        // hears when unsaved changes appear or go away.
        if let Some(host) = &self.host {
            host.sync(&self.app.session);
        }
        if let Some(a) = &self.audio {
            a.pump();
        }
        if self.browser_recovery_initialized && self.host.is_none() && self.rejected_recovery.is_none() && self.recovery_unavailable.is_none() {
            self.autosave.tick(&self.app.session, ctx);
        }
        self.app.finish_ime_reset_logic(ctx);
    }

    /// Draw recovery holds and warnings only after the shared UI has installed its fonts.
    fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
        let Self { app, rejected_recovery, recovery_unavailable, media_entry_limit, .. } = self;
        app.ui_with_host_overlays(ui, frame, |app, ui| {
            // These host dialogs use FilmCraft's named fonts after the app installs them.
            if !app.host_overlays_ready() {
                return;
            }
            if let Some(error) = recovery_unavailable.as_ref()
                && rejected_recovery.is_none()
            {
                let window = recovery_window(
                    i18n::t("Recovery unavailable"),
                    ui.ctx(),
                    |window| window.resizable(false),
                    |ui| {
                        let message = recovery_storage_message(error, *media_entry_limit);
                        let response = ui.label(&message);
                        app.auto.add_response("browserRecovery.unavailable", &response, &message);
                    },
                );
                if let Some(response) = window {
                    app.auto.add_response("browserRecovery.unavailableWindow", &response, i18n::t("Recovery unavailable"));
                }
            }
            if let Some(rejected) = rejected_recovery.as_mut() {
                let mut continue_after_download = false;
                let window = recovery_window(
                    i18n::t("Recover Unsaved Changes"),
                    ui.ctx(),
                    |window| window,
                    |ui| {
                        let message = match &rejected.reason {
                            RecoveryHoldReason::OpenFailed(error) => {
                                i18n::fmt(i18n::t("Could not open the browser recovery file “{name}”: {error}"), &[("name", &rejected.name), ("error", error)])
                            }
                            RecoveryHoldReason::SkippedByLaunchOption => {
                                i18n::fmt(i18n::t("The browser recovery file “{name}” was skipped by a launch option."), &[("name", &rejected.name)])
                            }
                            RecoveryHoldReason::StorageReadFailed(error) => {
                                i18n::fmt(i18n::t("Could not inspect the browser recovery file: {error}"), &[("error", error)])
                            }
                            RecoveryHoldReason::StorageUnavailable => {
                                let error = recovery_unavailable.as_deref().unwrap_or(i18n::t("Recovery unavailable"));
                                recovery_storage_message(error, *media_entry_limit)
                            }
                        };
                        let response = ui.label(&message);
                        app.auto.add_response("recovery.reason", &response, &message);
                        if let Some(error) = recovery_unavailable.as_deref()
                            && !matches!(&rejected.reason, RecoveryHoldReason::StorageUnavailable)
                        {
                            ui.label(recovery_storage_message(error, *media_entry_limit));
                        }
                        if recovery_unavailable.is_none() {
                            ui.label(i18n::t("The file is still in this browser. New recovery snapshots are paused until you preserve it."));
                        }
                        let download_label = i18n::t("Download recovery file");
                        let download = ui.button(download_label);
                        app.auto.add_response("recovery.download", &download, download_label);
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
                            if recovery_unavailable.is_none() {
                                ui.label(i18n::t("After the file has been saved, continue. Editing a new project may replace the browser recovery copy."));
                            }
                            let continue_label = i18n::t("I saved the recovery file");
                            let continue_button = ui.button(continue_label);
                            app.auto.add_response("recovery.continue", &continue_button, continue_label);
                            if continue_button.clicked() {
                                continue_after_download = true;
                            }
                        }
                        if let Some(error) = &rejected.download_error {
                            ui.label(i18n::fmt(i18n::t("Could not download recovery file: {error}"), &[("error", error)]));
                        }
                    },
                );
                if let Some(response) = window {
                    app.auto.add_response("browserRecovery.holdWindow", &response, i18n::t("Recover Unsaved Changes"));
                }
                if continue_after_download {
                    *rejected_recovery = None;
                }
            }
        });
    }
}

fn query_flag(name: &str) -> bool {
    web_sys::window().and_then(|w| w.location().search().ok()).and_then(|s| web_sys::UrlSearchParams::new_with_str(&s).ok()).is_some_and(|p| p.has(name))
}

/// Entry point (called by the page's bootstrap script once the wasm module is instantiated).
#[wasm_bindgen]
pub async fn start(canvas_id: String) -> Result<(), JsValue> {
    recovery::set_media_names_ready(false);
    eframe::WebLogger::init(log::LevelFilter::Info).ok();
    let t0 = web_time::Instant::now();
    let doc = web_sys::window().and_then(|w| w.document()).ok_or("no document")?;
    let canvas: web_sys::HtmlCanvasElement = doc.get_element_by_id(&canvas_id).ok_or("no canvas")?.dyn_into()?;

    // Host modes (`craft-web-host`): `?host=parent`, a same-origin page framing FilmCraft in an
    // `<iframe>`, opens files in it and stores what it saves (`host`); unframed there is no parent,
    // so that URL on its own stays the normal app with downloads. `data-host="page"` on the canvas
    // opts a page in instead: it runs FilmCraft in its own document and talks over DOM
    // `CustomEvent`s on the canvas. In host mode the host sends the document to edit, so no demo
    // project opens and nothing from an earlier visit is recovered.
    let host = host::Bridge::connect(&canvas);
    let in_host_mode = host.is_some();

    let isolated = js_sys::Reflect::get(&js_sys::global(), &"crossOriginIsolated".into()).ok().and_then(|v| v.as_bool()).unwrap_or(false);
    // Frame rendering runs cooperatively on this thread. wasm threads need a cross-origin
    // isolated page *and* a build with atomics (nightly `build-std`); this build has none.
    set_info("crossOriginIsolated", json!(isolated));
    set_info("threads", json!(false));
    set_info("frameWorkers", json!("cooperative"));
    set_info("opfs", json!(opfs::available()));

    // Media kept in OPFS by earlier sessions come back under their paths (recovery).
    let skip_recovery = query_flag("fresh") || query_flag("norecover");
    let restored = if !in_host_mode && opfs::available() && !query_flag("fresh") { recovery::restore_media().await } else { 0 };
    // Skipped or unreadable media still own the names referenced by the recovery snapshot.
    let media_protection_error = if in_host_mode {
        None
    } else if opfs::available() {
        recovery::reserve_media_names().await.err()
    } else {
        Some("OPFS not available".into())
    };
    let media_names_ready = !in_host_mode && opfs::available() && media_protection_error.is_none();
    let media_protection_failed = media_protection_error.is_some();
    let media_entry_limit = media_protection_error.as_deref() == Some(opfs::MEDIA_ENTRY_LIMIT_ERROR);
    // A skipped dirty snapshot stays in OPFS as a Blob; do not decode it or load its bytes.
    let recovered = if in_host_mode {
        None
    } else {
        let recovered_result = match recovery_policy::startup_recovery_mode(skip_recovery || media_protection_failed) {
            recovery_policy::StartupRecoveryMode::Open => {
                recovery::load_snapshot().await.map(|snapshot| snapshot.map(|(name, bytes)| StartupRecovery::Open(name, bytes)))
            }
            recovery_policy::StartupRecoveryMode::HoldForDownload => {
                recovery::load_snapshot_blob().await.map(|snapshot| snapshot.map(|(name, blob)| StartupRecovery::Skipped(name, blob)))
            }
        };
        let recovered = match recovered_result {
            Ok(recovered) => recovered,
            Err(error) => match recovery_policy::recovery_read_fallback(recovery::load_raw_snapshot_blob().await) {
                recovery_policy::RecoveryReadFallback::Hold(blob) => Some(StartupRecovery::Unreadable(error, blob)),
                recovery_policy::RecoveryReadFallback::Proceed => {
                    log::warn!("browser recovery metadata ignored because no snapshot exists");
                    None
                }
                recovery_policy::RecoveryReadFallback::Unavailable(fallback_error) => Some(StartupRecovery::Unavailable(format!("{error}; {fallback_error}"))),
            },
        };
        recovered
    };
    webcodecs::probe().await;

    api::install()?;
    import::install_drop_handlers()?;

    let demo = !in_host_mode && !query_flag("empty");
    let mut web_options = eframe::WebOptions::default();
    // `?webgl`: WebGL2 only. The page reloads with it when starting on WebGPU fails (an adapter
    // whose device can't be created, a driver the browser rejects…); see `index.html`.
    if query_flag("webgl")
        && let eframe::egui_wgpu::WgpuSetup::CreateNew(c) = &mut web_options.wgpu_options.wgpu_setup
    {
        c.instance_descriptor.backends = eframe::wgpu::Backends::GL;
    }
    let runner = eframe::WebRunner::new();
    let host_for_app = host.clone();
    let started = runner
        .start(
            canvas,
            web_options,
            Box::new(move |cc| {
                filmcraft_ui_egui::install_ime_reset_callbacks(&cc.egui_ctx);
                set_context(cc.egui_ctx.clone());
                let ctx = cc.egui_ctx.clone();
                fs::set_on_data(move || ctx.request_repaint());
                // Host mode: written files (saved projects, exports, …) are handed to the host
                // instead of downloading, and it opens the files it sends. Otherwise the app
                // offers what it writes as a browser download.
                match &host_for_app {
                    Some(bridge) => bridge.install(),
                    None => {
                        fs::set_on_write(|path, data| {
                            if let Err(e) = fs::download(path, data) {
                                log::warn!("download {path}: {e:?}");
                            }
                        });
                    }
                }
                webcodecs::install();
                let mut session = Session::new(Arc::new(fs::WebServices));
                let mut toast = None;
                let mut rejected_recovery = None;
                let mut recovery_unavailable = media_protection_error;
                let mut media_entry_limit = media_entry_limit;
                match recovered {
                    Some(StartupRecovery::Skipped(name, blob)) => {
                        rejected_recovery = Some(RejectedRecovery {
                            name,
                            download: RecoveryDownload::Blob(blob),
                            reason: if media_protection_failed { RecoveryHoldReason::StorageUnavailable } else { RecoveryHoldReason::SkippedByLaunchOption },
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
                                log::warn!("browser recovery project could not be opened");
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
                        // Keep both diagnostics visible when snapshot storage also failed.
                        media_entry_limit = false;
                        log::warn!("browser recovery storage is unavailable");
                        recovery_unavailable = Some(match recovery_unavailable.take() {
                            Some(media_error) => format!("{media_error}; {error}"),
                            None => error,
                        });
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
                let mut web_app = WebApp::new(app);
                web_app.audio = audio;
                web_app.rejected_recovery = rejected_recovery;
                web_app.recovery_unavailable = recovery_unavailable;
                web_app.host = host_for_app;
                web_app.browser_recovery_initialized = !in_host_mode;
                web_app.media_entry_limit = media_entry_limit;
                recovery::set_media_names_ready(media_names_ready);
                Ok(Box::new(web_app))
            }),
        )
        .await;
    // Ready only once the runner is up: starting can still fail after the app is made.
    if let Some(bridge) = &host {
        match &started {
            Ok(()) => bridge.post_ready(),
            Err(e) => bridge.post_failed(&e.as_string().unwrap_or_else(|| format!("{e:?}"))),
        }
    }
    started?;
    set_info("startupMs", json!(t0.elapsed().as_secs_f64() * 1000.0));
    // keep the runner alive for the page's lifetime
    std::mem::forget(runner);
    if let Some(el) = doc.get_element_by_id("loading") {
        el.remove();
    }
    Ok(())
}

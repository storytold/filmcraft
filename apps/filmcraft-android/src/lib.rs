//! FilmCraft Android mobile runner.
//!
//! Compiles to a `cdylib` and runs inside an Android NativeActivity
//! using `android-activity`, `eframe` and `wgpu`.
//!
//! The file browser is also built on the host so its path handling can be tested without Android.

#![cfg_attr(not(test), deny(clippy::unwrap_used, clippy::expect_used, clippy::panic, clippy::unimplemented, clippy::todo, clippy::unreachable))]

#[cfg(target_os = "android")]
mod audio;
#[cfg(target_os = "android")]
mod backup;
pub mod browser;
#[cfg(target_os = "android")]
mod connection;
#[cfg(target_os = "android")]
mod create;
#[cfg(target_os = "android")]
mod creator;
#[cfg(target_os = "android")]
mod mcp;
#[cfg(target_os = "android")]
mod media_storage;
#[cfg(target_os = "android")]
mod pairing;
#[cfg(target_os = "android")]
mod performance;
#[cfg(target_os = "android")]
mod picker;
#[cfg(target_os = "android")]
mod restore;

#[cfg(target_os = "android")]
mod android_app {
    use std::net::Ipv4Addr;
    use std::path::PathBuf;
    use std::sync::mpsc::{Receiver, channel};

    use android_activity::AndroidApp;
    use filmcraft_engine::Session;
    use filmcraft_engine::autosave::AutosaveConfig;
    use filmcraft_ui_egui::FilmcraftApp;
    use filmcraft_ui_egui::native_dialogs::FileDialog;
    use filmcraft_ui_egui::state::Mode;
    use log::LevelFilter;
    use serde_json::json;

    use crate::audio::AndroidAudio;
    use crate::backup::Backup;
    use crate::browser::{Browser, Request, Root};
    use crate::connection::Connection;
    use crate::create::CreateMenu;
    use crate::creator::Creator;
    use crate::mcp::{Client, generate_token, private_address};
    use crate::media_storage::MediaStorage;
    use crate::pairing::SavedPairing;
    use crate::performance::Performance;
    use crate::picker::{Outcome, Picker};
    use crate::restore::Restore;

    /// Where the user's media usually is: shared storage and its standard folders, then the app's
    /// own folders. Reading shared storage needs the permission the user grants in system settings.
    fn shared_folders() -> Vec<Root> {
        let shared = PathBuf::from("/storage/emulated/0");
        [("Movies", "Movies"), ("DCIM", "DCIM"), ("Music", "Music"), ("Pictures", "Pictures"), ("Downloads", "Download")]
            .into_iter()
            .map(|(label, sub)| Root { label: label.to_string(), path: shared.join(sub) })
            .collect()
    }

    fn app_folders(app: &AndroidApp) -> Vec<Root> {
        let Some(dir) = app.external_data_path().or_else(|| app.internal_data_path()) else { return vec![] };
        ["Projects", "Exports", "Media"]
            .into_iter()
            .filter_map(|label| {
                let path = dir.join(label);
                match std::fs::create_dir_all(&path) {
                    Ok(()) => Some(Root { label: label.into(), path }),
                    Err(err) => {
                        log::warn!("could not prepare {label}: {err}");
                        None
                    }
                }
            })
            .collect()
    }

    /// The editor plus the in-app file browser it asks for whenever it needs a file dialog.
    struct Shell {
        app: FilmcraftApp,
        android: AndroidApp,
        requests: Receiver<Request>,
        browser: Option<Browser>,
        picker: Option<Picker>,
        roots: Vec<Root>,
        app_folders: Vec<Root>,
        connection: Option<Connection>,
        creator: Creator,
        media_storage: MediaStorage,
        performance: Performance,
        backup: Backup,
        restore: Restore,
        pairing: SavedPairing,
        show_ai: bool,
        show_review: bool,
        probe: Option<Receiver<Result<(), std::io::Error>>>,
        probe_result: Option<String>,
        ai_client: Client,
        lan_ip: Option<String>,
        mcp_entry_only: bool,
        show_mcp_token: bool,
        copied_config: bool,
        bridge_error: Option<String>,
        create_menu: CreateMenu,
    }

    impl Shell {
        /// Route file dialogs one at a time so each answer reaches the command that requested it.
        fn overlay(&mut self, ctx: &egui::Context) {
            let document = self.app.session.document_epoch;
            let outcome = self.picker.as_mut().and_then(|picker| picker.poll(ctx, &self.android, document));
            if let Some(outcome) = outcome
                && let Some(picker) = self.picker.take()
                && picker.belongs_to(document)
            {
                match outcome {
                    Outcome::Selected(files) => files.deliver(picker.request.reply),
                    Outcome::Cancelled => {}
                    Outcome::Fallback(error) => {
                        self.app.status(error);
                        self.open_browser(picker.request);
                    }
                    Outcome::Failed(error) => self.app.status(error),
                }
            }
            if self.browser.is_none()
                && self.picker.is_none()
                && !self.restore.picker_busy()
                && let Ok(request) = self.requests.try_recv()
            {
                if Picker::supports(&request.dialog)
                    && let Some(media) = self.app_folders.iter().find(|root| root.label == "Media")
                {
                    self.picker = Some(Picker::new(request, document, &media.path, &self.android));
                } else {
                    self.open_browser(request);
                }
            }
            if self.browser.as_mut().is_some_and(|b| b.show(ctx)) {
                self.browser = None;
            }
        }

        fn open_browser(&mut self, request: Request) {
            let mut roots = self.app_folders.clone();
            match request.dialog {
                FileDialog::SaveAs { .. } | FileDialog::Folder { .. } => roots.sort_by_key(|root| root.label != "Exports"),
                FileDialog::SaveProject { .. } | FileDialog::OpenProject => {}
                _ => {
                    roots.sort_by_key(|root| root.label != "Media");
                    roots.extend(self.roots.clone());
                }
            }
            self.browser = Some(Browser::new(request, roots));
        }

        fn creator_bar(&mut self, ui: &mut egui::Ui) {
            egui::Panel::top("googlebook_creator").show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.menu_button("New sequence", |ui| {
                        for (label, width, height) in [("Vertical 9:16", 1080, 1920), ("Wide 16:9", 1920, 1080), ("Square 1:1", 1080, 1080)] {
                            if ui.button(label).clicked() {
                                let p = json!({"name":label,"width":width,"height":height,"fps":30,"video":2,"audio":2});
                                match self.app.session.execute("file.newSequence", p) {
                                    Ok(_) => self.app.ui.mode = Mode::Edit,
                                    Err(err) => self.app.status(err.to_string()),
                                }
                                ui.close();
                            }
                        }
                    });
                    self.create_menu.show(ui, &mut self.app);
                    if let Some(picker) = &self.picker {
                        picker.show_progress(ui);
                    }
                    if ui.button("MCP Config").clicked() {
                        self.show_ai = true;
                    }
                    if self.connection.as_ref().is_some_and(Connection::has_proposal) && ui.button("Review AI edit").clicked() {
                        self.show_review = true;
                    }
                    self.creator.toolbar(ui, &mut self.app);
                    ui.menu_button("Tools", |ui| {
                        if ui.button("Project backup").clicked() {
                            self.backup.open();
                            ui.close();
                        }
                        if ui.button("Restore backup").clicked() {
                            self.restore.open();
                            ui.close();
                        }
                        if ui.button("Performance").clicked() {
                            self.performance.open();
                            ui.close();
                        }
                        let storage = ui.button("Media Storage");
                        self.app.auto.add_response("creator.mediaStorage", &storage, "Media Storage");
                        if storage.clicked() {
                            self.media_storage.open(ui.ctx());
                            ui.close();
                        }
                    });
                });
            });
        }

        fn ai_window(&mut self, ctx: &egui::Context) {
            if let Some(probe) = &self.probe {
                self.probe_result = match probe.try_recv() {
                    Ok(Ok(())) => Some("Local endpoint and token passed. Ask your AI to call filmcraft_inspect to test its connection.".into()),
                    Ok(Err(error)) => Some(format!("Local endpoint failed. {error}")),
                    Err(std::sync::mpsc::TryRecvError::Disconnected) => Some("The connection test stopped.".into()),
                    Err(std::sync::mpsc::TryRecvError::Empty) => None,
                };
                if self.probe_result.is_some() {
                    self.probe = None;
                }
            }
            if !self.show_ai {
                return;
            }
            let mut open = self.show_ai;
            egui::Window::new("MCP Config").open(&mut open).default_width(480.0).vscroll(true).show(ctx, |ui| {
                if let Some(connection) = &mut self.connection {
                    let document = self.app.session.document_epoch;
                    ui.label(if connection.server.running() { "MCP server running" } else { "MCP server stopped" });
                    ui.label(if connection.server.contacted() { "MCP client initialized" } else { "Waiting for an MCP client to initialize" });
                    ui.label(if connection.is_for(document) { "Current project approved" } else { "Project approval required" });
                    if !connection.is_for(document) {
                        ui.label(format!("Current project · {}", self.app.session.project.name));
                        if ui.button("Allow access to this project").clicked() {
                            connection.approve_project(document);
                            self.bridge_error = None;
                        }
                    }
                    if connection.has_proposal() && ui.button("Review AI edit").clicked() {
                        self.show_review = true;
                    }
                    if ui.add_enabled(self.probe.is_none(), egui::Button::new("Check connection")).clicked() {
                        let test = connection.server.connection_probe();
                        let wake = ctx.clone();
                        let (reply, result) = channel();
                        match std::thread::Builder::new().name("filmcraft-mcp-check".into()).spawn(move || {
                            let _ = reply.send(test());
                            wake.request_repaint();
                        }) {
                            Ok(_) => {
                                self.probe = Some(result);
                                self.probe_result = None;
                            }
                            Err(error) => self.probe_result = Some(error.to_string()),
                        }
                    }
                    if self.probe.is_some() {
                        ui.spinner();
                    }
                    if let Some(result) = &self.probe_result {
                        ui.label(result);
                    }
                    if connection.server.rejected_auth() > 0 {
                        ui.label("FilmCraft received a request with a missing or invalid token. Copy the current config and reload it in your AI client.");
                    }
                    ui.separator();
                    let previous = self.ai_client;
                    let previous_format = self.mcp_entry_only;
                    ui.horizontal_wrapped(|ui| {
                        for client in Client::ALL {
                            ui.selectable_value(&mut self.ai_client, client, client.label());
                        }
                    });
                    if previous != self.ai_client {
                        self.show_mcp_token = false;
                    }
                    ui.strong(self.ai_client.location());
                    let entry_only = if self.ai_client.uses_json() {
                        ui.horizontal_wrapped(|ui| {
                            ui.selectable_value(&mut self.mcp_entry_only, false, "Full JSON");
                            ui.selectable_value(&mut self.mcp_entry_only, true, "FilmCraft entry only");
                        });
                        self.mcp_entry_only
                    } else {
                        false
                    };
                    self.copied_config &= previous == self.ai_client && previous_format == self.mcp_entry_only;
                    ui.label(match (self.ai_client.uses_json(), entry_only) {
                        (false, _) => "Add this block to your config.toml and restart Codex.",
                        (true, true) => {
                            "Paste inside mcpServers. Add a comma between entries, or replace your existing filmcraft entry. Save and reload your MCP servers."
                        }
                        (true, false) => "Use this full file for a new or empty config. Save and reload your MCP servers.",
                    });
                    ui.horizontal_wrapped(|ui| {
                        let copy = if entry_only { "Copy FilmCraft entry" } else { "Copy config" };
                        if ui.button(copy).clicked() {
                            let config = connection.server.config(self.ai_client, entry_only, true);
                            match filmcraft_platform::android_ui::copy_sensitive_text(&self.android, &config) {
                                Ok(()) => {
                                    self.copied_config = true;
                                    self.bridge_error = None;
                                }
                                Err(error) => {
                                    self.copied_config = false;
                                    self.bridge_error = Some(format!("Could not copy config. {error}"));
                                }
                            }
                        }
                        if self.copied_config {
                            ui.label("Copied with your private token");
                        }
                        ui.checkbox(&mut self.show_mcp_token, "Reveal private token");
                    });
                    let config = connection.server.config(self.ai_client, entry_only, self.show_mcp_token);
                    egui::Frame::group(ui.style()).show(ui, |ui| {
                        egui::ScrollArea::both().id_salt("mcp-config-preview").max_height(152.0).show(ui, |ui| {
                            ui.add(
                                egui::Label::new(egui::RichText::new(config).monospace()).wrap_mode(egui::TextWrapMode::Extend).selectable(self.show_mcp_token),
                            );
                        });
                    });
                    ui.label(if connection.saved_pairing {
                        "Reuse this config after restarting the saved pairing. Approve access when you switch projects."
                    } else {
                        "Copy a fresh config after restarting the server or switching projects."
                    });
                    ui.collapsing("Connection help", |ui| {
                        ui.label("Keep FilmCraft open while your client edits this project.");
                        if connection.server.address.ip().is_loopback() {
                            let port = connection.server.address.port();
                            ui.label("If Android’s loopback is isolated, run this where ADB can see the device and use the same config.");
                            ui.monospace(format!("adb forward tcp:{port} tcp:{port}"));
                        }
                        ui.label("ChatGPT and Claude on the web need an authenticated HTTPS connector.");
                    });
                    if ui.button("Stop server").clicked() {
                        self.connection = None;
                        self.show_mcp_token = false;
                    }
                } else {
                    ui.label("Open the project you want to edit, then start the server to generate its MCP config.");
                    if ui.button("Start on this device").clicked() {
                        self.start_ai(ctx, Ipv4Addr::LOCALHOST, None);
                    }
                    if self.pairing.available() {
                        let label = if self.pairing.exists() { "Start saved local pairing" } else { "Create saved local pairing" };
                        if ui.button(label).on_hover_text("Use a fixed local port and authorize the current project.").clicked() {
                            match self.pairing.token(generate_token) {
                                Ok(token) => self.start_ai(ctx, Ipv4Addr::LOCALHOST, Some(token)),
                                Err(error) => self.bridge_error = Some(error.to_string()),
                            }
                        }
                    }
                    if ui.button("Connect over a trusted network…").clicked() {
                        self.lan_ip = Some(private_address().map_or_else(String::new, |ip| ip.to_string()));
                    }
                    if let Some(ip) = &mut self.lan_ip {
                        ui.label("Use this option when your AI client is on another device. Keep both on a trusted private network. The connection uses HTTP.");
                        ui.horizontal(|ui| {
                            ui.label("FilmCraft IP");
                            ui.add(egui::TextEdit::singleline(ip).char_limit(15).desired_width(115.0));
                        });
                        let address = ip.trim().parse::<Ipv4Addr>().ok().filter(Ipv4Addr::is_private);
                        if ui.add_enabled(address.is_some(), egui::Button::new("Start network server")).clicked()
                            && let Some(address) = address
                        {
                            self.start_ai(ctx, address, None);
                        }
                    }
                }
                if self.pairing.exists() && ui.button("Revoke saved pairing").clicked() {
                    match self.pairing.forget() {
                        Ok(()) => {
                            if self.connection.as_ref().is_some_and(|connection| connection.saved_pairing) {
                                self.connection = None;
                            }
                            self.show_mcp_token = false;
                            self.bridge_error = None;
                        }
                        Err(error) => self.bridge_error = Some(format!("Could not revoke the pairing. {error}")),
                    }
                }
                if let Some(err) = &self.bridge_error {
                    ui.label(err);
                }
            });
            self.show_ai = open;
            if !open {
                self.show_mcp_token = false;
            }
        }

        fn start_ai(&mut self, ctx: &egui::Context, ip: Ipv4Addr, saved_token: Option<String>) {
            match Connection::start(ctx.clone(), self.app.session.document_epoch, ip, saved_token) {
                Ok(connection) => {
                    self.connection = Some(connection);
                    self.bridge_error = None;
                    self.copied_config = false;
                    self.show_mcp_token = false;
                    self.probe = None;
                    self.probe_result = None;
                }
                Err(err) => self.bridge_error = Some(format!("Could not start the MCP server. {err}")),
            }
        }
    }

    impl eframe::App for Shell {
        fn raw_input_hook(&mut self, ctx: &egui::Context, raw_input: &mut egui::RawInput) {
            eframe::App::raw_input_hook(&mut self.app, ctx, raw_input);
        }

        fn logic(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
            if let Some(connection) = &mut self.connection
                && !connection.is_for(self.app.session.document_epoch)
            {
                if connection.saved_pairing {
                    if connection.require_project_approval() {
                        self.show_mcp_token = false;
                    }
                } else {
                    self.connection = None;
                    self.bridge_error = Some("Project changed. Start a new connection for this project.".into());
                }
            }
            if self.connection.as_ref().is_some_and(|connection| !connection.server.running()) {
                self.connection = None;
                self.bridge_error = Some("The MCP server stopped. Start a new connection.".into());
            }
            if let Some(connection) = &mut self.connection {
                connection.drain(&mut self.app.session);
            }
            eframe::App::logic(&mut self.app, ctx, frame);
            self.creator.poll(ctx, &mut self.app);
            self.performance.poll(ctx, &mut self.app);
        }

        fn on_exit(&mut self) {
            self.restore.on_exit(&self.android);
            if let Some(picker) = &self.picker {
                picker.cancel_copy();
                filmcraft_platform::android_picker::cancel_documents(&self.android);
            }
            eframe::App::on_exit(&mut self.app);
        }

        fn ui(&mut self, ui: &mut egui::Ui, frame: &mut eframe::Frame) {
            self.creator_bar(ui);
            eframe::App::ui(&mut self.app, ui, frame);
            let ctx = ui.ctx().clone();
            self.overlay(&ctx);
            self.ai_window(&ctx);
            if self.show_review {
                if let Some(connection) = &mut self.connection {
                    let transport_running = self.app.transport_running();
                    egui::Window::new("Review AI edit").open(&mut self.show_review).default_width(520.0).vscroll(true).show(&ctx, |ui| {
                        connection.show_review(ui, &mut self.app.session, transport_running);
                    });
                    if !self.show_review {
                        connection.stop_preview();
                    }
                } else {
                    self.show_review = false;
                }
            }
            self.backup.show(&ctx, &mut self.app, &self.android);
            self.restore.show(&ctx, &mut self.app, &self.android, self.picker.is_some());
            self.media_storage.show(&ctx, &mut self.app);
            self.performance.show(&ctx, &mut self.app, self.creator.last_export());
        }
    }

    // SAFETY: The `android_main` function is the mandatory C-ABI entrypoint invoked by
    // the `android-activity` runtime when the native activity starts up.
    #[allow(unsafe_code)]
    #[unsafe(no_mangle)]
    fn android_main(app: AndroidApp) {
        android_logger::init_once(android_logger::Config::default().with_max_level(LevelFilter::Info).with_tag("FilmCraft"));

        log::info!("Starting FilmCraft on Android");
        log::info!("Android media registration: {:?}", filmcraft_platform::register_android(&app));

        // Preferences, shortcuts, presets, auto-save and crash recovery live in the app-private
        // data folder, the only place an app can always write.
        let data_dir = app.internal_data_path();
        let roots = shared_folders();
        let app_folders = app_folders(&app);

        let mut native_options = eframe::NativeOptions::default();
        native_options.android_app = Some(app.clone());

        let res = eframe::run_native(
            "FilmCraft",
            native_options,
            Box::new(move |cc| {
                let mut session = Session::default();
                let pairing = SavedPairing::new(data_dir.as_deref());
                let create_menu = CreateMenu::new(data_dir.as_deref());
                match data_dir {
                    Some(dir) => {
                        if let Err(e) = session.start_autosave(AutosaveConfig::new(dir)) {
                            log::warn!("settings, auto-save and crash recovery unavailable: {e}");
                        }
                    }
                    None => log::warn!("no app data folder: settings and auto-save are not kept"),
                }
                let mut filmcraft = FilmcraftApp::new(session);
                let exports = app_folders.iter().find(|root| root.label == "Exports").map(|root| root.path.clone());
                let media = app_folders.iter().find(|root| root.label == "Media").map(|root| root.path.clone());
                let projects = app_folders.iter().find(|root| root.label == "Projects").map(|root| root.path.clone());
                filmcraft_ui_egui::install_ime_reset_callbacks(&cc.egui_ctx);
                if let Some(render_state) = cc.wgpu_render_state.clone() {
                    filmcraft.set_wgpu(render_state);
                }
                if let Some(exports) = app_folders.iter().find(|root| root.label == "Exports") {
                    filmcraft.ui.export.location = exports.path.to_string_lossy().into_owned();
                }
                // playback sound and the audio master clock
                filmcraft.audio = Some(Box::new(AndroidAudio::new()));
                // Shell routes dialogs to the system picker or to the in-app browser.
                let (requests_tx, requests) = channel::<Request>();
                let ctx = cc.egui_ctx.clone();
                filmcraft.hooks.run_dialog = Some(Box::new(move |dialog| {
                    let (reply, answer) = channel();
                    // a failed send drops `reply`, which the app takes as cancelled
                    let _ = requests_tx.send(Request { dialog, reply });
                    ctx.request_repaint();
                    answer
                }));
                Ok(Box::new(Shell {
                    app: filmcraft,
                    android: app.clone(),
                    requests,
                    browser: None,
                    picker: None,
                    roots,
                    app_folders,
                    connection: None,
                    backup: Backup::new(exports.clone()),
                    restore: Restore::new(projects),
                    creator: Creator::new(app, exports),
                    media_storage: MediaStorage::new(media),
                    performance: Performance::default(),
                    pairing,
                    show_ai: false,
                    show_review: false,
                    probe: None,
                    probe_result: None,
                    ai_client: Client::Antigravity,
                    lan_ip: None,
                    mcp_entry_only: false,
                    show_mcp_token: false,
                    copied_config: false,
                    bridge_error: None,
                    create_menu,
                }))
            }),
        );

        if let Err(err) = res {
            log::error!("Failed to launch FilmCraft Android runner: {err:?}");
        }
    }
}

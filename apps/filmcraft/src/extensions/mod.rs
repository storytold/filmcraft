//! Opt-in desktop workflow extensions. The editor owns installation, grants and child lifetime.
mod bridge;
mod package;
mod window;

use filmcraft_ui_egui::FilmcraftApp;
use package::{Manifest, Package};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs,
    io::{BufRead, BufReader, Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc::{self, Receiver, Sender, SyncSender},
    },
    time::{Duration, Instant},
};

const MAX_INSTALLED: usize = 32;
const MAX_RUNNING: usize = 4;
const QUEUE: usize = 32;

pub fn helper(args: &[std::ffi::OsString]) -> Result<(), String> {
    let [flag, path, expected, data_dir] = args else {
        return Err("Internal extension host needs a package path, approved digest and webview data directory".into());
    };
    if flag != "--extension-window" {
        return Err("Invalid extension host invocation".into());
    }
    let bytes = read_bytes(Path::new(path), package::MAX_ARCHIVE_BYTES)?;
    if expected.to_str() != Some(digest(&bytes).as_str()) {
        return Err("Extension package changed after approval".into());
    }
    window::run(Package::from_bytes(&bytes)?, PathBuf::from(data_dir))
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    sha256: String,
    enabled: bool,
}
struct Installed {
    manifest: Manifest,
    path: PathBuf,
    receipt: Receipt,
}
struct Rejected {
    path: PathBuf,
    error: String,
    removable: bool,
}
struct Pending {
    bytes: Vec<u8>,
    package: Package,
}
struct Running {
    child: ManagedChild,
    incoming: Receiver<String>,
    outgoing: SyncSender<String>,
    context: Value,
    theme: Value,
    disconnected_at: Option<Instant>,
}
// The reaper is created before the process, so launching fails cleanly if a
// worker cannot be created. Closing a window kills it promptly but never waits
// on the editor's event thread. The reaper also owns shutdown cleanup.
struct ManagedChild {
    child: Option<Child>,
    reaper: Sender<Child>,
}
impl ManagedChild {
    fn spawn(command: &mut Command) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel::<Child>();
        std::thread::Builder::new()
            .name("extension-reaper".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(|| {
                    if let Ok(mut child) = rx.recv() {
                        child.wait().map_err(|e| e.to_string())?;
                    }
                    Ok::<(), String>(())
                });
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(e)) => eprintln!("Extension process cleanup failed: {e}"),
                    Err(_) => eprintln!("Extension process cleanup worker failed"),
                }
            })
            .map_err(|e| format!("Could not start extension process cleanup: {e}"))?;
        let child = command.spawn().map_err(|e| e.to_string())?;
        Ok(Self { child: Some(child), reaper: tx })
    }
    fn child_mut(&mut self) -> Result<&mut Child, String> {
        self.child.as_mut().ok_or_else(|| "Extension process is already closed".into())
    }
}
impl Drop for ManagedChild {
    fn drop(&mut self) {
        if let Some(mut child) = self.child.take() {
            // kill does not wait for the OS to release the process. Even if the
            // child exited between polling and closing, wait still reaps it.
            let _ = child.kill();
            if self.reaper.send(child).is_err() {
                eprintln!("Extension process cleanup worker is unavailable");
            }
        }
    }
}
impl Running {
    fn send(&self, value: &Value) -> Result<(), String> {
        let line = serde_json::to_string(value).map_err(|e| e.to_string())?;
        if line.len() >= bridge::MAX_MESSAGE {
            return Err("Extension response exceeds 256 KiB".into());
        }
        self.outgoing.try_send(line).map_err(|_| "Extension stopped responding; close and reopen it".into())
    }
}

pub struct Manager {
    root: PathBuf,
    installed: BTreeMap<String, Installed>,
    rejected: BTreeMap<String, Rejected>,
    running: BTreeMap<String, Running>,
    pending: Option<Pending>,
    loading: Option<Receiver<Result<Pending, String>>>,
    open: bool,
    error: Option<String>,
    last_event: Instant,
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}
fn receipt_path(path: &Path) -> PathBuf {
    path.with_extension("receipt.json")
}
fn read_bytes(path: &Path, limit: usize) -> Result<Vec<u8>, String> {
    let kind = fs::symlink_metadata(path).map_err(|e| e.to_string())?.file_type();
    if !kind.is_file() || kind.is_symlink() {
        return Err("Extension packages and receipts must be regular files".into());
    }
    let mut bytes = Vec::new();
    fs::File::open(path).map_err(|e| e.to_string())?.take(limit as u64 + 1).read_to_end(&mut bytes).map_err(|e| e.to_string())?;
    if bytes.len() > limit {
        return Err("Extension file exceeds its size limit".into());
    }
    Ok(bytes)
}
fn write_new(path: &Path, bytes: &[u8]) -> Result<(), String> {
    let mut f = fs::OpenOptions::new().write(true).create_new(true).open(path).map_err(|e| e.to_string())?;
    if let Err(e) = f.write_all(bytes).and_then(|_| f.sync_all()) {
        let _ = fs::remove_file(path);
        return Err(e.to_string());
    }
    Ok(())
}
fn write_receipt(path: &Path, receipt: &Receipt) -> Result<(), String> {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let bytes = serde_json::to_vec(receipt).map_err(|e| e.to_string())?;
    // A stale temporary file from an interrupted save must not permanently
    // prevent Enable/Disable. Each write gets an exclusive, unique filename.
    let mut temporary = None;
    for _ in 0..128 {
        let nonce = NEXT.fetch_add(1, Ordering::Relaxed);
        let candidate = path.with_extension(format!("receipt.{}.{nonce}.pending", std::process::id()));
        match fs::OpenOptions::new().write(true).create_new(true).open(&candidate) {
            Ok(file) => {
                temporary = Some((candidate, file));
                break;
            }
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e.to_string()),
        }
    }
    let (tmp, mut file) = temporary.ok_or("Could not reserve an extension receipt temporary file")?;
    let result = file.write_all(&bytes).and_then(|_| file.sync_all()).map_err(|e| e.to_string());
    drop(file);
    let result = result.and_then(|()| fs::rename(&tmp, receipt_path(path)).map_err(|e| e.to_string()));
    if result.is_err() {
        let _ = fs::remove_file(tmp);
    }
    result
}

impl Manager {
    pub fn new(data_root: PathBuf) -> Self {
        let mut m = Self {
            root: data_root.join("Extensions"),
            installed: BTreeMap::new(),
            rejected: BTreeMap::new(),
            running: BTreeMap::new(),
            pending: None,
            loading: None,
            open: false,
            error: None,
            last_event: Instant::now(),
        };
        if let Err(e) = m.scan() {
            m.error = Some(e);
        }
        m
    }
    fn scan(&mut self) -> Result<(), String> {
        if !self.root.exists() {
            return Ok(());
        }
        for entry in fs::read_dir(&self.root).map_err(|e| e.to_string())?.take(MAX_INSTALLED * 3 + 1) {
            let path = entry.map_err(|e| e.to_string())?.path();
            if path.extension().is_none_or(|e| e != "fcplugin") {
                continue;
            }
            if self.installed.len() + self.rejected.len() >= MAX_INSTALLED {
                return Err("At most 32 extensions may be installed".into());
            }
            match self.load_installed(&path) {
                Ok(i) => {
                    self.installed.insert(i.manifest.id.clone(), i);
                }
                Err(error) => {
                    let removable = fs::symlink_metadata(&path).is_ok_and(|m| !m.file_type().is_dir());
                    // This opaque key is never interpreted as a filesystem path.
                    let key = format!("rejected:{}", digest(path.as_os_str().as_encoded_bytes()));
                    self.rejected.insert(key, Rejected { path, error, removable });
                }
            }
        }
        Ok(())
    }
    fn load_installed(&self, path: &Path) -> Result<Installed, String> {
        let bytes = read_bytes(path, package::MAX_ARCHIVE_BYTES)?;
        let receipt: Receipt = serde_json::from_slice(&read_bytes(&receipt_path(path), 4096)?).map_err(|e| e.to_string())?;
        if digest(&bytes) != receipt.sha256 {
            return Err("Package changed after installation; remove and reinstall it to review its permissions".into());
        }
        let package = Package::from_bytes(&bytes)?;
        if path.file_stem().and_then(|s| s.to_str()) != Some(package.manifest.id.as_str()) {
            return Err("Package id does not match its installed filename".into());
        }
        Ok(Installed { manifest: package.manifest, path: path.to_owned(), receipt })
    }
    fn choose(&mut self, path: Option<PathBuf>, ctx: egui::Context) -> Result<(), String> {
        if self.loading.is_some() || self.pending.is_some() {
            return Err("Finish the current extension installation first".into());
        }
        let (tx, rx) = mpsc::sync_channel(1);
        std::thread::Builder::new()
            .name("extension-package".into())
            .spawn(move || {
                let result = std::panic::catch_unwind(|| {
                    let path = path.or_else(|| rfd::FileDialog::new().add_filter("FilmCraft Extension", &["fcplugin", "zip"]).pick_file());
                    let Some(path) = path else {
                        return Err(String::new());
                    };
                    let bytes = read_bytes(&path, package::MAX_ARCHIVE_BYTES)?;
                    let package = Package::from_bytes(&bytes)?;
                    Ok(Pending { bytes, package })
                })
                .unwrap_or_else(|_| Err("Could not read the extension package".into()));
                let _ = tx.send(result);
                ctx.request_repaint();
            })
            .map_err(|e| e.to_string())?;
        self.loading = Some(rx);
        self.open = true;
        Ok(())
    }
    fn install_pending(&mut self) -> Result<(), String> {
        let pending = self.pending.as_ref().ok_or("No package is awaiting installation")?;
        let id = &pending.package.manifest.id;
        if self.installed.contains_key(id) {
            return Err("This extension is already installed. Remove it before installing another version".into());
        }
        if self.installed.len() + self.rejected.len() >= MAX_INSTALLED {
            return Err("At most 32 extensions may be installed".into());
        }
        if self.rejected.values().any(|entry| entry.path.file_stem().and_then(|s| s.to_str()) == Some(id.as_str())) {
            return Err("An unavailable copy of this extension exists. Remove it before reinstalling".into());
        }
        fs::create_dir_all(&self.root).map_err(|e| e.to_string())?;
        let path = self.root.join(format!("{id}.fcplugin"));
        let receipt = Receipt { sha256: digest(&pending.bytes), enabled: true };
        write_new(&path, &pending.bytes)?;
        if let Err(e) = write_receipt(&path, &receipt) {
            let _ = fs::remove_file(&path);
            return Err(e);
        }
        self.installed.insert(id.clone(), Installed { manifest: pending.package.manifest.clone(), path, receipt });
        self.pending = None;
        Ok(())
    }
    fn open_extension(&mut self, id: &str, ctx: egui::Context) -> Result<(), String> {
        if self.running.contains_key(id) {
            return Ok(());
        }
        if self.running.len() >= MAX_RUNNING {
            return Err("Close an extension before opening another (maximum four windows)".into());
        }
        let installed = self.installed.get(id).ok_or("Extension is not installed")?;
        if !installed.receipt.enabled {
            return Err("Enable this extension first".into());
        }
        let checked = self.load_installed(&installed.path)?;
        if checked.receipt.sha256 != installed.receipt.sha256 {
            return Err("Package changed since installation".into());
        }
        let runtime = self.root.join("Runtime").join(id);
        fs::create_dir_all(&runtime).map_err(|e| e.to_string())?;
        let mut child = ManagedChild::spawn(
            Command::new(std::env::current_exe().map_err(|e| e.to_string())?)
                .arg("--extension-window")
                .arg(&installed.path)
                .arg(&installed.receipt.sha256)
                .arg(runtime)
                .env_remove("FILMCRAFT_CONTROL_PORT")
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit()),
        )?;
        let Some(stdout) = child.child_mut()?.stdout.take() else {
            return Err("Extension output pipe unavailable".into());
        };
        let Some(mut stdin) = child.child_mut()?.stdin.take() else {
            return Err("Extension input pipe unavailable".into());
        };
        let (in_tx, in_rx) = mpsc::sync_channel(QUEUE);
        let (out_tx, out_rx) = mpsc::sync_channel::<String>(QUEUE);
        let read_ctx = ctx.clone();
        let reader = std::thread::Builder::new().name("extension-reader".into()).spawn(move || {
            // The context is used only to request repaint; no editor state crosses this worker.
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
                let mut reader = BufReader::new(stdout);
                loop {
                    let mut line = String::new();
                    match reader.by_ref().take(bridge::MAX_MESSAGE as u64 + 1).read_line(&mut line) {
                        Ok(0) | Err(_) => break,
                        Ok(_) => {
                            if line.len() > bridge::MAX_MESSAGE || !line.ends_with('\n') || in_tx.try_send(line).is_err() {
                                break;
                            }
                            read_ctx.request_repaint();
                        }
                    }
                }
            }));
            if result.is_err() {
                log::error!("Extension output reader failed");
            }
        });
        if let Err(e) = reader {
            return Err(e.to_string());
        }
        let writer = std::thread::Builder::new().name("extension-writer".into()).spawn(move || {
            let result = std::panic::catch_unwind(move || {
                while let Ok(line) = out_rx.recv() {
                    if stdin.write_all(line.as_bytes()).and_then(|_| stdin.write_all(b"\n")).and_then(|_| stdin.flush()).is_err() {
                        break;
                    }
                }
            });
            if result.is_err() {
                log::error!("Extension input writer failed");
            }
        });
        if let Err(e) = writer {
            return Err(e.to_string());
        }
        self.running.insert(id.into(), Running { child, incoming: in_rx, outgoing: out_tx, context: Value::Null, theme: Value::Null, disconnected_at: None });
        ctx.request_repaint();
        Ok(())
    }
    fn enable(&mut self, id: &str, enabled: bool) -> Result<(), String> {
        let installed = self.installed.get_mut(id).ok_or("Extension is not installed")?;
        let receipt = Receipt { sha256: installed.receipt.sha256.clone(), enabled };
        write_receipt(&installed.path, &receipt)?;
        installed.receipt = receipt;
        if !enabled {
            self.running.remove(id);
        }
        Ok(())
    }
    fn remove(&mut self, id: &str) -> Result<(), String> {
        self.running.remove(id);
        let path = if let Some(installed) = self.installed.get(id) {
            &installed.path
        } else if let Some(rejected) = self.rejected.get(id) {
            if !rejected.removable {
                return Err("This extension entry is a directory; remove it manually before reinstalling".into());
            }
            &rejected.path
        } else {
            return Err("Extension is not installed".into());
        };
        // Only entries enumerated directly inside our installation directory can
        // be removed. remove_file unlinks a symlink itself, never its target.
        if path.parent() != Some(self.root.as_path()) || path.extension().is_none_or(|e| e != "fcplugin") {
            return Err("Extension removal must stay inside its installation directory".into());
        }
        if fs::symlink_metadata(path).is_ok_and(|m| m.file_type().is_dir()) {
            return Err("This extension entry is a directory; remove it manually before reinstalling".into());
        }
        // Delete the receipt first: if an unexpected directory or a permission
        // error blocks cleanup, keep the package visible and retryable.
        match fs::remove_file(receipt_path(path)) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("Could not remove extension receipt: {e}")),
        }
        fs::remove_file(path).map_err(|e| e.to_string())?;
        self.installed.remove(id);
        self.rejected.remove(id);
        Ok(())
    }
    fn inspection(&self) -> Value {
        json!({
            "open": self.open,
            "loading": self.loading.is_some(),
            "pending": self.pending.as_ref().map(|p| &p.package.manifest),
            "error": self.error,
            "installed": self.installed.iter().map(|(id, i)| json!({
                "id": id, "name": i.manifest.name, "enabled": i.receipt.enabled,
                "running": self.running.contains_key(id),
            })).collect::<Vec<_>>(),
            "rejected": self.rejected.iter().map(|(id, r)| json!({
                "id": id, "filename": r.path.file_name().map(|n| n.to_string_lossy()),
                "error": r.error, "removable": r.removable,
            })).collect::<Vec<_>>(),
        })
    }
    pub fn invoke(&mut self, app: &mut FilmcraftApp, ctx: &egui::Context, command: &str, p: Value) -> Result<Value, String> {
        let id = p.get("id").and_then(Value::as_str).unwrap_or("");
        match command {
            "extensions.frame" => self.frame(app, ctx),
            "extensions.manage" => self.open = true,
            "extensions.inspect" => return Ok(self.inspection()),
            "extensions.install" => self.choose(p.get("path").and_then(Value::as_str).map(PathBuf::from), ctx.clone())?,
            "extensions.open" => self.open_extension(id, ctx.clone())?,
            "extensions.close" => {
                self.running.remove(id);
            }
            "extensions.enable" => self.enable(id, p.get("enabled").and_then(Value::as_bool).ok_or("enabled must be a boolean")?)?,
            "extensions.remove" => self.remove(id)?,
            _ => return Err("Unknown extension command".into()),
        }
        Ok(Value::Null)
    }
    fn poll(&mut self, app: &mut FilmcraftApp, ctx: &egui::Context) {
        if let Some(rx) = self.loading.take() {
            match rx.try_recv() {
                Ok(Ok(p)) => {
                    self.pending = Some(p);
                    self.error = None;
                }
                Ok(Err(e)) => {
                    if !e.is_empty() {
                        self.error = Some(e);
                    }
                }
                Err(mpsc::TryRecvError::Empty) => self.loading = Some(rx),
                Err(mpsc::TryRecvError::Disconnected) => self.error = Some("Package loader stopped".into()),
            }
        }
        let tick = self.last_event.elapsed() >= Duration::from_millis(250);
        if tick {
            self.last_event = Instant::now();
        }
        let mut closed = Vec::new();
        for (id, run) in &mut self.running {
            match run.child.child_mut().and_then(|c| c.try_wait().map_err(|e| e.to_string())) {
                Ok(Some(status)) => {
                    if !status.success() {
                        self.error = Some(format!(
                            "Extension {id} closed unexpectedly ({status}). Check that the system webview runtime is available, then try reopening it."
                        ));
                        self.open = true;
                    }
                    closed.push(id.clone());
                    continue;
                }
                Ok(None) => {}
                Err(e) => {
                    self.error = Some(format!("Could not check extension process: {e}"));
                    self.open = true;
                    closed.push(id.clone());
                    continue;
                }
            }
            let Some(installed) = self.installed.get(id) else {
                closed.push(id.clone());
                continue;
            };
            for _ in 0..8 {
                match run.incoming.try_recv() {
                    Ok(line) => {
                        if serde_json::from_str::<Value>(&line).ok().is_some_and(|v| v.get("event") == Some(&json!("ready"))) {
                            run.context = Value::Null;
                            run.theme = Value::Null;
                            continue;
                        }
                        let response = bridge::dispatch(&mut app.session, &app.tokens, &installed.manifest, &line);
                        if response.get("result").is_some()
                            && serde_json::from_str::<Value>(&line).ok().is_some_and(|v| v.get("method").and_then(Value::as_str) == Some("playhead.seek"))
                        {
                            // Navigation ends ordinary playback. Recording is rejected before
                            // the bridge changes time, so stopping cannot commit a recording.
                            app.stop_source();
                            app.stop();
                        }
                        if let Err(e) = run.send(&response) {
                            self.error = Some(e);
                            self.open = true;
                            closed.push(id.clone());
                            break;
                        }
                    }
                    Err(mpsc::TryRecvError::Empty) => break,
                    Err(mpsc::TryRecvError::Disconnected) => {
                        // EOF can arrive just before the OS reports process exit. Keep
                        // polling briefly so normal closes and failed starts stay distinct.
                        let since = run.disconnected_at.get_or_insert_with(Instant::now);
                        if since.elapsed() >= Duration::from_secs(1) {
                            self.error = Some(format!("Extension {id} lost its connection. Close and reopen it to try again."));
                            self.open = true;
                            closed.push(id.clone());
                        }
                        break;
                    }
                }
            }
            if tick && run.disconnected_at.is_none() {
                let theme = bridge::theme(&app.tokens);
                let context = if installed.manifest.has_permission("project.read") {
                    bridge::snapshot(&app.session)
                } else {
                    json!({"documentId":app.session.document_generation.to_string(),"revision":app.session.revision.to_string()})
                };
                let result = (|| {
                    if theme != run.theme {
                        run.send(&json!({"event":"theme.changed","payload":theme}))?;
                        run.theme = theme;
                    }
                    if context != run.context {
                        run.send(&json!({"event":"document.changed","payload":context}))?;
                        run.context = context;
                    }
                    Ok::<(), String>(())
                })();
                if let Err(e) = result {
                    self.error = Some(e);
                    self.open = true;
                    closed.push(id.clone());
                }
            }
        }
        for id in closed {
            self.running.remove(&id);
        }
        if !self.running.is_empty() || self.loading.is_some() {
            ctx.request_repaint_after(Duration::from_millis(33));
        }
    }
    fn frame(&mut self, app: &mut FilmcraftApp, ctx: &egui::Context) {
        self.poll(app, ctx);
        if !self.open {
            return;
        }
        let mut open = self.open;
        let mut action = None;
        egui::Window::new("Extensions").id(egui::Id::new("extensions.manager")).default_width(500.0).min_width(360.0).open(&mut open).show(ctx, |ui| {
            ui.label("Install web extensions with their own windows and workflows.");
            let button = ui.add_enabled(self.loading.is_none() && self.pending.is_none(), egui::Button::new("Install from File…"));
            app.auto.add_response("extensions.choose", &button, "Install from File");
            if button.clicked() {
                action = Some(("choose".to_owned(), String::new(), false));
            }
            if self.loading.is_some() {
                ui.label("Reading extension package…");
            }
            if let Some(error) = &self.error {
                ui.colored_label(app.tokens.accent, error);
            }
            if let Some(pending) = &self.pending {
                ui.separator();
                let m = &pending.package.manifest;
                ui.heading(&m.name);
                ui.label(format!("{} · {} · Editor API {}", m.id, m.version, m.api_version));
                ui.label("Requested editor permissions:");
                if m.permissions.is_empty() {
                    ui.label("None");
                }
                for permission in &m.permissions {
                    ui.label(permission_label(permission));
                }
                if !m.network_origins.is_empty() {
                    ui.label("Network destinations:");
                    for origin in &m.network_origins {
                        ui.monospace(origin);
                    }
                }
                ui.label("Install packages from authors you trust. Web content runs in a native web view.");
                ui.horizontal(|ui| {
                    let accept = ui.button("Install Extension");
                    app.auto.add_response("extensions.confirmInstall", &accept, "Install Extension");
                    if accept.clicked() {
                        action = Some(("install".into(), String::new(), false));
                    }
                    let cancel = ui.button("Cancel");
                    app.auto.add_response("extensions.cancelInstall", &cancel, "Cancel");
                    if cancel.clicked() {
                        action = Some(("cancel".into(), String::new(), false));
                    }
                });
            }
            ui.separator();
            if self.installed.is_empty() && self.rejected.is_empty() {
                ui.label("No extensions installed.");
            }
            egui::ScrollArea::vertical().max_height(380.0).show(ui, |ui| {
                for (id, i) in &self.installed {
                    ui.group(|ui| {
                        ui.label(egui::RichText::new(&i.manifest.name).strong());
                        ui.small(format!("{} · {}", id, i.manifest.version));
                        ui.horizontal_wrapped(|ui| {
                            let mut enabled = i.receipt.enabled;
                            let toggle = ui.checkbox(&mut enabled, "Enabled");
                            app.auto.add_response(&format!("extensions.{id}.enabled"), &toggle, "Enabled");
                            if toggle.changed() {
                                action = Some(("enable".into(), id.clone(), enabled));
                            }
                            let running = self.running.contains_key(id);
                            let label = if running { "Close" } else { "Open" };
                            let button = ui.add_enabled(enabled, egui::Button::new(label));
                            app.auto.add_response(&format!("extensions.{id}.open"), &button, label);
                            if button.clicked() {
                                action = Some((if running { "close" } else { "open" }.into(), id.clone(), false));
                            }
                            let remove = ui.button("Remove");
                            app.auto.add_response(&format!("extensions.{id}.remove"), &remove, "Remove");
                            if remove.clicked() {
                                action = Some(("remove".into(), id.clone(), false));
                            }
                        });
                    });
                }
                for (id, rejected) in &self.rejected {
                    ui.group(|ui| {
                        ui.label(egui::RichText::new("Unavailable extension").strong());
                        ui.small(rejected.path.file_name().map(|name| name.to_string_lossy()).unwrap_or_default());
                        ui.label(&rejected.error);
                        let remove = ui.add_enabled(rejected.removable, egui::Button::new("Remove"));
                        app.auto.add_response(&format!("extensions.{id}.remove"), &remove, "Remove unavailable extension");
                        if remove.clicked() {
                            action = Some(("remove".into(), id.clone(), false));
                        }
                        if !rejected.removable {
                            ui.small("This entry is a directory. Remove it manually before reinstalling.");
                        }
                    });
                }
            });
        });
        self.open = open;
        if let Some((action, id, enabled)) = action {
            let result = match action.as_str() {
                "choose" => self.choose(None, ctx.clone()),
                "install" => self.install_pending(),
                "cancel" => {
                    self.pending = None;
                    Ok(())
                }
                "open" => self.open_extension(&id, ctx.clone()),
                "close" => {
                    self.running.remove(&id);
                    Ok(())
                }
                "enable" => self.enable(&id, enabled),
                "remove" => self.remove(&id),
                _ => Ok(()),
            };
            self.error = result.err();
        }
    }
}
fn permission_label(p: &str) -> &str {
    match p {
        "project.read" => "Read the current sequence and its markers",
        "playhead.write" => "Move the playhead",
        "markers.write" => "Add timeline markers (undoable)",
        "network" => "Connect to the destinations below",
        _ => p,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;
    struct Temp(PathBuf);
    impl Temp {
        fn new() -> Self {
            static NEXT: AtomicU64 = AtomicU64::new(0);
            let path = std::env::temp_dir().join(format!("fc-ext-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
            fs::create_dir_all(&path).unwrap();
            Self(path)
        }
    }
    impl Drop for Temp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
    fn pending() -> Pending {
        let manifest = json!({"id":"org.example.test","name":"Test","version":"1.0.0","apiVersion":1,"entry":"index.html","permissions":["project.read"]});
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, content) in [("manifest.json", manifest.to_string()), ("index.html", "<!doctype html><p>Test</p>".into())] {
            zip.start_file(name, zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored)).unwrap();
            zip.write_all(content.as_bytes()).unwrap();
        }
        let bytes = zip.finish().unwrap().into_inner();
        let package = Package::from_bytes(&bytes).unwrap();
        Pending { bytes, package }
    }
    #[test]
    fn installation_is_reviewed_persistent_disabled_and_removable() {
        let temp = Temp::new();
        let mut manager = Manager::new(temp.0.clone());
        manager.pending = Some(pending());
        assert!(Manager::new(temp.0.clone()).installed.is_empty());
        manager.install_pending().unwrap();
        assert!(manager.running.is_empty(), "install must not execute code");
        let mut reloaded = Manager::new(temp.0.clone());
        assert_eq!(reloaded.installed.len(), 1);
        reloaded.enable("org.example.test", false).unwrap();
        let mut reloaded = Manager::new(temp.0.clone());
        assert!(!reloaded.installed["org.example.test"].receipt.enabled);
        assert!(reloaded.open_extension("org.example.test", egui::Context::default()).is_err());
        reloaded.remove("org.example.test").unwrap();
        assert!(Manager::new(temp.0.clone()).installed.is_empty());
    }
    #[test]
    fn changed_or_unreviewed_packages_never_load() {
        let temp = Temp::new();
        let mut manager = Manager::new(temp.0.clone());
        manager.pending = Some(pending());
        manager.install_pending().unwrap();
        let path = manager.installed["org.example.test"].path.clone();
        let original = fs::read(&path).unwrap();
        fs::write(&path, b"changed").unwrap();
        assert!(Manager::new(temp.0.clone()).installed.is_empty());
        fs::write(&path, &original).unwrap();
        fs::remove_file(receipt_path(&path)).unwrap();
        assert!(Manager::new(temp.0.clone()).installed.is_empty());
        let args = ["--extension-window".into(), path.into_os_string(), digest(b"other").into(), temp.0.clone().into_os_string()];
        assert!(helper(&args).unwrap_err().contains("changed"));
    }
    #[test]
    fn duplicate_install_does_not_replace_approved_package() {
        let temp = Temp::new();
        let mut manager = Manager::new(temp.0.clone());
        manager.pending = Some(pending());
        manager.install_pending().unwrap();
        let path = manager.installed["org.example.test"].path.clone();
        let original = fs::read(&path).unwrap();
        manager.pending = Some(pending());
        assert!(manager.install_pending().is_err());
        assert_eq!(fs::read(&path).unwrap(), original);
    }
    #[test]
    fn corrupted_packages_remain_visible_and_can_be_removed_then_reinstalled() {
        let temp = Temp::new();
        let mut manager = Manager::new(temp.0.clone());
        manager.pending = Some(pending());
        manager.install_pending().unwrap();
        let path = manager.installed["org.example.test"].path.clone();
        fs::write(&path, b"changed package").unwrap();

        let mut manager = Manager::new(temp.0.clone());
        assert!(manager.installed.is_empty());
        assert_eq!(manager.rejected.len(), 1);
        let state = manager.inspection();
        let row = state["rejected"].as_array().unwrap().first().unwrap();
        assert_eq!(row["filename"], "org.example.test.fcplugin");
        assert_eq!(row["removable"], true);
        assert!(row["error"].as_str().unwrap().contains("changed"));
        let rejected_id = row["id"].as_str().unwrap();
        assert!(manager.open_extension(rejected_id, egui::Context::default()).is_err());
        manager.pending = Some(pending());
        assert!(manager.install_pending().unwrap_err().contains("Remove"));
        manager.remove(rejected_id).unwrap();
        assert!(manager.rejected.is_empty());
        assert!(!path.exists());
        assert!(!receipt_path(&path).exists());
        manager.install_pending().unwrap();
        assert_eq!(Manager::new(temp.0.clone()).installed.len(), 1);
    }

    #[test]
    fn interrupted_receipt_writes_do_not_block_toggles_and_failed_renames_clean_up() {
        let temp = Temp::new();
        let mut manager = Manager::new(temp.0.clone());
        manager.pending = Some(pending());
        manager.install_pending().unwrap();
        let path = manager.installed["org.example.test"].path.clone();
        let stranded = path.with_extension("receipt.pending");
        fs::write(&stranded, b"interrupted previous save").unwrap();
        manager.enable("org.example.test", false).unwrap();
        assert!(!Manager::new(temp.0.clone()).installed["org.example.test"].receipt.enabled);
        manager.enable("org.example.test", true).unwrap();
        assert!(Manager::new(temp.0.clone()).installed["org.example.test"].receipt.enabled);
        assert_eq!(fs::read(&stranded).unwrap(), b"interrupted previous save");
        let before: std::collections::BTreeSet<_> = fs::read_dir(&manager.root).unwrap().map(|entry| entry.unwrap().path()).collect();
        fs::remove_file(receipt_path(&path)).unwrap();
        fs::create_dir(receipt_path(&path)).unwrap();
        assert!(manager.enable("org.example.test", false).is_err());
        let after: std::collections::BTreeSet<_> = fs::read_dir(&manager.root).unwrap().map(|entry| entry.unwrap().path()).collect();
        assert_eq!(before, after, "failed receipt writes must clean up their unique temporary file");
        assert!(manager.installed["org.example.test"].receipt.enabled, "a failed disk update must not change the current grant");
    }

    #[test]
    fn directories_are_reported_but_never_recursively_removed() {
        let temp = Temp::new();
        let root = temp.0.join("Extensions");
        let directory = root.join("org.example.test.fcplugin");
        fs::create_dir_all(&directory).unwrap();
        let sentinel = directory.join("keep.txt");
        fs::write(&sentinel, b"unrelated data").unwrap();
        let mut manager = Manager::new(temp.0.clone());
        assert!(manager.installed.is_empty());
        let id = manager.rejected.keys().next().unwrap().clone();
        assert!(!manager.rejected[&id].removable);
        assert!(manager.remove(&id).unwrap_err().contains("directory"));
        assert_eq!(fs::read(sentinel).unwrap(), b"unrelated data");
    }

    #[cfg(unix)]
    #[test]
    fn special_files_are_rejected_and_symlink_removal_preserves_the_target() {
        use std::os::unix::{fs::symlink, net::UnixListener};
        let temp = Temp::new();
        let root = temp.0.join("Extensions");
        fs::create_dir_all(&root).unwrap();
        let socket_path = root.join("special.fcplugin");
        let _listener = UnixListener::bind(&socket_path).unwrap();
        assert!(read_bytes(&socket_path, 4096).unwrap_err().contains("regular files"));
        let target = temp.0.join("outside.fcplugin");
        fs::write(&target, b"preserve target").unwrap();
        let link = root.join("linked.fcplugin");
        symlink(&target, &link).unwrap();
        let mut manager = Manager::new(temp.0.clone());
        assert!(manager.installed.is_empty());
        assert_eq!(manager.rejected.len(), 2);
        let ids: Vec<_> = manager.rejected.keys().cloned().collect();
        for id in ids {
            manager.remove(&id).unwrap();
        }
        assert!(!socket_path.exists());
        assert!(!link.exists());
        assert_eq!(fs::read(target).unwrap(), b"preserve target");
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_helper_reopens_the_manager_with_an_error() {
        let temp = Temp::new();
        let mut manager = Manager::new(temp.0.clone());
        manager.pending = Some(pending());
        manager.install_pending().unwrap();
        manager.open = false;
        let mut child = ManagedChild::spawn(Command::new("/bin/sh").args(["-c", "exit 7"])).unwrap();
        child.child_mut().unwrap().wait().unwrap();
        let (_, incoming) = mpsc::sync_channel(1);
        let (outgoing, _) = mpsc::sync_channel(1);
        manager
            .running
            .insert("org.example.test".into(), Running { child, incoming, outgoing, context: Value::Null, theme: Value::Null, disconnected_at: None });
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        manager.poll(&mut app, &egui::Context::default());
        assert!(manager.running.is_empty());
        assert!(manager.open);
        assert!(manager.error.as_deref().unwrap().contains("closed unexpectedly"));
    }

    #[cfg(unix)]
    #[test]
    fn dropping_a_managed_window_terminates_and_reaps_its_child() {
        let mut child = ManagedChild::spawn(Command::new("/bin/sleep").arg("60")).unwrap();
        let pid = child.child_mut().unwrap().id().to_string();
        drop(child);
        // kill -0 also finds a zombie, so disappearance proves the reaper ran.
        for _ in 0..200 {
            let exists = Command::new("/bin/kill").args(["-0", &pid]).stdout(Stdio::null()).stderr(Stdio::null()).status().unwrap().success();
            if !exists {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("The closed extension process was not reaped");
    }
}

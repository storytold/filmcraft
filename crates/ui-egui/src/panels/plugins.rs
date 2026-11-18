//! Window ▸ Plug-ins and its windows: the installed plug-ins' commands, the Plug-ins window
//! (installed plug-ins, installing from a file, a GitHub repository or a catalog, updates), the
//! install confirmation (permissions, kind, source, SHA-256) and the parameter dialog generated
//! from a plug-in command's `params`. Everything goes through the engine's `plugin.*` commands
//! ([`filmcraft_engine::plugin`], RFC-0046). Downloads run on a worker thread under
//! `catch_unwind`, so the interface keeps drawing. Plug-in names and command labels are the
//! plug-in's own and are not translated.

use std::sync::mpsc::{Receiver, TryRecvError, channel};

use egui::RichText;
use filmcraft_engine::plugin::craft_plugin::{Access, Param, ParamKind, PluginCommand};
use filmcraft_engine::plugin::{self, Candidate, InstalledPlugin};
use serde_json::{Map, Value, json};

use crate::FilmcraftApp;
use crate::menus::MenuItem;
use crate::native_dialogs::FileDialog;
use crate::widgets::CenteredDialog;

/// Window ▸ Plug-ins ▸ Manage Plug-ins…: the Plug-ins window.
pub const MANAGE: &str = "plugins.manage";
/// A plug-in command's menu item: `plugins.run:<plugin id>:<command id>` (neither id holds a `:`).
const RUN: &str = "plugins.run:";
/// The Window submenu that lists the plug-ins (built at run time, from what is installed).
const MENU: &str = "Plug-ins";
/// Width of the repository and catalog address fields (points).
const ADDRESS_WIDTH: f32 = 260.0;
/// Width of a text parameter's field (points).
const TEXT_FIELD_WIDTH: f32 = 240.0;

/// The install confirmation's content: the `plugin.install` parameters that named the bundle, and
/// what its review said about it.
#[derive(Clone, Debug)]
pub struct Review {
    pub request: Value,
    pub info: Value,
}

impl Review {
    /// Where the plug-in comes from, in a line.
    pub fn source_text(&self) -> String {
        let source = &self.info["source"];
        let t = |k: &str| source[k].as_str().unwrap_or_default();
        match t("type") {
            "github" => format!("github.com/{} {} ({})", t("repo"), t("tag"), t("asset")),
            "catalog" => format!("{} · {}", t("package"), t("catalog")),
            "other" => format!("{} · {}", t("kind"), t("location")),
            _ => t("path").to_string(),
        }
    }
}

/// A plug-in command waiting for its parameters.
#[derive(Clone, Debug)]
pub struct ParamsDialog {
    pub plugin: String,
    pub command: String,
    pub title: String,
    pub params: Vec<Param>,
    pub values: Map<String, Value>,
}

impl ParamsDialog {
    /// The dialog for `command` of plug-in `plugin` (named `name`), with each parameter's default.
    pub fn new(plugin: &str, name: &str, command: &PluginCommand) -> Self {
        ParamsDialog {
            plugin: plugin.to_string(),
            command: command.id.clone(),
            title: format!("{name}: {}", command.label.trim_end_matches(['…', '.'])),
            params: command.params.clone(),
            values: command.params.iter().map(|p| (p.name.clone(), p.default_value())).collect(),
        }
    }

    /// The `plugin.run` parameters with the values entered.
    pub fn run_params(&self) -> Value {
        json!({"id": self.plugin, "command": self.command, "params": self.values})
    }
}

/// What a download worker brings back.
enum Done {
    /// The bundle `request` named, or why it couldn't be had.
    Downloaded(Value, Result<Candidate, String>),
    /// The catalogs' packages and their warnings.
    Packages(Vec<Value>, Vec<String>),
    /// The updates found, and the plug-ins that couldn't be checked.
    Updates(Vec<Value>, Vec<String>),
    /// The worker stopped with an internal error.
    Failed(String),
}

/// The Plug-ins window's state, the install confirmation, the parameter dialog and the download
/// in progress.
#[derive(Default)]
pub struct PluginsUi {
    /// The Plug-ins window shows.
    pub open: bool,
    /// The GitHub repository typed (`owner/repo` or `owner/repo@tag`).
    repo: String,
    /// The catalog address typed.
    catalog_url: String,
    /// The last action's message.
    pub status: String,
    /// The catalogs' packages, after Browse Catalogs.
    packages: Vec<Value>,
    /// The updates found, after Check for Updates.
    updates: Vec<Value>,
    /// The install confirmation, while it waits for an answer.
    pub review: Option<Review>,
    /// A plug-in command's parameters, while they are entered.
    pub params: Option<ParamsDialog>,
    /// The download running on a worker thread.
    task: Option<Receiver<Done>>,
}

impl PluginsUi {
    /// Whether a download runs (the network buttons wait for it).
    pub fn busy(&self) -> bool {
        self.task.is_some()
    }
}

// ---------- menu and commands ----------

/// Window ▸ Plug-ins: each installed plug-in's commands (a submenu for a plug-in with several),
/// then Manage Plug-ins….
pub fn menu(app: &FilmcraftApp, items: &mut Vec<MenuItem>) {
    let item = |label: &str, id: String, path: Vec<String>| MenuItem { id, label: label.to_string(), path, shortcut: None, enabled: true, checked: None };
    let base = vec!["Window".to_string(), MENU.to_string()];
    for p in app.session.plugins.list() {
        let m = &p.manifest;
        match m.commands.as_slice() {
            [] => {}
            [one] => items.push(item(&m.name, format!("{RUN}{}:{}", m.id, one.id), base.clone())),
            many => {
                let mut path = base.clone();
                path.push(m.name.clone());
                items.extend(many.iter().map(|c| item(&c.label, format!("{RUN}{}:{}", m.id, c.id), path.clone())));
            }
        }
    }
    items.push(item(app.ui.language.tr("Manage Plug-ins…"), MANAGE.into(), base));
}

/// The UI commands of this module: Manage Plug-ins… and each plug-in command's menu item. A
/// command's item with `params` runs it with them; without, a command that declares parameters
/// asks for them first.
pub fn route(app: &mut FilmcraftApp, id: &str, params: &Value) -> Option<Result<Value, String>> {
    if id == MANAGE {
        app.plugins.open = true;
        return Some(Ok(json!({"dialog": "plugins"})));
    }
    let (plugin, command) = id.strip_prefix(RUN)?.split_once(':')?;
    let given = params.get("params").cloned();
    Some(run_command(app, plugin, command, given))
}

/// Run a plug-in command, after its parameter dialog when it declares parameters and none are
/// `given`.
pub fn run_command(app: &mut FilmcraftApp, id: &str, command: &str, given: Option<Value>) -> Result<Value, String> {
    let p = app.session.plugins.get(id).ok_or_else(|| format!("no plug-in `{id}` is installed"))?;
    let c = p.manifest.command(Some(command)).ok_or_else(|| format!("{} has no command `{command}`", p.manifest.name))?;
    let run = json!({"id": p.manifest.id, "command": c.id, "params": given.clone().unwrap_or_else(|| json!({}))});
    if c.params.is_empty() || given.is_some() {
        return run_plugin(app, run);
    }
    app.plugins.params = Some(ParamsDialog::new(&p.manifest.id, &p.manifest.name, c));
    Ok(json!({"dialog": "pluginParams"}))
}

/// `plugin.run`, its notes (or that it is done) on the status line.
fn run_plugin(app: &mut FilmcraftApp, params: Value) -> Result<Value, String> {
    let name = params["id"].as_str().and_then(|id| app.session.plugins.get(id)).map(|p| p.manifest.name.clone()).unwrap_or_default();
    let r = app.session.execute("plugin.run", params).map_err(|e| e.to_string());
    match &r {
        Ok(out) => {
            let notes: Vec<&str> = out["notes"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
            let mut line = if notes.is_empty() { tlf!("{name}: done", name) } else { notes.join(" · ") };
            if out["panel"].is_string() {
                line = format!("{line} ({})", tl!("FilmCraft doesn't show plug-in panels yet"));
            }
            app.ui.status = line;
        }
        Err(e) => app.ui.status = e.clone(),
    }
    r
}

// ---------- reviewing and installing ----------

/// Check the bundle `request` names and show its confirmation: a file or data at once, a
/// repository or a catalog's package after it downloads on a worker.
fn review(app: &mut FilmcraftApp, ctx: &egui::Context, request: Value) {
    if request.get("path").is_some() || request.get("data").is_some() {
        review_local(app, request);
        return;
    }
    let fetcher = app.session.plugins.fetcher();
    let catalogs = app.session.prefs.plugin_data.catalogs.clone();
    start(app, ctx, move || {
        let got = fetcher.with(|f| plugin::download(f, &catalogs, &request));
        Done::Downloaded(request, got)
    });
}

/// [`review`] of a file or base64 data (`plugin.install {…, review: true}`).
fn review_local(app: &mut FilmcraftApp, request: Value) {
    let mut asked = request.clone();
    asked["review"] = json!(true);
    match app.session.execute("plugin.install", asked) {
        Ok(info) => {
            app.plugins.review = Some(Review { request, info });
            app.plugins.status.clear();
        }
        Err(e) => app.plugins.status = e.to_string(),
    }
}

/// Install the reviewed bundle (the one with the reviewed SHA-256 only, which the review kept).
fn install_reviewed(app: &mut FilmcraftApp, reviewed: Review) {
    let mut request = reviewed.request;
    request["sha256"] = reviewed.info["sha256"].clone();
    app.plugins.status = match app.session.execute("plugin.install", request) {
        Ok(_) => tlf!("Installed {name}.", name = reviewed.info["name"].as_str().unwrap_or_default()),
        Err(e) => e.to_string(),
    };
}

/// Browse Catalogs, on a worker.
fn browse(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let fetcher = app.session.plugins.fetcher();
    let urls = app.session.prefs.plugin_data.catalogs.clone();
    let installed: Vec<InstalledPlugin> = app.session.plugins.list().to_vec();
    start(app, ctx, move || {
        let (mut packages, mut warnings) = (Vec::new(), Vec::new());
        for url in &urls {
            match fetcher.with(|f| plugin::catalog_packages_with(f, url, &installed)) {
                Ok((found, w)) => {
                    packages.extend(found);
                    warnings.extend(w.into_iter().map(|w| format!("{url}: {w}")));
                }
                Err(e) => warnings.push(format!("{url}: {e}")),
            }
        }
        Done::Packages(packages, warnings)
    });
}

/// Check for Updates, on a worker.
fn check_updates(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let fetcher = app.session.plugins.fetcher();
    let installed: Vec<InstalledPlugin> = app.session.plugins.list().to_vec();
    start(app, ctx, move || {
        let (mut updates, mut errors) = (Vec::new(), Vec::new());
        for p in &installed {
            match fetcher.with(|f| plugin::update_for_with(f, p)) {
                Ok(Some((install, available))) => {
                    updates.push(json!({"id": p.manifest.id, "installed": p.manifest.version, "available": available, "install": install}))
                }
                Ok(None) => {}
                Err(e) => errors.push(format!("{}: {e}", p.manifest.name)),
            }
        }
        Done::Updates(updates, errors)
    });
}

/// Run `work` on a worker thread under `catch_unwind` (at once on the web, which has none); its
/// result is applied on a later frame ([`poll`]).
fn start(app: &mut FilmcraftApp, ctx: &egui::Context, work: impl FnOnce() -> Done + Send + 'static) {
    if app.plugins.task.is_some() {
        app.plugins.status = tl!("A plug-in download is already running").into();
        return;
    }
    let (tx, rx) = channel();
    // the worker has no interface language of its own: its message is translated here
    let internal = tl!("internal error (see the crash log)").to_string();
    let guarded = move || std::panic::catch_unwind(std::panic::AssertUnwindSafe(work)).unwrap_or(Done::Failed(internal));
    #[cfg(not(target_arch = "wasm32"))]
    {
        let ctx = ctx.clone();
        let spawned = std::thread::Builder::new().name("plugin-download".into()).spawn(move || {
            // the receiver may be gone (the app closed): nothing to tell
            let _ = tx.send(guarded());
            ctx.request_repaint();
        });
        if let Err(e) = spawned {
            app.plugins.status = e.to_string();
            return;
        }
    }
    #[cfg(target_arch = "wasm32")]
    {
        let _ = ctx;
        // the receiver is alive (held below): sending can't fail
        let _ = tx.send(guarded());
    }
    app.plugins.task = Some(rx);
    app.plugins.status = tl!("Downloading…").into();
}

/// Apply what a finished worker brought back.
fn poll(app: &mut FilmcraftApp) {
    let Some(rx) = &app.plugins.task else { return };
    let done = match rx.try_recv() {
        Ok(done) => done,
        Err(TryRecvError::Empty) => return,
        Err(TryRecvError::Disconnected) => Done::Failed(tl!("the download stopped").into()),
    };
    app.plugins.task = None;
    app.plugins.status.clear();
    match done {
        Done::Downloaded(request, Ok(c)) => match plugin::offer(&mut app.session, &request, c) {
            Ok(info) => app.plugins.review = Some(Review { request, info }),
            Err(e) => app.plugins.status = e.to_string(),
        },
        Done::Downloaded(_, Err(e)) | Done::Failed(e) => app.plugins.status = e,
        Done::Packages(packages, warnings) => {
            app.plugins.packages = packages;
            app.plugins.status = warnings.join("\n");
        }
        Done::Updates(updates, errors) => {
            app.plugins.status = if updates.is_empty() && errors.is_empty() { tl!("Every plug-in is up to date.").into() } else { errors.join("\n") };
            app.plugins.updates = updates;
        }
    }
}

/// From File…: the `.craftplugin` picker, then the file's confirmation.
fn pick_file(app: &mut FilmcraftApp) {
    let dialog = FileDialog::OpenFile { filter: tl!("FilmCraft Plug-ins").into(), exts: vec!["craftplugin".into()] };
    app.pick_ui(dialog, |app, paths| {
        if let Some(path) = paths.into_iter().next() {
            review_local(app, json!({"path": path}));
        }
    });
}

// ---------- drawing ----------

/// The Plug-ins window, the install confirmation and the parameter dialog, when they show (every
/// frame), and the result of a finished download.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    poll(app);
    if app.plugins.open {
        manager(app, ctx);
    }
    if app.plugins.review.is_some() {
        confirmation(app, ctx);
    }
    if app.plugins.params.is_some() {
        parameters(app, ctx);
    }
}

/// A button registered for automation as `id`; whether it was clicked.
fn button(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, text: &str, enabled: bool) -> bool {
    let r = ui.add_enabled(enabled, egui::Button::new(text));
    app.auto.add_response(id, &r, text);
    r.clicked()
}

/// A one-line text field registered for automation as `id`.
fn field(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, label: &str, text: &mut String, hint: &str) {
    let r = ui.add(egui::TextEdit::singleline(text).desired_width(ADDRESS_WIDTH).hint_text(hint));
    app.auto.add_response(id, &r, label);
}

/// The Plug-ins window.
fn manager(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let mut open = true;
    egui::Window::new(tl!("Plug-ins")).id(egui::Id::unique("plugins-window")).open(&mut open).collapsible(false).default_width(480.0).centered(ctx).show(
        ctx,
        |ui| {
            egui::ScrollArea::vertical().auto_shrink([false, true]).max_height(520.0).show(ui, |ui| {
                installed(app, ui);
                ui.add_space(8.0);
                install_section(app, ui, ctx);
                ui.add_space(8.0);
                catalogs(app, ui, ctx);
                if !app.plugins.status.is_empty() {
                    ui.add_space(6.0);
                    ui.label(RichText::new(app.plugins.status.as_str()).color(app.tokens.text_dim));
                }
            });
        },
    );
    if !open || (crate::widgets::escape_closes(ctx) && app.plugins.review.is_none() && app.plugins.params.is_none()) {
        app.plugins.open = false;
    }
}

/// The installed plug-ins, their commands and problems.
fn installed(app: &mut FilmcraftApp, ui: &mut egui::Ui) {
    ui.strong(tl!("Installed"));
    let plugins: Vec<InstalledPlugin> = app.session.plugins.list().to_vec();
    let dim = app.tokens.text_dim;
    if plugins.is_empty() {
        ui.label(RichText::new(tl!("No plug-ins installed.")).color(dim));
    }
    for (i, p) in plugins.iter().enumerate() {
        let m = &p.manifest;
        ui.horizontal(|ui| {
            ui.label(RichText::new(format!("{} {}", m.name, m.version)).strong());
            let access = if m.permissions.access == Access::Write { tl!("edits the project") } else { tl!("reads the project") };
            ui.label(RichText::new(access).color(dim));
            if !p.stored {
                ui.label(RichText::new(tl!("this session only")).color(dim));
            }
        });
        ui.horizontal_wrapped(|ui| {
            for (j, c) in m.commands.iter().enumerate() {
                if button(app, ui, &format!("plugins.installed.{i}.command.{j}"), &c.label, true)
                    && let Err(e) = run_command(app, &m.id, &c.id, None)
                {
                    app.plugins.status = e;
                }
            }
            if button(app, ui, &format!("plugins.installed.{i}.remove"), tl!("Remove"), true) {
                app.plugins.status = app.session.execute("plugin.remove", json!({"id": m.id})).err().map(|e| e.to_string()).unwrap_or_default();
            }
        });
    }
    for problem in app.session.plugins.problems().to_vec() {
        ui.label(RichText::new(problem).color(dim));
    }
}

/// Install: from a file, or a GitHub repository.
fn install_section(app: &mut FilmcraftApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.strong(tl!("Install"));
    if button(app, ui, "plugins.install.file", tl!("From File…"), true) {
        pick_file(app);
    }
    let busy = app.plugins.busy();
    ui.horizontal(|ui| {
        ui.label(tl!("GitHub repository"));
        let mut repo = std::mem::take(&mut app.plugins.repo);
        field(app, ui, "plugins.install.repo", "GitHub repository", &mut repo, "owner/repo");
        let ready = !repo.trim().is_empty() && !busy;
        if button(app, ui, "plugins.install.repo.review", tl!("Review…"), ready) {
            review(app, ctx, json!({"repo": repo.trim()}));
        }
        app.plugins.repo = repo;
    });
}

/// The catalogs added, browsing them and checking for updates.
fn catalogs(app: &mut FilmcraftApp, ui: &mut egui::Ui, ctx: &egui::Context) {
    ui.strong(tl!("Catalogs"));
    for (i, url) in app.session.prefs.plugin_data.catalogs.clone().into_iter().enumerate() {
        ui.horizontal(|ui| {
            ui.label(url.as_str());
            if button(app, ui, &format!("plugins.catalog.{i}.remove"), tl!("Remove"), true) {
                app.plugins.status =
                    app.session.execute("plugin.catalog.add", json!({"url": url, "remove": true})).err().map(|e| e.to_string()).unwrap_or_default();
            }
        });
    }
    ui.horizontal(|ui| {
        let mut url = std::mem::take(&mut app.plugins.catalog_url);
        field(app, ui, "plugins.catalog.url", "Catalog address", &mut url, "https://raw.githubusercontent.com/…/index.json");
        if button(app, ui, "plugins.catalog.add", tl!("Add"), !url.trim().is_empty()) {
            match app.session.execute("plugin.catalog.add", json!({"url": url.trim()})) {
                Ok(_) => url.clear(),
                Err(e) => app.plugins.status = e.to_string(),
            }
        }
        app.plugins.catalog_url = url;
    });
    let busy = app.plugins.busy();
    ui.horizontal(|ui| {
        if button(app, ui, "plugins.catalog.browse", tl!("Browse Catalogs"), !busy && !app.session.prefs.plugin_data.catalogs.is_empty()) {
            browse(app, ctx);
        }
        if button(app, ui, "plugins.updates.check", tl!("Check for Updates"), !busy && !app.session.plugins.list().is_empty()) {
            check_updates(app, ctx);
        }
    });
    packages(app, ui, ctx, busy);
    updates(app, ui, ctx, busy);
}

/// The packages Browse Catalogs found.
fn packages(app: &mut FilmcraftApp, ui: &mut egui::Ui, ctx: &egui::Context, busy: bool) {
    let dim = app.tokens.text_dim;
    for (i, pkg) in app.plugins.packages.clone().into_iter().enumerate() {
        ui.horizontal(|ui| {
            let installed = pkg["installed"].as_str().map(|v| format!(" ({})", tlf!("installed: {version}", version = v))).unwrap_or_default();
            ui.label(format!("{} {}{installed}", pkg["name"].as_str().unwrap_or_default(), pkg["version"].as_str().unwrap_or_default()));
            if button(app, ui, &format!("plugins.package.{i}.install"), tl!("Install…"), !busy) {
                review(app, ctx, json!({"catalog": pkg["catalog"], "id": pkg["id"]}));
            }
        });
        if let Some(text) = pkg["description"].as_str().filter(|t| !t.is_empty()) {
            ui.label(RichText::new(text).color(dim));
        }
    }
}

/// The updates Check for Updates found.
fn updates(app: &mut FilmcraftApp, ui: &mut egui::Ui, ctx: &egui::Context, busy: bool) {
    for (i, u) in app.plugins.updates.clone().into_iter().enumerate() {
        ui.horizontal(|ui| {
            let text = |k: &str| u[k].as_str().unwrap_or_default().to_string();
            ui.label(format!("{} {} → {}", text("id"), text("installed"), text("available")));
            if button(app, ui, &format!("plugins.update.{i}"), tl!("Update…"), !busy) {
                review(app, ctx, u["install"].clone());
            }
        });
    }
}

/// The install confirmation: nothing is written before Install.
fn confirmation(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(reviewed) = app.plugins.review.clone() else { return };
    let mut open = true;
    let mut answer = None;
    egui::Window::new(tl!("Install Plug-in")).id(egui::Id::unique("plugin-install")).open(&mut open).collapsible(false).resizable(false).centered(ctx).show(
        ctx,
        |ui| {
            confirmation_body(app, ui, &reviewed);
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if button(app, ui, "plugins.confirm.install", tl!("Install"), true) {
                    answer = Some(true);
                }
                if button(app, ui, "plugins.confirm.cancel", tl!("Cancel"), true) {
                    answer = Some(false);
                }
            });
        },
    );
    if !open || answer.is_some() || crate::widgets::escape_closes(ctx) {
        app.plugins.review = None;
    }
    if answer == Some(true) {
        install_reviewed(app, reviewed);
    }
}

/// What the confirmation says: the plug-in, its kind, access and commands, where it comes from
/// and its SHA-256, and warnings.
fn confirmation_body(app: &FilmcraftApp, ui: &mut egui::Ui, reviewed: &Review) {
    let info = &reviewed.info;
    let text = |k: &str| info[k].as_str().unwrap_or_default().to_string();
    let dim = app.tokens.text_dim;
    ui.heading(format!("{} {}", text("name"), text("version")));
    for k in ["author", "description", "license", "repo"] {
        if !text(k).is_empty() {
            ui.label(text(k));
        }
    }
    egui::Grid::new("plugin-install-grid").num_columns(2).spacing([12.0, 4.0]).show(ui, |ui| {
        ui.label(tl!("Kind"));
        ui.label(text("kind"));
        ui.end_row();
        ui.label(tl!("Access"));
        let write = info["permissions"]["access"] == "write";
        ui.label(if write { tl!("Reads and edits the open project") } else { tl!("Reads the open project") });
        ui.end_row();
        ui.label(tl!("Commands"));
        let allowed: Vec<&str> = info["permissions"]["commands"].as_array().map(|a| a.iter().filter_map(Value::as_str).collect()).unwrap_or_default();
        ui.label(if allowed.is_empty() { tl!("Any command its access allows").to_string() } else { allowed.join(", ") });
        ui.end_row();
        if info["permissions"]["saveFile"] == true {
            ui.label(tl!("Files"));
            ui.label(tl!("Asks to save files (FilmCraft doesn't offer this yet)"));
            ui.end_row();
        }
        ui.label(tl!("Source"));
        ui.label(reviewed.source_text());
        ui.end_row();
        ui.label("SHA-256");
        ui.add(egui::Label::new(RichText::new(text("sha256")).monospace()).wrap());
        ui.end_row();
    });
    if info["inCatalog"] == false {
        ui.label(RichText::new(tl!("This plug-in isn't in any catalog you use: install it only if you trust its author.")).strong());
    }
    if let Some(old) = info["replaces"].as_str() {
        ui.label(RichText::new(tlf!("Replaces version {version}.", version = old)).color(dim));
    }
    if info["permissionsChanged"] == true {
        ui.label(RichText::new(tl!("This version asks for different permissions.")).strong());
    }
    if info["stored"] == false {
        ui.label(RichText::new(tl!("It is kept for this session only.")).color(dim));
    }
}

/// A plug-in command's parameter dialog.
fn parameters(app: &mut FilmcraftApp, ctx: &egui::Context) {
    let Some(mut dialog) = app.plugins.params.take() else { return };
    let mut open = true;
    let mut answer = None;
    egui::Window::new(dialog.title.clone()).id(egui::Id::unique("plugin-params")).open(&mut open).collapsible(false).resizable(false).centered(ctx).show(
        ctx,
        |ui| {
            egui::Grid::new("plugin-params-grid").num_columns(2).spacing([12.0, 6.0]).show(ui, |ui| {
                for p in &dialog.params {
                    ui.label(p.title());
                    let v = dialog.values.entry(p.name.clone()).or_insert_with(|| p.default_value());
                    let r = param_field(ui, p, v);
                    app.auto.add_response(&format!("plugins.params.{}", p.name), &r, p.title());
                    ui.end_row();
                }
            });
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                if button(app, ui, "plugins.params.run", tl!("Run"), true) {
                    answer = Some(true);
                }
                if button(app, ui, "plugins.params.cancel", tl!("Cancel"), true) {
                    answer = Some(false);
                }
            });
        },
    );
    if open && answer.is_none() && !crate::widgets::escape_closes(ctx) {
        app.plugins.params = Some(dialog);
        return;
    }
    if answer == Some(true) {
        // its outcome is on the status line
        let _ = run_plugin(app, dialog.run_params());
    }
}

/// One parameter's control, editing `v`.
fn param_field(ui: &mut egui::Ui, p: &Param, v: &mut Value) -> egui::Response {
    match p.kind {
        ParamKind::Number | ParamKind::Slider => number_field(ui, p, v),
        ParamKind::Boolean => {
            let mut b = v.as_bool().unwrap_or(false);
            let r = ui.checkbox(&mut b, "");
            if r.changed() {
                *v = json!(b);
            }
            r
        }
        ParamKind::Select | ParamKind::List => {
            let current = v.as_str().unwrap_or_default().to_string();
            let r = craft_ui::ComboBox::from_id_salt(("plugin-param", &p.name)).selected_text(current.clone()).show_ui(ui, |ui| {
                for o in &p.options {
                    if ui.selectable_label(*o == current, o.as_str()).clicked() {
                        *v = json!(o);
                    }
                }
            });
            r.response
        }
        // Text, and a colour as `#RRGGBB`
        _ => {
            let mut s = v.as_str().unwrap_or_default().to_string();
            let r = ui.add(egui::TextEdit::singleline(&mut s).desired_width(TEXT_FIELD_WIDTH));
            if r.changed() {
                *v = json!(s);
            }
            r
        }
    }
}

/// A number parameter: a slider between its bounds, else a drag value (within them when given).
fn number_field(ui: &mut egui::Ui, p: &Param, v: &mut Value) -> egui::Response {
    let mut n = v.as_f64().unwrap_or(0.0);
    let r = match (p.kind, p.min, p.max) {
        (ParamKind::Slider, Some(lo), Some(hi)) if lo < hi => ui.add(egui::Slider::new(&mut n, lo..=hi)),
        _ => {
            let mut drag = egui::DragValue::new(&mut n);
            if let (Some(lo), Some(hi)) = (p.min, p.max)
                && lo <= hi
            {
                drag = drag.range(lo..=hi);
            }
            ui.add(drag)
        }
    };
    if r.changed() && n.is_finite() {
        *v = json!(n);
    }
    r
}

#[cfg(test)]
mod tests {
    use filmcraft_engine::Session;
    use filmcraft_engine::plugin::craft_plugin::bundle::Bundle;

    use super::*;

    /// A plug-in whose commands each add a video track; `asks` declares a parameter.
    fn install(app: &mut FilmcraftApp) {
        let wasm = wat::parse_str(
            r#"(module
              (import "craft" "call" (func $call (param i32 i32 i32 i32) (result i32)))
              (memory (export "memory") 1)
              (data (i32.const 0) "sequence.addTracks")
              (data (i32.const 32) "{\"video\":1,\"audio\":0}")
              (func (export "craft_run") (param i32) (result i32)
                (drop (call $call (i32.const 0) (i32.const 18) (i32.const 32) (i32.const 21))) (i32.const 0)))"#,
        )
        .unwrap();
        let manifest = json!({"id": "io.example.ui", "name": "UI Test", "version": "1.0.0", "main": "p.wasm",
            "apps": {"filmcraft": {"api": 6}}, "permissions": {"access": "write"},
            "commands": [{"id": "plain", "label": "Plain"},
                         {"id": "asks", "label": "Asks…", "params": [{"name": "count", "type": "number", "default": 2, "min": 1, "max": 9}]}]});
        let bytes = Bundle { manifest: manifest.to_string(), wasm, assets: vec![] }.encode();
        let data = filmcraft_project::gtemplate::base64_encode(&bytes);
        // Review, then install what was reviewed, as the confirmation does
        review_local(app, json!({"data": data}));
        let reviewed = app.plugins.review.take().unwrap_or_else(|| panic!("{}", app.plugins.status));
        assert_eq!(reviewed.info["permissions"]["access"], "write");
        assert_eq!(reviewed.source_text(), "data");
        install_reviewed(app, reviewed);
    }

    fn demo_app() -> FilmcraftApp {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        FilmcraftApp::new(s)
    }

    fn tracks(app: &FilmcraftApp) -> usize {
        app.session.active_sequence().unwrap().video_tracks.len()
    }

    #[test]
    fn plugin_commands_are_in_the_window_menu_run_as_one_step_and_ask_for_their_parameters() {
        let mut app = demo_app();
        install(&mut app);
        let count = tracks(&app);
        let ctx = egui::Context::default();

        // Window ▸ Plug-ins lists the plug-in's commands, then Manage Plug-ins…
        let items = crate::menus::menu_items(&app);
        let mine: Vec<&MenuItem> = items.iter().filter(|i| i.path.get(1).map(String::as_str) == Some(MENU)).collect();
        assert_eq!(mine.len(), 3, "{mine:?}");
        assert_eq!(mine[0].path, ["Window", MENU, "UI Test"]);
        assert_eq!(mine.last().map(|i| i.id.as_str()), Some(MANAGE));

        // A command without parameters runs at once, as one undo step
        crate::menus::invoke(&mut app, &ctx, &mine[0].id.clone(), json!({})).unwrap();
        assert_eq!(tracks(&app), count + 1);
        assert_eq!(app.session.history.undo.last().map(|u| u.0.as_str()), Some("UI Test: Plain"));

        // One with parameters asks first
        let r = crate::menus::invoke(&mut app, &ctx, &mine[1].id.clone(), json!({})).unwrap();
        assert_eq!(r["dialog"], "pluginParams");
        let dialog = app.plugins.params.clone().unwrap();
        assert_eq!(dialog.values["count"], 2.0);
        assert_eq!(dialog.title, "UI Test: Asks");
        run_plugin(&mut app, dialog.run_params()).unwrap();
        assert_eq!(tracks(&app), count + 2);

        crate::menus::invoke(&mut app, &ctx, MANAGE, json!({})).unwrap();
        assert!(app.plugins.open);
    }

    #[test]
    fn a_repository_in_no_catalog_says_where_it_comes_from() {
        let reviewed = Review {
            request: json!({"repo": "example/x"}),
            info: json!({"source": {"type": "github", "repo": "example/x", "tag": "v1", "asset": "x.craftplugin"}, "inCatalog": false}),
        };
        assert_eq!(reviewed.source_text(), "github.com/example/x v1 (x.craftplugin)");
    }

    #[test]
    fn a_repository_review_downloads_on_a_worker_and_reports_no_network() {
        let mut app = demo_app();
        let ctx = egui::Context::default();
        review(&mut app, &ctx, json!({"repo": "example/tools"}));
        for _ in 0..500 {
            poll(&mut app);
            if !app.plugins.busy() {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        assert!(!app.plugins.busy());
        assert!(app.plugins.review.is_none());
        assert!(app.plugins.status.contains("no network access"), "{}", app.plugins.status);
    }

    #[test]
    fn the_plugins_window_and_its_dialogs_draw_and_register_their_widgets() {
        let mut app = demo_app();
        install(&mut app);
        run_command(&mut app, "io.example.ui", "asks", None).unwrap();
        app.plugins.open = true;
        app.plugins.review = Some(Review { request: json!({}), info: json!({"name": "X", "inCatalog": false, "source": {"type": "file", "path": "x"}}) });
        let ctx = egui::Context::default();
        let input = egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1600.0, 1000.0))), ..Default::default() };
        let mut output = ctx.run_ui(input, |ui| show(&mut app, ui.ctx()));
        output.textures_delta.clear();
        assert!(app.plugins.params.is_some() && app.plugins.review.is_some(), "nothing was answered");
        let ids: Vec<&str> = app.auto.elements.iter().map(|e| e.id.as_str()).collect();
        for id in [
            "plugins.install.file",
            "plugins.install.repo",
            "plugins.confirm.install",
            "plugins.params.count",
            "plugins.params.run",
            "plugins.installed.0.remove",
        ] {
            assert!(ids.contains(&id), "{id} in {ids:?}");
        }
    }
}

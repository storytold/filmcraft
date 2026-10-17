//! FilmCraft's in-app file dialogs: Save, Open Project, Import and folder picks without a
//! system picker.
//!
//! A host with a system picker sets the desktop hooks (`HostHooks::pick_save`, `pick_files`, …)
//! and this module draws nothing. The Android build has no rfd backend at all, so it offers its
//! app-private storage directory instead ([`HostHooks::app_files_dir`], resolved from
//! `AndroidApp::internal_data_path` by `apps/filmcraft`), and every
//! [`FileDialog`](crate::native_dialogs::FileDialog) is served here: project saves open in
//! `Projects/`, other saves (renders, frame stills, presets) in `Exports/`, Open Project lists
//! the saved projects (or browses for one anywhere below the root), folder picks and media
//! import browse the directory itself. The answer travels on the same channel a host's
//! non-blocking dialog uses ([`crate::native_dialogs::pick`]), so the frame loop keeps running
//! and the command continues on the frame it arrives.
//!
//! The browser walks **two roots**: that app-private directory, always, and shared internal
//! storage ([`HostHooks::shared_storage_root`], `/storage/emulated/0`) once Android's "All files
//! access" grant holds — offered as a switcher only when the host set that hook, so a desktop or
//! web host still draws the one-root dialog exactly as before. Which root is shown can never be
//! left: entries are only ever *chosen* from listings, never typed; a suggested folder outside
//! the root is clamped back inside it; and "Up one level" stops at *that* root's top — so no
//! answer can point at anything the host did not offer while the browser was inside it.
//! An existing file is never replaced without being asked (`overwrite`), since a save that
//! silently replaced a project would lose work.
//!
//! The whole module compiles on every platform (desktop and web hosts leave `app_files_dir`
//! `None` and keep their pickers), so the modals and the listings run in the host's own tests.

use std::path::{Path, PathBuf};
use std::sync::mpsc::{Receiver, Sender};

use egui::{Align2, Color32, CornerRadius, Frame, Margin, Pos2, Rect, RichText, Stroke, vec2};

use crate::icons::Icon;
use crate::native_dialogs::FileDialog;
use crate::theme::Tokens;
use crate::{FilmcraftApp, HostHooks};

/// Project extensions: what `Open Project` lists, what its Browse… filters on, and what a
/// project save writes.
const PROJECT_EXTS: [&str; 1] = ["fcproj"];

/// Where a save goes when no location was chosen: projects with their own folder, everything
/// else with the exports.
pub fn save_dir(base: &Path, project: bool) -> PathBuf {
    base.join(if project { "Projects" } else { "Exports" })
}

/// A user-typed name reduced to a single file name inside the dialog's folder: surrounding
/// whitespace and any typed directories are dropped, so the answer can never escape the folder
/// it was asked in. Empty (or separator-only) input falls back to `fallback`.
pub fn sanitize_name(input: &str, fallback: &str) -> String {
    let name = Path::new(input.trim()).file_name().and_then(|n| n.to_str()).unwrap_or("").trim().to_string();
    if name.is_empty() || name == "." || name == ".." { fallback.to_string() } else { name }
}

/// Project saves keep their extension: a name with `.fcproj` keeps it, anything else gains one
/// (the desktop dialog appends it too, #293), so a typed name can't save an unopenable file.
pub fn enforce_project_ext(name: &str) -> String {
    if name.split('.').nth(1).is_some_and(|e| e.eq_ignore_ascii_case("fcproj")) { name.to_string() } else { format!("{name}.fcproj") }
}

/// The full save path for a save into `dir` answered with `name`.
pub fn save_path_in(dir: &Path, project: bool, name: &str, fallback: &str) -> PathBuf {
    let mut name = sanitize_name(name, fallback);
    if project {
        name = enforce_project_ext(&name);
    }
    dir.join(name)
}

/// Saved projects, newest first. Unreadable directories (or entries) are skipped, never an error.
pub fn list_projects(base: &Path) -> Vec<PathBuf> {
    let mut out: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
    let Ok(entries) = std::fs::read_dir(base.join("Projects")) else { return Vec::new() };
    for entry in entries.flatten() {
        let path = entry.path();
        let is_project = path.extension().and_then(|e| e.to_str()).is_some_and(|e| PROJECT_EXTS.iter().any(|p| e.eq_ignore_ascii_case(p)));
        if is_project {
            let modified = entry.metadata().and_then(|m| m.modified()).unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            out.push((modified, path));
        }
    }
    out.sort_by_key(|a| std::cmp::Reverse(a.0));
    out.into_iter().map(|(_, p)| p).collect()
}

/// The sub-folders of `dir` for the browser, sorted case-insensitively. Unreadable directories
/// and non-directories are skipped, never an error.
pub fn list_dirs(dir: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries.flatten().filter_map(|e| e.path().is_dir().then(|| e.path())).collect();
    out.sort_by_key(|p| name_key(p));
    out
}

/// The files in `dir` whose extension is in `exts` (case-insensitive; an empty `exts` matches
/// every file), sorted case-insensitively. Directories and unreadable entries are skipped.
pub fn list_files(dir: &Path, exts: &[String]) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(dir) else { return Vec::new() };
    let mut out: Vec<PathBuf> = entries
        .flatten()
        .filter_map(|e| {
            let path = e.path();
            let matches = exts.is_empty() || path.extension().and_then(|x| x.to_str()).is_some_and(|x| exts.iter().any(|e| x.eq_ignore_ascii_case(e)));
            (path.is_file() && matches).then_some(path)
        })
        .collect();
    out.sort_by_key(|p| name_key(p));
    out
}

/// "Up one level" from `dir`: the parent while it stays inside the root, else the root itself —
/// the browser can never climb out of the directory the host offered.
pub fn up_from(root: &Path, dir: &Path) -> PathBuf {
    match dir.parent() {
        Some(parent) if dir.starts_with(root) && parent.starts_with(root) => parent.to_path_buf(),
        _ => root.to_path_buf(),
    }
}

/// Whether `dir` can be listed (it exists and is readable): an unreadable folder says so
/// instead of looking like an empty one.
pub fn readable(dir: &Path) -> bool {
    std::fs::read_dir(dir).is_ok()
}

/// Whether `p` lies inside a directory the browser may open on: the app-private base always, the
/// shared root when the host offers one. A path outside both is not a folder this dialog may
/// start at (it falls back to the base), which is what keeps a stale saved location honest.
pub fn inside_root(p: &Path, base: &Path, shared: Option<&Path>) -> bool {
    p.starts_with(base) || shared.is_some_and(|s| p.starts_with(s))
}

/// The root the browser is currently inside: the app-private base (always), or shared internal
/// storage while Android's "All files access" grant holds. Only *which* tree is shown changes —
/// [`up_from`] clamps to [`StorageRoot::dir`] the same way for both.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StorageRoot {
    /// The app-private directory ([`HostHooks::app_files_dir`]), always browsable.
    App(PathBuf),
    /// Shared internal storage ([`HostHooks::shared_storage_root`], `/storage/emulated/0`),
    /// browsable only while the "All files access" grant holds.
    Shared(PathBuf),
}

impl StorageRoot {
    /// The directory this root starts at; the bound "Up one level" may not cross.
    pub fn dir(&self) -> &Path {
        match self {
            StorageRoot::App(base) | StorageRoot::Shared(base) => base,
        }
    }
}

fn name_key(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().to_lowercase()).unwrap_or_default()
}

/// The file name shown for a path.
fn name_of(p: &Path) -> String {
    p.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_else(|| p.to_string_lossy().into_owned())
}

/// The dialog-local state of the in-app browser. The roots it may reach come from the host's
/// hooks ([`HostHooks::app_files_dir`], plus [`HostHooks::shared_storage_root`] when the host
/// offers a second one), not from here: the first can't change while a dialog is open (and if it
/// does, [`show`] cancels rather than stranding the command). Only *which* root the browser is
/// inside is state, and every dialog starts in [`StorageRoot::App`].
#[derive(Default)]
pub struct AndroidFileDraft {
    /// The request being served (`None`: no in-app dialog open).
    pub request: Option<FileDialog>,
    /// Who to answer: the chosen paths, empty when cancelled.
    answer: Option<Sender<Vec<String>>>,
    /// The directory the browser is open at (`None`: only a prompt is shown).
    browser: Option<PathBuf>,
    /// The root the browser is clamped to (`None`: [`StorageRoot::App`] at the host's storage
    /// directory — where every dialog starts, and the only root without a shared-storage hook).
    root: Option<StorageRoot>,
    /// The directory a save would write into (`None`: [`save_dir`] at the root).
    folder: Option<PathBuf>,
    /// What the file-name field holds while it is edited.
    name: String,
    /// The suggested name the field started with: what an empty field falls back to.
    fallback: String,
    /// What the new-folder field holds while it is edited.
    folder_name: String,
    /// Extensions the browser lists (empty: every file).
    exts: Vec<String>,
    /// Whether more than one file may be chosen.
    many: bool,
    /// The files chosen so far (the browser's checkmarks).
    selected: Vec<PathBuf>,
    /// A save that would replace an existing file, waiting for the answer (see [`show`]).
    confirm: Option<PathBuf>,
}

impl AndroidFileDraft {
    /// Serve `dialog` from the host's storage directory and return the channel its answer
    /// arrives on. `None`: this host offers no in-app storage (the desktop), so the caller keeps
    /// its own pickers.
    pub(crate) fn serve(&mut self, hooks: &HostHooks, dialog: &FileDialog) -> Option<Receiver<Vec<String>>> {
        let base = hooks.app_files_dir.clone()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut d = AndroidFileDraft { request: Some(dialog.clone()), answer: Some(tx), ..Default::default() };
        match dialog {
            FileDialog::SaveProject { name } => {
                let (folder, file) = suggested(&base, &PROJECT_EXTS, name);
                d.folder = Some(folder);
                d.name = file.clone();
                d.fallback = file;
                d.exts = PROJECT_EXTS.iter().map(|e| (*e).to_string()).collect();
            }
            FileDialog::SaveAs { exts, name, .. } => {
                let names: Vec<&str> = exts.iter().map(String::as_str).collect();
                let (folder, file) = suggested(&base, &names, name);
                d.folder = Some(folder);
                d.name = file.clone();
                d.fallback = file;
                d.exts = exts.clone();
            }
            FileDialog::OpenProject => {
                d.exts = PROJECT_EXTS.iter().map(|e| (*e).to_string()).collect();
                // the saved projects are the first thing to show; Browse… opens the browser
            }
            FileDialog::OpenFiles { exts } | FileDialog::OpenFile { exts, .. } | FileDialog::Relink { exts, .. } => {
                d.exts = exts.clone();
                d.many = matches!(dialog, FileDialog::OpenFiles { .. });
                d.browser = Some(base);
            }
            FileDialog::Folder { at } => {
                // A saved location inside *either* root is honored (a settings folder already
                // picked in shared storage stays picked); anything else starts at the app-private
                // base, as before.
                let shared = hooks.shared_storage_root.as_deref();
                d.browser = Some(at.as_deref().map(Path::new).filter(|p| p.is_dir() && inside_root(p, &base, shared)).map(Path::to_path_buf).unwrap_or(base));
            }
        }
        *self = d;
        Some(rx)
    }

    /// The pending request answered with `paths` (empty: cancelled): hand them to the command
    /// and close, so [`crate::FilmcraftApp::poll_native_dialog`] runs its continuation.
    fn finish(&mut self, paths: Vec<String>) {
        let draft = std::mem::take(self);
        if let Some(answer) = draft.answer {
            // the receiver is alive (held by the pending dialog): sending can't fail
            let _ = answer.send(paths);
        }
    }
}

/// A suggested save path split into the folder it names and the file name to show: the folder
/// only when it exists *inside* the root (a suggestion from outside is not a folder this dialog
/// may write to), else [`save_dir`].
fn suggested(base: &Path, exts: &[&str], path: &str) -> (PathBuf, String) {
    let p = Path::new(path);
    // a project-format save belongs with the projects, anything else with the exports
    let project = exts.iter().any(|e| e.eq_ignore_ascii_case("fcproj"));
    let folder =
        p.parent().filter(|d| !d.as_os_str().is_empty() && d.is_dir() && d.starts_with(base)).map(Path::to_path_buf).unwrap_or_else(|| save_dir(base, project));
    let file = p.file_name().and_then(|n| n.to_str()).filter(|n| !n.trim().is_empty()).map(str::to_string).unwrap_or_else(|| match exts.first() {
        Some(e) => format!("Untitled.{e}"),
        None => "Untitled".into(),
    });
    (folder, file)
}

/// The modal frame the other dialogs in this crate use (`panels::file_dialogs`).
fn modal_frame(t: &Tokens) -> Frame {
    Frame::new().fill(t.panel_bg).stroke(Stroke::new(1.0, t.separator)).corner_radius(CornerRadius::same(10)).inner_margin(Margin::same(0))
}

/// Draw `id` as a modal: the frame, a title, and `body` inside, at `width`. Returns what `body`
/// answered.
fn modal<R>(
    app: &mut FilmcraftApp,
    ctx: &egui::Context,
    id: &str,
    title: &str,
    width: f32,
    body: impl FnOnce(&mut FilmcraftApp, &mut egui::Ui) -> R,
) -> Option<R> {
    let t = app.tokens;
    let mut out = None;
    egui::Modal::new(egui::Id::new(id)).frame(modal_frame(&t)).show(ctx, |ui| {
        Frame::new().inner_margin(Margin { left: 20, right: 20, top: 16, bottom: 14 }).show(ui, |ui| {
            ui.set_width(width);
            ui.label(RichText::new(title).size(16.0).strong().color(t.text));
            ui.add_space(6.0);
            ui.separator();
            ui.add_space(6.0);
            out = Some(body(app, ui));
        });
    });
    out
}

/// A dialog width that fits the screen (a phone's window, a desktop's window alike).
fn width(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().width() - 40.0).clamp(300.0, 560.0)
}

/// The height a listing gets: the screen minus the title, the rows around it and the buttons.
fn list_height(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().height() - 300.0).clamp(140.0, 460.0)
}

/// Cut `text` to `max_w` with an ellipsis (measured, so a long name never runs off its row).
fn fit(ui: &egui::Ui, text: &str, max_w: f32) -> String {
    let font = Tokens::ui(13.5);
    let measure = |s: &str| ui.painter().layout_no_wrap(s.to_string(), font.clone(), Color32::WHITE).size().x;
    if measure(text) <= max_w {
        return text.to_string();
    }
    let mut keep = text.chars().count().saturating_sub(1);
    while keep > 0 && measure(&format!("{}…", text.chars().take(keep).collect::<String>())) > max_w {
        keep -= 1;
    }
    format!("{}…", text.chars().take(keep).collect::<String>())
}

/// One list row: a folder (its icon and name) or a file (its name, and the tick of a chosen
/// one), `ROW` tall so a finger can hit it. Returns whether it was tapped.
const ROW: f32 = 34.0;

fn row(app: &mut FilmcraftApp, ui: &mut egui::Ui, t: &Tokens, id: &str, label: &str, folder: bool, selected: bool) -> bool {
    let (rect, _) = ui.allocate_exact_size(vec2(ui.available_width(), ROW), egui::Sense::hover());
    let resp = ui.interact(rect, egui::Id::new(("androidFile", id)), egui::Sense::click());
    if selected {
        ui.painter().rect_filled(rect, 4.0, t.row_selected);
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, 4.0, t.hover);
    }
    app.auto.add(&format!("androidFile.{id}"), rect, label);
    let x = rect.left() + 8.0;
    let x = if folder {
        let r = Rect::from_center_size(Pos2::new(x + 9.0, rect.center().y), vec2(17.0, 15.0));
        crate::icons::paint(ui.painter(), r, Icon::Folder, Color32::from_rgb(237, 150, 58));
        r.right() + 8.0
    } else {
        x
    };
    let right = if selected {
        ui.painter().text(Pos2::new(rect.right() - 10.0, rect.center().y), Align2::RIGHT_CENTER, "✓", Tokens::ui(13.0), t.accent);
        rect.right() - 24.0
    } else {
        rect.right() - 8.0
    };
    let shown = fit(ui, label, (right - x).max(20.0));
    ui.painter().text(Pos2::new(x, rect.center().y), Align2::LEFT_CENTER, shown, Tokens::ui(13.5), t.text);
    resp.clicked()
}

/// Draw the in-app dialog the host's storage directory enabled, if one is open. Called every
/// frame from `panels::dialogs::show`, before `app.dialog` is consulted: the browser draws over
/// whatever else is open (Settings, a panel dialog), like a system picker would.
pub fn show(app: &mut FilmcraftApp, ctx: &egui::Context) {
    if app.file_dialogs.android_file.request.is_none() {
        return;
    }
    let Some(base) = app.hooks.app_files_dir.clone() else {
        // The directory went away while the prompt was open: cancel, don't strand the command.
        app.file_dialogs.android_file.finish(Vec::new());
        return;
    };
    let Some(request) = app.file_dialogs.android_file.request.clone() else { return };
    if let Some(dir) = app.file_dialogs.android_file.browser.clone() {
        browser(app, ctx, &base, dir, &request);
        return;
    }
    match request {
        FileDialog::SaveProject { .. } => save_prompt(app, ctx, &base, true),
        FileDialog::SaveAs { .. } => save_prompt(app, ctx, &base, false),
        FileDialog::OpenProject => open_project(app, ctx, &base),
        // `serve` opened the browser for these already; if it is not open, open it rather than
        // stranding the command that asked for it.
        FileDialog::OpenFiles { .. } | FileDialog::OpenFile { .. } | FileDialog::Relink { .. } | FileDialog::Folder { .. } => {
            app.file_dialogs.android_file.browser = Some(base);
        }
    }
}

/// The Save prompt: a file name and the folder it lands in, with a Location row whose
/// `Browse…` opens the browser. An existing file is only replaced after it is asked (`overwrite`).
fn save_prompt(app: &mut FilmcraftApp, ctx: &egui::Context, base: &Path, project: bool) {
    let title = if project { tl!("Save Project") } else { tl!("Save As…") };
    let folder = app.file_dialogs.android_file.folder.clone().unwrap_or_else(|| save_dir(base, project));
    let mut name = std::mem::take(&mut app.file_dialogs.android_file.name);
    let fallback = app.file_dialogs.android_file.fallback.clone();
    // A save that would replace something says so first (a desktop picker asks the same).
    if let Some(path) = app.file_dialogs.android_file.confirm.clone() {
        let label = name_of(&path);
        let mut overwrite = false;
        let mut cancel = false;
        if modal(app, ctx, "android-file-overwrite", title, width(ctx), |app, ui| {
            ui.label(RichText::new(tlf!("A file with this name already exists: {name}", name = label)).size(13.5).color(app.tokens.text));
            ui.add_space(4.0);
            ui.label(RichText::new(path.to_string_lossy()).size(11.5).color(app.tokens.text_dim));
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                overwrite = button(app, ui, "overwrite", tl!("Overwrite"), true);
                cancel = button(app, ui, "cancel", tl!("Cancel"), false);
            })
            .inner
        })
        .is_some()
        {
            if overwrite {
                let _ = std::fs::create_dir_all(&path);
                app.file_dialogs.android_file.finish(vec![path.to_string_lossy().into_owned()]);
            } else if cancel || crate::widgets::escape_closes(ctx) {
                app.file_dialogs.android_file.confirm = None;
            }
        }
        return;
    }
    let mut save = false;
    let mut cancel = false;
    let mut browse = false;
    if modal(app, ctx, "android-file-save", title, width(ctx), |app, ui| {
        let t = app.tokens;
        ui.label(RichText::new(tl!("Location")).size(11.5).color(t.text_dim));
        ui.label(RichText::new(fit(ui, &folder.to_string_lossy(), ui.available_width())).size(13.0).color(t.text));
        ui.add_space(6.0);
        let r = ui.add(egui::TextEdit::singleline(&mut name).hint_text(&fallback).desired_width(f32::INFINITY));
        app.auto.add("androidFile.name", r.rect, &fallback);
        ui.add_space(12.0);
        ui.horizontal(|ui| {
            browse = button(app, ui, "browse", tl!("Browse…"), false);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                cancel = button(app, ui, "cancel", tl!("Cancel"), false);
                save = button(app, ui, "save", tl!("Save"), true);
            });
        });
    })
    .is_some()
    {
        if save {
            let _ = std::fs::create_dir_all(&folder);
            let path = save_path_in(&folder, project, &name, &fallback);
            if path.exists() {
                app.file_dialogs.android_file.confirm = Some(path);
            } else {
                app.file_dialogs.android_file.finish(vec![path.to_string_lossy().into_owned()]);
            }
        } else if cancel || crate::widgets::escape_closes(ctx) {
            app.file_dialogs.android_file.finish(Vec::new());
        } else if browse {
            app.file_dialogs.android_file.folder = Some(folder.clone());
            app.file_dialogs.android_file.browser = Some(folder);
        }
    }
    // the field keeps what was typed (and the fallback it falls back to) for the next frame
    app.file_dialogs.android_file.name = name;
}

/// Open Project: the saved projects, newest first, and a Browse… into the browser for one
/// anywhere below the root.
fn open_project(app: &mut FilmcraftApp, ctx: &egui::Context, base: &Path) {
    let projects = list_projects(base);
    let mut open: Option<PathBuf> = None;
    let mut browse = false;
    let mut cancel = false;
    if modal(app, ctx, "android-file-open-project", tl!("Open Project…"), width(ctx), |app, ui| {
        let t = app.tokens;
        if projects.is_empty() {
            ui.label(RichText::new(tl!("No saved projects yet.")).size(13.0).color(t.text_dim));
        } else {
            egui::ScrollArea::vertical().id_salt("android-file-open-project-list").auto_shrink([false, false]).max_height(list_height(ctx)).show(ui, |ui| {
                for (i, p) in projects.iter().enumerate() {
                    let label = name_of(p);
                    if row(app, ui, &t, &format!("project.{i}"), &label, false, false) {
                        open = Some(p.clone());
                    }
                }
            });
        }
        ui.add_space(10.0);
        ui.horizontal(|ui| {
            browse = button(app, ui, "browse", tl!("Browse…"), false);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                cancel = button(app, ui, "cancel", tl!("Cancel"), false);
            });
        });
    })
    .is_some()
    {
        if let Some(path) = open {
            app.file_dialogs.android_file.finish(vec![path.to_string_lossy().into_owned()]);
        } else if browse {
            let start = save_dir(base, true);
            app.file_dialogs.android_file.browser = Some(if readable(&start) { start } else { base.to_path_buf() });
        } else if cancel || crate::widgets::escape_closes(ctx) {
            app.file_dialogs.android_file.finish(Vec::new());
        }
    }
}

/// The browser: the location with `Up one level` and a new-folder field, then the folders and
/// files of the directory. A file pick answers on a tap (or with Choose, for several); a folder
/// pick answers with the directory shown. A save's `Browse…` is a folder pick too, except that
/// it comes *back* to the prompt with the folder it landed on rather than answering the save.
fn browser(app: &mut FilmcraftApp, ctx: &egui::Context, base: &Path, dir: PathBuf, request: &FileDialog) {
    let exts = app.file_dialogs.android_file.exts.clone();
    let many = app.file_dialogs.android_file.many;
    let selected = app.file_dialogs.android_file.selected.clone();
    let mut folder_name = std::mem::take(&mut app.file_dialogs.android_file.folder_name);
    // The two roots: the app-private base always, and shared storage while the host offers it —
    // the grant is re-read every frame, so returning from Android's settings page is enough.
    let shared = app.hooks.shared_storage_root.clone();
    let granted = app.hooks.shared_storage_granted.as_ref().is_some_and(|g| g());
    let root = app.file_dialogs.android_file.root.clone().unwrap_or_else(|| StorageRoot::App(base.to_path_buf()));
    let in_shared = matches!(root, StorageRoot::Shared(_));
    let root_dir = root.dir().to_path_buf();
    // A save browses for the folder it lands in and a folder pick for the folder itself: there
    // the files are context, and the folders plus Choose are the answers.
    let picks_folder = matches!(request, FileDialog::Folder { .. } | FileDialog::SaveProject { .. } | FileDialog::SaveAs { .. });
    let is_file = !picks_folder;
    let projects_only = !exts.is_empty() && exts.iter().all(|e| e.eq_ignore_ascii_case("fcproj"));
    let title = match request {
        FileDialog::OpenFile { filter, .. } => filter.clone(),
        FileDialog::Folder { .. } | FileDialog::SaveProject { .. } | FileDialog::SaveAs { .. } => tl!("Choose Folder").to_string(),
        _ if projects_only => tl!("Open Project…").to_string(),
        _ => tl!("Media").to_string(),
    };
    let dirs = list_dirs(&dir);
    let files = list_files(&dir, &exts);
    let listed = readable(&dir);
    let up = up_from(&root_dir, &dir);
    // The switcher row takes the listing that much less of the screen.
    let list_h = list_height(ctx) - if shared.is_some() { 36.0 } else { 0.0 };

    let mut enter: Option<PathBuf> = None;
    let mut go_up = false;
    let mut pick: Option<PathBuf> = None;
    let mut toggle: Option<usize> = None;
    let mut choose = false;
    let mut cancel = false;
    let mut create = false;
    let mut to_app = false;
    let mut to_shared = false;
    let mut ask_grant = false;
    let mut grant = false;

    if modal(app, ctx, "android-file-browser", &title, width(ctx), |app, ui| {
        let t = app.tokens;
        // The location: where this listing is, and the way out of it.
        ui.horizontal(|ui| {
            go_up = button(app, ui, "up", tl!("Up one level"), false);
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                cancel = button(app, ui, "cancel", tl!("Cancel"), false);
            });
        });
        ui.label(RichText::new(fit(ui, &dir.to_string_lossy(), ui.available_width())).size(11.5).color(t.text_dim));
        // Which tree this listing is in: the app-private storage, or shared storage. The row
        // exists only while the host offers the second root (a desktop or web host draws
        // nothing), and an ungranted shared root asks for the grant instead of navigating.
        if shared.is_some() {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                let here = ui.selectable_label(!in_shared, tl!("App storage"));
                app.auto.add("androidFile.rootApp", here.rect, tl!("App storage"));
                if here.clicked() {
                    to_app = true;
                }
                let label = if granted { tl!("Shared storage") } else { tl!("Shared storage (grant access)") };
                let there = ui.selectable_label(in_shared, label);
                app.auto.add("androidFile.rootShared", there.rect, label);
                if there.clicked() {
                    if granted {
                        to_shared = true;
                    } else {
                        ask_grant = true;
                    }
                }
            });
        }
        ui.add_space(6.0);
        // A folder can be created here (an export's date folder, a project's shots).
        ui.horizontal(|ui| {
            let r = ui.add(egui::TextEdit::singleline(&mut folder_name).hint_text(tl!("Name")).desired_width((ui.available_width() - 96.0).max(80.0)));
            app.auto.add("androidFile.folderName", r.rect, tl!("New Folder"));
            let made = button(app, ui, "create", tl!("Create"), false);
            if made && !folder_name.trim().is_empty() {
                create = true;
            }
        });
        ui.add_space(6.0);
        if !listed {
            ui.label(RichText::new(tl!("This folder can't be read.")).size(13.0).color(t.text_dim));
            // In the shared root an unreadable listing is the grant that does not hold yet, not
            // an empty folder: offer the way to get it (the settings page, or why not).
            if in_shared && !granted {
                ui.add_space(4.0);
                grant = button(app, ui, "grant", tl!("Grant access"), true);
            }
        } else if dirs.is_empty() && files.is_empty() {
            ui.label(RichText::new(tl!("Nothing here.")).size(13.0).color(t.text_dim));
        } else {
            egui::ScrollArea::vertical().id_salt("android-file-list").auto_shrink([false, false]).max_height(list_h).show(ui, |ui| {
                for (i, d) in dirs.iter().enumerate() {
                    let label = name_of(d);
                    if row(app, ui, &t, &format!("dir.{i}"), &label, true, false) {
                        enter = Some(d.clone());
                    }
                }
                for (i, f) in files.iter().enumerate() {
                    let label = name_of(f);
                    // Where the folder is the answer, a file is context: tapping one must never
                    // answer a save (that would replace it without the overwrite prompt).
                    let tapped = row(app, ui, &t, &format!("file.{i}"), &label, false, is_file && selected.contains(f));
                    if tapped && is_file {
                        if many {
                            toggle = Some(i);
                        } else {
                            pick = Some(f.clone());
                        }
                    }
                }
            });
        }
        if !is_file || many {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if !is_file {
                        choose = button(app, ui, "choose", tl!("Choose"), true);
                    } else {
                        let n = selected.len();
                        let label = format!("{} ({n})", tl!("Choose"));
                        let r = ui.add_enabled(
                            n > 0,
                            egui::Button::new(RichText::new(label).size(13.0).color(Color32::WHITE))
                                .min_size(vec2(88.0, 28.0))
                                .corner_radius(CornerRadius::same(14))
                                .fill(t.accent),
                        );
                        app.auto.add("androidFile.choose", r.rect, tl!("Choose"));
                        if r.clicked() {
                            choose = true;
                        }
                    }
                });
            });
        }
    })
    .is_some()
    {
        // Switching roots starts at the new root's top. Asking for the grant leaves the browser
        // exactly where it is: Android re-enters the app from its own settings page and the
        // grant is re-read every frame, so the listing opens up on its own — no callback, and
        // a device that cannot ask just says so instead of pretending.
        if ask_grant || grant {
            if let Some(ask) = app.hooks.shared_storage_grant.as_ref() {
                ask();
                app.ui.status = tl!("Allow \"All files access\" for FilmCraft on the settings page, then tap Shared storage.").into();
            } else {
                app.ui.status = tl!("This build can't request shared-storage access.").into();
            }
        } else if to_app && in_shared {
            app.file_dialogs.android_file.root = Some(StorageRoot::App(base.to_path_buf()));
            app.file_dialogs.android_file.browser = Some(base.to_path_buf());
            app.file_dialogs.android_file.selected.clear();
        } else if to_shared && !in_shared {
            if let Some(sroot) = shared.clone() {
                app.file_dialogs.android_file.root = Some(StorageRoot::Shared(sroot.clone()));
                app.file_dialogs.android_file.browser = Some(sroot);
                app.file_dialogs.android_file.selected.clear();
            }
        } else if let Some(path) = enter {
            app.file_dialogs.android_file.browser = Some(path);
            app.file_dialogs.android_file.selected.clear();
        } else if go_up {
            app.file_dialogs.android_file.browser = Some(up);
            app.file_dialogs.android_file.selected.clear();
        } else if let Some(path) = pick {
            app.file_dialogs.android_file.finish(vec![path.to_string_lossy().into_owned()]);
        } else if let Some(i) = toggle {
            if let Some(path) = files.get(i) {
                if let Some(at) = app.file_dialogs.android_file.selected.iter().position(|p| p == path) {
                    app.file_dialogs.android_file.selected.remove(at);
                } else {
                    app.file_dialogs.android_file.selected.push(path.clone());
                }
            }
        } else if create {
            let name = sanitize_name(&folder_name, "");
            let made = dir.join(name);
            match std::fs::create_dir_all(&made) {
                Ok(()) => {
                    app.file_dialogs.android_file.browser = Some(made);
                    app.file_dialogs.android_file.folder_name.clear();
                }
                Err(e) => app.ui.status = tlf!("Could not create the folder: {error}", error = e),
            }
        } else if choose {
            if is_file {
                let paths: Vec<String> = app.file_dialogs.android_file.selected.iter().map(|p| p.to_string_lossy().into_owned()).collect();
                if paths.is_empty() {
                    app.ui.status = tl!("Select at least one file.").into();
                } else {
                    app.file_dialogs.android_file.finish(paths);
                }
            } else if matches!(request, FileDialog::Folder { .. }) {
                app.file_dialogs.android_file.finish(vec![dir.to_string_lossy().into_owned()]);
            } else {
                // A save's browse: this is the folder it lands in, and the prompt comes back
                // with it — answering the save itself with a directory would save nowhere.
                app.file_dialogs.android_file.folder = Some(dir);
                app.file_dialogs.android_file.browser = None;
                app.file_dialogs.android_file.selected.clear();
            }
        } else if cancel || crate::widgets::escape_closes(ctx) {
            app.file_dialogs.android_file.finish(Vec::new());
        }
    }
    app.file_dialogs.android_file.folder_name = folder_name;
}

/// A dialog button (registered for automation like every other control in this crate).
fn button(app: &mut FilmcraftApp, ui: &mut egui::Ui, id: &str, label: &str, primary: bool) -> bool {
    let t = app.tokens;
    let b = egui::Button::new(RichText::new(label).size(13.0).color(if primary { Color32::WHITE } else { t.text }))
        .min_size(vec2(88.0, 28.0))
        .corner_radius(CornerRadius::same(14))
        .fill(if primary { t.accent } else { Color32::TRANSPARENT })
        .stroke(if primary { Stroke::NONE } else { Stroke::new(1.5, t.text_faint) });
    let r = ui.add(b);
    app.auto.add(&format!("androidFile.{id}"), r.rect, label);
    r.clicked()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A device-shaped directory: a sub-folder, a saved project, media and a text file.
    fn fixture(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("filmcraft-picker-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("Projects")).unwrap();
        std::fs::create_dir_all(dir.join("shots")).unwrap();
        std::fs::write(dir.join("Projects").join("Scene.fcproj"), "{}").unwrap();
        std::fs::write(dir.join("clip.mp4"), "x").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        dir
    }

    /// The host that offers `dir` as its storage, with a channel and the frames to draw.
    fn picker(dir: &Path) -> (FilmcraftApp, egui::Context) {
        let ctx = egui::Context::default();
        let mut app = FilmcraftApp::new(filmcraft_engine::Session::default());
        crate::theme::install(&ctx, &app.tokens);
        app.hooks.app_files_dir = Some(dir.to_path_buf());
        (app, ctx)
    }

    /// One frame the way `FilmcraftApp::update` draws it: the pending dialog's answer first,
    /// then the modals.
    fn frame(app: &mut FilmcraftApp, ctx: &egui::Context) {
        frame_with(app, ctx, Vec::new());
    }

    /// [`frame`] with `events` fed to egui first (a pointer position, a press, a release).
    fn frame_with(app: &mut FilmcraftApp, ctx: &egui::Context, events: Vec<egui::Event>) {
        app.poll_native_dialog(ctx);
        let input = egui::RawInput { events, ..Default::default() };
        let mut out = ctx.run_ui(input, |ui| {
            app.auto.begin_frame();
            crate::panels::dialogs::show(app, ui.ctx());
        });
        out.textures_delta.clear();
    }

    /// The automation element `id` as the last frame registered it.
    fn element(app: &FilmcraftApp, id: &str) -> Option<[f32; 4]> {
        app.auto.elements.iter().find(|e| e.id == id).map(|e| e.rect)
    }

    /// Tap the automation element `id` the way a finger does: hover it, then press and release
    /// where it is *after* that frame — the headless modal settles its centered position on some
    /// pointer frames, so a rect read earlier may already be stale.
    fn tap(app: &mut FilmcraftApp, ctx: &egui::Context, id: &str) {
        let Some(rect) = element(app, id) else { panic!("no `{id}` on screen") };
        let p = egui::pos2(rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
        frame_with(app, ctx, vec![egui::Event::PointerMoved(p)]);
        let Some(rect) = element(app, id) else { panic!("no `{id}` after hover") };
        let p = egui::pos2(rect[0] + rect[2] / 2.0, rect[1] + rect[3] / 2.0);
        assert!(p.x > 0.0 && p.y > 0.0, "`{id}` is on screen");
        let press =
            |pressed: bool| vec![egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::default() }];
        frame_with(app, ctx, press(true));
        frame_with(app, ctx, press(false));
        frame(app, ctx);
    }

    /// A file pick answers the command with the file a finger chose, and lets it run.
    #[test]
    fn a_tap_on_a_file_answers_the_command() {
        let dir = fixture("pick");
        let (mut app, ctx) = picker(&dir);
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&got);
        app.pick_ui(FileDialog::open_file(tl!("Media"), &["mp4"]), move |_, paths| *answer.borrow_mut() = Some(paths));
        assert!(app.native_dialog_open(), "the dialog is open until it is answered");
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.file.0");
        let want = dir.join("clip.mp4").to_string_lossy().into_owned();
        assert_eq!(got.borrow().clone(), Some(vec![want]), "one file, chosen on the tap");
        assert!(!app.native_dialog_open(), "answered: the next dialog may open");
        assert!(app.file_dialogs.android_file.request.is_none(), "the browser closed");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Several files: a tap checks one (nothing is answered yet), Choose answers with them.
    #[test]
    fn a_multi_file_pick_answers_with_the_selection() {
        let dir = fixture("many");
        let (mut app, ctx) = picker(&dir);
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&got);
        app.pick_ui(FileDialog::open_files(&["mp4"]), move |_, paths| *answer.borrow_mut() = Some(paths));
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.file.0");
        assert_eq!(got.borrow().as_ref(), None, "nothing is answered before Choose");
        assert!(app.native_dialog_open());
        assert_eq!(app.file_dialogs.android_file.selected, vec![dir.join("clip.mp4")], "the tap checked it");
        tap(&mut app, &ctx, "androidFile.choose");
        assert_eq!(got.borrow().clone(), Some(vec![dir.join("clip.mp4").to_string_lossy().into_owned()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Cancelling answers with no paths (as a dismissed system picker does) and closes.
    #[test]
    fn cancelling_answers_with_nothing() {
        let dir = fixture("cancel");
        let (mut app, ctx) = picker(&dir);
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&got);
        app.pick_ui(FileDialog::open_files(&["mp4"]), move |_, paths| *answer.borrow_mut() = Some(paths));
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.cancel");
        assert_eq!(got.borrow().clone(), Some(Vec::new()), "cancelled");
        assert!(!app.native_dialog_open());
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A save writes where the dialog said, and an existing file is replaced only after the
    /// prompt asked for it — never by the first tap on Save.
    #[test]
    fn a_save_replaces_an_existing_file_only_when_asked() {
        let dir = fixture("save");
        std::fs::write(dir.join("Projects").join("Scene.fcproj"), "old").unwrap();
        let (mut app, ctx) = picker(&dir);
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&got);
        app.pick_ui(FileDialog::SaveProject { name: "Scene.fcproj".into() }, move |_, paths| {
            *answer.borrow_mut() = Some(paths);
        });
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.save");
        assert_eq!(got.borrow().as_ref(), None, "the file exists: nothing saved yet");
        assert_eq!(app.file_dialogs.android_file.confirm.as_deref(), Some(dir.join("Projects/Scene.fcproj").as_path()));
        tap(&mut app, &ctx, "androidFile.overwrite");
        let want = dir.join("Projects/Scene.fcproj").to_string_lossy().into_owned();
        assert_eq!(got.borrow().clone(), Some(vec![want]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The folder picker answers with the directory it is showing.
    #[test]
    fn a_folder_pick_answers_with_the_directory_shown() {
        let dir = fixture("folder");
        let (mut app, ctx) = picker(&dir);
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&got);
        app.pick_ui(FileDialog::Folder { at: None }, move |_, paths| *answer.borrow_mut() = Some(paths));
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        // enter the first folder, then choose it: the answer is the folder shown, not the root
        tap(&mut app, &ctx, "androidFile.dir.0");
        assert_eq!(app.file_dialogs.android_file.browser.as_deref(), Some(dir.join("Projects").as_path()), "the first folder is entered");
        tap(&mut app, &ctx, "androidFile.choose");
        let want = dir.join("Projects").to_string_lossy().into_owned();
        assert_eq!(got.borrow().clone(), Some(vec![want]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Chosen with nothing entered: the root the host offered.
    #[test]
    fn a_folder_pick_answers_with_the_root_when_nothing_is_entered() {
        let dir = fixture("folder-root");
        let (mut app, ctx) = picker(&dir);
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&got);
        app.pick_ui(FileDialog::Folder { at: None }, move |_, paths| *answer.borrow_mut() = Some(paths));
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.choose");
        assert_eq!(got.borrow().clone(), Some(vec![dir.to_string_lossy().into_owned()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A tap on a file never answers a folder pick: the folder is the answer.
    #[test]
    fn a_folder_pick_ignores_a_tap_on_a_file() {
        let dir = fixture("folder-file-tap");
        let (mut app, ctx) = picker(&dir);
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&got);
        app.pick_ui(FileDialog::Folder { at: None }, move |_, paths| *answer.borrow_mut() = Some(paths));
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.file.0");
        assert!(got.borrow().is_none(), "the file is context, not an answer");
        assert!(app.file_dialogs.android_file.browser.is_some(), "the listing stays open");
        tap(&mut app, &ctx, "androidFile.choose");
        assert_eq!(got.borrow().clone(), Some(vec![dir.to_string_lossy().into_owned()]));
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Browse… on the save prompt picks the folder the save lands in and comes back to the
    /// prompt (answering the save with a directory would save nowhere); a tap on a file there
    /// must not answer it either, which would replace that file without the overwrite prompt.
    #[test]
    fn a_save_browse_picks_the_folder_and_returns_to_the_prompt() {
        let dir = fixture("save-browse");
        let (mut app, ctx) = picker(&dir);
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&got);
        app.pick_ui(FileDialog::SaveProject { name: "Take2.fcproj".into() }, move |_, paths| *answer.borrow_mut() = Some(paths));
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.browse");
        assert!(app.file_dialogs.android_file.browser.is_some(), "the browser is open");
        tap(&mut app, &ctx, "androidFile.file.0");
        assert!(got.borrow().is_none(), "a tap on a file does not answer the save");
        assert!(app.file_dialogs.android_file.browser.is_some(), "the browser stays open");
        tap(&mut app, &ctx, "androidFile.choose");
        assert!(got.borrow().is_none(), "the browse does not answer the save");
        assert!(app.file_dialogs.android_file.browser.is_none(), "the prompt is back");
        assert_eq!(app.file_dialogs.android_file.folder.as_deref(), Some(dir.join("Projects").as_path()), "with the folder that was chosen");
        tap(&mut app, &ctx, "androidFile.save");
        let want = dir.join("Projects").join("Take2.fcproj").to_string_lossy().into_owned();
        assert_eq!(got.borrow().clone(), Some(vec![want]), "the save lands in the folder the browse picked");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Nothing is served where the host offers no storage: the desktop keeps its pickers.
    #[test]
    fn without_a_storage_directory_nothing_is_served() {
        let mut draft = AndroidFileDraft::default();
        let hooks = HostHooks::default();
        assert!(draft.serve(&hooks, &FileDialog::OpenProject).is_none());
        assert!(draft.request.is_none());
        assert!(!FileDialog::OpenProject.available(&hooks), "no host picker either: the command reports it");
    }

    #[test]
    fn save_paths_stay_inside_the_chosen_folder() {
        let base = Path::new("/files");
        assert_eq!(save_dir(base, true), PathBuf::from("/files/Projects"));
        assert_eq!(save_dir(base, false), PathBuf::from("/files/Exports"));
        assert_eq!(save_path_in(&save_dir(base, true), true, "Scene", "F.fcproj"), PathBuf::from("/files/Projects/Scene.fcproj"));
        assert_eq!(save_path_in(&save_dir(base, false), false, "Frame.png", "F.png"), PathBuf::from("/files/Exports/Frame.png"));
        // a typed directory can't move the save out of the folder it was asked for
        assert_eq!(save_path_in(&save_dir(base, true), true, "../x", "F.fcproj"), PathBuf::from("/files/Projects/x.fcproj"));
        assert_eq!(save_path_in(&save_dir(base, true), true, "/abs/path/x", "F.fcproj"), PathBuf::from("/files/Projects/x.fcproj"));
        assert_eq!(save_path_in(&save_dir(base, true), true, "   ", "Untitled.fcproj"), PathBuf::from("/files/Projects/Untitled.fcproj"));
        assert_eq!(save_path_in(&save_dir(base, true), true, "..", "Untitled.fcproj"), PathBuf::from("/files/Projects/Untitled.fcproj"));
    }

    #[test]
    fn project_saves_keep_their_extension() {
        assert_eq!(enforce_project_ext("Scene.fcproj"), "Scene.fcproj");
        assert_eq!(enforce_project_ext("Scene"), "Scene.fcproj");
        assert_eq!(enforce_project_ext("Scene.png"), "Scene.png.fcproj");
        // a dotted name that is not a project is not one either
        assert_eq!(enforce_project_ext("Copy.FCPRX"), "Copy.FCPRX.fcproj");
    }

    #[test]
    fn only_projects_list_newest_first() {
        let dir = std::env::temp_dir().join(format!("filmcraft-android-projects-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let projects = dir.join("Projects");
        std::fs::create_dir_all(&projects).unwrap();
        std::fs::write(projects.join("old.fcproj"), "{}").unwrap();
        std::thread::sleep(std::time::Duration::from_millis(10));
        std::fs::write(projects.join("new.fcproj"), "{}").unwrap();
        std::fs::write(projects.join("clip.mp4"), "x").unwrap();
        assert_eq!(list_projects(&dir), vec![projects.join("new.fcproj"), projects.join("old.fcproj")]);
        assert!(list_projects(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn the_browser_lists_only_directories_sorted() {
        let dir = std::env::temp_dir().join(format!("filmcraft-android-dirs-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("zeta")).unwrap();
        std::fs::create_dir_all(dir.join("Alpha")).unwrap();
        std::fs::write(dir.join("a-file.txt"), "x").unwrap();
        assert_eq!(list_dirs(&dir), vec![dir.join("Alpha"), dir.join("zeta")]);
        assert!(list_dirs(&dir.join("missing")).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn files_list_filtered_by_extension_sorted() {
        let dir = std::env::temp_dir().join(format!("filmcraft-android-files-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub")).unwrap();
        std::fs::write(dir.join("Clip.MP4"), "x").unwrap();
        std::fs::write(dir.join("still.png"), "x").unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();
        let mp4: Vec<String> = vec!["mp4".to_string()];
        // case-insensitive, directories never listed
        assert_eq!(list_files(&dir, &mp4), vec![dir.join("Clip.MP4")]);
        // an empty filter matches every file, still excluding directories
        assert_eq!(list_files(&dir, &[]), vec![dir.join("Clip.MP4"), dir.join("notes.txt"), dir.join("still.png")]);
        assert!(list_files(&dir.join("missing"), &[]).is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn up_is_clamped_to_the_root() {
        let root = Path::new("/files");
        assert_eq!(up_from(root, Path::new("/files/Projects/Deep")), PathBuf::from("/files/Projects"));
        assert_eq!(up_from(root, Path::new("/files/Projects")), PathBuf::from("/files"));
        // the root's own top is as far up as the browser may go — never out of the root
        assert_eq!(up_from(root, Path::new("/files")), PathBuf::from("/files"));
        // a directory that somehow sits outside the root goes straight back to it
        assert_eq!(up_from(root, Path::new("/elsewhere")), PathBuf::from("/files"));
        // the shared root clamps the same way, and neither root's bound leads to the other
        let shared = Path::new("/storage/emulated/0");
        assert_eq!(up_from(shared, Path::new("/storage/emulated/0/DCIM/Cam")), PathBuf::from("/storage/emulated/0/DCIM"));
        assert_eq!(up_from(shared, Path::new("/storage/emulated/0")), PathBuf::from("/storage/emulated/0"));
        assert_eq!(up_from(shared, Path::new("/files")), PathBuf::from("/storage/emulated/0"));
        // and `StorageRoot` names the two roots the switcher chooses between
        assert_eq!(StorageRoot::App("/files".into()).dir(), Path::new("/files"));
        assert_eq!(StorageRoot::Shared("/storage/emulated/0".into()).dir(), shared);
        assert!(!inside_root(Path::new("/storage/emulated/0/DCIM"), root, None), "no shared root offered: nothing outside the base");
        assert!(inside_root(Path::new("/storage/emulated/0/DCIM"), root, Some(shared)), "offered: a shared path is inside a root");
        assert!(!inside_root(Path::new("/etc"), root, Some(shared)), "and /etc is in neither");
    }

    #[test]
    fn a_suggested_folder_outside_the_root_is_not_offered() {
        let base = Path::new("/files");
        // a full path inside the root is where the dialog opens
        let (folder, name) = suggested(base, &[], "/files/Exports/Frame 1.png");
        assert_eq!(folder, PathBuf::from("/files/Exports"));
        assert_eq!(name, "Frame 1.png");
        // one outside it falls back to the root's own folder, and keeps its name
        let (folder, name) = suggested(base, &[], "/etc/passwd");
        assert_eq!(folder, PathBuf::from("/files/Exports"));
        assert_eq!(name, "passwd");
        // a bare name is a name
        let (folder, name) = suggested(base, &["fcproj"], "Scene.fcproj");
        assert_eq!(folder, PathBuf::from("/files/Projects"));
        assert_eq!(name, "Scene.fcproj");
        // nothing at all suggests something the export can actually write
        let (folder, name) = suggested(base, &["mp4"], "");
        assert_eq!(folder, PathBuf::from("/files/Exports"));
        assert_eq!(name, "Untitled.mp4");
    }

    #[test]
    fn readable_tells_an_unreadable_folder_from_an_empty_one() {
        let dir = std::env::temp_dir().join(format!("filmcraft-android-read-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        assert!(!readable(&dir));
        std::fs::create_dir_all(&dir).unwrap();
        assert!(readable(&dir));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn names_cannot_escape_the_dialog_folder() {
        assert_eq!(sanitize_name("Scene.fcproj", "F.fcproj"), "Scene.fcproj");
        assert_eq!(sanitize_name("../../evil.fcproj", "F.fcproj"), "evil.fcproj");
        assert_eq!(sanitize_name("", "Fallback.fcproj"), "Fallback.fcproj");
        assert_eq!(sanitize_name("..", "Fallback.fcproj"), "Fallback.fcproj");
    }

    /// Two roots side by side — `…/app` (the app-private storage) and `…/emulated` (the shared
    /// one, with `DCIM/Cam` and a photo inside) — deliberately not nested, so a clamp that
    /// confused the two would fail a test instead of hiding in it. The third path is the parent
    /// to clean up.
    fn two_roots(tag: &str) -> (PathBuf, PathBuf, PathBuf) {
        let root = std::env::temp_dir().join(format!("filmcraft-roots-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let base = root.join("app");
        let shared = root.join("emulated");
        std::fs::create_dir_all(base.join("Projects")).unwrap();
        std::fs::create_dir_all(base.join("Exports")).unwrap();
        std::fs::write(base.join("Projects").join("Scene.fcproj"), "{}").unwrap();
        std::fs::create_dir_all(shared.join("DCIM/Cam")).unwrap();
        std::fs::write(shared.join("DCIM").join("IMG_1.jpg"), "x").unwrap();
        (root, base, shared)
    }

    /// The host that offers `base` as its app-private storage and `shared` as the second root,
    /// with the grant check and the ask under the test's hands (both start ungranted).
    fn picker_with_shared(base: &Path, shared: &Path) -> (FilmcraftApp, egui::Context, std::rc::Rc<std::cell::Cell<bool>>, std::rc::Rc<std::cell::Cell<bool>>) {
        let (mut app, ctx) = picker(base);
        let granted = std::rc::Rc::new(std::cell::Cell::new(false));
        let asked = std::rc::Rc::new(std::cell::Cell::new(false));
        let g = std::rc::Rc::clone(&granted);
        let a = std::rc::Rc::clone(&asked);
        app.hooks.shared_storage_root = Some(shared.to_path_buf());
        app.hooks.shared_storage_granted = Some(Box::new(move || g.get()));
        app.hooks.shared_storage_grant = Some(Box::new(move || a.set(true)));
        (app, ctx, granted, asked)
    }

    /// Without a shared-storage hook there is no switcher at all — the desktop and the web draw
    /// the one-root dialog exactly as before — and with one, both roots are offered.
    #[test]
    fn the_switcher_is_drawn_only_when_the_host_offers_shared_storage() {
        let dir = fixture("switcher-off");
        let (mut app, ctx) = picker(&dir);
        app.pick_ui(FileDialog::Folder { at: None }, |_, _| {});
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        assert!(element(&app, "androidFile.rootApp").is_none(), "no switcher without a shared-storage hook");
        assert!(element(&app, "androidFile.rootShared").is_none(), "…nor a shared button");
        assert!(element(&app, "androidFile.grant").is_none(), "…nor a grant button");
        let _ = std::fs::remove_dir_all(&dir);

        let (root, base, shared) = two_roots("switcher-on");
        let (mut app, ctx, _, _) = picker_with_shared(&base, &shared);
        app.pick_ui(FileDialog::Folder { at: None }, |_, _| {});
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        assert!(element(&app, "androidFile.rootApp").is_some(), "the app-storage side of the switcher");
        assert!(element(&app, "androidFile.rootShared").is_some(), "the shared-storage side");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// The shared root is not entered while the grant is missing: the tap asks for it instead
    /// (the settings page), says so, and the browser stays where it was.
    #[test]
    fn the_shared_root_is_asked_for_not_entered_without_the_grant() {
        let (root, base, shared) = two_roots("grant");
        let (mut app, ctx, granted, asked) = picker_with_shared(&base, &shared);
        app.pick_ui(FileDialog::Folder { at: None }, |_, _| {});
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.rootShared");
        assert!(asked.get(), "the grant was asked for");
        assert_eq!(app.file_dialogs.android_file.browser.as_deref(), Some(base.as_path()), "still in the app-private root");
        assert!(!app.ui.status.is_empty(), "the user is told what to do next");
        // the grant lands (the settings page was used): the very same tap now navigates
        granted.set(true);
        tap(&mut app, &ctx, "androidFile.rootShared");
        assert_eq!(app.file_dialogs.android_file.browser.as_deref(), Some(shared.as_path()), "inside the shared root");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Inside shared storage "Up one level" stops at *its* top, and the switcher takes the
    /// browser back: neither root can be climbed out of, and neither is reached by accident.
    #[test]
    fn switching_roots_stays_inside_the_root_chosen() {
        let (root, base, shared) = two_roots("roots");
        let (mut app, ctx, granted, _) = picker_with_shared(&base, &shared);
        granted.set(true);
        app.pick_ui(FileDialog::Folder { at: None }, |_, _| {});
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.rootShared");
        tap(&mut app, &ctx, "androidFile.dir.0"); // DCIM
        tap(&mut app, &ctx, "androidFile.dir.0"); // DCIM/Cam
        assert_eq!(app.file_dialogs.android_file.browser.as_deref(), Some(shared.join("DCIM/Cam").as_path()));
        tap(&mut app, &ctx, "androidFile.up");
        tap(&mut app, &ctx, "androidFile.up");
        assert_eq!(app.file_dialogs.android_file.browser.as_deref(), Some(shared.as_path()), "climbing stops at the shared root");
        tap(&mut app, &ctx, "androidFile.up");
        assert_eq!(app.file_dialogs.android_file.browser.as_deref(), Some(shared.as_path()), "and never leaves it");
        tap(&mut app, &ctx, "androidFile.rootApp");
        assert_eq!(app.file_dialogs.android_file.browser.as_deref(), Some(base.as_path()), "the switcher takes it back");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// Browse… on the save prompt walks into shared storage and comes back with that folder, so
    /// the save lands there — outside the app-private directory, because the user chose it.
    #[test]
    fn a_save_browse_can_choose_a_folder_in_shared_storage() {
        let (root, base, shared) = two_roots("save-shared");
        let (mut app, ctx, granted, _) = picker_with_shared(&base, &shared);
        granted.set(true);
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&got);
        app.pick_ui(FileDialog::SaveProject { name: "Take3.fcproj".into() }, move |_, paths| *answer.borrow_mut() = Some(paths));
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.browse");
        tap(&mut app, &ctx, "androidFile.rootShared");
        assert_eq!(app.file_dialogs.android_file.browser.as_deref(), Some(shared.as_path()));
        tap(&mut app, &ctx, "androidFile.dir.0"); // DCIM
        tap(&mut app, &ctx, "androidFile.choose");
        assert!(got.borrow().is_none(), "the browse does not answer the save");
        assert!(app.file_dialogs.android_file.browser.is_none(), "the prompt is back");
        assert_eq!(app.file_dialogs.android_file.folder.as_deref(), Some(shared.join("DCIM").as_path()), "with the folder that was chosen");
        tap(&mut app, &ctx, "androidFile.save");
        let want = shared.join("DCIM").join("Take3.fcproj").to_string_lossy().into_owned();
        assert_eq!(got.borrow().clone(), Some(vec![want]), "the save lands in shared storage");
        let _ = std::fs::remove_dir_all(&root);
    }

    /// A shared root that cannot be read yet says so and offers the grant — the state an
    /// ungranted device shows — instead of looking like an empty folder.
    #[test]
    fn the_unreadable_shared_root_offers_the_grant() {
        let (root, base, _shared) = two_roots("unreadable");
        // not a directory at all: exactly what an ungranted shared root looks like to the app
        let missing = root.join("not-there");
        let (mut app, ctx, granted, asked) = picker_with_shared(&base, &missing);
        granted.set(true);
        app.pick_ui(FileDialog::Folder { at: None }, |_, _| {});
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        tap(&mut app, &ctx, "androidFile.rootShared");
        assert_eq!(app.file_dialogs.android_file.browser.as_deref(), Some(missing.as_path()));
        // the grant stops holding while browsing it (or was never really there)
        granted.set(false);
        frame(&mut app, &ctx);
        assert!(element(&app, "androidFile.grant").is_some(), "the grant is offered inside the unreadable shared root");
        tap(&mut app, &ctx, "androidFile.grant");
        assert!(asked.get(), "asking opens the settings page");
        assert_eq!(app.file_dialogs.android_file.browser.as_deref(), Some(missing.as_path()), "the listing stays put");
        let _ = std::fs::remove_dir_all(&root);
    }
}

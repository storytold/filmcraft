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
    let has = Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|e| e.eq_ignore_ascii_case("fcproj"));
    if has { name.to_string() } else { format!("{name}.fcproj") }
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

/// How the picker lists entries: one column of rows (the shape a desktop's system picker has), or
/// a grid of icons at one of three sizes — so what a file *is* (its picture) is visible before it
/// is chosen. Stored at `general.pickerView` (the engine's [`PICKER_VIEWS`]) and switched with the
/// picker's own view buttons.
///
/// [`PICKER_VIEWS`]: filmcraft_engine::settings::PICKER_VIEWS
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PickView {
    /// Rows: a folder icon, then a name (and the tick of a chosen file).
    List,
    /// Icons at the smallest size: the most entries on screen at once.
    Small,
    Medium,
    /// Icons large enough for real thumbnails — the default.
    #[default]
    Large,
}

impl PickView {
    /// The stored value at `general.pickerView`; anything unknown reads as the default, so a
    /// hand-edited settings file cannot break the picker.
    pub fn parse(value: &str) -> PickView {
        match value {
            "list" => PickView::List,
            "small" => PickView::Small,
            "medium" => PickView::Medium,
            _ => PickView::Large,
        }
    }

    /// The value stored at `general.pickerView` (one of the engine's `PICKER_VIEWS`).
    pub fn as_str(self) -> &'static str {
        match self {
            PickView::List => "list",
            PickView::Small => "small",
            PickView::Medium => "medium",
            PickView::Large => "large",
        }
    }

    /// An icon view's tile: its width, its height and the characters of a file name it fits
    /// (`List` has no tiles — its rows size themselves).
    fn tile(self) -> (f32, f32, usize) {
        match self {
            PickView::List => (0.0, 0.0, 0),
            PickView::Small => (84.0, 84.0, 14),
            PickView::Medium => (120.0, 112.0, 18),
            PickView::Large => (152.0, 136.0, 24),
        }
    }
}

/// How many tiles of `tile_w` fit across `width`, never zero (a sliver still lists its entries one
/// per row).
fn grid_cols(width: f32, tile_w: f32) -> usize {
    ((width - 8.0) / tile_w.max(1.0)).floor().max(1.0) as usize
}

/// The extension of `path`, upper-cased: what an icon tile says when it has no picture to show (a
/// project reads `FCPROJ`, a file with no extension reads `File`).
fn ext_label(path: &Path) -> String {
    path.extension().and_then(|e| e.to_str()).map(str::to_uppercase).filter(|e| !e.is_empty()).unwrap_or_else(|| tl!("File").to_string())
}

/// The view the picker opens in, remembered across dialogs at `general.pickerView`.
pub(crate) fn stored_view(app: &FilmcraftApp) -> PickView {
    PickView::parse(&app.session.prefs.general.picker_view)
}

/// Switch the open picker to `view` and remember it for the next dialog. A host whose prefs cannot
/// be written keeps the view for this dialog alone.
fn remember_view(app: &mut FilmcraftApp, ctx: &egui::Context, view: PickView) {
    app.file_dialogs.android_file.view = view;
    if let Err(e) = crate::menus::invoke(app, ctx, "prefs.set", serde_json::json!({"key": "general.pickerView", "value": view.as_str()})) {
        log::warn!("file picker: could not remember the view: {e}");
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
    /// How the browser and the Open Project list show their entries: [`stored_view`] when the
    /// dialog opens, [`remember_view`] when the view buttons switch it.
    view: PickView,
}

impl AndroidFileDraft {
    /// Serve `dialog` from the host's storage directory and return the channel its answer
    /// arrives on. `None`: this host offers no in-app storage (the desktop), so the caller keeps
    /// its own pickers. `view` is how the caller's prefs say to list entries ([`stored_view`]).
    pub(crate) fn serve(&mut self, hooks: &HostHooks, view: PickView, dialog: &FileDialog) -> Option<Receiver<Vec<String>>> {
        let base = hooks.app_files_dir.clone()?;
        let (tx, rx) = std::sync::mpsc::channel();
        let mut d = AndroidFileDraft { request: Some(dialog.clone()), answer: Some(tx), view, ..Default::default() };
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
                // base, as before. A start inside shared storage also makes the browser *be* in
                // the shared root, so "Up one level" stays there and the switcher shows it.
                let shared = hooks.shared_storage_root.as_deref();
                let start = at.as_deref().map(Path::new).filter(|p| p.is_dir() && inside_root(p, &base, shared)).map(Path::to_path_buf);
                if let (Some(p), Some(s)) = (start.as_deref(), shared)
                    && !p.starts_with(&base)
                    && p.starts_with(s)
                {
                    d.root = Some(StorageRoot::Shared(s.to_path_buf()));
                }
                d.browser = Some(start.unwrap_or(base));
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

/// The dialog width for the icon views: their grid needs the columns, so the dialog is wider than
/// the row list's — still clamped to what the screen actually has.
fn icon_width(ctx: &egui::Context) -> f32 {
    (ctx.content_rect().width() - 40.0).clamp(300.0, 660.0)
}

/// The view toolbar's row: the listing gets that much less of the screen.
const TOOLBAR_H: f32 = 38.0;

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

/// An icon-view tile: a folder, or a file whose picture the grid shows (falling back to the file's
/// extension when there is no picture to decode).
enum Tile {
    Folder,
    File(PathBuf),
}

/// One entry of the picker's icon grid.
struct TileItem {
    label: String,
    tile: Tile,
    selected: bool,
}

/// The picker's view buttons — `List`, then the three icon sizes — the current one highlighted.
/// Returns the one tapped this frame (applied after the modal, like the dialog's other actions).
fn view_toolbar(app: &mut FilmcraftApp, ui: &mut egui::Ui, view: PickView) -> Option<PickView> {
    let t = app.tokens;
    let mut picked = None;
    ui.horizontal(|ui| {
        ui.label(RichText::new(tl!("View")).size(11.5).color(t.text_dim));
        for (mode, label) in
            [(PickView::List, tl!("List")), (PickView::Small, tl!("Small")), (PickView::Medium, tl!("Medium")), (PickView::Large, tl!("Large"))]
        {
            let on = view == mode;
            let b = egui::Button::new(RichText::new(label).size(12.5).color(if on { Color32::WHITE } else { t.text })).selected(on).min_size(vec2(0.0, 26.0));
            let r = ui.add(b);
            app.auto.add(&format!("androidFile.view.{}", mode.as_str()), r.rect, label);
            if r.clicked() {
                picked = Some(mode);
            }
        }
    });
    picked
}

/// Draw the icon grid of `items` — `size.x` wide and `size.y` tall, scrolled by a finger through
/// [`crate::widgets::finger_scroll`] — and return the index of the entry tapped this frame. The
/// grid's tiles all carry a name-based automation id, so a test (or an agent) taps the same entry
/// in every view. Thumbnails decode at most twice per frame, so the rest arrive on later repaints.
fn icon_grid(
    app: &mut FilmcraftApp,
    ui: &mut egui::Ui,
    ctx: &egui::Context,
    t: &Tokens,
    size: egui::Vec2,
    view: PickView,
    items: &[TileItem],
    key: &str,
) -> Option<usize> {
    let (tw, th, chars) = view.tile();
    let (tw, th) = (tw.max(1.0), th.max(1.0));
    let mut budget = 2u32;
    let mut clicked = None;
    crate::widgets::finger_bar(ui);
    crate::widgets::finger_scroll(("android-file-grid", key)).max_height(size.y).auto_shrink([false, false]).show(ui, |ui| {
        let width = ui.available_width();
        let cols = grid_cols(width, tw);
        let rows = items.len().div_ceil(cols.max(1));
        let (rect, _) = ui.allocate_exact_size(vec2(width, (rows as f32 * th + 8.0).max(1.0)), egui::Sense::hover());
        let clip = ui.clip_rect();
        let lp = ui.painter().with_clip_rect(clip);
        for (i, item) in items.iter().enumerate() {
            let r =
                Rect::from_min_size(Pos2::new(rect.min.x + 4.0 + (i % cols) as f32 * tw, rect.min.y + 4.0 + (i / cols) as f32 * th), vec2(tw - 8.0, th - 8.0));
            if !r.intersects(clip) {
                continue;
            }
            let resp = ui.interact(r, egui::Id::new(("androidFile.grid", key, item.label.as_str())), egui::Sense::click());
            app.auto.add(&format!("androidFile.entry.{}", item.label), r, &item.label);
            lp.rect_filled(
                r,
                4.0,
                if item.selected {
                    t.row_selected
                } else if resp.hovered() {
                    t.hover
                } else {
                    t.field_bg
                },
            );
            // the picture sits above the name line at the tile's foot
            let picture = Rect::from_min_max(Pos2::new(r.min.x + 6.0, r.min.y + 6.0), Pos2::new(r.max.x - 6.0, (r.max.y - 22.0).max(r.min.y + 7.0)));
            match &item.tile {
                Tile::Folder => {
                    let side = (picture.width().min(picture.height()) * 0.6).max(10.0);
                    crate::icons::paint(&lp, Rect::from_center_size(picture.center(), vec2(side, side * 0.88)), Icon::Folder, Color32::from_rgb(237, 150, 58));
                }
                Tile::File(path) => {
                    let name = path.to_string_lossy();
                    if !crate::panels::media_browser::file_thumbnail(app, ctx, &lp, &name, &item.label, picture, &mut budget) {
                        lp.text(picture.center(), Align2::CENTER_CENTER, ext_label(path), Tokens::ui(10.0), t.text_faint);
                    }
                }
            }
            let shown = if item.label.chars().count() > chars {
                format!("{}…", item.label.chars().take(chars.saturating_sub(1)).collect::<String>())
            } else {
                item.label.clone()
            };
            lp.text(Pos2::new(r.center().x, r.max.y - 11.0), Align2::CENTER_CENTER, shown, Tokens::ui(11.0), t.text);
            if item.selected {
                lp.text(Pos2::new(r.max.x - 5.0, r.min.y + 12.0), Align2::RIGHT_CENTER, "✓", Tokens::ui(11.0), t.accent);
            }
            if resp.clicked() {
                clicked = Some(i);
            }
        }
    });
    clicked
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
    let view = app.file_dialogs.android_file.view;
    let projects = list_projects(base);
    let w = if view == PickView::List { width(ctx) } else { icon_width(ctx) };
    let list_h = (list_height(ctx) - TOOLBAR_H).max(80.0);
    let mut new_view: Option<PickView> = None;
    let mut open: Option<PathBuf> = None;
    let mut browse = false;
    let mut cancel = false;
    if modal(app, ctx, "android-file-open-project", tl!("Open Project…"), w, |app, ui| {
        let t = app.tokens;
        if let Some(v) = view_toolbar(app, ui, view) {
            new_view = Some(v);
        }
        ui.add_space(6.0);
        if projects.is_empty() {
            ui.label(RichText::new(tl!("No saved projects yet.")).size(13.0).color(t.text_dim));
        } else if view == PickView::List {
            crate::widgets::finger_bar(ui);
            crate::widgets::finger_scroll("android-file-open-project-list").auto_shrink([false, false]).max_height(list_h).show(ui, |ui| {
                for p in &projects {
                    let label = name_of(p);
                    if row(app, ui, &t, &format!("entry.{label}"), &label, false, false) {
                        open = Some(p.clone());
                    }
                }
            });
        } else {
            let items: Vec<TileItem> = projects.iter().map(|p| TileItem { label: name_of(p), tile: Tile::File(p.clone()), selected: false }).collect();
            let size = vec2(ui.available_width(), list_h);
            if let Some(i) = icon_grid(app, ui, ctx, &t, size, view, &items, "open-project")
                && let Some(p) = projects.get(i)
            {
                open = Some(p.clone());
            }
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
        if let Some(v) = new_view {
            remember_view(app, ctx, v);
        } else if let Some(path) = open {
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
    let view = app.file_dialogs.android_file.view;
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
    // The switcher row takes the listing that much less of the screen (and the view toolbar too).
    let list_h = (list_height(ctx) - if shared.is_some() { 36.0 } else { 0.0 } - TOOLBAR_H).max(80.0);
    // The icon views get a wider dialog: their grid lays tiles across it.
    let dialog_w = if view == PickView::List { width(ctx) } else { icon_width(ctx) };

    let mut new_view: Option<PickView> = None;
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

    if modal(app, ctx, "android-file-browser", &title, dialog_w, |app, ui| {
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
        // How the listing below is drawn; the buttons are the picker's own.
        if let Some(v) = view_toolbar(app, ui, view) {
            new_view = Some(v);
        }
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
        } else if view == PickView::List {
            crate::widgets::finger_bar(ui);
            crate::widgets::finger_scroll("android-file-list").auto_shrink([false, false]).max_height(list_h).show(ui, |ui| {
                for d in &dirs {
                    let label = name_of(d);
                    if row(app, ui, &t, &format!("entry.{label}"), &label, true, false) {
                        enter = Some(d.clone());
                    }
                }
                for (i, f) in files.iter().enumerate() {
                    let label = name_of(f);
                    // Where the folder is the answer, a file is context: tapping one must never
                    // answer a save (that would replace it without the overwrite prompt).
                    let tapped = row(app, ui, &t, &format!("entry.{label}"), &label, false, is_file && selected.contains(f));
                    if tapped && is_file {
                        if many {
                            toggle = Some(i);
                        } else {
                            pick = Some(f.clone());
                        }
                    }
                }
            });
        } else {
            let mut items: Vec<TileItem> = dirs.iter().map(|d| TileItem { label: name_of(d), tile: Tile::Folder, selected: false }).collect();
            items.extend(files.iter().map(|f| TileItem { label: name_of(f), tile: Tile::File(f.clone()), selected: is_file && selected.contains(f) }));
            let size = vec2(ui.available_width(), list_h);
            if let Some(i) = icon_grid(app, ui, ctx, &t, size, view, &items, "browser") {
                if let Some(d) = dirs.get(i) {
                    enter = Some(d.clone());
                } else if let Some(fi) = i.checked_sub(dirs.len())
                    && is_file
                {
                    if many {
                        toggle = Some(fi);
                    } else {
                        pick = files.get(fi).cloned();
                    }
                }
            }
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
        if let Some(v) = new_view {
            remember_view(app, ctx, v);
        } else if ask_grant || grant {
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
        tap(&mut app, &ctx, "androidFile.entry.clip.mp4");
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
        tap(&mut app, &ctx, "androidFile.entry.clip.mp4");
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
        tap(&mut app, &ctx, "androidFile.entry.Projects");
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
        tap(&mut app, &ctx, "androidFile.entry.clip.mp4");
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
        tap(&mut app, &ctx, "androidFile.entry.Scene.fcproj");
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
        assert!(draft.serve(&hooks, PickView::Large, &FileDialog::OpenProject).is_none());
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
        // the *last* extension decides, not the segment after the first dot
        assert_eq!(enforce_project_ext("Scene.v2.fcproj"), "Scene.v2.fcproj");
        assert_eq!(enforce_project_ext("Scene.fcproj.bak"), "Scene.fcproj.bak.fcproj");
    }

    /// A folder pick that starts inside shared storage keeps the shared root: "Up one level" must
    /// not jump into the app-private tree, and the switcher must show the root the browser is in.
    #[test]
    fn a_folder_pick_inside_shared_storage_stays_in_the_shared_root() {
        let base = std::env::temp_dir().join(format!("filmcraft-picker-root-{}", std::process::id()));
        let shared = std::env::temp_dir().join(format!("filmcraft-picker-shared-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&shared);
        std::fs::create_dir_all(&base).unwrap();
        std::fs::create_dir_all(shared.join("Movies")).unwrap();
        let hooks = HostHooks { app_files_dir: Some(base.clone()), shared_storage_root: Some(shared.clone()), ..Default::default() };
        let mut draft = AndroidFileDraft::default();

        let at = Some(shared.join("Movies").to_string_lossy().into_owned());
        assert!(draft.serve(&hooks, PickView::Large, &FileDialog::Folder { at }).is_some());
        assert_eq!(draft.browser.as_deref(), Some(shared.join("Movies").as_path()), "the browser starts where the pick did");
        assert_eq!(draft.root, Some(StorageRoot::Shared(shared.clone())), "and the shared root is the one it is in");

        let _ = std::fs::remove_dir_all(&base);
        let _ = std::fs::remove_dir_all(&shared);
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
        tap(&mut app, &ctx, "androidFile.entry.DCIM"); // DCIM
        tap(&mut app, &ctx, "androidFile.entry.Cam"); // DCIM/Cam
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
        tap(&mut app, &ctx, "androidFile.entry.DCIM"); // DCIM
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

    /// The stored view string parses to a view and back (anything unknown reads as the default),
    /// each icon view knows its tile, and a narrow width still lays a grid out one tile per row.
    #[test]
    fn picker_views_parse_and_lay_out() {
        for view in [PickView::List, PickView::Small, PickView::Medium, PickView::Large] {
            assert_eq!(PickView::parse(view.as_str()), view, "a stored view round-trips");
        }
        assert_eq!(PickView::parse("huge"), PickView::Large, "an unknown view reads as the default");
        assert_eq!(PickView::default(), PickView::Large, "the picker opens in large icons by default");
        assert_eq!(PickView::Small.tile(), (84.0, 84.0, 14));
        assert_eq!(PickView::Medium.tile(), (120.0, 112.0, 18));
        assert_eq!(PickView::Large.tile(), (152.0, 136.0, 24));
        assert_eq!(PickView::List.tile(), (0.0, 0.0, 0), "rows size themselves");
        assert_eq!(grid_cols(4.0, 152.0), 1);
        assert!(grid_cols(400.0, 0.0) > 0, "a zero tile is clamped, never divided by");
        // the label an icon tile falls back to when it has no picture to show
        assert_eq!(ext_label(Path::new("/x/Scene.fcproj")), "FCPROJ");
        assert_eq!(ext_label(Path::new("/x/Clip.MP4")), "MP4");
        assert_eq!(ext_label(Path::new("/x/README")), tl!("File"), "no extension: it is still a file");
    }

    /// The view buttons switch the listing between rows and icon grids and remember the choice at
    /// `general.pickerView`; an entry keeps the same automation id in every view, so a tap lands
    /// on the same file whichever one is shown.
    #[test]
    fn the_view_buttons_switch_the_listing_and_are_remembered() {
        let dir = fixture("views");
        let (mut app, ctx) = picker(&dir);
        app.pick_ui(FileDialog::open_files(&["mp4"]), |_, _| {});
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        assert_eq!(app.file_dialogs.android_file.view, PickView::Large, "the default view is the large icons");
        for mode in ["list", "small", "medium", "large"] {
            assert!(element(&app, &format!("androidFile.view.{mode}")).is_some(), "the {mode} view button");
        }
        tap(&mut app, &ctx, "androidFile.view.list");
        assert_eq!(app.file_dialogs.android_file.view, PickView::List, "switched to the rows");
        assert_eq!(app.session.prefs.general.picker_view, "list", "and remembered");
        // a row in the list view is the same entry as a tile in a grid view
        tap(&mut app, &ctx, "androidFile.entry.clip.mp4");
        assert_eq!(app.file_dialogs.android_file.selected, vec![dir.join("clip.mp4")], "the row checked it");
        tap(&mut app, &ctx, "androidFile.view.small");
        frame(&mut app, &ctx);
        assert!(element(&app, "androidFile.entry.clip.mp4").is_some(), "the same entry is a tile in the grid");
        tap(&mut app, &ctx, "androidFile.entry.clip.mp4");
        assert!(app.file_dialogs.android_file.selected.is_empty(), "the tile tapped the same file and unchecked it");
        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The remembered view is what the *next* dialog opens in — the browser and the Open Project
    /// list alike — and a project tile answers with the project file.
    #[test]
    fn the_next_dialog_opens_in_the_view_that_was_remembered() {
        let dir = fixture("view-next");
        let (mut app, ctx) = picker(&dir);
        app.session.prefs.general.picker_view = "small".into();
        app.pick_ui(FileDialog::Folder { at: None }, |_, _| {});
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        assert_eq!(app.file_dialogs.android_file.view, PickView::Small, "the stored view, not the default");
        tap(&mut app, &ctx, "androidFile.cancel");
        app.session.prefs.general.picker_view = "medium".into();
        let got = std::rc::Rc::new(std::cell::RefCell::new(None));
        let answer = std::rc::Rc::clone(&got);
        app.pick_ui(FileDialog::OpenProject, move |_, paths| *answer.borrow_mut() = Some(paths));
        frame(&mut app, &ctx);
        frame(&mut app, &ctx);
        assert_eq!(app.file_dialogs.android_file.view, PickView::Medium, "the Open Project list takes the same view");
        assert!(element(&app, "androidFile.entry.Scene.fcproj").is_some(), "the project is a tile");
        tap(&mut app, &ctx, "androidFile.entry.Scene.fcproj");
        assert_eq!(got.borrow().clone(), Some(vec![dir.join("Projects/Scene.fcproj").to_string_lossy().into_owned()]));
        let _ = std::fs::remove_dir_all(&dir);
    }
}

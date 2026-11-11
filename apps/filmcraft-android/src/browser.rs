//! A touch-friendly file browser drawn inside the app, used for every file dialog on Android.
//!
//! Android has no native file dialog a pure-Rust app can open (the system picker is a Java
//! activity that returns `content://` URIs, which the engine cannot read), so the app shows its own
//! browser over the paths it can read directly. The host asks for a dialog with a [`Request`]; the
//! browser answers on the request's channel, and dropping it unanswered counts as "cancelled".
//!
//! This module is plain egui and `std::fs`, so it builds and is tested on every target.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::sync::mpsc::Sender;

use filmcraft_ui_egui::native_dialogs::FileDialog;

/// Most entries listed for one folder, so a huge folder cannot stall a frame.
const MAX_ENTRIES: usize = 5_000;

/// A dialog the app asked for and where to send the chosen paths (none: cancelled).
pub struct Request {
    pub dialog: FileDialog,
    pub reply: Sender<Vec<String>>,
}

/// A quick-access folder shown as a button above the list.
#[derive(Clone, Debug)]
pub struct Root {
    pub label: String,
    pub path: PathBuf,
}

#[derive(Clone, Debug, PartialEq)]
enum Mode {
    /// Choose files (`multi`: more than one).
    Files { multi: bool },
    /// Choose the folder being shown.
    Folder,
    /// Name a file to save in the folder being shown.
    Save,
}

#[derive(Clone, Debug, PartialEq)]
struct Entry {
    name: String,
    path: PathBuf,
    is_dir: bool,
}

/// What the user did this frame.
#[derive(Debug, PartialEq)]
enum Action {
    None,
    Go(PathBuf),
    Up,
    Toggle(PathBuf),
    Accept,
    Cancel,
}

pub struct Browser {
    title: String,
    mode: Mode,
    exts: Vec<String>,
    roots: Vec<Root>,
    dir: PathBuf,
    entries: Vec<Entry>,
    truncated: bool,
    error: Option<String>,
    selected: BTreeSet<PathBuf>,
    name: String,
    reply: Option<Sender<Vec<String>>>,
}

impl Browser {
    /// Browser for `request`, starting in the first of `roots` that exists (else `/`).
    pub fn new(request: Request, roots: Vec<Root>) -> Self {
        let Request { dialog, reply } = request;
        let (title, mode, exts, name, start) = match dialog {
            FileDialog::OpenFiles { exts } => ("Choose files", Mode::Files { multi: true }, exts, String::new(), None),
            FileDialog::OpenFile { exts, .. } => ("Choose a file", Mode::Files { multi: false }, exts, String::new(), None),
            FileDialog::Relink { exts, .. } => ("Locate file", Mode::Files { multi: false }, exts, String::new(), None),
            FileDialog::OpenProject => ("Open project", Mode::Files { multi: false }, vec!["fcproj".to_string()], String::new(), None),
            FileDialog::SaveProject { name } => ("Save project", Mode::Save, vec!["fcproj".to_string()], name, None),
            FileDialog::SaveAs { exts, name, .. } => ("Save as", Mode::Save, exts, name, None),
            FileDialog::Folder { at } => ("Choose a folder", Mode::Folder, Vec::new(), String::new(), at.filter(|a| !a.is_empty()).map(PathBuf::from)),
        };
        let dir = start.filter(|p| p.is_dir()).or_else(|| roots.iter().map(|r| r.path.clone()).find(|p| p.is_dir())).unwrap_or_else(|| PathBuf::from("/"));
        let mut browser = Self {
            title: title.to_string(),
            mode,
            exts,
            roots,
            dir,
            entries: Vec::new(),
            truncated: false,
            error: None,
            selected: BTreeSet::new(),
            name,
            reply: Some(reply),
        };
        browser.refresh();
        browser
    }

    /// Folder being shown.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// Entries shown (folders first), for tests.
    #[cfg(test)]
    fn names(&self) -> Vec<&str> {
        self.entries.iter().map(|e| e.name.as_str()).collect()
    }

    /// Does `name` pass the filter? Case-insensitive; no extensions (or `*`) accepts everything.
    fn accepts(&self, name: &str) -> bool {
        let wanted: Vec<&str> = self.exts.iter().map(|e| e.trim_start_matches(['*', '.'])).filter(|e| !e.is_empty()).collect();
        if wanted.is_empty() {
            return true;
        }
        Path::new(name).extension().and_then(|e| e.to_str()).is_some_and(|ext| wanted.iter().any(|w| w.eq_ignore_ascii_case(ext)))
    }

    /// Read the current folder again: folders first, then files that pass the filter, both by name.
    fn refresh(&mut self) {
        self.entries.clear();
        self.truncated = false;
        self.error = None;
        let read = match std::fs::read_dir(&self.dir) {
            Ok(r) => r,
            Err(e) => {
                self.error = Some(format!("Cannot open {}: {e}", self.dir.display()));
                return;
            }
        };
        let mut seen = 0usize;
        for item in read.flatten() {
            seen += 1;
            if seen > MAX_ENTRIES {
                self.truncated = true;
                break;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') {
                continue;
            }
            // `metadata` follows links, so a link to a folder opens like a folder
            let is_dir = item.metadata().map(|m| m.is_dir()).unwrap_or(false);
            if is_dir || (self.mode != Mode::Folder && self.accepts(&name)) {
                self.entries.push(Entry { name, path: item.path(), is_dir });
            }
        }
        self.entries.sort_by(|a, b| b.is_dir.cmp(&a.is_dir).then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase())));
    }

    fn go(&mut self, dir: PathBuf) {
        self.dir = dir;
        self.selected.clear();
        self.refresh();
    }

    fn up(&mut self) {
        if let Some(parent) = self.dir.parent().map(Path::to_path_buf) {
            self.go(parent);
        }
    }

    fn toggle(&mut self, path: PathBuf) {
        let multi = matches!(self.mode, Mode::Files { multi: true });
        if self.selected.contains(&path) {
            self.selected.remove(&path);
            return;
        }
        if !multi {
            self.selected.clear();
        }
        if self.mode == Mode::Save
            && let Some(name) = path.file_name().and_then(|n| n.to_str())
        {
            self.name = name.to_string();
        }
        self.selected.insert(path);
    }

    /// The paths the dialog would answer with now (none: nothing valid chosen yet).
    fn chosen(&self) -> Vec<String> {
        let text = |p: &Path| p.to_string_lossy().into_owned();
        match self.mode {
            Mode::Files { .. } => self.selected.iter().map(|p| text(p)).collect(),
            Mode::Folder => vec![text(&self.dir)],
            Mode::Save => {
                let name = self.name.trim();
                // a name must stay inside the folder shown
                if name.is_empty() || name.contains(['/', '\\']) || name == "." || name == ".." {
                    return Vec::new();
                }
                vec![text(&self.dir.join(name))]
            }
        }
    }

    fn finish(&mut self, paths: Vec<String>) {
        if let Some(reply) = self.reply.take() {
            // a closed receiver (the app dropped the dialog) is not an error here
            let _ = reply.send(paths);
        }
    }

    fn apply(&mut self, action: Action) -> bool {
        match action {
            Action::None => false,
            Action::Go(dir) => {
                self.go(dir);
                false
            }
            Action::Up => {
                self.up();
                false
            }
            Action::Toggle(path) => {
                self.toggle(path);
                false
            }
            Action::Accept => {
                let paths = self.chosen();
                if paths.is_empty() {
                    return false;
                }
                self.finish(paths);
                true
            }
            Action::Cancel => {
                self.finish(Vec::new());
                true
            }
        }
    }

    /// Draw the browser; true when it is finished (answered or cancelled) and should be dropped.
    pub fn show(&mut self, ctx: &egui::Context) -> bool {
        let mut action = Action::None;
        let screen = ctx.content_rect();
        let size = egui::vec2((screen.width() - 24.0).clamp(280.0, 720.0), (screen.height() - 24.0).clamp(240.0, 560.0));
        egui::Window::new(self.title.clone())
            .collapsible(false)
            .resizable(false)
            .movable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .fixed_size(size)
            .show(ctx, |ui| {
                ui.horizontal_wrapped(|ui| {
                    if ui.add_sized([56.0, 40.0], egui::Button::new("⬆ Up")).clicked() {
                        action = Action::Up;
                    }
                    for root in &self.roots {
                        if root.path.is_dir() && ui.add_sized([0.0, 40.0], egui::Button::new(root.label.as_str())).clicked() {
                            action = Action::Go(root.path.clone());
                        }
                    }
                });
                ui.label(egui::RichText::new(self.dir.display().to_string()).small().weak());
                ui.separator();
                let list_height = (size.y - 190.0).max(80.0);
                egui::ScrollArea::vertical().max_height(list_height).auto_shrink([false, false]).show(ui, |ui| {
                    if let Some(error) = &self.error {
                        ui.colored_label(egui::Color32::LIGHT_RED, error.as_str());
                        ui.label("If this is shared storage, allow FilmCraft to read your files: Settings > Apps > FilmCraft > Permissions.");
                    } else if self.entries.is_empty() {
                        ui.weak("Nothing to show in this folder.");
                    }
                    for entry in &self.entries {
                        let selected = self.selected.contains(&entry.path);
                        let label = if entry.is_dir { format!("📁  {}", entry.name) } else { entry.name.clone() };
                        let button = egui::Button::selectable(selected, label).wrap_mode(egui::TextWrapMode::Truncate);
                        if ui.add_sized([ui.available_width(), 40.0], button).clicked() {
                            action = if entry.is_dir { Action::Go(entry.path.clone()) } else { Action::Toggle(entry.path.clone()) };
                        }
                    }
                    if self.truncated {
                        ui.weak(format!("Only the first {MAX_ENTRIES} entries are shown."));
                    }
                });
                ui.separator();
                if self.mode == Mode::Save {
                    ui.horizontal(|ui| {
                        ui.label("Name");
                        ui.add_sized([ui.available_width(), 36.0], egui::TextEdit::singleline(&mut self.name));
                    });
                }
                ui.horizontal(|ui| {
                    if ui.add_sized([96.0, 44.0], egui::Button::new("Cancel")).clicked() {
                        action = Action::Cancel;
                    }
                    let (text, enabled) = match self.mode {
                        Mode::Files { .. } => (format!("Choose ({})", self.selected.len()), !self.selected.is_empty()),
                        Mode::Folder => ("Select this folder".to_string(), true),
                        Mode::Save => ("Save".to_string(), !self.chosen().is_empty()),
                    };
                    if ui.add_enabled(enabled, egui::Button::new(text).min_size(egui::vec2(160.0, 44.0))).clicked() {
                        action = Action::Accept;
                    }
                });
            });
        self.apply(action)
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::mpsc::{Receiver, channel};

    use super::*;

    /// A fresh folder under the system temp dir, removed when dropped.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new(tag: &str) -> Self {
            let dir = std::env::temp_dir().join(format!("filmcraft-android-browser-{tag}-{}", std::process::id()));
            let _ = fs::remove_dir_all(&dir);
            fs::create_dir_all(&dir).expect("temp dir");
            Self(dir)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn root(dir: &Path) -> Vec<Root> {
        vec![Root { label: "Test".into(), path: dir.to_path_buf() }]
    }

    fn browser(dialog: FileDialog, dir: &Path) -> (Browser, Receiver<Vec<String>>) {
        let (reply, rx) = channel();
        (Browser::new(Request { dialog, reply }, root(dir)), rx)
    }

    fn files(exts: &[&str]) -> FileDialog {
        FileDialog::OpenFiles { exts: exts.iter().map(|e| e.to_string()).collect() }
    }

    fn text(p: PathBuf) -> String {
        p.to_string_lossy().into_owned()
    }

    #[test]
    fn lists_folders_first_filters_by_extension_and_hides_dotfiles() {
        let tmp = Tmp::new("list");
        fs::create_dir(tmp.0.join("Zeta")).expect("dir");
        fs::create_dir(tmp.0.join("alpha")).expect("dir");
        fs::write(tmp.0.join("b.MP4"), b"").expect("file");
        fs::write(tmp.0.join("a.mov"), b"").expect("file");
        fs::write(tmp.0.join("notes.txt"), b"").expect("file");
        fs::write(tmp.0.join(".hidden.mp4"), b"").expect("file");
        let (b, _rx) = browser(files(&["mp4", "mov"]), &tmp.0);
        assert_eq!(b.names(), ["alpha", "Zeta", "a.mov", "b.MP4"]);
        let (all, _rx) = browser(files(&["*"]), &tmp.0);
        assert!(all.names().contains(&"notes.txt"));
    }

    #[test]
    fn multi_select_answers_with_every_chosen_path() {
        let tmp = Tmp::new("multi");
        fs::write(tmp.0.join("a.mp4"), b"").expect("file");
        fs::write(tmp.0.join("b.mp4"), b"").expect("file");
        let (mut b, rx) = browser(files(&["mp4"]), &tmp.0);
        assert!(!b.apply(Action::Accept), "nothing chosen yet keeps the dialog open");
        b.apply(Action::Toggle(tmp.0.join("a.mp4")));
        b.apply(Action::Toggle(tmp.0.join("b.mp4")));
        assert!(b.apply(Action::Accept));
        assert_eq!(rx.try_recv().expect("answer"), vec![text(tmp.0.join("a.mp4")), text(tmp.0.join("b.mp4"))]);
    }

    #[test]
    fn single_select_replaces_the_choice() {
        let tmp = Tmp::new("single");
        fs::write(tmp.0.join("a.mp4"), b"").expect("file");
        fs::write(tmp.0.join("b.mp4"), b"").expect("file");
        let dialog = FileDialog::OpenFile { filter: "Media".into(), exts: vec!["mp4".into()] };
        let (mut b, rx) = browser(dialog, &tmp.0);
        b.apply(Action::Toggle(tmp.0.join("a.mp4")));
        b.apply(Action::Toggle(tmp.0.join("b.mp4")));
        assert!(b.apply(Action::Accept));
        assert_eq!(rx.try_recv().expect("answer"), vec![text(tmp.0.join("b.mp4"))]);
    }

    #[test]
    fn cancel_answers_nothing_and_a_dropped_dialog_closes_the_channel() {
        let tmp = Tmp::new("cancel");
        let (mut b, rx) = browser(files(&["mp4"]), &tmp.0);
        assert!(b.apply(Action::Cancel));
        assert_eq!(rx.try_recv().expect("answer"), Vec::<String>::new());
        let (b, rx) = browser(files(&["mp4"]), &tmp.0);
        drop(b);
        assert!(rx.try_recv().is_err(), "a dropped dialog sends nothing: the sender closes");
    }

    #[test]
    fn navigation_goes_into_folders_and_up_clears_the_selection() {
        let tmp = Tmp::new("nav");
        fs::create_dir(tmp.0.join("sub")).expect("dir");
        fs::write(tmp.0.join("sub").join("c.mp4"), b"").expect("file");
        let (mut b, _rx) = browser(files(&["mp4"]), &tmp.0);
        b.apply(Action::Go(tmp.0.join("sub")));
        assert_eq!(b.names(), ["c.mp4"]);
        b.apply(Action::Toggle(tmp.0.join("sub").join("c.mp4")));
        b.apply(Action::Up);
        assert_eq!(b.dir(), tmp.0.as_path());
        assert!(!b.apply(Action::Accept), "the selection did not survive leaving its folder");
    }

    #[test]
    fn folder_mode_answers_with_the_folder_shown_and_lists_only_folders() {
        let tmp = Tmp::new("folder");
        fs::create_dir(tmp.0.join("sub")).expect("dir");
        fs::write(tmp.0.join("a.mp4"), b"").expect("file");
        let (mut b, rx) = browser(FileDialog::Folder { at: Some(text(tmp.0.clone())) }, &tmp.0);
        assert_eq!(b.names(), ["sub"]);
        assert!(b.apply(Action::Accept));
        assert_eq!(rx.try_recv().expect("answer"), vec![text(tmp.0.clone())]);
    }

    #[test]
    fn save_mode_keeps_the_name_inside_the_folder() {
        let tmp = Tmp::new("save");
        let (mut b, rx) = browser(FileDialog::SaveProject { name: "edit".into() }, &tmp.0);
        for bad in ["", "  ", "..", ".", "../escape", "a/b", "a\\b"] {
            b.name = bad.to_string();
            assert!(!b.apply(Action::Accept), "{bad:?}");
        }
        b.name = "edit.fcproj".into();
        assert!(b.apply(Action::Accept));
        assert_eq!(rx.try_recv().expect("answer"), vec![text(tmp.0.join("edit.fcproj"))]);
    }

    #[test]
    fn an_unreadable_folder_reports_instead_of_failing() {
        let tmp = Tmp::new("missing");
        let (mut b, _rx) = browser(files(&["mp4"]), &tmp.0);
        b.go(tmp.0.join("does-not-exist"));
        assert!(b.error.is_some());
        assert!(b.names().is_empty());
    }

    #[test]
    fn the_browser_draws_without_panicking() {
        let tmp = Tmp::new("draw");
        fs::write(tmp.0.join("a.mp4"), b"").expect("file");
        let (mut b, _rx) = browser(files(&["mp4"]), &tmp.0);
        let ctx = egui::Context::default();
        let mut output = ctx.run_ui(egui::RawInput::default(), |ui| {
            assert!(!b.show(ui.ctx()));
        });
        // egui panics on dropped texture deltas that nobody uploaded
        output.textures_delta.clear();
    }
}

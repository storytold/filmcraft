//! Drag-and-drop straight from the OS file manager onto the window: a dropped MP4
//! (or any media file) lands in the Project bin with a status message, multiple dropped
//! files import with a plural count, failures are reported instead of swallowed,
//! and a drop over the Source monitor opens the clip there, ready to cut.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

fn step(h: &mut Harness<'static, FilmcraftApp>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    ctx.request_repaint();
    h.step();
}

fn harness() -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"OS drop test","width":1280,"height":720,"fps":24})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| FilmcraftApp::new(s));
    for _ in 0..4 {
        step(&mut h);
    }
    h
}

fn pointer_to(h: &mut Harness<'static, FilmcraftApp>, pos: egui::Pos2) {
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    step(h);
}

fn control_rect(h: &Harness<'static, FilmcraftApp>, id: &str) -> egui::Rect {
    let [x, y, w, height] = h.state().auto.elements.iter().find(|e| e.id == id).or_else(|| h.state().auto.find(id)).unwrap().rect;
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, height))
}

/// A file the OS dropped on the window, as the native integration hands it to egui.
#[derive(Debug)]
struct OsFile(PathBuf);

impl egui::DroppedFile for OsFile {
    fn path(&self) -> &Path {
        &self.0
    }

    fn bytes(&self) -> Result<Vec<u8>, String> {
        std::fs::read(&self.0).map_err(|e| e.to_string())
    }
}

fn drop_at(h: &mut Harness<'static, FilmcraftApp>, pos: egui::Pos2, path: &Path) {
    pointer_to(h, pos);
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(path.to_path_buf())));
    step(h);
    h.input_mut().dropped_files.clear();
    step(h);
}

fn fixture(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/ui-egui");
    std::fs::create_dir_all(&dir).unwrap();
    dir.join(name)
}

/// A small generated PNG still.
fn still(name: &str) -> PathBuf {
    let p = fixture(name);
    image::RgbaImage::from_pixel(64, 36, image::Rgba([40, 140, 220, 255])).save(&p).unwrap();
    p
}

fn clip_counts(h: &Harness<'static, FilmcraftApp>) -> (usize, usize) {
    let q = h.state().session.active_sequence().unwrap();
    (q.video_tracks.iter().map(|t| t.items.len()).sum(), q.audio_tracks.iter().map(|t| t.items.len()).sum())
}

#[test]
fn drop_on_the_project_panel_imports_to_the_bin_with_a_status_message() {
    let mut h = harness();
    let at = control_rect(&h, "panel.Project").center();
    let items_before = h.state().session.project.items.len();
    drop_at(&mut h, at, &still("os_drop_project_panel.png"));
    assert_eq!(h.state().session.project.items.len(), items_before + 1, "the file lands in the bin");
    assert_eq!(clip_counts(&h), (0, 0), "nothing is placed on the Timeline");
    assert_eq!(h.state().ui.status, "Imported 1 file", "the user is told what happened");
}

#[test]
fn drop_on_the_source_monitor_opens_the_clip_there() {
    let mut h = harness();
    let at = control_rect(&h, "panel.Source").center();
    let items_before = h.state().session.project.items.len();
    drop_at(&mut h, at, &still("os_drop_source_monitor.png"));
    assert_eq!(h.state().session.project.items.len(), items_before + 1, "the file lands in the bin");
    let opened = h.state().session.state.source_item.expect("the dropped clip opens in Source");
    assert!(h.state().session.project.item(opened).is_some(), "the opened item exists");
}

#[test]
fn drop_multiple_files_on_project_panel_imports_all_with_status_message() {
    let mut h = harness();
    let a = still("os_drop_multi_a.png");
    let b = still("os_drop_multi_b.png");
    let at = control_rect(&h, "panel.Project").center();
    let items_before = h.state().session.project.items.len();
    pointer_to(&mut h, at);
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(a)));
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(b)));
    step(&mut h);
    h.input_mut().dropped_files.clear();
    step(&mut h);
    assert_eq!(h.state().session.project.items.len(), items_before + 2, "both files land in the bin");
    assert_eq!(clip_counts(&h), (0, 0), "nothing is placed on the Timeline");
    assert_eq!(h.state().ui.status, "Imported 2 files", "the user is told what happened");
}

#[test]
fn import_mode_warns_only_where_os_drops_cannot_arrive() {
    // winit has no file drag-and-drop on Wayland: Import mode must say drops need
    // X11 there, and keep promising drops everywhere they actually arrive.
    let mut h = harness();
    assert!(!h.state().os_file_drop_unsupported, "the host opts in; tests default to drops working");
    h.state_mut().ui.mode = filmcraft_ui_egui::state::Mode::Import;
    for _ in 0..2 {
        step(&mut h);
    }
    assert!(h.state().auto.find("import.dropTip").is_none(), "no warning where drops work");
    h.state_mut().os_file_drop_unsupported = true;
    for _ in 0..2 {
        step(&mut h);
    }
    assert!(h.state().auto.find("import.dropTip").is_some(), "Wayland points at Browse files… instead");
}

#[test]
fn drop_a_missing_file_reports_an_error_instead_of_silence() {
    let mut h = harness();
    let at = control_rect(&h, "panel.Project").center();
    let items_before = h.state().session.project.items.len();
    drop_at(&mut h, at, &fixture("os_drop_no_such_file.png"));
    assert_eq!(h.state().session.project.items.len(), items_before, "nothing is imported");
    assert_eq!(h.state().ui.status, "Could not import the dropped files");
}

//! Files dropped from the OS onto the Project panel import into the bin under the pointer: a bin
//! row takes them, else the bin the view shows. Also files received by the host itself
//! (`HostHooks::os_file_drags`, Wayland) arrive like the windowing layer's own drops.

use std::path::{Path, PathBuf};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::{BinEntry, BinId};
use filmcraft_ui_egui::{FilmcraftApp, OsFileDrag};
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
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| FilmcraftApp::new(s));
    for _ in 0..4 {
        step(&mut h);
    }
    h
}

fn control_rect(h: &Harness<'static, FilmcraftApp>, id: &str) -> egui::Rect {
    let [x, y, w, height] = h.state().auto.elements.iter().find(|e| e.id == id).or_else(|| h.state().auto.find(id)).unwrap().rect;
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, height))
}

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

fn still(name: &str) -> PathBuf {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/fixtures/ui-egui");
    std::fs::create_dir_all(&dir).unwrap();
    let p = dir.join(name);
    image::RgbaImage::from_pixel(64, 36, image::Rgba([220, 140, 40, 255])).save(&p).unwrap();
    p
}

fn bin_named(h: &Harness<'static, FilmcraftApp>, name: &str) -> BinId {
    h.state()
        .session
        .project
        .root
        .children
        .iter()
        .find_map(|c| match c {
            BinEntry::Bin(b) if b.name == name => Some(b.id),
            _ => None,
        })
        .unwrap()
}

/// The bin directly holding the project item named `name`.
fn bin_of(h: &Harness<'static, FilmcraftApp>, name: &str) -> Option<BinId> {
    fn walk(b: &filmcraft_project::Bin, name: &str, items: &filmcraft_project::Project) -> Option<BinId> {
        for c in &b.children {
            match c {
                BinEntry::Item(i) if items.items.get(i).is_some_and(|it| it.name == name) => return Some(b.id),
                BinEntry::Bin(sub) => {
                    if let Some(found) = walk(sub, name, items) {
                        return Some(found);
                    }
                }
                _ => {}
            }
        }
        None
    }
    walk(&h.state().session.project.root, name, &h.state().session.project)
}

/// Hover files at `at` for a frame (as the OS reports a drag), then drop them there.
fn drop_at(h: &mut Harness<'static, FilmcraftApp>, at: egui::Pos2, path: &Path) {
    h.input_mut().events.push(egui::Event::PointerMoved(at));
    h.input_mut().hovered_files.push(egui::HoveredFile::default());
    step(h);
    h.input_mut().hovered_files.push(egui::HoveredFile::default());
    step(h);
    h.input_mut().hovered_files.clear();
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(path.to_path_buf())));
    step(h);
    h.input_mut().dropped_files.clear();
    step(h);
}

#[test]
fn a_file_dropped_on_a_bin_row_is_imported_into_that_bin() {
    let mut h = harness();
    let footage = bin_named(&h, "Footage");
    let row = control_rect(&h, &format!("project.bin.{}", footage.0));
    drop_at(&mut h, row.center(), &still("project_drop_on_bin_row.png"));
    assert_eq!(bin_of(&h, "project_drop_on_bin_row.png"), Some(footage), "status: {}", h.state().ui.status);
}

#[test]
fn a_file_dropped_on_the_project_panel_outside_a_bin_goes_to_the_shown_bin() {
    let mut h = harness();
    let root = h.state().session.project.root.id;
    let empty = control_rect(&h, "project.empty");
    drop_at(&mut h, empty.center(), &still("project_drop_on_empty.png"));
    assert_eq!(bin_of(&h, "project_drop_on_empty.png"), Some(root), "status: {}", h.state().ui.status);
}

/// The last move and the release can arrive together, with no frame drawn in between: the drop
/// still goes to the bin at the release point, not the one under the last hover frame.
#[test]
fn a_release_right_after_moving_onto_a_bin_row_uses_that_row() {
    let mut h = harness();
    let footage = bin_named(&h, "Footage");
    let row = control_rect(&h, &format!("project.bin.{}", footage.0)).center();
    let empty = control_rect(&h, "project.empty").center();
    for _ in 0..2 {
        h.input_mut().events.push(egui::Event::PointerMoved(empty));
        h.input_mut().hovered_files.push(egui::HoveredFile::default());
        step(&mut h);
    }
    h.input_mut().events.push(egui::Event::PointerMoved(row));
    h.input_mut().hovered_files.clear();
    let path = still("project_drop_release_after_move.png");
    h.input_mut().dropped_files.push(std::sync::Arc::new(OsFile(path)));
    step(&mut h);
    h.input_mut().dropped_files.clear();
    step(&mut h);
    assert_eq!(bin_of(&h, "project_drop_release_after_move.png"), Some(footage), "status: {}", h.state().ui.status);
}

/// Wayland: the host hands the drag over through `os_file_drags` (winit reports none there).
#[test]
fn a_host_reported_drop_on_a_bin_row_is_imported_into_that_bin() {
    let mut h = harness();
    let footage = bin_named(&h, "Footage");
    let at = control_rect(&h, &format!("project.bin.{}", footage.0)).center();
    let path = still("project_host_drop_on_bin_row.png");
    // the host reports logical pixels; egui's zoom is 1 here
    let events = std::rc::Rc::new(std::cell::RefCell::new(vec![OsFileDrag::Hover(at)]));
    let feed = events.clone();
    h.state_mut().hooks.os_file_drags = Some(Box::new(move || std::mem::take(&mut *feed.borrow_mut())));
    step(&mut h);
    step(&mut h);
    events.borrow_mut().push(OsFileDrag::Drop(at, vec![path]));
    step(&mut h);
    step(&mut h);
    assert_eq!(bin_of(&h, "project_host_drop_on_bin_row.png"), Some(footage), "status: {}", h.state().ui.status);
}

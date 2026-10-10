//! Files the OS asks the app to open (`HostHooks::opened_files`, fed on macOS by the
//! open-documents Apple Event of a Finder double-click): a `.fcproj` among them is opened like
//! File ▸ Open, other files are imported, and a missing file only sets the status line.

use std::cell::RefCell;
use std::rc::Rc;

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

#[test]
fn opened_files_open_the_project_and_survive_bad_paths() {
    let dir = std::env::temp_dir().join(format!("filmcraft-opened-files-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("Double Clicked.fcproj").to_string_lossy().to_string();
    let mut saved = Session::default();
    saved.execute("file.openDemoProject", json!({})).unwrap();
    saved.execute("file.saveAs", json!({"path": path})).unwrap();

    // what the host hands over, one batch per poll
    let batches: Rc<RefCell<Vec<Vec<String>>>> = Rc::new(RefCell::new(vec![
        vec![dir.join("missing.mov").to_string_lossy().to_string(), path.clone()],
        vec![dir.join("gone.FCPROJ").to_string_lossy().to_string()],
    ]));
    let mut app = FilmcraftApp::new(Session::default());
    let feed = batches.clone();
    app.hooks.opened_files = Some(Box::new(move || {
        let mut b = feed.borrow_mut();
        if b.is_empty() { Vec::new() } else { b.remove(0) }
    }));
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).build_eframe(move |_cc| app);
    h.step();
    assert_eq!(h.state().session.path.as_deref(), Some(path.as_str()), "the project was opened");
    assert!(h.state().session.project.sequences().next().is_some());
    // a project that is gone: the open project stays, the error is shown
    h.step();
    assert!(batches.borrow().is_empty());
    assert_eq!(h.state().session.path.as_deref(), Some(path.as_str()));
    assert!(!h.state().ui.status.is_empty(), "the failure is reported");
    let _ = std::fs::remove_dir_all(&dir);
}

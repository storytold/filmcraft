//! Opening a project by path goes through the host's `open_project` hook when it has one.

use std::cell::RefCell;
use std::rc::Rc;

use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;

/// The host hook opens the project when set.
#[test]
fn the_host_hook_opens_the_project_when_set() {
    let mut app = FilmcraftApp::new(Session::default());
    let seen: Rc<RefCell<Vec<String>>> = Rc::default();
    let s = seen.clone();
    app.hooks.open_project = Some(Box::new(move |p| s.borrow_mut().push(p.to_string())));
    let r = app.open_project_path("/data/a.fcproj").unwrap();
    assert_eq!(r["pending"], true);
    assert_eq!(*seen.borrow(), ["/data/a.fcproj"]);
    assert!(app.session.path.is_none(), "the hook, not file.open, opens the project");
}

/// Without the hook file open runs.
#[test]
fn without_the_hook_file_open_runs() {
    let mut app = FilmcraftApp::new(Session::default());
    let e = app.open_project_path("/definitely/not/here.fcproj").unwrap_err();
    assert!(e.contains("here.fcproj"), "{e}");
    assert_eq!(app.ui.status, e);
}

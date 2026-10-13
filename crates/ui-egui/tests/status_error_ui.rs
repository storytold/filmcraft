//! An error in the status bar is easy to miss at the size and colour of a note (#1267): it is
//! drawn larger, in the danger colour, with its own automation id, and stays until it is clicked
//! away or twenty seconds have passed. A plain status is as before.

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;

fn app() -> Harness<'static, FilmcraftApp> {
    let mut session = Session::default();
    session.execute("file.openDemoProject", serde_json::json!({})).expect("demo project");
    let mut h = Harness::builder().with_size(egui::vec2(1400.0, 900.0)).with_max_steps(10_000).build_eframe(move |_cc| FilmcraftApp::new(session));
    for _ in 0..4 {
        h.step();
    }
    h
}

fn has(h: &Harness<'static, FilmcraftApp>, id: &str) -> bool {
    h.state().auto.elements.iter().any(|e| e.id == id)
}

#[test]
fn an_error_is_marked_and_a_note_is_not() {
    let mut h = app();
    h.state_mut().ui.status = "Saved".into();
    h.step();
    h.step();
    assert!(!has(&h, "status.error"), "a note is not an error");
    h.state_mut().ui.fail("Cannot export: the folder is read-only");
    h.step();
    h.step();
    assert!(has(&h, "status.error"), "an error has its own element");
    assert!(h.state().ui.status_is_error());
    // a later note replaces it, and the emphasis goes with the text
    h.state_mut().ui.status = "Saved".into();
    h.step();
    h.step();
    assert!(!h.state().ui.status_is_error());
    assert!(!has(&h, "status.error"));
}

#[test]
fn a_click_on_the_status_bar_dismisses_an_error() {
    let mut h = app();
    h.state_mut().ui.fail("Import failed: unsupported file");
    h.step();
    h.step();
    let e = h.state().auto.elements.iter().find(|e| e.id == "status.error").expect("the error is shown").rect;
    let at = egui::pos2(e[0] + e[2] / 2.0, e[1] + e[3] / 2.0);
    h.event(egui::Event::PointerMoved(at));
    h.step();
    for pressed in [true, false] {
        h.event(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() });
        h.step();
    }
    h.step();
    assert!(h.state().ui.status.is_empty(), "the click cleared it");
}

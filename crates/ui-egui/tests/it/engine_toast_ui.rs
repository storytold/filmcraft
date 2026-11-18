//! An engine toast (#2443) reaches the status bar, for a message and for an error.

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;

fn harness() -> Harness<'static, FilmcraftApp> {
    let app = FilmcraftApp::new(Session::default());
    let mut harness = Harness::builder().with_size(egui::vec2(1200.0, 800.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    for _ in 0..3 {
        harness.step();
    }
    harness
}

#[test]
fn engine_toasts_show_in_the_status_bar() {
    let mut h = harness();
    h.state_mut().session.toast("Saved the thing");
    for _ in 0..2 {
        h.step();
    }
    assert_eq!(h.state().ui.status, "Saved the thing");
    h.state_mut().session.error_toast("test", "It failed");
    for _ in 0..2 {
        h.step();
    }
    assert_eq!(h.state().ui.status, "It failed");
}

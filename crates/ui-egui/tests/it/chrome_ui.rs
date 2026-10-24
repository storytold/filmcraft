//! Headless UI tests of the window chrome flags: `UiState::show_header` and
//! `UiState::show_status_bar` default to on, and an app that embeds FilmCraft can turn them off
//! to give the panels the whole window.

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::state::UiState;
use serde_json::json;

const W: f32 = 1600.0;
const H: f32 = 980.0;

fn harness(show_header: bool, show_status_bar: bool) -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    let mut app = FilmcraftApp::new(s);
    app.ui.show_header = show_header;
    app.ui.show_status_bar = show_status_bar;
    let mut harness = Harness::builder().with_size(egui::vec2(W, H)).with_max_steps(10_000).build_eframe(move |_cc| app);
    for _ in 0..4 {
        harness.step();
    }
    harness
}

/// Top and bottom edge of every registered element outside the header (the menu bar, `menu.*`, is part of it).
fn body_extent(h: &Harness<'static, FilmcraftApp>) -> (f32, f32) {
    let els = h.state().auto.query("");
    let body: Vec<_> = els.iter().filter(|e| !e.id.starts_with("header.") && !e.id.starts_with("menu.")).collect();
    assert!(!body.is_empty(), "no panel elements registered");
    let top = body.iter().map(|e| e.rect[1]).fold(f32::INFINITY, f32::min);
    let bottom = body.iter().map(|e| e.rect[1] + e.rect[3]).fold(f32::NEG_INFINITY, f32::max);
    (top, bottom)
}

#[test]
fn header_and_status_bar_are_shown_by_default() {
    assert!(UiState::default().show_header);
    assert!(UiState::default().show_status_bar);
    let h = harness(true, true);
    assert!(!h.state().auto.query("header.").is_empty());
    let (top, bottom) = body_extent(&h);
    assert!(top >= 38.0, "panels start under the header: {top}");
    assert!(bottom <= H - 20.0, "panels end above the status bar: {bottom}");
    assert!(h.state().ui_error.is_none(), "{:?}", h.state().ui_error);
}

#[test]
fn hidden_header_and_status_bar_give_the_panels_the_window() {
    let h = harness(false, false);
    assert!(h.state().auto.query("header.").is_empty(), "header drawn while hidden");
    let (top, bottom) = body_extent(&h);
    assert!(top < 38.0, "panels start at the top of the window: {top}");
    assert!(bottom > H - 20.0, "panels reach the bottom of the window: {bottom}");
    assert!(h.state().ui_error.is_none(), "{:?}", h.state().ui_error);
}

#[test]
fn saved_ui_state_without_the_flags_shows_the_chrome() {
    let mut v = serde_json::to_value(UiState::default()).unwrap();
    let obj = v.as_object_mut().unwrap();
    assert!(obj.remove("show_header").is_some());
    assert!(obj.remove("show_status_bar").is_some());
    let ui: UiState = serde_json::from_value(v).unwrap();
    assert!(ui.show_header);
    assert!(ui.show_status_bar);
}

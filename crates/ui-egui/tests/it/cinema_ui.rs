//! Cinema mode (#3652), through the control channel: `view.fullScreenProgram` shows the Program
//! monitor alone, Esc leaves it and gives back the focus, the playback keys keep working, and it
//! shows the finished picture whatever the monitor's display settings are.

use crate::support::{Drive, Launch, driver};
use filmcraft_ui_egui::dock::PanelKind;
use filmcraft_ui_egui::state::{DisplayMode, UiState};
use serde_json::{Value, json};

driver!(Driver);

impl Driver {
    fn cinema(&mut self) -> Value {
        self.ok("ui.menu.invoke", json!({"id": "view.fullScreenProgram"}))
    }
    fn ids(&mut self, prefix: &str) -> Vec<String> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().expect("elements").iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
}

#[test]
fn entering_and_leaving_keeps_the_focus() {
    let mut d = Driver::demo();
    d.harness.state_mut().ui.focused = PanelKind::Timeline;
    assert_eq!(d.cinema()["fullScreenProgram"], true);
    assert!(d.harness.state().ui.fullscreen_program);
    assert_eq!(d.harness.state().ui.focused, PanelKind::Program, "the Program monitor takes the focus");
    assert_eq!(d.cinema()["fullScreenProgram"], false, "the command leaves it too");
    assert!(!d.harness.state().ui.fullscreen_program);
    assert_eq!(d.harness.state().ui.focused, PanelKind::Timeline, "the focus goes back");
}

#[test]
fn escape_leaves_and_gives_the_focus_back() {
    let mut d = Driver::demo();
    d.harness.state_mut().ui.focused = PanelKind::Timeline;
    d.cinema();
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    assert!(!d.harness.state().ui.fullscreen_program, "Esc leaves cinema mode");
    assert_eq!(d.harness.state().ui.focused, PanelKind::Timeline);
}

#[test]
fn the_playback_key_works_in_cinema_mode() {
    let mut d = Driver::demo();
    d.cinema();
    assert!(!d.harness.state().playback.playing);
    d.ok("ui.key", json!({"key": "Space"}));
    d.frames(2);
    assert!(d.harness.state().playback.playing, "Space plays");
    assert!(d.harness.state().ui.fullscreen_program, "and cinema mode stays on");
}

#[test]
fn cinema_mode_shows_the_composite_and_keeps_the_saved_display() {
    let mut d = Driver::demo();
    d.harness.state_mut().ui.program.display = DisplayMode::Comparison;
    assert!(!d.ids("program.compare").is_empty(), "the Comparison View is showing");
    d.cinema();
    assert!(d.ids("program.compare").is_empty(), "no reference frame in cinema mode");
    assert_eq!(d.harness.state().ui.program.display, DisplayMode::Comparison, "the saved display stays");
    d.cinema();
    assert!(!d.ids("program.compare").is_empty(), "and comes back");
}

#[test]
fn cinema_mode_is_not_saved_with_the_ui_state() {
    let state = UiState { fullscreen_program: true, ..UiState::default() };
    let saved = serde_json::to_string(&state).expect("the state serializes").to_lowercase();
    assert!(!saved.contains("fullscreenprogram") && !saved.contains("fullscreen_program"), "only the command turns it on");
}

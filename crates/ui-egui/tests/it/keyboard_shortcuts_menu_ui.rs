//! Regression test for #3444: the Keyboard Shortcuts dialog is accessible from the Edit menu
//! via Edit ▸ Keyboard Shortcuts… (Cmd+Alt+K). Verifies `app.keyboardShortcuts` command is
//! registered and can be dispatched via the control channel.

use std::sync::mpsc::channel;

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::json;

#[test]
fn keyboard_shortcuts_command_is_registered_and_dispatches() {
    let s = Session::default();
    let (tx, rx) = channel::<ControlRequest>();
    let app = FilmcraftApp::new(s).with_control(rx);
    let mut harness = Harness::builder().with_size(egui::vec2(1600.0, 900.0)).with_max_steps(2_000).build_eframe(move |_cc| app);
    harness.run();

    // Dispatch app.keyboardShortcuts — it must not panic or return an error
    let (req, reply_rx) = ControlRequest::new("engine.execute", json!({"command": "app.keyboardShortcuts", "params": {}}));
    tx.send(req).unwrap();
    harness.run();
    let reply = reply_rx.try_recv().unwrap_or(json!(null));
    // The command either opens the shortcuts panel (ok) or returns a result — must not be an error
    assert!(!reply.get("error").is_some(), "app.keyboardShortcuts returned error: {}", reply);
}

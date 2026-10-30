use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::{
    FilmcraftApp, menus,
    speed_editor::{Button, Event},
};
use serde_json::json;

fn app() -> FilmcraftApp {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    session.execute("playhead.set", json!({"frame":24})).unwrap();
    FilmcraftApp::new(session)
}

#[test]
fn devices_menu_opens_a_nonmodal_window_with_photo_and_connection_status() {
    let mut harness = Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).build_eframe(|_| app());
    harness.step();
    let ctx = harness.ctx.clone();
    let menu = menus::menu_items(harness.state());
    assert!(menu.iter().any(|item| item.id == "devices.speedEditor" && item.label == "DaVinci Speed Editor" && item.path == ["Devices"]));
    menus::invoke(harness.state_mut(), &ctx, "devices.speedEditor", json!({})).unwrap();
    harness.step();
    harness.step();
    assert!(harness.state().dialog.is_none(), "diagnostics must not be a modal that hides or blocks USB input");
    for id in [
        "speedEditor.debugger.window",
        "speedEditor.debugger.board",
        "speedEditor.debugger.button.split",
        "speedEditor.debugger.button.smoothCut",
        "speedEditor.debugger.wheel",
        "speedEditor.debugger.monitorOnly",
        "speedEditor.debugger.sensitivity",
        "speedEditor.debugger.clear",
        "speedEditor.debugger.close",
    ] {
        assert!(harness.state().auto.find(id).is_some(), "missing {id}");
    }
    assert!(harness.state().auto.find("speedEditor.debugger.connection").unwrap().label.contains("Disconnected"));
    assert_eq!(harness.state().auto.find("speedEditor.debugger.udevHint").is_some(), cfg!(target_os = "linux"));
    let window = harness.state().auto.find("speedEditor.debugger.window").unwrap().rect;
    for id in ["speedEditor.debugger.board", "speedEditor.debugger.clear", "speedEditor.debugger.close"] {
        let rect = harness.state().auto.find(id).unwrap().rect;
        assert!(rect[1] >= window[1] && rect[1] + rect[3] <= window[1] + window[3], "{id} must be visible in the default window");
    }
    harness.state_mut().speed_editor_event(&ctx, Event::Connection { connected: true, detail: "Connected via USB (/dev/hidraw3)".into() }).unwrap();
    harness.step();
    assert!(harness.state().auto.find("speedEditor.debugger.connection").unwrap().label.contains("Connected"));
    assert!(harness.state().auto.find("speedEditor.debugger.udevHint").is_none());
    menus::invoke(harness.state_mut(), &ctx, "devices.speedEditor", json!({"open":false})).unwrap();
    harness.step();
    harness.step();
    assert!(!harness.state().ui.speed_editor_debugger.open);
    assert!(!harness.state().auto.all().iter().any(|element| element.id == "speedEditor.debugger.window"));
}

#[test]
fn sensitivity_slider_changes_saved_gain_without_touching_the_project() {
    use filmcraft_ui_egui::control::ControlRequest;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("preferences.json");
    let (tx, rx) = std::sync::mpsc::channel();
    let mut app = app();
    app.session.prefs_path = Some(path.clone());
    let before = app.session.project.to_json();
    let mut harness = Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).build_eframe(|_| app.with_control(rx));
    let ctx = harness.ctx.clone();
    menus::invoke(harness.state_mut(), &ctx, "devices.speedEditor", json!({})).unwrap();
    for _ in 0..3 {
        harness.step();
    }
    let (request, reply) = ControlRequest::new(
        "ui.drag",
        json!({"from":{"id":"speedEditor.debugger.sensitivity","fx":0.3},
        "to":{"id":"speedEditor.debugger.sensitivity","fx":0.05},"steps":8}),
    );
    tx.send(request).unwrap();
    let mut result = None;
    for _ in 0..120 {
        let ctx = harness.ctx.clone();
        let mut raw = std::mem::take(harness.input_mut());
        eframe::App::raw_input_hook(harness.state_mut(), &ctx, &mut raw);
        *harness.input_mut() = raw;
        harness.step();
        if let Ok(reply) = reply.try_recv() {
            result = Some(reply);
            break;
        }
    }
    assert_eq!(result.unwrap()["ok"], true);
    let percent = harness.state().session.prefs.general.speed_editor_sensitivity;
    assert!((1..100).contains(&percent), "dragging left should slow the dial: {percent}");
    assert_eq!(filmcraft_engine::autosave::Preferences::load(&path).general.speed_editor_sensitivity, percent);
    assert_eq!(harness.state().session.project.to_json(), before);
    menus::invoke(harness.state_mut(), &ctx, "devices.speedEditor", json!({"sensitivity":25})).unwrap();
    assert_eq!(filmcraft_engine::autosave::Preferences::load(&path).general.speed_editor_sensitivity, 25);
}

#[test]
fn monitor_only_highlights_keys_and_dial_and_keeps_the_project_and_playhead() {
    let mut harness = Harness::builder().with_size(egui::vec2(1600.0, 1000.0)).build_eframe(|_| app());
    harness.step();
    let ctx = harness.ctx.clone();
    menus::invoke(harness.state_mut(), &ctx, "devices.speedEditor", json!({})).unwrap();
    let before = harness.state().session.project.to_json();
    let ph = harness.state().session.playhead();
    for params in [json!({"button":"split","pressed":true}), json!({"button":"smoothCut","pressed":true}), json!({"wheel":256})] {
        menus::invoke(harness.state_mut(), &ctx, "speedEditor.input", params).unwrap();
    }
    harness.step();
    assert_eq!(harness.state().session.project.to_json(), before);
    assert_eq!(harness.state().session.playhead(), ph);
    assert!(harness.state().speed_editor.telemetry.highlighted(Button::Split, 0.0));
    assert!(harness.state().speed_editor.telemetry.highlighted(Button::SmoothCut, 0.0));
    assert!(harness.state().auto.find("speedEditor.debugger.lastWheel").unwrap().label.contains("+256"));
    menus::invoke(harness.state_mut(), &ctx, "speedEditor.input", json!({"button":"split","pressed":false})).unwrap();
    assert!(!harness.state().speed_editor.telemetry.pressed.contains(&Button::Split));
    let released_at = harness.state().speed_editor.telemetry.history.back().unwrap().time;
    assert!(harness.state().speed_editor.telemetry.highlighted(Button::Split, released_at));
    menus::invoke(harness.state_mut(), &ctx, "speedEditor.input", json!({"button":"smoothCut","pressed":false})).unwrap();
    menus::invoke(harness.state_mut(), &ctx, "speedEditor.debugger.clear", json!({})).unwrap();
    assert!(harness.state().speed_editor.telemetry.history.is_empty());
    assert_eq!(harness.state().session.project.to_json(), before);
    for pressed in [true, false, true, false] {
        menus::invoke(harness.state_mut(), &ctx, "speedEditor.input", json!({"button":"audioLevel","pressed":pressed})).unwrap();
    }
    harness.step();
    assert_eq!(harness.state().speed_editor.telemetry.double_taps, 1);
    assert!(harness.state().auto.find("speedEditor.debugger.lastGesture").unwrap().label.contains("MARK"));
    assert_eq!(harness.state().session.project.to_json(), before);
}

#[test]
fn live_mode_retains_edit_bindings_and_closing_monitor_mode_waits_for_release() {
    let mut app = app();
    let ctx = egui::Context::default();
    let before = app.session.project.to_json();
    menus::invoke(&mut app, &ctx, "devices.speedEditor", json!({})).unwrap();
    menus::invoke(&mut app, &ctx, "speedEditor.input", json!({"button":"split","pressed":true})).unwrap();
    menus::invoke(&mut app, &ctx, "devices.speedEditor", json!({"open":false})).unwrap();
    menus::invoke(&mut app, &ctx, "speedEditor.input", json!({"wheel":128})).unwrap();
    assert_eq!(app.session.project.to_json(), before);
    assert_eq!(app.session.sequence_rate().frame_at(app.session.playhead()), 24);
    menus::invoke(&mut app, &ctx, "speedEditor.input", json!({"button":"split","pressed":false})).unwrap();
    menus::invoke(&mut app, &ctx, "devices.speedEditor", json!({"monitorOnly":false})).unwrap();
    menus::invoke(&mut app, &ctx, "speedEditor.input", json!({"button":"cut","pressed":true})).unwrap();
    assert_ne!(app.session.project.to_json(), before);
    app.session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(app.session.project.to_json(), before);
}

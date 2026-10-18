//! Review speed belongs to the Program transport, never to timeline clip effects.
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::Tick;
use filmcraft_ui_egui::{FilmcraftApp, menus};
use serde_json::json;

fn app() -> FilmcraftApp {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    FilmcraftApp::new(s)
}

#[test]
fn review_rates_are_validated_retained_and_never_edit_the_project() {
    let mut app = app();
    let ctx = egui::Context::default();
    let before = app.session.project.to_json();
    let undo = app.session.history.undo.len();
    for rate in [0.5, 1.0, 1.5, 2.0, 2.5, 3.0] {
        menus::invoke(&mut app, &ctx, "playback.setReviewSpeed", json!({"speed":rate})).unwrap();
        assert!(!app.playback.playing);
        menus::invoke(&mut app, &ctx, "playback.toggle", json!({"monitor":"program"})).unwrap();
        assert_eq!(app.playback.speed, rate);
        assert!(app.playback.playing);
        menus::invoke(&mut app, &ctx, "playback.toggle", json!({"monitor":"program"})).unwrap();
        assert_eq!(app.playback.review_speed, rate);
    }
    for bad in [json!({}), json!({"speed":0}), json!({"speed":-1}), json!({"speed":4}), json!({"speed":"2"})] {
        assert!(menus::invoke(&mut app, &ctx, "playback.setReviewSpeed", bad).is_err());
        assert_eq!(app.playback.review_speed, 3.0);
    }
    app.session.set_playhead(Tick::from_seconds_f64(3.0));
    let playhead = app.session.playhead();
    app.play(2.0);
    menus::invoke(&mut app, &ctx, "playback.setReviewSpeed", json!({"speed":0.5})).unwrap();
    assert!(app.playback.playing);
    assert_eq!(app.playback.speed, 0.5);
    assert_eq!(app.session.playhead(), playhead);
    assert_eq!(app.session.project.to_json(), before);
    assert_eq!(app.session.history.undo.len(), undo);
}

#[test]
fn review_clock_advances_at_the_selected_rate_and_changes_live() {
    let mut h = Harness::builder().with_step_dt(0.1).with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_| app());
    h.step();
    let ctx = h.ctx.clone();
    let before = h.state().session.project.to_json();
    menus::invoke(h.state_mut(), &ctx, "playback.setReviewSpeed", json!({"speed":2.5})).unwrap();
    menus::invoke(h.state_mut(), &ctx, "playback.toggle", json!({"monitor":"program"})).unwrap();
    for _ in 0..30 {
        h.step();
    }
    let start = h.state().session.playhead().seconds();
    for _ in 0..10 {
        h.step();
    }
    let fast = h.state().session.playhead().seconds() - start;
    assert!((fast - 2.5).abs() < 0.1, "{fast}");
    menus::invoke(h.state_mut(), &ctx, "playback.setReviewSpeed", json!({"speed":0.5})).unwrap();
    for _ in 0..30 {
        h.step();
    }
    let start = h.state().session.playhead().seconds();
    for _ in 0..10 {
        h.step();
    }
    let slow = h.state().session.playhead().seconds() - start;
    assert!((slow - 0.5).abs() < 0.1, "{slow}");
    h.state_mut().stop();
    assert_eq!(h.state().session.project.to_json(), before);
}

#[test]
fn review_dropdown_is_visible_in_normal_and_maximized_program() {
    let mut h = Harness::builder().with_size(egui::vec2(1280.0, 800.0)).build_eframe(move |_| app());
    for _ in 0..4 {
        h.step();
    }
    let ctx = h.ctx.clone();
    let dropdown = h.state().auto.find("program.playbackSpeed").unwrap().rect;
    let resolution = h.state().auto.find("program.resolution").unwrap().rect;
    assert!(dropdown[0] + dropdown[2] < resolution[0]);
    h.state_mut().ui.focused = filmcraft_ui_egui::dock::PanelKind::Program;
    menus::invoke(h.state_mut(), &ctx, "window.maximizeFrame", json!({})).unwrap();
    for _ in 0..4 {
        h.step();
    }
    assert!(h.state().auto.find("program.playbackSpeed").is_some());
}

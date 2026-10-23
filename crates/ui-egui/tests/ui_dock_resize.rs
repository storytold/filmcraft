//! Shared pane resizing in the full application, including application shortcut ownership.

use egui::{Event, Id, Key, Modifiers, PointerButton, Rect, pos2, vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::dock::{DockNode, PanelKind, SplitSize};
use filmcraft_ui_egui::theme::ThemeKind;
use serde_json::json;

fn step(h: &mut Harness<'_, FilmcraftApp>) {
    h.step();
    assert!(h.state().ui_error.is_none(), "the app caught a UI panic: {:?}", h.state().ui_error);
}

fn frames(h: &mut Harness<'_, FilmcraftApp>, count: usize) {
    for _ in 0..count {
        step(h);
    }
}

fn snapshots(h: &mut Harness<'_, FilmcraftApp>, directory: Option<&std::path::Path>, phase: &str) {
    let Some(directory) = directory else { return };
    std::fs::create_dir_all(directory).unwrap();
    for (name, theme) in [("dark", ThemeKind::Dark), ("medium", ThemeKind::Medium), ("light", ThemeKind::Light)] {
        let ctx = h.ctx.clone();
        h.state_mut().set_theme(&ctx, theme);
        frames(h, 3);
        h.render().unwrap().save(directory.join(format!("filmcraft-{phase}-{name}.png"))).unwrap();
    }
    let ctx = h.ctx.clone();
    h.state_mut().set_theme(&ctx, ThemeKind::Dark);
    frames(h, 3);
}

fn first_size(h: &Harness<'_, FilmcraftApp>) -> f32 {
    match h.state().ui.dock {
        DockNode::Split { size: SplitSize::FixedA(value), .. } => value,
        ref other => panic!("fixed-size mode changed: {other:?}"),
    }
}

#[test]
fn focused_dock_resize_owns_arrows_and_keeps_pointer_dragging() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let app = FilmcraftApp::new(session);
    // Opt in to GPU snapshots while the ordinary test remains headless and renderer-independent.
    let directory = std::env::var_os("CRAFT_UI_PANES_SNAPSHOT_DIR").map(std::path::PathBuf::from);
    let mut builder = Harness::builder().with_size(vec2(1400.0, 900.0)).with_max_steps(10_000);
    if directory.is_some() {
        builder = builder.wgpu();
    }
    let mut h = builder.build_eframe(move |_| app);
    frames(&mut h, 4);
    snapshots(&mut h, directory.as_deref(), "workspace");
    h.state_mut().ui.focused = PanelKind::Timeline;
    h.state_mut().ui.dock = DockNode::Split {
        vertical: false,
        size: SplitSize::FixedA(400.0),
        a: Box::new(DockNode::Tabs { panels: vec![PanelKind::Project], active: 0 }),
        b: Box::new(DockNode::Tabs { panels: vec![PanelKind::Timeline], active: 0 }),
    };
    frames(&mut h, 3);
    let before_project = h.state().session.project.clone();
    let before_playheads = h.state().session.state.playheads.clone();
    let id = Id::new(("dock-gutter", String::new()));
    h.ctx.memory_mut(|memory| memory.request_focus(id));
    frames(&mut h, 2);
    assert_eq!(h.ctx.memory(|memory| memory.focused()), Some(id));
    snapshots(&mut h, directory.as_deref(), "focused-resize");
    h.event(Event::Key { key: Key::ArrowRight, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    step(&mut h);
    h.event(Event::Key { key: Key::ArrowRight, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    step(&mut h);
    assert_eq!(first_size(&h), 408.0);
    assert_eq!(h.state().session.state.playheads, before_playheads, "the Timeline shortcut must not seek");
    assert!(std::sync::Arc::ptr_eq(&h.state().session.project, &before_project));

    frames(&mut h, 2);
    let element = h.state().auto.find("dock.gutter.").unwrap();
    let from = Rect::from_min_size(pos2(element.rect[0], element.rect[1]), vec2(element.rect[2], element.rect[3])).center();
    h.event(Event::PointerMoved(from));
    step(&mut h);
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    step(&mut h);
    for offset in [5.0, 10.0, 15.0, 20.0] {
        h.event(Event::PointerMoved(from + vec2(offset, 0.0)));
        step(&mut h);
    }
    h.event(Event::PointerButton { pos: from + vec2(20.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    step(&mut h);
    assert_eq!(first_size(&h), 428.0);
    assert_eq!(h.state().session.state.playheads, before_playheads);
}

fn press(h: &mut Harness<'_, FilmcraftApp>, key: Key) {
    for pressed in [true, false] {
        h.event(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
        step(h);
    }
    frames(h, 2);
}

#[test]
fn space_plays_after_a_pane_is_resized_with_the_pointer() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(move |_| app);
    frames(&mut h, 4);
    h.state_mut().ui.focused = PanelKind::Timeline;
    h.state_mut().ui.dock = DockNode::Split {
        vertical: false,
        size: SplitSize::FixedA(400.0),
        a: Box::new(DockNode::Tabs { panels: vec![PanelKind::Project], active: 0 }),
        b: Box::new(DockNode::Tabs { panels: vec![PanelKind::Timeline], active: 0 }),
    };
    frames(&mut h, 3);
    let element = h.state().auto.find("dock.gutter.").unwrap();
    let from = Rect::from_min_size(pos2(element.rect[0], element.rect[1]), vec2(element.rect[2], element.rect[3])).center();
    h.event(Event::PointerMoved(from));
    step(&mut h);
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    step(&mut h);
    for offset in [5.0, 10.0, 15.0, 20.0] {
        h.event(Event::PointerMoved(from + vec2(offset, 0.0)));
        step(&mut h);
    }
    h.event(Event::PointerButton { pos: from + vec2(20.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    frames(&mut h, 3);
    assert_eq!(first_size(&h), 420.0);
    // The divider keeps keyboard focus so arrows can nudge it; that must not cost the editor its shortcuts.
    assert_eq!(h.ctx.memory(|memory| memory.focused()), Some(Id::new(("dock-gutter", String::new()))));
    press(&mut h, Key::Space);
    assert!(h.state().playback.playing, "Space must start playback after resizing a pane (#1482)");
    press(&mut h, Key::Space);
    assert!(!h.state().playback.playing);
}

/// Only the primary button moves a panel: a right-button drag on a panel's grip starts no panel
/// drag, a left-button one does.
#[test]
fn only_a_left_button_drag_on_a_panel_grip_moves_the_panel() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).with_max_steps(10_000).build_eframe(move |_| app);
    frames(&mut h, 4);
    let r = h.state().auto.find("panel.drag.Timeline").expect("the Timeline's grip").rect;
    let grip = pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    // the dock keeps the panel being dragged under this id while the drag lasts
    let dragging = |h: &Harness<'_, FilmcraftApp>| h.ctx.data(|d| d.get_temp::<PanelKind>(Id::new("dock-panel-drag")));
    for (button, moves) in [(PointerButton::Secondary, false), (PointerButton::Primary, true)] {
        // the pointer arrives a frame before the press, as a hand on a mouse does
        h.input_mut().events.push(Event::PointerMoved(grip));
        frames(&mut h, 2);
        h.input_mut().events.push(Event::PointerButton { pos: grip, button, pressed: true, modifiers: Modifiers::NONE });
        frames(&mut h, 1);
        for i in 1..=6 {
            h.input_mut().events.push(Event::PointerMoved(grip + vec2(0.0, 10.0 * i as f32)));
            frames(&mut h, 1);
        }
        assert_eq!(dragging(&h).is_some(), moves, "{button:?}: a panel drag {}", if moves { "starts" } else { "does not start" });
        h.input_mut().events.push(Event::PointerButton { pos: grip + vec2(0.0, 60.0), button, pressed: false, modifiers: Modifiers::NONE });
        h.input_mut().events.push(Event::Key { key: Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        frames(&mut h, 3);
    }
}

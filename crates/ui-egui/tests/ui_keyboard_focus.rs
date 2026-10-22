//! Keyboard traversal reaches actual controls and preserves document/playback state.

use std::sync::Arc;

use egui::{Event, Id, Key, Modifiers, Rect, vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::{
    FilmcraftApp,
    dock::{DockNode, PanelKind},
    state::Tool,
};
use serde_json::json;

/// Press and release `key` with `modifiers`, then let the app settle; a UI error fails the test.
fn key(h: &mut Harness<'_, FilmcraftApp>, key: Key, modifiers: Modifiers) {
    for pressed in [true, false] {
        h.event(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
        h.step();
    }
    h.run_steps(2);
    assert!(h.state().ui_error.is_none());
}

/// Tab reaches each custom-painted tool, Shift+Tab goes back, and Space activates only the
/// focused tool — never playback — with a visible focus outline.
#[test]
fn tab_traversal_and_space_select_only_the_focused_tool_with_visible_focus() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let project = session.project.clone();
    let playheads = session.state.playheads.clone();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).build_eframe(move |_| app);
    h.run_steps(4);
    h.state_mut().ui.dock = DockNode::Tabs { panels: vec![PanelKind::Tools], active: 0 };
    h.run_steps(3);
    let first = Id::new(("tool", "Selection".to_string()));
    let second = Id::new(("tool", "TrackSelectForward".to_string()));
    let third = Id::new(("tool", "Ripple".to_string()));
    for _ in 0..128 {
        key(&mut h, Key::Tab, Modifiers::NONE);
        if h.ctx.memory(|m| m.focused()) == Some(first) {
            break;
        }
    }
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(first), "Tab must reach the first tool");
    key(&mut h, Key::Tab, Modifiers::NONE);
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(second));
    key(&mut h, Key::Tab, Modifiers::NONE);
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(third));
    key(&mut h, Key::Tab, Modifiers::SHIFT);
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(second));
    assert_eq!(h.state().ui.tool, Tool::Selection, "traversal does not activate controls");
    key(&mut h, Key::Space, Modifiers::NONE);
    assert_eq!(h.state().ui.tool, Tool::TrackSelectForward);
    assert!(!h.state().playback.playing, "Space belongs to the focused tool");
    assert_eq!(h.state().session.state.playheads, playheads);
    assert!(Arc::ptr_eq(&h.state().session.project, &project));
    let response = h.ctx.read_response(second).unwrap();
    assert!(
        h.output().shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Rect(r) if r.rect == response.rect.shrink(2.0)
            && r.stroke.color == h.state().tokens.focus && r.stroke.width > 0.0)),
        "the focused custom-painted tool needs a visible focus outline"
    );
}

/// Tab reaches the dock tabs, Space and Enter switch the visible panel, and the focused tab shows
/// a focus outline without touching the project or playback state.
#[test]
fn dock_tab_traversal_and_enter_change_the_visible_panel_without_editing() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let project = session.project.clone();
    let playheads = session.state.playheads.clone();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).build_eframe(move |_| app);
    h.run_steps(4);
    h.state_mut().ui.show_header = false;
    h.state_mut().ui.show_status_bar = false;
    h.state_mut().ui.dock = DockNode::Tabs { panels: vec![PanelKind::Project, PanelKind::Effects], active: 0 };
    h.run_steps(3);
    let first = Id::new(("tab", String::new(), 0usize));
    let second = Id::new(("tab", String::new(), 1usize));
    for _ in 0..128 {
        key(&mut h, Key::Tab, Modifiers::NONE);
        if h.ctx.memory(|m| m.focused()) == Some(second) {
            break;
        }
    }
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(second));
    assert!(matches!(h.state().ui.dock, DockNode::Tabs { active: 0, .. }));
    let response = h.ctx.read_response(second).unwrap();
    assert!(
        h.output().shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Rect(r) if r.rect == response.rect.shrink(2.0)
            && r.stroke.color == h.state().tokens.focus && r.stroke.width > 0.0)),
        "the focused dock tab needs a visible focus outline"
    );
    key(&mut h, Key::Space, Modifiers::NONE);
    assert!(matches!(h.state().ui.dock, DockNode::Tabs { active: 1, .. }));
    assert_eq!(h.state().ui.focused, PanelKind::Effects);
    key(&mut h, Key::Tab, Modifiers::SHIFT);
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(first));
    key(&mut h, Key::Enter, Modifiers::NONE);
    assert!(matches!(h.state().ui.dock, DockNode::Tabs { active: 0, .. }));
    assert_eq!(h.state().ui.focused, PanelKind::Project);
    assert!(Arc::ptr_eq(&h.state().session.project, &project));
    assert_eq!(h.state().session.state.playheads, playheads);
    assert!(!h.state().playback.playing);
}

/// Selecting a tab must not move the strip: the active tab's panel menu is a reserved slot on every
/// tab, so Properties / Essential Graphics / Text (or any group) stays put when you switch.
#[test]
fn dock_tab_rects_do_not_move_when_the_active_tab_changes() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).build_eframe(move |_| app);
    h.run_steps(4);
    let panels = vec![PanelKind::Properties, PanelKind::EssentialGraphics, PanelKind::Text];
    let tabs = [Id::new(("tab", String::new(), 0usize)), Id::new(("tab", String::new(), 1usize)), Id::new(("tab", String::new(), 2usize))];
    let rects = |h: &Harness<'_, FilmcraftApp>| -> Vec<Rect> { tabs.iter().map(|id| h.ctx.read_response(*id).expect("tab").rect).collect() };
    h.state_mut().ui.dock = DockNode::Tabs { panels: panels.clone(), active: 0 };
    h.run_steps(3);
    let before = rects(&h);
    for active in [1, 2, 0, 2] {
        h.state_mut().ui.dock = DockNode::Tabs { panels: panels.clone(), active };
        h.run_steps(2);
        assert_eq!(rects(&h), before, "selecting tab {active} moved the tabs");
    }
}

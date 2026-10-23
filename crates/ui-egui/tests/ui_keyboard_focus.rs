//! Keyboard traversal reaches actual controls and preserves document/playback state.

use std::sync::Arc;

use egui::{Event, Id, Key, Modifiers, vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::{FilmcraftApp, dock::PanelKind, state::Tool};
use serde_json::json;

fn key(h: &mut Harness<'_, FilmcraftApp>, key: Key, modifiers: Modifiers) {
    for pressed in [true, false] {
        h.event(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers });
        h.step();
    }
    h.run_steps(2);
    assert!(h.state().ui_error.is_none());
}

#[test]
fn tab_traversal_and_space_select_only_the_focused_tool_with_visible_focus() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let project = session.project.clone();
    let playheads = session.state.playheads.clone();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).build_eframe(move |_| app);
    h.run_steps(4);
    h.state_mut().ui.dock =
        craft_ui::docking::Layout { root: Some(craft_ui::docking::Node::Tabs { panels: vec![PanelKind::Tools], active: 0 }), floating: vec![] };
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
    h.state_mut().ui.dock = craft_ui::docking::Layout {
        root: Some(craft_ui::docking::Node::Tabs { panels: vec![PanelKind::Project, PanelKind::Effects], active: 0 }),
        floating: vec![],
    };
    h.run_steps(3);
    let first = craft_ui::docking::DockArea::<PanelKind>::hit_id(filmcraft_ui_egui::dock_area_id(), &craft_ui::docking::Hit::Tab(PanelKind::Project));
    let second = craft_ui::docking::DockArea::<PanelKind>::hit_id(filmcraft_ui_egui::dock_area_id(), &craft_ui::docking::Hit::Tab(PanelKind::Effects));
    for _ in 0..128 {
        key(&mut h, Key::Tab, Modifiers::NONE);
        if h.ctx.memory(|m| m.focused()) == Some(second) {
            break;
        }
    }
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(second));
    assert!(matches!(h.state().ui.dock.root, Some(craft_ui::docking::Node::Tabs { active: 0, .. })));
    let response = h.ctx.read_response(second).unwrap();
    assert!(
        h.output().shapes.iter().any(|shape| matches!(&shape.shape,
        egui::Shape::Rect(r) if r.rect == response.rect.shrink(2.0)
            && r.stroke.color == h.state().tokens.focus && r.stroke.width > 0.0)),
        "the focused dock tab needs a visible focus outline"
    );
    key(&mut h, Key::Space, Modifiers::NONE);
    assert!(matches!(h.state().ui.dock.root, Some(craft_ui::docking::Node::Tabs { active: 1, .. })));
    assert_eq!(h.state().ui.focused, PanelKind::Effects);
    key(&mut h, Key::Tab, Modifiers::SHIFT);
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(first));
    key(&mut h, Key::Enter, Modifiers::NONE);
    assert!(matches!(h.state().ui.dock.root, Some(craft_ui::docking::Node::Tabs { active: 0, .. })));
    assert_eq!(h.state().ui.focused, PanelKind::Project);
    assert!(Arc::ptr_eq(&h.state().session.project, &project));
    assert_eq!(h.state().session.state.playheads, playheads);
    assert!(!h.state().playback.playing);
}

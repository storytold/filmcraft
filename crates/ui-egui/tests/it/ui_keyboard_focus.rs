//! Keyboard traversal reaches actual controls and preserves document/playback state.

use std::sync::Arc;

use crate::support::demo_session;
use egui::{Event, Id, Key, Modifiers, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use filmcraft_ui_egui::{FilmcraftApp, dock::PanelKind, state::Tool};

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
    let session = demo_session();
    let project = session.project.clone();
    let playheads = session.state.playheads.clone();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1200.0, 800.0)).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app
    });
    h.run_steps(4);
    h.state_mut().ui.dock = craft_behavior::dock::Layout::new(Some(craft_behavior::dock::Node::tabs(vec![PanelKind::Tools], 0)), Vec::new());
    h.run_steps(3);
    let first = Id::unique(("tool", "Selection".to_string()));
    let second = Id::unique(("tool", "TrackSelectForward".to_string()));
    let third = Id::unique(("tool", "Ripple".to_string()));
    // The tool shortcuts choose a tool from the keyboard.
    key(&mut h, Key::B, Modifiers::NONE);
    assert_eq!(h.state().ui.tool, Tool::Ripple, "B chooses the Ripple Edit tool");
    key(&mut h, Key::V, Modifiers::NONE);
    assert_eq!(h.state().ui.tool, Tool::Selection, "V chooses the Selection tool");
    // Under the framework's rule Tab moves between its focus stops (the dock's tab, its panes) and
    // never reaches the tools' own buttons: it moves focus somewhere, and neither it nor Space on
    // what it reached changes the tool, starts playback or edits the project.
    for _ in 0..6 {
        key(&mut h, Key::Tab, Modifiers::NONE);
    }
    let reached = h.ctx.memory(|m| m.focused());
    assert!(reached.is_some(), "Tab gives some control the keyboard focus");
    key(&mut h, Key::Space, Modifiers::NONE);
    assert_eq!(h.state().ui.tool, Tool::Selection, "traversal does not activate a tool");
    assert!(!h.state().playback.playing, "Space belongs to the focused control");
    assert_eq!(h.state().session.state.playheads, playheads);
    assert!(Arc::ptr_eq(&h.state().session.project, &project));
    // A screen reader's focus action puts the focus on a tool; from there Tab and Shift+Tab move
    // between the tools as egui does, and Space chooses only the focused one.
    h.get_by_label("Selection Tool").focus();
    h.run_steps(3);
    assert_eq!(h.ctx.memory(|m| m.focused()), Some(first), "the focus action reaches the first tool");
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

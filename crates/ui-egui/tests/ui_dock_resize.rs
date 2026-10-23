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

/// The fill painted on the divider this frame, if any.
fn divider_fill(h: &Harness<'_, FilmcraftApp>, hit: Rect) -> Option<egui::Color32> {
    h.output().shapes.iter().find_map(|clipped| match &clipped.shape {
        egui::Shape::Rect(rect) if hit.contains_rect(rect.rect) && rect.rect.width() < 10.0 && rect.fill != egui::Color32::TRANSPARENT => Some(rect.fill),
        _ => None,
    })
}

#[test]
fn divider_highlight_ends_with_the_hover_after_a_pointer_resize() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(move |_| app);
    frames(&mut h, 4);
    h.state_mut().ui.dock = DockNode::Split {
        vertical: false,
        size: SplitSize::FixedA(400.0),
        a: Box::new(DockNode::Tabs { panels: vec![PanelKind::Project], active: 0 }),
        b: Box::new(DockNode::Tabs { panels: vec![PanelKind::Timeline], active: 0 }),
    };
    frames(&mut h, 3);
    let focus = h.state().tokens.focus;
    let element = h.state().auto.find("dock.gutter.").unwrap();
    let hit = Rect::from_min_size(pos2(element.rect[0], element.rect[1]), vec2(element.rect[2], element.rect[3]));
    let from = hit.center();
    h.event(Event::PointerMoved(from));
    frames(&mut h, 2);
    assert_eq!(divider_fill(&h, hit), Some(focus.gamma_multiply(0.4)), "hovering shows the divider");
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    step(&mut h);
    for offset in [5.0, 10.0, 15.0, 20.0] {
        h.event(Event::PointerMoved(from + vec2(offset, 0.0)));
        step(&mut h);
    }
    // The layout applies the drag after painting, so the divider may still be drawn a step behind.
    let hit = hit.union(hit.translate(vec2(20.0, 0.0)));
    assert_eq!(divider_fill(&h, hit), Some(focus.gamma_multiply(0.9)), "dragging shows the divider");
    h.event(Event::PointerButton { pos: from + vec2(20.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    step(&mut h);
    h.event(Event::PointerMoved(from + vec2(300.0, 200.0)));
    frames(&mut h, 3);
    assert_eq!(first_size(&h), 420.0);
    assert_eq!(h.ctx.memory(|memory| memory.focused()), Some(Id::new(("dock-gutter", String::new()))), "arrows can still nudge it");
    assert_eq!(divider_fill(&h, hit), None, "the divider stays lit after the pointer leaves it (#1769)");
    h.event(Event::PointerMoved(from + vec2(20.0, 0.0)));
    frames(&mut h, 2);
    assert_eq!(divider_fill(&h, hit), Some(focus.gamma_multiply(0.4)), "hovering it again shows it again");
}

fn project_beside_timeline() -> (Harness<'static, FilmcraftApp>, Rect) {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(move |_| app);
    frames(&mut h, 4);
    h.state_mut().ui.dock = DockNode::Split {
        vertical: false,
        size: SplitSize::FixedA(400.0),
        a: Box::new(DockNode::Tabs { panels: vec![PanelKind::Project], active: 0 }),
        b: Box::new(DockNode::Tabs { panels: vec![PanelKind::Timeline], active: 0 }),
    };
    frames(&mut h, 3);
    let element = h.state().auto.find("dock.gutter.").unwrap();
    let hit = Rect::from_min_size(pos2(element.rect[0], element.rect[1]), vec2(element.rect[2], element.rect[3]));
    (h, hit)
}

#[test]
fn divider_highlight_ends_with_the_hover_after_a_click() {
    let (mut h, hit) = project_beside_timeline();
    let from = hit.center();
    h.event(Event::PointerMoved(from));
    frames(&mut h, 2);
    // ResizeHandle senses drag only, and egui starts a drag-only widget's drag on the press: a click
    // takes focus through the same drag_started() as a drag
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    step(&mut h);
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    step(&mut h);
    h.event(Event::PointerMoved(from + vec2(300.0, 200.0)));
    frames(&mut h, 3);
    assert_eq!(first_size(&h), 400.0);
    assert_eq!(h.ctx.memory(|memory| memory.focused()), Some(Id::new(("dock-gutter", String::new()))), "the click focused it");
    assert_eq!(divider_fill(&h, hit), None, "the divider stays lit after a click");
}

#[test]
fn an_arrow_after_a_pointer_resize_shows_the_focus_again() {
    let (mut h, hit) = project_beside_timeline();
    let focus = h.state().tokens.focus;
    let from = hit.center();
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
    h.event(Event::PointerMoved(from + vec2(300.0, 200.0)));
    frames(&mut h, 3);
    let hit = hit.translate(vec2(20.0, 0.0));
    assert_eq!(divider_fill(&h, hit), None);
    // nudging it with the keyboard makes it a keyboard user's divider: the focus fill is back
    press(&mut h, Key::ArrowRight);
    assert_eq!(first_size(&h), 428.0);
    assert_eq!(divider_fill(&h, hit.translate(vec2(8.0, 0.0))), Some(focus), "the arrow shows where the focus is");
}

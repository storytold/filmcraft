//! Shared pane resizing in the full application, including application shortcut ownership.

use egui::{Event, Key, Modifiers, PointerButton, Rect, pos2, vec2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::dock::PanelKind;
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
    match h.state().ui.dock.root {
        Some(craft_ui::docking::Node::Split { size: craft_ui::layout::SplitSize::FixedFirst(value), .. }) => value,
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
    h.state_mut().ui.dock = craft_ui::docking::Layout {
        root: Some(craft_ui::docking::Node::Split {
            axis: craft_ui::layout::SplitAxis::Horizontal,
            size: craft_ui::layout::SplitSize::FixedFirst(400.0),
            first: Box::new(craft_ui::docking::Node::Tabs { panels: vec![PanelKind::Project], active: 0 }),
            second: Box::new(craft_ui::docking::Node::Tabs { panels: vec![PanelKind::Timeline], active: 0 }),
        }),
        floating: vec![],
    };
    frames(&mut h, 3);
    let before_project = h.state().session.project.clone();
    let before_playheads = h.state().session.state.playheads.clone();
    let id = craft_ui::docking::DockArea::<PanelKind>::hit_id(
        filmcraft_ui_egui::dock_area_id(),
        &craft_ui::docking::Hit::Divider(craft_ui::docking::Divider::Split(vec![])),
    );
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
    h.state_mut().ui.dock = craft_ui::docking::Layout {
        root: Some(craft_ui::docking::Node::Split {
            axis: craft_ui::layout::SplitAxis::Horizontal,
            size: craft_ui::layout::SplitSize::FixedFirst(400.0),
            first: Box::new(craft_ui::docking::Node::Tabs { panels: vec![PanelKind::Project], active: 0 }),
            second: Box::new(craft_ui::docking::Node::Tabs { panels: vec![PanelKind::Timeline], active: 0 }),
        }),
        floating: vec![],
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
    // A pointer drag leaves no keyboard focus on the divider, so the editor keeps its shortcuts.
    assert_ne!(
        h.ctx.memory(|memory| memory.focused()),
        Some(craft_ui::docking::DockArea::<PanelKind>::hit_id(
            filmcraft_ui_egui::dock_area_id(),
            &craft_ui::docking::Hit::Divider(craft_ui::docking::Divider::Split(vec![]))
        ))
    );
    press(&mut h, Key::Space);
    assert!(h.state().playback.playing, "Space must start playback after resizing a pane (#1482)");
    press(&mut h, Key::Space);
    assert!(!h.state().playback.playing);
}

/// Space plays while any edge of a floating window's resize grip has keyboard focus, as it does
/// for the bottom-right corner and pane dividers (#1482).
#[test]
fn space_plays_while_any_floating_resize_edge_has_focus() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(move |_| app);
    frames(&mut h, 4);
    let ctx = h.ctx.clone();
    filmcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [300, 200, 400, 300]})).unwrap();
    frames(&mut h, 3);
    use craft_ui::docking::{DockArea, Hit, ResizeEdge};
    for edge in [ResizeEdge::Top, ResizeEdge::Left, ResizeEdge::BottomRight] {
        let grip = DockArea::<PanelKind>::hit_id(filmcraft_ui_egui::dock_area_id(), &Hit::Resize(PanelKind::Info, edge));
        h.ctx.memory_mut(|memory| memory.request_focus(grip));
        frames(&mut h, 2);
        assert_eq!(h.ctx.memory(|memory| memory.focused()), Some(grip), "{edge:?}");
        press(&mut h, Key::Space);
        assert!(h.state().playback.playing, "Space must play with the {edge:?} grip focused");
        press(&mut h, Key::Space);
        assert!(!h.state().playback.playing);
    }
}

/// The divider between groups stacked in a floating window has an automation id, and Space plays
/// while it has keyboard focus (#1482).
#[test]
fn space_plays_while_a_floating_group_divider_has_focus() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let app = FilmcraftApp::new(session);
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(move |_| app);
    frames(&mut h, 4);
    let ctx = h.ctx.clone();
    filmcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [300, 200, 400, 400]})).unwrap();
    filmcraft_ui_egui::menus::invoke(h.state_mut(), &ctx, "window.dockPanel", json!({"panel": "Project", "anchor": "Info", "zone": "bottom"})).unwrap();
    frames(&mut h, 3);
    use craft_ui::docking::{Divider, DockArea, Hit};
    let area = filmcraft_ui_egui::dock_area_id();
    let hit = Hit::Divider(Divider::Group(PanelKind::Info));
    assert!(DockArea::<PanelKind>::hit_rects(&h.ctx, area).iter().any(|(drawn, _)| *drawn == hit), "no group divider drawn");
    let divider = DockArea::<PanelKind>::hit_id(area, &hit);
    let element = h.state().auto.find("dock.gutter.group.Info").expect("the group divider has no automation id");
    assert_eq!(element.widget_id, Some(format!("{:016x}", divider.value())));
    h.ctx.memory_mut(|memory| memory.request_focus(divider));
    frames(&mut h, 2);
    assert_eq!(h.ctx.memory(|memory| memory.focused()), Some(divider));
    press(&mut h, Key::Space);
    assert!(h.state().playback.playing, "Space must play with the group divider focused");
    press(&mut h, Key::Space);
    assert!(!h.state().playback.playing);
}

/// Tools keeps its factory width: dragging the divider beside it does not widen it.
#[test]
fn the_divider_beside_tools_cannot_widen_it() {
    let app = FilmcraftApp::new(Session::default());
    let mut h = Harness::builder().with_size(vec2(1400.0, 900.0)).build_eframe(move |_| app);
    frames(&mut h, 4);
    h.state_mut().ui.dock = craft_ui::docking::Layout {
        root: Some(craft_ui::docking::Node::Split {
            axis: craft_ui::layout::SplitAxis::Horizontal,
            size: craft_ui::layout::SplitSize::FixedFirst(filmcraft_ui_egui::dock::TOOLS_WIDTH),
            first: Box::new(craft_ui::docking::Node::Tabs { panels: vec![PanelKind::Tools], active: 0 }),
            second: Box::new(craft_ui::docking::Node::Tabs { panels: vec![PanelKind::Timeline], active: 0 }),
        }),
        floating: vec![],
    };
    frames(&mut h, 3);
    let element = h.state().auto.find("dock.gutter.").unwrap();
    let from = Rect::from_min_size(pos2(element.rect[0], element.rect[1]), vec2(element.rect[2], element.rect[3])).center();
    h.event(Event::PointerMoved(from));
    step(&mut h);
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
    step(&mut h);
    for offset in [10.0, 20.0, 30.0, 40.0] {
        h.event(Event::PointerMoved(from + vec2(offset, 0.0)));
        step(&mut h);
    }
    h.event(Event::PointerButton { pos: from + vec2(40.0, 0.0), button: PointerButton::Primary, pressed: false, modifiers: Modifiers::NONE });
    frames(&mut h, 3);
    assert!(first_size(&h) <= filmcraft_ui_egui::dock::TOOLS_WIDTH, "Tools widened to {}", first_size(&h));
    let tools = h.state().auto.find("panel.Tools").unwrap();
    assert!(tools.rect[2] <= filmcraft_ui_egui::dock::TOOLS_WIDTH, "Tools drawn {} wide", tools.rect[2]);
}

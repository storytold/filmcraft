//! Shared docking preserves FilmCraft document state and legacy workspace files.

use crate::support::demo_session;
use craft_behavior::dock::{Action, DockPlacement, DockSide, Node, SplitSize, Target};
use craft_behavior::flow::Axis;
use egui::{Event, Modifiers, PointerButton, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use filmcraft_engine::Session;
use filmcraft_ui_egui::{
    FilmcraftApp,
    dock::{self, Layout, LayoutExt, PanelKind},
    menus,
};
use serde_json::json;

fn tabs(panels: &[PanelKind], active: usize) -> Node<PanelKind> {
    Node::tabs(panels.to_vec(), active)
}

fn split(vertical: bool, size: SplitSize, a: Node<PanelKind>, b: Node<PanelKind>) -> Node<PanelKind> {
    Node::split(if vertical { Axis::Y } else { Axis::X }, size, a, b)
}

fn root(node: Node<PanelKind>) -> Layout {
    Layout::new(Some(node), Vec::new())
}

/// A context with the framework runtime installed, for commands run outside a harness.
fn context() -> egui::Context {
    let ctx = egui::Context::default();
    filmcraft_ui_egui::framework::install_headless(&ctx);
    ctx
}

/// The app in a harness of `size`, with the framework runtime installed.
fn harness(size: egui::Vec2, app: FilmcraftApp) -> Harness<'static, FilmcraftApp> {
    Harness::builder().with_size(size).build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app
    })
}

/// Whether a panel is in the docked frame tree (not a floating window).
trait Docked {
    fn docked(&self, panel: PanelKind) -> bool;
}

impl Docked for Layout {
    fn docked(&self, panel: PanelKind) -> bool {
        self.is_docked(panel)
    }
}

fn layout() -> Layout {
    root(split(
        false,
        SplitSize::Ratio(0.3),
        tabs(&[PanelKind::Project, PanelKind::Info, PanelKind::History], 1),
        tabs(&[PanelKind::Source, PanelKind::Program], 1),
    ))
}

#[test]
fn active_tab_underline_follows_selection_even_when_another_tab_is_hovered() {
    let mut session = Session::default();
    session.prefs.appearance.accessible_contrast = true;
    let mut app = FilmcraftApp::new(session);
    app.ui.dock = layout();
    let mut h = harness(vec2(1200.0, 760.0), app);
    for _ in 0..4 {
        step(&mut h);
    }
    // The dock's tab strip marks the selected tab itself (not by colour alone) and reports it as
    // the tab's selected state; FilmCraft no longer draws the tab labels.
    let underlined = |h: &Harness<'_, FilmcraftApp>, panel: PanelKind| {
        h.query_all_by_label(panel.title())
            .find(|node| node.accesskit_node().role() == egui::accesskit::Role::Tab)
            .map(|node| node.accesskit_node().is_selected() == Some(true))
    };
    assert_eq!(underlined(&h, PanelKind::Info), Some(true));
    assert_eq!(underlined(&h, PanelKind::History), Some(false));
    click(&mut h, "panel.tab.History");
    let inactive = rect(&h, "panel.tab.Info").center();
    h.event(Event::PointerMoved(inactive));
    step(&mut h);
    assert_eq!(underlined(&h, PanelKind::Info), Some(false));
    assert_eq!(underlined(&h, PanelKind::History), Some(true));
}

#[test]
fn shared_transactions_keep_selection_and_reject_invalid_moves_atomically() {
    let mut layout = layout();
    layout.apply(Action::close(PanelKind::Project)).unwrap();
    assert!(layout.is_visible(PanelKind::Info));
    let before = layout.clone();
    for action in [
        Action::move_beside(PanelKind::Info, PanelKind::Tools, DockPlacement::Split(DockSide::Start)),
        Action::float(PanelKind::Info, dock::rect_of([0.0, 0.0, f32::NAN, 100.0])),
        Action::move_beside(PanelKind::Info, PanelKind::Program, DockPlacement::tab(Some(PanelKind::Tools))),
    ] {
        assert!(layout.apply(action).is_err());
        assert_eq!(layout, before);
    }
    layout.apply(Action::float(PanelKind::Info, dock::rect_of([50.0, 50.0, 400.0, 300.0]))).unwrap();
    layout.apply(Action::move_beside(PanelKind::Info, PanelKind::Program, DockPlacement::Split(DockSide::Bottom))).unwrap();
    assert!(layout.floating.is_empty());
    assert!(layout.is_visible(PanelKind::Info));
}

#[test]
fn builtin_and_legacy_workspace_shapes_roundtrip_and_save_floating() {
    for name in dock::WORKSPACES {
        let layout = dock::workspace(name);
        for entry in
            [json!({"name": name, "workspace": dock::Workspace::new(layout.clone())}), json!({"name": name, "layout": legacy(layout.root.as_ref().unwrap())})]
        {
            let prefs: dock::WorkspacePrefs = serde_json::from_value(json!({"saved": [entry], "current": name})).unwrap();
            let loaded = dock::saved_docking(&prefs, name);
            loaded.validate().unwrap();
            assert_eq!(loaded, layout);
            assert!(serde_json::to_value(&prefs).unwrap()["saved"][0].get("floating").is_none());
        }
    }
    let dir = std::env::temp_dir().join(format!("filmcraft-shared-dock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut session = Session::default();
    session.prefs_path = Some(dir.join("preferences.json"));
    let mut app = FilmcraftApp::new(session);
    let ctx = context();
    app.ui.dock = layout();
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [45, 65, 420, 320]})).unwrap();
    menus::invoke(&mut app, &ctx, "window.workspace.saveAs", json!({"name": "Floating test"})).unwrap();
    let prefs = dock::WorkspacePrefs::load(&dir.join(dock::WORKSPACES_FILE));
    let restored = dock::saved_docking(&prefs, "Floating test");
    assert_eq!(restored, app.ui.dock);
    // Earlier versions read `layout` and `floating`.
    assert!(!prefs.saved[0].layout.is_null() && !prefs.saved[0].workspace.is_null());
    assert_eq!(prefs.saved[0].floating[0]["panels"], json!(["Info"]));
    let mut restarted_session = Session::default();
    restarted_session.prefs_path = Some(dir.join("preferences.json"));
    let restarted = FilmcraftApp::new(restarted_session);
    let mut h = harness(vec2(1200.0, 760.0), restarted);
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(h.state().ui.dock, restored);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legacy_effects_workspace_drops_only_the_known_inactive_duplicate() {
    let expected = dock::workspace("Effects");
    let mut legacy_tree = expected.root.clone().unwrap();
    fn duplicate(node: &mut Node<PanelKind>) {
        match node {
            Node::Tabs { panels, active, .. } if panels.as_slice() == [PanelKind::EssentialGraphics] => {
                panels.insert(0, PanelKind::EffectControls);
                *active = 1;
            }
            Node::Split { first, second, .. } => {
                duplicate(first);
                duplicate(second);
            }
            _ => {}
        }
    }
    duplicate(&mut legacy_tree);
    let prefs: dock::WorkspacePrefs =
        serde_json::from_value(json!({"saved": [{"name": "Historical Effects", "layout": legacy(&legacy_tree)}], "current": "Historical Effects"})).unwrap();
    assert_eq!(dock::saved_docking(&prefs, "Historical Effects"), expected);
}

/// The frame tree as FilmCraft wrote it before its workspaces were craft-ui workspaces.
fn legacy(node: &Node<PanelKind>) -> serde_json::Value {
    match node {
        Node::Tabs { panels, active, .. } => json!({"Tabs": {"panels": panels, "active": active}}),
        Node::Split { axis, size, first, second, .. } => json!({"Split": {
            "vertical": *axis == Axis::Y,
            "size": match size {
                SplitSize::Ratio(v) => json!({"Ratio": v}),
                SplitSize::FixedFirst(v) => json!({"FixedA": v}),
                SplitSize::FixedSecond(v) => json!({"FixedB": v}),
                _ => unreachable!(),
            },
            "a": legacy(first),
            "b": legacy(second),
        }}),
        _ => unreachable!(),
    }
}

fn step(h: &mut Harness<'_, FilmcraftApp>) {
    let ctx = h.ctx.clone();
    let mut input = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut input);
    *h.input_mut() = input;
    h.step();
}
fn rect(h: &Harness<'_, FilmcraftApp>, id: &str) -> Rect {
    let element = h.state().auto.find(id).unwrap_or_else(|| panic!("missing {id}"));
    Rect::from_min_size(pos2(element.rect[0], element.rect[1]), vec2(element.rect[2], element.rect[3]))
}
fn click(h: &mut Harness<'_, FilmcraftApp>, id: &str) {
    let pos = rect(h, id).center();
    h.event(Event::PointerMoved(pos));
    step(h);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        step(h);
    }
    for _ in 0..3 {
        step(h);
    }
}
fn drag(h: &mut Harness<'_, FilmcraftApp>, from: egui::Pos2, to: egui::Pos2, modifiers: Modifiers) {
    h.event(Event::ModifiersChanged(modifiers));
    h.event(Event::PointerMoved(from));
    step(h);
    h.event(Event::PointerButton { pos: from, button: PointerButton::Primary, pressed: true, modifiers });
    step(h);
    for fraction in [0.05, 0.25, 0.5, 0.75, 1.0] {
        h.event(Event::PointerMoved(from.lerp(to, fraction)));
        step(h);
    }
    h.event(Event::PointerButton { pos: to, button: PointerButton::Primary, pressed: false, modifiers });
    step(h);
    h.event(Event::ModifiersChanged(Modifiers::NONE));
    for _ in 0..3 {
        step(h);
    }
}

fn maximize_frame_round_trip(h: &mut Harness<'_, FilmcraftApp>, action_log: &mut Vec<serde_json::Value>) {
    let saved_root = h.state().ui.dock.clone();
    let saved_floating = h.state().ui.dock.floating.clone();
    let ctx = h.ctx.clone();
    // Maximize Frame fills the area with a docked frame
    h.state_mut().ui.focused = PanelKind::Program;
    menus::invoke(h.state_mut(), &ctx, "window.maximizeFrame", json!({})).unwrap();
    for _ in 0..3 {
        step(h);
    }
    assert!(rect(h, "panel.Program").width() > 1000.0);
    action_log.push(action_state(h, "maximized"));
    assert_eq!(h.state().ui.dock, saved_root);
    assert_eq!(h.state().ui.dock.floating, saved_floating);
    menus::invoke(h.state_mut(), &ctx, "window.maximizeFrame", json!({})).unwrap();
    for _ in 0..3 {
        step(h);
    }
    assert_eq!(h.state().ui.dock.floating, saved_floating);
}

#[test]
fn real_panel_drag_merges_splits_floats_and_redocks_without_editing_documents() {
    let session = demo_session();
    let app = FilmcraftApp::new(session);
    let snapshots = std::env::var_os("CRAFT_UI_DOCKING_SNAPSHOT_DIR").map(std::path::PathBuf::from);
    let mut builder = Harness::builder().with_size(vec2(1200.0, 760.0));
    if snapshots.is_some() {
        builder = builder.wgpu();
    }
    let mut h = builder.build_eframe(move |cc| {
        filmcraft_ui_egui::framework::install_headless(&cc.egui_ctx);
        app
    });
    for _ in 0..4 {
        step(&mut h);
    }
    h.state_mut().ui.dock = root(split(true, SplitSize::Ratio(0.62), layout().root.unwrap(), tabs(&[PanelKind::Timeline], 0)));
    for _ in 0..3 {
        step(&mut h);
    }
    let project = h.state().session.project.clone();
    let mut action_log = vec![action_state(&h, "start")];
    let playheads = h.state().session.state.playheads.clone();
    let from = rect(&h, "panel.tab.Info").center();
    let to = rect(&h, "panel.Program").center();
    drag(&mut h, from, to, Modifiers::NONE);
    assert!(h.state().ui.dock.is_visible(PanelKind::Info));
    assert!(same_group(&h.state().ui.dock, PanelKind::Info, PanelKind::Program));
    action_log.push(action_state(&h, "merged"));
    let from = rect(&h, "panel.tab.Info").center();
    let before = rect(&h, "panel.tab.Source");
    drag(&mut h, from, pos2(before.left() + 2.0, before.center().y), Modifiers::NONE);
    assert_eq!(tabs_containing(&h.state().ui.dock, PanelKind::Info).first(), Some(&PanelKind::Info));
    action_log.push(action_state(&h, "reordered"));

    let from = rect(&h, "panel.tab.Info").center();
    let left = rect(&h, "panel.History");
    // Activate a visible sibling after the source left its original group.
    let to = pos2(left.left() + 8.0, left.center().y);
    drag(&mut h, from, to, Modifiers::NONE);
    assert!(h.state().ui.dock.is_visible(PanelKind::Info));
    assert!(!same_group(&h.state().ui.dock, PanelKind::Info, PanelKind::History));
    assert!(!same_group(&h.state().ui.dock, PanelKind::Info, PanelKind::Program));
    action_log.push(action_state(&h, "split"));
    click(&mut h, "panel.tab.Program");
    assert!(h.state().auto.elements.iter().any(|element| element.id.starts_with("timeline.clip.")));
    assert!(h.state().auto.find("program.transport.playhead.stepForward").is_some());
    let from = rect(&h, "panel.tab.Info").center();
    drag(&mut h, from, pos2(700.0, 400.0), Modifiers::COMMAND);
    assert_eq!(h.state().ui.dock.floating.len(), 1);
    assert_eq!(h.query_all_by_label("Resize panel window").next().unwrap().accesskit_node().role(), egui::accesskit::Role::Splitter);
    action_log.push(action_state(&h, "floated"));
    let original = h.state().ui.dock.floating[0].rect;
    let grip = rect(&h, "panel.float.0.resize").center();
    drag(&mut h, grip, grip + vec2(36.0, 24.0), Modifiers::NONE);
    assert!(h.state().ui.dock.floating[0].rect.width() > original.width() + 25.0);
    assert!(h.state().ui.dock.floating[0].rect.height() > original.height() + 15.0);
    action_log.push(action_state(&h, "resized"));
    let resized = h.state().ui.dock.floating[0].rect;
    let header = rect(&h, "panel.float.0.move");
    // next's title row ends in its Close action; the band's leading part is free to move by.
    let from = pos2(header.left() + 24.0, header.center().y);
    drag(&mut h, from, from - vec2(24.0, 18.0), Modifiers::NONE);
    assert!(h.state().ui.dock.floating[0].rect.min.x < resized.min.x - 15.0);
    assert!(h.state().ui.dock.floating[0].rect.min.y < resized.min.y - 10.0);
    action_log.push(action_state(&h, "moved"));
    maximize_frame_round_trip(&mut h, &mut action_log);

    if let Some(directory) = &snapshots {
        std::fs::create_dir_all(directory).unwrap();
        h.render().unwrap().save(directory.join("filmcraft-floating.png")).unwrap();
    }
    let from = rect(&h, "panel.tab.Info").center();
    let to = rect(&h, "panel.History").center();
    drag(&mut h, from, to, Modifiers::NONE);
    assert!(h.state().ui.dock.floating.is_empty());
    action_log.push(action_state(&h, "redocked"));
    assert!(std::sync::Arc::ptr_eq(&project, &h.state().session.project));
    assert_eq!(playheads, h.state().session.state.playheads);
    let playhead = h.state().session.playhead();
    click(&mut h, "program.transport.playhead.stepForward");
    assert!(h.state().session.playhead() > playhead);
    assert!(h.state().auto.elements.iter().any(|element| element.id.starts_with("timeline.clip.")));
    action_log.push(action_state(&h, "program-step-forward"));
    if let Some(directory) = &snapshots {
        std::fs::write(directory.join("filmcraft-actions.json"), serde_json::to_vec_pretty(&action_log).unwrap()).unwrap();
    }
    if let Some(directory) = &snapshots {
        h.render().unwrap().save(directory.join("filmcraft-redocked.png")).unwrap();
    }
    assert!(std::sync::Arc::ptr_eq(&project, &h.state().session.project));
    assert!(h.state().ui_error.is_none());
}

#[test]
fn dropping_on_the_window_edge_docks_beside_the_whole_workspace() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = layout();
    let mut h = harness(vec2(1200.0, 760.0), app);
    for _ in 0..4 {
        step(&mut h);
    }
    let from = rect(&h, "panel.tab.Info").center();
    let leftmost = rect(&h, "panel.Info");
    // The outer edge of the leftmost group: without the window-edge zone this splits only that group.
    // The dock insets a panel's body from its group's frame by the panel padding, so the point is
    // halfway between the window's edge and the body, inside the frame.
    let to = pos2(leftmost.left() / 2.0, leftmost.center().y);
    drag(&mut h, from, to, Modifiers::NONE);
    assert_eq!(
        h.state().ui.dock,
        root(split(
            false,
            SplitSize::Ratio(0.25),
            tabs(&[PanelKind::Info], 0),
            split(false, SplitSize::Ratio(0.3), tabs(&[PanelKind::Project, PanelKind::History], 1), tabs(&[PanelKind::Source, PanelKind::Program], 1)),
        ))
    );
}

/// A release over no drop target (the menu bar, the gap between two groups) cancels the drag:
/// nothing floats and nothing moves. Only Ctrl/Cmd or leaving the window floats.
#[test]
fn a_release_over_no_target_cancels_the_drag() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = layout();
    let mut h = harness(vec2(1200.0, 760.0), app);
    for _ in 0..4 {
        step(&mut h);
    }
    let before = h.state().ui.dock.clone();
    let info = rect(&h, "panel.Info");
    // The gap between the two groups is the divider between them: the dock insets a panel's body
    // from its group's frame by the panel padding, so a point just right of the body is still in
    // the group.
    let gap = rect(&h, "dock.gutter.").center();
    for to in [pos2(600.0, 10.0), pos2(gap.x, info.center().y)] {
        let from = rect(&h, "panel.tab.Info").center();
        drag(&mut h, from, to, Modifiers::NONE);
        assert_eq!(h.state().ui.dock, before, "release at {to:?}");
    }
}

#[test]
fn dock_panel_without_anchor_docks_along_a_workspace_edge() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = layout();
    let ctx = context();
    menus::invoke(&mut app, &ctx, "window.dockPanel", json!({"panel": "Program", "zone": "bottom"})).unwrap();
    let Some(Node::Split { axis: Axis::Y, size: SplitSize::Ratio(ratio), first, second, .. }) = &app.ui.dock.root else {
        panic!("root not split: {:?}", app.ui.dock)
    };
    assert_eq!(*ratio, 0.75);
    assert_eq!(**second, tabs(&[PanelKind::Program], 0));
    let a = root((**first).clone());
    assert!(!a.contains(&PanelKind::Program) && a.contains(&PanelKind::Source) && a.contains(&PanelKind::Info));
    // without an anchor or a side, a docked panel stays where it is
    let before = app.ui.dock.clone();
    let _ = menus::invoke(&mut app, &ctx, "window.dockPanel", json!({"panel": "Info", "zone": "center"}));
    assert_eq!(app.ui.dock, before);
    app.ui.dock = root(tabs(&[PanelKind::Project], 0));
    assert!(menus::invoke(&mut app, &ctx, "window.dockPanel", json!({"panel": "Project", "zone": "left"})).is_err());
    assert_eq!(app.ui.dock, root(tabs(&[PanelKind::Project], 0)));
}

fn same_group(layout: &Layout, first: PanelKind, second: PanelKind) -> bool {
    tabs_containing(layout, first).contains(&second)
}

fn tabs_containing(layout: &Layout, panel: PanelKind) -> Vec<PanelKind> {
    fn find(node: &Node<PanelKind>, panel: PanelKind) -> Vec<PanelKind> {
        match node {
            Node::Tabs { panels, .. } if panels.contains(&panel) => panels.clone(),
            Node::Split { first, second, .. } => {
                let found = find(first, panel);
                if found.is_empty() { find(second, panel) } else { found }
            }
            _ => vec![],
        }
    }
    layout.root.as_ref().map(|node| find(node, panel)).unwrap_or_default()
}

#[test]
fn docking_commands_reject_bad_geometry_and_targets_without_partial_changes() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = layout();
    let ctx = context();
    let before = app.ui.dock.clone();
    for (id, params) in [
        ("window.floatPanel", json!({"panel": "Info", "rect": [1, 2, 300]})),
        ("window.floatPanel", json!({"panel": "Info", "rect": [1, 2, 300, 200, 7]})),
        ("window.floatPanel", json!({"panel": "Info", "rect": [1, 2, 1e100, 200]})),
        ("window.dockPanel", json!({"panel": "Info", "anchor": "Tools"})),
        ("window.dockPanel", json!({"panel": "Info", "anchor": "Program", "zone": 42})),
        ("window.dockPanel", json!({"panel": "Info", "anchor": "Program", "before": "Tools"})),
    ] {
        assert!(menus::invoke(&mut app, &ctx, id, params.clone()).is_err(), "{id} accepted {params}");
        assert_eq!(app.ui.dock, before);
        assert!(app.ui.dock.floating.is_empty());
    }
    app.ui.dock = root(tabs(&[PanelKind::Project], 0));
    assert!(menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Project", "rect": [1, 2, 300, 200]})).is_err());
}

fn action_state(h: &Harness<'_, FilmcraftApp>, action: &str) -> serde_json::Value {
    json!({"action": action, "dock": h.state().ui.dock, "floating": h.state().ui.dock.floating, "focused": h.state().ui.focused})
}

#[test]
fn catalog_docking_action_supports_resize_reorder_and_floating_geometry() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = root(split(false, SplitSize::FixedFirst(300.0), tabs(&[PanelKind::Project, PanelKind::Info], 1), tabs(&[PanelKind::Program], 0)));
    let ctx = context();
    for action in [
        Action::resize_split(vec![], SplitSize::FixedFirst(360.0)),
        Action::move_beside(PanelKind::Info, PanelKind::Project, DockPlacement::tab(Some(PanelKind::Project))),
        Action::float(PanelKind::Info, dock::rect_of([20.0, 40.0, 300.0, 200.0])),
        Action::move_floating(PanelKind::Info, dock::rect_of([45.0, 65.0, 330.0, 240.0])),
    ] {
        filmcraft_ui_egui::menus::invoke(&mut app, &ctx, "window.dockAction", json!({"action": action})).unwrap();
    }
    assert_eq!(app.ui.dock.floating[0].rect, dock::rect_of([45.0, 65.0, 330.0, 240.0]));
    let Some(Node::Split { size, .. }) = &app.ui.dock.root else { panic!("split lost") };
    assert_eq!(*size, SplitSize::FixedFirst(360.0));
    let before = json!({"dock": app.ui.dock, "floating": app.ui.dock.floating});
    assert!(
        filmcraft_ui_egui::menus::invoke(
            &mut app,
            &ctx,
            "window.dockAction",
            json!({"action": {
                "ResizeSplit": {"path": [true, true], "size": {"Ratio": 0.5}}
            }})
        )
        .is_err()
    );
    assert_eq!(json!({"dock": app.ui.dock, "floating": app.ui.dock.floating}), before);
}

#[test]
fn unreadable_floating_workspace_does_not_erase_valid_workspaces_or_current_name() {
    let tree = json!({"Tabs": {"panels": ["Project"], "active": 0}});
    let valid = json!({"name": "Valid", "layout": tree, "floating": [{
        "panels": ["TextToSpeech", "Timeline"], "active": 0, "rect": [80, 90, 420, 320]
    }]});
    for (index, bad) in [
        json!([{"panels": ["FuturePanel"], "active": 0, "rect": [80, 90, 420, 320]}]),
        json!([{"panels": ["Info"], "active": "invalid", "rect": [80, 90, 420, 320]}]),
        json!({"malformed": true}),
    ]
    .into_iter()
    .enumerate()
    {
        let path = std::env::temp_dir().join(format!("filmcraft-floating-isolation-{}-{index}.json", std::process::id()));
        let prefs = json!({"saved": [valid.clone(), {"name": "Broken", "layout": tree, "floating": bad}], "current": "Valid"});
        std::fs::write(&path, serde_json::to_vec(&prefs).unwrap()).unwrap();
        let loaded = dock::WorkspacePrefs::load(&path);
        assert_eq!(loaded.current, "Valid");
        assert_eq!(loaded.saved.len(), 2);
        let restored = dock::saved_docking(&loaded, "Valid");
        restored.validate().unwrap();
        assert_eq!(restored.root, Some(tabs(&[PanelKind::Project], 0)));
        assert_eq!(restored.floating[0].panels().copied().collect::<Vec<_>>(), vec![PanelKind::TextToSpeech, PanelKind::Timeline]);
        assert_eq!(dock::saved_docking(&loaded, "Broken"), dock::workspace("Broken"));
        loaded.save(&path).unwrap();
        assert_eq!(dock::WorkspacePrefs::load(&path), loaded, "saving preserves the unreadable entry for a future version");
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn floating_tts_and_timeline_reveal_keeps_one_owner_and_draft_without_project_edits() {
    let session = demo_session();
    let mut app = FilmcraftApp::new(session);
    app.ui.tts.text = "Synthetic retained narration".into();
    app.show_panel(PanelKind::TextToSpeech);
    let project = app.session.project.clone();
    let state = serde_json::to_value(&app.session.state).unwrap();
    let draft = serde_json::to_value(&app.ui.tts).unwrap();
    let ctx = context();
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "TextToSpeech", "rect": [80, 90, 420, 320]})).unwrap();
    menus::invoke(
        &mut app,
        &ctx,
        "window.dockAction",
        json!({"action": Action::move_beside(PanelKind::Timeline, PanelKind::TextToSpeech, DockPlacement::tab(None))}),
    )
    .unwrap();
    app.show_panel(PanelKind::TextToSpeech);
    app.show_panel(PanelKind::Timeline);
    assert!(!app.ui.dock.docked(PanelKind::TextToSpeech));
    assert!(!app.ui.dock.docked(PanelKind::Timeline));
    assert_eq!(app.ui.dock.floating.iter().flat_map(|group| group.panels()).filter(|panel| **panel == PanelKind::TextToSpeech).count(), 1);
    assert_eq!(app.ui.dock.floating.iter().flat_map(|group| group.panels()).filter(|panel| **panel == PanelKind::Timeline).count(), 1);
    assert_eq!(serde_json::to_value(&app.ui.tts).unwrap(), draft);
    assert!(std::sync::Arc::ptr_eq(&project, &app.session.project));
    assert_eq!(serde_json::to_value(&app.session.state).unwrap(), state);
}

#[test]
fn narration_selection_reveals_covered_floating_tts_once_without_editing_the_project() {
    let mut session = demo_session();
    let clip = session
        .active_sequence()
        .unwrap()
        .audio_tracks
        .iter()
        .flat_map(|track| &track.items)
        .find(|clip| {
            session.project.item(clip.item).and_then(|item| item.as_media()).is_some_and(|media| media.info.kind == filmcraft_media::MediaKind::AudioOnly)
        })
        .unwrap()
        .clone();
    let narration = filmcraft_project::Narration {
        text: "Synthetic narration behind an overlapping pane".into(),
        language: "en-US".into(),
        voice: filmcraft_tts::default_voice_id().into(),
        pitch: filmcraft_project::VocalPitch::Default,
        pace: 1.2,
        speech_duration: filmcraft_time::Tick::ZERO,
    };
    std::sync::Arc::make_mut(&mut session.project).narrations.insert(clip.item, narration.clone());
    let project = session.project.clone();
    let mut app = FilmcraftApp::new(session);
    app.show_panel(PanelKind::TextToSpeech);
    let mut h = harness(vec2(1200.0, 760.0), app);
    for _ in 0..4 {
        step(&mut h);
    }
    let ctx = h.ctx.clone();
    // Info covers the middle of the TTS window, and neither window holds the other.
    for (panel, rect) in [("TextToSpeech", [400, 220, 450, 360]), ("Info", [480, 260, 450, 360])] {
        menus::invoke(h.state_mut(), &ctx, "window.floatPanel", json!({"panel": panel, "rect": rect})).unwrap();
        for _ in 0..4 {
            step(&mut h);
        }
    }
    let covered = rect(&h, "panel.TextToSpeech").center();
    assert!(rect(&h, "panel.Info").contains(covered));
    ctx.move_to_top(window_layer(&h, PanelKind::Info));
    for _ in 0..3 {
        step(&mut h);
    }
    assert_eq!(ctx.layer_id_at(covered), Some(window_layer(&h, PanelKind::Info)), "precondition: Info really covers TTS");
    h.state_mut().session.state.selection = vec![clip.id];
    for _ in 0..4 {
        step(&mut h);
    }
    let draft = h.state().ui.tts.clone();
    assert_eq!(draft.text, narration.text);
    assert_eq!(draft.pace, narration.pace);
    assert_eq!(draft.loaded_from, Some((clip.id.0, clip.item.0)));
    assert_eq!(ctx.layer_id_at(covered), Some(window_layer(&h, PanelKind::TextToSpeech)), "narration selection must bring its floating editor forward");
    assert_eq!(h.state().ui.focused, PanelKind::TextToSpeech);
    assert!(!h.state().ui.dock.docked(PanelKind::TextToSpeech));
    assert_eq!(h.state().ui.dock.floating.iter().flat_map(|group| group.panels()).filter(|panel| **panel == PanelKind::TextToSpeech).count(), 1);
    assert!(std::sync::Arc::ptr_eq(&project, &h.state().session.project));
    // A reveal is one-shot: drawing the selected narration must not steal stacking
    // from another pane that the user subsequently brings forward. next's overlays own the
    // window stacking and hand egui their order every pass, so the user brings Info forward
    // the way a user does: a press on a part of Info that Text to Speech does not cover.
    bring_info_forward(&mut h);
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(ctx.layer_id_at(covered), Some(window_layer(&h, PanelKind::Info)));
    assert_eq!(h.state().ui.tts, draft);
    menus::invoke(h.state_mut(), &ctx, "window.panel.TextToSpeech", json!({})).unwrap();
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(ctx.layer_id_at(covered), Some(window_layer(&h, PanelKind::TextToSpeech)));
    assert_eq!(h.state().ui.tts, draft);
    assert!(std::sync::Arc::ptr_eq(&project, &h.state().session.project));
    // Replacing the workspace cancels a queued reveal before the next render,
    // even when the destination happens to contain the same floating panel IDs.
    bring_info_forward(&mut h);
    for _ in 0..3 {
        step(&mut h);
    }
    let saved = dock::SavedWorkspace {
        name: "Reveal cancellation".into(),
        workspace: serde_json::to_value(dock::Workspace::new(h.state().ui.dock.clone())).unwrap(),
        ..Default::default()
    };
    h.state_mut().workspaces.saved.push(saved);
    h.state_mut().show_panel(PanelKind::TextToSpeech);
    h.state_mut().set_workspace("Reveal cancellation");
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(ctx.layer_id_at(covered), Some(window_layer(&h, PanelKind::Info)));
    assert_eq!(h.state().ui.tts, draft);
    assert!(std::sync::Arc::ptr_eq(&project, &h.state().session.project));
}

/// A primary press on the floating Info window, at a corner of its body that the Text to Speech
/// window does not cover: the user bringing Info to the front.
fn bring_info_forward(h: &mut Harness<'_, FilmcraftApp>) {
    let info = rect(h, "panel.Info");
    // Clear of the window's corner resize grip.
    let pos = info.right_bottom() - vec2(40.0, 16.0);
    assert!(!rect(h, "panel.TextToSpeech").contains(pos), "the press must land on Info alone");
    click_at(h, pos, PointerButton::Primary);
}

/// #1985: a panel closed while floating came back docked when opened again from the Window menu;
/// it now floats again where it was. Closed while docked, it opens docked as before.
#[test]
fn a_panel_closed_while_floating_reopens_floating_where_it_was() {
    let mut app = FilmcraftApp::new(Session::default());
    let ctx = context();
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [45, 65, 420, 320]})).unwrap();
    let rect = app.ui.dock.floating.iter().find(|g| g.contains(&PanelKind::Info)).unwrap().rect;
    menus::invoke(&mut app, &ctx, "window.closePanel", json!({"panel": "Info"})).unwrap();
    assert!(!app.ui.dock.docked(PanelKind::Info) && !app.ui.dock.floating.iter().any(|g| g.contains(&PanelKind::Info)));
    menus::invoke(&mut app, &ctx, &menus::panel_command_id(PanelKind::Info), json!({})).unwrap();
    let back = app.ui.dock.floating.iter().find(|g| g.contains(&PanelKind::Info)).expect("Info floats again");
    assert_eq!(back.rect, rect, "where it was");
    assert!(!app.ui.dock.docked(PanelKind::Info));
    // docked, then closed: it opens docked
    menus::invoke(&mut app, &ctx, "window.dockPanel", json!({"panel": "Info", "anchor": "Project"})).unwrap();
    menus::invoke(&mut app, &ctx, "window.closePanel", json!({"panel": "Info"})).unwrap();
    menus::invoke(&mut app, &ctx, &menus::panel_command_id(PanelKind::Info), json!({})).unwrap();
    assert!(app.ui.dock.docked(PanelKind::Info));
    assert!(!app.ui.dock.floating.iter().any(|g| g.contains(&PanelKind::Info)));
    // the Timeline too, though reopening it first puts it back in its own place in the dock
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Timeline", "rect": [60, 80, 700, 300]})).unwrap();
    let rect = app.ui.dock.floating.iter().find(|g| g.contains(&PanelKind::Timeline)).unwrap().rect;
    menus::invoke(&mut app, &ctx, "window.closePanel", json!({"panel": "Timeline"})).unwrap();
    menus::invoke(&mut app, &ctx, &menus::panel_command_id(PanelKind::Timeline), json!({})).unwrap();
    let back = app.ui.dock.floating.iter().find(|g| g.contains(&PanelKind::Timeline)).expect("the Timeline floats again");
    assert_eq!(back.rect, rect);
    assert!(!app.ui.dock.docked(PanelKind::Timeline));

    // with nothing else docked, a reopened panel has to stay docked (the workspace keeps one
    // docked panel); its floating place is kept, not lost (#2482 review)
    let mut app = FilmcraftApp::new(Session::default());
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [45, 65, 420, 320]})).unwrap();
    let rect = app.ui.dock.floating.iter().find(|g| g.contains(&PanelKind::Info)).unwrap().rect;
    for _ in 0..64 {
        let Some(p) = PanelKind::ALL.iter().copied().find(|p| app.ui.dock.docked(*p)) else { break };
        if menus::invoke(&mut app, &ctx, "window.closePanel", json!({"panel": p.id()})).is_err() {
            break;
        }
    }
    let docked = PanelKind::ALL.iter().filter(|p| app.ui.dock.docked(**p)).count();
    if docked > 0 {
        eprintln!("SKIPPED: the dock cannot be emptied ({docked} panels stay)");
        return;
    }
    menus::invoke(&mut app, &ctx, "window.closePanel", json!({"panel": "Info"})).unwrap();
    menus::invoke(&mut app, &ctx, &menus::panel_command_id(PanelKind::Info), json!({})).unwrap();
    assert!(app.ui.dock.docked(PanelKind::Info), "the only panel stays docked");
    assert!(app.ui.closed_floating.iter().any(|(p, r)| *p == PanelKind::Info && *r == dock::array_of(rect)), "{:?}", app.ui.closed_floating);
}

/// craft-ui's `window.panel.*` commands and FilmCraft's own docking commands share one path.
#[test]
fn shared_dock_commands_and_filmcraft_names_move_panels() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = layout();
    let ctx = context();
    menus::invoke(&mut app, &ctx, "window.panel.move", json!({"panel": "Info", "anchor": "Program", "zone": "right"})).unwrap();
    assert!(app.ui.dock.is_visible(PanelKind::Info) && !same_group(&app.ui.dock, PanelKind::Info, PanelKind::Program));
    menus::invoke(&mut app, &ctx, "window.dockPanel", json!({"panel": "Info", "anchor": "Program", "before": "Source"})).unwrap();
    assert_eq!(tabs_containing(&app.ui.dock, PanelKind::Info), vec![PanelKind::Info, PanelKind::Source, PanelKind::Program]);
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Info"})).unwrap();
    assert!(app.ui.dock.floating.iter().any(|window| window.contains(&PanelKind::Info)));
    menus::invoke(&mut app, &ctx, "window.panel.close", json!({"panel": "Info"})).unwrap();
    assert!(!app.ui.dock.contains(&PanelKind::Info));
    assert!(menus::invoke(&mut app, &ctx, "window.panel.move", json!({"panel": "Info", "anchor": "Nowhere"})).is_err());
}

/// The layer of the floating window holding `panel`: the smallest area above the background that
/// holds the panel's body.
fn window_layer(h: &Harness<'_, FilmcraftApp>, panel: PanelKind) -> egui::LayerId {
    let body = rect(h, &format!("panel.{}", panel.id()));
    h.ctx
        .memory(|memory| {
            memory
                .layer_ids()
                .filter(|layer| layer.order != egui::Order::Background)
                .filter_map(|layer| memory.area_rect(layer.id).map(|area| (layer, area)))
                .filter(|(_, area)| area.contains_rect(body))
                .min_by(|(_, a), (_, b)| a.area().total_cmp(&b.area()))
                .map(|(layer, _)| layer)
        })
        .unwrap_or_else(|| panic!("{panel:?} is in no floating window"))
}

fn demo_harness() -> Harness<'static, FilmcraftApp> {
    let session = demo_session();
    let app = FilmcraftApp::new(session);
    let mut h = harness(vec2(1400.0, 900.0), app);
    for _ in 0..4 {
        step(&mut h);
    }
    h
}

/// Clicking the Timeline's active sequence tab, in its group header, focuses the Timeline.
#[test]
fn clicking_the_active_sequence_tab_focuses_the_timeline() {
    let mut h = demo_harness();
    click(&mut h, "panel.tab.Project");
    assert_eq!(h.state().ui.focused, PanelKind::Project);
    let active = h.state().session.state.active_sequence.unwrap().0;
    click(&mut h, &format!("timeline.tab.{active}"));
    assert_eq!(h.state().ui.focused, PanelKind::Timeline);
}

/// Dock `panel` as a tab of `anchor`'s group, in front.
fn tab_into(h: &mut Harness<'_, FilmcraftApp>, panel: PanelKind, anchor: PanelKind) {
    let ctx = h.ctx.clone();
    let action = Action::move_beside(panel, anchor, DockPlacement::tab(None));
    menus::invoke(h.state_mut(), &ctx, "window.dockAction", json!({ "action": action })).unwrap();
    menus::invoke(h.state_mut(), &ctx, "window.panel.activate", json!({ "panel": panel.id() })).unwrap();
    for _ in 0..3 {
        step(h);
    }
    assert!(h.state().ui.dock.is_visible(panel) && same_group(&h.state().ui.dock, panel, anchor));
}

/// #3653: with another tab in front of the Program monitor, clicking the Timeline (its body or a
/// sequence tab) brings Program to the front, as in Premiere; the focus stays on the Timeline.
#[test]
fn clicking_the_timeline_brings_the_program_monitor_to_the_front() {
    let mut h = demo_harness();
    tab_into(&mut h, PanelKind::Properties, PanelKind::Program);
    assert!(!h.state().ui.dock.is_visible(PanelKind::Program));
    click(&mut h, "panel.tab.Project");
    click(&mut h, "panel.Timeline");
    let dock = &h.state().ui.dock;
    assert!(dock.is_visible(PanelKind::Program), "Program stayed behind Properties");
    assert!(same_group(dock, PanelKind::Program, PanelKind::Properties), "the group changed");
    assert_eq!(h.state().ui.focused, PanelKind::Timeline);

    click(&mut h, "panel.tab.Properties");
    assert!(!h.state().ui.dock.is_visible(PanelKind::Program));
    let active = h.state().session.state.active_sequence.unwrap().0;
    click(&mut h, &format!("timeline.tab.{active}"));
    assert!(h.state().ui.dock.is_visible(PanelKind::Program), "a sequence tab click left Program behind");
    assert_eq!(h.state().ui.focused, PanelKind::Timeline);
}

/// #3653: Program sharing the Timeline's own group stays behind it, so the Timeline is not hidden
/// by a click on it.
#[test]
fn program_in_the_timeline_group_stays_behind_it() {
    let mut h = demo_harness();
    tab_into(&mut h, PanelKind::Program, PanelKind::Timeline);
    click(&mut h, "panel.tab.Timeline");
    click(&mut h, "panel.Timeline");
    assert!(h.state().ui.dock.is_visible(PanelKind::Timeline), "the Timeline was hidden");
    assert!(!h.state().ui.dock.is_visible(PanelKind::Program));
    assert_eq!(h.state().ui.focused, PanelKind::Timeline);
}

/// #3653: a closed Program monitor stays closed when the Timeline is clicked, and a floating one is
/// left as it is.
#[test]
fn clicking_the_timeline_leaves_a_closed_or_floating_program_monitor_alone() {
    let mut h = demo_harness();
    let ctx = h.ctx.clone();
    menus::invoke(h.state_mut(), &ctx, "window.panel.close", json!({"panel": "Program"})).unwrap();
    click(&mut h, "panel.Timeline");
    assert!(!h.state().ui.dock.contains(&PanelKind::Program), "a click reopened Program");

    menus::invoke(h.state_mut(), &ctx, "window.panel.activate", json!({"panel": "Program"})).unwrap();
    menus::invoke(h.state_mut(), &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [45, 65, 420, 320]})).unwrap();
    let action = Action::move_beside(PanelKind::Program, PanelKind::Info, DockPlacement::tab(None));
    menus::invoke(h.state_mut(), &ctx, "window.dockAction", json!({ "action": action })).unwrap();
    menus::invoke(h.state_mut(), &ctx, "window.panel.activate", json!({"panel": "Info"})).unwrap();
    for _ in 0..3 {
        step(&mut h);
    }
    assert!(!h.state().ui.dock.docked(PanelKind::Program) && !h.state().ui.dock.is_visible(PanelKind::Program));
    click(&mut h, "panel.Timeline");
    assert!(h.state().ui.dock.is_visible(PanelKind::Info), "the floating window changed tab");
    assert_eq!(h.state().ui.focused, PanelKind::Timeline);
}

/// No move or float may empty the frame tree, whichever unit it moves.
#[test]
fn the_last_docked_group_cannot_float_away() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = root(tabs(&[PanelKind::Project, PanelKind::Info], 0));
    let ctx = context();
    let before = app.ui.dock.clone();
    let float = Target::float(dock::rect_of([20.0, 20.0, 300.0, 200.0]));
    for action in [Action::move_group(PanelKind::Project, float.clone()), Action::move_column(PanelKind::Project, float.clone())] {
        let _ = menus::invoke(&mut app, &ctx, "window.dockAction", json!({ "action": action }));
        assert_eq!(app.ui.dock, before);
    }
    assert!(menus::invoke(&mut app, &ctx, "window.panel.float", json!({"panel": "Project", "group": true})).is_err());
    assert_eq!(app.ui.dock, before);
    // one panel of the group may float; the other stays docked
    menus::invoke(&mut app, &ctx, "window.panel.float", json!({"panel": "Info"})).unwrap();
    assert!(app.ui.dock.docked(PanelKind::Project) && !app.ui.dock.docked(PanelKind::Info));
}

/// Tools and the audio meters are ordinary groups with a tab strip: the meters show their tab,
/// narrow Tools at least its drag bar, and either one joins another group.
#[test]
fn tools_and_meters_move_like_any_group() {
    let mut h = demo_harness();
    assert!(h.state().auto.find("panel.tab.AudioMeters").is_some(), "the meters have no tab");
    let from = rect(&h, "panel.drag.Tools").center();
    let to = rect(&h, "panel.Program").center();
    drag(&mut h, from, to, Modifiers::NONE);
    assert!(same_group(&h.state().ui.dock, PanelKind::Tools, PanelKind::Program), "{:?}", h.state().ui.dock);
}

/// A panel floated by command without a rect takes a useful height, not the 120 pt minimum.
#[test]
fn program_floats_at_a_useful_height() {
    let mut h = demo_harness();
    let ctx = h.ctx.clone();
    menus::invoke(h.state_mut(), &ctx, "window.panel.float", json!({"panel": "Program"})).unwrap();
    for _ in 0..3 {
        step(&mut h);
    }
    let window = h.state().ui.dock.floating.iter().find(|window| window.contains(&PanelKind::Program)).unwrap();
    // Content that fills its frame floats at the dock's default float height, the most a command gives.
    let sizes = craft_ui::docking::DockSizes::of(&ctx);
    assert!(window.rect.height() >= sizes.float_size.y && window.rect.height() > sizes.least, "Program floated at {:?}", window.rect);
}

/// Only docked frames maximize: Maximize Frame on a floating panel leaves nothing maximized.
#[test]
fn a_floating_panel_does_not_maximize() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = layout();
    let ctx = context();
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [45, 65, 420, 320]})).unwrap();
    app.ui.focused = PanelKind::Info;
    menus::invoke(&mut app, &ctx, "window.maximizeFrame", json!({})).unwrap();
    assert_eq!(app.ui.keys.maximized, None);
    app.ui.focused = PanelKind::Program;
    menus::invoke(&mut app, &ctx, "window.maximizeFrame", json!({})).unwrap();
    assert_eq!(app.ui.keys.maximized, Some(PanelKind::Program));
}

/// A closed panel reopens through `window.panel.activate`.
#[test]
fn activate_reopens_a_closed_panel() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = layout();
    let ctx = context();
    menus::invoke(&mut app, &ctx, "window.panel.close", json!({"panel": "Info"})).unwrap();
    assert!(!app.ui.dock.contains(&PanelKind::Info));
    menus::invoke(&mut app, &ctx, "window.panel.activate", json!({"panel": "Info"})).unwrap();
    assert!(app.ui.dock.is_visible(PanelKind::Info));
}

/// Press and release `button` at `pos`.
fn click_at(h: &mut Harness<'_, FilmcraftApp>, pos: egui::Pos2, button: PointerButton) {
    h.event(Event::PointerMoved(pos));
    step(h);
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos, button, pressed, modifiers: Modifiers::NONE });
        step(h);
    }
    for _ in 0..3 {
        step(h);
    }
}

/// The centre of the open menu's item labeled `item`.
fn menu_item(h: &Harness<'_, FilmcraftApp>, item: &str) -> egui::Pos2 {
    h.query_all_by_label(item)
        .find(|node| node.accesskit_node().role() == egui::accesskit::Role::MenuItem)
        .unwrap_or_else(|| panic!("no menu item {item:?}"))
        .rect()
        .center()
}

/// Right-click the tab `tab` and choose the context-menu item labeled `item`.
fn tab_menu(h: &mut Harness<'_, FilmcraftApp>, tab: &str, item: &str) {
    let pos = rect(h, tab).center();
    click_at(h, pos, PointerButton::Secondary);
    let item = menu_item(h, item);
    click_at(h, item, PointerButton::Primary);
}

/// FilmCraft's Timeline items (Reveal Sequence in Project, Close Other Timeline Panels, Restore
/// Workspace) are in the Timeline's ≡ panel menu: next's tab context menu holds only its own rows.
/// The tab menu's Close item (Close Panel before the port) closes the active sequence: the
/// Timeline stays.
#[test]
fn close_panel_on_the_timeline_tab_closes_the_sequence() {
    let mut h = demo_harness();
    let active = h.state().session.state.active_sequence.unwrap();
    let menu = rect(&h, "panel.menu.Timeline").center();
    click_at(&mut h, menu, PointerButton::Primary);
    for id in ["panel.menu.Timeline.revealSequence", "panel.menu.Timeline.closeOthers"] {
        assert!(h.state().auto.find(id).is_some(), "{id} is missing from the Timeline's panel menu");
    }
    assert!(h.query_all_by_label("Restore Workspace").next().is_some(), "Restore Workspace is missing from the Timeline's panel menu");
    let pos = rect(&h, "panel.tab.Timeline").center();
    click_at(&mut h, pos, PointerButton::Secondary);
    let close = menu_item(&h, "Close");
    click_at(&mut h, close, PointerButton::Primary);
    assert!(!h.state().session.state.open_sequences.contains(&active), "the sequence stays open");
    assert!(h.state().ui.dock.is_visible(PanelKind::Timeline), "the Timeline closed");
}

/// Close Tab Group (Close Panel Group before the port) on a group holding the Timeline closes the
/// active sequence and the group's other panels; the Timeline stays.
#[test]
fn close_panel_group_keeps_the_timeline_and_closes_the_sequence() {
    let mut h = demo_harness();
    let ctx = h.ctx.clone();
    menus::invoke(h.state_mut(), &ctx, "window.dockPanel", json!({"panel": "Info", "anchor": "Timeline", "zone": "center"})).unwrap();
    for _ in 0..3 {
        step(&mut h);
    }
    assert!(same_group(&h.state().ui.dock, PanelKind::Info, PanelKind::Timeline));
    let active = h.state().session.state.active_sequence.unwrap();
    tab_menu(&mut h, "panel.tab.Info", "Close Tab Group");
    let state = h.state();
    assert!(!state.session.state.open_sequences.contains(&active), "the sequence stays open");
    assert!(state.ui.dock.is_visible(PanelKind::Timeline), "the Timeline closed");
    assert!(!state.ui.dock.contains(&PanelKind::Info), "Info stays open");
}

/// With the header in the macOS title-bar strip, a floating window saved at the top of the
/// viewport is drawn below the strip, where a press on it cannot move the app window.
#[test]
fn a_floating_panel_stays_out_of_the_integrated_titlebar() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = layout();
    app.integrated_titlebar = true;
    let ctx = context();
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [45, 65, 420, 320]})).unwrap();
    let window = app.ui.dock.floating.iter_mut().find(|window| window.contains(&PanelKind::Info)).unwrap();
    window.rect = window.rect.translate(vec2(0.0, -window.rect.min.y));
    let mut h = harness(vec2(1200.0, 760.0), app);
    for _ in 0..3 {
        step(&mut h);
    }
    let drawn = h.ctx.memory(|memory| memory.area_rect(window_layer(&h, PanelKind::Info).id)).expect("Info is drawn floating");
    assert!(drawn.min.y >= filmcraft_ui_egui::header::HEIGHT, "drawn at {drawn:?}");
}

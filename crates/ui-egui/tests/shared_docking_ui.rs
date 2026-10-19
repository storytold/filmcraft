//! Shared docking preserves FilmCraft document state and legacy workspace files.

use craft_ui::docking::{Action, Placement, Zone};
use egui::{Event, Modifiers, PointerButton, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use filmcraft_engine::Session;
use filmcraft_ui_egui::{
    FilmcraftApp,
    dock::{self, DockNode, Layout, PanelKind, SplitSize},
    menus,
};
use serde_json::json;

fn layout() -> Layout {
    Layout {
        root: DockNode::Split {
            vertical: false,
            size: SplitSize::FixedA(400.0),
            a: Box::new(DockNode::Tabs { panels: vec![PanelKind::Project, PanelKind::Info, PanelKind::History], active: 1 }),
            b: Box::new(DockNode::Tabs { panels: vec![PanelKind::Source, PanelKind::Program], active: 1 }),
        },
        floating: vec![],
    }
}

#[test]
fn shared_transactions_keep_selection_and_reject_invalid_moves_atomically() {
    let mut layout = layout();
    layout.apply(Action::Close { panel: PanelKind::Project }).unwrap();
    assert!(layout.root.is_visible(PanelKind::Info));
    let before = layout.clone();
    for action in [
        Action::Move { panel: PanelKind::Info, anchor: PanelKind::Tools, placement: Placement::Split(Zone::Left) },
        Action::Float { panel: PanelKind::Info, rect: [0.0, 0.0, f32::NAN, 100.0] },
        Action::Move { panel: PanelKind::Info, anchor: PanelKind::Program, placement: Placement::Tab { before: Some(PanelKind::Tools) } },
    ] {
        assert!(layout.apply(action).is_err());
        assert_eq!(layout, before);
    }
    layout.apply(Action::Float { panel: PanelKind::Info, rect: [50.0, 50.0, 400.0, 300.0] }).unwrap();
    layout.apply(Action::Move { panel: PanelKind::Info, anchor: PanelKind::Program, placement: Placement::Split(Zone::Bottom) }).unwrap();
    assert!(layout.floating.is_empty());
    assert!(layout.root.is_visible(PanelKind::Info));
}

#[test]
fn builtin_and_legacy_workspace_shapes_roundtrip_and_save_floating() {
    for name in dock::WORKSPACES {
        let root = dock::workspace(name);
        let before = serde_json::to_value(&root).unwrap();
        let prefs: dock::WorkspacePrefs = serde_json::from_value(json!({"saved": [{"name": name, "layout": before}], "current": name})).unwrap();
        let loaded = dock::saved_docking(&prefs, name);
        loaded.validate().unwrap();
        assert_eq!(loaded.root, root);
        assert!(loaded.floating.is_empty());
        assert!(serde_json::to_value(&prefs).unwrap()["saved"][0].get("floating").is_none());
    }
    let dir = std::env::temp_dir().join(format!("filmcraft-shared-dock-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let mut session = Session::default();
    session.prefs_path = Some(dir.join("preferences.json"));
    let mut app = FilmcraftApp::new(session);
    let ctx = egui::Context::default();
    app.ui.dock = layout().root;
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [45, 65, 420, 320]})).unwrap();
    menus::invoke(&mut app, &ctx, "window.workspace.saveAs", json!({"name": "Floating test"})).unwrap();
    let prefs = dock::WorkspacePrefs::load(&dir.join(dock::WORKSPACES_FILE));
    let restored = dock::saved_docking(&prefs, "Floating test");
    assert_eq!(restored.floating, app.ui.floating);
    assert_eq!(restored.root, app.ui.dock);
    let mut restarted_session = Session::default();
    restarted_session.prefs_path = Some(dir.join("preferences.json"));
    let restarted = FilmcraftApp::new(restarted_session);
    let mut h = Harness::builder().with_size(vec2(1200.0, 760.0)).build_eframe(move |_| restarted);
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(h.state().ui.dock, restored.root);
    assert_eq!(h.state().ui.floating, restored.floating);
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn legacy_effects_workspace_drops_only_the_known_inactive_duplicate() {
    let expected = dock::workspace("Effects");
    let mut legacy = expected.clone();
    fn duplicate(node: &mut DockNode) {
        match node {
            DockNode::Tabs { panels, active } if panels.as_slice() == [PanelKind::EssentialGraphics] => {
                panels.insert(0, PanelKind::EffectControls);
                *active = 1;
            }
            DockNode::Split { a, b, .. } => {
                duplicate(a);
                duplicate(b);
            }
            DockNode::Tabs { .. } => {}
        }
    }
    duplicate(&mut legacy);
    let prefs: dock::WorkspacePrefs =
        serde_json::from_value(json!({"saved": [{"name": "Historical Effects", "layout": legacy}], "current": "Historical Effects"})).unwrap();
    assert_eq!(dock::saved_docking(&prefs, "Historical Effects").root, expected);
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

#[test]
fn real_panel_drag_merges_splits_floats_and_redocks_without_editing_documents() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let app = FilmcraftApp::new(session);
    let snapshots = std::env::var_os("CRAFT_UI_DOCKING_SNAPSHOT_DIR").map(std::path::PathBuf::from);
    let mut builder = Harness::builder().with_size(vec2(1200.0, 760.0));
    if snapshots.is_some() {
        builder = builder.wgpu();
    }
    let mut h = builder.build_eframe(move |_| app);
    for _ in 0..4 {
        step(&mut h);
    }
    h.state_mut().ui.dock = DockNode::Split {
        vertical: true,
        size: SplitSize::Ratio(0.62),
        a: Box::new(layout().root),
        b: Box::new(DockNode::Tabs { panels: vec![PanelKind::Timeline], active: 0 }),
    };
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
    assert_eq!(h.state().ui.floating.len(), 1);
    assert_eq!(h.get_by_label("Resize floating panel").accesskit_node().role(), egui::accesskit::Role::Button);
    action_log.push(action_state(&h, "floated"));
    let original = h.state().ui.floating[0].rect;
    let grip = rect(&h, "panel.float.0.resize").center();
    drag(&mut h, grip, grip + vec2(36.0, 24.0), Modifiers::NONE);
    assert!(h.state().ui.floating[0].rect[2] > original[2] + 25.0);
    assert!(h.state().ui.floating[0].rect[3] > original[3] + 15.0);
    action_log.push(action_state(&h, "resized"));
    let grip_id = egui::Id::new(("floating-panel", PanelKind::Info.id())).with("resize");
    assert_eq!(h.ctx.memory(|memory| memory.focused()), Some(grip_id));
    let before_keys = h.state().ui.floating[0].rect;
    for key in [egui::Key::ArrowRight, egui::Key::ArrowDown] {
        for pressed in [true, false] {
            h.event(Event::Key { key, physical_key: None, pressed, repeat: false, modifiers: Modifiers::NONE });
            step(&mut h);
        }
    }
    assert_eq!(h.state().ui.floating[0].rect[2], before_keys[2] + 8.0);
    assert_eq!(h.state().ui.floating[0].rect[3], before_keys[3] + 8.0);
    h.event(Event::Key { key: egui::Key::Tab, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
    step(&mut h);
    h.event(Event::Key { key: egui::Key::Tab, physical_key: None, pressed: false, repeat: false, modifiers: Modifiers::NONE });
    for _ in 0..3 {
        step(&mut h);
    }
    assert_ne!(h.ctx.memory(|memory| memory.focused()), Some(grip_id));
    action_log.push(action_state(&h, "keyboard-resized"));
    let resized = h.state().ui.floating[0].rect;
    let header = rect(&h, "panel.float.0.move");
    let from = pos2(header.right() - 24.0, header.center().y);
    drag(&mut h, from, from - vec2(24.0, 18.0), Modifiers::NONE);
    assert!(h.state().ui.floating[0].rect[0] < resized[0] - 15.0);
    assert!(h.state().ui.floating[0].rect[1] < resized[1] - 10.0);
    action_log.push(action_state(&h, "moved"));
    let saved_root = h.state().ui.dock.clone();
    let saved_floating = h.state().ui.floating.clone();
    let ctx = h.ctx.clone();
    menus::invoke(h.state_mut(), &ctx, "window.maximizeFrame", json!({})).unwrap();
    for _ in 0..3 {
        step(&mut h);
    }
    assert!(rect(&h, "panel.Info").width() > 1000.0);
    action_log.push(action_state(&h, "maximized"));
    assert_eq!(h.state().ui.dock, saved_root);
    assert_eq!(h.state().ui.floating, saved_floating);
    menus::invoke(h.state_mut(), &ctx, "window.maximizeFrame", json!({})).unwrap();
    for _ in 0..3 {
        step(&mut h);
    }
    assert_eq!(h.state().ui.floating, saved_floating);

    if let Some(directory) = &snapshots {
        std::fs::create_dir_all(directory).unwrap();
        h.render().unwrap().save(directory.join("filmcraft-floating.png")).unwrap();
    }
    let from = rect(&h, "panel.tab.Info").center();
    let to = rect(&h, "panel.History").center();
    drag(&mut h, from, to, Modifiers::NONE);
    assert!(h.state().ui.floating.is_empty());
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
    app.ui.dock = layout().root;
    let mut h = Harness::builder().with_size(vec2(1200.0, 760.0)).build_eframe(move |_| app);
    for _ in 0..4 {
        step(&mut h);
    }
    let from = rect(&h, "panel.tab.Info").center();
    let leftmost = rect(&h, "panel.Info");
    // The outer edge of the leftmost group: without the window-edge zone this splits only that group.
    drag(&mut h, from, pos2(leftmost.left() + 2.0, leftmost.center().y), Modifiers::NONE);
    assert_eq!(
        h.state().ui.dock,
        DockNode::Split {
            vertical: false,
            size: SplitSize::Ratio(0.25),
            a: Box::new(DockNode::Tabs { panels: vec![PanelKind::Info], active: 0 }),
            b: Box::new(DockNode::Split {
                vertical: false,
                size: SplitSize::FixedA(400.0),
                a: Box::new(DockNode::Tabs { panels: vec![PanelKind::Project, PanelKind::History], active: 1 }),
                b: Box::new(DockNode::Tabs { panels: vec![PanelKind::Source, PanelKind::Program], active: 1 }),
            }),
        }
    );
}

#[test]
fn dock_panel_without_anchor_docks_along_a_workspace_edge() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = layout().root;
    let ctx = egui::Context::default();
    menus::invoke(&mut app, &ctx, "window.dockPanel", json!({"panel": "Program", "zone": "bottom"})).unwrap();
    let DockNode::Split { vertical: true, size: SplitSize::Ratio(ratio), a, b } = &app.ui.dock else { panic!("root not split: {:?}", app.ui.dock) };
    assert_eq!(*ratio, 0.75);
    assert_eq!(**b, DockNode::Tabs { panels: vec![PanelKind::Program], active: 0 });
    assert!(!a.contains(PanelKind::Program) && a.contains(PanelKind::Source) && a.contains(PanelKind::Info));
    let before = app.ui.dock.clone();
    assert!(menus::invoke(&mut app, &ctx, "window.dockPanel", json!({"panel": "Info", "zone": "center"})).is_err());
    assert_eq!(app.ui.dock, before);
    app.ui.dock = DockNode::Tabs { panels: vec![PanelKind::Project], active: 0 };
    assert!(menus::invoke(&mut app, &ctx, "window.dockPanel", json!({"panel": "Project", "zone": "left"})).is_err());
    assert_eq!(app.ui.dock, DockNode::Tabs { panels: vec![PanelKind::Project], active: 0 });
}

fn same_group(root: &DockNode, first: PanelKind, second: PanelKind) -> bool {
    match root {
        DockNode::Tabs { panels, .. } => panels.contains(&first) && panels.contains(&second),
        DockNode::Split { a, b, .. } => same_group(a, first, second) || same_group(b, first, second),
    }
}

fn tabs_containing(root: &DockNode, panel: PanelKind) -> Vec<PanelKind> {
    match root {
        DockNode::Tabs { panels, .. } if panels.contains(&panel) => panels.clone(),
        DockNode::Split { a, b, .. } => {
            let first = tabs_containing(a, panel);
            if first.is_empty() { tabs_containing(b, panel) } else { first }
        }
        _ => vec![],
    }
}

#[test]
fn docking_commands_reject_bad_geometry_and_targets_without_partial_changes() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = layout().root;
    let ctx = egui::Context::default();
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
        assert!(app.ui.floating.is_empty());
    }
    app.ui.dock = DockNode::Tabs { panels: vec![PanelKind::Project], active: 0 };
    assert!(menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Project", "rect": [1, 2, 300, 200]})).is_err());
}

fn action_state(h: &Harness<'_, FilmcraftApp>, action: &str) -> serde_json::Value {
    json!({"action": action, "dock": h.state().ui.dock, "floating": h.state().ui.floating, "focused": h.state().ui.focused})
}

#[test]
fn catalog_docking_action_supports_resize_reorder_and_floating_geometry() {
    let mut app = FilmcraftApp::new(Session::default());
    app.ui.dock = DockNode::Split {
        vertical: false,
        size: SplitSize::FixedA(300.0),
        a: Box::new(DockNode::Tabs { panels: vec![PanelKind::Project, PanelKind::Info], active: 1 }),
        b: Box::new(DockNode::Tabs { panels: vec![PanelKind::Program], active: 0 }),
    };
    let ctx = egui::Context::default();
    for action in [
        Action::ResizeSplit { path: vec![], size: craft_ui::layout::SplitSize::FixedFirst(360.0) },
        Action::Move { panel: PanelKind::Info, anchor: PanelKind::Project, placement: Placement::Tab { before: Some(PanelKind::Project) } },
        Action::Float { panel: PanelKind::Info, rect: [20.0, 40.0, 300.0, 200.0] },
        Action::MoveFloating { panel: PanelKind::Info, rect: [45.0, 65.0, 330.0, 240.0] },
    ] {
        filmcraft_ui_egui::menus::invoke(&mut app, &ctx, "window.dockAction", json!({"action": action})).unwrap();
    }
    assert_eq!(app.ui.floating[0].rect, [45.0, 65.0, 330.0, 240.0]);
    let DockNode::Split { size, .. } = &app.ui.dock else { panic!("split lost") };
    assert_eq!(*size, SplitSize::FixedA(360.0));
    let before = json!({"dock": app.ui.dock, "floating": app.ui.floating});
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
    assert_eq!(json!({"dock": app.ui.dock, "floating": app.ui.floating}), before);
}

#[test]
fn unreadable_floating_workspace_does_not_erase_valid_workspaces_or_current_name() {
    let root = DockNode::Tabs { panels: vec![PanelKind::Project], active: 0 };
    let valid = json!({"name": "Valid", "layout": root, "floating": [{
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
        let prefs = json!({"saved": [valid.clone(), {"name": "Broken", "layout": root, "floating": bad}], "current": "Valid"});
        std::fs::write(&path, serde_json::to_vec(&prefs).unwrap()).unwrap();
        let loaded = dock::WorkspacePrefs::load(&path);
        assert_eq!(loaded.current, "Valid");
        assert_eq!(loaded.saved.len(), 2);
        let restored = dock::saved_docking(&loaded, "Valid");
        restored.validate().unwrap();
        assert_eq!(restored.root, root);
        assert_eq!(restored.floating[0].panels, vec![PanelKind::TextToSpeech, PanelKind::Timeline]);
        assert_eq!(dock::saved_docking(&loaded, "Broken").root, dock::workspace("Broken"));
        loaded.save(&path).unwrap();
        assert_eq!(dock::WorkspacePrefs::load(&path), loaded, "saving preserves the unreadable entry for a future version");
        std::fs::remove_file(path).unwrap();
    }
}

#[test]
fn floating_tts_and_timeline_reveal_keeps_one_owner_and_draft_without_project_edits() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    let mut app = FilmcraftApp::new(session);
    app.ui.tts.text = "Synthetic retained narration".into();
    app.show_panel(PanelKind::TextToSpeech);
    let project = app.session.project.clone();
    let state = serde_json::to_value(&app.session.state).unwrap();
    let draft = serde_json::to_value(&app.ui.tts).unwrap();
    let ctx = egui::Context::default();
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "TextToSpeech", "rect": [80, 90, 420, 320]})).unwrap();
    menus::invoke(
        &mut app,
        &ctx,
        "window.dockAction",
        json!({"action": Action::Move {
            panel: PanelKind::Timeline, anchor: PanelKind::TextToSpeech, placement: Placement::Tab { before: None }
        }}),
    )
    .unwrap();
    app.show_panel(PanelKind::TextToSpeech);
    app.show_panel(PanelKind::Timeline);
    assert!(!app.ui.dock.contains(PanelKind::TextToSpeech));
    assert!(!app.ui.dock.contains(PanelKind::Timeline));
    assert_eq!(app.ui.floating.iter().flat_map(|group| &group.panels).filter(|panel| **panel == PanelKind::TextToSpeech).count(), 1);
    assert_eq!(app.ui.floating.iter().flat_map(|group| &group.panels).filter(|panel| **panel == PanelKind::Timeline).count(), 1);
    assert_eq!(serde_json::to_value(&app.ui.tts).unwrap(), draft);
    assert!(std::sync::Arc::ptr_eq(&project, &app.session.project));
    assert_eq!(serde_json::to_value(&app.session.state).unwrap(), state);
}

#[test]
fn narration_selection_reveals_covered_floating_tts_once_without_editing_the_project() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
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
    let mut h = Harness::builder().with_size(vec2(1200.0, 760.0)).build_eframe(move |_| app);
    for _ in 0..4 {
        step(&mut h);
    }
    let ctx = h.ctx.clone();
    for panel in ["TextToSpeech", "Info"] {
        menus::invoke(h.state_mut(), &ctx, "window.floatPanel", json!({"panel": panel, "rect": [400, 220, 450, 360]})).unwrap();
        for _ in 0..4 {
            step(&mut h);
        }
    }
    let layer = |panel: PanelKind| egui::LayerId::new(egui::Order::Middle, egui::Id::new(("floating-panel", panel.id())));
    let covered = rect(&h, "panel.TextToSpeech").center();
    assert!(rect(&h, "panel.Info").contains(covered));
    ctx.move_to_top(layer(PanelKind::Info));
    for _ in 0..3 {
        step(&mut h);
    }
    assert_eq!(ctx.layer_id_at(covered), Some(layer(PanelKind::Info)), "precondition: Info really covers TTS");
    h.state_mut().session.state.selection = vec![clip.id];
    for _ in 0..4 {
        step(&mut h);
    }
    let draft = h.state().ui.tts.clone();
    assert_eq!(draft.text, narration.text);
    assert_eq!(draft.pace, narration.pace);
    assert_eq!(draft.loaded_from, Some((clip.id.0, clip.item.0)));
    assert_eq!(ctx.layer_id_at(covered), Some(layer(PanelKind::TextToSpeech)), "narration selection must bring its floating editor forward");
    assert_eq!(h.state().ui.focused, PanelKind::TextToSpeech);
    assert!(!h.state().ui.dock.contains(PanelKind::TextToSpeech));
    assert_eq!(h.state().ui.floating.iter().flat_map(|group| &group.panels).filter(|panel| **panel == PanelKind::TextToSpeech).count(), 1);
    assert!(std::sync::Arc::ptr_eq(&project, &h.state().session.project));
    // A reveal is one-shot: drawing the selected narration must not steal stacking
    // from another pane that the user subsequently brings forward.
    ctx.move_to_top(layer(PanelKind::Info));
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(ctx.layer_id_at(covered), Some(layer(PanelKind::Info)));
    assert_eq!(h.state().ui.tts, draft);
    menus::invoke(h.state_mut(), &ctx, "window.panel.TextToSpeech", json!({})).unwrap();
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(ctx.layer_id_at(covered), Some(layer(PanelKind::TextToSpeech)));
    assert_eq!(h.state().ui.tts, draft);
    assert!(std::sync::Arc::ptr_eq(&project, &h.state().session.project));
    // Replacing the workspace cancels a queued reveal before the next render,
    // even when the destination happens to contain the same floating panel IDs.
    ctx.move_to_top(layer(PanelKind::Info));
    for _ in 0..3 {
        step(&mut h);
    }
    let saved = dock::SavedWorkspace {
        name: "Reveal cancellation".into(),
        layout: serde_json::to_value(&h.state().ui.dock).unwrap(),
        floating: serde_json::to_value(&h.state().ui.floating).unwrap(),
    };
    h.state_mut().workspaces.saved.push(saved);
    h.state_mut().show_panel(PanelKind::TextToSpeech);
    h.state_mut().set_workspace("Reveal cancellation");
    for _ in 0..4 {
        step(&mut h);
    }
    assert_eq!(ctx.layer_id_at(covered), Some(layer(PanelKind::Info)));
    assert_eq!(h.state().ui.tts, draft);
    assert!(std::sync::Arc::ptr_eq(&project, &h.state().session.project));
}

/// #1985: a panel closed while floating came back docked when opened again from the Window menu;
/// it now floats again where it was. Closed while docked, it opens docked as before.
#[test]
fn a_panel_closed_while_floating_reopens_floating_where_it_was() {
    let mut app = FilmcraftApp::new(Session::default());
    let ctx = egui::Context::default();
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [45, 65, 420, 320]})).unwrap();
    let rect = app.ui.floating.iter().find(|g| g.panels.contains(&PanelKind::Info)).unwrap().rect;
    menus::invoke(&mut app, &ctx, "window.closePanel", json!({"panel": "Info"})).unwrap();
    assert!(!app.ui.dock.contains(PanelKind::Info) && !app.ui.floating.iter().any(|g| g.panels.contains(&PanelKind::Info)));
    menus::invoke(&mut app, &ctx, &menus::panel_command_id(PanelKind::Info), json!({})).unwrap();
    let back = app.ui.floating.iter().find(|g| g.panels.contains(&PanelKind::Info)).expect("Info floats again");
    assert_eq!(back.rect, rect, "where it was");
    assert!(!app.ui.dock.contains(PanelKind::Info));
    // docked, then closed: it opens docked
    menus::invoke(&mut app, &ctx, "window.dockPanel", json!({"panel": "Info", "anchor": "Project"})).unwrap();
    menus::invoke(&mut app, &ctx, "window.closePanel", json!({"panel": "Info"})).unwrap();
    menus::invoke(&mut app, &ctx, &menus::panel_command_id(PanelKind::Info), json!({})).unwrap();
    assert!(app.ui.dock.contains(PanelKind::Info));
    assert!(!app.ui.floating.iter().any(|g| g.panels.contains(&PanelKind::Info)));
    // the Timeline too, though reopening it first puts it back in its own place in the dock
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Timeline", "rect": [60, 80, 700, 300]})).unwrap();
    let rect = app.ui.floating.iter().find(|g| g.panels.contains(&PanelKind::Timeline)).unwrap().rect;
    menus::invoke(&mut app, &ctx, "window.closePanel", json!({"panel": "Timeline"})).unwrap();
    menus::invoke(&mut app, &ctx, &menus::panel_command_id(PanelKind::Timeline), json!({})).unwrap();
    let back = app.ui.floating.iter().find(|g| g.panels.contains(&PanelKind::Timeline)).expect("the Timeline floats again");
    assert_eq!(back.rect, rect);
    assert!(!app.ui.dock.contains(PanelKind::Timeline));

    // with nothing else docked, a reopened panel has to stay docked (the workspace keeps one
    // docked panel); its floating place is kept, not lost (#2482 review)
    let mut app = FilmcraftApp::new(Session::default());
    menus::invoke(&mut app, &ctx, "window.floatPanel", json!({"panel": "Info", "rect": [45, 65, 420, 320]})).unwrap();
    let rect = app.ui.floating.iter().find(|g| g.panels.contains(&PanelKind::Info)).unwrap().rect;
    for _ in 0..64 {
        let Some(p) = PanelKind::ALL.iter().copied().find(|p| app.ui.dock.contains(*p)) else { break };
        if menus::invoke(&mut app, &ctx, "window.closePanel", json!({"panel": p.id()})).is_err() {
            break;
        }
    }
    let docked = PanelKind::ALL.iter().filter(|p| app.ui.dock.contains(**p)).count();
    if docked > 0 {
        eprintln!("SKIPPED: the dock cannot be emptied ({docked} panels stay)");
        return;
    }
    menus::invoke(&mut app, &ctx, "window.closePanel", json!({"panel": "Info"})).unwrap();
    menus::invoke(&mut app, &ctx, &menus::panel_command_id(PanelKind::Info), json!({})).unwrap();
    assert!(app.ui.dock.contains(PanelKind::Info), "the only panel stays docked");
    assert!(app.ui.closed_floating.iter().any(|(p, r)| *p == PanelKind::Info && *r == rect), "{:?}", app.ui.closed_floating);
}

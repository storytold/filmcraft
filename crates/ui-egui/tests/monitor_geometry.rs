//! Geometry and reachability checks against the production monitor renderer.

use egui::{Event, Modifiers, PointerButton, Rect, pos2, vec2};
use egui_kittest::Harness;
use egui_kittest::kittest::{NodeT, Queryable};
use filmcraft_engine::Session;
use filmcraft_ui_egui::{
    FilmcraftApp,
    dock::{self, DockNode, PanelKind, SeqTabs},
    panels::monitor::{self, Which},
    theme::{self, ThemeKind},
};
use serde_json::json;

fn harness(width: f32, kind: ThemeKind, which: Which) -> Harness<'static, FilmcraftApp> {
    harness_at_scale(width, kind, which, 1.0)
}

fn harness_at_scale(width: f32, kind: ThemeKind, which: Which, scale: f32) -> Harness<'static, FilmcraftApp> {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    session.execute("source.open", json!({"item":5})).unwrap();
    let mut h = Harness::builder().with_size(vec2(width, 340.0)).with_pixels_per_point(scale).build_ui_state(
        move |ui, app: &mut FilmcraftApp| {
            app.set_theme(ui.ctx(), kind);
            app.auto.begin_frame();
            let panel = if which == Which::Source { PanelKind::Source } else { PanelKind::Program };
            let mut node = DockNode::Tabs { panels: vec![panel], active: 0 };
            let mut groups = Vec::new();
            dock::layout(ui, &mut node, ui.max_rect(), &app.tokens, "monitor", &mut groups, &mut app.auto);
            for group in groups {
                // Exercise the actual parent chrome, including its themed panel background.
                dock::draw_group_chrome(ui, &group, panel, &SeqTabs::default(), &app.tokens, &mut app.auto);
                monitor::show(app, ui, group.content, which);
            }
            let source = filmcraft_ui_egui::panels::dragged_source(ui);
            ui.ctx().data_mut(|data| data.insert_temp(egui::Id::new("observed-source-drag"), source));
        },
        FilmcraftApp::new(session),
    );
    theme::install(&h.ctx, &h.state().tokens);
    h.run_steps(4);
    h
}

fn element(h: &Harness<'_, FilmcraftApp>, id: &str) -> Rect {
    let element = h.state().auto.elements.iter().find(|element| element.id == id).unwrap_or_else(|| panic!("missing {id}"));
    let [x, y, w, h] = element.rect;
    Rect::from_min_size(pos2(x, y), vec2(w, h))
}

fn click(h: &mut Harness<'_, FilmcraftApp>, id: &str) {
    let pos = element(h, id).center();
    h.event(Event::PointerMoved(pos));
    for pressed in [true, false] {
        h.event(Event::PointerButton { pos, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        h.step();
    }
    h.run_steps(3);
}

fn overlaps(first: Rect, second: Rect) -> bool {
    let intersection = first.intersect(second);
    intersection.width() > 0.0 && intersection.height() > 0.0
}

#[test]
fn monitor_readouts_and_dropdowns_do_not_overlap() {
    let mut failures = Vec::new();
    for (which, prefix) in [(Which::Source, "source"), (Which::Program, "program")] {
        for kind in [ThemeKind::Dark, ThemeKind::Medium, ThemeKind::Light] {
            for width in [720.0, 500.0, 300.0, 250.0] {
                for scale in [1.0, 2.0] {
                    let mut h = harness_at_scale(width, kind, which, scale);
                    if let Ok(directory) = std::env::var("CRAFT_UI_VISUAL_DIR") {
                        std::fs::create_dir_all(&directory).unwrap();
                        h.render().unwrap().save(format!("{directory}/film-{prefix}-{kind:?}-{width}-{scale}x.png")).unwrap();
                    }
                    let controls = ["timecode", "zoom", "resolution", "settings"].map(|name| element(&h, &format!("{prefix}.{name}")));
                    let timecodes: Vec<_> = h
                        .output()
                        .shapes
                        .iter()
                        .filter_map(|shape| match &shape.shape {
                            egui::Shape::Text(text) if text.galley.text().matches(':').count() == 3 => {
                                Some(text.visual_bounding_rect().intersect(shape.clip_rect))
                            }
                            _ => None,
                        })
                        .collect();
                    assert_eq!(timecodes.len(), 2, "both actual timecode galleys must be painted");
                    if overlaps(timecodes[0], timecodes[1]) {
                        failures.push(format!("{prefix} {kind:?} {width} {scale}x: actual timecode text overlaps: {timecodes:?}"));
                    }
                    for (i, first) in controls.iter().enumerate() {
                        if first.left() < 0.0 || first.right() > width {
                            failures.push(format!("{prefix} {kind:?} {width} {scale}x: outside monitor: {first:?}"));
                        }
                        for second in controls.iter().skip(i + 1) {
                            if overlaps(*first, *second) {
                                failures.push(format!("{prefix} {kind:?} {width} {scale}x: {first:?} overlaps {second:?}"));
                            }
                        }
                    }
                }
            }
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn compact_transport_buttons_stay_inside_monitor() {
    for width in [720.0, 500.0, 300.0, 250.0] {
        let h = harness(width, ThemeKind::Dark, Which::Program);
        for e in &h.state().auto.elements {
            if e.id.starts_with("program.transport.") {
                let r = element(&h, &e.id);
                assert!(r.left() >= 0.0 && r.right() <= width, "{width}: {} {r:?}", e.id);
            }
        }
    }
}

#[test]
fn transport_buttons_are_large_and_evenly_spaced() {
    for (which, prefix) in [(Which::Source, "source"), (Which::Program, "program")] {
        let h = harness(720.0, ThemeKind::Dark, which);
        let mut buttons: Vec<Rect> = h
            .state()
            .auto
            .elements
            .iter()
            .filter(|e| e.id.starts_with(&format!("{prefix}.transport.")) && !e.id.contains(".editor"))
            .map(|e| element(&h, &e.id))
            .collect();
        buttons.sort_by(|a, b| a.left().total_cmp(&b.left()));
        assert!(buttons.len() >= 10, "{prefix}: {buttons:?}");
        for b in &buttons {
            assert!(b.width() >= 28.0 && b.height() >= 28.0, "{prefix}: small button {b:?}");
        }
        let gaps: Vec<f32> = buttons.windows(2).map(|w| w[1].left() - w[0].right()).collect();
        assert!(gaps.iter().all(|g| (g - gaps[0]).abs() < 0.5 && *g > 2.0), "{prefix}: gaps {gaps:?}");
    }
}

#[test]
fn transport_editor_keeps_its_own_space_and_customizes_compact_controls() {
    for width in [720.0, 500.0, 300.0, 250.0, 90.0] {
        let mut h = harness(width, ThemeKind::Light, Which::Program);
        let editor = element(&h, "program.transport.editor");
        for e in &h.state().auto.elements {
            if e.id.starts_with("program.transport.") && e.id != "program.transport.editor" {
                assert!(!overlaps(editor, element(&h, &e.id)), "{width}: editor overlaps {}", e.id);
            }
        }
        click(&mut h, "program.transport.editor");
        let id = "program.transport.editor.playback.toggle";
        let target = element(&h, id);
        if width == 90.0
            && let Ok(directory) = std::env::var("CRAFT_UI_VISUAL_DIR")
        {
            std::fs::create_dir_all(&directory).unwrap();
            h.render().unwrap().save(format!("{directory}/film-program-editor-90.png")).unwrap();
        }
        click(&mut h, id);
        assert!(
            h.state().ui.program.transport_hidden.iter().any(|command| command == "playback.toggle"),
            "width={width} editor={editor:?} checkbox={target:?} hidden={:?}",
            h.state().ui.program.transport_hidden
        );
        if width == 90.0 {
            let scroll = element(&h, "program.transport.editor.scroll");
            let reset_before = element(&h, "program.transport.editor.reset");
            h.event(Event::PointerMoved(scroll.center()));
            h.event(Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: vec2(0.0, -1200.0),
                modifiers: Modifiers::NONE,
                phase: egui::TouchPhase::Move,
            });
            h.run_steps(5);
            let reset = element(&h, "program.transport.editor.reset");
            assert!(reset.top() < reset_before.top(), "real wheel moves clipped editor controls");
            assert!(scroll.contains(reset.center()), "Reset is visible after scrolling: {reset:?}, {scroll:?}");
            click(&mut h, "program.transport.editor.reset");
            assert_eq!(h.state().ui.program.transport_hidden, filmcraft_ui_egui::state::MonitorView::default().transport_hidden);
        }
    }
}

#[test]
fn compact_transport_more_runs_the_existing_proxy_command() {
    let mut h = harness(250.0, ThemeKind::Light, Which::Program);
    let before = h.state().session.media.use_proxies();
    click(&mut h, "program.transport.more");
    assert_eq!(
        h.get_by_label("Toggle Proxies").accesskit_node().toggled(),
        Some(if before { egui::accesskit::Toggled::True } else { egui::accesskit::Toggled::False })
    );
    click(&mut h, "program.transport.media.toggleProxies");
    assert_eq!(h.state().session.media.use_proxies(), !before);
    click(&mut h, "program.transport.more");
    assert_eq!(
        h.get_by_label("Toggle Proxies").accesskit_node().toggled(),
        Some(if before { egui::accesskit::Toggled::False } else { egui::accesskit::Toggled::True })
    );
}

#[test]
fn compact_source_overflow_keeps_source_navigation_routing() {
    let mut h = harness(250.0, ThemeKind::Dark, Which::Source);
    let before = h.state().session.state.source_playhead;
    let program = h.state().session.playhead();
    click(&mut h, "source.transport.more");
    click(&mut h, "source.transport.src.goOut");
    assert!(h.state().session.state.source_playhead > before);
    assert_eq!(h.state().session.playhead(), program);
}

/// #1800: as in Premiere, Go to Previous / Next Edit Point are optional Program transport
/// buttons: off the default bar, added from the Button Editor, beside the one-frame steps (which
/// stay one-frame steps).
#[test]
fn program_transport_edit_point_buttons_come_from_the_button_editor() {
    let mut h = harness(720.0, ThemeKind::Dark, Which::Program);
    let has = |h: &Harness<'_, FilmcraftApp>, id: &str| h.state().auto.elements.iter().any(|e| e.id == id);
    for command in ["playhead.prevEdit", "playhead.nextEdit"] {
        assert!(!has(&h, &format!("program.transport.{command}")), "{command} is off the default bar");
        click(&mut h, "program.transport.editor");
        click(&mut h, &format!("program.transport.editor.{command}"));
        h.event(Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Modifiers::NONE });
        h.run_steps(3);
    }
    let step_back = element(&h, "program.transport.playhead.stepBack");
    let step_fwd = element(&h, "program.transport.playhead.stepForward");
    assert!(element(&h, "program.transport.playhead.prevEdit").right() <= step_back.left(), "Previous Edit Point sits left of Step Back");
    assert!(element(&h, "program.transport.playhead.nextEdit").left() >= step_fwd.right(), "Next Edit Point sits right of Step Forward");

    let first = h.state().session.active_sequence().unwrap().video_tracks[0].items[0].clone();
    h.state_mut().session.set_playhead(first.start + first.duration.mul_ratio(1, 2));
    h.run_steps(2);
    click(&mut h, "program.transport.playhead.nextEdit");
    assert_eq!(h.state().session.playhead(), first.end(), "Next Edit Point lands on the cut after the first clip");
    click(&mut h, "program.transport.playhead.prevEdit");
    assert_eq!(h.state().session.playhead(), first.start, "Previous Edit Point goes back to the first clip's start");
}

fn source_drag_choices(width: f32) {
    for (control, video, audio) in [("video", true, false), ("audio", false, true), ("both", true, true)] {
        let mut h = harness(width, ThemeKind::Dark, Which::Source);
        let rect = element(&h, &format!("source.drag.{control}"));
        assert!(rect.left() >= 0.0 && rect.right() <= width);
        let point = rect.center();
        h.event(Event::PointerMoved(point));
        h.step();
        h.event(Event::PointerButton { pos: point, button: PointerButton::Primary, pressed: true, modifiers: Modifiers::NONE });
        h.step();
        h.event(Event::PointerMoved(point + vec2(0.0, 10.0)));
        h.run_steps(2);
        let source = h
            .ctx
            .data(|data| data.get_temp::<Option<filmcraft_ui_egui::panels::source_drag::SourceDrag>>(egui::Id::new("observed-source-drag")))
            .flatten()
            .expect("drag must create the production Source payload");
        assert_eq!((source.video, source.audio), (video, audio));
        assert_eq!(source.item.0, 5);
        assert!(source.range.duration.0 > 0);
    }
}

#[test]
fn source_drag_choices_remain_reachable_at_500_pixels() {
    source_drag_choices(500.0);
}

#[test]
fn source_drag_choices_remain_reachable_at_250_pixels() {
    source_drag_choices(250.0);
}

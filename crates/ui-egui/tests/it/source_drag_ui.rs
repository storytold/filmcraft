use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use serde_json::json;

fn step(h: &mut Harness<'static, FilmcraftApp>) {
    let ctx = h.ctx.clone();
    let mut raw = std::mem::take(h.input_mut());
    eframe::App::raw_input_hook(h.state_mut(), &ctx, &mut raw);
    *h.input_mut() = raw;
    ctx.request_repaint(); // Direct engine edits in a harness do not wake the native event loop.
    h.step();
}
fn harness(item: u64) -> Harness<'static, FilmcraftApp> {
    let mut s = Session::default();
    s.execute("file.openDemoProject", json!({})).unwrap();
    s.execute("file.newSequence", json!({"name":"Drop test","width":1280,"height":720,"fps":24})).unwrap();
    s.execute("source.open", json!({"item":item})).unwrap();
    let mut h = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).build_eframe(move |_cc| FilmcraftApp::new(s));
    for _ in 0..4 {
        step(&mut h);
    }
    h
}
fn pointer(h: &mut Harness<'static, FilmcraftApp>, pos: egui::Pos2, pressed: Option<bool>) {
    h.input_mut().events.push(egui::Event::PointerMoved(pos));
    if pressed == Some(true) {
        step(h); // Establish hover before pressing, as the live control-channel drag does.
    }
    if let Some(pressed) = pressed {
        h.input_mut().events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE });
    }
    step(h);
}

fn control_rect(h: &Harness<'static, FilmcraftApp>, id: &str) -> egui::Rect {
    let [x, y, w, height] = h.state().auto.elements.iter().find(|e| e.id == id).or_else(|| h.state().auto.find(id)).unwrap().rect;
    egui::Rect::from_min_size(egui::pos2(x, y), egui::vec2(w, height))
}

fn drag(h: &mut Harness<'static, FilmcraftApp>, control: &str, audio: bool, change_marks: bool) {
    let from = control_rect(h, control).center();
    let panel = control_rect(h, "panel.Timeline");
    let row = control_rect(h, if audio { "timeline.track.A1.target" } else { "timeline.track.V1.target" });
    let to = egui::pos2(panel.min.x + h.state().ui.timeline.header_w + 60.0, row.center().y);
    pointer(h, from, Some(true));
    pointer(h, from + egui::vec2(0.0, 9.0), None);
    if change_marks {
        h.state_mut().session.execute("project.setMarks", json!({"item":5,"in":Tick::from_seconds_f64(8.0).0})).unwrap();
    }
    pointer(h, to, None);
    pointer(h, to, Some(false));
    for _ in 0..2 {
        step(h);
    }
}

#[test]
fn source_drag_controls_place_each_stream_choice_and_preserve_range() {
    for (control, audio, video_count, audio_count) in [("source.drag.video", false, 1, 0), ("source.drag.audio", true, 0, 1), ("source.drag.both", false, 1, 1)]
    {
        let mut h = harness(5);
        let start = Tick::from_seconds_f64(1.0);
        let out = Tick::from_seconds_f64(3.0);
        h.state_mut().session.execute("project.setMarks", json!({"item":5,"in":start.0,"out":out.0})).unwrap();
        step(&mut h);
        drag(&mut h, control, audio, false);
        let s = &h.state().session;
        let q = s.active_sequence().unwrap();
        assert_eq!(q.video_tracks.iter().map(|t| t.items.len()).sum::<usize>(), video_count, "{control}: {}", h.state().ui.status);
        assert_eq!(q.audio_tracks.iter().map(|t| t.items.len()).sum::<usize>(), audio_count, "{control}: {}", h.state().ui.status);
        for c in q.all_tracks().flat_map(|t| t.items.iter()) {
            assert_eq!(c.source_in, start);
        }
        if video_count == 1 && audio_count == 1 {
            assert!(q.video_tracks[0].items[0].link.is_some());
            assert_eq!(q.video_tracks[0].items[0].link, q.audio_tracks[0].items[0].link);
        }
    }
}

#[test]
fn drag_captures_marked_range_before_later_marker_changes() {
    let mut h = harness(5);
    let start = Tick::from_seconds_f64(1.0);
    h.state_mut().session.execute("project.setMarks", json!({"item":5,"in":start.0,"out":Tick::from_seconds_f64(3.0).0})).unwrap();
    step(&mut h);
    drag(&mut h, "source.drag.video", false, true);
    assert_eq!(h.state().session.active_sequence().unwrap().video_tracks[0].items[0].source_in, start);
}

#[test]
fn unmarked_clip_drags_full_length_without_creating_marks() {
    let mut h = harness(5);
    drag(&mut h, "source.picture", false, false);
    let s = &h.state().session;
    let c = &s.active_sequence().unwrap().video_tracks[0].items[0];
    assert_eq!(c.source_in, Tick::ZERO);
    assert_eq!(c.duration, s.active_sequence().unwrap().settings.frame_rate.snap_nearest(Tick::from_seconds_f64(18.0)));
    let m = s.project.item(filmcraft_engine::project::ItemId(5)).unwrap().as_media().unwrap();
    assert!(m.mark_in.is_none() && m.mark_out.is_none());
}

#[test]
fn audio_file_drag_places_only_audio() {
    let mut h = harness(11);
    drag(&mut h, "source.drag.audio", true, false);
    let q = h.state().session.active_sequence().unwrap();
    assert_eq!(q.video_tracks.iter().map(|t| t.items.len()).sum::<usize>(), 0);
    assert_eq!(q.audio_tracks[0].items.len(), 1);
}

#[test]
fn clear_in_out_shortcut_addresses_the_focused_monitor_and_is_undoable() {
    use filmcraft_engine::clip_ops::source_view;
    use filmcraft_engine::project::ItemId;
    use filmcraft_ui_egui::dock::PanelKind;
    let mut h = harness(5);
    let start = Tick::from_seconds_f64(1.0);
    let out = Tick::from_seconds_f64(3.0);
    h.state_mut().session.execute("project.setMarks", json!({"item":5,"in":start.0,"out":out.0})).unwrap();
    h.state_mut().session.execute("markers.markIn", json!({"time":start.0})).unwrap();
    h.state_mut().session.execute("markers.markOut", json!({"time":out.0})).unwrap();
    let before = h.state().session.project.to_json();
    let modifiers = egui::Modifiers { shift: true, command: true, ctrl: !cfg!(target_os = "macos"), mac_cmd: cfg!(target_os = "macos"), ..Default::default() };
    let press = |h: &mut Harness<'static, FilmcraftApp>| {
        h.input_mut().events.push(egui::Event::Key { key: egui::Key::X, physical_key: Some(egui::Key::X), pressed: true, repeat: false, modifiers });
        step(h);
        h.input_mut().events.push(egui::Event::Key { key: egui::Key::X, physical_key: Some(egui::Key::X), pressed: false, repeat: false, modifiers });
        step(h);
    };
    h.state_mut().ui.focused = PanelKind::Source;
    press(&mut h);
    let view = source_view(&h.state().session, ItemId(5)).unwrap();
    assert!(view.mark_in.is_none() && view.mark_out.is_none());
    assert_eq!(view.selected_range().duration, view.end - view.start);
    assert_eq!(h.state().session.active_sequence().unwrap().mark_in, Some(start));
    assert_eq!(h.state().session.active_sequence().unwrap().mark_out, Some(out));
    h.state_mut().session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(h.state().session.project.to_json(), before);
    h.state_mut().ui.focused = PanelKind::Program;
    press(&mut h);
    assert!(h.state().session.active_sequence().unwrap().mark_in.is_none());
    assert!(h.state().session.active_sequence().unwrap().mark_out.is_none());
    assert_eq!(source_view(&h.state().session, ItemId(5)).unwrap().mark_in, Some(start));
    h.state_mut().session.state.active_sequence = None;
    h.state_mut().ui.focused = PanelKind::Source;
    press(&mut h);
    assert!(source_view(&h.state().session, ItemId(5)).unwrap().mark_in.is_none());
    let cleared = h.state().session.project.to_json();
    for target in [json!("other"), json!(23), json!(null)] {
        assert!(h.state_mut().session.execute("markers.clearInOut", json!({"target":target})).is_err());
        assert_eq!(h.state().session.project.to_json(), cleared);
    }
}

fn marked_harness() -> Harness<'static, FilmcraftApp> {
    let mut h = harness(5);
    let view = filmcraft_engine::clip_ops::source_view(&h.state().session, filmcraft_engine::project::ItemId(5)).unwrap();
    h.state_mut().session.execute("project.setMarks", json!({"item":5,"in":view.rate.tick_of(48).0,"out":view.rate.tick_of(95).0})).unwrap();
    step(&mut h);
    h
}
fn marked_range(h: &Harness<'static, FilmcraftApp>) -> filmcraft_time::TimeRange {
    filmcraft_engine::clip_ops::source_view(&h.state().session, filmcraft_engine::project::ItemId(5)).unwrap().selected_range()
}
#[test]
fn range_handles_preview_then_commit_one_undo_step() {
    for id in ["source.range.in", "source.range.out", "source.range.body"] {
        let mut h = marked_harness();
        let original = marked_range(&h);
        let before = h.state().session.project.to_json();
        let from = control_rect(&h, id).center();
        let to = from + egui::vec2(35.0, 0.0);
        pointer(&mut h, from, Some(true));
        pointer(&mut h, to, None);
        assert_eq!(h.state().session.project.to_json(), before, "{id} must preview without editing");
        assert!(
            filmcraft_ui_egui::panels::source_range::preview(h.state(), &h.ctx).is_some(),
            "{id}: from={from:?}, source_time={:?}, playhead={:?}",
            h.state().session.state.source_playhead,
            h.state().auto.find("source.playhead")
        );
        pointer(&mut h, to, Some(false));
        let changed = marked_range(&h);
        assert_ne!(changed, original, "{id}: {}", h.state().ui.status);
        if id.ends_with("body") {
            assert_eq!(changed.duration, original.duration);
        }
        if id.ends_with("in") {
            assert_eq!(changed.end(), original.end());
        }
        if id.ends_with("out") {
            assert_eq!(changed.start, original.start);
        }
        h.state_mut().session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(h.state().session.project.to_json(), before);
        h.state_mut().session.execute("edit.redo", json!({})).unwrap();
        assert_eq!(marked_range(&h), changed);
    }
}
#[test]
fn range_drag_escape_and_concurrent_edits_cancel_without_overwriting_marks() {
    for concurrent in [false, true] {
        let mut h = marked_harness();
        let before = h.state().session.project.to_json();
        let from = control_rect(&h, "source.range.in").center();
        let to = from + egui::vec2(35.0, 0.0);
        pointer(&mut h, from, Some(true));
        pointer(&mut h, to, None);
        if concurrent {
            h.state_mut().session.execute("project.setMarks", json!({"item":5,"in":null,"out":null})).unwrap();
        } else {
            h.input_mut().events.push(egui::Event::Key {
                key: egui::Key::Escape,
                physical_key: Some(egui::Key::Escape),
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            });
        }
        step(&mut h);
        let current = h.state().session.project.to_json();
        pointer(&mut h, to, Some(false));
        assert_eq!(h.state().session.project.to_json(), current);
        if !concurrent {
            assert_eq!(current, before);
        }
    }
}
#[test]
fn range_move_clamps_to_media_bounds_and_keeps_duration() {
    for delta in [-3000.0, 3000.0] {
        let mut h = marked_harness();
        let before = marked_range(&h);
        let from = control_rect(&h, "source.range.body").center();
        let to = from + egui::vec2(delta, 0.0);
        pointer(&mut h, from, Some(true));
        pointer(&mut h, to, None);
        pointer(&mut h, to, Some(false));
        let after = marked_range(&h);
        let view = filmcraft_engine::clip_ops::source_view(&h.state().session, filmcraft_engine::project::ItemId(5)).unwrap();
        assert_eq!(after.duration, before.duration);
        assert!(after.start >= view.start && after.end() <= view.end);
    }
}
fn chord(h: &mut Harness<'static, FilmcraftApp>, key: egui::Key, shift: bool, primary: bool) {
    let modifiers = egui::Modifiers {
        shift,
        command: primary,
        ctrl: primary && !cfg!(target_os = "macos"),
        mac_cmd: primary && cfg!(target_os = "macos"),
        ..Default::default()
    };
    for pressed in [true, false] {
        h.input_mut().events.push(egui::Event::Key { key, physical_key: Some(key), pressed, repeat: false, modifiers });
        step(h);
    }
}
#[test]
fn source_marker_navigation_and_clear_shortcuts_leave_program_alone() {
    use filmcraft_ui_egui::dock::PanelKind;
    let mut h = marked_harness();
    h.state_mut().ui.focused = PanelKind::Source;
    let before = h.state().session.active_sequence().unwrap().clone();
    let program_time = h.state().session.playhead();
    chord(&mut h, egui::Key::I, true, false);
    assert_eq!(h.state().session.state.source_playhead, marked_range(&h).start);
    assert_eq!(h.state().session.playhead(), program_time);
    chord(&mut h, egui::Key::M, false, false);
    assert_eq!(filmcraft_engine::clip_ops::source_view(&h.state().session, filmcraft_engine::project::ItemId(5)).unwrap().markers.len(), 1);
    assert_eq!(h.state().session.active_sequence().unwrap(), &before);
    chord(&mut h, egui::Key::I, true, true);
    assert!(filmcraft_engine::clip_ops::source_view(&h.state().session, filmcraft_engine::project::ItemId(5)).unwrap().mark_in.is_none());
    chord(&mut h, egui::Key::O, true, true);
    assert!(filmcraft_engine::clip_ops::source_view(&h.state().session, filmcraft_engine::project::ItemId(5)).unwrap().mark_out.is_none());
    assert_eq!(h.state().session.active_sequence().unwrap(), &before);
    assert_eq!(h.state().session.playhead(), program_time);
}

#[test]
fn program_range_handles_share_behavior_and_leave_source_marks_alone() {
    use filmcraft_engine::{clip_ops::source_view, project::ItemId};
    use filmcraft_ui_egui::dock::PanelKind;
    for id in ["program.range.in", "program.range.out", "program.range.body"] {
        let mut h = marked_harness();
        h.state_mut()
            .session
            .execute("timeline.place", json!({"item":5,"sourceIn":0,"duration":Tick::from_seconds_f64(12.0).0,"video":true,"audio":true}))
            .unwrap();
        let seq = h.state().session.state.active_sequence.unwrap();
        let rate = h.state().session.active_sequence().unwrap().settings.frame_rate;
        h.state_mut().session.execute("project.setMarks", json!({"item":seq.0,"in":rate.tick_of(48).0,"out":rate.tick_of(95).0})).unwrap();
        h.state_mut().ui.focused = PanelKind::Program;
        step(&mut h);
        let original = source_view(&h.state().session, seq).unwrap().selected_range();
        let source = source_view(&h.state().session, ItemId(5)).unwrap().selected_range();
        let before = h.state().session.project.to_json();
        let source_time = h.state().session.state.source_playhead;
        let program_time = h.state().session.playhead();
        let from = control_rect(&h, id).center();
        let to = from + egui::vec2(35.0, 0.0);
        pointer(&mut h, from, Some(true));
        pointer(&mut h, to, None);
        assert_eq!(h.state().session.project.to_json(), before);
        pointer(&mut h, to, Some(false));
        let changed = source_view(&h.state().session, seq).unwrap().selected_range();
        assert_ne!(changed, original, "{id}: {}", h.state().ui.status);
        if id.ends_with("body") {
            assert_eq!(changed.duration, original.duration);
        }
        if id.ends_with("in") {
            assert_eq!(changed.end(), original.end());
        }
        if id.ends_with("out") {
            assert_eq!(changed.start, original.start);
        }
        assert_eq!(source_view(&h.state().session, ItemId(5)).unwrap().selected_range(), source);
        assert_eq!(h.state().session.state.source_playhead, source_time);
        assert_eq!(h.state().session.playhead(), program_time);
        h.state_mut().session.execute("edit.undo", json!({})).unwrap();
        assert_eq!(h.state().session.project.to_json(), before);
        h.state_mut().session.execute("edit.redo", json!({})).unwrap();
        assert_eq!(source_view(&h.state().session, seq).unwrap().selected_range(), changed);
    }
}
#[test]
fn program_range_draft_cancels_when_active_sequence_changes() {
    let mut h = marked_harness();
    h.state_mut().session.execute("timeline.place", json!({"item":5,"sourceIn":0,"duration":Tick::from_seconds_f64(12.0).0})).unwrap();
    let seq = h.state().session.state.active_sequence.unwrap();
    h.state_mut().session.execute("project.setMarks", json!({"item":seq.0,"in":Tick::from_seconds_f64(2.0).0,"out":Tick::from_seconds_f64(5.0).0})).unwrap();
    step(&mut h);
    let before = h.state().session.project.to_json();
    let from = control_rect(&h, "program.range.in").center();
    let to = from + egui::vec2(35.0, 0.0);
    pointer(&mut h, from, Some(true));
    pointer(&mut h, to, None);
    h.state_mut().session.state.active_sequence = None;
    step(&mut h);
    pointer(&mut h, to, Some(false));
    assert_eq!(h.state().session.project.to_json(), before);
}

#[test]
fn playhead_scrubs_above_full_range_and_trim_handles_in_both_monitors() {
    use filmcraft_ui_egui::dock::PanelKind;
    for source in [true, false] {
        let mut h = harness(5);
        if !source {
            h.state_mut().session.execute("timeline.place", json!({"item":5})).unwrap();
        }
        h.state_mut().ui.focused = if source { PanelKind::Source } else { PanelKind::Program };
        if source {
            h.state_mut().session.execute("source.setPlayhead", json!({"time":0})).unwrap();
        } else {
            h.state_mut().session.execute("playhead.set", json!({"time":0})).unwrap();
        }
        step(&mut h);
        let before = h.state().session.project.to_json();
        let from = control_rect(&h, if source { "source.playhead" } else { "program.playhead" }).center();
        let to = from + egui::vec2(110.0, 0.0);
        pointer(&mut h, from, Some(true));
        pointer(&mut h, to, None);
        pointer(&mut h, to, Some(false));
        assert_eq!(h.state().session.project.to_json(), before, "scrubbing must not edit range marks");
        let time = if source { h.state().session.state.source_playhead } else { h.state().session.playhead() };
        assert!(time > Tick::ZERO, "scrubber must win over the In handle");
    }
}

#[test]
fn program_handles_exist_only_for_explicit_marks_and_clear_hides_them() {
    let mut h = harness(5);
    h.state_mut().session.execute("timeline.place", json!({"item":5})).unwrap();
    let seq = h.state().session.state.active_sequence.unwrap();
    step(&mut h);
    let has = |h: &Harness<'static, FilmcraftApp>, id: &str| h.state().auto.elements.iter().any(|e| e.id == id);
    assert!(!has(&h, "program.range.in") && !has(&h, "program.range.out") && !has(&h, "program.range.body"));
    assert!(has(&h, "program.playhead"));
    h.state_mut().session.execute("project.setMarks", json!({"item":seq.0,"in":Tick::from_seconds_f64(2.0).0})).unwrap();
    step(&mut h);
    assert!(has(&h, "program.range.in"));
    assert!(!has(&h, "program.range.out") && !has(&h, "program.range.body"));
    h.state_mut().session.execute("project.setMarks", json!({"item":seq.0,"out":Tick::from_seconds_f64(5.0).0})).unwrap();
    step(&mut h);
    assert!(has(&h, "program.range.in") && has(&h, "program.range.out") && has(&h, "program.range.body"));
    h.state_mut().session.execute("markers.clearInOut", json!({"target":"program"})).unwrap();
    step(&mut h);
    assert!(!has(&h, "program.range.in") && !has(&h, "program.range.out") && !has(&h, "program.range.body"));
    assert!(has(&h, "source.range.in") && has(&h, "source.range.out"));
}

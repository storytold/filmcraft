use super::*;
use crate::{menus, speed_editor::Event};
use filmcraft_engine::Session;

fn app() -> FilmcraftApp {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    session.execute("playhead.set", json!({"frame":24})).unwrap();
    let mut app = FilmcraftApp::new(session);
    app.set_now_for_test(1.0);
    app
}

fn key(app: &mut FilmcraftApp, button: Button, pressed: bool) {
    let button = serde_json::to_value(button).unwrap();
    menus::invoke(app, &Context::default(), "speedEditor.input", json!({"button":button,"pressed":pressed})).unwrap();
}

fn advance(app: &mut FilmcraftApp, seconds: f64) {
    app.set_now_for_test(app.now() + seconds);
    app.poll_speed_editor(&Context::default());
}

fn double(app: &mut FilmcraftApp, button: Button, hold: bool) {
    key(app, button, true);
    key(app, button, false);
    advance(app, 0.1);
    key(app, button, true);
    if !hold {
        key(app, button, false);
    }
}

fn wheel(app: &mut FilmcraftApp, value: i32) {
    menus::invoke(app, &Context::default(), "speedEditor.input", json!({"wheel":value})).unwrap();
}

fn selected_clip(app: &mut FilmcraftApp) -> ClipId {
    let clip = app.session.active_sequence().unwrap().video_tracks.first().unwrap().items.first().unwrap().id;
    app.session.execute("timeline.select", json!({"clips":[clip.0]})).unwrap();
    clip
}

#[test]
fn recognition_requires_a_release_a_matching_key_and_a_short_interval() {
    let mut taps = Taps::default();
    assert!(!taps.press(Button::Escape, 1.0));
    assert!(!taps.press(Button::Escape, 1.1), "held reports are not taps");
    taps.release(Button::Escape);
    assert!(taps.press(Button::Escape, 1.2));
    taps.release(Button::Escape);
    assert!(!taps.press(Button::Escape, 1.3), "a triple tap must not undo twice");
    taps.release(Button::Escape);
    assert!(!taps.press(Button::Escape, 2.0));
    taps.release(Button::Escape);
    assert!(!taps.press(Button::In, 2.1));
    taps.release(Button::In);
    assert!(!taps.press(Button::In, 1.0), "a backwards clock cannot create a double tap");
    for time in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
        assert!(!taps.press(Button::Escape, time));
    }
}

#[test]
fn single_split_waits_then_cuts_at_the_original_frame_even_without_a_usb_hook() {
    let mut app = app();
    let before = app.session.project.to_json();
    key(&mut app, Button::Split, true);
    key(&mut app, Button::Split, false);
    advance(&mut app, 0.2);
    assert_eq!(app.session.project.to_json(), before);
    app.session.execute("playhead.set", json!({"frame":48})).unwrap();
    advance(&mut app, 0.2);
    assert_ne!(app.session.project.to_json(), before);
    let cut = app.session.sequence_rate().tick_of(24);
    assert!(app.session.active_sequence().unwrap().video_tracks.iter().any(|track| track.items.iter().any(|item| item.start == cut)));
    app.session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(app.session.project.to_json(), before);
}

#[test]
fn double_split_moves_unique_selected_clips_without_an_extra_cut_and_is_undoable() {
    let mut app = app();
    let clip = selected_clip(&mut app);
    app.session.state.selection.push(clip);
    let before = app.session.project.to_json();
    let start = app.session.active_sequence().unwrap().find_item(clip).unwrap().1.start;
    double(&mut app, Button::Split, true);
    assert_eq!(app.session.project.to_json(), before);
    advance(&mut app, 0.5);
    assert_eq!(app.session.project.to_json(), before, "a held second tap must never become a cut");
    wheel(&mut app, 128);
    assert_eq!(app.session.active_sequence().unwrap().find_item(clip).unwrap().1.start, start + app.session.sequence_rate().frame_duration());
    key(&mut app, Button::Split, false);
    app.session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(app.session.project.to_json(), before);
    assert_eq!(app.speed_editor.telemetry.double_taps, 1);
}

#[test]
fn double_escape_undoes_once_and_leaves_redo_available() {
    let mut app = app();
    let before = app.session.project.to_json();
    app.session.execute("sequence.addEdit", json!({})).unwrap();
    let edited = app.session.project.to_json();
    double(&mut app, Button::Escape, false);
    assert_eq!(app.session.project.to_json(), before);
    advance(&mut app, 0.5);
    assert_eq!(app.session.project.to_json(), before);
    app.session.execute("edit.redo", json!({})).unwrap();
    assert_eq!(app.session.project.to_json(), edited);
}

#[test]
fn double_roll_slides_the_clip_and_release_restores_scrubbing() {
    let mut app = app();
    let rate = app.session.sequence_rate();
    for frame in [24, 48] {
        app.session.execute("sequence.addEdit", json!({"time":rate.tick_of(frame).0})).unwrap();
    }
    let clip = app.session.active_sequence().unwrap().video_tracks.first().unwrap().items.iter().find(|item| item.start == rate.tick_of(24)).unwrap().id;
    app.session.execute("timeline.select", json!({"clips":[clip.0]})).unwrap();
    app.session.execute("playhead.set", json!({"frame":36})).unwrap();
    let before = app.session.project.to_json();
    let length = app.session.active_sequence().unwrap().duration();
    let source_in = app.session.active_sequence().unwrap().find_item(clip).unwrap().1.source_in;
    double(&mut app, Button::Roll, true);
    wheel(&mut app, 128);
    let item = app.session.active_sequence().unwrap().find_item(clip).unwrap().1;
    assert_eq!(item.start, rate.tick_of(25));
    assert_eq!(item.source_in, source_in);
    assert_eq!(app.session.active_sequence().unwrap().duration(), length);
    key(&mut app, Button::Roll, false);
    app.session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(app.session.project.to_json(), before);
    let ph = app.session.playhead();
    wheel(&mut app, 128);
    assert_eq!(app.session.playhead(), ph + rate.frame_duration());
    assert_eq!(app.session.project.to_json(), before);
}

#[test]
fn native_capture_times_distinguish_queued_double_taps_from_two_slow_taps() {
    use std::{
        cell::RefCell,
        rc::Rc,
        time::{Duration, Instant},
    };
    for interval in [0.1, 0.5] {
        let mut app = app();
        app.set_now_for_test(10.0);
        app.session.execute("sequence.addEdit", json!({})).unwrap();
        let edited = app.session.project.to_json();
        let base = Instant::now().checked_sub(Duration::from_secs(2)).unwrap();
        let pending = Rc::new(RefCell::new(vec![
            Event::TimedButtons { buttons: vec![Button::Escape], at: base },
            Event::TimedButtons { buttons: vec![], at: base + Duration::from_millis(20) },
            Event::TimedButtons { buttons: vec![Button::Escape], at: base + Duration::from_secs_f64(interval) },
            Event::TimedButtons { buttons: vec![], at: base + Duration::from_secs_f64(interval + 0.02) },
        ]));
        let events = pending.clone();
        app.hooks.speed_editor = Some(Box::new(move |_| events.borrow_mut().drain(..).collect()));
        let ctx = Context::default();
        ctx.run_ui(egui::RawInput { focused: true, ..Default::default() }, |ui| app.poll_speed_editor(ui.ctx())).textures_delta.clear();
        assert_eq!(app.speed_editor.telemetry.double_taps, u64::from(interval < DOUBLE_TAP_SECONDS));
        assert_eq!(app.session.project.to_json() == edited, interval > DOUBLE_TAP_SECONDS);
    }
}

#[test]
fn double_audio_level_adds_one_marker_in_the_selected_monitor() {
    for source in [false, true] {
        let mut app = app();
        if source {
            app.session.execute("source.open", json!({"item":5})).unwrap();
            key(&mut app, Button::Source, true);
            key(&mut app, Button::Source, false);
        }
        let before: Vec<_> = if source {
            filmcraft_engine::clip_ops::source_view(&app.session, app.session.state.source_item.unwrap())
                .unwrap()
                .markers
                .iter()
                .map(|marker| marker.id)
                .collect()
        } else {
            app.session.active_sequence().unwrap().markers.iter().map(|marker| marker.id).collect()
        };
        double(&mut app, Button::AudioLevel, false);
        let markers = if source {
            filmcraft_engine::clip_ops::source_view(&app.session, app.session.state.source_item.unwrap()).unwrap().markers
        } else {
            app.session.active_sequence().unwrap().markers.clone()
        };
        assert_eq!(markers.len(), before.len() + 1);
        assert_eq!(
            markers.iter().find(|marker| !before.contains(&marker.id)).unwrap().start,
            if source { app.session.state.source_playhead } else { app.session.playhead() }
        );
    }
}

#[test]
fn double_in_and_out_clear_the_correct_monitors_marks() {
    for source in [false, true] {
        let mut app = app();
        if source {
            app.session.execute("source.open", json!({"item":5})).unwrap();
            app.speed_editor.source = true;
        }
        double(&mut app, Button::In, false);
        double(&mut app, Button::Out, false);
        if source {
            let view = filmcraft_engine::clip_ops::source_view(&app.session, app.session.state.source_item.unwrap()).unwrap();
            assert_eq!((view.mark_in, view.mark_out), (None, None));
        } else {
            let sequence = app.session.active_sequence().unwrap();
            assert_eq!((sequence.mark_in, sequence.mark_out), (None, None));
        }
    }
}

#[test]
fn transition_dial_and_set_use_existing_transition_commands_and_persist_defaults() {
    let mut app = app();
    let clip = selected_clip(&mut app);
    let applied = app.session.execute("sequence.applyVideoTransition", json!({"clip":clip.0,"edge":"out","frames":12})).unwrap();
    let transition = applied["transition"].as_u64().unwrap();
    app.session.state.transition_selection = vec![TransitionId(transition)];
    key(&mut app, Button::TransitionDuration, true);
    wheel(&mut app, 128);
    key(&mut app, Button::TransitionDuration, false);
    advance(&mut app, 0.5);
    let before = app.session.project.to_json();
    double(&mut app, Button::TransitionDuration, false);
    assert_eq!(app.session.project.to_json(), before, "SET updates preferences, not the project");
    assert_eq!(app.session.prefs.timeline.video_transition_unit, "frames");
    assert_eq!(app.session.prefs.timeline.video_transition_duration, 13.0);
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("prefs.json");
    app.session.prefs.save(&path).unwrap();
    assert_eq!(filmcraft_engine::autosave::Preferences::load(&path).timeline.video_transition_duration, 13.0);
}

#[test]
fn whole_clip_append_ignores_marks_and_adds_only_once() {
    let mut app = app();
    app.session.execute("source.open", json!({"item":5})).unwrap();
    app.session.execute("markers.markIn", json!({"target":"source","time":app.session.sequence_rate().tick_of(12).0})).unwrap();
    let view = filmcraft_engine::clip_ops::source_view(&app.session, app.session.state.source_item.unwrap()).unwrap();
    let duration = app.session.active_sequence().unwrap().duration();
    let before = app.session.project.to_json();
    double(&mut app, Button::Append, false);
    advance(&mut app, 0.5);
    let added = app.session.active_sequence().unwrap().duration() - duration;
    assert!(added >= view.end - view.start && added - (view.end - view.start) < app.session.sequence_rate().frame_duration());
    app.session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(app.session.project.to_json(), before);
}

#[test]
fn title_secondary_does_not_apply_the_primary_transition() {
    let mut app = app();
    let before = app.session.project.to_json();
    let transitions = app.session.active_sequence().unwrap().all_tracks().map(|track| track.transitions.len()).sum::<usize>();
    double(&mut app, Button::Transition, false);
    assert_eq!(app.session.active_sequence().unwrap().all_tracks().map(|track| track.transitions.len()).sum::<usize>(), transitions);
    assert_ne!(app.session.project.to_json(), before);
    app.session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(app.session.project.to_json(), before);
}

#[test]
fn review_maximizes_program_and_plays_around_a_cut_without_editing() {
    let mut app = app();
    app.session.execute("sequence.addEdit", json!({})).unwrap();
    let before = app.session.project.to_json();
    double(&mut app, Button::FullView, false);
    assert_eq!(app.ui.keys.maximized, Some(PanelKind::Program));
    assert!(app.session.trim_play.active());
    assert_eq!(app.session.project.to_json(), before);
}

#[test]
fn y_position_and_audio_level_wheel_edits_are_undoable() {
    let mut app = app();
    let clip = selected_clip(&mut app);
    let before = app.session.project.to_json();
    double(&mut app, Button::CloseUp, true);
    wheel(&mut app, 128);
    key(&mut app, Button::CloseUp, false);
    assert_ne!(app.session.project.to_json(), before);
    app.session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(app.session.project.to_json(), before);
    assert_eq!(app.session.active_sequence().unwrap().find_item(clip).unwrap().1.id, clip);

    let audio = app.session.active_sequence().unwrap().audio_tracks.first().unwrap().items.first().unwrap().id;
    app.session.execute("timeline.select", json!({"clips":[audio.0]})).unwrap();
    let gain = app.session.active_sequence().unwrap().find_item(audio).unwrap().1.gain_db;
    key(&mut app, Button::AudioLevel, true);
    wheel(&mut app, 128);
    key(&mut app, Button::AudioLevel, false);
    assert!((app.session.active_sequence().unwrap().find_item(audio).unwrap().1.gain_db - gain - 0.1).abs() < 1e-9);
    app.session.execute("edit.undo", json!({})).unwrap();
    assert_eq!(app.session.project.to_json(), before);
}

#[test]
fn secondary_viewer_zoom_does_not_toggle_primary_snapping() {
    let mut app = app();
    let snapping = app.session.state.snapping;
    double(&mut app, Button::Snap, false);
    assert_eq!(app.session.state.snapping, snapping);
    assert_eq!(app.ui.program.zoom, Some(1.0));
    double(&mut app, Button::Snap, false);
    assert_eq!(app.ui.program.zoom, None);
}

#[test]
fn monitor_only_records_double_taps_without_applying_them() {
    let mut app = app();
    menus::invoke(&mut app, &Context::default(), "devices.speedEditor", json!({})).unwrap();
    let before = app.session.project.to_json();
    for button in [Button::Escape, Button::Split, Button::AudioLevel, Button::Roll] {
        double(&mut app, button, false);
    }
    assert_eq!(app.session.project.to_json(), before);
    assert_eq!(app.speed_editor.telemetry.double_taps, 4);
    assert_eq!(app.speed_editor.telemetry.last_gesture.as_ref().unwrap().action, "SLIDE");
    assert!(app.speed_editor.gestures.pending.is_none());
}

#[test]
fn pending_cuts_cancel_on_escape_disconnect_focus_loss_and_project_changes() {
    for reason in 0..4 {
        let mut app = app();
        let ctx = Context::default();
        key(&mut app, Button::Split, true);
        key(&mut app, Button::Split, false);
        match reason {
            0 => {
                key(&mut app, Button::Escape, true);
                key(&mut app, Button::Escape, false);
            }
            1 => app.speed_editor_event(&ctx, Event::Connection { connected: false, detail: "unplugged".into() }).unwrap(),
            2 => {
                app.speed_editor.virtual_input = false;
                ctx.run_ui(egui::RawInput { focused: false, ..Default::default() }, |ui| app.poll_speed_editor(ui.ctx())).textures_delta.clear();
            }
            _ => {
                app.session.execute("graphics.newText", json!({"text":"Independent edit"})).unwrap();
            }
        }
        let before = app.session.project.to_json();
        advance(&mut app, 0.5);
        assert_eq!(app.session.project.to_json(), before);
    }
}

use super::super::FilmcraftApp;

/// Fixed Alternate Drag Speed rates used by the side track sliders.
pub const SHIFT_THUMB_RATE: f64 = 0.06;
pub const SHIFT_CONTENT_RATE: f64 = 0.2;

/// The track sliders' own Controls bindings, deliberately UNLINKED from the zoom bar's: both
/// menus share the action names, but naming a control on one slider must never rebind the
/// other's (Mirror Slider / Explode Slider / ...).
pub(in crate::panels::timeline::scrollbars) fn track_control_binding(app: &mut FilmcraftApp, slot: usize) -> &mut String {
    let s = &mut app.session.state.timeline_ui;
    match slot {
        0 => &mut s.track_control_mirror,
        1 => &mut s.track_control_explode,
        2 => &mut s.track_control_alternate_drag_speed,
        4 => s.timeline_control_bindings.get_mut(0).unwrap_or(&mut s.track_control_settings),
        6 => s.timeline_control_bindings.get_mut(2).unwrap_or(&mut s.track_control_settings),
        _ => &mut s.track_control_settings,
    }
}

pub(in crate::panels::timeline::scrollbars) fn track_slider_command(slot: usize) -> Option<(&'static str, &'static str)> {
    match slot {
        0 => Some(("timeline.trackSliderMirror", "Double Click")),
        1 => Some(("timeline.trackSliderExplode", "Shift+Double Click")),
        2 => Some(("timeline.trackSliderAlternate", "Shift+Drag")),
        4 => Some(("timeline.adjustTrackHeight", "")),
        6 => Some(("timeline.scrollTrackRows", "Ctrl+Scroll")),
        _ => None,
    }
}

pub(in crate::panels::timeline::scrollbars) fn controls_listen_track_key() -> egui::Id {
    egui::Id::unique("tl-controls-listening-track")
}

pub(in crate::panels::timeline::scrollbars) fn track_trigger_key() -> egui::Id {
    egui::Id::unique("tl-controls-track-trigger")
}

fn close_track_controls_for_conflict(app: &mut FilmcraftApp, ctx: &egui::Context) {
    app.tl.track_slider_settings_pos = None;
    super::super::track_slider_state_reset(ctx);
}

fn reset_track_control(app: &mut FilmcraftApp, slot: usize) {
    super::reset_slider_binding_for(app, controls_profile(), slot);
}

pub(in crate::panels::timeline::scrollbars) fn controls_profile() -> super::SliderControlProfile {
    super::SliderControlProfile {
        captures_track: true,
        binding: track_control_binding,
        command: track_slider_command,
        reset: reset_track_control,
        close_for_conflict: close_track_controls_for_conflict,
        secondary_binding: None,
        show_control_tracks_angle: false,
    }
}

/// Whether the side-slider bindings can own a shortcut at the current pointer location.
pub(in crate::panels::timeline::scrollbars) fn available_at(
    app: &FilmcraftApp,
    ctx: &egui::Context,
    pointer: Option<egui::Pos2>,
    require_pointer: bool,
) -> bool {
    !app.session.state.timeline_ui.track_slider_hidden
        && (!require_pointer
            || pointer.is_some_and(|pos| {
                ["timeline-track-slider-video-rect", "timeline-track-slider-audio-rect"]
                    .into_iter()
                    .any(|id| ctx.data(|data| data.get_temp::<egui::Rect>(egui::Id::unique(id))).is_some_and(|rect| rect.contains(pos)))
            }))
}

pub(in crate::panels::timeline::scrollbars) fn still_owns_alternate_drag(app: &FilmcraftApp, binding: &str) -> bool {
    app.tl.track_slider_dragging && binding == app.session.state.timeline_ui.track_control_alternate_drag_speed
}

/// Route only side-slider bindings. Shared capture and Timeline-wide controls stay in the parent
/// module so the zoom bar does not depend on track-slider commands.
pub(in crate::panels::timeline::scrollbars) fn route_track_shortcut_text(
    app: &FilmcraftApp,
    ctx: &egui::Context,
    events: &mut Vec<egui::Event>,
    text: &str,
    pointer: Option<egui::Pos2>,
    require_pointer: bool,
) {
    let settings = &app.session.state.timeline_ui;
    if app.tl.track_slider_dragging && !settings.track_control_alternate_drag_speed.is_empty() && settings.track_control_alternate_drag_speed == text {
        ctx.data_mut(|data| data.insert_temp(super::alternate_drag_capture_key(), text.to_owned()));
        super::consume_slider_shortcut_events(events);
        ctx.input_mut(|input| input.events = events.to_vec());
        return;
    }
    if app.session.state.active_sequence.is_none() || !available_at(app, ctx, pointer, require_pointer) {
        return;
    }
    let bindings =
        [&settings.track_control_mirror, &settings.track_control_explode, &settings.track_control_alternate_drag_speed, &settings.track_control_settings];
    let Some((slot, _)) = bindings.iter().enumerate().find(|(slot, binding)| *slot != 2 && !binding.is_empty() && binding.as_str() == text) else {
        return;
    };
    ctx.data_mut(|data| data.insert_temp(track_trigger_key(), slot));
    super::consume_slider_shortcut_events(events);
    ctx.input_mut(|input| input.events = events.to_vec());
}

pub fn sync_track_slider_bindings(app: &mut FilmcraftApp) {
    for slot in 0..=2 {
        super::refresh_slider_binding_for(app, controls_profile(), slot);
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn resetting_scroll_track_rows_restores_ctrl_scroll_and_spaced_gesture_label() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.shortcuts.set_gesture("timeline.scrollTrackRows", "Alt + Scroll").expect("set a custom row-scroll gesture");

        super::super::reset_slider_binding(&mut app, false, 6);

        assert_eq!(app.session.shortcuts.primary("timeline.scrollTrackRows").as_deref(), Some("Ctrl+Scroll"));
        assert_eq!(super::super::control_binding(&mut app, 6), "Ctrl + Scroll");
    }

    #[test]
    fn track_slider_reset_restores_its_default_without_changing_zoom_bindings() {
        let mut app = crate::FilmcraftApp::new(filmcraft_engine::Session::default());
        app.session.shortcuts.set("timeline.sliderCenter", "Ctrl+K", None, false, true).expect("assign zoom-slider key");
        app.session.shortcuts.set("timeline.trackSliderMirror", "Ctrl+L", None, false, true).expect("assign track-slider key");
        app.session.state.timeline_ui.control_center_on_playhead = "Ctrl + K".into();

        super::super::reset_slider_binding(&mut app, true, 0);

        assert_eq!(app.session.shortcuts.primary("timeline.trackSliderMirror").as_deref(), Some("Double Click"));
        assert_eq!(app.session.shortcuts.primary("timeline.sliderCenter").as_deref(), Some("Ctrl+K"));
        assert_eq!(app.session.state.timeline_ui.control_center_on_playhead, "Ctrl + K");
        assert_eq!(app.session.state.timeline_ui.track_control_mirror, "Double Click");
    }
}

#[cfg(test)]
mod routing_tests {
    use egui::{Event, Modifiers};

    #[test]
    fn hidden_or_missing_track_slider_does_not_reserve_its_shortcut() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;

        for (hidden, open_sequence, expect_reserved) in [(true, true, false), (false, false, false), (false, true, true)] {
            let mut session = Session::default();
            if open_sequence {
                session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
            }
            session.state.timeline_ui.track_slider_hidden = hidden;
            session.state.timeline_ui.track_control_mirror = "Ctrl + K".into();
            let mut app = FilmcraftApp::new(session);
            app.ui.focused = PanelKind::Timeline;
            let ctx = egui::Context::default();
            let modifiers = Modifiers { ctrl: true, ..Modifiers::default() };
            ctx.data_mut(|data| {
                data.insert_temp(
                    egui::Id::unique("timeline-track-slider-video-rect"),
                    egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)),
                );
            });
            let input = egui::RawInput {
                events: vec![
                    Event::PointerMoved(egui::pos2(50.0, 50.0)),
                    Event::Key { key: egui::Key::K, physical_key: Some(egui::Key::K), pressed: true, repeat: false, modifiers },
                ],
                ..Default::default()
            };
            let mut output = ctx.run_ui(input, |ui| super::super::super::route_slider_shortcut(&app, ui.ctx()));
            output.textures_delta.clear();
            assert_eq!(ctx.data(|data| data.get_temp::<usize>(super::track_trigger_key()).is_some()), expect_reserved);
        }
    }

    #[test]
    fn zoom_owned_extra_binding_is_not_reserved_when_only_the_track_slider_is_available() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        session.state.timeline_ui.zoom_bar_hidden = true;
        session.state.timeline_ui.track_slider_hidden = false;
        assert!(session.state.timeline_ui.timeline_control_bindings.len() > 4);
        session.state.timeline_ui.timeline_control_bindings[4] = "Ctrl + M".into();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        let ctx = egui::Context::default();
        ctx.data_mut(|d| {
            d.insert_temp(egui::Id::unique("timeline-track-slider-video-rect"), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)))
        });
        let input = egui::RawInput {
            events: vec![
                Event::PointerMoved(egui::pos2(50.0, 50.0)),
                Event::Key {
                    key: egui::Key::M,
                    physical_key: Some(egui::Key::M),
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers { ctrl: true, ..Modifiers::default() },
                },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| super::super::super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(ctx.data(|d| d.get_temp::<usize>(egui::Id::unique("tl-controls-zoom-extra-action"))), None);
        assert!(ctx.input(|i| i.events.iter().any(|event| matches!(event, Event::Key { key: egui::Key::M, pressed: true, .. }))));
    }

    #[test]
    fn alternate_drag_binding_is_reserved_only_during_its_own_drag() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        session.state.timeline_ui.track_control_alternate_drag_speed = "Ctrl + S".into();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        let input = || egui::RawInput {
            events: vec![Event::Key {
                key: egui::Key::S,
                physical_key: Some(egui::Key::S),
                pressed: true,
                repeat: false,
                modifiers: Modifiers { ctrl: true, ..Modifiers::default() },
            }],
            ..Default::default()
        };

        let outside_drag = egui::Context::default();
        let mut output = outside_drag.run_ui(input(), |ui| super::super::super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert!(outside_drag.input(|i| i.events.iter().any(|event| matches!(event, Event::Key { key: egui::Key::S, pressed: true, .. }))));

        app.tl.track_slider_dragging = true;
        let during_drag = egui::Context::default();
        let mut alternate_is_down = false;
        let mut output = during_drag.run_ui(input(), |ui| {
            super::super::super::route_slider_shortcut(&app, ui.ctx());
            alternate_is_down = super::super::alternate_down(ui.ctx(), "Ctrl + S");
        });
        output.textures_delta.clear();
        assert!(during_drag.input(|i| !i.events.iter().any(|event| matches!(event, Event::Key { key: egui::Key::S, pressed: true, .. }))));
        assert!(alternate_is_down, "the drag still sees its alternate-speed chord");
        assert_eq!(during_drag.data(|data| data.get_temp::<usize>(super::track_trigger_key())), None);
    }

    #[test]
    fn track_slider_binding_does_not_reserve_shortcut_over_the_zoom_slider() {
        use crate::{FilmcraftApp, dock::PanelKind};
        use filmcraft_engine::Session;
        let mut session = Session::default();
        session.execute("file.openDemoProject", serde_json::json!({})).unwrap();
        session.state.timeline_ui.track_control_mirror = "Ctrl + K".into();
        let mut app = FilmcraftApp::new(session);
        app.ui.focused = PanelKind::Timeline;
        let ctx = egui::Context::default();
        app.session.state.timeline_ui.track_slider_hidden = true;
        ctx.data_mut(|data| {
            data.insert_temp(egui::Id::unique("timeline-zoom-slider-rect"), egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(100.0, 100.0)))
        });
        let input = egui::RawInput {
            events: vec![
                Event::PointerMoved(egui::pos2(50.0, 50.0)),
                Event::Key {
                    key: egui::Key::K,
                    physical_key: Some(egui::Key::K),
                    pressed: true,
                    repeat: false,
                    modifiers: Modifiers { ctrl: true, ..Modifiers::default() },
                },
            ],
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| super::super::super::route_slider_shortcut(&app, ui.ctx()));
        output.textures_delta.clear();
        assert_eq!(ctx.data(|data| data.get_temp::<usize>(super::track_trigger_key())), None);
        assert!(ctx.input(|i| i.events.iter().any(|event| matches!(event, Event::Key { key: egui::Key::K, pressed: true, .. }))));
    }
}

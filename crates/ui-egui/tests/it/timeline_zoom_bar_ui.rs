//! Headless UI tests of the Timeline zoom bar: thumb scrolling, edge zooming, custom Controls bindings, and cursor feedback.

use crate::support::{Drive, builder, demo_session, driver, f32x4, launch, start};
use egui::CursorIcon;
use filmcraft_ui_egui::panels::timeline::max_scroll;
use serde_json::json;

driver! {
    Driver {
        /// Where the pointer was last put by `move_to` or `press`.
        pointer: egui::Pos2,
    }
}

impl Driver {
    fn demo() -> Self {
        let (harness, tx) = launch(demo_session(), builder());
        start(Driver { harness, tx, pointer: egui::Pos2::ZERO })
    }
}

impl Driver {
    fn rect(&mut self, id: &str) -> [f32; 4] {
        f32x4(self.expect_rect(id))
    }

    /// (time at the left edge, zoom, video scroll, audio scroll), once the view has settled.
    fn view(&mut self) -> (f64, f64, f32, f32) {
        for _ in 0..200 {
            let v = &self.app().ui.timeline;
            if v.pps == v.target_pps && v.scroll == v.target_scroll {
                break;
            }
            self.frames(1);
        }
        let v = &self.app().ui.timeline;
        (v.target_scroll, v.target_pps, v.v_scroll, v.a_scroll)
    }

    /// The demo sequence with enough tracks to scroll both halves, at a known zoom.
    fn tall() -> Driver {
        let mut d = Driver::demo();
        d.exec("sequence.addTracks", json!({"video": 12, "audio": 12}));
        d.ok("ui.set", json!({"timeline": {"pps": 50.0, "scroll": 4.0}}));
        d.frames(4);
        assert_eq!(d.view(), (4.0, 50.0, 0.0, 0.0));
        d
    }

    fn seconds(&mut self) -> f64 {
        self.app().session.active_sequence().unwrap().duration().seconds()
    }

    /// Move the pointer to `(x, y)` a few points per frame, as a mouse does (a button held stays
    /// held), and let the Timeline react.
    fn move_to(&mut self, x: f32, y: f32) {
        let (from, to) = (self.pointer, egui::pos2(x, y));
        let steps = ((to - from).length() / 20.0).ceil().max(1.0) as usize;
        for i in 1..=steps {
            self.harness.input_mut().events.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
        self.pointer = to;
        self.frames(2);
    }

    /// Press the primary button at `(x, y)`.
    fn press(&mut self, x: f32, y: f32) {
        let pos = egui::pos2(x, y);
        let events = &mut self.harness.input_mut().events;
        events.push(egui::Event::PointerMoved(pos));
        events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.pointer = pos;
        self.frames(1);
    }

    fn release(&mut self, x: f32, y: f32) {
        let pos = egui::pos2(x, y);
        self.harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        self.frames(2);
    }

    /// The cursor the last frame asked for.
    fn cursor(&self) -> CursorIcon {
        self.harness.output().platform_output.cursor_icon
    }
}

#[test]
fn the_scroll_bar_thumb_follows_the_pointer_and_reaches_both_ends() {
    let mut d = Driver::tall();
    let seconds = d.seconds();
    let track = d.rect("timeline.zoomBar.track");
    let limit = max_scroll(seconds, d.rect("timeline.tracks")[2], 50.0);
    // at the start the thumb is at the left end of the bar, and wide enough to grab
    d.ok("ui.set", json!({"timeline": {"scroll": 0.0}}));
    d.frames(3);
    let thumb = d.rect("timeline.zoomBar");
    assert!((thumb[0] - track[0]).abs() < 0.5 && thumb[2] >= 30.0 && thumb[2] < track[2], "{thumb:?} in {track:?}");
    // dragged by its middle it moves with the pointer, and the view with it
    let y = thumb[1] + thumb[3] / 2.0;
    let grab = thumb[0] + thumb[2] / 2.0;
    d.ok("ui.drag", json!({"from": {"x": grab, "y": y}, "to": {"x": grab + 300.0, "y": y}, "steps": 10}));
    d.frames(3);
    let moved = d.rect("timeline.zoomBar");
    assert!((moved[0] - thumb[0] - 300.0).abs() < 2.0, "the thumb went {} points for a 300 point drag", moved[0] - thumb[0]);
    let travel = (track[2] - thumb[2]) as f64;
    assert!((d.view().0 - 300.0 / travel * limit).abs() < limit * 0.01, "{}", d.view().0);
    assert_eq!(d.view().1, 50.0, "a drag by the middle does not zoom");
    // dragged past the right end it stops there: the view is at its limit
    let grab = moved[0] + moved[2] / 2.0;
    d.ok("ui.drag", json!({"from": {"x": grab, "y": y}, "to": {"x": track[0] + track[2] + 400.0, "y": y}, "steps": 10}));
    d.frames(3);
    let right = d.rect("timeline.zoomBar");
    assert!((right[0] + right[2] - (track[0] + track[2])).abs() < 0.5, "{right:?}");
    assert!((d.view().0 - limit).abs() < 1e-6);
    // and from the right end it comes back: dragged left it leaves the end and follows again
    let grab = right[0] + right[2] / 2.0;
    d.ok("ui.drag", json!({"from": {"x": grab, "y": y}, "to": {"x": grab - 250.0, "y": y}, "steps": 10}));
    d.frames(3);
    let back = d.rect("timeline.zoomBar");
    assert!((right[0] - back[0] - 250.0).abs() < 2.0, "{back:?} from {right:?}");
    // all the way left: the start of the sequence
    let grab = back[0] + back[2] / 2.0;
    d.ok("ui.drag", json!({"from": {"x": grab, "y": y}, "to": {"x": track[0] - 500.0, "y": y}, "steps": 10}));
    d.frames(3);
    assert_eq!(d.view().0, 0.0);
    assert!((d.rect("timeline.zoomBar")[0] - track[0]).abs() < 0.5);
    // a click on the bar beside the thumb brings the thumb there
    let target = track[0] + track[2] * 0.6;
    d.ok("ui.click", json!({"x": target, "y": y}));
    d.frames(3);
    let jumped = d.rect("timeline.zoomBar");
    assert!((jumped[0] + jumped[2] / 2.0 - target).abs() < 2.0, "{jumped:?}");
    // the handles at its ends still zoom: the right one out, the left edge of the view staying
    let at = d.view().0;
    d.ok("ui.drag", json!({"from": {"x": jumped[0] + jumped[2] - 3.0, "y": y}, "to": {"x": jumped[0] + jumped[2] + 60.0, "y": y}, "steps": 8}));
    d.frames(3);
    let (scroll, pps, ..) = d.view();
    assert!(pps < 50.0 && (scroll - at).abs() < 1e-6, "{pps} {scroll}");
}

#[test]
fn zoom_bar_remains_interactive_when_the_minimap_is_disabled() {
    let mut d = Driver::tall();
    d.app().session.state.timeline_ui.slider_minimap = false;
    d.frames(2);

    let (thumb, _track, y) = zoom_bar_at_start(&mut d);
    let start = d.view().0;
    d.ok(
        "ui.drag",
        json!({
            "from": {"x": thumb[0] + thumb[2] / 2.0, "y": y},
            "to": {"x": thumb[0] + thumb[2] / 2.0 + 80.0, "y": y},
            "steps": 8
        }),
    );
    d.frames(2);

    assert!(d.view().0 > start, "the zoom bar still pans the Timeline without minimap layers");
}

/// The zoom bar with the view at the start: the thumb at the left end of the track. Returns the
/// thumb (x, y, width, height), the track, and the y of the bar's middle.
fn zoom_bar_at_start(d: &mut Driver) -> ([f32; 4], [f32; 4], f32) {
    d.ok("ui.set", json!({"timeline": {"scroll": 0.0}}));
    d.frames(3);
    let (thumb, track) = (d.rect("timeline.zoomBar"), d.rect("timeline.zoomBar.track"));
    (thumb, track, thumb[1] + thumb[3] / 2.0)
}

/// The zoom bar's circular end handles are clickable controls; hover shows a hand. The middle of
/// the thumb scrolls and the bar beside it jumps, which keep the normal pointer.
#[test]
fn zoom_bar_end_handles_show_a_pointer_cursor_on_hover() {
    let mut d = Driver::tall();
    let (thumb, track, y) = zoom_bar_at_start(&mut d);
    d.move_to(thumb[0] + 3.0, y);
    assert_eq!(d.cursor(), CursorIcon::PointingHand, "left handle");
    d.move_to(thumb[0] + thumb[2] - 3.0, y);
    assert_eq!(d.cursor(), CursorIcon::PointingHand, "right handle");
    d.move_to(thumb[0] + thumb[2] / 2.0, y);
    assert_eq!(d.cursor(), CursorIcon::Default, "middle of the thumb");
    let beside = track[0] + track[2] - 20.0;
    assert!(beside > thumb[0] + thumb[2] + 9.0, "the probe is on the bar beside the thumb: {thumb:?} {track:?}");
    d.move_to(beside, y);
    assert_eq!(d.cursor(), CursorIcon::Default, "the bar beside the thumb");
}

#[test]
fn visible_zoom_bar_controls_capture_a_shortcut_and_close() {
    let mut d = Driver::demo();
    d.ok("ui.click", json!({"id": "timeline.zoomBar", "button": "right"}));
    d.frames(3);

    let controls = d.rect("timeline.zoomBar.settings.controls");
    d.ok("ui.move", json!({"x": controls[0] + controls[2] / 2.0, "y": controls[1] + controls[3] / 2.0}));
    d.frames(2);
    d.rect("timeline.zoomBar.settings.controlsFlyout.box0");
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.box0"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "F9"}));
    d.frames(2);
    assert_eq!(d.app().session.state.timeline_ui.control_center_on_playhead, "F9", "the visible Controls flyout captures the key");

    d.ok("ui.click", json!({"x": 1550.0, "y": 40.0}));
    d.frames(2);
    let settings = d.ok("ui.elements", json!({"prefix": "timeline.zoomBar.settings.controls"}));
    assert!(settings.as_array().is_some_and(Vec::is_empty), "clicking outside closes the visible zoom-bar popup");
}

#[test]
fn each_conflicting_controls_binding_shows_its_own_warning() {
    let mut d = Driver::demo();
    d.app().session.shortcuts.set_gesture("timeline.sliderExplode", "Double Click").expect("assign the conflicting gesture");
    d.app().session.state.timeline_ui.control_explode_slider = "Double Click".into();

    d.ok("ui.click", json!({"id": "timeline.zoomBar", "button": "right"}));
    d.frames(3);
    let controls = d.rect("timeline.zoomBar.settings.controls");
    d.ok("ui.move", json!({"x": controls[0] + controls[2] / 2.0, "y": controls[1] + controls[3] / 2.0}));
    d.frames(3);

    d.rect("timeline.zoomBar.settings.controlsFlyout.conflict0");
    d.rect("timeline.zoomBar.settings.controlsFlyout.conflict1");
}

/// A handle drag goes on whatever the pointer is over: it zooms from anywhere, so the arrow stays
/// while the pointer is off the bar, over the tracks, and goes when the button is released.
#[test]
fn zoom_bar_handle_drag_keeps_the_resize_cursor_off_the_bar() {
    let mut d = Driver::tall();
    let (thumb, _, y) = zoom_bar_at_start(&mut d);
    let handle = thumb[0] + thumb[2] - 3.0;
    let before = d.app().ui.timeline.target_pps;
    d.press(handle, y);
    d.move_to(handle + 40.0, y);
    let after = d.app().ui.timeline.target_pps;
    assert!(after < before, "the right handle dragged right zooms out: {before} -> {after}");
    assert_eq!(d.cursor(), CursorIcon::ResizeHorizontal, "dragging along the bar");
    d.move_to(handle + 40.0, y - 300.0);
    assert_eq!(d.cursor(), CursorIcon::ResizeHorizontal, "dragging over the tracks");
    d.release(handle + 40.0, y - 300.0);
    assert_ne!(d.cursor(), CursorIcon::ResizeHorizontal, "button released, pointer over the tracks");
}

/// Dragging the thumb by its middle scrolls: with the thumb stopped at the end of the bar the
/// pointer can sit where a handle is, and must not show the zoom arrow.
#[test]
fn zoom_bar_middle_drag_never_shows_the_resize_cursor() {
    let mut d = Driver::tall();
    let (thumb, track, y) = zoom_bar_at_start(&mut d);
    d.press(thumb[0] + thumb[2] / 2.0, y);
    d.move_to(thumb[0] + thumb[2] / 2.0 + 60.0, y);
    d.move_to(track[0] + track[2] - 3.0, y);
    let end = d.rect("timeline.zoomBar");
    assert!((end[0] + end[2] - (track[0] + track[2])).abs() < 0.5, "the thumb is at the end of the bar: {end:?}");
    assert_eq!(d.cursor(), CursorIcon::Default, "pointer over the right handle's place");
    d.release(track[0] + track[2] - 3.0, y);
}

#[test]
fn pending_zoom_setting_change_flushes_after_its_popup_stops_drawing() {
    let mut d = Driver::demo();
    let before = d.app().session.view_epoch;
    let key = egui::Id::unique("tl-zoombar-view-pending");
    d.harness.ctx.data_mut(|data| data.insert_temp(key, true));
    let pos = egui::pos2(30.0, 30.0);
    d.harness.input_mut().events.extend([
        egui::Event::PointerMoved(pos),
        egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE },
    ]);
    d.frames(1);
    assert_eq!(d.app().session.view_epoch, before, "the setting is not committed before its drag ends");
    assert_eq!(d.harness.ctx.data(|data| data.get_temp::<bool>(key)), Some(true));

    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(1);
    assert_eq!(d.app().session.view_epoch, before + 1, "the app UI pass commits the pending view change even when the settings popup is not drawn");
    assert_eq!(d.harness.ctx.data(|data| data.get_temp::<bool>(key)), None);
}

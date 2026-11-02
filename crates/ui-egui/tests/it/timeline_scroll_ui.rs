//! Headless UI tests of scrolling the Timeline: what the wheel does with each modifier, how far
//! the view can go, and the zoom scroll bar. Premiere Pro 26.5.2 on macOS, checked in the app: the
//! wheel moves the tracks up and down, Cmd + wheel moves the Timeline sideways, Option + wheel
//! zooms, and the view stops ten minutes past the end of the sequence.

use std::sync::mpsc::{Sender, channel};

use egui::CursorIcon;
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::panels::timeline::{max_scroll, timeline_extent};
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    /// Where the pointer was last put by `move_to` or `press`.
    pointer: egui::Pos2,
}

impl Driver {
    fn demo() -> Self {
        Self::sized(1600.0, 980.0)
    }

    fn sized(w: f32, h: f32) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(w, h)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, pointer: egui::Pos2::ZERO };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let v = self.call(method, params.clone());
        assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
        v["result"].clone()
    }

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
}

impl Driver {
    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn rect(&mut self, id: &str) -> [f32; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r = e["rect"].as_array().unwrap();
        [0, 1, 2, 3].map(|k| r[k].as_f64().unwrap() as f32)
    }

    /// Whether an element with this exact id is registered (drawn) this frame.
    fn has(&mut self, id: &str) -> bool {
        self.ok("ui.elements", json!({"prefix": id})).as_array().is_some_and(|a| a.iter().any(|e| e["id"] == json!(id)))
    }

    /// A click at a raw point, with its button (`"left"` / `"right"`).
    fn click_at(&mut self, x: f32, y: f32, button: &str) {
        self.ok("ui.click", json!({"x": x, "y": y, "button": button}));
        self.frames(2);
    }

    /// Turn the wheel over the tracks (`fy`: 0.2 is among the video tracks, 0.8 among the audio
    /// tracks), with modifiers.
    fn wheel(&mut self, fy: f64, dx: f64, dy: f64, modifiers: Value) {
        self.ok("ui.scroll", json!({"id": "timeline.tracks", "fx": 0.5, "fy": fy, "dx": dx, "dy": dy, "modifiers": modifiers}));
        self.frames(3);
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

fn none() -> Value {
    json!({})
}

#[test]
fn ctrl_wheel_moves_the_tracks_up_and_down() {
    let mut d = Driver::tall();
    // over the video tracks: Ctrl + wheeling up brings the higher tracks in (Video 1 is at the
    // bottom); the plain wheel belongs to the Timeline now
    d.wheel(0.2, 0.0, 40.0, json!({"command": true}));
    assert_eq!(d.view(), (4.0, 50.0, 40.0, 0.0), "the video tracks moved, nothing else");
    d.wheel(0.2, 0.0, -25.0, json!({"command": true}));
    assert_eq!(d.view(), (4.0, 50.0, 15.0, 0.0));
    // over the audio tracks: wheeling down brings the lower tracks in
    d.wheel(0.8, 0.0, -30.0, json!({"command": true}));
    assert_eq!(d.view(), (4.0, 50.0, 15.0, 30.0));
    // neither half goes past its first or last track
    d.wheel(0.2, 0.0, -1e6, json!({"command": true}));
    d.wheel(0.8, 0.0, 1e6, json!({"command": true}));
    assert_eq!(d.view(), (4.0, 50.0, 0.0, 0.0));
    d.wheel(0.8, 0.0, -1e6, json!({"command": true}));
    let (_, _, _, lowest) = d.view();
    assert!(lowest > 100.0 && lowest < 15.0 * 200.0, "{lowest}");
}

#[test]
fn the_wheel_moves_the_timeline_sideways_and_option_wheel_zooms() {
    let mut d = Driver::tall();
    // The wheel: sideways, a point of wheel for a point of Timeline; the tracks stay
    d.wheel(0.2, 0.0, -100.0, none());
    assert_eq!(d.view(), (6.0, 50.0, 0.0, 0.0), "100 points at 50 points a second: 2 s later");
    d.wheel(0.8, 0.0, 250.0, none());
    assert_eq!(d.view(), (1.0, 50.0, 0.0, 0.0));
    d.wheel(0.8, 0.0, 1e5, none());
    assert_eq!(d.view().0, 0.0, "not before the start");
    // Option + wheel: zoom about the pointer; it is not what the wheel does
    d.ok("ui.set", json!({"timeline": {"scroll": 4.0}}));
    d.frames(3);
    let tracks = d.rect("timeline.tracks");
    let under_pointer = 4.0 + (tracks[2] as f64 * 0.5) / 50.0;
    d.wheel(0.2, 0.0, 20.0, json!({"alt": true}));
    let (scroll, pps, v, a) = d.view();
    assert!((pps - 60.0).abs() < 1e-6, "zoomed in by a fifth: {pps}");
    assert!((scroll + (tracks[2] as f64 * 0.5) / pps - under_pointer).abs() < 0.02, "the time under the pointer stays there: {scroll}");
    assert_eq!((v, a), (0.0, 0.0));
    d.wheel(0.2, 0.0, -20.0, json!({"alt": true}));
    assert!((d.view().1 - 48.0).abs() < 1e-6, "and out again");
    // a sideways gesture (trackpad swipe, tilt wheel) is sideways whatever is held; Shift is the
    // wheel a lot faster
    d.ok("ui.set", json!({"timeline": {"pps": 50.0, "scroll": 4.0}}));
    d.frames(3);
    d.wheel(0.2, -50.0, 5.0, none());
    assert_eq!(d.view(), (5.0, 50.0, 0.0, 0.0));
    d.wheel(0.2, 0.0, -50.0, json!({"shift": true}));
    assert_eq!(d.view(), (15.0, 50.0, 0.0, 0.0), "Shift + wheel is the same ten times over");
}

/// Settings ▸ Timeline ▸ Timeline Mouse Scrolling "Horizontal": the wheel moves the Timeline
/// sideways and Cmd + wheel moves the tracks.
#[test]
fn the_mouse_scrolling_setting_swaps_wheel_and_cmd_wheel() {
    let mut d = Driver::tall();
    assert_eq!(d.app().session.prefs.timeline.mouse_scrolling, "vertical", "Premiere's default on macOS");
    // the setting swaps the wheel and Ctrl + wheel: the wheel moves the tracks now
    d.app().session.prefs.timeline.mouse_scrolling = "horizontal".into();
    d.wheel(0.2, 0.0, 40.0, none());
    assert_eq!(d.view(), (4.0, 50.0, 40.0, 0.0), "swapped: the wheel moves the tracks");
    d.wheel(0.2, 0.0, -100.0, json!({"command": true}));
    assert_eq!(d.view(), (6.0, 50.0, 40.0, 0.0), "and Ctrl + wheel the Timeline");
    d.wheel(0.2, 0.0, 20.0, json!({"alt": true}));
    assert!((d.view().1 - 60.0).abs() < 1e-6, "Option + wheel zooms either way");
}

/// The view cannot leave the Timeline: scrolled 39 minutes past a 26 second sequence, nothing was
/// in view and the scroll bar's thumb sat at the right end whatever was done to it.
#[test]
fn the_view_stops_ten_minutes_past_the_end_of_the_sequence() {
    let mut d = Driver::tall();
    let seconds = d.seconds();
    let width = d.rect("timeline.tracks")[2];
    assert_eq!(timeline_extent(seconds), seconds + 600.0);
    let limit = max_scroll(seconds, width, 50.0);
    assert!((limit - (seconds + 600.0 - width as f64 / 50.0)).abs() < 1e-9);
    // by the wheel
    d.wheel(0.2, 0.0, -1e7, none());
    assert!((d.view().0 - limit).abs() < 1e-6, "{} vs {limit}", d.view().0);
    d.wheel(0.2, 0.0, -500.0, none());
    assert!((d.view().0 - limit).abs() < 1e-6);
    // by anything else that moves the view (a script here: the state that was reported stuck)
    d.ok("ui.set", json!({"timeline": {"pps": 2.9863201066269047, "scroll": 2325.538505030118}}));
    d.frames(4);
    let (scroll, pps, ..) = d.view();
    assert!((scroll - max_scroll(seconds, width, pps)).abs() < 1e-6 && scroll < 400.0, "{scroll}");
    // zoomed out until the whole Timeline is in view, there is nowhere to scroll to
    d.ok("ui.set", json!({"timeline": {"pps": 0.5, "scroll": 50.0}}));
    d.frames(4);
    assert_eq!(d.view().0, 0.0);
    // numbers that are not numbers do no harm
    d.app().ui.timeline.target_scroll = f64::NAN;
    d.app().ui.timeline.scroll = f64::INFINITY;
    d.frames(3);
    assert_eq!(d.view().0, 0.0);
}

/// Zooming out stops where the whole extent (the sequence and ten minutes) is in the view, however
/// far the wheel or a script asks.
#[test]
fn zooming_out_stops_where_the_whole_timeline_is_in_view() {
    let mut d = Driver::tall();
    let seconds = d.seconds();
    let width = d.rect("timeline.tracks")[2] as f64;
    let floor = width / timeline_extent(seconds);
    for _ in 0..20 {
        d.wheel(0.2, 0.0, -200.0, json!({"alt": true}));
    }
    let (scroll, pps, ..) = d.view();
    assert!((pps - floor).abs() < floor * 0.01, "the wheel zoomed to {pps}, the whole extent is {floor}");
    assert_eq!(scroll, 0.0);
    // a script's zoom is held to the same limit
    d.ok("ui.set", json!({"timeline": {"pps": 0.05, "scroll": 0.0}}));
    d.frames(6);
    let (_, pps, ..) = d.view();
    assert!((pps - floor).abs() < floor * 0.01, "a script zoomed to {pps}");
    // and zooming back in works from there
    d.wheel(0.2, 0.0, 20.0, json!({"alt": true}));
    assert!(d.view().1 > pps * 1.1);
}

/// A sequence longer than twenty view widths in seconds (the view is about a thousand points here) still zooms out until all of it is in view.
#[test]
fn a_twelve_hour_sequence_zooms_out_until_it_is_all_in_view() {
    let mut d = Driver::tall();
    let (item, duration) = {
        let it = d.app().session.active_sequence().expect("sequence").video_tracks[0].items.first().expect("a clip");
        (it.item.0, it.duration.0)
    };
    let twelve_hours = 12 * 3600 * 254_016_000_000_i64;
    d.exec("timeline.place", json!({"item": item, "track": "V2", "time": twelve_hours, "sourceIn": 0, "duration": duration, "audio": false}));
    d.frames(4);
    let width = d.rect("timeline.tracks")[2] as f64;
    let floor = width / timeline_extent(d.seconds());
    assert!(floor < 0.05, "the test needs a floor under 0.05, got {floor}");
    for _ in 0..40 {
        d.wheel(0.2, 0.0, -200.0, json!({"alt": true}));
    }
    let (scroll, pps, ..) = d.view();
    assert!((pps - floor).abs() < floor * 0.01, "the wheel zoomed to {pps}, the whole extent is {floor}");
    assert_eq!(scroll, 0.0);
    // the Zoom Out command stops at the same place
    for _ in 0..40 {
        d.exec("view.zoomOut", json!({}));
        d.frames(2);
    }
    let (_, pps, ..) = d.view();
    assert!((pps - floor).abs() < floor * 0.01, "view.zoomOut zoomed to {pps}, the whole extent is {floor}");
}

#[test]
fn the_scroll_bar_thumb_follows_the_pointer_and_reaches_both_ends() {
    let mut d = Driver::tall();
    let seconds = d.seconds();
    let track = d.rect("timeline.zoomBar.track");
    let limit = max_scroll(seconds, d.rect("timeline.tracks")[2], 50.0);
    // at the start the thumb is at the left end of the bar, and wide enough to grab: the log-mapped
    // zoom meter is about half the bar for a quarter of a 10-minute Timeline at this zoom, but the
    // invariant that matters is grabbable and well short of the full extent
    d.ok("ui.set", json!({"timeline": {"scroll": 0.0}}));
    d.frames(3);
    let thumb = d.rect("timeline.zoomBar");
    assert!((thumb[0] - track[0]).abs() < 0.5 && thumb[2] >= 30.0 && thumb[2] < track[2] * 0.9, "{thumb:?} in {track:?}");
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
    // the handles at its ends still zoom: the right one outward makes the thumb grow on both edges
    // (mirrored about the middle), zooming out with the footage at the view's center staying put
    let (at, at_pps) = (d.view().0, d.view().1);
    let width = f64::from(d.rect("timeline.tracks")[2]);
    let center_old = at + width / (2.0 * at_pps);
    d.ok("ui.drag", json!({"from": {"x": jumped[0] + jumped[2] - 3.0, "y": y}, "to": {"x": jumped[0] + jumped[2] + 60.0, "y": y}, "steps": 8}));
    d.frames(3);
    let (scroll, pps, ..) = d.view();
    assert!(pps < 50.0, "dragging the right handle outward zooms out: {pps}");
    let center_new = scroll + width / (2.0 * pps);
    assert!((center_new - center_old).abs() < 3.0, "the view's center holds: {center_new} was {center_old} (scroll {scroll}, was {at})");
}

#[test]
fn timeline_zoom_settings_default_off_and_set_custom_playhead_range() {
    let mut d = Driver::demo();
    assert!(!d.app().session.state.timeline_ui.smooth_animations);
    assert_eq!(d.app().session.state.timeline_ui.shift_drag_speed, 1.0);
    assert!(!d.app().session.state.timeline_ui.alternate_slider_position);
    d.ok("ui.set", json!({"timeline": {"smoothAnimations": true, "playheadSideSeconds": 17.5, "shiftDragSpeed": 2.5, "zoomBarHidden": true, "alternateSliderPosition": true}}));
    assert!(d.app().session.state.timeline_ui.smooth_animations);
    assert_eq!(d.app().session.state.timeline_ui.playhead_side_seconds, 17.5);
    assert_eq!(d.app().session.state.timeline_ui.shift_drag_speed, 2.5);
    assert!(d.app().session.state.timeline_ui.zoom_bar_hidden);
    assert!(d.app().session.state.timeline_ui.alternate_slider_position);
    d.ok("ui.set", json!({"timeline": {"zoomBarHidden": false, "alternateSliderPosition": false}}));
    assert!(!d.app().session.state.timeline_ui.zoom_bar_hidden);
    assert!(!d.app().session.state.timeline_ui.alternate_slider_position);
}

/// The zoom bar with the view at the start: the thumb at the left end of the track. Returns the
/// thumb (x, y, width, height), the track, and the y of the bar's middle.
fn zoom_bar_at_start(d: &mut Driver) -> ([f32; 4], [f32; 4], f32) {
    d.ok("ui.set", json!({"timeline": {"scroll": 0.0}}));
    d.frames(3);
    let (thumb, track) = (d.rect("timeline.zoomBar"), d.rect("timeline.zoomBar.track"));
    (thumb, track, thumb[1] + thumb[3] / 2.0)
}

/// The zoom bar's circular end handles zoom, so the pointer over one shows the horizontal resize
/// arrow; the middle of the thumb scrolls and the bar beside it jumps, which keep the normal pointer.
#[test]
fn zoom_bar_handles_show_the_resize_cursor_on_hover() {
    let mut d = Driver::tall();
    let (thumb, track, y) = zoom_bar_at_start(&mut d);
    d.move_to(thumb[0] + 3.0, y);
    assert_eq!(d.cursor(), CursorIcon::ResizeHorizontal, "left handle");
    d.move_to(thumb[0] + thumb[2] - 3.0, y);
    assert_eq!(d.cursor(), CursorIcon::ResizeHorizontal, "right handle");
    d.move_to(thumb[0] + thumb[2] / 2.0, y);
    assert_eq!(d.cursor(), CursorIcon::Default, "middle of the thumb");
    let beside = track[0] + track[2] - 20.0;
    assert!(beside > thumb[0] + thumb[2] + 9.0, "the probe is on the bar beside the thumb: {thumb:?} {track:?}");
    d.move_to(beside, y);
    assert_eq!(d.cursor(), CursorIcon::Default, "the bar beside the thumb");
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

/// Show Video Thumbnails ▸ Continuous asks for frames across each video clip, not only its head;
/// `ui.set` switches the mode (and Off) and refuses an unknown one.
#[test]
fn continuous_video_thumbnails_ask_for_frames_across_each_clip() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"timeline": {"thumbnails": "head", "pps": 50.0, "scroll": 0.0}}));
    d.view();
    d.frames(10);
    let head = d.app().frames.stats().request_misses;
    d.ok("ui.set", json!({"timeline": {"thumbnails": "continuous"}}));
    d.frames(3);
    let continuous = d.app().frames.stats().request_misses;
    assert!(continuous >= head + 5, "continuous thumbnails queued {} more frames", continuous - head);
    let tl = d.ok("ui.inspect", none())["ui"]["timeline"].clone();
    assert_eq!((tl["show_thumbnails"].clone(), tl["thumbnail_mode"].clone()), (json!(true), json!("Continuous")));
    d.ok("ui.set", json!({"timeline": {"thumbnails": "off"}}));
    assert!(!d.app().ui.timeline.show_thumbnails);
    let v = d.call("ui.set", json!({"timeline": {"thumbnails": "filmstrip"}}));
    assert_eq!(v["ok"], json!(false), "{v}");
    assert!(!d.app().ui.timeline.show_thumbnails);
}

#[test]
fn home_and_end_bring_the_playhead_into_view() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"timeline": {"pps": 400.0, "scroll": 3.0}}));
    d.frames(3);
    d.exec("playhead.start", json!({}));
    d.frames(3);
    assert_eq!(d.app().ui.timeline.target_scroll, 0.0, "Home scrolls back to the start");
    d.exec("playhead.end", json!({}));
    d.frames(3);
    let end = d.app().session.playhead().seconds();
    let v = &d.app().ui.timeline;
    let width = 1600.0 / v.pps;
    assert!(v.target_scroll > 3.0 && v.target_scroll <= end && end - v.target_scroll < width, "End shows the playhead: scroll {} end {end}", v.target_scroll);
}

fn repeated_endpoint_navigation_reveals_the_same_playhead(use_keys: bool) {
    let mut d = Driver::demo();
    let project = d.app().session.project.clone();
    let undo = d.app().session.history.undo.len();
    for (command, key, manual_scroll) in [("playhead.start", "Home", 5.0), ("playhead.end", "End", 0.0)] {
        d.exec(command, json!({}));
        d.frames(3);
        let playhead = d.app().session.playhead();
        d.ok("ui.set", json!({"timeline": {"pps": 400.0, "scroll": manual_scroll}}));
        d.frames(3);
        assert_eq!(d.view().0, manual_scroll, "manual browsing is allowed while stopped");
        if use_keys {
            d.ok("ui.key", json!({"key": key}));
        } else {
            d.exec(command, json!({}));
        }
        d.frames(3);
        let (scroll, pps, ..) = d.view();
        let x = (playhead.seconds() - scroll) * pps;
        let width = f64::from(d.app().last_timeline_width);
        assert!((0.0..=width).contains(&x), "{key} reveals an unchanged playhead: x={x}, width={width}, scroll={scroll}");
        assert_eq!(d.app().session.playhead(), playhead);
        assert!(std::sync::Arc::ptr_eq(&project, &d.app().session.project));
        assert_eq!(d.app().session.history.undo.len(), undo);
        // An explicit navigation must not turn later manual browsing into continuous following.
        d.ok("ui.set", json!({"timeline": {"scroll": manual_scroll}}));
        d.frames(3);
        assert_eq!(d.view().0, manual_scroll);
    }
}

#[test]
fn repeated_home_and_end_keys_reveal_a_playhead_already_at_the_endpoint() {
    repeated_endpoint_navigation_reveals_the_same_playhead(true);
}

#[test]
fn repeated_start_and_end_commands_reveal_a_playhead_already_at_the_endpoint() {
    repeated_endpoint_navigation_reveals_the_same_playhead(false);
}

/// #2325: Settings ▸ Appearance ▸ "Animate zoom, scrolling and menus" off makes a zoom and a
/// scroll land in one frame, and egui's fades (menus, collapsing sections, tooltips) instant.
#[test]
fn turning_animations_off_zooms_and_scrolls_in_one_step() {
    let mut d = Driver::tall();
    let animation_time = |d: &mut Driver| d.harness.ctx.global_style().animation_time;
    assert!(animation_time(&mut d) > 0.0, "animated by default");
    // animated: one frame later the zoom is on its way, not there
    let v = &mut d.app().ui.timeline;
    v.target_pps = 200.0;
    v.target_scroll = 8.0;
    d.frames(1);
    let v = &d.app().ui.timeline;
    assert!(v.pps > 50.0 && v.pps < 200.0 && v.scroll < 8.0, "eased: {} {}", v.pps, v.scroll);
    assert_eq!(d.view(), (8.0, 200.0, 0.0, 0.0));
    d.exec("prefs.set", json!({"key": "appearance.animations", "value": false}));
    d.frames(2);
    assert_eq!(animation_time(&mut d), 0.0);
    let v = &mut d.app().ui.timeline;
    v.target_pps = 50.0;
    v.target_scroll = 3.0;
    d.frames(1);
    let v = &d.app().ui.timeline;
    assert_eq!((v.pps, v.scroll), (50.0, 3.0), "in one step");
    // and back on
    d.exec("prefs.set", json!({"key": "appearance.animations", "value": true}));
    d.frames(2);
    assert!(animation_time(&mut d) > 0.0);
}

/// Hide Slider: pressing it hides the bar at once and takes the settings popup with it (the
/// window left behind only explains how to bring the bar back), the tracks fill its space, and
/// the keyboard shortcuts still move the Timeline without the bar.
#[test]
fn hiding_the_slider_frees_its_space_and_only_then_offers_unhide() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    let tracks = d.rect("timeline.tracks");
    assert!(tracks[1] + tracks[3] < bar[1], "the bar sits under the tracks: {tracks:?} {bar:?}");
    let centre = |r: [f32; 4]| (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let mid = centre(tracks);
    // no Unhide Timeline Slider while the slider is showing
    d.click_at(mid.0, mid.1, "right");
    assert!(!d.has("timeline.unhideZoomBar"), "no Unhide Timeline Slider while the bar shows");
    d.click_at(tracks[0] + 30.0, tracks[1] + 15.0, "left");
    // Hide Slider from the bar's own settings popup: the bar and the popup go at once, and the
    // window that is left only explains how to bring the bar back
    let bar_mid = centre(bar);
    d.click_at(bar_mid.0, bar_mid.1, "right");
    assert!(d.has("timeline.zoomBar.settings.hideSlider"), "the bar's settings popup");
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.hideSlider"}));
    d.frames(2);
    assert!(d.app().session.state.timeline_ui.zoom_bar_hidden, "hidden the moment Hide Slider is pressed");
    assert!(!d.has("timeline.zoomBar.track"), "the hidden bar draws nothing at all");
    assert!(!d.has("timeline.zoomBar.settings.smoothAnimations"), "the settings popup is gone at once");
    assert!(d.has("timeline.zoomBar.hideDialog.ok"), "the window explaining how to bring it back");
    d.ok("ui.click", json!({"id": "timeline.zoomBar.hideDialog.ok"}));
    d.frames(2);
    assert!(!d.has("timeline.zoomBar.hideDialog.ok"), "OK only closes that window");
    let grown = d.rect("timeline.tracks");
    assert!((grown[1] + grown[3] - tracks[1] - tracks[3] - 17.0).abs() < 1.0, "the tracks fill the bar's space: {tracks:?} -> {grown:?}");
    // the keyboard shortcuts still move the Timeline with the slider hidden
    d.ok("ui.set", json!({"timeline": {"pps": 400.0, "scroll": 3.0}}));
    d.frames(3);
    d.ok("ui.key", json!({"key": "Home"}));
    d.frames(3);
    assert_eq!(d.app().ui.timeline.target_scroll, 0.0, "Home scrolls back to the start without the bar");
    d.ok("ui.key", json!({"key": "End"}));
    d.frames(3);
    let end = d.app().session.playhead().seconds();
    let v = &d.app().ui.timeline;
    let width = 1600.0 / v.pps;
    assert!(v.target_scroll > 0.0 && v.target_scroll <= end && end - v.target_scroll < width, "End shows the playhead without the bar");
    // Unhide Timeline Slider brings it back (its menu behaviour has its own test)
    d.ok("ui.set", json!({"timeline": {"zoomBarHidden": false}}));
    d.frames(3);
    assert!(!d.app().session.state.timeline_ui.zoom_bar_hidden);
    assert!(d.has("timeline.zoomBar.track"), "the bar is back");
    let back = d.rect("timeline.tracks");
    assert!((back[1] + back[3] - tracks[1] - tracks[3]).abs() < 1.0, "the tracks make room again: {back:?}");
    d.click_at(mid.0, mid.1, "right");
    assert!(!d.has("timeline.unhideZoomBar"), "gone again while the bar shows");
}

/// Alternate Slider Position: the bar moves to the top edge, just above the ruler, the same
/// length horizontally, and the ruler and tracks move down by its height.
#[test]
fn alternate_slider_position_moves_the_bar_just_above_the_ruler() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    let tracks = d.rect("timeline.tracks");
    let ruler = d.rect("timeline.ruler");
    assert!(bar[1] > tracks[1] + tracks[3], "normally the bar is at the bottom: {bar:?}");
    // a pill switch in the bar's settings popup, like Smooth Animations
    let bar_mid = (bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0);
    d.click_at(bar_mid.0, bar_mid.1, "right");
    assert!(d.has("timeline.zoomBar.settings.alternateSliderPosition"), "the pill in the settings popup");
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.alternateSliderPosition"}));
    d.frames(3);
    assert!(d.app().session.state.timeline_ui.alternate_slider_position, "on by pressing the pill");
    let top_bar = d.rect("timeline.zoomBar.track");
    let top_ruler = d.rect("timeline.ruler");
    let top_tracks = d.rect("timeline.tracks");
    assert!(top_bar[1] < top_ruler[1], "the bar is above the ruler: {top_bar:?} {top_ruler:?}");
    assert!(top_bar[1] < top_tracks[1], "the bar is above the tracks: {top_bar:?}");
    assert!((top_bar[0] - bar[0]).abs() < 0.5 && (top_bar[2] - bar[2]).abs() < 0.5, "the same horizontal length: {top_bar:?} was {bar:?}");
    assert!((top_ruler[1] - ruler[1] - 17.0).abs() < 1.0, "the ruler moved down 17: {top_ruler:?} was {ruler:?}");
    assert!((top_tracks[1] - tracks[1] - 17.0).abs() < 1.0, "the tracks moved down 17: {top_tracks:?}");
    assert!((top_ruler[1] + top_ruler[3] - top_tracks[1]).abs() < 1.0, "the ruler still meets the tracks: {top_ruler:?} {top_tracks:?}");
    // off again: everything is back where it was
    d.ok("ui.set", json!({"timeline": {"alternateSliderPosition": false}}));
    d.frames(3);
    assert!((d.rect("timeline.ruler")[1] - ruler[1]).abs() < 1.0, "the ruler is back");
    assert!((d.rect("timeline.tracks")[1] - tracks[1]).abs() < 1.0, "the tracks are back");
}

/// Unhide Timeline Slider is the right-click menu's list last row: on a screen where the menu
/// scrolls it is out of sight at the top and only comes into view at the bottom, with the list.
#[test]
fn unhide_is_the_menus_last_row_and_scrolls_with_it() {
    let mut d = Driver::sized(1600.0, 600.0);
    d.ok("ui.set", json!({"timeline": {"zoomBarHidden": true}}));
    d.frames(3);
    let els = d.ok("ui.elements", json!({"prefix": "timeline.clip."}));
    let clip = els
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["id"].as_str().is_some_and(|s| s.strip_prefix("timeline.clip.").is_some_and(|r| !r.is_empty() && r.chars().all(|c| c.is_ascii_digit()))))
        .expect("a clip on the timeline")["id"]
        .as_str()
        .unwrap()
        .to_string();
    d.ok("ui.click", json!({"id": clip, "button": "right"}));
    d.frames(3);
    assert!(d.has("timeline.unhideZoomBar"), "in the menu");
    let y0 = d.rect("timeline.unhideZoomBar")[1];
    assert!(y0 > 600.0, "not visible while the menu is scrolled to the top: {y0}");
    assert!(y0 > d.rect("timeline.clipMenu.sequence.joinThroughEdits")[1], "the list's last row");
    // it is a row of the list: scrolling the menu brings it up like every other row
    d.ok("ui.scroll", json!({"id": "timeline.clipMenu.edit.cut", "dy": -2000.0}));
    d.frames(5);
    let r = d.rect("timeline.unhideZoomBar");
    assert!(r[1] < y0 - 100.0, "it scrolls with the menu: {r:?} was at {y0}");
    assert!(r[1] >= 0.0 && r[1] + r[3] <= 600.0, "scrolled into view: {r:?}");
    d.ok("ui.click", json!({"id": "timeline.unhideZoomBar"}));
    d.frames(3);
    assert!(!d.app().session.state.timeline_ui.zoom_bar_hidden, "Unhide brings the bar back");
    assert!(d.has("timeline.zoomBar.track"), "the bar is back");
}

/// The Minimap Settings flyout must be reachable with the mouse: it opens on the Slider Minimap
/// row and STAYS open while the pointer walks onto it, and its settings can be pressed there.
/// (egui's `rect_contains_pointer` is layer-bound and used to kill the flyout the moment the
/// pointer left the row -- this walks the pointer there in small steps to prove it cannot.)
#[test]
fn the_minimap_flyout_stays_open_on_the_way_in_and_its_settings_click() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    assert!(d.has("timeline.zoomBar.settings.sliderMinimap"), "the bar's settings popup");
    let row = d.rect("timeline.zoomBar.settings.sliderMinimap");
    let from = (row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    d.ok("ui.move", json!({"x": from.0, "y": from.1}));
    d.frames(2);
    assert!(d.has("timeline.zoomBar.settings.minimapFlyout"), "the flyout opens on the row");
    let fly = d.rect("timeline.zoomBar.settings.minimapFlyout");
    let to = (fly[0] + fly[2] / 2.0, fly[1] + fly[3] / 2.0);
    for i in 1..=12 {
        let f = i as f32 / 12.0;
        d.ok("ui.move", json!({"x": f64::from(from.0 + (to.0 - from.0) * f), "y": f64::from(from.1 + (to.1 - from.1) * f)}));
        d.frames(2);
        assert!(d.has("timeline.zoomBar.settings.minimapFlyout"), "still open on the way in, step {i}");
    }
    assert!(d.has("timeline.zoomBar.settings.showWholeTimeline"), "the setting is on screen");
    let before = d.app().session.state.timeline_ui.show_whole_timeline;
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.showWholeTimeline"}));
    d.frames(2);
    assert_eq!(d.app().session.state.timeline_ui.show_whole_timeline, !before, "the pill presses while over the flyout");
    let before_fill = d.app().session.state.timeline_ui.fill_clips;
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.fillClips"}));
    d.frames(2);
    assert_eq!(d.app().session.state.timeline_ui.fill_clips, !before_fill, "Fill Clips presses too");
}

/// "Timeline Slider Settings": the Timeline menu's row above Unhide Timeline Slider, only while
/// the bar is hidden, opening the exact same settings as right-clicking the bar itself.
#[test]
fn timeline_slider_settings_is_the_menus_row_while_the_bar_is_hidden() {
    let mut d = Driver::demo();
    let tracks = d.rect("timeline.tracks");
    let mid = (tracks[0] + tracks[2] / 2.0, tracks[1] + tracks[3] / 2.0);
    d.click_at(mid.0, mid.1, "right");
    assert!(!d.has("timeline.zoomBarSettings"), "no Timeline Slider Settings while the bar shows");
    d.click_at(mid.0 + 200.0, mid.1, "left");
    d.ok("ui.set", json!({"timeline": {"zoomBarHidden": true}}));
    d.frames(2);
    d.click_at(mid.0, mid.1, "right");
    assert!(d.has("timeline.zoomBarSettings"), "the row is there while the bar is hidden");
    let settings = d.rect("timeline.zoomBarSettings");
    let unhide = d.rect("timeline.unhideZoomBar");
    assert!(settings[1] + settings[3] <= unhide[1] + 1.0, "above Unhide Timeline Slider: {settings:?} {unhide:?}");
    d.ok("ui.click", json!({"id": "timeline.zoomBarSettings"}));
    d.frames(2);
    assert!(d.has("timeline.zoomBar.settings.smoothAnimations"), "the same settings popup as right-clicking the bar");
    assert!(d.has("timeline.zoomBar.settings.maxZoomLevel"), "the same settings, all of them");
    // while hidden the top row is the blue Unhide Slider, never Hide Slider
    assert!(d.has("timeline.zoomBar.settings.unhideSlider"), "Unhide Slider while the bar is hidden");
    assert!(!d.has("timeline.zoomBar.settings.hideSlider"), "no Hide Slider there");
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.unhideSlider"}));
    d.frames(2);
    assert!(!d.app().session.state.timeline_ui.zoom_bar_hidden, "Unhide Slider brings the bar back");
}

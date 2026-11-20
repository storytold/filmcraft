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
        Self::sized_with_step(w, h, 0.25)
    }

    fn sized_with_step(w: f32, h: f32, step: f32) -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_step_dt(step).with_size(egui::vec2(w, h)).with_max_steps(10_000).build_eframe(move |_cc| app);
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

    fn rect_now(&self, id: &str) -> [f32; 4] {
        self.harness
            .state()
            .auto
            .elements
            .iter()
            .find(|element| element.id == id)
            .map(|element| element.rect)
            .unwrap_or_else(|| panic!("no current-frame element {id}"))
    }

    fn right_click_first_visible_frame(&mut self, x: f32, y: f32) {
        let pos = egui::pos2(x, y);
        let input = self.harness.input_mut();
        input.events.push(egui::Event::PointerMoved(pos));
        input.events.push(egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed: true, modifiers: egui::Modifiers::default() });
        self.frames(1);
        self.harness.input_mut().events.push(egui::Event::PointerButton {
            pos,
            button: egui::PointerButton::Secondary,
            pressed: false,
            modifiers: egui::Modifiers::default(),
        });
        self.frames(1);
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

fn track_view(d: &mut Driver, video: bool) -> (f32, f32) {
    let (content_top, content_bottom, split_y) = {
        let layout = d.app().tl.layout.as_ref().expect("timeline layout");
        (layout.content.min.y, layout.content.max.y, layout.split_y)
    };
    if video { (content_top, split_y) } else { (split_y + 5.0, content_bottom - 34.0) }
}

#[test]
fn timeline_middle_vertical_drag_controls_both_track_heights() {
    let mut d = Driver::tall();
    d.ok("ui.set", json!({"timeline": {"controlTracks": true, "videoTrackHeight": 100.0, "audioTrackHeight": 100.0}}));
    d.frames(3);
    let r = d.rect("timeline.zoomBar");
    let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.press(x, y);
    d.move_to(x, y - 12.0);
    let v = &d.app().ui.timeline;
    assert!(v.video_track_h > 100.0 && v.audio_track_h > 100.0, "{} {}", v.video_track_h, v.audio_track_h);
    let scroll = v.target_scroll;
    d.move_to(x + 20.0, y - 18.0);
    assert_eq!(d.app().ui.timeline.target_scroll, scroll, "track control cannot pan the timeline");
    d.release(x + 20.0, y - 18.0);
}

#[test]
fn shift_linked_resize_pins_the_cursor_and_keeps_resizing() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    d.ok("ui.set", json!({"timeline": {"controlTracks": true, "videoTrackHeight": 100.0, "audioTrackHeight": 100.0}}));
    d.frames(4);
    let r = d.rect("timeline.zoomBar");
    let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.press(x, y);
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    d.move_to(x, y - 24.0);
    assert!(d.app().tl.zoombar_cursor_lock.is_some(), "Shift-linked resize should pin and hide the cursor");
    let before = d.app().ui.timeline.video_track_h;
    d.move_to(x, y - 48.0);
    assert!(d.app().ui.timeline.video_track_h > before, "pinned vertical movement continues resizing");
    d.release(x, y - 48.0);
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    d.frames(1);
}

#[test]
fn right_button_alternate_drag_pins_and_moves_zoom_and_track_sliders() {
    let mut d = Driver::tall();
    d.app().session.shortcuts.set("timeline.sliderAlternate", "Right Click", None, false, true).expect("assign");
    d.app().session.shortcuts.set("timeline.trackSliderAlternate", "Right Click", None, false, true).expect("assign");
    d.frames(3);

    let bar = d.rect("timeline.zoomBar");
    let (x, y) = (bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0);
    d.press(x, y);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(x, y),
        button: egui::PointerButton::Secondary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(1);
    d.move_to(x + 30.0, y);
    assert!(d.app().tl.zoombar_cursor_lock.is_some(), "right-button alternate drag pins the Timeline slider cursor");
    assert!(d.app().ui.timeline.target_scroll > 4.0, "right-button alternate drag pans the Timeline");
    d.release(x + 30.0, y);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(x + 30.0, y),
        button: egui::PointerButton::Secondary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(2);

    let bar = d.rect("timeline.zoomBar");
    let (ex, ey) = (bar[0] + 6.0, bar[1] + bar[3] / 2.0);
    let old_pps = d.app().ui.timeline.target_pps;
    d.press(ex, ey);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(ex, ey),
        button: egui::PointerButton::Secondary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(1);
    d.move_to(ex - 30.0, ey);
    assert!(d.app().tl.zoombar_cursor_lock.is_some(), "right-button alternate edge drag pins the Timeline slider cursor");
    assert_ne!(d.app().ui.timeline.target_pps, old_pps, "right-button alternate edge drag zooms");
    d.release(ex - 30.0, ey);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(ex - 30.0, ey),
        button: egui::PointerButton::Secondary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(2);

    let thumb = d.rect("timeline.trackSlider.video");
    let (tx, ty) = (thumb[0] + thumb[2] / 2.0, thumb[1] + thumb[3] / 2.0);
    let old_vscroll = d.app().ui.timeline.v_scroll;
    d.press(tx, ty);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(tx, ty),
        button: egui::PointerButton::Secondary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(1);
    d.move_to(tx, ty - 30.0);
    assert!(d.app().tl.zoombar_cursor_lock.is_some(), "right-button alternate drag pins the Track slider cursor");
    assert_ne!(d.app().ui.timeline.v_scroll, old_vscroll, "right-button alternate drag scrolls the tracks");
    d.release(tx, ty - 30.0);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(tx, ty - 30.0),
        button: egui::PointerButton::Secondary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(2);

    let circle = d.rect("timeline.trackSlider.video.topCircle");
    let (ex, ey) = (circle[0] + circle[2] / 2.0, circle[1] + circle[3] / 2.0);
    let old_height = d.app().ui.timeline.video_track_h;
    d.press(ex, ey);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(ex, ey),
        button: egui::PointerButton::Secondary,
        pressed: true,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(1);
    d.move_to(ex, ey - 24.0);
    assert!(d.app().tl.zoombar_cursor_lock.is_some(), "right-button alternate edge drag pins the Track slider cursor");
    assert_ne!(d.app().ui.timeline.video_track_h, old_height, "right-button alternate edge drag zooms tracks");
    d.release(ex, ey - 24.0);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(ex, ey - 24.0),
        button: egui::PointerButton::Secondary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(2);
}

#[test]
fn shift_drag_with_right_button_down_keeps_the_slider_moving() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"timeline": {"pps": 100.0, "scroll": 10.0}}));
    d.frames(4);
    let r = d.rect("timeline.zoomBar");
    let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    d.press(x, y);
    d.harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::ShiftLeft,
        physical_key: Some(egui::Key::ShiftLeft),
        pressed: true,
        repeat: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(x, y),
        button: egui::PointerButton::Secondary,
        pressed: true,
        modifiers: egui::Modifiers::SHIFT,
    });
    d.frames(1);
    d.move_to(x + 36.0, y);
    assert!(d.app().ui.timeline.target_scroll > 10.0, "the Timeline slider should continue moving while Shift and right mouse are held");
    d.release(x + 36.0, y);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: egui::pos2(x + 36.0, y),
        button: egui::PointerButton::Secondary,
        pressed: false,
        modifiers: egui::Modifiers::SHIFT,
    });
    d.harness.input_mut().events.push(egui::Event::Key {
        key: egui::Key::ShiftLeft,
        physical_key: Some(egui::Key::ShiftLeft),
        pressed: false,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    });
    d.frames(2);
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
    // A sideways gesture is still time scroll. Shift+wheel now adjusts the hovered track height;
    // the legacy fast time-scroll gesture is available as an optional Controls binding.
    d.ok("ui.set", json!({"timeline": {"pps": 50.0, "scroll": 4.0}}));
    d.frames(3);
    d.wheel(0.2, -50.0, 5.0, none());
    assert_eq!(d.view(), (5.0, 50.0, 0.0, 0.0));
    let before_h = d.app().ui.timeline.video_track_h;
    let before_scroll = d.view().0;
    d.wheel(0.2, 0.0, -50.0, json!({"shift": true}));
    assert!(d.app().ui.timeline.video_track_h < before_h);
    assert_eq!(d.view().0, before_scroll, "default Shift+wheel adjusts track height");
    d.app().session.shortcuts.set("timeline.fastScroll", "Shift+Scroll", None, false, true).expect("assign");
    d.wheel(0.2, 0.0, -50.0, json!({"shift": true}));
    assert!(d.view().0 > before_scroll, "the legacy fast time-scroll action is configurable");
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

/// The zoom bar's circular end handles zoom, so hovering one shows a pointing hand; the middle of
/// the thumb scrolls and the bar beside it jumps, which keep the normal pointer.
#[test]
fn zoom_bar_handles_show_the_resize_cursor_on_hover() {
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

/// A handle drag goes on whatever the pointer is over: it zooms from anywhere, so the hand stays
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
    assert_eq!(d.cursor(), CursorIcon::PointingHand, "dragging along the bar");
    d.move_to(handle + 40.0, y - 300.0);
    assert_eq!(d.cursor(), CursorIcon::PointingHand, "dragging over the tracks");
    d.release(handle + 40.0, y - 300.0);
    assert_ne!(d.cursor(), CursorIcon::PointingHand, "button released, pointer over the tracks");
}

/// Dragging the thumb by its middle scrolls: with the thumb stopped at the end of the bar the
/// pointer can sit where a handle is, and must not show the hand.
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

#[test]
fn alternate_track_slider_position_moves_the_magnet_toolbar_clear_of_the_slider() {
    let mut d = Driver::demo();
    d.app().session.state.timeline_ui.track_slider_alternate_position = true;
    d.frames(3);
    let slider = d.rect("timeline.trackSlider.video.track");
    let magnet = d.rect("timeline.toggle.snap");
    let timecode = d.rect("timeline.timecode");
    assert!(magnet[0] >= slider[0] + slider[2], "Magnet hit target overlaps the left track slider: {magnet:?} {slider:?}");
    assert!(timecode[0] >= slider[0] + slider[2], "timecode hit target overlaps the left track slider: {timecode:?} {slider:?}");
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

#[test]
fn fullscreen_timeline_settings_shows_every_row() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    d.right_click_first_visible_frame(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0);
    d.frames(3);
    let rows: Vec<_> = d.harness.state().auto.elements.iter().filter(|e| e.id.starts_with("timeline.zoomBar.settings.")).collect();
    assert!(!rows.is_empty(), "settings are visible");
    let top = rows.iter().map(|r| r.rect[1]).fold(f32::INFINITY, f32::min);
    let bottom = rows.iter().map(|r| r.rect[1] + r.rect[3]).fold(f32::NEG_INFINITY, f32::max);
    assert!(bottom - top > 400.0, "all settings must occupy their full natural height");
    assert!((bar[1] + bar[3] / 2.0 - bottom).abs() < 40.0, "menu bottom must be beside the click: bottom={bottom}, bar={bar:?}");
    for row in rows {
        assert!(row.rect[1] >= 0.0 && row.rect[1] + row.rect[3] <= 980.0, "row outside fullscreen window: {} {:?}", row.id, row.rect);
    }
    for label in [
        "Hide Slider",
        "Controls",
        "Smooth Animations",
        "Invert Scrolling",
        "Control Track Sliders",
        "Playhead Centering",
        "Alternate Slider Position",
        "Maximum Zoom Level",
        "Alternate Drag Speed",
        "Playhead Centering Range",
        "Slider Minimap",
    ] {
        let found = d.harness.output().shapes.iter().any(|clipped| {
            if let egui::Shape::Text(text) = &clipped.shape {
                let bounds = clipped.shape.visual_bounding_rect();
                text.galley.job.text.contains(label) && text.opacity_factor > 0.0 && clipped.clip_rect.contains_rect(bounds)
            } else {
                false
            }
        });
        assert!(found, "menu label is missing or clipped: {label}");
    }
}

#[test]
fn fullscreen_timeline_settings_does_not_scroll() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    d.frames(3);
    let id = "timeline.zoomBar.settings.sliderMinimap";
    let before = d.rect(id);
    for dy in [-2000.0, 2000.0] {
        d.ok("ui.scroll", json!({"id": id, "fx": 0.5, "fy": 0.5, "dx": 0.0, "dy": dy, "modifiers": {}}));
        d.frames(3);
        let after = d.rect(id);
        assert!((after[1] - before[1]).abs() < 0.5, "menu scrolled: {before:?} -> {after:?}");
    }
}

/// The zoom-bar settings popup must use its measured height before its first visible frame.
#[test]
fn zoom_bar_settings_menu_opens_above_on_its_first_frame_without_jumping() {
    let mut d = Driver::demo();
    // A minimized native window can keep the app at its last tiny client size until it is
    // maximized from the title bar. Recreate that viewport transition before the first menu open.
    d.harness.set_size(egui::vec2(1.0, 1.0));
    d.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(true);
    d.frames(1);
    d.harness.set_size(egui::vec2(1600.0, 980.0));
    d.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(false);
    d.frames(1);
    let bar = d.rect("timeline.zoomBar.track");
    let point = (bar[0] + bar[2] * 0.67, bar[1] + bar[3] / 2.0);
    d.right_click_first_visible_frame(point.0, point.1);

    let row = "timeline.zoomBar.settings.smoothAnimations";
    assert!(d.harness.state().auto.elements.iter().any(|element| element.id == row), "the menu must be visible on the release frame");
    let first = d.rect_now(row);
    let popup = d.harness.ctx.data(|data| data.get_temp::<egui::Rect>(egui::Id::unique("timeline-zoombar-popup-rect"))).expect("popup rectangle");
    let painted_frame = d
        .harness
        .output()
        .shapes
        .iter()
        .filter_map(|shape| match &shape.shape {
            egui::Shape::Rect(rect)
                if rect.rect.width() > 250.0
                    && rect.rect.width() <= popup.width() + 1.0
                    && rect.rect.height() > 600.0
                    && rect.rect.height() <= popup.height() + 1.0 =>
            {
                Some(rect.rect)
            }
            _ => None,
        })
        .max_by(|a, b| (a.width() * a.height()).total_cmp(&(b.width() * b.height())))
        .expect("painted settings-menu frame");
    let painted = |d: &Driver| {
        d.harness.output().shapes.iter().any(|shape| {
            if let egui::Shape::Text(text) = &shape.shape {
                text.galley.job.text == "Smooth Animations" && text.opacity_factor > 0.0 && shape.clip_rect.contains_rect(shape.shape.visual_bounding_rect())
            } else {
                false
            }
        })
    };
    assert!(painted(&d), "menu must be painted on the release frame: row={first:?}, popup={popup:?}, click={point:?}");
    assert!(popup.contains_rect(painted_frame), "the painted menu frame is inside its popup area: {popup:?}, {painted_frame:?}");
    assert!((painted_frame.left() - point.0).abs() < 1.0, "the painted menu frame starts at the click x: {painted_frame:?}, {point:?}");
    assert!((painted_frame.bottom() - point.1).abs() < 1.0, "the painted menu frame must touch the click: {painted_frame:?}, {point:?}");
    assert!(first[1] + first[3] <= point.1 + 1.0, "the menu opens above the clicked slider: {first:?}, pointer={point:?}");
    let last = d.rect_now("timeline.zoomBar.settings.hideSlider");
    assert!(last[1] + last[3] <= 980.0, "the full menu fits without a default scrollbar: {last:?}");

    d.frames(1);
    let next = d.rect_now(row);
    assert!((next[1] - first[1]).abs() < 1.0, "the menu stays in place after its first visible frame: {first:?} -> {next:?}");
    d.click_at(bar[0] + 5.0, point.1, "left");
    d.frames(2);
    assert!(!d.harness.state().auto.elements.iter().any(|element| element.id == row), "outside click closed the menu");
    let bar_after = d.rect("timeline.zoomBar.track");
    assert!((bar_after[1] - bar[1]).abs() < 1.0, "bar moved: {bar:?} -> {bar_after:?}");
    d.harness.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(point.0, point.1)));
    d.frames(1);
    d.right_click_first_visible_frame(point.0, point.1);
    let reopened = d.rect_now(row);
    assert!(painted(&d), "reopened menu must be painted on the release frame");
    d.frames(1);
    let reopened_next = d.rect_now(row);
    assert!((reopened_next[1] - reopened[1]).abs() < 1.0, "reopened menu jumped: {reopened:?} -> {reopened_next:?}");
}

#[test]
fn timeline_settings_scrollbar_only_appears_when_short_and_menu_survives_minimize() {
    let overflow_key = egui::Id::unique("timeline-zoombar-popup-overflow");
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] * 0.67, bar[1] + bar[3] / 2.0, "right");
    assert!(d.harness.state().auto.elements.iter().any(|element| element.id == "timeline.zoomBar.settings.smoothAnimations"));
    let overflow = |d: &Driver| d.harness.ctx.data(|data| data.get_temp::<bool>(overflow_key)).unwrap_or(false);
    let normal_content = d.harness.ctx.data(|data| data.get_temp::<f32>(egui::Id::unique("timeline-zoombar-popup-content-height"))).unwrap_or(-1.0);
    let normal_view = d.harness.ctx.data(|data| data.get_temp::<f32>(egui::Id::unique("timeline-zoombar-popup-view-height"))).unwrap_or(-1.0);
    assert!(!overflow(&d), "a normal-height window must not show a menu scrollbar: content={normal_content}, view={normal_view}");

    d.harness.set_size(egui::vec2(1600.0, 600.0));
    d.frames(3);
    assert!(overflow(&d), "the scrollbar appears when the window is too short for the menu");
    let first = d.rect("timeline.zoomBar.settings.hideSlider");
    let last = d.rect("timeline.zoomBar.settings.sliderMinimap");
    assert!(first[1] < 0.0, "the short menu opens scrolled to its bottom: {first:?}");
    assert!(last[1] >= 0.0 && last[1] + last[3] <= 600.0, "the bottom settings row stays in view: {last:?}");

    d.harness.set_size(egui::vec2(1.0, 1.0));
    d.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(true);
    d.frames(2);
    d.harness.set_size(egui::vec2(1600.0, 600.0));
    d.harness.input_mut().viewports.entry(egui::ViewportId::ROOT).or_default().minimized = Some(false);
    for _ in 0..4 {
        d.frames(1);
        assert!(
            d.harness.state().auto.elements.iter().any(|element| element.id == "timeline.zoomBar.settings.smoothAnimations"),
            "the menu stays present while the window restores"
        );
    }

    for height in [420.0, 700.0, 980.0] {
        d.harness.set_size(egui::vec2(1600.0, height));
        d.frames(2);
        assert!(
            d.harness.state().auto.elements.iter().any(|element| element.id == "timeline.zoomBar.settings.smoothAnimations"),
            "the menu survives viewport height {height}"
        );
        assert_eq!(overflow(&d), height < 700.0, "scrollbar visibility follows available height {height}");
        let last = d.rect("timeline.zoomBar.settings.sliderMinimap");
        assert!(last[1] >= 0.0 && last[1] + last[3] <= height, "the bottom row remains on screen at height {height}: {last:?}");
    }

    d.harness.set_size(egui::vec2(1600.0, 980.0));
    d.frames(3);
    assert!(!overflow(&d), "restoring a normal-height window removes the scrollbar");
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
    // the menu starts at its bottom when it has to scroll: wheel up (over a bottom row) to reach it
    d.ok("ui.scroll", json!({"id": "timeline.zoomBar.settings.sliderMinimap", "fx": 0.5, "fy": 0.5, "dx": 0.0, "dy": 2000.0, "modifiers": {}}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.unhideSlider"}));
    d.frames(2);
    assert!(!d.app().session.state.timeline_ui.zoom_bar_hidden, "Unhide Slider brings the bar back");
}

/// The rows this session added to both slider menus: the Controls row and Invert Scrolling on the
/// Timeline slider, the Controls row and Snap to Tracks on the track sliders -- and Snap to
/// Tracks toggles and persists even though it drives nothing yet.
#[test]
fn both_slider_menus_carry_the_controls_rows_and_snap_to_tracks() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    d.frames(2);
    assert!(d.has("timeline.zoomBar.settings.controls"), "the Controls row on the Timeline slider's menu");
    assert!(d.has("timeline.zoomBar.settings.invertedControls"), "Invert Scrolling (the old Inverted Controls)");
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.invertedControls"}));
    d.frames(2);
    assert!(d.app().session.state.timeline_ui.inverted_controls, "the switch flips and the menu stays open");
    assert!(d.has("timeline.zoomBar.settings.controls"), "still open");
    // the track sliders' menu: Snap to Tracks above Decouple Edges, on by default
    let slider = d.rect("timeline.trackSlider.video");
    d.click_at(slider[0] + slider[2] / 2.0, slider[1] + slider[3] / 2.0, "right");
    d.frames(2);
    assert!(d.has("timeline.trackSlider.settings.controls"), "the Controls row on the track sliders' menu");
    assert!(d.has("timeline.trackSlider.settings.snapToTracks"), "Snap to Tracks");
    assert!(d.app().session.state.timeline_ui.track_slider_snap, "on by default");
    d.ok("ui.click", json!({"id": "timeline.trackSlider.settings.snapToTracks"}));
    d.frames(2);
    assert!(!d.app().session.state.timeline_ui.track_slider_snap, "the toggle takes (it drives nothing yet)");
}

#[test]
fn short_window_opens_timeline_settings_at_the_bottom() {
    // 420 pt is well below the menu; 600 and 700 pt are the ordinary "too small" windows where
    // the menu still has to scroll. Every one of them must start at the menu's bottom.
    for height in [420.0, 600.0, 700.0] {
        let mut d = Driver::sized_with_step(1200.0, height, 1.0 / 60.0);
        let bar = d.rect("timeline.zoomBar.track");
        d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
        d.frames(3);
        assert!(d.has("timeline.zoomBar.settings.sliderMinimap"), "the final settings row is visible without scrolling");
        let first = d.rect("timeline.zoomBar.settings.hideSlider");
        let last = d.rect("timeline.zoomBar.settings.sliderMinimap");
        let content = last[1] + last[3] - first[1];
        assert!(last[1] >= -0.5 && last[1] + last[3] <= height + 0.5, "bottom row inside the {height} pt window: {last:?}");
        if content > height {
            assert!(first[1] < 0.5, "the menu starts at its bottom (top rows scrolled off) in a {height} pt window: first row {first:?}, content {content}");
        }
    }
}

/// The Controls boxes: clicking one listens ("Enter Shortcut"), the next key becomes the binding
/// and then that chord does what the gesture did -- fitting the Playhead Centering Range.
#[test]
fn clicking_a_controls_box_takes_the_next_key_and_that_key_does_the_action() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    d.frames(2);
    // the flyout opens on a real hover of the Controls row: the pointer has to move first
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + 8.0);
    d.frames(2);
    d.move_to(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    d.frames(3);
    assert!(d.has("timeline.zoomBar.settings.controlsFlyout"), "the flyout opens beside the row");
    assert!(d.has("timeline.zoomBar.settings.controlsFlyout.box0"), "the first box");
    assert!(d.has("timeline.zoomBar.settings.controlsFlyout.box6"), "the full set of Timeline actions is listed");
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.box0"}));
    d.frames(2);
    assert_eq!(d.app().session.state.timeline_ui.control_center_on_playhead, "Double Click", "binding stays until capture completes");
    let before_capture = d.app().ui.timeline.target_pps;
    d.ok("ui.key", json!({"key": "K"}));
    d.frames(2);
    assert_eq!(d.app().session.state.timeline_ui.control_center_on_playhead, "K", "the key became the binding");
    assert_eq!(d.app().ui.timeline.target_pps, before_capture, "capturing a key does not fire it");
    // the binding replaces the double click: pressing K fits the Playhead Centering Range
    let before = d.app().ui.timeline.target_pps;
    d.ok("ui.key", json!({"key": "K"}));
    d.frames(3);
    assert!(d.app().ui.timeline.target_pps != before, "K does what Double Click did");
    // and the row's reset arrow puts the built-in gesture back
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.reset0"}));
    d.frames(2);
    assert_eq!(d.app().session.state.timeline_ui.control_center_on_playhead, "Double Click", "Reset restores the default gesture");
}

#[test]
fn track_fit_fills_the_bar_and_updates_when_the_timeline_grows() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    d.ok("ui.click", json!({"id": "timeline.trackSlider.video", "count": 2, "modifiers": {"shift": true}}));
    d.frames(4);
    let thumb = d.rect("timeline.trackSlider.video");
    let bar = d.rect("timeline.trackSlider.video.track");
    assert!((thumb[3] - bar[3]).abs() < 0.1, "fit {thumb:?} {bar:?}");
    let h = d.app().ui.timeline.video_track_h;
    d.harness.set_size(egui::vec2(1600.0, 1400.0));
    d.frames(5);
    let thumb = d.rect("timeline.trackSlider.video");
    let bar = d.rect("timeline.trackSlider.video.track");
    assert!((thumb[3] - bar[3]).abs() < 0.1, "resized {thumb:?} {bar:?}");
    assert!(d.app().ui.timeline.video_track_h >= h);
    d.app().session.state.timeline_ui.track_slider_snap = false;
    d.app().ui.timeline.video_track_h *= 1.5;
    d.frames(3);
    let circle = d.rect("timeline.trackSlider.video.topCircle");
    let x = circle[0] + circle[2] / 2.0;
    let y = circle[1] + circle[3] / 2.0;
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x, "y": bar[1] - 100.0}}));
    d.frames(3);
    let thumb = d.rect("timeline.trackSlider.video");
    assert!((thumb[3] - bar[3]).abs() < 0.2, "manual fit {thumb:?} {bar:?}");
}

#[test]
fn mirror_copies_the_visible_track_position_at_the_outer_end() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    d.exec("sequence.addTracks", json!({"video": 12, "audio": 12}));
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 180.0, "audioTrackHeight": 180.0}}));
    d.app().ui.timeline.v_scroll = 100000.0;
    d.frames(4);
    d.ok("ui.click", json!({"id": "timeline.trackSlider.video", "count": 2}));
    d.frames(4);
    let vb = d.rect("timeline.trackSlider.video.track");
    let ab = d.rect("timeline.trackSlider.audio.track");
    let vt = d.rect("timeline.trackSlider.video");
    let at = d.rect("timeline.trackSlider.audio");
    let vf = (vt[1] - vb[1]) / (vb[3] - vt[3]).max(1.0);
    let af = (at[1] - ab[1]) / (ab[3] - at[3]).max(1.0);
    let v = &d.app().ui.timeline;
    assert!((v.video_track_h - v.audio_track_h).abs() < 0.01);
    assert!((vf + af - 1.0).abs() < 0.02, "mirror positions should reverse: video {vf}, audio {af}");
}

#[test]
fn mirroring_a_fit_track_slider_fits_the_other_section_too() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    d.exec("sequence.addTracks", json!({"video": 12, "audio": 12}));
    d.frames(3);
    let vb = d.rect("timeline.trackSlider.video.track");
    let ab = d.rect("timeline.trackSlider.audio.track");
    let slots = 12.0 + 1.0 / 3.0;
    let video_fit = ((vb[3] + 5.0) / slots).max(22.0);
    let audio_fit = ((ab[3] + 5.0) / slots).max(22.0);
    d.app().ui.timeline.video_track_h = video_fit;
    d.app().ui.timeline.audio_track_h = 180.0;
    d.frames(3);
    d.ok("ui.click", json!({"id": "timeline.trackSlider.video", "count": 2}));
    d.frames(4);
    assert!((d.app().ui.timeline.audio_track_h - audio_fit).abs() < 0.05);
}

#[test]
fn mirror_aligns_the_center_track_between_different_pane_heights() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    d.exec("sequence.addTracks", json!({"video": 12, "audio": 12}));
    d.app().ui.timeline.split = 0.7;
    d.app().ui.timeline.video_track_h = 170.0;
    d.app().ui.timeline.audio_track_h = 170.0;
    d.app().ui.timeline.v_scroll = 2.0 * 170.0;
    d.app().ui.timeline.a_scroll = 0.0;
    d.frames(4);
    let vb = d.rect("timeline.trackSlider.video.track");
    let v3 = d.rect("timeline.track.V3.color");
    let thumb = d.rect("timeline.trackSlider.video");
    d.ok("ui.click", json!({"x": thumb[0] + thumb[2] / 2.0, "y": thumb[1] + thumb[3] / 2.0, "count": 2}));
    d.frames(100);
    let a3 = d.rect("timeline.track.A3.color");
    let divider = vb[1] + vb[3] + 3.0 + 2.5;
    let video_offset = v3[1] + v3[3] / 2.0 - divider;
    let audio_offset = a3[1] + a3[3] / 2.0 - divider;
    assert!((video_offset + audio_offset).abs() < 0.1, "V3 and A3 should occupy pixel-mirrored positions around the divider: {video_offset} / {audio_offset}");
}

#[test]
fn mirroring_audio_a1_halfway_through_places_video_v1_halfway_through() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    d.exec("sequence.addTracks", json!({"video": 8, "audio": 8}));
    d.app().session.state.timeline_ui.track_slider_smooth = false;
    d.app().ui.timeline.video_track_h = 170.0;
    d.app().ui.timeline.audio_track_h = 170.0;
    d.app().ui.timeline.a_scroll = 85.0;
    d.frames(4);
    let thumb = d.rect("timeline.trackSlider.audio");
    d.ok("ui.click", json!({"x": thumb[0] + thumb[2] / 2.0, "y": thumb[1] + thumb[3] / 2.0, "count": 2}));
    d.frames(3);

    let layout = d.app().tl.layout.as_ref().expect("timeline layout");
    let v1 = layout.rows.iter().find(|r| r.kind == filmcraft_project::TrackKind::Video && r.index == 0).expect("V1");
    let a1 = layout.rows.iter().find(|r| r.kind == filmcraft_project::TrackKind::Audio && r.index == 0).expect("A1");
    let visible_v1 = layout.shown(v1).height();
    let visible_a1 = layout.shown(a1).height();
    assert!((d.app().ui.timeline.v_scroll - 85.0).abs() < 0.01, "V1 mirror offset must be half a row: {}", d.app().ui.timeline.v_scroll);
    assert!((visible_v1 - visible_a1).abs() < 0.1, "A1 exposes {visible_a1}px while mirrored V1 exposes {visible_v1}px");
    assert!((visible_v1 - 85.0).abs() < 0.1, "both first tracks should expose exactly half their height");
}

#[test]
fn controls_flyout_is_centered_on_its_first_visible_frame() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.harness.input_mut().events.push(egui::Event::PointerMoved(egui::pos2(row[0] + 10.0, row[1] + row[3] / 2.0)));
    d.frames(1);
    let first = d.app().auto.find("timeline.zoomBar.settings.controlsFlyout").unwrap().rect;
    d.frames(1);
    let second = d.app().auto.find("timeline.zoomBar.settings.controlsFlyout").unwrap().rect;
    let first_y = first[1] + first[3] / 2.0;
    let second_y = second[1] + second[3] / 2.0;
    assert!((first_y - second_y).abs() < 0.1, "{first:?} -> {second:?}");
    assert!((first_y - (row[1] + row[3] / 2.0)).abs() < 0.5, "{first:?}, row {row:?}");
}

#[test]
fn minimap_flyout_is_centered_without_a_first_frame_jump_at_the_top_slider_position() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"timeline": {"alternateSliderPosition": true}}));
    d.frames(3);
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    let row = d.rect("timeline.zoomBar.settings.sliderMinimap");
    let point = egui::pos2(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    d.harness.input_mut().events.push(egui::Event::PointerMoved(point));
    d.frames(1);
    let first = d.app().auto.find("timeline.zoomBar.settings.minimapFlyout").unwrap().rect;
    d.frames(1);
    let second = d.app().auto.find("timeline.zoomBar.settings.minimapFlyout").unwrap().rect;
    let row2 = d.app().auto.find("timeline.zoomBar.settings.sliderMinimap").unwrap().rect;
    let first_center = first[1] + first[3] / 2.0;
    let second_center = second[1] + second[3] / 2.0;
    assert!((first_center - second_center).abs() < 0.1, "first {first:?}, second {second:?}, row1 {row:?}, row2 {row2:?}");
    assert!((first_center - (row[1] + row[3] / 2.0)).abs() < 0.5, "flyout centered on the row: {first:?}, row {row:?}");
}

#[test]
fn controls_capture_triple_click_and_warn_on_duplicate_without_firing() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + row[3] / 2.0);
    d.frames(2);
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.box0"}));
    let before = d.app().ui.timeline.target_pps;
    d.ok("ui.click", json!({"x": row[0] + 20.0, "y": row[1] + row[3] / 2.0, "count": 3}));
    d.frames(4);
    assert_eq!(d.app().session.state.timeline_ui.control_center_on_playhead, "Triple Click");
    assert_eq!(d.app().ui.timeline.target_pps, before);
    assert!(d.has("timeline.zoomBar.settings.controlsFlyout.conflict0"));
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.conflict0"}));
    d.frames(2);
    assert!(d.app().dialog.is_some(), "the warning opens Keyboard Shortcuts");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
}

#[test]
fn controls_can_capture_double_click_and_escape_to_unbind() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + row[3] / 2.0);
    d.frames(2);
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.box1"}));
    let before = d.app().ui.timeline.target_pps;
    d.ok("ui.click", json!({"x": row[0] + 20.0, "y": row[1] + row[3] / 2.0, "count": 2}));
    d.frames(30);
    assert_eq!(d.app().session.state.timeline_ui.control_explode_slider, "Double Click");
    assert_eq!(d.app().ui.timeline.target_pps, before);
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.box1"}));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    assert_eq!(d.app().session.state.timeline_ui.control_explode_slider, "Unbound");
    let before_unbound_click = d.app().ui.timeline.target_pps;
    d.ok("ui.click", json!({"id": "timeline.zoomBar", "count": 2}));
    d.frames(3);
    assert_eq!(d.app().ui.timeline.target_pps, before_unbound_click, "Unbound disables the original double-click action");
}

#[test]
fn every_controls_black_box_can_be_clicked_without_crashing() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + row[3] / 2.0);
    d.frames(2);
    for i in 0..7 {
        d.ok("ui.click", json!({"id": format!("timeline.zoomBar.settings.controlsFlyout.box{i}")}));
        d.ok("ui.key", json!({"key": "Escape"}));
        d.frames(1);
    }

    let video = d.rect("timeline.trackSlider.video");
    d.click_at(video[0] + video[2] / 2.0, video[1] + video[3] / 2.0, "right");
    let row = d.rect("timeline.trackSlider.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + row[3] / 2.0);
    d.frames(2);
    for i in 0..5 {
        d.ok("ui.click", json!({"id": format!("timeline.trackSlider.settings.controlsFlyout.box{i}")}));
        d.ok("ui.key", json!({"key": "Escape"}));
        d.frames(1);
    }
}

#[test]
fn linked_track_resize_uses_main_alternate_drag_speed_for_shift_and_right_click() {
    let resize = |main_speed: f64, track_speed: f64, right_click: bool| {
        let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
        d.ok("ui.set", json!({"timeline": {"controlTracks": true, "videoTrackHeight": 100.0, "audioTrackHeight": 100.0}}));
        let settings = &mut d.app().session.state.timeline_ui;
        settings.shift_drag_speed = main_speed;
        settings.track_slider_shift_drag_speed = track_speed;
        if right_click {
            d.app().session.shortcuts.set("timeline.sliderAlternate", "Right Click", None, false, true).expect("assign");
        }
        d.frames(4);
        let start = d.app().ui.timeline.video_track_h;
        let r = d.rect("timeline.zoomBar");
        let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        d.press(x, y);
        if right_click {
            d.harness.input_mut().events.push(egui::Event::PointerButton {
                pos: egui::pos2(x, y),
                button: egui::PointerButton::Secondary,
                pressed: true,
                modifiers: egui::Modifiers::NONE,
            });
            d.frames(1);
        } else {
            d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
            d.frames(1);
        }
        d.move_to(x, y - 40.0);
        let moved = (d.app().ui.timeline.video_track_h - start).abs();
        d.release(x, y - 40.0);
        if right_click {
            d.harness.input_mut().events.push(egui::Event::PointerButton {
                pos: egui::pos2(x, y - 40.0),
                button: egui::PointerButton::Secondary,
                pressed: false,
                modifiers: egui::Modifiers::NONE,
            });
        } else {
            d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
        }
        d.frames(1);
        moved
    };
    for right_click in [false, true] {
        let slow = resize(0.5, 2.0, right_click);
        let same_speed_other_track_setting = resize(0.5, 0.5, right_click);
        let fast = resize(2.0, 0.5, right_click);
        assert!((slow - same_speed_other_track_setting).abs() < 0.1, "track-slider speed must not affect linked drag speed");
        assert!(fast > slow * 2.0, "main speed should apply (right-click modifier: {right_click}): {slow} -> {fast}");
    }
}

#[test]
fn rebinding_replaces_the_old_gesture_and_runs_the_new_key() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    d.app().session.shortcuts.set("timeline.sliderCenter", "O", None, false, true).expect("assign");
    d.ok("ui.set", json!({"timeline": {"pps": 100.0, "scroll": 10.0}}));
    d.frames(8);
    let before = d.app().ui.timeline.target_pps;
    d.ok("ui.click", json!({"id": "timeline.zoomBar", "count": 2}));
    d.frames(3);
    assert_eq!(d.app().ui.timeline.target_pps, before, "old double-click no longer fires");
    d.ok("ui.key", json!({"key": "O"}));
    d.frames(3);
    assert_ne!(d.app().ui.timeline.target_pps, before, "the new key fires");
}

#[test]
fn zooming_out_past_the_playhead_does_not_capture_it() {
    let mut d = Driver::demo();
    d.exec("playhead.end", json!({}));
    let ph = d.app().session.playhead().seconds();
    d.ok("ui.set", json!({"timeline": {"pps": 100.0, "scroll": ph + 5.0, "playheadCentering": true}}));
    d.frames(4);
    let width = d.rect("timeline.tracks")[2] as f64;
    let (scroll, pps, _, _) = d.view();
    let center = scroll + width / pps / 2.0;
    let thumb = d.rect("timeline.zoomBar");
    let x = thumb[0] + 6.0;
    let y = thumb[1] + thumb[3] / 2.0;
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x - 100.0, "y": y}}));
    d.frames(3);
    let (scroll, pps, _, _) = d.view();
    assert!(ph >= scroll && ph <= scroll + width / pps, "playhead entered view");
    assert!((scroll + width / pps / 2.0 - center).abs() < 0.05, "zoom-out keeps the view center fixed");
}

#[test]
fn plus_minus_zoom_centers_on_playhead_when_enabled_and_on_view_center_when_disabled() {
    let mut d = Driver::demo();
    d.exec("playhead.end", json!({}));
    let ph = d.app().session.playhead().seconds();
    d.ok("ui.set", json!({"timeline": {"pps": 40.0, "scroll": 0.0, "playheadCentering": true}}));
    d.frames(4);
    d.ok("ui.key", json!({"key": "="}));
    d.frames(4);
    let width = d.rect("timeline.tracks")[2] as f64;
    let v = &d.app().ui.timeline;
    assert!((v.target_scroll + width / v.target_pps / 2.0 - ph).abs() < 0.02);
    d.ok("ui.set", json!({"timeline": {"playheadCentering": false, "pps": 40.0, "scroll": 20.0}}));
    d.frames(4);
    let before = d.app().ui.timeline.target_scroll + width / (2.0 * d.app().ui.timeline.target_pps);
    d.ok("ui.key", json!({"key": "-"}));
    d.frames(4);
    let v = &d.app().ui.timeline;
    assert!((v.target_scroll + width / (2.0 * v.target_pps) - before).abs() < 0.02);
}

#[test]
fn playhead_centering_range_centers_at_the_maximum_zoom_cap() {
    let mut d = Driver::demo();
    d.exec("playhead.end", json!({}));
    let ph = d.app().session.playhead().seconds();
    d.app().session.shortcuts.set("timeline.sliderCenter", "O", None, false, true).expect("assign center shortcut");
    d.ok("ui.set", json!({"timeline": {"playheadSideSeconds": 0.1}}));
    d.frames(3);
    d.ok("ui.key", json!({"key": "O"}));
    d.frames(8);
    let width = f64::from(d.rect("timeline.tracks")[2]);
    let view = &d.app().ui.timeline;
    let center = view.target_scroll + width / (2.0 * view.target_pps);
    assert!((center - ph).abs() < 0.05, "playhead should be centered at the zoom cap: center={center}, playhead={ph}, pps={}", view.pps);
}

#[test]
fn hiding_and_unhiding_track_sliders_keeps_timeline_slider_geometry_fixed() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"timeline": {"pps": 120.0, "scroll": 2.0}}));
    d.frames(4);
    let before = d.rect("timeline.zoomBar");
    d.ok("ui.set", json!({"timeline": {"trackSliderHidden": true}}));
    d.frames(4);
    let hidden = d.rect("timeline.zoomBar");
    for i in 0..4 {
        assert!((hidden[i] - before[i]).abs() < 0.1, "hidden {before:?} -> {hidden:?}");
    }
    d.ok("ui.set", json!({"timeline": {"trackSliderHidden": false}}));
    d.frames(4);
    let after = d.rect("timeline.zoomBar");
    for i in 0..4 {
        assert!((after[i] - before[i]).abs() < 0.1, "restored {before:?} -> {after:?}");
    }
}

#[test]
fn linked_shift_drag_ignores_track_slider_alternate_drag_speed() {
    let resize = |speed: f64| {
        let mut d = Driver::demo();
        d.ok("ui.set", json!({"timeline": {"controlTracks": true, "videoTrackHeight": 100.0, "audioTrackHeight": 100.0}}));
        d.app().session.state.timeline_ui.track_slider_shift_drag_speed = speed;
        d.app().session.state.timeline_ui.shift_drag_speed = 1.0;
        d.frames(3);
        let start = d.app().ui.timeline.video_track_h;
        let r = d.rect("timeline.zoomBar");
        let x = r[0] + r[2] / 2.0;
        let y = r[1] + r[3] / 2.0;
        d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x, "y": y - 40.0}, "steps": 8, "modifiers": {"shift": true}}));
        d.frames(3);
        (d.app().ui.timeline.video_track_h - start).abs()
    };
    let slow = resize(0.5);
    let fast = resize(2.0);
    assert!((fast - slow).abs() < 0.1, "the track slider speed setting must not affect linked track control: {slow} vs {fast}");
}

#[test]
fn rebound_timeline_key_replaces_default_shortcut() {
    let mut d = Driver::demo();
    d.app().session.shortcuts.set("view.zoomOut", "K", None, false, true).expect("set shortcut");
    d.ok("ui.set", json!({"timeline": {"pps": 100.0, "scroll": 10.0}}));
    d.frames(3);
    let before = d.app().ui.timeline.target_pps;
    d.ok("ui.key", json!({"key": "-"}));
    d.frames(3);
    assert_eq!(d.app().ui.timeline.target_pps, before, "old shortcut must stop working");
    d.ok("ui.key", json!({"key": "K"}));
    d.frames(3);
    assert_ne!(d.app().ui.timeline.target_pps, before, "new shortcut must work");
}

#[test]
fn short_timeline_settings_scroll_up_from_bottom() {
    let mut d = Driver::sized_with_step(1200.0, 420.0, 1.0 / 60.0);
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    d.frames(4);
    let before = d.rect("timeline.zoomBar.settings.hideSlider")[1];
    d.ok("ui.scroll", json!({"id": "timeline.zoomBar.settings.sliderMinimap", "dy": 2000.0}));
    d.frames(4);
    let after = d.rect("timeline.zoomBar.settings.hideSlider")[1];
    assert!(after > before + 50.0, "scroll up moves rows: {before} -> {after}");
}
#[test]
fn short_track_settings_scroll_up_from_bottom() {
    let mut d = Driver::sized_with_step(1200.0, 300.0, 1.0 / 60.0);
    d.app().tl.track_slider_settings_pos = Some(egui::pos2(400.0, 100.0));
    d.frames(4);
    let before = d.rect("timeline.trackSlider.settings.hideSlider")[1];
    d.ok("ui.scroll", json!({"id": "timeline.trackSlider.settings.shiftDragSpeed", "dy": 2000.0}));
    d.frames(4);
    let after = d.rect("timeline.trackSlider.settings.hideSlider")[1];
    assert!(after > before + 30.0, "scroll up moves rows: {before} -> {after}");
}
#[test]
fn unbound_timeline_key_does_not_run_default() {
    let mut d = Driver::demo();
    d.app().session.shortcuts.clear("view.zoomOut", None, None).expect("clear shortcut");
    d.ok("ui.set", json!({"timeline": {"pps": 100.0, "scroll": 10.0}}));
    d.frames(3);
    let before = d.app().ui.timeline.target_pps;
    d.ok("ui.key", json!({"key": "-"}));
    d.frames(3);
    assert_eq!(d.app().ui.timeline.target_pps, before);
}
#[test]
fn slider_controls_share_shortcuts_and_timeout_unbinds() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 0.25);
    assert!(d.app().session.shortcuts.command("timeline.sliderCenter").is_some());
    d.app().session.shortcuts.set("timeline.sliderCenter", "K", None, false, true).expect("set shortcut");
    d.frames(2);
    assert_eq!(d.app().session.state.timeline_ui.control_center_on_playhead, "K");
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    d.frames(2);
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + 8.0);
    d.frames(2);
    d.move_to(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    d.frames(2);
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.box0"}));
    d.frames(15);
    assert!(d.app().session.shortcuts.primary("timeline.sliderCenter").is_none(), "idle capture clears the shared binding");
}
#[test]
fn slider_conflict_warning_updates_when_other_binding_cleared() {
    let mut d = Driver::demo();
    d.app().session.shortcuts.set("timeline.sliderExplode", "Double Click", None, false, true).expect("assign");
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    d.frames(2);
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + 8.0);
    d.frames(2);
    d.move_to(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    d.frames(2);
    assert!(d.has("timeline.zoomBar.settings.controlsFlyout.conflict1"));
    d.app().session.shortcuts.clear("timeline.sliderCenter", None, None).expect("clear");
    d.frames(2);
    assert!(!d.has("timeline.zoomBar.settings.controlsFlyout.conflict1"));
}
#[test]
fn resized_window_keeps_timeline_settings_scrollable() {
    let mut d = Driver::sized_with_step(1200.0, 980.0, 1.0 / 60.0);
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    d.frames(4);
    d.harness.set_size(egui::vec2(1200.0, 520.0));
    d.frames(8);
    let last = d.rect("timeline.zoomBar.settings.sliderMinimap");
    assert!(last[1] >= 0.0 && last[1] + last[3] <= 520.0, "bottom row visible: {last:?}");
    let before = d.rect("timeline.zoomBar.settings.hideSlider")[1];
    d.ok("ui.scroll", json!({"id": "timeline.zoomBar.settings.sliderMinimap", "dy": 2000.0}));
    d.frames(4);
    let after = d.rect("timeline.zoomBar.settings.hideSlider")[1];
    assert!(after > before + 30.0, "scroll after resize: {before} -> {after}");
}
#[test]
fn default_zoom_controls_have_no_conflict_warning() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    d.frames(2);
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + 8.0);
    d.frames(2);
    d.move_to(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    d.frames(2);
    assert!(!d.has("timeline.zoomBar.settings.controlsFlyout.conflict5"));
    assert!(!d.has("timeline.zoomBar.settings.controlsFlyout.conflict6"));
    assert!(d.has("timeline.zoomBar.settings.controlsFlyout.sequenceBox"));
}
#[test]
fn right_click_binding_is_rejected_with_message() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + row[3] / 2.0);
    d.frames(2);
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.box0"}));
    d.click_at(row[0] + 20.0, row[1] + row[3] / 2.0, "right");
    d.frames(30);
    assert_eq!(d.app().session.state.timeline_ui.control_center_on_playhead, "Double Click");
    assert!(d.has("timeline.rightClickDialog.ok"));
}

#[test]
fn controls_capture_modifier_chord() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + row[3] / 2.0);
    d.frames(2);
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.box2"}));
    let mods = egui::Modifiers { shift: true, alt: true, ..Default::default() };
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(mods));
    d.frames(2);
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    d.frames(2);
    let binding = d.app().session.shortcuts.primary("timeline.sliderAlternate");
    assert_eq!(binding.as_deref(), Some("Alt+Shift"));
}
#[test]
fn sequence_box_captures_new_shortcut() {
    let mut d = Driver::demo();
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    d.frames(2);
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + 8.0);
    d.frames(2);
    d.move_to(row[0] + row[2] / 2.0, row[1] + row[3] / 2.0);
    d.frames(3);
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.sequenceBox"}));
    d.frames(2);
    d.ok("ui.key", json!({"key": "K"}));
    d.frames(2);
    assert_eq!(d.app().session.shortcuts.primary("view.zoomToSequence").as_deref(), Some("K"));
    d.app().session.shortcuts.set("view.zoomToSequence", "Ctrl+Z", None, false, true).expect("assign conflict");
    d.frames(2);
    assert!(d.has("timeline.zoomBar.settings.controlsFlyout.sequenceConflict"));
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.reset1"}));
    d.frames(2);
    assert_eq!(d.app().session.shortcuts.primary("view.zoomToSequence").as_deref(), Some("\\"));
}

#[test]
fn linked_track_control_uses_independent_angle_limits() {
    for (dx, left, right, expected) in [(-10.0, 30.0, 89.0, true), (10.0, 30.0, 89.0, false), (-10.0, 89.0, 30.0, false), (10.0, 89.0, 30.0, true)] {
        let mut d = Driver::tall();
        d.app().session.state.timeline_ui.control_tracks = true;
        d.app().session.state.timeline_ui.control_tracks_angle_left = left;
        d.app().session.state.timeline_ui.control_tracks_angle_right = right;
        let r = d.rect("timeline.zoomBar");
        let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        d.press(x, y);
        d.move_to(x + dx, y - 20.0);
        assert_eq!(d.app().tl.zoombar_track_control, expected, "dx={dx}, left={left}, right={right}");
        d.release(x + dx, y - 20.0);
    }
}

#[test]
fn equal_track_counts_fit_and_mirror_at_both_outer_walls() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    let seq = d.app().session.active_sequence().unwrap();
    assert_eq!(seq.video_tracks.len(), seq.audio_tracks.len());
    d.app().session.state.timeline_ui.track_slider_smooth = false;
    d.app().ui.timeline.video_track_h = 22.0;
    d.app().ui.timeline.audio_track_h = 22.0;
    d.frames(4);
    let vb = d.rect("timeline.trackSlider.video.track");
    let ab = d.rect("timeline.trackSlider.audio.track");
    assert!((vb[3] - ab[3]).abs() < 0.01, "bars {vb:?} {ab:?}");
    let v = &d.app().ui.timeline;
    assert!((v.video_track_h - v.audio_track_h).abs() < 0.01, "fit heights {} {}", v.video_track_h, v.audio_track_h);
    d.app().ui.timeline.video_track_h = 130.0;
    d.app().ui.timeline.v_scroll = 100000.0;
    d.frames(3);
    d.ok("ui.click", json!({"id": "timeline.trackSlider.video", "count": 2}));
    d.frames(4);
    let vt = d.rect("timeline.trackSlider.video");
    let at = d.rect("timeline.trackSlider.audio");
    assert!((vt[1] - vb[1]).abs() < 0.01, "video wall {vt:?} {vb:?}");
    assert!((at[1] + at[3] - ab[1] - ab[3]).abs() < 0.01, "audio wall {at:?} {ab:?}");
    assert!((vt[3] - at[3]).abs() < 0.01, "thumb lengths {vt:?} {at:?}");
}

#[test]
fn track_snap_aligns_visible_area_edge_with_named_track_boundaries() {
    for window_height in [900.0, 980.0] {
        for count in [3usize, 8] {
            for video in [true, false] {
                for split in [0.45, 0.55] {
                    let mut d = Driver::sized_with_step(1600.0, window_height, 1.0 / 60.0);
                    if count > 3 {
                        d.exec("sequence.addTracks", json!({"video": count - 3, "audio": count - 3}));
                    }
                    d.app().session.state.timeline_ui.track_slider_decouple = true;
                    d.app().session.state.timeline_ui.track_slider_snap = true;
                    d.app().ui.timeline.split = split;
                    d.app().ui.timeline.video_track_h = 220.0;
                    d.app().ui.timeline.audio_track_h = 220.0;
                    d.frames(4);
                    let circle_id = if video { "timeline.trackSlider.video.topCircle" } else { "timeline.trackSlider.audio.bottomCircle" };
                    let (view_min, view_max) = track_view(&mut d, video);
                    let view_edge = if video { view_min } else { view_max };
                    // Check the useful edit boundary itself: the audio track edge sits two points
                    // below the visible cutoff, putting its separator fully outside the edit area.
                    let snap_edge = if video { view_min } else { view_max + 2.0 };
                    let circle = d.rect(circle_id);
                    let (x, start) = (circle[0] + circle[2] / 2.0, circle[1] + circle[3] / 2.0);
                    let mut reached = Vec::new();
                    d.press(x, start);
                    for step in 1..=180 {
                        let y = start + if video { -(step as f32) } else { step as f32 };
                        d.move_to(x, y);
                        let layout = d.app().tl.layout.as_ref().unwrap();
                        let rows: Vec<_> = layout
                            .rows
                            .iter()
                            .filter(|r| matches!((video, r.kind), (true, filmcraft_project::TrackKind::Video) | (false, filmcraft_project::TrackKind::Audio)))
                            .collect();
                        for (index, row) in rows.iter().enumerate().take(3) {
                            let line = if video { row.rect.min.y + 1.0 } else { row.rect.max.y };
                            if (line - snap_edge).abs() < 0.02 && !reached.contains(&index) {
                                reached.push(index);
                            }
                        }
                    }
                    d.release(x, start + if video { -180.0 } else { 180.0 });
                    let (snap, track_h, scroll) = {
                        let app = d.app();
                        (
                            app.session.state.timeline_ui.track_slider_snap,
                            if video { app.ui.timeline.video_track_h } else { app.ui.timeline.audio_track_h },
                            if video { app.ui.timeline.v_scroll } else { app.ui.timeline.a_scroll },
                        )
                    };
                    let closest = d
                        .app()
                        .tl
                        .layout
                        .as_ref()
                        .map(|layout| {
                            layout
                                .rows
                                .iter()
                                .filter(|r| {
                                    matches!((video, r.kind), (true, filmcraft_project::TrackKind::Video) | (false, filmcraft_project::TrackKind::Audio))
                                })
                                .map(|r| {
                                    let line = if video { r.rect.min.y + 1.0 } else { r.rect.max.y - 1.0 };
                                    (line - snap_edge).abs()
                                })
                                .fold(f32::INFINITY, f32::min)
                        })
                        .unwrap_or(f32::INFINITY);
                    let boundaries = d
                        .app()
                        .tl
                        .layout
                        .as_ref()
                        .map(|layout| {
                            layout
                                .rows
                                .iter()
                                .filter(|r| {
                                    matches!((video, r.kind), (true, filmcraft_project::TrackKind::Video) | (false, filmcraft_project::TrackKind::Audio))
                                })
                                .map(|r| format!("{}:{:.2}", r.index, if video { r.rect.min.y + 1.0 } else { r.rect.max.y }))
                                .collect::<Vec<_>>()
                                .join(",")
                        })
                        .unwrap_or_default();
                    assert_eq!(
                        reached,
                        vec![0, 1, 2],
                        "height={window_height}, tracks={count}, video={video}, split={split}, snap={snap}, track_h={track_h}, scroll={scroll}, view_edge={view_edge}, closest={closest}, boundaries={boundaries}"
                    );
                }
            }
        }
    }
}

#[test]
fn track_snap_uses_scrolled_rows_and_survives_release() {
    for video in [true, false] {
        let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
        d.exec("sequence.addTracks", json!({"video": 5, "audio": 5}));
        d.app().session.state.timeline_ui.track_slider_decouple = true;
        d.app().session.state.timeline_ui.track_slider_snap = true;
        d.app().ui.timeline.video_track_h = 100.0;
        d.app().ui.timeline.audio_track_h = 100.0;
        d.app().ui.timeline.v_scroll = 80.0;
        d.app().ui.timeline.a_scroll = 80.0;
        d.frames(4);
        assert!(if video { d.app().ui.timeline.v_scroll > 0.5 } else { d.app().ui.timeline.a_scroll > 0.5 });
        let id = if video { "timeline.trackSlider.video.topCircle" } else { "timeline.trackSlider.audio.bottomCircle" };
        let (view_min, view_max) = track_view(&mut d, video);
        let snap_edge = if video { view_min } else { view_max + 2.0 };
        let c = d.rect(id);
        let (x, start) = (c[0] + c[2] / 2.0, c[1] + c[3] / 2.0);
        d.press(x, start);
        let mut found = false;
        for step in 1..=100 {
            let y = start + if video { -(step as f32) } else { step as f32 };
            d.move_to(x, y);
            let layout = d.app().tl.layout.as_ref().unwrap();
            let matched = layout
                .rows
                .iter()
                .filter(|r| matches!((video, r.kind), (true, filmcraft_project::TrackKind::Video) | (false, filmcraft_project::TrackKind::Audio)))
                .any(|r| {
                    let line = if video { r.rect.min.y + 1.0 } else { r.rect.max.y };
                    (line - snap_edge).abs() < 0.02
                });
            if matched {
                d.release(x, y);
                d.frames(3);
                let layout = d.app().tl.layout.as_ref().unwrap();
                let stays = layout
                    .rows
                    .iter()
                    .filter(|r| matches!((video, r.kind), (true, filmcraft_project::TrackKind::Video) | (false, filmcraft_project::TrackKind::Audio)))
                    .any(|r| {
                        let line = if video { r.rect.min.y + 1.0 } else { r.rect.max.y };
                        (line - snap_edge).abs() < 0.02
                    });
                assert!(stays, "line moved off the viewport edge after release: video={video}");
                found = true;
                break;
            }
        }
        assert!(found, "no scrolled track boundary reached the viewport edge: video={video}");
    }
}

#[test]
fn twelve_tracks_zoom_out_without_scrolling_to_the_middle() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    d.exec("sequence.addTracks", json!({"video": 9, "audio": 9}));
    d.app().ui.timeline.audio_track_h = 100.0;
    d.app().ui.timeline.a_scroll = 0.0;
    d.frames(4);
    let c = d.rect("timeline.trackSlider.audio.bottomCircle");
    let bar = d.rect("timeline.trackSlider.audio.track");
    let (x, y) = (c[0] + c[2] / 2.0, c[1] + c[3] / 2.0);
    d.press(x, y);
    d.move_to(x, bar[1] + bar[3] + 30.0);
    d.release(x, bar[1] + bar[3] + 30.0);
    assert!(d.app().ui.timeline.a_scroll < 0.1, "zooming out changed the scroll position: {}", d.app().ui.timeline.a_scroll);
}

#[test]
fn decoupled_track_edges_keep_opposite_edge_fixed_in_tall_timeline() {
    for video in [true, false] {
        for drag_top in [true, false] {
            let mut d = Driver::sized_with_step(1600.0, 1800.0, 1.0 / 60.0);
            d.app().session.state.timeline_ui.track_slider_decouple = true;
            d.app().session.state.timeline_ui.track_slider_snap = false;
            d.app().ui.timeline.video_track_h = 22.0;
            d.app().ui.timeline.audio_track_h = 22.0;
            d.frames(4);
            let name = if video { "video" } else { "audio" };
            let thumb_id = format!("timeline.trackSlider.{name}");
            let circle_id = format!("timeline.trackSlider.{name}.{}Circle", if drag_top { "top" } else { "bottom" });
            let before = d.rect(&thumb_id);
            let c = d.rect(&circle_id);
            let (x, y) = (c[0] + c[2] / 2.0, c[1] + c[3] / 2.0);
            let target = y + if drag_top { 15.0 } else { -15.0 };
            let fixed = if drag_top { before[1] + before[3] } else { before[1] };
            d.press(x, y);
            d.move_to(x, target);
            let during = d.rect(&thumb_id);
            let at = if drag_top { during[1] + during[3] } else { during[1] };
            assert!((at - fixed).abs() < 0.1, "opposite edge moved during drag: video={video}, top={drag_top}, {before:?} {during:?}");
            d.release(x, target);
            d.frames(3);
            let after = d.rect(&thumb_id);
            let at = if drag_top { after[1] + after[3] } else { after[1] };
            assert!((at - fixed).abs() < 0.1, "opposite edge moved on release: video={video}, top={drag_top}, {before:?} {after:?}");
        }
    }
}

#[test]
fn inner_circle_snaps_scrolled_tracks_to_clip_edge() {
    for video in [true, false] {
        for decouple in [true] {
            let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
            d.exec("sequence.addTracks", json!({"video": 5, "audio": 5}));
            d.app().session.state.timeline_ui.track_slider_decouple = decouple;
            d.app().session.state.timeline_ui.track_slider_snap = true;
            d.app().ui.timeline.video_track_h = 100.0;
            d.app().ui.timeline.audio_track_h = 100.0;
            d.app().ui.timeline.v_scroll = 80.0;
            d.app().ui.timeline.a_scroll = 80.0;
            d.frames(4);
            let id = if video { "timeline.trackSlider.video.bottomCircle" } else { "timeline.trackSlider.audio.topCircle" };
            let (view_min, view_max) = track_view(&mut d, video);
            let edge = if video { view_max } else { view_min };
            let offset = if video { -0.5 } else { 0.5 };
            let fixed_view = ((view_max - view_min) + 80.0 + offset) / 100.0;
            let c = d.rect(id);
            let (x, start) = (c[0] + c[2] / 2.0, c[1] + c[3] / 2.0);
            d.press(x, start);
            let mut found = false;
            for step in 1..=90 {
                let y = start + if video { -(step as f32) } else { step as f32 };
                d.move_to(x, y);
                let layout = d.app().tl.layout.as_ref().unwrap();
                let matched = layout
                    .rows
                    .iter()
                    .filter(|r| matches!((video, r.kind), (true, filmcraft_project::TrackKind::Video) | (false, filmcraft_project::TrackKind::Audio)))
                    .any(|r| {
                        let clip_edge = if video { r.rect.max.y - 1.0 } else { r.rect.min.y + 1.0 };
                        (clip_edge - edge).abs() < 0.02
                    });
                if matched {
                    d.release(x, y);
                    d.frames(3);
                    let (height, scroll) = if video {
                        (d.app().ui.timeline.video_track_h, d.app().ui.timeline.v_scroll)
                    } else {
                        (d.app().ui.timeline.audio_track_h, d.app().ui.timeline.a_scroll)
                    };
                    let after = ((view_max - view_min) + scroll + offset) / height;
                    assert!((after - fixed_view).abs() < 0.0001, "opposite visible edge moved while snapping: {fixed_view} -> {after}");
                    let layout = d.app().tl.layout.as_ref().unwrap();
                    assert!(
                        layout
                            .rows
                            .iter()
                            .filter(|r| matches!((video, r.kind), (true, filmcraft_project::TrackKind::Video) | (false, filmcraft_project::TrackKind::Audio)))
                            .any(|r| {
                                let clip_edge = if video { r.rect.max.y - 1.0 } else { r.rect.min.y + 1.0 };
                                (clip_edge - edge).abs() < 0.02
                            }),
                        "inner snap moved away on release"
                    );
                    found = true;
                    break;
                }
            }
            assert!(found, "inner circle found no snap: video={video}, decouple={decouple}");
        }
    }
}

#[test]
fn audio_outer_snap_reaches_the_reveal_and_keeps_the_inner_edge_fixed() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    d.exec("sequence.addTracks", json!({"video": 5, "audio": 9}));
    d.app().session.state.timeline_ui.track_slider_decouple = true;
    d.app().session.state.timeline_ui.track_slider_snap = true;
    d.app().ui.timeline.audio_track_h = 120.0;
    d.frames(4);
    let (view_min, view_max) = track_view(&mut d, false);
    let window = view_max - view_min;
    let slots = d.app().session.active_sequence().unwrap().audio_tracks.len() as f32 + 1.0 / 3.0;
    let height = d.app().ui.timeline.audio_track_h;
    d.app().ui.timeline.a_scroll = (slots * height - window).max(0.0);
    d.frames(3);

    // Pin the actual content coordinate at the top of the audio edit area, not the scrollbar's circle.
    let fixed_content_top = (d.app().ui.timeline.a_scroll + 0.5) / d.app().ui.timeline.audio_track_h;
    let outer = d.rect("timeline.trackSlider.audio.bottomCircle");
    let (x, start) = (outer[0] + outer[2] / 2.0, outer[1] + outer[3] / 2.0);
    d.press(x, start);
    d.move_to(x, start + 80.0);
    d.release(x, start + 80.0);
    d.frames(3);

    let after_height = d.app().ui.timeline.audio_track_h;
    let after_scroll = d.app().ui.timeline.a_scroll;
    let reveal = (window + after_scroll + 2.0) / after_height;
    let after_content_top = (after_scroll + 0.5) / after_height;
    assert!(
        (after_content_top - fixed_content_top).abs() < 0.0001,
        "outer-edge snapping moved the actual visible content coordinate: {fixed_content_top} -> {after_content_top}"
    );
    assert!(reveal > slots - 0.05, "audio outer edge should snap to the one-third reveal: q={reveal}, tracks={}", slots - 1.0 / 3.0);
}

#[test]
fn audio_outer_snap_to_another_track_keeps_the_actual_visible_top_fixed() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    d.harness.ctx.set_pixels_per_point(1.25);
    d.exec("sequence.addTracks", json!({"video": 5, "audio": 9}));
    d.app().session.state.timeline_ui.track_slider_decouple = true;
    d.app().session.state.timeline_ui.track_slider_snap = true;
    d.app().ui.timeline.audio_track_h = 120.0;
    d.frames(4);
    let (view_min, view_max) = track_view(&mut d, false);
    let window = view_max - view_min;
    let start_height = d.app().ui.timeline.audio_track_h;
    d.app().ui.timeline.a_scroll = (2.0 * start_height - window).max(0.0);
    d.frames(3);
    let fixed_content_top = (d.app().ui.timeline.a_scroll + 0.5) / d.app().ui.timeline.audio_track_h;
    let inner_before = d.rect("timeline.trackSlider.audio.topCircle");
    let inner_center_x = inner_before[0] + inner_before[2] / 2.0;
    let painted_inner_center_y = |d: &Driver, near_y: f32| {
        d.harness
            .output()
            .shapes
            .iter()
            .filter_map(|shape| match &shape.shape {
                egui::Shape::Circle(circle) if circle.radius >= 5.9 && (circle.center.x - inner_center_x).abs() < 0.1 => Some(circle.center.y),
                _ => None,
            })
            .min_by(|a, b| (a - near_y).abs().total_cmp(&(b - near_y).abs()))
            .expect("painted inner audio handle")
    };
    let inner_center_y = painted_inner_center_y(&d, inner_before[1] + inner_before[3] / 2.0);
    let pixels_per_point = d.harness.ctx.pixels_per_point();
    let inner_center_pixel = (inner_center_y * pixels_per_point).round();
    let outer = d.rect("timeline.trackSlider.audio.bottomCircle");
    let (x, start) = (outer[0] + outer[2] / 2.0, outer[1] + outer[3] / 2.0);
    d.press(x, start);

    let mut matched_tracks = Vec::new();
    let mut end_y = start;
    // Move in normal pointer-sized packets instead of one point per frame; small increments hid
    // the one-pixel snap jump that can occur when several events land between UI frames.
    for step in 1..=20 {
        let y = start + step as f32 * 8.0;
        end_y = y;
        d.move_to(x, y);
        let inner_center_after = painted_inner_center_y(&d, inner_center_y);
        let active_mode = d.harness.ctx.data(|data| data.get_temp::<u8>(egui::Id::unique(("tl-vs-mode", "as"))));
        let (track_height, track_scroll) = {
            let view = &d.app().ui.timeline;
            (view.audio_track_h, view.a_scroll)
        };
        assert!(
            (inner_center_after - inner_center_y).abs() < 0.1,
            "inner audio handle jumped during outer-edge snapping at step {step}: {inner_center_y} -> {inner_center_after}, mode={active_mode:?}, height={track_height}, scroll={track_scroll}"
        );
        assert_eq!(
            (inner_center_after * pixels_per_point).round(),
            inner_center_pixel,
            "inner audio handle moved a rendered pixel during outer-edge snapping at step {step}: {inner_center_pixel} -> {}",
            (inner_center_after * pixels_per_point).round()
        );
        let visible_content_top = (d.app().ui.timeline.a_scroll + 0.5) / d.app().ui.timeline.audio_track_h;
        assert!(
            (visible_content_top - fixed_content_top).abs() < 0.0001,
            "audio content cut moved at snap step {step}: {fixed_content_top} -> {visible_content_top}"
        );
        let layout = d.app().tl.layout.as_ref().unwrap();
        let matched_track = layout
            .rows
            .iter()
            .filter(|row| row.kind == filmcraft_project::TrackKind::Audio)
            .find(|row| (row.rect.max.y - (view_max + 2.0)).abs() < 0.02)
            .map(|row| row.index);
        if let Some(track) = matched_track
            && matched_tracks.last() != Some(&track)
        {
            matched_tracks.push(track);
            if matched_tracks.len() >= 3 {
                break;
            }
        }
    }
    d.release(x, end_y);
    d.frames(3);

    let inner_center_at_rest = painted_inner_center_y(&d, inner_center_y);
    let inner_rest_rect = d.rect("timeline.trackSlider.audio.topCircle");
    let rest_view = (d.app().ui.timeline.audio_track_h, d.app().ui.timeline.a_scroll);
    let after_content_top = (d.app().ui.timeline.a_scroll + 0.5) / d.app().ui.timeline.audio_track_h;
    assert!(matched_tracks.len() >= 3, "one drag must visit three distinct track snaps at the actual edit-area cutoff: {matched_tracks:?}");
    assert!(
        (inner_center_at_rest - inner_center_y).abs() < 0.1,
        "inner audio handle jumped when the outer drag ended: {inner_center_y} -> {inner_center_at_rest}; rect={inner_before:?}->{inner_rest_rect:?}, view={start_height:?},{fixed_content_top:?}->{rest_view:?}"
    );
    assert_eq!(
        (inner_center_at_rest * pixels_per_point).round(),
        inner_center_pixel,
        "inner audio handle moved a rendered pixel when the outer drag ended: {inner_center_pixel} -> {}",
        (inner_center_at_rest * pixels_per_point).round()
    );
    assert!(
        (after_content_top - fixed_content_top).abs() < 0.0001,
        "snapping the outer edge moved the actual content cut at the top of the edit area: {fixed_content_top} -> {after_content_top}"
    );
}

#[test]
fn fine_track_drag_ignores_all_snap_detents() {
    let drag = |snap: bool| {
        let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
        d.exec("sequence.addTracks", json!({"video": 9, "audio": 9}));
        d.app().session.state.timeline_ui.track_slider_snap = snap;
        d.app().session.state.timeline_ui.track_slider_shift_drag_speed = 0.1;
        d.app().ui.timeline.audio_track_h = 40.0;
        d.frames(4);
        let c = d.rect("timeline.trackSlider.audio.bottomCircle");
        let (x, y) = (c[0] + c[2] / 2.0, c[1] + c[3] / 2.0);
        d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x, "y": y + 30.0}, "steps": 15, "modifiers": {"shift": true}}));
        d.frames(3);
        d.app().ui.timeline.audio_track_h
    };
    let without_snap = drag(false);
    let with_snap = drag(true);
    assert!((with_snap - without_snap).abs() < 0.01, "fine drag changed when snapping was enabled: {with_snap} vs {without_snap}");
    assert!(with_snap > 39.0, "fine drag jumped to the minimum height: {with_snap}");
}

#[test]
fn track_separator_header_and_timeline_share_vertical_clip() {
    for video in [true, false] {
        let mut d = Driver::demo();
        let layout = d.app().tl.layout.as_ref().unwrap();
        let content_x = layout.content.min.x;
        let row = layout
            .rows
            .iter()
            .find(|r| {
                r.index == if video { 1 } else { 0 }
                    && matches!((video, r.kind), (true, filmcraft_project::TrackKind::Video) | (false, filmcraft_project::TrackKind::Audio))
            })
            .unwrap();
        let y = row.rect.max.y - 0.5;
        let separator = d.app().tokens.separator;
        let mut header = None;
        let mut timeline = None;
        for shape in &d.harness.output().shapes {
            if let egui::Shape::LineSegment { points, stroke } = &shape.shape
                && stroke.color == separator
                && (points[0].y - y).abs() < 0.01
                && (points[1].x - points[0].x).abs() > 50.0
            {
                if points[0].x < content_x - 1.0 && points[1].x <= content_x + 0.1 {
                    header = Some(shape.clip_rect);
                }
                if (points[0].x - content_x).abs() < 0.1 {
                    timeline = Some(shape.clip_rect);
                }
            }
        }
        let header = header.expect("painted header separator");
        let timeline = timeline.expect("painted timeline separator");
        assert!((header.min.y - timeline.min.y).abs() < 0.01 && (header.max.y - timeline.max.y).abs() < 0.01, "different clipping: {header:?} / {timeline:?}");
    }
}

#[test]
fn decoupled_scrolled_track_slider_holds_other_edge_while_snapping() {
    for video in [true, false] {
        for drag_top in [true, false] {
            for snap in [true, false] {
                let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
                d.exec("sequence.addTracks", json!({"video": 5, "audio": 5}));
                d.app().session.state.timeline_ui.track_slider_decouple = true;
                d.app().session.state.timeline_ui.track_slider_snap = true;
                d.app().session.state.timeline_ui.track_slider_snap = snap;
                d.app().ui.timeline.video_track_h = 100.0;
                d.app().ui.timeline.audio_track_h = 100.0;
                d.app().ui.timeline.v_scroll = 80.0;
                d.app().ui.timeline.a_scroll = 80.0;
                d.frames(4);
                let name = if video { "video" } else { "audio" };
                let thumb_id = format!("timeline.trackSlider.{name}");
                let circle_id = format!("timeline.trackSlider.{name}.{}Circle", if drag_top { "top" } else { "bottom" });
                let before = d.rect(&thumb_id);
                let fixed = if drag_top { before[1] + before[3] } else { before[1] };
                let circle = d.rect(&circle_id);
                let (x, y) = (circle[0] + circle[2] / 2.0, circle[1] + circle[3] / 2.0);
                let target = y + if drag_top { 5.0 } else { -5.0 };
                d.press(x, y);
                d.move_to(x, target);
                d.release(x, target);
                d.frames(3);
                let after = d.rect(&thumb_id);
                let other = if drag_top { after[1] + after[3] } else { after[1] };
                assert!((other - fixed).abs() < 0.1, "other edge moved: video={video} top={drag_top} snap={snap}: {before:?} -> {after:?}");
            }
        }
    }
}

#[test]
fn snapping_requires_decoupled_edges_and_explains_why() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    let slider = d.rect("timeline.trackSlider.video");
    d.click_at(slider[0] + slider[2] / 2.0, slider[1] + slider[3] / 2.0, "right");
    d.ok("ui.click", json!({"id": "timeline.trackSlider.settings.decoupleEdges"}));
    d.frames(2);
    assert!(!d.app().session.state.timeline_ui.track_slider_decouple);
    assert!(!d.app().session.state.timeline_ui.track_slider_snap);
    d.ok("ui.click", json!({"id": "timeline.trackSlider.settings.snapToTracks"}));
    d.frames(2);
    assert!(!d.app().session.state.timeline_ui.track_slider_snap);
    assert!(d.has("timeline.trackSlider.snapRequiresDecoupling.ok"));
    let ok = d.rect("timeline.trackSlider.snapRequiresDecoupling.ok");
    let layer = d.harness.ctx.layer_id_at(egui::pos2(ok[0] + ok[2] / 2.0, ok[1] + ok[3] / 2.0)).expect("popup layer");
    assert_eq!(layer, egui::LayerId::new(egui::Order::Foreground, egui::Id::unique("timeline-snap-requires-decoupling-window")));
    d.ok("engine.execute", json!({"command": "file.newBin", "params": {"name": "Unsaved"}}));
    d.ok("ui.menu.invoke", json!({"id": "file.closeProject"}));
    d.frames(3);
    let save = d.rect("closeProject.save");
    let layer = d.harness.ctx.layer_id_at(egui::pos2(save[0] + save[2] / 2.0, save[1] + save[3] / 2.0)).expect("save layer");
    assert_eq!(layer.order, egui::Order::Tooltip, "Save Project must remain above slider popups");
    d.ok("ui.click", json!({"id": "closeProject.cancel"}));
    d.ok("ui.click", json!({"id": "timeline.trackSlider.snapRequiresDecoupling.ok"}));
    d.frames(2);
    assert!(!d.has("timeline.trackSlider.snapRequiresDecoupling.ok"));
}

#[test]
fn simple_left_click_binding_is_rejected() {
    let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
    let bar = d.rect("timeline.zoomBar.track");
    d.click_at(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0, "right");
    let row = d.rect("timeline.zoomBar.settings.controls");
    d.move_to(row[0] + 10.0, row[1] + row[3] / 2.0);
    d.frames(2);
    d.ok("ui.click", json!({"id": "timeline.zoomBar.settings.controlsFlyout.box0"}));
    let before = d.app().session.state.timeline_ui.control_center_on_playhead.clone();
    d.click_at(row[0] + 20.0, row[1] + row[3] / 2.0, "left");
    d.frames(30);
    assert_eq!(d.app().session.state.timeline_ui.control_center_on_playhead, before);
    assert!(d.has("timeline.rightClickDialog.ok"));
}

#[test]
fn decoupled_drag_keeps_opposite_visible_track_coordinate_fixed() {
    for video in [true, false] {
        for outer in [true, false] {
            for snap in [true, false] {
                let mut d = Driver::sized_with_step(1600.0, 980.0, 1.0 / 60.0);
                d.exec("sequence.addTracks", json!({"video": 5, "audio": 5}));
                d.app().session.state.timeline_ui.track_slider_decouple = true;
                d.app().session.state.timeline_ui.track_slider_snap = snap;
                d.app().ui.timeline.video_track_h = 100.0;
                d.app().ui.timeline.audio_track_h = 100.0;
                d.frames(4);
                let (view_min, view_max) = track_view(&mut d, video);
                let window = view_max - view_min;
                let offset = if video { -0.5 } else { 0.5 };
                let aligned_scroll = if outer { 100.0 - offset } else { 200.0 - window - offset };
                if video {
                    d.app().ui.timeline.v_scroll = aligned_scroll;
                } else {
                    d.app().ui.timeline.a_scroll = aligned_scroll;
                }
                d.frames(3);
                let (height, scroll) = if video {
                    (d.app().ui.timeline.video_track_h, d.app().ui.timeline.v_scroll)
                } else {
                    (d.app().ui.timeline.audio_track_h, d.app().ui.timeline.a_scroll)
                };
                let anchor = ((if outer { scroll } else { window + scroll }) + offset) / height;
                let fixed_screen_edge = if video == outer { view_max } else { view_min };
                let circle_id = match (video, outer) {
                    (true, true) => "timeline.trackSlider.video.topCircle",
                    (true, false) => "timeline.trackSlider.video.bottomCircle",
                    (false, true) => "timeline.trackSlider.audio.bottomCircle",
                    (false, false) => "timeline.trackSlider.audio.topCircle",
                };
                let c = d.rect(circle_id);
                let (x, y) = (c[0] + c[2] / 2.0, c[1] + c[3] / 2.0);
                let direction = if video == outer { 1.0 } else { -1.0 };
                d.press(x, y);
                d.move_to(x, y + direction * 12.0);
                d.release(x, y + direction * 12.0);
                d.frames(3);
                let (height, scroll) = if video {
                    (d.app().ui.timeline.video_track_h, d.app().ui.timeline.v_scroll)
                } else {
                    (d.app().ui.timeline.audio_track_h, d.app().ui.timeline.a_scroll)
                };
                let after = ((if outer { scroll } else { window + scroll }) + offset) / height;
                assert!((height - 100.0).abs() > 0.01, "drag did not resize");
                assert!((after - anchor).abs() < 0.0001, "visible edge shifted: video={video}, outer={outer}, snap={snap}, {anchor} -> {after}");
                let index = match (video, outer) {
                    (true, true) | (false, false) => 1,
                    (true, false) => 2,
                    (false, true) => 0,
                };
                let layout = d.app().tl.layout.as_ref().unwrap();
                let row = layout
                    .rows
                    .iter()
                    .find(|r| {
                        r.index == index
                            && matches!((video, r.kind), (true, filmcraft_project::TrackKind::Video) | (false, filmcraft_project::TrackKind::Audio))
                    })
                    .unwrap();
                let line = row.rect.max.y - 0.5;
                assert!(
                    (line - fixed_screen_edge).abs() < 0.02,
                    "painted line moved at the fixed viewport edge: video={video}, outer={outer}, snap={snap}, line={line}, edge={fixed_screen_edge}"
                );
            }
        }
    }
}

//! Headless UI tests of scrolling the Timeline: what the wheel does with each modifier, how far
//! the view can go, and the zoom scroll bar. Premiere Pro 26.5.2 on macOS, checked in the app: the
//! wheel moves the tracks up and down, Cmd + wheel moves the Timeline sideways, Option + wheel
//! zooms, and the view stops ten minutes past the end of the sequence.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::settings::ZoomAnchor;
use filmcraft_time::Tick;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use filmcraft_ui_egui::panels::timeline::{max_scroll, timeline_extent};
use filmcraft_ui_egui::panels::timeline_zoom::wheel_zoom_factor;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
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

    /// Turn the wheel over the tracks (`fy`: 0.2 is among the video tracks, 0.8 among the audio
    /// tracks), with modifiers.
    fn wheel(&mut self, fy: f64, dx: f64, dy: f64, modifiers: Value) {
        self.wheel_on("timeline.tracks", fy, dx, dy, modifiers);
    }

    /// The same over another part of the Timeline: `timeline.headers` is the column of track headers.
    fn wheel_on(&mut self, id: &str, fy: f64, dx: f64, dy: f64, modifiers: Value) {
        self.ok("ui.scroll", json!({"id": id, "fx": 0.5, "fy": fy, "dx": dx, "dy": dy, "modifiers": modifiers}));
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
}

fn none() -> Value {
    json!({})
}

#[test]
fn the_wheel_moves_the_tracks_up_and_down() {
    let mut d = Driver::tall();
    // over the video tracks: wheeling up brings the higher tracks in (Video 1 is at the bottom)
    d.wheel(0.2, 0.0, 40.0, none());
    assert_eq!(d.view(), (4.0, 50.0, 40.0, 0.0), "the video tracks moved, nothing else");
    d.wheel(0.2, 0.0, -25.0, none());
    assert_eq!(d.view(), (4.0, 50.0, 15.0, 0.0));
    // over the audio tracks: wheeling down brings the lower tracks in
    d.wheel(0.8, 0.0, -30.0, none());
    assert_eq!(d.view(), (4.0, 50.0, 15.0, 30.0));
    // neither half goes past its first or last track
    d.wheel(0.2, 0.0, -1e6, none());
    d.wheel(0.8, 0.0, 1e6, none());
    assert_eq!(d.view(), (4.0, 50.0, 0.0, 0.0));
    d.wheel(0.8, 0.0, -1e6, none());
    let (_, _, _, lowest) = d.view();
    assert!(lowest > 100.0 && lowest < 15.0 * 200.0, "{lowest}");
}

#[test]
fn cmd_wheel_moves_the_timeline_sideways_and_option_wheel_zooms() {
    let mut d = Driver::tall();
    // Cmd + wheel: sideways, a point of wheel for a point of Timeline; the tracks stay
    d.wheel(0.2, 0.0, -100.0, json!({"command": true}));
    assert_eq!(d.view(), (6.0, 50.0, 0.0, 0.0), "100 points at 50 points a second: 2 s later");
    d.wheel(0.8, 0.0, 250.0, json!({"command": true}));
    assert_eq!(d.view(), (1.0, 50.0, 0.0, 0.0));
    d.wheel(0.8, 0.0, 1e5, json!({"command": true}));
    assert_eq!(d.view().0, 0.0, "not before the start");
    // Option + wheel: zoom about the pointer; it is not what Cmd + wheel does
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
    // a sideways gesture (trackpad swipe, tilt wheel) is sideways whatever is held; so is Shift
    d.ok("ui.set", json!({"timeline": {"pps": 50.0, "scroll": 4.0}}));
    d.frames(3);
    d.wheel(0.2, -50.0, 5.0, none());
    assert_eq!(d.view(), (5.0, 50.0, 0.0, 0.0));
    d.wheel(0.2, 0.0, -50.0, json!({"shift": true}));
    assert_eq!(d.view(), (6.0, 50.0, 0.0, 0.0));
}

/// Settings ▸ Timeline ▸ Timeline Mouse Scrolling "Horizontal": the wheel moves the Timeline
/// sideways and Cmd + wheel moves the tracks.
#[test]
fn the_mouse_scrolling_setting_swaps_wheel_and_cmd_wheel() {
    let mut d = Driver::tall();
    assert_eq!(d.app().session.prefs.navigation.mouse_scrolling, "vertical", "Premiere's default on macOS");
    d.app().session.prefs.navigation.mouse_scrolling = "horizontal".into();
    d.wheel(0.2, 0.0, -100.0, none());
    assert_eq!(d.view(), (6.0, 50.0, 0.0, 0.0));
    d.wheel(0.2, 0.0, 40.0, json!({"command": true}));
    assert_eq!(d.view(), (6.0, 50.0, 40.0, 0.0));
    d.wheel(0.2, 0.0, 20.0, json!({"alt": true}));
    assert!((d.view().1 - 60.0).abs() < 1e-6, "Option + wheel zooms either way");
}

/// Settings ▸ Timeline ▸ Timeline Mouse Scrolling "Zoom": the bare wheel over the tracks zooms about
/// the pointer (the time under it stays under it) and no longer moves the tracks. Off by default.
#[test]
fn the_zoom_wheel_setting_makes_the_bare_wheel_zoom_about_the_pointer() {
    let mut d = Driver::tall();
    assert_eq!(d.app().session.prefs.navigation.mouse_scrolling, "vertical", "the classic wheel by default");
    assert_eq!(d.app().session.prefs.navigation.zoom_anchor, ZoomAnchor::Cursor);
    d.app().session.prefs.navigation.mouse_scrolling = "zoom".into();
    let tracks = d.rect("timeline.tracks");
    let under_pointer = 4.0 + (tracks[2] as f64 * 0.5) / 50.0;
    d.wheel(0.2, 0.0, 40.0, none());
    let (scroll, pps, v, a) = d.view();
    assert!((pps - 50.0 * wheel_zoom_factor(40.0)).abs() < 1e-6, "a notch zooms in by a quarter: {pps}");
    assert!((scroll + (tracks[2] as f64 * 0.5) / pps - under_pointer).abs() < 0.02, "the time under the pointer stays there: {scroll}");
    assert_eq!((v, a), (0.0, 0.0), "the tracks did not move");
    // the opposite turn undoes it
    d.wheel(0.2, 0.0, -40.0, none());
    let (scroll, pps, ..) = d.view();
    assert!((pps - 50.0).abs() < 1e-6, "{pps}");
    assert!((scroll - 4.0).abs() < 0.02, "{scroll}");
    // the zoom stays within the Timeline's limits
    for _ in 0..40 {
        d.wheel(0.2, 0.0, 1e4, none());
    }
    assert!(d.view().1 <= 24_000.0 + 1e-6);
}

/// With the wheel on the zoom, and Settings ▸ Navigation set so that Option or Shift + wheel moves the
/// Timeline sideways and Cmd + wheel moves the tracks; a sideways swipe still scrolls sideways.
#[test]
fn with_the_wheel_on_zoom_option_and_shift_wheel_scroll_sideways_and_cmd_wheel_moves_the_tracks() {
    let mut d = Driver::tall();
    d.app().session.prefs.navigation.mouse_scrolling = "zoom".into();
    d.app().session.prefs.navigation.alt_scrolling = "horizontal".into();
    d.app().session.prefs.navigation.ctrl_scrolling = "vertical".into();
    d.wheel(0.2, 0.0, -100.0, json!({"alt": true}));
    assert_eq!(d.view(), (6.0, 50.0, 0.0, 0.0), "Option + wheel: 100 points at 50 points a second is 2 s, and no zoom");
    d.wheel(0.2, 0.0, 50.0, json!({"shift": true}));
    assert_eq!(d.view(), (5.0, 50.0, 0.0, 0.0), "Shift + wheel: sideways too");
    d.wheel(0.2, 0.0, 40.0, json!({"command": true}));
    assert_eq!(d.view(), (5.0, 50.0, 40.0, 0.0), "Cmd + wheel: the tracks, not sideways and not the zoom");
    d.wheel(0.2, -50.0, 5.0, none());
    assert_eq!(d.view(), (6.0, 50.0, 40.0, 0.0), "a sideways swipe scrolls sideways");
}

/// Settings ▸ Navigation ▸ "Zoom Anchor Point" "Playhead Position": the playhead stays where it is on
/// screen while the zoom happens around it; with "Cursor position" it moves. A playhead out of view
/// cannot hold the pivot: the pointer does.
#[test]
fn the_playhead_anchor_keeps_the_playhead_in_place_while_zooming() {
    let mut d = Driver::tall();
    d.app().session.prefs.navigation.mouse_scrolling = "zoom".into();
    d.app().session.set_playhead(Tick::from_seconds_f64(10.0));
    d.frames(4);
    assert_eq!(d.view(), (4.0, 50.0, 0.0, 0.0), "a playhead in view does not scroll the Timeline");
    let tracks = d.rect("timeline.tracks");
    // the playhead sits on a frame, so a little off 10 s: work from where it really is
    let ph = d.app().session.playhead().seconds();
    let playhead_x = |d: &mut Driver| {
        let (scroll, pps, ..) = d.view();
        (ph - scroll) * pps
    };
    let start = playhead_x(&mut d);
    assert!((start - 300.0).abs() < 2.0, "about 6 s from the left edge at 50 points a second: {start}");
    // the pointer is somewhere else: zooming about it moves the playhead
    d.wheel(0.2, 0.0, 40.0, none());
    let moved = playhead_x(&mut d);
    assert!((moved - start).abs() > 10.0, "about the cursor the playhead moves: {start} -> {moved}");
    // about the playhead it stays put
    d.ok("ui.set", json!({"timeline": {"pps": 50.0, "scroll": 4.0}}));
    d.frames(4);
    d.app().session.prefs.navigation.zoom_anchor = ZoomAnchor::Playhead;
    d.wheel(0.2, 0.0, 40.0, none());
    assert!((d.view().1 - 50.0 * wheel_zoom_factor(40.0)).abs() < 1e-6);
    let held = playhead_x(&mut d);
    assert!((held - start).abs() < 0.5, "the playhead stayed where it was: {start} -> {held}");
    d.wheel(0.2, 0.0, -40.0, none());
    let held = playhead_x(&mut d);
    assert!((held - start).abs() < 0.5, "and again zooming out: {start} -> {held}");
    // out of view (left of it), the playhead cannot be the pivot: the time under the pointer stays under it
    d.app().session.set_playhead(Tick::from_seconds_f64(1.0));
    d.frames(4);
    d.ok("ui.set", json!({"timeline": {"pps": 50.0, "scroll": 4.0}}));
    d.frames(4);
    let under_pointer = 4.0 + (tracks[2] as f64 * 0.5) / 50.0;
    d.wheel(0.2, 0.0, 40.0, none());
    let (scroll, pps, ..) = d.view();
    assert!((scroll + (tracks[2] as f64 * 0.5) / pps - under_pointer).abs() < 0.02, "the pointer held the pivot: {scroll}");
}

/// The anchor setting also applies to the Option + wheel zoom (the wheel's own zoom with the
/// switch off); with the default anchor that zoom is the one it always was.
#[test]
fn the_playhead_anchor_applies_to_option_wheel_zoom_too() {
    let mut d = Driver::tall();
    d.app().session.prefs.navigation.zoom_anchor = ZoomAnchor::Playhead;
    d.app().session.set_playhead(Tick::from_seconds_f64(10.0));
    d.frames(4);
    let ph = d.app().session.playhead().seconds();
    let start = (ph - 4.0) * 50.0;
    d.wheel(0.2, 0.0, 20.0, json!({"alt": true}));
    let (scroll, pps, ..) = d.view();
    assert!((pps - 60.0).abs() < 1e-6, "Option + wheel keeps its own step: {pps}");
    assert!(((ph - scroll) * pps - start).abs() < 0.5, "the playhead stayed at {start} points: {}", (ph - scroll) * pps);
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
    d.wheel(0.2, 0.0, -1e7, json!({"command": true}));
    assert!((d.view().0 - limit).abs() < 1e-6, "{} vs {limit}", d.view().0);
    d.wheel(0.2, 0.0, -500.0, json!({"command": true}));
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
    assert!((thumb[0] - track[0]).abs() < 0.5 && thumb[2] >= 30.0 && thumb[2] < track[2] / 4.0, "{thumb:?} in {track:?}");
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

/// Ctrl + Shift + wheel over the video tracks sizes all of them, over the audio tracks all of those.
#[test]
fn the_wheel_can_resize_all_video_or_all_audio_tracks_by_the_pointer() {
    let mut d = Driver::tall();
    let (v0, a0) = (d.app().ui.timeline.video_track_h, d.app().ui.timeline.audio_track_h);
    let both = json!({"command": true, "shift": true});
    d.wheel(0.2, 0.0, 40.0, both.clone());
    let (v1, a1) = (d.app().ui.timeline.video_track_h, d.app().ui.timeline.audio_track_h);
    assert!(v1 > v0, "wheel up over the video tracks makes them taller: {v0} -> {v1}");
    assert_eq!(a1, a0, "the audio tracks stay as they were");
    d.wheel(0.8, 0.0, -40.0, both.clone());
    let (v2, a2) = (d.app().ui.timeline.video_track_h, d.app().ui.timeline.audio_track_h);
    assert_eq!(v2, v1, "the video tracks stay as they were");
    assert!(a2 < a1, "wheel down over the audio tracks makes them shorter: {a1} -> {a2}");
    for _ in 0..40 {
        d.wheel(0.8, 0.0, -1e5, both.clone());
    }
    assert!(d.app().ui.timeline.audio_track_h >= 20.0, "never smaller than the minimum");
}

/// Alt + wheel over a track header sizes that track alone, whatever "Alt + Scroll" is set to; over
/// the clips it zooms, as the setting says.
#[test]
fn alt_wheel_over_a_track_header_resizes_only_that_track() {
    let mut d = Driver::tall();
    d.app().session.prefs.navigation.alt_scrolling = "horizontal".into();
    let (v0, a0) = (d.app().ui.timeline.video_track_h, d.app().ui.timeline.audio_track_h);
    let before = d.view();
    d.wheel_on("timeline.headers", 0.2, 0.0, 40.0, json!({"alt": true}));
    let tv = &d.app().ui.timeline;
    assert_eq!((tv.video_track_h, tv.audio_track_h), (v0, a0), "the shared heights do not change");
    assert_eq!(tv.track_h_override.len(), 1, "one track has its own height");
    assert!(tv.track_h_override.values().all(|h| *h > v0));
    assert_eq!(d.view().1, before.1, "the zoom did not change");
    d.wheel(0.2, 0.0, 40.0, json!({"alt": true}));
    assert_eq!(d.app().ui.timeline.track_h_override.len(), 1, "over the clips Alt + wheel does not resize a track");
}

/// Ctrl + Shift + wheel resizes all video tracks, or all audio tracks, depending on the half the
/// pointer is in (over the headers or the clips); Ctrl + Shift + Alt + wheel does nothing.
#[test]
fn ctrl_shift_wheel_resizes_all_tracks_of_the_half_under_the_pointer() {
    let mut d = Driver::tall();
    let (v0, a0) = (d.app().ui.timeline.video_track_h, d.app().ui.timeline.audio_track_h);
    d.wheel(0.2, 0.0, 40.0, json!({"command": true, "shift": true, "alt": true}));
    assert_eq!((d.app().ui.timeline.video_track_h, d.app().ui.timeline.audio_track_h), (v0, a0), "Ctrl + Shift + Alt does nothing");
    let both = json!({"command": true, "shift": true});
    d.wheel(0.2, 0.0, 40.0, both.clone());
    let (v1, a1) = (d.app().ui.timeline.video_track_h, d.app().ui.timeline.audio_track_h);
    assert!(v1 > v0 && a1 == a0, "video tracks taller, audio untouched: {v0} -> {v1}, {a0} -> {a1}");
    d.wheel_on("timeline.headers", 0.8, 0.0, 40.0, both);
    assert!(d.app().ui.timeline.audio_track_h > a1, "over the audio track headers the audio tracks grow");
}

/// A platform may report a modified wheel sideways (Shift + wheel on Windows): set to Vertical (or
/// Zoom), it still acts, on the wheel's main axis.
#[test]
fn a_modified_wheel_reported_sideways_still_does_its_action() {
    let mut d = Driver::tall();
    d.exec("prefs.set", json!({"key": "navigation.shiftScrolling", "value": "vertical"}));
    d.wheel(0.2, 40.0, 0.0, json!({"shift": true}));
    let (t, pps, v, a) = d.view();
    assert!(v > 0.0, "Shift + a sideways wheel set to Vertical scrolls the video tracks: {v}");
    assert_eq!((t, pps, a), (4.0, 50.0, 0.0), "and nothing else");
    d.exec("prefs.set", json!({"key": "navigation.shiftScrolling", "value": "zoom"}));
    d.wheel(0.2, 40.0, 0.0, json!({"shift": true}));
    assert!(d.view().1 > 50.0, "set to Zoom it zooms");
}

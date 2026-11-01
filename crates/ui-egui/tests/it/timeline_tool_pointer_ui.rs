//! Headless UI tests of the tools' pointers in the Timeline: every tool but Selection shows its
//! own icon as the pointer (the system pointer hidden) wherever it acts in the tracks, the pointer
//! stays the system one where a tool does not act (the ruler, the work area bar, a locked clip),
//! and the Razor's preview line crosses the clip and the clips linked to it, or every track with
//! Shift.

use std::sync::mpsc::{Sender, channel};

use egui::{Color32, CursorIcon, Pos2, Shape, pos2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// A clip of the demo project on V1, linked to its audio.
const CITY: &str = "City_Night_Drive.mp4";

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project, snapping off so drags land exactly where the pointer goes. Frames are
    /// 1/60 s apart, as on a display: at the harness's default 1/4 s, a press held for four frames
    /// is already a drag by time (egui's 0.8 s click limit), wherever the pointer is.
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        session.execute("sequence.snap", json!({"on": false})).expect("snapping off");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        // `ui.elements` answers once the timeline has finished fitting the sequence
        d.ok("ui.elements", json!({"prefix": "timeline.clip."}));
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

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    /// Move the pointer to `at` with no button down, and let the Timeline react.
    fn hover(&mut self, at: Pos2) {
        self.harness.input_mut().events.push(egui::Event::PointerMoved(at));
        self.frames(2);
    }

    /// The cursor the last frame asked for.
    fn cursor(&self) -> CursorIcon {
        self.harness.output().platform_output.cursor_icon
    }

    /// How many white one point wide vertical lines the last frame drew: the Razor's preview line
    /// has one segment on each track it crosses.
    fn white_vertical_lines(&self) -> usize {
        fn count(shape: &Shape) -> usize {
            match shape {
                Shape::Vec(shapes) => shapes.iter().map(count).sum(),
                Shape::LineSegment { points, stroke } => usize::from(points[0].x == points[1].x && stroke.width == 1.0 && stroke.color == Color32::WHITE),
                _ => 0,
            }
        }
        self.harness.output().shapes.iter().map(|s| count(&s.shape)).sum()
    }
}

impl Driver {
    /// The clip named `name` on V1.
    fn v1_clip(&self, name: &str) -> u64 {
        let seq = self.app().session.active_sequence().expect("sequence");
        seq.video_tracks[0].items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("no {name} on V1")).id.0
    }
}

fn clip_center(d: &mut Driver, clip: u64) -> Pos2 {
    let id = format!("timeline.clip.{clip}");
    let els = d.ok("ui.elements", json!({"prefix": id}));
    let el = els.as_array().expect("elements").iter().find(|e| e["id"] == id.as_str()).unwrap_or_else(|| panic!("no {id}")).clone();
    let r: Vec<f32> = el["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
    pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
}

#[test]
fn the_editing_tools_draw_their_own_pointer_over_the_tracks() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let at = clip_center(&mut d, city);
    for tool in [
        "Razor",
        "Ripple",
        "Rolling",
        "RateStretch",
        "Slip",
        "Slide",
        "TrackSelectForward",
        "TrackSelectBackward",
        "Remix",
        "Hand",
        "Zoom",
        "Pen",
        "Type",
        "Rectangle",
        "Ellipse",
    ] {
        d.ok("ui.set", json!({"tool": tool}));
        d.frames(2);
        d.hover(at);
        assert_eq!(d.cursor(), CursorIcon::None, "{tool}: its own icon is the pointer, the system one hidden");
    }
    // only the Selection tool keeps the system arrow
    d.ok("ui.set", json!({"tool": "Selection"}));
    d.frames(2);
    d.hover(at);
    assert_eq!(d.cursor(), CursorIcon::Default, "Selection");
}

#[test]
fn the_razor_line_crosses_the_linked_clips_or_every_track_with_shift() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let (video, link) = {
        let seq = d.app().session.active_sequence().unwrap();
        let (track, item) = seq.find_item(ClipId(city)).unwrap();
        (track, item.link)
    };
    let partner = {
        let seq = d.app().session.active_sequence().unwrap();
        seq.audio_tracks.iter().find(|t| t.items.iter().any(|i| i.link == link && link.is_some())).map(|t| t.id)
    };
    let linked = filmcraft_ui_egui::panels::timeline::razor_line_tracks(d.app(), ClipId(city), false);
    assert!(linked.contains(&video), "the clip's own track");
    if let Some(audio) = partner {
        assert!(linked.contains(&audio), "the linked audio's track");
    }
    let every = filmcraft_ui_egui::panels::timeline::razor_line_tracks(d.app(), ClipId(city), true);
    let seq = d.app().session.active_sequence().unwrap();
    assert_eq!(every.len(), seq.video_tracks.len() + seq.audio_tracks.len(), "Shift: every track");
    assert!(every.len() > linked.len());
    // a locked track is not cut, so the line leaves it out, with or without Shift
    let locked = partner.unwrap_or(video);
    d.ok("engine.execute", json!({"command": "timeline.setTrack", "params": {"track": locked.0, "locked": true}}));
    d.frames(2);
    let linked = filmcraft_ui_egui::panels::timeline::razor_line_tracks(d.app(), ClipId(city), false);
    assert!(!linked.contains(&locked), "a locked partner track gets no line: {linked:?}");
    let shifted = filmcraft_ui_egui::panels::timeline::razor_line_tracks(d.app(), ClipId(city), true);
    assert!(!shifted.contains(&locked) && shifted.len() + 1 == every.len(), "Shift: every track but the locked one: {shifted:?}");
}

/// The tool's pointer stays on through the drag it starts, not only while hovering.
#[test]
fn the_tool_pointer_stays_on_during_a_drag() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let at = clip_center(&mut d, city);
    for tool in ["Slip", "Slide", "Hand"] {
        d.ok("ui.set", json!({"tool": tool}));
        d.frames(2);
        d.hover(at);
        let button = |pressed| egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        d.harness.input_mut().events.push(button(true));
        d.frames(1);
        for dx in [4.0, 10.0, 18.0, 26.0] {
            d.harness.input_mut().events.push(egui::Event::PointerMoved(at + egui::vec2(dx, 0.0)));
            d.frames(1);
        }
        assert_eq!(d.cursor(), CursorIcon::None, "{tool}: its icon is still the pointer mid-drag");
        d.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: at + egui::vec2(26.0, 0.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        d.frames(2);
    }
}

/// A trimming tool keeps its icon as the pointer through the drag of an edge, as it does over the
/// edge, instead of the trim arrow the Selection tool keeps.
#[test]
fn a_trimming_tool_keeps_its_pointer_through_an_edge_drag() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let id = format!("timeline.clip.{city}");
    let els = d.ok("ui.elements", json!({"prefix": id}));
    let el = els.as_array().expect("elements").iter().find(|e| e["id"] == id.as_str()).expect("clip").clone();
    let r: Vec<f32> = el["rect"].as_array().expect("rect").iter().map(|x| x.as_f64().unwrap_or_default() as f32).collect();
    let edge = pos2(r[0] + 2.0, r[1] + r[3] / 2.0);
    for (tool, shown) in [("Selection", CursorIcon::ResizeColumn), ("Ripple", CursorIcon::None), ("Rolling", CursorIcon::None)] {
        d.ok("ui.set", json!({"tool": tool}));
        d.frames(2);
        d.hover(edge);
        assert_eq!(d.cursor(), shown, "{tool}: the pointer over the edge");
        let button = |pressed| egui::Event::PointerButton { pos: edge, button: egui::PointerButton::Primary, pressed, modifiers: egui::Modifiers::NONE };
        d.harness.input_mut().events.push(button(true));
        d.frames(1);
        for dx in [4.0, 10.0, 18.0, 26.0] {
            d.harness.input_mut().events.push(egui::Event::PointerMoved(edge + egui::vec2(dx, 0.0)));
            d.frames(1);
        }
        assert_eq!(d.cursor(), shown, "{tool}: the pointer mid-drag");
        d.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: edge + egui::vec2(26.0, 0.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        d.frames(2);
    }
}

/// An Option-drag copies, and its Copy pointer is what shows, whatever the tool.
#[test]
fn an_option_drag_keeps_the_copy_pointer() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let at = clip_center(&mut d, city);
    d.ok("ui.set", json!({"tool": "TrackSelectForward"}));
    d.frames(2);
    d.hover(at);
    let alt = egui::Modifiers::ALT;
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(alt));
    d.harness.input_mut().events.push(egui::Event::PointerButton { pos: at, button: egui::PointerButton::Primary, pressed: true, modifiers: alt });
    d.frames(1);
    for dx in [4.0, 10.0, 18.0, 26.0] {
        d.harness.input_mut().events.push(egui::Event::PointerMoved(at + egui::vec2(dx, 0.0)));
        d.frames(1);
    }
    assert_eq!(d.cursor(), CursorIcon::Copy, "the drag copies, so the pointer says so");
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: at + egui::vec2(26.0, 0.0),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: alt,
    });
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    d.frames(2);
}

/// The preview line is drawn on the clip's own track and its linked clips' tracks, and on every
/// track with Shift.
#[test]
fn the_razor_preview_line_is_drawn_on_the_linked_tracks_or_every_track_with_shift() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let at = clip_center(&mut d, city);
    d.ok("ui.set", json!({"tool": "Razor"}));
    d.frames(2);
    // nothing to cut off the tracks: what else draws white vertical lines is the baseline
    d.hover(pos2(2.0, 2.0));
    let base = d.white_vertical_lines();
    let linked = filmcraft_ui_egui::panels::timeline::razor_line_tracks(d.app(), ClipId(city), false).len();
    let every = filmcraft_ui_egui::panels::timeline::razor_line_tracks(d.app(), ClipId(city), true).len();
    assert!(linked > 1 && every > linked, "the demo clip is linked and there are more tracks: {linked}, {every}");
    d.hover(at);
    assert_eq!(d.white_vertical_lines() - base, linked, "the clip's track and its linked clips' tracks");
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::SHIFT));
    d.hover(at);
    assert_eq!(d.white_vertical_lines() - base, every, "Shift: every track");
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::NONE));
    d.frames(2);
}

impl Driver {
    fn rect(&mut self, id: &str) -> [f32; 4] {
        let els = self.ok("ui.elements", json!({"prefix": id}));
        let el = els.as_array().expect("elements").iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no {id}")).clone();
        let r: Vec<f32> = el["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        [r[0], r[1], r[2], r[3]]
    }
}

/// Over the work area bar a tool keeps the bar's own pointer, while hovering and while dragging it.
#[test]
fn the_work_area_bar_keeps_its_own_pointer_under_every_tool() {
    let mut d = Driver::demo();
    let rate = d.app().session.sequence_rate();
    for tool in ["Razor", "Slip", "Hand", "Zoom", "Pen"] {
        d.ok("engine.execute", json!({"command": "sequence.setWorkArea", "params": {"start": rate.tick_of(24).0, "end": rate.tick_of(72).0}}));
        d.frames(2);
        let bar = d.rect("timeline.workArea");
        let (middle, out_edge) = (pos2(bar[0] + bar[2] / 2.0, bar[1] + bar[3] / 2.0), pos2(bar[0] + bar[2] - 1.0, bar[1] + bar[3] / 2.0));
        d.ok("ui.set", json!({"tool": tool}));
        d.frames(2);
        d.hover(middle);
        assert_eq!(d.cursor(), CursorIcon::Grab, "{tool}: the bar's middle");
        d.hover(out_edge);
        assert_eq!(d.cursor(), CursorIcon::ResizeColumn, "{tool}: the bar's end");
        // a drag of the bar is not drawn as the tool either
        d.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: middle,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        });
        d.frames(1);
        for dx in [4.0, 10.0, 18.0] {
            d.harness.input_mut().events.push(egui::Event::PointerMoved(middle + egui::vec2(dx, 0.0)));
            d.frames(1);
        }
        assert_ne!(d.cursor(), CursorIcon::None, "{tool}: dragging the bar");
        d.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: middle + egui::vec2(18.0, 0.0),
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        });
        d.frames(2);
    }
}

/// Hand and Zoom act on empty Timeline space, so they show their pointer there; the other tools
/// keep the system one.
#[test]
fn hand_and_zoom_draw_their_pointer_over_empty_timeline_space() {
    let mut d = Driver::demo();
    let tracks = d.rect("timeline.tracks");
    // a point over no track (the divider between the video and audio tracks)
    let x = tracks[0] + tracks[2] / 2.0;
    let empty = (0..tracks[3] as i32)
        .map(|dy| pos2(x, tracks[1] + dy as f32))
        .find(|p| d.ok("ui.timeline.hit", json!({"x": p.x, "y": p.y}))["hit"] == "none")
        .expect("a point over no track");
    for tool in ["Hand", "Zoom"] {
        d.ok("ui.set", json!({"tool": tool}));
        d.frames(2);
        d.hover(empty);
        assert_eq!(d.cursor(), CursorIcon::None, "{tool}: its icon over empty space");
    }
    for tool in ["Razor", "Slip", "Pen"] {
        d.ok("ui.set", json!({"tool": tool}));
        d.frames(2);
        d.hover(empty);
        assert_ne!(d.cursor(), CursorIcon::None, "{tool}: nothing to act on over empty space");
    }
}

/// A clip on a locked track cannot be slipped or slid, and the pointer says so.
#[test]
fn slip_and_slide_over_a_locked_clip_show_the_blocked_pointer() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let at = clip_center(&mut d, city);
    let v1 = d.app().session.active_sequence().unwrap().video_tracks[0].id;
    for tool in ["Slip", "Slide"] {
        d.ok("ui.set", json!({"tool": tool}));
        d.frames(2);
        d.hover(at);
        assert_eq!(d.cursor(), CursorIcon::None, "{tool}: its icon over an unlocked clip");
    }
    d.ok("engine.execute", json!({"command": "timeline.setTrack", "params": {"track": v1.0, "locked": true}}));
    d.frames(2);
    for tool in ["Slip", "Slide"] {
        d.ok("ui.set", json!({"tool": tool}));
        d.frames(2);
        d.hover(at);
        assert_eq!(d.cursor(), CursorIcon::NotAllowed, "{tool}: the edit is refused");
    }
}

//! Headless UI tests of changing track heights by dragging the lines between track headers,
//! including the outer edges: the top of the highest video track and the bottom of the last audio
//! track.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 1400.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        // short enough that every track is in view, with empty space above and below them
        d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 24.0, "audioTrackHeight": 24.0}}));
        d.frames(3);
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

    fn ok(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    fn heights(&self) -> (f32, f32) {
        let v = &self.app().ui.timeline;
        (v.video_track_h, v.audio_track_h)
    }

    /// Drag from the middle of the element `id` by `dy` points.
    fn drag(&mut self, id: &str, fy: f32, dy: f32) {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
        let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] * fy);
        self.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x, "y": y + dy}}));
        self.frames(3);
    }

    fn labels(&self) -> (String, String) {
        let seq = self.app().session.active_sequence().unwrap();
        (format!("V{}", seq.video_tracks.len()), format!("A{}", seq.audio_tracks.len()))
    }
}

#[test]
fn the_header_edges_no_longer_resize_the_tracks_the_side_slider_circles_do() {
    let mut d = Driver::demo();
    let (top, _) = d.labels();
    let (v0, a0) = d.heights();
    // the old header-edge drag zones are gone: dragging at the highest video track's top and
    // bottom edges must not change the track heights any more (the old top zone sat just above
    // the A/V divider and kept firing by accident)
    let strip = rect_of(&mut d, &format!("timeline.track.{top}.color"));
    let (x, y) = (strip.center().x, strip.min.y + 1.0);
    d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x, "y": y - 15.0}}));
    d.frames(3);
    d.ok("ui.drag", json!({"from": {"x": x, "y": strip.max.y - 1.0}, "to": {"x": x, "y": strip.max.y + 14.0}}));
    d.frames(3);
    assert_eq!(d.heights(), (v0, a0), "the header edges are inert now");
    // the side slider's end circles are where vertical zoom lives now: dragging DOWN makes the
    // tracks taller (the thumb shrinks about its middle), up makes them shorter
    d.drag("timeline.trackSlider.video.topCircle", 0.5, 30.0);
    let (v1, a1) = d.heights();
    assert!(v1 > v0 + 1.0, "video tracks grow: {v0} -> {v1}");
    assert_eq!(a1, a0, "audio tracks are left alone");
    d.drag("timeline.trackSlider.video.topCircle", 0.5, -30.0);
    assert!(d.heights().0 < v1 - 1.0, "and up makes them shorter again: {v1} -> {}", d.heights().0);
    let v_before = d.heights().0;
    // the bottom circle mirrors the top: an inward pull (up) makes the audio tracks taller, an
    // outward pull (down) grows the thumb again, i.e. makes them shorter
    d.drag("timeline.trackSlider.audio.bottomCircle", 0.5, -30.0);
    let a_tall = d.heights().1;
    assert!(a_tall > a0 + 1.0, "audio tracks grow: {a0} -> {a_tall}");
    d.drag("timeline.trackSlider.audio.bottomCircle", 0.5, 30.0);
    let (v2, a2) = d.heights();
    assert!(a2 < a_tall - 1.0, "audio tracks shrink: {a_tall} -> {a2}");
    assert_eq!(v2, v_before, "video tracks are left alone");
}

#[test]
fn the_bottom_edge_of_the_last_audio_track_no_longer_resizes_the_audio_tracks() {
    let mut d = Driver::demo();
    let (_, last) = d.labels();
    let (v0, a0) = d.heights();
    let strip = rect_of(&mut d, &format!("timeline.track.{last}.color"));
    let x = strip.center().x;
    // just past the old line, in the empty space below the last track, and from just inside it
    d.ok("ui.drag", json!({"from": {"x": x, "y": strip.max.y - 1.0}, "to": {"x": x, "y": strip.max.y + 14.0}}));
    d.frames(3);
    d.ok("ui.drag", json!({"from": {"x": x, "y": strip.min.y + 1.0}, "to": {"x": x, "y": strip.min.y - 14.0}}));
    d.frames(3);
    assert_eq!(d.heights(), (v0, a0), "the header edges are inert now");
}

/// Every shape drawn this frame, flattened.
fn flat_shapes(d: &Driver) -> Vec<egui::Shape> {
    fn walk(s: &egui::Shape, out: &mut Vec<egui::Shape>) {
        match s {
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            other => out.push(other.clone()),
        }
    }
    let mut out = Vec::new();
    d.harness.output().shapes.iter().for_each(|c| walk(&c.shape, &mut out));
    out
}

fn rect_of(d: &mut Driver, id: &str) -> egui::Rect {
    let v = d.ok("ui.elements", json!({"prefix": id}));
    let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
    let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
    egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
}

// Regression test for #703: the lines between the timeline's tracks were drawn in the timeline's own
// background colour, so they were invisible, and nothing marked the top of the highest video track.
#[test]
fn timeline_draws_visible_lines_between_tracks_and_above_the_top_video_track() {
    let mut d = Driver::demo();
    let (top, _) = d.labels();
    let separator = d.app().tokens.separator;
    let ruler = rect_of(&mut d, "timeline.ruler");
    // the colour strip spans the header: its top is the top of the highest video track
    let top_edge = rect_of(&mut d, &format!("timeline.track.{top}.color"));
    let seq = d.app().session.active_sequence().unwrap();
    let tracks = seq.video_tracks.len() + seq.audio_tracks.len();
    // horizontal separator lines across the track area (the headers' own lines start left of the ruler)
    let mut ys: Vec<f32> = flat_shapes(&d)
        .into_iter()
        .filter_map(|s| match s {
            egui::Shape::LineSegment { points, stroke }
                if stroke.color == separator
                    && points[0].y == points[1].y
                    && points[0].x >= ruler.min.x - 1.0
                    && points[1].x - points[0].x > 200.0
                    && points[0].y > ruler.max.y =>
            {
                Some(points[0].y)
            }
            _ => None,
        })
        .collect();
    ys.sort_by(f32::total_cmp);
    ys.dedup_by(|a, b| (*a - *b).abs() < 0.1);
    assert!(ys.len() > tracks, "a line under each of the {tracks} tracks and one above the top video track, found {ys:?}");
    let first = ys[0];
    assert!((first - top_edge.min.y).abs() < 1.5, "the first line ({first}) is at the top edge of the highest video track ({})", top_edge.min.y);
}

// Regression test for #703: the playhead's head spanned the whole ruler and covered the markers and
// the timecode labels; it must be a short head on the tick row at the bottom of the ruler.
#[test]
fn playhead_head_is_short_and_sits_on_the_ruler_tick_row() {
    let mut d = Driver::demo();
    let playhead = d.app().tokens.playhead;
    let ruler = rect_of(&mut d, "timeline.ruler");
    let heads: Vec<egui::Rect> = flat_shapes(&d)
        .into_iter()
        .filter_map(|s| match s {
            egui::Shape::Path(p) if p.fill == playhead && p.points.len() == 5 => Some(egui::Rect::from_points(&p.points)),
            _ => None,
        })
        .filter(|r| r.max.y <= ruler.max.y + 1.0 && r.min.y >= ruler.min.y)
        .collect();
    assert_eq!(heads.len(), 1, "one playhead head over the ruler: {heads:?}");
    let head = heads[0];
    assert!(head.height() <= 14.0, "the head is {} tall, ruler is {}", head.height(), ruler.height());
    assert!(
        head.min.y >= ruler.max.y - 14.0,
        "the head starts at y={}, leaving the ruler's marker and label rows (above y={}) clear",
        head.min.y,
        ruler.max.y - 14.0
    );
}

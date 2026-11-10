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

}

#[test]
fn track_slider_changes_video_track_height_without_changing_audio_height() {
    let mut d = Driver::demo();
    let (v0, a0) = d.heights();
    d.drag("timeline.trackSlider.video.topCircle", 0.5, -16.0);
    let (v1, a1) = d.heights();
    assert_ne!(v1, v0, "the video track slider changes video height");
    assert_eq!(a1, a0, "audio tracks are left alone");
}

#[test]
fn track_slider_changes_audio_track_height_without_changing_video_height() {
    let mut d = Driver::demo();
    let (v0, a0) = d.heights();
    d.drag("timeline.trackSlider.audio.topCircle", 0.5, -16.0);
    let (v1, a1) = d.heights();
    assert_eq!(v1, v0, "video tracks are left alone");
    assert_ne!(a1, a0, "the audio track slider changes audio height");
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
    let separator = d.app().tokens.separator;
    let ruler = rect_of(&mut d, "timeline.ruler");
    let seq = d.app().session.active_sequence().unwrap();
    let tracks = seq.video_tracks.len() + seq.audio_tracks.len();
    // horizontal separator lines across the track area, including the top video edge
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
    assert!(first > ruler.max.y, "the top video edge is drawn below the ruler at y={first}");
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

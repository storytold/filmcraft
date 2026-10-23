//! Headless UI tests of changing track heights by dragging the lines between track headers,
//! including the outer edges: the top of the highest video track and the bottom of the last audio
//! track.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::TrackKind;
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

    /// Drag from the middle of the element `id` by `dy` points.
    fn drag(&mut self, id: &str, fy: f32, dy: f32) {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
        let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] * fy);
        self.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x, "y": y + dy}}));
        self.frames(3);
    }

    /// The height of each row of `kind`, in track order (V1 or A1 first).
    fn row_heights(&self, kind: TrackKind) -> Vec<f32> {
        let layout = self.app().tl.layout.as_ref().expect("timeline layout");
        let mut rows: Vec<_> = layout.rows.iter().filter(|r| r.kind == kind).collect();
        rows.sort_by_key(|r| r.index);
        rows.iter().map(|r| r.rect.height()).collect()
    }

    fn labels(&self) -> (String, String) {
        let seq = self.app().session.active_sequence().unwrap();
        (format!("V{}", seq.video_tracks.len()), format!("A{}", seq.audio_tracks.len()))
    }
}

#[test]
fn the_top_edge_of_the_highest_video_track_resizes_that_track() {
    let mut d = Driver::demo();
    let (top, _) = d.labels();
    let (v0, a0) = (d.row_heights(TrackKind::Video), d.row_heights(TrackKind::Audio));
    let n = v0.len() - 1;
    // dragging the outer edge up makes the track taller, as the line below it does
    d.drag(&format!("timeline.track.{top}.resizeTop"), 0.5, -15.0);
    let v1 = d.row_heights(TrackKind::Video);
    assert!(v1[n] > v0[n] + 5.0, "{top} grows: {v0:?} -> {v1:?}");
    assert_eq!(v1[..n], v0[..n], "the video tracks below it are left alone");
    assert_eq!(d.row_heights(TrackKind::Audio), a0, "audio tracks are left alone");
    // and down makes it shorter again
    d.drag(&format!("timeline.track.{top}.resizeTop"), 0.5, 15.0);
    assert!(d.row_heights(TrackKind::Video)[n] < v1[n] - 5.0);
    // the line below it still works the same way
    let v2 = d.row_heights(TrackKind::Video)[n];
    d.drag(&format!("timeline.track.{top}.resize"), 0.5, -15.0);
    assert!(d.row_heights(TrackKind::Video)[n] > v2 + 5.0);
}

#[test]
fn the_bottom_edge_of_the_last_audio_track_resizes_that_track_from_either_side_of_the_line() {
    let mut d = Driver::demo();
    let (_, last) = d.labels();
    let id = format!("timeline.track.{last}.resize");
    let (v0, a0) = (d.row_heights(TrackKind::Video), d.row_heights(TrackKind::Audio));
    let n = a0.len() - 1;
    // from just past the line, in the empty space below the last track
    d.drag(&id, 0.9, 15.0);
    let a1 = d.row_heights(TrackKind::Audio);
    assert!(a1[n] > a0[n] + 5.0, "{last} grows: {a0:?} -> {a1:?}");
    assert_eq!(a1[..n], a0[..n], "the audio tracks above it are left alone");
    assert_eq!(d.row_heights(TrackKind::Video), v0, "video tracks are left alone");
    // and from just inside it
    d.drag(&id, 0.1, -15.0);
    assert!(d.row_heights(TrackKind::Audio)[n] < a1[n] - 5.0);
}

#[test]
fn dragging_a_line_resizes_only_the_track_it_belongs_to() {
    let mut d = Driver::demo();
    let (v0, a0) = (d.row_heights(TrackKind::Video), d.row_heights(TrackKind::Audio));
    assert!(v0.len() >= 2 && a0.len() >= 2, "the demo has several tracks of each kind: {v0:?} {a0:?}");
    // V1's line is below it: up makes V1 taller
    d.drag("timeline.track.V1.resize", 0.5, -15.0);
    let v1 = d.row_heights(TrackKind::Video);
    assert!(v1[0] > v0[0] + 5.0, "V1 grows: {v0:?} -> {v1:?}");
    assert_eq!(v1[1..], v0[1..], "the other video tracks keep their height");
    assert_eq!(d.row_heights(TrackKind::Audio), a0, "audio tracks are left alone");
    // A1's line is below it: down makes A1 taller
    d.drag("timeline.track.A1.resize", 0.5, 15.0);
    let a1 = d.row_heights(TrackKind::Audio);
    assert!(a1[0] > a0[0] + 5.0, "A1 grows: {a0:?} -> {a1:?}");
    assert_eq!(a1[1..], a0[1..], "the other audio tracks keep their height");
    assert_eq!(d.row_heights(TrackKind::Video), v1, "video tracks are left alone");
    // setting the height of all the video tracks includes the one resized on its own
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 30.0}}));
    d.frames(2);
    assert!(d.row_heights(TrackKind::Video).iter().all(|h| *h == 30.0), "{:?}", d.row_heights(TrackKind::Video));
    assert_eq!(d.row_heights(TrackKind::Audio), a1, "audio tracks are left alone");
}

#[test]
fn expand_all_tracks_pressed_again_gives_back_a_track_resized_on_its_own() {
    let mut d = Driver::demo();
    // row heights come from rects, so they are compared to a tenth of a point
    let rows = |d: &Driver| [TrackKind::Video, TrackKind::Audio].map(|k| d.row_heights(k).iter().map(|h| (h * 10.0).round() / 10.0).collect::<Vec<_>>());
    d.drag("timeline.track.V1.resize", 0.5, -15.0);
    d.drag("timeline.track.A1.resize", 0.5, 15.0);
    let before = rows(&d);
    let [v0, a0] = &before;
    assert!(v0[0] > v0[1] && a0[0] > a0[1], "V1 and A1 resized on their own: {v0:?} {a0:?}");
    for key in ["Shift+=", "Shift+-"] {
        d.ok("ui.key", json!({"key": key}));
        d.frames(3);
        let [v1, a1] = rows(&d);
        assert!(v1.iter().chain(&a1).all(|h| *h == v1[0]) && v1[0] != v0[0], "{key}: every track at one height: {v1:?} {a1:?}");
        // pressing again gives every track the height it had, the ones resized on their own too
        d.ok("ui.key", json!({"key": key}));
        d.frames(3);
        assert_eq!(rows(&d), before, "{key} pressed again");
    }
}

#[test]
fn a_track_resized_on_its_own_keeps_its_height_in_the_saved_project() {
    let dir = std::env::temp_dir().join(format!("filmcraft-track-heights-ui-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("heights.fcproj").to_string_lossy().to_string();
    let mut d = Driver::demo();
    d.drag("timeline.track.V1.resize", 0.5, -15.0);
    d.drag("timeline.track.A1.resize", 0.5, 15.0);
    let rows = |d: &Driver| [TrackKind::Video, TrackKind::Audio].map(|k| d.row_heights(k).iter().map(|h| (h * 10.0).round() / 10.0).collect::<Vec<_>>());
    let before = rows(&d);
    assert!(before[0][0] > before[0][1] && before[1][0] > before[1][1], "V1 and A1 resized on their own: {before:?}");
    d.ok("engine.execute", json!({"command": "file.saveAs", "params": {"path": path}}));

    // another window of the app opens the file
    let mut e = Driver::demo();
    e.ok("engine.execute", json!({"command": "file.open", "params": {"path": path}}));
    e.frames(3);
    assert_eq!(rows(&e), before);
    let _ = std::fs::remove_dir_all(&dir);
}

/// What Expand / Minimize All Tracks gives back belongs to the sequence it was pressed in: another
/// sequence already at the target height does not get those heights, shared or per track.
#[test]
fn expand_all_tracks_gives_back_only_the_heights_of_the_same_sequence() {
    let mut d = Driver::demo();
    d.drag("timeline.track.V1.resize", 0.5, -15.0);
    d.ok("ui.key", json!({"key": "Shift+="}));
    d.frames(3);
    // another sequence, already at the height Expand All Tracks gives
    d.ok("engine.execute", json!({"command": "file.newSequence", "params": {"name": "B"}}));
    d.frames(3);
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 120.0, "audioTrackHeight": 120.0}}));
    d.frames(3);
    d.ok("ui.key", json!({"key": "Shift+="}));
    d.frames(3);
    let tv = &d.app().ui.timeline;
    assert_eq!((tv.video_track_h, tv.audio_track_h), (120.0, 120.0), "the first sequence's shared heights");
    assert!(tv.video_heights.is_empty() && tv.audio_heights.is_empty(), "the first sequence's tracks: {:?} {:?}", tv.video_heights, tv.audio_heights);
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
    let top_edge = rect_of(&mut d, &format!("timeline.track.{top}.resizeTop"));
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
    assert!((first - top_edge.center().y).abs() < 1.5, "the first line ({first}) is at the top edge of the highest video track ({})", top_edge.center().y);
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

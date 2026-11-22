//! Headless UI tests of changing track heights by dragging the lines between track headers,
//! including the outer edges: the top of the highest video track and the bottom of the last audio
//! track.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
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

    /// The `(top, height)` of every video row, V1 first, as the Timeline laid them out this frame.
    /// This is the one geometry the headers and the clips both read.
    fn video_bands(&self) -> Vec<(f32, f32)> {
        let l = self.app().tl.layout.as_ref().expect("the timeline is laid out");
        let nv = self.app().session.active_sequence().unwrap().video_tracks.len();
        l.rows.iter().take(nv).map(|r| (r.rect.min.y, r.rect.height())).collect()
    }

    /// The `(top, height)` of every audio row, A1 first, as the Timeline laid them out this frame.
    fn audio_bands(&self) -> Vec<(f32, f32)> {
        let l = self.app().tl.layout.as_ref().expect("the timeline is laid out");
        let nv = self.app().session.active_sequence().unwrap().video_tracks.len();
        l.rows.iter().skip(nv).map(|r| (r.rect.min.y, r.rect.height())).collect()
    }
}

#[test]
fn the_top_edge_of_a_video_track_resizes_that_track_alone() {
    let mut d = Driver::demo();
    let before = d.video_bands();
    assert!(before.len() >= 2, "the demo project has at least two video tracks");
    // the video block is anchored at the divider and grows upward, so V1's moving edge is its top one
    d.drag("timeline.track.V1.resize", 0.5, -15.0);
    let after = d.video_bands();
    assert!(after[0].1 > before[0].1 + 5.0, "V1 grows: {} -> {}", before[0].1, after[0].1);
    for i in 1..after.len() {
        let (was, now) = (before[i].1, after[i].1);
        assert!((now - was).abs() < 0.6, "V{} keeps its own height: {was} -> {now}", i + 1);
    }
    // and the rows still share one bottom edge: each one ends where the row below it starts
    for i in 1..after.len() {
        let (ends, starts) = (after[i].0 + after[i].1, after[i - 1].0);
        assert!((ends - starts).abs() < 0.6, "V{} ends at {ends} but the row below starts at {starts}", i + 1);
    }
    // dragging it back down makes it shorter again
    let grown = after[0].1;
    d.drag("timeline.track.V1.resize", 0.5, 15.0);
    assert!(d.video_bands()[0].1 < grown - 5.0);
}

/// The commands that resize every track have to reach the ones with their own height too, or
/// "Minimize All Tracks" would only move the tracks that never got their own.
#[test]
fn the_global_height_commands_reach_a_track_with_its_own_height() {
    let mut d = Driver::demo();
    d.drag("timeline.track.V1.resize", 0.5, -15.0);
    let own = d.video_bands()[0].1;
    assert!(own > 30.0, "V1 got its own height: {own}");
    // Minimize All Tracks leaves no track behind
    d.ok("ui.menu.invoke", json!({"command": "timeline.minimizeAllTracks"}));
    d.frames(3);
    let minimized = d.video_bands();
    let small = minimized[0].1;
    // minimizing has to make it shorter, not merely different: 39 -> 50 must fail too
    assert!(small < own - 2.0, "V1 got shorter: {own} -> {small}");
    for (i, b) in minimized.iter().enumerate() {
        assert!((b.1 - small).abs() < 0.6, "V{} is {} tall, V1 is {small}", i + 1, b.1);
    }
    // Expand All Tracks takes every track up, own heights included
    d.ok("ui.menu.invoke", json!({"command": "timeline.expandAllTracks"}));
    d.frames(3);
    let expanded = d.video_bands()[0].1;
    assert!(expanded > small + 5.0, "all tracks grow: {small} -> {expanded}");
    // and pressing it again brings back the own height the toggle collapsed
    d.ok("ui.menu.invoke", json!({"command": "timeline.expandAllTracks"}));
    d.frames(3);
    let back = d.video_bands()[0].1;
    assert!((back - own).abs() < 0.6, "the second press restores V1's own height: {back} vs {own}");
}

#[test]
fn the_bottom_edge_of_an_audio_track_resizes_that_track_from_either_side_of_the_line() {
    let mut d = Driver::demo();
    let (_, last) = d.labels();
    let id = format!("timeline.track.{last}.resize");
    let before = d.audio_bands();
    let n = before.len();
    // from just past the line, in the empty space below the last track
    d.drag(&id, 0.9, 15.0);
    let after = d.audio_bands();
    let (was, now) = (before[n - 1].1, after[n - 1].1);
    assert!(now > was + 5.0, "the last audio track grows: {was} -> {now}");
    for i in 0..n - 1 {
        let (was, now) = (before[i].1, after[i].1);
        assert!((now - was).abs() < 0.6, "A{} keeps its own height: {was} -> {now}", i + 1);
    }
    // and from just inside it
    let grown = after[n - 1].1;
    d.drag(&id, 0.1, -15.0);
    assert!(d.audio_bands()[n - 1].1 < grown - 5.0);
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
    let top_edge = rect_of(&mut d, &format!("timeline.track.{top}.resize"));
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

/// #5091 review (H2): a height that is not a number must not throw away the per-track overrides.
/// `ui.set` cleared all of them whenever the key was present, so a request that changed no default
/// still dropped the heights someone had dragged.
#[test]
fn a_height_that_is_not_a_number_keeps_the_per_track_overrides() {
    let mut d = Driver::demo();
    d.drag("timeline.track.V1.resize", 0.5, -15.0);
    let own = d.video_bands()[0].1;
    assert!(own > 30.0, "V1 got its own height: {own}");
    // null is present but is not a number: nothing applies, so nothing is cleared
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": null, "audioTrackHeight": null}}));
    d.frames(3);
    let after = d.video_bands()[0].1;
    assert!((after - own).abs() < 0.6, "V1 kept its own height: {own} -> {after}");
    // a real number is a new default for every track, so it does clear them.
    // 60, not the 39 the drag above left on V1: with the dragged height as the default V1 reads
    // 39 whether the override survived or was cleared, so the test could not tell the two apart.
    d.ok("ui.set", json!({"timeline": {"videoTrackHeight": 60.0}}));
    d.frames(3);
    let applied = d.video_bands()[0].1;
    assert!((applied - 60.0).abs() < 2.0, "the number applies to every track: {applied}");
}

/// #5091 review: the two actions in the Timeline's own settings menu have to reach a track with
/// its own height as well, or "Minimize All Tracks" only moves the tracks that never got one. The
/// keyboard commands had a test; these two buttons did not.
#[test]
fn the_settings_menu_height_actions_reach_a_track_with_its_own_height() {
    let mut d = Driver::demo();
    d.drag("timeline.track.V1.resize", 0.5, -15.0);
    let own = d.video_bands()[0].1;
    assert!(own > 30.0, "V1 got its own height: {own}");
    d.ok("ui.click", json!({"id": "timeline.toggle.settings"}));
    d.frames(2);
    d.harness.get_by_label("Minimize All Tracks").click();
    d.frames(3);
    let small = d.video_bands()[0].1;
    assert!(small < own - 2.0, "the menu button shrank V1 too: {own} -> {small}");
    d.ok("ui.click", json!({"id": "timeline.toggle.settings"}));
    d.frames(2);
    d.harness.get_by_label("Expand All Tracks").click();
    d.frames(3);
    let big = d.video_bands()[0].1;
    assert!(big > small + 5.0, "the menu button grew V1 too: {small} -> {big}");
}

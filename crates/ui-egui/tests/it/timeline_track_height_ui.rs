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
        // the Timeline enforces its per-track minimum from the available pane height; all tracks
        // fit, leaving the end circles ready to zoom the track height.
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

    fn add_many_tracks(&mut self, count: usize) {
        self.harness.state_mut().session.execute("sequence.addTracks", json!({"video": count, "audio": count})).expect("add regression tracks");
        self.frames(4);
    }

    fn pointer_event(&mut self, event: egui::Event) {
        self.harness.input_mut().events.push(event);
        self.frames(1);
    }
}

#[test]
fn the_top_edge_of_the_highest_video_track_resizes_the_video_tracks() {
    let mut d = Driver::demo();
    let (v0, a0) = d.heights();
    // drag the top slider end inward to make the video tracks taller
    d.drag("timeline.trackSlider.video.topCircle", 0.5, 15.0);
    let (v1, a1) = d.heights();
    assert!(v1 > v0 + 5.0, "video tracks grow: {v0} -> {v1}");
    assert_eq!(a1, a0, "audio tracks are left alone");
    // and back outward makes them shorter again
    d.drag("timeline.trackSlider.video.topCircle", 0.5, -15.0);
    assert!(d.heights().0 < v1 - 5.0);
    assert!((d.heights().0 - v0).abs() < 5.0, "returning the slider restores its starting height");
}

#[test]
fn the_bottom_edge_of_the_last_audio_track_resizes_the_audio_tracks_from_either_side_of_the_line() {
    let mut d = Driver::demo();
    let (v0, a0) = d.heights();
    // Pull the bottom slider end inward to make the audio tracks taller.
    d.drag("timeline.trackSlider.audio.bottomCircle", 0.5, -15.0);
    let (v1, a1) = d.heights();
    assert!(a1 > a0 + 5.0, "audio tracks grow: {a0} -> {a1}");
    assert_eq!(v1, v0, "video tracks are left alone");
    // and back outward to make them shorter again
    d.drag("timeline.trackSlider.audio.bottomCircle", 0.5, 15.0);
    assert!(d.heights().1 < a1 - 5.0);
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

fn audio_a1_a2_divider_y(d: &mut Driver) -> f32 {
    // A1 or A2 may be clipped completely once the divider is snapped to a viewport edge.
    // Reconstruct the exact row boundary in screen coordinates from the rendered timeline state.
    let layout = d.app().tl.layout.as_ref().expect("timeline layout");
    layout.split_y + 5.0 + d.app().ui.timeline.audio_track_h - d.app().ui.timeline.a_scroll + 1.0
}

fn audio_window_height(d: &Driver) -> f32 {
    let layout = d.app().tl.layout.as_ref().expect("timeline layout");
    // Match the audio-area rectangle built by the Timeline: content bottom minus the master
    // strip, less the divider and audio-area top.
    (layout.content.max.y - 34.0 - layout.split_y - 5.0).max(0.0)
}

fn track_slider_thumb_height(track_height: f32, bar_height: f32, min_height: f32) -> f32 {
    let max_height = 220.0f32.max(min_height * 2.0);
    let fraction = 1.0 - (track_height.clamp(min_height, max_height) / min_height).ln() / (max_height / min_height).ln();
    34.0 + fraction.clamp(0.0, 1.0) * (bar_height - 34.0).max(1.0)
}

#[test]
fn audio_outer_edge_snaps_without_moving_the_rendered_inner_boundary() {
    let mut probe = Driver::demo();
    probe.add_many_tracks(6);
    let audio_window = audio_window_height(&probe);
    let audio_count = probe.app().session.active_sequence().unwrap().audio_tracks.len();
    let slots = audio_count as f32 + 1.0 / 3.0;
    let last_whole_divider = (slots - 1.0 / 3.0).round();

    // Independently drive the outer edge to both neighboring endpoints, starting with A1/A2
    // exactly at the inner viewport edge. The test measures the rendered row boundary itself,
    // not the slider circles or just the internal scroll ratio.
    for detent in [last_whole_divider, slots] {
        let mut d = Driver::demo();
        d.add_many_tracks(6);
        let bar = rect_of(&mut d, "timeline.trackSlider.audio.track");
        let target_height = (audio_window + 0.5) / (detent - 1.0);
        let start_height = target_height + 6.0;
        {
            let app = d.harness.state_mut();
            app.session.state.timeline_ui.track_slider_decouple = true;
            app.session.state.timeline_ui.track_slider_snap = true;
            app.session.state.timeline_ui.track_slider_smooth = false;
            app.ui.timeline.audio_track_h = start_height;
            app.ui.timeline.a_scroll = start_height - 0.5;
        }
        d.frames(4);

        let divider_before = audio_a1_a2_divider_y(&mut d);
        let a2_controls_before = [
            rect_of(&mut d, "timeline.track.A2.locked").center().y,
            rect_of(&mut d, "timeline.track.A2.keyframes").center().y,
            rect_of(&mut d, "timeline.track.A2.syncLock").center().y,
            rect_of(&mut d, "timeline.track.A2.muted").center().y,
            rect_of(&mut d, "timeline.track.A2.solo").center().y,
            rect_of(&mut d, "timeline.track.A2.voiceover").center().y,
        ];
        assert!(divider_before.is_finite(), "the rendered A1/A2 boundary is visible before the snap");
        let thumb_before = rect_of(&mut d, "timeline.trackSlider.audio");
        let target_thumb_height = track_slider_thumb_height(target_height, bar.height(), audio_window / slots);
        let outward_drag = target_thumb_height - thumb_before.height();
        let available_thumb = bar.max.y + 2.0 - thumb_before.min.y;
        assert!(
            target_thumb_height <= available_thumb + 0.1,
            "detent thumb fits with the inner edge fixed: target {target_thumb_height}, available {available_thumb}"
        );
        assert!(outward_drag > 2.0, "the endpoint requires an actual outer-edge drag: {outward_drag}");
        // Slightly pass the detent so the fractional end slot is tested after the snap
        // threshold, rather than exactly at the tie with the preceding whole-track detent.
        d.drag("timeline.trackSlider.audio.bottomCircle", 0.5, outward_drag + 2.0);

        let divider_after = audio_a1_a2_divider_y(&mut d);
        assert!((divider_after - divider_before).abs() < 0.01, "the rendered A1/A2 boundary must not move: {divider_before} -> {divider_after}");
        let a2_controls_after = [
            rect_of(&mut d, "timeline.track.A2.locked").center().y,
            rect_of(&mut d, "timeline.track.A2.keyframes").center().y,
            rect_of(&mut d, "timeline.track.A2.syncLock").center().y,
            rect_of(&mut d, "timeline.track.A2.muted").center().y,
            rect_of(&mut d, "timeline.track.A2.solo").center().y,
            rect_of(&mut d, "timeline.track.A2.voiceover").center().y,
        ];
        for (before, after) in a2_controls_before.into_iter().zip(a2_controls_after) {
            assert_eq!(after, before, "A2 header control must not move during an outer-edge snap");
        }
        let view = &d.app().ui.timeline;
        let snapped_edge = (audio_window + view.a_scroll + 1.0) / view.audio_track_h;
        assert!(
            (snapped_edge - detent).abs() < 0.02,
            "outer edge reaches detent {detent} without an in-between position: {snapped_edge}; target height {target_height}, actual height {}, scroll {}, target thumb {target_thumb_height}, available thumb {available_thumb}",
            view.audio_track_h,
            view.a_scroll
        );
        let outer_line_offset = audio_window + view.a_scroll - detent * view.audio_track_h;
        assert!((outer_line_offset + 1.0).abs() < 0.1, "audio outer white line matches video's one-point inset: {outer_line_offset}");
    }
}

#[test]
fn repeated_audio_outer_snaps_keep_a1_a2_and_its_header_controls_pinned_with_many_tracks() {
    let mut d = Driver::demo();
    d.add_many_tracks(6);
    let bar = rect_of(&mut d, "timeline.trackSlider.audio.track");
    let window = audio_window_height(&d);
    let slots = d.app().session.active_sequence().unwrap().audio_tracks.len() as f32 + 1.0 / 3.0;
    {
        let app = d.harness.state_mut();
        app.session.state.timeline_ui.track_slider_decouple = true;
        app.session.state.timeline_ui.track_slider_snap = true;
        app.session.state.timeline_ui.track_slider_smooth = false;
        // Start with the visible A1/A2 boundary pinned to the viewport's inner edge.
        app.ui.timeline.audio_track_h = (window + 0.5) / 3.0 + 8.0;
        app.ui.timeline.a_scroll = app.ui.timeline.audio_track_h - 0.5;
    }
    d.frames(4);

    let mut fixed_boundary = audio_a1_a2_divider_y(&mut d);
    let mut fixed_controls =
        ["locked", "keyframes", "syncLock", "muted", "solo", "voiceover"].map(|name| rect_of(&mut d, &format!("timeline.track.A2.{name}")).center().y);

    // Reproduce A3/A4, then A2/A3. Regrab the outer handle between snaps, as in the report.
    for detent in [4.0, 3.0] {
        let current = rect_of(&mut d, "timeline.trackSlider.audio");
        let target_height = (window + 0.5) / (detent - 1.0);
        let target_thumb = track_slider_thumb_height(target_height, bar.height(), window / slots);
        let delta = target_thumb - current.height();
        d.drag("timeline.trackSlider.audio.bottomCircle", 0.5, delta + delta.signum() * 2.0);

        let boundary = audio_a1_a2_divider_y(&mut d);
        assert!((boundary - fixed_boundary).abs() < 0.01, "A1/A2 visible boundary moved at detent {detent}: {fixed_boundary} -> {boundary}");
        let controls =
            ["locked", "keyframes", "syncLock", "muted", "solo", "voiceover"].map(|name| rect_of(&mut d, &format!("timeline.track.A2.{name}")).center().y);
        for (before, after) in fixed_controls.into_iter().zip(controls) {
            assert!((after - before).abs() < 0.01, "A2 control moved by a visible pixel during outer-edge snap to {detent}: {before} -> {after}");
        }
        let view = &d.app().ui.timeline;
        let actual_detent = (window + view.a_scroll + 1.0) / view.audio_track_h;
        assert!((actual_detent - detent).abs() < 0.02, "outer edge reaches detent {detent}: {actual_detent}");
        fixed_boundary = boundary;
        fixed_controls = controls;
    }
}

#[test]
fn actual_inner_then_outer_audio_snaps_keep_rendered_boundary_fixed() {
    let mut d = Driver::demo();
    d.add_many_tracks(8);
    let bar = rect_of(&mut d, "timeline.trackSlider.audio.track");
    let window = audio_window_height(&d);
    let slots = d.app().session.active_sequence().unwrap().audio_tracks.len() as f32 + 1.0 / 3.0;
    {
        let app = d.harness.state_mut();
        app.session.state.timeline_ui.track_slider_decouple = true;
        app.session.state.timeline_ui.track_slider_snap = true;
        app.session.state.timeline_ui.track_slider_smooth = false;
        // Begin away from either scroll endpoint so the inner-edge snap has a stable viewport
        // anchor and can snap to the A1/A2 separator through the real handle interaction.
        app.ui.timeline.audio_track_h = 120.0;
        app.ui.timeline.a_scroll = 2.5 * 120.0 - window - 0.5;
    }
    d.frames(4);

    let inner_anchor = (d.app().ui.timeline.a_scroll + window + 0.5) / d.app().ui.timeline.audio_track_h;
    let first_height = (window + 1.5) / (inner_anchor - 1.0);
    let thumb = rect_of(&mut d, "timeline.trackSlider.audio");
    let first_len = track_slider_thumb_height(first_height, bar.height(), window / slots);
    d.drag("timeline.trackSlider.audio.topCircle", 0.5, thumb.height() - first_len + 2.0);
    let inner_view = &d.app().ui.timeline;
    let inner_detent = (inner_view.a_scroll - 1.0) / inner_view.audio_track_h;
    assert!((inner_detent - 1.0).abs() < 0.02, "inner handle really snaps to A1/A2: {inner_detent}");

    let mut fixed_boundary = audio_a1_a2_divider_y(&mut d);
    let mut fixed_controls =
        ["locked", "keyframes", "syncLock", "muted", "solo", "voiceover"].map(|name| rect_of(&mut d, &format!("timeline.track.A2.{name}")).center().y);
    for detent in [4.0, 3.0] {
        let view = &d.app().ui.timeline;
        let anchor = (view.a_scroll + 0.5) / view.audio_track_h;
        let target_height = (window + 0.5) / (detent - anchor);
        let thumb = rect_of(&mut d, "timeline.trackSlider.audio");
        let target_len = track_slider_thumb_height(target_height, bar.height(), window / slots);
        d.drag("timeline.trackSlider.audio.bottomCircle", 0.5, target_len - thumb.height() + 2.0);

        let boundary = audio_a1_a2_divider_y(&mut d);
        assert!((boundary - fixed_boundary).abs() < 0.01, "rendered A1/A2 boundary moved at detent {detent}: {fixed_boundary} -> {boundary}");
        let controls =
            ["locked", "keyframes", "syncLock", "muted", "solo", "voiceover"].map(|name| rect_of(&mut d, &format!("timeline.track.A2.{name}")).center().y);
        for (before, after) in fixed_controls.into_iter().zip(controls) {
            assert!((after - before).abs() < 0.01, "A2 control moved by a visible pixel at outer detent {detent}: {before} -> {after}");
        }
        let view = &d.app().ui.timeline;
        let outer_detent = (window + view.a_scroll + 1.0) / view.audio_track_h;
        assert!((outer_detent - detent).abs() < 0.02, "outer edge reaches A3/A4 then A2/A3 detent {detent}: {outer_detent}");
        fixed_boundary = boundary;
        fixed_controls = controls;
    }
}

#[test]
fn audio_outer_edge_snap_keeps_inner_view_fixed_and_middle_drag_moves_visually() {
    let mut d = Driver::demo();
    d.add_many_tracks(6);
    {
        let app = d.harness.state_mut();
        app.session.state.timeline_ui.track_slider_decouple = true;
        app.session.state.timeline_ui.track_slider_snap = true;
        app.session.state.timeline_ui.track_slider_smooth = false;
        // Start away from the top so the fixed inner edge has a meaningful content position.
        app.ui.timeline.audio_track_h = 600.0;
        // Put the A1/A2 row boundary exactly at the visible inner edge, as if snapped there.
        app.ui.timeline.a_scroll = 601.0;
    }
    d.frames(4);

    // Resize from the other end while the A1/A2 visible boundary is the captured anchor.
    let height_before = d.app().ui.timeline.audio_track_h;
    let inner_edge_before = audio_a1_a2_divider_y(&mut d);
    d.drag("timeline.trackSlider.audio.bottomCircle", 0.5, 20.0);
    let height_after = d.app().ui.timeline.audio_track_h;
    assert!((height_after - height_before).abs() > 1.0, "outer-edge drag resizes audio tracks");
    let inner_edge_after = audio_a1_a2_divider_y(&mut d);
    assert!((inner_edge_after - inner_edge_before).abs() < 0.01, "outer-edge resize keeps the rendered inner timeline boundary fixed");

    // Re-grabbing and moving the outer edge to another detent must not accumulate a pixel nudge.
    d.drag("timeline.trackSlider.audio.bottomCircle", 0.5, -12.0);
    let boundary_again = audio_a1_a2_divider_y(&mut d);
    assert!((boundary_again - inner_edge_after).abs() < 0.01, "a later outer-edge snap still keeps the rendered inner boundary fixed");

    let thumb_before = rect_of(&mut d, "timeline.trackSlider.audio");
    let scroll_before_middle = d.app().ui.timeline.a_scroll;
    d.drag("timeline.trackSlider.audio", 0.5, 30.0);
    let thumb_after = rect_of(&mut d, "timeline.trackSlider.audio");
    let scroll_after_middle = d.app().ui.timeline.a_scroll;
    assert!((scroll_after_middle - scroll_before_middle).abs() > 1.0, "middle drag changes the audio scroll");
    assert!((thumb_after.min.y - thumb_before.min.y).abs() > 1.0, "middle drag moves the slider visually");

    // The repeated inner/outer snaps must leave a usable slider that can still reveal the
    // one-third empty slot below the last audio track.
    d.drag("timeline.trackSlider.audio.topCircle", 0.5, 15.0);
    let bar = rect_of(&mut d, "timeline.trackSlider.audio.track");
    let thumb = rect_of(&mut d, "timeline.trackSlider.audio");
    let travel_to_end = (bar.max.y - thumb.max.y).max(0.0);
    d.drag("timeline.trackSlider.audio", 0.5, travel_to_end + 2.0);
    let view = &d.app().ui.timeline;
    let audio_count = d.app().session.active_sequence().unwrap().audio_tracks.len();
    let slots = audio_count as f32 + 1.0 / 3.0;
    let max_scroll = (slots * view.audio_track_h - audio_window_height(&d)).max(0.0);
    assert!(
        (view.a_scroll - max_scroll).abs() < 2.0,
        "slider reaches the one-third slot below the last audio track: scroll {}, expected {max_scroll}, height {}, slots {slots}, animation {:?}",
        view.a_scroll,
        view.audio_track_h,
        d.app().tl.track_anim
    );
}

#[test]
fn many_video_tracks_reach_the_top_reveal_after_repeated_edge_resizes() {
    let mut d = Driver::demo();
    d.add_many_tracks(6);
    {
        let app = d.harness.state_mut();
        app.session.state.timeline_ui.track_slider_decouple = true;
        app.session.state.timeline_ui.track_slider_snap = true;
        app.session.state.timeline_ui.track_slider_smooth = false;
        app.ui.timeline.video_track_h = 600.0;
        app.ui.timeline.v_scroll = 300.0;
    }
    d.frames(4);

    // Exercise both video ends and regrab the outer edge before scrolling to the reveal slot.
    d.drag("timeline.trackSlider.video.topCircle", 0.5, -18.0);
    d.drag("timeline.trackSlider.video.bottomCircle", 0.5, 14.0);
    d.drag("timeline.trackSlider.video.topCircle", 0.5, -10.0);
    let bar = rect_of(&mut d, "timeline.trackSlider.video.track");
    let thumb = rect_of(&mut d, "timeline.trackSlider.video");
    let travel_to_top = (thumb.min.y - bar.min.y).max(0.0);
    d.drag("timeline.trackSlider.video", 0.5, -travel_to_top - 2.0);

    let view = &d.app().ui.timeline;
    let layout = d.app().tl.layout.as_ref().expect("timeline layout");
    let window = layout.split_y - layout.content.min.y;
    let slots = d.app().session.active_sequence().unwrap().video_tracks.len() as f32 + 1.0 / 3.0;
    let max_scroll = (slots * view.video_track_h - window).max(0.0);
    assert!(
        (view.v_scroll - max_scroll).abs() < 2.0,
        "video slider reaches the one-third reveal above the top track: scroll {}, expected {max_scroll}, height {}, slots {slots}",
        view.v_scroll,
        view.video_track_h
    );
}

#[test]
fn middle_drag_never_snaps_video_or_audio_track_views() {
    for (id, kind) in [("timeline.trackSlider.video", "video"), ("timeline.trackSlider.audio", "audio")] {
        let mut d = Driver::demo();
        d.add_many_tracks(6);
        let window = if kind == "video" {
            let layout = d.app().tl.layout.as_ref().expect("timeline layout");
            layout.split_y - layout.content.min.y
        } else {
            audio_window_height(&d)
        };
        let sequence = d.app().session.active_sequence().unwrap();
        let count = if kind == "video" { sequence.video_tracks.len() } else { sequence.audio_tracks.len() };
        let slots = count as f32 + 1.0 / 3.0;
        {
            let app = d.harness.state_mut();
            app.session.state.timeline_ui.track_slider_decouple = true;
            app.session.state.timeline_ui.track_slider_snap = true;
            app.session.state.timeline_ui.track_slider_smooth = false;
            if kind == "video" {
                app.ui.timeline.video_track_h = 100.0;
                app.ui.timeline.v_scroll = 137.3;
            } else {
                app.ui.timeline.audio_track_h = 100.0;
                app.ui.timeline.a_scroll = 137.3;
            }
        }
        d.frames(4);
        let before = if kind == "video" { d.app().ui.timeline.v_scroll } else { d.app().ui.timeline.a_scroll };
        d.drag(id, 0.5, 13.7);
        let view = &d.app().ui.timeline;
        let (scroll, height) = if kind == "video" { (view.v_scroll, view.video_track_h) } else { (view.a_scroll, view.audio_track_h) };
        let live_max = (slots * height - window).max(0.0);
        let coordinate = (if kind == "video" { live_max - scroll } else { scroll }) / height;
        assert!((scroll - before).abs() > 1.0, "the {kind} middle drag moves the view");
        assert!((coordinate - coordinate.round()).abs() > 0.05, "the {kind} middle drag remains between white-line detents: {coordinate}");
    }
}

#[test]
fn video_inner_edge_keeps_a_midrow_anchor_and_can_resize_across_detents() {
    let mut d = Driver::demo();
    d.add_many_tracks(8);
    let layout = d.app().tl.layout.as_ref().expect("timeline layout");
    let window = layout.split_y - layout.content.min.y;
    let slots = d.app().session.active_sequence().unwrap().video_tracks.len() as f32 + 1.0 / 3.0;
    {
        let app = d.harness.state_mut();
        app.session.state.timeline_ui.track_slider_decouple = true;
        app.session.state.timeline_ui.track_slider_snap = true;
        app.session.state.timeline_ui.track_slider_smooth = false;
        app.ui.timeline.video_track_h = 100.0;
        // The captured opposite visible edge lies halfway through a track. It must remain at
        // that exact screen position without rounding to a nearby divider and limiting resize.
        app.ui.timeline.v_scroll = 5.5 * 100.0 - window;
    }
    d.frames(4);

    let before = &d.app().ui.timeline;
    let fixed_edge = (before.v_scroll + window) / before.video_track_h;
    let initial_height = before.video_track_h;
    d.drag("timeline.trackSlider.video.bottomCircle", 0.5, 35.0);
    let after = &d.app().ui.timeline;
    let fixed_after = (after.v_scroll + window) / after.video_track_h;
    assert!(after.video_track_h < initial_height - 5.0, "the inner video edge continues resizing past nearby dividers");
    assert!((fixed_after - fixed_edge).abs() < 0.02, "the opposite mid-row visible boundary stays pinned");

    let moved_edge = (after.v_scroll - 1.0) / after.video_track_h;
    assert!((moved_edge - moved_edge.round()).abs() < 0.05, "the moved edge settles on a white-line detent: {moved_edge}");
    assert!(after.v_scroll <= (slots * after.video_track_h - window).max(0.0) + 0.01);
}

#[test]
fn video_outer_edge_drag_updates_the_visible_range_before_release() {
    let mut d = Driver::demo();
    d.add_many_tracks(8);
    {
        let app = d.harness.state_mut();
        app.session.state.timeline_ui.track_slider_decouple = true;
        app.session.state.timeline_ui.track_slider_snap = false;
        app.session.state.timeline_ui.track_slider_smooth = false;
        app.ui.timeline.video_track_h = 100.0;
    }
    let layout = d.app().tl.layout.as_ref().expect("timeline layout");
    let window = layout.split_y - layout.content.min.y;
    // Keep the inner visible edge in the middle of V2, away from a white-line detent.
    d.harness.state_mut().ui.timeline.v_scroll = 2.5 * 100.0;
    d.frames(4);

    let bar = rect_of(&mut d, "timeline.trackSlider.video.track");
    let thumb_before = rect_of(&mut d, "timeline.trackSlider.video");
    let start = egui::pos2(thumb_before.center().x, thumb_before.min.y + thumb_before.width() * 0.5);
    let target_y = bar.min.y + 1.0;
    let initial_height = d.app().ui.timeline.video_track_h;
    let fixed_inner = d.app().ui.timeline.v_scroll / initial_height;
    let initial_outer = (d.app().ui.timeline.v_scroll + window) / initial_height;

    // Press then move over several frames while primary remains down. A release would hide the
    // bug: the stale active-drag geometry is reconciled only after the interaction ends.
    d.pointer_event(egui::Event::PointerMoved(start));
    d.pointer_event(egui::Event::PointerButton { pos: start, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
    for step in 1..=12 {
        let y = start.y + (target_y - start.y) * step as f32 / 12.0;
        d.pointer_event(egui::Event::PointerMoved(egui::pos2(start.x, y)));
    }

    let thumb_held = rect_of(&mut d, "timeline.trackSlider.video");
    let live = &d.app().ui.timeline;
    let live_inner = live.v_scroll / live.video_track_h;
    let live_outer = (live.v_scroll + window) / live.video_track_h;
    assert!(live.video_track_h < initial_height - 1.0, "outer edge resizes while held: {} -> {}", initial_height, live.video_track_h);
    assert!((live_inner - fixed_inner).abs() < 0.01, "the other visible boundary stays pinned while held: {fixed_inner} -> {live_inner}");
    assert!(live_outer > initial_outer + 0.05, "visible outer boundary moves toward the top while held: {initial_outer} -> {live_outer}");
    assert!(thumb_held.min.y <= start.y - 1.0, "rendered thumb follows the held outer-edge pointer in real time: {:?}, pointer y={}", thumb_held, target_y);

    d.pointer_event(egui::Event::PointerButton {
        pos: egui::pos2(start.x, target_y),
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: egui::Modifiers::NONE,
    });
    let thumb_released = rect_of(&mut d, "timeline.trackSlider.video");
    assert!(
        (thumb_released.min.y - thumb_held.min.y).abs() < 0.01 && (thumb_released.max.y - thumb_held.max.y).abs() < 0.01,
        "releasing the drag must not reconcile the visible thumb to a different position: {thumb_held:?} -> {thumb_released:?}"
    );
}

#[test]
fn resetting_one_track_control_keeps_its_sibling_bindings() {
    let mut d = Driver::demo();
    {
        let app = d.harness.state_mut();
        app.tl.track_slider_settings_pos = Some(egui::pos2(600.0, 220.0));
        app.session.shortcuts.set("timeline.trackSliderMirror", "Ctrl+K", None, false, true).expect("assign mirror shortcut");
        app.session.shortcuts.set("timeline.trackSliderExplode", "Ctrl+J", None, false, true).expect("assign explode shortcut");
        app.session.shortcuts.set("timeline.trackSliderAlternate", "Ctrl+U", None, false, true).expect("assign alternate shortcut");
        app.session.state.timeline_ui.track_control_mirror = "Ctrl + K".into();
        app.session.state.timeline_ui.track_control_explode = "Ctrl + J".into();
        app.session.state.timeline_ui.track_control_alternate_drag_speed = "Ctrl + U".into();
    }
    d.frames(4);

    let controls = rect_of(&mut d, "timeline.trackSlider.settings.controls");
    d.pointer_event(egui::Event::PointerMoved(controls.center()));
    d.frames(3);
    let reset_id = "timeline.trackSlider.settings.controlsFlyout.reset0";
    assert!(
        d.ok("ui.elements", json!({"prefix": reset_id})).as_array().is_some_and(|items| items.iter().any(|item| item["id"] == json!(reset_id))),
        "the reset control is available after opening the Controls flyout"
    );
    d.ok("ui.click", json!({"id": reset_id}));
    d.frames(3);

    let shortcuts = &d.app().session.shortcuts;
    assert_eq!(shortcuts.primary("timeline.trackSliderMirror").as_deref(), Some("Double Click"));
    assert_eq!(shortcuts.primary("timeline.trackSliderExplode").as_deref(), Some("Ctrl+J"));
    assert_eq!(shortcuts.primary("timeline.trackSliderAlternate").as_deref(), Some("Ctrl+U"));
    let settings = &d.app().session.state.timeline_ui;
    assert_eq!(settings.track_control_mirror, "Double Click");
    assert_eq!(settings.track_control_explode, "Ctrl + J");
    assert_eq!(settings.track_control_alternate_drag_speed, "Ctrl + U");
}

#[test]
fn slider_warnings_stay_above_both_settings_menus() {
    for (menu, warning_state, button) in [
        ("zoom", "timeline-right-click-dialog", "timeline.rightClickDialog.ok"),
        ("track", "timeline-right-click-dialog", "timeline.rightClickDialog.ok"),
        ("track", "timeline-snap-requires-decoupling", "timeline.trackSlider.snapRequiresDecoupling.ok"),
    ] {
        let mut d = Driver::demo();
        {
            let app = d.harness.state_mut();
            let pos = egui::pos2(670.0, 280.0);
            if menu == "zoom" {
                app.tl.zoombar_settings_pos = Some(pos);
            } else {
                app.tl.track_slider_settings_pos = Some(pos);
            }
        }
        d.harness.ctx.data_mut(|data| data.insert_temp(egui::Id::unique(warning_state), true));
        d.frames(4);

        assert!(
            d.ok("ui.elements", json!({"prefix": button})).as_array().is_some_and(|items| items.iter().any(|item| item["id"] == json!(button))),
            "the {warning_state} dialog is present above the {menu} settings menu"
        );
        d.ok("ui.click", json!({"id": button}));
        d.frames(2);
        assert!(!d.harness.ctx.data(|data| data.get_temp::<bool>(egui::Id::unique(warning_state)).unwrap_or(false)), "the warning button receives input");
    }
}

// Regression test for #703: the lines between the timeline's tracks were drawn in the timeline's own
// background colour, so they were invisible, and nothing marked the top of the highest video track.
#[test]
fn timeline_draws_visible_lines_between_tracks_and_above_the_top_video_track() {
    let mut d = Driver::demo();
    let (top, _) = d.labels();
    let separator = d.app().tokens.separator;
    let ruler = rect_of(&mut d, "timeline.ruler");
    let top_edge = rect_of(&mut d, &format!("timeline.track.{top}.sourcePatch"));
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
    assert!((first - (top_edge.min.y - 1.0)).abs() < 1.5, "the first line ({first}) is at the top edge of the highest video track ({})", top_edge.min.y - 1.0);
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

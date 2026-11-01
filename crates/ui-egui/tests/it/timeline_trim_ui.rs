//! Headless UI tests of trimming in the Timeline (#259): a press acts on what was under the
//! pointer when the button went down, the side of a cut picks the clip, and the cursor,
//! `ui.timeline.hit` and the press agree. The real `FilmcraftApp` under `egui_kittest` with the
//! demo project, the pointer moving a few pixels per frame as a mouse does.

use std::sync::mpsc::{Sender, channel};

use egui::{CursorIcon, Pos2, pos2};
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

/// Two clips of the demo project that share a cut on V1.
const CITY: &str = "City_Night_Drive.mp4";
const DUNES: &str = "Desert_Dunes.mp4";

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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn app(&self) -> &FilmcraftApp {
        self.harness.state()
    }

    /// Press at `from`, move to `to` over `steps` frames (as a mouse does), release.
    fn drag(&mut self, from: Pos2, to: Pos2, steps: usize) {
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(from));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.frames(1);
        for i in 1..=steps {
            push(self, egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(2);
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

    /// Render the window to `<target tmp>/ui-screenshots/<name>.png` (needs a GPU).
    fn screenshot(&mut self, name: &str) -> std::path::PathBuf {
        self.frames(2);
        let img = self.harness.render().expect("render");
        let png = filmcraft_ui_egui::control::encode_png(img.as_raw(), img.width(), img.height()).expect("png");
        let dir = std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("ui-screenshots");
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(format!("{name}.png"));
        std::fs::write(&path, png).unwrap();
        eprintln!("screenshot: {}", path.display());
        path
    }

    /// The clip named `name` on V1.
    fn v1_clip(&self, name: &str) -> u64 {
        let seq = self.app().session.active_sequence().expect("sequence");
        seq.video_tracks[0].items.iter().find(|i| i.name == name).unwrap_or_else(|| panic!("no {name} on V1")).id.0
    }

    /// A clip's (start, end) in ticks.
    fn span(&self, clip: u64) -> (i64, i64) {
        let seq = self.app().session.active_sequence().expect("sequence");
        let (_, it) = seq.find_item(ClipId(clip)).expect("clip");
        (it.start.0, it.end().0)
    }

    /// Screen x of a clip's In and Out edges, and a y 8 px below the top of its track: the band
    /// where edges are grabbed, above the lower part of a track where a transition on a cut takes
    /// the press (the demo has one on the City_Night_Drive / Desert_Dunes cut).
    fn edges(&mut self, clip: u64) -> (f32, f32, f32) {
        // `ui.timeline.locate` answers with points 2 px inside the edges
        let i = self.ok("ui.timeline.locate", json!({"clip": clip, "edge": "in"}));
        let o = self.ok("ui.timeline.locate", json!({"clip": clip, "edge": "out"}));
        let f = |v: &Value, k: &str| v[k].as_f64().expect("number") as f32;
        let id = format!("timeline.clip.{clip}");
        let els = self.ok("ui.elements", json!({"prefix": id}));
        let el = els.as_array().expect("elements").iter().find(|e| e["id"] == id.as_str()).unwrap_or_else(|| panic!("no {id}: {els}")).clone();
        let top = el["rect"][1].as_f64().expect("rect") as f32;
        (f(&i, "x") - 2.0, f(&o, "x") + 2.0, top + 8.0)
    }

    /// Ticks per screen pixel, from a clip's length on screen.
    fn ticks_per_px(&mut self, clip: u64) -> f64 {
        let (s, e) = self.span(clip);
        let (x0, x1, _) = self.edges(clip);
        (e - s) as f64 / f64::from(x1 - x0)
    }

    /// One frame of the sequence, in ticks.
    fn frame(&self) -> i64 {
        self.app().session.active_sequence().expect("sequence").settings.frame_rate.frame_duration().0
    }

    fn undo_steps(&self) -> usize {
        self.app().session.history.undo.len()
    }
}

/// #259: grabbing a clip's Out edge 3 px inside and dragging inward trims it, by as far as the
/// pointer moved. The drag is recognised only once the pointer has moved past egui's 6 pt drag
/// threshold, about 10 px inside the clip and so outside the 7 px edge zone, where the clip used to
/// be selected and moved instead.
#[test]
fn dragging_an_out_edge_inward_trims_the_clip() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let (x0, x1, y) = d.edges(city);
    let (s0, e0) = d.span(city);
    let (tpp, frame, undo0) = (d.ticks_per_px(city), d.frame(), d.undo_steps());
    let by = ((x1 - x0) / 4.0).round();
    d.drag(pos2(x1 - 3.0, y), pos2(x1 - 3.0 - by, y), by as usize);
    let (s1, e1) = d.span(city);
    assert_eq!(s1, s0, "the In edge stayed: trimmed, not moved");
    assert!(e1 < e0, "the Out edge moved in: {e0} → {e1}");
    let want = f64::from(by) * tpp;
    assert!(((e0 - e1) as f64 - want).abs() <= frame as f64, "the edge moved {} ticks for {by} px ({want} ticks) ± a frame ({frame})", e0 - e1);
    assert_eq!(d.undo_steps(), undo0 + 1, "one undo step");
}

/// Duplicate frame markers stay inside a clip while its Out edge is dragged in, and follow the
/// trim once it is released.
#[test]
fn duplicate_frame_markers_stay_inside_a_clip_being_trimmed() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let (item, start, source_in, duration) = {
        let (_, it) = d.app().session.active_sequence().expect("sequence").find_item(ClipId(city)).expect("clip");
        (it.item.0, it.start.0, it.source_in.0, it.duration.0)
    };
    d.exec("timeline.place", json!({"item": item, "track": "V2", "time": start, "sourceIn": source_in, "duration": duration, "audio": false}));
    d.exec("sequence.showDuplicateFrameMarkers", json!({"on": true}));
    d.frames(3);
    let (x0, x1, y) = d.edges(city);
    let by = ((x1 - x0) / 2.0).round();
    let stripe_right = |d: &mut Driver| {
        let els = d.ok("ui.elements", json!({"prefix": format!("timeline.duplicateFrames.{city}.")}));
        els.as_array()
            .expect("elements")
            .iter()
            .map(|e| (e["rect"][0].as_f64().expect("x") + e["rect"][2].as_f64().expect("w")) as f32)
            .fold(f32::MIN, f32::max)
    };
    // the markers are worked out on a background thread
    let t0 = std::time::Instant::now();
    while stripe_right(&mut d) == f32::MIN && t0.elapsed().as_secs() < 30 {
        d.frames(1);
        std::thread::sleep(std::time::Duration::from_millis(5));
    }
    assert!((stripe_right(&mut d) - x1).abs() <= 2.0, "the stripe runs to the Out edge");
    // press on the Out edge and move half the clip inward, without releasing
    let push = |d: &mut Driver, e: egui::Event| d.harness.input_mut().events.push(e);
    push(&mut d, egui::Event::PointerMoved(pos2(x1 - 3.0, y)));
    d.frames(1);
    push(&mut d, egui::Event::PointerButton { pos: pos2(x1 - 3.0, y), button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    d.frames(1);
    for i in 1..=by as usize {
        push(&mut d, egui::Event::PointerMoved(pos2(x1 - 3.0 - i as f32, y)));
        d.frames(1);
    }
    // (the painter's clip rect is rounded to whole pixels)
    let body_right = |d: &mut Driver| {
        let id = format!("timeline.clip.{city}");
        let els = d.ok("ui.elements", json!({"prefix": id}));
        let r = &els.as_array().expect("elements").iter().find(|e| e["id"] == id.as_str()).expect("clip")["rect"];
        (r[0].as_f64().expect("x") + r[2].as_f64().expect("w")) as f32
    };
    let (during, right) = (stripe_right(&mut d), body_right(&mut d));
    assert!(right < x1 - by / 2.0, "the clip is drawn trimmed: {right}");
    assert!(during <= right + 1.5, "inside the trimmed clip during the drag: {during} > {right}");
    push(
        &mut d,
        egui::Event::PointerButton { pos: pos2(x1 - 3.0 - by, y), button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() },
    );
    d.frames(3);
    let (_, e1) = d.span(city);
    assert!(e1 < start + duration, "trimmed");
    let (after, right) = (stripe_right(&mut d), body_right(&mut d));
    assert!(after <= right + 1.5, "after the trim: {after} > {right}");
}

/// Just right of a cut the clip on the right is grabbed: its In edge trims, and the clip on the
/// left stays as it was. The left clip's Out edge used to claim both sides of the cut.
#[test]
fn right_of_a_cut_trims_the_right_clip() {
    let mut d = Driver::demo();
    let (city, dunes) = (d.v1_clip(CITY), d.v1_clip(DUNES));
    assert_eq!(d.span(city).1, d.span(dunes).0, "the demo's City_Night_Drive and Desert_Dunes share a cut");
    let (cut, _, y) = d.edges(dunes);
    let (city0, (s0, e0)) = (d.span(city), d.span(dunes));
    d.drag(pos2(cut + 3.0, y), pos2(cut + 43.0, y), 40);
    let (s1, e1) = d.span(dunes);
    assert!(s1 > s0, "Desert_Dunes' In edge moved right: {s0} → {s1}");
    assert_eq!(e1, e0, "Desert_Dunes' Out edge stayed: trimmed, not moved");
    assert_eq!(d.span(city), city0, "City_Night_Drive is untouched");
}

/// A click that wobbles 5.5 px (still a click to egui, under its 6 pt limit) selects the edit point
/// that was pressed. It used to be hit-tested where the button came up, 8.5 px inside the clip and
/// outside the edge zone, and selected the clip.
#[test]
fn a_wobbly_click_on_an_edge_selects_the_edit_point() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let st = &mut d.harness.state_mut().session.state;
    st.selection.clear();
    st.edit_points.clear();
    let (_, x1, y) = d.edges(city);
    d.drag(pos2(x1 - 3.0, y), pos2(x1 - 8.5, y), 3);
    let st = &d.app().session.state;
    assert!(st.edit_points.iter().any(|e| e.clip == ClipId(city) && e.out), "City_Night_Drive's Out edge is the edit point: {:?}", st.edit_points);
    assert!(!st.selection.contains(&ClipId(city)), "the clip is not selected");
}

/// Click a real rolling edge, then the Trim Monitor's +1 button: linked sound follows exactly once.
fn linked_trim_monitor() -> Driver {
    let mut d = Driver::demo();
    let rate = d.app().session.sequence_rate();
    let video = ClipId(d.v1_clip(CITY));
    let q = d.app().session.active_sequence().unwrap();
    let picture = q.find_item(video).unwrap().1;
    let audio = q.audio_tracks[0].items.iter().find(|i| i.link.is_some() && i.link == picture.link).unwrap().id;
    let cut = picture.end();
    d.exec("tool.rolling", json!({}));
    d.frames(4);
    let (_, x, y) = d.edges(video.0);
    d.drag(pos2(x - 2.0, y), pos2(x - 2.0, y), 1);
    let points = &d.app().session.state.edit_points;
    assert_eq!(points.len(), 2, "both linked edges selected: {points:?}");
    assert!(points.iter().any(|e| e.clip == audio));
    let before = d.app().session.project.clone();
    d.ok("ui.click", json!({"id": "trimMonitor.forward"}));
    d.frames(4);
    for clip in [video, audio] {
        assert_eq!(d.span(clip.0).1, (cut + rate.frame_duration()).0);
    }
    let after = d.app().session.project.clone();
    d.exec("edit.undo", json!({}));
    assert_eq!(*d.app().session.project, *before);
    d.exec("edit.redo", json!({}));
    assert_eq!(*d.app().session.project, *after);
    d
}

/// #3085: the timeline gesture and Trim Monitor button keep picture and sound synchronized.
#[test]
fn trim_monitor_button_rolls_linked_audio_once() {
    linked_trim_monitor();
}

/// Visual check of the linked edit brackets and Trim Monitor after one frame of rolling.
#[test]
#[ignore = "renders a screenshot; requires a GPU"]
fn screenshot_linked_trim_monitor() {
    linked_trim_monitor().screenshot("linked-trim-monitor");
}

/// A moved clip stays under the point it was grabbed by. It used to lag behind the pointer by the
/// distance egui waits before it calls a press a drag.
#[test]
fn a_moved_clip_follows_the_pointer_from_where_it_was_grabbed() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    let (x0, x1, y) = d.edges(city);
    let (s0, _) = d.span(city);
    let (tpp, frame) = (d.ticks_per_px(city), d.frame());
    let mid = (x0 + x1) / 2.0;
    d.drag(pos2(mid, y), pos2(mid + 120.0, y), 30);
    let (s1, _) = d.span(city);
    let want = 120.0 * tpp;
    assert!(((s1 - s0) as f64 - want).abs() <= frame as f64, "moved {} ticks for 120 px ({want} ticks) ± a frame ({frame})", s1 - s0);
}

/// The cursor, `ui.timeline.hit` and the press agree on what is under the pointer: just left of
/// a cut the left clip's Out edge, just right of it the right clip's In edge, mid-clip no edge.
/// (That a drag from those points trims those edges is tested above, from the same points.)
#[test]
fn cursor_hit_test_and_press_agree_at_a_cut() {
    let mut d = Driver::demo();
    let (city, dunes) = (d.v1_clip(CITY), d.v1_clip(DUNES));
    let (x0, _, _) = d.edges(city);
    let (cut, _, y) = d.edges(dunes);
    let hit = |d: &mut Driver, x: f32, mods: Value| d.ok("ui.timeline.hit", json!({"x": x, "y": y, "modifiers": mods}));
    for (x, clip, edge) in [(cut - 3.0, city, "Out"), (cut + 3.0, dunes, "In")] {
        d.hover(pos2(x, y));
        assert_eq!(d.cursor(), CursorIcon::ResizeColumn, "trim cursor at x {x}");
        let h = hit(&mut d, x, json!({}));
        assert_eq!((h["clip"].as_u64(), h["edge"].as_str(), h["kind"].as_str()), (Some(clip), Some(edge), Some("trim")), "at x {x}: {h}");
        let h = hit(&mut d, x, json!({"command": true}));
        assert_eq!(h["kind"], "ripple", "Cmd at x {x}: {h}");
    }
    let mid = (x0 + cut) / 2.0;
    d.hover(pos2(mid, y));
    assert_eq!(d.cursor(), CursorIcon::Default, "no trim cursor mid-clip");
    let h = hit(&mut d, mid, json!({}));
    assert_eq!((h["clip"].as_u64(), h["kind"].clone()), (Some(city), Value::Null), "mid-clip: {h}");
}

/// Renders the hover bracket at the City_Night_Drive / Desert_Dunes cut to PNGs for visual
/// review (needs a GPU): `cargo test -p filmcraft-ui-egui --test it timeline_trim_ui:: -- --ignored
/// hover_bracket_screenshots`. Yellow on City_Night_Drive's Out edge just left of the cut and on
/// Desert_Dunes' In edge just right of it, red for the Ripple tool, both sides for the Rolling tool.
#[test]
#[ignore]
fn hover_bracket_screenshots() {
    let mut d = Driver::demo();
    let dunes = d.v1_clip(DUNES);
    let (cut, _, y) = d.edges(dunes);
    for (tool, name, x) in
        [("selection", "trim-out", cut - 3.0), ("selection", "trim-in", cut + 3.0), ("ripple", "ripple-in", cut + 3.0), ("rolling", "roll", cut + 3.0)]
    {
        d.ok("ui.set", json!({"tool": tool}));
        d.hover(pos2(x, y));
        d.screenshot(&format!("hover-{name}"));
    }
}

/// A caption's Out edge, grabbed 2 px inside and dragged inward, trims the caption. The gesture
/// used to be decided where the drag was recognised, about 9 px inside (2 px plus egui's 6 pt drag
/// threshold), past the 5 px edge zone, so the caption moved.
#[test]
fn dragging_a_caption_edge_inward_trims_it() {
    let mut d = Driver::demo();
    let id = d.exec("captions.add", json!({"seconds": 2.0, "durationSeconds": 2.0}))["caption"].as_u64().expect("caption id");
    d.frames(4);
    let name = format!("timeline.caption.{id}");
    let els = d.ok("ui.elements", json!({"prefix": name}));
    let el = els.as_array().expect("elements").iter().find(|e| e["id"] == name.as_str()).unwrap_or_else(|| panic!("no {name}: {els}")).clone();
    let r: Vec<f32> = el["rect"].as_array().expect("rect").iter().map(|v| v.as_f64().expect("number") as f32).collect();
    let (x1, y) = (r[0] + r[2], r[1] + r[3] / 2.0);
    let span = |d: &Driver| {
        let seq = d.app().session.active_sequence().expect("sequence");
        let (_, c) = seq.find_caption(ClipId(id)).expect("caption");
        (c.start.0, c.end().0)
    };
    let (s0, e0) = span(&d);
    d.drag(pos2(x1 - 2.0, y), pos2(x1 - 32.0, y), 30);
    let (s1, e1) = span(&d);
    assert_eq!(s1, s0, "the caption's start stayed: trimmed, not moved");
    assert!(e1 < e0, "the caption's end moved in: {e0} → {e1}");
}

/// Screen rect (x, y, w, h) of a caption block.
fn caption_rect(d: &mut Driver, id: u64) -> [f32; 4] {
    let name = format!("timeline.caption.{id}");
    let els = d.ok("ui.elements", json!({"prefix": name}));
    let el = els.as_array().expect("elements").iter().find(|e| e["id"] == name.as_str()).unwrap_or_else(|| panic!("no {name}: {els}")).clone();
    let r: Vec<f32> = el["rect"].as_array().expect("rect").iter().map(|v| v.as_f64().expect("number") as f32).collect();
    [r[0], r[1], r[2], r[3]]
}

/// A caption's (start, end) in ticks.
fn caption_span(d: &Driver, id: u64) -> (i64, i64) {
    let seq = d.app().session.active_sequence().expect("sequence");
    let (_, c) = seq.find_caption(ClipId(id)).expect("caption");
    (c.start.0, c.end().0)
}

fn add_caption(d: &mut Driver, start: i64, seconds: f64) -> u64 {
    let id = d.exec("captions.add", json!({"time": start, "durationSeconds": seconds}))["caption"].as_u64().expect("caption id");
    d.frames(4);
    id
}

/// Desert_Dunes, its In point (the cut with City_Night_Drive) and one second, in ticks.
fn dunes_cut(d: &Driver) -> (u64, i64, i64) {
    let dunes = d.v1_clip(DUNES);
    let sec = filmcraft_time::Tick::from_seconds_f64(1.0).0;
    let cut = d.span(dunes).0;
    assert!(cut >= 2 * sec, "the demo's cut is at least 2 s in");
    (dunes, cut, sec)
}

/// A caption ending a second before the cut, its Out edge dragged to 4 px short of the cut:
/// returns the caption's end and the cut.
fn trim_caption_out_near_the_cut(snapping: bool) -> (i64, i64) {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": snapping}));
    let (dunes, cut, sec) = dunes_cut(&d);
    let id = add_caption(&mut d, cut - 2 * sec, 1.0);
    let r = caption_rect(&mut d, id);
    let (x1, y) = (r[0] + r[2], r[1] + r[3] / 2.0);
    let (cut_x, _, _) = d.edges(dunes);
    // grabbed 2 px inside, so the edge ends 2 px right of the pointer
    d.drag(pos2(x1 - 2.0, y), pos2(cut_x - 4.0 - 2.0, y), 40);
    (caption_span(&d, id).1, cut)
}

/// #2192: with snapping on, a caption's edge trimmed close to a clip's edge lands on it, as a
/// clip's does; with snapping off the same drag stops where the pointer does.
#[test]
fn a_trimmed_caption_edge_snaps_to_a_cut() {
    let (end, cut) = trim_caption_out_near_the_cut(true);
    assert_eq!(end, cut, "the caption's Out edge snapped to the cut");
    let (end, cut) = trim_caption_out_near_the_cut(false);
    assert_ne!(end, cut, "with snapping off the same drag stops short of the cut");
}

/// Two one-second captions inside Desert_Dunes, the later one dragged left until its start is
/// 4 px after the earlier one's end: returns that start and that end.
fn move_caption_near_another(snapping: bool) -> (i64, i64) {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": snapping}));
    let (_, cut, sec) = dunes_cut(&d);
    let first = add_caption(&mut d, cut + 2 * sec, 1.0);
    let second = add_caption(&mut d, cut + 4 * sec, 1.0);
    let (a, b) = (caption_rect(&mut d, first), caption_rect(&mut d, second));
    let from = pos2(b[0] + b[2] / 2.0, b[1] + b[3] / 2.0);
    let by = b[0] - (a[0] + a[2]) - 4.0;
    d.drag(from, pos2(from.x - by, from.y), 40);
    (caption_span(&d, second).0, caption_span(&d, first).1)
}

/// #2192: with snapping on, a moved caption snaps to another caption's edge.
#[test]
fn a_moved_caption_snaps_to_another_caption() {
    let (start, end) = move_caption_near_another(true);
    assert_eq!(start, end, "the moved caption starts where the other one ends");
    let (start, end) = move_caption_near_another(false);
    assert_ne!(start, end, "with snapping off the same drag stops short of the other caption");
}

/// A start for a one-second caption with no other snap target (edit, marker, playhead, in/out)
/// within a second of either of its edges.
fn quiet_spot(d: &Driver) -> i64 {
    let (_, cut, sec) = dunes_cut(d);
    let seq = d.app().session.active_sequence().expect("sequence");
    let mut targets: Vec<i64> = seq.all_tracks().flat_map(|t| &t.items).flat_map(|i| [i.start.0, i.end().0]).collect();
    targets.extend(seq.markers.iter().map(|m| m.start.0));
    targets.extend(seq.mark_in.iter().chain(&seq.mark_out).map(|t| t.0));
    targets.push(d.app().session.playhead().0);
    (0..40)
        .map(|q| cut + q * sec / 4)
        .find(|&t| targets.iter().all(|&c| (c - t).abs() > sec && (c - (t + sec)).abs() > sec))
        .expect("a quiet second on the demo timeline")
}

/// A caption at a [`quiet_spot`] nudged 7.5 px right (past egui's 6 pt drag threshold, inside the
/// 9 px snap distance): its start before and after.
fn nudge_caption(snapping: bool) -> (i64, i64) {
    let mut d = Driver::demo();
    d.exec("sequence.snap", json!({"on": snapping}));
    let at = quiet_spot(&d);
    let id = add_caption(&mut d, at, 1.0);
    let r = caption_rect(&mut d, id);
    let from = pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let s0 = caption_span(&d, id).0;
    d.drag(from, pos2(from.x + 7.5, from.y), 20);
    (s0, caption_span(&d, id).0)
}

/// #2192: a moved caption does not snap to where it started, or a short drag would leave it in
/// place: with snapping on a short nudge moves it as far as with snapping off.
#[test]
fn a_moved_caption_does_not_snap_to_itself() {
    let (s0, on) = nudge_caption(true);
    let (_, off) = nudge_caption(false);
    assert!(off > s0, "with snapping off the nudge moves the caption: {s0} → {off}");
    assert_eq!(on, off, "snapping on, the nudge moves it as far");
}

/// #374: renders the Ambient_Score music clip mid-way through a trim of its In edge, with the
/// mouse button still down, for visual review (needs a GPU): `cargo test -p filmcraft-ui-egui
/// --test it timeline_trim_ui::trim_waveform_screenshots -- --ignored`. The waveform stays where it was
/// and is cut off at the moving edge; it used to squeeze the whole clip's waveform into the shorter
/// clip, so the sound under the pointer moved while dragging.
#[test]
#[ignore]
fn trim_waveform_screenshots() {
    let mut d = Driver::demo();
    let seq = d.app().session.active_sequence().expect("sequence");
    let music = seq.audio_tracks.iter().flat_map(|t| &t.items).find(|i| i.name.contains("Ambient")).expect("music clip").id.0;
    let (x0, _, y) = d.edges(music);
    // waveform peaks are read on a background thread
    for _ in 0..40 {
        std::thread::sleep(std::time::Duration::from_millis(50));
        d.frames(1);
    }
    d.screenshot("trim-waveform-before");
    let push = |d: &mut Driver, e: egui::Event| d.harness.input_mut().events.push(e);
    let to = pos2(x0 + 160.0, y);
    push(&mut d, egui::Event::PointerMoved(pos2(x0, y)));
    d.frames(1);
    push(&mut d, egui::Event::PointerButton { pos: pos2(x0, y), button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    d.frames(1);
    for i in 1..=20 {
        push(&mut d, egui::Event::PointerMoved(pos2(x0 + 160.0 * i as f32 / 20.0, y)));
        d.frames(1);
    }
    d.screenshot("trim-waveform-dragging");
    push(&mut d, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(2);
    d.screenshot("trim-waveform-released");
}

/// #653: an Out edge dragged far past the end of the clip's media stops there while the drag is
/// still in progress, at the same place the trim lands on release. It used to follow the pointer
/// past the media and then jump back when the button came up.
#[test]
fn an_edge_dragged_past_the_media_stops_at_its_end() {
    use filmcraft_ui_egui::panels::timeline::Drag;
    let mut d = Driver::demo();
    let last = d.app().session.active_sequence().expect("sequence").video_tracks[0].items.last().expect("a clip on V1").id.0;
    let (_, x1, y) = d.edges(last);
    let (_, e0) = d.span(last);
    let far = filmcraft_time::Tick(3600 * filmcraft_time::TICKS_PER_SECOND);
    let limit = filmcraft_engine::commands::trim_delta(&d.app().session, ClipId(last), filmcraft_edit::Edge::Out, filmcraft_edit::TrimMode::Regular, far)
        .expect("limit");
    let (from, to) = (pos2(x1 - 3.0, y), pos2(x1 + 300.0, y));
    let push = |d: &mut Driver, e: egui::Event| d.harness.input_mut().events.push(e);
    push(&mut d, egui::Event::PointerMoved(from));
    d.frames(1);
    push(&mut d, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    d.frames(1);
    for i in 1..=60 {
        push(&mut d, egui::Event::PointerMoved(from + (to - from) * (i as f32 / 60.0)));
        d.frames(1);
    }
    match &d.app().tl.drag {
        Some(Drag::Trim { delta, .. }) => assert_eq!(*delta, limit, "the dragged edge stops at the end of the media"),
        other => panic!("not trimming: {other:?}"),
    }
    push(&mut d, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(2);
    assert_eq!(d.span(last).1, e0 + limit.0, "released where it was shown");
}

/// With the Rolling Edit tool, a cut dragged far past the end of the right clip stops while the
/// drag is still in progress, at the same place the roll lands on release.
#[test]
fn a_cut_rolled_past_its_limit_stops_there() {
    use filmcraft_ui_egui::panels::timeline::Drag;
    let mut d = Driver::demo();
    let (city, dunes) = (d.v1_clip(CITY), d.v1_clip(DUNES));
    d.ok("ui.menu.invoke", json!({"id": "tool.rolling"}));
    let (cut, x1, y) = d.edges(dunes);
    let (_, e0) = d.span(city);
    let far = filmcraft_time::Tick(3600 * filmcraft_time::TICKS_PER_SECOND);
    let limit = filmcraft_engine::commands::roll_delta(&d.app().session, ClipId(city), ClipId(dunes), far).expect("limit");
    assert!(filmcraft_time::Tick::ZERO < limit && limit < far, "the roll has a limit: {limit:?}");
    let (from, to) = (pos2(cut + 3.0, y), pos2(x1 + 100.0, y));
    let push = |d: &mut Driver, e: egui::Event| d.harness.input_mut().events.push(e);
    push(&mut d, egui::Event::PointerMoved(from));
    d.frames(1);
    push(&mut d, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
    d.frames(1);
    for i in 1..=60 {
        push(&mut d, egui::Event::PointerMoved(from + (to - from) * (i as f32 / 60.0)));
        d.frames(1);
    }
    match &d.app().tl.drag {
        Some(Drag::Roll { delta, .. }) => assert_eq!(*delta, limit, "the rolled cut stops at the roll's limit"),
        other => panic!("not rolling: {other:?}"),
    }
    push(&mut d, egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
    d.frames(2);
    assert_eq!(d.span(city).1, e0 + limit.0, "released where it was shown");
    assert_eq!(d.span(dunes).0, e0 + limit.0);
}

/// Dragging a picture in the Trim Monitor must not deadlock: the shift bookkeeping used to call
/// `drag_delta()` inside `data_mut()`, and both take egui's non-reentrant context write-lock, so
/// the drag froze the whole app on its first recognised frame (the delta is read before locking
/// now). The drag commits one normal trim on release.
#[test]
fn dragging_in_the_trim_monitor_trims_without_hanging() {
    let mut d = Driver::demo();
    let city = d.v1_clip(CITY);
    d.exec("trim.selectEditPoint", json!({"clip": city, "edge": "out", "kind": "trim"}));
    d.frames(2);
    // the Program panel shows the two-up Trim Monitor once an edit point is selected
    let els = d.ok("ui.elements", json!({"prefix": "trimMonitor."}));
    let out =
        els.as_array().expect("elements").iter().find(|e| e["id"] == "trimMonitor.outgoing").unwrap_or_else(|| panic!("trim monitor not shown: {els}")).clone();
    let r: Vec<f32> = out["rect"].as_array().expect("rect").iter().map(|v| v.as_f64().expect("number") as f32).collect();
    let (cx, cy) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    // about 66 px of drag = the 6 pt threshold + 60 px of motion, at 6 px per frame;
    // inward (left) trims into the clip's media, outward would clamp at the seamless cut
    let (_, e0) = d.span(city);
    d.drag(pos2(cx, cy), pos2(cx - 66.0, cy), 33);
    let (_, e1) = d.span(city);
    let frames = (e1 - e0) / d.frame();
    assert!(e1 < e0, "the Out edge moved in by {} frames", frames.abs());
    assert!((6..=14).contains(&-frames), "about 11 frames expected for 60 px at 6 px/frame, moved {}", frames.abs());
}

fn locked_move_keeps_the_original_preview(audio: bool, destination: bool, linked_partner: bool, edge: Option<&str>) {
    let mut d = Driver::demo();
    d.exec("sequence.linkedSelection", json!({"on": linked_partner}));
    let seq = d.app().session.active_sequence().unwrap();
    let tracks = if audio { &seq.audio_tracks } else { &seq.video_tracks };
    let clip = tracks[0].items[0].id;
    let locked = if linked_partner { seq.audio_tracks[0].id } else { tracks[usize::from(destination)].id };
    d.exec("timeline.setTrack", json!({"track": locked.0, "locked": true}));
    d.frames(3);
    let prefix = format!("timeline.clip.{}", clip.0);
    let original = d.ok("ui.elements", json!({"prefix": prefix}))[0]["rect"].clone();
    let r = original.as_array().unwrap();
    let from = pos2(r[0].as_f64().unwrap() as f32 + r[2].as_f64().unwrap() as f32 * 0.5, r[1].as_f64().unwrap() as f32 + 8.0);
    let from = if let Some(edge) = edge {
        let point = d.ok("ui.timeline.locate", json!({"clip": clip.0, "edge": edge}));
        pos2(point["x"].as_f64().unwrap() as f32, from.y)
    } else {
        from
    };
    let mut to = from + egui::vec2(60.0, 0.0);
    if destination {
        let label = if audio { "A2" } else { "V2" };
        let rows = d.ok("ui.elements", json!({"prefix": format!("timeline.track.{label}.target")}));
        let y = &rows[0]["rect"];
        to.y = (y[1].as_f64().unwrap() + y[3].as_f64().unwrap() * 0.5) as f32;
    }
    let before = d.app().session.project.clone();
    let undo = d.app().session.history.undo.len();
    d.harness.input_mut().events.push(egui::Event::PointerMoved(from));
    d.frames(1);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    d.frames(1);
    for i in 1..=8 {
        d.harness.input_mut().events.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 8.0)));
        d.frames(1);
    }
    d.frames(2);
    let elements = d.ok("ui.elements", json!({"prefix": prefix}));
    assert_eq!(elements[0]["rect"], original, "a prohibited move never previews a displaced clip");
    assert_eq!(d.cursor(), CursorIcon::NotAllowed);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    d.frames(2);
    assert_eq!(serde_json::to_value(&*d.app().session.project).unwrap(), serde_json::to_value(&*before).unwrap());
    assert_eq!(d.app().session.history.undo.len(), undo);
}

#[test]
fn locked_sources_do_not_preview_a_move() {
    for audio in [false, true] {
        locked_move_keeps_the_original_preview(audio, false, false, None);
    }
}
#[test]
fn locked_destinations_do_not_preview_a_move() {
    for audio in [false, true] {
        locked_move_keeps_the_original_preview(audio, true, false, None);
    }
}
#[test]
fn a_locked_linked_partner_prevents_the_move_preview() {
    locked_move_keeps_the_original_preview(false, false, true, None);
}

#[test]
fn locked_edges_do_not_preview_a_trim() {
    for audio in [false, true] {
        for edge in ["in", "out"] {
            locked_move_keeps_the_original_preview(audio, false, false, Some(edge));
        }
    }
}

fn move_existing_clip_to_a_new_track(audio: bool, linked: bool) {
    let mut d = Driver::demo();
    d.exec("sequence.linkedSelection", json!({"on": linked}));
    d.exec("sequence.snap", json!({"on": false}));
    d.ok("ui.set", json!({"timeline":{"videoTrackHeight":32,"audioTrackHeight":32}}));
    d.frames(6);
    let q = d.app().session.active_sequence().unwrap();
    let tracks = if audio { &q.audio_tracks } else { &q.video_tracks };
    let count = tracks.len();
    let clip = tracks[0].items[0].id;
    let original = q.find_item(clip).unwrap().1.clone();
    let other = if audio { &q.video_tracks } else { &q.audio_tracks };
    let other_count = other.len();
    let partner = linked.then(|| {
        assert!(original.link.is_some());
        other.iter().flat_map(|t| &t.items).find(|i| i.link == original.link).unwrap().clone()
    });
    let layout = d.app().tl.layout.as_ref().unwrap();
    let row = layout.rows.iter().find(|r| r.track == tracks[0].id).unwrap();
    let outer = layout.rows.iter().find(|r| r.track == tracks.last().unwrap().id).unwrap();
    let from = egui::pos2(layout.x_of(original.start + filmcraft_time::Tick(original.duration.0 / 2)), row.rect.min.y + 8.0);
    let to = egui::pos2(from.x, if audio { outer.rect.max.y + 8.0 } else { outer.rect.min.y - 8.0 });
    assert!(layout.content.contains(to));
    assert!(layout.row_at(to.y).is_none());
    let before = d.app().session.project.clone();
    let undo = d.app().session.history.undo.len();
    d.harness.input_mut().events.push(egui::Event::PointerMoved(from));
    d.frames(1);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: from,
        button: egui::PointerButton::Primary,
        pressed: true,
        modifiers: Default::default(),
    });
    d.frames(1);
    for i in 1..=12 {
        d.harness.input_mut().events.push(egui::Event::PointerMoved(from + (to - from) * (i as f32 / 12.0)));
        d.frames(1);
    }
    // A frame updates the gesture, then the next frame paints its destination.
    d.frames(2);
    let preview = d.ok("ui.elements", json!({"prefix":format!("timeline.newTrackPreview.{}",clip.0)}));
    assert_eq!(preview.as_array().unwrap().len(), 1, "new destination is shown during the drag");
    assert_eq!(*d.app().session.project, *before);
    assert_eq!(d.app().session.history.undo.len(), undo);
    d.harness.input_mut().events.push(egui::Event::PointerButton {
        pos: to,
        button: egui::PointerButton::Primary,
        pressed: false,
        modifiers: Default::default(),
    });
    d.frames(2);
    let q = d.app().session.active_sequence().unwrap();
    let tracks = if audio { &q.audio_tracks } else { &q.video_tracks };
    assert_eq!(tracks.len(), count + 1, "drop creates a new outer track");
    assert_eq!(q.find_item(clip).unwrap().0, tracks.last().unwrap().id);
    assert_eq!(q.find_item(clip).unwrap().1, &original);
    let other = if audio { &q.video_tracks } else { &q.audio_tracks };
    assert_eq!(other.len(), other_count + usize::from(linked));
    if let Some(partner) = partner {
        assert_eq!(q.find_item(partner.id).unwrap().0, other.last().unwrap().id);
        assert_eq!(q.find_item(partner.id).unwrap().1, &partner);
    }
    assert_eq!(d.app().session.history.undo.len(), undo + 1);
    let after = d.app().session.project.clone();
    d.exec("edit.undo", json!({}));
    assert_eq!(*d.app().session.project, *before);
    d.exec("edit.redo", json!({}));
    assert_eq!(*d.app().session.project, *after);
}

#[test]
fn existing_video_clip_drag_above_tracks_adds_a_track() {
    move_existing_clip_to_a_new_track(false, false);
}

#[test]
fn existing_audio_clip_drag_below_tracks_adds_a_track() {
    move_existing_clip_to_a_new_track(true, false);
}

#[test]
fn linked_video_drag_adds_video_and_audio_tracks() {
    move_existing_clip_to_a_new_track(false, true);
}

#[test]
fn linked_audio_drag_adds_audio_and_video_tracks() {
    move_existing_clip_to_a_new_track(true, true);
}

/// #3300: with Alt held, dragging the edge of one side of a linked clip trims only that side; the
/// linked audio keeps its range. Without Alt the partner still follows.
#[test]
fn alt_dragging_an_edge_trims_only_that_side_of_a_link() {
    for alt in [true, false] {
        let mut d = Driver::demo();
        let seq = d.app().session.active_sequence().expect("sequence");
        let (video, sound) = seq.video_tracks[0]
            .items
            .iter()
            .find_map(|v| {
                let a = seq.audio_tracks.iter().flat_map(|t| &t.items).find(|a| v.link.is_some() && a.link == v.link)?;
                Some((v.id.0, a.id.0))
            })
            .expect("a linked clip on V1");
        let (v0, a0) = (d.span(video), d.span(sound));
        let (_, x1, y) = d.edges(video);
        d.ok("ui.drag", json!({"from": {"x": x1 - 3.0, "y": y}, "to": {"x": x1 - 63.0, "y": y}, "steps": 20, "modifiers": {"alt": alt}}));
        d.frames(2);
        let v1 = d.span(video);
        assert_eq!(v1.0, v0.0, "alt={alt}: the In point stays");
        assert!(v1.1 < v0.1, "alt={alt}: the video's Out edge was trimmed: {v0:?} -> {v1:?}");
        if alt {
            assert_eq!(d.span(sound), a0, "Alt-trim leaves the linked audio alone");
        } else {
            assert_eq!(d.span(sound).1, a0.1 - (v0.1 - v1.1), "the linked audio follows a plain trim");
        }
    }
}

impl Driver {
    /// Press at `from` and move to `to` over `steps` frames, the button still down.
    fn press_and_move(&mut self, from: Pos2, to: Pos2, steps: usize) {
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(from));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.frames(1);
        for i in 1..=steps {
            push(self, egui::Event::PointerMoved(from + (to - from) * (i as f32 / steps as f32)));
            self.frames(1);
        }
    }

    fn release(&mut self, at: Pos2) {
        self.harness.input_mut().events.push(egui::Event::PointerButton {
            pos: at,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        self.frames(2);
    }
}

/// #1780: the trim cursor stays while an edge is dragged, after the pointer has left the 7 px edge
/// zone, and goes back to the arrow on release. It used to show only on hover, so it turned back
/// into the arrow as soon as the drag began. The edge is dragged past the end of the media, where
/// it stops (#653), so the release leaves the pointer over empty track, away from any edge.
#[test]
fn the_trim_cursor_stays_while_an_edge_is_dragged() {
    use filmcraft_ui_egui::panels::timeline::Drag;
    let mut d = Driver::demo();
    let last = d.app().session.active_sequence().expect("sequence").video_tracks[0].items.last().expect("a clip on V1").id.0;
    let (_, x1, y) = d.edges(last);
    let (from, to) = (pos2(x1 - 3.0, y), pos2(x1 + 300.0, y));
    d.press_and_move(from, to, 60);
    assert!(matches!(d.app().tl.drag, Some(Drag::Trim { .. })), "trimming: {:?}", d.app().tl.drag);
    assert_eq!(d.cursor(), CursorIcon::ResizeColumn, "trim cursor while dragging, 300 px past the edge");
    d.release(to);
    assert_eq!(d.cursor(), CursorIcon::Default, "the arrow after the release");
}

/// #1780: as for a clip, the trim cursor stays while a transition's end is dragged.
#[test]
fn the_trim_cursor_stays_while_a_transition_end_is_dragged() {
    use filmcraft_ui_egui::panels::timeline::Drag;
    let mut d = Driver::demo();
    let id = d.app().session.active_sequence().expect("sequence").video_tracks[0].transitions.first().expect("the demo's transition on V1").id.0;
    let name = format!("timeline.transition.{id}.out");
    let els = d.ok("ui.elements", json!({"prefix": name}));
    let el = els.as_array().expect("elements").iter().find(|e| e["id"] == name.as_str()).unwrap_or_else(|| panic!("no {name}: {els}")).clone();
    let r: Vec<f32> = el["rect"].as_array().expect("rect").iter().map(|v| v.as_f64().expect("number") as f32).collect();
    let from = pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
    let to = from + egui::vec2(40.0, 0.0);
    d.hover(from);
    assert_eq!(d.cursor(), CursorIcon::ResizeColumn, "trim cursor over the transition's end");
    d.press_and_move(from, to, 40);
    assert!(matches!(d.app().tl.drag, Some(Drag::Transition { edge: Some(_), .. })), "dragging the end: {:?}", d.app().tl.drag);
    assert_eq!(d.cursor(), CursorIcon::ResizeColumn, "trim cursor while dragging, 40 px on");
    d.release(to);
}

//! Snapping a clip drag: Option (Alt) bypasses it on the frame it is held, and a snap is sticky so
//! the block does not flutter in and out of the edge it is stuck to while the pointer sits near it.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

const PPS: f64 = 40.0;
const FPS: f64 = 24.0;

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
    fn demo_with_clips(frames: &[i64]) -> Self {
        let mut d = Driver::demo();
        d.exec("file.newSequence", json!({"name": "Snap", "video": 1, "audio": 1}));
        let session = &d.harness.state().session;
        let item = session.project.items.values().find(|i| i.has_video() && i.has_audio() && i.as_media().is_some()).expect("a media item").id;
        let duration = session.sequence_rate().tick_of(48).0;
        for frame in frames {
            d.exec("timeline.place", json!({"item": item.0, "frame": frame, "duration": duration}));
        }
        d.ok("ui.set", json!({"timeline": {"pps": PPS, "scroll": 0.0}}));
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
        self.tx.send(req).expect("control channel");
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                assert_eq!(v["ok"], json!(true), "{method} {params} failed: {v}");
                return v["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn rect(&mut self, id: &str) -> egui::Rect {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().and_then(|a| a.iter().find(|e| e["id"] == id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().expect("rect").iter().map(|x| x.as_f64().expect("number")).collect();
        egui::Rect::from_min_size(egui::pos2(r[0] as f32, r[1] as f32), egui::vec2(r[2] as f32, r[3] as f32))
    }
    fn video_clips(&mut self) -> Vec<ClipId> {
        let seq = self.harness.state().session.active_sequence().expect("a sequence");
        seq.video_tracks.iter().flat_map(|t| t.items.iter().map(|i| i.id)).collect()
    }
}

/// Points of pointer x for `frames` frames of timeline time at the test's fixed zoom.
fn px(frames: i64) -> f32 {
    (frames as f64 / FPS * PPS) as f32
}

fn start_of(d: &Driver, clip: ClipId) -> i64 {
    d.harness.state().session.active_sequence().and_then(|q| q.find_item(clip)).map(|(_, i)| i.start.0).unwrap_or(-1)
}

/// The tick of a frame count in the sequence's timebase.
fn tick_of(d: &Driver, frames: i64) -> i64 {
    d.harness.state().session.sequence_rate().tick_of(frames).0
}

/// The start a free (unsnapped) drag to `raw` frames lands on: the frame-snapped offset from where
/// the clip was.
fn free_start(d: &Driver, clip: ClipId, raw: i64) -> i64 {
    start_of(d, clip) + tick_of(d, raw)
}

/// Presses in the middle of `clip`; the drag offset is measured from the pointer's x, so the grab
/// point only has to stay inside the block.
fn press(d: &mut Driver, clip: ClipId, mods: egui::Modifiers) -> egui::Pos2 {
    let r = d.rect(&format!("timeline.clip.{}", clip.0));
    let from = r.center();
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(mods));
    d.harness.input_mut().events.push(egui::Event::PointerMoved(from));
    d.frames(1);
    d.harness.input_mut().events.push(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: mods });
    d.frames(1);
    from
}

/// Moves the pointer so the drag offset is `raw` frames, one frame of UI per call.
fn to_offset(d: &mut Driver, from: egui::Pos2, raw: i64) {
    d.harness.input_mut().events.push(egui::Event::PointerMoved(from + egui::vec2(px(raw), 0.0)));
    d.frames(1);
}

/// Lets the button go at `raw`, with `mods` held at release (Option copies there).
fn release(d: &mut Driver, from: egui::Pos2, raw: i64, mods: egui::Modifiers) {
    let to = from + egui::vec2(px(raw), 0.0);
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(mods));
    d.harness.input_mut().events.push(egui::Event::PointerMoved(to));
    d.harness.input_mut().events.push(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: mods });
    d.frames(3);
}

#[test]
fn free_drags_land_exactly_where_the_pointer_lets_go() {
    let mut d = Driver::demo_with_clips(&[0]);
    d.exec("sequence.snap", json!({"on": false}));
    let clip = d.video_clips().first().copied().expect("a clip");
    // Three chained drags from the clip's new place each time: no snap, no rounding to edges.
    for raw in [70_i64, -38, 53] {
        let expected = free_start(&d, clip, raw);
        let from = press(&mut d, clip, egui::Modifiers::NONE);
        to_offset(&mut d, from, raw);
        release(&mut d, from, raw, egui::Modifiers::NONE);
        assert_eq!(start_of(&d, clip), expected, "raw {raw}");
    }
}

#[test]
fn a_drag_snaps_to_a_neighbouring_edit() {
    let mut d = Driver::demo_with_clips(&[0, 100]);
    let clips = d.video_clips();
    let clip = clips[0];
    // 2 frames short of the second clip's start: inside SNAP_PX, so the drag sticks to frame 100.
    let raw = 98;
    let from = press(&mut d, clip, egui::Modifiers::NONE);
    to_offset(&mut d, from, raw);
    release(&mut d, from, raw, egui::Modifiers::NONE);
    assert_eq!(start_of(&d, clip), tick_of(&d, 100), "snapped to the neighbour's start");
}

#[test]
fn option_bypasses_snapping_while_it_is_held() {
    let mut d = Driver::demo_with_clips(&[0, 100]);
    let clip = d.video_clips()[0];
    let raw = 98;
    let expected = free_start(&d, clip, raw);
    let from = press(&mut d, clip, egui::Modifiers::NONE);
    to_offset(&mut d, from, raw); // without Option this sticks to the neighbour
    // Option goes down mid-drag: the very next frame lets go of the snap and takes the raw offset.
    d.harness.input_mut().events.push(egui::Event::ModifiersChanged(egui::Modifiers::ALT));
    d.harness.input_mut().events.push(egui::Event::PointerMoved(from + egui::vec2(px(raw), 0.0)));
    d.frames(1);
    // Option is the copy modifier at the drop: let it go before the button comes up.
    release(&mut d, from, raw, egui::Modifiers::NONE);
    assert_eq!(start_of(&d, clip), expected, "held free of the neighbour");
    assert_eq!(d.video_clips().len(), 2, "a move, not a copy");
}

#[test]
fn a_stuck_snap_holds_until_the_pointer_pulls_well_away() {
    let mut d = Driver::demo_with_clips(&[0, 100]);
    let clip = d.video_clips()[0];
    // Stuck at the neighbour's start, then 10 px past it (over SNAP_PX, under 1.6 * SNAP_PX): the
    // drag keeps the snap instead of dropping it at the threshold.
    let from = press(&mut d, clip, egui::Modifiers::NONE);
    to_offset(&mut d, from, 98);
    to_offset(&mut d, from, 106);
    release(&mut d, from, 106, egui::Modifiers::NONE);
    assert_eq!(start_of(&d, clip), tick_of(&d, 100), "sticky snap held at 10 px");
    d.exec("edit.undo", json!({}));
    d.frames(2);
    assert_eq!(start_of(&d, clip), 0, "undo puts the clip back");

    // Pulled 2 * SNAP_PX past the stuck point: the drag breaks free and the raw offset wins.
    let expected = free_start(&d, clip, 116);
    let from = press(&mut d, clip, egui::Modifiers::NONE);
    to_offset(&mut d, from, 98);
    to_offset(&mut d, from, 116);
    release(&mut d, from, 116, egui::Modifiers::NONE);
    assert_eq!(start_of(&d, clip), expected, "broke free of the snap");
    assert_ne!(start_of(&d, clip), tick_of(&d, 100));
}

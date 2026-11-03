//! Headless UI tests of box (marquee) selection in the Timeline: the selection follows the box
//! while it is dragged, Shift adds to the selection the drag began with, and Escape cancels.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    modifiers: egui::Modifiers,
}

impl Driver {
    fn demo() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, modifiers: egui::Modifiers::NONE };
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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn rect(&mut self, id: &str) -> egui::Rect {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
    }

    fn send(&mut self, e: egui::Event) {
        let modifiers = self.modifiers;
        self.harness.input_mut().events.extend([egui::Event::ModifiersChanged(modifiers), e]);
        self.frames(1);
    }

    fn button(&mut self, pos: egui::Pos2, pressed: bool) {
        let modifiers = self.modifiers;
        self.send(egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers });
    }

    /// Move the pointer to `to` in small steps, a frame each.
    fn move_to(&mut self, from: egui::Pos2, to: egui::Pos2) {
        for k in 1..=8 {
            self.send(egui::Event::PointerMoved(from.lerp(to, k as f32 / 8.0)));
        }
    }

    fn selected(&self, clip: u64) -> bool {
        self.harness.state().session.state.selection.contains(&ClipId(clip))
    }

    /// The last two clips of Video 1, and a point on Video 1 just past the end of the sequence.
    fn setup(&mut self) -> (u64, u64, egui::Rect, egui::Rect, egui::Pos2) {
        self.ok("ui.set", json!({"focused": "Timeline"}));
        let seq = self.exec("sequence.inspect", json!({}));
        let items = seq["video"][0]["items"].as_array().unwrap().clone();
        let id = |i: &Value| i["clip"].as_u64().unwrap();
        let (a, b) = (id(&items[items.len() - 2]), id(&items[items.len() - 1]));
        let (ra, rb) = (self.rect(&format!("timeline.clip.{a}")), self.rect(&format!("timeline.clip.{b}")));
        let start = egui::pos2(rb.max.x + 12.0, rb.center().y);
        assert_eq!(self.ok("ui.timeline.hit", json!({"x": start.x, "y": start.y}))["hit"], "empty", "the drag starts on empty space");
        (a, b, ra, rb, start)
    }
}

#[test]
fn the_selection_follows_the_box_while_it_is_dragged() {
    let mut d = Driver::demo();
    let (a, b, ra, rb, start) = d.setup();
    d.exec("timeline.select", json!({"clips": []}));
    d.send(egui::Event::PointerMoved(start));
    d.button(start, true);
    // into the last clip: selected before the button comes up
    let over_b = egui::pos2(rb.center().x, start.y + 2.0);
    d.move_to(start, over_b);
    assert!(d.selected(b) && !d.selected(a), "the box selects what it touches as it grows");
    // on into the clip before it
    let over_a = egui::pos2(ra.center().x, start.y + 2.0);
    d.move_to(over_b, over_a);
    assert!(d.selected(a) && d.selected(b));
    // and back: the clip that left the box is no longer selected
    d.move_to(over_a, over_b);
    assert!(d.selected(b) && !d.selected(a), "a clip leaving the box is deselected");
    d.button(over_b, false);
    d.frames(2);
    assert!(d.selected(b) && !d.selected(a), "the release keeps what the box showed");
}

#[test]
fn alt_box_selects_only_video_and_delete_keeps_linked_audio() {
    let mut d = Driver::demo();
    let (_, video, _, rv, start) = d.setup();
    let audio = {
        let seq = d.harness.state().session.active_sequence().unwrap();
        let link = seq.find_item(ClipId(video)).unwrap().1.link;
        seq.audio_tracks.iter().flat_map(|tr| &tr.items).find(|clip| clip.link == link).unwrap().id.0
    };
    d.exec("timeline.select", json!({"clips": []}));
    d.modifiers = egui::Modifiers::ALT;
    d.send(egui::Event::PointerMoved(start));
    d.button(start, true);
    let over = egui::pos2(rv.center().x, start.y + 2.0);
    d.move_to(start, over);
    assert!(d.selected(video));
    assert!(!d.selected(audio), "Alt excludes the linked audio during the drag");
    d.button(over, false);
    d.modifiers = egui::Modifiers::NONE;
    d.frames(2);
    assert!(d.selected(video) && !d.selected(audio), "release preserves the individual selection");
    d.exec("edit.clear", json!({}));
    let seq = d.harness.state().session.active_sequence().unwrap();
    assert!(seq.find_item(ClipId(video)).is_none());
    assert!(seq.find_item(ClipId(audio)).is_some(), "Delete leaves the unselected linked audio");
}

#[test]
fn shift_adds_to_the_selection_the_drag_began_with() {
    let mut d = Driver::demo();
    let (a, b, _, rb, start) = d.setup();
    d.exec("timeline.select", json!({"clips": [a]}));
    d.modifiers = egui::Modifiers::SHIFT;
    d.send(egui::Event::PointerMoved(start));
    d.button(start, true);
    let over_b = egui::pos2(rb.center().x, start.y + 2.0);
    d.move_to(start, over_b);
    assert!(d.selected(a) && d.selected(b), "Shift keeps the earlier selection and adds the box");
    // shrinking the box to nothing leaves the selection the drag began with
    d.move_to(over_b, start + egui::vec2(-2.0, 2.0));
    assert!(d.selected(a) && !d.selected(b));
    d.move_to(start + egui::vec2(-2.0, 2.0), over_b);
    d.button(over_b, false);
    d.modifiers = egui::Modifiers::NONE;
    d.frames(2);
    assert!(d.selected(a) && d.selected(b));
}

#[test]
fn escape_cancels_the_box_and_restores_the_selection() {
    let mut d = Driver::demo();
    let (a, b, ra, _, start) = d.setup();
    d.exec("timeline.select", json!({"clips": [a]}));
    d.send(egui::Event::PointerMoved(start));
    d.button(start, true);
    let over_a = egui::pos2(ra.center().x, start.y + 2.0);
    d.move_to(start, over_a);
    assert!(d.selected(b));
    d.send(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
    d.button(over_a, false);
    d.frames(2);
    assert!(d.selected(a) && !d.selected(b), "Escape put back the selection the drag began with");
}

/// Only the primary button draws the box: a right-button drag over clips selects none of them.
#[test]
fn a_right_button_drag_draws_no_box() {
    let mut d = Driver::demo();
    let (a, b, ra, rb, start) = d.setup();
    d.exec("timeline.select", json!({"clips": []}));
    let modifiers = d.modifiers;
    let secondary = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Secondary, pressed, modifiers };
    d.send(egui::Event::PointerMoved(start));
    d.send(secondary(start, true));
    let over_b = egui::pos2(rb.center().x, start.y + 2.0);
    d.move_to(start, over_b);
    let over_a = egui::pos2(ra.center().x, start.y + 2.0);
    d.move_to(over_b, over_a);
    assert!(!d.selected(a) && !d.selected(b), "a right-button drag selected clips");
    d.send(secondary(over_a, false));
    d.frames(2);
    assert!(!d.selected(a) && !d.selected(b));
}

#[test]
fn scrolling_mid_drag_keeps_the_box_anchored_to_the_content() {
    let mut d = Driver::demo();
    // scrolled right a little, so the view can move back while the box is open
    d.ok("ui.set", json!({"timeline": {"scroll": 10.0}}));
    d.frames(40);
    let (_, b, _, rb, start) = d.setup();
    d.exec("timeline.select", json!({"clips": []}));
    d.send(egui::Event::PointerMoved(start));
    d.button(start, true);
    let over_b = egui::pos2(rb.center().x, start.y + 2.0);
    d.move_to(start, over_b);
    assert!(d.selected(b), "the box reaches the last clip before the view scrolls");
    // scroll the content right by more than the clip is wide, the pointer held still: the clip ends
    // up past where the box began on screen, but the box's corner moved with the content
    let shift_px = rb.width() + 40.0;
    let pps = d.harness.state().ui.timeline.pps as f32;
    d.ok("ui.set", json!({"timeline": {"scroll": 10.0 - (shift_px / pps) as f64}}));
    d.frames(60);
    let rb_now = d.rect(&format!("timeline.clip.{b}"));
    assert!(rb_now.min.x > start.x + 10.0, "the clip really moved past the screen position the drag began at");
    assert!(d.selected(b), "scrolling does not drop the clip from the box");
    d.button(over_b, false);
    d.frames(2);
    assert!(d.selected(b), "the release keeps what the box showed");
}

impl Driver {
    fn wheel(&mut self, dy: f32) {
        self.send(egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            source: egui::MouseWheelSource::Wheel,
            delta: egui::vec2(0.0, dy),
            modifiers: egui::Modifiers::NONE,
            phase: egui::TouchPhase::Move,
        });
    }

    fn selection(&self) -> Vec<ClipId> {
        let mut s = self.harness.state().session.state.selection.clone();
        s.sort();
        s
    }
}

/// Review of #3583: the audio tracks scroll on their own, so a box begun on them keeps its corner
/// on them while the wheel scrolls them under a pointer held still.
#[test]
fn scrolling_the_audio_tracks_mid_drag_keeps_the_box_on_them() {
    let mut d = Driver::demo();
    d.exec("sequence.addTracks", json!({"audio": 12}));
    d.ok("ui.set", json!({"focused": "Timeline"}));
    d.frames(4);
    let seq = d.exec("sequence.inspect", json!({}));
    let items = seq["audio"][0]["items"].as_array().unwrap().clone();
    let b = items[items.len() - 1]["clip"].as_u64().unwrap();
    let rb = d.rect(&format!("timeline.clip.{b}"));
    let start = egui::pos2(rb.max.x + 12.0, rb.center().y);
    assert_eq!(d.ok("ui.timeline.hit", json!({"x": start.x, "y": start.y}))["hit"], "empty", "the drag starts on empty space");
    let ah = d.harness.state().ui.timeline.audio_track_h;
    d.exec("timeline.select", json!({"clips": []}));
    d.send(egui::Event::PointerMoved(start));
    d.button(start, true);
    // into the last clip of Audio 1 and down onto the track below it
    let over = egui::pos2(rb.center().x, start.y + ah);
    d.move_to(start, over);
    assert!(d.selected(b), "the box reaches the clip before the tracks scroll");
    // wheel down over the audio tracks: they move up most of a track, the pointer held still
    let dy = (ah * 0.75).round();
    d.wheel(-dy);
    d.frames(2);
    assert_eq!(d.harness.state().ui.timeline.a_scroll, dy, "the audio tracks scrolled");
    let rb_now = d.rect(&format!("timeline.clip.{b}"));
    assert!(rb_now.max.y < start.y, "the clip really moved up past where the drag began: {rb_now:?}");
    assert!(d.selected(b), "the box's corner moved up with the clip it began beside");
    d.button(over, false);
    d.frames(2);
    assert!(d.selected(b), "the release keeps what the box showed");
}

/// Review of #3583: a wheel turn mid-drag changes the scroll after the tracks were laid out for the
/// frame; the box follows the tracks as drawn, not the scroll they have yet to be drawn at. At the
/// lowest video scroll, wheeling further must not stretch the box over the tracks above, even for a frame.
#[test]
fn a_wheel_turn_past_the_end_of_the_video_scroll_does_not_move_the_box() {
    let mut d = Driver::demo();
    let (_, b, _, _, start) = d.setup();
    assert_eq!(d.harness.state().ui.timeline.v_scroll, 0.0);
    let left = d.rect("timeline.tracks").min.x + 20.0;
    d.exec("timeline.select", json!({"clips": []}));
    d.send(egui::Event::PointerMoved(start));
    d.button(start, true);
    // along Video 1 to the start of the view
    let over = egui::pos2(left, start.y + 2.0);
    d.move_to(start, over);
    assert!(d.selected(b));
    let before = d.selection();
    // the video tracks are already as low as they go: the wheel cannot scroll them
    d.wheel(-400.0);
    assert_eq!(d.selection(), before, "the box did not reach the tracks above for the frame of the wheel turn");
    d.frames(2);
    assert_eq!(d.harness.state().ui.timeline.v_scroll, 0.0);
    assert_eq!(d.selection(), before);
    d.button(over, false);
    d.frames(2);
    assert_eq!(d.selection(), before, "the release keeps what the box showed");
}

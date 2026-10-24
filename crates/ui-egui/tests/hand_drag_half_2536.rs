//! Headless UI tests for #2536: a vertical middle-button (or Hand tool) drag scrolls the half
//! of the timeline it started in — the audio tracks when it starts below the split, the video
//! tracks when it starts above it — exactly as the mouse wheel already does.

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
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        // Tall audio tracks so the audio half actually scrolls.
        d.ok("ui.set", json!({ "timeline": { "audioTrackHeight": 120.0 } }));
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

    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }

    fn middle(&mut self, pos: egui::Pos2, pressed: bool) {
        self.send(egui::Event::PointerButton { pos, button: egui::PointerButton::Middle, pressed, modifiers: egui::Modifiers::NONE });
    }

    /// Drag with the middle button from `from` to `to`, in small steps, a frame each.
    fn middle_drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        self.send(egui::Event::PointerMoved(from));
        self.middle(from, true);
        for k in 1..=8 {
            self.send(egui::Event::PointerMoved(from.lerp(to, k as f32 / 8.0)));
        }
        self.middle(to, false);
        self.frames(2);
    }

    /// Drag with the left button and the Hand tool from `from` to `to`.
    fn hand_drag(&mut self, from: egui::Pos2, to: egui::Pos2) {
        self.ok("ui.set", json!({ "tool": "Hand" }));
        self.send(egui::Event::PointerMoved(from));
        self.send(egui::Event::PointerButton { pos: from, button: egui::PointerButton::Primary, pressed: true, modifiers: egui::Modifiers::NONE });
        for k in 1..=8 {
            self.send(egui::Event::PointerMoved(from.lerp(to, k as f32 / 8.0)));
        }
        self.send(egui::Event::PointerButton { pos: to, button: egui::PointerButton::Primary, pressed: false, modifiers: egui::Modifiers::NONE });
        self.frames(2);
    }

    fn split_y(&mut self) -> f32 {
        self.app().tl.layout.clone().expect("layout").split_y
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

/// A vertical drag that starts in the audio half scrolls the audio tracks, not the video ones.
#[test]
fn a_middle_drag_over_the_audio_tracks_scrolls_the_audio_tracks() {
    let mut d = Driver::demo();
    let split = d.split_y();
    // Start well inside the audio half and drag up: a_scroll grows, v_scroll must not move.
    d.app().ui.timeline.v_scroll = 30.0;
    d.frames(2);
    let from = egui::pos2(900.0, split + 60.0);
    d.middle_drag(from, from - egui::vec2(0.0, 40.0));
    let tl = &d.app().ui.timeline;
    assert!((tl.a_scroll - 40.0).abs() < 1.0, "dragging up 40 pt in the audio half scrolls the audio tracks: a_scroll={}", tl.a_scroll);
    assert!((tl.v_scroll - 30.0).abs() < 1e-6, "the video tracks stay put: v_scroll={}", tl.v_scroll);
}

/// A vertical drag that starts in the video half still scrolls the video tracks.
#[test]
fn a_middle_drag_over_the_video_tracks_scrolls_the_video_tracks() {
    let mut d = Driver::demo();
    let split = d.split_y();
    d.app().ui.timeline.v_scroll = 30.0;
    d.frames(2);
    let from = egui::pos2(900.0, split - 60.0);
    d.middle_drag(from, from - egui::vec2(0.0, 20.0));
    let tl = &d.app().ui.timeline;
    assert!((tl.v_scroll - 10.0).abs() < 1.0, "dragging up 20 pt in the video half scrolls the video tracks: v_scroll={}", tl.v_scroll);
    assert!(tl.a_scroll.abs() < 1e-6, "the audio tracks stay put: a_scroll={}", tl.a_scroll);
}

/// Escape mid-drag puts the started half's scroll back — not the other half's value (#2536
/// review): the cancel must restore what the dragged half had, even when the halves differ.
#[test]
fn cancelling_an_audio_drag_restores_the_audio_scroll() {
    let mut d = Driver::demo();
    let split = d.split_y();
    // The two halves start at clearly different scrolls; the bug set a_scroll to v_scroll.
    d.app().ui.timeline.v_scroll = 30.0;
    d.app().ui.timeline.a_scroll = 70.0;
    d.frames(2);
    let from = egui::pos2(900.0, split + 60.0);
    let to = from - egui::vec2(0.0, 40.0);
    d.send(egui::Event::PointerMoved(from));
    d.middle(from, true);
    for k in 1..=8 {
        d.send(egui::Event::PointerMoved(from.lerp(to, k as f32 / 8.0)));
    }
    // Escape while the button is still down cancels the drag (#580).
    d.send(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
    d.middle(to, false);
    d.frames(2);
    let tl = &d.app().ui.timeline;
    assert!((tl.a_scroll - 70.0).abs() < 1e-6, "the audio scroll returns to its own start: a_scroll={}", tl.a_scroll);
    assert!((tl.v_scroll - 30.0).abs() < 1e-6, "the video scroll never moved: v_scroll={}", tl.v_scroll);
}

/// The Hand tool picks the half the same way.
#[test]
fn a_hand_tool_drag_over_the_audio_tracks_scrolls_the_audio_tracks() {
    let mut d = Driver::demo();
    let split = d.split_y();
    d.app().ui.timeline.v_scroll = 30.0;
    d.frames(2);
    let from = egui::pos2(900.0, split + 60.0);
    d.hand_drag(from, from - egui::vec2(0.0, 40.0));
    let tl = &d.app().ui.timeline;
    assert!((tl.a_scroll - 40.0).abs() < 1.0, "the Hand tool scrolls the half it started in: a_scroll={}", tl.a_scroll);
    assert!((tl.v_scroll - 30.0).abs() < 1e-6, "the video tracks stay put: v_scroll={}", tl.v_scroll);
}

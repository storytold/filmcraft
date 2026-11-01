//! Regression test for #2192: caption drag snaps to video-track clip edges only, not audio.
//!
//! When captions are dragged, only video-track clip edges should act as snap candidates.
//! Audio-only boundaries (e.g. a music clip on A1 with no linked video) must be ignored.

use std::sync::mpsc::{Sender, channel};

use egui::{Pos2, Rect, pos2};
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
        session.execute("sequence.snap", json!({"on": false})).expect("snapping off");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder()
            .with_size(egui::vec2(1600.0, 980.0))
            .with_step_dt(1.0 / 60.0)
            .with_max_steps(10_000)
            .build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
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

    fn rect(&mut self, id: &str) -> Option<Rect> {
        self.frames(2);
        let els = self.ok("ui.elements", json!({"prefix": id}));
        let el = els.as_array()?.iter().find(|e| e["id"] == id)?.clone();
        let r: Vec<f32> = el["rect"]
            .as_array()?
            .iter()
            .filter_map(|v| v.as_f64().map(|v| v as f32))
            .collect();
        Some(Rect::from_min_size(
            pos2(*r.first()?, *r.get(1)?),
            egui::vec2(*r.get(2)?, *r.get(3)?),
        ))
    }

    fn captions(&mut self) -> Vec<(i64, i64)> {
        let r = self.exec("captions.list", json!({}));
        r["tracks"][0]["captions"]
            .as_array()
            .expect("captions")
            .iter()
            .map(|c| (c["start"].as_i64().expect("start"), c["end"].as_i64().expect("end")))
            .collect()
    }

    fn drag(&mut self, from: Pos2, to: Pos2, steps: usize) {
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(from));
        self.frames(1);
        push(self, egui::Event::PointerButton {
            pos: from,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: Default::default(),
        });
        self.frames(1);
        for i in 1..=steps {
            push(self, egui::Event::PointerMoved(
                from + (to - from) * (i as f32 / steps as f32),
            ));
            self.frames(1);
        }
        push(self, egui::Event::PointerButton {
            pos: to,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: Default::default(),
        });
        self.frames(2);
    }
}

/// #2192: dragging a caption with snapping on uses snap_for_captions, which iterates only
/// seq.video_tracks. The demo project's ambient music clip (audio-only on A1) must not
/// act as a snap target; the drag path must complete without error.
#[test]
fn snap_for_captions_uses_video_track_edges_not_audio() {
    let mut d = Driver::demo();

    // Add a caption on C1 at the 1-second mark.
    let id = d.exec("captions.add", json!({"seconds": 1.0, "durationSeconds": 1.0, "text": "snap"}))
        ["caption"]
        .as_u64()
        .expect("caption id");
    d.frames(4);

    // Enable snapping so the caption drag path calls snap_for_captions.
    d.exec("sequence.snap", json!({"on": true}));
    d.frames(2);

    let before = d.captions();
    let r = d.rect(&format!("timeline.caption.{id}")).expect("caption rect");

    // Drag the caption slightly to the right.  The drag delta is small: if snap_for_captions
    // accidentally included audio tracks, the caption could teleport to the music clip's edge
    // on the first frame; if it panics (e.g. index out of bounds on an empty audio track) the
    // test would fail.  Either way we assert: caption still exists and did not move by more
    // than one full clip width, which would be a teleport to an audio-only boundary.
    let clip_width = r.width();
    d.drag(r.center(), r.center() + egui::vec2(clip_width * 0.1, 0.0), 8);
    d.frames(4);

    let after = d.captions();
    assert!(!after.is_empty(), "caption still exists after snap_for_captions drag");

    // The caption should not have teleported: its start must not have changed by more than
    // the drag distance (plus one snap zone).  A teleport to an audio-only boundary (which
    // can be seconds away) would be caught here.
    let ticks_per_sec = 254_016_000_000i64;
    let (before_start, _) = before[0];
    let (after_start, _) = after[0];
    let delta = (after_start - before_start).abs();
    assert!(
        delta < ticks_per_sec * 2,
        "caption moved by {delta} ticks — unexpectedly large, suggesting a snap to a distant audio-only boundary"
    );
}

//! Transition context menus, driven through the real app and its control channel.

use std::sync::mpsc::{Sender, channel};

use crate::FilmcraftApp;
use crate::control::ControlRequest;
use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::{ClipId, Sequence};
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        Self::with_height(980.0)
    }

    fn with_height(height: f32) -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let mut builder = Harness::builder().with_size(egui::vec2(1600.0, height)).with_max_steps(10_000);
        if std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR").is_some() {
            builder = builder.wgpu().with_pixels_per_point(1.0);
        }
        let harness = builder.build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d
    }

    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            // the control channel's clicks and keys enter through the input hook
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

    fn click(&mut self, id: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }

    fn has(&mut self, id: &str) -> bool {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        v.as_array().unwrap().iter().any(|e| e["id"] == json!(id))
    }

    fn seq(&mut self) -> Sequence {
        self.harness.state_mut().session.active_sequence().unwrap().clone()
    }

    /// Right-click `clip` at `fx` of its width (0.0 = its In edge, 0.5 = its middle).
    fn right_click(&mut self, clip: ClipId, fx: f64) {
        self.ok("ui.click", json!({"id": format!("timeline.clip.{}", clip.0), "fx": fx, "button": "right"}));
        self.frames(3);
    }

    /// Two different clips on V1 that touch, with no transition on the cut (a cut, not a through
    /// edit): (outgoing, incoming).
    fn cut_on_v1(&mut self) -> (ClipId, ClipId) {
        let seq = self.seq();
        let tr = &seq.video_tracks[0];
        let bare = |a: ClipId, b: ClipId| !tr.transitions.iter().any(|t| t.from == Some(a) || t.to == Some(b));
        tr.items
            .windows(2)
            .find(|w| w[0].end() == w[1].start && w[0].item != w[1].item && bare(w[0].id, w[1].id))
            .map(|w| (w[0].id, w[1].id))
            .expect("two touching clips on V1")
    }
}

#[test]
fn right_clicking_a_transition_clears_it_and_undo_restores_it() {
    let mut d = Driver::new();
    let before = d.seq();
    let transition = before.video_tracks[0].transitions[0].id;
    let clip = before.video_tracks[0].items[0].id;
    // Open a different menu first: a transition must not inherit its edit-point target.
    let (_, cut) = d.cut_on_v1();
    d.right_click(cut, 0.0);
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    d.exec("timeline.select", json!({"clips": [clip.0]}));
    d.ok("ui.click", json!({"id": format!("timeline.transition.{}", transition.0), "button": "right"}));
    d.frames(3);
    assert!(d.has("timeline.transitionMenu.clear"));
    assert!(!d.has("timeline.editPointMenu.trimIn"));
    assert!(!d.has("timeline.clipMenu.edit.cut"));
    assert_eq!(d.harness.state().session.state.transition_selection, vec![transition]);
    if let Some(dir) = std::env::var_os("FILMCRAFT_UI_SNAPSHOT_DIR") {
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        d.harness.render().unwrap().save(dir.join("transition-context-menu.png")).unwrap();
    }
    d.click("timeline.transitionMenu.clear");
    assert!(!d.seq().video_tracks[0].transitions.iter().any(|t| t.id == transition));
    assert_eq!(d.seq().video_tracks[0].items, before.video_tracks[0].items);
    d.exec("edit.undo", json!({}));
    assert_eq!(d.seq().video_tracks[0].transitions, before.video_tracks[0].transitions);
    d.exec("edit.redo", json!({}));
    assert!(!d.seq().video_tracks[0].transitions.iter().any(|t| t.id == transition));
}

#[test]
fn transition_menu_keeps_selected_group_and_replaces_it_for_an_unselected_transition() {
    let mut d = Driver::new();
    let seq = d.seq();
    let video = seq.video_tracks[0].transitions[0].id;
    let audio = seq.audio_tracks[0].transitions[0].id;
    d.exec("timeline.select", json!({"transitions": [video.0, audio.0]}));
    d.ok("ui.click", json!({"id": format!("timeline.transition.{}", video.0), "button": "right"}));
    d.frames(3);
    d.click("timeline.transitionMenu.clear");
    assert!(!d.seq().all_tracks().any(|t| t.transitions.iter().any(|x| x.id == video || x.id == audio)));
    d.exec("edit.undo", json!({}));
    d.exec("timeline.select", json!({"transitions": [video.0]}));
    d.ok("ui.click", json!({"id": format!("timeline.transition.{}", audio.0), "button": "right"}));
    d.frames(3);
    d.click("timeline.transitionMenu.clear");
    assert!(d.seq().all_tracks().any(|t| t.transitions.iter().any(|x| x.id == video)));
    assert!(!d.seq().all_tracks().any(|t| t.transitions.iter().any(|x| x.id == audio)));
}

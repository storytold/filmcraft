//! Headless UI test of the timeline's edit point menu (#219): right-clicking a cut between two
//! clips offers its trim types, Apply Default Transitions and Join Through Edits, as in Premiere;
//! right-clicking the body of a clip still opens the clip menu.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::{ClipId, Sequence};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    fn new() -> Self {
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
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
fn right_clicking_a_cut_opens_the_edit_point_menu() {
    let mut d = Driver::new();
    let (a, b) = d.cut_on_v1();
    d.right_click(b, 0.0);
    for id in ["rippleIn", "rippleOut", "roll", "trimIn", "trimOut", "applyDefaultTransitions", "joinThroughEdits"] {
        assert!(d.has(&format!("timeline.editPointMenu.{id}")), "{id}");
    }
    assert!(!d.has("timeline.clipMenu.edit.cut"), "not the clip menu");
    let eps = d.harness.state_mut().session.state.edit_points.clone();
    assert_eq!(eps.len(), 1, "the cut is selected as an edit point");
    assert!((eps[0].clip == a && eps[0].out) || (eps[0].clip == b && !eps[0].out), "{eps:?}");

    // Trim In: the incoming clip's In edge, as a regular trim
    d.click("timeline.editPointMenu.trimIn");
    let ep = d.harness.state_mut().session.state.edit_points.clone();
    assert_eq!(ep.len(), 1);
    assert_eq!((ep[0].clip, ep[0].out, serde_json::to_value(ep[0].kind).unwrap()), (b, false, json!("trim")));

    // Apply Default Transitions puts a transition on the cut
    d.right_click(b, 0.0);
    let before = d.seq().video_tracks[0].transitions.len();
    d.click("timeline.editPointMenu.applyDefaultTransitions");
    assert_eq!(d.seq().video_tracks[0].transitions.len(), before + 1, "a transition was added");
}

#[test]
fn the_body_of_a_clip_still_opens_the_clip_menu() {
    let mut d = Driver::new();
    let (_, b) = d.cut_on_v1();
    d.right_click(b, 0.0);
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    d.right_click(b, 0.5);
    assert!(d.has("timeline.clipMenu.edit.cut"), "the clip menu");
    assert!(!d.has("timeline.editPointMenu.rippleIn"), "not the edit point menu");
    assert!(d.harness.state_mut().session.state.selection.contains(&b), "the clip is selected");
}

#[test]
fn join_through_edits_joins_only_the_right_clicked_cut() {
    let mut d = Driver::new();
    let first = d.seq().video_tracks[0].items[0].clone();
    // two through edits inside the first clip on V1
    let secs = |t: i64| t as f64 / filmcraft_time::TICKS_PER_SECOND as f64;
    let (start, len) = (secs(first.start.0), secs(first.duration.0));
    for f in [0.3, 0.6] {
        d.exec("playhead.set", json!({"seconds": start + len * f}));
        assert_eq!(d.exec("timeline.razor", json!({"track": "V1"}))["cuts"], json!(1));
    }
    d.frames(2);
    let n = d.seq().video_tracks[0].items.len();
    let pieces: Vec<ClipId> = d.seq().video_tracks[0].items.iter().take(3).map(|i| i.id).collect();
    // the cut between the 2nd and 3rd pieces
    d.right_click(pieces[2], 0.0);
    d.click("timeline.editPointMenu.joinThroughEdits");
    let items = d.seq().video_tracks[0].items.clone();
    assert_eq!(items.len(), n - 1, "one cut joined");
    assert_eq!((items[0].id, items[1].id), (pieces[0], pieces[1]), "the other through edit is still there");

    // a cut between two different clips is not a through edit
    let (_, b) = d.cut_on_v1();
    d.right_click(b, 0.0);
    assert!(d.has("timeline.editPointMenu.joinThroughEdits"));
    let before = d.seq().video_tracks[0].items.len();
    d.click("timeline.editPointMenu.joinThroughEdits");
    assert_eq!(d.seq().video_tracks[0].items.len(), before, "disabled: nothing joined");
}

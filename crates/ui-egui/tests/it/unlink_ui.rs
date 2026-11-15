//! Headless UI test of Unlink from the timeline clip menu (#220): after unlinking, the clip that
//! was right-clicked is the only one selected, so a drag moves it away from its former partner.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::{ClipId, TrackItem};
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

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }

    fn item(&mut self, c: ClipId) -> TrackItem {
        self.app().session.active_sequence().unwrap().find_item(c).unwrap().1.clone()
    }

    fn selection(&mut self) -> Vec<ClipId> {
        let mut v = self.app().session.state.selection.clone();
        v.sort_by_key(|c| c.0);
        v
    }

    /// Right-click `clip` on the timeline and choose Link / Unlink from its menu.
    fn menu_link(&mut self, clip: ClipId) {
        self.ok("ui.click", json!({"id": format!("timeline.clip.{}", clip.0), "button": "right"}));
        self.frames(3);
        self.ok("ui.click", json!({"id": "timeline.clipMenu.clip.link"}));
        self.frames(3);
    }

    /// A linked picture on V1 and its sound.
    fn linked_pair(&mut self) -> (ClipId, ClipId) {
        let seq = self.app().session.active_sequence().unwrap().clone();
        let v = seq.video_tracks[0].items.iter().find(|i| i.link.is_some()).expect("a linked clip");
        let a = seq.audio_tracks.iter().flat_map(|t| &t.items).find(|i| i.link == v.link).expect("its sound");
        (v.id, a.id)
    }
}

#[test]
fn unlinked_picture_drags_away_from_its_sound() {
    let mut d = Driver::new();
    let (v, a) = d.linked_pair();
    let (v0, a0) = (d.item(v).start, d.item(a).start);
    d.menu_link(v);
    assert_eq!((d.item(v).link, d.item(a).link), (None, None), "unlinked");
    assert_eq!(d.selection(), [v], "only the right-clicked clip stays selected");
    // dragging the picture straight away leaves the sound where it was
    let id = format!("timeline.clip.{}", v.0);
    d.ok("ui.drag", json!({"from": {"id": id, "fx": 0.3}, "to": {"id": id, "fx": 0.6}, "steps": 12}));
    d.frames(4);
    assert_ne!(d.item(v).start, v0, "the picture moved");
    assert_eq!(d.item(a).start, a0, "its sound stayed");
}

#[test]
fn unlinking_from_the_sound_keeps_the_sound_selected_and_link_keeps_both() {
    let mut d = Driver::new();
    let (v, a) = d.linked_pair();
    d.menu_link(a);
    assert_eq!(d.selection(), [a]);
    // Link again: select both, link from the menu; the selection is left alone
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [v.0, a.0]}}));
    d.menu_link(v);
    assert!(d.item(v).link.is_some() && d.item(v).link == d.item(a).link, "linked again");
    let mut both = vec![v, a];
    both.sort_by_key(|c| c.0);
    assert_eq!(d.selection(), both);
}

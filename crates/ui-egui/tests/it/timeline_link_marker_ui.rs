//! Headless UI test of the link marker in the timeline (#1347): the name of a clip that is linked
//! to another clip of the sequence is underlined, as in Premiere; an unlinked clip's is not.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project::ClipId;
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

    /// A linked picture on V1 and its sound.
    fn linked_pair(&mut self) -> (ClipId, ClipId) {
        let seq = self.harness.state_mut().session.active_sequence().unwrap().clone();
        let v = seq.video_tracks[0].items.iter().find(|i| i.link.is_some()).expect("a linked clip");
        let a = seq.audio_tracks.iter().flat_map(|t| &t.items).find(|i| i.link == v.link).expect("its sound");
        (v.id, a.id)
    }

    fn rect(&mut self, clip: ClipId) -> egui::Rect {
        let id = format!("timeline.clip.{}", clip.0);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|n| n.as_f64().unwrap() as f32).collect();
        egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
    }

    /// Whether a horizontal line is drawn under the name in the top band of `clip`.
    fn underlined(&mut self, clip: ClipId) -> bool {
        fn walk(s: &egui::Shape, out: &mut Vec<egui::Shape>) {
            match s {
                egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
                other => out.push(other.clone()),
            }
        }
        let r = self.rect(clip);
        let mut shapes = Vec::new();
        self.harness.output().shapes.iter().for_each(|c| walk(&c.shape, &mut shapes));
        shapes.iter().any(|s| match s {
            egui::Shape::LineSegment { points, .. } => {
                let [a, b] = *points;
                a.y == b.y && b.x - a.x > 8.0 && a.y > r.min.y + 10.0 && a.y < r.min.y + 17.0 && a.x >= r.min.x && b.x <= r.max.x
            }
            _ => false,
        })
    }
}

#[test]
fn the_names_of_linked_clips_are_underlined_and_unlinking_removes_it() {
    let mut d = Driver::new();
    let (v, a) = d.linked_pair();
    assert!(d.underlined(v), "the picture's name is underlined");
    assert!(d.underlined(a), "so is its sound's");
    d.ok("ui.click", json!({"id": format!("timeline.clip.{}", v.0), "button": "right"}));
    d.frames(3);
    d.ok("ui.click", json!({"id": "timeline.clipMenu.clip.link"}));
    d.frames(3);
    assert!(!d.underlined(v) && !d.underlined(a), "unlinked clips are plain");
}

#[test]
fn a_clip_whose_partner_is_gone_is_not_marked() {
    let mut d = Driver::new();
    let (v, a) = d.linked_pair();
    // take only the sound away: the picture keeps a link id nobody shares
    let session = &mut d.harness.state_mut().session;
    let active = session.state.active_sequence.expect("an active sequence");
    let seq = std::sync::Arc::make_mut(&mut session.project).sequence_mut(active).expect("the sequence");
    seq.all_tracks_mut().for_each(|t| t.items.retain(|i| i.id != a));
    d.frames(3);
    let seq = d.harness.state_mut().session.active_sequence().unwrap().clone();
    assert!(seq.find_item(a).is_none(), "the sound is gone");
    assert!(seq.find_item(v).unwrap().1.link.is_some(), "the picture still carries a link id");
    assert!(!d.underlined(v), "a lone clip is not marked");
}

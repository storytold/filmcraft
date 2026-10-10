//! Escape during a drag-and-drop abandons it (#580): moving clips and captions on the Timeline,
//! dragging an item out of the Project panel, moving a Freeform card and dragging an effect onto a
//! clip. Each test
//! also makes the same drag without Escape, so it shows the drag would otherwise have done something.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use filmcraft_engine::Session;
use filmcraft_engine::project_panel::FREEFORM_POS;
use filmcraft_project::{BinEntry, ItemId};
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
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                self.frames(2);
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn rect(&mut self, id: &str) -> [f32; 4] {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        [r[0], r[1], r[2], r[3]]
    }
    fn send(&mut self, e: egui::Event) {
        self.harness.input_mut().events.push(e);
        self.frames(1);
    }
    /// A left-button drag in 12 steps, with Escape pressed half way when `escape`.
    fn drag(&mut self, from: egui::Pos2, to: egui::Pos2, escape: bool) {
        let button = |pos, pressed| egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() };
        self.send(egui::Event::PointerMoved(from));
        self.send(button(from, true));
        for k in 1..=12 {
            self.send(egui::Event::PointerMoved(from.lerp(to, k as f32 / 12.0)));
            if escape && k == 6 {
                self.send(egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
            }
        }
        self.send(button(to, false));
        self.frames(4);
    }
    fn project(&self) -> String {
        self.harness.state().session.project.to_json().to_string()
    }
    fn undo_len(&self) -> usize {
        self.harness.state().session.history.undo.len()
    }
    /// The demo's Footage bin open in place in the Project panel in `view`; its items.
    fn footage_in(&mut self, view: &str) -> Vec<u64> {
        let p = &self.harness.state().session.project;
        let bin = p.root.children.iter().find_map(|e| if let BinEntry::Bin(b) = e { (b.name == "Footage").then(|| b.clone()) } else { None }).expect("Footage");
        let items: Vec<u64> = bin.children.iter().filter_map(|e| if let BinEntry::Item(i) = e { Some(i.0) } else { None }).collect();
        self.ok("ui.panel.show", json!({"panel": "Project"}));
        self.ok("engine.execute", json!({"command": "project.view.set", "params": {"view": view}}));
        self.ok("ui.menu.invoke", json!({"id": "projectPanel.openBin", "params": {"bin": bin.id.0, "how": "inPlace"}}));
        self.frames(4);
        items
    }
}

fn centre(r: [f32; 4]) -> egui::Pos2 {
    egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
}

#[test]
fn escape_puts_a_moved_timeline_clip_back() {
    let mut d = Driver::demo();
    let clip = d.harness.state().session.active_sequence().unwrap().video_tracks[0].items[0].id;
    d.ok("engine.execute", json!({"command": "timeline.select", "params": {"clips": [clip.0]}}));
    let (before, undo) = (d.project(), d.undo_len());
    let from = centre(d.rect(&format!("timeline.clip.{}", clip.0)));
    let to = from + egui::vec2(120.0, 0.0);
    d.drag(from, to, true);
    assert_eq!(d.project(), before, "the clip stayed where it was");
    assert_eq!(d.undo_len(), undo, "nothing to undo");
    assert!(d.harness.state().session.state.selection.contains(&clip), "the clip is still selected (with its linked audio)");
    d.drag(from, to, false);
    assert_ne!(d.project(), before, "without Escape the same drag moves the clip");
}

#[test]
fn escape_puts_a_moved_caption_back() {
    let mut d = Driver::demo();
    let id =
        d.ok("engine.execute", json!({"command": "captions.add", "params": {"seconds": 2.0, "durationSeconds": 2.0}}))["caption"].as_u64().expect("caption id");
    d.frames(4);
    let from = centre(d.rect(&format!("timeline.caption.{id}")));
    let to = from + egui::vec2(60.0, 0.0);
    let (before, undo) = (d.project(), d.undo_len());
    d.drag(from, to, true);
    assert_eq!(d.project(), before, "the caption stayed where it was");
    assert_eq!(d.undo_len(), undo);
    d.drag(from, to, false);
    assert_ne!(d.project(), before, "without Escape the same drag moves the caption");
}

#[test]
fn escape_drops_nothing_from_the_project_panel() {
    let mut d = Driver::demo();
    let items = d.footage_in("list");
    let from = centre(d.rect(&format!("project.item.{}", items[0])));
    let tl = d.rect("panel.Timeline");
    let to = egui::pos2(tl[0] + tl[2] * 0.6, tl[1] + tl[3] * 0.75);
    let (before, undo) = (d.project(), d.undo_len());
    d.drag(from, to, true);
    assert_eq!(d.project(), before, "nothing was placed on the Timeline");
    assert_eq!(d.undo_len(), undo);
    d.drag(from, to, false);
    assert_ne!(d.project(), before, "without Escape the same drag places the clip");
}

#[test]
fn escape_puts_a_freeform_card_back_and_the_next_drag_works() {
    let mut d = Driver::demo();
    let items = d.footage_in("freeform");
    let from = centre(d.rect(&format!("project.item.{}", items[0])));
    let to = from + egui::vec2(90.0, 60.0);
    d.drag(from, to, true);
    let meta = |d: &Driver| d.harness.state().session.project.item(ItemId(items[0])).unwrap().metadata.contains_key(FREEFORM_POS);
    assert!(!meta(&d), "the card was not placed");
    d.drag(from, to, false);
    assert!(meta(&d), "after a cancel the next drag places the card");
}

#[test]
fn escape_cancels_dragging_an_effect_onto_a_clip() {
    let mut d = Driver::demo();
    d.ok("ui.panel.show", json!({"panel": "Effects"}));
    let effects = d.ok("ui.elements", json!({"prefix": "effects.item."}));
    let id = effects.as_array().unwrap().iter().filter_map(|e| e["id"].as_str()).find(|i| i.matches('.').count() == 2).expect("an effect").to_string();
    let from = centre(d.rect(&id));
    let clip = d.harness.state().session.active_sequence().unwrap().video_tracks[0].items[0].id;
    let to = centre(d.rect(&format!("timeline.clip.{}", clip.0)));
    let (before, undo) = (d.project(), d.undo_len());
    d.drag(from, to, true);
    assert_eq!(d.project(), before, "no effect was applied");
    assert_eq!(d.undo_len(), undo);
    d.drag(from, to, false);
    assert_ne!(d.project(), before, "without Escape the same drag applies {id}");
}

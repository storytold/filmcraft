//! Headless UI test of the Text panel's caption Size field on the real Captions tab: a drag saves the
//! value it reached, Escape cancels it, and typing still works.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use filmcraft_engine::Session;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project with a captioned track, on the Text panel's Captions tab.
    fn captions() -> Self {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
        d.ok("engine.execute", json!({"command": "captions.add", "params": {"text": "Hello", "seconds": 1.0}}));
        d.frames(2);
        d.ok("ui.click", json!({"id": "text.tab.Captions"}));
        d.frames(3);
        d
    }

    /// Run `n` frames, passing queued input through the app's raw input hook as eframe does.
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
        }
    }

    /// Send a control request and wait for its successful result.
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

    /// The first caption track's stored style.
    fn style(&mut self) -> Value {
        self.ok("engine.execute", json!({"command": "captions.list", "params": {}}))["tracks"][0]["style"].clone()
    }

    /// Press on `id`, move by each of `moves` (px) on its own frame, optionally press Escape on move
    /// `escape_at` (0-based), hold still for a few frames, release. Returns what the field showed on
    /// each hold-still frame.
    fn drag(&mut self, id: &str, moves: &[f32], escape_at: Option<usize>) -> Vec<f64> {
        let elements = self.ok("ui.elements", json!({"prefix": id}));
        let e = elements.as_array().unwrap().iter().find(|e| e["id"] == json!(id)).unwrap_or_else(|| panic!("no element {id}"));
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|v| v.as_f64().unwrap() as f32).collect();
        let mut p = egui::pos2(r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        let push = |d: &mut Self, e: egui::Event| d.harness.input_mut().events.push(e);
        push(self, egui::Event::PointerMoved(p));
        self.frames(1);
        push(self, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: true, modifiers: Default::default() });
        self.frames(1);
        for (step, dx) in moves.iter().enumerate() {
            p.x += dx;
            push(self, egui::Event::PointerMoved(p));
            if escape_at == Some(step) {
                push(self, egui::Event::Key { key: egui::Key::Escape, physical_key: None, pressed: true, repeat: false, modifiers: Default::default() });
            }
            self.frames(1);
        }
        let mut shown = Vec::new();
        for _ in 0..3 {
            self.frames(1);
            shown.push(self.shown_at(p));
        }
        push(self, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(3);
        shown
    }

    /// The number shown by the field the drag at `p` started on: the drag moves the pointer but not
    /// the field, so it is the field at `p`'s height.
    fn shown_at(&self, p: egui::Pos2) -> f64 {
        let y = f64::from(p.y);
        let mut fields =
            self.harness.query_all(By::new().predicate(move |n| n.numeric_value().is_some() && n.bounding_box().is_some_and(|b| b.y0 <= y && y <= b.y1)));
        let field = fields.next().unwrap_or_else(|| panic!("no number field at y {y}"));
        field.accesskit_node().numeric_value().unwrap()
    }

    /// Click `id` to edit it, type `text`, press Return.
    fn type_into(&mut self, id: &str, text: &str) {
        self.ok("ui.click", json!({"id": id}));
        self.frames(2);
        self.ok("ui.type", json!({"text": text}));
        self.frames(1);
        self.ok("ui.key", json!({"key": "Enter"}));
        self.frames(3);
    }
}

const SIZE: &str = "text.captions.style.size";

/// The stored size of the first caption track.
fn size(d: &mut Driver) -> f64 {
    d.style()["size"].as_f64().unwrap()
}

/// How many steps the session can undo.
fn undo_steps(d: &Driver) -> usize {
    d.harness.state().session.history.undo.len()
}

/// The first move only starts the drag; the rest count at the field's 0.5 per px.
const GROW_BY_15: [f32; 4] = [5.0, 10.0, 10.0, 10.0];

#[test]
fn dragging_the_size_field_saves_the_new_size() {
    let mut d = Driver::captions();
    d.type_into(SIZE, "60");
    assert_eq!(size(&mut d), 60.0, "typing sets it");
    let undo = undo_steps(&d);
    let shown = d.drag(SIZE, &GROW_BY_15, None);
    assert_eq!(shown, [75.0; 3], "the field keeps showing the dragged value while the pointer holds still");
    assert_eq!(size(&mut d), 75.0, "the value of the drag's last move, not an earlier one");
    assert_eq!(undo_steps(&d), undo + 1, "one undo step for the drag");
    d.drag(SIZE, &GROW_BY_15, Some(2));
    assert_eq!(size(&mut d), 75.0, "Escape cancels the drag");
    assert_eq!(undo_steps(&d), undo + 1, "and adds no undo step");
}

#[test]
fn a_drag_back_to_the_stored_size_changes_nothing() {
    let mut d = Driver::captions();
    d.type_into(SIZE, "60");
    let undo = undo_steps(&d);
    d.drag(SIZE, &[5.0, 10.0, 10.0, -10.0, -10.0], None);
    assert_eq!(size(&mut d), 60.0, "the field ended the drag back at the stored size");
    assert_eq!(undo_steps(&d), undo, "no undo step for it either");
}

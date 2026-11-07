//! Headless UI test of the Text panel's box fields on the real Captions tab: a drag saves the value
//! it reached, Escape cancels it, and typing still works.

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

    fn style(&mut self) -> Value {
        self.ok("engine.execute", json!({"command": "captions.list", "params": {}}))["tracks"][0]["style"].clone()
    }

    /// Press on `id`, move by each of `moves` (px) on its own frame, optionally press Escape on move
    /// `escape_at`, hold still for a few frames, release.
    fn drag(&mut self, id: &str, moves: &[f32], escape_at: Option<usize>) {
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
        self.frames(3);
        push(self, egui::Event::PointerButton { pos: p, button: egui::PointerButton::Primary, pressed: false, modifiers: Default::default() });
        self.frames(3);
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

const PAD_X: &str = "text.captions.style.backgroundPadding.x";
const PAD_Y: &str = "text.captions.style.backgroundPadding.y";

fn padding(d: &mut Driver) -> [f64; 2] {
    let v = d.style()["backgroundPadding"].clone();
    [v[0].as_f64().unwrap(), v[1].as_f64().unwrap()]
}

/// The first move only starts the drag; the rest count at the field's 0.5 per px.
const GROW_BY_15: [f32; 4] = [5.0, 10.0, 10.0, 10.0];
/// Out and back to where the drag started.
const OUT_AND_BACK: [f32; 5] = [5.0, 10.0, 10.0, -10.0, -10.0];

#[test]
fn dragging_the_padding_fields_saves_the_new_padding() {
    let mut d = Driver::captions();
    assert!(d.style()["backgroundPadding"].is_null(), "automatic by default");
    d.type_into(PAD_X, "30");
    assert_eq!(padding(&mut d), [30.0, 0.0], "typing sets it");
    d.drag(PAD_X, &GROW_BY_15, None);
    assert_eq!(padding(&mut d), [45.0, 0.0], "a drag on Pad X saves its last move, on x only");
    d.drag(PAD_Y, &GROW_BY_15, None);
    assert_eq!(padding(&mut d), [45.0, 15.0], "a drag on Pad Y saves its last move, on y only");
    d.drag(PAD_X, &GROW_BY_15, Some(2));
    assert_eq!(padding(&mut d), [45.0, 15.0], "Escape cancels the drag");
}

#[test]
fn a_drag_back_to_the_stored_padding_changes_nothing() {
    let mut d = Driver::captions();
    d.drag(PAD_X, &OUT_AND_BACK, None);
    assert!(d.style()["backgroundPadding"].is_null(), "automatic padding stays automatic");
    d.type_into(PAD_X, "30");
    d.drag(PAD_X, &OUT_AND_BACK, None);
    assert_eq!(padding(&mut d), [30.0, 0.0], "the field ended the drag back at 30");
}

#[test]
fn dragging_the_radius_field_saves_the_new_radius() {
    const RADIUS: &str = "text.captions.style.cornerRadius";
    let mut d = Driver::captions();
    d.drag(RADIUS, &GROW_BY_15, None);
    assert_eq!(d.style()["cornerRadius"].as_f64().unwrap(), 15.0, "the value of the drag's last move, not an earlier one");
    d.drag(RADIUS, &GROW_BY_15, Some(2));
    assert_eq!(d.style()["cornerRadius"].as_f64().unwrap(), 15.0, "Escape cancels the drag");
}

#[test]
fn a_drag_back_to_the_stored_radius_changes_nothing() {
    const RADIUS: &str = "text.captions.style.cornerRadius";
    let mut d = Driver::captions();
    d.type_into(RADIUS, "20");
    d.drag(RADIUS, &OUT_AND_BACK, None);
    assert_eq!(d.style()["cornerRadius"].as_f64().unwrap(), 20.0, "the field ended the drag back at 20");
}

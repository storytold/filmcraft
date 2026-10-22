//! The status bar (#2295): its hint or message is a label, so it can be selected and copied (painted
//! text could not), a click beside the text still dismisses a message, and a long message ends
//! before the running job's progress instead of running under it.

use std::sync::atomic::Ordering;
use std::sync::mpsc::{Sender, channel};

use egui::{Event, Modifiers, PointerButton, Pos2, Rect, pos2};
use egui_kittest::Harness;
use egui_kittest::kittest::Queryable;
use filmcraft_engine::{Job, Session};
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

const MESSAGE: &str = "Could not open /media/day 2/A012C003.mov: unsupported codec";

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
    /// Text the app put on the system clipboard.
    copied: Vec<String>,
}

impl Driver {
    fn new() -> Self {
        let (tx, rx) = channel();
        let mut s = Session::default();
        s.execute("file.openDemoProject", json!({})).unwrap();
        let app = FilmcraftApp::new(s).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_step_dt(1.0 / 60.0).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx, copied: Vec::new() };
        d.frames(4);
        d
    }
    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
    fn frames(&mut self, n: usize) {
        for _ in 0..n {
            let ctx = self.harness.ctx.clone();
            let mut raw = std::mem::take(self.harness.input_mut());
            eframe::App::raw_input_hook(self.harness.state_mut(), &ctx, &mut raw);
            *self.harness.input_mut() = raw;
            self.harness.step();
            let out = &self.harness.output().platform_output.commands;
            self.copied.extend(out.iter().filter_map(|c| if let egui::OutputCommand::CopyText(t) = c { Some(t.clone()) } else { None }));
        }
    }
    fn call(&mut self, method: &str, params: Value) -> Value {
        let (req, reply) = ControlRequest::new(method, params.clone());
        self.tx.send(req).unwrap();
        for _ in 0..600 {
            self.frames(1);
            if let Ok(v) = reply.try_recv() {
                return v;
            }
        }
        panic!("no reply to {method} {params}");
    }
    /// The rect and label of the automation element `id`.
    fn element(&mut self, id: &str) -> (Rect, String) {
        let v = self.call("ui.elements", json!({"prefix": id}));
        let e = v["result"].as_array().unwrap().iter().find(|e| e["id"] == id).cloned().unwrap_or_else(|| panic!("no {id} in {v}"));
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        (Rect::from_min_size(pos2(r[0], r[1]), egui::vec2(r[2], r[3])), e["label"].as_str().unwrap().to_string())
    }
    fn press(&mut self, at: Pos2, pressed: bool) {
        self.harness.event(Event::PointerButton { pos: at, button: PointerButton::Primary, pressed, modifiers: Modifiers::NONE });
        self.frames(1);
    }
    fn click(&mut self, at: Pos2) {
        self.harness.hover_at(at);
        self.frames(1);
        self.press(at, true);
        self.press(at, false);
        self.frames(2);
    }
    fn drag(&mut self, from: Pos2, to: Pos2) {
        self.harness.hover_at(from);
        self.frames(1);
        self.press(from, true);
        for k in 1..=8 {
            self.harness.hover_at(from + (to - from) * (k as f32 / 8.0));
            self.frames(1);
        }
        self.press(to, false);
        self.frames(2);
    }
    /// Ctrl+C, as egui-winit reports it: a Copy event with Ctrl held, in one frame. (The harness
    /// runs a pass per queued event and keeps only the last one's output, so this goes straight
    /// into the frame's input.)
    fn ctrl_c(&mut self) {
        self.harness.input_mut().events.extend([Event::ModifiersChanged(Modifiers::COMMAND), Event::Copy]);
        self.frames(1);
        self.harness.input_mut().events.push(Event::ModifiersChanged(Modifiers::NONE));
        self.frames(1);
    }
    /// The rect of painted text starting with `prefix`.
    fn painted(&self, prefix: &str) -> Option<Rect> {
        self.harness.output().shapes.iter().find_map(|s| match &s.shape {
            egui::Shape::Text(t) if t.galley.text().starts_with(prefix) => Some(t.visual_bounding_rect()),
            _ => None,
        })
    }
}

#[test]
fn a_status_message_can_be_selected_and_copied() {
    let mut d = Driver::new();
    // with clips selected too, Ctrl+C belongs to the selected text
    d.app().session.execute("edit.selectAll", json!({})).unwrap();
    assert!(!d.app().session.state.selection.is_empty());
    d.app().ui.status = MESSAGE.into();
    d.frames(2);
    // a widget with the message as its text: painted text was neither in the accessibility tree
    // nor an automation element
    assert!(d.harness.query_by_label(MESSAGE).is_some(), "the message is not a label");
    let (text, label) = d.element("status.text");
    assert_eq!(label, MESSAGE);
    // drag across it and copy
    d.copied.clear();
    d.drag(pos2(text.min.x + 1.0, text.center().y), pos2(text.max.x - 0.5, text.center().y));
    assert_eq!(d.app().ui.status, MESSAGE, "selecting the text dismissed it");
    d.ctrl_c();
    assert_eq!(d.copied, vec![MESSAGE.to_string()], "Ctrl+C did not copy the message");
    assert!(d.app().session.state.clipboard.is_empty(), "Ctrl+C copied the clips as well");
    assert_eq!(d.app().ui.status, MESSAGE, "copying the message replaced it");
}

#[test]
fn the_hint_is_a_label_too() {
    let mut d = Driver::new();
    assert!(d.app().ui.status.is_empty());
    let (text, hint) = d.element("status.text");
    assert!(hint.starts_with("Click to select"), "{hint}");
    assert!(text.width() > 0.0 && d.harness.query_by_label(&hint).is_some());
}

#[test]
fn a_click_beside_the_text_dismisses_the_message() {
    let mut d = Driver::new();
    d.app().ui.status = MESSAGE.into();
    d.frames(2);
    let (text, _) = d.element("status.text");
    // on the text, a click places the selection and leaves the message
    d.click(text.center());
    assert_eq!(d.app().ui.status, MESSAGE);
    // beside it, it dismisses the message as before
    d.click(pos2(text.max.x + 200.0, text.center().y));
    assert!(d.app().ui.status.is_empty(), "{}", d.app().ui.status);
}

#[test]
fn a_long_message_ends_before_the_job_progress() {
    let mut d = Driver::new();
    let job = Job { id: 901, label: "Exporting".into(), progress: Default::default(), result: Default::default() };
    job.progress.total.store(1000, Ordering::Relaxed);
    job.progress.done.store(250, Ordering::Relaxed);
    d.app().session.jobs.push(job);
    let long = format!("Could not write {}", "/very/long/folder/name".repeat(40));
    d.app().ui.status = long.clone();
    d.frames(3);
    let (text, label) = d.element("status.text");
    assert_eq!(label, long, "agents read the whole message");
    let progress = d.painted("Exporting…").expect("the job's progress text");
    assert!(text.max.x <= progress.min.x, "the message {text:?} runs under the progress {progress:?}");
    // and the cancel button still takes its own click
    let (cancel, _) = d.element("status.job.cancel");
    d.click(cancel.center());
    let cancelled = d.app().session.jobs.iter().find(|j| j.id == 901).map(|j| j.progress.cancel.load(Ordering::Relaxed));
    assert_eq!(cancelled, Some(true));
}

//! Headless UI tests of the middle mouse button in the Timeline (#1490): a middle-button drag pans
//! the view, whatever the tool and whatever it starts on, and never selects, moves or cuts a clip.
//! The Program and Source monitors already pan with it (#824).

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

    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }

    fn rect(&mut self, id: &str) -> egui::Rect {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f32> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap() as f32).collect();
        egui::Rect::from_min_size(egui::pos2(r[0], r[1]), egui::vec2(r[2], r[3]))
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

    /// A clip of Video 1 in view, wide enough to start a drag on: its id, its start (ticks) and
    /// its rectangle, with nothing selected and the view at a known zoom and scroll.
    fn setup(&mut self) -> (u64, i64, egui::Rect) {
        self.ok("ui.set", json!({"focused": "Timeline", "timeline": {"pps": 50.0, "scroll": 4.0}}));
        self.frames(4);
        self.exec("timeline.select", json!({"clips": []}));
        let shown = self.ok("ui.elements", json!({"prefix": "timeline.clip."}));
        let shown: Vec<String> = shown.as_array().unwrap().iter().map(|e| e["id"].as_str().unwrap().to_string()).collect();
        let seq = self.exec("sequence.inspect", json!({}));
        for it in seq["video"][0]["items"].as_array().unwrap().clone() {
            let (clip, start) = (it["clip"].as_u64().unwrap(), it["start"].as_i64().unwrap());
            let id = format!("timeline.clip.{clip}");
            if shown.contains(&id) {
                let r = self.rect(&id);
                if r.width() > 40.0 && r.center().x > 600.0 {
                    return (clip, start, r);
                }
            }
        }
        panic!("no clip of Video 1 in view");
    }

    fn start_of(&mut self, clip: u64) -> i64 {
        let seq = self.exec("sequence.inspect", json!({}));
        let items = seq["video"][0]["items"].as_array().unwrap().clone();
        items.iter().find(|i| i["clip"] == clip).map(|i| i["start"].as_i64().unwrap()).unwrap_or_else(|| panic!("clip {clip} gone"))
    }

    fn clips(&mut self) -> usize {
        let seq = self.exec("sequence.inspect", json!({}));
        seq["video"][0]["items"].as_array().unwrap().len()
    }

    fn app(&mut self) -> &mut FilmcraftApp {
        self.harness.state_mut()
    }
}

#[test]
fn a_middle_button_drag_on_a_clip_pans_and_leaves_the_clip_alone() {
    let mut d = Driver::demo();
    let (clip, start, r) = d.setup();
    let from = r.center();
    d.middle_drag(from, from - egui::vec2(200.0, 0.0));
    let scroll = d.app().ui.timeline.target_scroll;
    assert!((scroll - 8.0).abs() < 1e-6, "200 pt to the left at 50 pt a second scrolls 4 s later: {scroll}");
    assert!(d.app().session.state.selection.is_empty(), "the middle button selects nothing");
    assert_eq!(d.start_of(clip), start, "the clip stays where it was");
}

#[test]
fn a_middle_button_drag_pans_whatever_the_tool() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"tool": "Razor"}));
    let (_, _, r) = d.setup();
    let before = d.clips();
    let from = r.center();
    d.middle_drag(from, from - egui::vec2(200.0, 0.0));
    let scroll = d.app().ui.timeline.target_scroll;
    assert!((scroll - 8.0).abs() < 1e-6, "the Razor tool pans with the middle button too: {scroll}");
    assert_eq!(d.clips(), before, "the middle button cuts nothing");
}

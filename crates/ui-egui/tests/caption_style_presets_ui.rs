//! Headless UI test for copying caption track styles and saving reusable presets.

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

    fn ids(&mut self, prefix: &str) -> Vec<String> {
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().unwrap().iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }

    fn size(&mut self, track: u64) -> f64 {
        let r = self.ok("engine.execute", json!({"command": "captions.list", "params": {}}));
        r["tracks"].as_array().unwrap().iter().find(|t| t["id"] == json!(track)).expect("track")["style"]["size"].as_f64().unwrap()
    }
}

#[test]
fn caption_style_menu_copies_and_saves_reusable_styles() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let track = d.ok("engine.execute", json!({"command": "captions.newTrack", "params": {"format": "Subtitle"}}))["track"].as_u64().unwrap();
    d.ok("engine.execute", json!({"command": "captions.setStyle", "params": {"track": track, "size": 42.0}}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    d.frames(3);

    d.ok("ui.click", json!({"id": "text.captions.style.menu"}));
    d.frames(2);
    assert!(d.ids("text.captions.style.copy").iter().any(|id| id == "text.captions.style.copy"), "copy action appears in the style menu");
    d.ok("ui.click", json!({"id": "text.captions.style.copy"}));
    d.frames(2);
    d.ok("engine.execute", json!({"command": "captions.setStyle", "params": {"track": track, "size": 18.0}}));
    d.ok("ui.click", json!({"id": "text.captions.style.menu"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.captions.style.paste"}));
    d.frames(3);
    assert_eq!(d.size(track), 42.0, "paste restores the copied style");

    d.ok("ui.click", json!({"id": "text.captions.style.menu"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.captions.style.preset.openSave"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.captions.style.preset.name"}));
    d.ok("ui.type", json!({"text": "Broadcast Blue"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.captions.style.preset.save"}));
    d.frames(3);
    d.ok("engine.execute", json!({"command": "captions.setStyle", "params": {"track": track, "size": 22.0}}));
    d.ok("ui.click", json!({"id": "text.captions.style.menu"}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.captions.style.preset.0.apply"}));
    d.frames(3);
    assert_eq!(d.size(track), 42.0, "saved style can be applied again");
}

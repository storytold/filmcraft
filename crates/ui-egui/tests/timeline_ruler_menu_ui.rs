//! The Timeline ruler's own context menu (#1535): right-clicking the ruler, away from a marker,
//! opens the In/Out and marker menu at the playhead, not the clip menu of an earlier press.

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
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                self.frames(2);
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    /// Centre of an element, from `ui.elements`.
    fn centre(&mut self, id: &str) -> (f64, f64) {
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap_or_else(|| panic!("no element {id}")).clone();
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0)
    }
}

fn has(d: &mut Driver, id: &str) -> bool {
    let v = d.ok("ui.elements", json!({"prefix": id}));
    v.as_array().unwrap().iter().any(|e| e["id"] == id)
}

#[test]
fn right_clicking_the_ruler_opens_the_ruler_menu_not_a_stale_clip_menu() {
    let mut d = Driver::demo();
    // an earlier right-click on a clip leaves the clip menu's state behind
    let clip = d.harness.state().session.active_sequence().unwrap().video_tracks[0].items[0].id.0;
    d.ok("ui.click", json!({"id": format!("timeline.clip.{clip}"), "button": "right"}));
    assert!(has(&mut d, "timeline.clipMenu.edit.cut"), "a right-click on a clip opens the clip menu");
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(2);
    // a right-click on the ruler, away from the markers, opens the ruler's menu
    let (x, y) = d.centre("timeline.ruler");
    d.ok("ui.click", json!({"x": x + 300.0, "y": y, "button": "right"}));
    assert!(has(&mut d, "timeline.rulerMenu.markers.markIn"), "Mark In is offered");
    assert!(has(&mut d, "timeline.rulerMenu.markers.markSelection"), "Mark Selection is offered");
    assert!(!has(&mut d, "timeline.clipMenu.edit.cut"), "and not the clip menu");
    // its entries act at the playhead
    d.ok("ui.click", json!({"id": "timeline.rulerMenu.markers.markIn"}));
    let q = d.harness.state().session.active_sequence().unwrap();
    assert!(q.mark_in.is_some(), "Mark In set the In point");
}

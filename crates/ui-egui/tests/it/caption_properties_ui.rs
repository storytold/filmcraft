//! The Properties panel edits the selected caption (#1846): an imported SRT caption picked in the
//! timeline shows its text and in/out times there, and editing the text changes the caption.

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
        self.tx.send(req).expect("control channel");
        for _ in 0..600 {
            self.frames(1);
            if let Ok(r) = reply.try_recv() {
                assert_eq!(r["ok"], true, "{method} {params}: {r}");
                return r["result"].clone();
            }
        }
        panic!("no reply to {method} {params}");
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn ids(&mut self, prefix: &str) -> Vec<String> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": prefix}));
        v.as_array().expect("elements").iter().filter_map(|e| e["id"].as_str().map(str::to_string)).collect()
    }
    fn captions(&mut self) -> Vec<Value> {
        let r = self.exec("captions.list", json!({}));
        r["tracks"][0]["captions"].as_array().expect("captions").clone()
    }
}

#[test]
fn properties_panel_edits_the_selected_caption() {
    let dir = std::env::temp_dir().join(format!("filmcraft-caption-props-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("temp dir");
    let srt = dir.join("two.srt");
    std::fs::write(&srt, "1\n00:00:01,000 --> 00:00:02,500\nFirst line\n\n2\n00:00:03,000 --> 00:00:04,000\nSecond line\n").expect("write srt");

    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    d.exec("captions.import", json!({"path": srt.to_string_lossy()}));
    let caps = d.captions();
    assert_eq!(caps.len(), 2, "{caps:?}");
    let first = caps[0]["id"].as_u64().expect("caption id");

    d.ok("ui.panel.show", json!({"panel": "Properties"}));
    d.ok("ui.click", json!({"id": format!("timeline.caption.{first}")}));
    d.frames(3);
    assert_eq!(d.harness.state().session.state.caption_selection.iter().map(|c| c.0).collect::<Vec<_>>(), vec![first]);
    let shown = d.ids("properties.caption.");
    for id in ["properties.caption.text", "properties.caption.in", "properties.caption.out"] {
        assert!(shown.iter().any(|s| s == id), "{id} missing from Properties: {shown:?}");
    }

    // the field holds the caption's text: typing at its end extends it
    d.ok("ui.click", json!({"id": "properties.caption.text"}));
    d.ok("ui.key", json!({"key": "End"}));
    d.ok("ui.type", json!({"text": " edited"}));
    d.ok("ui.key", json!({"key": "Escape"}));
    d.frames(3);
    let caps = d.captions();
    assert_eq!(caps[0]["text"], "First line edited", "{caps:?}");
    assert_eq!(caps[1]["text"], "Second line", "{caps:?}");
    let _ = std::fs::remove_dir_all(&dir);
}

/// #2526 review: dragging a number field of the Text panel's caption style strip stores the
/// dragged value (the release frame used to hand the field the style's old value back, so the
/// X offset, and the Size field before it, ended where they started).
#[test]
fn dragging_the_caption_style_numbers_stores_them() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).expect("demo project");
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    d.exec("captions.add", json!({"text": "Hello", "durationSeconds": 2.0}));
    d.ok("ui.panel.show", json!({"panel": "Text"}));
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    d.frames(3);
    let drag = |d: &mut Driver, id: &str| {
        let v = d.ok("ui.elements", json!({"prefix": id}));
        let r: Vec<f64> = v.as_array().unwrap()[0]["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        let (x, y) = (r[0] + r[2] / 2.0, r[1] + r[3] / 2.0);
        d.ok("ui.drag", json!({"from": {"x": x, "y": y}, "to": {"x": x + 50.0, "y": y}, "steps": 6}));
        d.frames(3);
    };
    drag(&mut d, "text.captions.style.size");
    // the strip is wider than the panel: scroll it sideways to the offset fields
    let v = d.ok("ui.elements", json!({"prefix": "text.captions.style.size"}));
    let r: Vec<f64> = v.as_array().unwrap()[0]["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
    for _ in 0..10 {
        d.ok("ui.scroll", json!({"x": r[0] + r[2] / 2.0, "y": r[1] + r[3] / 2.0, "dx": -200.0, "dy": 0.0}));
        d.frames(2);
    }
    drag(&mut d, "text.captions.style.offsetX");
    let st = d.harness.state().session.active_sequence().unwrap().caption_tracks[0].style.clone();
    assert!(st.offset[0] > 0.02, "the dragged offset was stored: {:?}", st.offset);
    assert!(st.size > 54.0, "the dragged size was stored: {}", st.size);
}

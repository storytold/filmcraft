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

/// Exercise shadow control ordering, enabling, opacity edits, undo and nested colour selection.
#[test]
fn caption_shadow_popup_edits_track_style_and_undoes() {
    let mut session = Session::default();
    session.execute("file.openDemoProject", json!({})).unwrap();
    session.execute("captions.add", json!({"text": "Caption shadow test", "seconds": 1, "durationSeconds": 5})).unwrap();
    session.execute("captions.setStyle", json!({"background": false, "shadowDistance": 20, "shadowBlur": 12})).unwrap();
    session.execute("playhead.set", json!({"seconds": 2})).unwrap();
    let (tx, rx) = channel();
    let app = FilmcraftApp::new(session).with_control(rx);
    let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
    let mut d = Driver { harness, tx };
    d.frames(4);
    d.exec("window.workspace.captionsandgraphics", json!({}));
    d.ok("ui.panel.show", json!({"panel": "Text"}));
    d.exec("window.floatPanel", json!({"panel": "Text", "rect": [10, 40, 850, 440]}));
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    let elements = d.ok("ui.elements", json!({"prefix": "text.captions.style."}));
    let rect = |id: &str| elements.as_array().unwrap().iter().find(|e| e["id"] == id).unwrap()["rect"].clone();
    let bottom = rect("text.captions.style.anchor.bottom");
    let shadow = rect("text.captions.style.shadow");
    let toggle = rect("text.captions.style.shadow.enabled");
    assert!(shadow[0].as_f64().unwrap() >= bottom[0].as_f64().unwrap() + bottom[2].as_f64().unwrap());
    assert!(toggle[0].as_f64().unwrap() > shadow[0].as_f64().unwrap());
    assert!((shadow[1].as_f64().unwrap() - bottom[1].as_f64().unwrap()).abs() < 5.0, "same style row");
    d.ok("ui.click", json!({"id": "text.captions.style.shadow.enabled"}));
    let ids = d.ids("text.captions.style.shadow.");
    for field in ["enabled", "color", "opacity", "angle", "distance", "size", "blur"] {
        assert!(ids.contains(&format!("text.captions.style.shadow.{field}")), "missing {field}: {ids:?}");
    }
    assert!(d.harness.state().session.active_sequence().unwrap().caption_tracks[0].style.shadow);
    d.ok("ui.click", json!({"id": "text.captions.style.shadow.opacity", "count": 2}));
    d.ok("ui.key", json!({"key": "Cmd+A"}));
    d.ok("ui.type", json!({"text": "40"}));
    d.ok("ui.key", json!({"key": "Enter"}));
    d.frames(3);
    assert_eq!(d.harness.state().session.active_sequence().unwrap().caption_tracks[0].style.shadow_opacity, 40.0);
    if let Ok(dir) = std::env::var("FILMCRAFT_UI_SHOTS") {
        d.frames(8);
        std::fs::create_dir_all(&dir).unwrap();
        d.harness.render().expect("render shadow popup").save(std::path::Path::new(&dir).join("caption-shadow.png")).unwrap();
    }
    d.exec("edit.undo", json!({}));
    assert_eq!(d.harness.state().session.active_sequence().unwrap().caption_tracks[0].style.shadow_opacity, 75.0);
    d.exec("edit.undo", json!({}));
    assert!(!d.harness.state().session.active_sequence().unwrap().caption_tracks[0].style.shadow);

    // The nested RGB picker must stay open and apply colours to the track.
    d.ok("ui.click", json!({"id": "text.captions.style.shadow.color"}));
    d.frames(4);
    let picker = d
        .harness
        .ctx
        .memory(|m| {
            m.areas()
                .visible_layer_ids()
                .into_iter()
                .filter(|l| l.order == egui::Order::Foreground)
                .filter_map(|l| m.area_rect(l.id))
                .max_by(|a, b| a.area().total_cmp(&b.area()))
        })
        .expect("colour picker opened");
    let before = d.harness.state().session.active_sequence().unwrap().caption_tracks[0].style.shadow_color;
    d.ok("ui.click", json!({"x": picker.min.x + 70.0, "y": picker.min.y + 90.0}));
    d.frames(4);
    assert_ne!(d.harness.state().session.active_sequence().unwrap().caption_tracks[0].style.shadow_color, before);
    assert!(d.ids("text.captions.style.shadow.").contains(&"text.captions.style.shadow.blur".into()), "settings survive colour selection");
}

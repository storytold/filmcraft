//! Headless UI test of the caption track font picker in the Text panel's Captions tab: picking a
//! family sets the track style's font, and macOS-private (`.`-prefixed) faces are not offered.

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

    fn font(&mut self, track: u64) -> String {
        let r = self.ok("engine.execute", json!({"command": "captions.list", "params": {}}));
        let t = r["tracks"].as_array().unwrap().iter().find(|t| t["id"] == json!(track)).expect("track").clone();
        t["style"]["font"].as_str().unwrap().to_string()
    }
}

#[test]
fn caption_font_picker_sets_the_track_font() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    let track = d.ok("engine.execute", json!({"command": "captions.newTrack", "params": {"format": "Subtitle"}}))["track"].as_u64().unwrap();
    d.ok("engine.execute", json!({"command": "captions.add", "params": {"track": track, "text": "Hello", "seconds": 1.0}}));
    d.frames(2);
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    d.frames(3);
    assert_eq!(d.font(track), "Inter");

    d.ok("ui.click", json!({"id": "text.captions.style.font"}));
    d.frames(3);
    let options = d.ids("text.captions.style.font.option.");
    assert!(!options.is_empty(), "the font list is open");
    assert!(!options.iter().any(|o| o.starts_with("text.captions.style.font.option..")), "private faces offered: {options:?}");
    // with system fonts the list is long: scroll until the bundled serif is on screen
    let serif = "text.captions.style.font.option.Noto Serif";
    for _ in 0..200 {
        let shown = d.ids("text.captions.style.font.option.");
        if shown.iter().any(|o| o == serif) {
            break;
        }
        // wheel over a row in the middle (the edge rows are clipped by the popup)
        let Some(mid) = shown.get(shown.len() / 2) else { break };
        d.ok("ui.scroll", json!({"id": mid, "dy": -120.0}));
        d.frames(2);
    }
    d.ok("ui.click", json!({"id": serif}));
    d.frames(3);
    assert_eq!(d.font(track), "Noto Serif");
}

/// Every text shape on screen, with the colour of its first run.
fn painted_text(d: &Driver) -> Vec<(String, egui::Color32)> {
    fn walk(s: &egui::Shape, out: &mut Vec<(String, egui::Color32)>) {
        match s {
            egui::Shape::Text(t) => {
                if let Some(sec) = t.galley.job.sections.first() {
                    out.push((t.galley.text().to_string(), sec.format.color));
                }
            }
            egui::Shape::Vec(v) => v.iter().for_each(|s| walk(s, out)),
            _ => {}
        }
    }
    let mut out = Vec::new();
    d.harness.output().shapes.iter().for_each(|c| walk(&c.shape, &mut out));
    out
}

/// Relative luminance (WCAG) of an opaque colour.
fn luminance(c: egui::Color32) -> f32 {
    let f = |v: u8| {
        let v = v as f32 / 255.0;
        if v <= 0.03928 { v / 12.92 } else { ((v + 0.055) / 1.055).powf(2.4) }
    };
    0.2126 * f(c.r()) + 0.7152 * f(c.g()) + 0.0722 * f(c.b())
}

// Regression test for #1072: the Captions tab's "Import captions file…" button label was always
// white, so it vanished against the light grey button in light mode.
#[test]
fn import_captions_button_label_is_legible_in_both_themes() {
    let mut d = Driver::demo();
    d.ok("ui.set", json!({"workspace": "Captions and Graphics"}));
    d.ok("ui.click", json!({"id": "text.tab.Captions"}));
    for theme in ["dark", "light"] {
        d.ok("ui.set", json!({"theme": theme}));
        d.frames(3);
        assert!(d.ids("text.captions.import").iter().any(|i| i == "text.captions.import"), "{theme}: the empty Captions tab offers the import button");
        let field_bg = d.harness.state().tokens.field_bg;
        let (_, color) = painted_text(&d)
            .into_iter()
            .find(|(text, _)| text.starts_with("Import captions file"))
            .unwrap_or_else(|| panic!("{theme}: the button label is not drawn"));
        let (hi, lo) = (luminance(color).max(luminance(field_bg)), luminance(color).min(luminance(field_bg)));
        let contrast = (hi + 0.05) / (lo + 0.05);
        assert!(contrast >= 4.5, "{theme}: label {color:?} on {field_bg:?} has contrast {contrast:.2}:1");
    }
}

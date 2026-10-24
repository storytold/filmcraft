//! The `#rrggbb` field beside a colour parameter in Effect Controls (#1561): an exact colour can
//! be typed a character at a time without the field snapping back, and typing six digits leaves
//! the parameter's alpha alone.
//!
//! Driven headless through the control channel, like the other UI tests.

use std::sync::mpsc::{Sender, channel};

use egui_kittest::Harness;
use egui_kittest::kittest::{By, NodeT, Queryable};
use filmcraft_engine::Session;
use filmcraft_project::ClipId;
use filmcraft_ui_egui::FilmcraftApp;
use filmcraft_ui_egui::control::ControlRequest;
use serde_json::{Value, json};

struct Driver {
    harness: Harness<'static, FilmcraftApp>,
    tx: Sender<ControlRequest>,
}

impl Driver {
    /// The demo project with its first video clip selected and Effect Controls showing.
    fn demo() -> (Self, u64) {
        let mut session = Session::default();
        session.execute("file.openDemoProject", json!({})).expect("demo project");
        let (tx, rx) = channel();
        let app = FilmcraftApp::new(session).with_control(rx);
        let harness = Harness::builder().with_size(egui::vec2(1600.0, 980.0)).with_max_steps(10_000).build_eframe(move |_cc| app);
        let mut d = Driver { harness, tx };
        d.frames(4);
        let seq = d.exec("sequence.inspect", json!({}));
        let clip = seq["video"][0]["items"][0]["clip"].as_u64().expect("a clip in the demo sequence");
        d.exec("timeline.select", json!({"clips": [clip]}));
        d.ok("ui.panel.show", json!({"panel": "EffectControls"}));
        d.frames(3);
        (d, clip)
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
    fn ok(&mut self, method: &str, params: Value) -> Value {
        let r = self.call(method, params.clone());
        assert_eq!(r["ok"], true, "{method} {params}: {r}");
        r["result"].clone()
    }
    fn exec(&mut self, command: &str, params: Value) -> Value {
        self.ok("engine.execute", json!({"command": command, "params": params}))
    }
    fn find(&mut self, id: &str) -> Option<[f64; 4]> {
        self.frames(2);
        let v = self.ok("ui.elements", json!({"prefix": id}));
        let e = v.as_array().unwrap().iter().find(|e| e["id"] == id)?;
        let r: Vec<f64> = e["rect"].as_array().unwrap().iter().map(|x| x.as_f64().unwrap()).collect();
        Some([r[0], r[1], r[2], r[3]])
    }
    fn click(&mut self, id: &str) {
        self.find(id).unwrap_or_else(|| panic!("no element {id}"));
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
    /// A colour parameter of `clip`, as it is stored: linear components and alpha.
    fn color(&self, clip: u64, effect: &str, param: &str) -> [f32; 4] {
        let s = &self.harness.state().session;
        let it = s.active_sequence().expect("a sequence").find_item(ClipId(clip)).expect("the clip").1;
        it.effect(effect).expect("the effect").param(param).expect("the parameter").value.as_color().expect("a colour")
    }
    /// The same parameter as the eight-bit values the hex field shows.
    fn rgb(&self, clip: u64, effect: &str, param: &str) -> [u8; 3] {
        let c = self.color(clip, effect, param);
        [0, 1, 2].map(|i| (c[i].clamp(0.0, 1.0) * 255.0).round() as u8)
    }
    /// Everything the accessibility tree exposes inside the smallest foreground area — the colour
    /// picker's popup while it is open.
    fn popup_text(&mut self) -> String {
        self.frames(2);
        let areas: Vec<egui::Rect> = self
            .harness
            .ctx
            .memory(|m| m.areas().visible_layer_ids().into_iter().filter(|l| l.order == egui::Order::Foreground).filter_map(|l| m.area_rect(l.id)).collect());
        let r = areas.into_iter().min_by(|a, b| a.area().total_cmp(&b.area())).expect("the colour picker opened");
        self.harness
            .query_all(By::new())
            // the tree's root carries no bounds, and `rect()` panics without them
            .filter(|n| n.accesskit_node().bounding_box().is_some())
            .filter(|n| r.contains(n.rect().center()))
            .map(|n| {
                let node = n.accesskit_node();
                format!("{} {}", node.label().unwrap_or_default(), node.value().unwrap_or_default())
            })
            .collect::<Vec<_>>()
            .join(" | ")
    }
}

#[test]
fn the_hex_field_takes_typing_a_character_at_a_time() {
    let (mut d, clip) = Driver::demo();
    d.exec("effects.apply", json!({"clips": [clip], "effect": "ultra_key"}));
    d.frames(3);

    let before = d.rgb(clip, "ultra_key", "key_color");
    let alpha = d.color(clip, "ultra_key", "key_color")[3];
    d.click("effectControls.ultra_key.key_color.hex");
    d.ok("ui.key", json!({"key": "Cmd+A"}));

    // Not a colour yet: the field keeps what was typed and the parameter stays put.
    d.ok("ui.type", json!({"text": "#12ab"}));
    d.frames(2);
    assert_eq!(d.rgb(clip, "ultra_key", "key_color"), before, "a half-typed colour must not be applied");

    // The sixth digit completes it, and it lands.
    d.ok("ui.type", json!({"text": "34"}));
    d.frames(2);
    assert_eq!(d.rgb(clip, "ultra_key", "key_color"), [0x12, 0xab, 0x34]);

    // Deleting a character leaves the last valid colour in place.
    d.ok("ui.key", json!({"key": "Backspace"}));
    d.frames(2);
    assert_eq!(d.rgb(clip, "ultra_key", "key_color"), [0x12, 0xab, 0x34]);
    d.ok("ui.type", json!({"text": "5"}));
    d.frames(2);
    assert_eq!(d.rgb(clip, "ultra_key", "key_color"), [0x12, 0xab, 0x35]);

    // Six digits say nothing about alpha, so the parameter keeps the one it had.
    assert_eq!(d.color(clip, "ultra_key", "key_color")[3], alpha, "typing rrggbb must leave alpha alone");
}

#[test]
fn color_parameters_are_stored_srgb_encoded() {
    // `ultra_key` encodes each pixel with `effects::enc` (linear → sRGB) before comparing it with
    // the key colour, which it uses exactly as stored, so the stored components are sRGB. Ultra
    // Key's default is `[0.0, 0.8, 0.2]`, which is `#00cc33`; reading it as linear would give
    // `#00e77c` instead. The swatch, the picker and the hex field all depend on this.
    let (mut d, clip) = Driver::demo();
    d.exec("effects.apply", json!({"clips": [clip], "effect": "ultra_key"}));
    d.frames(3);
    assert_eq!(d.rgb(clip, "ultra_key", "key_color"), [0x00, 0xcc, 0x33]);
}

#[test]
fn the_picker_opens_on_the_parameter_read_as_srgb() {
    // The swatch and its picker go through `color_edit_button_srgba`, so they must open on the
    // stored components read as sRGB. Ultra Key's default `[0.0, 0.8, 0.2]` is 0 / 204 / 51;
    // reading it as linear, as egui's `Rgba` does, would show 0 / 231 / 124 instead.
    let (mut d, clip) = Driver::demo();
    d.exec("effects.apply", json!({"clips": [clip], "effect": "ultra_key"}));
    d.frames(3);
    d.click("effectControls.ultra_key.key_color.swatch");
    let shown = d.popup_text();
    for n in ["204", "51"] {
        assert!(shown.contains(n), "the picker should show {n}: {shown:?}");
    }
    for n in ["231", "124"] {
        assert!(!shown.contains(n), "the picker read the parameter as linear ({n}): {shown:?}");
    }
}

#[test]
fn every_keying_effect_with_a_color_parameter_has_the_field() {
    let (mut d, clip) = Driver::demo();
    for (effect, param) in [("ultra_key", "key_color"), ("color_key", "color")] {
        d.exec("effects.apply", json!({"clips": [clip], "effect": effect}));
        d.frames(3);
        let id = format!("effectControls.{effect}.{param}.hex");
        assert!(d.find(&id).is_some(), "no hex field for {effect}.{param}");
    }
}

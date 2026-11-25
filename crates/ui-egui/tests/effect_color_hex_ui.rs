//! The `#rrggbb` field beside a colour parameter in Effect Controls (#1561): an exact colour can
//! be typed a character at a time without the field snapping back, and typing six digits leaves
//! the parameter's alpha alone.
//!
//! Driven headless through the control channel, like the other UI tests.

use egui_kittest::kittest::{By, NodeT, Queryable};
use filmcraft_project::ClipId;
use serde_json::json;
#[path = "it/support.rs"]
mod support;
use support::{Drive, Launch, builder, demo_session, driver};

driver!(Driver);

impl Driver {
    /// The demo project with its first video clip selected and Effect Controls showing.
    fn demo() -> (Self, u64) {
        let mut d = Driver::open(demo_session(), builder());
        let seq = d.exec("sequence.inspect", json!({}));
        let clip = seq["video"][0]["items"][0]["clip"].as_u64().expect("a clip in the demo sequence");
        d.exec("timeline.select", json!({"clips": [clip]}));
        d.ok("ui.panel.show", json!({"panel": "EffectControls"}));
        d.frames(3);
        (d, clip)
    }
    fn find(&mut self, id: &str) -> Option<[f64; 4]> {
        self.frames(2);
        self.element_rect(id)
    }
    fn click(&mut self, id: &str) {
        self.find(id).unwrap_or_else(|| panic!("no element {id}"));
        self.ok("ui.click", json!({"id": id}));
        self.frames(3);
    }
    /// Scrolls `panel` with the wheel until element `id` sits in the upper part of the panel, where
    /// the user sees it: next's dock gives a panel a shorter body than the old dock did. Stops
    /// when the element no longer moves (the list is at its end).
    fn reveal(&mut self, id: &str, panel: &str) {
        let p = self.find(panel).unwrap_or_else(|| panic!("no panel {panel}"));
        let mut last = None;
        for _ in 0..30 {
            let e = self.find(id).unwrap_or_else(|| panic!("no element {id}"));
            if e[1] + e[3] <= p[1] + p[3] * 0.6 || last == Some(e[1]) {
                return;
            }
            last = Some(e[1]);
            self.ok("ui.scroll", json!({"x": p[0] + p[2] / 4.0, "y": p[1] + p[3] / 2.0, "dy": -40.0}));
            self.frames(3);
        }
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
    d.reveal("effectControls.ultra_key.key_color.hex", "panel.EffectControls");
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
    d.reveal("effectControls.ultra_key.key_color.swatch", "panel.EffectControls");
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
